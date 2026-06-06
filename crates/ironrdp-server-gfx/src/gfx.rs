//! RDPGFX (Graphics Pipeline) DVC handler

use std::sync::{Arc, Mutex};

use ironrdp_core::{encode_vec, impl_as_any, Encode, EncodeResult, WriteCursor};
use ironrdp_dvc::{DvcEncode, DvcProcessor, DvcServerProcessor};
use ironrdp_pdu::gcc::{Monitor, MonitorFlags};
use ironrdp_pdu::geometry::InclusiveRectangle;
use ironrdp_pdu::rdp::vc::dvc::gfx::{
    Avc420BitmapStream, Avc444BitmapStream, CapabilitiesAdvertisePdu, CapabilitiesConfirmPdu,
    CapabilitiesV103Flags, CapabilitiesV104Flags, CapabilitiesV107Flags, CapabilitiesV10Flags,
    CapabilitiesV81Flags, CapabilitiesV8Flags, CapabilitySet, ClientPdu, Codec1Type,
    CreateSurfacePdu, Encoding, EndFramePdu, MapSurfaceToOutputPdu, PixelFormat as GfxPixelFormat,
    QuantQuality, ResetGraphicsPdu, ServerPdu, StartFramePdu, Timestamp, WireToSurface1Pdu,
};
use ironrdp_pdu::{decode, PduResult};

type DvcMessage = ironrdp_dvc::DvcMessage;
use tracing::{debug, info};

use crate::display::{GfxFrameUpdate, GfxUncompressedUpdate};

/// GFX channel name as defined by RDP spec
pub const GFX_CHANNEL_NAME: &str = "Microsoft::Windows::RDS::Graphics";

/// Wrapper to make raw PDU bytes usable as DvcMessage
pub struct RawGfxPdu(pub Vec<u8>);

impl Encode for RawGfxPdu {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        dst.write_slice(&self.0);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "GfxServerPdu"
    }

    fn size(&self) -> usize {
        self.0.len()
    }
}

impl DvcEncode for RawGfxPdu {}

// SAFETY: RawGfxPdu only contains Vec<u8> which is Send
unsafe impl Send for RawGfxPdu {}

/// Wrap raw GFX PDU bytes in RDP_SEGMENTED_DATA (ZGFX) format.
/// MS-RDPEGFX Section 2.2.5: ALL GFX PDUs must be ZGFX-wrapped before DVC transport.
/// Using uncompressed mode (descriptor 0xE0/0xE1, compression type 0x04).
fn wrap_zgfx(data: &[u8]) -> Vec<u8> {
    const SINGLE: u8 = 0xE0;
    const MULTIPART: u8 = 0xE1;
    const UNCOMPRESSED: u8 = 0x04;
    // Max data per segment = 65534 bytes. The segmentSize field includes the
    // 1-byte compression type, so segmentSize = data_len + 1 ≤ 65535 (0xFFFF).
    const MAX_SEG_DATA: usize = 65534;

    if data.len() <= MAX_SEG_DATA {
        // Single segment: descriptor(1) + compression_type(1) + data
        let mut out = Vec::with_capacity(2 + data.len());
        out.push(SINGLE);
        out.push(UNCOMPRESSED);
        out.extend_from_slice(data);
        out
    } else {
        // Multipart: descriptor(1) + seg_count(2) + uncompressed_size(4) + segments
        let seg_count = data.len().div_ceil(MAX_SEG_DATA);
        let mut out = Vec::with_capacity(7 + data.len() + seg_count * 5);
        out.push(MULTIPART);
        out.extend_from_slice(&(seg_count as u16).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        for chunk in data.chunks(MAX_SEG_DATA) {
            // segmentSize = compression_type(1) + chunk data
            let seg_size = (chunk.len() + 1) as u32;
            out.extend_from_slice(&seg_size.to_le_bytes());
            out.push(UNCOMPRESSED);
            out.extend_from_slice(chunk);
        }
        out
    }
}

/// Unwrap ZGFX segmented data from client. Returns None if not ZGFX-wrapped.
fn unwrap_zgfx(data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() {
        return None;
    }
    match data[0] {
        0xE0 => {
            // Single segment: skip descriptor(1) + compression_type(1)
            if data.len() > 2 && data[1] == 0x04 {
                Some(data[2..].to_vec())
            } else {
                None
            }
        }
        0xE1 => {
            // Multipart: descriptor(1) + segment_count(2) + uncompressed_size(4)
            if data.len() < 7 {
                return None;
            }
            let seg_count = u16::from_le_bytes([data[1], data[2]]) as usize;
            let mut offset = 7;
            let mut result = Vec::new();
            for _ in 0..seg_count {
                if offset + 4 > data.len() {
                    break;
                }
                let seg_size = u32::from_le_bytes([
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                ]) as usize;
                offset += 4;
                if offset < data.len() && data[offset] == 0x04 {
                    // Uncompressed: skip compression_type byte
                    offset += 1;
                    let end = (offset + seg_size - 1).min(data.len());
                    result.extend_from_slice(&data[offset..end]);
                    offset = end;
                } else {
                    break;
                }
            }
            Some(result)
        }
        _ => None, // Not ZGFX-wrapped
    }
}

fn make_dvc_message(pdu: &ServerPdu) -> PduResult<DvcMessage> {
    let data = encode_vec(pdu).map_err(|e| ironrdp_pdu::encode_err!(e))?;
    let wrapped = wrap_zgfx(&data);
    Ok(Box::new(RawGfxPdu(wrapped)))
}

fn encode_pdu_into<T: Encode>(buf: &mut Vec<u8>, pdu: &T) -> bool {
    let size = pdu.size();
    let start = buf.len();
    buf.resize(start + size, 0);
    let mut cursor = WriteCursor::new(&mut buf[start..]);
    match pdu.encode(&mut cursor) {
        Ok(()) => true,
        Err(_) => {
            buf.truncate(start);
            false
        }
    }
}

/// Lenient parser for an RDPGFX `CapabilitiesAdvertisePDU` that tolerates
/// unknown capability versions instead of failing the whole PDU.
///
/// Wire format (all little-endian):
///
/// ```text
///  pduType:     u16   (0x0012 = CapabilitiesAdvertise)
///  flags:       u16
///  pduLength:   u32   (total bytes including this 8-byte header)
///  count:       u16
///  capsSets[count]: { version: u32, dataLength: u32, data: dataLength bytes }
/// ```
///
/// Each cap set carries an explicit `dataLength`, so an unknown version
/// can be skipped without losing framing for the rest of the list — this
/// fixes the regression where FreeRDP 3.x advertises a version beyond
/// the vendored `ironrdp_pdu` 0.7.0's enum, the upstream decoder bails
/// on the first unknown entry, and the older V10_x sets the same client
/// advertised are never seen.
///
/// Known versions are decoded into their typed `CapabilitySet` variants
/// so the existing match in `process()` lights up `avc420_supported`
/// just as if upstream had parsed them. Unknown versions are preserved
/// as `CapabilitySet::Unknown(data)`.
///
/// Returns `None` if `data` is not a CapabilitiesAdvertise or is
/// truncated/malformed at the envelope level — caller falls through to
/// the upstream error handling.
fn parse_caps_advertise_lenient(data: &[u8]) -> Option<Vec<CapabilitySet>> {
    fn read_u16_le(b: &[u8], off: usize) -> Option<u16> {
        b.get(off..off + 2)
            .map(|s| u16::from_le_bytes([s[0], s[1]]))
    }
    fn read_u32_le(b: &[u8], off: usize) -> Option<u32> {
        b.get(off..off + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    // PDU header
    if read_u16_le(data, 0)? != 0x0012 {
        return None;
    }
    // skip flags (u16) at offset 2, pduLength (u32) at offset 4
    let count = read_u16_le(data, 8)? as usize;
    let mut off = 10;

    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let version = read_u32_le(data, off)?;
        let data_len = read_u32_le(data, off + 4)? as usize;
        off += 8;
        let body = data.get(off..off.checked_add(data_len)?)?;
        off += data_len;

        // The upstream decoder reads exactly `data_len` bytes for the cap
        // body, then a u32 flag word for V8/V8_1/V10/V10_2..V10_7. We only
        // try to decode the flag word when the body is at least 4 bytes.
        let flag_word = if body.len() >= 4 {
            Some(u32::from_le_bytes([body[0], body[1], body[2], body[3]]))
        } else {
            None
        };

        let cap_set = match version {
            0x0008_0004 => CapabilitySet::V8 {
                flags: CapabilitiesV8Flags::from_bits_truncate(flag_word.unwrap_or(0)),
            },
            0x0008_0105 => CapabilitySet::V8_1 {
                flags: CapabilitiesV81Flags::from_bits_truncate(flag_word.unwrap_or(0)),
            },
            0x000a_0002 => CapabilitySet::V10 {
                flags: CapabilitiesV10Flags::from_bits_truncate(flag_word.unwrap_or(0)),
            },
            0x000a_0100 => CapabilitySet::V10_1,
            0x000a_0200 => CapabilitySet::V10_2 {
                flags: CapabilitiesV10Flags::from_bits_truncate(flag_word.unwrap_or(0)),
            },
            0x000a_0301 => CapabilitySet::V10_3 {
                flags: CapabilitiesV103Flags::from_bits_truncate(flag_word.unwrap_or(0)),
            },
            0x000a_0400 => CapabilitySet::V10_4 {
                flags: CapabilitiesV104Flags::from_bits_truncate(flag_word.unwrap_or(0)),
            },
            0x000a_0502 => CapabilitySet::V10_5 {
                flags: CapabilitiesV104Flags::from_bits_truncate(flag_word.unwrap_or(0)),
            },
            0x000a_0600 => CapabilitySet::V10_6 {
                flags: CapabilitiesV104Flags::from_bits_truncate(flag_word.unwrap_or(0)),
            },
            0x000a_0601 => CapabilitySet::V10_6Err {
                flags: CapabilitiesV104Flags::from_bits_truncate(flag_word.unwrap_or(0)),
            },
            0x000a_0701 => CapabilitySet::V10_7 {
                flags: CapabilitiesV107Flags::from_bits_truncate(flag_word.unwrap_or(0)),
            },
            other => {
                debug!(
                    version = format!("0x{other:08x}"),
                    data_len,
                    "GFX: cap set version unknown to ironrdp_pdu 0.7.0; preserving as Unknown"
                );
                CapabilitySet::Unknown(body.to_vec())
            }
        };
        out.push(cap_set);
    }

    Some(out)
}

/// Shared state between GfxHandler and the server
#[derive(Debug)]
pub struct GfxState {
    pub channel_id: Option<u32>,
    pub surface_created: bool,
    pub caps_confirmed: bool,
    pub frame_id: u32,
    pub avc420_supported: bool,
    /// Whether the client supports AVC444 (V10+ with AVC not disabled)
    pub avc444_supported: bool,
    /// Whether AVC444 is enabled by server config (chroma_mode = "avc444")
    pub avc444_enabled: bool,
    pub confirmed_cap: Option<CapabilitySet>,
    pub width: u16,
    pub height: u16,
    /// Frames sent but not yet acknowledged
    pub pending_acks: u32,
    /// Last acknowledged frame ID
    pub last_ack_frame: u32,
    /// Network quality estimate (0.0 = congested, 1.0 = excellent)
    pub network_quality: f32,
    /// Send timestamps for RTT calculation (frame_id → Instant)
    pub frame_send_times: std::collections::HashMap<u32, std::time::Instant>,
    /// Exponentially weighted moving average RTT in ms
    pub rtt_ewma_ms: f64,
    /// EWMA of pending_acks (smoothed to filter per-frame jitter)
    pub pending_acks_ewma: f64,
    /// EWMA of pending_acks delta between consecutive ack events.
    /// Positive = queue growing (congestion), zero/negative = stable/recovering.
    pub ack_queue_trend: f64,
    /// pending_acks snapshot at the previous ack event, for trend computation
    prev_pending_at_ack: u32,
    /// Last frame encode time in ms
    pub last_encode_ms: f64,
    /// Last frame size in bytes
    pub last_frame_bytes: u32,
    /// Total bytes sent
    pub total_bytes_sent: u64,
    /// Time of first frame sent
    pub start_time: Option<std::time::Instant>,
    /// Client peer IP address (set after TCP connection is established)
    pub peer_addr: Option<core::net::IpAddr>,
    /// Instantaneous bitrate tracking (per-frame Mbps samples since last log)
    bitrate_samples: Vec<f64>,
    bitrate_max: f64,
    bitrate_min: f64,
    last_frame_time: Option<std::time::Instant>,
}

impl GfxState {
    pub fn new(width: u16, height: u16, avc444_enabled: bool) -> Self {
        Self {
            channel_id: None,
            surface_created: false,
            caps_confirmed: false,
            frame_id: 0,
            avc420_supported: false,
            avc444_supported: false,
            avc444_enabled,
            confirmed_cap: None,
            width,
            height,
            pending_acks: 0,
            last_ack_frame: 0,
            network_quality: 1.0,
            frame_send_times: std::collections::HashMap::new(),
            rtt_ewma_ms: 0.0,
            pending_acks_ewma: 0.0,
            ack_queue_trend: 0.0,
            prev_pending_at_ack: 0,
            last_encode_ms: 0.0,
            last_frame_bytes: 0,
            total_bytes_sent: 0,
            start_time: None,
            peer_addr: None,
            bitrate_samples: Vec::new(),
            bitrate_max: 0.0,
            bitrate_min: f64::MAX,
            last_frame_time: None,
        }
    }

    pub fn next_frame_id(&mut self) -> u32 {
        self.frame_id += 1;
        self.pending_acks += 1;
        self.total_bytes_sent += u64::from(self.last_frame_bytes);
        if self.start_time.is_none() {
            self.start_time = Some(std::time::Instant::now());
        }
        // Track instantaneous bitrate per frame
        if let Some(prev) = self.last_frame_time {
            let dt = prev.elapsed().as_secs_f64().max(0.0001);
            let mbps = f64::from(self.last_frame_bytes) * 8.0 / dt / 1_000_000.0;
            self.bitrate_samples.push(mbps);
            if mbps > self.bitrate_max {
                self.bitrate_max = mbps;
            }
            if mbps < self.bitrate_min {
                self.bitrate_min = mbps;
            }
        }
        self.last_frame_time = Some(std::time::Instant::now());
        self.frame_send_times
            .insert(self.frame_id, std::time::Instant::now());
        // Limit map size to prevent unbounded growth
        if self.frame_send_times.len() > 120 {
            let cutoff = self.frame_id.saturating_sub(120);
            self.frame_send_times.retain(|&id, _| id > cutoff);
        }
        self.frame_id
    }

    /// Update network quality and RTT based on frame acknowledgment.
    pub fn ack_frame(&mut self, ack_frame_id: u32) {
        self.last_ack_frame = ack_frame_id;
        if self.pending_acks > 0 {
            self.pending_acks -= 1;
        }

        // Calculate RTT from send timestamp
        if let Some(send_time) = self.frame_send_times.remove(&ack_frame_id) {
            let rtt_ms = send_time.elapsed().as_secs_f64() * 1000.0;
            if self.rtt_ewma_ms == 0.0 {
                self.rtt_ewma_ms = rtt_ms;
            } else {
                self.rtt_ewma_ms = self.rtt_ewma_ms * 0.8 + rtt_ms * 0.2;
            }
        }

        // Ack queue trend: delta in pending_acks between consecutive ack events.
        // Positive = more frames sent than acked since last time (queue growing).
        let delta = self.pending_acks as i32 - self.prev_pending_at_ack as i32;
        self.prev_pending_at_ack = self.pending_acks;
        self.ack_queue_trend = self.ack_queue_trend * 0.8 + f64::from(delta) * 0.2;
        self.pending_acks_ewma = self.pending_acks_ewma * 0.9 + f64::from(self.pending_acks) * 0.1;

        // Network-only RTT estimate (subtract server-side encode time)
        let net_rtt_ms = (self.rtt_ewma_ms - self.last_encode_ms).max(0.0);

        // Adaptive bitrate: ack queue TREND driven, not absolute RTT driven.
        //
        // Absolute RTT conflates client decode/render latency with network delay.
        // A pipelined RDP client sustaining 60fps with 200ms pipeline latency
        // produces RTT=200ms on a sub-1ms LAN — the old absolute thresholds
        // would cut bitrate to 0.15× while the link sits idle.
        //
        // Ack queue trend directly measures whether the transport pipeline is
        // falling behind: growing queue = congestion, stable queue = healthy
        // (regardless of absolute latency from slow client decode).
        self.network_quality = if self.pending_acks <= 2 {
            1.0
        } else if self.ack_queue_trend <= 0.5 {
            // Queue stable or shrinking — client keeping up, even with pipeline latency.
            if self.pending_acks_ewma < 15.0 {
                1.0
            } else {
                0.85
            }
        } else if self.ack_queue_trend <= 2.0 {
            // Queue growing slowly — early congestion or transient burst.
            if net_rtt_ms < 30.0 {
                0.85
            } else {
                0.6
            }
        } else if self.ack_queue_trend <= 5.0 {
            0.4
        } else {
            0.15
        };
    }

    /// Get recommended bitrate based on network quality and base bitrate
    pub fn adaptive_bitrate(&self, base_bitrate: u32) -> u32 {
        (base_bitrate as f32 * self.network_quality) as u32
    }

    pub fn is_ready(&self) -> bool {
        self.channel_id.is_some() && self.avc420_supported && self.caps_confirmed
    }
}

/// GFX DVC processor
pub struct GfxHandler {
    pub state: Arc<Mutex<GfxState>>,
}

impl_as_any!(GfxHandler);

impl GfxHandler {
    pub fn new(state: Arc<Mutex<GfxState>>) -> Self {
        Self { state }
    }

    /// Create a single ZGFX-wrapped buffer containing all GFX PDUs for an H.264 frame.
    /// Per MS-RDPEGFX Section 2.2.5: "The server SHOULD combine multiple RDPGFX commands
    /// into a single RDP_SEGMENTED_DATA structure."
    /// First call also includes surface setup PDUs (ResetGraphics + CreateSurface + MapSurfaceToOutput).
    pub fn create_frame_pdu(state: &mut GfxState, frame: &GfxFrameUpdate) -> Vec<u8> {
        let aux_len = frame.h264_aux.as_ref().map_or(0, |a| a.len());
        let mut raw_pdus = Vec::with_capacity(frame.h264_data.len() + aux_len + 512);

        let enc_w = frame.enc_width;
        let enc_h = frame.enc_height;

        // First frame: surface setup (CapConfirm already sent by DVC handler)
        if !state.surface_created {
            encode_pdu_into(
                &mut raw_pdus,
                &ServerPdu::ResetGraphics(ResetGraphicsPdu {
                    width: u32::from(state.width),
                    height: u32::from(state.height),
                    monitors: vec![Monitor {
                        left: 0,
                        top: 0,
                        right: i32::from(state.width) - 1,
                        bottom: i32::from(state.height) - 1,
                        flags: MonitorFlags::PRIMARY,
                    }],
                }),
            );

            encode_pdu_into(
                &mut raw_pdus,
                &ServerPdu::CreateSurface(CreateSurfacePdu {
                    surface_id: 0,
                    width: enc_w,
                    height: enc_h,
                    pixel_format: GfxPixelFormat::XRgb,
                }),
            );

            encode_pdu_into(
                &mut raw_pdus,
                &ServerPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                    surface_id: 0,
                    output_origin_x: 0,
                    output_origin_y: 0,
                }),
            );

            state.surface_created = true;
            info!(
                "GFX surface created: {}x{} (enc: {}x{})",
                state.width, state.height, enc_w, enc_h
            );
        }

        let frame_id = state.next_frame_id();

        encode_pdu_into(
            &mut raw_pdus,
            &ServerPdu::StartFrame(StartFramePdu {
                timestamp: Timestamp {
                    milliseconds: 0,
                    seconds: 0,
                    minutes: 0,
                    hours: 0,
                },
                frame_id,
            }),
        );

        let make_rect = || InclusiveRectangle {
            left: 0,
            top: 0,
            right: frame.width,   // RDPGFX_RECT16 exclusive bound = visible crop
            bottom: frame.height, // RDPGFX_RECT16 exclusive bound = visible crop
        };

        let make_dest_rect = || InclusiveRectangle {
            left: 0,
            top: 0,
            right: enc_w,  // RDPGFX_RECT16 exclusive bound = encoder-aligned
            bottom: enc_h, // RDPGFX_RECT16 exclusive bound = encoder-aligned
        };

        let make_qq = || QuantQuality {
            quantization_parameter: 22,
            progressive: false,
            quality: 100,
        };

        // Choose AVC444 or AVC420 path based on available data and negotiated caps
        let use_avc444 = frame.h264_aux.is_some() && state.avc444_supported && state.avc444_enabled;

        if use_avc444 {
            // AVC444 path: WireToSurface1 + Avc444BitmapStream
            let aux_data = frame.h264_aux.as_ref().unwrap();
            let avc444_stream = Avc444BitmapStream {
                encoding: Encoding::LUMA_AND_CHROMA,
                stream1: Avc420BitmapStream {
                    rectangles: vec![make_rect()],
                    quant_qual_vals: vec![make_qq()],
                    data: &frame.h264_data,
                },
                stream2: Some(Avc420BitmapStream {
                    rectangles: vec![make_rect()],
                    quant_qual_vals: vec![make_qq()],
                    data: aux_data,
                }),
            };

            if let Ok(avc444_data) = encode_vec(&avc444_stream) {
                encode_pdu_into(
                    &mut raw_pdus,
                    &ServerPdu::WireToSurface1(WireToSurface1Pdu {
                        surface_id: 0,
                        codec_id: Codec1Type::Avc444,
                        pixel_format: GfxPixelFormat::XRgb,
                        destination_rectangle: make_dest_rect(),
                        bitmap_data: avc444_data,
                    }),
                );
            }
        } else {
            // AVC420 path: WireToSurface1 + Avc420BitmapStream
            let avc_stream = Avc420BitmapStream {
                rectangles: vec![make_rect()],
                quant_qual_vals: vec![make_qq()],
                data: &frame.h264_data,
            };

            if let Ok(avc_data) = encode_vec(&avc_stream) {
                encode_pdu_into(
                    &mut raw_pdus,
                    &ServerPdu::WireToSurface1(WireToSurface1Pdu {
                        surface_id: 0,
                        codec_id: Codec1Type::Avc420,
                        pixel_format: GfxPixelFormat::XRgb,
                        destination_rectangle: make_dest_rect(),
                        bitmap_data: avc_data,
                    }),
                );
            }
        }

        encode_pdu_into(
            &mut raw_pdus,
            &ServerPdu::EndFrame(EndFramePdu { frame_id }),
        );

        debug!(
            frame_id,
            raw_bytes = raw_pdus.len(),
            h264_bytes = frame.h264_data.len(),
            "GFX frame PDU created",
        );

        // Single ZGFX wrap for all concatenated PDUs
        wrap_zgfx(&raw_pdus)
    }

    /// Create a ZGFX-wrapped buffer for uncompressed dirty rect updates.
    /// Uses WireToSurface1 with Codec1Type::Uncompressed for each rect.
    pub fn create_uncompressed_pdu(
        state: &mut GfxState,
        update: &GfxUncompressedUpdate,
    ) -> Vec<u8> {
        let mut raw_pdus = Vec::new();

        if !state.surface_created {
            encode_pdu_into(
                &mut raw_pdus,
                &ServerPdu::ResetGraphics(ResetGraphicsPdu {
                    width: u32::from(state.width),
                    height: u32::from(state.height),
                    monitors: vec![Monitor {
                        left: 0,
                        top: 0,
                        right: i32::from(state.width) - 1,
                        bottom: i32::from(state.height) - 1,
                        flags: MonitorFlags::PRIMARY,
                    }],
                }),
            );

            encode_pdu_into(
                &mut raw_pdus,
                &ServerPdu::CreateSurface(CreateSurfacePdu {
                    surface_id: 0,
                    width: update.width,
                    height: update.height,
                    pixel_format: GfxPixelFormat::XRgb,
                }),
            );

            encode_pdu_into(
                &mut raw_pdus,
                &ServerPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                    surface_id: 0,
                    output_origin_x: 0,
                    output_origin_y: 0,
                }),
            );

            state.surface_created = true;
            info!(
                "GFX surface created (uncompressed): {}x{}",
                update.width, update.height
            );
        }

        let frame_id = state.next_frame_id();

        encode_pdu_into(
            &mut raw_pdus,
            &ServerPdu::StartFrame(StartFramePdu {
                timestamp: Timestamp {
                    milliseconds: 0,
                    seconds: 0,
                    minutes: 0,
                    hours: 0,
                },
                frame_id,
            }),
        );

        let mut total_bytes = 0usize;
        for rect in &update.rects {
            encode_pdu_into(
                &mut raw_pdus,
                &ServerPdu::WireToSurface1(WireToSurface1Pdu {
                    surface_id: 0,
                    codec_id: Codec1Type::Uncompressed,
                    pixel_format: GfxPixelFormat::XRgb,
                    destination_rectangle: InclusiveRectangle {
                        left: rect.x,
                        top: rect.y,
                        right: rect.x + rect.width,
                        bottom: rect.y + rect.height,
                    },
                    bitmap_data: rect.pixel_data.to_vec(),
                }),
            );
            total_bytes += rect.pixel_data.len();
        }

        encode_pdu_into(
            &mut raw_pdus,
            &ServerPdu::EndFrame(EndFramePdu { frame_id }),
        );

        debug!(
            frame_id,
            rects = update.rects.len(),
            raw_bytes = total_bytes,
            "GFX uncompressed frame PDU created",
        );

        wrap_zgfx(&raw_pdus)
    }
}

impl DvcProcessor for GfxHandler {
    fn channel_name(&self) -> &str {
        GFX_CHANNEL_NAME
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        info!(channel_id, "GFX channel opened");
        let mut state = self.state.lock().unwrap();
        state.channel_id = Some(channel_id);
        Ok(Vec::new())
    }

    fn process(&mut self, _channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        // Client GFX data is also ZGFX-wrapped. Unwrap the ZGFX layer first.
        let raw_data = unwrap_zgfx(payload);
        let data = raw_data.as_deref().unwrap_or(payload);

        let client_pdu: ClientPdu = match decode(data) {
            Ok(pdu) => pdu,
            Err(e) => {
                // Upstream `ClientPdu::decode` errors as soon as any inner
                // `CapabilitySet` reports a version the vendored
                // `ironrdp_pdu` doesn't know about. Modern clients (FreeRDP
                // 3.x, recent mstsc) advertise newer capability versions
                // mixed in alongside the older ones we *do* understand,
                // which means a single unknown entry blows away the whole
                // CapabilitiesAdvertise — caps_confirmed never flips,
                // the client times out, and the user gets a white screen.
                //
                // Salvage CapsAdvertise (PDU type 0x12) by walking the wire
                // format manually and tolerating unknown versions. Other
                // PDUs (QoE FrameAck 0x16, CacheImportOffer 0x10, …)
                // continue to be silently ignored as before.
                if data.first() == Some(&0x12) {
                    if let Some(cap_sets) = parse_caps_advertise_lenient(data) {
                        debug!(
                            sets = cap_sets.len(),
                            "GFX: salvaged CapabilitiesAdvertise via lenient \
                             parser (upstream decode failed: {e})"
                        );
                        ClientPdu::CapabilitiesAdvertise(CapabilitiesAdvertisePdu(cap_sets))
                    } else {
                        debug!(
                            payload_len = data.len(),
                            first_bytes = ?&data[..data.len().min(8)],
                            "GFX: failed to lenient-parse CapabilitiesAdvertise: {e}"
                        );
                        return Ok(Vec::new());
                    }
                } else {
                    debug!(
                        payload_len = data.len(),
                        first_bytes = ?&data[..data.len().min(8)],
                        "GFX: ignoring unknown client PDU: {e}"
                    );
                    return Ok(Vec::new());
                }
            }
        };

        match client_pdu {
            ClientPdu::CapabilitiesAdvertise(caps) => {
                let cap_sets = &caps.0;
                info!("GFX client capabilities: {} sets", cap_sets.len());

                let mut state = self.state.lock().unwrap();

                // If already confirmed, ignore duplicate CapabilitiesAdvertise
                if state.caps_confirmed {
                    info!("GFX caps already confirmed, ignoring duplicate CapabilitiesAdvertise");
                    return Ok(Vec::new());
                }

                let mut best_cap = None;

                for cap in cap_sets {
                    match cap {
                        CapabilitySet::V10_7 { flags }
                            if !flags.contains(CapabilitiesV107Flags::AVC_DISABLED) =>
                        {
                            state.avc420_supported = true;
                            state.avc444_supported = true;
                            best_cap = Some(cap.clone());
                            break;
                        }
                        CapabilitySet::V10_6 { flags } | CapabilitySet::V10_6Err { flags }
                            if !flags.contains(CapabilitiesV104Flags::AVC_DISABLED) =>
                        {
                            state.avc420_supported = true;
                            state.avc444_supported = true;
                            best_cap = Some(cap.clone());
                        }
                        CapabilitySet::V10_5 { flags } | CapabilitySet::V10_4 { flags }
                            if !flags.contains(CapabilitiesV104Flags::AVC_DISABLED) =>
                        {
                            state.avc420_supported = true;
                            state.avc444_supported = true;
                            if best_cap.is_none() {
                                best_cap = Some(cap.clone());
                            }
                        }
                        CapabilitySet::V10_3 { flags }
                            if !flags.contains(CapabilitiesV103Flags::AVC_DISABLED) =>
                        {
                            state.avc420_supported = true;
                            state.avc444_supported = true;
                            if best_cap.is_none() {
                                best_cap = Some(cap.clone());
                            }
                        }
                        CapabilitySet::V10_2 { flags } | CapabilitySet::V10 { flags }
                            if !flags.contains(CapabilitiesV10Flags::AVC_DISABLED) =>
                        {
                            state.avc420_supported = true;
                            state.avc444_supported = true;
                            if best_cap.is_none() {
                                best_cap = Some(cap.clone());
                            }
                        }
                        CapabilitySet::V10_1 => {
                            // V10_1 has no AVC_DISABLED flag — always supports AVC
                            state.avc420_supported = true;
                            state.avc444_supported = true;
                            if best_cap.is_none() {
                                best_cap = Some(cap.clone());
                            }
                        }
                        CapabilitySet::V8_1 { flags }
                            if flags.contains(CapabilitiesV81Flags::AVC420_ENABLED) =>
                        {
                            state.avc420_supported = true;
                            // V8.1 only supports AVC420, not AVC444
                            if best_cap.is_none() {
                                best_cap = Some(cap.clone());
                            }
                        }
                        _ => {
                            if best_cap.is_none() {
                                best_cap = Some(cap.clone());
                            }
                        }
                    }
                }

                let confirmed = best_cap.unwrap_or(CapabilitySet::V8 {
                    flags: CapabilitiesV8Flags::empty(),
                });

                info!(
                    avc420 = state.avc420_supported,
                    avc444_client = state.avc444_supported,
                    avc444_enabled = state.avc444_enabled,
                    "GFX capabilities negotiated"
                );

                // Send CapabilitiesConfirm from the DVC handler so it goes through
                // DrdynvcServer's proper encoding path. Bitmaps are suppressed once
                // GFX channel is open, so no bitmap/GFX mixing will occur.
                state.confirmed_cap = Some(confirmed.clone());
                state.caps_confirmed = true;

                let confirm_pdu = ServerPdu::CapabilitiesConfirm(CapabilitiesConfirmPdu(confirmed));
                let msg = make_dvc_message(&confirm_pdu)?;
                info!("GFX CapabilitiesConfirm sent via DVC handler");
                Ok(vec![msg])
            }

            ClientPdu::FrameAcknowledge(ack) => {
                let mut state = self.state.lock().unwrap();
                state.ack_frame(ack.frame_id);

                // Log stats every 60 acked frames (~1 second at 60fps)
                if ack.frame_id % 60 == 0 {
                    let net_ms = (state.rtt_ewma_ms - state.last_encode_ms).max(0.0);

                    // Compute instant bitrate stats from samples since last log
                    let n = state.bitrate_samples.len() as f64;
                    let (inst_avg, inst_std) = if n > 0.0 {
                        let sum: f64 = state.bitrate_samples.iter().sum();
                        let avg = sum / n;
                        let var: f64 = state
                            .bitrate_samples
                            .iter()
                            .map(|x| (x - avg).powi(2))
                            .sum::<f64>()
                            / n;
                        (avg, var.sqrt())
                    } else {
                        (0.0, 0.0)
                    };
                    let inst_max = if state.bitrate_max > 0.0 {
                        state.bitrate_max
                    } else {
                        0.0
                    };
                    let inst_min = if state.bitrate_min < f64::MAX {
                        state.bitrate_min
                    } else {
                        0.0
                    };

                    info!(
                        "RTT {:.1}ms | encode {:.1}ms | net {:.1}ms | instant {:.1}/{:.1}/{:.1} Mbps (avg/max/min) | std {:.1} | {}KB/f | {} pending | trend {:.2} | quality {:.2}",
                        state.rtt_ewma_ms,
                        state.last_encode_ms,
                        net_ms,
                        inst_avg,
                        inst_max,
                        inst_min,
                        inst_std,
                        state.last_frame_bytes / 1024,
                        state.pending_acks,
                        state.ack_queue_trend,
                        state.network_quality,
                    );

                    // Reset for next window
                    state.bitrate_samples.clear();
                    state.bitrate_max = 0.0;
                    state.bitrate_min = f64::MAX;
                }
                Ok(Vec::new())
            }
        }
    }

    fn close(&mut self, channel_id: u32) {
        info!(channel_id, "GFX channel closed");
        let mut state = self.state.lock().unwrap();
        state.channel_id = None;
        state.surface_created = false;
        state.caps_confirmed = false;
        state.avc420_supported = false;
        state.avc444_supported = false;
        state.confirmed_cap = None;
    }
}

impl DvcServerProcessor for GfxHandler {}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap_set(version: u32, flags: u32) -> (u32, Vec<u8>) {
        (version, flags.to_le_bytes().to_vec())
    }

    fn raw_caps_advertise(cap_sets: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let caps_len: usize = cap_sets.iter().map(|(_, data)| 8 + data.len()).sum();
        let pdu_len = 10 + caps_len;

        let mut pdu = Vec::with_capacity(pdu_len);
        pdu.extend_from_slice(&0x0012u16.to_le_bytes());
        pdu.extend_from_slice(&0u16.to_le_bytes());
        pdu.extend_from_slice(&(pdu_len as u32).to_le_bytes());
        pdu.extend_from_slice(&(cap_sets.len() as u16).to_le_bytes());

        for (version, data) in cap_sets {
            pdu.extend_from_slice(&version.to_le_bytes());
            pdu.extend_from_slice(&(data.len() as u32).to_le_bytes());
            pdu.extend_from_slice(data);
        }

        pdu
    }

    fn process_caps(cap_sets: Vec<(u32, Vec<u8>)>) -> (GfxState, usize) {
        let state = Arc::new(Mutex::new(GfxState::new(1920, 1080, false)));
        let mut handler = GfxHandler::new(Arc::clone(&state));

        handler.start(7).expect("GFX channel should open");
        let messages = handler
            .process(7, &raw_caps_advertise(&cap_sets))
            .expect("CapabilitiesAdvertise should process");
        drop(handler);

        let state = Arc::try_unwrap(state)
            .expect("test should hold the only state reference")
            .into_inner()
            .expect("state mutex should not be poisoned");

        (state, messages.len())
    }

    #[test]
    fn avc420_negotiates_for_representative_client_capabilities() {
        let avc420_v81 = cap_set(0x0008_0105, CapabilitiesV81Flags::AVC420_ENABLED.bits());
        let v10_2 = cap_set(0x000a_0200, 0);
        let v10_4 = cap_set(0x000a_0400, 0);
        let v10_6 = cap_set(0x000a_0600, 0);
        let v10_7 = cap_set(0x000a_0701, 0);
        let unknown_future = cap_set(0x000a_0800, 0);

        let profiles = [
            ("mstsc", vec![unknown_future.clone(), v10_7]),
            ("Microsoft Remote Desktop for macOS", vec![v10_6.clone()]),
            ("Microsoft Remote Desktop for iOS/iPadOS", vec![v10_4]),
            ("FreeRDP", vec![unknown_future, v10_6]),
            ("Remmina", vec![v10_2]),
            ("legacy AVC420-only client", vec![avc420_v81]),
        ];

        for (client_name, cap_sets) in profiles {
            let (state, message_count) = process_caps(cap_sets);
            assert!(
                state.caps_confirmed,
                "{client_name} should complete GFX capability negotiation",
            );
            assert!(
                state.avc420_supported,
                "{client_name} should negotiate AVC420",
            );
            assert!(
                state.confirmed_cap.is_some(),
                "{client_name} should have a confirmed capability",
            );
            assert_eq!(
                message_count, 1,
                "{client_name} should receive exactly one CapabilitiesConfirm",
            );
        }
    }

    #[test]
    fn avc420_stays_disabled_when_client_disables_avc() {
        let (state, message_count) = process_caps(vec![cap_set(
            0x000a_0200,
            CapabilitiesV10Flags::AVC_DISABLED.bits(),
        )]);

        assert!(state.caps_confirmed);
        assert!(!state.avc420_supported);
        assert_eq!(message_count, 1);
    }

    #[test]
    fn stable_ack_queue_preserves_full_quality() {
        let mut gfx = GfxState::new(1920, 1080, false);

        // Simulate pipelined client: send one frame, ack one frame, repeat.
        // pending_acks stays at 0-1, queue trend stays at zero.
        for _ in 0..60 {
            gfx.next_frame_id();
            gfx.ack_frame(gfx.frame_id);
        }

        assert!(
            gfx.network_quality >= 1.0,
            "stable queue should get full quality, got {}",
            gfx.network_quality,
        );
        assert!(
            gfx.ack_queue_trend <= 0.5,
            "balanced send/ack should have non-growing trend, got {}",
            gfx.ack_queue_trend,
        );
    }

    #[test]
    fn slow_client_with_stable_pipeline_keeps_quality() {
        let mut gfx = GfxState::new(1920, 1080, false);

        // Build up a pipeline: send 10 frames without acking (simulates
        // 10-frame pipeline latency from a slow decoder).
        for _ in 0..10 {
            gfx.next_frame_id();
        }
        // Now acks arrive at the same rate as sends — 1:1 steady state.
        // pending_acks stays around 10 but the queue is not growing.
        for _ in 0..120 {
            gfx.next_frame_id();
            let ack_id = gfx.frame_id - 10; // ack a frame from 10 frames ago
            gfx.ack_frame(ack_id);
        }

        assert!(
            gfx.ack_queue_trend <= 0.5,
            "1:1 send/ack rate should have non-growing trend, got {}",
            gfx.ack_queue_trend,
        );
        assert!(
            gfx.network_quality >= 0.85,
            "stable pipeline (even with backlog) should not heavily penalize quality, got {}",
            gfx.network_quality,
        );
    }

    #[test]
    fn growing_ack_queue_reduces_quality() {
        let mut gfx = GfxState::new(1920, 1080, false);

        // Simulate severe congestion: send 5 frames per ack — queue grows fast.
        for round in 0..40 {
            for _ in 0..5 {
                gfx.next_frame_id();
            }
            gfx.ack_frame(round + 1);
        }

        assert!(
            gfx.ack_queue_trend > 2.0,
            "5:1 send/ack imbalance should produce strongly growing trend, got {}",
            gfx.ack_queue_trend,
        );
        assert!(
            gfx.network_quality <= 0.4,
            "rapidly growing queue should significantly reduce quality, got {}",
            gfx.network_quality,
        );
    }

    #[test]
    fn recovery_from_congestion_restores_quality() {
        let mut gfx = GfxState::new(1920, 1080, false);

        // Phase 1: congestion — send faster than ack
        for round in 0..30 {
            gfx.next_frame_id();
            gfx.next_frame_id();
            gfx.ack_frame(round + 1);
        }
        let congested_quality = gfx.network_quality;

        // Phase 2: recovery — ack faster than send (drain backlog)
        let drain_start = gfx.frame_id - gfx.pending_acks + 1;
        for i in 0..gfx.pending_acks.min(30) {
            gfx.ack_frame(drain_start + i);
        }
        // Then stabilize at 1:1
        for _ in 0..60 {
            gfx.next_frame_id();
            gfx.ack_frame(gfx.frame_id);
        }

        assert!(
            gfx.network_quality > congested_quality,
            "quality should improve after recovery: {} > {}",
            gfx.network_quality,
            congested_quality,
        );
    }
}

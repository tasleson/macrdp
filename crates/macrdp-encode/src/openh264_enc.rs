//! OpenH264 software H.264 encoder optimized for screen content

use std::ffi::c_void;

use anyhow::{Context, Result};
use bytes::Bytes;
use openh264::encoder::{Encoder, EncoderConfig, FrameType};
use openh264::formats::YUVBuffer;

use crate::color_convert::VImageConverter;
use crate::{Avc444EncodedFrame, EncodedFrame, VideoEncoder};

pub struct OpenH264Encoder {
    encoder: Encoder,
    /// Auxiliary encoder for AVC444 chroma stream
    encoder_aux: Option<Encoder>,
    width: u32,
    height: u32,
    force_keyframe: bool,
    yuv_buf: Vec<u8>,
    /// Current target bitrate
    target_bitrate: u32,
    mode_444: bool,
    /// vImage SIMD accelerated BGRA→I420 converter (macOS Accelerate.framework)
    vimage: Option<VImageConverter>,
    /// Reusable buffers for AVC444 YUV444 split
    yuv444_bufs: Option<OpenH264Yuv444Bufs>,
}

/// OpenH264-specific wrapper: adds packed I420 buffers on top of the shared split buffers.
struct OpenH264Yuv444Bufs {
    inner: crate::Yuv444SplitBufs,
    main_yuv_buf: Vec<u8>,
    aux_yuv_buf: Vec<u8>,
}

impl OpenH264Yuv444Bufs {
    fn new(width: u32, height: u32) -> Self {
        let full = (width * height) as usize;
        let quarter = ((width / 2) * (height / 2)) as usize;
        let yuv420_size = full + quarter * 2;
        Self {
            inner: crate::Yuv444SplitBufs::new(width, height),
            main_yuv_buf: vec![0u8; yuv420_size],
            aux_yuv_buf: vec![0u8; yuv420_size],
        }
    }
}

/// OpenH264 rejects bitrates above the level-dependent MaxSpatialBitrate
/// (288 Mbps at Level 5.2). Cap to 250 Mbps to stay safely within bounds.
const OPENH264_MAX_BITRATE: u32 = 250_000_000;

fn create_oh264_encoder(_width: u32, _height: u32, fps: f32, bitrate: u32) -> Result<Encoder> {
    let capped = bitrate.min(OPENH264_MAX_BITRATE);
    let config = EncoderConfig::new()
        .bitrate(openh264::encoder::BitRate::from_bps(capped))
        .max_frame_rate(openh264::encoder::FrameRate::from_hz(fps))
        .rate_control_mode(openh264::encoder::RateControlMode::Quality)
        .background_detection(false)
        .adaptive_quantization(false)
        .qp(openh264::encoder::QpRange::new(20, 40))
        .skip_frames(true)
        .usage_type(openh264::encoder::UsageType::ScreenContentRealTime)
        .complexity(openh264::encoder::Complexity::Medium)
        .intra_frame_period(openh264::encoder::IntraFramePeriod::from_num_frames(
            fps as u32 * 5,
        ))
        .long_term_reference(true)
        .num_threads(4);

    Encoder::with_api_config(openh264::OpenH264API::from_source(), config)
        .context("Failed to create OpenH264 encoder")
}

impl OpenH264Encoder {
    pub fn new(width: u32, height: u32, fps: f32, bitrate: u32, mode_444: bool) -> Result<Self> {
        let encoder = create_oh264_encoder(width, height, fps, bitrate)?;

        // AVC444: create auxiliary encoder with 70% bitrate
        let encoder_aux = if mode_444 {
            let aux_bitrate = (bitrate as f64 * 0.7) as u32;
            let aux = create_oh264_encoder(width, height, fps, aux_bitrate)?;
            tracing::info!(
                aux_bitrate_mbps = aux_bitrate as f64 / 1_000_000.0,
                "AVC444 auxiliary OpenH264 encoder created"
            );
            Some(aux)
        } else {
            None
        };

        let vimage = VImageConverter::new()
            .map_err(|e| tracing::warn!("vImage init failed, using scalar fallback: {e}"))
            .ok();

        let yuv_size = (width * height * 3 / 2) as usize;

        let yuv444_bufs = if mode_444 {
            Some(OpenH264Yuv444Bufs::new(width, height))
        } else {
            None
        };

        tracing::info!(
            width,
            height,
            fps,
            mode_444,
            bitrate_mbps = bitrate as f64 / 1_000_000.0,
            "OpenH264 encoder created (screen-optimized)"
        );

        Ok(Self {
            encoder,
            encoder_aux,
            width,
            height,
            force_keyframe: false,
            yuv_buf: vec![0u8; yuv_size],
            target_bitrate: bitrate,
            mode_444,
            vimage,
            yuv444_bufs,
        })
    }
}

/// Convert BGRA to YUV420 into an encoder-sized buffer (handles padding).
fn bgra_to_yuv420_padded(
    bgra: &[u8],
    src_w: u32,
    src_h: u32,
    stride: usize,
    enc_w: u32,
    enc_h: u32,
    yuv: &mut [u8],
) {
    let ew = enc_w as usize;
    let sw = src_w as usize;
    let sh = src_h as usize;
    let y_plane_size = ew * enc_h as usize;
    let uv_w = ew / 2;

    let (y_plane, uv_planes) = yuv.split_at_mut(y_plane_size);
    let uv_plane_size = uv_w * (enc_h as usize / 2);
    let (u_plane, v_plane) = uv_planes.split_at_mut(uv_plane_size);

    for row in 0..sh {
        for col in 0..sw {
            let bgra_offset = row * stride + col * 4;
            if bgra_offset + 2 >= bgra.len() {
                continue;
            }
            let b = bgra[bgra_offset] as i32;
            let g = bgra[bgra_offset + 1] as i32;
            let r = bgra[bgra_offset + 2] as i32;

            let y = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
            y_plane[row * ew + col] = y.clamp(0, 255) as u8;

            if row % 2 == 0 && col % 2 == 0 {
                let u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
                let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
                let uv_idx = (row / 2) * uv_w + (col / 2);
                u_plane[uv_idx] = u.clamp(0, 255) as u8;
                v_plane[uv_idx] = v.clamp(0, 255) as u8;
            }
        }
    }
}

/// Pack separate Y, U, V planes into a contiguous I420 buffer for OpenH264
fn pack_i420(y: &[u8], u: &[u8], v: &[u8], dst: &mut [u8]) {
    let y_len = y.len();
    let u_len = u.len();
    dst[..y_len].copy_from_slice(y);
    dst[y_len..y_len + u_len].copy_from_slice(u);
    dst[y_len + u_len..y_len + u_len + v.len()].copy_from_slice(v);
}

impl VideoEncoder for OpenH264Encoder {
    fn encode_bgra(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: usize,
    ) -> Result<EncodedFrame> {
        if width > self.width || height > self.height {
            anyhow::bail!(
                "Frame too large: encoder {}x{}, got {}x{}",
                self.width,
                self.height,
                width,
                height
            );
        }

        // Zero the padding area
        let y_size = (self.width * self.height) as usize;
        let uv_size = (self.width / 2 * self.height / 2) as usize;
        self.yuv_buf[..y_size].fill(0);
        self.yuv_buf[y_size..y_size + uv_size].fill(128);
        self.yuv_buf[y_size + uv_size..].fill(128);

        // Convert visible BGRA area to YUV420
        if let Some(ref converter) = self.vimage {
            // vImage handles width*height directly; we need to handle encoder padding separately
            // For now, use vImage for the visible area, then zero-fill padding
            if width == self.width && height == self.height {
                // No padding needed — vImage directly
                converter
                    .bgra_to_i420(data, width, height, stride, &mut self.yuv_buf)
                    .map_err(|e| anyhow::anyhow!("vImage BGRA->I420 failed: {e}"))?;
            } else {
                // Encoder dimensions have padding — use scalar fallback for now
                // TODO: vImage with manual padding
                bgra_to_yuv420_padded(
                    data,
                    width,
                    height,
                    stride,
                    self.width,
                    self.height,
                    &mut self.yuv_buf,
                );
            }
        } else {
            bgra_to_yuv420_padded(
                data,
                width,
                height,
                stride,
                self.width,
                self.height,
                &mut self.yuv_buf,
            );
        }

        let yuv = YUVBuffer::from_vec(
            self.yuv_buf.clone(),
            self.width as usize,
            self.height as usize,
        );

        // Force IDR on next frame if requested
        if self.force_keyframe {
            self.encoder.force_intra_frame();
            self.force_keyframe = false;
        }

        let bitstream = self
            .encoder
            .encode(&yuv)
            .context("OpenH264 encode failed")?;

        let mut nal_data = Vec::new();
        bitstream.write_vec(&mut nal_data);

        let is_keyframe = matches!(bitstream.frame_type(), FrameType::IDR | FrameType::I);

        Ok(EncodedFrame {
            data: Bytes::from(nal_data),
            is_keyframe,
            width: self.width,
            height: self.height,
        })
    }

    fn encode_bgra_444(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: usize,
    ) -> Result<Avc444EncodedFrame> {
        let encoder_aux = self
            .encoder_aux
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("AVC444 not enabled: no auxiliary encoder"))?;
        let bufs = self
            .yuv444_bufs
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("AVC444 not enabled: no YUV444 buffers"))?;

        let w = self.width;
        let h = self.height;

        bufs.inner.split_bgra(data, width, height, stride, w, h);

        // Pack into I420 and encode both streams
        pack_i420(
            &bufs.inner.main_view.y,
            &bufs.inner.main_view.u,
            &bufs.inner.main_view.v,
            &mut bufs.main_yuv_buf,
        );
        let main_yuv = YUVBuffer::from_vec(bufs.main_yuv_buf.clone(), w as usize, h as usize);
        let main_bitstream = self
            .encoder
            .encode(&main_yuv)
            .context("OpenH264 main encode failed")?;
        let mut main_nal = Vec::new();
        main_bitstream.write_vec(&mut main_nal);
        let main_keyframe = matches!(main_bitstream.frame_type(), FrameType::IDR | FrameType::I);

        pack_i420(
            &bufs.inner.aux_view.y,
            &bufs.inner.aux_view.u,
            &bufs.inner.aux_view.v,
            &mut bufs.aux_yuv_buf,
        );
        let aux_yuv = YUVBuffer::from_vec(bufs.aux_yuv_buf.clone(), w as usize, h as usize);
        let aux_bitstream = encoder_aux
            .encode(&aux_yuv)
            .context("OpenH264 aux encode failed")?;
        let mut aux_nal = Vec::new();
        aux_bitstream.write_vec(&mut aux_nal);
        let aux_keyframe = matches!(aux_bitstream.frame_type(), FrameType::IDR | FrameType::I);

        tracing::debug!(
            main_bytes = main_nal.len(),
            aux_bytes = aux_nal.len(),
            main_keyframe,
            aux_keyframe,
            "AVC444 OpenH264 dual-stream encode complete"
        );

        Ok(Avc444EncodedFrame::new(
            main_nal,
            main_keyframe,
            aux_nal,
            aux_keyframe,
            w,
            h,
        ))
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) {
        if self.target_bitrate == bitrate_bps {
            return;
        }
        self.target_bitrate = bitrate_bps;
        unsafe {
            let raw = self.encoder.raw_api();
            let mut bitrate_info = openh264_sys2::SBitrateInfo {
                iLayer: openh264_sys2::SPATIAL_LAYER_ALL,
                iBitrate: bitrate_bps as i32,
            };
            let ret = raw.set_option(
                openh264_sys2::ENCODER_OPTION_BITRATE,
                &mut bitrate_info as *mut _ as *mut c_void,
            );
            if ret != 0 {
                tracing::warn!(
                    ret,
                    "OpenH264 set_option(BITRATE) failed, bitrate change deferred"
                );
            } else {
                tracing::info!(
                    bitrate_mbps = bitrate_bps as f64 / 1_000_000.0,
                    "Bitrate updated via SetOption"
                );
            }
        }
        if let Some(ref mut aux) = self.encoder_aux {
            let aux_bitrate = (bitrate_bps as f64 * 0.7) as u32;
            unsafe {
                let raw = aux.raw_api();
                let mut bitrate_info = openh264_sys2::SBitrateInfo {
                    iLayer: openh264_sys2::SPATIAL_LAYER_ALL,
                    iBitrate: aux_bitrate as i32,
                };
                let _ = raw.set_option(
                    openh264_sys2::ENCODER_OPTION_BITRATE,
                    &mut bitrate_info as *mut _ as *mut c_void,
                );
            }
        }
    }

    fn force_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    fn supports_444(&self) -> bool {
        self.mode_444
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_frame() {
        let mut encoder = OpenH264Encoder::new(64, 64, 30.0, 1_000_000, false).unwrap();
        let bgra = vec![128u8; 64 * 64 * 4];
        let frame = encoder.encode_bgra(&bgra, 64, 64, 64 * 4).unwrap();
        assert!(!frame.data.is_empty());
        assert!(frame.is_keyframe);
    }

    #[test]
    fn test_screen_bitrate() {
        let br = crate::screen_bitrate(1920, 1080, 60.0, crate::Quality::HighQuality);
        assert!(
            br > 30_000_000,
            "1080p60 HighQuality should be > 30Mbps, got {}",
            br
        );
    }

    #[test]
    fn encode_4k_at_server_bitrate() {
        let bitrate = crate::screen_bitrate(3840, 2160, 60.0, crate::Quality::HighQuality);
        let mut encoder = OpenH264Encoder::new(3840, 2160, 60.0, bitrate, false).unwrap();
        let bgra = vec![128u8; 3840 * 2160 * 4];
        let frame = encoder.encode_bgra(&bgra, 3840, 2160, 3840 * 4).unwrap();
        assert!(!frame.data.is_empty());
    }

    #[test]
    fn encode_4k_with_larger_stride() {
        let stride = 3840 * 4 + 64; // extra padding per row
        let mut encoder = OpenH264Encoder::new(3840, 2160, 60.0, 50_000_000, false).unwrap();
        let bgra = vec![128u8; stride * 2160];
        let frame = encoder.encode_bgra(&bgra, 3840, 2160, stride).unwrap();
        assert!(!frame.data.is_empty());
    }
}

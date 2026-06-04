use anyhow::Result;
use bytes::Bytes;
use ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout;
use ironrdp_server::{
    gfx::GfxState, BitmapUpdate, DesktopSize, DisplayUpdate, GfxFrameUpdate,
    PixelFormat as RdpPixelFormat, RdpServerDisplay, RdpServerDisplayUpdates,
};
use macrdp_capture::{
    CaptureConfig, CapturePixelFormat, CapturedFrame, CgFallbackCapturer, FrameData, Rect,
    ScreenCapturer,
};
use macrdp_encode::{self, Quality, VideoEncoder};
use std::num::{NonZeroU16, NonZeroUsize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// OpenH264 enforces max(w,h) ≤ 3840 and min(w,h) ≤ 2160 (Level 5.2).
/// We clamp client-requested resolutions to these limits so that the
/// software encoder fallback always succeeds.  VideoToolbox has its own
/// constraint (native resolution only) but does not need this cap.
const MAX_ENCODE_LONG: u16 = 3840;
const MAX_ENCODE_SHORT: u16 = 2160;

/// How long to wait after the last resize request before triggering
/// deactivation/reactivation.  When a user drags a window corner, the RDP
/// client sends a new display-control PDU on every mouse-move frame.
/// Without debouncing each PDU triggers a full session reset (~200ms),
/// creating a cascade that can crash the session.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(300);

const ADAPTIVE_BITRATE_MIN_INTERVAL_FRAMES: u64 = 30;
const ADAPTIVE_BITRATE_RELATIVE_HYSTERESIS: f64 = 0.10;
const ADAPTIVE_BITRATE_MIN_DELTA_BPS: u32 = 1_000_000;
const ADAPTIVE_BITRATE_FLOOR_BPS: u32 = 500_000;
const GFX_CAPS_WAIT_TIMEOUT_SECONDS: u64 = 5;

/// Compute the clamped bounding box `(x, y, width, height)` that covers every
/// dirty rect, or `None` when there is nothing worth sending: no rects, or a
/// region that collapses to zero area after clamping to the frame. Coordinates
/// are clamped to the frame bounds so a capturer reporting a rect that extends
/// past the frame edge can never produce an out-of-bounds region.
fn dirty_bounding_box(
    dirty_rects: &[Rect],
    frame_width: u32,
    frame_height: u32,
) -> Option<(u32, u32, u32, u32)> {
    if dirty_rects.is_empty() {
        return None;
    }

    let mut min_x = frame_width;
    let mut min_y = frame_height;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    for r in dirty_rects {
        min_x = min_x.min(r.x);
        min_y = min_y.min(r.y);
        max_x = max_x.max(r.x + r.width);
        max_y = max_y.max(r.y + r.height);
    }

    // Clamp the far edges to the frame; the near edges are already valid unless
    // the whole region sits off-screen, which the emptiness check below catches.
    max_x = max_x.min(frame_width);
    max_y = max_y.min(frame_height);

    if max_x > min_x && max_y > min_y {
        Some((min_x, min_y, max_x - min_x, max_y - min_y))
    } else {
        None
    }
}

/// Display adapter that bridges ScreenCapturer to ironrdp-server
pub struct MacDisplay {
    width: u16,
    height: u16,
    /// Native macOS display resolution. VideoToolbox silently drops frames at other sizes.
    native_width: u16,
    native_height: u16,
    /// Whether resolution is fixed by config (true) or follows client (false)
    fixed_resolution: bool,
    frame_rate: u32,
    quality: Quality,
    encoder_pref: macrdp_encode::EncoderPreference,
    /// Whether AVC444 mode is requested by config
    mode_444: bool,
    base_bitrate: u32,
    skip_unchanged: bool,
    idle_keyframe_interval: Option<Duration>,
    gfx_state: Arc<Mutex<GfxState>>,
    pending_resize: Arc<Mutex<Option<(DesktopSize, Instant)>>>,
    /// Shared with the input handler. `macos_point = rdp_coord / scale`
    /// where `scale = rdp_dim / native_dim`. Updated on every resize.
    mouse_scale: Arc<Mutex<(f64, f64)>>,
}

impl MacDisplay {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        width: u16,
        height: u16,
        native_width: u16,
        native_height: u16,
        fixed_resolution: bool,
        frame_rate: u32,
        quality: Quality,
        encoder_pref: macrdp_encode::EncoderPreference,
        mode_444: bool,
        bitrate_override: Option<u32>,
        skip_unchanged: bool,
        idle_keyframe_sec: Option<u32>,
        gfx_state: Arc<Mutex<GfxState>>,
    ) -> Self {
        let base_bitrate = bitrate_override.unwrap_or_else(|| {
            macrdp_encode::screen_bitrate(width as u32, height as u32, frame_rate as f32, quality)
        });
        tracing::info!(
            base_bitrate_mbps = base_bitrate as f64 / 1_000_000.0,
            "Base bitrate"
        );
        let scale_x = (width as f64 / native_width as f64).max(f64::EPSILON);
        let scale_y = (height as f64 / native_height as f64).max(f64::EPSILON);
        Self {
            width,
            height,
            native_width,
            native_height,
            fixed_resolution,
            frame_rate,
            quality,
            encoder_pref,
            mode_444,
            base_bitrate,
            skip_unchanged,
            idle_keyframe_interval: idle_keyframe_sec
                .filter(|seconds| *seconds > 0)
                .map(|seconds| Duration::from_secs(seconds as u64)),
            gfx_state,
            pending_resize: Arc::new(Mutex::new(None)),
            mouse_scale: Arc::new(Mutex::new((scale_x, scale_y))),
        }
    }

    /// Returns a shared handle to the mouse coordinate scale.
    ///
    /// Pass the returned Arc to [`MacInputHandler`] so it reads the current
    /// scale after each client-driven resize.
    pub fn mouse_scale(&self) -> Arc<Mutex<(f64, f64)>> {
        Arc::clone(&self.mouse_scale)
    }

    /// Effective encoder preference, accounting for VideoToolbox native-resolution constraint.
    #[cfg(test)]
    fn effective_encoder_pref(&self) -> macrdp_encode::EncoderPreference {
        if self.encoder_pref.prefers_hardware_on_this_platform()
            && (self.width != self.native_width || self.height != self.native_height)
        {
            macrdp_encode::EncoderPreference::Software
        } else {
            self.encoder_pref
        }
    }

    fn apply_resize_request(&mut self, width: u16, height: u16, reason: &'static str) {
        if self.fixed_resolution {
            tracing::debug!(
                reason,
                "Ignoring resize request — resolution is fixed by config"
            );
            return;
        }

        let (long, short) = if width >= height {
            (MAX_ENCODE_LONG, MAX_ENCODE_SHORT)
        } else {
            (MAX_ENCODE_SHORT, MAX_ENCODE_LONG)
        };
        let w = width.min(long);
        let h = height.min(short);
        if w == 0 || h == 0 || (w == self.width && h == self.height) {
            return;
        }

        if w != width || h != height {
            tracing::info!(
                requested_w = width,
                requested_h = height,
                clamped_w = w,
                clamped_h = h,
                reason,
                "Clamped client resolution to encoder limit"
            );
        }

        tracing::info!(
            old_w = self.width,
            old_h = self.height,
            new_w = w,
            new_h = h,
            reason,
            "Adopting client-requested resolution"
        );
        self.width = w;
        self.height = h;
        let scale_x = (w as f64 / self.native_width as f64).max(f64::EPSILON);
        let scale_y = (h as f64 / self.native_height as f64).max(f64::EPSILON);
        *self.mouse_scale.lock().unwrap() = (scale_x, scale_y);
        self.base_bitrate =
            macrdp_encode::screen_bitrate(w as u32, h as u32, self.frame_rate as f32, self.quality);

        {
            let mut gfx = self.gfx_state.lock().unwrap();
            gfx.width = w;
            gfx.height = h;
        }

        *self.pending_resize.lock().unwrap() =
            Some((DesktopSize { width: w, height: h }, Instant::now()));
    }
}

#[async_trait::async_trait]
impl RdpServerDisplay for MacDisplay {
    async fn size(&mut self) -> DesktopSize {
        DesktopSize {
            width: self.width,
            height: self.height,
        }
    }

    fn request_resize(&mut self, width: u16, height: u16) {
        self.apply_resize_request(width, height, "client-confirm-active");
    }

    fn request_layout(&mut self, layout: DisplayControlMonitorLayout) {
        let monitors = layout.monitors();
        if monitors.len() != 1 {
            tracing::debug!(
                monitor_count = monitors.len(),
                "Ignoring multi-monitor layout request; v1 supports one client desktop"
            );
            return;
        }

        let (width, height) = monitors[0].dimensions();
        let (Ok(width), Ok(height)) = (u16::try_from(width), u16::try_from(height)) else {
            tracing::debug!(
                width,
                height,
                "Ignoring layout request with dimensions outside u16 range"
            );
            return;
        };

        self.apply_resize_request(width, height, "display-control-monitor-layout");
    }

    async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
        {
            let mut pending_resize = self.pending_resize.lock().unwrap();
            if pending_resize.take().is_some() {
                tracing::debug!("Cleared pre-stream resize request");
            }
        }

        // Create the capturer first — this wakes the display if it was asleep,
        // which lets us re-detect the true native resolution below.  We pass
        // a preliminary pixel format (BGRA is safe for every encoder path);
        // the final capture config is rebuilt after the encoder is chosen.
        let preliminary_config = CaptureConfig {
            width: self.width as u32,
            height: self.height as u32,
            frame_rate: self.frame_rate,
            pixel_format: CapturePixelFormat::Bgra,
        };
        let capturer = ScreenCapturer::new(preliminary_config).await?;

        // The display is now awake.  Re-detect native resolution so the mouse
        // scale and VideoToolbox decision use the real values, not a fallback
        // that was guessed while the display was asleep at startup.
        if let Ok((w, h)) = macrdp_capture::detect_display_size() {
            let (nw, nh) = (w as u16, h as u16);
            if nw != self.native_width || nh != self.native_height {
                tracing::info!(
                    old_native_w = self.native_width,
                    old_native_h = self.native_height,
                    new_native_w = nw,
                    new_native_h = nh,
                    "Updated native display resolution (display was asleep at startup)"
                );
                self.native_width = nw;
                self.native_height = nh;
                let scale_x = (self.width as f64 / nw as f64).max(f64::EPSILON);
                let scale_y = (self.height as f64 / nh as f64).max(f64::EPSILON);
                *self.mouse_scale.lock().unwrap() = (scale_x, scale_y);
            }
        }

        // VideoToolbox hardware encoder silently drops frames at non-native resolutions.
        // When a resize moved us away from native, fall back to software encoding.
        let effective_encoder_pref = if self.encoder_pref.prefers_hardware_on_this_platform()
            && (self.width != self.native_width || self.height != self.native_height)
        {
            tracing::warn!(
                current_w = self.width,
                current_h = self.height,
                native_w = self.native_width,
                native_h = self.native_height,
                "VideoToolbox requires native display resolution; using software encoder after resize"
            );
            macrdp_encode::EncoderPreference::Software
        } else {
            self.encoder_pref
        };

        let encoder = macrdp_encode::create_encoder(
            self.width as u32,
            self.height as u32,
            self.frame_rate as f32,
            self.quality,
            effective_encoder_pref,
            self.mode_444,
            self.base_bitrate,
        )
        .map_err(|e| tracing::warn!("H.264 encoder unavailable: {e}; using bitmap fallback"))
        .ok();

        if encoder.is_some() {
            tracing::info!("H.264 encoder available — will use GFX path when client supports it");
        }

        let capture_config = CaptureConfig {
            width: self.width as u32,
            height: self.height as u32,
            frame_rate: self.frame_rate,
            pixel_format: capture_pixel_format(self.mode_444, encoder.as_deref()),
        };

        // The preliminary capturer used BGRA; recreate if the encoder needs NV12.
        let capturer = if capture_config.pixel_format != CapturePixelFormat::Bgra {
            ScreenCapturer::new(capture_config.clone()).await?
        } else {
            capturer
        };

        Ok(Box::new(MacDisplayUpdates {
            capturer,
            capture_config,
            encoder,
            gfx_state: Arc::clone(&self.gfx_state),
            adaptive_bitrate: AdaptiveBitrateController::new(self.base_bitrate),
            idle_frames: IdleFrameController::new(self.skip_unchanged, self.idle_keyframe_interval),
            pending_resize: Arc::clone(&self.pending_resize),
            mode_444: self.mode_444,
            display_frame_count: 0,
            frame_pacer: FramePacer::new(self.frame_rate),
            backpressure_skip_remaining: 0,
            last_backpressure_warn: None,
            gfx_wait_frames: 0,
            gfx_no_caps_frames: 0,
        }))
    }
}

fn capture_pixel_format(mode_444: bool, encoder: Option<&dyn VideoEncoder>) -> CapturePixelFormat {
    if !mode_444 {
        if let Some(encoder) = encoder {
            if encoder.supports_pixel_buffer_input() {
                return CapturePixelFormat::Nv12;
            }
        }
    }

    CapturePixelFormat::Bgra
}

struct MacDisplayUpdates {
    capturer: ScreenCapturer,
    capture_config: CaptureConfig,
    encoder: Option<Box<dyn VideoEncoder>>,
    gfx_state: Arc<Mutex<GfxState>>,
    adaptive_bitrate: AdaptiveBitrateController,
    idle_frames: IdleFrameController,
    pending_resize: Arc<Mutex<Option<(DesktopSize, Instant)>>>,
    mode_444: bool,
    display_frame_count: u64,
    frame_pacer: FramePacer,
    /// Backpressure: frames remaining to skip before encoding the next one.
    /// Set after each encode based on the client's pending ack queue depth.
    backpressure_skip_remaining: u32,
    last_backpressure_warn: Option<std::time::Instant>,
    /// Frame counter for rate-limiting "GFX not ready" diagnostic logs
    gfx_wait_frames: u64,
    /// Consecutive frames spent with the GFX channel open but no capabilities advertised
    gfx_no_caps_frames: u64,
}

#[derive(Debug, Clone)]
struct AdaptiveBitrateController {
    base_bitrate: u32,
    current_bitrate: u32,
    last_update_frame: u64,
}

impl AdaptiveBitrateController {
    fn new(base_bitrate: u32) -> Self {
        Self {
            base_bitrate,
            current_bitrate: base_bitrate,
            last_update_frame: 0,
        }
    }

    fn recommended_bitrate(&self, gfx: &GfxState) -> u32 {
        let floor = ADAPTIVE_BITRATE_FLOOR_BPS.min(self.base_bitrate);
        gfx.adaptive_bitrate(self.base_bitrate)
            .clamp(floor, self.base_bitrate)
    }

    fn next_update(&mut self, frame_count: u64, gfx: &GfxState) -> Option<u32> {
        if frame_count.saturating_sub(self.last_update_frame) < ADAPTIVE_BITRATE_MIN_INTERVAL_FRAMES
        {
            return None;
        }

        let recommended = self.recommended_bitrate(gfx);
        if !bitrate_change_exceeds_hysteresis(self.current_bitrate, recommended) {
            return None;
        }

        self.current_bitrate = recommended;
        self.last_update_frame = frame_count;
        Some(recommended)
    }
}

fn bitrate_change_exceeds_hysteresis(current: u32, recommended: u32) -> bool {
    if current == 0 {
        return recommended > 0;
    }

    let delta = current.abs_diff(recommended);
    if delta >= ADAPTIVE_BITRATE_MIN_DELTA_BPS {
        return true;
    }

    (delta as f64 / current as f64) >= ADAPTIVE_BITRATE_RELATIVE_HYSTERESIS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdleFrameDecision {
    Encode { force_keyframe: bool },
    Skip,
}

#[derive(Debug, Clone)]
struct IdleFrameController {
    skip_unchanged: bool,
    idle_keyframe_interval: Option<Duration>,
    last_sent_at: Option<Instant>,
    skipped_unchanged_frames: u64,
}

impl IdleFrameController {
    fn new(skip_unchanged: bool, idle_keyframe_interval: Option<Duration>) -> Self {
        Self {
            skip_unchanged,
            idle_keyframe_interval,
            last_sent_at: None,
            skipped_unchanged_frames: 0,
        }
    }

    fn next_decision(&self, frame: &CapturedFrame, now: Instant) -> IdleFrameDecision {
        if !self.skip_unchanged || !is_known_unchanged_frame(frame) || self.last_sent_at.is_none() {
            return IdleFrameDecision::Encode {
                force_keyframe: false,
            };
        }

        let Some(interval) = self.idle_keyframe_interval else {
            return IdleFrameDecision::Skip;
        };

        let Some(last_sent_at) = self.last_sent_at else {
            return IdleFrameDecision::Encode {
                force_keyframe: false,
            };
        };

        if now.duration_since(last_sent_at) >= interval {
            IdleFrameDecision::Encode {
                force_keyframe: true,
            }
        } else {
            IdleFrameDecision::Skip
        }
    }

    fn record_sent(&mut self, now: Instant) {
        self.last_sent_at = Some(now);
        self.skipped_unchanged_frames = 0;
    }

    fn record_skipped(&mut self) -> u64 {
        self.skipped_unchanged_frames += 1;
        self.skipped_unchanged_frames
    }
}

#[derive(Debug, Clone)]
struct FramePacer {
    base_interval: Duration,
    encode_time_ewma_ms: f64,
    last_sent: Option<Instant>,
}

impl FramePacer {
    fn new(fps: u32) -> Self {
        Self {
            base_interval: Duration::from_secs_f64(1.0 / fps.max(1) as f64),
            encode_time_ewma_ms: 0.0,
            last_sent: None,
        }
    }

    fn record_encode(&mut self, encode_ms: f64) {
        if self.encode_time_ewma_ms == 0.0 {
            self.encode_time_ewma_ms = encode_ms;
        } else {
            self.encode_time_ewma_ms = self.encode_time_ewma_ms * 0.8 + encode_ms * 0.2;
        }
    }

    fn effective_interval(&self) -> Duration {
        let encode_interval_ms = self.encode_time_ewma_ms * 1.2;
        let base_ms = self.base_interval.as_secs_f64() * 1000.0;
        Duration::from_secs_f64(base_ms.max(encode_interval_ms) / 1000.0)
    }

    fn should_send(&self, now: Instant) -> bool {
        match self.last_sent {
            None => true,
            Some(last) => now.duration_since(last) >= self.effective_interval(),
        }
    }

    fn record_sent(&mut self, now: Instant) {
        self.last_sent = Some(now);
    }
}

fn is_known_unchanged_frame(frame: &CapturedFrame) -> bool {
    frame.dirty_rects_available && frame.dirty_rects.is_empty()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GfxPipelineDecision {
    ready: bool,
    use_444: bool,
    hopeless: bool,
    no_caps_timed_out: bool,
    channel_open: bool,
    caps_confirmed: bool,
}

fn gfx_pipeline_decision(
    state: &GfxState,
    encoder_available: bool,
    mode_444: bool,
    no_caps_wait_frames: u64,
    no_caps_timeout_frames: u64,
) -> GfxPipelineDecision {
    let no_caps_timed_out = state.channel_id.is_some()
        && !state.caps_confirmed
        && no_caps_wait_frames >= no_caps_timeout_frames;

    GfxPipelineDecision {
        ready: state.is_ready() && encoder_available,
        use_444: mode_444 && state.avc444_supported && state.avc444_enabled,
        hopeless: no_caps_timed_out
            || (state.channel_id.is_some() && state.caps_confirmed && !state.avc420_supported),
        no_caps_timed_out,
        channel_open: state.channel_id.is_some(),
        caps_confirmed: state.caps_confirmed,
    }
}

fn gfx_caps_timeout_frames(frame_rate: u32) -> u64 {
    u64::from(frame_rate.max(1)) * GFX_CAPS_WAIT_TIMEOUT_SECONDS
}

/// How many frames to skip after encoding one, based on ack queue depth.
/// Returns 0 (no skip) through 3 (keep 1 in 4) to let the client drain its buffer.
fn backpressure_skip_count(pending_acks: u32) -> u32 {
    match pending_acks {
        0..=4 => 0,
        5..=9 => 1,
        10..=19 => 2,
        _ => 3,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BitmapSourceKind {
    Bgra,
    PixelBuffer,
}

impl From<&FrameData> for BitmapSourceKind {
    fn from(data: &FrameData) -> Self {
        match data {
            FrameData::Raw(_) => Self::Bgra,
            FrameData::PixelBuffer(_) => Self::PixelBuffer,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BitmapFallbackBlock {
    Nv12AfterNoCapsTimeout,
    Nv12AfterAvcDisabled,
    UnexpectedPixelBuffer,
}

fn bitmap_fallback_decision(
    gfx: &GfxPipelineDecision,
    source: BitmapSourceKind,
) -> Result<(), BitmapFallbackBlock> {
    match source {
        BitmapSourceKind::Bgra => Ok(()),
        BitmapSourceKind::PixelBuffer if gfx.hopeless && gfx.no_caps_timed_out => {
            Err(BitmapFallbackBlock::Nv12AfterNoCapsTimeout)
        }
        BitmapSourceKind::PixelBuffer if gfx.hopeless => {
            Err(BitmapFallbackBlock::Nv12AfterAvcDisabled)
        }
        BitmapSourceKind::PixelBuffer => Err(BitmapFallbackBlock::UnexpectedPixelBuffer),
    }
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for MacDisplayUpdates {
    async fn next_update(&mut self) -> Result<Option<DisplayUpdate>> {
        // `encode_and_send` returns `Ok(None)` for ordinary, transient frame
        // skips: frame pacing, ack backpressure, unchanged/idle frames, and
        // degenerate (zero-size) dimensions. The RDP server loop, however,
        // treats a `next_update()` result of `Ok(None)` as end-of-stream and
        // tears the whole session down. A skipped frame is emphatically not
        // end-of-stream, so we loop and pull the next frame instead. This does
        // not busy-spin: `next_frame().await` blocks until the capturer
        // produces the next frame, which paces us at the capture rate. We only
        // ever surface an actual `DisplayUpdate`; genuine capture loss is
        // handled inline by the CoreGraphics fallback below.
        'outer: loop {
            // Debounce resize: only fire after no new request for RESIZE_DEBOUNCE.
            // During a window drag, the client sends a new display-control PDU
            // every mouse-move frame; we coalesce them into a single reactivation
            // at the final size.
            let settled_resize = {
                let mut pending = self.pending_resize.lock().unwrap();
                match pending.as_ref() {
                    Some((_, ts)) if ts.elapsed() >= RESIZE_DEBOUNCE => {
                        pending.take().map(|(ds, _)| ds)
                    }
                    _ => None,
                }
            };
            if let Some(desktop_size) = settled_resize {
                if desktop_size.width as u32 != self.capture_config.width
                    || desktop_size.height as u32 != self.capture_config.height
                {
                    tracing::info!(
                        width = desktop_size.width,
                        height = desktop_size.height,
                        "Display resize requested; triggering deactivation/reactivation"
                    );
                    return Ok(Some(DisplayUpdate::Resize(desktop_size)));
                }
            }

            // Drain stale frames — always use the latest available frame.
            // If SCK capturer stops (e.g. screen locked), fall back to CGDisplayCreateImage
            // which works at the display level (including lock screen).
            let frame = loop {
                let frame = match self.capturer.next_frame().await {
                    Some(f) => f,
                    None => {
                        // SCK stopped — fall back to CoreGraphics capture (works on lock screen)
                        tracing::warn!(
                            "SCStream stopped — switching to CoreGraphics fallback (lock screen?)"
                        );
                        let fallback = CgFallbackCapturer::new(&self.capture_config);
                        loop {
                            // Try to restore SCK (faster, has dirty rects)
                            match ScreenCapturer::new(self.capture_config.clone()).await {
                                Ok(new_capturer) => {
                                    tracing::info!(
                                        "SCStream recovered — switching back from CoreGraphics"
                                    );
                                    self.capturer = new_capturer;
                                    break;
                                }
                                Err(_) => {
                                    // SCK still unavailable — use CGDisplayCreateImage
                                    if let Some(cg_frame) = fallback.capture_frame() {
                                        // Send this fallback frame through the normal encoding
                                        // path; a skip here loops for the next frame rather than
                                        // ending the session.
                                        match self.encode_and_send(cg_frame)? {
                                            Some(update) => return Ok(Some(update)),
                                            None => continue 'outer,
                                        }
                                    }
                                    tokio::time::sleep(fallback.frame_interval()).await;
                                }
                            }
                        }
                        continue; // retry next_frame with restored SCK capturer
                    }
                };
                // If another frame is already buffered, skip this one and grab the newer one
                // This prevents frame queuing which adds latency
                match self.capturer.try_next_frame() {
                    Some(_newer) => continue, // drop older frame, grab newer
                    None => break frame,
                }
            };

            match self.encode_and_send(frame)? {
                Some(update) => return Ok(Some(update)),
                None => continue 'outer, // transient skip — pull the next frame
            }
        }
    }
}

impl MacDisplayUpdates {
    fn update_backpressure(&mut self, pending_acks: u32) {
        let skip = backpressure_skip_count(pending_acks);
        if skip > 0 {
            let should_warn = self
                .last_backpressure_warn
                .map(|t| t.elapsed().as_secs_f64() >= 2.0)
                .unwrap_or(true);
            if should_warn {
                self.last_backpressure_warn = Some(std::time::Instant::now());
                tracing::warn!(
                    pending_acks,
                    skip_frames = skip,
                    "Backpressure: client ack queue deep, throttling frame rate"
                );
            }
        }
        self.backpressure_skip_remaining = skip;
    }

    fn apply_adaptive_bitrate(&mut self) {
        let target_bitrate = {
            let state = self.gfx_state.lock().unwrap();
            self.adaptive_bitrate
                .next_update(self.display_frame_count, &state)
        };

        if let Some(target_bitrate) = target_bitrate {
            if let Some(encoder) = &mut self.encoder {
                encoder.set_bitrate(target_bitrate);
            }
            tracing::info!(
                target_bitrate_mbps = target_bitrate as f64 / 1_000_000.0,
                "Adaptive bitrate applied"
            );
        }
    }

    fn encode_and_send(&mut self, frame: CapturedFrame) -> Result<Option<DisplayUpdate>> {
        let (gfx, pending_acks) = {
            let state = self.gfx_state.lock().unwrap();
            if state.channel_id.is_some() && !state.caps_confirmed {
                self.gfx_no_caps_frames += 1;
            } else {
                self.gfx_no_caps_frames = 0;
            }

            (
                gfx_pipeline_decision(
                    &state,
                    self.encoder.is_some(),
                    self.mode_444,
                    self.gfx_no_caps_frames,
                    gfx_caps_timeout_frames(self.capture_config.frame_rate),
                ),
                state.pending_acks,
            )
        };

        if gfx.ready {
            // Backpressure: skip frames when the client's ack queue is deep.
            if self.backpressure_skip_remaining > 0 {
                self.backpressure_skip_remaining -= 1;
                return Ok(None);
            }

            // Frame pacing: skip if we're sending faster than the encoder can sustain.
            let now = Instant::now();
            if !self.frame_pacer.should_send(now) {
                return Ok(None);
            }
            let idle_decision = self.idle_frames.next_decision(&frame, now);
            if idle_decision == IdleFrameDecision::Skip {
                let skipped = self.idle_frames.record_skipped();
                if skipped == 1 || skipped.is_multiple_of(self.capture_config.frame_rate as u64) {
                    tracing::debug!(
                        skipped_unchanged_frames = skipped,
                        "Display: skipping unchanged frame"
                    );
                }
                return Ok(None);
            }
            let force_idle_keyframe = matches!(
                idle_decision,
                IdleFrameDecision::Encode {
                    force_keyframe: true
                }
            );

            // GFX H.264 path — always send at capture rate, never block on acks
            self.display_frame_count += 1;
            self.apply_adaptive_bitrate();
            if let Some(encoder) = &mut self.encoder {
                let t0 = std::time::Instant::now();

                // Force IDR keyframe on the first GFX frame so the decoder initializes cleanly.
                let force_keyframe = self.display_frame_count == 1 || force_idle_keyframe;
                if force_keyframe {
                    if self.display_frame_count == 1 {
                        tracing::info!(
                            "First GFX frame — forcing IDR keyframe for clean decoder init"
                        );
                    } else {
                        tracing::debug!("Idle interval elapsed — forcing IDR keyframe");
                    }
                    encoder.force_keyframe();
                }

                // Route based on frame data type
                match &frame.data {
                    FrameData::PixelBuffer(buf) => {
                        // Zero-copy VT path — encode CVPixelBuffer directly
                        match encoder.encode_pixel_buffer(buf.as_ptr(), force_keyframe) {
                            Ok(encoded) if !encoded.data.is_empty() => {
                                let encode_ms = t0.elapsed().as_secs_f64() * 1000.0;
                                tracing::debug!(
                                    display_frame = self.display_frame_count,
                                    h264_bytes = encoded.data.len(),
                                    is_keyframe = encoded.is_keyframe,
                                    encode_ms = format!("{:.1}", encode_ms),
                                    "Display: sending zero-copy GFX frame"
                                );
                                {
                                    let mut st = self.gfx_state.lock().unwrap();
                                    st.last_encode_ms = encode_ms;
                                    st.last_frame_bytes = encoded.data.len() as u32;
                                }
                                self.frame_pacer.record_encode(encode_ms);
                                self.frame_pacer.record_sent(Instant::now());
                                self.update_backpressure(pending_acks);
                                self.idle_frames.record_sent(Instant::now());
                                return Ok(Some(DisplayUpdate::GfxFrame(GfxFrameUpdate {
                                    h264_data: encoded.data,
                                    width: frame.width as u16,
                                    height: frame.height as u16,
                                    enc_width: encoded.width as u16,
                                    enc_height: encoded.height as u16,
                                    is_keyframe: encoded.is_keyframe,
                                    h264_aux: None,
                                })));
                            }
                            Ok(_) => {
                                tracing::warn!("Zero-copy encode returned empty data");
                                return Ok(Some(DisplayUpdate::DefaultPointer));
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Zero-copy encode failed: {e}, falling back to DefaultPointer"
                                );
                                return Ok(Some(DisplayUpdate::DefaultPointer));
                            }
                        }
                    }
                    FrameData::Raw(_) => {
                        // Existing BGRA encode path — continues below
                    }
                }
                let bgra = frame.data.as_bgra_bytes().unwrap();

                // AVC444 dual-stream path
                if gfx.use_444 && encoder.supports_444() {
                    match encoder.encode_bgra_444(bgra, frame.width, frame.height, frame.stride) {
                        Ok(encoded) if !encoded.main_view.data.is_empty() => {
                            let encode_ms = t0.elapsed().as_secs_f64() * 1000.0;
                            let total_bytes =
                                encoded.main_view.data.len() + encoded.aux_view.data.len();
                            tracing::debug!(
                                display_frame = self.display_frame_count,
                                main_bytes = encoded.main_view.data.len(),
                                aux_bytes = encoded.aux_view.data.len(),
                                is_keyframe = encoded.main_view.is_keyframe,
                                encode_ms = format!("{:.1}", encode_ms),
                                "Display: sending AVC444 GFX frame"
                            );
                            {
                                let mut st = self.gfx_state.lock().unwrap();
                                st.last_encode_ms = encode_ms;
                                st.last_frame_bytes = total_bytes as u32;
                            }
                            self.frame_pacer.record_encode(encode_ms);
                            self.frame_pacer.record_sent(Instant::now());
                            self.update_backpressure(pending_acks);
                            self.idle_frames.record_sent(Instant::now());
                            return Ok(Some(DisplayUpdate::GfxFrame(GfxFrameUpdate {
                                h264_data: encoded.main_view.data,
                                width: frame.width as u16,
                                height: frame.height as u16,
                                enc_width: encoded.main_view.width as u16,
                                enc_height: encoded.main_view.height as u16,
                                is_keyframe: encoded.main_view.is_keyframe,
                                h264_aux: Some(encoded.aux_view.data),
                            })));
                        }
                        Ok(_) => {
                            tracing::warn!(
                                display_frame = self.display_frame_count,
                                "AVC444 encode returned EMPTY data — frame dropped!"
                            );
                            return Ok(Some(DisplayUpdate::DefaultPointer));
                        }
                        Err(e) => {
                            tracing::warn!(
                                display_frame = self.display_frame_count,
                                "AVC444 encode failed: {e}, falling back to AVC420"
                            );
                            // Fall through to AVC420 path below
                        }
                    }
                }

                // AVC420 path (default or fallback from AVC444 failure)
                match encoder.encode_bgra(bgra, frame.width, frame.height, frame.stride) {
                    Ok(encoded) if !encoded.data.is_empty() => {
                        let encode_ms = t0.elapsed().as_secs_f64() * 1000.0;
                        tracing::debug!(
                            display_frame = self.display_frame_count,
                            h264_bytes = encoded.data.len(),
                            is_keyframe = encoded.is_keyframe,
                            encode_ms = format!("{:.1}", encode_ms),
                            "Display: sending GFX frame"
                        );
                        {
                            let mut st = self.gfx_state.lock().unwrap();
                            st.last_encode_ms = encode_ms;
                            st.last_frame_bytes = encoded.data.len() as u32;
                        }
                        self.frame_pacer.record_encode(encode_ms);
                        self.frame_pacer.record_sent(Instant::now());
                        self.update_backpressure(pending_acks);
                        self.idle_frames.record_sent(Instant::now());
                        return Ok(Some(DisplayUpdate::GfxFrame(GfxFrameUpdate {
                            h264_data: encoded.data,
                            width: frame.width as u16,
                            height: frame.height as u16,
                            enc_width: encoded.width as u16,
                            enc_height: encoded.height as u16,
                            is_keyframe: encoded.is_keyframe,
                            h264_aux: None,
                        })));
                    }
                    Ok(_) => {
                        tracing::warn!(
                            display_frame = self.display_frame_count,
                            "H.264 encode returned EMPTY data — frame dropped!"
                        );
                        return Ok(Some(DisplayUpdate::DefaultPointer));
                    }
                    Err(e) => {
                        tracing::warn!(
                            display_frame = self.display_frame_count,
                            "H.264 encode failed: {e:#}"
                        );
                    }
                }
            }
        } else if self.encoder.is_some() && !gfx.hopeless {
            // H.264 encoder exists and GFX channel may still become ready — wait for it.
            // Mixing bitmap and GFX causes 0xd06 DECOMPRESSION_FAILED on reconnect.
            self.gfx_wait_frames += 1;
            if self.gfx_wait_frames == 1 || self.gfx_wait_frames.is_multiple_of(300) {
                tracing::warn!(
                    frame = self.gfx_wait_frames,
                    gfx_channel_open = gfx.channel_open,
                    gfx_caps_confirmed = gfx.caps_confirmed,
                    "GFX not ready — waiting for DVC channel/capabilities (white screen until ready)"
                );
            }
            return Ok(Some(DisplayUpdate::DefaultPointer));
        } else if gfx.hopeless {
            // GFX cannot become usable. Fall through to bitmap path, which only
            // works if capture is BGRA, not NV12/PixelBuffer.
            self.gfx_wait_frames += 1;
            if self.gfx_wait_frames == 1 || self.gfx_wait_frames.is_multiple_of(300) {
                if gfx.no_caps_timed_out {
                    tracing::warn!(
                        frame = self.gfx_wait_frames,
                        no_caps_frames = self.gfx_no_caps_frames,
                        "GFX channel opened but client capabilities were not received — falling back to bitmap path"
                    );
                } else {
                    tracing::warn!(
                        frame = self.gfx_wait_frames,
                        "GFX hopeless (client AVC disabled) — falling back to bitmap path"
                    );
                }
            }
        }

        // Bitmap path (only when GFX is not available at all, or gfx_hopeless with BGRA capture)
        // Requires BGRA raw bytes — PixelBuffer (NV12) frames cannot be bitmap-encoded.
        if let Err(block) = bitmap_fallback_decision(&gfx, BitmapSourceKind::from(&frame.data)) {
            match block {
                BitmapFallbackBlock::Nv12AfterNoCapsTimeout => {
                    tracing::warn!(
                        "GFX capabilities timed out + NV12 capture: cannot bitmap-encode PixelBuffer frames. \
                         Use software encoder or BGRA capture to get screen output."
                    );
                }
                BitmapFallbackBlock::Nv12AfterAvcDisabled => {
                    tracing::warn!(
                        "GFX hopeless + NV12 capture: cannot bitmap-encode PixelBuffer frames. \
                         Use software encoder or BGRA capture to get screen output."
                    );
                }
                BitmapFallbackBlock::UnexpectedPixelBuffer => {
                    tracing::warn!("PixelBuffer frame in bitmap path — should not happen");
                }
            }
            return Ok(Some(DisplayUpdate::DefaultPointer));
        }

        let FrameData::Raw(bgra_bitmap) = &frame.data else {
            tracing::warn!("Bitmap fallback reached without BGRA frame data");
            return Ok(Some(DisplayUpdate::DefaultPointer));
        };

        let now = Instant::now();
        let idle_decision = self.idle_frames.next_decision(&frame, now);
        if idle_decision == IdleFrameDecision::Skip {
            let skipped = self.idle_frames.record_skipped();
            if skipped == 1 || skipped.is_multiple_of(self.capture_config.frame_rate as u64) {
                tracing::debug!(
                    skipped_unchanged_frames = skipped,
                    "Display: skipping unchanged bitmap frame"
                );
            }
            return Ok(None);
        }

        // Send a single update covering the bounding box of all dirty rects.
        // A `None` here (no rects, or a degenerate region) falls through to the
        // full-frame path below.
        if let Some((min_x, min_y, w, h)) =
            dirty_bounding_box(&frame.dirty_rects, frame.width, frame.height)
        {
            let max_y = min_y + h;
            let Some(width) = NonZeroU16::new(w as u16) else {
                return Ok(None);
            };
            let Some(height) = NonZeroU16::new(h as u16) else {
                return Ok(None);
            };

            // Extract only the dirty region from the full frame buffer
            let bpp = 4usize;
            let dirty_stride = w as usize * bpp;
            let mut dirty_data = Vec::with_capacity(dirty_stride * h as usize);
            for row in min_y..max_y {
                let src_offset = row as usize * frame.stride + min_x as usize * bpp;
                let src_end = src_offset + dirty_stride;
                if src_end <= bgra_bitmap.len() {
                    dirty_data.extend_from_slice(&bgra_bitmap[src_offset..src_end]);
                }
            }

            let Some(stride) = NonZeroUsize::new(dirty_stride) else {
                return Ok(None);
            };

            let update = BitmapUpdate {
                x: min_x as u16,
                y: min_y as u16,
                width,
                height,
                format: RdpPixelFormat::BgrA32,
                data: Bytes::from(dirty_data),
                stride,
            };

            self.idle_frames.record_sent(Instant::now());
            return Ok(Some(DisplayUpdate::Bitmap(update)));
        }

        // No dirty rects available — send full frame (first frame or fallback)
        let Some(width) = NonZeroU16::new(frame.width as u16) else {
            return Ok(None);
        };
        let Some(height) = NonZeroU16::new(frame.height as u16) else {
            return Ok(None);
        };
        let Some(stride) = NonZeroUsize::new(frame.stride) else {
            return Ok(None);
        };

        let update = BitmapUpdate {
            x: 0,
            y: 0,
            width,
            height,
            format: RdpPixelFormat::BgrA32,
            data: bgra_bitmap.clone(),
            stride,
        };

        self.idle_frames.record_sent(Instant::now());
        Ok(Some(DisplayUpdate::Bitmap(update)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_display(width: u16, height: u16, fixed_resolution: bool) -> MacDisplay {
        MacDisplay::new(
            width,
            height,
            width,
            height,
            fixed_resolution,
            60,
            Quality::Balanced,
            macrdp_encode::EncoderPreference::Software,
            false,
            None,
            false,
            None,
            Arc::new(Mutex::new(GfxState::new(width, height, false))),
        )
    }

    fn pending_resize(display: &MacDisplay) -> Option<DesktopSize> {
        display
            .pending_resize
            .lock()
            .unwrap()
            .as_ref()
            .map(|(ds, _)| *ds)
    }

    struct StubEncoder {
        supports_pixel_buffer_input: bool,
        supports_444: bool,
    }

    impl VideoEncoder for StubEncoder {
        fn encode_bgra(
            &mut self,
            _data: &[u8],
            _width: u32,
            _height: u32,
            _stride: usize,
        ) -> Result<macrdp_encode::EncodedFrame> {
            anyhow::bail!("not used in capture-format tests")
        }

        fn encode_bgra_444(
            &mut self,
            _data: &[u8],
            _width: u32,
            _height: u32,
            _stride: usize,
        ) -> Result<macrdp_encode::Avc444EncodedFrame> {
            anyhow::bail!("not used in capture-format tests")
        }

        fn supports_pixel_buffer_input(&self) -> bool {
            self.supports_pixel_buffer_input
        }

        fn set_bitrate(&mut self, _bitrate_bps: u32) {}

        fn force_keyframe(&mut self) {}

        fn supports_444(&self) -> bool {
            self.supports_444
        }
    }

    #[tokio::test]
    async fn flexible_display_adopts_client_requested_smaller_size() {
        let mut display = test_display(1920, 1080, false);

        display.request_resize(1280, 720);

        assert_eq!(
            display.size().await,
            DesktopSize {
                width: 1280,
                height: 720,
            }
        );
        assert_eq!(
            pending_resize(&display),
            Some(DesktopSize {
                width: 1280,
                height: 720,
            })
        );
    }

    #[tokio::test]
    async fn flexible_display_adopts_client_requested_larger_size() {
        let mut display = test_display(1920, 1080, false);

        display.request_resize(3840, 2160);

        assert_eq!(
            display.size().await,
            DesktopSize {
                width: 3840,
                height: 2160,
            }
        );
    }

    #[tokio::test]
    async fn flexible_display_clamps_at_encoder_limit() {
        let mut display = test_display(1920, 1080, false);

        display.request_resize(5000, 3000);

        assert_eq!(
            display.size().await,
            DesktopSize {
                width: MAX_ENCODE_LONG,
                height: MAX_ENCODE_SHORT,
            },
            "landscape: clamped to 3840x2160 encoder limit"
        );
    }

    #[tokio::test]
    async fn flexible_display_clamps_portrait_at_encoder_limit() {
        let mut display = test_display(1080, 1920, false);

        display.request_resize(3000, 5000);

        assert_eq!(
            display.size().await,
            DesktopSize {
                width: MAX_ENCODE_SHORT,
                height: MAX_ENCODE_LONG,
            },
            "portrait: clamped to 2160x3840 encoder limit"
        );
    }

    #[tokio::test]
    async fn fixed_display_ignores_client_requested_size() {
        let mut display = test_display(1920, 1080, true);

        display.request_resize(1280, 720);

        assert_eq!(
            display.size().await,
            DesktopSize {
                width: 1920,
                height: 1080,
            }
        );
        assert_eq!(pending_resize(&display), None);
    }

    #[tokio::test]
    async fn display_control_layout_requests_resize() {
        let mut display = test_display(1920, 1080, false);
        let layout =
            DisplayControlMonitorLayout::new_single_primary_monitor(1440, 900, None, None).unwrap();

        display.request_layout(layout);

        assert_eq!(
            display.size().await,
            DesktopSize {
                width: 1440,
                height: 900,
            }
        );
        assert_eq!(
            pending_resize(&display),
            Some(DesktopSize {
                width: 1440,
                height: 900,
            })
        );
    }

    #[tokio::test]
    async fn display_control_multi_monitor_layout_is_deferred() {
        let mut display = test_display(1920, 1080, false);
        let primary =
            ironrdp_displaycontrol::pdu::MonitorLayoutEntry::new_primary(1440, 900).unwrap();
        let secondary = ironrdp_displaycontrol::pdu::MonitorLayoutEntry::new_secondary(1024, 768)
            .unwrap()
            .with_position(1440, 0)
            .unwrap();
        let layout = DisplayControlMonitorLayout::new(&[primary, secondary]).unwrap();

        display.request_layout(layout);

        assert_eq!(
            display.size().await,
            DesktopSize {
                width: 1920,
                height: 1080,
            }
        );
        assert_eq!(pending_resize(&display), None);
    }

    #[test]
    fn mouse_scale_is_1_when_native_equals_configured() {
        let display = test_display(1920, 1080, false);
        let (sx, sy) = *display.mouse_scale().lock().unwrap();
        assert!((sx - 1.0).abs() < 1e-9, "scale_x should be 1.0, got {sx}");
        assert!((sy - 1.0).abs() < 1e-9, "scale_y should be 1.0, got {sy}");
    }

    #[test]
    fn mouse_scale_reflects_non_native_initial_resolution() {
        // Software encoder with a smaller configured resolution: scale < 1.
        let display = MacDisplay::new(
            1280,
            720,
            1920,
            1080,
            true, // fixed
            60,
            Quality::Balanced,
            macrdp_encode::EncoderPreference::Software,
            false,
            None,
            false,
            None,
            Arc::new(Mutex::new(GfxState::new(1280, 720, false))),
        );
        let (sx, sy) = *display.mouse_scale().lock().unwrap();
        let expected_x = 1280.0 / 1920.0;
        let expected_y = 720.0 / 1080.0;
        assert!(
            (sx - expected_x).abs() < 1e-9,
            "scale_x should be {expected_x}, got {sx}"
        );
        assert!(
            (sy - expected_y).abs() < 1e-9,
            "scale_y should be {expected_y}, got {sy}"
        );
    }

    #[test]
    fn mouse_scale_updates_on_client_resize() {
        // Regression: mouse coords were injected using the original startup scale
        // even after a client requested a smaller resolution, causing cursor mismatch.
        let mut display = MacDisplay::new(
            1920,
            1080,
            1920,
            1080,
            false,
            60,
            Quality::Balanced,
            macrdp_encode::EncoderPreference::Software,
            false,
            None,
            false,
            None,
            Arc::new(Mutex::new(GfxState::new(1920, 1080, false))),
        );
        let scale = display.mouse_scale();

        display.request_resize(1280, 720);

        let (sx, sy) = *scale.lock().unwrap();
        let expected_x = 1280.0 / 1920.0;
        let expected_y = 720.0 / 1080.0;
        assert!(
            (sx - expected_x).abs() < 1e-9,
            "scale_x should update to {expected_x} after resize, got {sx}"
        );
        assert!(
            (sy - expected_y).abs() < 1e-9,
            "scale_y should update to {expected_y} after resize, got {sy}"
        );
    }

    #[test]
    fn rapid_layout_requests_coalesce_to_final_size() {
        let mut display = test_display(1920, 1080, false);

        for (w, h) in [(2000, 1100), (2200, 1300), (2560, 1440)] {
            let layout =
                DisplayControlMonitorLayout::new_single_primary_monitor(w, h, None, None).unwrap();
            display.request_layout(layout);
        }

        assert_eq!(
            pending_resize(&display),
            Some(DesktopSize {
                width: 2560,
                height: 1440,
            }),
            "only the final size should be pending after rapid requests"
        );
    }

    #[test]
    fn avc420_uses_nv12_only_when_encoder_supports_zero_copy_pixel_buffers() {
        let encoder = StubEncoder {
            supports_pixel_buffer_input: true,
            supports_444: false,
        };

        assert_eq!(
            capture_pixel_format(false, Some(&encoder)),
            CapturePixelFormat::Nv12
        );
    }

    #[test]
    fn software_and_no_encoder_capture_use_bgra_for_bgra_encoding_or_bitmap_fallback() {
        let software_encoder = StubEncoder {
            supports_pixel_buffer_input: false,
            supports_444: false,
        };

        assert_eq!(
            capture_pixel_format(false, Some(&software_encoder)),
            CapturePixelFormat::Bgra
        );
        assert_eq!(capture_pixel_format(false, None), CapturePixelFormat::Bgra);
    }

    #[test]
    fn avc444_capture_uses_bgra_even_with_zero_copy_encoder() {
        let encoder = StubEncoder {
            supports_pixel_buffer_input: true,
            supports_444: true,
        };

        assert_eq!(
            capture_pixel_format(true, Some(&encoder)),
            CapturePixelFormat::Bgra
        );
    }

    #[test]
    fn dirty_bounding_box_cases() {
        fn r(x: u32, y: u32, width: u32, height: u32) -> Rect {
            Rect {
                x,
                y,
                width,
                height,
            }
        }

        struct Case {
            name: &'static str,
            rects: Vec<Rect>,
            frame: (u32, u32),
            expected: Option<(u32, u32, u32, u32)>,
        }

        let cases = [
            Case {
                name: "no rects -> nothing to send",
                rects: vec![],
                frame: (1920, 1080),
                expected: None,
            },
            Case {
                name: "single rect passes through",
                rects: vec![r(10, 20, 30, 40)],
                frame: (1920, 1080),
                expected: Some((10, 20, 30, 40)),
            },
            Case {
                name: "union spans all rects",
                rects: vec![r(10, 10, 10, 10), r(100, 50, 20, 30)],
                frame: (1920, 1080),
                expected: Some((10, 10, 110, 70)),
            },
            Case {
                name: "far edges clamp to frame",
                rects: vec![r(1900, 1060, 100, 100)],
                frame: (1920, 1080),
                expected: Some((1900, 1060, 20, 20)),
            },
            Case {
                name: "fully off-screen collapses to none",
                rects: vec![r(1920, 1080, 10, 10)],
                frame: (1920, 1080),
                expected: None,
            },
            Case {
                name: "zero-area rect collapses to none",
                rects: vec![r(50, 50, 0, 0)],
                frame: (1920, 1080),
                expected: None,
            },
            Case {
                name: "full-frame rect",
                rects: vec![r(0, 0, 1920, 1080)],
                frame: (1920, 1080),
                expected: Some((0, 0, 1920, 1080)),
            },
        ];

        for c in &cases {
            assert_eq!(
                dirty_bounding_box(&c.rects, c.frame.0, c.frame.1),
                c.expected,
                "{}",
                c.name
            );
        }
    }

    #[test]
    fn adaptive_bitrate_respects_interval_and_hysteresis() {
        let mut gfx = GfxState::new(1920, 1080, false);
        let mut controller = AdaptiveBitrateController::new(10_000_000);

        gfx.network_quality = 0.8;
        assert_eq!(controller.next_update(29, &gfx), None);
        assert_eq!(controller.next_update(30, &gfx), Some(8_000_000));

        gfx.network_quality = 0.75;
        assert_eq!(controller.next_update(60, &gfx), None);

        gfx.network_quality = 0.5;
        assert_eq!(controller.next_update(60, &gfx), Some(5_000_000));
    }

    #[test]
    fn adaptive_bitrate_clamps_to_floor_and_base() {
        let mut gfx = GfxState::new(1920, 1080, false);
        let controller = AdaptiveBitrateController::new(10_000_000);

        gfx.network_quality = 0.01;
        assert_eq!(controller.recommended_bitrate(&gfx), 500_000);

        gfx.network_quality = 1.5;
        assert_eq!(controller.recommended_bitrate(&gfx), 10_000_000);
    }

    fn test_frame_with_dirty_state(
        dirty_rects_available: bool,
        dirty_rects: Vec<macrdp_capture::Rect>,
    ) -> CapturedFrame {
        CapturedFrame {
            width: 16,
            height: 16,
            data: FrameData::Raw(Bytes::from(vec![0u8; 16 * 16 * 4])),
            stride: 16 * 4,
            timestamp_us: 0,
            dirty_rects,
            dirty_rects_available,
        }
    }

    #[test]
    fn idle_controller_skips_known_unchanged_frames_until_interval() {
        let mut controller = IdleFrameController::new(true, Some(Duration::from_secs(5)));
        let changed = test_frame_with_dirty_state(
            true,
            vec![macrdp_capture::Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }],
        );
        let unchanged = test_frame_with_dirty_state(true, vec![]);
        let start = Instant::now();

        assert_eq!(
            controller.next_decision(&changed, start),
            IdleFrameDecision::Encode {
                force_keyframe: false
            }
        );
        controller.record_sent(start);

        assert_eq!(
            controller.next_decision(&unchanged, start + Duration::from_secs(4)),
            IdleFrameDecision::Skip
        );
        assert_eq!(
            controller.next_decision(&unchanged, start + Duration::from_secs(5)),
            IdleFrameDecision::Encode {
                force_keyframe: true
            }
        );
    }

    #[test]
    fn idle_controller_treats_unknown_dirty_rects_as_changed() {
        let mut controller = IdleFrameController::new(true, Some(Duration::from_secs(5)));
        let unknown = test_frame_with_dirty_state(false, vec![]);
        let start = Instant::now();
        controller.record_sent(start);

        assert_eq!(
            controller.next_decision(&unknown, start + Duration::from_secs(1)),
            IdleFrameDecision::Encode {
                force_keyframe: false
            }
        );
    }

    #[test]
    fn idle_controller_can_disable_skipping() {
        let mut controller = IdleFrameController::new(false, Some(Duration::from_secs(5)));
        let unchanged = test_frame_with_dirty_state(true, vec![]);
        let start = Instant::now();
        controller.record_sent(start);

        assert_eq!(
            controller.next_decision(&unchanged, start + Duration::from_secs(1)),
            IdleFrameDecision::Encode {
                force_keyframe: false
            }
        );
    }

    #[test]
    fn gfx_decision_waits_while_encoder_available_and_gfx_negotiates() {
        let mut state = GfxState::new(1920, 1080, false);

        assert_eq!(
            gfx_pipeline_decision(&state, true, false, 0, 300),
            GfxPipelineDecision {
                ready: false,
                use_444: false,
                hopeless: false,
                no_caps_timed_out: false,
                channel_open: false,
                caps_confirmed: false,
            }
        );

        state.channel_id = Some(1);
        assert_eq!(
            gfx_pipeline_decision(&state, true, false, 299, 300),
            GfxPipelineDecision {
                ready: false,
                use_444: false,
                hopeless: false,
                no_caps_timed_out: false,
                channel_open: true,
                caps_confirmed: false,
            }
        );
    }

    #[test]
    fn gfx_decision_falls_back_when_open_channel_never_sends_capabilities() {
        let mut state = GfxState::new(1920, 1080, false);
        state.channel_id = Some(1);

        assert_eq!(
            gfx_pipeline_decision(&state, true, false, 300, 300),
            GfxPipelineDecision {
                ready: false,
                use_444: false,
                hopeless: true,
                no_caps_timed_out: true,
                channel_open: true,
                caps_confirmed: false,
            }
        );
    }

    #[test]
    fn gfx_decision_falls_back_only_after_confirmed_no_avc_support() {
        let mut state = GfxState::new(1920, 1080, false);
        state.channel_id = Some(1);
        state.caps_confirmed = true;

        assert_eq!(
            gfx_pipeline_decision(&state, true, false, 0, 300),
            GfxPipelineDecision {
                ready: false,
                use_444: false,
                hopeless: true,
                no_caps_timed_out: false,
                channel_open: true,
                caps_confirmed: true,
            }
        );
    }

    #[test]
    fn bitmap_fallback_for_no_avc_client_requires_bgra_frame_data() {
        let mut state = GfxState::new(1920, 1080, false);
        state.channel_id = Some(1);
        state.caps_confirmed = true;

        let gfx = gfx_pipeline_decision(&state, true, false, 0, 300);

        assert_eq!(
            bitmap_fallback_decision(&gfx, BitmapSourceKind::Bgra),
            Ok(())
        );
        assert_eq!(
            bitmap_fallback_decision(&gfx, BitmapSourceKind::PixelBuffer),
            Err(BitmapFallbackBlock::Nv12AfterAvcDisabled)
        );
    }

    #[test]
    fn bitmap_fallback_for_missing_caps_timeout_requires_bgra_frame_data() {
        let mut state = GfxState::new(1920, 1080, false);
        state.channel_id = Some(1);

        let gfx = gfx_pipeline_decision(&state, true, false, 300, 300);

        assert_eq!(
            bitmap_fallback_decision(&gfx, BitmapSourceKind::Bgra),
            Ok(())
        );
        assert_eq!(
            bitmap_fallback_decision(&gfx, BitmapSourceKind::PixelBuffer),
            Err(BitmapFallbackBlock::Nv12AfterNoCapsTimeout)
        );
    }

    #[test]
    fn gfx_decision_uses_gfx_after_avc420_is_confirmed() {
        let mut state = GfxState::new(1920, 1080, false);
        state.channel_id = Some(1);
        state.caps_confirmed = true;
        state.avc420_supported = true;

        assert_eq!(
            gfx_pipeline_decision(&state, true, false, 0, 300),
            GfxPipelineDecision {
                ready: true,
                use_444: false,
                hopeless: false,
                no_caps_timed_out: false,
                channel_open: true,
                caps_confirmed: true,
            }
        );
    }

    #[test]
    fn gfx_decision_enables_avc444_only_when_config_and_client_allow_it() {
        let mut state = GfxState::new(1920, 1080, true);
        state.channel_id = Some(1);
        state.caps_confirmed = true;
        state.avc420_supported = true;

        assert!(!gfx_pipeline_decision(&state, true, true, 0, 300).use_444);

        state.avc444_supported = true;
        assert!(gfx_pipeline_decision(&state, true, true, 0, 300).use_444);
        assert!(!gfx_pipeline_decision(&state, true, false, 0, 300).use_444);
    }

    #[test]
    fn vt_encoder_falls_back_to_software_at_non_native_resolution() {
        // At native resolution, hardware preference is preserved
        let display = MacDisplay::new(
            1920,
            1080,
            1920,
            1080,
            false,
            60,
            Quality::Balanced,
            macrdp_encode::EncoderPreference::Auto,
            false,
            None,
            false,
            None,
            Arc::new(Mutex::new(GfxState::new(1920, 1080, false))),
        );
        assert_eq!(
            display.effective_encoder_pref(),
            macrdp_encode::EncoderPreference::Auto
        );

        // After resize away from native, hardware preference should become software
        let display = MacDisplay::new(
            1280,
            720,
            1920,
            1080,
            false,
            60,
            Quality::Balanced,
            macrdp_encode::EncoderPreference::Auto,
            false,
            None,
            false,
            None,
            Arc::new(Mutex::new(GfxState::new(1280, 720, false))),
        );
        assert_eq!(
            display.effective_encoder_pref(),
            macrdp_encode::EncoderPreference::Software
        );

        // Software preference is unchanged regardless of native match
        let display = MacDisplay::new(
            1280,
            720,
            1920,
            1080,
            false,
            60,
            Quality::Balanced,
            macrdp_encode::EncoderPreference::Software,
            false,
            None,
            false,
            None,
            Arc::new(Mutex::new(GfxState::new(1280, 720, false))),
        );
        assert_eq!(
            display.effective_encoder_pref(),
            macrdp_encode::EncoderPreference::Software
        );
    }

    #[test]
    fn backpressure_skip_count_scales_with_pending_acks() {
        assert_eq!(backpressure_skip_count(0), 0);
        assert_eq!(backpressure_skip_count(4), 0);
        assert_eq!(backpressure_skip_count(5), 1);
        assert_eq!(backpressure_skip_count(9), 1);
        assert_eq!(backpressure_skip_count(10), 2);
        assert_eq!(backpressure_skip_count(19), 2);
        assert_eq!(backpressure_skip_count(20), 3);
        assert_eq!(backpressure_skip_count(100), 3);
    }

    #[test]
    fn frame_pacer_allows_first_frame_immediately() {
        let pacer = FramePacer::new(60);
        assert!(pacer.should_send(Instant::now()));
    }

    #[test]
    fn frame_pacer_uses_base_interval_when_encode_is_fast() {
        let mut pacer = FramePacer::new(60);
        pacer.record_encode(5.0); // 5ms encode at 60fps (16.7ms interval)

        let effective = pacer.effective_interval();
        let base = Duration::from_secs_f64(1.0 / 60.0);
        assert_eq!(effective, base, "fast encode should use base interval");
    }

    #[test]
    fn frame_pacer_stretches_interval_when_encode_is_slow() {
        let mut pacer = FramePacer::new(60);
        // Seed EWMA with slow encode times
        for _ in 0..20 {
            pacer.record_encode(25.0); // 25ms encode, exceeds 16.7ms frame interval
        }

        let effective = pacer.effective_interval();
        let base = Duration::from_secs_f64(1.0 / 60.0);
        assert!(
            effective > base,
            "slow encode should stretch interval: {:?} > {:?}",
            effective,
            base,
        );
        // 25ms * 1.2 = 30ms effective interval → ~33fps
        let expected_ms = 30.0;
        let actual_ms = effective.as_secs_f64() * 1000.0;
        assert!(
            (actual_ms - expected_ms).abs() < 1.0,
            "effective interval should be ~30ms, got {:.1}ms",
            actual_ms,
        );
    }

    #[test]
    fn frame_pacer_blocks_send_within_interval() {
        let mut pacer = FramePacer::new(60);
        let now = Instant::now();
        pacer.record_sent(now);

        // Immediately after sending, should_send returns false
        assert!(!pacer.should_send(now));
        assert!(!pacer.should_send(now + Duration::from_millis(5)));

        // After the base interval, should_send returns true
        assert!(pacer.should_send(now + Duration::from_millis(17)));
    }
}

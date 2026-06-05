//! Video encoding abstraction (H.264 via VideoToolbox / OpenH264)

#[cfg(target_os = "macos")]
pub mod color_convert;
mod openh264_enc;
#[cfg(target_os = "macos")]
mod videotoolbox;
pub mod yuv444_split;
#[cfg(target_os = "macos")]
use anyhow::Result;
use bytes::Bytes;

pub use openh264_enc::OpenH264Encoder;
#[cfg(target_os = "macos")]
pub use videotoolbox::VtEncoder;

/// Encoded frame output
pub struct EncodedFrame {
    /// H.264 NAL units (Annex B format)
    pub data: Bytes,
    /// Whether this is a key frame (IDR)
    pub is_keyframe: bool,
    pub width: u32,
    pub height: u32,
}

/// AVC444 dual-stream encoded result
pub struct Avc444EncodedFrame {
    /// Stream1: Main YUV420 H.264 (luma + downsampled chroma)
    pub main_view: EncodedFrame,
    /// Stream2: Auxiliary YUV420 H.264 (chroma compensation)
    pub aux_view: EncodedFrame,
}

impl Avc444EncodedFrame {
    pub fn new(
        main_data: Vec<u8>,
        main_keyframe: bool,
        aux_data: Vec<u8>,
        aux_keyframe: bool,
        w: u32,
        h: u32,
    ) -> Self {
        Self {
            main_view: EncodedFrame {
                data: Bytes::from(main_data),
                is_keyframe: main_keyframe,
                width: w,
                height: h,
            },
            aux_view: EncodedFrame {
                data: Bytes::from(aux_data),
                is_keyframe: aux_keyframe,
                width: w,
                height: h,
            },
        }
    }
}

/// Reusable buffers for AVC444 YUV444 → dual YUV420 split
pub(crate) struct Yuv444SplitBufs {
    pub y444: Vec<u8>,
    pub u444: Vec<u8>,
    pub v444: Vec<u8>,
    pub main_view: yuv444_split::Yuv420Frame,
    pub aux_view: yuv444_split::Yuv420Frame,
}

impl Yuv444SplitBufs {
    pub fn new(width: u32, height: u32) -> Self {
        let full = (width * height) as usize;
        Self {
            y444: vec![0u8; full],
            u444: vec![0u8; full],
            v444: vec![0u8; full],
            main_view: yuv444_split::Yuv420Frame::new(width, height),
            aux_view: yuv444_split::Yuv420Frame::new(width, height),
        }
    }

    pub fn split_bgra(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: usize,
        enc_w: u32,
        enc_h: u32,
    ) {
        yuv444_split::bgra_to_yuv444(
            data,
            width,
            height,
            stride,
            &mut self.y444,
            &mut self.u444,
            &mut self.v444,
        );
        yuv444_split::yuv444_split_to_yuv420(
            &self.y444,
            &self.u444,
            &self.v444,
            enc_w,
            enc_h,
            &mut self.main_view,
            &mut self.aux_view,
        );
    }
}

/// Quality preset
#[derive(Debug, Clone, Copy)]
pub enum Quality {
    LowLatency,
    Balanced,
    HighQuality,
}

/// Video encoder trait
pub trait VideoEncoder: Send {
    /// AVC420 encode (existing)
    fn encode_bgra(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: usize,
    ) -> Result<EncodedFrame>;

    /// AVC444 dual-stream encode.
    /// Internally performs BGRA -> YUV444 -> B-area split -> dual session encode.
    fn encode_bgra_444(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: usize,
    ) -> Result<Avc444EncodedFrame>;

    /// Zero-copy encode from CVPixelBuffer (NV12). Default returns error (unsupported).
    /// Only VtEncoder overrides this.
    fn encode_pixel_buffer(
        &mut self,
        _ptr: *mut std::ffi::c_void,
        _force_keyframe: bool,
    ) -> Result<EncodedFrame> {
        anyhow::bail!("encode_pixel_buffer not supported by this encoder")
    }

    /// Whether this encoder can consume NV12 CVPixelBuffer frames directly.
    fn supports_pixel_buffer_input(&self) -> bool {
        false
    }

    fn set_bitrate(&mut self, bitrate_bps: u32);
    fn force_keyframe(&mut self);

    /// Whether this encoder supports AVC444 dual-stream encoding
    fn supports_444(&self) -> bool;
}

/// Align a dimension up to the nearest multiple of 16 (H.264 macroblock size)
pub fn align16(v: u32) -> u32 {
    (v + 15) & !15
}

/// Calculate optimal bitrate for screen content
pub fn screen_bitrate(width: u32, height: u32, fps: f32, quality: Quality) -> u32 {
    let pixels = width as f64 * height as f64;
    let base_bpp = match quality {
        Quality::LowLatency => 8.0,
        Quality::Balanced => 16.0,
        Quality::HighQuality => 24.0,
    };
    let fps_factor = (fps as f64 / 30.0).max(1.0);
    (pixels * base_bpp * fps_factor) as u32
}

/// Encoder preference
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EncoderPreference {
    /// OpenH264 CPU encoder — full P-frame support, higher latency (~40ms)
    Software,
    /// VideoToolbox GPU encoder — low latency (~6ms), supports P-frames
    Hardware,
    /// Prefer platform hardware when available, with software fallback.
    Auto,
}

impl EncoderPreference {
    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s.map(|s| s.to_lowercase()).as_deref() {
            Some("hardware") | Some("gpu") | Some("videotoolbox") | Some("vt") => Self::Hardware,
            Some("software") | Some("cpu") | Some("openh264") | Some("oh264") => Self::Software,
            _ => Self::Auto,
        }
    }

    /// Whether this preference should prepare for platform hardware encode.
    pub fn prefers_hardware_on_this_platform(self) -> bool {
        cfg!(target_os = "macos") && matches!(self, Self::Hardware | Self::Auto)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EncoderBackend {
    VideoToolbox,
    OpenH264,
}

#[cfg(target_os = "macos")]
fn encoder_backend_order(preference: EncoderPreference) -> &'static [EncoderBackend] {
    match preference {
        EncoderPreference::Software => &[EncoderBackend::OpenH264],
        EncoderPreference::Hardware | EncoderPreference::Auto => {
            &[EncoderBackend::VideoToolbox, EncoderBackend::OpenH264]
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn encoder_backend_order(_preference: EncoderPreference) -> &'static [EncoderBackend] {
    &[EncoderBackend::OpenH264]
}

/// Create an H.264 encoder based on preference.
/// When `mode_444` is true, the encoder tries to initialize AVC444 support.
/// AVC444 is optional: if setup fails, creation retries AVC420 before giving up.
pub fn create_encoder(
    width: u32,
    height: u32,
    fps: f32,
    _quality: Quality,
    preference: EncoderPreference,
    mode_444: bool,
    bitrate: u32,
) -> Result<Box<dyn VideoEncoder>> {
    let enc_w = align16(width);
    let enc_h = align16(height);

    // Hardware: VideoToolbox GPU encoder
    #[cfg(target_os = "macos")]
    if encoder_backend_order(preference).contains(&EncoderBackend::VideoToolbox) {
        match VtEncoder::new(enc_w, enc_h, fps, bitrate, mode_444) {
            Ok(encoder) => {
                tracing::info!(
                    enc_w,
                    enc_h,
                    mode_444,
                    "Using VideoToolbox hardware encoder (GPU)"
                );
                return Ok(Box::new(encoder));
            }
            Err(e) => {
                if mode_444 {
                    tracing::warn!(
                        "VideoToolbox AVC444 unavailable: {e}; retrying VideoToolbox AVC420"
                    );
                    match VtEncoder::new(enc_w, enc_h, fps, bitrate, false) {
                        Ok(encoder) => {
                            tracing::info!(
                                enc_w,
                                enc_h,
                                mode_444 = false,
                                "Using VideoToolbox hardware encoder (GPU)"
                            );
                            return Ok(Box::new(encoder));
                        }
                        Err(e) => {
                            tracing::warn!(
                                "VideoToolbox AVC420 unavailable after AVC444 retry: {e}; falling back to OpenH264"
                            );
                        }
                    }
                } else {
                    tracing::warn!("VideoToolbox unavailable: {e}, falling back to OpenH264");
                }
            }
        }
    }

    // Software / Auto: OpenH264 CPU encoder (full P-frame support)
    match OpenH264Encoder::new(enc_w, enc_h, fps, bitrate, mode_444) {
        Ok(encoder) => {
            tracing::info!(
                enc_w,
                enc_h,
                mode_444,
                "Using OpenH264 software encoder (CPU)"
            );
            Ok(Box::new(encoder))
        }
        Err(e) if mode_444 => {
            tracing::warn!("OpenH264 AVC444 unavailable: {e}; retrying OpenH264 AVC420");
            let encoder = OpenH264Encoder::new(enc_w, enc_h, fps, bitrate, false)?;
            tracing::info!(
                enc_w,
                enc_h,
                mode_444 = false,
                "Using OpenH264 software encoder (CPU)"
            );
            Ok(Box::new(encoder))
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_preference_aliases_parse_as_expected() {
        assert_eq!(
            EncoderPreference::from_str_opt(Some("videotoolbox")),
            EncoderPreference::Hardware
        );
        assert_eq!(
            EncoderPreference::from_str_opt(Some("OPENH264")),
            EncoderPreference::Software
        );
        assert_eq!(
            EncoderPreference::from_str_opt(Some("auto")),
            EncoderPreference::Auto
        );
        assert_eq!(
            EncoderPreference::from_str_opt(None),
            EncoderPreference::Auto
        );
    }

    #[test]
    fn auto_prefers_videotoolbox_on_macos_with_openh264_fallback() {
        let order = encoder_backend_order(EncoderPreference::Auto);

        #[cfg(target_os = "macos")]
        assert_eq!(
            order,
            &[EncoderBackend::VideoToolbox, EncoderBackend::OpenH264]
        );

        #[cfg(not(target_os = "macos"))]
        assert_eq!(order, &[EncoderBackend::OpenH264]);
    }

    #[test]
    fn software_preference_skips_videotoolbox() {
        assert_eq!(
            encoder_backend_order(EncoderPreference::Software),
            &[EncoderBackend::OpenH264]
        );
    }

    #[test]
    fn hardware_preparation_matches_platform_support() {
        #[cfg(target_os = "macos")]
        {
            assert!(EncoderPreference::Auto.prefers_hardware_on_this_platform());
            assert!(EncoderPreference::Hardware.prefers_hardware_on_this_platform());
        }

        #[cfg(not(target_os = "macos"))]
        {
            assert!(!EncoderPreference::Auto.prefers_hardware_on_this_platform());
            assert!(!EncoderPreference::Hardware.prefers_hardware_on_this_platform());
        }

        assert!(!EncoderPreference::Software.prefers_hardware_on_this_platform());
    }
}

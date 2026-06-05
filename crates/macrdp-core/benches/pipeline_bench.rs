// Performance benchmarks for the non-encoder stages of the macrdp pipeline.
//
// The encoder/color-conversion benches live in `macrdp-encode`; this harness
// covers the two remaining hot stages that need access to `macrdp-capture`
// and `ironrdp-server`'s GFX code:
//
//   * capture copy     — the per-frame BGRA copy out of the locked CVPixelBuffer
//   * GFX PDU assembly  — wrapping an encoded frame into ZGFX-segmented PDUs
//
// plus a composite encode -> PDU latency measurement on macOS.
//
// Run with: cargo bench -p macrdp-core

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use ironrdp_server::gfx::{GfxHandler, GfxState};
use ironrdp_server::GfxFrameUpdate;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Representative encoded-frame size. The first IDR observed in testing was
/// ~0.85 MB at 2560x1440; later P-frames are far smaller, so this is a
/// pessimistic-but-realistic payload for PDU assembly cost.
const REPRESENTATIVE_FRAME_BYTES: usize = 880_000;

fn dummy_encoded_frame(width: u16, height: u16, bytes: usize) -> GfxFrameUpdate {
    // PDU assembly only copies/wraps the H.264 payload; its contents are
    // irrelevant to the wire-encoding cost, so a filled buffer is enough.
    GfxFrameUpdate {
        h264_data: Bytes::from(vec![0x41u8; bytes]),
        width,
        height,
        enc_width: width,
        enc_height: height,
        is_keyframe: true,
        h264_aux: None,
    }
}

// ---------------------------------------------------------------------------
// Capture copy benchmark
// ---------------------------------------------------------------------------

/// Mirrors the `FrameData::Raw` path in `macrdp_capture::extract_frame`, which
/// does `Bytes::copy_from_slice(pixels)` — a fresh heap allocation plus a full
/// memcpy of the locked BGRA pixel buffer on every captured frame. This is the
/// allocation hotspot flagged for cleanup, so the bench guards the copy cost
/// against regressions (and would show the win if the buffer were ever reused).
fn bench_capture_copy(c: &mut Criterion) {
    let mut group = c.benchmark_group("capture_copy");

    for &(label, width, height) in &[
        ("bgra_copy_4k", 3840usize, 2160usize),
        ("bgra_copy_1080p", 1920, 1080),
    ] {
        let stride = width * 4;
        let pixels = vec![0u8; stride * height];
        group.throughput(criterion::Throughput::Bytes(pixels.len() as u64));
        group.bench_function(label, |b| {
            b.iter(|| {
                let copied = Bytes::copy_from_slice(&pixels);
                criterion::black_box(copied);
            })
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// GFX PDU assembly benchmark
// ---------------------------------------------------------------------------

/// `GfxHandler::create_frame_pdu` for an AVC420 frame: StartFrame +
/// WireToSurface1 + EndFrame, concatenated and wrapped in a single ZGFX
/// segmented buffer. Two cases:
///   * first_frame  — fresh state, so ResetGraphics/CreateSurface are included
///   * steady_state — surface already created (the common per-frame path)
fn bench_gfx_pdu_assembly(c: &mut Criterion) {
    let (width, height) = (2560u16, 1440u16);
    let frame = dummy_encoded_frame(width, height, REPRESENTATIVE_FRAME_BYTES);

    let mut group = c.benchmark_group("gfx_pdu_assembly");
    group.throughput(criterion::Throughput::Bytes(
        REPRESENTATIVE_FRAME_BYTES as u64,
    ));

    group.bench_function("first_frame", |b| {
        b.iter_batched(
            || GfxState::new(width, height, false),
            |mut state| GfxHandler::create_frame_pdu(&mut state, &frame),
            BatchSize::SmallInput,
        )
    });

    group.bench_function("steady_state", |b| {
        let mut state = GfxState::new(width, height, false);
        // Prime past the one-time surface setup so we measure the per-frame path.
        let _ = GfxHandler::create_frame_pdu(&mut state, &frame);
        b.iter(|| GfxHandler::create_frame_pdu(&mut state, &frame))
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// End-to-end frame latency (capture copy is excluded: it needs a live capture
// session, so this composite covers encode -> PDU assembly, the deterministic
// tail of the pipeline).
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn bench_end_to_end_frame(c: &mut Criterion) {
    use macrdp_encode::{align16, VideoEncoder, VtEncoder};

    let mut group = c.benchmark_group("end_to_end_frame");
    group.sample_size(20);

    for &(label, width, height, bitrate) in &[
        ("encode_plus_pdu_1080p", 1920u32, 1080u32, 30_000_000u32),
        ("encode_plus_pdu_1440p", 2560, 1440, 40_000_000),
    ] {
        let stride = width as usize * 4;
        let bgra = vec![0u8; stride * height as usize];
        let mut encoder =
            match VtEncoder::new(align16(width), align16(height), 120.0, bitrate, false) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("skipping {label}: VtEncoder unavailable: {e}");
                    continue;
                }
            };

        let (vw, vh) = (width as u16, height as u16);
        let mut state = GfxState::new(vw, vh, false);

        group.bench_function(label, |b| {
            b.iter(|| {
                let enc = encoder.encode_bgra(&bgra, width, height, stride).unwrap();
                let frame = GfxFrameUpdate {
                    h264_data: enc.data,
                    width: vw,
                    height: vh,
                    enc_width: align16(width) as u16,
                    enc_height: align16(height) as u16,
                    is_keyframe: enc.is_keyframe,
                    h264_aux: None,
                };
                GfxHandler::create_frame_pdu(&mut state, &frame)
            })
        });
    }

    group.finish();
}

#[cfg(target_os = "macos")]
criterion_group!(
    benches,
    bench_capture_copy,
    bench_gfx_pdu_assembly,
    bench_end_to_end_frame
);
#[cfg(not(target_os = "macos"))]
criterion_group!(benches, bench_capture_copy, bench_gfx_pdu_assembly);
criterion_main!(benches);

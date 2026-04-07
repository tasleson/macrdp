# Encode Speed Optimization Design

**Date**: 2026-04-07
**Status**: Draft
**Scope**: Reduce VT encode latency and improve pipeline throughput for 4K@30fps target of <10ms

## Overview

Four optimizations targeting the VideoToolbox encoding hot path. The first two are quick wins reducing per-frame overhead; the third restructures the encode pipeline for higher throughput; the fourth replaces the binary LAN/WAN detection with continuous network quality assessment for more precise adaptive control.

**Target**: 4K@30fps encode_ms < 10ms (currently 3.5-11ms depending on path)

### Current Latency Breakdown (4K@30fps, VT)

| Stage | Zero-copy Path | BGRA Path |
|-------|---------------|-----------|
| Color conversion | 0ms (skipped) | 1-3ms (hand-coded scalar loop) |
| GPU encode + callback wait | 3-7ms | 2-6ms |
| NAL data Vec clone | 0.5-2ms | 0.5-2ms |
| **Total** | **3.5-9ms** | **3.5-11ms** |

## 1. Arc-Shared NAL Data (Eliminate Vec Clone)

### Problem

After VT callback delivers encoded NAL data into `CallbackCtx::output`, the encoder thread clones the entire `Vec<u8>` (1-4MB for 4K frames) at videotoolbox.rs:986 (`guard.0.clone()`). This memcpy costs 0.5-2ms per frame.

### Design

Replace `Vec<u8>` clone with zero-copy ownership transfer:

**Current flow** (videotoolbox.rs):
```
CallbackCtx::output: Mutex<(Vec<u8>, bool)>

Callback thread:  extend annex_b Vec → lock output → *guard = (annex_b, is_kf)
Encoder thread:   lock output → clone Vec → return cloned data
```

**New flow**:
```
CallbackCtx::output: Mutex<Option<(Vec<u8>, bool)>>

Callback thread:  build annex_b Vec → lock output → *guard = Some((annex_b, is_kf))
Encoder thread:   lock output → guard.take() → return owned Vec (zero-copy)
```

Note: Using `Option::take()` instead of clone — transfers ownership of the Vec without memcpy. No `Arc` needed since the callback always relinquishes ownership. The final consumer is `Bytes::from(Vec<u8>)` which is also zero-copy.

**Changes required**:
- `CallbackCtx::output` type: `Mutex<(Vec<u8>, bool)>` → `Mutex<Option<(Vec<u8>, bool)>>`
- Callback (`encode_callback`): `*guard = Some((annex_b, is_keyframe))`
- State reset before encode: `*guard = None`
- Result retrieval in `encode_session_frame()`: `guard.take()` instead of `guard.0.clone()`
- All three callers of `encode_session_frame` (encode_bgra:1019, encode_pixel_buffer:1122, encode_bgra_444:1185/1199) need to handle the updated return type

**Implementation**: `crates/macrdp-encode/src/videotoolbox.rs` — CallbackCtx, encode_callback, encode_session_frame

**Expected benefit**: 0.5-1ms/frame saved

## 2. vImage SIMD Color Conversion for VT BGRA Path

### Problem

`VtEncoder::create_nv12_from_bgra_fast()` uses a hand-coded scalar loop for BGRA→NV12 conversion (~1-3ms at 4K). Meanwhile, `color_convert.rs` already has a vImage SIMD implementation, and VtEncoder already has a `vimage: Option<VImageConverter>` field (videotoolbox.rs:383) initialized in `new()` (line 412) but marked `#[allow(dead_code)]` — it's never actually used in the encode path.

### Design

Wire the existing `VImageConverter` into the BGRA encode path:

**VImageConverter API** (color_convert.rs:279-287):
```rust
pub fn bgra_to_nv12(
    &self,
    bgra: &[u8], width: u32, height: u32, stride: usize,
    y_out: &mut [u8],   // caller-provided output buffer
    uv_out: &mut [u8],  // caller-provided output buffer
) -> Result<(), String>
```

The function writes into caller-provided output buffers (does NOT return data).

**New fields** in VtEncoder (or reuse existing pre-allocated buffers):
```rust
nv12_y_buf: Vec<u8>,   // pre-allocated: width * height
nv12_uv_buf: Vec<u8>,  // pre-allocated: width * height / 2
```

**BGRA encode flow change**:
```
Current:
  1. Get pool CVPixelBuffer
  2. Lock → hand-coded BGRA→NV12 scalar loop → Unlock
  3. VTCompressionSessionEncodeFrame()

New:
  1. vimage.bgra_to_nv12(bgra, w, h, stride, &mut y_buf, &mut uv_buf)  [vImage SIMD ~0.3-0.8ms]
  2. Get pool CVPixelBuffer
  3. Lock → memcpy y_buf to Y plane, uv_buf to UV plane → Unlock  [~0.2-0.4ms]
  4. VTCompressionSessionEncodeFrame()

Total: 0.5-1.2ms vs current 1-3ms
```

**Fallback**: If `vimage` is `None` (init failed), fall through to existing `create_nv12_from_bgra_fast()` scalar loop.

**Implementation**:
- `crates/macrdp-encode/src/videotoolbox.rs` — modify `encode_bgra()` to use vimage path, add nv12 buffer fields
- Remove `#[allow(dead_code)]` from vimage field

**Expected benefit**: 0.5-2ms/frame saved (BGRA path only; zero-copy path unaffected)

## 3. Async Pipelined Encoding (Throughput Improvement)

### Problem

Current encoding is synchronous: CPU thread submits frame to GPU then blocks 3-7ms waiting for the callback. During this time, CPU is idle and cannot process the next frame's color conversion or handle input events.

### Clarification

This optimization does **not** reduce single-frame encode_ms (end-to-end latency from capture to send is unchanged). It **does** improve throughput by allowing CPU work (color conversion, input handling) to overlap with GPU encoding.

### Design

Split `encode_session_frame()` into two phases:

```rust
impl VtEncoder {
    /// Submit frame to GPU encoder. Returns immediately.
    fn submit_frame(&mut self, pixel_buffer: /* CVPixelBuffer or ptr */) -> Result<()> {
        // Clear callback state (*guard = None)
        // VTCompressionSessionEncodeFrame() — non-blocking enqueue
        // Return immediately (no condvar wait)
    }

    /// Collect result from previous submission. Blocks with timeout if not ready.
    fn collect_result(&mut self, timeout: Duration) -> Result<Option<EncodedFrame>> {
        // condvar.wait_timeout_while(timeout, !has_data)
        // Take Vec from callback output (Option::take, zero-copy)
        // Return EncodedFrame
    }

    /// Non-blocking check if previous result is ready.
    fn try_collect(&self) -> bool {
        self.callback_ctx.has_data.load(Ordering::Acquire)
    }
}
```

**VideoEncoder trait extension** (with default implementations for backward compatibility):

```rust
pub trait VideoEncoder: Send {
    // Existing methods unchanged...

    /// Whether this encoder supports async pipelining.
    fn supports_pipelining(&self) -> bool { false }

    /// Submit frame for async encoding. Default: no-op.
    fn submit_bgra(&mut self, data: &[u8], w: u32, h: u32, stride: usize) -> Result<()> {
        Ok(())
    }

    /// Collect result from previous submit. Default: returns None.
    fn collect_encoded(&mut self, timeout: Duration) -> Result<Option<EncodedFrame>> {
        Ok(None)
    }
}
```

**Display pipeline flow** (display.rs encode_and_send):

```
if encoder.supports_pipelining():
    1. collect_encoded(16ms) — get previous frame result (usually ready, wait ≈ 0ms)
    2. If result available → construct GFX frame for sending
    3. Color convert current frame (CPU, overlaps with nothing — GPU is now free)
    4. submit_bgra(current_frame) — enqueue to GPU, return immediately
    5. Return previous frame's DisplayUpdate
else:
    Current synchronous flow (OpenH264 falls here)
```

**Edge cases**:
- **First frame (cold start)**: `collect_encoded` returns `None` (no previous frame). Submit current frame, return `DefaultPointer` or skip. The pipeline "warms up" with one frame.
- **Last frame / shutdown**: `VtEncoder::drop()` already calls `VTCompressionSessionInvalidate()`. If a frame is pending, it gets discarded — acceptable during shutdown.
- **Force keyframe + pipeline**: `force_keyframe()` sets a flag consumed by the next `submit`. Since submit happens after collect, the flag correctly applies to the new frame.

**OpenH264 compatibility**: Default trait implementations return `supports_pipelining() = false`, so OpenH264 continues using synchronous `encode_bgra()`.

**Implementation**:
- `crates/macrdp-encode/src/videotoolbox.rs` — split encode_session_frame into submit + collect
- `crates/macrdp-encode/src/lib.rs` — extend VideoEncoder trait with default pipeline methods
- `crates/macrdp-core/src/display.rs` — pipelined encode_and_send flow

**Expected benefit**: No single-frame latency reduction, but CPU/GPU overlap enables higher sustainable frame rates and frees CPU for input/clipboard processing during encode.

## 4. Unified Network Quality Assessment

### Problem

Current binary `is_lan: bool` in BitrateController is too coarse. Real network quality is a spectrum (loopback, LAN, Wi-Fi, VPN, WAN, cellular). A single boolean can't capture this, leading to over-aggressive or insufficient adaptation.

### Design

#### 4.1 NetworkQuality (replaces is_lan)

```rust
pub struct NetworkQuality {
    rtt_ms: f64,          // EWMA RTT from GfxState
    is_private_ip: bool,  // IP range hint for initial estimate
}

impl NetworkQuality {
    /// Quality score 0.0-1.0 (0 = terrible, 1 = excellent)
    pub fn score(&self) -> f64 {
        match self.rtt_ms {
            r if r < 2.0  => 1.0,   // loopback
            r if r < 5.0  => 0.9,   // excellent LAN
            r if r < 10.0 => 0.8,   // good LAN
            r if r < 20.0 => 0.6,   // moderate LAN or good WAN
            r if r < 50.0 => 0.4,   // WAN
            r if r < 100.0 => 0.2,  // high latency
            _ => 0.1,               // poor
        }
    }

    /// Initial estimate from IP before RTT data available
    pub fn from_ip(is_private: bool) -> Self {
        Self {
            rtt_ms: if is_private { 5.0 } else { 50.0 },
            is_private_ip: is_private,
        }
    }

    /// Update with actual RTT measurement
    pub fn update_rtt(&mut self, rtt_ewma_ms: f64) {
        self.rtt_ms = rtt_ewma_ms;
    }
}
```

#### 4.2 Score-Based Decisions

| Score | Bitrate Control | FPS Control |
|-------|----------------|-------------|
| 0.8-1.0 | No ceiling, no adjustment | No adjustment |
| 0.5-0.8 | Gentle (10% steps) | Hold target |
| 0.2-0.5 | Active (15% steps, current behavior) | Can step down |
| 0.0-0.2 | Aggressive (20% steps) | Aggressive step down |

Bitrate ceiling scales: `ceiling = initial_bitrate * (1.0 + score * 0.5)` — high quality networks get up to 50% above initial bitrate.

#### 4.3 BitrateController Changes

Replace `is_lan: bool` with `network: NetworkQuality`. The `evaluate()` method uses `network.score()` to scale adjustment aggressiveness:

- score > 0.8: skip adjustment (like current LAN bypass)
- 0.5-0.8: bitrate adjusts in 10% steps, fps stays at target
- 0.2-0.5: full adjustment logic (current WAN behavior, 15% steps)
- < 0.2: more aggressive — 20% bitrate steps, fps drops faster

### VT Profile Note

VT Profile stays fixed at Constrained Baseline + CAVLC. Apple Silicon hardware encoder produces null frames (silent frame loss) when using High Profile with `RequireHardwareAcceleratedVideoEncoder` + `EnableLowLatencyRateControl` enabled (documented in videotoolbox.rs:499-501). Profile switching is explicitly a non-goal.

**Implementation**:
- `crates/macrdp-core/src/bitrate_controller.rs` — `NetworkQuality` struct replaces `is_lan`, `detect_network_phase1/phase2` replaced by `NetworkQuality::from_ip()` + `update_rtt()`, `evaluate()` uses score-based scaling
- `crates/macrdp-core/src/display.rs` — pass NetworkQuality to BitrateController

**Expected benefit**: More precise adaptation — less unnecessary quality loss on decent networks, faster degradation on poor networks.

## Architecture Summary

```
┌──────────┐    CaptureEvent    ┌───────────────────────┐
│  SCK /   │ ──────────────────►│  Display Pipeline     │
│  Capture │                    │                       │
└──────────┘                    │  ┌─ collect_result ◄─────── GPU (prev frame)
                                │  │                    │
                                │  ├─ vImage SIMD ◄──── BGRA frame
                                │  │  (0.3-0.8ms)      │
                                │  │                    │
                                │  ├─ submit_frame ────────► GPU (async)
                                │  │                    │
                                │  └─ take() NAL ─► GFX │
                                │     (zero-copy)       │
                                │                       │
                                │  NetworkQuality ──────┤
                                │  (RTT-based score)    │
                                │   → bitrate scaling   │
                                │   → fps tiers         │
                                └───────────────────────┘
```

## Files to Modify

| File | Changes |
|------|---------|
| `crates/macrdp-encode/src/videotoolbox.rs` | Option<Vec> in CallbackCtx (take instead of clone), vImage integration in encode_bgra, split submit/collect |
| `crates/macrdp-encode/src/lib.rs` | VideoEncoder trait: add pipeline methods with defaults |
| `crates/macrdp-core/src/bitrate_controller.rs` | NetworkQuality replaces is_lan + detect_network_phase1/phase2, score-based evaluation scaling |
| `crates/macrdp-core/src/display.rs` | Pipelined encode_and_send, NetworkQuality integration |

## Expected Results

| Path | Before | After |
|------|--------|-------|
| Zero-copy (NV12→VT) | 3.5-9ms | 3-7.5ms (take saves 0.5-1ms) |
| BGRA→VT | 3.5-11ms | 2-8ms (take + vImage save 1-3ms) |
| Throughput (60fps) | CPU idle during GPU wait | CPU/GPU parallel |
| Network adaptation | Binary LAN/WAN | Continuous 0-1 score |

## Testing Strategy

### Unit Tests
1. **NetworkQuality score tests**: Known RTT values → expected scores
2. **BitrateController with NetworkQuality**: Score-based scaling verification (high score = less aggressive, low score = more aggressive)
3. **vImage vs scalar NV12 correctness**: Same BGRA input → compare Y/UV outputs within ±2 tolerance (catch BT.601 coefficient mismatch)

### Benchmark Tests
4. **Encode latency A/B**: `cargo bench` before/after Option::take + vImage changes
5. **Pipeline throughput**: Measure frames/second with sync vs async pipeline

### Integration Tests
6. **Loopback connection**: Verify score ~1.0, no bitrate adjustment
7. **Pipeline cold start**: First frame returns None from collect, second frame returns valid data

## Non-Goals

- No VT Profile switching (High Profile causes frame loss on Apple Silicon + low-latency mode)
- No B-frame support (AllowFrameReordering stays false)
- No dirty rect partial encoding (VT session size is fixed)
- No OpenH264 encoder speed improvements (VT is primary)
- No codec changes (H.264 stays)

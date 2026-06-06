# Upstream Port: main → experimental

Status: analysis as of 2026-06-05. Cherry-picking won't work — the branches
have diverged structurally (main refactored macrdp-server to add display.rs,
handler.rs, tls.rs, removed keychain.rs/lib.rs; main added
bitrate_controller.rs, log_bridge.rs, perf_stats.rs to macrdp-core). These
features need manual porting.

Experimental already has: lenient GFX caps parser, CredSSP public key binding
fix, rude-client tests, Keychain integration, vImage I420, scalar NV12, various
refactored helpers.


## High value — Video pipeline performance

**GFX uncompressed dirty-rect path** (6be70bb) — DONE
For small changes (≤65536px), sends raw BGRA via Codec1Type::Uncompressed —
zero encode latency for cursor blinks, text entry, etc. Adds
GfxUncompressedUpdate type and create_uncompressed_pdu() in
ironrdp-server-gfx/src/gfx.rs, plus a new DisplayUpdate variant in
ironrdp-server-gfx/src/display.rs. Relatively self-contained and additive.

**Async pipelined VT encoding** (801e053, e879e9f) — DONE (API only; display wiring deferred)
Splits encode_session_frame into submit_session_frame + collect_session_frame.
The display pipeline can do other work while the hardware encoder runs.
Contained within macrdp-encode/src/videotoolbox.rs. The wiring commit (e879e9f)
integrates it into the display pipeline in macrdp-core.

**vImage SIMD BGRA→NV12** (ac9fe2b) — DONE
create_nv12_vimage() uses VImageConverter for NV12 conversion, falling back to
the scalar path. Experimental already has vImage for I420 but not the NV12 path
used by VT. Contained in macrdp-encode/src/videotoolbox.rs and color_convert.rs.

**NAL zero-copy** (7906dcc) — DONE
Option::take instead of Vec::clone in the encode callback — eliminates an
allocation per frame. Small change in macrdp-encode/src/videotoolbox.rs.


## High value — Adaptive quality

**BitrateController** (6f8cccc, c267284)
Adaptive bitrate with dynamic FPS tiers and LAN bypass. Full feedback loop from
GFX ack timing. New file macrdp-core/src/bitrate_controller.rs plus integration
into macrdp-core/src/server.rs. This is the most invasive change — touches the
core server loop and depends on several of the items below.

**Static scene detection** (c2a915d)
Idle tracking with IDR keepalive — stops encoding when the screen is static.
Integrated into macrdp-core alongside BitrateController.

**CaptureEvent / Idle propagation** (476085e, 55c1e57)
CaptureEvent enum propagates Idle events from capture to the display pipeline.
New enum in macrdp-capture, handling in macrdp-server display adapter. Prereq
for static scene detection.

**Runtime bitrate update for OpenH264** (57d2a26) — DONE
Uses SetOption API to change bitrate at runtime instead of requiring session
recreation. In macrdp-encode/src/openh264_enc.rs. Prereq for BitrateController.

**Runtime FPS control** (e882751) — DONE
SCStream::update_configuration changes capture framerate dynamically. In
macrdp-capture/src/lib.rs. Used by BitrateController to drop FPS on congestion.

**LAN detection** (7d0e7be)
IP + RTT two-phase approach to detect LAN vs WAN. Used by BitrateController
for quality bypass on fast networks. Needs peer_addr exposed via GfxState
(36287f9).

**NetworkQuality scoring** (376a066)
Replaces binary LAN/WAN with continuous 0.0–1.0 quality score derived from RTT,
ack trends, and encode time. Refactors how the server interprets network
conditions.


## Medium value — Observability and config

**--perf CLI flag** (f8731fb)
Frame-level performance statistics: encode time, frame size, pipeline latency.
New perf_stats.rs in macrdp-core (and duplicated in macrdp-server on main), plus
CLI arg in macrdp-server/src/main.rs and config.rs.

**Log bridge** (6be70bb)
LogBridgeLayer writes JSONL to a temp file for external log consumers. New file
macrdp-core/src/log_bridge.rs. Mainly useful if a UI or external tool needs
structured logs.

**Hot config** (6be70bb)
ServerHandle::update_config() pushes bitrate/cursor/resolution/encoder/credential
changes to a running server without restart. Wired through
macrdp-core/src/server.rs. Mainly useful alongside the desktop UI.

**Resolution system overhaul** (6be70bb)
Replaces hidpi_scale with flexible resolution config ("auto" / "WxH" / legacy
int). Auto-detects Retina scale via detect_display_scale(). Separates SCK
(capture) and CG (mouse mapping) display size detection. Adds MouseCoordMapper
for proportional RDP→macOS coordinate mapping. Experimental already has some
of this (show_cursor, dynamic resolution clamping, mouse scale sync).


## Separate feature crates

**macrdp-clipboard** (6b25950 → 49b013e, ~10 commits)
Full clipboard crate: PasteboardBridge with text (UTF-8 ↔ UTF-16LE), DIB ↔ PNG
image conversion, HTML format wrap/unwrap, FileGroupDescriptorW file transfer.
Integrated into macrdp-core and macrdp-server via clipboard factory. Depends on
ironrdp-cliprdr. This is a self-contained crate but the integration touches the
core server.

**macrdp-audio** (ed7aada → 5e80d32, 4 commits)
macrdp-audio crate with AudioConverter, SCK audio capture with AudioFrame
channel, MacAudioHandler implementing RdpsndServerHandler, audio processing loop
integrated into macrdp-core. Optional Opus encoding (feature-gated). Also
self-contained but integration touches macrdp-core/src/server.rs.


## Build / CI

**DYLD_LIBRARY_PATH auto-set** (4c254c2)
Fixes Swift runtime linking for standalone binaries and adds CoreFoundation
framework link. Build script and macrdp-encode changes.

**Benchmarks** (98ece50)
vImage vs scalar, take vs clone, and VT encoder benchmarks in
macrdp-encode/benches/encode_bench.rs.


## Suggested porting order

1. NAL zero-copy (small, self-contained perf win)
2. GFX uncompressed dirty-rect path (high impact, mostly additive)
3. vImage NV12 path (perf, contained in encode crate)
4. Async pipelined VT encoding (perf, contained but needs display pipeline wiring)
5. CaptureEvent/Idle + runtime FPS + runtime bitrate update (prereqs for adaptive)
6. BitrateController + LAN detection + NetworkQuality (the full adaptive system)
7. --perf flag (observability)
8. Clipboard and audio crates (if in scope)

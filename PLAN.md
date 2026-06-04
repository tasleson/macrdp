# macrdp CLI Daemon Completion Plan

## Product Goal

macrdp should provide a stable, high-performance RDP server for macOS with a CLI interface that can be integrated as a per-user daemon. Clients are standard RDP clients such as Windows Remote Desktop (`mstsc`), Microsoft Remote Desktop for macOS/iOS, FreeRDP, and Remmina.

The primary deliverable is a reliable CLI daemon, not a desktop management app. macOS Screen Recording and Accessibility permissions are tied to the logged-in GUI session, so the daemon target should be a user `launchd` LaunchAgent rather than a root LaunchDaemon.

## Current State

macrdp is already a substantial prototype. It includes ScreenCaptureKit capture, CoreGraphics fallback capture, keyboard and mouse injection, CredSSP/NLA, TLS certificate generation, an RDPGFX H.264 path, VideoToolbox NV12 zero-copy encoding, OpenH264 fallback, AVC444 work, a Tauri UI, tray integration, metrics, and persistent UI config.

The remaining work should be judged against the CLI daemon goal. Anything that improves daemon stability, protocol compatibility, performance, security, observability, or macOS service integration is in scope. UI/Tauri work is optional and should not block the CLI daemon.

## Highest-Risk Findings

1. DONE: Security: failed authentication logs password bytes in `crates/ironrdp-acceptor-patched/src/connection.rs`.
2. DONE: Security: non-hybrid auth currently accepts an empty client password before checking the username/password pair. That behavior needs a narrower mstsc pre-prompt solution that cannot become a credential bypass.
3. DONE: Runtime drift: the CLI server and `macrdp-core` server paths have diverged. The CLI has newer GFX fallback diagnostics and behavior that `macrdp-core` does not have.
4. DONE: Single-client server: v1 is explicitly single-client; the misleading `max_connections` UI/config field was removed instead of advertising unsupported concurrent sessions.
5. DONE: Daemon architecture: make `macrdp-core` the single runtime implementation and thin `crates/macrdp-server` into a CLI wrapper around it.
6. DONE: Daemon binding/config drift: CLI config needs explicit `bind_address`, TLS certificate/key paths, stable config/log paths, and no UI-only settings.
7. DONE: Daemon credentials: replace PID/time generated fallback passwords with OS CSPRNG, and make noninteractive credential behavior suitable for launchd.
8. DONE: Adaptive bitrate is computed but not applied to encoders.
9. DONE: Idle-frame settings exist in CLI config but are not wired into capture/display behavior.
10. DONE: Shutdown and binding reliability: avoid the pre-bind/drop port-selection race and make signal-triggered shutdown graceful.

## Phase 1: CLI Runtime Authority

Goal: one authoritative daemon runtime with no duplicate server behavior.

- DONE: Add `crates/macrdp-core` to the root workspace so `cargo test` covers it by default.
- DONE: Move CLI runtime construction into `macrdp-core` or reuse `macrdp-core::start_server` from `crates/macrdp-server`.
- DONE: Keep `crates/macrdp-server` as a thin binary for argument parsing, logging setup, config loading, and process lifecycle.
- DONE: Remove duplicated display, handler, TLS, and runtime code from the CLI crate after the wrapper is in place.
- DONE: Preserve the CLI's current GFX fallback diagnostics and behavior in the single core path.
- DONE: Add integration tests for config conversion, server startup failure modes, and graceful shutdown.

## Phase 2: Daemon Configuration and Operations

Goal: the CLI can run unattended as a launchd-managed user service.

- DONE: Add explicit `bind_address` to CLI/core config and bind to that address instead of hardcoded `0.0.0.0`.
- DONE: Keep v1 single-client and document that concurrent sessions are unsupported.
- DONE: Wire TLS certificate and key path fields through the CLI daemon path.
- DONE: Normalize config, TLS, and log paths under macOS Application Support or a documented XDG-compatible location.
- DONE: Provide CLI flags for config path, bind address, port, log level, credentials source, TLS paths, frame rate, encoder, bitrate, chroma mode, and HiDPI scale.
- DONE: Use structured, daemon-friendly logs to stderr and/or configured log file without printing secrets.
- DONE: Implement signal handling for `SIGINT` and `SIGTERM` so launchd stop/restart is graceful.
- DONE: Avoid pre-bind/drop port selection races; either bind exactly what was configured or fail clearly.
- DONE: Provide a sample `launchd` LaunchAgent plist and install/uninstall documentation.
- DONE: Add permission diagnostics for Screen Recording and Accessibility that work in a noninteractive CLI context.

## Phase 3: Security

Goal: safe defaults for a network daemon.

- DONE: Require configured credentials for unattended daemon mode, or generate fallback credentials using OS CSPRNG only when explicitly allowed.
- DONE: Never log passwords, password bytes, generated secrets, or credential comparison details.
- DONE: Keep NLA/CredSSP as the default path for standard RDP clients.
- DONE: Keep TLS certificate generation for development, but support durable configured cert/key paths for daemon deployments.
- DONE: Add focused credential-matching and generated-credential tests.
- DONE: Review whether TLS private key permissions are restricted when generated.

## Phase 4: Protocol Compatibility

Goal: reliable standard-client behavior with graceful fallbacks.

- DONE: Verify RDPGFX negotiation for AVC420 with representative `mstsc`, Microsoft Remote Desktop for macOS/iOS, FreeRDP, and Remmina capability profiles. The patched `ironrdp-server-gfx` crate is now a workspace member with enabled unit tests, and `gfx::tests::avc420_negotiates_for_representative_client_capabilities` drives the real `GfxHandler::process` path to assert that AVC420 is negotiated and a `CapabilitiesConfirm` is emitted, including forward-rolled/unknown capability versions before known AVC-capable sets.
- DONE: Tolerate unknown RDPGFX capability versions from forward-rolled clients (FreeRDP 3.x advertises versions newer than vendored `ironrdp-pdu` 0.7.0 knows; the upstream decoder rejected the whole `CapabilitiesAdvertise` PDU on the first unknown set, leaving `caps_confirmed` false and the client on a white screen). Lenient parser in `crates/ironrdp-server-gfx/src/gfx.rs` walks the wire format manually and preserves unknown sets as `CapabilitySet::Unknown` so the V10_x sets the same client advertised still light up `avc420_supported`.
- DONE: Treat AVC420/H.264 as the v1 compatibility priority.
- DONE: Keep AVC444 as optional quality work, not a release blocker.
- DONE: If the client lacks AVC support, fall back to bitmap only when BGRA capture is available. The bitmap path now uses an explicit `bitmap_fallback_decision` gate, with tests covering AVC-disabled clients and no-capability timeouts for both BGRA and NV12/PixelBuffer frame sources.
- DONE: Avoid indefinite white-screen behavior when GFX opens but cannot become usable.
- DONE: Handle the "GFX channel opened but client never sends `CapabilitiesAdvertise`" case (observed with GNOME Connections / gnome-remote-desktop / gtk-frdp). Existing `hopeless` heuristic only catches `caps_confirmed=true && !avc420_supported`; this case has `caps_confirmed=false` indefinitely. Treat "channel open for N frames without caps" as hopeless and fall back to bitmap (requires BGRA capture; NV12 zero-copy can't bitmap-encode).
- DONE: Honor the client's `desktop_width`/`desktop_height` from `ConnectInitial` as the initial server size. Currently the server always picks `detect_display_size()` and ignores the client's request, so `/size:WxH` on FreeRDP-family clients silently doesn't matter; the client's hint only arrives later in `ClientConfirmActive` and goes through a `request_resize` path that doesn't actually rebuild the pipeline.
- DONE: Test dynamic resolution and reconnect flows. Today `MacDisplay::request_layout` is the default no-op and `request_resize` updates `self.width`/`self.height` but never propagates a `DisplayUpdate::Resize` back to the display pump, so neither DisplayControl monitor-layout PDUs (client window resize) nor the `ClientConfirmActive` size hint actually rebuild the SCK capturer / VT encoder. Wire both paths through a resize signal that triggers deactivation/reactivation.
- DONE: Add clipboard support if the existing IronRDP plumbing is solid enough for daemon use.
- DONE: Defer audio output, printer redirection, file redirection, smartcard, and broad multi-monitor support unless explicitly required. `macrdp-core::features` now records the v1 supported/deferred feature policy and logs it at startup; tests pin that clipboard remains the only supported redirection feature and that multi-monitor DisplayControl layouts are ignored by the single-monitor daemon path.

## Phase 5: Performance

Goal: excellent interactive performance with predictable latency and low idle cost.

- DONE: Prefer VideoToolbox hardware encoding on macOS when available, with OpenH264 fallback. `EncoderPreference::Auto` now tries VideoToolbox first on macOS and falls back to OpenH264, while explicit `software` skips VideoToolbox; tests pin the backend order and encoder preference aliases.
- DONE: Keep NV12 zero-copy as the primary AVC420 path. AVC420 capture now selects NV12 whenever the encoder preference prepares for platform hardware encode (`auto` or `hardware` on macOS), while AVC444 and explicit software stay on BGRA; tests pin those capture-format decisions.
- DONE: Use BGRA only for AVC444, software encoding, or bitmap fallback.
- DONE: VideoToolbox silently drops every frame (`status=0`, `kVTEncodeInfo_FrameDropped`) when the SCK capture size differs from the macOS native display logical size — i.e. whenever the daemon is configured for any width/height other than what `detect_display_size()` returns, hardware encoding goes dark with no visible error. Confirmed in this session: at the macOS native 2560×1440 VT runs cleanly at ~11ms/frame; at any other size every frame is silently dropped. Either force SCK to always emit native-size buffers and resize/clip downstream, or reinitialize VT to whatever size SCK actually delivers, or surface the constraint as a startup config error.
- DONE: Distinguish "VT silently dropped this frame (`kVTEncodeInfo_FrameDropped`)" from a real callback error in `videotoolbox.rs:encode_callback`. Previously every drop logged as a generic "encode callback error" with no signal about whether VT was reporting the dropped-flag or something else, which made the silent-drop bug above hard to diagnose.
- DONE: Apply adaptive bitrate using ACK/RTT state and `VideoEncoder::set_bitrate()` with hysteresis.
- DONE: Distinguish client-side decode/render latency from real network congestion in the adaptive bitrate controller. Today "RTT" is the GFX FrameAcknowledge round-trip, which includes client decode + render + ack-cadence — so a slow xfreerdp on a 4K HiDPI desktop produces 200–2000ms "RTT" on a sub-1ms gigabit LAN, and the controller responds by scaling bitrate down even though the link is idle. Need a separate signal for "ack queue full because client is slow" vs "packets being lost or queued in the network."
- DONE: Add backpressure: reduce FPS or skip frames when `pending_acks` grows.
- DONE: Add frame pacing based on measured encode/network latency.
- DONE: Implement `skip_unchanged` and `idle_keyframe_sec`.
- DONE: Avoid encoding unchanged frames; send idle keyframes/keepalives only as needed.
- DONE: Reduce avoidable allocations in bitmap dirty-region extraction and GFX PDU assembly.
- Clean up unused/dead encode code after benchmarks identify the winning paths.

## Phase 6: Verification and Release

Goal: reproducible daemon builds and compatibility confidence.

- Add CI for root Rust tests, formatting, clippy where practical, and release builds.
- Add release profiles with real optimization for the CLI daemon.
- Benchmark capture copy, BGRA to NV12/I420 conversion, VideoToolbox encode, OpenH264 encode, GFX PDU assembly, and end-to-end frame latency.
- Add automated tests for credentials, config validation, keymap, color conversion, AVC444 split, bitmap dirty rects, adaptive bitrate decisions, and graceful shutdown.
- Manually validate Windows Remote Desktop (`mstsc`), Microsoft Remote Desktop for macOS, Microsoft Remote Desktop for iOS/iPadOS, FreeRDP, and Remmina.

## Non-Goals for the CLI Daemon

- The Tauri UI, tray, React frontend, dashboard, settings pages, popover, theme settings, UI database/history, and `npm` build path are not required for the CLI daemon.
- UI-only config fields such as `autostart`, `theme`, frontend hot-update controls, and connection history should not drive daemon scope.
- Multiple concurrent clients are not required for v1.
- AVC444 is not required for v1 if AVC420 is stable and performant.
- Audio, printer redirection, file redirection, smartcard, and broad multi-monitor support are deferred unless a concrete deployment requires them.
- Code signing, notarization, and `.app` packaging are not required for a CLI daemon release, though binary signing may be considered later.

## Optional Desktop App Track

The existing Tauri UI can remain as a separate optional product track. If revived, it should consume the same `macrdp-core` daemon runtime and must not introduce runtime drift. Its build/test issues, including Swift runtime rpath for Tauri tests and frontend `npm` availability, should be tracked separately from CLI daemon readiness.

## Performance Targets

- 1080p60 hardware encode with low interactive latency.
- 4K60 hardware encode on Apple Silicon where network allows.
- 4K30 software fallback as a minimum usable target.
- Stable reconnect without white screen or decoder errors.
- Idle CPU/GPU usage close to zero when the screen is unchanged.
- Clean fallback when the screen is locked and ScreenCaptureKit stops.

## Validation Snapshot

- `cargo test` from the repository root previously passed: 26 tests passed across capture, encode, input, and CLI server crates.
- `cargo test --manifest-path macrdp-ui/src-tauri/Cargo.toml --no-run` compiled after recent UI config edits.
- `npm run build` could not run because `npm` was not installed in the current environment; this is not blocking for the CLI daemon track.

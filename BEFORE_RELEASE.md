# Before Release Checklist

🤖 Assisted-by: Claude Sonnet 4.6

## Confirmed bugs (all fixed)

### 1. ~~TLS-only clients immediately rejected~~ — HIGH — **fixed in 8a58fea**

**File:** `crates/ironrdp-acceptor-patched/src/connection.rs:630`

`check_credentials()` returns `NeedsClientPrompt` when both username and password are
empty, but the call site only passes on `Accepted`. Since `NeedsClientPrompt != Accepted`
it falls into the same `ServerDeniedConnection` arm as a real rejection.

This breaks any TLS-only (non-NLA / SSL-mode) client that probes with empty credentials
as its standard first-connect flow — mstsc does this to trigger the Windows login dialog.
Those clients will see a hard connection error rather than a credential prompt. The
three-way `CredentialCheck` enum was clearly intended to be handled distinctly at the
call site, but the branch was never wired up.

### 2. ~~HiDPI width/height can wrap to zero~~ — MEDIUM — **fixed in 5ada438**

**File:** `crates/macrdp-core/src/server.rs:349`

```rust
let (width, height) = (width * hidpi_scale as u16, height * hidpi_scale as u16);
```

Plain `u16 * u16` with no overflow guard. At scale 4, a display wider than ~16384px (or
any combination where the product exceeds 65535) wraps to 0 in release mode, producing a
zero-dimension display that propagates into the GFX state and bitrate calculations.
Should use `saturating_mul` or `checked_mul` with a reasonable cap.

### 3. ~~Clipboard write silently succeeds with empty content~~ — LOW — **fixed in e4880bd**

**File:** `crates/macrdp-core/src/clipboard.rs:269`

`if let Some(stdin)` no-ops silently if the pipe handle is absent. `pbcopy` receives EOF,
overwrites the local clipboard with an empty string, exits 0, and `write_pasteboard_text`
returns `Ok(())`. The caller has no indication the write failed. Should be
`.take().ok_or_else(...)` to surface the failure properly.

## Cleanup (all resolved in fdceb1d)

- ~~**Post-encode bookkeeping triplicated** (`display.rs:934`)~~ — extracted into `MacDisplayUpdates::record_gfx_frame_sent()`
- ~~**NAL diagnostic loop duplicated** (`videotoolbox.rs`)~~ — extracted into `log_nal_diagnostic()`; also fixed silent divergence where zero-copy path omitted SPS profile
- ~~**Force-keyframe `CFDictionary` construction duplicated 3×** (`videotoolbox.rs`)~~ — extracted into `VtEncoder::take_force_keyframe_props()`

## Testing needed before release

Finding #1 is exactly the kind of bug that only surfaces with real client testing. If
testing has been NLA-only (CredSSP), that path never hits `check_credentials` at all.
Cover at minimum:

- **mstsc** with "Network Level Authentication" unchecked (exercises the `NeedsClientPrompt` path)
- **FreeRDP 3.x** (the GFX/AVC compatibility work targets this client)
- A client that explicitly disconnects and reconnects to stress the deactivation/reactivation path

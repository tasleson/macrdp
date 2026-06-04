//! macOS permission checking, requesting, and diagnostics.
//!
//! The diagnostic helpers are safe to run in a noninteractive CLI context
//! (e.g. under launchd): they never block, never prompt, and never open
//! System Settings. The `request_permissions()` startup helper also avoids
//! launching the Settings UI unless stdout is a real TTY — important so a
//! launchd-managed daemon doesn't pop the Settings app on every restart.

use std::io::IsTerminal;

use serde::Serialize;

use crate::callbacks::PermissionStatus;

/// Where the user grants a missing permission. Surfaced in diagnostics so
/// noninteractive callers can print actionable next steps.
const SCREEN_RECORDING_PATH: &str = "System Settings > Privacy & Security > Screen Recording";
const ACCESSIBILITY_PATH: &str = "System Settings > Privacy & Security > Accessibility";

/// Output format for [`format_report`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    Text,
    Json,
}

/// One row of the diagnostic report. Serialized to JSON when the caller
/// chose `ReportFormat::Json`.
#[derive(Debug, Clone, Serialize)]
pub struct PermissionReport {
    pub all_granted: bool,
    pub screen_recording: PermissionEntry,
    pub accessibility: PermissionEntry,
}

#[derive(Debug, Clone, Serialize)]
pub struct PermissionEntry {
    pub granted: bool,
    /// Where in System Settings the user grants this permission.
    pub grant_path: &'static str,
}

/// Check current macOS permission status. Non-blocking, no prompts, no UI.
/// Safe to call from a noninteractive daemon context.
pub fn check_permissions() -> PermissionStatus {
    PermissionStatus {
        screen_capture: macrdp_capture::check_screen_recording_permission(),
        accessibility: macrdp_input::check_accessibility_permission(),
        microphone: false, // Phase 3
    }
}

/// Build a structured diagnostic report from the current permission status.
pub fn permission_report() -> PermissionReport {
    let status = check_permissions();
    PermissionReport {
        all_granted: status.screen_capture && status.accessibility,
        screen_recording: PermissionEntry {
            granted: status.screen_capture,
            grant_path: SCREEN_RECORDING_PATH,
        },
        accessibility: PermissionEntry {
            granted: status.accessibility,
            grant_path: ACCESSIBILITY_PATH,
        },
    }
}

/// Render a permission report as either human-readable text or one-line JSON.
/// Used by the CLI's `--check-permissions` flag so scripts and humans can both
/// consume the output.
pub fn format_report(report: &PermissionReport, format: ReportFormat) -> String {
    match format {
        ReportFormat::Json => {
            serde_json::to_string(report).expect("PermissionReport always serializes")
        }
        ReportFormat::Text => {
            let row = |name: &str, entry: &PermissionEntry| -> String {
                if entry.granted {
                    format!("{name:<18} granted")
                } else {
                    format!("{name:<18} NOT granted — grant at: {}", entry.grant_path)
                }
            };
            format!(
                "{}\n{}\n",
                row("Screen Recording:", &report.screen_recording),
                row("Accessibility:", &report.accessibility),
            )
        }
    }
}

/// Request all required macOS permissions and return the resulting status.
///
/// Triggers the TCC prompt for any missing permission (safe to call from
/// launchd — macOS surfaces the prompt in the user's GUI session). Only
/// opens System Settings if stdout is a real TTY, so a launchd-managed
/// daemon does not pop the Settings app on every restart.
pub fn request_permissions() -> PermissionStatus {
    request_permissions_with_ui(std::io::stdout().is_terminal())
}

/// Like [`request_permissions`], but the caller chooses whether opening
/// System Settings is allowed. Tests and noninteractive callers pass `false`.
pub fn request_permissions_with_ui(may_open_settings: bool) -> PermissionStatus {
    tracing::info!("Checking macOS permissions...");

    if macrdp_capture::check_screen_recording_permission() {
        tracing::info!("Screen Recording permission granted");
    } else {
        tracing::warn!("Screen Recording permission NOT granted");
        macrdp_capture::request_screen_recording_permission();
        if !macrdp_capture::check_screen_recording_permission() {
            tracing::error!(
                grant_path = SCREEN_RECORDING_PATH,
                "Screen Recording denied; grant the permission and restart the daemon"
            );
            if may_open_settings {
                macrdp_capture::open_screen_recording_settings();
            }
        }
    }

    if macrdp_input::check_accessibility_permission() {
        tracing::info!("Accessibility permission granted");
    } else {
        tracing::warn!("Accessibility permission NOT granted");
        macrdp_input::request_accessibility_permission();
        if !macrdp_input::check_accessibility_permission() {
            tracing::error!(
                grant_path = ACCESSIBILITY_PATH,
                "Accessibility denied; grant the permission and restart the daemon"
            );
            if may_open_settings {
                macrdp_input::open_accessibility_settings();
            }
        }
    }

    check_permissions()
}

/// Detect the main display's logical pixel size
pub fn detect_display_size() -> anyhow::Result<(u32, u32)> {
    macrdp_capture::detect_display_size()
}

/// Verify that ScreenCaptureKit can actually enumerate at least one display
/// for this process. Returns `Ok(display_count)` on success.
///
/// The CoreGraphics `CGPreflightScreenCaptureAccess` check used by
/// [`request_permissions`] checks a different TCC scope than the SCK API the
/// capture pipeline actually uses, and the two regularly disagree for
/// unsigned CLI binaries: the CG path returns "granted" while
/// `SCShareableContent.displays()` returns an empty list. When that happens
/// the daemon would accept a client connection, fail the first frame with
/// "No display found", and leave the client staring at a blank canvas.
/// Calling this at startup turns that silent class of failure into an
/// actionable error before any client can connect.
pub fn verify_sck_capture_ready() -> anyhow::Result<usize> {
    use anyhow::bail;
    match macrdp_capture::check_screen_recording_via_sck() {
        macrdp_capture::SckPreflight::Ok { display_count } => Ok(display_count),
        macrdp_capture::SckPreflight::NoDisplays => {
            // Distinguish a permission gap (real problem) from a transient
            // display-sleep state (safe to continue — capture will wake the
            // display when the first client connects).
            if macrdp_capture::is_display_asleep() {
                tracing::warn!(
                    "Display is asleep; ScreenCaptureKit sees zero displays at startup. \
                     The server will start — capture will wake the display when a \
                     client connects."
                );
                return Ok(0);
            }
            bail!(
                "ScreenCaptureKit reports zero displays for this process. \
                 macOS may have granted the legacy CoreGraphics screen-capture \
                 scope but not the ScreenCaptureKit scope to this specific \
                 binary. Open System Settings → Privacy & Security → \
                 Screen Recording, remove any prior entry for `macrdp-server`, \
                 and re-launch so macOS prompts again. Re-granting may be \
                 required after each `cargo build` for unsigned binaries.",
            )
        }
        macrdp_capture::SckPreflight::Error(e) => bail!(
            "SCShareableContent.get() failed: {e}. \
             Screen Recording permission is likely not granted for this \
             binary; grant it under System Settings → Privacy & Security \
             → Screen Recording and re-launch.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report(screen: bool, access: bool) -> PermissionReport {
        PermissionReport {
            all_granted: screen && access,
            screen_recording: PermissionEntry {
                granted: screen,
                grant_path: SCREEN_RECORDING_PATH,
            },
            accessibility: PermissionEntry {
                granted: access,
                grant_path: ACCESSIBILITY_PATH,
            },
        }
    }

    #[test]
    fn text_report_lists_grant_paths_for_missing_permissions() {
        let out = format_report(&sample_report(true, false), ReportFormat::Text);
        assert!(out.contains("Screen Recording:"));
        assert!(out.contains("granted"));
        assert!(out.contains("Accessibility:"));
        assert!(out.contains("NOT granted"));
        assert!(out.contains(ACCESSIBILITY_PATH));
    }

    #[test]
    fn json_report_is_one_line_and_machine_parseable() {
        let out = format_report(&sample_report(false, true), ReportFormat::Json);
        assert!(!out.contains('\n'));
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["all_granted"], false);
        assert_eq!(parsed["screen_recording"]["granted"], false);
        assert_eq!(parsed["accessibility"]["granted"], true);
        assert_eq!(
            parsed["screen_recording"]["grant_path"],
            SCREEN_RECORDING_PATH
        );
    }

    #[test]
    fn all_granted_only_true_when_both_granted() {
        assert!(!sample_report(false, false).all_granted);
        assert!(!sample_report(true, false).all_granted);
        assert!(!sample_report(false, true).all_granted);
        assert!(sample_report(true, true).all_granted);
    }
}

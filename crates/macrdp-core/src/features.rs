//! Runtime feature policy for the v1 CLI daemon.

/// RDP features that the macrdp daemon intentionally supports in v1.
pub const SUPPORTED_DAEMON_FEATURES: &[&str] = &[
    "single-client session",
    "single-monitor desktop",
    "keyboard input",
    "mouse input",
    "RDPGFX AVC420/AVC444 video",
    "bitmap fallback",
    "clipboard text redirection",
    "audio output (RDPSND)",
];

/// RDP features intentionally deferred from the v1 daemon scope.
pub const DEFERRED_DAEMON_FEATURES: &[&str] = &[
    "printer redirection",
    "file/drive redirection",
    "smartcard redirection",
    "broad multi-monitor desktop",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_policy_defers_non_daemon_redirection_features() {
        for feature in [
            "printer redirection",
            "file/drive redirection",
            "smartcard redirection",
            "broad multi-monitor desktop",
        ] {
            assert!(
                DEFERRED_DAEMON_FEATURES.contains(&feature),
                "{feature} should remain explicitly deferred for v1"
            );
        }
    }

    #[test]
    fn v1_policy_keeps_clipboard_as_supported_redirection() {
        assert!(SUPPORTED_DAEMON_FEATURES.contains(&"clipboard text redirection"));
        assert!(!DEFERRED_DAEMON_FEATURES.contains(&"clipboard text redirection"));
    }
}

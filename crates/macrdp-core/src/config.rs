//! Server configuration

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Server configuration (loaded from TOML, shared between CLI and UI).
///
/// `Debug` is implemented manually so the password never appears in log output.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub bind_address: String,
    pub port: u16,
    pub frame_rate: u32,
    /// Resolution width (0 = auto-detect from display)
    pub width: u32,
    /// Resolution height (0 = auto-detect from display)
    pub height: u32,
    pub username: Option<String>,
    pub password: Option<String>,
    pub allow_generated_credentials: bool,
    pub cert_path: Option<PathBuf>,
    pub key_path: Option<PathBuf>,
    pub log_path: Option<PathBuf>,
    pub idle_timeout_secs: u64,
    /// Log level: trace, debug, info, warn, error
    pub log_level: Option<String>,
    /// Log format: "text" (human-readable, default) or "json" (one JSON object per line)
    pub log_format: Option<String>,
    /// Video quality: low_latency, balanced, high_quality (default: high_quality)
    pub quality: Option<String>,
    /// H.264 encoder: software, hardware, auto (default: auto)
    pub encoder: Option<String>,
    /// Chroma subsampling mode: "avc420" or "avc444" (default: "avc420")
    pub chroma_mode: Option<String>,
    /// HiDPI scale factor (default: 1)
    pub hidpi_scale: Option<u32>,
    /// Include the macOS cursor in captured frames. Usually false for RDP
    /// because clients draw a local cursor.
    pub show_cursor: Option<bool>,
    /// Target bitrate in Mbps (default: auto-calculated)
    pub bitrate_mbps: Option<u32>,
    /// Skip encoding unchanged frames when capture can detect them.
    pub skip_unchanged: Option<bool>,
    /// Seconds between idle keyframes/keepalives.
    pub idle_keyframe_sec: Option<u32>,
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("bind_address", &self.bind_address)
            .field("port", &self.port)
            .field("frame_rate", &self.frame_rate)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field(
                "allow_generated_credentials",
                &self.allow_generated_credentials,
            )
            .field("cert_path", &self.cert_path)
            .field("key_path", &self.key_path)
            .field("log_path", &self.log_path)
            .field("idle_timeout_secs", &self.idle_timeout_secs)
            .field("log_level", &self.log_level)
            .field("log_format", &self.log_format)
            .field("quality", &self.quality)
            .field("encoder", &self.encoder)
            .field("chroma_mode", &self.chroma_mode)
            .field("hidpi_scale", &self.hidpi_scale)
            .field("show_cursor", &self.show_cursor)
            .field("bitrate_mbps", &self.bitrate_mbps)
            .field("skip_unchanged", &self.skip_unchanged)
            .field("idle_keyframe_sec", &self.idle_keyframe_sec)
            .finish()
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_address: "0.0.0.0".to_string(),
            port: 3389,
            frame_rate: 60,
            width: 0,
            height: 0,
            username: None,
            password: None,
            allow_generated_credentials: false,
            cert_path: None,
            key_path: None,
            log_path: None,
            idle_timeout_secs: 1800,
            log_level: None,
            log_format: None,
            quality: None,
            encoder: None,
            chroma_mode: None,
            hidpi_scale: None,
            show_cursor: None,
            bitrate_mbps: None,
            skip_unchanged: None,
            idle_keyframe_sec: None,
        }
    }
}

impl ServerConfig {
    /// Load config from a TOML file path, or use defaults if None.
    pub fn load_from_file(path: Option<&std::path::Path>) -> anyhow::Result<Self> {
        if let Some(path) = path {
            let content = std::fs::read_to_string(path)?;
            Ok(toml::from_str(&content)?)
        } else {
            let default_path = default_config_path();
            if default_path.exists() {
                let content = std::fs::read_to_string(&default_path)?;
                Ok(toml::from_str(&content)?)
            } else {
                Ok(ServerConfig::default())
            }
        }
    }
}

/// Returns the macrdp config directory.
///
/// All daemon state (config, TLS material, logs) lives under this directory:
///
/// ```text
/// <config_dir>/config.toml      (default_config_path)
/// <config_dir>/tls/cert.pem     (default_cert_path)
/// <config_dir>/tls/key.pem      (default_key_path)
/// <config_dir>/logs/macrdp.log  (default_log_path)
/// ```
///
/// Uses the platform-native config directory:
/// `~/Library/Application Support/macrdp` on macOS and `$XDG_CONFIG_HOME/macrdp`
/// or `~/.config/macrdp` on Unix-like systems.
pub fn config_dir() -> PathBuf {
    if let Some(native) = dirs::config_dir() {
        return native.join("macrdp");
    }

    if let Some(home) = dirs::home_dir() {
        return home.join(".config").join("macrdp");
    }

    PathBuf::from(".macrdp")
}

pub fn default_config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn default_log_path() -> PathBuf {
    config_dir().join("logs").join("macrdp.log")
}

/// Directory holding the daemon's TLS material.
pub fn default_tls_dir() -> PathBuf {
    config_dir().join("tls")
}

pub fn default_cert_path() -> PathBuf {
    default_tls_dir().join("cert.pem")
}

pub fn default_key_path() -> PathBuf {
    default_tls_dir().join("key.pem")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ServerConfig::default();
        assert_eq!(config.port, 3389);
        assert_eq!(config.bind_address, "0.0.0.0");
        assert_eq!(config.frame_rate, 60);
        assert!(config.username.is_none());
    }

    #[test]
    fn test_parse_toml_config() {
        let toml_str = r#"
            port = 13389
            bind_address = "127.0.0.1"
            frame_rate = 120
            username = "admin"
            password = "secret"
            allow_generated_credentials = true
            cert_path = "/tmp/macrdp-cert.pem"
            key_path = "/tmp/macrdp-key.pem"
            log_path = "/tmp/macrdp.log"
        "#;
        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.port, 13389);
        assert_eq!(config.bind_address, "127.0.0.1");
        assert_eq!(config.frame_rate, 120);
        assert_eq!(config.username.as_deref(), Some("admin"));
        assert!(config.allow_generated_credentials);
        assert_eq!(
            config.cert_path.as_deref(),
            Some(std::path::Path::new("/tmp/macrdp-cert.pem"))
        );
        assert_eq!(
            config.key_path.as_deref(),
            Some(std::path::Path::new("/tmp/macrdp-key.pem"))
        );
        assert_eq!(
            config.log_path.as_deref(),
            Some(std::path::Path::new("/tmp/macrdp.log"))
        );
    }

    #[test]
    fn test_parse_idle_frame_config() {
        let toml_str = r#"
            skip_unchanged = false
            idle_keyframe_sec = 5
        "#;
        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.skip_unchanged, Some(false));
        assert_eq!(config.idle_keyframe_sec, Some(5));
    }

    #[test]
    fn test_parse_cursor_capture_config() {
        let toml_str = r#"
            show_cursor = true
        "#;
        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.show_cursor, Some(true));
    }

    #[test]
    fn debug_redacts_password() {
        let config = ServerConfig {
            username: Some("admin".to_string()),
            password: Some("hunter2".to_string()),
            ..ServerConfig::default()
        };

        let rendered = format!("{config:?}");

        assert!(!rendered.contains("hunter2"), "password leaked: {rendered}");
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains("admin"));
    }

    #[test]
    fn debug_redaction_marks_unset_password_as_none() {
        let config = ServerConfig::default();
        let rendered = format!("{config:?}");
        assert!(rendered.contains("password: None"));
    }

    #[test]
    fn parses_log_format() {
        let toml_str = r#"log_format = "json""#;
        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.log_format.as_deref(), Some("json"));
    }

    #[test]
    fn default_paths_share_config_dir() {
        let base = config_dir();

        assert!(default_config_path().starts_with(&base));
        assert!(default_log_path().starts_with(&base));
        assert!(default_tls_dir().starts_with(&base));
        assert!(default_cert_path().starts_with(default_tls_dir()));
        assert!(default_key_path().starts_with(default_tls_dir()));
    }

    #[test]
    fn test_rejects_ui_only_config_fields() {
        let toml_str = r#"
            port = 13389
            theme = "system"
            autostart = true
        "#;

        assert!(toml::from_str::<ServerConfig>(toml_str).is_err());
    }
}

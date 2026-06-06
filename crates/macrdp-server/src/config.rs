use clap::Parser;
use std::path::PathBuf;

use anyhow::Context;
use macrdp_core::{default_config_path, ServerConfig};

#[derive(Parser, Debug, Default)]
#[command(name = "macrdp", about = "macOS RDP Server")]
pub struct Cli {
    /// TCP port to listen on
    #[arg(short, long)]
    pub port: Option<u16>,

    /// IP address to listen on
    #[arg(long)]
    pub bind_address: Option<String>,

    /// Path to config file
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Path to TLS certificate file
    #[arg(long)]
    pub cert_path: Option<PathBuf>,

    /// Path to TLS private key file
    #[arg(long)]
    pub key_path: Option<PathBuf>,

    /// Path to append daemon logs
    #[arg(long)]
    pub log_path: Option<PathBuf>,

    /// RDP username override
    #[arg(long)]
    pub username: Option<String>,

    /// Environment variable containing the RDP password
    #[arg(long)]
    pub password_env: Option<String>,

    /// Allow temporary generated credentials when username/password are missing
    #[arg(long)]
    pub allow_generated_credentials: bool,

    /// Target frame rate (30, 60, or 120)
    #[arg(long)]
    pub frame_rate: Option<u32>,

    /// H.264 encoder: software, hardware, auto
    #[arg(long)]
    pub encoder: Option<String>,

    /// Target bitrate in Mbps (overrides auto-calculated ceiling)
    #[arg(long)]
    pub bitrate_mbps: Option<u32>,

    /// Chroma subsampling mode: avc420 or avc444
    #[arg(long, value_parser = ["avc420", "avc444"])]
    pub chroma_mode: Option<String>,

    /// Resolution: "auto", "WxH" (e.g. 3840x2160), or legacy scale factor (1-4)
    #[arg(long)]
    pub resolution: Option<String>,

    /// Log level: trace, debug, info, warn, error
    #[arg(long)]
    pub log_level: Option<String>,

    /// Log format: text (human-readable) or json (one JSON object per line)
    #[arg(long, value_parser = ["text", "json"])]
    pub log_format: Option<String>,

    /// CoreGraphics input tap: session (default), annotated_session, or hid
    #[arg(long, value_parser = ["session", "annotated_session", "hid"])]
    pub input_tap: Option<String>,

    /// Advertise the FreeRDP advanced-input channel
    #[arg(long)]
    pub advanced_input: bool,

    /// Print the macOS Screen Recording and Accessibility permission status
    /// and exit. Exit status is 0 if both are granted, 1 otherwise. Safe in a
    /// noninteractive context — never opens System Settings or prompts.
    #[arg(long)]
    pub check_permissions: bool,

    /// Output format for `--check-permissions`: text or json.
    #[arg(long, value_parser = ["text", "json"], default_value = "text")]
    pub check_permissions_format: String,

    /// Store the RDP password in the macOS Keychain and exit
    #[arg(long)]
    pub keychain_set_password: bool,

    /// Read the RDP password from the macOS Keychain at startup
    #[arg(long)]
    pub password_keychain: bool,
}

/// Load core config from file, then apply CLI overrides.
pub fn load_config(cli: &Cli) -> anyhow::Result<ServerConfig> {
    let default_path = default_config_path();
    let config_path = cli.config.as_deref().or_else(|| {
        if default_path.exists() {
            Some(default_path.as_path())
        } else {
            None
        }
    });

    if let Some(path) = config_path {
        tracing::info!(path = %path.display(), "Loading config");
    } else {
        tracing::info!("No config.toml found, using defaults");
    }

    let mut config = ServerConfig::load_from_file(config_path)?;

    if let Some(port) = cli.port {
        config.port = port;
    }
    if let Some(bind_address) = &cli.bind_address {
        config.bind_address = bind_address.clone();
    }
    if let Some(fps) = cli.frame_rate {
        config.frame_rate = fps;
    }
    if let Some(encoder) = &cli.encoder {
        config.encoder = Some(encoder.clone());
    }
    if let Some(bitrate) = cli.bitrate_mbps {
        config.bitrate_mbps = Some(bitrate);
    }
    if let Some(chroma) = &cli.chroma_mode {
        config.chroma_mode = Some(chroma.clone());
    }
    if let Some(res) = &cli.resolution {
        config.resolution = Some(res.clone());
    }
    if let Some(level) = &cli.log_level {
        config.log_level = Some(level.clone());
    }
    if let Some(format) = &cli.log_format {
        config.log_format = Some(format.clone());
    }
    if let Some(input_tap) = &cli.input_tap {
        config.input_tap = Some(input_tap.clone());
    }
    if cli.advanced_input {
        config.advanced_input = Some(true);
    }
    if let Some(cert_path) = &cli.cert_path {
        config.cert_path = Some(cert_path.clone());
    }
    if let Some(key_path) = &cli.key_path {
        config.key_path = Some(key_path.clone());
    }
    if let Some(log_path) = &cli.log_path {
        config.log_path = Some(log_path.clone());
    }
    if let Some(username) = &cli.username {
        config.username = Some(username.clone());
    }
    if let Some(password_env) = &cli.password_env {
        let password = std::env::var(password_env)
            .with_context(|| format!("Failed to read password from ${password_env}"))?;
        config.password = Some(password);
    }
    if cli.password_keychain {
        let password = crate::keychain::get_password(config.username.as_deref())?;
        config.password = Some(password);
    }
    if cli.allow_generated_credentials {
        config.allow_generated_credentials = true;
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ServerConfig::default();
        assert_eq!(config.port, 3389);
        assert_eq!(config.frame_rate, 60);
        assert!(config.username.is_none());
    }

    #[test]
    fn test_parse_encoding_config() {
        let toml_str = r#"
            port = 3389
            skip_unchanged = false
            idle_keyframe_sec = 5
        "#;
        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.skip_unchanged, Some(false));
        assert_eq!(config.idle_keyframe_sec, Some(5));
    }

    #[test]
    fn test_default_encoding_config() {
        let config = ServerConfig::default();
        assert!(config.skip_unchanged.is_none());
        assert!(config.idle_keyframe_sec.is_none());
    }

    #[test]
    fn test_parse_toml_config() {
        let toml_str = r#"
            port = 13389
            bind_address = "127.0.0.1"
            frame_rate = 120
            username = "admin"
            password = "secret"
        "#;
        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.port, 13389);
        assert_eq!(config.bind_address, "127.0.0.1");
        assert_eq!(config.frame_rate, 120);
        assert_eq!(config.username.as_deref(), Some("admin"));
        assert_eq!(config.password.as_deref(), Some("secret"));
    }

    #[test]
    fn test_cli_overrides_core_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
            port = 13389
            frame_rate = 30
            log_level = "warn"
            "#,
        )
        .unwrap();

        let cli = Cli {
            port: Some(3390),
            bind_address: Some("127.0.0.1".to_string()),
            config: Some(config_path),
            cert_path: Some(dir.path().join("cert.pem")),
            key_path: Some(dir.path().join("key.pem")),
            log_path: Some(dir.path().join("macrdp.log")),
            username: Some("daemon".to_string()),
            allow_generated_credentials: true,
            frame_rate: Some(60),
            encoder: Some("hardware".to_string()),
            bitrate_mbps: Some(20),
            chroma_mode: Some("avc444".to_string()),
            resolution: Some("2".to_string()),
            log_level: Some("debug".to_string()),
            input_tap: Some("hid".to_string()),
            advanced_input: true,
            ..Cli::default()
        };

        let config = load_config(&cli).unwrap();
        assert_eq!(config.port, 3390);
        assert_eq!(config.bind_address, "127.0.0.1");
        assert_eq!(config.frame_rate, 60);
        assert_eq!(config.username.as_deref(), Some("daemon"));
        assert!(config.allow_generated_credentials);
        assert_eq!(config.log_level.as_deref(), Some("debug"));
        assert_eq!(config.encoder.as_deref(), Some("hardware"));
        assert_eq!(config.bitrate_mbps, Some(20));
        assert_eq!(config.chroma_mode.as_deref(), Some("avc444"));
        assert_eq!(config.resolution.as_deref(), Some("2"));
        assert_eq!(config.input_tap.as_deref(), Some("hid"));
        assert_eq!(config.advanced_input, Some(true));
        assert_eq!(
            config.cert_path.as_deref(),
            Some(dir.path().join("cert.pem").as_path())
        );
        assert_eq!(
            config.key_path.as_deref(),
            Some(dir.path().join("key.pem").as_path())
        );
        assert_eq!(
            config.log_path.as_deref(),
            Some(dir.path().join("macrdp.log").as_path())
        );
    }

    #[test]
    fn test_cli_can_override_config_port_to_default_rdp_port() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "port = 13389\n").unwrap();

        let cli = Cli {
            port: Some(3389),
            config: Some(config_path),
            ..Cli::default()
        };

        let config = load_config(&cli).unwrap();
        assert_eq!(config.port, 3389);
    }

    #[test]
    fn test_cli_password_env_overrides_config_password() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
            username = "admin"
            password = "from-config"
            "#,
        )
        .unwrap();

        let env_name = "MACRDP_TEST_PASSWORD_ENV";
        std::env::set_var(env_name, "from-env");

        let cli = Cli {
            config: Some(config_path),
            password_env: Some(env_name.to_string()),
            ..Cli::default()
        };

        let config = load_config(&cli).unwrap();
        assert_eq!(config.password.as_deref(), Some("from-env"));
    }
}

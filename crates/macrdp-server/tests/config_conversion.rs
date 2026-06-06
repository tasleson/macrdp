use macrdp_server::config::{load_config, Cli};

fn cli_for_config(config: std::path::PathBuf) -> Cli {
    Cli {
        config: Some(config),
        ..Cli::default()
    }
}

#[test]
fn converts_file_config_to_core_config() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    let cert_path = dir.path().join("tls").join("cert.pem");
    let key_path = dir.path().join("tls").join("key.pem");
    let log_path = dir.path().join("logs").join("macrdp.log");

    std::fs::write(
        &config_path,
        format!(
            r#"
            bind_address = "127.0.0.1"
            port = 13389
            frame_rate = 30
            username = "daemon"
            password = "secret"
            cert_path = "{}"
            key_path = "{}"
            log_path = "{}"
            encoder = "hardware"
            bitrate_mbps = 12
            chroma_mode = "avc420"
            resolution = "2"
            skip_unchanged = true
            idle_keyframe_sec = 7
            "#,
            cert_path.display(),
            key_path.display(),
            log_path.display()
        ),
    )
    .unwrap();

    let config = load_config(&cli_for_config(config_path)).unwrap();

    assert_eq!(config.bind_address, "127.0.0.1");
    assert_eq!(config.port, 13389);
    assert_eq!(config.frame_rate, 30);
    assert_eq!(config.username.as_deref(), Some("daemon"));
    assert_eq!(config.password.as_deref(), Some("secret"));
    assert_eq!(config.cert_path.as_deref(), Some(cert_path.as_path()));
    assert_eq!(config.key_path.as_deref(), Some(key_path.as_path()));
    assert_eq!(config.log_path.as_deref(), Some(log_path.as_path()));
    assert_eq!(config.encoder.as_deref(), Some("hardware"));
    assert_eq!(config.bitrate_mbps, Some(12));
    assert_eq!(config.chroma_mode.as_deref(), Some("avc420"));
    assert_eq!(config.resolution.as_deref(), Some("2"));
    assert_eq!(config.skip_unchanged, Some(true));
    assert_eq!(config.idle_keyframe_sec, Some(7));
}

#[test]
fn cli_overrides_loaded_core_config() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
        bind_address = "0.0.0.0"
        port = 13389
        frame_rate = 30
        username = "from-file"
        log_level = "warn"
        "#,
    )
    .unwrap();

    let cli = Cli {
        port: Some(3390),
        bind_address: Some("127.0.0.1".to_string()),
        config: Some(config_path),
        cert_path: Some(dir.path().join("override-cert.pem")),
        key_path: Some(dir.path().join("override-key.pem")),
        log_path: Some(dir.path().join("override.log")),
        username: Some("from-cli".to_string()),
        allow_generated_credentials: true,
        frame_rate: Some(60),
        encoder: Some("software".to_string()),
        bitrate_mbps: Some(8),
        chroma_mode: Some("avc420".to_string()),
        resolution: Some("1".to_string()),
        log_level: Some("debug".to_string()),
        ..Cli::default()
    };

    let config = load_config(&cli).unwrap();

    assert_eq!(config.bind_address, "127.0.0.1");
    assert_eq!(config.port, 3390);
    assert_eq!(config.frame_rate, 60);
    assert_eq!(config.username.as_deref(), Some("from-cli"));
    assert_eq!(config.log_level.as_deref(), Some("debug"));
    assert!(config.allow_generated_credentials);
    assert_eq!(
        config.cert_path.as_deref(),
        Some(dir.path().join("override-cert.pem").as_path())
    );
    assert_eq!(
        config.key_path.as_deref(),
        Some(dir.path().join("override-key.pem").as_path())
    );
    assert_eq!(
        config.log_path.as_deref(),
        Some(dir.path().join("override.log").as_path())
    );
    assert_eq!(config.encoder.as_deref(), Some("software"));
    assert_eq!(config.bitrate_mbps, Some(8));
    assert_eq!(config.chroma_mode.as_deref(), Some("avc420"));
    assert_eq!(config.resolution.as_deref(), Some("1"));
}

use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use macrdp_core::{
    start_server_with_options, ServerConfig, ServerEventHandler, ServerStartupOptions, ServerStatus,
};
use tempfile::TempDir;

#[derive(Clone, Default)]
struct RecordingHandler {
    statuses: Arc<Mutex<Vec<ServerStatus>>>,
}

impl RecordingHandler {
    fn statuses(&self) -> Vec<ServerStatus> {
        self.statuses.lock().unwrap().clone()
    }
}

impl ServerEventHandler for RecordingHandler {
    fn on_status_change(&self, status: ServerStatus) {
        self.statuses.lock().unwrap().push(status);
    }
}

fn test_options() -> ServerStartupOptions {
    ServerStartupOptions {
        request_permissions: false,
    }
}

fn test_config(dir: &TempDir) -> ServerConfig {
    // Pre-create the cert/key because operator-supplied paths in
    // `cert_path`/`key_path` must already exist on disk — the daemon refuses
    // to generate at explicit paths to avoid masking operator misconfiguration.
    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");
    macrdp_core::generate_self_signed_cert(&cert_path, &key_path)
        .expect("test setup: generate cert");
    ServerConfig {
        bind_address: "127.0.0.1".to_string(),
        port: 0,
        width: 640,
        height: 480,
        frame_rate: 30,
        username: Some("daemon".to_string()),
        password: Some("secret".to_string()),
        cert_path: Some(cert_path),
        key_path: Some(key_path),
        ..ServerConfig::default()
    }
}

#[tokio::test]
async fn startup_rejects_missing_credentials() {
    let handler = RecordingHandler::default();
    let err =
        match start_server_with_options(ServerConfig::default(), handler.clone(), test_options())
            .await
        {
            Ok(handle) => {
                handle.stop().await.unwrap();
                panic!("server started without credentials");
            }
            Err(err) => err,
        };

    assert!(err.to_string().contains("credentials are required"));
    assert_eq!(handler.statuses()[0].state, "starting");
}

#[tokio::test]
async fn startup_rejects_invalid_bind_address() {
    let dir = TempDir::new().unwrap();
    let mut config = test_config(&dir);
    config.bind_address = "localhost".to_string();

    let err = match start_server_with_options(config, RecordingHandler::default(), test_options())
        .await
    {
        Ok(handle) => {
            handle.stop().await.unwrap();
            panic!("server started with invalid bind address");
        }
        Err(err) => err,
    };

    assert!(err.to_string().contains("Invalid bind_address"));
}

#[tokio::test]
async fn startup_rejects_occupied_port() {
    let occupied = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let occupied_port = occupied.local_addr().unwrap().port();
    let dir = TempDir::new().unwrap();
    let mut config = test_config(&dir);
    config.port = occupied_port;

    let err = match start_server_with_options(config, RecordingHandler::default(), test_options())
        .await
    {
        Ok(handle) => {
            handle.stop().await.unwrap();
            panic!("server started on occupied port");
        }
        Err(err) => err,
    };

    assert!(err.to_string().contains("Failed to bind"));
}

#[tokio::test]
async fn server_stops_gracefully() {
    let dir = TempDir::new().unwrap();
    let handler = RecordingHandler::default();

    let handle = start_server_with_options(test_config(&dir), handler.clone(), test_options())
        .await
        .unwrap();

    assert_ne!(handle.port(), 0);
    assert!(handle.status().running);
    assert!(handler
        .statuses()
        .iter()
        .any(|status| status.state == "running"));

    tokio::time::timeout(Duration::from_secs(2), handle.stop())
        .await
        .expect("server stop timed out")
        .unwrap();

    assert!(!handle.status().running);
}

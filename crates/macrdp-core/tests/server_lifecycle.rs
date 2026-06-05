use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use macrdp_core::{
    start_server_with_options, ServerConfig, ServerEventHandler, ServerStartupOptions, ServerStatus,
};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

/// Connect and immediately drop the TCP stream — no bytes sent, clean EOF.
/// The server must survive and remain stoppable.
#[tokio::test]
async fn rude_client_immediate_disconnect_does_not_hang_server() {
    let dir = TempDir::new().unwrap();
    let handle = start_server_with_options(
        test_config(&dir),
        RecordingHandler::default(),
        test_options(),
    )
    .await
    .unwrap();

    let port = handle.port();
    for _ in 0..3 {
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("failed to connect to test server");
        drop(stream);
        // Small gap so the server accept-loop can process the close.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(handle.status().running);
    tokio::time::timeout(Duration::from_secs(5), handle.stop())
        .await
        .expect("server stop timed out after rude disconnects")
        .unwrap();
    assert!(!handle.status().running);
}

/// Send random garbage bytes then drop — exercises the protocol-parse error path.
/// The server must survive and remain stoppable.
#[tokio::test]
async fn rude_client_garbage_bytes_does_not_hang_server() {
    let dir = TempDir::new().unwrap();
    let handle = start_server_with_options(
        test_config(&dir),
        RecordingHandler::default(),
        test_options(),
    )
    .await
    .unwrap();

    let port = handle.port();
    for _ in 0..3 {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("failed to connect to test server");
        let _ = stream.write_all(b"GARBAGE GARBAGE GARBAGE\r\n\r\n").await;
        drop(stream);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(handle.status().running);
    tokio::time::timeout(Duration::from_secs(5), handle.stop())
        .await
        .expect("server stop timed out after garbage-byte clients")
        .unwrap();
    assert!(!handle.status().running);
}

/// Stress the server's connection-state reset path by aborting at a randomly
/// chosen stage of the pre-TLS RDP handshake, across many iterations.
///
/// The RDP pre-TLS path the server side runs through is:
///   InitiationWaitRequest → InitiationSendConfirm → SecurityUpgrade → (TLS)
///
/// Seven abort stages are exercised at random:
///   0 — pure EOF, no bytes sent
///   1 — partial TPKT header (1–3 bytes)
///   2 — TPKT header + truncated X.224 CR body
///   3 — complete valid X.224 CR (SSL), then EOF before TLS
///   4 — complete X.224 CR, read server CC, then EOF during TLS upgrade
///   5 — complete X.224 CR, read server CC, send a truncated TLS ClientHello
///   6 — random-length random-byte garbage
#[tokio::test]
async fn rude_client_random_stage_abort_stresses_deactivation_reactivation() {
    use rand::Rng as _;

    let dir = TempDir::new().unwrap();
    let handle = start_server_with_options(
        test_config(&dir),
        RecordingHandler::default(),
        test_options(),
    )
    .await
    .unwrap();

    let port = handle.port();
    let mut rng = rand::thread_rng();

    // Valid X.224 Connection Request selecting SSL-only (0x01).  The server
    // replies with a Connection Confirm and then attempts a TLS upgrade —
    // giving us two extra drop points after the first protocol exchange.
    //
    // Packet layout (19 bytes total):
    //   [TPKT]  03 00 00 13           version=3, len=19
    //   [X.224] 0e e0 00 00 00 00 00  LI=14, CR code, dst-ref, src-ref, class-opt
    //   [NEG]   01 00 08 00           RDP_NEG_REQ: type=1, flags=0, length=8
    //           01 00 00 00           requestedProtocols = PROTOCOL_SSL
    const X224_CR_SSL: &[u8] = &[
        0x03, 0x00, 0x00, 0x13, 0x0e, 0xe0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x08, 0x00,
        0x01, 0x00, 0x00, 0x00,
    ];

    const ITERATIONS: usize = 40;
    let mut buf = vec![0u8; 512];

    for _ in 0..ITERATIONS {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("failed to connect to test server");

        match rng.gen_range(0u8..=6) {
            // Pure EOF — server sees immediate connection close.
            0 => {}

            // Partial TPKT header: 1–3 bytes, then EOF.
            1 => {
                let n = rng.gen_range(1usize..4);
                let _ = stream.write_all(&X224_CR_SSL[..n]).await;
            }

            // TPKT header + truncated X.224 CR body, then EOF.
            2 => {
                let n = rng.gen_range(4usize..X224_CR_SSL.len());
                let _ = stream.write_all(&X224_CR_SSL[..n]).await;
            }

            // Complete X.224 CR, then immediate EOF before reading response.
            3 => {
                let _ = stream.write_all(X224_CR_SSL).await;
            }

            // Complete X.224 CR, wait for server's CC, then EOF during TLS upgrade.
            4 => {
                let _ = stream.write_all(X224_CR_SSL).await;
                let _ =
                    tokio::time::timeout(Duration::from_millis(400), stream.read(&mut buf)).await;
            }

            // X.224 CR + server CC read + truncated TLS ClientHello.
            // TLS record header: type=22 (handshake), version=3.1, then junk.
            5 => {
                let _ = stream.write_all(X224_CR_SSL).await;
                let _ =
                    tokio::time::timeout(Duration::from_millis(400), stream.read(&mut buf)).await;
                let tls_junk_len = rng.gen_range(1usize..=32);
                let tls_junk: Vec<u8> = std::iter::once(0x16u8)
                    .chain(std::iter::once(0x03))
                    .chain(std::iter::once(0x01))
                    .chain((0..tls_junk_len).map(|_| rng.gen::<u8>()))
                    .collect();
                let _ = stream.write_all(&tls_junk).await;
            }

            // Random-length, random-content garbage.
            _ => {
                let len = rng.gen_range(1usize..=128);
                let garbage: Vec<u8> = (0..len).map(|_| rng.gen::<u8>()).collect();
                let _ = stream.write_all(&garbage).await;
            }
        }

        drop(stream);
        // Brief gap so the server accept-loop can process the close before
        // the next connection arrives.
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    assert!(
        handle.status().running,
        "server crashed during rude-client stress"
    );
    tokio::time::timeout(Duration::from_secs(10), handle.stop())
        .await
        .expect("server stop timed out after random-stage stress")
        .unwrap();
    assert!(!handle.status().running);
}

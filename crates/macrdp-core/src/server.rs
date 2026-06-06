//! RDP server lifecycle: start, stop, metrics, connections

use std::net::{IpAddr, SocketAddr, TcpListener};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use ironrdp_server::gfx::GfxState;
use rand::{rngs::OsRng, RngCore};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::callbacks::*;
use crate::config::ServerConfig;
use crate::features;
use crate::permissions;
use crate::tls;

/// Startup behavior options for the RDP server runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerStartupOptions {
    /// Whether startup should request/check macOS Screen Recording and
    /// Accessibility permissions. Tests can disable this to avoid OS prompts.
    pub request_permissions: bool,
}

impl Default for ServerStartupOptions {
    fn default() -> Self {
        Self {
            request_permissions: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedCredentials {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedCredentials {
    username: String,
    password: String,
    generated: bool,
}

/// Handle to a running RDP server. Use to query state and stop the server.
pub struct ServerHandle {
    port: u16,
    started_at: Instant,
    gfx_state: Arc<Mutex<GfxState>>,
    shutdown_notify: Arc<Notify>,
    generated_credentials: Option<GeneratedCredentials>,
    stopped: AtomicBool,
    /// The OS thread running the RDP server (RdpServer is !Send, needs dedicated thread)
    server_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    metrics_task: Mutex<Option<JoinHandle<()>>>,
}

impl ServerHandle {
    /// The TCP port the server is listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Credentials generated for this process, if explicitly enabled.
    pub fn generated_credentials(&self) -> Option<&GeneratedCredentials> {
        self.generated_credentials.as_ref()
    }

    /// Current server status.
    pub fn status(&self) -> ServerStatus {
        if self.stopped.load(Ordering::Relaxed) {
            ServerStatus {
                running: false,
                state: "stopped".to_string(),
                uptime_secs: 0,
            }
        } else {
            ServerStatus {
                running: true,
                state: "running".to_string(),
                uptime_secs: self.started_at.elapsed().as_secs(),
            }
        }
    }

    /// Current performance metrics.
    pub fn metrics(&self) -> Metrics {
        let gfx = self.gfx_state.lock().unwrap();
        let net_ms = (gfx.rtt_ewma_ms - gfx.last_encode_ms).max(0.0);
        Metrics {
            fps: 0, // Actual FPS comes from config/runtime
            bitrate_kbps: 0,
            rtt_ms: gfx.rtt_ewma_ms,
            bytes_sent: gfx.total_bytes_sent,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            encode_ms: gfx.last_encode_ms,
            net_ms,
            last_frame_bytes: gfx.last_frame_bytes,
            network_quality: gfx.network_quality,
            pending_acks: gfx.pending_acks,
        }
    }

    /// Gracefully stop the server.
    pub async fn stop(&self) -> Result<()> {
        if self.stopped.swap(true, Ordering::Relaxed) {
            return Ok(()); // Already stopped
        }
        self.shutdown_notify.notify_one();

        // Extract thread handle from mutex *before* the await point so that
        // the MutexGuard is dropped and the future remains Send.
        let thread = self.server_thread.lock().unwrap().take();
        if let Some(thread) = thread {
            let _ = tokio::task::spawn_blocking(move || {
                let _ = thread.join();
            })
            .await;
        }
        // Cancel metrics task
        let task = self.metrics_task.lock().unwrap().take();
        if let Some(task) = task {
            task.abort();
        }
        Ok(())
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        if !self.stopped.load(Ordering::Relaxed) {
            self.shutdown_notify.notify_one();
        }
    }
}

/// Arguments that are Send-safe, passed to the server thread.
/// RdpServer itself is !Send, so we pass everything needed to construct it.
struct ServerThreadArgs {
    config: ServerConfig,
    bind_addr: SocketAddr,
    listener: TcpListener,
    cert_path: std::path::PathBuf,
    key_path: std::path::PathBuf,
    /// SCK logical display size (used for capture sizing and native resolution)
    sck_w: u16,
    sck_h: u16,
    /// CG logical display size (CGEvent coordinate space, used for mouse mapping)
    cg_w: u16,
    cg_h: u16,
    width: u16,
    height: u16,
    quality: macrdp_encode::Quality,
    encoder_pref: macrdp_encode::EncoderPreference,
    mode_444: bool,
    gfx_state: Arc<Mutex<GfxState>>,
    shutdown_notify: Arc<Notify>,
    credentials: ResolvedCredentials,
    handler: Arc<dyn ServerEventHandler>,
}

fn validate_bind_address(bind_address: &str, port: u16) -> Result<(TcpListener, SocketAddr)> {
    let ip: IpAddr = bind_address.parse().with_context(|| {
        format!("Invalid bind_address `{bind_address}`; expected an IP address")
    })?;
    let requested_addr = SocketAddr::new(ip, port);

    // For the IPv6 unspecified address (`::`), build a dual-stack listener so
    // mDNS or other resolvers handing the client an IPv4-mapped address still
    // reach us. macOS defaults IPV6_V6ONLY to true, so clear it explicitly.
    let listener = if matches!(ip, IpAddr::V6(v6) if v6.is_unspecified()) {
        use socket2::{Domain, Protocol, Socket, Type};
        let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))
            .context("Failed to create IPv6 TCP socket")?;
        socket
            .set_only_v6(false)
            .context("Failed to clear IPV6_V6ONLY for dual-stack listener")?;
        socket
            .bind(&requested_addr.into())
            .with_context(|| format!("Failed to bind to {requested_addr}"))?;
        socket
            .listen(1024)
            .with_context(|| format!("Failed to listen on {requested_addr}"))?;
        TcpListener::from(socket)
    } else {
        TcpListener::bind(requested_addr)
            .with_context(|| format!("Failed to bind to {requested_addr}"))?
    };
    let actual_addr = listener.local_addr()?;
    Ok((listener, actual_addr))
}

fn resolve_credentials(config: &ServerConfig) -> Result<ResolvedCredentials> {
    match (&config.username, &config.password) {
        (Some(username), Some(password))
            if !username.trim().is_empty() && !password.trim().is_empty() =>
        {
            Ok(ResolvedCredentials {
                username: username.clone(),
                password: password.clone(),
                generated: false,
            })
        }
        (None, None) if config.allow_generated_credentials => Ok(ResolvedCredentials {
            username: "macrdp".to_string(),
            password: generate_password(24),
            generated: true,
        }),
        _ => {
            bail!(
                "RDP credentials are required; configure non-empty username/password or explicitly enable generated credentials"
            )
        }
    }
}

fn generate_password(len: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let mut password = String::with_capacity(len);
    let threshold = u8::MAX - (u8::MAX % ALPHABET.len() as u8);
    let mut rng = OsRng;
    let mut byte = [0u8; 1];

    while password.len() < len {
        rng.fill_bytes(&mut byte);
        if byte[0] < threshold {
            let idx = (byte[0] as usize) % ALPHABET.len();
            password.push(ALPHABET[idx] as char);
        }
    }

    password
}

/// Parse the `resolution` config: "auto" (Retina-aware), "WxH" (explicit), or
/// a legacy integer scale factor. Returns `(width, height, is_auto)`.
pub fn resolve_resolution(resolution: Option<&str>, base_w: u16, base_h: u16) -> (u16, u16, bool) {
    let mode = resolution.unwrap_or("auto");
    if mode == "auto" {
        let scale = macrdp_capture::detect_display_scale().unwrap_or(1);
        tracing::info!(scale, base_w, base_h, "Resolution auto: display scale");
        (
            base_w.saturating_mul(scale as u16),
            base_h.saturating_mul(scale as u16),
            true,
        )
    } else if let Some((w, h)) = mode
        .split_once('x')
        .and_then(|(w, h)| Some((w.parse::<u16>().ok()?, h.parse::<u16>().ok()?)))
    {
        tracing::info!(w, h, "Resolution: explicit WxH");
        (w, h, false)
    } else if let Ok(scale) = mode.parse::<u32>() {
        let scale = scale.clamp(1, 4);
        let rw = base_w.saturating_mul(scale as u16);
        let rh = base_h.saturating_mul(scale as u16);
        tracing::info!(scale, rw, rh, "Resolution: legacy hidpi_scale");
        (rw, rh, false)
    } else {
        tracing::warn!(mode, "Resolution: unrecognized, using logical");
        (base_w, base_h, true)
    }
}

fn resolve_chroma_mode(chroma_mode: Option<&str>) -> Result<bool> {
    match chroma_mode {
        None | Some("avc420") => Ok(false),
        Some("avc444") => Ok(true),
        Some(other) => {
            bail!("Invalid chroma_mode `{other}`; expected `avc420` or `avc444`")
        }
    }
}

fn resolve_input_tap(input_tap: Option<&str>) -> Result<macrdp_input::InputTapLocation> {
    match input_tap {
        None | Some("session") => Ok(macrdp_input::InputTapLocation::Session),
        Some("annotated_session") => Ok(macrdp_input::InputTapLocation::AnnotatedSession),
        Some("hid") => Ok(macrdp_input::InputTapLocation::Hid),
        Some(other) => {
            bail!("Invalid input_tap `{other}`; expected `session`, `annotated_session`, or `hid`")
        }
    }
}

/// Start the RDP server with the given configuration and event handler.
/// Returns a handle to control the running server.
pub async fn start_server(
    config: ServerConfig,
    handler: impl ServerEventHandler,
) -> Result<Arc<ServerHandle>> {
    start_server_with_options(config, handler, ServerStartupOptions::default()).await
}

/// Start the RDP server with explicit startup behavior options.
pub async fn start_server_with_options(
    config: ServerConfig,
    handler: impl ServerEventHandler,
    options: ServerStartupOptions,
) -> Result<Arc<ServerHandle>> {
    let handler: Arc<dyn ServerEventHandler> = Arc::new(handler);

    // Notify handler: starting
    handler.on_status_change(ServerStatus {
        running: true,
        state: "starting".to_string(),
        uptime_secs: 0,
    });

    let credentials = resolve_credentials(&config)?;
    if credentials.generated {
        tracing::warn!(
            user = %credentials.username,
            "Using generated temporary credentials; password is not logged"
        );
    }

    // TLS
    let (cert_path, key_path) =
        tls::ensure_tls_files(config.cert_path.as_deref(), config.key_path.as_deref())?;

    let (listener, bind_addr) = validate_bind_address(&config.bind_address, config.port)?;
    let port = bind_addr.port();

    // Check and request permissions only after deterministic startup validation
    // has completed, so config/bind failures do not trigger OS permission prompts.
    let perms = if options.request_permissions {
        permissions::request_permissions()
    } else {
        PermissionStatus::default()
    };
    tracing::info!(?perms, "Permission status");

    // Stronger preflight than the CoreGraphics permission check: ask
    // ScreenCaptureKit how many displays it sees right now. The CG check can
    // (and does) report "granted" for a binary that SCK refuses to enumerate
    // displays for, in which case the daemon would accept a client and then
    // fail the first frame — the visible "white screen" symptom. Skip when
    // permission prompting is disabled (tests, sandboxed contexts) so the
    // suite stays portable.
    if options.request_permissions {
        match permissions::verify_sck_capture_ready() {
            Ok(0) => {
                tracing::warn!(
                    "ScreenCaptureKit preflight: display asleep, \
                     capture will wake it when a client connects"
                );
            }
            Ok(n) => {
                tracing::info!(display_count = n, "ScreenCaptureKit preflight ok");
            }
            Err(e) => {
                handler.on_status_change(ServerStatus {
                    running: false,
                    state: format!("error: {e}"),
                    uptime_secs: 0,
                });
                return Err(e.context(
                    "ScreenCaptureKit cannot enumerate displays — refusing to start; \
                     a running daemon would accept clients and serve blank screens",
                ));
            }
        }
    }

    // Display detection — separate SCK (capture) and CG (mouse mapping) sizes.
    // CGEvent operates in the CG coordinate space, so mouse mapping MUST use CG dimensions.
    let fixed_config_resolution = config.width > 0 && config.height > 0;
    let (sck_w, sck_h) = if !options.request_permissions && fixed_config_resolution {
        (config.width as u16, config.height as u16)
    } else {
        match permissions::detect_display_size() {
            Ok((w, h)) => {
                tracing::info!(width = w, height = h, "SCK logical display size");
                (w as u16, h as u16)
            }
            Err(e) => {
                tracing::warn!("Failed to detect SCK display size: {e}, defaulting to 1920x1080");
                (1920u16, 1080u16)
            }
        }
    };
    let (cg_w, cg_h) = if !options.request_permissions && fixed_config_resolution {
        (sck_w, sck_h)
    } else {
        match macrdp_capture::detect_cg_display_size() {
            Ok((w, h)) => {
                tracing::info!(
                    width = w,
                    height = h,
                    "CG logical display size (CGEvent coordinate space)"
                );
                (w as u16, h as u16)
            }
            Err(e) => {
                tracing::warn!("Failed to detect CG display size: {e}, falling back to SCK size");
                (sck_w, sck_h)
            }
        }
    };
    if sck_w != cg_w || sck_h != cg_h {
        tracing::warn!(
            sck_w,
            sck_h,
            cg_w,
            cg_h,
            "SCK and CG display sizes differ — using CG for mouse mapping, SCK for capture"
        );
    }

    // Base resolution from config or auto-detect
    let (width, height) = if config.width > 0 && config.height > 0 {
        (config.width as u16, config.height as u16)
    } else {
        (sck_w, sck_h)
    };

    // Quality / encoder / chroma
    let quality = match config.quality.as_deref() {
        Some("low_latency") => macrdp_encode::Quality::LowLatency,
        Some("balanced") => macrdp_encode::Quality::Balanced,
        _ => macrdp_encode::Quality::HighQuality,
    };
    let encoder_pref = macrdp_encode::EncoderPreference::from_str_opt(config.encoder.as_deref());
    let mode_444 = resolve_chroma_mode(config.chroma_mode.as_deref())?;
    tracing::info!(
        chroma_mode = if mode_444 { "avc444" } else { "avc420" },
        "Resolved H.264 chroma mode"
    );

    // Resolution: "auto" (Retina-aware), "WxH" (explicit), or legacy integer scale
    let (width, height, _res_auto) =
        resolve_resolution(config.resolution.as_deref(), width, height);

    // GFX state (shared between server thread and metrics task)
    let gfx_state = Arc::new(Mutex::new(GfxState::new(width, height, mode_444)));

    let shutdown_notify = Arc::new(Notify::new());

    // Pack everything the server thread needs
    let args = ServerThreadArgs {
        config: config.clone(),
        bind_addr,
        listener,
        cert_path,
        key_path,
        sck_w,
        sck_h,
        cg_w,
        cg_h,
        width,
        height,
        quality,
        encoder_pref,
        mode_444,
        gfx_state: Arc::clone(&gfx_state),
        shutdown_notify: Arc::clone(&shutdown_notify),
        credentials: credentials.clone(),
        handler: Arc::clone(&handler),
    };

    // Spawn RDP server on a dedicated OS thread.
    // RdpServer is !Send (contains dyn SoundServerFactory, dyn CliprdrServerFactory),
    // so we construct and run it entirely within this thread.
    let server_thread = std::thread::Builder::new()
        .name("rdp-server".into())
        .spawn(move || {
            run_server_thread(args);
        })
        .context("Failed to spawn RDP server thread")?;

    // Spawn metrics ticker (1 second interval) — all types here are Send
    let metrics_gfx = Arc::clone(&gfx_state);
    let handler_for_metrics = Arc::clone(&handler);
    let fps = config.frame_rate;
    let metrics_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        let mut prev_bytes_sent: u64 = 0;
        loop {
            interval.tick().await;
            let (rtt_ms, bytes_sent, encode_ms, last_frame_bytes, network_quality, pending_acks) = {
                let gfx = metrics_gfx.lock().unwrap();
                (
                    gfx.rtt_ewma_ms,
                    gfx.total_bytes_sent,
                    gfx.last_encode_ms,
                    gfx.last_frame_bytes,
                    gfx.network_quality,
                    gfx.pending_acks,
                )
            };
            let delta_bytes = bytes_sent.saturating_sub(prev_bytes_sent);
            prev_bytes_sent = bytes_sent;
            let bitrate_kbps = delta_bytes * 8 / 1000;
            let net_ms = (rtt_ms - encode_ms).max(0.0);
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            handler_for_metrics.on_metrics(Metrics {
                fps,
                bitrate_kbps,
                rtt_ms,
                bytes_sent,
                timestamp,
                encode_ms,
                net_ms,
                last_frame_bytes,
                network_quality,
                pending_acks,
            });
        }
    });

    let handle = Arc::new(ServerHandle {
        port,
        started_at: Instant::now(),
        gfx_state,
        shutdown_notify,
        generated_credentials: credentials.generated.then_some(GeneratedCredentials {
            username: credentials.username,
            password: credentials.password,
        }),
        stopped: AtomicBool::new(false),
        server_thread: Mutex::new(Some(server_thread)),
        metrics_task: Mutex::new(Some(metrics_task)),
    });

    // Notify handler: running
    handler.on_status_change(ServerStatus {
        running: true,
        state: "running".to_string(),
        uptime_secs: 0,
    });

    Ok(handle)
}

/// Runs the RDP server entirely on the current thread. Called from the dedicated
/// server thread. Constructs the !Send RdpServer here so it never crosses thread
/// boundaries.
fn run_server_thread(args: ServerThreadArgs) {
    use crate::clipboard::MacClipboardFactory;
    use crate::display::MacDisplay;
    use crate::handler::MacInputHandler;
    use ironrdp_server::{Credentials, RdpServer, ServerEvent, TlsIdentityCtx};

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime for RDP server");

    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let supported_features = features::SUPPORTED_DAEMON_FEATURES.join(", ");
        let deferred_features = features::DEFERRED_DAEMON_FEATURES.join(", ");
        tracing::info!(
            supported = %supported_features,
            deferred = %deferred_features,
            "macrdp v1 daemon feature policy"
        );

        // Load TLS identity (on this thread)
        let tls_identity = match TlsIdentityCtx::init_from_paths(&args.cert_path, &args.key_path) {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("Failed to load TLS certificate: {e}");
                args.handler.on_status_change(ServerStatus {
                    running: false,
                    state: format!("error: {e}"),
                    uptime_secs: 0,
                });
                return;
            }
        };
        let tls_acceptor = match tls_identity.make_acceptor() {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("Failed to create TLS acceptor: {e}");
                args.handler.on_status_change(ServerStatus {
                    running: false,
                    state: format!("error: {e}"),
                    uptime_secs: 0,
                });
                return;
            }
        };

        // Create display and coordinate mapper. The mapper is shared with the
        // input handler so mouse mapping stays correct after client-driven resizes.
        let fixed_resolution =
            (args.config.width > 0 && args.config.height > 0) || args.config.resolution.is_some();
        let bitrate_override = args.config.bitrate_mbps.map(|mbps| mbps * 1_000_000);
        let skip_unchanged = args
            .config
            .skip_unchanged
            .unwrap_or(args.config.idle_keyframe_sec.is_some());
        let idle_keyframe_sec = if skip_unchanged {
            Some(args.config.idle_keyframe_sec.unwrap_or(5))
        } else {
            None
        };
        let coord_mapper =
            crate::handler::MouseCoordMapper::new(args.cg_w, args.cg_h, args.width, args.height);
        let display = MacDisplay::new(
            args.width,
            args.height,
            args.sck_w,
            args.sck_h,
            fixed_resolution,
            args.config.frame_rate,
            args.quality,
            args.encoder_pref,
            args.mode_444,
            args.config.show_cursor.unwrap_or(true),
            bitrate_override,
            skip_unchanged,
            idle_keyframe_sec,
            Arc::clone(&args.gfx_state),
            coord_mapper.clone(),
        );

        let input_tap = match resolve_input_tap(args.config.input_tap.as_deref()) {
            Ok(tap) => tap,
            Err(e) => {
                tracing::error!("{e}");
                args.handler.on_status_change(ServerStatus {
                    running: false,
                    state: format!("error: {e}"),
                    uptime_secs: 0,
                });
                return;
            }
        };
        let input_handler = MacInputHandler::new_with_tap_location(coord_mapper, input_tap);

        // Build RDP server (this is the !Send type)
        let builder = match RdpServer::builder().with_listener(args.listener) {
            Ok(builder) => builder,
            Err(e) => {
                tracing::error!("Failed to use pre-bound RDP listener: {e}");
                args.handler.on_status_change(ServerStatus {
                    running: false,
                    state: format!("error: {e}"),
                    uptime_secs: 0,
                });
                return;
            }
        };

        // NLA/CredSSP is the default path: with_hybrid advertises HYBRID,
        // HYBRID_EX, and SSL, and the acceptor selects the strongest mutually
        // supported protocol (HYBRID_EX > HYBRID > SSL). TLS-only is reachable
        // only when the client does not advertise either HYBRID variant.
        let mut server = builder
            .with_hybrid(tls_acceptor, tls_identity.pub_key)
            .with_input_handler(input_handler)
            .with_display_handler(display)
            .with_cliprdr_factory(Some(Box::new(MacClipboardFactory::new())))
            .build();

        server.set_gfx_state(Arc::clone(&args.gfx_state));
        let advanced_input = args.config.advanced_input.unwrap_or(false);
        server.set_advanced_input_enabled(advanced_input);
        tracing::info!(advanced_input, "Advanced input channel configuration");

        server.set_credentials(Some(Credentials {
            username: args.credentials.username.clone(),
            password: args.credentials.password.clone(),
            domain: None,
        }));
        tracing::info!(
            "Authentication configured for user: {}",
            args.credentials.username
        );

        tracing::info!(%args.bind_addr, "RDP server listening");
        let quit_sender = server.event_sender().clone();
        let run = server.run();
        tokio::pin!(run);

        tokio::select! {
            result = &mut run => {
                if let Err(e) = result {
                    tracing::error!("RDP server error: {e}");
                    args.handler.on_status_change(ServerStatus {
                        running: false,
                        state: format!("error: {e}"),
                        uptime_secs: 0,
                    });
                }
            }
            _ = args.shutdown_notify.notified() => {
                tracing::info!("Shutdown signal received, stopping server");
                let _ = quit_sender.send(ServerEvent::Quit("shutdown requested".to_string()));
                match tokio::time::timeout(std::time::Duration::from_secs(5), &mut run).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::error!("RDP server error during shutdown: {e}");
                        args.handler.on_status_change(ServerStatus {
                            running: false,
                            state: format!("error: {e}"),
                            uptime_secs: 0,
                        });
                    }
                    Err(_) => {
                        tracing::warn!("Timed out waiting for RDP server to stop gracefully");
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_configured_credentials() {
        let config = ServerConfig {
            username: Some("admin".to_string()),
            password: Some("secret".to_string()),
            ..ServerConfig::default()
        };

        let credentials = resolve_credentials(&config).unwrap();

        assert_eq!(credentials.username, "admin");
        assert_eq!(credentials.password, "secret");
        assert!(!credentials.generated);
    }

    #[test]
    fn rejects_missing_credentials_by_default() {
        let err = resolve_credentials(&ServerConfig::default()).unwrap_err();

        assert!(err.to_string().contains("credentials are required"));
    }

    #[test]
    fn rejects_partial_or_blank_credentials() {
        let partial = ServerConfig {
            username: Some("admin".to_string()),
            password: None,
            ..ServerConfig::default()
        };
        assert!(resolve_credentials(&partial).is_err());

        let blank = ServerConfig {
            username: Some(" ".to_string()),
            password: Some(" ".to_string()),
            ..ServerConfig::default()
        };
        assert!(resolve_credentials(&blank).is_err());
    }

    #[test]
    fn generates_credentials_only_when_explicitly_allowed() {
        let config = ServerConfig {
            allow_generated_credentials: true,
            ..ServerConfig::default()
        };

        let credentials = resolve_credentials(&config).unwrap();

        assert_eq!(credentials.username, "macrdp");
        assert_eq!(credentials.password.len(), 24);
        assert!(credentials
            .password
            .chars()
            .all(|c| c.is_ascii_alphanumeric()));
        assert!(credentials.generated);
    }

    #[test]
    fn does_not_generate_when_partial_credentials_are_set() {
        // Generation kicks in only when *both* fields are None — a half-filled
        // config is an operator mistake, not an implicit opt-in to generation.
        let only_username = ServerConfig {
            username: Some("admin".to_string()),
            password: None,
            allow_generated_credentials: true,
            ..ServerConfig::default()
        };
        assert!(resolve_credentials(&only_username).is_err());

        let only_password = ServerConfig {
            username: None,
            password: Some("secret".to_string()),
            allow_generated_credentials: true,
            ..ServerConfig::default()
        };
        assert!(resolve_credentials(&only_password).is_err());
    }

    #[test]
    fn does_not_generate_when_credentials_are_blank_not_none() {
        // `Some("")`/`Some(" ")` is treated as a configured-but-invalid
        // credential and must fail rather than falling through to generation.
        let config = ServerConfig {
            username: Some("".to_string()),
            password: Some("".to_string()),
            allow_generated_credentials: true,
            ..ServerConfig::default()
        };
        assert!(resolve_credentials(&config).is_err());
    }

    #[test]
    fn generated_passwords_use_unambiguous_alphabet() {
        // The alphabet deliberately omits visually ambiguous glyphs so the
        // password printed at first launch is transcribable. Pin that.
        const FORBIDDEN: &[char] = &['0', 'O', '1', 'I', 'l'];
        for _ in 0..32 {
            let password = generate_password(64);
            for c in password.chars() {
                assert!(
                    c.is_ascii_alphanumeric(),
                    "non-alphanumeric character: {c:?}"
                );
                assert!(
                    !FORBIDDEN.contains(&c),
                    "ambiguous character {c:?} leaked into generated password"
                );
            }
        }
    }

    #[test]
    fn generated_passwords_are_distinct_across_calls() {
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        for _ in 0..16 {
            let password = generate_password(24);
            assert_eq!(password.len(), 24);
            assert!(
                seen.insert(password),
                "OsRng produced a duplicate 24-char password — CSPRNG sanity check failed"
            );
        }
    }

    #[test]
    fn generated_passwords_exercise_full_alphabet() {
        // A bug in the rejection-sampling loop (e.g. wrong modulus) could
        // collapse the output onto a small slice of the alphabet. Drawing a
        // long string should cover most of it.
        let password = generate_password(2048);
        let unique: std::collections::HashSet<char> = password.chars().collect();
        assert!(
            unique.len() >= 40,
            "expected broad alphabet coverage, got {} distinct chars",
            unique.len()
        );
    }

    #[test]
    fn bind_validation_keeps_selected_port_reserved() {
        let (listener, addr) = validate_bind_address("127.0.0.1", 0).unwrap();

        assert_ne!(addr.port(), 0);
        assert!(
            TcpListener::bind(addr).is_err(),
            "selected port should stay reserved while startup owns the listener"
        );

        drop(listener);
        let rebound = TcpListener::bind(addr).unwrap();
        drop(rebound);
    }

    #[test]
    fn bind_validation_rejects_invalid_bind_address() {
        let err = validate_bind_address("localhost", 3389).unwrap_err();

        assert!(err.to_string().contains("Invalid bind_address"));
    }

    #[test]
    fn bind_validation_dual_stack_accepts_ipv4_clients() {
        // The IPv6 unspecified address must produce a dual-stack listener so
        // mDNS/IPv4 clients can still reach the server. Without IPV6_V6ONLY
        // cleared this connect() would be refused on macOS.
        let (listener, addr) = validate_bind_address("::", 0).unwrap();
        let port = addr.port();

        let v4 = std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .expect("IPv4 client should reach dual-stack listener");
        drop(v4);
        drop(listener);
    }

    #[test]
    fn chroma_mode_defaults_to_avc420() {
        assert!(!resolve_chroma_mode(None).unwrap());
        assert!(!resolve_chroma_mode(Some("avc420")).unwrap());
    }

    #[test]
    fn chroma_mode_allows_explicit_avc444() {
        assert!(resolve_chroma_mode(Some("avc444")).unwrap());
    }

    #[test]
    fn chroma_mode_rejects_unknown_values() {
        let err = resolve_chroma_mode(Some("h264")).unwrap_err();

        assert!(err.to_string().contains("Invalid chroma_mode"));
    }

    #[test]
    fn input_tap_defaults_to_session() {
        assert_eq!(
            resolve_input_tap(None).unwrap(),
            macrdp_input::InputTapLocation::Session
        );
        assert_eq!(
            resolve_input_tap(Some("session")).unwrap(),
            macrdp_input::InputTapLocation::Session
        );
    }

    #[test]
    fn input_tap_allows_explicit_hid_and_annotated_session() {
        assert_eq!(
            resolve_input_tap(Some("hid")).unwrap(),
            macrdp_input::InputTapLocation::Hid
        );
        assert_eq!(
            resolve_input_tap(Some("annotated_session")).unwrap(),
            macrdp_input::InputTapLocation::AnnotatedSession
        );
    }

    #[test]
    fn input_tap_rejects_unknown_values() {
        let err = resolve_input_tap(Some("device")).unwrap_err();

        assert!(err.to_string().contains("Invalid input_tap"));
    }
}

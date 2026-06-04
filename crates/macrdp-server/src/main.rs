use anyhow::{Context, Result};
use clap::Parser;
use macrdp_core::{
    default_log_path, format_report, permission_report, Metrics, ReportFormat, ServerEventHandler,
    ServerStatus,
};
use macrdp_server::config::{load_config, Cli};
use std::fs::OpenOptions;
use std::io::IsTerminal;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let cli = Cli::parse();

    if cli.check_permissions {
        return Ok(run_permission_check(&cli.check_permissions_format));
    }

    if cli.keychain_set_password {
        return run_keychain_set_password(&cli);
    }

    let config = load_config(&cli)?;

    // Initialize logging — write to both stderr and file
    let log_level = config.log_level.as_deref().unwrap_or("info");
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level));

    let log_path = config.log_path.clone().unwrap_or_else(default_log_path);
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create log directory {}", parent.display()))?;
    }
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("Failed to open log file {}", log_path.display()))?;

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let json_format = matches!(config.log_format.as_deref(), Some("json"));
    let stderr_is_terminal = std::io::stderr().is_terminal();
    let registry = tracing_subscriber::registry().with(env_filter);

    if json_format {
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_current_span(false)
                    .with_span_list(false)
                    .with_writer(std::io::stderr),
            )
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_current_span(false)
                    .with_span_list(false)
                    .with_writer(std::sync::Mutex::new(log_file)),
            )
            .init();
    } else {
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(stderr_is_terminal)
                    .with_writer(std::io::stderr),
            )
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(std::sync::Mutex::new(log_file)),
            )
            .init();
    }

    tracing::info!(
        log_path = %log_path.display(),
        log_format = if json_format { "json" } else { "text" },
        "Logging initialized"
    );

    tracing::info!(
        bind_address = %config.bind_address,
        port = config.port,
        frame_rate = config.frame_rate,
        "macrdp server starting"
    );
    tracing::info!("Connect using an RDP client (e.g., Windows mstsc or Microsoft Remote Desktop)");
    tracing::info!("Single-client mode: one active RDP session is supported; concurrent sessions are unsupported");

    let handle = macrdp_core::start_server(config, CliEventHandler)
        .await
        .context("Failed to start macrdp runtime")?;

    if let Some(credentials) = handle.generated_credentials() {
        if std::io::stdout().is_terminal() {
            println!("Generated temporary RDP credentials:");
            println!("  Username: {}", credentials.username);
            println!("  Password: {}", credentials.password);
        } else {
            tracing::warn!(
                user = %credentials.username,
                "Generated temporary credentials are active; password not printed because stdout is noninteractive"
            );
        }
    }

    tracing::info!(port = handle.port(), "macrdp runtime started");

    run_until_shutdown(handle).await?;
    Ok(ExitCode::SUCCESS)
}

/// Print Screen Recording / Accessibility permission status to stdout and
/// return an exit code. Side-effect-free: never opens System Settings, never
/// triggers TCC prompts, never starts the server. Safe to invoke under
/// launchd or from automation scripts.
fn run_permission_check(format: &str) -> ExitCode {
    let report = permission_report();
    let format = match format {
        "json" => ReportFormat::Json,
        _ => ReportFormat::Text,
    };
    let rendered = format_report(&report, format);
    if matches!(format, ReportFormat::Json) {
        println!("{rendered}");
    } else {
        print!("{rendered}");
    }
    if report.all_granted {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn run_keychain_set_password(cli: &Cli) -> Result<ExitCode> {
    let username = cli
        .username
        .as_deref()
        .map(str::to_owned)
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "default".to_string());
    let password = rpassword::prompt_password(format!("RDP password for '{username}': "))
        .context("Failed to read password")?;
    macrdp_server::keychain::set_password(Some(&username), &password)
        .context("Failed to store password in keychain")?;
    println!("Password stored in keychain (service=macrdp, account={username})");
    Ok(ExitCode::SUCCESS)
}

/// Wait for the first SIGINT/SIGTERM, then drive `handle.stop()` to completion.
/// If a second signal arrives before stop finishes, force-exit so launchd's
/// SIGKILL escalation isn't needed.
async fn run_until_shutdown(handle: std::sync::Arc<macrdp_core::ServerHandle>) -> Result<()> {
    let mut signals = ShutdownSignals::install()?;

    signals.recv().await;
    tracing::info!("Shutdown requested, stopping macrdp runtime");

    tokio::select! {
        stop_result = handle.stop() => {
            stop_result?;
            tracing::info!("macrdp runtime stopped");
            Ok(())
        }
        _ = signals.recv() => {
            tracing::warn!("Second shutdown signal received; forcing immediate exit");
            std::process::exit(130);
        }
    }
}

/// Cross-platform shutdown signal stream. On Unix, multiplexes SIGINT and
/// SIGTERM; on other platforms, falls back to Ctrl-C.
struct ShutdownSignals {
    #[cfg(unix)]
    sigint: tokio::signal::unix::Signal,
    #[cfg(unix)]
    sigterm: tokio::signal::unix::Signal,
}

impl ShutdownSignals {
    #[cfg(unix)]
    fn install() -> Result<Self> {
        use tokio::signal::unix::{signal, SignalKind};
        let sigint = signal(SignalKind::interrupt()).context("Failed to install SIGINT handler")?;
        let sigterm =
            signal(SignalKind::terminate()).context("Failed to install SIGTERM handler")?;
        Ok(Self { sigint, sigterm })
    }

    #[cfg(not(unix))]
    fn install() -> Result<Self> {
        Ok(Self {})
    }

    #[cfg(unix)]
    async fn recv(&mut self) {
        tokio::select! {
            _ = self.sigint.recv() => tracing::info!("SIGINT received"),
            _ = self.sigterm.recv() => tracing::info!("SIGTERM received"),
        }
    }

    #[cfg(not(unix))]
    async fn recv(&mut self) {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("Ctrl-C received");
    }
}

struct CliEventHandler;

impl ServerEventHandler for CliEventHandler {
    fn on_status_change(&self, status: ServerStatus) {
        tracing::info!(
            running = status.running,
            state = %status.state,
            uptime_secs = status.uptime_secs,
            "Server status changed"
        );
    }

    fn on_metrics(&self, metrics: Metrics) {
        tracing::debug!(
            fps = metrics.fps,
            bitrate_kbps = metrics.bitrate_kbps,
            rtt_ms = metrics.rtt_ms,
            encode_ms = metrics.encode_ms,
            pending_acks = metrics.pending_acks,
            "Server metrics"
        );
    }
}

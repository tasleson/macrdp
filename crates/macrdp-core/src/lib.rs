//! macrdp-core: unified RDP server library

pub mod bitrate_controller;
pub mod callbacks;
pub mod config;
pub mod display;
pub mod features;
pub mod handler;
pub mod permissions;
pub mod server;
pub mod tls;

pub use callbacks::*;
pub use config::{
    config_dir, default_cert_path, default_config_path, default_key_path, default_log_path,
    default_tls_dir, AudioConfig, ClipboardConfig, ServerConfig,
};
pub use permissions::{
    check_permissions, format_report, permission_report, PermissionEntry, PermissionReport,
    ReportFormat,
};
pub use server::{
    resolve_resolution, start_server, start_server_with_options, ServerHandle, ServerStartupOptions,
};
pub use tls::{ensure_tls_files, generate_self_signed_cert};

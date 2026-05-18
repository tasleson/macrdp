//! macrdp-core: unified RDP server library

pub mod bitrate_controller;
pub mod callbacks;
pub mod config;
pub mod display;
pub mod handler;
pub mod log_bridge;
pub mod permissions;
pub mod perf_stats;
pub mod server;
pub mod tls;

pub use callbacks::*;
pub use config::{config_dir, AudioConfig, ClipboardConfig, ServerConfig};
pub use log_bridge::{LogBridgeLayer, init_log_file, log_file_path};
pub use server::{start_server, resolve_resolution, ServerHandle};

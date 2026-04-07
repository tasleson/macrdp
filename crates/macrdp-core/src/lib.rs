//! macrdp-core: unified RDP server library

pub mod bitrate_controller;
pub mod callbacks;
pub mod config;
pub mod display;
pub mod handler;
pub mod permissions;
pub mod server;
pub mod tls;

pub use callbacks::*;
pub use config::{config_dir, AudioConfig, ClipboardConfig, ServerConfig};
pub use server::{start_server, ServerHandle};

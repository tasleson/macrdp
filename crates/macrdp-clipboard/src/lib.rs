pub mod pasteboard;
pub mod formats;
pub mod file;
pub mod html;

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use ironrdp_cliprdr::backend::{CliprdrBackend, CliprdrBackendFactory, ClipboardMessage};
use ironrdp_cliprdr::pdu::*;
use ironrdp_core::{impl_as_any, IntoOwned};
use ironrdp_server::{CliprdrServerFactory, ServerEvent, ServerEventSender};
use tokio::sync::mpsc;

use crate::formats::{uti_to_rdp_format_id, png_to_dib, dib_to_png};
use crate::pasteboard::PasteboardBridge;

/// macOS clipboard backend implementing the CLIPRDR protocol.
///
/// Bridges between NSPasteboard and RDP CLIPRDR channel with:
/// - A polling thread that checks `NSPasteboard.changeCount` every 500ms
/// - Anti-echo mechanism to prevent clipboard feedback loops
/// - Text clipboard support (CF_UNICODETEXT <-> NSPasteboard text)
pub struct MacClipboardBackend {
    event_sender: mpsc::UnboundedSender<ServerEvent>,
    last_change_count: Arc<AtomicI64>,
    remote_formats: Vec<ClipboardFormat>,
    file_handles: HashMap<u32, File>,
    temp_dir: PathBuf,
    poll_handle: Option<JoinHandle<()>>,
    stop_signal: Arc<AtomicBool>,
    locked: bool,
}

impl fmt::Debug for MacClipboardBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacClipboardBackend")
            .field("temp_dir", &self.temp_dir)
            .finish_non_exhaustive()
    }
}

impl_as_any!(MacClipboardBackend);

impl MacClipboardBackend {
    pub fn new(event_sender: mpsc::UnboundedSender<ServerEvent>, temp_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&temp_dir).ok();
        Self {
            event_sender,
            last_change_count: Arc::new(AtomicI64::new(0)),
            remote_formats: Vec::new(),
            file_handles: HashMap::new(),
            temp_dir,
            poll_handle: None,
            stop_signal: Arc::new(AtomicBool::new(false)),
            locked: false,
        }
    }

    /// Build a format list from the current pasteboard contents.
    fn build_format_list() -> Vec<ClipboardFormat> {
        let pb = PasteboardBridge::new();
        pb.available_types()
            .iter()
            .filter_map(|uti| {
                let fmt_id = uti_to_rdp_format_id(uti)?;
                Some(ClipboardFormat::new(ClipboardFormatId::new(fmt_id)))
            })
            .collect()
    }
}

impl CliprdrBackend for MacClipboardBackend {
    fn temporary_directory(&self) -> &str {
        self.temp_dir.to_str().unwrap_or("/tmp/macrdp-clipboard")
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
    }

    fn on_ready(&mut self) {
        // Initialize change count and start polling thread
        let pb = PasteboardBridge::new();
        let initial = pb.change_count();
        self.last_change_count.store(initial, Ordering::SeqCst);

        let last = self.last_change_count.clone();
        let sender = self.event_sender.clone();
        let stop = self.stop_signal.clone();

        self.poll_handle = Some(std::thread::spawn(move || {
            let bridge = PasteboardBridge::new();
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
                if stop.load(Ordering::Relaxed) {
                    break;
                }

                let current = bridge.change_count();
                let prev = last.swap(current, Ordering::SeqCst);
                if current != prev {
                    // Clipboard changed -- build format list and notify server
                    let formats: Vec<ClipboardFormat> = bridge
                        .available_types()
                        .iter()
                        .filter_map(|uti| {
                            let fmt_id = uti_to_rdp_format_id(uti)?;
                            Some(ClipboardFormat::new(ClipboardFormatId::new(fmt_id)))
                        })
                        .collect();
                    if !formats.is_empty() {
                        let _ = sender.send(ServerEvent::Clipboard(
                            ClipboardMessage::SendInitiateCopy(formats),
                        ));
                    }
                }
            }
        }));
        tracing::info!("Clipboard polling started");
    }

    fn on_process_negotiated_capabilities(&mut self, _caps: ClipboardGeneralCapabilityFlags) {}

    fn on_request_format_list(&mut self) {
        let formats = Self::build_format_list();
        let _ = self.event_sender.send(ServerEvent::Clipboard(
            ClipboardMessage::SendInitiateCopy(formats),
        ));
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        self.remote_formats = available_formats.to_vec();
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let pb = PasteboardBridge::new();
        let format_id = request.format.value();

        let response = match format_id {
            13 => {
                // CF_UNICODETEXT -- use ironrdp's built-in unicode string helper
                match pb.read_string() {
                    Some(text) => FormatDataResponse::new_unicode_string(&text),
                    None => FormatDataResponse::new_error(),
                }
            }
            8 => {
                // CF_DIB — convert macOS image to Windows DIB
                match pb.read_image() {
                    Some(png_data) => match png_to_dib(&png_data) {
                        Ok(dib) => FormatDataResponse::new_data(dib),
                        Err(e) => {
                            tracing::warn!("Failed to convert image to DIB: {e}");
                            FormatDataResponse::new_error()
                        }
                    },
                    None => FormatDataResponse::new_error(),
                }
            }
            _ => {
                tracing::warn!(format_id, "Unsupported clipboard format requested");
                FormatDataResponse::new_error()
            }
        };

        // Convert to owned and send
        let owned: OwnedFormatDataResponse = response.into_owned();
        let _ = self.event_sender.send(ServerEvent::Clipboard(
            ClipboardMessage::SendFormatData(owned),
        ));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            tracing::warn!("Received error format data response from client");
            return;
        }

        // Try to decode as unicode text
        if let Ok(text) = response.to_unicode_string() {
            let pb = PasteboardBridge::new();
            let new_count = pb.write_string(&text);
            // Anti-echo: update change count so polling thread skips this change
            self.last_change_count.store(new_count, Ordering::SeqCst);
            return;
        }

        // Try to decode as DIB image
        let data = response.data();
        if data.len() >= formats::BITMAPINFOHEADER_SIZE {
            let header_size = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            if header_size == 40 {
                if let Ok(png_data) = dib_to_png(data) {
                    let pb = PasteboardBridge::new();
                    let new_count = pb.write_image(&png_data);
                    self.last_change_count.store(new_count, Ordering::SeqCst);
                    return;
                }
            }
        }

        tracing::debug!(
            "Unhandled format data response ({} bytes)",
            response.data().len()
        );
    }

    fn on_file_contents_request(&mut self, _request: FileContentsRequest) {
        // Implemented in Task 11
    }

    fn on_file_contents_response(&mut self, _response: FileContentsResponse<'_>) {
        // Implemented in Task 11
    }

    fn on_lock(&mut self, _data_id: LockDataId) {
        self.locked = true;
    }

    fn on_unlock(&mut self, _data_id: LockDataId) {
        self.locked = false;
        self.file_handles.clear();
    }
}

impl Drop for MacClipboardBackend {
    fn drop(&mut self) {
        self.stop_signal.store(true, Ordering::SeqCst);
        if let Some(handle) = self.poll_handle.take() {
            let _ = handle.join();
        }
    }
}

// --- Factory ---

/// Factory for creating `MacClipboardBackend` instances.
///
/// Implements `CliprdrServerFactory` which combines `CliprdrBackendFactory`
/// and `ServerEventSender`. The event sender is injected by the server
/// before `build_cliprdr_backend` is called.
pub struct MacClipboardFactory {
    event_sender: Option<mpsc::UnboundedSender<ServerEvent>>,
    temp_dir: PathBuf,
}

impl MacClipboardFactory {
    pub fn new(temp_dir: PathBuf) -> Self {
        Self {
            event_sender: None,
            temp_dir,
        }
    }
}

impl CliprdrBackendFactory for MacClipboardFactory {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        Box::new(MacClipboardBackend::new(
            self.event_sender
                .clone()
                .expect("event sender not set before building clipboard backend"),
            self.temp_dir.clone(),
        ))
    }
}

impl ServerEventSender for MacClipboardFactory {
    fn set_sender(&mut self, sender: mpsc::UnboundedSender<ServerEvent>) {
        self.event_sender = Some(sender);
    }
}

impl CliprdrServerFactory for MacClipboardFactory {}

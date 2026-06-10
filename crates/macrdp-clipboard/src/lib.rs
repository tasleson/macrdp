pub mod file;
pub mod formats;
pub mod html;
pub mod pasteboard;
pub mod transfer;

use std::fmt;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use ironrdp_cliprdr::backend::{ClipboardMessage, CliprdrBackend, CliprdrBackendFactory};
use ironrdp_cliprdr::pdu::*;
use ironrdp_core::{impl_as_any, IntoOwned};
use ironrdp_server::{CliprdrServerFactory, ServerEvent, ServerEventSender};
use objc2::rc::autoreleasepool;
use tokio::sync::mpsc;

use crate::formats::{
    dib_to_png, png_to_dib, uti_to_rdp_format_id, BITMAPINFOHEADER_SIZE, FORMAT_ID_FILE_LIST,
    FORMAT_ID_HTML,
};
use crate::html::{unwrap_html_format, wrap_html_format};
use crate::pasteboard::PasteboardBridge;
use crate::transfer::FileTransferManager;

/// How often the worker thread checks `NSPasteboard.changeCount` for local edits.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// CF_UNICODETEXT registered format id.
const FORMAT_ID_UNICODE_TEXT: u32 = 13;
/// CF_DIB registered format id.
const FORMAT_ID_DIB: u32 = 8;

/// macOS clipboard backend implementing the CLIPRDR protocol.
///
/// All `NSPasteboard` access happens on a single dedicated worker thread (see
/// [`PasteboardWorker`]). The CLIPRDR callbacks below run on the RDP server's
/// event-loop thread, so they must never block: each one simply hands a
/// [`Command`] to the worker and returns immediately. This keeps the video and
/// input pipeline responsive and confines all AppKit calls (which are not
/// thread-safe) to one thread.
pub struct MacClipboardBackend {
    event_sender: mpsc::UnboundedSender<ServerEvent>,
    temp_dir: PathBuf,
    max_file_size: u64,
    /// Channel to the pasteboard worker. `None` until `on_ready` starts it.
    cmd_tx: Option<Sender<Command>>,
    worker: Option<JoinHandle<()>>,
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
    pub fn new(
        event_sender: mpsc::UnboundedSender<ServerEvent>,
        temp_dir: PathBuf,
        max_file_size: u64,
    ) -> Self {
        std::fs::create_dir_all(&temp_dir).ok();
        Self {
            event_sender,
            temp_dir,
            max_file_size,
            cmd_tx: None,
            worker: None,
        }
    }

    /// Enqueue a command for the worker thread, dropping it if the worker is
    /// gone (e.g. during shutdown).
    fn dispatch(&self, cmd: Command) {
        let Some(tx) = self.cmd_tx.as_ref() else {
            tracing::debug!("Clipboard command dropped before worker thread started");
            return;
        };
        if tx.send(cmd).is_err() {
            tracing::debug!("Clipboard command dropped because worker thread has exited");
        }
    }
}

impl CliprdrBackend for MacClipboardBackend {
    fn temporary_directory(&self) -> &str {
        self.temp_dir.to_str().unwrap_or("/tmp/macrdp-clipboard")
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
            | ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED
            | ClipboardGeneralCapabilityFlags::FILECLIP_NO_FILE_PATHS
            | ClipboardGeneralCapabilityFlags::CAN_LOCK_CLIPDATA
    }

    fn on_ready(&mut self) {
        if self.worker.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = PasteboardWorker::new(
            self.event_sender.clone(),
            self.temp_dir.clone(),
            self.max_file_size,
        );
        let handle = std::thread::Builder::new()
            .name("clipboard-pasteboard".into())
            .spawn(move || worker.run(rx))
            .expect("failed to spawn clipboard worker thread");
        self.cmd_tx = Some(tx);
        self.worker = Some(handle);
        tracing::info!("Clipboard worker started");
    }

    fn on_process_negotiated_capabilities(&mut self, _caps: ClipboardGeneralCapabilityFlags) {}

    fn on_request_format_list(&mut self) {
        self.dispatch(Command::AdvertiseFormats);
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        self.dispatch(Command::RemoteCopy(available_formats.to_vec()));
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        self.dispatch(Command::FormatDataRequest(request.format.value()));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            tracing::warn!("Received error format data response from client");
            return;
        }
        self.dispatch(Command::FormatDataResponse(response.into_owned()));
    }

    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        self.dispatch(Command::FileContentsRequest(request));
    }

    fn on_file_contents_response(&mut self, response: FileContentsResponse<'_>) {
        self.dispatch(Command::FileContentsResponse(response.into_owned()));
    }

    fn on_lock(&mut self, _data_id: LockDataId) {
        self.dispatch(Command::Lock);
    }

    fn on_unlock(&mut self, _data_id: LockDataId) {
        self.dispatch(Command::Unlock);
    }
}

impl Drop for MacClipboardBackend {
    fn drop(&mut self) {
        // Dropping the sender disconnects the channel, so the worker's
        // `recv_timeout` returns `Disconnected` and it exits its loop.
        self.cmd_tx.take();
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

/// A unit of pasteboard work handed from a CLIPRDR callback to the worker thread.
enum Command {
    /// Advertise the current local clipboard formats to the client.
    AdvertiseFormats,
    /// The client advertised its clipboard; pick the best format and request it.
    RemoteCopy(Vec<ClipboardFormat>),
    /// The client wants the data for a format we advertised (by format id).
    FormatDataRequest(u32),
    /// The client sent data for a format we requested.
    FormatDataResponse(OwnedFormatDataResponse),
    /// The client wants the contents of a local file we advertised.
    FileContentsRequest(FileContentsRequest),
    /// The client sent contents for a file we requested.
    FileContentsResponse(OwnedFileContentsResponse),
    Lock,
    Unlock,
}

/// Owns every piece of clipboard state that touches `NSPasteboard` or the
/// in-progress file transfer, and runs on its own thread. Because it is the
/// sole accessor of the pasteboard, all AppKit calls are serialized and each is
/// wrapped in an autorelease pool to bound temporary allocations.
struct PasteboardWorker {
    bridge: PasteboardBridge,
    transfer: FileTransferManager,
    event_sender: mpsc::UnboundedSender<ServerEvent>,
    /// Change count of the last edit we know about, used to suppress echoing
    /// our own writes back to the client as local-clipboard changes.
    last_change_count: i64,
    /// Format id requested via the most recent `RemoteCopy`, awaiting data.
    pending_paste_format: Option<u32>,
}

impl PasteboardWorker {
    fn new(
        event_sender: mpsc::UnboundedSender<ServerEvent>,
        temp_dir: PathBuf,
        max_file_size: u64,
    ) -> Self {
        Self {
            bridge: PasteboardBridge::new(),
            transfer: FileTransferManager::new(temp_dir, max_file_size),
            event_sender,
            last_change_count: 0,
            pending_paste_format: None,
        }
    }

    fn run(mut self, cmd_rx: Receiver<Command>) {
        self.last_change_count = autoreleasepool(|_| self.bridge.change_count());
        loop {
            match cmd_rx.recv_timeout(POLL_INTERVAL) {
                Ok(cmd) => autoreleasepool(|_| self.handle(cmd)),
                Err(RecvTimeoutError::Timeout) => autoreleasepool(|_| self.poll_local_changes()),
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    fn send(&self, message: ClipboardMessage) {
        if self
            .event_sender
            .send(ServerEvent::Clipboard(message))
            .is_err()
        {
            tracing::debug!("Clipboard event dropped because server event loop is closed");
        }
    }

    /// Detect a local clipboard edit and advertise the new formats to the client.
    fn poll_local_changes(&mut self) {
        let current = self.bridge.change_count();
        if current == self.last_change_count {
            return;
        }
        self.last_change_count = current;
        let formats = build_format_list(&self.bridge);
        if !formats.is_empty() {
            self.send(ClipboardMessage::SendInitiateCopy(formats));
        }
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::AdvertiseFormats => {
                let formats = build_format_list(&self.bridge);
                self.send(ClipboardMessage::SendInitiateCopy(formats));
            }
            Command::RemoteCopy(formats) => {
                // Request the highest-fidelity format available, in order of preference.
                let preferred = [
                    FORMAT_ID_FILE_LIST,
                    FORMAT_ID_HTML,
                    FORMAT_ID_DIB,
                    FORMAT_ID_UNICODE_TEXT,
                ];
                for &fmt_id in &preferred {
                    if formats.iter().any(|f| f.id().value() == fmt_id) {
                        self.pending_paste_format = Some(fmt_id);
                        self.send(ClipboardMessage::SendInitiatePaste(ClipboardFormatId::new(
                            fmt_id,
                        )));
                        return;
                    }
                }
            }
            Command::FormatDataRequest(format_id) => {
                let response = self.read_local_format(format_id);
                self.send(ClipboardMessage::SendFormatData(response));
            }
            Command::FormatDataResponse(response) => self.write_remote_format(response),
            Command::FileContentsRequest(request) => {
                let response = self.transfer.handle_contents_request(&request);
                let _ = self
                    .event_sender
                    .send(ServerEvent::ClipboardFileContents(response.into_owned()));
            }
            Command::FileContentsResponse(response) => {
                self.transfer.handle_contents_response(&response);
                if self.transfer.all_incoming_complete() {
                    let paths = self.transfer.flush_incoming_files();
                    if !paths.is_empty() {
                        self.bridge.write_file_urls(&paths);
                        self.last_change_count = self.bridge.change_count();
                        tracing::info!(count = paths.len(), "Wrote incoming files to pasteboard");
                    }
                }
            }
            Command::Lock => self.transfer.lock(),
            Command::Unlock => self.transfer.unlock(),
        }
    }

    /// Read a format the client requested from the local pasteboard.
    fn read_local_format(&mut self, format_id: u32) -> OwnedFormatDataResponse {
        match format_id {
            FORMAT_ID_UNICODE_TEXT => match self.bridge.read_string() {
                Some(text) => FormatDataResponse::new_unicode_string(&text).into_owned(),
                None => FormatDataResponse::new_error(),
            },
            FORMAT_ID_DIB => match self.bridge.read_image() {
                Some(png_data) => match png_to_dib(&png_data) {
                    Ok(dib) => FormatDataResponse::new_data(dib).into_owned(),
                    Err(e) => {
                        tracing::warn!("Failed to convert image to DIB: {e}");
                        FormatDataResponse::new_error()
                    }
                },
                None => FormatDataResponse::new_error(),
            },
            FORMAT_ID_HTML => match self.bridge.read_html() {
                Some(html_content) => {
                    FormatDataResponse::new_data(wrap_html_format(&html_content)).into_owned()
                }
                None => FormatDataResponse::new_error(),
            },
            FORMAT_ID_FILE_LIST => {
                let paths = self.bridge.read_file_urls();
                self.transfer.set_local_files(paths);
                let file_list = self.transfer.build_file_list();
                match FormatDataResponse::new_file_list(&file_list) {
                    Ok(resp) => resp.into_owned(),
                    Err(e) => {
                        tracing::warn!("Failed to encode file list: {e}");
                        FormatDataResponse::new_error()
                    }
                }
            }
            _ => {
                tracing::warn!(format_id, "Unsupported clipboard format requested");
                FormatDataResponse::new_error()
            }
        }
    }

    /// Write data the client sent us to the local pasteboard.
    fn write_remote_format(&mut self, response: OwnedFormatDataResponse) {
        match self.pending_paste_format.take() {
            Some(FORMAT_ID_UNICODE_TEXT) => self.write_text(&response),
            Some(FORMAT_ID_DIB) => self.write_dib(response.data()),
            Some(FORMAT_ID_HTML) => {
                if let Some(html) = unwrap_html_format(response.data()) {
                    self.bridge.write_html(&html);
                    self.last_change_count = self.bridge.change_count();
                }
            }
            Some(FORMAT_ID_FILE_LIST) => {
                if let Ok(file_list) = response.to_file_list() {
                    self.transfer.set_incoming_descriptors(file_list.files);
                    for req in self.transfer.generate_contents_requests() {
                        let _ = self
                            .event_sender
                            .send(ServerEvent::ClipboardFileContentsRequest(req));
                    }
                }
            }
            // No pending format (or an unexpected one): fall back to sniffing
            // the payload so a bare text/image paste still works.
            _ => {
                if response.to_unicode_string().is_ok() {
                    self.write_text(&response);
                    return;
                }
                let data = response.data();
                if data.len() >= BITMAPINFOHEADER_SIZE {
                    let header_size = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                    if header_size == BITMAPINFOHEADER_SIZE as u32 {
                        self.write_dib(data);
                    }
                }
            }
        }
    }

    fn write_text(&mut self, response: &OwnedFormatDataResponse) {
        if let Ok(text) = response.to_unicode_string() {
            self.bridge.write_string(&text);
            self.last_change_count = self.bridge.change_count();
        }
    }

    fn write_dib(&mut self, data: &[u8]) {
        if data.len() < BITMAPINFOHEADER_SIZE {
            return;
        }
        match dib_to_png(data) {
            Ok(png_data) => {
                self.bridge.write_image(&png_data);
                self.last_change_count = self.bridge.change_count();
            }
            Err(e) => tracing::warn!("Failed to convert DIB to image: {e}"),
        }
    }
}

/// Build a CLIPRDR format list from the current pasteboard contents.
fn build_format_list(bridge: &PasteboardBridge) -> Vec<ClipboardFormat> {
    bridge
        .available_types()
        .iter()
        .filter_map(|uti| {
            let fmt_id = uti_to_rdp_format_id(uti)?;
            let mut fmt = ClipboardFormat::new(ClipboardFormatId::new(fmt_id));
            match fmt_id {
                FORMAT_ID_HTML => fmt = fmt.with_name(ClipboardFormatName::HTML),
                FORMAT_ID_FILE_LIST => fmt = fmt.with_name(ClipboardFormatName::FILE_LIST),
                _ => {}
            }
            Some(fmt)
        })
        .collect()
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
    max_file_size: u64,
}

impl MacClipboardFactory {
    pub fn new(temp_dir: PathBuf, max_file_size_mb: u32) -> Self {
        Self {
            event_sender: None,
            temp_dir,
            max_file_size: max_file_size_mb as u64 * 1024 * 1024,
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
            self.max_file_size,
        ))
    }
}

impl ServerEventSender for MacClipboardFactory {
    fn set_sender(&mut self, sender: mpsc::UnboundedSender<ServerEvent>) {
        self.event_sender = Some(sender);
    }
}

impl CliprdrServerFactory for MacClipboardFactory {}

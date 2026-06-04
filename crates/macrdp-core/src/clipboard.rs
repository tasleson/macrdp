use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::JoinHandle;
use std::time::Duration;

use ironrdp_cliprdr::backend::{ClipboardMessage, CliprdrBackend, CliprdrBackendFactory};
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest,
    FileContentsResponse, FormatDataRequest, FormatDataResponse, LockDataId,
};
use ironrdp_core::{AsAny, IntoOwned};
use ironrdp_server::{CliprdrServerFactory, ServerEvent, ServerEventSender};
use tokio::sync::mpsc;

const CLIPBOARD_POLL_INTERVAL: Duration = Duration::from_millis(750);

#[derive(Debug, Default)]
struct ClipboardState {
    last_text: Option<String>,
}

#[derive(Debug, Default)]
pub struct MacClipboardFactory {
    sender: Arc<Mutex<Option<mpsc::UnboundedSender<ServerEvent>>>>,
}

impl MacClipboardFactory {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ServerEventSender for MacClipboardFactory {
    fn set_sender(&mut self, sender: mpsc::UnboundedSender<ServerEvent>) {
        *self.sender.lock().unwrap() = Some(sender);
    }
}

impl CliprdrBackendFactory for MacClipboardFactory {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        Box::new(MacClipboardBackend::new(Arc::clone(&self.sender)))
    }
}

impl CliprdrServerFactory for MacClipboardFactory {}

#[derive(Debug)]
struct MacClipboardBackend {
    sender: Arc<Mutex<Option<mpsc::UnboundedSender<ServerEvent>>>>,
    state: Arc<Mutex<ClipboardState>>,
    stop_polling: Arc<AtomicBool>,
    poll_thread: Option<JoinHandle<()>>,
}

impl MacClipboardBackend {
    fn new(sender: Arc<Mutex<Option<mpsc::UnboundedSender<ServerEvent>>>>) -> Self {
        Self {
            sender,
            state: Arc::new(Mutex::new(ClipboardState::default())),
            stop_polling: Arc::new(AtomicBool::new(false)),
            poll_thread: None,
        }
    }

    fn send_clipboard_message(
        sender: &Arc<Mutex<Option<mpsc::UnboundedSender<ServerEvent>>>>,
        message: ClipboardMessage,
    ) {
        let Some(sender) = sender.lock().unwrap().clone() else {
            tracing::debug!("Clipboard event dropped before server event sender was installed");
            return;
        };

        if sender.send(ServerEvent::Clipboard(message)).is_err() {
            tracing::debug!("Clipboard event dropped because server event loop is closed");
        }
    }

    fn advertise_local_formats(&self) {
        let formats = match read_pasteboard_text() {
            Ok(Some(text)) => {
                self.state.lock().unwrap().last_text = Some(text);
                vec![unicode_text_format()]
            }
            Ok(None) => {
                self.state.lock().unwrap().last_text = None;
                Vec::new()
            }
            Err(error) => {
                tracing::debug!(%error, "Failed to read local pasteboard for clipboard format list");
                return;
            }
        };

        Self::send_clipboard_message(&self.sender, ClipboardMessage::SendInitiateCopy(formats));
    }

    fn start_polling(&mut self) {
        if self.poll_thread.is_some() {
            return;
        }

        let sender = Arc::clone(&self.sender);
        let state = Arc::clone(&self.state);
        let stop = Arc::clone(&self.stop_polling);
        self.poll_thread = Some(std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match read_pasteboard_text() {
                    Ok(text) => {
                        let changed = {
                            let mut state = state.lock().unwrap();
                            if state.last_text == text {
                                false
                            } else {
                                state.last_text = text.clone();
                                true
                            }
                        };

                        if changed {
                            let formats = if text.is_some() {
                                vec![unicode_text_format()]
                            } else {
                                Vec::new()
                            };
                            MacClipboardBackend::send_clipboard_message(
                                &sender,
                                ClipboardMessage::SendInitiateCopy(formats),
                            );
                        }
                    }
                    Err(error) => {
                        tracing::debug!(%error, "Failed to poll local pasteboard");
                    }
                }

                std::thread::sleep(CLIPBOARD_POLL_INTERVAL);
            }
        }));
    }
}

impl Drop for MacClipboardBackend {
    fn drop(&mut self) {
        self.stop_polling.store(true, Ordering::Relaxed);
        if let Some(thread) = self.poll_thread.take() {
            let _ = thread.join();
        }
    }
}

impl AsAny for MacClipboardBackend {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl CliprdrBackend for MacClipboardBackend {
    fn temporary_directory(&self) -> &str {
        "/tmp"
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::empty()
    }

    fn on_ready(&mut self) {
        self.start_polling();
        self.advertise_local_formats();
    }

    fn on_request_format_list(&mut self) {
        self.advertise_local_formats();
    }

    fn on_process_negotiated_capabilities(
        &mut self,
        _capabilities: ClipboardGeneralCapabilityFlags,
    ) {
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        if available_formats
            .iter()
            .any(|format| format.id() == ClipboardFormatId::CF_UNICODETEXT)
        {
            Self::send_clipboard_message(
                &self.sender,
                ClipboardMessage::SendInitiatePaste(ClipboardFormatId::CF_UNICODETEXT),
            );
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let response = if request.format == ClipboardFormatId::CF_UNICODETEXT {
            match read_pasteboard_text() {
                Ok(Some(text)) => FormatDataResponse::new_data(encode_unicode_text(&text)),
                Ok(None) => FormatDataResponse::new_error(),
                Err(error) => {
                    tracing::debug!(%error, "Failed to read local pasteboard for clipboard data request");
                    FormatDataResponse::new_error()
                }
            }
        } else {
            FormatDataResponse::new_error()
        };

        Self::send_clipboard_message(
            &self.sender,
            ClipboardMessage::SendFormatData(response.into_owned()),
        );
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            tracing::debug!("Remote clipboard data response reported an error");
            return;
        }

        match decode_unicode_text(response.data()) {
            Ok(text) => {
                if let Err(error) = write_pasteboard_text(&text) {
                    tracing::debug!(%error, "Failed to write remote clipboard text to local pasteboard");
                    return;
                }
                self.state.lock().unwrap().last_text = Some(text);
            }
            Err(error) => {
                tracing::debug!(%error, "Failed to decode remote clipboard text");
            }
        }
    }

    fn on_file_contents_request(&mut self, _request: FileContentsRequest) {}

    fn on_file_contents_response(&mut self, _response: FileContentsResponse<'_>) {}

    fn on_lock(&mut self, _data_id: LockDataId) {}

    fn on_unlock(&mut self, _data_id: LockDataId) {}
}

fn unicode_text_format() -> ClipboardFormat {
    ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)
}

fn read_pasteboard_text() -> anyhow::Result<Option<String>> {
    let output = Command::new("/usr/bin/pbpaste").output()?;
    if !output.status.success() || output.stdout.is_empty() {
        return Ok(None);
    }

    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok((!text.is_empty()).then_some(text))
}

fn write_pasteboard_text(text: &str) -> anyhow::Result<()> {
    let mut child = Command::new("/usr/bin/pbcopy")
        .stdin(Stdio::piped())
        .spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(text.as_bytes())?;
    }

    let status = child.wait()?;
    anyhow::ensure!(status.success(), "pbcopy exited with status {status}");
    Ok(())
}

fn encode_unicode_text(text: &str) -> Vec<u8> {
    let normalized = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n");
    let mut bytes = Vec::with_capacity((normalized.len() + 1) * 2);
    for unit in normalized.encode_utf16().chain(std::iter::once(0)) {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

fn decode_unicode_text(data: &[u8]) -> anyhow::Result<String> {
    anyhow::ensure!(
        data.len().is_multiple_of(2),
        "CF_UNICODETEXT payload has odd byte length"
    );

    let mut units = data
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    while units.last() == Some(&0) {
        units.pop();
    }

    let text = String::from_utf16(&units)?;
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_text_roundtrips_with_crlf_wire_endings() {
        let encoded = encode_unicode_text("hello\nworld");

        assert!(encoded.ends_with(&[0, 0]));
        assert_eq!(decode_unicode_text(&encoded).unwrap(), "hello\nworld");
    }

    #[test]
    fn unicode_text_rejects_odd_length_payloads() {
        assert!(decode_unicode_text(&[0]).is_err());
    }
}

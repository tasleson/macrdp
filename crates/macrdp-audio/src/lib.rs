pub mod converter;

#[cfg(feature = "opus")]
pub mod opus;

use std::fmt;
use std::sync::Mutex;

use ironrdp_rdpsnd::pdu::{AudioFormat, WaveFormat};
use ironrdp_rdpsnd::server::RdpsndServerHandler;
#[allow(unused_imports)]
use ironrdp_server::{ServerEvent, RdpsndServerMessage, SoundServerFactory, ServerEventSender};
use macrdp_capture::AudioFrame;
use tokio::sync::mpsc;

/// Find best matching server format index given client's supported formats.
/// Iterates server formats in reverse (higher index = higher priority like Opus).
pub fn find_matching_format(
    server_formats: &[AudioFormat],
    client_formats: &[AudioFormat],
) -> Option<usize> {
    for (idx, server_fmt) in server_formats.iter().enumerate().rev() {
        for client_fmt in client_formats {
            if server_fmt.format == client_fmt.format
                && server_fmt.n_samples_per_sec == client_fmt.n_samples_per_sec
                && server_fmt.n_channels == client_fmt.n_channels
                && server_fmt.bits_per_sample == client_fmt.bits_per_sample
            {
                return Some(idx);
            }
        }
    }
    None
}

pub struct MacAudioHandler {
    formats: Vec<AudioFormat>,
    selected_format: Option<usize>,
}

impl fmt::Debug for MacAudioHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacAudioHandler")
            .field("selected_format", &self.selected_format)
            .finish_non_exhaustive()
    }
}

impl MacAudioHandler {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        let formats = vec![
            AudioFormat {
                format: WaveFormat::PCM,
                n_channels: channels,
                n_samples_per_sec: sample_rate,
                n_avg_bytes_per_sec: sample_rate * (channels as u32) * 2,
                n_block_align: channels * 2,
                bits_per_sample: 16,
                data: None,
            },
        ];

        Self {
            formats,
            selected_format: None,
        }
    }
}

impl RdpsndServerHandler for MacAudioHandler {
    fn get_formats(&self) -> &[AudioFormat] {
        &self.formats
    }

    fn start(&mut self, client_format: &ironrdp_rdpsnd::pdu::ClientAudioFormatPdu) -> Option<u16> {
        let idx = find_matching_format(&self.formats, &client_format.formats)?;
        self.selected_format = Some(idx);
        tracing::info!(format_idx = idx, "Audio started");
        Some(idx as u16)
    }

    fn stop(&mut self) {
        tracing::info!("Audio stopped");
        self.selected_format = None;
    }
}

/// Factory that creates MacAudioHandler instances.
pub struct MacAudioFactory {
    audio_rx: Mutex<Option<mpsc::Receiver<AudioFrame>>>,
    event_sender: Option<tokio::sync::mpsc::UnboundedSender<ServerEvent>>,
    sample_rate: u32,
    channels: u16,
}

impl MacAudioFactory {
    pub fn new(audio_rx: mpsc::Receiver<AudioFrame>, sample_rate: u32, channels: u16) -> Self {
        Self {
            audio_rx: Mutex::new(Some(audio_rx)),
            event_sender: None,
            sample_rate,
            channels,
        }
    }
}

impl SoundServerFactory for MacAudioFactory {
    fn build_backend(&self) -> Box<dyn RdpsndServerHandler> {
        let _rx = self.audio_rx.lock().unwrap().take()
            .expect("audio_rx already taken — build_backend called more than once");

        // TODO Task 4: spawn audio_loop with _rx and event_sender

        Box::new(MacAudioHandler::new(self.sample_rate, self.channels))
    }
}

impl ServerEventSender for MacAudioFactory {
    fn set_sender(&mut self, sender: tokio::sync::mpsc::UnboundedSender<ServerEvent>) {
        self.event_sender = Some(sender);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm_48k_stereo() -> AudioFormat {
        AudioFormat {
            format: WaveFormat::PCM,
            n_channels: 2,
            n_samples_per_sec: 48000,
            n_avg_bytes_per_sec: 192000,
            n_block_align: 4,
            bits_per_sample: 16,
            data: None,
        }
    }

    #[test]
    fn match_format_pcm_found() {
        let server_formats = vec![pcm_48k_stereo()];
        let client_formats = vec![pcm_48k_stereo()];
        let result = find_matching_format(&server_formats, &client_formats);
        assert_eq!(result, Some(0));
    }

    #[test]
    fn match_format_no_match() {
        let server_formats = vec![pcm_48k_stereo()];
        let client_formats = vec![AudioFormat {
            format: WaveFormat::PCM,
            n_channels: 1,
            n_samples_per_sec: 44100,
            n_avg_bytes_per_sec: 88200,
            n_block_align: 2,
            bits_per_sample: 16,
            data: None,
        }];
        let result = find_matching_format(&server_formats, &client_formats);
        assert_eq!(result, None);
    }
}

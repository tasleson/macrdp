pub mod converter;
mod local_mute;

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ironrdp_rdpsnd::pdu::{AudioFormat, WaveFormat};
use ironrdp_rdpsnd::server::{RdpsndServerHandler, RdpsndServerMessage};
use ironrdp_server::{ServerEvent, ServerEventSender, SoundServerFactory};
use macrdp_capture::AudioFrame;
use tokio::sync::mpsc;

const SILENCE_THRESHOLD: f32 = 0.002;
const FADE_MS: u32 = 5;
const DISCONTINUITY_THRESHOLD: f32 = 0.15;
// Cap the ring buffer at 500ms to bound memory if capture races ahead
const MAX_BUFFER_CHUNKS: usize = 25;

fn is_chunk_silent(chunk: &[f32]) -> bool {
    chunk.iter().all(|&s| s.abs() < SILENCE_THRESHOLD)
}

fn apply_fade_in(chunk: &mut [f32], channels: u16, fade_frames: usize) {
    let ch = channels as usize;
    let total_frames = chunk.len() / ch;
    let ramp_len = fade_frames.min(total_frames);
    for frame_idx in 0..ramp_len {
        let gain = frame_idx as f32 / ramp_len as f32;
        for c in 0..ch {
            chunk[frame_idx * ch + c] *= gain;
        }
    }
}

fn crossfade_discontinuity(
    chunk: &mut [f32],
    prev_tail: &[f32],
    channels: u16,
    fade_frames: usize,
) {
    let ch = channels as usize;
    let total_frames = chunk.len() / ch;
    let ramp_len = fade_frames.min(total_frames);
    let tail_frames = prev_tail.len() / ch;
    if tail_frames == 0 || ramp_len == 0 {
        return;
    }
    for c in 0..ch {
        let expected = prev_tail[(tail_frames - 1) * ch + c];
        for i in 0..ramp_len {
            let t = (i + 1) as f32 / (ramp_len + 1) as f32;
            let idx = i * ch + c;
            chunk[idx] = expected * (1.0 - t) + chunk[idx] * t;
        }
    }
}

fn has_discontinuity(prev_tail: &[f32], chunk: &[f32], channels: u16) -> bool {
    let ch = channels as usize;
    if prev_tail.len() < ch || chunk.len() < ch {
        return false;
    }
    let tail_start = prev_tail.len() - ch;
    for c in 0..ch {
        if (chunk[c] - prev_tail[tail_start + c]).abs() > DISCONTINUITY_THRESHOLD {
            return true;
        }
    }
    false
}

struct ChannelClosed;

/// Drain all pending frames from the capture channel into the ring buffer.
/// Returns the timestamp of the first frame drained (if any), or
/// `Err(ChannelClosed)` once the capture side has gone away for good.
fn drain_channel(
    audio_rx: &mut mpsc::Receiver<AudioFrame>,
    ring_buffer: &mut VecDeque<f32>,
) -> Result<Option<u64>, ChannelClosed> {
    let mut first_ts = None;
    loop {
        match audio_rx.try_recv() {
            Ok(frame) => {
                if first_ts.is_none() {
                    first_ts = Some(frame.timestamp_ms);
                }
                ring_buffer.extend(frame.data.iter());
            }
            Err(mpsc::error::TryRecvError::Empty) => return Ok(first_ts),
            Err(mpsc::error::TryRecvError::Disconnected) => return Err(ChannelClosed),
        }
    }
}

async fn audio_loop(
    mut audio_rx: mpsc::Receiver<AudioFrame>,
    event_sender: tokio::sync::mpsc::UnboundedSender<ServerEvent>,
    ready: Arc<AtomicBool>,
    frame_size_interleaved: usize,
    channels: u16,
    sample_rate: u32,
) {
    let ch = channels as usize;
    let mut ring_buffer: VecDeque<f32> = VecDeque::with_capacity(frame_size_interleaved * 8);
    let mut samples_sent: u64 = 0;
    let mut was_silent = true;
    let fade_frames = (sample_rate * FADE_MS / 1000) as usize;
    let mut prev_tail: Vec<f32> = Vec::new();
    let max_buffer_samples = frame_size_interleaved * MAX_BUFFER_CHUNKS;

    let chunk_ms = (frame_size_interleaved as u64 * 1000) / (sample_rate as u64 * ch as u64);
    let chunk_duration = Duration::from_millis(chunk_ms);

    tracing::info!(
        frame_size_interleaved,
        chunk_ms,
        "Audio loop started ({}Hz, {}ch)",
        sample_rate,
        channels
    );

    // This loop outlives any single RDP connection: each connection's RDPSND
    // handler toggles the shared `ready` flag, and streaming re-baselines its
    // timestamps every time the flag turns on again. `None` means streaming is
    // (re)starting and needs a fresh timestamp anchor from the next frame.
    let mut base_timestamp_ms: Option<u64> = None;

    let mut interval = tokio::time::interval(chunk_duration);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        let first_ts = match drain_channel(&mut audio_rx, &mut ring_buffer) {
            Ok(ts) => ts,
            Err(ChannelClosed) => {
                tracing::info!("Audio loop ended (capture channel closed)");
                return;
            }
        };

        if !ready.load(Ordering::Relaxed) {
            // No client listening — discard captured audio and reset state so
            // the next connection starts clean.
            ring_buffer.clear();
            prev_tail.clear();
            was_silent = true;
            if base_timestamp_ms.take().is_some() {
                tracing::info!("Audio streaming paused (RDPSND stopped)");
            }
            continue;
        }

        let base_ts = match base_timestamp_ms {
            Some(ts) => ts,
            None => {
                // (Re)starting: anchor timestamps to the first captured frame.
                let Some(ts) = first_ts else { continue };
                base_timestamp_ms = Some(ts);
                samples_sent = 0;
                tracing::info!("Audio streaming started");
                ts
            }
        };

        // Trim excess buffer to bound latency
        if ring_buffer.len() > max_buffer_samples {
            let excess = ring_buffer.len() - max_buffer_samples;
            ring_buffer.drain(..excess);
            tracing::debug!(excess, "Trimmed audio ring buffer");
        }

        let wave_data = if ring_buffer.len() >= frame_size_interleaved {
            let mut chunk: Vec<f32> = ring_buffer.drain(..frame_size_interleaved).collect();

            let silent = is_chunk_silent(&chunk);
            if was_silent && !silent {
                apply_fade_in(&mut chunk, channels, fade_frames);
            } else if !was_silent && !silent && has_discontinuity(&prev_tail, &chunk, channels) {
                crossfade_discontinuity(&mut chunk, &prev_tail, channels, fade_frames);
            }
            was_silent = silent;

            if chunk.len() >= ch {
                prev_tail.clear();
                prev_tail.extend_from_slice(&chunk[chunk.len() - ch..]);
            }

            converter::AudioConverter::float32_to_s16le(&chunk)
        } else {
            // Buffer underrun — send silence to keep client buffer fed
            was_silent = true;
            prev_tail.clear();
            vec![0u8; frame_size_interleaved * 2]
        };

        let samples_per_ch = frame_size_interleaved / ch;
        let offset_ms = (samples_sent * 1000) / sample_rate as u64;
        let ts = base_ts + offset_ms;

        let _ = event_sender.send(ServerEvent::Rdpsnd(RdpsndServerMessage::Wave(
            wave_data, ts as u32,
        )));

        samples_sent += samples_per_ch as u64;
    }
}

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
    ready: Arc<AtomicBool>,
    mute_guard: Option<local_mute::LocalMuteGuard>,
}

impl fmt::Debug for MacAudioHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacAudioHandler")
            .field("selected_format", &self.selected_format)
            .finish_non_exhaustive()
    }
}

impl MacAudioHandler {
    pub fn new(sample_rate: u32, channels: u16, ready: Arc<AtomicBool>) -> Self {
        let formats = vec![AudioFormat {
            format: WaveFormat::PCM,
            n_channels: channels,
            n_samples_per_sec: sample_rate,
            n_avg_bytes_per_sec: sample_rate * (channels as u32) * 2,
            n_block_align: channels * 2,
            bits_per_sample: 16,
            data: None,
        }];

        Self {
            formats,
            selected_format: None,
            ready,
            mute_guard: None,
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
        self.ready.store(true, Ordering::Relaxed);
        self.mute_guard = local_mute::LocalMuteGuard::mute_local();
        tracing::info!(
            format_idx = idx,
            "Audio format negotiated — streaming enabled"
        );
        Some(idx as u16)
    }

    fn stop(&mut self) {
        self.ready.store(false, Ordering::Relaxed);
        if let Some(guard) = self.mute_guard.take() {
            guard.restore_local();
        }
        tracing::info!("Audio stopped");
        self.selected_format = None;
    }
}

pub struct MacAudioFactory {
    audio_rx: Mutex<Option<mpsc::Receiver<AudioFrame>>>,
    event_sender: Option<tokio::sync::mpsc::UnboundedSender<ServerEvent>>,
    sample_rate: u32,
    channels: u16,
    ready: Arc<AtomicBool>,
}

impl MacAudioFactory {
    pub fn new(audio_rx: mpsc::Receiver<AudioFrame>, sample_rate: u32, channels: u16) -> Self {
        Self {
            audio_rx: Mutex::new(Some(audio_rx)),
            event_sender: None,
            sample_rate,
            channels,
            ready: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl SoundServerFactory for MacAudioFactory {
    fn build_backend(&self) -> Box<dyn RdpsndServerHandler> {
        // The audio loop is spawned once, on the first connection, and runs
        // for the server's lifetime. Every connection's handler shares the
        // same `ready` flag, so a reconnecting client resumes streaming on
        // the existing loop instead of going permanently silent.
        if let Some(sender) = self.event_sender.clone() {
            if let Some(rx) = self.audio_rx.lock().unwrap().take() {
                let frame_size_interleaved =
                    (self.sample_rate as usize / 50) * self.channels as usize;
                tokio::spawn(audio_loop(
                    rx,
                    sender,
                    Arc::clone(&self.ready),
                    frame_size_interleaved,
                    self.channels,
                    self.sample_rate,
                ));
            }
        }

        Box::new(MacAudioHandler::new(
            self.sample_rate,
            self.channels,
            Arc::clone(&self.ready),
        ))
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

    #[test]
    fn silence_detection() {
        assert!(is_chunk_silent(&[0.0; 100]));
        assert!(is_chunk_silent(&[0.001, -0.001, 0.0]));
        assert!(!is_chunk_silent(&[0.0, 0.5, 0.0]));
    }

    #[test]
    fn fade_in_ramps_from_zero() {
        let mut chunk = vec![1.0; 20]; // 10 stereo frames
        apply_fade_in(&mut chunk, 2, 10);
        assert_eq!(chunk[0], 0.0); // frame 0: gain = 0/10
        assert_eq!(chunk[1], 0.0);
        assert!((chunk[10] - 0.5).abs() < 1e-6); // frame 5: gain = 5/10
        assert!((chunk[11] - 0.5).abs() < 1e-6);
        assert!((chunk[18] - 0.9).abs() < 1e-6); // frame 9: gain = 9/10
    }

    #[test]
    fn fade_in_shorter_than_chunk() {
        let mut chunk = vec![1.0; 20]; // 10 stereo frames
        apply_fade_in(&mut chunk, 2, 4);
        assert_eq!(chunk[0], 0.0);
        assert!((chunk[6] - 0.75).abs() < 1e-6); // frame 3: gain = 3/4
        assert_eq!(chunk[8], 1.0); // frame 4: untouched
    }

    #[test]
    fn discontinuity_detected() {
        let prev_tail = vec![0.5, 0.5]; // stereo: L=0.5, R=0.5
        let chunk = vec![0.9, 0.9, 0.8, 0.8]; // jump of 0.4 > 0.15
        assert!(has_discontinuity(&prev_tail, &chunk, 2));
    }

    #[test]
    fn no_discontinuity_for_smooth_signal() {
        let prev_tail = vec![0.5, 0.5];
        let chunk = vec![0.55, 0.55, 0.6, 0.6]; // jump of 0.05 < 0.15
        assert!(!has_discontinuity(&prev_tail, &chunk, 2));
    }

    fn audio_frame(timestamp_ms: u64, data: Vec<f32>) -> AudioFrame {
        let num_samples = data.len() / 2;
        AudioFrame {
            data,
            sample_rate: 48000,
            channels: 2,
            num_samples,
            timestamp_ms,
        }
    }

    /// Receive events until none arrive for a while; returns how many were drained.
    async fn drain_events(ev_rx: &mut tokio::sync::mpsc::UnboundedReceiver<ServerEvent>) -> usize {
        let mut n = 0;
        while tokio::time::timeout(Duration::from_millis(200), ev_rx.recv())
            .await
            .is_ok()
        {
            n += 1;
        }
        n
    }

    fn wave_ts(ev: ServerEvent) -> u32 {
        match ev {
            ServerEvent::Rdpsnd(RdpsndServerMessage::Wave(_, ts)) => ts,
            other => panic!("expected Wave event, got {other:?}"),
        }
    }

    /// Regression test: a client that disconnects (handler `stop()`) and
    /// reconnects (new handler's `start()` on the shared `ready` flag) must
    /// resume audio on the same long-lived loop, with re-based timestamps.
    #[tokio::test(start_paused = true)]
    async fn audio_loop_resumes_after_reconnect() {
        const FRAME_SIZE: usize = 1920; // 20ms at 48kHz stereo

        let (audio_tx, audio_rx) = mpsc::channel::<AudioFrame>(64);
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let ready = Arc::new(AtomicBool::new(false));

        tokio::spawn(audio_loop(
            audio_rx,
            ev_tx,
            Arc::clone(&ready),
            FRAME_SIZE,
            2,
            48000,
        ));

        // Frames arriving before the RDPSND handshake are discarded.
        audio_tx
            .send(audio_frame(500, vec![0.5; FRAME_SIZE]))
            .await
            .unwrap();
        assert_eq!(drain_events(&mut ev_rx).await, 0);

        // First connection completes the handshake.
        ready.store(true, Ordering::Relaxed);
        audio_tx
            .send(audio_frame(1000, vec![0.5; FRAME_SIZE]))
            .await
            .unwrap();
        let ts = wave_ts(ev_rx.recv().await.expect("loop alive"));
        assert_eq!(ts, 1000);

        // Client disconnects: handler stop() flips the shared flag. The loop
        // must go quiet but stay alive.
        ready.store(false, Ordering::Relaxed);
        drain_events(&mut ev_rx).await;
        audio_tx
            .send(audio_frame(50_000, vec![0.5; FRAME_SIZE]))
            .await
            .unwrap();
        assert_eq!(drain_events(&mut ev_rx).await, 0);

        // New connection negotiates RDPSND again: streaming must resume,
        // re-anchored to the new frame's timestamp.
        ready.store(true, Ordering::Relaxed);
        audio_tx
            .send(audio_frame(99_000, vec![0.5; FRAME_SIZE]))
            .await
            .unwrap();
        let ts = wave_ts(ev_rx.recv().await.expect("loop must survive reconnect"));
        assert_eq!(ts, 99_000);
    }

    #[test]
    fn crossfade_blends_from_previous() {
        let prev_tail = vec![1.0, 1.0]; // stereo: last frame was (1.0, 1.0)
        let mut chunk = vec![0.0; 10]; // 5 stereo frames, all zero
        crossfade_discontinuity(&mut chunk, &prev_tail, 2, 4);
        // frame 0: t = 1/5 = 0.2, expected * 0.8 + chunk * 0.2 = 1.0 * 0.8 = 0.8
        assert!((chunk[0] - 0.8).abs() < 1e-6);
        assert!((chunk[1] - 0.8).abs() < 1e-6);
        // frame 3: t = 4/5 = 0.8, expected * 0.2 + chunk * 0.8 = 1.0 * 0.2 = 0.2
        assert!((chunk[6] - 0.2).abs() < 1e-6);
        // frame 4: untouched (beyond fade_frames)
        assert_eq!(chunk[8], 0.0);
    }
}

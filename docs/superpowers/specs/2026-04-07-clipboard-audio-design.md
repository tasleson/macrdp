# Phase 3: Clipboard Sync + Audio Forwarding — Design Spec

**Date:** 2026-04-07
**Status:** Draft
**Scope:** macrdp Phase 3 — Clipboard synchronization and audio forwarding over RDP

---

## 1. Overview

This spec covers two independent but related features for the macrdp RDP server:

1. **Clipboard Synchronization** — Bidirectional clipboard sync between macOS and RDP clients, supporting text, images, and files via the RDP CLIPRDR (MS-RDPECLIP) static virtual channel.
2. **Audio Forwarding** — macOS system audio output forwarded to RDP clients for playback via the RDP RDPSND (MS-RDPEA) static virtual channel.

### Out of Scope

- **AUDIN (Audio Input)** — Client microphone → macOS. Deferred to a future phase; IronRDP does not have an AUDIN crate.
- **Performance optimization** — Encoding pipeline and network transport improvements are a separate spec.

### Prerequisites

- macOS 14+ (ScreenCaptureKit audio APIs)
- IronRDP 0.10 (ironrdp-cliprdr 0.5, ironrdp-rdpsnd 0.7)
- Existing ironrdp-server-gfx fork with CLIPRDR/RDPSND stub framework

---

## 2. Architecture

### 2.1 New Crate Structure

```
crates/
├── macrdp-clipboard/        # NEW — macOS clipboard backend
│   └── src/
│       ├── lib.rs           # CliprdrBackend implementation + factory
│       ├── pasteboard.rs    # NSPasteboard objc2 wrapper
│       ├── formats.rs       # Format conversion (macOS UTI ↔ RDP ClipboardFormat)
│       └── file.rs          # File clipboard (FileGroupDescriptor + FileContents)
│
├── macrdp-audio/            # NEW — Audio capture processing + RDP handler
│   └── src/
│       ├── lib.rs           # RdpsndServerHandler implementation + factory
│       ├── converter.rs     # Float32 PCM → S16LE PCM conversion
│       └── opus.rs          # Optional Opus encoding (feature-gated)
│
├── macrdp-capture/          # MODIFIED — Add audio output channel from SCK
├── macrdp-core/             # MODIFIED — Integrate clipboard + audio factories
└── ironrdp-server-gfx/     # EXISTING — CLIPRDR/RDPSND SVC framework already in place
```

### 2.2 Data Flow

#### Clipboard (Bidirectional)

```
macOS → RDP Client (Server-initiated Copy):
  NSPasteboard changeCount polling (500ms interval)
    → Detect change → Read available UTI types
    → Map UTIs to RDP ClipboardFormats
    → ClipboardMessage::SendInitiateCopy(formats)
    → [ironrdp-server-gfx event loop] → SVC → Client
    → Client requests FormatDataRequest(format_id)
    → [event loop] → CliprdrBackend::on_format_data_request()
    → Read NSPasteboard data for format → Convert to RDP format
    → ClipboardMessage::SendFormatData(response)
    → [event loop] → SVC → Client receives data

RDP Client → macOS (Client-initiated Copy):
  Client sends FormatList → [SVC] → CliprdrBackend::on_remote_copy(formats)
    → Store remote format list (lazy fetch)
    → [Later: macOS app pastes]
    → CliprdrBackend::on_format_data_request() triggered
    → ClipboardMessage::SendInitiatePaste(format_id)
    → [event loop] → SVC → Client sends FormatDataResponse
    → CliprdrBackend::on_format_data_response(data)
    → Convert RDP format → macOS format → Write to NSPasteboard
```

#### Audio (Server → Client)

```
ScreenCaptureKit Audio Callback (48kHz Float32 PCM, non-interleaved)
  → Interleave channels if needed
  → mpsc::channel<AudioFrame> → macrdp-audio
  → Ring buffer accumulation (20ms frames = 960 samples @ 48kHz)
  → Float32 → S16LE conversion (or Opus encoding)
  → RdpsndServerMessage::Wave(pcm_data, timestamp_ms)
  → ServerEvent::Rdpsnd → [ironrdp-server-gfx event loop]
  → SVC encode → Client playback
```

---

## 3. Clipboard Module (`macrdp-clipboard`)

### 3.1 PasteboardBridge — NSPasteboard Wrapper

A safe Rust wrapper around `NSPasteboard` using `objc2` + `objc2-app-kit` + `objc2-foundation`.

**Public API:**

```rust
pub struct PasteboardBridge { /* NSPasteboard reference */ }

impl PasteboardBridge {
    pub fn new() -> Self;                           // NSPasteboard.generalPasteboard
    pub fn change_count(&self) -> i64;              // Current changeCount
    pub fn available_types(&self) -> Vec<String>;   // Available UTI type identifiers
    pub fn read_string(&self) -> Option<String>;    // Read UTF-8 text
    pub fn read_image(&self) -> Option<Vec<u8>>;    // Read image as PNG bytes
    pub fn read_html(&self) -> Option<String>;      // Read HTML content
    pub fn read_file_urls(&self) -> Vec<PathBuf>;   // Read file URL references
    pub fn write_string(&self, text: &str);         // Write text
    pub fn write_image(&self, png_data: &[u8]);     // Write PNG image
    pub fn write_file_urls(&self, paths: &[PathBuf]); // Write file references
    pub fn clear(&self);                            // Clear and increment changeCount
}
```

**Threading:** NSPasteboard must be accessed from a thread with a RunLoop. The polling thread will use `CFRunLoopRunInMode` or a dedicated `NSThread`.

### 3.2 FormatConverter — macOS ↔ RDP Format Mapping

| Direction | macOS Format | RDP Format | Conversion Logic |
|-----------|-------------|------------|-----------------|
| mac→rdp | NSPasteboardTypeString (UTF-8) | CF_UNICODETEXT (13) | UTF-8 → UTF-16LE + null terminator |
| rdp→mac | CF_UNICODETEXT (13) | NSPasteboardTypeString | UTF-16LE → UTF-8, strip null |
| mac→rdp | PNG/TIFF image | CF_DIB (8) | Decode image → BITMAPINFOHEADER + BGR pixel rows (bottom-up) |
| rdp→mac | CF_DIB (8) | PNG image | Parse DIB header → extract pixels → encode PNG |
| mac→rdp | NSPasteboardTypeHTML | "HTML Format" (registered) | Wrap in Windows HTML Format header (StartHTML/EndHTML/StartFragment/EndFragment) |
| rdp→mac | "HTML Format" | NSPasteboardTypeHTML | Strip Windows HTML Format header, extract fragment |
| mac→rdp | file URLs | FileGroupDescriptorW (registered) | Build FILEDESCRIPTORW array from file metadata |
| mac→rdp | file content | FileContentsResponse | Read file bytes at offset + length |
| rdp→mac | FileGroupDescriptorW | file URLs | Parse descriptors → request contents → write to temp dir → NSPasteboard file URLs |

**UTF-16LE Conversion:**
```rust
fn utf8_to_utf16le(s: &str) -> Vec<u8> {
    let utf16: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    utf16.iter().flat_map(|&w| w.to_le_bytes()).collect()
}

fn utf16le_to_utf8(data: &[u8]) -> Option<String> {
    let words: Vec<u16> = data.chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&words).ok()
        .map(|s| s.trim_end_matches('\0').to_string())
}
```

**DIB Conversion:**
Use the `image` crate for PNG/TIFF decode/encode. DIB format requires:
- BITMAPINFOHEADER (40 bytes): width, height (negative = top-down), planes=1, bitCount=32, compression=BI_RGB
- Pixel data: BGRA or BGR, row-aligned to 4-byte boundary

### 3.3 File Clipboard — FileGroupDescriptor + FileContents

**macOS → Client file transfer:**

1. Detect `public.file-url` in NSPasteboard
2. Resolve file URLs to absolute paths
3. Collect file metadata: name, size, timestamps (creation, modification, access)
4. Build `FileGroupDescriptorW` structure:
   - `cItems: u32` — number of files
   - For each file: `FILEDESCRIPTORW` with `dwFlags`, `nFileSizeHigh/Low`, `ftCreationTime`, `ftLastWriteTime`, `cFileName[260]` (UTF-16LE)
5. When client sends `FileContentsRequest`:
   - `dwFlags` specifies SIZE or RANGE
   - For SIZE: return file size as 8-byte LE u64
   - For RANGE: open file, seek to `nPositionLow/High`, read `cbRequested` bytes, return in `FileContentsResponse`
6. Maintain open file handle cache (HashMap<u32, File>) keyed by `streamId`
7. Clean up file handles when clipboard changes or lock is released

**Client → macOS file transfer:**

1. Receive `FormatList` containing FileGroupDescriptorW format
2. On paste: request `FormatDataRequest` for FileGroupDescriptorW
3. Parse response to get file list (names, sizes)
4. For each file: send `FileContentsRequest` with RANGE flag, accumulate data
5. Write received files to `temporary_directory()` (typically `~/.cache/macrdp/clipboard/`)
6. Set NSPasteboard with file URLs pointing to temp directory

**Lock mechanism:**
- `on_lock(lock_id)` — prevent clipboard changes while file transfer is in progress
- `on_unlock(lock_id)` — release lock, allow clipboard updates

### 3.4 MacClipboardBackend — CliprdrBackend Implementation

```rust
pub struct MacClipboardBackend {
    pasteboard: PasteboardBridge,
    event_sender: mpsc::UnboundedSender<ServerEvent>,
    last_change_count: AtomicI64,
    remote_formats: Mutex<Vec<ClipboardFormat>>,
    file_handles: Mutex<HashMap<u32, File>>,
    temp_dir: PathBuf,
    poll_handle: Option<JoinHandle<()>>,
}
```

**Polling thread:**
```rust
fn poll_clipboard(bridge: &PasteboardBridge, last: &AtomicI64, sender: &Sender) {
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let current = bridge.change_count();
        let prev = last.swap(current, Ordering::SeqCst);
        if current != prev {
            // Read formats, send ClipboardMessage::SendInitiateCopy
            let formats = bridge.available_types()
                .iter()
                .filter_map(|uti| uti_to_rdp_format(uti))
                .collect();
            sender.send(ServerEvent::Clipboard(
                ClipboardMessage::SendInitiateCopy(formats)
            ));
        }
    }
}
```

**Anti-echo:** When we write to NSPasteboard ourselves (due to client copy), increment `last_change_count` to skip the next poll detection. This prevents infinite clipboard echo loops.

### 3.5 Dependencies

```toml
[dependencies]
objc2 = "0.6"
objc2-foundation = { version = "0.3", features = ["NSString", "NSArray", "NSURL", "NSData"] }
objc2-app-kit = { version = "0.3", features = ["NSPasteboard"] }
image = { version = "0.25", default-features = false, features = ["png", "tiff"] }
anyhow = { workspace = true }
tracing = { workspace = true }
tokio = { workspace = true }
```

---

## 4. Audio Module (`macrdp-audio`)

### 4.1 SCK Audio Integration (macrdp-capture changes)

**Configuration additions to SCStreamConfiguration:**

```rust
let config = SCStreamConfiguration::new()
    // ... existing video config ...
    .with_captures_audio(true)
    .with_sample_rate(AudioSampleRate::Rate48000)
    .with_channel_count(AudioChannelCount::Stereo)
    .with_excludes_current_process_audio(true);
```

**Output handler modification:**

```rust
fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
    match of_type {
        SCStreamOutputType::Screen => {
            // Existing video frame handling
        }
        SCStreamOutputType::Audio => {
            if let Some(audio_buffer_list) = sample.audio_buffer_list() {
                let format = sample.format_description();
                let timestamp = sample.presentation_timestamp();
                let frame = AudioFrame::from_sample_buffer(
                    &audio_buffer_list, &format, &timestamp
                );
                let _ = self.audio_tx.try_send(frame);
            }
        }
        _ => {} // Microphone type ignored for now
    }
}
```

**AudioFrame struct (in macrdp-capture):**

```rust
pub struct AudioFrame {
    pub data: Vec<f32>,       // Interleaved Float32 PCM samples
    pub sample_rate: u32,     // 48000
    pub channels: u16,        // 2
    pub num_samples: usize,   // Number of samples per channel
    pub timestamp_ms: u64,    // Presentation timestamp in milliseconds
}
```

**Channel creation:**

```rust
// In macrdp-capture or macrdp-core
let (audio_tx, audio_rx) = tokio::sync::mpsc::channel::<AudioFrame>(32);
// audio_tx → capture output handler
// audio_rx → MacAudioHandler
```

### 4.2 AudioConverter — Format Conversion

```rust
pub struct AudioConverter;

impl AudioConverter {
    /// Convert Float32 interleaved PCM to S16LE interleaved PCM bytes.
    pub fn float32_to_s16le(input: &[f32]) -> Vec<u8> {
        let mut output = Vec::with_capacity(input.len() * 2);
        for &sample in input {
            let clamped = sample.clamp(-1.0, 1.0);
            let scaled = (clamped * 32767.0) as i16;
            output.extend_from_slice(&scaled.to_le_bytes());
        }
        output
    }

    /// Interleave non-interleaved audio buffers.
    /// SCK may output non-interleaved: [L0,L1,...,Ln, R0,R1,...,Rn]
    /// RDP expects interleaved: [L0,R0, L1,R1, ..., Ln,Rn]
    pub fn interleave(buffers: &[&[f32]], num_samples: usize) -> Vec<f32> {
        let channels = buffers.len();
        let mut output = Vec::with_capacity(num_samples * channels);
        for i in 0..num_samples {
            for ch in buffers {
                output.push(ch[i]);
            }
        }
        output
    }
}
```

**Performance note:** At 48kHz stereo, Float32→S16LE processes ~384KB/s of input data. This is trivial CPU work. SIMD optimization (vDSP) is possible but unnecessary at this scale.

### 4.3 OpusEncoder (Feature-gated)

```toml
[features]
default = []
opus = ["dep:opus"]

[dependencies.opus]
version = "0.3"
optional = true
```

```rust
#[cfg(feature = "opus")]
pub struct OpusEncoder {
    encoder: opus::Encoder,
    frame_size: usize,     // 960 samples (20ms @ 48kHz)
    buffer: Vec<f32>,      // Accumulation buffer
}

#[cfg(feature = "opus")]
impl OpusEncoder {
    pub fn new(sample_rate: u32, channels: u16) -> Result<Self>;
    pub fn encode(&mut self, input: &[f32]) -> Vec<Vec<u8>>;  // Returns encoded Opus frames
}
```

### 4.4 MacAudioHandler — RdpsndServerHandler Implementation

```rust
pub struct MacAudioHandler {
    audio_rx: mpsc::Receiver<AudioFrame>,
    event_sender: mpsc::UnboundedSender<ServerEvent>,
    formats: Vec<AudioFormat>,
    selected_format: Option<usize>,
    ring_buffer: VecDeque<f32>,
    frame_size: usize,         // Samples per RDP wave packet (960 for 20ms@48kHz)
    block_number: u16,         // Wave block sequence counter
    #[cfg(feature = "opus")]
    opus_encoder: Option<OpusEncoder>,
}

impl RdpsndServerHandler for MacAudioHandler {
    fn get_formats(&self) -> &[AudioFormat] {
        // Returns:
        // [0] PCM S16LE, 48000Hz, 2ch, 16-bit
        //     n_block_align = 4 (2ch * 2bytes)
        //     n_avg_bytes_per_sec = 192000 (48000 * 4)
        // [1] (if opus feature) Opus, 48000Hz, 2ch
        &self.formats
    }

    fn start(&mut self, client_format: &ClientAudioFormatPdu) -> Option<u16> {
        // Client selected a format index from our list
        // Store the selection, start the audio processing task
        let server_idx = /* match client selection to server format */ ;
        self.selected_format = Some(server_idx);
        // Spawn audio processing loop
        Some(server_idx as u16)
    }

    fn stop(&mut self) {
        self.selected_format = None;
        self.ring_buffer.clear();
    }
}
```

### 4.5 Audio Processing Loop

```rust
async fn audio_loop(
    mut audio_rx: mpsc::Receiver<AudioFrame>,
    event_sender: mpsc::UnboundedSender<ServerEvent>,
    frame_size: usize,
    selected_format: AudioFormatType,
) {
    let mut ring_buffer = VecDeque::with_capacity(frame_size * 4);
    let mut block_number: u16 = 0;

    while let Some(frame) = audio_rx.recv().await {
        ring_buffer.extend(frame.data.iter());

        // Emit 20ms frames
        while ring_buffer.len() >= frame_size * 2 /* stereo */ {
            let chunk: Vec<f32> = ring_buffer.drain(..frame_size * 2).collect();

            let wave_data = match selected_format {
                AudioFormatType::Pcm => AudioConverter::float32_to_s16le(&chunk),
                #[cfg(feature = "opus")]
                AudioFormatType::Opus => opus_encoder.encode(&chunk),
            };

            let timestamp = block_number as u32 * 20; // 20ms per block
            let _ = event_sender.send(ServerEvent::Rdpsnd(
                RdpsndServerMessage::Wave(wave_data, timestamp)
            ));
            block_number = block_number.wrapping_add(1);
        }
    }
}
```

**Back-pressure:** `mpsc::channel` with bounded capacity (32 frames). If the channel is full, `try_send` drops the oldest frame. Audio tolerates frame drops better than accumulated latency.

### 4.6 Dependencies

```toml
[dependencies]
anyhow = { workspace = true }
tracing = { workspace = true }
tokio = { workspace = true, features = ["sync"] }
opus = { version = "0.3", optional = true }

[features]
default = []
opus = ["dep:opus"]
```

---

## 5. Integration Layer (`macrdp-core` changes)

### 5.1 Server Builder Integration

```rust
// In macrdp-core server setup
pub async fn build_rdp_server(config: &Config) -> Result<RdpServer> {
    let (ev_sender, ev_receiver) = ServerEvent::create_channel();

    // Audio setup
    let (audio_tx, audio_rx) = if config.audio.enabled {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    // Screen capture (pass audio_tx if enabled)
    let capturer = SckCapturer::new(config, audio_tx)?;

    // Audio factory
    let sound_factory = if let Some(rx) = audio_rx {
        Some(Box::new(MacAudioFactory::new(rx, config.audio.clone()))
            as Box<dyn SoundServerFactory>)
    } else {
        None
    };

    // Clipboard factory
    let cliprdr_factory = if config.clipboard.enabled {
        Some(Box::new(MacClipboardFactory::new(config.clipboard.clone()))
            as Box<dyn CliprdrServerFactory>)
    } else {
        None
    };

    let server = RdpServer::builder()
        .with_sound_factory(sound_factory)
        .with_cliprdr_factory(cliprdr_factory)
        // ... existing config ...
        .build();

    Ok(server)
}
```

### 5.2 Configuration Extensions

```rust
#[derive(Deserialize, Clone)]
pub struct ClipboardConfig {
    pub enabled: bool,           // default: true
    pub file_transfer: bool,     // default: true
    pub max_file_size_mb: u32,   // default: 100
    pub temp_dir: Option<PathBuf>,
}

#[derive(Deserialize, Clone)]
pub struct AudioConfig {
    pub enabled: bool,           // default: true
    pub codec: AudioCodec,       // "pcm" or "opus", default: pcm
    pub sample_rate: u32,        // default: 48000
    pub channels: u16,           // default: 2
    pub buffer_ms: u32,          // default: 20
}

#[derive(Deserialize, Clone)]
pub enum AudioCodec {
    Pcm,
    Opus,
}
```

**Config TOML example:**

```toml
[clipboard]
enabled = true
file_transfer = true
max_file_size_mb = 100

[audio]
enabled = true
codec = "pcm"
sample_rate = 48000
channels = 2
buffer_ms = 20
```

---

## 6. Error Handling

| Scenario | Behavior | Impact |
|----------|----------|--------|
| NSPasteboard access denied | Log warning, skip sync cycle | Clipboard disabled gracefully |
| Format conversion failure (corrupted image/DIB) | Return empty response, log warning | Single paste fails, no crash |
| Audio capture interrupted | Stop sending Wave packets | Client goes silent, video unaffected |
| File read failure during transfer | Send FileContentsResponse with error | Client shows error dialog |
| Channel capacity full (audio back-pressure) | Drop oldest audio frame | Brief audio skip, no latency buildup |
| Clipboard echo loop detected | Anti-echo via changeCount tracking | No infinite loop |
| SCK audio permission denied | Disable audio, log error | Audio feature unavailable |
| Opus encoding failure | Fallback to PCM if possible, else skip | Degraded audio quality |

---

## 7. Testing Strategy

### 7.1 Unit Tests

- **Format conversion:** UTF-8 ↔ UTF-16LE roundtrip, DIB ↔ PNG roundtrip, HTML Format header generation/parsing
- **Audio conversion:** Float32 → S16LE correctness (boundary values: -1.0, 0.0, 1.0, overflow)
- **Interleave:** Non-interleaved → interleaved channel reordering
- **FileGroupDescriptorW:** Serialization/deserialization of file descriptor structures
- **Ring buffer:** Frame accumulation and 20ms slicing

### 7.2 Integration Tests

- **Clipboard polling:** Verify changeCount detection with mock NSPasteboard
- **Anti-echo:** Write to pasteboard, verify no re-trigger
- **Audio pipeline:** SCK AudioFrame → converter → Wave packet sizing

### 7.3 Manual End-to-End Tests

- **Windows mstsc:** Text copy/paste both directions, image paste, file drag
- **FreeRDP (Linux):** Same clipboard tests + audio playback
- **Microsoft Remote Desktop (iOS/Android):** Basic text clipboard + audio

### 7.4 Test Environment Notes

- Tests involving NSPasteboard require macOS GUI session (not headless CI)
- Audio tests need ScreenCaptureKit permission (Screen & System Audio Recording)
- `DYLD_LIBRARY_PATH` setup needed for Swift concurrency runtime (existing requirement)

---

## 8. Implementation Order

| Phase | Component | Complexity | Description |
|-------|-----------|-----------|-------------|
| 3.1 | Audio RDPSND | Medium | SCK audio capture → Float32→S16LE → RDPSND Wave |
| 3.2 | Clipboard — Text | Low | NSPasteboard text ↔ CF_UNICODETEXT, polling |
| 3.3 | Clipboard — Image | Medium | PNG/TIFF ↔ CF_DIB format conversion |
| 3.4 | Clipboard — File | High | FileGroupDescriptorW, FileContents, temp dir management |
| 3.5 | Opus Encoding | Low | Optional Opus codec behind feature flag |

Each phase is independently testable and deployable. Audio (3.1) is prioritized as it is simpler and provides immediate user-visible value.

---

## 9. Security Considerations

- **Clipboard data size limits:** Enforce `max_file_size_mb` to prevent memory exhaustion during file transfer
- **Temporary file cleanup:** Auto-delete temp files on session end or clipboard change
- **Path traversal:** Validate file paths from `FileGroupDescriptorW` — reject `..` components and absolute paths
- **Audio privacy:** `excludes_current_process_audio(true)` prevents capturing our own audio output

---

## 10. Future Work

- **AUDIN (Audio Input):** Client microphone → macOS virtual audio device. Requires implementing the AUDIN DVC protocol (not yet in IronRDP) and a virtual audio driver on macOS.
- **Clipboard File Promises:** Support for `kPasteboardTypeFileURLPromise` (lazy file generation from network sources).
- **Audio resampling:** If client only supports 44.1kHz, add a resampling step (rubato crate).
- **Clipboard HTML rendering:** Rich text with embedded images.

use anyhow::Result;

pub struct OpusEncoder {
    encoder: opus::Encoder,
    frame_size: usize,
    channels: u16,
}

impl OpusEncoder {
    pub fn new(sample_rate: u32, channels: u16) -> Result<Self> {
        let ch = match channels {
            1 => opus::Channels::Mono,
            _ => opus::Channels::Stereo,
        };
        let encoder = opus::Encoder::new(sample_rate, ch, opus::Application::Audio)?;
        let frame_size = (sample_rate / 50) as usize; // 20ms

        Ok(Self {
            encoder,
            frame_size,
            channels,
        })
    }

    /// Encode interleaved Float32 PCM to Opus frame.
    /// Input length must be frame_size * channels.
    pub fn encode_frame(&mut self, input: &[f32]) -> Result<Vec<u8>> {
        let mut output = vec![0u8; 4000]; // Max Opus frame size
        let len = self.encoder.encode_float(input, &mut output)?;
        output.truncate(len);
        Ok(output)
    }

    pub fn frame_size(&self) -> usize {
        self.frame_size
    }
}

/// Audio format conversion utilities.
pub struct AudioConverter;

impl AudioConverter {
    /// Convert a slice of f32 samples (range [-1.0, 1.0]) to 16-bit signed little-endian PCM bytes.
    ///
    /// Each f32 sample is clamped to [-1.0, 1.0], scaled by 32767, then written as two LE bytes.
    pub fn float32_to_s16le(input: &[f32]) -> Vec<u8> {
        let mut output = Vec::with_capacity(input.len() * 2);
        for &sample in input {
            let clamped = sample.clamp(-1.0, 1.0);
            let scaled = (clamped * 32767.0) as i16;
            output.extend_from_slice(&scaled.to_le_bytes());
        }
        output
    }

    /// Interleave non-interleaved channel buffers.
    ///
    /// Given `buffers` where each element is a channel's samples, produce a single interleaved
    /// buffer of length `num_channels * num_samples`.
    ///
    /// Example: buffers = [[L0, L1, L2], [R0, R1, R2]] → [L0, R0, L1, R1, L2, R2]
    pub fn interleave(buffers: &[&[f32]], num_samples: usize) -> Vec<f32> {
        let num_channels = buffers.len();
        let mut output = Vec::with_capacity(num_channels * num_samples);
        for i in 0..num_samples {
            for channel in buffers {
                output.push(channel[i]);
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    #[test]
    fn float32_to_s16le_silence() {
        let input = vec![0.0f32; 4];
        let output = AudioConverter::float32_to_s16le(&input);
        assert_eq!(output.len(), 8);
        assert!(output.iter().all(|&b| b == 0));
    }

    #[test]
    fn float32_to_s16le_max_positive() {
        let output = AudioConverter::float32_to_s16le(&[1.0f32]);
        let value = i16::from_le_bytes([output[0], output[1]]);
        assert_eq!(value, 32767);
    }

    #[test]
    fn float32_to_s16le_max_negative() {
        let output = AudioConverter::float32_to_s16le(&[-1.0f32]);
        let value = i16::from_le_bytes([output[0], output[1]]);
        assert_eq!(value, -32767);
    }

    #[test]
    fn float32_to_s16le_clamps_overflow() {
        let output = AudioConverter::float32_to_s16le(&[2.0f32, -3.0f32]);
        let v0 = i16::from_le_bytes([output[0], output[1]]);
        let v1 = i16::from_le_bytes([output[2], output[3]]);
        assert_eq!(v0, 32767);
        assert_eq!(v1, -32767);
    }

    #[test]
    fn interleave_stereo() {
        let left = [0.1f32, 0.2, 0.3];
        let right = [0.4f32, 0.5, 0.6];
        let result = AudioConverter::interleave(&[&left, &right], 3);
        assert_eq!(result.len(), 6);
        assert_abs_diff_eq!(result[0], 0.1, epsilon = 1e-6);
        assert_abs_diff_eq!(result[1], 0.4, epsilon = 1e-6);
        assert_abs_diff_eq!(result[2], 0.2, epsilon = 1e-6);
        assert_abs_diff_eq!(result[3], 0.5, epsilon = 1e-6);
        assert_abs_diff_eq!(result[4], 0.3, epsilon = 1e-6);
        assert_abs_diff_eq!(result[5], 0.6, epsilon = 1e-6);
    }

    #[test]
    fn interleave_mono() {
        let mono = [0.1f32, 0.2, 0.3];
        let result = AudioConverter::interleave(&[&mono], 3);
        assert_eq!(result.len(), 3);
        assert_abs_diff_eq!(result[0], 0.1, epsilon = 1e-6);
        assert_abs_diff_eq!(result[1], 0.2, epsilon = 1e-6);
        assert_abs_diff_eq!(result[2], 0.3, epsilon = 1e-6);
    }
}

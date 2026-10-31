pub const MIN_SAMPLE_RATE: f64 = 8_000.0;
pub const MAX_SAMPLE_RATE: f64 = 192_000.0;
pub const MAX_CHANNELS: usize = 32;

#[derive(Clone, Copy)]
pub enum SampleFormat {
    Pcm { bytes: u8, valid_bits: u8 },
    Float32,
    Float64,
}

impl SampleFormat {
    pub fn pcm(bytes: u32, valid_bits: u32) -> Option<Self> {
        if (1..=4).contains(&bytes) && valid_bits > 0 && valid_bits <= bytes * 8 {
            Some(Self::Pcm {
                bytes: bytes as u8,
                valid_bits: valid_bits as u8,
            })
        } else {
            None
        }
    }

    pub fn bytes(self) -> usize {
        match self {
            Self::Pcm { bytes, .. } => bytes as usize,
            Self::Float32 => 4,
            Self::Float64 => 8,
        }
    }

    /// # Panics
    /// Panics if `sample.len()` does not equal `self.bytes()`.
    pub fn decode(self, sample: &[u8]) -> f64 {
        let value = match self {
            Self::Float32 => {
                f64::from(f32::from_le_bytes(sample.try_into().expect(
                    "UNREACHABLE: Float32 sample should contain exactly 4 bytes",
                )))
            }
            Self::Float64 => f64::from_le_bytes(
                sample
                    .try_into()
                    .expect("UNREACHABLE: Float64 sample should contain exactly 8 bytes"),
            ),
            Self::Pcm { bytes, valid_bits } => {
                let input = if bytes == 1 {
                    i32::from(sample[0]) - 128
                } else {
                    let mut encoded = [0; 4];
                    encoded[..bytes as usize].copy_from_slice(sample);
                    let extension = (4 - bytes) * 8;
                    (i32::from_le_bytes(encoded) << extension) >> extension
                } >> (bytes * 8 - valid_bits);
                f64::from(input) / (1_i64 << (valid_bits - 1)) as f64
            }
        };
        if value.is_finite() {
            value.clamp(-1.0, 1.0)
        } else {
            0.0
        }
    }

    pub fn encode(self, sample: &mut [u8], value: f64) {
        let value = if value.is_finite() {
            value.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        match self {
            Self::Float32 => sample.copy_from_slice(&(value as f32).to_le_bytes()),
            Self::Float64 => sample.copy_from_slice(&value.to_le_bytes()),
            Self::Pcm { bytes, valid_bits } => {
                let limit = (1_i64 << (valid_bits - 1)) as f64;
                let output = ((value * limit).round().clamp(-limit, limit - 1.0) as i64)
                    << (bytes * 8 - valid_bits);
                if bytes == 1 {
                    sample[0] = (output + 128) as u8;
                } else {
                    sample.copy_from_slice(&(output as i32).to_le_bytes()[..bytes as usize]);
                }
            }
        }
    }
}

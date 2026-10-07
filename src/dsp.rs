//! Real-time audio processing, independent of COM and Windows.

/// A fully wet ring modulator makes speech sound conspicuously robotic.
pub const ROBOT_FREQUENCY_HZ: f64 = 80.0;

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

    fn bytes(self) -> usize {
        match self {
            Self::Pcm { bytes, .. } => bytes as usize,
            Self::Float32 => 4,
            Self::Float64 => 8,
        }
    }

    fn modulate(self, sample: &mut [u8], carrier: f64) {
        match self {
            Self::Float32 => {
                let input = f32::from_le_bytes(sample.try_into().unwrap());
                let output = if input.is_finite() {
                    input * carrier as f32
                } else {
                    0.0
                };
                sample.copy_from_slice(&output.to_le_bytes());
            }
            Self::Float64 => {
                let input = f64::from_le_bytes(sample.try_into().unwrap());
                let output = if input.is_finite() {
                    input * carrier
                } else {
                    0.0
                };
                sample.copy_from_slice(&output.to_le_bytes());
            }
            Self::Pcm { bytes, valid_bits } => {
                let padding = bytes * 8 - valid_bits;
                let input = if bytes == 1 {
                    i32::from(sample[0]) - 128
                } else {
                    let mut encoded = [0; 4];
                    encoded[..bytes as usize].copy_from_slice(sample);
                    let extension = (4 - bytes) * 8;
                    (i32::from_le_bytes(encoded) << extension) >> extension
                } >> padding;
                let limit = (1_i64 << (valid_bits - 1)) as f64;
                let output = (f64::from(input) * carrier)
                    .round()
                    .clamp(-limit, limit - 1.0) as i32;
                let output = output << padding;
                if bytes == 1 {
                    sample[0] = (output + 128) as u8;
                } else {
                    sample.copy_from_slice(&output.to_le_bytes()[..bytes as usize]);
                }
            }
        }
    }
}

/// In-place robot voice with no audio buffering or runtime allocation.
#[derive(Default)]
pub struct VoiceChanger {
    format: Option<SampleFormat>,
    frame_bytes: usize,
    phase: f64,
    phase_step: f64,
}

impl VoiceChanger {
    pub fn configure(
        &mut self,
        format: SampleFormat,
        channels: usize,
        sample_rate: f64,
    ) -> Result<(), ()> {
        let frame_bytes = channels.checked_mul(format.bytes()).ok_or(())?;
        if frame_bytes == 0 || !sample_rate.is_finite() || sample_rate <= ROBOT_FREQUENCY_HZ * 2.0 {
            return Err(());
        }
        self.format = Some(format);
        self.frame_bytes = frame_bytes;
        self.phase_step = ROBOT_FREQUENCY_HZ / sample_rate;
        self.reset();
        Ok(())
    }

    pub fn reset(&mut self) {
        self.phase = 0.0;
    }

    pub fn silence_byte(&self) -> u8 {
        if matches!(self.format, Some(SampleFormat::Pcm { bytes: 1, .. })) {
            128
        } else {
            0
        }
    }

    pub fn frame_bytes(&self) -> usize {
        self.frame_bytes
    }

    pub fn process(&mut self, buffer: &mut [u8]) {
        let Some(format) = self.format else { return };
        for frame in buffer.chunks_exact_mut(self.frame_bytes) {
            // All channels share a carrier; phase continues across APO buffers.
            let carrier = (core::f64::consts::TAU * self.phase).cos();
            for sample in frame.chunks_exact_mut(format.bytes()) {
                format.modulate(sample, carrier);
            }
            self.phase += self.phase_step;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
        }
    }
}

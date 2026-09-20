//! Real-time audio processing, independent of COM and Windows.

/// The fixed delay introduced by VoiceBox.
pub const DELAY_SECONDS: f64 = 5.0;

/// A format-agnostic, allocation-free-at-runtime delay line.
///
/// Audio frames are stored as opaque bytes. This lets the processor preserve
/// every sample representation and channel layout without converting it.
#[derive(Default)]
pub struct Delay {
    buffer: Vec<u8>,
    cursor: usize,
    frame_bytes: usize,
    silence_byte: u8,
}

impl Delay {
    /// Allocate and initialize the delay line. This must be called off the
    /// real-time thread, before [`process`](Self::process).
    pub fn configure(
        &mut self,
        frame_bytes: usize,
        delay_frames: usize,
        silence_byte: u8,
    ) -> Result<(), ()> {
        let buffer_bytes = frame_bytes.checked_mul(delay_frames).ok_or(())?;
        if frame_bytes == 0 || buffer_bytes == 0 {
            return Err(());
        }

        let mut buffer = Vec::new();
        buffer.try_reserve_exact(buffer_bytes).map_err(|_| ())?;
        buffer.resize(buffer_bytes, silence_byte);

        self.buffer = buffer;
        self.cursor = 0;
        self.frame_bytes = frame_bytes;
        self.silence_byte = silence_byte;
        Ok(())
    }

    /// Restore the configured delay line to five seconds of silence.
    pub fn reset(&mut self) {
        self.buffer.fill(self.silence_byte);
        self.cursor = 0;
    }

    pub fn silence_byte(&self) -> u8 {
        self.silence_byte
    }

    pub fn frame_bytes(&self) -> usize {
        self.frame_bytes
    }

    /// Replace each byte with the byte received exactly one delay-line length
    /// earlier. Complete frames stay aligned because configuration and callers
    /// always supply frame-sized buffers.
    #[inline]
    pub fn process(&mut self, buffer: &mut [u8]) {
        if self.buffer.is_empty() || self.frame_bytes == 0 {
            return;
        }

        for byte in buffer {
            core::mem::swap(byte, &mut self.buffer[self.cursor]);
            self.cursor += 1;
            if self.cursor == self.buffer.len() {
                self.cursor = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_arbitrary_frames_without_changing_their_bytes() {
        let mut delay = Delay::default();
        delay.configure(3, 2, 0).unwrap();

        let mut first = [1, 2, 3, 4, 5, 6];
        delay.process(&mut first);
        assert_eq!(first, [0; 6]);

        let mut second = [7, 8, 9];
        delay.process(&mut second);
        assert_eq!(second, [1, 2, 3]);

        let mut third = [10, 11, 12];
        delay.process(&mut third);
        assert_eq!(third, [4, 5, 6]);
    }

    #[test]
    fn reset_restores_silence_and_position() {
        let mut delay = Delay::default();
        delay.configure(1, 2, 0x80).unwrap();
        let mut samples = [1, 2];
        delay.process(&mut samples);

        delay.reset();
        let mut sample = [3];
        delay.process(&mut sample);
        assert_eq!(sample, [0x80]);
    }
}

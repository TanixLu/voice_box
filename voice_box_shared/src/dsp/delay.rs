//! Delay each microphone frame together with its capture-time gain.

pub(super) struct FrameDelay {
    frames: Vec<f32>,
    stride: usize,
    position: usize,
}

impl FrameDelay {
    pub(super) fn new(latency_frames: usize, channels: usize) -> Self {
        let stride = channels + 1;
        Self {
            frames: vec![0.0; latency_frames * stride],
            stride,
            position: 0,
        }
    }

    pub(super) fn reset(&mut self) {
        self.frames.fill(0.0);
        self.position = 0;
    }

    /// Replace input audio with delayed audio and return its matching gain.
    pub(super) fn process_frame(&mut self, audio: &mut [f32], gain: f32) -> f32 {
        debug_assert_eq!(audio.len() + 1, self.stride);
        let frame = &mut self.frames[self.position..self.position + self.stride];
        audio.swap_with_slice(&mut frame[..audio.len()]);
        let delayed_gain = std::mem::replace(&mut frame[audio.len()], gain);
        self.position += self.stride;
        if self.position == self.frames.len() {
            self.position = 0;
        }
        delayed_gain
    }
}

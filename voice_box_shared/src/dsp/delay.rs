//! Capture-time gain history for the denoiser's fixed latency.

pub(super) struct GainDelay {
    frames: Vec<f32>,
    write: usize,
}

impl GainDelay {
    pub(super) fn new(latency: usize) -> Self {
        Self {
            frames: vec![0.0; latency + 1],
            write: 0,
        }
    }

    pub(super) fn reset(&mut self) {
        self.frames.fill(0.0);
        self.write = 0;
    }

    pub(super) fn process_frame(&mut self, gain: f32) -> f32 {
        self.frames[self.write] = gain;
        self.write = (self.write + 1) % self.frames.len();
        self.frames[self.write]
    }
}

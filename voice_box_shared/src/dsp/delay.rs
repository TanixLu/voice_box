//! A bounded, frame-clocked variable delay shared by all microphone channels.

pub(super) struct FrameDelay {
    frames: Vec<f32>,
    stride: usize,
    write: usize,
    maximum: usize,
    delay: f64,
    speed: f64,
    max_speed: f64,
    smoothing: f64,
}

impl FrameDelay {
    // Full-range travel time, before the short speed smoothing at either end.
    const TRANSITION_SECONDS: f64 = 1.0;

    pub(super) fn new(maximum: usize, channels: usize, sample_rate: f64) -> Self {
        let stride = channels + 1;
        Self {
            frames: vec![0.0; (maximum + 2) * stride],
            stride,
            write: 0,
            maximum,
            delay: 0.0,
            speed: 0.0,
            max_speed: maximum as f64 / (sample_rate * Self::TRANSITION_SECONDS),
            smoothing: -(-1.0 / (sample_rate * super::RAMP_SECONDS)).exp_m1(),
        }
    }

    pub(super) fn reset(&mut self, delayed: bool) {
        self.frames.fill(0.0);
        self.write = 0;
        self.delay = if delayed { self.maximum as f64 } else { 0.0 };
        self.speed = 0.0;
    }

    pub(super) fn is_full(&self) -> bool {
        self.delay == self.maximum as f64
    }

    /// Store capture-time gain with the audio. Retargeting never clears history.
    /// Linear interpolation is causal even at zero delay and handles one-frame
    /// callbacks exactly like larger blocks. Stable integer delays are exact.
    pub(super) fn process_frame(&mut self, audio: &mut [f32], gain: f32, delayed: bool) -> f32 {
        debug_assert_eq!(audio.len() + 1, self.stride);
        let target = if delayed { self.maximum as f64 } else { 0.0 };
        let error = target - self.delay;
        let speed = error.clamp(-self.max_speed, self.max_speed);
        self.speed += self.smoothing * (speed - self.speed);
        self.delay = (self.delay + self.speed).clamp(0.0, self.maximum as f64);
        if error.abs() <= self.max_speed && (target - self.delay).abs() <= self.max_speed {
            self.delay = target;
            self.speed = 0.0;
        }
        let frame = &mut self.frames[self.write..self.write + self.stride];
        for (stored, &sample) in frame.iter_mut().zip(audio.iter()) {
            *stored = sample * gain;
        }
        frame[audio.len()] = gain;

        let whole = self.delay.floor() as usize;
        let fraction = self.delay.fract() as f32;
        let capacity = self.frames.len();
        let recent = (self.write + capacity - whole * self.stride) % capacity;
        let older = (recent + capacity - self.stride) % capacity;
        for (channel, sample) in audio.iter_mut().enumerate() {
            let a = self.frames[recent + channel];
            *sample = a + (self.frames[older + channel] - a) * fraction;
        }
        let a = self.frames[recent + audio.len()];
        let delayed_gain = a + (self.frames[older + audio.len()] - a) * fraction;
        self.write = (self.write + self.stride) % capacity;
        delayed_gain
    }
}

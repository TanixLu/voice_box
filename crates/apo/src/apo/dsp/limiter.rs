//! Linked-channel sample-peak limiting after resampling and user gain.

const LOOKAHEAD_SECONDS: f64 = 0.002;
const RELEASE_SECONDS: f64 = 0.050;
// Round down slightly from 10^(-1 / 20) to leave room for f32 rounding.
const CEILING: f32 = 0.891_250_9;
const TINY: f32 = 1.0e-20;

pub(super) struct Limiter {
    channels: usize,
    delay_frames: usize,
    delay: Vec<f32>,
    delay_position: usize,
    window_frames: usize,
    window_position: usize,
    leaves: usize,
    peaks: Vec<f32>,
    gains: Vec<f64>,
    gain: f64,
    release_step: f64,
}

impl Limiter {
    /// Formats and resource bounds have already been validated by prepare.
    pub(super) fn new(sample_rate: f64, channels: usize) -> Self {
        debug_assert!(sample_rate.is_finite() && sample_rate > 0.0 && channels > 0);
        let delay_frames = (sample_rate * LOOKAHEAD_SECONDS).ceil() as usize;
        let window_frames = delay_frames + 1;
        let leaves = window_frames.next_power_of_two();
        let mut limiter = Self {
            channels,
            delay_frames,
            delay: vec![0.0; delay_frames * channels],
            delay_position: 0,
            window_frames,
            window_position: 0,
            leaves,
            peaks: vec![0.0; 2 * leaves],
            gains: vec![0.0; 2 * leaves],
            gain: 1.0,
            release_step: -(-1.0 / (sample_rate * RELEASE_SECONDS)).exp_m1(),
        };
        limiter.reset();
        limiter
    }

    pub(super) fn reset(&mut self) {
        self.delay.fill(0.0);
        self.delay_position = 0;
        self.window_position = 0;
        self.peaks.fill(0.0);
        self.gains.fill(0.0);
        self.gains[self.leaves..self.leaves + self.window_frames].fill(1.0);
        for node in (1..self.leaves).rev() {
            self.gains[node] = self.gains[2 * node] + self.gains[2 * node + 1];
        }
        self.gain = 1.0;
    }

    pub(super) fn latency_frames(&self) -> usize {
        self.delay_frames
    }

    pub(super) fn process_frame(&mut self, frame: &mut [f32]) {
        debug_assert_eq!(frame.len(), self.channels);
        let mut peak = 0.0f32;
        for sample in frame.iter_mut() {
            if !sample.is_finite() || sample.abs() < TINY {
                *sample = 0.0;
            }
            peak = peak.max(sample.abs());
        }

        let leaf = self.leaves + self.window_position;
        self.peaks[leaf] = peak;
        let mut node = leaf / 2;
        while node != 0 {
            self.peaks[node] = self.peaks[2 * node].max(self.peaks[2 * node + 1]);
            node /= 2;
        }
        self.gains[leaf] = if self.peaks[1] > CEILING {
            f64::from(CEILING) / f64::from(self.peaks[1])
        } else {
            1.0
        };
        let mut node = leaf / 2;
        while node != 0 {
            self.gains[node] = self.gains[2 * node] + self.gains[2 * node + 1];
            node /= 2;
        }

        // Each of these N=L+1 gains comes from a peak window containing the
        // delayed output frame x[n-L]. Their mean is therefore also a safe
        // gain for that frame, with a linear L-frame attack ahead of a peak.
        // Fixed-depth trees bound work per frame and avoid rolling-sum drift.
        let safe_gain = (self.gains[1] / self.window_frames as f64).clamp(0.0, 1.0);
        let released_gain = self.gain + (1.0 - self.gain) * self.release_step;
        self.gain = released_gain.min(safe_gain);

        let start = self.delay_position * self.channels;
        for (channel, sample) in frame.iter_mut().enumerate() {
            let delayed = self.delay[start + channel];
            self.delay[start + channel] = *sample;
            let output = f64::from(delayed) * self.gain;
            // Only a rounding backstop; the lookahead envelope limits peaks.
            // Flush before narrowing so the conversion cannot create a
            // subnormal f32 while recovering from extreme input peaks.
            *sample = if output.abs() < f64::from(TINY) {
                0.0
            } else {
                (output as f32).clamp(-CEILING, CEILING)
            };
        }

        self.delay_position += 1;
        if self.delay_position == self.delay_frames {
            self.delay_position = 0;
        }
        self.window_position += 1;
        if self.window_position == self.window_frames {
            self.window_position = 0;
        }
    }
}

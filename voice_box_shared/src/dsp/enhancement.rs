//! Denoising lifecycle and dry/wet transitions, independent of microphone gain.

use super::{RAMP_SECONDS, channel::Channel, ramp::Ramp};

#[derive(Clone, Copy)]
enum State {
    Off,
    Warming { remaining: usize },
    Running,
}

pub(super) struct Enhancement {
    channels: Vec<Channel>,
    state: State,
    wet: Ramp,
    latency_frames: usize,
}

impl Enhancement {
    pub(super) fn new(sample_rate: f64, channels: usize, enabled: bool) -> Self {
        let channels: Vec<_> = (0..channels).map(|_| Channel::new(sample_rate)).collect();
        let latency_frames = channels[0].delay_frames(sample_rate).ceil() as usize;
        let mut enhancement = Self {
            channels,
            state: State::Off,
            wet: Ramp::new(0.0, (sample_rate * RAMP_SECONDS).ceil() as usize),
            latency_frames,
        };
        enhancement.reset(enabled);
        enhancement
    }

    pub(super) fn channels(&self) -> usize {
        self.channels.len()
    }

    pub(super) fn latency_frames(&self) -> usize {
        self.latency_frames
    }

    fn start(&mut self) {
        for channel in &mut self.channels {
            channel.reset();
        }
        self.state = State::Warming {
            remaining: self.latency_frames,
        };
    }

    /// Stream initialization selects the configured path without a user fade.
    pub(super) fn reset(&mut self, enabled: bool) {
        self.wet.reset(if enabled { 1.0 } else { 0.0 });
        if enabled {
            self.start();
        } else {
            self.state = State::Off;
        }
    }

    pub(super) fn set_enabled(&mut self, enabled: bool) {
        self.wet.set_target(if enabled { 1.0 } else { 0.0 });
        match (self.state, enabled) {
            (State::Off, true) => self.start(),
            (State::Warming { .. }, false) => {
                // No wet audio has been exposed yet; cancellation needs no fade.
                self.state = State::Off;
                self.wet.reset(0.0);
            }
            _ => {}
        }
    }

    /// Return the wet mix for this frame. The caller supplies aligned dry audio.
    pub(super) fn process_frame(&mut self, input: &[f32], output: &mut [f32]) -> f32 {
        let wet = match self.state {
            State::Off => {
                output.fill(0.0);
                return 0.0;
            }
            State::Warming { remaining } => {
                self.state = if remaining == 1 {
                    State::Running
                } else {
                    State::Warming {
                        remaining: remaining - 1,
                    }
                };
                0.0
            }
            State::Running => {
                let wet = self.wet.next();
                if wet == 0.0 {
                    self.state = State::Off;
                    output.fill(0.0);
                    return 0.0;
                }
                wet
            }
        };
        for ((&sample, channel), output) in input.iter().zip(&mut self.channels).zip(output) {
            *output = channel.process_sample(sample);
        }
        wet
    }
}

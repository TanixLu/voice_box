//! Denoising lifecycle and bypass fades; the model controls attenuation strength.

use super::{RAMP_SECONDS, channel::Channel, ramp::Ramp};
use crate::apo_config::NoiseReductionDb;

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
    strength: NoiseReductionDb,
}

impl Enhancement {
    pub(super) fn new(sample_rate: f64, channels: usize, strength: NoiseReductionDb) -> Self {
        let channels: Vec<_> = (0..channels).map(|_| Channel::new(sample_rate)).collect();
        let latency_frames = channels[0].delay_frames(sample_rate).ceil() as usize;
        let mut enhancement = Self {
            channels,
            state: State::Off,
            wet: Ramp::new(0.0, (sample_rate * RAMP_SECONDS).ceil() as usize),
            latency_frames,
            strength,
        };
        enhancement.reset(strength);
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
            channel.reset(self.strength);
        }
        self.state = State::Warming {
            remaining: self.latency_frames,
        };
    }

    /// Stream initialization selects the configured path without a user fade.
    pub(super) fn reset(&mut self, strength: NoiseReductionDb) {
        self.strength = strength;
        self.state = State::Off;
        self.set_strength(strength);
        self.wet
            .reset(if strength.is_enabled() { 1.0 } else { 0.0 });
    }

    pub(super) fn set_strength(&mut self, strength: NoiseReductionDb) {
        if self.strength != strength {
            self.strength = strength;
            for channel in &mut self.channels {
                channel.set_strength(strength);
            }
        }
        let enabled = strength.is_enabled();
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

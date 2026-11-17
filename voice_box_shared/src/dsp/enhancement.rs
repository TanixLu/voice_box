//! Denoiser lifetime and warmup, independent of output fades and mixing.

use super::channel::Channel;
use crate::apo_config::NoiseReductionDb;
use dfn3_rs::Model;

const KEEP_WARM_SECONDS: f64 = 0.020;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Prepared,
    Running,
    Stopped,
}

pub(super) struct Enhancement {
    channels: Vec<Channel>,
    latency_frames: usize,
    warmup_remaining: usize,
    state: State,
    keep_warm_frames: usize,
    keep_warm_remaining: usize,
    strength: NoiseReductionDb,
}

impl Enhancement {
    pub(super) fn new(
        sample_rate: f64,
        channels: usize,
        model: Model,
        strength: NoiseReductionDb,
    ) -> Self {
        let channels: Vec<_> = (0..channels)
            .map(|_| Channel::new(sample_rate, model))
            .collect();
        let latency_frames = Channel::latency_frames(sample_rate);
        let mut enhancement = Self {
            channels,
            latency_frames,
            warmup_remaining: 0,
            state: State::Prepared,
            keep_warm_frames: (sample_rate * KEEP_WARM_SECONDS).ceil() as usize,
            keep_warm_remaining: 0,
            strength,
        };
        // Prepare every channel off the real-time thread, including bypassed streams.
        enhancement.reset(strength);
        enhancement
    }

    pub(super) fn latency_frames(&self) -> usize {
        self.latency_frames
    }

    pub(super) fn reset(&mut self, strength: NoiseReductionDb) {
        for channel in &mut self.channels {
            channel.reset(strength);
        }
        self.strength = strength;
        self.state = State::Prepared;
        self.warmup_remaining = self.latency_frames;
        self.keep_warm_remaining = 0;
    }

    pub(super) fn set_strength(&mut self, strength: NoiseReductionDb) {
        // Keep the previous attenuation during an off fade. Zero means bypass,
        // not a request to alter the model while its last output is audible.
        if strength.is_enabled() && self.strength != strength {
            self.strength = strength;
            for channel in &mut self.channels {
                channel.set_strength(strength);
            }
        }
    }

    pub(super) fn process_frame(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        active: bool,
    ) -> bool {
        // Brief off pulses keep the model warm rather than repeatedly resetting
        // all channels on the real-time thread. Long-term bypass still stops it.
        if active {
            if self.state == State::Stopped {
                self.reset(self.strength);
            }
            self.keep_warm_remaining = self.keep_warm_frames;
        } else if self.state != State::Running || self.keep_warm_remaining == 0 {
            // Only channels that have processed input need a reset on reactivation.
            if self.state == State::Running {
                self.state = State::Stopped;
            }
            output.fill(0.0);
            return false;
        } else {
            self.keep_warm_remaining -= 1;
        }
        self.state = State::Running;
        for ((&sample, channel), output) in input.iter().zip(&mut self.channels).zip(output) {
            *output = channel.process_sample(sample);
        }
        if self.warmup_remaining != 0 {
            self.warmup_remaining -= 1;
            return false;
        }
        true
    }
}

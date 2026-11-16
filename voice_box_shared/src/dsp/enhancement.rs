//! Denoiser lifetime and warmup, independent of output retiming and mixing.

use super::channel::Channel;
use crate::apo_config::NoiseReductionDb;
use dfn3_rs::Model;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Running,
    Stopped,
}

pub(super) struct Enhancement {
    channels: Vec<Channel>,
    latency_frames: usize,
    remaining: usize,
    state: State,
    idle_frames: usize,
    idle_remaining: usize,
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
            remaining: 0,
            state: State::Ready,
            idle_frames: (sample_rate * super::RAMP_SECONDS).ceil() as usize,
            idle_remaining: 0,
            strength,
        };
        // Prepare every channel off the real-time thread, including bypassed streams.
        enhancement.reset(strength);
        enhancement
    }

    pub(super) fn channels(&self) -> usize {
        self.channels.len()
    }
    pub(super) fn latency_frames(&self) -> usize {
        self.latency_frames
    }

    pub(super) fn reset(&mut self, strength: NoiseReductionDb) {
        for channel in &mut self.channels {
            channel.reset(strength);
        }
        self.strength = strength;
        self.state = State::Ready;
        self.remaining = self.latency_frames;
        self.idle_remaining = 0;
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
            self.idle_remaining = self.idle_frames;
        } else if self.state != State::Running || self.idle_remaining == 0 {
            // An unused, prepared channel stays ready however long bypass lasts.
            if self.state == State::Running {
                self.state = State::Stopped;
            }
            output.fill(0.0);
            return false;
        } else {
            self.idle_remaining -= 1;
        }
        self.state = State::Running;
        for ((&sample, channel), output) in input.iter().zip(&mut self.channels).zip(output) {
            *output = channel.process_sample(sample);
        }
        if self.remaining != 0 {
            self.remaining -= 1;
            return false;
        }
        true
    }
}

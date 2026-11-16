//! One channel's fixed-rate enhancement and host-rate output queue.

use dfn3_rs::{ALGORITHM_DELAY, Denoiser, FRAME_SIZE, Model, SAMPLE_RATE};
use std::time::Duration;

use super::{
    RAMP_SECONDS, StreamingResampler, finite_sample, high_pass::HighPass, pre_gain::PreGain,
};
use crate::apo_config::NoiseReductionDb;

pub(super) struct Channel {
    denoiser: Box<Denoiser>,
    high_pass: HighPass,
    pre_gain: PreGain,
    input_resampler: StreamingResampler,
    output_resampler: StreamingResampler,
    input: [f32; FRAME_SIZE],
    output: [f32; FRAME_SIZE],
    pending: usize,
    queue: Vec<f32>,
    read: usize,
    write: usize,
    queued: usize,
    prefill: usize,
}

impl Channel {
    pub(super) fn new(sample_rate: f64, model: Model) -> Self {
        let rate = f64::from(SAMPLE_RATE);
        let input_resampler = StreamingResampler::new(sample_rate, rate);
        let output_resampler = StreamingResampler::new(rate, sample_rate);
        let prefill = Self::prefill_frames(sample_rate);
        Self {
            denoiser: Box::new(Denoiser::with_model(model)),
            high_pass: HighPass::new(rate),
            pre_gain: PreGain::new(),
            input_resampler,
            output_resampler,
            input: [0.0; FRAME_SIZE],
            output: [0.0; FRAME_SIZE],
            pending: 0,
            queue: vec![0.0; 2 * prefill + 8],
            read: 0,
            write: prefill,
            queued: prefill,
            prefill,
        }
    }

    fn prefill_frames(sample_rate: f64) -> usize {
        let rate = f64::from(SAMPLE_RATE);
        ((FRAME_SIZE + StreamingResampler::input_chunk_size(rate, sample_rate)) as f64
            * sample_rate
            / rate)
            .ceil() as usize
            + StreamingResampler::input_chunk_size(sample_rate, rate)
            + 2
    }

    pub(super) fn latency_frames(sample_rate: f64) -> usize {
        let rate = f64::from(SAMPLE_RATE);
        (Self::prefill_frames(sample_rate) as f64
            + (ALGORITHM_DELAY + FRAME_SIZE) as f64 * sample_rate / rate
            + (StreamingResampler::delay_seconds(sample_rate, rate)
                + StreamingResampler::delay_seconds(rate, sample_rate))
                * sample_rate)
            .ceil() as usize
    }

    pub(super) fn reset(&mut self, strength: NoiseReductionDb) {
        self.denoiser
            .set_attenuation_limit(f32::from(strength.as_u8()));
        self.denoiser.reset();
        self.high_pass.reset();
        self.pre_gain.reset();
        self.input_resampler.reset();
        self.output_resampler.reset();
        self.input.fill(0.0);
        self.output.fill(0.0);
        self.pending = 0;
        self.queue.fill(0.0);
        self.read = 0;
        self.write = self.prefill;
        self.queued = self.prefill;
    }

    pub(super) fn set_strength(&mut self, strength: NoiseReductionDb) {
        self.denoiser.set_attenuation_limit_smooth(
            f32::from(strength.as_u8()),
            Duration::from_secs_f64(RAMP_SECONDS),
        );
    }

    pub(super) fn process_sample(&mut self, input: f32) -> f32 {
        self.input_resampler.push(input);
        while let Some(sample) = self.input_resampler.next_sample() {
            self.input[self.pending] = self.high_pass.process_sample(sample);
            self.pending += 1;
            if self.pending == FRAME_SIZE {
                self.pre_gain.prepare(&mut self.input);
                self.denoiser.process(&self.input, &mut self.output);
                self.pre_gain.restore(&mut self.output);
                self.pending = 0;
                for &sample in &self.output {
                    self.output_resampler.push(finite_sample(sample));
                    while let Some(sample) = self.output_resampler.next_sample() {
                        debug_assert!(self.queued < self.queue.len());
                        self.queue[self.write] = sample;
                        self.write = (self.write + 1) % self.queue.len();
                        self.queued += 1;
                    }
                }
            }
        }
        debug_assert!(self.queued > 0);
        if self.queued == 0 {
            return 0.0;
        }
        let sample = self.queue[self.read];
        self.read = (self.read + 1) % self.queue.len();
        self.queued -= 1;
        sample
    }
}

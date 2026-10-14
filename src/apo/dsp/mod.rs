//! Single-threaded, preallocated UL-UNAS audio processing, independent of COM.

mod high_pass;
mod limiter;
mod simd;
mod ul_unas;

pub use crate::shared::audio::SampleFormat;
use crate::shared::resample::Resampler;
use high_pass::HighPass;
use limiter::Limiter;
use ul_unas::{HOP_SIZE, LATENCY_SAMPLES, SAMPLE_RATE, UlUnas};

pub const MIN_SAMPLE_RATE: f64 = 8_000.0;
pub const MAX_SAMPLE_RATE: f64 = 192_000.0;
pub const MAX_CHANNELS: usize = 32;

struct Channel {
    model: Box<UlUnas>,
    high_pass: HighPass,
    down: Resampler,
    up: Resampler,
    input: [f32; HOP_SIZE],
    output: [f32; HOP_SIZE],
    pending: usize,
    queue: Vec<f32>,
    read: usize,
    write: usize,
    queued: usize,
    prefill: usize,
}

impl Channel {
    fn new(sample_rate: f64) -> Self {
        let rate = f64::from(SAMPLE_RATE);
        let down = Resampler::new(sample_rate, rate);
        let up = Resampler::new(rate, sample_rate);
        let prefill = ((HOP_SIZE + up.input_chunk()) as f64 * sample_rate / rate).ceil() as usize
            + down.input_chunk()
            + 2;
        let mut channel = Self {
            model: Box::new(UlUnas::new()),
            high_pass: HighPass::new(rate),
            down,
            up,
            input: [0.0; HOP_SIZE],
            output: [0.0; HOP_SIZE],
            pending: 0,
            queue: vec![0.0; 2 * prefill + 8],
            read: 0,
            write: prefill,
            queued: prefill,
            prefill,
        };
        channel.reset();
        channel
    }

    fn delay_frames(&self, sample_rate: f64) -> f64 {
        self.prefill as f64
            + LATENCY_SAMPLES as f64 * sample_rate / f64::from(SAMPLE_RATE)
            + (self.down.delay_seconds() + self.up.delay_seconds()) * sample_rate
    }

    fn reset(&mut self) {
        self.model.reset();
        self.high_pass.reset();
        self.down.reset();
        self.up.reset();
        self.input.fill(0.0);
        self.output.fill(0.0);
        self.pending = 0;
        self.queue.fill(0.0);
        self.read = 0;
        self.write = self.prefill;
        self.queued = self.prefill;
    }

    fn process_sample(&mut self, input: f32) -> f32 {
        self.down.push(input);
        while let Some(sample) = self.down.next() {
            self.input[self.pending] = self.high_pass.process_sample(sample);
            self.pending += 1;
            if self.pending == HOP_SIZE {
                self.model.process(&self.input, &mut self.output);
                self.pending = 0;
                for &sample in &self.output {
                    self.up.push(if sample.is_finite() { sample } else { 0.0 });
                    while let Some(sample) = self.up.next() {
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

/// Each channel has one UL-UNAS instance, prepared during LockForProcess.
#[derive(Default)]
pub struct AudioProcessor {
    format: Option<SampleFormat>,
    frame_bytes: usize,
    channels: Vec<Channel>,
    limiter: Option<Limiter>,
    latency_hns: i64,
    muted: bool,
}

impl AudioProcessor {
    pub fn prepare(
        &mut self,
        format: SampleFormat,
        channels: usize,
        sample_rate: f64,
        process_effect: bool,
    ) -> Result<(), ()> {
        if !(1..=MAX_CHANNELS).contains(&channels)
            || !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate)
        {
            return Err(());
        }
        let frame_bytes = channels.checked_mul(format.bytes()).ok_or(())?;
        let limiter = process_effect.then(|| Limiter::new(sample_rate, channels));
        let mut processors = Vec::with_capacity(if process_effect { channels } else { 0 });
        let mut latency_frames = 0.0;
        if process_effect {
            let first = Channel::new(sample_rate);
            latency_frames = first.delay_frames(sample_rate)
                + limiter.as_ref().unwrap().latency_samples() as f64;
            processors.push(first);
            for _ in 1..channels {
                processors.push(Channel::new(sample_rate));
            }
        }
        self.format = Some(format);
        self.frame_bytes = frame_bytes;
        self.channels = processors;
        self.limiter = limiter;
        self.latency_hns = (latency_frames / sample_rate * 10_000_000.0).ceil() as i64;
        self.muted = false;
        Ok(())
    }

    pub fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.reset();
        }
        if let Some(limiter) = &mut self.limiter {
            limiter.reset();
        }
    }

    pub fn silence_byte(&self) -> u8 {
        if matches!(self.format, Some(SampleFormat::Pcm { bytes: 1, .. })) {
            128
        } else {
            0
        }
    }

    pub fn frame_bytes(&self) -> usize {
        self.frame_bytes
    }

    pub fn latency_hns(&self) -> i64 {
        self.latency_hns
    }

    pub fn process_with_settings(&mut self, buffer: &mut [u8], gain: f32, enabled: bool) -> bool {
        let Some(format) = self.format else {
            return true;
        };
        if gain == 0.0 {
            buffer.fill(self.silence_byte());
            if !self.muted {
                self.reset();
                self.muted = true;
            }
            return true;
        }
        self.muted = false;
        // Only neural state is reset when enhancement is enabled or disabled;
        // the STFT, resamplers and output FIFO keep consuming the audio stream.
        for channel in &mut self.channels {
            channel.model.set_enabled(enabled);
        }
        let mut silent = true;
        let mut samples = [0.0; MAX_CHANNELS];
        for frame in buffer.chunks_exact_mut(self.frame_bytes) {
            for ((sample, channel), output) in frame
                .chunks_exact(format.bytes())
                .zip(&mut self.channels)
                .zip(&mut samples)
            {
                *output = channel.process_sample(format.decode(sample) as f32) * gain;
            }
            let samples = &mut samples[..self.channels.len()];
            if let Some(limiter) = &mut self.limiter {
                limiter.process_frame(samples);
            }
            for (sample, &output) in frame.chunks_exact_mut(format.bytes()).zip(samples.iter()) {
                format.encode(sample, f64::from(output));
                silent &= output == 0.0;
            }
        }
        silent
    }
}

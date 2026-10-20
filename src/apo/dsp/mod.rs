//! Single-threaded, preallocated UL-UNAS audio processing, independent of COM.

mod high_pass;
mod limiter;

pub use crate::shared::audio::SampleFormat;
use crate::shared::resample::Resampler;
use high_pass::HighPass;
use limiter::Limiter;
use ul_unas_rs::{ALGORITHM_DELAY, Denoiser, FRAME_SIZE, SAMPLE_RATE};

pub const MIN_SAMPLE_RATE: f64 = 8_000.0;
pub const MAX_SAMPLE_RATE: f64 = 192_000.0;
pub const MAX_CHANNELS: usize = 32;

struct Channel {
    model: Box<Denoiser>,
    high_pass: HighPass,
    down: Resampler,
    up: Resampler,
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
    fn new(sample_rate: f64) -> Self {
        let rate = f64::from(SAMPLE_RATE);
        let down = Resampler::new(sample_rate, rate);
        let up = Resampler::new(rate, sample_rate);
        let prefill = ((FRAME_SIZE + up.input_chunk()) as f64 * sample_rate / rate).ceil() as usize
            + down.input_chunk()
            + 2;
        let mut channel = Self {
            model: Box::new(Denoiser::new()),
            high_pass: HighPass::new(rate),
            down,
            up,
            input: [0.0; FRAME_SIZE],
            output: [0.0; FRAME_SIZE],
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
            + ALGORITHM_DELAY as f64 * sample_rate / f64::from(SAMPLE_RATE)
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
            if self.pending == FRAME_SIZE {
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

/// The complete enhancement chain, prepared during LockForProcess.
struct Enhancement {
    channels: Vec<Channel>,
    limiter: Limiter,
    latency_frames: usize,
}

impl Enhancement {
    fn new(sample_rate: f64, channels: usize) -> Self {
        let channels: Vec<_> = (0..channels).map(|_| Channel::new(sample_rate)).collect();
        let limiter = Limiter::new(sample_rate, channels.len());
        let latency_frames = (channels[0].delay_frames(sample_rate)
            + limiter.latency_samples() as f64)
            .ceil() as usize;
        Self {
            channels,
            limiter,
            latency_frames,
        }
    }

    fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.reset();
        }
        self.limiter.reset();
    }

    fn process_frame(&mut self, input: &[u8], format: SampleFormat, gain: f32, output: &mut [f32]) {
        for ((sample, channel), value) in input
            .chunks_exact(format.bytes())
            .zip(&mut self.channels)
            .zip(output.iter_mut())
        {
            *value = channel.process_sample(format.decode(sample) as f32) * gain;
        }
        self.limiter.process_frame(output);
    }
}

#[derive(Default)]
pub struct AudioProcessor {
    format: Option<SampleFormat>,
    frame_bytes: usize,
    enhancement: Option<Enhancement>,
    bypass: Vec<u8>,
    bypass_position: usize,
    warmup: usize,
    latency_hns: i64,
    muted: bool,
    enabled: bool,
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
        let enhancement = process_effect.then(|| Enhancement::new(sample_rate, channels));
        let latency_frames = enhancement
            .as_ref()
            .map_or(0, |effect| effect.latency_frames);
        self.format = Some(format);
        self.frame_bytes = frame_bytes;
        self.enhancement = enhancement;
        // Preserve native sample bytes, including Float64 precision and PCM
        // padding bits. Bypass introduces only the host's fixed processing delay.
        self.bypass = vec![self.silence_byte(); latency_frames * frame_bytes];
        self.bypass_position = 0;
        self.warmup = 0;
        self.latency_hns = (latency_frames as f64 / sample_rate * 10_000_000.0).ceil() as i64;
        self.muted = false;
        self.enabled = false;
        Ok(())
    }

    pub fn reset(&mut self) {
        if let Some(effect) = &mut self.enhancement {
            effect.reset();
        }
        let silence = self.silence_byte();
        self.bypass.fill(silence);
        self.bypass_position = 0;
        self.warmup = self.latency_frames();
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

    pub fn latency_frames(&self) -> usize {
        self.enhancement
            .as_ref()
            .map_or(0, |effect| effect.latency_frames)
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
        let Some(effect) = &mut self.enhancement else {
            return apply_gain(buffer, format, gain);
        };
        if self.enabled != enabled {
            if enabled {
                // Restart all DSP state before re-entering the enhancement
                // path; no old filter, model or limiter history is resumed.
                effect.reset();
                self.warmup = effect.latency_frames;
            }
            self.enabled = enabled;
        }
        let mut silent = true;
        let mut samples = [0.0; MAX_CHANNELS];
        let samples = &mut samples[..effect.channels.len()];
        for frame in buffer.chunks_exact_mut(self.frame_bytes) {
            if enabled {
                effect.process_frame(frame, format, gain, samples);
            }
            let delayed =
                &mut self.bypass[self.bypass_position..self.bypass_position + self.frame_bytes];
            frame.swap_with_slice(delayed);
            self.bypass_position += self.frame_bytes;
            if self.bypass_position == self.bypass.len() {
                self.bypass_position = 0;
            }
            if enabled && self.warmup == 0 {
                for (sample, &output) in frame.chunks_exact_mut(format.bytes()).zip(samples.iter())
                {
                    format.encode(sample, f64::from(output));
                    silent &= output == 0.0;
                }
            } else {
                // Keep the aligned bypass audible while newly enabled DSP fills
                // its buffers. Disabled DSP is never called.
                silent &= apply_gain(frame, format, gain);
                self.warmup = self.warmup.saturating_sub(1);
            }
        }
        silent
    }
}

fn apply_gain(buffer: &mut [u8], format: SampleFormat, gain: f32) -> bool {
    if gain == 1.0 {
        // BUFFER_VALID is also valid for zero-filled audio. Leave all sample
        // bytes untouched instead of decoding, narrowing or re-quantizing them.
        return buffer.is_empty();
    }
    let mut silent = true;
    for sample in buffer.chunks_exact_mut(format.bytes()) {
        let output = format.decode(sample) * f64::from(gain);
        format.encode(sample, output);
        silent &= output == 0.0;
    }
    silent
}

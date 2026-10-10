//! Real-time audio processing, independent of COM and Windows.

mod aec;
mod gtcrn;
mod high_pass;
mod limiter;
mod simd;
mod spectral;
mod weights;

use crate::apo::reference_buffer::Reference;
pub use crate::shared::audio::SampleFormat;
use crate::shared::resample::Resampler;
use aec::AecChannel;
use gtcrn::Gtcrn;
use high_pass::HighPass;
use limiter::Limiter;

// Resource bounds apply during format negotiation, before any model allocation.
pub const MIN_SAMPLE_RATE: f64 = 8_000.0;
pub const MAX_SAMPLE_RATE: f64 = 192_000.0;
pub const MAX_CHANNELS: usize = 32;

/// Add future models here; selection and model loading happen outside APOProcess.
#[derive(Clone, Copy, Default)]
pub enum ModelKind {
    #[default]
    Gtcrn,
}

trait StreamingModel: Send {
    fn set_enabled(&mut self, _enabled: bool) {}
    fn sample_rate(&self) -> u32;
    fn hop_size(&self) -> usize;
    /// Intrinsic sample delay, excluding the adapter's frame collection buffer.
    fn latency_samples(&self) -> usize;
    fn reset(&mut self);
    fn process(&mut self, input: &[f32], output: &mut [f32]);
}

impl ModelKind {
    fn create(self) -> Box<dyn StreamingModel> {
        match self {
            Self::Gtcrn => Box::new(Gtcrn::new()),
        }
    }
}

struct Channel {
    model: Box<dyn StreamingModel>,
    aec: AecChannel,
    high_pass: HighPass,
    down: Resampler,
    up: Resampler,
    input: Vec<f32>,
    output: Vec<f32>,
    pending: usize,
    queue: Vec<f32>,
    read: usize,
    write: usize,
    queued: usize,
    prefill: usize,
}

impl Channel {
    fn new(model: Box<dyn StreamingModel>, sample_rate: f64) -> Self {
        let hop = model.hop_size();
        let rate = f64::from(model.sample_rate());
        let down = Resampler::new(sample_rate, rate);
        let up = Resampler::new(rate, sample_rate);
        // Reserve a complete model hop and both chunk boundaries. Filter group
        // delays are in the samples themselves, not this scheduling FIFO.
        let prefill = ((hop + up.input_chunk()) as f64 * sample_rate / rate).ceil() as usize
            + down.input_chunk()
            + 2;
        let mut channel = Self {
            model,
            aec: AecChannel::new(),
            high_pass: HighPass::new(),
            down,
            up,
            input: vec![0.0; hop],
            output: vec![0.0; hop],
            pending: 0,
            queue: vec![0.0; 2 * prefill + 8],
            read: 0,
            write: 0,
            queued: 0,
            prefill,
        };
        channel.reset();
        channel
    }

    fn reset(&mut self) {
        self.model.reset();
        self.aec.reset();
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

    fn process_sample(
        &mut self,
        input: f32,
        aec_enabled: bool,
        reference: Option<&Reference>,
    ) -> f32 {
        self.down.push(input);
        while let Some(sample) = self.down.next() {
            self.input[self.pending] = self.high_pass.process_sample(sample);
            self.pending += 1;
            if self.pending == self.input.len() {
                self.aec.process(&mut self.input, aec_enabled, reference);
                self.model.process(&self.input, &mut self.output);
                self.pending = 0;
                for &sample in &self.output {
                    self.up.push(if sample.is_finite() { sample } else { 0.0 });
                    while let Some(sample) = self.up.next() {
                        // The fixed queue accommodates two complete model hops.
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
        let output = self.queue[self.read];
        self.read = (self.read + 1) % self.queue.len();
        self.queued -= 1;
        output
    }
}

/// Formats, channels and all model buffers are fixed during LockForProcess.
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
        model: Option<ModelKind>,
    ) -> Result<(), ()> {
        if !(1..=MAX_CHANNELS).contains(&channels)
            || !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate)
        {
            return Err(());
        }
        let frame_bytes = channels.checked_mul(format.bytes()).ok_or(())?;
        let limiter = model.map(|_| Limiter::new(sample_rate, channels));
        let mut processors = Vec::new();
        let mut latency_hns = 0;
        if let Some(kind) = model {
            let first = kind.create();
            let rate = f64::from(first.sample_rate());
            let intrinsic = first.latency_samples() + aec::LATENCY_SAMPLES;
            let first = Channel::new(first, sample_rate);
            let delay = first.prefill as f64 / sample_rate
                + intrinsic as f64 / rate
                + first.down.delay_seconds()
                + first.up.delay_seconds()
                + limiter.as_ref().unwrap().latency_samples() as f64 / sample_rate;
            latency_hns = (delay * 10_000_000.0).ceil() as i64;
            processors.push(first);
            for _ in 1..channels {
                processors.push(Channel::new(kind.create(), sample_rate));
            }
        }
        self.format = Some(format);
        self.frame_bytes = frame_bytes;
        self.channels = processors;
        self.limiter = limiter;
        self.latency_hns = latency_hns;
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

    pub fn process_with_settings(
        &mut self,
        buffer: &mut [u8],
        enabled: bool,
        aec_enabled: bool,
        gain: f32,
        reference: Option<&Reference>,
    ) -> bool {
        let Some(format) = self.format else {
            return true;
        };
        if gain == 0.0 {
            buffer.fill(self.silence_byte());
            // Discard buffered speech so unmuting cannot replay pre-mute audio.
            if !self.muted {
                self.reset();
                self.muted = true;
            }
            return true;
        }
        self.muted = false;
        if aec_enabled && let Some(reference) = reference {
            reference.request_capture();
        }
        let mut silent = true;
        for channel in &mut self.channels {
            channel.model.set_enabled(enabled);
        }
        let mut samples = [0.0; MAX_CHANNELS];
        for frame in buffer.chunks_exact_mut(self.frame_bytes) {
            for ((sample, channel), output) in frame
                .chunks_exact(format.bytes())
                .zip(&mut self.channels)
                .zip(&mut samples)
            {
                *output =
                    channel.process_sample(format.decode(sample) as f32, aec_enabled, reference)
                        * gain;
            }
            // Limit after resampling and manual gain, before format conversion
            // can hard-clip peaks. A shared envelope preserves channel balance.
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

//! Real-time audio processing, independent of COM and Windows.

mod gtcrn;
mod resample;
mod simd;
mod spectral;
mod weights;

use gtcrn::Gtcrn;
use resample::Resampler;

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

#[derive(Clone, Copy)]
pub enum SampleFormat {
    Pcm { bytes: u8, valid_bits: u8 },
    Float32,
    Float64,
}

impl SampleFormat {
    pub fn pcm(bytes: u32, valid_bits: u32) -> Option<Self> {
        if (1..=4).contains(&bytes) && valid_bits > 0 && valid_bits <= bytes * 8 {
            Some(Self::Pcm {
                bytes: bytes as u8,
                valid_bits: valid_bits as u8,
            })
        } else {
            None
        }
    }

    fn bytes(self) -> usize {
        match self {
            Self::Pcm { bytes, .. } => bytes as usize,
            Self::Float32 => 4,
            Self::Float64 => 8,
        }
    }

    fn decode(self, sample: &[u8]) -> f64 {
        let value = match self {
            Self::Float32 => f64::from(f32::from_le_bytes(sample.try_into().unwrap())),
            Self::Float64 => f64::from_le_bytes(sample.try_into().unwrap()),
            Self::Pcm { bytes, valid_bits } => {
                let input = if bytes == 1 {
                    i32::from(sample[0]) - 128
                } else {
                    let mut encoded = [0; 4];
                    encoded[..bytes as usize].copy_from_slice(sample);
                    let extension = (4 - bytes) * 8;
                    (i32::from_le_bytes(encoded) << extension) >> extension
                } >> (bytes * 8 - valid_bits);
                f64::from(input) / (1_i64 << (valid_bits - 1)) as f64
            }
        };
        if value.is_finite() {
            value.clamp(-1.0, 1.0)
        } else {
            0.0
        }
    }

    fn encode(self, sample: &mut [u8], value: f64) {
        let value = if value.is_finite() {
            value.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        match self {
            Self::Float32 => sample.copy_from_slice(&(value as f32).to_le_bytes()),
            Self::Float64 => sample.copy_from_slice(&value.to_le_bytes()),
            Self::Pcm { bytes, valid_bits } => {
                let limit = (1_i64 << (valid_bits - 1)) as f64;
                let output = ((value * limit).round().clamp(-limit, limit - 1.0) as i64)
                    << (bytes * 8 - valid_bits);
                if bytes == 1 {
                    sample[0] = (output + 128) as u8;
                } else {
                    sample.copy_from_slice(&(output as i32).to_le_bytes()[..bytes as usize]);
                }
            }
        }
    }
}

struct Channel {
    model: Box<dyn StreamingModel>,
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
            self.input[self.pending] = sample;
            self.pending += 1;
            if self.pending == self.input.len() {
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
        let mut processors = Vec::new();
        let mut latency_hns = 0;
        if let Some(kind) = model {
            let first = kind.create();
            let rate = f64::from(first.sample_rate());
            let intrinsic = first.latency_samples();
            let first = Channel::new(first, sample_rate);
            let delay = first.prefill as f64 / sample_rate
                + intrinsic as f64 / rate
                + first.down.delay_seconds()
                + first.up.delay_seconds();
            latency_hns = (delay * 10_000_000.0).ceil() as i64;
            processors.push(first);
            for _ in 1..channels {
                processors.push(Channel::new(kind.create(), sample_rate));
            }
        }
        self.format = Some(format);
        self.frame_bytes = frame_bytes;
        self.channels = processors;
        self.latency_hns = latency_hns;
        self.muted = false;
        Ok(())
    }

    pub fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.reset();
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

    pub fn process_with_settings(&mut self, buffer: &mut [u8], enabled: bool, gain: f32) -> bool {
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
        let mut silent = true;
        for channel in &mut self.channels {
            channel.model.set_enabled(enabled);
        }
        for frame in buffer.chunks_exact_mut(self.frame_bytes) {
            for (sample, channel) in frame
                .chunks_exact_mut(format.bytes())
                .zip(&mut self.channels)
            {
                let output = channel.process_sample(format.decode(sample) as f32) * gain;
                format.encode(sample, f64::from(output));
                silent &= output == 0.0;
            }
        }
        silent
    }
}

//! Single-threaded, preallocated audio processing, independent of COM.

mod deep_filter;
mod gtcrn;
mod high_pass;
mod limiter;
mod simd;
mod ul_unas;

pub use crate::shared::audio::SampleFormat;
use crate::shared::resample::Resampler;
pub use crate::shared::settings::DenoiseModel as ModelKind;
use deep_filter::DeepFilterNet3;
use gtcrn::Gtcrn;
use high_pass::HighPass;
use limiter::Limiter;
use ul_unas::UlUnas;

pub const MIN_SAMPLE_RATE: f64 = 8_000.0;
pub const MAX_SAMPLE_RATE: f64 = 192_000.0;
pub const MAX_CHANNELS: usize = 32;
const FADE_SECONDS: f64 = 0.005;

trait StreamingModel: Send {
    fn set_enabled(&mut self, enabled: bool);
    fn sample_rate(&self) -> u32;
    fn hop_size(&self) -> usize;
    /// Intrinsic delay, excluding the adapter's frame collection buffer.
    fn latency_samples(&self) -> usize;
    fn reset(&mut self);
    fn process(&mut self, input: &[f32], output: &mut [f32]);
}

impl ModelKind {
    const MODELS: [Self; 3] = [Self::DeepFilterNet3, Self::UlUnas, Self::Gtcrn];

    // Off uses the full-band DF3 STFT bypass, retaining delay without inference.
    fn pipeline_index(self) -> usize {
        match self {
            Self::Off | Self::DeepFilterNet3 => 0,
            Self::UlUnas => 1,
            Self::Gtcrn => 2,
        }
    }

    fn create(self) -> Box<dyn StreamingModel> {
        match self {
            Self::Gtcrn => Box::new(Gtcrn::new()),
            Self::UlUnas => Box::new(UlUnas::new()),
            Self::Off | Self::DeepFilterNet3 => Box::new(DeepFilterNet3::new()),
        }
    }
}

struct Channel {
    model: Box<dyn StreamingModel>,
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
    fraction: f32,
    previous: f32,
}

impl Channel {
    fn new(kind: ModelKind, sample_rate: f64) -> Self {
        let model = kind.create();
        let hop = model.hop_size();
        let rate = f64::from(model.sample_rate());
        let down = Resampler::new(sample_rate, rate);
        let up = Resampler::new(rate, sample_rate);
        let prefill = ((hop + up.input_chunk()) as f64 * sample_rate / rate).ceil() as usize
            + down.input_chunk()
            + 2;
        Self {
            model,
            high_pass: HighPass::new(rate),
            down,
            up,
            input: vec![0.0; hop],
            output: vec![0.0; hop],
            pending: 0,
            queue: Vec::new(),
            read: 0,
            write: 0,
            queued: 0,
            prefill,
            fraction: 0.0,
            previous: 0.0,
        }
    }

    fn delay_frames(&self, sample_rate: f64) -> f64 {
        self.prefill as f64
            + self.model.latency_samples() as f64 * sample_rate
                / f64::from(self.model.sample_rate())
            + (self.down.delay_seconds() + self.up.delay_seconds()) * sample_rate
    }

    fn align_delay(&mut self, sample_rate: f64, frames: usize) {
        let extra = (frames as f64 - self.delay_frames(sample_rate)).max(0.0);
        let whole = extra.floor() as usize;
        self.prefill += whole;
        // Align fractional resampler group delays with a one-sample linear delay.
        self.fraction = (extra - whole as f64) as f32;
        self.queue = vec![0.0; 2 * self.prefill + 8];
        self.reset();
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
        self.previous = 0.0;
    }

    fn process_sample(&mut self, input: f32) -> f32 {
        self.down.push(input);
        while let Some(sample) = self.down.next() {
            self.input[self.pending] = self.high_pass.process_sample(sample);
            self.pending += 1;
            if self.pending == self.input.len() {
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
        let output = sample * (1.0 - self.fraction) + self.previous * self.fraction;
        self.previous = sample;
        output
    }
}

struct ChannelModels([Channel; 3]);

#[derive(Default)]
enum Transition {
    #[default]
    Steady,
    FadeOut {
        remaining: usize,
        start: f32,
    },
    Filling {
        remaining: usize,
    },
    FadeIn {
        remaining: usize,
    },
}

/// All models and adapters are allocated only during LockForProcess.
#[derive(Default)]
pub struct AudioProcessor {
    format: Option<SampleFormat>,
    frame_bytes: usize,
    channels: Vec<ChannelModels>,
    limiter: Option<Limiter>,
    latency_hns: i64,
    latency_frames: usize,
    fade_frames: usize,
    active: ModelKind,
    requested: ModelKind,
    transition: Transition,
    switch_gain: f32,
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
        let mut processors = Vec::with_capacity(if model.is_some() { channels } else { 0 });
        let mut latency_frames = 0;
        if model.is_some() {
            let mut first =
                ChannelModels(ModelKind::MODELS.map(|kind| Channel::new(kind, sample_rate)));
            let common_frames = first
                .0
                .iter()
                .map(|channel| channel.delay_frames(sample_rate))
                .fold(0.0, f64::max)
                .ceil() as usize;
            for channel in &mut first.0 {
                channel.align_delay(sample_rate, common_frames);
            }
            latency_frames = common_frames + limiter.as_ref().unwrap().latency_samples();
            processors.push(first);
            for _ in 1..channels {
                let mut pair =
                    ChannelModels(ModelKind::MODELS.map(|kind| Channel::new(kind, sample_rate)));
                for channel in &mut pair.0 {
                    channel.align_delay(sample_rate, common_frames);
                }
                processors.push(pair);
            }
        }
        self.format = Some(format);
        self.frame_bytes = frame_bytes;
        self.channels = processors;
        self.limiter = limiter;
        self.latency_hns = (latency_frames as f64 / sample_rate * 10_000_000.0).ceil() as i64;
        self.latency_frames = latency_frames;
        self.fade_frames = (sample_rate * FADE_SECONDS).ceil().max(2.0) as usize;
        self.active = model.unwrap_or_default();
        self.requested = self.active;
        self.muted = false;
        self.begin_fill();
        Ok(())
    }

    fn begin_fill(&mut self) {
        self.switch_gain = 0.0;
        self.transition = Transition::Filling {
            remaining: self.latency_frames.max(1),
        };
    }

    fn activate_requested(&mut self) {
        self.active = self.requested;
        for pair in &mut self.channels {
            let channel = &mut pair.0[self.active.pipeline_index()];
            channel.reset();
            channel.model.set_enabled(self.active != ModelKind::Off);
        }
        if let Some(limiter) = &mut self.limiter {
            limiter.reset();
        }
        self.begin_fill();
    }

    pub fn reset(&mut self) {
        self.active = self.requested;
        for pair in &mut self.channels {
            for channel in &mut pair.0 {
                channel.reset();
            }
        }
        if let Some(limiter) = &mut self.limiter {
            limiter.reset();
        }
        self.begin_fill();
    }

    fn request_model(&mut self, model: ModelKind) {
        self.requested = model;
        if self.requested == self.active {
            return;
        }
        match self.transition {
            Transition::Steady | Transition::FadeIn { .. } => {
                self.transition = Transition::FadeOut {
                    remaining: self.fade_frames,
                    start: self.switch_gain,
                };
            }
            Transition::Filling { .. } => self.activate_requested(),
            Transition::FadeOut { .. } => {}
        }
    }

    // One device frame at a time; only the active model runs during every phase.
    fn advance_transition(&mut self) -> bool {
        let mut activate = false;
        self.switch_gain = match self.transition {
            Transition::Steady => 1.0,
            Transition::FadeOut { remaining, start } => {
                let gain = start * (remaining - 1) as f32 / (self.fade_frames - 1) as f32;
                if remaining == 1 {
                    activate = true;
                } else {
                    self.transition = Transition::FadeOut {
                        remaining: remaining - 1,
                        start,
                    };
                }
                gain
            }
            Transition::Filling { remaining } => {
                if remaining <= 1 {
                    self.transition = Transition::FadeIn {
                        remaining: self.fade_frames,
                    };
                } else {
                    self.transition = Transition::Filling {
                        remaining: remaining - 1,
                    };
                }
                0.0
            }
            Transition::FadeIn { remaining } => {
                let gain = 1.0 - (remaining - 1) as f32 / (self.fade_frames - 1) as f32;
                if remaining == 1 {
                    self.transition = Transition::Steady;
                } else {
                    self.transition = Transition::FadeIn {
                        remaining: remaining - 1,
                    };
                }
                gain
            }
        };
        activate
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
        gain: f32,
        model: ModelKind,
    ) -> bool {
        let Some(format) = self.format else {
            return true;
        };
        self.request_model(model);
        if gain == 0.0 {
            buffer.fill(self.silence_byte());
            if !self.muted {
                self.reset();
                self.muted = true;
            }
            return true;
        }
        self.muted = false;
        for pair in &mut self.channels {
            pair.0[self.active.pipeline_index()]
                .model
                .set_enabled(self.active != ModelKind::Off);
        }
        let mut silent = true;
        let mut samples = [0.0; MAX_CHANNELS];
        for frame in buffer.chunks_exact_mut(self.frame_bytes) {
            for ((sample, pair), output) in frame
                .chunks_exact(format.bytes())
                .zip(&mut self.channels)
                .zip(&mut samples)
            {
                *output = pair.0[self.active.pipeline_index()]
                    .process_sample(format.decode(sample) as f32)
                    * gain;
            }
            let samples = &mut samples[..self.channels.len()];
            if let Some(limiter) = &mut self.limiter {
                limiter.process_frame(samples);
            }
            // Fade after the limiter, so its delayed tail cannot leak into switch silence.
            let activate = self.advance_transition();
            for (sample, &output) in frame.chunks_exact_mut(format.bytes()).zip(samples.iter()) {
                let output = output * self.switch_gain;
                format.encode(sample, f64::from(output));
                silent &= output == 0.0;
            }
            if activate {
                self.activate_requested();
            }
        }
        silent
    }
}

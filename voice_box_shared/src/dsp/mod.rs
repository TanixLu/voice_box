//! Preallocated audio processing with capture-time gain envelopes.
//! Audio and gain share a delay line; mute never resets buffers or stops models.

mod channel;
mod delay;
mod enhancement;
mod high_pass;
mod limiter;
mod ramp;
mod rate_matcher;
mod resampler;
mod sample;

pub use self::{
    limiter::ZeroLatencyLimiter,
    rate_matcher::RateMatcher,
    resampler::Resampler,
    sample::{SampleFormat, clamp_sample, finite_sample},
};

use self::{delay::FrameDelay, enhancement::Enhancement, limiter::LookaheadLimiter, ramp::Ramp};
use crate::{apo_cfg::ApoCfg, pcm_transport::PcmFrame};

pub const MIN_SAMPLE_RATE: f64 = 8_000.0;
pub const MAX_SAMPLE_RATE: f64 = 192_000.0;
pub const MAX_CHANNELS: usize = 32;

const RAMP_SECONDS: f64 = 0.020;

/// A continuous float pipeline through the single final limiter.
struct ProcessingChain {
    enhancement: Enhancement,
    delay: FrameDelay,
    gain: Ramp,
    limiter: LookaheadLimiter,
}

impl ProcessingChain {
    fn new(sample_rate: f64, channels: usize, config: &ApoCfg) -> Self {
        let enhancement = Enhancement::new(sample_rate, channels, config.denoising_enabled);
        let delay = FrameDelay::new(enhancement.latency_frames(), channels);
        Self {
            enhancement,
            delay,
            gain: Ramp::new(
                config.gain_db.as_f32_gain(),
                (sample_rate * RAMP_SECONDS).ceil() as usize,
            ),
            limiter: LookaheadLimiter::new(sample_rate, channels),
        }
    }

    fn latency_frames(&self) -> usize {
        self.enhancement.latency_frames() + self.limiter.latency_frames()
    }

    fn reset(&mut self, config: &ApoCfg) {
        self.enhancement.reset(config.denoising_enabled);
        self.delay.reset();
        self.gain.reset(config.gain_db.as_f32_gain());
        self.limiter.reset();
    }

    fn configure(&mut self, config: &ApoCfg) {
        self.gain.set_target(config.gain_db.as_f32_gain());
        self.enhancement.set_enabled(config.denoising_enabled);
    }

    fn process_microphone(&mut self, samples: &mut [f32], enhanced: &mut [f32]) {
        let wet = self.enhancement.process_frame(samples, enhanced);
        let gain = self.delay.process_frame(samples, self.gain.next());
        for (dry, &enhanced) in samples.iter_mut().zip(enhanced.iter()) {
            *dry = (*dry * (1.0 - wet) + enhanced * wet) * gain;
        }
    }
}

/// A prepared processor with fixed allocations. Construct and drop off the
/// real-time thread; processing and reset never resize its buffers.
pub struct AudioProcessor {
    format: SampleFormat,
    frame_bytes: usize,
    chain: Option<ProcessingChain>,
    latency_hns: i64,
}

impl AudioProcessor {
    pub fn new(
        format: SampleFormat,
        channels: usize,
        sample_rate: f64,
        process_effect: bool,
        config: &ApoCfg,
    ) -> windows::core::Result<Self> {
        if !(1..=MAX_CHANNELS).contains(&channels)
            || !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate)
        {
            return Err(windows::core::Error::from_hresult(
                windows::Win32::Foundation::E_INVALIDARG,
            ));
        }
        let frame_bytes = channels * format.bytes();
        // RAW-mode instances keep only format metadata and introduce no delay.
        let chain = process_effect.then(|| ProcessingChain::new(sample_rate, channels, config));
        let latency_frames = chain.as_ref().map_or(0, ProcessingChain::latency_frames);
        Ok(Self {
            format,
            frame_bytes,
            chain,
            latency_hns: (latency_frames as f64 / sample_rate * 10_000_000.0).ceil() as i64,
        })
    }

    /// Discard pending audio and controls and start a stream with this config.
    /// An enabled denoiser resumes fully wet after its buffers have warmed up.
    pub fn reset(&mut self, config: &ApoCfg) {
        if let Some(chain) = &mut self.chain {
            chain.reset(config);
        }
    }

    pub fn silence_byte(&self) -> u8 {
        if matches!(self.format, SampleFormat::Pcm { bytes: 1, .. }) {
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
        self.chain
            .as_ref()
            .map_or(0, ProcessingChain::latency_frames)
    }

    /// Mix host-rate stereo playback before limiting and encoding exactly once.
    /// Microphone gain travels with input audio and takes effect at the output
    /// after `latency_frames()`. A zero gain does not stop the denoiser.
    /// `buffer` must contain complete frames; playback, when present, has one
    /// stereo frame per buffer frame. No playback means microphone-only output.
    pub fn process_with_config(
        &mut self,
        buffer: &mut [u8],
        config: &ApoCfg,
        playback: Option<&[PcmFrame]>,
    ) {
        let format = self.format;
        let Some(chain) = &mut self.chain else {
            return;
        };
        debug_assert!(buffer.len().is_multiple_of(self.frame_bytes));
        debug_assert!(playback.is_none_or(|sound| sound.len() == buffer.len() / self.frame_bytes));
        chain.configure(config);
        let channels = chain.enhancement.channels();
        let mut samples = [0.0; MAX_CHANNELS];
        let samples = &mut samples[..channels];
        let mut enhanced = [0.0; MAX_CHANNELS];
        let enhanced = &mut enhanced[..channels];
        for (index, frame) in buffer.chunks_exact_mut(self.frame_bytes).enumerate() {
            for (sample, value) in frame.chunks_exact(format.bytes()).zip(samples.iter_mut()) {
                *value = format.decode(sample) as f32;
            }
            chain.process_microphone(samples, enhanced);
            if let Some(playback) = playback {
                if !config.playback_include_microphone_audio {
                    samples.fill(0.0);
                }
                let stereo = playback[index];
                if channels == 1 {
                    samples[0] += stereo[0] * 0.5 + stereo[1] * 0.5;
                } else {
                    for (value, sound) in samples.iter_mut().zip(stereo) {
                        *value += sound;
                    }
                }
            }
            chain.limiter.process_frame(samples);
            for (sample, &value) in frame.chunks_exact_mut(format.bytes()).zip(samples.iter()) {
                format.encode(sample, f64::from(value));
            }
        }
    }
}

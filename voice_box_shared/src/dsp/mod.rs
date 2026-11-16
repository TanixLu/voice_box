//! Preallocated audio processing with capture-time gain envelopes.
//! Audio and gain share a variable delay line; mute never resets processing.

mod channel;
mod delay;
#[cfg(target_arch = "x86_64")]
mod denormals;
mod enhancement;
mod high_pass;
mod limiter;
mod pre_gain;
mod ramp;
mod rate_matcher;
mod resampler;
mod sample;

pub use self::{
    limiter::ZeroLatencyLimiter,
    rate_matcher::RateMatcher,
    resampler::{RandomAccessResampler, StreamingResampler},
    sample::{SampleFormat, clamp_sample, finite_sample},
};

#[cfg(target_arch = "x86_64")]
use self::denormals::DenormalGuard;
use self::{delay::FrameDelay, enhancement::Enhancement, limiter::LookaheadLimiter, ramp::Ramp};
use crate::{apo_config::ApoConfig, pcm_transport::PcmFrame};
use dfn3_rs::Model;

pub const MIN_SAMPLE_RATE: f64 = 8_000.0;
pub const MAX_SAMPLE_RATE: f64 = 192_000.0;
pub const MAX_CHANNELS: usize = 32;

const RAMP_SECONDS: f64 = 0.020;

/// Steady-state microphone latency for a validated sample rate and live config.
/// Smooth transitions may still be in progress when this target is queried.
pub fn target_latency_hns(sample_rate: f64, config: &ApoConfig) -> i64 {
    let enhancement = if config.noise_reduction_db.is_enabled() {
        channel::Channel::latency_frames(sample_rate)
    } else {
        0
    };
    let frames = enhancement + LookaheadLimiter::latency_frames(sample_rate);
    (frames as f64 / sample_rate * 10_000_000.0).ceil() as i64
}

/// A continuous float pipeline through the single final limiter.
struct ProcessingChain {
    enhancement: Enhancement,
    delay: FrameDelay,
    gain: Ramp,
    wet: Ramp,
    enabled: bool,
    limiter: LookaheadLimiter,
}

impl ProcessingChain {
    fn new(sample_rate: f64, channels: usize, model: Model, config: &ApoConfig) -> Self {
        let enhancement = Enhancement::new(sample_rate, channels, model, config.noise_reduction_db);
        let mut delay = FrameDelay::new(enhancement.latency_frames(), channels, sample_rate);
        let enabled = config.noise_reduction_db.is_enabled();
        delay.reset(enabled);
        let ramp_frames = (sample_rate * RAMP_SECONDS).ceil() as usize;
        Self {
            enhancement,
            delay,
            gain: Ramp::new(config.gain_db.as_f32_gain(), ramp_frames),
            wet: Ramp::new(0.0, ramp_frames),
            enabled,
            limiter: LookaheadLimiter::new(sample_rate, channels),
        }
    }

    fn reset(&mut self, config: &ApoConfig) {
        self.enabled = config.noise_reduction_db.is_enabled();
        self.enhancement.reset(config.noise_reduction_db);
        self.delay.reset(self.enabled);
        self.gain.reset(config.gain_db.as_f32_gain());
        self.wet.reset(0.0);
        self.limiter.reset();
    }

    fn configure(&mut self, config: &ApoConfig) {
        self.enabled = config.noise_reduction_db.is_enabled();
        self.gain.set_target(config.gain_db.as_f32_gain());
        self.enhancement.set_strength(config.noise_reduction_db);
    }

    fn process_microphone(&mut self, samples: &mut [f32], enhanced: &mut [f32]) {
        // While any wet audio is audible, pin dry delay at the wet latency.
        // Retargeting reverses the same ramps/cursor without clearing history.
        let hold = self.enabled || self.wet.current() != 0.0;
        let ready = self.enhancement.process_frame(samples, enhanced, hold);
        let gain = self.delay.process_frame(samples, self.gain.next(), hold);
        self.wet
            .set_target(if self.enabled && ready && self.delay.is_full() {
                1.0
            } else {
                0.0
            });
        let wet = self.wet.next();
        for (dry, &enhanced) in samples.iter_mut().zip(enhanced.iter()) {
            *dry = *dry * (1.0 - wet) + enhanced * gain * wet;
        }
    }
}

/// A prepared processor with fixed allocations. Construct and drop off the
/// real-time thread; processing and reset never resize its buffers.
pub struct AudioProcessor {
    format: SampleFormat,
    frame_bytes: usize,
    chain: Option<ProcessingChain>,
}

impl AudioProcessor {
    pub fn new(
        format: SampleFormat,
        channels: usize,
        sample_rate: f64,
        process_effect: bool,
        model: Model,
        config: &ApoConfig,
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
        let chain =
            process_effect.then(|| ProcessingChain::new(sample_rate, channels, model, config));
        Ok(Self {
            format,
            frame_bytes,
            chain,
        })
    }

    /// Discard pending audio and controls and start a stream with this config.
    /// An enabled denoiser resumes at its configured mix after warming its buffers.
    pub fn reset(&mut self, config: &ApoConfig) {
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

    pub fn is_bypassed(&self) -> bool {
        self.chain.is_none()
    }

    /// Mix host-rate stereo playback before limiting and encoding exactly once.
    /// Microphone gain travels with input audio through the current delay.
    /// Live configuration changes retime the microphone toward its target latency.
    /// A zero gain does not stop the denoiser.
    /// `buffer` must contain complete frames; playback, when present, has one
    /// stereo frame per buffer frame. No playback means microphone-only output.
    /// x86-64 processing enables FTZ/DAZ for this call and restores the caller's
    /// floating-point control state before returning. RAW bypass leaves it alone.
    pub fn process_with_config(
        &mut self,
        buffer: &mut [u8],
        config: &ApoConfig,
        playback: Option<&[PcmFrame]>,
    ) {
        let format = self.format;
        let Some(chain) = &mut self.chain else {
            return;
        };
        debug_assert!(buffer.len().is_multiple_of(self.frame_bytes));
        debug_assert!(playback.is_none_or(|sound| sound.len() == buffer.len() / self.frame_bytes));
        #[cfg(target_arch = "x86_64")]
        let _denormals = DenormalGuard::new();
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

//! Preallocated audio processing with capture-time gain envelopes.
//! The denoised path uses delayed gain. Live switches fade the mic output to zero.

mod channel;
mod delay;
#[cfg(target_arch = "x86_64")]
mod denormals;
mod enhancement;
mod ramp;
mod rate_matcher;
mod resampler;
mod sample;

pub use self::{
    rate_matcher::RateMatcher,
    resampler::{RandomAccessResampler, StreamingResampler},
    sample::{SampleFormat, finite_sample},
};

#[cfg(target_arch = "x86_64")]
use self::denormals::DenormalGuard;
use self::{delay::GainDelay, enhancement::Enhancement, ramp::Ramp};
use crate::{apo_config::ApoConfig, pcm_transport::PcmFrame};
use dfn3_rs::Model;

pub const MIN_SAMPLE_RATE: f64 = 8_000.0;
pub const MAX_SAMPLE_RATE: f64 = 192_000.0;
pub const MAX_CHANNELS: usize = 32;

const PARAMETER_RAMP_SECONDS: f64 = 0.020;
const SWITCH_FADE_SECONDS: f64 = 0.010;

/// Steady-state microphone latency for a validated sample rate and live config.
/// A live switch may still be in progress when this target is queried.
pub fn target_latency_hns(sample_rate: f64, config: &ApoConfig) -> i64 {
    let frames = if config.noise_reduction_db.is_enabled() {
        channel::Channel::latency_frames(sample_rate)
    } else {
        0
    };
    (frames as f64 / sample_rate * 10_000_000.0).ceil() as i64
}

/// State for microphone enhancement, gain, and bypass transitions.
struct ProcessingChain {
    enhancement: Enhancement,
    gain_delay: GainDelay,
    gain: Ramp,
    switch_fade: Ramp,
    using_denoised_path: bool,
}

impl ProcessingChain {
    fn new(sample_rate: f64, channels: usize, model: Model, config: &ApoConfig) -> Self {
        let enhancement = Enhancement::new(sample_rate, channels, model, config.noise_reduction_db);
        let gain_delay = GainDelay::new(enhancement.latency_frames());
        let using_denoised_path = config.noise_reduction_db.is_enabled();
        let gain_ramp_frames = (sample_rate * PARAMETER_RAMP_SECONDS).ceil() as usize;
        let fade_frames = (sample_rate * SWITCH_FADE_SECONDS).ceil() as usize;
        Self {
            enhancement,
            gain_delay,
            gain: Ramp::new(config.gain_db.as_f32_gain(), gain_ramp_frames),
            switch_fade: Ramp::new(1.0, fade_frames),
            using_denoised_path,
        }
    }

    fn reset(&mut self, config: &ApoConfig) {
        self.using_denoised_path = config.noise_reduction_db.is_enabled();
        self.enhancement.reset(config.noise_reduction_db);
        self.gain_delay.reset();
        self.gain.reset(config.gain_db.as_f32_gain());
        self.switch_fade.reset(1.0);
    }

    fn configure(&mut self, config: &ApoConfig) {
        self.gain.set_target(config.gain_db.as_f32_gain());
        self.enhancement.set_strength(config.noise_reduction_db);
    }

    fn process_microphone(
        &mut self,
        samples: &mut [f32],
        enhanced: &mut [f32],
        denoising_requested: bool,
    ) {
        let denoiser_active = denoising_requested || self.using_denoised_path;
        let denoiser_ready = self
            .enhancement
            .process_frame(samples, enhanced, denoiser_active);
        let gain = self.gain.next();
        let delayed_gain = self.gain_delay.process_frame(gain);

        if self.using_denoised_path && !denoiser_ready {
            if denoising_requested {
                // At startup, hold silence until the selected path has valid audio.
                samples.fill(0.0);
                return;
            }
            // Cancelling warmup starts the dry fade from silence.
            self.switch_fade.reset(0.0);
        }

        let target_uses_denoising = denoising_requested && denoiser_ready;
        // Switch at silence. The latency jump can replay captured audio when
        // enabling and skip buffered audio when disabling.
        if self.switch_fade.current() == 0.0 {
            self.using_denoised_path = target_uses_denoising;
        }

        let fade_in = self.using_denoised_path == target_uses_denoising;
        self.switch_fade.set_target(f32::from(fade_in));
        let level = self.switch_fade.next();
        if self.using_denoised_path {
            let gain = delayed_gain * level;
            for (sample, &enhanced) in samples.iter_mut().zip(enhanced.iter()) {
                *sample = enhanced * gain;
            }
        } else {
            let gain = gain * level;
            for sample in samples {
                *sample *= gain;
            }
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
    /// An enabled denoiser emits silence until its buffers are ready.
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

    /// Mix host-rate stereo playback before encoding exactly once.
    /// Denoised audio uses the microphone gain from its capture time.
    /// Live denoising switches fade the microphone output to zero before changing paths.
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
        let denoising_requested = config.noise_reduction_db.is_enabled();
        let sample_bytes = format.bytes();
        let channels = self.frame_bytes / sample_bytes;
        let mut samples = [0.0; MAX_CHANNELS];
        let samples = &mut samples[..channels];
        let mut enhanced = [0.0; MAX_CHANNELS];
        let enhanced = &mut enhanced[..channels];
        for (index, frame) in buffer.chunks_exact_mut(self.frame_bytes).enumerate() {
            for (sample, value) in frame.chunks_exact(sample_bytes).zip(samples.iter_mut()) {
                *value = format.decode(sample);
            }
            chain.process_microphone(samples, enhanced, denoising_requested);
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
            for (sample, &value) in frame.chunks_exact_mut(sample_bytes).zip(samples.iter()) {
                format.encode(sample, value);
            }
        }
    }
}

use crate::api::config::{EchoCanceller3Config, HighFrequencySuppression};
use crate::audio_processing::aec3::aec_state::AecState;
use crate::audio_processing::aec3::aec3_common::{
    Aec3Optimization, BLOCK_SIZE, FFT_LENGTH_BY_2, FFT_LENGTH_BY_2_MINUS_1, FFT_LENGTH_BY_2_PLUS_1,
};
use crate::audio_processing::aec3::block::Block;
use crate::audio_processing::aec3::dominant_nearend_detector::DominantNearendDetector;
use crate::audio_processing::aec3::moving_average::MovingAverage;
use crate::audio_processing::aec3::nearend_detector::NearendDetector;
use crate::audio_processing::aec3::render_signal_analyzer::RenderSignalAnalyzer;
use crate::audio_processing::aec3::subband_nearend_detector::SubbandNearendDetector;
use crate::audio_processing::aec3::vector_math::VectorMath;
use crate::audio_processing::logging::apm_data_dumper::ApmDataDumper;

pub struct SuppressionGain {
    #[allow(dead_code)]
    data_dumper: ApmDataDumper,
    #[allow(dead_code)]
    optimization: Aec3Optimization,
    config: EchoCanceller3Config,
    num_capture_channels: usize,
    state_change_duration_blocks: i32,
    last_gain: [f32; FFT_LENGTH_BY_2_PLUS_1],
    last_nearend: Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>,
    last_echo: Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>,
    low_render_detector: LowNoiseRenderDetector,
    initial_state: bool,
    initial_state_change_counter: i32,
    nearend_smoothers: Vec<MovingAverage>,
    nearend_params: GainParameters,
    normal_params: GainParameters,
    nearend_detector: Box<dyn NearendDetector>,
}

impl SuppressionGain {
    pub fn new(
        config: EchoCanceller3Config,
        optimization: Aec3Optimization,
        _sample_rate_hz: i32,
        num_capture_channels: usize,
    ) -> Self {
        assert!(num_capture_channels > 0);
        let data_dumper = ApmDataDumper::new_unique();
        let state_change_duration_blocks = config.filter.config_change_duration_blocks as i32;
        assert!(state_change_duration_blocks > 0);
        let nearend_smoothers = (0..num_capture_channels)
            .map(|_| {
                MovingAverage::new(
                    FFT_LENGTH_BY_2_PLUS_1,
                    config.suppressor.nearend_average_blocks,
                )
            })
            .collect();
        let nearend_params = GainParameters::new(
            config.suppressor.last_lf_band,
            config.suppressor.first_hf_band,
            &config.suppressor.nearend_tuning,
        );
        let normal_params = GainParameters::new(
            config.suppressor.last_lf_band,
            config.suppressor.first_hf_band,
            &config.suppressor.normal_tuning,
        );
        let detector: Box<dyn NearendDetector> = if config.suppressor.use_subband_nearend_detection
        {
            Box::new(SubbandNearendDetector::new(
                &config.suppressor.subband_nearend_detection,
                num_capture_channels,
            ))
        } else {
            Box::new(DominantNearendDetector::new(
                &config.suppressor.dominant_nearend_detection,
                num_capture_channels,
            ))
        };

        Self {
            data_dumper,
            optimization,
            config,
            num_capture_channels,
            state_change_duration_blocks,
            last_gain: [1.0; FFT_LENGTH_BY_2_PLUS_1],
            last_nearend: vec![[0.0; FFT_LENGTH_BY_2_PLUS_1]; num_capture_channels],
            last_echo: vec![[0.0; FFT_LENGTH_BY_2_PLUS_1]; num_capture_channels],
            low_render_detector: LowNoiseRenderDetector::default(),
            initial_state: true,
            initial_state_change_counter: 0,
            nearend_smoothers,
            nearend_params,
            normal_params,
            nearend_detector: detector,
        }
    }

    pub fn is_dominant_nearend(&self) -> bool {
        self.nearend_detector.is_nearend_state()
    }

    pub fn set_initial_state(&mut self, state: bool) {
        self.initial_state = state;
        if state {
            self.initial_state_change_counter = self.state_change_duration_blocks;
        } else {
            self.initial_state_change_counter = 0;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn get_gain(
        &mut self,
        nearend_spectrum: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
        echo_spectrum: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
        residual_echo_spectrum: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
        residual_echo_spectrum_unbounded: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
        comfort_noise_spectrum: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
        render_signal_analyzer: &RenderSignalAnalyzer,
        aec_state: &AecState,
        render: &Block,
        clock_drift: bool,
        high_bands_gain: Option<&mut f32>,
        low_band_gain: Option<&mut [f32; FFT_LENGTH_BY_2_PLUS_1]>,
    ) {
        let high_bands_gain = high_bands_gain.expect("high_bands_gain must be provided");
        let low_band_gain = low_band_gain.expect("low_band_gain must be provided");
        assert_eq!(nearend_spectrum.len(), self.num_capture_channels);
        assert_eq!(echo_spectrum.len(), self.num_capture_channels);
        assert_eq!(residual_echo_spectrum.len(), self.num_capture_channels);
        assert_eq!(comfort_noise_spectrum.len(), self.num_capture_channels);
        assert!(render.num_channels() > 0);

        // The dominant nearend decision uses the uncapped residual echo, so a
        // well-converged filter is not penalised by the ERLE cap.
        let echo_for_nearend_detection = if self
            .config
            .suppressor
            .dominant_nearend_detection
            .use_unbounded_echo_spectrum
        {
            residual_echo_spectrum_unbounded
        } else {
            residual_echo_spectrum
        };
        self.nearend_detector.update(
            nearend_spectrum,
            echo_for_nearend_detection,
            comfort_noise_spectrum,
            self.initial_state,
        );

        let low_noise_render = self.low_render_detector.detect(render);
        self.lower_band_gain(
            low_noise_render,
            aec_state,
            nearend_spectrum,
            residual_echo_spectrum,
            comfort_noise_spectrum,
            clock_drift,
            low_band_gain,
        );

        let narrow_peak_band = render_signal_analyzer.narrow_peak_band();
        *high_bands_gain = self.upper_bands_gain(
            echo_spectrum,
            comfort_noise_spectrum,
            narrow_peak_band,
            aec_state.saturated_echo(),
            render,
            low_band_gain,
        );
    }

    fn upper_bands_gain(
        &self,
        echo_spectrum: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
        comfort_noise_spectrum: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
        narrow_peak_band: Option<usize>,
        saturated_echo: bool,
        render: &Block,
        low_band_gain: &[f32; FFT_LENGTH_BY_2_PLUS_1],
    ) -> f32 {
        if render.num_bands() == 1 {
            return 1.0;
        }

        if let Some(band) = narrow_peak_band {
            if band > FFT_LENGTH_BY_2_PLUS_1 - 10 {
                return 0.001;
            }
        }

        let low_band_gain_limit = FFT_LENGTH_BY_2 / 2;
        let mut gain_below_8khz = low_band_gain[low_band_gain_limit..]
            .iter()
            .fold(f32::MAX, |acc, &g| acc.min(g));
        if !gain_below_8khz.is_finite() {
            gain_below_8khz = 1.0;
        }
        gain_below_8khz = gain_below_8khz.min(1.0);

        if saturated_echo {
            return gain_below_8khz.min(0.001);
        }

        let mut low_band_energy = 0.0f32;
        let mut high_band_energy = 0.0f32;
        for band_index in 0..render.num_bands() {
            for channel in render.band_channels(band_index) {
                let energy: f32 = channel.iter().map(|s| s * s).sum();
                if band_index == 0 {
                    low_band_energy = low_band_energy.max(energy);
                } else {
                    high_band_energy = high_band_energy.max(energy);
                }
            }
        }

        let activation_threshold = BLOCK_SIZE as f32
            * self
                .config
                .suppressor
                .high_bands_suppression
                .anti_howling_activation_threshold;
        let anti_howling_gain = if high_band_energy < low_band_energy.max(activation_threshold) {
            1.0
        } else {
            (self
                .config
                .suppressor
                .high_bands_suppression
                .anti_howling_gain
                * (low_band_energy / high_band_energy).sqrt())
            .min(1.0)
        };

        let mut gain_bound = 1.0f32;
        if !self.nearend_detector.is_nearend_state() {
            let cfg = &self.config.suppressor.high_bands_suppression;
            let low_frequency_energy =
                |spectrum: &[f32; FFT_LENGTH_BY_2_PLUS_1]| -> f32 { spectrum[1..16].iter().sum() };
            for ch in 0..self.num_capture_channels {
                let echo_sum = low_frequency_energy(&echo_spectrum[ch]);
                let noise_sum = low_frequency_energy(&comfort_noise_spectrum[ch]);
                if echo_sum > cfg.enr_threshold * noise_sum {
                    gain_bound = cfg.max_gain_during_echo;
                    break;
                }
            }
        }

        gain_below_8khz
            .min(anti_howling_gain)
            .min(gain_bound)
            .clamp(0.0, 1.0)
    }

    fn lower_band_gain(
        &mut self,
        low_noise_render: bool,
        aec_state: &AecState,
        suppressor_input: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
        residual_echo: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
        comfort_noise: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
        clock_drift: bool,
        gain: &mut [f32; FFT_LENGTH_BY_2_PLUS_1],
    ) {
        gain.fill(1.0);
        let saturated_echo = aec_state.saturated_echo();
        let mut max_gain = [1.0f32; FFT_LENGTH_BY_2_PLUS_1];
        self.get_max_gain(&mut max_gain);

        for ch in 0..self.num_capture_channels {
            let mut nearend = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
            self.nearend_smoothers[ch].average(&suppressor_input[ch], &mut nearend);

            let mut weighted_residual_echo = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
            weight_echo_for_audibility(
                &self.config,
                &residual_echo[ch],
                &mut weighted_residual_echo,
            );

            let mut min_gain = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
            self.get_min_gain(
                &weighted_residual_echo,
                &self.last_nearend[ch],
                &self.last_echo[ch],
                low_noise_render,
                saturated_echo,
                &mut min_gain,
            );

            let mut channel_gain = [1.0f32; FFT_LENGTH_BY_2_PLUS_1];
            self.gain_to_no_audible_echo(
                &nearend,
                &weighted_residual_echo,
                &comfort_noise[0],
                &mut channel_gain,
            );

            for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
                let clamped = channel_gain[k].min(max_gain[k]).max(min_gain[k]);
                gain[k] = gain[k].min(clamped);
            }

            self.last_nearend[ch] = nearend;
            self.last_echo[ch] = weighted_residual_echo;
        }

        limit_low_frequency_gains(gain);
        // Use conservative high-frequency gains during clock-drift or when not
        // in dominant nearend.
        let conservative_hf_suppression = self.config.suppressor.conservative_hf_suppression;
        if !self.nearend_detector.is_nearend_state() || clock_drift || conservative_hf_suppression {
            limit_high_frequency_gains(
                &self.config.suppressor.high_frequency_suppression,
                conservative_hf_suppression,
                gain,
            );
        }

        self.last_gain.copy_from_slice(gain);
        sqrt_in_place(self.optimization, gain);
    }

    fn gain_to_no_audible_echo(
        &self,
        nearend: &[f32; FFT_LENGTH_BY_2_PLUS_1],
        echo: &[f32; FFT_LENGTH_BY_2_PLUS_1],
        masker: &[f32; FFT_LENGTH_BY_2_PLUS_1],
        gain: &mut [f32; FFT_LENGTH_BY_2_PLUS_1],
    ) {
        let params = if self.nearend_detector.is_nearend_state() {
            &self.nearend_params
        } else {
            &self.normal_params
        };
        for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
            let enr = echo[k] / (nearend[k] + 1.0);
            let emr = echo[k] / (masker[k] + 1.0);
            let mut g = 1.0f32;
            if enr > params.enr_transparent[k] && emr > params.emr_transparent[k] {
                g = (params.enr_suppress[k] - enr)
                    / (params.enr_suppress[k] - params.enr_transparent[k]);
                g = g.max(params.emr_transparent[k] / emr);
            }
            gain[k] = g.clamp(0.0, 1.0);
        }
    }

    fn get_min_gain(
        &self,
        weighted_residual_echo: &[f32; FFT_LENGTH_BY_2_PLUS_1],
        last_nearend: &[f32; FFT_LENGTH_BY_2_PLUS_1],
        last_echo: &[f32; FFT_LENGTH_BY_2_PLUS_1],
        low_noise_render: bool,
        saturated_echo: bool,
        min_gain: &mut [f32; FFT_LENGTH_BY_2_PLUS_1],
    ) {
        if saturated_echo {
            min_gain.fill(0.0);
            return;
        }

        let min_echo_power = if low_noise_render {
            self.config.echo_audibility.low_render_limit
        } else {
            self.config.echo_audibility.normal_render_limit
        };

        for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
            let value = if weighted_residual_echo[k] > 0.0 {
                min_echo_power / weighted_residual_echo[k]
            } else {
                1.0
            };
            min_gain[k] = value.min(1.0);
        }

        let suppressor = &self.config.suppressor;
        if !self.initial_state || suppressor.lf_smoothing_during_initial_phase {
            let dec = if self.nearend_detector.is_nearend_state() {
                self.nearend_params.max_dec_factor_lf
            } else {
                self.normal_params.max_dec_factor_lf
            };
            let last_band = suppressor
                .last_lf_smoothing_band
                .min(FFT_LENGTH_BY_2_PLUS_1 - 1);
            for k in 0..=last_band {
                // Make sure the gains of the low frequencies do not decrease too
                // quickly after strong nearend.
                if last_nearend[k] > last_echo[k]
                    || k <= suppressor.last_permanent_lf_smoothing_band
                {
                    min_gain[k] = min_gain[k].max(self.last_gain[k] * dec).min(1.0);
                }
            }
        }
    }

    fn get_max_gain(&self, max_gain: &mut [f32; FFT_LENGTH_BY_2_PLUS_1]) {
        let inc = if self.nearend_detector.is_nearend_state() {
            self.nearend_params.max_inc_factor
        } else {
            self.normal_params.max_inc_factor
        };
        let floor = self.config.suppressor.floor_first_increase;
        for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
            max_gain[k] = (self.last_gain[k] * inc).max(floor).min(1.0);
        }
    }
}

fn sqrt_in_place(optimization: Aec3Optimization, values: &mut [f32; FFT_LENGTH_BY_2_PLUS_1]) {
    for value in values.iter_mut() {
        *value = value.max(0.0);
    }
    VectorMath::new(optimization).sqrt(values);
}

fn weight_echo_for_audibility(
    config: &EchoCanceller3Config,
    echo: &[f32; FFT_LENGTH_BY_2_PLUS_1],
    weighted_echo: &mut [f32; FFT_LENGTH_BY_2_PLUS_1],
) {
    let floor_power = config.echo_audibility.floor_power;
    weighted_echo.copy_from_slice(echo);

    let mut apply = |range: std::ops::Range<usize>, threshold_multiplier: f32| {
        let threshold = floor_power * threshold_multiplier;
        let denom = (threshold - floor_power).max(1e-12);
        let normalizer = 1.0 / denom;
        for k in range.clone() {
            if echo[k] < threshold {
                let tmp = (threshold - echo[k]) * normalizer;
                let factor = (1.0 - tmp * tmp).max(0.0);
                weighted_echo[k] = echo[k] * factor;
            } else {
                weighted_echo[k] = echo[k];
            }
        }
    };

    apply(0..3, config.echo_audibility.audibility_threshold_lf);
    apply(3..7, config.echo_audibility.audibility_threshold_mf);
    apply(
        7..FFT_LENGTH_BY_2_PLUS_1,
        config.echo_audibility.audibility_threshold_hf,
    );
}

/// Limits the low frequency gains to avoid the impact of the high-pass filter
/// on the lower-frequency gain influencing the overall achieved gain.
fn limit_low_frequency_gains(gain: &mut [f32; FFT_LENGTH_BY_2_PLUS_1]) {
    gain[0] = gain[1].min(gain[2]);
    gain[1] = gain[0];
}

/// Limits the high frequency gains to avoid echo leakage due to an imperfect
/// filter.
fn limit_high_frequency_gains(
    high_frequency_suppression: &HighFrequencySuppression,
    conservative_hf_suppression: bool,
    gain: &mut [f32; FFT_LENGTH_BY_2_PLUS_1],
) {
    let limiting_gain_band = high_frequency_suppression.limiting_gain_band;
    let bands_in_limiting_gain = high_frequency_suppression.bands_in_limiting_gain;
    if bands_in_limiting_gain > 0 {
        debug_assert!(limiting_gain_band + bands_in_limiting_gain <= FFT_LENGTH_BY_2_PLUS_1);
        let mut min_upper_gain = 1.0f32;
        for &g in &gain[limiting_gain_band..limiting_gain_band + bands_in_limiting_gain] {
            min_upper_gain = min_upper_gain.min(g);
        }
        for g in gain[limiting_gain_band + 1..].iter_mut() {
            *g = (*g).min(min_upper_gain);
        }
    }
    gain[FFT_LENGTH_BY_2] = gain[FFT_LENGTH_BY_2_MINUS_1];

    if conservative_hf_suppression {
        // Limits the gain in the frequencies for which the adaptive filter has
        // not converged.
        const UPPER_ACCURATE_BAND_PLUS1: usize = 29;
        let one_by_bands = 1.0 / (UPPER_ACCURATE_BAND_PLUS1 - 20) as f32;
        let hf_gain_bound: f32 =
            gain[20..UPPER_ACCURATE_BAND_PLUS1].iter().sum::<f32>() * one_by_bands;
        for g in gain[UPPER_ACCURATE_BAND_PLUS1..].iter_mut() {
            *g = (*g).min(hf_gain_bound);
        }
    }
}

#[derive(Clone)]
struct GainParameters {
    max_inc_factor: f32,
    max_dec_factor_lf: f32,
    enr_transparent: [f32; FFT_LENGTH_BY_2_PLUS_1],
    enr_suppress: [f32; FFT_LENGTH_BY_2_PLUS_1],
    emr_transparent: [f32; FFT_LENGTH_BY_2_PLUS_1],
}

impl GainParameters {
    fn new(last_lf_band: usize, first_hf_band: usize, tuning: &crate::api::config::Tuning) -> Self {
        debug_assert!(last_lf_band < first_hf_band);
        let mut enr_transparent = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
        let mut enr_suppress = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
        let mut emr_transparent = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
        for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
            let alpha = if k <= last_lf_band {
                0.0
            } else if k < first_hf_band {
                (k - last_lf_band) as f32 / (first_hf_band - last_lf_band) as f32
            } else {
                1.0
            };
            enr_transparent[k] = (1.0 - alpha) * tuning.mask_lf.enr_transparent
                + alpha * tuning.mask_hf.enr_transparent;
            enr_suppress[k] =
                (1.0 - alpha) * tuning.mask_lf.enr_suppress + alpha * tuning.mask_hf.enr_suppress;
            emr_transparent[k] = (1.0 - alpha) * tuning.mask_lf.emr_transparent
                + alpha * tuning.mask_hf.emr_transparent;
        }
        Self {
            max_inc_factor: tuning.max_inc_factor,
            max_dec_factor_lf: tuning.max_dec_factor_lf,
            enr_transparent,
            enr_suppress,
            emr_transparent,
        }
    }
}

struct LowNoiseRenderDetector {
    average_power: f32,
}

impl Default for LowNoiseRenderDetector {
    fn default() -> Self {
        Self {
            average_power: 32_768.0 * 32_768.0,
        }
    }
}

impl LowNoiseRenderDetector {
    fn detect(&mut self, render: &Block) -> bool {
        if render.num_channels() == 0 {
            return false;
        }
        let mut x2_sum = 0.0f32;
        let mut x2_max = 0.0f32;
        for channel in render.band_channels(0) {
            for &sample in channel {
                let x2 = sample * sample;
                x2_sum += x2;
                x2_max = x2_max.max(x2);
            }
        }
        let num_render_channels = render.num_channels() as f32;
        if num_render_channels == 0.0 {
            return false;
        }
        x2_sum /= num_render_channels;
        const THRESHOLD: f32 = 50.0 * 50.0 * 64.0;
        let low_noise_render = self.average_power < THRESHOLD && x2_max < 3.0 * self.average_power;
        self.average_power = self.average_power * 0.9 + x2_sum * 0.1;
        low_noise_render
    }
}

use crate::api::config::MainConfiguration;
use crate::audio_processing::aec3::aec3_common::FFT_LENGTH_BY_2_PLUS_1;
use crate::audio_processing::aec3::echo_path_variability::{DelayAdjustment, EchoPathVariability};
use crate::audio_processing::aec3::fft_data::FftData;
use crate::audio_processing::aec3::render_signal_analyzer::RenderSignalAnalyzer;
use crate::audio_processing::aec3::subtractor_output::SubtractorOutput;

const H_ERROR_INITIAL: f32 = 10_000.0;
const POOR_EXCITATION_COUNTER_INITIAL: usize = 1000;

/// Provides the adaptive gain computation for the main filter.
pub struct MainFilterUpdateGain {
    config_change_duration_blocks: i32,
    one_by_config_change_duration_blocks: f32,
    current_config: MainConfiguration,
    target_config: MainConfiguration,
    old_target_config: MainConfiguration,
    h_error: [f32; FFT_LENGTH_BY_2_PLUS_1],
    poor_signal_excitation_counter: usize,
    call_counter: usize,
    config_change_counter: i32,
}

impl MainFilterUpdateGain {
    pub fn new(config: MainConfiguration, config_change_duration_blocks: usize) -> Self {
        assert!(config_change_duration_blocks > 0);
        let duration = config_change_duration_blocks as i32;
        let current_config = config.clone();
        Self {
            config_change_duration_blocks: duration,
            one_by_config_change_duration_blocks: 1.0 / duration as f32,
            current_config: current_config.clone(),
            target_config: current_config.clone(),
            old_target_config: config,
            h_error: [H_ERROR_INITIAL; FFT_LENGTH_BY_2_PLUS_1],
            poor_signal_excitation_counter: POOR_EXCITATION_COUNTER_INITIAL,
            call_counter: 0,
            config_change_counter: 0,
        }
    }

    pub fn set_config(&mut self, config: MainConfiguration, immediate_effect: bool) {
        if immediate_effect {
            self.current_config = config.clone();
            self.target_config = config.clone();
            self.old_target_config = config;
            self.config_change_counter = 0;
        } else {
            self.old_target_config = self.current_config.clone();
            self.target_config = config;
            self.config_change_counter = self.config_change_duration_blocks;
        }
    }

    pub fn handle_echo_path_change(&mut self, variability: &EchoPathVariability) {
        if !matches!(variability.delay_change, DelayAdjustment::None) {
            self.h_error.fill(H_ERROR_INITIAL);
        }

        if !variability.gain_change {
            self.poor_signal_excitation_counter = POOR_EXCITATION_COUNTER_INITIAL;
            self.call_counter = 0;
        } else {
            // TODO(bugs.webrtc.org/9526): handle gain changes explicitly.
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn compute(
        &mut self,
        render_power: &[f32; FFT_LENGTH_BY_2_PLUS_1],
        render_signal_analyzer: &RenderSignalAnalyzer,
        subtractor_output: &SubtractorOutput,
        erl: &[f32; FFT_LENGTH_BY_2_PLUS_1],
        size_partitions: usize,
        saturated_capture_signal: bool,
        disallow_leakage_diverged: bool,
        gain_fft: &mut FftData,
    ) {
        let e_main_fft = &subtractor_output.e_main_fft;
        let e2_main = &subtractor_output.e2_main_spectrum;
        let e2_shadow = &subtractor_output.e2_shadow_spectrum;
        self.call_counter = self.call_counter.saturating_add(1);

        self.update_current_config();

        if render_signal_analyzer.poor_signal_excitation() {
            self.poor_signal_excitation_counter = 0;
        }

        self.poor_signal_excitation_counter = self.poor_signal_excitation_counter.saturating_add(1);

        if self.poor_signal_excitation_counter < size_partitions
            || saturated_capture_signal
            || self.call_counter <= size_partitions
        {
            gain_fft.re.fill(0.0);
            gain_fft.im.fill(0.0);
        } else {
            let mut mu = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
            let size_partitions_f32 = size_partitions as f32;
            for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
                if render_power[k] >= self.current_config.noise_gate {
                    mu[k] = self.h_error[k]
                        / (0.5 * self.h_error[k] * render_power[k]
                            + size_partitions_f32 * e2_main[k]);
                } else {
                    mu[k] = 0.0;
                }
            }
            render_signal_analyzer.mask_regions_around_narrow_bands(&mut mu);

            for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
                self.h_error[k] -= 0.5 * mu[k] * render_power[k] * self.h_error[k];
            }

            for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
                gain_fft.re[k] = mu[k] * e_main_fft.re[k];
                gain_fft.im[k] = mu[k] * e_main_fft.im[k];
            }
        }

        for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
            if e2_shadow[k] >= e2_main[k] || disallow_leakage_diverged {
                self.h_error[k] += self.current_config.leakage_converged * erl[k];
            } else {
                self.h_error[k] += self.current_config.leakage_diverged * erl[k];
            }
            self.h_error[k] = self.h_error[k].max(self.current_config.error_floor);
            self.h_error[k] = self.h_error[k].min(self.current_config.error_ceil);
        }
    }

    fn update_current_config(&mut self) {
        if self.config_change_counter > 0 {
            self.config_change_counter -= 1;
            if self.config_change_counter > 0 {
                let change_factor =
                    self.config_change_counter as f32 * self.one_by_config_change_duration_blocks;
                let average =
                    |from: f32, to: f32| from * change_factor + to * (1.0 - change_factor);
                self.current_config.leakage_converged = average(
                    self.old_target_config.leakage_converged,
                    self.target_config.leakage_converged,
                );
                self.current_config.leakage_diverged = average(
                    self.old_target_config.leakage_diverged,
                    self.target_config.leakage_diverged,
                );
                self.current_config.error_floor = average(
                    self.old_target_config.error_floor,
                    self.target_config.error_floor,
                );
                self.current_config.error_ceil = average(
                    self.old_target_config.error_ceil,
                    self.target_config.error_ceil,
                );
                self.current_config.noise_gate = average(
                    self.old_target_config.noise_gate,
                    self.target_config.noise_gate,
                );
            } else {
                self.current_config = self.target_config.clone();
                self.old_target_config = self.target_config.clone();
            }
        }
    }
}

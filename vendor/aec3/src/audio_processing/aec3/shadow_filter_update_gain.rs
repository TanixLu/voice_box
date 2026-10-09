use crate::api::config::ShadowConfiguration;
use crate::audio_processing::aec3::aec3_common::FFT_LENGTH_BY_2_PLUS_1;
use crate::audio_processing::aec3::fft_data::FftData;
use crate::audio_processing::aec3::render_signal_analyzer::RenderSignalAnalyzer;

/// Implements the fixed gain computation for the shadow adaptive filter.
pub struct ShadowFilterUpdateGain {
    current_config: ShadowConfiguration,
    target_config: ShadowConfiguration,
    old_target_config: ShadowConfiguration,
    config_change_duration_blocks: i32,
    one_by_config_change_duration_blocks: f32,
    poor_signal_excitation_counter: usize,
    call_counter: usize,
    config_change_counter: i32,
}

impl ShadowFilterUpdateGain {
    pub fn new(config: ShadowConfiguration, config_change_duration_blocks: usize) -> Self {
        assert!(config_change_duration_blocks > 0);
        let duration = config_change_duration_blocks as i32;
        Self {
            current_config: config.clone(),
            target_config: config.clone(),
            old_target_config: config,
            config_change_duration_blocks: duration,
            one_by_config_change_duration_blocks: 1.0 / duration as f32,
            poor_signal_excitation_counter: 0,
            call_counter: 0,
            config_change_counter: 0,
        }
    }

    pub fn handle_echo_path_change(&mut self) {
        self.poor_signal_excitation_counter = 0;
        self.call_counter = 0;
    }

    pub fn set_config(&mut self, config: ShadowConfiguration, immediate_effect: bool) {
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

    pub fn compute(
        &mut self,
        render_power: &[f32; FFT_LENGTH_BY_2_PLUS_1],
        render_signal_analyzer: &RenderSignalAnalyzer,
        e_shadow: &FftData,
        size_partitions: usize,
        saturated_capture_signal: bool,
        g: &mut FftData,
    ) {
        self.call_counter += 1;
        self.update_current_config();

        if render_signal_analyzer.poor_signal_excitation() {
            self.poor_signal_excitation_counter = 0;
        }

        self.poor_signal_excitation_counter += 1;
        if self.poor_signal_excitation_counter < size_partitions
            || saturated_capture_signal
            || self.call_counter <= size_partitions
        {
            g.re.fill(0.0);
            g.im.fill(0.0);
            return;
        }

        let mut mu = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
        for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
            if render_power[k] > self.current_config.noise_gate {
                mu[k] = self.current_config.rate / render_power[k];
            } else {
                mu[k] = 0.0;
            }
        }
        render_signal_analyzer.mask_regions_around_narrow_bands(&mut mu);

        for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
            g.re[k] = mu[k] * e_shadow.re[k];
            g.im[k] = mu[k] * e_shadow.im[k];
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
                self.current_config.rate =
                    average(self.old_target_config.rate, self.target_config.rate);
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

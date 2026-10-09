use crate::api::config::{EchoCanceller3Config, EchoModel, EpStrength};
use crate::audio_processing::aec3::aec_state::AecState;
use crate::audio_processing::aec3::aec3_common::FFT_LENGTH_BY_2_PLUS_1;
use crate::audio_processing::aec3::render_buffer::RenderBuffer;
use crate::audio_processing::aec3::reverb_model::ReverbModel;
use crate::audio_processing::aec3::spectrum_buffer::SpectrumBuffer;

/// Estimates the residual echo power seen on the capture channels.
pub struct ResidualEchoEstimator {
    config: EchoCanceller3Config,
    num_render_channels: usize,
    x2_noise_floor: [f32; FFT_LENGTH_BY_2_PLUS_1],
    x2_noise_floor_counter: [usize; FFT_LENGTH_BY_2_PLUS_1],
    echo_reverb: ReverbModel,
}

impl ResidualEchoEstimator {
    pub fn new(config: EchoCanceller3Config, num_render_channels: usize) -> Self {
        assert!(num_render_channels > 0);
        let mut instance = Self {
            config,
            num_render_channels,
            x2_noise_floor: [0.0; FFT_LENGTH_BY_2_PLUS_1],
            x2_noise_floor_counter: [0; FFT_LENGTH_BY_2_PLUS_1],
            echo_reverb: ReverbModel::new(),
        };
        instance.reset();
        instance
    }

    #[allow(clippy::too_many_arguments)]
    pub fn estimate(
        &mut self,
        aec_state: &AecState,
        render_buffer: &RenderBuffer<'_>,
        s2_linear: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
        y2: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
        dominant_nearend: bool,
        r2: &mut [[f32; FFT_LENGTH_BY_2_PLUS_1]],
        r2_unbounded: &mut [[f32; FFT_LENGTH_BY_2_PLUS_1]],
    ) {
        assert_eq!(r2.len(), y2.len());
        assert_eq!(r2.len(), s2_linear.len());
        assert_eq!(r2.len(), r2_unbounded.len());
        let num_capture_channels = r2.len();

        self.update_render_noise_power(render_buffer);

        if aec_state.usable_linear_estimate() {
            if aec_state.saturated_echo() {
                copy_power_spectra(y2, r2);
                copy_power_spectra(y2, r2_unbounded);
            } else {
                let onset_compensated = self
                    .config
                    .ep_strength
                    .erle_onset_compensation_in_dominant_nearend
                    || !dominant_nearend;
                linear_estimate_with_erle(s2_linear, aec_state.erle(onset_compensated), r2);
                linear_estimate_with_erle(s2_linear, aec_state.erle_unbounded(), r2_unbounded);
            }

            self.update_reverb(
                ReverbType::Linear,
                aec_state,
                render_buffer,
                dominant_nearend,
            );
            add_reverb(self.echo_reverb.reverb(), r2);
            add_reverb(self.echo_reverb.reverb(), r2_unbounded);
        } else {
            let echo_path_gain = get_echo_path_gain(aec_state, &self.config.ep_strength);

            if aec_state.saturated_echo() {
                copy_power_spectra(y2, r2);
                copy_power_spectra(y2, r2_unbounded);
            } else {
                let mut x2 = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
                echo_generating_power(
                    self.num_render_channels,
                    render_buffer.spectrum_buffer(),
                    &self.config.echo_model,
                    aec_state.min_direct_path_filter_delay(),
                    &mut x2,
                );
                if !aec_state.use_stationarity_properties() {
                    apply_noise_gate(&self.config.echo_model, &mut x2);
                }
                for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
                    x2[k] -= self.config.echo_model.stationary_gate_slope * self.x2_noise_floor[k];
                    if x2[k] < 0.0 {
                        x2[k] = 0.0;
                    }
                }
                non_linear_estimate(echo_path_gain, &x2, r2);
                non_linear_estimate(echo_path_gain, &x2, r2_unbounded);
            }

            if self.config.echo_model.model_reverb_in_nonlinear_mode
                && !aec_state.transparent_mode()
            {
                self.update_reverb(
                    ReverbType::NonLinear,
                    aec_state,
                    render_buffer,
                    dominant_nearend,
                );
                add_reverb(self.echo_reverb.reverb(), r2);
                add_reverb(self.echo_reverb.reverb(), r2_unbounded);
            }
        }

        if aec_state.use_stationarity_properties() {
            let mut residual_scaling = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
            aec_state.get_residual_echo_scaling(&mut residual_scaling);
            for ch in 0..num_capture_channels {
                for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
                    r2[ch][k] *= residual_scaling[k];
                    r2_unbounded[ch][k] *= residual_scaling[k];
                }
            }
        }
    }

    fn reset(&mut self) {
        self.echo_reverb.reset();
        self.x2_noise_floor
            .fill(self.config.echo_model.min_noise_floor_power);
        self.x2_noise_floor_counter
            .fill(self.config.echo_model.noise_floor_hold);
    }

    fn update_render_noise_power(&mut self, render_buffer: &RenderBuffer<'_>) {
        let spectra = render_buffer.spectrum(0);
        debug_assert_eq!(self.num_render_channels, spectra.len());
        let mut render_power_data = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
        let render_power: &[f32; FFT_LENGTH_BY_2_PLUS_1] = if self.num_render_channels > 1 {
            render_power_data.fill(0.0);
            for ch in 0..self.num_render_channels {
                for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
                    render_power_data[k] += spectra[ch][k];
                }
            }
            &render_power_data
        } else {
            &spectra[0]
        };

        for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
            if render_power[k] < self.x2_noise_floor[k] {
                self.x2_noise_floor[k] = render_power[k];
                self.x2_noise_floor_counter[k] = 0;
            } else if self.x2_noise_floor_counter[k] >= self.config.echo_model.noise_floor_hold {
                self.x2_noise_floor[k] = (self.x2_noise_floor[k] * 1.1)
                    .max(self.config.echo_model.min_noise_floor_power);
            } else {
                self.x2_noise_floor_counter[k] += 1;
            }
        }
    }

    /// Updates the reverb model. Split from [`add_reverb`] because the model is
    /// updated once per block but added to both the bounded and unbounded
    /// residual echo spectra.
    fn update_reverb(
        &mut self,
        reverb_type: ReverbType,
        aec_state: &AecState,
        render_buffer: &RenderBuffer<'_>,
        dominant_nearend: bool,
    ) {
        let first_reverb_partition = match reverb_type {
            ReverbType::Linear => (aec_state.filter_length_blocks() + 1) as isize,
            ReverbType::NonLinear => (aec_state.min_direct_path_filter_delay() + 1) as isize,
        };
        let spectra = render_buffer.spectrum(first_reverb_partition);
        debug_assert_eq!(self.num_render_channels, spectra.len());

        let mut render_power_data = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
        let render_power: &[f32; FFT_LENGTH_BY_2_PLUS_1] = if self.num_render_channels > 1 {
            render_power_data.fill(0.0);
            for ch in 0..self.num_render_channels {
                for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
                    render_power_data[k] += spectra[ch][k];
                }
            }
            &render_power_data
        } else {
            &spectra[0]
        };

        let reverb_decay = aec_state.reverb_decay(/*mild=*/ dominant_nearend);
        if matches!(reverb_type, ReverbType::Linear) {
            self.echo_reverb.update_reverb(
                render_power,
                aec_state.get_reverb_frequency_response(),
                reverb_decay,
            );
        } else {
            let echo_path_gain = get_echo_path_gain(aec_state, &self.config.ep_strength);
            self.echo_reverb.update_reverb_no_freq_shaping(
                render_power,
                echo_path_gain,
                reverb_decay,
            );
        }
    }
}

fn add_reverb(
    reverb_power: &[f32; FFT_LENGTH_BY_2_PLUS_1],
    r2: &mut [[f32; FFT_LENGTH_BY_2_PLUS_1]],
) {
    for channel in r2.iter_mut() {
        for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
            channel[k] += reverb_power[k];
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum ReverbType {
    Linear,
    NonLinear,
}

fn copy_power_spectra(
    src: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
    dst: &mut [[f32; FFT_LENGTH_BY_2_PLUS_1]],
) {
    for (d, s) in dst.iter_mut().zip(src.iter()) {
        d.copy_from_slice(s);
    }
}

fn linear_estimate_with_erle(
    s2_linear: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
    erle: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
    r2: &mut [[f32; FFT_LENGTH_BY_2_PLUS_1]],
) {
    for ch in 0..r2.len() {
        for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
            debug_assert!(erle[ch][k] > 0.0);
            r2[ch][k] = if erle[ch][k] > 0.0 {
                s2_linear[ch][k] / erle[ch][k]
            } else {
                0.0
            };
        }
    }
}

fn non_linear_estimate(
    echo_path_gain: f32,
    x2: &[f32; FFT_LENGTH_BY_2_PLUS_1],
    r2: &mut [[f32; FFT_LENGTH_BY_2_PLUS_1]],
) {
    for ch in 0..r2.len() {
        for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
            r2[ch][k] = x2[k] * echo_path_gain;
        }
    }
}

fn apply_noise_gate(echo_model: &EchoModel, x2: &mut [f32; FFT_LENGTH_BY_2_PLUS_1]) {
    for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
        if echo_model.noise_gate_power > x2[k] {
            let delta = echo_model.noise_gate_power - x2[k];
            x2[k] = (x2[k] - echo_model.noise_gate_slope * delta).max(0.0);
        }
    }
}

fn echo_generating_power(
    num_render_channels: usize,
    spectrum_buffer: &SpectrumBuffer,
    echo_model: &EchoModel,
    filter_delay_blocks: i32,
    x2: &mut [f32; FFT_LENGTH_BY_2_PLUS_1],
) {
    let (mut idx_start, idx_stop) =
        get_render_indexes_to_analyze(spectrum_buffer, echo_model, filter_delay_blocks);
    x2.fill(0.0);
    if num_render_channels == 1 {
        while idx_start != idx_stop {
            let channel_power = &spectrum_buffer.buffer[idx_start][0];
            for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
                x2[k] = x2[k].max(channel_power[k]);
            }
            idx_start = spectrum_buffer.inc_index(idx_start);
        }
    } else {
        while idx_start != idx_stop {
            let mut render_power = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
            for ch in 0..num_render_channels {
                let channel_power = &spectrum_buffer.buffer[idx_start][ch];
                for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
                    render_power[k] += channel_power[k];
                }
            }
            for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
                x2[k] = x2[k].max(render_power[k]);
            }
            idx_start = spectrum_buffer.inc_index(idx_start);
        }
    }
}

fn get_render_indexes_to_analyze(
    spectrum_buffer: &SpectrumBuffer,
    echo_model: &EchoModel,
    filter_delay_blocks: i32,
) -> (usize, usize) {
    let pre = echo_model.render_pre_window_size as i32;
    let post = echo_model.render_post_window_size as i32;
    let window_start = (filter_delay_blocks - pre).max(0);
    let window_end = filter_delay_blocks + post;
    let idx_start = spectrum_buffer.offset_index(spectrum_buffer.read, window_start as isize);
    let idx_stop = spectrum_buffer.offset_index(spectrum_buffer.read, (window_end + 1) as isize);
    (idx_start, idx_stop)
}

fn get_echo_path_gain(aec_state: &AecState, config: &EpStrength) -> f32 {
    let gain_amplitude = if aec_state.transparent_mode() {
        0.01
    } else {
        config.default_gain
    };
    gain_amplitude * gain_amplitude
}

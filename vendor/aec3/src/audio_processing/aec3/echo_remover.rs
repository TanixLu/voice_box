use std::ptr;

use crate::api::config::EchoCanceller3Config;
use crate::audio_processing::aec3::aec_state::AecState;
use crate::audio_processing::aec3::aec3_common::{
    Aec3Optimization, BLOCK_SIZE, FFT_LENGTH_BY_2_PLUS_1, detect_optimization, num_bands_for_rate,
    valid_full_band_rate,
};
use crate::audio_processing::aec3::aec3_fft::{Aec3Fft, Window};
use crate::audio_processing::aec3::block::Block;
use crate::audio_processing::aec3::comfort_noise_generator::ComfortNoiseGenerator;
use crate::audio_processing::aec3::delay_estimate::DelayEstimate;
use crate::audio_processing::aec3::echo_path_variability::{DelayAdjustment, EchoPathVariability};
use crate::audio_processing::aec3::fft_data::FftData;
use crate::audio_processing::aec3::render_buffer::RenderBuffer;
use crate::audio_processing::aec3::render_signal_analyzer::RenderSignalAnalyzer;
use crate::audio_processing::aec3::residual_echo_estimator::ResidualEchoEstimator;
use crate::audio_processing::aec3::subtractor::Subtractor;
use crate::audio_processing::aec3::subtractor_output::SubtractorOutput;
use crate::audio_processing::aec3::suppression_filter::SuppressionFilter;
use crate::audio_processing::aec3::suppression_gain::SuppressionGain;

const GAIN_CHANGE_HANGOVER_BLOCKS: i32 = 3;

/// Top-level component that removes echo from the capture signal by
/// orchestrating the linear echo canceller and suppression stages.
pub struct EchoRemover {
    config: EchoCanceller3Config,
    fft: Aec3Fft,

    optimization: Aec3Optimization,
    sample_rate_hz: i32,
    num_render_channels: usize,
    num_capture_channels: usize,
    use_shadow_filter_output: bool,
    subtractor: Subtractor,
    suppression_gain: SuppressionGain,
    cng: ComfortNoiseGenerator,
    suppression_filter: SuppressionFilter,
    render_signal_analyzer: RenderSignalAnalyzer,
    residual_echo_estimator: ResidualEchoEstimator,
    aec_state: AecState,

    gain_change_hangover: i32,
    main_filter_output_last_selected: bool,
    e_blocks: Vec<[f32; BLOCK_SIZE]>,
    e_old: Vec<[f32; BLOCK_SIZE]>,
    y_old: Vec<[f32; BLOCK_SIZE]>,
    y_fft: Vec<FftData>,
    e_fft: Vec<FftData>,
    y2: Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>,
    e2: Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>,
    r2: Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>,
    r2_unbounded: Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>,
    s2_linear: Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>,
    comfort_noise: Vec<FftData>,
    high_band_comfort_noise: Vec<FftData>,
    subtractor_output: Vec<SubtractorOutput>,
}

impl EchoRemover {
    pub fn new(
        config: EchoCanceller3Config,
        sample_rate_hz: i32,
        num_render_channels: usize,
        num_capture_channels: usize,
    ) -> Self {
        assert!(valid_full_band_rate(sample_rate_hz));
        assert!(num_capture_channels > 0);
        assert!(num_render_channels > 0);

        let fft = Aec3Fft::new();
        let optimization = detect_optimization();
        let subtractor = Subtractor::new(
            config.clone(),
            num_render_channels,
            num_capture_channels,
            optimization,
        );
        let suppression_gain = SuppressionGain::new(
            config.clone(),
            optimization,
            sample_rate_hz,
            num_capture_channels,
        );
        let cng = ComfortNoiseGenerator::new(
            optimization,
            config.comfort_noise.noise_floor_dbfs,
            num_capture_channels,
        );
        let suppression_filter =
            SuppressionFilter::new(optimization, sample_rate_hz, num_capture_channels);
        let render_signal_analyzer = RenderSignalAnalyzer::new(&config);
        let residual_echo_estimator =
            ResidualEchoEstimator::new(config.clone(), num_render_channels);
        let aec_state = AecState::new(config.clone(), num_capture_channels);
        let use_shadow_filter_output = config.filter.enable_shadow_filter_output_usage;

        Self {
            config,
            fft,

            optimization,
            sample_rate_hz,
            num_render_channels,
            num_capture_channels,
            use_shadow_filter_output,
            subtractor,
            suppression_gain,
            cng,
            suppression_filter,
            render_signal_analyzer,
            residual_echo_estimator,
            aec_state,

            gain_change_hangover: 0,
            main_filter_output_last_selected: true,
            e_blocks: vec![[0.0; BLOCK_SIZE]; num_capture_channels],
            e_old: vec![[0.0; BLOCK_SIZE]; num_capture_channels],
            y_old: vec![[0.0; BLOCK_SIZE]; num_capture_channels],
            y_fft: vec![FftData::default(); num_capture_channels],
            e_fft: vec![FftData::default(); num_capture_channels],
            y2: vec![[0.0; FFT_LENGTH_BY_2_PLUS_1]; num_capture_channels],
            e2: vec![[0.0; FFT_LENGTH_BY_2_PLUS_1]; num_capture_channels],
            r2: vec![[0.0; FFT_LENGTH_BY_2_PLUS_1]; num_capture_channels],
            r2_unbounded: vec![[0.0; FFT_LENGTH_BY_2_PLUS_1]; num_capture_channels],
            s2_linear: vec![[0.0; FFT_LENGTH_BY_2_PLUS_1]; num_capture_channels],
            comfort_noise: vec![FftData::default(); num_capture_channels],
            high_band_comfort_noise: vec![FftData::default(); num_capture_channels],
            subtractor_output: vec![SubtractorOutput::default(); num_capture_channels],
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn process_capture(
        &mut self,
        mut echo_path_variability: EchoPathVariability,
        capture_signal_saturation: bool,
        external_delay: Option<DelayEstimate>,
        render_buffer: &RenderBuffer<'_>,
        capture: &mut Block,
    ) {
        let render_block = render_buffer.block(0);
        let num_bands = num_bands_for_rate(self.sample_rate_hz);
        assert_eq!(render_block.num_bands(), num_bands);
        assert_eq!(capture.num_bands(), num_bands);
        assert_eq!(render_block.num_channels(), self.num_render_channels);
        assert_eq!(capture.num_channels(), self.num_capture_channels);

        self.aec_state
            .update_capture_saturation(capture_signal_saturation);

        if echo_path_variability.audio_path_changed() {
            if echo_path_variability.gain_change {
                if self.gain_change_hangover == 0 {
                    self.gain_change_hangover = GAIN_CHANGE_HANGOVER_BLOCKS;
                } else {
                    echo_path_variability.gain_change = false;
                }
            }

            self.subtractor
                .handle_echo_path_change(&echo_path_variability);
            self.aec_state
                .handle_echo_path_change(&echo_path_variability);

            if !matches!(echo_path_variability.delay_change, DelayAdjustment::None) {
                self.suppression_gain.set_initial_state(true);
            }
        }

        if self.gain_change_hangover > 0 {
            self.gain_change_hangover -= 1;
        }

        let min_delay = self.aec_state.min_direct_path_filter_delay();
        let delay_partitions = (min_delay >= 0).then_some(min_delay as usize);
        self.render_signal_analyzer
            .update(render_buffer, delay_partitions);

        if self.aec_state.transition_triggered() {
            self.subtractor.exit_initial_state();
            self.suppression_gain.set_initial_state(false);
        }

        {
            self.subtractor.process(
                render_buffer,
                capture,
                &self.render_signal_analyzer,
                &self.aec_state,
                &mut self.subtractor_output,
            );
        }

        {
            for ch in 0..self.num_capture_channels {
                form_linear_filter_output(
                    self.use_shadow_filter_output,
                    &mut self.main_filter_output_last_selected,
                    &self.subtractor_output[ch],
                    &mut self.e_blocks[ch],
                );
                windowed_padded_fft(
                    &self.fft,
                    capture.view(0, ch),
                    &mut self.y_old[ch],
                    &mut self.y_fft[ch],
                );
                windowed_padded_fft(
                    &self.fft,
                    &self.e_blocks[ch],
                    &mut self.e_old[ch],
                    &mut self.e_fft[ch],
                );
                linear_echo_power(&self.e_fft[ch], &self.y_fft[ch], &mut self.s2_linear[ch]);
                self.y_fft[ch].spectrum(self.optimization, &mut self.y2[ch]);
                self.e_fft[ch].spectrum(self.optimization, &mut self.e2[ch]);
            }
        }

        // The reference selects the nearend spectrum before updating the AEC
        // state, so the choice uses the previous block's state.
        let nearend_from_linear = self.aec_state.usable_linear_estimate();

        self.aec_state.update(
            external_delay,
            self.subtractor.filter_frequency_responses(),
            self.subtractor.filter_impulse_responses(),
            render_buffer,
            &self.e2,
            &self.y2,
            &self.subtractor_output,
        );

        let y_fft = if self.aec_state.use_linear_filter_output() {
            &self.e_fft
        } else {
            &self.y_fft
        };

        // Comfort noise is estimated from the unclamped nearend spectrum.
        let saturated_capture = self.aec_state.saturated_capture();
        let nearend_spectrum = if nearend_from_linear {
            &self.e2
        } else {
            &self.y2
        };
        self.cng.compute(
            saturated_capture,
            nearend_spectrum,
            &mut self.comfort_noise,
            &mut self.high_band_comfort_noise,
        );

        // Estimate and suppress the residual echo in the capture output.
        let mut gain = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];

        self.residual_echo_estimator.estimate(
            &self.aec_state,
            render_buffer,
            &self.s2_linear,
            &self.y2,
            self.suppression_gain.is_dominant_nearend(),
            &mut self.r2,
            &mut self.r2_unbounded,
        );

        if self.aec_state.usable_linear_estimate() {
            for ch in 0..self.num_capture_channels {
                for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
                    if self.e2[ch][k] > self.y2[ch][k] {
                        self.e2[ch][k] = self.y2[ch][k];
                    }
                }
            }
        }

        // Re-derived after the clamp above, so the suppressor sees the
        // clamped spectrum while comfort noise saw the unclamped one.
        let nearend_spectrum = if nearend_from_linear {
            &self.e2
        } else {
            &self.y2
        };

        let echo_spectrum = if self.aec_state.usable_linear_estimate() {
            &self.s2_linear
        } else {
            &self.r2
        };

        let mut high_bands_gain = 1.0f32;
        let clock_drift =
            self.config.echo_removal_control.has_clock_drift || echo_path_variability.clock_drift;
        self.suppression_gain.get_gain(
            nearend_spectrum,
            echo_spectrum,
            &self.r2,
            &self.r2_unbounded,
            self.cng.noise_spectrum(),
            &self.render_signal_analyzer,
            &self.aec_state,
            render_block,
            clock_drift,
            Some(&mut high_bands_gain),
            Some(&mut gain),
        );

        self.suppression_filter.apply_gain(
            &self.comfort_noise,
            &self.high_band_comfort_noise,
            &gain,
            high_bands_gain,
            y_fft,
            Some(capture),
        );
    }
}

fn windowed_padded_fft(
    fft: &Aec3Fft,
    v: &[f32],
    v_old: &mut [f32; BLOCK_SIZE],
    spectrum: &mut FftData,
) {
    assert_eq!(v.len(), BLOCK_SIZE);
    fft.padded_fft_with_window(v, v_old, Window::SqrtHanning, spectrum);
    v_old.copy_from_slice(v);
}

fn linear_echo_power(e: &FftData, y: &FftData, dst: &mut [f32; FFT_LENGTH_BY_2_PLUS_1]) {
    for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
        let dr = y.re[k] - e.re[k];
        let di = y.im[k] - e.im[k];
        dst[k] = dr * dr + di * di;
    }
}

fn signal_transition(from: &[f32], to: &[f32], out: &mut [f32]) {
    assert_eq!(from.len(), to.len());
    assert_eq!(from.len(), out.len());
    if ptr::eq(from.as_ptr(), to.as_ptr()) {
        out.copy_from_slice(to);
        return;
    }

    const TRANSITION_SIZE: usize = 30;
    assert!(TRANSITION_SIZE <= out.len());
    let inv = 1.0f32 / (TRANSITION_SIZE as f32 + 1.0);
    for k in 0..TRANSITION_SIZE {
        let a = (k as f32 + 1.0) * inv;
        out[k] = a * to[k] + (1.0 - a) * from[k];
    }
    out[TRANSITION_SIZE..].copy_from_slice(&to[TRANSITION_SIZE..]);
}

fn form_linear_filter_output(
    use_shadow_filter_output: bool,
    main_filter_output_last_selected: &mut bool,
    subtractor_output: &SubtractorOutput,
    output: &mut [f32; BLOCK_SIZE],
) {
    let mut use_main = true;
    if use_shadow_filter_output {
        if subtractor_output.e2_shadow < 0.9f32 * subtractor_output.e2_main
            && subtractor_output.y2 > 30.0f32 * 30.0f32 * BLOCK_SIZE as f32
            && (subtractor_output.s2_main > 60.0f32 * 60.0f32 * BLOCK_SIZE as f32
                || subtractor_output.s2_shadow > 60.0f32 * 60.0f32 * BLOCK_SIZE as f32)
        {
            use_main = false;
        } else if subtractor_output.e2_shadow < subtractor_output.e2_main
            && subtractor_output.y2 < subtractor_output.e2_main
        {
            use_main = false;
        }
    }

    let from = if *main_filter_output_last_selected {
        &subtractor_output.e_main
    } else {
        &subtractor_output.e_shadow
    };
    let to = if use_main {
        &subtractor_output.e_main
    } else {
        &subtractor_output.e_shadow
    };
    signal_transition(from, to, output);
    *main_filter_output_last_selected = use_main;
}

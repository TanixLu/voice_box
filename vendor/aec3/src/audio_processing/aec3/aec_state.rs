use crate::api::config::EchoCanceller3Config;
use crate::audio_processing::aec3::aec3_common::{
    FFT_LENGTH_BY_2, FFT_LENGTH_BY_2_PLUS_1, NUM_BLOCKS_PER_SECOND,
};
use crate::audio_processing::aec3::delay_estimate::DelayEstimate;
use crate::audio_processing::aec3::echo_audibility::EchoAudibility;
use crate::audio_processing::aec3::echo_path_variability::{DelayAdjustment, EchoPathVariability};
use crate::audio_processing::aec3::echo_remover_metrics::EchoRemoverMetricsAecState;
use crate::audio_processing::aec3::erl_estimator::ErlEstimator;
use crate::audio_processing::aec3::erle_estimator::ErleEstimator;
use crate::audio_processing::aec3::filter_analyzer::FilterAnalyzer;
use crate::audio_processing::aec3::render_buffer::RenderBuffer;
use crate::audio_processing::aec3::reverb_model::ReverbModel;
use crate::audio_processing::aec3::reverb_model_estimator::ReverbModelEstimator;
use crate::audio_processing::aec3::subtractor_output::SubtractorOutput;
use crate::audio_processing::aec3::subtractor_output_analyzer::SubtractorOutputAnalyzer;
use crate::audio_processing::logging::apm_data_dumper::{ApmDataDumper, DiagnosticLevel};

mod avg_render_reverb;
mod filter_delay;
mod filtering_quality_analyzer;
mod initial_state;
mod saturation_detector;
mod transparent_mode;

use self::avg_render_reverb::compute_avg_render_reverb;
use self::filter_delay::FilterDelay;
use self::filtering_quality_analyzer::FilteringQualityAnalyzer;
use self::initial_state::InitialState;
use self::saturation_detector::SaturationDetector;
use self::transparent_mode::TransparentMode;

pub struct AecState {
    data_dumper: ApmDataDumper,
    config: EchoCanceller3Config,
    num_capture_channels: usize,
    initial_state: InitialState,
    delay_state: FilterDelay,
    transparent_state: TransparentMode,
    filter_quality_state: FilteringQualityAnalyzer,
    erl_estimator: ErlEstimator,
    erle_estimator: ErleEstimator,
    filter_analyzer: FilterAnalyzer,
    echo_audibility: EchoAudibility,
    reverb_model_estimator: ReverbModelEstimator,
    avg_render_reverb: ReverbModel,
    subtractor_output_analyzer: SubtractorOutputAnalyzer,
    saturation_detector: SaturationDetector,
    strong_not_saturated_render_blocks: usize,
    blocks_with_active_render: usize,
    capture_signal_saturation: bool,
}

impl AecState {
    pub fn new(config: EchoCanceller3Config, num_capture_channels: usize) -> Self {
        assert!(num_capture_channels > 0);
        let data_dumper = ApmDataDumper::new_unique();
        let initial_state = InitialState::new(&config);
        let delay_state = FilterDelay::new(&config, num_capture_channels);
        let transparent_state = TransparentMode::new(&config);
        let filter_quality_state = FilteringQualityAnalyzer::new(&config, num_capture_channels);
        let erl_estimator = ErlEstimator::new(2 * NUM_BLOCKS_PER_SECOND);
        let erle_estimator =
            ErleEstimator::new(2 * NUM_BLOCKS_PER_SECOND, &config, num_capture_channels);
        let filter_analyzer = FilterAnalyzer::new(&config, num_capture_channels);
        let echo_audibility =
            EchoAudibility::new(config.echo_audibility.use_stationarity_properties_at_init);
        let reverb_model_estimator = ReverbModelEstimator::new(&config, num_capture_channels);
        let avg_render_reverb = ReverbModel::new();
        let subtractor_output_analyzer = SubtractorOutputAnalyzer::new(num_capture_channels);
        let saturation_detector = SaturationDetector::default();

        Self {
            data_dumper,
            config,
            num_capture_channels,
            initial_state,
            delay_state,
            transparent_state,
            filter_quality_state,
            erl_estimator,
            erle_estimator,
            filter_analyzer,
            echo_audibility,
            reverb_model_estimator,
            avg_render_reverb,
            subtractor_output_analyzer,
            saturation_detector,
            strong_not_saturated_render_blocks: 0,
            blocks_with_active_render: 0,
            capture_signal_saturation: false,
        }
    }

    pub fn usable_linear_estimate(&self) -> bool {
        self.filter_quality_state.linear_filter_usable() && self.config.filter.use_linear_filter
    }

    pub fn use_linear_filter_output(&self) -> bool {
        self.usable_linear_estimate()
    }

    pub fn active_render(&self) -> bool {
        self.blocks_with_active_render > 200
    }

    pub fn get_residual_echo_scaling(&self, residual_scaling: &mut [f32]) {
        let limit_blocks = if self.config.filter.conservative_initial_phase {
            1.5 * NUM_BLOCKS_PER_SECOND as f32
        } else {
            0.8 * NUM_BLOCKS_PER_SECOND as f32
        };
        let filter_has_had_time_to_converge =
            self.strong_not_saturated_render_blocks as f32 >= limit_blocks;
        self.echo_audibility
            .get_residual_echo_scaling(filter_has_had_time_to_converge, residual_scaling);
    }

    pub fn use_stationarity_properties(&self) -> bool {
        self.config.echo_audibility.use_stationarity_properties
    }

    pub fn erle(&self, onset_compensated: bool) -> &[[f32; FFT_LENGTH_BY_2_PLUS_1]] {
        self.erle_estimator.erle(onset_compensated)
    }

    /// The uncapped ERLE, used for the dominant nearend decision.
    pub fn erle_unbounded(&self) -> &[[f32; FFT_LENGTH_BY_2_PLUS_1]] {
        self.erle_estimator.erle_unbounded()
    }

    pub fn fullband_erle_log2(&self) -> f32 {
        self.erle_estimator.fullband_erle_log2()
    }

    pub fn erl(&self) -> &[f32; FFT_LENGTH_BY_2_PLUS_1] {
        self.erl_estimator.erl()
    }

    pub fn erl_time_domain(&self) -> f32 {
        self.erl_estimator.erl_time_domain()
    }

    pub fn min_direct_path_filter_delay(&self) -> i32 {
        self.delay_state.min_direct_path_filter_delay()
    }

    pub fn saturated_capture(&self) -> bool {
        self.capture_signal_saturation
    }

    pub fn saturated_echo(&self) -> bool {
        self.saturation_detector.saturated_echo()
    }

    pub fn update_capture_saturation(&mut self, capture_signal_saturation: bool) {
        self.capture_signal_saturation = capture_signal_saturation;
    }

    pub fn transparent_mode(&self) -> bool {
        self.transparent_state.active()
    }

    pub fn handle_echo_path_change(&mut self, variability: &EchoPathVariability) {
        if !matches!(variability.delay_change, DelayAdjustment::None) {
            self.full_reset();
        } else if variability.gain_change {
            self.erle_estimator.reset(false);
        }
        self.subtractor_output_analyzer.handle_echo_path_change();
    }

    fn full_reset(&mut self) {
        self.filter_analyzer.reset();
        self.capture_signal_saturation = false;
        self.strong_not_saturated_render_blocks = 0;
        self.blocks_with_active_render = 0;
        self.initial_state.reset();
        self.transparent_state.reset();
        self.erle_estimator.reset(true);
        self.erl_estimator.reset();
        self.filter_quality_state.reset();
    }

    pub fn reverb_decay(&self, mild: bool) -> f32 {
        self.reverb_model_estimator.reverb_decay(mild)
    }

    pub fn get_reverb_frequency_response(&self) -> &[f32; FFT_LENGTH_BY_2_PLUS_1] {
        self.reverb_model_estimator.get_reverb_frequency_response()
    }

    pub fn transition_triggered(&self) -> bool {
        self.initial_state.transition_triggered()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        external_delay: Option<DelayEstimate>,
        adaptive_filter_frequency_responses: &[Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>],
        adaptive_filter_impulse_responses: &[Vec<f32>],
        render_buffer: &RenderBuffer<'_>,
        e2_main: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
        y2: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
        subtractor_output: &[SubtractorOutput],
    ) {
        assert_eq!(self.num_capture_channels, e2_main.len());
        assert_eq!(self.num_capture_channels, y2.len());
        assert_eq!(self.num_capture_channels, subtractor_output.len());
        assert_eq!(
            self.num_capture_channels,
            adaptive_filter_frequency_responses.len()
        );
        assert_eq!(
            self.num_capture_channels,
            adaptive_filter_impulse_responses.len()
        );

        let mut any_filter_converged = false;
        let mut any_coarse_filter_converged = false;
        let mut all_filters_diverged = false;
        self.subtractor_output_analyzer.update(
            subtractor_output,
            &mut any_filter_converged,
            &mut any_coarse_filter_converged,
            &mut all_filters_diverged,
        );

        let mut any_filter_consistent = false;
        let mut max_echo_path_gain = 0.0f32;
        self.filter_analyzer.update(
            adaptive_filter_impulse_responses,
            render_buffer,
            &mut any_filter_consistent,
            &mut max_echo_path_gain,
        );

        if self.config.filter.use_linear_filter {
            self.delay_state.update(
                self.filter_analyzer.filter_delays_blocks(),
                external_delay,
                self.strong_not_saturated_render_blocks,
            );
        }

        let min_delay = self.delay_state.min_direct_path_filter_delay();
        let aligned_render_block = render_buffer.block(-(min_delay as isize));

        let limit = self.config.render_levels.active_render_limit
            * self.config.render_levels.active_render_limit
            * FFT_LENGTH_BY_2 as f32;
        let mut active_render = false;
        for channel in aligned_render_block.band_channels(0) {
            let energy: f32 = channel.iter().map(|sample| sample * sample).sum();
            if energy > limit {
                active_render = true;
                break;
            }
        }
        if active_render {
            self.blocks_with_active_render = self.blocks_with_active_render.saturating_add(1);
        }
        if active_render && !self.saturated_capture() {
            self.strong_not_saturated_render_blocks =
                self.strong_not_saturated_render_blocks.saturating_add(1);
        }

        let mut avg_render_spectrum_with_reverb = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
        compute_avg_render_reverb(
            render_buffer.spectrum_buffer(),
            min_delay,
            self.reverb_decay(/*mild=*/ false),
            &mut self.avg_render_reverb,
            &mut avg_render_spectrum_with_reverb,
        );

        if self.use_stationarity_properties() {
            self.echo_audibility.update(
                render_buffer,
                self.avg_render_reverb.reverb(),
                min_delay,
                self.delay_state.external_delay_reported(),
            );
        }

        if self.initial_state.transition_triggered() {
            self.erle_estimator.reset(false);
        }

        self.erle_estimator.update(
            render_buffer,
            adaptive_filter_frequency_responses,
            &avg_render_spectrum_with_reverb,
            y2,
            e2_main,
            self.subtractor_output_analyzer.converged_filters(),
        );

        self.erl_estimator.update(
            self.subtractor_output_analyzer.converged_filters(),
            render_buffer.spectrum(min_delay as isize),
            y2,
        );

        self.saturation_detector.update(
            aligned_render_block,
            self.saturated_capture(),
            self.usable_linear_estimate(),
            subtractor_output,
            max_echo_path_gain,
        );

        self.initial_state
            .update(active_render, self.saturated_capture());

        self.transparent_state.update(
            min_delay,
            any_filter_consistent,
            any_filter_converged,
            any_coarse_filter_converged,
            all_filters_diverged,
            active_render,
            self.saturated_capture(),
        );

        self.filter_quality_state.update(
            active_render,
            self.transparent_mode(),
            self.saturated_capture(),
            external_delay,
            any_filter_converged,
        );

        let stationary_block =
            self.use_stationarity_properties() && self.echo_audibility.is_block_stationary();

        self.reverb_model_estimator.update(
            self.filter_analyzer.adjusted_filters(),
            adaptive_filter_frequency_responses,
            self.erle_estimator.get_inst_linear_quality_estimates(),
            self.delay_state.direct_path_filter_delays(),
            self.filter_quality_state.usable_linear_filter_outputs(),
            stationary_block,
        );

        self.erle_estimator.dump(&self.data_dumper);
        self.reverb_model_estimator.dump(&self.data_dumper);
        self.data_dumper
            .dump_raw_f32_slice(DiagnosticLevel::DeepDebug, "aec3_erl", self.erl());
        self.data_dumper.dump_raw_f32(
            DiagnosticLevel::Developer,
            "aec3_erl_time_domain",
            self.erl_time_domain(),
        );
        self.data_dumper.dump_raw_f32_slice(
            DiagnosticLevel::DeepDebug,
            "aec3_erle",
            &self.erle(/*onset_compensated=*/ false)[0],
        );
        self.data_dumper.dump_raw_f32(
            DiagnosticLevel::Production,
            "aec3_usable_linear_estimate",
            if self.usable_linear_estimate() {
                1.0
            } else {
                0.0
            },
        );
        self.data_dumper.dump_raw_f32(
            DiagnosticLevel::Production,
            "aec3_transparent_mode",
            if self.transparent_mode() { 1.0 } else { 0.0 },
        );
        self.data_dumper.dump_raw_i32(
            DiagnosticLevel::Production,
            "aec3_filter_delay",
            self.filter_analyzer.min_filter_delay_blocks(),
        );
        self.data_dumper.dump_raw_f32(
            DiagnosticLevel::Production,
            "aec3_any_filter_consistent",
            if any_filter_consistent { 1.0 } else { 0.0 },
        );
        self.data_dumper.dump_raw_f32(
            DiagnosticLevel::Production,
            "aec3_initial_state",
            if self.initial_state.initial_state_active() {
                1.0
            } else {
                0.0
            },
        );
        self.data_dumper.dump_raw_f32(
            DiagnosticLevel::Production,
            "aec3_capture_saturation",
            if self.saturated_capture() { 1.0 } else { 0.0 },
        );
        self.data_dumper.dump_raw_f32(
            DiagnosticLevel::Production,
            "aec3_echo_saturation",
            if self.saturated_echo() { 1.0 } else { 0.0 },
        );
        self.data_dumper.dump_raw_f32(
            DiagnosticLevel::Production,
            "aec3_any_filter_converged",
            if any_filter_converged { 1.0 } else { 0.0 },
        );
        self.data_dumper.dump_raw_f32(
            DiagnosticLevel::Production,
            "aec3_all_filters_diverged",
            if all_filters_diverged { 1.0 } else { 0.0 },
        );
        self.data_dumper.dump_raw_f32(
            DiagnosticLevel::Production,
            "aec3_external_delay_avaliable",
            if external_delay.is_some() { 1.0 } else { 0.0 },
        );
        self.data_dumper.dump_raw_f32_slice(
            DiagnosticLevel::DeepDebug,
            "aec3_filter_tail_freq_resp_est",
            self.get_reverb_frequency_response(),
        );
    }

    pub fn filter_length_blocks(&self) -> usize {
        self.filter_analyzer.filter_length_blocks()
    }
}

impl EchoRemoverMetricsAecState for AecState {
    fn erl(&self) -> &[f32; FFT_LENGTH_BY_2_PLUS_1] {
        self.erl_estimator.erl()
    }

    fn erl_time_domain(&self) -> f32 {
        self.erl_estimator.erl_time_domain()
    }

    fn erle(&self) -> &[f32; FFT_LENGTH_BY_2_PLUS_1] {
        &self.erle_estimator.erle(/*onset_compensated=*/ false)[0]
    }

    fn fullband_erle_log2(&self) -> f32 {
        self.erle_estimator.fullband_erle_log2()
    }

    fn active_render(&self) -> bool {
        self.active_render()
    }

    fn saturated_capture(&self) -> bool {
        self.saturated_capture()
    }

    fn usable_linear_estimate(&self) -> bool {
        self.usable_linear_estimate()
    }

    fn min_direct_path_filter_delay(&self) -> i32 {
        self.min_direct_path_filter_delay()
    }
}

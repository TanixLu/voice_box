use crate::api::config::EchoCanceller3Config;
use crate::audio_processing::aec3::aec3_common::{
    BLOCK_SIZE, MATCHED_FILTER_ALIGNMENT_SHIFT_SIZE_SUB_BLOCKS,
    MATCHED_FILTER_WINDOW_SIZE_SUB_BLOCKS, NUM_BLOCKS_PER_SECOND, detect_optimization,
};
use crate::audio_processing::aec3::alignment_mixer::AlignmentMixer;
use crate::audio_processing::aec3::block::Block;
use crate::audio_processing::aec3::clockdrift_detector::{ClockDriftDetector, ClockDriftLevel};
use crate::audio_processing::aec3::decimator::Decimator;
use crate::audio_processing::aec3::delay_estimate::{DelayEstimate, DelayEstimateQuality};
use crate::audio_processing::aec3::downsampled_render_buffer::DownsampledRenderBuffer;
use crate::audio_processing::aec3::matched_filter::MatchedFilter;
use crate::audio_processing::aec3::matched_filter_lag_aggregator::MatchedFilterLagAggregator;

/// Estimates the render/capture delay by correlating the decimated capture signal
/// with the downsampled render buffer produced by the render delay buffer.
pub struct EchoPathDelayEstimator {
    down_sampling_factor: usize,
    sub_block_size: usize,
    capture_mixer: AlignmentMixer,
    capture_decimator: Decimator,
    matched_filter: MatchedFilter,
    matched_filter_lag_aggregator: MatchedFilterLagAggregator,
    old_aggregated_lag: Option<DelayEstimate>,
    consistent_estimate_counter: usize,
    clockdrift_detector: ClockDriftDetector,
}

impl EchoPathDelayEstimator {
    pub fn new(config: &EchoCanceller3Config, num_capture_channels: usize) -> Self {
        assert!(num_capture_channels > 0);
        let down_sampling_factor = config.delay.down_sampling_factor;
        assert!(down_sampling_factor > 0);
        assert_eq!(0, BLOCK_SIZE % down_sampling_factor);
        let sub_block_size = BLOCK_SIZE / down_sampling_factor;

        let capture_mixer = AlignmentMixer::new(
            num_capture_channels,
            config.delay.capture_alignment_mixing.clone(),
        );
        let capture_decimator = Decimator::new(down_sampling_factor);
        let excitation_limit = if down_sampling_factor == 8 {
            config.render_levels.poor_excitation_render_limit_ds8
        } else {
            config.render_levels.poor_excitation_render_limit
        };
        let matched_filter = MatchedFilter::new(
            detect_optimization(),
            sub_block_size,
            MATCHED_FILTER_WINDOW_SIZE_SUB_BLOCKS,
            config.delay.num_filters,
            MATCHED_FILTER_ALIGNMENT_SHIFT_SIZE_SUB_BLOCKS,
            excitation_limit,
            config.delay.delay_estimate_smoothing,
            config.delay.delay_estimate_smoothing_delay_found,
            config.delay.delay_candidate_detection_threshold,
            config.delay.detect_pre_echo,
        );
        let matched_filter_lag_aggregator =
            MatchedFilterLagAggregator::new(matched_filter.max_filter_lag(), &config.delay);

        Self {
            down_sampling_factor,
            sub_block_size,
            capture_mixer,
            capture_decimator,
            matched_filter,
            matched_filter_lag_aggregator,
            old_aggregated_lag: None,
            consistent_estimate_counter: 0,
            clockdrift_detector: ClockDriftDetector::new(),
        }
    }

    pub fn reset(&mut self, reset_delay_confidence: bool) {
        self.reset_internal(true, reset_delay_confidence);
    }

    pub fn estimate_delay(
        &mut self,
        render_buffer: &DownsampledRenderBuffer,
        capture: &Block,
    ) -> Option<DelayEstimate> {
        let mut downmixed_capture = [0.0f32; BLOCK_SIZE];
        self.capture_mixer
            .produce_output(capture, &mut downmixed_capture);

        let mut downsampled_capture_storage = [0.0f32; BLOCK_SIZE];
        let downsampled_capture = &mut downsampled_capture_storage[..self.sub_block_size];
        self.capture_decimator
            .decimate(&downmixed_capture, downsampled_capture);

        self.matched_filter.update(
            render_buffer,
            downsampled_capture,
            self.matched_filter_lag_aggregator.reliable_delay_found(),
        );

        let mut aggregated = self
            .matched_filter_lag_aggregator
            .aggregate(self.matched_filter.best_lag_estimate());

        if let Some(estimate) = aggregated {
            if matches!(estimate.quality, DelayEstimateQuality::Refined) {
                self.clockdrift_detector.update(estimate.delay as i32);
            }
        }

        if let Some(estimate) = aggregated.as_mut() {
            estimate.delay *= self.down_sampling_factor;
        }

        let previous = self.old_aggregated_lag;
        if let (Some(prev), Some(current)) = (previous, aggregated) {
            if prev.delay == current.delay {
                self.consistent_estimate_counter += 1;
            } else {
                self.consistent_estimate_counter = 0;
            }
        } else {
            self.consistent_estimate_counter = 0;
        }

        self.old_aggregated_lag = aggregated;
        if self.consistent_estimate_counter > (NUM_BLOCKS_PER_SECOND / 2) {
            self.reset_internal(false, false);
        }

        aggregated
    }

    pub fn clockdrift(&self) -> ClockDriftLevel {
        self.clockdrift_detector.level()
    }

    fn reset_internal(&mut self, reset_lag_aggregator: bool, reset_delay_confidence: bool) {
        if reset_lag_aggregator {
            self.matched_filter_lag_aggregator
                .reset(reset_delay_confidence);
        }
        self.matched_filter
            .reset(/*full_reset=*/ reset_lag_aggregator);
        self.old_aggregated_lag = None;
        self.consistent_estimate_counter = 0;
    }
}

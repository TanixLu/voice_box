use crate::api::config::EchoCanceller3Config;
use crate::audio_processing::aec3::aec3_common::{
    BLOCK_SIZE_LOG2, NUM_BLOCKS_PER_SECOND, valid_full_band_rate,
};
use crate::audio_processing::aec3::block::Block;
use crate::audio_processing::aec3::clockdrift_detector::ClockDriftLevel;
use crate::audio_processing::aec3::delay_estimate::{DelayEstimate, DelayEstimateQuality};
use crate::audio_processing::aec3::downsampled_render_buffer::DownsampledRenderBuffer;
use crate::audio_processing::aec3::echo_path_delay_estimator::EchoPathDelayEstimator;
use crate::audio_processing::aec3::render_delay_controller_metrics::RenderDelayControllerMetrics;
use crate::audio_processing::logging::apm_data_dumper::{ApmDataDumper, DiagnosticLevel};

/// Aligns the render buffer content with the capture signal by selecting a delay that
/// accounts for buffer headroom and hysteresis.
pub struct RenderDelayController {
    data_dumper: ApmDataDumper,
    hysteresis_limit_blocks: usize,
    delay: Option<DelayEstimate>,
    delay_estimator: EchoPathDelayEstimator,
    metrics: RenderDelayControllerMetrics,
    delay_samples: Option<DelayEstimate>,
    capture_call_counter: usize,
    delay_change_counter: usize,
    last_delay_estimate_quality: DelayEstimateQuality,
}

impl RenderDelayController {
    pub fn new(
        config: &EchoCanceller3Config,
        sample_rate_hz: i32,
        num_capture_channels: usize,
    ) -> Self {
        assert!(valid_full_band_rate(sample_rate_hz));
        let data_dumper = ApmDataDumper::new_unique();
        let delay_estimator = EchoPathDelayEstimator::new(config, num_capture_channels);
        delay_estimator.log_delay_estimation_properties(sample_rate_hz, 0);

        Self {
            data_dumper,
            hysteresis_limit_blocks: config.delay.hysteresis_limit_blocks,
            delay: None,
            delay_estimator,
            metrics: RenderDelayControllerMetrics::new(),
            delay_samples: None,
            capture_call_counter: 0,
            delay_change_counter: 0,
            last_delay_estimate_quality: DelayEstimateQuality::Coarse,
        }
    }

    pub fn reset(&mut self, reset_delay_confidence: bool) {
        self.delay = None;
        self.delay_samples = None;
        self.delay_estimator.reset(reset_delay_confidence);
        self.delay_change_counter = 0;
        if reset_delay_confidence {
            self.last_delay_estimate_quality = DelayEstimateQuality::Coarse;
        }
    }

    pub fn log_render_call(&mut self) {}

    pub fn get_delay(
        &mut self,
        render_buffer: &DownsampledRenderBuffer,
        render_delay_buffer_delay: usize,
        capture: &Block,
    ) -> Option<DelayEstimate> {
        let _ = render_delay_buffer_delay;

        self.capture_call_counter += 1;

        let delay_samples = self.delay_estimator.estimate_delay(render_buffer, capture);

        if let Some(estimate) = delay_samples {
            let delay_changed = self
                .delay_samples
                .map(|stored| stored.delay != estimate.delay)
                .unwrap_or(true);
            if delay_changed {
                self.delay_change_counter = 0;
            }

            if let Some(mut stored) = self.delay_samples {
                if stored.delay == estimate.delay {
                    stored.blocks_since_last_change += 1;
                } else {
                    stored.blocks_since_last_change = 0;
                }
                stored.blocks_since_last_update = 0;
                stored.delay = estimate.delay;
                stored.quality = estimate.quality;
                self.delay_samples = Some(stored);
            } else {
                self.delay_samples = Some(estimate);
            }
        } else if let Some(mut stored) = self.delay_samples {
            stored.blocks_since_last_change += 1;
            stored.blocks_since_last_update += 1;
            self.delay_samples = Some(stored);
        }

        if self.delay_change_counter < 2 * NUM_BLOCKS_PER_SECOND {
            self.delay_change_counter += 1;
        }

        if let Some(delay_samples) = self.delay_samples {
            let use_hysteresis = self.last_delay_estimate_quality == DelayEstimateQuality::Refined
                && delay_samples.quality == DelayEstimateQuality::Refined;
            let hysteresis = if use_hysteresis {
                self.hysteresis_limit_blocks
            } else {
                0
            };
            let new_delay = compute_buffer_delay(self.delay.as_ref(), hysteresis, delay_samples);
            self.delay = Some(new_delay);
            self.last_delay_estimate_quality = delay_samples.quality;
        }

        let delay_samples_value = self.delay_samples.map(|d| d.delay);
        let buffer_delay_blocks = self.delay.map(|d| d.delay).unwrap_or(0);
        self.metrics.update(
            delay_samples_value,
            buffer_delay_blocks,
            None,
            self.delay_estimator.clockdrift(),
        );

        self.data_dumper.dump_raw_usize(
            DiagnosticLevel::Production,
            "aec3_render_delay_controller_delay",
            delay_samples_value.unwrap_or(0),
        );
        self.data_dumper.dump_raw_usize(
            DiagnosticLevel::Production,
            "aec3_render_delay_controller_buffer_delay",
            buffer_delay_blocks,
        );

        self.delay
    }

    pub fn has_clockdrift(&self) -> bool {
        self.delay_estimator.clockdrift() != ClockDriftLevel::None
    }
}

/// The delay headroom is already subtracted by `MatchedFilterLagAggregator`, in
/// downsampled units.
fn compute_buffer_delay(
    current_delay: Option<&DelayEstimate>,
    hysteresis_limit_blocks: usize,
    mut estimated_delay: DelayEstimate,
) -> DelayEstimate {
    let mut new_delay_blocks = estimated_delay.delay >> BLOCK_SIZE_LOG2;

    if let Some(current) = current_delay {
        if new_delay_blocks > current.delay
            && new_delay_blocks <= current.delay + hysteresis_limit_blocks
        {
            new_delay_blocks = current.delay;
        }
    }

    estimated_delay.delay = new_delay_blocks;
    estimated_delay
}

use crate::api::config::EchoCanceller3Config;
use crate::audio_processing::aec3::aec3_common::{BLOCK_SIZE_LOG2, valid_full_band_rate};
use crate::audio_processing::aec3::block::Block;
use crate::audio_processing::aec3::clockdrift_detector::ClockDriftLevel;
use crate::audio_processing::aec3::delay_estimate::{DelayEstimate, DelayEstimateQuality};
use crate::audio_processing::aec3::downsampled_render_buffer::DownsampledRenderBuffer;
use crate::audio_processing::aec3::echo_path_delay_estimator::EchoPathDelayEstimator;

/// Aligns the render buffer content with the capture signal by selecting a delay that
/// accounts for buffer headroom and hysteresis.
pub struct RenderDelayController {
    hysteresis_limit_blocks: usize,
    delay: Option<DelayEstimate>,
    delay_estimator: EchoPathDelayEstimator,

    delay_samples: Option<DelayEstimate>,
    last_delay_estimate_quality: DelayEstimateQuality,
}

impl RenderDelayController {
    pub fn new(
        config: &EchoCanceller3Config,
        sample_rate_hz: i32,
        num_capture_channels: usize,
    ) -> Self {
        assert!(valid_full_band_rate(sample_rate_hz));
        let delay_estimator = EchoPathDelayEstimator::new(config, num_capture_channels);

        Self {
            hysteresis_limit_blocks: config.delay.hysteresis_limit_blocks,
            delay: None,
            delay_estimator,

            delay_samples: None,
            last_delay_estimate_quality: DelayEstimateQuality::Coarse,
        }
    }

    pub fn reset(&mut self, reset_delay_confidence: bool) {
        self.delay = None;
        self.delay_samples = None;
        self.delay_estimator.reset(reset_delay_confidence);
        if reset_delay_confidence {
            self.last_delay_estimate_quality = DelayEstimateQuality::Coarse;
        }
    }

    pub fn get_delay(
        &mut self,
        render_buffer: &DownsampledRenderBuffer,
        capture: &Block,
    ) -> Option<DelayEstimate> {
        let delay_samples = self.delay_estimator.estimate_delay(render_buffer, capture);

        if let Some(estimate) = delay_samples {
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

use crate::api::config::EchoCanceller3Config;

use super::aec3_common::FFT_LENGTH_BY_2_PLUS_1;
use super::reverb_decay_estimator::ReverbDecayEstimator;
use super::reverb_frequency_response::ReverbFrequencyResponse;

pub struct ReverbModelEstimator {
    reverb_decay_estimators: Vec<ReverbDecayEstimator>,
    reverb_frequency_responses: Vec<ReverbFrequencyResponse>,
}

impl ReverbModelEstimator {
    pub fn new(config: &EchoCanceller3Config, num_capture_channels: usize) -> Self {
        let mut decay_estimators = Vec::with_capacity(num_capture_channels);
        let mut frequency_responses = Vec::with_capacity(num_capture_channels);
        for _ in 0..num_capture_channels {
            decay_estimators.push(ReverbDecayEstimator::new(config));
            frequency_responses.push(ReverbFrequencyResponse::new(
                config.ep_strength.use_conservative_tail_frequency_response,
            ));
        }
        Self {
            reverb_decay_estimators: decay_estimators,
            reverb_frequency_responses: frequency_responses,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        impulse_responses: &[Vec<f32>],
        frequency_responses: &[Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>],
        linear_filter_qualities: &[Option<f32>],
        filter_delays_blocks: &[i32],
        usable_linear_estimates: &[bool],
        stationary_block: bool,
    ) {
        let num_capture_channels = self.reverb_decay_estimators.len();
        assert_eq!(impulse_responses.len(), num_capture_channels);
        assert_eq!(frequency_responses.len(), num_capture_channels);
        assert_eq!(linear_filter_qualities.len(), num_capture_channels);
        assert_eq!(filter_delays_blocks.len(), num_capture_channels);
        assert_eq!(usable_linear_estimates.len(), num_capture_channels);

        for ch in 0..num_capture_channels {
            self.reverb_frequency_responses[ch].update(
                &frequency_responses[ch],
                filter_delays_blocks[ch],
                linear_filter_qualities[ch],
                stationary_block,
            );
            self.reverb_decay_estimators[ch].update(
                &impulse_responses[ch],
                linear_filter_qualities[ch],
                filter_delays_blocks[ch],
                usable_linear_estimates[ch],
                stationary_block,
            );
        }
    }

    pub fn reverb_decay(&self, mild: bool) -> f32 {
        self.reverb_decay_estimators[0].decay(mild)
    }

    pub fn get_reverb_frequency_response(&self) -> &[f32; FFT_LENGTH_BY_2_PLUS_1] {
        self.reverb_frequency_responses[0].frequency_response()
    }
}

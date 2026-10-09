use crate::api::config::{Delay, DelaySelectionThresholds};
use crate::audio_processing::aec3::aec3_common::{
    BLOCK_SIZE_LOG2, MATCHED_FILTER_WINDOW_SIZE_SUB_BLOCKS, NUM_BLOCKS_PER_SECOND,
};
use crate::audio_processing::aec3::delay_estimate::{DelayEstimate, DelayEstimateQuality};
use crate::audio_processing::aec3::matched_filter::LagEstimate;

const HISTOGRAM_DATA_SIZE: usize = 250;
const PRE_ECHO_HISTOGRAM_DATA_NOT_UPDATED: i32 = -1;

/// Number of bits to shift a downsampled lag by to get a block index.
fn down_sampling_block_size_log2(down_sampling_factor: usize) -> u32 {
    let down_sampling_factor_log2 = down_sampling_factor.max(1).ilog2();
    (BLOCK_SIZE_LOG2 as u32).saturating_sub(down_sampling_factor_log2)
}

/// Tracks the most frequent lag over the recent history.
struct HighestPeakAggregator {
    histogram: Vec<i32>,
    histogram_data: [usize; HISTOGRAM_DATA_SIZE],
    histogram_data_index: usize,
    candidate: i32,
}

impl HighestPeakAggregator {
    fn new(max_filter_lag: usize) -> Self {
        Self {
            histogram: vec![0; max_filter_lag + 1],
            histogram_data: [0; HISTOGRAM_DATA_SIZE],
            histogram_data_index: 0,
            candidate: -1,
        }
    }

    fn reset(&mut self) {
        self.histogram.fill(0);
        self.histogram_data.fill(0);
        self.histogram_data_index = 0;
    }

    fn aggregate(&mut self, lag: usize) {
        let lag = lag.min(self.histogram.len() - 1);
        self.histogram[self.histogram_data[self.histogram_data_index]] -= 1;
        self.histogram_data[self.histogram_data_index] = lag;
        self.histogram[lag] += 1;
        self.histogram_data_index = (self.histogram_data_index + 1) % HISTOGRAM_DATA_SIZE;
        self.candidate = self
            .histogram
            .iter()
            .enumerate()
            .max_by_key(|(_, value)| *value)
            .map(|(idx, _)| idx as i32)
            .unwrap_or(-1);
    }

    fn candidate(&self) -> i32 {
        self.candidate
    }

    fn histogram(&self) -> &[i32] {
        &self.histogram
    }
}

/// Tracks the most frequent pre-echo lag, in blocks, over the recent history.
struct PreEchoLagAggregator {
    block_size_log2: u32,
    histogram_data: [i32; HISTOGRAM_DATA_SIZE],
    histogram: Vec<i32>,
    histogram_data_index: usize,
    pre_echo_candidate: i32,
    number_updates: usize,
}

impl PreEchoLagAggregator {
    fn new(max_filter_lag: usize, down_sampling_factor: usize) -> Self {
        let histogram_len = ((max_filter_lag + 1) * down_sampling_factor) >> BLOCK_SIZE_LOG2;
        let mut instance = Self {
            block_size_log2: down_sampling_block_size_log2(down_sampling_factor),
            histogram_data: [PRE_ECHO_HISTOGRAM_DATA_NOT_UPDATED; HISTOGRAM_DATA_SIZE],
            histogram: vec![0; histogram_len.max(1)],
            histogram_data_index: 0,
            pre_echo_candidate: 0,
            number_updates: 0,
        };
        instance.reset();
        instance
    }

    fn reset(&mut self) {
        self.histogram.fill(0);
        self.histogram_data
            .fill(PRE_ECHO_HISTOGRAM_DATA_NOT_UPDATED);
        self.histogram_data_index = 0;
        self.pre_echo_candidate = 0;
    }

    fn aggregate(&mut self, pre_echo_lag: usize) {
        let pre_echo_block_size = ((pre_echo_lag >> self.block_size_log2) as i32)
            .clamp(0, self.histogram.len() as i32 - 1);

        // Drop the oldest point, skipping the initial unfilled entries.
        let oldest = self.histogram_data[self.histogram_data_index];
        if oldest != PRE_ECHO_HISTOGRAM_DATA_NOT_UPDATED {
            self.histogram[oldest as usize] -= 1;
        }
        self.histogram_data[self.histogram_data_index] = pre_echo_block_size;
        self.histogram[pre_echo_block_size as usize] += 1;
        self.histogram_data_index = (self.histogram_data_index + 1) % HISTOGRAM_DATA_SIZE;

        let mut pre_echo_candidate_block_size = 0usize;
        if self.number_updates < NUM_BLOCKS_PER_SECOND * 2 {
            self.number_updates += 1;
            // Favour earlier windows while the histogram is still filling.
            let mut penalization_per_delay = 1.0f32;
            let mut max_histogram_value = -1.0f32;
            let mut window_start = 0usize;
            while window_start + MATCHED_FILTER_WINDOW_SIZE_SUB_BLOCKS <= self.histogram.len() {
                let window = &self.histogram
                    [window_start..window_start + MATCHED_FILTER_WINDOW_SIZE_SUB_BLOCKS];
                let (offset, &value) = window
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, value)| *value)
                    .expect("non-empty window");
                let weighted_max_value = value as f32 * penalization_per_delay;
                if weighted_max_value > max_histogram_value {
                    max_histogram_value = weighted_max_value;
                    pre_echo_candidate_block_size = window_start + offset;
                }
                penalization_per_delay *= 0.7;
                window_start += MATCHED_FILTER_WINDOW_SIZE_SUB_BLOCKS;
            }
        } else {
            pre_echo_candidate_block_size = self
                .histogram
                .iter()
                .enumerate()
                .max_by_key(|(_, value)| *value)
                .map(|(idx, _)| idx)
                .unwrap_or(0);
        }
        self.pre_echo_candidate = (pre_echo_candidate_block_size << self.block_size_log2) as i32;
    }

    fn pre_echo_candidate(&self) -> i32 {
        self.pre_echo_candidate
    }
}

pub struct MatchedFilterLagAggregator {
    significant_candidate_found: bool,
    thresholds: DelaySelectionThresholds,
    headroom: usize,
    highest_peak_aggregator: HighestPeakAggregator,
    pre_echo_lag_aggregator: Option<PreEchoLagAggregator>,
}

impl MatchedFilterLagAggregator {
    pub fn new(max_filter_lag: usize, delay_config: &Delay) -> Self {
        assert!(delay_config.down_sampling_factor > 0);
        assert!(
            delay_config.delay_selection_thresholds.initial
                <= delay_config.delay_selection_thresholds.converged
        );
        Self {
            significant_candidate_found: false,
            thresholds: delay_config.delay_selection_thresholds.clone(),
            headroom: delay_config.delay_headroom_samples / delay_config.down_sampling_factor,
            highest_peak_aggregator: HighestPeakAggregator::new(max_filter_lag),
            pre_echo_lag_aggregator: delay_config.detect_pre_echo.then(|| {
                PreEchoLagAggregator::new(max_filter_lag, delay_config.down_sampling_factor)
            }),
        }
    }

    pub fn reset(&mut self, hard_reset: bool) {
        self.highest_peak_aggregator.reset();
        if let Some(aggregator) = self.pre_echo_lag_aggregator.as_mut() {
            aggregator.reset();
        }
        if hard_reset {
            self.significant_candidate_found = false;
        }
    }

    /// Whether a delay candidate has been seen often enough to be trusted.
    pub fn reliable_delay_found(&self) -> bool {
        self.significant_candidate_found
    }

    pub fn aggregate(&mut self, lag_estimate: Option<LagEstimate>) -> Option<DelayEstimate> {
        let lag_estimate = lag_estimate?;

        if let Some(aggregator) = self.pre_echo_lag_aggregator.as_mut() {
            aggregator.aggregate(lag_estimate.pre_echo_lag.saturating_sub(self.headroom));
        }

        self.highest_peak_aggregator
            .aggregate(lag_estimate.lag.saturating_sub(self.headroom));

        let candidate = self.highest_peak_aggregator.candidate();
        let count = self.highest_peak_aggregator.histogram()[candidate as usize];
        if count > self.thresholds.converged {
            self.significant_candidate_found = true;
        }

        if count > self.thresholds.converged
            || (count > self.thresholds.initial && !self.significant_candidate_found)
        {
            let quality = if self.significant_candidate_found {
                DelayEstimateQuality::Refined
            } else {
                DelayEstimateQuality::Coarse
            };
            let reported_delay = match self.pre_echo_lag_aggregator.as_ref() {
                Some(aggregator) => aggregator.pre_echo_candidate() as usize,
                None => candidate as usize,
            };
            return Some(DelayEstimate::new(quality, reported_delay));
        }

        None
    }
}

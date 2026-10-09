use crate::api::config::EchoCanceller3Config;
use crate::audio_processing::aec3::aec3_common::FFT_LENGTH_BY_2_PLUS_1;
use crate::audio_processing::aec3::fullband_erle_estimator::FullBandErleEstimator;
use crate::audio_processing::aec3::render_buffer::RenderBuffer;
use crate::audio_processing::aec3::signal_dependent_erle_estimator::SignalDependentErleEstimator;
use crate::audio_processing::aec3::subband_erle_estimator::SubbandErleEstimator;

pub struct ErleEstimator {
    startup_phase_length_blocks: usize,
    fullband_erle_estimator: FullBandErleEstimator,
    subband_erle_estimator: SubbandErleEstimator,
    signal_dependent_erle_estimator: Option<SignalDependentErleEstimator>,
    blocks_since_reset: usize,
}

impl ErleEstimator {
    pub fn new(
        startup_phase_length_blocks: usize,
        config: &EchoCanceller3Config,
        num_capture_channels: usize,
    ) -> Self {
        let fullband_erle_estimator =
            FullBandErleEstimator::new(&config.erle, num_capture_channels);
        let subband_erle_estimator = SubbandErleEstimator::new(config, num_capture_channels);
        let signal_dependent_erle_estimator = if config.erle.num_sections > 1 {
            Some(SignalDependentErleEstimator::new(
                config,
                num_capture_channels,
            ))
        } else {
            None
        };
        let mut instance = Self {
            startup_phase_length_blocks,
            fullband_erle_estimator,
            subband_erle_estimator,
            signal_dependent_erle_estimator,
            blocks_since_reset: 0,
        };
        instance.reset(true);
        instance
    }

    pub fn reset(&mut self, delay_change: bool) {
        self.fullband_erle_estimator.reset();
        self.subband_erle_estimator.reset();
        if let Some(signal_dependent) = &mut self.signal_dependent_erle_estimator {
            signal_dependent.reset();
        }
        if delay_change {
            self.blocks_since_reset = 0;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        render_buffer: &RenderBuffer<'_>,
        filter_frequency_responses: &[Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>],
        avg_render_spectrum_with_reverb: &[f32; FFT_LENGTH_BY_2_PLUS_1],
        capture_spectra: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
        subtractor_spectra: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
        converged_filters: &[bool],
    ) {
        self.blocks_since_reset += 1;
        if self.blocks_since_reset < self.startup_phase_length_blocks {
            return;
        }

        self.subband_erle_estimator.update(
            avg_render_spectrum_with_reverb,
            capture_spectra,
            subtractor_spectra,
            converged_filters,
        );

        if let Some(signal_dependent) = &mut self.signal_dependent_erle_estimator {
            signal_dependent.update(
                render_buffer,
                filter_frequency_responses,
                avg_render_spectrum_with_reverb,
                capture_spectra,
                subtractor_spectra,
                self.subband_erle_estimator
                    .erle(/*onset_compensated=*/ false),
                self.subband_erle_estimator
                    .erle(/*onset_compensated=*/ true),
                converged_filters,
            );
        }

        self.fullband_erle_estimator.update(
            avg_render_spectrum_with_reverb,
            capture_spectra,
            subtractor_spectra,
            converged_filters,
        );
    }

    pub fn erle(&self, onset_compensated: bool) -> &[[f32; FFT_LENGTH_BY_2_PLUS_1]] {
        if let Some(signal_dependent) = &self.signal_dependent_erle_estimator {
            signal_dependent.erle(onset_compensated)
        } else {
            self.subband_erle_estimator.erle(onset_compensated)
        }
    }

    /// The uncapped ERLE. Only the subband estimator produces one; with the
    /// signal-dependent estimator the capped ERLE is returned, as upstream.
    pub fn erle_unbounded(&self) -> &[[f32; FFT_LENGTH_BY_2_PLUS_1]] {
        if let Some(signal_dependent) = &self.signal_dependent_erle_estimator {
            signal_dependent.erle(/*onset_compensated=*/ false)
        } else {
            self.subband_erle_estimator.erle_unbounded()
        }
    }

    pub fn get_inst_linear_quality_estimates(&self) -> &[Option<f32>] {
        self.fullband_erle_estimator.get_linear_quality_estimates()
    }
}

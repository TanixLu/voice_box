use rubato::{
    Adjustable as _, FixedAsync, Resampler as _, Resizable as _, Slip,
    audioadapter_buffers::direct::InterleavedSlice,
};

use crate::pcm_transport::{PCM_CHANNELS, PcmFrame};

/// Bounded clock-drift correction after nominal-rate conversion, for stereo effects only.
pub struct RateMatcher {
    engine: Slip<f32>,
    input: Vec<PcmFrame>,
    output: Vec<PcmFrame>,
    sample_rate: f64,
    filtered_error: f64,
}

impl RateMatcher {
    const FILTER_SECONDS: f64 = 0.5;
    const CORRECTION_SECONDS: f64 = 5.0;
    const MAX_RATIO_OFFSET: f64 = 0.002;

    pub fn new(sample_rate: f64, max_output_frames: usize) -> Self {
        let engine = Slip::new(max_output_frames.max(4), PCM_CHANNELS, FixedAsync::Output)
            .expect("UNREACHABLE: stereo Slip with at least four frames should be valid");
        Self {
            input: vec![PcmFrame::default(); engine.input_frames_max()],
            output: vec![PcmFrame::default(); engine.output_frames_max()],
            engine,
            sample_rate,
            filtered_error: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.filtered_error = 0.0;
        self.engine.reset();
    }

    /// `error_seconds` is source read position minus expected position.
    /// Calls `read` once per consumed input frame; returns stereo frames.
    pub fn process(
        &mut self,
        frames: usize,
        error_seconds: f64,
        mut read: impl FnMut() -> PcmFrame,
    ) -> &[PcmFrame] {
        if frames == 0 {
            return &self.output[..0];
        }
        let duration = frames as f64 / self.sample_rate;
        let alpha = -(-duration / Self::FILTER_SECONDS).exp_m1();
        self.filtered_error += alpha * (error_seconds - self.filtered_error);
        if frames < 4 {
            for frame in &mut self.output[..frames] {
                *frame = read();
            }
            return &self.output[..frames];
        }
        let ratio = (1.0 + self.filtered_error / Self::CORRECTION_SECONDS)
            .clamp(1.0 - Self::MAX_RATIO_OFFSET, 1.0 + Self::MAX_RATIO_OFFSET);
        self.engine
            .set_chunk_size(frames)
            .expect("UNREACHABLE: output block should fit the prepared maximum");
        self.engine
            .set_resample_ratio(ratio, false)
            .expect("UNREACHABLE: bounded drift ratio should fit Slip's range");
        let input_frames = self.engine.input_frames_next();
        for frame in &mut self.input[..input_frames] {
            *frame = read();
        }
        let input = InterleavedSlice::new(self.input.as_flattened(), PCM_CHANNELS, input_frames)
            .expect("UNREACHABLE: input view should fit the prepared buffer");
        let mut output =
            InterleavedSlice::new_mut(self.output.as_flattened_mut(), PCM_CHANNELS, frames)
                .expect("UNREACHABLE: output view should fit the prepared buffer");
        self.engine
            .process_into_buffer(&input, &mut output, None)
            .expect("UNREACHABLE: prepared buffers should satisfy Slip requirements");
        &self.output[..frames]
    }
}

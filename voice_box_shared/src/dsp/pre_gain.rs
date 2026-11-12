//! Speech-driven model input calibration, one-block peak lookahead and matched undo.

use dfn3_rs::{ALGORITHM_DELAY, FRAME_SIZE, SAMPLE_RATE};

use super::StreamingResampler;

const VAD_RATE: f64 = 16_000.0;
const VAD_FRAME: usize = 256;
const TARGET_DB: f32 = -24.0;
const MAX_GAIN_DB: f32 = 48.0;
const MAX_NOISE_DB: f32 = -35.0;
const CEILING: f32 = 0.5;
const DB_PER_BLOCK: f32 = 6.0 * FRAME_SIZE as f32 / SAMPLE_RATE as f32;

pub(super) struct PreGain {
    vad: Box<earshot::Detector>,
    vad_resampler: StreamingResampler,
    vad_input: [f32; VAD_FRAME],
    vad_pending: usize,
    speech_frames: usize,
    speech_db: Option<f32>,
    noise_db: Option<f32>,
    noise_frames: usize,
    calibration_blocks: usize,
    gain_db: f32,
    gain: f32,
    lookahead: [f32; FRAME_SIZE],
    inverse_delay: [f32; ALGORITHM_DELAY],
    inverse_position: usize,
    compensation: [f32; FRAME_SIZE],
}

impl PreGain {
    pub(super) fn new() -> Self {
        Self {
            vad: earshot::Detector::default_boxed(),
            vad_resampler: StreamingResampler::new(f64::from(SAMPLE_RATE), VAD_RATE),
            vad_input: [0.0; VAD_FRAME],
            vad_pending: 0,
            speech_frames: 0,
            speech_db: None,
            noise_db: None,
            noise_frames: 0,
            calibration_blocks: 0,
            gain_db: 0.0,
            gain: 1.0,
            lookahead: [0.0; FRAME_SIZE],
            inverse_delay: [1.0; ALGORITHM_DELAY],
            inverse_position: 0,
            compensation: [1.0; FRAME_SIZE],
        }
    }

    pub(super) fn reset(&mut self) {
        self.vad.reset();
        self.vad_resampler.reset();
        self.vad_input.fill(0.0);
        self.vad_pending = 0;
        self.speech_frames = 0;
        self.speech_db = None;
        self.noise_db = None;
        self.noise_frames = 0;
        self.calibration_blocks = 0;
        self.gain_db = 0.0;
        self.gain = 1.0;
        self.lookahead.fill(0.0);
        self.inverse_delay.fill(1.0);
        self.inverse_position = 0;
        self.compensation.fill(1.0);
    }

    fn analyze(&mut self, input: &[f32; FRAME_SIZE]) {
        for &sample in input {
            self.vad_resampler.push(sample);
            while let Some(sample) = self.vad_resampler.next_sample() {
                self.vad_input[self.vad_pending] = sample;
                self.vad_pending += 1;
                if self.vad_pending == VAD_FRAME {
                    self.vad_pending = 0;
                    self.update_level();
                }
            }
        }
    }

    fn update_level(&mut self) {
        let power = self.vad_input.iter().map(|x| x * x).sum::<f32>() / VAD_FRAME as f32;
        let level_db = 10.0 * power.max(1e-12).log10();
        let peak = self.vad_input.iter().fold(0.0_f32, |p, x| p.max(x.abs()));
        // Normalize only the analysis copy. Very quiet speech must be able to
        // trigger calibration before DFN itself recognizes it as speech.
        let analysis_gain = db_to_gain(TARGET_DB - level_db)
            .min(db_to_gain(MAX_GAIN_DB))
            .min(CEILING / peak.max(1e-12));
        for sample in &mut self.vad_input {
            *sample *= analysis_gain;
        }
        let probability = self.vad.predict_f32(&self.vad_input);
        if probability >= 0.6 && level_db > -100.0 {
            self.noise_frames = 0;
            self.speech_frames = self.speech_frames.saturating_add(1);
            if self.speech_frames >= 3 {
                self.speech_db = Some(match self.speech_db {
                    Some(previous) => previous + 0.05 * (level_db - previous),
                    None => level_db,
                });
            }
        } else {
            self.speech_frames = 0;
            // Require sustained non-speech before learning the noise floor;
            // VAD startup context and individual missed syllables aren't noise.
            self.noise_frames = if probability < 0.2 {
                self.noise_frames.saturating_add(1)
            } else {
                0
            };
            if self.noise_frames >= 20 {
                self.noise_db = Some(match self.noise_db {
                    Some(previous) => previous + 0.05 * (level_db - previous),
                    None => level_db,
                });
            }
        }
    }

    /// Consume the next block and replace it with the calibrated previous block.
    /// The added FRAME_SIZE delay must also be included in the dry audio path.
    pub(super) fn prepare(&mut self, frame: &mut [f32; FRAME_SIZE]) {
        self.analyze(frame);
        let noise_limit = self.noise_db.map_or(MAX_GAIN_DB, |db| MAX_NOISE_DB - db);
        let target = (TARGET_DB - self.speech_db.unwrap_or(TARGET_DB))
            .min(noise_limit)
            .clamp(0.0, MAX_GAIN_DB);
        // Initial speech calibration is faster; established streams move at
        // 6 dB/s. Silence never causes the slow controller to increase gain.
        let step = if self.calibration_blocks < 100 {
            DB_PER_BLOCK * 8.0
        } else {
            DB_PER_BLOCK
        };
        if self.speech_frames >= 3 || target < self.gain_db {
            self.gain_db += (target - self.gain_db).clamp(-step, step);
            if self.speech_frames >= 3 {
                self.calibration_blocks += 1;
            }
        }
        let peak = frame
            .iter()
            .chain(&self.lookahead)
            .fold(0.0_f32, |p, x| p.max(x.abs()));
        let safe_gain = CEILING / peak.max(1e-12);
        let target_gain = db_to_gain(self.gain_db).min(safe_gain);
        // Both endpoints are safe for the outgoing block: the previous call
        // already saw its peak. The new endpoint also anticipates the next one.
        let end_gain = target_gain.min(self.gain * db_to_gain(step));
        std::mem::swap(frame, &mut self.lookahead);
        for (index, (sample, compensation)) in
            frame.iter_mut().zip(&mut self.compensation).enumerate()
        {
            let gain =
                self.gain + (end_gain - self.gain) * ((index + 1) as f32 / FRAME_SIZE as f32);
            *sample *= gain;
            debug_assert!(sample.abs() <= CEILING + 1e-6);
            *compensation = self.inverse_delay[self.inverse_position];
            self.inverse_delay[self.inverse_position] = gain.recip();
            self.inverse_position = (self.inverse_position + 1) % ALGORITHM_DELAY;
        }
        self.gain = end_gain;
    }

    /// Undo the actual applied envelope after DFN's fixed-rate algorithm delay,
    /// before resampling. Windowed neural processing makes this approximate.
    pub(super) fn restore(&self, frame: &mut [f32; FRAME_SIZE]) {
        for (sample, inverse) in frame.iter_mut().zip(self.compensation) {
            *sample *= inverse;
        }
    }
}

fn db_to_gain(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

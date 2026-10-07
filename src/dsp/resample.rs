//! Rubato sinc conversion with fixed one-millisecond input chunks and buffers.

use rubato::audioadapter_buffers::direct::SequentialSlice;
use rubato::{
    Async, FixedAsync, Resampler as _, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

pub(super) struct Resampler {
    engine: Option<Async<f32>>,
    input: Vec<f32>,
    output: Vec<f32>,
    pending: usize,
    read: usize,
    available: usize,
    delay_seconds: f64,
}

impl Resampler {
    pub(super) fn new(input_rate: f64, output_rate: f64) -> Self {
        if input_rate == output_rate {
            return Self {
                engine: None,
                input: vec![0.0],
                output: vec![0.0],
                pending: 0,
                read: 0,
                available: 0,
                delay_seconds: 0.0,
            };
        }
        let ratio = output_rate / input_rate;
        let parameters = SincInterpolationParameters {
            sinc_len: (64.0 / ratio.min(1.0)).ceil() as usize,
            f_cutoff: Some(0.9),
            oversampling_factor: 128,
            interpolation: SincInterpolationType::Linear,
            window: WindowFunction::Blackman,
        };
        let engine = Async::new_sinc(
            ratio,
            1.0,
            &parameters,
            (input_rate / 1000.0).ceil() as usize,
            1,
            FixedAsync::Input,
        )
        .expect("validated sample rates and sinc parameters");
        // Preserve the fractional part truncated by Rubato's output_delay().
        // Its interpolation index advances before the first output sample.
        let sinc_len = parameters.sinc_len.next_multiple_of(8);
        let fractional = sinc_len as f64 * ratio / 2.0 - engine.output_delay() as f64;
        let delay_seconds = (engine.output_delay() as f64 + fractional - 1.0) / output_rate;
        Self {
            input: vec![0.0; engine.input_frames_max()],
            output: vec![0.0; engine.output_frames_max()],
            engine: Some(engine),
            pending: 0,
            read: 0,
            available: 0,
            delay_seconds,
        }
    }

    pub(super) fn delay_seconds(&self) -> f64 {
        self.delay_seconds
    }
    pub(super) fn input_chunk(&self) -> usize {
        if self.engine.is_some() {
            self.input.len()
        } else {
            0
        }
    }

    pub(super) fn reset(&mut self) {
        if let Some(engine) = &mut self.engine {
            engine.reset();
        }
        self.input.fill(0.0);
        self.output.fill(0.0);
        self.pending = 0;
        self.read = 0;
        self.available = 0;
    }

    pub(super) fn push(&mut self, sample: f32) {
        self.input[self.pending] = sample;
        self.pending += 1;
        if self.pending != self.input.len() {
            return;
        }
        assert_eq!(
            self.read, self.available,
            "previous resampler output must be drained"
        );
        self.pending = 0;
        self.read = 0;
        self.available = if let Some(engine) = &mut self.engine {
            let input =
                SequentialSlice::new(&self.input, 1, self.input.len()).expect("fixed mono input");
            let output_len = self.output.len();
            let mut output = SequentialSlice::new_mut(&mut self.output, 1, output_len)
                .expect("fixed mono output");
            let (consumed, produced) = engine
                .process_into_buffer(&input, &mut output, None)
                .expect("fixed resampler buffers");
            debug_assert_eq!(consumed, self.input.len());
            produced
        } else {
            self.output[0] = sample;
            1
        };
    }

    pub(super) fn next(&mut self) -> Option<f32> {
        if self.read == self.available {
            return None;
        }
        let sample = self.output[self.read];
        self.read += 1;
        Some(sample)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsampling_rejects_aliases_and_preserves_speech_band() {
        let measure = |frequency: f64| {
            let mut resampler = Resampler::new(48_000.0, 16_000.0);
            let mut energy = 0.0;
            let mut count = 0;
            for i in 0..48_000 {
                resampler
                    .push((std::f64::consts::TAU * frequency * i as f64 / 48_000.0).sin() as f32);
                while let Some(value) = resampler.next() {
                    if i > 1_000 {
                        energy += f64::from(value * value);
                        count += 1;
                    }
                }
            }
            (energy / count as f64).sqrt()
        };
        assert!((measure(1_000.0) - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.005);
        assert!(measure(12_000.0) < 0.001);
    }

    #[test]
    fn fractional_and_upsampling_clocks_do_not_drift() {
        for (input_rate, output_rate) in [
            (16_000.0, 16_000.0),
            (44_100.0, 16_000.0),
            (8_000.0, 16_000.0),
            (16_000.0, 192_000.0),
        ] {
            let mut resampler = Resampler::new(input_rate, output_rate);
            let mut count = 0;
            let tolerance =
                (resampler.input_chunk() as f64 * output_rate / input_rate).ceil() as usize + 2;
            for seconds in 1..=10 {
                for _ in 0..input_rate as usize {
                    resampler.push(0.5);
                    while let Some(value) = resampler.next() {
                        assert!(value.is_finite());
                        if count > output_rate as usize / 10 {
                            assert!((value - 0.5).abs() < 1e-5);
                        }
                        count += 1;
                    }
                }
                let expected = seconds * output_rate as usize;
                assert!(
                    count.abs_diff(expected) <= tolerance,
                    "seconds={seconds} count={count} expected={expected}"
                );
            }
            resampler.reset();
            for _ in 0..(input_rate / 100.0) as usize {
                resampler.push(0.0);
                while let Some(value) = resampler.next() {
                    assert_eq!(value, 0.0);
                }
            }
        }
    }
}

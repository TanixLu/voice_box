//! Causal band-limited sample-rate conversion with precomputed polyphase FIRs.
//! No coefficient generation, allocation or buffering growth during processing.

use std::sync::Arc;

const PHASES: usize = 128;

pub(super) struct Kernel {
    coefficients: Vec<f32>,
    taps: usize,
    radius: usize,
    step: f64,
}

impl Kernel {
    pub(super) fn new(input_rate: f64, output_rate: f64) -> Arc<Self> {
        if input_rate == output_rate {
            return Arc::new(Self {
                coefficients: vec![1.0],
                taps: 1,
                radius: 0,
                step: 1.0,
            });
        }
        let ratio = (input_rate / output_rate).max(1.0);
        let radius = (32.0 * ratio).ceil() as usize;
        let taps = 2 * radius + 2;
        let cutoff = 0.45 / ratio;
        let mut coefficients = vec![0.0; (PHASES + 1) * taps];
        for phase in 0..=PHASES {
            let fraction = phase as f64 / PHASES as f64;
            let row = &mut coefficients[phase * taps..(phase + 1) * taps];
            let mut total = 0.0;
            for (i, value) in row.iter_mut().enumerate() {
                let distance = radius as f64 + fraction - i as f64;
                let sinc_argument = 2.0 * cutoff * distance;
                let sinc = if sinc_argument.abs() < 1e-12 {
                    1.0
                } else {
                    let angle = std::f64::consts::PI * sinc_argument;
                    angle.sin() / angle
                };
                let normalized = distance / radius as f64;
                let window = if normalized.abs() <= 1.0 {
                    0.42 + 0.5 * (std::f64::consts::PI * normalized).cos()
                        + 0.08 * (std::f64::consts::TAU * normalized).cos()
                } else {
                    0.0
                };
                *value = (2.0 * cutoff * sinc * window) as f32;
                total += f64::from(*value);
            }
            for value in row {
                *value = (f64::from(*value) / total) as f32;
            }
        }
        Arc::new(Self {
            coefficients,
            taps,
            radius,
            step: input_rate / output_rate,
        })
    }

    pub(super) fn delay_samples(&self) -> usize {
        self.radius
    }
}

pub(super) struct Resampler {
    kernel: Arc<Kernel>,
    history: Vec<f32>,
    write: usize,
    input_count: u64,
    output_count: u64,
}

impl Resampler {
    pub(super) fn new(kernel: Arc<Kernel>) -> Self {
        Self {
            history: vec![0.0; kernel.taps],
            kernel,
            write: 0,
            input_count: 0,
            output_count: 0,
        }
    }

    pub(super) fn reset(&mut self) {
        self.history.fill(0.0);
        self.write = 0;
        self.input_count = 0;
        self.output_count = 0;
    }

    pub(super) fn push(&mut self, input: f32) {
        self.history[self.write] = input;
        self.write = (self.write + 1) % self.history.len();
        self.input_count += 1;
    }

    pub(super) fn next(&mut self) -> Option<f32> {
        if self.input_count == 0 {
            return None;
        }
        let time = self.output_count as f64 * self.kernel.step;
        let latest = (self.input_count - 1) as f64;
        if time > latest + 1e-8 {
            return None;
        }
        self.output_count += 1;
        if self.kernel.taps == 1 {
            return Some(self.history[0]);
        }
        let fraction = (latest - time).clamp(0.0, 1.0);
        let phase = (fraction * PHASES as f64).round() as usize;
        let coefficients =
            &self.kernel.coefficients[phase * self.kernel.taps..(phase + 1) * self.kernel.taps];
        let mut sum = 0.0;
        let mut index = if self.write == 0 {
            self.history.len() - 1
        } else {
            self.write - 1
        };
        for coefficient in coefficients {
            sum += coefficient * self.history[index];
            index = if index == 0 {
                self.history.len() - 1
            } else {
                index - 1
            };
        }
        Some(sum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsampling_rejects_aliases_and_preserves_speech_band() {
        let measure = |frequency: f64| {
            let mut resampler = Resampler::new(Kernel::new(48_000.0, 16_000.0));
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
            (44_100.0, 16_000.0),
            (8_000.0, 16_000.0),
            (16_000.0, 192_000.0),
        ] {
            let mut resampler = Resampler::new(Kernel::new(input_rate, output_rate));
            let mut count = 0;
            for _ in 0..input_rate as usize {
                resampler.push(0.5);
                while let Some(value) = resampler.next() {
                    assert!(value.is_finite());
                    if resampler.input_count > resampler.kernel.taps as u64 {
                        assert!((value - 0.5).abs() < 1e-5);
                    }
                    count += 1;
                }
            }
            let expected = ((input_rate - 1.0) * output_rate / input_rate).floor() as usize + 1;
            assert_eq!(count, expected);
            resampler.reset();
            resampler.push(0.0);
            assert_eq!(resampler.next(), Some(0.0));
        }
    }
}

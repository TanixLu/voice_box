//! RealFFT with preallocated scratch and causal square-root Hann STFT.

use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;
pub(super) type Complex = rustfft::num_complex::Complex<f32>;

pub(super) struct Stft {
    window: [f32; 512],
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    forward_scratch: Vec<Complex>,
    inverse_scratch: Vec<Complex>,
    previous: [f32; 256],
    overlap: [f32; 256],
}

impl Stft {
    pub(super) fn new() -> Self {
        let mut planner = RealFftPlanner::new();
        let forward = planner.plan_fft_forward(512);
        let inverse = planner.plan_fft_inverse(512);
        let forward_scratch = forward.make_scratch_vec();
        let inverse_scratch = inverse.make_scratch_vec();
        Self {
            window: std::array::from_fn(|i| {
                (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / 512.0).cos()).sqrt() as f32
            }),
            forward,
            inverse,
            forward_scratch,
            inverse_scratch,
            previous: [0.0; 256],
            overlap: [0.0; 256],
        }
    }

    pub(super) fn reset(&mut self) {
        self.previous.fill(0.0);
        self.overlap.fill(0.0);
    }

    pub(super) fn analyze(&mut self, input: &[f32]) -> [Complex; 257] {
        let mut time = [0.0; 512];
        for i in 0..256 {
            time[i] = self.previous[i] * self.window[i];
            time[i + 256] = input[i] * self.window[i + 256];
        }
        self.previous.copy_from_slice(input);
        let mut spectrum = [Complex::default(); 257];
        self.forward
            .process_with_scratch(&mut time, &mut spectrum, &mut self.forward_scratch)
            .expect("fixed forward FFT buffers");
        spectrum
    }

    pub(super) fn synthesize(&mut self, spectrum: &[Complex; 257], output: &mut [f32]) {
        let mut spectrum = *spectrum;
        spectrum[0].im = 0.0;
        spectrum[256].im = 0.0;
        let mut time = [0.0; 512];
        self.inverse
            .process_with_scratch(&mut spectrum, &mut time, &mut self.inverse_scratch)
            .expect("fixed inverse FFT buffers and real endpoints");
        for i in 0..256 {
            output[i] = self.overlap[i] + time[i] * (self.window[i] / 512.0);
            self.overlap[i] = time[i + 256] * (self.window[i + 256] / 512.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_add_reconstructs_with_exact_one_hop_delay() {
        let mut stft = Stft::new();
        let mut previous = [0.0; 256];
        for frame in 0..8 {
            let input =
                std::array::from_fn::<_, 256, _>(|i| ((frame * 256 + i) as f32 * 0.13).sin());
            let spectrum = stft.analyze(&input);
            let mut output = [0.0; 256];
            stft.synthesize(&spectrum, &mut output);
            for (actual, expected) in output.iter().zip(previous) {
                assert!((actual - expected).abs() < 2e-6);
            }
            previous = input;
        }
        stft.reset();
        let spectrum = stft.analyze(&[0.0; 256]);
        let mut output = [1.0; 256];
        stft.synthesize(&spectrum, &mut output);
        assert_eq!(output, [0.0; 256]);
    }
}

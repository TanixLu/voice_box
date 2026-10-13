//! UL-UNAS's periodic Hann STFT with normalized 50% overlap-add.

use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;
pub(super) type Complex = rustfft::num_complex::Complex<f32>;

pub(super) struct Stft {
    window: [f32; 512],
    norm: [f32; 256],
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    forward_scratch: Vec<Complex>,
    inverse_scratch: Vec<Complex>,
    previous: [f32; 256],
    overlap: [f32; 256],
    time: [f32; 512],
}

impl Stft {
    pub(super) fn new() -> Self {
        let mut planner = RealFftPlanner::new();
        let forward = planner.plan_fft_forward(512);
        let inverse = planner.plan_fft_inverse(512);
        let window: [f32; 512] = std::array::from_fn(|i| {
            (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / 512.0).cos()) as f32
        });
        Self {
            norm: std::array::from_fn(|i| {
                1.0 / (window[i] * window[i] + window[i + 256] * window[i + 256])
            }),
            window,
            forward_scratch: forward.make_scratch_vec(),
            inverse_scratch: inverse.make_scratch_vec(),
            forward,
            inverse,
            previous: [0.0; 256],
            overlap: [0.0; 256],
            time: [0.0; 512],
        }
    }

    pub(super) fn reset(&mut self) {
        self.previous.fill(0.0);
        self.overlap.fill(0.0);
        self.time.fill(0.0);
    }

    pub(super) fn analyze(&mut self, input: &[f32], spectrum: &mut [Complex; 257]) {
        for (i, &sample) in input.iter().enumerate() {
            self.time[i] = self.previous[i] * self.window[i];
            self.time[i + 256] = sample * self.window[i + 256];
        }
        self.previous.copy_from_slice(input);
        self.forward
            .process_with_scratch(&mut self.time, spectrum, &mut self.forward_scratch)
            .expect("fixed UL-UNAS forward FFT buffers");
    }

    pub(super) fn synthesize(&mut self, spectrum: &mut [Complex; 257], output: &mut [f32]) {
        spectrum[0].im = 0.0;
        spectrum[256].im = 0.0;
        self.inverse
            .process_with_scratch(spectrum, &mut self.time, &mut self.inverse_scratch)
            .expect("fixed UL-UNAS inverse FFT buffers");
        for (i, sample) in output.iter_mut().enumerate() {
            *sample = (self.overlap[i] + self.time[i] * (self.window[i] / 512.0)) * self.norm[i];
            self.overlap[i] = self.time[i + 256] * (self.window[i + 256] / 512.0);
        }
    }
}

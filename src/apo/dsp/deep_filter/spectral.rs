//! DeepFilterNet3's 48 kHz Vorbis STFT and feature normalization.
//!
//! The window, analysis scaling and ERB partition follow upstream libDF.
//! FFT plans, scratch and time-domain buffers are allocated only in `new`.

use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub(super) type Complex = rustfft::num_complex::Complex<f32>;

pub(super) const SAMPLE_RATE: u32 = 48_000;
pub(super) const FFT_SIZE: usize = 960;
pub(super) const HOP_SIZE: usize = 480;
pub(super) const FREQ_BINS: usize = FFT_SIZE / 2 + 1;
pub(super) const ERB_BANDS: usize = 32;
pub(super) const DF_BINS: usize = 96;

// norm_tau = 1 s; libDF rounds exp(-hop / sample_rate / tau) to 3 digits.
const NORM_ALPHA: f32 = 0.99;

pub(super) struct Stft {
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    forward_scratch: Vec<Complex>,
    inverse_scratch: Vec<Complex>,
    time: Vec<f32>,
    window: [f32; FFT_SIZE],
    previous: [f32; HOP_SIZE],
    overlap: [f32; HOP_SIZE],
}

impl Stft {
    pub(super) fn new() -> Self {
        let mut planner = RealFftPlanner::new();
        let forward = planner.plan_fft_forward(FFT_SIZE);
        let inverse = planner.plan_fft_inverse(FFT_SIZE);
        let forward_scratch = forward.make_scratch_vec();
        let inverse_scratch = inverse.make_scratch_vec();
        Self {
            forward,
            inverse,
            forward_scratch,
            inverse_scratch,
            time: vec![0.0; FFT_SIZE],
            window: std::array::from_fn(|i| {
                let phase = std::f64::consts::PI * (i as f64 + 0.5) / FFT_SIZE as f64;
                let sin = phase.sin();
                (std::f64::consts::FRAC_PI_2 * sin * sin).sin() as f32
            }),
            previous: [0.0; HOP_SIZE],
            overlap: [0.0; HOP_SIZE],
        }
    }

    pub(super) fn reset(&mut self) {
        self.previous.fill(0.0);
        self.overlap.fill(0.0);
        self.time.fill(0.0);
    }

    pub(super) fn analyze(&mut self, input: &[f32], spectrum: &mut [Complex; FREQ_BINS]) {
        debug_assert_eq!(input.len(), HOP_SIZE);
        for (i, &sample) in input.iter().enumerate() {
            self.time[i] = self.previous[i] * self.window[i];
            self.time[i + HOP_SIZE] = sample * self.window[i + HOP_SIZE];
        }
        self.previous.copy_from_slice(input);
        self.forward
            .process_with_scratch(&mut self.time, spectrum, &mut self.forward_scratch)
            .expect("fixed DeepFilterNet3 forward FFT buffers");
        // libDF normalizes the analysis rather than the inverse FFT. For this
        // 50% overlap, wnorm = 2 * hop / fft_size^2 = 1 / fft_size.
        for bin in spectrum {
            *bin *= 1.0 / FFT_SIZE as f32;
        }
    }

    pub(super) fn synthesize(&mut self, spectrum: &mut [Complex; FREQ_BINS], output: &mut [f32]) {
        debug_assert_eq!(output.len(), HOP_SIZE);
        spectrum[0].im = 0.0;
        spectrum[FREQ_BINS - 1].im = 0.0;
        self.inverse
            .process_with_scratch(spectrum, &mut self.time, &mut self.inverse_scratch)
            .expect("fixed DeepFilterNet3 inverse FFT buffers and real endpoints");
        for (i, sample) in output.iter_mut().enumerate() {
            *sample = self.time[i] * self.window[i] + self.overlap[i];
            self.overlap[i] = self.time[i + HOP_SIZE] * self.window[i + HOP_SIZE];
        }
    }
}

pub(super) struct Features {
    widths: [usize; ERB_BANDS],
    mean: [f32; ERB_BANDS],
    unit: [f32; DF_BINS],
}

impl Features {
    pub(super) fn new() -> Self {
        let mut result = Self {
            widths: erb_widths(),
            mean: [0.0; ERB_BANDS],
            unit: [0.0; DF_BINS],
        };
        result.reset();
        result
    }

    pub(super) fn reset(&mut self) {
        // These frequency-dependent initial states are part of the trained
        // feature transform, including after a model switch.
        for (i, mean) in self.mean.iter_mut().enumerate() {
            *mean = -60.0 + i as f32 * (-30.0 / (ERB_BANDS - 1) as f32);
        }
        for (i, unit) in self.unit.iter_mut().enumerate() {
            *unit = 0.001 + i as f32 * ((0.0001 - 0.001) / (DF_BINS - 1) as f32);
        }
    }

    pub(super) fn compute(
        &mut self,
        spectrum: &[Complex; FREQ_BINS],
        erb: &mut [f32; ERB_BANDS],
        spec: &mut [f32; 2 * DF_BINS],
    ) {
        let mut start = 0;
        for ((&width, mean), feature) in self.widths.iter().zip(&mut self.mean).zip(erb) {
            let scale = 1.0 / width as f32;
            let mut power = 0.0;
            for bin in &spectrum[start..start + width] {
                power += (bin.re * bin.re + bin.im * bin.im) * scale;
            }
            let db = (power + 1e-10).log10() * 10.0;
            *mean = db * (1.0 - NORM_ALPHA) + *mean * NORM_ALPHA;
            *feature = (db - *mean) / 40.0;
            start += width;
        }
        for i in 0..DF_BINS {
            let bin = spectrum[i];
            self.unit[i] = bin.norm() * (1.0 - NORM_ALPHA) + self.unit[i] * NORM_ALPHA;
            let norm = self.unit[i].sqrt();
            // Very long exact silence can underflow the running norm on CPUs
            // that flush subnormals. Zero bins then still produce zero features.
            if norm > 0.0 {
                spec[i] = bin.re / norm;
                spec[i + DF_BINS] = bin.im / norm;
            } else {
                spec[i] = 0.0;
                spec[i + DF_BINS] = 0.0;
            }
        }
    }

    pub(super) fn apply_mask(&self, spectrum: &mut [Complex; FREQ_BINS], mask: &[f32; ERB_BANDS]) {
        let mut start = 0;
        for (&width, &gain) in self.widths.iter().zip(mask) {
            for bin in &mut spectrum[start..start + width] {
                *bin *= gain;
            }
            start += width;
        }
    }
}

fn erb_widths() -> [usize; ERB_BANDS] {
    fn freq_to_erb(freq: f32) -> f32 {
        9.265 * (freq / (24.7 * 9.265)).ln_1p()
    }
    fn erb_to_freq(erb: f32) -> f32 {
        24.7 * 9.265 * ((erb / 9.265).exp() - 1.0)
    }
    let erb_high = freq_to_erb((SAMPLE_RATE / 2) as f32);
    let step = erb_high / ERB_BANDS as f32;
    let freq_width = SAMPLE_RATE as f32 / FFT_SIZE as f32;
    let mut previous = 0;
    let mut excess = 0;
    let mut widths = [0; ERB_BANDS];
    for (i, width) in widths.iter_mut().enumerate() {
        let freq = (erb_to_freq((i + 1) as f32 * step) / freq_width).round() as i32;
        let mut count = freq - previous - excess;
        if count < 2 {
            excess = 2 - count;
            count = 2;
        } else {
            excess = 0;
        }
        *width = count as usize;
        previous = freq;
    }
    widths[ERB_BANDS - 1] += 1;
    let sum: usize = widths.iter().sum();
    if sum > FREQ_BINS {
        widths[ERB_BANDS - 1] -= sum - FREQ_BINS;
    }
    debug_assert_eq!(widths.iter().sum::<usize>(), FREQ_BINS);
    widths
}

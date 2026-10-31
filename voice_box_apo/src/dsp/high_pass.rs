//! A 60 Hz Butterworth high-pass at UL-UNAS's sample rate.

use std::f64::consts::{PI, SQRT_2};

const CUTOFF_HZ: f64 = 60.0;
const STATE_FLOOR: f64 = 1e-20;

pub struct HighPass {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl HighPass {
    pub fn new(sample_rate: f64) -> Self {
        // Bilinear transform of the second-order Butterworth high-pass.
        // Keep coefficients and DF2T state in f64 at this low cutoff.
        let k = (PI * CUTOFF_HZ / sample_rate).tan();
        let norm = 1.0 / (1.0 + SQRT_2 * k + k * k);
        Self {
            b0: norm,
            b1: -2.0 * norm,
            b2: norm,
            a1: 2.0 * (k * k - 1.0) * norm,
            a2: (1.0 - SQRT_2 * k + k * k) * norm,
            z1: 0.0,
            z2: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    pub fn process_sample(&mut self, sample: f32) -> f32 {
        if !sample.is_finite() {
            self.reset();
            return 0.0;
        }
        let input = f64::from(sample);
        let output = self.b0 * input + self.z1;
        self.z1 = flush_small(self.b1 * input - self.a1 * output + self.z2);
        self.z2 = flush_small(self.b2 * input - self.a2 * output);
        flush_small(output) as f32
    }
}

#[inline]
fn flush_small(value: f64) -> f64 {
    if value.abs() < STATE_FLOOR {
        0.0
    } else {
        value
    }
}

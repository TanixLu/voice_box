//! Fixed 512-point radix-2 FFT and causal 50%-overlap square-root Hann STFT.

#[derive(Clone, Copy, Default)]
pub(super) struct Complex {
    pub re: f32,
    pub im: f32,
}

pub(super) struct Stft {
    window: [f32; 512],
    twiddles: [Complex; 256],
    previous: [f32; 256],
    overlap: [f32; 256],
}

impl Stft {
    pub(super) fn new() -> Self {
        Self {
            window: std::array::from_fn(|i| {
                (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / 512.0).cos()).sqrt() as f32
            }),
            twiddles: std::array::from_fn(|i| {
                let angle = -std::f64::consts::TAU * i as f64 / 512.0;
                Complex {
                    re: angle.cos() as f32,
                    im: angle.sin() as f32,
                }
            }),
            previous: [0.0; 256],
            overlap: [0.0; 256],
        }
    }

    pub(super) fn reset(&mut self) {
        self.previous.fill(0.0);
        self.overlap.fill(0.0);
    }

    fn fft(&self, values: &mut [Complex; 512], inverse: bool) {
        for i in 0_usize..512 {
            let j = i.reverse_bits() >> (usize::BITS - 9);
            if j > i {
                values.swap(i, j);
            }
        }
        let mut size = 2;
        while size <= 512 {
            for start in (0..512).step_by(size) {
                for j in 0..size / 2 {
                    let mut w = self.twiddles[j * 512 / size];
                    if inverse {
                        w.im = -w.im;
                    }
                    let a = values[start + j];
                    let b = values[start + j + size / 2];
                    let re = b.re * w.re - b.im * w.im;
                    let im = b.re * w.im + b.im * w.re;
                    values[start + j] = Complex {
                        re: a.re + re,
                        im: a.im + im,
                    };
                    values[start + j + size / 2] = Complex {
                        re: a.re - re,
                        im: a.im - im,
                    };
                }
            }
            size *= 2;
        }
        if inverse {
            for value in values {
                value.re /= 512.0;
                value.im /= 512.0;
            }
        }
    }

    pub(super) fn analyze(&mut self, input: &[f32]) -> [Complex; 257] {
        let mut values = [Complex::default(); 512];
        for i in 0..256 {
            values[i].re = self.previous[i] * self.window[i];
            values[i + 256].re = input[i] * self.window[i + 256];
        }
        self.previous.copy_from_slice(input);
        self.fft(&mut values, false);
        std::array::from_fn(|i| values[i])
    }

    pub(super) fn synthesize(&mut self, spectrum: &[Complex; 257], output: &mut [f32]) {
        let mut values = [Complex::default(); 512];
        values[..257].copy_from_slice(spectrum);
        values[0].im = 0.0;
        values[256].im = 0.0;
        for i in 1..256 {
            values[512 - i] = Complex {
                re: spectrum[i].re,
                im: -spectrum[i].im,
            };
        }
        self.fft(&mut values, true);
        for i in 0..256 {
            output[i] = self.overlap[i] + values[i].re * self.window[i];
            self.overlap[i] = values[i + 256].re * self.window[i + 256];
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

use super::aec3_common::{Aec3Optimization, FFT_LENGTH_BY_2, FFT_LENGTH_BY_2_PLUS_1};

/// Holds the positive-frequency bins (including DC and Nyquist) for a
/// 128-point real-valued FFT.
#[derive(Clone, Debug, PartialEq)]
pub struct FftData {
    pub re: [f32; FFT_LENGTH_BY_2_PLUS_1],
    pub im: [f32; FFT_LENGTH_BY_2_PLUS_1],
}

impl Default for FftData {
    fn default() -> Self {
        Self {
            re: [0.0; FFT_LENGTH_BY_2_PLUS_1],
            im: [0.0; FFT_LENGTH_BY_2_PLUS_1],
        }
    }
}

impl FftData {
    pub fn assign(&mut self, src: &FftData) {
        self.re = src.re;
        self.im = src.im;
        self.im[0] = 0.0;
        self.im[FFT_LENGTH_BY_2] = 0.0;
    }

    pub fn clear(&mut self) {
        self.re.fill(0.0);
        self.im.fill(0.0);
    }

    pub fn spectrum(&self, optimization: Aec3Optimization, power_spectrum: &mut [f32]) {
        assert_eq!(power_spectrum.len(), FFT_LENGTH_BY_2_PLUS_1);
        match optimization {
            Aec3Optimization::Avx2 => self.spectrum_avx2(power_spectrum),
            Aec3Optimization::Sse2 => self.spectrum_sse2(power_spectrum),
            Aec3Optimization::Neon | Aec3Optimization::None => self.spectrum_scalar(power_spectrum),
        }
    }

    fn spectrum_scalar(&self, power_spectrum: &mut [f32]) {
        for (dst, (&re, &im)) in power_spectrum
            .iter_mut()
            .zip(self.re.iter().zip(self.im.iter()))
        {
            *dst = re * re + im * im;
        }
    }

    fn spectrum_avx2(&self, power_spectrum: &mut [f32]) {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            use super::aec3_common::detect_avx2;
            if detect_avx2() {
                unsafe {
                    self.spectrum_avx2_impl(power_spectrum);
                }
                return;
            }
        }
        self.spectrum_scalar(power_spectrum);
    }

    fn spectrum_sse2(&self, power_spectrum: &mut [f32]) {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            use super::aec3_common::detect_sse2;
            if detect_sse2() {
                unsafe {
                    self.spectrum_sse2_impl(power_spectrum);
                }
                return;
            }
        }
        self.spectrum_scalar(power_spectrum);
    }
}

#[cfg(target_arch = "x86")]
use std::arch::x86::{
    _mm_add_ps, _mm_loadu_ps, _mm_mul_ps, _mm_storeu_ps, _mm256_add_ps, _mm256_loadu_ps,
    _mm256_mul_ps, _mm256_storeu_ps,
};
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::{
    _mm_add_ps, _mm_loadu_ps, _mm_mul_ps, _mm_storeu_ps, _mm256_add_ps, _mm256_loadu_ps,
    _mm256_mul_ps, _mm256_storeu_ps,
};

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
impl FftData {
    #[allow(unsafe_op_in_unsafe_fn)]
    #[target_feature(enable = "avx2")]
    unsafe fn spectrum_avx2_impl(&self, power_spectrum: &mut [f32]) {
        let mut k = 0usize;
        while k < FFT_LENGTH_BY_2 {
            let r = _mm256_loadu_ps(self.re.as_ptr().add(k));
            let i = _mm256_loadu_ps(self.im.as_ptr().add(k));
            let power = _mm256_add_ps(_mm256_mul_ps(r, r), _mm256_mul_ps(i, i));
            _mm256_storeu_ps(power_spectrum.as_mut_ptr().add(k), power);
            k += 8;
        }
        power_spectrum[FFT_LENGTH_BY_2] = self.re[FFT_LENGTH_BY_2] * self.re[FFT_LENGTH_BY_2]
            + self.im[FFT_LENGTH_BY_2] * self.im[FFT_LENGTH_BY_2];
    }

    #[allow(unsafe_op_in_unsafe_fn)]
    #[target_feature(enable = "sse2")]
    unsafe fn spectrum_sse2_impl(&self, power_spectrum: &mut [f32]) {
        let mut k = 0usize;
        while k < FFT_LENGTH_BY_2 {
            let r = _mm_loadu_ps(self.re.as_ptr().add(k));
            let i = _mm_loadu_ps(self.im.as_ptr().add(k));
            let power = _mm_add_ps(_mm_mul_ps(r, r), _mm_mul_ps(i, i));
            _mm_storeu_ps(power_spectrum.as_mut_ptr().add(k), power);
            k += 4;
        }
        power_spectrum[FFT_LENGTH_BY_2] = self.re[FFT_LENGTH_BY_2] * self.re[FFT_LENGTH_BY_2]
            + self.im[FFT_LENGTH_BY_2] * self.im[FFT_LENGTH_BY_2];
    }
}

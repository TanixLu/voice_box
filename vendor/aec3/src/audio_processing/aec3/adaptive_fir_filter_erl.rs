#[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
use crate::audio_processing::aec3::aec3_common::FFT_LENGTH_BY_2;
#[cfg(target_arch = "aarch64")]
use crate::audio_processing::aec3::aec3_common::detect_neon;
use crate::audio_processing::aec3::aec3_common::{Aec3Optimization, FFT_LENGTH_BY_2_PLUS_1};
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use crate::audio_processing::aec3::aec3_common::{detect_avx2, detect_sse2};

/// Reference implementation for accumulating an echo return loss estimate from
/// frequency responses per partition.
pub fn erl_computer(h2: &[[f32; FFT_LENGTH_BY_2_PLUS_1]], erl: &mut [f32; FFT_LENGTH_BY_2_PLUS_1]) {
    assert_eq!(erl.len(), FFT_LENGTH_BY_2_PLUS_1);
    erl.fill(0.0);
    for partition in h2 {
        for (dst, &value) in erl.iter_mut().zip(partition.iter()) {
            *dst += value;
        }
    }
}

/// Selects the optimization specific implementation for computing the ERL.
pub fn compute_erl(
    optimization: Aec3Optimization,
    h2: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
    erl: &mut [f32; FFT_LENGTH_BY_2_PLUS_1],
) {
    match optimization {
        Aec3Optimization::Avx2 => compute_erl_avx2(h2, erl),
        Aec3Optimization::Sse2 => compute_erl_sse2(h2, erl),
        Aec3Optimization::Neon => compute_erl_neon(h2, erl),
        Aec3Optimization::None => erl_computer(h2, erl),
    }
}

fn compute_erl_avx2(h2: &[[f32; FFT_LENGTH_BY_2_PLUS_1]], erl: &mut [f32; FFT_LENGTH_BY_2_PLUS_1]) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if detect_avx2() {
        unsafe {
            compute_erl_avx2_impl(h2, erl);
        }
        return;
    }
    erl_computer(h2, erl);
}

fn compute_erl_sse2(h2: &[[f32; FFT_LENGTH_BY_2_PLUS_1]], erl: &mut [f32; FFT_LENGTH_BY_2_PLUS_1]) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if detect_sse2() {
        unsafe {
            compute_erl_sse2_impl(h2, erl);
        }
        return;
    }
    erl_computer(h2, erl);
}

fn compute_erl_neon(h2: &[[f32; FFT_LENGTH_BY_2_PLUS_1]], erl: &mut [f32; FFT_LENGTH_BY_2_PLUS_1]) {
    #[cfg(target_arch = "aarch64")]
    if detect_neon() {
        unsafe {
            compute_erl_neon_impl(h2, erl);
        }
        return;
    }
    erl_computer(h2, erl);
}

#[cfg(target_arch = "x86")]
use std::arch::x86::{
    _mm_add_ps, _mm_loadu_ps, _mm_storeu_ps, _mm256_add_ps, _mm256_loadu_ps, _mm256_storeu_ps,
};
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::{
    _mm_add_ps, _mm_loadu_ps, _mm_storeu_ps, _mm256_add_ps, _mm256_loadu_ps, _mm256_storeu_ps,
};

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[allow(unsafe_op_in_unsafe_fn)]
#[target_feature(enable = "avx2")]
unsafe fn compute_erl_avx2_impl(
    h2: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
    erl: &mut [f32; FFT_LENGTH_BY_2_PLUS_1],
) {
    erl.fill(0.0);
    for partition in h2 {
        let mut k = 0usize;
        while k < FFT_LENGTH_BY_2 {
            let sum = _mm256_add_ps(
                _mm256_loadu_ps(erl.as_ptr().add(k)),
                _mm256_loadu_ps(partition.as_ptr().add(k)),
            );
            _mm256_storeu_ps(erl.as_mut_ptr().add(k), sum);
            k += 8;
        }
        erl[FFT_LENGTH_BY_2] += partition[FFT_LENGTH_BY_2];
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[allow(unsafe_op_in_unsafe_fn)]
#[target_feature(enable = "sse2")]
unsafe fn compute_erl_sse2_impl(
    h2: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
    erl: &mut [f32; FFT_LENGTH_BY_2_PLUS_1],
) {
    erl.fill(0.0);
    for partition in h2 {
        let mut k = 0usize;
        while k < FFT_LENGTH_BY_2 {
            let sum = _mm_add_ps(
                _mm_loadu_ps(erl.as_ptr().add(k)),
                _mm_loadu_ps(partition.as_ptr().add(k)),
            );
            _mm_storeu_ps(erl.as_mut_ptr().add(k), sum);
            k += 4;
        }
        erl[FFT_LENGTH_BY_2] += partition[FFT_LENGTH_BY_2];
    }
}

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::{vaddq_f32, vld1q_f32, vst1q_f32};

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_op_in_unsafe_fn)]
#[target_feature(enable = "neon")]
unsafe fn compute_erl_neon_impl(
    h2: &[[f32; FFT_LENGTH_BY_2_PLUS_1]],
    erl: &mut [f32; FFT_LENGTH_BY_2_PLUS_1],
) {
    erl.fill(0.0);
    for partition in h2 {
        let mut k = 0usize;
        while k < FFT_LENGTH_BY_2 {
            let sum = vaddq_f32(
                vld1q_f32(erl.as_ptr().add(k)),
                vld1q_f32(partition.as_ptr().add(k)),
            );
            vst1q_f32(erl.as_mut_ptr().add(k), sum);
            k += 4;
        }
        erl[FFT_LENGTH_BY_2] += partition[FFT_LENGTH_BY_2];
    }
}

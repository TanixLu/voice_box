#[cfg(target_arch = "aarch64")]
use crate::audio_processing::aec3::aec3_common::detect_neon;
use crate::audio_processing::aec3::aec3_common::{
    Aec3Optimization, FFT_LENGTH, FFT_LENGTH_BY_2, FFT_LENGTH_BY_2_PLUS_1, get_time_domain_length,
};
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use crate::audio_processing::aec3::aec3_common::{detect_avx2, detect_sse2};
use crate::audio_processing::aec3::aec3_fft::Aec3Fft;
use crate::audio_processing::aec3::fft_data::FftData;
use crate::audio_processing::aec3::render_buffer::RenderBuffer;
use crate::audio_processing::logging::apm_data_dumper::ApmDataDumper;

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::{
    vaddq_f32, vld1q_f32, vmaxq_f32, vmlaq_f32, vmlsq_f32, vmulq_f32, vst1q_f32,
};
#[cfg(target_arch = "x86")]
use std::arch::x86::{
    _mm_add_ps, _mm_loadu_ps, _mm_max_ps, _mm_mul_ps, _mm_storeu_ps, _mm_sub_ps, _mm256_add_ps,
    _mm256_loadu_ps, _mm256_max_ps, _mm256_mul_ps, _mm256_storeu_ps, _mm256_sub_ps,
};
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::{
    _mm_add_ps, _mm_loadu_ps, _mm_max_ps, _mm_mul_ps, _mm_storeu_ps, _mm_sub_ps, _mm256_add_ps,
    _mm256_loadu_ps, _mm256_max_ps, _mm256_mul_ps, _mm256_storeu_ps, _mm256_sub_ps,
};

/// Computes and stores the frequency response of the filter.
pub fn compute_frequency_response(
    num_partitions: usize,
    h: &[Vec<FftData>],
    h2: &mut Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>,
) {
    assert!(num_partitions <= h.len());
    if h2.len() < num_partitions {
        h2.resize(num_partitions, [0.0; FFT_LENGTH_BY_2_PLUS_1]);
    }
    for response in h2.iter_mut().take(num_partitions) {
        response.fill(0.0);
    }

    if num_partitions == 0 {
        return;
    }

    let num_render_channels = h[0].len();
    for p in 0..num_partitions {
        for ch in 0..num_render_channels {
            for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
                let re = h[p][ch].re[k];
                let im = h[p][ch].im[k];
                let magnitude = re * re + im * im;
                h2[p][k] = h2[p][k].max(magnitude);
            }
        }
    }
}

fn compute_frequency_response_avx2(
    num_partitions: usize,
    h: &[Vec<FftData>],
    h2: &mut Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>,
) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if detect_avx2() {
        unsafe {
            compute_frequency_response_avx2_impl(num_partitions, h, h2);
        }
        return;
    }
    compute_frequency_response(num_partitions, h, h2);
}

fn compute_frequency_response_sse2(
    num_partitions: usize,
    h: &[Vec<FftData>],
    h2: &mut Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>,
) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if detect_sse2() {
        unsafe {
            compute_frequency_response_sse2_impl(num_partitions, h, h2);
        }
        return;
    }
    compute_frequency_response(num_partitions, h, h2);
}

fn compute_frequency_response_neon(
    num_partitions: usize,
    h: &[Vec<FftData>],
    h2: &mut Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>,
) {
    #[cfg(target_arch = "aarch64")]
    if detect_neon() {
        unsafe {
            compute_frequency_response_neon_impl(num_partitions, h, h2);
        }
        return;
    }
    compute_frequency_response(num_partitions, h, h2);
}

/// Adapts the filter partitions as H(t+1)=H(t)+G(t)*conj(X(t)).
pub fn adapt_partitions(
    render_buffer: &RenderBuffer,
    g: &FftData,
    num_partitions: usize,
    h: &mut [Vec<FftData>],
) {
    assert!(num_partitions <= h.len());
    if num_partitions == 0 {
        return;
    }

    let render_fft = render_buffer.fft_buffer();
    let mut index = render_buffer.position();
    let len = render_fft.len();
    let num_render_channels = render_fft[index].len();

    for p in 0..num_partitions {
        for ch in 0..num_render_channels {
            let x = &render_fft[index][ch];
            let h_entry = &mut h[p][ch];
            for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
                h_entry.re[k] += x.re[k] * g.re[k] + x.im[k] * g.im[k];
                h_entry.im[k] += x.re[k] * g.im[k] - x.im[k] * g.re[k];
            }
        }
        index = if index + 1 < len { index + 1 } else { 0 };
    }
}

fn adapt_partitions_avx2(
    render_buffer: &RenderBuffer,
    g: &FftData,
    num_partitions: usize,
    h: &mut [Vec<FftData>],
) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if detect_avx2() {
        unsafe {
            adapt_partitions_avx2_impl(render_buffer, g, num_partitions, h);
        }
        return;
    }
    adapt_partitions(render_buffer, g, num_partitions, h);
}

fn adapt_partitions_sse2(
    render_buffer: &RenderBuffer,
    g: &FftData,
    num_partitions: usize,
    h: &mut [Vec<FftData>],
) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if detect_sse2() {
        unsafe {
            adapt_partitions_sse2_impl(render_buffer, g, num_partitions, h);
        }
        return;
    }
    adapt_partitions(render_buffer, g, num_partitions, h);
}

fn adapt_partitions_neon(
    render_buffer: &RenderBuffer,
    g: &FftData,
    num_partitions: usize,
    h: &mut [Vec<FftData>],
) {
    #[cfg(target_arch = "aarch64")]
    if detect_neon() {
        unsafe {
            adapt_partitions_neon_impl(render_buffer, g, num_partitions, h);
        }
        return;
    }
    adapt_partitions(render_buffer, g, num_partitions, h);
}

/// Produces the filter output from the current partitions.
pub fn apply_filter(
    render_buffer: &RenderBuffer,
    num_partitions: usize,
    h: &[Vec<FftData>],
    s: &mut FftData,
) {
    assert!(num_partitions <= h.len());
    s.re.fill(0.0);
    s.im.fill(0.0);

    if num_partitions == 0 {
        return;
    }

    let render_fft = render_buffer.fft_buffer();
    let mut index = render_buffer.position();
    let len = render_fft.len();
    let num_render_channels = render_fft[index].len();

    for p in 0..num_partitions {
        for ch in 0..num_render_channels {
            let x = &render_fft[index][ch];
            let h_entry = &h[p][ch];
            for k in 0..FFT_LENGTH_BY_2_PLUS_1 {
                s.re[k] += x.re[k] * h_entry.re[k] - x.im[k] * h_entry.im[k];
                s.im[k] += x.re[k] * h_entry.im[k] + x.im[k] * h_entry.re[k];
            }
        }
        index = if index + 1 < len { index + 1 } else { 0 };
    }
}

fn apply_filter_avx2(
    render_buffer: &RenderBuffer,
    num_partitions: usize,
    h: &[Vec<FftData>],
    s: &mut FftData,
) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if detect_avx2() {
        unsafe {
            apply_filter_avx2_impl(render_buffer, num_partitions, h, s);
        }
        return;
    }
    apply_filter(render_buffer, num_partitions, h, s);
}

fn apply_filter_sse2(
    render_buffer: &RenderBuffer,
    num_partitions: usize,
    h: &[Vec<FftData>],
    s: &mut FftData,
) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if detect_sse2() {
        unsafe {
            apply_filter_sse2_impl(render_buffer, num_partitions, h, s);
        }
        return;
    }
    apply_filter(render_buffer, num_partitions, h, s);
}

fn apply_filter_neon(
    render_buffer: &RenderBuffer,
    num_partitions: usize,
    h: &[Vec<FftData>],
    s: &mut FftData,
) {
    #[cfg(target_arch = "aarch64")]
    if detect_neon() {
        unsafe {
            apply_filter_neon_impl(render_buffer, num_partitions, h, s);
        }
        return;
    }
    apply_filter(render_buffer, num_partitions, h, s);
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[allow(unsafe_op_in_unsafe_fn)]
#[target_feature(enable = "avx2")]
unsafe fn compute_frequency_response_avx2_impl(
    num_partitions: usize,
    h: &[Vec<FftData>],
    h2: &mut Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>,
) {
    assert!(num_partitions <= h.len());
    if h2.len() < num_partitions {
        h2.resize(num_partitions, [0.0; FFT_LENGTH_BY_2_PLUS_1]);
    }
    for response in h2.iter_mut().take(num_partitions) {
        response.fill(0.0);
    }
    if num_partitions == 0 {
        return;
    }
    let num_render_channels = h[0].len();
    for p in 0..num_partitions {
        for ch in 0..num_render_channels {
            let h_p_ch = &h[p][ch];
            let h2_p = &mut h2[p];
            let mut j = 0usize;
            while j < FFT_LENGTH_BY_2 {
                let re = _mm256_loadu_ps(h_p_ch.re.as_ptr().add(j));
                let im = _mm256_loadu_ps(h_p_ch.im.as_ptr().add(j));
                let magnitude = _mm256_add_ps(_mm256_mul_ps(re, re), _mm256_mul_ps(im, im));
                let current = _mm256_loadu_ps(h2_p.as_ptr().add(j));
                _mm256_storeu_ps(h2_p.as_mut_ptr().add(j), _mm256_max_ps(current, magnitude));
                j += 8;
            }
            let nyquist = h_p_ch.re[FFT_LENGTH_BY_2] * h_p_ch.re[FFT_LENGTH_BY_2]
                + h_p_ch.im[FFT_LENGTH_BY_2] * h_p_ch.im[FFT_LENGTH_BY_2];
            h2_p[FFT_LENGTH_BY_2] = h2_p[FFT_LENGTH_BY_2].max(nyquist);
        }
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[allow(unsafe_op_in_unsafe_fn)]
#[target_feature(enable = "sse2")]
unsafe fn compute_frequency_response_sse2_impl(
    num_partitions: usize,
    h: &[Vec<FftData>],
    h2: &mut Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>,
) {
    assert!(num_partitions <= h.len());
    if h2.len() < num_partitions {
        h2.resize(num_partitions, [0.0; FFT_LENGTH_BY_2_PLUS_1]);
    }
    for response in h2.iter_mut().take(num_partitions) {
        response.fill(0.0);
    }
    if num_partitions == 0 {
        return;
    }
    let num_render_channels = h[0].len();
    for p in 0..num_partitions {
        for ch in 0..num_render_channels {
            let h_p_ch = &h[p][ch];
            let h2_p = &mut h2[p];
            let mut j = 0usize;
            while j < FFT_LENGTH_BY_2 {
                let re = _mm_loadu_ps(h_p_ch.re.as_ptr().add(j));
                let im = _mm_loadu_ps(h_p_ch.im.as_ptr().add(j));
                let magnitude = _mm_add_ps(_mm_mul_ps(re, re), _mm_mul_ps(im, im));
                let current = _mm_loadu_ps(h2_p.as_ptr().add(j));
                _mm_storeu_ps(h2_p.as_mut_ptr().add(j), _mm_max_ps(current, magnitude));
                j += 4;
            }
            let nyquist = h_p_ch.re[FFT_LENGTH_BY_2] * h_p_ch.re[FFT_LENGTH_BY_2]
                + h_p_ch.im[FFT_LENGTH_BY_2] * h_p_ch.im[FFT_LENGTH_BY_2];
            h2_p[FFT_LENGTH_BY_2] = h2_p[FFT_LENGTH_BY_2].max(nyquist);
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_op_in_unsafe_fn)]
#[target_feature(enable = "neon")]
unsafe fn compute_frequency_response_neon_impl(
    num_partitions: usize,
    h: &[Vec<FftData>],
    h2: &mut Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>,
) {
    assert!(num_partitions <= h.len());
    if h2.len() < num_partitions {
        h2.resize(num_partitions, [0.0; FFT_LENGTH_BY_2_PLUS_1]);
    }
    for response in h2.iter_mut().take(num_partitions) {
        response.fill(0.0);
    }
    if num_partitions == 0 {
        return;
    }
    let num_render_channels = h[0].len();
    for p in 0..num_partitions {
        for ch in 0..num_render_channels {
            let h_p_ch = &h[p][ch];
            let h2_p = &mut h2[p];
            let mut j = 0usize;
            while j < FFT_LENGTH_BY_2 {
                let re = vld1q_f32(h_p_ch.re.as_ptr().add(j));
                let im = vld1q_f32(h_p_ch.im.as_ptr().add(j));
                let magnitude = vmlaq_f32(vmulq_f32(re, re), im, im);
                let current = vld1q_f32(h2_p.as_ptr().add(j));
                vst1q_f32(h2_p.as_mut_ptr().add(j), vmaxq_f32(current, magnitude));
                j += 4;
            }
            let nyquist = h_p_ch.re[FFT_LENGTH_BY_2] * h_p_ch.re[FFT_LENGTH_BY_2]
                + h_p_ch.im[FFT_LENGTH_BY_2] * h_p_ch.im[FFT_LENGTH_BY_2];
            h2_p[FFT_LENGTH_BY_2] = h2_p[FFT_LENGTH_BY_2].max(nyquist);
        }
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[allow(unsafe_op_in_unsafe_fn)]
#[target_feature(enable = "avx2")]
unsafe fn adapt_partitions_avx2_impl(
    render_buffer: &RenderBuffer,
    g: &FftData,
    num_partitions: usize,
    h: &mut [Vec<FftData>],
) {
    let render_fft = render_buffer.fft_buffer();
    let num_render_channels = render_fft[0].len();
    let limit_1 = (render_fft.len() - render_buffer.position()).min(num_partitions);
    let limit_2 = num_partitions;
    let mut x_partition = render_buffer.position();
    let mut limit = limit_1;
    let mut p = 0usize;
    loop {
        while p < limit {
            for ch in 0..num_render_channels {
                let x = &render_fft[x_partition][ch];
                let h_entry = &mut h[p][ch];
                let mut k = 0usize;
                while k < FFT_LENGTH_BY_2 {
                    let g_re = _mm256_loadu_ps(g.re.as_ptr().add(k));
                    let g_im = _mm256_loadu_ps(g.im.as_ptr().add(k));
                    let x_re = _mm256_loadu_ps(x.re.as_ptr().add(k));
                    let x_im = _mm256_loadu_ps(x.im.as_ptr().add(k));
                    let h_re = _mm256_loadu_ps(h_entry.re.as_ptr().add(k));
                    let h_im = _mm256_loadu_ps(h_entry.im.as_ptr().add(k));
                    let new_re = _mm256_add_ps(
                        h_re,
                        _mm256_add_ps(_mm256_mul_ps(x_re, g_re), _mm256_mul_ps(x_im, g_im)),
                    );
                    let new_im = _mm256_add_ps(
                        h_im,
                        _mm256_sub_ps(_mm256_mul_ps(x_re, g_im), _mm256_mul_ps(x_im, g_re)),
                    );
                    _mm256_storeu_ps(h_entry.re.as_mut_ptr().add(k), new_re);
                    _mm256_storeu_ps(h_entry.im.as_mut_ptr().add(k), new_im);
                    k += 8;
                }
                h_entry.re[FFT_LENGTH_BY_2] += x.re[FFT_LENGTH_BY_2] * g.re[FFT_LENGTH_BY_2]
                    + x.im[FFT_LENGTH_BY_2] * g.im[FFT_LENGTH_BY_2];
                h_entry.im[FFT_LENGTH_BY_2] += x.re[FFT_LENGTH_BY_2] * g.im[FFT_LENGTH_BY_2]
                    - x.im[FFT_LENGTH_BY_2] * g.re[FFT_LENGTH_BY_2];
            }
            p += 1;
            x_partition += 1;
        }
        if p >= limit_2 {
            break;
        }
        x_partition = 0;
        limit = limit_2;
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[allow(unsafe_op_in_unsafe_fn)]
#[target_feature(enable = "sse2")]
unsafe fn adapt_partitions_sse2_impl(
    render_buffer: &RenderBuffer,
    g: &FftData,
    num_partitions: usize,
    h: &mut [Vec<FftData>],
) {
    let render_fft = render_buffer.fft_buffer();
    let num_render_channels = render_fft[0].len();
    let limit_1 = (render_fft.len() - render_buffer.position()).min(num_partitions);
    let limit_2 = num_partitions;
    let mut x_partition = render_buffer.position();
    let mut limit = limit_1;
    let mut p = 0usize;
    loop {
        while p < limit {
            for ch in 0..num_render_channels {
                let x = &render_fft[x_partition][ch];
                let h_entry = &mut h[p][ch];
                let mut k = 0usize;
                while k < FFT_LENGTH_BY_2 {
                    let g_re = _mm_loadu_ps(g.re.as_ptr().add(k));
                    let g_im = _mm_loadu_ps(g.im.as_ptr().add(k));
                    let x_re = _mm_loadu_ps(x.re.as_ptr().add(k));
                    let x_im = _mm_loadu_ps(x.im.as_ptr().add(k));
                    let h_re = _mm_loadu_ps(h_entry.re.as_ptr().add(k));
                    let h_im = _mm_loadu_ps(h_entry.im.as_ptr().add(k));
                    let new_re = _mm_add_ps(
                        h_re,
                        _mm_add_ps(_mm_mul_ps(x_re, g_re), _mm_mul_ps(x_im, g_im)),
                    );
                    let new_im = _mm_add_ps(
                        h_im,
                        _mm_sub_ps(_mm_mul_ps(x_re, g_im), _mm_mul_ps(x_im, g_re)),
                    );
                    _mm_storeu_ps(h_entry.re.as_mut_ptr().add(k), new_re);
                    _mm_storeu_ps(h_entry.im.as_mut_ptr().add(k), new_im);
                    k += 4;
                }
                h_entry.re[FFT_LENGTH_BY_2] += x.re[FFT_LENGTH_BY_2] * g.re[FFT_LENGTH_BY_2]
                    + x.im[FFT_LENGTH_BY_2] * g.im[FFT_LENGTH_BY_2];
                h_entry.im[FFT_LENGTH_BY_2] += x.re[FFT_LENGTH_BY_2] * g.im[FFT_LENGTH_BY_2]
                    - x.im[FFT_LENGTH_BY_2] * g.re[FFT_LENGTH_BY_2];
            }
            p += 1;
            x_partition += 1;
        }
        if p >= limit_2 {
            break;
        }
        x_partition = 0;
        limit = limit_2;
    }
}

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_op_in_unsafe_fn)]
#[target_feature(enable = "neon")]
unsafe fn adapt_partitions_neon_impl(
    render_buffer: &RenderBuffer,
    g: &FftData,
    num_partitions: usize,
    h: &mut [Vec<FftData>],
) {
    let render_fft = render_buffer.fft_buffer();
    let num_render_channels = render_fft[0].len();
    let limit_1 = (render_fft.len() - render_buffer.position()).min(num_partitions);
    let limit_2 = num_partitions;
    let mut x_partition = render_buffer.position();
    let mut limit = limit_1;
    let mut p = 0usize;
    loop {
        while p < limit {
            for ch in 0..num_render_channels {
                let x = &render_fft[x_partition][ch];
                let h_entry = &mut h[p][ch];
                let mut k = 0usize;
                while k < FFT_LENGTH_BY_2 {
                    let g_re = vld1q_f32(g.re.as_ptr().add(k));
                    let g_im = vld1q_f32(g.im.as_ptr().add(k));
                    let x_re = vld1q_f32(x.re.as_ptr().add(k));
                    let x_im = vld1q_f32(x.im.as_ptr().add(k));
                    let h_re = vld1q_f32(h_entry.re.as_ptr().add(k));
                    let h_im = vld1q_f32(h_entry.im.as_ptr().add(k));
                    let new_re = vaddq_f32(h_re, vmlaq_f32(vmulq_f32(x_re, g_re), x_im, g_im));
                    let new_im = vaddq_f32(h_im, vmlsq_f32(vmulq_f32(x_re, g_im), x_im, g_re));
                    vst1q_f32(h_entry.re.as_mut_ptr().add(k), new_re);
                    vst1q_f32(h_entry.im.as_mut_ptr().add(k), new_im);
                    k += 4;
                }
                h_entry.re[FFT_LENGTH_BY_2] += x.re[FFT_LENGTH_BY_2] * g.re[FFT_LENGTH_BY_2]
                    + x.im[FFT_LENGTH_BY_2] * g.im[FFT_LENGTH_BY_2];
                h_entry.im[FFT_LENGTH_BY_2] += x.re[FFT_LENGTH_BY_2] * g.im[FFT_LENGTH_BY_2]
                    - x.im[FFT_LENGTH_BY_2] * g.re[FFT_LENGTH_BY_2];
            }
            p += 1;
            x_partition += 1;
        }
        if p >= limit_2 {
            break;
        }
        x_partition = 0;
        limit = limit_2;
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[allow(unsafe_op_in_unsafe_fn)]
#[target_feature(enable = "avx2")]
unsafe fn apply_filter_avx2_impl(
    render_buffer: &RenderBuffer,
    num_partitions: usize,
    h: &[Vec<FftData>],
    s: &mut FftData,
) {
    s.re.fill(0.0);
    s.im.fill(0.0);
    if num_partitions == 0 {
        return;
    }
    let render_fft = render_buffer.fft_buffer();
    let num_render_channels = render_fft[0].len();
    let limit_1 = (render_fft.len() - render_buffer.position()).min(num_partitions);
    let limit_2 = num_partitions;
    let mut x_partition = render_buffer.position();
    let mut limit = limit_1;
    let mut p = 0usize;
    loop {
        while p < limit {
            for ch in 0..num_render_channels {
                let x = &render_fft[x_partition][ch];
                let h_entry = &h[p][ch];
                let mut k = 0usize;
                while k < FFT_LENGTH_BY_2 {
                    let x_re = _mm256_loadu_ps(x.re.as_ptr().add(k));
                    let x_im = _mm256_loadu_ps(x.im.as_ptr().add(k));
                    let h_re = _mm256_loadu_ps(h_entry.re.as_ptr().add(k));
                    let h_im = _mm256_loadu_ps(h_entry.im.as_ptr().add(k));
                    let s_re = _mm256_loadu_ps(s.re.as_ptr().add(k));
                    let s_im = _mm256_loadu_ps(s.im.as_ptr().add(k));
                    let new_re = _mm256_add_ps(
                        s_re,
                        _mm256_sub_ps(_mm256_mul_ps(x_re, h_re), _mm256_mul_ps(x_im, h_im)),
                    );
                    let new_im = _mm256_add_ps(
                        s_im,
                        _mm256_add_ps(_mm256_mul_ps(x_re, h_im), _mm256_mul_ps(x_im, h_re)),
                    );
                    _mm256_storeu_ps(s.re.as_mut_ptr().add(k), new_re);
                    _mm256_storeu_ps(s.im.as_mut_ptr().add(k), new_im);
                    k += 8;
                }
                s.re[FFT_LENGTH_BY_2] += x.re[FFT_LENGTH_BY_2] * h_entry.re[FFT_LENGTH_BY_2]
                    - x.im[FFT_LENGTH_BY_2] * h_entry.im[FFT_LENGTH_BY_2];
                s.im[FFT_LENGTH_BY_2] += x.re[FFT_LENGTH_BY_2] * h_entry.im[FFT_LENGTH_BY_2]
                    + x.im[FFT_LENGTH_BY_2] * h_entry.re[FFT_LENGTH_BY_2];
            }
            p += 1;
            x_partition += 1;
        }
        if p >= limit_2 {
            break;
        }
        x_partition = 0;
        limit = limit_2;
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[allow(unsafe_op_in_unsafe_fn)]
#[target_feature(enable = "sse2")]
unsafe fn apply_filter_sse2_impl(
    render_buffer: &RenderBuffer,
    num_partitions: usize,
    h: &[Vec<FftData>],
    s: &mut FftData,
) {
    s.re.fill(0.0);
    s.im.fill(0.0);
    if num_partitions == 0 {
        return;
    }
    let render_fft = render_buffer.fft_buffer();
    let num_render_channels = render_fft[0].len();
    let limit_1 = (render_fft.len() - render_buffer.position()).min(num_partitions);
    let limit_2 = num_partitions;
    let mut x_partition = render_buffer.position();
    let mut limit = limit_1;
    let mut p = 0usize;
    loop {
        while p < limit {
            for ch in 0..num_render_channels {
                let x = &render_fft[x_partition][ch];
                let h_entry = &h[p][ch];
                let mut k = 0usize;
                while k < FFT_LENGTH_BY_2 {
                    let x_re = _mm_loadu_ps(x.re.as_ptr().add(k));
                    let x_im = _mm_loadu_ps(x.im.as_ptr().add(k));
                    let h_re = _mm_loadu_ps(h_entry.re.as_ptr().add(k));
                    let h_im = _mm_loadu_ps(h_entry.im.as_ptr().add(k));
                    let s_re = _mm_loadu_ps(s.re.as_ptr().add(k));
                    let s_im = _mm_loadu_ps(s.im.as_ptr().add(k));
                    let new_re = _mm_add_ps(
                        s_re,
                        _mm_sub_ps(_mm_mul_ps(x_re, h_re), _mm_mul_ps(x_im, h_im)),
                    );
                    let new_im = _mm_add_ps(
                        s_im,
                        _mm_add_ps(_mm_mul_ps(x_re, h_im), _mm_mul_ps(x_im, h_re)),
                    );
                    _mm_storeu_ps(s.re.as_mut_ptr().add(k), new_re);
                    _mm_storeu_ps(s.im.as_mut_ptr().add(k), new_im);
                    k += 4;
                }
                s.re[FFT_LENGTH_BY_2] += x.re[FFT_LENGTH_BY_2] * h_entry.re[FFT_LENGTH_BY_2]
                    - x.im[FFT_LENGTH_BY_2] * h_entry.im[FFT_LENGTH_BY_2];
                s.im[FFT_LENGTH_BY_2] += x.re[FFT_LENGTH_BY_2] * h_entry.im[FFT_LENGTH_BY_2]
                    + x.im[FFT_LENGTH_BY_2] * h_entry.re[FFT_LENGTH_BY_2];
            }
            p += 1;
            x_partition += 1;
        }
        if p >= limit_2 {
            break;
        }
        x_partition = 0;
        limit = limit_2;
    }
}

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_op_in_unsafe_fn)]
#[target_feature(enable = "neon")]
unsafe fn apply_filter_neon_impl(
    render_buffer: &RenderBuffer,
    num_partitions: usize,
    h: &[Vec<FftData>],
    s: &mut FftData,
) {
    s.re.fill(0.0);
    s.im.fill(0.0);
    if num_partitions == 0 {
        return;
    }
    let render_fft = render_buffer.fft_buffer();
    let num_render_channels = render_fft[0].len();
    let limit_1 = (render_fft.len() - render_buffer.position()).min(num_partitions);
    let limit_2 = num_partitions;
    let mut x_partition = render_buffer.position();
    let mut limit = limit_1;
    let mut p = 0usize;
    loop {
        while p < limit {
            for ch in 0..num_render_channels {
                let x = &render_fft[x_partition][ch];
                let h_entry = &h[p][ch];
                let mut k = 0usize;
                while k < FFT_LENGTH_BY_2 {
                    let x_re = vld1q_f32(x.re.as_ptr().add(k));
                    let x_im = vld1q_f32(x.im.as_ptr().add(k));
                    let h_re = vld1q_f32(h_entry.re.as_ptr().add(k));
                    let h_im = vld1q_f32(h_entry.im.as_ptr().add(k));
                    let s_re = vld1q_f32(s.re.as_ptr().add(k));
                    let s_im = vld1q_f32(s.im.as_ptr().add(k));
                    let new_re = vaddq_f32(s_re, vmlsq_f32(vmulq_f32(x_re, h_re), x_im, h_im));
                    let new_im = vaddq_f32(s_im, vmlaq_f32(vmulq_f32(x_re, h_im), x_im, h_re));
                    vst1q_f32(s.re.as_mut_ptr().add(k), new_re);
                    vst1q_f32(s.im.as_mut_ptr().add(k), new_im);
                    k += 4;
                }
                s.re[FFT_LENGTH_BY_2] += x.re[FFT_LENGTH_BY_2] * h_entry.re[FFT_LENGTH_BY_2]
                    - x.im[FFT_LENGTH_BY_2] * h_entry.im[FFT_LENGTH_BY_2];
                s.im[FFT_LENGTH_BY_2] += x.re[FFT_LENGTH_BY_2] * h_entry.im[FFT_LENGTH_BY_2]
                    + x.im[FFT_LENGTH_BY_2] * h_entry.re[FFT_LENGTH_BY_2];
            }
            p += 1;
            x_partition += 1;
        }
        if p >= limit_2 {
            break;
        }
        x_partition = 0;
        limit = limit_2;
    }
}

/// Frequency-domain adaptive FIR filter mirroring the WebRTC reference implementation.
pub struct AdaptiveFirFilter {
    _data_dumper: ApmDataDumper,
    fft: Aec3Fft,
    optimization: Aec3Optimization,
    num_render_channels: usize,
    max_size_partitions: usize,
    size_change_duration_blocks: i32,
    one_by_size_change_duration_blocks: f32,
    current_size_partitions: usize,
    target_size_partitions: usize,
    old_target_size_partitions: usize,
    size_change_counter: i32,
    h: Vec<Vec<FftData>>,
    partition_to_constrain: usize,
}

impl AdaptiveFirFilter {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        max_size_partitions: usize,
        initial_size_partitions: usize,
        size_change_duration_blocks: usize,
        num_render_channels: usize,
        optimization: Aec3Optimization,
        data_dumper: ApmDataDumper,
    ) -> Self {
        assert!(max_size_partitions >= initial_size_partitions);
        assert!(size_change_duration_blocks > 0);
        assert!(num_render_channels > 0);

        let mut h = Vec::with_capacity(max_size_partitions);
        for _ in 0..max_size_partitions {
            h.push(vec![FftData::default(); num_render_channels]);
        }

        let size_change_duration_blocks = size_change_duration_blocks as i32;
        let mut filter = Self {
            _data_dumper: data_dumper,
            fft: Aec3Fft::new(),
            optimization,
            num_render_channels,
            max_size_partitions,
            size_change_duration_blocks,
            one_by_size_change_duration_blocks: 1.0 / size_change_duration_blocks as f32,
            current_size_partitions: initial_size_partitions,
            target_size_partitions: initial_size_partitions,
            old_target_size_partitions: initial_size_partitions,
            size_change_counter: 0,
            h,
            partition_to_constrain: 0,
        };
        zero_filter(0, filter.max_size_partitions, &mut filter.h);
        filter.set_size_partitions(initial_size_partitions, true);
        filter
    }

    pub fn handle_echo_path_change(&mut self) {
        zero_filter(
            self.current_size_partitions,
            self.max_size_partitions,
            &mut self.h,
        );
    }

    pub fn size_partitions(&self) -> usize {
        self.current_size_partitions
    }

    pub fn max_filter_size_partitions(&self) -> usize {
        self.max_size_partitions
    }

    pub fn set_size_partitions(&mut self, size: usize, immediate_effect: bool) {
        let size = size.min(self.max_size_partitions);
        self.target_size_partitions = size;
        if immediate_effect {
            let old_size = self.current_size_partitions;
            self.current_size_partitions = size;
            self.old_target_size_partitions = size;
            zero_filter(old_size, self.current_size_partitions, &mut self.h);
            if self.current_size_partitions > 0 {
                self.partition_to_constrain = self
                    .partition_to_constrain
                    .min(self.current_size_partitions - 1);
            } else {
                self.partition_to_constrain = 0;
            }
            self.size_change_counter = 0;
        } else {
            self.size_change_counter = self.size_change_duration_blocks;
        }
    }

    pub fn filter(&self, render_buffer: &RenderBuffer, s: &mut FftData) {
        match self.optimization {
            Aec3Optimization::Avx2 => {
                apply_filter_avx2(render_buffer, self.current_size_partitions, &self.h, s)
            }
            Aec3Optimization::Sse2 => {
                apply_filter_sse2(render_buffer, self.current_size_partitions, &self.h, s)
            }
            Aec3Optimization::Neon => {
                apply_filter_neon(render_buffer, self.current_size_partitions, &self.h, s)
            }
            Aec3Optimization::None => {
                apply_filter(render_buffer, self.current_size_partitions, &self.h, s)
            }
        }
    }

    pub fn adapt(&mut self, render_buffer: &RenderBuffer, g: &FftData) {
        self.adapt_and_update_size(render_buffer, g);
        self.constrain();
    }

    pub fn adapt_with_impulse_response(
        &mut self,
        render_buffer: &RenderBuffer,
        g: &FftData,
        impulse_response: &mut Vec<f32>,
    ) {
        self.adapt_and_update_size(render_buffer, g);
        self.constrain_and_update_impulse_response(impulse_response);
    }

    pub fn compute_frequency_response(&self, h2: &mut Vec<[f32; FFT_LENGTH_BY_2_PLUS_1]>) {
        if h2.len() < self.current_size_partitions {
            h2.resize(self.current_size_partitions, [0.0; FFT_LENGTH_BY_2_PLUS_1]);
        }
        match self.optimization {
            Aec3Optimization::Avx2 => {
                compute_frequency_response_avx2(self.current_size_partitions, &self.h, h2)
            }
            Aec3Optimization::Sse2 => {
                compute_frequency_response_sse2(self.current_size_partitions, &self.h, h2)
            }
            Aec3Optimization::Neon => {
                compute_frequency_response_neon(self.current_size_partitions, &self.h, h2)
            }
            Aec3Optimization::None => {
                compute_frequency_response(self.current_size_partitions, &self.h, h2)
            }
        }
    }

    pub fn scale_filter(&mut self, factor: f32) {
        for partition in &mut self.h {
            for channel in partition {
                for value in channel.re.iter_mut() {
                    *value *= factor;
                }
                for value in channel.im.iter_mut() {
                    *value *= factor;
                }
            }
        }
    }

    pub fn set_filter(&mut self, num_partitions: usize, h: &[Vec<FftData>]) {
        let limit = num_partitions.min(self.current_size_partitions);
        assert!(h.len() >= limit);
        for p in 0..limit {
            assert_eq!(self.h[p].len(), h[p].len());
            for ch in 0..self.num_render_channels {
                self.h[p][ch] = h[p][ch].clone();
            }
        }
    }

    pub fn get_filter(&self) -> &[Vec<FftData>] {
        &self.h
    }

    fn adapt_and_update_size(&mut self, render_buffer: &RenderBuffer, g: &FftData) {
        self.update_size();
        match self.optimization {
            Aec3Optimization::Avx2 => {
                adapt_partitions_avx2(render_buffer, g, self.current_size_partitions, &mut self.h)
            }
            Aec3Optimization::Sse2 => {
                adapt_partitions_sse2(render_buffer, g, self.current_size_partitions, &mut self.h)
            }
            Aec3Optimization::Neon => {
                adapt_partitions_neon(render_buffer, g, self.current_size_partitions, &mut self.h)
            }
            Aec3Optimization::None => {
                adapt_partitions(render_buffer, g, self.current_size_partitions, &mut self.h)
            }
        }
    }

    fn constrain(&mut self) {
        if self.current_size_partitions == 0 {
            return;
        }
        let mut h_time = [0.0f32; FFT_LENGTH];
        let partition = self
            .partition_to_constrain
            .min(self.current_size_partitions - 1);
        for ch in 0..self.num_render_channels {
            self.fft.ifft(&self.h[partition][ch], &mut h_time);
            for sample in &mut h_time[..FFT_LENGTH_BY_2] {
                *sample *= 1.0 / FFT_LENGTH_BY_2 as f32;
            }
            h_time[FFT_LENGTH_BY_2..].fill(0.0);
            self.fft.fft(&mut h_time, &mut self.h[partition][ch]);
        }
        self.partition_to_constrain = if partition + 1 < self.current_size_partitions {
            partition + 1
        } else {
            0
        };
    }

    fn constrain_and_update_impulse_response(&mut self, impulse_response: &mut Vec<f32>) {
        if self.current_size_partitions == 0 {
            impulse_response.clear();
            return;
        }

        let required_capacity = get_time_domain_length(self.max_size_partitions);
        if impulse_response.capacity() < required_capacity {
            impulse_response.reserve(required_capacity - impulse_response.capacity());
        }
        impulse_response.resize(get_time_domain_length(self.current_size_partitions), 0.0);

        let mut h_time = [0.0f32; FFT_LENGTH];
        let partition = self
            .partition_to_constrain
            .min(self.current_size_partitions - 1);
        let base = partition * FFT_LENGTH_BY_2;
        impulse_response[base..base + FFT_LENGTH_BY_2].fill(0.0);

        for ch in 0..self.num_render_channels {
            self.fft.ifft(&self.h[partition][ch], &mut h_time);
            for sample in &mut h_time[..FFT_LENGTH_BY_2] {
                *sample *= 1.0 / FFT_LENGTH_BY_2 as f32;
            }
            h_time[FFT_LENGTH_BY_2..].fill(0.0);

            if ch == 0 {
                impulse_response[base..base + FFT_LENGTH_BY_2]
                    .copy_from_slice(&h_time[..FFT_LENGTH_BY_2]);
            } else {
                for (dst, &src) in impulse_response[base..base + FFT_LENGTH_BY_2]
                    .iter_mut()
                    .zip(&h_time[..FFT_LENGTH_BY_2])
                {
                    if dst.abs() < src.abs() {
                        *dst = src;
                    }
                }
            }

            self.fft.fft(&mut h_time, &mut self.h[partition][ch]);
        }

        self.partition_to_constrain = if partition + 1 < self.current_size_partitions {
            partition + 1
        } else {
            0
        };
    }

    fn update_size(&mut self) {
        let old_size = self.current_size_partitions;
        if self.size_change_counter > 0 {
            self.size_change_counter -= 1;
            let change_factor =
                self.size_change_counter as f32 * self.one_by_size_change_duration_blocks;
            let averaged = self.old_target_size_partitions as f32 * change_factor
                + self.target_size_partitions as f32 * (1.0 - change_factor);
            self.current_size_partitions = averaged.max(1.0) as usize;
            if self.current_size_partitions > 0 {
                self.partition_to_constrain = self
                    .partition_to_constrain
                    .min(self.current_size_partitions - 1);
            } else {
                self.partition_to_constrain = 0;
            }
        } else {
            self.current_size_partitions = self.target_size_partitions;
            self.old_target_size_partitions = self.target_size_partitions;
        }
        zero_filter(old_size, self.current_size_partitions, &mut self.h);
    }
}

fn zero_filter(start: usize, end: usize, h: &mut [Vec<FftData>]) {
    if start >= end {
        return;
    }
    for partition in &mut h[start..end] {
        for channel in partition {
            channel.clear();
        }
    }
}

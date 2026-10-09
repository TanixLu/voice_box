use std::fmt;

/// Identifies the CPU-specific optimization path used by the AEC3 pipeline.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Aec3Optimization {
    None,
    Sse2,
    Avx2,
    Neon,
}

pub const NUM_BLOCKS_PER_SECOND: usize = 250;
pub const FFT_LENGTH_BY_2: usize = 64;
pub const FFT_LENGTH_BY_2_PLUS_1: usize = FFT_LENGTH_BY_2 + 1;
pub const FFT_LENGTH_BY_2_MINUS_1: usize = FFT_LENGTH_BY_2 - 1;
pub const FFT_LENGTH: usize = 2 * FFT_LENGTH_BY_2;
pub const FFT_LENGTH_BY_2_LOG2: usize = 6;
pub const BLOCK_SIZE: usize = FFT_LENGTH_BY_2;
pub const BLOCK_SIZE_LOG2: usize = FFT_LENGTH_BY_2_LOG2;
pub const MATCHED_FILTER_WINDOW_SIZE_SUB_BLOCKS: usize = 32;
pub const MATCHED_FILTER_ALIGNMENT_SHIFT_SIZE_SUB_BLOCKS: usize =
    MATCHED_FILTER_WINDOW_SIZE_SUB_BLOCKS * 3 / 4;

/// Returns the number of bands for a given sampling rate.
pub const fn num_bands_for_rate(sample_rate_hz: i32) -> usize {
    if sample_rate_hz <= 0 {
        0
    } else {
        (sample_rate_hz as usize) / 16_000
    }
}

/// Checks whether the provided sampling rate is supported by the full-band pipeline.
pub const fn valid_full_band_rate(sample_rate_hz: i32) -> bool {
    matches!(sample_rate_hz, 16_000 | 32_000 | 48_000)
}

pub const fn get_time_domain_length(filter_length_blocks: usize) -> usize {
    filter_length_blocks * FFT_LENGTH_BY_2
}

pub fn get_down_sampled_buffer_size(
    down_sampling_factor: usize,
    num_matched_filters: usize,
) -> usize {
    assert!(down_sampling_factor > 0);
    let blocks_per_factor = BLOCK_SIZE / down_sampling_factor;
    blocks_per_factor
        * (MATCHED_FILTER_ALIGNMENT_SHIFT_SIZE_SUB_BLOCKS * num_matched_filters
            + MATCHED_FILTER_WINDOW_SIZE_SUB_BLOCKS
            + 1)
}

pub fn get_render_delay_buffer_size(
    down_sampling_factor: usize,
    num_matched_filters: usize,
    filter_length_blocks: usize,
) -> usize {
    assert!(down_sampling_factor > 0);
    let base = get_down_sampled_buffer_size(down_sampling_factor, num_matched_filters);
    let blocks_per_factor = BLOCK_SIZE / down_sampling_factor;
    base / blocks_per_factor + filter_length_blocks + 1
}

pub fn fast_approx_log2f(input: f32) -> f32 {
    assert!(input > 0.0, "input must be positive");
    let bits = input.to_bits();
    let mut out = bits as f32;
    out *= 1.192_092_9e-7_f32;
    out -= 126.942_695_f32;
    out
}

pub fn detect_optimization() -> Aec3Optimization {
    let has_avx2 = detect_avx2();
    let has_sse2 = detect_sse2();
    let has_neon = detect_neon();
    resolve_optimization(has_avx2, has_sse2, has_neon)
}

fn resolve_optimization(has_avx2: bool, has_sse2: bool, has_neon: bool) -> Aec3Optimization {
    if has_avx2 {
        Aec3Optimization::Avx2
    } else if has_sse2 {
        Aec3Optimization::Sse2
    } else if has_neon {
        Aec3Optimization::Neon
    } else {
        Aec3Optimization::None
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub(crate) fn detect_avx2() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
pub(crate) fn detect_avx2() -> bool {
    false
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub(crate) fn detect_sse2() -> bool {
    std::arch::is_x86_feature_detected!("sse2")
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
pub(crate) fn detect_sse2() -> bool {
    false
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn detect_neon() -> bool {
    std::arch::is_aarch64_feature_detected!("neon")
}

#[cfg(not(target_arch = "aarch64"))]
pub(crate) fn detect_neon() -> bool {
    false
}

impl fmt::Display for Aec3Optimization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Aec3Optimization::None => "none",
            Aec3Optimization::Sse2 => "sse2",
            Aec3Optimization::Avx2 => "avx2",
            Aec3Optimization::Neon => "neon",
        };
        f.write_str(label)
    }
}

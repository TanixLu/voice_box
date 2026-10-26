//! Offline entry point into the same processor used by the real-time APO.
use std::{mem::size_of, ptr, slice};

use windows::Win32::Foundation::{E_INVALIDARG, E_POINTER, S_OK};
use windows_core::HRESULT;

use super::dsp::{AudioProcessor, MAX_CHANNELS, MAX_SAMPLE_RATE, MIN_SAMPLE_RATE, SampleFormat};
use voice_box_shared::{apo_cfg::ApoConfig, preview};

#[unsafe(no_mangle)]
/// Process and align a complete recording with a fresh DSP state.
///
/// # Safety
/// `input` must contain `frames * channels` readable, aligned float32 samples;
/// `output` must have the same writable capacity. Buffers may overlap. The
/// caller must retain its DLL reference until this synchronous call returns.
pub unsafe extern "system" fn VoiceBoxPreviewProcess(
    sample_rate: u32,
    channels: u32,
    frames: u32,
    gain_millidb: i32,
    denoising: u32,
    input: *const f32,
    output: *mut f32,
) -> HRESULT {
    let config = ApoConfig {
        gain_millidb,
        denoising_enabled: denoising != 0,
    };
    let Some(samples) = (frames as usize).checked_mul(channels as usize) else {
        return E_INVALIDARG;
    };
    if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&f64::from(sample_rate))
        || !(1..=MAX_CHANNELS).contains(&(channels as usize))
        || frames == 0
        || frames > sample_rate * preview::MAX_RECORD_SECONDS
        || samples > preview::MAX_SAMPLES
        || denoising > 1
        || config.validate().is_err()
    {
        return E_INVALIDARG;
    }
    if input.is_null() || output.is_null() {
        return E_POINTER;
    }
    if !input.is_aligned() || !output.is_aligned() {
        return E_INVALIDARG;
    }
    let gain = config.linear_gain();
    if gain == 0.0 {
        unsafe { ptr::write_bytes(output, 0, samples) };
        return S_OK;
    }
    let mut processor = AudioProcessor::default();
    if let Err(error) = processor.prepare(
        SampleFormat::Float32,
        channels as usize,
        f64::from(sample_rate),
        true,
        false,
    ) {
        return error.code();
    }
    // Flush the entire latency, including resamplers, STFT/FIFO and limiter,
    // then remove it so A/B playback has the same length and starting point.
    let delay_frames = processor.latency_frames();
    let delay_samples = delay_frames * channels as usize;
    let mut buffer = unsafe { slice::from_raw_parts(input, samples) }.to_vec();
    buffer.resize(samples + delay_samples, 0.0);
    let bytes = unsafe {
        slice::from_raw_parts_mut(
            buffer.as_mut_ptr().cast::<u8>(),
            buffer.len() * size_of::<f32>(),
        )
    };
    processor.process_with_config(bytes, gain, config.denoising_enabled);
    // `buffer` is DLL-owned; only sample bytes are copied back to the caller.
    unsafe { ptr::copy_nonoverlapping(buffer.as_ptr().add(delay_samples), output, samples) };
    S_OK
}

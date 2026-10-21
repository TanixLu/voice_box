use super::{
    ApoCom, DspAccess,
    formats::{
        formats_match, frame_bytes, is_supported_format, log_connection_format, read_format,
        sample_format,
    },
    object_from,
};
use crate::apo::etw::{self, apo_log};
use core::{ffi::c_void, ptr};
use std::sync::atomic::Ordering;
use windows::Win32::{
    Foundation::{E_INVALIDARG, E_POINTER, S_OK},
    Media::Audio::Apo::{
        APO_CONNECTION_DESCRIPTOR, APO_CONNECTION_PROPERTY, APOERR_ALREADY_UNLOCKED,
        APOERR_APO_LOCKED, APOERR_INVALID_CONNECTION_FORMAT, APOERR_INVALID_OUTPUT_MAXFRAMECOUNT,
        APOERR_NOT_INITIALIZED, APOERR_NUM_CONNECTIONS_INVALID, BUFFER_INVALID, BUFFER_SILENT,
        BUFFER_VALID,
    },
};
use windows_core::{HRESULT, Interface};

pub(super) unsafe extern "system" fn reset(this: *mut c_void) -> HRESULT {
    let object = unsafe { object_from(this) };
    unsafe { (*object).reset_requested.store(true, Ordering::Release) };
    S_OK
}

pub(super) unsafe extern "system" fn get_latency(this: *mut c_void, latency: *mut i64) -> HRESULT {
    if latency.is_null() {
        return E_POINTER;
    }
    let object = unsafe { object_from(this) };
    unsafe { *latency = (*object).latency_hns.load(Ordering::Acquire) };
    S_OK
}

fn log_lock_failure(
    object: *mut ApoCom,
    input_count: u32,
    output_count: u32,
    reason: &str,
    result: HRESULT,
) -> HRESULT {
    apo_log!(
        etw::ERROR,
        etw::SETTINGS,
        "LockForProcess failed instance={object:p} inputs={input_count} outputs={output_count} reason={reason} result=0x{:08x}",
        result.0 as u32
    );
    result
}

pub(super) unsafe extern "system" fn lock_for_process(
    this: *mut c_void,
    input_count: u32,
    inputs: *const *const APO_CONNECTION_DESCRIPTOR,
    output_count: u32,
    outputs: *const *const APO_CONNECTION_DESCRIPTOR,
) -> HRESULT {
    let object = unsafe { object_from(this) };
    if !unsafe { (*object).initialized.load(Ordering::Acquire) } {
        return log_lock_failure(
            object,
            input_count,
            output_count,
            "not initialized",
            APOERR_NOT_INITIALIZED,
        );
    }
    if input_count != 1 || output_count != 1 {
        return log_lock_failure(
            object,
            input_count,
            output_count,
            "connection count must be 1 input and 1 output",
            APOERR_NUM_CONNECTIONS_INVALID,
        );
    }
    if inputs.is_null()
        || outputs.is_null()
        || unsafe { (*inputs).is_null() || (*outputs).is_null() }
    {
        return log_lock_failure(
            object,
            input_count,
            output_count,
            "null connection descriptor",
            E_POINTER,
        );
    }
    let input_format = unsafe { (**inputs).pFormat.as_ref() };
    let output_format = unsafe { (**outputs).pFormat.as_ref() };
    let Some(input_format) = input_format else {
        return log_lock_failure(
            object,
            input_count,
            output_count,
            "null input media type",
            E_POINTER,
        );
    };
    let Some(output_format) = output_format else {
        return log_lock_failure(
            object,
            input_count,
            output_count,
            "null output media type",
            E_POINTER,
        );
    };
    let input_format = match unsafe { read_format(input_format.as_raw()) } {
        Ok(format) => format,
        Err(result) => {
            return log_lock_failure(
                object,
                input_count,
                output_count,
                "cannot read input format",
                result,
            );
        }
    };
    let output_format = match unsafe { read_format(output_format.as_raw()) } {
        Ok(format) => format,
        Err(result) => {
            return log_lock_failure(
                object,
                input_count,
                output_count,
                "cannot read output format",
                result,
            );
        }
    };
    log_connection_format("LockForProcess input", &input_format);
    log_connection_format("LockForProcess output", &output_format);
    if !is_supported_format(&input_format)
        || !is_supported_format(&output_format)
        || !formats_match(&input_format, &output_format)
    {
        return log_lock_failure(
            object,
            input_count,
            output_count,
            "unsupported or mismatched input/output format",
            APOERR_INVALID_CONNECTION_FORMAT,
        );
    }
    let _control =
        unsafe { (*object).control_guard.lock() }.unwrap_or_else(|poisoned| poisoned.into_inner());
    if unsafe { (*object).discovery_only.load(Ordering::Acquire) } {
        return log_lock_failure(
            object,
            input_count,
            output_count,
            "discovery-only instance",
            APOERR_NOT_INITIALIZED,
        );
    }
    if unsafe { (*object).locked.load(Ordering::Acquire) } {
        return log_lock_failure(
            object,
            input_count,
            output_count,
            "already locked",
            APOERR_APO_LOCKED,
        );
    }
    let frame_bytes = frame_bytes(&input_format).expect("validated format has a frame size");
    let sample_format = sample_format(&input_format).expect("validated sample format");
    let max_input_frames = unsafe { (**inputs).u32MaxFrameCount };
    let max_output_frames = unsafe { (**outputs).u32MaxFrameCount };
    if max_output_frames < max_input_frames {
        return log_lock_failure(
            object,
            input_count,
            output_count,
            "output buffer cannot hold maximum input frames",
            APOERR_INVALID_OUTPUT_MAXFRAMECOUNT,
        );
    }
    if max_input_frames == 0
        || max_output_frames == 0
        || ![max_input_frames, max_output_frames]
            .into_iter()
            .all(|frames| {
                (frames as usize)
                    .checked_mul(frame_bytes)
                    .is_some_and(|bytes| bytes <= isize::MAX as usize)
            })
    {
        return log_lock_failure(
            object,
            input_count,
            output_count,
            "invalid connection buffer capacity",
            E_INVALIDARG,
        );
    }
    let Some(_dsp_access) = (unsafe { DspAccess::try_acquire(&(*object).dsp_in_use) }) else {
        return log_lock_failure(
            object,
            input_count,
            output_count,
            "previous processing pass still active",
            APOERR_APO_LOCKED,
        );
    };
    if unsafe {
        (&mut *(*object).dsp.get()).prepare(
            sample_format,
            input_format.dwSamplesPerFrame as usize,
            f64::from(input_format.fFramesPerSecond),
            (*object).effect_enabled.load(Ordering::Acquire),
        )
    }
    .is_err()
    {
        return log_lock_failure(
            object,
            input_count,
            output_count,
            "cannot prepare speech enhancement model",
            APOERR_INVALID_CONNECTION_FORMAT,
        );
    }
    unsafe {
        *(*object).injection.get() = None;
        (*object).injection_stop = None;
        if (*object).effect_enabled.load(Ordering::Acquire)
            && let Some(path) = (*object).soundboard_path.clone()
        {
            match crate::apo::soundboard::Injection::start(
                path,
                f64::from(input_format.fFramesPerSecond),
                input_format.dwSamplesPerFrame as usize,
                sample_format,
            ) {
                Ok(injection) => {
                    (*object).injection_stop = Some(injection.cancellation());
                    *(*object).injection.get() = Some(injection);
                }
                Err(error) => apo_log!(
                    etw::WARNING,
                    etw::LIFECYCLE,
                    "Soundboard unavailable: {error:#}"
                ),
            }
        }
        (*object)
            .latency_hns
            .store((&*(*object).dsp.get()).latency_hns(), Ordering::Release);
        (*object)
            .input_channels
            .store(input_format.dwSamplesPerFrame, Ordering::Release);
        (*object).process_calls.store(0, Ordering::Relaxed);
        (*object).processed_frames.store(0, Ordering::Relaxed);
        (*object)
            .max_input_frames
            .store(max_input_frames, Ordering::Relaxed);
        (*object)
            .max_output_frames
            .store(max_output_frames, Ordering::Relaxed);
        (*object).reset_requested.store(false, Ordering::Release);
        (*object).locked.store(true, Ordering::Release);
    }
    apo_log!(
        etw::INFO,
        etw::SETTINGS,
        "LockForProcess completed instance={object:p} input_max_frames={} output_max_frames={} channels={} frame_bytes={frame_bytes} effect=denoise enabled={} latency_hns={}",
        unsafe { (**inputs).u32MaxFrameCount },
        unsafe { (**outputs).u32MaxFrameCount },
        input_format.dwSamplesPerFrame,
        unsafe { (*object).params.load().0 },
        unsafe { (*object).latency_hns.load(Ordering::Acquire) },
    );
    S_OK
}

pub(super) unsafe extern "system" fn unlock_for_process(this: *mut c_void) -> HRESULT {
    let object = unsafe { object_from(this) };
    let _control =
        unsafe { (*object).control_guard.lock() }.unwrap_or_else(|poisoned| poisoned.into_inner());
    if !unsafe { (*object).locked.swap(false, Ordering::AcqRel) } {
        return APOERR_ALREADY_UNLOCKED;
    }
    if let Some(stop) = unsafe { &(*object).injection_stop } {
        stop.store(true, Ordering::Release);
    }
    apo_log!(
        etw::INFO,
        etw::SETTINGS,
        "UnlockForProcess completed instance={object:p} process_calls={} processed_frames={}",
        unsafe { (*object).process_calls.load(Ordering::Relaxed) },
        unsafe { (*object).processed_frames.load(Ordering::Relaxed) }
    );
    S_OK
}

// Real-time boundary: no logging, allocation, waits, COM calls, or syscalls.
pub(super) unsafe extern "system" fn process(
    this: *mut c_void,
    input_count: u32,
    inputs: *const *const APO_CONNECTION_PROPERTY,
    output_count: u32,
    outputs: *mut *mut APO_CONNECTION_PROPERTY,
) {
    let object = unsafe { object_from(this) };
    unsafe { (*object).process_calls.fetch_add(1, Ordering::Relaxed) };
    if input_count != 1 || output_count != 1 || inputs.is_null() || outputs.is_null() {
        return;
    }
    let input = unsafe { *inputs };
    let output = unsafe { *outputs };
    if input.is_null() || output.is_null() {
        return;
    }
    // The engine may reuse the connection property as well as the audio buffer
    // for an in-place APO. Preserve input metadata before changing the output.
    let input = unsafe { *input };
    let output = unsafe { &mut *output };
    output.u32ValidFrameCount = 0;
    output.u32BufferFlags = BUFFER_INVALID;
    let Some(_dsp_access) = (unsafe { DspAccess::try_acquire(&(*object).dsp_in_use) }) else {
        return;
    };
    if !unsafe { (*object).locked.load(Ordering::Acquire) } {
        return;
    }
    if input.u32ValidFrameCount > unsafe { (*object).max_input_frames.load(Ordering::Relaxed) }
        || input.u32ValidFrameCount > unsafe { (*object).max_output_frames.load(Ordering::Relaxed) }
    {
        return;
    }
    if input.u32BufferFlags == BUFFER_INVALID {
        return;
    }
    if output.pBuffer == 0 {
        return;
    }
    let bytes_per_frame = unsafe { (&*(*object).dsp.get()).frame_bytes() };
    let Some(bytes) = (input.u32ValidFrameCount as usize).checked_mul(bytes_per_frame) else {
        return;
    };
    let destination = output.pBuffer as *mut u8;
    if input.u32BufferFlags == BUFFER_VALID {
        if input.pBuffer == 0 {
            return;
        }
        let source = input.pBuffer as *const u8;
        if source != destination {
            unsafe { ptr::copy(source, destination, bytes) };
        }
    } else if input.u32BufferFlags == BUFFER_SILENT {
        let silence = unsafe { (&*(*object).dsp.get()).silence_byte() };
        unsafe { ptr::write_bytes(destination, silence, bytes) };
    } else {
        return;
    }
    if unsafe { (*object).reset_requested.swap(false, Ordering::AcqRel) } {
        unsafe { (&mut *(*object).dsp.get()).reset() };
    }
    let mut output_flags = input.u32BufferFlags;
    if unsafe { (*object).effect_enabled.load(Ordering::Acquire) } {
        let buffer = unsafe { core::slice::from_raw_parts_mut(destination, bytes) };
        let params = unsafe { &(*object).params };
        let (enabled, gain) = params.load();
        unsafe { (&mut *(*object).dsp.get()).process_with_settings(buffer, gain, enabled) };
        if let Some(injection) = unsafe { &mut *(*object).injection.get() } {
            injection.mix(buffer);
        }
        // The DSP materializes every frame, including startup padding and mute.
        // Keep these packets valid even when all samples are zero: legacy capture
        // clients can stop recording when initial packets are BUFFER_SILENT.
        // Silent input can also yield audio while the model's buffers drain.
        output_flags = BUFFER_VALID;
    }
    output.u32ValidFrameCount = input.u32ValidFrameCount;
    output.u32BufferFlags = output_flags;
    unsafe {
        (*object)
            .processed_frames
            .fetch_add(input.u32ValidFrameCount as u64, Ordering::Relaxed);
    }
}

pub(super) unsafe extern "system" fn identity_frames(_: *mut c_void, frames: u32) -> u32 {
    frames
}

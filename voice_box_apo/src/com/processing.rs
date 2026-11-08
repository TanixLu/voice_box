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

use voice_box_shared::dsp::AudioProcessor;

use crate::{
    com::{
        ApoCom,
        formats::{
            formats_match, is_supported_format, log_connection_format, read_format, sample_format,
        },
        object_from,
        session::ProcessingSession,
    },
    etw::{self, apo_log},
    memory::Resident,
};

pub unsafe extern "system" fn reset(this: *mut c_void) -> HRESULT {
    let object = unsafe { &*object_from(this) };
    object.reset_requested.store(true, Ordering::Relaxed);
    S_OK
}

pub unsafe extern "system" fn get_latency(this: *mut c_void, latency: *mut i64) -> HRESULT {
    if latency.is_null() {
        return E_POINTER;
    }
    let object = unsafe { &*object_from(this) };
    unsafe { *latency = object.control().latency_hns };
    S_OK
}

pub unsafe extern "system" fn lock_for_process(
    this: *mut c_void,
    input_count: u32,
    inputs: *const *const APO_CONNECTION_DESCRIPTOR,
    output_count: u32,
    outputs: *const *const APO_CONNECTION_DESCRIPTOR,
) -> HRESULT {
    let object = unsafe { &*object_from(this) };
    let fail = |reason: &str, result: HRESULT| {
        apo_log!(
            etw::ERROR,
            etw::APO_CFG,
            "LockForProcess failed instance={object:p} inputs={input_count} outputs={output_count} reason={reason} result=0x{:08x}",
            result.0 as u32
        );
        result
    };
    if !object.control().initialized {
        return fail("not initialized", APOERR_NOT_INITIALIZED);
    }
    if input_count != 1 || output_count != 1 {
        return fail(
            "connection count must be 1 input and 1 output",
            APOERR_NUM_CONNECTIONS_INVALID,
        );
    }
    if inputs.is_null()
        || outputs.is_null()
        || unsafe { (*inputs).is_null() || (*outputs).is_null() }
    {
        return fail("null connection descriptor", E_POINTER);
    }
    let input_format = unsafe { (**inputs).pFormat.as_ref() };
    let output_format = unsafe { (**outputs).pFormat.as_ref() };
    let Some(input_format) = input_format else {
        return fail("null input media type", E_POINTER);
    };
    let Some(output_format) = output_format else {
        return fail("null output media type", E_POINTER);
    };
    let max_input_frames = unsafe { (**inputs).u32MaxFrameCount };
    let max_output_frames = unsafe { (**outputs).u32MaxFrameCount };
    let input_format = match unsafe { read_format(input_format.as_raw()) } {
        Ok(format) => format,
        Err(result) => {
            return fail("cannot read input format", result);
        }
    };
    let output_format = match unsafe { read_format(output_format.as_raw()) } {
        Ok(format) => format,
        Err(result) => {
            return fail("cannot read output format", result);
        }
    };
    log_connection_format("LockForProcess input", &input_format);
    log_connection_format("LockForProcess output", &output_format);
    if !is_supported_format(&input_format) || !formats_match(&input_format, &output_format) {
        return fail(
            "unsupported or mismatched input/output format",
            APOERR_INVALID_CONNECTION_FORMAT,
        );
    }
    let mut control = object.control();
    if control.discovery_only {
        return fail("discovery-only instance", APOERR_NOT_INITIALIZED);
    }
    if object.processing.is_accepting() {
        return fail("already locked", APOERR_APO_LOCKED);
    }
    let sample_format = sample_format(&input_format)
        .expect("UNREACHABLE: validated format should have a supported sample format");
    let frame_bytes = input_format.dwSamplesPerFrame as usize * sample_format.bytes();
    if max_output_frames < max_input_frames {
        return fail(
            "output buffer cannot hold maximum input frames",
            APOERR_INVALID_OUTPUT_MAXFRAMECOUNT,
        );
    }
    if max_input_frames == 0
        || (max_output_frames as usize)
            .checked_mul(frame_bytes)
            .is_none_or(|bytes| bytes > isize::MAX as usize)
    {
        return fail("invalid connection buffer capacity", E_INVALIDARG);
    }
    let Some(access) = object.processing.try_control() else {
        return fail("previous processing pass still active", APOERR_APO_LOCKED);
    };
    let resident_pages = if control.resident_pages.is_none() {
        match crate::memory::image_ranges().and_then(|mut ranges| {
            ranges.push((object as *const ApoCom as usize, size_of::<ApoCom>()));
            let config = object.config.as_ref();
            ranges.push((config as *const _ as usize, size_of_val(config)));
            crate::memory::LockedPages::new(ranges)
        }) {
            Ok(pages) => Some(pages),
            Err(error) => {
                return fail(
                    "cannot query or lock processing code and control data",
                    error.code(),
                );
            }
        }
    } else {
        None
    };
    // Capture all DSP allocations before publishing the prepared state.
    // A failed construction or page lock leaves the previous state intact.
    let dsp = match Resident::new(|| {
        AudioProcessor::new(
            sample_format,
            input_format.dwSamplesPerFrame as usize,
            f64::from(input_format.fFramesPerSecond),
            control.supports_effects,
            &object.config.load(),
        )
    }) {
        Ok(dsp) => dsp,
        Err(error) => {
            return fail(
                "cannot allocate or lock speech enhancement state",
                error.code(),
            );
        }
    };
    let injection = if control.supports_effects
        && let Some(name) = control.soundboard_name.clone()
    {
        match crate::soundboard::Injection::start(
            name,
            f64::from(input_format.fFramesPerSecond),
            max_input_frames as usize,
        ) {
            Ok(injection) => Some(injection),
            Err(error) => {
                apo_log!(
                    etw::WARNING,
                    etw::LIFECYCLE,
                    "Soundboard unavailable: {error:#}"
                );
                None
            }
        }
    } else {
        None
    };
    let session = ProcessingSession::new(dsp, injection, max_input_frames);
    let latency_hns = session.latency_hns();
    if let Some(pages) = resident_pages {
        control.resident_pages = Some(pages);
    }
    control.injection_stop = session.cancellation();
    control.latency_hns = latency_hns;
    control.input_channels = input_format.dwSamplesPerFrame;
    object.process_calls.store(0, Ordering::Relaxed);
    object.processed_frames.store(0, Ordering::Relaxed);
    object.reset_requested.store(false, Ordering::Relaxed);
    access.install(session);
    apo_log!(
        etw::INFO,
        etw::APO_CFG,
        "LockForProcess completed instance={object:p} input_max_frames={} output_max_frames={} channels={} frame_bytes={frame_bytes} effect=denoise enabled={} latency_hns={}",
        max_input_frames,
        max_output_frames,
        input_format.dwSamplesPerFrame,
        object.config.load().denoising_enabled,
        latency_hns,
    );
    S_OK
}

pub unsafe extern "system" fn unlock_for_process(this: *mut c_void) -> HRESULT {
    let object = unsafe { &*object_from(this) };
    let mut control = object.control();
    if !object.processing.deactivate() {
        return APOERR_ALREADY_UNLOCKED;
    }
    if let Some(stop) = control.injection_stop.take() {
        stop.store(true, Ordering::Relaxed);
    }
    // Never wait for a callback: retain its session until the next lock or drop.
    if let Some(access) = object.processing.try_control() {
        access.clear();
    }
    apo_log!(
        etw::INFO,
        etw::APO_CFG,
        "UnlockForProcess completed instance={object:p} process_calls={} processed_frames={}",
        object.process_calls.load(Ordering::Relaxed),
        object.processed_frames.load(Ordering::Relaxed)
    );
    S_OK
}

// Real-time boundary: no logging, allocation, waits, COM calls, or syscalls.
pub unsafe extern "system" fn process(
    this: *mut c_void,
    input_count: u32,
    inputs: *const *const APO_CONNECTION_PROPERTY,
    output_count: u32,
    outputs: *mut *mut APO_CONNECTION_PROPERTY,
) {
    let object = unsafe { &*object_from(this) };
    object.process_calls.fetch_add(1, Ordering::Relaxed);
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
    let Some(mut session) = object.processing.try_process() else {
        return;
    };
    let Some(bytes) = session.buffer_bytes(input.u32ValidFrameCount) else {
        return;
    };
    if output.pBuffer == 0 {
        return;
    }
    // LockForProcess validated capacity; the session bounded the frame count.
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
        let silence = session.silence_byte();
        unsafe { ptr::write_bytes(destination, silence, bytes) };
    } else {
        return;
    }
    let apo_config = object.config.load();
    let reset = object.reset_requested.swap(false, Ordering::Relaxed);
    let buffer = unsafe { core::slice::from_raw_parts_mut(destination, bytes) };
    let effects_processed = session.process(buffer, &apo_config, reset);
    let mut output_flags = input.u32BufferFlags;
    if effects_processed {
        // The DSP materializes every frame, including startup padding and mute.
        // Keep these packets valid even when all samples are zero: legacy capture
        // clients can stop recording when initial packets are BUFFER_SILENT.
        // Silent input can also yield audio while the model's buffers drain.
        output_flags = BUFFER_VALID;
    }
    output.u32ValidFrameCount = input.u32ValidFrameCount;
    output.u32BufferFlags = output_flags;
    object
        .processed_frames
        .fetch_add(input.u32ValidFrameCount as u64, Ordering::Relaxed);
}

pub unsafe extern "system" fn identity_frames(_: *mut c_void, frames: u32) -> u32 {
    frames
}

use super::{ApoCom, object_from, unknown_vtable};
use crate::apo::{
    dsp::{MAX_CHANNELS, MAX_SAMPLE_RATE, MIN_SAMPLE_RATE, SampleFormat},
    etw::{self, apo_log},
};
use core::ffi::c_void;
use std::sync::atomic::Ordering;
use windows::Win32::{
    Foundation::{E_POINTER, S_FALSE, S_OK},
    Media::{
        Audio::Apo::{
            APOERR_FORMAT_NOT_SUPPORTED, APOERR_NOT_INITIALIZED, IAudioMediaType,
            UNCOMPRESSEDAUDIOFORMAT,
        },
        KernelStreaming::KSDATAFORMAT_SUBTYPE_PCM,
        Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    },
};
use windows_core::{HRESULT, Interface};

unsafe fn negotiate_format(
    object: *mut ApoCom,
    opposite: *mut c_void,
    requested: *mut c_void,
    supported: *mut *mut c_void,
) -> HRESULT {
    if requested.is_null() || supported.is_null() {
        return E_POINTER;
    }
    let requested_format = match unsafe { read_format(requested) } {
        Ok(format) if is_supported_format(&format) => format,
        _ => return APOERR_FORMAT_NOT_SUPPORTED,
    };
    // This APO processes in place and cannot convert between formats. If the
    // other end is already fixed, suggest that format instead of accepting a
    // pair which LockForProcess would later reject.
    let (selected, format, result) = if opposite.is_null() {
        (requested, requested_format, S_OK)
    } else {
        let opposite_format = match unsafe { read_format(opposite) } {
            Ok(format) if is_supported_format(&format) => format,
            _ => return APOERR_FORMAT_NOT_SUPPORTED,
        };
        if formats_match(&requested_format, &opposite_format) {
            (requested, requested_format, S_OK)
        } else {
            (opposite, opposite_format, S_FALSE)
        }
    };
    // AddRef may invoke an external COM object, so do not hold our mutex there.
    unsafe { ((*unknown_vtable(selected)).AddRef)(selected) };
    {
        let _control = unsafe { (*object).control_guard.lock() }
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !unsafe { (*object).locked.load(Ordering::Acquire) } {
            unsafe {
                (*object)
                    .input_channels
                    .store(format.dwSamplesPerFrame, Ordering::Release)
            };
        }
    }
    unsafe {
        *supported = selected;
    }
    result
}

pub(super) unsafe fn read_format(raw: *mut c_void) -> Result<UNCOMPRESSEDAUDIOFORMAT, HRESULT> {
    let Some(media_type) = (unsafe { IAudioMediaType::from_raw_borrowed(&raw) }) else {
        return Err(E_POINTER);
    };
    let mut format = UNCOMPRESSEDAUDIOFORMAT::default();
    unsafe { media_type.GetUncompressedAudioFormat(&mut format) }
        .map(|()| format)
        .map_err(|error| error.code())
}

pub(super) fn is_supported_format(format: &UNCOMPRESSEDAUDIOFORMAT) -> bool {
    (1..=MAX_CHANNELS as u32).contains(&format.dwSamplesPerFrame)
        && sample_format(format).is_some()
        && format.fFramesPerSecond.is_finite()
        && (MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&f64::from(format.fFramesPerSecond))
        && frame_bytes(format).is_some()
}

pub(super) fn formats_match(
    left: &UNCOMPRESSEDAUDIOFORMAT,
    right: &UNCOMPRESSEDAUDIOFORMAT,
) -> bool {
    left.guidFormatType == right.guidFormatType
        && left.dwSamplesPerFrame == right.dwSamplesPerFrame
        && left.dwBytesPerSampleContainer == right.dwBytesPerSampleContainer
        && left.dwValidBitsPerSample == right.dwValidBitsPerSample
        && left.fFramesPerSecond == right.fFramesPerSecond
        && left.dwChannelMask == right.dwChannelMask
}

pub(super) fn frame_bytes(format: &UNCOMPRESSEDAUDIOFORMAT) -> Option<usize> {
    (format.dwSamplesPerFrame as usize).checked_mul(format.dwBytesPerSampleContainer as usize)
}

pub(super) fn sample_format(format: &UNCOMPRESSEDAUDIOFORMAT) -> Option<SampleFormat> {
    if format.guidFormatType == KSDATAFORMAT_SUBTYPE_PCM {
        SampleFormat::pcm(
            format.dwBytesPerSampleContainer,
            format.dwValidBitsPerSample,
        )
    } else if format.guidFormatType == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
        match (
            format.dwBytesPerSampleContainer,
            format.dwValidBitsPerSample,
        ) {
            (4, 32) => Some(SampleFormat::Float32),
            (8, 64) => Some(SampleFormat::Float64),
            _ => None,
        }
    } else {
        None
    }
}

pub(super) fn log_connection_format(label: &str, format: &UNCOMPRESSEDAUDIOFORMAT) {
    apo_log!(
        etw::INFO,
        etw::FORMAT,
        "{label} format_type={:?} channels={} container_bytes={} valid_bits={} sample_rate={} channel_mask=0x{:08x} supported={}",
        format.guidFormatType,
        format.dwSamplesPerFrame,
        format.dwBytesPerSampleContainer,
        format.dwValidBitsPerSample,
        format.fFramesPerSecond,
        format.dwChannelMask,
        is_supported_format(format)
    );
}

unsafe fn log_media_format(label: &str, raw: *mut c_void) {
    if raw.is_null() {
        apo_log!(etw::VERBOSE, etw::FORMAT, "{label} unspecified");
    } else if let Ok(format) = unsafe { read_format(raw) } {
        log_connection_format(label, &format);
    }
}

pub(super) unsafe extern "system" fn is_input_format_supported(
    this: *mut c_void,
    opposite: *mut c_void,
    requested: *mut c_void,
    supported: *mut *mut c_void,
) -> HRESULT {
    let object = unsafe { object_from(this) };
    unsafe {
        log_media_format("IsInputFormatSupported requested", requested);
        log_media_format("IsInputFormatSupported opposite", opposite);
    }
    let result = unsafe { negotiate_format(object, opposite, requested, supported) };
    apo_log!(
        etw::VERBOSE,
        etw::FORMAT,
        "IsInputFormatSupported instance={object:p} result=0x{:08x}",
        result.0 as u32
    );
    result
}

pub(super) unsafe extern "system" fn is_output_format_supported(
    this: *mut c_void,
    opposite: *mut c_void,
    requested: *mut c_void,
    supported: *mut *mut c_void,
) -> HRESULT {
    let object = unsafe { object_from(this) };
    // The audio engine is allowed to negotiate only the output side before it
    // asks GetInputChannelCount. Registration requires samples-per-frame to
    // match, so the accepted output format also gives us the input channel
    // count in that call order.
    unsafe {
        log_media_format("IsOutputFormatSupported requested", requested);
        log_media_format("IsOutputFormatSupported opposite", opposite);
    }
    let result = unsafe { negotiate_format(object, opposite, requested, supported) };
    apo_log!(
        etw::VERBOSE,
        etw::FORMAT,
        "IsOutputFormatSupported instance={object:p} result=0x{:08x}",
        result.0 as u32
    );
    result
}

pub(super) unsafe extern "system" fn get_input_channel_count(
    this: *mut c_void,
    count: *mut u32,
) -> HRESULT {
    if count.is_null() {
        apo_log!(
            etw::ERROR,
            etw::FORMAT,
            "GetInputChannelCount rejected null output pointer"
        );
        return E_POINTER;
    }
    let object = unsafe { object_from(this) };
    if !unsafe { (*object).initialized.load(Ordering::Acquire) } {
        apo_log!(
            etw::ERROR,
            etw::FORMAT,
            "GetInputChannelCount failed instance={object:p} reason=not_initialized result=0x{:08x}",
            APOERR_NOT_INITIALIZED.0 as u32
        );
        return APOERR_NOT_INITIALIZED;
    }
    let channels = unsafe { (*object).input_channels.load(Ordering::Acquire) };
    if channels == 0 {
        apo_log!(
            etw::ERROR,
            etw::FORMAT,
            "GetInputChannelCount failed instance={object:p} reason=format_not_negotiated result=0x{:08x}",
            APOERR_NOT_INITIALIZED.0 as u32
        );
        return APOERR_NOT_INITIALIZED;
    }
    unsafe { *count = channels };
    apo_log!(
        etw::VERBOSE,
        etw::FORMAT,
        "GetInputChannelCount completed instance={object:p} channels={channels}"
    );
    S_OK
}

use core::ffi::c_void;

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

use voice_box_shared::dsp::{MAX_CHANNELS, MAX_SAMPLE_RATE, MIN_SAMPLE_RATE, SampleFormat};

use crate::{
    com::{ApoCom, object_from, unknown_vtable},
    etw::{self, apo_log},
};

pub unsafe fn read_format(raw: *mut c_void) -> Result<UNCOMPRESSEDAUDIOFORMAT, HRESULT> {
    let Some(media_type) = (unsafe { IAudioMediaType::from_raw_borrowed(&raw) }) else {
        return Err(E_POINTER);
    };
    let mut format = UNCOMPRESSEDAUDIOFORMAT::default();
    unsafe { media_type.GetUncompressedAudioFormat(&mut format) }
        .map(|()| format)
        .map_err(|error| error.code())
}

pub fn is_supported_format(format: &UNCOMPRESSEDAUDIOFORMAT) -> bool {
    (1..=MAX_CHANNELS as u32).contains(&format.dwSamplesPerFrame)
        && sample_format(format).is_some()
        && format.fFramesPerSecond.is_finite()
        && (MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&f64::from(format.fFramesPerSecond))
}

pub fn formats_match(left: &UNCOMPRESSEDAUDIOFORMAT, right: &UNCOMPRESSEDAUDIOFORMAT) -> bool {
    left.guidFormatType == right.guidFormatType
        && left.dwSamplesPerFrame == right.dwSamplesPerFrame
        && left.dwBytesPerSampleContainer == right.dwBytesPerSampleContainer
        && left.dwValidBitsPerSample == right.dwValidBitsPerSample
        && left.fFramesPerSecond == right.fFramesPerSecond
        && left.dwChannelMask == right.dwChannelMask
}

pub fn sample_format(format: &UNCOMPRESSEDAUDIOFORMAT) -> Option<SampleFormat> {
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

pub fn log_connection_format(label: &str, format: &UNCOMPRESSEDAUDIOFORMAT) {
    apo_log!(
        etw::level::INFO,
        etw::keyword::FORMAT,
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

unsafe fn negotiate_format(
    object: &ApoCom,
    opposite: *mut c_void,
    requested: *mut c_void,
    supported: *mut *mut c_void,
) -> HRESULT {
    if requested.is_null() || supported.is_null() {
        return E_POINTER;
    }
    // Media type methods and AddRef may reenter this APO; keep them outside our mutex.
    let requested_format = match unsafe { read_format(requested) } {
        Ok(format) => format,
        Err(result) => return result,
    };
    log_connection_format("Format negotiation requested", &requested_format);
    let opposite_format = if opposite.is_null() {
        None
    } else {
        let format = match unsafe { read_format(opposite) } {
            Ok(format) => format,
            Err(result) => return result,
        };
        log_connection_format("Format negotiation opposite", &format);
        Some(format)
    };
    // This APO processes in place and cannot convert between formats. If the
    // other end is already fixed, suggest that format instead of accepting a
    // pair which LockForProcess would later reject.
    let (selected, format, result) = match opposite_format {
        Some(format) if !formats_match(&requested_format, &format) => (opposite, format, S_FALSE),
        _ => (requested, requested_format, S_OK),
    };
    if !is_supported_format(&format) {
        return APOERR_FORMAT_NOT_SUPPORTED;
    }
    unsafe { ((*unknown_vtable(selected)).AddRef)(selected) };
    {
        let mut control = object.control();
        // Either side may be negotiated first. Matching input/output formats
        // give the input channel count in both call orders.
        if !object.processing.is_accepting() {
            control.input_channels = format.dwSamplesPerFrame;
        }
    }
    unsafe { *supported = selected };
    result
}

pub unsafe extern "system" fn is_format_supported(
    this: *mut c_void,
    opposite: *mut c_void,
    requested: *mut c_void,
    supported: *mut *mut c_void,
) -> HRESULT {
    let object = unsafe { &*object_from(this) };
    let result = unsafe { negotiate_format(object, opposite, requested, supported) };
    apo_log!(
        etw::level::VERBOSE,
        etw::keyword::FORMAT,
        "IsFormatSupported instance={object:p} result=0x{:08x}",
        result.0 as u32
    );
    result
}

pub unsafe extern "system" fn get_input_channel_count(
    this: *mut c_void,
    count: *mut u32,
) -> HRESULT {
    if count.is_null() {
        apo_log!(
            etw::level::ERROR,
            etw::keyword::FORMAT,
            "GetInputChannelCount rejected null output pointer"
        );
        return E_POINTER;
    }
    let object = unsafe { &*object_from(this) };
    let control = object.control();
    if !control.initialized {
        apo_log!(
            etw::level::ERROR,
            etw::keyword::FORMAT,
            "GetInputChannelCount failed instance={object:p} reason=not_initialized result=0x{:08x}",
            APOERR_NOT_INITIALIZED.0 as u32
        );
        return APOERR_NOT_INITIALIZED;
    }
    let channels = control.input_channels;
    if channels == 0 {
        apo_log!(
            etw::level::ERROR,
            etw::keyword::FORMAT,
            "GetInputChannelCount failed instance={object:p} reason=format_not_negotiated result=0x{:08x}",
            APOERR_NOT_INITIALIZED.0 as u32
        );
        return APOERR_NOT_INITIALIZED;
    }
    unsafe { *count = channels };
    apo_log!(
        etw::level::VERBOSE,
        etw::keyword::FORMAT,
        "GetInputChannelCount completed instance={object:p} channels={channels}"
    );
    S_OK
}

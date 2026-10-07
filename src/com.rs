//! Windows APO COM bridge. The DSP implementation has no dependency on this module.

use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::ptr;
use std::boxed::Box;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering, fence};

use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_INVALIDARG, E_NOINTERFACE, E_NOTIMPL,
    E_OUTOFMEMORY, E_POINTER, HANDLE, S_FALSE, S_OK,
};
use windows::Win32::Media::Audio::Apo::{
    APO_CONNECTION_DESCRIPTOR, APO_CONNECTION_PROPERTY, APO_FLAG,
    APO_FLAG_BITSPERSAMPLE_MUST_MATCH, APO_FLAG_FRAMESPERSECOND_MUST_MATCH, APO_FLAG_INPLACE,
    APO_FLAG_SAMPLESPERFRAME_MUST_MATCH, APO_REG_PROPERTIES, APOERR_ALREADY_INITIALIZED,
    APOERR_ALREADY_UNLOCKED, APOERR_APO_LOCKED, APOERR_FORMAT_NOT_SUPPORTED,
    APOERR_INVALID_APO_CLSID, APOERR_INVALID_CONNECTION_FORMAT,
    APOERR_INVALID_OUTPUT_MAXFRAMECOUNT, APOERR_NOT_INITIALIZED, APOERR_NUM_CONNECTIONS_INVALID,
    APOInitBaseStruct, APOInitSystemEffects, APOInitSystemEffects2, APOInitSystemEffects3,
    AUDIO_SYSTEMEFFECT, AUDIO_SYSTEMEFFECT_STATE_ON, BUFFER_INVALID, BUFFER_SILENT, BUFFER_VALID,
    IAudioMediaType, IAudioProcessingObject, IAudioProcessingObject_Vtbl,
    IAudioProcessingObjectConfiguration, IAudioProcessingObjectConfiguration_Vtbl,
    IAudioProcessingObjectRT, IAudioProcessingObjectRT_Vtbl, IAudioSystemEffects,
    IAudioSystemEffects2, IAudioSystemEffects2_Vtbl, IAudioSystemEffects3,
    IAudioSystemEffects3_Vtbl, UNCOMPRESSEDAUDIOFORMAT,
};
use windows::Win32::Media::KernelStreaming::{
    AUDIO_EFFECT_TYPE_NOISE_SUPPRESSION, AUDIO_SIGNALPROCESSINGMODE_COMMUNICATIONS,
    AUDIO_SIGNALPROCESSINGMODE_DEFAULT, AUDIO_SIGNALPROCESSINGMODE_FAR_FIELD_SPEECH,
    AUDIO_SIGNALPROCESSINGMODE_MEDIA, AUDIO_SIGNALPROCESSINGMODE_MOVIE,
    AUDIO_SIGNALPROCESSINGMODE_NOTIFICATION, AUDIO_SIGNALPROCESSINGMODE_RAW,
    AUDIO_SIGNALPROCESSINGMODE_SPEECH, KSDATAFORMAT_SUBTYPE_PCM,
};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{CoTaskMemAlloc, IClassFactory, IClassFactory_Vtbl};
use windows_core::{BOOL, GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface};

use crate::dsp::{
    AudioProcessor, MAX_CHANNELS, MAX_SAMPLE_RATE, MIN_SAMPLE_RATE, ModelKind, SampleFormat,
};
use crate::etw::{self, apo_log};

const CLSID_VOICE_BOX_APO: GUID = GUID::from_u128(0xae672075_b244_4417_b5a5_fbd45b7f0268);
const EFFECT_ID_DENOISE: GUID = AUDIO_EFFECT_TYPE_NOISE_SUPPRESSION;

static LIVE_OBJECTS: AtomicU32 = AtomicU32::new(0);
static SERVER_LOCKS: AtomicU32 = AtomicU32::new(0);
static FACTORY_REFS: AtomicU32 = AtomicU32::new(1);

#[repr(C)]
struct InterfaceHeader<V> {
    vtable: *const V,
    object: *mut ApoCom,
}

/// A single carrier supports normal activation and COM aggregation.
#[repr(C)]
struct ApoCom {
    params: Arc<crate::config::RtParams>,
    config_worker: Option<crate::config::ConfigWorker>,
    inner_unknown: InterfaceHeader<IUnknown_Vtbl>,
    apo: InterfaceHeader<IAudioProcessingObject_Vtbl>,
    configuration: InterfaceHeader<IAudioProcessingObjectConfiguration_Vtbl>,
    realtime: InterfaceHeader<IAudioProcessingObjectRT_Vtbl>,
    system_effects: InterfaceHeader<IAudioSystemEffects3_Vtbl>,
    controlling_unknown: *mut c_void,
    inner_refs: AtomicU32,
    initialized: AtomicBool,
    locked: AtomicBool,
    configuration_guard: Mutex<()>,
    dsp_in_use: AtomicBool,
    reset_requested: AtomicBool,
    effect_enabled: AtomicBool,
    discovery_only: AtomicBool,
    process_calls: AtomicU64,
    processed_frames: AtomicU64,
    input_channels: AtomicU32,
    max_input_frames: AtomicU32,
    max_output_frames: AtomicU32,
    latency_hns: AtomicI64,
    dsp: UnsafeCell<AudioProcessor>,
}

// Non-real-time configuration is serialized. Access to UnsafeCell is exclusive
// through DspAccess, which never waits on the processing thread. Reset only posts
// a request that the next processing pass consumes while it owns the DSP.
unsafe impl Sync for ApoCom {}

struct DspAccess<'a>(&'a AtomicBool);

impl<'a> DspAccess<'a> {
    fn try_acquire(busy: &'a AtomicBool) -> Option<Self> {
        busy.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Self(busy))
    }
}

impl Drop for DspAccess<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

unsafe fn object_from(this: *mut c_void) -> *mut ApoCom {
    unsafe { (*(this as *mut InterfaceHeader<IUnknown_Vtbl>)).object }
}

unsafe fn unknown_vtable(this: *mut c_void) -> *const IUnknown_Vtbl {
    unsafe { *(this as *mut *const IUnknown_Vtbl) }
}

unsafe extern "system" fn delegating_query_interface(
    this: *mut c_void,
    iid: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    if iid.is_null() || output.is_null() {
        apo_log!(
            etw::ERROR,
            etw::LIFECYCLE,
            "Delegating QueryInterface rejected null argument iid={iid:p} output={output:p}"
        );
        return E_POINTER;
    }
    let object = unsafe { object_from(this) };
    let outer = unsafe { (*object).controlling_unknown };
    let requested = unsafe { *iid };
    let result = unsafe { ((*unknown_vtable(outer)).QueryInterface)(outer, iid, output) };
    apo_log!(
        if result == S_OK {
            etw::VERBOSE
        } else {
            etw::WARNING
        },
        etw::LIFECYCLE,
        "Delegating QueryInterface instance={object:p} iid={requested:?} result=0x{:08x}",
        result.0 as u32
    );
    result
}

unsafe extern "system" fn delegating_add_ref(this: *mut c_void) -> u32 {
    let object = unsafe { object_from(this) };
    let outer = unsafe { (*object).controlling_unknown };
    unsafe { ((*unknown_vtable(outer)).AddRef)(outer) }
}

unsafe extern "system" fn delegating_release(this: *mut c_void) -> u32 {
    let object = unsafe { object_from(this) };
    let outer = unsafe { (*object).controlling_unknown };
    unsafe { ((*unknown_vtable(outer)).Release)(outer) }
}

unsafe extern "system" fn inner_query_interface(
    this: *mut c_void,
    iid: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    if iid.is_null() || output.is_null() {
        apo_log!(
            etw::ERROR,
            etw::LIFECYCLE,
            "Inner QueryInterface rejected null argument iid={iid:p} output={output:p}"
        );
        return E_POINTER;
    }
    unsafe { *output = ptr::null_mut() };
    let object = unsafe { object_from(this) };
    let iid = unsafe { *iid };

    let (interface, inner_identity, name) = if iid == IUnknown::IID {
        (
            unsafe { ptr::addr_of_mut!((*object).inner_unknown).cast::<c_void>() },
            true,
            "IUnknown",
        )
    } else if iid == IAudioProcessingObject::IID {
        (
            unsafe { ptr::addr_of_mut!((*object).apo).cast::<c_void>() },
            false,
            "IAudioProcessingObject",
        )
    } else if iid == IAudioProcessingObjectConfiguration::IID {
        (
            unsafe { ptr::addr_of_mut!((*object).configuration).cast::<c_void>() },
            false,
            "IAudioProcessingObjectConfiguration",
        )
    } else if iid == IAudioProcessingObjectRT::IID {
        (
            unsafe { ptr::addr_of_mut!((*object).realtime).cast::<c_void>() },
            false,
            "IAudioProcessingObjectRT",
        )
    } else if iid == IAudioSystemEffects::IID
        || iid == IAudioSystemEffects2::IID
        || iid == IAudioSystemEffects3::IID
    {
        let name = if iid == IAudioSystemEffects3::IID {
            "IAudioSystemEffects3"
        } else if iid == IAudioSystemEffects2::IID {
            "IAudioSystemEffects2"
        } else {
            "IAudioSystemEffects"
        };
        (
            unsafe { ptr::addr_of_mut!((*object).system_effects).cast::<c_void>() },
            false,
            name,
        )
    } else {
        apo_log!(
            etw::WARNING,
            etw::LIFECYCLE,
            "Inner QueryInterface instance={object:p} iid={iid:?} result=0x{:08x}",
            E_NOINTERFACE.0 as u32
        );
        return E_NOINTERFACE;
    };

    unsafe {
        *output = interface;
        if inner_identity {
            inner_add_ref(this);
        } else {
            delegating_add_ref(interface);
        }
    }
    apo_log!(
        etw::VERBOSE,
        etw::LIFECYCLE,
        "Inner QueryInterface instance={object:p} interface={name} iid={iid:?} result=0x{:08x}",
        S_OK.0 as u32
    );
    S_OK
}

unsafe extern "system" fn inner_add_ref(this: *mut c_void) -> u32 {
    let object = unsafe { object_from(this) };
    unsafe { (*object).inner_refs.fetch_add(1, Ordering::Relaxed) + 1 }
}

unsafe extern "system" fn inner_release(this: *mut c_void) -> u32 {
    let object = unsafe { object_from(this) };
    let refs = unsafe { (*object).inner_refs.fetch_sub(1, Ordering::Release) - 1 };
    if refs == 0 {
        fence(Ordering::Acquire);
        let live_objects = LIVE_OBJECTS.load(Ordering::Acquire);
        apo_log!(
            etw::INFO,
            etw::LIFECYCLE,
            "Destroy instance={object:p} initialized={} locked={} process_calls={} processed_frames={} live_objects={live_objects}",
            unsafe { (*object).initialized.load(Ordering::Relaxed) },
            unsafe { (*object).locked.load(Ordering::Relaxed) },
            unsafe { (*object).process_calls.load(Ordering::Relaxed) },
            unsafe { (*object).processed_frames.load(Ordering::Relaxed) }
        );
        unsafe { drop(Box::from_raw(object)) };
        // The object's worker has stopped before DLL unloading becomes possible.
        LIVE_OBJECTS.fetch_sub(1, Ordering::AcqRel);
    }
    refs
}

const DELEGATING_UNKNOWN: IUnknown_Vtbl = IUnknown_Vtbl {
    QueryInterface: delegating_query_interface,
    AddRef: delegating_add_ref,
    Release: delegating_release,
};

static INNER_UNKNOWN: IUnknown_Vtbl = IUnknown_Vtbl {
    QueryInterface: inner_query_interface,
    AddRef: inner_add_ref,
    Release: inner_release,
};

unsafe extern "system" fn reset(this: *mut c_void) -> HRESULT {
    let object = unsafe { object_from(this) };
    unsafe { (*object).reset_requested.store(true, Ordering::Release) };
    S_OK
}

unsafe extern "system" fn get_latency(this: *mut c_void, latency: *mut i64) -> HRESULT {
    if latency.is_null() {
        return E_POINTER;
    }
    let object = unsafe { object_from(this) };
    unsafe { *latency = (*object).latency_hns.load(Ordering::Acquire) };
    S_OK
}

unsafe extern "system" fn get_registration_properties(
    _: *mut c_void,
    output: *mut *mut APO_REG_PROPERTIES,
) -> HRESULT {
    if output.is_null() {
        return E_POINTER;
    }
    unsafe { *output = ptr::null_mut() };
    let memory = unsafe { CoTaskMemAlloc(core::mem::size_of::<APO_REG_PROPERTIES>()) }
        .cast::<APO_REG_PROPERTIES>();
    if memory.is_null() {
        return E_OUTOFMEMORY;
    }

    let mut properties = APO_REG_PROPERTIES {
        clsid: CLSID_VOICE_BOX_APO,
        Flags: APO_FLAG(
            APO_FLAG_INPLACE.0
                | APO_FLAG_SAMPLESPERFRAME_MUST_MATCH.0
                | APO_FLAG_FRAMESPERSECOND_MUST_MATCH.0
                | APO_FLAG_BITSPERSAMPLE_MUST_MATCH.0,
        ),
        u32MajorVersion: 1,
        u32MinInputConnections: 1,
        u32MaxInputConnections: 1,
        u32MinOutputConnections: 1,
        u32MaxOutputConnections: 1,
        u32MaxInstances: u32::MAX,
        u32NumAPOInterfaces: 1,
        iidAPOInterfaceList: [IAudioProcessingObject::IID],
        ..Default::default()
    };
    write_wide(&mut properties.szFriendlyName, "VoiceBox APO");
    write_wide(&mut properties.szCopyrightInfo, "Copyright (C) 2026 Tanix");

    unsafe {
        memory.write(properties);
        *output = memory;
    }
    S_OK
}

unsafe extern "system" fn initialize(this: *mut c_void, size: u32, data: *const u8) -> HRESULT {
    let object = unsafe { object_from(this) };
    let _configuration = unsafe { (*object).configuration_guard.lock() }
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if unsafe { (*object).initialized.load(Ordering::Acquire) } {
        return APOERR_ALREADY_INITIALIZED;
    }
    if data.is_null() || (size as usize) < core::mem::size_of::<APOInitBaseStruct>() {
        return E_INVALIDARG;
    }
    if unsafe { ptr::read_unaligned(data.cast::<APOInitBaseStruct>()) }.clsid != CLSID_VOICE_BOX_APO
    {
        return APOERR_INVALID_APO_CLSID;
    }

    let (version, mode, discovery_only) =
        if size as usize == core::mem::size_of::<APOInitSystemEffects3>() {
            let init = unsafe { ptr::read_unaligned(data.cast::<APOInitSystemEffects3>()) };
            (
                3,
                init.AudioProcessingMode,
                init.InitializeForDiscoveryOnly.as_bool(),
            )
        } else if size as usize == core::mem::size_of::<APOInitSystemEffects2>() {
            let init = unsafe { ptr::read_unaligned(data.cast::<APOInitSystemEffects2>()) };
            (
                2,
                init.AudioProcessingMode,
                init.InitializeForDiscoveryOnly.as_bool(),
            )
        } else if size as usize == core::mem::size_of::<APOInitSystemEffects>() {
            (1, AUDIO_SIGNALPROCESSINGMODE_DEFAULT, false)
        } else {
            apo_log!(
                etw::ERROR,
                etw::LIFECYCLE,
                "Initialize rejected instance={object:p} unsupported_data_size={size}"
            );
            return E_INVALIDARG;
        };

    unsafe {
        if !discovery_only && mode != AUDIO_SIGNALPROCESSINGMODE_RAW {
            match crate::config::ConfigWorker::start(Arc::clone(&(*object).params)) {
                Ok(worker) => (*object).config_worker = worker,
                Err(error) => {
                    apo_log!(
                        etw::WARNING,
                        etw::CONFIGURATION,
                        "Configuration watcher failed: {error}"
                    );
                }
            }
        }
        (*object)
            .effect_enabled
            .store(mode != AUDIO_SIGNALPROCESSINGMODE_RAW, Ordering::Release);
        (*object)
            .discovery_only
            .store(discovery_only, Ordering::Release);
        (*object).initialized.store(true, Ordering::Release);
    }
    apo_log!(
        etw::INFO,
        etw::LIFECYCLE,
        "Initialize completed instance={object:p} system_effects_version={version} data_size={size} mode={} mode_guid={mode:?} discovery_only={discovery_only}",
        processing_mode_name(&mode)
    );
    S_OK
}

fn processing_mode_name(mode: &GUID) -> &'static str {
    if *mode == AUDIO_SIGNALPROCESSINGMODE_DEFAULT {
        "default"
    } else if *mode == AUDIO_SIGNALPROCESSINGMODE_COMMUNICATIONS {
        "communications"
    } else if *mode == AUDIO_SIGNALPROCESSINGMODE_SPEECH {
        "speech"
    } else if *mode == AUDIO_SIGNALPROCESSINGMODE_RAW {
        "raw"
    } else if *mode == AUDIO_SIGNALPROCESSINGMODE_MEDIA {
        "media"
    } else if *mode == AUDIO_SIGNALPROCESSINGMODE_MOVIE {
        "movie"
    } else if *mode == AUDIO_SIGNALPROCESSINGMODE_NOTIFICATION {
        "notification"
    } else if *mode == AUDIO_SIGNALPROCESSINGMODE_FAR_FIELD_SPEECH {
        "far_field_speech"
    } else {
        "unknown"
    }
}

unsafe extern "system" fn get_effects_list(
    this: *mut c_void,
    effects: *mut *mut GUID,
    count: *mut u32,
    _: HANDLE,
) -> HRESULT {
    if effects.is_null() || count.is_null() {
        return E_POINTER;
    }
    unsafe {
        *effects = ptr::null_mut();
        *count = 0;
    }
    let object = unsafe { object_from(this) };
    if !unsafe { (*object).effect_enabled.load(Ordering::Acquire) } {
        return S_OK;
    }
    let memory = unsafe { CoTaskMemAlloc(core::mem::size_of::<GUID>()) }.cast::<GUID>();
    if memory.is_null() {
        return E_OUTOFMEMORY;
    }
    unsafe {
        memory.write(EFFECT_ID_DENOISE);
        *effects = memory;
        *count = 1;
    }
    apo_log!(
        etw::INFO,
        etw::LIFECYCLE,
        "GetEffectsList instance={object:p} count=1 effect={EFFECT_ID_DENOISE:?}"
    );
    S_OK
}

unsafe extern "system" fn get_controllable_effects_list(
    this: *mut c_void,
    effects: *mut *mut AUDIO_SYSTEMEFFECT,
    count: *mut u32,
    _: HANDLE,
) -> HRESULT {
    if effects.is_null() || count.is_null() {
        return E_POINTER;
    }
    unsafe {
        *effects = ptr::null_mut();
        *count = 0;
    }
    let object = unsafe { object_from(this) };
    if !unsafe { (*object).effect_enabled.load(Ordering::Acquire) } {
        return S_OK;
    }
    let memory = unsafe { CoTaskMemAlloc(core::mem::size_of::<AUDIO_SYSTEMEFFECT>()) }
        .cast::<AUDIO_SYSTEMEFFECT>();
    if memory.is_null() {
        return E_OUTOFMEMORY;
    }
    unsafe {
        memory.write(AUDIO_SYSTEMEFFECT {
            id: EFFECT_ID_DENOISE,
            canSetState: BOOL::from(false),
            state: AUDIO_SYSTEMEFFECT_STATE_ON,
        });
        *effects = memory;
        *count = 1;
    }
    apo_log!(
        etw::INFO,
        etw::LIFECYCLE,
        "GetControllableSystemEffectsList instance={object:p} count=1 effect={EFFECT_ID_DENOISE:?} state=on controllable=false"
    );
    S_OK
}

unsafe extern "system" fn set_effect_state(
    this: *mut c_void,
    effect: GUID,
    state: windows::Win32::Media::Audio::Apo::AUDIO_SYSTEMEFFECT_STATE,
) -> HRESULT {
    let object = unsafe { object_from(this) };
    apo_log!(
        etw::WARNING,
        etw::LIFECYCLE,
        "SetAudioSystemEffectState rejected instance={object:p} effect={effect:?} state={} result=0x{:08x}",
        state.0,
        E_NOTIMPL.0 as u32
    );
    E_NOTIMPL
}

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
        let _configuration = unsafe { (*object).configuration_guard.lock() }
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

unsafe fn read_format(raw: *mut c_void) -> Result<UNCOMPRESSEDAUDIOFORMAT, HRESULT> {
    let Some(media_type) = (unsafe { IAudioMediaType::from_raw_borrowed(&raw) }) else {
        return Err(E_POINTER);
    };
    let mut format = UNCOMPRESSEDAUDIOFORMAT::default();
    unsafe { media_type.GetUncompressedAudioFormat(&mut format) }
        .map(|()| format)
        .map_err(|error| error.code())
}

fn is_supported_format(format: &UNCOMPRESSEDAUDIOFORMAT) -> bool {
    (1..=MAX_CHANNELS as u32).contains(&format.dwSamplesPerFrame)
        && sample_format(format).is_some()
        && format.fFramesPerSecond.is_finite()
        && (MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&f64::from(format.fFramesPerSecond))
        && frame_bytes(format).is_some()
}

fn formats_match(left: &UNCOMPRESSEDAUDIOFORMAT, right: &UNCOMPRESSEDAUDIOFORMAT) -> bool {
    left.guidFormatType == right.guidFormatType
        && left.dwSamplesPerFrame == right.dwSamplesPerFrame
        && left.dwBytesPerSampleContainer == right.dwBytesPerSampleContainer
        && left.dwValidBitsPerSample == right.dwValidBitsPerSample
        && left.fFramesPerSecond == right.fFramesPerSecond
        && left.dwChannelMask == right.dwChannelMask
}

fn frame_bytes(format: &UNCOMPRESSEDAUDIOFORMAT) -> Option<usize> {
    (format.dwSamplesPerFrame as usize).checked_mul(format.dwBytesPerSampleContainer as usize)
}

fn sample_format(format: &UNCOMPRESSEDAUDIOFORMAT) -> Option<SampleFormat> {
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

fn log_lock_failure(
    object: *mut ApoCom,
    input_count: u32,
    output_count: u32,
    reason: &str,
    result: HRESULT,
) -> HRESULT {
    apo_log!(
        etw::ERROR,
        etw::CONFIGURATION,
        "LockForProcess failed instance={object:p} inputs={input_count} outputs={output_count} reason={reason} result=0x{:08x}",
        result.0 as u32
    );
    result
}

fn log_connection_format(label: &str, format: &UNCOMPRESSEDAUDIOFORMAT) {
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

unsafe extern "system" fn is_input_format_supported(
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

unsafe extern "system" fn is_output_format_supported(
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

unsafe extern "system" fn get_input_channel_count(this: *mut c_void, count: *mut u32) -> HRESULT {
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

unsafe extern "system" fn lock_for_process(
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
    let _configuration = unsafe { (*object).configuration_guard.lock() }
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        (&mut *(*object).dsp.get()).configure(
            sample_format,
            input_format.dwSamplesPerFrame as usize,
            f64::from(input_format.fFramesPerSecond),
            (*object)
                .effect_enabled
                .load(Ordering::Acquire)
                .then_some(ModelKind::default()),
        )
    }
    .is_err()
    {
        return log_lock_failure(
            object,
            input_count,
            output_count,
            "cannot configure speech enhancement model",
            APOERR_INVALID_CONNECTION_FORMAT,
        );
    }
    unsafe {
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
        etw::CONFIGURATION,
        "LockForProcess completed instance={object:p} input_max_frames={} output_max_frames={} channels={} frame_bytes={frame_bytes} effect=gtcrn latency_hns={}",
        unsafe { (**inputs).u32MaxFrameCount },
        unsafe { (**outputs).u32MaxFrameCount },
        input_format.dwSamplesPerFrame,
        unsafe { (*object).latency_hns.load(Ordering::Acquire) },
    );
    S_OK
}

unsafe extern "system" fn unlock_for_process(this: *mut c_void) -> HRESULT {
    let object = unsafe { object_from(this) };
    let _configuration = unsafe { (*object).configuration_guard.lock() }
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !unsafe { (*object).locked.swap(false, Ordering::AcqRel) } {
        return APOERR_ALREADY_UNLOCKED;
    }
    apo_log!(
        etw::INFO,
        etw::CONFIGURATION,
        "UnlockForProcess completed instance={object:p} process_calls={} processed_frames={}",
        unsafe { (*object).process_calls.load(Ordering::Relaxed) },
        unsafe { (*object).processed_frames.load(Ordering::Relaxed) }
    );
    S_OK
}

// Real-time boundary: no logging, allocation, waits, COM calls, or syscalls.
unsafe extern "system" fn process(
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
        let (enabled, gain) = unsafe { (*object).params.load() };
        let silent =
            unsafe { (&mut *(*object).dsp.get()).process_with_settings(buffer, enabled, gain) };
        // Buffered speech must drain even when the next input packet is SILENT.
        if input.u32BufferFlags == BUFFER_SILENT && !silent {
            output_flags = BUFFER_VALID;
        }
    }
    output.u32ValidFrameCount = input.u32ValidFrameCount;
    output.u32BufferFlags = output_flags;
    unsafe {
        (*object)
            .processed_frames
            .fetch_add(input.u32ValidFrameCount as u64, Ordering::Relaxed);
    }
}

unsafe extern "system" fn identity_frames(_: *mut c_void, frames: u32) -> u32 {
    frames
}

static APO_VTABLE: IAudioProcessingObject_Vtbl = IAudioProcessingObject_Vtbl {
    base__: DELEGATING_UNKNOWN,
    Reset: reset,
    GetLatency: get_latency,
    GetRegistrationProperties: get_registration_properties,
    Initialize: initialize,
    IsInputFormatSupported: is_input_format_supported,
    IsOutputFormatSupported: is_output_format_supported,
    GetInputChannelCount: get_input_channel_count,
};

static CONFIGURATION_VTABLE: IAudioProcessingObjectConfiguration_Vtbl =
    IAudioProcessingObjectConfiguration_Vtbl {
        base__: DELEGATING_UNKNOWN,
        LockForProcess: lock_for_process,
        UnlockForProcess: unlock_for_process,
    };

static REALTIME_VTABLE: IAudioProcessingObjectRT_Vtbl = IAudioProcessingObjectRT_Vtbl {
    base__: DELEGATING_UNKNOWN,
    APOProcess: process,
    CalcInputFrames: identity_frames,
    CalcOutputFrames: identity_frames,
};

static SYSTEM_EFFECTS_VTABLE: IAudioSystemEffects3_Vtbl = IAudioSystemEffects3_Vtbl {
    base__: IAudioSystemEffects2_Vtbl {
        base__: windows::Win32::Media::Audio::Apo::IAudioSystemEffects_Vtbl {
            base__: DELEGATING_UNKNOWN,
        },
        GetEffectsList: get_effects_list,
    },
    GetControllableSystemEffectsList: get_controllable_effects_list,
    SetAudioSystemEffectState: set_effect_state,
};

fn create_apo(outer: *mut c_void) -> Result<*mut ApoCom, HRESULT> {
    let raw = unsafe { std::alloc::alloc(std::alloc::Layout::new::<ApoCom>()) }.cast::<ApoCom>();
    if raw.is_null() {
        return Err(E_OUTOFMEMORY);
    }
    unsafe {
        raw.write(ApoCom {
            params: Arc::new(crate::config::RtParams::default()),
            config_worker: None,
            inner_unknown: InterfaceHeader {
                vtable: &INNER_UNKNOWN,
                object: ptr::null_mut(),
            },
            apo: InterfaceHeader {
                vtable: &APO_VTABLE,
                object: ptr::null_mut(),
            },
            configuration: InterfaceHeader {
                vtable: &CONFIGURATION_VTABLE,
                object: ptr::null_mut(),
            },
            realtime: InterfaceHeader {
                vtable: &REALTIME_VTABLE,
                object: ptr::null_mut(),
            },
            system_effects: InterfaceHeader {
                vtable: &SYSTEM_EFFECTS_VTABLE,
                object: ptr::null_mut(),
            },
            controlling_unknown: ptr::null_mut(),
            inner_refs: AtomicU32::new(1),
            initialized: AtomicBool::new(false),
            locked: AtomicBool::new(false),
            configuration_guard: Mutex::new(()),
            dsp_in_use: AtomicBool::new(false),
            reset_requested: AtomicBool::new(false),
            effect_enabled: AtomicBool::new(true),
            discovery_only: AtomicBool::new(false),
            process_calls: AtomicU64::new(0),
            processed_frames: AtomicU64::new(0),
            input_channels: AtomicU32::new(0),
            max_input_frames: AtomicU32::new(0),
            max_output_frames: AtomicU32::new(0),
            latency_hns: AtomicI64::new(0),
            dsp: UnsafeCell::new(AudioProcessor::default()),
        })
    };
    let object = unsafe { &mut *raw };
    object.inner_unknown.object = raw;
    object.apo.object = raw;
    object.configuration.object = raw;
    object.realtime.object = raw;
    object.system_effects.object = raw;
    object.controlling_unknown = if outer.is_null() {
        &mut object.inner_unknown as *mut _ as *mut c_void
    } else {
        outer
    };
    LIVE_OBJECTS.fetch_add(1, Ordering::Release);
    Ok(raw)
}

#[repr(C)]
struct ClassFactory {
    vtable: *const IClassFactory_Vtbl,
}
unsafe impl Sync for ClassFactory {}

unsafe extern "system" fn factory_query_interface(
    this: *mut c_void,
    iid: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    if iid.is_null() || output.is_null() {
        return E_POINTER;
    }
    unsafe { *output = ptr::null_mut() };
    let iid = unsafe { *iid };
    if iid != IUnknown::IID && iid != IClassFactory::IID {
        return E_NOINTERFACE;
    }
    unsafe { *output = this };
    unsafe { factory_add_ref(this) };
    S_OK
}

unsafe extern "system" fn factory_add_ref(_: *mut c_void) -> u32 {
    FACTORY_REFS.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn factory_release(_: *mut c_void) -> u32 {
    FACTORY_REFS.fetch_sub(1, Ordering::Release) - 1
}

unsafe extern "system" fn factory_create_instance(
    _: *mut c_void,
    outer: *mut c_void,
    iid: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    if iid.is_null() || output.is_null() {
        return E_POINTER;
    }
    unsafe { *output = ptr::null_mut() };
    if !outer.is_null() && unsafe { *iid } != IUnknown::IID {
        return CLASS_E_NOAGGREGATION;
    }

    let object = match create_apo(outer) {
        Ok(object) => object,
        Err(result) => return result,
    };
    let inner = unsafe { ptr::addr_of_mut!((*object).inner_unknown).cast::<c_void>() };
    let result = if outer.is_null() {
        let result = unsafe { inner_query_interface(inner, iid, output) };
        unsafe { inner_release(inner) };
        result
    } else {
        unsafe { *output = inner };
        S_OK
    };
    apo_log!(
        if result == S_OK {
            etw::INFO
        } else {
            etw::ERROR
        },
        etw::LIFECYCLE,
        "CreateInstance instance={object:p} aggregated={} result=0x{:08x}",
        !outer.is_null(),
        result.0 as u32
    );
    result
}

unsafe extern "system" fn factory_lock_server(_: *mut c_void, lock: BOOL) -> HRESULT {
    if lock.as_bool() {
        SERVER_LOCKS.fetch_add(1, Ordering::Relaxed);
    } else {
        let _ = SERVER_LOCKS.try_update(Ordering::Release, Ordering::Relaxed, |value| {
            value.checked_sub(1)
        });
    }
    S_OK
}

static FACTORY_VTABLE: IClassFactory_Vtbl = IClassFactory_Vtbl {
    base__: IUnknown_Vtbl {
        QueryInterface: factory_query_interface,
        AddRef: factory_add_ref,
        Release: factory_release,
    },
    CreateInstance: factory_create_instance,
    LockServer: factory_lock_server,
};

static CLASS_FACTORY: ClassFactory = ClassFactory {
    vtable: &FACTORY_VTABLE,
};

#[unsafe(no_mangle)]
/// Return the VoiceBox class factory to the COM loader.
///
/// # Safety
///
/// The caller must provide valid COM ABI pointers. Null pointers are rejected.
pub unsafe extern "system" fn DllGetClassObject(
    clsid: *const GUID,
    iid: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    if clsid.is_null() || iid.is_null() || output.is_null() {
        return E_POINTER;
    }
    unsafe { *output = ptr::null_mut() };
    if unsafe { *clsid } != CLSID_VOICE_BOX_APO {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let result =
        unsafe { factory_query_interface(&CLASS_FACTORY as *const _ as *mut c_void, iid, output) };
    apo_log!(
        if result == S_OK {
            etw::VERBOSE
        } else {
            etw::WARNING
        },
        etw::LIFECYCLE,
        "DllGetClassObject result=0x{:08x}",
        result.0 as u32
    );
    result
}

#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if LIVE_OBJECTS.load(Ordering::Acquire) == 0
        && SERVER_LOCKS.load(Ordering::Acquire) == 0
        && FACTORY_REFS.load(Ordering::Acquire) == 1
    {
        etw::unregister();
        S_OK
    } else {
        S_FALSE
    }
}

fn write_wide(destination: &mut [u16], text: &str) {
    for (slot, value) in destination.iter_mut().zip(text.encode_utf16()) {
        *slot = value;
    }
}

#[cfg(test)]
mod tests;

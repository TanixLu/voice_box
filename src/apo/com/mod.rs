//! COM object identity, ownership and interface tables.
use super::{
    dsp::AudioProcessor,
    etw::{self, apo_log},
};
use core::{cell::UnsafeCell, ffi::c_void, ptr};
use std::{
    boxed::Box,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering, fence},
    },
};
use windows::Win32::{
    Foundation::{E_NOINTERFACE, E_OUTOFMEMORY, E_POINTER, S_OK},
    Media::{
        Audio::Apo::{
            IAudioProcessingObject, IAudioProcessingObject_Vtbl,
            IAudioProcessingObjectConfiguration, IAudioProcessingObjectConfiguration_Vtbl,
            IAudioProcessingObjectRT, IAudioProcessingObjectRT_Vtbl, IAudioSystemEffects,
            IAudioSystemEffects2, IAudioSystemEffects2_Vtbl, IAudioSystemEffects3,
            IAudioSystemEffects3_Vtbl,
        },
        KernelStreaming::AUDIO_EFFECT_TYPE_NOISE_SUPPRESSION,
    },
};
use windows_core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface};

mod effects;
pub(super) mod factory;
mod formats;
mod processing;

use effects::{
    get_controllable_effects_list, get_effects_list, get_registration_properties, initialize,
    set_effect_state,
};
use formats::{get_input_channel_count, is_input_format_supported, is_output_format_supported};
use processing::{
    get_latency, identity_frames, lock_for_process, process, reset, unlock_for_process,
};

const EFFECT_ID_DENOISE: GUID = AUDIO_EFFECT_TYPE_NOISE_SUPPRESSION;

static LIVE_OBJECTS: AtomicU32 = AtomicU32::new(0);

#[repr(C)]
struct InterfaceHeader<V> {
    vtable: *const V,
    object: *mut ApoCom,
}

/// A single carrier supports normal activation and COM aggregation.
#[repr(C)]
struct ApoCom {
    // Drop leases before any of the allocations whose pages they cover.
    resident_pages: Option<super::memory::LockedPages>,
    params: Arc<super::settings_watcher::RtParams>,
    settings_watcher: Option<super::settings_watcher::SettingsWatcher>,
    soundboard_path: Option<std::path::PathBuf>,
    injection: UnsafeCell<Option<super::soundboard::Injection>>,
    injection_stop: Option<Arc<AtomicBool>>,
    inner_unknown: InterfaceHeader<IUnknown_Vtbl>,
    apo: InterfaceHeader<IAudioProcessingObject_Vtbl>,
    process_control: InterfaceHeader<IAudioProcessingObjectConfiguration_Vtbl>,
    realtime: InterfaceHeader<IAudioProcessingObjectRT_Vtbl>,
    system_effects: InterfaceHeader<IAudioSystemEffects3_Vtbl>,
    controlling_unknown: *mut c_void,
    inner_refs: AtomicU32,
    initialized: AtomicBool,
    locked: AtomicBool,
    control_guard: Mutex<()>,
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

// Non-real-time control operations are serialized. Access to UnsafeCell is exclusive
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
            unsafe { ptr::addr_of_mut!((*object).process_control).cast::<c_void>() },
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
        // Detached workers retain their own DLL references through final cleanup.
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

static CONTROL_VTABLE: IAudioProcessingObjectConfiguration_Vtbl =
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
            resident_pages: None,
            params: Arc::new(super::settings_watcher::RtParams::default()),
            settings_watcher: None,
            soundboard_path: None,
            injection: UnsafeCell::new(None),
            injection_stop: None,
            inner_unknown: InterfaceHeader {
                vtable: &INNER_UNKNOWN,
                object: ptr::null_mut(),
            },
            apo: InterfaceHeader {
                vtable: &APO_VTABLE,
                object: ptr::null_mut(),
            },
            process_control: InterfaceHeader {
                vtable: &CONTROL_VTABLE,
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
            control_guard: Mutex::new(()),
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
    object.process_control.object = raw;
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

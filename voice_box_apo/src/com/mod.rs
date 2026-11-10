//! COM object identity, ownership and interface tables.
//! Entry points live by topic in `effects`, `formats` and `processing`; `factory` creates objects.

mod effects;
pub mod factory;
mod formats;
mod processing;
mod session;

use core::{ffi::c_void, ptr};
use std::{
    boxed::Box,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence},
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

use crate::{
    com::{
        effects::{
            get_controllable_effects_list, get_effects_list, get_registration_properties,
            initialize, set_effect_state,
        },
        formats::{get_input_channel_count, is_format_supported},
        processing::{
            calc_frames, get_latency, lock_for_process, process, reset, unlock_for_process,
        },
        session::ProcessingCell,
    },
    config::{ConfigWatcher, RtConfig},
    etw::{self, apo_log},
    memory::LockedPages,
};

const EFFECT_ID_DENOISE: GUID = AUDIO_EFFECT_TYPE_NOISE_SUPPRESSION;

static LIVE_OBJECTS: AtomicU32 = AtomicU32::new(0);

#[repr(C)]
struct InterfaceHeader<V> {
    vtable: *const V,
    object: *mut ApoCom,
}

/// A single carrier supports normal activation and COM aggregation.
///
/// Control state is accessed through its mutex; processing resources through
/// ProcessingCell. Control paths take the mutex before session access, while
/// real-time processing never takes the mutex. Reset only posts an atomic request.
#[repr(C)]
struct ApoCom {
    // Drop control's page leases before the allocations whose pages they cover.
    control: Mutex<ControlState>,
    config: Arc<RtConfig>,
    processing: ProcessingCell,
    inner_unknown: InterfaceHeader<IUnknown_Vtbl>,
    apo: InterfaceHeader<IAudioProcessingObject_Vtbl>,
    process_control: InterfaceHeader<IAudioProcessingObjectConfiguration_Vtbl>,
    realtime: InterfaceHeader<IAudioProcessingObjectRT_Vtbl>,
    system_effects: InterfaceHeader<IAudioSystemEffects3_Vtbl>,
    controlling_unknown: *mut c_void,
    inner_refs: AtomicU32,
    reset_requested: AtomicBool,
    process_calls: AtomicU64,
    processed_frames: AtomicU64,
}

/// Non-RT state serialized with session preparation. Queries may wait for model
/// construction, page locking and injection startup; the RT path never uses it.
struct ControlState {
    // This lease also covers the APO and RtConfig allocations.
    resident_pages: Option<LockedPages>,
    config_watcher: Option<ConfigWatcher>,
    soundboard_name: Option<String>,
    injection_stop: Option<Arc<AtomicBool>>,
    initialized: bool,
    discovery_only: bool,
    supports_effects: bool,
    // Query snapshots: channels may come from negotiation before a session exists;
    // latency comes from the last successful lock. Unlock preserves both values.
    input_channels: u32,
    latency_hns: i64,
}

impl ApoCom {
    fn control(&self) -> MutexGuard<'_, ControlState> {
        self.control
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
}

impl Drop for ApoCom {
    fn drop(&mut self) {
        // The detached configuration watcher may outlive this COM instance.
        self.config.clear_effects_events();
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
            etw::level::ERROR,
            etw::keyword::LIFECYCLE,
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
            etw::level::VERBOSE
        } else {
            etw::level::WARNING
        },
        etw::keyword::LIFECYCLE,
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
            etw::level::ERROR,
            etw::keyword::LIFECYCLE,
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
            etw::level::WARNING,
            etw::keyword::LIFECYCLE,
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
        etw::level::VERBOSE,
        etw::keyword::LIFECYCLE,
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
        let live_objects = LIVE_OBJECTS.load(Ordering::Relaxed);
        apo_log!(
            etw::level::INFO,
            etw::keyword::LIFECYCLE,
            "Destroy instance={object:p} initialized={} locked={} process_calls={} processed_frames={} live_objects={live_objects}",
            unsafe { (*object).control().initialized },
            unsafe { (*object).processing.is_accepting() },
            unsafe { (*object).process_calls.load(Ordering::Relaxed) },
            unsafe { (*object).processed_frames.load(Ordering::Relaxed) }
        );
        unsafe { drop(Box::from_raw(object)) };
        // Detached workers retain their own DLL references through final cleanup.
        // Publish completed destruction to DllCanUnloadNow's Acquire load.
        LIVE_OBJECTS.fetch_sub(1, Ordering::Release);
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
    IsInputFormatSupported: is_format_supported,
    IsOutputFormatSupported: is_format_supported,
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
    CalcInputFrames: calc_frames,
    CalcOutputFrames: calc_frames,
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
            control: Mutex::new(ControlState {
                resident_pages: None,
                config_watcher: None,
                soundboard_name: None,
                injection_stop: None,
                initialized: false,
                discovery_only: false,
                supports_effects: true,
                input_channels: 0,
                latency_hns: 0,
            }),
            config: Arc::new(RtConfig::default()),
            processing: ProcessingCell::new(),
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
            reset_requested: AtomicBool::new(false),
            process_calls: AtomicU64::new(0),
            processed_frames: AtomicU64::new(0),
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
    LIVE_OBJECTS.fetch_add(1, Ordering::Relaxed);
    Ok(raw)
}

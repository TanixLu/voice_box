use super::{EFFECT_ID_DENOISE, object_from};
use crate::{
    apo::etw::{self, apo_log},
    shared::plugin,
};
use core::{ffi::c_void, ptr};
use std::sync::{Arc, atomic::Ordering};
use windows::Win32::{
    Foundation::{E_INVALIDARG, E_NOTIMPL, E_OUTOFMEMORY, E_POINTER, HANDLE, S_OK},
    Media::{
        Audio::Apo::{
            APO_REG_PROPERTIES, APOERR_ALREADY_INITIALIZED, APOERR_INVALID_APO_CLSID,
            APOInitBaseStruct, APOInitSystemEffects2, APOInitSystemEffects3, AUDIO_SYSTEMEFFECT,
            AUDIO_SYSTEMEFFECT_STATE_OFF, AUDIO_SYSTEMEFFECT_STATE_ON,
        },
        KernelStreaming::{
            AUDIO_SIGNALPROCESSINGMODE_COMMUNICATIONS, AUDIO_SIGNALPROCESSINGMODE_DEFAULT,
            AUDIO_SIGNALPROCESSINGMODE_FAR_FIELD_SPEECH, AUDIO_SIGNALPROCESSINGMODE_MEDIA,
            AUDIO_SIGNALPROCESSINGMODE_MOVIE, AUDIO_SIGNALPROCESSINGMODE_NOTIFICATION,
            AUDIO_SIGNALPROCESSINGMODE_RAW, AUDIO_SIGNALPROCESSINGMODE_SPEECH,
        },
    },
    System::Com::{CoTaskMemAlloc, CoTaskMemFree},
};
use windows_core::{BOOL, GUID, HRESULT};

pub(super) unsafe extern "system" fn get_registration_properties(
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

    let properties = plugin::registration_properties();

    unsafe {
        memory.write(properties);
        *output = memory;
    }
    S_OK
}

pub(super) unsafe extern "system" fn initialize(
    this: *mut c_void,
    size: u32,
    data: *const u8,
) -> HRESULT {
    let object = unsafe { object_from(this) };
    let _control =
        unsafe { (*object).control_guard.lock() }.unwrap_or_else(|poisoned| poisoned.into_inner());
    if unsafe { (*object).initialized.load(Ordering::Acquire) } {
        return APOERR_ALREADY_INITIALIZED;
    }
    if data.is_null() || (size as usize) < core::mem::size_of::<APOInitBaseStruct>() {
        return E_INVALIDARG;
    }
    if unsafe { ptr::read_unaligned(data.cast::<APOInitBaseStruct>()) }.clsid != plugin::CLSID {
        return APOERR_INVALID_APO_CLSID;
    }

    let (version, mode, discovery_only, soundboard_path) =
        if size as usize == core::mem::size_of::<APOInitSystemEffects3>() {
            let init = unsafe { ptr::read_unaligned(data.cast::<APOInitSystemEffects3>()) };
            (
                3,
                init.AudioProcessingMode,
                init.InitializeForDiscoveryOnly.as_bool(),
                soundboard_path(init.pAPOEndpointProperties.as_ref()),
            )
        } else if size as usize == core::mem::size_of::<APOInitSystemEffects2>() {
            let init = unsafe { ptr::read_unaligned(data.cast::<APOInitSystemEffects2>()) };
            (
                2,
                init.AudioProcessingMode,
                init.InitializeForDiscoveryOnly.as_bool(),
                soundboard_path(init.pAPOEndpointProperties.as_ref()),
            )
        } else {
            apo_log!(
                etw::ERROR,
                etw::LIFECYCLE,
                "Initialize rejected instance={object:p} unsupported_data_size={size}"
            );
            return E_INVALIDARG;
        };

    unsafe {
        (*object).soundboard_path = soundboard_path;
        if mode != AUDIO_SIGNALPROCESSINGMODE_RAW {
            // Discovery objects also expose live effect lists and change events.
            // They watch settings but still cannot allocate/process a DSP graph.
            match crate::apo::settings_watcher::SettingsWatcher::start(Arc::clone(
                &(*object).params,
            )) {
                Ok(worker) => (*object).settings_watcher = Some(worker),
                Err(error) => {
                    apo_log!(
                        etw::WARNING,
                        etw::SETTINGS,
                        "Settings watcher failed: {error}"
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

fn soundboard_path(
    store: Option<&windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore>,
) -> Option<std::path::PathBuf> {
    use windows::Win32::{
        Media::Audio::PKEY_AudioEndpoint_GUID,
        System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc},
    };
    let store = store?;
    let mut value = unsafe { store.GetValue(&PKEY_AudioEndpoint_GUID) }.ok()?;
    let text = unsafe { PropVariantToStringAlloc(&value) };
    unsafe {
        let _ = PropVariantClear(&mut value);
    }
    let text = text.ok()?;
    let guid = unsafe { text.to_string() };
    unsafe {
        CoTaskMemFree(Some(text.0.cast()));
    }
    crate::shared::pcm_transport::transport_path(&guid.ok()?).ok()
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

pub(super) unsafe extern "system" fn get_effects_list(
    this: *mut c_void,
    effects: *mut *mut GUID,
    count: *mut u32,
    event: HANDLE,
) -> HRESULT {
    if effects.is_null() || count.is_null() {
        return E_POINTER;
    }
    unsafe {
        *effects = ptr::null_mut();
        *count = 0;
    }
    let object = unsafe { object_from(this) };
    let supports_effect = unsafe { (*object).effect_enabled.load(Ordering::Acquire) };
    // Allocate before subscribing so an allocation failure cannot replace the
    // caller's existing subscription. The returned snapshot and subscription
    // are synchronized even if Enabled changes while this call is running.
    let memory = if supports_effect {
        unsafe { CoTaskMemAlloc(core::mem::size_of::<GUID>()) }.cast::<GUID>()
    } else {
        ptr::null_mut()
    };
    if supports_effect && memory.is_null() {
        return E_OUTOFMEMORY;
    }
    let denoise = match unsafe { (*object).params.subscribe_effects(event, false) } {
        Ok(enabled) => enabled,
        Err(error) => {
            unsafe { CoTaskMemFree(Some(memory.cast())) };
            return error.code();
        }
    };
    if !supports_effect || !denoise {
        unsafe { CoTaskMemFree(Some(memory.cast())) };
        return S_OK;
    }
    unsafe {
        memory.write(EFFECT_ID_DENOISE);
        *effects = memory;
        *count = 1;
    }
    apo_log!(
        etw::INFO,
        etw::LIFECYCLE,
        "GetEffectsList instance={object:p} denoise={denoise}"
    );
    S_OK
}

pub(super) unsafe extern "system" fn get_controllable_effects_list(
    this: *mut c_void,
    effects: *mut *mut AUDIO_SYSTEMEFFECT,
    count: *mut u32,
    event: HANDLE,
) -> HRESULT {
    if effects.is_null() || count.is_null() {
        return E_POINTER;
    }
    unsafe {
        *effects = ptr::null_mut();
        *count = 0;
    }
    let object = unsafe { object_from(this) };
    let supports_effect = unsafe { (*object).effect_enabled.load(Ordering::Acquire) };
    let memory = if supports_effect {
        unsafe { CoTaskMemAlloc(core::mem::size_of::<AUDIO_SYSTEMEFFECT>()) }
            .cast::<AUDIO_SYSTEMEFFECT>()
    } else {
        ptr::null_mut()
    };
    if supports_effect && memory.is_null() {
        return E_OUTOFMEMORY;
    }
    let denoise = match unsafe { (*object).params.subscribe_effects(event, true) } {
        Ok(enabled) => enabled,
        Err(error) => {
            unsafe { CoTaskMemFree(Some(memory.cast())) };
            return error.code();
        }
    };
    if !supports_effect {
        return S_OK;
    }
    unsafe {
        memory.write(AUDIO_SYSTEMEFFECT {
            id: EFFECT_ID_DENOISE,
            canSetState: BOOL::from(false),
            state: if denoise {
                AUDIO_SYSTEMEFFECT_STATE_ON
            } else {
                AUDIO_SYSTEMEFFECT_STATE_OFF
            },
        });
        *effects = memory;
        *count = 1;
    }
    apo_log!(
        etw::INFO,
        etw::LIFECYCLE,
        "GetControllableSystemEffectsList instance={object:p} denoise={denoise} controllable=false"
    );
    S_OK
}

pub(super) unsafe extern "system" fn set_effect_state(
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

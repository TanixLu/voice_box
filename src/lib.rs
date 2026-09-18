//! Minimal pure-Rust SFX Audio Processing Object (APO).
//!
//! This DLL implements the raw COM ABI expected by the Windows audio engine.
//! The first version is deliberately narrow: Float32, interleaved stereo,
//! one input and one output, with zero latency. It swaps L/R per frame.

use core::ffi::c_void;
use core::ptr;
use std::boxed::Box;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

type Hresult = i32;
const S_OK: Hresult = 0;
const S_FALSE: Hresult = 1;
const E_NOINTERFACE: Hresult = 0x8000_4002_u32 as Hresult;
const E_POINTER: Hresult = 0x8000_4003_u32 as Hresult;
const E_INVALIDARG: Hresult = 0x8007_0057_u32 as Hresult;
const E_OUTOFMEMORY: Hresult = 0x8007_000e_u32 as Hresult;
const CLASS_E_NOAGGREGATION: Hresult = 0x8004_0110_u32 as Hresult;
const CLASS_E_CLASSNOTAVAILABLE: Hresult = 0x8004_0111_u32 as Hresult;
const APOERR_ALREADY_INITIALIZED: Hresult = 0x887d_0001_u32 as Hresult;
const APOERR_NOT_INITIALIZED: Hresult = 0x887d_0002_u32 as Hresult;
const APOERR_INVALID_APO_CLSID: Hresult = 0x887d_0004_u32 as Hresult;
const APOERR_ALREADY_UNLOCKED: Hresult = 0x887d_0006_u32 as Hresult;
const APOERR_NUM_CONNECTIONS_INVALID: Hresult = 0x887d_0007_u32 as Hresult;
const APOERR_APO_LOCKED: Hresult = 0x887d_000a_u32 as Hresult;
const BUFFER_VALID: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const IID_IUNKNOWN: Guid = Guid {
    data1: 0,
    data2: 0,
    data3: 0,
    data4: [0xc0, 0, 0, 0, 0, 0, 0, 0x46],
};
const IID_ICLASS_FACTORY: Guid = Guid {
    data1: 1,
    data2: 0,
    data3: 0,
    data4: [0xc0, 0, 0, 0, 0, 0, 0, 0x46],
};
const IID_IAUDIO_PROCESSING_OBJECT_RT: Guid = Guid {
    data1: 0x9e1d6a6d,
    data2: 0xddbc,
    data3: 0x4e95,
    data4: [0xa4, 0xc7, 0xad, 0x64, 0xba, 0x37, 0x84, 0x6c],
};
const IID_IAUDIO_PROCESSING_OBJECT_CONFIGURATION: Guid = Guid {
    data1: 0x0e5ed805,
    data2: 0xaba6,
    data3: 0x49c3,
    data4: [0x8f, 0x9a, 0x2b, 0x8c, 0x88, 0x9c, 0x4f, 0xa8],
};
const IID_IAUDIO_PROCESSING_OBJECT: Guid = Guid {
    data1: 0xfd7f2b29,
    data2: 0x24d0,
    data3: 0x4b5c,
    data4: [0xb1, 0x77, 0x59, 0x2c, 0x39, 0xf9, 0xca, 0x10],
};
const IID_IAUDIO_SYSTEM_EFFECTS: Guid = Guid {
    data1: 0x5fa00f27,
    data2: 0xadd6,
    data3: 0x499a,
    data4: [0x8a, 0x9d, 0x6b, 0x98, 0x52, 0x1f, 0xa7, 0x5b],
};
const CLSID_VOICE_BOX_APO: Guid = Guid {
    data1: 0xae672075,
    data2: 0xb244,
    data3: 0x4417,
    data4: [0xb5, 0xa5, 0xfb, 0xd4, 0x5b, 0x7f, 0x02, 0x68],
};

#[repr(C)]
struct ApoConnectionDescriptor {
    _type: u32,
    _buffer: usize,
    _max_frames: u32,
    _format: *mut c_void,
    _signature: u32,
}
#[repr(C)]
struct ApoConnectionProperty {
    buffer: *mut c_void,
    valid_frames: u32,
    buffer_flags: u32,
    _signature: u32,
}
#[repr(C)]
struct ApoInitBase {
    _size: u32,
    clsid: Guid,
}
#[repr(C)]
struct ApoRegistrationProperties {
    clsid: Guid,
    flags: u32,
    friendly_name: [u16; 256],
    copyright: [u16; 256],
    major_version: u32,
    minor_version: u32,
    min_inputs: u32,
    max_inputs: u32,
    min_outputs: u32,
    max_outputs: u32,
    max_instances: u32,
    interface_count: u32,
    interfaces: [Guid; 1],
}

#[repr(C)]
struct InterfaceHeader<V> {
    vtbl: *const V,
    object: *mut VoiceBoxApo,
}
#[repr(C)]
struct UnknownVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> Hresult,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}
#[repr(C)]
struct AudioProcessingObjectVtbl {
    unknown: UnknownVtbl,
    reset: unsafe extern "system" fn(*mut c_void) -> Hresult,
    get_latency: unsafe extern "system" fn(*mut c_void, *mut i64) -> Hresult,
    get_registration_properties:
        unsafe extern "system" fn(*mut c_void, *mut *mut ApoRegistrationProperties) -> Hresult,
    initialize: unsafe extern "system" fn(*mut c_void, u32, *mut u8) -> Hresult,
    is_input_format_supported: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *mut *mut c_void,
    ) -> Hresult,
    is_output_format_supported: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *mut *mut c_void,
    ) -> Hresult,
    get_input_channel_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> Hresult,
}
#[repr(C)]
struct AudioProcessingObjectConfigurationVtbl {
    unknown: UnknownVtbl,
    lock_for_process: unsafe extern "system" fn(
        *mut c_void,
        u32,
        *mut *mut ApoConnectionDescriptor,
        u32,
        *mut *mut ApoConnectionDescriptor,
    ) -> Hresult,
    unlock_for_process: unsafe extern "system" fn(*mut c_void) -> Hresult,
}
#[repr(C)]
struct AudioProcessingObjectRtVtbl {
    unknown: UnknownVtbl,
    apo_process: unsafe extern "system" fn(
        *mut c_void,
        u32,
        *mut *mut ApoConnectionProperty,
        u32,
        *mut *mut ApoConnectionProperty,
    ),
    calc_input_frames: unsafe extern "system" fn(*mut c_void, u32) -> u32,
    calc_output_frames: unsafe extern "system" fn(*mut c_void, u32) -> u32,
}
#[repr(C)]
struct ClassFactoryVtbl {
    unknown: UnknownVtbl,
    create_instance: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *const Guid,
        *mut *mut c_void,
    ) -> Hresult,
    lock_server: unsafe extern "system" fn(*mut c_void, i32) -> Hresult,
}

#[repr(C)]
struct VoiceBoxApo {
    apo: InterfaceHeader<AudioProcessingObjectVtbl>,
    configuration: InterfaceHeader<AudioProcessingObjectConfigurationVtbl>,
    realtime: InterfaceHeader<AudioProcessingObjectRtVtbl>,
    system_effects: InterfaceHeader<UnknownVtbl>,
    refs: AtomicU32,
    initialized: AtomicBool,
    locked: AtomicBool,
}
static LIVE_OBJECTS: AtomicU32 = AtomicU32::new(0);

unsafe fn object_from_interface(this: *mut c_void) -> *mut VoiceBoxApo {
    unsafe { (*(this as *mut InterfaceHeader<UnknownVtbl>)).object }
}
unsafe extern "system" fn apo_query_interface(
    this: *mut c_void,
    iid: *const Guid,
    output: *mut *mut c_void,
) -> Hresult {
    if iid.is_null() || output.is_null() {
        return E_POINTER;
    }
    unsafe {
        *output = ptr::null_mut();
    }
    let object = unsafe { object_from_interface(this) };
    let iid = unsafe { *iid };
    let result = if iid == IID_IUNKNOWN || iid == IID_IAUDIO_PROCESSING_OBJECT {
        unsafe { &mut (*object).apo as *mut _ as *mut c_void }
    } else if iid == IID_IAUDIO_PROCESSING_OBJECT_CONFIGURATION {
        unsafe { &mut (*object).configuration as *mut _ as *mut c_void }
    } else if iid == IID_IAUDIO_PROCESSING_OBJECT_RT {
        unsafe { &mut (*object).realtime as *mut _ as *mut c_void }
    } else if iid == IID_IAUDIO_SYSTEM_EFFECTS {
        unsafe { &mut (*object).system_effects as *mut _ as *mut c_void }
    } else {
        return E_NOINTERFACE;
    };
    unsafe {
        *output = result;
        apo_add_ref(this);
    }
    S_OK
}
unsafe extern "system" fn apo_add_ref(this: *mut c_void) -> u32 {
    let object = unsafe { object_from_interface(this) };
    unsafe { (*object).refs.fetch_add(1, Ordering::Relaxed) + 1 }
}
unsafe extern "system" fn apo_release(this: *mut c_void) -> u32 {
    let object = unsafe { object_from_interface(this) };
    let refs = unsafe { (*object).refs.fetch_sub(1, Ordering::Release) - 1 };
    if refs == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        LIVE_OBJECTS.fetch_sub(1, Ordering::Release);
        unsafe {
            drop(Box::from_raw(object));
        }
    }
    refs
}
unsafe extern "system" fn apo_reset(_: *mut c_void) -> Hresult {
    S_OK
}
unsafe extern "system" fn apo_get_latency(_: *mut c_void, latency: *mut i64) -> Hresult {
    if latency.is_null() {
        return E_POINTER;
    }
    unsafe {
        *latency = 0;
    }
    S_OK
}
unsafe extern "system" fn apo_get_registration_properties(
    _: *mut c_void,
    output: *mut *mut ApoRegistrationProperties,
) -> Hresult {
    if output.is_null() {
        return E_POINTER;
    }
    let memory = unsafe { CoTaskMemAlloc(core::mem::size_of::<ApoRegistrationProperties>()) }
        as *mut ApoRegistrationProperties;
    if memory.is_null() {
        return E_OUTOFMEMORY;
    }
    let mut props = ApoRegistrationProperties {
        clsid: CLSID_VOICE_BOX_APO,
        flags: 0x0d,
        friendly_name: [0; 256],
        copyright: [0; 256],
        major_version: 1,
        minor_version: 0,
        min_inputs: 1,
        max_inputs: 1,
        min_outputs: 1,
        max_outputs: 1,
        max_instances: u32::MAX,
        interface_count: 1,
        interfaces: [IID_IAUDIO_PROCESSING_OBJECT],
    };
    write_wide(&mut props.friendly_name, "VoiceBox APO");
    write_wide(&mut props.copyright, "Copyright (C) 2026 Tanix");
    unsafe {
        memory.write(props);
        *output = memory;
    }
    S_OK
}
unsafe extern "system" fn apo_initialize(this: *mut c_void, size: u32, data: *mut u8) -> Hresult {
    let object = unsafe { object_from_interface(this) };
    if unsafe { (*object).initialized.load(Ordering::Acquire) } {
        return APOERR_ALREADY_INITIALIZED;
    }
    if data.is_null() || (size as usize) < core::mem::size_of::<ApoInitBase>() {
        return E_INVALIDARG;
    }
    if unsafe { (*(data as *const ApoInitBase)).clsid } != CLSID_VOICE_BOX_APO {
        return APOERR_INVALID_APO_CLSID;
    }
    unsafe {
        (*object).initialized.store(true, Ordering::Release);
    }
    S_OK
}
unsafe fn support_requested_format(requested: *mut c_void, supported: *mut *mut c_void) -> Hresult {
    if requested.is_null() || supported.is_null() {
        return E_POINTER;
    }
    let vtbl = unsafe { *(requested as *mut *const UnknownVtbl) };
    unsafe {
        ((*vtbl).add_ref)(requested);
        *supported = requested;
    }
    S_OK
}
unsafe extern "system" fn apo_is_input_format_supported(
    _: *mut c_void,
    _: *mut c_void,
    requested: *mut c_void,
    supported: *mut *mut c_void,
) -> Hresult {
    unsafe { support_requested_format(requested, supported) }
}
unsafe extern "system" fn apo_is_output_format_supported(
    _: *mut c_void,
    _: *mut c_void,
    requested: *mut c_void,
    supported: *mut *mut c_void,
) -> Hresult {
    unsafe { support_requested_format(requested, supported) }
}
unsafe extern "system" fn apo_get_input_channel_count(
    this: *mut c_void,
    count: *mut u32,
) -> Hresult {
    if count.is_null() {
        return E_POINTER;
    }
    let object = unsafe { object_from_interface(this) };
    if !unsafe { (*object).initialized.load(Ordering::Acquire) } {
        return APOERR_NOT_INITIALIZED;
    }
    unsafe {
        *count = 2;
    }
    S_OK
}
unsafe extern "system" fn apo_lock_for_process(
    this: *mut c_void,
    input_count: u32,
    inputs: *mut *mut ApoConnectionDescriptor,
    output_count: u32,
    outputs: *mut *mut ApoConnectionDescriptor,
) -> Hresult {
    let object = unsafe { object_from_interface(this) };
    if !unsafe { (*object).initialized.load(Ordering::Acquire) } {
        return APOERR_NOT_INITIALIZED;
    }
    if input_count != 1 || output_count != 1 {
        return APOERR_NUM_CONNECTIONS_INVALID;
    }
    if inputs.is_null()
        || outputs.is_null()
        || unsafe { (*inputs).is_null() || (*outputs).is_null() }
    {
        return E_POINTER;
    }
    if unsafe { (*object).locked.swap(true, Ordering::AcqRel) } {
        return APOERR_APO_LOCKED;
    }
    S_OK
}
unsafe extern "system" fn apo_unlock_for_process(this: *mut c_void) -> Hresult {
    let object = unsafe { object_from_interface(this) };
    if !unsafe { (*object).locked.swap(false, Ordering::AcqRel) } {
        return APOERR_ALREADY_UNLOCKED;
    }
    S_OK
}
unsafe extern "system" fn apo_process(
    this: *mut c_void,
    input_count: u32,
    inputs: *mut *mut ApoConnectionProperty,
    output_count: u32,
    outputs: *mut *mut ApoConnectionProperty,
) {
    if input_count != 1 || output_count != 1 || inputs.is_null() || outputs.is_null() {
        return;
    }
    let object = unsafe { object_from_interface(this) };
    if !unsafe { (*object).locked.load(Ordering::Acquire) } {
        return;
    }
    let input = unsafe { *inputs };
    let output = unsafe { *outputs };
    if input.is_null() || output.is_null() {
        return;
    }
    let input = unsafe { &*input };
    let output = unsafe { &mut *output };
    output.valid_frames = input.valid_frames;
    output.buffer_flags = input.buffer_flags;
    if input.buffer_flags != BUFFER_VALID || input.buffer.is_null() || output.buffer.is_null() {
        return;
    }
    let samples = match (input.valid_frames as usize).checked_mul(2) {
        Some(value) => value,
        None => return,
    };
    let source = input.buffer as *const f32;
    let destination = output.buffer as *mut f32;
    if source != destination {
        unsafe {
            ptr::copy(source, destination, samples);
        }
    }
    let mut frame = 0usize;
    while frame < input.valid_frames as usize {
        let pair = unsafe { destination.add(frame * 2) };
        unsafe {
            ptr::swap(pair, pair.add(1));
        }
        frame += 1;
    }
}
unsafe extern "system" fn apo_calc_input_frames(_: *mut c_void, frames: u32) -> u32 {
    frames
}
unsafe extern "system" fn apo_calc_output_frames(_: *mut c_void, frames: u32) -> u32 {
    frames
}

static APO_VTBL: AudioProcessingObjectVtbl = AudioProcessingObjectVtbl {
    unknown: UnknownVtbl {
        query_interface: apo_query_interface,
        add_ref: apo_add_ref,
        release: apo_release,
    },
    reset: apo_reset,
    get_latency: apo_get_latency,
    get_registration_properties: apo_get_registration_properties,
    initialize: apo_initialize,
    is_input_format_supported: apo_is_input_format_supported,
    is_output_format_supported: apo_is_output_format_supported,
    get_input_channel_count: apo_get_input_channel_count,
};
static CONFIGURATION_VTBL: AudioProcessingObjectConfigurationVtbl =
    AudioProcessingObjectConfigurationVtbl {
        unknown: UnknownVtbl {
            query_interface: apo_query_interface,
            add_ref: apo_add_ref,
            release: apo_release,
        },
        lock_for_process: apo_lock_for_process,
        unlock_for_process: apo_unlock_for_process,
    };
static REALTIME_VTBL: AudioProcessingObjectRtVtbl = AudioProcessingObjectRtVtbl {
    unknown: UnknownVtbl {
        query_interface: apo_query_interface,
        add_ref: apo_add_ref,
        release: apo_release,
    },
    apo_process,
    calc_input_frames: apo_calc_input_frames,
    calc_output_frames: apo_calc_output_frames,
};
static SYSTEM_EFFECTS_VTBL: UnknownVtbl = UnknownVtbl {
    query_interface: apo_query_interface,
    add_ref: apo_add_ref,
    release: apo_release,
};

fn create_apo() -> *mut VoiceBoxApo {
    let mut apo = Box::new(VoiceBoxApo {
        apo: InterfaceHeader {
            vtbl: &APO_VTBL,
            object: ptr::null_mut(),
        },
        configuration: InterfaceHeader {
            vtbl: &CONFIGURATION_VTBL,
            object: ptr::null_mut(),
        },
        realtime: InterfaceHeader {
            vtbl: &REALTIME_VTBL,
            object: ptr::null_mut(),
        },
        system_effects: InterfaceHeader {
            vtbl: &SYSTEM_EFFECTS_VTBL,
            object: ptr::null_mut(),
        },
        refs: AtomicU32::new(1),
        initialized: AtomicBool::new(false),
        locked: AtomicBool::new(false),
    });
    let raw = &mut *apo as *mut VoiceBoxApo;
    apo.apo.object = raw;
    apo.configuration.object = raw;
    apo.realtime.object = raw;
    apo.system_effects.object = raw;
    LIVE_OBJECTS.fetch_add(1, Ordering::Release);
    Box::into_raw(apo)
}

#[repr(C)]
struct ClassFactory {
    vtbl: *const ClassFactoryVtbl,
}
// The factory is immutable and contains only a pointer to its immutable vtable.
unsafe impl Sync for ClassFactory {}
unsafe extern "system" fn factory_query_interface(
    this: *mut c_void,
    iid: *const Guid,
    output: *mut *mut c_void,
) -> Hresult {
    if iid.is_null() || output.is_null() {
        return E_POINTER;
    }
    unsafe {
        *output = ptr::null_mut();
    }
    let iid = unsafe { *iid };
    if iid != IID_IUNKNOWN && iid != IID_ICLASS_FACTORY {
        return E_NOINTERFACE;
    }
    unsafe {
        *output = this;
    }
    2
}
unsafe extern "system" fn factory_add_ref(_: *mut c_void) -> u32 {
    2
}
unsafe extern "system" fn factory_release(_: *mut c_void) -> u32 {
    1
}
unsafe extern "system" fn factory_create_instance(
    _: *mut c_void,
    outer: *mut c_void,
    iid: *const Guid,
    output: *mut *mut c_void,
) -> Hresult {
    if !outer.is_null() {
        return CLASS_E_NOAGGREGATION;
    }
    if iid.is_null() || output.is_null() {
        return E_POINTER;
    }
    let object = create_apo();
    let result =
        unsafe { apo_query_interface(&mut (*object).apo as *mut _ as *mut c_void, iid, output) };
    unsafe {
        apo_release(&mut (*object).apo as *mut _ as *mut c_void);
    }
    result
}
unsafe extern "system" fn factory_lock_server(_: *mut c_void, _: i32) -> Hresult {
    S_OK
}
static FACTORY_VTBL: ClassFactoryVtbl = ClassFactoryVtbl {
    unknown: UnknownVtbl {
        query_interface: factory_query_interface,
        add_ref: factory_add_ref,
        release: factory_release,
    },
    create_instance: factory_create_instance,
    lock_server: factory_lock_server,
};
static CLASS_FACTORY: ClassFactory = ClassFactory {
    vtbl: &FACTORY_VTBL,
};

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    clsid: *const Guid,
    iid: *const Guid,
    output: *mut *mut c_void,
) -> Hresult {
    if clsid.is_null() || iid.is_null() || output.is_null() {
        return E_POINTER;
    }
    if unsafe { *clsid } != CLSID_VOICE_BOX_APO {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    unsafe { factory_query_interface(&CLASS_FACTORY as *const _ as *mut c_void, iid, output) }
}
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> Hresult {
    if LIVE_OBJECTS.load(Ordering::Acquire) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoTaskMemAlloc(bytes: usize) -> *mut c_void;
}
fn write_wide(destination: &mut [u16], text: &str) {
    for (slot, value) in destination.iter_mut().zip(text.encode_utf16()) {
        *slot = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swaps_each_stereo_pair() {
        let mut samples = [1.0_f32, 2.0, 3.0, 4.0];
        for pair in samples.chunks_exact_mut(2) {
            pair.swap(0, 1);
        }
        assert_eq!(samples, [2.0, 1.0, 4.0, 3.0]);
    }

    #[test]
    fn apo_lifecycle_swaps_an_out_of_place_buffer() {
        unsafe {
            let object = create_apo();
            let apo = &mut (*object).apo as *mut _ as *mut c_void;
            let mut init = ApoInitBase {
                _size: core::mem::size_of::<ApoInitBase>() as u32,
                clsid: CLSID_VOICE_BOX_APO,
            };
            assert_eq!(
                apo_initialize(
                    apo,
                    core::mem::size_of::<ApoInitBase>() as u32,
                    &mut init as *mut _ as *mut u8
                ),
                S_OK
            );

            let mut input_descriptor = ApoConnectionDescriptor {
                _type: 0,
                _buffer: 0,
                _max_frames: 2,
                _format: ptr::null_mut(),
                _signature: 0,
            };
            let mut output_descriptor = ApoConnectionDescriptor {
                _type: 0,
                _buffer: 0,
                _max_frames: 2,
                _format: ptr::null_mut(),
                _signature: 0,
            };
            let mut descriptors_in = [&mut input_descriptor as *mut ApoConnectionDescriptor];
            let mut descriptors_out = [&mut output_descriptor as *mut ApoConnectionDescriptor];
            assert_eq!(
                apo_lock_for_process(
                    apo,
                    1,
                    descriptors_in.as_mut_ptr(),
                    1,
                    descriptors_out.as_mut_ptr()
                ),
                S_OK
            );

            let mut input = [0.25_f32, -0.5, 0.75, -1.0];
            let mut output = [0.0_f32; 4];
            let mut input_property = ApoConnectionProperty {
                buffer: input.as_mut_ptr().cast(),
                valid_frames: 2,
                buffer_flags: BUFFER_VALID,
                _signature: 0,
            };
            let mut output_property = ApoConnectionProperty {
                buffer: output.as_mut_ptr().cast(),
                valid_frames: 0,
                buffer_flags: 0,
                _signature: 0,
            };
            let mut properties_in = [&mut input_property as *mut ApoConnectionProperty];
            let mut properties_out = [&mut output_property as *mut ApoConnectionProperty];
            apo_process(
                apo,
                1,
                properties_in.as_mut_ptr(),
                1,
                properties_out.as_mut_ptr(),
            );
            assert_eq!(output, [-0.5, 0.25, -1.0, 0.75]);
            assert_eq!(apo_unlock_for_process(apo), S_OK);
            apo_release(apo);
        }
    }
}

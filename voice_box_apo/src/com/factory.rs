//! DLL exports and the class factory: `DllGetClassObject`, `DllCanUnloadNow` and `IClassFactory`.

use core::{ffi::c_void, ptr};
use std::sync::atomic::{AtomicU32, Ordering};

use voice_box_shared::apo_identity::APO_CLSID;
use windows::Win32::{
    Foundation::{
        CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_NOINTERFACE, E_POINTER, S_FALSE, S_OK,
    },
    System::Com::{IClassFactory, IClassFactory_Vtbl},
};
use windows_core::{BOOL, GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface};

use crate::{
    com::{LIVE_OBJECTS, create_apo, inner_query_interface, inner_release},
    etw::{self, apo_log},
    worker,
};

static SERVER_LOCKS: AtomicU32 = AtomicU32::new(0);
static FACTORY_REFS: AtomicU32 = AtomicU32::new(1);

#[repr(C)]
struct ClassFactory {
    vtable: *const IClassFactory_Vtbl,
}
unsafe impl Sync for ClassFactory {}

unsafe extern "system" fn query_interface(
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
    unsafe { add_ref(this) };
    S_OK
}

unsafe extern "system" fn add_ref(_: *mut c_void) -> u32 {
    FACTORY_REFS.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn release(_: *mut c_void) -> u32 {
    FACTORY_REFS.fetch_sub(1, Ordering::Release) - 1
}

unsafe extern "system" fn create_instance(
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
    let result = unsafe { inner_query_interface(inner, iid, output) };
    unsafe { inner_release(inner) };
    apo_log!(
        if result == S_OK {
            etw::level::INFO
        } else {
            etw::level::ERROR
        },
        etw::keyword::LIFECYCLE,
        "CreateInstance instance={object:p} aggregated={} result=0x{:08x}",
        !outer.is_null(),
        result.0 as u32
    );
    result
}

unsafe extern "system" fn lock_server(_: *mut c_void, lock: BOOL) -> HRESULT {
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
        QueryInterface: query_interface,
        AddRef: add_ref,
        Release: release,
    },
    CreateInstance: create_instance,
    LockServer: lock_server,
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
    if unsafe { *clsid } != APO_CLSID {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let result = unsafe { query_interface(&CLASS_FACTORY as *const _ as *mut c_void, iid, output) };
    apo_log!(
        if result == S_OK {
            etw::level::VERBOSE
        } else {
            etw::level::WARNING
        },
        etw::keyword::LIFECYCLE,
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
        && worker::active_count() == 0
    {
        etw::unregister();
        S_OK
    } else {
        S_FALSE
    }
}

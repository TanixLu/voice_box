//! Detached native workers that keep this DLL loaded until exit.
//! Callers never wait for worker termination.

use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::{AtomicU32, Ordering},
};

use windows::{
    Win32::{
        Foundation::{CloseHandle, FreeLibrary, HMODULE},
        System::{
            LibraryLoader::{
                FreeLibraryAndExitThread, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
                GetModuleHandleExW,
            },
            Threading::{CreateThread, SetThreadDescription, THREAD_CREATION_FLAGS},
        },
    },
    core::PCWSTR,
};
use windows_core::HSTRING;

use crate::etw;

static ACTIVE_WORKER_COUNT: AtomicU32 = AtomicU32::new(0);
pub fn get_active_worker_count() -> u32 {
    ACTIVE_WORKER_COUNT.load(Ordering::Acquire)
}
struct ActiveWorkerGuard;
impl ActiveWorkerGuard {
    pub fn enter() -> Self {
        let _ = ACTIVE_WORKER_COUNT.fetch_add(1, Ordering::AcqRel);
        Self
    }
}
impl Drop for ActiveWorkerGuard {
    fn drop(&mut self) {
        let _ = ACTIVE_WORKER_COUNT.fetch_sub(1, Ordering::AcqRel);
    }
}

struct DllRef(HMODULE);
impl DllRef {
    fn acquire() -> windows::core::Result<Self> {
        let mut module = HMODULE::default();
        // FROM_ADDRESS increments the reference count. Do not use PIN (which
        // leaks until process exit), or UNCHANGED_REFCOUNT (which cannot protect us).
        unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
                PCWSTR(thread_entry as *const () as *const u16),
                &mut module,
            )?;
        }
        Ok(Self(module))
    }
    fn into_raw(self) -> HMODULE {
        std::mem::ManuallyDrop::new(self).0
    }
}
impl Drop for DllRef {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeLibrary(self.0);
        }
    }
}

struct WorkerContext {
    action: Box<dyn FnOnce() + Send>,
    active_worker_guard: ActiveWorkerGuard,
    dll_ref: DllRef,
}

pub fn spawn(name: &str, action: impl FnOnce() + Send + 'static) -> windows::core::Result<()> {
    let worker_context_ptr = Box::into_raw(Box::new(WorkerContext {
        action: Box::new(action),
        active_worker_guard: ActiveWorkerGuard::enter(),
        dll_ref: DllRef::acquire()?,
    }));
    let create_result = unsafe {
        CreateThread(
            None,
            0,
            Some(thread_entry),
            Some(worker_context_ptr.cast()),
            THREAD_CREATION_FLAGS(0),
            None,
        )
    };
    let thread = create_result.inspect_err(|_| unsafe {
        drop(Box::from_raw(worker_context_ptr));
    })?;
    unsafe {
        let name = HSTRING::from(name);
        let _ = SetThreadDescription(thread, &name);
        let _ = CloseHandle(thread); // Detach; cleanup belongs to the worker.
    }
    Ok(())
}

unsafe extern "system" fn thread_entry(raw_worker_context_ptr: *mut c_void) -> u32 {
    let WorkerContext {
        action,
        active_worker_guard,
        dll_ref,
    } = unsafe { *Box::from_raw(raw_worker_context_ptr.cast::<WorkerContext>()) };
    {
        let action_result = catch_unwind(AssertUnwindSafe(action));
        if action_result.is_err() {
            etw::apo_log!(etw::ERROR, etw::LIFECYCLE, "APO background worker panicked");
        }
    }
    let dll_ref_handle = dll_ref.into_raw();
    // Stop counting this worker before checking whether the DLL is idle.
    drop(active_worker_guard);
    // Unregister ETW if no COM references or active workers remain.
    let _ = super::DllCanUnloadNow();
    // Release the DLL reference and exit without returning to DLL code.
    unsafe { FreeLibraryAndExitThread(dll_ref_handle, 0) }
}

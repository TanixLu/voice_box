//! Detached native workers with an owned DLL reference through their final exit.
//! No APO control callback waits for a APO configuration worker to exit.

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

use crate::etw;

static ACTIVE_WORKERS: AtomicU32 = AtomicU32::new(0);
pub fn active_count() -> u32 {
    ACTIVE_WORKERS.load(Ordering::Acquire)
}

struct ModuleRef(HMODULE);
impl ModuleRef {
    fn acquire() -> windows::core::Result<Self> {
        let mut module = HMODULE::default();
        // FROM_ADDRESS increments the reference count. Do not use PIN (which
        // leaks until process exit), or UNCHANGED_REFCOUNT (which cannot protect us).
        unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
                PCWSTR(entry as *const () as *const u16),
                &mut module,
            )?;
        }
        Ok(Self(module))
    }
    fn into_raw(self) -> HMODULE {
        std::mem::ManuallyDrop::new(self).0
    }
}
impl Drop for ModuleRef {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeLibrary(self.0);
        }
    }
}

struct WorkerContext {
    action: Option<Box<dyn FnOnce() + Send>>,
    module: Option<ModuleRef>,
}
impl Drop for WorkerContext {
    fn drop(&mut self) {
        ACTIVE_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

pub fn spawn(name: PCWSTR, action: impl FnOnce() + Send + 'static) -> windows::core::Result<()> {
    let module = ModuleRef::acquire()?;
    let action = Box::new(action);
    ACTIVE_WORKERS.fetch_add(1, Ordering::AcqRel);
    let start = Box::into_raw(Box::new(WorkerContext {
        action: Some(action),
        module: Some(module),
    }));
    let result = unsafe {
        CreateThread(
            None,
            0,
            Some(entry),
            Some(start.cast()),
            THREAD_CREATION_FLAGS(0),
            None,
        )
    };
    match result {
        Ok(thread) => unsafe {
            // Available since Windows 10 1607, below the existing 1803 minimum.
            let _ = SetThreadDescription(thread, name);
            let _ = CloseHandle(thread); // Detach; cleanup belongs to the worker.
            Ok(())
        },
        Err(error) => {
            unsafe {
                drop(Box::from_raw(start));
            }
            Err(error)
        }
    }
}

unsafe extern "system" fn entry(raw: *mut c_void) -> u32 {
    let mut start = unsafe { Box::from_raw(raw.cast::<WorkerContext>()) };
    let action = start
        .action
        .take()
        .expect("UNREACHABLE: worker context should contain an action");
    if catch_unwind(AssertUnwindSafe(action)).is_err() {
        etw::apo_log!(etw::ERROR, etw::LIFECYCLE, "APO background worker panicked");
    }
    let module = start
        .module
        .take()
        .expect("UNREACHABLE: worker context should contain a DLL reference")
        .into_raw();
    // All Rust values, buffers and counters are destroyed first.
    drop(start);
    // Clean up ETW too if the host already released its loader reference and
    // this worker is now the last owner. COM hosts otherwise do this themselves.
    let _ = super::DllCanUnloadNow();
    // Use a native thread rather than std::thread's trampoline: ExitThread must
    // not bypass the Rust join packet or leave any Rust destructors pending.
    unsafe { FreeLibraryAndExitThread(module, 0) }
}

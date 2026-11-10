//! Thin Win32 helpers shared by both targets. Names omit a `win_` prefix: the module says it.

use std::{
    ffi::OsStr,
    os::windows::ffi::OsStrExt,
    os::windows::io::{FromRawHandle, OwnedHandle},
    sync::OnceLock,
};

use windows::Win32::{
    Foundation::{ERROR_ALREADY_EXISTS, GetLastError},
    System::{Performance::QueryPerformanceFrequency, Threading::CreateMutexW},
};
use windows_core::PCWSTR;

pub fn wide_null(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

pub fn qpc_frequency() -> i64 {
    static CACHE: OnceLock<i64> = OnceLock::new();
    *CACHE.get_or_init(|| {
        let mut frequency = 0;
        unsafe {
            QueryPerformanceFrequency(&mut frequency)
                .unwrap_or_else(|e| panic!("Failed to query performance frequency: {}", e));
        }
        if frequency <= 0 {
            panic!("Failed to query performance frequency");
        }
        frequency
    })
}

/// Creates a named mutex and keeps it alive until the returned handle is dropped.
/// Returns `None` if it already exists. Uses object existence, not mutex ownership.
pub fn try_create_named_mutex(name: &str) -> windows_core::Result<Option<OwnedHandle>> {
    let name = wide_null(name);
    let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }?;
    // Read the status before another Win32 call can overwrite it.
    let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    // SAFETY: CreateMutexW returned a valid, uniquely owned handle.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
    if already_exists {
        Ok(None)
    } else {
        Ok(Some(handle))
    }
}

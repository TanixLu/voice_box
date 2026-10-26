//! ETW diagnostics for non-real-time APO paths.

use crate::shared::utils::wide_null;
use core::fmt;
use std::sync::atomic::{AtomicI64, Ordering};
use windows::Win32::System::Diagnostics::Etw::{
    EventProviderEnabled, EventRegister, EventUnregister, EventWriteString, REGHANDLE,
};
use windows_core::{GUID, PCWSTR};

pub const ERROR: u8 = 2;
pub const WARNING: u8 = 3;
pub const INFO: u8 = 4;
pub const VERBOSE: u8 = 5;
pub const LIFECYCLE: u64 = 1 << 0;
pub const FORMAT: u64 = 1 << 1;
pub const APO_CFG: u64 = 1 << 2;

const PROVIDER: GUID = GUID::from_u128(0xd75e13f3_18c2_45f2_9397_9a6641423354);
static REGISTRATION: AtomicI64 = AtomicI64::new(0);

fn register() -> REGHANDLE {
    let current = REGISTRATION.load(Ordering::Acquire);
    if current != 0 {
        return REGHANDLE(current);
    }

    let mut handle = REGHANDLE::default();
    if unsafe { EventRegister(&PROVIDER, None, None, &mut handle) } != 0 || handle.0 == 0 {
        return REGHANDLE(0);
    }
    match REGISTRATION.compare_exchange(0, handle.0, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => handle,
        Err(existing) => {
            unsafe { EventUnregister(handle) };
            REGHANDLE(existing)
        }
    }
}

pub fn unregister() {
    let handle = REGISTRATION.swap(0, Ordering::AcqRel);
    if handle != 0 {
        unsafe { EventUnregister(REGHANDLE(handle)) };
    }
}

/// Never call from `APOProcess`: formatting allocates and ETW enters the kernel.
pub fn log(level: u8, keyword: u64, arguments: fmt::Arguments<'_>) {
    let handle = register();
    if handle.0 == 0 || !unsafe { EventProviderEnabled(handle, level, keyword) } {
        return;
    }
    let message = wide_null(format!("VoiceBox APO: {arguments}"));
    unsafe { EventWriteString(handle, level, keyword, PCWSTR(message.as_ptr())) };
}

macro_rules! apo_log {
    ($level:expr, $keyword:expr, $($arg:tt)*) => {
        $crate::apo::etw::log($level, $keyword, format_args!($($arg)*))
    };
}

pub(crate) use apo_log;

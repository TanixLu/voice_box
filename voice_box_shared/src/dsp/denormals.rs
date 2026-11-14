//! Scoped x86-64 FTZ/DAZ policy for the application's audio processing calls.
//! This deliberately changes Rust's default floating-point environment; saving
//! and restoring MXCSR isolates the host but does not remove compiler assumptions.

use std::{arch::asm, marker::PhantomData};

const FTZ_DAZ: u32 = (1 << 15) | (1 << 6);

#[must_use]
pub(super) struct DenormalGuard {
    previous: u32,
    // MXCSR belongs to the current thread; the guard must be dropped there.
    _thread_bound: PhantomData<*mut ()>,
}

impl DenormalGuard {
    pub(super) fn new() -> Self {
        let mut previous = 0_u32;
        // SAFETY: both operands point to live u32 storage. x86-64 provides SSE2;
        // only FTZ/DAZ change, preserving rounding and exception-mask control.
        unsafe {
            asm!("stmxcsr [{state}]", state = in(reg) &mut previous, options(nostack, preserves_flags));
            let enabled = previous | FTZ_DAZ;
            asm!("ldmxcsr [{state}]", state = in(reg) &enabled, options(nostack, preserves_flags));
        }
        Self {
            previous,
            _thread_bound: PhantomData,
        }
    }
}

impl Drop for DenormalGuard {
    fn drop(&mut self) {
        // SAFETY: restore the exact state read on this thread, including when
        // leaving through an early return or unwinding in development builds.
        unsafe {
            asm!("ldmxcsr [{state}]", state = in(reg) &self.previous, options(nostack));
        }
    }
}

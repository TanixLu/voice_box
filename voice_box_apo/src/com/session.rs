//! Exclusive session access. Only control access can replace or reclaim resources.

use std::{
    cell::UnsafeCell,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use voice_box_shared::{apo_config::ApoConfig, dsp::AudioProcessor};

use crate::{memory::Resident, soundboard::Injection};

pub(super) struct ProcessingSession {
    dsp: Resident<AudioProcessor>,
    injection: Option<Injection>,
    max_input_frames: u32,
}

impl ProcessingSession {
    pub(super) fn new(
        dsp: Resident<AudioProcessor>,
        injection: Option<Injection>,
        max_input_frames: u32,
    ) -> Self {
        Self {
            dsp,
            injection,
            max_input_frames,
        }
    }

    pub(super) fn latency_hns(&self) -> i64 {
        self.dsp.latency_hns()
    }

    pub(super) fn cancellation(&self) -> Option<Arc<AtomicBool>> {
        self.injection.as_ref().map(Injection::cancellation)
    }
}

/// Nonblocking exclusive access to a processing session.
///
/// `accepting` permits processing; it does not describe callbacks in flight.
/// Deactivation does not wait for or revoke an admitted callback, so a stopped
/// cell may retain its session until the next installation or APO destruction.
///
/// All session access requires `busy`: acquiring it with Acquire observes the
/// previous owner's writes, published by the guard's Release on exit. The
/// Relaxed admission flag does not publish session data. Every access attempt
/// tries once without waiting; processing guards only release access, while
/// control guards install and reclaim resources off the real-time thread.
/// Control callers hold the APO control mutex first; processing never takes it.
pub(super) struct ProcessingCell {
    accepting: AtomicBool,
    busy: AtomicBool,
    session: UnsafeCell<Option<ProcessingSession>>,
}

impl ProcessingCell {
    pub(super) fn new() -> Self {
        Self {
            accepting: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            session: UnsafeCell::new(None),
        }
    }

    pub(super) fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::Relaxed)
    }

    /// Close the processing gate without waiting for an admitted callback.
    /// A stopped session may remain allocated until a later control call.
    pub(super) fn deactivate(&self) -> bool {
        self.accepting.swap(false, Ordering::Relaxed)
    }

    pub(super) fn try_process(&self) -> Option<ProcessGuard<'_>> {
        let access = self.try_access()?;
        if !self.is_accepting() {
            return None;
        }
        // Exclusive access lasts at least as long as the session borrow.
        let session = unsafe { &mut *self.session.get() }.as_mut()?;
        Some(ProcessGuard {
            session,
            _access: access,
        })
    }

    /// Non-RT callers hold the APO control mutex before taking session access.
    pub(super) fn try_control(&self) -> Option<ControlGuard<'_>> {
        self.try_access().map(|access| ControlGuard { access })
    }

    fn try_access(&self) -> Option<Access<'_>> {
        self.busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Access(self))
    }
}

struct Access<'a>(&'a ProcessingCell);

impl Drop for Access<'_> {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}

pub(super) struct ControlGuard<'a> {
    access: Access<'a>,
}

impl ControlGuard<'_> {
    /// Publish the complete session when this guard releases exclusive access.
    pub(super) fn install(self, session: ProcessingSession) {
        // Exclusive access is held until the complete session has been installed.
        unsafe { *self.access.0.session.get() = Some(session) };
        self.access.0.accepting.store(true, Ordering::Relaxed);
    }

    pub(super) fn clear(self) {
        // Reclaiming must leave the gate closed even if the session was active.
        self.access.0.accepting.store(false, Ordering::Relaxed);
        unsafe { *self.access.0.session.get() = None };
    }
}

pub(super) struct ProcessGuard<'a> {
    session: &'a mut ProcessingSession,
    _access: Access<'a>,
}

impl ProcessGuard<'_> {
    pub(super) fn buffer_bytes(&self, frames: u32) -> Option<usize> {
        (frames <= self.session.max_input_frames)
            .then(|| frames as usize * self.session.dsp.frame_bytes())
    }

    pub(super) fn silence_byte(&self) -> u8 {
        self.session.dsp.silence_byte()
    }

    /// Reset and process without transferring ownership of any session resource.
    /// Returns whether the effects chain ran; bypass preserves input buffer flags.
    pub(super) fn process(&mut self, buffer: &mut [u8], config: &ApoConfig, reset: bool) -> bool {
        let ProcessingSession { dsp, injection, .. } = &mut *self.session;
        if reset {
            dsp.reset(config);
            if let Some(injection) = injection {
                injection.reset();
            }
        }
        if dsp.is_bypassed() {
            return false;
        }
        let frames = buffer.len() / dsp.frame_bytes();
        let playback = injection
            .as_mut()
            .and_then(|injection| injection.read(frames));
        dsp.process_with_config(buffer, config, playback);
        true
    }
}

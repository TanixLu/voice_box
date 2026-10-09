//! One playback-reference worker per audiodg process, shared by its capture APOs.
//! Subscriptions release asynchronously; no COM/APO callback joins the worker.
use super::{
    etw, loopback,
    reference_buffer::{FAILED, Reference},
    worker,
};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU32, Ordering},
    },
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_FAILED},
        System::Threading::{CreateEventW, SetEvent, WaitForSingleObject},
    },
    core::w,
};

static SHARED: Mutex<Weak<Worker>> = Mutex::new(Weak::new());
const RETIRING: u32 = u32::MAX;

struct RegistryEntry<'a>(&'a Worker);
impl Drop for RegistryEntry<'_> {
    fn drop(&mut self) {
        let mut shared = SHARED.lock().unwrap_or_else(|error| error.into_inner());
        if std::ptr::eq(shared.as_ptr(), self.0) {
            // Release the static weak allocation before the DLL can unload.
            // A newer worker may already have replaced this registry entry.
            *shared = Weak::new();
        }
    }
}

/// Zero users can be revived until the worker commits to retiring. A committed
/// worker never accepts a new subscriber while its WASAPI resources are closing.
struct Users(AtomicU32);
impl Users {
    fn acquire(&self) -> bool {
        let mut count = self.0.load(Ordering::Acquire);
        while count < RETIRING - 1 {
            match self.0.compare_exchange_weak(
                count,
                count + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(current) => count = current,
            }
        }
        false
    }
    fn release(&self) -> bool {
        let previous = self.0.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0 && previous != RETIRING);
        previous == 1
    }
    fn retire(&self) -> bool {
        match self
            .0
            .compare_exchange(0, RETIRING, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => true,
            Err(count) => count == RETIRING,
        }
    }
}

struct Wake(HANDLE);
unsafe impl Send for Wake {}
unsafe impl Sync for Wake {}
impl Drop for Wake {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub(super) struct Worker {
    pub(super) reference: Reference,
    users: Users,
    wake: Wake,
}
impl Worker {
    fn new() -> windows::core::Result<Self> {
        Ok(Self {
            reference: Reference::new(),
            users: Users(AtomicU32::new(1)),
            wake: Wake(unsafe { CreateEventW(None, false, false, None)? }),
        })
    }
    pub(super) fn retiring(&self) -> bool {
        self.users.retire()
    }
    pub(super) fn wake_handle(&self) -> HANDLE {
        self.wake.0
    }
    pub(super) fn wait(&self, milliseconds: u32) -> windows::core::Result<()> {
        if unsafe { WaitForSingleObject(self.wake.0, milliseconds) } == WAIT_FAILED {
            Err(windows::core::Error::from_thread())
        } else {
            Ok(())
        }
    }
    fn run(&self) {
        let _registry_entry = RegistryEntry(self);
        let mut last_error = None;
        while !self.retiring() {
            // A transient COM/endpoint initialization error must not leave live
            // subscribers pointing to a dead worker. Unwind-mode builds also
            // recover after unwinding the current COM session on this thread.
            let result = catch_unwind(AssertUnwindSafe(|| loopback::run(self)));
            let error = match result {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(format!("Reference worker will retry: {error:#}")),
                Err(_) => Some("Reference worker panicked; retrying".to_owned()),
            };
            if let Some(error) = error {
                self.reference.restart(FAILED, 3);
                if last_error.as_ref() != Some(&error) {
                    etw::apo_log!(etw::WARNING, etw::LIFECYCLE, "{error}");
                    last_error = Some(error);
                }
            }
            if !self.retiring() {
                let _ = self.wait(500);
            }
        }
    }
}

pub(super) struct Subscription {
    worker: Arc<Worker>,
}
impl Subscription {
    pub fn acquire() -> windows::core::Result<Self> {
        let mut shared = SHARED.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(worker) = shared.upgrade()
            && worker.users.acquire()
        {
            return Ok(Self { worker });
        }
        let worker = Arc::new(Worker::new()?);
        let owned = Arc::clone(&worker);
        // The registry mutex never crosses any WASAPI or COM call. Only the
        // detached worker calls those APIs, without consulting this registry.
        worker::spawn(w!("VoiceBoxLoopback"), move || owned.run())?;
        *shared = Arc::downgrade(&worker);
        Ok(Self { worker })
    }
    pub fn reference(&self) -> &Reference {
        &self.worker.reference
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        if self.worker.users.release() {
            unsafe {
                let _ = SetEvent(self.worker.wake.0);
            }
        }
        // The worker retains its own Arc and DLL reference through COM cleanup.
    }
}

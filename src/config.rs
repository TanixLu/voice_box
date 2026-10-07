//! Registry notifications and effect events stay off the real-time thread.
use crate::settings::{PATH, Settings};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::thread::JoinHandle;
use windows::Win32::{
    Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, WAIT_OBJECT_0},
    System::{
        Registry::{
            HKEY, REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_SECURITY,
            REG_NOTIFY_THREAD_AGNOSTIC, RegNotifyChangeKeyValue,
        },
        Threading::{
            CreateEventW, GetCurrentProcess, INFINITE, ResetEvent, SetEvent,
            WaitForMultipleObjects, WaitForSingleObject,
        },
    },
};
use winreg::{
    RegKey,
    enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY},
};

pub struct RtParams {
    bits: AtomicU64,
    // Only configuration and COM discovery touch this mutex. RT loads bits alone.
    effects: Mutex<[Option<Event>; 2]>,
}
impl Default for RtParams {
    fn default() -> Self {
        Self {
            bits: AtomicU64::new((1u64 << 32) | 1.0_f32.to_bits() as u64),
            effects: Mutex::new([None, None]),
        }
    }
}
impl RtParams {
    pub(crate) fn store(&self, settings: Settings) {
        let gain = 10.0_f32.powf(settings.gain_millidb as f32 / 20_000.0);
        let bits = ((settings.enabled as u64) << 32) | gain.to_bits() as u64;
        // Subscription plus its returned snapshot must be serialized with updates.
        let effects = self
            .effects
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let previous = self.bits.swap(bits, Ordering::AcqRel);
        if previous >> 32 != bits >> 32 {
            for event in effects.iter().flatten() {
                unsafe {
                    let _ = SetEvent(event.handle());
                }
            }
        }
    }
    pub fn load(&self) -> (bool, f32) {
        let bits = self.bits.load(Ordering::Acquire);
        (bits >> 32 != 0, f32::from_bits(bits as u32))
    }

    pub fn subscribe_effects(
        &self,
        event: HANDLE,
        controllable: bool,
    ) -> windows::core::Result<bool> {
        let event = if event.0.is_null() {
            None
        } else {
            let mut duplicate = HANDLE::default();
            unsafe {
                let process = GetCurrentProcess();
                DuplicateHandle(
                    process,
                    event,
                    process,
                    &mut duplicate,
                    0,
                    false,
                    DUPLICATE_SAME_ACCESS,
                )?;
            }
            Some(Event(duplicate))
        };
        let mut effects = self
            .effects
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        effects[usize::from(controllable)] = event;
        Ok(self.load().0)
    }
}

struct Event(HANDLE);
impl Event {
    fn handle(&self) -> HANDLE {
        self.0
    }
}
// Owned kernel event handles may be signalled and waited from different threads.
unsafe impl Send for Event {}
unsafe impl Sync for Event {}
impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub struct ConfigWorker {
    stop: Arc<Event>,
    thread: Option<JoinHandle<()>>,
}
impl ConfigWorker {
    pub fn start(params: Arc<RtParams>) -> windows::core::Result<Self> {
        if let Ok(settings) = Settings::try_load() {
            params.store(settings);
        }
        let stop = Arc::new(Event(unsafe { CreateEventW(None, true, false, None)? }));
        let changed = Event(unsafe { CreateEventW(None, false, false, None)? });
        let worker_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("VoiceBoxConfig".into())
            .spawn(move || watch(&worker_stop, &changed, &params))
            .map_err(|error| {
                windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    format!("Cannot start configuration watcher: {error}"),
                )
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for ConfigWorker {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(self.stop.handle());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn arm(key: &RegKey, changed: &Event) -> anyhow::Result<()> {
    unsafe {
        RegNotifyChangeKeyValue(
            HKEY(key.raw_handle().cast()),
            false,
            REG_NOTIFY_CHANGE_LAST_SET | REG_NOTIFY_CHANGE_SECURITY | REG_NOTIFY_THREAD_AGNOSTIC,
            Some(changed.handle()),
            true,
        )
        .ok()?;
    }
    Ok(())
}

fn open_watch(changed: &Event) -> anyhow::Result<(RegKey, Settings)> {
    // Closing an old key can signal its event; discard that stale notification.
    unsafe {
        ResetEvent(changed.handle())?;
    }
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(PATH, KEY_READ | KEY_WOW64_64KEY)?;
    arm(&key, changed)?;
    // Arm first so a commit during this read also produces a later notification.
    let settings = Settings::read(&key)?;
    Ok((key, settings))
}

fn watch(stop: &Event, changed: &Event, params: &RtParams) {
    let events = [stop.handle(), changed.handle()];
    let mut key = None;
    let mut retry_ms = 250u32;
    let mut faulted = false;
    loop {
        if key.is_none() {
            if unsafe { WaitForSingleObject(stop.handle(), 0) } == WAIT_OBJECT_0 {
                break;
            }
            match open_watch(changed) {
                Ok((opened, settings)) => {
                    params.store(settings);
                    key = Some(opened);
                    retry_ms = 250;
                    if faulted {
                        crate::etw::apo_log!(
                            crate::etw::INFO,
                            crate::etw::CONFIGURATION,
                            "Configuration watcher recovered"
                        );
                        faulted = false;
                    }
                }
                Err(error) => {
                    report_fault(&mut faulted, &error);
                    // Only failed subscriptions retry. Normal operation waits indefinitely.
                    // Ignore cancelled registry events here to avoid a busy retry loop.
                    if unsafe { WaitForSingleObject(stop.handle(), retry_ms) } == WAIT_OBJECT_0 {
                        break;
                    }
                    retry_ms = (retry_ms * 2).min(5_000);
                    continue;
                }
            }
        }
        let result = unsafe { WaitForMultipleObjects(&events, false, INFINITE) };
        if result == WAIT_OBJECT_0 {
            break;
        }
        if result.0 == WAIT_OBJECT_0.0 + 1 {
            let current = key.as_ref().expect("subscription exists while waiting");
            // Each completed notification has exactly one replacement subscription.
            match arm(current, changed).and_then(|_| Ok(Settings::read(current)?)) {
                Ok(settings) => params.store(settings),
                Err(error) => {
                    report_fault(&mut faulted, &error);
                    key = None; // closes and cancels any outstanding notification
                }
            }
        } else {
            report_fault(
                &mut faulted,
                &anyhow::anyhow!("Registry wait failed: {result:?}"),
            );
            key = None;
            if unsafe { WaitForSingleObject(stop.handle(), retry_ms) } == WAIT_OBJECT_0 {
                break;
            }
            retry_ms = (retry_ms * 2).min(5_000);
        }
    }
}

fn report_fault(faulted: &mut bool, error: &anyhow::Error) {
    if !*faulted {
        crate::etw::apo_log!(
            crate::etw::WARNING,
            crate::etw::CONFIGURATION,
            "Configuration watcher will retry; retaining last valid settings: {error:#}"
        );
        *faulted = true;
    }
}

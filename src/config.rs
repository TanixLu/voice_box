//! Registry notifications stay on an owned worker; RT loads one atomic snapshot.
use crate::settings::{PATH, Settings};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::thread::JoinHandle;
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
    System::{
        Registry::{
            HKEY, REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_THREAD_AGNOSTIC, RegNotifyChangeKeyValue,
        },
        Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects},
    },
};
use winreg::{
    RegKey,
    enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY},
};

pub struct RtParams(AtomicU64);
impl Default for RtParams {
    fn default() -> Self {
        Self(AtomicU64::new((1u64 << 32) | 1.0_f32.to_bits() as u64))
    }
}
impl RtParams {
    fn store(&self, settings: Settings) {
        self.0.store(
            ((settings.enabled as u64) << 32) | settings.gain_linear().to_bits() as u64,
            Ordering::Release,
        );
    }
    pub fn load(&self) -> (bool, f32) {
        let bits = self.0.load(Ordering::Acquire);
        (bits >> 32 != 0, f32::from_bits(bits as u32))
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
    pub fn start(params: Arc<RtParams>) -> windows::core::Result<Option<Self>> {
        params.store(Settings::load());
        let Ok(key) = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey_with_flags(PATH, KEY_READ | KEY_WOW64_64KEY)
        else {
            return Ok(None);
        };
        Self::watch(key, params).map(Some)
    }
    fn watch(key: RegKey, params: Arc<RtParams>) -> windows::core::Result<Self> {
        let stop = Arc::new(Event(unsafe { CreateEventW(None, true, false, None)? }));
        let changed = Event(unsafe { CreateEventW(None, false, false, None)? });
        // Arm before the initial read so changes during initialization are retained.
        unsafe {
            RegNotifyChangeKeyValue(
                HKEY(key.raw_handle().cast()),
                false,
                REG_NOTIFY_CHANGE_LAST_SET | REG_NOTIFY_THREAD_AGNOSTIC,
                Some(changed.0),
                true,
            )
            .ok()?;
        }
        params.store(Settings::read(&key));
        let worker_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("VoiceBoxConfig".into())
            .spawn(move || {
                let events = [worker_stop.handle(), changed.handle()];
                loop {
                    let result = unsafe { WaitForMultipleObjects(&events, false, INFINITE) };
                    if result.0 != WAIT_OBJECT_0.0 + 1 {
                        break;
                    }
                    // Rearm before reading, avoiding a lost update between read and arm.
                    if unsafe {
                        RegNotifyChangeKeyValue(
                            HKEY(key.raw_handle().cast()),
                            false,
                            REG_NOTIFY_CHANGE_LAST_SET | REG_NOTIFY_THREAD_AGNOSTIC,
                            Some(changed.handle()),
                            true,
                        )
                        .ok()
                    }
                    .is_err()
                    {
                        break;
                    }
                    params.store(Settings::read(&key));
                }
            })
            .map_err(|_| {
                windows::core::Error::from_hresult(windows::Win32::Foundation::E_OUTOFMEMORY)
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
            let _ = SetEvent(self.stop.0);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

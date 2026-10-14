//! Settings file notifications and effect events stay off the real-time thread.
use super::{etw, worker};
use crate::shared::{paths, settings::Settings, utils::wide_null};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use windows::Win32::{
    Foundation::{
        CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Storage::FileSystem::{
        FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SECURITY,
        FILE_NOTIFY_CHANGE_SIZE, FindCloseChangeNotification, FindFirstChangeNotificationW,
        FindNextChangeNotification,
    },
    System::Threading::{
        CreateEventW, GetCurrentProcess, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
    },
};
use windows::core::{PCWSTR, w};

const ENABLED_BIT: u64 = 1 << 32;

pub struct RtParams {
    bits: AtomicU64,
    // Only settings updates and COM discovery touch this mutex. RT loads bits alone.
    effects: Mutex<[Option<Event>; 2]>,
}
impl Default for RtParams {
    fn default() -> Self {
        Self {
            bits: AtomicU64::new(ENABLED_BIT | u64::from(1.0_f32.to_bits())),
            effects: Mutex::new([None, None]),
        }
    }
}
impl RtParams {
    pub(crate) fn store(&self, settings: Settings) {
        let gain = settings.linear_gain();
        let bits = if settings.enable_denoising {
            ENABLED_BIT
        } else {
            0
        } | u64::from(gain.to_bits());
        // Subscription plus its returned snapshot must be serialized with updates.
        let effects = self
            .effects
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let previous = self.bits.swap(bits, Ordering::AcqRel);
        let was_enabled = previous & ENABLED_BIT != 0;
        if was_enabled != settings.enable_denoising {
            for event in effects.iter().flatten() {
                unsafe {
                    let _ = SetEvent(event.handle());
                }
            }
        }
    }
    pub fn load(&self) -> (bool, f32) {
        let bits = self.bits.load(Ordering::Acquire);
        (bits & ENABLED_BIT != 0, f32::from_bits(bits as u32))
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

pub struct SettingsWatcher {
    stop: Arc<Event>,
}
impl SettingsWatcher {
    pub fn start(params: Arc<RtParams>) -> windows::core::Result<Self> {
        if let Ok(settings) = Settings::try_load() {
            params.store(settings);
        }
        let stop = Arc::new(Event(unsafe { CreateEventW(None, true, false, None)? }));
        let worker_stop = Arc::clone(&stop);
        worker::spawn(w!("VoiceBoxSettings"), move || watch(&worker_stop, &params))?;
        Ok(Self { stop })
    }
}
impl Drop for SettingsWatcher {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(self.stop.handle());
        }
        // The detached thread retains params and its DLL reference until exit.
        // Never join while an audio-engine control callback may hold a lock.
    }
}

struct DirectoryWatch(HANDLE);
impl DirectoryWatch {
    fn open() -> anyhow::Result<Self> {
        let path = wide_null(paths::data_dir()?);
        let handle = unsafe {
            FindFirstChangeNotificationW(
                PCWSTR(path.as_ptr()),
                false,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_LAST_WRITE
                    | FILE_NOTIFY_CHANGE_SIZE
                    | FILE_NOTIFY_CHANGE_SECURITY,
            )?
        };
        Ok(Self(handle))
    }
    fn rearm(&self) -> anyhow::Result<()> {
        unsafe {
            FindNextChangeNotification(self.0)?;
        }
        Ok(())
    }
}
impl Drop for DirectoryWatch {
    fn drop(&mut self) {
        unsafe {
            let _ = FindCloseChangeNotification(self.0);
        }
    }
}

fn report_fault(faulted: &mut bool, error: &anyhow::Error) {
    if !*faulted {
        etw::apo_log!(
            etw::WARNING,
            etw::SETTINGS,
            "Settings file watcher will retry; retaining last valid settings: {error:#}"
        );
        *faulted = true;
    }
}

fn reload(params: &RtParams, faulted: &mut bool) {
    match Settings::try_load() {
        Ok(settings) => {
            params.store(settings);
            if *faulted {
                etw::apo_log!(etw::INFO, etw::SETTINGS, "Settings file watcher recovered");
                *faulted = false;
            }
        }
        Err(error) => report_fault(faulted, &error.into()),
    }
}

fn watch(stop: &Event, params: &RtParams) {
    let mut watch: Option<DirectoryWatch> = None;
    let mut retry_ms = 250;
    let mut faulted = false;
    loop {
        if unsafe { WaitForSingleObject(stop.handle(), 0) } == WAIT_OBJECT_0 {
            break;
        }
        if watch.is_none() {
            match DirectoryWatch::open() {
                Ok(opened) => {
                    // Subscribe before reading so a concurrent replacement is not lost.
                    watch = Some(opened);
                    reload(params, &mut faulted);
                    retry_ms = 250;
                }
                Err(error) => {
                    report_fault(&mut faulted, &error);
                    if unsafe { WaitForSingleObject(stop.handle(), retry_ms) } == WAIT_OBJECT_0 {
                        break;
                    }
                    retry_ms = (retry_ms * 2).min(5_000);
                    continue;
                }
            }
        }
        let current = watch.as_ref().expect("directory subscription exists");
        let events = [stop.handle(), current.0];
        // Directory notifications handle writes and atomic replacements. A slow
        // health check also recovers if the whole directory is deleted/recreated.
        let timeout = if faulted { retry_ms } else { 30_000 };
        let result = unsafe { WaitForMultipleObjects(&events, false, timeout) };
        if result == WAIT_OBJECT_0 {
            break;
        }
        if result.0 == WAIT_OBJECT_0.0 + 1 {
            // Rearm before loading so another save during the read is observed.
            if let Err(error) = current.rearm() {
                report_fault(&mut faulted, &error);
                watch = None;
                continue;
            }
            reload(params, &mut faulted);
        } else if result == WAIT_TIMEOUT {
            if faulted {
                reload(params, &mut faulted);
            } else {
                watch = None;
            }
        } else {
            report_fault(
                &mut faulted,
                &anyhow::anyhow!("Settings directory wait failed: {result:?}"),
            );
            watch = None;
            if unsafe { WaitForSingleObject(stop.handle(), retry_ms) } == WAIT_OBJECT_0 {
                break;
            }
        }
        retry_ms = if faulted {
            (retry_ms * 2).min(5_000)
        } else {
            250
        };
    }
}

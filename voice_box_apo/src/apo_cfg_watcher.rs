//! APO configuration file notifications and effect events stay off the real-time thread.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU16, Ordering},
};

use windows::{
    Win32::{
        Foundation::{
            CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, WAIT_OBJECT_0,
            WAIT_TIMEOUT,
        },
        Storage::FileSystem::{
            FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
            FILE_NOTIFY_CHANGE_SECURITY, FILE_NOTIFY_CHANGE_SIZE, FindCloseChangeNotification,
            FindFirstChangeNotificationW, FindNextChangeNotification,
        },
        System::Threading::{
            CreateEventW, GetCurrentProcess, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
        },
    },
    core::{PCWSTR, w},
};

use voice_box_shared::{
    apo_cfg::{ApoCfg, GainDb},
    utils::{data_dir, wide_null},
};

use crate::{etw, worker};

const DENOISING_ENABLED_MASK: u16 = 0x0100;
const GAIN_DB_MASK: u16 = 0x00FF;

fn apo_cfg_to_bits(apo_config: ApoCfg) -> u16 {
    (if apo_config.denoising_enabled {
        DENOISING_ENABLED_MASK
    } else {
        0
    }) | (apo_config.gain_db.as_i8() as u16 & GAIN_DB_MASK)
}

fn bits_to_apo_cfg(bits: u16) -> ApoCfg {
    ApoCfg {
        denoising_enabled: (bits & DENOISING_ENABLED_MASK) != 0,
        gain_db: GainDb::new((bits & GAIN_DB_MASK) as i8),
    }
}

pub struct RtConfig {
    atomic_bits: AtomicU16,
    // Only APO configuration updates and COM discovery touch this mutex. RT loads bits alone.
    effects: Mutex<[Option<Event>; 2]>,
}

impl Default for RtConfig {
    fn default() -> Self {
        Self {
            atomic_bits: AtomicU16::new(apo_cfg_to_bits(ApoCfg::default())),
            effects: Mutex::new([None, None]),
        }
    }
}
impl RtConfig {
    pub fn load(&self) -> ApoCfg {
        bits_to_apo_cfg(self.atomic_bits.load(Ordering::Acquire))
    }
    pub fn store(&self, apo_config: ApoCfg) {
        // Subscription plus its returned snapshot must be serialized with updates.
        let effects = self
            .effects
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let previous = self
            .atomic_bits
            .swap(apo_cfg_to_bits(apo_config.clone()), Ordering::AcqRel);
        let was_enabled = (previous & DENOISING_ENABLED_MASK) != 0;
        if was_enabled != apo_config.denoising_enabled {
            for event in effects.iter().flatten() {
                unsafe {
                    let _ = SetEvent(event.handle());
                }
            }
        }
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
        Ok(self.load().denoising_enabled)
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

pub struct ApoCfgWatcher {
    stop: Arc<Event>,
}
impl ApoCfgWatcher {
    pub fn start(config: Arc<RtConfig>) -> windows::core::Result<Self> {
        if let Ok(cfg) = ApoCfg::read() {
            config.store(cfg);
        }
        let stop = Arc::new(Event(unsafe { CreateEventW(None, true, false, None)? }));
        let worker_stop = Arc::clone(&stop);
        worker::spawn(w!("VoiceBoxApoCfg"), move || watch(&worker_stop, &config))?;
        Ok(Self { stop })
    }
}
impl Drop for ApoCfgWatcher {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(self.stop.handle());
        }
        // The detached thread retains config and its DLL reference until exit.
        // Never join while an audio-engine control callback may hold a lock.
    }
}

struct DirectoryWatch(HANDLE);
impl DirectoryWatch {
    fn open() -> anyhow::Result<Self> {
        let path = wide_null(data_dir());
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
            etw::APO_CFG,
            "APO configuration file watcher will retry; retaining last valid APO configuration: {error:#}"
        );
        *faulted = true;
    }
}

fn reload(config: &RtConfig, faulted: &mut bool) {
    match ApoCfg::read() {
        Ok(cfg) => {
            config.store(cfg);
            if *faulted {
                etw::apo_log!(
                    etw::INFO,
                    etw::APO_CFG,
                    "APO configuration file watcher recovered"
                );
                *faulted = false;
            }
        }
        Err(error) => report_fault(faulted, &error.into()),
    }
}

fn watch(stop: &Event, config: &RtConfig) {
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
                    reload(config, &mut faulted);
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
        let current = watch
            .as_ref()
            .expect("UNREACHABLE: directory subscription should exist");
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
            reload(config, &mut faulted);
        } else if result == WAIT_TIMEOUT {
            if faulted {
                reload(config, &mut faulted);
            } else {
                watch = None;
            }
        } else {
            report_fault(
                &mut faulted,
                &anyhow::anyhow!("APO configuration directory wait failed: {result:?}"),
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

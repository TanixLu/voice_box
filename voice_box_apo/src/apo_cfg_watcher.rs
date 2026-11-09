//! APO configuration registry notifications and effect events stay off the real-time thread.

use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicU32, Ordering},
};

use windows::{
    Win32::{
        Foundation::{
            CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0,
        },
        System::{
            Registry::{
                HKEY, HKEY_LOCAL_MACHINE, KEY_NOTIFY, REG_NOTIFY_CHANGE_LAST_SET, RegCloseKey,
                RegNotifyChangeKeyValue, RegOpenKeyExW,
            },
            Threading::{
                CreateEventW, GetCurrentProcess, INFINITE, SetEvent, WaitForMultipleObjects,
            },
        },
    },
    core::HSTRING,
};

use voice_box_shared::{
    apo_cfg::{APO_CFG_KEY, ApoCfg},
    utils::GainDb,
};

use crate::{etw, worker};

const DENOISING_ENABLED_MASK: u32 = 0x0100;
const GAIN_DB_MASK: u32 = 0x00FF;
const INCLUDE_MICROPHONE_MASK: u32 = 1 << 9;

fn apo_cfg_to_bits(apo_config: ApoCfg) -> u32 {
    (if apo_config.denoising_enabled {
        DENOISING_ENABLED_MASK
    } else {
        0
    }) | (apo_config.gain_db.as_i8() as u32 & GAIN_DB_MASK)
        | if apo_config.playback_include_microphone_audio {
            INCLUDE_MICROPHONE_MASK
        } else {
            0
        }
}

fn bits_to_apo_cfg(bits: u32) -> ApoCfg {
    ApoCfg {
        denoising_enabled: (bits & DENOISING_ENABLED_MASK) != 0,
        gain_db: GainDb::new((bits & GAIN_DB_MASK) as i8),
        playback_include_microphone_audio: (bits & INCLUDE_MICROPHONE_MASK) != 0,
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

pub struct GetEffectsListEventSlots {
    get_effects_list_event: Option<Event>,
    get_controllable_effects_list_event: Option<Event>,
}

pub struct RtConfig {
    atomic_bits: AtomicU32,
    // Only APO configuration updates and COM discovery touch this mutex. RT loads bits alone.
    get_effects_list_event_slots: Mutex<GetEffectsListEventSlots>,
}

impl Default for RtConfig {
    fn default() -> Self {
        Self {
            atomic_bits: AtomicU32::new(apo_cfg_to_bits(ApoCfg::default())),
            get_effects_list_event_slots: Mutex::new(GetEffectsListEventSlots {
                get_effects_list_event: None,
                get_controllable_effects_list_event: None,
            }),
        }
    }
}
impl RtConfig {
    fn lock_slots(&self) -> MutexGuard<'_, GetEffectsListEventSlots> {
        self.get_effects_list_event_slots
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn load(&self) -> ApoCfg {
        // The entire RT snapshot fits in one atomic; subscriptions use lock_slots.
        bits_to_apo_cfg(self.atomic_bits.load(Ordering::Relaxed))
    }
    pub fn store(&self, apo_config: ApoCfg) {
        // Subscription plus its returned snapshot must be serialized with updates.
        let slots = self.lock_slots();
        let previous = self
            .atomic_bits
            .swap(apo_cfg_to_bits(apo_config.clone()), Ordering::Relaxed);
        let denoising_was_enabled = (previous & DENOISING_ENABLED_MASK) != 0;
        if denoising_was_enabled != apo_config.denoising_enabled {
            unsafe {
                if let Some(event) = &slots.get_effects_list_event {
                    let _ = SetEvent(event.handle());
                }
                if let Some(event) = &slots.get_controllable_effects_list_event {
                    let _ = SetEvent(event.handle());
                }
            }
        }
    }

    fn subscribe_effects_list_aux(
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
        let mut slots = self.lock_slots();
        if controllable {
            slots.get_controllable_effects_list_event = event;
        } else {
            slots.get_effects_list_event = event;
        }
        Ok(self.load().denoising_enabled)
    }

    pub fn subscribe_effects_list(&self, event: HANDLE) -> windows::core::Result<bool> {
        self.subscribe_effects_list_aux(event, false)
    }

    pub fn subscribe_controllable_effects_list(
        &self,
        event: HANDLE,
    ) -> windows::core::Result<bool> {
        self.subscribe_effects_list_aux(event, true)
    }

    pub fn clear_effects_events(&self) {
        // store() holds this same lock while signalling, so no notification can
        // be sent through these subscriptions after they have been cleared.
        let mut slots = self.lock_slots();
        slots.get_effects_list_event = None;
        slots.get_controllable_effects_list_event = None;
    }
}

pub struct ApoCfgWatcher {
    stop: Arc<Event>,
}
impl ApoCfgWatcher {
    pub fn start(config: Arc<RtConfig>) -> windows::core::Result<Self> {
        if let Ok(cfg) = ApoCfg::load() {
            config.store(cfg);
        }
        let stop = Arc::new(Event(unsafe { CreateEventW(None, true, false, None)? }));
        let changed = Event(unsafe { CreateEventW(None, false, false, None)? });
        let worker_stop = Arc::clone(&stop);
        worker::spawn("VoiceBoxApoCfgWatcher", move || {
            run(&worker_stop, &changed, &config)
        })?;
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

/// The key is missing until the GUI first saves, so it is retried until it can be watched.
const RETRY_INTERVAL_MS: u32 = 2_000;

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn open_key() -> Option<Key> {
    let mut key = HKEY::default();
    let path = HSTRING::from(APO_CFG_KEY);
    unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, &path, None, KEY_NOTIFY, &mut key) }
        .ok()
        .ok()?;
    Some(Key(key))
}

fn run(stop: &Event, changed: &Event, config: &RtConfig) {
    let mut key = None;
    let mut faulted = false;
    loop {
        // Arm the notification before reading so a concurrent save is not lost.
        key = key.or_else(open_key).filter(|key: &Key| {
            let filter = REG_NOTIFY_CHANGE_LAST_SET;
            unsafe { RegNotifyChangeKeyValue(key.0, false, filter, Some(changed.handle()), true) }
                .is_ok()
        });
        match ApoCfg::load() {
            Ok(cfg) => {
                config.store(cfg);
                log_status_change(&mut faulted, None);
            }
            Err(error) => log_status_change(&mut faulted, Some(&error)),
        }
        let timeout = if key.is_some() {
            INFINITE
        } else {
            RETRY_INTERVAL_MS
        };
        let handles = [stop.handle(), changed.handle()];
        match unsafe { WaitForMultipleObjects(&handles, false, timeout) } {
            WAIT_OBJECT_0 => break,
            WAIT_FAILED => {
                let error = windows::core::Error::from_thread();
                etw::apo_log!(
                    etw::ERROR,
                    etw::APO_CFG,
                    "APO configuration wait failed; watcher exiting: {error}"
                );
                break;
            }
            _ => {}
        }
    }
}

fn log_status_change(faulted: &mut bool, error: Option<&anyhow::Error>) {
    if *faulted == error.is_some() {
        return;
    }
    *faulted = error.is_some();
    if let Some(error) = error {
        etw::apo_log!(
            etw::WARNING,
            etw::APO_CFG,
            "APO configuration reload failed; will retry: {error:#}"
        );
    } else {
        etw::apo_log!(
            etw::INFO,
            etw::APO_CFG,
            "APO configuration reload recovered"
        );
    }
}

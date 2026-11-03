//! APO configuration file notifications and effect events stay off the real-time thread.

use std::{
    os::windows::ffi::OsStrExt,
    path::Path,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU16, Ordering},
    },
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{
            CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_NOTIFY_ENUM_DIR, HANDLE,
            WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
            FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
            FILE_NOTIFY_CHANGE_SECURITY, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesW,
        },
        System::{
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
            Threading::{
                CreateEventW, GetCurrentProcess, ResetEvent, SetEvent, WaitForMultipleObjects,
                WaitForSingleObject,
            },
        },
    },
    core::PCWSTR,
};

use voice_box_shared::{
    apo_cfg::{ApoCfg, GainDb},
    utils::{apo_cfg_path, data_dir, wide_null},
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
    atomic_bits: AtomicU16,
    // Only APO configuration updates and COM discovery touch this mutex. RT loads bits alone.
    get_effects_list_event_slots: Mutex<GetEffectsListEventSlots>,
}

impl Default for RtConfig {
    fn default() -> Self {
        Self {
            atomic_bits: AtomicU16::new(apo_cfg_to_bits(ApoCfg::default())),
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
        bits_to_apo_cfg(self.atomic_bits.load(Ordering::Acquire))
    }
    pub fn store(&self, apo_config: ApoCfg) {
        // Subscription plus its returned snapshot must be serialized with updates.
        let slots = self.lock_slots();
        let previous = self
            .atomic_bits
            .swap(apo_cfg_to_bits(apo_config.clone()), Ordering::AcqRel);
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
        worker::spawn("VoiceBoxApoCfgWatcher", move || watch(worker_stop, config))?;
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

struct DirectoryWatch {
    directory: HANDLE,
    event: Event,
    // Both addresses must remain stable while the asynchronous read is pending.
    overlapped: Box<OVERLAPPED>,
    // ReadDirectoryChangesW requires DWORD alignment.
    buffer: Box<[u32; 4096]>,
    file_name: Vec<u16>,
    pending: bool,
}
impl DirectoryWatch {
    fn open() -> anyhow::Result<Self> {
        Self::open_at(data_dir())
    }
    fn open_at(directory: &Path) -> anyhow::Result<Self> {
        let path = wide_null(directory);
        let event = Event(unsafe { CreateEventW(None, true, false, None)? });
        let directory = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                None,
            )?
        };
        let mut watch = Self {
            directory,
            overlapped: Box::new(OVERLAPPED {
                hEvent: event.handle(),
                ..Default::default()
            }),
            event,
            buffer: Box::new([0; 4096]),
            file_name: apo_cfg_path()
                .file_name()
                .expect("UNREACHABLE: APO configuration path must have a file name")
                .encode_wide()
                .collect(),
            pending: false,
        };
        watch.rearm()?;
        Ok(watch)
    }
    fn rearm(&mut self) -> anyhow::Result<()> {
        debug_assert!(!self.pending);
        unsafe {
            ResetEvent(self.event.handle())?;
            ReadDirectoryChangesW(
                self.directory,
                self.buffer.as_mut_ptr().cast(),
                size_of_val(self.buffer.as_ref()) as u32,
                false,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_LAST_WRITE
                    | FILE_NOTIFY_CHANGE_SIZE
                    | FILE_NOTIFY_CHANGE_SECURITY,
                None,
                Some(self.overlapped.as_mut()),
                None,
            )?;
        }
        self.pending = true;
        Ok(())
    }
    fn config_changed(&mut self) -> anyhow::Result<bool> {
        let mut bytes = 0;
        let result = unsafe {
            GetOverlappedResult(self.directory, self.overlapped.as_ref(), &mut bytes, true)
        };
        // Called only after the completion event is signalled.
        self.pending = false;
        let changed = match result {
            Ok(()) => {
                anyhow::ensure!(bytes as usize <= size_of_val(self.buffer.as_ref()));
                // The read has completed, so Windows no longer writes to this buffer.
                let notifications = unsafe {
                    std::slice::from_raw_parts(self.buffer.as_ptr().cast(), bytes as usize)
                };
                should_reload_config(notifications, &self.file_name)
            }
            // Lost notification details: reload once to avoid missing a configuration change.
            Err(error) if error.code() == ERROR_NOTIFY_ENUM_DIR.to_hresult() => true,
            Err(error) => return Err(error.into()),
        };
        // Rearm before the caller reloads, so a concurrent save is still observed.
        self.rearm()?;
        Ok(changed)
    }
}
impl Drop for DirectoryWatch {
    fn drop(&mut self) {
        unsafe {
            if self.pending {
                let _ = CancelIoEx(self.directory, Some(self.overlapped.as_ref()));
                // Cancellation is asynchronous; keep the buffer and OVERLAPPED alive until done.
                let mut bytes = 0;
                let _ =
                    GetOverlappedResult(self.directory, self.overlapped.as_ref(), &mut bytes, true);
            }
            let _ = CloseHandle(self.directory);
        }
    }
}

fn should_reload_config(mut bytes: &[u8], file_name: &[u16]) -> bool {
    // Empty buffers indicate overflow. Invalid records also warrant a conservative reload.
    loop {
        if bytes.len() < 12 {
            return true;
        }
        // FILE_NOTIFY_INFORMATION has a 12-byte header followed by a non-NUL-terminated name.
        let next = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        let name_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let Some(name) = bytes[12..].get(..name_len).filter(|_| name_len % 2 == 0) else {
            return true;
        };
        // apo.cfg is ASCII; compare case-insensitively without allocating for unrelated events.
        let ascii_lower = |unit: u16| {
            if unit <= 0x7f {
                (unit as u8).to_ascii_lowercase() as u16
            } else {
                unit
            }
        };
        if name
            .chunks_exact(2)
            .map(|pair| ascii_lower(u16::from_le_bytes([pair[0], pair[1]])))
            .eq(file_name.iter().copied().map(ascii_lower))
        {
            return true;
        }
        if next == 0 {
            return false;
        }
        if next < 12 + name_len || next % 4 != 0 || next >= bytes.len() {
            return true;
        }
        bytes = &bytes[next..];
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

fn watch(stop: Arc<Event>, config: Arc<RtConfig>) {
    let stop = stop.as_ref();
    let config = config.as_ref();

    let mut watch: Option<DirectoryWatch> = None;
    let mut retry_ms = 250;
    let mut faulted = false;
    let mut check_at = Instant::now();
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
                    check_at = Instant::now()
                        + Duration::from_millis(if faulted { retry_ms.into() } else { 30_000 });
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
            .as_mut()
            .expect("UNREACHABLE: directory subscription should exist");
        let events = [stop.handle(), current.event.handle()];
        // Directory notifications handle writes and atomic replacements. A slow
        // health check also recovers if the whole directory is deleted/recreated.
        // Unrelated traffic must not postpone retries or the directory health check.
        let remaining = check_at.saturating_duration_since(Instant::now());
        let result = if remaining.is_zero() {
            WAIT_TIMEOUT
        } else {
            unsafe { WaitForMultipleObjects(&events, false, remaining.as_millis().max(1) as u32) }
        };
        if result == WAIT_OBJECT_0 {
            break;
        }
        if result.0 == WAIT_OBJECT_0.0 + 1 {
            match current.config_changed() {
                Ok(true) => reload(config, &mut faulted),
                Ok(false) => continue,
                Err(error) => {
                    report_fault(&mut faulted, &error);
                    watch = None;
                    continue;
                }
            }
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
        check_at =
            Instant::now() + Duration::from_millis(if faulted { retry_ms.into() } else { 30_000 });
    }
}

use std::{
    ffi::OsStr,
    ffi::OsString,
    fs::{self, OpenOptions},
    io,
    os::windows::ffi::OsStrExt,
    os::windows::ffi::OsStringExt,
    os::windows::io::{FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::ensure;
use atomic_write_file::AtomicWriteFile;
use windows::Win32::{
    Foundation::{ERROR_ALREADY_EXISTS, GetLastError},
    System::{
        Com::CoTaskMemFree, Performance::QueryPerformanceFrequency,
        SystemInformation::GetSystemDirectoryW, Threading::CreateMutexW,
    },
    UI::Shell::{FOLDERID_ProgramData, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
};
use windows_core::{GUID, PCWSTR};

#[macro_export]
macro_rules! field_name {
    ($obj:expr, $field:ident) => {{
        if false {
            let _ = &$obj.$field;
        }
        stringify!($field)
    }};
}

pub fn wide_null(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

pub enum WriteMode {
    AtomicReplace,
    Append,
}

pub fn write_file_with_parents(
    path: impl AsRef<Path>,
    mode: WriteMode,
    write: impl FnOnce(&mut dyn io::Write) -> io::Result<()>,
) -> io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match mode {
        WriteMode::AtomicReplace => {
            let mut file = AtomicWriteFile::open(path)?;
            write(&mut file)?;
            file.commit()
        }
        WriteMode::Append => {
            let mut file = OpenOptions::new().create(true).append(true).open(path)?;
            write(&mut file)
        }
    }
}

// ===== System Directory Paths =====
pub fn system_dir() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut buffer = vec![0_u16; 260];
        loop {
            let length = unsafe { GetSystemDirectoryW(Some(buffer.as_mut_slice())) } as usize;
            if length == 0 {
                panic!(
                    "Failed to get system directory: {}",
                    io::Error::last_os_error()
                );
            }
            if length < buffer.len() {
                return PathBuf::from(OsString::from_wide(&buffer[..length]));
            }
            buffer.resize(length.max(buffer.len() + 1), 0);
        }
    })
}
pub fn conhost_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| system_dir().join("conhost.exe"))
}
pub fn powershell_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| system_dir().join(r"WindowsPowerShell\v1.0\powershell.exe"))
}
pub fn apo_dll_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| system_dir().join("voice_box_apo.dll"))
}

// ===== Data Directory Paths =====
pub fn data_dir() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| {
        let folder = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, KF_FLAG_DEFAULT, None) }
            .unwrap_or_else(|e| panic!("Failed to get ProgramData directory: {}", e));
        let path = unsafe { PathBuf::from(OsString::from_wide(folder.as_wide())) };
        unsafe {
            CoTaskMemFree(Some(folder.0.cast()));
        }
        path.join("VoiceBox")
    })
}
pub fn apo_cfg_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| data_dir().join("apo.cfg"))
}
pub fn database_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| data_dir().join("voice_box.db"))
}
pub fn ui_state_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| data_dir().join("voice_box.ron"))
}
pub fn log_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| data_dir().join("voice_box.log"))
}
pub fn rotated_log_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| data_dir().join("voice_box.log.1"))
}

pub fn transport_name(guid: &str) -> anyhow::Result<String> {
    let id = guid
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(guid);
    ensure!(GUID::try_from(id).is_ok(), "Invalid microphone ID");
    Ok(format!(r"Global\VoiceBox.Pcm.{}", id.to_ascii_lowercase()))
}

pub fn win_qpf() -> i64 {
    static CACHE: OnceLock<i64> = OnceLock::new();
    *CACHE.get_or_init(|| {
        let mut frequency = 0;
        unsafe {
            QueryPerformanceFrequency(&mut frequency)
                .unwrap_or_else(|e| panic!("Failed to query performance frequency: {}", e));
        }
        if frequency <= 0 {
            panic!("Failed to query performance frequency");
        }
        frequency
    })
}

/// Creates a named mutex and keeps it alive until the returned handle is dropped.
/// Returns `None` if it already exists. Uses object existence, not mutex ownership.
pub fn win_try_create_named_mutex(name: &str) -> windows_core::Result<Option<OwnedHandle>> {
    let name = wide_null(name);
    let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }?;
    // Read the status before another Win32 call can overwrite it.
    let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    // SAFETY: CreateMutexW returned a valid, uniquely owned handle.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
    if already_exists {
        Ok(None)
    } else {
        Ok(Some(handle))
    }
}

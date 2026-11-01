use std::{
    ffi::OsStr,
    ffi::OsString,
    fs::{self, OpenOptions},
    io,
    os::windows::ffi::OsStrExt,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::ensure;
use atomic_write_file::AtomicWriteFile;
use windows::Win32::{
    System::{Com::CoTaskMemFree, SystemInformation::GetSystemDirectoryW},
    UI::Shell::{FOLDERID_ProgramData, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
};
use windows_core::GUID;

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

fn cached_path(cell: &'static OnceLock<PathBuf>, init: impl FnOnce() -> PathBuf) -> &'static Path {
    cell.get_or_init(init)
}

// ===== System Directory Paths =====
pub fn system_dir() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    cached_path(&CACHE, || {
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
    cached_path(&CACHE, || system_dir().join("conhost.exe"))
}
pub fn powershell_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    cached_path(&CACHE, || {
        system_dir().join(r"WindowsPowerShell\v1.0\powershell.exe")
    })
}
pub fn apo_dll_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    cached_path(&CACHE, || system_dir().join("voice_box_apo.dll"))
}

// ===== Data Directory Paths =====
pub fn data_dir() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    cached_path(&CACHE, || {
        let folder = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, KF_FLAG_DEFAULT, None) }
            .unwrap_or_else(|error| panic!("Failed to get ProgramData directory: {error}"));
        let path = unsafe { PathBuf::from(OsString::from_wide(folder.as_wide())) };
        unsafe {
            CoTaskMemFree(Some(folder.0.cast()));
        }
        path.join("VoiceBox")
    })
}
pub fn apo_cfg_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    cached_path(&CACHE, || data_dir().join("apo.cfg"))
}
pub fn database_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    cached_path(&CACHE, || data_dir().join("voice_box.db"))
}
pub fn ui_state_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    cached_path(&CACHE, || data_dir().join("voice_box.ron"))
}
pub fn log_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    cached_path(&CACHE, || data_dir().join("voice_box.log"))
}
pub fn rotated_log_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    cached_path(&CACHE, || data_dir().join("voice_box.log.1"))
}
pub fn transport_path(guid: &str) -> anyhow::Result<PathBuf> {
    let id = guid
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(guid);
    ensure!(GUID::try_from(id).is_ok(), "Invalid microphone ID");
    Ok(data_dir().join(format!("soundboard-{}.pcm", id.to_ascii_lowercase())))
}

//! Where the app keeps its files, and how it writes them.

use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use atomic_write_file::AtomicWriteFile;
use windows::Win32::{
    System::{Com::CoTaskMemFree, SystemInformation::GetSystemDirectoryW},
    UI::Shell::{FOLDERID_ProgramData, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
};

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

macro_rules! cached_path {
    ($($name:ident => $init:expr;)*) => {
        $(pub fn $name() -> &'static Path {
            static CACHE: OnceLock<PathBuf> = OnceLock::new();
            CACHE.get_or_init(|| $init)
        })*
    };
}

cached_path! {
    system_dir => {
        let mut buffer = vec![0_u16; 260];
        loop {
            let length = unsafe { GetSystemDirectoryW(Some(buffer.as_mut_slice())) } as usize;
            if length == 0 {
                panic!("Failed to get system directory: {}", io::Error::last_os_error());
            }
            if length < buffer.len() {
                break PathBuf::from(OsString::from_wide(&buffer[..length]));
            }
            buffer.resize(length.max(buffer.len() + 1), 0);
        }
    };
    conhost_path => system_dir().join("conhost.exe");
    powershell_path => system_dir().join(r"WindowsPowerShell\v1.0\powershell.exe");
    apo_dll_path => system_dir().join("voice_box_apo.dll");
    data_dir => {
        let folder = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, KF_FLAG_DEFAULT, None) }
            .unwrap_or_else(|e| panic!("Failed to get ProgramData directory: {e}"));
        let path = unsafe { PathBuf::from(OsString::from_wide(folder.as_wide())) };
        unsafe { CoTaskMemFree(Some(folder.0.cast())) };
        path.join("VoiceBox")
    };
    database_path => data_dir().join("data.db");
    ui_state_path => data_dir().join("ui.ron");
    settings_path => data_dir().join("settings.json");
    log_path => data_dir().join("app.log");
    rotated_log_path => data_dir().join("app.log.1");
}

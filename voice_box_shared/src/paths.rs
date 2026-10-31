//! Windows directories and machine-wide VoiceBox data paths.

use std::{
    ffi::OsString,
    io,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::ensure;
use windows::Win32::{
    System::{Com::CoTaskMemFree, SystemInformation::GetSystemDirectoryW},
    UI::Shell::{FOLDERID_ProgramData, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
};
use windows_core::GUID;

pub fn cached_path(
    cell: &'static OnceLock<PathBuf>,
    init: impl FnOnce() -> io::Result<PathBuf>,
) -> io::Result<&'static Path> {
    if let Some(path) = cell.get() {
        return Ok(path.as_path());
    }
    let path = init()?;
    Ok(cell.get_or_init(|| path).as_path())
}

pub fn system_dir() -> io::Result<&'static Path> {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    cached_path(&CACHE, || {
        let mut buffer = vec![0_u16; 260];
        loop {
            let length =
                (|buffer| unsafe { GetSystemDirectoryW(Some(buffer)) })(&mut buffer) as usize;
            if length == 0 {
                return Err(io::Error::last_os_error());
            }
            if length < buffer.len() {
                return Ok(PathBuf::from(OsString::from_wide(&buffer[..length])));
            }
            buffer.resize(length.max(buffer.len() + 1), 0);
        }
    })
}

pub fn data_dir() -> io::Result<&'static Path> {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    cached_path(&CACHE, || {
        let folder = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, KF_FLAG_DEFAULT, None) }
            .map_err(io::Error::other)?;
        let path = unsafe { folder.to_string() };
        unsafe {
            CoTaskMemFree(Some(folder.0.cast()));
        }
        Ok(PathBuf::from(path.map_err(io::Error::other)?).join("VoiceBox"))
    })
}

pub fn apo_dll_path() -> io::Result<&'static Path> {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    cached_path(&CACHE, || Ok(system_dir()?.join("voice_box_apo.dll")))
}

pub fn apo_cfg_path() -> io::Result<&'static Path> {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    cached_path(&CACHE, || Ok(data_dir()?.join("apo.cfg")))
}

pub fn transport_path(guid: &str) -> anyhow::Result<PathBuf> {
    let id = guid.trim_matches(['{', '}']);
    ensure!(GUID::try_from(id).is_ok(), "Invalid microphone ID");
    Ok(data_dir()?.join(format!("soundboard-{}.pcm", id.to_ascii_lowercase())))
}

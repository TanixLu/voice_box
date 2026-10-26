//! Windows directories and machine-wide VoiceBox data paths.
use std::{ffi::OsString, io, os::windows::ffi::OsStringExt, path::PathBuf};
use windows::Win32::{
    System::{Com::CoTaskMemFree, SystemInformation::GetSystemDirectoryW},
    UI::Shell::{FOLDERID_ProgramData, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
};

pub const APO_CFG_FILE: &str = "apo.cfg";

pub fn system_dir() -> io::Result<PathBuf> {
    directory_from_api(|buffer| unsafe { GetSystemDirectoryW(Some(buffer)) })
}

pub fn apo_dll_path() -> io::Result<PathBuf> {
    Ok(system_dir()?.join("voice_box_apo.dll"))
}

fn directory_from_api(get_directory: impl Fn(&mut [u16]) -> u32) -> io::Result<PathBuf> {
    let mut buffer = vec![0_u16; 260];
    loop {
        let length = get_directory(&mut buffer) as usize;
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if length < buffer.len() {
            return Ok(PathBuf::from(OsString::from_wide(&buffer[..length])));
        }
        // An insufficient buffer returns the required size including the terminator.
        buffer.resize(length.max(buffer.len() + 1), 0);
    }
}

pub fn data_dir() -> io::Result<PathBuf> {
    let folder = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, KF_FLAG_DEFAULT, None) }
        .map_err(io::Error::other)?;
    let path = unsafe { folder.to_string() };
    unsafe {
        CoTaskMemFree(Some(folder.0.cast()));
    }
    Ok(PathBuf::from(path.map_err(io::Error::other)?).join("VoiceBox"))
}

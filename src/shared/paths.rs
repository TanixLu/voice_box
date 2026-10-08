//! Machine-wide VoiceBox data paths, resolved identically for users and audiodg.
use std::{io, path::PathBuf};
use windows::Win32::{
    System::Com::CoTaskMemFree,
    UI::Shell::{FOLDERID_ProgramData, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
};

pub const SETTINGS_FILE: &str = "settings.json";

pub fn data_dir() -> io::Result<PathBuf> {
    let folder = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, KF_FLAG_DEFAULT, None) }
        .map_err(io::Error::other)?;
    let path = unsafe { folder.to_string() };
    unsafe {
        CoTaskMemFree(Some(folder.0.cast()));
    }
    Ok(PathBuf::from(path.map_err(io::Error::other)?).join("VoiceBox"))
}

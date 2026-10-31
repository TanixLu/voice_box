use std::{
    io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use windows::{
    Win32::Security::Cryptography::{
        CRYPT_STRING, CRYPT_STRING_BASE64, CRYPT_STRING_NOCRLF, CryptBinaryToStringW,
    },
    core::PWSTR,
};

use voice_box_shared::paths::{cached_path, data_dir, system_dir};

pub fn conhost_path() -> io::Result<&'static Path> {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    cached_path(&CACHE, || Ok(system_dir()?.join("conhost.exe")))
}

pub fn powershell_path() -> io::Result<&'static Path> {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    cached_path(&CACHE, || {
        Ok(system_dir()?.join(r"WindowsPowerShell\v1.0\powershell.exe"))
    })
}

pub fn database_path() -> io::Result<&'static Path> {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    cached_path(&CACHE, || Ok(data_dir()?.join("voice_box.db")))
}

pub fn ui_state_path() -> io::Result<&'static Path> {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    cached_path(&CACHE, || Ok(data_dir()?.join("voice_box_ui.ron")))
}

pub fn base64_encode(bytes: &[u8]) -> windows::core::Result<String> {
    let flags = CRYPT_STRING(CRYPT_STRING_BASE64.0 | CRYPT_STRING_NOCRLF);
    let mut length = 0;
    unsafe {
        CryptBinaryToStringW(bytes, flags, None, &mut length).ok()?;
        let mut output = vec![0; length as usize];
        CryptBinaryToStringW(bytes, flags, Some(PWSTR(output.as_mut_ptr())), &mut length).ok()?;
        let end = output
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(output.len());
        Ok(String::from_utf16_lossy(&output[..end]))
    }
}

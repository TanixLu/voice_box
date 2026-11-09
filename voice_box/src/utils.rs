use std::{marker::PhantomData, rc::Rc};

use serde::{Deserialize, Serialize};
use windows::{
    Win32::{
        Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME},
        Security::Cryptography::{
            BCRYPT_SHA256_ALG_HANDLE, BCryptHash, CRYPT_STRING, CRYPT_STRING_BASE64,
            CRYPT_STRING_NOCRLF, CryptBinaryToStringW,
        },
        UI::{
            Shell::IsUserAnAdmin,
            WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
        },
    },
    core::PWSTR,
};
use windows_core::{PCWSTR, w};

use voice_box_shared::utils::wide_null;

pub const VOICE_BOX_SINGLE_INSTANCE: &str = r"Global\VoiceBoxSingleInstance";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct GainPercent(u16);

impl<'de> Deserialize<'de> for GainPercent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        u16::deserialize(deserializer).map(Self::new)
    }
}

impl GainPercent {
    pub const MAX_VALUE: u16 = 200;

    pub fn new(value: u16) -> Self {
        Self(value.min(Self::MAX_VALUE))
    }

    pub fn is_mute(&self) -> bool {
        self.0 == 0
    }

    pub fn as_u16(&self) -> u16 {
        self.0
    }

    pub fn as_f32_gain(&self) -> f32 {
        self.0 as f32 / 100.0
    }
}

pub struct ComGuard(PhantomData<Rc<()>>);
impl ComGuard {
    pub fn mta() -> anyhow::Result<Self> {
        wasapi::initialize_mta().ok()?;
        Ok(Self(PhantomData))
    }
    pub fn sta() -> anyhow::Result<Self> {
        wasapi::initialize_sta().ok()?;
        Ok(Self(PhantomData))
    }
}
impl Drop for ComGuard {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}

pub fn win_base64_encode(bytes: &[u8]) -> windows::core::Result<String> {
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

pub fn win_sha256(bytes: &[u8]) -> anyhow::Result<String> {
    anyhow::ensure!(bytes.len() <= u32::MAX as usize, "Executable is too large");
    let mut hash = [0; 32];
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, bytes, &mut hash).ok()? };
    Ok(hash.iter().map(|byte| format!("{byte:02X}")).collect())
}

pub fn win_preferred_ui_languages() -> Vec<String> {
    let mut count = 0;
    let mut size = 0;
    unsafe {
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut size).is_err()
            || size == 0
            || size > 65_536
        {
            return Vec::new();
        }
        let mut buffer = vec![0u16; size as usize];
        if GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            Some(windows::core::PWSTR(buffer.as_mut_ptr())),
            &mut size,
        )
        .is_err()
        {
            return Vec::new();
        }
        buffer
            .split(|&c| c == 0)
            .filter(|s| !s.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    }
}

pub fn win_popup_error(msg: &str) {
    let message = wide_null(msg);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            w!("VoiceBox"),
            MB_OK | MB_ICONERROR,
        );
    }
}

pub fn win_is_admin() -> bool {
    unsafe { IsUserAnAdmin().as_bool() }
}

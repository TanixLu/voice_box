use std::{
    ffi::OsStr,
    os::windows::ffi::OsStrExt,
    os::windows::io::{FromRawHandle, OwnedHandle},
    sync::OnceLock,
};

use serde::{Deserialize, Serialize};
use windows::Win32::{
    Foundation::{ERROR_ALREADY_EXISTS, GetLastError},
    Media::Audio::Apo::{
        APO_FLAG, APO_FLAG_BITSPERSAMPLE_MUST_MATCH, APO_FLAG_FRAMESPERSECOND_MUST_MATCH,
        APO_FLAG_INPLACE, APO_FLAG_SAMPLESPERFRAME_MUST_MATCH, APO_REG_PROPERTIES,
        IAudioProcessingObject,
    },
    System::{Performance::QueryPerformanceFrequency, Threading::CreateMutexW},
};
use windows_core::{GUID, Interface as _, PCWSTR};

pub const APO_CLSID: GUID = GUID::from_u128(0xae672075_b244_4417_b5a5_fbd45b7f0268);
pub const APO_NAME: &str = "VoiceBox APO";
pub const COPYRIGHT: &str = "Copyright (C) 2026 Tanix";

pub fn apo_registration_properties() -> APO_REG_PROPERTIES {
    let mut properties = APO_REG_PROPERTIES {
        clsid: APO_CLSID,
        Flags: APO_FLAG(
            APO_FLAG_INPLACE.0
                | APO_FLAG_SAMPLESPERFRAME_MUST_MATCH.0
                | APO_FLAG_FRAMESPERSECOND_MUST_MATCH.0
                | APO_FLAG_BITSPERSAMPLE_MUST_MATCH.0,
        ),
        u32MajorVersion: 1,
        u32MinorVersion: 0,
        u32MinInputConnections: 1,
        u32MaxInputConnections: 1,
        u32MinOutputConnections: 1,
        u32MaxOutputConnections: 1,
        u32MaxInstances: u32::MAX,
        u32NumAPOInterfaces: 1,
        iidAPOInterfaceList: [IAudioProcessingObject::IID],
        ..Default::default()
    };
    write_wide(&mut properties.szFriendlyName, APO_NAME);
    write_wide(&mut properties.szCopyrightInfo, COPYRIGHT);
    properties
}

fn write_wide(destination: &mut [u16], text: &str) {
    for (slot, value) in destination.iter_mut().zip(text.encode_utf16()) {
        *slot = value;
    }
}

pub fn wide_null(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
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

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "i8", into = "i8")]
pub struct GainDb(i8);

impl From<i8> for GainDb {
    fn from(value: i8) -> Self {
        Self::new(value)
    }
}

impl From<GainDb> for i8 {
    fn from(gain: GainDb) -> Self {
        gain.0
    }
}

impl GainDb {
    pub const MIN_VALUE: i8 = -60;
    pub const MAX_VALUE: i8 = 60;

    pub fn new(value: i8) -> Self {
        Self(value.clamp(Self::MIN_VALUE, Self::MAX_VALUE))
    }

    pub fn is_mute(&self) -> bool {
        self.0 == Self::MIN_VALUE
    }

    pub fn as_i8(&self) -> i8 {
        self.0
    }

    pub fn as_f32_gain(&self) -> f32 {
        if self.is_mute() {
            0.0
        } else {
            10.0_f32.powf(self.0 as f32 / 20.0)
        }
    }
}

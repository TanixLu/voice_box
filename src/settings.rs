//! Shared 64-bit machine-wide configuration schema.
use windows::{
    Win32::System::Registry::{HKEY, REG_DWORD, RegQueryMultipleValuesW, VALENTW},
    core::{PWSTR, w},
};
use winreg::{
    RegKey,
    enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY},
};

pub const ROOT: &str = r"SOFTWARE\VoiceBox";
pub const PATH: &str = constcat::concat!(ROOT, r"\Settings");
pub const MIN_GAIN_MILLIDB: i32 = -60_000;
pub const MAX_GAIN_MILLIDB: i32 = 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub gain_millidb: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            gain_millidb: 0,
        }
    }
}

impl Settings {
    pub fn read(key: &RegKey) -> std::io::Result<Self> {
        // One query observes a complete committed configuration, even when a
        // transaction commits concurrently with the read.
        let mut entries = [
            VALENTW {
                ve_valuename: PWSTR(w!("Enabled").as_ptr().cast_mut()),
                ..Default::default()
            },
            VALENTW {
                ve_valuename: PWSTR(w!("GainMilliDb").as_ptr().cast_mut()),
                ..Default::default()
            },
        ];
        // Leave room for DWORD alignment padding between returned entries.
        let mut buffer = [0u64; 2];
        let mut size = core::mem::size_of_val(&buffer) as u32;
        let result = unsafe {
            RegQueryMultipleValuesW(
                HKEY(key.raw_handle().cast()),
                &mut entries,
                Some(PWSTR(buffer.as_mut_ptr().cast())),
                Some(&mut size),
            )
        };
        if result.0 != 0 {
            return Err(std::io::Error::from_raw_os_error(result.0 as i32));
        }
        let invalid = || {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid VoiceBox configuration",
            )
        };
        let mut values = [0u32; 2];
        let bytes = unsafe {
            core::slice::from_raw_parts(
                buffer.as_ptr().cast::<u8>(),
                core::mem::size_of_val(&buffer),
            )
        };
        for (entry, value) in entries.iter().zip(&mut values) {
            if entry.ve_type != REG_DWORD || entry.ve_valuelen != 4 {
                return Err(invalid());
            }
            let offset = entry
                .ve_valueptr
                .checked_sub(bytes.as_ptr() as usize)
                .ok_or_else(invalid)?;
            let data = bytes
                .get(offset..offset.checked_add(4).ok_or_else(invalid)?)
                .ok_or_else(invalid)?;
            *value = u32::from_le_bytes(data.try_into().map_err(|_| invalid())?);
        }
        let gain_millidb = values[1] as i32;
        if !(MIN_GAIN_MILLIDB..=MAX_GAIN_MILLIDB).contains(&gain_millidb) {
            return Err(invalid());
        }
        Ok(Self {
            enabled: values[0] != 0,
            gain_millidb,
        })
    }
    pub fn try_load() -> std::io::Result<Self> {
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey_with_flags(PATH, KEY_READ | KEY_WOW64_64KEY)
            .and_then(|key| Self::read(&key))
    }
}

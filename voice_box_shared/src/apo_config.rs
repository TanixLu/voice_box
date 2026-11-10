//! Machine-wide APO configuration, stored as JSON in the registry for the GUI and APO.

use serde::{Deserialize, Serialize};
use windows::{
    Win32::{
        Foundation::ERROR_FILE_NOT_FOUND,
        System::Registry::{
            HKEY_LOCAL_MACHINE, REG_SZ, RRF_RT_REG_SZ, RegGetValueW, RegSetKeyValueW,
        },
    },
    core::HSTRING,
};

use crate::utils::GainDb;

/// Under HKEY_LOCAL_MACHINE, which the audio engine's service account can read and watch.
pub const APO_CONFIG_KEY: &str = r"SOFTWARE\VoiceBox";
const APO_CONFIG_VALUE: &str = "ApoConfig";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApoConfig {
    pub denoising_enabled: bool,
    pub gain_db: GainDb,
    pub playback_include_microphone_audio: bool,
}

impl Default for ApoConfig {
    fn default() -> Self {
        Self {
            denoising_enabled: false,
            gain_db: GainDb::new(0),
            playback_include_microphone_audio: true,
        }
    }
}

impl ApoConfig {
    /// The saved configuration, or the defaults when none has been saved.
    pub fn load() -> anyhow::Result<Self> {
        let (key, value) = (
            HSTRING::from(APO_CONFIG_KEY),
            HSTRING::from(APO_CONFIG_VALUE),
        );
        let mut bytes = 0;
        let query = |data: Option<*mut _>, bytes: &mut u32| unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                &key,
                &value,
                RRF_RT_REG_SZ,
                None,
                data,
                Some(bytes),
            )
        };
        let status = query(None, &mut bytes);
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(Self::default());
        }
        status.ok()?;
        let mut text = vec![0_u16; bytes as usize / 2];
        query(Some(text.as_mut_ptr().cast()), &mut bytes).ok()?;
        let text = String::from_utf16(&text[..bytes as usize / 2])?;
        Ok(serde_json::from_str(text.trim_end_matches('\0'))?)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let text: Vec<u16> = serde_json::to_string(self)?
            .encode_utf16()
            .chain([0])
            .collect();
        unsafe {
            RegSetKeyValueW(
                HKEY_LOCAL_MACHINE,
                &HSTRING::from(APO_CONFIG_KEY),
                &HSTRING::from(APO_CONFIG_VALUE),
                REG_SZ.0,
                Some(text.as_ptr().cast()),
                size_of_val(text.as_slice()) as u32,
            )
        }
        .ok()?;
        Ok(())
    }
}

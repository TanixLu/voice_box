//! Use the installed APO DLL for offline preview, unloading after processing.

use std::fs;

use anyhow::{anyhow, ensure};
use windows::{
    Win32::{
        Foundation::{FreeLibrary, HMODULE},
        System::LibraryLoader::{
            GetProcAddress, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
            LoadLibraryExW,
        },
    },
    core::{PCSTR, PCWSTR},
};

use voice_box_shared::{apo_cfg::ApoConfig, paths::apo_dll_path, preview, utils::wide_null};

use crate::{
    audio::capture::Recording,
    i18n::{Text, UserContext},
    static_data::apo_dll_bytes,
};

struct PreviewDll(HMODULE);

impl PreviewDll {
    fn load() -> anyhow::Result<Self> {
        let apo_dll_path = apo_dll_path()?;
        // The unversioned ABI requires the installed DLL to match this EXE.
        ensure!(
            fs::read(&apo_dll_path)
                .user_context(Text::ReadPreviewComponent)?
                .as_slice()
                == apo_dll_bytes(),
            Text::PreviewComponentOutdated
        );
        let path = wide_null(&apo_dll_path);
        let module = unsafe {
            LoadLibraryExW(
                PCWSTR(path.as_ptr()),
                None,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }
        .user_context(Text::LoadPreviewComponent)?;
        Ok(Self(module))
    }

    fn resolve_process_function(&self) -> anyhow::Result<preview::Process> {
        let address = unsafe { GetProcAddress(self.0, PCSTR(preview::PROCESS_SYMBOL.as_ptr())) }
            .ok_or_else(|| anyhow!(Text::PreviewInterfaceMissing))?;
        // Matching DLL bytes above ensure this shared signature is compatible.
        Ok(unsafe {
            std::mem::transmute::<unsafe extern "system" fn() -> isize, preview::Process>(address)
        })
    }
}

impl Drop for PreviewDll {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeLibrary(self.0);
        }
    }
}

pub fn process(recording: &Recording, config: &ApoConfig) -> anyhow::Result<Vec<f32>> {
    config.validate()?;
    ensure!(
        recording.channels > 0
            && !recording.samples.is_empty()
            && recording
                .samples
                .len()
                .is_multiple_of(recording.channels as usize),
        Text::InvalidRecordingData
    );
    let dll = PreviewDll::load()?;
    let process = dll.resolve_process_function()?;
    let mut output = vec![0.0; recording.samples.len()];
    unsafe {
        process(
            recording.sample_rate,
            u32::from(recording.channels),
            u32::try_from(recording.samples.len() / recording.channels as usize)?,
            config.gain_millidb,
            u32::from(config.denoising_enabled),
            recording.samples.as_ptr(),
            output.as_mut_ptr(),
        )
        .ok()
    }
    .user_context(Text::ProcessPreview)?;
    Ok(output)
}

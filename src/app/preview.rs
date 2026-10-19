//! Use the installed APO DLL for offline preview, unloading after processing.
use std::fs;

use anyhow::{Context, anyhow, ensure};
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

use super::{audio_test::Recording, setup::APO_DLL};
use crate::shared::{paths, preview, settings::Settings, utils::wide_null};

struct PreviewDll(HMODULE);

impl PreviewDll {
    fn load() -> anyhow::Result<Self> {
        let apo_dll = paths::apo_dll_path().context("读取试听组件路径失败")?;
        // The unversioned ABI requires the installed DLL to match this EXE.
        ensure!(
            fs::read(&apo_dll)
                .context("无法读取试听组件，请先安装或更新插件")?
                .as_slice()
                == APO_DLL,
            "已安装的试听组件与当前程序不一致，请先更新插件"
        );
        let path = wide_null(&apo_dll);
        let module = unsafe {
            LoadLibraryExW(
                PCWSTR(path.as_ptr()),
                None,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }
        .context("加载试听组件失败，请先安装或更新插件")?;
        Ok(Self(module))
    }

    fn process(&self) -> anyhow::Result<preview::Process> {
        let address = unsafe { GetProcAddress(self.0, PCSTR(preview::PROCESS_SYMBOL.as_ptr())) }
            .ok_or_else(|| anyhow!("试听组件缺少处理接口，请更新 VoiceBox"))?;
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

pub(super) fn process(recording: &Recording, settings: &Settings) -> anyhow::Result<Vec<f32>> {
    settings.validate()?;
    ensure!(
        recording.channels > 0
            && !recording.samples.is_empty()
            && recording
                .samples
                .len()
                .is_multiple_of(recording.channels as usize),
        "录音数据无效"
    );
    let dll = PreviewDll::load()?;
    let process = dll.process()?;
    let mut output = vec![0.0; recording.samples.len()];
    unsafe {
        process(
            recording.sample_rate,
            u32::from(recording.channels),
            u32::try_from(recording.samples.len() / recording.channels as usize)?,
            settings.gain_millidb,
            u32::from(settings.enable_denoising),
            recording.samples.as_ptr(),
            output.as_mut_ptr(),
        )
        .ok()
    }
    .context("处理试听录音失败")?;
    Ok(output)
}

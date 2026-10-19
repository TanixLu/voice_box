//! Elevated APO removal followed by recycling from a separate system process.
use std::{
    os::windows::{io::AsRawHandle, process::CommandExt},
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, ensure};
use windows::{
    Win32::{
        Foundation::{HANDLE, WAIT_OBJECT_0},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateGuid, CoCreateInstance,
                CoInitializeEx, CoUninitialize,
            },
            Threading::{
                CREATE_NO_WINDOW, CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects,
            },
        },
        UI::Shell::{
            FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING, FOFX_EARLYFAILURE,
            FOFX_RECYCLEONDELETE, FileOperation, IFileOperation, IShellItem,
            SHCreateItemFromParsingName,
        },
    },
    core::{Owned, PCWSTR},
};

use super::setup;
use crate::shared::{paths, utils::wide_null};

// The GUI is already elevated; only self-deletion needs a separate process.
pub fn run() -> anyhow::Result<()> {
    let directory = paths::data_dir()?;
    let apo_dll = paths::apo_dll_path().context("读取 APO DLL 路径失败")?;
    let cleanup = start_cleanup(&directory)?;
    let _apartment = ShellApartment::new()?;
    setup::with_audio_service_stopped(|| {
        setup::unregister_all()?;
        recycle(&apo_dll).context("回收 APO DLL 失败")
    })?;
    cleanup.finish()
}

// On failure, dismiss the progress window without permitting file recycling.
struct Cleanup {
    complete: Owned<HANDLE>,
    cancel: Owned<HANDLE>,
    committed: bool,
}

impl Cleanup {
    fn finish(mut self) -> anyhow::Result<()> {
        unsafe { SetEvent(*self.complete).context("通知清理进程卸载完成失败")? };
        self.committed = true;
        Ok(())
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.committed {
            unsafe {
                let _ = SetEvent(*self.cancel);
            }
        }
    }
}

fn create_event(name: &str) -> anyhow::Result<Owned<HANDLE>> {
    let name = wide_null(name);
    Ok(unsafe { Owned::new(CreateEventW(None, true, false, PCWSTR(name.as_ptr()))?) })
}

fn start_cleanup(directory: &Path) -> anyhow::Result<Cleanup> {
    let system_directory = paths::system_dir().context("读取 Windows 系统目录失败")?;
    let event_name = format!("Local\\VoiceBox.Cleanup.{:?}", unsafe { CoCreateGuid()? });
    let ready = create_event(&event_name)?;
    let complete_name = format!("{event_name}.complete");
    let cancel_name = format!("{event_name}.cancel");
    let cleanup_state = Cleanup {
        complete: create_event(&complete_name)?,
        cancel: create_event(&cancel_name)?,
        committed: false,
    };

    let cleanup = Command::new(system_directory.join(r"WindowsPowerShell\v1.0\powershell.exe"))
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-STA",
            "-Command",
        ])
        .arg(include_str!("uninstall_cleanup.ps1"))
        // Paths are data, never interpolated into PowerShell source code.
        .env("VOICEBOX_UNINSTALL_EXE", std::env::current_exe()?)
        .env("VOICEBOX_UNINSTALL_DATA", directory)
        .env("VOICEBOX_UNINSTALL_READY", event_name)
        .env("VOICEBOX_UNINSTALL_COMPLETE", complete_name)
        .env("VOICEBOX_UNINSTALL_CANCEL", cancel_name)
        .env(
            "VOICEBOX_UNINSTALL_PROCESS_ID",
            std::process::id().to_string(),
        )
        .current_dir(system_directory)
        .creation_flags(CREATE_NO_WINDOW.0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("启动回收站清理进程失败")?;
    ensure!(
        unsafe {
            WaitForMultipleObjects(&[*ready, HANDLE(cleanup.as_raw_handle())], false, INFINITE)
        } == WAIT_OBJECT_0,
        "回收站清理进程初始化失败，请查看错误提示后重试"
    );
    Ok(cleanup_state)
}

struct ShellApartment;

impl ShellApartment {
    fn new() -> anyhow::Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
        Ok(Self)
    }
}

impl Drop for ShellApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn recycle(path: &Path) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("读取 {} 失败", path.display())),
    }
    let wide_path = wide_null(path);
    unsafe {
        let operation: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
        operation.SetOperationFlags(
            FOFX_RECYCLEONDELETE
                | FOFX_EARLYFAILURE
                | FOF_NOERRORUI
                | FOF_NOCONFIRMATION
                | FOF_WANTNUKEWARNING
                | FOF_SILENT,
        )?;
        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(wide_path.as_ptr()), None)?;
        operation.DeleteItem(&item, None)?;
        operation.PerformOperations()?;
        ensure!(
            !operation.GetAnyOperationsAborted()?.as_bool(),
            "回收 {} 的操作已取消或未完成",
            path.display()
        );
    }
    ensure!(
        !path.try_exists()?,
        "{} 仍然存在，回收未完成",
        path.display()
    );
    Ok(())
}

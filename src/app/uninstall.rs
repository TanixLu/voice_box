//! Elevated APO removal followed by recycling from a separate system process.
use std::{
    ffi::OsString,
    os::windows::{ffi::OsStringExt, io::AsRawHandle, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, ensure};
use is_elevated::is_elevated;
use windows::{
    Win32::{
        Foundation::{HANDLE, WAIT_OBJECT_0},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateGuid, CoCreateInstance,
                CoInitializeEx, CoUninitialize,
            },
            SystemInformation::GetSystemDirectoryW,
            Threading::{
                CREATE_NO_WINDOW, CreateEventW, EVENT_MODIFY_STATE, INFINITE, OpenEventW,
                OpenProcess, PROCESS_SYNCHRONIZE, SetEvent, WaitForMultipleObjects,
                WaitForSingleObject,
            },
        },
        UI::{
            Shell::{
                FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING,
                FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE, FileOperation, IFileOperation, IShellItem,
                SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
                SHCreateItemFromParsingName, SHELLEXECUTEINFOW, ShellExecuteExW,
            },
            WindowsAndMessaging::SW_HIDE,
        },
    },
    core::{Owned, PCWSTR, w},
};

use super::{logging, setup};
use crate::shared::{paths, utils::wide_null};

const ELEVATED_UNINSTALL: &str = "--uninstall-all";

// Runs on a GUI worker thread. The independent progress window is shown before
// the GUI closes; file recycling still waits for the entire process to exit.
pub fn run() -> anyhow::Result<()> {
    if is_elevated() {
        return elevated_uninstall(None);
    }

    // Keep the GUI visible until the elevated process has shown its progress UI.
    // The elevated process shares this EXE and takes over only after the GUI exits.
    let event_name = format!("Local\\VoiceBox.Uninstall.{:?}", unsafe { CoCreateGuid()? });
    let ready = create_event(&event_name)?;
    let parameters = wide_null(format!(
        "{ELEVATED_UNINSTALL} {} {event_name}",
        std::process::id()
    ));
    let executable = wide_null(std::env::current_exe()?);
    let working_directory = wide_null(std::env::temp_dir());
    let _apartment = ShellApartment::new()?;
    let mut launch = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(executable.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        lpDirectory: PCWSTR(working_directory.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut launch).context("无法启动管理员卸载操作，或管理员授权已取消")?;
    }
    ensure!(!launch.hProcess.is_invalid(), "未取得管理员卸载进程句柄");
    let child = unsafe { Owned::new(launch.hProcess) };
    ensure!(
        unsafe { WaitForMultipleObjects(&[*ready, *child], false, INFINITE) } == WAIT_OBJECT_0,
        "管理员卸载进程未能接管，请查看错误提示后重试"
    );
    Ok(())
}

pub fn maybe_elevated_uninstall() -> anyhow::Result<bool> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_none_or(|arg| arg != ELEVATED_UNINSTALL) {
        return Ok(false);
    }
    ensure!(is_elevated(), "完全卸载需要管理员权限");
    ensure!(args.len() == 4, "完全卸载参数无效");
    let parent_id: u32 = args[2].to_str().context("卸载进程参数无效")?.parse()?;
    let ready_name = wide_null(&args[3]);
    let ready = unsafe {
        Owned::new(OpenEventW(
            EVENT_MODIFY_STATE,
            false,
            PCWSTR(ready_name.as_ptr()),
        )?)
    };
    let parent = unsafe {
        Owned::new(
            OpenProcess(PROCESS_SYNCHRONIZE, false, parent_id).context("打开原程序进程失败")?,
        )
    };
    elevated_uninstall(Some((*parent, *ready)))?;
    Ok(true)
}

fn elevated_uninstall(handoff: Option<(HANDLE, HANDLE)>) -> anyhow::Result<()> {
    ensure!(is_elevated(), "完全卸载需要管理员权限");
    let directory = paths::data_dir()?;
    let cleanup = start_cleanup(&directory)?;
    // Close the original GUI only after the independent progress window is shown.
    // That window also retains the single-instance mutex during the handoff.
    if let Some((parent, ready)) = handoff {
        unsafe { SetEvent(ready).context("通知原程序退出失败")? };
        ensure!(
            unsafe { WaitForSingleObject(parent, INFINITE) } == WAIT_OBJECT_0,
            "等待原程序退出失败"
        );
    }
    // Never initialize settings or recreate a missing data directory here.
    let _ = logging::init(true);
    let _apartment = ShellApartment::new()?;
    setup::with_audio_service_stopped(|| {
        setup::unregister_all()?;
        recycle(Path::new(setup::APO_DLL_PATH)).context("回收 APO DLL 失败")
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
    let mut buffer = vec![0_u16; 32768];
    let length = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    ensure!(
        length > 0 && length < buffer.len(),
        "读取 Windows 系统目录失败"
    );
    let system_directory = PathBuf::from(OsString::from_wide(&buffer[..length]));
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

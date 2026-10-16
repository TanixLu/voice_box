//! Elevated uninstall; self-replace handles executable deletion on exit.
use std::path::Path;

use anyhow::{Context, ensure};
use is_elevated::is_elevated;
use windows::{
    Win32::{
        Foundation::WAIT_OBJECT_0,
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateGuid, CoCreateInstance,
                CoInitializeEx, CoUninitialize,
            },
            Threading::{
                CreateEventW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, OpenProcess,
                PROCESS_SYNCHRONIZE, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
            },
        },
        UI::{
            Shell::{
                FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING,
                FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE, FileOperation, IFileOperation, IShellItem,
                SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
                SHCreateItemFromParsingName, SHELLEXECUTEINFOW, ShellExecuteExW,
            },
            WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK, MessageBoxW, SW_HIDE},
        },
    },
    core::{Owned, PCWSTR, w},
};

use super::{logging, setup};
use crate::shared::{paths, utils::wide_null};

const ELEVATED_UNINSTALL: &str = "--uninstall-all";

// Called after eframe has closed and finished all persistence writes, so it
// cannot recreate the data directory after that directory has been recycled.
pub fn run() -> anyhow::Result<()> {
    if is_elevated() {
        return elevated_uninstall();
    }

    // Keep the GUI process alive until the elevated process has opened its handle.
    // self_delete must run after the GUI exits, since both processes use this EXE.
    let event_name = format!("Local\\VoiceBox.Uninstall.{:?}", unsafe { CoCreateGuid()? });
    let ready_name = wide_null(&event_name);
    let ready = unsafe {
        Owned::new(CreateEventW(
            None,
            true,
            false,
            PCWSTR(ready_name.as_ptr()),
        )?)
    };
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
    unsafe { SetEvent(*ready).context("通知原程序退出失败")? };
    ensure!(
        unsafe { WaitForSingleObject(*parent, INFINITE) } == WAIT_OBJECT_0,
        "等待原程序退出失败"
    );
    let single_instance = single_instance::SingleInstance::new("VoiceBoxSingleInstance")?;
    ensure!(
        single_instance.is_single(),
        "另一个 VoiceBox 实例正在运行，请关闭后重试卸载"
    );
    elevated_uninstall()?;
    Ok(true)
}

fn elevated_uninstall() -> anyhow::Result<()> {
    ensure!(is_elevated(), "完全卸载需要管理员权限");
    let directory = paths::data_dir()?;
    if directory.try_exists()? {
        // self_delete may stage files alongside the EXE if moving to TEMP fails.
        // Keep that staging area outside the directory we are about to recycle.
        ensure!(
            !std::env::current_exe()?
                .canonicalize()?
                .starts_with(directory.canonicalize()?),
            "当前 exe 位于 VoiceBox 数据目录内，请先将 exe 移至其他目录再卸载"
        );
    }
    // Never initialize settings or recreate a missing data directory here.
    let _ = logging::init(true);
    let _apartment = ShellApartment::new()?;
    setup::with_audio_service_stopped(|| {
        setup::unregister_all()?;
        recycle(Path::new(setup::APO_DLL_PATH)).context("回收 APO DLL 失败")
    })?;
    recycle(&directory).context("回收 VoiceBox 数据目录失败")?;
    self_replace::self_delete().context("安排程序自删除失败")?;
    unsafe {
        MessageBoxW(
            None,
            w!("VoiceBox 已完全从您的电脑中移除。"),
            w!("VoiceBox 卸载完成"),
            MB_OK | MB_ICONINFORMATION,
        );
    }
    Ok(())
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

//! Remove the APO and its data; leave the executable for the user to delete.
use std::path::Path;

use anyhow::{Context, ensure};
use is_elevated::is_elevated;
use windows::{
    Win32::{
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
        UI::{
            Shell::{
                FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING,
                FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE, FileOperation, IFileOperation, IShellItem,
                SHCreateItemFromParsingName,
            },
            WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK, MessageBoxW},
        },
    },
    core::{PCWSTR, w},
};

use super::{logging, setup};
use crate::shared::{paths, utils::wide_null};

const ELEVATED_UNINSTALL: &str = "--uninstall-all";

// Called after eframe has closed and finished all persistence writes, so it
// cannot recreate the data directory after that directory has been recycled.
pub fn run() -> anyhow::Result<()> {
    let executable = std::env::current_exe()?;
    if is_elevated() {
        elevated_uninstall()?;
    } else {
        let status = runas::Command::new(&executable)
            .arg(ELEVATED_UNINSTALL)
            .gui(false)
            .show(false)
            .status()
            .context("无法启动管理员卸载操作")?;
        ensure!(
            status.success(),
            "管理员卸载操作失败或已取消，请查看错误提示后重试"
        );
    }

    let message = wide_null(format!(
        "VoiceBox 卸载完成。\n\n点击“确定”退出后，请手动删除以下程序文件：\n{}",
        executable.display()
    ));
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            w!("VoiceBox 卸载完成"),
            MB_OK | MB_ICONINFORMATION,
        );
    }
    Ok(())
}

pub fn maybe_elevated_uninstall() -> anyhow::Result<bool> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_none_or(|arg| arg != ELEVATED_UNINSTALL) {
        return Ok(false);
    }
    ensure!(is_elevated(), "完全卸载需要管理员权限");
    ensure!(args.len() == 2, "完全卸载参数无效");
    // Removal must also work with a missing directory or corrupt settings.
    // Never initialize settings or recreate the data directory here.
    let _ = logging::init(true);
    elevated_uninstall()?;
    Ok(true)
}

fn elevated_uninstall() -> anyhow::Result<()> {
    ensure!(is_elevated(), "完全卸载需要管理员权限");
    let directory = paths::data_dir()?;
    if directory.try_exists()? {
        // Keep the executable even if it was placed inside the data directory.
        let directory = directory.canonicalize()?;
        let executable = std::env::current_exe()?.canonicalize()?;
        ensure!(
            !executable.starts_with(&directory),
            "当前 exe 位于 VoiceBox 数据目录内，请先将 exe 移至其他目录再卸载"
        );
    }

    let _apartment = ShellApartment::new()?;
    setup::with_audio_service_stopped(|| {
        setup::unregister_all()?;
        recycle(Path::new(setup::APO_DLL_PATH)).context("回收 APO DLL 失败")
    })?;
    recycle(&directory).context("回收 VoiceBox 数据目录失败")?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recycle_data_preserves_executable_and_allows_missing_paths() -> anyhow::Result<()> {
        let _apartment = ShellApartment::new()?;
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("uninstall-test-{}-{unique}", std::process::id()));
        let data = root.join("VoiceBox 中文 & % ! ' (test)");
        let executable = root.join("voice_box.exe");
        let dll = root.join("voice_box.dll");
        std::fs::create_dir_all(&data)?;
        std::fs::write(data.join("settings.json"), "{}")?;
        std::fs::write(&executable, "keep this fixture")?;
        std::fs::write(&dll, "APO fixture")?;

        recycle(&dll)?;
        recycle(&data)?;
        recycle(&data)?;
        assert!(!dll.try_exists()?);
        assert!(!data.try_exists()?);
        assert_eq!(std::fs::read_to_string(&executable)?, "keep this fixture");

        std::fs::remove_file(executable)?;
        std::fs::remove_dir(root)?;
        Ok(())
    }
}

//! Elevated APO removal followed by recycling from a separate system process.

use std::{
    os::windows::{io::AsRawHandle, process::CommandExt},
    path::Path,
    process::{Command, Stdio},
};

use anyhow::ensure;
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

use voice_box_shared::{
    paths::{apo_dll_path, data_dir, system_dir},
    utils::wide_null,
};

use crate::{
    i18n::{Message, Text, UserContext},
    setup,
    static_data::UNINSTALL_CLEANUP_SCRIPT,
    utils::powershell_path,
};

// The GUI is already elevated; only self-deletion needs a separate process.
pub fn run() -> anyhow::Result<()> {
    crate::startup::synchronize(false)?;
    let cleanup = start_cleanup(&data_dir()?)?;
    let _apartment = ShellApartment::new()?;
    setup::with_audio_service_stopped(|| {
        setup::unregister_all()?;
        recycle(&apo_dll_path()?).user_context(Text::RecycleApo)
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
        unsafe { SetEvent(*self.complete).user_context(Text::SignalCleanup)? };
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
    let event_name = format!("Local\\VoiceBox.Cleanup.{:?}", unsafe { CoCreateGuid()? });
    let ready = create_event(&event_name)?;
    let complete_name = format!("{event_name}.complete");
    let cancel_name = format!("{event_name}.cancel");
    let cleanup_state = Cleanup {
        complete: create_event(&complete_name)?,
        cancel: create_event(&cancel_name)?,
        committed: false,
    };

    let cleanup = Command::new(powershell_path()?)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-STA",
            "-Command",
        ])
        .arg(UNINSTALL_CLEANUP_SCRIPT)
        // Paths are data, never interpolated into PowerShell source code.
        .envs(
            cleanup_texts(crate::i18n::current_language())
                .map(|(name, value)| (format!("VOICEBOX_UNINSTALL_TEXT_{name}"), value)),
        )
        .env("VOICEBOX_UNINSTALL_EXE", std::env::current_exe()?)
        .env("VOICEBOX_UNINSTALL_DATA", directory)
        .env("VOICEBOX_UNINSTALL_READY", event_name)
        .env("VOICEBOX_UNINSTALL_COMPLETE", complete_name)
        .env("VOICEBOX_UNINSTALL_CANCEL", cancel_name)
        .env(
            "VOICEBOX_SINGLE_INSTANCE_MUTEX",
            super::SINGLE_INSTANCE_MUTEX,
        )
        .env(
            "VOICEBOX_UNINSTALL_PROCESS_ID",
            std::process::id().to_string(),
        )
        .current_dir(system_dir()?)
        .creation_flags(CREATE_NO_WINDOW.0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .user_context(Text::StartCleanup)?;
    ensure!(
        unsafe {
            WaitForMultipleObjects(&[*ready, HANDLE(cleanup.as_raw_handle())], false, INFINITE)
        } == WAIT_OBJECT_0,
        Text::CleanupInitializationFailed
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
        Err(error) => {
            return Err(error).with_user_context(|| Message::ReadPathFailed {
                path: path.display().to_string(),
            });
        }
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
            Message::RecycleCancelled {
                path: path.display().to_string()
            }
        );
    }
    ensure!(
        !path.try_exists()?,
        Message::RecycleIncomplete {
            path: path.display().to_string()
        }
    );
    Ok(())
}

fn cleanup_texts(language: crate::i18n::Language) -> [(&'static str, &'static str); 14] {
    [
        (
            "UninstallIncompleteTitle",
            Text::UninstallIncompleteTitle.in_language(language),
        ),
        (
            "UninstallCompleteTitle",
            Text::UninstallCompleteTitle.in_language(language),
        ),
        (
            "UninstallingVoiceBox",
            Text::UninstallingVoiceBox.in_language(language),
        ),
        (
            "UninstallProgress",
            Text::UninstallProgress.in_language(language),
        ),
        ("Ok", Text::Ok.in_language(language)),
        (
            "UninstallProcessExited",
            Text::UninstallProcessExited.in_language(language),
        ),
        (
            "RecyclingExecutable",
            Text::RecyclingExecutable.in_language(language),
        ),
        (
            "RecycleExecutableIncomplete",
            Text::RecycleExecutableIncomplete.in_language(language),
        ),
        (
            "RecyclingSettings",
            Text::RecyclingSettings.in_language(language),
        ),
        (
            "RecycleDataIncomplete",
            Text::RecycleDataIncomplete.in_language(language),
        ),
        (
            "UninstallComplete",
            Text::UninstallComplete.in_language(language),
        ),
        (
            "UninstallIncomplete",
            Text::UninstallIncomplete.in_language(language),
        ),
        (
            "UninstallWindowFailed",
            Text::UninstallWindowFailed.in_language(language),
        ),
        ("CleanupFailed", Text::CleanupFailed.in_language(language)),
    ]
}

//! Management application: GUI, plugin setup, writable settings and file logs.
mod audio_test;
mod data_permissions;
mod gui;
mod logging;
mod preview;
mod registry;
mod settings_writer;
mod setup;
mod uninstall;

const UI_STATE_FILE: &str = "ui.ron";

use crate::shared::utils::wide_null;
use tracing::{error, info};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::{PCWSTR, w};

pub fn run() {
    if let Err(error) = run_inner() {
        error!("Application error: {error:?}");
        let message = wide_null(format!("{error:#}"));
        unsafe {
            MessageBoxW(
                None,
                PCWSTR(message.as_ptr()),
                w!("VoiceBox"),
                MB_OK | MB_ICONERROR,
            );
        }
        std::process::exit(1);
    }
}

fn run_inner() -> anyhow::Result<()> {
    if uninstall::maybe_elevated_uninstall()? {
        return Ok(());
    }
    // The same EXE performs elevated setup without creating another GUI.
    if setup::maybe_elevated_setup()? {
        info!("Elevated setup operation completed successfully");
        return Ok(());
    }
    anyhow::ensure!(
        std::env::args_os().len() == 1,
        "不支持此命令行参数，请在窗口中操作。"
    );
    let single_instance = single_instance::SingleInstance::new("VoiceBoxSingleInstance")?;
    anyhow::ensure!(
        single_instance.is_single(),
        "Another instance of VoiceBox is already running."
    );
    setup::ensure_data_ready()?;
    logging::init(false)?;
    gui::run()
}

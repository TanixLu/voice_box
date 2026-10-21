//! Management application: GUI, plugin setup, writable settings and file logs.
mod audio;
mod audio_session;
mod devices;
mod gui;
mod hotkeys;
mod logging;
mod music_decoder;
mod preview;
mod registry;
mod settings_writer;
mod setup;
mod soundboard;
mod uninstall;

const UI_STATE_FILE: &str = "ui.ron";

use crate::shared::utils::wide_null;
use tracing::error;
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
    anyhow::ensure!(
        std::env::args_os().len() == 1,
        "不支持此命令行参数，请在窗口中操作。"
    );
    let single_instance = single_instance::SingleInstance::new("VoiceBoxSingleInstance")?;
    anyhow::ensure!(
        single_instance.is_single(),
        "Another instance of VoiceBox is already running."
    );
    settings_writer::initialize()?;
    logging::init()?;
    gui::run()
}

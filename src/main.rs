#![windows_subsystem = "windows"]

mod apo_reg;
mod config_reg;
mod gui;
mod settings;
mod utils;

use tracing::{error, info};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::{PCWSTR, w};

fn main() {
    utils::init_logging();

    if let Err(e) = run() {
        error!("Application error: {e:?}");
        let message: Vec<_> = format!("{e:#}").encode_utf16().chain(Some(0)).collect();
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

fn run() -> anyhow::Result<()> {
    // The elevated registration helper runs without creating another GUI.
    if apo_reg::maybe_elevated_reg()? {
        info!("Elevated registration operation completed successfully");
        return Ok(());
    }

    anyhow::ensure!(
        std::env::args_os().len() == 1,
        "不支持此命令行参数，请在窗口中操作。"
    );
    let _single_instance = utils::check_single_instance()?;
    gui::run()
}

#![windows_subsystem = "windows"]

//! Management application: GUI, plugin setup, writable settings and file logs.

mod apo_cfg_writer;
mod audio;
mod database;
mod devices;
mod gui;
mod hotkeys;
mod i18n;
mod logging;
mod registry;
mod settings;
mod setup;
mod single;
mod soundboard;
mod startup;
mod static_data;
mod uninstall;
mod utils;

use tracing::error;
use windows::{
    Win32::UI::{
        Shell::IsUserAnAdmin,
        WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
    },
    core::{PCWSTR, w},
};

use voice_box_shared::utils::wide_null;

use crate::i18n::Text;

const SINGLE_INSTANCE_MUTEX: &str = r"Global\VoiceBoxSingleInstance";

fn startup_mode(arguments: &[std::ffi::OsString]) -> anyhow::Result<bool> {
    match arguments {
        [] => Ok(false),
        // [argument] if argument == startup::SILEN => Ok(true),
        _ => anyhow::bail!(Text::UnsupportedArguments),
    }
}

fn run(start_in_tray: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        unsafe { IsUserAnAdmin() }.as_bool(),
        Text::AdministratorRequired
    );
    let single_instance = single_instance::SingleInstance::new(SINGLE_INSTANCE_MUTEX)?;
    anyhow::ensure!(single_instance.is_single(), Text::AnotherInstance);
    apo_cfg_writer::init()?;
    logging::init()?;
    let settings = settings::Settings::load()?;
    i18n::set_language(settings.language_preference.resolve());
    gui::run(settings, start_in_tray)
}

fn main() {
    i18n::set_language(i18n::LanguagePreference::System.resolve());
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let mode = startup_mode(&arguments);
    let start_in_tray = matches!(&mode, Ok(true));
    if let Err(error) = mode.and_then(run) {
        error!("Application error: {error:?}");
        if !start_in_tray {
            let message = wide_null(i18n::Message::error(error).text().as_ref());
            unsafe {
                MessageBoxW(
                    None,
                    PCWSTR(message.as_ptr()),
                    w!("VoiceBox"),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
        std::process::exit(1);
    }
    tracing::info!("VoiceBox exited");
}

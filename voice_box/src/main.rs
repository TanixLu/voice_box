#![windows_subsystem = "windows"]

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

use anyhow::ensure;
use tracing::{error, info};

use crate::{
    i18n::{LanguagePreference, Message, Text, set_language},
    single::is_single_instance,
    startup::silent_start,
    utils::{win_error_popup, win_is_admin},
};

fn main() {
    match run() {
        Ok(_) => info!("VoiceBox exited successfully"),
        Err(err) => {
            error!("Application error: {err:?}");
            if !silent_start() {
                win_error_popup(&Message::error(err).text());
            }
            std::process::exit(1);
        }
    }
}

fn run() -> anyhow::Result<()> {
    set_language(LanguagePreference::System.resolve());
    ensure!(win_is_admin(), Text::AdministratorRequired);
    ensure!(is_single_instance(), Text::AnotherInstance);
    apo_cfg_writer::init()?; // 这里****************
    todo!()
}

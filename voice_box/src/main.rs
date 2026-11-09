#![windows_subsystem = "windows"]

mod audio;
mod database;
mod devices;
mod gui;
mod hotkeys;
mod i18n;
mod logging;
mod model;
mod registry;
mod settings;
mod setup;
mod soundboard;
mod startup;
mod static_data;
mod task;
mod uninstall;
mod utils;

use anyhow::ensure;
use tracing::{error, info};

use crate::{
    i18n::{LanguagePreference, Message, Text, set_language},
    startup::silent_start,
    utils::{is_single_instance, win_is_admin, win_popup_error},
};

fn main() {
    match run() {
        Ok(_) => info!("VoiceBox exited successfully"),
        Err(err) => {
            error!("Application error: {err:?}");
            if !silent_start() {
                win_popup_error(&Message::error(err).text());
            }
            std::process::exit(1);
        }
    }
}

fn run() -> anyhow::Result<()> {
    set_language(LanguagePreference::System.resolve());
    ensure!(win_is_admin(), Text::AdministratorRequired);
    ensure!(is_single_instance()?, Text::AnotherInstance);
    logging::init()?;
    database::init()?;
    let settings = settings::Settings::load()?;
    set_language(settings.language_preference.resolve());
    gui::run(settings, silent_start())
}

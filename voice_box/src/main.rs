#![windows_subsystem = "windows"]

mod audio;
mod database;
mod devices;
mod gui;
mod hotkeys;
mod i18n;
mod logging;
mod model;
mod paths;
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
use voice_box_shared::utils::win_try_create_named_mutex;

use crate::{
    i18n::{LanguagePreference, Message, Text, set_language},
    startup::silent_start,
    utils::{VOICE_BOX_SINGLE_INSTANCE, win_is_admin, win_popup_error},
};

fn main() {
    let silent = silent_start();
    match run(silent) {
        Ok(_) => info!("VoiceBox exited successfully"),
        Err(err) => {
            error!("Application error: {err:?}");
            if !silent {
                win_popup_error(&Message::error(err).text());
            }
            std::process::exit(1);
        }
    }
}

fn run(silent: bool) -> anyhow::Result<()> {
    set_language(LanguagePreference::System.resolve());
    ensure!(win_is_admin(), Text::AdministratorRequired);
    // Held until the GUI exits; the uninstall cleanup opens it before then.
    let instance = win_try_create_named_mutex(VOICE_BOX_SINGLE_INSTANCE)?;
    ensure!(instance.is_some(), Text::AnotherInstance);
    logging::init()?;
    database::init()?;
    let settings = settings::Settings::load()?;
    set_language(settings.language_preference.resolve());
    gui::run(settings, silent)
}

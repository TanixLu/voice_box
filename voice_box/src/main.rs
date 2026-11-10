#![windows_subsystem = "windows"]
//! VoiceBox: the GUI, soundboard and installer for the VoiceBox APO.
//!
//! Layers, each depending only on the ones above it:
//! - `model`: plain domain data (`Sound`, `Hotkey`); depends on nothing in the crate.
//! - Infrastructure: `audio` (WASAPI streams), `database`, `devices`, `hotkeys`, `i18n`,
//!   `paths`, `registry`, `settings`, `task`, `win`, and the system integrations
//!   `plugin`, `startup` and `uninstall`.
//! - Features: `soundboard` and `voice` own their rules and background work, and report
//!   results shaped for the view.
//! - `gui`: widgets and the app shell; it renders feature state and returns intents.
//!
//! Naming follows the workspace conventions in `voice_box_shared`: modules name one
//! concern (no `utils`), free functions read well qualified (`plugin::execute`,
//! `registry::read`), and types keep a domain prefix when imported bare (`PluginStatus`).

mod audio;
mod database;
mod devices;
mod embedded;
mod gui;
mod hotkeys;
mod i18n;
mod logging;
mod model;
mod paths;
mod plugin;
mod registry;
mod settings;
mod soundboard;
mod startup;
mod task;
mod uninstall;
mod voice;
mod win;

use anyhow::ensure;
use tracing::{error, info};
use voice_box_shared::win::try_create_named_mutex;

use crate::{
    i18n::{LanguagePreference, Message, Text, set_language},
    startup::silent_start,
};

/// Held by the running GUI; the uninstall cleanup waits for it to be released.
pub const SINGLE_INSTANCE_MUTEX: &str = r"Global\VoiceBoxSingleInstance";

fn main() {
    let silent = silent_start();
    match run(silent) {
        Ok(_) => info!("VoiceBox exited successfully"),
        Err(err) => {
            error!("Application error: {err:?}");
            if !silent {
                win::popup_error(&Message::error(err).text());
            }
            std::process::exit(1);
        }
    }
}

fn run(silent: bool) -> anyhow::Result<()> {
    set_language(LanguagePreference::System.resolve());
    let settings = settings::Settings::load()?;
    set_language(settings.language_preference.resolve());
    ensure!(win::is_admin(), Text::AdministratorRequired);
    // Held until the GUI exits; the uninstall cleanup opens it before then.
    let instance = try_create_named_mutex(SINGLE_INSTANCE_MUTEX)?;
    ensure!(instance.is_some(), Text::AnotherInstance);
    logging::init()?;
    database::init()?;
    gui::run(settings, silent)
}

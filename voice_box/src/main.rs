#![windows_subsystem = "windows"]
//! VoiceBox: the GUI, soundboard and installer for the VoiceBox APO.
//!
//! Layers, each depending only on the ones above it:
//! - `model`: plain domain data (`Sound`, `Hotkey`); depends on nothing in the crate.
//! - Infrastructure: `audio` (WASAPI streams), `database`, `devices`, `embedded`, `hotkeys`,
//!   `i18n`, `logging`, `paths`, `registry`, `task`, `win`, and the system integrations
//!   `plugin`, `startup` and `uninstall`.
//! - Features: `soundboard` and `voice` own their rules and background work, and report
//!   results shaped for the view.
//! - `settings`: the persisted app settings, which embed each feature's own settings.
//! - `gui`: widgets and the app shell; it renders feature state and returns intents. A panel
//!   may run a one-shot infrastructure call on a `Task` (saving, listing devices, installing
//!   the plugin) when no feature rule is involved.
//!
//! Naming follows the workspace conventions in `voice_box_shared`: modules name one concern
//! (no `utils`), a function called through its module never repeats it (`plugin::execute`,
//! `registry::read`), and bare-imported functions and types keep a domain word
//! (`is_valid_sound_name`, `PluginStatus`).
//!
//! Vocabulary, one name per concept:
//! - hotkey: a key combination (`Hotkey`), also as registered system-wide (`Hotkeys`).
//! - shortcut: a hotkey bound to a soundboard action (`Shortcuts`, `ShortcutAction`).
//! - binding: assigning a hotkey to a target (`bindings`, `Soundboard::bind`).
//! - delete: removing sounds from the library for good, never "remove".
//! - dialog: a modal `*Dialog`, never "tool".
//! - model: only the domain data in `model`; other state is named for what it holds.
//! - silent start: launched by the logon task, so errors are logged instead of shown and
//!   the window stays hidden until it is shown from the tray.

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
use voice_box_shared::win::{try_create_named_mutex, wide_null};
use windows::Win32::UI::{
    Shell::IsUserAnAdmin,
    WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
};
use windows_core::{PCWSTR, w};

use crate::i18n::{LanguagePreference, Message, Text, set_language};

/// Held by the running GUI; the uninstall cleanup waits for it to be released.
pub const SINGLE_INSTANCE_MUTEX: &str = r"Global\VoiceBoxSingleInstance";

fn main() {
    let silent_start = startup::is_silent_start();
    match run(silent_start) {
        Ok(_) => info!("VoiceBox exited successfully"),
        Err(err) => {
            error!("Application error: {err:?}");
            if !silent_start {
                popup_error(&Message::error(err).text());
            }
            std::process::exit(1);
        }
    }
}

fn run(silent_start: bool) -> anyhow::Result<()> {
    set_language(LanguagePreference::System.resolve());
    let settings = settings::Settings::load()?;
    set_language(settings.language_preference.resolve());
    ensure!(
        unsafe { IsUserAnAdmin() }.as_bool(),
        Text::AdministratorRequired
    );
    // Held until the GUI exits; the uninstall cleanup opens it before then.
    let instance = try_create_named_mutex(SINGLE_INSTANCE_MUTEX)?;
    ensure!(instance.is_some(), Text::AnotherInstance);
    logging::init()?;
    database::init()?;
    gui::run(settings, silent_start)
}

fn popup_error(message: &str) {
    let message = wide_null(message);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            w!("VoiceBox"),
            MB_OK | MB_ICONERROR,
        );
    }
}

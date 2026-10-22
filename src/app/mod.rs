//! Management application: GUI, plugin setup, writable settings and file logs.
use crate::app::i18n::Text;
mod app_settings;
mod audio;
mod devices;
mod gui;
mod hotkeys;
mod i18n;
mod logging;
mod registry;
mod settings_writer;
mod setup;
mod soundboard;
mod uninstall;

const UI_STATE_FILE: &str = "ui.ron";
const SINGLE_INSTANCE_MUTEX: &str = r"Global\VoiceBoxSingleInstance";
static COMPRESSED_APO_DLL: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/voice_box_apo.dll.lz4"));
static APO_DLL: std::sync::LazyLock<Box<[u8]>> = std::sync::LazyLock::new(|| {
    lz4_flex::block::decompress_size_prepended(COMPRESSED_APO_DLL)
        .expect("Failed to decompress embedded APO DLL")
        .into_boxed_slice()
});

fn apo_dll_bytes() -> &'static [u8] {
    APO_DLL.as_ref()
}

use crate::shared::utils::wide_null;
use tracing::error;
use windows::Win32::UI::{
    Shell::IsUserAnAdmin,
    WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
};
use windows::core::{PCWSTR, w};

pub fn run() {
    i18n::set_language(app_settings::load_language().resolve());
    if let Err(error) = run_inner() {
        error!("Application error: {error:?}");
        let message = wide_null(i18n::Message::error(error).text().as_ref());
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
    anyhow::ensure!(std::env::args_os().len() == 1, Text::UnsupportedArguments);
    anyhow::ensure!(
        unsafe { IsUserAnAdmin() }.as_bool(),
        Text::AdministratorRequired
    );
    let single_instance = single_instance::SingleInstance::new(SINGLE_INSTANCE_MUTEX)?;
    anyhow::ensure!(single_instance.is_single(), Text::AnotherInstance);
    settings_writer::initialize()?;
    logging::init()?;
    gui::run()
}

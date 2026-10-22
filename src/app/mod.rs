//! Management application: GUI, plugin setup, writable settings and file logs.
mod audio;
mod devices;
mod gui;
mod hotkeys;
mod logging;
mod registry;
mod settings_writer;
mod setup;
mod soundboard;
mod uninstall;

const UI_STATE_FILE: &str = "ui.ron";
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
    anyhow::ensure!(
        unsafe { IsUserAnAdmin() }.as_bool(),
        "VoiceBox 需要管理员权限，请右键选择“以管理员身份运行”。"
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

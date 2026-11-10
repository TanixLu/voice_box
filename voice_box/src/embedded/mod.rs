//! Files baked into the executable, exposed as raw data; the modules that use them
//! interpret them (`plugin` installs the DLL, `startup` fills the script template).

use std::sync::LazyLock;

static COMPRESSED_APO_DLL: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/voice_box_apo.dll.lz4"));
pub static APO_DLL: LazyLock<Box<[u8]>> = LazyLock::new(|| {
    lz4_flex::block::decompress_size_prepended(COMPRESSED_APO_DLL)
        .expect("UNREACHABLE: embedded APO DLL should be valid LZ4 data")
        .into_boxed_slice()
});

pub static ICON_PNG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon.png"));

/// Contains `__VOICEBOX_*__` placeholders that `startup` replaces.
pub static STARTUP_SCRIPT: &str = include_str!("startup.ps1");

pub static UNINSTALL_CLEANUP_SCRIPT: &str = include_str!("uninstall_cleanup.ps1");

pub static LICENSES: &str = concat!(
    "VoiceBox v",
    env!("CARGO_PKG_VERSION"),
    "\nCopyright (C) 2026 Tanix\n\n",
    "Project source: https://github.com/TanixLu/voice_box\n\n",
    "Original VoiceBox code is licensed under MIT OR Apache-2.0, at your option.\n",
    "Third-party components and model weights retain their respective licenses.\n\n",
    "===== VoiceBox: MIT License =====\n\n",
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../LICENSE-MIT")),
    "\n===== VoiceBox: Apache License 2.0 =====\n\n",
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../LICENSE-APACHE")),
    "\n===== Third-party licenses and notices =====\n\n",
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../THIRD_PARTY_LICENSES"
    )),
);

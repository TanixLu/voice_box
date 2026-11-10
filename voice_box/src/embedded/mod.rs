//! Files baked into the executable, exposed as raw data; the modules that use them
//! interpret them (`plugin` installs the DLL, `startup` fills the script template).

use std::sync::LazyLock;

fn decompress(data: &[u8]) -> Vec<u8> {
    lz4_flex::block::decompress_size_prepended(data)
        .expect("UNREACHABLE: embedded data should be valid LZ4 data")
}

pub static APO_DLL: LazyLock<Box<[u8]>> = LazyLock::new(|| {
    decompress(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/voice_box_apo.dll.lz4"
    )))
    .into()
});

pub static ICON_PNG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon.png"));

/// Contains `__VOICEBOX_*__` placeholders that `startup` replaces.
pub static STARTUP_SCRIPT: &str = include_str!("startup.ps1");

pub static UNINSTALL_CLEANUP_SCRIPT: &str = include_str!("uninstall_cleanup.ps1");

/// VoiceBox's own licenses followed by the third-party notices, assembled by `build.rs`.
pub static LICENSES: LazyLock<String> = LazyLock::new(|| {
    String::from_utf8(decompress(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/licenses.txt.lz4"
    ))))
    .expect("UNREACHABLE: embedded licenses should be UTF-8")
});

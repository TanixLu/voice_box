use crate::utils::base64_encode;

static COMPRESSED_APO_DLL: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/voice_box_apo.dll.lz4"));
static APO_DLL: std::sync::LazyLock<Box<[u8]>> = std::sync::LazyLock::new(|| {
    lz4_flex::block::decompress_size_prepended(COMPRESSED_APO_DLL)
        .expect("Failed to decompress embedded APO DLL")
        .into_boxed_slice()
});
pub fn apo_dll_bytes() -> &'static [u8] {
    APO_DLL.as_ref()
}

pub static ICON_PNG_DATA: &'static [u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon.png"));

pub static UNINSTALL_CLEANUP_SCRIPT: &'static str = include_str!("uninstall_cleanup.ps1");

static STARTUP_SCRIPT: &'static str = include_str!("startup.ps1");
pub fn startup_script(
    startup_argument: &str,
    hash: &str,
    path: &str,
) -> windows::core::Result<String> {
    // Keep paths as encoded data, never PowerShell source.
    Ok(STARTUP_SCRIPT
        .replace("__VOICEBOX_ARGUMENT__", startup_argument)
        .replace("__VOICEBOX_SHA256__", hash)
        .replace("__VOICEBOX_PATH__", &base64_encode(path.as_bytes())?))
}

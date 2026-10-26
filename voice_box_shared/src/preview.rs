//! Caller-owned float32 buffers for the DLL's offline preview ABI.
use windows_core::HRESULT;

pub const PROCESS_SYMBOL: &[u8] = b"VoiceBoxPreviewProcess\0";
pub const MAX_RECORD_SECONDS: u32 = 60;
pub const MAX_SAMPLES: usize = 16 * 1024 * 1024;

// The GUI and APO DLL share this signature. No Rust objects or allocations
// cross the DLL boundary; input/output contain frames * channels samples.
pub type Process = unsafe extern "system" fn(
    sample_rate: u32,
    channels: u32,
    frames: u32,
    gain_millidb: i32,
    denoising: u32,
    input: *const f32,
    output: *mut f32,
) -> HRESULT;

use std::{
    ffi::OsStr,
    ffi::OsString,
    fs::{self, OpenOptions},
    io,
    os::windows::ffi::OsStrExt,
    os::windows::ffi::OsStringExt,
    os::windows::io::{FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::ensure;
use atomic_write_file::AtomicWriteFile;
use rubato::{
    Async, FixedAsync, Resampler as _, SincInterpolationParameters, SincInterpolationType,
    WindowFunction, audioadapter_buffers::direct::SequentialSlice,
};
use windows::Win32::{
    Foundation::{ERROR_ALREADY_EXISTS, GetLastError},
    Media::Audio::Apo::{
        APO_FLAG, APO_FLAG_BITSPERSAMPLE_MUST_MATCH, APO_FLAG_FRAMESPERSECOND_MUST_MATCH,
        APO_FLAG_INPLACE, APO_FLAG_SAMPLESPERFRAME_MUST_MATCH, APO_REG_PROPERTIES,
        IAudioProcessingObject,
    },
    System::{
        Com::CoTaskMemFree, Performance::QueryPerformanceFrequency,
        SystemInformation::GetSystemDirectoryW, Threading::CreateMutexW,
    },
    UI::Shell::{FOLDERID_ProgramData, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
};
use windows_core::{GUID, HRESULT, Interface as _, PCWSTR};

#[macro_export]
macro_rules! field_name {
    ($obj:expr, $field:ident) => {{
        if false {
            let _ = &$obj.$field;
        }
        stringify!($field)
    }};
}

pub const APO_CLSID: GUID = GUID::from_u128(0xae672075_b244_4417_b5a5_fbd45b7f0268);
pub const APO_NAME: &str = "VoiceBox APO";
pub const COPYRIGHT: &str = "Copyright (C) 2026 Tanix";

pub const PREVIEW_PROCESS_SYMBOL: &[u8] = b"VoiceBoxPreviewProcess\0";
pub const PREVIEW_MAX_RECORD_SECONDS: u32 = 60;
pub const PREVIEW_MAX_SAMPLES: usize = 16 * 1024 * 1024;

// The GUI and APO DLL share this signature. No Rust objects or allocations
// cross the DLL boundary; input/output contain frames * channels samples.
pub type PreviewProcessFn = unsafe extern "system" fn(
    sample_rate: u32,
    channels: u32,
    frames: u32,
    gain_db: i8,
    denoising: u32,
    input: *const f32,
    output: *mut f32,
) -> HRESULT;

pub fn apo_registration_properties() -> APO_REG_PROPERTIES {
    let mut properties = APO_REG_PROPERTIES {
        clsid: APO_CLSID,
        Flags: APO_FLAG(
            APO_FLAG_INPLACE.0
                | APO_FLAG_SAMPLESPERFRAME_MUST_MATCH.0
                | APO_FLAG_FRAMESPERSECOND_MUST_MATCH.0
                | APO_FLAG_BITSPERSAMPLE_MUST_MATCH.0,
        ),
        u32MajorVersion: 1,
        u32MinorVersion: 0,
        u32MinInputConnections: 1,
        u32MaxInputConnections: 1,
        u32MinOutputConnections: 1,
        u32MaxOutputConnections: 1,
        u32MaxInstances: u32::MAX,
        u32NumAPOInterfaces: 1,
        iidAPOInterfaceList: [IAudioProcessingObject::IID],
        ..Default::default()
    };
    write_wide(&mut properties.szFriendlyName, APO_NAME);
    write_wide(&mut properties.szCopyrightInfo, COPYRIGHT);
    properties
}

fn write_wide(destination: &mut [u16], text: &str) {
    for (slot, value) in destination.iter_mut().zip(text.encode_utf16()) {
        *slot = value;
    }
}

pub fn wide_null(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

pub enum WriteMode {
    AtomicReplace,
    Append,
}

pub fn write_file_with_parents(
    path: impl AsRef<Path>,
    mode: WriteMode,
    write: impl FnOnce(&mut dyn io::Write) -> io::Result<()>,
) -> io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match mode {
        WriteMode::AtomicReplace => {
            let mut file = AtomicWriteFile::open(path)?;
            write(&mut file)?;
            file.commit()
        }
        WriteMode::Append => {
            let mut file = OpenOptions::new().create(true).append(true).open(path)?;
            write(&mut file)
        }
    }
}

// ===== System Directory Paths =====
pub fn system_dir() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut buffer = vec![0_u16; 260];
        loop {
            let length = unsafe { GetSystemDirectoryW(Some(buffer.as_mut_slice())) } as usize;
            if length == 0 {
                panic!(
                    "Failed to get system directory: {}",
                    io::Error::last_os_error()
                );
            }
            if length < buffer.len() {
                return PathBuf::from(OsString::from_wide(&buffer[..length]));
            }
            buffer.resize(length.max(buffer.len() + 1), 0);
        }
    })
}
pub fn conhost_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| system_dir().join("conhost.exe"))
}
pub fn powershell_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| system_dir().join(r"WindowsPowerShell\v1.0\powershell.exe"))
}
pub fn apo_dll_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| system_dir().join("voice_box_apo.dll"))
}

// ===== Data Directory Paths =====
pub fn data_dir() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| {
        let folder = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, KF_FLAG_DEFAULT, None) }
            .unwrap_or_else(|e| panic!("Failed to get ProgramData directory: {}", e));
        let path = unsafe { PathBuf::from(OsString::from_wide(folder.as_wide())) };
        unsafe {
            CoTaskMemFree(Some(folder.0.cast()));
        }
        path.join("VoiceBox")
    })
}
pub fn apo_cfg_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| data_dir().join("apo.cfg"))
}
pub fn database_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| data_dir().join("voice_box.db"))
}
pub fn ui_state_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| data_dir().join("voice_box.ron"))
}
pub fn log_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| data_dir().join("voice_box.log"))
}
pub fn rotated_log_path() -> &'static Path {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| data_dir().join("voice_box.log.1"))
}

pub fn transport_name(guid: &str) -> anyhow::Result<String> {
    let id = guid
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(guid);
    ensure!(GUID::try_from(id).is_ok(), "Invalid microphone ID");
    Ok(format!(r"Global\VoiceBox.Pcm.{}", id.to_ascii_lowercase()))
}

pub fn win_qpf() -> i64 {
    static CACHE: OnceLock<i64> = OnceLock::new();
    *CACHE.get_or_init(|| {
        let mut frequency = 0;
        unsafe {
            QueryPerformanceFrequency(&mut frequency)
                .unwrap_or_else(|e| panic!("Failed to query performance frequency: {}", e));
        }
        if frequency <= 0 {
            panic!("Failed to query performance frequency");
        }
        frequency
    })
}

/// Creates a named mutex and keeps it alive until the returned handle is dropped.
/// Returns `None` if it already exists. Uses object existence, not mutex ownership.
pub fn win_try_create_named_mutex(name: &str) -> windows_core::Result<Option<OwnedHandle>> {
    let name = wide_null(name);
    let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }?;
    // Read the status before another Win32 call can overwrite it.
    let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    // SAFETY: CreateMutexW returned a valid, uniquely owned handle.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
    if already_exists {
        Ok(None)
    } else {
        Ok(Some(handle))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GainDb(i8);

impl GainDb {
    pub const MIN_VALUE: i8 = -60;
    pub const MAX_VALUE: i8 = 60;

    pub fn new(value: i8) -> Self {
        Self(value.clamp(Self::MIN_VALUE, Self::MAX_VALUE))
    }

    pub fn as_i8(&self) -> i8 {
        self.0
    }

    pub fn as_f32_gain(&self) -> f32 {
        if self.0 == Self::MIN_VALUE {
            0.0
        } else {
            10.0_f32.powf(self.0 as f32 / 20.0)
        }
    }
}

pub const MIN_SAMPLE_RATE: f64 = 8_000.0;
pub const MAX_SAMPLE_RATE: f64 = 192_000.0;
pub const MAX_CHANNELS: usize = 32;

/// Reject invalid samples without clipping floating-point processing headroom.
pub fn finite_sample(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

/// Linked-channel output limiting with immediate attack and no added latency.
pub struct PeakLimiter {
    gain: f64,
    release_step: f64,
}

impl PeakLimiter {
    // -1 dBFS, rounded down to leave room for f32 rounding.
    const CEILING: f32 = 0.891_250_9;

    pub fn new(sample_rate: f64) -> Self {
        Self {
            gain: 1.0,
            release_step: -(-1.0 / (sample_rate * 0.05)).exp_m1(),
        }
    }

    pub fn process_frame(&mut self, frame: &mut [f32]) {
        let mut peak = 0.0_f32;
        for sample in frame.iter_mut() {
            *sample = finite_sample(*sample);
            peak = peak.max(sample.abs());
        }
        let safe_gain = if peak > Self::CEILING {
            f64::from(Self::CEILING) / f64::from(peak)
        } else {
            1.0
        };
        self.gain = (self.gain + (1.0 - self.gain) * self.release_step).min(safe_gain);
        if safe_gain == 1.0 && (1.0 - self.gain) < 1e-5 {
            self.gain = 1.0;
        }
        for sample in frame {
            // The clamp only guards rounding; the linked gain controls peaks.
            *sample =
                ((f64::from(*sample) * self.gain) as f32).clamp(-Self::CEILING, Self::CEILING);
        }
    }
}

#[derive(Clone, Copy)]
pub enum SampleFormat {
    Pcm { bytes: u8, valid_bits: u8 },
    Float32,
    Float64,
}

impl SampleFormat {
    pub fn pcm(bytes: u32, valid_bits: u32) -> Option<Self> {
        if (1..=4).contains(&bytes) && valid_bits > 0 && valid_bits <= bytes * 8 {
            Some(Self::Pcm {
                bytes: bytes as u8,
                valid_bits: valid_bits as u8,
            })
        } else {
            None
        }
    }

    pub fn bytes(self) -> usize {
        match self {
            Self::Pcm { bytes, .. } => bytes as usize,
            Self::Float32 => 4,
            Self::Float64 => 8,
        }
    }

    /// # Panics
    /// Panics if `sample.len()` does not equal `self.bytes()`.
    pub fn decode(self, sample: &[u8]) -> f64 {
        let value = match self {
            Self::Float32 => {
                f64::from(f32::from_le_bytes(sample.try_into().expect(
                    "UNREACHABLE: Float32 sample should contain exactly 4 bytes",
                )))
            }
            Self::Float64 => f64::from_le_bytes(
                sample
                    .try_into()
                    .expect("UNREACHABLE: Float64 sample should contain exactly 8 bytes"),
            ),
            Self::Pcm { bytes, valid_bits } => {
                let input = if bytes == 1 {
                    i32::from(sample[0]) - 128
                } else {
                    let mut encoded = [0; 4];
                    encoded[..bytes as usize].copy_from_slice(sample);
                    let extension = (4 - bytes) * 8;
                    (i32::from_le_bytes(encoded) << extension) >> extension
                } >> (bytes * 8 - valid_bits);
                f64::from(input) / (1_i64 << (valid_bits - 1)) as f64
            }
        };
        if value.is_finite() {
            value.clamp(-1.0, 1.0)
        } else {
            0.0
        }
    }

    pub fn encode(self, sample: &mut [u8], value: f64) {
        let value = if value.is_finite() {
            value.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        match self {
            Self::Float32 => sample.copy_from_slice(&(value as f32).to_le_bytes()),
            Self::Float64 => sample.copy_from_slice(&value.to_le_bytes()),
            Self::Pcm { bytes, valid_bits } => {
                let limit = (1_i64 << (valid_bits - 1)) as f64;
                let output = ((value * limit).round().clamp(-limit, limit - 1.0) as i64)
                    << (bytes * 8 - valid_bits);
                if bytes == 1 {
                    sample[0] = (output + 128) as u8;
                } else {
                    sample.copy_from_slice(&(output as i32).to_le_bytes()[..bytes as usize]);
                }
            }
        }
    }
}

/// Rubato sinc conversion with fixed one-millisecond input chunks and buffers.
pub struct Resampler {
    engine: Option<Async<f32>>,
    input: Vec<f32>,
    output: Vec<f32>,
    pending: usize,
    read: usize,
    available: usize,
    delay_seconds: f64,
}

impl Resampler {
    pub fn new(input_rate: f64, output_rate: f64) -> Self {
        if input_rate == output_rate {
            return Self {
                engine: None,
                input: vec![0.0],
                output: vec![0.0],
                pending: 0,
                read: 0,
                available: 0,
                delay_seconds: 0.0,
            };
        }
        let ratio = output_rate / input_rate;
        let parameters = SincInterpolationParameters {
            sinc_len: (64.0 / ratio.min(1.0)).ceil() as usize,
            f_cutoff: Some(0.9),
            oversampling_factor: 128,
            interpolation: SincInterpolationType::Linear,
            window: WindowFunction::Blackman,
        };
        let engine = Async::new_sinc(
            ratio,
            1.0,
            &parameters,
            (input_rate / 1000.0).ceil() as usize,
            1,
            FixedAsync::Input,
        )
        .expect("UNREACHABLE: validated rates and fixed parameters should create a resampler");
        // Account for Rubato's sinc-table phase offset and initial index advance.
        let sinc_len = parameters.sinc_len.next_multiple_of(8);
        let delay_seconds = (sinc_len as f64 / 2.0 - 1.0 / parameters.oversampling_factor as f64)
            / input_rate
            - 1.0 / output_rate;
        Self {
            input: vec![0.0; engine.input_frames_max()],
            output: vec![0.0; engine.output_frames_max()],
            engine: Some(engine),
            pending: 0,
            read: 0,
            available: 0,
            delay_seconds,
        }
    }

    pub fn delay_seconds(&self) -> f64 {
        self.delay_seconds
    }
    pub fn input_chunk_size(&self) -> usize {
        if self.engine.is_some() {
            self.input.len()
        } else {
            0
        }
    }

    pub fn reset(&mut self) {
        if let Some(engine) = &mut self.engine {
            engine.reset();
        }
        self.input.fill(0.0);
        self.output.fill(0.0);
        self.pending = 0;
        self.read = 0;
        self.available = 0;
    }

    pub fn push(&mut self, sample: f32) {
        self.input[self.pending] = sample;
        self.pending += 1;
        if self.pending != self.input.len() {
            return;
        }
        assert_eq!(
            self.read, self.available,
            "previous resampler output must be drained"
        );
        self.pending = 0;
        self.read = 0;
        self.available = if let Some(engine) = &mut self.engine {
            let input = SequentialSlice::new(&self.input, 1, self.input.len())
                .expect("UNREACHABLE: mono input view should fit its backing slice");
            let output_len = self.output.len();
            let mut output = SequentialSlice::new_mut(&mut self.output, 1, output_len)
                .expect("UNREACHABLE: mono output view should fit its backing slice");
            let (consumed, produced) = engine
                .process_into_buffer(&input, &mut output, None)
                .expect("UNREACHABLE: preallocated buffers should satisfy resampler requirements");
            debug_assert_eq!(consumed, self.input.len());
            produced
        } else {
            self.output[0] = sample;
            1
        };
    }

    pub fn next_sample(&mut self) -> Option<f32> {
        if self.read == self.available {
            return None;
        }
        let sample = self.output[self.read];
        self.read += 1;
        Some(sample)
    }
}

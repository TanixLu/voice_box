//! Single-writer / multiple-reader PCM broadcast. Only background
//! workers access the file mapping; the APO copies into its own locked memory.
use anyhow::ensure;
use std::{
    fs::{File, OpenOptions},
    mem::size_of,
    os::windows::io::AsRawHandle,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering::SeqCst},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            Memory::{
                CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS,
                MapViewOfFile, PAGE_READONLY, PAGE_READWRITE, UnmapViewOfFile,
            },
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
        },
    },
    core::PCWSTR,
};

pub const SAMPLE_RATE: u32 = 48_000;
pub const MAX_VOLUME: f32 = 2.0;
pub const BUFFER_CAPACITY_FRAMES: usize = 32_768;
pub const DECODE_AHEAD_FRAMES: u64 = 12_000; // 250 ms of decoded audio, bounded independently of file length.
const MAGIC: u64 = u64::from_le_bytes(*b"VBPCM\0\0\0");

#[derive(Clone, Copy)]
pub struct Clock {
    frequency: i64,
}
impl Clock {
    pub fn new() -> anyhow::Result<Self> {
        let mut frequency = 0;
        unsafe {
            QueryPerformanceFrequency(&mut frequency)?;
        }
        ensure!(frequency > 0, "系统音频时钟不可用");
        Ok(Self { frequency })
    }
    pub fn frame(self) -> u64 {
        let mut ticks = 0;
        // QPC is system-wide and monotonic, including across the EXE/APO boundary.
        if unsafe { QueryPerformanceCounter(&mut ticks) }.is_err() {
            return 0;
        }
        (i128::from(ticks.max(0)) * i128::from(SAMPLE_RATE) / i128::from(self.frequency)) as u64
    }
}

#[repr(C, align(8))]
#[derive(Default)]
pub struct Slot {
    stamp: AtomicU64,
    stereo: AtomicU64,
}
impl Slot {
    pub fn write(&self, frame: u64, generation: u64, samples: [f32; 2]) {
        self.stamp.store(0, SeqCst);
        let bits = u64::from(samples[0].to_bits()) | (u64::from(samples[1].to_bits()) << 32);
        self.stereo.store(bits, SeqCst);
        self.stamp.store(tag(frame, generation), SeqCst);
    }
    pub fn read(&self, frame: u64, generation: u64) -> [f32; 2] {
        let expected = tag(frame, generation);
        if generation == 0 || self.stamp.load(SeqCst) != expected {
            return [0.0; 2];
        }
        let bits = self.stereo.load(SeqCst);
        if self.stamp.load(SeqCst) != expected {
            return [0.0; 2];
        }
        [
            sanitize_sample(f32::from_bits(bits as u32)),
            sanitize_sample(f32::from_bits((bits >> 32) as u32)),
        ]
    }
}
fn tag(frame: u64, generation: u64) -> u64 {
    (frame << 16) | (generation & 0xffff)
}
pub fn sanitize_sample(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

#[repr(C, align(8))]
struct Header {
    magic: AtomicU64,
    generation: AtomicU64,
    heartbeat: AtomicU64,
    end: AtomicU64,
    gain: AtomicU64,
    mute_microphone: AtomicU64,
}

pub fn transport_path(guid: &str) -> anyhow::Result<PathBuf> {
    // Never derive an arbitrary filesystem path from endpoint or IPC input.
    let id = guid.trim_matches(['{', '}']);
    ensure!(
        id.len() == 36 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
        "麦克风标识无效"
    );
    Ok(super::paths::data_dir()?.join(format!("soundboard-{}.pcm", id.to_ascii_lowercase())))
}

pub struct Mapping {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    handle: HANDLE,
    _file: File,
    writer: bool,
}
impl Mapping {
    pub fn open(path: &Path, writer: bool) -> anyhow::Result<Self> {
        let bytes = size_of::<Header>() + BUFFER_CAPACITY_FRAMES * size_of::<Slot>();
        let file = OpenOptions::new()
            .read(true)
            .write(writer)
            .create(writer)
            .truncate(false)
            .open(path)?;
        if writer {
            // An existing reader can retain its mapping across GUI restarts. Never
            // truncate a live mapping; fixed-size atomic storage is reused instead.
            if file.metadata()?.len() == 0 {
                file.set_len(bytes as u64)?;
            }
        }
        ensure!(file.metadata()?.len() == bytes as u64, "播放缓冲区布局无效");
        let handle = unsafe {
            CreateFileMappingW(
                HANDLE(file.as_raw_handle()),
                None,
                if writer {
                    PAGE_READWRITE
                } else {
                    PAGE_READONLY
                },
                0,
                0,
                PCWSTR::null(),
            )?
        };
        let view = unsafe {
            MapViewOfFile(
                handle,
                if writer {
                    FILE_MAP_READ | FILE_MAP_WRITE
                } else {
                    FILE_MAP_READ
                },
                0,
                0,
                bytes,
            )
        };
        if view.Value.is_null() {
            unsafe {
                let _ = CloseHandle(handle);
            }
            anyhow::bail!("映射播放缓冲区失败");
        }
        let result = Self {
            view,
            handle,
            _file: file,
            writer,
        };
        if writer {
            result.stop();
            result.header().magic.store(MAGIC, SeqCst);
        } else {
            ensure!(
                result.header().magic.load(SeqCst) == MAGIC,
                "播放缓冲区标识无效"
            );
        }
        Ok(result)
    }
    fn header(&self) -> &Header {
        unsafe { &*self.view.Value.cast::<Header>() }
    }
    fn slot(&self, frame: u64) -> &Slot {
        unsafe {
            &*self
                .view
                .Value
                .cast::<u8>()
                .add(size_of::<Header>())
                .cast::<Slot>()
                .add(frame as usize % BUFFER_CAPACITY_FRAMES)
        }
    }
    pub fn write(&self, frame: u64, generation: u64, samples: [f32; 2]) {
        self.slot(frame).write(frame, generation, samples);
    }
    pub fn read(&self, frame: u64, generation: u64) -> [f32; 2] {
        self.slot(frame).read(frame, generation)
    }
    pub fn publish(&self, generation: u64, end: u64, gain: f32, mute_microphone: bool, now: u64) {
        let gain = sanitize_gain(gain);
        self.header().gain.store(u64::from(gain.to_bits()), SeqCst);
        self.header().end.store(end, SeqCst);
        self.header()
            .mute_microphone
            .store(u64::from(mute_microphone && gain > 0.0), SeqCst);
        self.header().heartbeat.store(now, SeqCst);
        self.header().generation.store(generation, SeqCst);
    }
    pub fn status(&self, now: u64) -> (u64, u64, f32, bool) {
        let h = self.header();
        let heartbeat = h.heartbeat.load(SeqCst);
        if now.abs_diff(heartbeat) > u64::from(SAMPLE_RATE) / 2 {
            return (0, 0, 0.0, false);
        }
        let gain = sanitize_gain(f32::from_bits(h.gain.load(SeqCst) as u32));
        (
            h.generation.load(SeqCst),
            h.end.load(SeqCst),
            gain,
            h.mute_microphone.load(SeqCst) != 0,
        )
    }
    pub fn stop(&self) {
        self.header().generation.store(0, SeqCst);
    }
}

fn sanitize_gain(gain: f32) -> f32 {
    if gain.is_finite() {
        gain.clamp(0.0, MAX_VOLUME)
    } else {
        0.0
    }
}
impl Drop for Mapping {
    fn drop(&mut self) {
        if self.writer {
            self.stop();
        }
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.handle);
        }
    }
}

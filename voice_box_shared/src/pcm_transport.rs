//! Single-writer / multiple-reader PCM broadcast. Only background
//! workers access the file mapping; the APO copies into its own locked memory.

use std::{
    fs::{File, OpenOptions},
    mem::size_of,
    os::windows::io::AsRawHandle,
    path::Path,
    sync::atomic::{
        AtomicU64,
        Ordering::{Acquire, Relaxed, Release},
    },
};

use anyhow::ensure;
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
pub const BUFFER_CAPACITY_FRAMES: usize = 32_768;
pub const DECODE_AHEAD_FRAMES: u64 = 12_000; // 250 ms of decoded audio, bounded independently of file length.
const MAGIC: u64 = u64::from_le_bytes(*b"VBPCM\0\0\0");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GainPercent(u16);

impl GainPercent {
    pub const ZERO: Self = Self(0);
    pub const MAX_VALUE: u16 = 200;

    pub fn new(value: u16) -> Self {
        Self(value.min(Self::MAX_VALUE))
    }

    pub fn is_zero(&self) -> bool {
        self.0 == 0
    }

    pub fn as_u16(&self) -> u16 {
        self.0
    }

    pub fn as_f32_gain(&self) -> f32 {
        self.0 as f32 / 100.0
    }

    pub fn to_u64_bits(&self) -> u64 {
        self.0 as u64
    }

    pub fn from_u64_bits(bits: u64) -> Self {
        Self::new(bits as u16)
    }
}

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
        ensure!(frequency > 0, "The system audio clock is unavailable");
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
    // Each slot has one writer. Tags must not be reused while a reader could
    // still be validating an earlier write with the same tag.
    pub fn write(&self, frame: u64, generation: u64, samples: [f32; 2]) {
        self.stamp.store(0, Relaxed);
        let bits = u64::from(samples[0].to_bits()) | (u64::from(samples[1].to_bits()) << 32);
        // Release the invalidation with the samples, so an overwritten payload
        // cannot pass the reader's final check under the previous tag.
        self.stereo.store(bits, Release);
        self.stamp.store(tag(frame, generation), Release);
    }
    pub fn read(&self, frame: u64, generation: u64) -> [f32; 2] {
        let expected = tag(frame, generation);
        // Reading the tag acquires its samples; reading newer samples acquires
        // their preceding stamp invalidation. Both edges are needed.
        if generation == 0 || self.stamp.load(Acquire) != expected {
            return [0.0; 2];
        }
        let bits = self.stereo.load(Acquire);
        if self.stamp.load(Relaxed) != expected {
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
    gain_percent: AtomicU64,
    include_microphone: AtomicU64,
}

pub struct Mapping {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    handle: HANDLE,
    _file: File,
    writer: bool,
}
impl Mapping {
    pub fn open(path: &Path, writer: bool) -> anyhow::Result<Self> {
        const SIZE: usize = size_of::<Header>() + BUFFER_CAPACITY_FRAMES * size_of::<Slot>();
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
                file.set_len(SIZE as u64)?;
            }
        }
        ensure!(
            file.metadata()?.len() == SIZE as u64,
            "Invalid playback buffer layout"
        );
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
                SIZE,
            )
        };
        if view.Value.is_null() {
            unsafe {
                let _ = CloseHandle(handle);
            }
            anyhow::bail!("Failed to map the playback buffer");
        }
        let result = Self {
            view,
            handle,
            _file: file,
            writer,
        };
        if writer {
            result.stop();
            result.header().magic.store(MAGIC, Release);
        } else {
            ensure!(
                result.header().magic.load(Acquire) == MAGIC,
                "Invalid playback buffer identifier"
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
    pub fn publish(
        &self,
        generation: u64,
        end: u64,
        gain_percent: GainPercent,
        include_microphone: bool,
        now: u64,
    ) {
        self.header()
            .gain_percent
            .store(gain_percent.to_u64_bits(), Relaxed);
        self.header().end.store(end, Relaxed);
        self.header()
            .include_microphone
            .store(include_microphone as u64, Relaxed);
        // Either publication point makes the preceding PCM and metadata visible.
        self.header().heartbeat.store(now, Release);
        self.header().generation.store(generation, Release);
    }
    pub fn status(&self, now: u64) -> (u64, u64, GainPercent, bool) {
        let h = self.header();
        // Acquire publication before reading its scalar metadata. The fields can
        // still span consecutive publications; this is not a coherent snapshot.
        let generation = h.generation.load(Acquire);
        let heartbeat = h.heartbeat.load(Acquire);
        if now.abs_diff(heartbeat) > u64::from(SAMPLE_RATE) / 2 {
            return (0, 0, GainPercent::ZERO, false);
        }
        let gain_percent = GainPercent::from_u64_bits(h.gain_percent.load(Relaxed));
        (
            generation,
            h.end.load(Relaxed),
            gain_percent,
            h.include_microphone.load(Relaxed) != 0,
        )
    }
    pub fn stop(&self) {
        self.header().generation.store(0, Relaxed);
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

//! Single-writer / multiple-reader PCM broadcast. Only background
//! workers access the named shared memory; the APO copies into its own locked memory.
//! Samples retain floating-point headroom; output limiting belongs after mixing.

use std::{
    mem::size_of,
    os::windows::io::OwnedHandle,
    sync::atomic::{
        AtomicU64,
        Ordering::{Acquire, Relaxed, Release},
        fence,
    },
};

use anyhow::ensure;
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
            LocalFree,
        },
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        },
        System::{
            Memory::{
                CreateFileMappingW, FILE_MAP, FILE_MAP_READ, FILE_MAP_WRITE,
                MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW, PAGE_READWRITE,
                UnmapViewOfFile,
            },
            Performance::QueryPerformanceCounter,
        },
    },
    core::{PCWSTR, w},
};

use crate::{
    audio::finite_sample,
    utils::{wide_null, win_qpf, win_try_create_named_mutex},
};

pub const PCM_SAMPLE_RATE: u32 = 48_000;

const MAGIC: u64 = u64::from_le_bytes(*b"VoiceBox");
pub const DECODE_AHEAD_FRAMES: u64 = 12_000; // 250 ms of decoded audio, bounded independently of file length.
const BUFFER_CAPACITY_FRAMES: usize = 32_768;
const MAPPING_SIZE: usize = size_of::<Header>() + BUFFER_CAPACITY_FRAMES * size_of::<Slot>();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HeaderState(u64);
impl HeaderState {
    // High to low: sequence (32), next generation (16), published generation (16).
    fn new(sequence: u32, next_generation: u16, generation: u16) -> Self {
        Self(
            (u64::from(sequence) << 32)
                | (u64::from(next_generation) << 16)
                | u64::from(generation),
        )
    }
    fn sequence(self) -> u32 {
        (self.0 >> 32) as u32
    }
    fn next_generation(self) -> u16 {
        (self.0 >> 16) as u16
    }
    fn generation(self) -> u16 {
        self.0 as u16
    }
    fn is_writing(self) -> bool {
        self.sequence() & 1 != 0
    }
}

#[repr(C, align(8))]
struct Header {
    magic: AtomicU64,
    state: AtomicU64,
    heartbeat: AtomicU64,
    end: AtomicU64,
}
impl Header {
    // All mutations are serialized by the channel's single writer.
    fn next_generation(&self) -> u16 {
        let state = HeaderState(self.state.load(Relaxed));
        // A zero-initialized mapping starts at 1; zero always means stopped.
        let generation = state.next_generation().max(1);
        self.state.store(
            HeaderState::new(
                state.sequence().wrapping_add(2),
                generation.wrapping_add(1).max(1),
                state.generation(),
            )
            .0,
            Release,
        );
        generation
    }
    fn publish(&self, generation: u16, end: u64, now: u64) {
        let state = HeaderState(self.state.load(Relaxed));
        let sequence = state.sequence().wrapping_add(1);
        self.state.store(
            HeaderState::new(sequence, state.next_generation(), state.generation()).0,
            Relaxed,
        );
        // Each payload Release carries the preceding odd-sequence invalidation.
        // Acquiring either new field makes the reader's final state check fail.
        self.end.store(end, Release);
        self.heartbeat.store(now, Release);
        self.state.store(
            HeaderState::new(
                sequence.wrapping_add(1),
                state.next_generation(),
                generation,
            )
            .0,
            Release,
        );
    }
    fn stop(&self) {
        let state = HeaderState(self.state.load(Relaxed));
        // Also recover an odd sequence left by a crashed writer on open. Keep
        // the allocation counter, and never revive its partially written data.
        self.state.store(
            HeaderState::new(
                (state.sequence() | 1).wrapping_add(1),
                state.next_generation(),
                0,
            )
            .0,
            Release,
        );
    }
    fn status(&self, now: u64) -> (u16, u64) {
        // A writer can be descheduled or crash with an odd sequence. Bound the
        // retries and remain silent until a later poll finds a stable snapshot.
        for _ in 0..4 {
            let before = HeaderState(load_acquire_readonly(&self.state));
            if before.is_writing() {
                continue;
            }
            let heartbeat = load_acquire_readonly(&self.heartbeat);
            let end = load_acquire_readonly(&self.end);
            if self.state.load(Relaxed) != before.0 {
                continue;
            }
            if before.generation() == 0 || now.abs_diff(heartbeat) > u64::from(PCM_SAMPLE_RATE) / 2
            {
                return (0, 0);
            }
            return (before.generation(), end);
        }
        (0, 0)
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
    pub fn write(&self, frame: u64, generation: u16, samples: [f32; 2]) {
        self.stamp.store(0, Relaxed);
        let bits = u64::from(samples[0].to_bits()) | (u64::from(samples[1].to_bits()) << 32);
        // Release the invalidation with the samples, so an overwritten payload
        // cannot pass the reader's final check under the previous tag.
        self.stereo.store(bits, Release);
        self.stamp.store(make_stamp(frame, generation), Release);
    }
    pub fn read(&self, frame: u64, generation: u16) -> [f32; 2] {
        let expected = make_stamp(frame, generation);
        // Reading the tag acquires its samples; reading newer samples acquires
        // their preceding stamp invalidation. Both edges are needed.
        if generation == 0 || load_acquire_readonly(&self.stamp) != expected {
            return [0.0; 2];
        }
        let bits = load_acquire_readonly(&self.stereo);
        if self.stamp.load(Relaxed) != expected {
            return [0.0; 2];
        }
        [
            finite_sample(f32::from_bits(bits as u32)),
            finite_sample(f32::from_bits((bits >> 32) as u32)),
        ]
    }
}
fn make_stamp(frame: u64, generation: u16) -> u64 {
    (frame << 16) | (generation as u64)
}
pub fn pcm_frame() -> u64 {
    let mut ticks = 0;
    if unsafe { QueryPerformanceCounter(&mut ticks) }.is_err() {
        0
    } else {
        (i128::from(ticks.max(0)) * i128::from(PCM_SAMPLE_RATE) / i128::from(win_qpf())) as u64
    }
}

// On x86-64, 64-bit Relaxed atomic loads are guaranteed
// to work on read-only mappings. An Acquire fence preserves
// synchronization with the writer's Release stores.
fn load_acquire_readonly(value: &AtomicU64) -> u64 {
    let result = value.load(Relaxed);
    fence(Acquire);
    result
}

struct Mapping {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    handle: HANDLE,
}
impl Mapping {
    fn map(handle: HANDLE, access: FILE_MAP) -> anyhow::Result<Self> {
        let view = unsafe { MapViewOfFile(handle, access, 0, 0, MAPPING_SIZE) };
        if view.Value.is_null() {
            unsafe {
                let _ = CloseHandle(handle);
            }
            anyhow::bail!("Failed to map the playback buffer");
        }
        Ok(Self { view, handle })
    }
    fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            load_acquire_readonly(&self.header().magic) == MAGIC,
            "Invalid playback buffer identifier"
        );
        Ok(())
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
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.handle);
        }
    }
}

pub struct PcmWriter {
    mapping: Mapping,
    // Fields drop in declaration order: release the mapping before admitting a new writer.
    _writer_mutex: OwnedHandle,
}
impl PcmWriter {
    pub fn open(name: &str) -> anyhow::Result<Self> {
        let _writer_mutex = win_try_create_named_mutex(&format!("{name}.Writer"))?
            .ok_or_else(|| anyhow::anyhow!("PCM channel already has a writer: {name}"))?;
        let name = wide_null(name);
        // Administrators and the owner can write. The audio host can read,
        // including through Audiosrv's restricted service SID.
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w!(
                    "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)(A;;GR;;;LS)(A;;GR;;;S-1-5-80-2676549577-1911656217-2625096541-4178041876-1366760775)"
                ),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )?;
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                Some(&attributes),
                PAGE_READWRITE,
                0,
                MAPPING_SIZE as u32,
                PCWSTR(name.as_ptr()),
            )
        };
        // Capture last error before freeing the temporary security descriptor.
        let created = unsafe { GetLastError() } != ERROR_ALREADY_EXISTS;
        unsafe {
            let _ = LocalFree(Some(HLOCAL(descriptor.0)));
        }
        let mapping = Mapping::map(handle?, FILE_MAP_READ | FILE_MAP_WRITE)?;
        if created {
            // New paging-file-backed mappings are zero initialized. Publish ready
            // last; readers that race initialization retry on their next poll.
            mapping.header().magic.store(MAGIC, Release);
        } else {
            mapping.validate()?;
        }
        // A reader can retain the same mapping across GUI restarts. Reuse its
        // atomic storage without clearing memory underneath active readers.
        let mut result = Self {
            mapping,
            _writer_mutex,
        };
        result.stop();
        Ok(result)
    }
    pub fn write(&mut self, frame: u64, generation: u16, samples: [f32; 2]) {
        self.mapping.slot(frame).write(frame, generation, samples);
    }
    pub fn next_generation(&mut self) -> u16 {
        self.mapping.header().next_generation()
    }
    pub fn publish(&mut self, generation: u16, end: u64, now: u64) {
        self.mapping.header().publish(generation, end, now);
    }
    pub fn stop(&mut self) {
        self.mapping.header().stop();
    }
}

impl Drop for PcmWriter {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct PcmReader {
    mapping: Mapping,
}
impl PcmReader {
    pub fn open(name: &str) -> anyhow::Result<Self> {
        let name = wide_null(name);
        let handle = unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, PCWSTR(name.as_ptr()))? };
        let mapping = Mapping::map(handle, FILE_MAP_READ)?;
        mapping.validate()?;
        Ok(Self { mapping })
    }
    pub fn read(&self, frame: u64, generation: u16) -> [f32; 2] {
        self.mapping.slot(frame).read(frame, generation)
    }
    pub fn status(&self, now: u64) -> (u16, u64) {
        self.mapping.header().status(now)
    }
}

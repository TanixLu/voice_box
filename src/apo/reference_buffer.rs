//! In-process, bounded, multi-reader playback reference. The capture worker is
//! the only producer; APOProcess reads atomics without waiting or allocating.
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use windows::Win32::System::{
    Memory::{VirtualLock, VirtualUnlock},
    SystemInformation::GetTickCount64,
};

pub const RATE: usize = 16_000;
pub const BLOCK: usize = 64;
pub const CHANNELS: usize = 2;
pub const CAPACITY: usize = 512;
pub const IDLE: u32 = 0;
pub const RUNNING: u32 = 1;
pub const FAILED: u32 = 2;
const MAX_AGE_MS: u64 = 250;

#[repr(C, align(64))]
struct Slot {
    sequence: AtomicU64,
    timestamp_hns: AtomicU64,
    samples: [AtomicU32; BLOCK * CHANNELS],
}
struct State {
    status: AtomicU32,
    error: AtomicU32,
    epoch: AtomicU64,
    latest: AtomicU64,
    packet_tick: AtomicU64,
    capture_tick: AtomicU64,
    worker_tick: AtomicU64,
    slots: Box<[Slot]>,
}
pub(super) struct Reference {
    state: State,
    locked: bool,
}
impl Reference {
    pub fn new() -> Self {
        // Build slots individually on the heap, avoiding a ~288 KiB stack value.
        // Initialization touches every audio page before the RT reader sees it.
        let slots: Box<[Slot]> = (0..CAPACITY)
            .map(|_| Slot {
                sequence: AtomicU64::new(0),
                timestamp_hns: AtomicU64::new(0),
                samples: std::array::from_fn(|_| AtomicU32::new(0)),
            })
            .collect();
        let locked = unsafe {
            VirtualLock(slots.as_ptr().cast(), std::mem::size_of_val(slots.as_ref())).is_ok()
        };
        Self {
            state: State {
                slots,
                status: AtomicU32::new(IDLE),
                error: AtomicU32::new(0),
                epoch: AtomicU64::new(0),
                latest: AtomicU64::new(0),
                packet_tick: AtomicU64::new(0),
                capture_tick: AtomicU64::new(0),
                worker_tick: AtomicU64::new(0),
            },
            locked,
        }
    }
    fn state(&self) -> &State {
        &self.state
    }
    pub fn now() -> u64 {
        unsafe { GetTickCount64() }
    }

    pub fn request_capture(&self) {
        self.state()
            .capture_tick
            .store(Self::now(), Ordering::Release);
    }

    pub fn capture_requested(&self) -> bool {
        let last = self.state().capture_tick.load(Ordering::Acquire);
        last != 0 && Self::now().saturating_sub(last) < 1_000
    }

    pub fn heartbeat(&self) {
        self.state()
            .worker_tick
            .store(Self::now(), Ordering::Release);
    }

    pub fn status(&self) -> (bool, u32, u32, bool) {
        let state = self.state();
        let heartbeat = state.worker_tick.load(Ordering::Acquire);
        let alive = heartbeat != 0 && Self::now().saturating_sub(heartbeat) < 3_000;
        (
            alive,
            state.status.load(Ordering::Acquire),
            state.error.load(Ordering::Acquire),
            self.latest().is_some(),
        )
    }

    /// Single-producer operation, serialized by the APO reference worker.
    pub fn restart(&self, status: u32, error: u32) {
        let state = self.state();
        state.latest.store(0, Ordering::SeqCst);
        state.packet_tick.store(0, Ordering::SeqCst);
        state.epoch.fetch_add(1, Ordering::SeqCst);
        state.error.store(error, Ordering::Release);
        state.status.store(status, Ordering::Release);
    }

    pub fn publish(&self, sequence: u64, timestamp_hns: u64, samples: &[[f32; BLOCK]; CHANNELS]) {
        let state = self.state();
        let slot = &state.slots[sequence as usize % CAPACITY];
        slot.sequence.store(0, Ordering::SeqCst);
        slot.timestamp_hns.store(timestamp_hns, Ordering::SeqCst);
        for (channel, input) in samples.iter().enumerate() {
            for (index, &sample) in input.iter().enumerate() {
                let sample = if sample.is_finite() {
                    sample.clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                slot.samples[channel * BLOCK + index].store(sample.to_bits(), Ordering::SeqCst);
            }
        }
        slot.sequence.store(sequence, Ordering::SeqCst);
        state.latest.store(sequence, Ordering::Release);
        state.packet_tick.store(Self::now(), Ordering::Release);
    }

    pub fn latest(&self) -> Option<(u64, u64)> {
        let state = self.state();
        let epoch = state.epoch.load(Ordering::Acquire);
        let latest = state.latest.load(Ordering::Acquire);
        let tick = state.packet_tick.load(Ordering::Acquire);
        if latest == 0 || tick == 0 || Self::now().saturating_sub(tick) > MAX_AGE_MS {
            return None;
        }
        Some((epoch, latest))
    }

    pub fn read(
        &self,
        epoch: u64,
        sequence: u64,
        output: &mut [[f32; BLOCK]; CHANNELS],
    ) -> Option<u64> {
        let state = self.state();
        if state.epoch.load(Ordering::SeqCst) != epoch {
            return None;
        }
        let slot = &state.slots[sequence as usize % CAPACITY];
        if slot.sequence.load(Ordering::SeqCst) != sequence {
            return None;
        }
        let timestamp = slot.timestamp_hns.load(Ordering::SeqCst);
        for (channel, dest) in output.iter_mut().enumerate() {
            for (index, sample) in dest.iter_mut().enumerate() {
                let value =
                    f32::from_bits(slot.samples[channel * BLOCK + index].load(Ordering::SeqCst));
                *sample = if value.is_finite() {
                    value.clamp(-1.0, 1.0)
                } else {
                    0.0
                };
            }
        }
        if slot.sequence.load(Ordering::SeqCst) != sequence
            || state.epoch.load(Ordering::SeqCst) != epoch
        {
            return None;
        }
        Some(timestamp)
    }
}

impl Drop for Reference {
    fn drop(&mut self) {
        if self.locked {
            unsafe {
                let _ = VirtualUnlock(
                    self.state.slots.as_ptr().cast(),
                    std::mem::size_of_val(self.state.slots.as_ref()),
                );
            }
        }
    }
}

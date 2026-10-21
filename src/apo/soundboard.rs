//! Background IPC reader plus resident, bounded-work post-denoise mixing.
use super::{
    etw::{self, apo_log},
    worker,
};
use crate::shared::{
    audio::SampleFormat,
    soundboard::{Clock, Mapping, RATE, Slot},
};
use std::{
    f64::consts::PI,
    mem::size_of,
    path::PathBuf,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{
            AtomicBool, AtomicU64,
            Ordering::{Acquire, Release},
        },
    },
    time::Duration,
};
use windows::{
    Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
        VirtualLock, VirtualUnlock,
    },
    core::w,
};

const FRAMES: usize = 4096;
const TAPS: usize = 32;
const PHASES: usize = 256;

#[repr(C)]
struct Storage {
    frames: [Slot; FRAMES],
    kernels: [[f32; TAPS]; PHASES],
    generation: AtomicU64,
    deadline: AtomicU64,
    gain: AtomicU64,
    mute_microphone: AtomicBool,
}
struct Resident(NonNull<Storage>);
// The allocation is owned through Arc. Samples and metadata are atomic; filter
// coefficients are immutable after construction, and freeing needs exclusive Drop.
unsafe impl Send for Resident {}
unsafe impl Sync for Resident {}
impl std::ops::Deref for Resident {
    type Target = Storage;
    fn deref(&self) -> &Storage {
        unsafe { self.0.as_ref() }
    }
}
impl Resident {
    fn new(rate: f64) -> anyhow::Result<Arc<Self>> {
        // A dedicated page allocation prevents unlocking a shared heap page that
        // another APO instance still relies on. All fields have valid zero bits.
        let pointer = unsafe {
            VirtualAlloc(
                None,
                size_of::<Storage>(),
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        let pointer = NonNull::new(pointer.cast::<Storage>())
            .ok_or_else(windows::core::Error::from_thread)?;
        unsafe {
            (*pointer.as_ptr()).kernels.copy_from_slice(&kernels(rate));
            if let Err(error) = VirtualLock(pointer.as_ptr().cast(), size_of::<Storage>()) {
                let _ = VirtualFree(pointer.as_ptr().cast(), 0, MEM_RELEASE);
                return Err(error.into());
            }
        }
        Ok(Arc::new(Self(pointer)))
    }
    fn sample(&self, position: f64, generation: u64, direct: bool) -> [f32; 2] {
        let base = position.floor() as u64;
        if direct {
            return self.frames[base as usize % FRAMES].read(base, generation);
        }
        let phase = ((position.fract() * PHASES as f64) as usize).min(PHASES - 1);
        let mut output = [0.0; 2];
        for (i, &coefficient) in self.kernels[phase].iter().enumerate() {
            let frame = base
                .saturating_add(i as u64)
                .saturating_sub((TAPS / 2 - 1) as u64);
            let stereo = self.frames[frame as usize % FRAMES].read(frame, generation);
            output[0] += stereo[0] * coefficient;
            output[1] += stereo[1] * coefficient;
        }
        output
    }
}
impl Drop for Resident {
    fn drop(&mut self) {
        unsafe {
            let _ = VirtualUnlock(self.0.as_ptr().cast(), size_of::<Storage>());
            let _ = VirtualFree(self.0.as_ptr().cast(), 0, MEM_RELEASE);
        }
    }
}

fn kernels(rate: f64) -> Box<[[f32; TAPS]]> {
    let cutoff = (rate / f64::from(RATE)).min(1.0) * 0.90;
    (0..PHASES)
        .map(|phase| {
            let mut row = [0.0; TAPS];
            let mut sum = 0.0;
            for (i, value) in row.iter_mut().enumerate() {
                let x = i as f64 - (TAPS / 2 - 1) as f64 - phase as f64 / PHASES as f64;
                let sinc = if x.abs() < 1e-10 {
                    cutoff
                } else {
                    (PI * cutoff * x).sin() / (PI * x)
                };
                let window = 0.42 - 0.5 * (2.0 * PI * i as f64 / (TAPS - 1) as f64).cos()
                    + 0.08 * (4.0 * PI * i as f64 / (TAPS - 1) as f64).cos();
                *value = (sinc * window) as f32;
                sum += *value;
            }
            for value in &mut row {
                *value /= sum;
            }
            row
        })
        .collect()
}

pub(super) struct Injection {
    state: Arc<Resident>,
    clock: Clock,
    position: f64,
    step: f64,
    channels: usize,
    format: SampleFormat,
    limiter_gain: f32,
    release: f32,
    stop: Arc<AtomicBool>,
}
impl Injection {
    pub fn start(
        path: PathBuf,
        rate: f64,
        channels: usize,
        format: SampleFormat,
    ) -> anyhow::Result<Self> {
        let clock = Clock::new()?;
        let state = Resident::new(rate)?;
        let background = Arc::clone(&state);
        let stop = Arc::new(AtomicBool::new(false));
        let background_stop = Arc::clone(&stop);
        worker::spawn(w!("VoiceBoxSoundboard"), move || {
            receive(path, &background, &background_stop, clock)
        })?;
        Ok(Self {
            state,
            clock,
            position: 0.0,
            step: f64::from(RATE) / rate,
            channels,
            format,
            limiter_gain: 1.0,
            release: (1.0 - (-1.0 / (rate * 0.05)).exp()) as f32,
            stop,
        })
    }
    pub fn cancellation(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }
    pub fn mix(&mut self, buffer: &mut [u8]) {
        let now = self.clock.frame();
        let generation = self.state.generation.load(Acquire);
        if self.position == 0.0 || (self.position - now as f64).abs() > f64::from(RATE) * 0.02 {
            self.position = now as f64;
        }
        let live = generation != 0 && now < self.state.deadline.load(Acquire);
        let mute_microphone = live && self.state.mute_microphone.load(Acquire);
        let gain = f32::from_bits(self.state.gain.load(Acquire) as u32);
        let bytes = self.format.bytes();
        let mut values = [0.0_f32; super::dsp::MAX_CHANNELS];
        for frame in buffer.chunks_exact_mut(bytes * self.channels) {
            let stereo = if live {
                self.state
                    .sample(self.position, generation, self.step == 1.0)
            } else {
                [0.0; 2]
            };
            self.position += self.step;
            let music = [stereo[0] * gain, stereo[1] * gain];
            // Preserve native bypass sample bytes exactly whenever the music and
            // limiter tail are both absent. No extra delay is added to the mic.
            if !mute_microphone && music == [0.0; 2] && self.limiter_gain == 1.0 {
                continue;
            }
            for (i, sample) in frame.chunks_exact(bytes).enumerate() {
                let song = if self.channels == 1 {
                    (music[0] + music[1]) * 0.5
                } else if i < 2 {
                    music[i]
                } else {
                    0.0
                };
                let microphone = if mute_microphone {
                    0.0
                } else {
                    self.format.decode(sample) as f32
                };
                values[i] = microphone + song;
            }
            let peak = values[..self.channels]
                .iter()
                .fold(0.0_f32, |p, x| p.max(x.abs()));
            let safe = if peak > 0.891_250_9 {
                0.891_250_9 / peak
            } else {
                1.0
            };
            // Linked-channel peak limiter: immediate attack, 50 ms release. The
            // existing speech lookahead limiter remains intact before this mixer.
            self.limiter_gain =
                (self.limiter_gain + (1.0 - self.limiter_gain) * self.release).min(safe);
            if safe == 1.0 && (1.0 - self.limiter_gain) < 1e-5 {
                self.limiter_gain = 1.0;
            }
            for (sample, &value) in frame.chunks_exact_mut(bytes).zip(&values[..self.channels]) {
                self.format
                    .encode(sample, f64::from(value * self.limiter_gain));
            }
        }
    }
}
impl Drop for Injection {
    fn drop(&mut self) {
        self.stop.store(true, Release);
    }
}

fn receive(path: PathBuf, state: &Resident, stop: &AtomicBool, clock: Clock) {
    let mut mapping = None;
    let mut retry_at = 0;
    let mut cursor = 0;
    let mut previous = 0;
    while !stop.load(Acquire) {
        let now = clock.frame();
        if mapping.is_none() && now >= retry_at {
            mapping = Mapping::open(&path, false).ok();
            retry_at = now + u64::from(RATE) / 2;
        }
        if let Some(mapping) = &mapping {
            let (generation, end, gain, mute_microphone) = mapping.status(now);
            if generation != previous || cursor < now.saturating_sub(480) {
                state.generation.store(0, Release);
                cursor = now.saturating_sub(480);
            }
            if generation != 0 && gain > 0.0 {
                // Bound work even for malformed shared headers. Each reader owns
                // its own cursor; recording clients never consume one another's PCM.
                let end = end.min(now + 2880);
                while cursor < end {
                    let stereo = mapping.read(cursor, generation);
                    state.frames[cursor as usize % FRAMES].write(cursor, generation, stereo);
                    cursor += 1;
                }
            }
            state.gain.store(u64::from(gain.to_bits()), Release);
            state.mute_microphone.store(mute_microphone, Release);
            state.deadline.store(
                if generation == 0 {
                    0
                } else {
                    end.min(now + u64::from(RATE) / 2)
                },
                Release,
            );
            state.generation.store(generation, Release);
            previous = generation;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    apo_log!(etw::INFO, etw::LIFECYCLE, "Soundboard receiver exited");
}

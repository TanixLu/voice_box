//! Background IPC reader plus resident, bounded-work post-denoise mixing.

use std::{
    f64::consts::PI,
    mem::size_of,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{
            AtomicBool, AtomicU16, AtomicU64,
            Ordering::{Acquire, Relaxed, Release},
        },
    },
    time::Duration,
};

use windows::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
};

use voice_box_shared::{
    apo_cfg::ApoCfg,
    pcm_transport::{
        PCM_CHANNELS, PCM_MAX_CURSOR_DRIFT_FRAMES, PCM_SAMPLE_RATE, PcmFrame, PcmReader, PcmSlot,
        pcm_frame, pcm_frame_with_frequency,
    },
    utils::{MAX_CHANNELS, PeakLimiter, RateMatcher, SampleFormat, win_qpf},
};

use crate::{
    etw::{self, apo_log},
    memory::LockedPages,
    worker,
};

const FRAMES: usize = 4096;
const TAPS: usize = 32;
const PHASES: usize = 256;

#[repr(C)]
struct Storage {
    frames: [PcmSlot; FRAMES],
    kernels: [[f32; TAPS]; PHASES],
    generation: AtomicU16,
    deadline: AtomicU64,
}
struct Resident {
    pointer: NonNull<Storage>,
    pages: Option<LockedPages>,
}
// The allocation is owned through Arc. Samples and metadata are atomic; filter
// coefficients are immutable after construction, and freeing needs exclusive Drop.
unsafe impl Send for Resident {}
unsafe impl Sync for Resident {}
impl std::ops::Deref for Resident {
    type Target = Storage;
    fn deref(&self) -> &Storage {
        unsafe { self.pointer.as_ref() }
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
        }
        let mut resident = Arc::new(Self {
            pointer,
            pages: None,
        });
        // Pin the Arc's pointer carrier as well as the sample storage. Account
        // for both in the same quota and page-reference registry as the DSP.
        let pages = LockedPages::new([
            (pointer.as_ptr() as usize, size_of::<Storage>()),
            (Arc::as_ptr(&resident) as usize, size_of::<Self>()),
        ])?;
        Arc::get_mut(&mut resident)
            .expect("UNREACHABLE: newly created soundboard state should be uniquely owned")
            .pages = Some(pages);
        Ok(resident)
    }
    fn sample(&self, position: f64, generation: u16, direct: bool) -> PcmFrame {
        let base = position.floor() as u64;
        if direct {
            return self.frames[base as usize % FRAMES].read(base, generation);
        }
        let phase = ((position.fract() * PHASES as f64) as usize).min(PHASES - 1);
        let mut output = PcmFrame::default();
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
        self.pages.take();
        unsafe {
            let _ = VirtualFree(self.pointer.as_ptr().cast(), 0, MEM_RELEASE);
        }
    }
}

fn kernels(rate: f64) -> Box<[[f32; TAPS]]> {
    let cutoff = (rate / f64::from(PCM_SAMPLE_RATE)).min(1.0) * 0.90;
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

pub struct Injection {
    state: Arc<Resident>,
    clock_frequency: i64,
    position: f64,
    generation: u16,
    matcher: crate::memory::Resident<RateMatcher>,
    step: f64,
    channels: usize,
    format: SampleFormat,
    limiter: PeakLimiter,
    stop: Arc<AtomicBool>,
}
impl Injection {
    pub fn start(
        name: String,
        rate: f64,
        channels: usize,
        format: SampleFormat,
        max_frames: usize,
    ) -> anyhow::Result<Self> {
        let clock_frequency = win_qpf();
        let state = Resident::new(rate)?;
        let matcher = crate::memory::Resident::new(|| RateMatcher::new(rate, max_frames), true)?;
        let background = Arc::clone(&state);
        let stop = Arc::new(AtomicBool::new(false));
        let background_stop = Arc::clone(&stop);
        worker::spawn("VoiceBoxSoundboard", move || {
            receive(name, &background, &background_stop)
        })?;
        Ok(Self {
            state,
            clock_frequency,
            position: 0.0,
            generation: 0,
            matcher,
            step: f64::from(PCM_SAMPLE_RATE) / rate,
            channels,
            format,
            limiter: PeakLimiter::new(rate),
            stop,
        })
    }
    pub fn cancellation(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }
    pub fn reset(&mut self) {
        self.generation = 0;
    }
    pub fn mix(&mut self, buffer: &mut [u8], config: &ApoCfg) {
        let bytes = self.format.bytes();
        let frame_bytes = bytes * self.channels;
        let frames = buffer.len() / frame_bytes;
        if frames == 0 {
            return;
        }
        let now = pcm_frame_with_frequency(self.clock_frequency);
        let generation = self.state.generation.load(Acquire);
        let deadline = self.state.deadline.load(Acquire) as f64;
        // Let a lagging source finish its tail after its scheduled QPC end.
        let injection_active = generation != 0
            && ((now as f64) < deadline
                || (generation == self.generation && self.position < deadline));
        if injection_active
            && (generation != self.generation
                || (self.position - now as f64).abs() > PCM_MAX_CURSOR_DRIFT_FRAMES as f64)
        {
            self.position = now as f64;
            self.matcher.reset();
        }
        self.generation = if injection_active { generation } else { 0 };
        let sound = if injection_active {
            let error = (self.position - now as f64) / f64::from(PCM_SAMPLE_RATE);
            Some(self.matcher.process(frames, error, || {
                let stereo = self
                    .state
                    .sample(self.position, generation, self.step == 1.0);
                self.position += self.step;
                stereo
            }))
        } else {
            None
        };
        let mute_microphone = injection_active && !config.playback_include_microphone_audio;
        let mut values = [0.0_f32; MAX_CHANNELS];
        for (index, frame) in buffer.chunks_exact_mut(frame_bytes).enumerate() {
            let sound_samples = sound.map_or(PcmFrame::default(), |sound| sound[index]);
            for (i, sample) in frame.chunks_exact(bytes).enumerate() {
                let sound_sample = if self.channels == 1 {
                    (sound_samples[0] + sound_samples[1]) / PCM_CHANNELS as f32
                } else if i < PCM_CHANNELS {
                    sound_samples[i]
                } else {
                    0.0
                };
                let microphone = if mute_microphone {
                    0.0
                } else {
                    self.format.decode(sample) as f32
                };
                values[i] = microphone + sound_sample;
            }
            // Always limit the mixed output, including microphone-only frames.
            // The speech lookahead limiter remains in the enhancement chain before this.
            self.limiter.process_frame(&mut values[..self.channels]);
            for (sample, &value) in frame.chunks_exact_mut(bytes).zip(&values[..self.channels]) {
                self.format.encode(sample, f64::from(value));
            }
        }
    }
}
impl Drop for Injection {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

fn receive(name: String, state: &Resident, stop: &AtomicBool) {
    let mut mapping = None;
    let mut retry_at = 0;
    let mut cursor = 0;
    let mut previous = 0;
    while !stop.load(Relaxed) {
        let now = pcm_frame();
        if mapping.is_none() && now >= retry_at {
            mapping = PcmReader::open(&name).ok();
            // Discover a new transport before playback's 100 ms startup lead ends.
            retry_at = now + u64::from(PCM_SAMPLE_RATE) / 100;
        }
        if let Some(mapping) = &mapping {
            let (generation, end) = mapping.status(now);
            if generation != previous || cursor < now.saturating_sub(480) {
                state.generation.store(0, Relaxed);
                cursor = now.saturating_sub(480);
            }
            if generation != 0 {
                // Bound work even for malformed shared headers. Each reader owns
                // its own cursor; recording clients never consume one another's PCM.
                let end = end.min(now + 2880);
                while cursor < end {
                    let stereo = mapping.read(cursor, generation);
                    state.frames[cursor as usize % FRAMES].write(cursor, generation, stereo);
                    cursor += 1;
                }
            }
            // Deadline and generation publish the copied frames.
            state.deadline.store(
                if generation == 0 {
                    0
                } else {
                    end.min(now + u64::from(PCM_SAMPLE_RATE) / 2)
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

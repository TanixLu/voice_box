//! Background IPC reader and resident host-rate playback for the shared DSP mixer.

use std::{
    sync::{
        Arc,
        atomic::{
            AtomicBool, AtomicU16, AtomicU64,
            Ordering::{Acquire, Relaxed, Release},
        },
    },
    time::Duration,
};

use voice_box_shared::{
    dsp::{RandomAccessResampler, RateMatcher},
    pcm_transport::{
        PCM_MAX_CURSOR_DRIFT_FRAMES, PCM_SAMPLE_RATE, PcmFrame, PcmReader, PcmSlot, pcm_frame,
        pcm_frame_f64,
    },
};

use crate::{
    etw::{self, apo_log},
    memory::{LockedAllocation, Resident},
    worker,
};

const FRAMES: usize = 4096; // ~85.3 ms at 48 kHz
const HISTORY_FRAMES: u64 = PCM_SAMPLE_RATE as u64 / 100; // 10 ms
const COPY_AHEAD_FRAMES: u64 = PCM_SAMPLE_RATE as u64 * 60 / 1000; // 60 ms

struct SoundboardState {
    frames: [PcmSlot; FRAMES],
    generation: AtomicU16,
    deadline: AtomicU64,
}
impl SoundboardState {
    fn read_frame(&self, frame: u64, generation: u16) -> PcmFrame {
        self.frames[frame as usize % FRAMES].read(frame, generation)
    }
}
pub struct Injection {
    state: Arc<LockedAllocation<SoundboardState>>,
    position: f64,
    generation: u16,
    resampler: Resident<RandomAccessResampler>,
    matcher: Resident<RateMatcher>,
    stop: Arc<AtomicBool>,
}
impl Injection {
    pub fn start(name: String, rate: f64, max_frames: usize) -> anyhow::Result<Self> {
        // SoundboardState contains only atomics with valid all-zero values.
        let state = unsafe { LockedAllocation::<SoundboardState>::new_zeroed()? };
        let resampler =
            Resident::new(|| Ok(RandomAccessResampler::new(f64::from(PCM_SAMPLE_RATE), rate)))?;
        let matcher = Resident::new(|| Ok(RateMatcher::new(rate, max_frames)))?;
        let background = Arc::clone(&state);
        let stop = Arc::new(AtomicBool::new(false));
        let background_stop = Arc::clone(&stop);
        worker::spawn("VoiceBoxSoundboard", move || {
            receive(name, &background, &background_stop)
        })?;
        Ok(Self {
            state,
            position: 0.0,
            generation: 0,
            resampler,
            matcher,
            stop,
        })
    }
    pub fn cancellation(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }
    pub fn reset(&mut self) {
        self.generation = 0;
    }
    pub fn read(&mut self, frames: usize) -> Option<&[PcmFrame]> {
        if frames == 0 {
            return None;
        }
        let now = pcm_frame_f64();
        let generation = self.state.generation.load(Acquire);
        let deadline = self.state.deadline.load(Acquire) as f64;
        // Let a lagging source finish its tail after its scheduled QPC end.
        let injection_active = generation != 0
            && (now < deadline || (generation == self.generation && self.position < deadline));
        if injection_active
            && (generation != self.generation
                || (self.position - now).abs() > PCM_MAX_CURSOR_DRIFT_FRAMES as f64)
        {
            self.position = now;
            self.matcher.reset();
        }
        self.generation = if injection_active { generation } else { 0 };
        if injection_active {
            let error = (self.position - now) / f64::from(PCM_SAMPLE_RATE);
            let step = self.resampler.source_step();
            Some(self.matcher.process(frames, error, || {
                let stereo = self.resampler.sample(self.position, |frame| {
                    self.state.read_frame(frame, generation)
                });
                self.position += step;
                stereo
            }))
        } else {
            None
        }
    }
}
impl Drop for Injection {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

fn receive(name: String, state: &SoundboardState, stop: &AtomicBool) {
    let mut mapping = None;
    let mut cursor = 0;
    let mut previous = 0;
    while !stop.load(Relaxed) {
        let now = pcm_frame();
        if mapping.is_none() {
            mapping = PcmReader::open(&name).ok();
        }
        if let Some(mapping) = &mapping {
            let (generation, source_end) = mapping.status(now);
            let copy_end = source_end.min(now + COPY_AHEAD_FRAMES);
            if generation != previous || cursor < now.saturating_sub(HISTORY_FRAMES) {
                state.generation.store(0, Relaxed);
                cursor = now.saturating_sub(HISTORY_FRAMES);
            }
            if generation != 0 {
                // Bound work even for malformed shared headers. Each reader owns
                // its own cursor; recording clients never consume one another's PCM.
                while cursor < copy_end {
                    let stereo = mapping.read(cursor, generation);
                    state.frames[cursor as usize % FRAMES].write(cursor, generation, stereo);
                    cursor += 1;
                }
            }
            // Keep injection active through the source's end, even if local PCM runs out.
            state.deadline.store(source_end, Release);
            state.generation.store(generation, Release);
            previous = generation;
        }
        std::thread::sleep(Duration::from_millis(if mapping.is_some() {
            5
        } else {
            20
        }));
    }
    apo_log!(etw::INFO, etw::LIFECYCLE, "Soundboard receiver exited");
}

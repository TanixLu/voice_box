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
        pcm_frame_with_frequency,
    },
    utils::win_qpf,
};

use crate::{
    etw::{self, apo_log},
    memory::{LockedAllocation, Resident},
    worker,
};

const FRAMES: usize = 4096;

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
    clock_frequency: i64,
    position: f64,
    generation: u16,
    resampler: Resident<RandomAccessResampler>,
    matcher: Resident<RateMatcher>,
    stop: Arc<AtomicBool>,
}
impl Injection {
    pub fn start(name: String, rate: f64, max_frames: usize) -> anyhow::Result<Self> {
        let clock_frequency = win_qpf();
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
            clock_frequency,
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
        if injection_active {
            let error = (self.position - now as f64) / f64::from(PCM_SAMPLE_RATE);
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

//! The recording dialog's capture and preview jobs; preview ranges always refer to complete frames.

use std::{
    io::Write,
    ops::Range,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use anyhow::ensure;

use voice_box_shared::pcm_transport::{PCM_CHANNELS, PCM_SAMPLE_RATE, PcmFrame};

use crate::{
    audio::{Recording, loopback, playback},
    i18n::{Text, UserContext},
    model::{Sound, is_valid_sound_name},
    task::{Task, Wake},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
}

#[derive(Clone, Copy)]
pub struct Selection {
    pub start: usize,
    pub end: usize,
}
impl Selection {
    pub fn full(recording: &Recording) -> Self {
        Self {
            start: 0,
            end: recording.samples.len() / recording.channels as usize,
        }
    }
    pub fn set(&mut self, edge: Edge, frame: usize, total: usize) {
        if total == 0 {
            self.start = 0;
            self.end = 0;
            return;
        }
        match edge {
            Edge::Left => self.start = frame.min(self.end.saturating_sub(1)),
            Edge::Right => self.end = frame.clamp(self.start + 1, total),
        }
    }
    pub fn range(self) -> Range<usize> {
        self.start..self.end
    }
    pub fn preview(self, edge: Edge, rate: u32) -> Range<usize> {
        let start = match edge {
            Edge::Left => self.start,
            Edge::Right => self.start.max(self.end.saturating_sub(rate as usize)),
        };
        start..self.end
    }
}

pub struct Clip {
    pub audio: Arc<Recording>,
    pub peaks: Vec<f32>,
}
impl Clip {
    fn new(audio: Recording) -> Self {
        let frames = audio.samples.len() / audio.channels as usize;
        let bucket = frames.div_ceil(640).max(1) * audio.channels as usize;
        let peaks = audio
            .samples
            .chunks(bucket)
            .map(|chunk| {
                chunk
                    .iter()
                    .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
            })
            .collect();
        Self {
            audio: Arc::new(audio),
            peaks,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Recording,
    Preview,
}
enum Outcome {
    Recorded(Clip),
    Previewed,
}
/// The next job; it starts once the running one has released its device.
enum Pending {
    Record,
    Preview(Range<usize>, Instant),
}

pub struct Recorder {
    wake: Wake,
    task: Option<(Activity, Range<usize>, Task<Outcome>)>,
    pending: Option<Pending>,
    pub clip: Option<Clip>,
}
impl Recorder {
    pub fn new(wake: Wake) -> Self {
        Self {
            wake,
            task: None,
            pending: None,
            clip: None,
        }
    }
    pub fn activity(&self) -> Option<Activity> {
        match self.pending {
            Some(Pending::Record) => Some(Activity::Recording),
            _ => self.task.as_ref().map(|(activity, ..)| *activity),
        }
    }
    pub fn busy(&self) -> bool {
        self.task.is_some() || self.pending.is_some()
    }
    pub fn stopping(&self) -> bool {
        !matches!(self.pending, Some(Pending::Record))
            && self
                .task
                .as_ref()
                .is_some_and(|(.., task)| task.cancelling())
    }
    pub fn elapsed_secs(&self) -> Option<f64> {
        if matches!(self.pending, Some(Pending::Record)) {
            return None;
        }
        let started = self.task.as_ref()?.2.started_at()?;
        Some(started.elapsed().as_secs_f64())
    }
    pub fn preview_frame(&self) -> Option<usize> {
        let (Activity::Preview, range, _) = self.task.as_ref()? else {
            return None;
        };
        let rate = f64::from(self.clip.as_ref()?.audio.sample_rate);
        Some((range.start + (self.elapsed_secs()? * rate) as usize).min(range.end))
    }
    pub fn stop(&mut self) {
        self.pending = None;
        if let Some((.., task)) = &self.task {
            task.cancel();
        }
    }
    fn locked(&self) -> bool {
        self.activity() == Some(Activity::Recording)
    }
    pub fn record(&mut self) {
        if self.locked() {
            return;
        }
        self.stop();
        self.clip = None;
        if self.task.is_some() {
            // Release the preview stream before capturing, so it cannot enter the new recording.
            self.pending = Some(Pending::Record);
        } else {
            self.begin_recording();
        }
    }
    fn begin_recording(&mut self) {
        let task =
            Task::spawn_cancellable("VoiceBoxSpeakerRecording", &self.wake, |stop, started| {
                loopback::capture(stop, started)
                    .map(Clip::new)
                    .map(Outcome::Recorded)
            });
        self.task = Some((Activity::Recording, 0..0, task));
    }
    pub fn queue_preview(&mut self, range: Range<usize>, immediate: bool) {
        if self.locked() {
            return;
        }
        self.stop();
        let delay = Duration::from_millis(if immediate { 0 } else { 150 });
        self.pending = Some(Pending::Preview(range, Instant::now() + delay));
    }
    pub fn poll(&mut self) -> anyhow::Result<()> {
        if let Some((.., task)) = &mut self.task
            && let Some(result) = task.poll()
        {
            self.task = None;
            match result {
                Ok(Outcome::Recorded(clip)) => {
                    tracing::info!(
                        duration_secs = clip.audio.duration_secs(),
                        sample_rate = clip.audio.sample_rate,
                        channels = clip.audio.channels,
                        "Speaker recording completed"
                    );
                    self.clip = Some(clip);
                }
                Ok(Outcome::Previewed) => {}
                Err(error) => {
                    if matches!(self.pending, Some(Pending::Preview(..))) {
                        self.pending = None;
                    }
                    return Err(error);
                }
            }
        }
        if self.task.is_some() {
            return Ok(());
        }
        let audio = self.clip.as_ref().map(|clip| clip.audio.clone());
        match (self.pending.take(), audio) {
            (Some(Pending::Record), _) => self.begin_recording(),
            (Some(Pending::Preview(range, when)), Some(audio)) if Instant::now() >= when => {
                let channels = audio.channels as usize;
                let samples = range.start * channels..range.end * channels;
                let task = Task::spawn_cancellable(
                    "VoiceBoxRecordingPreview",
                    &self.wake,
                    move |stop, started| {
                        if !stop.load(Ordering::Relaxed) {
                            let source = playback::once(&audio.samples[samples]);
                            playback::play(&audio, source, stop, started)?;
                        }
                        Ok(Outcome::Previewed)
                    },
                );
                self.task = Some((Activity::Preview, range, task));
            }
            // Wait for the preview deadline; work without a clip can never start.
            (pending @ Some(Pending::Preview(..)), Some(_)) => self.pending = pending,
            _ => {}
        }
        Ok(())
    }
}

fn write_wave(
    output: &mut dyn Write,
    audio: &Recording,
    selection: Selection,
) -> anyhow::Result<()> {
    let frames = &audio.samples.as_chunks::<PCM_CHANNELS>().0[selection.range()];
    let bytes = u32::try_from(size_of_val(frames))?;
    let frame_bytes = size_of::<PcmFrame>() as u16;
    // IEEE float WAV with a fact chunk preserves recorded samples without quantization.
    output.write_all(b"RIFF")?;
    output.write_all(&(bytes + 50).to_le_bytes())?;
    output.write_all(b"WAVEfmt ")?;
    output.write_all(&18_u32.to_le_bytes())?;
    output.write_all(&3_u16.to_le_bytes())?;
    output.write_all(&audio.channels.to_le_bytes())?;
    output.write_all(&audio.sample_rate.to_le_bytes())?;
    output.write_all(&(audio.sample_rate * u32::from(frame_bytes)).to_le_bytes())?;
    output.write_all(&frame_bytes.to_le_bytes())?;
    output.write_all(&32_u16.to_le_bytes())?;
    output.write_all(&0_u16.to_le_bytes())?;
    output.write_all(b"fact")?;
    output.write_all(&4_u32.to_le_bytes())?;
    output.write_all(&u32::try_from(frames.len())?.to_le_bytes())?;
    output.write_all(b"data")?;
    output.write_all(&bytes.to_le_bytes())?;
    for sample in frames.as_flattened() {
        output.write_all(&sample.to_le_bytes())?;
    }
    Ok(())
}

/// Saves a recording: writes the selected frames to the database as a new sound.
pub fn save(audio: &Recording, selection: Selection, name: &str) -> anyhow::Result<Sound> {
    let name = name.trim();
    ensure!(is_valid_sound_name(name), Text::InvalidRecordingName);
    ensure!(
        audio.channels as usize == PCM_CHANNELS && audio.sample_rate == PCM_SAMPLE_RATE,
        Text::InvalidRecordingFormat
    );
    ensure!(
        selection.start < selection.end && selection.end <= audio.samples.len() / PCM_CHANNELS,
        Text::InvalidSelection
    );
    // The float WAV header is 58 bytes; encode directly into the buffered BLOB writer.
    let length = 58 + (selection.end - selection.start) as u64 * size_of::<PcmFrame>() as u64;
    let duration_secs = (selection.end - selection.start) as f64 / f64::from(audio.sample_rate);
    crate::database::sounds::insert(name, duration_secs, length, |output| {
        write_wave(output, audio, selection)
    })
    .user_context(Text::SaveRecordingFailed)
}

//! Recording/import tool jobs; preview ranges always refer to complete frames.
use super::library::{Sound, is_valid_file_name};
use crate::app::audio::{capture::Recording, loopback, playback::playback};
use anyhow::{Context, ensure};
use std::{
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    ops::Range,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum Edge {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug)]
pub(in crate::app) struct Selection {
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

pub(in crate::app) struct Clip {
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
pub(in crate::app) enum Activity {
    Recording,
    Preview,
    Saving,
}
enum Outcome {
    Recorded(Clip),
    Previewed,
    Saved(Sound),
}
enum Event {
    Started(Instant),
    Finished(anyhow::Result<Outcome>),
}
struct Job {
    activity: Activity,
    stop: Arc<AtomicBool>,
    receiver: mpsc::Receiver<Event>,
    started: Option<Instant>,
    range: Range<usize>,
}

#[derive(Default)]
pub(in crate::app) struct Recorder {
    job: Option<Job>,
    pending_preview: Option<(Range<usize>, Instant)>,
    pending_save: Option<(Selection, String)>,
    pending_record: bool,
    pub clip: Option<Clip>,
    pub error: String,
}
impl Recorder {
    pub fn activity(&self) -> Option<Activity> {
        if self.pending_record {
            Some(Activity::Recording)
        } else if self.pending_save.is_some() {
            Some(Activity::Saving)
        } else {
            self.job.as_ref().map(|job| job.activity)
        }
    }
    pub fn busy(&self) -> bool {
        self.job.is_some()
            || self.pending_preview.is_some()
            || self.pending_save.is_some()
            || self.pending_record
    }
    pub fn stopping(&self) -> bool {
        !self.pending_record
            && self
                .job
                .as_ref()
                .is_some_and(|job| job.stop.load(Ordering::Relaxed))
    }
    pub fn elapsed(&self) -> Option<f64> {
        if self.pending_record {
            return None;
        }
        self.job
            .as_ref()?
            .started
            .map(|start| start.elapsed().as_secs_f64())
    }
    pub fn preview_frame(&self) -> Option<usize> {
        let job = self.job.as_ref()?;
        if job.activity != Activity::Preview {
            return None;
        }
        Some(
            (job.range.start
                + (self.elapsed()? * f64::from(self.clip.as_ref()?.audio.sample_rate)) as usize)
                .min(job.range.end),
        )
    }
    pub fn stop(&mut self) {
        self.pending_preview = None;
        self.pending_save = None;
        self.pending_record = false;
        if let Some(job) = &self.job {
            job.stop.store(true, Ordering::Relaxed);
        }
    }
    fn begin(
        &mut self,
        activity: Activity,
        range: Range<usize>,
        wake: impl Fn() + Send + 'static,
        operation: impl FnOnce(&AtomicBool, &dyn Fn()) -> anyhow::Result<Outcome> + Send + 'static,
    ) {
        if self.job.is_some() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (sender, receiver) = mpsc::channel();
        self.error.clear();
        match std::thread::Builder::new()
            .name("VoiceBoxRecordingImport".into())
            .spawn(move || {
                let started = || {
                    if activity == Activity::Recording {
                        tracing::info!("Speaker recording started");
                    }
                    let _ = sender.send(Event::Started(Instant::now()));
                    wake();
                };
                let result = operation(&worker_stop, &started);
                if let Err(error) = &result {
                    tracing::error!(?activity, "Recording import failed: {error:#}");
                }
                let _ = sender.send(Event::Finished(result));
                wake();
            }) {
            Ok(_) => {
                self.job = Some(Job {
                    activity,
                    stop,
                    receiver,
                    started: None,
                    range,
                })
            }
            Err(error) => {
                tracing::error!(
                    ?activity,
                    "Failed to spawn recording import operation: {error}"
                );
                self.error = format!("无法开始操作：{error}");
            }
        }
    }
    pub fn record(&mut self, wake: impl Fn() + Send + 'static) {
        if matches!(
            self.activity(),
            Some(Activity::Recording | Activity::Saving)
        ) {
            return;
        }
        self.stop();
        self.clip = None;
        if self.job.is_some() {
            // Release the preview stream before capturing, so it cannot enter the new recording.
            self.pending_record = true;
        } else {
            self.begin_recording(wake);
        }
    }
    fn begin_recording(&mut self, wake: impl Fn() + Send + 'static) {
        self.begin(Activity::Recording, 0..0, wake, |stop, started| {
            loopback::capture(stop, started)
                .map(Clip::new)
                .map(Outcome::Recorded)
        });
    }
    pub fn queue_preview(&mut self, range: Range<usize>, immediate: bool) {
        if matches!(
            self.activity(),
            Some(Activity::Recording | Activity::Saving)
        ) {
            return;
        }
        self.stop();
        self.pending_preview = Some((
            range,
            Instant::now()
                + if immediate {
                    Duration::ZERO
                } else {
                    Duration::from_millis(150)
                },
        ));
    }
    pub fn save(&mut self, selection: Selection, name: String) {
        if self.clip.is_none()
            || matches!(
                self.activity(),
                Some(Activity::Recording | Activity::Saving)
            )
        {
            return;
        }
        self.stop();
        self.pending_save = Some((selection, name));
    }
    fn begin_save(&mut self, selection: Selection, name: String, wake: impl Fn() + Send + 'static) {
        let Some(clip) = &self.clip else {
            return;
        };
        let audio = clip.audio.clone();
        self.begin(Activity::Saving, selection.range(), wake, move |_, _| {
            save_clip(
                &crate::shared::paths::sound_library_dir()?,
                &audio,
                selection,
                &name,
            )
            .map(Outcome::Saved)
        });
    }
    // Returns a newly written file. The caller commits it to the configuration.
    pub fn poll(&mut self, wake: impl Fn() + Send + 'static) -> Option<Sound> {
        while let Some(job) = &mut self.job {
            match job.receiver.try_recv() {
                Ok(Event::Started(started)) => job.started = Some(started),
                Ok(Event::Finished(result)) => {
                    self.job = None;
                    match result {
                        Ok(Outcome::Recorded(clip)) => {
                            tracing::info!(
                                duration_secs = clip.audio.duration(),
                                sample_rate = clip.audio.sample_rate,
                                channels = clip.audio.channels,
                                "Speaker recording completed"
                            );
                            self.clip = Some(clip);
                        }
                        Ok(Outcome::Previewed) => {}
                        Ok(Outcome::Saved(sound)) => return Some(sound),
                        Err(error) => {
                            self.error = format!("操作失败：{error:#}");
                            self.pending_preview = None;
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    tracing::error!(
                        activity = ?job.activity,
                        "Recording import operation exited without a result"
                    );
                    self.job = None;
                    self.pending_preview = None;
                    self.error = "录音工具意外退出，请重试".into();
                }
            }
        }
        if self.job.is_none() && self.pending_record {
            self.pending_record = false;
            self.begin_recording(wake);
        } else if self.job.is_none()
            && let Some((selection, name)) = self.pending_save.take()
        {
            self.begin_save(selection, name, wake);
        } else if self.job.is_none()
            && self
                .pending_preview
                .as_ref()
                .is_some_and(|(_, when)| Instant::now() >= *when)
        {
            let (range, _) = self.pending_preview.take().unwrap();
            if let Some(clip) = &self.clip {
                let audio = clip.audio.clone();
                let samples =
                    range.start * audio.channels as usize..range.end * audio.channels as usize;
                self.begin(Activity::Preview, range, wake, move |stop, started| {
                    if !stop.load(Ordering::Relaxed) {
                        playback(&audio, &audio.samples[samples], stop, started)?;
                    }
                    Ok(Outcome::Previewed)
                });
            }
        }
        None
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop();
    }
}

fn write_wave(
    output: &mut impl Write,
    audio: &Recording,
    selection: Selection,
) -> anyhow::Result<()> {
    let total = audio.samples.len() / audio.channels as usize;
    ensure!(
        selection.start < selection.end && selection.end <= total,
        "截取范围无效"
    );
    ensure!(
        audio.channels == 2 && audio.sample_rate == 48_000,
        "录音格式无效"
    );
    let samples = &audio.samples[selection.start * 2..selection.end * 2];
    let bytes = u32::try_from(samples.len() * 4)?;
    // IEEE float WAV with a fact chunk preserves recorded samples without quantization.
    output.write_all(b"RIFF")?;
    output.write_all(&(bytes + 50).to_le_bytes())?;
    output.write_all(b"WAVEfmt ")?;
    output.write_all(&18_u32.to_le_bytes())?;
    output.write_all(&3_u16.to_le_bytes())?;
    output.write_all(&audio.channels.to_le_bytes())?;
    output.write_all(&audio.sample_rate.to_le_bytes())?;
    output.write_all(&(audio.sample_rate * 8).to_le_bytes())?;
    output.write_all(&8_u16.to_le_bytes())?;
    output.write_all(&32_u16.to_le_bytes())?;
    output.write_all(&0_u16.to_le_bytes())?;
    output.write_all(b"fact")?;
    output.write_all(&4_u32.to_le_bytes())?;
    output.write_all(&u32::try_from(selection.end - selection.start)?.to_le_bytes())?;
    output.write_all(b"data")?;
    output.write_all(&bytes.to_le_bytes())?;
    for sample in samples {
        output.write_all(&sample.to_le_bytes())?;
    }
    Ok(())
}

fn save_clip(
    directory: &Path,
    audio: &Recording,
    selection: Selection,
    name: &str,
) -> anyhow::Result<Sound> {
    let name = name.trim();
    ensure!(
        is_valid_file_name(name),
        "名称不能为空，也不能包含路径或 Windows 文件名保留字符"
    );
    fs::create_dir_all(directory).context("无法创建音效库目录")?;
    for number in 1_u64.. {
        let file_name = if number == 1 {
            format!("{name}.wav")
        } else {
            format!("{name} ({number}).wav")
        };
        let path = directory.join(&file_name);
        let file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("无法创建录音文件"),
        };
        let mut output = BufWriter::new(file);
        let result = write_wave(&mut output, audio, selection).and_then(|()| {
            output.flush()?;
            output.get_ref().sync_all()?;
            Ok(())
        });
        drop(output);
        if let Err(error) = result {
            let _ = fs::remove_file(&path);
            return Err(error).context("保存录音失败");
        }
        return Ok(Sound {
            file_name,
            path,
            duration: Some((selection.end - selection.start) as f64 / f64::from(audio.sample_rate)),
            ..Default::default()
        });
    }
    unreachable!()
}

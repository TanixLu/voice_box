//! Cancellable background recording, DLL preview and playback.
use super::{
    capture::{Recording, capture},
    playback::playback,
};
use crate::shared::settings::Settings;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Instant,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum Activity {
    Record,
    Original,
    Processed,
}

struct Processed {
    settings: Settings,
    samples: Arc<Vec<f32>>,
}

enum Outcome {
    Recorded(Arc<Recording>),
    Played(Option<Processed>),
}

enum Event {
    Started,
    Finished(anyhow::Result<Outcome>),
}

struct Job {
    activity: Activity,
    stop: Arc<AtomicBool>,
    receiver: mpsc::Receiver<Event>,
    started: Option<Instant>,
}

#[derive(Default)]
pub(in crate::app) struct AudioSession {
    job: Option<Job>,
    recording: Option<Arc<Recording>>,
    processed: Option<Processed>,
    pub message: String,
    pub failed: bool,
}

impl AudioSession {
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }
    pub fn activity(&self) -> Option<Activity> {
        self.job.as_ref().map(|job| job.activity)
    }
    pub fn has_recording(&self) -> bool {
        self.recording.is_some()
    }
    pub fn stopping(&self) -> bool {
        self.job
            .as_ref()
            .is_some_and(|job| job.stop.load(Ordering::Relaxed))
    }
    pub fn elapsed(&self) -> Option<f64> {
        self.job
            .as_ref()
            .filter(|job| job.activity == Activity::Record)
            .and_then(|job| job.started)
            .map(|started| started.elapsed().as_secs_f64())
    }
    pub fn playback_progress(&self) -> Option<(f64, f64)> {
        let job = self.job.as_ref()?;
        if job.activity == Activity::Record {
            return None;
        }
        let started = job.started?;
        let duration = self.recording.as_ref()?.duration();
        Some((started.elapsed().as_secs_f64().min(duration), duration))
    }
    pub fn stop(&self) {
        if let Some(job) = &self.job {
            job.stop.store(true, Ordering::Relaxed);
        }
    }

    fn begin(
        &mut self,
        wake: impl Fn() + Send + 'static,
        activity: Activity,
        message: &str,
        operation: impl FnOnce(&AtomicBool, &dyn Fn()) -> anyhow::Result<Outcome> + Send + 'static,
    ) {
        if self.busy() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (sender, receiver) = mpsc::channel();
        self.message = message.into();
        self.failed = false;
        match std::thread::Builder::new()
            .name("VoiceBoxAudioSession".into())
            .spawn(move || {
                let started = || {
                    let _ = sender.send(Event::Started);
                    wake();
                };
                let result = operation(&worker_stop, &started);
                if let Err(error) = &result {
                    tracing::error!("Audio session failed: {error:#}");
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
                })
            }
            Err(error) => {
                self.message = format!("无法开始试听操作：{error}");
                self.failed = true;
            }
        }
    }

    pub fn record(&mut self, device_id: String, wake: impl Fn() + Send + 'static) {
        self.begin(
            wake,
            Activity::Record,
            "正在打开麦克风…",
            move |stop, started| {
                capture(&device_id, stop, started)
                    .map(|recording| Outcome::Recorded(Arc::new(recording)))
            },
        );
    }

    pub fn play(&mut self, settings: Option<Settings>, wake: impl Fn() + Send + 'static) {
        let Some(recording) = self.recording.clone() else {
            return;
        };
        let cached = settings.as_ref().and_then(|settings| {
            self.processed
                .as_ref()
                .filter(|processed| &processed.settings == settings)
                .map(|processed| Arc::clone(&processed.samples))
        });
        let activity = if settings.is_some() {
            Activity::Processed
        } else {
            Activity::Original
        };
        self.begin(
            wake,
            activity,
            if settings.is_some() {
                "正在准备处理后的声音…"
            } else {
                "正在打开播放设备…"
            },
            move |stop, started| {
                let processed = if let Some(settings) = settings {
                    let samples = match cached {
                        Some(samples) => samples,
                        None => Arc::new(super::preview::process(&recording, &settings)?),
                    };
                    Some(Processed { settings, samples })
                } else {
                    None
                };
                if !stop.load(Ordering::Relaxed) {
                    let samples = processed
                        .as_ref()
                        .map_or(recording.samples.as_slice(), |processed| {
                            processed.samples.as_slice()
                        });
                    playback(&recording, samples, stop, started)?;
                }
                Ok(Outcome::Played(processed))
            },
        );
    }

    pub fn poll(&mut self) {
        loop {
            let Some(job) = &mut self.job else {
                return;
            };
            let event = match job.receiver.try_recv() {
                Ok(event) => event,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.job = None;
                    self.message = "试听操作未完成，请重试。".into();
                    self.failed = true;
                    return;
                }
            };
            match event {
                Event::Started => {
                    job.started = Some(Instant::now());
                    self.message = match job.activity {
                        Activity::Record => "正在录音…",
                        Activity::Original | Activity::Processed => "正在播放…",
                    }
                    .into();
                }
                Event::Finished(result) => {
                    self.job = None;
                    match result {
                        Ok(Outcome::Recorded(recording)) => {
                            self.message = format!("已录音 {:.1} 秒。", recording.duration());
                            self.recording = Some(recording);
                            self.processed = None;
                        }
                        Ok(Outcome::Played(processed)) => {
                            if let Some(processed) = processed {
                                self.processed = Some(processed);
                            }
                            if let Some(recording) = &self.recording {
                                self.message = format!("已录音 {:.1} 秒。", recording.duration());
                            }
                        }
                        Err(error) => {
                            self.message = format!("试听失败：{error:#}");
                            self.failed = true;
                        }
                    }
                    return;
                }
            }
        }
    }
}

impl Drop for AudioSession {
    fn drop(&mut self) {
        self.stop();
    }
}

//! Cancellable background recording, DLL preview and playback.
use super::{
    capture::{Recording, capture},
    playback::playback,
};
use crate::app::i18n::Text;
use crate::app::i18n::{Details, Message};
use crate::shared::settings::Settings;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Instant,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum Activity {
    Recording,
    PlayingOriginal,
    PlayingProcessed,
}

struct ProcessedRecording {
    settings: Settings,
    samples: Arc<Vec<f32>>,
}

enum Outcome {
    Recorded(Arc<Recording>),
    Played(Option<ProcessedRecording>),
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
    processed: Option<ProcessedRecording>,
    pub message: Message,
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
    pub fn recording_elapsed(&self) -> Option<f64> {
        self.job
            .as_ref()
            .filter(|job| job.activity == Activity::Recording)
            .and_then(|job| job.started)
            .map(|started| started.elapsed().as_secs_f64())
    }
    pub fn playback_progress(&self) -> Option<(f64, f64)> {
        let job = self.job.as_ref()?;
        if job.activity == Activity::Recording {
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
        message: Text,
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
                    tracing::info!(?activity, "Audio session started");
                    let _ = sender.send(Event::Started);
                    wake();
                };
                let result = operation(&worker_stop, &started);
                if let Err(error) = &result {
                    tracing::error!(?activity, "Audio session failed: {error:#}");
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
                tracing::error!(?activity, "Failed to spawn audio session: {error}");
                self.message = Message::CannotStartPreview {
                    error: Details::new(error),
                };
                self.failed = true;
            }
        }
    }

    pub fn record(&mut self, device_id: String, wake: impl Fn() + Send + 'static) {
        self.begin(
            wake,
            Activity::Recording,
            Text::OpeningMicrophone,
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
            Activity::PlayingProcessed
        } else {
            Activity::PlayingOriginal
        };
        self.begin(
            wake,
            activity,
            if settings.is_some() {
                Text::PreparingProcessedAudio
            } else {
                Text::OpeningPlaybackDevice
            },
            move |stop, started| {
                let processed = if let Some(settings) = settings {
                    let samples = match cached {
                        Some(samples) => samples,
                        None => Arc::new(super::preview::process(&recording, &settings)?),
                    };
                    Some(ProcessedRecording { settings, samples })
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
                    tracing::error!(activity = ?job.activity, "Audio session exited without a result");
                    self.job = None;
                    self.message = Text::PreviewIncomplete.into();
                    self.failed = true;
                    return;
                }
            };
            match event {
                Event::Started => {
                    job.started = Some(Instant::now());
                    self.message = match job.activity {
                        Activity::Recording => Text::Recording,
                        Activity::PlayingOriginal | Activity::PlayingProcessed => Text::Playing,
                    }
                    .into();
                }
                Event::Finished(result) => {
                    self.job = None;
                    match result {
                        Ok(Outcome::Recorded(recording)) => {
                            tracing::info!(
                                duration_secs = recording.duration(),
                                sample_rate = recording.sample_rate,
                                channels = recording.channels,
                                "Microphone recording completed"
                            );
                            self.message = Message::Recorded {
                                seconds: recording.duration(),
                            };
                            self.recording = Some(recording);
                            self.processed = None;
                        }
                        Ok(Outcome::Played(processed)) => {
                            if let Some(processed) = processed {
                                self.processed = Some(processed);
                            }
                            if let Some(recording) = &self.recording {
                                self.message = Message::Recorded {
                                    seconds: recording.duration(),
                                };
                            }
                        }
                        Err(error) => {
                            self.message = Message::PreviewFailed {
                                error: Details::new(error),
                            };
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

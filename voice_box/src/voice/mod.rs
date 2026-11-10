//! The voice tab: a cancellable test recording, played back original or processed.

mod offline;

use std::sync::{Arc, atomic::Ordering};

use voice_box_shared::apo_config::ApoConfig;

use crate::{
    audio::{Recording, microphone, playback},
    task::{Task, Wake},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Recording,
    PlayingOriginal,
    PlayingProcessed,
}

struct ProcessedRecording {
    config: ApoConfig,
    samples: Arc<Vec<f32>>,
}

enum Outcome {
    Recorded(Arc<Recording>),
    Played(Option<ProcessedRecording>),
}

pub struct VoicePreview {
    wake: Wake,
    task: Option<(Activity, Task<Outcome>)>,
    recording: Option<Arc<Recording>>,
    processed: Option<ProcessedRecording>,
}

impl VoicePreview {
    pub fn new(wake: Wake) -> Self {
        Self {
            wake,
            task: None,
            recording: None,
            processed: None,
        }
    }
    pub fn busy(&self) -> bool {
        self.task.is_some()
    }
    pub fn activity(&self) -> Option<Activity> {
        self.task.as_ref().map(|(activity, _)| *activity)
    }
    /// The device is still being opened or the audio prepared.
    pub fn starting(&self) -> bool {
        self.task
            .as_ref()
            .is_some_and(|(_, task)| task.started_at().is_none())
    }
    pub fn has_recording(&self) -> bool {
        self.recording.is_some()
    }
    pub fn recording_duration_secs(&self) -> Option<f64> {
        self.recording
            .as_ref()
            .map(|recording| recording.duration_secs())
    }
    pub fn stopping(&self) -> bool {
        self.task
            .as_ref()
            .is_some_and(|(_, task)| task.cancelling())
    }
    pub fn recording_elapsed_secs(&self) -> Option<f64> {
        match &self.task {
            Some((Activity::Recording, task)) => {
                task.started_at().map(|at| at.elapsed().as_secs_f64())
            }
            _ => None,
        }
    }
    pub fn playback_progress_secs(&self) -> Option<(f64, f64)> {
        let (activity, task) = self.task.as_ref()?;
        if *activity == Activity::Recording {
            return None;
        }
        let elapsed_secs = task.started_at()?.elapsed().as_secs_f64();
        let duration_secs = self.recording_duration_secs()?;
        Some((elapsed_secs.min(duration_secs), duration_secs))
    }
    pub fn stop(&self) {
        if let Some((_, task)) = &self.task {
            task.cancel();
        }
    }

    pub fn record(&mut self, device_id: String) {
        if self.busy() {
            return;
        }
        let task =
            Task::spawn_cancellable("VoiceBoxRecording", &self.wake, move |stop, started| {
                microphone::capture(&device_id, stop, started)
                    .map(|recording| Outcome::Recorded(Arc::new(recording)))
            });
        self.task = Some((Activity::Recording, task));
    }

    pub fn play(&mut self, config: Option<ApoConfig>) {
        let Some(recording) = self.recording.clone() else {
            return;
        };
        if self.busy() {
            return;
        }
        let cached = config.as_ref().and_then(|config| {
            self.processed
                .as_ref()
                .filter(|processed| &processed.config == config)
                .map(|processed| Arc::clone(&processed.samples))
        });
        let activity = if config.is_some() {
            Activity::PlayingProcessed
        } else {
            Activity::PlayingOriginal
        };
        let task = Task::spawn_cancellable("VoiceBoxPreview", &self.wake, move |stop, started| {
            let processed = match config {
                Some(config) => {
                    let samples = match cached {
                        Some(samples) => samples,
                        None => Arc::new(offline::process(&recording, &config)?),
                    };
                    Some(ProcessedRecording { config, samples })
                }
                None => None,
            };
            if !stop.load(Ordering::Relaxed) {
                let samples = processed
                    .as_ref()
                    .map_or(recording.samples.as_slice(), |processed| {
                        processed.samples.as_slice()
                    });
                playback::play(&recording, samples, stop, started)?;
            }
            Ok(Outcome::Played(processed))
        });
        self.task = Some((activity, task));
    }

    /// Returns the result of an activity once it finishes.
    pub fn poll(&mut self) -> Option<anyhow::Result<()>> {
        let result = self.task.as_mut()?.1.poll()?;
        self.task = None;
        Some(result.map(|outcome| match outcome {
            Outcome::Recorded(recording) => {
                tracing::info!(
                    duration_secs = recording.duration_secs(),
                    sample_rate = recording.sample_rate,
                    channels = recording.channels,
                    "Microphone recording completed"
                );
                self.recording = Some(recording);
                self.processed = None;
            }
            Outcome::Played(processed) => {
                if processed.is_some() {
                    self.processed = processed;
                }
            }
        }))
    }
}

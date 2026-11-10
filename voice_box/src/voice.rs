//! The voice tab: a cancellable test recording, played back once as recorded or
//! looped through the APO's processing chain, which follows config edits live.

use std::{
    slice,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use voice_box_shared::{
    apo_config::ApoConfig,
    dsp::{AudioProcessor, SampleFormat},
};

use crate::{
    audio::{Recording, microphone, playback},
    i18n::{Text, UserContext},
    task::{Task, Wake},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Recording,
    PlayingOriginal,
    PlayingProcessed,
}

pub struct VoicePreview {
    wake: Wake,
    /// The running activity; it yields the new recording, if it made one.
    task: Option<(Activity, Task<Option<Recording>>)>,
    recording: Option<Arc<Recording>>,
    /// The config processed playback reads for each buffer.
    config: Arc<Mutex<ApoConfig>>,
}

impl VoicePreview {
    pub fn new(wake: Wake) -> Self {
        Self {
            wake,
            task: None,
            recording: None,
            config: Arc::default(),
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
        let elapsed_secs = task.started_at()?.elapsed().as_secs_f64();
        let duration_secs = self.recording_duration_secs()?;
        match activity {
            Activity::Recording => None,
            Activity::PlayingOriginal => Some((elapsed_secs.min(duration_secs), duration_secs)),
            Activity::PlayingProcessed => Some((elapsed_secs % duration_secs, duration_secs)),
        }
    }
    pub fn stop(&self) {
        if let Some((_, task)) = &self.task {
            task.cancel();
        }
    }
    /// Applies `config` to processed playback from its next buffer on.
    pub fn set_config(&self, config: &ApoConfig) {
        let mut live = lock(&self.config);
        if *live != *config {
            live.clone_from(config);
        }
    }

    pub fn record(&mut self, device_id: String) {
        if self.busy() {
            return;
        }
        let task =
            Task::spawn_cancellable("VoiceBoxRecording", &self.wake, move |stop, started| {
                microphone::capture(&device_id, stop, started).map(Some)
            });
        self.task = Some((Activity::Recording, task));
    }

    /// Plays the recording once, or loops it processed with the live config.
    pub fn play(&mut self, processed: bool) {
        let Some(recording) = self.recording.clone() else {
            return;
        };
        if self.busy() {
            return;
        }
        let config = Arc::clone(&self.config);
        let task = Task::spawn_cancellable("VoiceBoxPreview", &self.wake, move |stop, started| {
            if processed {
                playback::play(
                    &recording,
                    process_loop(&recording, &config)?,
                    stop,
                    started,
                )?;
            } else {
                playback::play(
                    &recording,
                    playback::once(&recording.samples),
                    stop,
                    started,
                )?;
            }
            Ok(None)
        });
        let activity = if processed {
            Activity::PlayingProcessed
        } else {
            Activity::PlayingOriginal
        };
        self.task = Some((activity, task));
    }

    /// Returns the result of an activity once it finishes.
    pub fn poll(&mut self) -> Option<anyhow::Result<()>> {
        let result = self.task.as_mut()?.1.poll()?;
        self.task = None;
        Some(result.map(|recording| {
            if let Some(recording) = recording {
                tracing::info!(
                    duration_secs = recording.duration_secs(),
                    sample_rate = recording.sample_rate,
                    channels = recording.channels,
                    "Microphone recording completed"
                );
                self.recording = Some(Arc::new(recording));
            }
        }))
    }
}

/// Endlessly fills buffers with the recording run through the APO's processing
/// chain, which, like the APO, ramps to the current config at each buffer.
fn process_loop<'a>(
    recording: &'a Recording,
    config: &'a Mutex<ApoConfig>,
) -> anyhow::Result<impl FnMut(&mut [f32]) -> usize + 'a> {
    let mut processor = AudioProcessor::new(
        SampleFormat::Float32,
        recording.channels.into(),
        recording.sample_rate.into(),
        true,
        &lock(config),
    )
    .user_context(Text::ProcessPreviewFailed)?;
    let mut source = recording.samples.iter().copied().cycle();
    Ok(move |buffer: &mut [f32]| {
        buffer.fill_with(|| source.next().unwrap_or_default());
        let config = lock(config).clone();
        // The float buffer is initialized and exclusively borrowed for this call.
        let bytes = unsafe {
            slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), size_of_val(buffer))
        };
        processor.process_with_config(bytes, &config, None);
        buffer.len()
    })
}

fn lock(config: &Mutex<ApoConfig>) -> MutexGuard<'_, ApoConfig> {
    config.lock().unwrap_or_else(PoisonError::into_inner)
}

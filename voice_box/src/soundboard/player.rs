//! Playback lifecycle and acknowledgements, addressed by stable sound IDs.

use std::sync::mpsc;

use voice_box_shared::pcm_transport;

use crate::{
    devices,
    i18n::{Details, Message},
    model::SoundId,
    soundboard::{
        Update,
        settings::SoundboardSettings,
        worker::{self, Command, Config, Event},
    },
    task::{Task, Wake},
};

#[derive(Clone, Copy, Default)]
pub struct PlaybackStatus {
    pub sound_id: Option<SoundId>,
    pub elapsed_secs: f64,
    pub duration_secs: Option<f64>,
}

struct Request {
    id: SoundId,
    duration_secs: f64,
    config: Config,
}

struct Job {
    sender: mpsc::Sender<Command>,
    receiver: mpsc::Receiver<Event>,
    task: Task<()>,
}

/// May be waited on by a background deletion job, never by the UI thread.
pub struct Completion(Task<()>);

impl Completion {
    pub fn wait(self) {
        self.0.join();
    }
}

pub struct Player {
    wake: Wake,
    job: Option<Job>,
    revision: u64,
    queued: Option<Request>,
    status: PlaybackStatus,
}

impl Player {
    pub fn new(wake: Wake) -> Self {
        Self {
            wake,
            job: None,
            revision: 0,
            queued: None,
            status: PlaybackStatus::default(),
        }
    }

    pub fn status(&self) -> PlaybackStatus {
        self.status
    }

    pub fn playing(&self) -> bool {
        self.job.is_some()
    }

    pub fn play(&mut self, id: SoundId, duration_secs: f64, settings: &SoundboardSettings) {
        self.start(id, duration_secs, Config::new(settings));
    }

    fn start(&mut self, id: SoundId, duration_secs: f64, config: Config) {
        (self.wake)();
        self.revision = self.revision.wrapping_add(1);
        self.status = PlaybackStatus {
            sound_id: Some(id),
            elapsed_secs: 0.0,
            duration_secs: Some(duration_secs),
        };
        if let Some(job) = &self.job {
            // A replacement remains queued until its Started acknowledgement.
            // If Stop or completion races the send, poll starts a fresh worker.
            self.queued = Some(Request {
                id,
                duration_secs,
                config: config.clone(),
            });
            let _ = job.sender.send(Command::Replace(id, self.revision, config));
            return;
        }
        self.job = Some(start_worker(id, self.revision, config, &self.wake));
    }

    pub fn stop(&mut self) {
        self.queued = None;
        if let Some(job) = &self.job {
            let _ = job.sender.send(Command::Stop);
        }
    }

    pub fn stop_and_release(&mut self) -> Option<Completion> {
        self.stop();
        self.status.sound_id = None;
        self.job.take().map(|job| Completion(job.task))
    }

    pub fn cancel_queued(&mut self) {
        self.queued = None;
    }

    pub fn configure(&mut self, settings: &SoundboardSettings) {
        let config = Config::new(settings);
        if let Some(request) = &mut self.queued {
            request.config = config.clone();
        }
        if let Some(job) = &self.job {
            let _ = job.sender.send(Command::Configure(config));
        }
    }

    pub fn poll(&mut self) -> Vec<Update> {
        let mut updates = Vec::new();
        if let Some(job) = &mut self.job {
            // Check completion first, so events sent just before it are still drained.
            let finished = job.task.poll();
            for event in job.receiver.try_iter() {
                match event {
                    Event::Started {
                        revision,
                        duration_secs,
                    } if revision == self.revision => {
                        self.queued = None;
                        self.status.duration_secs = duration_secs.or(self.status.duration_secs);
                        updates.push(Update::Cleared);
                    }
                    Event::Progress {
                        revision,
                        elapsed_secs,
                    } if revision == self.revision => self.status.elapsed_secs = elapsed_secs,
                    Event::OutputError(revision, error) if revision == self.revision => {
                        updates.push(Update::Error(error));
                    }
                    _ => {}
                }
            }
            if let Some(result) = finished {
                self.finish();
                if let Err(error) = result {
                    updates.push(Update::Error(Message::PlaybackFailed {
                        error: Details::new(error),
                    }));
                }
            }
        }
        if self.job.is_none()
            && let Some(request) = self.queued.take()
        {
            updates.push(Update::Cleared);
            self.start(request.id, request.duration_secs, request.config);
        }
        updates
    }

    fn finish(&mut self) {
        if let Some(job) = self.job.take() {
            job.task.join();
        }
        self.status.sound_id = None;
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
        self.finish();
    }
}

fn start_worker(id: SoundId, revision: u64, config: Config, wake: &Wake) -> Job {
    let (sender, commands) = mpsc::channel();
    let (events, receiver) = mpsc::channel();
    let task = Task::run("VoiceBoxSoundboard", wake, move || {
        // A device stays fixed for a worker, so discovery and plugin checks happen once,
        // here: they use COM and the registry, which must stay off the UI thread.
        let transport = devices::soundboard_microphone_guid(config.microphone_device.as_deref())
            .and_then(|guid| pcm_transport::mapping_name(&guid))
            .map_err(Details::new);
        let outcome = worker::run(id, config, revision, transport, &commands, &events)?;
        tracing::info!(
            sound_id = outcome.sound_id,
            reason = outcome.reason,
            played_secs = outcome.played_secs,
            "Soundboard playback finished"
        );
        Ok(())
    });
    Job {
        sender,
        receiver,
        task,
    }
}

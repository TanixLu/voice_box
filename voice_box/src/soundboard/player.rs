//! Playback lifecycle and acknowledgements, addressed by stable sound IDs.

use std::{sync::mpsc, thread::JoinHandle};

use crate::{
    i18n::{Details, Message, Text},
    soundboard::{
        playback::{self, Command, Config, Event},
        sound::SoundId,
    },
};

pub enum Update {
    Started,
    Restarted,
    Error(Message),
}

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
    thread: JoinHandle<()>,
}

/// May be waited on by a background deletion job, never by the UI thread.
pub struct Completion(JoinHandle<()>);

impl Completion {
    pub fn wait(self) {
        let _ = self.0.join();
    }
}

#[derive(Default)]
pub struct Player {
    job: Option<Job>,
    revision: u64,
    queued: Option<Request>,
    status: PlaybackStatus,
}

impl Player {
    pub fn status(&self) -> PlaybackStatus {
        self.status
    }

    pub fn playing(&self) -> bool {
        self.job.is_some()
    }

    pub fn play(
        &mut self,
        id: SoundId,
        duration_secs: f64,
        config: Config,
        wake: impl Fn() + Send + 'static,
    ) -> anyhow::Result<()> {
        wake();
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
            return Ok(());
        }
        match start_worker(id, self.revision, config, wake) {
            Ok(job) => self.job = Some(job),
            Err(error) => {
                self.status.sound_id = None;
                return Err(error);
            }
        }
        Ok(())
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
        self.job.take().map(|job| Completion(job.thread))
    }

    pub fn cancel_queued(&mut self) {
        self.queued = None;
    }

    pub fn configure(&mut self, config: Config) {
        if let Some(request) = &mut self.queued {
            request.config = config.clone();
        }
        if let Some(job) = &self.job {
            let _ = job.sender.send(Command::Configure(config));
        }
    }

    pub fn poll(&mut self, wake: impl Fn() + Send + 'static) -> Vec<Update> {
        let mut updates = Vec::new();
        while let Some(job) = &self.job {
            match job.receiver.try_recv() {
                Ok(Event::Started {
                    revision,
                    duration_secs,
                }) if revision == self.revision => {
                    self.queued = None;
                    self.status.duration_secs = duration_secs.or(self.status.duration_secs);
                    updates.push(Update::Started);
                }
                Ok(Event::Progress {
                    revision,
                    elapsed_secs,
                }) if revision == self.revision => {
                    self.status.elapsed_secs = elapsed_secs;
                }
                Ok(Event::OutputError(revision, error)) if revision == self.revision => {
                    updates.push(Update::Error(error));
                }
                Ok(Event::Finished(result)) => {
                    self.finish();
                    if let Err(error) = result {
                        updates.push(Update::Error(Message::PlaybackFailed {
                            error: Details::new(error),
                        }));
                    }
                    break;
                }
                Ok(_) => {}
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    tracing::error!(sound_id = ?self.status.sound_id, "Soundboard playback exited without a result");
                    self.finish();
                    updates.push(Update::Error(Text::PlaybackThreadExited.into()));
                    break;
                }
            }
        }
        if self.job.is_none()
            && let Some(request) = self.queued.take()
        {
            updates.push(Update::Restarted);
            if let Err(error) = self.play(request.id, request.duration_secs, request.config, wake) {
                updates.push(Update::Error(Message::StartPlaybackFailed {
                    error: Details::new(error),
                }));
            }
        }
        updates
    }

    fn finish(&mut self) {
        if let Some(job) = self.job.take() {
            let _ = job.thread.join();
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

fn start_worker(
    id: SoundId,
    revision: u64,
    config: Config,
    wake: impl Fn() + Send + 'static,
) -> anyhow::Result<Job> {
    let (sender, commands) = mpsc::channel();
    let (events, receiver) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("VoiceBoxSoundboard".into())
        .spawn(move || {
            let started = std::time::Instant::now();
            let result = playback::run(id, config, revision, &commands, &events);
            match &result {
                Err(error) => tracing::error!(
                    elapsed_secs = started.elapsed().as_secs_f64(),
                    "Soundboard failed: {error:#}"
                ),
                Ok(outcome) => tracing::info!(
                    sound_id = outcome.sound_id,
                    reason = outcome.reason,
                    played_secs = outcome.played_secs,
                    elapsed_secs = started.elapsed().as_secs_f64(),
                    "Soundboard playback finished"
                ),
            }
            let _ = events.send(Event::Finished(result.map(|_| ())));
            wake();
        })?;
    Ok(Job {
        sender,
        receiver,
        thread,
    })
}

//! Sound library operations and background playback coordination.
use crate::app::hotkeys::Hotkey;
use crate::app::i18n::Text;
use crate::app::i18n::{Details, Message, UserContext};
use anyhow::ensure;
use std::{collections::HashSet, path::PathBuf, sync::mpsc, thread::JoinHandle};

mod decoder;
pub(super) mod library;
mod playback;
pub(super) mod recording;
pub(super) mod rename;
mod shuffle;

use crate::app::settings::{BindingTarget, Settings};
use library::{ImportedSounds, Library, SoundId, import_files};
use playback::{Command, Event};

struct PlaybackJob {
    sender: mpsc::Sender<Command>,
    receiver: mpsc::Receiver<Event>,
    thread: JoinHandle<()>,
}

struct RemovalJob {
    ids: HashSet<SoundId>,
    receiver: mpsc::Receiver<anyhow::Result<Library>>,
}

#[derive(Default)]
pub(super) struct Soundboard {
    library: Library,
    pub settings: Settings,
    pub active_sound_index: Option<usize>,
    pub elapsed: f64,
    pub duration: Option<f64>,
    pub message: Message,
    pub failed: bool,
    pub import_error: Option<Message>,
    playback_job: Option<PlaybackJob>,
    playback_revision: u64,
    queued_playback: Option<usize>,
    import_job: Option<mpsc::Receiver<anyhow::Result<ImportedSounds>>>,
    removal_job: Option<RemovalJob>,
    removed_sounds: Option<HashSet<SoundId>>,
    rename_job: Option<mpsc::Receiver<(Library, anyhow::Result<()>)>>,
    rename_result: Option<anyhow::Result<()>>,
    shuffle: shuffle::Shuffle,
}
impl Soundboard {
    pub fn new(mut settings: Settings, library: Library) -> Self {
        settings.sanitize();
        let mut result = Self::default();
        result.settings = settings;
        result.library = library;
        result
    }
    pub fn save_settings(&mut self, settings: Settings) -> anyhow::Result<()> {
        settings.validate(&self.library)?;
        settings.save()?;
        if settings.playback_microphone_output_device
            != self.settings.playback_microphone_output_device
            || settings.playback_speaker_output_device
                != self.settings.playback_speaker_output_device
        {
            self.stop();
        }
        self.settings = settings;
        self.update_playback_settings();
        Ok(())
    }

    pub fn library(&self) -> &Library {
        &self.library
    }
    pub fn random_sound_index(&mut self, hotkey: Option<Hotkey>) -> Option<usize> {
        self.shuffle.choose(&self.library.sounds, hotkey)
    }
    pub fn move_sound(&mut self, from: usize, to: usize) -> anyhow::Result<()> {
        self.library.move_sound(from, to, Library::save)?;
        if let Some(index) = &mut self.active_sound_index {
            *index = moved_index(*index, from, to);
        }
        if let Some(index) = &mut self.queued_playback {
            *index = moved_index(*index, from, to);
        }
        Ok(())
    }
    pub fn move_sounds(&mut self, ids: &HashSet<SoundId>, to_top: bool) -> anyhow::Result<()> {
        let order = self.library.move_sounds(ids, to_top, Library::save)?;
        self.active_sound_index = self
            .active_sound_index
            .and_then(|index| order.iter().position(|&old| old == index));
        self.queued_playback = self
            .queued_playback
            .and_then(|index| order.iter().position(|&old| old == index));
        Ok(())
    }

    pub fn batch_renaming(&self) -> bool {
        self.rename_job.is_some()
    }

    pub fn take_rename_result(&mut self) -> Option<anyhow::Result<()>> {
        self.rename_result.take()
    }

    pub fn rename_sounds(
        &mut self,
        plan: rename::RenamePlan,
        wake: impl Fn() + Send + 'static,
    ) -> anyhow::Result<()> {
        ensure!(!self.playing(), Text::StopBeforeRename);
        ensure!(
            !self.importing() && !self.deleting() && !self.batch_renaming(),
            Text::RenameBusy
        );
        let mut library = self.library.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("VoiceBoxBatchRename".into())
            .spawn(move || {
                let result = library.rename_batch(plan, Library::save);
                let _ = sender.send((library, result));
                wake();
            })
            .user_context(Text::RenameSoundFailed)?;
        self.rename_result = None;
        self.rename_job = Some(receiver);
        Ok(())
    }
    pub fn bind_hotkey(
        &mut self,
        target: BindingTarget,
        key: Option<Hotkey>,
        draft: Option<&mut Settings>,
    ) -> anyhow::Result<()> {
        if let Some(key) = key {
            draft
                .as_deref()
                .unwrap_or(&self.settings)
                .validate_binding(&self.library, target, key)?;
        }
        match target {
            BindingTarget::Sound(index) => {
                let id = self
                    .library
                    .sounds
                    .get(index)
                    .user_context(Text::SoundNotFound)?
                    .id;
                self.library
                    .set_hotkeys(&HashSet::from([id]), key, Library::save)
            }
            BindingTarget::Hold | BindingTarget::Stop | BindingTarget::Random => {
                let settings = draft.user_context(Text::OpenSoundSettingsFirst)?;
                match target {
                    BindingTarget::Hold => settings.playback_auto_hold_hotkey = key,
                    BindingTarget::Stop => settings.playback_stop_hotkey = key,
                    BindingTarget::Random => settings.playback_random_hotkey = key,
                    BindingTarget::Sound(_) => unreachable!(),
                }
                Ok(())
            }
        }
    }
    pub fn bind_sound_hotkeys(
        &mut self,
        ids: &HashSet<SoundId>,
        key: Option<Hotkey>,
    ) -> anyhow::Result<()> {
        if let Some(key) = key {
            let index = self
                .library
                .sounds
                .iter()
                .position(|sound| ids.contains(&sound.id))
                .user_context(Text::SoundNotFound)?;
            self.settings
                .validate_binding(&self.library, BindingTarget::Sound(index), key)?;
        }
        self.library.set_hotkeys(ids, key, Library::save)
    }
    pub fn busy(&self) -> bool {
        self.playback_job.is_some() || self.importing() || self.deleting() || self.batch_renaming()
    }
    pub fn playing(&self) -> bool {
        self.playback_job.is_some()
    }
    pub fn importing(&self) -> bool {
        self.import_job.is_some()
    }
    pub fn deleting(&self) -> bool {
        self.removal_job.is_some()
    }
    pub fn take_removed_sounds(&mut self) -> Option<HashSet<SoundId>> {
        self.removed_sounds.take()
    }
    pub fn add_recording(&mut self, sound: library::Sound) {
        tracing::info!(sound_id = sound.id, "Recorded sound committed to library");
        self.library.sounds.push(sound);
        self.failed = false;
        self.message.clear();
    }
    pub fn import(
        &mut self,
        select_files: impl FnOnce() -> anyhow::Result<Vec<PathBuf>> + Send + 'static,
        wake: impl Fn() + Send + 'static,
    ) {
        if self.importing() || self.deleting() || self.batch_renaming() {
            return;
        }
        // Stop before opening the picker: importing disables playback controls.
        // Also cancel any pending replacement so it cannot restart behind it.
        self.stop();
        let (tx, rx) = mpsc::channel();
        match std::thread::Builder::new()
            .name("VoiceBoxSoundPicker".into())
            .spawn(move || {
                let _ = tx.send(select_files().and_then(import_files));
                wake();
            }) {
            Ok(_) => self.import_job = Some(rx),
            Err(e) => {
                tracing::error!("Failed to spawn sound import: {e}");
                self.import_error = Some(Message::OpenPickerFailed {
                    error: Details::new(e),
                });
            }
        }
    }
    pub fn set_error(&mut self, message: Message) {
        self.failed = true;
        self.message = message;
    }
    fn finish_import(&mut self, mut imported: ImportedSounds) {
        let previous_count = self.library.sounds.len();
        self.library.sounds.append(&mut imported.sounds);
        if !imported.errors.is_empty() {
            tracing::warn!(
                errors = ?imported.errors,
                "Some sound files could not be imported"
            );
            self.import_error = Some(Message::Many(imported.errors.clone()));
        } else {
            self.failed = false;
            self.message.clear();
        }
        let added = self.library.sounds.len() - previous_count;
        if added > 0 || !imported.errors.is_empty() {
            tracing::info!(
                added,
                failed = imported.errors.len(),
                "Sound import processed"
            );
        }
    }
    pub fn rename(&mut self, index: usize, name: &str) -> anyhow::Result<()> {
        ensure!(!self.playing(), Text::StopBeforeRename);
        self.library.rename(index, name, Library::save)
    }
    pub fn remove(&mut self, ids: &HashSet<SoundId>, wake: impl Fn() + Send + 'static) {
        if ids.is_empty() || self.importing() || self.deleting() || self.batch_renaming() {
            return;
        }
        let active_id = self
            .active_sound_index
            .and_then(|index| self.library.sounds.get(index))
            .map(|sound| sound.id);
        let stopped_playback = if active_id.is_some_and(|id| ids.contains(&id)) {
            self.stop();
            self.active_sound_index = None;
            self.playback_job.take().map(|job| job.thread)
        } else {
            None
        };
        let reclaim_space = !self.playing();
        self.queued_playback = None;
        self.failed = false;
        self.message.clear();
        let ids = ids.clone();
        let selected = ids.clone();
        let mut library = self.library.clone();
        let (sender, receiver) = mpsc::channel();
        match std::thread::Builder::new()
            .name("VoiceBoxSoundRemoval".into())
            .spawn(move || {
                let started = std::time::Instant::now();
                tracing::info!(count = selected.len(), "Sound deletion started");
                // Waiting for playback, cascading BLOB deletion, transaction
                // commit and connection cleanup must all stay off the UI thread.
                if let Some(thread) = stopped_playback {
                    let _ = thread.join();
                }
                let result = crate::app::database::open()
                    .and_then(|mut connection| {
                        library.remove(&mut connection, &selected, reclaim_space)
                    })
                    .map(|()| library);
                match &result {
                    Ok(_) => tracing::info!(
                        count = selected.len(),
                        elapsed_ms = started.elapsed().as_millis(),
                        "Sound deletion completed"
                    ),
                    Err(error) => tracing::error!(
                        count = selected.len(),
                        elapsed_ms = started.elapsed().as_millis(),
                        "Sound deletion failed: {error:#}"
                    ),
                }
                let _ = sender.send(result);
                wake();
            }) {
            Ok(_) => self.removal_job = Some(RemovalJob { ids, receiver }),
            Err(error) => self.set_error(Message::DeleteSoundFailed {
                error: Details::new(error),
            }),
        }
    }
    pub fn play(&mut self, index: usize, wake: impl Fn() + Send + 'static) {
        if self.importing() || self.deleting() || self.batch_renaming() {
            return;
        }
        let Some(sound) = self.library.sounds.get(index) else {
            return;
        };
        if self.settings.playback_speaker_output_volume == 0.0
            && self.settings.playback_microphone_output_volume == 0.0
        {
            self.set_error(Text::OutputsMuted.into());
            return;
        }
        self.shuffle.last = Some(sound.id);
        wake();
        let id = sound.id;
        let settings = self.settings.clone();
        self.playback_revision = self.playback_revision.wrapping_add(1);
        let revision = self.playback_revision;
        self.failed = false;
        self.elapsed = 0.0;
        self.duration = Some(sound.duration);
        self.message.clear();
        self.active_sound_index = Some(index);
        if let Some(job) = &self.playback_job {
            // Retain the request until Started acknowledges it. If completion or
            // Stop races this send, poll restarts it after the old worker exits.
            self.queued_playback = Some(index);
            let _ = job.sender.send(Command::Replace(id, revision, settings));
            return;
        }
        let (tx, rx) = mpsc::channel();
        let (events, receiver) = mpsc::channel();
        match std::thread::Builder::new()
            .name("VoiceBoxSoundboard".into())
            .spawn(move || {
                let started = std::time::Instant::now();
                let result = playback::run(id, settings, revision, &rx, &events);
                match &result {
                    Err(e) => {
                        tracing::error!(
                            elapsed_secs = started.elapsed().as_secs_f64(),
                            "Soundboard failed: {e:#}"
                        );
                    }
                    Ok(outcome) => {
                        tracing::info!(
                            sound_id = outcome.sound_id,
                            reason = outcome.reason,
                            played_secs = outcome.played_secs,
                            elapsed_secs = started.elapsed().as_secs_f64(),
                            "Soundboard playback finished"
                        );
                    }
                }
                let _ = events.send(Event::Finished(result.map(|_| ())));
                wake();
            }) {
            Ok(thread) => {
                self.active_sound_index = Some(index);
                self.playback_job = Some(PlaybackJob {
                    sender: tx,
                    receiver,
                    thread,
                });
            }
            Err(e) => {
                self.active_sound_index = None;
                tracing::error!(
                    sound_index = index,
                    "Failed to spawn soundboard playback: {e}"
                );
                self.set_error(Message::StartPlaybackFailed {
                    error: Details::new(e),
                });
            }
        }
    }
    pub fn stop(&mut self) {
        self.queued_playback = None;
        if let Some(job) = &self.playback_job {
            let _ = job.sender.send(Command::Stop);
        }
    }
    pub fn update_playback_settings(&self) {
        if let Some(job) = &self.playback_job {
            let _ = job.sender.send(Command::Configure(self.settings.clone()));
        }
    }
    pub fn poll(&mut self, wake: impl Fn() + Clone + Send + 'static) {
        let removal = match self.removal_job.as_ref().map(|job| job.receiver.try_recv()) {
            Some(Ok(result)) => Some(result),
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                Some(Err(anyhow::anyhow!(Text::DeleteThreadExited)))
            }
            _ => None,
        };
        if let Some(result) = removal {
            let job = self.removal_job.take().expect("pending sound deletion");
            match result {
                Ok(library) => {
                    let active_id = self
                        .active_sound_index
                        .and_then(|index| self.library.sounds.get(index))
                        .map(|sound| sound.id);
                    self.library = library;
                    self.active_sound_index = active_id
                        .and_then(|id| self.library.sounds.iter().position(|sound| sound.id == id));
                    self.removed_sounds = Some(job.ids);
                    self.failed = false;
                    self.message.clear();
                }
                Err(error) => self.set_error(Message::DeleteSoundFailed {
                    error: Details::new(error),
                }),
            }
        }
        if let Some(job) = &self.rename_job {
            match job.try_recv() {
                Ok((library, result)) => {
                    self.library = library;
                    self.rename_job = None;
                    self.failed = result.is_err();
                    self.message.clear();
                    self.rename_result = Some(result.map_err(|error| {
                        self.message = Message::error(error);
                        anyhow::anyhow!(self.message.clone())
                    }));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.rename_job = None;
                    self.rename_result = Some(Err(anyhow::anyhow!(Text::RenameThreadExited)));
                    self.set_error(Text::RenameThreadExited.into());
                }
                Err(mpsc::TryRecvError::Empty) => return,
            }
        }
        if let Some(import_job) = &self.import_job {
            match import_job.try_recv() {
                Ok(result) => {
                    self.import_job = None;
                    match result {
                        Ok(imported) => self.finish_import(imported),
                        Err(e) => {
                            tracing::error!("Sound file selection or import failed: {e:#}");
                            self.import_error = Some(Message::ImportSoundsFailed {
                                error: Details::new(e),
                            });
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    tracing::error!("Sound import exited without a result");
                    self.import_job = None;
                    self.import_error = Some(Text::ImportThreadExited.into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        while let Some(job) = &self.playback_job {
            match job.receiver.try_recv() {
                Ok(Event::Started(revision, duration)) => {
                    if revision == self.playback_revision {
                        self.queued_playback = None;
                        self.duration = duration.or(self.duration);
                        self.message.clear();
                    }
                }
                Ok(Event::Progress(revision, elapsed)) => {
                    if revision == self.playback_revision {
                        self.elapsed = elapsed;
                    }
                }
                Ok(Event::OutputError(revision, error)) => {
                    if revision == self.playback_revision {
                        self.set_error(error);
                    }
                }
                Ok(Event::Finished(result)) => {
                    if let Some(job) = self.playback_job.take() {
                        let _ = job.thread.join();
                    }
                    self.active_sound_index = None;
                    if let Err(e) = result {
                        self.set_error(Message::PlaybackFailed {
                            error: Details::new(e),
                        });
                    }
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    tracing::error!(
                        sound_index = ?self.active_sound_index,
                        "Soundboard playback exited without a result"
                    );
                    if let Some(job) = self.playback_job.take() {
                        let _ = job.thread.join();
                    }
                    self.active_sound_index = None;
                    self.set_error(Text::PlaybackThreadExited.into());
                    break;
                }
            }
        }
        if self.playback_job.is_none()
            && let Some(index) = self.queued_playback.take()
        {
            self.play(index, wake);
        }
    }
}
impl Drop for Soundboard {
    fn drop(&mut self) {
        self.stop();
        if let Some(job) = self.playback_job.take() {
            let _ = job.thread.join();
        }
    }
}

fn moved_index(index: usize, from: usize, to: usize) -> usize {
    if index == from {
        to
    } else if from < index && index <= to {
        index - 1
    } else if to <= index && index < from {
        index + 1
    } else {
        index
    }
}

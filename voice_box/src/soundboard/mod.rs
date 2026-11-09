//! Sound library operations and background playback coordination.

pub mod bindings;
mod decoder;
mod export;
mod import;
pub mod library;
mod playback;
mod player;
pub mod prefs;
pub mod recording;
pub mod rename;
mod shuffle;

use std::{collections::HashSet, path::PathBuf};

use anyhow::ensure;

pub use crate::soundboard::player::PlaybackStatus;
use crate::{
    i18n::{Details, Message, Text},
    model::{Hotkey, Sound, SoundId},
    soundboard::{
        import::{ImportedSounds, import_files},
        library::Library,
        player::Player,
        prefs::Prefs,
    },
    task::{Task, Wake},
};

/// What the view should show after background work progressed.
pub enum Update {
    /// A previous playback or library error no longer applies.
    Cleared,
    Error(Message),
    /// Shown as a dialog with this title.
    FileError(Text, Message),
    Edited(anyhow::Result<()>),
    Removed(HashSet<SoundId>),
}

/// Library writers exclude each other, so they share one slot.
enum LibraryTask {
    Edit(Task<Library>),
    Import(Task<ImportedSounds>),
    Export(Task<()>),
    Remove(HashSet<SoundId>, Task<Library>),
}

pub struct Soundboard {
    library: Library,
    prefs: Prefs,
    player: Player,
    library_task: Option<LibraryTask>,
    shuffle: shuffle::Shuffle,
    wake: Wake,
}
impl Soundboard {
    pub fn new(library: Library, prefs: Prefs, wake: Wake) -> Self {
        Self {
            library,
            prefs,
            player: Player::new(wake.clone()),
            wake,
            library_task: None,
            shuffle: shuffle::Shuffle::default(),
        }
    }
    pub fn prefs(&self) -> &Prefs {
        &self.prefs
    }
    pub fn configure(&mut self, prefs: Prefs) {
        let (config, current) = (prefs.playback(), self.prefs.playback());
        if config.microphone_device != current.microphone_device
            || config.speaker_device != current.speaker_device
        {
            self.stop();
        }
        self.player.configure(config);
        self.prefs = prefs;
    }
    pub fn library(&self) -> &Library {
        &self.library
    }
    pub fn random_sound_index(&mut self, hotkey: Option<Hotkey>) -> Option<usize> {
        self.shuffle.choose(&self.library.sounds, hotkey)
    }
    pub fn busy(&self) -> bool {
        self.playing() || self.library_busy()
    }
    pub fn library_busy(&self) -> bool {
        self.library_task.is_some()
    }
    pub fn playing(&self) -> bool {
        self.player.playing()
    }
    pub fn importing(&self) -> bool {
        matches!(self.library_task, Some(LibraryTask::Import(_)))
    }
    pub fn add_recording(&mut self, sound: Sound) {
        tracing::info!(sound_id = sound.id, "Recorded sound committed to library");
        self.library.sounds.push(sound);
    }
    /// Applies `edit` to a copy of the library and saves it in the background.
    pub fn edit(
        &mut self,
        edit: impl FnOnce(&mut Library) -> anyhow::Result<()> + Send + 'static,
    ) -> anyhow::Result<()> {
        ensure!(!self.library_busy(), Text::RenameBusy);
        let mut library = self.library.clone();
        let task = Task::run("VoiceBoxLibrarySave", &self.wake, move || {
            edit(&mut library)?;
            library.save()?;
            Ok(library)
        });
        self.library_task = Some(LibraryTask::Edit(task));
        Ok(())
    }
    pub fn import(
        &mut self,
        select_files: impl FnOnce() -> anyhow::Result<Vec<PathBuf>> + Send + 'static,
    ) {
        if self.library_busy() {
            return;
        }
        // Importing disables playback controls; a queued replacement must not restart behind it.
        self.stop();
        let task = Task::run("VoiceBoxSoundImport", &self.wake, move || {
            select_files().and_then(import_files)
        });
        self.library_task = Some(LibraryTask::Import(task));
    }
    pub fn export(
        &mut self,
        ids: &HashSet<SoundId>,
        select_folder: impl FnOnce() -> anyhow::Result<Option<PathBuf>> + Send + 'static,
    ) {
        if self.library_busy() {
            return;
        }
        self.stop();
        let sounds: Vec<_> = self
            .library
            .sounds
            .iter()
            .filter(|sound| ids.contains(&sound.id))
            .cloned()
            .collect();
        let task = Task::run("VoiceBoxSoundExport", &self.wake, move || {
            select_folder()?.map_or(Ok(()), |folder| export::export_sounds(&sounds, &folder))
        });
        self.library_task = Some(LibraryTask::Export(task));
    }
    pub fn remove(&mut self, ids: &HashSet<SoundId>) {
        if ids.is_empty() || self.library_busy() {
            return;
        }
        let stopped_playback = if self.playback().sound_id.is_some_and(|id| ids.contains(&id)) {
            self.player.stop_and_release()
        } else {
            None
        };
        let reclaim_space = !self.playing();
        self.player.cancel_queued();
        let selected = ids.clone();
        let mut library = self.library.clone();
        let task = Task::run("VoiceBoxSoundRemoval", &self.wake, move || {
            // Waiting for playback, cascading BLOB deletion, transaction
            // commit and connection cleanup must all stay off the UI thread.
            if let Some(playback) = stopped_playback {
                playback.wait();
            }
            library.remove(&selected, reclaim_space)?;
            Ok(library)
        });
        self.library_task = Some(LibraryTask::Remove(ids.clone(), task));
    }
    pub fn play(&mut self, index: usize) -> anyhow::Result<()> {
        let Some(sound) = self.library.sounds.get(index) else {
            return Ok(());
        };
        if self.library_busy() {
            return Ok(());
        }
        ensure!(
            !self.prefs.speaker_gain_percent.is_mute()
                || !self.prefs.microphone_gain_percent.is_mute(),
            Text::OutputsMuted
        );
        self.shuffle.last = Some(sound.id);
        self.player
            .play(sound.id, sound.duration_secs, self.prefs.playback());
        Ok(())
    }
    pub fn stop(&mut self) {
        self.player.stop();
    }
    pub fn playback(&self) -> PlaybackStatus {
        self.player.status()
    }
    pub fn active_sound(&self) -> Option<&Sound> {
        let id = self.playback().sound_id?;
        self.library.sounds.iter().find(|sound| sound.id == id)
    }
    pub fn poll(&mut self) -> Vec<Update> {
        let mut updates = self.player.poll();
        updates.extend(self.poll_library());
        updates
    }
    fn poll_library(&mut self) -> Option<Update> {
        let update = match self.library_task.as_mut()? {
            LibraryTask::Edit(task) => Update::Edited(task.poll()?.map(|library| {
                self.library = library;
            })),
            LibraryTask::Import(task) => match task.poll()? {
                Ok(mut imported) => {
                    tracing::info!(
                        added = imported.sounds.len(),
                        failed = imported.errors.len(),
                        "Sound import processed"
                    );
                    self.library.sounds.append(&mut imported.sounds);
                    if imported.errors.is_empty() {
                        Update::Cleared
                    } else {
                        let errors = Message::Many(imported.errors);
                        Update::FileError(Text::SoundImportFailedTitle, errors)
                    }
                }
                Err(error) => Update::FileError(
                    Text::SoundImportFailedTitle,
                    Message::ImportSoundsFailed {
                        error: Details::new(error),
                    },
                ),
            },
            LibraryTask::Export(task) => match task.poll()? {
                Ok(()) => Update::Cleared,
                Err(error) => {
                    Update::FileError(Text::SoundExportFailedTitle, Message::error(error))
                }
            },
            LibraryTask::Remove(ids, task) => match task.poll()? {
                Ok(library) => {
                    self.library = library;
                    Update::Removed(std::mem::take(ids))
                }
                Err(error) => Update::Error(Message::DeleteSoundFailed {
                    error: Details::new(error),
                }),
            },
        };
        self.library_task = None;
        Some(update)
    }
}

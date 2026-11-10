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
mod shortcuts;
mod shuffle;

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use anyhow::ensure;
use windows::Win32::Foundation::HWND;

pub use crate::soundboard::player::PlaybackStatus;
use crate::{
    audio::capture::Recording,
    i18n::{Details, Message, Text, UserContext},
    model::{Hotkey, Sound, SoundId},
    soundboard::{
        bindings::{Action, Target},
        import::{ImportedSounds, import_files},
        library::Library,
        player::Player,
        prefs::Prefs,
        recording::{Selection, save_clip},
        shortcuts::Shortcuts,
    },
    task::{Task, Wake},
};

/// The playing sound, as shown to the user.
#[derive(Clone)]
pub struct NowPlaying {
    pub name: String,
    pub elapsed_secs: f64,
    /// Known, finite and positive when present.
    pub duration_secs: Option<f64>,
    pub repeat: bool,
}

/// Which kind of library edit a result belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Rename,
    BatchRename,
    Hotkeys,
    Move,
}

/// What the view should show after background work progressed.
pub enum Update {
    /// A previous playback or library error no longer applies.
    Cleared,
    Error(Message),
    /// Shown as a dialog with this title.
    FileError(Text, Message),
    Edited(Edit, anyhow::Result<()>),
    Removed(HashSet<SoundId>),
    /// A recorded clip was saved, or failed to save.
    Recorded(anyhow::Result<()>),
}

/// Library writers exclude each other, so they share one slot.
enum LibraryTask {
    Edit(Edit, Task<Library>),
    Import(Task<ImportedSounds>),
    Export(Task<()>),
    Remove(HashSet<SoundId>, Task<Library>),
    Record(Task<Sound>),
}

pub struct Soundboard {
    library: Library,
    prefs: Prefs,
    player: Player,
    shortcuts: Shortcuts,
    library_task: Option<LibraryTask>,
    shuffle: shuffle::Shuffle,
    wake: Wake,
}
impl Soundboard {
    /// Global shortcuts are delivered to `window`, which must belong to the calling thread.
    pub fn new(library: Library, prefs: Prefs, wake: Wake, window: HWND) -> Self {
        Self {
            library,
            prefs,
            player: Player::new(wake.clone()),
            shortcuts: Shortcuts::new(window, wake.clone()),
            wake,
            library_task: None,
            shuffle: shuffle::Shuffle::default(),
        }
    }
    pub fn prefs(&self) -> &Prefs {
        &self.prefs
    }
    pub fn configure(&mut self, prefs: Prefs) {
        let devices = |prefs: &Prefs| {
            [&prefs.microphone_device, &prefs.speaker_device]
                .map(|device| device.as_ref().map(|device| device.id.clone()))
        };
        if devices(&prefs) != devices(&self.prefs) {
            self.stop();
        }
        self.player.configure(&prefs);
        self.prefs = prefs;
    }
    pub fn library(&self) -> &Library {
        &self.library
    }
    pub fn shortcut_error(&self) -> &Message {
        self.shortcuts.error()
    }
    /// Registers the bound shortcuts, or none when disabled, and runs the pressed ones.
    /// Returns the result of the last playback a shortcut started.
    pub fn shortcuts(&mut self, enabled: bool) -> Option<anyhow::Result<()>> {
        let bindings = if enabled {
            self.prefs.keys.actions(&self.library.sounds)
        } else {
            HashMap::new()
        };
        let mut played = None;
        for action in self.shortcuts.poll(bindings) {
            match action {
                Action::Stop => self.stop(),
                Action::Random(key) => {
                    if let Some(index) = self.shuffle.choose(&self.library.sounds, key) {
                        played = Some(self.play(index));
                    }
                }
            }
        }
        played
    }
    /// Binds `key` to `target`. Playback keys are stored in `draft`, the open settings
    /// dialog; sound keys are validated against it when open and saved to the library.
    pub fn bind(
        &mut self,
        target: &Target,
        key: Hotkey,
        draft: Option<&mut Prefs>,
    ) -> anyhow::Result<()> {
        // The hold key is simulated rather than registered.
        if *target != Target::Hold {
            self.shortcuts.validate(key)?;
        }
        let sounds = &self.library.sounds;
        let Target::Sounds(ids) = target else {
            let draft = draft.user_context(Text::OpenSoundSettingsFirst)?;
            return draft.keys.bind(sounds, target, key);
        };
        let keys = draft.map_or(self.prefs.keys, |draft| draft.keys);
        keys.validate(sounds, target, key)?;
        let ids = ids.clone();
        self.edit(Edit::Hotkeys, move |library| {
            library.set_hotkeys(&ids, Some(key))
        })
    }
    /// Checks preferences against the library's keys and with Windows before saving.
    pub fn validate_prefs(&self, prefs: &Prefs) -> anyhow::Result<()> {
        [prefs.keys.stop, prefs.keys.random]
            .into_iter()
            .flatten()
            .try_for_each(|key| self.shortcuts.validate(key))?;
        prefs.keys.validate_all(&self.library.sounds)
    }
    /// Must run while the main window still exists.
    pub fn shutdown(&mut self) {
        self.shortcuts.shutdown();
        self.stop();
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
    pub fn saving_recording(&self) -> bool {
        matches!(self.library_task, Some(LibraryTask::Record(_)))
    }
    pub fn save_recording(&mut self, audio: Arc<Recording>, selection: Selection, name: String) {
        if self.library_busy() {
            return;
        }
        let task = Task::run("VoiceBoxSaveRecording", &self.wake, move || {
            save_clip(&audio, selection, &name)
        });
        self.library_task = Some(LibraryTask::Record(task));
    }
    /// Applies `edit` to a copy of the library and saves it in the background.
    pub fn edit(
        &mut self,
        kind: Edit,
        edit: impl FnOnce(&mut Library) -> anyhow::Result<()> + Send + 'static,
    ) -> anyhow::Result<()> {
        ensure!(!self.library_busy(), Text::RenameBusy);
        ensure!(
            !matches!(kind, Edit::Rename | Edit::BatchRename) || !self.playing(),
            Text::StopBeforeRename
        );
        let mut library = self.library.clone();
        let task = Task::run("VoiceBoxLibrarySave", &self.wake, move || {
            edit(&mut library)?;
            library.save()?;
            Ok(library)
        });
        self.library_task = Some(LibraryTask::Edit(kind, task));
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
        self.player.play(sound.id, sound.duration_secs, &self.prefs);
        Ok(())
    }
    pub fn stop(&mut self) {
        self.player.stop();
    }
    pub fn playback(&self) -> PlaybackStatus {
        self.player.status()
    }
    pub fn now_playing(&self) -> Option<NowPlaying> {
        let status = self.playback();
        let id = status.sound_id?;
        let sound = self.library.sounds.iter().find(|sound| sound.id == id)?;
        Some(NowPlaying {
            name: sound.name.clone(),
            elapsed_secs: status.elapsed_secs,
            duration_secs: status
                .duration_secs
                .or(Some(sound.duration_secs))
                .filter(|duration_secs| duration_secs.is_finite() && *duration_secs > 0.0),
            repeat: self.prefs.repeat,
        })
    }
    pub fn poll(&mut self) -> Vec<Update> {
        let mut updates = self.player.poll();
        updates.extend(self.poll_library());
        updates
    }
    fn poll_library(&mut self) -> Option<Update> {
        let update = match self.library_task.as_mut()? {
            LibraryTask::Edit(kind, task) => Update::Edited(
                *kind,
                task.poll()?.map(|library| {
                    self.library = library;
                }),
            ),
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
            LibraryTask::Record(task) => Update::Recorded(task.poll()?.map(|sound| {
                tracing::info!(sound_id = sound.id, "Recorded sound committed to library");
                self.library.sounds.push(sound);
            })),
        };
        self.library_task = None;
        Some(update)
    }
}

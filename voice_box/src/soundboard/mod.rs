//! Sound library operations and background playback coordination.

mod decoder;
pub mod export;
mod import;
pub mod library;
mod playback;
mod player;
pub mod recording;
pub mod rename;
mod shuffle;
pub mod sound;
mod storage;

use std::{collections::HashSet, path::PathBuf, sync::mpsc};

use anyhow::ensure;

pub use crate::soundboard::player::PlaybackStatus;
use crate::{
    hotkeys::Hotkey,
    i18n::{Details, Message, Text, UserContext},
    settings::{BindingTarget, Settings},
    soundboard::{
        import::{ImportedSounds, import_files},
        library::Library,
        player::Player,
        sound::SoundId,
    },
};

struct RemovalJob {
    ids: HashSet<SoundId>,
    receiver: mpsc::Receiver<anyhow::Result<Library>>,
}

#[derive(Default)]
pub struct Soundboard {
    library: Library,
    pub settings: Settings,
    pub message: Message,
    pub failed: bool,
    pub file_error: Option<(Text, Message)>,
    player: Player,
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
        Self {
            settings,
            library,
            ..Default::default()
        }
    }
    pub fn save_settings(&mut self, settings: Settings) -> anyhow::Result<()> {
        settings.validate(self.library.sounds.iter().filter_map(|sound| sound.hotkey))?;
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
        Ok(())
    }
    pub fn move_sounds(&mut self, ids: &HashSet<SoundId>, to_top: bool) -> anyhow::Result<()> {
        self.library.move_sounds(ids, to_top, Library::save)?;
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
                .validate_binding(
                    self.library.sounds.iter().filter_map(|sound| sound.hotkey),
                    target,
                    key,
                )?;
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
            self.settings.validate_binding(
                self.library.sounds.iter().filter_map(|sound| sound.hotkey),
                BindingTarget::Sound(index),
                key,
            )?;
        }
        self.library.set_hotkeys(ids, key, Library::save)
    }
    pub fn busy(&self) -> bool {
        self.playing() || self.importing() || self.deleting() || self.batch_renaming()
    }
    pub fn playing(&self) -> bool {
        self.player.playing()
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
    pub fn add_recording(&mut self, sound: sound::Sound) {
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
                self.file_error = Some((
                    Text::SoundImportFailedTitle,
                    Message::OpenPickerFailed {
                        error: Details::new(e),
                    },
                ));
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
            self.file_error = Some((
                Text::SoundImportFailedTitle,
                Message::Many(imported.errors.clone()),
            ));
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
        let stopped_playback = if self.playback().sound_id.is_some_and(|id| ids.contains(&id)) {
            self.player.stop_and_release()
        } else {
            None
        };
        let reclaim_space = !self.playing();
        self.player.cancel_queued();
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
                    thread.wait();
                }
                let result = crate::database::open()
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
        if self.settings.playback_speaker_output_gain_percent.is_zero()
            && self
                .settings
                .playback_microphone_output_gain_percent
                .is_zero()
        {
            self.set_error(Text::OutputsMuted.into());
            return;
        }
        self.shuffle.last = Some(sound.id);
        self.failed = false;
        self.message.clear();
        if let Err(error) =
            self.player
                .play(sound.id, sound.duration_secs, self.playback_config(), wake)
        {
            self.set_error(Message::StartPlaybackFailed {
                error: Details::new(error),
            });
        }
    }
    pub fn stop(&mut self) {
        self.player.stop();
    }
    pub fn update_playback_settings(&mut self) {
        self.player.configure(self.playback_config());
    }
    pub fn playback(&self) -> PlaybackStatus {
        self.player.status()
    }
    pub fn active_sound(&self) -> Option<&sound::Sound> {
        let id = self.playback().sound_id?;
        self.library.sounds.iter().find(|sound| sound.id == id)
    }
    fn playback_config(&self) -> playback::Config {
        playback::Config {
            microphone_device: self
                .settings
                .playback_microphone_output_device
                .as_ref()
                .map(|device| device.id.clone()),
            speaker_device: self
                .settings
                .playback_speaker_output_device
                .as_ref()
                .map(|device| device.id.clone()),
            microphone_gain_percent: self.settings.playback_microphone_output_gain_percent,
            speaker_gain_percent: self.settings.playback_speaker_output_gain_percent,
            include_microphone: self.settings.playback_include_microphone_audio,
            hold_hotkey: self.settings.playback_auto_hold_hotkey,
            repeat: self.settings.playback_repeat,
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
            let job = self
                .removal_job
                .take()
                .expect("UNREACHABLE: a deletion result requires a pending deletion job");
            match result {
                Ok(library) => {
                    self.library = library;
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
                            self.file_error = Some((
                                Text::SoundImportFailedTitle,
                                Message::ImportSoundsFailed {
                                    error: Details::new(e),
                                },
                            ));
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    tracing::error!("Sound import exited without a result");
                    self.import_job = None;
                    self.file_error = Some((
                        Text::SoundImportFailedTitle,
                        Text::ImportThreadExited.into(),
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        for update in self.player.poll(wake) {
            match update {
                player::Update::Started => self.message.clear(),
                player::Update::Restarted => {
                    self.failed = false;
                    self.message.clear();
                }
                player::Update::Error(error) => self.set_error(error),
            }
        }
    }
}

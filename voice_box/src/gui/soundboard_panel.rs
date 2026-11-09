use std::collections::HashSet;

use anyhow::ensure;

use crate::{
    devices::{self, Microphone, Speaker},
    gui::{
        batch_rename::{self, BatchRename},
        hotkey_binding::HotkeyManager,
        recording_import::RecordingImport,
        sound_list::{self, SoundList},
        sound_settings::{self, SoundSettings},
        wake_ui,
        widgets::{format_duration, notice},
    },
    hotkeys::Hotkey,
    i18n::{Details, Message, Text, UserContext},
    settings::{BindingTarget, Settings},
    soundboard::{Soundboard, Update, library::Library},
    task::{self, Task},
};

pub struct SoundboardPanel {
    model: Soundboard,
    settings: SoundSettings,
    settings_error: Message,
    devices: Option<Task<(Vec<Microphone>, Vec<Speaker>)>>,
    list: SoundList,
    batch_rename: Option<BatchRename>,
    recording_import: Option<RecordingImport>,
    error: Message,
    file_error: Option<(Text, Message)>,
}

impl SoundboardPanel {
    pub fn new(library: Library, settings: &Settings) -> Self {
        Self {
            model: Soundboard::new(library, settings.playback()),
            settings: SoundSettings::default(),
            settings_error: Message::default(),
            devices: None,
            list: SoundList::default(),
            batch_rename: None,
            recording_import: None,
            error: Message::default(),
            file_error: None,
        }
    }
    pub fn model(&self) -> &Soundboard {
        &self.model
    }
    pub fn stop(&mut self) {
        self.model.stop();
    }
    pub fn set_error(&mut self, error: Message) {
        self.error = error;
    }
    pub fn apply_settings(&mut self, settings: &Settings) {
        self.model.configure(settings.playback());
    }
    pub fn renaming(&self) -> bool {
        self.list.renaming() || self.batch_rename.is_some()
    }
    pub fn cancel_rename(&mut self) {
        self.list.cancel_rename();
    }
    pub fn cancel_delete(&mut self) {
        self.list.cancel_delete();
    }
    fn cancel_batch(&mut self) {
        if !self.model.library_busy() {
            self.list.cancel_batch();
            self.batch_rename = None;
        }
    }
    pub fn recording_open(&self) -> bool {
        self.recording_import.is_some()
    }
    pub fn notice_open(&self) -> bool {
        self.file_error.is_some()
    }
    /// Background work that must finish before other edits, exit or uninstall.
    pub fn busy(&self) -> bool {
        self.model.library_busy()
            || self.devices.is_some()
            || self
                .recording_import
                .as_ref()
                .is_some_and(|tool| tool.saving())
    }
    /// Called in app logic, including when the main viewport is hidden.
    pub fn poll(&mut self, ctx: &egui::Context) {
        for update in self.model.poll(wake_ui(ctx.clone())) {
            match update {
                Update::Cleared => self.error.clear(),
                Update::Error(error) => self.error = error,
                Update::FileError(title, error) => self.file_error = Some((title, error)),
                Update::Edited(result) => self.finish_edit(result),
                Update::Removed(ids) => {
                    self.list.remove_ids(&ids);
                    self.error.clear();
                }
            }
        }
        match task::finished(&mut self.devices) {
            Some(Ok((microphones, speakers))) => self.settings.set_devices(microphones, speakers),
            Some(Err(error)) => self.settings_error = Message::error(error),
            None => {}
        }
        if let Some(tool) = &mut self.recording_import
            && let Some(sound) = tool.poll(ctx)
        {
            self.model.add_recording(sound);
            self.error.clear();
            self.recording_import = None;
        }
    }
    pub fn recording_ui(&mut self, ctx: &egui::Context) {
        if let Some(tool) = &mut self.recording_import
            && tool.ui(ctx)
        {
            self.recording_import = None;
        }
    }

    fn edit_library(
        &mut self,
        ctx: &egui::Context,
        edit: impl FnOnce(&mut Library) -> anyhow::Result<()> + Send + 'static,
    ) -> anyhow::Result<()> {
        ensure!(!self.busy(), Text::RenameBusy);
        ensure!(
            !self.renaming() || !self.model.playing(),
            Text::StopBeforeRename
        );
        self.model.edit(edit, wake_ui(ctx.clone()));
        Ok(())
    }

    fn sound_hotkeys(&self) -> impl Iterator<Item = Option<Hotkey>> + Clone {
        self.model.library().sounds.iter().map(|sound| sound.hotkey)
    }

    pub fn play_sound(&mut self, ctx: &egui::Context, index: usize, hotkeys: &mut HotkeyManager) {
        hotkeys.clear_error();
        match self.model.play(index, wake_ui(ctx.clone())) {
            Ok(()) => self.error.clear(),
            Err(error) => self.error = Message::error(error),
        }
    }

    pub fn play_random_sound(
        &mut self,
        ctx: &egui::Context,
        hotkey: Option<Hotkey>,
        hotkeys: &mut HotkeyManager,
    ) {
        if let Some(index) = self.model.random_sound_index(hotkey) {
            self.play_sound(ctx, index, hotkeys);
        }
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        settings: &Settings,
        external_busy: bool,
        hotkeys: &mut HotkeyManager,
    ) {
        let available =
            !self.busy() && !external_busy && !self.settings.is_open() && !self.recording_open();
        ui.add_enabled_ui(available, |ui| {
            self.toolbar_ui(ui, settings, hotkeys);
            // The toolbar can start an operation during this same frame.
            if ui.is_enabled() && (self.busy() || self.recording_open()) {
                ui.disable();
            }
            let actions = self
                .list
                .ui(ui, &self.model.library().sounds, self.list_status(hotkeys));
            for action in actions {
                if self.busy() {
                    break;
                }
                self.list_action(ui.ctx(), hotkeys, action);
            }
            if self.busy() && ui.is_enabled() {
                ui.disable();
            }
            let batch_actions =
                self.list
                    .batch_ui(ui, &self.model.library().sounds, self.list_status(hotkeys));
            for action in batch_actions {
                if self.busy() {
                    break;
                }
                self.batch_action(ui.ctx(), hotkeys, action);
            }
            if self.busy() && ui.is_enabled() {
                ui.disable();
            }
            self.playback_ui(ui, settings, hotkeys);
        });
        let available = available && !self.busy();
        if let Some(ids) = self
            .list
            .delete_ui(ui.ctx(), &self.model.library().sounds, available)
        {
            self.model.remove(&ids, wake_ui(ui.ctx().clone()));
            self.error.clear();
        }
        self.batch_rename_ui(ui.ctx(), available);
    }

    fn list_status(&self, hotkeys: &HotkeyManager) -> sound_list::Status {
        sound_list::Status {
            active_sound: self.model.playback().sound_id,
            capture_target: hotkeys.target(),
            batch_capturing: hotkeys.batch_capturing(),
        }
    }

    fn batch_rename_ui(&mut self, ctx: &egui::Context, available: bool) {
        use batch_rename::{Action, Status};
        let Some(tool) = &mut self.batch_rename else {
            return;
        };
        let status = Status {
            available,
            busy: self.model.busy() || self.devices.is_some(),
            playing: self.model.playing(),
        };
        match tool.ui(ctx, self.model.library(), status) {
            Action::None => {}
            Action::Closed => {
                self.list.clear_anchor();
                self.batch_rename = None;
            }
            Action::StopPlayback => self.model.stop(),
            Action::Apply(plan) => {
                let result = self.edit_library(ctx, move |library| library.rename_batch(plan));
                if let Some(tool) = &mut self.batch_rename {
                    tool.apply_result(result);
                }
            }
        }
    }

    fn toolbar_ui(&mut self, ui: &mut egui::Ui, settings: &Settings, hotkeys: &mut HotkeyManager) {
        ui.horizontal(|ui| {
            ui.strong(
                Message::SoundCount {
                    count: self.model.library().sounds.len(),
                }
                .text(),
            );
            if self.list.batch_mode() {
                if ui.button(Text::Done.text()).clicked() {
                    hotkeys.cancel_capture();
                    self.cancel_batch();
                }
            } else if ui
                .add_enabled(
                    !self.model.library().sounds.is_empty() && !self.renaming(),
                    egui::Button::new(Text::BatchManage.text()),
                )
                .clicked()
            {
                self.cancel_delete();
                self.list.begin_batch();
            }
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center).with_main_wrap(true),
                |ui| {
                    if !self.list.batch_mode() {
                        if ui.button(Text::Settings.text()).clicked() {
                            self.settings.open(settings);
                            self.settings_error.clear();
                            self.devices = Some(Task::run(
                                "VoiceBoxOutputDevices",
                                wake_ui(ui.ctx().clone()),
                                devices::enumerate_soundboard_devices,
                            ));
                        }
                        // This row is laid out right-to-left: recording sits to the right of import.
                        if ui.button(Text::RecordImport.text()).clicked() {
                            self.model.stop();
                            self.cancel_rename();
                            self.cancel_delete();
                            hotkeys.cancel_capture();
                            self.recording_import = Some(RecordingImport::default());
                        }
                        if ui.button(Text::Import.text()).clicked() {
                            self.model
                                .import(super::picker::choose_files, wake_ui(ui.ctx().clone()));
                        }
                    }
                    if self.busy() {
                        ui.spinner();
                    }
                },
            );
        });
    }

    fn list_action(
        &mut self,
        ctx: &egui::Context,
        hotkeys: &mut HotkeyManager,
        action: sound_list::Action,
    ) {
        use sound_list::Action;
        let result = match action {
            Action::Play(index) => return self.play_sound(ctx, index, hotkeys),
            Action::Stop => return self.model.stop(),
            Action::Remove(id) => {
                return self
                    .model
                    .remove(&HashSet::from([id]), wake_ui(ctx.clone()));
            }
            Action::Capture(target) => {
                self.model.stop();
                return hotkeys.begin_capture(target, None);
            }
            Action::ClearHotkey(index) => {
                let id = self.model.library().sounds[index].id;
                self.edit_library(ctx, move |library| {
                    library.set_hotkeys(&HashSet::from([id]), None)
                })
            }
            Action::Move { from, to } => {
                self.edit_library(ctx, move |library| library.move_sound(from, to))
            }
        };
        if let Err(error) = result {
            tracing::error!("Failed to edit the sound library: {error:#}");
            self.error = Message::error(error);
        }
    }

    fn playback_ui(&mut self, ui: &mut egui::Ui, settings: &Settings, hotkeys: &HotkeyManager) {
        ui.horizontal(|ui| {
            let key = settings
                .playback_stop_hotkey
                .map(|key| Message::StopPlaybackHotkey { key }.text().into_owned())
                .unwrap_or_else(|| Text::StopPlayback.text().into());
            if ui
                .add_enabled(self.model.playing(), egui::Button::new(key))
                .clicked()
            {
                self.model.stop();
            }
            let error = [hotkeys.error(), &self.error]
                .into_iter()
                .find(|error| !error.is_empty());
            if let Some(error) = error {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(error.text()).color(ui.visuals().error_fg_color),
                    )
                    .truncate()
                    .show_tooltip_when_elided(false),
                )
                .on_hover_text(error.text());
            } else if let Some(active) = self.model.active_sound() {
                let name = active.name.as_str();
                let duration_text = self
                    .model
                    .playback()
                    .duration_secs
                    .or(Some(active.duration_secs))
                    .filter(|duration_secs| duration_secs.is_finite() && *duration_secs > 0.0)
                    .map(format_duration)
                    .unwrap_or_else(|| "…".into());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!(
                        "{} / {duration_text}",
                        format_duration(self.model.playback().elapsed_secs)
                    ));
                    ui.add(
                        egui::Label::new(name)
                            .truncate()
                            .show_tooltip_when_elided(false),
                    )
                    .on_hover_text(name);
                });
            }
        });
    }

    fn batch_action(
        &mut self,
        ctx: &egui::Context,
        hotkeys: &mut HotkeyManager,
        action: sound_list::BatchAction,
    ) {
        use sound_list::BatchAction;
        let Some(ids) = self.list.selected().cloned() else {
            return;
        };
        match action {
            BatchAction::Export => {
                self.model
                    .export(&ids, super::picker::choose_folder, wake_ui(ctx.clone()));
                self.error.clear();
            }
            BatchAction::Move(to_top) => {
                match self.edit_library(ctx, move |library| library.move_sounds(&ids, to_top)) {
                    Ok(()) => self.error.clear(),
                    Err(error) => self.error = Message::error(error),
                }
            }
            BatchAction::Rename => {
                self.batch_rename = Some(BatchRename::new(self.model.library(), ids))
            }
            BatchAction::BindHotkey => {
                if let Some(index) = self
                    .model
                    .library()
                    .sounds
                    .iter()
                    .position(|sound| ids.contains(&sound.id))
                {
                    self.model.stop();
                    hotkeys.begin_capture(BindingTarget::Sound(index), Some(ids));
                }
                ctx.request_repaint();
            }
            BatchAction::ClearHotkey => {
                match self.edit_library(ctx, move |library| library.set_hotkeys(&ids, None)) {
                    Ok(()) => hotkeys.message.clear(),
                    Err(error) => {
                        tracing::error!("Failed to clear selected hotkeys: {error:#}");
                        hotkeys.message = Message::error(error);
                    }
                }
            }
        }
    }

    pub fn file_error_ui(&mut self, ctx: &egui::Context) {
        if let Some((title, error)) = &self.file_error
            && notice(ctx, *title, error)
        {
            self.file_error = None;
        }
    }

    /// Returns a validated draft for the caller to save.
    pub fn settings_ui(
        &mut self,
        ctx: &egui::Context,
        settings: &Settings,
        saving: bool,
        hotkeys: &mut HotkeyManager,
    ) -> Option<Settings> {
        use sound_settings::{Action, Status};
        let status = Status {
            busy: self.busy() || saving || hotkeys.is_capturing(),
            capture_target: hotkeys.target(),
            capture_message: &hotkeys.settings_message,
            error: (!self.settings_error.is_empty()).then_some(&self.settings_error),
        };
        match self.settings.ui(ctx, settings, status) {
            Action::None => {}
            Action::Closed => {
                hotkeys.cancel_capture();
                hotkeys.settings_message.clear();
            }
            Action::ClearMessage => hotkeys.settings_message.clear(),
            Action::Save(draft) => {
                let result = [draft.playback_stop_hotkey, draft.playback_random_hotkey]
                    .into_iter()
                    .flatten()
                    .try_for_each(|key| hotkeys.validate(key))
                    .and_then(|()| draft.validate(self.sound_hotkeys()));
                match result {
                    Ok(()) => {
                        self.settings_error.clear();
                        return Some(draft);
                    }
                    Err(error) => {
                        tracing::error!("Failed to save soundboard settings: {error:#}");
                        self.settings_error = Message::OperationFailed {
                            error: Details::new(error),
                        };
                    }
                }
            }
            Action::Capture(target) => {
                self.model.stop();
                hotkeys.begin_capture(target, None);
                ctx.request_repaint();
            }
        }
        None
    }

    pub fn poll_capture(
        &mut self,
        ctx: &egui::Context,
        settings: &Settings,
        hotkeys: &mut HotkeyManager,
    ) {
        let Some(capture) = hotkeys.poll_capture(ctx) else {
            return;
        };
        let sound_hotkeys = self.model.library().sounds.iter().map(|sound| sound.hotkey);
        let result = self
            .settings
            .draft_mut()
            .map_or(settings, |draft| &*draft)
            .validate_binding(sound_hotkeys, capture.target, capture.key)
            .and_then(|()| match capture.target {
                BindingTarget::Sound(index) => {
                    let ids = match capture.ids {
                        Some(ids) => ids,
                        None => HashSet::from([self
                            .model
                            .library()
                            .sounds
                            .get(index)
                            .user_context(Text::SoundNotFound)?
                            .id]),
                    };
                    self.edit_library(ctx, move |library| {
                        library.set_hotkeys(&ids, Some(capture.key))
                    })
                }
                target => {
                    let draft = self
                        .settings
                        .draft_mut()
                        .user_context(Text::OpenSoundSettingsFirst)?;
                    *match target {
                        BindingTarget::Hold => &mut draft.playback_auto_hold_hotkey,
                        BindingTarget::Stop => &mut draft.playback_stop_hotkey,
                        BindingTarget::Random => &mut draft.playback_random_hotkey,
                        BindingTarget::Sound(_) => unreachable!(),
                    } = Some(capture.key);
                    Ok(())
                }
            });
        hotkeys.capture_result(capture.target, result);
    }

    pub fn finish_rename(&mut self, ctx: &egui::Context, soundboard_tab_selected: bool) {
        let keep_editing = soundboard_tab_selected && !self.settings.is_open();
        if let Some((index, name)) = self.list.rename_request(keep_editing, self.model.playing())
            && let Err(error) = self.edit_library(ctx, move |library| library.rename(index, &name))
        {
            self.list.rename_finished(false);
            self.error = Message::error(error);
        }
    }

    fn finish_edit(&mut self, result: anyhow::Result<()>) {
        let result = result.inspect(|()| self.error.clear()).map_err(|error| {
            self.error = Message::error(error);
            anyhow::anyhow!(self.error.clone())
        });
        // Editing blocks other library changes, so the open editor owns the result.
        if self.list.renaming() {
            self.list.rename_finished(result.is_ok());
        }
        if let Some(tool) = &mut self.batch_rename
            && tool.applying()
        {
            let closed = tool.complete(self.model.library(), result);
            if let Some(selected) = self.list.selected_mut() {
                *selected = tool.selected().clone();
            }
            if closed {
                self.list.clear_anchor();
                self.batch_rename = None;
            }
        }
    }
}

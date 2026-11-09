use std::collections::HashSet;

use anyhow::ensure;

use crate::{
    devices::{self, Microphone, Speaker},
    gui::{
        batch_rename::{self, BatchRename},
        hotkey_binding::HotkeyManager,
        overlay::Overlay,
        recording_import::RecordingImport,
        sound_list::{self, SoundList},
        sound_settings::{self, SoundSettings},
        wake_ui,
        widgets::{format_duration, notice},
    },
    i18n::{Details, Message, Text, UserContext},
    model::Hotkey,
    soundboard::{
        Soundboard, Update,
        bindings::{Action, Target},
        library::Library,
        prefs::Prefs,
    },
    task::{self, Task},
};

pub struct SoundboardPanel {
    model: Soundboard,
    prefs: Prefs,
    hotkeys: HotkeyManager,
    settings: SoundSettings,
    settings_error: Message,
    /// Binding feedback for the playback keys in the settings dialog.
    capture_message: Message,
    devices: Option<Task<(Vec<Microphone>, Vec<Speaker>)>>,
    list: SoundList,
    batch_rename: Option<BatchRename>,
    recording_import: Option<RecordingImport>,
    error: Message,
    file_error: Option<(Text, Message)>,
    overlay: Overlay,
    overlay_visible: bool,
}

impl SoundboardPanel {
    pub fn new(ctx: &egui::Context, library: Library, prefs: &Prefs) -> Self {
        Self {
            model: Soundboard::new(library, prefs),
            prefs: prefs.clone(),
            hotkeys: HotkeyManager::new(ctx),
            settings: SoundSettings::default(),
            settings_error: Message::default(),
            capture_message: Message::default(),
            devices: None,
            list: SoundList::default(),
            batch_rename: None,
            recording_import: None,
            error: Message::default(),
            file_error: None,
            overlay: Overlay::default(),
            overlay_visible: false,
        }
    }
    pub fn apply_prefs(&mut self, prefs: &Prefs) {
        self.model.configure(prefs);
        self.prefs = prefs.clone();
    }
    pub fn playing(&self) -> bool {
        self.model.playing()
    }
    pub fn capturing(&self) -> bool {
        self.hotkeys.target().is_some()
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
    /// Playback, library work or recording that needs frequent repaints.
    pub fn active(&self) -> bool {
        self.model.busy() || self.recording_open()
    }
    pub fn shutdown(&mut self) {
        // Unregister while the main window still exists.
        self.hotkeys.shutdown();
        self.model.stop();
    }
    fn renaming(&self) -> bool {
        self.list.renaming() || self.batch_rename.is_some()
    }
    fn recording_open(&self) -> bool {
        self.recording_import.is_some()
    }
    fn cancel_batch(&mut self) {
        if !self.model.library_busy() {
            self.list.cancel_batch();
            self.batch_rename = None;
        }
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

    /// Registers and dispatches global shortcuts; `enabled` is false while the rest
    /// of the app cannot accept playback.
    pub fn shortcuts(&mut self, ctx: &egui::Context, enabled: bool) {
        let focused = ctx.input(|input| {
            input.viewport().focused == Some(true) && input.viewport().minimized != Some(true)
        });
        if !focused {
            self.hotkeys.cancel_capture();
        }
        let enabled = enabled
            && self.file_error.is_none()
            && !self.recording_open()
            && !(focused && (self.renaming() || ctx.text_edit_focused()));
        let bindings = self.prefs.keys.actions(&self.model.library().sounds);
        for action in self.hotkeys.poll(enabled, bindings) {
            match action {
                Action::Stop => self.model.stop(),
                Action::Random(key) => self.play_random_sound(ctx, key),
            }
        }
        // Bootstrap the overlay while minimized; visible children keep UI passes running.
        if self.prefs.show_overlay
            && self.model.playing()
            && !self.overlay_visible
            && ctx.input(|input| input.viewport().visible() == Some(false))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
        }
    }

    /// Input that must be handled before this frame's widgets, while interaction is allowed.
    pub fn prepare(&mut self, ctx: &egui::Context, visible: bool) {
        self.apply_capture(ctx);
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.list.cancel_rename();
        }
        if !visible
            || ctx.input(|input| {
                !input.focused
                    || input.smooth_scroll_delta != egui::Vec2::ZERO
                    || input.pointer.is_decidedly_dragging()
                    || input.key_pressed(egui::Key::Escape)
            })
        {
            self.list.cancel_delete();
        }
    }

    /// Dialogs; returns validated preferences for the caller to save.
    pub fn windows(
        &mut self,
        ctx: &egui::Context,
        blocked: bool,
        saving: bool,
        visible: bool,
    ) -> Option<Prefs> {
        let prefs = self.settings_ui(ctx, saving);
        if let Some(tool) = &mut self.recording_import
            && tool.ui(ctx)
        {
            self.recording_import = None;
        }
        if !blocked && !self.recording_open() {
            self.finish_rename(ctx, visible);
        }
        if let Some((title, error)) = &self.file_error
            && notice(ctx, *title, error)
        {
            self.file_error = None;
        }
        prefs
    }

    /// Must run every frame once shown, so the overlay viewport keeps existing.
    pub fn overlay_ui(&mut self, ctx: &egui::Context, allowed: bool) {
        let enabled = allowed && self.prefs.show_overlay;
        self.overlay_visible = match self
            .overlay
            .show(ctx, &self.model, self.prefs.repeat, enabled)
        {
            Ok(visible) => visible,
            Err(error) => {
                tracing::warn!("Failed to show playback overlay: {error:#}");
                self.error = Message::ShowOverlayFailed {
                    error: Details::new(error),
                };
                false
            }
        };
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

    fn play_sound(&mut self, ctx: &egui::Context, index: usize) {
        match self.model.play(index, wake_ui(ctx.clone())) {
            Ok(()) => self.error.clear(),
            Err(error) => self.error = Message::error(error),
        }
    }

    fn play_random_sound(&mut self, ctx: &egui::Context, hotkey: Option<Hotkey>) {
        if let Some(index) = self.model.random_sound_index(hotkey) {
            self.play_sound(ctx, index);
        }
    }

    fn message_for(&mut self, target: &Target) -> &mut Message {
        if matches!(target, Target::Sounds(_)) {
            &mut self.error
        } else {
            &mut self.capture_message
        }
    }

    fn begin_capture(&mut self, target: Target) {
        self.model.stop();
        self.message_for(&target).clear();
        self.hotkeys.begin_capture(target);
    }

    fn apply_capture(&mut self, ctx: &egui::Context) {
        let Some((target, key)) = self.hotkeys.poll_capture(ctx) else {
            return;
        };
        let result = key.and_then(|key| {
            let keys = self
                .settings
                .draft_mut()
                .map_or(self.prefs.keys, |draft| draft.keys);
            keys.validate(&self.model.library().sounds, &target, key)?;
            if let Target::Sounds(ids) = &target {
                let ids = ids.clone();
                return self.edit_library(ctx, move |library| library.set_hotkeys(&ids, Some(key)));
            }
            let draft = self
                .settings
                .draft_mut()
                .user_context(Text::OpenSoundSettingsFirst)?;
            if let Some(slot) = draft.keys.slot(&target) {
                *slot = Some(key);
            }
            Ok(())
        });
        let message = self.message_for(&target);
        match result {
            Ok(()) => message.clear(),
            Err(error) => {
                tracing::error!("Failed to bind hotkey: {error:#}");
                *message = Message::error(error);
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, external_busy: bool) {
        let available =
            !self.busy() && !external_busy && !self.settings.is_open() && !self.recording_open();
        ui.add_enabled_ui(available, |ui| {
            self.toolbar_ui(ui);
            // The toolbar can start an operation during this same frame.
            if ui.is_enabled() && (self.busy() || self.recording_open()) {
                ui.disable();
            }
            let actions = self
                .list
                .ui(ui, &self.model.library().sounds, self.list_status());
            for action in actions {
                if self.busy() {
                    break;
                }
                self.list_action(ui.ctx(), action);
            }
            if self.busy() && ui.is_enabled() {
                ui.disable();
            }
            let batch_actions =
                self.list
                    .batch_ui(ui, &self.model.library().sounds, self.list_status());
            for action in batch_actions {
                if self.busy() {
                    break;
                }
                self.batch_action(ui.ctx(), action);
            }
            if self.busy() && ui.is_enabled() {
                ui.disable();
            }
            self.playback_ui(ui);
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

    fn list_status(&self) -> sound_list::Status {
        let capturing = match self.hotkeys.target() {
            Some(Target::Sounds(ids)) => Some(ids),
            _ => None,
        };
        sound_list::Status {
            active_sound: self.model.playback().sound_id,
            capture: capturing
                .filter(|ids| ids.len() == 1)
                .and_then(|ids| ids.iter().next().copied()),
            batch_capturing: capturing.is_some(),
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

    fn toolbar_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong(
                Message::SoundCount {
                    count: self.model.library().sounds.len(),
                }
                .text(),
            );
            if self.list.batch_mode() {
                if ui.button(Text::Done.text()).clicked() {
                    self.hotkeys.cancel_capture();
                    self.cancel_batch();
                }
            } else if ui
                .add_enabled(
                    !self.model.library().sounds.is_empty() && !self.renaming(),
                    egui::Button::new(Text::BatchManage.text()),
                )
                .clicked()
            {
                self.list.cancel_delete();
                self.list.begin_batch();
            }
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center).with_main_wrap(true),
                |ui| {
                    if !self.list.batch_mode() {
                        if ui.button(Text::Settings.text()).clicked() {
                            self.settings.open(&self.prefs);
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
                            self.list.cancel_rename();
                            self.list.cancel_delete();
                            self.hotkeys.cancel_capture();
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

    fn list_action(&mut self, ctx: &egui::Context, action: sound_list::Action) {
        use sound_list::Action;
        let result = match action {
            Action::Play(index) => return self.play_sound(ctx, index),
            Action::Stop => return self.model.stop(),
            Action::Remove(id) => {
                return self
                    .model
                    .remove(&HashSet::from([id]), wake_ui(ctx.clone()));
            }
            Action::Capture(id) => return self.begin_capture(Target::Sounds(HashSet::from([id]))),
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

    fn playback_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let key = self
                .prefs
                .keys
                .stop
                .map(|key| Message::StopPlaybackHotkey { key }.text().into_owned())
                .unwrap_or_else(|| Text::StopPlayback.text().into());
            if ui
                .add_enabled(self.model.playing(), egui::Button::new(key))
                .clicked()
            {
                self.model.stop();
            }
            let error = [&self.error, self.hotkeys.error()]
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

    fn batch_action(&mut self, ctx: &egui::Context, action: sound_list::BatchAction) {
        use sound_list::BatchAction;
        let Some(ids) = self.list.selected().cloned() else {
            return;
        };
        let result = match action {
            BatchAction::Export => {
                self.model
                    .export(&ids, super::picker::choose_folder, wake_ui(ctx.clone()));
                Ok(())
            }
            BatchAction::Move(to_top) => {
                self.edit_library(ctx, move |library| library.move_sounds(&ids, to_top))
            }
            BatchAction::Rename => {
                self.batch_rename = Some(BatchRename::new(self.model.library(), ids));
                return;
            }
            BatchAction::BindHotkey => {
                if !ids.is_empty() {
                    self.begin_capture(Target::Sounds(ids));
                }
                ctx.request_repaint();
                return;
            }
            BatchAction::ClearHotkey => {
                self.edit_library(ctx, move |library| library.set_hotkeys(&ids, None))
            }
        };
        match result {
            Ok(()) => self.error.clear(),
            Err(error) => {
                tracing::error!("Failed to edit the selected sounds: {error:#}");
                self.error = Message::error(error);
            }
        }
    }

    fn settings_ui(&mut self, ctx: &egui::Context, saving: bool) -> Option<Prefs> {
        use sound_settings::{Action, Status};
        let status = Status {
            busy: self.busy() || saving || self.capturing(),
            capture_target: self.hotkeys.target(),
            capture_message: &self.capture_message,
            error: (!self.settings_error.is_empty()).then_some(&self.settings_error),
        };
        match self.settings.ui(ctx, &self.prefs, status) {
            Action::None => {}
            Action::Closed => {
                self.hotkeys.cancel_capture();
                self.capture_message.clear();
            }
            Action::ClearMessage => self.capture_message.clear(),
            Action::Save(draft) => {
                let result = [draft.keys.stop, draft.keys.random]
                    .into_iter()
                    .flatten()
                    .try_for_each(|key| self.hotkeys.validate(key))
                    .and_then(|()| draft.keys.validate_all(&self.model.library().sounds));
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
                self.begin_capture(target);
                ctx.request_repaint();
            }
        }
        None
    }

    fn finish_rename(&mut self, ctx: &egui::Context, visible: bool) {
        let keep_editing = visible && !self.settings.is_open();
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

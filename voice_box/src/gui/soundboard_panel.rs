use std::collections::HashSet;

use anyhow::ensure;
use windows::Win32::Foundation::HWND;

use crate::{
    devices::{self, Microphone, Speaker},
    gui::{
        Busy,
        batch_rename::{self, BatchRename},
        hotkey_binding::HotkeyManager,
        overlay::Overlay,
        recording_import::RecordingImport,
        sound_list::{self, SoundList},
        sound_settings::{self, SoundSettings},
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
    task::{self, Task, Wake},
};

pub struct SoundboardPanel {
    wake: Wake,
    model: Soundboard,
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
    pub fn new(wake: Wake, window: HWND, library: Library, prefs: Prefs) -> Self {
        Self {
            model: Soundboard::new(library, prefs, wake.clone()),
            hotkeys: HotkeyManager::new(window, wake.clone()),
            wake,
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
    pub fn apply_prefs(&mut self, prefs: Prefs) {
        self.model.configure(prefs);
    }
    pub fn capturing(&self) -> bool {
        self.hotkeys.target().is_some()
    }
    /// Background work that must finish before other edits, exit or uninstall.
    fn working(&self) -> bool {
        self.model.library_busy()
            || self.devices.is_some()
            || self
                .recording_import
                .as_ref()
                .is_some_and(|tool| tool.saving())
    }
    pub fn busy(&self) -> Busy {
        Busy {
            working: self.working(),
            audio: self.model.playing() || self.recording_open(),
        }
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
        for update in self.model.poll() {
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
            && !self.working()
            && self.file_error.is_none()
            && !self.recording_open()
            && !(focused && (self.renaming() || ctx.text_edit_focused()));
        let bindings = self
            .model
            .prefs()
            .keys
            .actions(&self.model.library().sounds);
        for action in self.hotkeys.poll(enabled, bindings) {
            match action {
                Action::Stop => self.model.stop(),
                Action::Random(key) => self.play_random_sound(key),
            }
        }
        // Bootstrap the overlay while minimized; visible children keep UI passes running.
        if self.model.prefs().show_overlay
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
    pub fn windows(&mut self, ctx: &egui::Context, blocked: bool, visible: bool) -> Option<Prefs> {
        let prefs = self.settings_ui(ctx, blocked);
        if let Some(tool) = &mut self.recording_import
            && tool.ui(ctx)
        {
            self.recording_import = None;
        }
        if !blocked && !self.recording_open() {
            self.finish_rename(visible);
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
        let enabled = allowed && self.model.prefs().show_overlay;
        self.overlay_visible = match self.overlay.show(ctx, &self.model, enabled) {
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
        edit: impl FnOnce(&mut Library) -> anyhow::Result<()> + Send + 'static,
    ) -> anyhow::Result<()> {
        ensure!(!self.working(), Text::RenameBusy);
        ensure!(
            !self.renaming() || !self.model.playing(),
            Text::StopBeforeRename
        );
        self.model.edit(edit)
    }

    fn play_sound(&mut self, index: usize) {
        match self.model.play(index) {
            Ok(()) => self.error.clear(),
            Err(error) => self.error = Message::error(error),
        }
    }

    fn play_random_sound(&mut self, hotkey: Option<Hotkey>) {
        if let Some(index) = self.model.random_sound_index(hotkey) {
            self.play_sound(index);
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
            let sounds = &self.model.library().sounds;
            let Target::Sounds(ids) = &target else {
                let draft = self
                    .settings
                    .draft_mut()
                    .user_context(Text::OpenSoundSettingsFirst)?;
                return draft.keys.bind(sounds, &target, key);
            };
            let keys = self
                .settings
                .draft_mut()
                .map_or(self.model.prefs().keys, |draft| draft.keys);
            keys.validate(sounds, &target, key)?;
            let ids = ids.clone();
            self.edit_library(move |library| library.set_hotkeys(&ids, Some(key)))
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

    pub fn ui(&mut self, ui: &mut egui::Ui, others: Busy) {
        let available =
            !self.working() && !others.any() && !self.settings.is_open() && !self.recording_open();
        ui.add_enabled_ui(available, |ui| {
            self.toolbar_ui(ui);
            // The toolbar can start an operation during this same frame.
            if ui.is_enabled() && (self.working() || self.recording_open()) {
                ui.disable();
            }
            let actions = self
                .list
                .ui(ui, &self.model.library().sounds, self.list_status());
            for action in actions {
                if self.working() {
                    break;
                }
                self.list_action(action);
            }
            if self.working() && ui.is_enabled() {
                ui.disable();
            }
            let batch_actions =
                self.list
                    .batch_ui(ui, &self.model.library().sounds, self.list_status());
            for action in batch_actions {
                if self.working() {
                    break;
                }
                self.batch_action(ui.ctx(), action);
            }
            if self.working() && ui.is_enabled() {
                ui.disable();
            }
            self.playback_ui(ui);
        });
        let available = available && !self.working();
        if let Some(ids) = self
            .list
            .delete_ui(ui.ctx(), &self.model.library().sounds, available)
        {
            self.model.remove(&ids);
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
                let result = self.edit_library(move |library| library.rename_batch(plan));
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
                            self.settings.open(self.model.prefs());
                            self.settings_error.clear();
                            self.devices = Some(Task::run(
                                "VoiceBoxOutputDevices",
                                &self.wake,
                                devices::enumerate_soundboard_devices,
                            ));
                        }
                        // This row is laid out right-to-left: recording sits to the right of import.
                        if ui.button(Text::RecordImport.text()).clicked() {
                            self.model.stop();
                            self.list.cancel_rename();
                            self.list.cancel_delete();
                            self.hotkeys.cancel_capture();
                            self.recording_import = Some(RecordingImport::new(self.wake.clone()));
                        }
                        if ui.button(Text::Import.text()).clicked() {
                            self.model.import(super::picker::choose_files);
                        }
                    }
                    if self.working() {
                        ui.spinner();
                    }
                },
            );
        });
    }

    fn list_action(&mut self, action: sound_list::Action) {
        use sound_list::Action;
        let result = match action {
            Action::Play(index) => return self.play_sound(index),
            Action::Stop => return self.model.stop(),
            Action::Remove(id) => {
                return self.model.remove(&HashSet::from([id]));
            }
            Action::Capture(id) => return self.begin_capture(Target::Sounds(HashSet::from([id]))),
            Action::ClearHotkey(index) => {
                let id = self.model.library().sounds[index].id;
                self.edit_library(move |library| library.set_hotkeys(&HashSet::from([id]), None))
            }
            Action::Move { from, to } => {
                self.edit_library(move |library| library.move_sound(from, to))
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
                .model
                .prefs()
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
                self.model.export(&ids, super::picker::choose_folder);
                Ok(())
            }
            BatchAction::Move(to_top) => {
                self.edit_library(move |library| library.move_sounds(&ids, to_top))
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
                self.edit_library(move |library| library.set_hotkeys(&ids, None))
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

    fn settings_ui(&mut self, ctx: &egui::Context, blocked: bool) -> Option<Prefs> {
        use sound_settings::{Action, Status};
        let status = Status {
            busy: blocked || self.capturing(),
            capture_target: self.hotkeys.target(),
            capture_message: &self.capture_message,
            error: (!self.settings_error.is_empty()).then_some(&self.settings_error),
        };
        match self.settings.ui(ctx, self.model.prefs(), status) {
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

    fn finish_rename(&mut self, visible: bool) {
        let keep_editing = visible && !self.settings.is_open();
        if let Some((index, name)) = self.list.rename_request(keep_editing, self.model.playing())
            && let Err(error) = self.edit_library(move |library| library.rename(index, &name))
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

use std::{collections::HashSet, sync::Arc};

use windows::Win32::Foundation::HWND;

use crate::{
    audio::Recording,
    devices::{self, Microphone, Speaker},
    gui::{
        Busy,
        batch_rename_dialog::{self, BatchRenameDialog},
        key_capture::KeyCapture,
        overlay::Overlay,
        recording_import_dialog::{self, RecordingImportDialog},
        sound_list::{self, SoundList},
        soundboard_settings_dialog::{self, SoundboardSettingsDialog},
        widgets::{format_duration, notice},
    },
    i18n::{Details, Message, Text},
    model::SoundId,
    soundboard::{
        Edit, Soundboard, Update, bindings::Target, library::Library, recording::Selection,
        rename::RenamePlan, settings::SoundboardSettings,
    },
    task::{self, Task, Wake},
};

/// An operation clicked while drawing; `apply` starts it once the frame is drawn.
/// Unlike the voice and microphone panels, this panel queues its own commands because
/// they come from both the tab (`ui`) and its dialogs (`windows`), drawn at different
/// points in the frame.
enum Command {
    List(sound_list::Action),
    Batch(sound_list::BatchAction),
    Capture(Target),
    Remove(HashSet<SoundId>),
    Rename(usize, String),
    BatchRename(RenamePlan),
    SaveRecording(Arc<Recording>, Selection, String),
    OpenSettings,
    RecordImport,
    Import,
}

pub struct SoundboardPanel {
    wake: Wake,
    model: Soundboard,
    capture: KeyCapture,
    settings_dialog: SoundboardSettingsDialog,
    settings_error: Message,
    /// Binding feedback for the playback keys in the settings dialog.
    capture_message: Message,
    devices: Option<Task<(Vec<Microphone>, Vec<Speaker>)>>,
    list: SoundList,
    batch_rename: Option<BatchRenameDialog>,
    recording_import: Option<RecordingImportDialog>,
    error: Message,
    file_error: Option<(Text, Message)>,
    overlay: Overlay,
    overlay_visible: bool,
    commands: Vec<Command>,
}

impl SoundboardPanel {
    pub fn new(wake: Wake, window: HWND, library: Library, settings: SoundboardSettings) -> Self {
        Self {
            model: Soundboard::new(library, settings, wake.clone(), window),
            capture: KeyCapture::default(),
            wake,
            settings_dialog: SoundboardSettingsDialog::default(),
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
            commands: Vec::new(),
        }
    }
    pub fn apply_settings(&mut self, settings: SoundboardSettings) {
        self.model.configure(settings);
    }
    pub fn capturing(&self) -> bool {
        self.capture.target().is_some()
    }
    /// Background work that must finish before other edits, exit or uninstall.
    fn working(&self) -> bool {
        self.model.library_busy() || self.devices.is_some()
    }
    pub fn busy(&self) -> Busy {
        Busy {
            working: self.working(),
            audio: self.model.playing() || self.recording_open(),
        }
    }
    pub fn shutdown(&mut self) {
        self.model.shutdown();
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
                Update::Edited(kind, result) => self.finish_edit(kind, result),
                Update::Removed(ids) => {
                    self.list.remove_ids(&ids);
                    self.error.clear();
                }
                Update::Recorded(Ok(())) => {
                    self.recording_import = None;
                    self.error.clear();
                }
                Update::Recorded(Err(error)) => {
                    if let Some(tool) = &mut self.recording_import {
                        tool.fail(error);
                    }
                }
            }
        }
        match task::finished(&mut self.devices) {
            Some(Ok((microphones, speakers))) => {
                self.settings_dialog.set_devices(microphones, speakers)
            }
            Some(Err(error)) => self.settings_error = Message::error(error),
            None => {}
        }
        if let Some(tool) = &mut self.recording_import {
            tool.poll(ctx);
        }
    }

    /// Registers and dispatches global shortcuts; `enabled` is false while the rest
    /// of the app cannot accept playback. The app decides from cross-panel state and
    /// this panel adds its own (capture, renaming, dialogs), so neither needs the other's.
    pub fn shortcuts(&mut self, ctx: &egui::Context, enabled: bool) {
        let focused = ctx.input(|input| {
            input.viewport().focused == Some(true) && input.viewport().minimized != Some(true)
        });
        if !focused {
            self.capture.cancel();
        }
        let enabled = enabled
            && !self.working()
            && !self.capturing()
            && self.file_error.is_none()
            && !self.recording_open()
            && !(focused && (self.renaming() || ctx.text_edit_focused()));
        match self.model.shortcuts(enabled) {
            Some(Ok(())) => self.error.clear(),
            Some(Err(error)) => self.error = Message::error(error),
            None => {}
        }
        // Bootstrap the overlay while minimized; visible children keep UI passes running.
        if self.model.settings().show_overlay
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

    /// Dialogs; returns validated settings for the caller to save.
    pub fn windows(
        &mut self,
        ctx: &egui::Context,
        blocked: bool,
        visible: bool,
    ) -> Option<SoundboardSettings> {
        let settings = self.settings_ui(ctx, blocked);
        if let Some(tool) = &mut self.recording_import {
            match tool.ui(ctx, self.model.saving_recording()) {
                Some(recording_import_dialog::Action::Close) => self.recording_import = None,
                Some(recording_import_dialog::Action::Save(audio, selection, name)) => {
                    self.commands
                        .push(Command::SaveRecording(audio, selection, name));
                }
                None => {}
            }
        }
        let keep_editing = visible && !self.settings_dialog.is_open();
        if !blocked
            && !self.recording_open()
            && let Some((index, name)) =
                self.list.rename_request(keep_editing, self.model.playing())
        {
            self.commands.push(Command::Rename(index, name));
        }
        if let Some((title, error)) = &self.file_error
            && notice(ctx, *title, error)
        {
            self.file_error = None;
        }
        settings
    }

    /// Must run every frame once shown, so the overlay viewport keeps existing.
    pub fn overlay_ui(&mut self, ctx: &egui::Context, allowed: bool) {
        let enabled = allowed && self.model.settings().show_overlay && !self.model.importing();
        let playing = self.model.now_playing().filter(|_| enabled);
        self.overlay_visible = match self.overlay.show(ctx, playing) {
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

    fn play_sound(&mut self, index: usize) {
        match self.model.play(index) {
            Ok(()) => self.error.clear(),
            Err(error) => self.error = Message::error(error),
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
        self.capture.begin(target);
    }

    fn apply_capture(&mut self, ctx: &egui::Context) {
        let Some((target, key)) = self.capture.poll(ctx) else {
            return;
        };
        // Closing the dialog cancels its captures, so without a draft only sound keys,
        // which `saved` merely checks against, can arrive here.
        let mut saved = self.model.settings().keys;
        let keys = self
            .settings_dialog
            .draft_mut()
            .map_or(&mut saved, |draft| &mut draft.keys);
        let result = self.model.bind(&target, key, keys);
        let message = self.message_for(&target);
        match result {
            Ok(()) => message.clear(),
            Err(error) => {
                tracing::error!("Failed to bind hotkey: {error:#}");
                *message = Message::error(error);
            }
        }
    }

    /// Operations clicked here are queued, so `available` holds for the whole frame.
    pub fn ui(&mut self, ui: &mut egui::Ui, others: Busy) {
        let available = !self.working()
            && !others.any()
            && !self.settings_dialog.is_open()
            && !self.recording_open();
        ui.add_enabled_ui(available, |ui| {
            self.toolbar_ui(ui);
            let actions = self
                .list
                .ui(ui, &self.model.library().sounds, self.list_status());
            self.commands.extend(actions.into_iter().map(Command::List));
            let actions = self
                .list
                .batch_ui(ui, &self.model.library().sounds, self.list_status());
            self.commands
                .extend(actions.into_iter().map(Command::Batch));
            self.playback_ui(ui);
        });
        if let Some(ids) = self
            .list
            .delete_ui(ui.ctx(), &self.model.library().sounds, available)
        {
            self.commands.push(Command::Remove(ids));
        }
        self.batch_rename_ui(ui.ctx(), available);
    }

    /// Starts the operations queued this frame; they are dropped when not `allowed`.
    pub fn apply(&mut self, ctx: &egui::Context, allowed: bool) {
        for command in std::mem::take(&mut self.commands) {
            if !allowed || self.working() {
                break;
            }
            match command {
                Command::List(action) => self.list_action(action),
                Command::Batch(action) => self.batch_action(ctx, action),
                Command::Capture(target) => {
                    self.begin_capture(target);
                    ctx.request_repaint();
                }
                Command::Remove(ids) => {
                    self.model.remove(&ids);
                    self.error.clear();
                }
                Command::Rename(index, name) => {
                    let result = self
                        .model
                        .edit(Edit::Rename, move |library| library.rename(index, &name));
                    if let Err(error) = result {
                        self.list.rename_finished(false);
                        self.error = Message::error(error);
                    }
                }
                Command::BatchRename(plan) => {
                    let result = self
                        .model
                        .edit(Edit::BatchRename, move |library| library.rename_batch(plan));
                    if let Some(tool) = &mut self.batch_rename {
                        tool.apply_result(result);
                    }
                }
                Command::SaveRecording(audio, selection, name) => {
                    self.model.save_recording(audio, selection, name)
                }
                Command::OpenSettings => {
                    self.settings_dialog.open(self.model.settings());
                    self.settings_error.clear();
                    self.devices = Some(Task::run(
                        "VoiceBoxOutputDevices",
                        &self.wake,
                        devices::enumerate_soundboard_devices,
                    ));
                }
                Command::RecordImport => {
                    self.model.stop();
                    self.list.cancel_rename();
                    self.list.cancel_delete();
                    self.capture.cancel();
                    self.recording_import = Some(RecordingImportDialog::new(self.wake.clone()));
                }
                Command::Import => self.model.import(super::picker::choose_files),
            }
        }
    }

    fn list_status(&self) -> sound_list::Status {
        let capturing = match self.capture.target() {
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
        use batch_rename_dialog::{Action, Status};
        let Some(tool) = &mut self.batch_rename else {
            return;
        };
        let status = Status {
            available,
            busy: self.model.busy() || self.devices.is_some(),
            playing: self.model.playing(),
        };
        match tool.ui(ctx, self.model.library(), status) {
            None => {}
            Some(Action::Close) => {
                self.list.clear_anchor();
                self.batch_rename = None;
            }
            Some(Action::StopPlayback) => {
                self.commands.push(Command::List(sound_list::Action::Stop))
            }
            Some(Action::Apply(plan)) => self.commands.push(Command::BatchRename(plan)),
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
                    self.capture.cancel();
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
                            self.commands.push(Command::OpenSettings);
                        }
                        // This row is laid out right-to-left: recording sits to the right of import.
                        if ui.button(Text::RecordImport.text()).clicked() {
                            self.commands.push(Command::RecordImport);
                        }
                        if ui.button(Text::Import.text()).clicked() {
                            self.commands.push(Command::Import);
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
                self.model.edit(Edit::Hotkeys, move |library| {
                    library.set_hotkeys(&HashSet::from([id]), None)
                })
            }
            Action::Move { from, to } => self
                .model
                .edit(Edit::Move, move |library| library.move_sound(from, to)),
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
                .settings()
                .keys
                .stop
                .map(|key| Message::StopPlaybackHotkey { key }.text().into_owned())
                .unwrap_or_else(|| Text::StopPlayback.text().into());
            if ui
                .add_enabled(self.model.playing(), egui::Button::new(key))
                .clicked()
            {
                self.commands.push(Command::List(sound_list::Action::Stop));
            }
            let error = [&self.error, self.model.shortcut_error()]
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
            } else if let Some(playing) = self.model.now_playing() {
                let duration_text = playing
                    .duration_secs
                    .map(format_duration)
                    .unwrap_or_else(|| "…".into());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!(
                        "{} / {duration_text}",
                        format_duration(playing.elapsed_secs)
                    ));
                    ui.add(
                        egui::Label::new(&playing.name)
                            .truncate()
                            .show_tooltip_when_elided(false),
                    )
                    .on_hover_text(&playing.name);
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
            BatchAction::Move(to_top) => self
                .model
                .edit(Edit::Move, move |library| library.move_sounds(&ids, to_top)),
            BatchAction::Rename => {
                self.batch_rename = Some(BatchRenameDialog::new(self.model.library(), ids));
                return;
            }
            BatchAction::BindHotkey => {
                if !ids.is_empty() {
                    self.begin_capture(Target::Sounds(ids));
                }
                ctx.request_repaint();
                return;
            }
            BatchAction::ClearHotkey => self.model.edit(Edit::Hotkeys, move |library| {
                library.set_hotkeys(&ids, None)
            }),
        };
        match result {
            Ok(()) => self.error.clear(),
            Err(error) => {
                tracing::error!("Failed to edit the selected sounds: {error:#}");
                self.error = Message::error(error);
            }
        }
    }

    fn settings_ui(&mut self, ctx: &egui::Context, blocked: bool) -> Option<SoundboardSettings> {
        use soundboard_settings_dialog::{Action, Status};
        let status = Status {
            busy: blocked || self.capturing(),
            capture_target: self.capture.target(),
            capture_message: &self.capture_message,
            error: (!self.settings_error.is_empty()).then_some(&self.settings_error),
        };
        match self.settings_dialog.ui(ctx, self.model.settings(), status) {
            None => {}
            Some(Action::Close) => {
                self.capture.cancel();
                self.capture_message.clear();
            }
            Some(Action::ClearMessage) => self.capture_message.clear(),
            Some(Action::Save(draft)) => match self.model.validate_settings(&draft) {
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
            },
            Some(Action::Capture(target)) => self.commands.push(Command::Capture(target)),
        }
        None
    }

    fn finish_edit(&mut self, kind: Edit, result: anyhow::Result<()>) {
        let result = result.inspect(|()| self.error.clear()).map_err(|error| {
            self.error = Message::error(error);
            anyhow::anyhow!(self.error.clone())
        });
        if kind == Edit::Rename {
            self.list.rename_finished(result.is_ok());
        }
        if kind == Edit::BatchRename
            && let Some(tool) = &mut self.batch_rename
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

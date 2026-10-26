use super::{
    batch_rename::{self, BatchRename},
    hotkey_binding::HotkeyManager,
    jobs::{JobResult, Jobs},
    recording_import::RecordingImport,
    sound_list::{self, SoundList},
    sound_settings::{self, SoundSettings},
    widgets::format_duration,
};
use crate::i18n::Text;
use crate::i18n::{Details, Message};
use crate::{
    settings::{BindingTarget, Settings},
    soundboard::{Soundboard, library::Library},
};
use std::collections::HashSet;

pub(super) struct SoundboardPanel {
    pub(super) model: Soundboard,
    settings: SoundSettings,
    list: SoundList,
    batch_rename: Option<BatchRename>,
    recording_import: Option<RecordingImport>,
}

impl SoundboardPanel {
    pub(super) fn new(settings: Settings, library: Library) -> Self {
        Self {
            model: Soundboard::new(settings, library),
            settings: SoundSettings::default(),
            list: SoundList::default(),
            batch_rename: None,
            recording_import: None,
        }
    }
    pub(super) fn renaming(&self) -> bool {
        self.list.renaming() || self.batch_rename.is_some()
    }
    pub(super) fn cancel_rename(&mut self) {
        self.list.cancel_rename();
    }
    pub(super) fn cancel_delete(&mut self) {
        self.list.cancel_delete();
    }
    pub(super) fn cancel_batch(&mut self) {
        if self.model.batch_renaming() || self.model.deleting() {
            return;
        }
        self.list.cancel_batch();
        self.batch_rename = None;
    }
    pub(super) fn recording_open(&self) -> bool {
        self.recording_import.is_some()
    }
    pub(super) fn recording_saving(&self) -> bool {
        self.recording_import
            .as_ref()
            .is_some_and(|tool| tool.saving())
    }
    pub(super) fn interaction_blocked(&self, jobs: &Jobs) -> bool {
        jobs.busy()
            || self.model.importing()
            || self.model.deleting()
            || self.model.batch_renaming()
            || self.recording_saving()
    }
    pub(super) fn poll_recording(&mut self, ctx: &egui::Context) {
        if let Some(tool) = &mut self.recording_import
            && let Some(sound) = tool.poll(ctx)
        {
            self.model.add_recording(sound);
            self.recording_import = None;
        }
    }
    pub(super) fn recording_ui(&mut self, ctx: &egui::Context) {
        if let Some(tool) = &mut self.recording_import
            && tool.ui(ctx)
        {
            self.recording_import = None;
        }
    }

    fn save_soundboard_settings(
        &mut self,
        settings: Settings,
        jobs: &mut Jobs,
        hotkeys: &HotkeyManager,
    ) {
        let result = [
            settings.playback_stop_hotkey,
            settings.playback_random_hotkey,
        ]
        .into_iter()
        .flatten()
        .try_for_each(|key| hotkeys.validate(key))
        .and_then(|()| self.model.save_settings(settings));
        match result {
            Ok(()) => {
                jobs.status.clear();
                jobs.failed = false;
            }
            Err(error) => {
                tracing::error!("Failed to save soundboard settings: {error:#}");
                jobs.status = Message::OperationFailed {
                    error: Details::new(error),
                };
                jobs.failed = true;
            }
        }
    }

    pub(super) fn play_sound(
        &mut self,
        ctx: &egui::Context,
        index: usize,
        hotkeys: &mut HotkeyManager,
    ) {
        hotkeys.clear_error();
        self.model.play(index, super::wake_ui(ctx.clone()));
    }

    pub(super) fn play_random_sound(
        &mut self,
        ctx: &egui::Context,
        hotkey: Option<crate::hotkeys::Hotkey>,
        hotkeys: &mut HotkeyManager,
    ) {
        if let Some(index) = self.model.random_sound_index(hotkey) {
            self.play_sound(ctx, index, hotkeys);
        }
    }

    pub(super) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        jobs: &mut Jobs,
        session_busy: bool,
        hotkeys: &mut HotkeyManager,
    ) {
        let available = !self.interaction_blocked(jobs)
            && !session_busy
            && !self.settings.is_open()
            && !self.recording_open();
        ui.add_enabled_ui(available, |ui| {
            if hotkeys.batch_capturing()
                && ui.is_enabled()
                && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                hotkeys.cancel_capture();
            }
            if self.list.batch_cancelable()
                && ui.is_enabled()
                && ui.ctx().memory(|memory| memory.top_modal_layer().is_none())
                && !ui.ctx().any_popup_open()
                && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                self.cancel_batch();
            }
            self.toolbar_ui(ui, jobs, hotkeys);
            // The toolbar can start an operation during this same frame.
            if self.interaction_blocked(jobs) || self.recording_open() {
                ui.disable();
            }
            let actions = self
                .list
                .ui(ui, &self.model.library().sounds, self.list_status(hotkeys));
            for action in actions {
                self.list_action(ui.ctx(), hotkeys, action);
            }
            let batch_actions =
                self.list
                    .batch_ui(ui, &self.model.library().sounds, self.list_status(hotkeys));
            for action in batch_actions {
                self.batch_action(ui.ctx(), jobs, hotkeys, action);
            }
            self.playback_ui(ui, hotkeys);
        });
        let available = available && !self.interaction_blocked(jobs);
        if let Some(ids) = self
            .list
            .delete_ui(ui.ctx(), &self.model.library().sounds, available)
        {
            self.model.remove(&ids, super::wake_ui(ui.ctx().clone()));
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
        if tool.applying()
            && let Some(result) = self.model.take_rename_result()
        {
            let closed = tool.complete(self.model.library(), result);
            if let Some(selected) = self.list.selected_mut() {
                *selected = tool.selected().clone();
            }
            if closed {
                self.list.clear_anchor();
                self.batch_rename = None;
                return;
            }
        }
        match tool.ui(
            ctx,
            self.model.library(),
            Status {
                available,
                busy: self.model.busy(),
                playing: self.model.playing(),
            },
        ) {
            Action::None => {}
            Action::Closed => {
                self.list.clear_anchor();
                self.batch_rename = None;
            }
            Action::StopPlayback => self.model.stop(),
            Action::Apply(plan) => {
                let result = self.model.rename_sounds(plan, super::wake_ui(ctx.clone()));
                tool.apply_result(result);
            }
        }
    }

    fn toolbar_ui(&mut self, ui: &mut egui::Ui, jobs: &mut Jobs, hotkeys: &mut HotkeyManager) {
        ui.horizontal(|ui| {
            if hotkeys.target().is_some() {
                ui.disable();
            }
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
                    if self.model.deleting() || jobs.exporting() {
                        ui.spinner();
                    }
                    if self.list.batch_mode() {
                        return;
                    }
                    if ui.button(Text::Settings.text()).clicked() {
                        self.settings.open(&self.model.settings);
                        jobs.begin(ui.ctx(), Text::ReadingOutputDevices, || {
                            super::jobs::JobResult::OutputDevices(
                                crate::devices::enumerate_soundboard_devices(),
                            )
                        });
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
                        self.model.import(
                            super::picker::choose_files,
                            super::wake_ui(ui.ctx().clone()),
                        );
                    }
                    if self.model.importing() {
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
        match action {
            Action::Play(index) => self.play_sound(ctx, index, hotkeys),
            Action::Stop => self.model.stop(),
            Action::Remove(id) => self
                .model
                .remove(&HashSet::from([id]), super::wake_ui(ctx.clone())),
            Action::Capture(target) => {
                if hotkeys.target() == Some(target) {
                    hotkeys.cancel_capture();
                } else {
                    self.model.stop();
                    hotkeys.begin_capture(target);
                }
            }
            Action::ClearHotkey(index) => {
                if let Err(error) = self
                    .model
                    .bind_hotkey(BindingTarget::Sound(index), None, None)
                {
                    tracing::error!("Failed to clear hotkey: {error:#}");
                    self.model.set_error(Message::error(error));
                }
            }
            Action::Move { from, to } => {
                if let Err(error) = self.model.move_sound(from, to) {
                    tracing::error!("Failed to reorder sounds: {error:#}");
                    self.model.set_error(Message::error(error));
                }
            }
        }
    }

    fn playback_ui(&mut self, ui: &mut egui::Ui, hotkeys: &HotkeyManager) {
        ui.horizontal(|ui| {
            let key = self
                .model
                .settings
                .playback_stop_hotkey
                .map(|key| Message::StopPlaybackHotkey { key }.text().into_owned())
                .unwrap_or_else(|| Text::StopPlayback.text().into());
            if ui
                .add_enabled(
                    self.model.playing() && hotkeys.target().is_none(),
                    egui::Button::new(key),
                )
                .clicked()
            {
                self.model.stop();
            }
            let error = if !hotkeys.error().is_empty() {
                Some(hotkeys.error())
            } else if self.model.failed {
                Some(&self.model.message)
            } else {
                None
            };
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
        jobs: &mut Jobs,
        hotkeys: &mut HotkeyManager,
        action: sound_list::BatchAction,
    ) {
        use sound_list::BatchAction;
        if matches!(action, BatchAction::CancelCapture) {
            hotkeys.cancel_capture();
            return;
        }
        let Some(ids) = self.list.selected().cloned() else {
            return;
        };
        match action {
            BatchAction::CancelCapture => unreachable!(),
            BatchAction::Export => {
                self.model.stop();
                let sounds: Vec<_> = self
                    .model
                    .library()
                    .sounds
                    .iter()
                    .filter(|sound| ids.contains(&sound.id))
                    .cloned()
                    .collect();
                if !jobs.begin(ctx, Text::ExportSounds, move || {
                    JobResult::Exported(super::picker::choose_folder().and_then(|folder| {
                        folder.map_or(Ok(()), |folder| {
                            crate::soundboard::export::export_sounds(&sounds, &folder)
                        })
                    }))
                }) {
                    self.model.file_error =
                        Some((Text::SoundExportFailedTitle, jobs.status.clone()));
                    jobs.status.clear();
                    jobs.failed = false;
                } else {
                    self.model.failed = false;
                    self.model.message.clear();
                }
            }
            BatchAction::Move(to_top) => match self.model.move_sounds(&ids, to_top) {
                Ok(()) => {
                    self.model.message.clear();
                    self.model.failed = false;
                }
                Err(error) => self.model.set_error(Message::error(error)),
            },
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
                    hotkeys.begin_batch_capture(BindingTarget::Sound(index), &ids);
                }
                ctx.request_repaint();
            }
            BatchAction::ClearHotkey => match self.model.bind_sound_hotkeys(&ids, None) {
                Ok(()) => hotkeys.message.clear(),
                Err(error) => {
                    tracing::error!("Failed to clear selected hotkeys: {error:#}");
                    hotkeys.message = Message::error(error);
                }
            },
        }
    }

    pub(super) fn file_error_ui(&mut self, ctx: &egui::Context) {
        let Some((title, error)) = &self.model.file_error else {
            return;
        };
        let available = ctx.content_rect().size();
        let response = egui::Modal::new(egui::Id::unique("sound_file_error"))
            .frame(egui::Frame::popup(&ctx.style_of(ctx.theme())).inner_margin(16))
            .show(ctx, |ui| {
                ui.set_width((available.x - 64.0).clamp(180.0, 600.0));
                ui.heading(egui::RichText::new(title.text()).size(24.0));
                ui.separator();
                ui.spacing_mut().scroll.floating = false;
                egui::ScrollArea::vertical()
                    .id_salt("sound_file_errors")
                    .max_height((available.y - 160.0).clamp(60.0, 400.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(error.text()).wrap().selectable(true));
                    });
                ui.separator();
                if ui.button(Text::Close.text()).clicked() {
                    ui.close();
                }
            });
        if response.should_close() {
            self.model.file_error = None;
        }
    }

    pub(super) fn settings_ui(
        &mut self,
        ctx: &egui::Context,
        jobs: &mut Jobs,
        hotkeys: &mut HotkeyManager,
    ) {
        use sound_settings::{Action, Status};
        match self.settings.ui(
            ctx,
            &self.model.settings,
            Status {
                busy: jobs.busy(),
                capture_target: hotkeys.target(),
                capture_message: &hotkeys.settings_message,
                error: jobs.failed.then_some(&jobs.status),
            },
        ) {
            Action::None => {}
            Action::Closed => {
                hotkeys.cancel_capture();
                hotkeys.settings_message.clear();
            }
            Action::ClearMessage => hotkeys.settings_message.clear(),
            Action::Save(settings) => self.save_soundboard_settings(settings, jobs, hotkeys),
            Action::Capture(target) => {
                if hotkeys.target() == Some(target) {
                    hotkeys.cancel_capture();
                } else {
                    self.model.stop();
                    hotkeys.begin_capture(target);
                }
                ctx.request_repaint();
            }
        }
    }

    pub(super) fn set_output_devices(
        &mut self,
        microphones: Vec<crate::devices::Microphone>,
        speakers: Vec<crate::devices::Speaker>,
    ) {
        self.settings.set_devices(microphones, speakers);
    }

    pub(super) fn poll_capture(&mut self, ctx: &egui::Context, hotkeys: &mut HotkeyManager) {
        if let Some(capture) = hotkeys.poll_capture(ctx) {
            let result = if let Some(ids) = capture.ids {
                self.model.bind_sound_hotkeys(&ids, Some(capture.key))
            } else {
                self.model
                    .bind_hotkey(capture.target, Some(capture.key), self.settings.draft_mut())
            };
            hotkeys.capture_result(capture.target, result);
        }
    }

    pub(super) fn finish_rename(&mut self, soundboard_tab_selected: bool) {
        let keep_editing = soundboard_tab_selected && !self.settings.is_open();
        if let Some((index, name)) = self.list.rename_request(keep_editing, self.model.playing()) {
            let result = self.model.rename(index, &name);
            self.list.rename_finished(result.is_ok());
            match result {
                Ok(()) => {
                    self.model.failed = false;
                    self.model.message.clear();
                }
                Err(error) => self.model.set_error(Message::error(error)),
            }
        }
    }
    pub(super) fn finish_delete(&mut self) {
        if let Some(removed) = self.model.take_removed_sounds() {
            self.list.remove_ids(&removed);
        }
    }
}

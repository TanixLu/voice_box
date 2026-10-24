use super::{
    hotkey_binding::{HotkeyManager, hotkey_badge, hotkey_editor},
    jobs::Jobs,
    widgets::{ButtonIcon, hint, icon_button, play_button, trailing_button, warning},
};
use crate::app::i18n::Text;
use crate::app::i18n::{Details, Message};
use crate::app::soundboard::{
    Soundboard,
    config::{BindingTarget, Config, Options, OutputDevice},
};

struct SoundRenameEditor {
    index: usize,
    name: String,
    request_focus: bool,
    save_requested: bool,
}

impl SoundRenameEditor {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let input = ui
            .add_sized(
                [ui.available_width().max(16.0), 30.0],
                egui::TextEdit::singleline(&mut self.name).id_salt("sound_name"),
            )
            .on_hover_text(Text::RenameHint.text());
        if self.request_focus && ui.is_enabled() {
            input.request_focus();
            let mut state = egui::TextEdit::load_state(ui.ctx(), input.id).unwrap_or_default();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::select_all_str(&self.name)));
            state.store(ui.ctx(), input.id);
            self.request_focus = false;
        } else if input.lost_focus() {
            self.save_requested = true;
        }
    }
}

pub(super) struct SoundboardPanel {
    pub(super) model: Soundboard,
    pub(super) draft_options: Option<Options>,
    filter: String,
    rename_editor: Option<SoundRenameEditor>,
    pending_delete_index: Option<usize>,
    recording_import: Option<super::recording_import::RecordingImport>,
    pub(super) microphones: Vec<(OutputDevice, String)>,
    pub(super) speakers: Vec<(OutputDevice, String)>,
}

impl SoundboardPanel {
    pub(super) fn new(config: Config) -> Self {
        Self {
            model: Soundboard::new(config),
            filter: String::new(),
            draft_options: None,
            rename_editor: None,
            pending_delete_index: None,
            recording_import: None,
            microphones: Vec::new(),
            speakers: Vec::new(),
        }
    }
    pub(super) fn renaming(&self) -> bool {
        self.rename_editor.is_some()
    }
    pub(super) fn cancel_rename(&mut self) {
        self.rename_editor = None;
    }
    pub(super) fn cancel_delete(&mut self) {
        self.pending_delete_index = None;
    }
    pub(super) fn recording_open(&self) -> bool {
        self.recording_import.is_some()
    }
    pub(super) fn recording_saving(&self) -> bool {
        self.recording_import
            .as_ref()
            .is_some_and(|tool| tool.saving())
    }
    pub(super) fn poll_recording(&mut self, ctx: &egui::Context) {
        if let Some(tool) = &mut self.recording_import
            && tool.poll(ctx, &mut self.model)
        {
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

    fn save_soundboard_options(&mut self, jobs: &mut Jobs, hotkeys: &HotkeyManager) {
        let Some(options) = self.draft_options.clone() else {
            return;
        };
        let result = [options.stop_hotkey, options.random_playback_hotkey]
            .into_iter()
            .flatten()
            .try_for_each(|key| hotkeys.validate(key))
            .and_then(|()| self.model.save_options(options));
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

    pub(super) fn play_sound(&mut self, ctx: &egui::Context, index: usize) {
        self.model.play(index, super::wake_ui(ctx.clone()));
    }

    pub(super) fn play_random_sound(
        &mut self,
        ctx: &egui::Context,
        hotkey: Option<crate::app::hotkeys::Hotkey>,
    ) {
        if let Some(index) = self.model.random_sound_index(hotkey) {
            self.play_sound(ctx, index);
        }
    }

    pub(super) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        jobs: &mut Jobs,
        session_busy: bool,
        hotkeys: &mut HotkeyManager,
    ) {
        let available =
            !jobs.busy() && !session_busy && self.draft_options.is_none() && !self.recording_open();
        ui.add_enabled_ui(available, |ui| {
            ui.horizontal(|ui| {
                ui.strong(
                    Message::SoundCount {
                        count: self.model.library().sounds.len(),
                    }
                    .text(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(Text::Settings.text()).clicked() {
                        self.draft_options = Some(self.model.options().clone());
                        jobs.begin(ui.ctx(), Text::ReadingOutputDevices, || {
                            super::jobs::JobResult::OutputDevices(
                                crate::app::devices::enumerate_soundboard_devices(),
                            )
                        });
                    }
                    // This row is laid out right-to-left: recording sits to the right of import.
                    if ui
                        .add_enabled(
                            !self.model.importing(),
                            egui::Button::new(Text::RecordImport.text()),
                        )
                        .clicked()
                    {
                        self.model.stop();
                        self.cancel_rename();
                        self.cancel_delete();
                        hotkeys.cancel_capture();
                        self.recording_import =
                            Some(super::recording_import::RecordingImport::default());
                    }
                    if ui
                        .add_enabled(
                            !self.model.importing(),
                            egui::Button::new(Text::Import.text()),
                        )
                        .clicked()
                    {
                        self.model.import(
                            super::picker::choose_files,
                            super::wake_ui(ui.ctx().clone()),
                        );
                    }
                    if self.model.importing() {
                        ui.spinner();
                    }
                });
            });
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text(Text::SearchSounds.text())
                        .desired_width(f32::INFINITY),
                )
                .changed()
            {
                self.pending_delete_index = None;
            }
            let filter = self.filter.to_lowercase();
            let indices: Vec<_> = self
                .model
                .library()
                .sounds
                .iter()
                .enumerate()
                .filter(|(index, sound)| {
                    self.rename_editor
                        .as_ref()
                        .is_some_and(|edit| edit.index == *index)
                        || sound.file_name.to_lowercase().contains(&filter)
                })
                .map(|(index, _)| index)
                .collect();
            let mut play = None;
            let mut stop = false;
            let mut remove = None;
            let mut delete_clicked = false;
            let mut capture = None;
            let mut rename = None;
            let mut clear_hotkey = None;
            let mut rename_visible = false;
            let renaming = self.rename_editor.is_some();
            let capture_target = hotkeys.target();
            let capturing = capture_target.is_some();
            let can_reorder = ui.is_enabled()
                && filter.is_empty()
                && !renaming
                && !capturing
                && !self.model.importing();
            if !can_reorder {
                egui::DragAndDrop::clear_payload(ui.ctx());
            }
            let mut reorder = None;
            let height = (ui.available_height() - 50.0).max(50.0);
            ui.scope(|ui| {
                // Keep the scroll bar outside the rows and leave a clear gap
                // between its hit area and the destructive action.
                ui.spacing_mut().scroll.floating = false;
                ui.spacing_mut().scroll.bar_inner_margin = 14.0;
                egui::ScrollArea::vertical()
                    .id_salt("sound_library")
                    .max_height(height)
                    .auto_shrink([false, false])
                    .show_rows(ui, 30.0, indices.len(), |ui, rows| {
                        let row_height = 30.0 + ui.spacing().item_spacing.y;
                        let top = ui.max_rect().top() - rows.start as f32 * row_height;
                        for row in rows {
                            let index = indices[row];
                            let active = self.model.active_sound_index == Some(index);
                            let sound = &self.model.library().sounds[index];
                            ui.push_id(&sound.path, |ui| {
                                ui.horizontal(|ui| {
                                    let handle = icon_button(ui, can_reorder, ButtonIcon::Reorder)
                                        .on_hover_cursor(egui::CursorIcon::Grab)
                                        .on_disabled_hover_text(if filter.is_empty() {
                                            Text::ReorderSoundHint.text()
                                        } else {
                                            Text::ClearSearchToReorder.text()
                                        });
                                    if can_reorder {
                                        if handle.drag_started_by(egui::PointerButton::Primary) {
                                            egui::DragAndDrop::set_payload(ui.ctx(), index);
                                        }
                                        handle.context_menu(|ui| {
                                            for (label, to) in [
                                                (Text::MoveSoundToTop, 0),
                                                (Text::MoveSoundToBottom, indices.len() - 1),
                                            ] {
                                                if ui
                                                    .add_enabled(
                                                        index != to,
                                                        egui::Button::new(label.text()),
                                                    )
                                                    .clicked()
                                                {
                                                    reorder = Some((index, to));
                                                    ui.close();
                                                }
                                            }
                                        });
                                    }
                                    if play_button(ui, !capturing && !renaming, active)
                                        .on_hover_text(if active {
                                            Text::StopPlayback.text()
                                        } else {
                                            Text::PlaySound.text()
                                        })
                                        .clicked()
                                    {
                                        if active {
                                            stop = true;
                                        } else {
                                            play = Some(index);
                                        }
                                    }
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let confirming =
                                                self.pending_delete_index == Some(index);
                                            let icon = if confirming {
                                                ButtonIcon::ConfirmDelete
                                            } else {
                                                ButtonIcon::Delete
                                            };
                                            if icon_button(ui, !capturing && !renaming, icon)
                                                .clicked()
                                            {
                                                ui.ctx().request_repaint();
                                                delete_clicked = true;
                                                if confirming {
                                                    remove = Some(index);
                                                } else {
                                                    self.pending_delete_index = Some(index);
                                                }
                                            }
                                            let duration = sound
                                                .duration
                                                .map(format_duration)
                                                .unwrap_or_else(|| {
                                                    if sound.duration_error.is_some() {
                                                        Text::Failed.text().into()
                                                    } else {
                                                        "…".into()
                                                    }
                                                });
                                            ui.add_sized(
                                                [48.0, 30.0],
                                                egui::Label::new(duration).truncate(),
                                            )
                                            .on_hover_text(
                                                sound
                                                    .duration_error
                                                    .as_ref()
                                                    .map(Message::text)
                                                    .unwrap_or_else(|| {
                                                        Text::AudioLength.text().into()
                                                    }),
                                            );
                                            if let Some(key) = sound.hotkey {
                                                if icon_button(
                                                    ui,
                                                    !capturing && !renaming,
                                                    ButtonIcon::ClearHotkey,
                                                )
                                                .clicked()
                                                {
                                                    clear_hotkey = Some(index);
                                                }
                                                let max_width =
                                                    (ui.available_width() - 74.0).max(24.0);
                                                hotkey_badge(ui, key, max_width);
                                            } else {
                                                let width = (ui.available_width() - 74.0)
                                                    .clamp(24.0, 132.0);
                                                let binding = capture_target
                                                    == Some(BindingTarget::Sound(index));
                                                let label = if binding {
                                                    Text::PressKey.text()
                                                } else {
                                                    Text::BindHotkey.text()
                                                };
                                                if ui
                                                    .add_enabled_ui(
                                                        (!capturing || binding) && !renaming,
                                                        |ui| {
                                                            ui.add_sized(
                                                                [width, 30.0],
                                                                egui::Button::new(label).truncate(),
                                                            )
                                                        },
                                                    )
                                                    .inner
                                                    .on_hover_text(if binding {
                                                        Text::CaptureHotkeyHint.text()
                                                    } else {
                                                        Text::BindHotkey.text()
                                                    })
                                                    .clicked()
                                                {
                                                    capture = Some(BindingTarget::Sound(index));
                                                }
                                            }
                                            if let Some(edit) = &mut self.rename_editor
                                                && edit.index == index
                                            {
                                                rename_visible = true;
                                                edit.ui(ui);
                                            } else {
                                                let name = if active {
                                                    Message::PlayingSound {
                                                        name: sound.file_name.clone(),
                                                    }
                                                    .text()
                                                    .into_owned()
                                                } else {
                                                    sound.file_name.clone()
                                                };
                                                let response = ui
                                                    .add_sized(
                                                        [ui.available_width().max(16.0), 30.0],
                                                        egui::Button::new(())
                                                            .left_text(name)
                                                            .frame_when_inactive(false)
                                                            .truncate(),
                                                    )
                                                    .on_hover_text(
                                                        Message::RenameSoundHint {
                                                            name: sound.path.display().to_string(),
                                                        }
                                                        .text(),
                                                    );
                                                if response.double_clicked()
                                                    && !capturing
                                                    && !renaming
                                                {
                                                    rename = Some((index, sound.file_name.clone()));
                                                }
                                            }
                                        },
                                    );
                                });
                            });
                        }
                        if can_reorder
                            && let Some(from) = egui::DragAndDrop::payload::<usize>(ui.ctx())
                            && let Some(pointer) = ui.input(|input| input.pointer.interact_pos())
                            && ui.clip_rect().contains(pointer)
                        {
                            let slot = ((pointer.y - top) / row_height)
                                .round()
                                .clamp(0.0, indices.len() as f32)
                                as usize;
                            let to = slot.saturating_sub(usize::from(*from < slot));
                            if *from != to {
                                let y = (top + slot as f32 * row_height
                                    - ui.spacing().item_spacing.y * 0.5)
                                    .clamp(
                                        ui.clip_rect().top() + 1.0,
                                        ui.clip_rect().bottom() - 1.0,
                                    );
                                ui.painter().hline(
                                    ui.max_rect().x_range(),
                                    y,
                                    egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
                                );
                            }
                            if ui.input(|input| input.pointer.primary_released()) {
                                reorder = Some((*from, to));
                                egui::DragAndDrop::clear_payload(ui.ctx());
                            } else {
                                let viewport = ui.clip_rect();
                                let edge = 30.0_f32.min(viewport.height() * 0.5);
                                let direction = if pointer.y < viewport.top() + edge {
                                    1.0
                                } else if pointer.y > viewport.bottom() - edge {
                                    -1.0
                                } else {
                                    0.0
                                };
                                if direction != 0.0 {
                                    let delta = ui.input(|input| input.stable_dt.min(0.1)) * 600.0;
                                    ui.scroll_with_delta_animation(
                                        egui::vec2(0.0, direction * delta),
                                        egui::style::ScrollAnimation::none(),
                                    );
                                    ui.ctx().request_repaint();
                                }
                            }
                        }
                    });
            });
            if let Some(index) = clear_hotkey
                && let Err(error) = self
                    .model
                    .bind_hotkey(BindingTarget::Sound(index), None, None)
            {
                tracing::error!("Failed to clear hotkey: {error:#}");
                self.model.set_error(Message::error(error));
            }
            if let Some(edit) = &mut self.rename_editor
                && !rename_visible
                && !edit.request_focus
            {
                edit.save_requested = true;
            }
            if !delete_clicked && ui.input(|input| input.pointer.any_click()) {
                self.pending_delete_index = None;
            }
            if indices.is_empty() {
                // The empty message is painted inside the list's reserved space.
                let text = if self.model.library().sounds.is_empty() {
                    Text::ImportSoundsHint.text()
                } else {
                    Text::NoMatchingSounds.text()
                };
                ui.painter().text(
                    ui.min_rect().center(),
                    egui::Align2::CENTER_CENTER,
                    text,
                    egui::TextStyle::Body.resolve(ui.style()),
                    ui.visuals().weak_text_color(),
                );
            }
            if stop {
                self.model.stop();
            } else if let Some(index) = play {
                self.play_sound(ui.ctx(), index);
            }
            if let Some(index) = remove {
                self.pending_delete_index = None;
                self.model.remove(index);
            }
            if let Some(target) = capture {
                if capture_target == Some(target) {
                    hotkeys.cancel_capture();
                } else {
                    hotkeys.begin_capture(target, &mut self.model);
                }
            }
            if let Some((index, name)) = rename {
                self.model.stop();
                self.pending_delete_index = None;
                self.rename_editor = Some(SoundRenameEditor {
                    index,
                    name,
                    request_focus: true,
                    save_requested: false,
                });
                ui.ctx().request_repaint();
            }
            if let Some((from, to)) = reorder {
                self.pending_delete_index = None;
                if let Err(error) = self.model.move_sound(from, to) {
                    tracing::error!("Failed to reorder sounds: {error:#}");
                    self.model.set_error(Message::error(error));
                }
                ui.ctx().request_repaint();
            }
            ui.horizontal(|ui| {
                let key = self
                    .model
                    .options()
                    .stop_hotkey
                    .map(|key| Message::StopPlaybackHotkey { key }.text().into_owned())
                    .unwrap_or_else(|| Text::StopPlayback.text().into());
                if ui
                    .add_enabled(self.model.playing(), egui::Button::new(key))
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
                        .truncate(),
                    );
                } else if let Some(index) = self.model.active_sound_index {
                    let sound = self.model.library().sounds.get(index);
                    let name = sound.map(|sound| sound.file_name.as_str()).unwrap_or("");
                    let duration = self
                        .model
                        .duration
                        .or(sound.and_then(|sound| sound.duration))
                        .filter(|duration| duration.is_finite() && *duration > 0.0)
                        .map(format_duration)
                        .unwrap_or_else(|| "…".into());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(format!(
                            "{} / {duration}",
                            format_duration(self.model.elapsed)
                        ));
                        ui.add(egui::Label::new(name).truncate())
                            .on_hover_text(name);
                    });
                }
            });
        });
    }

    pub(super) fn import_error_ui(&mut self, ctx: &egui::Context) {
        let Some(error) = &self.model.import_error else {
            return;
        };
        let available = ctx.content_rect().size();
        let response = egui::Modal::new(egui::Id::unique("sound_import_error"))
            .frame(egui::Frame::popup(&ctx.style_of(ctx.theme())).inner_margin(16))
            .show(ctx, |ui| {
                ui.set_width((available.x - 64.0).clamp(180.0, 600.0));
                ui.heading(Text::SoundImportFailedTitle.text());
                ui.separator();
                ui.spacing_mut().scroll.floating = false;
                egui::ScrollArea::vertical()
                    .id_salt("sound_import_errors")
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
            self.model.import_error = None;
        }
    }

    pub(super) fn settings_ui(
        &mut self,
        ctx: &egui::Context,
        jobs: &mut Jobs,
        hotkeys: &mut HotkeyManager,
    ) {
        if self.draft_options.is_none() {
            return;
        }
        let available = ctx.content_rect().size();
        let capture_target = hotkeys.target();
        let capturing = capture_target.is_some();
        let idle = !jobs.busy();
        let mut capture = None;
        let mut save = false;
        let mut close = false;
        let response = egui::Modal::new(egui::Id::unique("sound_settings"))
            .frame(egui::Frame::popup(&ctx.style_of(ctx.theme())).inner_margin(16))
            .show(ctx, |ui| {
                ui.set_width((available.x - 64.0).clamp(240.0, 460.0));
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(Text::SoundSettings.text())
                            .size(17.0)
                            .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close = icon_button(ui, idle, ButtonIcon::Close).clicked();
                    });
                });
                ui.add_space(12.0);
                egui::ScrollArea::vertical()
                    .max_height((available.y - 230.0).max(80.0))
                    .show(ui, |ui| {
                        ui.add_enabled_ui(idle, |ui| {
                            let options = self.draft_options.as_mut().unwrap();
                            playback_volume(
                                ui,
                                Text::MicrophoneOutput.text(),
                                &mut options.microphone_output_volume,
                            );
                            output_device_picker(
                                ui,
                                "sound_microphone",
                                &mut options.microphone_output_device,
                                &self.microphones,
                            );
                            ui.add_space(8.0);
                            playback_volume(
                                ui,
                                Text::SpeakerOutput.text(),
                                &mut options.speaker_output_volume,
                            );
                            output_device_picker(
                                ui,
                                "sound_speaker",
                                &mut options.speaker_output_device,
                                &self.speakers,
                            );
                            ui.add_space(8.0);
                            ui.checkbox(
                                &mut options.include_microphone_audio,
                                Text::IncludeMicrophoneAudio.text(),
                            );
                            ui.checkbox(&mut options.show_overlay, Text::ShowOverlay.text());
                            ui.checkbox(&mut options.repeat, Text::RepeatPlayback.text());
                            if hotkey_editor(
                                ui,
                                Text::RandomHotkey.text(),
                                &mut options.random_playback_hotkey,
                                BindingTarget::Random,
                                capture_target,
                            ) {
                                capture = Some(BindingTarget::Random);
                            }
                            if hotkey_editor(
                                ui,
                                Text::StopHotkey.text(),
                                &mut options.stop_hotkey,
                                BindingTarget::Stop,
                                capture_target,
                            ) {
                                capture = Some(BindingTarget::Stop);
                            }
                            if hotkey_editor(
                                ui,
                                Text::HoldDuringPlayback.text(),
                                &mut options.auto_hold_hotkey,
                                BindingTarget::Hold,
                                capture_target,
                            ) {
                                capture = Some(BindingTarget::Hold);
                            }
                            if !hotkeys.error().is_empty() {
                                warning(ui, &hotkeys.error().text());
                            }
                        });
                    });
                ui.add_space(12.0);
                let dirty = self
                    .draft_options
                    .as_ref()
                    .is_some_and(|options| options != self.model.options());
                ui.horizontal_wrapped(|ui| {
                    save = ui
                        .add_enabled(
                            idle && !capturing && dirty,
                            egui::Button::new(Text::SaveSettings.text()),
                        )
                        .clicked();
                    if ui
                        .add_enabled(
                            idle && !capturing && dirty,
                            egui::Button::new(Text::UndoChanges.text()),
                        )
                        .clicked()
                    {
                        self.draft_options = Some(self.model.options().clone());
                        hotkeys.message.clear();
                    }
                    if trailing_button(
                        ui,
                        idle && !capturing
                            && self
                                .draft_options
                                .as_ref()
                                .is_some_and(|options| *options != Options::default()),
                        Text::RestoreDefaults.text(),
                    )
                    .clicked()
                    {
                        self.draft_options = Some(Options::default());
                        hotkeys.message.clear();
                    }
                });
                if self
                    .draft_options
                    .as_ref()
                    .is_some_and(|options| options != self.model.options())
                {
                    warning(ui, Text::UnsavedSoundSettings.text());
                } else {
                    hint(ui, Text::SoundSettingsApplied.text());
                }
                if jobs.failed {
                    warning(ui, &jobs.status.text());
                }
            });
        if close || (response.should_close() && !capturing && idle) {
            self.draft_options = None;
            hotkeys.cancel_capture();
        } else if save {
            self.save_soundboard_options(jobs, hotkeys);
        } else if let Some(target) = capture {
            if capture_target == Some(target) {
                hotkeys.cancel_capture();
            } else {
                hotkeys.begin_capture(target, &mut self.model);
            }
            ctx.request_repaint();
        }
    }

    pub(super) fn finish_rename(&mut self, soundboard_tab_selected: bool) {
        let Some(edit) = &mut self.rename_editor else {
            return;
        };
        if !edit.request_focus && (!soundboard_tab_selected || self.draft_options.is_some()) {
            edit.save_requested = true;
        }
        // Stopping playback is asynchronous; keep the edit until the file is released.
        if edit.save_requested && !self.model.playing() {
            match self.model.rename(edit.index, &edit.name) {
                Ok(()) => {
                    self.rename_editor = None;
                    self.model.failed = false;
                    self.model.message.clear();
                }
                Err(error) => {
                    edit.save_requested = false;
                    edit.request_focus = true;
                    self.model.set_error(Message::error(error));
                }
            }
        }
    }
}

fn output_device_picker(
    ui: &mut egui::Ui,
    id: &str,
    selected: &mut Option<OutputDevice>,
    devices: &[(OutputDevice, String)],
) {
    let name = match selected {
        None => Text::FollowSystemDefault.text().to_owned(),
        Some(device) => devices
            .iter()
            .find(|(available, _)| available.id == device.id)
            .map(|(_, label)| label.clone())
            .unwrap_or_else(|| {
                Message::UnavailableOutputDevice {
                    name: device.name.clone(),
                }
                .text()
                .into_owned()
            }),
    };
    egui::ComboBox::from_id_salt(id)
        .width(ui.available_width())
        .wrap()
        .selected_text(name)
        .show_ui(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.selectable_value(selected, None, Text::FollowSystemDefault.text());
            for (device, name) in devices {
                let is_selected = selected
                    .as_ref()
                    .is_some_and(|selected| selected.id == device.id);
                if ui
                    .add(egui::Button::selectable(is_selected, name).wrap())
                    .clicked()
                {
                    *selected = Some(device.clone());
                }
            }
        });
}

pub(super) fn format_duration(seconds: f64) -> String {
    let total_seconds = seconds.max(0.0) as u64;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    if minutes >= 60 {
        format!("{}:{:02}:{seconds:02}", minutes / 60, minutes % 60)
    } else {
        format!("{minutes}:{seconds:02}")
    }
}
fn playback_volume(ui: &mut egui::Ui, label: &str, volume: &mut f32) -> bool {
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        ui.add(
            egui::Slider::new(volume, 0.0..=2.0)
                .step_by(0.01)
                .custom_formatter(|value, _| {
                    if value == 0.0 {
                        Text::NoOutput.text().into()
                    } else {
                        format!("{:.0}%", value * 100.0)
                    }
                })
                .custom_parser(|text| {
                    if Text::NoOutput.matches_input(text) {
                        Some(0.0)
                    } else {
                        text.trim()
                            .trim_end_matches('%')
                            .trim()
                            .parse::<f64>()
                            .ok()
                            .filter(|value| value.is_finite())
                            .map(|value| value / 100.0)
                    }
                }),
        )
        .changed()
    })
    .inner
}

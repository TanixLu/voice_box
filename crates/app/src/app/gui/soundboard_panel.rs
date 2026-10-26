use super::{
    hotkey_binding::{HotkeyManager, hotkey_badge, hotkey_editor},
    jobs::{JobResult, Jobs},
    widgets::{ButtonIcon, hint, icon_button, play_button, trailing_button, warning},
};
use crate::app::i18n::Text;
use crate::app::i18n::{Details, Message};
use crate::app::{
    settings::{BindingTarget, OutputDevice, Settings},
    soundboard::{
        Soundboard,
        library::{Library, SoundId},
    },
};
use std::collections::HashSet;

#[derive(Default)]
struct SoundSelection {
    ids: HashSet<SoundId>,
    anchor: Option<SoundId>,
    confirming_delete: bool,
}

impl SoundSelection {
    fn toggle(&mut self, library: &Library, indices: &[usize], row: usize, extend: bool) {
        let id = &library.sounds[indices[row]].id;
        let selected = !self.ids.contains(id);
        let start = if extend {
            indices
                .iter()
                .position(|&index| self.anchor.as_ref() == Some(&library.sounds[index].id))
                .unwrap_or(row)
        } else {
            row
        };
        for &index in &indices[start.min(row)..=start.max(row)] {
            let id = &library.sounds[index].id;
            if selected {
                self.ids.insert(*id);
            } else {
                self.ids.remove(id);
            }
        }
        self.anchor = Some(*id);
    }
}

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
    pub(super) draft_settings: Option<Settings>,
    filter: String,
    rename_editor: Option<SoundRenameEditor>,
    pending_delete_index: Option<usize>,
    selection: Option<SoundSelection>,
    batch_actions_height: f32,
    batch_rename: Option<super::batch_rename::BatchRename>,
    recording_import: Option<super::recording_import::RecordingImport>,
    pub(super) microphones: Vec<(OutputDevice, String)>,
    pub(super) speakers: Vec<(OutputDevice, String)>,
}

impl SoundboardPanel {
    pub(super) fn new(settings: Settings, library: Library) -> Self {
        Self {
            model: Soundboard::new(settings, library),
            filter: String::new(),
            draft_settings: None,
            rename_editor: None,
            pending_delete_index: None,
            selection: None,
            batch_actions_height: 30.0,
            batch_rename: None,
            recording_import: None,
            microphones: Vec::new(),
            speakers: Vec::new(),
        }
    }
    pub(super) fn renaming(&self) -> bool {
        self.rename_editor.is_some() || self.batch_rename.is_some()
    }
    pub(super) fn cancel_rename(&mut self) {
        self.rename_editor = None;
    }
    pub(super) fn cancel_delete(&mut self) {
        self.pending_delete_index = None;
    }
    pub(super) fn cancel_batch(&mut self) {
        if self.model.batch_renaming() || self.model.deleting() {
            return;
        }
        self.selection = None;
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

    fn save_soundboard_settings(&mut self, jobs: &mut Jobs, hotkeys: &HotkeyManager) {
        let Some(settings) = self.draft_settings.as_mut() else {
            return;
        };
        settings.language = self.model.settings.language;
        settings.theme = self.model.settings.theme;
        let settings = settings.clone();
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
        hotkey: Option<crate::app::hotkeys::Hotkey>,
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
            && self.draft_settings.is_none()
            && !self.recording_open();
        ui.add_enabled_ui(available, |ui| {
            if hotkeys.batch_capturing()
                && ui.is_enabled()
                && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                hotkeys.cancel_capture();
            }
            if self
                .selection
                .as_ref()
                .is_some_and(|selection| !selection.confirming_delete)
                && ui.is_enabled()
                && ui.ctx().memory(|memory| memory.top_modal_layer().is_none())
                && !ui.ctx().any_popup_open()
                && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                self.cancel_batch();
            }
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
                if self.selection.is_some() {
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
                    self.selection = Some(SoundSelection::default());
                }
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center).with_main_wrap(true),
                    |ui| {
                        if self.model.deleting() || jobs.exporting() {
                            ui.spinner();
                        }
                        if self.selection.is_some() {
                            return;
                        }
                        if ui.button(Text::Settings.text()).clicked() {
                            self.draft_settings = Some(self.model.settings.clone());
                            jobs.begin(ui.ctx(), Text::ReadingOutputDevices, || {
                                super::jobs::JobResult::OutputDevices(
                                    crate::app::devices::enumerate_soundboard_devices(),
                                )
                            });
                        }
                        // This row is laid out right-to-left: recording sits to the right of import.
                        if ui.button(Text::RecordImport.text()).clicked() {
                            self.model.stop();
                            self.cancel_rename();
                            self.cancel_delete();
                            hotkeys.cancel_capture();
                            self.recording_import =
                                Some(super::recording_import::RecordingImport::default());
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
            // The toolbar can start an operation during this same frame.
            if self.interaction_blocked(jobs) || self.recording_open() {
                ui.disable();
            }
            if ui
                .add_enabled(
                    hotkeys.target().is_none(),
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
                        || sound.name.to_lowercase().contains(&filter)
                })
                .map(|(index, _)| index)
                .collect();
            if let Some(selection) = &mut self.selection {
                ui.horizontal_wrapped(|ui| {
                    let mut visible_selected = indices
                        .iter()
                        .filter(|&&index| {
                            selection
                                .ids
                                .contains(&self.model.library().sounds[index].id)
                        })
                        .count();
                    let mut all = !indices.is_empty() && visible_selected == indices.len();
                    let select_all = ui
                        .add_enabled(
                            !indices.is_empty() && hotkeys.target().is_none(),
                            egui::Checkbox::new(&mut all, Text::SelectSearchResults.text())
                                .indeterminate(
                                    visible_selected > 0 && visible_selected < indices.len(),
                                ),
                        )
                        .clicked();
                    let shortcut = ui.is_enabled()
                        && hotkeys.target().is_none()
                        && !selection.confirming_delete
                        && ui.ctx().memory(|memory| memory.top_modal_layer().is_none())
                        && !ui.ctx().text_edit_focused()
                        && ui.input_mut(|input| {
                            input.consume_key(egui::Modifiers::COMMAND, egui::Key::A)
                        });
                    if select_all || shortcut {
                        for &index in &indices {
                            let id = &self.model.library().sounds[index].id;
                            if all || shortcut {
                                selection.ids.insert(*id);
                            } else {
                                selection.ids.remove(id);
                            }
                        }
                        selection.anchor = None;
                        visible_selected = if all || shortcut { indices.len() } else { 0 };
                    }
                    let count = selection.ids.len();
                    let hidden = count - visible_selected;
                    let message = if hidden > 0 {
                        Message::SelectedSoundsWithHidden { count, hidden }
                    } else {
                        Message::SelectedSounds { count }
                    };
                    ui.label(message.text());
                });
            }
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
                && self.selection.is_none()
                && filter.is_empty()
                && !renaming
                && !capturing;
            if !can_reorder {
                egui::DragAndDrop::clear_payload(ui.ctx());
            }
            let mut reorder = None;
            let footer_height = if self.selection.is_some() {
                58.0 + self.batch_actions_height
            } else {
                50.0
            };
            let height = (ui.available_height() - footer_height).max(50.0);
            ui.scope(|ui| {
                if hotkeys.batch_capturing() {
                    ui.disable();
                }
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
                            let mut toggle_selection = false;
                            ui.push_id(sound.id, |ui| {
                                ui.horizontal(|ui| {
                                    if let Some(selection) = &self.selection {
                                        let mut selected = selection.ids.contains(&sound.id);
                                        let checkbox = ui
                                            .add_sized(
                                                [30.0, 30.0],
                                                egui::Checkbox::without_text(&mut selected),
                                            )
                                            .on_hover_text(Text::SelectSoundHint.text());
                                        checkbox.widget_info(|| {
                                            egui::WidgetInfo::selected(
                                                egui::Role::CheckBox,
                                                ui.is_enabled(),
                                                selected,
                                                &sound.name,
                                            )
                                        });
                                        toggle_selection = checkbox.clicked();
                                    } else {
                                        let handle =
                                            icon_button(ui, can_reorder, ButtonIcon::Reorder)
                                                .on_hover_cursor(egui::CursorIcon::Grab)
                                                .on_disabled_hover_text(if filter.is_empty() {
                                                    Text::ReorderSoundHint.text()
                                                } else {
                                                    Text::ClearSearchToReorder.text()
                                                });
                                        if can_reorder {
                                            if handle.drag_started_by(egui::PointerButton::Primary)
                                            {
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
                                            if self.selection.is_none() {
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
                                            }
                                            ui.add_sized(
                                                [48.0, 30.0],
                                                egui::Label::new(format_duration(
                                                    sound.duration_secs,
                                                ))
                                                .truncate(),
                                            )
                                            .on_hover_text(Text::AudioLength.text());
                                            if let Some(key) = sound.hotkey {
                                                if self.selection.is_none()
                                                    && icon_button(
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
                                            } else if self.selection.is_none() {
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
                                                        name: sound.name.clone(),
                                                    }
                                                    .text()
                                                    .into_owned()
                                                } else {
                                                    sound.name.clone()
                                                };
                                                let response = ui
                                                    .add_enabled_ui(!capturing, |ui| {
                                                        ui.add_sized(
                                                            [ui.available_width().max(16.0), 30.0],
                                                            egui::Button::new(())
                                                                .left_text(name)
                                                                .frame_when_inactive(false)
                                                                .selected(
                                                                    self.selection
                                                                        .as_ref()
                                                                        .is_some_and(|selection| {
                                                                            selection
                                                                                .ids
                                                                                .contains(&sound.id)
                                                                        }),
                                                                )
                                                                .truncate(),
                                                        )
                                                    })
                                                    .inner
                                                    .on_hover_text(if self.selection.is_some() {
                                                        format!(
                                                            "{}\n{}",
                                                            sound.name,
                                                            Text::SelectSoundHint.text()
                                                        )
                                                    } else {
                                                        Message::RenameSoundHint {
                                                            name: sound.name.clone(),
                                                        }
                                                        .text()
                                                        .into_owned()
                                                    });
                                                if self.selection.is_some() && response.clicked() {
                                                    toggle_selection = true;
                                                }
                                                if response.double_clicked()
                                                    && !capturing
                                                    && !renaming
                                                    && self.selection.is_none()
                                                {
                                                    rename = Some((index, sound.name.clone()));
                                                }
                                            }
                                        },
                                    );
                                });
                            });
                            if toggle_selection && let Some(selection) = &mut self.selection {
                                selection.toggle(
                                    self.model.library(),
                                    &indices,
                                    row,
                                    ui.input(|input| input.modifiers.shift),
                                );
                            }
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
                self.play_sound(ui.ctx(), index, hotkeys);
            }
            if let Some(index) = remove {
                self.pending_delete_index = None;
                let id = self.model.library().sounds[index].id;
                self.model
                    .remove(&HashSet::from([id]), super::wake_ui(ui.ctx().clone()));
            }
            if let Some(target) = capture {
                if capture_target == Some(target) {
                    hotkeys.cancel_capture();
                } else {
                    hotkeys.begin_capture(target, &mut self.model);
                }
                ui.ctx().request_repaint();
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
            let mut bind_selected = false;
            let mut clear_selected = false;
            let mut move_selected = None;
            let mut rename_selected = false;
            let mut export_selected = false;
            if let Some(selection) = &mut self.selection {
                let actions = ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !selection.ids.is_empty() && !hotkeys.batch_capturing(),
                            egui::Button::new(Text::ClearSelection.text()),
                        )
                        .clicked()
                    {
                        *selection = SoundSelection::default();
                    }
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center).with_main_wrap(true),
                        |ui| {
                            if ui
                                .add_enabled(
                                    !selection.ids.is_empty() && !hotkeys.batch_capturing(),
                                    egui::Button::new(
                                        Message::DeleteSelectedSounds {
                                            count: selection.ids.len(),
                                        }
                                        .text(),
                                    ),
                                )
                                .clicked()
                            {
                                selection.confirming_delete = true;
                            }
                            if hotkeys.batch_capturing() {
                                if ui
                                    .button(Text::PressKey.text())
                                    .on_hover_text(Text::CaptureHotkeyHint.text())
                                    .clicked()
                                {
                                    hotkeys.cancel_capture();
                                }
                            } else {
                                ui.add_enabled(!selection.ids.is_empty(), |ui: &mut egui::Ui| {
                                    ui.menu_button(Text::Hotkeys.text(), |ui| {
                                        if ui
                                            .button(Text::BindHotkey.text())
                                            .on_hover_text(Text::BatchHotkeyHint.text())
                                            .clicked()
                                        {
                                            bind_selected = true;
                                            ui.close();
                                        }
                                        let has_hotkey =
                                            self.model.library().sounds.iter().any(|sound| {
                                                selection.ids.contains(&sound.id)
                                                    && sound.hotkey.is_some()
                                            });
                                        if ui
                                            .add_enabled(
                                                has_hotkey,
                                                egui::Button::new(Text::ClearHotkey.text()),
                                            )
                                            .clicked()
                                        {
                                            clear_selected = true;
                                            ui.close();
                                        }
                                    })
                                    .response
                                });
                            }
                            let enabled = !selection.ids.is_empty() && !hotkeys.batch_capturing();
                            rename_selected = ui
                                .add_enabled(enabled, egui::Button::new(Text::BatchRename.text()))
                                .clicked();
                            ui.add_enabled(enabled, |ui: &mut egui::Ui| {
                                ui.menu_button(Text::MoveSounds.text(), |ui| {
                                    if ui.button(Text::MoveToTop.text()).clicked() {
                                        move_selected = Some(true);
                                        ui.close();
                                    }
                                    if ui.button(Text::MoveToBottom.text()).clicked() {
                                        move_selected = Some(false);
                                        ui.close();
                                    }
                                })
                                .response
                            });
                            export_selected = ui
                                .add_enabled(enabled, egui::Button::new(Text::ExportSounds.text()))
                                .clicked();
                        },
                    );
                });
                let height = actions.response.rect.height();
                if self.batch_actions_height != height {
                    self.batch_actions_height = height;
                    ui.ctx().request_discard("batch actions height changed");
                }
            }
            if let Some(selection) = &self.selection {
                if export_selected {
                    self.model.stop();
                    let sounds: Vec<_> = self
                        .model
                        .library()
                        .sounds
                        .iter()
                        .filter(|sound| selection.ids.contains(&sound.id))
                        .cloned()
                        .collect();
                    if !jobs.begin(ui.ctx(), Text::ExportSounds, move || {
                        JobResult::Exported(super::picker::choose_folder().and_then(|folder| {
                            folder.map_or(Ok(()), |folder| {
                                crate::app::soundboard::export::export_sounds(&sounds, &folder)
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
                if let Some(to_top) = move_selected {
                    match self.model.move_sounds(&selection.ids, to_top) {
                        Ok(()) => {
                            self.model.message.clear();
                            self.model.failed = false;
                        }
                        Err(error) => self.model.set_error(Message::error(error)),
                    }
                }
                if rename_selected {
                    self.batch_rename = Some(super::batch_rename::BatchRename::new(
                        self.model.library(),
                        selection.ids.clone(),
                    ));
                }
                if bind_selected {
                    hotkeys.begin_batch_capture(&selection.ids, &mut self.model);
                    ui.ctx().request_repaint();
                }
                if clear_selected {
                    match self.model.bind_sound_hotkeys(&selection.ids, None) {
                        Ok(()) => hotkeys.message.clear(),
                        Err(error) => {
                            tracing::error!("Failed to clear selected hotkeys: {error:#}");
                            hotkeys.message = Message::error(error);
                        }
                    }
                }
            }
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
                } else if let Some(index) = self.model.active_sound_index {
                    let sound = self.model.library().sounds.get(index);
                    let name = sound.map(|sound| sound.name.as_str()).unwrap_or("");
                    let duration_text = self
                        .model
                        .duration_secs
                        .or(sound.map(|sound| sound.duration_secs))
                        .filter(|duration_secs| duration_secs.is_finite() && *duration_secs > 0.0)
                        .map(format_duration)
                        .unwrap_or_else(|| "…".into());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(format!(
                            "{} / {duration_text}",
                            format_duration(self.model.elapsed_secs)
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
        });
        let available = available && !self.interaction_blocked(jobs);
        self.batch_delete_ui(ui.ctx(), available);
        if let Some(tool) = &mut self.batch_rename
            && let Some(selection) = &mut self.selection
            && tool.ui(ui.ctx(), &mut self.model, &mut selection.ids, available)
        {
            selection.anchor = None;
            self.batch_rename = None;
        }
    }

    fn batch_delete_ui(&mut self, ctx: &egui::Context, available: bool) {
        let Some(selection) = &mut self.selection else {
            return;
        };
        if !selection.confirming_delete {
            return;
        }
        let mut delete = false;
        let response = egui::Modal::new(egui::Id::unique("batch_delete_sounds"))
            .frame(egui::Frame::popup(&ctx.style_of(ctx.theme())).inner_margin(16))
            .show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 64.0).clamp(180.0, 400.0));
                ui.heading(
                    egui::RichText::new(Text::ConfirmDeleteSound.text())
                        .size(18.0)
                        .strong(),
                );
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(
                        Message::ConfirmDeleteSounds {
                            count: selection.ids.len(),
                        }
                        .text(),
                    )
                    .wrap(),
                );
                ui.spacing_mut().scroll.floating = false;
                egui::ScrollArea::vertical()
                    .id_salt("batch_delete_names")
                    .max_height((ctx.content_rect().height() - 240.0).clamp(60.0, 240.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for sound in &self.model.library().sounds {
                            if selection.ids.contains(&sound.id) {
                                ui.add(egui::Label::new(&sound.name).wrap().selectable(true));
                            }
                        }
                    });
                ui.add_space(12.0);
                ui.horizontal_wrapped(|ui| {
                    delete = ui
                        .add_enabled(
                            available,
                            egui::Button::new(
                                Message::DeleteSelectedSounds {
                                    count: selection.ids.len(),
                                }
                                .text(),
                            ),
                        )
                        .clicked();
                    if ui.button(Text::Cancel.text()).clicked() {
                        ui.close();
                    }
                });
            });
        if delete {
            self.model
                .remove(&selection.ids, super::wake_ui(ctx.clone()));
        }
        if delete || response.should_close() {
            selection.confirming_delete = false;
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
        if self.draft_settings.is_none() {
            return;
        }
        let defaults = Settings {
            language: self.model.settings.language,
            theme: self.model.settings.theme,
            ..Settings::default()
        };
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
                        close = icon_button(ui, idle && !capturing, ButtonIcon::Close).clicked();
                    });
                });
                ui.add_space(12.0);
                egui::ScrollArea::vertical()
                    .max_height((available.y - 230.0).max(80.0))
                    .show(ui, |ui| {
                        ui.add_enabled_ui(idle, |ui| {
                            let settings = self.draft_settings.as_mut().unwrap();
                            ui.add_enabled_ui(!capturing, |ui| {
                                playback_volume(
                                    ui,
                                    Text::MicrophoneOutput.text(),
                                    &mut settings.playback_microphone_output_volume,
                                );
                                output_device_picker(
                                    ui,
                                    "sound_microphone",
                                    &mut settings.playback_microphone_output_device,
                                    &self.microphones,
                                );
                                ui.add_space(8.0);
                                playback_volume(
                                    ui,
                                    Text::SpeakerOutput.text(),
                                    &mut settings.playback_speaker_output_volume,
                                );
                                output_device_picker(
                                    ui,
                                    "sound_speaker",
                                    &mut settings.playback_speaker_output_device,
                                    &self.speakers,
                                );
                                ui.add_space(8.0);
                                ui.checkbox(
                                    &mut settings.playback_include_microphone_audio,
                                    Text::IncludeMicrophoneAudio.text(),
                                );
                                ui.checkbox(
                                    &mut settings.playback_show_overlay,
                                    Text::ShowOverlay.text(),
                                );
                                ui.checkbox(
                                    &mut settings.playback_repeat,
                                    Text::RepeatPlayback.text(),
                                );
                            });
                            if hotkey_editor(
                                ui,
                                Text::RandomHotkey.text(),
                                &mut settings.playback_random_hotkey,
                                BindingTarget::Random,
                                capture_target,
                            ) {
                                capture = Some(BindingTarget::Random);
                            }
                            if hotkey_editor(
                                ui,
                                Text::StopHotkey.text(),
                                &mut settings.playback_stop_hotkey,
                                BindingTarget::Stop,
                                capture_target,
                            ) {
                                capture = Some(BindingTarget::Stop);
                            }
                            if hotkey_editor(
                                ui,
                                Text::HoldDuringPlayback.text(),
                                &mut settings.playback_auto_hold_hotkey,
                                BindingTarget::Hold,
                                capture_target,
                            ) {
                                capture = Some(BindingTarget::Hold);
                            }
                            if !hotkeys.settings_message.is_empty() {
                                warning(ui, &hotkeys.settings_message.text());
                            }
                        });
                    });
                ui.add_space(12.0);
                let dirty = self
                    .draft_settings
                    .as_ref()
                    .is_some_and(|settings| *settings != self.model.settings);
                ui.horizontal_wrapped(|ui| {
                    save = ui
                        .add_enabled(
                            idle && !capturing && dirty,
                            egui::Button::new(Text::SaveChanges.text()),
                        )
                        .clicked();
                    if ui
                        .add_enabled(
                            idle && !capturing && dirty,
                            egui::Button::new(Text::UndoChanges.text()),
                        )
                        .clicked()
                    {
                        self.draft_settings = Some(self.model.settings.clone());
                        hotkeys.settings_message.clear();
                    }
                    if trailing_button(
                        ui,
                        idle && !capturing
                            && self
                                .draft_settings
                                .as_ref()
                                .is_some_and(|settings| *settings != defaults),
                        Text::RestoreDefaults.text(),
                    )
                    .clicked()
                    {
                        self.draft_settings = Some(defaults.clone());
                        hotkeys.settings_message.clear();
                    }
                });
                if self
                    .draft_settings
                    .as_ref()
                    .is_some_and(|settings| *settings != self.model.settings)
                {
                    warning(ui, Text::UnsavedSoundSettings.text());
                } else {
                    hint(ui, Text::SoundSettingsApplied.text());
                }
                if jobs.failed {
                    warning(ui, &jobs.status.text());
                }
            });
        // Ignore backdrop clicks so they do not discard unsaved settings.
        if close
            || (!response.backdrop_response.clicked()
                && response.should_close()
                && !capturing
                && idle)
        {
            self.draft_settings = None;
            hotkeys.cancel_capture();
            hotkeys.settings_message.clear();
        } else if save {
            self.save_soundboard_settings(jobs, hotkeys);
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
        if !edit.request_focus && (!soundboard_tab_selected || self.draft_settings.is_some()) {
            edit.save_requested = true;
        }
        // Stopping playback is asynchronous; keep the edit until it finishes.
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

    pub(super) fn finish_delete(&mut self) {
        if let Some(removed) = self.model.take_removed_sounds() {
            if let Some(selection) = &mut self.selection {
                selection.ids.retain(|id| !removed.contains(id));
                selection.anchor = None;
            }
            self.pending_delete_index = None;
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

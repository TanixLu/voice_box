//! List interaction state. Rendering only returns intents; it never mutates the soundboard.

mod row;

use std::collections::HashSet;

use crate::{
    i18n::{Message, Text},
    settings::BindingTarget,
    soundboard::sound::{Sound, SoundId},
};

#[derive(Clone, Copy)]
pub struct Status {
    pub active_sound: Option<SoundId>,
    pub capture_target: Option<BindingTarget>,
    pub batch_capturing: bool,
}

pub enum Action {
    Play(usize),
    Stop,
    Remove(SoundId),
    Capture(BindingTarget),
    ClearHotkey(usize),
    Move { from: usize, to: usize },
}
pub enum BatchAction {
    Export,
    Move(bool),
    Rename,
    BindHotkey,
    ClearHotkey,
}

#[derive(Default)]
struct RowActions {
    play: Option<usize>,
    stop: bool,
    remove: Option<usize>,
    delete_clicked: bool,
    capture: Option<BindingTarget>,
    rename: Option<(usize, String)>,
    clear_hotkey: Option<usize>,
    rename_visible: bool,
    reorder: Option<(usize, usize)>,
}

pub struct SoundList {
    filter: String,
    rename_editor: Option<row::RenameEditor>,
    pending_delete_index: Option<usize>,
    selection: Option<SoundSelection>,
    batch_actions_height: f32,
}

impl Default for SoundList {
    fn default() -> Self {
        Self {
            filter: String::new(),
            rename_editor: None,
            pending_delete_index: None,
            selection: None,
            batch_actions_height: 30.0,
        }
    }
}

#[derive(Default)]
struct SoundSelection {
    ids: HashSet<SoundId>,
    anchor: Option<SoundId>,
    confirming_delete: bool,
}

impl SoundSelection {
    fn toggle(&mut self, sounds: &[Sound], indices: &[usize], row: usize, extend: bool) {
        let id = &sounds[indices[row]].id;
        let selected = !self.ids.contains(id);
        let start = if extend {
            indices
                .iter()
                .position(|&index| self.anchor.as_ref() == Some(&sounds[index].id))
                .unwrap_or(row)
        } else {
            row
        };
        for &index in &indices[start.min(row)..=start.max(row)] {
            let id = &sounds[index].id;
            if selected {
                self.ids.insert(*id);
            } else {
                self.ids.remove(id);
            }
        }
        self.anchor = Some(*id);
    }
}

impl SoundList {
    pub fn renaming(&self) -> bool {
        self.rename_editor.is_some()
    }
    pub fn cancel_rename(&mut self) {
        self.rename_editor = None;
    }
    pub fn cancel_delete(&mut self) {
        self.pending_delete_index = None;
    }
    pub fn batch_mode(&self) -> bool {
        self.selection.is_some()
    }
    pub fn begin_batch(&mut self) {
        self.cancel_delete();
        self.selection = Some(SoundSelection::default());
    }
    pub fn cancel_batch(&mut self) {
        self.selection = None;
    }
    pub fn selected(&self) -> Option<&HashSet<SoundId>> {
        self.selection.as_ref().map(|s| &s.ids)
    }
    pub fn selected_mut(&mut self) -> Option<&mut HashSet<SoundId>> {
        self.selection.as_mut().map(|s| &mut s.ids)
    }
    pub fn clear_anchor(&mut self) {
        if let Some(selection) = &mut self.selection {
            selection.anchor = None;
        }
    }
    pub fn remove_ids(&mut self, ids: &HashSet<SoundId>) {
        if let Some(selection) = &mut self.selection {
            selection.ids.retain(|id| !ids.contains(id));
            selection.anchor = None;
        }
        self.cancel_delete();
    }
    pub fn rename_request(&mut self, keep_editing: bool, playing: bool) -> Option<(usize, String)> {
        let edit = self.rename_editor.as_mut()?;
        if !edit.request_focus && !keep_editing {
            edit.save_requested = true;
        }
        (edit.save_requested && !playing).then(|| (edit.index, edit.name.clone()))
    }
    pub fn rename_finished(&mut self, success: bool) {
        if success {
            self.rename_editor = None;
        } else if let Some(edit) = &mut self.rename_editor {
            edit.save_requested = false;
            edit.request_focus = true;
        }
    }
    pub fn ui(&mut self, ui: &mut egui::Ui, sounds: &[Sound], status: Status) -> Vec<Action> {
        let (filter, indices) = self.filter_ui(ui, sounds);
        self.selection_ui(ui, sounds, &indices);
        let RowActions {
            play,
            stop,
            remove,
            delete_clicked,
            capture,
            rename,
            clear_hotkey,
            rename_visible,
            reorder,
        } = self.rows_ui(ui, sounds, &indices, status, &filter);
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
            let text = if sounds.is_empty() {
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
        let mut actions = Vec::new();
        if let Some(index) = clear_hotkey {
            actions.push(Action::ClearHotkey(index));
        }
        if stop {
            actions.push(Action::Stop);
        } else if let Some(index) = play {
            actions.push(Action::Play(index));
        }
        if let Some(index) = remove {
            self.pending_delete_index = None;
            actions.push(Action::Remove(sounds[index].id));
        }
        if let Some(target) = capture {
            actions.push(Action::Capture(target));
            ui.ctx().request_repaint();
        }
        if let Some((index, name)) = rename {
            actions.push(Action::Stop);
            self.pending_delete_index = None;
            self.rename_editor = Some(row::RenameEditor {
                index,
                name,
                request_focus: true,
                save_requested: false,
            });
            ui.ctx().request_repaint();
        }
        if let Some((from, to)) = reorder {
            self.pending_delete_index = None;
            actions.push(Action::Move { from, to });
            ui.ctx().request_repaint();
        }
        actions
    }

    fn filter_ui(&mut self, ui: &mut egui::Ui, sounds: &[Sound]) -> (String, Vec<usize>) {
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
        let indices: Vec<_> = sounds
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
        (filter, indices)
    }

    fn selection_ui(&mut self, ui: &mut egui::Ui, sounds: &[Sound], indices: &[usize]) {
        if let Some(selection) = &mut self.selection {
            ui.horizontal_wrapped(|ui| {
                let mut visible_selected = indices
                    .iter()
                    .filter(|&&index| selection.ids.contains(&sounds[index].id))
                    .count();
                let mut all = !indices.is_empty() && visible_selected == indices.len();
                let select_all = ui
                    .add_enabled(
                        !indices.is_empty(),
                        egui::Checkbox::new(&mut all, Text::SelectSearchResults.text())
                            .indeterminate(
                                visible_selected > 0 && visible_selected < indices.len(),
                            ),
                    )
                    .clicked();
                let shortcut = ui.is_enabled()
                    && !selection.confirming_delete
                    && ui.ctx().memory(|memory| memory.top_modal_layer().is_none())
                    && !ui.ctx().text_edit_focused()
                    && ui.input_mut(|input| {
                        input.consume_key(egui::Modifiers::COMMAND, egui::Key::A)
                    });
                if select_all || shortcut {
                    for &index in indices {
                        let id = &sounds[index].id;
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
    }

    fn rows_ui(
        &mut self,
        ui: &mut egui::Ui,
        sounds: &[Sound],
        indices: &[usize],
        status: Status,
        filter: &str,
    ) -> RowActions {
        let mut result = RowActions::default();
        let renaming = self.rename_editor.is_some();
        let can_reorder =
            ui.is_enabled() && self.selection.is_none() && filter.is_empty() && !renaming;
        if !can_reorder {
            egui::DragAndDrop::clear_payload(ui.ctx());
        }
        let footer_height = if self.selection.is_some() {
            58.0 + self.batch_actions_height
        } else {
            50.0
        };
        let height = (ui.available_height() - footer_height).max(50.0);
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
                        let sound = &sounds[index];
                        let active = status.active_sound == Some(sound.id);
                        let edit = self
                            .rename_editor
                            .as_mut()
                            .filter(|edit| edit.index == index);
                        result.rename_visible |= edit.is_some();
                        let response = row::show(
                            ui,
                            row::Row {
                                sound,
                                index,
                                last_index: indices.len() - 1,
                                selected: self
                                    .selection
                                    .as_ref()
                                    .map(|selection| selection.ids.contains(&sound.id)),
                                confirming_delete: self.pending_delete_index == Some(index),
                                can_reorder,
                                filter_empty: filter.is_empty(),
                                renaming,
                                status,
                            },
                            edit,
                        );
                        for action in response.actions {
                            match action {
                                row::Action::TogglePlayback => {
                                    if active {
                                        result.stop = true;
                                    } else {
                                        result.play = Some(index);
                                    }
                                }
                                row::Action::Delete => {
                                    result.delete_clicked = true;
                                    if self.pending_delete_index == Some(index) {
                                        result.remove = Some(index);
                                    } else {
                                        self.pending_delete_index = Some(index);
                                    }
                                }
                                row::Action::ClearHotkey => result.clear_hotkey = Some(index),
                                row::Action::Capture => {
                                    result.capture = Some(BindingTarget::Sound(index));
                                }
                                row::Action::Rename => {
                                    result.rename = Some((index, sound.name.clone()));
                                }
                                row::Action::MoveTo(to) => result.reorder = Some((index, to)),
                            }
                        }
                        let toggle_selection = response.toggle_selection;
                        if toggle_selection && let Some(selection) = &mut self.selection {
                            selection.toggle(
                                sounds,
                                indices,
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
                                .clamp(ui.clip_rect().top() + 1.0, ui.clip_rect().bottom() - 1.0);
                            ui.painter().hline(
                                ui.max_rect().x_range(),
                                y,
                                egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
                            );
                        }
                        if ui.input(|input| input.pointer.primary_released()) {
                            result.reorder = Some((*from, to));
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
        result
    }

    pub fn batch_ui(
        &mut self,
        ui: &mut egui::Ui,
        sounds: &[Sound],
        status: Status,
    ) -> Vec<BatchAction> {
        let mut bind_selected = false;
        let mut clear_selected = false;
        let mut move_selected = None;
        let mut rename_selected = false;
        let mut export_selected = false;
        if let Some(selection) = &mut self.selection {
            let actions = ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !selection.ids.is_empty(),
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
                                !selection.ids.is_empty(),
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
                        if status.batch_capturing {
                            ui.button(Text::PressKey.text())
                                .show_tooltip_text(Text::CaptureHotkeyHint.text());
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
                                    let has_hotkey = sounds.iter().any(|sound| {
                                        selection.ids.contains(&sound.id) && sound.hotkey.is_some()
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
                        let enabled = !selection.ids.is_empty();
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
        let mut actions = Vec::new();
        if export_selected {
            actions.push(BatchAction::Export);
        }
        if let Some(to_top) = move_selected {
            actions.push(BatchAction::Move(to_top));
        }
        if rename_selected {
            actions.push(BatchAction::Rename);
        }
        if bind_selected {
            actions.push(BatchAction::BindHotkey);
        }
        if clear_selected {
            actions.push(BatchAction::ClearHotkey);
        }
        actions
    }

    pub fn delete_ui(
        &mut self,
        ctx: &egui::Context,
        sounds: &[Sound],
        available: bool,
    ) -> Option<HashSet<SoundId>> {
        let Some(selection) = &mut self.selection else {
            return None;
        };
        if !selection.confirming_delete {
            return None;
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
                        for sound in sounds {
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
        let removed = delete.then(|| selection.ids.clone());
        if delete || response.should_close() {
            selection.confirming_delete = false;
        }
        removed
    }
}

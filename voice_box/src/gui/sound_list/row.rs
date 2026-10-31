//! A row renders sound data and emits gestures; it owns no library or playback state.

use crate::{
    gui::{
        sound_list::Status,
        widgets::{ButtonIcon, format_duration, hotkey_badge, icon_button, play_button},
    },
    i18n::{Message, Text},
    settings::BindingTarget,
    soundboard::sound::Sound,
};

#[derive(Clone, Copy)]
pub struct Row<'a> {
    pub sound: &'a Sound,
    pub index: usize,
    pub last_index: usize,
    pub selected: Option<bool>,
    pub confirming_delete: bool,
    pub can_reorder: bool,
    pub filter_empty: bool,
    pub renaming: bool,
    pub status: Status,
}

pub enum Action {
    TogglePlayback,
    Delete,
    ClearHotkey,
    Capture,
    Rename,
    MoveTo(usize),
}
#[derive(Default)]
pub struct Response {
    pub actions: Vec<Action>,
    pub toggle_selection: bool,
}

pub struct RenameEditor {
    pub index: usize,
    pub name: String,
    pub request_focus: bool,
    pub save_requested: bool,
}

impl RenameEditor {
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

pub fn show(ui: &mut egui::Ui, row: Row<'_>, edit: Option<&mut RenameEditor>) -> Response {
    let sound = row.sound;
    let index = row.index;
    let can_reorder = row.can_reorder;
    let renaming = row.renaming;
    let active = row.status.active_sound == Some(sound.id);
    let capture_target = row.status.capture_target;
    let mut response = Response::default();
    ui.push_id(sound.id, |ui| {
        ui.horizontal(|ui| {
            if let Some(mut selected) = row.selected {
                let checkbox = ui
                    .add_sized([30.0, 30.0], egui::Checkbox::without_text(&mut selected))
                    .on_hover_text(Text::SelectSoundHint.text());
                checkbox.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::Role::CheckBox,
                        ui.is_enabled(),
                        selected,
                        &sound.name,
                    )
                });
                response.toggle_selection = checkbox.clicked();
            } else {
                let handle = icon_button(ui, can_reorder, ButtonIcon::Reorder)
                    .on_hover_cursor(egui::CursorIcon::Grab)
                    .on_disabled_hover_text(if row.filter_empty {
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
                            (Text::MoveSoundToBottom, row.last_index),
                        ] {
                            if ui
                                .add_enabled(index != to, egui::Button::new(label.text()))
                                .clicked()
                            {
                                response.actions.push(Action::MoveTo(to));
                                ui.close();
                            }
                        }
                    });
                }
            }
            if play_button(ui, !renaming, active)
                .on_hover_text(if active {
                    Text::StopPlayback.text()
                } else {
                    Text::PlaySound.text()
                })
                .clicked()
            {
                response.actions.push(Action::TogglePlayback);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if row.selected.is_none() {
                    let confirming = row.confirming_delete;
                    let icon = if confirming {
                        ButtonIcon::ConfirmDelete
                    } else {
                        ButtonIcon::Delete
                    };
                    if icon_button(ui, !renaming, icon).clicked() {
                        ui.ctx().request_repaint();
                        response.actions.push(Action::Delete);
                    }
                }
                ui.add_sized(
                    [48.0, 30.0],
                    egui::Label::new(format_duration(sound.duration_secs)).truncate(),
                )
                .on_hover_text(Text::AudioLength.text());
                if let Some(key) = sound.hotkey {
                    if row.selected.is_none()
                        && icon_button(ui, !renaming, ButtonIcon::ClearHotkey).clicked()
                    {
                        response.actions.push(Action::ClearHotkey);
                    }
                    let max_width = (ui.available_width() - 74.0).max(24.0);
                    hotkey_badge(ui, key, max_width);
                } else if row.selected.is_none() {
                    let width = (ui.available_width() - 74.0).clamp(24.0, 132.0);
                    let binding = capture_target == Some(BindingTarget::Sound(index));
                    let label = if binding {
                        Text::PressKey.text()
                    } else {
                        Text::BindHotkey.text()
                    };
                    let button_response = ui
                        .add_enabled_ui(!renaming, |ui| {
                            ui.add_sized([width, 30.0], egui::Button::new(label).truncate())
                        })
                        .inner
                        .on_hover_text(Text::BindHotkey.text());
                    if binding {
                        button_response.show_tooltip_text(Text::CaptureHotkeyHint.text());
                    }
                    if button_response.clicked() {
                        response.actions.push(Action::Capture);
                    }
                }
                if let Some(edit) = edit {
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
                    let name_response = ui
                        .add_sized(
                            [ui.available_width().max(16.0), 30.0],
                            egui::Button::new(())
                                .left_text(name)
                                .frame_when_inactive(false)
                                .selected(row.selected.unwrap_or(false))
                                .truncate(),
                        )
                        .on_hover_text(if row.selected.is_some() {
                            format!("{}\n{}", sound.name, Text::SelectSoundHint.text())
                        } else {
                            Message::RenameSoundHint {
                                name: sound.name.clone(),
                            }
                            .text()
                            .into_owned()
                        });
                    if row.selected.is_some() && name_response.clicked() {
                        response.toggle_selection = true;
                    }
                    if name_response.double_clicked() && !renaming && row.selected.is_none() {
                        response.actions.push(Action::Rename);
                    }
                }
            });
        });
    });
    response
}

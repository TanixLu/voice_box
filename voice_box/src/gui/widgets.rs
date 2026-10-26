use crate::i18n::Text;
use crate::{hotkeys::Hotkey, settings::BindingTarget};
pub(super) const ICON_BUTTON_SIZE: f32 = 30.0;

/// Right-align a trailing button when it fits, otherwise let the row wrap it.
/// Call inside `Ui::horizontal_wrapped`.
pub(super) fn trailing_button(ui: &mut egui::Ui, enabled: bool, label: &str) -> egui::Response {
    let button_width = ui
        .painter()
        .layout_no_wrap(
            label.into(),
            egui::TextStyle::Button.resolve(ui.style()),
            ui.visuals().text_color(),
        )
        .size()
        .x
        + 2.0 * ui.spacing().button_padding.x;
    // Wrapping layouts report the full row in available_width(); use the
    // remaining space so the spacer cannot expand the section.
    let spacer =
        (ui.available_size_before_wrap().x - button_width - ui.spacing().item_spacing.x).max(0.0);
    ui.add_space(spacer);
    ui.add_enabled(enabled, egui::Button::new(label))
}

#[derive(Clone, Copy)]
pub(super) enum ButtonIcon {
    Close,
    ClearHotkey,
    Delete,
    ConfirmDelete,
    Reorder,
}

pub(super) fn icon_button(ui: &mut egui::Ui, enabled: bool, icon: ButtonIcon) -> egui::Response {
    let mut button = egui::Button::new("").min_size(egui::Vec2::splat(ICON_BUTTON_SIZE));
    if matches!(icon, ButtonIcon::Close) {
        button = button.frame_when_inactive(false);
    }
    if matches!(icon, ButtonIcon::Reorder) {
        button = button
            .frame_when_inactive(false)
            .sense(egui::Sense::click_and_drag());
    }
    let confirming = matches!(icon, ButtonIcon::ConfirmDelete);
    if confirming {
        let color = ui.visuals().error_fg_color;
        button = button
            .fill(color.gamma_multiply(0.15))
            .stroke(egui::Stroke::new(1.0, color));
    }
    let response = ui.add_enabled(enabled, button);
    let color = if confirming && response.enabled() {
        ui.visuals().error_fg_color
    } else {
        ui.style().interact(&response).fg_stroke.color
    };
    let stroke = egui::Stroke::new(1.5, color);
    let center = response.rect.center();
    let point = |x, y| center + egui::vec2(x, y);
    let painter = ui.painter();
    let (label, tooltip) = match icon {
        ButtonIcon::Reorder => {
            for x in [-3.0, 3.0] {
                for y in [-6.0, 0.0, 6.0] {
                    painter.circle_filled(point(x, y), 1.5, color);
                }
            }
            (Text::ReorderSound.text(), Text::ReorderSoundHint.text())
        }
        ButtonIcon::Close | ButtonIcon::ClearHotkey => {
            painter.line_segment([point(-4.0, -4.0), point(4.0, 4.0)], stroke);
            painter.line_segment([point(-4.0, 4.0), point(4.0, -4.0)], stroke);
            if matches!(icon, ButtonIcon::Close) {
                (Text::Close.text(), Text::CloseDiscardChanges.text())
            } else {
                (Text::ClearHotkey.text(), Text::ClearHotkey.text())
            }
        }
        ButtonIcon::Delete => {
            painter.line_segment([point(-6.0, -4.0), point(6.0, -4.0)], stroke);
            painter.add(egui::Shape::line(
                vec![
                    point(-3.0, -4.0),
                    point(-3.0, -7.0),
                    point(3.0, -7.0),
                    point(3.0, -4.0),
                ],
                stroke,
            ));
            painter.add(egui::Shape::line(
                vec![
                    point(-5.0, -3.0),
                    point(-4.0, 7.0),
                    point(4.0, 7.0),
                    point(5.0, -3.0),
                ],
                stroke,
            ));
            painter.line_segment([point(-2.0, -1.0), point(-1.5, 4.0)], stroke);
            painter.line_segment([point(2.0, -1.0), point(1.5, 4.0)], stroke);
            (Text::DeleteSound.text(), Text::DeleteSoundHint.text())
        }
        ButtonIcon::ConfirmDelete => {
            painter.add(egui::Shape::line(
                vec![point(-5.0, 0.0), point(-1.0, 4.0), point(6.0, -4.0)],
                stroke,
            ));
            (
                Text::ConfirmDeleteSound.text(),
                Text::ConfirmDeleteSoundHint.text(),
            )
        }
    };
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, response.enabled(), label));
    response.on_hover_text(tooltip)
}

pub(super) fn play_button(ui: &mut egui::Ui, enabled: bool, active: bool) -> egui::Response {
    let response = ui.add_enabled(
        enabled,
        egui::Button::new("").min_size(egui::vec2(34.0, 30.0)),
    );
    let color = ui.style().interact(&response).fg_stroke.color;
    let center = response.rect.center();
    if active {
        ui.painter().rect_filled(
            egui::Rect::from_center_size(center, egui::vec2(11.0, 11.0)),
            0.0,
            color,
        );
    } else {
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                center + egui::vec2(-4.0, -6.0),
                center + egui::vec2(-4.0, 6.0),
                center + egui::vec2(6.0, 0.0),
            ],
            color,
            egui::Stroke::NONE,
        ));
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::Role::Button,
            enabled,
            if active {
                Text::StopPlayback.text()
            } else {
                Text::PlaySound.text()
            },
        )
    });
    response
}

pub(super) fn section(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .inner_margin(12)
        .corner_radius(8)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            contents(ui);
        });
}

pub(super) fn hint(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).small().weak()).wrap());
}

pub(super) fn warning(ui: &mut egui::Ui, text: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .small()
                .color(ui.visuals().warn_fg_color),
        )
        .wrap(),
    );
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

fn hotkey_badge_size(ui: &egui::Ui, key: Hotkey, max_width: f32) -> egui::Vec2 {
    let text_size = ui
        .painter()
        .layout_no_wrap(
            key.label(),
            egui::TextStyle::Body.resolve(ui.style()),
            ui.visuals().text_color(),
        )
        .size();
    egui::vec2(
        (text_size.x + 18.0).clamp(24.0, max_width.max(24.0)),
        text_size.y + 10.0,
    )
}

pub(super) fn hotkey_badge(ui: &mut egui::Ui, key: Hotkey, max_width: f32) {
    let text = key.label();
    let font = egui::TextStyle::Body.resolve(ui.style());
    let size = hotkey_badge_size(ui, key, max_width);
    // Allocate and paint only the text's measured width. A Frame inherits the
    // centered column's bounds and can stretch its border across the whole row.
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect(
        rect,
        4,
        ui.visuals().extreme_bg_color,
        ui.visuals().widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    );
    ui.place(
        rect.shrink2(egui::vec2(9.0, 5.0)),
        egui::Label::new(egui::RichText::new(&text).font(font))
            .truncate()
            .show_tooltip_when_elided(false),
    );
    response.on_hover_text(text);
}

pub(super) fn hotkey_editor(
    ui: &mut egui::Ui,
    label: &str,
    key: &mut Option<Hotkey>,
    target: BindingTarget,
    capture_target: Option<BindingTarget>,
) -> bool {
    let binding = capture_target == Some(target);
    let enabled = capture_target.is_none() || binding;
    ui.push_id(label, |ui| {
        ui.horizontal(|ui| {
            ui.label(label);
            let mut capture = false;
            let hotkey = *key;
            let button_text = if binding {
                Text::PressKey.text()
            } else {
                Text::BindHotkey.text()
            };
            let available_width = ui.available_width();
            let button_width = if hotkey.is_some() {
                ICON_BUTTON_SIZE
            } else {
                (ui.painter()
                    .layout_no_wrap(
                        button_text.into(),
                        egui::TextStyle::Button.resolve(ui.style()),
                        ui.visuals().text_color(),
                    )
                    .size()
                    .x
                    + 2.0 * ui.spacing().button_padding.x)
                    .min(available_width)
            };
            let badge_width = hotkey.map(|hotkey| {
                hotkey_badge_size(
                    ui,
                    hotkey,
                    available_width - button_width - ui.spacing().item_spacing.x,
                )
                .x
            });
            let group_width =
                button_width + badge_width.map_or(0.0, |width| width + ui.spacing().item_spacing.x);
            let (row_rect, _) =
                ui.allocate_exact_size(egui::vec2(available_width, 30.0), egui::Sense::hover());
            // Center the key and clear button together as one compact group.
            let group_rect =
                egui::Rect::from_center_size(row_rect.center(), egui::vec2(group_width, 30.0));
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(group_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    if let Some(hotkey) = hotkey {
                        hotkey_badge(ui, hotkey, badge_width.unwrap());
                        if icon_button(ui, enabled, ButtonIcon::ClearHotkey).clicked() {
                            *key = None;
                        }
                    } else {
                        capture = ui
                            .add_enabled(enabled, egui::Button::new(button_text).truncate())
                            .on_hover_text(if binding {
                                Text::CaptureHotkeyHint.text()
                            } else {
                                Text::BindHotkey.text()
                            })
                            .clicked();
                    }
                },
            );
            capture
        })
        .inner
    })
    .inner
}

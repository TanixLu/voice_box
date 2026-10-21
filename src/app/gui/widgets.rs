pub(super) const ICON_BUTTON_SIZE: f32 = 30.0;

#[derive(Clone, Copy)]
pub(super) enum ButtonIcon {
    ClearHotkey,
    Delete,
    ConfirmDelete,
}

pub(super) fn icon_button(ui: &mut egui::Ui, enabled: bool, icon: ButtonIcon) -> egui::Response {
    let mut button = egui::Button::new("").min_size(egui::Vec2::splat(ICON_BUTTON_SIZE));
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
        ButtonIcon::ClearHotkey => {
            painter.line_segment([point(-4.0, -4.0), point(4.0, 4.0)], stroke);
            painter.line_segment([point(-4.0, 4.0), point(4.0, -4.0)], stroke);
            ("清除快捷键", "清除快捷键")
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
            ("删除音效", "删除音效，点击后再次确认")
        }
        ButtonIcon::ConfirmDelete => {
            painter.add(egui::Shape::line(
                vec![point(-5.0, 0.0), point(-1.0, 4.0), point(6.0, -4.0)],
                stroke,
            ));
            (
                "确认删除音效",
                "再次点击确认删除音效及音效库副本，保留原文件；点击其他位置或按 Esc 取消",
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
                "停止播放"
            } else {
                "播放音效"
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

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

use crate::shared::paths;

pub(super) fn apply_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        style
            .text_styles
            .insert(egui::TextStyle::Heading, egui::FontId::proportional(24.0));
    });
}

pub(super) fn install_system_font(ctx: &egui::Context) {
    let Ok(windows) = paths::windows_dir() else {
        return;
    };
    let mut fonts = FontDefinitions::default();
    let chinese_font = ["msyh.ttc", "simsun.ttc"]
        .into_iter()
        .find(|file| windows.join("Fonts").join(file).is_file());
    for file in std::iter::once("segoeui.ttf").chain(chinese_font) {
        if let Ok(data) = std::fs::read(windows.join("Fonts").join(file)) {
            fonts
                .font_data
                .insert(file.into(), Arc::new(FontData::from_owned(data)));
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push(file.into());
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .push(file.into());
        }
    }
    if !fonts.font_data.is_empty() {
        ctx.set_fonts(fonts);
    }
}

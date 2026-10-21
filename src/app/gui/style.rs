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
    for file in ["msyh.ttc", "simsun.ttc", "segoeui.ttf"] {
        if let Ok(data) = std::fs::read(windows.join("Fonts").join(file)) {
            let mut fonts = FontDefinitions::empty();
            fonts
                .font_data
                .insert("system".into(), Arc::new(FontData::from_owned(data)));
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push("system".into());
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .push("system".into());
            ctx.set_fonts(fonts);
            break;
        }
    }
}

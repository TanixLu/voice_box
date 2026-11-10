//! Modals owned by the app shell itself, drawn as `VoiceBoxApp` methods.

use crate::{
    embedded::LICENSES,
    gui::{Busy, Lifecycle, Startup, VoiceBoxApp, widgets::notice},
    i18n::Text,
    task::Task,
    uninstall,
};

impl VoiceBoxApp {
    pub fn notice_ui(&mut self, ctx: &egui::Context) {
        if let Some((title, message)) = &self.notice
            && notice(ctx, *title, message)
        {
            self.notice = None;
        }
    }

    pub fn full_uninstall_ui(&mut self, ctx: &egui::Context, busy: Busy) {
        if !matches!(self.lifecycle, Lifecycle::ConfirmUninstall) {
            return;
        }
        let available = ctx.content_rect().size();
        let response = egui::Modal::new(egui::Id::unique("full_uninstall")).show(ctx, |ui| {
            ui.set_width((available.x - 64.0).clamp(180.0, 400.0));
            ui.heading(egui::RichText::new(Text::UninstallVoiceBox.text()).size(25.0));
            ui.add(egui::Label::new(Text::ConfirmFullUninstall.text()).wrap());
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(!busy.any(), egui::Button::new(Text::Uninstall.text()))
                    .clicked()
                {
                    self.startup = Startup::Unsynced;
                    let task = Task::spawn("VoiceBoxUninstall", &self.wake, uninstall::run);
                    self.lifecycle = Lifecycle::Uninstalling(task);
                }
                if ui.button(Text::Cancel.text()).clicked() {
                    ui.close();
                }
            });
        });
        if response.should_close() && matches!(self.lifecycle, Lifecycle::ConfirmUninstall) {
            self.lifecycle = Lifecycle::Running;
        }
    }

    pub fn licenses_ui(&mut self, ctx: &egui::Context) {
        if !self.show_licenses {
            return;
        }
        let available = ctx.content_rect().size();
        let response = egui::Modal::new(egui::Id::unique("copyright_notice")).show(ctx, |ui| {
            ui.set_width((available.x - 64.0).clamp(180.0, 760.0));
            ui.heading(Text::Licenses.text());
            ui.label(concat!(
                "VoiceBox v",
                env!("CARGO_PKG_VERSION"),
                " · MIT OR Apache-2.0"
            ));
            ui.horizontal_wrapped(|ui| {
                ui.label(Text::ProjectSource.text());
                ui.hyperlink("https://github.com/TanixLu/voice_box");
            });
            ui.separator();
            ui.spacing_mut().scroll.floating = false;
            ui.spacing_mut().scroll.bar_inner_margin = 14.0;
            // Lay out only the visible lines; one label for the whole notice costs tens of MB.
            let lines: Vec<&str> = LICENSES.lines().collect();
            let row_height = ui.text_style_height(&egui::TextStyle::Body);
            egui::ScrollArea::both()
                .id_salt("copyright_scroll")
                .max_height((available.y - 180.0).clamp(60.0, 360.0))
                .auto_shrink([false, false])
                .show_rows(ui, row_height, lines.len(), |ui, rows| {
                    for line in &lines[rows] {
                        ui.add(egui::Label::new(*line).extend());
                    }
                });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(Text::CopyAll.text()).clicked() {
                    ctx.copy_text(LICENSES.clone());
                }
                if ui.button(Text::Close.text()).clicked() {
                    ui.close();
                }
            });
        });
        if response.should_close() {
            self.show_licenses = false;
        }
    }
}

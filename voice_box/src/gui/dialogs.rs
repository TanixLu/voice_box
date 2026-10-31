use crate::{
    gui::{VoiceBoxApp, jobs::JobResult},
    i18n::Text,
    uninstall,
};

const LICENSES: &str = concat!(
    "VoiceBox v",
    env!("CARGO_PKG_VERSION"),
    "\nCopyright (c) 2026 Tanix\n\n",
    "Project source: https://github.com/TanixLu/voice_box\n\n",
    "Original VoiceBox code is licensed under MIT OR Apache-2.0, at your option.\n",
    "Third-party components and model weights retain their respective licenses.\n\n",
    "===== VoiceBox: MIT License =====\n\n",
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../LICENSE-MIT")),
    "\n===== VoiceBox: Apache License 2.0 =====\n\n",
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../LICENSE-APACHE")),
    "\n===== Third-party licenses and notices =====\n\n",
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../THIRD_PARTY_LICENSES"
    )),
);

impl VoiceBoxApp {
    pub fn startup_message_ui(&mut self, ctx: &egui::Context) {
        let Some(message) = &self.startup_message else {
            return;
        };
        let available = ctx.content_rect().size();
        let response = egui::Modal::new(egui::Id::unique("startup_message")).show(ctx, |ui| {
            ui.set_width((available.x - 64.0).clamp(180.0, 400.0));
            ui.heading(Text::Startup.text());
            ui.add(egui::Label::new(message.text()).wrap());
            ui.add_space(12.0);
            if ui.button(Text::Ok.text()).clicked() {
                ui.close();
            }
        });
        if response.should_close() {
            self.startup_message = None;
        }
    }

    pub fn full_uninstall_ui(&mut self, ctx: &egui::Context) {
        if !self.confirm_full_uninstall {
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
                    .add_enabled(
                        !self.interaction_blocked()
                            && !self.voice.session.busy()
                            && !self.soundboard.model.playing(),
                        egui::Button::new(Text::Uninstall.text()),
                    )
                    .clicked()
                {
                    self.confirm_full_uninstall = false;
                    self.startup_synchronized = false;
                    self.uninstalling = self.jobs.begin(ctx, Text::PreparingUninstall, || {
                        JobResult::Uninstall(uninstall::run())
                    });
                }
                if ui.button(Text::Cancel.text()).clicked() {
                    self.confirm_full_uninstall = false;
                }
            });
        });
        if response.should_close() {
            self.confirm_full_uninstall = false;
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
            egui::ScrollArea::vertical()
                .id_salt("copyright_scroll")
                .max_height((available.y - 180.0).clamp(60.0, 360.0))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(egui::Label::new(LICENSES).wrap().selectable(true));
                });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(Text::CopyAll.text()).clicked() {
                    ctx.copy_text(LICENSES.to_owned());
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

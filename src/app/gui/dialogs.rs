use super::{VoiceBoxApp, jobs::JobResult};
use crate::app::uninstall;

const LICENSES: &str = concat!(
    "VoiceBox v",
    env!("CARGO_PKG_VERSION"),
    "\nCopyright (c) 2026 Tanix\n\n",
    "Project source: https://github.com/TanixLu/voice_box\n\n",
    "Original VoiceBox code is licensed under MIT OR Apache-2.0, at your option.\n",
    "Third-party components and model weights retain their respective licenses.\n\n",
    "===== VoiceBox: MIT License =====\n\n",
    include_str!("../../../LICENSE-MIT"),
    "\n===== VoiceBox: Apache License 2.0 =====\n\n",
    include_str!("../../../LICENSE-APACHE"),
    "\n===== Third-party licenses and notices =====\n\n",
    include_str!("../../../THIRD_PARTY_LICENSES"),
);

impl VoiceBoxApp {
    pub(super) fn full_uninstall_ui(&mut self, ctx: &egui::Context) {
        if !self.confirm_full_uninstall {
            return;
        }
        let available = ctx.content_rect().size();
        let response = egui::Modal::new(egui::Id::unique("full_uninstall")).show(ctx, |ui| {
            ui.set_width((available.x - 64.0).clamp(180.0, 400.0));
            ui.heading(egui::RichText::new("卸载 VoiceBox").size(25.0));
            ui.add(egui::Label::new("将卸载 VoiceBox 的所有组件及配置。\n\n确定要继续吗？").wrap());
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        !self.jobs.busy()
                            && !self.voice.session.busy()
                            && !self.soundboard.model.busy(),
                        egui::Button::new("卸载"),
                    )
                    .clicked()
                {
                    self.confirm_full_uninstall = false;
                    self.uninstalling = self.jobs.begin(ctx, "正在准备卸载…", || {
                        JobResult::Uninstall(uninstall::run())
                    });
                }
                if ui.button("取消").clicked() {
                    self.confirm_full_uninstall = false;
                }
            });
        });
        if response.should_close() {
            self.confirm_full_uninstall = false;
        }
    }

    pub(super) fn licenses_ui(&mut self, ctx: &egui::Context) {
        if !self.show_licenses {
            return;
        }
        let available = ctx.content_rect().size();
        let response = egui::Modal::new(egui::Id::unique("copyright_notice")).show(ctx, |ui| {
            ui.set_width((available.x - 64.0).clamp(180.0, 760.0));
            ui.heading("版权声明");
            ui.label(concat!(
                "VoiceBox v",
                env!("CARGO_PKG_VERSION"),
                " · MIT OR Apache-2.0"
            ));
            ui.horizontal_wrapped(|ui| {
                ui.label("Project source:");
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
                if ui.button("复制全部").clicked() {
                    ctx.copy_text(LICENSES.to_owned());
                }
                if ui.button("关闭").clicked() {
                    ui.close();
                }
            });
        });
        if response.should_close() {
            self.show_licenses = false;
        }
    }
}

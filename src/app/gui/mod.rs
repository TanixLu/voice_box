use crate::{
    app::{settings_writer, soundboard::config::Config as SoundboardConfig},
    shared::{paths, settings::Settings},
};

mod dialogs;
mod hotkey_binding;
mod jobs;
mod microphone;
mod picker;
mod soundboard_panel;
mod style;
mod voice;
mod widgets;

use hotkey_binding::{HotkeyBinding, PlaybackAction};
use jobs::{JobResult, Jobs};
use microphone::MicrophonePanel;
use soundboard_panel::SoundboardPanel;
use style::{apply_style, install_system_font};
use voice::VoicePanel;
use widgets::section;

fn wake_ui(ctx: egui::Context) -> impl Fn() + Clone + Send + 'static {
    move || ctx.request_repaint()
}

pub fn run() -> anyhow::Result<()> {
    let settings = Settings::try_load()?;
    let soundboard_config = SoundboardConfig::try_load()?;
    let icon =
        eframe::icon_data::from_png_bytes(include_bytes!(concat!(env!("OUT_DIR"), "/icon.png")))?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(icon)
            .with_inner_size([560.0, 760.0])
            .with_min_inner_size([380.0, 340.0]),
        persist_window: true,
        persistence_path: Some(paths::data_dir()?.join(super::UI_STATE_FILE)),
        ..Default::default()
    };
    eframe::run_native(
        "VoiceBox",
        options,
        Box::new(move |cc| Ok(Box::new(VoiceBoxApp::new(cc, settings, soundboard_config)))),
    )
    .map_err(|error| anyhow::anyhow!("无法打开 VoiceBox 窗口：{error}"))?;
    // The cleanup process waits for this entire process to exit, including all
    // eframe persistence writes, before recycling the data directory.
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct UiPreferences {
    selected_microphone: Option<String>,
    zoom_factor: f32,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            selected_microphone: None,
            zoom_factor: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Tab {
    Voice,
    Soundboard,
}

struct VoiceBoxApp {
    tab: Tab,
    zoom_factor: f32,
    microphones: MicrophonePanel,
    voice: VoicePanel,
    soundboard: SoundboardPanel,
    hotkeys: HotkeyBinding,
    jobs: Jobs,
    close_enabled: bool,
    confirm_full_uninstall: bool,
    uninstalling: bool,
    uninstall_requested: bool,
    show_licenses: bool,
}

impl VoiceBoxApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        settings: Settings,
        soundboard_config: SoundboardConfig,
    ) -> Self {
        let ctx = &cc.egui_ctx;
        let preferences: UiPreferences = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, "voice_box.ui"))
            .unwrap_or_default();
        if preferences.zoom_factor.is_finite() && preferences.zoom_factor > 0.0 {
            ctx.set_zoom_factor(preferences.zoom_factor);
        }
        install_system_font(ctx);
        apply_style(ctx);
        let mut app = Self {
            tab: Tab::Voice,
            zoom_factor: ctx.zoom_factor(),
            microphones: MicrophonePanel::new(preferences.selected_microphone),
            voice: VoicePanel::new(settings),
            soundboard: SoundboardPanel::new(soundboard_config),
            hotkeys: HotkeyBinding::new(ctx),
            jobs: Jobs::default(),
            close_enabled: true,
            confirm_full_uninstall: false,
            uninstalling: false,
            uninstall_requested: false,
            show_licenses: false,
        };
        app.microphones.refresh(ctx, &mut app.jobs);
        app
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        self.zoom_factor = ui.ctx().zoom_factor();
        self.poll_job();
        self.voice.session.poll();
        self.soundboard.model.poll(wake_ui(ui.ctx().clone()));
        self.hotkeys.poll_capture(
            ui.ctx(),
            &mut self.soundboard.model,
            self.soundboard.options.as_mut(),
        );
        let shortcuts_enabled = !self.jobs.busy()
            && !self.voice.session.busy()
            && !self.soundboard.model.importing()
            && !self.uninstalling
            && !self.uninstall_requested
            && !self.confirm_full_uninstall
            && !self.soundboard.renaming()
            && !(ui.input(|input| input.focused) && ui.ctx().text_edit_focused());
        let actions = self.hotkeys.poll(shortcuts_enabled, &self.soundboard.model);
        for action in actions {
            let guid = self.microphones.installed_guid();
            match action {
                PlaybackAction::Stop => self.soundboard.model.stop(),
                PlaybackAction::Random => self.soundboard.play_random_sound(ui.ctx(), guid),
                PlaybackAction::Play(index) => self.soundboard.play_sound(ui.ctx(), index, guid),
            }
        }
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.soundboard.cancel_rename();
        }
        if self.tab != Tab::Soundboard
            || ui.input(|input| {
                !input.focused
                    || input.smooth_scroll_delta != egui::Vec2::ZERO
                    || input.pointer.is_decidedly_dragging()
                    || input.key_pressed(egui::Key::Escape)
            })
        {
            self.soundboard.cancel_delete();
        }
        if self.uninstall_requested {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.uninstalling {
            if ui.ctx().input(|input| input.viewport().close_requested()) {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::EnableButtons {
                    close: false,
                    minimized: true,
                    maximize: false,
                });
            self.close_enabled = false;
            egui::CentralPanel::default().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(64.0);
                    ui.heading("正在卸载 VoiceBox");
                    ui.add_space(20.0);
                    ui.spinner();
                    ui.add_space(12.0);
                    ui.label(&self.jobs.status);
                });
            });
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        if self.voice.session.busy() || self.soundboard.model.busy() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        let busy = self.jobs.busy();
        // Also repaint if the worker exits unexpectedly without sending a result.
        if busy {
            // An elevated GUI can stop the audio service itself; let registration
            // finish and restart the service before allowing the process to exit.
            if ui.ctx().input(|input| input.viewport().close_requested()) {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(20))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.tab, Tab::Voice, "语音");
                    ui.selectable_value(&mut self.tab, Tab::Soundboard, "音效");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button("其他", |ui| {
                            if ui
                                .add_enabled(
                                    !self.jobs.busy()
                                        && !self.voice.session.busy()
                                        && !self.soundboard.model.busy(),
                                    egui::Button::new("完全卸载"),
                                )
                                .clicked()
                            {
                                self.microphones.cancel_uninstall();
                                self.confirm_full_uninstall = true;
                                ui.close();
                            }
                            if ui.button("版权声明").clicked() {
                                self.show_licenses = true;
                                ui.close();
                            }
                        });
                    });
                });
                ui.add_space(12.0);
                if self.tab == Tab::Soundboard {
                    self.soundboard.ui(
                        ui,
                        &mut self.jobs,
                        self.voice.session.busy(),
                        &mut self.hotkeys,
                        self.microphones.installed_guid(),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt("voice_tab_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                                let width = ui.available_width().min(680.0);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(width, 0.0),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        section(ui, |ui| {
                                            self.microphones.ui(
                                                ui,
                                                self.voice.session.busy()
                                                    || self.soundboard.model.busy(),
                                                &mut self.jobs,
                                            );
                                        });
                                        section(ui, |ui| {
                                            if let Some(settings) =
                                                self.voice.settings_ui(ui, self.jobs.busy())
                                            {
                                                self.jobs.begin(
                                                    ui.ctx(),
                                                    "正在保存设置…",
                                                    move || JobResult::Saved {
                                                        result: settings_writer::save(
                                                            settings.clone(),
                                                        ),
                                                        settings,
                                                    },
                                                );
                                            }
                                        });
                                        section(ui, |ui| {
                                            self.voice.recording_ui(
                                                ui,
                                                self.jobs.busy() || self.soundboard.model.busy(),
                                                self.microphones.selected(),
                                            )
                                        });
                                    },
                                );
                            });
                        });
                }
            });

        self.licenses_ui(ui.ctx());
        self.soundboard
            .settings_ui(ui.ctx(), &mut self.jobs, &mut self.hotkeys);
        self.soundboard.finish_rename(self.tab == Tab::Soundboard);
        self.full_uninstall_ui(ui.ctx());

        // Apply after drawing so a job started by a button click disables the
        // native close button in the same frame. Every completion path restores it.
        let close_enabled = !self.jobs.busy();
        if self.close_enabled != close_enabled {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::EnableButtons {
                    close: close_enabled,
                    minimized: true,
                    maximize: true,
                });
            self.close_enabled = close_enabled;
        }
    }
}

impl eframe::App for VoiceBoxApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.soundboard.model.stop();
    }
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(
            storage,
            "voice_box.ui",
            &UiPreferences {
                selected_microphone: self.microphones.preferred_guid(),
                zoom_factor: self.zoom_factor,
            },
        );
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

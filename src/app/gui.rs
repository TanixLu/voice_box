use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

use egui::{FontData, FontDefinitions, FontFamily};
use wasapi::{DeviceEnumerator, Direction};

use super::{
    audio_test::{Activity, AudioTest},
    settings_writer,
    setup::{self, PluginStatus},
    uninstall,
};
use crate::shared::{
    paths,
    settings::{MAX_GAIN_MILLIDB, MIN_GAIN_MILLIDB, Settings},
};

const LICENSES: &str = concat!(
    "VoiceBox\nCopyright (c) 2026 Tanix\n\n",
    "Original VoiceBox code is licensed under MIT OR Apache-2.0, at your option.\n",
    "Third-party components and model weights retain their respective licenses.\n\n",
    "===== VoiceBox: MIT License =====\n\n",
    include_str!("../../LICENSE-MIT"),
    "\n===== VoiceBox: Apache License 2.0 =====\n\n",
    include_str!("../../LICENSE-APACHE"),
    "\n===== Third-party licenses and notices =====\n\n",
    include_str!("../../THIRD_PARTY_LICENSES"),
);

pub fn run() -> anyhow::Result<()> {
    let settings = Settings::try_load()?;
    let uninstall_requested = Arc::new(AtomicBool::new(false));
    let app_uninstall_requested = Arc::clone(&uninstall_requested);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 760.0])
            .with_min_inner_size([380.0, 340.0]),
        persist_window: true,
        persistence_path: Some(paths::data_dir()?.join(super::UI_STATE_FILE)),
        ..Default::default()
    };
    eframe::run_native(
        "VoiceBox",
        options,
        Box::new(move |cc| {
            Ok(Box::new(VoiceBoxApp::new(
                cc,
                settings,
                app_uninstall_requested,
            )))
        }),
    )
    .map_err(|error| anyhow::anyhow!("无法打开 VoiceBox 窗口：{error}"))?;
    // Wait for eframe storage (including its save thread) to be dropped before
    // recycling the directory. Otherwise shutdown would recreate ui.ron.
    if uninstall_requested.load(Ordering::Relaxed) {
        uninstall::run()?;
    }
    Ok(())
}

struct Microphone {
    name: String,
    device_id: String,
    guid: String,
    plugin_status: PluginStatus,
}

impl Microphone {
    fn display_name(&self) -> String {
        let status = match self.plugin_status {
            PluginStatus::Installed => "已安装插件",
            PluginStatus::NotInstalled => "插件未安装",
            PluginStatus::NeedsUpdate => "插件需要更新",
        };
        format!("{} ({status})", self.name)
    }
}

enum JobResult {
    Microphones(anyhow::Result<(Vec<Microphone>, usize)>),
    Plugin {
        result: anyhow::Result<()>,
        statuses: Vec<(String, PluginStatus)>,
        settings: Option<std::io::Result<Settings>>,
    },
    Saved {
        settings: Settings,
        result: anyhow::Result<()>,
    },
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
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

struct VoiceBoxApp {
    microphones: Vec<Microphone>,
    selected: usize,
    preferred_guid: Option<String>,
    zoom_factor: f32,
    settings: Settings,
    saved: Settings,
    job: Option<mpsc::Receiver<JobResult>>,
    close_enabled: bool,
    status: String,
    failed: bool,
    confirm_uninstall: bool,
    confirm_full_uninstall: bool,
    uninstall_requested: Arc<AtomicBool>,
    show_licenses: bool,
    audio_test: AudioTest,
}

impl VoiceBoxApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        settings: Settings,
        uninstall_requested: Arc<AtomicBool>,
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
            microphones: Vec::new(),
            selected: 0,
            preferred_guid: preferences.selected_microphone,
            zoom_factor: ctx.zoom_factor(),
            saved: settings.clone(),
            settings,
            job: None,
            close_enabled: true,
            status: String::new(),
            failed: false,
            confirm_uninstall: false,
            confirm_full_uninstall: false,
            uninstall_requested,
            show_licenses: false,
            audio_test: AudioTest::default(),
        };
        app.refresh_microphones(ctx);
        app
    }

    fn begin_job(
        &mut self,
        ctx: &egui::Context,
        status: &str,
        operation: impl FnOnce() -> JobResult + Send + 'static,
    ) {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        self.job = Some(receiver);
        self.status = status.into();
        self.failed = false;
        ctx.request_repaint();
        if let Err(error) = std::thread::Builder::new()
            .name("VoiceBoxOperation".into())
            .spawn(move || {
                let result = operation();
                match &result {
                    JobResult::Microphones(Err(error))
                    | JobResult::Plugin {
                        result: Err(error), ..
                    }
                    | JobResult::Saved {
                        result: Err(error), ..
                    } => {
                        tracing::error!("Background operation failed: {error:#}");
                    }
                    JobResult::Plugin {
                        settings: Some(Err(error)),
                        ..
                    } => {
                        tracing::error!(%error, "Cannot read settings after plugin installation");
                    }
                    _ => {}
                }
                let _ = sender.send(result);
                ctx.request_repaint();
            })
        {
            self.job = None;
            self.status = format!("无法开始操作：{error}");
            self.failed = true;
        }
    }

    fn refresh_microphones(&mut self, ctx: &egui::Context) {
        self.confirm_uninstall = false;
        self.begin_job(ctx, "正在读取麦克风列表…", || {
            JobResult::Microphones(microphones())
        });
    }

    fn poll_job(&mut self) {
        let Some(receiver) = &self.job else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.job = None;
                self.status = "操作未完成，请重试。".into();
                self.failed = true;
                return;
            }
        };
        self.job = None;
        match result {
            JobResult::Microphones(Ok((microphones, selected))) => {
                let selected = self
                    .preferred_guid
                    .as_ref()
                    .and_then(|guid| {
                        microphones
                            .iter()
                            .position(|microphone| microphone.guid.eq_ignore_ascii_case(guid))
                    })
                    .unwrap_or(selected);
                self.microphones = microphones;
                self.selected = selected;
                if self.preferred_guid.is_none() {
                    self.preferred_guid =
                        self.microphones.get(selected).map(|mic| mic.guid.clone());
                }
                self.status.clear();
            }
            JobResult::Plugin {
                result,
                statuses,
                settings,
            } => {
                for (guid, status) in statuses {
                    if let Some(mic) = self.microphones.iter_mut().find(|mic| mic.guid == guid) {
                        mic.plugin_status = status;
                    }
                }
                self.status.clear();
                if let Err(error) = result {
                    self.status = format!("操作失败：{error:#}");
                    self.failed = true;
                } else if let Some(settings) = settings {
                    match settings {
                        Ok(settings) => {
                            if self.settings == self.saved {
                                self.settings = settings.clone();
                            }
                            self.saved = settings;
                        }
                        Err(error) => {
                            self.status = format!("APO 已注册，但配置读取失败：{error}");
                            self.failed = true;
                        }
                    }
                }
            }
            JobResult::Saved {
                settings,
                result: Ok(()),
            } => {
                self.saved = settings;
                self.status.clear();
            }
            JobResult::Microphones(Err(error))
            | JobResult::Saved {
                result: Err(error), ..
            } => {
                self.status = format!("操作失败：{error:#}");
                self.failed = true;
            }
        }
    }

    fn save_settings(&mut self, ctx: &egui::Context) {
        let settings = self.settings.clone();
        self.begin_job(ctx, "正在保存设置…", move || JobResult::Saved {
            result: settings_writer::save(settings.clone()),
            settings,
        });
    }
}

impl VoiceBoxApp {
    fn show(&mut self, ui: &mut egui::Ui) {
        self.zoom_factor = ui.ctx().zoom_factor();
        self.poll_job();
        self.audio_test.poll();
        if self.uninstall_requested.load(Ordering::Relaxed) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.audio_test.busy() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        let busy = self.job.is_some();
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
                egui::ScrollArea::vertical()
                    .id_salt("settings_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Keep the form readable when the window is maximized.
                        ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                            let width = ui.available_width().min(680.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2(width, 0.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    ui.horizontal(|ui| {
                                        ui.menu_button("其他", |ui| {
                                            if ui
                                                .add_enabled(
                                                    self.job.is_none() && !self.audio_test.busy(),
                                                    egui::Button::new("完全卸载"),
                                                )
                                                .clicked()
                                            {
                                                self.confirm_uninstall = false;
                                                self.confirm_full_uninstall = true;
                                                ui.close();
                                            }
                                            if ui.button("版权声明").clicked() {
                                                self.show_licenses = true;
                                                ui.close();
                                            }
                                        });
                                    });
                                    ui.add_space(6.0);
                                    section(ui, |ui| {
                                        self.microphone_ui(ui);
                                    });
                                    section(ui, |ui| self.settings_ui(ui));
                                    section(ui, |ui| self.test_ui(ui));
                                },
                            );
                        });
                    });
            });

        self.licenses_ui(ui.ctx());
        self.full_uninstall_ui(ui.ctx());

        // Apply after drawing so a job started by a button click disables the
        // native close button in the same frame. Every completion path restores it.
        let close_enabled = self.job.is_none();
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

    fn full_uninstall_ui(&mut self, ctx: &egui::Context) {
        if !self.confirm_full_uninstall {
            return;
        }
        let available = ctx.content_rect().size();
        let response = egui::Modal::new(egui::Id::new("full_uninstall")).show(ctx, |ui| {
            ui.set_width((available.x - 64.0).clamp(180.0, 400.0));
            ui.heading("卸载 VoiceBox");
            ui.add(egui::Label::new("将卸载 VoiceBox 的所有组件及配置。\n\n确定要继续吗？").wrap());
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        self.job.is_none() && !self.audio_test.busy(),
                        egui::Button::new("卸载"),
                    )
                    .clicked()
                {
                    self.confirm_full_uninstall = false;
                    self.uninstall_requested.store(true, Ordering::Relaxed);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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

    fn licenses_ui(&mut self, ctx: &egui::Context) {
        if !self.show_licenses {
            return;
        }
        let available = ctx.content_rect().size();
        let response = egui::Modal::new(egui::Id::new("copyright_notice")).show(ctx, |ui| {
            ui.set_width((available.x - 64.0).clamp(180.0, 760.0));
            ui.heading("版权声明");
            ui.label("VoiceBox · MIT OR Apache-2.0");
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("copyright_scroll")
                .max_height((available.y - 180.0).clamp(60.0, 560.0))
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

    fn microphone_ui(&mut self, ui: &mut egui::Ui) -> egui::Response {
        ui.horizontal(|ui| {
            ui.strong("麦克风");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.job.is_none() && !self.audio_test.busy(),
                        egui::Button::new("刷新"),
                    )
                    .clicked()
                {
                    self.refresh_microphones(ui.ctx());
                }
            });
        });
        let previous = self.selected;
        let name = self
            .microphones
            .get(self.selected)
            .map_or_else(|| "暂无可用麦克风".to_owned(), Microphone::display_name);
        let response = ui
            .add_enabled_ui(
                self.job.is_none() && !self.audio_test.busy() && !self.microphones.is_empty(),
                |ui| {
                    egui::ComboBox::from_id_salt("microphone")
                        .width(ui.available_width())
                        .height(240.0)
                        .wrap()
                        .selected_text(&name)
                        .show_ui(ui, |ui| {
                            // ComboBox popups default to extending their text. Explicitly
                            // constrain every row, including names with no word breaks.
                            ui.set_width(ui.available_width());
                            for (index, microphone) in self.microphones.iter().enumerate() {
                                let row = egui::Button::selectable(
                                    self.selected == index,
                                    microphone.display_name(),
                                )
                                .wrap()
                                .min_size(egui::vec2(ui.available_width(), 30.0));
                                if ui.add(row).clicked() {
                                    self.selected = index;
                                    self.preferred_guid = Some(microphone.guid.clone());
                                }
                            }
                        })
                        .response
                        .on_hover_text(name)
                },
            )
            .inner;
        if self.selected != previous {
            self.confirm_uninstall = false;
            self.preferred_guid = self
                .microphones
                .get(self.selected)
                .map(|mic| mic.guid.clone());
        }
        if self.microphones.is_empty() {
            hint(ui, "连接麦克风后，点击刷新重新查找。");
        }
        self.plugin_ui(ui);
        if self.job.is_some() {
            ui.horizontal_top(|ui| {
                ui.spinner();
                ui.add(egui::Label::new(&self.status).wrap());
            });
        } else if self.failed {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&self.status).color(ui.visuals().error_fg_color),
                )
                .wrap(),
            );
        }
        response
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.strong("语音设置");
        ui.add_enabled_ui(self.job.is_none(), |ui| {
            ui.checkbox(&mut self.settings.enable_denoising, "启用降噪");
            let mut gain_db = self.settings.gain_millidb as f64 / 1000.0;
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.label("音量调节");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut gain_db)
                                .range(
                                    MIN_GAIN_MILLIDB as f64 / 1000.0
                                        ..=MAX_GAIN_MILLIDB as f64 / 1000.0,
                                )
                                .speed(0.1)
                                .fixed_decimals(1)
                                .custom_formatter(|value, _| {
                                    if value <= MIN_GAIN_MILLIDB as f64 / 1000.0 {
                                        "静音".into()
                                    } else if value > 0.0 {
                                        format!("+{value:.1} dB")
                                    } else {
                                        format!("{value:.1} dB")
                                    }
                                })
                                .custom_parser(|text| {
                                    if text.trim() == "静音" {
                                        Some(MIN_GAIN_MILLIDB as f64 / 1000.0)
                                    } else {
                                        text.trim().trim_end_matches("dB").trim().parse().ok()
                                    }
                                }),
                        )
                        .on_hover_text("拖动调整，或点击数值直接输入。")
                        .changed();
                    if ui
                        .add_enabled(gain_db != 0.0, egui::Button::new("置零"))
                        .on_hover_text("恢复到 0 dB")
                        .clicked()
                    {
                        gain_db = 0.0;
                        changed = true;
                    }
                });
            });
            ui.scope(|ui| {
                ui.spacing_mut().slider_width = ui.available_width();
                let slider = ui.add(
                    egui::Slider::new(
                        &mut gain_db,
                        MIN_GAIN_MILLIDB as f64 / 1000.0..=MAX_GAIN_MILLIDB as f64 / 1000.0,
                    )
                    .step_by(0.1)
                    .show_value(false),
                );
                changed |= slider.changed();
                if slider.dragged() && gain_db.abs() <= 0.5 {
                    gain_db = 0.0;
                    changed = true;
                }
                // Zero lies at the midpoint of this symmetric, linear range.
                let marker = slider.rect.center_bottom() + egui::vec2(0.0, 2.0);
                ui.painter()
                    .circle_filled(marker, 2.0, ui.visuals().weak_text_color());
            });
            if changed {
                self.settings.gain_millidb = (gain_db * 1000.0).round() as i32;
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.job.is_none(), egui::Button::new("保存设置"))
                .clicked()
            {
                self.save_settings(ui.ctx());
            }
            if ui
                .add_enabled(
                    self.job.is_none() && self.settings != self.saved,
                    egui::Button::new("撤销修改"),
                )
                .clicked()
            {
                self.settings = self.saved.clone();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.job.is_none() && self.settings != Settings::default(),
                        egui::Button::new("恢复默认"),
                    )
                    .clicked()
                {
                    self.settings = Settings::default();
                }
            });
        });
        if self.settings != self.saved {
            warning(
                ui,
                "有未保存的更改；试听会使用当前设置，保存后应用于麦克风插件。",
            );
        } else {
            hint(ui, "设置已作用于所有安装VoiceBox插件的麦克风。");
        }
    }

    fn test_ui(&mut self, ui: &mut egui::Ui) {
        ui.strong("测试");
        let activity = self.audio_test.activity();
        let idle = self.job.is_none() && !self.audio_test.busy();
        let stopping = self.audio_test.stopping();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !stopping
                        && (activity == Some(Activity::Record)
                            || (idle && !self.microphones.is_empty())),
                    egui::Button::new(if activity == Some(Activity::Record) {
                        "结束录音"
                    } else {
                        "开始录音"
                    }),
                )
                .clicked()
            {
                if activity == Some(Activity::Record) {
                    self.audio_test.stop();
                } else if let Some(mic) = self.microphones.get(self.selected) {
                    self.audio_test.record(ui.ctx(), mic.device_id.clone());
                }
            }
            if ui
                .add_enabled(
                    !stopping
                        && (activity == Some(Activity::Original)
                            || (idle && self.audio_test.has_recording())),
                    egui::Button::new(if activity == Some(Activity::Original) {
                        "停止播放"
                    } else {
                        "播放原音频"
                    }),
                )
                .clicked()
            {
                if activity == Some(Activity::Original) {
                    self.audio_test.stop();
                } else {
                    self.audio_test.play(ui.ctx(), None);
                }
            }
            // Place processed playback at the right on normal window sizes;
            // wrapped layout keeps every button reachable in a narrow window.
            let processed_label = if activity == Some(Activity::Processed) {
                "停止播放"
            } else {
                "播放处理后声音"
            };
            let button_width = ui
                .painter()
                .layout_no_wrap(
                    processed_label.into(),
                    egui::TextStyle::Button.resolve(ui.style()),
                    ui.visuals().text_color(),
                )
                .size()
                .x
                + 2.0 * ui.spacing().button_padding.x;
            // Wrapping layouts report the full row in available_width(); use
            // the remaining space so the spacer cannot expand the section.
            let spacer =
                (ui.available_size_before_wrap().x - button_width - ui.spacing().item_spacing.x)
                    .max(0.0);
            ui.add_space(spacer);
            if ui
                .add_enabled(
                    !stopping
                        && (activity == Some(Activity::Processed)
                            || (idle && self.audio_test.has_recording())),
                    egui::Button::new(processed_label),
                )
                .clicked()
            {
                if activity == Some(Activity::Processed) {
                    self.audio_test.stop();
                } else {
                    self.audio_test.play(ui.ctx(), Some(self.settings.clone()));
                }
            }
        });
        if stopping {
            hint(ui, "正在停止…");
        } else if let Some(elapsed) = self.audio_test.elapsed() {
            ui.label(format!("正在录音：{elapsed:.1} 秒"));
        } else if let Some((elapsed, duration)) = self.audio_test.playback_progress() {
            ui.label(format!("正在播放 {elapsed:.1} / {duration:.1} 秒"));
        } else if self.audio_test.failed {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&self.audio_test.message)
                        .color(ui.visuals().error_fg_color),
                )
                .wrap(),
            );
        } else if !self.audio_test.message.is_empty() {
            ui.horizontal(|ui| {
                if self.audio_test.busy() {
                    ui.spinner();
                }
                ui.add(egui::Label::new(&self.audio_test.message).wrap());
            });
        }
    }

    fn plugin_ui(&mut self, ui: &mut egui::Ui) {
        let status = self
            .microphones
            .get(self.selected)
            .map_or(PluginStatus::NotInstalled, |mic| mic.plugin_status);
        let needs_update = status == PluginStatus::NeedsUpdate;
        ui.add_enabled_ui(
            !self.microphones.is_empty() && self.job.is_none() && !self.audio_test.busy(),
            |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            status != PluginStatus::Installed,
                            egui::Button::new(if needs_update {
                                "更新插件"
                            } else {
                                "安装插件"
                            }),
                        )
                        .on_hover_text("安装或更新所选麦克风的VoiceBox插件")
                        .clicked()
                        && self.job.is_none()
                    {
                        self.confirm_uninstall = false;
                        let guid = self.microphones[self.selected].guid.clone();
                        let guids = self
                            .microphones
                            .iter()
                            .map(|mic| mic.guid.clone())
                            .collect();
                        self.begin_job(
                            ui.ctx(),
                            if needs_update {
                                "正在更新插件，请完成管理员授权…"
                            } else {
                                "正在安装插件，请完成管理员授权…"
                            },
                            move || plugin_operation(guid, guids, false),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                status != PluginStatus::NotInstalled,
                                egui::Button::new("卸载插件"),
                            )
                            .clicked()
                            && self.job.is_none()
                        {
                            self.confirm_uninstall = true;
                        }
                    });
                });
            },
        );
        ui.add_enabled_ui(self.job.is_none() && !self.audio_test.busy(), |ui| {
            if self.confirm_uninstall {
                ui.separator();
                ui.add(
                    egui::Label::new(format!(
                        "确认卸载此麦克风的VoiceBox插件？\n{}",
                        self.microphones[self.selected].name
                    ))
                    .wrap(),
                );
                ui.horizontal_wrapped(|ui| {
                    if ui.button("确认卸载").clicked() && self.job.is_none() {
                        self.confirm_uninstall = false;
                        let guid = self.microphones[self.selected].guid.clone();
                        let guids = self
                            .microphones
                            .iter()
                            .map(|mic| mic.guid.clone())
                            .collect();
                        self.begin_job(
                            ui.ctx(),
                            "正在卸载插件，请完成管理员授权…",
                            move || plugin_operation(guid, guids, true),
                        );
                    }
                    if ui.button("取消").clicked() {
                        self.confirm_uninstall = false;
                    }
                });
            }
        });
    }
}

impl eframe::App for VoiceBoxApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(
            storage,
            "voice_box.ui",
            &UiPreferences {
                selected_microphone: self.preferred_guid.clone(),
                zoom_factor: self.zoom_factor,
            },
        );
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

fn plugin_operation(guid: String, guids: Vec<String>, uninstall: bool) -> JobResult {
    let result = if uninstall {
        setup::uninstall_device(&guid)
    } else {
        setup::ensure_device_reg(&guid)
    };
    // Recheck even after failure: the helper can have partially succeeded. All
    // DLL, registry, ACL and settings reads stay in the background worker.
    let statuses = guids
        .into_iter()
        .map(|guid| {
            let status = setup::device_plugin_status(&guid);
            (guid, status)
        })
        .collect();
    let settings = (!uninstall && result.is_ok()).then(Settings::try_load);
    JobResult::Plugin {
        result,
        statuses,
        settings,
    }
}

fn section(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .inner_margin(12)
        .corner_radius(8)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            contents(ui);
        });
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).small().weak()).wrap());
}

fn warning(ui: &mut egui::Ui, text: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .small()
                .color(ui.visuals().warn_fg_color),
        )
        .wrap(),
    );
}

fn apply_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        style
            .text_styles
            .insert(egui::TextStyle::Heading, egui::FontId::proportional(24.0));
    });
}

struct ComApartment;

impl ComApartment {
    fn new() -> anyhow::Result<Self> {
        wasapi::initialize_mta().ok()?;
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}

fn microphones() -> anyhow::Result<(Vec<Microphone>, usize)> {
    let _apartment = ComApartment::new()?;
    let enumerator = DeviceEnumerator::new()?;
    let default_id = enumerator
        .get_default_device(&Direction::Capture)
        .and_then(|device| device.get_id())
        .ok();
    let mut microphones = Vec::new();
    let mut selected = 0;
    for device in &enumerator.get_device_collection(&Direction::Capture)? {
        let device = device?;
        let id = device.get_id()?;
        let guid = id
            .rsplit('.')
            .next()
            .ok_or_else(|| anyhow::anyhow!("麦克风标识无效"))?
            .to_owned();
        let is_default = default_id.as_deref() == Some(&id);
        if is_default {
            selected = microphones.len();
        }
        let name = device.get_friendlyname()?;
        microphones.push(Microphone {
            device_id: id,
            name: if is_default {
                format!("{name}（默认）")
            } else {
                name
            },
            plugin_status: setup::device_plugin_status(&guid),
            guid,
        });
    }
    Ok((microphones, selected))
}

fn install_system_font(ctx: &egui::Context) {
    let windows = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "C:\\Windows".into());
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

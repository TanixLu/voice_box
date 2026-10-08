use std::sync::{Arc, mpsc};

use egui::{FontData, FontDefinitions, FontFamily};
use wasapi::{DeviceEnumerator, Direction};

use crate::{
    apo_reg,
    apo_reg::PluginStatus,
    config_reg,
    settings::{MAX_GAIN_MILLIDB, MIN_GAIN_MILLIDB, Settings},
};

pub fn run() -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 620.0])
            .with_min_inner_size([380.0, 340.0]),
        ..Default::default()
    };
    eframe::run_native(
        "VoiceBox",
        options,
        Box::new(|cc| Ok(Box::new(VoiceBoxApp::new(&cc.egui_ctx)))),
    )
    .map_err(|error| anyhow::anyhow!("无法打开 VoiceBox 窗口：{error}"))
}

struct Microphone {
    name: String,
    guid: String,
    plugin_status: PluginStatus,
}

impl Microphone {
    fn display_name(&self) -> String {
        let status = match self.plugin_status {
            PluginStatus::Installed => "已安装插件",
            PluginStatus::NotInstalled => "未安装插件",
            PluginStatus::NeedsUpdate => "插件需要更新",
        };
        format!("{} ({status})", self.name)
    }
}

enum JobResult {
    Microphones(anyhow::Result<(Vec<Microphone>, usize)>),
    Registration(anyhow::Result<()>),
    Uninstallation(anyhow::Result<()>),
}

struct VoiceBoxApp {
    microphones: Vec<Microphone>,
    selected: usize,
    settings: Settings,
    saved: Settings,
    job: Option<mpsc::Receiver<JobResult>>,
    close_enabled: bool,
    status: String,
    failed: bool,
    confirm_uninstall: bool,
}

impl VoiceBoxApp {
    fn new(ctx: &egui::Context) -> Self {
        install_chinese_font(ctx);
        configure_style(ctx);
        let settings = Settings::try_load().unwrap_or_default();
        let mut app = Self {
            microphones: Vec::new(),
            selected: 0,
            settings,
            saved: settings,
            job: None,
            close_enabled: true,
            status: String::new(),
            failed: false,
            confirm_uninstall: false,
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
        // A helper may partially succeed before reporting an error. Read the
        // association again instead of inferring installation from its exit code.
        if matches!(
            &result,
            JobResult::Registration(_) | JobResult::Uninstallation(_)
        ) {
            for microphone in &mut self.microphones {
                microphone.plugin_status = apo_reg::device_plugin_status(&microphone.guid);
            }
        }
        match result {
            JobResult::Microphones(Ok((microphones, selected))) => {
                let selected = self
                    .microphones
                    .get(self.selected)
                    .and_then(|previous| {
                        microphones
                            .iter()
                            .position(|microphone| microphone.guid == previous.guid)
                    })
                    .unwrap_or(selected);
                self.microphones = microphones;
                self.selected = selected;
                self.status.clear();
            }
            JobResult::Registration(Ok(())) => {
                let unsaved = self.settings != self.saved;
                match Settings::try_load() {
                    Ok(settings) => {
                        self.saved = settings;
                        if !unsaved {
                            self.settings = settings;
                        }
                        self.status.clear();
                    }
                    Err(error) => {
                        self.status = format!("APO 已注册，但配置读取失败：{error}");
                        self.failed = true;
                    }
                }
            }
            JobResult::Uninstallation(Ok(())) => {
                self.status.clear();
            }
            JobResult::Microphones(Err(error))
            | JobResult::Registration(Err(error))
            | JobResult::Uninstallation(Err(error)) => {
                self.status = format!("操作失败：{error:#}");
                self.failed = true;
            }
        }
    }

    fn save_settings(&mut self) {
        match config_reg::save(self.settings) {
            Ok(()) => {
                self.saved = self.settings;
                self.status.clear();
                self.failed = false;
            }
            Err(error) => {
                self.status = format!("保存失败：{error:#}");
                self.failed = true;
            }
        }
    }
}

impl VoiceBoxApp {
    fn show(&mut self, ui: &mut egui::Ui) {
        self.poll_job();
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
                                    ui.heading("VoiceBox");
                                    ui.add_space(6.0);
                                    section(ui, |ui| {
                                        self.microphone_ui(ui);
                                    });
                                    section(ui, |ui| self.settings_ui(ui));
                                },
                            );
                        });
                    });
            });

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

    fn microphone_ui(&mut self, ui: &mut egui::Ui) -> egui::Response {
        ui.horizontal(|ui| {
            ui.strong("麦克风");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(self.job.is_none(), egui::Button::new("刷新"))
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
            .add_enabled_ui(self.job.is_none() && !self.microphones.is_empty(), |ui| {
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
                            }
                        }
                    })
                    .response
                    .on_hover_text(name)
            })
            .inner;
        if self.selected != previous {
            self.confirm_uninstall = false;
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
            ui.checkbox(&mut self.settings.enabled, "启用语音降噪");
            ui.separator();
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
            ui.separator();
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.job.is_none(), egui::Button::new("保存设置"))
                .clicked()
            {
                self.save_settings();
            }
            if ui
                .add_enabled(
                    self.job.is_none() && self.settings != self.saved,
                    egui::Button::new("撤销修改"),
                )
                .clicked()
            {
                self.settings = self.saved;
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
            warning(ui, "有未保存的更改，点击“保存设置”后生效。");
        } else {
            hint(ui, "设置已作用于所有安装VoiceBox插件的麦克风。");
        }
    }

    fn plugin_ui(&mut self, ui: &mut egui::Ui) {
        let status = self
            .microphones
            .get(self.selected)
            .map_or(PluginStatus::NotInstalled, |mic| mic.plugin_status);
        let needs_update = status == PluginStatus::NeedsUpdate;
        ui.add_enabled_ui(!self.microphones.is_empty() && self.job.is_none(), |ui| {
            ui.horizontal_wrapped(|ui| {
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
                    self.begin_job(
                        ui.ctx(),
                        if needs_update {
                            "正在更新插件，请完成管理员授权…"
                        } else {
                            "正在安装插件，请完成管理员授权…"
                        },
                        move || JobResult::Registration(apo_reg::ensure_device_reg(&guid)),
                    );
                }
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
        ui.add_enabled_ui(self.job.is_none(), |ui| {
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
                        self.begin_job(
                            ui.ctx(),
                            "正在卸载插件，请完成管理员授权…",
                            move || JobResult::Uninstallation(apo_reg::uninstall_device(&guid)),
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
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
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

fn configure_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        style
            .text_styles
            .insert(egui::TextStyle::Heading, egui::FontId::proportional(24.0));
    });
}

fn microphones() -> anyhow::Result<(Vec<Microphone>, usize)> {
    wasapi::initialize_mta().ok()?;
    struct ComApartment;
    impl Drop for ComApartment {
        fn drop(&mut self) {
            wasapi::deinitialize();
        }
    }
    let _apartment = ComApartment;
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
            name: if is_default {
                format!("{name}（默认）")
            } else {
                name
            },
            plugin_status: apo_reg::device_plugin_status(&guid),
            guid,
        });
    }
    Ok((microphones, selected))
}

fn install_chinese_font(ctx: &egui::Context) {
    let windows = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "C:\\Windows".into());
    for file in ["msyh.ttc", "simsun.ttc"] {
        if let Ok(data) = std::fs::read(windows.join("Fonts").join(file)) {
            let mut fonts = FontDefinitions::default();
            fonts
                .font_data
                .insert("chinese".into(), Arc::new(FontData::from_owned(data)));
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .insert(0, "chinese".into());
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .push("chinese".into());
            ctx.set_fonts(fonts);
            break;
        }
    }
}

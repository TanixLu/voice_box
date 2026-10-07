use std::sync::{Arc, mpsc};

use egui::{Color32, FontData, FontDefinitions, FontFamily};
use wasapi::{DeviceEnumerator, Direction};

use crate::{apo_reg, config_reg, settings::Settings};

pub fn run() -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([520.0, 410.0])
            .with_min_inner_size([440.0, 360.0]),
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
    config_ready: bool,
    job: Option<mpsc::Receiver<JobResult>>,
    status: String,
    failed: bool,
}

impl VoiceBoxApp {
    fn new(ctx: &egui::Context) -> Self {
        install_chinese_font(ctx);
        let settings = Settings::try_load().unwrap_or_default();
        let mut app = Self {
            microphones: Vec::new(),
            selected: 0,
            settings,
            saved: settings,
            config_ready: config_reg::ready(),
            job: None,
            status: String::new(),
            failed: false,
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
                self.status = if self.microphones.is_empty() {
                    "没有找到可用麦克风，请连接后刷新。".into()
                } else {
                    "请选择麦克风，然后注册 APO。".into()
                };
            }
            JobResult::Registration(Ok(())) => {
                self.config_ready = config_reg::ready();
                let unsaved = self.settings != self.saved;
                match Settings::try_load() {
                    Ok(settings) => {
                        self.saved = settings;
                        if !unsaved {
                            self.settings = settings;
                        }
                        self.status = "APO 注册完成，可以修改配置。".into();
                    }
                    Err(error) => {
                        self.status = format!("APO 已注册，但配置读取失败：{error}");
                        self.failed = true;
                    }
                }
            }
            JobResult::Uninstallation(Ok(())) => {
                self.status =
                    "已卸载 VoiceBox APO，并恢复所有备份的原有 APO。请重新打开语音或录音应用。"
                        .into();
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
                self.status = "配置已保存，正在使用的 APO 会自动更新。".into();
                self.failed = false;
            }
            Err(error) => {
                self.status = format!("保存失败：{error:#}");
                self.failed = true;
            }
        }
    }
}

impl eframe::App for VoiceBoxApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
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
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("VoiceBox");
            ui.label("麦克风语音降噪");
            ui.add_space(12.0);

            ui.group(|ui| {
                ui.set_min_width(ui.available_width());
                ui.strong("注册 APO");
                ui.add_enabled_ui(!busy, |ui| {
                    egui::ComboBox::from_id_salt("microphone")
                        .width(360.0)
                        .selected_text(
                            self.microphones
                                .get(self.selected)
                                .map_or("暂无麦克风", |microphone| microphone.name.as_str()),
                        )
                        .show_ui(ui, |ui| {
                            for (index, microphone) in self.microphones.iter().enumerate() {
                                ui.selectable_value(&mut self.selected, index, &microphone.name);
                            }
                        });
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self.microphones.is_empty(),
                                egui::Button::new("注册 / 更新 APO"),
                            )
                            .clicked()
                        {
                            let guid = self.microphones[self.selected].guid.clone();
                            self.begin_job(
                                ui.ctx(),
                                "正在注册 APO，请完成管理员授权…",
                                move || JobResult::Registration(apo_reg::ensure_device_reg(&guid)),
                            );
                        }
                        if ui
                            .add_enabled(
                                !self.microphones.is_empty() && self.job.is_none(),
                                egui::Button::new("卸载 VoiceBox APO"),
                            )
                            .clicked()
                        {
                            let guid = self.microphones[self.selected].guid.clone();
                            self.begin_job(
                                ui.ctx(),
                                "正在卸载 VoiceBox APO，请完成管理员授权…",
                                move || JobResult::Uninstallation(apo_reg::uninstall_device(&guid)),
                            );
                        }
                        if ui.button("刷新麦克风").clicked() && self.job.is_none() {
                            self.refresh_microphones(ui.ctx());
                        }
                    });
                });
                ui.small("注册时可能请求管理员权限，并短暂重启音频服务。");
                ui.small("注册后仅保留 VoiceBox APO；卸载时恢复原有全部 APO。");
            });

            ui.add_space(10.0);
            ui.group(|ui| {
                ui.set_min_width(ui.available_width());
                ui.strong("APO 配置");
                if !self.config_ready {
                    ui.label("请先注册 APO，之后修改配置无需管理员权限。");
                }
                ui.add_enabled_ui(self.config_ready && !busy, |ui| {
                    ui.checkbox(&mut self.settings.enabled, "启用 GTCRN 降噪");
                    let mut gain_db = self.settings.gain_millidb as f64 / 1000.0;
                    if ui
                        .add(
                            egui::Slider::new(&mut gain_db, -60.0..=12.0)
                                .step_by(0.1)
                                .fixed_decimals(1)
                                .suffix(" dB")
                                .text("输出增益"),
                        )
                        .changed()
                    {
                        self.settings.gain_millidb = (gain_db * 1000.0).round() as i32;
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(self.settings != self.saved, egui::Button::new("保存配置"))
                            .clicked()
                        {
                            self.save_settings();
                        }
                        if ui.button("恢复默认").clicked() {
                            self.settings = Settings::default();
                            self.save_settings();
                        }
                        if ui.button("重新读取").clicked() {
                            match Settings::try_load() {
                                Ok(settings) => {
                                    self.settings = settings;
                                    self.saved = settings;
                                    self.status = "已读取当前配置。".into();
                                    self.failed = false;
                                }
                                Err(error) => {
                                    self.status = format!("读取失败，保留当前配置：{error}");
                                    self.failed = true;
                                }
                            }
                        }
                    });
                });
                ui.small("配置对所有使用 VoiceBox APO 的麦克风生效。");
            });

            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                if busy {
                    ui.spinner();
                }
                if self.failed {
                    ui.colored_label(Color32::from_rgb(220, 80, 80), &self.status);
                } else {
                    ui.label(&self.status);
                }
            });
        });
    }
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
            guid,
        });
    }
    Ok((microphones, selected))
}

fn install_chinese_font(ctx: &egui::Context) {
    let windows = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "C:\\Windows".into());
    for file in ["msyh.ttc", "simhei.ttf", "simsun.ttc"] {
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

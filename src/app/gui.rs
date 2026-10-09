use std::sync::{Arc, mpsc};

use egui::{FontData, FontDefinitions, FontFamily};
use wasapi::{DeviceEnumerator, Direction};

use super::{
    settings_writer,
    setup::{self, PluginStatus},
};
use crate::shared::{
    paths,
    settings::{MAX_GAIN_MILLIDB, MIN_GAIN_MILLIDB, ReferenceDevice, Settings},
};

pub fn run() -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 620.0])
            .with_min_inner_size([380.0, 340.0]),
        persist_window: true,
        persistence_path: Some(paths::data_dir()?.join(super::UI_STATE_FILE)),
        ..Default::default()
    };
    eframe::run_native(
        "VoiceBox",
        options,
        Box::new(|cc| Ok(Box::new(VoiceBoxApp::new(cc)))),
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
            PluginStatus::NeedsUpdate => "需要更新修复",
        };
        format!("{} ({status})", self.name)
    }
}

enum JobResult {
    Microphones(anyhow::Result<(Vec<Microphone>, usize, Vec<RenderDevice>)>),
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

struct RenderDevice {
    id: String,
    name: String,
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
    render_devices: Vec<RenderDevice>,
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
}

impl VoiceBoxApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = &cc.egui_ctx;
        let preferences: UiPreferences = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, "voice_box.ui"))
            .unwrap_or_default();
        if preferences.zoom_factor.is_finite() && preferences.zoom_factor > 0.0 {
            ctx.set_zoom_factor(preferences.zoom_factor);
        }
        install_chinese_font(ctx);
        apply_style(ctx);
        let settings = Settings::try_load().unwrap_or_default();
        let mut app = Self {
            microphones: Vec::new(),
            render_devices: Vec::new(),
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
            JobResult::Microphones(Ok((microphones, selected, render_devices))) => {
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
                self.render_devices = render_devices;
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
                                self.preferred_guid = Some(microphone.guid.clone());
                            }
                        }
                    })
                    .response
                    .on_hover_text(name)
            })
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
            ui.checkbox(&mut self.settings.enabled, "启用语音降噪");
            ui.checkbox(&mut self.settings.aec_enabled, "启用回声消除")
                .on_hover_text(
                    "消除麦克风收到的扬声器声音，减少通话回声。使用耳机时通常可以关闭。",
                );
            ui.add_enabled_ui(self.settings.aec_enabled, |ui| {
                let selected = match &self.settings.reference_device {
                    ReferenceDevice::DefaultPlayback => "跟随默认播放设备",
                    ReferenceDevice::DefaultCommunications => "跟随默认通信设备",
                    ReferenceDevice::Device(id) => self
                        .render_devices
                        .iter()
                        .find(|device| &device.id == id)
                        .map_or("所选设备未连接", |device| device.name.as_str()),
                };
                egui::ComboBox::from_id_salt("aec_output_device")
                    .selected_text(selected)
                    .width(ui.available_width())
                    .height(240.0)
                    .wrap()
                    .show_ui(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.selectable_value(
                            &mut self.settings.reference_device,
                            ReferenceDevice::DefaultPlayback,
                            "跟随默认播放设备",
                        );
                        ui.selectable_value(
                            &mut self.settings.reference_device,
                            ReferenceDevice::DefaultCommunications,
                            "跟随默认通信设备",
                        );
                        for device in &self.render_devices {
                            let choice = ReferenceDevice::Device(device.id.clone());
                            let row = egui::Button::selectable(
                                self.settings.reference_device == choice,
                                &device.name,
                            )
                            .wrap()
                            .min_size(egui::vec2(ui.available_width(), 30.0));
                            if ui.add(row).clicked() {
                                self.settings.reference_device = choice;
                            }
                        }
                    });
            });
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
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        status != PluginStatus::Installed,
                        egui::Button::new(if needs_update {
                            "更新修复"
                        } else {
                            "安装插件"
                        }),
                    )
                    .on_hover_text("安装或更新修复所选麦克风的VoiceBox插件")
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
                            "正在更新修复插件，请完成管理员授权…"
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

fn microphones() -> anyhow::Result<(Vec<Microphone>, usize, Vec<RenderDevice>)> {
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
            plugin_status: setup::device_plugin_status(&guid),
            guid,
        });
    }
    let mut render_devices = Vec::new();
    match enumerator.get_device_collection(&Direction::Render) {
        Ok(collection) => {
            for device in &collection {
                match device.and_then(|device| {
                    Ok(RenderDevice {
                        id: device.get_id()?,
                        name: device.get_friendlyname()?,
                    })
                }) {
                    Ok(device) => render_devices.push(device),
                    Err(error) => {
                        tracing::warn!(%error, "Cannot read an output device; keeping microphone list")
                    }
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "Cannot enumerate output devices; keeping microphone list")
        }
    }
    Ok((microphones, selected, render_devices))
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

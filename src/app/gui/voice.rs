use super::widgets::{hint, warning};
use crate::{
    app::{
        audio::session::{Activity, AudioSession},
        devices::Microphone,
    },
    shared::settings::{MAX_GAIN_MILLIDB, MIN_GAIN_MILLIDB, Settings},
};

pub(super) struct VoicePanel {
    settings: Settings,
    saved: Settings,
    pub(super) session: AudioSession,
}

impl VoicePanel {
    pub(super) fn new(settings: Settings) -> Self {
        Self {
            saved: settings.clone(),
            settings,
            session: AudioSession::default(),
        }
    }
    pub(super) fn mark_saved(&mut self, settings: Settings) {
        self.saved = settings;
    }
    pub(super) fn settings_ui(&mut self, ui: &mut egui::Ui, busy: bool) -> Option<Settings> {
        let mut save_requested = false;
        ui.strong("语音设置");
        ui.add_enabled_ui(!busy, |ui| {
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
            let dirty = self.settings != self.saved;
            if ui
                .add_enabled(!busy && dirty, egui::Button::new("保存设置"))
                .clicked()
            {
                save_requested = true;
            }
            if ui
                .add_enabled(!busy && dirty, egui::Button::new("撤销修改"))
                .clicked()
            {
                self.settings = self.saved.clone();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !busy && self.settings != Settings::default(),
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
                "有未保存的修改；试听使用当前设置，保存后应用于麦克风插件。",
            );
        } else {
            hint(ui, "设置已应用到所有安装 VoiceBox 插件的麦克风。");
        }
        save_requested.then(|| self.settings.clone())
    }

    pub(super) fn recording_ui(
        &mut self,
        ui: &mut egui::Ui,
        busy: bool,
        microphone: Option<&Microphone>,
    ) {
        ui.strong("录音试听");
        let activity = self.session.activity();
        let idle = !busy && !self.session.busy();
        let stopping = self.session.stopping();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !stopping
                        && (activity == Some(Activity::Record) || (idle && microphone.is_some())),
                    egui::Button::new(if activity == Some(Activity::Record) {
                        "结束录音"
                    } else {
                        "开始录音"
                    }),
                )
                .clicked()
            {
                if activity == Some(Activity::Record) {
                    self.session.stop();
                } else if let Some(mic) = microphone {
                    self.session
                        .record(mic.device_id.clone(), super::wake_ui(ui.ctx().clone()));
                }
            }
            if ui
                .add_enabled(
                    !stopping
                        && (activity == Some(Activity::Original)
                            || (idle && self.session.has_recording())),
                    egui::Button::new(if activity == Some(Activity::Original) {
                        "停止播放"
                    } else {
                        "播放原音频"
                    }),
                )
                .clicked()
            {
                if activity == Some(Activity::Original) {
                    self.session.stop();
                } else {
                    self.session.play(None, super::wake_ui(ui.ctx().clone()));
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
                            || (idle && self.session.has_recording())),
                    egui::Button::new(processed_label),
                )
                .clicked()
            {
                if activity == Some(Activity::Processed) {
                    self.session.stop();
                } else {
                    self.session.play(
                        Some(self.settings.clone()),
                        super::wake_ui(ui.ctx().clone()),
                    );
                }
            }
        });
        if stopping {
            hint(ui, "正在停止…");
        } else if let Some(elapsed) = self.session.elapsed() {
            ui.label(format!("正在录音：{elapsed:.1} 秒"));
        } else if let Some((elapsed, duration)) = self.session.playback_progress() {
            ui.label(format!("正在播放 {elapsed:.1} / {duration:.1} 秒"));
        } else if self.session.failed {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&self.session.message).color(ui.visuals().error_fg_color),
                )
                .wrap(),
            );
        } else if !self.session.message.is_empty() {
            ui.horizontal(|ui| {
                if self.session.busy() {
                    ui.spinner();
                }
                ui.add(egui::Label::new(&self.session.message).wrap());
            });
        }
    }
}

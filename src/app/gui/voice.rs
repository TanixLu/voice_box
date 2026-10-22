use super::widgets::{hint, trailing_button, warning};
use crate::app::i18n::Message;
use crate::app::i18n::Text;
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
        ui.strong(Text::VoiceSettings.text());
        ui.add_enabled_ui(!busy, |ui| {
            ui.checkbox(
                &mut self.settings.denoising_enabled,
                Text::EnableDenoising.text(),
            );
            let mut gain_db = self.settings.gain_millidb as f64 / 1000.0;
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.label(Text::VolumeAdjustment.text());
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
                                        Text::Mute.text().into()
                                    } else if value > 0.0 {
                                        format!("+{value:.1} dB")
                                    } else {
                                        format!("{value:.1} dB")
                                    }
                                })
                                .custom_parser(|text| {
                                    if Text::Mute.matches_input(text) {
                                        Some(MIN_GAIN_MILLIDB as f64 / 1000.0)
                                    } else {
                                        text.trim().trim_end_matches("dB").trim().parse().ok()
                                    }
                                }),
                        )
                        .on_hover_text(Text::AdjustVolumeHint.text())
                        .changed();
                    if ui
                        .add_enabled(gain_db != 0.0, egui::Button::new(Text::ResetGain.text()))
                        .on_hover_text(Text::ResetGainHint.text())
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
        ui.horizontal_wrapped(|ui| {
            let dirty = self.settings != self.saved;
            if ui
                .add_enabled(!busy && dirty, egui::Button::new(Text::SaveSettings.text()))
                .clicked()
            {
                save_requested = true;
            }
            if ui
                .add_enabled(!busy && dirty, egui::Button::new(Text::UndoChanges.text()))
                .clicked()
            {
                self.settings = self.saved.clone();
            }
            if trailing_button(
                ui,
                !busy && self.settings != Settings::default(),
                Text::RestoreDefaults.text(),
            )
            .clicked()
            {
                self.settings = Settings::default();
            }
        });
        if self.settings != self.saved {
            warning(ui, Text::UnsavedVoiceSettings.text());
        } else {
            hint(ui, Text::VoiceSettingsApplied.text());
        }
        save_requested.then(|| self.settings.clone())
    }

    pub(super) fn recording_ui(
        &mut self,
        ui: &mut egui::Ui,
        busy: bool,
        microphone: Option<&Microphone>,
    ) {
        ui.strong(Text::RecordingPreview.text());
        let activity = self.session.activity();
        let idle = !busy && !self.session.busy();
        let stopping = self.session.stopping();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !stopping
                        && (activity == Some(Activity::Recording)
                            || (idle && microphone.is_some())),
                    egui::Button::new(if activity == Some(Activity::Recording) {
                        Text::FinishRecording.text()
                    } else {
                        Text::StartRecording.text()
                    }),
                )
                .clicked()
            {
                if activity == Some(Activity::Recording) {
                    self.session.stop();
                } else if let Some(mic) = microphone {
                    self.session
                        .record(mic.device_id.clone(), super::wake_ui(ui.ctx().clone()));
                }
            }
            if ui
                .add_enabled(
                    !stopping
                        && (activity == Some(Activity::PlayingOriginal)
                            || (idle && self.session.has_recording())),
                    egui::Button::new(if activity == Some(Activity::PlayingOriginal) {
                        Text::StopPlayback.text()
                    } else {
                        Text::PlayOriginal.text()
                    }),
                )
                .clicked()
            {
                if activity == Some(Activity::PlayingOriginal) {
                    self.session.stop();
                } else {
                    self.session.play(None, super::wake_ui(ui.ctx().clone()));
                }
            }
            // Place processed playback at the right on normal window sizes;
            // wrapped layout keeps every button reachable in a narrow window.
            let processed_label = if activity == Some(Activity::PlayingProcessed) {
                Text::StopPlayback.text()
            } else {
                Text::PlayProcessed.text()
            };
            if trailing_button(
                ui,
                !stopping
                    && (activity == Some(Activity::PlayingProcessed)
                        || (idle && self.session.has_recording())),
                processed_label,
            )
            .clicked()
            {
                if activity == Some(Activity::PlayingProcessed) {
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
            hint(ui, Text::Stopping.text());
        } else if let Some(elapsed) = self.session.recording_elapsed() {
            ui.label(Message::RecordingProgress { elapsed }.text());
        } else if let Some((elapsed, duration)) = self.session.playback_progress() {
            ui.label(Message::PlaybackProgress { elapsed, duration }.text());
        } else if self.session.failed {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(self.session.message.text())
                        .color(ui.visuals().error_fg_color),
                )
                .wrap(),
            );
        } else if !self.session.message.is_empty() {
            ui.horizontal(|ui| {
                if self.session.busy() {
                    ui.spinner();
                }
                ui.add(egui::Label::new(self.session.message.text()).wrap());
            });
        }
    }
}

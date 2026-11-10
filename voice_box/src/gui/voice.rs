use voice_box_shared::{apo_config::ApoConfig, utils::GainDb};

use crate::{
    audio::voice_preview::{Activity, VoicePreview},
    devices::Microphone,
    gui::{
        Busy,
        widgets::{hint, trailing_button, warning},
    },
    i18n::{Details, Message, Text},
    task::{self, Task, Wake},
};

/// An operation clicked while drawing; the app applies it once the frame is drawn.
pub enum Command {
    Save,
    Record(String),
    /// Plays the recording, processed with this configuration when present.
    Play(Option<ApoConfig>),
    Stop,
}

pub struct VoicePanel {
    wake: Wake,
    config: ApoConfig,
    saved: ApoConfig,
    save: Option<Task<ApoConfig>>,
    save_error: Message,
    preview: VoicePreview,
    preview_error: Message,
}

impl VoicePanel {
    pub fn new(config: ApoConfig, wake: Wake) -> Self {
        Self {
            preview: VoicePreview::new(wake.clone()),
            wake,
            saved: config.clone(),
            config,
            save: None,
            save_error: Message::default(),
            preview_error: Message::default(),
        }
    }
    pub fn busy(&self) -> Busy {
        Busy {
            working: self.save.is_some(),
            audio: self.preview.busy(),
        }
    }
    pub fn poll(&mut self) {
        match task::finished(&mut self.save) {
            Some(Ok(config)) => self.saved = config,
            Some(Err(error)) => {
                self.save_error = Message::OperationFailed {
                    error: Details::new(error),
                }
            }
            None => {}
        }
        if let Some(Err(error)) = self.preview.poll() {
            self.preview_error = Message::PreviewFailed {
                error: Details::new(error),
            };
        }
    }
    pub fn apply(&mut self, command: Command) {
        match command {
            Command::Save => {
                let config = self.config.clone();
                self.save_error.clear();
                self.save = Some(Task::run("VoiceBoxApoConfigSave", &self.wake, move || {
                    config.save()?;
                    Ok(config)
                }));
            }
            Command::Record(device_id) => {
                self.preview_error.clear();
                self.preview.record(device_id);
            }
            Command::Play(config) => {
                self.preview_error.clear();
                self.preview.play(config);
            }
            Command::Stop => self.preview.stop(),
        }
    }

    pub fn config_ui(&mut self, ui: &mut egui::Ui) -> Option<Command> {
        let mut command = None;
        let defaults = ApoConfig::default();
        let busy = self.save.is_some();
        ui.strong(Text::VoiceConfig.text());
        ui.add_enabled_ui(!busy, |ui| {
            ui.checkbox(
                &mut self.config.denoising_enabled,
                Text::EnableDenoising.text(),
            );
            const MIN_DB: f64 = GainDb::MIN_VALUE as f64;
            const MAX_DB: f64 = GainDb::MAX_VALUE as f64;
            let mut gain_db = self.config.gain_db.as_i8();
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.label(Text::VolumeAdjustment.text());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut gain_db)
                                .range(MIN_DB..=MAX_DB)
                                .speed(1.0)
                                .fixed_decimals(0)
                                .custom_formatter(|value, _| {
                                    if value <= MIN_DB {
                                        Text::Mute.text().into()
                                    } else if value > 0.0 {
                                        format!("+{value:.0} dB")
                                    } else {
                                        format!("{value:.0} dB")
                                    }
                                })
                                .custom_parser(|text| {
                                    if Text::Mute.matches_input(text) {
                                        Some(MIN_DB)
                                    } else {
                                        text.trim().trim_end_matches("dB").trim().parse().ok()
                                    }
                                }),
                        )
                        .on_hover_text(Text::AdjustVolumeHint.text())
                        .changed();
                    if ui
                        .add_enabled(gain_db != 0, egui::Button::new(Text::ResetGain.text()))
                        .on_hover_text(Text::ResetGainHint.text())
                        .clicked()
                    {
                        gain_db = 0;
                        changed = true;
                    }
                });
            });
            ui.scope(|ui| {
                ui.spacing_mut().slider_width = ui.available_width();
                let slider = ui.add(
                    egui::Slider::new(&mut gain_db, GainDb::MIN_VALUE..=GainDb::MAX_VALUE)
                        .step_by(1.0)
                        .show_value(false),
                );
                changed |= slider.changed();
                // Zero lies at the midpoint of this symmetric, linear range.
                let marker = slider.rect.center_bottom() + egui::vec2(0.0, 2.0);
                ui.painter()
                    .circle_filled(marker, 2.0, ui.visuals().weak_text_color());
            });
            if changed {
                self.config.gain_db = GainDb::new(gain_db);
            }
            ui.checkbox(
                &mut self.config.playback_include_microphone_audio,
                Text::IncludeMicrophoneAudio.text(),
            );
        });
        ui.horizontal_wrapped(|ui| {
            let dirty = self.config != self.saved;
            if ui
                .add_enabled(!busy && dirty, egui::Button::new(Text::SaveChanges.text()))
                .clicked()
            {
                command = Some(Command::Save);
            }
            if ui
                .add_enabled(!busy && dirty, egui::Button::new(Text::UndoChanges.text()))
                .clicked()
            {
                self.config = self.saved.clone();
            }
            if trailing_button(
                ui,
                !busy && self.config != defaults,
                Text::RestoreDefaults.text(),
            )
            .clicked()
            {
                self.config = defaults;
            }
        });
        if !self.save_error.is_empty() {
            warning(ui, &self.save_error.text());
        } else if self.config != self.saved {
            warning(ui, Text::UnsavedVoiceConfig.text());
        } else {
            hint(ui, Text::VoiceConfigApplied.text());
        }
        command
    }

    pub fn recording_ui(
        &mut self,
        ui: &mut egui::Ui,
        others: Busy,
        microphone: Option<&Microphone>,
    ) -> Option<Command> {
        let mut command = None;
        ui.strong(Text::RecordingPreview.text());
        let activity = self.preview.activity();
        let idle = !others.any() && !self.preview.busy();
        let stopping = self.preview.stopping();
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
                command = if activity == Some(Activity::Recording) {
                    Some(Command::Stop)
                } else {
                    microphone.map(|mic| Command::Record(mic.device_id.clone()))
                };
            }
            if ui
                .add_enabled(
                    !stopping
                        && (activity == Some(Activity::PlayingOriginal)
                            || (idle && self.preview.has_recording())),
                    egui::Button::new(if activity == Some(Activity::PlayingOriginal) {
                        Text::StopPlayback.text()
                    } else {
                        Text::PlayOriginal.text()
                    }),
                )
                .clicked()
            {
                command = Some(if activity == Some(Activity::PlayingOriginal) {
                    Command::Stop
                } else {
                    Command::Play(None)
                });
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
                        || (idle && self.preview.has_recording())),
                processed_label,
            )
            .clicked()
            {
                command = Some(if activity == Some(Activity::PlayingProcessed) {
                    Command::Stop
                } else {
                    Command::Play(Some(self.config.clone()))
                });
            }
        });
        if stopping {
            hint(ui, Text::Stopping.text());
        } else if let Some(elapsed_secs) = self.preview.recording_elapsed_secs() {
            ui.label(Message::RecordingProgress { elapsed_secs }.text());
        } else if let Some((elapsed_secs, duration_secs)) = self.preview.playback_progress_secs() {
            ui.label(
                Message::PlaybackProgress {
                    elapsed_secs,
                    duration_secs,
                }
                .text(),
            );
        } else if !self.preview_error.is_empty() {
            warning(ui, &self.preview_error.text());
        } else if let Some(activity) = self.preview.activity().filter(|_| self.preview.starting()) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(match activity {
                    Activity::Recording => Text::OpeningMicrophone.text(),
                    Activity::PlayingOriginal => Text::OpeningPlaybackDevice.text(),
                    Activity::PlayingProcessed => Text::PreparingProcessedAudio.text(),
                });
            });
        } else if let Some(duration_secs) = self.preview.recording_duration_secs() {
            ui.label(Message::Recorded { duration_secs }.text());
        }
        command
    }
}

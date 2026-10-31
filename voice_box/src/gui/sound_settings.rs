//! A settings draft and its presentation; effects are handled by the parent panel.

use voice_box_shared::pcm_transport::GainPercent;

use crate::{
    devices::{Microphone, Speaker},
    gui::widgets::{ButtonIcon, hint, hotkey_editor, icon_button, trailing_button, warning},
    i18n::{Message, Text},
    settings::{BindingTarget, OutputDevice, Settings},
};

#[derive(Default)]
pub struct SoundSettings {
    draft: Option<Settings>,
    microphones: Vec<(OutputDevice, String)>,
    speakers: Vec<(OutputDevice, String)>,
}

pub struct Status<'a> {
    pub busy: bool,
    pub capture_target: Option<BindingTarget>,
    pub capture_message: &'a Message,
    pub error: Option<&'a Message>,
}

pub enum Action {
    None,
    Closed,
    Save(Settings),
    Capture(BindingTarget),
    ClearMessage,
}

impl SoundSettings {
    pub fn open(&mut self, settings: &Settings) {
        self.draft = Some(settings.clone());
    }
    pub fn is_open(&self) -> bool {
        self.draft.is_some()
    }
    pub fn draft_mut(&mut self) -> Option<&mut Settings> {
        self.draft.as_mut()
    }
    pub fn set_devices(&mut self, microphones: Vec<Microphone>, speakers: Vec<Speaker>) {
        self.microphones = microphones
            .into_iter()
            .map(|mic| {
                let name = super::microphone::display_name(&mic);
                (
                    OutputDevice {
                        id: mic.device_id,
                        name: mic.name,
                    },
                    name,
                )
            })
            .collect();
        self.speakers = speakers
            .into_iter()
            .map(|speaker| {
                let device = OutputDevice {
                    id: speaker.device_id,
                    name: speaker.name.clone(),
                };
                let name = if speaker.is_default {
                    Message::DefaultDevice { name: speaker.name }
                        .text()
                        .into_owned()
                } else {
                    speaker.name
                };
                (device, name)
            })
            .collect();
    }
    pub fn ui(&mut self, ctx: &egui::Context, current: &Settings, status: Status<'_>) -> Action {
        if self.draft.is_none() {
            return Action::None;
        }
        let defaults = Settings {
            language_preference: current.language_preference,
            theme_preference: current.theme_preference,
            startup_enabled: current.startup_enabled,
            ..Settings::default()
        };
        let available = ctx.content_rect().size();
        let capture_target = status.capture_target;
        let idle = !status.busy;
        let mut capture = None;
        let mut save = false;
        let mut close = false;
        let mut clear_message = false;
        let response = egui::Modal::new(egui::Id::unique("sound_settings"))
            .frame(egui::Frame::popup(&ctx.style_of(ctx.theme())).inner_margin(16))
            .show(ctx, |ui| {
                if !idle {
                    ui.disable();
                }
                ui.set_width((available.x - 64.0).clamp(240.0, 460.0));
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(Text::SoundSettings.text())
                            .size(17.0)
                            .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close = icon_button(ui, true, ButtonIcon::Close).clicked();
                    });
                });
                ui.add_space(12.0);
                egui::ScrollArea::vertical()
                    .max_height((available.y - 230.0).max(80.0))
                    .show(ui, |ui| {
                        let settings = self.draft.as_mut().unwrap();
                        playback_volume(
                            ui,
                            Text::MicrophoneOutput.text(),
                            &mut settings.playback_microphone_output_gain_percent,
                        );
                        output_device_picker(
                            ui,
                            "sound_microphone",
                            &mut settings.playback_microphone_output_device,
                            &self.microphones,
                        );
                        ui.add_space(8.0);
                        playback_volume(
                            ui,
                            Text::SpeakerOutput.text(),
                            &mut settings.playback_speaker_output_gain_percent,
                        );
                        output_device_picker(
                            ui,
                            "sound_speaker",
                            &mut settings.playback_speaker_output_device,
                            &self.speakers,
                        );
                        ui.add_space(8.0);
                        ui.checkbox(
                            &mut settings.playback_include_microphone_audio,
                            Text::IncludeMicrophoneAudio.text(),
                        );
                        ui.checkbox(
                            &mut settings.playback_show_overlay,
                            Text::ShowOverlay.text(),
                        );
                        ui.checkbox(&mut settings.playback_repeat, Text::RepeatPlayback.text());
                        if hotkey_editor(
                            ui,
                            Text::RandomHotkey.text(),
                            &mut settings.playback_random_hotkey,
                            BindingTarget::Random,
                            capture_target,
                        ) {
                            capture = Some(BindingTarget::Random);
                        }
                        if hotkey_editor(
                            ui,
                            Text::StopHotkey.text(),
                            &mut settings.playback_stop_hotkey,
                            BindingTarget::Stop,
                            capture_target,
                        ) {
                            capture = Some(BindingTarget::Stop);
                        }
                        if hotkey_editor(
                            ui,
                            Text::HoldDuringPlayback.text(),
                            &mut settings.playback_auto_hold_hotkey,
                            BindingTarget::Hold,
                            capture_target,
                        ) {
                            capture = Some(BindingTarget::Hold);
                        }
                        if !status.capture_message.is_empty() {
                            warning(ui, &status.capture_message.text());
                        }
                    });
                ui.add_space(12.0);
                let dirty = self
                    .draft
                    .as_ref()
                    .is_some_and(|settings| *settings != *current);
                ui.horizontal_wrapped(|ui| {
                    save = ui
                        .add_enabled(dirty, egui::Button::new(Text::SaveChanges.text()))
                        .clicked();
                    if ui
                        .add_enabled(dirty, egui::Button::new(Text::UndoChanges.text()))
                        .clicked()
                    {
                        self.draft = Some(current.clone());
                        clear_message = true;
                    }
                    if trailing_button(
                        ui,
                        self.draft
                            .as_ref()
                            .is_some_and(|settings| *settings != defaults),
                        Text::RestoreDefaults.text(),
                    )
                    .clicked()
                    {
                        self.draft = Some(defaults.clone());
                        clear_message = true;
                    }
                });
                if self
                    .draft
                    .as_ref()
                    .is_some_and(|settings| *settings != *current)
                {
                    warning(ui, Text::UnsavedSoundSettings.text());
                } else {
                    hint(ui, Text::SoundSettingsApplied.text());
                }
                if let Some(error) = status.error {
                    warning(ui, &error.text());
                }
            });
        // Ignore backdrop clicks so they do not discard unsaved settings.
        if idle && (close || (!response.backdrop_response.clicked() && response.should_close())) {
            self.draft = None;
            Action::Closed
        } else if save {
            let draft = self.draft.as_mut().unwrap();
            draft.language_preference = current.language_preference;
            draft.theme_preference = current.theme_preference;
            draft.startup_enabled = current.startup_enabled;
            Action::Save(draft.clone())
        } else if let Some(target) = capture {
            Action::Capture(target)
        } else if clear_message {
            Action::ClearMessage
        } else {
            Action::None
        }
    }
}

fn output_device_picker(
    ui: &mut egui::Ui,
    id: &str,
    selected: &mut Option<OutputDevice>,
    devices: &[(OutputDevice, String)],
) {
    let name = match selected {
        None => Text::FollowSystemDefault.text().to_owned(),
        Some(device) => devices
            .iter()
            .find(|(available, _)| available.id == device.id)
            .map(|(_, label)| label.clone())
            .unwrap_or_else(|| {
                Message::UnavailableOutputDevice {
                    name: device.name.clone(),
                }
                .text()
                .into_owned()
            }),
    };
    egui::ComboBox::from_id_salt(id)
        .width(ui.available_width())
        .wrap()
        .selected_text(name)
        .show_ui(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.selectable_value(selected, None, Text::FollowSystemDefault.text());
            for (device, name) in devices {
                let is_selected = selected
                    .as_ref()
                    .is_some_and(|selected| selected.id == device.id);
                if ui
                    .add(egui::Button::selectable(is_selected, name).wrap())
                    .clicked()
                {
                    *selected = Some(device.clone());
                }
            }
        });
}

fn playback_volume(ui: &mut egui::Ui, label: &str, gain_percent: &mut GainPercent) -> bool {
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        ui.add(
            egui::Slider::new(gain_percent.inner_mut_u16(), 0..=GainPercent::MAX_VALUE)
                .step_by(1.0)
                .custom_formatter(|value, _| {
                    if value == 0.0 {
                        Text::NoOutput.text().into()
                    } else {
                        format!("{:.0}%", value)
                    }
                })
                .custom_parser(|text| {
                    if Text::NoOutput.matches_input(text) {
                        Some(0.0)
                    } else {
                        text.trim()
                            .trim_end_matches('%')
                            .trim()
                            .parse::<f64>()
                            .ok()
                            .filter(|value| value.is_finite())
                    }
                }),
        )
        .changed()
    })
    .inner
}

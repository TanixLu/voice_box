//! A settings draft and its presentation; effects are handled by the parent panel.

use crate::{
    devices::{Microphone, Speaker},
    gui::widgets::{ButtonIcon, hint, hotkey_editor, icon_button, trailing_button, warning},
    i18n::{Message, Text},
    soundboard::{
        bindings::Target,
        settings::{Endpoint, GainPercent, SoundboardSettings},
    },
};

#[derive(Default)]
pub struct SoundboardSettingsDialog {
    draft: Option<SoundboardSettings>,
    microphones: Vec<Microphone>,
    speakers: Vec<Speaker>,
}

/// A listed device, labeled in the current language whenever it is drawn.
trait PickerDevice {
    fn id(&self) -> &str;
    fn endpoint(&self) -> Endpoint;
    fn label(&self) -> String;
}

impl PickerDevice for Microphone {
    fn id(&self) -> &str {
        &self.device_id
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint {
            id: self.device_id.clone(),
            name: self.name.clone(),
        }
    }
    fn label(&self) -> String {
        super::widgets::microphone_label(self)
    }
}

impl PickerDevice for Speaker {
    fn id(&self) -> &str {
        &self.device_id
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint {
            id: self.device_id.clone(),
            name: self.name.clone(),
        }
    }
    fn label(&self) -> String {
        super::widgets::device_label(&self.name, self.is_default)
    }
}

pub struct Status<'a> {
    pub busy: bool,
    pub capture_target: Option<&'a Target>,
    pub capture_message: &'a Message,
    pub error: Option<&'a Message>,
}

pub enum Action {
    Close,
    Save(SoundboardSettings),
    Capture(Target),
    ClearMessage,
}

impl SoundboardSettingsDialog {
    pub fn open(&mut self, settings: &SoundboardSettings) {
        self.draft = Some(settings.clone());
    }
    pub fn is_open(&self) -> bool {
        self.draft.is_some()
    }
    pub fn draft_mut(&mut self) -> Option<&mut SoundboardSettings> {
        self.draft.as_mut()
    }

    fn is_dirty(&self, settings: &SoundboardSettings) -> bool {
        self.draft.as_ref().is_some_and(|draft| draft != settings)
    }
    pub fn set_devices(&mut self, microphones: Vec<Microphone>, speakers: Vec<Speaker>) {
        self.microphones = microphones;
        self.speakers = speakers;
    }
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        current: &SoundboardSettings,
        status: Status<'_>,
    ) -> Option<Action> {
        self.draft.as_ref()?;
        let defaults = SoundboardSettings::default();
        let available = ctx.content_rect().size();
        let capture_target = status.capture_target;
        let idle = !status.busy;
        let mut capture = None;
        let mut save = false;
        let mut close = false;
        let mut clear_message = false;
        egui::Modal::new(egui::Id::unique("soundboard_settings"))
            .frame(egui::Frame::popup(&ctx.style_of(ctx.theme())).inner_margin(16))
            .show(ctx, |ui| {
                if !idle {
                    ui.disable();
                }
                ui.set_width((available.x - 64.0).clamp(240.0, 460.0));
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(Text::SoundboardSettings.text())
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
                        let settings = self.draft.as_mut().expect(
                            "UNREACHABLE: settings draft should exist while rendering the dialog",
                        );
                        playback_volume(
                            ui,
                            Text::MicrophoneOutput.text(),
                            &mut settings.microphone_gain_percent,
                        );
                        device_picker(
                            ui,
                            "sound_microphone",
                            &mut settings.microphone_device,
                            &self.microphones,
                        );
                        ui.add_space(8.0);
                        playback_volume(
                            ui,
                            Text::SpeakerOutput.text(),
                            &mut settings.speaker_gain_percent,
                        );
                        device_picker(
                            ui,
                            "sound_speaker",
                            &mut settings.speaker_device,
                            &self.speakers,
                        );
                        ui.add_space(8.0);
                        ui.checkbox(&mut settings.show_overlay, Text::ShowOverlay.text());
                        ui.checkbox(&mut settings.repeat, Text::RepeatPlayback.text());
                        if hotkey_editor(
                            ui,
                            Text::RandomHotkey.text(),
                            &mut settings.keys.random,
                            capture_target == Some(&Target::Random),
                        ) {
                            capture = Some(Target::Random);
                        }
                        if hotkey_editor(
                            ui,
                            Text::StopHotkey.text(),
                            &mut settings.keys.stop,
                            capture_target == Some(&Target::Stop),
                        ) {
                            capture = Some(Target::Stop);
                        }
                        if hotkey_editor(
                            ui,
                            Text::HoldDuringPlayback.text(),
                            &mut settings.keys.hold,
                            capture_target == Some(&Target::Hold),
                        ) {
                            capture = Some(Target::Hold);
                        }
                        if !status.capture_message.is_empty() {
                            warning(ui, &status.capture_message.text());
                        }
                    });
                ui.add_space(12.0);
                let dirty = self.is_dirty(current);
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
                    if trailing_button(ui, self.is_dirty(&defaults), Text::RestoreDefaults.text())
                        .clicked()
                    {
                        self.draft = Some(defaults.clone());
                        clear_message = true;
                    }
                });
                if self.is_dirty(current) {
                    warning(ui, Text::UnsavedSoundboardSettings.text());
                } else {
                    hint(ui, Text::SoundboardSettingsApplied.text());
                }
                if let Some(error) = status.error {
                    warning(ui, &error.text());
                }
            });
        // Only the close button closes, so neither Escape nor a backdrop click discards unsaved settings.
        if idle && close {
            self.draft = None;
            Some(Action::Close)
        } else if save {
            let draft = self
                .draft
                .clone()
                .expect("UNREACHABLE: settings draft should exist while rendering the dialog");
            Some(Action::Save(draft))
        } else if let Some(target) = capture {
            Some(Action::Capture(target))
        } else if clear_message {
            Some(Action::ClearMessage)
        } else {
            None
        }
    }
}

fn device_picker(
    ui: &mut egui::Ui,
    id: &str,
    selected: &mut Option<Endpoint>,
    devices: &[impl PickerDevice],
) {
    let name = match selected {
        None => Text::FollowSystemDefault.text().to_owned(),
        Some(device) => devices
            .iter()
            .find(|available| available.id() == device.id)
            .map(PickerDevice::label)
            .unwrap_or_else(|| {
                Message::UnavailableDevice {
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
            for device in devices {
                let is_selected = selected
                    .as_ref()
                    .is_some_and(|selected| selected.id == device.id());
                if ui
                    .add(egui::Button::selectable(is_selected, device.label()).wrap())
                    .clicked()
                {
                    *selected = Some(device.endpoint());
                }
            }
        });
}

fn playback_volume(ui: &mut egui::Ui, label: &str, gain_percent: &mut GainPercent) {
    let mut value = gain_percent.as_u16();
    let changed = ui
        .horizontal_wrapped(|ui| {
            ui.label(label);
            ui.add(
                egui::Slider::new(&mut value, 0..=GainPercent::MAX_VALUE)
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
        .inner;
    if changed {
        *gain_percent = GainPercent::new(value);
    }
}

//! Routing controls embedded below the microphone selector, with independent state.

use super::widgets::device_label;
use crate::{
    devices::Speaker,
    forwarding::{Forwarding, Status, Target},
    i18n::Text,
    task::Wake,
};

pub struct ForwardingPanel {
    forwarding: Forwarding,
    speakers: Vec<Speaker>,
}

impl ForwardingPanel {
    pub fn new(wake: Wake) -> Self {
        Self {
            forwarding: Forwarding::new(wake),
            speakers: Vec::new(),
        }
    }

    pub fn poll(&mut self, source: Option<&str>) {
        // The shared microphone selector is the only input to this feature.
        self.forwarding.poll(source);
    }

    pub fn set_speakers(&mut self, speakers: Vec<Speaker>) {
        self.speakers = speakers;
    }

    pub fn configure(&mut self, source: Option<&str>, target: Target) {
        self.forwarding.configure(source, target);
    }

    pub fn target(&self) -> Target {
        self.forwarding.target()
    }

    pub fn ui(&self, ui: &mut egui::Ui, has_source: bool) -> Option<Target> {
        ui.strong(Text::SoundForwarding.text());
        let speakers = &self.speakers;
        let mut selected = self.target();
        let name = match &selected {
            Target::Off => Text::ForwardingOff.text().to_owned(),
            Target::DefaultSpeaker => Text::DefaultSpeaker.text().to_owned(),
            Target::Speaker(id) => speakers
                .iter()
                .find(|speaker| &speaker.device_id == id)
                .map_or_else(
                    || Text::DeviceUnavailable.text().to_owned(),
                    |speaker| device_label(&speaker.name, speaker.is_default),
                ),
        };
        ui.add_enabled_ui(has_source || selected != Target::Off, |ui| {
            egui::ComboBox::from_id_salt("sound_forwarding_target")
                .width(ui.available_width())
                .height(240.0)
                .wrap()
                .selected_text(name)
                .show_ui(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let targets = [
                        (Target::Off, Text::ForwardingOff.text().to_owned()),
                        (
                            Target::DefaultSpeaker,
                            Text::DefaultSpeaker.text().to_owned(),
                        ),
                    ]
                    .into_iter()
                    .chain(speakers.iter().map(|speaker| {
                        (
                            Target::Speaker(speaker.device_id.clone()),
                            device_label(&speaker.name, speaker.is_default),
                        )
                    }));
                    for (target, name) in targets {
                        if ui
                            .add(egui::Button::selectable(selected == target, name).wrap())
                            .clicked()
                        {
                            selected = target;
                        }
                    }
                });
        });
        let status = self.forwarding.status();
        let label = match &status {
            Status::Idle if selected != Target::Off => Some(Text::ForwardingWaitingForMicrophone),
            Status::Opening => Some(Text::OpeningMicrophone),
            Status::Active => Some(Text::ForwardingActive),
            Status::Retrying(_) => Some(Text::ForwardingRetrying),
            Status::Idle | Status::Failed(_) => None,
        };
        if let Some(label) = label {
            ui.horizontal(|ui| {
                if matches!(status, Status::Opening | Status::Retrying(_)) {
                    ui.spinner();
                }
                ui.label(label.text());
            });
        }
        let forwarding_error = match &status {
            Status::Retrying(error) | Status::Failed(error) => Some(error),
            _ => None,
        };
        if let Some(error) = forwarding_error
            && !error.is_empty()
        {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(error.text()).color(ui.visuals().error_fg_color),
                )
                .truncate(),
            );
        }
        (selected != self.target()).then_some(selected)
    }
}

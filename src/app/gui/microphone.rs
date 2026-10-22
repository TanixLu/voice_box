use super::jobs::{JobResult, Jobs};
use crate::app::i18n::Message;
use crate::app::i18n::Text;
use crate::app::{
    devices::{self, Microphone, PluginAction},
    setup::PluginStatus,
};

#[derive(Default)]
pub(super) struct MicrophonePanel {
    devices: Vec<Microphone>,
    selected: usize,
    preferred_guid: Option<String>,
    confirm_uninstall: bool,
}

fn display_name(microphone: &Microphone) -> String {
    let status = match microphone.plugin_status {
        PluginStatus::Installed => Text::PluginInstalled.text(),
        PluginStatus::NotInstalled => Text::PluginNotInstalled.text(),
        PluginStatus::NeedsUpdate => Text::PluginNeedsUpdate.text(),
    };
    let name = if microphone.is_default {
        Message::DefaultDevice {
            name: microphone.name.clone(),
        }
        .text()
        .into_owned()
    } else {
        microphone.name.clone()
    };
    format!("{name} ({status})")
}

impl MicrophonePanel {
    pub(super) fn new(preferred_guid: Option<String>) -> Self {
        Self {
            preferred_guid,
            ..Self::default()
        }
    }
    pub(super) fn selected(&self) -> Option<&Microphone> {
        self.devices.get(self.selected)
    }
    pub(super) fn installed_guid(&self) -> Option<&str> {
        self.selected()
            .filter(|mic| mic.plugin_status == PluginStatus::Installed)
            .map(|mic| mic.guid.as_str())
    }
    pub(super) fn preferred_guid(&self) -> Option<String> {
        self.preferred_guid.clone()
    }
    pub(super) fn cancel_uninstall(&mut self) {
        self.confirm_uninstall = false;
    }
    pub(super) fn replace_devices(&mut self, devices: Vec<Microphone>, selected: usize) {
        let selected = self
            .preferred_guid
            .as_ref()
            .and_then(|guid| {
                devices
                    .iter()
                    .position(|mic| mic.guid.eq_ignore_ascii_case(guid))
            })
            .unwrap_or(selected);
        self.devices = devices;
        self.selected = selected;
        if self.preferred_guid.is_none() {
            self.preferred_guid = self.devices.get(selected).map(|mic| mic.guid.clone());
        }
    }
    pub(super) fn apply_statuses(&mut self, statuses: Vec<(String, PluginStatus)>) {
        for (guid, status) in statuses {
            if let Some(mic) = self.devices.iter_mut().find(|mic| mic.guid == guid) {
                mic.plugin_status = status;
            }
        }
    }

    pub(super) fn refresh(&mut self, ctx: &egui::Context, jobs: &mut Jobs) {
        self.confirm_uninstall = false;
        jobs.begin(ctx, Text::ReadingMicrophones, || {
            JobResult::Microphones(devices::enumerate_microphones())
        });
    }

    pub(super) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        other_busy: bool,
        jobs: &mut Jobs,
    ) -> egui::Response {
        ui.horizontal(|ui| {
            ui.strong(Text::Microphone.text());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !jobs.busy() && !other_busy,
                        egui::Button::new(Text::Refresh.text()),
                    )
                    .clicked()
                {
                    self.refresh(ui.ctx(), jobs);
                }
            });
        });
        let previous = self.selected;
        let name = self
            .devices
            .get(self.selected)
            .map_or_else(|| Text::NoMicrophones.text().to_owned(), display_name);
        let response = ui
            .add_enabled_ui(
                !jobs.busy() && !other_busy && !self.devices.is_empty(),
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
                            for (index, microphone) in self.devices.iter().enumerate() {
                                let row = egui::Button::selectable(
                                    self.selected == index,
                                    display_name(microphone),
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
            self.preferred_guid = self.devices.get(self.selected).map(|mic| mic.guid.clone());
        }
        self.plugin_ui(ui, other_busy, jobs);
        response
    }

    pub(super) fn status_ui(&self, ui: &mut egui::Ui, jobs: &Jobs) {
        let status = if jobs.busy() {
            Some(egui::RichText::new(jobs.status.text()))
        } else if jobs.failed {
            Some(egui::RichText::new(jobs.status.text()).color(ui.visuals().error_fg_color))
        } else if self.devices.is_empty() {
            Some(
                egui::RichText::new(Text::ConnectMicrophoneHint.text())
                    .small()
                    .weak(),
            )
        } else {
            None
        };
        if let Some(status) = status {
            ui.horizontal(|ui| {
                if jobs.busy() {
                    ui.spinner();
                }
                ui.add(egui::Label::new(status.clone()).truncate())
                    .on_hover_text(status);
            });
        }
    }

    fn plugin_ui(&mut self, ui: &mut egui::Ui, other_busy: bool, jobs: &mut Jobs) {
        let status = self
            .devices
            .get(self.selected)
            .map_or(PluginStatus::NotInstalled, |mic| mic.plugin_status);
        let needs_update = status == PluginStatus::NeedsUpdate;
        ui.add_enabled_ui(
            !self.devices.is_empty() && !jobs.busy() && !other_busy,
            |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            status != PluginStatus::Installed,
                            egui::Button::new(if needs_update {
                                Text::UpdatePlugin.text()
                            } else {
                                Text::InstallPlugin.text()
                            }),
                        )
                        .on_hover_text(Text::InstallPluginHint.text())
                        .clicked()
                        && !jobs.busy()
                    {
                        self.confirm_uninstall = false;
                        let guid = self.devices[self.selected].guid.clone();
                        let guids = self.devices.iter().map(|mic| mic.guid.clone()).collect();
                        jobs.begin(
                            ui.ctx(),
                            if needs_update {
                                Text::UpdatingPlugin
                            } else {
                                Text::InstallingPlugin
                            },
                            move || {
                                JobResult::Plugin(devices::execute_plugin_action(
                                    guid,
                                    guids,
                                    PluginAction::Install,
                                ))
                            },
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                status != PluginStatus::NotInstalled,
                                egui::Button::new(Text::UninstallPlugin.text()),
                            )
                            .clicked()
                            && !jobs.busy()
                        {
                            self.confirm_uninstall = true;
                        }
                    });
                });
            },
        );
        ui.add_enabled_ui(!jobs.busy() && !other_busy, |ui| {
            if self.confirm_uninstall {
                ui.separator();
                ui.add(
                    egui::Label::new(
                        Message::ConfirmPluginUninstall {
                            name: self.devices[self.selected].name.clone(),
                        }
                        .text(),
                    )
                    .wrap(),
                );
                ui.horizontal_wrapped(|ui| {
                    if ui.button(Text::ConfirmUninstall.text()).clicked() && !jobs.busy() {
                        self.confirm_uninstall = false;
                        let guid = self.devices[self.selected].guid.clone();
                        let guids = self.devices.iter().map(|mic| mic.guid.clone()).collect();
                        jobs.begin(ui.ctx(), Text::UninstallingPlugin, move || {
                            JobResult::Plugin(devices::execute_plugin_action(
                                guid,
                                guids,
                                PluginAction::Uninstall,
                            ))
                        });
                    }
                    if ui.button(Text::Cancel.text()).clicked() {
                        self.confirm_uninstall = false;
                    }
                });
            }
        });
    }
}

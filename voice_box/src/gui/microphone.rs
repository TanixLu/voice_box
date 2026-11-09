use crate::{
    devices::{self, Microphone, PluginAction, PluginOperationResult},
    gui::Busy,
    i18n::{Details, Message, Text},
    setup::PluginStatus,
    task::{Task, Wake},
};

enum Outcome {
    Devices(Vec<Microphone>, usize),
    Plugin(PluginOperationResult),
}

pub struct MicrophonePanel {
    wake: Wake,
    devices: Vec<Microphone>,
    selected: usize,
    preferred_guid: Option<String>,
    confirm_uninstall: bool,
    /// The running job and its status text.
    job: Option<(Text, Task<Outcome>)>,
    error: Message,
}

pub fn display_name(microphone: &Microphone) -> String {
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
    pub fn new(wake: Wake) -> Self {
        Self {
            wake,
            devices: Vec::new(),
            selected: 0,
            preferred_guid: None,
            confirm_uninstall: false,
            job: None,
            error: Message::default(),
        }
    }
    pub fn selected(&self) -> Option<&Microphone> {
        self.devices.get(self.selected)
    }
    pub fn cancel_uninstall(&mut self) {
        self.confirm_uninstall = false;
    }
    pub fn busy(&self) -> Busy {
        Busy {
            working: self.job.is_some(),
            audio: false,
        }
    }
    pub fn poll(&mut self) {
        let Some(result) = self.job.as_mut().and_then(|(_, task)| task.poll()) else {
            return;
        };
        self.job = None;
        let error = match result {
            Ok(Outcome::Devices(devices, selected)) => {
                self.replace_devices(devices, selected);
                None
            }
            Ok(Outcome::Plugin(PluginOperationResult { result, statuses })) => {
                for (guid, status) in statuses {
                    if let Some(mic) = self.devices.iter_mut().find(|mic| mic.guid == guid) {
                        mic.plugin_status = status;
                    }
                }
                result.err()
            }
            Err(error) => Some(error),
        };
        if let Some(error) = error {
            self.error = Message::OperationFailed {
                error: Details::new(error),
            };
        }
    }
    fn replace_devices(&mut self, devices: Vec<Microphone>, selected: usize) {
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
    fn start(
        &mut self,
        status: Text,
        work: impl FnOnce() -> anyhow::Result<Outcome> + Send + 'static,
    ) {
        self.confirm_uninstall = false;
        self.error.clear();
        self.job = Some((status, Task::run("VoiceBoxMicrophones", &self.wake, work)));
    }
    pub fn refresh(&mut self) {
        self.start(Text::ReadingMicrophones, || {
            devices::enumerate_microphones()
                .map(|(devices, selected)| Outcome::Devices(devices, selected))
        });
    }
    fn run_plugin_action(&mut self, status: Text, action: PluginAction) {
        let guid = self.devices[self.selected].guid.clone();
        let guids = self.devices.iter().map(|mic| mic.guid.clone()).collect();
        self.start(status, move || {
            Ok(Outcome::Plugin(devices::execute_plugin_action(
                guid, guids, action,
            )))
        });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, others: Busy) -> egui::Response {
        let idle = self.job.is_none() && !others.any();
        ui.horizontal(|ui| {
            ui.strong(Text::Microphone.text());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(idle, egui::Button::new(Text::Refresh.text()))
                    .clicked()
                {
                    self.refresh();
                }
            });
        });
        let previous = self.selected;
        let name = self
            .devices
            .get(self.selected)
            .map_or_else(|| Text::NoMicrophones.text().to_owned(), display_name);
        let response = ui
            .add_enabled_ui(idle && !self.devices.is_empty(), |ui| {
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
            })
            .inner;
        if self.selected != previous {
            self.confirm_uninstall = false;
            self.preferred_guid = self.devices.get(self.selected).map(|mic| mic.guid.clone());
        }
        self.plugin_ui(ui, idle);
        response
    }

    pub fn status_ui(&self, ui: &mut egui::Ui) {
        let status = if let Some((status, _)) = &self.job {
            Some(egui::RichText::new(status.text()))
        } else if !self.error.is_empty() {
            Some(egui::RichText::new(self.error.text()).color(ui.visuals().error_fg_color))
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
                if self.job.is_some() {
                    ui.spinner();
                }
                ui.add(egui::Label::new(status).truncate());
            });
        }
    }

    fn plugin_ui(&mut self, ui: &mut egui::Ui, idle: bool) {
        let status = self
            .devices
            .get(self.selected)
            .map_or(PluginStatus::NotInstalled, |mic| mic.plugin_status);
        let needs_update = status == PluginStatus::NeedsUpdate;
        ui.add_enabled_ui(idle && !self.devices.is_empty(), |ui| {
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
                {
                    let status = if needs_update {
                        Text::UpdatingPlugin
                    } else {
                        Text::InstallingPlugin
                    };
                    self.run_plugin_action(status, PluginAction::Install);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            status != PluginStatus::NotInstalled,
                            egui::Button::new(Text::UninstallPlugin.text()),
                        )
                        .clicked()
                    {
                        self.confirm_uninstall = true;
                    }
                });
            });
        });
        if !self.confirm_uninstall {
            return;
        }
        ui.add_enabled_ui(idle, |ui| {
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
                if ui.button(Text::ConfirmUninstall.text()).clicked() {
                    self.run_plugin_action(Text::UninstallingPlugin, PluginAction::Uninstall);
                }
                if ui.button(Text::Cancel.text()).clicked() {
                    self.confirm_uninstall = false;
                }
            });
        });
    }
}

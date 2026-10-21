use super::{
    jobs::{JobResult, Jobs},
    widgets::hint,
};
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
        PluginStatus::Installed => "已安装插件",
        PluginStatus::NotInstalled => "插件未安装",
        PluginStatus::NeedsUpdate => "插件需要更新",
    };
    format!("{} ({status})", microphone.name)
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
        jobs.begin(ctx, "正在读取麦克风列表…", || {
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
            ui.strong("麦克风");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!jobs.busy() && !other_busy, egui::Button::new("刷新"))
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
            .map_or_else(|| "暂无可用麦克风".to_owned(), display_name);
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
        if self.devices.is_empty() {
            hint(ui, "连接麦克风后，点击刷新重新查找。");
        }
        self.plugin_ui(ui, other_busy, jobs);
        if jobs.busy() {
            ui.horizontal_top(|ui| {
                ui.spinner();
                ui.add(egui::Label::new(&jobs.status).wrap());
            });
        } else if jobs.failed {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&jobs.status).color(ui.visuals().error_fg_color),
                )
                .wrap(),
            );
        }
        response
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
                                "更新插件"
                            } else {
                                "安装插件"
                            }),
                        )
                        .on_hover_text("安装或更新所选麦克风的VoiceBox插件")
                        .clicked()
                        && !jobs.busy()
                    {
                        self.confirm_uninstall = false;
                        let guid = self.devices[self.selected].guid.clone();
                        let guids = self.devices.iter().map(|mic| mic.guid.clone()).collect();
                        jobs.begin(
                            ui.ctx(),
                            if needs_update {
                                "正在更新插件…"
                            } else {
                                "正在安装插件…"
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
                                egui::Button::new("卸载插件"),
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
                    egui::Label::new(format!(
                        "确认卸载此麦克风的VoiceBox插件？\n{}",
                        self.devices[self.selected].name
                    ))
                    .wrap(),
                );
                ui.horizontal_wrapped(|ui| {
                    if ui.button("确认卸载").clicked() && !jobs.busy() {
                        self.confirm_uninstall = false;
                        let guid = self.devices[self.selected].guid.clone();
                        let guids = self.devices.iter().map(|mic| mic.guid.clone()).collect();
                        jobs.begin(ui.ctx(), "正在卸载插件…", move || {
                            JobResult::Plugin(devices::execute_plugin_action(
                                guid,
                                guids,
                                PluginAction::Uninstall,
                            ))
                        });
                    }
                    if ui.button("取消").clicked() {
                        self.confirm_uninstall = false;
                    }
                });
            }
        });
    }
}

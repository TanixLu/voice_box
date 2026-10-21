use super::{
    hotkey_binding::{HotkeyBinding, hotkey_badge, hotkey_editor},
    jobs::Jobs,
    widgets::{ButtonIcon, hint, icon_button, play_button, warning},
};
use crate::app::soundboard::{
    Soundboard,
    config::{BindingTarget, Config, Options},
};

struct SoundRename {
    index: usize,
    name: String,
    request_focus: bool,
    save_requested: bool,
}

impl SoundRename {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let input = ui
            .add_sized(
                [ui.available_width().max(16.0), 30.0],
                egui::TextEdit::singleline(&mut self.name).id_salt("sound_name"),
            )
            .on_hover_text("Enter 或点击别处保存，Esc 取消；扩展名保持不变");
        if self.request_focus && ui.is_enabled() {
            input.request_focus();
            let mut state = egui::TextEdit::load_state(ui.ctx(), input.id).unwrap_or_default();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::select_all_str(&self.name)));
            state.store(ui.ctx(), input.id);
            self.request_focus = false;
        } else if input.lost_focus() {
            self.save_requested = true;
        }
    }
}

pub(super) struct SoundboardPanel {
    pub(super) model: Soundboard,
    pub(super) options: Option<Options>,
    filter: String,
    rename: Option<SoundRename>,
    confirm_delete: Option<usize>,
}

impl SoundboardPanel {
    pub(super) fn new(config: Config) -> Self {
        Self {
            model: Soundboard::new(config),
            filter: String::new(),
            options: None,
            rename: None,
            confirm_delete: None,
        }
    }
    pub(super) fn renaming(&self) -> bool {
        self.rename.is_some()
    }
    pub(super) fn cancel_rename(&mut self) {
        self.rename = None;
    }
    pub(super) fn cancel_delete(&mut self) {
        self.confirm_delete = None;
    }

    fn save_sound_settings(&mut self, jobs: &mut Jobs, hotkeys: &HotkeyBinding) {
        let Some(options) = self.options.clone() else {
            return;
        };
        let result = [options.stop_hotkey, options.random_hotkey]
            .into_iter()
            .flatten()
            .try_for_each(|key| hotkeys.validate(key))
            .and_then(|()| self.model.save_options(options));
        match result {
            Ok(()) => {
                jobs.status.clear();
                jobs.failed = false;
            }
            Err(error) => {
                tracing::error!("Failed to save soundboard settings: {error:#}");
                jobs.status = format!("操作失败：{error:#}");
                jobs.failed = true;
            }
        }
    }

    pub(super) fn play_sound(&mut self, ctx: &egui::Context, index: usize, guid: Option<&str>) {
        self.model.play(index, guid, super::wake_ui(ctx.clone()));
    }

    pub(super) fn play_random_sound(&mut self, ctx: &egui::Context, guid: Option<&str>) {
        use std::hash::{BuildHasher, RandomState};
        let count = self.model.library().sounds.len();
        if count == 0 {
            return;
        }
        let index =
            (RandomState::new().hash_one(std::time::Instant::now()) % count as u64) as usize;
        self.play_sound(ctx, index, guid);
    }

    pub(super) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        jobs: &mut Jobs,
        session_busy: bool,
        hotkeys: &mut HotkeyBinding,
        guid: Option<&str>,
    ) {
        let available = !jobs.busy() && !session_busy && self.options.is_none();
        ui.add_enabled_ui(available, |ui| {
            ui.horizontal(|ui| {
                ui.strong(format!("音效库 · {} 个", self.model.library().sounds.len()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("设置").clicked() {
                        self.options = Some(self.model.options().clone());
                        jobs.status.clear();
                        jobs.failed = false;
                    }
                    if ui
                        .add_enabled(!self.model.importing(), egui::Button::new("导入"))
                        .clicked()
                    {
                        self.model.import(
                            super::picker::choose_files,
                            super::wake_ui(ui.ctx().clone()),
                        );
                    }
                    if self.model.importing() {
                        ui.spinner();
                    }
                });
            });
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("搜索音效")
                        .desired_width(f32::INFINITY),
                )
                .changed()
            {
                self.confirm_delete = None;
            }
            let filter = self.filter.to_lowercase();
            let indices: Vec<_> = self
                .model
                .library()
                .sounds
                .iter()
                .enumerate()
                .filter(|(index, sound)| {
                    self.rename
                        .as_ref()
                        .is_some_and(|edit| edit.index == *index)
                        || sound.name().to_lowercase().contains(&filter)
                })
                .map(|(index, _)| index)
                .collect();
            let mut play = None;
            let mut stop = false;
            let mut remove = None;
            let mut delete_clicked = false;
            let mut capture = None;
            let mut rename = None;
            let mut clear_hotkey = None;
            let mut rename_visible = false;
            let renaming = self.rename.is_some();
            let capture_target = hotkeys.target();
            let capturing = capture_target.is_some();
            let height = (ui.available_height() - 50.0).max(50.0);
            ui.scope(|ui| {
                // Keep the scroll bar outside the rows and leave a clear gap
                // between its hit area and the destructive action.
                ui.spacing_mut().scroll.floating = false;
                ui.spacing_mut().scroll.bar_inner_margin = 14.0;
                egui::ScrollArea::vertical()
                    .id_salt("sound_library")
                    .max_height(height)
                    .auto_shrink([false, false])
                    .show_rows(ui, 30.0, indices.len(), |ui, rows| {
                        for row in rows {
                            let index = indices[row];
                            let active = self.model.active == Some(index);
                            let sound = &self.model.library().sounds[index];
                            ui.push_id(index, |ui| {
                                ui.horizontal(|ui| {
                                    if play_button(ui, !capturing && !renaming, active)
                                        .on_hover_text(if active {
                                            "停止播放"
                                        } else {
                                            "播放音效"
                                        })
                                        .clicked()
                                    {
                                        if active {
                                            stop = true;
                                        } else {
                                            play = Some(index);
                                        }
                                    }
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let confirming = self.confirm_delete == Some(index);
                                            let icon = if confirming {
                                                ButtonIcon::ConfirmDelete
                                            } else {
                                                ButtonIcon::Delete
                                            };
                                            if icon_button(ui, !capturing && !renaming, icon)
                                                .clicked()
                                            {
                                                ui.ctx().request_repaint();
                                                delete_clicked = true;
                                                if confirming {
                                                    remove = Some(index);
                                                } else {
                                                    self.confirm_delete = Some(index);
                                                }
                                            }
                                            let duration = sound
                                                .duration
                                                .map(format_duration)
                                                .unwrap_or_else(|| {
                                                    if sound.duration_error.is_some() {
                                                        "失败".into()
                                                    } else {
                                                        "…".into()
                                                    }
                                                });
                                            ui.add_sized(
                                                [48.0, 30.0],
                                                egui::Label::new(duration).truncate(),
                                            )
                                            .on_hover_text(
                                                sound
                                                    .duration_error
                                                    .as_deref()
                                                    .unwrap_or("音频长度"),
                                            );
                                            if let Some(key) = sound.hotkey {
                                                if icon_button(
                                                    ui,
                                                    !capturing && !renaming,
                                                    ButtonIcon::ClearHotkey,
                                                )
                                                .clicked()
                                                {
                                                    clear_hotkey = Some(index);
                                                }
                                                let max_width =
                                                    (ui.available_width() - 74.0).max(24.0);
                                                hotkey_badge(ui, key, max_width);
                                            } else {
                                                let width = (ui.available_width() - 74.0)
                                                    .clamp(24.0, 132.0);
                                                let binding = capture_target
                                                    == Some(BindingTarget::Sound(index));
                                                let label = if binding {
                                                    "请按键…"
                                                } else {
                                                    "绑定快捷键"
                                                };
                                                if ui
                                                    .add_enabled_ui(
                                                        (!capturing || binding) && !renaming,
                                                        |ui| {
                                                            ui.add_sized(
                                                                [width, 30.0],
                                                                egui::Button::new(label).truncate(),
                                                            )
                                                        },
                                                    )
                                                    .inner
                                                    .on_hover_text(if binding {
                                                        "请按下快捷键，按 Esc 或再次点击取消"
                                                    } else {
                                                        "绑定快捷键"
                                                    })
                                                    .clicked()
                                                {
                                                    capture = Some(BindingTarget::Sound(index));
                                                }
                                            }
                                            if let Some(edit) = &mut self.rename
                                                && edit.index == index
                                            {
                                                rename_visible = true;
                                                edit.ui(ui);
                                            } else {
                                                let name = if active {
                                                    format!("播放中 · {}", sound.name())
                                                } else {
                                                    sound.name().to_owned()
                                                };
                                                let response = ui
                                                    .add_sized(
                                                        [ui.available_width().max(16.0), 30.0],
                                                        egui::Button::new(name)
                                                            .frame_when_inactive(false)
                                                            .truncate(),
                                                    )
                                                    .on_hover_text(format!(
                                                        "{}\n双击重命名",
                                                        sound.path.display()
                                                    ));
                                                if response.double_clicked()
                                                    && !capturing
                                                    && !renaming
                                                {
                                                    rename = Some((index, sound.name().to_owned()));
                                                }
                                            }
                                        },
                                    );
                                });
                            });
                        }
                    });
            });
            if let Some(index) = clear_hotkey
                && let Err(error) = self
                    .model
                    .bind_hotkey(BindingTarget::Sound(index), None, None)
            {
                tracing::error!("Failed to clear hotkey: {error:#}");
                self.model.error(format!("{error:#}"));
            }
            if let Some(edit) = &mut self.rename
                && !rename_visible
                && !edit.request_focus
            {
                edit.save_requested = true;
            }
            if !delete_clicked && ui.input(|input| input.pointer.any_click()) {
                self.confirm_delete = None;
            }
            if indices.is_empty() {
                // The empty message is painted inside the list's reserved space.
                let text = if self.model.library().sounds.is_empty() {
                    "点击「导入」添加音效，可一次选择多个文件"
                } else {
                    "没有匹配的音效"
                };
                ui.painter().text(
                    ui.min_rect().center(),
                    egui::Align2::CENTER_CENTER,
                    text,
                    egui::TextStyle::Body.resolve(ui.style()),
                    ui.visuals().weak_text_color(),
                );
            }
            if stop {
                self.model.stop();
            } else if let Some(index) = play {
                self.play_sound(ui.ctx(), index, guid);
            }
            if let Some(index) = remove {
                self.confirm_delete = None;
                self.model.remove(index);
            }
            if let Some(target) = capture {
                if capture_target == Some(target) {
                    hotkeys.cancel_capture();
                } else {
                    hotkeys.begin_capture(target, &mut self.model);
                }
            }
            if let Some((index, name)) = rename {
                self.model.stop();
                self.confirm_delete = None;
                self.rename = Some(SoundRename {
                    index,
                    name,
                    request_focus: true,
                    save_requested: false,
                });
                ui.ctx().request_repaint();
            }
            ui.horizontal(|ui| {
                let key = self
                    .model
                    .options()
                    .stop_hotkey
                    .map(|key| format!("停止播放 · {}", key.label()))
                    .unwrap_or_else(|| "停止播放".into());
                if ui
                    .add_enabled(self.model.playing(), egui::Button::new(key))
                    .clicked()
                {
                    self.model.stop();
                }
                let error = if !hotkeys.error().is_empty() {
                    Some(hotkeys.error())
                } else if self.model.failed {
                    Some(self.model.message.as_str())
                } else {
                    None
                };
                if let Some(error) = error {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(error).color(ui.visuals().error_fg_color),
                        )
                        .truncate(),
                    )
                    .on_hover_text(error);
                } else if let Some(index) = self.model.active {
                    let name = self
                        .model
                        .library()
                        .sounds
                        .get(index)
                        .map(|sound| sound.name())
                        .unwrap_or("");
                    ui.add(
                        egui::Label::new(format!(
                            "{name} · {}",
                            format_duration(self.model.elapsed)
                        ))
                        .truncate(),
                    );
                }
            });
        });
    }

    pub(super) fn settings_ui(
        &mut self,
        ctx: &egui::Context,
        jobs: &mut Jobs,
        hotkeys: &mut HotkeyBinding,
    ) {
        if self.options.is_none() {
            return;
        }
        let available = ctx.content_rect().size();
        let capture_target = hotkeys.target();
        let capturing = capture_target.is_some();
        let idle = !jobs.busy();
        let mut capture = None;
        let mut save = false;
        let mut close = false;
        let response = egui::Modal::new(egui::Id::unique("sound_settings"))
            .frame(egui::Frame::popup(&ctx.style_of(ctx.theme())).inner_margin(16))
            .show(ctx, |ui| {
                ui.set_width((available.x - 64.0).clamp(240.0, 460.0));
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("音效设置").size(16.0).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close = icon_button(ui, idle, ButtonIcon::Close).clicked();
                    });
                });
                ui.add_space(12.0);
                egui::ScrollArea::vertical()
                    .max_height((available.y - 230.0).max(80.0))
                    .show(ui, |ui| {
                        ui.add_enabled_ui(idle, |ui| {
                            let options = self.options.as_mut().unwrap();
                            playback_volume(ui, "麦克风输出", &mut options.send_volume);
                            playback_volume(ui, "扬声器输出", &mut options.monitor_volume);
                            ui.checkbox(&mut options.microphone, "音效播放时保持收音");
                            ui.checkbox(&mut options.show_overlay, "显示播放浮层");
                            ui.checkbox(&mut options.repeat, "循环播放");
                            if hotkey_editor(
                                ui,
                                "播放时自动按住",
                                &mut options.hold,
                                BindingTarget::Hold,
                                capture_target,
                            ) {
                                capture = Some(BindingTarget::Hold);
                            }
                            if hotkey_editor(
                                ui,
                                "停止播放快捷键",
                                &mut options.stop_hotkey,
                                BindingTarget::Stop,
                                capture_target,
                            ) {
                                capture = Some(BindingTarget::Stop);
                            }
                            if hotkey_editor(
                                ui,
                                "随机播放快捷键",
                                &mut options.random_hotkey,
                                BindingTarget::Random,
                                capture_target,
                            ) {
                                capture = Some(BindingTarget::Random);
                            }
                            if !hotkeys.error().is_empty() {
                                warning(ui, hotkeys.error());
                            }
                        });
                    });
                ui.add_space(12.0);
                let dirty = self
                    .options
                    .as_ref()
                    .is_some_and(|options| options != self.model.options());
                ui.horizontal(|ui| {
                    save = ui
                        .add_enabled(idle && !capturing && dirty, egui::Button::new("保存设置"))
                        .clicked();
                    if ui
                        .add_enabled(idle && !capturing && dirty, egui::Button::new("撤销修改"))
                        .clicked()
                    {
                        self.options = Some(self.model.options().clone());
                        hotkeys.message.clear();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                idle && !capturing
                                    && self
                                        .options
                                        .as_ref()
                                        .is_some_and(|options| *options != Options::default()),
                                egui::Button::new("恢复默认"),
                            )
                            .clicked()
                        {
                            self.options = Some(Options::default());
                            hotkeys.message.clear();
                        }
                    });
                });
                if self
                    .options
                    .as_ref()
                    .is_some_and(|options| options != self.model.options())
                {
                    warning(ui, "有未保存的修改，保存后生效；关闭将放弃未保存的修改。");
                } else {
                    hint(ui, "当前设置已生效。");
                }
                if !idle {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(&jobs.status);
                    });
                } else if jobs.failed {
                    warning(ui, &jobs.status);
                }
            });
        if close || (response.should_close() && !capturing && idle) {
            self.options = None;
            hotkeys.cancel_capture();
        } else if save {
            self.save_sound_settings(jobs, hotkeys);
        } else if let Some(target) = capture {
            if capture_target == Some(target) {
                hotkeys.cancel_capture();
            } else {
                hotkeys.begin_capture(target, &mut self.model);
            }
            ctx.request_repaint();
        }
    }

    pub(super) fn finish_rename(&mut self, active: bool) {
        let Some(edit) = &mut self.rename else {
            return;
        };
        if !edit.request_focus && (!active || self.options.is_some()) {
            edit.save_requested = true;
        }
        // Stopping playback is asynchronous; keep the edit until the file is released.
        if edit.save_requested && !self.model.playing() {
            match self.model.rename(edit.index, &edit.name) {
                Ok(()) => {
                    self.rename = None;
                    self.model.failed = false;
                    self.model.message.clear();
                }
                Err(error) => {
                    edit.save_requested = false;
                    edit.request_focus = true;
                    self.model.error(format!("{error:#}"));
                }
            }
        }
    }
}

pub(super) fn format_duration(seconds: f64) -> String {
    let total_seconds = seconds.max(0.0) as u64;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    if minutes >= 60 {
        format!("{}:{:02}:{seconds:02}", minutes / 60, minutes % 60)
    } else {
        format!("{minutes}:{seconds:02}")
    }
}
fn playback_volume(ui: &mut egui::Ui, label: &str, volume: &mut f32) -> bool {
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        ui.add(
            egui::Slider::new(volume, 0.0..=2.0)
                .step_by(0.01)
                .custom_formatter(|value, _| {
                    if value == 0.0 {
                        "不输出".into()
                    } else {
                        format!("{:.0}%", value * 100.0)
                    }
                })
                .custom_parser(|text| {
                    if text.trim() == "不输出" {
                        Some(0.0)
                    } else {
                        text.trim()
                            .trim_end_matches('%')
                            .trim()
                            .parse::<f64>()
                            .ok()
                            .filter(|value| value.is_finite())
                            .map(|value| value / 100.0)
                    }
                }),
        )
        .changed()
    })
    .inner
}

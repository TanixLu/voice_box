use super::widgets::{ButtonIcon, ICON_BUTTON_SIZE, icon_button};
use crate::app::{
    hotkeys::{self, Hotkey, Hotkeys},
    soundboard::{
        Soundboard,
        config::{BindingTarget, Options},
    },
};
use std::hash::{BuildHasher, RandomState};

struct KeyCapture {
    target: BindingTarget,
    armed: bool,
    modifier: Option<Hotkey>,
}
pub(super) enum PlaybackAction {
    Stop,
    Random,
    Play(usize),
}

pub(super) struct HotkeyManager {
    listener: Option<Hotkeys>,
    bindings: Vec<Option<Hotkey>>,
    attempted_bindings: Vec<Option<Hotkey>>,
    revision: u64,
    capture: Option<KeyCapture>,
    registration_error: String,
    pub(super) message: String,
}
impl HotkeyManager {
    pub(super) fn new(ctx: &egui::Context) -> Self {
        let (listener, message) = match Hotkeys::new(super::wake_ui(ctx.clone())) {
            Ok(listener) => (Some(listener), String::new()),
            Err(error) => (None, format!("无法启用全局快捷键：{error}")),
        };
        Self {
            listener,
            bindings: Vec::new(),
            attempted_bindings: Vec::new(),
            revision: 0,
            capture: None,
            registration_error: String::new(),
            message,
        }
    }
    pub(super) fn target(&self) -> Option<BindingTarget> {
        self.capture.as_ref().map(|capture| capture.target)
    }
    pub(super) fn error(&self) -> &str {
        if self.message.is_empty() {
            &self.registration_error
        } else {
            &self.message
        }
    }
    pub(super) fn cancel_capture(&mut self) {
        self.capture = None;
    }
    pub(super) fn validate(&self, key: Hotkey) -> anyhow::Result<()> {
        self.listener
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("全局快捷键服务不可用"))?
            .validate(key)
    }
    pub(super) fn poll(&mut self, enabled: bool, soundboard: &Soundboard) -> Vec<PlaybackAction> {
        let enabled = enabled && self.capture.is_none();
        let bindings: Vec<_> = if enabled {
            std::iter::once(soundboard.options().stop_hotkey)
                .chain(std::iter::once(soundboard.options().random_playback_hotkey))
                .chain(soundboard.library().sounds.iter().map(|sound| sound.hotkey))
                .collect()
        } else {
            Vec::new()
        };
        let changed = bindings != self.bindings;
        let mut triggers = Vec::new();
        if let Some(hotkeys) = &self.listener {
            while let Ok(event) = hotkeys.receiver.try_recv() {
                match event {
                    hotkeys::Event::Trigger { index, revision }
                        if enabled && !changed && revision == self.revision =>
                    {
                        triggers.push(index)
                    }
                    hotkeys::Event::Error(error) => self.registration_error = error,
                    _ => {}
                }
            }
            if bindings != self.attempted_bindings {
                let revision = self.revision.wrapping_add(1);
                match hotkeys.configure(revision, bindings.clone()) {
                    Ok(()) => {
                        self.revision = revision;
                        self.bindings = bindings.clone();
                        self.registration_error.clear();
                    }
                    Err(error) => self.registration_error = format!("{error:#}"),
                }
                self.attempted_bindings = bindings;
            }
        }

        // Stop takes precedence over playback queued in the same frame.
        if triggers.contains(&0) {
            vec![PlaybackAction::Stop]
        } else {
            triggers
                .into_iter()
                .map(|index| {
                    if index == 1 {
                        PlaybackAction::Random
                    } else {
                        let candidates: Vec<_> = soundboard
                            .library()
                            .sounds
                            .iter()
                            .enumerate()
                            .filter_map(|(sound_index, sound)| {
                                (sound.hotkey == self.bindings[index]).then_some(sound_index)
                            })
                            .collect();
                        let choice = (RandomState::new().hash_one(std::time::Instant::now())
                            % candidates.len() as u64)
                            as usize;
                        PlaybackAction::Play(candidates[choice])
                    }
                })
                .collect()
        }
    }

    pub(super) fn poll_capture(
        &mut self,
        ctx: &egui::Context,
        soundboard: &mut Soundboard,
        draft: Option<&mut Options>,
    ) {
        let Some(capture) = &mut self.capture else {
            return;
        };
        ctx.request_repaint_after(std::time::Duration::from_millis(20));
        if !ctx.input(|input| input.focused) {
            self.cancel_capture();
            return;
        }
        let pressed = Hotkey::pressed();
        let modifier = Hotkey::pressed_modifier();
        if !capture.armed {
            capture.armed = pressed.is_none() && modifier.is_none();
            return;
        }
        let pressed = if capture.target == BindingTarget::Hold && pressed.is_none() {
            if modifier.is_some() {
                capture.modifier = modifier;
                return;
            }
            capture.modifier.take()
        } else {
            pressed
        };
        let Some(key) = pressed else {
            return;
        };
        let target = capture.target;
        self.capture = None;
        if key.virtual_key_code == 0x1b {
            return;
        }
        let result = if target == BindingTarget::Hold {
            Ok(())
        } else {
            self.validate(key)
        }
        .and_then(|()| soundboard.bind_hotkey(target, Some(key), draft));
        match result {
            Ok(()) => self.message.clear(),
            Err(error) => {
                tracing::error!("Failed to bind hotkey: {error:#}");
                self.message = format!("{error:#}");
            }
        }
    }

    pub(super) fn begin_capture(&mut self, target: BindingTarget, soundboard: &mut Soundboard) {
        soundboard.stop();
        self.capture = Some(KeyCapture {
            target,
            armed: false,
            modifier: None,
        });
        self.message.clear();
    }
}

fn hotkey_badge_size(ui: &egui::Ui, key: Hotkey, max_width: f32) -> egui::Vec2 {
    let text_size = ui
        .painter()
        .layout_no_wrap(
            key.label(),
            egui::TextStyle::Body.resolve(ui.style()),
            ui.visuals().text_color(),
        )
        .size();
    egui::vec2(
        (text_size.x + 18.0).clamp(24.0, max_width.max(24.0)),
        text_size.y + 10.0,
    )
}

pub(super) fn hotkey_badge(ui: &mut egui::Ui, key: Hotkey, max_width: f32) {
    let text = key.label();
    let font = egui::TextStyle::Body.resolve(ui.style());
    let size = hotkey_badge_size(ui, key, max_width);
    // Allocate and paint only the text's measured width. A Frame inherits the
    // centered column's bounds and can stretch its border across the whole row.
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect(
        rect,
        4,
        ui.visuals().extreme_bg_color,
        ui.visuals().widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    );
    ui.place(
        rect.shrink2(egui::vec2(9.0, 5.0)),
        egui::Label::new(egui::RichText::new(&text).font(font)).truncate(),
    );
    response.on_hover_text(text);
}

pub(super) fn hotkey_editor(
    ui: &mut egui::Ui,
    label: &str,
    key: &mut Option<Hotkey>,
    target: BindingTarget,
    capture_target: Option<BindingTarget>,
) -> bool {
    let binding = capture_target == Some(target);
    let enabled = capture_target.is_none() || binding;
    ui.push_id(label, |ui| {
        ui.horizontal(|ui| {
            ui.label(label);
            let mut capture = false;
            let hotkey = *key;
            let button_text = if binding {
                "请按键…"
            } else {
                "绑定快捷键"
            };
            let available_width = ui.available_width();
            let button_width = if hotkey.is_some() {
                ICON_BUTTON_SIZE
            } else {
                (ui.painter()
                    .layout_no_wrap(
                        button_text.into(),
                        egui::TextStyle::Button.resolve(ui.style()),
                        ui.visuals().text_color(),
                    )
                    .size()
                    .x
                    + 2.0 * ui.spacing().button_padding.x)
                    .min(available_width)
            };
            let badge_width = hotkey.map(|hotkey| {
                hotkey_badge_size(
                    ui,
                    hotkey,
                    available_width - button_width - ui.spacing().item_spacing.x,
                )
                .x
            });
            let group_width =
                button_width + badge_width.map_or(0.0, |width| width + ui.spacing().item_spacing.x);
            let (row_rect, _) =
                ui.allocate_exact_size(egui::vec2(available_width, 30.0), egui::Sense::hover());
            // Center the key and clear button together as one compact group.
            let group_rect =
                egui::Rect::from_center_size(row_rect.center(), egui::vec2(group_width, 30.0));
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(group_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    if let Some(hotkey) = hotkey {
                        hotkey_badge(ui, hotkey, badge_width.unwrap());
                        if icon_button(ui, enabled, ButtonIcon::ClearHotkey).clicked() {
                            *key = None;
                        }
                    } else {
                        capture = ui
                            .add_enabled(enabled, egui::Button::new(button_text).truncate())
                            .on_hover_text(if binding {
                                "请按下快捷键，按 Esc 或再次点击取消"
                            } else {
                                "绑定快捷键"
                            })
                            .clicked();
                    }
                },
            );
            capture
        })
        .inner
    })
    .inner
}

use super::widgets::{ButtonIcon, ICON_BUTTON_SIZE, icon_button};
use crate::app::i18n::Text;
use crate::app::i18n::{Details, Message};
use crate::app::{
    hotkeys::{self, Hotkey, Hotkeys},
    soundboard::{
        Soundboard,
        config::{BindingTarget, Options},
    },
};
use std::{collections::HashSet, path::PathBuf};

struct KeyCapture {
    target: BindingTarget,
    paths: Option<HashSet<PathBuf>>,
    armed: bool,
    modifier: Option<Hotkey>,
}
pub(super) enum PlaybackAction {
    Stop,
    Random(Option<Hotkey>),
}

#[derive(Default)]
pub(super) struct HotkeyManager {
    listener: Option<Hotkeys>,
    bindings: Vec<Option<Hotkey>>,
    attempted_bindings: Vec<Option<Hotkey>>,
    revision: u64,
    capture: Option<KeyCapture>,
    registration_error: Message,
    pub(super) message: Message,
    pub(super) settings_message: Message,
}
impl HotkeyManager {
    pub(super) fn new(ctx: &egui::Context) -> Self {
        let (listener, message) = match Hotkeys::new(super::wake_ui(ctx.clone())) {
            Ok(listener) => (Some(listener), Message::default()),
            Err(error) => (
                None,
                Message::EnableHotkeysFailed {
                    error: Details::new(error),
                },
            ),
        };
        Self {
            listener,
            bindings: Vec::new(),
            attempted_bindings: Vec::new(),
            revision: 0,
            capture: None,
            registration_error: Message::default(),
            message,
            settings_message: Message::default(),
        }
    }
    pub(super) fn target(&self) -> Option<BindingTarget> {
        self.capture.as_ref().map(|capture| capture.target)
    }
    pub(super) fn batch_capturing(&self) -> bool {
        self.capture
            .as_ref()
            .is_some_and(|capture| capture.paths.is_some())
    }
    pub(super) fn error(&self) -> &Message {
        if self.message.is_empty() {
            &self.registration_error
        } else {
            &self.message
        }
    }
    pub(super) fn clear_error(&mut self) {
        self.message.clear();
        self.registration_error.clear();
    }
    pub(super) fn cancel_capture(&mut self) {
        self.capture = None;
    }
    pub(super) fn validate(&self, key: Hotkey) -> anyhow::Result<()> {
        self.listener
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(Text::HotkeyServiceUnavailable))?
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
                    Err(error) => self.registration_error = Message::error(error),
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
                .filter_map(|index| {
                    if index == 1 {
                        Some(PlaybackAction::Random(None))
                    } else {
                        self.bindings
                            .get(index)
                            .copied()
                            .flatten()
                            .map(|key| PlaybackAction::Random(Some(key)))
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
        if !ctx.input(|input| input.focused)
            || ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
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
        let paths = capture.paths.take();
        self.capture = None;
        if key.virtual_key_code == 0x1b {
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            return;
        }
        let result = if target == BindingTarget::Hold {
            Ok(())
        } else {
            self.validate(key)
        }
        .and_then(|()| {
            if let Some(paths) = paths {
                soundboard.bind_sound_hotkeys(&paths, Some(key))
            } else {
                soundboard.bind_hotkey(target, Some(key), draft)
            }
        });
        let message = if matches!(target, BindingTarget::Sound(_)) {
            &mut self.message
        } else {
            &mut self.settings_message
        };
        match result {
            Ok(()) => message.clear(),
            Err(error) => {
                tracing::error!("Failed to bind hotkey: {error:#}");
                *message = Message::error(error);
            }
        }
    }

    pub(super) fn begin_capture(&mut self, target: BindingTarget, soundboard: &mut Soundboard) {
        soundboard.stop();
        self.capture = Some(KeyCapture {
            target,
            paths: None,
            armed: false,
            modifier: None,
        });
        if matches!(target, BindingTarget::Sound(_)) {
            self.message.clear();
        } else {
            self.settings_message.clear();
        }
    }

    pub(super) fn begin_batch_capture(
        &mut self,
        paths: &HashSet<PathBuf>,
        soundboard: &mut Soundboard,
    ) {
        let Some(index) = soundboard
            .library()
            .sounds
            .iter()
            .position(|sound| paths.contains(&sound.path))
        else {
            return;
        };
        self.begin_capture(BindingTarget::Sound(index), soundboard);
        self.capture.as_mut().unwrap().paths = Some(paths.clone());
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
        egui::Label::new(egui::RichText::new(&text).font(font))
            .truncate()
            .show_tooltip_when_elided(false),
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
                Text::PressKey.text()
            } else {
                Text::BindHotkey.text()
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
                                Text::CaptureHotkeyHint.text()
                            } else {
                                Text::BindHotkey.text()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{
        gui::soundboard_panel::SoundboardPanel,
        soundboard::{
            config::Config,
            library::{Library, Sound},
        },
    };

    #[test]
    fn playback_clears_old_hotkey_errors_and_preserves_new_playback_errors() {
        let ctx = egui::Context::default();
        for random in [false, true] {
            let mut panel = SoundboardPanel::new(Config {
                options: Options {
                    speaker_output_volume: 0.0,
                    microphone_output_volume: 0.0,
                    ..Options::default()
                },
                library: Library {
                    sounds: vec![Sound {
                        file_name: "test.wav".into(),
                        ..Sound::default()
                    }],
                },
            });
            let mut hotkeys = HotkeyManager {
                message: Message::error(anyhow::anyhow!("old binding error")),
                registration_error: Message::error(anyhow::anyhow!("old registration error")),
                ..HotkeyManager::default()
            };
            if random {
                panel.play_random_sound(&ctx, None, &mut hotkeys);
            } else {
                panel.play_sound(&ctx, 0, &mut hotkeys);
            }
            assert!(hotkeys.error().is_empty());
            assert!(panel.model.failed);
            assert_eq!(panel.model.message.text(), Text::OutputsMuted.text());
        }
    }
}

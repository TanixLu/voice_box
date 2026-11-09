use std::collections::{HashMap, HashSet};

use crate::{
    hotkeys::{Hotkey, Hotkeys},
    i18n::{Details, Message, Text},
    settings::BindingTarget,
    soundboard::sound::SoundId,
};

struct KeyCapture {
    target: BindingTarget,
    ids: Option<HashSet<SoundId>>,
    armed: bool,
    modifier: Option<Hotkey>,
}
pub struct CapturedBinding {
    pub target: BindingTarget,
    pub ids: Option<HashSet<SoundId>>,
    pub key: Hotkey,
}

#[derive(Clone, Copy)]
pub enum PlaybackAction {
    Stop,
    Random(Option<Hotkey>),
}

#[derive(Default)]
pub struct HotkeyManager {
    listener: Option<Hotkeys>,
    /// Last key set sent to the hotkey thread, so a failure is not retried every frame.
    attempted: HashSet<Hotkey>,
    capture: Option<KeyCapture>,
    registration_error: Message,
    pub message: Message,
    pub settings_message: Message,
}
impl HotkeyManager {
    pub fn new(ctx: &egui::Context) -> Self {
        match Hotkeys::new(super::main_window(), super::wake_ui(ctx.clone())) {
            Ok(listener) => Self {
                listener: Some(listener),
                ..Default::default()
            },
            Err(error) => Self {
                message: Message::EnableHotkeysFailed {
                    error: Details::new(error),
                },
                ..Default::default()
            },
        }
    }
    pub fn target(&self) -> Option<BindingTarget> {
        self.capture.as_ref().map(|capture| capture.target)
    }
    pub fn is_capturing(&self) -> bool {
        self.target().is_some()
    }
    pub fn batch_capturing(&self) -> bool {
        self.capture
            .as_ref()
            .is_some_and(|capture| capture.ids.is_some())
    }
    pub fn error(&self) -> &Message {
        if self.message.is_empty() {
            &self.registration_error
        } else {
            &self.message
        }
    }
    pub fn clear_error(&mut self) {
        self.message.clear();
        self.registration_error.clear();
    }
    pub fn cancel_capture(&mut self) {
        self.capture = None;
    }
    pub fn shutdown(&mut self) {
        self.listener = None;
    }
    pub fn validate(&self, key: Hotkey) -> anyhow::Result<()> {
        self.listener
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(Text::HotkeyServiceUnavailable))?
            .validate(key)
    }
    pub fn poll(
        &mut self,
        enabled: bool,
        shortcuts: impl IntoIterator<Item = (BindingTarget, Hotkey)>,
    ) -> Vec<PlaybackAction> {
        let Some(hotkeys) = &mut self.listener else {
            return Vec::new();
        };
        let bindings: HashMap<Hotkey, PlaybackAction> = if enabled && self.capture.is_none() {
            shortcuts
                .into_iter()
                .map(|(target, key)| {
                    let action = match target {
                        BindingTarget::Stop => PlaybackAction::Stop,
                        BindingTarget::Random => PlaybackAction::Random(None),
                        BindingTarget::Sound(_) | BindingTarget::Hold => {
                            PlaybackAction::Random(Some(key))
                        }
                    };
                    (key, action)
                })
                .collect()
        } else {
            HashMap::new()
        };
        let actions: Vec<_> = hotkeys
            .pressed
            .try_iter()
            .filter_map(|key| bindings.get(&key).copied())
            .collect();
        let keys: HashSet<_> = bindings.into_keys().collect();
        if keys != self.attempted {
            match hotkeys.configure(keys.clone()) {
                Ok(()) => self.registration_error.clear(),
                Err(error) => self.registration_error = Message::error(error),
            }
            self.attempted = keys;
        }
        // Stop takes precedence over playback queued in the same frame.
        if actions
            .iter()
            .any(|action| matches!(action, PlaybackAction::Stop))
        {
            vec![PlaybackAction::Stop]
        } else {
            actions
        }
    }

    pub fn poll_capture(&mut self, ctx: &egui::Context) -> Option<CapturedBinding> {
        let Some(capture) = &mut self.capture else {
            return None;
        };
        ctx.request_repaint_after(std::time::Duration::from_millis(20));
        if !ctx.input(|input| input.focused)
            || ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.cancel_capture();
            return None;
        }
        let pressed = Hotkey::pressed();
        let modifier = Hotkey::pressed_modifier();
        if !capture.armed {
            capture.armed = pressed.is_none() && modifier.is_none();
            return None;
        }
        let pressed = if capture.target == BindingTarget::Hold && pressed.is_none() {
            if modifier.is_some() {
                capture.modifier = modifier;
                return None;
            }
            capture.modifier.take()
        } else {
            pressed
        };
        let key = pressed?;
        let target = capture.target;
        let ids = capture.ids.take();
        self.capture = None;
        if key.is_escape() {
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            return None;
        }
        if target != BindingTarget::Hold
            && let Err(error) = self.validate(key)
        {
            self.capture_result(target, Err(error));
            return None;
        }
        Some(CapturedBinding { target, ids, key })
    }

    fn message_for(&mut self, target: BindingTarget) -> &mut Message {
        if matches!(target, BindingTarget::Sound(_)) {
            &mut self.message
        } else {
            &mut self.settings_message
        }
    }

    pub fn capture_result(&mut self, target: BindingTarget, result: anyhow::Result<()>) {
        let message = self.message_for(target);
        match result {
            Ok(()) => message.clear(),
            Err(error) => {
                tracing::error!("Failed to bind hotkey: {error:#}");
                *message = Message::error(error);
            }
        }
    }

    /// `ids` binds every selected sound at once instead of only `target`.
    pub fn begin_capture(&mut self, target: BindingTarget, ids: Option<HashSet<SoundId>>) {
        self.capture = Some(KeyCapture {
            target,
            ids,
            armed: false,
            modifier: None,
        });
        self.message_for(target).clear();
    }
}

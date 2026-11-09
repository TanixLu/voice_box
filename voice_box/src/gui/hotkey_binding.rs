//! Global shortcut registration and interactive key capture.

use std::collections::{HashMap, HashSet};

use crate::{
    hotkeys::{self, Hotkeys},
    i18n::{Details, Message, Text},
    model::Hotkey,
    soundboard::bindings::{Action, Target},
};

struct KeyCapture {
    target: Target,
    armed: bool,
    modifier: Option<Hotkey>,
}

#[derive(Default)]
pub struct HotkeyManager {
    listener: Option<Hotkeys>,
    /// Last key set sent for registration, so a failure is not retried every frame.
    attempted: HashSet<Hotkey>,
    capture: Option<KeyCapture>,
    error: Message,
}
impl HotkeyManager {
    pub fn new(ctx: &egui::Context) -> Self {
        match Hotkeys::new(super::main_window(), super::wake_ui(ctx.clone())) {
            Ok(listener) => Self {
                listener: Some(listener),
                ..Default::default()
            },
            Err(error) => Self {
                error: Message::EnableHotkeysFailed {
                    error: Details::new(error),
                },
                ..Default::default()
            },
        }
    }
    pub fn target(&self) -> Option<&Target> {
        self.capture.as_ref().map(|capture| &capture.target)
    }
    /// The hotkey service or registration failure, if any.
    pub fn error(&self) -> &Message {
        &self.error
    }
    pub fn cancel_capture(&mut self) {
        self.capture = None;
    }
    pub fn begin_capture(&mut self, target: Target) {
        self.capture = Some(KeyCapture {
            target,
            armed: false,
            modifier: None,
        });
    }
    pub fn shutdown(&mut self) {
        self.listener = None;
    }
    /// Checks that Windows will accept `key` as a global shortcut.
    pub fn validate(&self, key: Hotkey) -> anyhow::Result<()> {
        self.listener
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(Text::HotkeyServiceUnavailable))?
            .validate(key)
    }

    /// Registers `bindings` and returns the actions pressed since the last poll.
    pub fn poll(&mut self, enabled: bool, bindings: HashMap<Hotkey, Action>) -> Vec<Action> {
        let Some(hotkeys) = &mut self.listener else {
            return Vec::new();
        };
        let bindings = if enabled && self.capture.is_none() {
            bindings
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
                Ok(()) => self.error.clear(),
                Err(error) => self.error = Message::error(error),
            }
            self.attempted = keys;
        }
        // Stop takes precedence over playback queued in the same frame.
        if actions.iter().any(|action| matches!(action, Action::Stop)) {
            vec![Action::Stop]
        } else {
            actions
        }
    }

    /// The key pressed for the current capture, once it is complete.
    pub fn poll_capture(
        &mut self,
        ctx: &egui::Context,
    ) -> Option<(Target, anyhow::Result<Hotkey>)> {
        let capture = self.capture.as_mut()?;
        ctx.request_repaint_after(std::time::Duration::from_millis(20));
        if !ctx.input(|input| input.focused)
            || ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.cancel_capture();
            return None;
        }
        let pressed = hotkeys::pressed();
        let modifier = hotkeys::pressed_modifier();
        if !capture.armed {
            capture.armed = pressed.is_none() && modifier.is_none();
            return None;
        }
        let hold = capture.target == Target::Hold;
        let pressed = if hold && pressed.is_none() {
            if modifier.is_some() {
                capture.modifier = modifier;
                return None;
            }
            capture.modifier.take()
        } else {
            pressed
        };
        let key = pressed?;
        let target = self.capture.take()?.target;
        if key.is_escape() {
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            return None;
        }
        // The hold key is simulated rather than registered.
        let result = if hold { Ok(()) } else { self.validate(key) };
        Some((target, result.map(|()| key)))
    }
}

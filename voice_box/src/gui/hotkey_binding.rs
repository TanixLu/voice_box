use std::collections::HashSet;

use crate::{
    hotkeys::{self, Hotkey, Hotkeys},
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

pub enum PlaybackAction {
    Stop,
    Random(Option<Hotkey>),
}

#[derive(Default)]
pub struct HotkeyManager {
    listener: Option<Hotkeys>,
    bindings: Vec<Option<Hotkey>>,
    attempted_bindings: Vec<Option<Hotkey>>,
    revision: u64,
    capture: Option<KeyCapture>,
    registration_error: Message,
    pub message: Message,
    pub settings_message: Message,
}
impl HotkeyManager {
    pub fn new(ctx: &egui::Context) -> Self {
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
    pub fn validate(&self, key: Hotkey) -> anyhow::Result<()> {
        self.listener
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(Text::HotkeyServiceUnavailable))?
            .validate(key)
    }
    pub fn poll(
        &mut self,
        enabled: bool,
        bindings: impl IntoIterator<Item = Option<Hotkey>>,
    ) -> Vec<PlaybackAction> {
        let enabled = enabled && self.capture.is_none();
        let bindings: Vec<_> = if enabled {
            bindings.into_iter().collect()
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
        if key.virtual_key_code == 0x1b {
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

    pub fn capture_result(&mut self, target: BindingTarget, result: anyhow::Result<()>) {
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

    pub fn begin_capture(&mut self, target: BindingTarget) {
        self.capture = Some(KeyCapture {
            target,
            ids: None,
            armed: false,
            modifier: None,
        });
        if matches!(target, BindingTarget::Sound(_)) {
            self.message.clear();
        } else {
            self.settings_message.clear();
        }
    }

    pub fn begin_batch_capture(&mut self, target: BindingTarget, ids: &HashSet<SoundId>) {
        self.begin_capture(target);
        self.capture.as_mut().unwrap().ids = Some(ids.clone());
    }
}

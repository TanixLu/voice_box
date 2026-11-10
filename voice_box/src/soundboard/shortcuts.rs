//! Global shortcut registration for the soundboard's key bindings.

use std::collections::{HashMap, HashSet};

use windows::Win32::Foundation::HWND;

use crate::{
    hotkeys::Hotkeys,
    i18n::{Details, Message, Text},
    model::Hotkey,
    soundboard::bindings::Action,
    task::Wake,
};

pub struct Shortcuts {
    listener: Option<Hotkeys>,
    /// Last key set sent for registration, so a failure is not retried every frame.
    attempted: HashSet<Hotkey>,
    error: Message,
}

impl Shortcuts {
    pub fn new(window: HWND, wake: Wake) -> Self {
        let (listener, error) = match Hotkeys::new(window, wake) {
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
            attempted: HashSet::new(),
            error,
        }
    }
    /// The hotkey service or registration failure, if any.
    pub fn error(&self) -> &Message {
        &self.error
    }
    /// Unregisters everything; must run while the main window still exists.
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

    /// Registers exactly `bindings` and returns the actions pressed since the last poll.
    pub fn poll(&mut self, bindings: HashMap<Hotkey, Action>) -> Vec<Action> {
        let Some(hotkeys) = &mut self.listener else {
            return Vec::new();
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
}

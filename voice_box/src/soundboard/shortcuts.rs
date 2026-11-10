//! The soundboard's shortcuts, registered as global hotkeys and dispatched as actions.

use std::collections::{HashMap, HashSet};

use windows::Win32::Foundation::HWND;

use crate::{
    hotkeys::Hotkeys,
    i18n::{Details, Message, Text},
    model::Hotkey,
    soundboard::bindings::ShortcutAction,
    task::Wake,
};

pub struct Shortcuts {
    hotkeys: Option<Hotkeys>,
    /// Last key set sent for registration, so a failure is not retried every frame.
    attempted: HashSet<Hotkey>,
    error: Message,
}

impl Shortcuts {
    pub fn new(window: HWND, wake: Wake) -> Self {
        let (hotkeys, error) = match Hotkeys::new(window, wake) {
            Ok(hotkeys) => (Some(hotkeys), Message::default()),
            Err(error) => (
                None,
                Message::EnableHotkeysFailed {
                    error: Details::new(error),
                },
            ),
        };
        Self {
            hotkeys,
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
        self.hotkeys = None;
    }
    /// Checks that Windows will accept `key` as a global hotkey.
    pub fn validate(&self, key: Hotkey) -> anyhow::Result<()> {
        self.hotkeys
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(Text::HotkeyServiceUnavailable))?
            .validate(key)
    }

    /// Registers exactly `shortcuts` and returns the actions pressed since the last poll.
    pub fn poll(&mut self, shortcuts: HashMap<Hotkey, ShortcutAction>) -> Vec<ShortcutAction> {
        let Some(hotkeys) = &mut self.hotkeys else {
            return Vec::new();
        };
        let actions: Vec<_> = hotkeys
            .triggered
            .try_iter()
            .filter_map(|key| shortcuts.get(&key).copied())
            .collect();
        let keys: HashSet<_> = shortcuts.into_keys().collect();
        if keys != self.attempted {
            match hotkeys.configure(keys.clone()) {
                Ok(()) => self.error.clear(),
                Err(error) => self.error = Message::error(error),
            }
            self.attempted = keys;
        }
        // Stop takes precedence over playback queued in the same frame.
        if actions
            .iter()
            .any(|action| matches!(action, ShortcutAction::Stop))
        {
            vec![ShortcutAction::Stop]
        } else {
            actions
        }
    }
}

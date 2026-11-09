//! Global shortcuts arrive on the main window; simulated keys use RAII.

use std::{
    collections::{HashMap, HashSet},
    mem::size_of,
    sync::mpsc,
};

use serde::{Deserialize, Serialize};
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::{
        Input::KeyboardAndMouse::*,
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{WM_HOTKEY, WM_NCDESTROY},
    },
};

use crate::i18n::{Message, Text, UserContext, current_language};

const SIMULATED_INPUT_TAG: usize = 0x5642_4b59;

/// Modifier flags in display and press order, with the key each one stands for.
const MODIFIERS: [(HOT_KEY_MODIFIERS, VIRTUAL_KEY, &str); 4] = [
    (MOD_CONTROL, VK_CONTROL, "Ctrl"),
    (MOD_SHIFT, VK_SHIFT, "Shift"),
    (MOD_ALT, VK_MENU, "Alt"),
    (MOD_WIN, VK_LWIN, "Win"),
];
const ALL_MODIFIERS: u32 = MOD_CONTROL.0 | MOD_SHIFT.0 | MOD_ALT.0 | MOD_WIN.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct Hotkey {
    pub virtual_key_code: u16,
    pub modifiers: u32,
}

impl Hotkey {
    pub fn is_valid(self) -> bool {
        self.is_valid_for_hold() && !is_modifier(self.virtual_key_code)
    }
    pub fn is_valid_for_hold(self) -> bool {
        key_name(self.virtual_key_code).is_some() && self.modifiers & !ALL_MODIFIERS == 0
    }
    pub fn is_escape(self) -> bool {
        self.virtual_key_code == VK_ESCAPE.0
    }
    pub fn label(self) -> String {
        self.label_in_language(current_language())
    }
    pub fn label_in_language(self, language: crate::i18n::Language) -> String {
        let key = key_name(self.virtual_key_code)
            .unwrap_or_else(|| Text::InvalidKey.in_language(language).into());
        self.modifier_keys()
            .map(|(_, _, name)| name.to_owned())
            .chain([key])
            .collect::<Vec<_>>()
            .join(" + ")
    }
    fn modifier_keys(self) -> impl Iterator<Item = (HOT_KEY_MODIFIERS, VIRTUAL_KEY, &'static str)> {
        MODIFIERS
            .into_iter()
            .filter(move |(flag, ..)| self.modifiers & flag.0 != 0)
    }
    pub fn pressed() -> Option<Self> {
        let modifiers = MODIFIERS
            .into_iter()
            .filter(|&(_, key, _)| is_key_down(key.0) || (key == VK_LWIN && is_key_down(VK_RWIN.0)))
            .fold(0, |flags, (flag, ..)| flags | flag.0);
        (8..=0xfe)
            .find(|&key| !is_modifier(key) && key_name(key).is_some() && is_key_down(key))
            .map(|virtual_key_code| Self {
                virtual_key_code,
                modifiers,
            })
    }
    pub fn pressed_modifier() -> Option<Self> {
        [
            VK_LSHIFT,
            VK_RSHIFT,
            VK_LCONTROL,
            VK_RCONTROL,
            VK_LMENU,
            VK_RMENU,
            VK_LWIN,
            VK_RWIN,
        ]
        .into_iter()
        .find(|key| is_key_down(key.0))
        .map(|key| Self {
            virtual_key_code: key.0,
            modifiers: 0,
        })
    }
    /// Registration id; virtual-key codes fit in the low byte.
    fn id(self) -> i32 {
        i32::from(self.virtual_key_code) | ((self.modifiers as i32) << 8)
    }
    fn from_id(id: i32) -> Self {
        Self {
            virtual_key_code: (id & 0xff) as u16,
            modifiers: (id >> 8) as u32,
        }
    }
}

fn is_modifier(key: u16) -> bool {
    matches!(
        VIRTUAL_KEY(key),
        VK_SHIFT
            | VK_CONTROL
            | VK_MENU
            | VK_LWIN
            | VK_RWIN
            | VK_LSHIFT
            | VK_RSHIFT
            | VK_LCONTROL
            | VK_RCONTROL
            | VK_LMENU
            | VK_RMENU
    )
}

fn key_name(key: u16) -> Option<String> {
    Some(match key {
        0x30..=0x39 | 0x41..=0x5a => char::from_u32(u32::from(key))?.to_string(),
        0x70..=0x87 => format!("F{}", key - 0x6f),
        0x60..=0x69 => format!("Num {}", key - 0x60),
        0x08 => "Backspace".into(),
        0x09 => "Tab".into(),
        0x0d => "Enter".into(),
        0x1b => "Esc".into(),
        0x20 => "Space".into(),
        0x10 => "Shift".into(),
        0x11 => "Ctrl".into(),
        0x12 => "Alt".into(),
        0x5b => "Left Win".into(),
        0x5c => "Right Win".into(),
        0xa0 => "Left Shift".into(),
        0xa1 => "Right Shift".into(),
        0xa2 => "Left Ctrl".into(),
        0xa3 => "Right Ctrl".into(),
        0xa4 => "Left Alt".into(),
        0xa5 => "Right Alt".into(),
        0x21 => "PageUp".into(),
        0x22 => "PageDown".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0x25 => "Left".into(),
        0x26 => "Up".into(),
        0x27 => "Right".into(),
        0x28 => "Down".into(),
        0x2d => "Insert".into(),
        0x2e => "Delete".into(),
        0x6a => "Num *".into(),
        0x6b => "Num +".into(),
        0x6d => "Num -".into(),
        0x6e => "Num .".into(),
        0x6f => "Num /".into(),
        0xba => ";".into(),
        0xbb => "=".into(),
        0xbc => ",".into(),
        0xbd => "-".into(),
        0xbe => ".".into(),
        0xbf => "/".into(),
        0xc0 => "`".into(),
        0xdb => "[".into(),
        0xdc => "\\".into(),
        0xdd => "]".into(),
        0xde => "'".into(),
        _ => return None,
    })
}

fn is_key_down(key: u16) -> bool {
    unsafe { GetAsyncKeyState(i32::from(key)) < 0 }
}

struct Registration {
    window: HWND,
    key: Hotkey,
}
impl Registration {
    fn new(window: HWND, key: Hotkey) -> anyhow::Result<Self> {
        anyhow::ensure!(key.is_valid(), Message::InvalidHotkey { key });
        unsafe {
            RegisterHotKey(
                Some(window),
                key.id(),
                HOT_KEY_MODIFIERS(key.modifiers) | MOD_NOREPEAT,
                u32::from(key.virtual_key_code),
            )
        }
        .with_user_context(|| Message::RegisterHotkey { key })?;
        Ok(Self { window, key })
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        if let Err(error) = unsafe { UnregisterHotKey(Some(self.window), self.key.id()) } {
            tracing::warn!("Failed to unregister {}: {error}", self.key.label());
        }
    }
}

/// Global shortcuts delivered as `WM_HOTKEY` to a window owned by the calling thread.
///
/// Window messages, unlike thread messages, are still dispatched while that thread
/// runs a modal loop such as a popup menu or a window drag.
pub struct Hotkeys {
    window: HWND,
    registrations: HashMap<Hotkey, Registration>,
    pub pressed: mpsc::Receiver<Hotkey>,
    // Boxed so its address stays fixed while the window subclass points at it.
    _subclass: Box<Subclass>,
}
struct Subclass {
    pressed: mpsc::Sender<Hotkey>,
    wake: Box<dyn Fn()>,
}
impl Hotkeys {
    /// Must be called on the thread that created `window`.
    pub fn new(window: HWND, wake: impl Fn() + 'static) -> anyhow::Result<Self> {
        let (sender, pressed) = mpsc::channel();
        let subclass = Box::new(Subclass {
            pressed: sender,
            wake: Box::new(wake),
        });
        unsafe {
            SetWindowSubclass(
                window,
                Some(window_proc),
                0,
                (&*subclass as *const Subclass) as usize,
            )
        }
        .ok()?;
        Ok(Self {
            window,
            registrations: HashMap::new(),
            pressed,
            _subclass: subclass,
        })
    }

    pub fn configure(&mut self, keys: HashSet<Hotkey>) -> anyhow::Result<()> {
        // Keep existing registrations until every new shortcut succeeds.
        let added = keys
            .iter()
            .filter(|key| !self.registrations.contains_key(key))
            .map(|&key| Ok((key, Registration::new(self.window, key)?)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        self.registrations.retain(|key, _| keys.contains(key));
        self.registrations.extend(added);
        Ok(())
    }

    pub fn validate(&self, key: Hotkey) -> anyhow::Result<()> {
        if !self.registrations.contains_key(&key) {
            drop(Registration::new(self.window, key)?);
        }
        Ok(())
    }
}
impl Drop for Hotkeys {
    fn drop(&mut self) {
        self.registrations.clear();
        let _ = unsafe { RemoveWindowSubclass(self.window, Some(window_proc), 0) };
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass_id: usize,
    data: usize,
) -> LRESULT {
    if message == WM_HOTKEY {
        // The boxed data stays alive until `Hotkeys` removes this subclass.
        let subclass = unsafe { &*(data as *const Subclass) };
        let _ = subclass.pressed.send(Hotkey::from_id(wparam.0 as i32));
        (subclass.wake)();
        return LRESULT(0);
    }
    unsafe {
        if message == WM_NCDESTROY {
            let _ = RemoveWindowSubclass(window, Some(window_proc), subclass_id);
        }
        DefSubclassProc(window, message, wparam, lparam)
    }
}

#[derive(Default)]
pub struct HeldKeys(Vec<u16>);
impl HeldKeys {
    pub fn press(key: Hotkey) -> anyhow::Result<Self> {
        anyhow::ensure!(key.is_valid_for_hold(), Text::InvalidHoldKey);
        let mut held = Self::default();
        let keys = key
            .modifier_keys()
            .map(|(_, vk, _)| vk.0)
            .chain([key.virtual_key_code]);
        for vk in keys {
            // Never release a key the user was already holding themselves.
            if !is_key_down(vk) {
                anyhow::ensure!(send_key(vk, false), Message::HoldHotkey { key });
                held.0.push(vk);
            }
        }
        Ok(held)
    }
}
fn send_key(key: u16, release: bool) -> bool {
    let scan = unsafe { MapVirtualKeyW(u32::from(key), MAPVK_VK_TO_VSC_EX) };
    let mut flags = if release {
        KEYEVENTF_KEYUP
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    if scan != 0 {
        flags |= KEYEVENTF_SCANCODE;
    }
    if scan & 0xff00 == 0xe000 {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: if scan == 0 {
                    VIRTUAL_KEY(key)
                } else {
                    VIRTUAL_KEY(0)
                },
                wScan: (scan & 0xff) as u16,
                dwFlags: flags,
                dwExtraInfo: SIMULATED_INPUT_TAG,
                ..Default::default()
            },
        },
    };
    unsafe { SendInput(&[input], size_of::<INPUT>() as i32) == 1 }
}
impl Drop for HeldKeys {
    fn drop(&mut self) {
        for key in self.0.drain(..).rev() {
            send_key(key, true);
        }
    }
}

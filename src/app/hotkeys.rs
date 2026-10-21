//! Registered global shortcuts run on a message thread; simulated keys use RAII.
use anyhow::Context;
use std::{mem::size_of, sync::mpsc, thread::JoinHandle};
use windows::Win32::{
    Foundation::{LPARAM, WPARAM},
    System::Threading::GetCurrentThreadId,
    UI::{
        Input::KeyboardAndMouse::*,
        WindowsAndMessaging::{
            GetMessageW, MSG, PM_NOREMOVE, PM_REMOVE, PeekMessageW, PostThreadMessageW, WM_APP,
            WM_HOTKEY, WM_QUIT,
        },
    },
};

const SIMULATED_INPUT_TAG: usize = 0x5642_4b59;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Hotkey {
    pub key: u16,
    pub modifiers: u32,
}

impl Hotkey {
    pub fn valid(self) -> bool {
        self.valid_for_hold() && !is_modifier(self.key)
    }
    pub fn valid_for_hold(self) -> bool {
        key_name(self.key).is_some() && self.modifiers & !0xf == 0
    }
    pub fn label(self) -> String {
        let mut parts = Vec::new();
        for (flag, name) in [(2, "Ctrl"), (4, "Shift"), (1, "Alt"), (8, "Win")] {
            if self.modifiers & flag != 0 {
                parts.push(name.to_owned());
            }
        }
        parts.push(key_name(self.key).unwrap_or_else(|| "无效按键".into()));
        parts.join(" + ")
    }
    pub fn pressed() -> Option<Self> {
        let modifiers = [(0x11, 2), (0x10, 4), (0x12, 1), (0x5b, 8), (0x5c, 8)]
            .into_iter()
            .filter(|&(key, _)| down(key))
            .fold(0, |flags, (_, flag)| flags | flag);
        (8..=0xfe)
            .find(|&key| !is_modifier(key) && key_name(key).is_some() && down(key))
            .map(|key| Self { key, modifiers })
    }
    pub fn pressed_modifier() -> Option<Self> {
        [0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0x5b, 0x5c]
            .into_iter()
            .find(|&key| down(key))
            .map(|key| Self { key, modifiers: 0 })
    }
}

fn is_modifier(key: u16) -> bool {
    matches!(key, 0x10..=0x12 | 0x5b..=0x5c | 0xa0..=0xa5)
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

fn down(key: u16) -> bool {
    unsafe { GetAsyncKeyState(i32::from(key)) < 0 }
}

const WM_COMMAND: u32 = WM_APP + 1;

struct Registration(Hotkey);
impl Registration {
    fn id(key: Hotkey) -> i32 {
        i32::from(key.key) | ((key.modifiers as i32) << 8)
    }

    fn new(key: Hotkey) -> anyhow::Result<Self> {
        anyhow::ensure!(key.valid(), "无效快捷键：{}", key.label());
        unsafe {
            RegisterHotKey(
                None,
                Self::id(key),
                HOT_KEY_MODIFIERS(key.modifiers) | MOD_NOREPEAT,
                u32::from(key.key),
            )
        }
        .with_context(|| format!("无法注册 {}，可能已被其他程序占用或由系统保留", key.label()))?;
        Ok(Self(key))
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        if let Err(error) = unsafe { UnregisterHotKey(None, Self::id(self.0)) } {
            tracing::warn!("Failed to unregister {}: {error}", self.0.label());
        }
    }
}

#[derive(Default)]
struct RegisteredHotkeys {
    registrations: Vec<Registration>,
    bindings: Vec<Option<Hotkey>>,
    revision: u64,
}
impl RegisteredHotkeys {
    fn validate(&self, key: Hotkey) -> anyhow::Result<()> {
        if !self
            .registrations
            .iter()
            .any(|registered| registered.0 == key)
        {
            drop(Registration::new(key)?);
        }
        Ok(())
    }

    fn configure(&mut self, revision: u64, bindings: Vec<Option<Hotkey>>) -> anyhow::Result<()> {
        let mut added = Vec::new();
        for key in bindings.iter().flatten().copied() {
            if !self
                .registrations
                .iter()
                .chain(&added)
                .any(|registered| registered.0 == key)
            {
                // Keep existing registrations until every new shortcut succeeds.
                added.push(Registration::new(key)?);
            }
        }
        self.registrations.extend(added);
        self.registrations
            .retain(|registered| bindings.contains(&Some(registered.0)));
        self.bindings = bindings;
        self.revision = revision;
        // Discard notifications queued for the previous action mapping.
        let mut msg = MSG::default();
        while unsafe { PeekMessageW(&mut msg, None, WM_HOTKEY, WM_HOTKEY, PM_REMOVE).as_bool() } {}
        Ok(())
    }
}

enum Command {
    Configure(u64, Vec<Option<Hotkey>>, mpsc::Sender<anyhow::Result<()>>),
    Validate(Hotkey, mpsc::Sender<anyhow::Result<()>>),
}

pub(super) enum Event {
    Trigger { index: usize, revision: u64 },
    Error(String),
}
pub(super) struct Hotkeys {
    sender: mpsc::Sender<Command>,
    pub receiver: mpsc::Receiver<Event>,
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}
impl Hotkeys {
    pub fn new(wake: impl Fn() + Send + 'static) -> anyhow::Result<Self> {
        let (sender, commands) = mpsc::channel();
        let (events, receiver) = mpsc::channel();
        let (ready, started) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("VoiceBoxHotkeys".into())
            .spawn(move || {
                let mut msg = MSG::default();
                // PostThreadMessage requires the destination queue to exist first.
                let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
                if ready.send(unsafe { GetCurrentThreadId() }).is_err() {
                    return;
                }
                let mut registered = RegisteredHotkeys::default();
                loop {
                    let result = unsafe { GetMessageW(&mut msg, None, 0, 0) }.0;
                    if result == -1 {
                        let error = windows::core::Error::from_thread();
                        let _ = events.send(Event::Error(format!("等待全局快捷键失败：{error}")));
                        wake();
                        break;
                    }
                    if result == 0 {
                        break;
                    }
                    if msg.message == WM_COMMAND {
                        for command in commands.try_iter() {
                            let (reply, result) = match command {
                                Command::Configure(revision, bindings, reply) => {
                                    (reply, registered.configure(revision, bindings))
                                }
                                Command::Validate(key, reply) => (reply, registered.validate(key)),
                            };
                            let _ = reply.send(result);
                        }
                    } else if msg.message == WM_HOTKEY
                        && let Some(index) = registered.bindings.iter().position(|binding| {
                            binding.is_some_and(|key| Registration::id(key) == msg.wParam.0 as i32)
                        })
                    {
                        let _ = events.send(Event::Trigger {
                            index,
                            revision: registered.revision,
                        });
                        wake();
                    }
                }
                // Registrations are released here, on the thread that owns them.
            })?;
        let thread_id = started.recv().context("快捷键线程启动失败")?;
        Ok(Self {
            sender,
            receiver,
            thread_id,
            thread: Some(thread),
        })
    }

    fn request(
        &self,
        command: impl FnOnce(mpsc::Sender<anyhow::Result<()>>) -> Command,
    ) -> anyhow::Result<()> {
        let (reply, result) = mpsc::channel();
        self.sender
            .send(command(reply))
            .context("快捷键线程已退出")?;
        unsafe { PostThreadMessageW(self.thread_id, WM_COMMAND, WPARAM(0), LPARAM(0)) }
            .context("无法通知快捷键线程")?;
        result.recv().context("快捷键线程未返回结果")?
    }

    pub fn configure(&self, revision: u64, bindings: Vec<Option<Hotkey>>) -> anyhow::Result<()> {
        self.request(|reply| Command::Configure(revision, bindings, reply))
    }

    pub fn validate(&self, key: Hotkey) -> anyhow::Result<()> {
        self.request(|reply| Command::Validate(key, reply))
    }
}
impl Drop for Hotkeys {
    fn drop(&mut self) {
        if let Err(error) =
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }
        {
            tracing::warn!("Failed to stop hotkey thread: {error}");
            return;
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
pub(super) struct HeldKeys(Vec<u16>);
impl HeldKeys {
    pub fn press(key: Hotkey) -> anyhow::Result<Self> {
        anyhow::ensure!(key.valid_for_hold(), "自动按键无效");
        let mut held = Self::default();
        let mut keys = Vec::new();
        for (flag, vk) in [(2, 0x11), (4, 0x10), (1, 0x12), (8, 0x5b)] {
            if key.modifiers & flag != 0 {
                keys.push(vk);
            }
        }
        keys.push(key.key);
        for vk in keys {
            // Never release a key the user was already holding themselves.
            if !down(vk) {
                anyhow::ensure!(
                    send_key(vk, false),
                    "无法自动按住 {}，请检查游戏与 VoiceBox 的权限",
                    key.label()
                );
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

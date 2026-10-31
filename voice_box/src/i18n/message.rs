use std::{borrow::Cow, error::Error, fmt, sync::Arc};

use crate::{
    hotkeys::Hotkey,
    i18n::{Language, Text, current_language},
};

#[derive(Clone, Debug)]
pub struct Details(Arc<anyhow::Error>);

impl Details {
    pub fn new(error: impl Into<anyhow::Error>) -> Self {
        Self(Arc::new(error.into()))
    }

    fn render(&self, language: Language) -> String {
        self.0
            .chain()
            .map(|error| {
                if let Some(error) = error.downcast_ref::<ContextError>() {
                    return error.message.render(language).into_owned();
                }
                if let Some(message) = error.downcast_ref::<Message>() {
                    return message.render(language).into_owned();
                }
                if let Some(text) = error.downcast_ref::<Text>() {
                    return text.in_language(language).to_owned();
                }
                if let Some(error) = error.downcast_ref::<voice_box_shared::error::SharedError>() {
                    return super::shared_error::render(*error, language).to_owned();
                }
                if let Some(error) = error
                    .downcast_ref::<std::io::Error>()
                    .and_then(std::io::Error::get_ref)
                    .and_then(|error| error.downcast_ref::<voice_box_shared::error::SharedError>())
                {
                    return super::shared_error::render(*error, language).to_owned();
                }
                error.to_string()
            })
            .collect::<Vec<_>>()
            .join(": ")
    }
}

impl fmt::Display for Details {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render(Language::English))
    }
}

macro_rules! messages {
    ($($key:ident { $($field:ident: $ty:ty),* } => ($en:literal, $zh:literal);)*) => {
        #[derive(Clone, Debug, Default)]
        pub enum Message {
            #[default]
            Empty,
            Text(Text),
            Error(Details),
            Many(Vec<Message>),
            $($key { $($field: $ty),* },)*
        }

        impl Message {
            pub fn render(&self, language: Language) -> Cow<'static, str> {
                match self {
                    Self::Empty => Cow::Borrowed(""),
                    Self::Text(text) => Cow::Borrowed(text.in_language(language)),
                    Self::Error(error) => Cow::Owned(error.render(language)),
                    Self::Many(messages) => Cow::Owned(messages.iter()
                        .map(|m| m.render(language)).collect::<Vec<_>>().join("\n\n")),
                    $(Self::$key { $($field),* } => {
                        $(let $field = RenderArgument::render_argument($field, language);)*
                        Cow::Owned(match language {
                            Language::English => format!($en),
                            Language::SimplifiedChinese => format!($zh),
                        })
                    },)*
                }
            }
        }
    }
}

// Nested errors and key labels are rendered in the requested language.
trait RenderArgument {
    type Output: fmt::Display;
    fn render_argument(&self, language: Language) -> Self::Output;
}
impl RenderArgument for Details {
    type Output = String;
    fn render_argument(&self, language: Language) -> String {
        self.render(language)
    }
}
impl RenderArgument for Hotkey {
    type Output = String;
    fn render_argument(&self, language: Language) -> String {
        self.label_in_language(language)
    }
}
macro_rules! plain_argument {
    ($($ty:ty),*) => {$ (
        impl RenderArgument for $ty {
            type Output = Self;
            fn render_argument(&self, _: Language) -> Self { self.clone() }
        }
    )*};
}
plain_argument!(String, f64, usize, u32);

messages! {
    ExportSoundFailed { name: String } => ("Failed to export {name}", "导出 {name} 失败");
    WritePathFailed { path: String } => ("Failed to write {path}", "写入 {path} 失败");
    StopAudioServiceFailed { result: String } => ("Audio service shutdown failed: {result}", "停止音频服务失败：{result}");
    InvalidRegistryPath { path: String } => ("Invalid registry path: {path}", "注册表路径无效：{path}");
    Recorded { duration_secs: f64 } => ("Recorded {duration_secs:.1} s.", "已录音 {duration_secs:.1} 秒。");
    RecordingProgress { elapsed_secs: f64 } => ("Recording: {elapsed_secs:.1} s", "正在录音：{elapsed_secs:.1} 秒");
    PlaybackProgress { elapsed_secs: f64, duration_secs: f64 } => ("Playing {elapsed_secs:.1} / {duration_secs:.1} s", "正在播放 {elapsed_secs:.1} / {duration_secs:.1} 秒");
    SoundCount { count: usize } => ("Sounds: {count}", "音效库 · {count} 个");
    SelectedSounds { count: usize } => ("{count} selected", "已选 {count} 项");
    RenameSummary { count: usize, changed: usize, errors: usize } => ("{count} sounds · {changed} changes · {errors} errors", "共 {count} 项 · 修改 {changed} 项 · 错误 {errors} 项");
    SelectedSoundsWithHidden { count: usize, hidden: usize } => ("{count} selected · {hidden} hidden", "已选 {count} 项 · 当前结果外 {hidden} 项");
    DeleteSelectedSounds { count: usize } => ("Delete ({count})", "删除（{count}）");
    ConfirmDeleteSounds { count: usize } => ("Delete {count} sounds from the library? This cannot be undone.", "从音效库删除这 {count} 个音效？此操作无法撤销。");
    CaptureProgress { time: String } => ("Recording · {time}", "正在录制 · {time}");
    RecordingLimit { minutes: u32 } => ("Default speaker · Up to {minutes} min", "默认扬声器 · 最长 {minutes} 分钟");
    DefaultDevice { name: String } => ("{name} (default)", "{name}（默认）");
    UnavailableOutputDevice { name: String } => ("{name} (device unavailable)", "{name}（设备不可用）");
    ConfirmPluginUninstall { name: String } => ("Uninstall the VoiceBox plugin for this microphone?\n{name}", "确认卸载此麦克风的VoiceBox插件？\n{name}");
    PlayingSound { name: String } => ("Playing · {name}", "播放中 · {name}");
    RenameSoundHint { name: String } => ("{name}\nDouble-click to rename", "{name}\n双击重命名");
    StopPlaybackHotkey { key: Hotkey } => ("Stop playback · {key}", "停止播放 · {key}");
    CannotStartPreview { error: Details } => ("Cannot start preview: {error}", "无法开始试听操作：{error}");
    PreviewFailed { error: Details } => ("Preview failed: {error}", "试听失败：{error}");
    CannotStartOperation { error: Details } => ("Cannot start operation: {error}", "无法开始操作：{error}");
    OperationFailed { error: Details } => ("Operation failed: {error}", "操作失败：{error}");
    UninstallFailed { error: Details } => ("Uninstallation incomplete: {error}", "卸载未完成：{error}");
    OpenWindowFailed { error: Details } => ("Cannot open the VoiceBox window: {error}", "无法打开 VoiceBox 窗口：{error}");
    ShowOverlayFailed { error: Details } => ("Failed to show the playback overlay: {error}", "显示播放浮层失败：{error}");
    LoggingFailed { error: Details } => ("Failed to initialize VoiceBox logging: {error}", "初始化 VoiceBox 日志失败：{error}");
    OpenPickerFailed { error: Details } => ("Failed to open the file picker: {error}", "打开文件选择器失败：{error}");
    DeleteSoundFailed { error: Details } => ("Failed to delete the sound: {error}", "删除音效失败：{error}");
    StartPlaybackFailed { error: Details } => ("Cannot start playback: {error}", "无法开始播放：{error}");
    ImportSoundsFailed { error: Details } => ("Failed to import sounds: {error}", "导入音效失败：{error}");
    PlaybackFailed { error: Details } => ("Playback failed: {error}", "播放失败：{error}");
    EnableHotkeysFailed { error: Details } => ("Cannot enable global hotkeys: {error}", "无法启用全局快捷键：{error}");
    WaitHotkeysFailed { error: Details } => ("Failed to wait for global hotkeys: {error}", "等待全局快捷键失败：{error}");
    SpeakerMonitorFailed { error: Details } => ("Speaker playback failed; microphone output continues: {error}", "扬声器播放失败，麦克风发送继续：{error}");
    MicrophoneOutputFailed { error: Details } => ("Microphone output unavailable: {error}", "麦克风输出不可用：{error}");
    WaitAudioDeviceFailed { result: String } => ("Failed to wait for the audio device: {result}", "等待音频设备失败：{result}");
    ReadPathFailed { path: String } => ("Failed to read {path}", "读取 {path} 失败");
    RecycleCancelled { path: String } => ("Recycling {path} was cancelled or did not complete", "回收 {path} 的操作已取消或未完成");
    RecycleIncomplete { path: String } => ("{path} still exists; recycling did not complete", "{path} 仍然存在，回收未完成");
    OpenFxProperties { device: String } => ("Failed to open FxProperties for microphone {device}", "打开麦克风 {device} 的 FxProperties 失败");
    OpenEndpointProperties { device: String } => ("Failed to open Properties for microphone {device}", "打开麦克风 {device} 的 Properties 失败");
    UninstallMicrophone { device: String } => ("Failed to uninstall the VoiceBox APO for microphone {device}", "卸载麦克风 {device} 的 VoiceBox APO 失败");
    DeleteApoRegistration { path: String } => ("Failed to delete the VoiceBox APO registration: {path}", "删除 VoiceBox APO 注册失败：{path}");
    InvalidHotkey { key: Hotkey } => ("Invalid hotkey: {key}", "无效快捷键：{key}");
    RegisterHotkey { key: Hotkey } => ("Cannot register {key}. It may be in use by another application or reserved by Windows", "无法注册 {key}，可能已被其他程序占用或由系统保留");
    HoldHotkey { key: Hotkey } => ("Cannot hold {key} automatically. Check the game and VoiceBox permissions", "无法自动按住 {key}，请检查游戏与 VoiceBox 的权限");
    HotkeyConflict { key: Hotkey } => ("{key} is already assigned to another action. Choose a different shortcut", "{key} 已绑定其他操作，请使用另一个快捷键");
    ImportFileFailed { path: String, error: Details } => ("{path}: {error}", "{path}：{error}");
}

impl Message {
    pub fn text(&self) -> Cow<'static, str> {
        self.render(current_language())
    }
    pub fn clear(&mut self) {
        *self = Self::Empty;
    }
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }
    pub fn error(error: impl Into<anyhow::Error>) -> Self {
        Self::Error(Details::new(error))
    }
}
impl From<Text> for Message {
    fn from(text: Text) -> Self {
        Self::Text(text)
    }
}
impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render(Language::English))
    }
}
impl Error for Message {}

#[derive(Debug)]
struct ContextError {
    message: Message,
    source: anyhow::Error,
}
impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}
impl Error for ContextError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Preserve translatable context as a concrete error node instead of formatting it early.
pub trait UserContext: Sized {
    type Output;
    fn user_context(self, message: impl Into<Message>) -> Self::Output;
    fn with_user_context(self, message: impl FnOnce() -> Message) -> Self::Output;
}
impl<T, E: Into<anyhow::Error>> UserContext for Result<T, E> {
    type Output = anyhow::Result<T>;
    fn user_context(self, message: impl Into<Message>) -> Self::Output {
        self.map_err(|source| {
            anyhow::Error::new(ContextError {
                message: message.into(),
                source: source.into(),
            })
        })
    }
    fn with_user_context(self, message: impl FnOnce() -> Message) -> Self::Output {
        self.map_err(|source| {
            anyhow::Error::new(ContextError {
                message: message(),
                source: source.into(),
            })
        })
    }
}
impl<T> UserContext for Option<T> {
    type Output = anyhow::Result<T>;
    fn user_context(self, message: impl Into<Message>) -> Self::Output {
        self.ok_or_else(|| anyhow::Error::new(message.into()))
    }
    fn with_user_context(self, message: impl FnOnce() -> Message) -> Self::Output {
        self.ok_or_else(|| anyhow::Error::new(message()))
    }
}
impl UserContext for anyhow::Error {
    type Output = anyhow::Error;
    fn user_context(self, message: impl Into<Message>) -> Self::Output {
        anyhow::Error::new(ContextError {
            message: message.into(),
            source: self,
        })
    }
    fn with_user_context(self, message: impl FnOnce() -> Message) -> Self::Output {
        self.user_context(message())
    }
}

use super::library::Library;
use crate::{
    app::hotkeys::Hotkey,
    shared::{paths, soundboard::MAX_VOLUME},
};
use anyhow::Context;
use std::fs;

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(in crate::app) struct Options {
    pub monitor_volume: f32,
    pub send_volume: f32,
    pub microphone: bool,
    pub hold: Option<Hotkey>,
    pub stop_hotkey: Option<Hotkey>,
    pub random_hotkey: Option<Hotkey>,
    pub repeat: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            monitor_volume: 1.0,
            send_volume: 1.0,
            microphone: false,
            hold: None,
            stop_hotkey: None,
            random_hotkey: None,
            repeat: false,
        }
    }
}
impl Options {
    pub(super) fn sanitize(&mut self) {
        for volume in [&mut self.monitor_volume, &mut self.send_volume] {
            *volume = if volume.is_finite() {
                volume.clamp(0.0, MAX_VOLUME)
            } else {
                1.0
            };
        }
        self.hold = self.hold.filter(|key| key.valid_for_hold());
        self.stop_hotkey = self.stop_hotkey.filter(|key| key.valid());
        self.random_hotkey = self.random_hotkey.filter(|key| key.valid());
    }
    pub(super) fn for_output(&self, microphone_available: bool) -> Self {
        let mut options = self.clone();
        if !microphone_available {
            options.send_volume = 0.0;
            options.microphone = true;
        }
        options
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(in crate::app) struct Config {
    pub options: Options,
    pub library: Library,
}

impl Config {
    pub fn try_load() -> anyhow::Result<Self> {
        let path = paths::data_dir()?.join(paths::SOUNDBOARD_SETTINGS_FILE);
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).context("读取音效配置失败"),
        };
        serde_json::from_reader(std::io::BufReader::new(file)).context("音效配置格式无效")
    }
}

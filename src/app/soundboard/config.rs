use super::library::Library;
use crate::{
    app::{hotkeys::Hotkey, settings_writer},
    shared::{paths, pcm_transport::MAX_VOLUME},
};
use anyhow::{Context, ensure};
use std::fs;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum BindingTarget {
    Sound(usize),
    Hold,
    Stop,
    Random,
}

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
    pub(super) fn validate_binding(
        &self,
        library: &Library,
        target: BindingTarget,
        key: Hotkey,
    ) -> anyhow::Result<()> {
        ensure!(
            if target == BindingTarget::Hold {
                key.valid_for_hold()
            } else {
                key.valid()
            },
            "无效快捷键"
        );
        let duplicate = (target != BindingTarget::Stop && self.stop_hotkey == Some(key))
            || (target != BindingTarget::Random && self.random_hotkey == Some(key))
            || library.sounds.iter().enumerate().any(|(index, sound)| {
                target != BindingTarget::Sound(index) && sound.hotkey == Some(key)
            });
        ensure!(
            !duplicate || target == BindingTarget::Hold,
            "{} 已绑定其他操作，请使用另一个快捷键",
            key.label()
        );
        let overlaps_hold = match target {
            BindingTarget::Sound(_) | BindingTarget::Stop | BindingTarget::Random => {
                self.hold.is_some_and(|hold| hold.key == key.key)
            }
            BindingTarget::Hold => {
                self.stop_hotkey
                    .is_some_and(|shortcut| shortcut.key == key.key)
                    || self
                        .random_hotkey
                        .is_some_and(|shortcut| shortcut.key == key.key)
                    || library
                        .sounds
                        .iter()
                        .any(|sound| sound.hotkey.is_some_and(|shortcut| shortcut.key == key.key))
            }
        };
        ensure!(
            !overlaps_hold,
            "自动按键不能与播放或停止快捷键使用同一个主键"
        );
        Ok(())
    }

    pub(super) fn validate(&self, library: &Library) -> anyhow::Result<()> {
        ensure!(
            [self.monitor_volume, self.send_volume]
                .iter()
                .all(|volume| { volume.is_finite() && (0.0..=MAX_VOLUME).contains(volume) }),
            "播放音量无效"
        );
        for (target, key) in [
            (BindingTarget::Hold, self.hold),
            (BindingTarget::Stop, self.stop_hotkey),
            (BindingTarget::Random, self.random_hotkey),
        ] {
            if let Some(key) = key {
                self.validate_binding(library, target, key)?;
            }
        }
        Ok(())
    }

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
    pub(super) fn save(&self) -> anyhow::Result<()> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        settings_writer::save_bytes(&paths::data_dir()?, paths::SOUNDBOARD_SETTINGS_FILE, &bytes)
            .context("保存音效配置失败")
    }

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

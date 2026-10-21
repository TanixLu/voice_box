//! Sound library operations and background playback coordination.
use crate::{
    app::hotkeys::Hotkey,
    shared::{paths, pcm_transport::transport_path},
};
use anyhow::{Context, ensure};
use std::{collections::HashSet, path::PathBuf, sync::mpsc, thread::JoinHandle};

pub(super) mod config;
mod decoder;
pub(super) mod library;
mod metadata;
mod playback;
pub(super) mod recording;

use config::{BindingTarget, Config, Options};
use library::{ImportedSounds, Library, import_files, is_valid_file_name};
use metadata::DurationReader;
use playback::{Command, Event};

struct PlaybackJob {
    sender: mpsc::Sender<Command>,
    receiver: mpsc::Receiver<Event>,
    thread: JoinHandle<()>,
    microphone_available: bool,
}

#[derive(Default)]
pub(super) struct Soundboard {
    library: Library,
    options: Options,
    pub active_sound_index: Option<usize>,
    pub elapsed: f64,
    pub duration: Option<f64>,
    pub message: String,
    pub failed: bool,
    playback_job: Option<PlaybackJob>,
    queued_playback: Option<(usize, Option<String>)>,
    import_job: Option<mpsc::Receiver<anyhow::Result<ImportedSounds>>>,
    duration_reader: DurationReader,
}
impl Soundboard {
    pub fn new(config: Config) -> Self {
        let Config {
            mut options,
            mut library,
        } = config;
        options.sanitize();
        let directory = match paths::sound_library_dir() {
            Ok(directory) => directory,
            Err(error) => {
                let mut result = Self::default();
                result.options = options;
                result.set_error(format!("读取音效库目录失败：{error}"));
                return result;
            }
        };
        library
            .sounds
            .retain(|sound| is_valid_file_name(&sound.file_name));
        for sound in &mut library.sounds {
            sound.path = directory.join(&sound.file_name);
            sound.hotkey = sound.hotkey.filter(|key| key.is_valid());
        }
        let mut result = Self::default();
        result.options = options;
        result.library = library;
        result
    }
    pub fn save_options(&mut self, options: Options) -> anyhow::Result<()> {
        options.validate(&self.library)?;
        Config {
            options: options.clone(),
            library: self.library.clone(),
        }
        .save()?;
        self.options = options;
        self.update_playback_options();
        Ok(())
    }
    fn save(&self) -> anyhow::Result<()> {
        Config {
            options: self.options.clone(),
            library: self.library.clone(),
        }
        .save()
    }
    fn save_library(&mut self) {
        if let Err(error) = self.save() {
            tracing::error!("Failed to save soundboard: {error:#}");
            self.set_error(format!("{error:#}"));
        }
    }
    pub fn library(&self) -> &Library {
        &self.library
    }
    pub fn options(&self) -> &Options {
        &self.options
    }
    pub fn bind_hotkey(
        &mut self,
        target: BindingTarget,
        key: Option<Hotkey>,
        draft: Option<&mut Options>,
    ) -> anyhow::Result<()> {
        if let Some(key) = key {
            draft.as_deref().unwrap_or(&self.options).validate_binding(
                &self.library,
                target,
                key,
            )?;
        }
        match target {
            BindingTarget::Sound(index) => {
                let options = &self.options;
                self.library.set_hotkey(index, key, |library| {
                    Config {
                        options: options.clone(),
                        library: library.clone(),
                    }
                    .save()
                })
            }
            BindingTarget::Hold | BindingTarget::Stop | BindingTarget::Random => {
                let options = draft.context("请先打开音效设置")?;
                match target {
                    BindingTarget::Hold => options.auto_hold_hotkey = key,
                    BindingTarget::Stop => options.stop_hotkey = key,
                    BindingTarget::Random => options.random_playback_hotkey = key,
                    BindingTarget::Sound(_) => unreachable!(),
                }
                Ok(())
            }
        }
    }
    pub fn busy(&self) -> bool {
        self.playback_job.is_some() || self.import_job.is_some()
    }
    pub fn playing(&self) -> bool {
        self.playback_job.is_some()
    }
    pub fn importing(&self) -> bool {
        self.import_job.is_some()
    }
    pub fn add_recording(&mut self, sound: library::Sound) -> anyhow::Result<()> {
        let path = sound.path.clone();
        self.library.sounds.push(sound);
        if let Err(error) = self.save() {
            self.library.sounds.pop();
            if let Err(cleanup) = std::fs::remove_file(&path) {
                return Err(error.context(format!(
                    "无法清理录音文件：{cleanup}；文件保留在 {}",
                    path.display()
                )));
            }
            return Err(error);
        }
        self.failed = false;
        self.message.clear();
        Ok(())
    }
    pub fn import(
        &mut self,
        select_files: impl FnOnce() -> anyhow::Result<Vec<PathBuf>> + Send + 'static,
        wake: impl Fn() + Send + 'static,
    ) {
        if self.import_job.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        match std::thread::Builder::new()
            .name("VoiceBoxSoundPicker".into())
            .spawn(move || {
                let _ = tx.send(select_files().and_then(import_files));
                wake();
            }) {
            Ok(_) => self.import_job = Some(rx),
            Err(e) => self.set_error(format!("打开文件选择器失败：{e}")),
        }
    }
    pub fn set_error(&mut self, message: String) {
        self.failed = true;
        self.message = message;
    }
    fn finish_import(&mut self, imported: ImportedSounds) {
        let mut known: HashSet<_> = self
            .library
            .sounds
            .iter()
            .map(|sound| sound.file_name.to_lowercase())
            .collect();
        for sound in imported.sounds {
            if !known.insert(sound.file_name.to_lowercase()) {
                continue;
            }
            self.library.sounds.push(sound);
        }
        if !imported.errors.is_empty() {
            self.set_error(imported.errors.join("\n"));
        } else {
            self.failed = false;
            self.message.clear();
        }
        self.save_library();
    }
    pub fn rename(&mut self, index: usize, name: &str) -> anyhow::Result<()> {
        ensure!(!self.playing(), "请先停止播放再重命名");
        let original_path = self
            .library
            .sounds
            .get(index)
            .context("音效不存在")?
            .path
            .clone();
        let options = &self.options;
        let result = self.library.rename(index, name, |library| {
            Config {
                options: options.clone(),
                library: library.clone(),
            }
            .save()
        });
        self.duration_reader.invalidate(&original_path);
        self.duration_reader
            .invalidate(&self.library.sounds[index].path);
        result
    }
    pub fn remove(&mut self, index: usize) {
        if index >= self.library.sounds.len() {
            return;
        }
        if self.active_sound_index == Some(index) {
            self.stop();
            if let Some(job) = self.playback_job.take() {
                let _ = job.thread.join();
            }
            self.active_sound_index = None;
        }
        let sound = match self.library.remove(index) {
            Ok(sound) => sound,
            Err(error) => {
                self.set_error(format!("删除音效文件失败：{error}"));
                return;
            }
        };
        if let Some(active) = &mut self.active_sound_index
            && *active > index
        {
            *active -= 1;
        }
        self.queued_playback = None;
        self.duration_reader.invalidate(&sound.path);
        self.save_library();
    }
    pub fn play(&mut self, index: usize, guid: Option<&str>, wake: impl Fn() + Send + 'static) {
        let Some(sound) = self.library.sounds.get(index) else {
            return;
        };
        if self.options.speaker_output_volume == 0.0 && self.options.microphone_output_volume == 0.0
        {
            self.set_error("麦克风和扬声器音量均为 0，未开始播放".into());
            return;
        }
        wake();
        if self.playback_job.is_some() {
            self.stop();
            self.queued_playback = Some((index, guid.map(str::to_owned)));
            return;
        }
        let path = sound.path.clone();
        let microphone_available = guid.is_some();
        let options = self.options.for_output(microphone_available);
        if options.speaker_output_volume == 0.0 && options.microphone_output_volume == 0.0 {
            self.set_error("麦克风插件未就绪，且扬声器音量为 0，没有可用的播放输出".into());
            return;
        }
        let transport = match transport_path(guid.unwrap_or("00000000-0000-0000-0000-000000000000"))
        {
            Ok(path) => path,
            Err(e) => {
                self.set_error(e.to_string());
                return;
            }
        };
        let (tx, rx) = mpsc::channel();
        let (events, receiver) = mpsc::channel();
        self.failed = false;
        self.elapsed = 0.0;
        self.duration = None;
        self.message.clear();
        match std::thread::Builder::new()
            .name("VoiceBoxSoundboard".into())
            .spawn(move || {
                let result = playback::run(path, transport, options, &rx, &events);
                if let Err(e) = &result {
                    tracing::error!("Soundboard failed: {e:#}");
                }
                let _ = events.send(Event::Finished(result));
                wake();
            }) {
            Ok(thread) => {
                self.active_sound_index = Some(index);
                self.playback_job = Some(PlaybackJob {
                    sender: tx,
                    receiver,
                    thread,
                    microphone_available,
                });
            }
            Err(e) => self.set_error(format!("无法开始播放：{e}")),
        }
    }
    pub fn stop(&mut self) {
        self.queued_playback = None;
        if let Some(job) = &self.playback_job {
            let _ = job.sender.send(Command::Stop);
        }
    }
    pub fn update_playback_options(&self) {
        if let Some(job) = &self.playback_job {
            let _ = job.sender.send(Command::Configure(
                self.options.for_output(job.microphone_available),
            ));
        }
    }
    pub fn poll(&mut self, wake: impl Fn() + Clone + Send + 'static) {
        for error in self.duration_reader.poll(&mut self.library, wake.clone()) {
            self.set_error(error);
        }
        if let Some(import_job) = &self.import_job {
            match import_job.try_recv() {
                Ok(result) => {
                    self.import_job = None;
                    match result {
                        Ok(imported) => self.finish_import(imported),
                        Err(e) => self.set_error(format!("选择文件失败：{e:#}")),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.import_job = None;
                    self.set_error("文件选择器意外退出".into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        while let Some(job) = &self.playback_job {
            match job.receiver.try_recv() {
                Ok(Event::Started(duration)) => {
                    self.duration = duration;
                    if let Some(sound) = self
                        .active_sound_index
                        .and_then(|index| self.library.sounds.get_mut(index))
                        && duration.is_some()
                    {
                        sound.duration = duration;
                    }
                    self.message.clear();
                }
                Ok(Event::Progress(elapsed)) => self.elapsed = elapsed,
                Ok(Event::MonitorError(error)) => self.set_error(error),
                Ok(Event::Finished(result)) => {
                    if let Some(job) = self.playback_job.take() {
                        let _ = job.thread.join();
                    }
                    self.active_sound_index = None;
                    if let Err(e) = result {
                        self.set_error(format!("播放失败：{e:#}"));
                    }
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if let Some(job) = self.playback_job.take() {
                        let _ = job.thread.join();
                    }
                    self.active_sound_index = None;
                    self.set_error("播放线程意外退出".into());
                    break;
                }
            }
        }
        if self.playback_job.is_none()
            && let Some((index, guid)) = self.queued_playback.take()
        {
            self.play(index, guid.as_deref(), wake);
        }
    }
}
impl Drop for Soundboard {
    fn drop(&mut self) {
        self.stop();
        if let Some(job) = self.playback_job.take() {
            let _ = job.thread.join();
        }
    }
}

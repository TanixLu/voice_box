//! Sound library operations and background playback coordination.
use crate::{
    app::settings_writer,
    shared::{paths, soundboard::transport_path},
};
use anyhow::{Context, ensure};
use std::{collections::HashSet, fs, path::PathBuf, sync::mpsc, thread::JoinHandle};

pub(super) mod config;
pub(super) mod library;
mod metadata;
mod picker;
mod playback;

use config::{Config, Options};
use library::{ImportedSounds, Library, import_files, valid_file_name};
use metadata::MetadataReader;
use picker::choose_files;
use playback::{Command, Event};

struct Job {
    sender: mpsc::Sender<Command>,
    receiver: mpsc::Receiver<Event>,
    thread: JoinHandle<()>,
    microphone_available: bool,
}

#[derive(Default)]
pub(super) struct Soundboard {
    pub library: Library,
    pub options: Options,
    pub active: Option<usize>,
    pub elapsed: f64,
    pub duration: Option<f64>,
    pub message: String,
    pub failed: bool,
    job: Option<Job>,
    pending: Option<(usize, Option<String>)>,
    picker: Option<mpsc::Receiver<anyhow::Result<ImportedSounds>>>,
    metadata: MetadataReader,
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
                result.error(format!("读取音效库目录失败：{error}"));
                return result;
            }
        };
        library
            .sounds
            .retain(|sound| valid_file_name(&sound.file_name));
        for sound in &mut library.sounds {
            sound.path = directory.join(&sound.file_name);
            sound.hotkey = sound.hotkey.filter(|key| key.valid());
        }
        let mut result = Self::default();
        result.options = options;
        result.library = library;
        result
    }
    pub fn save_options(&mut self, options: Options) -> anyhow::Result<()> {
        settings_writer::save_soundboard(&Config {
            options: options.clone(),
            library: self.library.clone(),
        })?;
        self.options = options;
        self.configure();
        Ok(())
    }
    fn save(&self) -> anyhow::Result<()> {
        settings_writer::save_soundboard(&Config {
            options: self.options.clone(),
            library: self.library.clone(),
        })
    }
    pub fn save_library(&mut self) {
        if let Err(error) = self.save() {
            tracing::error!("Failed to save soundboard: {error:#}");
            self.error(format!("{error:#}"));
        }
    }
    pub fn busy(&self) -> bool {
        self.job.is_some() || self.picker.is_some()
    }
    pub fn playing(&self) -> bool {
        self.job.is_some()
    }
    pub fn choosing(&self) -> bool {
        self.picker.is_some()
    }
    pub fn choose(&mut self, ctx: &egui::Context) {
        if self.picker.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        match std::thread::Builder::new()
            .name("VoiceBoxSoundPicker".into())
            .spawn(move || {
                let _ = tx.send(choose_files().and_then(import_files));
                ctx.request_repaint();
            }) {
            Ok(_) => self.picker = Some(rx),
            Err(e) => self.error(format!("打开文件选择器失败：{e}")),
        }
    }
    pub fn error(&mut self, message: String) {
        self.failed = true;
        self.message = message;
    }
    fn import(&mut self, imported: ImportedSounds) {
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
            self.error(imported.errors.join("\n"));
        } else {
            self.failed = false;
            self.message.clear();
        }
        self.save_library();
    }
    pub fn rename(&mut self, index: usize, name: &str) -> anyhow::Result<()> {
        ensure!(!self.playing(), "请先停止播放再重命名");
        let sound = self.library.sounds.get_mut(index).context("音效不存在")?;
        let name = name.trim();
        ensure!(
            valid_file_name(name),
            "名称不能为空，也不能包含路径、控制字符或 Windows 文件名保留字符"
        );
        let mut file_name = PathBuf::from(name);
        if let Some(extension) = sound.path.extension() {
            file_name.as_mut_os_string().push(".");
            file_name.as_mut_os_string().push(extension);
        }
        let file_name = file_name
            .into_os_string()
            .into_string()
            .map_err(|_| anyhow::anyhow!("音效名称无效"))?;
        ensure!(valid_file_name(&file_name), "音效名称无效");
        if file_name == sound.file_name {
            return self.save();
        }
        let destination = paths::sound_library_dir()?.join(&file_name);
        // Do not replace an existing library file when names collide.
        unsafe {
            windows::Win32::Storage::FileSystem::MoveFileExW(
                windows::core::PCWSTR(crate::shared::utils::wide_null(&sound.path).as_ptr()),
                windows::core::PCWSTR(crate::shared::utils::wide_null(&destination).as_ptr()),
                windows::Win32::Storage::FileSystem::MOVE_FILE_FLAGS(0),
            )
        }
        .context("重命名音效文件失败，请检查名称是否重复或文件是否被占用")?;
        self.metadata.invalidate(&sound.path);
        self.metadata.invalidate(&destination);
        sound.path = destination;
        sound.file_name = file_name;
        self.save()
    }
    pub fn remove(&mut self, index: usize) {
        if index >= self.library.sounds.len() {
            return;
        }
        if self.active == Some(index) {
            self.stop();
            if let Some(job) = self.job.take() {
                let _ = job.thread.join();
            }
            self.active = None;
        }
        if let Err(error) = fs::remove_file(&self.library.sounds[index].path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            self.error(format!("删除音效文件失败：{error}"));
            return;
        }
        if let Some(active) = &mut self.active
            && *active > index
        {
            *active -= 1;
        }
        self.pending = None;
        self.metadata.invalidate(&self.library.sounds[index].path);
        self.library.sounds.remove(index);
        self.save_library();
    }
    pub fn play(&mut self, ctx: &egui::Context, index: usize, guid: Option<&str>) {
        let Some(sound) = self.library.sounds.get(index) else {
            return;
        };
        if self.options.monitor_volume == 0.0 && self.options.send_volume == 0.0 {
            self.error("麦克风和扬声器音量均为 0，未开始播放".into());
            return;
        }
        ctx.request_repaint();
        if self.job.is_some() {
            self.stop();
            self.pending = Some((index, guid.map(str::to_owned)));
            return;
        }
        let path = sound.path.clone();
        let microphone_available = guid.is_some();
        let options = self.options.for_output(microphone_available);
        if options.monitor_volume == 0.0 && options.send_volume == 0.0 {
            self.error("麦克风插件未就绪，且扬声器音量为 0，没有可用的播放输出".into());
            return;
        }
        let transport = match transport_path(guid.unwrap_or("00000000-0000-0000-0000-000000000000"))
        {
            Ok(path) => path,
            Err(e) => {
                self.error(e.to_string());
                return;
            }
        };
        let (tx, rx) = mpsc::channel();
        let (events, receiver) = mpsc::channel();
        let ctx = ctx.clone();
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
                ctx.request_repaint();
            }) {
            Ok(thread) => {
                self.active = Some(index);
                self.job = Some(Job {
                    sender: tx,
                    receiver,
                    thread,
                    microphone_available,
                });
            }
            Err(e) => self.error(format!("无法开始播放：{e}")),
        }
    }
    pub fn stop(&mut self) {
        self.pending = None;
        if let Some(job) = &self.job {
            let _ = job.sender.send(Command::Stop);
        }
    }
    pub fn configure(&self) {
        if let Some(job) = &self.job {
            let _ = job.sender.send(Command::Configure(
                self.options.for_output(job.microphone_available),
            ));
        }
    }
    pub fn poll(&mut self, ctx: &egui::Context) {
        for error in self.metadata.poll(&mut self.library, ctx) {
            self.error(error);
        }
        if let Some(picker) = &self.picker {
            match picker.try_recv() {
                Ok(result) => {
                    self.picker = None;
                    match result {
                        Ok(paths) => self.import(paths),
                        Err(e) => self.error(format!("选择文件失败：{e:#}")),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.picker = None;
                    self.error("文件选择器意外退出".into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        while let Some(job) = &self.job {
            match job.receiver.try_recv() {
                Ok(Event::Started(duration)) => {
                    self.duration = duration;
                    if let Some(sound) = self
                        .active
                        .and_then(|index| self.library.sounds.get_mut(index))
                        && duration.is_some()
                    {
                        sound.duration = duration;
                    }
                    self.message.clear();
                }
                Ok(Event::Progress(elapsed)) => self.elapsed = elapsed,
                Ok(Event::MonitorError(error)) => self.error(error),
                Ok(Event::Finished(result)) => {
                    if let Some(job) = self.job.take() {
                        let _ = job.thread.join();
                    }
                    self.active = None;
                    if let Err(e) = result {
                        self.error(format!("播放失败：{e:#}"));
                    }
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if let Some(job) = self.job.take() {
                        let _ = job.thread.join();
                    }
                    self.active = None;
                    self.error("播放线程意外退出".into());
                    break;
                }
            }
        }
        if self.job.is_none()
            && let Some((index, guid)) = self.pending.take()
        {
            self.play(ctx, index, guid.as_deref());
        }
    }
}
impl Drop for Soundboard {
    fn drop(&mut self) {
        self.stop();
        if let Some(job) = self.job.take() {
            let _ = job.thread.join();
        }
    }
}

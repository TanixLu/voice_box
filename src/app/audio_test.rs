//! Cancellable background capture, DLL preview and playback; no DSP in the EXE.
use std::{
    ptr, slice,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, bail, ensure};
use wasapi::{SampleType, WaveFormat};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Media::Audio::{
            AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDCLNT_STREAMOPTIONS_RAW,
            AudioCategory_Other, AudioClientProperties, IAudioCaptureClient, IAudioClient,
            IAudioClient2, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator, eConsole,
            eRender,
        },
        System::{
            Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree},
            Threading::{CreateEventW, WaitForSingleObject},
        },
    },
    core::{Interface, PCWSTR},
};

use crate::shared::{
    audio::SampleFormat,
    preview::{MAX_RECORD_SECONDS, MAX_SAMPLES},
    settings::Settings,
    utils::wide_null,
};

pub(super) struct Recording {
    pub sample_rate: u32,
    pub channels: u16,
    pub channel_mask: u32,
    pub samples: Vec<f32>,
}

impl Recording {
    fn duration(&self) -> f64 {
        self.samples.len() as f64 / f64::from(self.channels) / f64::from(self.sample_rate)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Activity {
    Record,
    Original,
    Processed,
}

struct Processed {
    settings: Settings,
    samples: Arc<Vec<f32>>,
}

enum Outcome {
    Recorded(Arc<Recording>),
    Played(Option<Processed>),
}

enum Event {
    Started,
    Finished(anyhow::Result<Outcome>),
}

struct Job {
    activity: Activity,
    stop: Arc<AtomicBool>,
    receiver: mpsc::Receiver<Event>,
    started: Option<Instant>,
}

#[derive(Default)]
pub(super) struct AudioTest {
    job: Option<Job>,
    recording: Option<Arc<Recording>>,
    processed: Option<Processed>,
    pub message: String,
    pub failed: bool,
}

impl AudioTest {
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }
    pub fn activity(&self) -> Option<Activity> {
        self.job.as_ref().map(|job| job.activity)
    }
    pub fn has_recording(&self) -> bool {
        self.recording.is_some()
    }
    pub fn stopping(&self) -> bool {
        self.job
            .as_ref()
            .is_some_and(|job| job.stop.load(Ordering::Relaxed))
    }
    pub fn elapsed(&self) -> Option<f64> {
        self.job
            .as_ref()
            .filter(|job| job.activity == Activity::Record)
            .and_then(|job| job.started)
            .map(|started| started.elapsed().as_secs_f64())
    }
    pub fn playback_progress(&self) -> Option<(f64, f64)> {
        let job = self.job.as_ref()?;
        if job.activity == Activity::Record {
            return None;
        }
        let started = job.started?;
        let duration = self.recording.as_ref()?.duration();
        Some((started.elapsed().as_secs_f64().min(duration), duration))
    }
    pub fn stop(&self) {
        if let Some(job) = &self.job {
            job.stop.store(true, Ordering::Relaxed);
        }
    }

    fn begin(
        &mut self,
        ctx: &egui::Context,
        activity: Activity,
        message: &str,
        operation: impl FnOnce(&AtomicBool, &dyn Fn()) -> anyhow::Result<Outcome> + Send + 'static,
    ) {
        if self.busy() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        self.message = message.into();
        self.failed = false;
        match std::thread::Builder::new()
            .name("VoiceBoxAudioTest".into())
            .spawn(move || {
                let started = || {
                    let _ = sender.send(Event::Started);
                    ctx.request_repaint();
                };
                let result = operation(&worker_stop, &started);
                if let Err(error) = &result {
                    tracing::error!("Audio test failed: {error:#}");
                }
                let _ = sender.send(Event::Finished(result));
                ctx.request_repaint();
            }) {
            Ok(_) => {
                self.job = Some(Job {
                    activity,
                    stop,
                    receiver,
                    started: None,
                })
            }
            Err(error) => {
                self.message = format!("无法开始试听操作：{error}");
                self.failed = true;
            }
        }
    }

    pub fn record(&mut self, ctx: &egui::Context, device_id: String) {
        self.begin(
            ctx,
            Activity::Record,
            "正在打开麦克风…",
            move |stop, started| {
                capture(&device_id, stop, started)
                    .map(|recording| Outcome::Recorded(Arc::new(recording)))
            },
        );
    }

    pub fn play(&mut self, ctx: &egui::Context, settings: Option<Settings>) {
        let Some(recording) = self.recording.clone() else {
            return;
        };
        let cached = settings.as_ref().and_then(|settings| {
            self.processed
                .as_ref()
                .filter(|processed| &processed.settings == settings)
                .map(|processed| Arc::clone(&processed.samples))
        });
        let activity = if settings.is_some() {
            Activity::Processed
        } else {
            Activity::Original
        };
        self.begin(
            ctx,
            activity,
            if settings.is_some() {
                "正在准备处理后的声音…"
            } else {
                "正在打开播放设备…"
            },
            move |stop, started| {
                let processed = if let Some(settings) = settings {
                    let samples = match cached {
                        Some(samples) => samples,
                        None => Arc::new(super::preview::process(&recording, &settings)?),
                    };
                    Some(Processed { settings, samples })
                } else {
                    None
                };
                if !stop.load(Ordering::Relaxed) {
                    let samples = processed
                        .as_ref()
                        .map_or(recording.samples.as_slice(), |processed| {
                            processed.samples.as_slice()
                        });
                    playback(&recording, samples, stop, started)?;
                }
                Ok(Outcome::Played(processed))
            },
        );
    }

    pub fn poll(&mut self) {
        loop {
            let Some(job) = &mut self.job else {
                return;
            };
            let event = match job.receiver.try_recv() {
                Ok(event) => event,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.job = None;
                    self.message = "试听操作未完成，请重试。".into();
                    self.failed = true;
                    return;
                }
            };
            match event {
                Event::Started => {
                    job.started = Some(Instant::now());
                    self.message = match job.activity {
                        Activity::Record => "正在录音…",
                        Activity::Original | Activity::Processed => "正在播放…",
                    }
                    .into();
                }
                Event::Finished(result) => {
                    self.job = None;
                    match result {
                        Ok(Outcome::Recorded(recording)) => {
                            self.message = format!("已录音 {:.1} 秒。", recording.duration());
                            self.recording = Some(recording);
                            self.processed = None;
                        }
                        Ok(Outcome::Played(processed)) => {
                            if let Some(processed) = processed {
                                self.processed = Some(processed);
                            }
                            if let Some(recording) = &self.recording {
                                self.message = format!("已录音 {:.1} 秒。", recording.duration());
                            }
                        }
                        Err(error) => {
                            self.message = format!("试听失败：{error:#}");
                            self.failed = true;
                        }
                    }
                    return;
                }
            }
        }
    }
}

impl Drop for AudioTest {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Apartment;
impl Apartment {
    fn new() -> anyhow::Result<Self> {
        wasapi::initialize_mta().ok()?;
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}

struct AudioEvent(HANDLE);
impl AudioEvent {
    fn wait(&self) -> anyhow::Result<()> {
        match unsafe { WaitForSingleObject(self.0, 100) } {
            WAIT_OBJECT_0 | WAIT_TIMEOUT => Ok(()),
            result => bail!("等待音频设备失败：{result:?}"),
        }
    }
}
impl Drop for AudioEvent {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct Stream {
    client: IAudioClient,
    event: AudioEvent,
}
impl Stream {
    fn new(client: IAudioClient, format: &WaveFormat, convert: bool) -> anyhow::Result<Self> {
        let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
        if convert {
            flags |= AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        }
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                1_000_000,
                0,
                &format.wave_fmt.Format,
                None,
            )?;
        }
        let event = AudioEvent(unsafe { CreateEventW(None, false, false, None)? });
        unsafe {
            client.SetEventHandle(event.0)?;
        }
        Ok(Self { client, event })
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

fn enumerator() -> anyhow::Result<IMMDeviceEnumerator> {
    Ok(unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? })
}

fn capture(device_id: &str, stop: &AtomicBool, started: &dyn Fn()) -> anyhow::Result<Recording> {
    let _apartment = Apartment::new()?;
    let device_id = wide_null(device_id);
    let device = unsafe { enumerator()?.GetDevice(PCWSTR(device_id.as_ptr()))? };
    let client = unsafe { device.Activate::<IAudioClient>(CLSCTX_ALL, None)? };
    let client2: IAudioClient2 = client.cast()?;
    let properties = AudioClientProperties {
        cbSize: size_of::<AudioClientProperties>() as u32,
        eCategory: AudioCategory_Other,
        Options: AUDCLNT_STREAMOPTIONS_RAW,
        ..Default::default()
    };
    unsafe { client2.SetClientProperties(&properties) }
        .context("此麦克风无法开启 RAW 录音，无法保证绕过 VoiceBox 处理")?;
    let raw_format = unsafe { client.GetMixFormat()? };
    let format = unsafe { WaveFormat::parse(&*raw_format) };
    unsafe {
        CoTaskMemFree(Some(raw_format.cast()));
    }
    let format = format?;
    let channels = format.get_nchannels();
    let rate = format.get_samplespersec();
    ensure!(
        (1..=32).contains(&channels) && (8_000..=192_000).contains(&rate),
        "麦克风采样率或声道数不支持试听"
    );
    let sample_format = match (format.get_subformat()?, format.get_bitspersample()) {
        (SampleType::Float, 32) => SampleFormat::Float32,
        (SampleType::Float, 64) => SampleFormat::Float64,
        (SampleType::Int, bits) if bits % 8 == 0 => SampleFormat::pcm(
            u32::from(bits / 8),
            u32::from(format.get_validbitspersample()),
        )
        .ok_or_else(|| anyhow::anyhow!("麦克风 PCM 格式不支持试听"))?,
        _ => bail!("麦克风音频格式不支持试听"),
    };
    let frame_bytes = sample_format.bytes() * channels as usize;
    ensure!(
        frame_bytes == format.get_blockalign() as usize,
        "麦克风音频帧格式无效"
    );
    let stream = Stream::new(client, &format, false).context("开启 RAW 录音失败")?;
    let capture: IAudioCaptureClient = unsafe { stream.client.GetService()? };
    let max_frames = (rate * MAX_RECORD_SECONDS) as usize;
    let max_samples =
        (max_frames * channels as usize).min(MAX_SAMPLES / channels as usize * channels as usize);
    let mut recording = Recording {
        sample_rate: rate,
        channels,
        channel_mask: format.get_dwchannelmask(),
        samples: Vec::new(),
    };
    unsafe {
        stream.client.Start()?;
    }
    started();
    let begin = Instant::now();
    let mut last_packet = begin;
    loop {
        while recording.samples.len() < max_samples && unsafe { capture.GetNextPacketSize()? } > 0 {
            let mut data = ptr::null_mut();
            let mut frames = 0;
            let mut flags = 0;
            unsafe {
                capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
            }
            let samples =
                (frames as usize * channels as usize).min(max_samples - recording.samples.len());
            // SILENT packets can have a null pointer. Only create a slice for
            // valid audio data; silence is synthesized from the frame count.
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                recording
                    .samples
                    .resize(recording.samples.len() + samples, 0.0);
            } else if !data.is_null() {
                let bytes = unsafe { slice::from_raw_parts(data, samples * sample_format.bytes()) };
                recording.samples.extend(
                    bytes
                        .chunks_exact(sample_format.bytes())
                        .map(|sample| sample_format.decode(sample) as f32),
                );
            } else {
                unsafe {
                    capture.ReleaseBuffer(frames)?;
                }
                bail!("麦克风返回了无效音频数据");
            }
            unsafe {
                capture.ReleaseBuffer(frames)?;
            }
            last_packet = Instant::now();
        }
        if stop.load(Ordering::Relaxed)
            || recording.samples.len() >= max_samples
            || begin.elapsed() >= Duration::from_secs(u64::from(MAX_RECORD_SECONDS))
        {
            break;
        }
        ensure!(
            last_packet.elapsed() < Duration::from_secs(3),
            "麦克风未返回音频，请检查设备连接和麦克风权限"
        );
        stream.event.wait()?;
    }
    ensure!(
        !recording.samples.is_empty(),
        "没有录到声音，请稍后再结束录音"
    );
    Ok(recording)
}

fn playback(
    recording: &Recording,
    samples: &[f32],
    stop: &AtomicBool,
    started: &dyn Fn(),
) -> anyhow::Result<()> {
    let _apartment = Apartment::new()?;
    let device = unsafe { enumerator()?.GetDefaultAudioEndpoint(eRender, eConsole)? };
    let client = unsafe { device.Activate::<IAudioClient>(CLSCTX_ALL, None)? };
    let format = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        recording.sample_rate as usize,
        recording.channels as usize,
        Some(recording.channel_mask),
    );
    let stream = Stream::new(client, &format, true).context("开启播放设备失败")?;
    let render: IAudioRenderClient = unsafe { stream.client.GetService()? };
    let channels = recording.channels as usize;
    let mut cursor = 0;
    let write = |cursor: &mut usize, available: u32| -> anyhow::Result<()> {
        let frames = (available as usize).min((samples.len() - *cursor) / channels);
        if frames == 0 {
            return Ok(());
        }
        let data = unsafe { render.GetBuffer(frames as u32)? };
        unsafe {
            ptr::copy_nonoverlapping(
                samples.as_ptr().add(*cursor).cast::<u8>(),
                data,
                frames * channels * size_of::<f32>(),
            );
            render.ReleaseBuffer(frames as u32, 0)?;
        }
        *cursor += frames * channels;
        Ok(())
    };
    write(&mut cursor, unsafe { stream.client.GetBufferSize()? })?;
    unsafe {
        stream.client.Start()?;
    }
    started();
    let mut last_padding = u32::MAX;
    let mut last_progress = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let padding = unsafe { stream.client.GetCurrentPadding()? };
        // A short recording may be completely queued before Start; wait until
        // the last frames have actually played, rather than cutting off its tail.
        if padding == 0 && cursor == samples.len() {
            return Ok(());
        }
        if padding != last_padding {
            last_progress = Instant::now();
            last_padding = padding;
        }
        ensure!(
            last_progress.elapsed() < Duration::from_secs(3),
            "播放设备没有响应，请检查设备连接"
        );
        let capacity = unsafe { stream.client.GetBufferSize()? };
        let previous_cursor = cursor;
        write(&mut cursor, capacity.saturating_sub(padding))?;
        if cursor != previous_cursor {
            last_progress = Instant::now();
        }
        stream.event.wait()?;
    }
}

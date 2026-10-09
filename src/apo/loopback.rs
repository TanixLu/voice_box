//! WASAPI playback reference. All COM/audio-client ownership remains on the
//! detached APO worker, including endpoint changes, Stop, Release and teardown.
use super::{
    etw,
    reference::Worker,
    reference_buffer::{BLOCK, CHANNELS, FAILED, IDLE, RATE, RUNNING, Reference},
};
use crate::shared::{
    audio::SampleFormat,
    resample::Resampler,
    settings::{ReferenceDevice, Settings},
};
use std::time::{Duration, Instant};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_FAILED},
        Media::{
            Audio::*, KernelStreaming::KSDATAFORMAT_SUBTYPE_PCM,
            Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
        },
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
                CoUninitialize,
            },
            Threading::{CreateEventW, WaitForMultipleObjects},
        },
    },
    core::PCWSTR,
};
struct Apartment;
impl Apartment {
    fn new() -> anyhow::Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

struct AudioEvent(HANDLE);
impl Drop for AudioEvent {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn packet_continuity(
    flags: u32,
    position: u64,
    frames: u32,
    previous: Option<u64>,
) -> (bool, Option<u64>) {
    let valid = flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0;
    let discontinuity = flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0
        || (valid && previous.is_some_and(|expected| expected != position));
    let next = if valid {
        position.checked_add(u64::from(frames))
    } else {
        None
    };
    (discontinuity, next)
}

fn selected_device(
    enumerator: &IMMDeviceEnumerator,
    selection: &ReferenceDevice,
) -> windows::core::Result<IMMDevice> {
    unsafe {
        match selection {
            ReferenceDevice::DefaultPlayback => {
                enumerator.GetDefaultAudioEndpoint(eRender, eConsole)
            }
            ReferenceDevice::DefaultCommunications => {
                enumerator.GetDefaultAudioEndpoint(eRender, eCommunications)
            }
            ReferenceDevice::Device(id) => {
                let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
                enumerator.GetDevice(PCWSTR(wide.as_ptr()))
            }
        }
    }
}

fn device_id(device: &IMMDevice) -> windows::core::Result<String> {
    let raw = unsafe { device.GetId()? };
    let id = unsafe { raw.to_string() };
    unsafe {
        CoTaskMemFree(Some(raw.0.cast()));
    }
    Ok(id?)
}

struct Loopback {
    capture: IAudioCaptureClient,
    client: IAudioClient,
    id: String,
    channels: usize,
    frame_bytes: usize,
    format: SampleFormat,
    converters: [Resampler; CHANNELS],
    samples: [[f32; BLOCK]; CHANNELS],
    pending: usize,
    sequence: u64,
    timestamp: u64,
    expected_position: Option<u64>,
    event: AudioEvent,
}

impl Loopback {
    fn open(device: IMMDevice) -> anyhow::Result<Self> {
        let id = device_id(&device)?;
        let event = AudioEvent(unsafe { CreateEventW(None, false, false, None)? });
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None)? };
        let raw = unsafe { client.GetMixFormat()? };
        let negotiated = (|| -> anyhow::Result<_> {
            let wave = unsafe { core::ptr::read_unaligned(raw) };
            let channels = wave.nChannels as usize;
            let sample_rate = wave.nSamplesPerSec;
            anyhow::ensure!(
                (1..=32).contains(&channels) && (8_000..=192_000).contains(&sample_rate),
                "Unsupported render format"
            );
            let bytes = u32::from(wave.wBitsPerSample).div_ceil(8);
            let (subtype, valid_bits) = if wave.wFormatTag == 0xfffe && wave.cbSize >= 22 {
                let extended =
                    unsafe { core::ptr::read_unaligned(raw.cast::<WAVEFORMATEXTENSIBLE>()) };
                (
                    extended.SubFormat,
                    unsafe { extended.Samples.wValidBitsPerSample } as u32,
                )
            } else {
                let subtype = match wave.wFormatTag {
                    1 => KSDATAFORMAT_SUBTYPE_PCM,
                    3 => KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
                    _ => anyhow::bail!("Unsupported render sample type"),
                };
                (subtype, u32::from(wave.wBitsPerSample))
            };
            let format = if subtype == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                match bytes {
                    4 => SampleFormat::Float32,
                    8 => SampleFormat::Float64,
                    _ => anyhow::bail!("Invalid float format"),
                }
            } else if subtype == KSDATAFORMAT_SUBTYPE_PCM {
                SampleFormat::pcm(bytes, valid_bits)
                    .ok_or_else(|| anyhow::anyhow!("Invalid PCM format"))?
            } else {
                anyhow::bail!("Unsupported render subtype")
            };
            anyhow::ensure!(
                wave.nBlockAlign as usize == channels * format.bytes(),
                "Invalid render block alignment"
            );
            // Event-driven loopback is supported since Windows 10 1703, below
            // this app's existing Composite MFX minimum of Windows 10 1803.
            // Capture never opens a microphone or its APO pipeline.
            unsafe {
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                    0,
                    0,
                    raw,
                    None,
                )?;
            }
            Ok((format, channels, sample_rate, wave.nBlockAlign as usize))
        })();
        unsafe {
            CoTaskMemFree(Some(raw.cast()));
        }
        let (format, channels, sample_rate, frame_bytes) = negotiated?;
        let capture = unsafe { client.GetService::<IAudioCaptureClient>()? };
        let converters =
            std::array::from_fn(|_| Resampler::new(f64::from(sample_rate), RATE as f64));
        unsafe {
            client.SetEventHandle(event.0)?;
            client.Start()?;
        }
        Ok(Self {
            client,
            capture,
            id,
            channels,
            frame_bytes,
            format,
            converters,
            samples: [[0.0; BLOCK]; CHANNELS],
            pending: 0,
            sequence: 0,
            timestamp: 0,
            expected_position: None,
            event,
        })
    }

    fn reset(&mut self, reference: &Reference) {
        for converter in &mut self.converters {
            converter.reset();
        }
        self.pending = 0;
        self.sequence = 0;
        self.timestamp = 0;
        self.expected_position = None;
        reference.restart(RUNNING, 0);
    }

    fn drain(&mut self, reference: &Reference) -> anyhow::Result<()> {
        // Bound packet work so worker shutdown and settings changes remain responsive.
        for _ in 0..128 {
            if unsafe { self.capture.GetNextPacketSize()? } == 0 {
                break;
            }
            let mut data = core::ptr::null_mut();
            let mut frames = 0;
            let mut flags = 0;
            let mut position = 0;
            let mut timestamp = 0;
            unsafe {
                self.capture.GetBuffer(
                    &mut data,
                    &mut frames,
                    &mut flags,
                    Some(&mut position),
                    Some(&mut timestamp),
                )?;
            }
            // A bad device/QPC timestamp does not invalidate its audio samples.
            // AEC3 estimates delay from the signals; don't discard usable audio.
            let timing_valid = flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0;
            let (discontinuity, next_position) =
                packet_continuity(flags, position, frames, self.expected_position);
            if discontinuity {
                self.reset(reference);
            }
            if self.timestamp == 0 && timing_valid {
                self.timestamp = timestamp
                    .saturating_sub((self.converters[0].delay_seconds() * 10_000_000.0) as u64);
            }
            self.expected_position = next_position;
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            if !silent && frames > 0 && data.is_null() {
                unsafe {
                    self.capture.ReleaseBuffer(frames)?;
                }
                anyhow::bail!("Null loopback packet");
            }
            // Silent WASAPI packets are allowed to have a null pointer.
            let bytes = if silent || frames == 0 {
                &[][..]
            } else {
                unsafe { core::slice::from_raw_parts(data, frames as usize * self.frame_bytes) }
            };
            for frame in 0..frames as usize {
                let mut stereo = [0.0; CHANNELS];
                if !silent {
                    for channel in 0..self.channels {
                        let offset = frame * self.frame_bytes + channel * self.format.bytes();
                        let sample = self
                            .format
                            .decode(&bytes[offset..offset + self.format.bytes()])
                            as f32;
                        if self.channels == 1 {
                            stereo.fill(sample);
                        } else {
                            stereo[channel % CHANNELS] +=
                                sample / self.channels.div_ceil(CHANNELS) as f32;
                        }
                    }
                }
                for (converter, sample) in self.converters.iter_mut().zip(stereo) {
                    converter.push(sample);
                }
                while let Some(left) = self.converters[0].next() {
                    let right = self.converters[1]
                        .next()
                        .expect("equal stereo resampler clocks");
                    self.samples[0][self.pending] = left;
                    self.samples[1][self.pending] = right;
                    self.pending += 1;
                    if self.pending == BLOCK {
                        self.sequence += 1;
                        reference.publish(self.sequence, self.timestamp, &self.samples);
                        self.timestamp += (BLOCK as u64 * 10_000_000) / RATE as u64;
                        self.pending = 0;
                    }
                }
            }
            unsafe {
                self.capture.ReleaseBuffer(frames)?;
            }
        }
        Ok(())
    }

    fn wait(&self, wake: HANDLE) -> anyhow::Result<()> {
        // Bound shutdown/settings response time while avoiding a timer-driven busy
        // poll (and Windows 10's default ~16 ms timer granularity).
        if unsafe { WaitForMultipleObjects(&[wake, self.event.0], false, 50) } == WAIT_FAILED {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

fn report_failure(
    reference: &Reference,
    code: u32,
    message: String,
    previous: &mut Option<String>,
) {
    if previous.as_ref() != Some(&message) {
        etw::apo_log!(etw::WARNING, etw::LIFECYCLE, "{message} (error {code})");
        *previous = Some(message);
    }
    reference.restart(FAILED, code);
}

pub(super) fn run(worker: &Worker) -> anyhow::Result<()> {
    let _apartment = Apartment::new()?;
    let reference = &worker.reference;
    reference.restart(IDLE, 0);
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    let mut settings = Settings::try_load().unwrap_or_default();
    let mut loopback: Option<Loopback> = None;
    let mut last_check = Instant::now() - Duration::from_secs(1);
    let mut last_error = None;
    while !worker.retiring() {
        reference.heartbeat();
        if last_check.elapsed() >= Duration::from_millis(500) {
            last_check = Instant::now();
            if let Ok(updated) = Settings::try_load() {
                settings = updated;
            }
            if !settings.aec_enabled || !reference.capture_requested() {
                last_error = None;
                if loopback.take().is_some() || reference.status().1 != IDLE {
                    reference.restart(IDLE, 0);
                }
            } else {
                let desired = selected_device(&enumerator, &settings.reference_device);
                match desired.and_then(|device| device_id(&device).map(|id| (device, id))) {
                    Ok((device, id)) => {
                        if !loopback.as_ref().is_some_and(|current| current.id == id) {
                            loopback = None;
                            match Loopback::open(device) {
                                Ok(mut opened) => {
                                    etw::apo_log!(
                                        etw::INFO,
                                        etw::LIFECYCLE,
                                        "Playback reference started: {}",
                                        opened.id
                                    );
                                    last_error = None;
                                    opened.reset(reference);
                                    loopback = Some(opened);
                                }
                                Err(error) => report_failure(
                                    reference,
                                    1,
                                    format!("Cannot open playback reference: {error:#}"),
                                    &mut last_error,
                                ),
                            }
                        }
                    }
                    Err(error) => {
                        loopback = None;
                        report_failure(
                            reference,
                            error.code().0 as u32,
                            format!("Cannot select playback reference: {error}"),
                            &mut last_error,
                        );
                    }
                }
            }
        }
        if let Some(current) = &mut loopback
            && let Err(error) = current
                .drain(reference)
                .and_then(|()| current.wait(worker.wake_handle()))
        {
            loopback = None;
            report_failure(
                reference,
                2,
                format!("Playback reference interrupted: {error:#}"),
                &mut last_error,
            );
        }
        if loopback.is_none() {
            worker.wait(50)?;
        }
    }
    drop(loopback);
    reference.restart(IDLE, 0);
    Ok(())
}

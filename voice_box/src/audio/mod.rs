//! WASAPI device I/O only: each submodule opens one kind of stream. Features built on
//! these streams (the voice preview, the soundboard) live in their own modules.

pub mod loopback;
pub mod microphone;
pub mod monitor;
pub mod playback;

use anyhow::bail;
use wasapi::WaveFormat;
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Media::Audio::{
        AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
        AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient, IMMDeviceEnumerator,
        MMDeviceEnumerator,
    },
    System::{
        Com::{CLSCTX_ALL, CoCreateInstance},
        Threading::{CreateEventW, WaitForSingleObject},
    },
};

use crate::i18n::Message;

pub const MICROPHONE_MAX_SECONDS: u32 = 60;
pub const MICROPHONE_MAX_SAMPLES: usize = 16 * 1024 * 1024;
pub const LOOPBACK_MAX_SECONDS: u32 = 600;

/// Interleaved float samples captured from a device.
pub struct Recording {
    pub sample_rate: u32,
    pub channels: u16,
    pub channel_mask: u32,
    pub samples: Vec<f32>,
}

impl Recording {
    pub fn duration_secs(&self) -> f64 {
        self.samples.len() as f64 / f64::from(self.channels) / f64::from(self.sample_rate)
    }
}

struct AudioEvent(HANDLE);
impl AudioEvent {
    fn wait(&self) -> anyhow::Result<()> {
        match unsafe { WaitForSingleObject(self.0, 100) } {
            WAIT_OBJECT_0 | WAIT_TIMEOUT => Ok(()),
            result => bail!(Message::WaitAudioDeviceFailed {
                result: format!("{result:?}")
            }),
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
        Self::with_buffer(client, format, convert, 1_000_000)
    }
    fn with_buffer(
        client: IAudioClient,
        format: &WaveFormat,
        convert: bool,
        duration_hns: i64, // WASAPI durations are measured in 100 ns units.
    ) -> anyhow::Result<Self> {
        Self::initialize(client, format, convert, duration_hns, 0)
    }
    fn loopback(client: IAudioClient, format: &WaveFormat) -> anyhow::Result<Self> {
        Self::initialize(
            client,
            format,
            true,
            1_000_000,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
        )
    }
    fn initialize(
        client: IAudioClient,
        format: &WaveFormat,
        convert: bool,
        duration_hns: i64,
        extra_flags: u32,
    ) -> anyhow::Result<Self> {
        let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK | extra_flags;
        if convert {
            flags |= AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        }
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                duration_hns,
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
        if let Err(error) = unsafe { self.client.Stop() } {
            tracing::warn!("Failed to stop audio stream: {error}");
        }
    }
}

fn enumerator() -> anyhow::Result<IMMDeviceEnumerator> {
    Ok(unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? })
}

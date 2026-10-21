//! Shared WASAPI streams and thread-local COM lifetime.
use anyhow::bail;
use wasapi::WaveFormat;
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Media::Audio::{
        AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient,
        IMMDeviceEnumerator, MMDeviceEnumerator,
    },
    System::{
        Com::{CLSCTX_ALL, CoCreateInstance},
        Threading::{CreateEventW, WaitForSingleObject},
    },
};

pub(super) mod capture;
pub(super) mod monitor;
pub(super) mod playback;

pub(super) struct Apartment;
impl Apartment {
    pub(super) fn new() -> anyhow::Result<Self> {
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
        Self::with_buffer(client, format, convert, 1_000_000)
    }
    fn with_buffer(
        client: IAudioClient,
        format: &WaveFormat,
        convert: bool,
        duration: i64,
    ) -> anyhow::Result<Self> {
        let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
        if convert {
            flags |= AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        }
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                duration,
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

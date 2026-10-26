use super::{Apartment, Stream, enumerator};
use crate::i18n::Text;
use anyhow::ensure;
use std::time::{Duration, Instant};
use voice_box_shared::utils::wide_null;
use wasapi::{SampleType, WaveFormat};
use windows::Win32::{
    Media::Audio::{IAudioClient, IAudioClock, IAudioRenderClient, eConsole, eRender},
    System::Com::{CLSCTX_ALL, CoTaskMemFree},
};
use windows::core::PCWSTR;

/// The soundboard reuses the same WASAPI/COM lifetime and format negotiation as
/// preview playback, with a shorter queue so route and volume changes feel quick.
pub(crate) struct Monitor {
    // Service interfaces must be released before the client and its event handle;
    // COM must remain initialized until all of them have been released.
    render: IAudioRenderClient,
    clock: IAudioClock,
    stream: Stream,
    started: bool,
    cursor: u64,
    last_position: u64,
    last_progress: Instant,
    _apartment: Apartment,
}
impl Monitor {
    pub fn open(device_id: Option<&str>) -> anyhow::Result<Self> {
        let apartment = Apartment::new()?;
        let enumerator = enumerator()?;
        let device = match device_id {
            Some(id) => {
                let id = wide_null(id);
                unsafe { enumerator.GetDevice(PCWSTR(id.as_ptr()))? }
            }
            None => unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole)? },
        };
        let client = unsafe { device.Activate::<IAudioClient>(CLSCTX_ALL, None)? };
        let format = WaveFormat::new(32, 32, &SampleType::Float, 48_000, 2, Some(3));
        let stream = Stream::with_buffer(client, &format, true, 200_000)?;
        let render = unsafe { stream.client.GetService()? };
        let clock = unsafe { stream.client.GetService()? };
        let id = unsafe { device.GetId()? };
        let endpoint_id = unsafe { id.to_string() };
        unsafe { CoTaskMemFree(Some(id.0.cast())) };
        tracing::info!(endpoint_id = ?endpoint_id, buffer_frames = unsafe { stream.client.GetBufferSize()? },
            "Speaker playback stream opened (shared mode)");
        Ok(Self {
            render,
            clock,
            stream,
            started: false,
            cursor: 0,
            last_position: 0,
            last_progress: Instant::now(),
            _apartment: apartment,
        })
    }
    pub fn fill(&mut self, now: u64, sample: impl Fn(u64) -> [f32; 2]) -> anyhow::Result<()> {
        if self.started {
            // Buffer padding can stay constant during normal playback. The
            // device clock detects stalls even when WASAPI keeps returning Ok.
            let mut position = 0;
            unsafe { self.clock.GetPosition(&mut position, None)? };
            if position != self.last_position {
                self.last_position = position;
                self.last_progress = Instant::now();
            }
            ensure!(
                self.last_progress.elapsed() < Duration::from_secs(3),
                Text::PlaybackDeviceUnresponsive
            );
        }
        let padding = unsafe { self.stream.client.GetCurrentPadding()? };
        let capacity = unsafe { self.stream.client.GetBufferSize()? };
        let expected = now + u64::from(padding);
        if self.cursor == 0 || self.cursor.abs_diff(expected) > 960 {
            self.cursor = expected;
        }
        let frames = capacity.saturating_sub(padding);
        if frames > 0 {
            let data = unsafe { self.render.GetBuffer(frames)? }.cast::<f32>();
            for i in 0..frames as usize {
                let stereo = sample(self.cursor + i as u64);
                unsafe {
                    data.add(i * 2).write(stereo[0]);
                    data.add(i * 2 + 1).write(stereo[1]);
                }
            }
            unsafe {
                self.render.ReleaseBuffer(frames, 0)?;
            }
            self.cursor += u64::from(frames);
        }
        if !self.started {
            unsafe {
                self.stream.client.Start()?;
            }
            self.started = true;
            self.last_progress = Instant::now();
        }
        Ok(())
    }
}

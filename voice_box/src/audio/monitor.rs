use std::{
    mem::MaybeUninit,
    slice,
    time::{Duration, Instant},
};

use anyhow::ensure;
use wasapi::{SampleType, WaveFormat};
use windows::{
    Win32::{
        Media::Audio::{IAudioClient, IAudioClock, IAudioRenderClient, eConsole, eRender},
        System::Com::{CLSCTX_ALL, CoTaskMemFree},
    },
    core::PCWSTR,
};

use voice_box_shared::{
    pcm_transport::{PCM_CHANNELS, PCM_MAX_CURSOR_DRIFT_FRAMES, PCM_SAMPLE_RATE, PcmFrame},
    utils::{PeakLimiter, RateMatcher, wide_null},
};

use crate::{
    audio::{Stream, enumerator},
    i18n::Text,
    utils::ComGuard,
};

/// The soundboard reuses the same WASAPI/COM lifetime and format negotiation as
/// preview playback, with a shorter queue so route and volume changes feel quick.
pub struct Monitor {
    // Service interfaces must be released before the client and its event handle;
    // COM must remain initialized until all of them have been released.
    render: IAudioRenderClient,
    clock: IAudioClock,
    stream: Stream,
    started: bool,
    cursor: u64,
    last_position: u64,
    last_progress: Instant,
    matcher: RateMatcher,
    limiter: PeakLimiter,
    _com_guard: ComGuard,
}
impl Monitor {
    pub fn open(device_id: Option<&str>) -> anyhow::Result<Self> {
        let _com_guard = ComGuard::mta()?;
        let enumerator = enumerator()?;
        let device = match device_id {
            Some(id) => {
                let id = wide_null(id);
                unsafe { enumerator.GetDevice(PCWSTR(id.as_ptr()))? }
            }
            None => unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole)? },
        };
        let client = unsafe { device.Activate::<IAudioClient>(CLSCTX_ALL, None)? };
        let format = WaveFormat::new(
            32,
            32,
            &SampleType::Float,
            PCM_SAMPLE_RATE as usize,
            PCM_CHANNELS,
            Some(3),
        );
        let stream = Stream::with_buffer(client, &format, true, 200_000)?;
        let render = unsafe { stream.client.GetService()? };
        let clock = unsafe { stream.client.GetService()? };
        let capacity = unsafe { stream.client.GetBufferSize()? };
        let id = unsafe { device.GetId()? };
        let endpoint_id = unsafe { id.to_string() };
        unsafe { CoTaskMemFree(Some(id.0.cast())) };
        tracing::info!(endpoint_id = ?endpoint_id, buffer_frames = capacity,
            "Speaker playback stream opened (shared mode)");
        Ok(Self {
            render,
            clock,
            stream,
            started: false,
            cursor: 0,
            last_position: 0,
            last_progress: Instant::now(),
            matcher: RateMatcher::new(f64::from(PCM_SAMPLE_RATE), capacity as usize),
            limiter: PeakLimiter::new(f64::from(PCM_SAMPLE_RATE)),
            _com_guard,
        })
    }
    pub fn fill(&mut self, now: u64, sample: impl Fn(u64) -> PcmFrame) -> anyhow::Result<()> {
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
        if self.cursor == 0 || self.cursor.abs_diff(expected) > PCM_MAX_CURSOR_DRIFT_FRAMES {
            self.cursor = expected;
            self.matcher.reset();
        }
        let frames = capacity.saturating_sub(padding);
        if frames > 0 {
            // WASAPI render memory need not contain initialized samples.
            let data = unsafe {
                slice::from_raw_parts_mut(
                    self.render
                        .GetBuffer(frames)?
                        .cast::<MaybeUninit<PcmFrame>>(),
                    frames as usize,
                )
            };
            let error = (i128::from(self.cursor) - i128::from(expected)) as f64
                / f64::from(PCM_SAMPLE_RATE);
            let output = self.matcher.process(frames as usize, error, || {
                let stereo = sample(self.cursor);
                self.cursor += 1;
                stereo
            });
            for (destination, mut stereo) in data.iter_mut().zip(output.iter().copied()) {
                self.limiter.process_frame(&mut stereo);
                destination.write(stereo);
            }
            unsafe {
                self.render.ReleaseBuffer(frames, 0)?;
            }
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

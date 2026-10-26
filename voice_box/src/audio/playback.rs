use super::{Apartment, Stream, capture::Recording, enumerator};
use crate::i18n::Text;
use crate::i18n::UserContext;
use anyhow::ensure;
use std::{
    ptr,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use wasapi::{SampleType, WaveFormat};
use windows::Win32::{
    Media::Audio::{IAudioClient, IAudioRenderClient, eConsole, eRender},
    System::Com::CLSCTX_ALL,
};

pub(crate) fn playback(
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
    let stream = Stream::new(client, &format, true).user_context(Text::OpenPlaybackDevice)?;
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
            Text::PlaybackDeviceUnresponsive
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

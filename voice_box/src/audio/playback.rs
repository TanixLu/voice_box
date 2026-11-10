use std::{
    slice,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use anyhow::ensure;
use wasapi::{SampleType, WaveFormat};
use windows::Win32::{
    Media::Audio::{IAudioClient, IAudioRenderClient, eConsole, eRender},
    System::Com::CLSCTX_ALL,
};

use crate::{
    audio::{Recording, Stream, enumerator},
    i18n::{Text, UserContext},
    win::ComGuard,
};

/// A `play` source that plays `samples` once.
pub fn once(mut samples: &[f32]) -> impl FnMut(&mut [f32]) -> usize + '_ {
    move |buffer| {
        let (head, tail) = samples.split_at(buffer.len().min(samples.len()));
        buffer[..head.len()].copy_from_slice(head);
        samples = tail;
        head.len()
    }
}

/// Plays audio in `recording`'s format. `source` fills a buffer of whole frames
/// and returns how many samples it wrote; fewer than requested ends the audio.
pub fn play(
    recording: &Recording,
    mut source: impl FnMut(&mut [f32]) -> usize,
    stop: &AtomicBool,
    started: &dyn Fn(),
) -> anyhow::Result<()> {
    let _com_guard = ComGuard::mta()?;
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
    let stream = Stream::new(client, &format, true).user_context(Text::OpenPlaybackDeviceFailed)?;
    let render: IAudioRenderClient = unsafe { stream.client.GetService()? };
    let channels = recording.channels as usize;
    let mut finished = false;
    // Returns whether any frames were written.
    let mut write = |finished: &mut bool, available: u32| -> anyhow::Result<bool> {
        if *finished || available == 0 {
            return Ok(false);
        }
        let frames = unsafe {
            let data = render.GetBuffer(available)?;
            let buffer =
                slice::from_raw_parts_mut(data.cast::<f32>(), available as usize * channels);
            (source(buffer) / channels) as u32
        };
        unsafe { render.ReleaseBuffer(frames, 0)? };
        *finished = frames < available;
        Ok(frames > 0)
    };
    write(&mut finished, unsafe { stream.client.GetBufferSize()? })?;
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
        if padding == 0 && finished {
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
        if write(&mut finished, capacity.saturating_sub(padding))? {
            last_progress = Instant::now();
        }
        stream.event.wait()?;
    }
}

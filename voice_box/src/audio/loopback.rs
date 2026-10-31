//! Default-speaker loopback, including time spent with no render packets.

use std::{
    ptr, slice,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use anyhow::ensure;
use wasapi::{SampleType, WaveFormat};
use windows::Win32::{
    Media::Audio::{
        AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, IAudioCaptureClient,
        IAudioClient, eConsole, eRender,
    },
    System::{
        Com::CLSCTX_ALL,
        Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
    },
};

use crate::{
    audio::{Stream, capture::Recording, enumerator},
    i18n::{Text, UserContext},
    utils::ComGuard,
};

pub const MAX_RECORD_SECONDS: u32 = 600;
const RATE: u32 = 48_000;
const CHANNELS: usize = 2;

fn clock_hns(frequency: i64) -> anyhow::Result<u64> {
    let mut counter = 0;
    unsafe { QueryPerformanceCounter(&mut counter)? };
    Ok((counter as u128 * 10_000_000 / frequency as u128) as u64)
}

// WASAPI timestamps use QPC in 100 ns units, independent of the negotiated rate.
fn timestamp_frame(timestamp: u64, origin: u64) -> usize {
    ((u128::from(timestamp.saturating_sub(origin)) * u128::from(RATE) + 5_000_000) / 10_000_000)
        as usize
}

fn place_packet(samples: &mut Vec<f32>, start: usize, frames: usize, data: Option<&[f32]>) {
    let end = (start + frames) * CHANNELS;
    samples.resize(samples.len().max(end), 0.0);
    let destination = &mut samples[start * CHANNELS..end];
    if let Some(data) = data {
        for (output, &input) in destination.iter_mut().zip(data) {
            *output = if input.is_finite() {
                input.clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }
    } else {
        destination.fill(0.0);
    }
}

pub fn capture(stop: &AtomicBool, started: &dyn Fn()) -> anyhow::Result<Recording> {
    let _com_guard = ComGuard::mta()?;
    let device = unsafe { enumerator()?.GetDefaultAudioEndpoint(eRender, eConsole) }
        .user_context(Text::DefaultSpeakerMissing)?;
    let client = unsafe { device.Activate::<IAudioClient>(CLSCTX_ALL, None)? };
    // Let the shared audio engine convert/downmix the endpoint's native format.
    let format = WaveFormat::new(32, 32, &SampleType::Float, RATE as usize, CHANNELS, Some(3));
    let stream = Stream::loopback(client, &format).user_context(Text::CaptureSpeaker)?;
    let capture: IAudioCaptureClient = unsafe { stream.client.GetService()? };
    let mut frequency = 0;
    unsafe { QueryPerformanceFrequency(&mut frequency)? };
    ensure!(frequency > 0, Text::ReadRecordingClock);
    let origin = clock_hns(frequency)?;
    unsafe { stream.client.Start()? };
    let begin = Instant::now();
    started();
    let max_frames = RATE as usize * MAX_RECORD_SECONDS as usize;
    let mut recording = Recording {
        sample_rate: RATE,
        channels: CHANNELS as u16,
        channel_mask: 3,
        samples: Vec::new(),
    };
    let mut stopped_at = None;
    loop {
        let elapsed_frames =
            ((begin.elapsed().as_secs_f64() * f64::from(RATE)).round() as usize).min(max_frames);
        if stop.load(Ordering::Relaxed) || elapsed_frames >= max_frames {
            unsafe { stream.client.Stop()? };
            stopped_at = Some(elapsed_frames);
        }
        // Drain queued packets before filling trailing silence on stop.
        while unsafe { capture.GetNextPacketSize()? } > 0 {
            let mut data = ptr::null_mut();
            let mut frames = 0;
            let mut flags = 0;
            let mut timestamp = 0;
            unsafe {
                capture.GetBuffer(
                    &mut data,
                    &mut frames,
                    &mut flags,
                    None,
                    Some(&mut timestamp),
                )?;
            }
            let start = if flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0 {
                timestamp_frame(timestamp, origin)
            } else {
                // A bad device timestamp must not turn a short recording into a huge gap.
                ((begin.elapsed().as_secs_f64() * f64::from(RATE)) as usize)
                    .saturating_sub(frames as usize)
            }
            .min(max_frames);
            let count = (frames as usize).min(max_frames - start);
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            let valid = silent || !data.is_null() || count == 0;
            if valid && count > 0 {
                let packet = if silent {
                    None
                } else {
                    Some(unsafe { slice::from_raw_parts(data.cast::<f32>(), count * CHANNELS) })
                };
                place_packet(&mut recording.samples, start, count, packet);
            }
            unsafe { capture.ReleaseBuffer(frames)? };
            ensure!(valid, Text::InvalidSpeakerData);
            if start + count >= max_frames {
                break;
            }
        }
        if let Some(elapsed_frames) = stopped_at {
            // Loopback may deliver no packets while all applications are silent.
            recording.samples.resize(elapsed_frames * CHANNELS, 0.0);
            break;
        }
        stream.event.wait()?;
    }
    ensure!(!recording.samples.is_empty(), Text::RecordingTooShort);
    Ok(recording)
}

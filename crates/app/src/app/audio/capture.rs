use super::{Apartment, Stream, enumerator};
use crate::app::i18n::Text;
use crate::app::i18n::UserContext;
use crate::shared::{
    audio::SampleFormat,
    preview::{MAX_RECORD_SECONDS, MAX_SAMPLES},
    utils::wide_null,
};
use anyhow::{bail, ensure};
use std::{
    ptr, slice,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use wasapi::{SampleType, WaveFormat};
use windows::{
    Win32::{
        Media::Audio::{
            AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_STREAMOPTIONS_RAW, AudioCategory_Other,
            AudioClientProperties, IAudioCaptureClient, IAudioClient, IAudioClient2,
        },
        System::Com::{CLSCTX_ALL, CoTaskMemFree},
    },
    core::{Interface, PCWSTR},
};

pub(in crate::app) struct Recording {
    pub sample_rate: u32,
    pub channels: u16,
    pub channel_mask: u32,
    pub samples: Vec<f32>,
}

impl Recording {
    pub(in crate::app) fn duration_secs(&self) -> f64 {
        self.samples.len() as f64 / f64::from(self.channels) / f64::from(self.sample_rate)
    }
}

pub(in crate::app) fn capture(
    device_id: &str,
    stop: &AtomicBool,
    started: &dyn Fn(),
) -> anyhow::Result<Recording> {
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
        .user_context(Text::RawCaptureUnavailable)?;
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
        Text::UnsupportedPreviewChannels
    );
    let sample_format = match (format.get_subformat()?, format.get_bitspersample()) {
        (SampleType::Float, 32) => SampleFormat::Float32,
        (SampleType::Float, 64) => SampleFormat::Float64,
        (SampleType::Int, bits) if bits % 8 == 0 => SampleFormat::pcm(
            u32::from(bits / 8),
            u32::from(format.get_validbitspersample()),
        )
        .ok_or_else(|| anyhow::anyhow!(Text::UnsupportedPreviewPcm))?,
        _ => bail!(Text::UnsupportedPreviewFormat),
    };
    let frame_bytes = sample_format.bytes() * channels as usize;
    ensure!(
        frame_bytes == format.get_blockalign() as usize,
        Text::InvalidMicrophoneFrame
    );
    let stream = Stream::new(client, &format, false).user_context(Text::OpenRawCapture)?;
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
                bail!(Text::InvalidMicrophoneData);
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
            Text::MicrophoneUnresponsive
        );
        stream.event.wait()?;
    }
    ensure!(!recording.samples.is_empty(), Text::NoRecordedAudio);
    Ok(recording)
}

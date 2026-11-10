//! Process and align recordings with the same DSP used by the real-time APO.

use std::{mem::size_of, slice};

use anyhow::ensure;

use voice_box_shared::{
    apo_config::ApoConfig,
    dsp::{AudioProcessor, MAX_CHANNELS, MAX_SAMPLE_RATE, MIN_SAMPLE_RATE, SampleFormat},
};

use crate::{
    audio::{PREVIEW_MAX_RECORD_SECONDS, PREVIEW_MAX_SAMPLES, capture::Recording},
    i18n::{Text, UserContext},
};

pub fn process(recording: &Recording, config: &ApoConfig) -> anyhow::Result<Vec<f32>> {
    let channels = usize::from(recording.channels);
    let samples = &recording.samples;
    ensure!(
        (MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&f64::from(recording.sample_rate))
            && (1..=MAX_CHANNELS).contains(&channels)
            && !samples.is_empty()
            && samples.len().is_multiple_of(channels)
            && samples.len() <= PREVIEW_MAX_SAMPLES
            && samples.len() / channels
                <= recording.sample_rate as usize * PREVIEW_MAX_RECORD_SECONDS as usize,
        Text::InvalidRecordingData
    );
    if config.gain_db.is_mute() {
        return Ok(vec![0.0; samples.len()]);
    }
    let mut processor = AudioProcessor::new(
        SampleFormat::Float32,
        channels,
        f64::from(recording.sample_rate),
        true,
        config,
    )
    .user_context(Text::ProcessPreview)?;
    // Drain the resamplers, model, FIFO and limiter, then remove the latency
    // so original and processed playback have the same length and start point.
    let delay_samples = processor.latency_frames() * channels;
    let mut buffer = samples.to_vec();
    buffer.resize(samples.len() + delay_samples, 0.0);
    // The float buffer is initialized and exclusively borrowed for this call.
    let bytes = unsafe {
        slice::from_raw_parts_mut(
            buffer.as_mut_ptr().cast::<u8>(),
            buffer.len() * size_of::<f32>(),
        )
    };
    processor.process_with_config(bytes, config, None);
    buffer.copy_within(delay_samples.., 0);
    buffer.truncate(samples.len());
    Ok(buffer)
}

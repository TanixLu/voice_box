//! Bounded packet decoding and stereo sinc conversion; never runs on an APO thread.
use crate::i18n::Text;
use crate::i18n::UserContext;
use anyhow::ensure;
use std::collections::VecDeque;
use symphonia::core::{
    codecs::audio::{AudioDecoder, AudioDecoderOptions},
    formats::{FormatReader, TrackType, probe::Hint},
    io::{MediaSource, MediaSourceStream},
};
use voice_box_shared::{
    audio::{MAX_SAMPLE_RATE, MIN_SAMPLE_RATE},
    pcm_transport::{SAMPLE_RATE, sanitize_sample},
    resample::Resampler,
};

pub(super) struct SoundDecoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    rate: u32,
    channels: usize,
    resamplers: [Resampler; 2],
    delay_frames_remaining: usize,
    samples: Vec<f32>,
    output: VecDeque<[f32; 2]>,
    input_frames: usize,
    output_frames: usize,
    eof: bool,
    pub duration_secs: Option<f64>,
}

impl SoundDecoder {
    pub fn new(source: Box<dyn MediaSource>) -> anyhow::Result<Self> {
        let format = open_format(source)?;
        let track = format
            .default_track(TrackType::Audio)
            .user_context(Text::NoAudioTrack)?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .user_context(Text::InvalidTrackParameters)?;
        let rate = params.sample_rate.user_context(Text::MissingSampleRate)?;
        let channels = params
            .channels
            .as_ref()
            .user_context(Text::MissingChannels)?
            .count();
        ensure!(
            (MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&f64::from(rate))
                && (1..=2).contains(&channels),
            Text::SupportedAudioRange
        );
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default().gapless(true))
            .user_context(Text::CreateAudioDecoder)?;
        let duration_secs = track.num_frames.map(|n| n as f64 / f64::from(rate));
        let id = track.id;
        let resamplers = [
            Resampler::new(f64::from(rate), f64::from(SAMPLE_RATE)),
            Resampler::new(f64::from(rate), f64::from(SAMPLE_RATE)),
        ];
        let delay_frames_remaining =
            (resamplers[0].delay_seconds().max(0.0) * f64::from(SAMPLE_RATE)).round() as usize;
        Ok(Self {
            format,
            decoder,
            track_id: id,
            rate,
            channels,
            resamplers,
            delay_frames_remaining,
            samples: Vec::new(),
            output: VecDeque::new(),
            input_frames: 0,
            output_frames: 0,
            eof: false,
            duration_secs,
        })
    }
    fn push(&mut self, frame: [f32; 2]) {
        for (r, sample) in self.resamplers.iter_mut().zip(frame) {
            r.push(sample);
        }
        while let Some(left) = self.resamplers[0].next_sample() {
            let right = self.resamplers[1]
                .next_sample()
                .expect("identical stereo converters");
            if self.delay_frames_remaining > 0 {
                self.delay_frames_remaining -= 1;
            } else {
                self.output
                    .push_back([sanitize_sample(left), sanitize_sample(right)]);
            }
        }
    }
    pub fn next_frame(&mut self) -> anyhow::Result<Option<[f32; 2]>> {
        loop {
            if self.eof
                && self.output_frames
                    >= (self.input_frames as f64 * f64::from(SAMPLE_RATE) / f64::from(self.rate))
                        .round() as usize
            {
                return Ok(None);
            }
            if let Some(sample) = self.output.pop_front() {
                self.output_frames += 1;
                return Ok(Some(sample));
            }
            if self.eof {
                return Ok(None);
            }
            let Some(packet) = self
                .format
                .next_packet()
                .user_context(Text::ReadAudioData)?
            else {
                self.eof = true;
                // Flush the converter's filter and partial chunk, but never append
                // its padding to the musical duration.
                let padding = (self.rate / 20) as usize + self.resamplers[0].input_chunk_size();
                for _ in 0..padding {
                    self.push([0.0; 2]);
                }
                continue;
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let audio = self
                .decoder
                .decode(&packet)
                .user_context(Text::DecodeAudio)?;
            ensure!(
                audio.spec().rate() == self.rate
                    && audio.spec().channels().count() == self.channels,
                Text::ChangingAudioFormat
            );
            ensure!(
                audio.samples_interleaved() <= 2 * 1024 * 1024,
                Text::AudioPacketTooLarge
            );
            self.samples.resize(audio.samples_interleaved(), 0.0);
            audio.copy_to_slice_interleaved(&mut self.samples);
            self.input_frames += self.samples.len() / self.channels;
            for i in (0..self.samples.len()).step_by(self.channels) {
                let l = sanitize_sample(self.samples[i]);
                let r = if self.channels == 2 {
                    sanitize_sample(self.samples[i + 1])
                } else {
                    l
                };
                self.push([l, r]);
            }
        }
    }
}

pub(super) fn open_format(source: Box<dyn MediaSource>) -> anyhow::Result<Box<dyn FormatReader>> {
    symphonia::default::get_probe()
        .probe(
            &Hint::new(),
            MediaSourceStream::new(source, Default::default()),
            Default::default(),
            Default::default(),
        )
        .user_context(Text::UnsupportedAudioFormat)
}

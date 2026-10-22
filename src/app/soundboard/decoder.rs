//! Bounded packet decoding and stereo sinc conversion; never runs on an APO thread.
use crate::shared::{
    pcm_transport::{SAMPLE_RATE, sanitize_sample},
    resample::Resampler,
};
use anyhow::{Context, ensure};
use std::{
    collections::VecDeque,
    fs::File,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
use symphonia::core::{
    codecs::audio::{AudioDecoder, AudioDecoderOptions},
    formats::{FormatReader, TrackType, probe::Hint},
    io::MediaSourceStream,
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
    pub duration: Option<f64>,
}

impl SoundDecoder {
    /// Reads container timing, scanning packets only when the header lacks it.
    pub fn probe_duration(path: &Path, stop: &AtomicBool) -> anyhow::Result<f64> {
        let mut format = open_format(path)?;
        let track = format
            .default_track(TrackType::Audio)
            .context("文件没有音轨")?;
        let time_base = track.time_base.context("音轨缺少时间基准")?;
        if let Some(duration) = track
            .duration
            .and_then(|duration| time_base.calc_duration(duration))
        {
            return Ok(duration.as_secs_f64());
        }
        if let Some(frames) = track.num_frames
            && let Some(rate) = track
                .codec_params
                .as_ref()
                .and_then(|params| params.audio())
                .and_then(|params| params.sample_rate)
                .filter(|&rate| rate > 0)
        {
            return Ok(frames as f64 / f64::from(rate));
        }
        let id = track.id;
        let mut ticks = symphonia::core::units::Duration::ZERO;
        while !stop.load(Ordering::Relaxed) {
            let Some(packet) = format.next_packet().context("读取音频时长失败")? else {
                break;
            };
            if packet.track_id == id {
                ticks = ticks.saturating_add(packet.dur);
            }
        }
        ensure!(!stop.load(Ordering::Relaxed), "已取消读取时长");
        Ok(time_base
            .calc_duration(ticks)
            .context("音频时长无效")?
            .as_secs_f64())
    }
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let format = open_format(path)?;
        let track = format
            .default_track(TrackType::Audio)
            .context("文件没有音轨")?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .context("音轨参数无效")?;
        let rate = params.sample_rate.context("音轨缺少采样率")?;
        let channels = params
            .channels
            .as_ref()
            .context("音轨缺少声道信息")?
            .count();
        ensure!(
            (8_000..=192_000).contains(&rate) && (1..=2).contains(&channels),
            "首版支持 8–192 kHz 单声道或立体声音频"
        );
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default().gapless(true))
            .context("无法创建音频解码器")?;
        let duration = track.num_frames.map(|n| n as f64 / f64::from(rate));
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
            duration,
        })
    }
    fn push(&mut self, frame: [f32; 2]) {
        for (r, sample) in self.resamplers.iter_mut().zip(frame) {
            r.push(sample);
        }
        while let Some(left) = self.resamplers[0].next() {
            let right = self.resamplers[1]
                .next()
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
            let Some(packet) = self.format.next_packet().context("读取音频数据失败")?
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
            let audio = self.decoder.decode(&packet).context("解码音频失败")?;
            ensure!(
                audio.spec().rate() == self.rate
                    && audio.spec().channels().count() == self.channels,
                "不支持播放中改变采样率或声道数的音频"
            );
            ensure!(
                audio.samples_interleaved() <= 2 * 1024 * 1024,
                "音频数据包过大"
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

fn open_format(path: &Path) -> anyhow::Result<Box<dyn FormatReader>> {
    let file = File::open(path).context("无法打开音频文件")?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        hint.with_extension(ext);
    }
    symphonia::default::get_probe()
        .probe(
            &hint,
            MediaSourceStream::new(Box::new(file), Default::default()),
            Default::default(),
            Default::default(),
        )
        .context("不支持此音频格式，请使用 MP3、WAV、FLAC 或 OGG/Vorbis")
}

use std::{
    collections::VecDeque,
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::ensure;

use voice_box_shared::pcm_transport::{
    PCM_DECODE_AHEAD_FRAMES, PCM_MAX_CURSOR_DRIFT_FRAMES, PCM_SAMPLE_RATE, PcmFrame, PcmWriter,
    pcm_frame, pcm_transport_name,
};

use crate::{
    audio::monitor::Monitor,
    devices,
    hotkeys::{HeldKeys, Hotkey},
    i18n::{Details, Message, Text, UserContext},
    soundboard::{decoder::SoundDecoder, sound::SoundId},
    utils::GainPercent,
};

/// Runtime output choices only; no GUI preferences or persistence behavior.
#[derive(Clone)]
pub struct Config {
    pub microphone_device: Option<String>,
    pub speaker_device: Option<String>,
    pub microphone_gain_percent: GainPercent,
    pub speaker_gain_percent: GainPercent,
    pub hold_hotkey: Option<Hotkey>,
    pub repeat: bool,
}

pub enum Command {
    Stop,
    Replace(SoundId, u64, Config),
    Configure(Config),
}
pub enum Event {
    Started {
        revision: u64,
        duration_secs: Option<f64>,
    },
    Progress {
        revision: u64,
        elapsed_secs: f64,
    },
    OutputError(u64, Message),
    Finished(anyhow::Result<()>),
}

pub struct PlaybackOutcome {
    pub sound_id: SoundId,
    pub reason: &'static str,
    /// Playback time in seconds for audio frames whose scheduled time has elapsed.
    pub played_secs: f64,
}

/// PCM before output gain, plus the history the speaker cursor can still request.
struct PlaybackBuffer {
    base: u64,
    played_until: u64,
    samples: VecDeque<PcmFrame>,
}

impl PlaybackBuffer {
    fn new(base: u64) -> Self {
        Self {
            base,
            played_until: base,
            samples: VecDeque::with_capacity(
                (PCM_DECODE_AHEAD_FRAMES + PCM_MAX_CURSOR_DRIFT_FRAMES) as usize,
            ),
        }
    }

    fn end(&self) -> u64 {
        self.base + self.samples.len() as u64
    }

    fn reset(&mut self, base: u64) {
        self.base = base;
        self.played_until = base;
        self.samples.clear();
    }

    fn advance(&mut self, now: u64) -> u64 {
        let played = now.min(self.end()).saturating_sub(self.played_until);
        self.played_until = self.played_until.max(now);
        // Retained history must not count toward progress or delay completion.
        let discard = now
            .saturating_sub(PCM_MAX_CURSOR_DRIFT_FRAMES)
            .saturating_sub(self.base)
            .min(self.samples.len() as u64);
        self.samples.drain(..discard as usize);
        self.base += discard;
        if self.end() < now {
            if self.samples.is_empty() {
                self.base = now;
            } else {
                // A short underrun leaves a hole before newly decoded PCM.
                // Keep older samples at their original timestamps for the speaker.
                self.samples
                    .resize((now - self.base) as usize, PcmFrame::default());
            }
        }
        played
    }

    fn sample(&self, frame: u64) -> PcmFrame {
        frame
            .checked_sub(self.base)
            .and_then(|offset| self.samples.get(offset as usize))
            .copied()
            .unwrap_or_default()
    }
}

fn drain_commands(
    commands: &mpsc::Receiver<Command>,
    configure: &mut Option<Config>,
) -> Result<Option<(SoundId, u64)>, &'static str> {
    let mut replacement = None;
    loop {
        match commands.try_recv() {
            Ok(Command::Stop) => return Err("stopped"),
            Err(mpsc::TryRecvError::Disconnected) => return Err("controller_disconnected"),
            Ok(Command::Replace(id, revision, next)) => {
                replacement = Some((id, revision));
                *configure = Some(next);
            }
            Ok(Command::Configure(next)) => *configure = Some(next),
            Err(mpsc::TryRecvError::Empty) => return Ok(replacement),
        }
    }
}

pub fn run(
    mut id: SoundId,
    mut settings: Config,
    mut revision: u64,
    commands: &mpsc::Receiver<Command>,
    events: &mpsc::Sender<Event>,
) -> anyhow::Result<PlaybackOutcome> {
    let mut decoder = SoundDecoder::new(Box::new(crate::database::audio::AudioSource::open(id)?))?;
    let mut duration_secs = decoder.duration_secs;
    let mut mapping: Option<PcmWriter> = None;
    let mut generation = 0;
    let mut microphone_cursor = 0;
    let mut pending = PlaybackBuffer::new(pcm_frame() + 4800);
    let mut played = 0_u64;
    let mut decoded = 0_u64;
    let mut eof = false;
    let mut monitor: Option<Monitor> = None;
    let mut monitor_failed = false;
    let mut held = HeldKeys::default();
    let mut held_binding = None;
    let mut progress = Instant::now();
    tracing::info!(
        sound_id = id,
        duration_secs = ?duration_secs,
        speaker_gain_percent = ?settings.speaker_gain_percent,
        microphone_gain_percent = ?settings.microphone_gain_percent,
        repeat = settings.repeat,
        "Soundboard playback started"
    );
    let _ = events.send(Event::Started {
        revision,
        duration_secs,
    });
    // Queue the initial settings through the same output validation as live changes.
    let mut configure = Some(settings.clone());
    let reason = 'playback: loop {
        let mut now = pcm_frame();
        // Keep the EOF timestamp fixed while lagging outputs finish reading.
        let advance_to = if eof { now.min(pending.end()) } else { now };
        played += pending.advance(advance_to);
        // Coalesce rapid requests before decoding or touching WASAPI. A stop
        // cancels pending replacements; the controller retains later requests.
        let replacement = match drain_commands(commands, &mut configure) {
            Ok(replacement) => replacement,
            Err(reason) => break 'playback reason,
        };
        if configure.is_some() {
            monitor_failed = false;
        }
        if let Some((next_id, next_revision)) = replacement {
            tracing::info!(
                sound_id = id,
                played_secs = played as f64 / f64::from(PCM_SAMPLE_RATE),
                reason = "replaced",
                "Soundboard sound finished"
            );
            if let Some(mapping) = &mut mapping {
                mapping.stop();
            }
            generation = 0;
            decoder = SoundDecoder::new(Box::new(crate::database::audio::AudioSource::open(
                next_id,
            )?))?;
            id = next_id;
            revision = next_revision;
            duration_secs = decoder.duration_secs;
            now = pcm_frame();
            pending.reset(now + 4800);
            played = 0;
            decoded = 0;
            eof = false;
            progress = Instant::now();
            // Keep the render client running on its owning COM thread. Its short
            // queue drains while the new sound's timeline is prepared.
            tracing::info!(sound_id = id, duration_secs = ?duration_secs,
                speaker_stream_reused = monitor.is_some(), "Soundboard playback switched");
            let _ = events.send(Event::Started {
                revision,
                duration_secs,
            });
        }
        if let Some(mut next) = configure.take() {
            if next.microphone_gain_percent != settings.microphone_gain_percent && generation != 0 {
                if let Some(mapping) = &mut mapping {
                    mapping.stop();
                }
                generation = 0;
            }
            if !next.microphone_gain_percent.is_mute() && generation == 0 {
                let result = (|| -> anyhow::Result<()> {
                    if mapping.is_none() {
                        let guid =
                            devices::soundboard_microphone_guid(next.microphone_device.as_deref())?;
                        mapping = Some(
                            PcmWriter::open(&pcm_transport_name(&guid)?)
                                .user_context(Text::CreatePlaybackBuffer)?,
                        );
                    }
                    if let Some(mapping) = &mut mapping {
                        generation = mapping.next_generation();
                    }
                    // Re-export the buffered audio, even when decoding has reached EOF.
                    microphone_cursor = pending.base.max(now.saturating_sub(480));
                    Ok(())
                })();
                if let Err(error) = result {
                    let error = Message::MicrophoneOutputFailed {
                        error: Details::new(error),
                    };
                    if next.speaker_gain_percent.is_mute() {
                        return Err(anyhow::anyhow!(error));
                    }
                    tracing::warn!("{error}");
                    let _ = events.send(Event::OutputError(revision, error));
                    next.microphone_gain_percent = GainPercent::new(0);
                }
            }
            settings = next;
            if pending.samples.is_empty() {
                // Device discovery can block; schedule startup after it finishes.
                now = pcm_frame();
                pending.reset(now + 4800);
            }
        }
        if settings.microphone_gain_percent.is_mute() && settings.speaker_gain_percent.is_mute() {
            break "outputs_muted";
        }
        if settings.hold_hotkey != held_binding {
            drop(held);
            held = if let Some(key) = settings.hold_hotkey {
                HeldKeys::press(key)?
            } else {
                HeldKeys::default()
            };
            held_binding = settings.hold_hotkey;
        }
        if eof && settings.repeat {
            decoder = SoundDecoder::new(Box::new(crate::database::audio::AudioSource::open(id)?))?;
            decoded = 0;
            eof = false;
        }
        let fill_end = (now + PCM_DECODE_AHEAD_FRAMES).min(pending.end() + 2400);
        while !eof && pending.end() < fill_end {
            // Decode at most 50 ms per pass so stop/configuration stay responsive.
            match decoder.next_frame()? {
                Some(stereo) => {
                    pending.samples.push_back(stereo);
                    decoded += 1;
                }
                None if settings.repeat && decoded > 0 => {
                    decoder = SoundDecoder::new(Box::new(
                        crate::database::audio::AudioSource::open(id)?,
                    ))?;
                    decoded = 0;
                }
                None => {
                    ensure!(
                        played > 0 || !pending.samples.is_empty(),
                        Text::NoPlayableAudio
                    );
                    eof = true;
                }
            }
        }
        if generation != 0
            && let Some(mapping) = &mut mapping
        {
            let now = pcm_frame();
            microphone_cursor = microphone_cursor
                .max(pending.base)
                .max(now.saturating_sub(480));
            // Bake this route's gain into PCM without clipping; the APO limits
            // only after resampling and mixing with the microphone.
            let gain = settings.microphone_gain_percent.as_f32_gain();
            while microphone_cursor < pending.end() {
                let stereo = pending.sample(microphone_cursor);
                mapping.write(
                    microphone_cursor,
                    generation,
                    [stereo[0] * gain, stereo[1] * gain],
                );
                microphone_cursor += 1;
            }
            mapping.publish(generation, pending.end(), now);
        }
        if !settings.speaker_gain_percent.is_mute() && !monitor_failed {
            let result = (|| -> anyhow::Result<()> {
                if monitor.is_none() {
                    monitor = Some(Monitor::open(settings.speaker_device.as_deref())?);
                }
                // Opening a device or decoding a packet can take time. Read the
                // current clock so the render queue does not use a stale timeline.
                monitor
                    .as_mut()
                    .expect("UNREACHABLE: monitor should exist after successful initialization")
                    .fill(pcm_frame(), |frame| {
                        let stereo = pending.sample(frame);
                        [
                            stereo[0] * settings.speaker_gain_percent.as_f32_gain(),
                            stereo[1] * settings.speaker_gain_percent.as_f32_gain(),
                        ]
                    })
            })();
            if let Err(error) = result {
                if settings.microphone_gain_percent.is_mute() {
                    return Err(error.user_context(Text::SpeakerPlaybackFailed));
                }
                monitor = None;
                monitor_failed = true;
                tracing::warn!("Speaker playback failed; microphone output continues: {error:#}");
                let _ = events.send(Event::OutputError(
                    revision,
                    Message::SpeakerMonitorFailed {
                        error: Details::new(error),
                    },
                ));
            }
        } else {
            monitor = None;
        }
        if eof && pending.end().saturating_add(PCM_MAX_CURSOR_DRIFT_FRAMES) <= now {
            break "completed";
        }
        if progress.elapsed() >= Duration::from_millis(100) {
            let mut elapsed_secs = played as f64 / f64::from(PCM_SAMPLE_RATE);
            if settings.repeat
                && let Some(duration_secs) = duration_secs.filter(|&d| d > 0.0)
            {
                elapsed_secs %= duration_secs;
            }
            let _ = events.send(Event::Progress {
                revision,
                elapsed_secs,
            });
            progress = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let played_secs = played as f64 / f64::from(PCM_SAMPLE_RATE);
    let _ = events.send(Event::Progress {
        revision,
        elapsed_secs: played_secs,
    });
    if reason == "completed" {
        std::thread::sleep(Duration::from_millis(30));
    }
    Ok(PlaybackOutcome {
        sound_id: id,
        reason,
        played_secs,
    })
}

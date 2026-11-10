use std::{
    collections::VecDeque,
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::ensure;

use voice_box_shared::pcm_transport::{
    PCM_DECODE_AHEAD_FRAMES, PCM_MAX_CURSOR_DRIFT_FRAMES, PCM_SAMPLE_RATE, PcmFrame, PcmWriter,
    pcm_frame,
};

use crate::{
    audio::monitor::Monitor,
    database::audio::AudioSource,
    hotkeys::HeldKeys,
    i18n::{Details, Message, Text, UserContext},
    model::{Hotkey, SoundId},
    soundboard::{
        decoder::SoundDecoder,
        prefs::{OutputDevice, Prefs},
    },
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

impl Config {
    pub fn new(prefs: &Prefs) -> Self {
        let id = |device: &Option<OutputDevice>| device.as_ref().map(|device| device.id.clone());
        Self {
            microphone_device: id(&prefs.microphone_device),
            speaker_device: id(&prefs.speaker_device),
            microphone_gain_percent: prefs.microphone_gain_percent,
            speaker_gain_percent: prefs.speaker_gain_percent,
            hold_hotkey: prefs.keys.hold,
            repeat: prefs.repeat,
        }
    }
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

/// One sound read from the library, restarted from the beginning when it repeats.
struct Source {
    id: SoundId,
    decoder: SoundDecoder,
    /// Frames decoded since the last start.
    decoded: u64,
    eof: bool,
}

impl Source {
    fn open(id: SoundId) -> anyhow::Result<Self> {
        Ok(Self {
            id,
            decoder: SoundDecoder::new(Box::new(AudioSource::open(id)?))?,
            decoded: 0,
            eof: false,
        })
    }

    fn restart(&mut self) -> anyhow::Result<()> {
        *self = Self::open(self.id)?;
        Ok(())
    }
}

/// PCM exported through shared memory to the APO on the soundboard microphone.
#[derive(Default)]
struct MicrophoneRoute {
    writer: Option<PcmWriter>,
    /// Zero while nothing is exported.
    generation: u16,
    cursor: u64,
}

impl MicrophoneRoute {
    fn stop(&mut self) {
        if let Some(writer) = &mut self.writer {
            writer.stop();
        }
        self.generation = 0;
    }

    /// Starts a generation that re-exports the buffered audio, even when decoding has reached EOF.
    fn start(
        &mut self,
        transport: &Result<String, Details>,
        buffer: &PlaybackBuffer,
        now: u64,
    ) -> Result<(), Details> {
        let writer = match self.writer.take() {
            Some(writer) => writer,
            None => PcmWriter::open(transport.as_ref().map_err(Clone::clone)?)
                .user_context(Text::CreatePlaybackBuffer)
                .map_err(Details::new)?,
        };
        self.generation = self.writer.insert(writer).next_generation();
        self.cursor = buffer.base.max(now.saturating_sub(480));
        Ok(())
    }

    /// Bakes this route's gain into PCM without clipping; the APO limits
    /// only after resampling and mixing with the microphone.
    fn write(&mut self, buffer: &PlaybackBuffer, gain: f32) {
        let Some(writer) = self.writer.as_mut().filter(|_| self.generation != 0) else {
            return;
        };
        let now = pcm_frame();
        self.cursor = self.cursor.max(buffer.base).max(now.saturating_sub(480));
        while self.cursor < buffer.end() {
            let [left, right] = buffer.sample(self.cursor);
            writer.write(self.cursor, self.generation, [left * gain, right * gain]);
            self.cursor += 1;
        }
        writer.publish(self.generation, buffer.end(), now);
    }
}

/// Monitoring on a speaker; after a failure it stays off until the next configuration.
#[derive(Default)]
struct SpeakerRoute {
    monitor: Option<Monitor>,
    failed: bool,
}

impl SpeakerRoute {
    fn play(&mut self, settings: &Config, buffer: &PlaybackBuffer) -> anyhow::Result<()> {
        if settings.speaker_gain_percent.is_mute() || self.failed {
            self.monitor = None;
            return Ok(());
        }
        let gain = settings.speaker_gain_percent.as_f32_gain();
        let result = self
            .monitor
            .take()
            .map_or_else(|| Monitor::open(settings.speaker_device.as_deref()), Ok)
            .and_then(|monitor| {
                // Opening a device or decoding a packet can take time. Read the
                // current clock so the render queue does not use a stale timeline.
                self.monitor.insert(monitor).fill(pcm_frame(), |frame| {
                    let [left, right] = buffer.sample(frame);
                    [left * gain, right * gain]
                })
            });
        if result.is_err() {
            self.monitor = None;
            self.failed = true;
        }
        result
    }
}

/// `transport` names the shared memory of the configured soundboard microphone.
pub fn run(
    id: SoundId,
    mut settings: Config,
    mut revision: u64,
    transport: Result<String, Details>,
    commands: &mpsc::Receiver<Command>,
    events: &mpsc::Sender<Event>,
) -> anyhow::Result<PlaybackOutcome> {
    let mut source = Source::open(id)?;
    let mut microphone = MicrophoneRoute::default();
    let mut speaker = SpeakerRoute::default();
    let mut pending = PlaybackBuffer::new(pcm_frame() + 4800);
    let mut played = 0_u64;
    let mut held = HeldKeys::default();
    let mut held_binding = None;
    let mut progress = Instant::now();
    tracing::info!(
        sound_id = id,
        duration_secs = ?source.decoder.duration_secs,
        speaker_gain_percent = ?settings.speaker_gain_percent,
        microphone_gain_percent = ?settings.microphone_gain_percent,
        repeat = settings.repeat,
        "Soundboard playback started"
    );
    let _ = events.send(Event::Started {
        revision,
        duration_secs: source.decoder.duration_secs,
    });
    // Queue the initial settings through the same output validation as live changes.
    let mut configure = Some(settings.clone());
    let reason = 'playback: loop {
        let mut now = pcm_frame();
        // Keep the EOF timestamp fixed while lagging outputs finish reading.
        let advance_to = if source.eof {
            now.min(pending.end())
        } else {
            now
        };
        played += pending.advance(advance_to);
        // Coalesce rapid requests before decoding or touching WASAPI. A stop
        // cancels pending replacements; the controller retains later requests.
        let replacement = match drain_commands(commands, &mut configure) {
            Ok(replacement) => replacement,
            Err(reason) => break 'playback reason,
        };
        if configure.is_some() {
            speaker.failed = false;
        }
        if let Some((next_id, next_revision)) = replacement {
            tracing::info!(
                sound_id = source.id,
                played_secs = played as f64 / f64::from(PCM_SAMPLE_RATE),
                reason = "replaced",
                "Soundboard sound finished"
            );
            microphone.stop();
            source = Source::open(next_id)?;
            revision = next_revision;
            now = pcm_frame();
            pending.reset(now + 4800);
            played = 0;
            progress = Instant::now();
            // Keep the render client running on its owning COM thread. Its short
            // queue drains while the new sound's timeline is prepared.
            tracing::info!(sound_id = next_id, duration_secs = ?source.decoder.duration_secs,
                speaker_stream_reused = speaker.monitor.is_some(), "Soundboard playback switched");
            let _ = events.send(Event::Started {
                revision,
                duration_secs: source.decoder.duration_secs,
            });
        }
        if let Some(mut next) = configure.take() {
            if next.microphone_gain_percent != settings.microphone_gain_percent {
                microphone.stop();
            }
            if !next.microphone_gain_percent.is_mute()
                && microphone.generation == 0
                && let Err(error) = microphone.start(&transport, &pending, now)
            {
                let error = Message::MicrophoneOutputFailed { error };
                if next.speaker_gain_percent.is_mute() {
                    return Err(anyhow::anyhow!(error));
                }
                tracing::warn!("{error}");
                let _ = events.send(Event::OutputError(revision, error));
                next.microphone_gain_percent = GainPercent::new(0);
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
            held = match settings.hold_hotkey {
                Some(key) => HeldKeys::press(key)?,
                None => HeldKeys::default(),
            };
            held_binding = settings.hold_hotkey;
        }
        if source.eof && settings.repeat {
            source.restart()?;
        }
        let fill_end = (now + PCM_DECODE_AHEAD_FRAMES).min(pending.end() + 2400);
        while !source.eof && pending.end() < fill_end {
            // Decode at most 50 ms per pass so stop/configuration stay responsive.
            match source.decoder.next_frame()? {
                Some(stereo) => {
                    pending.samples.push_back(stereo);
                    source.decoded += 1;
                }
                None if settings.repeat && source.decoded > 0 => source.restart()?,
                None => {
                    ensure!(
                        played > 0 || !pending.samples.is_empty(),
                        Text::NoPlayableAudio
                    );
                    source.eof = true;
                }
            }
        }
        microphone.write(&pending, settings.microphone_gain_percent.as_f32_gain());
        if let Err(error) = speaker.play(&settings, &pending) {
            if settings.microphone_gain_percent.is_mute() {
                return Err(error.user_context(Text::SpeakerPlaybackFailed));
            }
            tracing::warn!("Speaker playback failed; microphone output continues: {error:#}");
            let _ = events.send(Event::OutputError(
                revision,
                Message::SpeakerMonitorFailed {
                    error: Details::new(error),
                },
            ));
        }
        if source.eof && pending.end().saturating_add(PCM_MAX_CURSOR_DRIFT_FRAMES) <= now {
            break "completed";
        }
        if progress.elapsed() >= Duration::from_millis(100) {
            let mut elapsed_secs = played as f64 / f64::from(PCM_SAMPLE_RATE);
            if settings.repeat
                && let Some(duration_secs) = source.decoder.duration_secs.filter(|&d| d > 0.0)
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
        sound_id: source.id,
        reason,
        played_secs,
    })
}

use super::{decoder::SoundDecoder, sound::SoundId};
use crate::i18n::Text;
use crate::i18n::{Details, Message, UserContext};
use crate::{audio::monitor::Monitor, devices, hotkeys::HeldKeys};
use anyhow::ensure;
use std::{
    collections::VecDeque,
    sync::mpsc,
    time::{Duration, Instant},
};
use voice_box_shared::pcm_transport::{
    Clock, DECODE_AHEAD_FRAMES, Mapping, SAMPLE_RATE, transport_path,
};

/// Runtime output choices only; no GUI preferences or persistence behavior.
#[derive(Clone, Debug)]
pub(super) struct Config {
    pub microphone_device: Option<String>,
    pub speaker_device: Option<String>,
    pub microphone_volume: f32,
    pub speaker_volume: f32,
    pub include_microphone: bool,
    pub hold_hotkey: Option<crate::hotkeys::Hotkey>,
    pub repeat: bool,
}

pub(super) enum Command {
    Stop,
    Replace(SoundId, u64, Config),
    Configure(Config),
}
pub(super) enum Event {
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

pub(super) struct PlaybackOutcome {
    pub sound_id: SoundId,
    pub reason: &'static str,
    /// Playback time in seconds for audio frames whose scheduled time has elapsed.
    pub played_secs: f64,
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

pub(super) fn run(
    mut id: SoundId,
    mut settings: Config,
    mut revision: u64,
    commands: &mpsc::Receiver<Command>,
    events: &mpsc::Sender<Event>,
) -> anyhow::Result<PlaybackOutcome> {
    let mut decoder = SoundDecoder::new(Box::new(crate::database::audio::AudioSource::open(id)?))?;
    let mut duration_secs = decoder.duration_secs;
    let (guid, microphone_error) =
        match devices::soundboard_microphone_guid(settings.microphone_device.as_deref()) {
            Ok(guid) => (Some(guid), None),
            Err(error) => (
                None,
                Some(Message::MicrophoneOutputFailed {
                    error: Details::new(error),
                }),
            ),
        };
    let transport = transport_path(
        guid.as_deref()
            .unwrap_or("00000000-0000-0000-0000-000000000000"),
    )?;
    let clock = Clock::new()?;
    let mapping = Mapping::open(&transport, true).user_context(Text::CreatePlaybackBuffer)?;
    let mut generation = (clock.frame() & 0xffff).max(1);
    let mut base = clock.frame() + 4800;
    let mut pending = VecDeque::with_capacity(DECODE_AHEAD_FRAMES as usize);
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
        speaker_volume = settings.speaker_volume,
        microphone_volume = settings.microphone_volume,
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
        let mut now = clock.frame();
        while base < now && !pending.is_empty() {
            pending.pop_front();
            base += 1;
            played += 1;
        }
        if base < now {
            base = now;
        }
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
                played_secs = played as f64 / f64::from(SAMPLE_RATE),
                reason = "replaced",
                "Soundboard sound finished"
            );
            mapping.stop();
            decoder = SoundDecoder::new(Box::new(crate::database::audio::AudioSource::open(
                next_id,
            )?))?;
            id = next_id;
            revision = next_revision;
            duration_secs = decoder.duration_secs;
            generation = (generation + 1) & 0xffff;
            if generation == 0 {
                generation = 1;
            }
            now = clock.frame();
            base = now + 4800;
            pending.clear();
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
            if next.microphone_volume > 0.0
                && let Some(error) = &microphone_error
            {
                if next.speaker_volume == 0.0 {
                    return Err(anyhow::anyhow!(error.clone()));
                }
                tracing::warn!("{error}");
                let _ = events.send(Event::OutputError(revision, error.clone()));
            }
            if guid.is_none() {
                next.microphone_volume = 0.0;
                next.include_microphone = true;
            }
            settings = next;
        }
        if settings.microphone_volume == 0.0 && settings.speaker_volume == 0.0 {
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
        let fill_end = (now + DECODE_AHEAD_FRAMES).min(base + pending.len() as u64 + 2400);
        while !eof && base + (pending.len() as u64) < fill_end {
            // Decode at most 50 ms per pass so stop/configuration stay responsive.
            match decoder.next_frame()? {
                Some(stereo) => {
                    mapping.write(base + pending.len() as u64, generation, stereo);
                    pending.push_back(stereo);
                    decoded += 1;
                }
                None if settings.repeat && decoded > 0 => {
                    decoder = SoundDecoder::new(Box::new(
                        crate::database::audio::AudioSource::open(id)?,
                    ))?;
                    decoded = 0;
                }
                None => {
                    ensure!(played > 0 || !pending.is_empty(), Text::NoPlayableAudio);
                    eof = true;
                }
            }
        }
        mapping.publish(
            generation,
            base + pending.len() as u64,
            settings.microphone_volume,
            !settings.include_microphone,
            now,
        );
        if settings.speaker_volume > 0.0 && !monitor_failed {
            let result = (|| -> anyhow::Result<()> {
                if monitor.is_none() {
                    monitor = Some(Monitor::open(settings.speaker_device.as_deref())?);
                }
                // Opening a device or decoding a packet can take time. Read the
                // current clock so the render queue does not use a stale timeline.
                monitor.as_mut().unwrap().fill(clock.frame(), |frame| {
                    let stereo = mapping.read(frame, generation);
                    [
                        stereo[0] * settings.speaker_volume,
                        stereo[1] * settings.speaker_volume,
                    ]
                })
            })();
            if let Err(error) = result {
                if settings.microphone_volume == 0.0 {
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
        if eof && pending.is_empty() {
            break "completed";
        }
        if progress.elapsed() >= Duration::from_millis(100) {
            let mut elapsed_secs = played as f64 / f64::from(SAMPLE_RATE);
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
    let played_secs = played as f64 / f64::from(SAMPLE_RATE);
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

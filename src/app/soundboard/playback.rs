use super::{config::Options, decoder::SoundDecoder, library::SoundId};
use crate::app::i18n::Text;
use crate::app::i18n::{Details, Message, UserContext};
use crate::{
    app::{audio::monitor::Monitor, devices, hotkeys::HeldKeys},
    shared::pcm_transport::{Clock, DECODE_AHEAD_FRAMES, Mapping, SAMPLE_RATE, transport_path},
};
use anyhow::ensure;
use std::{
    collections::VecDeque,
    sync::mpsc,
    time::{Duration, Instant},
};

pub(super) enum StopReason {
    Requested,
    Replaced,
}

pub(super) enum Command {
    Stop(StopReason),
    Configure(Options),
}
pub(super) enum Event {
    Started(Option<f64>),
    Progress(f64),
    OutputError(Message),
    Finished(anyhow::Result<()>),
}

pub(super) struct PlaybackOutcome {
    pub reason: &'static str,
    /// Audio frames whose scheduled playback time has elapsed.
    pub played_secs: f64,
}

pub(super) fn run(
    id: SoundId,
    mut options: Options,
    commands: &mpsc::Receiver<Command>,
    events: &mpsc::Sender<Event>,
) -> anyhow::Result<PlaybackOutcome> {
    let mut decoder = SoundDecoder::new(Box::new(crate::app::database::AudioSource::open(id)?))?;
    let duration = decoder.duration;
    let (guid, microphone_error) = match devices::soundboard_microphone_guid(
        options
            .microphone_output_device
            .as_ref()
            .map(|device| device.id.as_str()),
    ) {
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
    let generation = (clock.frame() & 0xffff).max(1);
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
        duration_secs = ?duration,
        speaker_volume = options.speaker_output_volume,
        microphone_volume = options.microphone_output_volume,
        repeat = options.repeat,
        "Soundboard playback started"
    );
    let _ = events.send(Event::Started(duration));
    // Queue the initial options through the same output validation as live changes.
    let mut configure = Some(options.clone());
    let reason = 'playback: loop {
        let now = clock.frame();
        while base < now && !pending.is_empty() {
            pending.pop_front();
            base += 1;
            played += 1;
        }
        if base < now {
            base = now;
        }
        loop {
            match commands.try_recv() {
                Ok(Command::Stop(reason)) => {
                    break 'playback match reason {
                        StopReason::Requested => "stopped",
                        StopReason::Replaced => "replaced",
                    };
                }
                Err(mpsc::TryRecvError::Disconnected) => break 'playback "controller_disconnected",
                Ok(Command::Configure(next)) => {
                    configure = Some(next);
                    monitor_failed = false;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        if let Some(next) = configure.take() {
            if next.microphone_output_volume > 0.0
                && let Some(error) = &microphone_error
            {
                if next.speaker_output_volume == 0.0 {
                    return Err(anyhow::anyhow!(error.clone()));
                }
                tracing::warn!("{error}");
                let _ = events.send(Event::OutputError(error.clone()));
            }
            options = next.for_output(guid.is_some());
        }
        if options.microphone_output_volume == 0.0 && options.speaker_output_volume == 0.0 {
            break "outputs_muted";
        }
        if options.auto_hold_hotkey != held_binding {
            drop(held);
            held = if let Some(key) = options.auto_hold_hotkey {
                HeldKeys::press(key)?
            } else {
                HeldKeys::default()
            };
            held_binding = options.auto_hold_hotkey;
        }
        if eof && options.repeat {
            decoder = SoundDecoder::new(Box::new(crate::app::database::AudioSource::open(id)?))?;
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
                None if options.repeat && decoded > 0 => {
                    decoder =
                        SoundDecoder::new(Box::new(crate::app::database::AudioSource::open(id)?))?;
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
            options.microphone_output_volume,
            !options.include_microphone_audio,
            now,
        );
        if options.speaker_output_volume > 0.0 && !monitor_failed {
            let result = (|| -> anyhow::Result<()> {
                if monitor.is_none() {
                    monitor = Some(Monitor::open(
                        options
                            .speaker_output_device
                            .as_ref()
                            .map(|device| device.id.as_str()),
                    )?);
                }
                monitor.as_mut().unwrap().fill(now, |frame| {
                    let stereo = mapping.read(frame, generation);
                    [
                        stereo[0] * options.speaker_output_volume,
                        stereo[1] * options.speaker_output_volume,
                    ]
                })
            })();
            if let Err(error) = result {
                if options.microphone_output_volume == 0.0 {
                    return Err(error.user_context(Text::SpeakerPlaybackFailed));
                }
                monitor = None;
                monitor_failed = true;
                tracing::warn!("Speaker playback failed; microphone output continues: {error:#}");
                let _ = events.send(Event::OutputError(Message::SpeakerMonitorFailed {
                    error: Details::new(error),
                }));
            }
        } else {
            monitor = None;
        }
        if eof && pending.is_empty() {
            break "completed";
        }
        if progress.elapsed() >= Duration::from_millis(100) {
            let mut elapsed = played as f64 / f64::from(SAMPLE_RATE);
            if options.repeat
                && let Some(duration) = duration.filter(|&d| d > 0.0)
            {
                elapsed %= duration;
            }
            let _ = events.send(Event::Progress(elapsed));
            progress = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let played_secs = played as f64 / f64::from(SAMPLE_RATE);
    let _ = events.send(Event::Progress(played_secs));
    if reason == "completed" {
        std::thread::sleep(Duration::from_millis(30));
    }
    Ok(PlaybackOutcome {
        reason,
        played_secs,
    })
}

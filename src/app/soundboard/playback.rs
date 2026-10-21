use super::{config::Options, decoder::MusicDecoder};
use crate::{
    app::{audio::monitor::Monitor, hotkeys::HeldKeys},
    shared::pcm_transport::{Clock, LEAD, Mapping, RATE},
};
use anyhow::{Context, ensure};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

pub(super) enum Command {
    Stop,
    Configure(Options),
}
pub(super) enum Event {
    Started(Option<f64>),
    Progress(f64),
    MonitorError(String),
    Finished(anyhow::Result<()>),
}

pub(super) fn run(
    path: PathBuf,
    transport: PathBuf,
    mut options: Options,
    commands: &mpsc::Receiver<Command>,
    events: &mpsc::Sender<Event>,
) -> anyhow::Result<()> {
    let mut decoder = MusicDecoder::open(&path)?;
    let duration = decoder.duration;
    let clock = Clock::new()?;
    let mapping = Mapping::open(&transport, true).context("创建播放传输缓冲区失败")?;
    let generation = (clock.frame() & 0xffff).max(1);
    let mut base = clock.frame() + 4800;
    let mut pending = VecDeque::with_capacity(LEAD as usize);
    let mut played = 0_u64;
    let mut decoded = 0_u64;
    let mut eof = false;
    let mut monitor: Option<Monitor> = None;
    let mut monitor_failed = false;
    let mut held = HeldKeys::default();
    let mut held_binding = None;
    let mut progress = Instant::now();
    let _ = events.send(Event::Started(duration));
    loop {
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
                Ok(Command::Stop) | Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                Ok(Command::Configure(next)) => {
                    options = next;
                    monitor_failed = false;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        if options.send_volume == 0.0 && options.monitor_volume == 0.0 {
            return Ok(());
        }
        if options.hold != held_binding {
            drop(held);
            held = if let Some(key) = options.hold {
                HeldKeys::press(key)?
            } else {
                HeldKeys::default()
            };
            held_binding = options.hold;
        }
        if eof && options.repeat {
            decoder = MusicDecoder::open(&path)?;
            decoded = 0;
            eof = false;
        }
        let fill_end = (now + LEAD).min(base + pending.len() as u64 + 2400);
        while !eof && base + (pending.len() as u64) < fill_end {
            // Decode at most 50 ms per pass so stop/configuration stay responsive.
            match decoder.next()? {
                Some(stereo) => {
                    mapping.write(base + pending.len() as u64, generation, stereo);
                    pending.push_back(stereo);
                    decoded += 1;
                }
                None if options.repeat && decoded > 0 => {
                    decoder = MusicDecoder::open(&path)?;
                    decoded = 0;
                }
                None => {
                    ensure!(
                        played > 0 || !pending.is_empty(),
                        "音频文件没有可播放的数据"
                    );
                    eof = true;
                }
            }
        }
        mapping.publish(
            generation,
            base + pending.len() as u64,
            options.send_volume,
            !options.microphone,
            now,
        );
        if options.monitor_volume > 0.0 && !monitor_failed {
            let result = (|| -> anyhow::Result<()> {
                if monitor.is_none() {
                    monitor = Some(Monitor::open()?);
                }
                monitor.as_mut().unwrap().fill(now, |frame| {
                    let stereo = mapping.read(frame, generation);
                    [
                        stereo[0] * options.monitor_volume,
                        stereo[1] * options.monitor_volume,
                    ]
                })
            })();
            if let Err(error) = result {
                if options.send_volume == 0.0 {
                    return Err(error.context("扬声器播放失败"));
                }
                monitor = None;
                monitor_failed = true;
                let _ = events.send(Event::MonitorError(format!(
                    "扬声器播放失败，麦克风发送继续：{error:#}"
                )));
            }
        } else {
            monitor = None;
        }
        if eof && pending.is_empty() {
            break;
        }
        if progress.elapsed() >= Duration::from_millis(100) {
            let mut elapsed = played as f64 / f64::from(RATE);
            if options.repeat
                && let Some(duration) = duration.filter(|&d| d > 0.0)
            {
                elapsed %= duration;
            }
            let _ = events.send(Event::Progress(elapsed));
            progress = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = events.send(Event::Progress(played as f64 / f64::from(RATE)));
    std::thread::sleep(Duration::from_millis(30));
    Ok(())
}

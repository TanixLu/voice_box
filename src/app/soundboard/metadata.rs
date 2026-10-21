use super::{decoder::SoundDecoder, library::Library};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

struct DurationJob {
    receiver: mpsc::Receiver<(PathBuf, Result<f64, String>)>,
    stop: Arc<AtomicBool>,
}

impl Drop for DurationJob {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub(super) struct DurationReader {
    job: Option<DurationJob>,
    scheduled_paths: HashSet<PathBuf>,
}

impl DurationReader {
    pub(super) fn invalidate(&mut self, path: &Path) {
        self.scheduled_paths.remove(path);
    }
    pub(super) fn poll(
        &mut self,
        library: &mut Library,
        wake: impl Fn() + Send + 'static,
    ) -> Vec<String> {
        let mut errors = Vec::new();
        if let Some(job) = &self.job {
            loop {
                match job.receiver.try_recv() {
                    Ok((path, result)) => {
                        if let Some(sound) =
                            library.sounds.iter_mut().find(|sound| sound.path == path)
                        {
                            match result {
                                Ok(duration) => {
                                    sound.duration = Some(duration);
                                    sound.duration_error = None;
                                }
                                Err(error) => sound.duration_error = Some(error),
                            }
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.job = None;
                        break;
                    }
                }
            }
        }
        if self.job.is_some() {
            return errors;
        }
        let paths: Vec<_> = library
            .sounds
            .iter()
            .filter(|sound| !self.scheduled_paths.contains(&sound.path))
            .map(|sound| sound.path.clone())
            .collect();
        if paths.is_empty() {
            return errors;
        }
        let (events, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_paths = paths.clone();
        match std::thread::Builder::new()
            .name("VoiceBoxSoundMetadata".into())
            .spawn(move || {
                for path in worker_paths {
                    if worker_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let result = SoundDecoder::probe_duration(&path, &worker_stop)
                        .map_err(|error| format!("{error:#}"));
                    if events.send((path, result)).is_err() {
                        break;
                    }
                    wake();
                }
            }) {
            Ok(_) => {
                self.scheduled_paths.extend(paths);
                self.job = Some(DurationJob { receiver, stop });
            }
            Err(error) => errors.push(format!("无法读取音效时长：{error}")),
        }
        errors
    }
}

//! Bounded application logs; APO diagnostics remain ETW-only.
use crate::i18n::{Details, Message};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tracing_subscriber::fmt::MakeWriter;

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 64 * 1024;
const LOG_FILE: &str = "voice_box.log";

#[derive(Clone)]
struct RollingLog {
    directory: PathBuf,
    mutex: Arc<Mutex<()>>,
}

impl RollingLog {
    fn append(&self, bytes: &[u8]) -> io::Result<()> {
        let _local = self.mutex.lock().unwrap_or_else(|error| error.into_inner());
        let path = self.directory.join(LOG_FILE);
        let size = match fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        if size + bytes.len() as u64 > MAX_FILE_BYTES {
            let backup = self.directory.join(format!("{LOG_FILE}.1"));
            match fs::remove_file(&backup) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            if size > 0 {
                fs::rename(&path, backup)?;
            }
        }
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.write_all(bytes)?;
        Ok(())
    }
}

struct EventWriter {
    log: RollingLog,
    bytes: Vec<u8>,
}
impl Write for EventWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let remaining = MAX_EVENT_BYTES.saturating_sub(self.bytes.len());
        self.bytes
            .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Drop for EventWriter {
    fn drop(&mut self) {
        if !self.bytes.is_empty() {
            if self.bytes.last() != Some(&b'\n') {
                self.bytes.push(b'\n');
            }
            let _ = self.log.append(&self.bytes);
        }
    }
}
impl<'a> MakeWriter<'a> for RollingLog {
    type Writer = EventWriter;
    fn make_writer(&'a self) -> Self::Writer {
        EventWriter {
            log: self.clone(),
            bytes: Vec::new(),
        }
    }
}

pub fn init() -> anyhow::Result<()> {
    let log = RollingLog {
        directory: voice_box_shared::paths::data_dir()?,
        mutex: Arc::new(Mutex::new(())),
    };
    // Detect inability to open the destination before installing the subscriber.
    log.append(&[])?;
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(log)
        .with_target(true)
        .try_init()
        .map_err(|error| {
            anyhow::anyhow!(Message::LoggingFailed {
                error: Details::new(anyhow::Error::from_boxed(error))
            })
        })?;
    std::panic::set_hook(Box::new(|panic| {
        tracing::error!("Application panic: {panic}");
    }));
    tracing::info!(
        pid = std::process::id(),
        version = env!("CARGO_PKG_VERSION"),
        "VoiceBox started"
    );
    Ok(())
}

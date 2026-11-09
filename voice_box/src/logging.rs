//! Bounded application logs; APO diagnostics remain ETW-only.

use std::{
    fs,
    io::{self, Write},
    sync::{Arc, Mutex},
};

use crate::{
    i18n::{Details, Message},
    paths::{WriteMode, log_path, rotated_log_path, write_file_with_parents},
};
use tracing_subscriber::fmt::MakeWriter;

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 64 * 1024;

#[derive(Default, Clone)]
struct RollingLog {
    mutex: Arc<Mutex<()>>,
}

impl RollingLog {
    fn append(&self, bytes: &[u8]) -> io::Result<()> {
        let _local = self.mutex.lock().unwrap_or_else(|error| error.into_inner());
        let size = match fs::metadata(log_path()) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        if size + bytes.len() as u64 > MAX_FILE_BYTES {
            match fs::remove_file(rotated_log_path()) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            if size > 0 {
                fs::rename(log_path(), rotated_log_path())?;
            }
        }
        write_file_with_parents(log_path(), WriteMode::Append, |file| file.write_all(bytes))
    }
}

struct EventWriter {
    log: RollingLog,
    bytes: Vec<u8>,
}
impl Write for EventWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let remaining = (MAX_EVENT_BYTES - 1).saturating_sub(self.bytes.len());
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
                if let Err(error) = std::str::from_utf8(&self.bytes) {
                    self.bytes.truncate(error.valid_up_to());
                }
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
    let log = RollingLog::default();
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
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        tracing::error!("Application panic: {panic}");
        previous_hook(panic);
    }));
    tracing::info!(
        pid = std::process::id(),
        version = env!("CARGO_PKG_VERSION"),
        "VoiceBox started"
    );
    Ok(())
}

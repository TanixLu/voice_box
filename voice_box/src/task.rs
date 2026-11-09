//! Background work that reports when it starts and delivers exactly one result.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Instant,
};

use crate::i18n::{Details, Message, Text};

/// Tells the owner of a task to look at it again, such as by repainting the UI.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

enum Event<T> {
    Started(Instant),
    Finished(anyhow::Result<T>),
}

/// Dropping a task asks it to cancel; it never blocks the caller.
pub struct Task<T> {
    events: Option<mpsc::Receiver<Event<T>>>,
    spawn_error: Option<anyhow::Error>,
    cancel: Arc<AtomicBool>,
    started: Option<Instant>,
    thread: Option<JoinHandle<()>>,
}

impl<T: Send + 'static> Task<T> {
    /// `work` calls `started` once it is really running, and should return early
    /// when the flag it receives is set.
    pub fn spawn(
        name: &'static str,
        wake: &Wake,
        work: impl FnOnce(&AtomicBool, &dyn Fn()) -> anyhow::Result<T> + Send + 'static,
    ) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (sender, events) = mpsc::channel();
        let wake = Arc::clone(wake);
        let spawned = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let begin = Instant::now();
                tracing::info!(task = name, "Task started");
                let started = || {
                    let _ = sender.send(Event::Started(Instant::now()));
                    wake();
                };
                let result = work(&worker_cancel, &started);
                let elapsed_ms = begin.elapsed().as_millis();
                match &result {
                    Ok(_) => tracing::info!(task = name, elapsed_ms, "Task completed"),
                    Err(error) => {
                        tracing::error!(task = name, elapsed_ms, "Task failed: {error:#}")
                    }
                }
                let _ = sender.send(Event::Finished(result));
                wake();
            });
        let (events, spawn_error, thread) = match spawned {
            Ok(thread) => (Some(events), None, Some(thread)),
            Err(error) => {
                tracing::error!(task = name, "Cannot start task: {error}");
                let error = anyhow::anyhow!(Message::CannotStartOperation {
                    error: Details::new(error),
                });
                (None, Some(error), None)
            }
        };
        Self {
            events,
            spawn_error,
            cancel,
            started: None,
            thread,
        }
    }

    /// For work that can be neither cancelled nor observed starting.
    pub fn run(
        name: &'static str,
        wake: &Wake,
        work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
    ) -> Self {
        Self::spawn(name, wake, move |_, _| work())
    }

    /// Returns the result once; the task is spent afterwards.
    pub fn poll(&mut self) -> Option<anyhow::Result<T>> {
        if let Some(error) = self.spawn_error.take() {
            return Some(Err(error));
        }
        let events = self.events.as_ref()?;
        loop {
            match events.try_recv() {
                Ok(Event::Started(at)) => self.started = Some(at),
                Ok(Event::Finished(result)) => {
                    self.events = None;
                    return Some(result);
                }
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.events = None;
                    return Some(Err(anyhow::anyhow!(Text::OperationIncomplete)));
                }
            }
        }
    }

    /// Waits for the worker thread to exit. On the UI thread, call it only after `poll`
    /// returned the result, when the thread is already exiting.
    pub fn join(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl<T> Task<T> {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn cancelling(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    /// When `work` reported that it started, as of the last poll.
    pub fn started_at(&self) -> Option<Instant> {
        self.started
    }
}

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Polls the task in `slot` and empties the slot once it has finished.
pub fn finished<T: Send + 'static>(slot: &mut Option<Task<T>>) -> Option<anyhow::Result<T>> {
    let result = slot.as_mut()?.poll()?;
    *slot = None;
    Some(result)
}

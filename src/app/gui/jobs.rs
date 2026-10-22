use super::VoiceBoxApp;
use crate::app::i18n::Text;
use crate::app::i18n::{Details, Message};
use crate::{
    app::devices::{Microphone, PluginOperationResult},
    shared::settings::Settings,
};
use std::sync::mpsc;

pub(super) enum JobResult {
    Uninstall(anyhow::Result<()>),
    Microphones(anyhow::Result<(Vec<Microphone>, usize)>),
    Plugin(PluginOperationResult),
    Saved {
        settings: Settings,
        result: anyhow::Result<()>,
    },
}

#[derive(Default)]
pub(super) struct Jobs {
    receiver: Option<mpsc::Receiver<JobResult>>,
    pub(super) status: Message,
    pub(super) failed: bool,
}

impl Jobs {
    pub(super) fn busy(&self) -> bool {
        self.receiver.is_some()
    }
    pub(super) fn begin(
        &mut self,
        ctx: &egui::Context,
        status: Text,
        operation: impl FnOnce() -> JobResult + Send + 'static,
    ) -> bool {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        self.receiver = Some(receiver);
        self.status = status.into();
        self.failed = false;
        ctx.request_repaint();
        let operation_name = status.in_language(crate::app::i18n::Language::English);
        if let Err(error) = std::thread::Builder::new()
            .name("VoiceBoxOperation".into())
            .spawn(move || {
                let started = std::time::Instant::now();
                tracing::info!(operation = %operation_name, "Background operation started");
                let result = operation();
                match &result {
                    JobResult::Uninstall(Err(error))
                    | JobResult::Microphones(Err(error))
                    | JobResult::Plugin(PluginOperationResult {
                        result: Err(error), ..
                    })
                    | JobResult::Saved {
                        result: Err(error), ..
                    } => {
                        tracing::error!(
                            operation = %operation_name,
                            elapsed_ms = started.elapsed().as_millis(),
                            "Background operation failed: {error:#}"
                        );
                    }
                    _ => tracing::info!(
                        operation = %operation_name,
                        elapsed_ms = started.elapsed().as_millis(),
                        "Background operation completed"
                    ),
                }
                let _ = sender.send(result);
                ctx.request_repaint();
            })
        {
            tracing::error!(
                operation = %status,
                "Failed to spawn background operation: {error}"
            );
            self.receiver = None;
            self.status = Message::CannotStartOperation {
                error: Details::new(error),
            };
            self.failed = true;
            return false;
        }
        true
    }
}

impl VoiceBoxApp {
    pub(super) fn poll_job(&mut self) {
        let Some(receiver) = &self.jobs.receiver else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                tracing::error!(
                    operation = %self.jobs.status,
                    "Background operation exited without a result"
                );
                self.jobs.receiver = None;
                self.uninstalling = false;
                self.jobs.status = Text::OperationIncomplete.into();
                self.jobs.failed = true;
                return;
            }
        };
        self.jobs.receiver = None;
        match result {
            JobResult::Uninstall(Ok(())) => self.uninstall_requested = true,
            JobResult::Uninstall(Err(error)) => {
                self.uninstalling = false;
                self.jobs.status = Message::UninstallFailed {
                    error: Details::new(error),
                };
                self.jobs.failed = true;
            }
            JobResult::Microphones(Ok((microphones, selected))) => {
                self.microphones.replace_devices(microphones, selected);
                self.jobs.status.clear();
            }
            JobResult::Plugin(PluginOperationResult { result, statuses }) => {
                self.microphones.apply_statuses(statuses);
                self.jobs.status.clear();
                if let Err(error) = result {
                    self.jobs.status = Message::OperationFailed {
                        error: Details::new(error),
                    };
                    self.jobs.failed = true;
                }
            }
            JobResult::Saved {
                settings,
                result: Ok(()),
            } => {
                self.voice.mark_saved(settings);
                self.jobs.status.clear();
            }
            JobResult::Microphones(Err(error))
            | JobResult::Saved {
                result: Err(error), ..
            } => {
                self.jobs.status = Message::OperationFailed {
                    error: Details::new(error),
                };
                self.jobs.failed = true;
            }
        }
    }
}

use super::VoiceBoxApp;
use crate::app::i18n::Text;
use crate::app::i18n::{Details, Message};
use crate::{
    app::{
        devices::{Microphone, PluginOperationResult, Speaker},
        settings::OutputDevice,
    },
    shared::apo_cfg::ApoConfig,
};
use std::sync::mpsc;

pub(super) enum JobResult {
    Exported(anyhow::Result<()>),
    Uninstall(anyhow::Result<()>),
    Microphones(anyhow::Result<(Vec<Microphone>, usize)>),
    OutputDevices(anyhow::Result<(Vec<Microphone>, Vec<Speaker>)>),
    Plugin(PluginOperationResult),
    Saved {
        config: ApoConfig,
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
    pub(super) fn exporting(&self) -> bool {
        self.busy() && matches!(self.status, Message::Text(Text::ExportSounds))
    }
    pub(super) fn begin(
        &mut self,
        ctx: &egui::Context,
        status: Text,
        operation: impl FnOnce() -> JobResult + Send + 'static,
    ) -> bool {
        if self.busy() {
            return false;
        }
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
                    | JobResult::Exported(Err(error))
                    | JobResult::Microphones(Err(error))
                    | JobResult::OutputDevices(Err(error))
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
                if self.jobs.exporting() {
                    self.soundboard.model.file_error = Some((
                        Text::SoundExportFailedTitle,
                        Text::OperationIncomplete.into(),
                    ));
                    self.jobs.receiver = None;
                    self.jobs.status.clear();
                    return;
                }
                self.jobs.receiver = None;
                self.uninstalling = false;
                self.jobs.status = Text::OperationIncomplete.into();
                self.jobs.failed = true;
                return;
            }
        };
        self.jobs.receiver = None;
        match result {
            JobResult::Exported(result) => {
                self.jobs.status.clear();
                if let Err(error) = result {
                    self.soundboard.model.file_error =
                        Some((Text::SoundExportFailedTitle, Message::error(error)));
                }
            }
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
            JobResult::OutputDevices(Ok((microphones, speakers))) => {
                self.soundboard.microphones = microphones
                    .into_iter()
                    .map(|mic| {
                        let name = super::microphone::display_name(&mic);
                        (
                            OutputDevice {
                                id: mic.device_id,
                                name: mic.name,
                            },
                            name,
                        )
                    })
                    .collect();
                self.soundboard.speakers = speakers
                    .into_iter()
                    .map(|speaker| {
                        let device = OutputDevice {
                            id: speaker.device_id,
                            name: speaker.name.clone(),
                        };
                        let name = if speaker.is_default {
                            Message::DefaultDevice { name: speaker.name }
                                .text()
                                .into_owned()
                        } else {
                            speaker.name
                        };
                        (device, name)
                    })
                    .collect();
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
                config,
                result: Ok(()),
            } => {
                self.voice.mark_saved(config);
                self.jobs.status.clear();
            }
            JobResult::Microphones(Err(error))
            | JobResult::OutputDevices(Err(error))
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

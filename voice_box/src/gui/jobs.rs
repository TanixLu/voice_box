use std::sync::mpsc;

use voice_box_shared::apo_cfg::ApoCfg;

use crate::{
    devices::{Microphone, PluginOperationResult, Speaker},
    gui::VoiceBoxApp,
    i18n::{self, Details, Message, Text},
    settings::Settings,
    soundboard::library::Library,
};

pub enum JobResult {
    LibrarySaved(anyhow::Result<Library>),
    SettingsSaved {
        settings: Settings,
        result: anyhow::Result<()>,
    },
    Exported(anyhow::Result<()>),
    Uninstall(anyhow::Result<()>),
    Startup {
        result: anyhow::Result<bool>,
        notice: Option<Text>,
    },
    Microphones(anyhow::Result<(Vec<Microphone>, usize)>),
    OutputDevices(anyhow::Result<(Vec<Microphone>, Vec<Speaker>)>),
    Plugin(PluginOperationResult),
    Saved {
        config: ApoCfg,
        result: anyhow::Result<()>,
    },
}

#[derive(Default)]
pub struct Jobs {
    receiver: Option<mpsc::Receiver<JobResult>>,
    pub status: Message,
    pub failed: bool,
}

impl Jobs {
    pub fn save_settings(&mut self, ctx: &egui::Context, settings: Settings) -> bool {
        self.begin(ctx, Text::Saving, move || JobResult::SettingsSaved {
            result: settings.save(),
            settings,
        })
    }

    pub fn busy(&self) -> bool {
        self.receiver.is_some()
    }
    pub fn exporting(&self) -> bool {
        self.busy() && matches!(self.status, Message::Text(Text::ExportSounds))
    }
    pub fn begin(
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
        let operation_name = status.in_language(crate::i18n::Language::English);
        if let Err(error) = std::thread::Builder::new()
            .name("VoiceBoxOperation".into())
            .spawn(move || {
                let started = std::time::Instant::now();
                tracing::info!(operation = %operation_name, "Background operation started");
                let result = operation();
                match &result {
                    JobResult::LibrarySaved(Err(error))
                    | JobResult::SettingsSaved {
                        result: Err(error), ..
                    }
                    | JobResult::Uninstall(Err(error))
                    | JobResult::Startup {
                        result: Err(error), ..
                    }
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
    pub fn poll_job(&mut self, ctx: &egui::Context) {
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
                if matches!(self.jobs.status, Message::Text(Text::SaveSoundLibrary)) {
                    self.soundboard
                        .finish_edit(Err(anyhow::anyhow!(Text::OperationIncomplete)));
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
            JobResult::LibrarySaved(result) => {
                self.soundboard.finish_edit(result);
                self.jobs.status.clear();
            }
            JobResult::SettingsSaved { settings, result } => {
                let startup_changed =
                    settings.startup_enabled != self.soundboard.model.settings.startup_enabled;
                match result {
                    Ok(()) => {
                        ctx.set_theme(settings.theme_preference);
                        i18n::set_language(settings.language_preference.resolve());
                        self.soundboard.model.apply_settings(settings);
                        self.jobs.status.clear();
                        if startup_changed {
                            let notice = self
                                .soundboard
                                .model
                                .settings
                                .startup_enabled
                                .then_some(Text::StartupRegistered);
                            self.synchronize_startup(ctx, notice);
                        }
                    }
                    Err(error) => {
                        self.jobs.status = Message::error(error);
                        self.jobs.failed = true;
                        if startup_changed {
                            self.startup_message = Some(self.jobs.status.clone());
                        }
                    }
                }
            }
            JobResult::Startup { result, notice } => {
                self.jobs.status.clear();
                self.startup_synchronized = result.is_ok();
                self.startup_message = match result {
                    Ok(changed) if changed || notice.is_some() => {
                        Some(notice.unwrap_or(Text::StartupReRegistered).into())
                    }
                    Ok(_) => None,
                    Err(error) => Some(Message::error(error)),
                };
                if self.startup_message.is_some() && !self.start_in_tray {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                }
            }
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
                self.soundboard.set_output_devices(microphones, speakers);
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

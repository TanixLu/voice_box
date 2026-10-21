use super::VoiceBoxApp;
use crate::{
    app::devices::{Microphone, PluginOperation},
    shared::settings::Settings,
};
use std::sync::mpsc;

pub(super) enum JobResult {
    Uninstall(anyhow::Result<()>),
    Microphones(anyhow::Result<(Vec<Microphone>, usize)>),
    Plugin(PluginOperation),
    Saved {
        settings: Settings,
        result: anyhow::Result<()>,
    },
}

#[derive(Default)]
pub(super) struct Jobs {
    receiver: Option<mpsc::Receiver<JobResult>>,
    pub(super) status: String,
    pub(super) failed: bool,
}

impl Jobs {
    pub(super) fn busy(&self) -> bool {
        self.receiver.is_some()
    }
    pub(super) fn begin(
        &mut self,
        ctx: &egui::Context,
        status: &str,
        operation: impl FnOnce() -> JobResult + Send + 'static,
    ) -> bool {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        self.receiver = Some(receiver);
        self.status = status.into();
        self.failed = false;
        ctx.request_repaint();
        if let Err(error) = std::thread::Builder::new()
            .name("VoiceBoxOperation".into())
            .spawn(move || {
                let result = operation();
                match &result {
                    JobResult::Uninstall(Err(error))
                    | JobResult::Microphones(Err(error))
                    | JobResult::Plugin(PluginOperation {
                        result: Err(error), ..
                    })
                    | JobResult::Saved {
                        result: Err(error), ..
                    } => {
                        tracing::error!("Background operation failed: {error:#}");
                    }
                    _ => {}
                }
                let _ = sender.send(result);
                ctx.request_repaint();
            })
        {
            self.receiver = None;
            self.status = format!("无法开始操作：{error}");
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
                self.jobs.receiver = None;
                self.uninstalling = false;
                self.jobs.status = "操作未完成，请重试。".into();
                self.jobs.failed = true;
                return;
            }
        };
        self.jobs.receiver = None;
        match result {
            JobResult::Uninstall(Ok(())) => self.uninstall_requested = true,
            JobResult::Uninstall(Err(error)) => {
                self.uninstalling = false;
                self.jobs.status = format!("卸载未完成：{error:#}");
                self.jobs.failed = true;
            }
            JobResult::Microphones(Ok((microphones, selected))) => {
                self.microphones.replace_devices(microphones, selected);
                self.jobs.status.clear();
            }
            JobResult::Plugin(PluginOperation { result, statuses }) => {
                self.microphones.apply_statuses(statuses);
                self.jobs.status.clear();
                if let Err(error) = result {
                    self.jobs.status = format!("操作失败：{error:#}");
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
                self.jobs.status = format!("操作失败：{error:#}");
                self.jobs.failed = true;
            }
        }
    }
}

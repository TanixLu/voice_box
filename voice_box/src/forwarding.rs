//! Microphone forwarding. One worker owns the graph; the UI sends routing intents.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, Thread},
    time::Duration,
};

use anyhow::{Context, bail, ensure};
use windows::{
    Devices::Enumeration::DeviceInformation,
    Foundation::{IPropertyValue, TypedEventHandler},
    Media::{
        Audio::{
            AudioDeviceNodeCreationStatus, AudioGraph, AudioGraphCreationStatus,
            AudioGraphSettings, AudioGraphUnrecoverableError,
            AudioGraphUnrecoverableErrorOccurredEventArgs, QuantumSizeSelectionMode,
        },
        AudioProcessing,
        Capture::MediaCategory,
        Devices::MediaDevice,
        Render::AudioRenderCategory,
    },
    Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
    core::{HSTRING, Interface},
};

use crate::{
    devices::endpoint_guid,
    i18n::{Details, Message, Text, UserContext},
    task::Wake,
};

#[derive(Clone, Default, PartialEq, Eq)]
pub enum Target {
    #[default]
    Off,
    DefaultSpeaker,
    Speaker(String),
}

struct Request {
    source: Option<String>,
    target: Target,
    cancelled: AtomicBool,
}

impl Request {
    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn enabled(&self) -> bool {
        self.source.is_some() && self.target != Target::Off
    }
}

#[derive(Clone, Default)]
pub enum Status {
    #[default]
    Idle,
    Opening,
    Active,
    Retrying(Message),
    Failed(Message),
}

struct Report(Arc<Request>, Status);

pub struct Forwarding {
    request: Arc<Request>,
    commands: Sender<Arc<Request>>,
    reports: Receiver<Report>,
    worker: Option<Thread>,
    status: Status,
}

impl Forwarding {
    pub fn new(wake: Wake) -> Self {
        let (commands, receiver) = mpsc::channel();
        let (sender, reports) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("VoiceBoxMicrophoneForwarding".into())
            .spawn(move || run(receiver, sender, wake));
        let (worker, status) = match spawned {
            Ok(handle) => (Some(handle.thread().clone()), Status::Idle),
            Err(error) => (
                None,
                Status::Failed(Message::StartOperationFailed {
                    error: Details::new(error),
                }),
            ),
        };
        Self {
            request: Arc::new(Request {
                source: None,
                target: Target::Off,
                cancelled: AtomicBool::new(false),
            }),
            commands,
            reports,
            worker,
            status,
        }
    }

    pub fn configure(&mut self, source: Option<&str>, target: Target) {
        if self.request.source.as_deref() == source && self.request.target == target {
            return;
        }
        self.request.cancelled.store(true, Ordering::Release);
        self.request = Arc::new(Request {
            source: source.map(str::to_owned),
            target,
            cancelled: AtomicBool::new(false),
        });
        if self.worker.is_none() {
            return;
        }
        self.status = if self.request.enabled() {
            Status::Opening
        } else {
            Status::Idle
        };
        if self.commands.send(self.request.clone()).is_err() {
            self.status = Status::Failed(Message::error(Text::OperationIncomplete));
        }
        if let Some(worker) = &self.worker {
            worker.unpark();
        }
    }

    pub fn poll(&mut self, source: Option<&str>) {
        self.configure(source, self.target());
        while let Ok(Report(request, status)) = self.reports.try_recv() {
            if Arc::ptr_eq(&request, &self.request) {
                self.status = status;
            }
        }
    }

    pub fn target(&self) -> Target {
        self.request.target.clone()
    }

    pub fn status(&self) -> Status {
        self.status.clone()
    }
}

impl Drop for Forwarding {
    fn drop(&mut self) {
        self.request.cancelled.store(true, Ordering::Release);
        if let Some(worker) = &self.worker {
            worker.unpark();
        }
    }
}

fn run(commands: Receiver<Arc<Request>>, reports: Sender<Report>, wake: Wake) {
    while let Ok(mut request) = commands.recv() {
        // Skip intermediate selections before opening any devices.
        while let Ok(next) = commands.try_recv() {
            request = next;
        }
        if !request.enabled() || request.cancelled() {
            continue;
        }
        let report = |status| {
            if !request.cancelled() {
                let _ = reports.send(Report(request.clone(), status));
                wake();
            }
        };
        while !request.cancelled() {
            let target = match &request.target {
                Target::Off => unreachable!(),
                Target::DefaultSpeaker => None,
                Target::Speaker(id) => Some(id.as_str()),
            };
            let result = run_graph(
                request.source.as_deref().unwrap(),
                target,
                &|| request.cancelled(),
                &|| report(Status::Active),
            );
            if let Err(error) = result {
                tracing::warn!("Microphone forwarding failed: {error:#}");
                report(Status::Retrying(Message::error(error)));
                if !request.cancelled() {
                    thread::park_timeout(Duration::from_secs(1));
                }
            }
        }
    }
}

struct Runtime;

impl Runtime {
    fn new() -> windows::core::Result<Self> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
        Ok(Self)
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}

struct Graph {
    graph: AudioGraph,
    error_token: Option<i64>,
}

impl Drop for Graph {
    fn drop(&mut self) {
        if let Some(token) = self.error_token {
            let _ = self.graph.RemoveUnrecoverableErrorOccurred(token);
        }
        let _ = self.graph.Stop();
        if let Err(error) = self.graph.Close() {
            tracing::warn!("Cannot close forwarding graph: {error}");
        }
    }
}

/// Match Core Audio endpoint IDs to WinRT interfaces using PKEY_AudioEndpoint_GUID.
fn device(endpoint: &str, selector: HSTRING) -> anyhow::Result<DeviceInformation> {
    let key = HSTRING::from("{1DA5D803-D492-4EDD-8C23-E0C0FFEE7F0E} 4");
    let properties = windows_collections::IIterable::<HSTRING>::from(vec![key.clone()]);
    let devices =
        DeviceInformation::FindAllAsyncAqsFilterAndAdditionalProperties(&selector, &properties)?
            .join()?;
    for device in devices {
        let Ok(guid) = device
            .Properties()
            .and_then(|properties| properties.Lookup(&key))
            .and_then(|value| value.cast::<IPropertyValue>())
            .and_then(|value| value.GetString())
        else {
            continue;
        };
        if guid
            .to_string()
            .eq_ignore_ascii_case(endpoint_guid(endpoint))
        {
            return Ok(device);
        }
    }
    bail!(Text::DeviceUnavailable)
}

fn run_graph(
    source: &str,
    target: Option<&str>,
    cancelled: &dyn Fn() -> bool,
    started: &dyn Fn(),
) -> anyhow::Result<()> {
    let _runtime = Runtime::new()?;
    let input_device = device(source, MediaDevice::GetAudioCaptureSelector()?)?;
    let settings = AudioGraphSettings::Create(AudioRenderCategory::Media)?;
    // LowestLatency forces Raw render processing. Keep the normal effects path.
    settings.SetQuantumSizeSelectionMode(QuantumSizeSelectionMode::SystemDefault)?;
    settings.SetDesiredRenderDeviceAudioProcessing(AudioProcessing::Default)?;
    if let Some(target) = target {
        settings
            .SetPrimaryRenderDevice(&device(target, MediaDevice::GetAudioRenderSelector()?)?)?;
    }
    if cancelled() {
        return Ok(());
    }
    let result = AudioGraph::CreateAsync(&settings)?.join()?;
    ensure!(
        result.Status()? == AudioGraphCreationStatus::Success,
        "AudioGraph creation failed: {:?} ({:?})",
        result.Status()?,
        result.ExtendedError()?
    );
    let mut graph = Graph {
        graph: result.Graph()?,
        error_token: None,
    };
    ensure!(
        graph.graph.RenderDeviceAudioProcessing()? == AudioProcessing::Default,
        "AudioGraph did not enable default render processing"
    );
    let failed = Arc::new(AtomicBool::new(false));
    let error = failed.clone();
    let worker = thread::current();
    graph.error_token = Some(
        graph
            .graph
            .UnrecoverableErrorOccurred(&TypedEventHandler::<
                AudioGraph,
                AudioGraphUnrecoverableErrorOccurredEventArgs,
            >::new(move |_, args| {
                if let Some(args) = args.as_ref() {
                    let reason = args.Error()?;
                    if reason != AudioGraphUnrecoverableError::None {
                        tracing::warn!(?reason, "Forwarding graph device error");
                        error.store(true, Ordering::Release);
                        worker.unpark();
                    }
                }
                Ok(())
            }))?,
    );
    let output_result = graph.graph.CreateDeviceOutputNodeAsync()?.join()?;
    ensure!(
        output_result.Status()? == AudioDeviceNodeCreationStatus::Success,
        "AudioGraph output creation failed: {:?} ({:?})",
        output_result.Status()?,
        output_result.ExtendedError()?
    );
    let output = output_result.DeviceOutputNode()?;
    if cancelled() {
        return Ok(());
    }
    // Retain the existing WASAPI capture category. Windows selects the capture
    // effects; do not request a communications or speech processing scenario.
    let encoding = graph.graph.EncodingProperties()?;
    let input_result = graph
        .graph
        .CreateDeviceInputNodeWithFormatOnDeviceAsync(
            MediaCategory::Other,
            &encoding,
            &input_device,
        )?
        .join()
        .user_context(Text::OpenMicrophoneForwardingFailed)?;
    ensure!(
        input_result.Status()? == AudioDeviceNodeCreationStatus::Success,
        "AudioGraph input creation failed: {:?} ({:?})",
        input_result.Status()?,
        input_result.ExtendedError()?
    );
    let input = input_result.DeviceInputNode()?;
    input.AddOutgoingConnection(&output)?;
    if cancelled() {
        return Ok(());
    }
    graph
        .graph
        .Start()
        .context("Cannot start microphone forwarding graph")?;
    tracing::info!(
        samples_per_quantum = graph.graph.SamplesPerQuantum().ok(),
        sample_rate = encoding.SampleRate().ok(),
        "Microphone forwarding graph started with default audio processing"
    );
    started();
    while !cancelled() {
        ensure!(
            !failed.load(Ordering::Acquire),
            "AudioGraph encountered an unrecoverable error"
        );
        thread::park_timeout(Duration::from_millis(100));
    }
    Ok(())
}

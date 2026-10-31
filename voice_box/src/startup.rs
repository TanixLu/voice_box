//! One machine-wide, administrator-managed logon task; the database owns its intent.

use std::{os::windows::fs::MetadataExt, path::Path, sync::OnceLock};

use anyhow::{Context, ensure};
use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND},
        Security::{
            DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
        },
        Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT,
        System::{
            Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
            TaskScheduler::{
                IExecAction, ILogonTrigger, IRegisteredTask, ITaskDefinition, ITaskFolder,
                ITaskService, TASK_ACTION_EXEC, TASK_CREATE_OR_UPDATE, TASK_DONT_ADD_PRINCIPAL_ACE,
                TASK_INSTANCES_IGNORE_NEW, TASK_LOGON_GROUP, TASK_RUNLEVEL_HIGHEST,
                TASK_TRIGGER_LOGON, TaskScheduler,
            },
            Variant::VARIANT,
        },
    },
    core::{BSTR, Interface},
};

use voice_box_shared::paths::system_dir;

use crate::{
    i18n::{Text, UserContext},
    static_data::startup_script,
    utils::{ComGuard, conhost_path, powershell_path, win_base64_encode, win_sha256},
};

const SILENT_START_ARG: &str = "--silent-start";
const TASK_NAME: &str = "VoiceBoxStartup";
const ADMINISTRATORS: &str = "S-1-5-32-544";
// A protected DACL and group owner also exclude the creator's unelevated token.
const TASK_SECURITY: &str = "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)";

// Task Scheduler's COM getters use out parameters rather than returned values.
macro_rules! get {
    ($object:expr, $property:ident) => {{
        let mut value = Default::default();
        $object.$property(&mut value)?;
        value
    }};
}

pub fn silent_start() -> bool {
    static SILENT_START: OnceLock<bool> = OnceLock::new();
    *SILENT_START.get_or_init(|| std::env::args().any(|arg| arg == SILENT_START_ARG))
}

/// Returns true only when an enabled task was registered or repaired.
pub fn synchronize_startup(enabled: bool) -> anyhow::Result<bool> {
    synchronize_inner(enabled).user_context(Text::SyncStartupFailed)
}

fn synchronize_inner(enabled: bool) -> anyhow::Result<bool> {
    let _com_guard = ComGuard::mta()?;
    unsafe {
        let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
        let empty = VARIANT::default();
        service.Connect(&empty, &empty, &empty, &empty)?;
        let root = service.GetFolder(&BSTR::from("\\"))?;
        if !enabled {
            match root.DeleteTask(&BSTR::from(TASK_NAME), 0) {
                Ok(()) => tracing::info!("Removed startup task"),
                Err(error) if missing(&error) => {}
                Err(error) => return Err(error.into()),
            }
            return Ok(false);
        }
        let executable = std::env::current_exe()?;
        for entry in executable.ancestors() {
            ensure!(
                std::fs::symlink_metadata(entry)?.file_attributes()
                    & FILE_ATTRIBUTE_REPARSE_POINT.0
                    == 0,
                Text::StartupRedirectedPath
            );
        }
        let arguments = startup_arguments(&executable, &win_sha256(&std::fs::read(&executable)?)?)?;
        let expected = definition(&service, &arguments)?;
        match root.GetTask(&BSTR::from(TASK_NAME)) {
            Ok(task) if matches_task(&task, &expected)? => return Ok(false),
            Ok(_) => {}
            Err(error) if missing(&error) => {}
            Err(error) => return Err(error.into()),
        }
        let task = register(&root, TASK_NAME, &expected)?;
        ensure!(
            matches_task(&task, &expected)?,
            Text::StartupVerificationFailed
        );
        tracing::info!(path = %executable.display(), "Registered startup task");
        Ok(true)
    }
}

fn missing(error: &windows::core::Error) -> bool {
    error.code() == ERROR_FILE_NOT_FOUND.to_hresult()
        || error.code() == ERROR_PATH_NOT_FOUND.to_hresult()
}

fn register(
    root: &ITaskFolder,
    name: &str,
    definition: &ITaskDefinition,
) -> windows::core::Result<IRegisteredTask> {
    unsafe {
        root.RegisterTaskDefinition(
            &BSTR::from(name),
            definition,
            TASK_CREATE_OR_UPDATE.0 | TASK_DONT_ADD_PRINCIPAL_ACE.0,
            &VARIANT::from(ADMINISTRATORS),
            &VARIANT::default(),
            TASK_LOGON_GROUP,
            &VARIANT::from(TASK_SECURITY),
        )
    }
}

fn definition(service: &ITaskService, arguments: &str) -> anyhow::Result<ITaskDefinition> {
    unsafe {
        let definition = service.NewTask(0)?;
        let principal = definition.Principal()?;
        principal.SetId(&BSTR::from("VoiceBox"))?;
        principal.SetGroupId(&BSTR::from(ADMINISTRATORS))?;
        principal.SetLogonType(TASK_LOGON_GROUP)?;
        principal.SetRunLevel(TASK_RUNLEVEL_HIGHEST)?;
        let trigger: ILogonTrigger = definition.Triggers()?.Create(TASK_TRIGGER_LOGON)?.cast()?;
        trigger.SetEnabled(true.into())?;
        let settings = definition.Settings()?;
        settings.SetEnabled(true.into())?;
        settings.SetDisallowStartIfOnBatteries(false.into())?;
        settings.SetStopIfGoingOnBatteries(false.into())?;
        settings.SetRunOnlyIfIdle(false.into())?;
        settings.SetRunOnlyIfNetworkAvailable(false.into())?;
        settings.SetExecutionTimeLimit(&BSTR::from("PT0S"))?;
        settings.SetMultipleInstances(TASK_INSTANCES_IGNORE_NEW)?;
        let actions = definition.Actions()?;
        actions.SetContext(&BSTR::from("VoiceBox"))?;
        let action: IExecAction = actions.Create(TASK_ACTION_EXEC)?.cast()?;
        action.SetPath(&BSTR::from(
            conhost_path()?
                .to_str()
                .context("Invalid console host path")?,
        ))?;
        action.SetWorkingDirectory(&BSTR::from(
            system_dir()?.to_str().context("Invalid system path")?,
        ))?;
        action.SetArguments(&BSTR::from(format!(
            "--headless \"{}\" {arguments}",
            powershell_path()?
                .to_str()
                .context("Invalid PowerShell path")?,
        )))?;
        Ok(definition)
    }
}

fn matches_task(task: &IRegisteredTask, expected: &ITaskDefinition) -> anyhow::Result<bool> {
    unsafe {
        Ok(task.Enabled()?.as_bool()
            && matches_security(
                &task
                    .GetSecurityDescriptor(
                        (OWNER_SECURITY_INFORMATION.0
                            | GROUP_SECURITY_INFORMATION.0
                            | DACL_SECURITY_INFORMATION.0) as i32,
                    )?
                    .to_string(),
            )
            && matches_definition(&task.Definition()?, expected)?)
    }
}

fn matches_security(sddl: &str) -> bool {
    // Task Scheduler may mark the protected DACL as auto-inherited on registration.
    sddl.replace("D:PAI(", "D:P(") == TASK_SECURITY
}

fn matches_definition(
    actual: &ITaskDefinition,
    expected: &ITaskDefinition,
) -> anyhow::Result<bool> {
    unsafe {
        let principal = actual.Principal()?;
        let wanted_principal = expected.Principal()?;
        if get!(principal, GroupId) != get!(wanted_principal, GroupId)
            || get!(principal, LogonType) != TASK_LOGON_GROUP
            || get!(principal, RunLevel) != TASK_RUNLEVEL_HIGHEST
            || get!(principal, Id) != get!(wanted_principal, Id)
        {
            return Ok(false);
        }
        let actions = actual.Actions()?;
        let triggers = actual.Triggers()?;
        if get!(actions, Count) != 1 || get!(triggers, Count) != 1 {
            return Ok(false);
        }
        let action = actions.get_Item(1)?;
        let trigger = triggers.get_Item(1)?;
        if get!(action, Type) != TASK_ACTION_EXEC || get!(trigger, Type) != TASK_TRIGGER_LOGON {
            return Ok(false);
        }
        let action: IExecAction = action.cast()?;
        let wanted_action: IExecAction = expected.Actions()?.get_Item(1)?.cast()?;
        let logon: ILogonTrigger = trigger.cast()?;
        let wanted_logon: ILogonTrigger = expected.Triggers()?.get_Item(1)?.cast()?;
        let repetition = logon.Repetition()?;
        if get!(actions, Context) != get!(expected.Actions()?, Context)
            || get!(action, Path) != get!(wanted_action, Path)
            || get!(action, Arguments) != get!(wanted_action, Arguments)
            || get!(action, WorkingDirectory) != get!(wanted_action, WorkingDirectory)
            || !get!(logon, Enabled).as_bool()
            || !get!(logon, UserId).is_empty()
            || get!(logon, Delay) != get!(wanted_logon, Delay)
            || !get!(logon, StartBoundary).is_empty()
            || !get!(logon, EndBoundary).is_empty()
            || !get!(repetition, Interval).is_empty()
            || !get!(repetition, Duration).is_empty()
        {
            return Ok(false);
        }
        let settings = actual.Settings()?;
        let wanted_settings = expected.Settings()?;
        Ok(get!(settings, Enabled) == get!(wanted_settings, Enabled)
            && get!(settings, DisallowStartIfOnBatteries)
                == get!(wanted_settings, DisallowStartIfOnBatteries)
            && get!(settings, StopIfGoingOnBatteries)
                == get!(wanted_settings, StopIfGoingOnBatteries)
            && get!(settings, RunOnlyIfIdle) == get!(wanted_settings, RunOnlyIfIdle)
            && get!(settings, RunOnlyIfNetworkAvailable)
                == get!(wanted_settings, RunOnlyIfNetworkAvailable)
            && get!(settings, ExecutionTimeLimit) == get!(wanted_settings, ExecutionTimeLimit)
            && get!(settings, MultipleInstances) == get!(wanted_settings, MultipleInstances)
            && get!(settings, StartWhenAvailable) == get!(wanted_settings, StartWhenAvailable)
            && get!(settings, RestartCount) == get!(wanted_settings, RestartCount)
            && get!(settings, WakeToRun) == get!(wanted_settings, WakeToRun))
    }
}

fn startup_arguments(path: &Path, hash: &str) -> anyhow::Result<String> {
    ensure!(path.is_absolute(), "Executable path must be absolute");
    let path = path.to_str().context("Invalid executable path")?;
    ensure!(
        hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()),
        "Invalid SHA-256"
    );
    let script = startup_script(SILENT_START_ARG, hash, path)?;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = win_base64_encode(&bytes)?;
    let arguments =
        format!("-NoLogo -NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand {encoded}");
    ensure!(arguments.len() < 30_000, "Startup command is too long");
    Ok(arguments)
}

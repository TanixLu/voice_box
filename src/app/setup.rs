use crate::shared::plugin;
use anyhow::Context;
use is_elevated::is_elevated;
use restart_manager::{RestartSession, ShutdownOptions};
use tracing::info;
use windows::Win32::Media::KernelStreaming::{
    AUDIO_SIGNALPROCESSINGMODE_COMMUNICATIONS, AUDIO_SIGNALPROCESSINGMODE_DEFAULT,
    AUDIO_SIGNALPROCESSINGMODE_RAW, AUDIO_SIGNALPROCESSINGMODE_SPEECH,
};
use winreg::{
    RegKey,
    enums::{HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_READ, KEY_SET_VALUE, KEY_WOW64_64KEY},
    transaction::Transaction,
    types::FromRegValue,
};

use super::{
    logging,
    registry::{reg_get_value, reg_set_value},
    settings_writer,
};

const ELEVATED_REG_STR: &str = "--elevated-reg";
const ELEVATED_UNREG_STR: &str = "--elevated-unreg";
const ELEVATED_DATA_STR: &str = "--initialize-data";

const DISABLE_PROTECTED_AUDIO_DG_REG_PATH: &str =
    r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio\DisableProtectedAudioDG";

fn plugin_guid() -> String {
    format!("{{{:?}}}", plugin::CLSID)
}

fn common_string_reg_pairs() -> [(String, String); 6] {
    let guid = plugin_guid();
    let com = format!(r"HKCR\CLSID\{guid}\");
    let apo = format!(r"HKCR\AudioEngine\AudioProcessingObjects\{guid}\");
    let properties = plugin::registration_properties();
    [
        (com.clone(), plugin::NAME.into()),
        (format!(r"{com}InprocServer32\"), APO_DLL_PATH.into()),
        (
            format!(r"{com}InprocServer32\ThreadingModel"),
            "Both".into(),
        ),
        (format!("{apo}FriendlyName"), plugin::NAME.into()),
        (format!("{apo}Copyright"), plugin::COPYRIGHT.into()),
        (
            format!("{apo}APOInterface0"),
            format!("{{{:?}}}", properties.iidAPOInterfaceList[0]),
        ),
    ]
}

fn common_u32_reg_pairs() -> [(String, u32); 10] {
    let guid = plugin_guid();
    let apo = format!(r"HKCR\AudioEngine\AudioProcessingObjects\{guid}\");
    let properties = plugin::registration_properties();
    [
        (DISABLE_PROTECTED_AUDIO_DG_REG_PATH.into(), 1),
        (format!("{apo}MajorVersion"), properties.u32MajorVersion),
        (format!("{apo}MinorVersion"), properties.u32MinorVersion),
        (
            format!("{apo}NumAPOInterfaces"),
            properties.u32NumAPOInterfaces,
        ),
        (
            format!("{apo}MinInputConnections"),
            properties.u32MinInputConnections,
        ),
        (
            format!("{apo}MaxInputConnections"),
            properties.u32MaxInputConnections,
        ),
        (
            format!("{apo}MinOutputConnections"),
            properties.u32MinOutputConnections,
        ),
        (
            format!("{apo}MaxOutputConnections"),
            properties.u32MaxOutputConnections,
        ),
        (format!("{apo}MaxInstances"), properties.u32MaxInstances),
        (format!("{apo}Flags"), properties.Flags.0 as u32),
    ]
}

const COMPOSITE_MFX_GUID: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},14";
const MFX_PROCESSING_MODES: &str = "{d3993a3f-99c2-4402-b5ec-a92a0367664b},6";

fn default_processing_modes() -> Vec<String> {
    [
        AUDIO_SIGNALPROCESSINGMODE_DEFAULT,
        AUDIO_SIGNALPROCESSINGMODE_COMMUNICATIONS,
        AUDIO_SIGNALPROCESSINGMODE_RAW,
        AUDIO_SIGNALPROCESSINGMODE_SPEECH,
    ]
    .into_iter()
    .map(|mode| format!("{{{mode:?}}}"))
    .collect()
}

const DISABLE_APO_GUID: &str = "{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},5";

pub(super) const APO_DLL: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/target/voice_box.dll"));

pub(super) const APO_DLL_PATH: &str = r"C:\Windows\System32\voice_box.dll";
const AUDIO_SERVICE_NAME: &str = "AudioSrv";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginStatus {
    NotInstalled,
    NeedsUpdate,
    Installed,
}

// The GUI and setup helper must use this same complete readiness check. An endpoint
// association alone does not mean the bundled DLL and registration are current.
pub fn device_plugin_status(device_guid: &str) -> PluginStatus {
    let plugin_guid = plugin_guid();
    let root = RegKey::predef(HKEY_LOCAL_MACHINE);
    let fx_path = format!(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture\{device_guid}\FxProperties"
    );
    let Ok(fx) = root.open_subkey_with_flags(fx_path, KEY_READ | KEY_WOW64_64KEY) else {
        return PluginStatus::NotInstalled;
    };
    let effects = fx
        .get_value::<Vec<String>, _>(COMPOSITE_MFX_GUID)
        .unwrap_or_default();
    if !effects
        .iter()
        .any(|effect| effect.eq_ignore_ascii_case(&plugin_guid))
    {
        return PluginStatus::NotInstalled;
    }
    if !settings_writer::ready() {
        return PluginStatus::NeedsUpdate;
    }
    // 检测dll是否需要更新
    if std::fs::read(APO_DLL_PATH).ok().as_deref() != Some(APO_DLL) {
        return PluginStatus::NeedsUpdate;
    }

    // 检测注册表是否需要更新
    for (reg_path, expected_value) in common_string_reg_pairs() {
        if reg_get_value::<String>(&reg_path).ok().as_deref() != Some(expected_value.as_str()) {
            return PluginStatus::NeedsUpdate;
        }
    }
    for (reg_path, expected_value) in common_u32_reg_pairs() {
        if reg_get_value::<u32>(&reg_path).ok() != Some(expected_value) {
            return PluginStatus::NeedsUpdate;
        }
    }
    // 检测设备FxProperties是否需要更新
    if !(effects
        .first()
        .is_some_and(|effect| effect.eq_ignore_ascii_case(&plugin_guid))
        && !effects
            .iter()
            .skip(1)
            .any(|effect| effect.eq_ignore_ascii_case(&plugin_guid)))
    {
        return PluginStatus::NeedsUpdate;
    }
    if !fx
        .get_value::<Vec<String>, _>(MFX_PROCESSING_MODES)
        .is_ok_and(|modes| modes.iter().any(|mode| !mode.is_empty()))
    {
        return PluginStatus::NeedsUpdate;
    }
    if fx
        .get_value::<u32, _>(DISABLE_APO_GUID)
        .is_ok_and(|value| value != 0)
    {
        return PluginStatus::NeedsUpdate;
    }

    PluginStatus::Installed
}

fn elevated_reg(device_guid: &str, uninstall: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_elevated(),
        "Elevated operation requested but not running as administrator"
    );

    let mut session = RestartSession::new()?;
    session.register_services([AUDIO_SERVICE_NAME])?;
    let report = session.affected_applications()?;
    for app in &report {
        info!("Affected application: {:?}", app);
    }
    let pending = session.shutdown_with_options(ShutdownOptions::default());
    if !pending.shutdown_outcome().is_success() {
        anyhow::bail!(
            "Audio service shutdown failed: {:?}",
            pending.shutdown_outcome()
        );
    }
    info!("Audio service shutdown successfully");

    // Always restart the service, including when a registry or DLL write fails.
    let registration = if uninstall {
        elevated_unreg_inner(device_guid)
    } else {
        elevated_reg_inner(device_guid)
    };

    let completion = pending.restart();
    let outcome = completion.outcome().clone();
    completion.end()?;
    outcome.shutdown_outcome().clone().into_result()?;
    outcome
        .restart_outcome()
        .expect("Failed to restart audio service")
        .clone()
        .into_result()?;
    info!("Audio service restarted successfully");
    registration?;
    Ok(())
}

fn elevated_reg_inner(device_guid: &str) -> anyhow::Result<()> {
    settings_writer::initialize().context("初始化 VoiceBox 数据目录失败")?;
    anyhow::ensure!(
        device_guid.len() == 38
            && device_guid.starts_with('{')
            && device_guid.ends_with('}')
            && device_guid.as_bytes()[1..37]
                .iter()
                .copied()
                .enumerate()
                .all(|(i, byte)| {
                    if [8, 13, 18, 23].contains(&i) {
                        byte == b'-'
                    } else {
                        byte.is_ascii_hexdigit()
                    }
                }),
        "Invalid device GUID"
    );
    // 更新dll
    if std::fs::read(APO_DLL_PATH).ok().as_deref() != Some(APO_DLL) {
        std::fs::write(APO_DLL_PATH, APO_DLL)?;
        info!("Updated APO DLL at {}", APO_DLL_PATH);
    }

    // 更新注册表
    for (reg_path, expected_value) in common_string_reg_pairs() {
        if reg_get_value::<String>(&reg_path).ok().as_deref() != Some(expected_value.as_str()) {
            reg_set_value(&reg_path, &expected_value)?;
            info!("Updated registry key {} to {}", reg_path, expected_value);
        }
    }
    for (reg_path, expected_value) in common_u32_reg_pairs() {
        if reg_get_value::<u32>(&reg_path).ok() != Some(expected_value) {
            reg_set_value(&reg_path, &expected_value)?;
            info!("Updated registry key {} to {}", reg_path, expected_value);
        }
    }

    update_device_effects(device_guid, false)
}

fn optional_value<T: FromRegValue>(key: &RegKey, name: &str) -> std::io::Result<Option<T>> {
    match key.get_value(name) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn delete_optional(key: &RegKey, name: &str) -> std::io::Result<()> {
    match key.delete_value(name) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn elevated_unreg_inner(device_guid: &str) -> anyhow::Result<()> {
    update_device_effects(device_guid, true)
}

fn update_device_effects(device_guid: &str, uninstall: bool) -> anyhow::Result<()> {
    let transaction = Transaction::new().context("创建 APO 注册事务失败")?;
    let root = RegKey::predef(HKEY_LOCAL_MACHINE);
    // MMDevices permits setting values but can deny creating subkeys, even to
    // administrators. KEY_WRITE includes KEY_CREATE_SUB_KEY, which we do not need.
    let flags = KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_WOW64_64KEY;
    let fx = root.open_subkey_transacted_with_flags(
        format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture\{device_guid}\FxProperties"),
        &transaction, flags,
    ).with_context(|| format!("打开麦克风 {device_guid} 的 FxProperties 失败"))?;
    update_fx_properties(&fx, uninstall)?;
    // All endpoint changes commit together; failures leave them untouched.
    transaction.commit().context("提交 APO 注册事务失败")?;
    info!(
        "{} VoiceBox device effects for {device_guid}",
        if uninstall {
            "Unregistered"
        } else {
            "Registered"
        }
    );
    Ok(())
}

fn update_fx_properties(fx: &RegKey, uninstall: bool) -> anyhow::Result<()> {
    let plugin_guid = plugin_guid();
    // Only an absent value means an empty list. Refuse to overwrite invalid types
    // or unreadable values, since they may contain another APO's settings.
    let mut effects: Vec<String> = optional_value(fx, COMPOSITE_MFX_GUID)
        .context("读取 Composite MFX APO 列表失败")?
        .unwrap_or_default();
    if uninstall {
        let count = effects.len();
        effects.retain(|effect| !effect.eq_ignore_ascii_case(&plugin_guid));
        if effects.len() != count {
            if effects.is_empty() {
                delete_optional(fx, COMPOSITE_MFX_GUID)?;
            } else {
                fx.set_value(COMPOSITE_MFX_GUID, &effects)?;
            }
        }
    } else {
        effects.retain(|effect| !effect.eq_ignore_ascii_case(&plugin_guid));
        effects.insert(0, plugin_guid);
        fx.set_value(COMPOSITE_MFX_GUID, &effects)?;
        let modes: Vec<String> = optional_value(fx, MFX_PROCESSING_MODES)
            .context("读取 MFX 处理模式失败")?
            .unwrap_or_default();
        if modes.iter().all(String::is_empty) {
            fx.set_value(MFX_PROCESSING_MODES, &default_processing_modes())?;
        }
        delete_optional(fx, DISABLE_APO_GUID)?;
    }
    Ok(())
}
pub fn maybe_elevated_setup() -> anyhow::Result<bool> {
    let args: Vec<String> = std::env::args().collect();
    if matches!(
        args.get(1).map(String::as_str),
        Some(ELEVATED_REG_STR | ELEVATED_UNREG_STR | ELEVATED_DATA_STR)
    ) {
        anyhow::ensure!(
            is_elevated(),
            "Elevated operation requested but not running as administrator"
        );
        settings_writer::initialize()?;
        logging::init(true)?;
        info!("Running elevated operation: {}", args[1]);
        if args[1] == ELEVATED_DATA_STR {
            return Ok(true);
        }
        let device_guid = args.get(2).ok_or_else(|| {
            anyhow::anyhow!("Missing device GUID argument for elevated registration operation")
        })?;
        // Validate before interpolating a device identifier into registry paths.
        let guid = device_guid.trim_matches(['{', '}']);
        anyhow::ensure!(
            device_guid.len() == 38 && windows::core::GUID::try_from(guid).is_ok(),
            "Invalid device GUID"
        );
        elevated_reg(device_guid, args[1] == ELEVATED_UNREG_STR)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn ensure_data_ready() -> anyhow::Result<()> {
    match settings_writer::initialize() {
        Ok(()) => Ok(()),
        Err(error)
            if !is_elevated()
                && error.chain().any(|cause| {
                    cause
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
                        || cause
                            .downcast_ref::<windows::core::Error>()
                            .is_some_and(|error| {
                                error.code() == windows::Win32::Foundation::E_ACCESSDENIED
                            })
                }) =>
        {
            let status = runas::Command::new(std::env::current_exe()?)
                .arg(ELEVATED_DATA_STR)
                .gui(false)
                .show(false)
                .status()?;
            anyhow::ensure!(status.success(), "初始化 VoiceBox 数据目录的管理员操作失败");
            anyhow::ensure!(settings_writer::ready(), "VoiceBox 数据目录权限检查未通过");
            Ok(())
        }
        Err(error) => Err(error),
    }
}

pub fn ensure_device_reg(device_guid: &str) -> anyhow::Result<()> {
    if device_plugin_status(device_guid) != PluginStatus::Installed {
        device_operation(device_guid, false)?;
        anyhow::ensure!(
            device_plugin_status(device_guid) == PluginStatus::Installed,
            "安装操作已结束，但插件检查未通过，请刷新后重试"
        );
    }
    Ok(())
}

pub fn uninstall_device(device_guid: &str) -> anyhow::Result<()> {
    device_operation(device_guid, true)
}

fn device_operation(device_guid: &str, uninstall: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        device_guid.len() == 38
            && windows::core::GUID::try_from(device_guid.trim_matches(['{', '}'])).is_ok(),
        "Invalid device GUID"
    );
    if is_elevated() {
        elevated_reg(device_guid, uninstall)?;
    } else {
        let status = runas::Command::new(std::env::current_exe()?)
            .arg(if uninstall {
                ELEVATED_UNREG_STR
            } else {
                ELEVATED_REG_STR
            })
            .arg(device_guid)
            .gui(false)
            .show(false)
            .status()?;
        anyhow::ensure!(
            status.success(),
            "Failed to execute elevated operation: {:?}",
            status
        );
    }
    Ok(())
}

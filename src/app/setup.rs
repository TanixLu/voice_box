use crate::shared::{paths, plugin};
use anyhow::Context;
use restart_manager::{RestartSession, ShutdownOptions};
use std::path::Path;
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
    apo_dll_bytes,
    registry::{read_registry_value, write_registry_value},
    settings_writer,
};

const DISABLE_PROTECTED_AUDIO_DG_REG_PATH: &str =
    r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio\DisableProtectedAudioDG";

fn plugin_guid() -> String {
    format!("{{{:?}}}", plugin::CLSID)
}

fn string_registration_values(apo_dll: &Path) -> anyhow::Result<[(String, String); 6]> {
    let apo_dll = apo_dll.to_str().context("APO DLL 路径不是有效的 Unicode")?;
    let guid = plugin_guid();
    let com = format!(r"HKLM\SOFTWARE\Classes\CLSID\{guid}\");
    let apo = format!(r"HKLM\SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{guid}\");
    let properties = plugin::registration_properties();
    Ok([
        (com.clone(), plugin::NAME.into()),
        (format!(r"{com}InprocServer32\"), apo_dll.into()),
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
    ])
}

fn dword_registration_values() -> [(String, u32); 10] {
    let guid = plugin_guid();
    let apo = format!(r"HKLM\SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{guid}\");
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

const COMPOSITE_MFX_PROPERTY_KEY: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},14";
const MFX_PROCESSING_MODES_PROPERTY_KEY: &str = "{d3993a3f-99c2-4402-b5ec-a92a0367664b},6";

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

const DISABLE_APO_PROPERTY_KEY: &str = "{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},5";

const AUDIO_SERVICE_NAME: &str = "AudioSrv";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginStatus {
    NotInstalled,
    NeedsUpdate,
    Installed,
}

// An endpoint association alone does not mean the bundled DLL and registration
// are current.
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
        .get_value::<Vec<String>, _>(COMPOSITE_MFX_PROPERTY_KEY)
        .unwrap_or_default();
    if !effects
        .iter()
        .any(|effect| effect.eq_ignore_ascii_case(&plugin_guid))
    {
        return PluginStatus::NotInstalled;
    }
    if !settings_writer::has_valid_settings() {
        return PluginStatus::NeedsUpdate;
    }
    // Check whether the DLL needs to be updated.
    let Ok(apo_dll) = paths::apo_dll_path() else {
        return PluginStatus::NeedsUpdate;
    };
    if std::fs::read(&apo_dll).ok().as_deref() != Some(apo_dll_bytes()) {
        return PluginStatus::NeedsUpdate;
    }

    // Check whether the registry needs to be updated.
    let Ok(string_values) = string_registration_values(&apo_dll) else {
        return PluginStatus::NeedsUpdate;
    };
    for (reg_path, expected_value) in string_values {
        if read_registry_value::<String>(&reg_path).ok().as_deref() != Some(expected_value.as_str())
        {
            return PluginStatus::NeedsUpdate;
        }
    }
    for (reg_path, expected_value) in dword_registration_values() {
        if read_registry_value::<u32>(&reg_path).ok() != Some(expected_value) {
            return PluginStatus::NeedsUpdate;
        }
    }
    // Check whether the device's FxProperties need to be updated.
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
        .get_value::<Vec<String>, _>(MFX_PROCESSING_MODES_PROPERTY_KEY)
        .is_ok_and(|modes| modes.iter().any(|mode| !mode.is_empty()))
    {
        return PluginStatus::NeedsUpdate;
    }
    if fx
        .get_value::<u32, _>(DISABLE_APO_PROPERTY_KEY)
        .is_ok_and(|value| value != 0)
    {
        return PluginStatus::NeedsUpdate;
    }

    PluginStatus::Installed
}

pub(super) fn with_audio_service_stopped(
    operation: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
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
    let operation_result = operation();

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
    operation_result?;
    Ok(())
}

fn install_device_plugin(device_guid: &str) -> anyhow::Result<()> {
    let apo_dll = paths::apo_dll_path().context("读取 APO DLL 路径失败")?;
    let string_values = string_registration_values(&apo_dll)?;
    // Update the DLL.
    if std::fs::read(&apo_dll).ok().as_deref() != Some(apo_dll_bytes()) {
        std::fs::write(&apo_dll, apo_dll_bytes())?;
        info!("Updated APO DLL at {}", apo_dll.display());
    }

    // Update the registry.
    for (reg_path, expected_value) in string_values {
        if read_registry_value::<String>(&reg_path).ok().as_deref() != Some(expected_value.as_str())
        {
            write_registry_value(&reg_path, &expected_value)?;
            info!("Updated registry key {} to {}", reg_path, expected_value);
        }
    }
    for (reg_path, expected_value) in dword_registration_values() {
        if read_registry_value::<u32>(&reg_path).ok() != Some(expected_value) {
            write_registry_value(&reg_path, &expected_value)?;
            info!("Updated registry key {} to {}", reg_path, expected_value);
        }
    }

    update_device_effects(device_guid, false)
}

fn read_optional_value<T: FromRegValue>(key: &RegKey, name: &str) -> std::io::Result<Option<T>> {
    match key.get_value(name) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn delete_value_if_present(key: &RegKey, name: &str) -> std::io::Result<()> {
    match key.delete_value(name) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
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
    let mut effects: Vec<String> = read_optional_value(fx, COMPOSITE_MFX_PROPERTY_KEY)
        .context("读取 Composite MFX APO 列表失败")?
        .unwrap_or_default();
    if uninstall {
        let count = effects.len();
        effects.retain(|effect| !effect.eq_ignore_ascii_case(&plugin_guid));
        if effects.len() != count {
            if effects.is_empty() {
                delete_value_if_present(fx, COMPOSITE_MFX_PROPERTY_KEY)?;
            } else {
                fx.set_value(COMPOSITE_MFX_PROPERTY_KEY, &effects)?;
            }
        }
    } else {
        effects.retain(|effect| !effect.eq_ignore_ascii_case(&plugin_guid));
        effects.insert(0, plugin_guid);
        fx.set_value(COMPOSITE_MFX_PROPERTY_KEY, &effects)?;
        let modes: Vec<String> = read_optional_value(fx, MFX_PROCESSING_MODES_PROPERTY_KEY)
            .context("读取 MFX 处理模式失败")?
            .unwrap_or_default();
        if modes.iter().all(String::is_empty) {
            fx.set_value(
                MFX_PROCESSING_MODES_PROPERTY_KEY,
                &default_processing_modes(),
            )?;
        }
        delete_value_if_present(fx, DISABLE_APO_PROPERTY_KEY)?;
    }
    Ok(())
}

pub(super) fn unregister_all() -> anyhow::Result<()> {
    let transaction = Transaction::new().context("创建完全卸载注册事务失败")?;
    let root = RegKey::predef(HKEY_LOCAL_MACHINE);
    let capture = match root.open_subkey_transacted_with_flags(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture",
        &transaction,
        KEY_READ | KEY_WOW64_64KEY,
    ) {
        Ok(capture) => Some(capture),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("枚举麦克风注册表失败"),
    };
    if let Some(capture) = capture {
        // Enumerate the registry rather than active WASAPI devices: disabled and
        // disconnected microphones must lose their VoiceBox association too.
        for device in capture.enum_keys() {
            let device = device.context("读取麦克风注册表项失败")?;
            let fx = match capture.open_subkey_transacted_with_flags(
                format!(r"{device}\FxProperties"),
                &transaction,
                KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_WOW64_64KEY,
            ) {
                Ok(fx) => fx,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("打开麦克风 {device} 的 FxProperties 失败"));
                }
            };
            update_fx_properties(&fx, true)
                .with_context(|| format!("卸载麦克风 {device} 的 VoiceBox APO 失败"))?;
        }
    }

    // Match the machine-wide 64-bit COM and APO registrations made by setup.
    // Do not reset shared Windows audio settings or other APOs' properties.
    let guid = plugin_guid();
    for path in [
        format!(r"SOFTWARE\Classes\CLSID\{guid}"),
        format!(r"SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{guid}"),
    ] {
        delete_tree_transacted(&root, &path, &transaction)
            .with_context(|| format!("删除 VoiceBox APO 注册失败：{path}"))?;
    }
    transaction.commit().context("提交完全卸载注册事务失败")?;
    info!("Removed VoiceBox from all capture devices and unregistered the APO");
    Ok(())
}

fn delete_tree_transacted(
    root: &RegKey,
    path: &str,
    transaction: &Transaction,
) -> std::io::Result<()> {
    let key =
        match root.open_subkey_transacted_with_flags(path, transaction, KEY_READ | KEY_WOW64_64KEY)
        {
            Ok(key) => key,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
    let children = key.enum_keys().collect::<std::io::Result<Vec<_>>>()?;
    for child in children {
        delete_tree_transacted(root, &format!(r"{path}\{child}"), transaction)?;
    }
    drop(key);
    root.delete_subkey_transacted_with_flags(path, transaction, KEY_WOW64_64KEY)
}

pub fn ensure_device_plugin_installed(device_guid: &str) -> anyhow::Result<()> {
    if device_plugin_status(device_guid) != PluginStatus::Installed {
        run_device_plugin_operation(device_guid, false)?;
        anyhow::ensure!(
            device_plugin_status(device_guid) == PluginStatus::Installed,
            "安装操作已结束，但插件检查未通过，请刷新后重试"
        );
    }
    Ok(())
}

pub fn uninstall_device_plugin(device_guid: &str) -> anyhow::Result<()> {
    run_device_plugin_operation(device_guid, true)
}

fn run_device_plugin_operation(device_guid: &str, uninstall: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        device_guid.len() == 38
            && device_guid.starts_with('{')
            && device_guid.ends_with('}')
            && windows::core::GUID::try_from(device_guid.trim_matches(['{', '}'])).is_ok(),
        "Invalid device GUID"
    );
    with_audio_service_stopped(|| {
        if uninstall {
            update_device_effects(device_guid, true)
        } else {
            install_device_plugin(device_guid)
        }
    })
}

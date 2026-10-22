use crate::app::i18n::Text;
use crate::app::i18n::{Message, UserContext};
use crate::shared::{paths, plugin};

use atomic_write_file::AtomicWriteFile;
use restart_manager::{RestartSession, ShutdownOptions};
use std::io::Write;
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
    let apo_dll = apo_dll.to_str().user_context(Text::InvalidApoPath)?;
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
const SELECTED_EFFECT_PACK_PROPERTY_KEY: &str = "{c876062a-a276-4ed6-b15b-3962e69007f8},2";
const DEVICE_DEFAULT_EFFECT_PACK: &str = "{00D5B26C-4D0F-4A83-BB69-C2F12DC5FB10}";

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
    let Ok(properties) = root.open_subkey_with_flags(
        format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture\{device_guid}\Properties"),
        KEY_QUERY_VALUE | KEY_WOW64_64KEY,
    ) else {
        return PluginStatus::NeedsUpdate;
    };
    if !device_default_effects_enabled(&fx, &properties) {
        return PluginStatus::NeedsUpdate;
    }

    PluginStatus::Installed
}

fn device_default_effects_enabled(fx: &RegKey, properties: &RegKey) -> bool {
    if !matches!(
        read_optional_value::<u32>(fx, DISABLE_APO_PROPERTY_KEY),
        Ok(None | Some(0))
    ) {
        return false;
    }
    // An absent selection may fall back to a different effect pack.
    properties
        .get_raw_value(SELECTED_EFFECT_PACK_PROPERTY_KEY)
        .is_ok_and(|value| {
            value.vtype == winreg::enums::REG_SZ
                && String::from_reg_value(&value)
                    .is_ok_and(|pack| pack.eq_ignore_ascii_case(DEVICE_DEFAULT_EFFECT_PACK))
        })
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
        anyhow::bail!(Message::StopAudioServiceFailed {
            result: format!("{:?}", pending.shutdown_outcome())
        });
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
    let apo_dll = paths::apo_dll_path().user_context(Text::ReadApoPath)?;
    let string_values = string_registration_values(&apo_dll)?;
    // Update the DLL.
    if std::fs::read(&apo_dll).ok().as_deref() != Some(apo_dll_bytes()) {
        let mut file = AtomicWriteFile::open(&apo_dll)?;
        file.write_all(apo_dll_bytes())?;
        file.commit()?;
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
    let transaction = Transaction::new().user_context(Text::CreateRegistrationTransaction)?;
    let root = RegKey::predef(HKEY_LOCAL_MACHINE);
    // MMDevices permits setting values but can deny creating subkeys, even to
    // administrators. KEY_WRITE includes KEY_CREATE_SUB_KEY, which we do not need.
    let flags = KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_WOW64_64KEY;
    let fx = root.open_subkey_transacted_with_flags(
        format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture\{device_guid}\FxProperties"),
        &transaction, flags,
    ).with_user_context(|| Message::OpenFxProperties { device: device_guid.to_owned() })?;
    update_fx_properties(&fx, uninstall)?;
    if !uninstall {
        let properties = root.open_subkey_transacted_with_flags(
            format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture\{device_guid}\Properties"),
            &transaction, flags,
        ).with_user_context(|| Message::OpenEndpointProperties { device: device_guid.to_owned() })?;
        properties
            .set_value(
                SELECTED_EFFECT_PACK_PROPERTY_KEY,
                &DEVICE_DEFAULT_EFFECT_PACK,
            )
            .user_context(Text::SelectDeviceDefaultEffects)?;
    }
    // All endpoint changes commit together; failures leave them untouched.
    transaction
        .commit()
        .user_context(Text::CommitRegistrationTransaction)?;
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
        .user_context(Text::ReadCompositeEffects)?
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
            .user_context(Text::ReadProcessingModes)?
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
    let transaction = Transaction::new().user_context(Text::CreateUninstallTransaction)?;
    let root = RegKey::predef(HKEY_LOCAL_MACHINE);
    let capture = match root.open_subkey_transacted_with_flags(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture",
        &transaction,
        KEY_READ | KEY_WOW64_64KEY,
    ) {
        Ok(capture) => Some(capture),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).user_context(Text::EnumerateMicrophoneRegistry),
    };
    if let Some(capture) = capture {
        // Enumerate the registry rather than active WASAPI devices: disabled and
        // disconnected microphones must lose their VoiceBox association too.
        for device in capture.enum_keys() {
            let device = device.user_context(Text::ReadMicrophoneRegistry)?;
            let fx = match capture.open_subkey_transacted_with_flags(
                format!(r"{device}\FxProperties"),
                &transaction,
                KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_WOW64_64KEY,
            ) {
                Ok(fx) => fx,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error).with_user_context(|| Message::OpenFxProperties {
                        device: device.to_owned(),
                    });
                }
            };
            update_fx_properties(&fx, true).with_user_context(|| Message::UninstallMicrophone {
                device: device.to_owned(),
            })?;
        }
    }

    // Match the machine-wide 64-bit COM and APO registrations made by setup.
    // Do not reset shared Windows audio settings or other APOs' properties.
    let guid = plugin_guid();
    for path in [
        format!(r"SOFTWARE\Classes\CLSID\{guid}"),
        format!(r"SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{guid}"),
    ] {
        delete_tree_transacted(&root, &path, &transaction).with_user_context(|| {
            Message::DeleteApoRegistration {
                path: path.to_owned(),
            }
        })?;
    }
    transaction
        .commit()
        .user_context(Text::CommitUninstallTransaction)?;
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
            Text::PluginVerificationFailed
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
        Text::InvalidDeviceGuid
    );
    with_audio_service_stopped(|| {
        if uninstall {
            update_device_effects(device_guid, true)
        } else {
            install_device_plugin(device_guid)
        }
    })
}

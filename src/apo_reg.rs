use is_elevated::is_elevated;
use restart_manager::{RestartSession, ShutdownOptions};
use tracing::info;
use windows::Win32::Media::Audio::Apo::{
    APO_FLAG_BITSPERSAMPLE_MUST_MATCH, APO_FLAG_FRAMESPERSECOND_MUST_MATCH, APO_FLAG_INPLACE,
    APO_FLAG_SAMPLESPERFRAME_MUST_MATCH,
};
use windows::Win32::Media::KernelStreaming::{
    AUDIO_SIGNALPROCESSINGMODE_COMMUNICATIONS, AUDIO_SIGNALPROCESSINGMODE_DEFAULT,
    AUDIO_SIGNALPROCESSINGMODE_FAR_FIELD_SPEECH, AUDIO_SIGNALPROCESSINGMODE_MEDIA,
    AUDIO_SIGNALPROCESSINGMODE_SPEECH,
};

use crate::utils::{
    LOG_ENABLE_STR, reg_delete_value, reg_get_optional_value, reg_get_value, reg_set_value,
};

const ELEVATED_REG_STR: &str = "--elevated-reg";

const VOICE_BOX_GUID: &str = "{AE672075-B244-4417-B5A5-FBD45B7F0268}";

const DISABLE_PROTECTED_AUDIO_DG_REG_PATH: &str =
    r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio\DisableProtectedAudioDG";

const COM_REG_BASE: &str = constcat::concat!(r"HKCR\CLSID\", VOICE_BOX_GUID, r"\");
const COM_REG_INPROC: &str = constcat::concat!(COM_REG_BASE, r"InprocServer32\");
const COM_REG_THREADING_MODEL: &str = constcat::concat!(COM_REG_INPROC, r"ThreadingModel");

const APO_REG_BASE: &str = constcat::concat!(
    r"HKCR\AudioEngine\AudioProcessingObjects\",
    VOICE_BOX_GUID,
    r"\"
);
const APO_REG_FRIENDLY_NAME: &str = constcat::concat!(APO_REG_BASE, r"FriendlyName");
const APO_REG_COPYRIGHT: &str = constcat::concat!(APO_REG_BASE, r"Copyright");
const APO_REG_INTERFACE: &str = constcat::concat!(APO_REG_BASE, r"APOInterface0");
const APO_REG_MAJOR_VERSION: &str = constcat::concat!(APO_REG_BASE, r"MajorVersion");
const APO_REG_MINOR_VERSION: &str = constcat::concat!(APO_REG_BASE, r"MinorVersion");
const APO_REG_NUM_APO_INTERFACES: &str = constcat::concat!(APO_REG_BASE, r"NumAPOInterfaces");
const APO_REG_MIN_INPUT_CONNECTIONS: &str = constcat::concat!(APO_REG_BASE, r"MinInputConnections");
const APO_REG_MAX_INPUT_CONNECTIONS: &str = constcat::concat!(APO_REG_BASE, r"MaxInputConnections");
const APO_REG_MIN_OUTPUT_CONNECTIONS: &str =
    constcat::concat!(APO_REG_BASE, r"MinOutputConnections");
const APO_REG_MAX_OUTPUT_CONNECTIONS: &str =
    constcat::concat!(APO_REG_BASE, r"MaxOutputConnections");
const APO_REG_MAX_INSTANCES: &str = constcat::concat!(APO_REG_BASE, r"MaxInstances");
const APO_REG_FLAGS: &str = constcat::concat!(APO_REG_BASE, r"Flags");

const APO_INTERFACE_IID: &str = "{FD7F2B29-24D0-4B5C-B177-592C39F9CA10}";

const COMMON_STRING_REG_PAIRS: [(&str, &str); 6] = [
    (COM_REG_BASE, "VoiceBox APO"),
    (COM_REG_INPROC, APO_DLL_PATH),
    (COM_REG_THREADING_MODEL, "Both"),
    (APO_REG_FRIENDLY_NAME, "VoiceBox APO"),
    (APO_REG_COPYRIGHT, "Copyright (C) 2026 Tanix"),
    (APO_REG_INTERFACE, APO_INTERFACE_IID),
];

const VOICE_BOX_APO_FLAGS: u32 = (APO_FLAG_INPLACE.0
    | APO_FLAG_SAMPLESPERFRAME_MUST_MATCH.0
    | APO_FLAG_FRAMESPERSECOND_MUST_MATCH.0
    | APO_FLAG_BITSPERSAMPLE_MUST_MATCH.0) as u32;

const COMMON_U32_REG_PAIRS: [(&str, u32); 10] = [
    (DISABLE_PROTECTED_AUDIO_DG_REG_PATH, 1),
    (APO_REG_MAJOR_VERSION, 1),
    (APO_REG_MINOR_VERSION, 0),
    (APO_REG_NUM_APO_INTERFACES, 1),
    (APO_REG_MIN_INPUT_CONNECTIONS, 1),
    (APO_REG_MAX_INPUT_CONNECTIONS, 1),
    (APO_REG_MIN_OUTPUT_CONNECTIONS, 1),
    (APO_REG_MAX_OUTPUT_CONNECTIONS, 1),
    (APO_REG_MAX_INSTANCES, u32::MAX),
    (APO_REG_FLAGS, VOICE_BOX_APO_FLAGS),
];

// Stream effects are omitted from RAW streams. Endpoint effects cannot provide
// that guarantee because the endpoint instance is shared by all modes.
const SFX_GUID: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},5";
const SFX_PROCESSING_MODES: &str = "{d3993a3f-99c2-4402-b5ec-a92a0367664b},5";

fn processing_modes() -> Vec<String> {
    [
        AUDIO_SIGNALPROCESSINGMODE_DEFAULT,
        AUDIO_SIGNALPROCESSINGMODE_COMMUNICATIONS,
        AUDIO_SIGNALPROCESSINGMODE_SPEECH,
        AUDIO_SIGNALPROCESSINGMODE_MEDIA,
        AUDIO_SIGNALPROCESSINGMODE_FAR_FIELD_SPEECH,
    ]
    .into_iter()
    .map(|mode| format!("{{{mode:?}}}"))
    .collect()
}

const DISABLE_APO_GUID: &str = "{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},5";

fn device_fx_property_reg_path(device_guid: &str, property: &str) -> String {
    format!(
        r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture\{device_guid}\FxProperties\{property}"
    )
}

fn device_fx_property_backup_reg_path(device_guid: &str, property: &str) -> String {
    format!(r"HKLM\SOFTWARE\VoiceBox\FxPropertyBackup\{device_guid}\{property}")
}

const APO_DLL: &[u8] = include_bytes!("../target/voice_box.dll");

const APO_DLL_PATH: &str = r"C:\Windows\System32\voice_box.dll";
const AUDIO_SERVICE_NAME: &str = "AudioSrv";

fn need_reg(device_guid: &str) -> bool {
    if !crate::config_reg::ready() {
        return true;
    }
    // 检测dll是否需要更新
    if std::fs::read(APO_DLL_PATH).ok().as_deref() != Some(APO_DLL) {
        return true;
    }

    // 检测注册表是否需要更新
    for (reg_path, expected_value) in COMMON_STRING_REG_PAIRS {
        if reg_get_value::<String>(reg_path).ok().as_deref() != Some(expected_value) {
            return true;
        }
    }
    for (reg_path, expected_value) in COMMON_U32_REG_PAIRS {
        if reg_get_value::<u32>(reg_path).ok() != Some(expected_value) {
            return true;
        }
    }
    // 检测设备FxProperties是否需要更新
    if reg_get_value::<String>(&device_fx_property_reg_path(device_guid, SFX_GUID))
        .ok()
        .as_deref()
        != Some(VOICE_BOX_GUID)
    {
        return true;
    }
    if reg_get_value::<Vec<String>>(&device_fx_property_reg_path(
        device_guid,
        SFX_PROCESSING_MODES,
    ))
    .ok()
    .as_ref()
        != Some(&processing_modes())
    {
        return true;
    }
    if reg_get_value::<u32>(&device_fx_property_reg_path(device_guid, DISABLE_APO_GUID)).is_ok() {
        return true;
    }

    false
}

fn elevated_reg(device_guid: &str) -> anyhow::Result<()> {
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
    let registration = elevated_reg_inner(device_guid);

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
    crate::config_reg::initialize()?;
    // The previous backup location is no longer used; do not migrate it.
    let root = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE).open_subkey_with_flags(
        crate::settings::ROOT,
        winreg::enums::KEY_READ | winreg::enums::KEY_WRITE | winreg::enums::KEY_WOW64_64KEY,
    )?;
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
    if let Err(error) = root.delete_subkey_all(device_guid)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return Err(error.into());
    }
    // 更新dll
    if std::fs::read(APO_DLL_PATH).ok().as_deref() != Some(APO_DLL) {
        std::fs::write(APO_DLL_PATH, APO_DLL)?;
        info!("Updated APO DLL at {}", APO_DLL_PATH);
    }

    // 更新注册表
    for (reg_path, expected_value) in COMMON_STRING_REG_PAIRS {
        if reg_get_value::<String>(reg_path).ok().as_deref() != Some(expected_value) {
            reg_set_value(reg_path, &expected_value)?;
            info!("Updated registry key {} to {}", reg_path, expected_value);
        }
    }
    for (reg_path, expected_value) in COMMON_U32_REG_PAIRS {
        if reg_get_value::<u32>(reg_path).ok() != Some(expected_value) {
            reg_set_value(reg_path, &expected_value)?;
            info!("Updated registry key {} to {}", reg_path, expected_value);
        }
    }

    // 更新设备FxProperties
    let device_fx_reg_path = device_fx_property_reg_path(device_guid, SFX_GUID);
    let maybe_old_fx_guid: Option<String> = reg_get_optional_value(&device_fx_reg_path)?;
    if maybe_old_fx_guid.as_deref() != Some(VOICE_BOX_GUID) {
        if let Some(old_fx_guid) = maybe_old_fx_guid
            && !old_fx_guid.is_empty()
        {
            reg_set_value(
                &device_fx_property_backup_reg_path(device_guid, SFX_GUID),
                &old_fx_guid,
            )?;
            info!(
                "Backed up old SFX GUID {} for device {}",
                old_fx_guid, device_guid
            );
        }
        reg_set_value(&device_fx_reg_path, &VOICE_BOX_GUID)?;
        info!(
            "Updated device {} SFX GUID to {}",
            device_guid, VOICE_BOX_GUID
        );
    }

    let modes_path = device_fx_property_reg_path(device_guid, SFX_PROCESSING_MODES);
    let modes = processing_modes();
    if reg_get_optional_value::<Vec<String>>(&modes_path)?.as_ref() != Some(&modes) {
        let backup_path = device_fx_property_backup_reg_path(device_guid, SFX_PROCESSING_MODES);
        let marker_path = format!("{backup_path}__present");
        if reg_get_optional_value::<u32>(&marker_path)?.is_none() {
            let old_modes = reg_get_optional_value::<Vec<String>>(&modes_path)?;
            if let Some(ref old_modes) = old_modes {
                reg_set_value(&backup_path, old_modes)?;
            }
            reg_set_value(&marker_path, &u32::from(old_modes.is_some()))?;
        }
        reg_set_value(&modes_path, &modes)?;
        info!(
            "Updated SFX processing modes for device {} (RAW excluded)",
            device_guid
        );
    }

    let device_disable_apo_reg_path = device_fx_property_reg_path(device_guid, DISABLE_APO_GUID);
    if reg_get_value::<u32>(&device_disable_apo_reg_path).is_ok() {
        reg_delete_value(&device_disable_apo_reg_path)?;
        info!(
            "Removed device {} DisableProtectedAudioDG property",
            device_guid
        );
    }

    Ok(())
}

pub fn maybe_elevated_reg() -> anyhow::Result<bool> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(ELEVATED_REG_STR) {
        info!("Running elevated registration operation");
        unsafe { std::env::set_var("RUST_BACKTRACE", "1") };
        anyhow::ensure!(
            is_elevated(),
            "Elevated operation requested but not running as administrator"
        );
        let device_guid = args.get(2).ok_or_else(|| {
            anyhow::anyhow!("Missing device GUID argument for elevated registration operation")
        })?;
        elevated_reg(device_guid)?;
        Ok(true)
    } else {
        info!("Running VoiceBox");
        Ok(false)
    }
}

pub fn ensure_device_reg(device_guid: &str) -> anyhow::Result<()> {
    if need_reg(device_guid) {
        if is_elevated() {
            elevated_reg(device_guid)?;
        } else {
            let log_enable_arg = if std::env::var_os(LOG_ENABLE_STR).is_some() {
                LOG_ENABLE_STR
            } else {
                ""
            };
            let status = runas::Command::new(std::env::current_exe()?)
                .arg(ELEVATED_REG_STR)
                .arg(device_guid)
                .arg(log_enable_arg)
                .gui(false)
                .show(false)
                .status()?;
            anyhow::ensure!(
                status.success(),
                "Failed to execute elevated operation: {:?}",
                status
            );
        }
    }
    Ok(())
}

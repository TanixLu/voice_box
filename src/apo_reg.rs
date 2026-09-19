use is_elevated::is_elevated;
use restart_manager::{RestartSession, ShutdownOptions};
use tracing::info;

use crate::utils::{LOG_ENABLE_STR, reg_delete_value, reg_get_value, reg_set_value};

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

const COMMON_STRING_REG_PAIRS: &[(&str, &str)] = &[
    (COM_REG_BASE, "VoiceBox APO"),
    (COM_REG_INPROC, APO_DLL_PATH),
    (COM_REG_THREADING_MODEL, "Both"),
    (APO_REG_FRIENDLY_NAME, "VoiceBox APO"),
    (APO_REG_COPYRIGHT, "Copyright (C) 2026 Tanix"),
    (APO_REG_INTERFACE, APO_INTERFACE_IID),
];

const COMMON_U32_REG_PAIRS: &[(&str, u32)] = &[
    (DISABLE_PROTECTED_AUDIO_DG_REG_PATH, 1),
    (APO_REG_MAJOR_VERSION, 1),
    (APO_REG_MINOR_VERSION, 0),
    (APO_REG_NUM_APO_INTERFACES, 1),
    (APO_REG_MIN_INPUT_CONNECTIONS, 1),
    (APO_REG_MAX_INPUT_CONNECTIONS, 1),
    (APO_REG_MIN_OUTPUT_CONNECTIONS, 1),
    (APO_REG_MAX_OUTPUT_CONNECTIONS, 1),
    (APO_REG_MAX_INSTANCES, u32::MAX),
    (APO_REG_FLAGS, 0b00001101),
];

const EFFECT_CLSID: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d}";
const SFX_GUID: &str = constcat::concat!(EFFECT_CLSID, ",5");
// const MFX_GUID: &str = constcat::concat!(EFFECT_CLSID, ",6");
// const EFX_GUID: &str = constcat::concat!(EFFECT_CLSID, ",7");
// const LFX_GUID: &str = constcat::concat!(EFFECT_CLSID, ",1");
// const GFX_GUID: &str = constcat::concat!(EFFECT_CLSID, ",2");

const DISABLE_APO_GUID: &str = "{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},5";

fn device_fx_property_reg_path(device_guid: &str, property: &str) -> String {
    format!(
        r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture\{device_guid}\FxProperties\{property}"
    )
}

fn device_fx_property_backup_reg_path(device_guid: &str, property: &str) -> String {
    format!(r"HKLM\SOFTWARE\VoiceBox\{device_guid}\{property}")
}

const APO_DLL: &[u8] = include_bytes!("../target/voice_box.dll");

const APO_DLL_PATH: &str = r"C:\Windows\System32\voice_box.dll";
const AUDIO_SERVICE_NAME: &str = "AudioSrv";

fn need_reg(device_guid: &str) -> bool {
    // 检测dll是否需要更新
    let dll_bytes = std::fs::read(APO_DLL_PATH).unwrap_or_default();
    if dll_bytes != APO_DLL {
        return true;
    }

    // 检测注册表是否需要更新
    for (reg_path, expected_value) in COMMON_STRING_REG_PAIRS {
        let actual_value: String = reg_get_value(reg_path).unwrap_or_default();
        if actual_value != *expected_value {
            return true;
        }
    }
    for (reg_path, expected_value) in COMMON_U32_REG_PAIRS {
        let actual_value: u32 = reg_get_value(reg_path).unwrap_or_default();
        if actual_value != *expected_value {
            return true;
        }
    }
    // 检测设备FxProperties是否需要更新
    let old_sfx_guid: String =
        reg_get_value(&device_fx_property_reg_path(device_guid, SFX_GUID)).unwrap_or_default();
    if old_sfx_guid != VOICE_BOX_GUID {
        return true;
    }
    if reg_get_value::<u32>(&device_fx_property_reg_path(device_guid, DISABLE_APO_GUID))
        .unwrap_or_default()
        != 0
    {
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
    session.register_files([APO_DLL_PATH])?;
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

    elevated_reg_inner(device_guid)?;

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
    Ok(())
}

fn elevated_reg_inner(device_guid: &str) -> anyhow::Result<()> {
    // 更新dll
    let dll_bytes = std::fs::read(APO_DLL_PATH).unwrap_or_default();
    if dll_bytes != APO_DLL {
        std::fs::write(APO_DLL_PATH, APO_DLL)?;
    }

    // 更新注册表
    for (reg_path, expected_value) in COMMON_STRING_REG_PAIRS {
        let actual_value: String = reg_get_value(reg_path).unwrap_or_default();
        if actual_value != *expected_value {
            reg_set_value(reg_path, expected_value)?;
        }
    }
    for (reg_path, expected_value) in COMMON_U32_REG_PAIRS {
        let actual_value: u32 = reg_get_value(reg_path).unwrap_or_default();
        if actual_value != *expected_value {
            reg_set_value(reg_path, expected_value)?;
        }
    }

    // 更新设备FxProperties
    let device_sfx_reg_path = device_fx_property_reg_path(device_guid, SFX_GUID);
    let old_sfx_guid: String = reg_get_value(&device_sfx_reg_path).unwrap_or_default();
    if old_sfx_guid != VOICE_BOX_GUID {
        if !old_sfx_guid.is_empty() {
            reg_set_value(
                &device_fx_property_backup_reg_path(device_guid, SFX_GUID),
                &old_sfx_guid,
            )?;
        }
        reg_set_value(&device_sfx_reg_path, &VOICE_BOX_GUID)?;
    }

    let device_disable_apo_reg_path = device_fx_property_reg_path(device_guid, DISABLE_APO_GUID);
    if reg_get_value::<u32>(&device_disable_apo_reg_path).unwrap_or_default() != 0 {
        reg_delete_value(&device_disable_apo_reg_path)?;
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

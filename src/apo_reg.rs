use anyhow::Context;
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
use winreg::{
    RegKey,
    enums::{HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_READ, KEY_SET_VALUE, KEY_WOW64_64KEY},
    transaction::Transaction,
    types::FromRegValue,
};

use crate::utils::{LOG_ENABLE_STR, reg_get_value, reg_set_value};

const ELEVATED_REG_STR: &str = "--elevated-reg";
const ELEVATED_UNREG_STR: &str = "--elevated-unreg";

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

// Append VoiceBox to the composite SFX list without replacing other APOs.
const COMPOSITE_SFX_GUID: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},13";
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

fn contains_guid(values: &[String], guid: &str) -> bool {
    values.iter().any(|value| value.eq_ignore_ascii_case(guid))
}

const APO_DLL: &[u8] = include_bytes!("../target/voice_box.dll");

const APO_DLL_PATH: &str = r"C:\Windows\System32\voice_box.dll";
const AUDIO_SERVICE_NAME: &str = "AudioSrv";

fn need_reg(device_guid: &str) -> bool {
    let root = RegKey::predef(HKEY_LOCAL_MACHINE);
    let fx_path = format!(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture\{device_guid}\FxProperties"
    );
    let Ok(fx) = root.open_subkey_with_flags(fx_path, KEY_READ | KEY_WOW64_64KEY) else {
        return true;
    };
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
    if !fx
        .get_value::<Vec<String>, _>(COMPOSITE_SFX_GUID)
        .is_ok_and(|effects| contains_guid(&effects, VOICE_BOX_GUID))
    {
        return true;
    }
    if !fx
        .get_value::<Vec<String>, _>(SFX_PROCESSING_MODES)
        .is_ok_and(|modes| {
            processing_modes()
                .iter()
                .all(|mode| contains_guid(&modes, mode))
        })
    {
        return true;
    }
    if fx
        .get_value::<u32, _>(DISABLE_APO_GUID)
        .is_ok_and(|value| value != 0)
    {
        return true;
    }

    false
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
    crate::config_reg::initialize().context("初始化 VoiceBox 配置注册表失败")?;
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
    // Only an absent value means an empty list. Refuse to overwrite invalid types
    // or unreadable values, since they may contain another APO's configuration.
    let mut effects: Vec<String> = optional_value(fx, COMPOSITE_SFX_GUID)
        .context("读取 Composite SFX APO 列表失败")?
        .unwrap_or_default();
    if uninstall {
        let count = effects.len();
        effects.retain(|effect| !effect.eq_ignore_ascii_case(VOICE_BOX_GUID));
        if effects.len() != count {
            if effects.is_empty() {
                delete_optional(fx, COMPOSITE_SFX_GUID)?;
            } else {
                fx.set_value(COMPOSITE_SFX_GUID, &effects)?;
            }
        }
    } else {
        if !contains_guid(&effects, VOICE_BOX_GUID) {
            effects.push(VOICE_BOX_GUID.into());
            fx.set_value(COMPOSITE_SFX_GUID, &effects)?;
        }
        let mut modes: Vec<String> = optional_value(fx, SFX_PROCESSING_MODES)
            .context("读取 SFX 处理模式失败")?
            .unwrap_or_default();
        for mode in processing_modes() {
            if !contains_guid(&modes, &mode) {
                modes.push(mode);
            }
        }
        fx.set_value(SFX_PROCESSING_MODES, &modes)?;
        // Preserve the existing registration behavior of enabling system effects.
        delete_optional(fx, DISABLE_APO_GUID)?;
    }
    Ok(())
}
pub fn maybe_elevated_reg() -> anyhow::Result<bool> {
    let args: Vec<String> = std::env::args().collect();
    if matches!(
        args.get(1).map(String::as_str),
        Some(ELEVATED_REG_STR | ELEVATED_UNREG_STR)
    ) {
        info!("Running elevated registration operation");
        unsafe { std::env::set_var("RUST_BACKTRACE", "1") };
        anyhow::ensure!(
            is_elevated(),
            "Elevated operation requested but not running as administrator"
        );
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
        info!("Running VoiceBox");
        Ok(false)
    }
}

pub fn ensure_device_reg(device_guid: &str) -> anyhow::Result<()> {
    if need_reg(device_guid) {
        device_operation(device_guid, false)?;
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
        let log_enable_arg = if std::env::var_os(LOG_ENABLE_STR).is_some() {
            LOG_ENABLE_STR
        } else {
            ""
        };
        let status = runas::Command::new(std::env::current_exe()?)
            .arg(if uninstall {
                ELEVATED_UNREG_STR
            } else {
                ELEVATED_REG_STR
            })
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
    Ok(())
}

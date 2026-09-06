mod utils;

use is_elevated::is_elevated;
use serde::{Deserialize, Serialize};
use wasapi::{DeviceEnumerator, Direction, initialize_mta};

use crate::{
    ElevatedOperation::DisableProtectedAudioDG,
    utils::{reg_get_value, reg_set_value, reg_subkey_exists},
};

const ELEVATED_STR: &str = "--elevated";

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
const APO_REG_FRIENDLY_NAME: &str = constcat::concat!(APO_REG_INTERFACE, r"FriendlyName");
const APO_REG_COPYRIGHT: &str = constcat::concat!(APO_REG_INTERFACE, r"Copyright");
const APO_REG_INTERFACE: &str = constcat::concat!(APO_REG_BASE, r"APOInterface0");
const APO_REG_MAJOR_VERSION: &str = constcat::concat!(APO_REG_INTERFACE, r"MajorVersion");
const APO_REG_MINOR_VERSION: &str = constcat::concat!(APO_REG_INTERFACE, r"MinorVersion");
const APO_REG_NUM_APO_INTERFACES: &str = constcat::concat!(APO_REG_INTERFACE, r"NumAPOInterfaces");
const APO_REG_MIN_INPUT_CONNECTIONS: &str =
    constcat::concat!(APO_REG_INTERFACE, r"MinInputConnections");
const APO_REG_MAX_INPUT_CONNECTIONS: &str =
    constcat::concat!(APO_REG_INTERFACE, r"MaxInputConnections");
const APO_REG_MIN_OUTPUT_CONNECTIONS: &str =
    constcat::concat!(APO_REG_INTERFACE, r"MinOutputConnections");
const APO_REG_MAX_OUTPUT_CONNECTIONS: &str =
    constcat::concat!(APO_REG_INTERFACE, r"MaxOutputConnections");
const APO_REG_MAX_INSTANCES: &str = constcat::concat!(APO_REG_INTERFACE, r"MaxInstances");
const APO_REG_FLAGS: &str = constcat::concat!(APO_REG_INTERFACE, r"Flags");

const APO_INTERFACE_IID: &str = "{FD7F2B29-24D0-4B5C-B177-592C39F9CA10}";

const APO_DLL: &[u8] = include_bytes!("../target/voice_box.dll");

fn main() -> anyhow::Result<()> {
    let s: String = reg_get_value(
        r"HKCR\CLSID\{EACD2258-FCAC-4FF4-B36D-419E924A6D79}\InprocServer32\ThreadingModel",
    )?;
    println!("Value: {}", s);
    return Ok(());

    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 && args[1] == ELEVATED_STR {
        anyhow::ensure!(
            is_elevated(),
            "Elevated operation requested but not running as administrator"
        );
        anyhow::ensure!(
            args.len() == 3,
            "Elevated operation requested but no operation specified"
        );
        let operation: ElevatedOperation = serde_json::from_str(args[2].as_str())?;
        operation.execute()
    } else {
        normal_main()
    }
}

fn normal_main() -> anyhow::Result<()> {
    initialize_mta().unwrap();

    let enumerator = DeviceEnumerator::new()?;

    let default_device = enumerator.get_default_device(&Direction::Capture)?;

    for device in &enumerator.get_device_collection(&Direction::Capture)? {
        let device = device?;
        print!(
            "Device: {}, {}",
            device.get_friendlyname()?,
            device.get_id()?,
        );
        if device.get_id()? == default_device.get_id()? {
            println!(" [default]");
        } else {
            println!();
        }
    }

    if reg_get_value(DISABLE_PROTECTED_AUDIO_DG_REG_PATH).unwrap_or(0) == 0u32 {
        DisableProtectedAudioDG(true).execute()?;
    }

    println!("APO size: {} bytes", APO_DLL.len());

    if reg_subkey_exists(COM_REG_BASE).unwrap_or(false)
        || reg_subkey_exists(APO_REG_BASE).unwrap_or(false)
    {
        reg_com_apo()?;
    }

    Ok(())
}

fn reg_com_apo() -> anyhow::Result<()> {
    // 需要管理员权限，将内嵌在自身的APO_DLL解压到C:\Windows\System32，然后注册
    // 要注意有可能C:\Windows\System32\voice_box.dll已经存在，可能是之前的版本
    // 需要先在注册表中删除COM和APO的注册信息，重启audiodg.exe，然后再覆盖C:\Windows\System32\voice_box.dll
    // 最后重新注册才行
    // let dll_path = std::env::current_exe()?;
    // let status = std::process::Command::new("regsvr32.exe")
    //     .arg("/s")
    //     .arg("arg");
    // todo!();

    reg_set_value(COM_REG_BASE, &"VoiceBox APO")?;
    reg_set_value(COM_REG_INPROC, &r"C:\Windows\System32\voice_box.dll")?;
    reg_set_value(COM_REG_THREADING_MODEL, &"Both")?;

    reg_set_value(APO_REG_FRIENDLY_NAME, &"VoiceBox APO")?;
    reg_set_value(APO_REG_COPYRIGHT, &"Copyright (C) 2026")?;
    reg_set_value(APO_REG_INTERFACE, &APO_INTERFACE_IID)?;
    reg_set_value(APO_REG_MAJOR_VERSION, &1u32)?;
    reg_set_value(APO_REG_MINOR_VERSION, &0u32)?;
    reg_set_value(APO_REG_NUM_APO_INTERFACES, &1u32)?;
    reg_set_value(APO_REG_MIN_INPUT_CONNECTIONS, &1u32)?;
    reg_set_value(APO_REG_MAX_INPUT_CONNECTIONS, &1u32)?;
    reg_set_value(APO_REG_MIN_OUTPUT_CONNECTIONS, &1u32)?;
    reg_set_value(APO_REG_MAX_OUTPUT_CONNECTIONS, &1u32)?;
    reg_set_value(APO_REG_MAX_INSTANCES, &u32::MAX)?;
    reg_set_value(APO_REG_FLAGS, &13u32)?;

    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum ElevatedOperation {
    DisableProtectedAudioDG(bool),
}

impl ElevatedOperation {
    fn execute(self) -> anyhow::Result<()> {
        if !is_elevated() {
            let status = runas::Command::new(std::env::current_exe()?)
                .arg(ELEVATED_STR)
                .arg(serde_json::to_string(&self)?)
                .status()?;
            anyhow::ensure!(
                status.success(),
                "Failed to execute elevated operation: {:?}",
                self
            );
            return Ok(());
        }

        match self {
            ElevatedOperation::DisableProtectedAudioDG(disable) => {
                reg_set_value(
                    DISABLE_PROTECTED_AUDIO_DG_REG_PATH,
                    &(if disable { 1u32 } else { 0u32 }),
                )?;
            }
        }

        Ok(())
    }
}

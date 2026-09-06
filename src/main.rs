mod utils;

use is_elevated::is_elevated;
use serde::{Deserialize, Serialize};
use wasapi::{DeviceEnumerator, Direction, initialize_mta};

use crate::{
    ElevatedOperation::DisableProtectedAudioDG,
    utils::{get_reg_value, reg_subkey_exists, set_reg_value},
};

const ELEVATED_STR: &str = "--elevated";
const VOICE_BOX_GUID: &str = "{AE672075-B244-4417-B5A5-FBD45B7F0268}";
const DISABLE_PROTECTED_AUDIO_DG_REG_PATH: &str =
    r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio\DisableProtectedAudioDG";
const COM_REG_PATH: &str = constcat::concat!(r"HKCR\CLSID\", VOICE_BOX_GUID, r"\");
const APO_REG_PATH: &str = constcat::concat!(
    r"HKCR\AudioEngine\AudioProcessingObjects\",
    VOICE_BOX_GUID,
    r"\"
);

fn main() -> anyhow::Result<()> {
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

    if get_reg_value(DISABLE_PROTECTED_AUDIO_DG_REG_PATH).unwrap_or(0) == 0u32 {
        DisableProtectedAudioDG(true).execute()?;
    }

    dbg!(reg_subkey_exists(COM_REG_PATH));

    if reg_subkey_exists(COM_REG_PATH).unwrap_or(false)
        || reg_subkey_exists(APO_REG_PATH).unwrap_or(false)
    {
        // reg_com_apo()?;
    }

    Ok(())
}

fn reg_com_apo() -> anyhow::Result<()> {
    todo!();
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
                set_reg_value(
                    DISABLE_PROTECTED_AUDIO_DG_REG_PATH,
                    &(if disable { 1u32 } else { 0u32 }),
                )?;
            }
        }

        Ok(())
    }
}

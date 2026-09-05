use is_elevated::is_elevated;
use serde::{Deserialize, Serialize};
use wasapi::{DeviceEnumerator, Direction, initialize_mta};
use windows_registry::LOCAL_MACHINE;

use crate::ElevatedOperation::DisableProtectedAudioDG;

const ELEVATED_STR: &str = "--elevated";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 3 && args[1] == ELEVATED_STR {
        anyhow::ensure!(
            is_elevated(),
            "Elevated operation requested but not running as administrator"
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

    DisableProtectedAudioDG(true).execute()?;

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
                const PATH: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Audio";
                const NAME: &str = "DisableProtectedAudioDG";
                let key = LOCAL_MACHINE.options().write().wow64_64().open(PATH)?;
                key.set_u32(NAME, if disable { 1 } else { 0 })?;
            }
        }

        Ok(())
    }
}

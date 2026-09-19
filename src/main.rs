mod apo_reg;
mod utils;

use tracing::{error, info};
use wasapi::{DeviceEnumerator, Direction, initialize_mta};

use crate::{
    apo_reg::{ensure_device_reg, maybe_elevated_reg},
    utils::init_logging,
};

fn main() -> anyhow::Result<()> {
    init_logging();

    match maybe_elevated_reg() {
        Ok(true) => {
            info!("Elevated registration operation completed successfully");
            Ok(())
        }
        Ok(false) => match voice_box() {
            Ok(_) => {
                info!("VoiceBox stopped successfully");
                Ok(())
            }
            Err(e) => {
                error!("Error during VoiceBox: {:?}", e);
                Err(e)
            }
        },
        Err(e) => {
            error!("Error during registration: {:?}", e);
            Err(e)
        }
    }
}

fn voice_box() -> anyhow::Result<()> {
    initialize_mta().unwrap();

    let enumerator = DeviceEnumerator::new()?;

    let default_device = enumerator.get_default_device(&Direction::Capture)?;

    for device in &enumerator.get_device_collection(&Direction::Capture)? {
        let device = device?;
        info!(
            "Device: {}, {} {}",
            device.get_friendlyname()?,
            device.get_id()?,
            if device.get_id()? == default_device.get_id()? {
                "[default]"
            } else {
                ""
            }
        );
    }

    let default_device_guid = default_device
        .get_id()?
        .rsplit(".")
        .next()
        .unwrap()
        .to_owned();

    ensure_device_reg(&default_device_guid)?;

    Ok(())
}

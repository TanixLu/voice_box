mod apo_reg;
mod utils;

use tracing::{error, info};
use wasapi::{DeviceEnumerator, Direction, initialize_mta};

use crate::{
    apo_reg::{ensure_device_reg, maybe_elevated_reg},
    utils::{check_single_instance, init_logging},
};

fn main() {
    init_logging();

    if let Err(e) = run() {
        error!("Application error: {e:?}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    if maybe_elevated_reg()? {
        info!("Elevated registration operation completed successfully");
        return Ok(());
    }

    let _single_instance = check_single_instance()?;

    voice_box()?;
    info!("VoiceBox stopped successfully");

    Ok(())
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

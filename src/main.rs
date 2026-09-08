mod apo_reg;
mod utils;

use wasapi::{DeviceEnumerator, Direction, initialize_mta};

use crate::apo_reg::{ensure_device_reg, maybe_elevated_reg};

fn main() -> anyhow::Result<()> {
    if maybe_elevated_reg()? {
        Ok(())
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

    let default_device_guid = default_device
        .get_id()?
        .rsplit(".")
        .next()
        .unwrap()
        .to_owned();

    ensure_device_reg(&default_device_guid)?;

    Ok(())
}

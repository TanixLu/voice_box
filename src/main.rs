use wasapi::{DeviceEnumerator, Direction, initialize_mta};

fn main() -> anyhow::Result<()> {
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

    Ok(())
}

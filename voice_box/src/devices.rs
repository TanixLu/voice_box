//! Audio endpoint discovery, independent of the GUI and of each feature's rules.

use wasapi::{DeviceEnumerator, DeviceState, Direction};

use crate::{
    i18n::Text,
    plugin::{self, PluginStatus},
    win::ComGuard,
};

pub struct Microphone {
    pub name: String,
    pub is_default: bool,
    pub device_id: String,
    pub guid: String,
    pub plugin_status: PluginStatus,
}

pub struct Speaker {
    pub name: String,
    pub device_id: String,
    pub is_default: bool,
}

/// The microphones, with the index of the default one.
pub fn enumerate_microphones() -> anyhow::Result<(Vec<Microphone>, usize)> {
    let _com_guard = ComGuard::mta()?;
    let enumerator = DeviceEnumerator::new()?;
    let default_id = enumerator
        .get_default_device(&Direction::Capture)
        .and_then(|device| device.get_id())
        .ok();
    let mut microphones = Vec::new();
    let mut selected = 0;
    for device in &enumerator.get_device_collection(&Direction::Capture)? {
        let device = device?;
        let id = device.get_id()?;
        let guid = endpoint_guid(&id)?.to_owned();
        let is_default = default_id.as_deref() == Some(&id);
        if is_default {
            selected = microphones.len();
        }
        let name = device.get_friendlyname()?;
        microphones.push(Microphone {
            device_id: id,
            name,
            is_default,
            plugin_status: plugin::device_status(&guid),
            guid,
        });
    }
    tracing::info!(
        count = microphones.len(),
        default_device_id = ?default_id,
        "Microphone discovery completed"
    );
    Ok((microphones, selected))
}

/// Every microphone and speaker.
pub fn enumerate_all() -> anyhow::Result<(Vec<Microphone>, Vec<Speaker>)> {
    let (microphones, _) = enumerate_microphones()?;
    let _com_guard = ComGuard::mta()?;
    let enumerator = DeviceEnumerator::new()?;
    let default_id = enumerator
        .get_default_device(&Direction::Render)
        .and_then(|device| device.get_id())
        .ok();
    let collection = enumerator.get_device_collection(&Direction::Render)?;
    let speakers = (&collection)
        .into_iter()
        .map(|device| {
            let device = device?;
            let id = device.get_id()?;
            Ok(Speaker {
                name: device.get_friendlyname()?,
                is_default: default_id.as_deref() == Some(&id),
                device_id: id,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok((microphones, speakers))
}

/// The GUID of microphone `device_id`, or of the default one, which must be active.
pub fn microphone_guid(device_id: Option<&str>) -> anyhow::Result<String> {
    let _com_guard = ComGuard::mta()?;
    let enumerator = DeviceEnumerator::new()?;
    let device = match device_id {
        Some(id) => enumerator.get_device(id)?,
        None => enumerator.get_default_device(&Direction::Capture)?,
    };
    anyhow::ensure!(
        device.get_direction() == Direction::Capture,
        "Invalid microphone ID"
    );
    anyhow::ensure!(
        device.get_state()? == DeviceState::Active,
        Text::OutputDeviceUnavailable
    );
    Ok(endpoint_guid(&device.get_id()?)?.to_owned())
}

/// An endpoint ID ends with the GUID that keys the endpoint's registry entries.
fn endpoint_guid(id: &str) -> anyhow::Result<&str> {
    id.rsplit('.')
        .next()
        .ok_or_else(|| anyhow::anyhow!("Invalid microphone ID"))
}

//! Microphone discovery and plugin operations, independent of the GUI.

use wasapi::{DeviceEnumerator, DeviceState, Direction};

use voice_box_shared::error::SharedError;

use crate::{
    audio::Apartment,
    setup::{self, PluginStatus},
};

pub struct Speaker {
    pub name: String,
    pub device_id: String,
    pub is_default: bool,
}

pub fn enumerate_soundboard_devices() -> anyhow::Result<(Vec<Microphone>, Vec<Speaker>)> {
    let (microphones, _) = enumerate_microphones()?;
    let _apartment = Apartment::new()?;
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

pub fn soundboard_microphone_guid(device_id: Option<&str>) -> anyhow::Result<String> {
    let _apartment = Apartment::new()?;
    let enumerator = DeviceEnumerator::new()?;
    let device = match device_id {
        Some(id) => enumerator.get_device(id)?,
        None => enumerator.get_default_device(&Direction::Capture)?,
    };
    anyhow::ensure!(
        device.get_direction() == Direction::Capture,
        SharedError::InvalidMicrophoneId
    );
    anyhow::ensure!(
        device.get_state()? == DeviceState::Active,
        crate::i18n::Text::OutputDeviceUnavailable
    );
    let id = device.get_id()?;
    let guid = id
        .rsplit('.')
        .next()
        .ok_or_else(|| anyhow::anyhow!(SharedError::InvalidMicrophoneId))?;
    anyhow::ensure!(
        setup::device_plugin_status(guid) == PluginStatus::Installed,
        crate::i18n::Text::SoundboardMicrophonePluginRequired
    );
    Ok(guid.to_owned())
}

pub struct Microphone {
    pub name: String,
    pub is_default: bool,
    pub device_id: String,
    pub guid: String,
    pub plugin_status: PluginStatus,
}

#[derive(Debug)]
pub enum PluginAction {
    Install,
    Uninstall,
}

pub struct PluginOperationResult {
    pub result: anyhow::Result<()>,
    pub statuses: Vec<(String, PluginStatus)>,
}

pub fn enumerate_microphones() -> anyhow::Result<(Vec<Microphone>, usize)> {
    let _apartment = Apartment::new()?;
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
        let guid = id
            .rsplit('.')
            .next()
            .ok_or_else(|| anyhow::anyhow!(SharedError::InvalidMicrophoneId))?
            .to_owned();
        let is_default = default_id.as_deref() == Some(&id);
        if is_default {
            selected = microphones.len();
        }
        let name = device.get_friendlyname()?;
        microphones.push(Microphone {
            device_id: id,
            name,
            is_default,
            plugin_status: setup::device_plugin_status(&guid),
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

pub fn execute_plugin_action(
    guid: String,
    guids: Vec<String>,
    action: PluginAction,
) -> PluginOperationResult {
    tracing::info!(?action, %guid, "Starting microphone plugin operation");
    let result = if matches!(action, PluginAction::Uninstall) {
        setup::uninstall_device_plugin(&guid)
    } else {
        setup::ensure_device_plugin_installed(&guid)
    };
    // Recheck even after failure: the operation can have partially succeeded. All
    // DLL and registry reads stay in the background worker.
    let statuses = guids
        .into_iter()
        .map(|guid| {
            let status = setup::device_plugin_status(&guid);
            (guid, status)
        })
        .collect();

    PluginOperationResult { result, statuses }
}

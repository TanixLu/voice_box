//! Microphone discovery and plugin operations, independent of the GUI.
use super::{
    audio::Apartment,
    setup::{self, PluginStatus},
};
use wasapi::{DeviceEnumerator, Direction};

pub(super) struct Microphone {
    pub name: String,
    pub device_id: String,
    pub guid: String,
    pub plugin_status: PluginStatus,
}

pub(super) enum PluginAction {
    Install,
    Uninstall,
}

pub(super) struct PluginOperationResult {
    pub result: anyhow::Result<()>,
    pub statuses: Vec<(String, PluginStatus)>,
}

pub(super) fn enumerate_microphones() -> anyhow::Result<(Vec<Microphone>, usize)> {
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
            .ok_or_else(|| anyhow::anyhow!("麦克风标识无效"))?
            .to_owned();
        let is_default = default_id.as_deref() == Some(&id);
        if is_default {
            selected = microphones.len();
        }
        let name = device.get_friendlyname()?;
        microphones.push(Microphone {
            device_id: id,
            name: if is_default {
                format!("{name}（默认）")
            } else {
                name
            },
            plugin_status: setup::device_plugin_status(&guid),
            guid,
        });
    }
    Ok((microphones, selected))
}

pub(super) fn execute_plugin_action(
    guid: String,
    guids: Vec<String>,
    action: PluginAction,
) -> PluginOperationResult {
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

use std::{marker::PhantomData, rc::Rc};

use windows::Win32::{
    Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
    Media::Audio::{
        DEVICE_STATE_ACTIVE, EDataFlow, IMMDevice, IMMDeviceCollection, IMMDeviceEnumerator,
        MMDeviceEnumerator, eCapture, eConsole, eRender,
    },
    System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
        STGM_READ,
    },
};

pub struct ComMtaGuard {
    _not_send_sync_marker: PhantomData<Rc<()>>,
}

impl Drop for ComMtaGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}

pub fn init_com_mta() -> anyhow::Result<ComMtaGuard> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
    Ok(ComMtaGuard {
        _not_send_sync_marker: PhantomData,
    })
}

pub struct AudioDevice {
    device: IMMDevice,
}

impl AudioDevice {
    pub fn get_id(&self) -> anyhow::Result<String> {
        let id = unsafe { self.device.GetId()?.to_string()? };
        Ok(id)
    }

    pub fn get_friendly_name(&self) -> anyhow::Result<String> {
        let property_store = unsafe { self.device.OpenPropertyStore(STGM_READ)? };
        let name = unsafe { property_store.GetValue(&PKEY_Device_FriendlyName)? };
        Ok(name.to_string())
    }
}

/// Get active audio devices (render or capture) and return them in a vector, with the default device first
fn get_active_audio_devices(e_data_flow: EDataFlow) -> anyhow::Result<Vec<AudioDevice>> {
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    let device_collection: IMMDeviceCollection =
        unsafe { enumerator.EnumAudioEndpoints(e_data_flow, DEVICE_STATE_ACTIVE)? };
    let count = unsafe { device_collection.GetCount()? };
    let mut devices = Vec::with_capacity(count as usize);
    for i in 0..count {
        let device = unsafe { device_collection.Item(i)? };
        devices.push(AudioDevice { device });
    }
    let default_device = AudioDevice {
        device: unsafe { enumerator.GetDefaultAudioEndpoint(e_data_flow, eConsole)? },
    };
    let default_device_id = default_device.get_id()?;
    for i in 0..count {
        if devices[i as usize].get_id()? == default_device_id {
            println!(
                "Default device: {}",
                devices[i as usize].get_friendly_name()?
            );
            devices.swap(0, i as usize);
            break;
        }
    }
    Ok(devices)
}

pub fn get_active_render_devices() -> anyhow::Result<Vec<AudioDevice>> {
    get_active_audio_devices(eRender)
}

pub fn get_active_capture_devices() -> anyhow::Result<Vec<AudioDevice>> {
    get_active_audio_devices(eCapture)
}

#[test]
fn test() -> anyhow::Result<()> {
    let _com_mta_guard = init_com_mta()?;
    let render_devices = get_active_render_devices()?;
    let capture_devices = get_active_capture_devices()?;

    println!("Active render devices: {}", render_devices.len());
    for device in render_devices {
        println!("  - {} ({})", device.get_friendly_name()?, device.get_id()?);
    }

    println!("Active capture devices: {}", capture_devices.len());
    for device in capture_devices {
        println!("  - {} ({})", device.get_friendly_name()?, device.get_id()?);
    }

    Ok(())
}

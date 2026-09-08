use std::{
    fmt::Debug,
    thread::sleep,
    time::{Duration, Instant},
};

use windows_service::service::{Service, ServiceState};
use winreg::{
    RegKey,
    enums::{
        HKEY_CLASSES_ROOT, HKEY_CURRENT_CONFIG, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, HKEY_USERS,
        KEY_READ, KEY_SET_VALUE,
    },
    types::{FromRegValue, ToRegValue},
};

fn split_reg_path(full_path: &str) -> anyhow::Result<(RegKey, &str, &str)> {
    let error_fn = || anyhow::anyhow!("Invalid registry full_path: {}", full_path);

    let (root_str, tail) = full_path.split_once('\\').ok_or_else(error_fn)?;

    let root = match root_str.to_uppercase().as_str() {
        "HKEY_CLASSES_ROOT" | "HKCR" => RegKey::predef(HKEY_CLASSES_ROOT),
        "HKEY_CURRENT_CONFIG" | "HKCC" => RegKey::predef(HKEY_CURRENT_CONFIG),
        "HKEY_CURRENT_USER" | "HKCU" => RegKey::predef(HKEY_CURRENT_USER),
        "HKEY_LOCAL_MACHINE" | "HKLM" => RegKey::predef(HKEY_LOCAL_MACHINE),
        "HKEY_USERS" | "HKU" => RegKey::predef(HKEY_USERS),
        _ => return Err(error_fn()),
    };

    let (subkey, name) = tail.rsplit_once('\\').ok_or_else(error_fn)?;

    Ok((root, subkey, name))
}

pub fn reg_get_value<T>(full_path: &str) -> anyhow::Result<T>
where
    T: FromRegValue,
{
    let (root, path, name) = split_reg_path(full_path)?;
    let subkey = root.open_subkey_with_flags(path, KEY_READ)?;
    let value: T = subkey.get_value(name)?;
    Ok(value)
}

pub fn reg_set_value<T>(full_path: &str, value: &T) -> anyhow::Result<()>
where
    T: ToRegValue + Debug,
{
    let (root, path, name) = split_reg_path(full_path)?;
    let (subkey, _) = root.create_subkey_with_flags(path, KEY_SET_VALUE)?;
    subkey.set_value(name, value)?;
    Ok(())
}

pub fn reg_delete_value(full_path: &str) -> anyhow::Result<()> {
    let (root, path, name) = split_reg_path(full_path)?;
    let subkey = root.open_subkey_with_flags(path, KEY_SET_VALUE)?;
    if let Err(e) = subkey.delete_value(name) {
        if e.kind() == std::io::ErrorKind::NotFound {
            return Ok(());
        } else {
            return Err(e.into());
        }
    }
    Ok(())
}

pub fn stop_service(service: &Service) -> anyhow::Result<()> {
    if service.query_status()?.current_state == ServiceState::Running {
        service.stop()?;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while service.query_status()?.current_state != ServiceState::Stopped {
        if Instant::now() > deadline {
            anyhow::bail!("Timeout waiting for audio service to stop");
        }
        sleep(Duration::from_millis(200));
    }
    Ok(())
}

pub fn start_service(service: &Service) -> windows_service::Result<()> {
    service.start::<&str>(&[])?;
    Ok(())
}

pub fn restart_servce(service: &Service) -> anyhow::Result<()> {
    stop_service(service)?;
    start_service(service)?;
    Ok(())
}

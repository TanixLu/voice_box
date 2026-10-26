use crate::app::i18n::Message;
use winreg::{
    RegKey,
    enums::{
        HKEY_CLASSES_ROOT, HKEY_CURRENT_CONFIG, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, HKEY_USERS,
        KEY_READ, KEY_SET_VALUE, KEY_WOW64_64KEY,
    },
    types::{FromRegValue, ToRegValue},
};
fn split_registry_key_path(full_path: &str) -> anyhow::Result<(RegKey, &str)> {
    let error_fn = || {
        anyhow::anyhow!(Message::InvalidRegistryPath {
            path: full_path.to_owned()
        })
    };

    let (root_str, tail) = full_path.split_once('\\').ok_or_else(error_fn)?;

    let root = match root_str.to_uppercase().as_str() {
        "HKEY_CLASSES_ROOT" | "HKCR" => RegKey::predef(HKEY_CLASSES_ROOT),
        "HKEY_CURRENT_CONFIG" | "HKCC" => RegKey::predef(HKEY_CURRENT_CONFIG),
        "HKEY_CURRENT_USER" | "HKCU" => RegKey::predef(HKEY_CURRENT_USER),
        "HKEY_LOCAL_MACHINE" | "HKLM" => RegKey::predef(HKEY_LOCAL_MACHINE),
        "HKEY_USERS" | "HKU" => RegKey::predef(HKEY_USERS),
        _ => return Err(error_fn()),
    };

    anyhow::ensure!(
        !tail.is_empty(),
        Message::InvalidRegistryPath {
            path: full_path.to_owned()
        }
    );
    Ok((root, tail))
}

fn split_registry_value_path(full_path: &str) -> anyhow::Result<(RegKey, &str, &str)> {
    let (root, tail) = split_registry_key_path(full_path)?;
    let (subkey, name) = tail.rsplit_once('\\').ok_or_else(|| {
        anyhow::anyhow!(Message::InvalidRegistryPath {
            path: full_path.to_owned()
        })
    })?;
    Ok((root, subkey, name))
}

pub fn read_registry_value<T>(full_path: &str) -> anyhow::Result<T>
where
    T: FromRegValue,
{
    let (root, path, name) = split_registry_value_path(full_path)?;
    let subkey = root.open_subkey_with_flags(path, KEY_READ | KEY_WOW64_64KEY)?;
    let value: T = subkey.get_value(name)?;
    Ok(value)
}

pub fn write_registry_value<T>(full_path: &str, value: &T) -> anyhow::Result<()>
where
    T: ToRegValue,
{
    let (root, path, name) = split_registry_value_path(full_path)?;
    let (subkey, _) = root.create_subkey_with_flags(path, KEY_SET_VALUE | KEY_WOW64_64KEY)?;
    subkey.set_value(name, value)?;
    Ok(())
}

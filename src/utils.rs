use winreg::{
    RegKey,
    enums::{
        HKEY_CLASSES_ROOT, HKEY_CURRENT_CONFIG, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, HKEY_USERS,
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

pub fn reg_subkey_exists(full_path: &str) -> anyhow::Result<bool> {
    let (root, path, _) = split_reg_path(full_path)?;
    match root.open_subkey(path) {
        Ok(_) => Ok(true),
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(false)
            } else {
                Err(e.into())
            }
        }
    }
}

pub fn reg_get_value<T>(full_path: &str) -> anyhow::Result<T>
where
    T: FromRegValue,
{
    let (root, path, name) = split_reg_path(full_path)?;
    let subkey = root.open_subkey(path)?;
    let value: T = subkey.get_value(name)?;
    Ok(value)
}

pub fn reg_set_value<T>(full_path: &str, value: &T) -> anyhow::Result<()>
where
    T: ToRegValue,
{
    let (root, path, name) = split_reg_path(full_path)?;
    let (subkey, _) = root.create_subkey(path)?;
    subkey.set_value(name, value)?;
    Ok(())
}

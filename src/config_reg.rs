//! Installer ACLs and the non-elevated configuration command.
use crate::settings::{PATH, ROOT, Settings};
use anyhow::{Context, ensure};
use windows::Win32::{
    Foundation::{HANDLE, HLOCAL, LocalFree},
    Security::Authorization::{
        BuildTrusteeWithSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetEffectiveRightsFromAclW,
        GetSecurityInfo, SE_REGISTRY_KEY, SetEntriesInAclW, SetSecurityInfo, TRUSTEE_W,
    },
    Security::{
        ACL, CONTAINER_INHERIT_ACE, CreateWellKnownSid, DACL_SECURITY_INFORMATION,
        OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID, WinWorldSid,
    },
};
use winreg::{
    RegKey,
    enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, KEY_WRITE},
};

struct LocalAllocation(*mut core::ffi::c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            LocalFree(Some(HLOCAL(self.0)));
        }
    }
}

fn with_acl<T>(
    key: &RegKey,
    operation: impl FnOnce(*mut ACL, TRUSTEE_W) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let mut acl = core::ptr::null_mut();
    unsafe {
        GetSecurityInfo(
            HANDLE(key.raw_handle().cast()),
            SE_REGISTRY_KEY,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut acl),
            None,
            Some(&mut descriptor),
        )
        .ok()?;
    }
    let _descriptor = LocalAllocation(descriptor.0);
    let mut sid = [0_u32; 17];
    let mut size = core::mem::size_of_val(&sid) as u32;
    unsafe {
        CreateWellKnownSid(
            WinWorldSid,
            None,
            Some(PSID(sid.as_mut_ptr().cast())),
            &mut size,
        )?;
    }
    let mut trustee = TRUSTEE_W::default();
    unsafe {
        BuildTrusteeWithSidW(&mut trustee, Some(PSID(sid.as_mut_ptr().cast())));
    }
    operation(acl, trustee)
}

fn everyone_can_read_write(key: &RegKey) -> anyhow::Result<bool> {
    with_acl(key, |acl, trustee| {
        if acl.is_null() {
            return Ok(true);
        }
        let mut rights = 0;
        unsafe {
            GetEffectiveRightsFromAclW(acl, &trustee, &mut rights).ok()?;
        }
        Ok(rights & (KEY_READ | KEY_WRITE) == KEY_READ | KEY_WRITE)
    })
}

fn grant_everyone(key: &RegKey) -> anyhow::Result<()> {
    with_acl(key, |acl, trustee| {
        if acl.is_null() {
            return Ok(());
        }
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: KEY_READ | KEY_WRITE,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE,
            Trustee: trustee,
        };
        let mut updated = core::ptr::null_mut();
        unsafe {
            SetEntriesInAclW(Some(&[entry]), Some(acl), &mut updated).ok()?;
        }
        let _updated = LocalAllocation(updated.cast());
        unsafe {
            SetSecurityInfo(
                HANDLE(key.raw_handle().cast()),
                SE_REGISTRY_KEY,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(updated),
                None,
            )
            .ok()?;
        }
        Ok(())
    })
}

pub fn ready() -> bool {
    [ROOT, PATH].into_iter().all(|path| {
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey_with_flags(path, KEY_READ | KEY_WOW64_64KEY)
            .ok()
            .and_then(|key| everyone_can_read_write(&key).ok())
            .unwrap_or(false)
    })
}

pub fn initialize() -> anyhow::Result<()> {
    // Preserve existing administrative ACEs; only grant data read/write to Everyone.
    for path in [ROOT, PATH] {
        let (key, _) = RegKey::predef(HKEY_LOCAL_MACHINE)
            .create_subkey_with_flags(path, KEY_READ | KEY_WRITE | KEY_WOW64_64KEY | 0x0004_0000)?; // WRITE_DAC
        grant_everyone(&key)?;
    }
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(PATH, KEY_READ | KEY_WRITE | KEY_WOW64_64KEY)?;
    for (name, value) in [("Enabled", 1u32), ("GainMilliDb", 0), ("Version", 1)] {
        if key
            .get_raw_value(name)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            key.set_value(name, &value)?;
        }
    }
    ensure!(
        ready(),
        "VoiceBox registry ACL still denies Everyone read/write access"
    );
    Ok(())
}

pub fn command() -> anyhow::Result<bool> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("--config") {
        return Ok(false);
    }
    let mut settings = Settings::load();
    let mut changed = false;
    let mut options = args[1..].iter();
    while let Some(option) = options.next() {
        let value = options
            .next()
            .context("Expected a value after configuration option")?;
        match option.as_str() {
            "--enabled" => {
                settings.enabled = value.parse().context("--enabled accepts true or false")?
            }
            "--gain-db" => {
                let gain: f64 = value.parse().context("Invalid gain")?;
                ensure!(
                    gain.is_finite() && (-60.0..=12.0).contains(&gain),
                    "Gain must be between -60 and +12 dB"
                );
                settings.gain_millidb = (gain * 1000.0).round() as i32;
            }
            _ => anyhow::bail!("Unknown configuration option: {option}"),
        }
        changed = true;
    }
    if changed {
        let key = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey_with_flags(PATH, KEY_WRITE | KEY_WOW64_64KEY)
            .context("Install VoiceBox once to create the writable configuration key")?;
        key.set_value("GainMilliDb", &(settings.gain_millidb as u32))?;
        key.set_value("Enabled", &u32::from(settings.enabled))?;
    }
    println!(
        "Enabled={} GainMilliDb={} GainLinear={:.6}",
        settings.enabled,
        settings.gain_millidb,
        settings.gain_linear()
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn acl_check_uses_everyone_and_grant_preserves_owner_access() {
        use winreg::enums::HKEY_CURRENT_USER;
        let root = RegKey::predef(HKEY_CURRENT_USER);
        let path = format!(r"Software\VoiceBoxTests\Acl-{}", std::process::id());
        let (key, _) = root.create_subkey(&path).unwrap();
        grant_everyone(&key).unwrap();
        assert!(everyone_can_read_write(&key).unwrap());
        let (child, _) = key.create_subkey("Child").unwrap();
        assert!(
            everyone_can_read_write(&child).unwrap(),
            "children must inherit read/write access"
        );
        child.set_value("Enabled", &1u32).unwrap();
        assert_eq!(child.get_value::<u32, _>("Enabled").unwrap(), 1);
        drop(child);
        drop(key);
        root.delete_subkey_all(&path).unwrap();
    }
}

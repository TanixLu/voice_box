//! Inheritable Everyone Modify ACLs on the VoiceBox data directory only.
use crate::shared::{paths, utils::wide_null};
use anyhow::{Context, ensure};
use std::{fs, path::Path};
use windows::{
    Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::{
            ACE_FLAGS, ACL,
            Authorization::{
                BuildTrusteeWithSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetEffectiveRightsFromAclW,
                GetExplicitEntriesFromAclW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
                SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_W,
            },
            CONTAINER_INHERIT_ACE, CreateWellKnownSid, DACL_SECURITY_INFORMATION, EqualSid,
            OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID, WinWorldSid,
        },
        Storage::FileSystem::{
            DELETE, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        },
    },
    core::PCWSTR,
};

const MODIFY: u32 = FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0 | FILE_GENERIC_EXECUTE.0 | DELETE.0;
const INHERIT: ACE_FLAGS = ACE_FLAGS(CONTAINER_INHERIT_ACE.0 | OBJECT_INHERIT_ACE.0);

struct LocalAllocation(*mut core::ffi::c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            LocalFree(Some(HLOCAL(self.0)));
        }
    }
}

fn with_acl<T>(
    path: &Path,
    operation: impl FnOnce(*mut ACL, TRUSTEE_W) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let path = wide_null(path);
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let mut acl = core::ptr::null_mut();
    unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(path.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut acl),
            None,
            &mut descriptor,
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

fn everyone_can_modify(path: &Path, directory: bool) -> anyhow::Result<bool> {
    with_acl(path, |acl, trustee| {
        if acl.is_null() {
            return Ok(true);
        }
        let mut rights = 0;
        unsafe {
            GetEffectiveRightsFromAclW(acl, &trustee, &mut rights).ok()?;
        }
        if rights & MODIFY != MODIFY {
            return Ok(false);
        }
        if !directory {
            return Ok(true);
        }
        // Directory access alone is insufficient: new/replaced settings and logs
        // must inherit the same Modify rights.
        let mut count = 0;
        let mut entries = core::ptr::null_mut();
        unsafe {
            GetExplicitEntriesFromAclW(acl, &mut count, &mut entries).ok()?;
        }
        let _entries = LocalAllocation(entries.cast());
        if count == 0 {
            return Ok(false);
        }
        Ok(
            unsafe { core::slice::from_raw_parts(entries, count as usize) }
                .iter()
                .any(|entry| {
                    entry.grfAccessMode == GRANT_ACCESS
                        && entry.grfAccessPermissions & MODIFY == MODIFY
                        && entry.grfInheritance & INHERIT == INHERIT
                        && entry.Trustee.TrusteeForm == TRUSTEE_IS_SID
                        && unsafe {
                            EqualSid(
                                PSID(entry.Trustee.ptstrName.0.cast()),
                                PSID(trustee.ptstrName.0.cast()),
                            )
                            .is_ok()
                        }
                }),
        )
    })
}

fn grant_modify(path: &Path, directory: bool) -> anyhow::Result<()> {
    with_acl(path, |acl, trustee| {
        if acl.is_null() {
            return Ok(());
        }
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: MODIFY,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: if directory { INHERIT } else { ACE_FLAGS(0) },
            Trustee: trustee,
        };
        let mut updated = core::ptr::null_mut();
        unsafe {
            SetEntriesInAclW(Some(&[entry]), Some(acl), &mut updated).ok()?;
        }
        let _updated = LocalAllocation(updated.cast());
        let path = wide_null(path);
        unsafe {
            SetNamedSecurityInfoW(
                PCWSTR(path.as_ptr()),
                SE_FILE_OBJECT,
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

pub fn ready(directory: &Path) -> anyhow::Result<bool> {
    if !directory.is_dir() || !everyone_can_modify(directory, true)? {
        return Ok(false);
    }
    let settings = directory.join(paths::SETTINGS_FILE);
    Ok(!settings.exists() || everyone_can_modify(&settings, false)?)
}

pub fn initialize(directory: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(directory).context("创建 VoiceBox 数据目录失败")?;
    if !everyone_can_modify(directory, true)? {
        grant_modify(directory, true)?;
    }
    for name in [
        paths::SETTINGS_FILE,
        super::UI_STATE_FILE,
        "voice_box.log",
        "voice_box.log.1",
        "setup.log",
        "setup.log.1",
        "logs.lock",
    ] {
        let path = directory.join(name);
        if path.exists() && !everyone_can_modify(&path, false)? {
            grant_modify(&path, false)?;
        }
    }
    ensure!(
        ready(directory)?,
        "VoiceBox 数据目录未授予所有用户可继承的修改权限"
    );
    Ok(())
}

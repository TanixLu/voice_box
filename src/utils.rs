use core::fmt;

use is_elevated::is_elevated;
use single_instance::SingleInstance;
use tracing::{Event, Subscriber};
use tracing_subscriber::{
    fmt::{
        FmtContext, FormatEvent, FormatFields,
        format::{self, Writer},
    },
    registry::LookupSpan,
};
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
    enums::{
        HKEY_CLASSES_ROOT, HKEY_CURRENT_CONFIG, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, HKEY_USERS,
        KEY_READ, KEY_SET_VALUE, KEY_WOW64_64KEY, KEY_WRITE,
    },
    transaction::Transaction,
    types::{FromRegValue, ToRegValue},
};

pub const LOG_ENABLE_STR: &str = "VOICE_BOX_LOG";
const LOG_FILE_NAME: &str = "voice_box.log";

fn split_reg_key_path(full_path: &str) -> anyhow::Result<(RegKey, &str)> {
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

    anyhow::ensure!(!tail.is_empty(), "Invalid registry key path: {full_path}");
    Ok((root, tail))
}

fn split_reg_path(full_path: &str) -> anyhow::Result<(RegKey, &str, &str)> {
    let (root, tail) = split_reg_key_path(full_path)?;
    let (subkey, name) = tail
        .rsplit_once('\\')
        .ok_or_else(|| anyhow::anyhow!("Invalid registry full_path: {full_path}"))?;
    Ok((root, subkey, name))
}

pub fn reg_get_value<T>(full_path: &str) -> anyhow::Result<T>
where
    T: FromRegValue,
{
    let (root, path, name) = split_reg_path(full_path)?;
    let subkey = root.open_subkey_with_flags(path, KEY_READ | KEY_WOW64_64KEY)?;
    let value: T = subkey.get_value(name)?;
    Ok(value)
}

pub fn reg_set_value<T>(full_path: &str, value: &T) -> anyhow::Result<()>
where
    T: ToRegValue,
{
    let (root, path, name) = split_reg_path(full_path)?;
    let (subkey, _) = root.create_subkey_with_flags(path, KEY_SET_VALUE | KEY_WOW64_64KEY)?;
    subkey.set_value(name, value)?;
    Ok(())
}

/// Atomically writes values without creating the containing key.
pub fn reg_set_existing_values<T: ToRegValue>(
    full_key_path: &str,
    values: &[(&str, T)],
) -> anyhow::Result<()> {
    let (root, path) = split_reg_key_path(full_key_path)?;
    let transaction = Transaction::new()?;
    let key = root.open_subkey_transacted_with_flags(
        path,
        &transaction,
        KEY_SET_VALUE | KEY_WOW64_64KEY,
    )?;
    for (name, value) in values {
        key.set_value(name, value)?;
    }
    transaction.commit()?;
    Ok(())
}

/// Deletes a registry key and its children in the 64-bit view; missing keys are harmless.
pub fn reg_delete_key(full_path: &str) -> anyhow::Result<()> {
    let (parent, name) = full_path
        .rsplit_once('\\')
        .filter(|(_, name)| !name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Invalid registry key path: {full_path}"))?;
    let (root, path) = split_reg_key_path(parent)?;
    let result = root
        .open_subkey_with_flags(path, KEY_READ | KEY_WRITE | KEY_WOW64_64KEY)
        .and_then(|key| key.delete_subkey_all(name));
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

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

pub fn reg_everyone_can_read_write(key: &RegKey) -> anyhow::Result<bool> {
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

pub fn reg_grant_everyone_read_write(key: &RegKey) -> anyhow::Result<()> {
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

struct ProcessFormatter {
    role: &'static str,
    pid: u32,
}

impl<S, N> FormatEvent<S, N> for ProcessFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        // 自定义前缀
        write!(writer, "[{}:{}] ", self.role, self.pid,)?;

        // 继续使用 tracing 默认的格式
        format::format().format_event(ctx, writer, event)
    }
}

pub fn init_logging() {
    // runas::Command不能传递环境变量，因此使用命令行参数来启用日志记录
    let args: Vec<String> = std::env::args().collect();
    let log_enable = std::env::var_os(LOG_ENABLE_STR).is_some()
        || args.get(3).map(String::as_str) == Some(LOG_ENABLE_STR);
    if !log_enable {
        return;
    }

    let role = if is_elevated() { "ADMIN" } else { "USER" };
    let pid = std::process::id();
    let formatter = ProcessFormatter { role, pid };
    let log_file = tracing_appender::rolling::never("", LOG_FILE_NAME);
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(log_file)
        .event_format(formatter)
        .init();
}

pub fn check_single_instance() -> anyhow::Result<SingleInstance> {
    let single_instance = SingleInstance::new("VoiceBoxSingleInstance")?;
    anyhow::ensure!(
        single_instance.is_single(),
        "Another instance of VoiceBox is already running."
    );
    Ok(single_instance)
}

use core::fmt;
use std::fmt::Debug;

use is_elevated::is_elevated;
use tracing::{Event, Subscriber};
use tracing_subscriber::{
    fmt::{
        FmtContext, FormatEvent, FormatFields,
        format::{self, Writer},
    },
    registry::LookupSpan,
};
use winreg::{
    RegKey,
    enums::{
        HKEY_CLASSES_ROOT, HKEY_CURRENT_CONFIG, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, HKEY_USERS,
        KEY_READ, KEY_SET_VALUE,
    },
    types::{FromRegValue, ToRegValue},
};

const LOG_ENV_VAR: &str = "VOICE_BOX_LOG";
const LOG_FILE_NAME: &str = "voice_box.log";

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
    if std::env::var_os(LOG_ENV_VAR).is_none() {
        return;
    }

    // 只有在环境变量 LOG_ENV_VAR 被设置时才启用日志记录
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

//! Installer ACLs and non-elevated GUI configuration writes.
use crate::{
    settings::{PATH, ROOT, Settings},
    utils::{reg_everyone_can_read_write, reg_grant_everyone_read_write, reg_set_existing_values},
};
use anyhow::{Context, ensure};
use winreg::{
    RegKey,
    enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, KEY_WRITE},
    transaction::Transaction,
};

pub fn ready() -> bool {
    [ROOT, PATH].into_iter().all(|path| {
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey_with_flags(path, KEY_READ | KEY_WOW64_64KEY)
            .ok()
            .and_then(|key| reg_everyone_can_read_write(&key).ok())
            .unwrap_or(false)
    })
}

pub fn initialize() -> anyhow::Result<()> {
    // Preserve existing administrative ACEs; only grant data read/write to Everyone.
    for path in [ROOT, PATH] {
        let (key, _) = RegKey::predef(HKEY_LOCAL_MACHINE)
            .create_subkey_with_flags(path, KEY_READ | KEY_WRITE | KEY_WOW64_64KEY | 0x0004_0000)?; // WRITE_DAC
        reg_grant_everyone_read_write(&key)?;
    }
    let transaction = Transaction::new()?;
    let key = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_transacted_with_flags(
        PATH,
        &transaction,
        KEY_READ | KEY_WRITE | KEY_WOW64_64KEY,
    )?;
    for (name, value) in [("Enabled", 1u32), ("GainMilliDb", 0), ("Version", 1)] {
        match key.get_raw_value(name) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                key.set_value(name, &value)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    transaction.commit()?;
    ensure!(
        ready(),
        "VoiceBox registry ACL still denies Everyone read/write access"
    );
    Ok(())
}

pub fn save(settings: Settings) -> anyhow::Result<()> {
    ensure!(
        (crate::settings::MIN_GAIN_MILLIDB..=crate::settings::MAX_GAIN_MILLIDB)
            .contains(&settings.gain_millidb),
        "增益必须介于 -60 和 +60 dB 之间"
    );
    reg_set_existing_values(
        &format!(r"HKLM\{PATH}"),
        &[
            ("Enabled", u32::from(settings.enabled)),
            ("GainMilliDb", settings.gain_millidb as u32),
        ],
    )
    .context("无法保存 APO 配置，请确认配置注册表已创建且可写")?;
    Ok(())
}

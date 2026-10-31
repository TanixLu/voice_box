use std::sync::OnceLock;

use single_instance::{SingleInstance, error::SingleInstanceError};

pub const SINGLE_INSTANCE_MUTEX: &str = r"Global\VoiceBoxSingleInstance";

pub fn is_single_instance() -> anyhow::Result<bool> {
    static SINGLE_INSTANCE: OnceLock<Result<SingleInstance, SingleInstanceError>> = OnceLock::new();

    let single_instance = SINGLE_INSTANCE
        .get_or_init(|| SingleInstance::new(SINGLE_INSTANCE_MUTEX))
        .as_ref()?;

    Ok(single_instance.is_single())
}

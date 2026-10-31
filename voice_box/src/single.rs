pub const SINGLE_INSTANCE_MUTEX: &str = r"Global\VoiceBoxSingleInstance";

pub fn is_single_instance() -> bool {
    let single_instance = single_instance::SingleInstance::new(SINGLE_INSTANCE_MUTEX).unwrap();
    single_instance.is_single()
}

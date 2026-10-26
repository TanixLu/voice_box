//! Language-neutral errors shared with the APO. Display is for diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharedError {
    GainOutOfRange,
    ApoCfgTooLarge,
    AudioClockUnavailable,
    InvalidMicrophoneId,
    InvalidPlaybackLayout,
    MapPlaybackBuffer,
    InvalidPlaybackId,
}
impl std::fmt::Display for SharedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::GainOutOfRange => "Gain must be between -60 and +60 dB",
            Self::ApoCfgTooLarge => "The VoiceBox APO configuration file is too large",
            Self::AudioClockUnavailable => "The system audio clock is unavailable",
            Self::InvalidMicrophoneId => "Invalid microphone identifier",
            Self::InvalidPlaybackLayout => "Invalid playback buffer layout",
            Self::MapPlaybackBuffer => "Failed to map the playback buffer",
            Self::InvalidPlaybackId => "Invalid playback buffer identifier",
        })
    }
}
impl std::error::Error for SharedError {}

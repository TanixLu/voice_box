use super::Language;
use voice_box_shared::error::SharedError;
pub(super) fn render(error: SharedError, language: Language) -> &'static str {
    match (error, language) {
        (SharedError::GainOutOfRange, Language::English) => "Gain must be between -60 and +60 dB",
        (SharedError::GainOutOfRange, Language::SimplifiedChinese) => {
            "增益必须介于 -60 和 +60 dB 之间"
        }
        (SharedError::ApoCfgTooLarge, Language::English) => {
            "The VoiceBox APO configuration file is too large"
        }
        (SharedError::ApoCfgTooLarge, Language::SimplifiedChinese) => "VoiceBox APO 配置文件过大",
        (SharedError::AudioClockUnavailable, Language::English) => {
            "The system audio clock is unavailable"
        }
        (SharedError::AudioClockUnavailable, Language::SimplifiedChinese) => "系统音频时钟不可用",
        (SharedError::InvalidMicrophoneId, Language::English) => "Invalid microphone identifier",
        (SharedError::InvalidMicrophoneId, Language::SimplifiedChinese) => "麦克风标识无效",
        (SharedError::InvalidPlaybackLayout, Language::English) => "Invalid playback buffer layout",
        (SharedError::InvalidPlaybackLayout, Language::SimplifiedChinese) => "播放缓冲区布局无效",
        (SharedError::MapPlaybackBuffer, Language::English) => "Failed to map the playback buffer",
        (SharedError::MapPlaybackBuffer, Language::SimplifiedChinese) => "映射播放缓冲区失败",
        (SharedError::InvalidPlaybackId, Language::English) => "Invalid playback buffer identifier",
        (SharedError::InvalidPlaybackId, Language::SimplifiedChinese) => "播放缓冲区标识无效",
    }
}

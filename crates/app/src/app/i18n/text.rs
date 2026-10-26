//! Central bilingual catalog. Every entry requires both languages.
use super::Language;

macro_rules! catalog {
    ($($key:ident => ($en:literal, $zh:literal);)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(in crate::app) enum Text { $($key,)* }

        impl Text {
            pub(in crate::app) fn in_language(self, language: Language) -> &'static str {
                match (self, language) {
                    $((Self::$key, Language::English) => $en,
                      (Self::$key, Language::SimplifiedChinese) => $zh,)*
                }
            }
            pub(in crate::app) fn text(self) -> &'static str {
                self.in_language(super::current_language())
            }
            pub(in crate::app) fn matches_input(self, input: &str) -> bool {
                [Language::English, Language::SimplifiedChinese].into_iter()
                    .any(|language| input.trim().eq_ignore_ascii_case(self.in_language(language)))
            }
        }
    }
}

catalog! {
    OpenDatabase => ("Cannot open voice_box.db", "无法打开 voice_box.db");
    AudioChangedDuringImport => ("The audio file changed during import. Try again", "导入过程中音频文件已变化，请重试");
    OpeningMicrophone => ("Opening microphone…", "正在打开麦克风…");
    PreparingProcessedAudio => ("Preparing processed audio…", "正在准备处理后的声音…");
    OpeningPlaybackDevice => ("Opening playback device…", "正在打开播放设备…");
    PreviewIncomplete => ("Preview did not complete. Please try again.", "试听操作未完成，请重试。");
    Recording => ("Recording…", "正在录音…");
    Playing => ("Playing…", "正在播放…");
    ReadPreviewPath => ("Failed to locate the preview component", "读取试听组件路径失败");
    ReadPreviewComponent => ("Cannot read the preview component. Install or update the plugin first", "无法读取试听组件，请先安装或更新插件");
    PreviewComponentOutdated => ("The installed preview component does not match this application. Update the plugin first", "已安装的试听组件与当前程序不一致，请先更新插件");
    LoadPreviewComponent => ("Failed to load the preview component. Install or update the plugin first", "加载试听组件失败，请先安装或更新插件");
    PreviewInterfaceMissing => ("The preview component is missing its processing interface. Update VoiceBox", "试听组件缺少处理接口，请更新 VoiceBox");
    InvalidRecordingData => ("Invalid recording data", "录音数据无效");
    ProcessPreview => ("Failed to process the preview recording", "处理试听录音失败");
    OpenPlaybackDevice => ("Failed to open the playback device", "开启播放设备失败");
    PlaybackDeviceUnresponsive => ("The playback device is not responding. Check its connection", "播放设备没有响应，请检查设备连接");
    ReadApoPath => ("Failed to locate the APO DLL", "读取 APO DLL 路径失败");
    RecycleApo => ("Failed to move the APO DLL to the Recycle Bin", "回收 APO DLL 失败");
    SignalCleanup => ("Failed to notify the cleanup process that uninstallation is complete", "通知清理进程卸载完成失败");
    ReadSystemDirectory => ("Failed to locate the Windows system directory", "读取 Windows 系统目录失败");
    StartCleanup => ("Failed to start the Recycle Bin cleanup process", "启动回收站清理进程失败");
    CleanupInitializationFailed => ("The cleanup process could not initialize. Check the error message and try again", "回收站清理进程初始化失败，请查看错误提示后重试");
    DefaultSpeakerMissing => ("No default speaker was found. Check Windows sound output settings", "找不到默认扬声器，请检查 Windows 声音输出设置");
    CaptureSpeaker => ("Cannot record the default speaker", "无法录制默认扬声器");
    ReadRecordingClock => ("Cannot read the recording clock", "无法读取录音时钟");
    InvalidSpeakerData => ("The speaker returned invalid audio data", "扬声器返回了无效音频数据");
    RecordingTooShort => ("The recording is too short. Please record again", "录音太短，请重新录制");
    RawCaptureUnavailable => ("This microphone cannot record in RAW mode, so bypassing VoiceBox processing cannot be guaranteed", "此麦克风无法开启 RAW 录音，无法保证绕过 VoiceBox 处理");
    UnsupportedPreviewChannels => ("The microphone sample rate or channel count is not supported for preview", "麦克风采样率或声道数不支持试听");
    UnsupportedPreviewPcm => ("The microphone PCM format is not supported for preview", "麦克风 PCM 格式不支持试听");
    UnsupportedPreviewFormat => ("The microphone audio format is not supported for preview", "麦克风音频格式不支持试听");
    InvalidMicrophoneFrame => ("Invalid microphone audio frame format", "麦克风音频帧格式无效");
    OpenRawCapture => ("Failed to start RAW recording", "开启 RAW 录音失败");
    InvalidMicrophoneData => ("The microphone returned invalid audio data", "麦克风返回了无效音频数据");
    MicrophoneUnresponsive => ("No audio was received from the microphone. Check its connection and microphone permissions", "麦克风未返回音频，请检查设备连接和麦克风权限");
    NoRecordedAudio => ("No audio has been recorded yet. Wait before stopping the recording", "没有录到声音，请稍后再结束录音");
    InvalidKey => ("Invalid key", "无效按键");
    HotkeyThreadStart => ("Failed to start the hotkey thread", "快捷键线程启动失败");
    HotkeyThreadExited => ("The hotkey thread has exited", "快捷键线程已退出");
    NotifyHotkeyThread => ("Cannot notify the hotkey thread", "无法通知快捷键线程");
    HotkeyResultMissing => ("The hotkey thread did not return a result", "快捷键线程未返回结果");
    InvalidHoldKey => ("Invalid automatic hold key", "自动按键无效");
    UnsupportedArguments => ("Unsupported command-line arguments. Use the application window.", "不支持此命令行参数，请在窗口中操作。");
    AdministratorRequired => ("VoiceBox requires administrator privileges. Right-click and select “Run as administrator”.", "VoiceBox 需要管理员权限，请右键选择“以管理员身份运行”。");
    InvalidApoPath => ("The APO DLL path is not valid Unicode", "APO DLL 路径不是有效的 Unicode");
    CreateRegistrationTransaction => ("Failed to create the APO registration transaction", "创建 APO 注册事务失败");
    CommitRegistrationTransaction => ("Failed to commit the APO registration transaction", "提交 APO 注册事务失败");
    ReadCompositeEffects => ("Failed to read the Composite MFX APO list", "读取 Composite MFX APO 列表失败");
    ReadProcessingModes => ("Failed to read the MFX processing modes", "读取 MFX 处理模式失败");
    SelectDeviceDefaultEffects => ("Failed to select Device Default Effects for the microphone", "为麦克风选择设备默认效果失败");
    CreateUninstallTransaction => ("Failed to create the full uninstall registry transaction", "创建完全卸载注册事务失败");
    EnumerateMicrophoneRegistry => ("Failed to enumerate microphone registry entries", "枚举麦克风注册表失败");
    ReadMicrophoneRegistry => ("Failed to read the microphone registry entry", "读取麦克风注册表项失败");
    CommitUninstallTransaction => ("Failed to commit the full uninstall registry transaction", "提交完全卸载注册事务失败");
    PluginVerificationFailed => ("Installation finished, but the plugin check failed. Refresh and try again", "安装操作已结束，但插件检查未通过，请刷新后重试");
    CreateDataDirectory => ("Failed to create the VoiceBox data directory", "创建 VoiceBox 数据目录失败");
    ReadApoCfgPreserved => ("Failed to read VoiceBox APO configuration. The original file has been preserved", "读取 VoiceBox APO 配置文件失败，保留原文件");
    SaveApoCfgFailed => ("Cannot save VoiceBox APO configuration", "无法保存 VoiceBox APO 配置文件");
    NoAudioTrack => ("The file has no audio track", "文件没有音轨");
    InvalidAudioDuration => ("Invalid audio duration", "音频时长无效");
    InvalidTrackParameters => ("Invalid audio track parameters", "音轨参数无效");
    MissingSampleRate => ("The audio track has no sample rate", "音轨缺少采样率");
    MissingChannels => ("The audio track has no channel information", "音轨缺少声道信息");
    SupportedAudioRange => ("Audio must be mono or stereo at 8–192 kHz", "首版支持 8–192 kHz 单声道或立体声音频");
    CreateAudioDecoder => ("Cannot create the audio decoder", "无法创建音频解码器");
    ReadAudioData => ("Failed to read audio data", "读取音频数据失败");
    DecodeAudio => ("Failed to decode audio", "解码音频失败");
    ChangingAudioFormat => ("Audio that changes sample rate or channel count during playback is not supported", "不支持播放中改变采样率或声道数的音频");
    AudioPacketTooLarge => ("The audio packet is too large", "音频数据包过大");
    OpenAudioFile => ("Cannot open the audio file", "无法打开音频文件");
    UnsupportedAudioFormat => ("Unsupported audio format. Use MP3, WAV, FLAC, or OGG/Vorbis", "不支持此音频格式，请使用 MP3、WAV、FLAC 或 OGG/Vorbis");
    HotkeyServiceUnavailable => ("The global hotkey service is unavailable", "全局快捷键服务不可用");
    PressKey => ("Press a key…", "请按键…");
    BindHotkey => ("Bind hotkey", "绑定快捷键");
    CaptureHotkeyHint => ("Press a shortcut. Press Esc or click again to cancel", "请按下快捷键，按 Esc 或再次点击取消");
    SoundNotFound => ("The sound does not exist", "音效不存在");
    InvalidSoundName => ("The name must not be empty or contain control characters", "音效名称不能为空，也不能包含控制字符");
    RenameSoundFailed => ("Failed to rename the sound", "重命名音效失败");
    ReadSourceAudioPath => ("Failed to read the original audio path", "读取原音频路径失败");
    AudioIsNotFile => ("The selected audio is not a file", "所选音频不是文件");
    InvalidAudioFileName => ("Invalid audio file name", "音频文件名无效");
    NoPlayableAudio => ("The audio file contains no playable data", "音频文件没有可播放的数据");
    UninstallVoiceBox => ("Uninstall VoiceBox", "卸载 VoiceBox");
    ConfirmFullUninstall => ("All VoiceBox components and settings will be removed.\n\nDo you want to continue?", "将卸载 VoiceBox 的所有组件及配置。\n\n确定要继续吗？");
    Uninstall => ("Uninstall", "卸载");
    PreparingUninstall => ("Preparing to uninstall…", "正在准备卸载…");
    Cancel => ("Cancel", "取消");
    Licenses => ("Licenses and notices", "版权声明");
    ProjectSource => ("Project source:", "项目源码：");
    CopyAll => ("Copy all", "复制全部");
    Close => ("Close", "关闭");
    InvalidHotkey => ("Invalid hotkey", "无效快捷键");
    HoldKeyConflict => ("The automatic hold key cannot use the same primary key as a play or stop shortcut", "自动按键不能与播放或停止快捷键使用同一个主键");
    InvalidPlaybackVolume => ("Invalid playback volume", "播放音量无效");
    SaveAppSettingsFailed => ("Failed to save application settings", "保存应用设置失败");
    SaveSoundLibrary => ("Failed to save sound library metadata", "保存音效库元数据失败");
    OpenSoundSettingsFirst => ("Open sound settings first", "请先打开音效设置");
    StopBeforeRename => ("Stop playback before renaming", "请先停止播放再重命名");
    OutputsMuted => ("Microphone and speaker volumes are both 0. Playback was not started", "麦克风和扬声器音量均为 0，未开始播放");
    ImportThreadExited => ("The sound import thread exited unexpectedly", "音效导入线程意外退出");
    DeleteThreadExited => ("The sound deletion thread exited unexpectedly", "音效删除线程意外退出");
    PlaybackThreadExited => ("The playback thread exited unexpectedly", "播放线程意外退出");
    CloseDiscardChanges => ("Close and discard unsaved changes", "关闭，放弃未保存的修改");
    ClearHotkey => ("Clear hotkey", "清除快捷键");
    Hotkeys => ("Hotkeys", "快捷键");
    BatchHotkeyHint => ("Apply to all selected sounds; a shared shortcut plays a random sound from its group", "应用到所有选中音效；同一快捷键会从绑定的音效中随机播放");
    DeleteSound => ("Delete sound", "删除音效");
    BatchManage => ("Manage", "批量管理");
    MoveSounds => ("Move", "移动");
    MoveToTop => ("Move to top", "移至顶部");
    MoveToBottom => ("Move to bottom", "移至底部");
    BatchRename => ("Rename", "重命名");
    RenameLocation => ("Locate", "定位到");
    RenameStart => ("Start of sound name", "音效名称开头");
    RenameEnd => ("End of sound name", "音效名称结尾");
    RenameMatches => ("Matching text", "匹配的文字");
    RenameEntire => ("Entire sound name", "整个音效名称");
    RenameFind => ("Find text", "查找文字");
    RenameContent => ("Write content", "写入内容");
    RenameSequence => ("<index>", "<序号>");
    RenameSoundName => ("Sound", "音效");
    RenamePreview => ("Live preview", "实时预览");
    RenameOldName => ("Original name", "原名称");
    RenameNewName => ("New name", "新名称");
    ApplyRename => ("Apply rename", "应用重命名");
    RenameApplying => ("Renaming…", "正在重命名…");
    RenameValidationFailed => ("Invalid sound names or changed entries. Check the preview again.", "音效名称无效或选中项已变化，请重新检查预览。");
    RenameBusy => ("Wait for the current library operation to finish", "请等待当前音效库操作完成");
    RenameThreadExited => ("The rename worker exited unexpectedly", "重命名任务意外退出");
    Done => ("Done", "完成");
    SelectSearchResults => ("Select all results", "全选当前结果");
    ClearSelection => ("Clear selection", "取消选择");
    SelectSoundHint => ("Click to select; Shift-click to select a range", "点击选择，Shift + 点击选择连续区间");
    DeleteSoundHint => ("Delete sound; click again to confirm", "删除音效，点击后再次确认");
    ConfirmDeleteSound => ("Confirm sound deletion", "确认删除音效");
    ConfirmDeleteSoundHint => ("Click again to delete the sound from the library, keeping the original import file. Click elsewhere or press Esc to cancel", "再次点击确认从音效库删除音效，保留导入来源文件；点击其他位置或按 Esc 取消");
    StopPlayback => ("Stop playback", "停止播放");
    PlaySound => ("Play sound", "播放音效");
    CreatePlaybackBuffer => ("Failed to create the playback transport buffer", "创建播放传输缓冲区失败");
    SpeakerPlaybackFailed => ("Speaker playback failed", "扬声器播放失败");
    PluginInstalled => ("Plugin installed", "已安装插件");
    PluginNotInstalled => ("Plugin not installed", "插件未安装");
    PluginNeedsUpdate => ("Plugin update required", "插件需要更新");
    ReadingMicrophones => ("Reading microphone list…", "正在读取麦克风列表…");
    Microphone => ("Microphone", "麦克风");
    Refresh => ("Refresh", "刷新");
    NoMicrophones => ("No microphones available", "暂无可用麦克风");
    ConnectMicrophoneHint => ("Connect a microphone, then click Refresh to search again.", "连接麦克风后，点击刷新重新查找。");
    UpdatePlugin => ("Update plugin", "更新插件");
    InstallPlugin => ("Install plugin", "安装插件");
    InstallPluginHint => ("Install or update the VoiceBox plugin for the selected microphone", "安装或更新所选麦克风的VoiceBox插件");
    UpdatingPlugin => ("Updating plugin…", "正在更新插件…");
    InstallingPlugin => ("Installing plugin…", "正在安装插件…");
    UninstallPlugin => ("Uninstall plugin", "卸载插件");
    ConfirmUninstall => ("Confirm uninstall", "确认卸载");
    UninstallingPlugin => ("Uninstalling plugin…", "正在卸载插件…");
    VoiceConfig => ("Voice settings", "语音设置");
    EnableDenoising => ("Enable noise reduction", "启用降噪");
    VolumeAdjustment => ("Volume adjustment", "音量调节");
    Mute => ("Mute", "静音");
    AdjustVolumeHint => ("Drag to adjust, or click the value to type.", "拖动调整，或点击数值直接输入。");
    ResetGain => ("Reset", "重置");
    ResetGainHint => ("Reset to 0 dB", "恢复到 0 dB");
    SaveChanges => ("Save settings", "保存设置");
    UndoChanges => ("Undo changes", "撤销修改");
    RestoreDefaults => ("Restore defaults", "恢复默认");
    UnsavedVoiceConfig => ("There are unsaved changes. Preview uses the current settings; save to apply them to microphone plugins.", "有未保存的修改；试听使用当前设置，保存后应用于麦克风插件。");
    VoiceConfigApplied => ("Settings are applied to all microphones with the VoiceBox plugin installed.", "设置已应用到所有安装 VoiceBox 插件的麦克风。");
    RecordingPreview => ("Voice preview", "语音试听");
    FinishRecording => ("Finish recording", "结束录音");
    StartRecording => ("Start recording", "开始录音");
    PlayOriginal => ("Play original", "播放原音频");
    PlayProcessed => ("Play processed", "播放处理后声音");
    Stopping => ("Stopping…", "正在停止…");
    OperationIncomplete => ("The operation did not complete. Please try again.", "操作未完成，请重试。");
    ImportSoundsTitle => ("Import sounds", "批量导入音效");
    AudioFiles => ("Audio files", "音频文件");
    AllFiles => ("All files", "所有文件");
    ReadWorkArea => ("Failed to read the primary display work area", "读取主屏工作区失败");
    InvalidDisplay => ("Invalid primary display parameters", "主屏显示参数无效");
    Repeat => ("Repeat", "循环");
    WrongOverlayProcess => ("The overlay window does not belong to this process", "浮层窗口不属于当前进程");
    InstallOverlayCallback => ("Failed to install the overlay window callback", "安装浮层窗口回调失败");
    RenameHint => ("Edit the sound name. Press Enter or click elsewhere to save; Esc to cancel", "编辑音效名称；Enter 或点击别处保存，Esc 取消");
    Settings => ("Settings", "设置");
    RecordImport => ("Record audio", "录制导入");
    Import => ("Import", "导入");
    SearchSounds => ("Search sounds", "搜索音效");
    ReorderSound => ("Reorder sound", "调整音效顺序");
    ReorderSoundHint => ("Drag to reorder. Right-click to move to the top or bottom", "拖拽调整顺序，右键可移至顶部或底部");
    MoveSoundToTop => ("Move to top", "移至顶部");
    MoveSoundToBottom => ("Move to bottom", "移至底部");
    ClearSearchToReorder => ("Clear the search to reorder sounds", "清空搜索后可调整顺序");
    AudioLength => ("Audio duration", "音频长度");
    ImportSoundsHint => ("Click Import to add sounds. You can select multiple files at once", "点击「导入」添加音效，可一次选择多个文件");
    NoMatchingSounds => ("No matching sounds", "没有匹配的音效");
    SoundImportFailedTitle => ("Sound import failed", "音效导入失败");
    SoundSettings => ("Sound settings", "音效设置");
    MicrophoneOutput => ("Microphone output", "麦克风输出");
    FollowSystemDefault => ("Follow system default", "跟随系统默认");
    OutputDeviceUnavailable => ("Selected device unavailable", "所选设备不可用");
    ReadingOutputDevices => ("Reading audio devices…", "正在读取音频设备…");
    SoundboardMicrophonePluginRequired => ("Install or update the VoiceBox plugin for this microphone", "请为此麦克风安装或更新 VoiceBox 插件");
    SpeakerOutput => ("Speaker output", "扬声器输出");
    IncludeMicrophoneAudio => ("Keep microphone audio during playback", "音效播放时保持收音");
    ShowOverlay => ("Show playback overlay", "显示播放浮层");
    RepeatPlayback => ("Repeat playback", "循环播放");
    HoldDuringPlayback => ("Hold key during playback", "播放时自动按住");
    StopHotkey => ("Stop playback shortcut", "停止播放快捷键");
    RandomHotkey => ("Random playback shortcut", "随机播放快捷键");
    UnsavedSoundSettings => ("There are unsaved changes. Save to apply them; closing discards unsaved changes.", "有未保存的修改，保存后生效；关闭将放弃未保存的修改。");
    SoundSettingsApplied => ("The current settings are active.", "当前设置已生效。");
    NoOutput => ("No output", "不输出");
    RecordingName => ("Recording", "录音");
    StopCapture => ("Stop recording", "停止录制");
    StartCapture => ("Start recording", "开始录制");
    RecordSpeakerHint => ("Record the default speaker. Starting again replaces the current recording", "录制默认扬声器；再次开始录制将替换当前录音");
    FinishingRecording => ("Finishing recording…", "正在整理录音…");
    OpeningDefaultSpeaker => ("Opening default speaker…", "正在打开默认扬声器…");
    Start => ("Start", "开始");
    SecondsSuffix => (" s", " 秒");
    End => ("End", "结束");
    StopPreview => ("Stop preview", "停止试听");
    PreviewSelection => ("Preview selection", "试听选中片段");
    SoundName => ("Sound name", "音效名称");
    EnterName => ("Enter a name", "输入名称");
    ImportIntoLibrary => ("Add to library", "导入音效库");
    Saving => ("Saving…", "正在保存…");
    InvalidSoundNameHint => ("Names must not be empty or contain control characters.", "名称不能为空，也不能包含控制字符。");
    UninstallingVoiceBox => ("Uninstalling VoiceBox", "正在卸载 VoiceBox");
    Voice => ("Voice", "语音");
    Soundboard => ("Sounds", "音效");
    More => ("More", "更多");
    FullUninstall => ("Uninstall VoiceBox", "完全卸载");
    SavingApoCfg => ("Saving settings…", "正在保存设置…");
    RecorderExited => ("The recording tool exited unexpectedly. Please try again", "录音工具意外退出，请重试");
    InvalidSelection => ("Invalid selection range", "截取范围无效");
    InvalidRecordingFormat => ("Invalid recording format", "录音格式无效");
    InvalidRecordingName => ("The name must not be empty or contain control characters", "名称不能为空，也不能包含控制字符");
    SaveRecording => ("Failed to save the recording", "保存录音失败");
    LanguageLabel => ("Language", "语言");
    FollowSystem => ("System default", "跟随系统");
    ThemeLabel => ("Theme", "主题");
    LightTheme => ("Light", "浅色");
    DarkTheme => ("Dark", "深色");
    AnotherInstance => ("Another instance of VoiceBox is already running.", "VoiceBox 已在运行。");
    InvalidDeviceGuid => ("Invalid device GUID", "设备 GUID 无效");
    UninstallIncompleteTitle => ("VoiceBox uninstall incomplete", "VoiceBox 卸载未完成");
    UninstallCompleteTitle => ("VoiceBox uninstalled", "VoiceBox 卸载完成");
    UninstallProgress => ("Uninstalling VoiceBox. Please wait…", "正在卸载 VoiceBox，请稍候…");
    Ok => ("OK", "确定");
    UninstallProcessExited => ("The uninstall process exited unexpectedly. Open VoiceBox and try again.", "卸载进程意外退出，请重新打开 VoiceBox 后重试。");
    RecyclingExecutable => ("Moving program files to the Recycle Bin…", "正在将程序文件移入回收站…");
    RecyclingSettings => ("Moving settings to the Recycle Bin…", "正在将配置移入回收站…");
    UninstallComplete => ("VoiceBox has been completely removed from your computer.", "VoiceBox 已完全从您的电脑中移除。");
    UninstallIncomplete => ("Uninstallation incomplete", "卸载未完成");
    UninstallWindowFailed => ("Failed to initialize the uninstall window", "卸载窗口初始化失败");
    CleanupFailed => ("Uninstall cleanup failed", "卸载清理失败");
    RecycleExecutableIncomplete => ("Program file recycling did not complete", "回收程序文件未完成");
    RecycleDataIncomplete => ("Data directory recycling did not complete", "回收数据目录未完成");
}

impl std::fmt::Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.in_language(Language::English))
    }
}
impl std::error::Error for Text {}

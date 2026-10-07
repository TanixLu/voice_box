# 麦克风 / APO 诊断

## COM 边界修复

当前版本在锁定时检查输入、输出最大帧数和字节容量，在处理时拒绝超出已声明容量的帧数。
锁定后的格式查询不会改写实际声道数；协商失败保留调用方的输出指针。
DSP 访问使用不等待的原子保护，Reset 请求由处理线程应用，避免并发修改 DSP。

变声注册为 SFX，支持的采集模式不包含 RAW；直接以 RAW 模式初始化时直通音频，并返回空效果列表。
安装器没有旧 EFX 的检测、恢复或删除逻辑。已有的旧 EFX 注册需单独处理，
同时挂载 VoiceBox 的 SFX 和 EFX 会重复处理音频，旧 EFX 也会继续影响 RAW 流。

运行回归检查：

```powershell
cargo test --lib --offline
cargo test --lib --release --offline
```

九项测试包含 160 种 PCM/浮点、采样率和声道组合，以及容量越界、锁定后协商、
失败输出参数、RAW 直通和效果发现、并发 Reset、COM 身份和聚合引用计数。
额外验证了 PCM 正负极值及有效位填充、浮点 NaN/Infinity、跨缓冲区相位连续性，
以及初始化结构版本 1/2/3、未对齐初始化数据、重复锁定/解锁和仅发现用途的生命周期。
本地发布 DLL 的协商和原地处理检查结果为 `target/audio-diagnostics-com-hardening-local/report.txt`，0 违规。
这些检查不代表其他硬件、Windows 版本或受保护 AudioDG 环境已经通过兼容性认证。
安装器仍采用开发安装方式；正式部署所需的签名和 HLK 验证需要另行完成。

## SFX 实机复测：2026-10-07

按用户授权删除了当前麦克风上指向 VoiceBox 的旧 EFX 注册值，再安装新版 SFX。
该删除是本次测试的操作，安装器没有添加旧 EFX 清理或迁移逻辑。
本机为 Windows build 26200，Intel Smart Sound 麦克风，48 kHz 双声道。
系统与本地发布 DLL 的 SHA256 均为 `5AEE54CE205A97E82956C3C6C240AD84CAF357F07298F2DB7BA553518E00DFB8`。

- Debug 和 Release 的九项回归测试均通过；Clippy（`-D warnings`）、格式和差异检查通过。
- 发布 DLL 的直接协商及原地处理检查连续十轮通过，无违规或 DLL 无法卸载的记录。
- 完整实机诊断 `target/audio-diagnostics-sfx-system/report.txt` 为 **8/8 通过**。
  每种采集方式 3 秒、重开三轮，共 21 轮采集；均有非零样本，NaN/Infinity 和时间戳错误为 0。
  Stop、Reset、释放及重新打开均成功，未检测到相关应用崩溃事件。
- ETW 提取结果为 `voicebox-events.csv` 和 `etw-summary.json`。
  五种非 RAW 共享场景每种都有三次实际处理后解锁记录；例如普通模式每轮处理 144480 帧。
  RAW 场景的测试时间段内没有 VoiceBox 锁定或实际处理记录，证实本机 RAW 绕过 SFX。
  独占采集同样绕过 VoiceBox；没有 LockForProcess 失败事件。
- 共享采集各轮仅首包报告一次 discontinuity，之后没有新增；独占采集没有 discontinuity。

测试证明当前 DLL 在本机接口和采集链路上的行为，Windows 设置、录音机和网页界面仍需实际打开确认。

以下“本机结果”记录的是之前 EFX 版本的实机诊断。

构建并在普通 PowerShell 中运行（允许麦克风访问，测试过程中可以说话）：

```powershell
cargo build --bin audio_diagnostics
.\target\debug\audio_diagnostics.exe
```

程序独立于安装器，不修改注册表、不替换系统 DLL、不重启音频服务。
默认每项采集 2 秒、重新创建并释放两轮；每项运行在独立子进程中。
子进程崩溃时保留最后一个 API 调用，超时后只终止该测试子进程。

输出目录默认是 `target/audio-diagnostics-时间戳`，包含：

- `report.txt`：默认设备、原生格式、FxProperties、COM/APO 注册、DLL 一致性、会话及所有测试结果。
- 每项测试的 `.log`：具体 API 调用、HRESULT、首个数据包、峰值/RMS、静音和数据中断统计、停止与释放过程。
- 每次成功进入采集阶段的 `.wav`：使用实际采集格式保存的音频。API 初始化失败时不会生成 WAV。

测试项：

| 名称 | 测试内容 |
| --- | --- |
| `default-event` | 普通共享模式，事件驱动 |
| `communications-event` | 通信类别共享模式 |
| `speech-event` | 语音类别共享模式 |
| `raw-event` | RAW 共享模式；EFX 仍可能运行，不能当作关闭 EFX 的对照 |
| `default-poll` | 普通共享模式，轮询 |
| `mono16-event` | 48 kHz 单声道 PCM16，共享模式自动格式转换 |
| `exclusive-poll` | 使用设备原生格式独占采集，作为共享音频处理链之外的对照 |
| `apo-contract` | 检查输入/输出协商、LockForProcess、MUST_MATCH 注册约束及原地处理；可测试已安装或本地 DLL |

独占模式可能因其他独占会话或设备策略被拒绝，需查看具体 HRESULT。
全静音会显示 WARNING；有数据包和 API 成功不能证明有可听的人声。
程序对 SILENT 数据包生成静音，不读取可能为空的音频数据指针。

只运行某项，或增加采集时长：

```powershell
.\target\debug\audio_diagnostics.exe --case apo-contract --cycles 1
.\target\debug\audio_diagnostics.exe --case apo-contract --apo-dll target/release/voice_box.dll --cycles 1
.\target\debug\audio_diagnostics.exe --case default-event --seconds 5 --cycles 3
.\target\debug\audio_diagnostics.exe --device '{0.0.1.00000000}.{设备GUID}' --output target/audio-retest
```

需要引擎和 APO 的 ETW 日志时，在**管理员 PowerShell** 中执行：

```powershell
.\tools\trace_audio_diagnostics.ps1 -Output target/audio-trace
```

脚本追踪 VoiceBox 和 Microsoft-Windows-Audio，运行测试后在 finally 中停止自己的追踪会话。
输出 `apo.etl`、`apo.xml`、`trace-run.log` 及相关应用崩溃事件。

## 本机结果（2026-10-07）

### 修复后

修复版已安装；系统 DLL 与 `target/release/voice_box.dll` 的 SHA256 一致。
完整复测结果：`target/audio-diagnostics-after-format-fix/report.txt`，**8/8 项通过**。
六种共享采集和独占采集均完成两轮，收到非零音频样本，没有 NaN/Infinity；
Stop、Reset、释放和重新创建都成功。共享采集的首包报告一次 discontinuity，后续没有新增。
输入/输出格式协商和单声道、双声道原地处理检查均为 0 违规。

修正内容：

- 协商检查另一端格式，完全匹配才返回 S_OK；不匹配时返回 S_FALSE 和另一端的受支持格式，
  保证建议格式与 LockForProcess 一致，声道数也从建议格式更新。
- APOProcess 在修改输出连接属性前复制输入属性，防止原地处理时 BUFFER_INVALID 覆盖输入状态。
- 增加协商两端格式的 ETW 日志，以及无需安装即可测试本地 DLL 的 `--apo-dll` 选项。

旧系统 DLL 备份：`target/apo-backup-before-format-fix/voice_box.dll`。
这些复测覆盖直接 WASAPI 和 APO 接口；Windows 设置、录音机和浏览器界面需要重新打开后确认。

### 修复前

最终完整复测：`target/audio-diagnostics-final/report.txt`。8 项中独占采集通过；
六项共享采集初始化失败，APO 协商检查发现两处矛盾。
完整故障日志：`target/audio-diagnostics-engine/report.txt` 和 `apo.xml`。
独占采集结果：`target/audio-diagnostics-exclusive/report.txt`。
格式协商检查：`target/audio-diagnostics-contract/apo-contract.log`。

1. 六种共享模式均在 `IAudioClient::Initialize` 返回 `0x887C001E`。
   设备枚举、激活、GetMixFormat 和 IsFormatSupported 均成功。
   沙箱内另有 `0x80070005` 权限限制，因此以上结论来自沙箱外测试。
2. ETW 中 VoiceBox 的 COM 创建和 Initialize 成功，随后 IsInputFormatSupported 返回 S_OK。
   没有进入 LockForProcess；对象销毁时 `process_calls=0`，`processed_frames=0`。
3. 独占模式两轮采集均成功，得到非零样本，Stop、Reset、接口释放及重新打开正常。
4. 设备原生格式为双声道 48 kHz PCM16；引擎混合格式为双声道 48 kHz float32。
5. 直接检查已安装 APO：float32 → float32、PCM16 → PCM16 的协商和 LockForProcess 都成功。
   PCM16 输入 / float32 输出的协商也错误地返回 S_OK，但 LockForProcess 随即返回 `0x887D0009`
   （APOERR_INVALID_CONNECTION_FORMAT）。这还违反了注册中的 BITSPERSAMPLE_MUST_MATCH。

明确的代码缺陷位于 `src/com.rs` 的 IsInputFormatSupported / IsOutputFormatSupported：
它们忽略 `pOppositeFormat`，仅检查请求端，导致协商结果与实际处理能力矛盾。
这是 EFX 音频链初始化失败的主要嫌疑；需在修正协商后复测共享采集，才能确认它是否是全部故障的唯一原因。
不能仅由浏览器 NotReadableError 断言设备被占用，也不能由录音机的 0x80004003 断言 audiodg 崩溃。

修复方向：在协商时检查两端格式；不能直接接受时，应按 APO 协商约定返回 S_FALSE 和可处理的建议格式，
或实现真实的输入/输出格式转换。只去掉 MUST_MATCH 标志不能补足 DSP 的格式转换能力。
上述修复前的诊断阶段没有更新已安装的 APO；修复后的安装和复测结果见前节。

参考：[Microsoft 的 IsInputFormatSupported 约定](https://learn.microsoft.com/en-us/windows/win32/api/audioenginebaseapo/nf-audioenginebaseapo-iaudioprocessingobject-isinputformatsupported)、
[APO 架构](https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/audio-processing-object-architecture)。

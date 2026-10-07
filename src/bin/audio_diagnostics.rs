//! Standalone WASAPI probe. Does not install an APO or change system settings.
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use wasapi::{DeviceEnumerator, Direction, Role};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Media::{
        Audio::*, KernelStreaming::KSDATAFORMAT_SUBTYPE_PCM,
        Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    },
    System::{
        Com::{
            CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
            CoUninitialize,
        },
        Threading::{CreateEventW, WaitForSingleObject},
    },
};
use windows_core::{Interface, PCWSTR};
use winreg::{
    RegKey,
    enums::{HKEY_CLASSES_ROOT, HKEY_LOCAL_MACHINE},
};

const CASES: &[&str] = &[
    "default-event",
    "communications-event",
    "speech-event",
    "raw-event",
    "default-poll",
    "mono16-event",
    "exclusive-poll",
    "apo-contract",
];
#[path = "audio_diagnostics/apo_contract.rs"]
mod apo_contract;
const APO_CLSID: &str = "{AE672075-B244-4417-B5A5-FBD45B7F0268}";

struct Options {
    device: Option<String>,
    apo_dll: Option<PathBuf>,
    output: PathBuf,
    seconds: u64,
    cycles: u32,
    case: Option<String>,
    worker: bool,
}

impl Options {
    fn parse() -> Result<Self> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let mut options = Self {
            device: None,
            apo_dll: None,
            output: PathBuf::from(format!("target/audio-diagnostics-{stamp}")),
            seconds: 2,
            cycles: 2,
            case: None,
            worker: false,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--device" => {
                    options.device = Some(args.next().context("--device needs an endpoint ID")?)
                }
                "--output" => {
                    options.output = args.next().context("--output needs a directory")?.into()
                }
                "--apo-dll" => {
                    options.apo_dll =
                        Some(args.next().context("--apo-dll needs a DLL path")?.into())
                }
                "--seconds" => {
                    options.seconds = args.next().context("--seconds needs a number")?.parse()?
                }
                "--cycles" => {
                    options.cycles = args.next().context("--cycles needs a number")?.parse()?
                }
                "--case" => options.case = Some(args.next().context("--case needs a name")?),
                "--worker" => options.worker = true,
                "--help" | "-h" => {
                    println!(
                        "audio_diagnostics [--device ENDPOINT_ID] [--seconds 2] [--cycles 2] [--output DIR] [--case NAME] [--apo-dll DLL]\nCases: {}\n--apo-dll requires --case apo-contract; tests the local DLL without installation.\nRecords short WAV files and logs every lifecycle call. No registry writes or service restarts.",
                        CASES.join(", ")
                    );
                    std::process::exit(0);
                }
                _ => bail!("Unknown option: {arg}"),
            }
        }
        ensure!(
            (1..=30).contains(&options.seconds),
            "--seconds must be 1..30"
        );
        ensure!((1..=10).contains(&options.cycles), "--cycles must be 1..10");
        if let Some(case) = &options.case {
            ensure!(CASES.contains(&case.as_str()), "Unknown case: {case}");
        }
        if options.apo_dll.is_some() {
            ensure!(
                options.case.as_deref() == Some("apo-contract"),
                "--apo-dll requires --case apo-contract"
            );
        }
        Ok(options)
    }
}

fn say(message: impl std::fmt::Display) {
    println!("{message}");
    let _ = std::io::stdout().flush();
}

fn call<T>(name: &str, run: impl FnOnce() -> windows_core::Result<T>) -> Result<T> {
    say(format!("CALL {name}"));
    let result = run().with_context(|| name.to_owned())?;
    say(format!("OK   {name}"));
    Ok(result)
}

struct ComApartment;
impl ComApartment {
    fn new() -> Result<Self> {
        call("CoInitializeEx(MTA)", || unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()
        })?;
        Ok(Self)
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct Event(HANDLE);
impl Drop for Event {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct MixFormat(*mut WAVEFORMATEX);
impl Drop for MixFormat {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.cast())) };
    }
}

fn format_bytes(format: *const WAVEFORMATEX) -> Vec<u8> {
    // All callers provide either a live GetMixFormat allocation or a WAVEFORMATEX.
    let length = 18 + unsafe { (*format).cbSize } as usize;
    unsafe { std::slice::from_raw_parts(format.cast::<u8>(), length) }.to_vec()
}

fn describe_format(format: *const WAVEFORMATEX) -> String {
    let f = unsafe { *format };
    let (tag, channels, rate, bits, align, extra) = (
        f.wFormatTag,
        f.nChannels,
        f.nSamplesPerSec,
        f.wBitsPerSample,
        f.nBlockAlign,
        f.cbSize,
    );
    let mut description = format!(
        "tag=0x{tag:04x} channels={channels} rate={rate} bits={bits} block_align={align} extra={extra}"
    );
    if tag == 0xfffe && extra >= 22 {
        let extension = unsafe { *(format.cast::<WAVEFORMATEXTENSIBLE>()) };
        let (subtype, mask, valid) = (extension.SubFormat, extension.dwChannelMask, unsafe {
            extension.Samples.wValidBitsPerSample
        });
        description +=
            &format!(" subtype={subtype:?} valid_bits={valid} channel_mask=0x{mask:08x}");
    }
    description
}

#[derive(Default)]
struct Statistics {
    packets: u64,
    frames: u64,
    silent_packets: u64,
    discontinuities: u64,
    timestamp_errors: u64,
    nonzero_samples: u64,
    samples: u64,
    invalid_samples: u64,
    peak: f64,
    squares: f64,
}

impl Statistics {
    fn add_samples(&mut self, data: &[u8], format: *const WAVEFORMATEX) -> Result<()> {
        let f = unsafe { *format };
        let mut tag = f.wFormatTag;
        if tag == 0xfffe && f.cbSize >= 22 {
            let subtype = unsafe { (*(format.cast::<WAVEFORMATEXTENSIBLE>())).SubFormat };
            tag = if subtype == KSDATAFORMAT_SUBTYPE_PCM {
                1
            } else if subtype == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                3
            } else {
                0
            };
        }
        let bytes = f.wBitsPerSample as usize / 8;
        ensure!(
            matches!((tag, bytes), (1, 1..=4) | (3, 4 | 8)),
            "Unsupported sample representation for metrics"
        );
        for sample in data.chunks_exact(bytes) {
            let value = match (tag, bytes) {
                (3, 4) => f32::from_le_bytes(sample.try_into()?) as f64,
                (3, 8) => f64::from_le_bytes(sample.try_into()?),
                (1, 1) => (sample[0] as f64 - 128.0) / 128.0,
                (1, _) => {
                    let mut raw = [0; 4];
                    raw[..bytes].copy_from_slice(sample);
                    let shift = (4 - bytes) * 8;
                    ((i32::from_le_bytes(raw) << shift) >> shift) as f64
                        / (1_u64 << (bytes * 8 - 1)) as f64
                }
                _ => unreachable!(),
            };
            self.samples += 1;
            if !value.is_finite() {
                self.invalid_samples += 1;
                continue;
            }
            self.nonzero_samples += u64::from(value != 0.0);
            self.peak = self.peak.max(value.abs());
            self.squares += value * value;
        }
        Ok(())
    }
}

fn write_wav(path: &Path, format: &[u8], data: &[u8], frames: u32) -> Result<()> {
    let mut file = File::create(path)?;
    let padding = data.len() % 2;
    file.write_all(b"RIFF")?;
    file.write_all(
        &(4 + 8 + format.len() as u32 + 12 + 8 + data.len() as u32 + padding as u32).to_le_bytes(),
    )?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&(format.len() as u32).to_le_bytes())?;
    file.write_all(format)?;
    file.write_all(b"fact")?;
    file.write_all(&4_u32.to_le_bytes())?;
    file.write_all(&frames.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&(data.len() as u32).to_le_bytes())?;
    file.write_all(data)?;
    if padding != 0 {
        file.write_all(&[0])?;
    }
    Ok(())
}

fn capture_cycle(options: &Options, cycle: u32) -> Result<()> {
    let case = options.case.as_deref().context("Missing worker case")?;
    say(format!(
        "BEGIN case={case} cycle={cycle} pid={}",
        std::process::id()
    ));
    if case == "apo-contract" {
        return apo_contract::run(
            options.device.as_deref().context("Missing device ID")?,
            options.apo_dll.as_deref(),
        );
    }
    let enumerator: IMMDeviceEnumerator =
        call("CoCreateInstance(MMDeviceEnumerator)", || unsafe {
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
        })?;
    let id: Vec<u16> = options
        .device
        .as_deref()
        .context("Missing device ID")?
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let device = call("IMMDeviceEnumerator::GetDevice", || unsafe {
        enumerator.GetDevice(PCWSTR(id.as_ptr()))
    })?;
    let client: IAudioClient = call("IMMDevice::Activate(IAudioClient)", || unsafe {
        device.Activate(CLSCTX_ALL, None)
    })?;
    let client2: IAudioClient2 = call("QueryInterface(IAudioClient2)", || client.cast())?;
    let properties = AudioClientProperties {
        cbSize: std::mem::size_of::<AudioClientProperties>() as u32,
        bIsOffload: false.into(),
        eCategory: match case {
            "communications-event" => AudioCategory_Communications,
            "speech-event" => AudioCategory_Speech,
            _ => AudioCategory_Other,
        },
        Options: if case == "raw-event" {
            AUDCLNT_STREAMOPTIONS_RAW
        } else {
            AUDCLNT_STREAMOPTIONS_NONE
        },
    };
    call("IAudioClient2::SetClientProperties", || unsafe {
        client2.SetClientProperties(&properties)
    })?;
    let mix = MixFormat(call("IAudioClient::GetMixFormat", || unsafe {
        client.GetMixFormat()
    })?);
    ensure!(
        !mix.0.is_null(),
        "GetMixFormat succeeded with a null pointer"
    );
    say(format!("MIX {}", describe_format(mix.0)));
    let native = DeviceEnumerator::new()?
        .get_device(options.device.as_deref().context("Missing device ID")?)?
        .get_device_format()?;
    say(format!(
        "DEVICE_FORMAT {}",
        describe_format(native.as_waveformatex_ref())
    ));
    let mono = WAVEFORMATEX {
        wFormatTag: 1,
        nChannels: 1,
        nSamplesPerSec: 48_000,
        nAvgBytesPerSec: 96_000,
        nBlockAlign: 2,
        wBitsPerSample: 16,
        cbSize: 0,
    };
    let exclusive = case == "exclusive-poll";
    let share_mode = if exclusive {
        AUDCLNT_SHAREMODE_EXCLUSIVE
    } else {
        AUDCLNT_SHAREMODE_SHARED
    };
    let share_name = if exclusive { "exclusive" } else { "shared" };
    let format = if exclusive {
        native.as_waveformatex_ref() as *const _
    } else if case == "mono16-event" {
        &mono as *const _
    } else {
        mix.0.cast_const()
    };
    say(format!("REQUEST {}", describe_format(format)));
    let mut closest = std::ptr::null_mut();
    say(format!(
        "CALL IAudioClient::IsFormatSupported({share_name})"
    ));
    let supported = unsafe {
        client.IsFormatSupported(
            share_mode,
            format,
            if exclusive { None } else { Some(&mut closest) },
        )
    };
    say(format!(
        "FORMAT_SUPPORT HRESULT=0x{:08x}",
        supported.0 as u32
    ));
    if !closest.is_null() {
        say(format!("CLOSEST {}", describe_format(closest)));
        unsafe { CoTaskMemFree(Some(closest.cast())) };
    }
    // The mono test deliberately exercises the engine's format conversion path.
    if case != "mono16-event" {
        supported.ok().context("IsFormatSupported")?;
    }
    let events = !matches!(case, "default-poll" | "exclusive-poll");
    let flags = if events {
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK
    } else {
        0
    } | if case == "mono16-event" {
        AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY
    } else {
        0
    };
    call(
        &format!("IAudioClient::Initialize({share_name})"),
        || unsafe {
            client.Initialize(
                share_mode,
                flags,
                1_000_000,
                if exclusive { 100_000 } else { 0 },
                format,
                None,
            )
        },
    )?;
    let event = if events {
        let event = Event(call("CreateEventW", || unsafe {
            CreateEventW(None, false, false, None)
        })?);
        call("IAudioClient::SetEventHandle", || unsafe {
            client.SetEventHandle(event.0)
        })?;
        Some(event)
    } else {
        None
    };
    let buffer_size = call("IAudioClient::GetBufferSize", || unsafe {
        client.GetBufferSize()
    })?;
    let latency = call("IAudioClient::GetStreamLatency", || unsafe {
        client.GetStreamLatency()
    })?;
    say(format!(
        "BUFFER frames={buffer_size} latency_ms={:.3}",
        latency as f64 / 10_000.0
    ));
    let capture: IAudioCaptureClient =
        call("IAudioClient::GetService(IAudioCaptureClient)", || unsafe {
            client.GetService()
        })?;
    let f = unsafe { *format };
    let mut audio = Vec::new();
    let mut stats = Statistics::default();
    call("IAudioClient::Start", || unsafe { client.Start() })?;
    let start = Instant::now();
    let result = (|| -> Result<()> {
        let mut first = true;
        let mut next_report = Duration::from_secs(1);
        while start.elapsed() < Duration::from_secs(options.seconds) {
            if let Some(event) = &event {
                match unsafe { WaitForSingleObject(event.0, 250) } {
                    WAIT_OBJECT_0 => (),
                    WAIT_TIMEOUT => {
                        say("WAIT timeout=250ms (checking for queued packets)");
                    }
                    status => bail!(
                        "WaitForSingleObject returned {status:?}: {}",
                        windows_core::Error::from_thread()
                    ),
                }
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
            while exclusive
                || unsafe { capture.GetNextPacketSize() }
                    .context("IAudioCaptureClient::GetNextPacketSize")?
                    > 0
            {
                let (mut ptr, mut frames, mut flags, mut position, mut qpc) =
                    (std::ptr::null_mut(), 0, 0, 0, 0);
                unsafe {
                    capture.GetBuffer(
                        &mut ptr,
                        &mut frames,
                        &mut flags,
                        Some(&mut position),
                        Some(&mut qpc),
                    )
                }
                .context("IAudioCaptureClient::GetBuffer")?;
                if frames == 0 {
                    break;
                }
                let length = frames as usize * f.nBlockAlign as usize;
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                if first {
                    say(format!(
                        "FIRST_PACKET frames={frames} flags=0x{flags:08x} ptr={ptr:p} position={position} qpc={qpc}"
                    ));
                    first = false;
                }
                let packet_result = if silent {
                    audio.resize(
                        audio.len() + length,
                        if f.wBitsPerSample == 8 { 128 } else { 0 },
                    );
                    stats.samples += frames as u64 * f.nChannels as u64;
                    Ok(())
                } else if ptr.is_null() {
                    Err(anyhow::anyhow!(
                        "GetBuffer returned a null data pointer without SILENT flag"
                    ))
                } else {
                    let packet = unsafe { std::slice::from_raw_parts(ptr, length) };
                    audio.extend_from_slice(packet);
                    stats.add_samples(packet, format)
                };
                unsafe { capture.ReleaseBuffer(frames) }
                    .context("IAudioCaptureClient::ReleaseBuffer")?;
                packet_result?;
                stats.packets += 1;
                stats.frames += frames as u64;
                stats.silent_packets += u64::from(silent);
                stats.discontinuities +=
                    u64::from(flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0);
                stats.timestamp_errors +=
                    u64::from(flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0);
                // A broken engine that never empties its queue must not hang the worker.
                if exclusive || start.elapsed() >= Duration::from_secs(options.seconds) {
                    break;
                }
            }
            if start.elapsed() >= next_report {
                say(format!(
                    "PROGRESS elapsed_ms={} packets={} frames={} peak={:.6}",
                    start.elapsed().as_millis(),
                    stats.packets,
                    stats.frames,
                    stats.peak
                ));
                next_report += Duration::from_secs(1);
            }
        }
        Ok(())
    })();
    let stop = call("IAudioClient::Stop", || unsafe { client.Stop() });
    let rms = (stats.squares / stats.samples.max(1) as f64).sqrt();
    say(format!(
        "RESULT packets={} frames={} captured_seconds={:.3} silent_packets={} nonzero_samples={} invalid_samples={} peak={:.8} rms={rms:.8} discontinuities={} timestamp_errors={}",
        stats.packets,
        stats.frames,
        stats.frames as f64 / f.nSamplesPerSec as f64,
        stats.silent_packets,
        stats.nonzero_samples,
        stats.invalid_samples,
        stats.peak,
        stats.discontinuities,
        stats.timestamp_errors
    ));
    let wav = options.output.join(format!("{case}-{cycle}.wav"));
    write_wav(&wav, &format_bytes(format), &audio, stats.frames as u32)?;
    say(format!("WAV {}", wav.display()));
    stop?;
    result?;
    call("IAudioClient::Reset", || unsafe { client.Reset() })?;
    say("CALL Release(IAudioCaptureClient / IAudioClient2 / IAudioClient / IMMDevice)");
    drop(capture);
    drop(client2);
    drop(client);
    drop(device);
    drop(enumerator);
    drop(event);
    say("OK   Release(all audio interfaces)");
    ensure!(
        stats.frames > 0,
        "Stream started but no capture packets arrived"
    );
    ensure!(
        stats.invalid_samples == 0,
        "Capture produced NaN or infinity"
    );
    if stats.nonzero_samples == 0 {
        say(
            "WARNING all samples are silent; API success does not confirm audible capture. Speak during the test.",
        );
    }
    say(format!("PASS case={case} cycle={cycle}"));
    Ok(())
}

fn registry_snapshot(report: &mut File, id: &str) -> Result<()> {
    if let Some(guid) = id.rsplit('.').next() {
        let path = format!(
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture\{guid}\FxProperties"
        );
        writeln!(report, "REGISTRY HKLM\\{path}")?;
        match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(&path) {
            Ok(key) => {
                for value in key.enum_values() {
                    writeln!(report, "  {value:?}")?;
                }
            }
            Err(error) => writeln!(report, "  READ ERROR {error}")?,
        }
    }
    for path in [
        format!(r"CLSID\{APO_CLSID}\InprocServer32"),
        format!(r"AudioEngine\AudioProcessingObjects\{APO_CLSID}"),
    ] {
        writeln!(report, "REGISTRY HKCR\\{path}")?;
        match RegKey::predef(HKEY_CLASSES_ROOT).open_subkey(&path) {
            Ok(key) => {
                for value in key.enum_values() {
                    writeln!(report, "  {value:?}")?;
                }
            }
            Err(error) => writeln!(report, "  READ ERROR {error}")?,
        }
    }
    let installed = fs::read(r"C:\Windows\System32\voice_box.dll");
    for candidate in [
        "target/voice_box.dll",
        "target/debug/voice_box.dll",
        "target/release/voice_box.dll",
    ] {
        let local = fs::read(candidate);
        writeln!(
            report,
            "DLL {candidate}: installed_bytes={:?} local_bytes={:?} identical={}",
            installed.as_ref().ok().map(Vec::len),
            local.as_ref().ok().map(Vec::len),
            matches!((&installed, &local), (Ok(a), Ok(b)) if a == b)
        )?;
    }
    Ok(())
}

fn parent(options: &Options) -> Result<bool> {
    fs::create_dir_all(&options.output)?;
    let mut report = File::create(options.output.join("report.txt"))?;
    let _com = ComApartment::new()?;
    let enumerator = DeviceEnumerator::new()?;
    let mut default_id = None;
    for role in [Role::Console, Role::Multimedia, Role::Communications] {
        match enumerator.get_default_device_for_role(&Direction::Capture, &role) {
            Ok(device) => {
                let id = device.get_id()?;
                let line = format!(
                    "DEFAULT role={role} name={} id={id}",
                    device.get_friendlyname()?
                );
                say(&line);
                writeln!(report, "{line}")?;
                if role == Role::Console {
                    default_id = Some(id);
                }
            }
            Err(error) => writeln!(report, "DEFAULT role={role} ERROR {error:?}")?,
        }
    }
    for device in &enumerator.get_device_collection(&Direction::Capture)? {
        let device = device?;
        writeln!(
            report,
            "DEVICE name={} id={} state={:?}",
            device.get_friendlyname()?,
            device.get_id()?,
            device.get_state()?
        )?;
        writeln!(report, "DEVICE_FORMAT {:?}", device.get_device_format())?;
    }
    let id = options
        .device
        .clone()
        .or(default_id)
        .context("No default capture endpoint; use --device")?;
    registry_snapshot(&mut report, &id)?;
    if let Ok(manager) = enumerator.get_device(&id)?.get_iaudiosessionmanager()
        && let Ok(sessions) = manager.get_audiosessionenumerator()
    {
        for index in 0..sessions.get_count()? {
            let session = sessions.get_session(index)?;
            writeln!(
                report,
                "SESSION pid={:?} state={:?} name={:?}",
                session.get_process_id(),
                session.get_state(),
                session.get_display_name()
            )?;
        }
    }
    report.flush()?;
    say(format!(
        "Speak into the microphone. Output: {}",
        options.output.display()
    ));
    say("RAW is a mode comparison; an EFX may still run in RAW mode.");
    let mut passed = 0;
    let cases: Vec<&str> = options
        .case
        .as_deref()
        .map(|case| vec![case])
        .unwrap_or_else(|| CASES.to_vec());
    for case in &cases {
        let path = options.output.join(format!("{case}.log"));
        let log = File::create(&path)?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args([
                "--worker",
                "--device",
                &id,
                "--case",
                case,
                "--seconds",
                &options.seconds.to_string(),
                "--cycles",
                &options.cycles.to_string(),
                "--output",
            ])
            .arg(&options.output)
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        if let Some(dll) = &options.apo_dll {
            command.arg("--apo-dll").arg(dll);
        }
        let mut child = command.spawn()?;
        say(format!("TEST {case} child_pid={}", child.id()));
        let start = Instant::now();
        let timeout = Duration::from_secs(options.seconds * options.cycles as u64 + 20);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break Some(status);
            }
            if start.elapsed() >= timeout {
                child.kill()?;
                child.wait()?;
                break None;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let summary = match status {
            Some(status) if status.success() => {
                passed += 1;
                format!("PASS {case}")
            }
            Some(status) => format!(
                "FAIL {case} exit={status} code_hex={:?}",
                status.code().map(|code| format!("0x{:08x}", code as u32))
            ),
            None => format!(
                "HANG {case} exceeded {}s; worker terminated",
                timeout.as_secs()
            ),
        };
        say(&summary);
        writeln!(report, "{summary}")?;
        let detail = fs::read_to_string(&path)?;
        for line in detail.lines().filter(|line| {
            line.starts_with("RESULT")
                || line.starts_with("WARNING")
                || line.starts_with("ERROR")
                || line.starts_with("HRESULT")
                || line.starts_with("CONTRACT_VIOLATION")
                || line.starts_with("CONTRACT_RESULT")
        }) {
            say(line);
        }
        writeln!(report, "LOG {}\n{detail}", path.display())?;
        report.flush()?;
    }
    say(format!(
        "Completed {passed}/{} cases. Report: {}",
        cases.len(),
        options.output.join("report.txt").display()
    ));
    Ok(passed == cases.len())
}

fn run() -> Result<bool> {
    let options = Options::parse()?;
    if options.worker {
        let _com = ComApartment::new()?;
        for cycle in 1..=options.cycles {
            capture_cycle(&options, cycle)?;
            std::thread::sleep(Duration::from_millis(200));
        }
        say("CALL CoUninitialize / process exit");
        drop(_com);
        say("OK   CoUninitialize");
        Ok(true)
    } else {
        parent(&options)
    }
}

fn main() {
    let code = match run() {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(error) => {
            say(format!("ERROR {error:#}"));
            for cause in error.chain() {
                if let Some(error) = cause.downcast_ref::<windows_core::Error>() {
                    say(format!(
                        "HRESULT=0x{:08x} message={}",
                        error.code().0 as u32,
                        error.message()
                    ));
                }
            }
            2
        }
    };
    std::process::exit(code);
}

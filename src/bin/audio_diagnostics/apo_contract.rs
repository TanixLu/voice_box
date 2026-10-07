//! Direct negotiation and processing probes of installed or locally built APOs.
use super::{call, say};
use anyhow::{Context, Result, ensure};
use std::{ffi::c_void, mem::ManuallyDrop, os::windows::ffi::OsStrExt, path::Path, ptr};
use wasapi::{DeviceEnumerator, SampleType, WaveFormat};
use windows::Win32::{
    Foundation::{E_NOTIMPL, E_POINTER, FreeLibrary, HMODULE, S_FALSE, S_OK},
    Media::Audio::{Apo::*, WAVEFORMATEX},
    Media::KernelStreaming::{AUDIO_SIGNALPROCESSINGMODE_DEFAULT, KSDATAFORMAT_SUBTYPE_PCM},
    Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    System::{
        Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, IClassFactory},
        LibraryLoader::{
            GetProcAddress, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
            LoadLibraryExW,
        },
    },
};
use windows_core::{BOOL, GUID, HRESULT, Interface, PCWSTR, Ref, implement, s};

struct LoadedModule(HMODULE);

impl Drop for LoadedModule {
    fn drop(&mut self) {
        // The APO and class factory have already been released. This also lets
        // the DLL clean up its diagnostics provider before it is unloaded.
        if let Some(address) = unsafe { GetProcAddress(self.0, s!("DllCanUnloadNow")) } {
            let can_unload: unsafe extern "system" fn() -> HRESULT =
                unsafe { std::mem::transmute(address) };
            let hr = unsafe { can_unload() };
            if hr != S_OK {
                say(format!(
                    "CONTRACT DLL retained because DllCanUnloadNow HRESULT=0x{:08x}",
                    hr.0 as u32
                ));
                return;
            }
        }
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

struct ApoInstance {
    // Fields drop in declaration order, so no COM vtable outlives its DLL.
    apo: IAudioProcessingObject,
    _module: Option<LoadedModule>,
}

fn create_apo(clsid: &GUID, dll_path: Option<&Path>) -> Result<ApoInstance> {
    let Some(path) = dll_path else {
        let apo = call("CoCreateInstance(installed VoiceBox APO)", || unsafe {
            CoCreateInstance(clsid, None, CLSCTX_INPROC_SERVER)
        })?;
        return Ok(ApoInstance { apo, _module: None });
    };
    let path = path
        .canonicalize()
        .with_context(|| format!("Resolve APO DLL {}", path.display()))?;
    say(format!("CONTRACT loading local APO DLL={}", path.display()));
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let module = LoadedModule(call("LoadLibraryExW(local VoiceBox APO)", || unsafe {
        LoadLibraryExW(
            PCWSTR(wide.as_ptr()),
            None,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
    })?);
    let address = unsafe { GetProcAddress(module.0, s!("DllGetClassObject")) }
        .context("APO DLL does not export DllGetClassObject")?;
    type GetClassObject =
        unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut c_void) -> HRESULT;
    let get_class_object: GetClassObject = unsafe { std::mem::transmute(address) };
    let mut factory = ptr::null_mut();
    call("DllGetClassObject(local VoiceBox APO)", || unsafe {
        get_class_object(clsid, &IClassFactory::IID, &mut factory).ok()
    })?;
    ensure!(!factory.is_null(), "DLL returned a null class factory");
    let factory = unsafe { IClassFactory::from_raw(factory) };
    let apo = call(
        "IClassFactory::CreateInstance(local VoiceBox APO)",
        || unsafe { factory.CreateInstance(None) },
    )?;
    drop(factory);
    Ok(ApoInstance {
        apo,
        _module: Some(module),
    })
}

// Provide the same media-type interface the audio engine passes to an APO,
// keeping the format allocation alive for the duration of every COM call.
#[implement(IAudioMediaType)]
struct MediaType {
    wave: WaveFormat,
    uncompressed: UNCOMPRESSEDAUDIOFORMAT,
}

impl IAudioMediaType_Impl for MediaType_Impl {
    fn IsCompressedFormat(&self) -> windows_core::Result<BOOL> {
        Ok(false.into())
    }
    fn IsEqual(&self, _: Ref<IAudioMediaType>) -> windows_core::Result<u32> {
        Err(E_NOTIMPL.into())
    }
    fn GetAudioFormat(&self) -> *mut WAVEFORMATEX {
        self.wave.as_waveformatex_ref() as *const _ as *mut _
    }
    fn GetUncompressedAudioFormat(
        &self,
        output: *mut UNCOMPRESSEDAUDIOFORMAT,
    ) -> windows_core::Result<()> {
        if output.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { output.write(self.uncompressed) };
        Ok(())
    }
}

fn media_type(wave: WaveFormat) -> Result<IAudioMediaType> {
    let uncompressed = UNCOMPRESSEDAUDIOFORMAT {
        guidFormatType: match wave.get_subformat()? {
            SampleType::Float => KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
            SampleType::Int => KSDATAFORMAT_SUBTYPE_PCM,
        },
        dwSamplesPerFrame: wave.get_nchannels() as u32,
        dwBytesPerSampleContainer: wave.get_bitspersample() as u32 / 8,
        dwValidBitsPerSample: wave.get_validbitspersample() as u32,
        fFramesPerSecond: wave.get_samplespersec() as f32,
        dwChannelMask: wave.get_dwchannelmask(),
    };
    Ok(MediaType { wave, uncompressed }.into())
}

fn describe(media: &IAudioMediaType) -> Result<UNCOMPRESSEDAUDIOFORMAT> {
    let mut format = UNCOMPRESSEDAUDIOFORMAT::default();
    unsafe { media.GetUncompressedAudioFormat(&mut format) }?;
    Ok(format)
}

fn same_format(a: &UNCOMPRESSEDAUDIOFORMAT, b: &UNCOMPRESSEDAUDIOFORMAT) -> bool {
    a.guidFormatType == b.guidFormatType
        && a.dwSamplesPerFrame == b.dwSamplesPerFrame
        && a.dwBytesPerSampleContainer == b.dwBytesPerSampleContainer
        && a.dwValidBitsPerSample == b.dwValidBitsPerSample
        && a.fFramesPerSecond == b.fFramesPerSecond
        && a.dwChannelMask == b.dwChannelMask
}

fn lock_pair(
    apo: &IAudioProcessingObject,
    input: &IAudioMediaType,
    output: &IAudioMediaType,
) -> Result<bool> {
    let configuration: IAudioProcessingObjectConfiguration = apo.cast()?;
    let mut descriptors = [input, output].map(|media| APO_CONNECTION_DESCRIPTOR {
        Type: APO_CONNECTION_BUFFER_TYPE_EXTERNAL,
        u32MaxFrameCount: 480,
        pFormat: ManuallyDrop::new(Some(media.clone())),
        ..Default::default()
    });
    let locked = unsafe { configuration.LockForProcess(&[&descriptors[0]], &[&descriptors[1]]) };
    say(format!("CONTRACT LockForProcess result={locked:?}"));
    for descriptor in &mut descriptors {
        unsafe { ManuallyDrop::drop(&mut descriptor.pFormat) };
    }
    if locked.is_ok() {
        unsafe { configuration.UnlockForProcess() }?;
    }
    Ok(locked.is_ok())
}

#[derive(Clone, Copy)]
enum Direction {
    Input,
    Output,
}

fn check_negotiation(
    apo: &IAudioProcessingObject,
    flags: APO_FLAG,
    direction: Direction,
    name: &str,
    requested: &IAudioMediaType,
    opposite: Option<&IAudioMediaType>,
    require_success: bool,
) -> Result<u32> {
    let (method, negotiate) = match direction {
        Direction::Input => (
            "IsInputFormatSupported",
            apo.vtable().IsInputFormatSupported,
        ),
        Direction::Output => (
            "IsOutputFormatSupported",
            apo.vtable().IsOutputFormatSupported,
        ),
    };
    let label = format!("{method}/{name}");
    let mut supported = ptr::null_mut();
    let hr = unsafe {
        negotiate(
            apo.as_raw(),
            opposite.map_or(ptr::null_mut(), Interface::as_raw),
            requested.as_raw(),
            &mut supported,
        )
    };
    say(format!("CONTRACT {label}: HRESULT=0x{:08x}", hr.0 as u32));
    let mut violations = 0;
    let mut violation = |reason: &str| {
        say(format!("CONTRACT_VIOLATION {label}: {reason}"));
        violations += 1;
    };
    if hr.is_err() {
        if !supported.is_null() {
            drop(unsafe { IAudioMediaType::from_raw(supported) });
            violation("failed negotiation returned a supported format");
        }
        if require_success {
            violation("rejected a supported float32 format or a compatible suggestion");
        }
        return Ok(violations);
    }
    if supported.is_null() {
        violation("successful negotiation returned a null supported format");
        return Ok(violations);
    }
    let supported = unsafe { IAudioMediaType::from_raw(supported) };
    let format = describe(&supported)?;
    let request = describe(requested)?;
    let exact_request = same_format(&format, &request);
    if hr == S_OK {
        if !exact_request {
            violation("S_OK returned a format different from the request");
        }
    } else if hr == S_FALSE {
        if exact_request {
            violation("S_FALSE returned the unchanged request instead of an alternative");
        }
    } else {
        violation("unexpected success HRESULT (expected S_OK or S_FALSE)");
    }
    if let Some(opposite) = opposite {
        let opposite_format = describe(opposite)?;
        say(format!(
            "CONTRACT {label}: supported={format:?} opposite={opposite_format:?}"
        ));
        if !same_format(&format, &opposite_format) {
            violation("accepted or suggested a format incompatible with the opposite connection");
        }
        let violates_flags = (flags.0 & APO_FLAG_BITSPERSAMPLE_MUST_MATCH.0 != 0
            && (format.dwBytesPerSampleContainer != opposite_format.dwBytesPerSampleContainer
                || format.dwValidBitsPerSample != opposite_format.dwValidBitsPerSample))
            || (flags.0 & APO_FLAG_SAMPLESPERFRAME_MUST_MATCH.0 != 0
                && format.dwSamplesPerFrame != opposite_format.dwSamplesPerFrame)
            || (flags.0 & APO_FLAG_FRAMESPERSECOND_MUST_MATCH.0 != 0
                && format.fFramesPerSecond != opposite_format.fFramesPerSecond);
        if violates_flags {
            violation("accepted a pair that contradicts MUST_MATCH registration flags");
        }
    } else {
        say(format!(
            "CONTRACT {label}: supported={format:?} opposite=null"
        ));
    }
    // Always lock the actual negotiated pair, including S_FALSE alternatives.
    // With no opposite connection, the format must work on both sides.
    let pair = opposite.unwrap_or(&supported);
    let (input, output) = match direction {
        Direction::Input => (&supported, pair),
        Direction::Output => (pair, &supported),
    };
    if !lock_pair(apo, input, output).context("APO direct LockForProcess probe")? {
        violation("successful negotiation produced a pair rejected by LockForProcess");
    }
    Ok(violations)
}

fn check_in_place_processing(apo: &IAudioProcessingObject, channels: usize) -> Result<u32> {
    const FRAMES: usize = 64;
    let label = format!("in-place-float32-{channels}ch");
    let format = media_type(WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        48_000,
        channels,
        None,
    ))?;
    let configuration: IAudioProcessingObjectConfiguration = apo.cast()?;
    let realtime: IAudioProcessingObjectRT = apo.cast()?;
    let mut samples = vec![0.0_f32; FRAMES * channels];
    let mut descriptor = APO_CONNECTION_DESCRIPTOR {
        Type: APO_CONNECTION_BUFFER_TYPE_EXTERNAL,
        pBuffer: samples.as_mut_ptr() as usize,
        u32MaxFrameCount: FRAMES as u32,
        pFormat: ManuallyDrop::new(Some(format)),
        ..Default::default()
    };
    let locked = unsafe { configuration.LockForProcess(&[&descriptor], &[&descriptor]) };
    unsafe { ManuallyDrop::drop(&mut descriptor.pFormat) };
    say(format!(
        "CONTRACT {label}: LockForProcess result={locked:?}"
    ));
    if locked.is_err() {
        say(format!(
            "CONTRACT_VIOLATION {label}: matching float32 pair rejected"
        ));
        return Ok(1);
    }
    let mut violations = 0;
    let mut property = APO_CONNECTION_PROPERTY {
        pBuffer: samples.as_mut_ptr() as usize,
        u32ValidFrameCount: FRAMES as u32,
        u32BufferFlags: BUFFER_VALID,
        ..Default::default()
    };
    // One shared connection property is deliberately used for input and output,
    // exactly as the engine may do for an APO registered with APO_FLAG_INPLACE.
    let mut output = ptr::from_mut(&mut property);
    let input = output.cast_const();
    for channel in 0..channels {
        samples[channel] = 0.5;
        samples[channels + channel] = -0.25;
    }
    unsafe { realtime.APOProcess(1, &input, 1, &mut output) };
    if property.u32BufferFlags != BUFFER_VALID
        || property.u32ValidFrameCount != FRAMES as u32
        || !samples[..channels * 2]
            .iter()
            .all(|sample| sample.is_finite() && sample.abs() > 0.1)
    {
        say(format!(
            "CONTRACT_VIOLATION {label}: valid aliased input lost audio or output validity; flags={:?} frames={} first_samples={:?}",
            property.u32BufferFlags,
            property.u32ValidFrameCount,
            &samples[..channels * 2]
        ));
        violations += 1;
    }
    if samples[channels * 2..].iter().any(|sample| *sample != 0.0) {
        say(format!(
            "CONTRACT_VIOLATION {label}: impulse created a delayed tail"
        ));
        violations += 1;
    }
    samples.fill(0.0);
    property.u32BufferFlags = BUFFER_VALID;
    unsafe { realtime.APOProcess(1, &input, 1, &mut output) };
    if property.u32BufferFlags != BUFFER_VALID || samples.iter().any(|sample| *sample != 0.0) {
        say(format!(
            "CONTRACT_VIOLATION {label}: zero valid input created audio or lost validity"
        ));
        violations += 1;
    }
    // A silent buffer need not contain initialized samples on input. The APO
    // must clear the output without confusing its aliased flags with INVALID.
    samples.fill(0.5);
    property.u32BufferFlags = BUFFER_SILENT;
    unsafe { realtime.APOProcess(1, &input, 1, &mut output) };
    if property.u32BufferFlags != BUFFER_SILENT
        || property.u32ValidFrameCount != FRAMES as u32
        || samples.iter().any(|sample| *sample != 0.0)
    {
        say(format!(
            "CONTRACT_VIOLATION {label}: silent aliased input was not preserved"
        ));
        violations += 1;
    }
    unsafe { configuration.UnlockForProcess() }?;
    say(format!(
        "CONTRACT {label}: processing_violations={violations}"
    ));
    Ok(violations)
}

pub fn run(device_id: &str, dll_path: Option<&Path>) -> Result<()> {
    let device = DeviceEnumerator::new()?.get_device(device_id)?;
    let native = media_type(device.get_device_format()?)?;
    let mix_wave = device.get_iaudioclient()?.get_mixformat()?;
    let mix = media_type(mix_wave.clone())?;
    say(format!("CONTRACT native={:?}", describe(&native)?));
    say(format!("CONTRACT mix={:?}", describe(&mix)?));
    let clsid = GUID::from_u128(0xae672075_b244_4417_b5a5_fbd45b7f0268);
    let instance = create_apo(&clsid, dll_path)?;
    let apo = &instance.apo;
    let properties = call("APO::GetRegistrationProperties", || unsafe {
        apo.GetRegistrationProperties()
    })?;
    ensure!(
        !properties.is_null(),
        "APO returned null registration properties"
    );
    let flags = unsafe { (*properties).Flags };
    unsafe { CoTaskMemFree(Some(properties.cast())) };
    say(format!("CONTRACT registered_flags=0x{:08x}", flags.0));
    let init = APOInitSystemEffects3 {
        APOInit: APOInitBaseStruct {
            cbSize: std::mem::size_of::<APOInitSystemEffects3>() as u32,
            clsid,
        },
        AudioProcessingMode: AUDIO_SIGNALPROCESSINGMODE_DEFAULT,
        ..Default::default()
    };
    let bytes = unsafe {
        std::slice::from_raw_parts((&raw const init).cast::<u8>(), std::mem::size_of_val(&init))
    };
    call("APO::Initialize", || unsafe { apo.Initialize(bytes) })?;
    let sample_type = mix_wave.get_subformat()?;
    let channels = mix_wave.get_nchannels() as usize;
    let rate = mix_wave.get_samplespersec() as usize;
    let make_variant = |variant_channels, variant_rate, mask| {
        media_type(WaveFormat::new(
            mix_wave.get_bitspersample() as usize,
            mix_wave.get_validbitspersample() as usize,
            &sample_type,
            variant_rate,
            variant_channels,
            mask,
        ))
    };
    let different_channels = make_variant(if channels == 1 { 2 } else { 1 }, rate, None)?;
    let different_rate = make_variant(
        channels,
        if rate == 48_000 { 44_100 } else { 48_000 },
        Some(mix_wave.get_dwchannelmask()),
    )?;
    let different_mask = make_variant(
        channels,
        rate,
        Some(if mix_wave.get_dwchannelmask() == 0 {
            (1_u32 << channels.min(18)) - 1
        } else {
            0
        }),
    )?;
    let mut violations = 0;
    let cases = [
        ("mix/null-opposite", &mix, None, true),
        ("native/null-opposite", &native, None, false),
        ("mix/matching", &mix, Some(&mix), true),
        ("native/matching", &native, Some(&native), false),
        ("native-request/mix-opposite", &native, Some(&mix), true),
        ("mix-request/native-opposite", &mix, Some(&native), false),
        (
            "different-channels/mix-opposite",
            &different_channels,
            Some(&mix),
            true,
        ),
        (
            "different-rate/mix-opposite",
            &different_rate,
            Some(&mix),
            true,
        ),
        (
            "different-mask/mix-opposite",
            &different_mask,
            Some(&mix),
            true,
        ),
    ];
    for direction in [Direction::Input, Direction::Output] {
        for (name, requested, opposite, require_success) in cases {
            violations += check_negotiation(
                apo,
                flags,
                direction,
                name,
                requested,
                opposite,
                require_success,
            )?;
        }
    }
    for channels in [1, 2] {
        violations += check_in_place_processing(apo, channels)?;
    }
    say(format!("CONTRACT_RESULT violations={violations}"));
    ensure!(
        violations == 0,
        "APO negotiation or processing contract is inconsistent ({violations} violations)"
    );
    Ok(())
}

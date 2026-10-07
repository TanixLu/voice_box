use super::*;
use std::mem::ManuallyDrop;
use windows::Win32::Media::Audio::{Apo::*, WAVEFORMATEX};
use windows::Win32::System::Com::CoTaskMemFree;
use windows_core::{Ref, implement};

#[implement(IAudioMediaType)]
struct MediaType {
    format: UNCOMPRESSEDAUDIOFORMAT,
}

impl IAudioMediaType_Impl for MediaType_Impl {
    fn IsCompressedFormat(&self) -> windows_core::Result<BOOL> {
        Ok(false.into())
    }
    fn IsEqual(&self, _: Ref<IAudioMediaType>) -> windows_core::Result<u32> {
        Err(E_NOTIMPL.into())
    }
    fn GetAudioFormat(&self) -> *mut WAVEFORMATEX {
        ptr::null_mut()
    }
    fn GetUncompressedAudioFormat(
        &self,
        output: *mut UNCOMPRESSEDAUDIOFORMAT,
    ) -> windows_core::Result<()> {
        if output.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { output.write(self.format) };
        Ok(())
    }
}

fn format(subtype: GUID, bytes: u32, bits: u32, channels: u32, rate: f32) -> IAudioMediaType {
    MediaType {
        format: UNCOMPRESSEDAUDIOFORMAT {
            guidFormatType: subtype,
            dwSamplesPerFrame: channels,
            dwBytesPerSampleContainer: bytes,
            dwValidBitsPerSample: bits,
            fFramesPerSecond: rate,
            dwChannelMask: 0,
        },
    }
    .into()
}

fn float(channels: u32) -> IAudioMediaType {
    format(KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, 4, 32, channels, 48_000.0)
}

fn instance(mode: GUID) -> IAudioProcessingObject {
    let object = create_apo(ptr::null_mut()).unwrap();
    let identity = unsafe { IUnknown::from_raw(ptr::addr_of_mut!((*object).inner_unknown).cast()) };
    let apo: IAudioProcessingObject = identity.cast().unwrap();
    let init = APOInitSystemEffects3 {
        APOInit: APOInitBaseStruct {
            cbSize: size_of::<APOInitSystemEffects3>() as u32,
            clsid: CLSID_VOICE_BOX_APO,
        },
        AudioProcessingMode: mode,
        ..Default::default()
    };
    let bytes =
        unsafe { core::slice::from_raw_parts((&raw const init).cast(), size_of_val(&init)) };
    unsafe { apo.Initialize(bytes).unwrap() };
    apo
}

// The descriptor borrows its media type; it must not release this ManuallyDrop.
fn descriptor(media: &IAudioMediaType, frames: u32) -> APO_CONNECTION_DESCRIPTOR {
    APO_CONNECTION_DESCRIPTOR {
        Type: APO_CONNECTION_BUFFER_TYPE_ALLOCATED,
        u32MaxFrameCount: frames,
        pFormat: ManuallyDrop::new(Some(unsafe { IAudioMediaType::from_raw(media.as_raw()) })),
        u32Signature: 0x41434453,
        ..Default::default()
    }
}

fn query(
    apo: &IAudioProcessingObject,
    output_side: bool,
    requested: *mut c_void,
    opposite: *mut c_void,
    supported: *mut *mut c_void,
) -> HRESULT {
    let function = if output_side {
        apo.vtable().IsOutputFormatSupported
    } else {
        apo.vtable().IsInputFormatSupported
    };
    unsafe { function(apo.as_raw(), opposite, requested, supported) }
}

fn process(apo: &IAudioProcessingObject, property: &mut APO_CONNECTION_PROPERTY) {
    let realtime: IAudioProcessingObjectRT = apo.cast().unwrap();
    let mut output = ptr::from_mut(property);
    let input = output.cast_const();
    unsafe { realtime.APOProcess(1, &input, 1, &mut output) };
}

#[test]
fn capacities_are_checked_before_any_buffer_access() {
    let apo = instance(AUDIO_SIGNALPROCESSINGMODE_DEFAULT);
    let config: IAudioProcessingObjectConfiguration = apo.cast().unwrap();
    let stereo = float(2);
    let input = descriptor(&stereo, 480);
    let output = descriptor(&stereo, 1);
    assert_eq!(
        unsafe { config.LockForProcess(&[&input], &[&output]) }
            .unwrap_err()
            .code(),
        APOERR_INVALID_OUTPUT_MAXFRAMECOUNT
    );
    let zero = descriptor(&stereo, 0);
    assert!(unsafe { config.LockForProcess(&[&zero], &[&zero]) }.is_err());
    let huge_format = format(KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, 8, 64, u32::MAX, 48_000.0);
    let huge = descriptor(&huge_format, u32::MAX);
    assert!(unsafe { config.LockForProcess(&[&huge], &[&huge]) }.is_err());

    let one = descriptor(&stereo, 1);
    unsafe { config.LockForProcess(&[&one], &[&one]).unwrap() };
    // The second frame is a canary: old code would overwrite it without
    // accessing outside the allocation, making the regression safe to run.
    let mut samples = [0.5_f32; 4];
    let before = samples;
    let mut property = APO_CONNECTION_PROPERTY {
        pBuffer: samples.as_mut_ptr() as usize,
        u32ValidFrameCount: 2,
        u32BufferFlags: BUFFER_VALID,
        ..Default::default()
    };
    process(&apo, &mut property);
    assert_eq!(property.u32BufferFlags, BUFFER_INVALID);
    assert_eq!(property.u32ValidFrameCount, 0);
    assert_eq!(samples, before);
    unsafe { config.UnlockForProcess().unwrap() };
}

#[test]
fn negotiation_failure_preserves_output_and_locked_channel_count() {
    let apo = instance(AUDIO_SIGNALPROCESSINGMODE_DEFAULT);
    let stereo = float(2);
    let mono = float(1);
    let unsupported = format(KSDATAFORMAT_SUBTYPE_PCM, 8, 64, 2, 48_000.0);
    let config: IAudioProcessingObjectConfiguration = apo.cast().unwrap();
    let descriptor = descriptor(&stereo, 64);
    unsafe {
        config
            .LockForProcess(&[&descriptor], &[&descriptor])
            .unwrap()
    };
    for output_side in [false, true] {
        let mut selected = ptr::null_mut();
        assert_eq!(
            query(
                &apo,
                output_side,
                mono.as_raw(),
                ptr::null_mut(),
                &mut selected
            ),
            S_OK
        );
        drop(unsafe { IAudioMediaType::from_raw(selected) });
        assert_eq!(unsafe { apo.GetInputChannelCount().unwrap() }, 2);
        let mut selected = ptr::null_mut();
        assert_eq!(
            query(
                &apo,
                output_side,
                mono.as_raw(),
                stereo.as_raw(),
                &mut selected
            ),
            S_FALSE
        );
        assert_eq!(selected, stereo.as_raw());
        drop(unsafe { IAudioMediaType::from_raw(selected) });
        assert_eq!(unsafe { apo.GetInputChannelCount().unwrap() }, 2);
        for requested in [unsupported.as_raw(), ptr::null_mut()] {
            let sentinel = mono.as_raw();
            let mut selected = sentinel;
            assert!(query(&apo, output_side, requested, stereo.as_raw(), &mut selected).is_err());
            assert_eq!(selected, sentinel);
        }
        let mut selected = mono.as_raw();
        assert_eq!(
            query(
                &apo,
                output_side,
                mono.as_raw(),
                unsupported.as_raw(),
                &mut selected
            ),
            APOERR_FORMAT_NOT_SUPPORTED
        );
        assert_eq!(selected, mono.as_raw());
        assert_eq!(
            query(
                &apo,
                output_side,
                mono.as_raw(),
                ptr::null_mut(),
                ptr::null_mut()
            ),
            E_POINTER
        );
    }
    unsafe { config.UnlockForProcess().unwrap() };
}

#[test]
fn raw_processing_and_effect_discovery_are_consistent() {
    for mode in [
        AUDIO_SIGNALPROCESSINGMODE_RAW,
        AUDIO_SIGNALPROCESSINGMODE_DEFAULT,
    ] {
        let apo = instance(mode);
        let is_raw = mode == AUDIO_SIGNALPROCESSINGMODE_RAW;
        let effects: IAudioSystemEffects2 = apo.cast().unwrap();
        let effects3: IAudioSystemEffects3 = apo.cast().unwrap();
        let mut list = ptr::null_mut();
        let mut count = 99;
        unsafe {
            effects
                .GetEffectsList(&mut list, &mut count, HANDLE::default())
                .unwrap()
        };
        assert_eq!(count, u32::from(!is_raw));
        assert_eq!(list.is_null(), is_raw);
        unsafe { CoTaskMemFree(Some(list.cast())) };
        let mut list3 = ptr::null_mut();
        unsafe {
            effects3
                .GetControllableSystemEffectsList(&mut list3, &mut count, None)
                .unwrap()
        };
        assert_eq!(count, u32::from(!is_raw));
        assert_eq!(list3.is_null(), is_raw);
        unsafe { CoTaskMemFree(Some(list3.cast())) };
        let media = float(1);
        let descriptor = descriptor(&media, 64);
        let config: IAudioProcessingObjectConfiguration = apo.cast().unwrap();
        unsafe {
            config
                .LockForProcess(&[&descriptor], &[&descriptor])
                .unwrap()
        };
        let mut samples = [0.5_f32; 64];
        let mut property = APO_CONNECTION_PROPERTY {
            pBuffer: samples.as_mut_ptr() as usize,
            u32ValidFrameCount: 64,
            u32BufferFlags: BUFFER_VALID,
            ..Default::default()
        };
        process(&apo, &mut property);
        assert_eq!(property.u32BufferFlags, BUFFER_VALID);
        assert_eq!(samples.iter().all(|sample| *sample == 0.5), is_raw);
        unsafe { config.UnlockForProcess().unwrap() };
    }
}

#[test]
fn reset_and_lifecycle_overlap_do_not_alias_the_dsp() {
    let apo = instance(AUDIO_SIGNALPROCESSINGMODE_DEFAULT);
    let config: IAudioProcessingObjectConfiguration = apo.cast().unwrap();
    let media = float(1);
    let descriptor = descriptor(&media, 64);
    unsafe {
        config
            .LockForProcess(&[&descriptor], &[&descriptor])
            .unwrap()
    };
    let object = unsafe { object_from(apo.as_raw()) };
    let guard = unsafe { DspAccess::try_acquire(&(*object).dsp_in_use) }.unwrap();
    unsafe { apo.Reset().unwrap() };
    let mut samples = [0.5_f32; 64];
    let mut property = APO_CONNECTION_PROPERTY {
        pBuffer: samples.as_mut_ptr() as usize,
        u32ValidFrameCount: 64,
        u32BufferFlags: BUFFER_VALID,
        ..Default::default()
    };
    process(&apo, &mut property);
    assert_eq!(property.u32BufferFlags, BUFFER_INVALID);
    assert_eq!(samples, [0.5; 64]);
    unsafe { config.UnlockForProcess().unwrap() };
    assert_eq!(
        unsafe { config.LockForProcess(&[&descriptor], &[&descriptor]) }
            .unwrap_err()
            .code(),
        APOERR_APO_LOCKED
    );
    drop(guard);
    unsafe {
        config
            .LockForProcess(&[&descriptor], &[&descriptor])
            .unwrap()
    };
    let resetter = apo.clone();
    // Our free-threaded carrier supports direct calls from either thread. Keep
    // the owning reference here until join; the worker borrows the raw interface.
    let resetter_address = resetter.as_raw() as usize;
    let worker = std::thread::spawn(move || {
        let resetter = ManuallyDrop::new(unsafe {
            IAudioProcessingObject::from_raw(resetter_address as *mut c_void)
        });
        for _ in 0..2_000 {
            unsafe { resetter.Reset().unwrap() };
        }
    });
    for _ in 0..128 {
        samples.fill(0.5);
        property.u32BufferFlags = BUFFER_VALID;
        property.u32ValidFrameCount = 64;
        process(&apo, &mut property);
        assert_eq!(property.u32BufferFlags, BUFFER_VALID);
        assert!(samples.iter().all(|sample| sample.is_finite()));
    }
    worker.join().unwrap();
    unsafe { apo.Reset().unwrap() };
    samples.fill(0.5);
    process(&apo, &mut property);
    assert_eq!(samples[0], 0.5);
    unsafe { config.UnlockForProcess().unwrap() };
}

#[test]
fn pcm_and_float_formats_process_across_rates_and_channel_counts() {
    let sample_formats = [
        (KSDATAFORMAT_SUBTYPE_PCM, 1, 8),
        (KSDATAFORMAT_SUBTYPE_PCM, 1, 4),
        (KSDATAFORMAT_SUBTYPE_PCM, 2, 16),
        (KSDATAFORMAT_SUBTYPE_PCM, 2, 12),
        (KSDATAFORMAT_SUBTYPE_PCM, 3, 24),
        (KSDATAFORMAT_SUBTYPE_PCM, 3, 20),
        (KSDATAFORMAT_SUBTYPE_PCM, 4, 32),
        (KSDATAFORMAT_SUBTYPE_PCM, 4, 24),
        (KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, 4, 32),
        (KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, 8, 64),
    ];
    for (subtype, bytes, bits) in sample_formats {
        for rate in [8_000.0, 44_100.0, 48_000.0, 192_000.0] {
            for channels in [1, 2, 6, 8] {
                let apo = instance(AUDIO_SIGNALPROCESSINGMODE_DEFAULT);
                let media = format(subtype, bytes, bits, channels, rate);
                let descriptor = descriptor(&media, 512);
                let config: IAudioProcessingObjectConfiguration = apo.cast().unwrap();
                unsafe {
                    config
                        .LockForProcess(&[&descriptor], &[&descriptor])
                        .unwrap()
                };
                let encoded = if subtype == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                    if bytes == 4 {
                        0.5_f32.to_le_bytes().to_vec()
                    } else {
                        0.5_f64.to_le_bytes().to_vec()
                    }
                } else {
                    let value = (1_i32 << (bits - 2)) << (bytes * 8 - bits);
                    if bytes == 1 {
                        vec![(value + 128) as u8]
                    } else {
                        value.to_le_bytes()[..bytes as usize].to_vec()
                    }
                };
                let mut buffer = encoded.repeat(512 * channels as usize);
                let original = buffer.clone();
                let mut property = APO_CONNECTION_PROPERTY {
                    pBuffer: buffer.as_mut_ptr() as usize,
                    u32ValidFrameCount: 512,
                    u32BufferFlags: BUFFER_VALID,
                    ..Default::default()
                };
                process(&apo, &mut property);
                assert_eq!(property.u32BufferFlags, BUFFER_VALID);
                assert_eq!(property.u32ValidFrameCount, 512);
                assert_eq!(&buffer[..bytes as usize], encoded);
                assert_ne!(
                    buffer, original,
                    "effect missing: bytes={bytes} bits={bits} rate={rate} channels={channels}"
                );
                buffer.fill(0xff);
                property.u32BufferFlags = BUFFER_SILENT;
                process(&apo, &mut property);
                let silence = if subtype == KSDATAFORMAT_SUBTYPE_PCM && bytes == 1 {
                    128
                } else {
                    0
                };
                assert_eq!(property.u32BufferFlags, BUFFER_SILENT);
                if subtype == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                    assert!(buffer.chunks_exact(bytes as usize).all(|sample| {
                        if bytes == 4 {
                            f32::from_le_bytes(sample.try_into().unwrap()) == 0.0
                        } else {
                            f64::from_le_bytes(sample.try_into().unwrap()) == 0.0
                        }
                    }));
                } else {
                    assert!(buffer.iter().all(|byte| *byte == silence));
                }
                unsafe { config.UnlockForProcess().unwrap() };
            }
        }
    }
}

#[test]
fn signed_pcm_extremes_and_padding_follow_the_carrier() {
    const FRAMES: usize = 1_201;
    for bytes in 1..=4 {
        for bits in [bytes * 8, bytes * 8 - 4] {
            let apo = instance(AUDIO_SIGNALPROCESSINGMODE_DEFAULT);
            let media = format(KSDATAFORMAT_SUBTYPE_PCM, bytes, bits, 4, 48_000.0);
            let descriptor = descriptor(&media, FRAMES as u32);
            let config: IAudioProcessingObjectConfiguration = apo.cast().unwrap();
            unsafe {
                config
                    .LockForProcess(&[&descriptor], &[&descriptor])
                    .unwrap()
            };
            let limit = 1_i64 << (bits - 1);
            let values = [-limit, limit - 1, -limit / 2, 0];
            let padding = bytes * 8 - bits;
            let mut frame = Vec::new();
            for value in values {
                if bytes == 1 {
                    frame.push(((value << padding) + 128) as u8);
                } else {
                    frame.extend_from_slice(
                        &((value << padding) as i32).to_le_bytes()[..bytes as usize],
                    );
                }
            }
            let mut buffer = frame.repeat(FRAMES);
            let mut property = APO_CONNECTION_PROPERTY {
                pBuffer: buffer.as_mut_ptr() as usize,
                u32ValidFrameCount: FRAMES as u32,
                u32BufferFlags: BUFFER_VALID,
                ..Default::default()
            };
            process(&apo, &mut property);
            assert_eq!(property.u32BufferFlags, BUFFER_VALID);
            for (index, frame) in buffer.chunks_exact(bytes as usize * 4).enumerate() {
                let carrier = (core::f64::consts::TAU * index as f64 * 80.0 / 48_000.0).cos();
                for (sample, input) in frame.chunks_exact(bytes as usize).zip(values) {
                    let container = if bytes == 1 {
                        i64::from(sample[0]) - 128
                    } else {
                        let mut encoded = [if sample[bytes as usize - 1] & 0x80 != 0 {
                            0xff
                        } else {
                            0
                        }; 4];
                        encoded[..bytes as usize].copy_from_slice(sample);
                        i64::from(i32::from_le_bytes(encoded))
                    };
                    assert_eq!(
                        container & ((1_i64 << padding) - 1),
                        0,
                        "padding changed: bytes={bytes} bits={bits}"
                    );
                    let actual = container >> padding;
                    let expected = (input as f64 * carrier)
                        .round()
                        .clamp(-limit as f64, (limit - 1) as f64)
                        as i64;
                    assert!(
                        (actual - expected).abs() <= 1,
                        "bytes={bytes} bits={bits} frame={index} input={input} actual={actual} expected={expected}"
                    );
                }
            }
            unsafe { config.UnlockForProcess().unwrap() };
        }
    }
}

#[test]
fn block_boundaries_preserve_phase_and_nonfinite_samples_are_sanitized() {
    for bytes in [4, 8] {
        let media = format(
            KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
            bytes,
            bytes * 8,
            2,
            44_100.0,
        );
        let render = |blocks: &[usize]| {
            let apo = instance(AUDIO_SIGNALPROCESSINGMODE_DEFAULT);
            let config: IAudioProcessingObjectConfiguration = apo.cast().unwrap();
            let descriptor = descriptor(&media, 601);
            unsafe {
                config
                    .LockForProcess(&[&descriptor], &[&descriptor])
                    .unwrap()
            };
            let mut result = Vec::new();
            for &frames in blocks {
                let mut buffer = Vec::new();
                for frame in 0..frames {
                    for value in [
                        if result.is_empty() && frame == 0 {
                            f64::NAN
                        } else {
                            0.5
                        },
                        f64::INFINITY,
                    ] {
                        if bytes == 4 {
                            buffer.extend_from_slice(&(value as f32).to_le_bytes());
                        } else {
                            buffer.extend_from_slice(&value.to_le_bytes());
                        }
                    }
                }
                let mut property = APO_CONNECTION_PROPERTY {
                    pBuffer: buffer.as_mut_ptr() as usize,
                    u32ValidFrameCount: frames as u32,
                    u32BufferFlags: BUFFER_VALID,
                    ..Default::default()
                };
                process(&apo, &mut property);
                assert_eq!(property.u32BufferFlags, BUFFER_VALID);
                result.extend_from_slice(&buffer);
            }
            unsafe { config.UnlockForProcess().unwrap() };
            result
        };
        let whole = render(&[601]);
        let split = render(&[1, 7, 63, 128, 2, 400]);
        assert_eq!(
            whole, split,
            "phase reset across buffers, float width {bytes}"
        );
        for (index, sample) in whole.chunks_exact(bytes as usize).enumerate() {
            let value = if bytes == 4 {
                f64::from(f32::from_le_bytes(sample.try_into().unwrap()))
            } else {
                f64::from_le_bytes(sample.try_into().unwrap())
            };
            assert!(value.is_finite());
            if index == 0 || index % 2 == 1 {
                assert_eq!(value, 0.0);
            }
        }
    }
}

#[test]
fn initialization_versions_unaligned_data_and_discovery_lifecycle() {
    let make_uninitialized = || {
        let object = create_apo(ptr::null_mut()).unwrap();
        let identity =
            unsafe { IUnknown::from_raw(ptr::addr_of_mut!((*object).inner_unknown).cast()) };
        identity.cast::<IAudioProcessingObject>().unwrap()
    };
    let base = APOInitBaseStruct {
        cbSize: 0,
        clsid: CLSID_VOICE_BOX_APO,
    };
    let init1 = APOInitSystemEffects {
        APOInit: base,
        ..Default::default()
    };
    let init2 = APOInitSystemEffects2 {
        APOInit: base,
        AudioProcessingMode: AUDIO_SIGNALPROCESSINGMODE_DEFAULT,
        ..Default::default()
    };
    let init3 = APOInitSystemEffects3 {
        APOInit: base,
        AudioProcessingMode: AUDIO_SIGNALPROCESSINGMODE_DEFAULT,
        ..Default::default()
    };
    let copy_bytes = |pointer: *const u8, length| unsafe {
        core::slice::from_raw_parts(pointer, length).to_vec()
    };
    for mut bytes in [
        copy_bytes((&raw const init1).cast(), size_of_val(&init1)),
        copy_bytes((&raw const init2).cast(), size_of_val(&init2)),
        copy_bytes((&raw const init3).cast(), size_of_val(&init3)),
    ] {
        let size = bytes.len() as u32;
        bytes[..4].copy_from_slice(&size.to_le_bytes());
        // An unaligned BYTE buffer is permitted by the COM signature.
        bytes.insert(0, 0xff);
        let apo = make_uninitialized();
        let config: IAudioProcessingObjectConfiguration = apo.cast().unwrap();
        let media = float(2);
        let descriptor = descriptor(&media, 64);
        assert_eq!(
            unsafe { config.LockForProcess(&[&descriptor], &[&descriptor]) }
                .unwrap_err()
                .code(),
            APOERR_NOT_INITIALIZED
        );
        unsafe { apo.Initialize(&bytes[1..]).unwrap() };
        assert_eq!(
            unsafe { apo.Initialize(&bytes[1..]) }.unwrap_err().code(),
            APOERR_ALREADY_INITIALIZED
        );
        unsafe {
            config
                .LockForProcess(&[&descriptor], &[&descriptor])
                .unwrap()
        };
        assert_eq!(
            unsafe { config.LockForProcess(&[&descriptor], &[&descriptor]) }
                .unwrap_err()
                .code(),
            APOERR_APO_LOCKED
        );
        unsafe { config.UnlockForProcess().unwrap() };
        assert_eq!(
            unsafe { config.UnlockForProcess() }.unwrap_err().code(),
            APOERR_ALREADY_UNLOCKED
        );
    }
    let discovery = make_uninitialized();
    let init = APOInitSystemEffects3 {
        APOInit: APOInitBaseStruct {
            cbSize: size_of::<APOInitSystemEffects3>() as u32,
            clsid: CLSID_VOICE_BOX_APO,
        },
        AudioProcessingMode: AUDIO_SIGNALPROCESSINGMODE_DEFAULT,
        InitializeForDiscoveryOnly: true.into(),
        ..Default::default()
    };
    unsafe {
        discovery
            .Initialize(core::slice::from_raw_parts(
                (&raw const init).cast(),
                size_of_val(&init),
            ))
            .unwrap()
    };
    let media = float(2);
    let descriptor = descriptor(&media, 64);
    let config: IAudioProcessingObjectConfiguration = discovery.cast().unwrap();
    assert_eq!(
        unsafe { config.LockForProcess(&[&descriptor], &[&descriptor]) }
            .unwrap_err()
            .code(),
        APOERR_NOT_INITIALIZED
    );
}

#[test]
fn interfaces_share_com_identity_and_aggregation_delegates_to_outer() {
    let apo = instance(AUDIO_SIGNALPROCESSINGMODE_DEFAULT);
    let identity: IUnknown = apo.cast().unwrap();
    let config: IAudioProcessingObjectConfiguration = apo.cast().unwrap();
    let rt: IAudioProcessingObjectRT = apo.cast().unwrap();
    let fx: IAudioSystemEffects3 = apo.cast().unwrap();
    for other in [
        config.cast::<IUnknown>().unwrap(),
        rt.cast().unwrap(),
        fx.cast().unwrap(),
    ] {
        assert_eq!(other.as_raw(), identity.as_raw());
    }
    let unknown_iid = GUID::from_u128(0xa69e23cc_02f0_4dea_a39c_b7bd977b3f8b);
    let mut output = identity.as_raw();
    assert_eq!(
        unsafe { (identity.vtable().QueryInterface)(identity.as_raw(), &unknown_iid, &mut output) },
        E_NOINTERFACE
    );
    assert!(output.is_null());

    #[repr(C)]
    struct Outer {
        vtable: *const IUnknown_Vtbl,
        refs: AtomicU32,
        inner: *mut c_void,
    }
    unsafe extern "system" fn add(this: *mut c_void) -> u32 {
        unsafe {
            (*(this.cast::<Outer>()))
                .refs
                .fetch_add(1, Ordering::Relaxed)
                + 1
        }
    }
    unsafe extern "system" fn release(this: *mut c_void) -> u32 {
        unsafe {
            (*(this.cast::<Outer>()))
                .refs
                .fetch_sub(1, Ordering::Relaxed)
                - 1
        }
    }
    unsafe extern "system" fn qi(
        this: *mut c_void,
        iid: *const GUID,
        output: *mut *mut c_void,
    ) -> HRESULT {
        if unsafe { *iid } == IUnknown::IID {
            unsafe {
                *output = this;
                add(this);
            }
            S_OK
        } else {
            let inner = unsafe { (*(this.cast::<Outer>())).inner };
            unsafe { ((*unknown_vtable(inner)).QueryInterface)(inner, iid, output) }
        }
    }
    let vtable = IUnknown_Vtbl {
        QueryInterface: qi,
        AddRef: add,
        Release: release,
    };
    let mut outer = Box::new(Outer {
        vtable: &vtable,
        refs: AtomicU32::new(1),
        inner: ptr::null_mut(),
    });
    let outer_ptr = ptr::from_mut(&mut *outer).cast::<c_void>();
    let mut inner = ptr::null_mut();
    assert_eq!(
        unsafe {
            factory_create_instance(
                ptr::null_mut(),
                outer_ptr,
                &IAudioProcessingObject::IID,
                &mut inner,
            )
        },
        CLASS_E_NOAGGREGATION
    );
    assert!(inner.is_null());
    assert_eq!(
        unsafe { factory_create_instance(ptr::null_mut(), outer_ptr, &IUnknown::IID, &mut inner) },
        S_OK
    );
    outer.inner = inner;
    let inner = unsafe { IUnknown::from_raw(inner) };
    let aggregated: IAudioProcessingObject = inner.cast().unwrap();
    assert_eq!(outer.refs.load(Ordering::Relaxed), 2);
    let controlling: IUnknown = aggregated.cast().unwrap();
    assert_eq!(controlling.as_raw(), outer_ptr);
    assert_ne!(controlling.as_raw(), inner.as_raw());
    drop(controlling);
    drop(aggregated);
    assert_eq!(outer.refs.load(Ordering::Relaxed), 1);
    drop(inner);
}

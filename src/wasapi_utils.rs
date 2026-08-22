use core::slice;
use std::{collections::VecDeque, fmt, marker::PhantomData, ptr, rc::Rc};

use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        Media::{
            Audio::{
                AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
                AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, DEVICE_STATE_ACTIVE, EDataFlow,
                IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceCollection,
                IMMDeviceEnumerator, MMDeviceEnumerator, WAVE_FORMAT_PCM, WAVEFORMATEX,
                WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0, eCapture, eConsole, eRender,
            },
            KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE},
            Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT},
        },
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
                CoUninitialize, STGM_READ,
            },
            Threading::{CreateEventA, WaitForSingleObject},
        },
    },
    core::PCSTR,
};

pub struct ComMtaGuard {
    _not_send_sync_marker: PhantomData<Rc<()>>,
}

impl Drop for ComMtaGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}

pub fn init_com_mta() -> anyhow::Result<ComMtaGuard> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
    Ok(ComMtaGuard {
        _not_send_sync_marker: PhantomData,
    })
}

pub struct EventHandle {
    handle: HANDLE,
}

impl Drop for EventHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

impl EventHandle {
    /// Wait for an event on a handle, with a timeout given in ms
    pub fn wait_for_event(&self, timeout_ms: u32) -> anyhow::Result<()> {
        let retval = unsafe { WaitForSingleObject(self.handle, timeout_ms) };
        if retval.0 != WAIT_OBJECT_0.0 {
            anyhow::bail!("Event wait timed out");
        }
        Ok(())
    }
}

pub struct AudioDevice {
    device: IMMDevice,
    direction: Direction,
}

impl AudioDevice {
    pub fn get_id(&self) -> anyhow::Result<String> {
        let id = unsafe { self.device.GetId()?.to_string()? };
        Ok(id)
    }

    pub fn get_friendly_name(&self) -> anyhow::Result<String> {
        let property_store = unsafe { self.device.OpenPropertyStore(STGM_READ)? };
        let name = unsafe { property_store.GetValue(&PKEY_Device_FriendlyName)? };
        Ok(name.to_string())
    }

    pub fn get_audio_client(&self) -> anyhow::Result<AudioClient> {
        let client = unsafe { self.device.Activate::<IAudioClient>(CLSCTX_ALL, None)? };
        Ok(AudioClient {
            client,
            direction: self.direction,
            bytes_per_frame: None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Render,
    Capture,
}

impl Direction {
    pub fn from_e_data_flow(e_data_flow: EDataFlow) -> anyhow::Result<Self> {
        match e_data_flow {
            eRender => Ok(Direction::Render),
            eCapture => Ok(Direction::Capture),
            _ => anyhow::bail!("Invalid EDataFlow value: {:?}", e_data_flow),
        }
    }
}

/// Struct wrapping an [IAudioCaptureClient](https://docs.microsoft.com/en-us/windows/win32/api/audioclient/nn-audioclient-iaudiocaptureclient).
pub struct AudioCaptureClient {
    client: IAudioCaptureClient,
    bytes_per_frame: usize,
}

impl AudioCaptureClient {
    /// Read raw bytes data from a device into a deque.
    /// Returns the [BufferInfo] describing the buffer that the data was read from.
    pub fn read_from_device_to_deque(&self, data: &mut VecDeque<u8>) -> anyhow::Result<()> {
        let mut buffer_ptr = ptr::null_mut();
        let mut nbr_frames_returned = 0;
        let mut index: u64 = 0;
        let mut timestamp: u64 = 0;
        let mut flags = 0;
        unsafe {
            self.client.GetBuffer(
                &mut buffer_ptr,
                &mut nbr_frames_returned,
                &mut flags,
                Some(&mut index),
                Some(&mut timestamp),
            )?
        };
        if nbr_frames_returned == 0 {
            // There is no need to release a buffer of 0 bytes
            return Ok(());
        }
        let len_in_bytes = nbr_frames_returned as usize * self.bytes_per_frame;
        let bufferslice = unsafe { slice::from_raw_parts(buffer_ptr, len_in_bytes) };
        for element in bufferslice.iter() {
            data.push_back(*element);
        }
        if nbr_frames_returned > 0 {
            unsafe { self.client.ReleaseBuffer(nbr_frames_returned).unwrap() };
        }
        Ok(())
    }
}

pub struct AudioClient {
    client: IAudioClient,
    direction: Direction,
    bytes_per_frame: Option<usize>,
}

const WAVEFORMATEXTENSIBLE_CB_SIZE: usize =
    size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>();

impl AudioClient {
    /// Get default and minimum periods in 100-nanosecond units
    fn get_device_period(&self) -> anyhow::Result<(i64, i64)> {
        let mut def_time = 0;
        let mut min_time = 0;
        unsafe {
            self.client
                .GetDevicePeriod(Some(&mut def_time), Some(&mut min_time))?
        };
        Ok((def_time, min_time))
    }

    /// Get buffer size in frames,
    /// see [IAudioClient::GetBufferSize](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-getbuffersize).
    pub fn get_buffer_frame_count(&self) -> anyhow::Result<usize> {
        Ok(unsafe { self.client.GetBufferSize()? as usize })
    }

    pub fn get_mix_format(&self) -> anyhow::Result<WaveFormatExtensible> {
        let temp_fmt_ptr = unsafe { self.client.GetMixFormat()? };
        let temp_fmt = unsafe { *temp_fmt_ptr };

        // GetMixFormat有可能返回WAVEFORMATEX或WAVEFORMATEXTENSIBLE
        if temp_fmt.wFormatTag as u32 == WAVE_FORMAT_EXTENSIBLE
            && temp_fmt.cbSize as usize == WAVEFORMATEXTENSIBLE_CB_SIZE
        {
            // 返回了WAVEFORMATEXTENSIBLE，直接读取
            let format = WaveFormatExtensible {
                wave_format_extensible: unsafe {
                    temp_fmt_ptr.cast::<WAVEFORMATEXTENSIBLE>().read()
                },
            };

            unsafe { CoTaskMemFree(Some(temp_fmt_ptr.cast())) };

            Ok(format)
        } else {
            unsafe { CoTaskMemFree(Some(temp_fmt_ptr.cast())) };

            // 返回了WAVEFORMATEX，转换为WAVEFORMATEXTENSIBLE
            WaveFormatExtensible::from_wave_format_ex(temp_fmt)
        }
    }

    pub fn init_client_events_shared(
        &mut self,
        wave_format: &WaveFormatExtensible,
        direction: &Direction,
        auto_convert: bool,
    ) -> anyhow::Result<()> {
        let mut streamflags = match (&self.direction, direction) {
            (Direction::Render, Direction::Capture) => AUDCLNT_STREAMFLAGS_LOOPBACK,
            (Direction::Capture, Direction::Render) => {
                anyhow::bail!("Cannot initialize render to capture device");
            }
            _ => 0,
        };
        if auto_convert {
            streamflags |=
                AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        }

        streamflags |= AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
        unsafe {
            self.client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                streamflags,
                self.get_device_period()?.0,
                0,
                wave_format.as_wave_format_ex_ref(),
                None,
            )?;
        }
        self.direction = *direction;
        self.bytes_per_frame = Some(wave_format.get_frame_bytes_count());
        Ok(())
    }

    /// Create and return an event handle for an [AudioClient].
    /// This is required when using an [AudioClient] initialized for event driven mode, [TimingMode::Events].
    pub fn set_get_event_handle(&self) -> anyhow::Result<EventHandle> {
        let handle = unsafe { CreateEventA(None, false, false, PCSTR::null())? };
        unsafe { self.client.SetEventHandle(handle)? };
        Ok(EventHandle { handle })
    }

    /// Get a capture client
    pub fn get_capture_client(&self) -> anyhow::Result<AudioCaptureClient> {
        let client = unsafe { self.client.GetService::<IAudioCaptureClient>()? };
        Ok(AudioCaptureClient {
            client,
            bytes_per_frame: self.bytes_per_frame.unwrap_or_default(),
        })
    }

    /// Start the stream on an [IAudioClient]
    ///
    /// For playback, fill the buffer with data before starting the stream,
    /// see [Rendering a Stream](https://learn.microsoft.com/en-us/windows/win32/coreaudio/rendering-a-stream).
    /// Use [AudioClient::get_available_space_in_frames()] to get the number of frames to write.
    /// When using [TimingMode::Events], the event should then be waited for
    /// at the start of the playback loop, before writing more data.
    pub fn start_stream(&self) -> anyhow::Result<()> {
        unsafe { self.client.Start()? };
        Ok(())
    }
}

pub enum SampleType {
    Float,
    Int,
}

pub struct WaveFormatExtensible {
    wave_format_extensible: WAVEFORMATEXTENSIBLE,
}

impl fmt::Debug for WaveFormatExtensible {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let format_ext = &self.wave_format_extensible;
        let format = &format_ext.Format;
        f.debug_struct("WaveFormat")
            .field("nAvgBytesPerSec", &{ format.nAvgBytesPerSec })
            .field("cbSize", &{ format.cbSize })
            .field("nBlockAlign", &{ format.nBlockAlign })
            .field("wBitsPerSample", &{ format.wBitsPerSample })
            .field("nSamplesPerSec", &{ format.nSamplesPerSec })
            .field("wFormatTag", &{ format.wFormatTag })
            .field("wValidBitsPerSample", &unsafe {
                format_ext.Samples.wValidBitsPerSample
            })
            .field("SubFormat", &{ format_ext.SubFormat })
            .field("nChannel", &{ format_ext.Format.nChannels })
            .field("dwChannelMask", &{ format_ext.dwChannelMask })
            .finish()
    }
}

impl WaveFormatExtensible {
    /// Build a [WAVEFORMATEXTENSIBLE](https://docs.microsoft.com/en-us/windows/win32/api/mmreg/ns-mmreg-waveformatextensible) struct for the given parameters.
    /// `channel_mask` is optional. If a mask is provided, it will be used. If not, a default mask will be created.
    /// This can be used to work around quirks for some device drivers.
    /// If the default is not accepted, try again using a zero mask, `Some(0)`.
    fn new(
        storebits: usize,
        validbits: usize,
        sample_type: SampleType,
        samplerate: usize,
        channels: usize,
        channel_mask: Option<u32>,
    ) -> Self {
        let blockalign = channels * storebits / 8;
        let byterate = samplerate * blockalign;

        let wave_format_ex = WAVEFORMATEX {
            cbSize: 22,
            nAvgBytesPerSec: byterate as u32,
            nBlockAlign: blockalign as u16,
            nChannels: channels as u16,
            nSamplesPerSec: samplerate as u32,
            wBitsPerSample: storebits as u16,
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
        };
        let sample = WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: validbits as u16,
        };
        let subformat = match sample_type {
            SampleType::Float => KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
            SampleType::Int => KSDATAFORMAT_SUBTYPE_PCM,
        };
        // Only max 18 mask channel positions are defined,
        // https://docs.microsoft.com/en-us/windows/win32/api/mmreg/ns-mmreg-waveformatextensible
        let mask = if let Some(given_mask) = channel_mask {
            given_mask
        } else {
            match channels {
                ch if ch <= 18 => {
                    // setting bit for each channel
                    (1 << ch) - 1
                }
                _ => 0,
            }
        };
        let wave_format_extensible = WAVEFORMATEXTENSIBLE {
            Format: wave_format_ex,
            Samples: sample,
            SubFormat: subformat,
            dwChannelMask: mask,
        };
        Self {
            wave_format_extensible,
        }
    }

    fn from_wave_format_ex(wave_format_ex: WAVEFORMATEX) -> anyhow::Result<Self> {
        Ok(Self {
            wave_format_extensible: WAVEFORMATEXTENSIBLE {
                Format: WAVEFORMATEX {
                    wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
                    cbSize: WAVEFORMATEXTENSIBLE_CB_SIZE as u16,
                    // blockalign是一个frame的字节数, nSamplesPerSec是每秒的frame数（或者说每秒每个通道的sample数）
                    nAvgBytesPerSec: wave_format_ex.nSamplesPerSec
                        * wave_format_ex.nBlockAlign as u32,
                    // 8 * blockalign算出一个frame的bits数，一个frame包含nChannels个sample，除以nChannels得到每个sample/channel的bits数
                    // frame1(channel1, channel2) frame2(channel1, channel2) frame3(channel1, channel2) ... frameN(channel1, channel2)
                    // 原来的wBitsPerSample是有效位数，但是WAVEFORMATEXTENSIBLE中代表存储位数
                    wBitsPerSample: 8 * wave_format_ex.nBlockAlign / wave_format_ex.nChannels,
                    ..wave_format_ex
                },
                Samples: WAVEFORMATEXTENSIBLE_0 {
                    wValidBitsPerSample: wave_format_ex.wBitsPerSample,
                },
                dwChannelMask: if wave_format_ex.nChannels <= 18 {
                    (1 << wave_format_ex.nChannels) - 1
                } else {
                    0
                },
                SubFormat: match wave_format_ex.wFormatTag as u32 {
                    WAVE_FORMAT_PCM => KSDATAFORMAT_SUBTYPE_PCM,
                    WAVE_FORMAT_IEEE_FLOAT => KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
                    _ => anyhow::bail!(
                        "Unsupported format tag: {}",
                        wave_format_ex.wFormatTag as u32
                    ),
                },
            },
        })
    }

    fn as_wave_format_ex_ref(&self) -> &WAVEFORMATEX {
        &self.wave_format_extensible.Format
    }

    pub fn get_frame_bytes_count(&self) -> usize {
        self.wave_format_extensible.Format.nBlockAlign as usize
    }
}

fn get_active_audio_devices(e_data_flow: EDataFlow) -> anyhow::Result<Vec<AudioDevice>> {
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    let device_collection: IMMDeviceCollection =
        unsafe { enumerator.EnumAudioEndpoints(e_data_flow, DEVICE_STATE_ACTIVE)? };
    let count = unsafe { device_collection.GetCount()? };
    let direction = Direction::from_e_data_flow(e_data_flow)?;
    let mut devices = Vec::with_capacity(count as usize);
    for i in 0..count {
        let device = unsafe { device_collection.Item(i)? };
        devices.push(AudioDevice { device, direction });
    }
    Ok(devices)
}

pub fn get_active_microphones() -> anyhow::Result<Vec<AudioDevice>> {
    get_active_audio_devices(eCapture)
}

pub fn get_active_speakers() -> anyhow::Result<Vec<AudioDevice>> {
    get_active_audio_devices(eRender)
}

fn get_default_audio_device(e_data_flow: EDataFlow) -> anyhow::Result<AudioDevice> {
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(e_data_flow, eConsole)? };
    let direction = Direction::from_e_data_flow(e_data_flow)?;
    Ok(AudioDevice { device, direction })
}

pub fn get_default_microphone() -> anyhow::Result<AudioDevice> {
    get_default_audio_device(eCapture)
}

pub fn get_default_speaker() -> anyhow::Result<AudioDevice> {
    get_default_audio_device(eRender)
}

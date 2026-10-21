use super::{Apartment, Stream, enumerator};
use wasapi::{SampleType, WaveFormat};
use windows::Win32::{
    Media::Audio::{IAudioClient, IAudioRenderClient, eConsole, eRender},
    System::Com::CLSCTX_ALL,
};

/// The soundboard reuses the same WASAPI/COM lifetime and format negotiation as
/// preview playback, with a shorter queue so route and volume changes feel quick.
pub(in crate::app) struct Monitor {
    stream: Stream,
    render: IAudioRenderClient,
    started: bool,
    cursor: u64,
    _apartment: Apartment,
}
impl Monitor {
    pub fn open() -> anyhow::Result<Self> {
        let apartment = Apartment::new()?;
        let device = unsafe { enumerator()?.GetDefaultAudioEndpoint(eRender, eConsole)? };
        let client = unsafe { device.Activate::<IAudioClient>(CLSCTX_ALL, None)? };
        let format = WaveFormat::new(32, 32, &SampleType::Float, 48_000, 2, Some(3));
        let stream = Stream::with_buffer(client, &format, true, 200_000)?;
        let render = unsafe { stream.client.GetService()? };
        Ok(Self {
            stream,
            render,
            started: false,
            cursor: 0,
            _apartment: apartment,
        })
    }
    pub fn fill(&mut self, now: u64, sample: impl Fn(u64) -> [f32; 2]) -> anyhow::Result<()> {
        let padding = unsafe { self.stream.client.GetCurrentPadding()? };
        let capacity = unsafe { self.stream.client.GetBufferSize()? };
        let expected = now + u64::from(padding);
        if self.cursor == 0 || self.cursor.abs_diff(expected) > 960 {
            self.cursor = expected;
        }
        let frames = capacity.saturating_sub(padding);
        if frames > 0 {
            let data = unsafe { self.render.GetBuffer(frames)? }.cast::<f32>();
            for i in 0..frames as usize {
                let stereo = sample(self.cursor + i as u64);
                unsafe {
                    data.add(i * 2).write(stereo[0]);
                    data.add(i * 2 + 1).write(stereo[1]);
                }
            }
            unsafe {
                self.render.ReleaseBuffer(frames, 0)?;
            }
            self.cursor += u64::from(frames);
        }
        if !self.started {
            unsafe {
                self.stream.client.Start()?;
            }
            self.started = true;
        }
        Ok(())
    }
}

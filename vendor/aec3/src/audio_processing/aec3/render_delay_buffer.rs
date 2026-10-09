use crate::api::config::EchoCanceller3Config;
use crate::audio_processing::aec3::aec3_common::{
    Aec3Optimization, BLOCK_SIZE, detect_optimization, get_down_sampled_buffer_size,
    get_render_delay_buffer_size, num_bands_for_rate,
};
use crate::audio_processing::aec3::aec3_fft::Aec3Fft;
use crate::audio_processing::aec3::alignment_mixer::AlignmentMixer;
use crate::audio_processing::aec3::block::Block;
use crate::audio_processing::aec3::block_buffer::BlockBuffer;
use crate::audio_processing::aec3::decimator::Decimator;
use crate::audio_processing::aec3::downsampled_render_buffer::DownsampledRenderBuffer;
use crate::audio_processing::aec3::fft_buffer::FftBuffer;
use crate::audio_processing::aec3::render_buffer::RenderBuffer;
use crate::audio_processing::aec3::spectrum_buffer::SpectrumBuffer;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BufferingEvent {
    None,
    RenderUnderrun,
    RenderOverrun,
}

pub struct RenderDelayBuffer {
    optimization: Aec3Optimization,
    config: EchoCanceller3Config,
    render_linear_amplitude_gain: f32,
    sub_block_size: usize,
    blocks: BlockBuffer,
    spectra: SpectrumBuffer,
    ffts: FftBuffer,
    delay: Option<usize>,
    low_rate: DownsampledRenderBuffer,
    render_mixer: AlignmentMixer,
    render_decimator: Decimator,
    fft: Aec3Fft,
    render_ds: Vec<f32>,
    buffer_headroom: usize,
    min_latency_blocks: usize,
    excess_render_detection_counter: usize,
}

impl RenderDelayBuffer {
    pub fn new(
        mut config: EchoCanceller3Config,
        sample_rate_hz: i32,
        num_render_channels: usize,
    ) -> Self {
        let requested_down_sampling_factor = config.delay.down_sampling_factor.max(1);
        config.validate();
        config.delay.down_sampling_factor = requested_down_sampling_factor;
        let down_sampling_factor = requested_down_sampling_factor;
        let sub_block_size = BLOCK_SIZE / down_sampling_factor;
        let buffer_size = get_render_delay_buffer_size(
            down_sampling_factor,
            config.delay.num_filters,
            config.filter.main.length_blocks,
        );
        let num_bands = num_bands_for_rate(sample_rate_hz);
        assert!(num_bands > 0, "Unsupported sample rate {sample_rate_hz}");

        let blocks = BlockBuffer::new(buffer_size, num_bands, num_render_channels);
        let spectra = SpectrumBuffer::new(buffer_size, num_render_channels);
        let ffts = FftBuffer::new(buffer_size, num_render_channels);
        let low_rate = DownsampledRenderBuffer::new(get_down_sampled_buffer_size(
            down_sampling_factor,
            config.delay.num_filters,
        ));

        let mut instance = Self {
            optimization: detect_optimization(),
            render_linear_amplitude_gain: 10f32
                .powf(config.render_levels.render_power_gain_db / 20.0),
            sub_block_size,
            blocks,
            spectra,
            ffts,
            delay: Some(config.delay.default_delay),
            low_rate,
            render_mixer: AlignmentMixer::new(
                num_render_channels,
                config.delay.render_alignment_mixing.clone(),
            ),
            render_decimator: Decimator::new(down_sampling_factor),
            fft: Aec3Fft::new(),
            render_ds: vec![0.0; sub_block_size],
            buffer_headroom: config.filter.main.length_blocks,
            min_latency_blocks: 0,
            excess_render_detection_counter: 0,
            config,
        };

        instance.reset();
        instance
    }

    pub fn reset(&mut self) {
        self.min_latency_blocks = 0;
        self.excess_render_detection_counter = 0;

        self.low_rate.read = self
            .low_rate
            .offset_index(self.low_rate.write, self.sub_block_size as isize);

        self.apply_total_delay(self.config.delay.default_delay as isize);
        self.delay = None;
    }

    pub fn insert(&mut self, block: &Block) -> BufferingEvent {
        let previous_write = self.blocks.write;
        self.increment_write_indices();

        let mut event = BufferingEvent::None;
        if self.render_overrun() {
            event = BufferingEvent::RenderOverrun;
        }

        self.insert_block(block, previous_write);

        if matches!(event, BufferingEvent::RenderOverrun) {
            self.reset();
        }

        event
    }

    pub fn prepare_capture_processing(&mut self) -> BufferingEvent {
        let mut event = BufferingEvent::None;
        if self.detect_excess_render_blocks() {
            self.reset();
            event = BufferingEvent::RenderOverrun;
        } else if self.render_underrun() {
            self.increment_read_indices();
            if let Some(delay) = self.delay {
                if delay > 0 {
                    self.delay = Some(delay - 1);
                }
            }
            event = BufferingEvent::RenderUnderrun;
        } else {
            self.increment_low_rate_read_indices();
            self.increment_read_indices();
        }

        event
    }

    pub fn align_from_delay(&mut self, delay: usize) -> bool {
        if self.delay == Some(delay) {
            return false;
        }
        self.delay = Some(delay);
        let mut total_delay = self.map_delay_to_total_delay(delay);
        total_delay = total_delay.clamp(0, self.max_delay() as i32);
        self.apply_total_delay(total_delay as isize);
        true
    }

    pub fn max_delay(&self) -> usize {
        self.blocks.size() - 1 - self.buffer_headroom
    }

    pub fn render_buffer(&self) -> RenderBuffer<'_> {
        RenderBuffer::new(&self.blocks, &self.spectra, &self.ffts)
    }

    pub fn downsampled_render_buffer(&self) -> &DownsampledRenderBuffer {
        &self.low_rate
    }

    fn map_delay_to_total_delay(&self, external_delay_blocks: usize) -> i32 {
        self.buffer_latency() + external_delay_blocks as i32
    }

    fn apply_total_delay(&mut self, delay: isize) {
        self.blocks.read = self.blocks.offset_index(self.blocks.write, -delay);
        self.spectra.read = self.spectra.offset_index(self.spectra.write, delay);
        self.ffts.read = self.ffts.offset_index(self.ffts.write, delay);
    }

    fn insert_block(&mut self, block: &Block, previous_write: usize) {
        let write = self.blocks.write;
        let num_bands = self.blocks.buffer[write].num_bands();
        let num_channels = self.blocks.buffer[write].num_channels();
        assert_eq!(block.num_bands(), num_bands);
        assert_eq!(block.num_channels(), num_channels);
        let gain = self.render_linear_amplitude_gain;
        let apply_gain = (gain - 1.0).abs() > f32::EPSILON;
        for band in 0..num_bands {
            for ch in 0..num_channels {
                let dst = self.blocks.buffer[write].view_mut(band, ch);
                dst.copy_from_slice(block.view(band, ch));
                if apply_gain {
                    for sample in dst.iter_mut() {
                        *sample *= gain;
                    }
                }
            }
        }

        let mut downmixed = [0.0f32; BLOCK_SIZE];
        self.render_mixer
            .produce_output(&self.blocks.buffer[write], &mut downmixed);
        self.render_decimator
            .decimate(&downmixed, &mut self.render_ds);
        assert!(self.low_rate.write + self.render_ds.len() <= self.low_rate.buffer.len());
        for (i, &sample) in self.render_ds.iter().rev().enumerate() {
            self.low_rate.buffer[self.low_rate.write + i] = sample;
        }

        let num_channels = self.blocks.buffer[self.blocks.write].num_channels();
        for ch in 0..num_channels {
            let current = self.blocks.buffer[self.blocks.write].view(0, ch);
            let previous = self.blocks.buffer[previous_write].view(0, ch);
            self.fft.padded_fft(
                current,
                previous,
                &mut self.ffts.buffer[self.ffts.write][ch],
            );
            self.ffts.buffer[self.ffts.write][ch].spectrum(
                self.optimization,
                &mut self.spectra.buffer[self.spectra.write][ch],
            );
        }
    }

    fn detect_excess_render_blocks(&mut self) -> bool {
        let latency_blocks = self.buffer_latency() as usize;
        self.min_latency_blocks = self.min_latency_blocks.min(latency_blocks);
        self.excess_render_detection_counter += 1;
        let mut excess_detected = false;
        if self.excess_render_detection_counter
            >= self
                .config
                .buffering
                .excess_render_detection_interval_blocks
        {
            excess_detected =
                self.min_latency_blocks > self.config.buffering.max_allowed_excess_render_blocks;
            self.min_latency_blocks = latency_blocks;
            self.excess_render_detection_counter = 0;
        }
        excess_detected
    }

    fn buffer_latency(&self) -> i32 {
        let size = self.low_rate.buffer.len() as isize;
        let mut latency_samples =
            (size + self.low_rate.read as isize - self.low_rate.write as isize) % size;
        if latency_samples < 0 {
            latency_samples += size;
        }
        (latency_samples as usize / self.sub_block_size) as i32
    }

    fn increment_write_indices(&mut self) {
        self.low_rate
            .update_write_index(-(self.sub_block_size as isize));
        self.blocks.inc_write_index();
        self.spectra.dec_write_index();
        self.ffts.dec_write_index();
    }

    fn increment_low_rate_read_indices(&mut self) {
        self.low_rate
            .update_read_index(-(self.sub_block_size as isize));
    }

    fn increment_read_indices(&mut self) {
        if self.blocks.read != self.blocks.write {
            self.blocks.inc_read_index();
            self.spectra.dec_read_index();
            self.ffts.dec_read_index();
        }
    }

    fn render_overrun(&self) -> bool {
        self.low_rate.read == self.low_rate.write || self.blocks.read == self.blocks.write
    }

    fn render_underrun(&self) -> bool {
        self.low_rate.read == self.low_rate.write
    }
}

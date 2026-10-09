//! Contiguous multiband, multichannel block of audio.
//!
//! Ported from `reference_aec_cpp/modules/audio_processing/aec3/block.h`.

use crate::audio_processing::aec3::aec3_common::BLOCK_SIZE;

/// One or more channels of 4 milliseconds of audio, split into one or more
/// frequency bands each sampled at 16 kHz.
///
/// The samples live in a single allocation laid out as `[band][channel][sample]`,
/// so a block is one allocation rather than one per channel per band.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    num_bands: usize,
    num_channels: usize,
    data: Vec<f32>,
}

impl Block {
    pub fn new(num_bands: usize, num_channels: usize) -> Self {
        Self::with_value(num_bands, num_channels, 0.0)
    }

    pub fn with_value(num_bands: usize, num_channels: usize, value: f32) -> Self {
        Self {
            num_bands,
            num_channels,
            data: vec![value; num_bands * num_channels * BLOCK_SIZE],
        }
    }

    pub fn num_bands(&self) -> usize {
        self.num_bands
    }

    pub fn num_channels(&self) -> usize {
        self.num_channels
    }

    pub fn view(&self, band: usize, channel: usize) -> &[f32; BLOCK_SIZE] {
        let start = self.index(band, channel);
        self.data[start..start + BLOCK_SIZE]
            .try_into()
            .expect("block slice has BLOCK_SIZE samples")
    }

    pub fn view_mut(&mut self, band: usize, channel: usize) -> &mut [f32; BLOCK_SIZE] {
        let start = self.index(band, channel);
        (&mut self.data[start..start + BLOCK_SIZE])
            .try_into()
            .expect("block slice has BLOCK_SIZE samples")
    }

    /// Iterates the channels of one band.
    pub fn band_channels(&self, band: usize) -> impl Iterator<Item = &[f32; BLOCK_SIZE]> {
        (0..self.num_channels).map(move |channel| self.view(band, channel))
    }

    fn index(&self, band: usize, channel: usize) -> usize {
        debug_assert!(band < self.num_bands);
        debug_assert!(channel < self.num_channels);
        (band * self.num_channels + channel) * BLOCK_SIZE
    }
}

//! Allocation-free streaming UL-UNAS DNS3, using the official trained model.

mod network;
mod spectral;
mod weights;

use network::Network;
use spectral::{Complex, Stft};

pub(super) const SAMPLE_RATE: u32 = 16_000;
pub(super) const HOP_SIZE: usize = 256;
pub(super) const LATENCY_SAMPLES: usize = HOP_SIZE;

pub(super) struct UlUnas {
    network: Network,
    stft: Stft,
    spectrum: [Complex; 257],
}

impl UlUnas {
    pub(super) fn new() -> Self {
        Self {
            network: Network::new(),
            stft: Stft::new(),
            spectrum: [Complex::new(0.0, 0.0); 257],
        }
    }
    pub(super) fn reset(&mut self) {
        self.network.reset();
        self.stft.reset();
        self.spectrum.fill(Complex::new(0.0, 0.0));
    }
    pub(super) fn process(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), HOP_SIZE);
        debug_assert_eq!(output.len(), HOP_SIZE);
        self.stft.analyze(input, &mut self.spectrum);
        self.network.enhance(&mut self.spectrum);
        self.stft.synthesize(&mut self.spectrum, output);
    }
}

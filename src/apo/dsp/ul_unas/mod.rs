//! Allocation-free streaming UL-UNAS DNS3, using the official trained model.

mod network;
mod spectral;
mod weights;

use super::StreamingModel;
use network::Network;
use spectral::{Complex, Stft};

pub(super) struct UlUnas {
    enabled: bool,
    network: Network,
    stft: Stft,
    spectrum: [Complex; 257],
}

impl UlUnas {
    pub(super) fn new() -> Self {
        Self {
            enabled: true,
            network: Network::new(),
            stft: Stft::new(),
            spectrum: [Complex::new(0.0, 0.0); 257],
        }
    }
}

impl StreamingModel for UlUnas {
    fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.network.reset();
            self.enabled = enabled;
        }
    }
    fn sample_rate(&self) -> u32 {
        16_000
    }
    fn hop_size(&self) -> usize {
        256
    }
    fn latency_samples(&self) -> usize {
        256
    }
    fn reset(&mut self) {
        self.network.reset();
        self.stft.reset();
        self.spectrum.fill(Complex::new(0.0, 0.0));
    }
    fn process(&mut self, input: &[f32], output: &mut [f32]) {
        self.stft.analyze(input, &mut self.spectrum);
        if self.enabled {
            self.network.enhance(&mut self.spectrum);
        }
        self.stft.synthesize(&mut self.spectrum, output);
    }
}

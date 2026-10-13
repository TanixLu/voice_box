//! Single-threaded, fixed-shape inference for the official DeepFilterNet3 model.
//!
//! Signal processing and default LSNR stage thresholds follow libDF. Every
//! frame buffer and neural state is retained between calls; the audio callback
//! never allocates, loads a model or waits for another thread.
//! Sustained silence skips only the neural network: unlike upstream's early
//! return, the STFT and feature states keep following the audio timeline.

mod network;
mod spectral;
mod weights;

use super::StreamingModel;
use network::Network;
use spectral::{Complex, DF_BINS, ERB_BANDS, FREQ_BINS, Features, HOP_SIZE, SAMPLE_RATE, Stft};

const DF_ORDER: usize = 5;
const LOOKAHEAD: usize = 2;
const MIN_LSNR: f32 = -10.0;
const MAX_ERB_LSNR: f32 = 30.0;
const MAX_DF_LSNR: f32 = 20.0;

pub(super) struct DeepFilterNet3 {
    stft: Stft,
    features: Features,
    network: Network,
    // Oldest to newest, using a rotating cursor instead of copying histories.
    history: [[Complex; FREQ_BINS]; DF_ORDER],
    next: usize,
    spectrum: [Complex; FREQ_BINS],
    erb: [f32; ERB_BANDS],
    spec: [f32; 2 * DF_BINS],
    gains: [f32; ERB_BANDS],
    // Frequency-major, then filter tap, then real / imaginary component.
    coefficients: [f32; DF_BINS * DF_ORDER * 2],
    silent_frames: usize,
    enabled: bool,
}

impl DeepFilterNet3 {
    pub(super) fn new() -> Self {
        Self {
            stft: Stft::new(),
            features: Features::new(),
            network: Network::new(),
            history: [[Complex::new(0.0, 0.0); FREQ_BINS]; DF_ORDER],
            next: 0,
            spectrum: [Complex::new(0.0, 0.0); FREQ_BINS],
            erb: [0.0; ERB_BANDS],
            spec: [0.0; 2 * DF_BINS],
            gains: [0.0; ERB_BANDS],
            coefficients: [0.0; DF_BINS * DF_ORDER * 2],
            silent_frames: 0,
            enabled: true,
        }
    }
}

impl StreamingModel for DeepFilterNet3 {
    fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.network.reset();
            self.features.reset();
            self.silent_frames = 0;
            self.enabled = enabled;
        }
    }

    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn hop_size(&self) -> usize {
        HOP_SIZE
    }

    fn latency_samples(&self) -> usize {
        // STFT overlap plus two model frames of lookahead, also on bypass.
        (1 + LOOKAHEAD) * HOP_SIZE
    }

    fn reset(&mut self) {
        self.stft.reset();
        self.features.reset();
        self.network.reset();
        for frame in &mut self.history {
            frame.fill(Complex::new(0.0, 0.0));
        }
        self.next = 0;
        self.spectrum.fill(Complex::new(0.0, 0.0));
        self.erb.fill(0.0);
        self.spec.fill(0.0);
        self.gains.fill(0.0);
        self.coefficients.fill(0.0);
        self.silent_frames = 0;
    }

    fn process(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), HOP_SIZE);
        debug_assert_eq!(output.len(), HOP_SIZE);
        let energy = input.iter().fold(0.0, |sum, sample| sum + sample * sample);
        if energy / (HOP_SIZE as f32) < 1e-7 {
            self.silent_frames = (self.silent_frames + 1).min(6);
        } else {
            self.silent_frames = 0;
        }

        self.stft.analyze(input, &mut self.spectrum);
        self.history[self.next].copy_from_slice(&self.spectrum);
        self.next = (self.next + 1) % DF_ORDER;
        self.features
            .compute(&self.spectrum, &mut self.erb, &mut self.spec);

        if self.enabled && self.silent_frames > 5 {
            // Upstream also freezes analysis and synthesis when skipping quiet
            // input. An APO must instead consume every hop at its fixed delay.
            // Keep normalized features and noisy history current, and synthesize
            // zero so no old overlap or pre-silence tail can reappear later.
            // LSNR thresholds below affect only enhancement, never this counter.
            self.spectrum.fill(Complex::new(0.0, 0.0));
            self.stft.synthesize(&mut self.spectrum, output);
            output.fill(0.0);
            return;
        }

        let lsnr = if self.enabled {
            self.network.process(
                &self.erb,
                &self.spec,
                &mut self.gains,
                &mut self.coefficients,
            )
        } else {
            MAX_ERB_LSNR + 1.0
        };

        // Coefficients for the current feature frame apply to the spectrum two
        // hops earlier. The five DF taps span two past and two future frames.
        let delayed = (self.next + DF_ORDER - LOOKAHEAD - 1) % DF_ORDER;
        self.spectrum.copy_from_slice(&self.history[delayed]);
        if self.enabled {
            if lsnr <= MAX_ERB_LSNR {
                // Network supplies a zero mask below MIN_LSNR, otherwise the
                // ERB decoder output. The high-frequency bins retain this mask.
                self.features.apply_mask(&mut self.spectrum, &self.gains);
            }
            if (MIN_LSNR..=MAX_DF_LSNR).contains(&lsnr) {
                for bin in 0..DF_BINS {
                    let mut enhanced = Complex::new(0.0, 0.0);
                    for tap in 0..DF_ORDER {
                        let source = self.history[(self.next + tap) % DF_ORDER][bin];
                        let index = (bin * DF_ORDER + tap) * 2;
                        let coefficient =
                            Complex::new(self.coefficients[index], self.coefficients[index + 1]);
                        enhanced += source * coefficient;
                    }
                    self.spectrum[bin] = enhanced;
                }
            }
        }
        self.stft.synthesize(&mut self.spectrum, output);
    }
}

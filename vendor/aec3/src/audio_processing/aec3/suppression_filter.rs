use crate::audio_processing::aec3::aec3_common::{
    Aec3Optimization, FFT_LENGTH, FFT_LENGTH_BY_2, FFT_LENGTH_BY_2_PLUS_1, num_bands_for_rate,
    valid_full_band_rate,
};
use crate::audio_processing::aec3::aec3_fft::Aec3Fft;
use crate::audio_processing::aec3::block::Block;
use crate::audio_processing::aec3::fft_data::FftData;
use crate::audio_processing::aec3::vector_math::VectorMath;

pub struct SuppressionFilter {
    optimization: Aec3Optimization,
    sample_rate_hz: i32,
    num_capture_channels: usize,
    fft: Aec3Fft,
    e_output_old: Vec<Vec<[f32; FFT_LENGTH_BY_2]>>,
}

impl SuppressionFilter {
    pub fn new(
        optimization: Aec3Optimization,
        sample_rate_hz: i32,
        num_capture_channels: usize,
    ) -> Self {
        assert!(
            valid_full_band_rate(sample_rate_hz),
            "invalid sample rate {sample_rate_hz}"
        );
        assert!(num_capture_channels > 0);
        let num_bands = num_bands_for_rate(sample_rate_hz);
        let e_output_old = (0..num_bands)
            .map(|_| vec![[0.0f32; FFT_LENGTH_BY_2]; num_capture_channels])
            .collect();
        Self {
            optimization,
            sample_rate_hz,
            num_capture_channels,
            fft: Aec3Fft::new(),
            e_output_old,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn apply_gain(
        &mut self,
        comfort_noise: &[FftData],
        comfort_noise_high_bands: &[FftData],
        suppression_gain: &[f32; FFT_LENGTH_BY_2_PLUS_1],
        high_bands_gain: f32,
        e_lowest_band: &[FftData],
        mut e: Option<&mut Block>,
    ) {
        let e = e
            .as_deref_mut()
            .expect("suppressor output must be provided");
        assert_eq!(comfort_noise.len(), self.num_capture_channels);
        assert_eq!(comfort_noise_high_bands.len(), self.num_capture_channels);
        assert_eq!(e_lowest_band.len(), self.num_capture_channels);
        let expected_bands = num_bands_for_rate(self.sample_rate_hz);
        assert_eq!(expected_bands, self.e_output_old.len());
        assert_eq!(e.num_bands(), expected_bands);
        assert_eq!(e.num_channels(), self.num_capture_channels);
        let num_bands = expected_bands;

        let vector_math = VectorMath::new(self.optimization);
        let mut suppression_gain_sq = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
        let mut noise_gain = [1.0f32; FFT_LENGTH_BY_2_PLUS_1];
        vector_math.multiply(suppression_gain, suppression_gain, &mut suppression_gain_sq);
        for (dst, &value) in noise_gain.iter_mut().zip(suppression_gain_sq.iter()) {
            *dst = (1.0 - value).max(0.0);
        }
        vector_math.sqrt(&mut noise_gain);
        let high_bands_noise_scaling =
            0.4f32 * (1.0 - high_bands_gain * high_bands_gain).max(0.0).sqrt();
        const IFFT_NORM: f32 = 2.0 / FFT_LENGTH as f32;

        for ch in 0..self.num_capture_channels {
            let mut spectrum = FftData::default();
            spectrum.assign(&e_lowest_band[ch]);
            let mut suppressed_re = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
            let mut suppressed_im = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
            let mut comfort_re = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
            let mut comfort_im = [0.0f32; FFT_LENGTH_BY_2_PLUS_1];
            vector_math.multiply(&spectrum.re, suppression_gain, &mut suppressed_re);
            vector_math.multiply(&spectrum.im, suppression_gain, &mut suppressed_im);
            vector_math.multiply(&noise_gain, &comfort_noise[ch].re, &mut comfort_re);
            vector_math.multiply(&noise_gain, &comfort_noise[ch].im, &mut comfort_im);
            spectrum.re.copy_from_slice(&suppressed_re);
            spectrum.im.copy_from_slice(&suppressed_im);
            vector_math.accumulate(&comfort_re, &mut spectrum.re);
            vector_math.accumulate(&comfort_im, &mut spectrum.im);

            let mut e_extended = [0.0f32; FFT_LENGTH];
            self.fft.ifft(&spectrum, &mut e_extended);

            let e0 = e.view_mut(0, ch);
            let e0_old = &mut self.e_output_old[0][ch];
            for i in 0..FFT_LENGTH_BY_2 {
                e0[i] = e0_old[i] * K_SQRT_HANNING[FFT_LENGTH_BY_2 + i]
                    + e_extended[i] * K_SQRT_HANNING[i];
                e0[i] *= IFFT_NORM;
            }
            e0_old.copy_from_slice(&e_extended[FFT_LENGTH_BY_2..]);

            for band in 1..num_bands {
                for sample in e.view_mut(band, ch).iter_mut() {
                    *sample *= high_bands_gain;
                }
            }

            if num_bands > 1 {
                let mut noise_fft = FftData::default();
                noise_fft.assign(&comfort_noise_high_bands[ch]);
                let mut noise_td = [0.0f32; FFT_LENGTH];
                self.fft.ifft(&noise_fft, &mut noise_td);
                let e1 = e.view_mut(1, ch);
                let gain = high_bands_noise_scaling * IFFT_NORM;
                for i in 0..FFT_LENGTH_BY_2 {
                    e1[i] += noise_td[i] * gain;
                }
            }

            for band in 1..num_bands {
                let current = e.view_mut(band, ch);
                let previous_storage = &mut self.e_output_old[band][ch];
                for i in 0..FFT_LENGTH_BY_2 {
                    std::mem::swap(&mut current[i], &mut previous_storage[i]);
                }
            }

            for band in 0..num_bands {
                for sample in e.view_mut(band, ch).iter_mut() {
                    *sample = sample.clamp(-32_768.0, 32_767.0);
                }
            }
        }
    }
}

const K_SQRT_HANNING: [f32; FFT_LENGTH] = [
    0.0,
    0.024541229,
    0.049067676,
    0.07356456,
    0.09801714,
    0.12241068,
    0.14673048,
    0.17096189,
    0.19509032,
    0.21910124,
    0.24298018,
    0.26671276,
    0.29028466,
    0.31368175,
    0.33688986,
    0.35989505,
    0.38268343,
    0.4052413,
    0.4275551,
    0.44961134,
    0.47139674,
    0.4928982,
    0.51410276,
    0.53499764,
    0.55557024,
    0.57580817,
    0.5956993,
    0.6152316,
    0.6343933,
    0.65317285,
    0.6715589,
    0.68954057,
    0.70710677,
    0.7242471,
    0.7409511,
    0.7572088,
    0.77301043,
    0.7883464,
    0.8032075,
    0.8175848,
    0.8314696,
    0.8448536,
    0.8577286,
    0.87008697,
    0.8819213,
    0.8932243,
    0.9039893,
    0.9142098,
    0.92387956,
    0.9329928,
    0.94154406,
    0.94952816,
    0.95694035,
    0.96377605,
    0.97003126,
    0.9757021,
    0.98078525,
    0.98527765,
    0.9891765,
    0.99247956,
    0.9951847,
    0.99729043,
    0.99879545,
    0.9996988,
    1.0,
    0.9996988,
    0.99879545,
    0.99729043,
    0.9951847,
    0.99247956,
    0.9891765,
    0.98527765,
    0.98078525,
    0.9757021,
    0.97003126,
    0.96377605,
    0.95694035,
    0.94952816,
    0.94154406,
    0.9329928,
    0.92387956,
    0.9142098,
    0.9039893,
    0.8932243,
    0.8819213,
    0.87008697,
    0.8577286,
    0.8448536,
    0.8314696,
    0.8175848,
    0.8032075,
    0.7883464,
    0.77301043,
    0.7572088,
    0.7409511,
    0.7242471,
    0.70710677,
    0.68954057,
    0.6715589,
    0.65317285,
    0.6343933,
    0.6152316,
    0.5956993,
    0.57580817,
    0.55557024,
    0.53499764,
    0.51410276,
    0.4928982,
    0.47139674,
    0.44961134,
    0.4275551,
    0.4052413,
    0.38268343,
    0.35989505,
    0.33688986,
    0.31368175,
    0.29028466,
    0.26671276,
    0.24298018,
    0.21910124,
    0.19509032,
    0.17096189,
    0.14673048,
    0.12241068,
    0.09801714,
    0.07356456,
    0.049067676,
    0.024541229,
];

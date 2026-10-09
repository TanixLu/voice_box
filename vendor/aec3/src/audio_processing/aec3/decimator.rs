use num_complex::Complex32;

use crate::audio_processing::utility::cascaded_biquad_filter::{BiQuadParam, CascadedBiQuadFilter};

use super::aec3_common::BLOCK_SIZE;

pub struct Decimator {
    down_sampling_factor: usize,
    anti_aliasing_filter: CascadedBiQuadFilter,
    noise_reduction_filter: CascadedBiQuadFilter,
}

impl Decimator {
    pub fn new(down_sampling_factor: usize) -> Self {
        assert!(matches!(down_sampling_factor, 2 | 4 | 8));
        let anti_alias_params = match down_sampling_factor {
            2 => low_pass_filter_ds2(),
            4 => low_pass_filter_ds4(),
            8 => band_pass_filter_ds8(),
            _ => unreachable!(),
        };
        let noise_reduction_params = if down_sampling_factor == 8 {
            pass_through_filter()
        } else {
            high_pass_filter()
        };
        Self {
            down_sampling_factor,
            anti_aliasing_filter: CascadedBiQuadFilter::from_params(&anti_alias_params),
            noise_reduction_filter: CascadedBiQuadFilter::from_params(&noise_reduction_params),
        }
    }

    pub fn decimate(&mut self, input: &[f32], output: &mut [f32]) {
        assert_eq!(input.len(), BLOCK_SIZE);
        assert_eq!(output.len(), BLOCK_SIZE / self.down_sampling_factor);
        let mut filtered = [0.0f32; BLOCK_SIZE];
        self.anti_aliasing_filter.process(input, &mut filtered);
        self.noise_reduction_filter.process_in_place(&mut filtered);
        let mut k = 0usize;
        for sample in output.iter_mut() {
            *sample = filtered[k];
            k += self.down_sampling_factor;
        }
    }
}

fn low_pass_filter_ds2() -> Vec<BiQuadParam> {
    vec![
        BiQuadParam::new(
            Complex32::new(-1.0, 0.0),
            Complex32::new(0.13833231, 0.40743176),
            0.22711796393486466,
        ),
        BiQuadParam::new(
            Complex32::new(-1.0, 0.0),
            Complex32::new(0.13833231, 0.40743176),
            0.22711796393486466,
        ),
        BiQuadParam::new(
            Complex32::new(-1.0, 0.0),
            Complex32::new(0.13833231, 0.40743176),
            0.22711796393486466,
        ),
    ]
}

fn low_pass_filter_ds4() -> Vec<BiQuadParam> {
    vec![
        BiQuadParam::new(
            Complex32::new(-0.08873842, 0.99605496),
            Complex32::new(0.75916227, 0.23841065),
            0.26250696827,
        ),
        BiQuadParam::new(
            Complex32::new(0.62273832, 0.78243018),
            Complex32::new(0.74892112, 0.5410152),
            0.26250696827,
        ),
        BiQuadParam::new(
            Complex32::new(0.71107693, 0.70311421),
            Complex32::new(0.74895534, 0.63924616),
            0.26250696827,
        ),
    ]
}

fn band_pass_filter_ds8() -> Vec<BiQuadParam> {
    let param = BiQuadParam {
        zero: Complex32::new(1.0, 0.0),
        pole: Complex32::new(0.7601815, 0.46423542),
        gain: 0.10330478266505948,
        mirror_zero_along_i_axis: true,
    };
    vec![param, param, param, param, param]
}

fn high_pass_filter() -> Vec<BiQuadParam> {
    vec![BiQuadParam::new(
        Complex32::new(1.0, 0.0),
        Complex32::new(0.72712179, 0.21296904),
        0.7570763753338849,
    )]
}

fn pass_through_filter() -> Vec<BiQuadParam> {
    Vec::new()
}

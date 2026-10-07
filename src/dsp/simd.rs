//! Single-threaded SIMD kernels. CPU selection is performed during model creation.

use pulp::{Arch, Simd, WithSimd};

#[derive(Clone, Copy)]
pub(super) struct Kernels(Arch);

impl Kernels {
    pub(super) fn new() -> Self {
        Self(Arch::new())
    }

    pub(super) fn linear(self, weight: &[f32], input: &[f32], bias: &[f32], output: &mut [f32]) {
        assert_eq!(weight.len(), output.len() * input.len());
        assert_eq!(bias.len(), output.len());
        self.0.dispatch(Linear {
            weight,
            input,
            bias,
            output,
        });
    }

    pub(super) fn norm(self, data: &mut [f32], scale: f32, bias: f32) {
        self.0.dispatch(Affine { data, scale, bias });
    }

    pub(super) fn prelu(self, data: &mut [f32], slope: f32) {
        self.0.dispatch(Prelu { data, slope });
    }
}

struct Linear<'a> {
    weight: &'a [f32],
    input: &'a [f32],
    bias: &'a [f32],
    output: &'a mut [f32],
}

impl WithSimd for Linear<'_> {
    type Output = ();
    #[inline(always)]
    fn with_simd<S: Simd>(self, simd: S) {
        let (input, input_tail) = S::as_simd_f32s(self.input);
        for ((weight, bias), output) in self
            .weight
            .chunks_exact(self.input.len())
            .zip(self.bias)
            .zip(self.output)
        {
            let (weight, weight_tail) = S::as_simd_f32s(weight);
            let mut sum = simd.splat_f32s(0.0);
            for (&a, &b) in weight.iter().zip(input) {
                sum = simd.add_f32s(sum, simd.mul_f32s(a, b));
            }
            let mut value = *bias + simd.reduce_sum_f32s(sum);
            for (&a, &b) in weight_tail.iter().zip(input_tail) {
                value += a * b;
            }
            *output = value;
        }
    }
}

struct Affine<'a> {
    data: &'a mut [f32],
    scale: f32,
    bias: f32,
}
impl WithSimd for Affine<'_> {
    type Output = ();
    #[inline(always)]
    fn with_simd<S: Simd>(self, simd: S) {
        let (head, tail) = S::as_mut_simd_f32s(self.data);
        let scale = simd.splat_f32s(self.scale);
        let bias = simd.splat_f32s(self.bias);
        for value in head {
            *value = simd.add_f32s(simd.mul_f32s(*value, scale), bias);
        }
        for value in tail {
            *value = *value * self.scale + self.bias;
        }
    }
}

struct Prelu<'a> {
    data: &'a mut [f32],
    slope: f32,
}
impl WithSimd for Prelu<'_> {
    type Output = ();
    #[inline(always)]
    fn with_simd<S: Simd>(self, simd: S) {
        let (head, tail) = S::as_mut_simd_f32s(self.data);
        let zero = simd.splat_f32s(0.0);
        let slope = simd.splat_f32s(self.slope);
        for value in head {
            *value = simd.select_f32s(
                simd.greater_than_or_equal_f32s(*value, zero),
                *value,
                simd.mul_f32s(*value, slope),
            );
        }
        for value in tail {
            if *value < 0.0 {
                *value *= self.slope;
            }
        }
    }
}

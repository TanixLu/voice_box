//! Fixed-shape, allocation-free inference for the official GTCRN DNS3 model.
//!
//! Architecture and weights: Rong Xiaobin, MIT, see assets/gtcrn/LICENSE.
//! The frequency-path GRUs are bidirectional within the current frame; all
//! temporal convolution and GRU state uses past frames only.

use super::StreamingModel;
use super::simd::Kernels;
use super::spectral::{Complex, Stft};
use super::weights;

const WIDTH: usize = 33;
const CHANNELS: usize = 16;
const FEATURES: usize = WIDTH * CHANNELS;

struct BatchNorm {
    kernels: Kernels,
    scale: [f32; CHANNELS],
    bias: [f32; CHANNELS],
}

impl BatchNorm {
    fn new(weights: weights::Norm) -> Self {
        let weight = weights.parameters.weight;
        let bias = weights.parameters.bias;
        let mean = weights.mean;
        let variance = weights.variance;
        let mut result = Self {
            kernels: Kernels::new(),
            scale: [0.0; CHANNELS],
            bias: [0.0; CHANNELS],
        };
        for i in 0..weight.len() {
            result.scale[i] = weight[i] / (variance[i] + 1e-5).sqrt();
            result.bias[i] = bias[i] - mean[i] * result.scale[i];
        }
        result
    }

    fn apply(&self, data: &mut [f32], width: usize) {
        for (c, row) in data.chunks_exact_mut(width).enumerate() {
            self.kernels.norm(row, self.scale[c], self.bias[c]);
        }
    }
}

struct Conv {
    kernels: Kernels,
    weight: &'static [f32],
    bias: &'static [f32],
    norm: BatchNorm,
    slope: f32,
    inputs: usize,
    outputs: usize,
    kernel: usize,
    stride: usize,
    groups: usize,
    transpose: bool,
    last: bool,
}

impl Conv {
    fn new(
        weights: weights::Conv,
        inputs: usize,
        outputs: usize,
        transpose: bool,
        groups: usize,
        last: bool,
    ) -> Self {
        Self {
            kernels: Kernels::new(),
            weight: weights.parameters.weight,
            bias: weights.parameters.bias,
            norm: BatchNorm::new(weights.norm),
            slope: weights.slope,
            inputs,
            outputs,
            kernel: 5,
            stride: 2,
            groups,
            transpose,
            last,
        }
    }

    fn point(weights: weights::Conv, inputs: usize, outputs: usize, transpose: bool) -> Self {
        Self {
            kernels: Kernels::new(),
            weight: weights.parameters.weight,
            bias: weights.parameters.bias,
            norm: BatchNorm::new(weights.norm),
            slope: weights.slope,
            inputs,
            outputs,
            kernel: 1,
            stride: 1,
            groups: 1,
            transpose,
            last: false,
        }
    }

    fn run(&self, input: &[f32], width: usize, output: &mut [f32]) {
        let out_width = output.len() / self.outputs;
        let in_group = self.inputs / self.groups;
        let out_group = self.outputs / self.groups;
        let pad = self.kernel / 2;
        for co in 0..self.outputs {
            let group = co / out_group;
            for f in 0..out_width {
                let mut sum = self.bias[co];
                for ci_local in 0..in_group {
                    let ci = group * in_group + ci_local;
                    for k in 0..self.kernel {
                        let source = if self.transpose {
                            let index = f as isize + pad as isize - k as isize;
                            if index < 0 || !(index as usize).is_multiple_of(self.stride) {
                                continue;
                            }
                            index as usize / self.stride
                        } else {
                            let index = (f * self.stride + k) as isize - pad as isize;
                            if index < 0 {
                                continue;
                            }
                            index as usize
                        };
                        if source >= width {
                            continue;
                        }
                        let wi = if self.transpose {
                            (ci * out_group + co % out_group) * self.kernel + k
                        } else {
                            (co * in_group + ci_local) * self.kernel + k
                        };
                        sum += input[ci * width + source] * self.weight[wi];
                    }
                }
                output[co * out_width + f] = sum;
            }
        }
        self.norm.apply(output, out_width);
        if self.last {
            for value in output {
                *value = value.tanh();
            }
        } else {
            self.kernels.prelu(output, self.slope);
        }
    }
}

fn prelu(value: f32, slope: f32) -> f32 {
    if value >= 0.0 { value } else { value * slope }
}

fn sigmoid(value: f32) -> f32 {
    1.0 / (1.0 + (-value).exp())
}

struct Gru {
    kernels: Kernels,
    weight_input: &'static [f32],
    weight_hidden: &'static [f32],
    bias_input: &'static [f32],
    bias_hidden: &'static [f32],
    inputs: usize,
    hidden: usize,
}

impl Gru {
    fn new(weights: weights::Gru, inputs: usize, hidden: usize) -> Self {
        Self {
            kernels: Kernels::new(),
            weight_input: weights.weight_input,
            weight_hidden: weights.weight_hidden,
            bias_input: weights.bias_input,
            bias_hidden: weights.bias_hidden,
            inputs,
            hidden,
        }
    }

    fn step(&self, input: &[f32], state: &mut [f32]) {
        let mut gates_input = [0.0; 48];
        let mut gates_hidden = [0.0; 48];
        let gates = self.hidden * 3;
        self.kernels.linear(
            self.weight_input,
            &input[..self.inputs],
            self.bias_input,
            &mut gates_input[..gates],
        );
        self.kernels.linear(
            self.weight_hidden,
            &state[..self.hidden],
            self.bias_hidden,
            &mut gates_hidden[..gates],
        );
        for i in 0..self.hidden {
            let reset = sigmoid(gates_input[i] + gates_hidden[i]);
            let update = sigmoid(gates_input[self.hidden + i] + gates_hidden[self.hidden + i]);
            let new = (gates_input[2 * self.hidden + i]
                + reset * gates_hidden[2 * self.hidden + i])
                .tanh();
            state[i] = (1.0 - update) * new + update * state[i];
        }
    }
}

struct Linear {
    kernels: Kernels,
    weight: &'static [f32],
    bias: &'static [f32],
    inputs: usize,
}

impl Linear {
    fn new(weights: weights::Pair, inputs: usize) -> Self {
        Self {
            kernels: Kernels::new(),
            weight: weights.weight,
            bias: weights.bias,
            inputs,
        }
    }

    fn run(&self, input: &[f32], output: &mut [f32]) {
        self.kernels
            .linear(self.weight, &input[..self.inputs], self.bias, output);
    }
}

struct GtBlock {
    point1: Conv,
    point2: Conv,
    depth_weight: &'static [f32],
    depth_bias: &'static [f32],
    depth_norm: BatchNorm,
    depth_slope: f32,
    attention: Gru,
    attention_fc: Linear,
    attention_state: [f32; 16],
    history: Vec<f32>,
    dilation: usize,
    transpose: bool,
}

impl GtBlock {
    fn new(weights: weights::GtBlock, dilation: usize, transpose: bool) -> Self {
        Self {
            point1: Conv::point(weights.point[0], 24, 16, transpose),
            point2: Conv::point(weights.point[1], 16, 8, transpose),
            depth_weight: weights.depth.parameters.weight,
            depth_bias: weights.depth.parameters.bias,
            depth_norm: BatchNorm::new(weights.depth.norm),
            depth_slope: weights.depth.slope,
            attention: Gru::new(weights.attention, 8, 16),
            attention_fc: Linear::new(weights.attention_fc, 16),
            attention_state: [0.0; 16],
            history: vec![0.0; 2 * dilation * FEATURES],
            dilation,
            transpose,
        }
    }

    fn reset(&mut self) {
        self.history.fill(0.0);
        self.attention_state.fill(0.0);
    }

    fn run(&mut self, input: &[f32; FEATURES], output: &mut [f32; FEATURES]) {
        let mut subbands = [0.0; 24 * WIDTH];
        unfold(&input[..8 * WIDTH], WIDTH, &mut subbands);
        let mut point = [0.0; FEATURES];
        self.point1.run(&subbands, WIDTH, &mut point);
        let mut depth = [0.0; FEATURES];
        for c in 0..16 {
            for f in 0..WIDTH {
                let mut sum = self.depth_bias[c];
                for t in 0..3 {
                    for k in 0..3 {
                        let source = f as isize + k as isize - 1;
                        if source < 0 || source as usize >= WIDTH {
                            continue;
                        }
                        let index = c * WIDTH + source as usize;
                        let value = if t == 2 {
                            point[index]
                        } else {
                            self.history[t * self.dilation * FEATURES + index]
                        };
                        let wi = if self.transpose {
                            c * 9 + (2 - t) * 3 + (2 - k)
                        } else {
                            c * 9 + t * 3 + k
                        };
                        sum += value * self.depth_weight[wi];
                    }
                }
                depth[c * WIDTH + f] = sum;
            }
        }
        self.history.copy_within(FEATURES.., 0);
        let offset = self.history.len() - FEATURES;
        self.history[offset..].copy_from_slice(&point);
        self.depth_norm.apply(&mut depth, WIDTH);
        for value in &mut depth {
            *value = prelu(*value, self.depth_slope);
        }
        let mut attended = [0.0; 8 * WIDTH];
        self.point2.run(&depth, WIDTH, &mut attended);
        let mut energy = [0.0; 8];
        for (c, row) in attended.as_chunks::<WIDTH>().0.iter().enumerate() {
            energy[c] = row.iter().map(|x| x * x).sum::<f32>() / WIDTH as f32;
        }
        self.attention.step(&energy, &mut self.attention_state);
        let mut gains = [0.0; 8];
        self.attention_fc.run(&self.attention_state, &mut gains);
        for c in 0..8 {
            let gain = sigmoid(gains[c]);
            for f in 0..WIDTH {
                output[(c * 2) * WIDTH + f] = attended[c * WIDTH + f] * gain;
                output[(c * 2 + 1) * WIDTH + f] = input[(c + 8) * WIDTH + f];
            }
        }
    }
}

fn unfold(input: &[f32], width: usize, output: &mut [f32]) {
    for (c, row) in input.chunks_exact(width).enumerate() {
        for k in 0..3 {
            for f in 0..width {
                let source = f as isize + k as isize - 1;
                output[(c * 3 + k) * width + f] = if source >= 0 && source < width as isize {
                    row[source as usize]
                } else {
                    0.0
                };
            }
        }
    }
}

struct LayerNorm {
    weight: &'static [f32],
    bias: &'static [f32],
}

impl LayerNorm {
    fn new(weights: weights::Pair) -> Self {
        Self {
            weight: weights.weight,
            bias: weights.bias,
        }
    }

    fn apply(&self, values: &mut [f32; FEATURES]) {
        let mean = values.iter().sum::<f32>() / FEATURES as f32;
        let variance =
            values.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / FEATURES as f32;
        let scale = 1.0 / (variance + 1e-8).sqrt();
        for (i, value) in values.iter_mut().enumerate() {
            *value = (*value - mean) * scale * self.weight[i] + self.bias[i];
        }
    }
}

struct DpBlock {
    intra: [Gru; 4],
    intra_fc: Linear,
    intra_norm: LayerNorm,
    inter: [Gru; 2],
    inter_fc: Linear,
    inter_norm: LayerNorm,
    state: [f32; FEATURES],
}

impl DpBlock {
    fn new(weights: weights::DpBlock) -> Self {
        Self {
            intra: std::array::from_fn(|i| Gru::new(weights.intra[i], 8, 4)),
            intra_fc: Linear::new(weights.intra_fc, 16),
            intra_norm: LayerNorm::new(weights.intra_norm),
            inter: std::array::from_fn(|i| Gru::new(weights.inter[i], 8, 8)),
            inter_fc: Linear::new(weights.inter_fc, 16),
            inter_norm: LayerNorm::new(weights.inter_norm),
            state: [0.0; FEATURES],
        }
    }

    fn run(&mut self, values: &mut [f32; FEATURES]) {
        // Frequency-major layout matches LayerNorm's (width, channels) axes.
        let mut intra = [0.0; FEATURES];
        for (i, gru) in self.intra.iter().enumerate() {
            let mut state = [0.0; 4];
            for step in 0..WIDTH {
                let f = if i % 2 == 0 { step } else { WIDTH - 1 - step };
                let mut input = [0.0; 8];
                for c in 0..8 {
                    input[c] = values[(i / 2 * 8 + c) * WIDTH + f];
                }
                gru.step(&input, &mut state);
                intra[f * CHANNELS + i * 4..f * CHANNELS + i * 4 + 4].copy_from_slice(&state);
            }
        }
        let mut projected = [0.0; FEATURES];
        for f in 0..WIDTH {
            self.intra_fc.run(
                &intra[f * CHANNELS..],
                &mut projected[f * CHANNELS..(f + 1) * CHANNELS],
            );
        }
        self.intra_norm.apply(&mut projected);
        for f in 0..WIDTH {
            for c in 0..CHANNELS {
                projected[f * CHANNELS + c] += values[c * WIDTH + f];
            }
        }
        let mut inter = [0.0; FEATURES];
        for f in 0..WIDTH {
            for g in 0..2 {
                self.inter[g].step(
                    &projected[f * CHANNELS + g * 8..],
                    &mut self.state[f * CHANNELS + g * 8..f * CHANNELS + (g + 1) * 8],
                );
            }
            self.inter_fc.run(
                &self.state[f * CHANNELS..],
                &mut inter[f * CHANNELS..(f + 1) * CHANNELS],
            );
        }
        self.inter_norm.apply(&mut inter);
        for f in 0..WIDTH {
            for c in 0..CHANNELS {
                values[c * WIDTH + f] = projected[f * CHANNELS + c] + inter[f * CHANNELS + c];
            }
        }
    }
}

pub(super) struct Gtcrn {
    enabled: bool,
    kernels: Kernels,
    encoder: [Conv; 2],
    blocks: [GtBlock; 6],
    dual_path: [DpBlock; 2],
    decoder: [Conv; 2],
    stft: Stft,
}

impl Gtcrn {
    pub(super) fn new() -> Self {
        Self {
            enabled: true,
            kernels: Kernels::new(),
            encoder: [
                Conv::new(weights::ENCODER[0], 9, 16, false, 1, false),
                Conv::new(weights::ENCODER[1], 16, 16, false, 2, false),
            ],
            blocks: std::array::from_fn(|i| {
                if i < 3 {
                    GtBlock::new(weights::BLOCKS[i], [1, 2, 5][i], false)
                } else {
                    GtBlock::new(weights::BLOCKS[i], [5, 2, 1][i - 3], true)
                }
            }),
            dual_path: [
                DpBlock::new(weights::DUAL_PATH[0]),
                DpBlock::new(weights::DUAL_PATH[1]),
            ],
            decoder: [
                Conv::new(weights::DECODER[0], 16, 16, true, 2, false),
                Conv::new(weights::DECODER[1], 16, 2, true, 1, true),
            ],
            stft: Stft::new(),
        }
    }

    fn enhance(&mut self, spectrum: &mut [Complex; 257]) {
        let filter = weights::ERB;
        let mut erb = [0.0; 3 * 129];
        for c in 0..3 {
            let mut feature = [0.0; 257];
            for (f, value) in feature.iter_mut().enumerate() {
                *value = match c {
                    0 => {
                        (spectrum[f].re * spectrum[f].re + spectrum[f].im * spectrum[f].im + 1e-12)
                            .sqrt()
                    }
                    1 => spectrum[f].re,
                    _ => spectrum[f].im,
                };
            }
            erb[c * 129..c * 129 + 65].copy_from_slice(&feature[..65]);
            self.kernels.linear(
                filter,
                &feature[65..],
                &[0.0; 64],
                &mut erb[c * 129 + 65..(c + 1) * 129],
            );
        }
        let mut subbands = [0.0; 9 * 129];
        unfold(&erb, 129, &mut subbands);
        let mut enc0 = [0.0; 16 * 65];
        self.encoder[0].run(&subbands, 129, &mut enc0);
        let mut skips = [[0.0; FEATURES]; 4];
        self.encoder[1].run(&enc0, 65, &mut skips[0]);
        for i in 0..3 {
            let (previous, next) = skips.split_at_mut(i + 1);
            self.blocks[i].run(&previous[i], &mut next[0]);
        }
        let mut current = skips[3];
        for block in &mut self.dual_path {
            block.run(&mut current);
        }
        for i in 0..3 {
            for j in 0..FEATURES {
                current[j] += skips[3 - i][j];
            }
            let mut next = [0.0; FEATURES];
            self.blocks[3 + i].run(&current, &mut next);
            current = next;
        }
        for j in 0..FEATURES {
            current[j] += skips[0][j];
        }
        let mut dec0 = [0.0; 16 * 65];
        self.decoder[0].run(&current, WIDTH, &mut dec0);
        for j in 0..dec0.len() {
            dec0[j] += enc0[j];
        }
        let mut mask = [0.0; 2 * 129];
        self.decoder[1].run(&dec0, 65, &mut mask);
        let inverse = weights::INVERSE_ERB;
        let mut expanded = [[0.0; 257]; 2];
        for (c, output) in expanded.iter_mut().enumerate() {
            output[..65].copy_from_slice(&mask[c * 129..c * 129 + 65]);
            self.kernels.linear(
                inverse,
                &mask[c * 129 + 65..(c + 1) * 129],
                &[0.0; 192],
                &mut output[65..],
            );
        }
        for f in 0..257 {
            let m = [expanded[0][f], expanded[1][f]];
            let input = spectrum[f];
            spectrum[f] = Complex {
                re: input.re * m[0] - input.im * m[1],
                im: input.im * m[0] + input.re * m[1],
            };
        }
    }
}

impl StreamingModel for Gtcrn {
    fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            for block in &mut self.blocks {
                block.reset();
            }
            for block in &mut self.dual_path {
                block.state.fill(0.0);
            }
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
        for block in &mut self.blocks {
            block.reset();
        }
        for block in &mut self.dual_path {
            block.state.fill(0.0);
        }
        self.stft.reset();
    }

    fn process(&mut self, input: &[f32], output: &mut [f32]) {
        let mut spectrum = self.stft.analyze(input);
        if self.enabled {
            self.enhance(&mut spectrum);
        }
        self.stft.synthesize(&spectrum, output);
    }
}

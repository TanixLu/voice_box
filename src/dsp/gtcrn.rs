//! Fixed-shape, allocation-free inference for the official GTCRN DNS3 model.
//!
//! Architecture and weights: Rong Xiaobin, MIT, see assets/gtcrn/LICENSE.
//! The frequency-path GRUs are bidirectional within the current frame; all
//! temporal convolution and GRU state uses past frames only.

use super::StreamingModel;
use super::spectral::{Complex, Stft};
use super::weights::tensor;

const WIDTH: usize = 33;
const CHANNELS: usize = 16;
const FEATURES: usize = WIDTH * CHANNELS;

struct BatchNorm {
    scale: [f32; CHANNELS],
    bias: [f32; CHANNELS],
}

impl BatchNorm {
    fn new(prefix: &str) -> Self {
        let weight = tensor(&format!("{prefix}.weight"));
        let bias = tensor(&format!("{prefix}.bias"));
        let mean = tensor(&format!("{prefix}.running_mean"));
        let variance = tensor(&format!("{prefix}.running_var"));
        let mut result = Self {
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
            for value in row {
                *value = *value * self.scale[c] + self.bias[c];
            }
        }
    }
}

struct Conv {
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
        prefix: &str,
        inputs: usize,
        outputs: usize,
        transpose: bool,
        groups: usize,
        last: bool,
    ) -> Self {
        Self {
            weight: tensor(&format!("{prefix}.conv.weight")),
            bias: tensor(&format!("{prefix}.conv.bias")),
            norm: BatchNorm::new(&format!("{prefix}.bn")),
            slope: if last {
                0.0
            } else {
                tensor(&format!("{prefix}.act.weight"))[0]
            },
            inputs,
            outputs,
            kernel: 5,
            stride: 2,
            groups,
            transpose,
            last,
        }
    }

    fn point(prefix: &str, stage: usize, inputs: usize, outputs: usize, transpose: bool) -> Self {
        Self {
            weight: tensor(&format!("{prefix}.point_conv{stage}.weight")),
            bias: tensor(&format!("{prefix}.point_conv{stage}.bias")),
            norm: BatchNorm::new(&format!("{prefix}.point_bn{stage}")),
            slope: if stage == 1 {
                tensor(&format!("{prefix}.point_act.weight"))[0]
            } else {
                1.0
            },
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
        for value in output {
            *value = if self.last {
                value.tanh()
            } else {
                prelu(*value, self.slope)
            };
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
    weight_input: &'static [f32],
    weight_hidden: &'static [f32],
    bias_input: &'static [f32],
    bias_hidden: &'static [f32],
    inputs: usize,
    hidden: usize,
}

impl Gru {
    fn new(prefix: &str, inputs: usize, hidden: usize, reverse: bool) -> Self {
        let suffix = if reverse { "_reverse" } else { "" };
        Self {
            weight_input: tensor(&format!("{prefix}.weight_ih_l0{suffix}")),
            weight_hidden: tensor(&format!("{prefix}.weight_hh_l0{suffix}")),
            bias_input: tensor(&format!("{prefix}.bias_ih_l0{suffix}")),
            bias_hidden: tensor(&format!("{prefix}.bias_hh_l0{suffix}")),
            inputs,
            hidden,
        }
    }

    fn step(&self, input: &[f32], state: &mut [f32]) {
        let mut gates_input = [0.0; 48];
        let mut gates_hidden = [0.0; 48];
        for row in 0..self.hidden * 3 {
            let mut a = self.bias_input[row];
            let mut b = self.bias_hidden[row];
            for (i, value) in input[..self.inputs].iter().enumerate() {
                a += self.weight_input[row * self.inputs + i] * value;
            }
            for (i, value) in state[..self.hidden].iter().enumerate() {
                b += self.weight_hidden[row * self.hidden + i] * value;
            }
            gates_input[row] = a;
            gates_hidden[row] = b;
        }
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
    weight: &'static [f32],
    bias: &'static [f32],
    inputs: usize,
}

impl Linear {
    fn new(prefix: &str, inputs: usize) -> Self {
        Self {
            weight: tensor(&format!("{prefix}.weight")),
            bias: tensor(&format!("{prefix}.bias")),
            inputs,
        }
    }

    fn run(&self, input: &[f32], output: &mut [f32]) {
        for (row, value) in output.iter_mut().enumerate() {
            let mut sum = self.bias[row];
            for (i, input) in input[..self.inputs].iter().enumerate() {
                sum += self.weight[row * self.inputs + i] * input;
            }
            *value = sum;
        }
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
    fn new(prefix: &str, dilation: usize, transpose: bool) -> Self {
        Self {
            point1: Conv::point(prefix, 1, 24, 16, transpose),
            point2: Conv::point(prefix, 2, 16, 8, transpose),
            depth_weight: tensor(&format!("{prefix}.depth_conv.weight")),
            depth_bias: tensor(&format!("{prefix}.depth_conv.bias")),
            depth_norm: BatchNorm::new(&format!("{prefix}.depth_bn")),
            depth_slope: tensor(&format!("{prefix}.depth_act.weight"))[0],
            attention: Gru::new(&format!("{prefix}.tra.att_gru"), 8, 16, false),
            attention_fc: Linear::new(&format!("{prefix}.tra.att_fc"), 16),
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
    fn new(prefix: &str) -> Self {
        Self {
            weight: tensor(&format!("{prefix}.weight")),
            bias: tensor(&format!("{prefix}.bias")),
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
    fn new(prefix: &str) -> Self {
        Self {
            intra: std::array::from_fn(|i| {
                Gru::new(
                    &format!("{prefix}.intra_rnn.rnn{}", i / 2 + 1),
                    8,
                    4,
                    i % 2 == 1,
                )
            }),
            intra_fc: Linear::new(&format!("{prefix}.intra_fc"), 16),
            intra_norm: LayerNorm::new(&format!("{prefix}.intra_ln")),
            inter: std::array::from_fn(|i| {
                Gru::new(&format!("{prefix}.inter_rnn.rnn{}", i + 1), 8, 8, false)
            }),
            inter_fc: Linear::new(&format!("{prefix}.inter_fc"), 16),
            inter_norm: LayerNorm::new(&format!("{prefix}.inter_ln")),
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
    encoder: [Conv; 2],
    blocks: [GtBlock; 6],
    dual_path: [DpBlock; 2],
    decoder: [Conv; 2],
    stft: Stft,
}

impl Gtcrn {
    pub(super) fn new() -> Self {
        Self {
            encoder: [
                Conv::new("encoder.en_convs.0", 9, 16, false, 1, false),
                Conv::new("encoder.en_convs.1", 16, 16, false, 2, false),
            ],
            blocks: std::array::from_fn(|i| {
                if i < 3 {
                    GtBlock::new(&format!("encoder.en_convs.{}", i + 2), [1, 2, 5][i], false)
                } else {
                    GtBlock::new(
                        &format!("decoder.de_convs.{}", i - 3),
                        [5, 2, 1][i - 3],
                        true,
                    )
                }
            }),
            dual_path: [DpBlock::new("dpgrnn1"), DpBlock::new("dpgrnn2")],
            decoder: [
                Conv::new("decoder.de_convs.3", 16, 16, true, 2, false),
                Conv::new("decoder.de_convs.4", 16, 2, true, 1, true),
            ],
            stft: Stft::new(),
        }
    }

    fn enhance(&mut self, spectrum: &mut [Complex; 257]) {
        let filter = tensor("erb.erb_fc.weight");
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
            for b in 0..64 {
                let mut sum = 0.0;
                for f in 0..192 {
                    sum += filter[b * 192 + f] * feature[f + 65];
                }
                erb[c * 129 + 65 + b] = sum;
            }
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
        let inverse = tensor("erb.ierb_fc.weight");
        for f in 0..257 {
            let mut m = [0.0; 2];
            for c in 0..2 {
                m[c] = if f < 65 {
                    mask[c * 129 + f]
                } else {
                    let mut sum = 0.0;
                    for b in 0..64 {
                        sum += inverse[(f - 65) * 64 + b] * mask[c * 129 + 65 + b];
                    }
                    sum
                };
            }
            let input = spectrum[f];
            spectrum[f] = Complex {
                re: input.re * m[0] - input.im * m[1],
                im: input.im * m[0] + input.re * m[1],
            };
        }
    }
}

impl StreamingModel for Gtcrn {
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
        self.enhance(&mut spectrum);
        self.stft.synthesize(&spectrum, output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recurrent_inference_matches_official_onnx() {
        let input = include_bytes!("../../assets/gtcrn/reference-input.f32");
        let expected = include_bytes!("../../assets/gtcrn/reference-output.f32");
        let mut model = Gtcrn::new();
        let mut max_error = 0.0_f32;
        for (frame, (input, expected)) in input
            .as_chunks::<{ 257 * 8 }>()
            .0
            .iter()
            .zip(expected.as_chunks::<{ 257 * 8 }>().0.iter())
            .enumerate()
        {
            let mut spectrum = std::array::from_fn(|i| Complex {
                re: f32::from_le_bytes(input[i * 8..i * 8 + 4].try_into().unwrap()),
                im: f32::from_le_bytes(input[i * 8 + 4..i * 8 + 8].try_into().unwrap()),
            });
            model.enhance(&mut spectrum);
            for (i, value) in spectrum.iter().enumerate() {
                for (c, actual) in [value.re, value.im].into_iter().enumerate() {
                    let offset = i * 8 + c * 4;
                    let reference =
                        f32::from_le_bytes(expected[offset..offset + 4].try_into().unwrap());
                    let error = (actual - reference).abs();
                    max_error = max_error.max(error);
                    assert!(
                        error < 0.00005 + 0.00001 * reference.abs(),
                        "frame={frame} bin={i} component={c} actual={actual} reference={reference} error={error}"
                    );
                }
            }
        }
        eprintln!("GTCRN reference max absolute error: {max_error}");
    }
}

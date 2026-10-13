//! Native UL-UNAS DNS3 inference with storage allocated only during creation.
//! Architecture and parameters: Xiaobin Rong, MIT; see assets/ul_unas/README.md.

use super::super::simd::Kernels;
use super::weights;

const WIDTH: usize = 33;
const CHANNELS: usize = 16;
const FEATURES: usize = WIDTH * CHANNELS;

fn sigmoid(value: f32) -> f32 {
    1.0 / (1.0 + (-value).exp())
}

struct Linear {
    kernels: Kernels,
    weight: &'static [f32],
    bias: &'static [f32],
    inputs: usize,
}

impl Linear {
    fn new(parameters: weights::Pair, inputs: usize) -> Self {
        assert_eq!(parameters.weight.len(), parameters.bias.len() * inputs);
        Self {
            kernels: Kernels::new(),
            weight: parameters.weight,
            bias: parameters.bias,
            inputs,
        }
    }

    fn run(&self, input: &[f32], output: &mut [f32]) {
        self.kernels
            .linear(self.weight, &input[..self.inputs], self.bias, output);
    }
}

struct Gru {
    kernels: Kernels,
    parameters: weights::Gru,
}

impl Gru {
    fn new(parameters: weights::Gru) -> Self {
        Self {
            kernels: Kernels::new(),
            parameters,
        }
    }

    fn step(&self, input: &[f32], state: &mut [f32]) {
        let hidden = self.parameters.hidden;
        let gates = hidden * 3;
        let mut input_gates = [0.0; 192];
        let mut hidden_gates = [0.0; 192];
        self.kernels.linear(
            self.parameters.weight_input,
            &input[..self.parameters.inputs],
            self.parameters.bias_input,
            &mut input_gates[..gates],
        );
        self.kernels.linear(
            self.parameters.weight_hidden,
            &state[..hidden],
            self.parameters.bias_hidden,
            &mut hidden_gates[..gates],
        );
        // The checkpoint retains PyTorch's reset/update/candidate gate order.
        for i in 0..hidden {
            let reset = sigmoid(input_gates[i] + hidden_gates[i]);
            let update = sigmoid(input_gates[hidden + i] + hidden_gates[hidden + i]);
            let candidate =
                (input_gates[2 * hidden + i] + reset * hidden_gates[2 * hidden + i]).tanh();
            state[i] = (1.0 - update) * candidate + update * state[i];
        }
    }
}

struct Conv {
    parameters: weights::Conv,
    kernels: Kernels,
    inputs: usize,
    outputs: usize,
    input_width: usize,
    output_width: usize,
    stride: usize,
    scale: Box<[f32]>,
    bias: Box<[f32]>,
    history: Box<[f32]>,
}

impl Conv {
    fn new(
        parameters: weights::Conv,
        input_width: usize,
        output_width: usize,
        stride: usize,
    ) -> Self {
        let inputs = if parameters.transpose {
            parameters.shape[0]
        } else {
            parameters.shape[1] * parameters.groups
        };
        let outputs = if parameters.transpose {
            parameters.shape[1] * parameters.groups
        } else {
            parameters.shape[0]
        };
        if let Some(affine) = parameters.affine {
            assert_eq!(affine.width, output_width);
        }
        let mut scale = vec![0.0; outputs].into_boxed_slice();
        let mut bias = vec![0.0; outputs].into_boxed_slice();
        for i in 0..outputs {
            scale[i] =
                parameters.norm.parameters.weight[i] / (parameters.norm.variance[i] + 1e-5).sqrt();
            bias[i] = parameters.norm.parameters.bias[i] - parameters.norm.mean[i] * scale[i];
        }
        let history = vec![
            0.0;
            if parameters.shape[2] > 1 {
                inputs * input_width * parameters.shape[2]
            } else {
                0
            }
        ]
        .into_boxed_slice();
        Self {
            parameters,
            kernels: Kernels::new(),
            inputs,
            outputs,
            input_width,
            output_width,
            stride,
            scale,
            bias,
            history,
        }
    }

    fn reset(&mut self) {
        self.history.fill(0.0);
    }

    fn run(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), self.inputs * self.input_width);
        debug_assert_eq!(output.len(), self.outputs * self.output_width);
        let kt = self.parameters.shape[2];
        let kf = self.parameters.shape[3];
        let padding = kf / 2;
        let frame = input.len();
        if kt > 1 {
            self.history.copy_within(frame.., 0);
            self.history[(kt - 1) * frame..].copy_from_slice(input);
        }
        let history = if kt > 1 { &self.history[..] } else { input };
        let in_group = self.inputs / self.parameters.groups;
        let out_group = self.outputs / self.parameters.groups;
        for co in 0..self.outputs {
            let group = co / out_group;
            for f in 0..self.output_width {
                let mut sum = self.parameters.parameters.bias[co];
                for local in 0..in_group {
                    let ci = group * in_group + local;
                    for t in 0..kt {
                        for k in 0..kf {
                            let source = if self.parameters.transpose {
                                let index = f as isize + padding as isize - k as isize;
                                if index < 0 || !(index as usize).is_multiple_of(self.stride) {
                                    continue;
                                }
                                index as usize / self.stride
                            } else {
                                let index = (f * self.stride + k) as isize - padding as isize;
                                if index < 0 {
                                    continue;
                                }
                                index as usize
                            };
                            if source >= self.input_width {
                                continue;
                            }
                            let (wi, time) = if self.parameters.transpose {
                                // Keep the current frame from the official causal
                                // ZeroPad2d + ConvTranspose2d streaming operation.
                                (
                                    ((ci * out_group + co % out_group) * kt + t) * kf + k,
                                    kt - 1 - t,
                                )
                            } else {
                                (((co * in_group + local) * kt + t) * kf + k, t)
                            };
                            sum += history[time * frame + ci * self.input_width + source]
                                * self.parameters.parameters.weight[wi];
                        }
                    }
                }
                output[co * self.output_width + f] = sum;
            }
        }
        for (c, row) in output.chunks_exact_mut(self.output_width).enumerate() {
            self.kernels.norm(row, self.scale[c], self.bias[c]);
        }
        if let Some(affine) = self.parameters.affine {
            for (i, value) in output.iter_mut().enumerate() {
                let x = *value;
                let activated = if x > 0.0 {
                    x
                } else {
                    affine.slope[i / self.output_width] * x
                };
                *value = affine.weight[i] * x + affine.bias[i] + activated;
            }
        }
    }
}

struct Attention {
    temporal: Gru,
    temporal_fc: Linear,
    temporal_state: Box<[f32]>,
    frequency: [Gru; 2],
    frequency_fc: Linear,
    channels: usize,
    width: usize,
}

impl Attention {
    fn new(parameters: weights::Attention, channels: usize, width: usize) -> Self {
        assert!(channels <= 32 && width <= 129);
        assert_eq!(parameters.temporal.inputs, channels);
        Self {
            temporal: Gru::new(parameters.temporal),
            temporal_fc: Linear::new(parameters.temporal_fc, parameters.temporal.hidden),
            temporal_state: vec![0.0; parameters.temporal.hidden].into_boxed_slice(),
            frequency: parameters.frequency.map(Gru::new),
            frequency_fc: Linear::new(parameters.frequency_fc, 8),
            channels,
            width,
        }
    }

    fn reset(&mut self) {
        self.temporal_state.fill(0.0);
    }

    fn run(&mut self, values: &mut [f32]) {
        let mut time_energy = [0.0; 32];
        let mut frequency_energy = [0.0; 132];
        for (c, row) in values.chunks_exact(self.width).enumerate() {
            for (f, &sample) in row.iter().enumerate() {
                time_energy[c] += sample * sample / self.width as f32;
                frequency_energy[f] += sample * sample / self.channels as f32;
            }
        }
        self.temporal
            .step(&time_energy[..self.channels], &mut self.temporal_state);
        let mut time_gain = [0.0; 32];
        self.temporal_fc
            .run(&self.temporal_state, &mut time_gain[..self.channels]);
        let groups = self.width.div_ceil(4);
        let mut frequency = [0.0; 264];
        for (direction, gru) in self.frequency.iter().enumerate() {
            let mut state = [0.0; 4];
            for step in 0..groups {
                let group = if direction == 0 {
                    step
                } else {
                    groups - 1 - step
                };
                gru.step(&frequency_energy[group * 4..group * 4 + 4], &mut state);
                frequency[group * 8 + direction * 4..group * 8 + direction * 4 + 4]
                    .copy_from_slice(&state);
            }
        }
        let mut frequency_gain = [0.0; 132];
        for group in 0..groups {
            self.frequency_fc.run(
                &frequency[group * 8..group * 8 + 8],
                &mut frequency_gain[group * 4..group * 4 + 4],
            );
        }
        for (c, row) in values.chunks_exact_mut(self.width).enumerate() {
            let time_gain = sigmoid(time_gain[c]);
            for (f, value) in row.iter_mut().enumerate() {
                *value *= time_gain * sigmoid(frequency_gain[f]);
            }
        }
    }
}

fn shuffle(input: &[f32], output: &mut [f32], channels: usize, width: usize) {
    let half = channels / 2;
    for c in 0..half {
        output[c * 2 * width..(c * 2 + 1) * width]
            .copy_from_slice(&input[c * width..(c + 1) * width]);
        output[(c * 2 + 1) * width..(c * 2 + 2) * width]
            .copy_from_slice(&input[(c + half) * width..(c + half + 1) * width]);
    }
}

struct Stage {
    parameters: weights::Stage,
    point_input: Option<Conv>,
    spatial: Conv,
    point_output: Option<Conv>,
    attention: Attention,
    point_values: Box<[f32]>,
    spatial_values: Box<[f32]>,
    shuffled: Box<[f32]>,
}

impl Stage {
    fn new(parameters: weights::Stage) -> Self {
        let spatial = Conv::new(
            parameters.spatial,
            parameters.input_width,
            parameters.output_width,
            parameters.stride,
        );
        let channels = spatial.outputs;
        let point_size = channels * parameters.input_width;
        let spatial_size = channels * parameters.output_width;
        Self {
            parameters,
            point_input: parameters
                .point_input
                .map(|p| Conv::new(p, parameters.input_width, parameters.input_width, 1)),
            spatial,
            point_output: parameters
                .point_output
                .map(|p| Conv::new(p, parameters.output_width, parameters.output_width, 1)),
            attention: Attention::new(parameters.attention, channels, parameters.output_width),
            point_values: vec![
                0.0;
                if parameters.point_input.is_some() {
                    point_size
                } else {
                    0
                }
            ]
            .into_boxed_slice(),
            spatial_values: vec![0.0; spatial_size].into_boxed_slice(),
            shuffled: vec![0.0; point_size.max(spatial_size)].into_boxed_slice(),
        }
    }

    fn reset(&mut self) {
        if let Some(conv) = &mut self.point_input {
            conv.reset();
        }
        self.spatial.reset();
        if let Some(conv) = &mut self.point_output {
            conv.reset();
        }
        self.attention.reset();
        self.point_values.fill(0.0);
        self.spatial_values.fill(0.0);
        self.shuffled.fill(0.0);
    }

    fn run(&mut self, input: &[f32], output: &mut [f32]) {
        if let Some(point) = &mut self.point_input {
            point.run(input, &mut self.point_values);
            if self.parameters.shuffle_input {
                let length = self.point_values.len();
                shuffle(
                    &self.point_values,
                    &mut self.shuffled[..length],
                    point.outputs,
                    point.output_width,
                );
                self.spatial
                    .run(&self.shuffled[..length], &mut self.spatial_values);
            } else {
                self.spatial
                    .run(&self.point_values, &mut self.spatial_values);
            }
        } else {
            self.spatial.run(input, &mut self.spatial_values);
        }
        if let Some(point) = &mut self.point_output {
            point.run(&self.spatial_values, output);
        } else {
            output.copy_from_slice(&self.spatial_values);
        }
        self.attention.run(output);
        if self.parameters.residual {
            for (value, residual) in output.iter_mut().zip(input) {
                *value += residual;
            }
        }
        if self.parameters.shuffle_output {
            shuffle(
                output,
                &mut self.shuffled[..output.len()],
                self.spatial.outputs,
                self.parameters.output_width,
            );
            output.copy_from_slice(&self.shuffled[..output.len()]);
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

pub(super) struct Network {
    kernels: Kernels,
    encoder: [Stage; 5],
    dual_path: [DpBlock; 2],
    decoder: [Stage; 5],
    encoded: [Box<[f32]>; 5],
    current: Box<[f32]>,
    next: Box<[f32]>,
    feature: [f32; 129],
    mask: [f32; 129],
    expanded: [f32; 257],
}

impl Network {
    pub(super) fn new() -> Self {
        Self {
            kernels: Kernels::new(),
            encoder: weights::ENCODER.map(Stage::new),
            dual_path: weights::DUAL_PATH.map(DpBlock::new),
            decoder: weights::DECODER.map(Stage::new),
            encoded: [12 * 65, 24 * 33, 24 * 33, 32 * 33, 16 * 33]
                .map(|n| vec![0.0; n].into_boxed_slice()),
            current: vec![0.0; 1560].into_boxed_slice(),
            next: vec![0.0; 1560].into_boxed_slice(),
            feature: [0.0; 129],
            mask: [0.0; 129],
            expanded: [0.0; 257],
        }
    }

    pub(super) fn reset(&mut self) {
        for stage in &mut self.encoder {
            stage.reset();
        }
        for block in &mut self.dual_path {
            block.state.fill(0.0);
        }
        for stage in &mut self.decoder {
            stage.reset();
        }
        for values in &mut self.encoded {
            values.fill(0.0);
        }
        self.current.fill(0.0);
        self.next.fill(0.0);
        self.feature.fill(0.0);
        self.mask.fill(0.0);
        self.expanded.fill(0.0);
    }

    pub(super) fn enhance(&mut self, spectrum: &mut [super::spectral::Complex; 257]) {
        // The trained UL-UNAS input is log10 magnitude, with no STFT scaling.
        let mut magnitude = [0.0; 257];
        for (feature, bin) in magnitude.iter_mut().zip(spectrum.iter()) {
            *feature = bin.norm().max(1e-12).log10();
        }
        self.feature[..65].copy_from_slice(&magnitude[..65]);
        self.kernels.linear(
            weights::ERB,
            &magnitude[65..],
            &[0.0; 64],
            &mut self.feature[65..],
        );
        self.encoder[0].run(&self.feature, &mut self.encoded[0]);
        for i in 1..5 {
            let (previous, next) = self.encoded.split_at_mut(i);
            self.encoder[i].run(&previous[i - 1], &mut next[0]);
        }
        self.current[..FEATURES].copy_from_slice(&self.encoded[4]);
        for block in &mut self.dual_path {
            block.run(
                (&mut self.current[..FEATURES])
                    .try_into()
                    .expect("fixed UL-UNAS bottleneck"),
            );
        }
        for i in 0..5 {
            let skip = &self.encoded[4 - i];
            for (current, residual) in self.current.iter_mut().zip(skip.iter()) {
                *current += residual;
            }
            let output_len =
                self.decoder[i].spatial.outputs * self.decoder[i].parameters.output_width;
            self.decoder[i].run(&self.current[..skip.len()], &mut self.next[..output_len]);
            self.current[..output_len].copy_from_slice(&self.next[..output_len]);
        }
        for (mask, &value) in self.mask.iter_mut().zip(self.current.iter()) {
            *mask = sigmoid(value);
        }
        self.expanded[..65].copy_from_slice(&self.mask[..65]);
        self.kernels.linear(
            weights::INVERSE_ERB,
            &self.mask[65..],
            &[0.0; 192],
            &mut self.expanded[65..],
        );
        for (bin, mask) in spectrum.iter_mut().zip(self.expanded.iter()) {
            *bin *= *mask;
        }
    }
}

impl DpBlock {
    fn new(weights: weights::DpBlock) -> Self {
        Self {
            intra: std::array::from_fn(|i| Gru::new(weights.intra[i])),
            intra_fc: Linear::new(weights.intra_fc, 16),
            intra_norm: LayerNorm::new(weights.intra_norm),
            inter: std::array::from_fn(|i| Gru::new(weights.inter[i])),
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

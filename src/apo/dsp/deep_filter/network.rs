//! Native, fixed-shape DeepFilterNet3 inference.
//!
//! Parameters are typed constants from the official exported ONNX model.
//! Matrix transposition and storage allocation happen in `Network::new`.
//! Processing and resetting use only the storage owned by the network.

use super::super::simd::Kernels;
use super::weights;

const CHANNELS: usize = 64;
const ERB_BINS: usize = 32;
const DF_BINS: usize = 96;
const EMBED: usize = 512;
const HIDDEN: usize = 256;
const DF_OUTPUT: usize = DF_BINS * 5 * 2;

#[inline]
fn sigmoid(value: f32) -> f32 {
    1.0 / (1.0 + (-value).exp())
}

#[derive(Clone, Copy)]
enum Activation {
    Relu,
    Sigmoid,
    Tanh,
    None,
}

impl Activation {
    fn apply(self, values: &mut [f32]) {
        match self {
            Self::Relu => {
                for value in values {
                    *value = value.max(0.0);
                }
            }
            Self::Sigmoid => {
                for value in values {
                    *value = sigmoid(*value);
                }
            }
            Self::Tanh => {
                for value in values {
                    *value = value.tanh();
                }
            }
            Self::None => {}
        }
    }
}

/// One frequency convolution, with causal history on the time axis.
struct Conv {
    weight: &'static [f32],
    bias: &'static [f32],
    zero_bias: Box<[f32]>,
    inputs: usize,
    outputs: usize,
    width: usize,
    output_width: usize,
    kt: usize,
    kf: usize,
    stride: usize,
    padding: usize,
    groups: usize,
    transpose: bool,
    history: Box<[f32]>,
}

impl Conv {
    fn new(parameters: weights::Conv, width: usize, output_width: usize, stride: usize) -> Self {
        let shape = parameters.shape;
        let groups = parameters.groups;
        let transpose = parameters.transpose;
        let inputs = if transpose {
            shape[0]
        } else {
            shape[1] * groups
        };
        let outputs = if transpose {
            shape[1] * groups
        } else {
            shape[0]
        };
        let kt = shape[2];
        let kf = shape[3];
        Self {
            weight: parameters.parameters.weight,
            bias: parameters.parameters.bias,
            zero_bias: vec![0.0; outputs].into_boxed_slice(),
            inputs,
            outputs,
            width,
            output_width,
            kt,
            kf,
            stride,
            padding: kf / 2,
            groups,
            transpose,
            history: vec![0.0; if kt > 1 { inputs * width * kt } else { 0 }].into_boxed_slice(),
        }
    }

    fn reset(&mut self) {
        self.history.fill(0.0);
    }

    fn process(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), self.inputs * self.width);
        debug_assert_eq!(output.len(), self.outputs * self.output_width);
        let frame = self.inputs * self.width;
        if self.kt > 1 {
            self.history.copy_within(frame.., 0);
            self.history[(self.kt - 1) * frame..].copy_from_slice(input);
        }
        let history = if self.kt > 1 {
            &self.history[..]
        } else {
            input
        };
        let bias = if self.bias.is_empty() {
            &self.zero_bias
        } else {
            self.bias
        };
        // A point convolution is a matrix product at each frequency. The
        // channel-major storage makes the small frequency loop contiguous.
        if self.kt == 1 && self.kf == 1 && self.groups == 1 {
            for (co, row) in output.chunks_exact_mut(self.output_width).enumerate() {
                row.fill(bias[co]);
                for ci in 0..self.inputs {
                    let scale = self.weight[co * self.inputs + ci];
                    let source = &input[ci * self.width..(ci + 1) * self.width];
                    for (value, source) in row.iter_mut().zip(source) {
                        *value += source * scale;
                    }
                }
            }
            return;
        }
        let in_group = self.inputs / self.groups;
        let out_group = self.outputs / self.groups;
        for co in 0..self.outputs {
            let group = co / out_group;
            for frequency in 0..self.output_width {
                let mut sum = bias[co];
                for ci_local in 0..in_group {
                    let ci = group * in_group + ci_local;
                    for time in 0..self.kt {
                        for kernel in 0..self.kf {
                            let source = if self.transpose {
                                let index =
                                    frequency as isize + self.padding as isize - kernel as isize;
                                if index < 0 || !(index as usize).is_multiple_of(self.stride) {
                                    continue;
                                }
                                index as usize / self.stride
                            } else {
                                let index = (frequency * self.stride + kernel) as isize
                                    - self.padding as isize;
                                if index < 0 {
                                    continue;
                                }
                                index as usize
                            };
                            if source >= self.width {
                                continue;
                            }
                            let index = if self.transpose {
                                ((ci * out_group + co % out_group) * self.kt + time) * self.kf
                                    + kernel
                            } else {
                                ((co * in_group + ci_local) * self.kt + time) * self.kf + kernel
                            };
                            sum += history[time * frame + ci * self.width + source]
                                * self.weight[index];
                        }
                    }
                }
                output[co * self.output_width + frequency] = sum;
            }
        }
    }
}

/// ONNX has already folded BatchNorm into each convolution's weight and bias.
struct ConvBlock {
    spatial: Conv,
    pointwise: Option<Conv>,
    temporary: Box<[f32]>,
    activation: Activation,
}

impl ConvBlock {
    fn new(
        parameters: weights::ConvBlock,
        width: usize,
        output_width: usize,
        stride: usize,
        activation: Activation,
    ) -> Self {
        let spatial = Conv::new(parameters.spatial, width, output_width, stride);
        let temporary = if parameters.pointwise.is_some() {
            vec![0.0; spatial.outputs * output_width].into_boxed_slice()
        } else {
            Box::new([])
        };
        Self {
            spatial,
            pointwise: parameters
                .pointwise
                .map(|point| Conv::new(point, output_width, output_width, 1)),
            temporary,
            activation,
        }
    }

    fn reset(&mut self) {
        self.spatial.reset();
        if let Some(pointwise) = &mut self.pointwise {
            pointwise.reset();
        }
        self.temporary.fill(0.0);
    }

    fn process(&mut self, input: &[f32], output: &mut [f32]) {
        if let Some(pointwise) = &mut self.pointwise {
            self.spatial.process(input, &mut self.temporary);
            pointwise.process(&self.temporary, output);
        } else {
            self.spatial.process(input, output);
        }
        self.activation.apply(output);
    }
}

/// PyTorch GroupedLinearEinsum stores [group, input, output]. Cache each
/// group's transpose once so the existing SIMD matrix kernel can process it.
struct GroupedLinear {
    weight: Box<[f32]>,
    bias: Box<[f32]>,
    inputs: usize,
    outputs: usize,
    groups: usize,
    activation: Activation,
    kernels: Kernels,
}

impl GroupedLinear {
    fn new(parameters: weights::GroupedLinear, activation: Activation) -> Self {
        let groups = parameters.shape[0];
        let inputs = parameters.shape[1];
        let outputs = parameters.shape[2];
        let mut weight = vec![0.0; parameters.weight.len()].into_boxed_slice();
        for group in 0..groups {
            for output in 0..outputs {
                for input in 0..inputs {
                    weight[(group * outputs + output) * inputs + input] =
                        parameters.weight[(group * inputs + input) * outputs + output];
                }
            }
        }
        Self {
            weight,
            bias: vec![0.0; groups * outputs].into_boxed_slice(),
            inputs,
            outputs,
            groups,
            activation,
            kernels: Kernels::new(),
        }
    }

    fn process(&self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), self.groups * self.inputs);
        debug_assert_eq!(output.len(), self.groups * self.outputs);
        for group in 0..self.groups {
            self.kernels.linear(
                &self.weight
                    [group * self.outputs * self.inputs..(group + 1) * self.outputs * self.inputs],
                &input[group * self.inputs..(group + 1) * self.inputs],
                &self.bias[group * self.outputs..(group + 1) * self.outputs],
                &mut output[group * self.outputs..(group + 1) * self.outputs],
            );
        }
        self.activation.apply(output);
    }
}

/// One ONNX GRU layer. ONNX gate order is update, reset, candidate; DF3
/// exports `linear_before_reset = 1`, matching PyTorch's GRU equation.
struct Gru {
    input_weight: &'static [f32],
    hidden_weight: &'static [f32],
    input_bias: &'static [f32],
    hidden_bias: &'static [f32],
    state: Box<[f32]>,
    input_gates: Box<[f32]>,
    hidden_gates: Box<[f32]>,
    kernels: Kernels,
}

impl Gru {
    fn new(parameters: weights::Gru) -> Self {
        Self {
            input_weight: parameters.weight_input,
            hidden_weight: parameters.weight_hidden,
            input_bias: parameters.bias_input,
            hidden_bias: parameters.bias_hidden,
            state: vec![0.0; HIDDEN].into_boxed_slice(),
            input_gates: vec![0.0; HIDDEN * 3].into_boxed_slice(),
            hidden_gates: vec![0.0; HIDDEN * 3].into_boxed_slice(),
            kernels: Kernels::new(),
        }
    }

    fn reset(&mut self) {
        self.state.fill(0.0);
        self.input_gates.fill(0.0);
        self.hidden_gates.fill(0.0);
    }

    fn process(&mut self, input: &[f32], output: &mut [f32]) {
        self.kernels.linear(
            self.input_weight,
            input,
            self.input_bias,
            &mut self.input_gates,
        );
        self.kernels.linear(
            self.hidden_weight,
            &self.state,
            self.hidden_bias,
            &mut self.hidden_gates,
        );
        for (index, output) in output.iter_mut().enumerate() {
            let update = sigmoid(self.input_gates[index] + self.hidden_gates[index]);
            let reset =
                sigmoid(self.input_gates[HIDDEN + index] + self.hidden_gates[HIDDEN + index]);
            let candidate = (self.input_gates[HIDDEN * 2 + index]
                + reset * self.hidden_gates[HIDDEN * 2 + index])
                .tanh();
            let value = (1.0 - update) * candidate + update * self.state[index];
            *output = value;
            self.state[index] = value;
        }
    }
}

struct SqueezedGru {
    input: GroupedLinear,
    layers: Vec<Gru>,
    output: Option<GroupedLinear>,
    a: Box<[f32]>,
    b: Box<[f32]>,
}

impl SqueezedGru {
    fn new<const L: usize>(parameters: weights::SqueezedGru<L>) -> Self {
        Self {
            input: GroupedLinear::new(parameters.input, Activation::Relu),
            layers: parameters.layers.into_iter().map(Gru::new).collect(),
            output: parameters
                .output
                .map(|output| GroupedLinear::new(output, Activation::Relu)),
            a: vec![0.0; HIDDEN].into_boxed_slice(),
            b: vec![0.0; HIDDEN].into_boxed_slice(),
        }
    }

    fn reset(&mut self) {
        for layer in &mut self.layers {
            layer.reset();
        }
        self.a.fill(0.0);
        self.b.fill(0.0);
    }

    fn process(&mut self, input: &[f32], output: &mut [f32]) {
        self.input.process(input, &mut self.a);
        for (index, layer) in self.layers.iter_mut().enumerate() {
            if index.is_multiple_of(2) {
                layer.process(&self.a, &mut self.b);
            } else {
                layer.process(&self.b, &mut self.a);
            }
        }
        let result = if self.layers.len().is_multiple_of(2) {
            &self.a
        } else {
            &self.b
        };
        if let Some(projection) = &self.output {
            projection.process(result, output);
        } else {
            output.copy_from_slice(result);
        }
    }
}

fn channel_to_frequency(input: &[f32], channels: usize, output: &mut [f32]) {
    let width = input.len() / channels;
    for frequency in 0..width {
        for channel in 0..channels {
            output[frequency * channels + channel] = input[channel * width + frequency];
        }
    }
}

fn frequency_to_channel(input: &[f32], channels: usize, output: &mut [f32]) {
    let width = input.len() / channels;
    for frequency in 0..width {
        for channel in 0..channels {
            output[channel * width + frequency] = input[frequency * channels + channel];
        }
    }
}

fn add(left: &[f32], right: &[f32], output: &mut [f32]) {
    for ((left, right), output) in left.iter().zip(right).zip(output) {
        *output = left + right;
    }
}

pub(super) struct Network {
    erb_encoder: [ConvBlock; 4],
    spec_encoder: [ConvBlock; 2],
    spec_projection: GroupedLinear,
    encoder_gru: SqueezedGru,
    erb_gru: SqueezedGru,
    erb_path: [ConvBlock; 4],
    erb_decoder: [ConvBlock; 4],
    df_gru: SqueezedGru,
    df_skip: GroupedLinear,
    df_output: GroupedLinear,
    df_path: ConvBlock,
    lsnr_weight: &'static [f32],
    lsnr_bias: &'static [f32],
    lsnr_scale: f32,
    lsnr_offset: f32,
    kernels: Kernels,
    e0: Box<[f32]>,
    e1: Box<[f32]>,
    e2: Box<[f32]>,
    e3: Box<[f32]>,
    c0: Box<[f32]>,
    c1: Box<[f32]>,
    spec_flat: Box<[f32]>,
    spec_embed: Box<[f32]>,
    erb_flat: Box<[f32]>,
    combined_embed: Box<[f32]>,
    embed: Box<[f32]>,
    decoder_embed: Box<[f32]>,
    decoder_channels: Box<[f32]>,
    path: [Box<[f32]>; 4],
    sums: [Box<[f32]>; 4],
    decoded: [Box<[f32]>; 3],
    df_recurrent: Box<[f32]>,
    df_skipped: Box<[f32]>,
    df_combined: Box<[f32]>,
    df_linear: Box<[f32]>,
    df_pathway: Box<[f32]>,
    df_pathway_flat: Box<[f32]>,
}

impl Network {
    pub(super) fn new() -> Self {
        let relu = Activation::Relu;
        Self {
            erb_encoder: std::array::from_fn(|i| {
                ConvBlock::new(
                    weights::ENCODER.erb[i],
                    [32, 32, 16, 8][i],
                    [32, 16, 8, 8][i],
                    [1, 2, 2, 1][i],
                    relu,
                )
            }),
            spec_encoder: std::array::from_fn(|i| {
                ConvBlock::new(
                    weights::ENCODER.spectrum[i],
                    96,
                    [96, 48][i],
                    [1, 2][i],
                    relu,
                )
            }),
            spec_projection: GroupedLinear::new(weights::ENCODER.spectrum_projection, relu),
            encoder_gru: SqueezedGru::new(weights::ENCODER.gru),
            erb_gru: SqueezedGru::new(weights::ERB_DECODER.gru),
            erb_path: std::array::from_fn(|i| {
                ConvBlock::new(
                    weights::ERB_DECODER.pathway[i],
                    [8, 8, 16, 32][i],
                    [8, 8, 16, 32][i],
                    1,
                    relu,
                )
            }),
            erb_decoder: std::array::from_fn(|i| {
                ConvBlock::new(
                    weights::ERB_DECODER.decoder[i],
                    [8, 8, 16, 32][i],
                    [8, 16, 32, 32][i],
                    [1, 2, 2, 1][i],
                    if i == 3 { Activation::Sigmoid } else { relu },
                )
            }),
            df_gru: SqueezedGru::new(weights::DF_DECODER.gru),
            df_skip: GroupedLinear::new(weights::DF_DECODER.skip, Activation::None),
            df_output: GroupedLinear::new(weights::DF_DECODER.output, Activation::Tanh),
            df_path: ConvBlock::new(weights::DF_DECODER.pathway, 96, 96, 1, relu),
            lsnr_weight: weights::ENCODER.lsnr.weight,
            lsnr_bias: weights::ENCODER.lsnr.bias,
            lsnr_scale: weights::ENCODER.lsnr_scale,
            lsnr_offset: weights::ENCODER.lsnr_offset,
            kernels: Kernels::new(),
            e0: vec![0.0; CHANNELS * 32].into_boxed_slice(),
            e1: vec![0.0; CHANNELS * 16].into_boxed_slice(),
            e2: vec![0.0; CHANNELS * 8].into_boxed_slice(),
            e3: vec![0.0; CHANNELS * 8].into_boxed_slice(),
            c0: vec![0.0; CHANNELS * 96].into_boxed_slice(),
            c1: vec![0.0; CHANNELS * 48].into_boxed_slice(),
            spec_flat: vec![0.0; CHANNELS * 48].into_boxed_slice(),
            spec_embed: vec![0.0; EMBED].into_boxed_slice(),
            erb_flat: vec![0.0; EMBED].into_boxed_slice(),
            combined_embed: vec![0.0; EMBED].into_boxed_slice(),
            embed: vec![0.0; EMBED].into_boxed_slice(),
            decoder_embed: vec![0.0; EMBED].into_boxed_slice(),
            decoder_channels: vec![0.0; EMBED].into_boxed_slice(),
            path: [8, 8, 16, 32].map(|width| vec![0.0; CHANNELS * width].into_boxed_slice()),
            sums: [8, 8, 16, 32].map(|width| vec![0.0; CHANNELS * width].into_boxed_slice()),
            decoded: [8, 16, 32].map(|width| vec![0.0; CHANNELS * width].into_boxed_slice()),
            df_recurrent: vec![0.0; HIDDEN].into_boxed_slice(),
            df_skipped: vec![0.0; HIDDEN].into_boxed_slice(),
            df_combined: vec![0.0; HIDDEN].into_boxed_slice(),
            df_linear: vec![0.0; DF_OUTPUT].into_boxed_slice(),
            df_pathway: vec![0.0; DF_OUTPUT].into_boxed_slice(),
            df_pathway_flat: vec![0.0; DF_OUTPUT].into_boxed_slice(),
        }
    }

    pub(super) fn reset(&mut self) {
        for block in &mut self.erb_encoder {
            block.reset();
        }
        for block in &mut self.spec_encoder {
            block.reset();
        }
        self.encoder_gru.reset();
        self.erb_gru.reset();
        for block in &mut self.erb_path {
            block.reset();
        }
        for block in &mut self.erb_decoder {
            block.reset();
        }
        self.df_gru.reset();
        self.df_path.reset();
        for values in [
            &mut self.e0,
            &mut self.e1,
            &mut self.e2,
            &mut self.e3,
            &mut self.c0,
            &mut self.c1,
            &mut self.spec_flat,
            &mut self.spec_embed,
            &mut self.erb_flat,
            &mut self.combined_embed,
            &mut self.embed,
            &mut self.decoder_embed,
            &mut self.decoder_channels,
            &mut self.df_recurrent,
            &mut self.df_skipped,
            &mut self.df_combined,
            &mut self.df_linear,
            &mut self.df_pathway,
            &mut self.df_pathway_flat,
        ] {
            values.fill(0.0);
        }
        for values in &mut self.path {
            values.fill(0.0);
        }
        for values in &mut self.sums {
            values.fill(0.0);
        }
        for values in &mut self.decoded {
            values.fill(0.0);
        }
    }

    /// `spec` is channel-major real/imaginary input. Coefficients are
    /// frequency-major [96][5][real, imaginary], as in the ONNX DF decoder.
    /// The default libDF SNR gates stop decoder states when a stage is skipped.
    pub(super) fn process(
        &mut self,
        erb: &[f32; ERB_BINS],
        spec: &[f32; DF_BINS * 2],
        gains: &mut [f32; ERB_BINS],
        coefficients: &mut [f32; DF_OUTPUT],
    ) -> f32 {
        self.erb_encoder[0].process(erb, &mut self.e0);
        self.erb_encoder[1].process(&self.e0, &mut self.e1);
        self.erb_encoder[2].process(&self.e1, &mut self.e2);
        self.erb_encoder[3].process(&self.e2, &mut self.e3);
        self.spec_encoder[0].process(spec, &mut self.c0);
        self.spec_encoder[1].process(&self.c0, &mut self.c1);
        channel_to_frequency(&self.c1, CHANNELS, &mut self.spec_flat);
        self.spec_projection
            .process(&self.spec_flat, &mut self.spec_embed);
        channel_to_frequency(&self.e3, CHANNELS, &mut self.erb_flat);
        add(&self.erb_flat, &self.spec_embed, &mut self.combined_embed);
        self.encoder_gru
            .process(&self.combined_embed, &mut self.embed);
        let mut lsnr = [0.0];
        self.kernels
            .linear(self.lsnr_weight, &self.embed, self.lsnr_bias, &mut lsnr);
        let lsnr = sigmoid(lsnr[0]) * self.lsnr_scale + self.lsnr_offset;

        if lsnr < -10.0 {
            gains.fill(0.0);
        } else if lsnr > 30.0 {
            gains.fill(1.0);
        } else {
            self.erb_gru.process(&self.embed, &mut self.decoder_embed);
            frequency_to_channel(&self.decoder_embed, CHANNELS, &mut self.decoder_channels);
            self.erb_path[0].process(&self.e3, &mut self.path[0]);
            add(&self.path[0], &self.decoder_channels, &mut self.sums[0]);
            self.erb_decoder[0].process(&self.sums[0], &mut self.decoded[0]);
            self.erb_path[1].process(&self.e2, &mut self.path[1]);
            add(&self.path[1], &self.decoded[0], &mut self.sums[1]);
            self.erb_decoder[1].process(&self.sums[1], &mut self.decoded[1]);
            self.erb_path[2].process(&self.e1, &mut self.path[2]);
            add(&self.path[2], &self.decoded[1], &mut self.sums[2]);
            self.erb_decoder[2].process(&self.sums[2], &mut self.decoded[2]);
            self.erb_path[3].process(&self.e0, &mut self.path[3]);
            add(&self.path[3], &self.decoded[2], &mut self.sums[3]);
            self.erb_decoder[3].process(&self.sums[3], gains);
        }

        if (-10.0..=20.0).contains(&lsnr) {
            self.df_gru.process(&self.embed, &mut self.df_recurrent);
            self.df_skip.process(&self.embed, &mut self.df_skipped);
            add(&self.df_recurrent, &self.df_skipped, &mut self.df_combined);
            self.df_output
                .process(&self.df_combined, &mut self.df_linear);
            self.df_path.process(&self.c0, &mut self.df_pathway);
            channel_to_frequency(&self.df_pathway, 10, &mut self.df_pathway_flat);
            add(&self.df_linear, &self.df_pathway_flat, coefficients);
        }
        lsnr
    }
}

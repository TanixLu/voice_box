// Official DF3 tensor offsets; provenance: assets/deepfilternet3/README.md.
const BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/deepfilternet3/df3.f32"
));

#[allow(long_running_const_eval)]
static DATA: [f32; 2134646] = decode();

const fn decode() -> [f32; 2134646] {
    let mut out = [0.0; 2134646];
    let mut i = 0;
    while i < out.len() {
        let j = i * 4;
        out[i] = f32::from_le_bytes([BYTES[j], BYTES[j + 1], BYTES[j + 2], BYTES[j + 3]]);
        i += 1;
    }
    out
}

// Every slice and parameter shape is checked during constant evaluation.
const fn slice(start: usize, len: usize) -> &'static [f32] {
    assert!(start <= 2134646 && len <= 2134646 - start);
    DATA.split_at(start + len).0.split_at(start).1
}

#[derive(Clone, Copy)]
pub(super) struct Pair {
    pub weight: &'static [f32],
    pub bias: &'static [f32],
}
#[derive(Clone, Copy)]
pub(super) struct Conv {
    pub parameters: Pair,
    pub shape: [usize; 4],
    pub groups: usize,
    pub transpose: bool,
}
#[derive(Clone, Copy)]
pub(super) struct ConvBlock {
    pub spatial: Conv,
    pub pointwise: Option<Conv>,
}
#[derive(Clone, Copy)]
pub(super) struct GroupedLinear {
    pub weight: &'static [f32],
    pub shape: [usize; 3],
}
#[derive(Clone, Copy)]
pub(super) struct Gru {
    pub weight_input: &'static [f32],
    pub weight_hidden: &'static [f32],
    pub bias_input: &'static [f32],
    pub bias_hidden: &'static [f32],
}
#[derive(Clone, Copy)]
pub(super) struct SqueezedGru<const L: usize> {
    pub input: GroupedLinear,
    pub layers: [Gru; L],
    pub output: Option<GroupedLinear>,
}
#[derive(Clone, Copy)]
pub(super) struct Encoder {
    pub erb: [ConvBlock; 4],
    pub spectrum: [ConvBlock; 2],
    pub spectrum_projection: GroupedLinear,
    pub gru: SqueezedGru<1>,
    pub lsnr: Pair,
    pub lsnr_scale: f32,
    pub lsnr_offset: f32,
}
#[derive(Clone, Copy)]
pub(super) struct ErbDecoder {
    pub gru: SqueezedGru<2>,
    pub pathway: [ConvBlock; 4],
    pub decoder: [ConvBlock; 4],
}
#[derive(Clone, Copy)]
pub(super) struct DfDecoder {
    pub gru: SqueezedGru<2>,
    pub skip: GroupedLinear,
    pub output: GroupedLinear,
    pub pathway: ConvBlock,
}

const fn conv(parameters: Pair, shape: [usize; 4], groups: usize, transpose: bool) -> Conv {
    assert!(groups > 0 && shape[0] > 0 && shape[1] > 0 && shape[2] > 0 && shape[3] > 0);
    assert!(parameters.weight.len() == shape[0] * shape[1] * shape[2] * shape[3]);
    assert!(shape[0].is_multiple_of(groups));
    assert!(!transpose || shape[2] == 1);
    let outputs = if transpose {
        shape[1] * groups
    } else {
        shape[0]
    };
    assert!(parameters.bias.is_empty() || parameters.bias.len() == outputs);
    Conv {
        parameters,
        shape,
        groups,
        transpose,
    }
}
const fn block(spatial: Conv, pointwise: Option<Conv>) -> ConvBlock {
    if let Some(point) = pointwise {
        let channels = if spatial.transpose {
            spatial.shape[1] * spatial.groups
        } else {
            spatial.shape[0]
        };
        assert!(point.shape[0] == channels && point.shape[1] == channels);
        assert!(
            point.shape[2] == 1 && point.shape[3] == 1 && point.groups == 1 && !point.transpose
        );
    }
    ConvBlock { spatial, pointwise }
}
const fn linear(weight: &'static [f32], shape: [usize; 3]) -> GroupedLinear {
    assert!(shape[0] > 0 && shape[1] > 0 && shape[2] > 0);
    assert!(weight.len() == shape[0] * shape[1] * shape[2]);
    GroupedLinear { weight, shape }
}
const fn gru(
    weight_input: &'static [f32],
    weight_hidden: &'static [f32],
    bias: &'static [f32],
) -> Gru {
    assert!(weight_input.len() == 3 * 256 * 256 && weight_hidden.len() == 3 * 256 * 256);
    assert!(bias.len() == 6 * 256);
    let (bias_input, bias_hidden) = bias.split_at(3 * 256);
    Gru {
        weight_input,
        weight_hidden,
        bias_input,
        bias_hidden,
    }
}

pub(super) const ENCODER: Encoder = Encoder {
    erb: [
        block(
            conv(
                Pair {
                    weight: slice(66881, 576),
                    bias: slice(67457, 64),
                },
                [64, 1, 3, 3],
                1,
                false,
            ),
            None,
        ),
        block(
            conv(
                Pair {
                    weight: slice(0, 192),
                    bias: &[],
                },
                [64, 1, 1, 3],
                64,
                false,
            ),
            Some(conv(
                Pair {
                    weight: slice(67521, 4096),
                    bias: slice(71617, 64),
                },
                [64, 64, 1, 1],
                1,
                false,
            )),
        ),
        block(
            conv(
                Pair {
                    weight: slice(192, 192),
                    bias: &[],
                },
                [64, 1, 1, 3],
                64,
                false,
            ),
            Some(conv(
                Pair {
                    weight: slice(71681, 4096),
                    bias: slice(75777, 64),
                },
                [64, 64, 1, 1],
                1,
                false,
            )),
        ),
        block(
            conv(
                Pair {
                    weight: slice(384, 192),
                    bias: &[],
                },
                [64, 1, 1, 3],
                64,
                false,
            ),
            Some(conv(
                Pair {
                    weight: slice(75841, 4096),
                    bias: slice(79937, 64),
                },
                [64, 64, 1, 1],
                1,
                false,
            )),
        ),
    ],
    spectrum: [
        block(
            conv(
                Pair {
                    weight: slice(576, 576),
                    bias: &[],
                },
                [64, 1, 3, 3],
                2,
                false,
            ),
            Some(conv(
                Pair {
                    weight: slice(80001, 4096),
                    bias: slice(84097, 64),
                },
                [64, 64, 1, 1],
                1,
                false,
            )),
        ),
        block(
            conv(
                Pair {
                    weight: slice(1152, 192),
                    bias: &[],
                },
                [64, 1, 1, 3],
                64,
                false,
            ),
            Some(conv(
                Pair {
                    weight: slice(84161, 4096),
                    bias: slice(88257, 64),
                },
                [64, 64, 1, 1],
                1,
                false,
            )),
        ),
    ],
    spectrum_projection: linear(slice(1344, 49152), [32, 96, 16]),
    gru: SqueezedGru {
        input: linear(slice(50496, 8192), [16, 32, 16]),
        layers: [gru(
            slice(88835, 196608),
            slice(285443, 196608),
            slice(482051, 1536),
        )],
        output: Some(linear(slice(58688, 8192), [16, 16, 32])),
    },
    lsnr: Pair {
        weight: slice(88321, 512),
        bias: slice(66880, 1),
    },
    lsnr_scale: slice(483587, 1)[0],
    lsnr_offset: slice(483588, 1)[0],
};

pub(super) const ERB_DECODER: ErbDecoder = ErbDecoder {
    gru: SqueezedGru {
        input: linear(slice(483589, 8192), [16, 32, 16]),
        layers: [
            gru(
                slice(513734, 196608),
                slice(710342, 196608),
                slice(906950, 1536),
            ),
            gru(
                slice(908486, 196608),
                slice(1105094, 196608),
                slice(1301702, 1536),
            ),
        ],
        output: Some(linear(slice(491781, 8192), [16, 16, 32])),
    },
    pathway: [
        block(
            conv(
                Pair {
                    weight: slice(500549, 64),
                    bias: slice(500613, 64),
                },
                [64, 1, 1, 1],
                64,
                false,
            ),
            None,
        ),
        block(
            conv(
                Pair {
                    weight: slice(504837, 64),
                    bias: slice(504901, 64),
                },
                [64, 1, 1, 1],
                64,
                false,
            ),
            None,
        ),
        block(
            conv(
                Pair {
                    weight: slice(509125, 64),
                    bias: slice(509189, 64),
                },
                [64, 1, 1, 1],
                64,
                false,
            ),
            None,
        ),
        block(
            conv(
                Pair {
                    weight: slice(513413, 64),
                    bias: slice(513477, 64),
                },
                [64, 1, 1, 1],
                64,
                false,
            ),
            None,
        ),
    ],
    decoder: [
        block(
            conv(
                Pair {
                    weight: slice(499973, 192),
                    bias: &[],
                },
                [64, 1, 1, 3],
                64,
                false,
            ),
            Some(conv(
                Pair {
                    weight: slice(500677, 4096),
                    bias: slice(504773, 64),
                },
                [64, 64, 1, 1],
                1,
                false,
            )),
        ),
        block(
            conv(
                Pair {
                    weight: slice(500165, 192),
                    bias: &[],
                },
                [64, 1, 1, 3],
                64,
                true,
            ),
            Some(conv(
                Pair {
                    weight: slice(504965, 4096),
                    bias: slice(509061, 64),
                },
                [64, 64, 1, 1],
                1,
                false,
            )),
        ),
        block(
            conv(
                Pair {
                    weight: slice(500357, 192),
                    bias: &[],
                },
                [64, 1, 1, 3],
                64,
                true,
            ),
            Some(conv(
                Pair {
                    weight: slice(509253, 4096),
                    bias: slice(513349, 64),
                },
                [64, 64, 1, 1],
                1,
                false,
            )),
        ),
        block(
            conv(
                Pair {
                    weight: slice(513541, 192),
                    bias: slice(513733, 1),
                },
                [1, 64, 1, 3],
                1,
                false,
            ),
            None,
        ),
    ],
};

pub(super) const DF_DECODER: DfDecoder = DfDecoder {
    gru: SqueezedGru {
        input: linear(slice(1304838, 16384), [8, 64, 32]),
        layers: [
            gru(
                slice(1344885, 196608),
                slice(1541493, 196608),
                slice(1738101, 1536),
            ),
            gru(
                slice(1739637, 196608),
                slice(1936245, 196608),
                slice(2132853, 1536),
            ),
        ],
        output: None,
    },
    skip: linear(slice(1321222, 8192), [16, 32, 16]),
    output: linear(slice(1329414, 15360), [16, 16, 60]),
    pathway: block(
        conv(
            Pair {
                weight: slice(1303238, 1600),
                bias: &[],
            },
            [10, 32, 5, 1],
            2,
            false,
        ),
        Some(conv(
            Pair {
                weight: slice(1344775, 100),
                bias: slice(1344875, 10),
            },
            [10, 10, 1, 1],
            1,
            false,
        )),
    ),
};

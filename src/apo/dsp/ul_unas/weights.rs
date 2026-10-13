// Official UL-UNAS DNS3 tensor offsets; provenance: assets/ul_unas/README.md.
const BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/ul_unas/dns3.f32"
));
#[allow(long_running_const_eval)]
static DATA: [f32; 172290] = decode();
const fn decode() -> [f32; 172290] {
    let mut out = [0.0; 172290];
    let mut i = 0;
    while i < out.len() {
        let j = i * 4;
        out[i] = f32::from_le_bytes([BYTES[j], BYTES[j + 1], BYTES[j + 2], BYTES[j + 3]]);
        i += 1;
    }
    out
}
const fn slice(start: usize, len: usize) -> &'static [f32] {
    assert!(start <= 172290 && len <= 172290 - start);
    DATA.split_at(start + len).0.split_at(start).1
}

#[derive(Clone, Copy)]
pub(super) struct Pair {
    pub weight: &'static [f32],
    pub bias: &'static [f32],
}
#[derive(Clone, Copy)]
pub(super) struct Norm {
    pub parameters: Pair,
    pub mean: &'static [f32],
    pub variance: &'static [f32],
}
#[derive(Clone, Copy)]
pub(super) struct Affine {
    pub weight: &'static [f32],
    pub bias: &'static [f32],
    pub slope: &'static [f32],
    pub width: usize,
}
#[derive(Clone, Copy)]
pub(super) struct Conv {
    pub parameters: Pair,
    pub norm: Norm,
    pub affine: Option<Affine>,
    pub shape: [usize; 4],
    pub groups: usize,
    pub transpose: bool,
}
#[derive(Clone, Copy)]
pub(super) struct Gru {
    pub weight_input: &'static [f32],
    pub weight_hidden: &'static [f32],
    pub bias_input: &'static [f32],
    pub bias_hidden: &'static [f32],
    pub inputs: usize,
    pub hidden: usize,
}
#[derive(Clone, Copy)]
pub(super) struct Attention {
    pub temporal: Gru,
    pub temporal_fc: Pair,
    pub frequency: [Gru; 2],
    pub frequency_fc: Pair,
}
#[derive(Clone, Copy)]
pub(super) struct Stage {
    pub point_input: Option<Conv>,
    pub spatial: Conv,
    pub point_output: Option<Conv>,
    pub attention: Attention,
    pub input_width: usize,
    pub output_width: usize,
    pub stride: usize,
    pub shuffle_input: bool,
    pub shuffle_output: bool,
    pub residual: bool,
}
#[derive(Clone, Copy)]
pub(super) struct DpBlock {
    pub intra: [Gru; 4],
    pub intra_fc: Pair,
    pub intra_norm: Pair,
    pub inter: [Gru; 2],
    pub inter_fc: Pair,
    pub inter_norm: Pair,
}

const fn pair(weight: &'static [f32], bias: &'static [f32]) -> Pair {
    Pair { weight, bias }
}
const fn norm(parameters: Pair, mean: &'static [f32], variance: &'static [f32]) -> Norm {
    assert!(
        parameters.weight.len() == parameters.bias.len()
            && mean.len() == variance.len()
            && mean.len() == parameters.weight.len()
    );
    Norm {
        parameters,
        mean,
        variance,
    }
}
const fn affine(
    weight: &'static [f32],
    bias: &'static [f32],
    slope: &'static [f32],
    width: usize,
) -> Affine {
    assert!(weight.len() == bias.len() && weight.len() == slope.len() * width);
    Affine {
        weight,
        bias,
        slope,
        width,
    }
}
const fn conv(
    parameters: Pair,
    norm: Norm,
    affine: Option<Affine>,
    shape: [usize; 4],
    groups: usize,
    transpose: bool,
) -> Conv {
    assert!(groups > 0 && shape[0] > 0 && shape[1] > 0 && shape[2] > 0 && shape[3] > 0);
    assert!(
        parameters.weight.len() == shape[0] * shape[1] * shape[2] * shape[3]
            && shape[0].is_multiple_of(groups)
    );
    let outputs = if transpose {
        shape[1] * groups
    } else {
        shape[0]
    };
    assert!(parameters.bias.len() == outputs && norm.mean.len() == outputs);
    if let Some(affine) = affine {
        assert!(affine.slope.len() == outputs);
    }
    Conv {
        parameters,
        norm,
        affine,
        shape,
        groups,
        transpose,
    }
}
const fn gru(
    weight_input: &'static [f32],
    weight_hidden: &'static [f32],
    bias_input: &'static [f32],
    bias_hidden: &'static [f32],
    inputs: usize,
    hidden: usize,
) -> Gru {
    assert!(inputs > 0 && inputs <= 32 && hidden > 0 && hidden <= 64);
    assert!(
        weight_input.len() == 3 * hidden * inputs && weight_hidden.len() == 3 * hidden * hidden
    );
    assert!(bias_input.len() == 3 * hidden && bias_hidden.len() == 3 * hidden);
    Gru {
        weight_input,
        weight_hidden,
        bias_input,
        bias_hidden,
        inputs,
        hidden,
    }
}

pub(super) const ERB: &[f32] = slice(0, 12288);
pub(super) const INVERSE_ERB: &[f32] = slice(12288, 12288);

pub(super) const ENCODER: [Stage; 5] = [
    Stage {
        point_input: None,
        spatial: conv(
            pair(slice(24576, 108), slice(24684, 12)),
            norm(
                pair(slice(24696, 12), slice(24708, 12)),
                slice(24720, 12),
                slice(24732, 12),
            ),
            Some(affine(
                slice(24744, 780),
                slice(25524, 780),
                slice(26304, 12),
                65,
            )),
            [12, 1, 3, 3],
            1,
            false,
        ),
        point_output: None,
        attention: Attention {
            temporal: gru(
                slice(26316, 864),
                slice(27180, 1728),
                slice(28908, 72),
                slice(28980, 72),
                12,
                24,
            ),
            temporal_fc: pair(slice(29052, 288), slice(29340, 12)),
            frequency: [
                gru(
                    slice(29352, 48),
                    slice(29400, 48),
                    slice(29448, 12),
                    slice(29460, 12),
                    4,
                    4,
                ),
                gru(
                    slice(29472, 48),
                    slice(29520, 48),
                    slice(29568, 12),
                    slice(29580, 12),
                    4,
                    4,
                ),
            ],
            frequency_fc: pair(slice(29592, 32), slice(29624, 4)),
        },
        input_width: 129,
        output_width: 65,
        stride: 2,
        shuffle_input: false,
        shuffle_output: false,
        residual: false,
    },
    Stage {
        point_input: Some(conv(
            pair(slice(29628, 144), slice(29772, 24)),
            norm(
                pair(slice(29796, 24), slice(29820, 24)),
                slice(29844, 24),
                slice(29868, 24),
            ),
            Some(affine(
                slice(29892, 1560),
                slice(31452, 1560),
                slice(33012, 24),
                65,
            )),
            [24, 6, 1, 1],
            2,
            false,
        )),
        spatial: conv(
            pair(slice(33036, 144), slice(33180, 24)),
            norm(
                pair(slice(33204, 24), slice(33228, 24)),
                slice(33252, 24),
                slice(33276, 24),
            ),
            Some(affine(
                slice(33300, 792),
                slice(34092, 792),
                slice(34884, 24),
                33,
            )),
            [24, 1, 2, 3],
            24,
            false,
        ),
        point_output: Some(conv(
            pair(slice(34908, 288), slice(35196, 24)),
            norm(
                pair(slice(35220, 24), slice(35244, 24)),
                slice(35268, 24),
                slice(35292, 24),
            ),
            None,
            [24, 12, 1, 1],
            2,
            false,
        )),
        attention: Attention {
            temporal: gru(
                slice(35316, 3456),
                slice(38772, 6912),
                slice(45684, 144),
                slice(45828, 144),
                24,
                48,
            ),
            temporal_fc: pair(slice(45972, 1152), slice(47124, 24)),
            frequency: [
                gru(
                    slice(47148, 48),
                    slice(47196, 48),
                    slice(47244, 12),
                    slice(47256, 12),
                    4,
                    4,
                ),
                gru(
                    slice(47268, 48),
                    slice(47316, 48),
                    slice(47364, 12),
                    slice(47376, 12),
                    4,
                    4,
                ),
            ],
            frequency_fc: pair(slice(47388, 32), slice(47420, 4)),
        },
        input_width: 65,
        output_width: 33,
        stride: 2,
        shuffle_input: true,
        shuffle_output: true,
        residual: false,
    },
    Stage {
        point_input: Some(conv(
            pair(slice(47424, 288), slice(47712, 24)),
            norm(
                pair(slice(47736, 24), slice(47760, 24)),
                slice(47784, 24),
                slice(47808, 24),
            ),
            Some(affine(
                slice(47832, 792),
                slice(48624, 792),
                slice(49416, 24),
                33,
            )),
            [24, 12, 1, 1],
            2,
            false,
        )),
        spatial: conv(
            pair(slice(49440, 144), slice(49584, 24)),
            norm(
                pair(slice(49608, 24), slice(49632, 24)),
                slice(49656, 24),
                slice(49680, 24),
            ),
            Some(affine(
                slice(49704, 792),
                slice(50496, 792),
                slice(51288, 24),
                33,
            )),
            [24, 1, 2, 3],
            24,
            false,
        ),
        point_output: None,
        attention: Attention {
            temporal: gru(
                slice(51312, 3456),
                slice(54768, 6912),
                slice(61680, 144),
                slice(61824, 144),
                24,
                48,
            ),
            temporal_fc: pair(slice(61968, 1152), slice(63120, 24)),
            frequency: [
                gru(
                    slice(63144, 48),
                    slice(63192, 48),
                    slice(63240, 12),
                    slice(63252, 12),
                    4,
                    4,
                ),
                gru(
                    slice(63264, 48),
                    slice(63312, 48),
                    slice(63360, 12),
                    slice(63372, 12),
                    4,
                    4,
                ),
            ],
            frequency_fc: pair(slice(63384, 32), slice(63416, 4)),
        },
        input_width: 33,
        output_width: 33,
        stride: 1,
        shuffle_input: true,
        shuffle_output: false,
        residual: false,
    },
    Stage {
        point_input: Some(conv(
            pair(slice(63420, 384), slice(63804, 32)),
            norm(
                pair(slice(63836, 32), slice(63868, 32)),
                slice(63900, 32),
                slice(63932, 32),
            ),
            Some(affine(
                slice(63964, 1056),
                slice(65020, 1056),
                slice(66076, 32),
                33,
            )),
            [32, 12, 1, 1],
            2,
            false,
        )),
        spatial: conv(
            pair(slice(66108, 160), slice(66268, 32)),
            norm(
                pair(slice(66300, 32), slice(66332, 32)),
                slice(66364, 32),
                slice(66396, 32),
            ),
            Some(affine(
                slice(66428, 1056),
                slice(67484, 1056),
                slice(68540, 32),
                33,
            )),
            [32, 1, 1, 5],
            32,
            false,
        ),
        point_output: Some(conv(
            pair(slice(68572, 512), slice(69084, 32)),
            norm(
                pair(slice(69116, 32), slice(69148, 32)),
                slice(69180, 32),
                slice(69212, 32),
            ),
            None,
            [32, 16, 1, 1],
            2,
            false,
        )),
        attention: Attention {
            temporal: gru(
                slice(69244, 6144),
                slice(75388, 12288),
                slice(87676, 192),
                slice(87868, 192),
                32,
                64,
            ),
            temporal_fc: pair(slice(88060, 2048), slice(90108, 32)),
            frequency: [
                gru(
                    slice(90140, 48),
                    slice(90188, 48),
                    slice(90236, 12),
                    slice(90248, 12),
                    4,
                    4,
                ),
                gru(
                    slice(90260, 48),
                    slice(90308, 48),
                    slice(90356, 12),
                    slice(90368, 12),
                    4,
                    4,
                ),
            ],
            frequency_fc: pair(slice(90380, 32), slice(90412, 4)),
        },
        input_width: 33,
        output_width: 33,
        stride: 1,
        shuffle_input: true,
        shuffle_output: true,
        residual: false,
    },
    Stage {
        point_input: Some(conv(
            pair(slice(90416, 256), slice(90672, 16)),
            norm(
                pair(slice(90688, 16), slice(90704, 16)),
                slice(90720, 16),
                slice(90736, 16),
            ),
            Some(affine(
                slice(90752, 528),
                slice(91280, 528),
                slice(91808, 16),
                33,
            )),
            [16, 16, 1, 1],
            2,
            false,
        )),
        spatial: conv(
            pair(slice(91824, 80), slice(91904, 16)),
            norm(
                pair(slice(91920, 16), slice(91936, 16)),
                slice(91952, 16),
                slice(91968, 16),
            ),
            Some(affine(
                slice(91984, 528),
                slice(92512, 528),
                slice(93040, 16),
                33,
            )),
            [16, 1, 1, 5],
            16,
            false,
        ),
        point_output: None,
        attention: Attention {
            temporal: gru(
                slice(93056, 1536),
                slice(94592, 3072),
                slice(97664, 96),
                slice(97760, 96),
                16,
                32,
            ),
            temporal_fc: pair(slice(97856, 512), slice(98368, 16)),
            frequency: [
                gru(
                    slice(98384, 48),
                    slice(98432, 48),
                    slice(98480, 12),
                    slice(98492, 12),
                    4,
                    4,
                ),
                gru(
                    slice(98504, 48),
                    slice(98552, 48),
                    slice(98600, 12),
                    slice(98612, 12),
                    4,
                    4,
                ),
            ],
            frequency_fc: pair(slice(98624, 32), slice(98656, 4)),
        },
        input_width: 33,
        output_width: 33,
        stride: 1,
        shuffle_input: true,
        shuffle_output: false,
        residual: false,
    },
];
pub(super) const DECODER: [Stage; 5] = [
    Stage {
        point_input: Some(conv(
            pair(slice(107044, 256), slice(107300, 32)),
            norm(
                pair(slice(107332, 32), slice(107364, 32)),
                slice(107396, 32),
                slice(107428, 32),
            ),
            Some(affine(
                slice(107460, 1056),
                slice(108516, 1056),
                slice(109572, 32),
                33,
            )),
            [32, 8, 1, 1],
            2,
            false,
        )),
        spatial: conv(
            pair(slice(109604, 160), slice(109764, 32)),
            norm(
                pair(slice(109796, 32), slice(109828, 32)),
                slice(109860, 32),
                slice(109892, 32),
            ),
            Some(affine(
                slice(109924, 1056),
                slice(110980, 1056),
                slice(112036, 32),
                33,
            )),
            [32, 1, 1, 5],
            32,
            true,
        ),
        point_output: None,
        attention: Attention {
            temporal: gru(
                slice(112068, 6144),
                slice(118212, 12288),
                slice(130500, 192),
                slice(130692, 192),
                32,
                64,
            ),
            temporal_fc: pair(slice(130884, 2048), slice(132932, 32)),
            frequency: [
                gru(
                    slice(132964, 48),
                    slice(133012, 48),
                    slice(133060, 12),
                    slice(133072, 12),
                    4,
                    4,
                ),
                gru(
                    slice(133084, 48),
                    slice(133132, 48),
                    slice(133180, 12),
                    slice(133192, 12),
                    4,
                    4,
                ),
            ],
            frequency_fc: pair(slice(133204, 32), slice(133236, 4)),
        },
        input_width: 33,
        output_width: 33,
        stride: 1,
        shuffle_input: true,
        shuffle_output: false,
        residual: false,
    },
    Stage {
        point_input: Some(conv(
            pair(slice(133240, 384), slice(133624, 24)),
            norm(
                pair(slice(133648, 24), slice(133672, 24)),
                slice(133696, 24),
                slice(133720, 24),
            ),
            Some(affine(
                slice(133744, 792),
                slice(134536, 792),
                slice(135328, 24),
                33,
            )),
            [24, 16, 1, 1],
            2,
            false,
        )),
        spatial: conv(
            pair(slice(135352, 120), slice(135472, 24)),
            norm(
                pair(slice(135496, 24), slice(135520, 24)),
                slice(135544, 24),
                slice(135568, 24),
            ),
            Some(affine(
                slice(135592, 792),
                slice(136384, 792),
                slice(137176, 24),
                33,
            )),
            [24, 1, 1, 5],
            24,
            true,
        ),
        point_output: Some(conv(
            pair(slice(137200, 288), slice(137488, 24)),
            norm(
                pair(slice(137512, 24), slice(137536, 24)),
                slice(137560, 24),
                slice(137584, 24),
            ),
            None,
            [24, 12, 1, 1],
            2,
            false,
        )),
        attention: Attention {
            temporal: gru(
                slice(137608, 3456),
                slice(141064, 6912),
                slice(147976, 144),
                slice(148120, 144),
                24,
                48,
            ),
            temporal_fc: pair(slice(148264, 1152), slice(149416, 24)),
            frequency: [
                gru(
                    slice(149440, 48),
                    slice(149488, 48),
                    slice(149536, 12),
                    slice(149548, 12),
                    4,
                    4,
                ),
                gru(
                    slice(149560, 48),
                    slice(149608, 48),
                    slice(149656, 12),
                    slice(149668, 12),
                    4,
                    4,
                ),
            ],
            frequency_fc: pair(slice(149680, 32), slice(149712, 4)),
        },
        input_width: 33,
        output_width: 33,
        stride: 1,
        shuffle_input: true,
        shuffle_output: true,
        residual: false,
    },
    Stage {
        point_input: Some(conv(
            pair(slice(149716, 288), slice(150004, 24)),
            norm(
                pair(slice(150028, 24), slice(150052, 24)),
                slice(150076, 24),
                slice(150100, 24),
            ),
            Some(affine(
                slice(150124, 792),
                slice(150916, 792),
                slice(151708, 24),
                33,
            )),
            [24, 12, 1, 1],
            2,
            false,
        )),
        spatial: conv(
            pair(slice(151732, 144), slice(151876, 24)),
            norm(
                pair(slice(151900, 24), slice(151924, 24)),
                slice(151948, 24),
                slice(151972, 24),
            ),
            Some(affine(
                slice(151996, 792),
                slice(152788, 792),
                slice(153580, 24),
                33,
            )),
            [24, 1, 2, 3],
            24,
            true,
        ),
        point_output: None,
        attention: Attention {
            temporal: gru(
                slice(153604, 3456),
                slice(157060, 6912),
                slice(163972, 144),
                slice(164116, 144),
                24,
                48,
            ),
            temporal_fc: pair(slice(164260, 1152), slice(165412, 24)),
            frequency: [
                gru(
                    slice(165436, 48),
                    slice(165484, 48),
                    slice(165532, 12),
                    slice(165544, 12),
                    4,
                    4,
                ),
                gru(
                    slice(165556, 48),
                    slice(165604, 48),
                    slice(165652, 12),
                    slice(165664, 12),
                    4,
                    4,
                ),
            ],
            frequency_fc: pair(slice(165676, 32), slice(165708, 4)),
        },
        input_width: 33,
        output_width: 33,
        stride: 1,
        shuffle_input: true,
        shuffle_output: false,
        residual: false,
    },
    Stage {
        point_input: Some(conv(
            pair(slice(165712, 144), slice(165856, 12)),
            norm(
                pair(slice(165868, 12), slice(165880, 12)),
                slice(165892, 12),
                slice(165904, 12),
            ),
            Some(affine(
                slice(165916, 396),
                slice(166312, 396),
                slice(166708, 12),
                33,
            )),
            [12, 12, 1, 1],
            2,
            false,
        )),
        spatial: conv(
            pair(slice(166720, 72), slice(166792, 12)),
            norm(
                pair(slice(166804, 12), slice(166816, 12)),
                slice(166828, 12),
                slice(166840, 12),
            ),
            Some(affine(
                slice(166852, 780),
                slice(167632, 780),
                slice(168412, 12),
                65,
            )),
            [12, 1, 2, 3],
            12,
            true,
        ),
        point_output: Some(conv(
            pair(slice(168424, 72), slice(168496, 12)),
            norm(
                pair(slice(168508, 12), slice(168520, 12)),
                slice(168532, 12),
                slice(168544, 12),
            ),
            None,
            [12, 6, 1, 1],
            2,
            false,
        )),
        attention: Attention {
            temporal: gru(
                slice(168556, 864),
                slice(169420, 1728),
                slice(171148, 72),
                slice(171220, 72),
                12,
                24,
            ),
            temporal_fc: pair(slice(171292, 288), slice(171580, 12)),
            frequency: [
                gru(
                    slice(171592, 48),
                    slice(171640, 48),
                    slice(171688, 12),
                    slice(171700, 12),
                    4,
                    4,
                ),
                gru(
                    slice(171712, 48),
                    slice(171760, 48),
                    slice(171808, 12),
                    slice(171820, 12),
                    4,
                    4,
                ),
            ],
            frequency_fc: pair(slice(171832, 32), slice(171864, 4)),
        },
        input_width: 33,
        output_width: 65,
        stride: 2,
        shuffle_input: true,
        shuffle_output: true,
        residual: false,
    },
    Stage {
        point_input: None,
        spatial: conv(
            pair(slice(171868, 108), slice(171976, 1)),
            norm(
                pair(slice(171977, 1), slice(171978, 1)),
                slice(171979, 1),
                slice(171980, 1),
            ),
            None,
            [12, 1, 3, 3],
            1,
            true,
        ),
        point_output: None,
        attention: Attention {
            temporal: gru(
                slice(171981, 6),
                slice(171987, 12),
                slice(171999, 6),
                slice(172005, 6),
                1,
                2,
            ),
            temporal_fc: pair(slice(172011, 2), slice(172013, 1)),
            frequency: [
                gru(
                    slice(172014, 48),
                    slice(172062, 48),
                    slice(172110, 12),
                    slice(172122, 12),
                    4,
                    4,
                ),
                gru(
                    slice(172134, 48),
                    slice(172182, 48),
                    slice(172230, 12),
                    slice(172242, 12),
                    4,
                    4,
                ),
            ],
            frequency_fc: pair(slice(172254, 32), slice(172286, 4)),
        },
        input_width: 65,
        output_width: 129,
        stride: 2,
        shuffle_input: false,
        shuffle_output: false,
        residual: false,
    },
];
pub(super) const DUAL_PATH: [DpBlock; 2] = [
    DpBlock {
        intra: [
            gru(
                slice(98660, 96),
                slice(98756, 48),
                slice(98804, 12),
                slice(98816, 12),
                8,
                4,
            ),
            gru(
                slice(98828, 96),
                slice(98924, 48),
                slice(98972, 12),
                slice(98984, 12),
                8,
                4,
            ),
            gru(
                slice(98996, 96),
                slice(99092, 48),
                slice(99140, 12),
                slice(99152, 12),
                8,
                4,
            ),
            gru(
                slice(99164, 96),
                slice(99260, 48),
                slice(99308, 12),
                slice(99320, 12),
                8,
                4,
            ),
        ],
        intra_fc: pair(slice(99332, 256), slice(99588, 16)),
        intra_norm: pair(slice(99604, 528), slice(100132, 528)),
        inter: [
            gru(
                slice(100660, 192),
                slice(100852, 192),
                slice(101044, 24),
                slice(101068, 24),
                8,
                8,
            ),
            gru(
                slice(101092, 192),
                slice(101284, 192),
                slice(101476, 24),
                slice(101500, 24),
                8,
                8,
            ),
        ],
        inter_fc: pair(slice(101524, 256), slice(101780, 16)),
        inter_norm: pair(slice(101796, 528), slice(102324, 528)),
    },
    DpBlock {
        intra: [
            gru(
                slice(102852, 96),
                slice(102948, 48),
                slice(102996, 12),
                slice(103008, 12),
                8,
                4,
            ),
            gru(
                slice(103020, 96),
                slice(103116, 48),
                slice(103164, 12),
                slice(103176, 12),
                8,
                4,
            ),
            gru(
                slice(103188, 96),
                slice(103284, 48),
                slice(103332, 12),
                slice(103344, 12),
                8,
                4,
            ),
            gru(
                slice(103356, 96),
                slice(103452, 48),
                slice(103500, 12),
                slice(103512, 12),
                8,
                4,
            ),
        ],
        intra_fc: pair(slice(103524, 256), slice(103780, 16)),
        intra_norm: pair(slice(103796, 528), slice(104324, 528)),
        inter: [
            gru(
                slice(104852, 192),
                slice(105044, 192),
                slice(105236, 24),
                slice(105260, 24),
                8,
                8,
            ),
            gru(
                slice(105284, 192),
                slice(105476, 192),
                slice(105668, 24),
                slice(105692, 24),
                8,
                8,
            ),
        ],
        inter_fc: pair(slice(105716, 256), slice(105972, 16)),
        inter_norm: pair(slice(105988, 528), slice(106516, 528)),
    },
];

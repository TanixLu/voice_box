// GTCRN DNS3 tensor offsets; model provenance: assets/gtcrn/README.md.
const BYTES: &[u8] = include_bytes!("../../assets/gtcrn/dns3.f32");
static DATA: [f32; 48825] = decode();
const fn decode() -> [f32; 48825] {
    let mut out = [0.0; 48825];
    let mut i = 0;
    while i < out.len() {
        let j = i * 4;
        out[i] = f32::from_le_bytes([BYTES[j], BYTES[j + 1], BYTES[j + 2], BYTES[j + 3]]);
        i += 1;
    }
    out
}

// Each slice is checked and resolved during constant evaluation; all reference DATA.
const fn slice(start: usize, end: usize) -> &'static [f32] {
    assert!(start <= end);
    DATA.split_at(end).0.split_at(start).1
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
pub(super) struct Conv {
    pub parameters: Pair,
    pub norm: Norm,
    pub slope: f32,
}
#[derive(Clone, Copy)]
pub(super) struct Gru {
    pub weight_input: &'static [f32],
    pub weight_hidden: &'static [f32],
    pub bias_input: &'static [f32],
    pub bias_hidden: &'static [f32],
}
#[derive(Clone, Copy)]
pub(super) struct GtBlock {
    pub point: [Conv; 2],
    pub depth: Conv,
    pub attention: Gru,
    pub attention_fc: Pair,
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
    Norm {
        parameters,
        mean,
        variance,
    }
}
const fn conv(parameters: Pair, norm: Norm, slope: f32) -> Conv {
    Conv {
        parameters,
        norm,
        slope,
    }
}
const fn gru(
    weight_input: &'static [f32],
    weight_hidden: &'static [f32],
    bias_input: &'static [f32],
    bias_hidden: &'static [f32],
) -> Gru {
    Gru {
        weight_input,
        weight_hidden,
        bias_input,
        bias_hidden,
    }
}
const fn gt_block(point: [Conv; 2], depth: Conv, attention: Gru, attention_fc: Pair) -> GtBlock {
    GtBlock {
        point,
        depth,
        attention,
        attention_fc,
    }
}
const fn dp_block(
    intra: [Gru; 4],
    intra_fc: Pair,
    intra_norm: Pair,
    inter: [Gru; 2],
    inter_fc: Pair,
    inter_norm: Pair,
) -> DpBlock {
    DpBlock {
        intra,
        intra_fc,
        intra_norm,
        inter,
        inter_fc,
        inter_norm,
    }
}
pub(super) const ERB: &[f32] = slice(0, 12288);
pub(super) const INVERSE_ERB: &[f32] = slice(12288, 24576);
pub(super) const ENCODER: [Conv; 2] = [
    conv(
        pair(slice(24576, 25296), slice(25296, 25312)),
        norm(
            pair(slice(25312, 25328), slice(25328, 25344)),
            slice(25344, 25360),
            slice(25360, 25376),
        ),
        slice(25376, 25377)[0],
    ),
    conv(
        pair(slice(25377, 26017), slice(26017, 26033)),
        norm(
            pair(slice(26033, 26049), slice(26049, 26065)),
            slice(26065, 26081),
            slice(26081, 26097),
        ),
        slice(26097, 26098)[0],
    ),
];
pub(super) const BLOCKS: [GtBlock; 6] = [
    gt_block(
        [
            conv(
                pair(slice(26098, 26482), slice(26482, 26498)),
                norm(
                    pair(slice(26498, 26514), slice(26514, 26530)),
                    slice(26530, 26546),
                    slice(26546, 26562),
                ),
                slice(26562, 26563)[0],
            ),
            conv(
                pair(slice(26788, 26916), slice(26916, 26924)),
                norm(
                    pair(slice(26924, 26932), slice(26932, 26940)),
                    slice(26940, 26948),
                    slice(26948, 26956),
                ),
                1.0,
            ),
        ],
        conv(
            pair(slice(26563, 26707), slice(26707, 26723)),
            norm(
                pair(slice(26723, 26739), slice(26739, 26755)),
                slice(26755, 26771),
                slice(26771, 26787),
            ),
            slice(26787, 26788)[0],
        ),
        gru(
            slice(26956, 27340),
            slice(27340, 28108),
            slice(28108, 28156),
            slice(28156, 28204),
        ),
        pair(slice(28204, 28332), slice(28332, 28340)),
    ),
    gt_block(
        [
            conv(
                pair(slice(28340, 28724), slice(28724, 28740)),
                norm(
                    pair(slice(28740, 28756), slice(28756, 28772)),
                    slice(28772, 28788),
                    slice(28788, 28804),
                ),
                slice(28804, 28805)[0],
            ),
            conv(
                pair(slice(29030, 29158), slice(29158, 29166)),
                norm(
                    pair(slice(29166, 29174), slice(29174, 29182)),
                    slice(29182, 29190),
                    slice(29190, 29198),
                ),
                1.0,
            ),
        ],
        conv(
            pair(slice(28805, 28949), slice(28949, 28965)),
            norm(
                pair(slice(28965, 28981), slice(28981, 28997)),
                slice(28997, 29013),
                slice(29013, 29029),
            ),
            slice(29029, 29030)[0],
        ),
        gru(
            slice(29198, 29582),
            slice(29582, 30350),
            slice(30350, 30398),
            slice(30398, 30446),
        ),
        pair(slice(30446, 30574), slice(30574, 30582)),
    ),
    gt_block(
        [
            conv(
                pair(slice(30582, 30966), slice(30966, 30982)),
                norm(
                    pair(slice(30982, 30998), slice(30998, 31014)),
                    slice(31014, 31030),
                    slice(31030, 31046),
                ),
                slice(31046, 31047)[0],
            ),
            conv(
                pair(slice(31272, 31400), slice(31400, 31408)),
                norm(
                    pair(slice(31408, 31416), slice(31416, 31424)),
                    slice(31424, 31432),
                    slice(31432, 31440),
                ),
                1.0,
            ),
        ],
        conv(
            pair(slice(31047, 31191), slice(31191, 31207)),
            norm(
                pair(slice(31207, 31223), slice(31223, 31239)),
                slice(31239, 31255),
                slice(31255, 31271),
            ),
            slice(31271, 31272)[0],
        ),
        gru(
            slice(31440, 31824),
            slice(31824, 32592),
            slice(32592, 32640),
            slice(32640, 32688),
        ),
        pair(slice(32688, 32816), slice(32816, 32824)),
    ),
    gt_block(
        [
            conv(
                pair(slice(41208, 41592), slice(41592, 41608)),
                norm(
                    pair(slice(41608, 41624), slice(41624, 41640)),
                    slice(41640, 41656),
                    slice(41656, 41672),
                ),
                slice(41672, 41673)[0],
            ),
            conv(
                pair(slice(41898, 42026), slice(42026, 42034)),
                norm(
                    pair(slice(42034, 42042), slice(42042, 42050)),
                    slice(42050, 42058),
                    slice(42058, 42066),
                ),
                1.0,
            ),
        ],
        conv(
            pair(slice(41673, 41817), slice(41817, 41833)),
            norm(
                pair(slice(41833, 41849), slice(41849, 41865)),
                slice(41865, 41881),
                slice(41881, 41897),
            ),
            slice(41897, 41898)[0],
        ),
        gru(
            slice(42066, 42450),
            slice(42450, 43218),
            slice(43218, 43266),
            slice(43266, 43314),
        ),
        pair(slice(43314, 43442), slice(43442, 43450)),
    ),
    gt_block(
        [
            conv(
                pair(slice(43450, 43834), slice(43834, 43850)),
                norm(
                    pair(slice(43850, 43866), slice(43866, 43882)),
                    slice(43882, 43898),
                    slice(43898, 43914),
                ),
                slice(43914, 43915)[0],
            ),
            conv(
                pair(slice(44140, 44268), slice(44268, 44276)),
                norm(
                    pair(slice(44276, 44284), slice(44284, 44292)),
                    slice(44292, 44300),
                    slice(44300, 44308),
                ),
                1.0,
            ),
        ],
        conv(
            pair(slice(43915, 44059), slice(44059, 44075)),
            norm(
                pair(slice(44075, 44091), slice(44091, 44107)),
                slice(44107, 44123),
                slice(44123, 44139),
            ),
            slice(44139, 44140)[0],
        ),
        gru(
            slice(44308, 44692),
            slice(44692, 45460),
            slice(45460, 45508),
            slice(45508, 45556),
        ),
        pair(slice(45556, 45684), slice(45684, 45692)),
    ),
    gt_block(
        [
            conv(
                pair(slice(45692, 46076), slice(46076, 46092)),
                norm(
                    pair(slice(46092, 46108), slice(46108, 46124)),
                    slice(46124, 46140),
                    slice(46140, 46156),
                ),
                slice(46156, 46157)[0],
            ),
            conv(
                pair(slice(46382, 46510), slice(46510, 46518)),
                norm(
                    pair(slice(46518, 46526), slice(46526, 46534)),
                    slice(46534, 46542),
                    slice(46542, 46550),
                ),
                1.0,
            ),
        ],
        conv(
            pair(slice(46157, 46301), slice(46301, 46317)),
            norm(
                pair(slice(46317, 46333), slice(46333, 46349)),
                slice(46349, 46365),
                slice(46365, 46381),
            ),
            slice(46381, 46382)[0],
        ),
        gru(
            slice(46550, 46934),
            slice(46934, 47702),
            slice(47702, 47750),
            slice(47750, 47798),
        ),
        pair(slice(47798, 47926), slice(47926, 47934)),
    ),
];
pub(super) const DUAL_PATH: [DpBlock; 2] = [
    dp_block(
        [
            gru(
                slice(32824, 32920),
                slice(32920, 32968),
                slice(32968, 32980),
                slice(32980, 32992),
            ),
            gru(
                slice(32992, 33088),
                slice(33088, 33136),
                slice(33136, 33148),
                slice(33148, 33160),
            ),
            gru(
                slice(33160, 33256),
                slice(33256, 33304),
                slice(33304, 33316),
                slice(33316, 33328),
            ),
            gru(
                slice(33328, 33424),
                slice(33424, 33472),
                slice(33472, 33484),
                slice(33484, 33496),
            ),
        ],
        pair(slice(33496, 33752), slice(33752, 33768)),
        pair(slice(33768, 34296), slice(34296, 34824)),
        [
            gru(
                slice(34824, 35016),
                slice(35016, 35208),
                slice(35208, 35232),
                slice(35232, 35256),
            ),
            gru(
                slice(35256, 35448),
                slice(35448, 35640),
                slice(35640, 35664),
                slice(35664, 35688),
            ),
        ],
        pair(slice(35688, 35944), slice(35944, 35960)),
        pair(slice(35960, 36488), slice(36488, 37016)),
    ),
    dp_block(
        [
            gru(
                slice(37016, 37112),
                slice(37112, 37160),
                slice(37160, 37172),
                slice(37172, 37184),
            ),
            gru(
                slice(37184, 37280),
                slice(37280, 37328),
                slice(37328, 37340),
                slice(37340, 37352),
            ),
            gru(
                slice(37352, 37448),
                slice(37448, 37496),
                slice(37496, 37508),
                slice(37508, 37520),
            ),
            gru(
                slice(37520, 37616),
                slice(37616, 37664),
                slice(37664, 37676),
                slice(37676, 37688),
            ),
        ],
        pair(slice(37688, 37944), slice(37944, 37960)),
        pair(slice(37960, 38488), slice(38488, 39016)),
        [
            gru(
                slice(39016, 39208),
                slice(39208, 39400),
                slice(39400, 39424),
                slice(39424, 39448),
            ),
            gru(
                slice(39448, 39640),
                slice(39640, 39832),
                slice(39832, 39856),
                slice(39856, 39880),
            ),
        ],
        pair(slice(39880, 40136), slice(40136, 40152)),
        pair(slice(40152, 40680), slice(40680, 41208)),
    ),
];
pub(super) const DECODER: [Conv; 2] = [
    conv(
        pair(slice(47934, 48574), slice(48574, 48590)),
        norm(
            pair(slice(48590, 48606), slice(48606, 48622)),
            slice(48622, 48638),
            slice(48638, 48654),
        ),
        slice(48654, 48655)[0],
    ),
    conv(
        pair(slice(48655, 48815), slice(48815, 48817)),
        norm(
            pair(slice(48817, 48819), slice(48819, 48821)),
            slice(48821, 48823),
            slice(48823, 48825),
        ),
        0.0,
    ),
];

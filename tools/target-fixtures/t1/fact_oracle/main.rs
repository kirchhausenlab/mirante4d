//! Zero-dependency scientific fact oracle for the WP-10A target T1 corpus.
//!
//! This program reads only the declarative TSV supplied on the command line.
//! It never opens a package or archive and has no dependency on Mirante4D code.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fmt::Write as _,
    fs,
    path::PathBuf,
    process,
};

const HEADER: &str = "spec_version|case_id|dtype|t|c|z|y|x|levels|validity|physical_channels|temporal_step_f64_bits|grid_to_world_f64_bits|ome_projection|ome_level_scale_zyx_f64_bits|ome_level_translation_zyx_f64_bits|value_rule|value_parameters|validity_rule|validity_parameters";
const TILE_SHAPE_TZYX: [u64; 4] = [1, 16, 256, 256];
const BRICK_2D_ZYX: [u64; 3] = [1, 256, 256];
const BRICK_3D_ZYX: [u64; 3] = [64, 64, 64];
const MERKLE_ARITY: usize = 1024;
const TILE_DOMAIN: &[u8] = b"M4D-SC-V1-TILE\0";
const NODE_DOMAIN: &[u8] = b"M4D-SC-V1-NODE\0";
const LAYER_DOMAIN: &[u8] = b"M4D-SC-V1-LAYER\0";
const DATASET_DOMAIN: &[u8] = b"M4D-SC-V1-DATASET\0";

type Digest = [u8; 32];

fn main() {
    if let Err(error) = real_main() {
        eprintln!("fact oracle failed: {error}");
        process::exit(1);
    }
}

fn real_main() -> Result<(), String> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if arguments == ["--self-test"] {
        self_test()?;
        println!("WP-10A T1 fact oracle self-test: PASS");
        return Ok(());
    }
    if arguments.len() != 4 || arguments[0] != "--spec" || arguments[2] != "--output" {
        return Err(
            "usage: fact-oracle --self-test | --spec <cases-v1.tsv> --output <facts.json>".into(),
        );
    }
    let spec_path = PathBuf::from(&arguments[1]);
    let output_path = PathBuf::from(&arguments[3]);
    if spec_path == output_path {
        return Err("specification and output paths must differ".into());
    }
    let encoded = fs::read(&spec_path)
        .map_err(|error| format!("cannot read {}: {error}", spec_path.display()))?;
    let text = std::str::from_utf8(&encoded).map_err(|_| "TSV must be UTF-8".to_owned())?;
    let cases = parse_cases(text)?;
    let report = build_report(&cases, sha256(&encoded))?;
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    fs::write(&output_path, report)
        .map_err(|error| format!("cannot write {}: {error}", output_path.display()))?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DType {
    Uint8,
    Uint16,
    Float32,
}

impl DType {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "uint8" => Ok(Self::Uint8),
            "uint16" => Ok(Self::Uint16),
            "float32" => Ok(Self::Float32),
            _ => Err(format!("unsupported dtype {value:?}")),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Uint8 => "uint8",
            Self::Uint16 => "uint16",
            Self::Float32 => "float32",
        }
    }

    const fn tag(self) -> u8 {
        match self {
            Self::Uint8 => 1,
            Self::Uint16 => 2,
            Self::Float32 => 3,
        }
    }

    const fn bytes(self) -> usize {
        match self {
            Self::Uint8 => 1,
            Self::Uint16 => 2,
            Self::Float32 => 4,
        }
    }
}

#[derive(Clone, Debug)]
enum ValueRule {
    Sparse(BTreeMap<[u64; 5], u8>),
    AffineMod {
        t: u64,
        c: u64,
        z: u64,
        y: u64,
        x: u64,
        modulus: u64,
    },
    F32Cycle(Vec<u32>),
}

#[derive(Clone, Debug)]
enum ValidityRule {
    AllValid,
    Mixed {
        all_invalid_channel: u64,
        modulus: u64,
        channel_stride: u64,
    },
}

#[derive(Clone, Debug)]
struct Case {
    id: String,
    dtype: DType,
    shape: [u64; 5],
    levels: u32,
    validity_name: String,
    physical_channels: Vec<u64>,
    temporal_step_hex: String,
    matrix_hex: Vec<String>,
    ome_projection: String,
    ome_scales_hex: Vec<[String; 3]>,
    ome_translations_hex: Vec<[String; 3]>,
    value_rule: ValueRule,
    validity_rule: ValidityRule,
}

fn parse_cases(text: &str) -> Result<Vec<Case>, String> {
    if text.contains('\r') {
        return Err("TSV must use LF line endings".into());
    }
    let mut lines = text.lines();
    if lines.next() != Some(HEADER) {
        return Err("TSV header does not match version 1".into());
    }
    let mut cases = Vec::new();
    let mut ids = BTreeSet::new();
    for (offset, line) in lines.enumerate() {
        let line_number = offset + 2;
        if line.is_empty() {
            return Err(format!("empty TSV row at line {line_number}"));
        }
        let fields = line.split('|').collect::<Vec<_>>();
        if fields.len() != 20 {
            return Err(format!(
                "line {line_number} has {} fields, expected 20",
                fields.len()
            ));
        }
        if fields[0] != "1" {
            return Err(format!("line {line_number} has unsupported spec version"));
        }
        let id = fields[1].to_owned();
        if id.is_empty() || !ids.insert(id.clone()) {
            return Err(format!(
                "line {line_number} has an empty or duplicate case id"
            ));
        }
        let dtype = DType::parse(fields[2])?;
        let shape = [
            parse_positive(fields[3], "t")?,
            parse_positive(fields[4], "c")?,
            parse_positive(fields[5], "z")?,
            parse_positive(fields[6], "y")?,
            parse_positive(fields[7], "x")?,
        ];
        let levels = u32::try_from(parse_positive(fields[8], "levels")?)
            .map_err(|_| "levels exceed u32".to_owned())?;
        let physical_channels = parse_u64_list(fields[10], ',')?;
        if physical_channels.len() != usize::try_from(shape[1]).map_err(|_| "c exceeds usize")? {
            return Err(format!("case {id} physical channel count differs from c"));
        }
        let expected_channels = (0..shape[1]).collect::<BTreeSet<_>>();
        if physical_channels.iter().copied().collect::<BTreeSet<_>>() != expected_channels {
            return Err(format!("case {id} physical channels are not a permutation"));
        }
        let temporal_step_hex = parse_f64_hex(fields[11], "temporal step")?;
        let temporal_step = f64::from_bits(parse_hex_u64(&temporal_step_hex)?);
        if !temporal_step.is_finite() || temporal_step <= 0.0 {
            return Err(format!("case {id} temporal step must be positive finite"));
        }
        let matrix_hex = parse_hex_list(fields[12], 16, "grid_to_world")?;
        if matrix_hex.len() != 16 {
            return Err(format!("case {id} grid_to_world needs 16 values"));
        }
        let ome_scales_hex = parse_triplet_levels(fields[14], levels, "OME scales")?;
        let ome_translations_hex = parse_triplet_levels(fields[15], levels, "OME translations")?;
        let value_rule = match fields[16] {
            "sparse_points" => {
                if dtype != DType::Uint8 {
                    return Err("sparse_points requires uint8".into());
                }
                ValueRule::Sparse(parse_sparse_points(fields[17], shape)?)
            }
            "affine_mod_decimate" => {
                if dtype != DType::Uint16 {
                    return Err("affine_mod_decimate requires uint16".into());
                }
                let parameters = parse_labeled_u64(fields[17])?;
                ValueRule::AffineMod {
                    t: required_parameter(&parameters, "t")?,
                    c: required_parameter(&parameters, "c")?,
                    z: required_parameter(&parameters, "z")?,
                    y: required_parameter(&parameters, "y")?,
                    x: required_parameter(&parameters, "x")?,
                    modulus: required_parameter(&parameters, "mod")?,
                }
            }
            "f32_cycle" => {
                if dtype != DType::Float32 {
                    return Err("f32_cycle requires float32".into());
                }
                let encoded = fields[17]
                    .strip_prefix("cycle_bits=")
                    .ok_or_else(|| "f32 cycle lacks cycle_bits label".to_owned())?;
                let bits = encoded
                    .split(',')
                    .map(parse_hex_u32)
                    .collect::<Result<Vec<_>, _>>()?;
                if bits.len() != 16 || bits.iter().any(|value| !f32::from_bits(*value).is_finite())
                {
                    return Err("f32 cycle must contain 16 finite bit patterns".into());
                }
                ValueRule::F32Cycle(bits)
            }
            other => return Err(format!("case {id} has unsupported value rule {other:?}")),
        };
        let validity_rule = match fields[18] {
            "all_valid" if fields[19].is_empty() => ValidityRule::AllValid,
            "mixed_and_all_invalid" => {
                let parameters = parse_labeled_u64(fields[19])?;
                ValidityRule::Mixed {
                    all_invalid_channel: required_parameter(&parameters, "all_invalid_channel")?,
                    modulus: required_parameter(&parameters, "mixed_modulus")?,
                    channel_stride: required_parameter(&parameters, "mixed_channel_stride")?,
                }
            }
            other => return Err(format!("case {id} has unsupported validity rule {other:?}")),
        };
        match (fields[9], &validity_rule) {
            ("all_valid", ValidityRule::AllValid) | ("explicit", ValidityRule::Mixed { .. }) => {}
            _ => {
                return Err(format!(
                    "case {id} validity declaration disagrees with its rule"
                ));
            }
        }
        cases.push(Case {
            id,
            dtype,
            shape,
            levels,
            validity_name: fields[9].to_owned(),
            physical_channels,
            temporal_step_hex,
            matrix_hex,
            ome_projection: fields[13].to_owned(),
            ome_scales_hex,
            ome_translations_hex,
            value_rule,
            validity_rule,
        });
    }
    let required = BTreeSet::from([
        "m4d-t1-u8-2d-sparse",
        "m4d-t1-u16-3d-multiscale",
        "m4d-t1-f32-3d-validity",
    ]);
    if cases.len() != 3 || ids.iter().map(String::as_str).collect::<BTreeSet<_>>() != required {
        return Err("version-1 corpus must contain exactly the three accepted cases".into());
    }
    cases.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(cases)
}

fn parse_positive(value: &str, label: &str) -> Result<u64, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| format!("invalid {label}"))?;
    if parsed == 0 {
        Err(format!("{label} must be positive"))
    } else {
        Ok(parsed)
    }
}

fn parse_u64_list(value: &str, separator: char) -> Result<Vec<u64>, String> {
    value
        .split(separator)
        .map(|part| {
            part.parse::<u64>()
                .map_err(|_| format!("invalid integer {part:?}"))
        })
        .collect()
}

fn parse_hex_u32(value: &str) -> Result<u32, String> {
    if value.len() != 8
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("invalid lowercase u32 bits {value:?}"));
    }
    u32::from_str_radix(value, 16).map_err(|_| format!("invalid u32 bits {value:?}"))
}

fn parse_hex_u64(value: &str) -> Result<u64, String> {
    if value.len() != 16
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("invalid lowercase u64 bits {value:?}"));
    }
    u64::from_str_radix(value, 16).map_err(|_| format!("invalid u64 bits {value:?}"))
}

fn parse_f64_hex(value: &str, label: &str) -> Result<String, String> {
    let bits = parse_hex_u64(value)?;
    let number = f64::from_bits(bits);
    if !number.is_finite() {
        return Err(format!("{label} is non-finite"));
    }
    Ok(if number == 0.0 {
        "0000000000000000".into()
    } else {
        value.into()
    })
}

fn parse_hex_list(value: &str, width: usize, label: &str) -> Result<Vec<String>, String> {
    value
        .split(',')
        .map(|item| {
            if width != 16 {
                return Err("unsupported hex width".into());
            }
            parse_f64_hex(item, label)
        })
        .collect()
}

fn parse_triplet_levels(value: &str, levels: u32, label: &str) -> Result<Vec<[String; 3]>, String> {
    let rows = value.split(';').collect::<Vec<_>>();
    if rows.len() != levels as usize {
        return Err(format!("{label} count differs from levels"));
    }
    rows.into_iter()
        .map(|row| {
            let values = parse_hex_list(row, 16, label)?;
            values
                .try_into()
                .map_err(|_| format!("{label} row is not a triplet"))
        })
        .collect()
}

fn parse_labeled_u64(value: &str) -> Result<BTreeMap<String, u64>, String> {
    let mut result = BTreeMap::new();
    for part in value.split(',') {
        let (name, encoded) = part
            .split_once('=')
            .ok_or_else(|| format!("unlabeled parameter {part:?}"))?;
        let parsed = encoded
            .parse::<u64>()
            .map_err(|_| format!("invalid parameter {part:?}"))?;
        if name.is_empty() || result.insert(name.to_owned(), parsed).is_some() {
            return Err(format!("empty or duplicate parameter {name:?}"));
        }
    }
    Ok(result)
}

fn required_parameter(parameters: &BTreeMap<String, u64>, name: &str) -> Result<u64, String> {
    parameters
        .get(name)
        .copied()
        .ok_or_else(|| format!("missing parameter {name}"))
}

fn parse_sparse_points(value: &str, shape: [u64; 5]) -> Result<BTreeMap<[u64; 5], u8>, String> {
    let mut points = BTreeMap::new();
    for encoded in value.split(';') {
        let fields = parse_u64_list(encoded, ',')?;
        if fields.len() != 6
            || fields[..5]
                .iter()
                .zip(shape)
                .any(|(coordinate, bound)| *coordinate >= bound)
        {
            return Err(format!("invalid sparse point {encoded:?}"));
        }
        let value =
            u8::try_from(fields[5]).map_err(|_| "sparse uint8 value overflow".to_owned())?;
        let coordinate = [fields[0], fields[1], fields[2], fields[3], fields[4]];
        if value == 0 || points.insert(coordinate, value).is_some() {
            return Err("sparse points must be unique and nonzero".into());
        }
    }
    Ok(points)
}

// Independent FIPS 180-4 SHA-256 implementation. It deliberately does not use
// the Rust workspace's identity crate or SHA dependency.
#[derive(Clone)]
struct Sha256 {
    state: [u32; 8],
    pending: [u8; 64],
    pending_len: usize,
    message_bytes: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            pending: [0; 64],
            pending_len: 0,
            message_bytes: 0,
        }
    }

    fn update(&mut self, mut bytes: &[u8]) {
        self.message_bytes = self
            .message_bytes
            .checked_add(bytes.len() as u64)
            .expect("oracle input length overflow");
        if self.pending_len != 0 {
            let take = (64 - self.pending_len).min(bytes.len());
            self.pending[self.pending_len..self.pending_len + take].copy_from_slice(&bytes[..take]);
            self.pending_len += take;
            bytes = &bytes[take..];
            if self.pending_len < 64 {
                return;
            }
            let block = self.pending;
            self.compress(&block);
            self.pending_len = 0;
        }
        while bytes.len() >= 64 {
            let block: &[u8; 64] = bytes[..64].try_into().expect("fixed block");
            self.compress(block);
            bytes = &bytes[64..];
        }
        self.pending[..bytes.len()].copy_from_slice(bytes);
        self.pending_len = bytes.len();
    }

    fn finalize(mut self) -> Digest {
        let bit_length = self
            .message_bytes
            .checked_mul(8)
            .expect("oracle SHA length overflow");
        self.pending[self.pending_len] = 0x80;
        self.pending_len += 1;
        if self.pending_len > 56 {
            self.pending[self.pending_len..].fill(0);
            let block = self.pending;
            self.compress(&block);
            self.pending = [0; 64];
            self.pending_len = 0;
        }
        self.pending[self.pending_len..56].fill(0);
        self.pending[56..64].copy_from_slice(&bit_length.to_be_bytes());
        let block = self.pending;
        self.compress(&block);
        let mut result = [0; 32];
        for (chunk, value) in result.chunks_exact_mut(4).zip(self.state) {
            chunk.copy_from_slice(&value.to_be_bytes());
        }
        result
    }

    fn compress(&mut self, block: &[u8; 64]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut schedule = [0u32; 64];
        for (index, bytes) in block.chunks_exact(4).enumerate() {
            schedule[index] = u32::from_be_bytes(bytes.try_into().expect("word"));
        }
        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for index in 0..64 {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(sum1)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(schedule[index]);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = sum0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (target, value) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *target = target.wrapping_add(value);
        }
    }
}

fn sha256(bytes: &[u8]) -> Digest {
    let mut state = Sha256::new();
    state.update(bytes);
    state.finalize()
}

fn digest_hex(digest: Digest) -> String {
    let mut result = String::with_capacity(64);
    for byte in digest {
        write!(result, "{byte:02x}").expect("write to string");
    }
    result
}

#[derive(Clone, Copy, Debug)]
enum Sample {
    Uint8(u8),
    Uint16(u16),
    Float32(u32),
}

impl Sample {
    fn raw_bytes(self) -> Vec<u8> {
        match self {
            Self::Uint8(value) => vec![value],
            Self::Uint16(value) => value.to_le_bytes().to_vec(),
            Self::Float32(bits) => bits.to_le_bytes().to_vec(),
        }
    }

    fn canonical_bytes(self, valid: bool) -> Vec<u8> {
        if valid {
            self.raw_bytes()
        } else {
            match self {
                Self::Uint8(_) => vec![0],
                Self::Uint16(_) => vec![0, 0],
                Self::Float32(_) => vec![0, 0, 0, 0],
            }
        }
    }

    fn json_value(self) -> String {
        match self {
            Self::Uint8(value) => format!("{{\"uint\":{value}}}"),
            Self::Uint16(value) => format!("{{\"uint\":{value}}}"),
            Self::Float32(bits) => format!("{{\"f32_bits\":\"{bits:08x}\"}}"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Mutation {
    None,
    ValueBit {
        case_layer: u64,
        coordinate_tzyx: [u64; 4],
    },
    ValidityBit {
        case_layer: u64,
        coordinate_tzyx: [u64; 4],
    },
    TransformBit,
}

fn level_shape(case: &Case, level: u32) -> [u64; 5] {
    let divisor = 1u64 << level;
    [
        case.shape[0],
        case.shape[1],
        case.shape[2].div_ceil(divisor),
        case.shape[3].div_ceil(divisor),
        case.shape[4].div_ceil(divisor),
    ]
}

fn base_coordinate(case: &Case, level: u32, coordinate: [u64; 5]) -> [u64; 5] {
    let factor = 1u64 << level;
    [
        coordinate[0],
        coordinate[1],
        (coordinate[2] * factor).min(case.shape[2] - 1),
        (coordinate[3] * factor).min(case.shape[3] - 1),
        (coordinate[4] * factor).min(case.shape[4] - 1),
    ]
}

fn raw_sample(case: &Case, level: u32, coordinate: [u64; 5], mutation: Mutation) -> Sample {
    let base = base_coordinate(case, level, coordinate);
    let mut sample = match &case.value_rule {
        ValueRule::Sparse(points) => Sample::Uint8(points.get(&base).copied().unwrap_or(0)),
        ValueRule::AffineMod {
            t,
            c,
            z,
            y,
            x,
            modulus,
        } => {
            let value =
                (t * base[0] + c * base[1] + z * base[2] + y * base[3] + x * base[4]) % modulus;
            Sample::Uint16(u16::try_from(value).expect("validated uint16 modulus"))
        }
        ValueRule::F32Cycle(bits) => {
            let ordinal = ((((base[0] * case.shape[1] + base[1]) * case.shape[2] + base[2])
                * case.shape[3]
                + base[3])
                * case.shape[4]
                + base[4]) as usize;
            Sample::Float32(bits[ordinal % bits.len()])
        }
    };
    if let Mutation::ValueBit {
        case_layer,
        coordinate_tzyx,
    } = mutation
    {
        if level == 0
            && coordinate[1] == case_layer
            && [coordinate[0], coordinate[2], coordinate[3], coordinate[4]] == coordinate_tzyx
        {
            sample = match sample {
                Sample::Uint8(value) => Sample::Uint8(value ^ 1),
                Sample::Uint16(value) => Sample::Uint16(value ^ 1),
                Sample::Float32(bits) => Sample::Float32(bits ^ 1),
            };
        }
    }
    sample
}

fn sample_valid(case: &Case, level: u32, coordinate: [u64; 5], mutation: Mutation) -> bool {
    let base = base_coordinate(case, level, coordinate);
    let mut valid = match case.validity_rule {
        ValidityRule::AllValid => true,
        ValidityRule::Mixed {
            all_invalid_channel,
            modulus,
            channel_stride,
        } => {
            base[1] != all_invalid_channel
                && (base[2] * case.shape[3] * case.shape[4]
                    + base[3] * case.shape[4]
                    + base[4]
                    + channel_stride * base[1])
                    % modulus
                    != 0
        }
    };
    if let Mutation::ValidityBit {
        case_layer,
        coordinate_tzyx,
    } = mutation
    {
        if level == 0
            && coordinate[1] == case_layer
            && [coordinate[0], coordinate[2], coordinate[3], coordinate[4]] == coordinate_tzyx
        {
            valid = !valid;
        }
    }
    valid
}

fn voxel_count(shape: [u64; 5]) -> Result<u64, String> {
    shape.into_iter().try_fold(1u64, |product, value| {
        product
            .checked_mul(value)
            .ok_or_else(|| "voxel count overflow".to_owned())
    })
}

fn layer_voxel_count(shape: [u64; 5]) -> Result<u64, String> {
    [shape[0], shape[2], shape[3], shape[4]]
        .into_iter()
        .try_fold(1u64, |product, value| {
            product
                .checked_mul(value)
                .ok_or_else(|| "layer voxel count overflow".to_owned())
        })
}

#[derive(Clone, Debug)]
struct LayerBytes {
    raw_values: Vec<u8>,
    canonical_values: Vec<u8>,
    validity: Vec<u8>,
}

fn build_layer_bytes(
    case: &Case,
    level: u32,
    layer: u64,
    mutation: Mutation,
) -> Result<LayerBytes, String> {
    let shape = level_shape(case, level);
    let count =
        usize::try_from(layer_voxel_count(shape)?).map_err(|_| "layer is too large".to_owned())?;
    let value_bytes = count
        .checked_mul(case.dtype.bytes())
        .ok_or_else(|| "layer byte count overflow".to_owned())?;
    let mut raw_values = Vec::with_capacity(value_bytes);
    let mut canonical_values = Vec::with_capacity(value_bytes);
    let mut validity = vec![0u8; count.div_ceil(8)];
    let mut ordinal = 0usize;
    for t in 0..shape[0] {
        for z in 0..shape[2] {
            for y in 0..shape[3] {
                for x in 0..shape[4] {
                    let coordinate = [t, layer, z, y, x];
                    let sample = raw_sample(case, level, coordinate, mutation);
                    let valid = sample_valid(case, level, coordinate, mutation);
                    if valid {
                        validity[ordinal / 8] |= 1 << (ordinal % 8);
                    }
                    raw_values.extend(sample.raw_bytes());
                    canonical_values.extend(sample.canonical_bytes(valid));
                    ordinal += 1;
                }
            }
        }
    }
    Ok(LayerBytes {
        raw_values,
        canonical_values,
        validity,
    })
}

fn validity_at(bytes: &[u8], ordinal: usize) -> bool {
    bytes[ordinal / 8] & (1 << (ordinal % 8)) != 0
}

fn layer_ordinal(shape: [u64; 5], t: u64, z: u64, y: u64, x: u64) -> usize {
    (((t * shape[2] + z) * shape[3] + y) * shape[4] + x) as usize
}

fn sample_from_bytes(dtype: DType, bytes: &[u8], ordinal: usize) -> Sample {
    let offset = ordinal * dtype.bytes();
    match dtype {
        DType::Uint8 => Sample::Uint8(bytes[offset]),
        DType::Uint16 => Sample::Uint16(u16::from_le_bytes(
            bytes[offset..offset + 2].try_into().expect("u16"),
        )),
        DType::Float32 => Sample::Float32(u32::from_le_bytes(
            bytes[offset..offset + 4].try_into().expect("f32"),
        )),
    }
}

#[derive(Clone, Debug)]
struct BrickFact {
    t: u64,
    layer: u64,
    z: u64,
    y: u64,
    x: u64,
    extent: [u64; 3],
    voxels: u64,
    valid: u64,
    nonfill_valid: u64,
    minimum: Option<Sample>,
    maximum: Option<Sample>,
}

fn sample_less(left: Sample, right: Sample) -> bool {
    match (left, right) {
        (Sample::Uint8(a), Sample::Uint8(b)) => a < b,
        (Sample::Uint16(a), Sample::Uint16(b)) => a < b,
        (Sample::Float32(a), Sample::Float32(b)) => {
            f32::from_bits(a).total_cmp(&f32::from_bits(b)).is_lt()
        }
        _ => unreachable!("one dtype per case"),
    }
}

fn sample_is_nonfill(sample: Sample) -> bool {
    match sample {
        Sample::Uint8(value) => value != 0,
        Sample::Uint16(value) => value != 0,
        Sample::Float32(bits) => bits != 0,
    }
}

fn brick_facts(case: &Case, level: u32, layers: &[LayerBytes]) -> Result<Vec<BrickFact>, String> {
    let shape = level_shape(case, level);
    let brick = if shape[2] == 1 {
        BRICK_2D_ZYX
    } else {
        BRICK_3D_ZYX
    };
    let mut result = Vec::new();
    for t in 0..shape[0] {
        for layer in 0..shape[1] {
            let bytes = &layers[layer as usize];
            for bz in 0..shape[2].div_ceil(brick[0]) {
                for by in 0..shape[3].div_ceil(brick[1]) {
                    for bx in 0..shape[4].div_ceil(brick[2]) {
                        let origin = [bz * brick[0], by * brick[1], bx * brick[2]];
                        let extent = [
                            brick[0].min(shape[2] - origin[0]),
                            brick[1].min(shape[3] - origin[1]),
                            brick[2].min(shape[4] - origin[2]),
                        ];
                        let mut valid_count = 0u64;
                        let mut nonfill_valid_count = 0u64;
                        let mut minimum = None;
                        let mut maximum = None;
                        for z in origin[0]..origin[0] + extent[0] {
                            for y in origin[1]..origin[1] + extent[1] {
                                for x in origin[2]..origin[2] + extent[2] {
                                    let ordinal = layer_ordinal(shape, t, z, y, x);
                                    if validity_at(&bytes.validity, ordinal) {
                                        valid_count += 1;
                                        let sample = sample_from_bytes(
                                            case.dtype,
                                            &bytes.raw_values,
                                            ordinal,
                                        );
                                        if sample_is_nonfill(sample) {
                                            nonfill_valid_count += 1;
                                        }
                                        if minimum.is_none_or(|value| sample_less(sample, value)) {
                                            minimum = Some(sample);
                                        }
                                        if maximum.is_none_or(|value| sample_less(value, sample)) {
                                            maximum = Some(sample);
                                        }
                                    }
                                }
                            }
                        }
                        result.push(BrickFact {
                            t,
                            layer,
                            z: bz,
                            y: by,
                            x: bx,
                            extent,
                            voxels: extent.into_iter().product(),
                            valid: valid_count,
                            nonfill_valid: nonfill_valid_count,
                            minimum,
                            maximum,
                        });
                    }
                }
            }
        }
    }
    Ok(result)
}

fn update_u32(hasher: &mut Sha256, value: u32) {
    hasher.update(&value.to_be_bytes());
}

fn update_u64(hasher: &mut Sha256, value: u64) {
    hasher.update(&value.to_be_bytes());
}

fn hash_node(level: u32, children: &[Digest]) -> Result<Digest, String> {
    if children.is_empty() || children.len() > MERKLE_ARITY {
        return Err("invalid Merkle child count".into());
    }
    let mut hasher = Sha256::new();
    hasher.update(NODE_DOMAIN);
    update_u32(&mut hasher, level);
    update_u32(
        &mut hasher,
        u32::try_from(children.len()).map_err(|_| "Merkle child count overflow")?,
    );
    for child in children {
        hasher.update(child);
    }
    Ok(hasher.finalize())
}

fn merkle_root(mut leaves: Vec<Digest>) -> Result<Digest, String> {
    if leaves.is_empty() {
        return Err("Merkle tree cannot be empty".into());
    }
    if leaves.len() == 1 {
        return Ok(leaves[0]);
    }
    let mut level = 1u32;
    while leaves.len() > 1 {
        let mut parents = Vec::with_capacity(leaves.len().div_ceil(MERKLE_ARITY));
        for children in leaves.chunks(MERKLE_ARITY) {
            parents.push(hash_node(level, children)?);
        }
        leaves = parents;
        level = level
            .checked_add(1)
            .ok_or_else(|| "Merkle level overflow".to_owned())?;
    }
    Ok(leaves[0])
}

fn canonical_matrix(case: &Case, mutation: Mutation) -> Result<Vec<String>, String> {
    let mut values = case.matrix_hex.clone();
    if matches!(mutation, Mutation::TransformBit) {
        let bits = parse_hex_u64(&values[0])? ^ 1;
        let number = f64::from_bits(bits);
        if !number.is_finite() {
            return Err("transform mutation became non-finite".into());
        }
        values[0] = format!("{bits:016x}");
    }
    Ok(values)
}

fn scientific_layer_root(case: &Case, layer: u64, mutation: Mutation) -> Result<Digest, String> {
    let shape = level_shape(case, 0);
    let tile_counts = [
        shape[0].div_ceil(TILE_SHAPE_TZYX[0]),
        shape[2].div_ceil(TILE_SHAPE_TZYX[1]),
        shape[3].div_ceil(TILE_SHAPE_TZYX[2]),
        shape[4].div_ceil(TILE_SHAPE_TZYX[3]),
    ];
    let tile_count = tile_counts.into_iter().product::<u64>();
    let mut tile_digests =
        Vec::with_capacity(usize::try_from(tile_count).map_err(|_| "tile count overflow")?);
    for tt in 0..tile_counts[0] {
        for tz in 0..tile_counts[1] {
            for ty in 0..tile_counts[2] {
                for tx in 0..tile_counts[3] {
                    let origin = [
                        tt * TILE_SHAPE_TZYX[0],
                        tz * TILE_SHAPE_TZYX[1],
                        ty * TILE_SHAPE_TZYX[2],
                        tx * TILE_SHAPE_TZYX[3],
                    ];
                    let extent = [
                        TILE_SHAPE_TZYX[0].min(shape[0] - origin[0]),
                        TILE_SHAPE_TZYX[1].min(shape[2] - origin[1]),
                        TILE_SHAPE_TZYX[2].min(shape[3] - origin[2]),
                        TILE_SHAPE_TZYX[3].min(shape[4] - origin[3]),
                    ];
                    let voxels = extent.into_iter().product::<u64>();
                    let mut validity = vec![
                        0u8;
                        usize::try_from(voxels.div_ceil(8))
                            .map_err(|_| "tile validity too large")?
                    ];
                    let mut values = Vec::with_capacity(
                        usize::try_from(voxels)
                            .map_err(|_| "tile too large")?
                            .checked_mul(case.dtype.bytes())
                            .ok_or_else(|| "tile byte overflow".to_owned())?,
                    );
                    let mut ordinal = 0usize;
                    for t in origin[0]..origin[0] + extent[0] {
                        for z in origin[1]..origin[1] + extent[1] {
                            for y in origin[2]..origin[2] + extent[2] {
                                for x in origin[3]..origin[3] + extent[3] {
                                    let coordinate = [t, layer, z, y, x];
                                    let valid = sample_valid(case, 0, coordinate, mutation);
                                    if valid {
                                        validity[ordinal / 8] |= 1 << (ordinal % 8);
                                    }
                                    values.extend(
                                        raw_sample(case, 0, coordinate, mutation)
                                            .canonical_bytes(valid),
                                    );
                                    ordinal += 1;
                                }
                            }
                        }
                    }
                    let mut hasher = Sha256::new();
                    hasher.update(TILE_DOMAIN);
                    update_u32(
                        &mut hasher,
                        u32::try_from(layer).map_err(|_| "layer exceeds u32")?,
                    );
                    hasher.update(&[case.dtype.tag()]);
                    for coordinate in origin {
                        update_u64(&mut hasher, coordinate);
                    }
                    for value in extent {
                        update_u64(&mut hasher, value);
                    }
                    update_u64(&mut hasher, validity.len() as u64);
                    hasher.update(&validity);
                    update_u64(&mut hasher, values.len() as u64);
                    hasher.update(&values);
                    tile_digests.push(hasher.finalize());
                }
            }
        }
    }
    let tree = merkle_root(tile_digests)?;
    let mut hasher = Sha256::new();
    hasher.update(LAYER_DOMAIN);
    update_u32(
        &mut hasher,
        u32::try_from(layer).map_err(|_| "layer exceeds u32")?,
    );
    hasher.update(&[case.dtype.tag()]);
    for dimension in [shape[0], shape[2], shape[3], shape[4]] {
        update_u64(&mut hasher, dimension);
    }
    hasher.update(&[1]);
    hasher.update(case.temporal_step_hex.as_bytes());
    for encoded in canonical_matrix(case, mutation)? {
        hasher.update(encoded.as_bytes());
    }
    update_u64(&mut hasher, tile_count);
    hasher.update(&tree);
    Ok(hasher.finalize())
}

fn scientific_content(case: &Case, mutation: Mutation) -> Result<(Vec<Digest>, String), String> {
    let mut roots = Vec::with_capacity(case.shape[1] as usize);
    for layer in 0..case.shape[1] {
        roots.push(scientific_layer_root(case, layer, mutation)?);
    }
    let mut hasher = Sha256::new();
    hasher.update(DATASET_DOMAIN);
    hasher.update(&[1, 1, 1, 1]);
    update_u32(
        &mut hasher,
        u32::try_from(case.shape[1]).map_err(|_| "layer count exceeds u32")?,
    );
    for (layer, root) in roots.iter().enumerate() {
        update_u32(&mut hasher, layer as u32);
        hasher.update(root);
    }
    Ok((
        roots,
        format!("m4d-sc-v1-sha256:{}", digest_hex(hasher.finalize())),
    ))
}

#[derive(Clone, Debug)]
struct SelectedFact {
    level: u32,
    coordinate: [u64; 5],
    physical_channel: u64,
    valid: bool,
    raw: Sample,
    canonical: Sample,
}

fn selected_facts(case: &Case, level: u32) -> Vec<SelectedFact> {
    let shape = level_shape(case, level);
    let mut coordinates = BTreeSet::new();
    for layer in 0..shape[1] {
        coordinates.insert([0, layer, 0, 0, 0]);
        coordinates.insert([
            shape[0] / 2,
            layer,
            shape[2] / 2,
            shape[3] / 2,
            shape[4] / 2,
        ]);
        coordinates.insert([
            shape[0] - 1,
            layer,
            shape[2] - 1,
            shape[3] - 1,
            shape[4] - 1,
        ]);
    }
    if level == 0 {
        if let ValueRule::Sparse(points) = &case.value_rule {
            coordinates.extend(points.keys().copied());
        }
        if matches!(case.value_rule, ValueRule::F32Cycle(_)) {
            for x in 0..16.min(shape[4]) {
                coordinates.insert([0, 0, 0, 0, x]);
            }
        }
    }
    coordinates
        .into_iter()
        .map(|coordinate| {
            let raw = raw_sample(case, level, coordinate, Mutation::None);
            let valid = sample_valid(case, level, coordinate, Mutation::None);
            let canonical = if valid {
                raw
            } else {
                match raw {
                    Sample::Uint8(_) => Sample::Uint8(0),
                    Sample::Uint16(_) => Sample::Uint16(0),
                    Sample::Float32(_) => Sample::Float32(0),
                }
            };
            SelectedFact {
                level,
                coordinate,
                physical_channel: case.physical_channels[coordinate[1] as usize],
                valid,
                raw,
                canonical,
            }
        })
        .collect()
}

#[derive(Clone, Debug)]
struct LevelFacts {
    ordinal: u32,
    shape: [u64; 5],
    raw_values_digest: Digest,
    canonical_values_digest: Digest,
    validity_digest: Digest,
    layer_digests: Vec<(Digest, Digest, Digest)>,
    bricks: Vec<BrickFact>,
    selected: Vec<SelectedFact>,
}

fn compute_level(case: &Case, level: u32) -> Result<LevelFacts, String> {
    let mut raw_all = Sha256::new();
    let mut canonical_all = Sha256::new();
    let mut validity_all = Sha256::new();
    let mut layer_digests = Vec::new();
    let mut layers = Vec::new();
    for layer in 0..case.shape[1] {
        let bytes = build_layer_bytes(case, level, layer, Mutation::None)?;
        let raw = sha256(&bytes.raw_values);
        let canonical = sha256(&bytes.canonical_values);
        let validity = sha256(&bytes.validity);
        raw_all.update(&bytes.raw_values);
        canonical_all.update(&bytes.canonical_values);
        validity_all.update(&bytes.validity);
        layer_digests.push((raw, canonical, validity));
        layers.push(bytes);
    }
    Ok(LevelFacts {
        ordinal: level,
        shape: level_shape(case, level),
        raw_values_digest: raw_all.finalize(),
        canonical_values_digest: canonical_all.finalize(),
        validity_digest: validity_all.finalize(),
        layer_digests,
        bricks: brick_facts(case, level, &layers)?,
        selected: selected_facts(case, level),
    })
}

fn json_quote(value: &str) -> String {
    let mut result = String::with_capacity(value.len() + 2);
    result.push('"');
    for character in value.chars() {
        match character {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            value if value < ' ' => write!(result, "\\u{:04x}", value as u32).expect("string"),
            value => result.push(value),
        }
    }
    result.push('"');
    result
}

fn json_u64_array<const N: usize>(values: [u64; N]) -> String {
    format!(
        "[{}]",
        values
            .into_iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn json_string_array(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| json_quote(value))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn sample_option_json(value: Option<Sample>) -> String {
    value
        .map(Sample::json_value)
        .unwrap_or_else(|| "null".into())
}

fn level_json(case: &Case, facts: &LevelFacts) -> String {
    let layers = facts
        .layer_digests
        .iter()
        .enumerate()
        .map(|(layer, (raw, canonical, validity))| {
            format!(
                "{{\"logical_layer\":{layer},\"physical_channel\":{},\"raw_values_sha256\":\"{}\",\"canonical_values_sha256\":\"{}\",\"validity_sha256\":\"{}\"}}",
                case.physical_channels[layer],
                digest_hex(*raw),
                digest_hex(*canonical),
                digest_hex(*validity),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let bricks = facts
        .bricks
        .iter()
        .map(|brick| {
            format!(
                "{{\"t\":{},\"logical_layer\":{},\"brick_zyx\":[{},{},{}],\"extent_zyx\":{},\"voxel_count\":{},\"valid_count\":{},\"nonfill_valid_count\":{},\"minimum\":{},\"maximum\":{}}}",
                brick.t,
                brick.layer,
                brick.z,
                brick.y,
                brick.x,
                json_u64_array(brick.extent),
                brick.voxels,
                brick.valid,
                brick.nonfill_valid,
                sample_option_json(brick.minimum),
                sample_option_json(brick.maximum),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let selected = facts
        .selected
        .iter()
        .map(|fact| {
            format!(
                "{{\"level\":{},\"coordinate_tczyx\":{},\"physical_channel\":{},\"valid\":{},\"raw_value\":{},\"canonical_value\":{}}}",
                fact.level,
                json_u64_array(fact.coordinate),
                fact.physical_channel,
                fact.valid,
                fact.raw.json_value(),
                fact.canonical.json_value(),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"ordinal\":{},\"shape_tczyx\":{},\"raw_values_sha256\":\"{}\",\"canonical_values_sha256\":\"{}\",\"validity_sha256\":\"{}\",\"layers\":[{}],\"selected_facts\":[{}],\"brick_statistics\":[{}]}}",
        facts.ordinal,
        json_u64_array(facts.shape),
        digest_hex(facts.raw_values_digest),
        digest_hex(facts.canonical_values_digest),
        digest_hex(facts.validity_digest),
        layers,
        selected,
        bricks,
    )
}

fn case_json(case: &Case) -> Result<String, String> {
    let levels = (0..case.levels)
        .map(|level| compute_level(case, level))
        .collect::<Result<Vec<_>, _>>()?;
    let (layer_roots, scientific_id) = scientific_content(case, Mutation::None)?;
    let value_coordinate = if case.dtype == DType::Float32 {
        [0, 0, 0, 1]
    } else {
        [0, 0, 0, 0]
    };
    let validity_coordinate = if case.dtype == DType::Float32 {
        [0, 0, 0, 1]
    } else {
        [0, 0, 0, 0]
    };
    let (_, value_mutation) = scientific_content(
        case,
        Mutation::ValueBit {
            case_layer: 0,
            coordinate_tzyx: value_coordinate,
        },
    )?;
    let (_, validity_mutation) = scientific_content(
        case,
        Mutation::ValidityBit {
            case_layer: 0,
            coordinate_tzyx: validity_coordinate,
        },
    )?;
    let (_, transform_mutation) = scientific_content(case, Mutation::TransformBit)?;
    for (label, value) in [
        ("value", &value_mutation),
        ("validity", &validity_mutation),
        ("transform", &transform_mutation),
    ] {
        if value == &scientific_id {
            return Err(format!(
                "case {} {label} mutation did not change scientific identity",
                case.id
            ));
        }
    }
    let mapping = case
        .physical_channels
        .iter()
        .enumerate()
        .map(|(logical, physical)| {
            format!("{{\"logical_layer\":{logical},\"physical_channel\":{physical}}}")
        })
        .collect::<Vec<_>>()
        .join(",");
    let ome_levels = case
        .ome_scales_hex
        .iter()
        .zip(&case.ome_translations_hex)
        .enumerate()
        .map(|(ordinal, (scale, translation))| {
            format!(
                "{{\"ordinal\":{ordinal},\"scale_zyx_f64_bits\":{},\"translation_zyx_f64_bits\":{}}}",
                json_string_array(scale),
                json_string_array(translation),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let roots = layer_roots
        .iter()
        .enumerate()
        .map(|(layer, digest)| {
            format!(
                "{{\"logical_layer\":{layer},\"sha256\":\"{}\"}}",
                digest_hex(*digest)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let level_json = levels
        .iter()
        .map(|facts| level_json(case, facts))
        .collect::<Vec<_>>()
        .join(",");
    let metamorphic = format!(
        concat!(
            "{{",
            "\"recompression\":{{\"expectation\":\"same\",\"scientific_content_id\":{}}},",
            "\"resharding\":{{\"expectation\":\"same\",\"scientific_content_id\":{}}},",
            "\"physical_channel_reordering\":{{\"expectation\":\"same\",\"scientific_content_id\":{}}},",
            "\"equivalent_validity_representation\":{{\"expectation\":\"same\",\"scientific_content_id\":{}}},",
            "\"provenance_or_display_change\":{{\"expectation\":\"same\",\"scientific_content_id\":{}}},",
            "\"one_bit_value\":{{\"expectation\":\"different\",\"coordinate_layer_tzyx\":[0,{},{},{},{}],\"scientific_content_id\":{}}},",
            "\"one_bit_validity\":{{\"expectation\":\"different\",\"coordinate_layer_tzyx\":[0,{},{},{},{}],\"scientific_content_id\":{}}},",
            "\"one_bit_transform\":{{\"expectation\":\"different\",\"matrix_index\":0,\"scientific_content_id\":{}}}",
            "}}"
        ),
        json_quote(&scientific_id),
        json_quote(&scientific_id),
        json_quote(&scientific_id),
        json_quote(&scientific_id),
        json_quote(&scientific_id),
        value_coordinate[0],
        value_coordinate[1],
        value_coordinate[2],
        value_coordinate[3],
        json_quote(&value_mutation),
        validity_coordinate[0],
        validity_coordinate[1],
        validity_coordinate[2],
        validity_coordinate[3],
        json_quote(&validity_mutation),
        json_quote(&transform_mutation),
    );
    Ok(format!(
        concat!(
            "{{",
            "\"case_id\":{},\"dtype\":{},\"shape_tczyx\":{},\"level_count\":{},",
            "\"validity_mode\":{},\"physical_mapping\":[{}],",
            "\"temporal_step_f64_bits\":{},\"grid_to_world_f64_bits\":{},",
            "\"ome_projection\":{},\"ome_levels\":[{}],",
            "\"levels\":[{}],\"scientific_layer_roots\":[{}],",
            "\"scientific_content_id\":{},\"metamorphic\":{}",
            "}}"
        ),
        json_quote(&case.id),
        json_quote(case.dtype.name()),
        json_u64_array(case.shape),
        case.levels,
        json_quote(&case.validity_name),
        mapping,
        json_quote(&case.temporal_step_hex),
        json_string_array(&case.matrix_hex),
        json_quote(&case.ome_projection),
        ome_levels,
        level_json,
        roots,
        json_quote(&scientific_id),
        metamorphic,
    ))
}

fn build_report(cases: &[Case], spec_digest: Digest) -> Result<String, String> {
    let mut encoded_cases = Vec::with_capacity(cases.len());
    let mut logical_voxels = 0u64;
    for case in cases {
        for level in 0..case.levels {
            logical_voxels = logical_voxels
                .checked_add(voxel_count(level_shape(case, level))?)
                .ok_or_else(|| "corpus voxel count overflow".to_owned())?;
        }
        encoded_cases.push(case_json(case)?);
    }
    Ok(format!(
        concat!(
            "{{\n",
            "  \"schema\": \"mirante4d-target-t1-independent-facts\",\n",
            "  \"schema_version\": 1,\n",
            "  \"lineage_id\": \"TGT-FACT-001\",\n",
            "  \"dependencies\": [],\n",
            "  \"spec_sha256\": \"{}\",\n",
            "  \"case_count\": {},\n",
            "  \"logical_voxel_count_all_levels\": {},\n",
            "  \"cases\": [{}]\n",
            "}}\n"
        ),
        digest_hex(spec_digest),
        cases.len(),
        logical_voxels,
        encoded_cases.join(","),
    ))
}

fn self_test() -> Result<(), String> {
    let published_sha = [
        (
            b"".as_slice(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            b"abc".as_slice(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
    ];
    for (input, expected) in published_sha {
        if digest_hex(sha256(input)) != expected {
            return Err("independent SHA-256 failed a FIPS vector".into());
        }
    }
    let merkle_vectors = [
        (
            1usize,
            "af5570f5a1810b7af78caf4bc70a660f0df51e42baf91d4de5b2328de0e83dfc",
        ),
        (
            1023,
            "8fbc67fcf2006a1b4a7e490a741c1ef87f922619525471730a1bcf1922adc4e0",
        ),
        (
            1024,
            "e5acecfbb30a6a36d183c6dc02f069fd98d07c6fa5e052d6efb94fbec81ca05b",
        ),
        (
            1025,
            "4ecfc5c533a154d369ffad4004dd8133c18c7e8d26b4e3a0d1ec97c91bbacc8a",
        ),
    ];
    for (count, expected) in merkle_vectors {
        let leaves = (0..count)
            .map(|index| sha256(&(index as u64).to_be_bytes()))
            .collect::<Vec<_>>();
        if digest_hex(merkle_root(leaves)?) != expected {
            return Err(format!(
                "independent Merkle implementation failed the {count} vector"
            ));
        }
    }
    published_one_voxel_check()?;
    Ok(())
}

fn published_one_voxel_check() -> Result<(), String> {
    let mut tile = Sha256::new();
    tile.update(TILE_DOMAIN);
    update_u32(&mut tile, 0);
    tile.update(&[1]);
    for _ in 0..4 {
        update_u64(&mut tile, 0);
    }
    for _ in 0..4 {
        update_u64(&mut tile, 1);
    }
    update_u64(&mut tile, 1);
    tile.update(&[1]);
    update_u64(&mut tile, 1);
    tile.update(&[7]);
    let tree = merkle_root(vec![tile.finalize()])?;

    let mut layer = Sha256::new();
    layer.update(LAYER_DOMAIN);
    update_u32(&mut layer, 0);
    layer.update(&[1]);
    for _ in 0..4 {
        update_u64(&mut layer, 1);
    }
    layer.update(&[0]);
    for value in [
        "3ff0000000000000",
        "0000000000000000",
        "0000000000000000",
        "0000000000000000",
        "0000000000000000",
        "3ff0000000000000",
        "0000000000000000",
        "0000000000000000",
        "0000000000000000",
        "0000000000000000",
        "3ff0000000000000",
        "0000000000000000",
        "0000000000000000",
        "0000000000000000",
        "0000000000000000",
        "3ff0000000000000",
    ] {
        layer.update(value.as_bytes());
    }
    update_u64(&mut layer, 1);
    layer.update(&tree);
    let layer_root = layer.finalize();
    if digest_hex(layer_root) != "a2ef9d5a469e4934434ef7dcb9df27a63dce43df2c6e31d77a4e894cfaf24e4f"
    {
        return Err("independent one-voxel layer vector failed".into());
    }

    let mut dataset = Sha256::new();
    dataset.update(DATASET_DOMAIN);
    dataset.update(&[1, 1, 1, 1]);
    update_u32(&mut dataset, 1);
    update_u32(&mut dataset, 0);
    dataset.update(&layer_root);
    if digest_hex(dataset.finalize())
        != "1dd0a7a4ce0561326783f5cdf7b6eeff476a9628061d35c705f4af2c863ad392"
    {
        return Err("independent one-voxel dataset vector failed".into());
    }
    Ok(())
}

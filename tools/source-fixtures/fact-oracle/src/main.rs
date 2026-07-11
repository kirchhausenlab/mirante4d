use std::{env, fs, path::Path};

const HEADER: &str = "kind|spec_id|path|dtype|pages|height|width|t|c|z_start|value_rule|rows_per_strip|expected_class|shape_tczyx|calibration_xyz_um|grouping_id";
const SPEC_IDS: [&str; 4] = [
    "SRC-TIFF-SPEC-001",
    "SRC-TIFF-SPEC-002",
    "SRC-TIFF-SPEC-003",
    "SRC-TIFF-SPEC-004",
];
const FINITE_F32_BITS: [u32; 12] = [
    0xbfc0_0000,
    0x0000_0000,
    0x3e80_0000,
    0x3f80_0000,
    0x4000_0000,
    0x4040_0000,
    0x4120_0000,
    0x4138_0000,
    0x4144_0000,
    0x4150_0000,
    0x4168_0000,
    0x417c_0000,
];
const NONFINITE_F32_BITS: [u32; 6] = [
    0x0000_0000,
    0x8000_0000,
    0x3f80_0000,
    0x7fc0_0000,
    0x7f80_0000,
    0xff80_0000,
];

#[derive(Clone)]
struct Family {
    id: String,
    shape: Option<[u64; 5]>,
    calibration: Option<[String; 3]>,
    grouping_id: String,
}

#[derive(Clone)]
struct FileSpec {
    spec_id: String,
    path: String,
    dtype: String,
    pages: u64,
    height: u64,
    width: u64,
    t: u64,
    c: u64,
    z_start: u64,
    rule: String,
    rows_per_strip: String,
    expected_class: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let spec = args
        .next()
        .ok_or("usage: fact-oracle <v1.tsv> <facts.json>")?;
    let output = args
        .next()
        .ok_or("usage: fact-oracle <v1.tsv> <facts.json>")?;
    if args.next().is_some() {
        return Err("usage: fact-oracle <v1.tsv> <facts.json>".into());
    }
    let (families, mut files) = parse_spec(Path::new(&spec))?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let encoded = expected_facts_json(&families, &files)?;
    fs::write(output, encoded)?;
    Ok(())
}

fn parse_spec(path: &Path) -> Result<(Vec<Family>, Vec<FileSpec>), Box<dyn std::error::Error>> {
    let text = fs::read_to_string(path)?;
    let mut lines = text.lines();
    if lines.next() != Some(HEADER) {
        return Err("unexpected source-fixture specification header".into());
    }
    let mut families = Vec::new();
    let mut files = Vec::new();
    for (index, line) in lines.enumerate() {
        if line.is_empty() {
            return Err(format!("empty specification row at line {}", index + 2).into());
        }
        let fields = line.split('|').collect::<Vec<_>>();
        if fields.len() != 16 {
            return Err(format!("line {} has {} fields", index + 2, fields.len()).into());
        }
        match fields[0] {
            "family" => {
                let values = fields[13]
                    .split(',')
                    .map(str::parse::<u64>)
                    .collect::<Result<Vec<_>, _>>()?;
                if values.len() != 5 {
                    return Err("family shape must have five t,c,z,y,x values".into());
                }
                let shape = if values.contains(&0) {
                    None
                } else {
                    Some([values[0], values[1], values[2], values[3], values[4]])
                };
                let calibration = if fields[14].is_empty() {
                    None
                } else {
                    let values = fields[14].split(',').map(str::to_owned).collect::<Vec<_>>();
                    if values.len() != 3 {
                        return Err("calibration must have three x,y,z values".into());
                    }
                    Some([values[0].clone(), values[1].clone(), values[2].clone()])
                };
                families.push(Family {
                    id: fields[1].to_owned(),
                    shape,
                    calibration,
                    grouping_id: fields[15].to_owned(),
                });
            }
            "file" => files.push(FileSpec {
                spec_id: fields[1].to_owned(),
                path: fields[2].to_owned(),
                dtype: fields[3].to_owned(),
                pages: fields[4].parse()?,
                height: fields[5].parse()?,
                width: fields[6].parse()?,
                t: fields[7].parse()?,
                c: fields[8].parse()?,
                z_start: fields[9].parse()?,
                rule: fields[10].to_owned(),
                rows_per_strip: fields[11].to_owned(),
                expected_class: fields[12].to_owned(),
            }),
            other => return Err(format!("unknown specification row kind {other:?}").into()),
        }
    }
    if families.len() != 4 || files.len() != 16 {
        return Err("v1 specification must contain four families and sixteen files".into());
    }
    for (index, expected) in SPEC_IDS.iter().enumerate() {
        if families.get(index).map(|family| family.id.as_str()) != Some(*expected) {
            return Err("family IDs or ordering differ from approved v1".into());
        }
    }
    let mut paths = files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths.dedup();
    if paths.len() != files.len() {
        return Err("duplicate source-fixture file path".into());
    }
    Ok((families, files))
}

fn expected_facts_json(
    families: &[Family],
    files: &[FileSpec],
) -> Result<String, Box<dyn std::error::Error>> {
    let mut global_payload = Vec::new();
    let mut payloads = Vec::new();
    for file in files {
        let payload = logical_payload(file)?;
        global_payload.extend_from_slice(&payload);
        payloads.push((file, payload));
    }

    let mut output = String::new();
    output.push_str("{\n");
    output.push_str("  \"schema\": \"mirante4d-source-fixture-expected-facts\",\n");
    output.push_str("  \"schema_version\": 1,\n");
    output.push_str("  \"fact_authority\": \"SRC-FACT-001\",\n");
    output.push_str("  \"axes\": [\"t\", \"c\", \"z\", \"y\", \"x\"],\n");
    output.push_str("  \"logical_value_digest_algorithm\": \"sha256\",\n");
    output.push_str(&format!(
        "  \"logical_value_sha256\": \"{}\",\n",
        sha256_hex(&global_payload)
    ));
    output.push_str(&format!(
        "  \"logical_voxel_bytes\": {},\n",
        global_payload.len()
    ));
    output.push_str("  \"specifications\": [\n");

    for (family_index, family) in families.iter().enumerate() {
        let family_files = payloads
            .iter()
            .filter(|(file, _)| file.spec_id == family.id)
            .collect::<Vec<_>>();
        output.push_str("    {\n");
        output.push_str(&format!("      \"id\": {},\n", json_string(&family.id)));
        match family.shape {
            Some(shape) => output.push_str(&format!(
                "      \"shape_tczyx\": [{}, {}, {}, {}, {}],\n",
                shape[0], shape[1], shape[2], shape[3], shape[4]
            )),
            None => output.push_str("      \"shape_tczyx\": null,\n"),
        }
        match &family.calibration {
            Some(values) => output.push_str(&format!(
                "      \"calibration_xyz_um\": [{}, {}, {}],\n",
                values[0], values[1], values[2]
            )),
            None => output.push_str("      \"calibration_xyz_um\": null,\n"),
        }
        output.push_str(&format!(
            "      \"grouping_id\": {},\n",
            json_string(&family.grouping_id)
        ));
        output.push_str(&format!("      \"path_count\": {},\n", family_files.len()));
        output.push_str("      \"files\": [\n");
        for (file_index, (file, payload)) in family_files.iter().enumerate() {
            output.push_str(&file_fact_json(file, payload, 8)?);
            if file_index + 1 != family_files.len() {
                output.push(',');
            }
            output.push('\n');
        }
        output.push_str("      ]\n");
        output.push_str("    }");
        if family_index + 1 != families.len() {
            output.push(',');
        }
        output.push('\n');
    }
    output.push_str("  ]\n");
    output.push_str("}\n");
    Ok(output)
}

fn file_fact_json(
    file: &FileSpec,
    payload: &[u8],
    indent: usize,
) -> Result<String, Box<dyn std::error::Error>> {
    let prefix = " ".repeat(indent);
    let bits_per_value = bytes_per_value(&file.dtype)? * 8;
    let first_bits = value_bits_hex(payload, &file.dtype, 0)?;
    let value_count = (file.pages * file.height * file.width) as usize;
    let last_bits = value_bits_hex(payload, &file.dtype, value_count - 1)?;
    let (minimum, maximum) = min_max(file, payload)?;
    let rows_per_strip = if file.rows_per_strip == "full" {
        file.height.to_string()
    } else {
        file.rows_per_strip.clone()
    };
    let mut output = String::new();
    output.push_str(&format!("{prefix}{{\n"));
    output.push_str(&format!(
        "{prefix}  \"path\": {},\n",
        json_string(&file.path)
    ));
    output.push_str(&format!(
        "{prefix}  \"dtype\": {},\n",
        json_string(&file.dtype)
    ));
    output.push_str(&format!("{prefix}  \"ifd_count\": {},\n", file.pages));
    output.push_str(&format!("{prefix}  \"width\": {},\n", file.width));
    output.push_str(&format!("{prefix}  \"height\": {},\n", file.height));
    output.push_str(&format!(
        "{prefix}  \"rows_per_strip\": {},\n",
        rows_per_strip
    ));
    output.push_str(&format!(
        "{prefix}  \"bits_per_value\": {},\n",
        bits_per_value
    ));
    output.push_str(&format!(
        "{prefix}  \"expected_class\": {},\n",
        json_string(&file.expected_class)
    ));
    output.push_str(&format!(
        "{prefix}  \"logical_bytes\": {},\n",
        payload.len()
    ));
    output.push_str(&format!(
        "{prefix}  \"logical_value_sha256\": \"{}\",\n",
        sha256_hex(payload)
    ));
    output.push_str(&format!(
        "{prefix}  \"minimum\": {},\n",
        minimum
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".to_owned())
    ));
    output.push_str(&format!(
        "{prefix}  \"maximum\": {},\n",
        maximum
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".to_owned())
    ));
    output.push_str(&format!(
        "{prefix}  \"selected_values\": [\n{prefix}    {{\"coordinate_pyx\": [0, 0, 0], \"bits_hex\": {}}},\n{prefix}    {{\"coordinate_pyx\": [{}, {}, {}], \"bits_hex\": {}}}\n{prefix}  ]\n",
        json_string(&first_bits),
        file.pages - 1,
        file.height - 1,
        file.width - 1,
        json_string(&last_bits)
    ));
    output.push_str(&format!("{prefix}}}"));
    Ok(output)
}

fn logical_payload(file: &FileSpec) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut payload = Vec::new();
    let mut explicit_index = 0usize;
    for page in 0..file.pages {
        let z = file.z_start + page;
        for y in 0..file.height {
            for x in 0..file.width {
                match file.rule.as_str() {
                    "spec001_u16" => push_u16(&mut payload, 10 * z + 3 * y + x)?,
                    "spec002_u16" => push_u16(&mut payload, 100 * file.t + 20 * z + 4 * y + x)?,
                    "spec003_u8" => push_u8(&mut payload, 100 * file.c + 20 * z + 4 * y + x)?,
                    "spec004_u8_no_data" => push_u8(
                        &mut payload,
                        if (z, y, x) == (0, 0, 0) {
                            255
                        } else {
                            9 * z + 3 * y + x
                        },
                    )?,
                    "spec004_f32_finite" => {
                        payload.extend_from_slice(&FINITE_F32_BITS[explicit_index].to_le_bytes());
                        explicit_index += 1;
                    }
                    "spec004_u16_striped" => push_u16(&mut payload, 100 * z + 10 * y + x)?,
                    "spec004_u16_zero" => push_u16(&mut payload, 0)?,
                    "spec004_u32_sequence" => push_u32(&mut payload, y * file.width + x)?,
                    "spec004_f32_nonfinite" => {
                        payload
                            .extend_from_slice(&NONFINITE_F32_BITS[explicit_index].to_le_bytes());
                        explicit_index += 1;
                    }
                    other => return Err(format!("unknown value rule {other:?}").into()),
                }
            }
        }
    }
    let expected = (file.pages * file.height * file.width) as usize * bytes_per_value(&file.dtype)?;
    if payload.len() != expected {
        return Err(format!("payload size mismatch for {}", file.path).into());
    }
    Ok(payload)
}

fn push_u8(output: &mut Vec<u8>, value: u64) -> Result<(), Box<dyn std::error::Error>> {
    output.push(u8::try_from(value)?);
    Ok(())
}

fn push_u16(output: &mut Vec<u8>, value: u64) -> Result<(), Box<dyn std::error::Error>> {
    output.extend_from_slice(&u16::try_from(value)?.to_le_bytes());
    Ok(())
}

fn push_u32(output: &mut Vec<u8>, value: u64) -> Result<(), Box<dyn std::error::Error>> {
    output.extend_from_slice(&u32::try_from(value)?.to_le_bytes());
    Ok(())
}

fn bytes_per_value(dtype: &str) -> Result<usize, Box<dyn std::error::Error>> {
    Ok(match dtype {
        "u8" => 1,
        "u16" => 2,
        "u32" | "f32" => 4,
        other => return Err(format!("unsupported dtype {other:?}").into()),
    })
}

fn value_bits_hex(
    payload: &[u8],
    dtype: &str,
    index: usize,
) -> Result<String, Box<dyn std::error::Error>> {
    let bytes = bytes_per_value(dtype)?;
    let start = index.checked_mul(bytes).ok_or("value offset overflow")?;
    let value = payload
        .get(start..start + bytes)
        .ok_or("value index out of range")?;
    Ok(value
        .iter()
        .rev()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn min_max(
    file: &FileSpec,
    payload: &[u8],
) -> Result<(Option<String>, Option<String>), Box<dyn std::error::Error>> {
    match file.dtype.as_str() {
        "u8" => {
            let min = payload.iter().min().ok_or("empty u8 payload")?;
            let max = payload.iter().max().ok_or("empty u8 payload")?;
            Ok((Some(min.to_string()), Some(max.to_string())))
        }
        "u16" => {
            let values = payload
                .chunks_exact(2)
                .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]));
            integer_min_max(values)
        }
        "u32" => {
            let values = payload
                .chunks_exact(4)
                .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
            integer_min_max(values)
        }
        "f32" => {
            let values = payload.chunks_exact(4).map(|bytes| {
                f32::from_bits(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            });
            let values = values.collect::<Vec<_>>();
            if values.iter().any(|value| !value.is_finite()) {
                return Ok((None, None));
            }
            let min = values
                .iter()
                .copied()
                .reduce(f32::min)
                .ok_or("empty f32 payload")?;
            let max = values
                .iter()
                .copied()
                .reduce(f32::max)
                .ok_or("empty f32 payload")?;
            Ok((Some(format_float(min)), Some(format_float(max))))
        }
        other => Err(format!("unsupported dtype {other:?}").into()),
    }
}

fn integer_min_max<T>(
    values: impl Iterator<Item = T>,
) -> Result<(Option<String>, Option<String>), Box<dyn std::error::Error>>
where
    T: Ord + ToString + Copy,
{
    let values = values.collect::<Vec<_>>();
    let min = values.iter().min().ok_or("empty integer payload")?;
    let max = values.iter().max().ok_or("empty integer payload")?;
    Ok((Some(min.to_string()), Some(max.to_string())))
}

fn format_float(value: f32) -> String {
    if value == 0.0 && value.is_sign_negative() {
        "-0.0".to_owned()
    } else if value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        value.to_string()
    }
}

fn json_string(value: &str) -> String {
    let mut output = String::from("\"");
    for character in value.chars() {
        match character {
            '\"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                output.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => output.push(character),
        }
    }
    output.push('\"');
    output
}

fn sha256_hex(input: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
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
    let bit_len = (input.len() as u64) * 8;
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    let mut state = INITIAL;
    for block in padded.chunks_exact(64) {
        let mut words = [0u32; 64];
        for (index, bytes) in block.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(sum1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
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
        for (target, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *target = target.wrapping_add(value);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    #[test]
    fn sha256_matches_standard_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

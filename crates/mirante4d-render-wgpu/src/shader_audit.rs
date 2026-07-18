//! Development-only structural evidence for the production rendering shader.
//!
//! This module deliberately reports facts that Naga can prove from the WGSL
//! and generated SPIR-V. Register allocation, spills, occupancy, cache
//! behavior, and divergence are vendor-compiler/runtime facts and remain
//! unavailable unless an external profiler supplies them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use wgpu::naga::{
    ArraySize, Block, Function, Handle, Module, Scalar, ScalarKind, ShaderStage, Statement,
    TypeInner, VectorSize,
};

const PRODUCTION_RENDER_WGSL: &str = include_str!("shader.wgsl");
const AUDIT_SCHEMA: &str = "mirante4d-ep00-production-shader-structural-audit-1";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FixedLocalArray {
    function: String,
    local: String,
    element_type: String,
    element_count: u32,
    element_layout_bytes: u32,
    stride_bytes: u32,
    conservative_private_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SpirvFacts {
    word_count: usize,
    instruction_count: usize,
    opcode_counts: BTreeMap<u16, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FragmentEntryAudit {
    entry_point: String,
    reachable_function_names: Vec<String>,
    render_mode_families: Vec<&'static str>,
    fixed_local_arrays: Vec<FixedLocalArray>,
    conservative_private_bytes_sum: u64,
    spirv: SpirvFacts,
}

#[derive(Debug)]
struct ShaderAudit {
    source_sha256: String,
    pipeline_entry_count: usize,
    fragment_entries: Vec<FragmentEntryAudit>,
}

fn parse_and_validate(source: &str) -> Result<(Module, wgpu::naga::valid::ModuleInfo), String> {
    let module =
        wgpu::naga::front::wgsl::parse_str(source).map_err(|error| error.emit_to_string(source))?;
    let mut validator = wgpu::naga::valid::Validator::new(
        wgpu::naga::valid::ValidationFlags::all(),
        wgpu::naga::valid::Capabilities::all(),
    );
    let info = validator
        .validate(&module)
        .map_err(|error| format!("WGSL validation failed: {error:#?}"))?;
    Ok((module, info))
}

fn audit_shader(source: &str) -> Result<ShaderAudit, String> {
    let (module, _) = parse_and_validate(source)?;
    let mut fragment_entries = Vec::new();
    for entry in &module.entry_points {
        if entry.stage != ShaderStage::Fragment {
            continue;
        }
        let reachable = reachable_functions(&module, &entry.function);
        let mut reachable_function_names = vec![entry.name.clone()];
        reachable_function_names.extend(reachable.iter().map(|handle| {
            module.functions[*handle]
                .name
                .clone()
                .unwrap_or_else(|| format!("<unnamed_function_{}>", handle.index()))
        }));
        reachable_function_names.sort();

        let mut fixed_local_arrays =
            fixed_arrays_in_function(&module, &entry.function, &entry.name);
        for handle in &reachable {
            let function = &module.functions[*handle];
            let function_name = function
                .name
                .clone()
                .unwrap_or_else(|| format!("<unnamed_function_{}>", handle.index()));
            fixed_local_arrays.extend(fixed_arrays_in_function(&module, function, &function_name));
        }
        fixed_local_arrays.sort();
        let conservative_private_bytes_sum = fixed_local_arrays
            .iter()
            .map(|array| array.conservative_private_bytes)
            .sum();
        let render_mode_families = render_mode_families(&reachable_function_names);
        let spirv_words = write_spirv(&module, &entry.name)?;
        let spirv = inspect_spirv(&spirv_words)?;

        fragment_entries.push(FragmentEntryAudit {
            entry_point: entry.name.clone(),
            reachable_function_names,
            render_mode_families,
            fixed_local_arrays,
            conservative_private_bytes_sum,
            spirv,
        });
    }
    fragment_entries.sort_by(|left, right| left.entry_point.cmp(&right.entry_point));

    Ok(ShaderAudit {
        source_sha256: sha256_hex(source.as_bytes()),
        pipeline_entry_count: module.entry_points.len(),
        fragment_entries,
    })
}

fn reachable_functions(module: &Module, root: &Function) -> BTreeSet<Handle<Function>> {
    let mut reachable = BTreeSet::new();
    let mut pending = BTreeSet::new();
    collect_block_calls(&root.body, &mut pending);
    while let Some(handle) = pending.pop_first() {
        if !reachable.insert(handle) {
            continue;
        }
        collect_block_calls(&module.functions[handle].body, &mut pending);
    }
    reachable
}

fn collect_block_calls(block: &Block, calls: &mut BTreeSet<Handle<Function>>) {
    for statement in block {
        match statement {
            Statement::Block(nested) => collect_block_calls(nested, calls),
            Statement::If { accept, reject, .. } => {
                collect_block_calls(accept, calls);
                collect_block_calls(reject, calls);
            }
            Statement::Switch { cases, .. } => {
                for case in cases {
                    collect_block_calls(&case.body, calls);
                }
            }
            Statement::Loop {
                body, continuing, ..
            } => {
                collect_block_calls(body, calls);
                collect_block_calls(continuing, calls);
            }
            Statement::Call { function, .. } => {
                calls.insert(*function);
            }
            _ => {}
        }
    }
}

fn fixed_arrays_in_function(
    module: &Module,
    function: &Function,
    function_name: &str,
) -> Vec<FixedLocalArray> {
    function
        .local_variables
        .iter()
        .filter_map(|(handle, local)| {
            let TypeInner::Array { base, size, stride } = module.types[local.ty].inner else {
                return None;
            };
            let ArraySize::Constant(count) = size else {
                return None;
            };
            let element_count = count.get();
            let conservative_private_bytes = u64::from(stride) * u64::from(element_count);
            Some(FixedLocalArray {
                function: function_name.to_owned(),
                local: local
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("<unnamed_local_{}>", handle.index())),
                element_type: type_label(module, base),
                element_count,
                element_layout_bytes: module.types[base].inner.size(module.to_ctx()),
                stride_bytes: stride,
                conservative_private_bytes,
            })
        })
        .collect()
}

fn type_label(module: &Module, handle: Handle<wgpu::naga::Type>) -> String {
    if let Some(name) = &module.types[handle].name {
        return name.clone();
    }
    match module.types[handle].inner {
        TypeInner::Scalar(scalar) => scalar_label(scalar),
        TypeInner::Vector { size, scalar } => {
            format!("vec{}<{}>", vector_size(size), scalar_label(scalar))
        }
        TypeInner::Matrix {
            columns,
            rows,
            scalar,
        } => format!(
            "mat{}x{}<{}>",
            vector_size(columns),
            vector_size(rows),
            scalar_label(scalar)
        ),
        TypeInner::Atomic(scalar) => format!("atomic<{}>", scalar_label(scalar)),
        TypeInner::Struct { .. } => "<anonymous_struct>".to_owned(),
        TypeInner::Array { .. } => "<nested_array>".to_owned(),
        _ => "<opaque_type>".to_owned(),
    }
}

const fn vector_size(size: VectorSize) -> u8 {
    match size {
        VectorSize::Bi => 2,
        VectorSize::Tri => 3,
        VectorSize::Quad => 4,
    }
}

fn scalar_label(scalar: Scalar) -> String {
    match scalar.kind {
        ScalarKind::Sint => format!("i{}", u16::from(scalar.width) * 8),
        ScalarKind::Uint => format!("u{}", u16::from(scalar.width) * 8),
        ScalarKind::Float => format!("f{}", u16::from(scalar.width) * 8),
        ScalarKind::Bool => "bool".to_owned(),
        ScalarKind::AbstractInt => "abstract_int".to_owned(),
        ScalarKind::AbstractFloat => "abstract_float".to_owned(),
    }
}

fn render_mode_families(reachable_function_names: &[String]) -> Vec<&'static str> {
    const MODE_ROOTS: [(&str, &[&str]); 4] = [
        ("cross_section", &["render_cross_section_layer"]),
        (
            "dvr",
            &["render_dvr_layer", "render_fused_dvr", "render_general_dvr"],
        ),
        ("iso", &["render_iso", "render_iso_stack"]),
        ("mip", &["render_mip"]),
    ];
    MODE_ROOTS
        .into_iter()
        .filter_map(|(family, roots)| {
            roots
                .iter()
                .any(|root| reachable_function_names.iter().any(|name| name == root))
                .then_some(family)
        })
        .collect()
}

fn write_spirv(module: &Module, entry_point: &str) -> Result<Vec<u32>, String> {
    // Naga otherwise emits every function in the source module even when
    // `PipelineOptions` selects one entry point. Retaining one entry and using
    // Naga's own compactor makes these counts specific to its reachable
    // selected-entry module. This is still not vendor-compiled machine code.
    let mut selected_module = module.clone();
    selected_module
        .entry_points
        .retain(|entry| entry.stage == ShaderStage::Fragment && entry.name == entry_point);
    if selected_module.entry_points.len() != 1 {
        return Err(format!(
            "expected exactly one fragment entry point named {entry_point}"
        ));
    }
    wgpu::naga::compact::compact(&mut selected_module, wgpu::naga::compact::KeepUnused::No);
    let mut validator = wgpu::naga::valid::Validator::new(
        wgpu::naga::valid::ValidationFlags::all(),
        wgpu::naga::valid::Capabilities::all(),
    );
    let selected_info = validator.validate(&selected_module).map_err(|error| {
        format!("compacted WGSL validation failed for {entry_point}: {error:#?}")
    })?;
    let mut options = wgpu::naga::back::spv::Options::default();
    options
        .flags
        .remove(wgpu::naga::back::spv::WriterFlags::DEBUG);
    let pipeline = wgpu::naga::back::spv::PipelineOptions {
        shader_stage: ShaderStage::Fragment,
        entry_point: entry_point.to_owned(),
    };
    wgpu::naga::back::spv::write_vec(&selected_module, &selected_info, &options, Some(&pipeline))
        .map_err(|error| format!("SPIR-V generation failed for {entry_point}: {error}"))
}

fn inspect_spirv(words: &[u32]) -> Result<SpirvFacts, String> {
    if words.len() < 5 || words[0] != 0x0723_0203 {
        return Err("Naga returned an invalid SPIR-V header".to_owned());
    }
    let mut cursor = 5;
    let mut instruction_count = 0;
    let mut opcode_counts = BTreeMap::new();
    while cursor < words.len() {
        let first = words[cursor];
        let word_count = usize::try_from(first >> 16)
            .map_err(|_| "SPIR-V instruction length does not fit usize".to_owned())?;
        if word_count == 0 || cursor.saturating_add(word_count) > words.len() {
            return Err(format!("invalid SPIR-V instruction at word {cursor}"));
        }
        let opcode = (first & 0xffff) as u16;
        *opcode_counts.entry(opcode).or_default() += 1;
        instruction_count += 1;
        cursor += word_count;
    }
    Ok(SpirvFacts {
        word_count: words.len(),
        instruction_count,
        opcode_counts,
    })
}

fn audit_json(audit: &ShaderAudit) -> String {
    let mut output = String::new();
    writeln!(output, "{{").unwrap();
    writeln!(output, "  \"schema\": {},", json_string(AUDIT_SCHEMA)).unwrap();
    writeln!(
        output,
        "  \"evidence_class\": \"development_structural_diagnostic_no_product_performance_claim\","
    )
    .unwrap();
    writeln!(output, "  \"source_module\": \"production_render_wgsl\",").unwrap();
    writeln!(
        output,
        "  \"source_sha256\": {},",
        json_string(&audit.source_sha256)
    )
    .unwrap();
    writeln!(
        output,
        "  \"pipeline_entry_count\": {},",
        audit.pipeline_entry_count
    )
    .unwrap();
    writeln!(
        output,
        "  \"fragment_entry_count\": {},",
        audit.fragment_entries.len()
    )
    .unwrap();
    writeln!(output, "  \"vendor_profiler_facts\": {{").unwrap();
    for (index, fact) in [
        "registers",
        "private_memory_spills",
        "occupancy",
        "cache_behavior",
        "divergence",
    ]
    .into_iter()
    .enumerate()
    {
        let suffix = if index == 4 { "" } else { "," };
        writeln!(
            output,
            "    {}: \"unavailable_without_external_vendor_profiler\"{suffix}",
            json_string(fact)
        )
        .unwrap();
    }
    writeln!(output, "  }},").unwrap();
    writeln!(
        output,
        "  \"private_byte_interpretation\": \"conservative_sum_of_reachable_fixed_function_local_array_layouts_not_liveness_register_or_spill_measurement\","
    )
    .unwrap();
    writeln!(output, "  \"fragment_entries\": [").unwrap();
    for (entry_index, entry) in audit.fragment_entries.iter().enumerate() {
        writeln!(output, "    {{").unwrap();
        writeln!(
            output,
            "      \"entry_point\": {},",
            json_string(&entry.entry_point)
        )
        .unwrap();
        writeln!(
            output,
            "      \"reachable_function_count\": {},",
            entry.reachable_function_names.len()
        )
        .unwrap();
        write_string_array(
            &mut output,
            "reachable_function_names",
            entry.reachable_function_names.iter().map(String::as_str),
            6,
            true,
        );
        write_string_array(
            &mut output,
            "reachable_render_mode_families",
            entry.render_mode_families.iter().copied(),
            6,
            true,
        );
        writeln!(
            output,
            "      \"conservative_private_bytes_sum\": {},",
            entry.conservative_private_bytes_sum
        )
        .unwrap();
        writeln!(output, "      \"fixed_function_local_arrays\": [").unwrap();
        for (array_index, array) in entry.fixed_local_arrays.iter().enumerate() {
            let suffix = if array_index + 1 == entry.fixed_local_arrays.len() {
                ""
            } else {
                ","
            };
            writeln!(
                output,
                "        {{\"function\": {}, \"local\": {}, \"element_type\": {}, \"element_count\": {}, \"element_layout_bytes\": {}, \"stride_bytes\": {}, \"conservative_private_bytes\": {}}}{suffix}",
                json_string(&array.function),
                json_string(&array.local),
                json_string(&array.element_type),
                array.element_count,
                array.element_layout_bytes,
                array.stride_bytes,
                array.conservative_private_bytes,
            )
            .unwrap();
        }
        writeln!(output, "      ],").unwrap();
        writeln!(output, "      \"spirv\": {{").unwrap();
        writeln!(
            output,
            "        \"scope\": \"naga_compacted_selected_entry_module_not_vendor_compiler_output\","
        )
        .unwrap();
        writeln!(
            output,
            "        \"word_count\": {},",
            entry.spirv.word_count
        )
        .unwrap();
        writeln!(
            output,
            "        \"instruction_count\": {},",
            entry.spirv.instruction_count
        )
        .unwrap();
        writeln!(output, "        \"opcode_counts\": [").unwrap();
        for (opcode_index, (opcode, count)) in entry.spirv.opcode_counts.iter().enumerate() {
            let suffix = if opcode_index + 1 == entry.spirv.opcode_counts.len() {
                ""
            } else {
                ","
            };
            writeln!(
                output,
                "          {{\"opcode\": {opcode}, \"count\": {count}}}{suffix}"
            )
            .unwrap();
        }
        writeln!(output, "        ]").unwrap();
        writeln!(output, "      }}").unwrap();
        let suffix = if entry_index + 1 == audit.fragment_entries.len() {
            ""
        } else {
            ","
        };
        writeln!(output, "    }}{suffix}").unwrap();
    }
    writeln!(output, "  ]").unwrap();
    writeln!(output, "}}").unwrap();
    output
}

fn write_string_array<'a>(
    output: &mut String,
    field: &str,
    values: impl Iterator<Item = &'a str>,
    indent: usize,
    trailing_comma: bool,
) {
    let values = values.map(json_string).collect::<Vec<_>>().join(", ");
    let suffix = if trailing_comma { "," } else { "" };
    writeln!(
        output,
        "{}{}: [{values}]{suffix}",
        " ".repeat(indent),
        json_string(field)
    )
    .unwrap();
}

fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            control if control.is_control() => {
                write!(escaped, "\\u{:04x}", u32::from(control)).unwrap();
            }
            other => escaped.push(other),
        }
    }
    escaped.push('"');
    escaped
}

// Test-only one-shot SHA-256 avoids adding a hashing dependency to the renderer.
fn sha256_hex(bytes: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    const ROUND: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];

    let bit_len = u64::try_from(bytes.len())
        .expect("shader source length fits u64")
        .checked_mul(8)
        .expect("shader source bit length fits u64");
    let mut padded = bytes.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut state = INITIAL;
    for chunk in padded.chunks_exact(64) {
        let mut schedule = [0_u32; 64];
        for (index, word) in chunk.chunks_exact(4).enumerate() {
            schedule[index] = u32::from_be_bytes(word.try_into().unwrap());
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

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temporary1 = h
                .wrapping_add(sum1)
                .wrapping_add(choice)
                .wrapping_add(ROUND[index])
                .wrapping_add(schedule[index]);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temporary2 = sum0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temporary1);
            d = c;
            c = b;
            b = a;
            a = temporary1.wrapping_add(temporary2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut digest = String::with_capacity(64);
    for word in state {
        write!(digest, "{word:08x}").unwrap();
    }
    digest
}

#[test]
fn call_graph_detector_follows_nested_control_flow_and_excludes_dead_functions() {
    const SOURCE: &str = r#"
fn leaf(value: f32) -> f32 { return value + 1.0; }
fn through_if(value: f32) -> f32 {
    if value > 0.0 { return leaf(value); }
    return value;
}
fn through_loop(value: f32) -> f32 {
    var result = value;
    loop {
        result = through_if(result);
        break;
    }
    return result;
}
fn unreachable(value: f32) -> f32 { return value * 99.0; }
@fragment
fn fragment_main() -> @location(0) vec4<f32> {
    return vec4<f32>(through_loop(1.0));
}
"#;
    let (module, _) = parse_and_validate(SOURCE).unwrap();
    let entry = module
        .entry_points
        .iter()
        .find(|entry| entry.name == "fragment_main")
        .unwrap();
    let reachable = reachable_functions(&module, &entry.function);
    let names = reachable
        .into_iter()
        .map(|handle| module.functions[handle].name.as_deref().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        BTreeSet::from(["leaf", "through_if", "through_loop"])
    );
    assert!(!names.contains("unreachable"));
}

#[test]
fn fixed_array_detector_reports_only_reachable_function_local_layouts() {
    const SOURCE: &str = r#"
struct Pair { left: vec4<f32>, right: u32 }
fn reachable_array() -> f32 {
    var pairs: array<Pair, 4>;
    pairs[0].left = vec4<f32>(1.0);
    return pairs[0].left.x;
}
fn dead_array() -> f32 {
    var words: array<u32, 32>;
    words[0] = 7u;
    return f32(words[0]);
}
@fragment
fn fragment_main() -> @location(0) vec4<f32> {
    var colors: array<vec4<f32>, 2>;
    colors[0] = vec4<f32>(reachable_array());
    return colors[0];
}
"#;
    let (module, _) = parse_and_validate(SOURCE).unwrap();
    let entry = module
        .entry_points
        .iter()
        .find(|entry| entry.name == "fragment_main")
        .unwrap();
    let reachable = reachable_functions(&module, &entry.function);
    let mut arrays = fixed_arrays_in_function(&module, &entry.function, &entry.name);
    for handle in reachable {
        let function = &module.functions[handle];
        arrays.extend(fixed_arrays_in_function(
            &module,
            function,
            function.name.as_deref().unwrap(),
        ));
    }
    arrays.sort();

    assert_eq!(arrays.len(), 2);
    assert_eq!(arrays[0].function, "fragment_main");
    assert_eq!(arrays[0].element_type, "vec4<f32>");
    assert_eq!(arrays[0].element_count, 2);
    assert_eq!(arrays[0].conservative_private_bytes, 32);
    assert_eq!(arrays[1].function, "reachable_array");
    assert_eq!(arrays[1].element_type, "Pair");
    assert_eq!(arrays[1].element_count, 4);
    assert_eq!(arrays[1].element_layout_bytes, 32);
    assert_eq!(arrays[1].conservative_private_bytes, 128);
    assert!(arrays.iter().all(|array| array.function != "dead_array"));
}

#[test]
fn sha256_implementation_matches_published_empty_vector() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn production_shader_audit_is_complete_and_mode_specific() {
    let audit = audit_shader(PRODUCTION_RENDER_WGSL).unwrap();
    assert_eq!(audit.pipeline_entry_count, 5);
    assert_eq!(audit.fragment_entries.len(), 4);
    assert_eq!(audit.source_sha256.len(), 64);

    for entry in &audit.fragment_entries {
        assert!(entry.spirv.word_count > 5);
        assert!(entry.spirv.instruction_count > 0);
        assert_eq!(
            entry.spirv.opcode_counts.values().sum::<usize>(),
            entry.spirv.instruction_count
        );
        if entry.entry_point.contains("mip") {
            assert_eq!(entry.render_mode_families, ["mip"]);
            assert!(entry.fixed_local_arrays.is_empty());
        } else {
            assert_eq!(
                entry.render_mode_families,
                ["cross_section", "dvr", "iso", "mip"]
            );
            assert_eq!(entry.fixed_local_arrays.len(), 3);
        }
    }

    let rendered = audit_json(&audit);
    assert!(rendered.contains("unavailable_without_external_vendor_profiler"));
    assert!(rendered.contains("\"opcode_counts\""));
}

/// Emits the sanitized EP-00 baseline with `--nocapture` when explicitly run.
#[test]
#[ignore = "development evidence; run explicitly to emit the structural baseline JSON"]
fn emit_production_shader_structural_audit_json() {
    let audit = audit_shader(PRODUCTION_RENDER_WGSL).unwrap();
    println!(
        "mirante4d-ep00-production-shader-structural-audit-json:{}",
        audit_json(&audit)
    );
}

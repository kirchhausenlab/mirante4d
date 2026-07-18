use anyhow::{Context, bail};
use mirante4d_identity::Sha256Hasher;
use serde::{Deserialize, Serialize};

const AUTHORITY_BYTES: &[u8] =
    include_bytes!("../../../../verification/viewer-performance-ep01-selection.json");
const AUTHORITY_SCHEMA: &str = "mirante4d-viewer-performance-ep01-selection-authority";
const AUTHORITY_SCHEMA_VERSION: u64 = 1;
const COMMITTED_AUTHORITY_SHA256: &str =
    "dc87d1b26acf22d65c5cac1a897d94b2d1b17da566cba73c6d1a27f435e9ab3c";

const GEOMETRY_AUTHORITIES: [&str; 3] = [
    "mirante4d-viewer-performance-workload-bundle-4",
    "mirante4d-viewer-performance-script-bundle-3",
    "mirante4d-viewer-performance-oracle-bundle-3",
];
const CANDIDATE_KEY_FIELDS: [&str; 7] = [
    "candidate_content_generation",
    "logical_layer",
    "time_index",
    "scale_level",
    "brick_z",
    "brick_y",
    "brick_x",
];
const TRACE_DIGEST_FIELDS: [&str; 8] = [
    "qualification_profile_contract_sha256",
    "ep01_selection_authority_sha256",
    "candidate_cubic_brick_edge",
    "trace_family",
    "candidate_content_generation",
    "canonical_candidate_BrickKey_entries",
    "unique_key_count",
    "unique_payload_bytes",
];
const CANDIDATE_EDGES: [u32; 2] = [32, 64];
const REQUIRED_TRACE_FAMILIES: [&str; 8] = [
    "arbitrary_plane",
    "four_panel",
    "time_navigation",
    "mip",
    "dvr",
    "iso",
    "analysis",
    "verification",
];
const BRICK_SUMMARIES: [&str; 4] = ["any_valid", "min", "max", "valid_count"];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SelectionAuthority {
    schema: String,
    schema_version: u64,
    trace_derivation: TraceDerivation,
    candidate_cubic_brick_edges: Vec<u32>,
    selection_rule: SelectionRule,
    fixed_comparison_defaults: FixedComparisonDefaults,
    required_trace_families: Vec<String>,
    gates: SelectionGates,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TraceDerivation {
    scheme: String,
    geometry_authorities: Vec<String>,
    candidate_key_fields: Vec<String>,
    canonical_order: String,
    deduplication: String,
    trace_digest_scheme: String,
    trace_digest_fields: Vec<String>,
    one_digest_per_candidate_and_trace_family: bool,
    public_receipt_serializes_raw_keys: bool,
    serialized_predecessor_keys: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SelectionRule {
    candidate_order: Vec<u32>,
    rule: String,
    scoring_allowed: bool,
    runtime_selector_allowed: bool,
    headroom_arithmetic: String,
    engineering_ratio_denominator: String,
    engineering_ratios_receive_additional_headroom: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FixedComparisonDefaults {
    pyramid: String,
    maximum_pyramid_levels: u32,
    outer_shard_edge: u32,
    inner_order: String,
    codec: String,
    codec_level: u32,
    payload_integrity: String,
    gpu_payload: String,
    brick_summaries: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SelectionGates {
    headroom: HeadroomGates,
    format_and_index: FormatAndIndexGates,
    checkpoint: CheckpointGates,
    plane_amplification: PlaneAmplificationGates,
    preprocessing: PreprocessingGates,
    runtime: RuntimeGates,
    gpu: GpuGates,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HeadroomGates {
    minimum_latency_basis_points: u32,
    minimum_resource_basis_points: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FormatAndIndexGates {
    compact_inner_record_bytes: u64,
    maximum_compact_inner_record_bytes: u64,
    shard_index_header_bytes: u64,
    maximum_per_shard_index_bytes: u64,
    maximum_total_index_bytes: u64,
    maximum_index_to_logical_pyramid_basis_points: u32,
    maximum_package_to_logical_pyramid_basis_points: u32,
    maximum_package_bytes_per_s0_byte: u64,
    maximum_temporary_to_package_basis_points: u32,
    maximum_physical_objects: u64,
    maximum_decoded_shard_bytes: u64,
    maximum_encoded_shard_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CheckpointGates {
    record_bytes: u64,
    maximum_record_bytes: u64,
    header_bytes: u64,
    read_window_bytes: u64,
    resident_read_windows: u64,
    maximum_resident_bytes: u64,
    batch_records: u64,
    maximum_record_window: u64,
    maximum_batch_payload_bytes: u64,
    maximum_batch_age_seconds: u64,
    maximum_regular_files: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PlaneAmplificationGates {
    maximum_encoded_bytes_per_useful_byte: u64,
    maximum_fetched_to_useful_basis_points: u32,
    maximum_decoded_to_useful_basis_points: u32,
    maximum_uploaded_to_useful_basis_points: u32,
    maximum_range_requests_per_new_brick: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PreprocessingGates {
    maximum_wall_time_ns: u64,
    maximum_process_cpu_time_ns: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RuntimeGates {
    maximum_cache_entry_metadata_bytes: u64,
    maximum_live_reads_per_new_brick: u64,
    maximum_codec_decodes_per_new_brick: u64,
    maximum_decoded_allocations_per_new_brick: u64,
    maximum_cache_authorities_per_brick: u64,
    maximum_resident_hash_load_basis_points: u32,
    maximum_resident_hash_probes: u64,
    maximum_residency_entries_per_brick: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct GpuGates {
    maximum_page_record_bytes: u64,
    maximum_directory_slot_bytes: u64,
    maximum_upload_batch_bytes: u64,
    maximum_staging_batches: u64,
    maximum_uploads_per_residency_epoch: u64,
    maximum_renderer_pipelines: u64,
    maximum_pick_pipelines: u64,
    maximum_pipeline_compiles_during_interaction: u64,
    maximum_common_path_fixed_private_array_entries: u64,
    maximum_descriptor_resolutions_per_crossed_brick: u64,
    maximum_command_encoders_per_coordinated_frame: u64,
    maximum_queue_submissions_per_coordinated_frame: u64,
    maximum_unaccounted_payload_allocations: u64,
}

pub(super) fn validate_committed_authority() -> anyhow::Result<()> {
    let observed = authority_fingerprint_sha256();
    if observed != COMMITTED_AUTHORITY_SHA256 {
        bail!("committed EP-01 selection authority changed without updating its exact commitment")
    }
    let authority = serde_json::from_slice::<SelectionAuthority>(AUTHORITY_BYTES)
        .context("committed EP-01 selection authority is not strict valid JSON")?;
    validate_authority(&authority)
}

pub(super) fn authority_fingerprint_sha256() -> String {
    Sha256Hasher::digest(AUTHORITY_BYTES).to_string()
}

fn validate_authority(authority: &SelectionAuthority) -> anyhow::Result<()> {
    if authority.schema != AUTHORITY_SCHEMA || authority.schema_version != AUTHORITY_SCHEMA_VERSION
    {
        bail!(
            "EP-01 selection authority must use schema {AUTHORITY_SCHEMA:?} version {AUTHORITY_SCHEMA_VERSION}"
        )
    }

    let trace = &authority.trace_derivation;
    if trace.scheme != "mirante4d-ep01-brickkey-trace-projection-1"
        || trace.geometry_authorities != GEOMETRY_AUTHORITIES
        || trace.candidate_key_fields != CANDIDATE_KEY_FIELDS
        || trace.canonical_order
            != "unsigned_lexicographic_candidate_content_generation_layer_time_scale_z_y_x"
        || trace.deduplication != "exact_candidate_BrickKey"
        || trace.trace_digest_scheme
            != "sha256_domain_mirante4d_ep01_candidate_brickkey_trace_v1_sorted_binary_le"
        || trace.trace_digest_fields != TRACE_DIGEST_FIELDS
        || !trace.one_digest_per_candidate_and_trace_family
        || trace.public_receipt_serializes_raw_keys
        || trace.serialized_predecessor_keys
    {
        bail!(
            "EP-01 candidate BrickKey traces must derive deterministically from the bound workload/script/oracle geometry without serialized predecessor keys"
        )
    }

    if authority.candidate_cubic_brick_edges != CANDIDATE_EDGES {
        bail!("EP-01 selection candidates must be exactly the 32- and 64-cubic brick edges")
    }

    let rule = &authority.selection_rule;
    if rule.candidate_order != CANDIDATE_EDGES
        || rule.rule != "select_32_if_every_gate_passes_else_64_if_every_gate_passes_else_stop"
        || rule.scoring_allowed
        || rule.runtime_selector_allowed
        || rule.headroom_arithmetic != "checked_u128_observed_times_10000_lte_limit_times_8000"
        || rule.engineering_ratio_denominator
            != "logical_candidate_pyramid_scalar_plus_validity_bytes_every_level"
        || rule.engineering_ratios_receive_additional_headroom
    {
        bail!(
            "EP-01 selection must admit the first candidate that passes every gate, without scoring or a runtime selector"
        )
    }

    let defaults = &authority.fixed_comparison_defaults;
    if defaults.pyramid != "calibrated_valid_aware"
        || defaults.maximum_pyramid_levels != 64
        || defaults.outer_shard_edge != 256
        || defaults.inner_order != "row_major"
        || defaults.codec != "zstd"
        || defaults.codec_level != 3
        || defaults.payload_integrity != "crc32c"
        || defaults.gpu_payload != "native_little_endian_buffer"
        || defaults.brick_summaries != BRICK_SUMMARIES
    {
        bail!("EP-01 fixed comparison dimensions differ from the selected default contract")
    }

    if authority.required_trace_families != REQUIRED_TRACE_FAMILIES {
        bail!("EP-01 selection authority omitted or changed a required BrickKey trace family")
    }

    let gates = &authority.gates;
    if gates.headroom.minimum_latency_basis_points != 2_000
        || gates.headroom.minimum_resource_basis_points != 2_000
    {
        bail!("EP-01 headroom gates differ from the committed contract")
    }

    let format = &gates.format_and_index;
    if format.compact_inner_record_bytes != 32
        || format.maximum_compact_inner_record_bytes != 32
        || format.shard_index_header_bytes != 64
        || format.maximum_per_shard_index_bytes != 16_448
        || format.maximum_total_index_bytes != 512 * 1024 * 1024
        || format.maximum_index_to_logical_pyramid_basis_points != 1_000
        || format.maximum_package_to_logical_pyramid_basis_points != 11_000
        || format.maximum_package_bytes_per_s0_byte != 2
        || format.maximum_temporary_to_package_basis_points != 30_000
        || format.maximum_physical_objects != 11_264
        || format.maximum_decoded_shard_bytes != 69_206_016
        || format.maximum_encoded_shard_bytes != 83_890_176
    {
        bail!("EP-01 format and index gates differ from the committed contract")
    }

    let checkpoint = &gates.checkpoint;
    if checkpoint.record_bytes != 64
        || checkpoint.maximum_record_bytes != 64
        || checkpoint.header_bytes != 4_096
        || checkpoint.read_window_bytes != 1024 * 1024
        || checkpoint.resident_read_windows != 2
        || checkpoint.maximum_resident_bytes != 2_101_248
        || checkpoint.batch_records != 512
        || checkpoint.maximum_record_window != 4_096
        || checkpoint.maximum_batch_payload_bytes != 64 * 1024 * 1024
        || checkpoint.maximum_batch_age_seconds != 15
        || checkpoint.maximum_regular_files != 8
    {
        bail!("EP-01 checkpoint gates differ from the committed contract")
    }

    let plane = &gates.plane_amplification;
    if plane.maximum_encoded_bytes_per_useful_byte != 96
        || plane.maximum_fetched_to_useful_basis_points != 960_000
        || plane.maximum_decoded_to_useful_basis_points != 960_000
        || plane.maximum_uploaded_to_useful_basis_points != 960_000
        || plane.maximum_range_requests_per_new_brick != 2
    {
        bail!("EP-01 plane amplification gates differ from the committed contract")
    }

    let preprocessing = &gates.preprocessing;
    if preprocessing.maximum_wall_time_ns != 900_000_000_000
        || preprocessing.maximum_process_cpu_time_ns != 1_200_000_000_000
    {
        bail!("EP-01 preprocessing gates differ from the committed contract")
    }

    let runtime = &gates.runtime;
    if runtime.maximum_cache_entry_metadata_bytes != 256
        || runtime.maximum_live_reads_per_new_brick != 1
        || runtime.maximum_codec_decodes_per_new_brick != 1
        || runtime.maximum_decoded_allocations_per_new_brick != 1
        || runtime.maximum_cache_authorities_per_brick != 1
        || runtime.maximum_resident_hash_load_basis_points != 5_000
        || runtime.maximum_resident_hash_probes != 32
        || runtime.maximum_residency_entries_per_brick != 1
    {
        bail!("EP-01 runtime gates differ from the committed contract")
    }

    let gpu = &gates.gpu;
    if gpu.maximum_page_record_bytes != 64
        || gpu.maximum_directory_slot_bytes != 32
        || gpu.maximum_upload_batch_bytes != 8 * 1024 * 1024
        || gpu.maximum_staging_batches != 2
        || gpu.maximum_uploads_per_residency_epoch != 1
        || gpu.maximum_renderer_pipelines != 5
        || gpu.maximum_pick_pipelines != 1
        || gpu.maximum_pipeline_compiles_during_interaction != 0
        || gpu.maximum_common_path_fixed_private_array_entries != 8
        || gpu.maximum_descriptor_resolutions_per_crossed_brick != 1
        || gpu.maximum_command_encoders_per_coordinated_frame != 1
        || gpu.maximum_queue_submissions_per_coordinated_frame != 1
        || gpu.maximum_unaccounted_payload_allocations != 0
    {
        bail!("EP-01 GPU gates differ from the committed contract")
    }

    validate_gate_coherence(authority)
}

fn validate_gate_coherence(authority: &SelectionAuthority) -> anyhow::Result<()> {
    let defaults = &authority.fixed_comparison_defaults;
    let gates = &authority.gates;
    let format = &gates.format_and_index;
    let checkpoint = &gates.checkpoint;
    let plane = &gates.plane_amplification;
    let runtime = &gates.runtime;

    let mut maximum_inner_records = 0_u64;
    for edge in &authority.candidate_cubic_brick_edges {
        if *edge == 0 || !defaults.outer_shard_edge.is_multiple_of(*edge) {
            bail!("every EP-01 brick candidate must divide the fixed outer shard edge")
        }
        let per_axis = u64::from(defaults.outer_shard_edge / edge);
        let per_plane = per_axis
            .checked_mul(per_axis)
            .context("EP-01 per-shard inner-record count overflowed")?;
        let inner_records = per_plane
            .checked_mul(per_axis)
            .context("EP-01 per-shard inner-record count overflowed")?;
        maximum_inner_records = maximum_inner_records.max(inner_records);
    }
    let inner_index_bytes = format
        .compact_inner_record_bytes
        .checked_mul(maximum_inner_records)
        .context("EP-01 per-shard inner index byte bound overflowed")?;
    let per_shard_index_bytes = format
        .shard_index_header_bytes
        .checked_add(inner_index_bytes)
        .context("EP-01 per-shard index byte bound overflowed")?;
    if per_shard_index_bytes != format.maximum_per_shard_index_bytes
        || format.maximum_per_shard_index_bytes > format.maximum_total_index_bytes
        || format.compact_inner_record_bytes > format.maximum_compact_inner_record_bytes
        || format.maximum_decoded_shard_bytes > format.maximum_encoded_shard_bytes
    {
        bail!("EP-01 format and index gate formulas are incoherent")
    }

    let resident_windows_bytes = checkpoint
        .read_window_bytes
        .checked_mul(checkpoint.resident_read_windows)
        .context("EP-01 resident checkpoint window byte bound overflowed")?;
    let checkpoint_resident_bytes = checkpoint
        .header_bytes
        .checked_add(resident_windows_bytes)
        .context("EP-01 resident checkpoint byte bound overflowed")?;
    let maximum_record_window_bytes = checkpoint
        .record_bytes
        .checked_mul(checkpoint.maximum_record_window)
        .context("EP-01 checkpoint record-window byte bound overflowed")?;
    let batch_record_bytes = checkpoint
        .record_bytes
        .checked_mul(checkpoint.batch_records)
        .context("EP-01 checkpoint batch record byte bound overflowed")?;
    if checkpoint.record_bytes > checkpoint.maximum_record_bytes
        || checkpoint_resident_bytes != checkpoint.maximum_resident_bytes
        || checkpoint.batch_records > checkpoint.maximum_record_window
        || maximum_record_window_bytes > checkpoint.read_window_bytes
        || batch_record_bytes > checkpoint.read_window_bytes
    {
        bail!("EP-01 checkpoint gate formulas are incoherent")
    }
    // The batch payload is a separate streaming allowance; it is intentionally
    // not added to or compared against the resident header-and-window bound.

    let encoded_basis_points = plane
        .maximum_encoded_bytes_per_useful_byte
        .checked_mul(10_000)
        .context("EP-01 plane amplification conversion overflowed")?;
    if encoded_basis_points != u64::from(plane.maximum_fetched_to_useful_basis_points)
        || plane.maximum_fetched_to_useful_basis_points
            != plane.maximum_decoded_to_useful_basis_points
        || plane.maximum_fetched_to_useful_basis_points
            != plane.maximum_uploaded_to_useful_basis_points
    {
        bail!("EP-01 plane amplification units are incoherent")
    }

    if gates.headroom.minimum_latency_basis_points >= 10_000
        || gates.headroom.minimum_resource_basis_points >= 10_000
        || 10_000 - gates.headroom.minimum_latency_basis_points != 8_000
        || 10_000 - gates.headroom.minimum_resource_basis_points != 8_000
        || runtime.maximum_resident_hash_load_basis_points >= 10_000
        || gates.preprocessing.maximum_wall_time_ns
            > gates.preprocessing.maximum_process_cpu_time_ns
    {
        bail!("EP-01 headroom and resource gates are incoherent")
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::{Number, Value, json};

    use super::*;

    fn authority_value() -> Value {
        serde_json::from_slice(AUTHORITY_BYTES).unwrap()
    }

    fn collect_leaf_pointers(value: &Value, prefix: &str, pointers: &mut Vec<String>) {
        match value {
            Value::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    collect_leaf_pointers(value, &format!("{prefix}/{index}"), pointers);
                }
            }
            Value::Object(entries) => {
                for (key, value) in entries {
                    let key = key.replace('~', "~0").replace('/', "~1");
                    collect_leaf_pointers(value, &format!("{prefix}/{key}"), pointers);
                }
            }
            _ => pointers.push(prefix.to_owned()),
        }
    }

    fn mutate_leaf(value: &mut Value, pointer: &str) {
        let leaf = value
            .pointer_mut(pointer)
            .unwrap_or_else(|| panic!("missing authority pointer {pointer}"));
        match leaf {
            Value::String(value) => value.push_str("_changed"),
            Value::Bool(value) => *value = !*value,
            Value::Number(value) => {
                let value = value.as_u64().expect("authority numbers must be unsigned");
                *leaf = Value::Number(Number::from(
                    value.checked_add(1).expect("authority mutation overflowed"),
                ));
            }
            Value::Null | Value::Array(_) | Value::Object(_) => {
                panic!("authority pointer {pointer} is not a primitive leaf")
            }
        }
    }

    #[test]
    fn committed_authority_is_strict_valid_and_matches_its_exact_commitment() {
        validate_committed_authority().unwrap();
        assert_eq!(authority_fingerprint_sha256(), COMMITTED_AUTHORITY_SHA256);
        let _: SelectionAuthority = serde_json::from_slice(AUTHORITY_BYTES).unwrap();

        let mut unknown = authority_value();
        unknown["unexpected"] = json!(true);
        assert!(serde_json::from_value::<SelectionAuthority>(unknown).is_err());
    }

    #[test]
    fn every_selection_authority_leaf_is_validated_and_commitment_sensitive() {
        let authority = authority_value();
        let canonical_authority_sha256 =
            Sha256Hasher::digest(serde_json::to_vec(&authority).unwrap()).to_string();
        let mut pointers = Vec::new();
        collect_leaf_pointers(&authority, "", &mut pointers);
        assert!(!pointers.is_empty());

        for pointer in pointers {
            let mut mutated = authority.clone();
            mutate_leaf(&mut mutated, &pointer);
            let encoded = serde_json::to_vec(&mutated).unwrap();
            let mutated_sha256 = Sha256Hasher::digest(&encoded).to_string();
            assert_ne!(
                mutated_sha256, canonical_authority_sha256,
                "authority commitment omitted {pointer}"
            );
            assert_ne!(
                mutated_sha256, COMMITTED_AUTHORITY_SHA256,
                "raw authority commitment omitted {pointer}"
            );
            let mutated: SelectionAuthority = serde_json::from_value(mutated).unwrap();
            assert!(
                validate_authority(&mutated).is_err(),
                "authority validation omitted {pointer}"
            );
        }
    }
}

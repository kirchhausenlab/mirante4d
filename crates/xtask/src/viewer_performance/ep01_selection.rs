use std::mem::size_of;

use anyhow::{Context, bail};
use mirante4d_identity::Sha256Hasher;
use serde::{Deserialize, Serialize};

const AUTHORITY_BYTES: &[u8] =
    include_bytes!("../../../../verification/viewer-performance-ep01-selection.json");
const AUTHORITY_SCHEMA: &str = "mirante4d-viewer-performance-ep01-selection-authority";
const AUTHORITY_SCHEMA_VERSION: u64 = 2;
const COMMITTED_AUTHORITY_SHA256: &str =
    "da070887a069728eee41aa2bc5b6c3f6b6e79d58e99bc844e711770bb7b441f0";
const COMMITTED_AUTHORITY_SEMANTIC_SHA256: &str =
    "505f4d671b7ca6c2c49fe8d55b975ee97ead9a907e75cdc14c743fe9205c2bd9";

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
const CANDIDATE_KEY_FIELD_WIDTHS: [u64; 7] = [32, 4, 8, 4, 4, 4, 4];
const TRACE_DIGEST_FIELDS: [&str; 8] = [
    "qualification_profile_contract_sha256_raw_bytes",
    "ep01_selection_authority_sha256_raw_bytes",
    "candidate_cubic_brick_edge_u32_le",
    "trace_family_tag_u8",
    "candidate_package_set_generation_raw_bytes",
    "unique_key_count_u64_le",
    "canonical_candidate_BrickKey_entries",
    "unique_payload_bytes_u64_le",
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
const TRACE_FAMILY_TAGS: [&str; 8] = [
    "0:arbitrary_plane",
    "1:four_panel",
    "2:time_navigation",
    "3:mip",
    "4:dvr",
    "5:iso",
    "6:analysis",
    "7:verification",
];
const CANDIDATE_PACKAGE_ROLES: [&str; 2] =
    ["representative_package", "supporting_temporal_package"];

const HEADROOM_PATHS: [&str; 25] = [
    "resources.max_cpu_total_bytes",
    "resources.max_cpu_decoded_residency_bytes",
    "resources.max_cpu_upload_staging_bytes",
    "resources.gpu_budget_bytes",
    "resources.max_gpu_resident_bytes",
    "resources.max_gpu_in_flight_bytes",
    "resources.max_open_objects",
    "resources.max_queued_requests",
    "absolute_gates.resident_input_to_current_presentation_p95_ns",
    "absolute_gates.maximum_current_presentation_gap_ns",
    "absolute_gates.maximum_main_loop_heartbeat_gap_ns",
    "absolute_gates.maximum_ui_thread_interaction_task_ns",
    "absolute_gates.maximum_plane_gpu_ns",
    "absolute_gates.maximum_mip_gpu_ns",
    "absolute_gates.maximum_dvr_gpu_ns",
    "absolute_gates.maximum_iso_gpu_ns",
    "absolute_gates.cold_first_useful_ns",
    "absolute_gates.cold_complete_coarse_ns",
    "absolute_gates.cold_target_settlement_ns",
    "absolute_gates.nonresident_target_settlement_ns",
    "gates.format_and_index.maximum_total_index_bytes",
    "gates.format_and_index.maximum_physical_objects",
    "gates.checkpoint.maximum_regular_files",
    "gates.preprocessing.maximum_wall_time_ns",
    "gates.preprocessing.maximum_process_cpu_time_ns",
];

const ENGINEERING_RATIO_DIRECT_PATHS: [&str; 10] = [
    "absolute_gates.maximum_instrumentation_overhead_basis_points",
    "gates.format_and_index.maximum_index_to_logical_pyramid_basis_points",
    "gates.format_and_index.maximum_package_to_logical_pyramid_basis_points",
    "gates.format_and_index.maximum_package_bytes_per_s0_byte",
    "gates.format_and_index.maximum_temporary_to_package_basis_points",
    "gates.plane_amplification.maximum_encoded_bytes_per_useful_byte",
    "gates.plane_amplification.maximum_fetched_to_useful_basis_points",
    "gates.plane_amplification.maximum_decoded_to_useful_basis_points",
    "gates.plane_amplification.maximum_uploaded_to_useful_basis_points",
    "gates.runtime.maximum_resident_hash_load_basis_points",
];

const STRUCTURAL_DIRECT_PATHS: [&str; 39] = [
    "gates.headroom.minimum_latency_basis_points",
    "gates.headroom.minimum_resource_basis_points",
    "gates.format_and_index.compact_inner_record_bytes",
    "gates.format_and_index.maximum_compact_inner_record_bytes",
    "gates.format_and_index.shard_index_header_bytes",
    "gates.format_and_index.maximum_per_shard_index_bytes",
    "gates.format_and_index.maximum_decoded_shard_bytes",
    "gates.format_and_index.maximum_encoded_shard_bytes",
    "gates.checkpoint.record_bytes",
    "gates.checkpoint.maximum_record_bytes",
    "gates.checkpoint.header_bytes",
    "gates.checkpoint.read_window_bytes",
    "gates.checkpoint.resident_read_windows",
    "gates.checkpoint.maximum_resident_bytes",
    "gates.checkpoint.batch_records",
    "gates.checkpoint.maximum_record_window",
    "gates.checkpoint.maximum_batch_payload_bytes",
    "gates.checkpoint.maximum_batch_age_seconds",
    "gates.plane_amplification.maximum_range_requests_per_new_brick",
    "gates.runtime.maximum_cache_entry_metadata_bytes",
    "gates.runtime.maximum_live_reads_per_new_brick",
    "gates.runtime.maximum_codec_decodes_per_new_brick",
    "gates.runtime.maximum_decoded_allocations_per_new_brick",
    "gates.runtime.maximum_cache_authorities_per_brick",
    "gates.runtime.maximum_resident_hash_probes",
    "gates.runtime.maximum_residency_entries_per_brick",
    "gates.gpu.maximum_page_record_bytes",
    "gates.gpu.maximum_directory_slot_bytes",
    "gates.gpu.maximum_upload_batch_bytes",
    "gates.gpu.maximum_staging_batches",
    "gates.gpu.maximum_uploads_per_residency_epoch",
    "gates.gpu.maximum_renderer_pipelines",
    "gates.gpu.maximum_pick_pipelines",
    "gates.gpu.maximum_pipeline_compiles_during_interaction",
    "gates.gpu.maximum_common_path_fixed_private_array_entries",
    "gates.gpu.maximum_descriptor_resolutions_per_crossed_brick",
    "gates.gpu.maximum_command_encoders_per_coordinated_frame",
    "gates.gpu.maximum_queue_submissions_per_coordinated_frame",
    "gates.gpu.maximum_unaccounted_payload_allocations",
];
const STRUCTURAL_EXACT_EQ_PATHS: [&str; 11] = [
    "gates.headroom.minimum_latency_basis_points",
    "gates.headroom.minimum_resource_basis_points",
    "gates.format_and_index.compact_inner_record_bytes",
    "gates.format_and_index.shard_index_header_bytes",
    "gates.checkpoint.record_bytes",
    "gates.checkpoint.header_bytes",
    "gates.checkpoint.read_window_bytes",
    "gates.checkpoint.resident_read_windows",
    "gates.checkpoint.maximum_resident_bytes",
    "gates.checkpoint.batch_records",
    "gates.checkpoint.maximum_record_window",
];
const STRUCTURAL_LTE_PATHS: [&str; 26] = [
    "gates.format_and_index.maximum_compact_inner_record_bytes",
    "gates.format_and_index.maximum_per_shard_index_bytes",
    "gates.format_and_index.maximum_decoded_shard_bytes",
    "gates.format_and_index.maximum_encoded_shard_bytes",
    "gates.checkpoint.maximum_record_bytes",
    "gates.checkpoint.maximum_batch_payload_bytes",
    "gates.checkpoint.maximum_batch_age_seconds",
    "gates.plane_amplification.maximum_range_requests_per_new_brick",
    "gates.runtime.maximum_cache_entry_metadata_bytes",
    "gates.runtime.maximum_live_reads_per_new_brick",
    "gates.runtime.maximum_codec_decodes_per_new_brick",
    "gates.runtime.maximum_decoded_allocations_per_new_brick",
    "gates.runtime.maximum_cache_authorities_per_brick",
    "gates.runtime.maximum_resident_hash_probes",
    "gates.runtime.maximum_residency_entries_per_brick",
    "gates.gpu.maximum_page_record_bytes",
    "gates.gpu.maximum_directory_slot_bytes",
    "gates.gpu.maximum_upload_batch_bytes",
    "gates.gpu.maximum_staging_batches",
    "gates.gpu.maximum_uploads_per_residency_epoch",
    "gates.gpu.maximum_renderer_pipelines",
    "gates.gpu.maximum_pick_pipelines",
    "gates.gpu.maximum_common_path_fixed_private_array_entries",
    "gates.gpu.maximum_descriptor_resolutions_per_crossed_brick",
    "gates.gpu.maximum_command_encoders_per_coordinated_frame",
    "gates.gpu.maximum_queue_submissions_per_coordinated_frame",
];
const STRUCTURAL_ZERO_EQ_PATHS: [&str; 2] = [
    "gates.gpu.maximum_pipeline_compiles_during_interaction",
    "gates.gpu.maximum_unaccounted_payload_allocations",
];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SelectionAuthority {
    schema: String,
    schema_version: u64,
    candidate_identity: CandidateIdentity,
    trace_derivation: TraceDerivation,
    candidate_cubic_brick_edges: Vec<u32>,
    selection_rule: SelectionRule,
    fixed_comparison_defaults: FixedComparisonDefaults,
    pyramid_contract: PyramidContract,
    compound_shard_contract: CompoundShardContract,
    accounting_contract: AccountingContract,
    checkpoint_contract: CheckpointContract,
    runtime_gpu_contract: RuntimeGpuContract,
    evidence_contract: EvidenceContract,
    required_trace_families: Vec<String>,
    comparator_partition: ComparatorPartition,
    gates: SelectionGates,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CandidateIdentity {
    scientific_content_identity_rule: String,
    pyramid_recipe_digest_scheme: String,
    pyramid_recipe_fields: Vec<String>,
    candidate_geometry_digest_scheme: String,
    candidate_geometry_digest_fields: Vec<String>,
    candidate_content_generation_scheme: String,
    candidate_content_generation_bytes: u64,
    candidate_package_roles: Vec<String>,
    candidate_package_set_generation_scheme: String,
    candidate_package_set_generation_bytes: u64,
    package_identity_rule: String,
    package_identity_is_candidate_generation: bool,
    runtime_gpu_projection_is_persisted_identity: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TraceDerivation {
    scheme: String,
    geometry_authorities: Vec<String>,
    candidate_key_fields: Vec<String>,
    candidate_key_field_widths: Vec<u64>,
    candidate_key_bytes: u64,
    canonical_order: String,
    candidate_key_binary_encoding: String,
    deduplication: String,
    trace_digest_scheme: String,
    trace_digest_domain: String,
    trace_digest_fields: Vec<String>,
    trace_family_tags: Vec<String>,
    trace_package_roles: Vec<String>,
    trace_package_set_generation_rule: String,
    package_role_state_rule: String,
    state_enumeration: String,
    phase_end_validation: String,
    family_projection_rules: Vec<String>,
    volume_traversal_rule: String,
    out_of_domain_support_adds_key: bool,
    unique_payload_bytes_rule: String,
    one_digest_per_candidate_and_trace_family: bool,
    public_receipt_serializes_raw_keys: bool,
    serialized_predecessor_keys: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SelectionRule {
    candidate_order: Vec<u32>,
    rule: String,
    every_candidate_executes_every_gate_before_selection: bool,
    short_circuit_candidate_execution_allowed: bool,
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
struct PyramidContract {
    basis_axis_order: String,
    norm_arithmetic: String,
    reduction_rule: String,
    reduction_factors: Vec<u64>,
    odd_tail_rule: String,
    integer_mean_rule: String,
    float32_mean_rule: String,
    provisional_validity_rule: String,
    invalid_dilation_rule: String,
    final_invalid_scalar_rule: String,
    centered_affine_rule: String,
    stop_rule: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CompoundShardContract {
    schema: String,
    decoded_payload_order: String,
    validity_bit_rule: String,
    all_valid_rule: String,
    all_invalid_rule: String,
    invalid_scalar_rule: String,
    frame_rule: String,
    crc32c_rule: String,
    region_order: String,
    header_magic: String,
    header_fields: Vec<String>,
    record_fields: Vec<String>,
    record_flags: Vec<String>,
    record_invariants: Vec<String>,
    row_major_mapping: Vec<String>,
    range_request_scope: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AccountingContract {
    logical_brick_bytes: String,
    logical_pyramid_bytes: String,
    s0_bytes: String,
    index_bytes: String,
    package_bytes: String,
    temporary_bytes: String,
    physical_objects: String,
    encoded_shard_bytes: String,
    decoded_shard_bytes: String,
    index_formula: String,
    package_formula: String,
    object_formula: String,
    package_ratio_gates_are_candidate_admission_not_universal_format_guarantees: bool,
    checkpoint_batch_payload_is_separate_from_resident_header_and_read_windows: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CheckpointContract {
    peak_regular_files: u64,
    regular_file_roles: Vec<String>,
    open_security_rule: String,
    header_fields: Vec<String>,
    planned_payload_rule: String,
    record_fields: Vec<String>,
    commit_rule: String,
    commit_slot_fields: Vec<String>,
    durability_order: String,
    recovery_rule: String,
    failure_rule: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RuntimeGpuContract {
    per_brick_epoch_scope: String,
    directory_slot_fields: Vec<String>,
    directory_empty_and_tombstone: String,
    directory_rule: String,
    page_record_fields: Vec<String>,
    binding_groups: Vec<String>,
    descriptor_resolution_scope: String,
    pipeline_semantics: String,
    coordinated_frame_scope: String,
    maximum_staging_bytes_formula: String,
    all_invalid_payload_slot_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EvidenceContract {
    sanitized_fields: Vec<String>,
    forbidden_sanitized_fields: Vec<String>,
    public_receipt_serializes_private_geometry: bool,
    raw_evidence_remains_external: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ComparatorPartition {
    path_syntax: String,
    headroom_paths: Vec<String>,
    engineering_ratio_direct_paths: Vec<String>,
    structural_direct_paths: Vec<String>,
    structural_exact_eq_paths: Vec<String>,
    structural_lte_paths: Vec<String>,
    structural_zero_eq_paths: Vec<String>,
    headroom_comparison: String,
    engineering_ratio_comparison: String,
    structural_comparison: String,
    partition_path_count: u64,
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

    let identity = &authority.candidate_identity;
    if identity.candidate_content_generation_bytes != 32
        || identity.candidate_package_roles != CANDIDATE_PACKAGE_ROLES
        || identity.candidate_package_set_generation_bytes != 32
        || identity.package_identity_is_candidate_generation
        || identity.runtime_gpu_projection_is_persisted_identity
    {
        bail!(
            "EP-01 candidate identity and fixed package-set lineage differ from the committed contract"
        )
    }

    let trace = &authority.trace_derivation;
    if trace.scheme != "mirante4d-ep01-brickkey-trace-projection-1"
        || trace.geometry_authorities != GEOMETRY_AUTHORITIES
        || trace.candidate_key_fields != CANDIDATE_KEY_FIELDS
        || trace.candidate_key_field_widths != CANDIDATE_KEY_FIELD_WIDTHS
        || trace.candidate_key_bytes != 60
        || trace.canonical_order
            != "generation_raw_bytes_then_unsigned_numeric_layer_time_scale_z_y_x"
        || trace.candidate_key_binary_encoding
            != "generation_raw_32_bytes_then_layer_u32_le_time_u64_le_scale_u32_le_z_u32_le_y_u32_le_x_u32_le"
        || trace.deduplication != "exact_candidate_BrickKey_per_trace_family"
        || trace.trace_digest_scheme
            != "sha256_domain_mirante4d_ep01_candidate_brickkey_trace_v1_sorted_binary_le"
        || trace.trace_digest_domain != "mirante4d-ep01-candidate-brickkey-trace-v1-nul"
        || trace.trace_digest_fields != TRACE_DIGEST_FIELDS
        || trace.trace_family_tags != TRACE_FAMILY_TAGS
        || trace.trace_package_roles != CANDIDATE_PACKAGE_ROLES
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
        || !rule.every_candidate_executes_every_gate_before_selection
        || rule.short_circuit_candidate_execution_allowed
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

    validate_clarification_contracts(authority)?;

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

    validate_gate_coherence(authority)?;

    let semantic = authority_semantic_fingerprint_sha256(authority)?;
    if semantic != COMMITTED_AUTHORITY_SEMANTIC_SHA256 {
        bail!(
            "EP-01 selection authority semantic contract changed without updating its exact commitment"
        )
    }
    Ok(())
}

fn authority_semantic_fingerprint_sha256(authority: &SelectionAuthority) -> anyhow::Result<String> {
    let encoded = serde_json::to_vec(authority)
        .context("EP-01 selection authority semantic serialization failed")?;
    Ok(Sha256Hasher::digest(encoded).to_string())
}

fn validate_clarification_contracts(authority: &SelectionAuthority) -> anyhow::Result<()> {
    let identity = &authority.candidate_identity;
    let trace = &authority.trace_derivation;
    let format = &authority.compound_shard_contract;
    let checkpoint = &authority.checkpoint_contract;
    let runtime = &authority.runtime_gpu_contract;
    let evidence = &authority.evidence_contract;
    let partition = &authority.comparator_partition;

    if identity.pyramid_recipe_fields.len() != 12
        || identity.candidate_geometry_digest_fields.len() != 8
        || trace.family_projection_rules.len() != REQUIRED_TRACE_FAMILIES.len()
        || trace.trace_package_roles != identity.candidate_package_roles
        || trace.out_of_domain_support_adds_key
    {
        bail!("EP-01 identity or trace projection contract is incomplete")
    }
    if format.header_fields.len() != 18
        || format.record_fields.len() != 7
        || format.record_flags.len() != 5
        || format.record_invariants.len() != 6
        || format.row_major_mapping.len() != 7
    {
        bail!(
            "EP-01 compound shard layout is not the exact 64-byte header and 32-byte record contract"
        )
    }
    if checkpoint.peak_regular_files != 6
        || checkpoint.regular_file_roles.len() != 6
        || checkpoint.header_fields.len() != 25
        || checkpoint.record_fields.len() != 5
        || checkpoint.commit_slot_fields.len() != 7
        || checkpoint.peak_regular_files > authority.gates.checkpoint.maximum_regular_files
    {
        bail!("EP-01 ordinal checkpoint contract is incomplete or exceeds its file gate")
    }
    if runtime.directory_slot_fields.len() != 8
        || runtime.page_record_fields.len() != 16
        || runtime.binding_groups.len() != 3
        || runtime.all_invalid_payload_slot_bytes != 0
        || runtime.directory_slot_fields.len() * size_of::<u32>()
            != usize::try_from(authority.gates.gpu.maximum_directory_slot_bytes)
                .context("EP-01 directory-slot gate does not fit usize")?
        || runtime.page_record_fields.len() * size_of::<u32>()
            != usize::try_from(authority.gates.gpu.maximum_page_record_bytes)
                .context("EP-01 page-record gate does not fit usize")?
    {
        bail!("EP-01 GPU directory, page-record, or binding contract is incoherent")
    }
    if evidence.public_receipt_serializes_private_geometry
        || !evidence.raw_evidence_remains_external
        || !evidence
            .forbidden_sanitized_fields
            .iter()
            .any(|field| field == "candidate_content_generation")
        || !evidence
            .forbidden_sanitized_fields
            .iter()
            .any(|field| field == "candidate_package_set_generation")
    {
        bail!("EP-01 evidence privacy contract permits private geometry or lineage")
    }

    if partition.path_syntax != "dotted_path_in_merged_profile_and_selection_authority"
        || partition.headroom_paths != HEADROOM_PATHS
        || partition.engineering_ratio_direct_paths != ENGINEERING_RATIO_DIRECT_PATHS
        || partition.structural_direct_paths != STRUCTURAL_DIRECT_PATHS
        || partition.structural_exact_eq_paths != STRUCTURAL_EXACT_EQ_PATHS
        || partition.structural_lte_paths != STRUCTURAL_LTE_PATHS
        || partition.structural_zero_eq_paths != STRUCTURAL_ZERO_EQ_PATHS
        || partition.headroom_comparison != "checked_u128_observed_times_10000_lte_limit_times_8000"
        || partition.engineering_ratio_comparison
            != "direct_observed_lte_limit_without_additional_headroom"
        || partition.structural_comparison
            != "operators_are_explicit_per_exhaustive_exact_eq_direct_lte_and_zero_eq_subpartitions_without_name_inference"
        || partition.partition_path_count != 74
    {
        bail!("EP-01 comparator paths or comparison classes differ from the exhaustive partition")
    }
    let all_paths = partition
        .headroom_paths
        .iter()
        .chain(&partition.engineering_ratio_direct_paths)
        .chain(&partition.structural_direct_paths)
        .collect::<std::collections::BTreeSet<_>>();
    if all_paths.len() != 74 {
        bail!("EP-01 comparator partition paths must be pairwise disjoint")
    }
    let structural_operators = partition
        .structural_exact_eq_paths
        .iter()
        .chain(&partition.structural_lte_paths)
        .chain(&partition.structural_zero_eq_paths)
        .collect::<std::collections::BTreeSet<_>>();
    let structural_paths = partition
        .structural_direct_paths
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    if structural_operators.len() != 39 || structural_operators != structural_paths {
        bail!("EP-01 structural comparator operators must partition all 39 structural paths")
    }
    Ok(())
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
    fn comparator_partition_is_exhaustive_disjoint_and_uses_explicit_operators() {
        let authority: SelectionAuthority = serde_json::from_slice(AUTHORITY_BYTES).unwrap();
        let partition = &authority.comparator_partition;
        assert_eq!(partition.headroom_paths, HEADROOM_PATHS);
        assert_eq!(
            partition.engineering_ratio_direct_paths,
            ENGINEERING_RATIO_DIRECT_PATHS
        );
        assert_eq!(partition.structural_direct_paths, STRUCTURAL_DIRECT_PATHS);
        assert_eq!(
            partition.structural_exact_eq_paths,
            STRUCTURAL_EXACT_EQ_PATHS
        );
        assert_eq!(partition.structural_lte_paths, STRUCTURAL_LTE_PATHS);
        assert_eq!(partition.structural_zero_eq_paths, STRUCTURAL_ZERO_EQ_PATHS);
        assert_eq!(partition.headroom_paths.len(), 25);
        assert_eq!(partition.engineering_ratio_direct_paths.len(), 10);
        assert_eq!(partition.structural_direct_paths.len(), 39);
        assert_eq!(partition.structural_exact_eq_paths.len(), 11);
        assert_eq!(partition.structural_lte_paths.len(), 26);
        assert_eq!(partition.structural_zero_eq_paths.len(), 2);
        assert!(
            partition
                .headroom_paths
                .iter()
                .any(|path| path == "gates.checkpoint.maximum_regular_files")
        );
        assert!(
            !partition
                .headroom_paths
                .iter()
                .any(|path| path == "gates.format_and_index.compact_inner_record_bytes")
        );

        let effective_wall = 720_000_000_000_u128;
        let wall_limit = u128::from(authority.gates.preprocessing.maximum_wall_time_ns);
        assert_eq!(effective_wall * 10_000, wall_limit * 8_000);
        assert!((effective_wall + 1) * 10_000 > wall_limit * 8_000);
        let effective_cpu = 960_000_000_000_u128;
        let cpu_limit = u128::from(authority.gates.preprocessing.maximum_process_cpu_time_ns);
        assert_eq!(effective_cpu * 10_000, cpu_limit * 8_000);
        assert!((effective_cpu + 1) * 10_000 > cpu_limit * 8_000);
    }

    #[test]
    fn both_candidate_edges_prove_the_shard_index_and_zstd_bounds() {
        let authority: SelectionAuthority = serde_json::from_slice(AUTHORITY_BYTES).unwrap();
        let gates = &authority.gates.format_and_index;
        assert_eq!(authority.trace_derivation.candidate_key_bytes, 60);
        assert_eq!(
            authority
                .trace_derivation
                .candidate_key_field_widths
                .iter()
                .sum::<u64>(),
            60
        );

        for edge in CANDIDATE_EDGES.map(u64::from) {
            let voxels = edge.pow(3);
            let logical_f32_brick = voxels * 4 + voxels.div_ceil(8);
            let inner_per_axis =
                u64::from(authority.fixed_comparison_defaults.outer_shard_edge) / edge;
            let records = inner_per_axis.pow(3);
            let decoded_shard = logical_f32_brick * records;
            assert_eq!(decoded_shard, gates.maximum_decoded_shard_bytes);

            let index = gates.shard_index_header_bytes + gates.compact_inner_record_bytes * records;
            assert!(index <= gates.maximum_per_shard_index_bytes);
            if edge == 32 {
                assert_eq!(index, gates.maximum_per_shard_index_bytes);
            }

            let zstd_bound = logical_f32_brick
                + (logical_f32_brick >> 8)
                + if logical_f32_brick < 128 * 1024 {
                    ((128 * 1024) - logical_f32_brick) >> 11
                } else {
                    0
                };
            let complete_encoded_bound = index + zstd_bound * records;
            assert!(complete_encoded_bound <= gates.maximum_encoded_shard_bytes);
        }
    }

    #[test]
    fn checkpoint_and_gpu_layouts_match_their_exact_byte_contracts() {
        let authority: SelectionAuthority = serde_json::from_slice(AUTHORITY_BYTES).unwrap();
        let checkpoint = &authority.checkpoint_contract;
        assert_eq!(checkpoint.peak_regular_files, 6);
        assert_eq!(checkpoint.regular_file_roles.len(), 6);
        assert_eq!(checkpoint.commit_slot_fields.len(), 7);
        assert_eq!(2 * 128, 256);
        assert_eq!(
            authority.runtime_gpu_contract.directory_slot_fields.len() * 4,
            32
        );
        assert_eq!(
            authority.runtime_gpu_contract.page_record_fields.len() * 4,
            64
        );
        assert_eq!(
            authority.gates.checkpoint.header_bytes
                + authority.gates.checkpoint.read_window_bytes
                    * authority.gates.checkpoint.resident_read_windows,
            authority.gates.checkpoint.maximum_resident_bytes
        );
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

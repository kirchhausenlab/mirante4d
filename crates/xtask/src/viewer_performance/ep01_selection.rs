use std::{mem::size_of, sync::OnceLock};

use anyhow::{Context, bail};
use mirante4d_identity::Sha256Hasher;
use serde::{Deserialize, Serialize};

const AUTHORITY_BYTES: &[u8] =
    include_bytes!("../../../../verification/viewer-performance-ep01-selection.json");
const COMMON_SCHEMA_BYTES: &[u8] =
    include_bytes!("../../../../verification/schemas/viewer-performance-ep01-common.schema.json");
const FAILURE_EVIDENCE_SCHEMA_BYTES: &[u8] = include_bytes!(
    "../../../../verification/schemas/viewer-performance-ep01-failure-evidence.schema.json"
);
const PROJECTION_INPUT_SCHEMA_BYTES: &[u8] = include_bytes!(
    "../../../../verification/schemas/viewer-performance-ep01-projection-input.schema.json"
);
const SOURCE_INVENTORY_SCHEMA_BYTES: &[u8] = include_bytes!(
    "../../../../verification/schemas/viewer-performance-ep01-source-inventory.schema.json"
);
const PACKAGE_VALIDATION_SCHEMA_BYTES: &[u8] = include_bytes!(
    "../../../../verification/schemas/viewer-performance-ep01-package-validation.schema.json"
);
const BUILD_IMPORT_SCHEMA_BYTES: &[u8] = include_bytes!(
    "../../../../verification/schemas/viewer-performance-ep01-build-import.schema.json"
);
const TRACE_SCHEMA_BYTES: &[u8] =
    include_bytes!("../../../../verification/schemas/viewer-performance-ep01-trace.schema.json");
const RUNTIME_GPU_SCHEMA_BYTES: &[u8] = include_bytes!(
    "../../../../verification/schemas/viewer-performance-ep01-runtime-gpu.schema.json"
);
const GATE_OBSERVATION_SCHEMA_BYTES: &[u8] = include_bytes!(
    "../../../../verification/schemas/viewer-performance-ep01-gate-observation.schema.json"
);
const AUTHORITY_SCHEMA: &str = "mirante4d-viewer-performance-ep01-selection-authority";
const AUTHORITY_SCHEMA_VERSION: u64 = 3;
const COMMITTED_AUTHORITY_SHA256: &str =
    "bcba48d78e0e522527136e90fa54ef28fb513feb8154d5ea1157ec9ab8ee4314";
const COMMITTED_AUTHORITY_SEMANTIC_SHA256: &str =
    "7d2f2906650e7c81d02be5e447d9c345f836d0ab1b5df0c80b0227319face9b7";

const GEOMETRY_AUTHORITIES: [&str; 4] = [
    "mirante4d-viewer-performance-workload-bundle-4",
    "mirante4d-viewer-performance-script-bundle-3",
    "mirante4d-viewer-performance-oracle-bundle-3",
    "mirante4d-viewer-performance-ep01-projection-input-1",
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
const TRACE_DIGEST_FIELDS: [&str; 9] = [
    "qualification_profile_contract_sha256_raw_bytes",
    "ep01_selection_authority_sha256_raw_bytes",
    "projection_input_sidecar_sha256_raw_bytes",
    "candidate_cubic_brick_edge_u32_le",
    "trace_family_tag_u8",
    "candidate_package_set_generation_raw_bytes",
    "unique_key_count_u64_le",
    "canonical_candidate_BrickKey_entries",
    "unique_payload_bytes_u64_le",
];
const ORDERED_TRACE_DIGEST_FIELDS: [&str; 8] = [
    "qualification_profile_contract_sha256_raw_bytes",
    "ep01_selection_authority_sha256_raw_bytes",
    "projection_input_sidecar_sha256_raw_bytes",
    "candidate_cubic_brick_edge_u32_le",
    "trace_family_tag_u8",
    "candidate_package_set_generation_raw_bytes",
    "state_count_u64_le",
    "canonical_state_frames",
];
const ORDERED_STATE_FRAME_FIELDS: [&str; 8] = [
    "state_ordinal_u64_le",
    "scenario_tag_u8",
    "phase_ordinal_u32_le",
    "state_kind_u8",
    "sample_ordinal_u64_le",
    "active_package_role_tag_u8",
    "state_unique_key_count_u64_le",
    "state_typed_sorted_unique_candidate_BrickKey_entries",
];
const SCENARIO_TAGS: [&str; 12] = [
    "0:RZ",
    "1:ZB",
    "2:RO",
    "3:ST",
    "4:NO",
    "5:FC",
    "6:VM",
    "7:PT",
    "8:VV",
    "9:IP",
    "10:analysis_oracle",
    "11:verification_catalog",
];
const STATE_KIND_TAGS: [&str; 4] = [
    "0:scenario_initial_with_sample_ordinal_u64_max",
    "1:after_input_sample_with_zero_based_sample_ordinal",
    "2:phase_end_with_sample_ordinal_u64_max",
    "3:synthetic_analysis_or_verification_with_request_ordinal",
];
const PACKAGE_ROLE_TAGS: [&str; 2] = ["0:representative_package", "1:supporting_temporal_package"];
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
const STORAGE_PROFILES_BY_EDGE: [&str; 2] = [
    "32:m4d-compound-brick-local-b32-1.0",
    "64:m4d-compound-brick-local-b64-1.0",
];
const FIXED_CONTROL_PATHS: [&str; 7] = [
    "m4d/profile.json",
    "m4d/science.json",
    "m4d/display.json",
    "m4d/records/r00000000.json",
    "m4d/records/r00000001.json",
    "m4d/records/r00000002.json",
    "m4d/manifest/root.json",
];
const REQUIRED_CAPABILITIES: [&str; 4] = [
    "m4d.bit-validity.v1",
    "m4d.compound-brick.v1",
    "m4d.identity.v1",
    "m4d.strict-profile.v1",
];
const OBSERVATION_UNITS: [&str; 8] = [
    "bytes",
    "count",
    "nanoseconds",
    "seconds",
    "basis_points",
    "bytes_per_byte",
    "requests_per_brick",
    "attempts_per_brick",
];
const RAW_RATIO_PATHS: [&str; 11] = [
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
    "gates.plane_amplification.maximum_range_requests_per_new_brick",
];
const PRIMITIVE_OBSERVATION_TIE_KEY: [&str; 5] = [
    "package_role_tag_u8_or_255",
    "trace_family_tag_u8_or_255",
    "state_ordinal_u64_or_u64_max",
    "protocol_sample_ordinal_u64_or_u64_max",
    "within_state_observation_ordinal_u64",
];
const RECEIPT_COMPARATORS: [&str; 4] = ["headroom_lte", "direct_lte", "exact_eq", "zero_eq"];
const CANONICAL_SOURCE_FILE_NAMES: [&str; 2] = ["canonical-data", "canonical-state"];
const CHECKPOINT_FILE_ROLES: [&str; 6] = [
    "canonical_data",
    "canonical_state",
    "ordinal_header",
    "ordinal_records",
    "ordinal_payload",
    "ordinal_commit",
];
const GPU_DIRECTORY_SLOT_FIELDS: [&str; 8] = [
    "0:runtime_generation_id_u32",
    "4:compact_layer_id_u32",
    "8:compact_time_id_u32",
    "12:compact_scale_id_u32",
    "16:brick_z_u32",
    "20:brick_y_u32",
    "24:brick_x_u32",
    "28:page_record_index_plus_one_u32",
];
const GPU_PAGE_RECORD_FIELDS: [&str; 16] = [
    "0:runtime_generation_id_u32",
    "4:flags_u32",
    "8:payload_segment_u32",
    "12:scalar_byte_offset_u32",
    "16:validity_byte_offset_u32",
    "20:origin_z_u32",
    "24:origin_y_u32",
    "28:origin_x_u32",
    "32:extent_z_u32",
    "36:extent_y_u32",
    "40:extent_x_u32",
    "44:minimum_bits_u32",
    "48:maximum_bits_u32",
    "52:valid_count_u32",
    "56:scalar_bytes_u32",
    "60:validity_bytes_u32",
];
const GPU_ADAPTER_LIMIT_EVIDENCE_FIELDS: [&str; 11] = [
    "max_bind_groups",
    "max_bindings_per_bind_group",
    "max_uniform_buffers_per_shader_stage",
    "max_storage_buffers_per_shader_stage",
    "max_uniform_buffer_binding_size",
    "max_storage_buffer_binding_size",
    "max_buffer_size",
    "max_dynamic_uniform_buffers_per_pipeline_layout",
    "max_dynamic_storage_buffers_per_pipeline_layout",
    "min_uniform_buffer_offset_alignment",
    "min_storage_buffer_offset_alignment",
];
const RUNTIME_SAMPLE_ALLOWED_PATHS: [&str; 5] = [
    "every_gate_observation_contract_registry_authority_path_whose_primitive_source_mapping_allows_runtime_gpu",
    "diagnostics.pipeline_compile_startup_ns",
    "diagnostics.shader_memory_bytes",
    "diagnostics.variant_direct_kernel_gpu_ns",
    "diagnostics.variant_end_to_end_ns",
];
const TIMESTAMP_DERIVED_SAMPLE_PATHS: [&str; 4] = [
    "absolute_gates.maximum_plane_gpu_ns",
    "absolute_gates.maximum_mip_gpu_ns",
    "absolute_gates.maximum_dvr_gpu_ns",
    "absolute_gates.maximum_iso_gpu_ns",
];
const DIAGNOSTIC_FIRST_ARM_ORDER: [&str; 4] = [
    "selected_direct",
    "mixed_fallback",
    "mixed_fallback",
    "selected_direct",
];
const DIAGNOSTIC_AGGREGATE_METRIC_PATHS: [&str; 2] = [
    "diagnostics.variant_direct_kernel_gpu_ns",
    "diagnostics.variant_end_to_end_ns",
];
const SOURCE_INPUT_ROLES: [&str; 3] = [
    "0:representative_package",
    "1:supporting_temporal_package",
    "2:import_source",
];
const SOURCE_CAPTURE_ORDER: [&str; 6] = [
    "0:before:representative_package",
    "1:before:supporting_temporal_package",
    "2:before:import_source",
    "3:after:representative_package",
    "4:after:supporting_temporal_package",
    "5:after:import_source",
];
const SOURCE_INVENTORY_LOCATOR_RULE: &str = "inside_the_private_evidence_bundle_resolve_the_fixed_source_inventory_jcs_sibling_against_the_held_canonical_parent_descriptor_without_symlink_traversal_create_exactly_once_with_O_WRONLY_O_CREAT_O_EXCL_O_CLOEXEC_O_NOFOLLOW_mode0600_require_open_descriptor_fstat_regular_file_nlink_exactly1_and_permission_bits_exactly0600_write_complete_canonical_bytes_file_sync_close_then_parent_directory_sync_before_raw_receipt_publication_before_initial_hash_parse_or_any_retained_audit_reopen_open_read_only_nofollow_from_the_same_held_parent_require_regular_nlink1_mode0600_and_the_same_device_inode_size_and_metadata_generation_before_and_after_the_bounded_read_reject_any_currentness_change_retain_immutably_for_audit_and_never_copy_the_private_sibling_when_copying_the_public_sanitized_receipt";
const POSITIVE_FINITE_F32_BITS_PATTERN: &str =
    "^(?!(?:00000000|7f[89a-f][0-9a-f]{5})$)[0-7][0-9a-f]{7}(?![\\s\\S])";
const COMMON_SCHEMA_ID: &str = "viewer-performance-ep01-common.schema.json";
const COMMON_SCHEMA_PATH: &str = "verification/schemas/viewer-performance-ep01-common.schema.json";
const FAILURE_EVIDENCE_SCHEMA_ID: &str =
    "mirante4d-viewer-performance-ep01-private-failure-evidence-1";
const FAILURE_EVIDENCE_SCHEMA_PATH: &str =
    "verification/schemas/viewer-performance-ep01-failure-evidence.schema.json";
const PROJECTION_INPUT_SCHEMA_ID: &str = "mirante4d-viewer-performance-ep01-projection-input-1";
const PROJECTION_INPUT_SCHEMA_PATH: &str =
    "verification/schemas/viewer-performance-ep01-projection-input.schema.json";
const SOURCE_INVENTORY_SCHEMA_ID: &str =
    "mirante4d-viewer-performance-ep01-source-inventory-evidence-1";
const SOURCE_INVENTORY_SCHEMA_PATH: &str =
    "verification/schemas/viewer-performance-ep01-source-inventory.schema.json";
const SCHEMA_DOCUMENT_ENCODING: &str = "UTF8_JSON_Schema_2020-12_without_BOM";
const ARTIFACT_INSTANCE_ENCODING: &str = "restricted_JCS";
const PRIMITIVE_SOURCE_KIND_TAGS: [&str; 4] = [
    "0:selection_authority",
    "1:package_validation",
    "2:build_import_accounting",
    "3:runtime_gpu",
];
const PRIMITIVE_SOURCE_MAPPING: [&str; 15] = [
    "EP01-G000:exactly1:build_import_accounting_or_runtime_gpu",
    "EP01-G001_through_EP01-G005:exactly1:runtime_gpu",
    "EP01-G006:exactly1:build_import_accounting_or_runtime_gpu",
    "EP01-G007_through_EP01-G019:exactly1:runtime_gpu",
    "EP01-G020_through_EP01-G021:exactly1:package_validation",
    "EP01-G022_through_EP01-G024:exactly1:build_import_accounting",
    "EP01-G025:exactly1:runtime_gpu",
    "EP01-G026_through_EP01-G028:exactly1:package_validation",
    "EP01-G029:exactly2:package_validation_then_build_import_accounting",
    "EP01-G030_through_EP01-G034:exactly1:runtime_gpu",
    "EP01-G035:exactly1:selection_authority:/gates/headroom/minimum_latency_basis_points",
    "EP01-G036:exactly1:selection_authority:/gates/headroom/minimum_resource_basis_points",
    "EP01-G037_through_EP01-G042:exactly1:package_validation",
    "EP01-G043_through_EP01-G052:exactly1:build_import_accounting",
    "EP01-G053_through_EP01-G073:exactly1:runtime_gpu",
];

struct ArtifactSchemaExpectation {
    logical_role_tag: u8,
    logical_role: &'static str,
    payload_schema: &'static str,
    schema_id: &'static str,
    schema_path: &'static str,
    artifact_cardinality: u64,
    bytes: &'static [u8],
}

const ARTIFACT_SCHEMA_EXPECTATIONS: [ArtifactSchemaExpectation; 5] = [
    ArtifactSchemaExpectation {
        logical_role_tag: 0,
        logical_role: "package_validation",
        payload_schema: "mirante4d-viewer-performance-ep01-package-validation-evidence-1",
        schema_id: "viewer-performance-ep01-package-validation.schema.json",
        schema_path: "verification/schemas/viewer-performance-ep01-package-validation.schema.json",
        artifact_cardinality: 4,
        bytes: PACKAGE_VALIDATION_SCHEMA_BYTES,
    },
    ArtifactSchemaExpectation {
        logical_role_tag: 1,
        logical_role: "build_import_accounting",
        payload_schema: "mirante4d-viewer-performance-ep01-build-import-evidence-1",
        schema_id: "viewer-performance-ep01-build-import.schema.json",
        schema_path: "verification/schemas/viewer-performance-ep01-build-import.schema.json",
        artifact_cardinality: 2,
        bytes: BUILD_IMPORT_SCHEMA_BYTES,
    },
    ArtifactSchemaExpectation {
        logical_role_tag: 2,
        logical_role: "ordered_unique_trace",
        payload_schema: "mirante4d-viewer-performance-ep01-trace-evidence-1",
        schema_id: "viewer-performance-ep01-trace.schema.json",
        schema_path: "verification/schemas/viewer-performance-ep01-trace.schema.json",
        artifact_cardinality: 2,
        bytes: TRACE_SCHEMA_BYTES,
    },
    ArtifactSchemaExpectation {
        logical_role_tag: 3,
        logical_role: "runtime_gpu",
        payload_schema: "mirante4d-viewer-performance-ep01-runtime-gpu-evidence-1",
        schema_id: "viewer-performance-ep01-runtime-gpu.schema.json",
        schema_path: "verification/schemas/viewer-performance-ep01-runtime-gpu.schema.json",
        artifact_cardinality: 2,
        bytes: RUNTIME_GPU_SCHEMA_BYTES,
    },
    ArtifactSchemaExpectation {
        logical_role_tag: 4,
        logical_role: "gate_observations",
        payload_schema: "mirante4d-viewer-performance-ep01-gate-observation-evidence-1",
        schema_id: "viewer-performance-ep01-gate-observation.schema.json",
        schema_path: "verification/schemas/viewer-performance-ep01-gate-observation.schema.json",
        artifact_cardinality: 2,
        bytes: GATE_OBSERVATION_SCHEMA_BYTES,
    },
];

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
    candidate_package_contract: CandidatePackageContract,
    pyramid_contract: PyramidContract,
    compound_shard_contract: CompoundShardContract,
    accounting_contract: AccountingContract,
    source_currentness_contract: SourceCurrentnessContract,
    checkpoint_contract: CheckpointContract,
    runtime_gpu_contract: RuntimeGpuContract,
    evidence_contract: EvidenceContract,
    required_trace_families: Vec<String>,
    gate_observation_contract: GateObservationContract,
    comparator_partition: ComparatorPartition,
    gates: SelectionGates,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CandidateIdentity {
    scientific_content_identity_rule: String,
    scientific_content_identity_bytes: String,
    pyramid_recipe_digest_scheme: String,
    pyramid_recipe_body_rule: String,
    pyramid_recipe_wire_rule: String,
    pyramid_recipe_operation_registry_preimage: String,
    pyramid_recipe_operation_registry_sha256: String,
    pyramid_recipe_operation_fields: Vec<String>,
    pyramid_recipe_parameter_fields: Vec<String>,
    pyramid_recipe_fields: Vec<String>,
    candidate_geometry_digest_scheme: String,
    candidate_geometry_digest_preimage: Vec<String>,
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
    ordered_trace_digest_domain: String,
    ordered_trace_digest_fields: Vec<String>,
    ordered_state_frame_fields: Vec<String>,
    scenario_tags: Vec<String>,
    state_kind_tags: Vec<String>,
    package_role_tags: Vec<String>,
    trace_family_tags: Vec<String>,
    trace_package_roles: Vec<String>,
    trace_package_set_generation_rule: String,
    package_role_state_rule: String,
    state_enumeration: String,
    state_field_rule: String,
    phase_end_validation: String,
    family_projection_rules: Vec<String>,
    family_state_selection: Vec<String>,
    projection_input_sidecar_schema: String,
    projection_input_sidecar_schema_path: String,
    projection_input_sidecar_schema_sha256: String,
    projection_input_sidecar_common_schema_path: String,
    projection_input_sidecar_common_schema_sha256: String,
    projection_input_sidecar_raw_receipt_relative_path: String,
    projection_input_sidecar_locator_rule: String,
    projection_input_sidecar_canonical_encoding: String,
    projection_input_sidecar_unknown_or_missing_fields_rejected: bool,
    projection_input_sidecar_rule: String,
    projection_input_rule: String,
    plane_projection_input_math_rule: String,
    volume_projection_input_math_rule: String,
    pixel_projection_rule: String,
    smooth_linear_rule: String,
    volume_traversal_rule: String,
    iso_support_rule: String,
    analysis_projection_rule: String,
    out_of_domain_support_adds_key: bool,
    unique_payload_bytes_rule: String,
    one_digest_per_candidate_and_trace_family: bool,
    ordered_and_unique_digest_per_candidate_and_trace_family: bool,
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
    engineering_ratio_arithmetic: String,
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
    codec_library: String,
    payload_integrity: String,
    gpu_payload: String,
    brick_summaries: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CandidatePackageContract {
    activation_scope: String,
    format_family: String,
    lifecycle: String,
    semantic_schema: String,
    storage_profiles_by_edge: Vec<String>,
    index_profile: String,
    identity_profile: String,
    interoperability: String,
    required_capabilities: Vec<String>,
    package_path_rule: String,
    control_encoding: String,
    fixed_control_paths: Vec<String>,
    profile_schema: String,
    profile_schema_version: u64,
    profile_fields: Vec<String>,
    profile_layer_fields: Vec<String>,
    profile_level_fields: Vec<String>,
    profile_identity_rule: String,
    profile_dynamic_encoding_rule: String,
    profile_exact_value_rule: String,
    science_control_rule: String,
    display_control_rule: String,
    candidate_role_geometry_admission_rule: String,
    provenance_record_rules: Vec<String>,
    portable_record_exact_rule: String,
    package_role_persistence: String,
    shard_path_grammar: String,
    shard_path_rule: String,
    manifest_schemas: Vec<String>,
    manifest_wire_rule: String,
    manifest_object_registry: Vec<String>,
    manifest_descriptor_fields: String,
    manifest_page_rule: String,
    manifest_root_rule: String,
    package_identity_rule: String,
    filesystem_closure_rule: String,
    physical_object_formula: String,
    package_byte_formula: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PyramidContract {
    basis_axis_order: String,
    norm_arithmetic: String,
    reduction_rule: String,
    reduction_factors: Vec<u64>,
    scale_chain_rule: String,
    odd_tail_rule: String,
    integer_mean_rule: String,
    float32_mean_rule: String,
    provisional_validity_rule: String,
    invalid_dilation_rule: String,
    final_invalid_scalar_rule: String,
    centered_affine_rule: String,
    summary_rule: String,
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
    strict_frame_decode_rule: String,
    zstd_compress_bound_rule: String,
    crc32c_rule: String,
    region_order: String,
    header_magic: String,
    header_fields: Vec<String>,
    record_fields: Vec<String>,
    record_flags: Vec<String>,
    legal_record_flag_words: Vec<String>,
    record_invariants: Vec<String>,
    row_major_mapping: Vec<String>,
    shard_profile_binding_rule: String,
    dtype_codes: Vec<String>,
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
    format_role_aggregation: String,
    plane_useful_bytes: String,
    plane_encoded_bytes: String,
    plane_fetched_bytes: String,
    plane_decoded_bytes: String,
    plane_uploaded_bytes: String,
    plane_amplification_aggregation: String,
    checkpoint_metadata_resident_bytes: String,
    package_ratio_gates_are_candidate_admission_not_universal_format_guarantees: bool,
    checkpoint_batch_payload_is_separate_from_resident_header_and_read_windows: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SourceCurrentnessContract {
    source_inventory_scheme: String,
    metadata_generation_scheme: String,
    maximum_entries_per_input: u64,
    stream_buffer_bytes: u64,
    source_input_roles: Vec<String>,
    capture_order: Vec<String>,
    capture_boundary_rule: String,
    open_and_stream_rule: String,
    content_inventory_rule: String,
    metadata_generation_rule: String,
    input_binding_rule: String,
    comparison_rule: String,
    source_preservation_commitment_rule: String,
    candidate_package_inventory_rule: String,
    candidate_package_commitment_rule: String,
    raw_receipt_closure_rule: String,
    privacy_rule: String,
    source_access_is_read_only: bool,
    mismatch_allows_selection_receipt: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CheckpointContract {
    peak_regular_files: u64,
    regular_file_roles: Vec<String>,
    scope_rule: String,
    open_security_rule: String,
    canonical_source_file_names: Vec<String>,
    canonical_source_state_header_fields: Vec<String>,
    canonical_source_data_rule: String,
    canonical_source_state_record_fields: Vec<String>,
    canonical_source_batch_digest_rule: String,
    canonical_source_durability_rule: String,
    canonical_source_recovery_rule: String,
    header_fields: Vec<String>,
    header_digest_rule: String,
    plan_commitment_rule: String,
    ordinal_map_rule: String,
    ordinal_map_commitment_rule: String,
    compact_record_contract_commitment_rule: String,
    planned_payload_rule: String,
    ordered_committer_rule: String,
    record_fields: Vec<String>,
    record_payload_rule: String,
    record_tag_rule: String,
    commit_rule: String,
    commit_slot_fields: Vec<String>,
    chain_rule: String,
    commit_slot_rule: String,
    commit_count_rule: String,
    batch_rule: String,
    durability_order: String,
    recovery_rule: String,
    failure_rule: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RuntimeGpuContract {
    per_brick_epoch_scope: String,
    compact_id_rule: String,
    harness_artifact_rule: String,
    layout_binding_evidence_rule: String,
    adapter_limit_evidence_fields: Vec<String>,
    adapter_limit_evidence_rule: String,
    timestamp_period_source_rule: String,
    timestamp_period_bits_rule: String,
    timestamp_tick_conversion_rule: String,
    runtime_state_binding_rule: String,
    runtime_execution_binding_rule: String,
    timestamp_derived_sample_paths: Vec<String>,
    timestamp_control_subtraction_rule: String,
    runtime_sample_allowed_paths: Vec<String>,
    runtime_sample_population_rule: String,
    runtime_variant_identity_rule: String,
    runtime_variant_state_rule: String,
    runtime_diagnostic_protocol: RuntimeDiagnosticProtocol,
    runtime_non_gate_diagnostic_rule: String,
    capacity_rule: String,
    directory_slot_fields: Vec<String>,
    directory_empty_and_tombstone: String,
    directory_hash_rule: String,
    directory_rule: String,
    residency_policy: String,
    page_and_segment_allocator_rule: String,
    publication_rule: String,
    page_record_fields: Vec<String>,
    page_flag_rule: String,
    buffer_word_encoding: String,
    page_coordinate_rule: String,
    page_summary_rule: String,
    payload_rule: String,
    all_invalid_page_rule: String,
    binding_groups: Vec<String>,
    descriptor_resolution_scope: String,
    pipeline_semantics: String,
    coordinated_frame_scope: String,
    maximum_staging_bytes_formula: String,
    all_invalid_payload_slot_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RuntimeDiagnosticProtocol {
    maximum_variants: u64,
    maximum_canonical_variant_bytes: u64,
    warmup_abba_blocks_per_variant: u64,
    warmup_pairs_per_variant: u64,
    measured_abba_blocks_per_variant: u64,
    pairs_per_abba_block: u64,
    measured_pairs_per_variant: u64,
    observations_per_arm_per_variant: u64,
    executions_per_pair: u64,
    execution_tickets_per_variant: u64,
    first_arm_order_within_block: Vec<String>,
    p95_population_size: u64,
    p95_nearest_rank_one_based: u64,
    p95_zero_based_sorted_index: u64,
    aggregate_metric_paths: Vec<String>,
    execution_ticket_domain: String,
    row_order_rule: String,
    population_rule: String,
    p95_rule: String,
    failure_rule: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EvidenceContract {
    raw_receipt_schema: String,
    sanitized_receipt_schema: String,
    failure_receipt_schema: String,
    canonical_encoding: String,
    validation_resource_contract: ValidationResourceContract,
    raw_receipt_fields: Vec<String>,
    sanitized_receipt_fields: Vec<String>,
    candidate_receipt_fields: Vec<String>,
    gate_row_fields: Vec<String>,
    gate_comparison_rule: String,
    gate_reason_codes: Vec<String>,
    completion_rule: String,
    private_commitment_nonce_rule: String,
    selection_recompute_rule: String,
    closure_rule: String,
    incomplete_rule: String,
    sanitized_fields: Vec<String>,
    forbidden_sanitized_fields: Vec<String>,
    receipt_schema_grammar: ReceiptSchemaGrammar,
    receipt_array_order: ReceiptArrayOrder,
    artifact_role_contracts: Vec<String>,
    artifact_common_schema_binding: ArtifactCommonSchemaBinding,
    failure_evidence_schema_binding: FailureEvidenceSchemaBinding,
    source_inventory_schema_binding: SourceInventorySchemaBinding,
    artifact_schema_bindings: Vec<ArtifactSchemaBinding>,
    receipt_hash_contract: ReceiptHashContract,
    public_projection_allowlist: Vec<String>,
    public_projection_forbidden: Vec<String>,
    public_receipt_serializes_private_geometry: bool,
    raw_evidence_remains_external: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ValidationResourceContract {
    maximum_candidate_artifact_bytes: u64,
    maximum_projection_input_sidecar_bytes: u64,
    maximum_source_inventory_sidecar_bytes: u64,
    maximum_raw_receipt_bytes: u64,
    maximum_public_projection_bytes: u64,
    maximum_sanitized_receipt_bytes: u64,
    maximum_private_failure_evidence_bytes: u64,
    maximum_failure_receipt_bytes: u64,
    maximum_schema_document_bytes: u64,
    maximum_variable_array_items: u64,
    maximum_layout_fields_per_binding: u64,
    maximum_diagnostic_variants: u64,
    maximum_canonical_diagnostic_variant_bytes: u64,
    maximum_instrumentation_overhead_pairs: u64,
    maximum_ascii_bytes: u64,
    file_admission_rule: String,
    stream_validation_rule: String,
    population_limit_rule: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ArtifactCommonSchemaBinding {
    schema_id: String,
    schema_path: String,
    schema_sha256: String,
    schema_document_encoding: String,
    instance_scalar_contract: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FailureEvidenceSchemaBinding {
    payload_schema: String,
    schema_path: String,
    schema_sha256: String,
    common_schema_path: String,
    common_schema_sha256: String,
    canonical_encoding: String,
    unknown_or_missing_fields_rejected: bool,
    private_failure_evidence_relative_path: String,
    locator_and_publication_rule: String,
    record_order_rule: String,
    bounded_failure_evidence_rule: String,
    failure_projection_rule: String,
    privacy_rule: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SourceInventorySchemaBinding {
    payload_schema: String,
    schema_path: String,
    schema_sha256: String,
    common_schema_path: String,
    common_schema_sha256: String,
    canonical_encoding: String,
    unknown_or_missing_fields_rejected: bool,
    private_source_inventory_relative_path: String,
    locator_and_publication_rule: String,
    capture_order_rule: String,
    comparison_rule: String,
    raw_receipt_closure_rule: String,
    privacy_rule: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ArtifactSchemaBinding {
    logical_role_tag: u8,
    logical_role: String,
    payload_schema: String,
    schema_path: String,
    schema_sha256: String,
    canonical_encoding: String,
    unknown_or_missing_fields_rejected: bool,
    artifact_cardinality: u64,
    package_role_rule: String,
    payload_array_order_rule: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ReceiptSchemaGrammar {
    objects_are_exact: bool,
    unknown_or_missing_fields_rejected: bool,
    field_token_grammar: String,
    scalar_types: ReceiptScalarTypes,
    enums: ReceiptEnums,
    objects: ReceiptObjects,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ReceiptScalarTypes {
    u8: String,
    u32: String,
    u64d: String,
    u128d: String,
    sha256: String,
    bytes32: String,
    git_oid: String,
    ascii: String,
    relative_path: String,
    package_id: String,
    r#bool: String,
    null: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ReceiptEnums {
    candidate_edge: Vec<u32>,
    selection: Vec<Option<u32>>,
    package_role: Vec<String>,
    trace_family: Vec<String>,
    comparator: Vec<String>,
    unit: Vec<String>,
    artifact_role: Vec<String>,
    failure_stage: Vec<String>,
    failure_reason: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ReceiptObjects {
    raw_revision: Vec<String>,
    raw_bindings: Vec<String>,
    raw_package: Vec<String>,
    raw_trace: Vec<String>,
    raw_gate: Vec<String>,
    raw_candidate: Vec<String>,
    artifact: Vec<String>,
    raw_receipt: Vec<String>,
    public_revision: Vec<String>,
    public_trace: Vec<String>,
    public_gate: Vec<String>,
    public_candidate: Vec<String>,
    public_projection: Vec<String>,
    sanitized_receipt: Vec<String>,
    failure_revision: Vec<String>,
    failure_receipt: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ReceiptArrayOrder {
    candidates: String,
    packages: String,
    traces: String,
    gate_rows: String,
    artifact_manifest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ReceiptHashContract {
    external_raw_gate_sha256: String,
    external_raw_candidate_sha256: String,
    external_failure_evidence_sha256: String,
    source_preservation_commitment_sha256: String,
    sanitized_projection_sha256: String,
    external_raw_receipt_sha256: String,
    artifact_sha256: String,
    construction_order: Vec<String>,
    failure_construction_order: Vec<String>,
    artifact_manifest_excludes: Vec<String>,
    standalone_public_projection_file_allowed: bool,
    failure_receipt_mutually_exclusive_with_selection_receipts: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct GateObservationContract {
    registry_row_grammar: String,
    registry_order: String,
    units: Vec<String>,
    raw_ratio_paths: Vec<String>,
    denominator_contract: DenominatorContract,
    comparison_matrix: ComparisonMatrix,
    primitive_observation_tie_key: Vec<String>,
    primitive_source_kind_tags: Vec<String>,
    primitive_source_commitment_rule: String,
    primitive_source_mapping: Vec<String>,
    ratio_aggregation_contract: RatioAggregationContract,
    cold_plane_population: ColdPlanePopulation,
    instrumentation_overhead_operand: InstrumentationOverheadOperand,
    invalid_observation_behavior: InvalidObservationBehavior,
    role_and_run_aggregation: String,
    p95_rule: String,
    ratio_rule: String,
    missing_rule: String,
    registry: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DenominatorContract {
    raw_ratio_paths: String,
    every_other_registry_path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ComparisonMatrix {
    headroom_lte: String,
    direct_lte_basis_points: String,
    direct_lte_all_other_units: String,
    exact_eq: String,
    zero_eq: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RatioAggregationContract {
    comparison: String,
    winner: String,
    equal_ratio_tie: String,
    completion_order_used: bool,
    gcd_reduction_allowed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ColdPlanePopulation {
    membership: String,
    clear_cpu_decoded_residency: bool,
    clear_gpu_directory_pages_and_payloads: bool,
    clear_shard_prefix_index_and_payload_cohort_cache: bool,
    #[serde(rename = "require_no_live_or_queued_BrickKey_work")]
    require_no_live_or_queued_brick_key_work: bool,
    retain_open_verified_package_controls: bool,
    retain_warmed_pipelines_and_static_controls: bool,
    #[serde(rename = "retain_bound_OS_cache_condition")]
    retain_bound_os_cache_condition: bool,
    require_positive_useful_logical_sample_bytes: bool,
    #[serde(rename = "require_positive_unique_newly_requested_BrickKey_count")]
    require_positive_unique_newly_requested_brick_key_count: bool,
    zero_or_missing_operand_behavior: String,
    untagged_state_eligible: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct InstrumentationOverheadOperand {
    per_bound_pair_numerator: String,
    per_bound_pair_denominator: String,
    control_must_be_positive: bool,
    aggregation: String,
    maximum_raw_pairs: u64,
    raw_pair_population_rule: String,
    raw_pair_order_rule: String,
    execution_ticket_domain: String,
    raw_pair_arithmetic_rule: String,
    raw_pair_source_rule: String,
    invalid_pair_allows_selection_receipt: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct InvalidObservationBehavior {
    selection_receipt_allowed: bool,
    sanitized_selection_receipt_allowed: bool,
    failure_receipt_required: bool,
    valid_failed_comparison_allowed_in_selection_receipt: bool,
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
    if identity.scientific_content_identity_bytes
        != "existing_ScientificContentId_digest_raw32_not_typed_ASCII"
        || identity.pyramid_recipe_operation_registry_sha256
            != "b046020e6ca976e7289b31200da1991dfd063b3e6afd142f0f8d529c13b22580"
        || identity.candidate_content_generation_bytes != 32
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
    if trace.scheme != "mirante4d-ep01-brickkey-trace-projection-2"
        || trace.geometry_authorities != GEOMETRY_AUTHORITIES
        || trace.candidate_key_fields != CANDIDATE_KEY_FIELDS
        || trace.candidate_key_field_widths != CANDIDATE_KEY_FIELD_WIDTHS
        || trace.candidate_key_bytes != 60
        || trace.canonical_order
            != "generation_raw_bytes_then_unsigned_numeric_layer_time_scale_z_y_x"
        || trace.candidate_key_binary_encoding
            != "generation_raw_32_bytes_then_layer_u32_le_time_u64_le_scale_u32_le_z_u32_le_y_u32_le_x_u32_le"
        || trace.deduplication
            != "within_each_state_sort_typed_and_deduplicate_for_ordered_replay_then_across_complete_family_sort_typed_and_deduplicate_for_unique_accounting"
        || trace.trace_digest_scheme
            != "two_SHA256_digests_ordered_state_framed_replay_and_unique_sorted_accounting"
        || trace.trace_digest_domain
            != "exact_ASCII_mirante4d-ep01-candidate-brickkey-trace-unique-v1_then_00"
        || trace.trace_digest_fields != TRACE_DIGEST_FIELDS
        || trace.ordered_trace_digest_domain
            != "exact_ASCII_mirante4d-ep01-candidate-brickkey-replay-v1_then_00"
        || trace.ordered_trace_digest_fields != ORDERED_TRACE_DIGEST_FIELDS
        || trace.ordered_state_frame_fields != ORDERED_STATE_FRAME_FIELDS
        || trace.scenario_tags != SCENARIO_TAGS
        || trace.state_kind_tags != STATE_KIND_TAGS
        || trace.package_role_tags != PACKAGE_ROLE_TAGS
        || trace.trace_family_tags != TRACE_FAMILY_TAGS
        || trace.trace_package_roles != CANDIDATE_PACKAGE_ROLES
        || trace.one_digest_per_candidate_and_trace_family
        || !trace.ordered_and_unique_digest_per_candidate_and_trace_family
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
            != "exact_per_path_numerator_and_denominator_from_gate_observation_registry_without_rounded_quotient"
        || rule.engineering_ratio_arithmetic
            != "basis_points_checked_u128_numerator_times_10000_lte_limit_times_denominator_bytes_per_byte_or_unitless_checked_numerator_lte_limit_times_denominator"
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
        || defaults.codec_library != "libzstd-1.5.7-via-zstd-0.13.3-zstd-safe-7.2.4-zstd-sys-2.0.16"
        || defaults.payload_integrity != "crc32c_reflected_Castagnoli"
        || defaults.gpu_payload
            != "persisted_little_endian_scalar_buffer_with_four_byte_GPU_region_alignment"
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
            "EP-01 selection authority semantic contract changed without updating its exact commitment: observed {semantic}"
        )
    }
    Ok(())
}

fn authority_semantic_fingerprint_sha256(authority: &SelectionAuthority) -> anyhow::Result<String> {
    let encoded = serde_json::to_vec(authority)
        .context("EP-01 selection authority semantic serialization failed")?;
    Ok(Sha256Hasher::digest(encoded).to_string())
}

fn receipt_schema_field_names(fields: &[String]) -> Option<Vec<&str>> {
    fields
        .iter()
        .map(|field| field.split_once(':').map(|(name, _)| name))
        .collect()
}

fn schema_uses_exact_common_ref(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(values) => values.iter().any(schema_uses_exact_common_ref),
        serde_json::Value::Object(entries) => entries.iter().any(|(key, value)| {
            (key == "$ref"
                && value.as_str().is_some_and(|reference| {
                    reference.starts_with(&format!("{COMMON_SCHEMA_ID}#/$defs/"))
                }))
                || schema_uses_exact_common_ref(value)
        }),
        _ => false,
    }
}

fn parse_positive_finite_timestamp_period_bits(encoded: &str) -> anyhow::Result<u32> {
    if encoded.len() != 8
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("timestamp period bits must be exactly eight lowercase hexadecimal digits")
    }
    let bits =
        u32::from_str_radix(encoded, 16).context("timestamp period bits are not a valid u32")?;
    let value = f32::from_bits(bits);
    if !value.is_finite() || value <= 0.0 {
        bail!("timestamp period must decode to a finite strictly positive IEEE-754 f32")
    }
    Ok(bits)
}

fn timestamp_ticks_to_nanoseconds(
    encoded_period_bits: &str,
    start_ticks: u64,
    end_ticks: u64,
) -> anyhow::Result<u64> {
    let bits = parse_positive_finite_timestamp_period_bits(encoded_period_bits)?;
    let exponent = (bits >> 23) & 0xff;
    let fraction = bits & 0x7f_ffff;
    let (significand, binary_shift) = if exponent == 0 {
        (u128::from(fraction), -149_i32)
    } else {
        (
            u128::from((1_u32 << 23) | fraction),
            i32::try_from(exponent).expect("u8 exponent fits i32") - 150,
        )
    };
    let delta = end_ticks
        .checked_sub(start_ticks)
        .context("timestamp interval tick counter moved backwards")?;
    let product = u128::from(delta)
        .checked_mul(significand)
        .context("timestamp interval exact rational product overflowed u128")?;

    if binary_shift >= 0 {
        let shift = u32::try_from(binary_shift).expect("nonnegative i32 fits u32");
        if shift >= 64 || product > u128::from(u64::MAX >> shift) {
            bail!("timestamp interval nanoseconds exceed u64")
        }
        return u64::try_from(product << shift)
            .context("timestamp interval nanoseconds do not fit u64");
    }

    let denominator_shift = binary_shift
        .checked_neg()
        .and_then(|shift| u32::try_from(shift).ok())
        .context("timestamp interval denominator shift is invalid")?;
    if denominator_shift >= 128 {
        return Ok(0);
    }
    let denominator = 1_u128 << denominator_shift;
    let quotient = product / denominator;
    let remainder = product % denominator;
    let doubled_remainder = remainder
        .checked_mul(2)
        .context("timestamp interval rounding remainder overflowed u128")?;
    let round_up =
        doubled_remainder > denominator || (doubled_remainder == denominator && quotient & 1 == 1);
    let rounded = quotient
        .checked_add(u128::from(round_up))
        .context("timestamp interval rounded value overflowed u128")?;
    u64::try_from(rounded).context("timestamp interval nanoseconds do not fit u64")
}

fn validate_embedded_schema(
    bytes: &[u8],
    expected_schema_id: &str,
    expected_instance_schema: Option<&str>,
) -> anyhow::Result<String> {
    let schema: serde_json::Value = serde_json::from_slice(bytes)
        .with_context(|| format!("EP-01 schema {expected_schema_id:?} is not strict valid JSON"))?;
    if schema.get("$schema").and_then(serde_json::Value::as_str)
        != Some("https://json-schema.org/draft/2020-12/schema")
        || schema.get("$id").and_then(serde_json::Value::as_str) != Some(expected_schema_id)
    {
        bail!("EP-01 schema document identity differs from its binding")
    }
    if let Some(instance_schema) = expected_instance_schema
        && (schema
            .pointer("/properties/schema/const")
            .and_then(serde_json::Value::as_str)
            != Some(instance_schema)
            || !schema_uses_exact_common_ref(&schema))
    {
        bail!("EP-01 instance schema name or shared common-schema reference is incoherent")
    }
    if expected_schema_id == "viewer-performance-ep01-runtime-gpu.schema.json"
        && (schema
            .pointer("/$defs/positive_finite_f32_bits/pattern")
            .and_then(serde_json::Value::as_str)
            != Some(POSITIVE_FINITE_F32_BITS_PATTERN)
            || parse_positive_finite_timestamp_period_bits("3f800000").is_err()
            || parse_positive_finite_timestamp_period_bits("00000001").is_err()
            || parse_positive_finite_timestamp_period_bits("00000000").is_ok()
            || parse_positive_finite_timestamp_period_bits("80000000").is_ok()
            || parse_positive_finite_timestamp_period_bits("7f800000").is_ok()
            || parse_positive_finite_timestamp_period_bits("7fc00000").is_ok()
            || timestamp_ticks_to_nanoseconds("3f800000", 7, 10).ok() != Some(3)
            || timestamp_ticks_to_nanoseconds("3f000000", 0, 1).ok() != Some(0)
            || timestamp_ticks_to_nanoseconds("3f000000", 0, 3).ok() != Some(2))
    {
        bail!(
            "EP-01 runtime schema does not enforce a positive finite timestamp-period bit pattern"
        )
    }
    let required_contains = |pointer: &str, field: &str| {
        schema
            .pointer(pointer)
            .and_then(serde_json::Value::as_array)
            .is_some_and(|values| values.iter().any(|value| value.as_str() == Some(field)))
    };
    if expected_schema_id == "viewer-performance-ep01-runtime-gpu.schema.json"
        && (schema
            .pointer("/properties/diagnostic_variants/maxItems")
            .and_then(serde_json::Value::as_u64)
            != Some(256)
            || schema
                .pointer("/properties/instrumentation_overhead_pairs/maxItems")
                .and_then(serde_json::Value::as_u64)
                != Some(8_192)
            || schema
                .pointer("/$defs/diagnostic_variant/properties/warmup_pairs/minItems")
                .and_then(serde_json::Value::as_u64)
                != Some(4)
            || schema
                .pointer("/$defs/diagnostic_variant/properties/warmup_pairs/maxItems")
                .and_then(serde_json::Value::as_u64)
                != Some(4)
            || schema
                .pointer("/$defs/diagnostic_variant/properties/measured_pairs/minItems")
                .and_then(serde_json::Value::as_u64)
                != Some(100)
            || schema
                .pointer("/$defs/diagnostic_variant/properties/measured_pairs/maxItems")
                .and_then(serde_json::Value::as_u64)
                != Some(100)
            || !required_contains("/$defs/sample/required", "timestamp_control_pair")
            || !required_contains(
                "/$defs/diagnostic_execution/required",
                "execution_binding_sha256",
            ))
    {
        bail!("EP-01 runtime schema does not freeze its bounded raw timing populations")
    }
    if expected_schema_id == "viewer-performance-ep01-build-import.schema.json"
        && (!required_contains("/required", "source_inventory_sidecar_sha256")
            || required_contains("/required", "source_unchanged"))
    {
        bail!("EP-01 build/import schema retained assertion-only source currentness")
    }
    if expected_schema_id == "viewer-performance-ep01-package-validation.schema.json"
        && (!required_contains("/required", "source_inventory_sidecar_sha256")
            || !required_contains("/$defs/checks/required", "package_currentness")
            || !required_contains(
                "/$defs/pre_package_inventory/required",
                "metadata_generation_sha256",
            )
            || !required_contains(
                "/$defs/post_package_inventory/required",
                "metadata_generation_sha256",
            ))
    {
        bail!("EP-01 package-validation schema lacks independently recomputable currentness")
    }
    if expected_schema_id == "viewer-performance-ep01-source-inventory.schema.json" {
        let expected_capture_identity = [
            (0, 0, 0, "representative_package"),
            (1, 0, 1, "supporting_temporal_package"),
            (2, 0, 2, "import_source"),
            (3, 1, 0, "representative_package"),
            (4, 1, 1, "supporting_temporal_package"),
            (5, 1, 2, "import_source"),
        ];
        let captures_are_exact = expected_capture_identity.iter().enumerate().all(
            |(index, (capture, phase, input, role))| {
                let prefix = format!("/properties/captures/prefixItems/{index}/allOf/1/properties");
                schema
                    .pointer(&format!("{prefix}/capture_ordinal/const"))
                    .and_then(serde_json::Value::as_u64)
                    == Some(*capture)
                    && schema
                        .pointer(&format!("{prefix}/phase_tag/const"))
                        .and_then(serde_json::Value::as_u64)
                        == Some(*phase)
                    && schema
                        .pointer(&format!("{prefix}/input_ordinal/const"))
                        .and_then(serde_json::Value::as_u64)
                        == Some(*input)
                    && schema
                        .pointer(&format!("{prefix}/input_role/const"))
                        .and_then(serde_json::Value::as_str)
                        == Some(*role)
            },
        );
        if schema
            .pointer("/properties/captures/minItems")
            .and_then(serde_json::Value::as_u64)
            != Some(6)
            || schema
                .pointer("/properties/captures/maxItems")
                .and_then(serde_json::Value::as_u64)
                != Some(6)
            || schema
                .pointer("/properties/captures/items")
                .and_then(serde_json::Value::as_bool)
                != Some(false)
            || !required_contains("/required", "qualification_profile_contract_sha256")
            || !required_contains("/required", "script_bundle_sha256")
            || !captures_are_exact
        {
            bail!("EP-01 source-inventory schema does not freeze its six-capture sandwich")
        }
    }
    Ok(Sha256Hasher::digest(bytes).to_string())
}

struct EmbeddedSchemaHashes {
    common: String,
    projection_input: String,
    source_inventory: String,
    failure_evidence: String,
    artifacts: Vec<String>,
}

fn embedded_schema_hashes() -> anyhow::Result<&'static EmbeddedSchemaHashes> {
    static HASHES: OnceLock<Result<EmbeddedSchemaHashes, String>> = OnceLock::new();
    let hashes = HASHES.get_or_init(|| {
        (|| -> anyhow::Result<EmbeddedSchemaHashes> {
            let common = validate_embedded_schema(COMMON_SCHEMA_BYTES, COMMON_SCHEMA_ID, None)?;
            let projection_input = validate_embedded_schema(
                PROJECTION_INPUT_SCHEMA_BYTES,
                "viewer-performance-ep01-projection-input.schema.json",
                Some(PROJECTION_INPUT_SCHEMA_ID),
            )?;
            let source_inventory = validate_embedded_schema(
                SOURCE_INVENTORY_SCHEMA_BYTES,
                "viewer-performance-ep01-source-inventory.schema.json",
                Some(SOURCE_INVENTORY_SCHEMA_ID),
            )?;
            let failure_evidence = validate_embedded_schema(
                FAILURE_EVIDENCE_SCHEMA_BYTES,
                "viewer-performance-ep01-failure-evidence.schema.json",
                Some(FAILURE_EVIDENCE_SCHEMA_ID),
            )?;
            let artifacts = ARTIFACT_SCHEMA_EXPECTATIONS
                .iter()
                .map(|expected| {
                    validate_embedded_schema(
                        expected.bytes,
                        expected.schema_id,
                        Some(expected.payload_schema),
                    )
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            Ok(EmbeddedSchemaHashes {
                common,
                projection_input,
                source_inventory,
                failure_evidence,
                artifacts,
            })
        })()
        .map_err(|error| format!("{error:#}"))
    });
    hashes
        .as_ref()
        .map_err(|error| anyhow::anyhow!(error.clone()))
}

fn validate_clarification_contracts(authority: &SelectionAuthority) -> anyhow::Result<()> {
    let identity = &authority.candidate_identity;
    let trace = &authority.trace_derivation;
    let package = &authority.candidate_package_contract;
    let pyramid = &authority.pyramid_contract;
    let format = &authority.compound_shard_contract;
    let accounting = &authority.accounting_contract;
    let source = &authority.source_currentness_contract;
    let checkpoint = &authority.checkpoint_contract;
    let runtime = &authority.runtime_gpu_contract;
    let evidence = &authority.evidence_contract;
    let observations = &authority.gate_observation_contract;
    let partition = &authority.comparator_partition;

    if identity.pyramid_recipe_operation_fields.len() != 16
        || identity.pyramid_recipe_parameter_fields.len() != 12
        || identity.pyramid_recipe_fields.len() != 12
        || identity.candidate_geometry_digest_preimage.len() != 13
        || identity.candidate_geometry_digest_fields.len() != 8
        || trace.family_projection_rules.len() != REQUIRED_TRACE_FAMILIES.len()
        || trace.family_state_selection.len() != REQUIRED_TRACE_FAMILIES.len()
        || trace.candidate_key_field_widths.iter().sum::<u64>() != trace.candidate_key_bytes
        || trace.trace_package_roles != identity.candidate_package_roles
        || trace.out_of_domain_support_adds_key
    {
        bail!("EP-01 identity or trace projection contract is incomplete")
    }
    let recipe_registry_preimage = identity
        .pyramid_recipe_operation_registry_preimage
        .strip_prefix("exact_ASCII_")
        .and_then(|value| value.strip_suffix("_without_NUL"))
        .context("EP-01 recipe operation registry preimage framing is malformed")?;
    if Sha256Hasher::digest(recipe_registry_preimage.as_bytes()).to_string()
        != identity.pyramid_recipe_operation_registry_sha256
    {
        bail!("EP-01 recipe operation registry preimage and digest differ")
    }
    for ((family, projection), state_selection) in REQUIRED_TRACE_FAMILIES
        .iter()
        .zip(&trace.family_projection_rules)
        .zip(&trace.family_state_selection)
    {
        let prefix = format!("{family}:");
        if !projection.starts_with(&prefix) || !state_selection.starts_with(&prefix) {
            bail!("EP-01 trace family projection and state-selection rows must stay tag ordered")
        }
    }
    let schema_hashes = embedded_schema_hashes()?;
    let common_schema_sha256 = schema_hashes.common.as_str();
    let projection_schema_sha256 = schema_hashes.projection_input.as_str();
    if trace.projection_input_sidecar_schema != PROJECTION_INPUT_SCHEMA_ID
        || trace.projection_input_sidecar_schema_path != PROJECTION_INPUT_SCHEMA_PATH
        || trace.projection_input_sidecar_schema_sha256 != projection_schema_sha256
        || trace.projection_input_sidecar_common_schema_path != COMMON_SCHEMA_PATH
        || trace.projection_input_sidecar_common_schema_sha256 != common_schema_sha256
        || trace.projection_input_sidecar_raw_receipt_relative_path != "projection-input.jcs"
        || trace.projection_input_sidecar_locator_rule.is_empty()
        || trace.projection_input_sidecar_canonical_encoding != ARTIFACT_INSTANCE_ENCODING
        || !trace.projection_input_sidecar_unknown_or_missing_fields_rejected
    {
        bail!("EP-01 projection-input sidecar schema binding is incomplete or stale")
    }

    if package.format_family != "mirante4d"
        || package.lifecycle != "EXPERIMENTAL"
        || package.semantic_schema != "m4d-science-1.0"
        || package.storage_profiles_by_edge != STORAGE_PROFILES_BY_EDGE
        || package.index_profile != "m4d-compound-brick-index-1.0"
        || package.identity_profile != "m4d-id-1"
        || package.required_capabilities != REQUIRED_CAPABILITIES
        || package.fixed_control_paths != FIXED_CONTROL_PATHS
        || package.profile_schema != "m4d-compound-profile"
        || package.profile_schema_version != 1
        || package.profile_fields.len() != 21
        || package.profile_layer_fields.len() != 4
        || package.profile_level_fields.len() != 6
        || package.provenance_record_rules.len() != 3
        || package.manifest_schemas.len() != 2
        || package.manifest_object_registry.len() != 5
        || package.interoperability
            != "NONE_native_Mirante4D_only_no_Zarr_or_OME_NGFF_array_group_or_mirror_objects"
    {
        bail!(
            "EP-01 candidate package, catalog, control, or interoperability contract is incomplete"
        )
    }
    let unique_profile_fields = package
        .profile_fields
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    if unique_profile_fields.len() != package.profile_fields.len()
        || !package
            .required_capabilities
            .windows(2)
            .all(|pair| pair[0] < pair[1])
    {
        bail!("EP-01 profile fields and capabilities must be unique and canonically ordered")
    }

    if pyramid.reduction_factors != [1, 2] {
        bail!("EP-01 pyramid reduction factors must remain exactly one or two")
    }
    if format.header_fields.len() != 18
        || format.record_fields.len() != 7
        || format.record_flags.len() != 5
        || format.legal_record_flag_words.len() != 3
        || format.record_invariants.len() != 6
        || format.row_major_mapping.len() != 9
        || format.dtype_codes.len() != 3
    {
        bail!(
            "EP-01 compound shard layout is not the exact 64-byte header and 32-byte record contract"
        )
    }
    if !accounting.package_ratio_gates_are_candidate_admission_not_universal_format_guarantees
        || !accounting.checkpoint_batch_payload_is_separate_from_resident_header_and_read_windows
    {
        bail!("EP-01 accounting contract changed admission or checkpoint byte semantics")
    }
    if source.source_inventory_scheme != "mirante4d-t5-source-inventory-1"
        || source.metadata_generation_scheme != "mirante4d-ep01-metadata-generation-v1"
        || source.maximum_entries_per_input != 4_096
        || source.stream_buffer_bytes != 65_536
        || source.source_input_roles != SOURCE_INPUT_ROLES
        || source.capture_order != SOURCE_CAPTURE_ORDER
        || source.capture_boundary_rule.is_empty()
        || source.open_and_stream_rule.is_empty()
        || source.content_inventory_rule.is_empty()
        || source.metadata_generation_rule.is_empty()
        || source.input_binding_rule.is_empty()
        || source.comparison_rule.is_empty()
        || !source.source_preservation_commitment_rule.contains(
            "qualification_profile_contract_raw32_workload_bundle_raw32_script_bundle_raw32",
        )
        || source.candidate_package_inventory_rule.is_empty()
        || source.candidate_package_commitment_rule.is_empty()
        || source.raw_receipt_closure_rule.is_empty()
        || source.privacy_rule.is_empty()
        || !source.source_access_is_read_only
        || source.mismatch_allows_selection_receipt
    {
        bail!("EP-01 source and candidate-package currentness contract is incomplete")
    }
    if checkpoint.peak_regular_files != 6
        || checkpoint.regular_file_roles != CHECKPOINT_FILE_ROLES
        || checkpoint.canonical_source_file_names != CANONICAL_SOURCE_FILE_NAMES
        || checkpoint.canonical_source_state_header_fields.len() != 9
        || checkpoint.canonical_source_state_record_fields.len() != 2
        || checkpoint.header_fields.len() != 31
        || checkpoint.record_fields.len() != 5
        || checkpoint.commit_slot_fields.len() != 7
        || checkpoint.peak_regular_files > authority.gates.checkpoint.maximum_regular_files
    {
        bail!("EP-01 ordinal checkpoint contract is incomplete or exceeds its file gate")
    }
    let diagnostic = &runtime.runtime_diagnostic_protocol;
    let diagnostic_warmup_pairs = diagnostic
        .warmup_abba_blocks_per_variant
        .checked_mul(diagnostic.pairs_per_abba_block);
    let diagnostic_measured_pairs = diagnostic
        .measured_abba_blocks_per_variant
        .checked_mul(diagnostic.pairs_per_abba_block);
    let diagnostic_execution_tickets = diagnostic
        .warmup_pairs_per_variant
        .checked_add(diagnostic.measured_pairs_per_variant)
        .and_then(|pairs| pairs.checked_mul(diagnostic.executions_per_pair));
    let maximum_diagnostic_bytes = diagnostic
        .maximum_variants
        .checked_mul(diagnostic.maximum_canonical_variant_bytes);
    if runtime.directory_slot_fields != GPU_DIRECTORY_SLOT_FIELDS
        || runtime.page_record_fields != GPU_PAGE_RECORD_FIELDS
        || runtime.adapter_limit_evidence_fields != GPU_ADAPTER_LIMIT_EVIDENCE_FIELDS
        || runtime.timestamp_period_source_rule.is_empty()
        || runtime.timestamp_period_bits_rule.is_empty()
        || runtime.timestamp_tick_conversion_rule.is_empty()
        || runtime.runtime_state_binding_rule.is_empty()
        || runtime.runtime_execution_binding_rule.is_empty()
        || runtime.timestamp_derived_sample_paths != TIMESTAMP_DERIVED_SAMPLE_PATHS
        || runtime.timestamp_control_subtraction_rule.is_empty()
        || runtime.runtime_sample_allowed_paths != RUNTIME_SAMPLE_ALLOWED_PATHS
        || runtime.runtime_sample_population_rule.is_empty()
        || runtime.runtime_variant_identity_rule.is_empty()
        || runtime.runtime_non_gate_diagnostic_rule.is_empty()
        || runtime.binding_groups.len() != 3
        || runtime.all_invalid_payload_slot_bytes != 0
        || diagnostic.maximum_variants != 256
        || diagnostic.maximum_canonical_variant_bytes != 98_304
        || diagnostic.warmup_abba_blocks_per_variant != 1
        || diagnostic.warmup_pairs_per_variant != 4
        || diagnostic.measured_abba_blocks_per_variant != 25
        || diagnostic.pairs_per_abba_block != 4
        || diagnostic.measured_pairs_per_variant != 100
        || diagnostic.observations_per_arm_per_variant != 100
        || diagnostic.executions_per_pair != 2
        || diagnostic.execution_tickets_per_variant != 208
        || diagnostic.first_arm_order_within_block != DIAGNOSTIC_FIRST_ARM_ORDER
        || diagnostic.p95_population_size != 100
        || diagnostic.p95_nearest_rank_one_based != 95
        || diagnostic.p95_zero_based_sorted_index != 94
        || diagnostic.aggregate_metric_paths != DIAGNOSTIC_AGGREGATE_METRIC_PATHS
        || diagnostic.execution_ticket_domain.is_empty()
        || diagnostic.row_order_rule.is_empty()
        || diagnostic.population_rule.is_empty()
        || diagnostic.p95_rule.is_empty()
        || diagnostic.failure_rule.is_empty()
        || diagnostic_warmup_pairs != Some(diagnostic.warmup_pairs_per_variant)
        || diagnostic_measured_pairs != Some(diagnostic.measured_pairs_per_variant)
        || diagnostic.measured_pairs_per_variant != diagnostic.observations_per_arm_per_variant
        || diagnostic_execution_tickets != Some(diagnostic.execution_tickets_per_variant)
        || diagnostic.p95_nearest_rank_one_based.checked_sub(1)
            != Some(diagnostic.p95_zero_based_sorted_index)
        || maximum_diagnostic_bytes.is_none_or(|bytes| {
            bytes
                > evidence
                    .validation_resource_contract
                    .maximum_candidate_artifact_bytes
        })
        || runtime.directory_slot_fields.len() * size_of::<u32>()
            != usize::try_from(authority.gates.gpu.maximum_directory_slot_bytes)
                .context("EP-01 directory-slot gate does not fit usize")?
        || runtime.page_record_fields.len() * size_of::<u32>()
            != usize::try_from(authority.gates.gpu.maximum_page_record_bytes)
                .context("EP-01 page-record gate does not fit usize")?
    {
        bail!("EP-01 GPU directory, page-record, or binding contract is incoherent")
    }
    let resources = &evidence.validation_resource_contract;
    let embedded_schemas: [&[u8]; 9] = [
        COMMON_SCHEMA_BYTES,
        FAILURE_EVIDENCE_SCHEMA_BYTES,
        PROJECTION_INPUT_SCHEMA_BYTES,
        SOURCE_INVENTORY_SCHEMA_BYTES,
        PACKAGE_VALIDATION_SCHEMA_BYTES,
        BUILD_IMPORT_SCHEMA_BYTES,
        TRACE_SCHEMA_BYTES,
        RUNTIME_GPU_SCHEMA_BYTES,
        GATE_OBSERVATION_SCHEMA_BYTES,
    ];
    if resources.maximum_candidate_artifact_bytes != 64 * 1024 * 1024
        || resources.maximum_projection_input_sidecar_bytes != 64 * 1024 * 1024
        || resources.maximum_source_inventory_sidecar_bytes != 64 * 1024 * 1024
        || resources.maximum_raw_receipt_bytes != 64 * 1024 * 1024
        || resources.maximum_public_projection_bytes != 64 * 1024 * 1024
        || resources.maximum_sanitized_receipt_bytes != 64 * 1024 * 1024
        || resources.maximum_private_failure_evidence_bytes != 64 * 1024 * 1024
        || resources.maximum_failure_receipt_bytes != 64 * 1024 * 1024
        || resources.maximum_schema_document_bytes != 1024 * 1024
        || resources.maximum_variable_array_items != 262_144
        || resources.maximum_layout_fields_per_binding != 256
        || resources.maximum_diagnostic_variants != 256
        || resources.maximum_canonical_diagnostic_variant_bytes != 98_304
        || resources.maximum_instrumentation_overhead_pairs != 8_192
        || resources.maximum_diagnostic_variants != diagnostic.maximum_variants
        || resources.maximum_canonical_diagnostic_variant_bytes
            != diagnostic.maximum_canonical_variant_bytes
        || resources.maximum_instrumentation_overhead_pairs
            != observations
                .instrumentation_overhead_operand
                .maximum_raw_pairs
        || resources.maximum_ascii_bytes != 512
        || embedded_schemas
            .iter()
            .any(|schema| match u64::try_from(schema.len()) {
                Ok(bytes) => bytes > resources.maximum_schema_document_bytes,
                Err(_) => true,
            })
        || resources.file_admission_rule.is_empty()
        || resources.stream_validation_rule.is_empty()
        || resources.population_limit_rule.is_empty()
    {
        bail!("EP-01 evidence validation resource contract is missing an exact byte or item cap")
    }
    if evidence.public_receipt_serializes_private_geometry
        || !evidence.raw_evidence_remains_external
        || evidence.raw_receipt_fields.len() != 7
        || evidence.sanitized_receipt_fields.len() != 3
        || evidence.candidate_receipt_fields.len() != 6
        || evidence.gate_row_fields.len() != 10
        || evidence.gate_reason_codes.len() != 1
        || evidence.sanitized_fields.len() != 8
        || evidence.forbidden_sanitized_fields.len() != 18
        || !evidence
            .forbidden_sanitized_fields
            .iter()
            .any(|field| field == "PackageIds_or_manifest_digests")
        || !evidence
            .forbidden_sanitized_fields
            .iter()
            .any(|field| field == "candidate_content_or_package_set_generations")
        || !evidence
            .forbidden_sanitized_fields
            .iter()
            .any(|field| field == "projection_input_sidecar_digest_or_values")
        || !evidence
            .forbidden_sanitized_fields
            .iter()
            .any(|field| field == "private_commitment_nonce_256")
        || !evidence
            .sanitized_fields
            .iter()
            .any(|field| field == "nonce_blinded_source_preservation_commitment")
        || !evidence
            .forbidden_sanitized_fields
            .iter()
            .any(|field| field == "source_inventory_sidecar_digest_or_raw_capture_facts")
        || !evidence
            .forbidden_sanitized_fields
            .iter()
            .any(|field| field == "raw_diagnostic_trial_or_timestamp_control_pair_rows")
    {
        bail!("EP-01 evidence privacy contract permits private geometry or lineage")
    }

    let grammar = &evidence.receipt_schema_grammar;
    let enums = &grammar.enums;
    let objects = &grammar.objects;
    let common_binding = &evidence.artifact_common_schema_binding;
    if common_binding.schema_id != COMMON_SCHEMA_ID
        || common_binding.schema_path != COMMON_SCHEMA_PATH
        || common_binding.schema_sha256 != common_schema_sha256
        || common_binding.schema_document_encoding != SCHEMA_DOCUMENT_ENCODING
        || common_binding.instance_scalar_contract
            != "evidence_contract.receipt_schema_grammar.scalar_types"
    {
        bail!("EP-01 common evidence schema binding is incomplete or stale")
    }
    let failure_schema_sha256 = schema_hashes.failure_evidence.as_str();
    let failure_binding = &evidence.failure_evidence_schema_binding;
    if failure_binding.payload_schema != FAILURE_EVIDENCE_SCHEMA_ID
        || failure_binding.schema_path != FAILURE_EVIDENCE_SCHEMA_PATH
        || failure_binding.schema_sha256 != failure_schema_sha256
        || failure_binding.common_schema_path != COMMON_SCHEMA_PATH
        || failure_binding.common_schema_sha256 != common_schema_sha256
        || failure_binding.canonical_encoding != ARTIFACT_INSTANCE_ENCODING
        || !failure_binding.unknown_or_missing_fields_rejected
        || failure_binding.private_failure_evidence_relative_path != "failure-evidence.jcs"
        || failure_binding.locator_and_publication_rule.is_empty()
        || failure_binding.record_order_rule.is_empty()
        || failure_binding.bounded_failure_evidence_rule.is_empty()
        || failure_binding.failure_projection_rule.is_empty()
        || failure_binding.privacy_rule.is_empty()
    {
        bail!("EP-01 private failure-evidence schema binding is incomplete or stale")
    }
    let source_inventory_schema_sha256 = schema_hashes.source_inventory.as_str();
    let source_binding = &evidence.source_inventory_schema_binding;
    if source_binding.payload_schema != SOURCE_INVENTORY_SCHEMA_ID
        || source_binding.schema_path != SOURCE_INVENTORY_SCHEMA_PATH
        || source_binding.schema_sha256 != source_inventory_schema_sha256
        || source_binding.common_schema_path != COMMON_SCHEMA_PATH
        || source_binding.common_schema_sha256 != common_schema_sha256
        || source_binding.canonical_encoding != ARTIFACT_INSTANCE_ENCODING
        || !source_binding.unknown_or_missing_fields_rejected
        || source_binding.private_source_inventory_relative_path != "source-inventory.jcs"
        || source_binding.locator_and_publication_rule != SOURCE_INVENTORY_LOCATOR_RULE
        || source_binding.capture_order_rule.is_empty()
        || source_binding.comparison_rule.is_empty()
        || source_binding.raw_receipt_closure_rule.is_empty()
        || source_binding.privacy_rule.is_empty()
    {
        bail!("EP-01 private source-inventory schema binding is incomplete or stale")
    }
    if evidence.artifact_schema_bindings.len() != ARTIFACT_SCHEMA_EXPECTATIONS.len() {
        bail!("EP-01 artifact schema binding count differs from the five artifact roles")
    }
    for (ordinal, ((binding, expected), schema_sha256)) in evidence
        .artifact_schema_bindings
        .iter()
        .zip(&ARTIFACT_SCHEMA_EXPECTATIONS)
        .zip(&schema_hashes.artifacts)
        .enumerate()
    {
        let role_contract = evidence.artifact_role_contracts[ordinal]
            .split(':')
            .collect::<Vec<_>>();
        if binding.logical_role_tag != expected.logical_role_tag
            || binding.logical_role != expected.logical_role
            || binding.payload_schema != expected.payload_schema
            || binding.schema_path != expected.schema_path
            || binding.schema_sha256 != schema_sha256.as_str()
            || binding.canonical_encoding != ARTIFACT_INSTANCE_ENCODING
            || !binding.unknown_or_missing_fields_rejected
            || binding.artifact_cardinality != expected.artifact_cardinality
            || binding.package_role_rule.is_empty()
            || binding.payload_array_order_rule.is_empty()
            || role_contract.len() != 5
            || role_contract[0] != expected.logical_role_tag.to_string()
            || role_contract[1] != expected.logical_role
            || role_contract[2] != expected.payload_schema
            || role_contract[3] != format!("cardinality{}", expected.artifact_cardinality)
        {
            bail!("EP-01 artifact schema binding {ordinal} is incomplete, stale, or out of order")
        }
    }
    let artifact_cardinalities = evidence
        .artifact_role_contracts
        .iter()
        .filter_map(|contract| {
            contract
                .split(':')
                .find_map(|field| field.strip_prefix("cardinality"))
                .and_then(|value| value.parse::<u64>().ok())
        })
        .collect::<Vec<_>>();
    if !grammar.objects_are_exact
        || !grammar.unknown_or_missing_fields_rejected
        || enums.candidate_edge != CANDIDATE_EDGES
        || enums.selection != [None, Some(32), Some(64)]
        || enums.package_role != CANDIDATE_PACKAGE_ROLES
        || enums.trace_family != REQUIRED_TRACE_FAMILIES
        || enums.comparator != RECEIPT_COMPARATORS
        || enums.unit != OBSERVATION_UNITS
        || enums.artifact_role.len() != 5
        || enums.failure_stage.len() != 8
        || enums.failure_reason.len() != 8
        || objects.raw_revision.len() != 6
        || grammar.scalar_types.bytes32 != "exactly_32_raw_bytes_encoded_as_64_lowercase_hex"
        || objects.raw_bindings.len() != 10
        || objects.raw_package.len() != 6
        || objects.raw_trace.len() != 8
        || objects.raw_gate.len() != 10
        || objects.raw_candidate.len() != 6
        || objects.artifact.len() != 8
        || objects.raw_receipt.len() != 7
        || objects.public_revision.len() != 5
        || objects.public_trace.len() != 4
        || objects.public_gate.len() != 9
        || objects.public_candidate.len() != 5
        || objects.public_projection.len() != 4
        || objects.sanitized_receipt.len() != 3
        || objects.failure_revision.len() != 3
        || objects.failure_receipt.len() != 9
        || receipt_schema_field_names(&objects.raw_receipt).as_deref()
            != Some(
                evidence
                    .raw_receipt_fields
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice(),
            )
        || receipt_schema_field_names(&objects.sanitized_receipt).as_deref()
            != Some(
                evidence
                    .sanitized_receipt_fields
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice(),
            )
        || receipt_schema_field_names(&objects.raw_candidate).as_deref()
            != Some(
                evidence
                    .candidate_receipt_fields
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice(),
            )
        || receipt_schema_field_names(&objects.raw_gate).as_deref()
            != Some(
                evidence
                    .gate_row_fields
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice(),
            )
        || evidence.artifact_role_contracts.len() != 5
        || artifact_cardinalities.len() != 5
        || artifact_cardinalities.iter().sum::<u64>() != 12
        || evidence
            .receipt_hash_contract
            .source_preservation_commitment_sha256
            != "exactly_source_currentness_contract.source_preservation_commitment_rule"
        || evidence.receipt_hash_contract.construction_order.len() != 14
        || evidence
            .receipt_hash_contract
            .failure_construction_order
            .len()
            != 6
        || evidence
            .receipt_hash_contract
            .artifact_manifest_excludes
            .len()
            != 6
        || evidence
            .receipt_hash_contract
            .standalone_public_projection_file_allowed
        || !evidence
            .receipt_hash_contract
            .failure_receipt_mutually_exclusive_with_selection_receipts
        || evidence.public_projection_allowlist.len() != 13
        || evidence.public_projection_forbidden.len() != 18
        || !evidence
            .public_projection_forbidden
            .iter()
            .any(|field| field == "package_ids_or_manifest_digests")
        || !evidence
            .public_projection_forbidden
            .iter()
            .any(|field| field == "content_or_package_set_generations")
        || !evidence
            .public_projection_forbidden
            .iter()
            .any(|field| field == "projection_input_sidecar_digest_or_values")
        || !evidence
            .public_projection_forbidden
            .iter()
            .any(|field| field == "private_commitment_nonce_256")
        || !evidence
            .public_projection_allowlist
            .iter()
            .any(|field| field == "source_preservation_commitment_sha256")
        || !evidence
            .public_projection_forbidden
            .iter()
            .any(|field| field == "source_inventory_sidecar_digest_or_raw_capture_facts")
    {
        bail!("EP-01 receipt schema dictionary, hash closure, or privacy projection is incomplete")
    }

    if observations.units != OBSERVATION_UNITS
        || observations.raw_ratio_paths != RAW_RATIO_PATHS
        || observations.raw_ratio_paths[..10] != partition.engineering_ratio_direct_paths
        || observations.raw_ratio_paths[10]
            != "gates.plane_amplification.maximum_range_requests_per_new_brick"
        || observations.primitive_observation_tie_key != PRIMITIVE_OBSERVATION_TIE_KEY
        || observations.primitive_source_kind_tags != PRIMITIVE_SOURCE_KIND_TAGS
        || observations.primitive_source_mapping != PRIMITIVE_SOURCE_MAPPING
        || observations.primitive_source_commitment_rule.is_empty()
        || observations
            .ratio_aggregation_contract
            .completion_order_used
        || observations
            .ratio_aggregation_contract
            .gcd_reduction_allowed
        || !observations
            .cold_plane_population
            .clear_cpu_decoded_residency
        || !observations
            .cold_plane_population
            .clear_gpu_directory_pages_and_payloads
        || !observations
            .cold_plane_population
            .clear_shard_prefix_index_and_payload_cohort_cache
        || !observations
            .cold_plane_population
            .require_no_live_or_queued_brick_key_work
        || !observations
            .cold_plane_population
            .retain_open_verified_package_controls
        || !observations
            .cold_plane_population
            .retain_warmed_pipelines_and_static_controls
        || !observations
            .cold_plane_population
            .retain_bound_os_cache_condition
        || !observations
            .cold_plane_population
            .require_positive_useful_logical_sample_bytes
        || !observations
            .cold_plane_population
            .require_positive_unique_newly_requested_brick_key_count
        || observations.cold_plane_population.untagged_state_eligible
        || !observations
            .instrumentation_overhead_operand
            .control_must_be_positive
        || observations
            .instrumentation_overhead_operand
            .maximum_raw_pairs
            != 8_192
        || observations
            .instrumentation_overhead_operand
            .raw_pair_population_rule
            .is_empty()
        || observations
            .instrumentation_overhead_operand
            .raw_pair_order_rule
            .is_empty()
        || observations
            .instrumentation_overhead_operand
            .execution_ticket_domain
            .is_empty()
        || observations
            .instrumentation_overhead_operand
            .execution_ticket_domain
            == diagnostic.execution_ticket_domain
        || observations
            .instrumentation_overhead_operand
            .raw_pair_arithmetic_rule
            .is_empty()
        || observations
            .instrumentation_overhead_operand
            .raw_pair_source_rule
            .is_empty()
        || observations
            .instrumentation_overhead_operand
            .invalid_pair_allows_selection_receipt
        || observations
            .invalid_observation_behavior
            .selection_receipt_allowed
        || observations
            .invalid_observation_behavior
            .sanitized_selection_receipt_allowed
        || !observations
            .invalid_observation_behavior
            .failure_receipt_required
        || !observations
            .invalid_observation_behavior
            .valid_failed_comparison_allowed_in_selection_receipt
        || observations.registry.len() != 74
        || observations.registry_order
            != "headroom_paths_then_engineering_ratio_direct_paths_then_structural_direct_paths_exactly_74_rows_gate_IDs_EP01-G000_through_EP01-G073"
    {
        bail!("EP-01 gate observation registry shape, units, or ordering rule changed")
    }
    let expected_registry_paths = HEADROOM_PATHS
        .iter()
        .chain(&ENGINEERING_RATIO_DIRECT_PATHS)
        .chain(&STRUCTURAL_DIRECT_PATHS);
    let mut observed_registry_paths = std::collections::BTreeSet::new();
    for (ordinal, (row, expected_path)) in observations
        .registry
        .iter()
        .zip(expected_registry_paths)
        .enumerate()
    {
        let fields = row.split('|').collect::<Vec<_>>();
        if fields.len() != 5
            || fields.iter().any(|field| field.is_empty())
            || fields[0] != *expected_path
            || !observations.units.iter().any(|unit| unit == fields[1])
            || !observed_registry_paths.insert(fields[0])
        {
            bail!("EP-01 gate observation registry row {ordinal} is malformed or out of order")
        }
    }
    if observed_registry_paths.len() != 74 {
        bail!("EP-01 gate observation registry must contain 74 unique authority paths")
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

    fn mutate_leaf(value: &mut Value, pointer: &str) -> Value {
        let leaf = value
            .pointer_mut(pointer)
            .unwrap_or_else(|| panic!("missing authority pointer {pointer}"));
        let original = leaf.clone();
        match leaf {
            Value::String(value) => value.push_str("_changed"),
            Value::Bool(value) => *value = !*value,
            Value::Number(value) => {
                let value = value.as_u64().expect("authority numbers must be unsigned");
                *leaf = Value::Number(Number::from(
                    value.checked_add(1).expect("authority mutation overflowed"),
                ));
            }
            Value::Null => *leaf = Value::Number(Number::from(0)),
            Value::Array(_) | Value::Object(_) => {
                panic!("authority pointer {pointer} is not a primitive leaf")
            }
        }
        original
    }

    fn leaf_commitment(domain: &str, pointer: &str, value: &Value) -> String {
        let encoded = serde_json::to_vec(&(domain, pointer, value)).unwrap();
        Sha256Hasher::digest(encoded).to_string()
    }

    #[test]
    fn committed_authority_is_strict_valid_and_matches_its_exact_commitment() {
        validate_committed_authority().unwrap();
        assert_eq!(authority_fingerprint_sha256(), COMMITTED_AUTHORITY_SHA256);
        let authority: SelectionAuthority = serde_json::from_slice(AUTHORITY_BYTES).unwrap();
        assert_eq!(
            authority_semantic_fingerprint_sha256(&authority).unwrap(),
            COMMITTED_AUTHORITY_SEMANTIC_SHA256
        );

        let mut unknown = authority_value();
        unknown["unexpected"] = json!(true);
        assert!(serde_json::from_value::<SelectionAuthority>(unknown).is_err());

        let mut nested_unknown = authority_value();
        nested_unknown["candidate_package_contract"]["unexpected"] = json!(true);
        assert!(serde_json::from_value::<SelectionAuthority>(nested_unknown).is_err());

        let mut missing = authority_value();
        missing["runtime_gpu_contract"]
            .as_object_mut()
            .unwrap()
            .remove("buffer_word_encoding");
        assert!(serde_json::from_value::<SelectionAuthority>(missing).is_err());
    }

    #[test]
    fn identity_package_and_dual_trace_contracts_have_exact_shapes() {
        let authority: SelectionAuthority = serde_json::from_slice(AUTHORITY_BYTES).unwrap();
        validate_clarification_contracts(&authority).unwrap();
        let identity = &authority.candidate_identity;
        assert_eq!(identity.pyramid_recipe_operation_fields.len(), 16);
        assert_eq!(identity.pyramid_recipe_parameter_fields.len(), 12);
        assert_eq!(identity.pyramid_recipe_fields.len(), 12);
        assert_eq!(identity.candidate_geometry_digest_preimage.len(), 13);
        assert_eq!(identity.candidate_geometry_digest_fields.len(), 8);
        assert_eq!(identity.candidate_package_roles, CANDIDATE_PACKAGE_ROLES);
        let registry_preimage = identity
            .pyramid_recipe_operation_registry_preimage
            .strip_prefix("exact_ASCII_")
            .and_then(|value| value.strip_suffix("_without_NUL"))
            .unwrap();
        assert_eq!(
            Sha256Hasher::digest(registry_preimage.as_bytes()).to_string(),
            identity.pyramid_recipe_operation_registry_sha256
        );

        let trace = &authority.trace_derivation;
        assert_eq!(trace.geometry_authorities, GEOMETRY_AUTHORITIES);
        assert_eq!(trace.trace_digest_fields, TRACE_DIGEST_FIELDS);
        assert_eq!(
            trace.ordered_trace_digest_fields,
            ORDERED_TRACE_DIGEST_FIELDS
        );
        assert_eq!(trace.ordered_state_frame_fields, ORDERED_STATE_FRAME_FIELDS);
        assert_eq!(trace.scenario_tags, SCENARIO_TAGS);
        assert_eq!(trace.state_kind_tags, STATE_KIND_TAGS);
        assert_eq!(trace.package_role_tags, PACKAGE_ROLE_TAGS);
        assert!(!trace.one_digest_per_candidate_and_trace_family);
        assert!(trace.ordered_and_unique_digest_per_candidate_and_trace_family);
        assert_eq!(trace.family_projection_rules.len(), 8);
        assert_eq!(trace.family_state_selection.len(), 8);
        assert_eq!(
            trace.projection_input_sidecar_schema_sha256,
            Sha256Hasher::digest(PROJECTION_INPUT_SCHEMA_BYTES).to_string()
        );
        assert_eq!(
            trace.projection_input_sidecar_common_schema_sha256,
            Sha256Hasher::digest(COMMON_SCHEMA_BYTES).to_string()
        );
        assert!(trace.projection_input_sidecar_unknown_or_missing_fields_rejected);

        let package = &authority.candidate_package_contract;
        assert_eq!(package.storage_profiles_by_edge, STORAGE_PROFILES_BY_EDGE);
        assert_eq!(package.required_capabilities, REQUIRED_CAPABILITIES);
        assert_eq!(package.fixed_control_paths, FIXED_CONTROL_PATHS);
        assert_eq!(package.profile_fields.len(), 21);
        assert_eq!(package.profile_layer_fields.len(), 4);
        assert_eq!(package.profile_level_fields.len(), 6);
        assert_eq!(package.provenance_record_rules.len(), 3);
        assert_eq!(package.manifest_schemas.len(), 2);
        assert_eq!(package.manifest_object_registry.len(), 5);
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
    fn observation_registry_has_exact_74_path_order_and_rejects_reordering() {
        let authority: SelectionAuthority = serde_json::from_slice(AUTHORITY_BYTES).unwrap();
        let expected_paths = HEADROOM_PATHS
            .iter()
            .chain(&ENGINEERING_RATIO_DIRECT_PATHS)
            .chain(&STRUCTURAL_DIRECT_PATHS);
        for (ordinal, (row, expected_path)) in authority
            .gate_observation_contract
            .registry
            .iter()
            .zip(expected_paths)
            .enumerate()
        {
            let fields = row.split('|').collect::<Vec<_>>();
            assert_eq!(fields.len(), 5, "registry row {ordinal}");
            assert_eq!(fields[0], *expected_path, "registry row {ordinal}");
            assert!(OBSERVATION_UNITS.contains(&fields[1]));
        }
        assert_eq!(authority.gate_observation_contract.registry.len(), 74);
        assert_eq!(format!("EP01-G{:03}", 0), "EP01-G000");
        assert_eq!(format!("EP01-G{:03}", 73), "EP01-G073");

        let mut reordered = authority.clone();
        reordered.gate_observation_contract.registry.swap(0, 1);
        assert!(validate_clarification_contracts(&reordered).is_err());

        let mut missing = authority.clone();
        missing.gate_observation_contract.registry.pop();
        assert!(validate_clarification_contracts(&missing).is_err());

        let mut duplicate = authority;
        duplicate.gate_observation_contract.registry[1] =
            duplicate.gate_observation_contract.registry[0].clone();
        assert!(validate_clarification_contracts(&duplicate).is_err());
    }

    #[test]
    fn receipt_and_gate_policy_dictionaries_have_exact_closed_shapes() {
        let authority: SelectionAuthority = serde_json::from_slice(AUTHORITY_BYTES).unwrap();
        validate_clarification_contracts(&authority).unwrap();

        let evidence = &authority.evidence_contract;
        let grammar = &evidence.receipt_schema_grammar;
        assert!(grammar.objects_are_exact);
        assert!(grammar.unknown_or_missing_fields_rejected);
        assert_eq!(grammar.enums.candidate_edge, CANDIDATE_EDGES);
        assert_eq!(grammar.enums.selection, [None, Some(32), Some(64)]);
        assert_eq!(grammar.enums.package_role, CANDIDATE_PACKAGE_ROLES);
        assert_eq!(grammar.enums.trace_family, REQUIRED_TRACE_FAMILIES);
        assert_eq!(grammar.enums.unit, OBSERVATION_UNITS);
        assert_eq!(grammar.objects.raw_bindings.len(), 10);
        assert_eq!(grammar.objects.raw_candidate.len(), 6);
        assert_eq!(grammar.objects.raw_gate.len(), 10);
        assert_eq!(grammar.objects.raw_receipt.len(), 7);
        assert_eq!(grammar.objects.public_gate.len(), 9);
        assert_eq!(grammar.objects.public_candidate.len(), 5);
        assert_eq!(grammar.objects.sanitized_receipt.len(), 3);
        assert_eq!(grammar.objects.failure_receipt.len(), 9);
        assert_eq!(evidence.artifact_role_contracts.len(), 5);
        assert_eq!(evidence.artifact_schema_bindings.len(), 5);
        assert_eq!(
            evidence.artifact_common_schema_binding.schema_sha256,
            Sha256Hasher::digest(COMMON_SCHEMA_BYTES).to_string()
        );
        assert_eq!(
            evidence.failure_evidence_schema_binding.schema_sha256,
            Sha256Hasher::digest(FAILURE_EVIDENCE_SCHEMA_BYTES).to_string()
        );
        assert!(
            evidence
                .failure_evidence_schema_binding
                .unknown_or_missing_fields_rejected
        );
        for (binding, expected) in evidence
            .artifact_schema_bindings
            .iter()
            .zip(&ARTIFACT_SCHEMA_EXPECTATIONS)
        {
            assert_eq!(binding.logical_role_tag, expected.logical_role_tag);
            assert_eq!(binding.logical_role, expected.logical_role);
            assert_eq!(binding.payload_schema, expected.payload_schema);
            assert_eq!(binding.schema_path, expected.schema_path);
            assert_eq!(
                binding.schema_sha256,
                Sha256Hasher::digest(expected.bytes).to_string()
            );
            assert!(binding.unknown_or_missing_fields_rejected);
        }
        assert_eq!(evidence.receipt_hash_contract.construction_order.len(), 14);
        assert_eq!(
            evidence
                .receipt_hash_contract
                .failure_construction_order
                .len(),
            6
        );
        assert_eq!(evidence.public_projection_allowlist.len(), 13);
        assert_eq!(evidence.public_projection_forbidden.len(), 18);
        assert_eq!(
            evidence
                .validation_resource_contract
                .maximum_candidate_artifact_bytes,
            64 * 1024 * 1024
        );
        assert_eq!(
            evidence
                .validation_resource_contract
                .maximum_variable_array_items,
            262_144
        );
        assert_eq!(
            evidence.validation_resource_contract.maximum_ascii_bytes,
            512
        );

        let observations = &authority.gate_observation_contract;
        assert_eq!(observations.raw_ratio_paths, RAW_RATIO_PATHS);
        assert_eq!(
            observations.primitive_observation_tie_key,
            PRIMITIVE_OBSERVATION_TIE_KEY
        );
        assert_eq!(
            observations.primitive_source_kind_tags,
            PRIMITIVE_SOURCE_KIND_TAGS
        );
        assert_eq!(
            observations.primitive_source_mapping,
            PRIMITIVE_SOURCE_MAPPING
        );
        assert_eq!(
            authority.runtime_gpu_contract.runtime_sample_allowed_paths,
            RUNTIME_SAMPLE_ALLOWED_PATHS
        );

        let mut nested_unknown = authority_value();
        nested_unknown["evidence_contract"]["receipt_schema_grammar"]["scalar_types"]["unexpected"] =
            json!("rejected");
        assert!(serde_json::from_value::<SelectionAuthority>(nested_unknown).is_err());

        let mut binding_unknown = authority_value();
        binding_unknown["evidence_contract"]["artifact_schema_bindings"][0]["unexpected"] =
            json!("rejected");
        assert!(serde_json::from_value::<SelectionAuthority>(binding_unknown).is_err());

        let mut failure_binding_unknown = authority_value();
        failure_binding_unknown["evidence_contract"]["failure_evidence_schema_binding"]["unexpected"] =
            json!("rejected");
        assert!(serde_json::from_value::<SelectionAuthority>(failure_binding_unknown).is_err());

        let mut resource_unknown = authority_value();
        resource_unknown["evidence_contract"]["validation_resource_contract"]["unexpected"] =
            json!(1);
        assert!(serde_json::from_value::<SelectionAuthority>(resource_unknown).is_err());

        let mut reordered_ratio_paths = authority.clone();
        reordered_ratio_paths
            .gate_observation_contract
            .raw_ratio_paths
            .swap(0, 1);
        assert!(validate_clarification_contracts(&reordered_ratio_paths).is_err());

        let mut missing_receipt_field = authority.clone();
        missing_receipt_field
            .evidence_contract
            .receipt_schema_grammar
            .objects
            .raw_gate
            .pop();
        assert!(validate_clarification_contracts(&missing_receipt_field).is_err());

        let mut stale_schema_hash = authority;
        stale_schema_hash.evidence_contract.artifact_schema_bindings[0]
            .schema_sha256
            .push('0');
        assert!(validate_clarification_contracts(&stale_schema_hash).is_err());
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
        let source = &authority.source_currentness_contract;
        assert_eq!(checkpoint.peak_regular_files, 6);
        assert_eq!(checkpoint.regular_file_roles.len(), 6);
        assert_eq!(
            checkpoint.canonical_source_file_names,
            CANONICAL_SOURCE_FILE_NAMES
        );
        assert_eq!(checkpoint.canonical_source_state_header_fields.len(), 9);
        assert_eq!(checkpoint.canonical_source_state_record_fields.len(), 2);
        assert_eq!(checkpoint.header_fields.len(), 31);
        assert_eq!(checkpoint.record_fields.len(), 5);
        assert_eq!(checkpoint.commit_slot_fields.len(), 7);
        assert_eq!(source.source_input_roles, SOURCE_INPUT_ROLES);
        assert_eq!(source.capture_order, SOURCE_CAPTURE_ORDER);
        assert!(source.source_access_is_read_only);
        assert!(!source.mismatch_allows_selection_receipt);
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
            authority.runtime_gpu_contract.adapter_limit_evidence_fields,
            GPU_ADAPTER_LIMIT_EVIDENCE_FIELDS
        );
        assert_eq!(
            authority
                .runtime_gpu_contract
                .timestamp_derived_sample_paths,
            TIMESTAMP_DERIVED_SAMPLE_PATHS
        );
        let diagnostic = &authority.runtime_gpu_contract.runtime_diagnostic_protocol;
        assert_eq!(
            diagnostic.first_arm_order_within_block,
            DIAGNOSTIC_FIRST_ARM_ORDER
        );
        assert_eq!(diagnostic.measured_abba_blocks_per_variant, 25);
        assert_eq!(diagnostic.measured_pairs_per_variant, 100);
        assert_eq!(diagnostic.observations_per_arm_per_variant, 100);
        assert_eq!(diagnostic.p95_zero_based_sorted_index, 94);
        assert_eq!(diagnostic.maximum_variants, 256);
        assert!(
            diagnostic.maximum_variants * diagnostic.maximum_canonical_variant_bytes
                < authority
                    .evidence_contract
                    .validation_resource_contract
                    .maximum_candidate_artifact_bytes
        );
        assert_eq!(
            authority
                .evidence_contract
                .source_inventory_schema_binding
                .schema_sha256,
            Sha256Hasher::digest(SOURCE_INVENTORY_SCHEMA_BYTES).to_string()
        );

        assert_eq!(
            parse_positive_finite_timestamp_period_bits("00000001").unwrap(),
            1
        );
        assert_eq!(
            parse_positive_finite_timestamp_period_bits("3f800000").unwrap(),
            1.0_f32.to_bits()
        );
        for invalid in [
            "00000000", "80000000", "80000001", "bf800000", "7f800000", "ff800000", "7f800001",
            "7fc00000", "7fffffff", "ffc00000", "3F800000",
        ] {
            assert!(
                parse_positive_finite_timestamp_period_bits(invalid).is_err(),
                "accepted invalid timestamp period bits {invalid}"
            );
        }
        assert_eq!(timestamp_ticks_to_nanoseconds("3f000000", 0, 1).unwrap(), 0);
        assert_eq!(timestamp_ticks_to_nanoseconds("3f000000", 0, 3).unwrap(), 2);
        assert_eq!(
            timestamp_ticks_to_nanoseconds("3f800000", 7, 10).unwrap(),
            3
        );
        assert!(timestamp_ticks_to_nanoseconds("3f800000", 2, 1).is_err());
        assert!(timestamp_ticks_to_nanoseconds("7f7fffff", 0, 1).is_err());

        let mut weak_runtime_schema: serde_json::Value =
            serde_json::from_slice(RUNTIME_GPU_SCHEMA_BYTES).unwrap();
        weak_runtime_schema["$defs"]["positive_finite_f32_bits"]["pattern"] =
            serde_json::Value::String("^[0-9a-f]{8}$".to_owned());
        assert!(
            validate_embedded_schema(
                &serde_json::to_vec(&weak_runtime_schema).unwrap(),
                "viewer-performance-ep01-runtime-gpu.schema.json",
                Some("mirante4d-viewer-performance-ep01-runtime-gpu-evidence-1"),
            )
            .is_err()
        );

        let mut reordered_source_schema: serde_json::Value =
            serde_json::from_slice(SOURCE_INVENTORY_SCHEMA_BYTES).unwrap();
        reordered_source_schema["properties"]["captures"]["prefixItems"]
            .as_array_mut()
            .unwrap()
            .swap(0, 1);
        assert!(
            validate_embedded_schema(
                &serde_json::to_vec(&reordered_source_schema).unwrap(),
                "viewer-performance-ep01-source-inventory.schema.json",
                Some(SOURCE_INVENTORY_SCHEMA_ID),
            )
            .is_err()
        );

        let mut assertion_only_build_schema: serde_json::Value =
            serde_json::from_slice(BUILD_IMPORT_SCHEMA_BYTES).unwrap();
        let required = assertion_only_build_schema["required"]
            .as_array_mut()
            .unwrap();
        let currentness = required
            .iter_mut()
            .find(|value| value.as_str() == Some("source_inventory_sidecar_sha256"))
            .unwrap();
        *currentness = serde_json::Value::String("source_unchanged".to_owned());
        assert!(
            validate_embedded_schema(
                &serde_json::to_vec(&assertion_only_build_schema).unwrap(),
                "viewer-performance-ep01-build-import.schema.json",
                Some("mirante4d-viewer-performance-ep01-build-import-evidence-1"),
            )
            .is_err()
        );

        let mut weak_package_schema: serde_json::Value =
            serde_json::from_slice(PACKAGE_VALIDATION_SCHEMA_BYTES).unwrap();
        weak_package_schema["$defs"]["pre_package_inventory"]["required"]
            .as_array_mut()
            .unwrap()
            .retain(|value| value.as_str() != Some("metadata_generation_sha256"));
        assert!(
            validate_embedded_schema(
                &serde_json::to_vec(&weak_package_schema).unwrap(),
                "viewer-performance-ep01-package-validation.schema.json",
                Some("mirante4d-viewer-performance-ep01-package-validation-evidence-1"),
            )
            .is_err()
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
        assert_eq!(authority_fingerprint_sha256(), COMMITTED_AUTHORITY_SHA256);
        let parsed: SelectionAuthority = serde_json::from_value(authority.clone()).unwrap();
        validate_authority(&parsed).unwrap();
        assert_eq!(
            authority_semantic_fingerprint_sha256(&parsed).unwrap(),
            COMMITTED_AUTHORITY_SEMANTIC_SHA256
        );
        let mut semantic_authority = serde_json::to_value(&parsed).unwrap();
        assert_eq!(semantic_authority, authority);
        let mut raw_authority = authority.clone();

        // Exercise the unconditional semantic guard with a field that has no
        // direct value check. Exact equality between the typed semantic tree
        // and the source tree below then proves that every enumerated leaf is
        // an input to the same guard without reparsing and reserializing the
        // complete 119 KiB authority for each leaf.
        let mut semantic_only_mutation = parsed.clone();
        semantic_only_mutation
            .trace_derivation
            .projection_input_sidecar_rule
            .push_str("_changed");
        let semantic_error = validate_authority(&semantic_only_mutation).unwrap_err();
        assert!(
            format!("{semantic_error:#}").contains("semantic contract changed"),
            "semantic-only mutation bypassed the authority commitment"
        );

        let mut pointers = Vec::new();
        collect_leaf_pointers(&authority, "", &mut pointers);
        assert!(!pointers.is_empty());

        for pointer in pointers {
            let original_raw_commitment = leaf_commitment(
                "raw-authority-leaf-v1",
                &pointer,
                raw_authority
                    .pointer(&pointer)
                    .unwrap_or_else(|| panic!("missing authority pointer {pointer}")),
            );
            let original_raw = mutate_leaf(&mut raw_authority, &pointer);
            let mutated_raw_commitment = leaf_commitment(
                "raw-authority-leaf-v1",
                &pointer,
                raw_authority
                    .pointer(&pointer)
                    .unwrap_or_else(|| panic!("missing authority pointer {pointer}")),
            );
            assert_ne!(
                mutated_raw_commitment, original_raw_commitment,
                "raw authority commitment omitted {pointer}"
            );
            *raw_authority
                .pointer_mut(&pointer)
                .unwrap_or_else(|| panic!("missing authority pointer {pointer}")) = original_raw;

            let original_semantic_commitment = leaf_commitment(
                "typed-semantic-leaf-v1",
                &pointer,
                semantic_authority
                    .pointer(&pointer)
                    .unwrap_or_else(|| panic!("missing authority pointer {pointer}")),
            );
            let original_semantic = mutate_leaf(&mut semantic_authority, &pointer);
            let mutated_semantic_commitment = leaf_commitment(
                "typed-semantic-leaf-v1",
                &pointer,
                semantic_authority
                    .pointer(&pointer)
                    .unwrap_or_else(|| panic!("missing authority pointer {pointer}")),
            );
            assert_ne!(
                mutated_semantic_commitment, original_semantic_commitment,
                "semantic authority commitment omitted {pointer}"
            );
            *semantic_authority
                .pointer_mut(&pointer)
                .unwrap_or_else(|| panic!("missing authority pointer {pointer}")) =
                original_semantic;
        }
        assert_eq!(raw_authority, authority);
        assert_eq!(semantic_authority, authority);
    }
}

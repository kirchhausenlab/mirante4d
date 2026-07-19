use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
        process::{CommandExt, ExitStatusExt},
    },
    path::{Component, Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use mirante4d_identity::Sha256Hasher;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    LoadedProfile, PreflightObservations, ProtocolAttestation, REQUIRED_SCENARIOS,
    ViewerQualificationProfile, binding_reasons, commitment_fingerprint,
    conformance_receipt::{self, ConformanceEvidence},
    load_external_profile, observe, profile_contract_sha256, read_bounded_regular_file,
    require_exact_release_admission, require_nonsymlink_components, require_sha256, require_text,
    sanitized_report as preflight_report, validate_owner_accepted_profile,
};
use crate::host::{
    QualificationBuildProvenance, RepositoryIdentity, qualification_build_provenance,
    qualification_build_provenance_evidence, repository_identity, repository_identity_at,
};
use crate::process::cargo_command;

const WORKLOAD_SCHEMA: &str = "mirante4d-viewer-performance-workload-bundle-4";
const SCRIPT_BUNDLE_SCHEMA: &str = "mirante4d-viewer-performance-script-bundle-5";
const ORACLE_SCHEMA: &str = "mirante4d-viewer-performance-oracle-bundle-3";
const RAW_REPORT_SCHEMA: &str = "mirante4d-viewer-performance-raw-private-report-5";
const RECEIPT_SCHEMA: &str = "mirante4d-viewer-performance-development-receipt-5";
const AUTOMATION_SCRIPT_SCHEMA: &str = "mirante4d-product-automation-script";
const AUTOMATION_REPORT_SCHEMA: &str = "mirante4d-product-automation-report";
const AUTOMATION_SCRIPT_SCHEMA_VERSION: u64 = 5;
const AUTOMATION_REPORT_SCHEMA_VERSION: u64 = 6;
const PRODUCT_GATE_OBSERVATION_SCHEMA: &str = "mirante4d-product-gate-batch-observation-1";
const GPU_TIMING_UNAVAILABLE_REASON: &str =
    "terminal_coordinated_presentation_settled_failure_without_exact_current_presented_interval";
const GPU_TIMING_UNAVAILABLE_DERIVATION: &str = "terminal_coordinated_presentation_settled_failure_bound_to_adjacent_unavailable_gpu_timing_checkpoint";
const IMPORTED_OPEN_READY_CONDITION: &str = "imported_open_ready";
const PRODUCT_GATE_ID_MAX_BYTES: usize = 128;
const PRODUCT_GATE_CONDITION_MAX_BYTES: usize = 128;
const PRODUCT_GATE_BATCH_MAX_OBSERVATIONS: usize = 64;
const PRODUCT_GATE_DEADLINE_MAX_NS: u64 = 7_200_000_000_000;
const MAX_NONIMPORT_STATIC_WAIT_NS: u64 = 35_000_000_000;
const PROCESS_STARTUP_ADMISSION_GRACE_NS: u64 = 30_000_000_000;
const PROCESS_CLOSEOUT_GRACE_NS: u64 = 10_000_000_000;
const SOURCE_VERIFICATION_QUIESCENCE_TIMEOUT_MS: u64 = 5_000;
const GPU_TIMING_AWAIT_TIMEOUT_MS: u64 = 5_000;
const CONFORMANCE_TIMEOUT: Duration = Duration::from_secs(30);
const ATTEMPT_ROOT_PLACEHOLDER: &str = "${ATTEMPT_ROOT}";
const WORKLOAD_MAX_BYTES: u64 = 4 * 1024 * 1024;
const SCRIPT_BUNDLE_MAX_BYTES: u64 = 32 * 1024 * 1024;
const ORACLE_MAX_BYTES: u64 = 16 * 1024 * 1024;
const SUPPORTING_PACKAGE_ROOT_MANIFEST_MAX_BYTES: u64 = 1024 * 1024;
const AUTOMATION_REPORT_MAX_BYTES: u64 = 64 * 1024 * 1024;
const RAW_REPORT_MAX_BYTES: usize = 64 * 1024 * 1024;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);
const EP01_TRACE_DERIVATION_CONTRACT: &str = "mirante4d-ep01-brickkey-trace-projection-1";
const EP01_TRACE_PACKAGE_ROLE: &str = "representative_package";
const EP01_TRACE_GEOMETRY_SHA256_DOMAIN: &[u8] = b"mirante4d-ep01-trace-geometry-v1\0";
const EP01_TRACE_ENTRIES_MAX: usize = 256;
const EP01_TRACE_LAYER_ORDINAL_MAX: u32 = 63;
const EP01_TRACE_TIME_INDEX_MAX: u64 = 1_048_575;
const EP01_TRACE_SCALE_LEVEL_MAX: u32 = 63;
const EP01_TRACE_SPATIAL_COORDINATE_MAX: u64 = 1_048_576;

pub(crate) const RUN_USAGE: &str = "usage: cargo run --release -p xtask -- \
viewer-performance-run --qualification-profile ABSOLUTE_EXTERNAL_PROFILE.json \
--workload-bundle ABSOLUTE_EXTERNAL_WORKLOAD.json \
--interaction-script-bundle ABSOLUTE_EXTERNAL_SCRIPTS.json \
--independent-oracle ABSOLUTE_EXTERNAL_ORACLE.json \
--result-directory NEW_ABSOLUTE_EXTERNAL_DIRECTORY \
--cache-condition warm|cold \
--competing-activity DESCRIPTION --power-state DESCRIPTION \
--compositor-scale-milli INTEGER";

#[derive(Clone, Debug, PartialEq, Eq)]
struct RunArgs {
    profile: PathBuf,
    workload_bundle: PathBuf,
    script_bundle: PathBuf,
    oracle_bundle: PathBuf,
    result_directory: PathBuf,
    attestation: ProtocolAttestation,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkloadBundle {
    schema: String,
    representative_package_root_manifest_sha256: String,
    supporting_temporal_package_root_manifest_sha256: String,
    import_source: ImportSourceBinding,
    ep01_trace_geometry: Ep01TraceGeometry,
    scenarios: Vec<WorkloadScenario>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Ep01TraceGeometry {
    pub(super) derivation_contract: String,
    pub(super) package_role: String,
    pub(super) whole_layer: Vec<Ep01WholeLayerTrace>,
    pub(super) numeric_boxes: Vec<Ep01NumericBoxTrace>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub(super) struct Ep01WholeLayerTrace {
    pub(super) logical_layer_ordinal: u32,
    pub(super) time_index: u64,
    pub(super) scale_level: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub(super) struct Ep01NumericBoxTrace {
    pub(super) logical_layer_ordinal: u32,
    pub(super) time_index: u64,
    pub(super) scale_level: u32,
    pub(super) start_zyx: [u64; 3],
    pub(super) end_zyx_exclusive: [u64; 3],
}

pub(super) fn ep01_trace_geometry_sha256(geometry: &Ep01TraceGeometry) -> String {
    let mut hasher = Sha256Hasher::new();
    hasher.update(EP01_TRACE_GEOMETRY_SHA256_DOMAIN);
    hasher.update(
        u64::try_from(geometry.derivation_contract.len())
            .expect("EP-01 trace derivation contract length fits u64")
            .to_le_bytes(),
    );
    hasher.update(geometry.derivation_contract.as_bytes());
    hasher.update(
        u64::try_from(geometry.package_role.len())
            .expect("EP-01 trace package-role length fits u64")
            .to_le_bytes(),
    );
    hasher.update(geometry.package_role.as_bytes());
    hasher.update(
        u64::try_from(geometry.whole_layer.len())
            .expect("EP-01 whole-layer trace count fits u64")
            .to_le_bytes(),
    );
    for trace in &geometry.whole_layer {
        hasher.update(trace.logical_layer_ordinal.to_le_bytes());
        hasher.update(trace.time_index.to_le_bytes());
        hasher.update(trace.scale_level.to_le_bytes());
    }
    hasher.update(
        u64::try_from(geometry.numeric_boxes.len())
            .expect("EP-01 numeric-box trace count fits u64")
            .to_le_bytes(),
    );
    for trace in &geometry.numeric_boxes {
        hasher.update(trace.logical_layer_ordinal.to_le_bytes());
        hasher.update(trace.time_index.to_le_bytes());
        hasher.update(trace.scale_level.to_le_bytes());
        for coordinate in trace.start_zyx {
            hasher.update(coordinate.to_le_bytes());
        }
        for coordinate in trace.end_zyx_exclusive {
            hasher.update(coordinate.to_le_bytes());
        }
    }
    hasher.finalize().to_string()
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ImportSourceBinding {
    inventory_sha256: String,
    reviewed_source_fingerprint_sha256: String,
    regular_files: u64,
    source_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkloadScenario {
    id: String,
    initial_state: WorkloadInitialState,
    phases: Vec<WorkloadPhase>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum WorkloadInitialState {
    ResidentCrossSectionAnd3d,
    ResidentFallback,
    ResidentFourPanel,
    NonresidentFourPanel,
    ApplicationCold,
    #[serde(rename = "resident_3d")]
    Resident3d,
    SettledTimepoint,
    ControlledVerification,
    FreshImport,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkloadPhase {
    name: String,
    action: String,
    primary_proof: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ScriptBundle {
    schema: String,
    scenarios: Vec<ScriptScenario>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ScriptScenario {
    id: String,
    phases: Vec<ScriptPhase>,
    instrumented_script: AutomationScriptTemplate,
    instrumentation_control_script: Option<AutomationScriptTemplate>,
    cleanup: AttemptCleanup,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ScriptPhase {
    name: String,
    start_diagnostic_label: Option<String>,
    end_diagnostic_label: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
struct AttemptCleanup {
    enabled: bool,
    imported_package_relative_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct AutomationScriptTemplate {
    schema: String,
    schema_version: u64,
    scenario: String,
    gpu_timing: bool,
    diagnostic_counters: bool,
    #[serde(default)]
    startup_bootstrap: Option<AutomationStartupBootstrap>,
    hard_safety_limits: AutomationHardSafetyLimits,
    commands: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct AutomationStartupBootstrap {
    capture_start_checkpoint: bool,
    start_diagnostic_label: Option<String>,
    commands: Vec<Value>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
struct AutomationHardSafetyLimits {
    max_cpu_total_bytes: Option<u64>,
    max_cpu_decoded_residency_bytes: Option<u64>,
    max_cpu_upload_staging_bytes: Option<u64>,
    max_cpu_in_flight_decode_bytes: Option<u64>,
    max_cpu_metadata_and_indexes_bytes: Option<u64>,
    max_cpu_queues_and_results_bytes: Option<u64>,
    max_cpu_prefetch_bytes: Option<u64>,
    max_cpu_import_working_set_bytes: Option<u64>,
    max_runtime_queued_requests: Option<u64>,
    max_runtime_in_flight_decodes: Option<u64>,
    max_runtime_pending_completions: Option<u64>,
    max_runtime_resident_resources: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OracleBundle {
    schema: String,
    independent_sources: IndependentOracleSources,
    numerical_contract: NumericalContract,
    conformance_cases: Vec<ConformanceCaseBinding>,
    scenarios: Vec<OracleScenario>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IndependentOracleSources {
    lod_oracle_source_sha256: String,
    numerical_oracle_source_sha256: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct NumericalContract {
    scalar_absolute_tolerance: f64,
    scalar_relative_tolerance: f64,
    premultiplied_rgba_absolute_tolerance: f64,
    world_position_absolute_tolerance: f64,
    ray_distance_absolute_tolerance: f64,
    rgba8_channel_tolerance: u8,
    coverage_exact: bool,
    validity_exact: bool,
    source_order_exact: bool,
    sample_ordinal_exact: bool,
    pick_kind_exact: bool,
    pick_completeness_exact: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConformanceCaseBinding {
    id: String,
    harness: ConformanceHarness,
    input_fact_sha256: String,
    expected_fact_sha256: String,
    pixel: [u32; 2],
    sampling: String,
    mode: String,
    expected_rgba8: [u8; 4],
    expected_premultiplied_rgba: [f64; 4],
    covered: bool,
    valid: bool,
    hit_depth_world: Option<f64>,
    pick: Option<ExpectedPickFact>,
    authored_order: Vec<u32>,
    source_order: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ConformanceHarness {
    PlaneMipIsoDepthAndPick,
    OffAxisPerspectiveDvrWorldDistance,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExpectedPickFact {
    kind: String,
    value: f64,
    world: [f64; 3],
    distance_world: f64,
    complete: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OracleScenario {
    id: String,
    phases: Vec<OraclePhase>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OraclePhase {
    name: String,
    phase_state: PhaseStateBinding,
    require_interaction_metrics: bool,
    require_current_complete: bool,
    require_coordinated_layout_complete: bool,
    expected_scale_level: Option<u32>,
    expected_cross_section_layers: Vec<ExpectedCrossSectionLayer>,
    gpu_gate: Option<GpuGate>,
    settlement_gate: Option<SettlementGate>,
    verification_gate: Option<VerificationGate>,
    phase_start_target_residency: Option<PhaseStartTargetResidencyExpectation>,
    structural_gate: StructuralGate,
    zero_work_counters: Vec<ZeroWorkCounter>,
    unique_work: UniqueWorkExpectation,
    minimum_exact_useful_sample_bytes: Option<u64>,
    expected_imported_root_manifest_sha256: Option<String>,
    import_gate: Option<ImportGate>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ImportGate {
    required_worker_stage_names: Vec<String>,
    required_projected_stage_names: Vec<String>,
    required_receipt_stage_names: Vec<String>,
    required_progress: Vec<ImportProgressExpectation>,
    expected: ImportExpectedCounts,
    limits: ImportLimits,
    publication_currentness: ImportPublicationCurrentnessExpectation,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ImportProgressExpectation {
    stage: String,
    minimum_completed_work_units: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ImportExpectedCounts {
    successful_runs: u64,
    published_events: u64,
    failed_runs: u64,
    cancelled_runs: u64,
    resumed_work_units: u64,
    checkpoint_pending_work_units: u64,
    produced_work_units: u64,
    checkpoint_durable_work_units: u64,
    scientific_brick_reads: u64,
    staged_structure_object_reads: u64,
    staged_exact_object_reads: u64,
    scientific_object_reads: u64,
    scientific_payload_object_reads: u64,
    object_reads: u64,
    tiff_open_count: u64,
    native_chunk_decode_count: u64,
    peak_checkpoint_regular_files: u64,
    minimum_progress_updates: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ImportLimits {
    maximum_peak_working_bytes: u64,
    maximum_peak_process_rss_bytes: u64,
    maximum_product_peak_open_file_descriptors: u64,
    maximum_open_file_descriptor_structural_bound: u64,
    maximum_preflight_temporary_bytes_bound: u64,
    maximum_peak_temporary_bytes: u64,
    maximum_sync_calls: u64,
    maximum_app_primary_wall_time_ns: u64,
    maximum_app_primary_cpu_time_ns: u64,
    maximum_publication_to_open_ready_wall_time_ns: u64,
    maximum_publication_to_open_ready_cpu_time_ns: u64,
    maximum_receipt_primary_wall_time_ns: u64,
    maximum_receipt_primary_cpu_time_ns: u64,
    maximum_source_read_amplification_numerator: u64,
    maximum_source_read_amplification_denominator: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ImportPublicationCurrentnessExpectation {
    contract_id: String,
    expected_snapshot_object_reads: u64,
    first_inventory_object_reads: u64,
    observed_snapshot_object_reads: u64,
    second_inventory_object_reads: u64,
    observed_total_object_reads: u64,
    observed_codec_decode_calls: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PhaseStartTargetResidencyExpectation {
    resident_target_intersection: ExactResourcePartition,
    nonresident_target_difference: ExactResourcePartition,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ExactResourcePartition {
    canonical_entries_sha256: String,
    unique_keys: u64,
    unique_payload_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct UniqueWorkExpectation {
    start_union: ExactResourceUnion,
    target_union: ExactResourceUnion,
    residency_baseline: Option<ResidencyBaselineExpectation>,
    delta_union: ExactResourceUnionDelta,
    physical_range_read_operations: InclusiveU64Range,
    physical_encoded_bytes_read: InclusiveU64Range,
    codec_decode_operations: InclusiveU64Range,
    codec_decoded_bytes: InclusiveU64Range,
    dataset_submitted_requests: InclusiveU64Range,
    dataset_started_decodes: InclusiveU64Range,
    runtime_decoded_output_bytes: InclusiveU64Range,
    gpu_uploaded_resources: InclusiveU64Range,
    gpu_uploaded_payload_bytes: InclusiveU64Range,
    gpu_control_dynamic_updates: InclusiveU64Range,
    gpu_control_dynamic_upload_bytes: InclusiveU64Range,
    gpu_control_publication_writes: InclusiveU64Range,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ResidencyBaselineExpectation {
    checkpoint_label: String,
    union: ExactResourceUnion,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ExactResourceUnion {
    canonical_entries_sha256: String,
    unique_keys: u64,
    unique_payload_bytes: u64,
    summed_scope_payload_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ExactResourceUnionDelta {
    partitions_pairwise_disjoint: bool,
    retained_entries_sha256: String,
    retained_unique_keys: u64,
    retained_unique_payload_bytes: u64,
    added_entries_sha256: String,
    added_unique_keys: u64,
    added_unique_payload_bytes: u64,
    removed_entries_sha256: String,
    removed_unique_keys: u64,
    removed_unique_payload_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct InclusiveU64Range {
    minimum: u64,
    maximum: u64,
    authority: IndependentRangeAuthority,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct IndependentRangeAuthority {
    kind: IndependentRangeAuthorityKind,
    fact_id: String,
    independent_fact_sha256: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum IndependentRangeAuthorityKind {
    ExactIndependentEnumeration,
    BoundedRangeCoalescing,
    BoundedHaloAndGuard,
    BoundedRangeCoalescingHaloAndGuard,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct PhaseStateBinding {
    checkpoint_label: String,
    render_extent: super::PixelExtent,
    mapped_client_extent: super::PixelExtent,
    layout: ExpectedViewerLayout,
    active_view: ViewerPanel,
    time_index: u32,
    camera: ExpectedCameraGeometry,
    cross_section: ExpectedCrossSectionGeometry,
    layers: Vec<ExpectedLayerState>,
    ray_step_rule: ExpectedRayStepRule,
    dvr_density_scale: Option<f64>,
    iso_display_level: Option<f64>,
    iso_shading: Option<String>,
    iso_light: Option<ExpectedIsoLight>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ExpectedViewerLayout {
    Single3d,
    FourPanel,
}

impl ExpectedViewerLayout {
    const fn report_label(self) -> &'static str {
        match self {
            Self::Single3d => "Single3d",
            Self::FourPanel => "FourPanel",
        }
    }

    const fn visible_panels(self) -> &'static [ViewerPanel] {
        match self {
            Self::Single3d => &[ViewerPanel::ThreeD],
            Self::FourPanel => &ViewerPanel::ALL,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum ViewerPanel {
    ThreeD,
    Xy,
    Xz,
    Yz,
}

impl ViewerPanel {
    const ALL: [Self; 4] = [Self::ThreeD, Self::Xy, Self::Xz, Self::Yz];

    const fn report_label(self) -> &'static str {
        match self {
            Self::ThreeD => "3D",
            Self::Xy => "XY",
            Self::Xz => "XZ",
            Self::Yz => "YZ",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ExpectedCameraGeometry {
    projection: ExpectedProjection,
    target_world: [f64; 3],
    orientation_xyzw: [f64; 4],
    orthographic_world_per_screen_point: f64,
    perspective_focal_length_screen_points: f64,
    perspective_view_distance_world: f64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ExpectedProjection {
    Orthographic,
    Perspective,
}

impl ExpectedProjection {
    const fn report_label(self) -> &'static str {
        match self {
            Self::Orthographic => "Orthographic",
            Self::Perspective => "Perspective",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ExpectedCrossSectionGeometry {
    center_world: [f64; 3],
    orientation_xyzw: [f64; 4],
    world_per_screen_point: f64,
    depth_world: f64,
    planes: Vec<ExpectedCrossSectionPlane>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ExpectedCrossSectionPlane {
    panel: CrossSectionPanel,
    plane_origin_world: [f64; 3],
    u_axis_world: [f64; 3],
    v_axis_world: [f64; 3],
    normal_away_world: [f64; 3],
    world_per_screen_point: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ExpectedLayerState {
    layer_ordinal: u32,
    source_order: u32,
    visible: bool,
    scale_level: u32,
    sampling: String,
    mode: String,
    window: [f64; 2],
    gamma: f64,
    inverted: bool,
    opacity: f64,
    color_rgba: [f64; 4],
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ExpectedRayStepRule {
    rule: String,
    step_world: f64,
    maximum_steps: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ExpectedIsoLight {
    kind: String,
    detached_screen_position: Option<[f64; 2]>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StructuralGate {
    kind: StructuralGateKind,
    display_batch_authority: DisplayBatchAuthority,
    cancellation_waste_authority: CancellationWasteAuthority,
    ceilings: Option<StructuralCeilings>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DisplayBatchAuthority {
    SynchronousUiThreadPredecessor,
    CoordinatedDisplayBatch,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CancellationWasteAuthority {
    PredecessorUnattributed,
    GenerationBoundSharedBrick,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StructuralGateKind {
    ResidentGesture,
    ResidentBoundary,
    NonresidentOverlap,
    SettledUnchanged,
    RendererCutoff,
    ColdStart,
    Preprocessing,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StructuralCeilings {
    durable_gesture_commits_per_sequence_exact: u64,
    pending_display_batches_peak_maximum: u64,
    in_flight_display_batches_peak_maximum: u64,
    command_encoders_delta_maximum: u64,
    color_passes_delta_maximum: u64,
    renderer_submissions_delta_maximum: u64,
    completion_notifications_delta_maximum: u64,
    backpressure_deferrals_delta_maximum: u64,
    encoded_display_batches_delta_maximum: u64,
    encoded_but_dropped_delta_maximum: u64,
    sealed_obsolete_submitted_delta_maximum: u64,
    stale_presentations_delta_maximum: u64,
    current_presentations_delta_maximum: u64,
    demand_work_delta_maximum: u64,
    cancellation_waste_count_delta_maximum: u64,
    cancellation_waste_encoded_bytes_delta_maximum: u64,
    cancellation_waste_decoded_bytes_delta_maximum: u64,
    cancellation_waste_uploaded_bytes_delta_maximum: u64,
    cancellation_waste_cpu_time_ns_delta_maximum: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct ExpectedCrossSectionLayer {
    panel: CrossSectionPanel,
    layer_ordinal: usize,
    scale_level: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum CrossSectionPanel {
    Xy,
    Xz,
    Yz,
}

impl CrossSectionPanel {
    const fn report_label(self) -> &'static str {
        match self {
            Self::Xy => "XY",
            Self::Xz => "XZ",
            Self::Yz => "YZ",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GpuGate {
    Plane,
    Mip,
    Dvr,
    Iso,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SettlementGate {
    ColdTarget,
    NonresidentTarget,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct VerificationGate {
    kind: VerificationGateKind,
    start: VerificationCheckpointExpectation,
    end: VerificationCheckpointExpectation,
    minimum_accepted_progress_updates_delta: u64,
    completed_reader_work: Option<VerificationReaderWorkExpectation>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum VerificationGateKind {
    ActiveThroughout,
    Completes,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct VerificationCheckpointExpectation {
    state: ExpectedSourceVerificationState,
    active_operation: bool,
    started_runs: u64,
    cancelled_runs: u64,
    failed_runs: u64,
    accepted_successes: u64,
    completed_reader_runs: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ExpectedSourceVerificationState {
    Required,
    Verifying,
    Verified,
}

impl ExpectedSourceVerificationState {
    const fn report_label(self) -> &'static str {
        match self {
            Self::Required => "Required",
            Self::Verifying => "Verifying",
            Self::Verified => "Verified",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct VerificationReaderWorkExpectation {
    object_open_operations: InclusiveU64Range,
    physical_range_read_operations: InclusiveU64Range,
    physical_encoded_bytes_read: InclusiveU64Range,
    codec_decode_operations: InclusiveU64Range,
    codec_decoded_bytes: InclusiveU64Range,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum ZeroWorkCounter {
    PhysicalRangeReads,
    CodecDecodes,
    ObjectOpens,
    DatasetRequests,
    DatasetDecodes,
    CancelledRequests,
    PayloadUploads,
    ResidencyEvictions,
    StaticControlRebuilds,
    DenseControlFallbacks,
    QueueSubmissions,
    PayloadReuploads,
    ArenaAllocatorPlans,
    GpuControlBufferAllocations,
    GpuBindGroupCreations,
    GpuPipelineCreations,
    ResidencyDirectoryUpdates,
    PageLayoutConstructions,
    PageTableUpdates,
    FullDemandTraversals,
    PlannerCandidateVisits,
    UiThreadCandidateVisits,
    UiWaitForDemandPreparation,
    RendererStaticPreparations,
    CancellationChurn,
    CancellationWasteEncodedBytes,
    CancellationWasteDecodedBytes,
    CancellationWasteUploadedBytes,
    CancellationWasteCpuTimeNs,
    DurableProjectRevisions,
    UndoHistoryEntries,
    EncodedDisplayBatches,
    EncodedButDroppedBatches,
    SealedObsoleteSubmittedBatches,
    StalePresentedBatches,
    RendererSubmissions,
    PresentationChurn,
    DemandWork,
}

impl ZeroWorkCounter {
    const RESIDENT_MANDATORY: &'static [Self] = &[
        Self::PhysicalRangeReads,
        Self::CodecDecodes,
        Self::ObjectOpens,
        Self::DatasetRequests,
        Self::DatasetDecodes,
        Self::CancelledRequests,
        Self::PayloadUploads,
        Self::ResidencyEvictions,
        Self::StaticControlRebuilds,
        Self::DenseControlFallbacks,
        Self::PayloadReuploads,
        Self::ArenaAllocatorPlans,
        Self::GpuControlBufferAllocations,
        Self::GpuBindGroupCreations,
        Self::GpuPipelineCreations,
        Self::ResidencyDirectoryUpdates,
        Self::PageLayoutConstructions,
        Self::PageTableUpdates,
        Self::FullDemandTraversals,
        Self::PlannerCandidateVisits,
        Self::UiThreadCandidateVisits,
        Self::UiWaitForDemandPreparation,
        Self::RendererStaticPreparations,
        Self::CancellationChurn,
        Self::CancellationWasteEncodedBytes,
        Self::CancellationWasteDecodedBytes,
        Self::CancellationWasteUploadedBytes,
        Self::CancellationWasteCpuTimeNs,
        Self::EncodedButDroppedBatches,
        Self::SealedObsoleteSubmittedBatches,
        Self::StalePresentedBatches,
    ];

    const SETTLED_ADDITIONAL: &'static [Self] = &[
        Self::EncodedDisplayBatches,
        Self::RendererSubmissions,
        Self::PresentationChurn,
        Self::DemandWork,
        Self::QueueSubmissions,
    ];

    const NONRESIDENT_MANDATORY: &'static [Self] =
        &[Self::PayloadReuploads, Self::StalePresentedBatches];

    fn value(self, diagnostics: &Value) -> Option<u64> {
        let pointer = match self {
            Self::PhysicalRangeReads => "/dataset_source_io/reader/physical_range_read_operations",
            Self::CodecDecodes => "/dataset_source_io/reader/codec_decode_operations",
            Self::ObjectOpens => "/dataset_source_io/reader/object_open_operations",
            Self::DatasetRequests => "/dataset_runtime/counters/submitted_requests",
            Self::DatasetDecodes => "/dataset_runtime/counters/started_decodes",
            Self::CancelledRequests | Self::CancellationChurn => {
                "/dataset_runtime/counters/cancelled_requests"
            }
            Self::PayloadUploads => "/gpu_adapter/uploads/payload_bytes",
            Self::ResidencyEvictions => "/gpu_adapter/residency/evictions",
            Self::StaticControlRebuilds => "/gpu_adapter/control/static_rebuilds",
            Self::DenseControlFallbacks => "/gpu_adapter/control/dense_fallbacks",
            Self::QueueSubmissions | Self::RendererSubmissions => "/gpu_adapter/queue_submissions",
            Self::PayloadReuploads => "/gpu_adapter/residency/epoch_reuploads",
            Self::ArenaAllocatorPlans => "/gpu_adapter/control/allocator_plans",
            Self::GpuControlBufferAllocations => "/gpu_adapter/control/buffer_allocations",
            Self::GpuBindGroupCreations => "/gpu_adapter/control/bind_group_creations",
            Self::GpuPipelineCreations => "/gpu_adapter/control/pipeline_creations",
            Self::ResidencyDirectoryUpdates => "/gpu_adapter/control/residency_directory_updates",
            Self::PageLayoutConstructions => "/gpu_adapter/control/page_layout_constructions",
            Self::PageTableUpdates => "/gpu_adapter/control/page_table_updates",
            Self::FullDemandTraversals => {
                "/dataset_demand/planned_scope_accounting/full_demand_traversals"
            }
            Self::PlannerCandidateVisits => {
                "/dataset_demand/planned_scope_accounting/planner_candidate_visits"
            }
            Self::UiThreadCandidateVisits => {
                "/dataset_demand/planned_scope_accounting/ui_thread_candidate_visits"
            }
            Self::UiWaitForDemandPreparation => {
                "/dataset_demand/planned_scope_accounting/ui_wait_for_demand_preparation_count"
            }
            Self::RendererStaticPreparations => {
                "/render/display_coordination/detailed_counters/renderer_static_preparations"
            }
            Self::CancellationWasteEncodedBytes => {
                "/dataset_source_io/reader/cancelled_encoded_bytes"
            }
            Self::CancellationWasteDecodedBytes => {
                "/dataset_runtime/performance/cancelled_decode_bytes"
            }
            Self::CancellationWasteUploadedBytes => "/gpu_adapter/uploads/cancelled_payload_bytes",
            Self::CancellationWasteCpuTimeNs => {
                "/dataset_runtime/performance/cancelled_decode_time_ns"
            }
            Self::DurableProjectRevisions => "/project_state/revision_high_water_sequence",
            Self::UndoHistoryEntries => "/project_state/history_entry_high_water_sequence",
            Self::EncodedDisplayBatches => {
                "/render/display_coordination/detailed_counters/encoded_display_batches"
            }
            Self::EncodedButDroppedBatches => {
                "/render/display_coordination/detailed_counters/encoded_but_dropped_batches"
            }
            Self::SealedObsoleteSubmittedBatches => {
                "/render/display_coordination/detailed_counters/sealed_obsolete_submitted_batches"
            }
            Self::StalePresentedBatches => "/render/progressive_presentation/stale_frames_rejected",
            Self::PresentationChurn => {
                "/render/display_coordination/detailed_counters/current_presentations"
            }
            Self::DemandWork => "/dataset_demand/planned_scope_accounting/demand_work",
        };
        diagnostics.pointer(pointer).and_then(Value::as_u64)
    }

    const fn reason_label(self) -> &'static str {
        match self {
            Self::PhysicalRangeReads => "physical_range_reads",
            Self::CodecDecodes => "codec_decodes",
            Self::ObjectOpens => "object_opens",
            Self::DatasetRequests => "dataset_requests",
            Self::DatasetDecodes => "dataset_decodes",
            Self::CancelledRequests => "cancelled_requests",
            Self::PayloadUploads => "payload_uploads",
            Self::ResidencyEvictions => "residency_evictions",
            Self::StaticControlRebuilds => "static_control_rebuilds",
            Self::DenseControlFallbacks => "dense_control_fallbacks",
            Self::QueueSubmissions => "queue_submissions",
            Self::PayloadReuploads => "payload_reuploads",
            Self::ArenaAllocatorPlans => "arena_allocator_plans",
            Self::GpuControlBufferAllocations => "gpu_control_buffer_allocations",
            Self::GpuBindGroupCreations => "gpu_bind_group_creations",
            Self::GpuPipelineCreations => "gpu_pipeline_creations",
            Self::ResidencyDirectoryUpdates => "residency_directory_updates",
            Self::PageLayoutConstructions => "page_layout_constructions",
            Self::PageTableUpdates => "page_table_updates",
            Self::FullDemandTraversals => "full_demand_traversals",
            Self::PlannerCandidateVisits => "planner_candidate_visits",
            Self::UiThreadCandidateVisits => "ui_thread_candidate_visits",
            Self::UiWaitForDemandPreparation => "ui_wait_for_demand_preparation",
            Self::RendererStaticPreparations => "renderer_static_preparations",
            Self::CancellationChurn => "cancellation_churn",
            Self::CancellationWasteEncodedBytes => "cancellation_waste_encoded_bytes",
            Self::CancellationWasteDecodedBytes => "cancellation_waste_decoded_bytes",
            Self::CancellationWasteUploadedBytes => "cancellation_waste_uploaded_bytes",
            Self::CancellationWasteCpuTimeNs => "cancellation_waste_cpu_time_ns",
            Self::DurableProjectRevisions => "durable_project_revisions",
            Self::UndoHistoryEntries => "undo_history_entries",
            Self::EncodedDisplayBatches => "encoded_display_batches",
            Self::EncodedButDroppedBatches => "encoded_but_dropped_batches",
            Self::SealedObsoleteSubmittedBatches => "sealed_obsolete_submitted_batches",
            Self::StalePresentedBatches => "stale_presented_batches",
            Self::RendererSubmissions => "renderer_submissions",
            Self::PresentationChurn => "presentation_churn",
            Self::DemandWork => "demand_work",
        }
    }
}

#[derive(Debug)]
struct LoadedBundle<T> {
    value: T,
    sha256: String,
    path: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BundleCommitments {
    pub(super) workload_bundle_sha256: String,
    pub(super) interaction_script_bundle_sha256: String,
    pub(super) independent_oracle_sha256: String,
    pub(super) ep01_trace_geometry_sha256: String,
}

pub(super) fn load_and_validate_preflight_bundles(
    profile: &ViewerQualificationProfile,
    workload_path: &Path,
    script_path: &Path,
    oracle_path: &Path,
    repository_root: &Path,
) -> anyhow::Result<BundleCommitments> {
    let workload = load_external_json::<WorkloadBundle>(
        workload_path,
        repository_root,
        WORKLOAD_MAX_BYTES,
        "viewer workload bundle",
    )?;
    let scripts = load_external_json::<ScriptBundle>(
        script_path,
        repository_root,
        SCRIPT_BUNDLE_MAX_BYTES,
        "viewer interaction-script bundle",
    )?;
    let oracle = load_external_json::<OracleBundle>(
        oracle_path,
        repository_root,
        ORACLE_MAX_BYTES,
        "viewer independent-oracle bundle",
    )?;
    validate_bundles(profile, &workload, &scripts, &oracle, repository_root)?;
    Ok(BundleCommitments {
        workload_bundle_sha256: workload.sha256,
        interaction_script_bundle_sha256: scripts.sha256,
        independent_oracle_sha256: oracle.sha256,
        ep01_trace_geometry_sha256: ep01_trace_geometry_sha256(&workload.value.ep01_trace_geometry),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttemptRole {
    Instrumented,
    InstrumentationControl,
}

impl AttemptRole {
    const fn directory_name(self) -> &'static str {
        match self {
            Self::Instrumented => "instrumented",
            Self::InstrumentationControl => "instrumentation-control",
        }
    }
}

#[derive(Debug)]
struct ProcessObservation {
    launch_attempted: bool,
    status: Option<ExitStatus>,
    external_wall_time_ns: u64,
    timed_out: bool,
    spawn_error: Option<String>,
}

#[derive(Debug)]
struct RoleEvidence {
    role: AttemptRole,
    root: PathBuf,
    expanded_script_sha256: String,
    template_script_sha256: String,
    process: ProcessObservation,
    automation_report: Option<Value>,
    automation_report_sha256: Option<String>,
    app_wall_time_ns: Option<u64>,
    process_cpu_time_ns: Option<u64>,
    derived_process_timeout_ns: u64,
    static_wait_bound_ns: u64,
    gate_batch_count: usize,
    gate_observation_count: usize,
    source_inventory_before: Option<super::source_inventory::InventoryFacts>,
    source_inventory_after: Option<super::source_inventory::InventoryFacts>,
    cleanup_manifest_sha256: Option<String>,
    cleanup_completed: bool,
    product_gate_outcomes: Vec<ProductGateOutcome>,
    reasons: BTreeSet<String>,
}

#[derive(Debug)]
struct ImmutableSourceBuild {
    source_root: PathBuf,
    app_binary: PathBuf,
}

#[derive(Debug)]
struct RunImmutabilityBinding {
    live_repository: RepositoryIdentity,
    source_repository: RepositoryIdentity,
    source_root: PathBuf,
    app_binary_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProductGateOutcome {
    command_index: usize,
    batch_id: String,
    phase_id: String,
    observation_index: usize,
    gate_id: String,
    condition: String,
    deadline_authority: String,
    deadline_after_origin_ns: u64,
    origin_kind: String,
    origin_command_index: Option<usize>,
    outcome: ProductGateStatus,
    condition_met: bool,
    timed_out: bool,
    observed_after_origin_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProductGateStatus {
    Passed,
    Failed,
}

impl ProductGateStatus {
    const fn report_label(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PopulationEvidence {
    expected_sample_records: usize,
    observed_sample_records: usize,
    expected_role_attempts: usize,
    observed_role_attempts: usize,
    completed_role_reports: usize,
    expected_phase_evaluations: usize,
    observed_phase_evaluations: usize,
    expected_product_gate_observations: usize,
    observed_product_gate_observations: usize,
    sample_identities_exact: bool,
    sample_order_exact: bool,
    role_identities_exact: bool,
    role_order_exact: bool,
    phase_identities_exact: bool,
    product_gate_bijections_exact: bool,
}

#[derive(Debug)]
struct PhaseEvaluation {
    name: String,
    reasons: BTreeSet<String>,
}

#[derive(Debug)]
struct SampleEvidence {
    sample_index: u32,
    scenario: String,
    role_launch_order: Vec<AttemptRole>,
    instrumented: RoleEvidence,
    control: Option<RoleEvidence>,
    phases: Vec<PhaseEvaluation>,
    instrumented_qualification_wait_wall_ns: Option<u64>,
    instrumented_adjusted_wall_time_ns: Option<u64>,
    wall_overhead_basis_points: Option<u64>,
    process_cpu_overhead_basis_points: Option<u64>,
    reasons: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InstrumentationOverheadPopulationEvidence {
    scenario: String,
    expected_sample_pairs: usize,
    observed_sample_pairs: usize,
    instrumented_raw_app_wall_time_ns: Option<u64>,
    instrumented_qualification_wait_wall_ns: Option<u64>,
    instrumented_adjusted_app_wall_time_ns: Option<u64>,
    control_app_wall_time_ns: Option<u64>,
    wall_overhead_basis_points: Option<u64>,
    instrumented_process_cpu_time_ns: Option<u64>,
    control_process_cpu_time_ns: Option<u64>,
    process_cpu_overhead_basis_points: Option<u64>,
    maximum_overhead_basis_points: u64,
    population_complete: bool,
    gate_evaluable: bool,
    gate_passed: Option<bool>,
}

pub(crate) fn run_measurement(arguments: Vec<String>) -> anyhow::Result<()> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "help" | "--help" | "-h") {
        println!("{RUN_USAGE}");
        return Ok(());
    }
    let args = parse_args(arguments)?;
    let repository = repository_identity();
    let repository_root = repository
        .root
        .as_deref()
        .context("viewer performance runner could not identify the repository root")?;
    let repository_root = fs::canonicalize(repository_root)
        .context("viewer performance runner could not resolve the repository root")?;

    let profile = load_external_profile(&args.profile, &repository_root)?;
    validate_owner_accepted_profile(&profile.profile)?;
    let xtask_build = qualification_build_provenance();
    require_exact_release_admission(
        &profile.profile,
        &repository,
        &xtask_build,
        &repository_root,
    )?;
    let workload = load_external_json::<WorkloadBundle>(
        &args.workload_bundle,
        &repository_root,
        WORKLOAD_MAX_BYTES,
        "viewer workload bundle",
    )?;
    let scripts = load_external_json::<ScriptBundle>(
        &args.script_bundle,
        &repository_root,
        SCRIPT_BUNDLE_MAX_BYTES,
        "viewer interaction-script bundle",
    )?;
    let oracle = load_external_json::<OracleBundle>(
        &args.oracle_bundle,
        &repository_root,
        ORACLE_MAX_BYTES,
        "viewer independent-oracle bundle",
    )?;
    validate_bundles(
        &profile.profile,
        &workload,
        &scripts,
        &oracle,
        &repository_root,
    )?;
    validate_oracle_source_commitments(&oracle.value, &repository_root)?;
    let result_root = create_result_directory(&args.result_directory, &repository_root)?;
    let immutable_build = build_fresh_release_app(
        &repository_root,
        &result_root,
        &profile.profile.build.repository_revision,
        &profile.profile.build.compiler,
    )?;
    let app_binary = immutable_build.app_binary;
    let app_binary_sha256 = digest_regular_file(&app_binary, "viewer app binary")?;
    let immutability = RunImmutabilityBinding {
        live_repository: repository.clone(),
        source_repository: repository_identity_at(&immutable_build.source_root),
        source_root: immutable_build.source_root,
        app_binary_sha256: app_binary_sha256.clone(),
    };

    let observations = observe(&profile.profile, &repository_root);
    let binding_reason_codes = binding_reasons(&profile.profile, &args.attestation, &observations)
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();
    let mut all_reasons = binding_reason_codes.clone();
    let mut conformance = None;
    if binding_reason_codes.is_empty() {
        let conformance_result = (|| -> anyhow::Result<ConformanceEvidence> {
            let fresh_target = app_binary
                .parent()
                .and_then(Path::parent)
                .context("fresh viewer target directory is unavailable")?;
            let oracle_cases = oracle
                .value
                .conformance_cases
                .iter()
                .map(serde_json::to_value)
                .collect::<Result<Vec<_>, _>>()
                .context("failed to encode numerical oracle cases for executable binding")?;
            let numerical_contract = serde_json::to_value(oracle.value.numerical_contract)
                .context("failed to encode the numerical contract for executable binding")?;
            conformance_receipt::execute(
                &immutability.source_root,
                fresh_target,
                &result_root,
                CONFORMANCE_TIMEOUT,
                &oracle_cases,
                &numerical_contract,
            )
        })();
        match conformance_result {
            Ok(evidence) => {
                all_reasons.extend(evidence.reason_codes().iter().cloned());
                conformance = Some(evidence);
            }
            Err(_) => {
                all_reasons.insert("conformance_execution_setup_failed".to_owned());
            }
        }
    }
    let mut samples = Vec::new();
    if !has_integrity_reasons(&all_reasons) {
        samples = execute_samples(
            &profile,
            &workload.value.import_source,
            &scripts.value,
            &oracle.value,
            &app_binary,
            &result_root,
            &immutability,
        );
        for sample in &samples {
            all_reasons.extend(sample.reasons.iter().cloned());
            all_reasons.extend(sample.instrumented.reasons.iter().cloned());
            if let Some(control) = &sample.control {
                all_reasons.extend(control.reasons.iter().cloned());
            }
            for phase in &sample.phases {
                all_reasons.extend(phase.reasons.iter().cloned());
            }
        }
    } else if !binding_reason_codes.is_empty() {
        all_reasons.insert("qualification_binding_mismatch".to_owned());
    }
    let population =
        validate_attempt_population(&profile.profile, &scripts.value, &samples, &mut all_reasons);
    let instrumentation_overhead_populations = validate_population_instrumentation_overhead(
        &profile.profile,
        &samples,
        population,
        &mut all_reasons,
    );

    let repository_end = repository_identity();
    if !repository_identity_unchanged_and_clean(&repository, &repository_end)
        || repository_end.commit.as_deref()
            != Some(profile.profile.build.repository_revision.as_str())
    {
        all_reasons.insert("repository_changed_or_dirty_during_run".to_owned());
    }
    let source_repository_end = repository_identity_at(&immutability.source_root);
    if !repository_identity_unchanged_and_clean(
        &immutability.source_repository,
        &source_repository_end,
    ) {
        all_reasons.insert("immutable_source_changed_during_run".to_owned());
    }
    let app_binary_sha256_end = digest_regular_file(&app_binary, "viewer app binary after run")?;
    if app_binary_sha256_end != app_binary_sha256 {
        all_reasons.insert("app_binary_changed_during_run".to_owned());
    }

    let raw = raw_report(
        &args,
        &profile,
        &workload,
        &scripts,
        &oracle,
        &app_binary,
        &app_binary_sha256,
        &app_binary_sha256_end,
        &result_root,
        &observations,
        &binding_reason_codes,
        conformance.as_ref(),
        &samples,
        population,
        &instrumentation_overhead_populations,
        &all_reasons,
        &repository,
        &repository_end,
        &immutability.source_repository,
        &source_repository_end,
        &xtask_build,
    );
    let raw_path = result_root.join("raw-private-report.json");
    write_new_synced_json(&raw_path, &raw)?;
    let raw_sha256 = digest_regular_file(&raw_path, "viewer raw private report")?;
    let receipt = sanitized_receipt(
        &profile,
        &workload,
        &scripts,
        &oracle,
        &app_binary_sha256,
        &raw_sha256,
        conformance.as_ref(),
        &samples,
        population,
        &instrumentation_overhead_populations,
        &all_reasons,
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&receipt)
            .context("failed to encode viewer development receipt")?
    );
    require_valid_evidence(&all_reasons)
}

fn require_valid_evidence(reasons: &BTreeSet<String>) -> anyhow::Result<()> {
    if has_integrity_reasons(reasons) {
        bail!(
            "viewer performance evidence is invalid or incomplete; inspect the private raw report"
        )
    }
    Ok(())
}

fn validate_oracle_source_commitments(
    oracle: &OracleBundle,
    repository_root: &Path,
) -> anyhow::Result<()> {
    for (relative, expected, label) in [
        (
            "crates/mirante4d-render-reference/src/lod_oracle.rs",
            oracle.independent_sources.lod_oracle_source_sha256.as_str(),
            "independent LOD oracle source",
        ),
        (
            "crates/mirante4d-render-reference/src/numerical_oracle.rs",
            oracle
                .independent_sources
                .numerical_oracle_source_sha256
                .as_str(),
            "independent numerical oracle source",
        ),
    ] {
        let observed = digest_regular_file(&repository_root.join(relative), label)?;
        if observed != expected {
            bail!("{label} commitment does not match the clean repository source")
        }
    }
    Ok(())
}

fn repository_identity_unchanged_and_clean(
    start: &RepositoryIdentity,
    end: &RepositoryIdentity,
) -> bool {
    start.root.is_some()
        && start.commit.is_some()
        && start.root == end.root
        && start.commit == end.commit
        && start.dirty_worktree == Some(false)
        && end.dirty_worktree == Some(false)
}

fn prelaunch_immutability_reason_codes_from(
    binding: &RunImmutabilityBinding,
    live_repository: &RepositoryIdentity,
    source_repository: &RepositoryIdentity,
    app_binary_sha256: Option<&str>,
) -> BTreeSet<String> {
    let mut reasons = BTreeSet::new();
    if !repository_identity_unchanged_and_clean(&binding.live_repository, live_repository) {
        reasons.insert("repository_changed_or_dirty_before_role_launch".to_owned());
    }
    if !repository_identity_unchanged_and_clean(&binding.source_repository, source_repository) {
        reasons.insert("immutable_source_changed_before_role_launch".to_owned());
    }
    match app_binary_sha256 {
        Some(observed) if observed == binding.app_binary_sha256 => {}
        Some(_) => {
            reasons.insert("app_binary_changed_before_role_launch".to_owned());
        }
        None => {
            reasons.insert("app_binary_unavailable_before_role_launch".to_owned());
        }
    }
    reasons
}

fn prelaunch_immutability_reason_codes(
    binding: &RunImmutabilityBinding,
    app_binary: &Path,
) -> BTreeSet<String> {
    let live_repository = repository_identity();
    let source_repository = repository_identity_at(&binding.source_root);
    let app_binary_sha256 =
        digest_regular_file(app_binary, "viewer app binary before role launch").ok();
    prelaunch_immutability_reason_codes_from(
        binding,
        &live_repository,
        &source_repository,
        app_binary_sha256.as_deref(),
    )
}

fn parse_args(arguments: Vec<String>) -> anyhow::Result<RunArgs> {
    let mut values = BTreeMap::<String, String>::new();
    let mut arguments = arguments.into_iter();
    while let Some(name) = arguments.next() {
        if matches!(name.as_str(), "help" | "--help" | "-h") {
            bail!("{RUN_USAGE}")
        }
        if !matches!(
            name.as_str(),
            "--qualification-profile"
                | "--workload-bundle"
                | "--interaction-script-bundle"
                | "--independent-oracle"
                | "--result-directory"
                | "--cache-condition"
                | "--competing-activity"
                | "--power-state"
                | "--compositor-scale-milli"
        ) {
            bail!("unknown viewer performance runner argument {name:?}; {RUN_USAGE}")
        }
        let value = arguments
            .next()
            .with_context(|| format!("{name} requires a value; {RUN_USAGE}"))?;
        if values.insert(name.clone(), value).is_some() {
            bail!("{name} may be supplied only once; {RUN_USAGE}")
        }
    }
    let required = |name: &str| {
        values
            .get(name)
            .cloned()
            .with_context(|| format!("{name} is required; {RUN_USAGE}"))
    };
    let compositor_scale_milli = required("--compositor-scale-milli")?
        .parse::<u32>()
        .context("--compositor-scale-milli must be an unsigned integer")?;
    Ok(RunArgs {
        profile: PathBuf::from(required("--qualification-profile")?),
        workload_bundle: PathBuf::from(required("--workload-bundle")?),
        script_bundle: PathBuf::from(required("--interaction-script-bundle")?),
        oracle_bundle: PathBuf::from(required("--independent-oracle")?),
        result_directory: PathBuf::from(required("--result-directory")?),
        attestation: ProtocolAttestation {
            cache_condition: required("--cache-condition")?,
            competing_activity: required("--competing-activity")?,
            power_state: required("--power-state")?,
            compositor_scale_milli,
        },
    })
}

fn load_external_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    repository_root: &Path,
    max_bytes: u64,
    label: &str,
) -> anyhow::Result<LoadedBundle<T>> {
    if !path.is_absolute() {
        bail!("{label} path must be absolute")
    }
    require_nonsymlink_components(path, label)?;
    let canonical = fs::canonicalize(path).with_context(|| format!("{label} is unavailable"))?;
    if canonical.starts_with(repository_root) {
        bail!("{label} must be outside the repository")
    }
    let bytes = read_bounded_regular_file(&canonical, max_bytes, label)?;
    let sha256 = Sha256Hasher::digest(&bytes).to_string();
    let value = serde_json::from_slice(&bytes)
        .with_context(|| format!("{label} is not strict valid JSON"))?;
    Ok(LoadedBundle {
        value,
        sha256,
        path: canonical,
    })
}

fn validate_bundles(
    profile: &ViewerQualificationProfile,
    workload: &LoadedBundle<WorkloadBundle>,
    scripts: &LoadedBundle<ScriptBundle>,
    oracle: &LoadedBundle<OracleBundle>,
    repository_root: &Path,
) -> anyhow::Result<()> {
    if workload.sha256 != profile.workload.workload_bundle_sha256 {
        bail!("viewer workload bundle does not match its qualification-profile commitment")
    }
    if scripts.sha256 != profile.workload.interaction_script_bundle_sha256 {
        bail!("viewer script bundle does not match its qualification-profile commitment")
    }
    if oracle.sha256 != profile.workload.independent_oracle_sha256 {
        bail!("viewer oracle bundle does not match its qualification-profile commitment")
    }
    validate_workload_schema(&workload.value.schema)?;
    if scripts.value.schema != SCRIPT_BUNDLE_SCHEMA {
        bail!("viewer script bundle schema must be {SCRIPT_BUNDLE_SCHEMA:?}")
    }
    if oracle.value.schema != ORACLE_SCHEMA {
        bail!("viewer oracle bundle schema must be {ORACLE_SCHEMA:?}")
    }
    validate_oracle_contract(&oracle.value)?;
    validate_oracle_source_commitments(&oracle.value, repository_root)?;
    require_sha256(
        &workload.value.representative_package_root_manifest_sha256,
        "workload representative-package manifest commitment",
    )?;
    require_sha256(
        &workload
            .value
            .supporting_temporal_package_root_manifest_sha256,
        "workload supporting temporal-package manifest commitment",
    )?;
    validate_import_source_binding(&workload.value.import_source)?;
    validate_ep01_trace_geometry(&workload.value.ep01_trace_geometry)?;
    if workload.value.representative_package_root_manifest_sha256
        != profile.workload.representative_package.root_manifest_sha256
    {
        bail!("viewer workload bundle binds a different representative package")
    }

    let workload_map = scenario_map(&workload.value.scenarios, |scenario| &scenario.id)?;
    let script_map = scenario_map(&scripts.value.scenarios, |scenario| &scenario.id)?;
    let oracle_map = scenario_map(&oracle.value.scenarios, |scenario| &scenario.id)?;
    let ip_script = script_map
        .get("IP")
        .expect("exact scenario coverage includes IP");
    let import_source = sole_ip_source_path(&ip_script.instrumented_script.commands)?;
    validate_import_source_path(import_source, repository_root)?;
    let mut product_gate_ids = BTreeSet::new();
    for id in REQUIRED_SCENARIOS {
        let workload_scenario = workload_map
            .get(id)
            .expect("exact scenario coverage was checked");
        let script_scenario = script_map
            .get(id)
            .expect("exact scenario coverage was checked");
        let oracle_scenario = oracle_map
            .get(id)
            .expect("exact scenario coverage was checked");
        validate_workload_scenario(id, workload_scenario)?;
        validate_oracle_scenario(id, oracle_scenario)?;
        validate_script_scenario(id, script_scenario, profile, oracle_scenario)?;
        for gate in
            expected_product_gate_observations(&script_scenario.instrumented_script.commands)?
        {
            if !product_gate_ids.insert(gate.gate_id) {
                bail!("viewer product gate IDs must be unique across all scenarios")
            }
        }
        let workload_phases = workload_scenario
            .phases
            .iter()
            .map(|phase| phase.name.as_str())
            .collect::<Vec<_>>();
        let script_phases = script_scenario
            .phases
            .iter()
            .map(|phase| phase.name.as_str())
            .collect::<Vec<_>>();
        let oracle_phases = oracle_scenario
            .phases
            .iter()
            .map(|phase| phase.name.as_str())
            .collect::<Vec<_>>();
        if workload_phases != script_phases || script_phases != oracle_phases {
            bail!("viewer scenario {id} phase order differs across committed bundles")
        }
        for (script_phase, oracle_phase) in
            script_scenario.phases.iter().zip(&oracle_scenario.phases)
        {
            if oracle_phase.phase_state.checkpoint_label != script_phase.end_diagnostic_label {
                bail!(
                    "viewer scenario {id} phase {:?} oracle checkpoint does not bind the script checkpoint",
                    script_phase.name
                )
            }
            if script_phase.start_diagnostic_label.is_none() {
                bail!(
                    "viewer scenario {id} phase {:?} needs a start checkpoint for structural and unique-work deltas",
                    script_phase.name
                )
            }
        }
        if id == "VV" {
            let commands = &script_scenario.instrumented_script.commands;
            let verification_wait = verification_completion_observation_index(commands)
                .expect("VV required-action validation found the measured completion observation");
            for (script_phase, oracle_phase) in
                script_scenario.phases.iter().zip(&oracle_scenario.phases)
            {
                let start = script_phase
                    .start_diagnostic_label
                    .as_deref()
                    .and_then(|label| diagnostic_command_index(commands, label))
                    .expect("VV phase start diagnostic was validated");
                let end = diagnostic_command_index(commands, &script_phase.end_diagnostic_label)
                    .expect("VV phase end diagnostic was validated");
                match oracle_phase
                    .verification_gate
                    .as_ref()
                    .expect("every VV oracle phase has a verification gate")
                    .kind
                {
                    VerificationGateKind::ActiveThroughout if end >= verification_wait => {
                        bail!(
                            "VV active-throughout phase {:?} must end before the verification completion wait",
                            script_phase.name
                        )
                    }
                    VerificationGateKind::Completes
                        if start >= verification_wait || end <= verification_wait =>
                    {
                        bail!(
                            "VV completion phase {:?} must bracket the verification completion wait",
                            script_phase.name
                        )
                    }
                    _ => {}
                }
            }
        }
    }
    let pt = script_map
        .get("PT")
        .expect("exact scenario coverage and PT action validation were checked");
    let temporal_target = sole_dataset_command_path(
        &pt.instrumented_script.commands,
        "switch_dataset",
        "PT switch target",
    )?;
    validate_supporting_temporal_package(
        temporal_target,
        &workload
            .value
            .supporting_temporal_package_root_manifest_sha256,
        repository_root,
    )?;
    if !scripts.value.scenarios.iter().any(|scenario| {
        template_has_extent(
            &scenario.instrumented_script,
            profile.extents.required_exercise,
        )
    }) {
        bail!("viewer scripts must exercise the profile-bound 1920x1080 extent")
    }
    Ok(())
}

fn validate_workload_schema(schema: &str) -> anyhow::Result<()> {
    if schema != WORKLOAD_SCHEMA {
        bail!("viewer workload bundle schema must be {WORKLOAD_SCHEMA:?}")
    }
    Ok(())
}

fn validate_ep01_trace_geometry(geometry: &Ep01TraceGeometry) -> anyhow::Result<()> {
    if geometry.derivation_contract != EP01_TRACE_DERIVATION_CONTRACT {
        bail!("EP-01 trace derivation contract must be {EP01_TRACE_DERIVATION_CONTRACT:?}")
    }
    if geometry.package_role != EP01_TRACE_PACKAGE_ROLE {
        bail!("EP-01 analysis traces must target the {EP01_TRACE_PACKAGE_ROLE:?} authority")
    }
    if !(1..=EP01_TRACE_ENTRIES_MAX).contains(&geometry.whole_layer.len()) {
        bail!(
            "EP-01 whole-layer trace list must contain between 1 and {EP01_TRACE_ENTRIES_MAX} entries"
        )
    }
    if !(1..=EP01_TRACE_ENTRIES_MAX).contains(&geometry.numeric_boxes.len()) {
        bail!(
            "EP-01 numeric-box trace list must contain between 1 and {EP01_TRACE_ENTRIES_MAX} entries"
        )
    }

    for (index, trace) in geometry.whole_layer.iter().enumerate() {
        validate_ep01_trace_location(
            &format!("EP-01 whole-layer trace entry {index}"),
            trace.logical_layer_ordinal,
            trace.time_index,
            trace.scale_level,
        )?;
    }
    for (index, trace) in geometry.numeric_boxes.iter().enumerate() {
        let label = format!("EP-01 numeric-box trace entry {index}");
        validate_ep01_trace_location(
            &label,
            trace.logical_layer_ordinal,
            trace.time_index,
            trace.scale_level,
        )?;
        for coordinate in trace.start_zyx.iter().chain(trace.end_zyx_exclusive.iter()) {
            if *coordinate > EP01_TRACE_SPATIAL_COORDINATE_MAX {
                bail!(
                    "{label} spatial coordinates must be at most {EP01_TRACE_SPATIAL_COORDINATE_MAX}"
                )
            }
        }
    }

    if !geometry
        .whole_layer
        .windows(2)
        .all(|pair| pair[0] < pair[1])
    {
        bail!("EP-01 whole-layer trace list must be strictly lexicographically increasing")
    }
    if !geometry
        .numeric_boxes
        .windows(2)
        .all(|pair| pair[0] < pair[1])
    {
        bail!("EP-01 numeric-box trace list must be strictly lexicographically increasing")
    }

    for (index, trace) in geometry.numeric_boxes.iter().enumerate() {
        for axis in 0..3 {
            if trace.start_zyx[axis] >= trace.end_zyx_exclusive[axis] {
                bail!(
                    "EP-01 numeric-box trace entry {index} axis {axis} must be a positive half-open interval"
                )
            }
        }
    }
    Ok(())
}

fn validate_ep01_trace_location(
    label: &str,
    logical_layer_ordinal: u32,
    time_index: u64,
    scale_level: u32,
) -> anyhow::Result<()> {
    if logical_layer_ordinal > EP01_TRACE_LAYER_ORDINAL_MAX {
        bail!("{label} logical layer ordinal must be at most {EP01_TRACE_LAYER_ORDINAL_MAX}")
    }
    if time_index > EP01_TRACE_TIME_INDEX_MAX {
        bail!("{label} time index must be at most {EP01_TRACE_TIME_INDEX_MAX}")
    }
    if scale_level > EP01_TRACE_SCALE_LEVEL_MAX {
        bail!("{label} scale level must be at most {EP01_TRACE_SCALE_LEVEL_MAX}")
    }
    Ok(())
}

fn validate_oracle_contract(oracle: &OracleBundle) -> anyhow::Result<()> {
    require_sha256(
        &oracle.independent_sources.lod_oracle_source_sha256,
        "LOD oracle source commitment",
    )?;
    require_sha256(
        &oracle.independent_sources.numerical_oracle_source_sha256,
        "numerical oracle source commitment",
    )?;
    let contract = oracle.numerical_contract;
    let expected_relative = f64::from(f32::EPSILON) * 4.0;
    if contract.scalar_absolute_tolerance != 1.0e-6
        || contract.scalar_relative_tolerance != expected_relative
        || contract.premultiplied_rgba_absolute_tolerance != 2.0e-6
        || contract.world_position_absolute_tolerance != 1.0e-5
        || contract.ray_distance_absolute_tolerance != 1.0e-5
        || contract.rgba8_channel_tolerance != 1
        || !contract.coverage_exact
        || !contract.validity_exact
        || !contract.source_order_exact
        || !contract.sample_ordinal_exact
        || !contract.pick_kind_exact
        || !contract.pick_completeness_exact
    {
        bail!("viewer numerical contract does not match the frozen EP-00 contract")
    }
    let required_ids = BTreeSet::from([
        "plane_smooth_valid",
        "plane_smooth_invalid",
        "perspective_mip",
        "perspective_dvr_world_distance",
        "perspective_iso",
        "perspective_iso_depth_order",
    ]);
    let mut observed_ids = BTreeSet::new();
    for case in &oracle.conformance_cases {
        require_label(&case.id, "numerical conformance case id")?;
        require_sha256(&case.input_fact_sha256, "conformance input fact commitment")?;
        require_sha256(
            &case.expected_fact_sha256,
            "conformance expected fact commitment",
        )?;
        if !observed_ids.insert(case.id.as_str()) {
            bail!("viewer numerical conformance case IDs must be unique")
        }
        let expected_harness = if case.id == "perspective_dvr_world_distance" {
            ConformanceHarness::OffAxisPerspectiveDvrWorldDistance
        } else {
            ConformanceHarness::PlaneMipIsoDepthAndPick
        };
        if case.harness != expected_harness {
            bail!("viewer numerical conformance case is bound to the wrong harness")
        }
        require_text(&case.sampling, 64, "conformance sampling")?;
        require_text(&case.mode, 64, "conformance mode")?;
        if !finite_values(&case.expected_premultiplied_rgba)
            || case
                .expected_premultiplied_rgba
                .iter()
                .any(|value| !(0.0..=1.0).contains(value))
            || case
                .hit_depth_world
                .is_some_and(|value| !value.is_finite() || value < 0.0)
            || case.pick.as_ref().is_some_and(|pick| {
                !pick.value.is_finite()
                    || !finite_values(&pick.world)
                    || !pick.distance_world.is_finite()
                    || pick.distance_world < 0.0
            })
        {
            bail!("viewer numerical conformance facts must be finite and physically bounded")
        }
        if let Some(pick) = &case.pick {
            require_text(&pick.kind, 64, "conformance pick kind")?;
        }
        if case.authored_order.len() > 64
            || case.source_order.len() > 64
            || case
                .authored_order
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len()
                != case.authored_order.len()
            || case
                .source_order
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len()
                != case.source_order.len()
        {
            bail!("viewer conformance layer orders must be bounded unique ordinal lists")
        }
    }
    if observed_ids != required_ids {
        bail!("viewer oracle must bind the six frozen EP-00 numerical conformance cases")
    }
    let canonical_cases = oracle
        .conformance_cases
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .context("viewer numerical conformance cases could not be canonicalized")?;
    conformance_receipt::validate_oracle_case_commitments(&canonical_cases)?;
    let mut imported_manifest_facts = 0_u8;
    for scenario in &oracle.scenarios {
        for phase in &scenario.phases {
            if let Some(digest) = &phase.expected_imported_root_manifest_sha256 {
                require_sha256(digest, "expected imported root-manifest commitment")?;
                if scenario.id != "IP" {
                    bail!("only the IP oracle may bind an imported root-manifest identity")
                }
                imported_manifest_facts = imported_manifest_facts.saturating_add(1);
            }
        }
    }
    if imported_manifest_facts != 1 {
        bail!("the IP oracle must bind exactly one deterministic imported root-manifest identity")
    }
    Ok(())
}

fn scenario_map<'a, T, F>(scenarios: &'a [T], id: F) -> anyhow::Result<BTreeMap<&'a str, &'a T>>
where
    F: Fn(&'a T) -> &'a str,
{
    if scenarios.len() != REQUIRED_SCENARIOS.len() {
        bail!("viewer bundle must contain exactly ten scenarios")
    }
    let expected = REQUIRED_SCENARIOS.into_iter().collect::<BTreeSet<_>>();
    let mut result = BTreeMap::new();
    for scenario in scenarios {
        let scenario_id = id(scenario);
        if !expected.contains(scenario_id) || result.insert(scenario_id, scenario).is_some() {
            bail!("viewer bundle scenarios must contain RZ,ZB,RO,ST,NO,FC,VM,PT,VV,IP exactly once")
        }
    }
    Ok(result)
}

fn validate_workload_scenario(id: &str, scenario: &WorkloadScenario) -> anyhow::Result<()> {
    let expected_initial_state = match id {
        "RZ" => WorkloadInitialState::ResidentCrossSectionAnd3d,
        "ZB" => WorkloadInitialState::ResidentFallback,
        "RO" | "ST" => WorkloadInitialState::ResidentFourPanel,
        "NO" => WorkloadInitialState::NonresidentFourPanel,
        "FC" => WorkloadInitialState::ApplicationCold,
        "VM" => WorkloadInitialState::Resident3d,
        "PT" => WorkloadInitialState::SettledTimepoint,
        "VV" => WorkloadInitialState::ControlledVerification,
        "IP" => WorkloadInitialState::FreshImport,
        _ => unreachable!("scenario IDs were checked"),
    };
    if scenario.initial_state != expected_initial_state {
        bail!("viewer scenario {id} has the wrong initial-state class")
    }
    validate_phase_names(id, scenario.phases.iter().map(|phase| phase.name.as_str()))?;
    for phase in &scenario.phases {
        require_text(&phase.action, 512, "workload phase action")?;
        require_text(&phase.primary_proof, 512, "workload phase primary proof")?;
    }
    Ok(())
}

fn validate_script_scenario(
    id: &str,
    scenario: &ScriptScenario,
    profile: &ViewerQualificationProfile,
    oracle: &OracleScenario,
) -> anyhow::Result<()> {
    validate_phase_names(id, scenario.phases.iter().map(|phase| phase.name.as_str()))?;
    let mut labels = Vec::new();
    for phase in &scenario.phases {
        if let Some(label) = &phase.start_diagnostic_label {
            require_label(label, "phase start diagnostic label")?;
            labels.push(label.as_str());
        }
        require_label(&phase.end_diagnostic_label, "phase end diagnostic label")?;
        labels.push(&phase.end_diagnostic_label);
    }
    if labels.iter().copied().collect::<BTreeSet<_>>().len() != labels.len() {
        bail!("viewer scenario {id} diagnostic labels must be unique")
    }

    validate_automation_template(id, &scenario.instrumented_script, true, labels.as_slice())?;
    validate_product_gate_inventory(id, &scenario.instrumented_script.commands)?;
    validate_product_gate_schedule(id, scenario, &scenario.instrumented_script, profile, oracle)?;
    validate_hard_safety_limits(&scenario.instrumented_script.hard_safety_limits, profile)?;
    validate_required_action_surface(
        id,
        scenario,
        profile.extents.blocking_qualification,
        &profile.workload.representative_package.root,
    )?;
    let control = scenario
        .instrumentation_control_script
        .as_ref()
        .context("every viewer scenario requires an instrumentation-control script")?;
    validate_automation_template(id, control, false, &[])?;
    validate_product_gate_inventory(id, &control.commands)?;
    validate_product_gate_schedule(id, scenario, control, profile, oracle)?;
    validate_hard_safety_limits(&control.hard_safety_limits, profile)?;
    if normalized_semantic_script(&scenario.instrumented_script)
        != normalized_semantic_script(control)
    {
        bail!("viewer scenario {id} instrumentation control changes semantic actions")
    }
    let first_start_label = scenario
        .phases
        .first()
        .and_then(|phase| phase.start_diagnostic_label.as_deref())
        .context("viewer scenario requires a first phase start checkpoint")?;
    match (
        id,
        scenario.instrumented_script.startup_bootstrap.as_ref(),
        control.startup_bootstrap.as_ref(),
    ) {
        ("FC", Some(instrumented), Some(control))
            if instrumented.capture_start_checkpoint
                && control.capture_start_checkpoint
                && instrumented.start_diagnostic_label.as_deref() == Some(first_start_label)
                && control.start_diagnostic_label.as_deref() == Some(first_start_label) =>
        {
            validate_fc_startup_bootstrap(instrumented, profile.extents.blocking_qualification)?;
            validate_fc_startup_bootstrap(control, profile.extents.blocking_qualification)?;
        }
        ("FC", _, _) => {
            bail!(
                "FC instrumented and control scripts must bind their first phase start to the pre-demand startup bootstrap"
            )
        }
        ("NO" | "VV", Some(instrumented), Some(control))
            if !instrumented.capture_start_checkpoint
                && !control.capture_start_checkpoint
                && instrumented.start_diagnostic_label.is_none()
                && control.start_diagnostic_label.is_none() => {}
        ("NO" | "VV", _, _) => {
            bail!(
                "NO and VV instrumented and control scripts require a setup-only pre-demand bootstrap"
            )
        }
        (_, None, None) => {}
        _ => bail!("only FC, NO, and VV may declare a pre-demand startup bootstrap"),
    }

    match (
        scenario.cleanup.enabled,
        scenario.cleanup.imported_package_relative_path.as_deref(),
    ) {
        (false, None) => {}
        (false, Some(_)) => {
            bail!("disabled viewer cleanup must not name a package")
        }
        (true, Some(path)) => {
            if id != "IP" {
                bail!("only the IP scenario may request attempt-local package cleanup")
            }
            validate_relative_attempt_path(path, "cleanup imported package")?;
        }
        (true, None) => bail!("enabled viewer cleanup must name one attempt-local package"),
    }
    Ok(())
}

fn validate_fc_startup_bootstrap(
    bootstrap: &AutomationStartupBootstrap,
    blocking_extent: super::PixelExtent,
) -> anyhow::Result<()> {
    let viewport_positions = bootstrap
        .commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| {
            (command.get("command").and_then(Value::as_str) == Some("set_four_panel_viewports"))
                .then_some((index, command))
        })
        .collect::<Vec<_>>();
    let camera_fit = bootstrap.commands.iter().position(|command| {
        command.get("command").and_then(Value::as_str) == Some("camera_fit_data")
    });
    let mapped_client = bootstrap.commands.iter().find(|command| {
        command.get("command").and_then(Value::as_str) == Some("set_mapped_client_pixels")
    });
    if viewport_positions.len() != 1
        || bootstrap.commands.iter().any(|command| {
            command.get("command").and_then(Value::as_str) == Some("set_render_target_size")
        })
        || camera_fit.is_none_or(|camera_fit| viewport_positions[0].0 >= camera_fit)
        || mapped_client
            .and_then(|command| command.get("width"))
            .and_then(Value::as_u64)
            != Some(u64::from(blocking_extent.width))
        || mapped_client
            .and_then(|command| command.get("height"))
            .and_then(Value::as_u64)
            != Some(u64::from(blocking_extent.height))
        || viewport_positions[0]
            .1
            .get("three_d_render_width")
            .and_then(Value::as_u64)
            != Some(u64::from(blocking_extent.width))
        || viewport_positions[0]
            .1
            .get("three_d_render_height")
            .and_then(Value::as_u64)
            != Some(u64::from(blocking_extent.height))
    {
        bail!(
            "FC startup bootstrap must install the exact mapped client and all-panel viewport geometry before camera_fit_data"
        )
    }
    Ok(())
}

fn sole_dataset_command_path<'a>(
    commands: &'a [Value],
    command_name: &str,
    label: &str,
) -> anyhow::Result<&'a str> {
    let matches = commands
        .iter()
        .filter(|command| command.get("command").and_then(Value::as_str) == Some(command_name))
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        bail!("{label} must appear exactly once")
    }
    let object = matches[0]
        .as_object()
        .with_context(|| format!("{label} must be a JSON object"))?;
    if object.len() != 2 || !object.contains_key("command") || !object.contains_key("path") {
        bail!("{label} must contain exactly command and path")
    }
    object
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .with_context(|| format!("{label} must have a nonempty string path"))
}

fn validate_import_source_binding(binding: &ImportSourceBinding) -> anyhow::Result<()> {
    require_sha256(
        &binding.inventory_sha256,
        "workload import-source inventory commitment",
    )?;
    require_sha256(
        &binding.reviewed_source_fingerprint_sha256,
        "workload reviewed import-source fingerprint commitment",
    )?;
    if binding.regular_files == 0 || binding.regular_files > 4_096 || binding.source_bytes == 0 {
        bail!("workload import-source inventory facts are empty or outside their fixed bound")
    }
    Ok(())
}

fn sole_ip_source_path(commands: &[Value]) -> anyhow::Result<&Path> {
    let matches = commands
        .iter()
        .filter(|command| {
            command.get("command").and_then(Value::as_str) == Some("begin_tiff_import_setup")
        })
        .collect::<Vec<_>>();
    let [command] = matches.as_slice() else {
        bail!("IP TIFF setup command must appear exactly once")
    };
    command
        .get("source")
        .and_then(Value::as_str)
        .filter(|source| !source.is_empty())
        .map(Path::new)
        .context("IP TIFF setup command must bind one nonempty source path")
}

fn validate_import_source_path(source: &Path, repository_root: &Path) -> anyhow::Result<()> {
    if !source.is_absolute()
        || source
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!("IP import source must be a normalized absolute path")
    }
    require_nonsymlink_components(source, "IP import source")?;
    let canonical = fs::canonicalize(source).context("IP import source is unavailable")?;
    if canonical != source {
        bail!("IP import source path must be fully resolved")
    }
    if canonical.starts_with(repository_root) {
        bail!("IP import source must be outside the repository")
    }
    let metadata = fs::symlink_metadata(&canonical)
        .context("IP import source is unavailable or unreadable")?;
    if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
        bail!("IP import source must be a nonsymlink regular file or directory")
    }
    Ok(())
}

fn source_verification_wait_indices(commands: &[Value]) -> impl Iterator<Item = usize> + '_ {
    commands.iter().enumerate().filter_map(|(index, command)| {
        (command.get("command").and_then(Value::as_str) == Some("wait_for")
            && command.get("condition").and_then(Value::as_str)
                == Some("source_verification_verified"))
        .then_some(index)
    })
}

fn source_verification_inactive_wait_indices(
    commands: &[Value],
) -> impl Iterator<Item = usize> + '_ {
    commands.iter().enumerate().filter_map(|(index, command)| {
        (command.get("command").and_then(Value::as_str) == Some("wait_for")
            && command.get("condition").and_then(Value::as_str)
                == Some("source_verification_inactive"))
        .then_some(index)
    })
}

fn validate_source_verification_isolation_contract(
    id: &str,
    commands: &[Value],
) -> anyhow::Result<()> {
    let inactive_waits = source_verification_inactive_wait_indices(commands).collect::<Vec<_>>();
    let cancel_indices = commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| {
            (command.get("command").and_then(Value::as_str)
                == Some("cancel_active_source_verification"))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    let expected_count = usize::from(id == "PT") + 1;
    if inactive_waits.len() != expected_count || cancel_indices.len() != expected_count {
        bail!(
            "viewer scenario {id} must quiesce automatic source verification exactly {expected_count} time(s)"
        )
    }
    for (&cancel, &wait) in cancel_indices.iter().zip(&inactive_waits) {
        if cancel.checked_add(1) != Some(wait) {
            bail!(
                "viewer scenario {id} must wait for verifier inactivity immediately after requesting active-verifier cancellation"
            )
        }
    }
    if id != "FC" {
        let first_measurement_boundary = commands
            .iter()
            .position(|command| {
                matches!(
                    command.get("command").and_then(Value::as_str),
                    Some("observe_gate_batch" | "sample_diagnostics")
                )
            })
            .context("viewer scenario must contain a measurement boundary")?;
        if inactive_waits
            .last()
            .is_none_or(|wait| *wait >= first_measurement_boundary)
        {
            bail!(
                "viewer scenario {id} must quiesce automatic source verification before its first measurement boundary"
            )
        }
    }
    if id == "VV" {
        if source_verification_wait_indices(commands).next().is_some() {
            bail!("VV must not perform an unmeasured full source verification during setup")
        }
        return Ok(());
    }
    if commands.iter().any(|command| {
        matches!(
            (
                command.get("command").and_then(Value::as_str),
                command.get("condition").and_then(Value::as_str)
            ),
            (Some("wait_for"), Some("source_verification_verified"))
                | (Some("wait_for"), Some("source_verification_required"))
                | (Some("request_source_verification"), _)
                | (Some("cancel_source_verification"), _)
        )
    }) {
        bail!("only VV may serialize a scenario through a full source-verification lifecycle")
    }
    Ok(())
}

fn sole_command_index(
    commands: &[Value],
    command_name: &str,
    label: &str,
) -> anyhow::Result<usize> {
    let matches = commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| {
            (command.get("command").and_then(Value::as_str) == Some(command_name)).then_some(index)
        })
        .collect::<Vec<_>>();
    let [index] = matches.as_slice() else {
        bail!("{label} must appear exactly once")
    };
    Ok(*index)
}

fn validate_ip_action_contract(scenario: &ScriptScenario) -> anyhow::Result<()> {
    let commands = &scenario.instrumented_script.commands;
    let [phase] = scenario.phases.as_slice() else {
        bail!("IP must declare exactly one preprocessing phase")
    };
    let start_label = phase
        .start_diagnostic_label
        .as_deref()
        .context("IP preprocessing phase must declare a start checkpoint")?;
    let start_checkpoint = diagnostic_command_index(commands, start_label)
        .context("IP preprocessing start checkpoint is missing from the command stream")?;
    let end_checkpoint = diagnostic_command_index(commands, &phase.end_diagnostic_label)
        .context("IP preprocessing end checkpoint is missing from the command stream")?;
    let begin_setup =
        sole_command_index(commands, "begin_tiff_import_setup", "IP TIFF setup command")?;
    let start_import = sole_command_index(
        commands,
        "start_reviewed_import",
        "IP reviewed import command",
    )?;
    let gate_batch = sole_command_index(commands, "observe_gate_batch", "IP gate batch")?;
    if commands.iter().any(|command| {
        command.get("command").and_then(Value::as_str) == Some("wait_for_imported_open_ready")
    }) {
        bail!("IP must not retain the legacy fatal imported open-ready wait")
    }
    let observations = expected_product_gate_observations(commands)?;
    if observations
        .iter()
        .map(|observation| observation.condition)
        .collect::<BTreeSet<_>>()
        != BTreeSet::from(["import_idle", "runtime_idle", IMPORTED_OPEN_READY_CONDITION])
        || observations.len() != 3
    {
        bail!("IP gate batch must contain the exact three import-primary observations")
    }
    if !(start_checkpoint < begin_setup
        && begin_setup < start_import
        && start_import < gate_batch
        && gate_batch < end_checkpoint)
    {
        bail!(
            "IP must batch import-idle, open-ready, and runtime-idle immediately after import start and before its end checkpoint"
        )
    }
    Ok(())
}

fn validate_ip_attempt_path_binding(scenario: &ScriptScenario) -> anyhow::Result<()> {
    let commands = &scenario.instrumented_script.commands;
    let setup_index =
        sole_command_index(commands, "begin_tiff_import_setup", "IP TIFF setup command")?;
    let output_parent = commands[setup_index]
        .get("output_parent")
        .and_then(Value::as_str)
        .context("IP TIFF setup output parent is unavailable")?;
    let output_parent = attempt_root_relative_path(output_parent, "IP output parent")?;
    let open_ready_paths = commands
        .iter()
        .filter(|command| {
            command.get("command").and_then(Value::as_str) == Some("observe_gate_batch")
        })
        .flat_map(|command| {
            command
                .get("observations")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|observation| {
            (observation.pointer("/target/kind").and_then(Value::as_str)
                == Some("imported_open_ready"))
            .then(|| observation.pointer("/target/path").and_then(Value::as_str))
            .flatten()
        })
        .collect::<Vec<_>>();
    let [open_ready_path] = open_ready_paths.as_slice() else {
        bail!("IP must bind exactly one imported-open-ready target path")
    };
    let open_ready_path = attempt_root_relative_path(open_ready_path, "IP open-ready target")?;
    let cleanup_path = scenario
        .cleanup
        .enabled
        .then_some(scenario.cleanup.imported_package_relative_path.as_deref())
        .flatten()
        .context("IP requires one enabled attempt-local package cleanup target")?;
    validate_relative_attempt_path(cleanup_path, "IP cleanup package")?;
    if open_ready_path != cleanup_path || open_ready_path.parent() != Some(output_parent.as_path())
    {
        bail!("IP output parent, open-ready target, and cleanup target do not cross-bind")
    }
    Ok(())
}

fn attempt_root_relative_path(value: &str, label: &str) -> anyhow::Result<PathBuf> {
    let suffix = value
        .strip_prefix(ATTEMPT_ROOT_PLACEHOLDER)
        .and_then(|suffix| suffix.strip_prefix('/'))
        .with_context(|| format!("{label} must be rooted in the runner-owned attempt directory"))?;
    let relative = PathBuf::from(suffix);
    validate_relative_attempt_path(&relative, label)?;
    Ok(relative)
}

fn validate_dataset_action_contract(
    id: &str,
    scenario: &ScriptScenario,
    representative_package_root: &Path,
) -> anyhow::Result<Option<PathBuf>> {
    let commands = &scenario.instrumented_script.commands;
    let open_indices = commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| {
            (command.get("command").and_then(Value::as_str) == Some("open_dataset"))
                .then_some(index)
        })
        .collect::<Vec<_>>();
    if open_indices.as_slice() != [0] {
        bail!(
            "viewer scenario {id} must contain exactly one open_dataset startup assertion at command index zero"
        )
    }
    let representative_path = representative_package_root
        .to_str()
        .context("representative package path must be valid UTF-8 for script binding")?;
    if sole_dataset_command_path(commands, "open_dataset", "representative open_dataset")?
        != representative_path
    {
        bail!("viewer scenario {id} open_dataset must bind the representative package path")
    }

    let switch_indices = commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| {
            (command.get("command").and_then(Value::as_str) == Some("switch_dataset"))
                .then_some(index)
        })
        .collect::<Vec<_>>();
    if id != "PT" {
        if !switch_indices.is_empty() {
            bail!("only PT may contain switch_dataset")
        }
        return Ok(None);
    }
    let [switch_index] = switch_indices.as_slice() else {
        bail!("PT must contain exactly one switch_dataset")
    };
    let target = sole_dataset_command_path(commands, "switch_dataset", "PT switch_dataset")?;
    let target_path = Path::new(target);
    if !target_path.is_absolute()
        || target_path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!("PT switch_dataset must bind a normalized absolute package path")
    }
    if target_path == representative_package_root {
        bail!("PT switch_dataset target must differ from the representative package")
    }
    if !source_verification_inactive_wait_indices(commands).any(|index| index < *switch_index) {
        bail!("PT must quiesce the representative source verifier before switch_dataset")
    }
    let first_checkpoint = scenario
        .phases
        .first()
        .and_then(|phase| phase.start_diagnostic_label.as_deref())
        .and_then(|label| diagnostic_command_index(commands, label))
        .context("PT first phase start checkpoint is missing from the command stream")?;
    if !source_verification_inactive_wait_indices(commands)
        .any(|index| *switch_index < index && index < first_checkpoint)
    {
        bail!(
            "PT must quiesce the successor source verifier after switch_dataset and before its first diagnostic checkpoint"
        )
    }
    Ok(Some(target_path.to_path_buf()))
}

fn validate_supporting_temporal_package(
    package_root: &str,
    expected_root_manifest_sha256: &str,
    repository_root: &Path,
) -> anyhow::Result<()> {
    let package_root = Path::new(package_root);
    require_nonsymlink_components(package_root, "supporting temporal package")?;
    let canonical = fs::canonicalize(package_root)
        .context("supporting temporal package is unavailable or unreadable")?;
    if canonical != package_root {
        bail!("supporting temporal package path must be fully resolved")
    }
    let repository_root = fs::canonicalize(repository_root)
        .context("repository root is unavailable while validating temporal package")?;
    if canonical.starts_with(repository_root) {
        bail!("supporting temporal package must be outside the repository")
    }
    let metadata = fs::symlink_metadata(&canonical)
        .context("supporting temporal package is unavailable or unreadable")?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("supporting temporal package must be a nonsymlink directory")
    }
    let manifest = canonical.join("m4d/manifest/root.json");
    require_nonsymlink_components(&manifest, "supporting temporal package root manifest")?;
    let bytes = read_bounded_regular_file(
        &manifest,
        SUPPORTING_PACKAGE_ROOT_MANIFEST_MAX_BYTES,
        "supporting temporal package root manifest",
    )?;
    let observed = Sha256Hasher::digest(&bytes).to_string();
    if observed != expected_root_manifest_sha256 {
        bail!("supporting temporal package root manifest does not match the workload commitment")
    }
    Ok(())
}

fn validate_required_action_surface(
    id: &str,
    scenario: &ScriptScenario,
    blocking_extent: super::PixelExtent,
    representative_package_root: &Path,
) -> anyhow::Result<()> {
    let commands = &scenario.instrumented_script.commands;
    validate_dataset_action_contract(id, scenario, representative_package_root)?;
    validate_source_verification_isolation_contract(id, commands)?;
    let names = commands
        .iter()
        .filter_map(|command| command.get("command").and_then(Value::as_str))
        .collect::<Vec<_>>();
    if !template_has_extent(&scenario.instrumented_script, blocking_extent) {
        bail!("viewer scenario {id} must exercise the profile-bound blocking extent")
    }
    let require = |command: &str| -> anyhow::Result<()> {
        if !names.contains(&command) {
            bail!("viewer scenario {id} lacks required semantic action {command}")
        }
        Ok(())
    };
    match id {
        "RZ" => {
            require("camera_zoom_sequence")?;
            require("cross_section_zoom_sequence")?;
            for command in commands.iter().filter(|command| {
                matches!(
                    command.get("command").and_then(Value::as_str),
                    Some("camera_zoom_sequence" | "cross_section_zoom_sequence")
                )
            }) {
                if command.get("duration_ms").and_then(Value::as_u64) != Some(2_000)
                    || command
                        .get("samples")
                        .and_then(Value::as_u64)
                        .is_none_or(|samples| samples < 120)
                {
                    bail!("RZ zoom sequences must bind two seconds and at least 120 samples")
                }
            }
        }
        "ZB" => {
            require("camera_zoom_sequence")?;
            if scenario.phases.len() < 2 {
                bail!("ZB must declare separate resident and nonresident boundary phases")
            }
        }
        "RO" => {
            require("cross_section_rotate_sequence")?;
            let compound = commands.iter().any(|command| {
                command.get("command").and_then(Value::as_str)
                    == Some("cross_section_rotate_sequence")
                    && command
                        .get("x_points_per_sample")
                        .and_then(Value::as_f64)
                        .is_some_and(|value| value != 0.0)
                    && command
                        .get("y_points_per_sample")
                        .and_then(Value::as_f64)
                        .is_some_and(|value| value != 0.0)
            });
            if !compound {
                bail!("RO must contain a compound-angle cross-section rotation")
            }
        }
        "ST" => require("cross_section_slice_sequence")?,
        "NO" => {
            require("cross_section_rotate_sequence")?;
            require("cross_section_pan_sequence")?;
        }
        "FC" => {
            if !scenario
                .instrumented_script
                .startup_bootstrap
                .as_ref()
                .into_iter()
                .flat_map(|bootstrap| &bootstrap.commands)
                .chain(commands)
                .any(|command| {
                    command.get("command").and_then(Value::as_str) == Some("set_viewer_layout")
                        && command.get("layout").and_then(Value::as_str) == Some("four_panel")
                })
            {
                bail!("FC must open the fixed four-panel product layout")
            }
        }
        "VM" => {
            let modes = commands
                .iter()
                .filter_map(|command| {
                    matches!(
                        command.get("command").and_then(Value::as_str),
                        Some("set_render_mode" | "set_layer_render_mode")
                    )
                    .then(|| command.get("mode").and_then(Value::as_str))
                    .flatten()
                })
                .collect::<BTreeSet<_>>();
            if modes != BTreeSet::from(["mip", "dvr", "iso"]) {
                bail!("VM must exercise MIP, DVR, and ISO exactly as named modes")
            }
        }
        "PT" => {
            require("set_projection")?;
            if names
                .iter()
                .filter(|name| **name == "set_time_index")
                .count()
                < 2
            {
                bail!("PT must advance and return through two time-index actions")
            }
        }
        "VV" => {
            require("wait_for")?;
            require("cancel_source_verification")?;
            require("request_source_verification")?;
            if names
                .iter()
                .filter(|name| **name == "cancel_source_verification")
                .count()
                != 1
                || names
                    .iter()
                    .filter(|name| **name == "request_source_verification")
                    .count()
                    != 1
            {
                bail!("VV must contain exactly one verifier reset/cancel and one measured request")
            }
            let cancel = commands
                .iter()
                .position(|command| {
                    command.get("command").and_then(Value::as_str)
                        == Some("cancel_source_verification")
                })
                .expect("required VV cancellation command exists");
            let startup_quiescence = source_verification_inactive_wait_indices(commands)
                .next()
                .context("VV startup verifier quiescence is unavailable")?;
            let request = commands
                .iter()
                .position(|command| {
                    command.get("command").and_then(Value::as_str)
                        == Some("request_source_verification")
                })
                .expect("required VV verification request exists");
            let completion_wait = verification_completion_observation_index(commands)
                .context("VV must observe verification completion after its measured request")?;
            let first_start = scenario
                .phases
                .first()
                .and_then(|phase| phase.start_diagnostic_label.as_deref())
                .and_then(|label| diagnostic_command_index(commands, label))
                .context("VV first phase start checkpoint is missing from the command stream")?;
            if startup_quiescence >= cancel
                || cancel >= request
                || request >= first_start
                || first_start >= completion_wait
            {
                bail!(
                    "VV must quiesce automatic verification, reset/cancel its setup verifier, request the measured verifier, then sample the first active phase"
                )
            }
        }
        "IP" => {
            require("begin_tiff_import_setup")?;
            require("start_reviewed_import")?;
            require("observe_gate_batch")?;
            validate_ip_action_contract(scenario)?;
            validate_ip_attempt_path_binding(scenario)?;
        }
        _ => unreachable!("scenario IDs were validated"),
    }
    Ok(())
}

fn has_extent_command(commands: &[Value], extent: super::PixelExtent) -> bool {
    commands.iter().any(|command| {
        command.get("command").and_then(Value::as_str) == Some("set_render_target_size")
            && command.get("width").and_then(Value::as_u64) == Some(u64::from(extent.width))
            && command.get("height").and_then(Value::as_u64) == Some(u64::from(extent.height))
    })
}

fn template_has_extent(script: &AutomationScriptTemplate, extent: super::PixelExtent) -> bool {
    has_extent_command(&script.commands, extent)
        || script.startup_bootstrap.as_ref().is_some_and(|bootstrap| {
            has_extent_command(&bootstrap.commands, extent)
                || bootstrap.commands.iter().any(|command| {
                    command.get("command").and_then(Value::as_str)
                        == Some("set_four_panel_viewports")
                        && command.get("three_d_render_width").and_then(Value::as_u64)
                            == Some(u64::from(extent.width))
                        && command.get("three_d_render_height").and_then(Value::as_u64)
                            == Some(u64::from(extent.height))
                })
        })
}

fn validate_hard_safety_limits(
    limits: &AutomationHardSafetyLimits,
    profile: &ViewerQualificationProfile,
) -> anyhow::Result<()> {
    if *limits != expected_hard_safety_limits(profile)? {
        bail!("viewer automation hard safety limits differ from the exact profile-derived caps")
    }
    Ok(())
}

fn expected_hard_safety_limits(
    profile: &ViewerQualificationProfile,
) -> anyhow::Result<AutomationHardSafetyLimits> {
    let max_runtime_queued_requests = profile
        .resources
        .max_queued_requests
        .checked_mul(2)
        .context("viewer hard runtime queue cap overflows u64")?;
    Ok(AutomationHardSafetyLimits {
        max_cpu_total_bytes: Some(profile.resources.max_cpu_total_bytes),
        max_cpu_decoded_residency_bytes: Some(profile.resources.max_cpu_total_bytes),
        max_cpu_upload_staging_bytes: Some(profile.resources.max_cpu_total_bytes),
        max_runtime_queued_requests: Some(max_runtime_queued_requests),
        ..AutomationHardSafetyLimits::default()
    })
}

fn validate_oracle_scenario(id: &str, scenario: &OracleScenario) -> anyhow::Result<()> {
    validate_phase_names(id, scenario.phases.iter().map(|phase| phase.name.as_str()))?;
    let mut active_verification_phases = 0_usize;
    let mut completed_verification_phases = 0_usize;
    for (phase_index, phase) in scenario.phases.iter().enumerate() {
        validate_phase_state(&phase.phase_state)?;
        validate_required_phase_gate_matrix(id, phase)?;
        let mut expected_layers = BTreeSet::new();
        for expected in &phase.expected_cross_section_layers {
            if !expected_layers.insert((expected.panel, expected.layer_ordinal)) {
                bail!("viewer oracle phase has duplicate cross-section layer expectations")
            }
        }
        let unique_counters = phase
            .zero_work_counters
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        if unique_counters.len() != phase.zero_work_counters.len() {
            bail!("viewer oracle phase has duplicate zero-work counters")
        }
        if phase.minimum_exact_useful_sample_bytes == Some(0) {
            bail!("minimum exact useful sample bytes must be positive when required")
        }
        validate_unique_work_expectation(&phase.unique_work)?;
        if let Some(gate) = &phase.verification_gate {
            if id != "VV" {
                bail!("only VV phases may bind source-verification contention evidence")
            }
            validate_verification_gate(gate)?;
            active_verification_phases +=
                usize::from(gate.kind == VerificationGateKind::ActiveThroughout);
            completed_verification_phases +=
                usize::from(gate.kind == VerificationGateKind::Completes);
        } else if id == "VV" {
            bail!("every VV phase must bind source-verification state and service evidence")
        }
        match (id, phase.name.as_str(), phase.settlement_gate) {
            ("FC", "blocking_target_settled", Some(SettlementGate::ColdTarget))
            | ("FC", "exercise_extent_settled", None) => {}
            ("FC", _, _) => {
                bail!(
                    "FC must prove first-useful, complete-coarse, and target milestones together at its blocking final checkpoint; exercise has no cold clock"
                )
            }
            _ => {}
        }
        if id != "FC" {
            let expected_settlement = (phase.structural_gate.kind
                == StructuralGateKind::NonresidentOverlap)
                .then_some(SettlementGate::NonresidentTarget);
            if phase.settlement_gate != expected_settlement {
                bail!("only nonresident phases may use the nonresident target-settlement gate")
            }
        }
        match (
            phase.structural_gate.kind == StructuralGateKind::NonresidentOverlap,
            phase.phase_start_target_residency.as_ref(),
        ) {
            (true, Some(residency)) => {
                validate_phase_start_target_residency(residency, &phase.unique_work.target_union)?;
                if id == "ZB"
                    && (residency.nonresident_target_difference.unique_keys == 0
                        || residency.nonresident_target_difference.unique_payload_bytes == 0)
                {
                    bail!(
                        "ZB nonresident boundary phase must prove a nonempty target difference against phase-start GPU residency"
                    )
                }
            }
            (true, None) => {
                bail!("every nonresident phase must bind exact phase-start target residency")
            }
            (false, Some(_)) => {
                bail!("only nonresident phases may bind phase-start target residency")
            }
            (false, None) => {}
        }
        if let Some(baseline) = &phase.unique_work.residency_baseline {
            if id != "ZB" || phase.structural_gate.kind != StructuralGateKind::NonresidentOverlap {
                bail!("only the ZB nonresident boundary phase may bind a prior residency baseline")
            }
            let Some(baseline_phase) = scenario.phases[..phase_index].iter().find(|candidate| {
                candidate.phase_state.checkpoint_label == baseline.checkpoint_label
            }) else {
                bail!(
                    "ZB nonresident residency baseline must name an earlier oracle phase checkpoint"
                )
            };
            if !baseline_phase.require_current_complete
                || baseline_phase.unique_work.target_union != baseline.union
            {
                bail!(
                    "ZB nonresident residency baseline must be the exact earlier complete target cohort"
                )
            }
        }
        validate_structural_gate(id, &phase.name, phase)?;
    }
    if id == "VV" && (active_verification_phases == 0 || completed_verification_phases != 1) {
        bail!(
            "VV must prove at least one active-throughout contention phase and exactly one completed verification phase"
        )
    }
    Ok(())
}

fn validate_required_phase_gate_matrix(id: &str, phase: &OraclePhase) -> anyhow::Result<()> {
    let is_viewer_phase = id != "IP";
    let interaction_required = !matches!(id, "FC" | "IP");
    if phase.require_interaction_metrics != interaction_required {
        bail!(
            "viewer scenario {id} phase {:?} has the wrong mandatory interaction-metric gate",
            phase.name
        )
    }
    if phase.require_current_complete != is_viewer_phase
        || phase.require_coordinated_layout_complete != is_viewer_phase
    {
        bail!(
            "viewer scenario {id} phase {:?} has the wrong mandatory current/complete-layout gate",
            phase.name
        )
    }
    if phase.expected_scale_level.is_some() != is_viewer_phase {
        bail!(
            "viewer scenario {id} phase {:?} has the wrong mandatory target-scale gate",
            phase.name
        )
    }
    if is_viewer_phase && !phase.phase_state.layers.iter().any(|layer| layer.visible) {
        bail!(
            "viewer scenario {id} phase {:?} must bind at least one visible layer",
            phase.name
        )
    }

    let expected_cross_section_layers =
        if is_viewer_phase && phase.phase_state.layout == ExpectedViewerLayout::FourPanel {
            let mut expected = BTreeSet::new();
            for panel in [
                CrossSectionPanel::Xy,
                CrossSectionPanel::Xz,
                CrossSectionPanel::Yz,
            ] {
                for layer in phase
                    .phase_state
                    .layers
                    .iter()
                    .filter(|layer| layer.visible)
                {
                    expected.insert((
                        panel,
                        usize::try_from(layer.layer_ordinal)
                            .expect("a u32 layer ordinal fits the target usize"),
                        layer.scale_level,
                    ));
                }
            }
            expected
        } else {
            BTreeSet::new()
        };
    let observed_cross_section_layers = phase
        .expected_cross_section_layers
        .iter()
        .map(|layer| (layer.panel, layer.layer_ordinal, layer.scale_level))
        .collect::<BTreeSet<_>>();
    if observed_cross_section_layers != expected_cross_section_layers
        || phase.expected_cross_section_layers.len() != expected_cross_section_layers.len()
    {
        bail!(
            "viewer scenario {id} phase {:?} must bind every visible cross-section layer scale exactly once",
            phase.name
        )
    }

    let fixed_gpu_gate = fixed_required_gpu_gate(id, phase);
    let gpu_gate_matches = if !is_viewer_phase {
        phase.gpu_gate.is_none()
    } else if let Some(expected) = fixed_gpu_gate {
        phase.gpu_gate == Some(expected)
    } else {
        matches!(
            phase.gpu_gate,
            Some(GpuGate::Mip | GpuGate::Dvr | GpuGate::Iso)
        )
    };
    if !gpu_gate_matches {
        bail!(
            "viewer scenario {id} phase {:?} has the wrong mandatory active-view GPU gate",
            phase.name
        )
    }

    let expected_settlement_gate = match (id, phase.name.as_str()) {
        ("FC", "blocking_target_settled") => Some(SettlementGate::ColdTarget),
        ("ZB", "nonresident_boundary_crossing")
        | ("NO", "nonresident_rotation_pan")
        | ("VV", "verification_complete_nonresident") => Some(SettlementGate::NonresidentTarget),
        _ => None,
    };
    if phase.settlement_gate != expected_settlement_gate {
        bail!(
            "viewer scenario {id} phase {:?} has the wrong mandatory settlement gate",
            phase.name
        )
    }

    let expected_verification_kind = match (id, phase.name.as_str()) {
        ("VV", "verification_active_resident") => Some(VerificationGateKind::ActiveThroughout),
        ("VV", "verification_complete_nonresident") => Some(VerificationGateKind::Completes),
        _ => None,
    };
    if phase.verification_gate.as_ref().map(|gate| gate.kind) != expected_verification_kind {
        bail!(
            "viewer scenario {id} phase {:?} has the wrong mandatory verification gate",
            phase.name
        )
    }
    if phase.expected_imported_root_manifest_sha256.is_some() != (id == "IP") {
        bail!(
            "viewer scenario {id} phase {:?} has the wrong deterministic import-identity gate",
            phase.name
        )
    }
    if phase.import_gate.is_some() != (id == "IP") {
        bail!(
            "viewer scenario {id} phase {:?} has the wrong mandatory import-workflow gate",
            phase.name
        )
    }
    if let Some(gate) = &phase.import_gate {
        validate_import_gate_contract(gate)?;
    }
    if phase.minimum_exact_useful_sample_bytes.is_some() != is_viewer_phase {
        bail!(
            "viewer scenario {id} phase {:?} has the wrong exact-useful-byte gate",
            phase.name
        )
    }
    Ok(())
}

fn validate_import_gate_contract(gate: &ImportGate) -> anyhow::Result<()> {
    let worker_stages = validate_import_stage_names(
        &gate.required_worker_stage_names,
        "IP required worker stage",
    )?;
    validate_import_stage_names(
        &gate.required_projected_stage_names,
        "IP required projected stage",
    )?;
    validate_import_stage_names(
        &gate.required_receipt_stage_names,
        "IP required receipt stage",
    )?;
    if gate.required_progress.is_empty() || gate.required_progress.len() > 64 {
        bail!("IP import gate must bind between one and 64 progress stages")
    }
    let mut progress_stages = BTreeSet::new();
    for progress in &gate.required_progress {
        require_text(&progress.stage, 128, "IP required progress stage")?;
        if progress.minimum_completed_work_units == 0
            || !progress_stages.insert(progress.stage.as_str())
        {
            bail!("IP import gate progress stages must be unique and require positive work")
        }
        if !worker_stages.contains(progress.stage.as_str()) {
            bail!("IP progress stages must be present in the worker-stage authority")
        }
    }

    let expected = &gate.expected;
    if expected.successful_runs != 1
        || expected.published_events != 1
        || expected.failed_runs != 0
        || expected.cancelled_runs != 0
        || expected.resumed_work_units != 0
        || expected.checkpoint_pending_work_units != 0
        || expected.produced_work_units == 0
        || expected.checkpoint_durable_work_units == 0
        || expected.peak_checkpoint_regular_files == 0
        || expected.minimum_progress_updates == 0
    {
        bail!(
            "IP import gate must bind one success/publication, zero failure/cancellation/resume/pending work, and positive completed work"
        )
    }
    let reconciled_object_reads = expected
        .staged_structure_object_reads
        .checked_add(expected.staged_exact_object_reads)
        .and_then(|value| value.checked_add(expected.scientific_object_reads));
    let reconciled_checkpoint_work = expected
        .produced_work_units
        .checked_add(expected.resumed_work_units);
    if reconciled_object_reads != Some(expected.object_reads)
        || expected.scientific_payload_object_reads > expected.scientific_object_reads
        || reconciled_checkpoint_work != Some(expected.checkpoint_durable_work_units)
    {
        bail!("IP import gate object-read or checkpoint expectations do not reconcile")
    }

    let limits = &gate.limits;
    if limits.maximum_peak_working_bytes == 0
        || limits.maximum_peak_process_rss_bytes == 0
        || limits.maximum_product_peak_open_file_descriptors == 0
        || limits.maximum_open_file_descriptor_structural_bound == 0
        || limits.maximum_open_file_descriptor_structural_bound
            >= limits.maximum_product_peak_open_file_descriptors
        || limits.maximum_preflight_temporary_bytes_bound == 0
        || limits.maximum_peak_temporary_bytes == 0
        || limits.maximum_peak_temporary_bytes > limits.maximum_preflight_temporary_bytes_bound
        || limits.maximum_sync_calls == 0
        || limits.maximum_app_primary_wall_time_ns == 0
        || limits.maximum_app_primary_cpu_time_ns == 0
        || limits.maximum_publication_to_open_ready_wall_time_ns == 0
        || limits.maximum_publication_to_open_ready_cpu_time_ns == 0
        || limits.maximum_receipt_primary_wall_time_ns == 0
        || limits.maximum_receipt_primary_cpu_time_ns == 0
        || limits.maximum_source_read_amplification_numerator == 0
        || limits.maximum_source_read_amplification_denominator == 0
    {
        bail!("IP import gate limits must be positive and internally bounded")
    }

    let currentness = &gate.publication_currentness;
    require_text(
        &currentness.contract_id,
        128,
        "IP publication-currentness contract ID",
    )?;
    let reconciled_currentness = currentness
        .first_inventory_object_reads
        .checked_add(currentness.observed_snapshot_object_reads)
        .and_then(|value| value.checked_add(currentness.second_inventory_object_reads));
    if currentness.expected_snapshot_object_reads == 0
        || currentness.first_inventory_object_reads == 0
        || currentness.first_inventory_object_reads != currentness.second_inventory_object_reads
        || currentness.observed_snapshot_object_reads != currentness.expected_snapshot_object_reads
        || reconciled_currentness != Some(currentness.observed_total_object_reads)
        || currentness.observed_codec_decode_calls != 0
    {
        bail!("IP publication-currentness expectations do not reconcile")
    }
    Ok(())
}

fn validate_import_stage_names<'a>(
    names: &'a [String],
    label: &str,
) -> anyhow::Result<BTreeSet<&'a str>> {
    if names.is_empty() || names.len() > 64 {
        bail!("{label} authority must contain between one and 64 names")
    }
    let mut unique = BTreeSet::new();
    for name in names {
        require_text(name, 128, label)?;
        if !unique.insert(name.as_str()) {
            bail!("{label} authority contains duplicate names")
        }
    }
    Ok(unique)
}

fn fixed_required_gpu_gate(id: &str, phase: &OraclePhase) -> Option<GpuGate> {
    let exact = match (id, phase.name.as_str()) {
        ("RZ", "resident_cross_section_zoom")
        | ("RO", "resident_compound_plane_rotation")
        | ("ST", "resident_axis_slice_translation")
        | ("NO", "nonresident_rotation_pan") => Some(GpuGate::Plane),
        ("VM", "resident_mip" | "exercise_mip") => Some(GpuGate::Mip),
        ("VM", "resident_dvr") => Some(GpuGate::Dvr),
        ("VM", "resident_iso") => Some(GpuGate::Iso),
        _ => None,
    };
    if let Some(exact) = exact {
        return Some(exact);
    }
    if phase.phase_state.active_view != ViewerPanel::ThreeD {
        return Some(GpuGate::Plane);
    }
    None
}

fn validate_phase_start_target_residency(
    expected: &PhaseStartTargetResidencyExpectation,
    target: &ExactResourceUnion,
) -> anyhow::Result<()> {
    for (label, partition) in [
        (
            "resident intersection",
            &expected.resident_target_intersection,
        ),
        (
            "nonresident difference",
            &expected.nonresident_target_difference,
        ),
    ] {
        require_sha256(
            &partition.canonical_entries_sha256,
            &format!("oracle phase-start target-residency {label} digest"),
        )?;
    }
    if expected
        .resident_target_intersection
        .unique_keys
        .checked_add(expected.nonresident_target_difference.unique_keys)
        != Some(target.unique_keys)
        || expected
            .resident_target_intersection
            .unique_payload_bytes
            .checked_add(expected.nonresident_target_difference.unique_payload_bytes)
            != Some(target.unique_payload_bytes)
    {
        bail!(
            "oracle phase-start target-residency partitions must reconcile exactly to the target union"
        )
    }
    Ok(())
}

fn validate_verification_gate(gate: &VerificationGate) -> anyhow::Result<()> {
    match gate.kind {
        VerificationGateKind::ActiveThroughout => {
            if gate.minimum_accepted_progress_updates_delta != 0
                || gate.start.state != ExpectedSourceVerificationState::Verifying
                || gate.end.state != ExpectedSourceVerificationState::Verifying
                || !gate.start.active_operation
                || !gate.end.active_operation
                || gate.start.started_runs != gate.end.started_runs
                || gate.start.cancelled_runs != gate.end.cancelled_runs
                || gate.start.failed_runs != gate.end.failed_runs
                || gate.start.accepted_successes != gate.end.accepted_successes
                || gate.start.completed_reader_runs != gate.end.completed_reader_runs
                || gate.completed_reader_work.is_some()
            {
                bail!(
                    "VV active-throughout gate must keep one verifier active without requiring progress or a terminal reader outcome"
                )
            }
        }
        VerificationGateKind::Completes => {
            if gate.minimum_accepted_progress_updates_delta == 0
                || gate.start.state != ExpectedSourceVerificationState::Verifying
                || gate.end.state != ExpectedSourceVerificationState::Verified
                || !gate.start.active_operation
                || gate.end.active_operation
                || gate.start.started_runs != gate.end.started_runs
                || gate.start.cancelled_runs != gate.end.cancelled_runs
                || gate.start.failed_runs != gate.end.failed_runs
                || gate.start.accepted_successes.checked_add(1) != Some(gate.end.accepted_successes)
                || gate.start.completed_reader_runs.checked_add(1)
                    != Some(gate.end.completed_reader_runs)
            {
                bail!(
                    "VV completion gate must bind progress and exactly one successful completed separate-reader verification"
                )
            }
            let reader = gate.completed_reader_work.as_ref().context(
                "VV completion gate must bind independent completed verification-reader work",
            )?;
            for (label, range) in [
                ("object_open_operations", &reader.object_open_operations),
                (
                    "physical_range_read_operations",
                    &reader.physical_range_read_operations,
                ),
                (
                    "physical_encoded_bytes_read",
                    &reader.physical_encoded_bytes_read,
                ),
                ("codec_decode_operations", &reader.codec_decode_operations),
                ("codec_decoded_bytes", &reader.codec_decoded_bytes),
            ] {
                validate_independent_range_authority(
                    &format!("VV completed verification reader {label}"),
                    range,
                )?;
            }
        }
    }
    Ok(())
}

fn validate_unique_work_expectation(expected: &UniqueWorkExpectation) -> anyhow::Result<()> {
    for (label, union) in [
        ("start", &expected.start_union),
        ("target", &expected.target_union),
    ] {
        require_sha256(
            &union.canonical_entries_sha256,
            &format!("oracle unique-work {label} canonical-entry digest"),
        )?;
        if union.unique_payload_bytes > union.summed_scope_payload_bytes {
            bail!("oracle unique-work {label} union byte facts are incoherent")
        }
    }
    if let Some(baseline) = &expected.residency_baseline {
        require_label(
            &baseline.checkpoint_label,
            "oracle unique-work residency-baseline checkpoint label",
        )?;
        require_sha256(
            &baseline.union.canonical_entries_sha256,
            "oracle unique-work residency-baseline canonical-entry digest",
        )?;
        if baseline.union.unique_payload_bytes > baseline.union.summed_scope_payload_bytes {
            bail!("oracle unique-work residency-baseline union byte facts are incoherent")
        }
    }
    for (label, digest) in [
        ("retained", &expected.delta_union.retained_entries_sha256),
        ("added", &expected.delta_union.added_entries_sha256),
        ("removed", &expected.delta_union.removed_entries_sha256),
    ] {
        require_sha256(
            digest,
            &format!("oracle unique-work {label} partition digest"),
        )?;
    }
    if !expected.delta_union.partitions_pairwise_disjoint
        || expected
            .delta_union
            .retained_unique_keys
            .checked_add(expected.delta_union.removed_unique_keys)
            != Some(expected.start_union.unique_keys)
        || expected
            .delta_union
            .retained_unique_payload_bytes
            .checked_add(expected.delta_union.removed_unique_payload_bytes)
            != Some(expected.start_union.unique_payload_bytes)
        || expected
            .delta_union
            .retained_unique_keys
            .checked_add(expected.delta_union.added_unique_keys)
            != Some(expected.target_union.unique_keys)
        || expected
            .delta_union
            .retained_unique_payload_bytes
            .checked_add(expected.delta_union.added_unique_payload_bytes)
            != Some(expected.target_union.unique_payload_bytes)
        || expected.delta_union.retained_unique_payload_bytes
            > expected.start_union.unique_payload_bytes
        || expected.delta_union.retained_unique_payload_bytes
            > expected.target_union.unique_payload_bytes
        || expected.delta_union.added_unique_payload_bytes
            > expected.target_union.unique_payload_bytes
        || expected.delta_union.removed_unique_payload_bytes
            > expected.start_union.unique_payload_bytes
    {
        bail!(
            "oracle unique-work partitions must be pairwise disjoint and reconcile exactly to both start and target unions"
        )
    }
    for (label, range) in [
        (
            "physical_range_read_operations",
            &expected.physical_range_read_operations,
        ),
        (
            "physical_encoded_bytes_read",
            &expected.physical_encoded_bytes_read,
        ),
        ("codec_decode_operations", &expected.codec_decode_operations),
        ("codec_decoded_bytes", &expected.codec_decoded_bytes),
        (
            "dataset_submitted_requests",
            &expected.dataset_submitted_requests,
        ),
        ("dataset_started_decodes", &expected.dataset_started_decodes),
        (
            "runtime_decoded_output_bytes",
            &expected.runtime_decoded_output_bytes,
        ),
        ("gpu_uploaded_resources", &expected.gpu_uploaded_resources),
        (
            "gpu_uploaded_payload_bytes",
            &expected.gpu_uploaded_payload_bytes,
        ),
        (
            "gpu_control_dynamic_updates",
            &expected.gpu_control_dynamic_updates,
        ),
        (
            "gpu_control_dynamic_upload_bytes",
            &expected.gpu_control_dynamic_upload_bytes,
        ),
        (
            "gpu_control_publication_writes",
            &expected.gpu_control_publication_writes,
        ),
    ] {
        validate_independent_range_authority(&format!("oracle unique-work {label}"), range)?;
    }
    Ok(())
}

fn validate_independent_range_authority(
    label: &str,
    range: &InclusiveU64Range,
) -> anyhow::Result<()> {
    if range.minimum > range.maximum {
        bail!("{label} range must have minimum <= maximum")
    }
    require_label(
        &range.authority.fact_id,
        &format!("{label} independent allowance fact ID"),
    )?;
    require_sha256(
        &range.authority.independent_fact_sha256,
        &format!("{label} independent allowance digest"),
    )?;
    let is_exact = range.minimum == range.maximum;
    if (range.authority.kind == IndependentRangeAuthorityKind::ExactIndependentEnumeration)
        != is_exact
    {
        bail!(
            "{label} must use exact independent authority for a point value and a named bounded authority only for a widened range"
        )
    }
    Ok(())
}

fn validate_phase_state(state: &PhaseStateBinding) -> anyhow::Result<()> {
    require_label(&state.checkpoint_label, "oracle phase checkpoint label")?;
    if state.render_extent.width == 0
        || state.render_extent.height == 0
        || state.mapped_client_extent.width == 0
        || state.mapped_client_extent.height == 0
    {
        bail!("oracle phase render and mapped-client extents must be nonzero")
    }
    if state.layout == ExpectedViewerLayout::Single3d && state.active_view != ViewerPanel::ThreeD {
        bail!("single-3D oracle phases must bind the 3D active view")
    }
    let camera = &state.camera;
    if !finite_values(&camera.target_world)
        || !finite_values(&camera.orientation_xyzw)
        || !finite_positive(camera.orthographic_world_per_screen_point)
        || !finite_positive(camera.perspective_focal_length_screen_points)
        || !finite_positive(camera.perspective_view_distance_world)
    {
        bail!("oracle canonical camera geometry must be finite and positive where required")
    }
    let cross = &state.cross_section;
    if !finite_values(&cross.center_world)
        || !finite_values(&cross.orientation_xyzw)
        || !finite_positive(cross.world_per_screen_point)
        || !cross.depth_world.is_finite()
    {
        bail!("oracle canonical cross-section geometry must be finite")
    }
    let mut panels = BTreeSet::new();
    for plane in &cross.planes {
        if !panels.insert(plane.panel)
            || !finite_values(&plane.plane_origin_world)
            || !finite_values(&plane.u_axis_world)
            || !finite_values(&plane.v_axis_world)
            || !finite_values(&plane.normal_away_world)
            || !finite_positive(plane.world_per_screen_point)
        {
            bail!("oracle cross-section plane geometry must be unique, finite, and positive")
        }
    }
    if panels
        != BTreeSet::from([
            CrossSectionPanel::Xy,
            CrossSectionPanel::Xz,
            CrossSectionPanel::Yz,
        ])
    {
        bail!("oracle phase must bind canonical XY, XZ, and YZ plane geometry exactly once")
    }
    if state.layers.is_empty() || state.layers.len() > 64 {
        bail!("oracle phase must bind between one and 64 layer states")
    }
    let mut layer_ordinals = BTreeSet::new();
    let mut source_orders = BTreeSet::new();
    for layer in &state.layers {
        require_text(&layer.sampling, 64, "oracle layer sampling")?;
        require_text(&layer.mode, 64, "oracle layer mode")?;
        if !matches!(layer.sampling.as_str(), "voxel_exact" | "smooth_linear")
            || !matches!(layer.mode.as_str(), "mip" | "dvr" | "iso")
            || !layer_ordinals.insert(layer.layer_ordinal)
            || !source_orders.insert(layer.source_order)
            || !finite_values(&layer.window)
            || layer.window[0] >= layer.window[1]
            || !finite_positive(layer.gamma)
            || !layer.opacity.is_finite()
            || !(0.0..=1.0).contains(&layer.opacity)
            || !finite_values(&layer.color_rgba)
            || layer
                .color_rgba
                .iter()
                .any(|component| !(0.0..=1.0).contains(component))
        {
            bail!("oracle phase layer states must be unique, finite, and physically bounded")
        }
    }
    let layer_count = u32::try_from(state.layers.len()).expect("64 layers fit u32");
    if source_orders != (0..layer_count).collect() {
        bail!("oracle phase layer source order must be contiguous from zero")
    }
    require_text(&state.ray_step_rule.rule, 128, "oracle ray-step rule")?;
    if !finite_positive(state.ray_step_rule.step_world)
        || state.ray_step_rule.maximum_steps == 0
        || state
            .dvr_density_scale
            .is_some_and(|value| !finite_positive(value))
        || state
            .iso_display_level
            .is_some_and(|value| !value.is_finite())
    {
        bail!("oracle phase volume parameters must be finite and bounded")
    }
    if let Some(shading) = &state.iso_shading {
        require_text(shading, 64, "oracle ISO shading")?;
    }
    if let Some(light) = &state.iso_light {
        require_text(&light.kind, 64, "oracle ISO light kind")?;
        if light.detached_screen_position.is_some_and(|position| {
            !finite_values(&position) || position.iter().any(|value| !(0.0..=1.0).contains(value))
        }) {
            bail!("oracle detached ISO light position must be finite")
        }
    }
    let has_dvr = state.layers.iter().any(|layer| layer.mode == "dvr");
    let has_iso = state.layers.iter().any(|layer| layer.mode == "iso");
    if state.dvr_density_scale.is_some() != has_dvr
        || state.iso_display_level.is_some() != has_iso
        || state.iso_shading.is_some() != has_iso
        || state.iso_light.is_some() != has_iso
    {
        bail!("oracle phase mode-specific parameters must exactly follow its layer modes")
    }
    Ok(())
}

fn finite_values(values: &[f64]) -> bool {
    values.iter().all(|value| value.is_finite())
}

fn finite_positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

fn validate_structural_gate(id: &str, name: &str, phase: &OraclePhase) -> anyhow::Result<()> {
    let expected_kind = match (id, name) {
        ("RZ", "resident_cross_section_zoom" | "resident_3d_zoom")
        | ("RO", "resident_compound_plane_rotation")
        | ("ST", "resident_axis_slice_translation")
        | ("VV", "verification_active_resident") => StructuralGateKind::ResidentGesture,
        ("ZB", "resident_boundary_crossing") => StructuralGateKind::ResidentBoundary,
        ("ZB", "nonresident_boundary_crossing")
        | ("NO", "nonresident_rotation_pan")
        | ("VV", "verification_complete_nonresident") => StructuralGateKind::NonresidentOverlap,
        ("FC", _) => StructuralGateKind::ColdStart,
        ("VM", _) | ("PT", _) => StructuralGateKind::RendererCutoff,
        ("IP", "preprocess_publish") => StructuralGateKind::Preprocessing,
        _ => bail!("viewer scenario {id} has an unrecognized frozen phase {name:?}"),
    };
    if phase.structural_gate.kind != expected_kind {
        bail!("viewer scenario {id} phase {name:?} has the wrong structural gate class")
    }
    if expected_kind == StructuralGateKind::NonresidentOverlap
        && (phase.unique_work.delta_union.added_unique_keys == 0
            || phase.unique_work.delta_union.added_unique_payload_bytes == 0)
    {
        bail!(
            "viewer scenario {id} phase {name:?} nonresident target must contain independently committed added keys and bytes"
        )
    }
    let mut required = match expected_kind {
        StructuralGateKind::ResidentGesture | StructuralGateKind::ResidentBoundary => {
            ZeroWorkCounter::RESIDENT_MANDATORY.to_vec()
        }
        StructuralGateKind::SettledUnchanged => {
            let mut counters = ZeroWorkCounter::RESIDENT_MANDATORY.to_vec();
            counters.extend_from_slice(ZeroWorkCounter::SETTLED_ADDITIONAL);
            counters
        }
        StructuralGateKind::NonresidentOverlap => ZeroWorkCounter::NONRESIDENT_MANDATORY.to_vec(),
        StructuralGateKind::RendererCutoff
        | StructuralGateKind::ColdStart
        | StructuralGateKind::Preprocessing => Vec::new(),
    };
    required.sort_unstable();
    let mut observed = phase.zero_work_counters.clone();
    observed.sort_unstable();
    if observed != required {
        bail!(
            "viewer scenario {id} phase {name:?} must declare its complete mandatory structural zero-work counter set"
        )
    }
    let ceilings_required = expected_kind != StructuralGateKind::Preprocessing;
    if phase.structural_gate.ceilings.is_some() != ceilings_required {
        bail!("viewer scenario {id} phase {name:?} structural ceilings are missing or inapplicable")
    }
    if let Some(ceilings) = &phase.structural_gate.ceilings
        && ceilings.durable_gesture_commits_per_sequence_exact != 1
    {
        bail!(
            "viewer scenario {id} phase {name:?} must require exactly one durable commit per gesture sequence"
        )
    }
    Ok(())
}

fn validate_phase_names<'a>(id: &str, phases: impl Iterator<Item = &'a str>) -> anyhow::Result<()> {
    let phases = phases.collect::<Vec<_>>();
    if phases.is_empty() || phases.len() > 64 {
        bail!("viewer scenario {id} must contain between one and 64 phases")
    }
    let mut unique = BTreeSet::new();
    for phase in &phases {
        require_label(phase, "viewer phase name")?;
        if !unique.insert(*phase) {
            bail!("viewer scenario {id} phase names must be unique")
        }
    }
    let expected: &[&str] = match id {
        "RZ" => &["resident_cross_section_zoom", "resident_3d_zoom"],
        "ZB" => &[
            "resident_boundary_crossing",
            "nonresident_boundary_crossing",
        ],
        "RO" => &["resident_compound_plane_rotation"],
        "ST" => &["resident_axis_slice_translation"],
        "NO" => &["nonresident_rotation_pan"],
        "FC" => &["blocking_target_settled", "exercise_extent_settled"],
        "VM" => &[
            "resident_mip",
            "resident_dvr",
            "resident_iso",
            "exercise_mip",
        ],
        "PT" => &["advance_timepoint", "return_timepoint"],
        "VV" => &[
            "verification_active_resident",
            "verification_complete_nonresident",
        ],
        "IP" => &["preprocess_publish"],
        _ => unreachable!("scenario IDs were validated before their phases"),
    };
    if phases != expected {
        bail!("viewer scenario {id} phases do not match the frozen v2 workload order")
    }
    Ok(())
}

fn require_label(value: &str, label: &str) -> anyhow::Result<()> {
    require_text(value, 128, label)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        bail!("{label} must use only ASCII letters, digits, underscore, hyphen, and dot")
    }
    Ok(())
}

fn validate_automation_template(
    id: &str,
    script: &AutomationScriptTemplate,
    instrumented: bool,
    expected_labels: &[&str],
) -> anyhow::Result<()> {
    if script.schema != AUTOMATION_SCRIPT_SCHEMA
        || script.schema_version != AUTOMATION_SCRIPT_SCHEMA_VERSION
    {
        bail!("viewer scenario {id} uses an unsupported product automation schema")
    }
    if script.scenario != id {
        bail!("viewer scenario {id} script scenario identity differs")
    }
    if script.commands.is_empty() || script.commands.len() > 16_384 {
        bail!("viewer scenario {id} script command count is outside 1..=16384")
    }
    for command in script.commands.iter().chain(
        script
            .startup_bootstrap
            .iter()
            .flat_map(|bootstrap| &bootstrap.commands),
    ) {
        if command.get("command").and_then(Value::as_str) == Some("assert") {
            bail!(
                "viewer scenario {id} contains a fatal product assertion; qualification product results must use typed gates and phase evaluation"
            )
        }
        if command.get("timeout_ms").is_some()
            && !matches!(
                command.get("command").and_then(Value::as_str),
                Some("wait_for" | "await_active_view_gpu_timing")
            )
        {
            bail!(
                "viewer scenario {id} contains a timeout-bearing command outside the accounted fatal wait surface"
            )
        }
    }
    let product_gates = expected_product_gate_batches(&script.commands)?;
    if instrumented && (!script.gpu_timing || !script.diagnostic_counters) {
        bail!("viewer instrumented scripts must enable GPU timing and diagnostic counters")
    }
    if !instrumented && (script.gpu_timing || script.diagnostic_counters) {
        bail!("viewer instrumentation controls must disable timing and diagnostic counters")
    }
    if !instrumented
        && script.commands.iter().any(|command| {
            command.get("command").and_then(Value::as_str) == Some("await_active_view_gpu_timing")
        })
    {
        bail!("instrumentation controls cannot await disabled GPU timing")
    }
    let command_labels = diagnostic_labels(&script.commands)?;
    let mut labels = Vec::new();
    if instrumented
        && let Some(bootstrap) = &script.startup_bootstrap
        && bootstrap.capture_start_checkpoint
        && let Some(label) = bootstrap.start_diagnostic_label.as_deref()
    {
        labels.push(label);
    }
    labels.extend(command_labels);
    if labels != expected_labels {
        bail!("viewer scenario {id} diagnostic checkpoints differ from declared phases")
    }
    if let Some(bootstrap) = &script.startup_bootstrap {
        validate_startup_bootstrap(bootstrap)?;
    }
    if script
        .commands
        .last()
        .and_then(|command| command.get("command"))
        .and_then(Value::as_str)
        != Some("quit")
    {
        bail!("viewer scenario {id} script must end with the normal quit command")
    }
    let final_quit_index = script.commands.len() - 1;
    if product_gates
        .iter()
        .any(|batch| batch.command_index >= final_quit_index)
    {
        bail!("viewer scenario {id} product gates must precede the mandatory final quit")
    }
    let value = serde_json::to_value(script)?;
    validate_placeholder_strings(&value)?;
    Ok(())
}

fn validate_product_gate_inventory(id: &str, commands: &[Value]) -> anyhow::Result<()> {
    let expected_acceptance = expected_acceptance_condition_multiset(id);
    let expected_fatal = expected_fatal_wait_condition_multiset(id);
    let gates = expected_product_gate_observations(commands)?;
    let observed_acceptance =
        gates
            .iter()
            .fold(BTreeMap::<&str, usize>::new(), |mut counts, gate| {
                *counts.entry(gate.condition).or_default() += 1;
                counts
            });
    if observed_acceptance != expected_acceptance {
        bail!("viewer scenario {id} has an incomplete or extra v5 product-gate inventory")
    }
    for (ordinal, gate) in gates.iter().enumerate() {
        let expected_id = format!("{id}.acceptance.{ordinal:03}.{}", gate.condition);
        if gate.gate_id != expected_id {
            bail!("viewer scenario {id} product-gate IDs differ from the frozen ordered identity")
        }
    }

    let mut observed_fatal = BTreeMap::<&str, usize>::new();
    for command in commands {
        if command.get("command").and_then(Value::as_str) == Some("wait_for") {
            let condition = command
                .get("condition")
                .and_then(Value::as_str)
                .context("fatal wait_for command condition is unavailable")?;
            *observed_fatal.entry(condition).or_default() += 1;
        }
    }
    if observed_fatal != expected_fatal {
        bail!(
            "viewer scenario {id} generic fatal-wait inventory differs from the frozen v5 contract"
        )
    }
    Ok(())
}

fn expected_acceptance_condition_multiset(id: &str) -> BTreeMap<&'static str, usize> {
    let entries: &[(&str, usize)] = match id {
        "RZ" => &[("coordinated_presentation_settled", 4), ("runtime_idle", 4)],
        "ZB" => &[("coordinated_presentation_settled", 6), ("runtime_idle", 6)],
        "RO" => &[("coordinated_presentation_settled", 4), ("runtime_idle", 4)],
        "ST" | "NO" => &[("coordinated_presentation_settled", 2), ("runtime_idle", 2)],
        "FC" => &[
            ("first_frame", 1),
            ("frame_freshness_current", 1),
            ("coordinated_presentation_settled", 2),
            ("runtime_idle", 2),
        ],
        "VM" => &[("coordinated_presentation_settled", 8), ("runtime_idle", 8)],
        "PT" => &[("coordinated_presentation_settled", 3), ("runtime_idle", 3)],
        "VV" => &[
            ("coordinated_presentation_settled", 3),
            ("runtime_idle", 3),
            ("source_verification_verified", 1),
        ],
        "IP" => &[
            ("import_idle", 1),
            ("runtime_idle", 1),
            (IMPORTED_OPEN_READY_CONDITION, 1),
        ],
        _ => &[],
    };
    entries.iter().copied().collect()
}

fn expected_fatal_wait_condition_multiset(id: &str) -> BTreeMap<&'static str, usize> {
    let entries: &[(&str, usize)] = match id {
        "RZ" | "ZB" | "RO" | "ST" | "NO" | "FC" | "VM" => {
            &[("window_ready", 1), ("source_verification_inactive", 1)]
        }
        "PT" => &[("window_ready", 1), ("source_verification_inactive", 2)],
        "VV" => &[
            ("window_ready", 1),
            ("source_verification_inactive", 1),
            ("source_verification_required", 1),
        ],
        "IP" => &[
            ("window_ready", 1),
            ("source_verification_inactive", 1),
            ("runtime_idle", 1),
            ("import_review_ready", 1),
        ],
        _ => &[],
    };
    entries.iter().copied().collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
struct RoleScheduleBound {
    gate_batch_count: usize,
    gate_observation_count: usize,
    grouped_gate_wait_bound_ns: u64,
    prerequisite_wait_bound_ns: u64,
    action_duration_bound_ns: u64,
    static_wait_bound_ns: u64,
    derived_process_timeout_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PhaseCheckpointKind {
    Setup,
    Checkpoint,
}

fn expected_product_gate_phase_ids(id: &str) -> &'static [&'static str] {
    match id {
        "RZ" => &[
            "resident_cross_section_zoom.setup.000",
            "resident_cross_section_zoom.checkpoint.000",
            "resident_3d_zoom.setup.000",
            "resident_3d_zoom.checkpoint.000",
        ],
        "ZB" => &[
            "resident_boundary_crossing.setup.000",
            "resident_boundary_crossing.setup.001",
            "resident_boundary_crossing.setup.002",
            "resident_boundary_crossing.checkpoint.000",
            "nonresident_boundary_crossing.setup.000",
            "nonresident_boundary_crossing.checkpoint.000",
        ],
        "RO" => &[
            "resident_compound_plane_rotation.setup.000",
            "resident_compound_plane_rotation.setup.001",
            "resident_compound_plane_rotation.setup.002",
            "resident_compound_plane_rotation.checkpoint.000",
        ],
        "ST" => &[
            "resident_axis_slice_translation.setup.000",
            "resident_axis_slice_translation.checkpoint.000",
        ],
        "NO" => &[
            "nonresident_rotation_pan.setup.000",
            "nonresident_rotation_pan.checkpoint.000",
        ],
        "FC" => &[
            "blocking_target_settled.checkpoint.000",
            "blocking_target_settled.checkpoint.001",
            "exercise_extent_settled.checkpoint.000",
        ],
        "VM" => &[
            "resident_mip.setup.000",
            "resident_mip.checkpoint.000",
            "resident_dvr.setup.000",
            "resident_dvr.checkpoint.000",
            "resident_iso.setup.000",
            "resident_iso.checkpoint.000",
            "exercise_mip.setup.000",
            "exercise_mip.checkpoint.000",
        ],
        "PT" => &[
            "advance_timepoint.setup.000",
            "advance_timepoint.checkpoint.000",
            "return_timepoint.checkpoint.000",
        ],
        "VV" => &[
            "verification_active_resident.setup.000",
            "verification_active_resident.setup.001",
            "verification_complete_nonresident.checkpoint.000",
            "verification_complete_nonresident.checkpoint.001",
        ],
        "IP" => &["preprocess_publish.checkpoint.000"],
        _ => &[],
    }
}

fn validate_product_gate_schedule(
    id: &str,
    scenario: &ScriptScenario,
    script: &AutomationScriptTemplate,
    profile: &ViewerQualificationProfile,
    oracle: &OracleScenario,
) -> anyhow::Result<RoleScheduleBound> {
    if script.diagnostic_counters {
        validate_gpu_timing_await_schedule(scenario, script, oracle)?;
    }
    let batches = expected_product_gate_batches(&script.commands)?;
    let expected_phase_ids = expected_product_gate_phase_ids(id);
    if batches.len() != expected_phase_ids.len() {
        bail!(
            "viewer scenario {id} gate-batch checkpoint count differs from the frozen v5 schedule"
        )
    }
    let mut setup_ordinals = vec![0_usize; scenario.phases.len()];
    let mut checkpoint_ordinals = vec![0_usize; scenario.phases.len()];
    let mut phase_batch_counts = vec![0_usize; scenario.phases.len()];
    let mut last_phase_index = 0_usize;
    let request_verification = script.commands.iter().position(|command| {
        command.get("command").and_then(Value::as_str) == Some("request_source_verification")
    });
    if id == "FC" {
        validate_fc_source_verification_isolation_order(&script.commands, &batches)?;
    }
    for (batch_ordinal, batch) in batches.iter().enumerate() {
        let expected_batch_id = format!("{id}.batch.{batch_ordinal:03}");
        if batch.batch_id != expected_batch_id {
            bail!("viewer scenario {id} product-gate batch IDs differ from the frozen order")
        }
        if batch.phase_id != expected_phase_ids[batch_ordinal] {
            bail!(
                "viewer scenario {id} product-gate phase IDs differ from the frozen checkpoint schedule"
            )
        }
        let mut matched = None;
        for (phase_index, phase) in scenario.phases.iter().enumerate() {
            let setup = format!("{}.setup.{:03}", phase.name, setup_ordinals[phase_index]);
            let checkpoint = format!(
                "{}.checkpoint.{:03}",
                phase.name, checkpoint_ordinals[phase_index]
            );
            if batch.phase_id == setup {
                matched = Some((phase_index, PhaseCheckpointKind::Setup));
                break;
            }
            if batch.phase_id == checkpoint {
                matched = Some((phase_index, PhaseCheckpointKind::Checkpoint));
                break;
            }
        }
        let (phase_index, checkpoint_kind) = matched
            .with_context(|| format!("viewer scenario {id} product-gate phase ID is not next"))?;
        if phase_index < last_phase_index {
            bail!("viewer scenario {id} product-gate phase IDs are not monotonic")
        }
        if checkpoint_kind == PhaseCheckpointKind::Setup && checkpoint_ordinals[phase_index] != 0 {
            bail!("viewer scenario {id} setup gate batch follows a phase checkpoint")
        }
        match checkpoint_kind {
            PhaseCheckpointKind::Setup => setup_ordinals[phase_index] += 1,
            PhaseCheckpointKind::Checkpoint => checkpoint_ordinals[phase_index] += 1,
        }
        phase_batch_counts[phase_index] += 1;
        last_phase_index = phase_index;

        if script.diagnostic_counters {
            let phase = &scenario.phases[phase_index];
            let start = phase
                .start_diagnostic_label
                .as_deref()
                .and_then(|label| diagnostic_command_index(&script.commands, label));
            let end = diagnostic_command_index(&script.commands, &phase.end_diagnostic_label)
                .context("phase end checkpoint is unavailable while validating gate schedule")?;
            match (checkpoint_kind, start) {
                (PhaseCheckpointKind::Setup, Some(start)) if batch.command_index >= start => {
                    bail!("viewer scenario {id} setup gate batch is not before its phase start")
                }
                (PhaseCheckpointKind::Checkpoint, Some(start)) if batch.command_index <= start => {
                    bail!("viewer scenario {id} checkpoint gate batch is not after its phase start")
                }
                (PhaseCheckpointKind::Checkpoint, _) if batch.command_index >= end => {
                    bail!(
                        "viewer scenario {id} checkpoint gate batch must resolve before its phase end diagnostic"
                    )
                }
                _ => {}
            }
            if let Some(next_start) = scenario
                .phases
                .get(phase_index + 1)
                .and_then(|phase| phase.start_diagnostic_label.as_deref())
                .and_then(|label| diagnostic_command_index(&script.commands, label))
                && batch.command_index >= next_start
            {
                bail!("viewer scenario {id} product-gate batch crosses into the next phase")
            }
        }

        if batch.command_index == 0
            || script.commands[batch.command_index - 1]
                .get("command")
                .and_then(Value::as_str)
                == Some("observe_gate_batch")
        {
            bail!("viewer product-gate batches must replace one complete contiguous gate run")
        }
        let phase_name = scenario.phases[phase_index].name.as_str();
        let has_source_verification = batch
            .observations
            .iter()
            .any(|observation| observation.condition == "source_verification_verified");
        let fc_cold_batch = id == "FC"
            && phase_name == "blocking_target_settled"
            && batch
                .observations
                .iter()
                .any(|observation| observation.condition != "runtime_idle");
        if fc_cold_batch
            && batch
                .observations
                .iter()
                .any(|observation| observation.condition == "runtime_idle")
        {
            bail!(
                "FC cold milestone batch must not include the post-verification runtime-idle gate"
            )
        }
        let expected_origin =
            if id == "IP" {
                ProductGateOrigin::ImportPrimaryStarted
            } else if fc_cold_batch {
                ProductGateOrigin::AutomationStarted
            } else if has_source_verification {
                if batch.observations.len() != 1 {
                    bail!("VV source-verification completion must use its own gate batch")
                }
                ProductGateOrigin::CommandCompleted(request_verification.context(
                    "VV source-verification gate batch lacks its request command origin",
                )?)
            } else {
                ProductGateOrigin::CommandCompleted(batch.command_index - 1)
            };
        if batch.origin != expected_origin {
            bail!("viewer scenario {id} product-gate batch has the wrong exact origin")
        }

        for observation in &batch.observations {
            let (authority, deadline) = expected_product_gate_deadline(
                id,
                phase_name,
                observation.condition,
                profile,
                oracle,
            )?;
            if observation.deadline_authority != authority
                || observation.deadline_after_origin_ns != deadline
            {
                bail!("viewer scenario {id} product-gate deadline differs from its owner authority")
            }
        }
    }
    if phase_batch_counts.contains(&0) {
        bail!("every declared viewer phase must own at least one product-gate batch")
    }
    if id == "IP" {
        let [batch] = batches.as_slice() else {
            bail!("IP must merge its three import-primary observations into exactly one batch")
        };
        let start_import = sole_command_index(
            &script.commands,
            "start_reviewed_import",
            "IP reviewed import command",
        )?;
        if batch.command_index != start_import + 1 || batch.observations.len() != 3 {
            bail!("IP import-primary gate batch must immediately follow import start")
        }
    }
    validate_fatal_wait_deadlines(id, &script.commands, profile)?;
    role_schedule_bound(id, script, profile, oracle)
}

fn validate_gpu_timing_await_schedule(
    scenario: &ScriptScenario,
    script: &AutomationScriptTemplate,
    oracle: &OracleScenario,
) -> anyhow::Result<()> {
    let expected_count = oracle
        .phases
        .iter()
        .filter(|phase| phase.gpu_gate.is_some())
        .count();
    let observed_count = script
        .commands
        .iter()
        .filter(|command| {
            command.get("command").and_then(Value::as_str) == Some("await_active_view_gpu_timing")
        })
        .count();
    if observed_count != expected_count {
        bail!("viewer GPU timing await count differs from the GPU-gated phase count")
    }
    for (script_phase, oracle_phase) in scenario.phases.iter().zip(&oracle.phases) {
        let Some(gpu_gate) = oracle_phase.gpu_gate else {
            continue;
        };
        let end = diagnostic_command_index(&script.commands, &script_phase.end_diagnostic_label)
            .context("GPU-gated phase end diagnostic is unavailable")?;
        let command = end
            .checked_sub(1)
            .and_then(|index| script.commands.get(index))
            .context("GPU-gated phase lacks its pre-diagnostic timing await")?;
        let object = command
            .as_object()
            .context("GPU timing await command must be an object")?;
        if object.keys().map(String::as_str).collect::<BTreeSet<_>>()
            != BTreeSet::from(["command", "target", "pass_kind", "timeout_ms"])
            || object.get("command").and_then(Value::as_str) != Some("await_active_view_gpu_timing")
        {
            bail!(
                "every GPU-gated phase must await exact active-view timing immediately before its end diagnostic"
            )
        }
        let expected_target = match oracle_phase.phase_state.active_view {
            ViewerPanel::ThreeD => "three_d",
            ViewerPanel::Xy => "xy",
            ViewerPanel::Xz => "xz",
            ViewerPanel::Yz => "yz",
        };
        let expected_pass_kind = match gpu_gate {
            GpuGate::Plane => "plane",
            GpuGate::Mip | GpuGate::Dvr | GpuGate::Iso => "volume",
        };
        if object.get("target").and_then(Value::as_str) != Some(expected_target)
            || object.get("pass_kind").and_then(Value::as_str) != Some(expected_pass_kind)
            || object.get("timeout_ms").and_then(Value::as_u64) != Some(GPU_TIMING_AWAIT_TIMEOUT_MS)
        {
            bail!("GPU timing await differs from its exact phase target, pass, or timeout")
        }
    }
    Ok(())
}

fn validate_fc_source_verification_isolation_order(
    commands: &[Value],
    batches: &[ExpectedProductGateBatch<'_>],
) -> anyhow::Result<()> {
    let waits = source_verification_inactive_wait_indices(commands).collect::<Vec<_>>();
    let [wait] = waits.as_slice() else {
        bail!("FC must contain exactly one source-verification quiescence wait")
    };
    let cold = batches
        .iter()
        .find(|batch| batch.phase_id == "blocking_target_settled.checkpoint.000")
        .context("FC cold milestone batch is unavailable")?;
    let runtime_idle = batches
        .iter()
        .find(|batch| batch.phase_id == "blocking_target_settled.checkpoint.001")
        .context("FC post-verification runtime-idle batch is unavailable")?;
    if cold.command_index >= *wait {
        bail!("FC cold milestone batch must precede verifier quiescence")
    }
    if runtime_idle.command_index <= *wait {
        bail!("FC resident runtime-idle batch must follow verifier quiescence")
    }
    Ok(())
}

fn expected_product_gate_deadline<'a>(
    id: &str,
    phase_name: &str,
    condition: &str,
    profile: &'a ViewerQualificationProfile,
    oracle: &'a OracleScenario,
) -> anyhow::Result<(&'static str, u64)> {
    let gates = &profile.absolute_gates;
    if id == "IP" {
        if !matches!(
            condition,
            "import_idle" | "runtime_idle" | IMPORTED_OPEN_READY_CONDITION
        ) {
            bail!("IP product-gate condition is not part of the frozen import batch")
        }
        return Ok(("import_primary_wall", import_primary_wall_deadline(oracle)?));
    }
    if condition == "source_verification_verified" {
        if id != "VV" {
            bail!("only VV may use the source-verification product gate")
        }
        return Ok((
            "source_verification_completion",
            gates.source_verification_completion_ns,
        ));
    }
    if id == "FC" && phase_name == "blocking_target_settled" {
        return match condition {
            "first_frame" => Ok(("cold_first_useful", gates.cold_first_useful_ns)),
            "frame_freshness_current" => {
                Ok(("cold_complete_coarse", gates.cold_complete_coarse_ns))
            }
            "coordinated_presentation_settled" => {
                Ok(("cold_target_settlement", gates.cold_target_settlement_ns))
            }
            "runtime_idle" => Ok((
                "maximum_current_presentation_gap_plus_poll_grace",
                gates
                    .maximum_current_presentation_gap_ns
                    .checked_mul(2)
                    .context("resident product-gate deadline overflows")?,
            )),
            _ => bail!("FC blocking phase has an unsupported product-gate condition"),
        };
    }
    let nonresident = matches!(
        (id, phase_name),
        ("ZB", "nonresident_boundary_crossing")
            | ("NO", "nonresident_rotation_pan")
            | ("VV", "verification_complete_nonresident")
    );
    if !matches!(
        condition,
        "coordinated_presentation_settled" | "runtime_idle"
    ) {
        bail!("viewer phase has an unsupported product-gate condition")
    }
    if nonresident {
        Ok((
            "nonresident_target_settlement",
            gates.nonresident_target_settlement_ns,
        ))
    } else {
        Ok((
            "maximum_current_presentation_gap_plus_poll_grace",
            gates
                .maximum_current_presentation_gap_ns
                .checked_mul(2)
                .context("resident product-gate deadline overflows")?,
        ))
    }
}

fn import_primary_wall_deadline(oracle: &OracleScenario) -> anyhow::Result<u64> {
    let deadline = oracle
        .phases
        .iter()
        .find_map(|phase| {
            phase
                .import_gate
                .as_ref()
                .map(|gate| gate.limits.maximum_app_primary_wall_time_ns)
        })
        .context("IP oracle import-primary wall authority is unavailable")?;
    if deadline == 0 || deadline > PRODUCT_GATE_DEADLINE_MAX_NS {
        bail!("IP oracle import-primary wall authority is outside the automation bound")
    }
    Ok(deadline)
}

fn ceil_ns_to_ms(value: u64) -> anyhow::Result<u64> {
    value
        .checked_add(999_999)
        .map(|value| value / 1_000_000)
        .context("viewer deadline overflows while converting to milliseconds")
}

fn validate_fatal_wait_deadlines(
    id: &str,
    commands: &[Value],
    profile: &ViewerQualificationProfile,
) -> anyhow::Result<()> {
    for command in commands
        .iter()
        .filter(|command| command.get("command").and_then(Value::as_str) == Some("wait_for"))
    {
        let object = command
            .as_object()
            .context("fatal wait_for command must be an object")?;
        if object.keys().map(String::as_str).collect::<BTreeSet<_>>()
            != BTreeSet::from(["command", "condition", "timeout_ms"])
        {
            bail!("fatal wait_for command has the wrong exact field set")
        }
        let condition = object
            .get("condition")
            .and_then(Value::as_str)
            .context("fatal wait_for condition is unavailable")?;
        let expected_ms = match condition {
            "window_ready" => 5_000,
            "source_verification_inactive" => SOURCE_VERIFICATION_QUIESCENCE_TIMEOUT_MS,
            "source_verification_verified" | "source_verification_required" => {
                ceil_ns_to_ms(profile.absolute_gates.source_verification_completion_ns)?
            }
            "runtime_idle" => 30_000,
            "import_review_ready" if id == "IP" => 60_000,
            _ => bail!("fatal wait_for condition has no frozen v5 deadline authority"),
        };
        if object.get("timeout_ms").and_then(Value::as_u64) != Some(expected_ms) {
            bail!("fatal wait_for timeout differs from its frozen v5 authority")
        }
    }
    Ok(())
}

fn role_schedule_bound(
    id: &str,
    script: &AutomationScriptTemplate,
    profile: &ViewerQualificationProfile,
    oracle: &OracleScenario,
) -> anyhow::Result<RoleScheduleBound> {
    let batches = expected_product_gate_batches(&script.commands)?;
    let mut grouped_gate_bounds = BTreeMap::<ProductGateOrigin, u64>::new();
    for batch in &batches {
        let bound = batch
            .observations
            .iter()
            .map(|observation| observation.deadline_after_origin_ns)
            .max()
            .context("product-gate batch unexpectedly has no observations")?;
        grouped_gate_bounds
            .entry(batch.origin)
            .and_modify(|current| *current = (*current).max(bound))
            .or_insert(bound);
    }
    let grouped_gate_wait_bound_ns = checked_sum(grouped_gate_bounds.values().copied())?;
    let mut prerequisite_wait_bound_ns = 0_u64;
    let mut action_duration_bound_ns = 0_u64;
    for command in &script.commands {
        let name = command.get("command").and_then(Value::as_str);
        if matches!(name, Some("wait_for" | "await_active_view_gpu_timing")) {
            let timeout_ns = command
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .context("validated fatal wait timeout is unavailable")?
                .checked_mul(1_000_000)
                .context("fatal wait timeout overflows nanoseconds")?;
            prerequisite_wait_bound_ns = prerequisite_wait_bound_ns
                .checked_add(timeout_ns)
                .context("prerequisite wait schedule overflows")?;
            continue;
        }
        if let Some(duration_ms) = command.get("duration_ms").and_then(Value::as_u64) {
            action_duration_bound_ns = action_duration_bound_ns
                .checked_add(
                    duration_ms
                        .checked_mul(1_000_000)
                        .context("action duration overflows nanoseconds")?,
                )
                .context("action duration schedule overflows")?;
        }
        if name == Some("sleep_frames") {
            let frames = command
                .get("frames")
                .and_then(Value::as_u64)
                .context("sleep_frames count is unavailable")?;
            action_duration_bound_ns = action_duration_bound_ns
                .checked_add(
                    frames
                        .checked_mul(profile.absolute_gates.maximum_current_presentation_gap_ns)
                        .context("sleep-frame schedule overflows")?,
                )
                .context("action duration schedule overflows")?;
        }
    }
    let static_wait_bound_ns = grouped_gate_wait_bound_ns
        .checked_add(prerequisite_wait_bound_ns)
        .context("static wait schedule overflows")?;
    if id != "IP" && grouped_gate_wait_bound_ns > MAX_NONIMPORT_STATIC_WAIT_NS {
        bail!("viewer role grouped product-gate schedule exceeds the 35-second v5 ceiling")
    }
    let derived_process_timeout_ns = static_wait_bound_ns
        .checked_add(action_duration_bound_ns)
        .and_then(|value| value.checked_add(PROCESS_STARTUP_ADMISSION_GRACE_NS))
        .and_then(|value| value.checked_add(PROCESS_CLOSEOUT_GRACE_NS))
        .context("derived viewer role process timeout overflows")?;
    if id == "IP" && import_primary_wall_deadline(oracle)? > grouped_gate_wait_bound_ns {
        bail!("IP derived schedule does not count its import-primary wall authority")
    }
    Ok(RoleScheduleBound {
        gate_batch_count: batches.len(),
        gate_observation_count: batches.iter().map(|batch| batch.observations.len()).sum(),
        grouped_gate_wait_bound_ns,
        prerequisite_wait_bound_ns,
        action_duration_bound_ns,
        static_wait_bound_ns,
        derived_process_timeout_ns,
    })
}

fn checked_sum(mut values: impl Iterator<Item = u64>) -> anyhow::Result<u64> {
    values.try_fold(0_u64, |sum, value| {
        sum.checked_add(value)
            .context("viewer schedule bound overflows")
    })
}

fn validate_startup_bootstrap(bootstrap: &AutomationStartupBootstrap) -> anyhow::Result<()> {
    match (
        bootstrap.capture_start_checkpoint,
        bootstrap.start_diagnostic_label.as_deref(),
    ) {
        (true, Some(label)) => require_label(label, "startup-bootstrap diagnostic label")?,
        (false, None) => {}
        (true, None) => {
            bail!("checkpoint-capturing startup bootstrap requires a diagnostic label")
        }
        (false, Some(_)) => {
            bail!("setup-only startup bootstrap must not declare a diagnostic label")
        }
    }
    if bootstrap.commands.is_empty() || bootstrap.commands.len() > 64 {
        bail!("startup bootstrap command count must be in 1..=64")
    }
    let mut mapped_client_sizes = 0_u8;
    let mut render_target_sizes = 0_u8;
    for command in &bootstrap.commands {
        let name = command
            .get("command")
            .and_then(Value::as_str)
            .context("startup bootstrap command must have a string command name")?;
        match name {
            "set_mapped_client_pixels" => {
                mapped_client_sizes = mapped_client_sizes.saturating_add(1)
            }
            "set_render_target_size" => render_target_sizes = render_target_sizes.saturating_add(1),
            "set_four_panel_viewports"
                if command
                    .get("presentation_width_points")
                    .and_then(Value::as_f64)
                    .is_some_and(|value| value.is_finite() && value > 0.0)
                    && command
                        .get("presentation_height_points")
                        .and_then(Value::as_f64)
                        .is_some_and(|value| value.is_finite() && value > 0.0)
                    && [
                        "three_d_render_width",
                        "three_d_render_height",
                        "linked_render_width",
                        "linked_render_height",
                    ]
                    .into_iter()
                    .all(|field| {
                        command
                            .get(field)
                            .and_then(Value::as_u64)
                            .is_some_and(|value| value > 0 && u32::try_from(value).is_ok())
                    }) =>
            {
                render_target_sizes = render_target_sizes.saturating_add(1)
            }
            "set_viewer_layout"
            | "set_time_index"
            | "set_layer_render_mode"
            | "set_projection"
            | "set_layer_sampling"
            | "set_layer_opacity"
            | "set_layer_window"
            | "camera_fit_data"
            | "set_active_cross_section_panel" => {}
            "set_camera_view"
                if command
                    .get("projection")
                    .and_then(Value::as_str)
                    .is_some_and(|value| matches!(value, "orthographic" | "perspective"))
                    && finite_array(command.get("target_world"), 3, false)
                    && finite_array(command.get("orientation_xyzw"), 4, true)
                    && positive_f64(command.get("orthographic_world_per_screen_point"))
                    && positive_f64(command.get("perspective_focal_length_screen_points"))
                    && positive_f64(command.get("perspective_view_distance_world")) => {}
            "set_cross_section_view"
                if finite_array(command.get("center_world"), 3, false)
                    && finite_array(command.get("orientation_xyzw"), 4, true)
                    && positive_f64(command.get("scale_world_per_screen_point"))
                    && positive_f64(command.get("depth_world")) => {}
            "cross_section_zoom_sequence"
                if command.get("samples").and_then(Value::as_u64) == Some(1)
                    && command.get("duration_ms").and_then(Value::as_u64) == Some(1)
                    && command.get("x_fraction").and_then(Value::as_f64) == Some(0.5)
                    && command.get("y_fraction").and_then(Value::as_f64) == Some(0.5) => {}
            _ => bail!("command {name:?} is not permitted in the startup bootstrap"),
        }
    }
    if mapped_client_sizes > 1 || render_target_sizes > 1 {
        bail!("startup bootstrap accepts at most one mapped-client size and one render-target size")
    }
    Ok(())
}

fn positive_f64(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_f64)
        .is_some_and(|value| value.is_finite() && value > 0.0)
}

fn finite_array(value: Option<&Value>, expected_len: usize, require_nonzero: bool) -> bool {
    let Some(values) = value.and_then(Value::as_array) else {
        return false;
    };
    values.len() == expected_len
        && values
            .iter()
            .all(|value| value.as_f64().is_some_and(f64::is_finite))
        && (!require_nonzero
            || values
                .iter()
                .any(|value| value.as_f64().is_some_and(|value| value != 0.0)))
}

fn diagnostic_labels(commands: &[Value]) -> anyhow::Result<Vec<&str>> {
    let mut labels = Vec::new();
    for command in commands {
        let object = command
            .as_object()
            .context("viewer automation command must be a JSON object")?;
        let name = object
            .get("command")
            .and_then(Value::as_str)
            .context("viewer automation command must have a string command name")?;
        if name == "sample_diagnostics" {
            let label = object
                .get("label")
                .and_then(Value::as_str)
                .context("sample_diagnostics must have a string label")?;
            labels.push(label);
        }
    }
    Ok(labels)
}

fn normalized_semantic_script(script: &AutomationScriptTemplate) -> Value {
    let mut commands = script
        .commands
        .iter()
        .filter(|command| {
            !matches!(
                command.get("command").and_then(Value::as_str),
                Some("sample_diagnostics" | "copy_diagnostics" | "await_active_view_gpu_timing")
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    for command in &mut commands {
        if command.get("command").and_then(Value::as_str) != Some("observe_gate_batch")
            || command.pointer("/origin/kind").and_then(Value::as_str) != Some("command_completed")
        {
            continue;
        }
        let source_verification_origin = command
            .get("observations")
            .and_then(Value::as_array)
            .is_some_and(|observations| {
                observations.iter().any(|observation| {
                    observation
                        .pointer("/target/condition")
                        .and_then(Value::as_str)
                        == Some("source_verification_verified")
                })
            });
        command["origin"] = json!({
            "kind": "command_completed",
            "semantic_origin": if source_verification_origin {
                "request_source_verification"
            } else {
                "immediate_predecessor"
            },
        });
    }
    json!({
        "schema": script.schema,
        "schema_version": script.schema_version,
        "scenario": script.scenario,
        "startup_bootstrap": script.startup_bootstrap,
        "hard_safety_limits": script.hard_safety_limits,
        "commands": commands,
    })
}

fn validate_placeholder_strings(value: &Value) -> anyhow::Result<()> {
    match value {
        Value::String(value) => validate_placeholder_string(value),
        Value::Array(values) => {
            for value in values {
                validate_placeholder_strings(value)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            for value in values.values() {
                validate_placeholder_strings(value)?;
            }
            Ok(())
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
    }
}

fn validate_placeholder_string(value: &str) -> anyhow::Result<()> {
    if !value.contains('$') {
        return Ok(());
    }
    let suffix = value
        .strip_prefix(ATTEMPT_ROOT_PLACEHOLDER)
        .context("viewer scripts may use only the ${ATTEMPT_ROOT} substitution")?;
    if suffix.contains('$') || (!suffix.is_empty() && !suffix.starts_with('/')) {
        bail!("${{ATTEMPT_ROOT}} must be the sole leading path substitution")
    }
    let suffix = suffix.strip_prefix('/').unwrap_or(suffix);
    if !suffix.is_empty() {
        validate_relative_attempt_path(Path::new(suffix), "attempt-root substituted path")?;
    }
    Ok(())
}

fn validate_relative_attempt_path(path: &Path, label: &str) -> anyhow::Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!("{label} must be a nonempty relative path")
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            bail!("{label} must contain only normal path components")
        }
    }
    Ok(())
}

fn build_fresh_release_app(
    repository_root: &Path,
    result_root: &Path,
    revision: &str,
    compiler: &str,
) -> anyhow::Result<ImmutableSourceBuild> {
    let source_root = create_immutable_source_worktree(repository_root, result_root, revision)?;
    crate::import_performance_t5::require_no_external_cargo_configuration(&source_root)?;
    let target_directory = result_root.join("fresh-private-target");
    let mut command = cargo_command();
    command
        .current_dir(&source_root)
        .env("RUSTC", "rustc")
        .env("RUSTFLAGS", "")
        .env("CARGO_ENCODED_RUSTFLAGS", "")
        .env("RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "")
        .env("CARGO_PROFILE_RELEASE_OPT_LEVEL", "3")
        .env("CARGO_PROFILE_RELEASE_DEBUG", "false")
        .env("CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS", "false")
        .env("CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS", "false")
        .env("CARGO_PROFILE_RELEASE_INCREMENTAL", "false")
        .env("CARGO_PROFILE_RELEASE_LTO", "false")
        .env("CARGO_PROFILE_RELEASE_PANIC", "unwind")
        .env("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "16")
        .env("CARGO_PROFILE_RELEASE_RPATH", "false")
        .env("CARGO_PROFILE_RELEASE_STRIP", "none")
        .env("MIRANTE4D_T5_BUILD_REVISION", revision)
        .env("MIRANTE4D_T5_BUILD_PROFILE", "release")
        .env("MIRANTE4D_T5_BUILD_COMPILER", compiler)
        .env("MIRANTE4D_T5_BUILD_TARGET_MODE", "fresh-private-target")
        .env("MIRANTE4D_VIEWER_BUILD_OPT_LEVEL", "3")
        .env("MIRANTE4D_VIEWER_BUILD_DEBUG", "false")
        .env("MIRANTE4D_VIEWER_BUILD_CUSTOM_RUSTFLAGS", "false")
        .env("MIRANTE4D_VIEWER_BUILD_RUSTC_WRAPPER", "false")
        .args(["build", "--locked", "--release", "--target-dir"])
        .arg(&target_directory)
        .args(["-p", "mirante4d-app"]);
    let status = command
        .status()
        .context("failed to spawn the fresh viewer release app build")?;
    if !status.success() {
        bail!("fresh viewer release app build failed with status {status}")
    }
    let app_binary = validate_app_binary(
        &target_directory
            .join("release")
            .join(format!("mirante4d-app{}", std::env::consts::EXE_SUFFIX)),
    )?;
    let source_after = repository_identity_at(&source_root);
    if source_after.commit.as_deref() != Some(revision)
        || source_after.dirty_worktree != Some(false)
    {
        bail!("immutable viewer source changed during the fresh app build")
    }
    Ok(ImmutableSourceBuild {
        source_root,
        app_binary,
    })
}

fn create_immutable_source_worktree(
    repository_root: &Path,
    result_root: &Path,
    revision: &str,
) -> anyhow::Result<PathBuf> {
    let source_root = result_root.join("immutable-source");
    if source_root.exists() {
        bail!("immutable viewer source root already exists")
    }
    let status = Command::new("git")
        .arg("-C")
        .arg(repository_root)
        .args(["worktree", "add", "--detach"])
        .arg(&source_root)
        .arg(revision)
        .status()
        .context("failed to create the immutable viewer source worktree")?;
    if !status.success() {
        bail!("immutable viewer source worktree creation failed with {status}")
    }
    let source_root = fs::canonicalize(&source_root)
        .context("immutable viewer source worktree is unavailable")?;
    let identity = repository_identity_at(&source_root);
    if identity.root.as_deref() != Some(source_root.as_path())
        || identity.commit.as_deref() != Some(revision)
        || identity.dirty_worktree != Some(false)
    {
        bail!("immutable viewer source worktree does not match the bound clean revision")
    }
    Ok(source_root)
}

fn validate_app_binary(path: &Path) -> anyhow::Result<PathBuf> {
    if !path.is_absolute() {
        bail!("viewer app binary path must be absolute")
    }
    require_nonsymlink_components(path, "viewer app binary")?;
    let canonical = fs::canonicalize(path).context("viewer app binary is unavailable")?;
    let metadata = fs::symlink_metadata(&canonical)
        .context("viewer app binary is unavailable or unreadable")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("viewer app binary must be a nonsymlink regular file")
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        bail!("viewer app binary must already be executable")
    }
    Ok(canonical)
}

fn create_result_directory(path: &Path, repository_root: &Path) -> anyhow::Result<PathBuf> {
    if !path.is_absolute() {
        bail!("viewer result directory path must be absolute")
    }
    if path.exists() {
        bail!("viewer result directory must not already exist")
    }
    let parent = path
        .parent()
        .context("viewer result directory has no parent")?;
    require_nonsymlink_components(parent, "viewer result directory parent")?;
    let parent =
        fs::canonicalize(parent).context("viewer result directory parent is unavailable")?;
    if parent.starts_with(repository_root) {
        bail!("viewer result directory must be outside the repository")
    }
    let name = path
        .file_name()
        .context("viewer result directory has no final component")?;
    let canonical_target = parent.join(name);
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(&canonical_target)
        .context("failed to create the new private viewer result directory")?;
    fs::canonicalize(canonical_target)
        .context("failed to resolve the private viewer result directory")
}

fn create_private_directory(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        bail!("private viewer attempt directory already exists")
    }
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path).with_context(|| {
        format!(
            "failed to create private attempt directory {}",
            path.display()
        )
    })
}

fn digest_regular_file(path: &Path, label: &str) -> anyhow::Result<String> {
    require_nonsymlink_components(path, label)?;
    let metadata = fs::symlink_metadata(path).with_context(|| format!("{label} is unavailable"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label} must be a nonsymlink regular file")
    }
    let mut file = File::open(path).with_context(|| format!("{label} is unreadable"))?;
    let mut hasher = Sha256Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("{label} could not be read"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_string())
}

fn write_new_synced_json(path: &Path, value: &Value) -> anyhow::Result<()> {
    let mut encoded = serde_json::to_vec_pretty(value).context("failed to encode private JSON")?;
    encoded.push(b'\n');
    if encoded.len() > RAW_REPORT_MAX_BYTES {
        bail!("private viewer JSON exceeds its 64 MiB aggregate bound")
    }
    write_new_synced(path, &encoded)
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("failed to create private evidence file {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("failed to write private evidence file {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync private evidence file {}", path.display()))?;
    if let Some(parent) = path.parent() {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| {
                format!(
                    "failed to sync private evidence directory {}",
                    parent.display()
                )
            })?;
    }
    Ok(())
}

fn create_attempt_tree(
    result_root: &Path,
    sample_index: u32,
    scenario: &str,
    role: AttemptRole,
) -> anyhow::Result<PathBuf> {
    let sample_root = result_root.join(format!("sample-{sample_index:02}"));
    if !sample_root.exists() {
        create_private_directory(&sample_root)?;
    }
    let scenario_root = sample_root.join(scenario);
    if !scenario_root.exists() {
        create_private_directory(&scenario_root)?;
    }
    let role_root = scenario_root.join(role.directory_name());
    create_private_directory(&role_root)?;
    for directory in ["config", "cache", "data", "state", "tmp"] {
        create_private_directory(&role_root.join(directory))?;
    }
    let mirante_settings = role_root.join("config/mirante4d");
    create_private_directory(&mirante_settings)?;
    Ok(role_root)
}

/// Creates only the declared parent of an attempt-local imported package.
/// The package itself remains create-new product output, and every component
/// is kept beneath the freshly created private role root.
fn prepare_attempt_import_parent(role_root: &Path, cleanup: &AttemptCleanup) -> anyhow::Result<()> {
    if !cleanup.enabled {
        return Ok(());
    }
    let relative = cleanup
        .imported_package_relative_path
        .as_deref()
        .context("enabled cleanup lacks its package path")?;
    validate_relative_attempt_path(relative, "attempt-local imported package")?;
    let Some(parent) = relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    let mut cursor = role_root.to_path_buf();
    for component in parent.components() {
        let Component::Normal(component) = component else {
            bail!("attempt-local import parent contains a non-normal component")
        };
        cursor.push(component);
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => bail!("attempt-local import parent is not a private directory"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_private_directory(&cursor)?;
            }
            Err(error) => {
                return Err(error).context("attempt-local import parent is unavailable");
            }
        }
    }
    Ok(())
}

fn write_resource_settings(
    role_root: &Path,
    profile: &ViewerQualificationProfile,
) -> anyhow::Result<()> {
    write_new_synced_json(
        &role_root.join("config/mirante4d/settings.json"),
        &json!({
            "schema": "mirante4d-settings",
            "schema_version": 1,
            "resource_policy": {
                "cpu_dataset_budget_bytes": profile.resources.max_cpu_total_bytes,
                "gpu_budget_bytes": profile.resources.gpu_budget_bytes,
            },
        }),
    )
}

fn execute_samples(
    profile: &LoadedProfile,
    import_source: &ImportSourceBinding,
    scripts: &ScriptBundle,
    oracle: &OracleBundle,
    app_binary: &Path,
    result_root: &Path,
    immutability: &RunImmutabilityBinding,
) -> Vec<SampleEvidence> {
    let script_map = scripts
        .scenarios
        .iter()
        .map(|scenario| (scenario.id.as_str(), scenario))
        .collect::<BTreeMap<_, _>>();
    let oracle_map = oracle
        .scenarios
        .iter()
        .map(|scenario| (scenario.id.as_str(), scenario))
        .collect::<BTreeMap<_, _>>();
    let mut samples = Vec::new();
    for sample_index in 1..=profile.profile.protocol.development_samples {
        for scenario_id in REQUIRED_SCENARIOS {
            let script = script_map
                .get(scenario_id)
                .expect("script coverage was validated");
            let oracle_scenario = oracle_map
                .get(scenario_id)
                .expect("oracle coverage was validated");
            let sample = execute_sample(
                &profile.profile,
                import_source,
                &oracle.numerical_contract,
                sample_index,
                script,
                oracle_scenario,
                app_binary,
                result_root,
                immutability,
            );
            let integrity_failed = has_integrity_reasons(&sample.reasons)
                || has_integrity_reasons(&sample.instrumented.reasons)
                || sample
                    .control
                    .as_ref()
                    .is_some_and(|role| has_integrity_reasons(&role.reasons))
                || sample
                    .phases
                    .iter()
                    .any(|phase| has_integrity_reasons(&phase.reasons));
            samples.push(sample);
            if integrity_failed {
                return samples;
            }
        }
    }
    samples
}

fn validate_attempt_population(
    profile: &ViewerQualificationProfile,
    scripts: &ScriptBundle,
    samples: &[SampleEvidence],
    reasons: &mut BTreeSet<String>,
) -> PopulationEvidence {
    let development_samples = usize::try_from(profile.protocol.development_samples)
        .expect("the bounded development sample count fits usize");
    let script_map = scripts
        .scenarios
        .iter()
        .map(|scenario| (scenario.id.as_str(), scenario))
        .collect::<BTreeMap<_, _>>();
    let expected_sample_records = development_samples
        .checked_mul(REQUIRED_SCENARIOS.len())
        .expect("the bounded sample population fits usize");
    let expected_role_attempts = expected_sample_records
        .checked_mul(2)
        .expect("the bounded role population fits usize");
    let phases_per_sample = REQUIRED_SCENARIOS
        .iter()
        .map(|id| {
            script_map
                .get(id)
                .expect("script coverage was validated")
                .phases
                .len()
        })
        .sum::<usize>();
    let expected_phase_evaluations = development_samples
        .checked_mul(phases_per_sample)
        .expect("the bounded phase population fits usize");
    let gates_per_sample = REQUIRED_SCENARIOS
        .iter()
        .map(|id| {
            let scenario = script_map.get(id).expect("script coverage was validated");
            expected_product_gate_observations(&scenario.instrumented_script.commands)
                .expect("script product gates were validated")
                .len()
                + expected_product_gate_observations(
                    &scenario
                        .instrumentation_control_script
                        .as_ref()
                        .expect("instrumentation controls were validated")
                        .commands,
                )
                .expect("control product gates were validated")
                .len()
        })
        .sum::<usize>();
    let expected_product_gate_observations = development_samples
        .checked_mul(gates_per_sample)
        .expect("the bounded product-gate population fits usize");

    let expected_keys = (1..=profile.protocol.development_samples)
        .flat_map(|sample_index| {
            REQUIRED_SCENARIOS
                .into_iter()
                .map(move |scenario| (sample_index, scenario))
        })
        .collect::<BTreeSet<_>>();
    let observed_keys = samples
        .iter()
        .map(|sample| (sample.sample_index, sample.scenario.as_str()))
        .collect::<BTreeSet<_>>();
    let sample_order_exact = samples
        .iter()
        .map(|sample| (sample.sample_index, sample.scenario.as_str()))
        .eq(
            (1..=profile.protocol.development_samples).flat_map(|sample_index| {
                REQUIRED_SCENARIOS
                    .into_iter()
                    .map(move |scenario| (sample_index, scenario))
            }),
        );
    if samples.len() != expected_sample_records {
        reasons.insert("sample_population_cardinality_mismatch".to_owned());
    }
    let sample_identities_exact =
        observed_keys == expected_keys && observed_keys.len() == samples.len();
    if !sample_identities_exact {
        reasons.insert("sample_population_identity_mismatch".to_owned());
    }
    if !sample_order_exact {
        reasons.insert("sample_population_order_mismatch".to_owned());
    }

    let mut observed_role_attempts = 0_usize;
    let mut completed_role_reports = 0_usize;
    let mut observed_phase_evaluations = 0_usize;
    let mut observed_product_gate_observations = 0_usize;
    let mut role_identities_exact = true;
    let mut role_order_exact = true;
    let mut phase_identities_exact = true;
    let mut product_gate_bijections_exact = true;
    for sample in samples {
        let role_order_matches = REQUIRED_SCENARIOS
            .iter()
            .position(|id| *id == sample.scenario.as_str())
            .is_some_and(|scenario_ordinal| {
                sample.role_launch_order.as_slice()
                    == balanced_role_order(sample.sample_index, scenario_ordinal).as_slice()
            });
        if !role_order_matches {
            role_order_exact = false;
            reasons.insert("role_attempt_order_mismatch".to_owned());
        }
        observed_role_attempts = observed_role_attempts
            .saturating_add(usize::from(sample.instrumented.process.launch_attempted));
        completed_role_reports = completed_role_reports.saturating_add(usize::from(
            sample.instrumented.automation_report_sha256.is_some(),
        ));
        observed_product_gate_observations = observed_product_gate_observations
            .saturating_add(sample.instrumented.product_gate_outcomes.len());
        if sample.instrumented.role != AttemptRole::Instrumented {
            role_identities_exact = false;
            reasons.insert("role_attempt_identity_mismatch".to_owned());
        }
        match &sample.control {
            Some(control) => {
                observed_role_attempts = observed_role_attempts
                    .saturating_add(usize::from(control.process.launch_attempted));
                completed_role_reports = completed_role_reports
                    .saturating_add(usize::from(control.automation_report_sha256.is_some()));
                observed_product_gate_observations = observed_product_gate_observations
                    .saturating_add(control.product_gate_outcomes.len());
                if control.role != AttemptRole::InstrumentationControl {
                    role_identities_exact = false;
                    reasons.insert("role_attempt_identity_mismatch".to_owned());
                }
            }
            None => {
                role_identities_exact = false;
                reasons.insert("instrumentation_control_missing".to_owned());
            }
        }
        observed_phase_evaluations = observed_phase_evaluations.saturating_add(sample.phases.len());
        match script_map.get(sample.scenario.as_str()) {
            Some(scenario) => {
                if !sample
                    .phases
                    .iter()
                    .map(|phase| phase.name.as_str())
                    .eq(scenario.phases.iter().map(|phase| phase.name.as_str()))
                {
                    phase_identities_exact = false;
                    reasons.insert("phase_evaluation_identity_mismatch".to_owned());
                }
                if !product_gate_outcomes_match_template(
                    &sample.instrumented.product_gate_outcomes,
                    &scenario.instrumented_script,
                ) || sample.control.as_ref().is_none_or(|control| {
                    scenario
                        .instrumentation_control_script
                        .as_ref()
                        .is_none_or(|template| {
                            !product_gate_outcomes_match_template(
                                &control.product_gate_outcomes,
                                template,
                            )
                        })
                }) {
                    product_gate_bijections_exact = false;
                    reasons.insert("product_gate_observation_identity_mismatch".to_owned());
                }
            }
            _ => {
                phase_identities_exact = false;
                product_gate_bijections_exact = false;
                reasons.insert("phase_evaluation_identity_mismatch".to_owned());
                reasons.insert("product_gate_observation_identity_mismatch".to_owned());
            }
        }
    }
    if observed_role_attempts != expected_role_attempts {
        reasons.insert("role_attempt_population_cardinality_mismatch".to_owned());
    }
    if completed_role_reports != expected_role_attempts {
        reasons.insert("completed_role_report_population_mismatch".to_owned());
    }
    if observed_phase_evaluations != expected_phase_evaluations {
        reasons.insert("phase_evaluation_population_mismatch".to_owned());
    }
    if observed_product_gate_observations != expected_product_gate_observations {
        reasons.insert("product_gate_observation_population_mismatch".to_owned());
    }
    PopulationEvidence {
        expected_sample_records,
        observed_sample_records: samples.len(),
        expected_role_attempts,
        observed_role_attempts,
        completed_role_reports,
        expected_phase_evaluations,
        observed_phase_evaluations,
        expected_product_gate_observations,
        observed_product_gate_observations,
        sample_identities_exact,
        sample_order_exact,
        role_identities_exact,
        role_order_exact,
        phase_identities_exact,
        product_gate_bijections_exact,
    }
}

fn product_gate_outcomes_match_template(
    outcomes: &[ProductGateOutcome],
    template: &AutomationScriptTemplate,
) -> bool {
    let Ok(expected) = expected_product_gate_observations(&template.commands) else {
        return false;
    };
    outcomes.len() == expected.len()
        && outcomes.iter().zip(expected).all(|(outcome, expected)| {
            outcome.command_index == expected.command_index
                && outcome.batch_id == expected.batch_id
                && outcome.phase_id == expected.phase_id
                && outcome.observation_index == expected.observation_index
                && outcome.gate_id == expected.gate_id
                && outcome.condition == expected.condition
                && outcome.deadline_authority == expected.deadline_authority
                && outcome.deadline_after_origin_ns == expected.deadline_after_origin_ns
                && outcome.origin_kind == expected.origin.kind_label()
                && outcome.origin_command_index == expected.origin.command_index()
                && product_gate_outcome_is_coherent(outcome)
        })
}

fn validate_population_instrumentation_overhead(
    profile: &ViewerQualificationProfile,
    samples: &[SampleEvidence],
    population: PopulationEvidence,
    reasons: &mut BTreeSet<String>,
) -> Vec<InstrumentationOverheadPopulationEvidence> {
    let population_exact = population_evidence_is_exact(population);
    let expected_sample_pairs =
        usize::try_from(profile.protocol.development_samples).unwrap_or(usize::MAX);
    let maximum = u64::from(
        profile
            .absolute_gates
            .maximum_instrumentation_overhead_basis_points,
    );
    let mut rows = Vec::with_capacity(REQUIRED_SCENARIOS.len());
    for scenario in REQUIRED_SCENARIOS {
        let scenario_samples = samples
            .iter()
            .filter(|sample| sample.scenario == scenario)
            .collect::<Vec<_>>();
        let observed_sample_pairs = scenario_samples.len();
        let population_complete =
            population_exact && observed_sample_pairs == expected_sample_pairs;
        if observed_sample_pairs != expected_sample_pairs {
            reasons.insert("instrumentation_overhead_population_missing".to_owned());
        }
        let instrumented_raw_wall = population_complete
            .then(|| {
                scenario_samples.iter().try_fold(0_u64, |total, sample| {
                    total.checked_add(sample.instrumented.app_wall_time_ns?)
                })
            })
            .flatten();
        let qualification_wait_wall = population_complete
            .then(|| {
                scenario_samples.iter().try_fold(0_u64, |total, sample| {
                    total.checked_add(sample.instrumented_qualification_wait_wall_ns?)
                })
            })
            .flatten();
        let instrumented_adjusted_wall = population_complete
            .then(|| {
                scenario_samples.iter().try_fold(0_u64, |total, sample| {
                    total.checked_add(sample.instrumented_adjusted_wall_time_ns?)
                })
            })
            .flatten();
        let control_wall = population_complete
            .then(|| {
                scenario_samples.iter().try_fold(0_u64, |total, sample| {
                    total.checked_add(sample.control.as_ref()?.app_wall_time_ns?)
                })
            })
            .flatten();
        let instrumented_cpu = population_complete
            .then(|| {
                scenario_samples.iter().try_fold(0_u64, |total, sample| {
                    total.checked_add(sample.instrumented.process_cpu_time_ns?)
                })
            })
            .flatten();
        let control_cpu = population_complete
            .then(|| {
                scenario_samples.iter().try_fold(0_u64, |total, sample| {
                    total.checked_add(sample.control.as_ref()?.process_cpu_time_ns?)
                })
            })
            .flatten();
        let adjusted_wall_reconciles = match (
            instrumented_raw_wall,
            qualification_wait_wall,
            instrumented_adjusted_wall,
        ) {
            (Some(raw), Some(wait), Some(adjusted)) => raw.checked_sub(wait) == Some(adjusted),
            _ => false,
        };
        if population_complete && !adjusted_wall_reconciles {
            reasons.insert(
                "instrumentation_adjusted_wall_time_population_reconciliation_failed".to_owned(),
            );
        }
        let wall = population_complete
            .then(|| {
                paired_overhead_basis_points(
                    instrumented_adjusted_wall.filter(|_| adjusted_wall_reconciles),
                    control_wall,
                    "instrumentation_wall_overhead_population_fact_missing",
                    reasons,
                )
            })
            .flatten();
        let cpu = population_complete
            .then(|| {
                paired_overhead_basis_points(
                    instrumented_cpu,
                    control_cpu,
                    "instrumentation_cpu_overhead_population_fact_missing",
                    reasons,
                )
            })
            .flatten();
        let gate_evaluable = wall.is_some() && cpu.is_some();
        let gate_passed = wall
            .zip(cpu)
            .map(|(wall, cpu)| wall <= maximum && cpu <= maximum);
        if gate_passed == Some(false) {
            reasons.insert("instrumentation_overhead_gate_exceeded".to_owned());
        }
        rows.push(InstrumentationOverheadPopulationEvidence {
            scenario: scenario.to_owned(),
            expected_sample_pairs,
            observed_sample_pairs,
            instrumented_raw_app_wall_time_ns: instrumented_raw_wall,
            instrumented_qualification_wait_wall_ns: qualification_wait_wall,
            instrumented_adjusted_app_wall_time_ns: instrumented_adjusted_wall,
            control_app_wall_time_ns: control_wall,
            wall_overhead_basis_points: wall,
            instrumented_process_cpu_time_ns: instrumented_cpu,
            control_process_cpu_time_ns: control_cpu,
            process_cpu_overhead_basis_points: cpu,
            maximum_overhead_basis_points: maximum,
            population_complete,
            gate_evaluable,
            gate_passed,
        });
    }
    rows
}

fn product_gate_outcome_is_coherent(outcome: &ProductGateOutcome) -> bool {
    match outcome.outcome {
        ProductGateStatus::Passed => {
            outcome.condition_met
                && !outcome.timed_out
                && outcome.observed_after_origin_ns < outcome.deadline_after_origin_ns
        }
        ProductGateStatus::Failed => {
            outcome.timed_out
                && outcome.observed_after_origin_ns >= outcome.deadline_after_origin_ns
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_sample(
    profile: &ViewerQualificationProfile,
    import_source: &ImportSourceBinding,
    numerical_contract: &NumericalContract,
    sample_index: u32,
    scenario: &ScriptScenario,
    oracle: &OracleScenario,
    app_binary: &Path,
    result_root: &Path,
    immutability: &RunImmutabilityBinding,
) -> SampleEvidence {
    execute_sample_with_role_executor(
        profile,
        numerical_contract,
        sample_index,
        scenario,
        oracle,
        result_root,
        |role, template| {
            execute_role(
                profile,
                (scenario.id == "IP").then_some(import_source),
                sample_index,
                scenario,
                oracle,
                template,
                role,
                app_binary,
                result_root,
                immutability,
            )
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_sample_with_role_executor<F>(
    profile: &ViewerQualificationProfile,
    numerical_contract: &NumericalContract,
    sample_index: u32,
    scenario: &ScriptScenario,
    oracle: &OracleScenario,
    result_root: &Path,
    mut role_executor: F,
) -> SampleEvidence
where
    F: FnMut(AttemptRole, &AutomationScriptTemplate) -> RoleEvidence,
{
    let mut instrumented = None;
    let mut control = None;
    let mut instrumented_phases = None;
    let mut role_launch_order = Vec::with_capacity(2);
    let expected_manifest = oracle
        .phases
        .iter()
        .find_map(|phase| phase.expected_imported_root_manifest_sha256.as_deref());
    let import_gate = oracle
        .phases
        .iter()
        .find_map(|phase| phase.import_gate.as_ref());
    let scenario_ordinal = REQUIRED_SCENARIOS
        .iter()
        .position(|id| *id == scenario.id)
        .expect("validated scenario has a frozen ordinal");
    let roles = balanced_role_order(sample_index, scenario_ordinal);
    for role in roles {
        let template = match role {
            AttemptRole::Instrumented => Some(&scenario.instrumented_script),
            AttemptRole::InstrumentationControl => scenario.instrumentation_control_script.as_ref(),
        };
        let mut evidence = template.map_or_else(
            || missing_control_evidence(result_root, sample_index, &scenario.id),
            |template| role_executor(role, template),
        );
        if let Some(expected_manifest) = expected_manifest {
            validate_imported_manifest_identity(&mut evidence, expected_manifest);
        }
        if role == AttemptRole::InstrumentationControl
            && let Some(import_gate) = import_gate
        {
            match evidence.automation_report.as_ref() {
                Some(report) => validate_import_workflow_gate(
                    report,
                    import_gate,
                    imported_open_ready_outcome(&evidence.product_gate_outcomes),
                    &mut evidence.reasons,
                ),
                None => {
                    evidence
                        .reasons
                        .insert("import_workflow_evidence_missing".to_owned());
                }
            }
        }
        let phases = (role == AttemptRole::Instrumented)
            .then(|| evaluate_phases(profile, numerical_contract, scenario, oracle, &evidence));
        let integrity_failed = has_integrity_reasons(&evidence.reasons)
            || phases.as_ref().is_some_and(|phases| {
                phases
                    .iter()
                    .any(|phase| has_integrity_reasons(&phase.reasons))
            });
        if evidence.process.launch_attempted {
            role_launch_order.push(role);
        }
        match role {
            AttemptRole::Instrumented => {
                instrumented = Some(evidence);
                instrumented_phases = phases;
            }
            AttemptRole::InstrumentationControl => control = Some(evidence),
        }
        if integrity_failed {
            break;
        }
    }
    let mut instrumented = instrumented.unwrap_or_else(|| {
        unlaunched_role_evidence(
            result_root,
            sample_index,
            &scenario.id,
            AttemptRole::Instrumented,
        )
    });
    if control.is_none() {
        control = Some(unlaunched_role_evidence(
            result_root,
            sample_index,
            &scenario.id,
            AttemptRole::InstrumentationControl,
        ));
    }
    let phases = instrumented_phases.unwrap_or_else(|| {
        evaluate_phases(profile, numerical_contract, scenario, oracle, &instrumented)
    });
    let mut reasons = BTreeSet::new();
    let instrumented_qualification_wait_wall_ns = qualification_gpu_timing_await_wall_ns(
        instrumented.automation_report.as_ref(),
        &scenario.instrumented_script,
        &mut reasons,
    );
    let instrumented_adjusted_wall_time_ns = match (
        instrumented.app_wall_time_ns,
        instrumented_qualification_wait_wall_ns,
    ) {
        (Some(wall), Some(wait)) if wait <= wall => Some(wall - wait),
        _ => {
            reasons.insert("instrumentation_adjusted_wall_time_unavailable".to_owned());
            None
        }
    };
    let (wall_overhead_basis_points, process_cpu_overhead_basis_points) = match &control {
        Some(control) => {
            let wall = paired_overhead_basis_points(
                instrumented_adjusted_wall_time_ns,
                control.app_wall_time_ns,
                "instrumentation_wall_overhead_fact_missing",
                &mut reasons,
            );
            let cpu = paired_overhead_basis_points(
                instrumented.process_cpu_time_ns,
                control.process_cpu_time_ns,
                "instrumentation_cpu_overhead_fact_missing",
                &mut reasons,
            );
            (wall, cpu)
        }
        None => {
            reasons.insert("instrumentation_control_missing".to_owned());
            (None, None)
        }
    };
    instrumented.automation_report = None;
    if let Some(control) = control.as_mut() {
        control.automation_report = None;
    }
    SampleEvidence {
        sample_index,
        scenario: scenario.id.clone(),
        role_launch_order,
        instrumented,
        control,
        phases,
        instrumented_qualification_wait_wall_ns,
        instrumented_adjusted_wall_time_ns,
        wall_overhead_basis_points,
        process_cpu_overhead_basis_points,
        reasons,
    }
}

fn balanced_role_order(sample_index: u32, scenario_ordinal: usize) -> [AttemptRole; 2] {
    if (usize::try_from(sample_index)
        .unwrap_or_default()
        .checked_add(scenario_ordinal)
        .expect("the bounded sample and scenario ordinals fit usize"))
    .is_multiple_of(2)
    {
        [
            AttemptRole::Instrumented,
            AttemptRole::InstrumentationControl,
        ]
    } else {
        [
            AttemptRole::InstrumentationControl,
            AttemptRole::Instrumented,
        ]
    }
}

fn validate_imported_manifest_identity(role: &mut RoleEvidence, expected: &str) {
    if prepublication_import_failure_is_exact(role) {
        if !role.cleanup_completed || role.cleanup_manifest_sha256.is_some() {
            role.reasons
                .insert("prepublication_import_cleanup_or_absence_evidence_invalid".to_owned());
        }
        return;
    }
    match role.cleanup_manifest_sha256.as_deref() {
        Some(observed) if observed == expected => {}
        Some(_) => {
            role.reasons
                .insert("imported_root_manifest_identity_mismatch".to_owned());
        }
        None => {
            role.reasons
                .insert("imported_root_manifest_identity_missing".to_owned());
        }
    }
}

fn prepublication_import_failure_is_exact(role: &RoleEvidence) -> bool {
    let Some(workflow) = role
        .automation_report
        .as_ref()
        .and_then(|report| report.get("import_workflow_evidence"))
    else {
        return false;
    };
    if workflow.get("primary_clock") != Some(&Value::Null)
        || workflow.get("publication_to_open_ready_clock") != Some(&Value::Null)
        || workflow.get("last_successful_receipt") != Some(&Value::Null)
        || role_product_gate_status(role, IMPORTED_OPEN_READY_CONDITION)
            != Some(ProductGateStatus::Failed)
        || role_product_gate_status(role, "import_idle") != Some(ProductGateStatus::Failed)
    {
        return false;
    }
    let run_counts = (
        import_u64(workflow, "successful_runs"),
        import_u64(workflow, "published_events"),
        import_u64(workflow, "failed_runs"),
        import_u64(workflow, "cancelled_runs"),
    );
    (run_counts == (Some(0), Some(0), Some(1), Some(0))
        && role_product_gate_status(role, "runtime_idle") == Some(ProductGateStatus::Passed))
        || (run_counts == (Some(0), Some(0), Some(0), Some(0))
            && role_product_gate_status(role, "runtime_idle") == Some(ProductGateStatus::Failed))
}

fn role_product_gate_status(role: &RoleEvidence, condition: &str) -> Option<ProductGateStatus> {
    let mut matches = role
        .product_gate_outcomes
        .iter()
        .filter(|outcome| outcome.condition == condition);
    let status = matches.next()?.outcome;
    matches.next().is_none().then_some(status)
}

fn capture_bound_import_source(
    template: &AutomationScriptTemplate,
    binding: &ImportSourceBinding,
) -> anyhow::Result<super::source_inventory::InventoryFacts> {
    let facts = capture_import_source(template)?;
    if !import_source_inventory_matches_binding(&facts, binding) {
        bail!("IP import source inventory differs from its workload commitment")
    }
    Ok(facts)
}

fn capture_import_source(
    template: &AutomationScriptTemplate,
) -> anyhow::Result<super::source_inventory::InventoryFacts> {
    let source = sole_ip_source_path(&template.commands)?;
    super::source_inventory::capture(source)
}

fn import_source_inventory_matches_binding(
    facts: &super::source_inventory::InventoryFacts,
    binding: &ImportSourceBinding,
) -> bool {
    facts.regular_files == binding.regular_files
        && facts.source_bytes == binding.source_bytes
        && facts.sha256 == binding.inventory_sha256
}

fn validate_import_report_source_binding(
    report: &Value,
    binding: &ImportSourceBinding,
    reasons: &mut BTreeSet<String>,
) {
    let start_matches =
        unique_passed_event_details(report, "start_reviewed_import").is_some_and(|details| {
            details
                .get("reviewed_source_fingerprint_sha256")
                .and_then(Value::as_str)
                == Some(binding.reviewed_source_fingerprint_sha256.as_str())
                && details.get("reviewed_source_bytes").and_then(Value::as_u64)
                    == Some(binding.source_bytes)
        });
    let receipt = report.pointer("/import_workflow_evidence/last_successful_receipt");
    let receipt_matches = match receipt {
        Some(Value::Null) => true,
        Some(receipt) if receipt.is_object() => {
            receipt
                .get("reviewed_source_fingerprint_sha256")
                .and_then(Value::as_str)
                == Some(binding.reviewed_source_fingerprint_sha256.as_str())
                && receipt.get("reviewed_source_bytes").and_then(Value::as_u64)
                    == Some(binding.source_bytes)
        }
        _ => false,
    };
    if !start_matches || !receipt_matches {
        reasons.insert("import_receipt_workload_source_binding_mismatch".to_owned());
    }
}

fn missing_control_evidence(result_root: &Path, sample_index: u32, scenario: &str) -> RoleEvidence {
    RoleEvidence {
        role: AttemptRole::InstrumentationControl,
        root: result_root.join(format!(
            "sample-{sample_index:02}/{scenario}/instrumentation-control"
        )),
        expanded_script_sha256: String::new(),
        template_script_sha256: String::new(),
        process: ProcessObservation {
            launch_attempted: false,
            status: None,
            external_wall_time_ns: 0,
            timed_out: false,
            spawn_error: Some("instrumentation control is absent".to_owned()),
        },
        automation_report: None,
        automation_report_sha256: None,
        app_wall_time_ns: None,
        process_cpu_time_ns: None,
        derived_process_timeout_ns: 0,
        static_wait_bound_ns: 0,
        gate_batch_count: 0,
        gate_observation_count: 0,
        source_inventory_before: None,
        source_inventory_after: None,
        cleanup_manifest_sha256: None,
        cleanup_completed: false,
        product_gate_outcomes: Vec::new(),
        reasons: BTreeSet::from(["instrumentation_control_missing".to_owned()]),
    }
}

fn unlaunched_role_evidence(
    result_root: &Path,
    sample_index: u32,
    scenario: &str,
    role: AttemptRole,
) -> RoleEvidence {
    RoleEvidence {
        role,
        root: result_root.join(format!(
            "sample-{sample_index:02}/{scenario}/{}",
            role.directory_name()
        )),
        expanded_script_sha256: String::new(),
        template_script_sha256: String::new(),
        process: ProcessObservation {
            launch_attempted: false,
            status: None,
            external_wall_time_ns: 0,
            timed_out: false,
            spawn_error: None,
        },
        automation_report: None,
        automation_report_sha256: None,
        app_wall_time_ns: None,
        process_cpu_time_ns: None,
        derived_process_timeout_ns: 0,
        static_wait_bound_ns: 0,
        gate_batch_count: 0,
        gate_observation_count: 0,
        source_inventory_before: None,
        source_inventory_after: None,
        cleanup_manifest_sha256: None,
        cleanup_completed: false,
        product_gate_outcomes: Vec::new(),
        reasons: BTreeSet::from(["population_aborted_after_integrity_failure".to_owned()]),
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_role(
    profile: &ViewerQualificationProfile,
    import_source: Option<&ImportSourceBinding>,
    sample_index: u32,
    scenario: &ScriptScenario,
    oracle: &OracleScenario,
    template: &AutomationScriptTemplate,
    role: AttemptRole,
    app_binary: &Path,
    result_root: &Path,
    immutability: &RunImmutabilityBinding,
) -> RoleEvidence {
    execute_role_with_prelaunch_check(
        profile,
        import_source,
        sample_index,
        scenario,
        oracle,
        template,
        role,
        app_binary,
        result_root,
        || prelaunch_immutability_reason_codes(immutability, app_binary),
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_role_with_prelaunch_check(
    profile: &ViewerQualificationProfile,
    import_source: Option<&ImportSourceBinding>,
    sample_index: u32,
    scenario: &ScriptScenario,
    oracle: &OracleScenario,
    template: &AutomationScriptTemplate,
    role: AttemptRole,
    app_binary: &Path,
    result_root: &Path,
    prelaunch_check: impl FnOnce() -> BTreeSet<String>,
) -> RoleEvidence {
    let schedule = role_schedule_bound(&scenario.id, template, profile, oracle)
        .expect("validated viewer script has a derived process schedule");
    let intended_root = result_root.join(format!(
        "sample-{sample_index:02}/{}/{role_name}",
        scenario.id,
        role_name = role.directory_name(),
    ));
    let prelaunch_reasons = prelaunch_check();
    if !prelaunch_reasons.is_empty() {
        return rejected_prelaunch_role_evidence(role, intended_root, schedule, prelaunch_reasons);
    }
    let setup = (|| -> anyhow::Result<(PathBuf, String, String)> {
        let role_root = create_attempt_tree(result_root, sample_index, &scenario.id, role)?;
        prepare_attempt_import_parent(&role_root, &scenario.cleanup)?;
        write_resource_settings(&role_root, profile)?;
        let template_value = serde_json::to_value(template)?;
        let template_bytes = serde_json::to_vec(&template_value)?;
        let template_sha256 = Sha256Hasher::digest(&template_bytes).to_string();
        let expanded = expand_script_template(template_value, &role_root)?;
        let expanded_bytes = serde_json::to_vec_pretty(&expanded)?;
        let expanded_sha256 = Sha256Hasher::digest(&expanded_bytes).to_string();
        write_new_synced(&role_root.join("automation-script.json"), &expanded_bytes)?;
        Ok((role_root, template_sha256, expanded_sha256))
    })();
    let (role_root, template_script_sha256, expanded_script_sha256) = match setup {
        Ok(setup) => setup,
        Err(error) => {
            return RoleEvidence {
                role,
                root: intended_root,
                expanded_script_sha256: String::new(),
                template_script_sha256: String::new(),
                process: ProcessObservation {
                    launch_attempted: false,
                    status: None,
                    external_wall_time_ns: 0,
                    timed_out: false,
                    spawn_error: Some(error.to_string()),
                },
                automation_report: None,
                automation_report_sha256: None,
                app_wall_time_ns: None,
                process_cpu_time_ns: None,
                derived_process_timeout_ns: schedule.derived_process_timeout_ns,
                static_wait_bound_ns: schedule.static_wait_bound_ns,
                gate_batch_count: schedule.gate_batch_count,
                gate_observation_count: schedule.gate_observation_count,
                source_inventory_before: None,
                source_inventory_after: None,
                cleanup_manifest_sha256: None,
                cleanup_completed: false,
                product_gate_outcomes: Vec::new(),
                reasons: BTreeSet::from(["attempt_setup_failed".to_owned()]),
            };
        }
    };
    let source_inventory_before = match import_source {
        Some(binding) => match capture_bound_import_source(template, binding) {
            Ok(facts) => Some(facts),
            Err(_) => {
                return RoleEvidence {
                    role,
                    root: role_root,
                    expanded_script_sha256,
                    template_script_sha256,
                    process: ProcessObservation {
                        launch_attempted: false,
                        status: None,
                        external_wall_time_ns: 0,
                        timed_out: false,
                        spawn_error: Some(
                            "import source inventory preflight was rejected".to_owned(),
                        ),
                    },
                    automation_report: None,
                    automation_report_sha256: None,
                    app_wall_time_ns: None,
                    process_cpu_time_ns: None,
                    derived_process_timeout_ns: schedule.derived_process_timeout_ns,
                    static_wait_bound_ns: schedule.static_wait_bound_ns,
                    gate_batch_count: schedule.gate_batch_count,
                    gate_observation_count: schedule.gate_observation_count,
                    source_inventory_before: None,
                    source_inventory_after: None,
                    cleanup_manifest_sha256: None,
                    cleanup_completed: false,
                    product_gate_outcomes: Vec::new(),
                    reasons: BTreeSet::from(
                        ["import_source_inventory_preflight_failed".to_owned()],
                    ),
                };
            }
        },
        None => None,
    };
    let process = run_app_process(
        app_binary,
        &profile.workload.representative_package.root,
        &role_root,
        Duration::from_nanos(schedule.derived_process_timeout_ns),
    );
    let mut reasons = BTreeSet::new();
    if process.spawn_error.is_some() {
        reasons.insert("app_process_spawn_failed".to_owned());
    }
    if process.timed_out {
        reasons.insert("app_process_timed_out".to_owned());
    }
    match process.status {
        Some(status) if status.success() => {}
        Some(_) => {
            reasons.insert("app_process_exit_failed".to_owned());
        }
        None => {
            reasons.insert("app_process_exit_unavailable".to_owned());
        }
    }
    let report_path = role_root.join("automation-report.json");
    let automation_report = read_automation_report(&report_path).map_or_else(
        |_| {
            reasons.insert("automation_report_missing_or_invalid".to_owned());
            None
        },
        Some,
    );
    let automation_report_sha256 = automation_report
        .as_ref()
        .and_then(|_| digest_regular_file(&report_path, "viewer automation report").ok());
    if automation_report.is_some() && automation_report_sha256.is_none() {
        reasons.insert("automation_report_digest_unavailable".to_owned());
    }
    let mut app_wall_time_ns = None;
    let mut process_cpu_time_ns = None;
    let mut product_gate_outcomes = Vec::new();
    if let Some(report) = &automation_report {
        validate_basic_automation_report(
            report,
            app_binary,
            &role_root.join("automation-script.json"),
            template,
            profile,
            role,
            &mut product_gate_outcomes,
            &mut reasons,
        );
        app_wall_time_ns = duration_ms_to_ns(report.get("duration_ms"));
        if app_wall_time_ns.is_none() {
            reasons.insert("automation_wall_time_missing".to_owned());
        }
        process_cpu_time_ns = report
            .pointer("/process_cpu_time/elapsed_ns")
            .and_then(Value::as_u64)
            .filter(|_| {
                report
                    .pointer("/process_cpu_time/available")
                    .and_then(Value::as_bool)
                    == Some(true)
            });
        if process_cpu_time_ns.is_none() {
            reasons.insert("automation_process_cpu_time_missing".to_owned());
        }
        if let Some(binding) = import_source {
            validate_import_report_source_binding(report, binding, &mut reasons);
        }
    }
    let source_inventory_after =
        import_source.and_then(|binding| match capture_import_source(template) {
            Ok(facts) => {
                if source_inventory_before.as_ref() != Some(&facts)
                    || !import_source_inventory_matches_binding(&facts, binding)
                {
                    reasons.insert("import_source_inventory_changed".to_owned());
                }
                Some(facts)
            }
            Err(_) => {
                reasons.insert("import_source_inventory_postflight_failed".to_owned());
                None
            }
        });
    let mut cleanup_manifest_sha256 = None;
    let mut cleanup_completed = false;
    // Cleanup is part of collecting the import manifest evidence.  An
    // authoritative product-gate failure must still reach it; only integrity
    // failures make the attempt unsafe to use as evidence.
    if !has_integrity_reasons(&reasons) && scenario.cleanup.enabled {
        match cleanup_attempt_package(&role_root, &scenario.cleanup) {
            Ok(digest) => {
                cleanup_manifest_sha256 = digest;
                cleanup_completed = true;
            }
            Err(_) => {
                reasons.insert("attempt_local_cleanup_failed".to_owned());
            }
        }
    }
    RoleEvidence {
        role,
        root: role_root,
        expanded_script_sha256,
        template_script_sha256,
        process,
        automation_report,
        automation_report_sha256,
        app_wall_time_ns,
        process_cpu_time_ns,
        derived_process_timeout_ns: schedule.derived_process_timeout_ns,
        static_wait_bound_ns: schedule.static_wait_bound_ns,
        gate_batch_count: schedule.gate_batch_count,
        gate_observation_count: schedule.gate_observation_count,
        source_inventory_before,
        source_inventory_after,
        cleanup_manifest_sha256,
        cleanup_completed,
        product_gate_outcomes,
        reasons,
    }
}

fn rejected_prelaunch_role_evidence(
    role: AttemptRole,
    root: PathBuf,
    schedule: RoleScheduleBound,
    reasons: BTreeSet<String>,
) -> RoleEvidence {
    RoleEvidence {
        role,
        root,
        expanded_script_sha256: String::new(),
        template_script_sha256: String::new(),
        process: ProcessObservation {
            launch_attempted: false,
            status: None,
            external_wall_time_ns: 0,
            timed_out: false,
            spawn_error: Some("prelaunch immutability binding rejected".to_owned()),
        },
        automation_report: None,
        automation_report_sha256: None,
        app_wall_time_ns: None,
        process_cpu_time_ns: None,
        derived_process_timeout_ns: schedule.derived_process_timeout_ns,
        static_wait_bound_ns: schedule.static_wait_bound_ns,
        gate_batch_count: schedule.gate_batch_count,
        gate_observation_count: schedule.gate_observation_count,
        source_inventory_before: None,
        source_inventory_after: None,
        cleanup_manifest_sha256: None,
        cleanup_completed: false,
        product_gate_outcomes: Vec::new(),
        reasons,
    }
}

fn expand_script_template(mut value: Value, attempt_root: &Path) -> anyhow::Result<Value> {
    let root = attempt_root
        .to_str()
        .context("viewer attempt root is not valid UTF-8")?;
    expand_value(&mut value, root)?;
    Ok(value)
}

fn expand_value(value: &mut Value, root: &str) -> anyhow::Result<()> {
    match value {
        Value::String(value) => {
            validate_placeholder_string(value)?;
            if let Some(suffix) = value.strip_prefix(ATTEMPT_ROOT_PLACEHOLDER) {
                *value = format!("{root}{suffix}");
            }
            Ok(())
        }
        Value::Array(values) => {
            for value in values {
                expand_value(value, root)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                expand_value(value, root)?;
            }
            Ok(())
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
    }
}

fn run_app_process(
    app_binary: &Path,
    startup_package: &Path,
    role_root: &Path,
    timeout: Duration,
) -> ProcessObservation {
    let stdout = open_attempt_output(&role_root.join("stdout.log"));
    let stderr = open_attempt_output(&role_root.join("stderr.log"));
    let (Ok(stdout), Ok(stderr)) = (stdout, stderr) else {
        return ProcessObservation {
            launch_attempted: false,
            status: None,
            external_wall_time_ns: 0,
            timed_out: false,
            spawn_error: Some("failed to create attempt stdout/stderr".to_owned()),
        };
    };
    let mut command = Command::new(app_binary);
    command
        .process_group(0)
        .env("MIRANTE4D_DEV_DATASET", startup_package)
        .env("MIRANTE4D_ENABLE_AUTOMATION", "1")
        .env(
            "MIRANTE4D_AUTOMATION_SCRIPT",
            role_root.join("automation-script.json"),
        )
        .env(
            "MIRANTE4D_AUTOMATION_REPORT",
            role_root.join("automation-report.json"),
        )
        .env("MIRANTE4D_LOG_FILE", role_root.join("app.log"))
        .env("XDG_CONFIG_HOME", role_root.join("config"))
        .env("XDG_CACHE_HOME", role_root.join("cache"))
        .env("XDG_DATA_HOME", role_root.join("data"))
        .env("XDG_STATE_HOME", role_root.join("state"))
        .env("TMPDIR", role_root.join("tmp"))
        .env("MESA_SHADER_CACHE_DIR", role_root.join("cache/mesa"))
        .env(
            "__GL_SHADER_DISK_CACHE_PATH",
            role_root.join("cache/nvidia"),
        )
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let started = Instant::now();
    let child = command.spawn();
    let Ok(mut child) = child else {
        return ProcessObservation {
            launch_attempted: true,
            status: None,
            external_wall_time_ns: elapsed_ns(started),
            timed_out: false,
            spawn_error: Some("failed to spawn supplied viewer app binary".to_owned()),
        };
    };
    let deadline = started + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return ProcessObservation {
                    launch_attempted: true,
                    status: Some(status),
                    external_wall_time_ns: elapsed_ns(started),
                    timed_out: false,
                    spawn_error: None,
                };
            }
            Ok(None) => {}
            Err(_) => {
                terminate_process_group(&mut child);
                return ProcessObservation {
                    launch_attempted: true,
                    status: None,
                    external_wall_time_ns: elapsed_ns(started),
                    timed_out: false,
                    spawn_error: Some("failed to poll supplied viewer app binary".to_owned()),
                };
            }
        }
        if Instant::now() >= deadline {
            terminate_process_group(&mut child);
            let status = child.wait().ok();
            return ProcessObservation {
                launch_attempted: true,
                status,
                external_wall_time_ns: elapsed_ns(started),
                timed_out: true,
                spawn_error: None,
            };
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
}

fn open_attempt_output(path: &Path) -> anyhow::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("failed to create attempt output {}", path.display()))
}

fn terminate_process_group(child: &mut Child) {
    let group = -(i32::try_from(child.id()).unwrap_or(i32::MAX));
    // SAFETY: the child is placed in a new process group immediately before
    // spawning; the negative PID therefore targets only that attempt tree.
    unsafe {
        kill(group, 9);
    }
}

unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn read_automation_report(path: &Path) -> anyhow::Result<Value> {
    require_nonsymlink_components(path, "viewer automation report")?;
    let bytes = read_bounded_regular_file(
        path,
        AUTOMATION_REPORT_MAX_BYTES,
        "viewer automation report",
    )?;
    serde_json::from_slice(&bytes).context("viewer automation report is malformed")
}

#[allow(clippy::too_many_arguments)]
fn validate_basic_automation_report(
    report: &Value,
    app_binary: &Path,
    script_path: &Path,
    template: &AutomationScriptTemplate,
    profile: &ViewerQualificationProfile,
    role: AttemptRole,
    product_gate_outcomes: &mut Vec<ProductGateOutcome>,
    reasons: &mut BTreeSet<String>,
) {
    if report.get("schema").and_then(Value::as_str) != Some(AUTOMATION_REPORT_SCHEMA)
        || report.get("schema_version").and_then(Value::as_u64)
            != Some(AUTOMATION_REPORT_SCHEMA_VERSION)
    {
        reasons.insert("automation_report_schema_mismatch".to_owned());
    }
    if report.get("status").and_then(Value::as_str) != Some("passed")
        || !report.get("failure_reason").is_some_and(Value::is_null)
    {
        reasons.insert("automation_report_failed".to_owned());
    }
    match product_gate_outcomes_from_report(report, template) {
        Ok(outcomes) => product_gate_outcomes.extend(outcomes),
        Err(_) => {
            reasons.insert("product_gate_observation_event_set_invalid".to_owned());
        }
    }
    validate_app_build_provenance(report, profile, reasons);
    validate_device_and_presentation_contract(report, profile, reasons);
    let binary_matches = report
        .get("binary")
        .and_then(Value::as_str)
        .and_then(|path| fs::canonicalize(path).ok())
        .is_some_and(|reported| reported == app_binary);
    if !binary_matches {
        reasons.insert("automation_report_binary_mismatch".to_owned());
    }
    let script_matches = report
        .pointer("/script/path")
        .and_then(Value::as_str)
        .and_then(|path| fs::canonicalize(path).ok())
        .and_then(|reported| {
            fs::canonicalize(script_path)
                .ok()
                .map(|expected| reported == expected)
        })
        .unwrap_or(false);
    if !script_matches
        || report.pointer("/script/schema").and_then(Value::as_str)
            != Some(AUTOMATION_SCRIPT_SCHEMA)
        || report
            .pointer("/script/schema_version")
            .and_then(Value::as_u64)
            != Some(AUTOMATION_SCRIPT_SCHEMA_VERSION)
        || report.pointer("/script/scenario").and_then(Value::as_str)
            != Some(template.scenario.as_str())
        || report
            .pointer("/script/command_count")
            .and_then(Value::as_u64)
            != u64::try_from(template.commands.len()).ok()
    {
        reasons.insert("automation_report_script_mismatch".to_owned());
    }
    if !automation_report_hard_safety_limits_match(report, template) {
        reasons.insert("automation_report_hard_safety_limits_mismatch".to_owned());
    }
    validate_startup_bootstrap_report(report, template, reasons);
    validate_resource_policy(report, profile, reasons);
    let timing_enabled = report
        .pointer("/final_diagnostics/gpu_adapter/timing/enabled")
        .and_then(Value::as_bool);
    let expected_timing = role == AttemptRole::Instrumented;
    if timing_enabled != Some(expected_timing) {
        reasons.insert("gpu_timing_instrumentation_state_mismatch".to_owned());
    }
    let detailed_counters =
        report.pointer("/final_diagnostics/render/display_coordination/detailed_counters");
    let detailed_state_matches = if expected_timing {
        detailed_counters
            .and_then(|value| value.get("enabled"))
            .and_then(Value::as_bool)
            == Some(true)
    } else {
        detailed_counters.is_some_and(Value::is_null)
    };
    if !detailed_state_matches {
        reasons.insert("diagnostic_counter_instrumentation_state_mismatch".to_owned());
    }
    if expected_timing {
        for pointer in [
            "/final_diagnostics/gpu_adapter/timing/timestamps_supported",
            "/final_diagnostics/gpu_adapter/timing/payload_copy_timestamps_supported",
        ] {
            if report.pointer(pointer).and_then(Value::as_bool) != Some(true) {
                reasons.insert("gpu_timestamp_capability_unavailable".to_owned());
            }
        }
        if report
            .pointer("/final_diagnostics/gpu_adapter/timing/gpu_timing_prelude_submissions")
            .and_then(Value::as_u64)
            .is_none()
        {
            reasons.insert("gpu_timing_prelude_count_missing".to_owned());
        }
        let cpu = report.pointer("/final_diagnostics/gpu_adapter/timing/cpu");
        if cpu
            .and_then(|value| value.get("last_planning_ns"))
            .and_then(Value::as_u64)
            .is_none()
            || cpu
                .and_then(|value| value.get("last_queue_submit_ns"))
                .and_then(Value::as_u64)
                .is_none()
            || !cpu.is_some_and(|value| optional_u64_field(value, "last_control_publication_ns"))
            || !cpu.is_some_and(|value| optional_u64_field(value, "last_payload_staging_ns"))
        {
            reasons.insert("cpu_frame_stage_metrics_missing_or_invalid".to_owned());
        }
        if report
            .pointer("/final_diagnostics/gpu_adapter/timing/last_upload_ns")
            .is_some()
            || report
                .pointer("/final_diagnostics/gpu_adapter/timing/last_volume_pass_ns")
                .is_some()
        {
            reasons.insert("removed_gpu_timing_alias_present".to_owned());
        }
    }
}

fn automation_report_hard_safety_limits_match(
    report: &Value,
    template: &AutomationScriptTemplate,
) -> bool {
    report.get("limits").is_none()
        && serde_json::to_value(template.hard_safety_limits)
            .ok()
            .as_ref()
            == report.get("hard_safety_limits")
}

fn product_gate_outcomes_from_report(
    report: &Value,
    template: &AutomationScriptTemplate,
) -> anyhow::Result<Vec<ProductGateOutcome>> {
    let expected = expected_product_gate_batches(&template.commands)?;
    let events = report
        .get("events")
        .and_then(Value::as_array)
        .context("automation product-gate events are unavailable")?;
    if events.iter().any(|event| {
        matches!(
            event.get("command").and_then(Value::as_str),
            Some("observe_gate" | "observe_imported_open_ready")
        )
    }) {
        bail!("automation report contains a removed serial product-gate event")
    }
    let observed = events
        .iter()
        .filter(|event| event.get("command").and_then(Value::as_str) == Some("observe_gate_batch"))
        .collect::<Vec<_>>();
    if observed.len() != expected.len() {
        bail!("automation product-gate batch event cardinality differs from the script")
    }

    let observation_count = expected.iter().map(|batch| batch.observations.len()).sum();
    let mut outcomes = Vec::with_capacity(observation_count);
    for (expected_batch, event) in expected.iter().zip(observed) {
        let event_object = event
            .as_object()
            .context("automation product-gate batch event must be an object")?;
        if event_object
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != BTreeSet::from([
                "command_index",
                "command",
                "status",
                "event_epoch_ms",
                "duration_ms",
                "details",
            ])
            || event
                .get("event_epoch_ms")
                .and_then(Value::as_u64)
                .is_none()
            || !event
                .get("duration_ms")
                .and_then(Value::as_f64)
                .is_some_and(|duration| duration.is_finite() && duration >= 0.0)
        {
            bail!("automation product-gate batch event has an invalid exact outer shape")
        }
        if event.get("command_index").and_then(Value::as_u64)
            != u64::try_from(expected_batch.command_index).ok()
            || event.get("command").and_then(Value::as_str) != Some("observe_gate_batch")
            || event.get("status").and_then(Value::as_str) != Some("passed")
        {
            bail!("automation product-gate batch event identity or order is invalid")
        }
        let details = event
            .get("details")
            .and_then(Value::as_object)
            .context("automation product-gate batch event details are unavailable")?;
        let expected_fields = BTreeSet::from([
            "schema",
            "batch_id",
            "phase_id",
            "origin",
            "completed_after_origin_ns",
            "observations",
        ]);
        if details.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected_fields {
            bail!("automation product-gate batch details have the wrong exact field set")
        }
        if details.get("schema").and_then(Value::as_str) != Some(PRODUCT_GATE_OBSERVATION_SCHEMA)
            || details.get("batch_id").and_then(Value::as_str) != Some(expected_batch.batch_id)
            || details.get("phase_id").and_then(Value::as_str) != Some(expected_batch.phase_id)
        {
            bail!("automation product-gate batch details differ from the bound command")
        }
        let observed_origin = parse_product_gate_origin(
            details
                .get("origin")
                .context("automation product-gate batch origin is unavailable")?,
        )?;
        if observed_origin != expected_batch.origin {
            bail!("automation product-gate batch origin differs from its bound command")
        }
        let completed_after_origin_ns = details
            .get("completed_after_origin_ns")
            .and_then(Value::as_u64)
            .context("automation product-gate batch completion offset is unavailable")?;
        let observed_rows = details
            .get("observations")
            .and_then(Value::as_array)
            .context("automation product-gate batch observations are unavailable")?;
        if observed_rows.len() != expected_batch.observations.len() {
            bail!("automation product-gate batch observation cardinality differs")
        }
        for (observation_index, (expected_observation, observed)) in expected_batch
            .observations
            .iter()
            .zip(observed_rows)
            .enumerate()
        {
            let observed = observed
                .as_object()
                .context("automation product-gate batch observation must be an object")?;
            if observed.keys().map(String::as_str).collect::<BTreeSet<_>>()
                != BTreeSet::from([
                    "observation_index",
                    "gate_id",
                    "condition",
                    "deadline_authority",
                    "deadline_after_origin_ns",
                    "outcome",
                    "condition_met",
                    "timed_out",
                    "observed_after_origin_ns",
                ])
            {
                bail!("automation product-gate batch observation has the wrong exact field set")
            }
            let gate_id = observed
                .get("gate_id")
                .and_then(Value::as_str)
                .context("automation product-gate observation gate ID is unavailable")?;
            let condition = observed
                .get("condition")
                .and_then(Value::as_str)
                .context("automation product-gate observation condition is unavailable")?;
            let deadline_authority = observed
                .get("deadline_authority")
                .and_then(Value::as_str)
                .context("automation product-gate observation deadline authority is unavailable")?;
            let deadline_after_origin_ns = observed
                .get("deadline_after_origin_ns")
                .and_then(Value::as_u64)
                .context("automation product-gate observation deadline is unavailable")?;
            if observed.get("observation_index").and_then(Value::as_u64)
                != u64::try_from(observation_index).ok()
                || gate_id != expected_observation.gate_id
                || condition != expected_observation.condition
                || deadline_authority != expected_observation.deadline_authority
                || deadline_after_origin_ns != expected_observation.deadline_after_origin_ns
            {
                bail!("automation product-gate observation differs from its bound command")
            }
            validate_product_gate_id(gate_id)?;
            validate_product_gate_condition(condition)?;
            validate_deadline_authority(deadline_authority)?;
            let outcome = match observed.get("outcome").and_then(Value::as_str) {
                Some("passed") => ProductGateStatus::Passed,
                Some("failed") => ProductGateStatus::Failed,
                _ => bail!("automation product-gate observation outcome is invalid"),
            };
            let condition_met = observed
                .get("condition_met")
                .and_then(Value::as_bool)
                .context("automation product-gate observation condition result is unavailable")?;
            let timed_out = observed
                .get("timed_out")
                .and_then(Value::as_bool)
                .context("automation product-gate observation timeout result is unavailable")?;
            let observed_after_origin_ns = observed
                .get("observed_after_origin_ns")
                .and_then(Value::as_u64)
                .context("automation product-gate observation offset is unavailable")?;
            if observed_after_origin_ns > completed_after_origin_ns {
                bail!("automation product-gate observation occurs after its batch completion")
            }
            match outcome {
                ProductGateStatus::Passed
                    if condition_met
                        && !timed_out
                        && observed_after_origin_ns < deadline_after_origin_ns => {}
                ProductGateStatus::Failed
                    if timed_out && observed_after_origin_ns >= deadline_after_origin_ns => {}
                _ => bail!("automation product-gate observation outcome flags are incoherent"),
            }
            outcomes.push(ProductGateOutcome {
                command_index: expected_batch.command_index,
                batch_id: expected_batch.batch_id.to_owned(),
                phase_id: expected_batch.phase_id.to_owned(),
                observation_index,
                gate_id: gate_id.to_owned(),
                condition: condition.to_owned(),
                deadline_authority: deadline_authority.to_owned(),
                deadline_after_origin_ns,
                origin_kind: expected_batch.origin.kind_label().to_owned(),
                origin_command_index: expected_batch.origin.command_index(),
                outcome,
                condition_met,
                timed_out,
                observed_after_origin_ns,
            });
        }
    }
    Ok(outcomes)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ProductGateOrigin {
    AutomationStarted,
    CommandCompleted(usize),
    ImportPrimaryStarted,
}

impl ProductGateOrigin {
    const fn kind_label(self) -> &'static str {
        match self {
            Self::AutomationStarted => "automation_started",
            Self::CommandCompleted(_) => "command_completed",
            Self::ImportPrimaryStarted => "import_primary_started",
        }
    }

    const fn command_index(self) -> Option<usize> {
        match self {
            Self::CommandCompleted(index) => Some(index),
            Self::AutomationStarted | Self::ImportPrimaryStarted => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ExpectedProductGateObservation<'a> {
    command_index: usize,
    batch_id: &'a str,
    phase_id: &'a str,
    observation_index: usize,
    gate_id: &'a str,
    condition: &'a str,
    deadline_authority: &'a str,
    deadline_after_origin_ns: u64,
    origin: ProductGateOrigin,
}

#[derive(Clone, Debug)]
struct ExpectedProductGateBatch<'a> {
    command_index: usize,
    batch_id: &'a str,
    phase_id: &'a str,
    origin: ProductGateOrigin,
    observations: Vec<ExpectedProductGateObservation<'a>>,
}

fn parse_product_gate_origin(value: &Value) -> anyhow::Result<ProductGateOrigin> {
    let object = value
        .as_object()
        .context("product-gate batch origin must be an object")?;
    match object.get("kind").and_then(Value::as_str) {
        Some("automation_started")
            if object.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == BTreeSet::from(["kind"]) =>
        {
            Ok(ProductGateOrigin::AutomationStarted)
        }
        Some("import_primary_started")
            if object.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == BTreeSet::from(["kind"]) =>
        {
            Ok(ProductGateOrigin::ImportPrimaryStarted)
        }
        Some("command_completed")
            if object.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == BTreeSet::from(["kind", "command_index"]) =>
        {
            let index = object
                .get("command_index")
                .and_then(Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
                .context("product-gate command-completed origin index is invalid")?;
            Ok(ProductGateOrigin::CommandCompleted(index))
        }
        _ => bail!("product-gate batch origin has an invalid exact shape"),
    }
}

fn expected_product_gate_batches(
    commands: &[Value],
) -> anyhow::Result<Vec<ExpectedProductGateBatch<'_>>> {
    let mut expected = Vec::new();
    let mut gate_ids = BTreeSet::new();
    let mut batch_ids = BTreeSet::new();
    for (index, command) in commands.iter().enumerate() {
        let Some(command_name) = command.get("command").and_then(Value::as_str) else {
            continue;
        };
        if matches!(command_name, "observe_gate" | "observe_imported_open_ready") {
            bail!("removed serial product-gate observation command is not accepted")
        }
        if command_name != "observe_gate_batch" {
            continue;
        }
        let object = command
            .as_object()
            .context("product-gate batch command must be a JSON object")?;
        let fields = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
        if fields != BTreeSet::from(["command", "batch_id", "phase_id", "origin", "observations"]) {
            bail!("observe_gate_batch command has the wrong exact field set")
        }
        let batch_id = object
            .get("batch_id")
            .and_then(Value::as_str)
            .context("product-gate batch ID is unavailable")?;
        let phase_id = object
            .get("phase_id")
            .and_then(Value::as_str)
            .context("product-gate phase ID is unavailable")?;
        validate_product_gate_id(batch_id)?;
        validate_product_gate_id(phase_id)?;
        if !batch_ids.insert(batch_id) {
            bail!("product-gate batch IDs must be unique within one script")
        }
        let origin = parse_product_gate_origin(
            object
                .get("origin")
                .context("product-gate batch origin is unavailable")?,
        )?;
        let rows = object
            .get("observations")
            .and_then(Value::as_array)
            .filter(|rows| (1..=PRODUCT_GATE_BATCH_MAX_OBSERVATIONS).contains(&rows.len()))
            .context("product-gate batch observation count is outside 1..=64")?;
        let mut observations = Vec::with_capacity(rows.len());
        for (observation_index, row) in rows.iter().enumerate() {
            let row = row
                .as_object()
                .context("product-gate batch observation must be an object")?;
            if row.keys().map(String::as_str).collect::<BTreeSet<_>>()
                != BTreeSet::from([
                    "gate_id",
                    "deadline_authority",
                    "deadline_after_origin_ns",
                    "target",
                ])
            {
                bail!("product-gate batch observation has the wrong exact field set")
            }
            let gate_id = row
                .get("gate_id")
                .and_then(Value::as_str)
                .context("product-gate observation gate ID is unavailable")?;
            validate_product_gate_id(gate_id)?;
            if !gate_ids.insert(gate_id) {
                bail!("product-gate observation IDs must be unique within one script")
            }
            let deadline_authority = row
                .get("deadline_authority")
                .and_then(Value::as_str)
                .context("product-gate deadline authority is unavailable")?;
            validate_deadline_authority(deadline_authority)?;
            let deadline_after_origin_ns = row
                .get("deadline_after_origin_ns")
                .and_then(Value::as_u64)
                .filter(|deadline| *deadline > 0 && *deadline <= PRODUCT_GATE_DEADLINE_MAX_NS)
                .context("product-gate deadline is outside its fixed bound")?;
            let target = row
                .get("target")
                .and_then(Value::as_object)
                .context("product-gate observation target must be an object")?;
            let condition = match target.get("kind").and_then(Value::as_str) {
                Some("condition")
                    if target.keys().map(String::as_str).collect::<BTreeSet<_>>()
                        == BTreeSet::from(["kind", "condition"]) =>
                {
                    target
                        .get("condition")
                        .and_then(Value::as_str)
                        .context("product-gate condition target is unavailable")?
                }
                Some("imported_open_ready")
                    if target.keys().map(String::as_str).collect::<BTreeSet<_>>()
                        == BTreeSet::from(["kind", "path"]) =>
                {
                    target
                        .get("path")
                        .and_then(Value::as_str)
                        .filter(|path| !path.is_empty())
                        .context("product-gate imported-open-ready path is unavailable")?;
                    IMPORTED_OPEN_READY_CONDITION
                }
                _ => bail!("product-gate observation target has an invalid exact shape"),
            };
            validate_product_gate_condition(condition)?;
            observations.push(ExpectedProductGateObservation {
                command_index: index,
                batch_id,
                phase_id,
                observation_index,
                gate_id,
                condition,
                deadline_authority,
                deadline_after_origin_ns,
                origin,
            });
        }
        expected.push(ExpectedProductGateBatch {
            command_index: index,
            batch_id,
            phase_id,
            origin,
            observations,
        });
    }
    Ok(expected)
}

fn expected_product_gate_observations(
    commands: &[Value],
) -> anyhow::Result<Vec<ExpectedProductGateObservation<'_>>> {
    Ok(expected_product_gate_batches(commands)?
        .into_iter()
        .flat_map(|batch| batch.observations)
        .collect())
}

fn validate_deadline_authority(value: &str) -> anyhow::Result<()> {
    if !matches!(
        value,
        "maximum_current_presentation_gap_plus_poll_grace"
            | "cold_first_useful"
            | "cold_complete_coarse"
            | "cold_target_settlement"
            | "nonresident_target_settlement"
            | "source_verification_completion"
            | "import_primary_wall"
    ) {
        bail!("product-gate deadline authority is not one of the frozen v5 authorities")
    }
    Ok(())
}

fn validate_product_gate_id(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > PRODUCT_GATE_ID_MAX_BYTES
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        bail!("product gate ID is not a bounded path-free identifier")
    }
    Ok(())
}

fn validate_product_gate_condition(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > PRODUCT_GATE_CONDITION_MAX_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        bail!("product gate condition is not a bounded snake-case identifier")
    }
    Ok(())
}

fn validate_device_and_presentation_contract(
    report: &Value,
    profile: &ViewerQualificationProfile,
    reasons: &mut BTreeSet<String>,
) {
    let expected_device_type = match profile.graphics.device_type.as_str() {
        "PHYSICAL_DEVICE_TYPE_DISCRETE_GPU" | "DiscreteGpu" => Some("DiscreteGpu"),
        "PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU" | "IntegratedGpu" => Some("IntegratedGpu"),
        "PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU" | "VirtualGpu" => Some("VirtualGpu"),
        "PHYSICAL_DEVICE_TYPE_CPU" | "Cpu" => Some("Cpu"),
        "PHYSICAL_DEVICE_TYPE_OTHER" | "Other" => Some("Other"),
        _ => None,
    };
    let identity = report.pointer("/final_diagnostics/gpu_adapter/identity");
    let backend_matches = identity
        .and_then(|value| value.get("backend"))
        .and_then(Value::as_str)
        .is_some_and(|backend| backend.eq_ignore_ascii_case(&profile.graphics.backend));
    if identity
        .and_then(|value| value.get("adapter_name"))
        .and_then(Value::as_str)
        != Some(profile.graphics.adapter_name.as_str())
        || !backend_matches
        || identity
            .and_then(|value| value.get("vendor_id"))
            .and_then(Value::as_u64)
            != Some(u64::from(profile.graphics.vendor_id))
        || identity
            .and_then(|value| value.get("device_id"))
            .and_then(Value::as_u64)
            != Some(u64::from(profile.graphics.device_id))
        || identity
            .and_then(|value| value.get("device_type"))
            .and_then(Value::as_str)
            != expected_device_type
        || identity
            .and_then(|value| value.get("driver_name"))
            .and_then(Value::as_str)
            != Some(profile.graphics.driver_name.as_str())
        || identity
            .and_then(|value| value.get("driver_info"))
            .and_then(Value::as_str)
            != Some(profile.graphics.driver_info.as_str())
        || identity
            .and_then(|value| value.get("source"))
            .and_then(Value::as_str)
            != Some("wgpu_adapter_info_for_exact_product_device")
    {
        reasons.insert("automation_report_gpu_adapter_identity_mismatch".to_owned());
    }
    let requested_features = report
        .pointer("/final_diagnostics/gpu_adapter/device_contract/requested_features")
        .and_then(Value::as_array)
        .and_then(|values| {
            values
                .iter()
                .map(Value::as_str)
                .map(|value| value.map(ToOwned::to_owned))
                .collect::<Option<Vec<_>>>()
        });
    if requested_features.as_ref() != Some(&profile.graphics.requested_features)
        || report
            .pointer("/final_diagnostics/gpu_adapter/device_contract/memory_hint")
            .and_then(Value::as_str)
            != Some(profile.graphics.device_memory_hint.as_str())
    {
        reasons.insert("automation_report_gpu_device_contract_mismatch".to_owned());
    }
    if report
        .pointer("/final_diagnostics/render/native_surface_configuration_contract/present_mode")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
        != Some(profile.display.presentation_mode.as_str())
        || report
            .pointer("/final_diagnostics/render/native_surface_configuration_contract/desired_maximum_frame_latency")
            .and_then(Value::as_u64)
            != Some(1)
    {
        reasons.insert("automation_report_presentation_contract_mismatch".to_owned());
    }
}

fn validate_app_build_provenance(
    report: &Value,
    profile: &ViewerQualificationProfile,
    reasons: &mut BTreeSet<String>,
) {
    let provenance = report.get("build_provenance");
    if provenance
        .and_then(|value| value.get("repository_revision"))
        .and_then(Value::as_str)
        != Some(profile.build.repository_revision.as_str())
        || provenance
            .and_then(|value| value.get("profile"))
            .and_then(Value::as_str)
            != Some(profile.build.profile.as_str())
        || provenance
            .and_then(|value| value.get("compiler"))
            .and_then(Value::as_str)
            != Some(profile.build.compiler.as_str())
        || provenance
            .and_then(|value| value.get("target_mode"))
            .and_then(Value::as_str)
            != Some(profile.build.target_mode.as_str())
        || provenance
            .and_then(|value| value.get("opt_level"))
            .and_then(Value::as_str)
            != Some("3")
        || provenance
            .and_then(|value| value.get("debug"))
            .and_then(Value::as_str)
            != Some("false")
        || provenance
            .and_then(|value| value.get("custom_rustflags"))
            .and_then(Value::as_str)
            != Some("false")
        || provenance
            .and_then(|value| value.get("rustc_wrapper"))
            .and_then(Value::as_str)
            != Some("false")
    {
        reasons.insert("automation_report_build_provenance_mismatch".to_owned());
    }
}

fn validate_startup_bootstrap_report(
    report: &Value,
    template: &AutomationScriptTemplate,
    reasons: &mut BTreeSet<String>,
) {
    let observed = report.get("startup_bootstrap");
    let Some(expected) = &template.startup_bootstrap else {
        if !observed.is_some_and(Value::is_null) {
            reasons.insert("undeclared_startup_bootstrap_evidence_present".to_owned());
        }
        return;
    };
    let observed_label_matches = match expected.start_diagnostic_label.as_deref() {
        Some(label) => {
            observed
                .and_then(|value| value.get("start_diagnostic_label"))
                .and_then(Value::as_str)
                == Some(label)
        }
        None => observed
            .and_then(|value| value.get("start_diagnostic_label"))
            .is_some_and(Value::is_null),
    };
    if observed
        .and_then(|value| value.get("qualification_only"))
        .and_then(Value::as_bool)
        != Some(true)
        || observed
            .and_then(|value| value.get("payload_requests_submitted"))
            .and_then(Value::as_bool)
            != Some(false)
        || observed
            .and_then(|value| value.get("intermediate_view_reconciliations"))
            .and_then(Value::as_u64)
            != Some(0)
        || observed
            .and_then(|value| value.get("canonical_commit_reconciliations"))
            .and_then(Value::as_u64)
            != Some(1)
        || observed
            .and_then(|value| value.get("duration_ns"))
            .and_then(Value::as_u64)
            .is_none()
        || observed
            .and_then(|value| value.get("capture_start_checkpoint"))
            .and_then(Value::as_bool)
            != Some(expected.capture_start_checkpoint)
        || observed
            .and_then(|value| value.get("start_checkpoint_captured_in_diagnostics"))
            .and_then(Value::as_bool)
            != Some(expected.capture_start_checkpoint)
        || !observed_label_matches
    {
        reasons.insert("startup_bootstrap_authority_or_start_checkpoint_mismatch".to_owned());
    }
    let bootstrap_work = observed.and_then(|value| value.get("observed_work"));
    let before = bootstrap_work.and_then(|value| value.get("before"));
    let after = bootstrap_work.and_then(|value| value.get("after"));
    let delta = bootstrap_work.and_then(|value| value.get("delta"));
    let mut work_counters_valid = bootstrap_work
        .and_then(|value| value.get("zero_payload_or_demand_work"))
        .and_then(Value::as_bool)
        == Some(true)
        && bootstrap_work
            .and_then(|value| value.get("counter_scope"))
            .and_then(Value::as_str)
            == Some("runtime_source_renderer_and_demand_planner_monotonic_counters");
    for field in [
        "runtime_submitted_requests",
        "runtime_started_decodes",
        "source_physical_range_reads",
        "source_codec_decodes",
        "gpu_uploaded_resources",
        "gpu_uploaded_payload_bytes",
        "gpu_queue_submissions",
        "gpu_frames_executed",
        "demand_jobs_submitted",
        "demand_jobs_completed",
    ] {
        let before = before
            .and_then(|value| value.get(field))
            .and_then(Value::as_u64);
        let after = after
            .and_then(|value| value.get(field))
            .and_then(Value::as_u64);
        let delta = delta
            .and_then(|value| value.get(field))
            .and_then(Value::as_u64);
        work_counters_valid &= matches!(
            (before, after, delta),
            (Some(before), Some(after), Some(0)) if after == before
        );
    }
    if !work_counters_valid {
        reasons.insert("startup_bootstrap_observed_zero_work_invalid".to_owned());
    }
    let expected_commands = expected
        .commands
        .iter()
        .filter_map(|command| command.get("command").and_then(Value::as_str))
        .collect::<Vec<_>>();
    let observed_commands = observed
        .and_then(|value| value.get("commands"))
        .and_then(Value::as_array)
        .map(|commands| {
            commands
                .iter()
                .filter_map(|command| command.get("command").and_then(Value::as_str))
                .collect::<Vec<_>>()
        });
    if observed_commands.as_deref() != Some(expected_commands.as_slice()) {
        reasons.insert("startup_bootstrap_command_evidence_mismatch".to_owned());
    }
    if expected.capture_start_checkpoint {
        let start = expected
            .start_diagnostic_label
            .as_deref()
            .and_then(|label| {
                report
                    .get("diagnostics")
                    .and_then(Value::as_array)
                    .and_then(|samples| {
                        samples.iter().find(|sample| {
                            sample.get("label").and_then(Value::as_str) == Some(label)
                        })
                    })
            });
        if start.is_none() {
            reasons.insert("startup_bootstrap_captured_checkpoint_missing".to_owned());
        }
        for pointer in [
            "/diagnostics/dataset_runtime/counters/submitted_requests",
            "/diagnostics/dataset_runtime/counters/started_decodes",
            "/diagnostics/gpu_adapter/uploads/resources",
            "/diagnostics/gpu_adapter/uploads/payload_bytes",
        ] {
            if start
                .and_then(|value| value.pointer(pointer))
                .and_then(Value::as_u64)
                != Some(0)
            {
                reasons.insert("startup_bootstrap_visible_payload_work_was_not_zero".to_owned());
            }
        }
    }
}

fn optional_u64_field(value: &Value, name: &str) -> bool {
    value
        .get(name)
        .is_some_and(|field| field.is_null() || field.as_u64().is_some())
}

fn validate_resource_policy(
    report: &Value,
    profile: &ViewerQualificationProfile,
    reasons: &mut BTreeSet<String>,
) {
    let cpu_capacity = report
        .pointer("/final_diagnostics/dataset_runtime/capacity/total_cpu_bytes")
        .and_then(Value::as_u64);
    let gpu_budget = report
        .pointer("/final_diagnostics/gpu_adapter/gpu_budget_bytes")
        .and_then(Value::as_u64);
    let gpu_payload = report
        .pointer("/final_diagnostics/gpu_adapter/payload_capacity_bytes")
        .and_then(Value::as_u64);
    let gpu_transfer = report
        .pointer("/final_diagnostics/gpu_adapter/transfer_capacity_bytes")
        .and_then(Value::as_u64);
    if cpu_capacity != Some(profile.resources.max_cpu_total_bytes)
        || gpu_budget != Some(profile.resources.gpu_budget_bytes)
        || gpu_payload != Some(profile.resources.max_gpu_resident_bytes)
        || gpu_transfer != Some(profile.resources.max_gpu_in_flight_bytes)
    {
        reasons.insert("resource_policy_diagnostics_mismatch_or_missing".to_owned());
    }
    let observed_cpu_peak = report
        .pointer("/limit_observations/max_cpu_total_bytes")
        .and_then(Value::as_u64);
    let observed_gpu_peak = report
        .pointer("/final_diagnostics/gpu_adapter/peak_resident_payload_bytes")
        .and_then(Value::as_u64);
    check_resource_peak(
        observed_cpu_peak,
        profile.resources.max_cpu_total_bytes,
        "cpu_resource_peak_missing",
        "cpu_resource_gate_exceeded",
        reasons,
    );
    check_resource_peak(
        report
            .pointer("/limit_observations/max_cpu_decoded_residency_bytes")
            .and_then(Value::as_u64),
        profile.resources.max_cpu_decoded_residency_bytes,
        "cpu_decoded_residency_peak_missing",
        "cpu_decoded_residency_gate_exceeded",
        reasons,
    );
    check_resource_peak(
        report
            .pointer("/limit_observations/max_cpu_upload_staging_bytes")
            .and_then(Value::as_u64),
        profile.resources.max_cpu_upload_staging_bytes,
        "cpu_upload_staging_peak_missing",
        "cpu_upload_staging_gate_exceeded",
        reasons,
    );
    check_resource_peak(
        report
            .pointer("/limit_observations/max_runtime_queued_requests")
            .and_then(Value::as_u64),
        profile.resources.max_queued_requests,
        "runtime_queue_peak_missing",
        "runtime_queue_gate_exceeded",
        reasons,
    );
    check_resource_peak(
        observed_gpu_peak,
        profile.resources.max_gpu_resident_bytes,
        "gpu_resource_peak_missing",
        "gpu_resource_gate_exceeded",
        reasons,
    );
    check_resource_peak(
        report
            .pointer("/final_diagnostics/gpu_adapter/staging/peak_transfer_bytes")
            .and_then(Value::as_u64),
        profile.resources.max_gpu_in_flight_bytes,
        "gpu_transfer_peak_missing",
        "gpu_transfer_gate_exceeded",
        reasons,
    );
    let open_handle_peak = report
        .pointer("/final_diagnostics/dataset_source_io/reader/peak_open_object_handles")
        .and_then(Value::as_u64);
    check_resource_peak(
        open_handle_peak,
        profile.resources.max_open_objects,
        "open_object_peak_missing",
        "open_object_gate_exceeded",
        reasons,
    );
    let gauge =
        report.pointer("/final_diagnostics/dataset_source_io/reader/open_object_handle_gauge");
    let current = gauge
        .and_then(|value| value.get("current"))
        .and_then(Value::as_u64);
    let gauge_peak = gauge
        .and_then(|value| value.get("peak"))
        .and_then(Value::as_u64);
    let retained_current = gauge
        .and_then(|value| value.get("retained_cache_current"))
        .and_then(Value::as_u64);
    let retained_peak = gauge
        .and_then(|value| value.get("retained_cache_peak"))
        .and_then(Value::as_u64);
    if gauge
        .and_then(|value| value.get("available"))
        .and_then(Value::as_bool)
        != Some(true)
        || gauge
            .and_then(|value| value.get("scope"))
            .and_then(Value::as_str)
            != Some("active_reader_root_cached_and_transient_object_descriptors")
        || gauge
            .and_then(|value| value.get("operation_counts_used_as_concurrency"))
            .and_then(Value::as_bool)
            != Some(false)
    {
        reasons.insert("open_object_handle_gauge_contract_missing_or_mismatched".to_owned());
    }
    if gauge_peak != open_handle_peak
        || current.is_none()
        || current
            .zip(gauge_peak)
            .is_none_or(|(current, peak)| current > peak)
        || retained_current.is_none()
        || retained_peak.is_none()
        || retained_current
            .zip(retained_peak)
            .is_none_or(|(current, peak)| current > peak)
    {
        reasons.insert("open_object_handle_gauge_values_missing_or_incoherent".to_owned());
    }
}

fn check_resource_peak(
    observed: Option<u64>,
    maximum: u64,
    missing: &str,
    exceeded: &str,
    reasons: &mut BTreeSet<String>,
) {
    match observed {
        Some(peak) if peak <= maximum => {}
        Some(_) => {
            reasons.insert(exceeded.to_owned());
        }
        None => {
            reasons.insert(missing.to_owned());
        }
    }
}

fn duration_ms_to_ns(value: Option<&Value>) -> Option<u64> {
    let milliseconds = value?.as_f64()?;
    if !milliseconds.is_finite() || milliseconds <= 0.0 {
        return None;
    }
    let nanoseconds = milliseconds * 1_000_000.0;
    (nanoseconds.is_finite() && nanoseconds <= u64::MAX as f64).then(|| nanoseconds.round() as u64)
}

fn nonnegative_duration_ms_to_ns(value: Option<&Value>) -> Option<u64> {
    let milliseconds = value?.as_f64()?;
    if !milliseconds.is_finite() || milliseconds < 0.0 {
        return None;
    }
    let nanoseconds = milliseconds * 1_000_000.0;
    (nanoseconds.is_finite() && nanoseconds <= u64::MAX as f64).then(|| nanoseconds.round() as u64)
}

fn paired_overhead_basis_points(
    instrumented: Option<u64>,
    control: Option<u64>,
    missing_reason: &str,
    reasons: &mut BTreeSet<String>,
) -> Option<u64> {
    let (Some(instrumented), Some(control)) = (instrumented, control) else {
        reasons.insert(missing_reason.to_owned());
        return None;
    };
    if control == 0 {
        reasons.insert(missing_reason.to_owned());
        return None;
    }
    let excess = instrumented.saturating_sub(control);
    let basis_points = u128::from(excess)
        .saturating_mul(10_000)
        .div_ceil(u128::from(control));
    Some(u64::try_from(basis_points).unwrap_or(u64::MAX))
}

fn qualification_gpu_timing_await_wall_ns(
    report: Option<&Value>,
    template: &AutomationScriptTemplate,
    reasons: &mut BTreeSet<String>,
) -> Option<u64> {
    let expected = template
        .commands
        .iter()
        .enumerate()
        .filter(|(_, command)| {
            command.get("command").and_then(Value::as_str) == Some("await_active_view_gpu_timing")
        })
        .collect::<Vec<_>>();
    let Some(events) = report
        .and_then(|report| report.get("events"))
        .and_then(Value::as_array)
    else {
        reasons.insert("qualification_gpu_timing_await_evidence_missing_or_invalid".to_owned());
        return None;
    };
    if events
        .iter()
        .filter(|event| {
            event.get("command").and_then(Value::as_str) == Some("await_active_view_gpu_timing")
        })
        .count()
        != expected.len()
    {
        reasons.insert("qualification_gpu_timing_await_evidence_missing_or_invalid".to_owned());
        return None;
    }
    let parsed_gate_outcomes = product_gate_outcomes_from_report(report?, template).ok();
    let mut total = 0_u64;
    for (command_index, command) in expected {
        let mut matching_events = events.iter().filter(|event| {
            event.get("command_index").and_then(Value::as_u64) == u64::try_from(command_index).ok()
        });
        let Some(event) = matching_events
            .next()
            .filter(|_| matching_events.next().is_none())
        else {
            reasons.insert("qualification_gpu_timing_await_evidence_missing_or_invalid".to_owned());
            return None;
        };
        let event_keys = event
            .as_object()
            .map(|object| object.keys().map(String::as_str).collect::<BTreeSet<_>>());
        let details = event.get("details");
        let detail_keys = details
            .and_then(Value::as_object)
            .map(|object| object.keys().map(String::as_str).collect::<BTreeSet<_>>());
        let available = details
            .and_then(|details| details.get("available"))
            .and_then(Value::as_bool);
        let waited_ns = details
            .and_then(|details| details.get("waited_ns"))
            .and_then(Value::as_u64);
        let waited_ms = details.and_then(|details| details.get("waited_ms"));
        let timeout_ns = command
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .filter(|timeout_ms| *timeout_ms == GPU_TIMING_AWAIT_TIMEOUT_MS)
            .and_then(|timeout_ms| timeout_ms.checked_mul(1_000_000));
        let target = command.get("target").and_then(Value::as_str);
        let pass_kind = command.get("pass_kind").and_then(Value::as_str);
        let expected_panel = match target {
            Some("three_d") => Some("3D"),
            Some("xy") => Some("XY"),
            Some("xz") => Some("XZ"),
            Some("yz") => Some("YZ"),
            _ => None,
        };
        let expected_checkpoint_pass = match pass_kind {
            Some("plane") => Some("Plane"),
            Some("volume") => Some("Volume"),
            _ => None,
        };
        let adjacent_index = command_index.checked_add(1);
        let adjacent_command = adjacent_index.and_then(|index| template.commands.get(index));
        let adjacent_label = adjacent_command
            .filter(|command| {
                command.get("command").and_then(Value::as_str) == Some("sample_diagnostics")
            })
            .and_then(|command| command.get("label"))
            .and_then(Value::as_str);
        let adjacent_event = adjacent_index.and_then(|index| {
            let mut matches = events.iter().filter(|event| {
                event.get("command_index").and_then(Value::as_u64) == u64::try_from(index).ok()
                    && event.get("command").and_then(Value::as_str) == Some("sample_diagnostics")
            });
            matches.next().filter(|_| matches.next().is_none())
        });
        let adjacent_details = adjacent_event.and_then(|event| event.get("details"));
        let matching_diagnostic = adjacent_label.and_then(|label| {
            let mut matches = report
                .and_then(|report| report.get("diagnostics"))
                .and_then(Value::as_array)?
                .iter()
                .filter(|diagnostic| {
                    diagnostic.get("label").and_then(Value::as_str) == Some(label)
                });
            matches.next().filter(|_| matches.next().is_none())
        });
        let checkpoint = adjacent_details.and_then(|details| {
            details.pointer("/diagnostics/render/qualification_gpu_timing_checkpoint")
        });
        let checkpoint_keys = checkpoint
            .and_then(Value::as_object)
            .map(|object| object.keys().map(String::as_str).collect::<BTreeSet<_>>());
        let display_generation = details
            .and_then(|details| details.get("display_generation"))
            .and_then(Value::as_u64);
        let current_presentation_generation =
            details.and_then(|details| details.get("current_presentation_generation"));
        let common_valid = event_keys
            == Some(BTreeSet::from([
                "command_index",
                "command",
                "status",
                "event_epoch_ms",
                "duration_ms",
                "details",
            ]))
            && detail_keys
                == Some(BTreeSet::from([
                    "available",
                    "unavailable_reason",
                    "target",
                    "pass_kind",
                    "display_generation",
                    "current_presentation_generation",
                    "execution_id",
                    "renderer_target",
                    "renderer_frame",
                    "identity_frozen_before_completion",
                    "exact_presented_interval_timing_complete",
                    "unavailable_authority",
                    "waited_ns",
                    "waited_ms",
                ]))
            && checkpoint_keys
                == Some(BTreeSet::from([
                    "available",
                    "derivation",
                    "reason",
                    "presented_interval_sequence",
                    "panel",
                    "execution_id",
                    "target",
                    "display_generation",
                    "current_presentation_generation",
                    "renderer_frame",
                    "pass_kind",
                    "gpu_batch_envelope_ns",
                    "gpu_payload_copy_ns",
                    "gpu_render_pass_ns",
                    "identity_frozen_before_completion",
                    "exact_presented_interval_timing_complete",
                    "unavailable_authority",
                    "waited_ns",
                ]))
            && event.get("command").and_then(Value::as_str) == Some("await_active_view_gpu_timing")
            && event.get("status").and_then(Value::as_str) == Some("passed")
            && event
                .get("event_epoch_ms")
                .and_then(Value::as_u64)
                .is_some()
            && event
                .get("duration_ms")
                .and_then(Value::as_f64)
                .is_some_and(|duration| duration.is_finite() && duration >= 0.0)
            && details
                .and_then(|details| details.get("target"))
                .and_then(Value::as_str)
                == target
            && details
                .and_then(|details| details.get("pass_kind"))
                .and_then(Value::as_str)
                == pass_kind
            && display_generation.is_some()
            && current_presentation_generation
                .is_some_and(|generation| generation.is_null() || generation.as_u64().is_some())
            && waited_ns.is_some_and(|waited_ns| {
                timeout_ns.is_some_and(|timeout_ns| waited_ns <= timeout_ns)
                    && nonnegative_duration_ms_to_ns(waited_ms) == Some(waited_ns)
            })
            && adjacent_event
                .and_then(|event| event.get("status"))
                .and_then(Value::as_str)
                == Some("passed")
            && adjacent_details.is_some_and(|details| Some(details) == matching_diagnostic)
            && checkpoint
                .and_then(|checkpoint| checkpoint.get("panel"))
                .and_then(Value::as_str)
                == expected_panel
            && checkpoint
                .and_then(|checkpoint| checkpoint.get("pass_kind"))
                .and_then(Value::as_str)
                == expected_checkpoint_pass
            && checkpoint.and_then(|checkpoint| checkpoint.get("display_generation"))
                == details.and_then(|details| details.get("display_generation"))
            && checkpoint.and_then(|checkpoint| checkpoint.get("current_presentation_generation"))
                == current_presentation_generation
            && checkpoint
                .and_then(|checkpoint| checkpoint.get("waited_ns"))
                .and_then(Value::as_u64)
                == waited_ns;
        let variant_valid = match available {
            Some(true) => {
                let execution_id = details
                    .and_then(|details| details.get("execution_id"))
                    .and_then(Value::as_u64);
                let renderer_target = details
                    .and_then(|details| details.get("renderer_target"))
                    .and_then(Value::as_u64);
                let renderer_frame = details
                    .and_then(|details| details.get("renderer_frame"))
                    .and_then(Value::as_u64);
                details
                    .and_then(|details| details.get("unavailable_reason"))
                    .is_some_and(Value::is_null)
                    && details
                        .and_then(|details| details.get("unavailable_authority"))
                        .is_some_and(Value::is_null)
                    && details
                        .and_then(|details| details.get("identity_frozen_before_completion"))
                        .and_then(Value::as_bool)
                        == Some(true)
                    && details
                        .and_then(|details| details.get("exact_presented_interval_timing_complete"))
                        .and_then(Value::as_bool)
                        == Some(true)
                    && execution_id.is_some_and(|value| value != 0)
                    && renderer_target.is_some_and(|value| value != 0)
                    && renderer_frame.is_some_and(|value| value != 0)
                    && checkpoint
                        .and_then(|checkpoint| checkpoint.get("available"))
                        .and_then(Value::as_bool)
                        == Some(true)
                    && checkpoint
                        .and_then(|checkpoint| checkpoint.get("derivation"))
                        .and_then(Value::as_str)
                        == Some(
                            "identity_frozen_from_current_execution_then_completed_by_exact_presented_interval_ticket",
                        )
                    && checkpoint
                        .and_then(|checkpoint| checkpoint.get("reason"))
                        .is_some_and(Value::is_null)
                    && checkpoint
                        .and_then(|checkpoint| checkpoint.get("presented_interval_sequence"))
                        .and_then(Value::as_u64)
                        .is_some()
                    && checkpoint
                        .and_then(|checkpoint| checkpoint.get("execution_id"))
                        .and_then(Value::as_u64)
                        == execution_id
                    && checkpoint
                        .and_then(|checkpoint| checkpoint.get("target"))
                        .and_then(Value::as_u64)
                        == renderer_target
                    && checkpoint
                        .and_then(|checkpoint| checkpoint.get("renderer_frame"))
                        .and_then(Value::as_u64)
                        == renderer_frame
                    && checkpoint
                        .and_then(|checkpoint| checkpoint.get("identity_frozen_before_completion"))
                        .and_then(Value::as_bool)
                        == Some(true)
                    && checkpoint
                        .and_then(|checkpoint| {
                            checkpoint.get("exact_presented_interval_timing_complete")
                        })
                        .and_then(Value::as_bool)
                        == Some(true)
                    && checkpoint
                        .and_then(|checkpoint| checkpoint.get("unavailable_authority"))
                        .is_some_and(Value::is_null)
            }
            Some(false) => {
                let authority = parsed_gate_outcomes.as_deref().and_then(|outcomes| {
                    exact_unavailable_gpu_timing_authority(outcomes, command_index)
                });
                let current_is_not_expected = current_presentation_generation
                    .is_some_and(|value| value.is_null() || value.as_u64() != display_generation);
                details
                    .and_then(|details| details.get("unavailable_reason"))
                    .and_then(Value::as_str)
                    == Some(GPU_TIMING_UNAVAILABLE_REASON)
                    && details
                        .and_then(|details| details.get("execution_id"))
                        .is_some_and(Value::is_null)
                    && details
                        .and_then(|details| details.get("renderer_target"))
                        .is_some_and(Value::is_null)
                    && details
                        .and_then(|details| details.get("renderer_frame"))
                        .is_some_and(Value::is_null)
                    && details
                        .and_then(|details| details.get("identity_frozen_before_completion"))
                        .and_then(Value::as_bool)
                        == Some(false)
                    && details
                        .and_then(|details| details.get("exact_presented_interval_timing_complete"))
                        .and_then(Value::as_bool)
                        == Some(false)
                    && authority.is_some()
                    && details.and_then(|details| details.get("unavailable_authority"))
                        == authority.as_ref()
                    && current_is_not_expected
                    && checkpoint
                        .and_then(|checkpoint| checkpoint.get("available"))
                        .and_then(Value::as_bool)
                        == Some(false)
                    && checkpoint
                        .and_then(|checkpoint| checkpoint.get("derivation"))
                        .and_then(Value::as_str)
                        == Some(GPU_TIMING_UNAVAILABLE_DERIVATION)
                    && checkpoint
                        .and_then(|checkpoint| checkpoint.get("reason"))
                        .and_then(Value::as_str)
                        == Some(GPU_TIMING_UNAVAILABLE_REASON)
                    && [
                        "presented_interval_sequence",
                        "execution_id",
                        "target",
                        "renderer_frame",
                        "gpu_batch_envelope_ns",
                        "gpu_payload_copy_ns",
                        "gpu_render_pass_ns",
                    ]
                    .into_iter()
                    .all(|field| {
                        checkpoint
                            .and_then(|checkpoint| checkpoint.get(field))
                            .is_some_and(Value::is_null)
                    })
                    && checkpoint
                        .and_then(|checkpoint| checkpoint.get("identity_frozen_before_completion"))
                        .and_then(Value::as_bool)
                        == Some(false)
                    && checkpoint
                        .and_then(|checkpoint| {
                            checkpoint.get("exact_presented_interval_timing_complete")
                        })
                        .and_then(Value::as_bool)
                        == Some(false)
                    && checkpoint.and_then(|checkpoint| checkpoint.get("unavailable_authority"))
                        == authority.as_ref()
            }
            None => false,
        };
        let Some(waited_ns) = waited_ns.filter(|_| common_valid && variant_valid) else {
            reasons.insert("qualification_gpu_timing_await_evidence_missing_or_invalid".to_owned());
            return None;
        };
        let Some(next) = total.checked_add(waited_ns) else {
            reasons.insert("qualification_gpu_timing_await_evidence_missing_or_invalid".to_owned());
            return None;
        };
        total = next;
    }
    Some(total)
}

fn exact_unavailable_gpu_timing_authority(
    outcomes: &[ProductGateOutcome],
    await_command_index: usize,
) -> Option<Value> {
    let preceding_command_index = await_command_index.checked_sub(1)?;
    let mut coordinated = outcomes.iter().filter(|outcome| {
        outcome.command_index == preceding_command_index
            && outcome.condition == "coordinated_presentation_settled"
    });
    let outcome = coordinated
        .next()
        .filter(|_| coordinated.next().is_none())?;
    if outcome.outcome != ProductGateStatus::Failed || outcome.condition_met || !outcome.timed_out {
        return None;
    }
    Some(json!({
        "command_index": outcome.command_index,
        "batch_id": outcome.batch_id,
        "phase_id": outcome.phase_id,
        "observation_index": outcome.observation_index,
        "gate_id": outcome.gate_id,
        "condition": outcome.condition,
        "deadline_authority": outcome.deadline_authority,
        "deadline_after_origin_ns": outcome.deadline_after_origin_ns,
        "outcome": outcome.outcome.report_label(),
        "condition_met": outcome.condition_met,
        "timed_out": outcome.timed_out,
        "observed_after_origin_ns": outcome.observed_after_origin_ns,
    }))
}

fn cleanup_attempt_package(
    role_root: &Path,
    cleanup: &AttemptCleanup,
) -> anyhow::Result<Option<String>> {
    let relative = cleanup
        .imported_package_relative_path
        .as_deref()
        .context("enabled cleanup lacks its package path")?;
    validate_relative_attempt_path(relative, "cleanup package")?;
    let package = role_root.join(relative);
    if verified_attempt_path_is_absent(role_root, relative)? {
        return Ok(None);
    }
    require_nonsymlink_components(&package, "attempt-local cleanup package")?;
    let canonical_root = fs::canonicalize(role_root)?;
    let canonical_package =
        fs::canonicalize(&package).context("attempt-local cleanup package is unavailable")?;
    if canonical_package == canonical_root || !canonical_package.starts_with(&canonical_root) {
        bail!("attempt-local cleanup package escaped its role root")
    }
    let metadata = fs::symlink_metadata(&canonical_package)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!("attempt-local cleanup target must be a nonsymlink directory")
    }
    let manifest = canonical_package.join("m4d/manifest/root.json");
    let manifest_sha256 = digest_regular_file(&manifest, "attempt-local imported root manifest")?;
    fs::remove_dir_all(&canonical_package)
        .context("failed to remove the explicitly authorized attempt-local imported package")?;
    if canonical_package.exists() {
        bail!("attempt-local imported package remains after cleanup")
    }
    Ok(Some(manifest_sha256))
}

fn verified_attempt_path_is_absent(role_root: &Path, relative: &Path) -> anyhow::Result<bool> {
    let canonical_root = fs::canonicalize(role_root)?;
    let mut cursor = canonical_root;
    for component in relative.components() {
        let Component::Normal(component) = component else {
            bail!("attempt-local cleanup path contains a non-normal component")
        };
        cursor.push(component);
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("attempt-local cleanup path contains a symlink component")
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(error) => return Err(error).context("attempt-local cleanup path is unavailable"),
        }
    }
    Ok(false)
}

fn evaluate_phases(
    profile: &ViewerQualificationProfile,
    numerical_contract: &NumericalContract,
    script: &ScriptScenario,
    oracle: &OracleScenario,
    instrumented: &RoleEvidence,
) -> Vec<PhaseEvaluation> {
    let Some(report) = instrumented.automation_report.as_ref() else {
        return oracle
            .phases
            .iter()
            .map(|phase| PhaseEvaluation {
                name: phase.name.clone(),
                reasons: BTreeSet::from(["phase_automation_report_unavailable".to_owned()]),
            })
            .collect();
    };
    let checkpoints = checkpoint_map(report);
    let Ok(checkpoints) = checkpoints else {
        return oracle
            .phases
            .iter()
            .map(|phase| PhaseEvaluation {
                name: phase.name.clone(),
                reasons: BTreeSet::from(["phase_checkpoint_set_invalid".to_owned()]),
            })
            .collect();
    };
    script
        .phases
        .iter()
        .zip(&oracle.phases)
        .map(|(script_phase, oracle_phase)| {
            evaluate_phase(
                profile,
                numerical_contract,
                script_phase,
                oracle_phase,
                &script.instrumented_script,
                report,
                &checkpoints,
                &instrumented.product_gate_outcomes,
                imported_open_ready_outcome(&instrumented.product_gate_outcomes),
            )
        })
        .collect()
}

fn checkpoint_map(report: &Value) -> anyhow::Result<BTreeMap<&str, &Value>> {
    let samples = report
        .get("diagnostics")
        .and_then(Value::as_array)
        .context("automation diagnostics are unavailable")?;
    let mut checkpoints = BTreeMap::new();
    for sample in samples {
        let Some(label) = sample.get("label").and_then(Value::as_str) else {
            continue;
        };
        if checkpoints.insert(label, sample).is_some() {
            bail!("automation diagnostics contain duplicate labels")
        }
    }
    Ok(checkpoints)
}

#[allow(clippy::too_many_arguments)]
fn evaluate_phase(
    profile: &ViewerQualificationProfile,
    numerical_contract: &NumericalContract,
    script_phase: &ScriptPhase,
    oracle: &OraclePhase,
    template: &AutomationScriptTemplate,
    report: &Value,
    checkpoints: &BTreeMap<&str, &Value>,
    product_gate_outcomes: &[ProductGateOutcome],
    imported_open_ready_outcome: Option<ProductGateStatus>,
) -> PhaseEvaluation {
    let mut reasons = BTreeSet::new();
    let end = checkpoints
        .get(script_phase.end_diagnostic_label.as_str())
        .copied();
    let Some(end) = end else {
        reasons.insert("phase_end_checkpoint_missing".to_owned());
        return PhaseEvaluation {
            name: oracle.name.clone(),
            reasons,
        };
    };
    validate_e1_checkpoint(end, &mut reasons);
    let end_diagnostics = end.get("diagnostics");
    let Some(end_diagnostics) = end_diagnostics else {
        reasons.insert("phase_diagnostics_missing".to_owned());
        return PhaseEvaluation {
            name: oracle.name.clone(),
            reasons,
        };
    };
    let phase_start = script_phase
        .start_diagnostic_label
        .as_deref()
        .and_then(|label| checkpoints.get(label).copied());
    if oracle.require_interaction_metrics {
        match phase_start.and_then(|checkpoint| checkpoint.get("diagnostics")) {
            Some(start_diagnostics) => validate_interaction_metrics(
                start_diagnostics,
                end_diagnostics,
                profile,
                &mut reasons,
            ),
            None => {
                reasons.insert("interaction_phase_start_diagnostics_missing".to_owned());
            }
        }
    }
    validate_phase_script_binding(
        report,
        template,
        script_phase,
        &oracle.phase_state,
        &mut reasons,
    );
    validate_phase_state_facts(
        end_diagnostics,
        &oracle.phase_state,
        numerical_contract,
        &mut reasons,
    );
    if oracle.require_current_complete {
        validate_current_complete(end_diagnostics, &mut reasons);
    }
    if oracle.require_coordinated_layout_complete {
        let coordinated = end_diagnostics
            .pointer("/render/display_coordination/coordinated_visible_layout_current_complete")
            .and_then(Value::as_bool);
        if coordinated.is_none() {
            reasons.insert("coordinated_layout_complete_fact_missing_or_false".to_owned());
        } else if coordinated != Some(true) {
            reasons.insert("product_gate_coordinated_layout_complete_false".to_owned());
        }
    }
    if let Some(scale) = oracle.expected_scale_level {
        validate_scale(end_diagnostics, scale, &mut reasons);
    }
    validate_cross_section_layers(
        end_diagnostics,
        &oracle.expected_cross_section_layers,
        &mut reasons,
    );
    if let Some(gpu_gate) = oracle.gpu_gate {
        let exact_unavailable_authority =
            diagnostic_command_index(&template.commands, &script_phase.end_diagnostic_label)
                .and_then(|end_index| end_index.checked_sub(1))
                .and_then(|await_index| {
                    exact_unavailable_gpu_timing_authority(product_gate_outcomes, await_index)
                });
        validate_gpu_gate(
            end_diagnostics,
            gpu_gate,
            oracle.phase_state.active_view,
            profile,
            exact_unavailable_authority.as_ref(),
            &mut reasons,
        );
    }
    if let Some(settlement_gate) = oracle.settlement_gate {
        validate_settlement_gate(
            end_diagnostics,
            settlement_gate,
            &oracle.phase_state,
            profile,
            &mut reasons,
        );
    }
    match phase_start {
        Some(start) => {
            validate_e1_checkpoint(start, &mut reasons);
            validate_unique_work(
                start,
                end,
                script_phase
                    .start_diagnostic_label
                    .as_deref()
                    .expect("phase start checkpoint was resolved"),
                &script_phase.end_diagnostic_label,
                oracle
                    .unique_work
                    .residency_baseline
                    .as_ref()
                    .and_then(|baseline| {
                        checkpoints.get(baseline.checkpoint_label.as_str()).copied()
                    }),
                &oracle.unique_work,
                &mut reasons,
            );
            if let Some(gate) = &oracle.verification_gate {
                validate_verification_evidence(start, end, gate, &mut reasons);
            }
            if oracle.structural_gate.kind == StructuralGateKind::NonresidentOverlap {
                validate_nonresident_target_residency(
                    start,
                    end,
                    script_phase
                        .start_diagnostic_label
                        .as_deref()
                        .expect("phase start checkpoint was resolved"),
                    oracle
                        .phase_start_target_residency
                        .as_ref()
                        .expect("nonresident oracle validation requires a residency partition"),
                    &mut reasons,
                );
            }
        }
        None => {
            reasons.insert("phase_start_checkpoint_missing_for_unique_work".to_owned());
        }
    }
    if !oracle.zero_work_counters.is_empty() {
        match phase_start {
            Some(start) => {
                validate_e1_checkpoint(start, &mut reasons);
                match start.get("diagnostics") {
                    Some(start_diagnostics) => validate_zero_work(
                        start_diagnostics,
                        end_diagnostics,
                        &oracle.zero_work_counters,
                        oracle.structural_gate.cancellation_waste_authority,
                        &mut reasons,
                    ),
                    None => {
                        reasons.insert("phase_start_diagnostics_missing".to_owned());
                    }
                }
            }
            None => {
                reasons.insert("phase_start_checkpoint_missing".to_owned());
            }
        }
    }
    if let Some(ceilings) = &oracle.structural_gate.ceilings {
        let start = phase_start.and_then(|checkpoint| checkpoint.get("diagnostics"));
        match start {
            Some(start_diagnostics) => {
                validate_structural_ceilings(
                    start_diagnostics,
                    end_diagnostics,
                    oracle.structural_gate.display_batch_authority,
                    oracle.structural_gate.cancellation_waste_authority,
                    ceilings,
                    &mut reasons,
                );
                validate_sequence_commit_events(
                    report,
                    template,
                    script_phase,
                    start_diagnostics,
                    end_diagnostics,
                    ceilings.durable_gesture_commits_per_sequence_exact,
                    &mut reasons,
                );
            }
            None => {
                reasons
                    .insert("phase_start_diagnostics_missing_for_structural_ceilings".to_owned());
            }
        }
    }
    if let Some(minimum) = oracle.minimum_exact_useful_sample_bytes {
        match end
            .pointer("/resource_accounting/exact_cross_scope_union/unique_payload_bytes")
            .and_then(Value::as_u64)
        {
            Some(bytes) if bytes >= minimum => {}
            Some(_) => {
                reasons.insert("exact_useful_sample_bytes_below_oracle".to_owned());
            }
            None => {
                reasons.insert("exact_useful_sample_bytes_unavailable".to_owned());
            }
        }
    }
    if let Some(gate) = &oracle.import_gate {
        validate_import_workflow_gate(report, gate, imported_open_ready_outcome, &mut reasons);
    }
    PhaseEvaluation {
        name: oracle.name.clone(),
        reasons,
    }
}

#[derive(Clone, Copy)]
struct ImportClockEvidence {
    primary_started_at_epoch_ms: u64,
    open_ready_at_epoch_ms: u64,
    published_at_epoch_ms: u64,
    primary_wall_time_ns: u64,
    primary_cpu_time_ns: u64,
}

const IMPORT_INSPECTION_CLOCK_FIELDS: &[&str] = &[
    "start_boundary",
    "end_boundary",
    "wall_clock",
    "cpu_clock",
    "started_at_epoch_ms",
    "start_command_at_epoch_ms",
    "wall_time_ns",
    "process_cpu_time_ns",
    "excluded_from_primary_clock",
    "human_review_interval_included_when_present",
];
const IMPORT_PRIMARY_CLOCK_FIELDS: &[&str] = &[
    "start_boundary",
    "end_boundary",
    "clock",
    "started_at_epoch_ms",
    "open_ready_at_epoch_ms",
    "wall_time_ns",
    "process_cpu_time_ns",
    "inspection_and_human_review_excluded",
    "published_capability_transfer_and_runtime_open_included",
];
const IMPORT_PUBLICATION_TO_OPEN_READY_CLOCK_FIELDS: &[&str] = &[
    "start_boundary",
    "end_boundary",
    "wall_clock",
    "cpu_clock",
    "published_at_epoch_ms",
    "open_ready_at_epoch_ms",
    "wall_time_ns",
    "process_cpu_time_ns",
    "included_in_primary_clock",
    "transfer_mode",
    "publication_currentness_execution",
    "source_verification_started_runs",
    "source_verification_progress_updates",
    "source_verification_cancelled_runs",
    "source_verification_failed_runs",
    "source_verification_successes",
];
const IMPORT_PUBLICATION_CURRENTNESS_EXECUTION_FIELDS: &[&str] = &[
    "contract_id",
    "expected_snapshot_object_reads",
    "first_inventory_object_reads",
    "observed_snapshot_object_reads",
    "second_inventory_object_reads",
    "observed_total_object_reads",
    "observed_codec_decode_calls",
];

fn import_object_has_exact_non_null_fields(value: Option<&Value>, fields: &[&str]) -> bool {
    value.and_then(Value::as_object).is_some_and(|object| {
        object.len() == fields.len()
            && fields
                .iter()
                .all(|field| object.get(*field).is_some_and(|value| !value.is_null()))
    })
}

fn validate_import_workflow_gate(
    report: &Value,
    gate: &ImportGate,
    imported_open_ready_outcome: Option<ProductGateStatus>,
    reasons: &mut BTreeSet<String>,
) {
    let require_open_ready = match imported_open_ready_outcome {
        Some(ProductGateStatus::Passed) => true,
        Some(ProductGateStatus::Failed) => false,
        None => {
            reasons.insert("imported_open_ready_observation_missing".to_owned());
            true
        }
    };
    let Some(workflow) = report
        .get("import_workflow_evidence")
        .filter(|value| value.is_object())
    else {
        reasons.insert("import_workflow_evidence_missing".to_owned());
        return;
    };
    if !require_open_ready
        && workflow.get("primary_clock") == Some(&Value::Null)
        && workflow.get("publication_to_open_ready_clock") == Some(&Value::Null)
        && workflow.get("last_successful_receipt") == Some(&Value::Null)
    {
        validate_prepublication_import_failure(report, workflow, gate, reasons);
        return;
    }
    let expected = &gate.expected;
    let run_facts = (
        import_u64(workflow, "successful_runs"),
        import_u64(workflow, "published_events"),
        import_u64(workflow, "failed_runs"),
        import_u64(workflow, "cancelled_runs"),
        import_u64(workflow, "maximum_resumed_work_units"),
        workflow
            .get("fabricated_global_percentage_or_eta_observed")
            .and_then(Value::as_bool),
    );
    if let (
        Some(successful),
        Some(published),
        Some(failed),
        Some(cancelled),
        Some(resumed),
        Some(fabricated),
    ) = run_facts
    {
        if successful != expected.successful_runs
            || published != expected.published_events
            || failed != expected.failed_runs
            || cancelled != expected.cancelled_runs
            || resumed != expected.resumed_work_units
            || fabricated
        {
            reasons.insert(
                "product_gate_import_workflow_run_counts_or_progress_claim_mismatch".to_owned(),
            );
        }
    } else {
        reasons.insert("import_workflow_run_counts_or_progress_claim_mismatch".to_owned());
    }

    let worker_stages = import_string_set(workflow, "worker_emitted_stage_names");
    let projected_stages = import_string_set(workflow, "projected_named_stage_observations");
    let expected_worker = gate
        .required_worker_stage_names
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let expected_projected = gate
        .required_projected_stage_names
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let progress = import_progress_map(workflow);
    let expected_progress = gate
        .required_progress
        .iter()
        .map(|expectation| {
            (
                expectation.stage.as_str(),
                expectation.minimum_completed_work_units,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let progress_matches = progress.as_ref().is_some_and(|observed| {
        observed.keys().copied().collect::<BTreeSet<_>>() == expected_worker
            && expected_progress.iter().all(|(stage, minimum)| {
                observed
                    .get(stage)
                    .is_some_and(|completed| completed >= minimum)
            })
    });
    let progress_updates = import_u64(workflow, "progress_updates");
    if worker_stages.is_none()
        || projected_stages.is_none()
        || progress.is_none()
        || progress_updates.is_none()
    {
        reasons.insert("import_required_stage_or_progress_evidence_mismatch".to_owned());
    } else if worker_stages.as_ref() != Some(&expected_worker)
        || projected_stages.as_ref() != Some(&expected_projected)
        || !progress_matches
        || progress_updates.is_some_and(|updates| updates < expected.minimum_progress_updates)
    {
        reasons.insert("product_gate_import_required_stage_or_progress_mismatch".to_owned());
    }

    let clocks = if require_open_ready {
        validate_import_clock_evidence(workflow, &gate.limits, reasons)
    } else {
        validate_import_inspection_clock_evidence(workflow, reasons);
        validate_failed_import_open_ready_evidence_shape(workflow, reasons);
        None
    };
    validate_import_publication_currentness(workflow, &gate.publication_currentness, reasons);
    let Some(receipt) = workflow
        .get("last_successful_receipt")
        .filter(|value| value.is_object())
    else {
        reasons.insert("import_successful_receipt_missing".to_owned());
        return;
    };
    validate_import_receipt(
        report,
        workflow,
        receipt,
        gate,
        clocks,
        require_open_ready,
        reasons,
    );
}

const IMPORT_WORKFLOW_EVIDENCE_FIELDS: &[&str] = &[
    "worker_emitted_stage_names",
    "projected_named_stage_observations",
    "maximum_projected_elapsed_ms",
    "maximum_completed_by_stage",
    "progress_updates",
    "published_events",
    "cancelled_runs",
    "successful_runs",
    "failed_runs",
    "maximum_resumed_work_units",
    "maximum_peak_working_bytes",
    "maximum_elapsed_ms",
    "inspection_and_review_clock",
    "primary_clock",
    "publication_to_open_ready_clock",
    "last_successful_receipt",
    "fabricated_global_percentage_or_eta_observed",
];

fn validate_prepublication_import_failure(
    report: &Value,
    workflow: &Value,
    gate: &ImportGate,
    reasons: &mut BTreeSet<String>,
) {
    let inspection = validate_import_inspection_clock_evidence(workflow, reasons);
    let start = unique_passed_event_details(report, "start_reviewed_import");
    let start_shape_is_exact = start.is_some_and(prepublication_import_start_is_exact);
    let start_time_reconciles = inspection
        .zip(start.and_then(|details| import_u64(details, "primary_clock_started_at_epoch_ms")))
        .is_some_and(|((inspection_started, start_command), primary_started)| {
            inspection_started <= start_command && start_command <= primary_started
        });
    let imported_open_ready_matches =
        unique_imported_open_ready_observation(report).is_some_and(|details| {
            imported_open_ready_details_match(
                details,
                ProductGateStatus::Failed,
                gate.limits.maximum_app_primary_wall_time_ns,
            )
        });
    let import_idle = unique_import_batch_observation_status(report, "import_idle");
    let runtime_idle = unique_import_batch_observation_status(report, "runtime_idle");
    let run_counts = (
        import_u64(workflow, "successful_runs"),
        import_u64(workflow, "published_events"),
        import_u64(workflow, "failed_runs"),
        import_u64(workflow, "cancelled_runs"),
    );
    let terminal_worker_failure = run_counts == (Some(0), Some(0), Some(1), Some(0))
        && import_idle == Some(ProductGateStatus::Failed)
        && runtime_idle == Some(ProductGateStatus::Passed);
    let active_at_deadline = run_counts == (Some(0), Some(0), Some(0), Some(0))
        && import_idle == Some(ProductGateStatus::Failed)
        && runtime_idle == Some(ProductGateStatus::Failed);
    let exact_fields = workflow.as_object().is_some_and(|object| {
        object.keys().map(String::as_str).collect::<BTreeSet<_>>()
            == IMPORT_WORKFLOW_EVIDENCE_FIELDS
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
    });
    if !exact_fields
        || !prepublication_progress_shape_is_exact(workflow)
        || workflow.get("primary_clock") != Some(&Value::Null)
        || workflow.get("publication_to_open_ready_clock") != Some(&Value::Null)
        || workflow.get("last_successful_receipt") != Some(&Value::Null)
        || import_u64(workflow, "maximum_resumed_work_units") != Some(0)
        || workflow
            .get("fabricated_global_percentage_or_eta_observed")
            .and_then(Value::as_bool)
            != Some(false)
        || !start_shape_is_exact
        || !start_time_reconciles
        || !imported_open_ready_matches
        || (!terminal_worker_failure && !active_at_deadline)
    {
        reasons.insert("prepublication_import_failure_evidence_shape_invalid".to_owned());
    }
}

fn prepublication_import_start_is_exact(details: &Value) -> bool {
    let Some(details) = details.as_object() else {
        return false;
    };
    let exact_fields = details.keys().map(String::as_str).collect::<BTreeSet<_>>()
        == BTreeSet::from([
            "review_id",
            "destination",
            "operation_token",
            "reviewed_source_fingerprint_sha256",
            "reviewed_source_bytes",
            "working_memory_bytes",
            "primary_clock_started_at_epoch_ms",
            "primary_clock_start_boundary",
            "normal_review_command_path",
        ]);
    let token = details.get("operation_token").and_then(Value::as_object);
    let exact_token = token.is_some_and(|token| {
        token.keys().map(String::as_str).collect::<BTreeSet<_>>()
            == BTreeSet::from([
                "operation_id",
                "task_id",
                "kind",
                "source_session_generation",
                "currentness_generation",
            ])
            && token.get("kind").and_then(Value::as_str) == Some("Import")
            && ["operation_id", "task_id", "source_session_generation"]
            .iter()
            .all(|field| {
                token
                    .get(*field)
                    .and_then(Value::as_u64)
                    .is_some_and(|value| value > 0)
            })
            && token
                .get("currentness_generation")
                .and_then(Value::as_u64)
                .is_some()
    });
    exact_fields
        && import_u64(&Value::Object(details.clone()), "review_id").is_some_and(|value| value > 0)
        && details
            .get("destination")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
        && details
            .get("reviewed_source_fingerprint_sha256")
            .and_then(Value::as_str)
            .is_some_and(|value| require_sha256(value, "IP reviewed-source fingerprint").is_ok())
        && [
            "reviewed_source_bytes",
            "working_memory_bytes",
            "primary_clock_started_at_epoch_ms",
        ]
        .iter()
        .all(|field| {
            details
                .get(*field)
                .and_then(Value::as_u64)
                .is_some_and(|value| value > 0)
        })
        && details
            .get("primary_clock_start_boundary")
            .and_then(Value::as_str)
            == Some("accepted_start_import_command_immediately_before_worker_spawn")
        && details
            .get("normal_review_command_path")
            .and_then(Value::as_bool)
            == Some(true)
        && exact_token
}

fn prepublication_progress_shape_is_exact(workflow: &Value) -> bool {
    let Some(worker_stages) = import_string_set(workflow, "worker_emitted_stage_names") else {
        return false;
    };
    let Some(projected_stages) = import_string_set(workflow, "projected_named_stage_observations")
    else {
        return false;
    };
    let Some(progress_rows) = workflow
        .get("maximum_completed_by_stage")
        .and_then(Value::as_array)
    else {
        return false;
    };
    let rows_are_exact = progress_rows.iter().all(|row| {
        row.as_object().is_some_and(|row| {
            row.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == BTreeSet::from(["stage", "completed_work_units"])
                && row
                    .get("stage")
                    .and_then(Value::as_str)
                    .is_some_and(|stage| !stage.is_empty() && stage.len() <= 128)
                && row
                    .get("completed_work_units")
                    .and_then(Value::as_u64)
                    .is_some()
        })
    });
    let progress_stages = progress_rows
        .iter()
        .filter_map(|row| row.get("stage").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    rows_are_exact
        && progress_stages == worker_stages
        && projected_stages.is_subset(&worker_stages)
        && [
            "maximum_projected_elapsed_ms",
            "progress_updates",
            "maximum_peak_working_bytes",
            "maximum_elapsed_ms",
        ]
        .iter()
        .all(|field| import_u64(workflow, field).is_some())
}

fn validate_failed_import_open_ready_evidence_shape(
    workflow: &Value,
    reasons: &mut BTreeSet<String>,
) {
    let publication = workflow
        .get("publication_to_open_ready_clock")
        .and_then(Value::as_object);
    let pass_only_evidence_present = workflow
        .get("primary_clock")
        .is_some_and(|primary| !primary.is_null())
        || publication.is_some_and(|publication| {
            [
                "start_boundary",
                "end_boundary",
                "wall_clock",
                "cpu_clock",
                "published_at_epoch_ms",
                "open_ready_at_epoch_ms",
                "wall_time_ns",
                "process_cpu_time_ns",
                "included_in_primary_clock",
                "transfer_mode",
            ]
            .iter()
            .any(|field| publication.contains_key(*field))
        });
    if pass_only_evidence_present {
        reasons.insert("failed_imported_open_ready_pass_only_evidence_present".to_owned());
    }

    let primary_is_exact_null = workflow.get("primary_clock") == Some(&Value::Null);
    let publication_has_exact_partial_shape = publication.is_some_and(|publication| {
        publication
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == BTreeSet::from([
                "publication_currentness_execution",
                "source_verification_started_runs",
                "source_verification_progress_updates",
                "source_verification_cancelled_runs",
                "source_verification_failed_runs",
                "source_verification_successes",
            ])
            && publication
                .get("publication_currentness_execution")
                .and_then(Value::as_object)
                .is_some_and(|currentness| {
                    currentness
                        .keys()
                        .map(String::as_str)
                        .collect::<BTreeSet<_>>()
                        == BTreeSet::from([
                            "contract_id",
                            "expected_snapshot_object_reads",
                            "first_inventory_object_reads",
                            "observed_snapshot_object_reads",
                            "second_inventory_object_reads",
                            "observed_total_object_reads",
                            "observed_codec_decode_calls",
                        ])
                })
    });
    if !primary_is_exact_null || !publication_has_exact_partial_shape {
        reasons.insert("failed_imported_open_ready_evidence_shape_invalid".to_owned());
    }
}

fn imported_open_ready_outcome(outcomes: &[ProductGateOutcome]) -> Option<ProductGateStatus> {
    outcomes
        .iter()
        .find(|outcome| outcome.condition == IMPORTED_OPEN_READY_CONDITION)
        .map(|outcome| outcome.outcome)
}

fn import_u64(value: &Value, field: &str) -> Option<u64> {
    value.get(field).and_then(Value::as_u64)
}

fn import_string_set<'a>(value: &'a Value, field: &str) -> Option<BTreeSet<&'a str>> {
    let values = value.get(field)?.as_array()?;
    let result = values
        .iter()
        .map(Value::as_str)
        .collect::<Option<BTreeSet<_>>>()?;
    (result.len() == values.len()).then_some(result)
}

fn import_progress_map(value: &Value) -> Option<BTreeMap<&str, u64>> {
    let values = value.get("maximum_completed_by_stage")?.as_array()?;
    let mut result = BTreeMap::<&str, u64>::new();
    for entry in values {
        let stage = entry.get("stage")?.as_str()?;
        let completed = entry.get("completed_work_units")?.as_u64()?;
        result
            .entry(stage)
            .and_modify(|maximum| *maximum = (*maximum).max(completed))
            .or_insert(completed);
    }
    Some(result)
}

fn validate_import_clock_evidence(
    workflow: &Value,
    limits: &ImportLimits,
    reasons: &mut BTreeSet<String>,
) -> Option<ImportClockEvidence> {
    let (inspection_started_at_epoch_ms, start_command_at_epoch_ms) =
        validate_import_inspection_clock_evidence(workflow, reasons)?;
    let primary_value = workflow.get("primary_clock");
    let publication_value = workflow.get("publication_to_open_ready_clock");
    if !import_object_has_exact_non_null_fields(primary_value, IMPORT_PRIMARY_CLOCK_FIELDS)
        || !import_object_has_exact_non_null_fields(
            publication_value,
            IMPORT_PUBLICATION_TO_OPEN_READY_CLOCK_FIELDS,
        )
        || !import_object_has_exact_non_null_fields(
            publication_value
                .and_then(|publication| publication.get("publication_currentness_execution")),
            IMPORT_PUBLICATION_CURRENTNESS_EXECUTION_FIELDS,
        )
    {
        reasons.insert("import_clock_evidence_shape_invalid".to_owned());
    }
    let primary = primary_value.filter(|value| value.is_object());
    let publication = publication_value.filter(|value| value.is_object());
    let (Some(primary), Some(publication)) = (primary, publication) else {
        reasons.insert("import_clock_evidence_missing".to_owned());
        return None;
    };
    let numeric = (
        import_u64(primary, "started_at_epoch_ms"),
        import_u64(primary, "open_ready_at_epoch_ms"),
        import_u64(primary, "wall_time_ns"),
        import_u64(primary, "process_cpu_time_ns"),
        import_u64(publication, "published_at_epoch_ms"),
        import_u64(publication, "open_ready_at_epoch_ms"),
        import_u64(publication, "wall_time_ns"),
        import_u64(publication, "process_cpu_time_ns"),
    );
    let (
        Some(primary_started_at_epoch_ms),
        Some(open_ready_at_epoch_ms),
        Some(primary_wall_time_ns),
        Some(primary_cpu_time_ns),
        Some(published_at_epoch_ms),
        Some(publication_open_ready_at_epoch_ms),
        Some(publication_wall_time_ns),
        Some(publication_cpu_time_ns),
    ) = numeric
    else {
        reasons.insert("import_clock_evidence_missing".to_owned());
        return None;
    };

    let boundaries_match = primary.get("start_boundary").and_then(Value::as_str)
        == Some("accepted_start_import_command_immediately_before_worker_spawn")
        && primary.get("end_boundary").and_then(Value::as_str)
            == Some("published_destination_verified_and_open_ready_for_normal_product_use")
        && primary.get("clock").and_then(Value::as_str) == Some("std_instant_monotonic")
        && primary
            .get("inspection_and_human_review_excluded")
            .and_then(Value::as_bool)
            == Some(true)
        && primary
            .get("published_capability_transfer_and_runtime_open_included")
            .and_then(Value::as_bool)
            == Some(true)
        && publication.get("start_boundary").and_then(Value::as_str)
            == Some("import_worker_published_event")
        && publication.get("end_boundary").and_then(Value::as_str)
            == Some("published_destination_verified_and_open_ready_for_normal_product_use")
        && publication.get("wall_clock").and_then(Value::as_str) == Some("std_instant_monotonic")
        && publication.get("cpu_clock").and_then(Value::as_str) == Some("process_cpu_time")
        && publication
            .get("included_in_primary_clock")
            .and_then(Value::as_bool)
            == Some(true)
        && publication.get("transfer_mode").and_then(Value::as_str)
            == Some("staged_verified_capability");
    let epochs_ordered = inspection_started_at_epoch_ms <= start_command_at_epoch_ms
        && start_command_at_epoch_ms <= primary_started_at_epoch_ms
        && primary_started_at_epoch_ms <= published_at_epoch_ms
        && published_at_epoch_ms <= open_ready_at_epoch_ms
        && publication_open_ready_at_epoch_ms == open_ready_at_epoch_ms;
    if !boundaries_match
        || !epochs_ordered
        || publication_wall_time_ns > primary_wall_time_ns
        || publication_cpu_time_ns > primary_cpu_time_ns
    {
        reasons.insert("import_clock_boundaries_or_order_mismatch".to_owned());
    }
    if primary_wall_time_ns > limits.maximum_app_primary_wall_time_ns
        || primary_cpu_time_ns > limits.maximum_app_primary_cpu_time_ns
        || publication_wall_time_ns > limits.maximum_publication_to_open_ready_wall_time_ns
        || publication_cpu_time_ns > limits.maximum_publication_to_open_ready_cpu_time_ns
    {
        reasons.insert("import_clock_limit_exceeded".to_owned());
    }
    Some(ImportClockEvidence {
        primary_started_at_epoch_ms,
        open_ready_at_epoch_ms,
        published_at_epoch_ms,
        primary_wall_time_ns,
        primary_cpu_time_ns,
    })
}

fn validate_import_inspection_clock_evidence(
    workflow: &Value,
    reasons: &mut BTreeSet<String>,
) -> Option<(u64, u64)> {
    let inspection_value = workflow.get("inspection_and_review_clock");
    if !import_object_has_exact_non_null_fields(inspection_value, IMPORT_INSPECTION_CLOCK_FIELDS) {
        reasons.insert("import_clock_evidence_shape_invalid".to_owned());
    }
    let Some(inspection) = inspection_value.filter(|value| value.is_object()) else {
        reasons.insert("import_clock_evidence_missing".to_owned());
        return None;
    };
    let numeric = (
        import_u64(inspection, "started_at_epoch_ms"),
        import_u64(inspection, "start_command_at_epoch_ms"),
        import_u64(inspection, "wall_time_ns"),
        import_u64(inspection, "process_cpu_time_ns"),
    );
    let (
        Some(started_at_epoch_ms),
        Some(start_command_at_epoch_ms),
        Some(_wall_time_ns),
        Some(_process_cpu_time_ns),
    ) = numeric
    else {
        reasons.insert("import_clock_evidence_missing".to_owned());
        return None;
    };
    let exact = inspection.get("start_boundary").and_then(Value::as_str)
        == Some("normal_import_setup_command_dispatch")
        && inspection.get("end_boundary").and_then(Value::as_str)
            == Some("reviewed_start_import_command_dispatch")
        && inspection.get("wall_clock").and_then(Value::as_str) == Some("std_instant_monotonic")
        && inspection.get("cpu_clock").and_then(Value::as_str) == Some("process_cpu_time")
        && inspection
            .get("excluded_from_primary_clock")
            .and_then(Value::as_bool)
            == Some(true)
        && inspection
            .get("human_review_interval_included_when_present")
            .and_then(Value::as_bool)
            == Some(true)
        && started_at_epoch_ms <= start_command_at_epoch_ms;
    if !exact {
        reasons.insert("import_clock_boundaries_or_order_mismatch".to_owned());
    }
    Some((started_at_epoch_ms, start_command_at_epoch_ms))
}

fn validate_import_publication_currentness(
    workflow: &Value,
    expected: &ImportPublicationCurrentnessExpectation,
    reasons: &mut BTreeSet<String>,
) {
    let execution =
        workflow.pointer("/publication_to_open_ready_clock/publication_currentness_execution");
    let fields_present = execution.is_some_and(|execution| {
        execution
            .get("contract_id")
            .and_then(Value::as_str)
            .is_some()
            && [
                "expected_snapshot_object_reads",
                "first_inventory_object_reads",
                "observed_snapshot_object_reads",
                "second_inventory_object_reads",
                "observed_total_object_reads",
                "observed_codec_decode_calls",
            ]
            .iter()
            .all(|field| import_u64(execution, field).is_some())
    });
    let authority_matches = execution.is_some_and(|execution| {
        execution.get("contract_id").and_then(Value::as_str) == Some(expected.contract_id.as_str())
            && import_u64(execution, "expected_snapshot_object_reads")
                == Some(expected.expected_snapshot_object_reads)
    });
    let observed_counts_match = execution.is_some_and(|execution| {
        import_u64(execution, "first_inventory_object_reads")
            == Some(expected.first_inventory_object_reads)
            && import_u64(execution, "observed_snapshot_object_reads")
                == Some(expected.observed_snapshot_object_reads)
            && import_u64(execution, "second_inventory_object_reads")
                == Some(expected.second_inventory_object_reads)
            && import_u64(execution, "observed_total_object_reads")
                == Some(expected.observed_total_object_reads)
            && import_u64(execution, "observed_codec_decode_calls")
                == Some(expected.observed_codec_decode_calls)
    });
    if !fields_present || !authority_matches {
        reasons.insert("import_publication_currentness_evidence_mismatch".to_owned());
    } else if !observed_counts_match {
        reasons
            .insert("product_gate_import_publication_currentness_observation_mismatch".to_owned());
    }
    for field in [
        "source_verification_started_runs",
        "source_verification_progress_updates",
        "source_verification_cancelled_runs",
        "source_verification_failed_runs",
        "source_verification_successes",
    ] {
        let observed = workflow
            .pointer("/publication_to_open_ready_clock")
            .and_then(|publication| import_u64(publication, field));
        if observed.is_none() {
            reasons.insert("import_ordinary_source_verifier_activity_observed".to_owned());
        } else if observed != Some(0) {
            reasons.insert(
                "product_gate_import_ordinary_source_verifier_activity_observed".to_owned(),
            );
        }
    }
}

fn validate_import_receipt(
    report: &Value,
    workflow: &Value,
    receipt: &Value,
    gate: &ImportGate,
    clocks: Option<ImportClockEvidence>,
    require_open_ready: bool,
    reasons: &mut BTreeSet<String>,
) {
    validate_import_receipt_binding(
        report,
        receipt,
        clocks,
        require_open_ready,
        gate.limits.maximum_app_primary_wall_time_ns,
        reasons,
    );

    let Some(statistics) = receipt.get("statistics").filter(|value| value.is_object()) else {
        reasons.insert("import_receipt_statistics_missing".to_owned());
        return;
    };
    const REQUIRED_STATISTICS: &[&str] = &[
        "source_bytes_read",
        "source_revalidation_bytes_read",
        "native_decoded_bytes",
        "base_native_decoded_bytes",
        "scientific_identity_native_decoded_bytes",
        "tiff_open_count",
        "native_chunk_decode_count",
        "logical_output_bytes",
        "checkpoint_payload_bytes",
        "checkpoint_journal_bytes",
        "checkpoint_watermark_bytes",
        "checkpoint_durable_work_units",
        "checkpoint_pending_work_units",
        "checkpoint_committed_batches",
        "codec_encode_calls",
        "codec_encode_time_ns",
        "codec_decode_calls",
        "codec_decode_time_ns",
        "sync_calls",
        "sync_time_ns",
        "scientific_brick_reads",
        "staged_structure_object_reads",
        "staged_exact_object_reads",
        "scientific_object_reads",
        "scientific_payload_object_reads",
        "scientific_range_requests",
        "scientific_encoded_bytes_read",
        "scientific_decoded_bytes",
        "object_reads",
        "sampled_peak_open_file_descriptors",
        "open_file_descriptor_structural_bound",
        "peak_open_file_descriptors",
        "preflight_temporary_bytes_bound",
        "peak_temporary_bytes",
        "peak_checkpoint_regular_files",
        "peak_working_bytes",
        "peak_process_rss_bytes",
        "resumed_work_units",
        "produced_work_units",
        "primary_wall_time_ns",
        "primary_cpu_time_ns",
    ];
    if REQUIRED_STATISTICS
        .iter()
        .any(|field| import_u64(statistics, field).is_none())
    {
        reasons.insert("import_receipt_statistics_missing".to_owned());
        return;
    }

    let expected = &gate.expected;
    let exact_counts_match = import_u64(statistics, "checkpoint_durable_work_units")
        == Some(expected.checkpoint_durable_work_units)
        && import_u64(statistics, "checkpoint_pending_work_units")
            == Some(expected.checkpoint_pending_work_units)
        && import_u64(statistics, "resumed_work_units") == Some(expected.resumed_work_units)
        && import_u64(statistics, "produced_work_units") == Some(expected.produced_work_units)
        && import_u64(statistics, "scientific_brick_reads")
            == Some(expected.scientific_brick_reads)
        && import_u64(statistics, "staged_structure_object_reads")
            == Some(expected.staged_structure_object_reads)
        && import_u64(statistics, "staged_exact_object_reads")
            == Some(expected.staged_exact_object_reads)
        && import_u64(statistics, "scientific_object_reads")
            == Some(expected.scientific_object_reads)
        && import_u64(statistics, "scientific_payload_object_reads")
            == Some(expected.scientific_payload_object_reads)
        && import_u64(statistics, "object_reads") == Some(expected.object_reads)
        && import_u64(statistics, "tiff_open_count") == Some(expected.tiff_open_count)
        && import_u64(statistics, "native_chunk_decode_count")
            == Some(expected.native_chunk_decode_count)
        && import_u64(statistics, "peak_checkpoint_regular_files")
            == Some(expected.peak_checkpoint_regular_files);
    if !exact_counts_match {
        reasons.insert("import_receipt_expected_count_mismatch".to_owned());
    }

    let reviewed_source_bytes = import_u64(receipt, "reviewed_source_bytes");
    let source_bytes_read = import_u64(statistics, "source_bytes_read").unwrap_or_default();
    let source_revalidation_bytes =
        import_u64(statistics, "source_revalidation_bytes_read").unwrap_or_default();
    let source_binding_matches = reviewed_source_bytes.is_some_and(|reviewed| {
        reviewed > 0
            && source_revalidation_bytes == reviewed
            && source_revalidation_bytes <= source_bytes_read
    });
    if !source_binding_matches {
        reasons.insert("import_receipt_source_revalidation_mismatch".to_owned());
    }
    if reviewed_source_bytes.is_none_or(|reviewed| reviewed == 0)
        || gate.limits.maximum_source_read_amplification_denominator == 0
    {
        reasons.insert("import_receipt_source_read_amplification_operand_invalid".to_owned());
    } else if !reviewed_source_bytes.is_some_and(|reviewed| {
        u128::from(source_bytes_read)
            * u128::from(gate.limits.maximum_source_read_amplification_denominator)
            <= u128::from(reviewed)
                * u128::from(gate.limits.maximum_source_read_amplification_numerator)
    }) {
        reasons.insert("import_receipt_source_read_amplification_exceeded".to_owned());
    }

    let native_decoded = import_u64(statistics, "native_decoded_bytes").unwrap_or_default();
    let native_by_stage = import_u64(statistics, "base_native_decoded_bytes").and_then(|base| {
        base.checked_add(import_u64(
            statistics,
            "scientific_identity_native_decoded_bytes",
        )?)
    });
    let object_reads = import_u64(statistics, "object_reads").unwrap_or_default();
    let object_reads_by_stage = import_u64(statistics, "staged_structure_object_reads")
        .and_then(|structure| {
            structure.checked_add(import_u64(statistics, "staged_exact_object_reads")?)
        })
        .and_then(|partial| {
            partial.checked_add(import_u64(statistics, "scientific_object_reads")?)
        });
    let sampled_fds =
        import_u64(statistics, "sampled_peak_open_file_descriptors").unwrap_or_default();
    let structural_fds =
        import_u64(statistics, "open_file_descriptor_structural_bound").unwrap_or_default();
    let peak_fds = import_u64(statistics, "peak_open_file_descriptors").unwrap_or_default();
    let durable_work = import_u64(statistics, "checkpoint_durable_work_units").unwrap_or_default();
    let produced_work = import_u64(statistics, "produced_work_units").unwrap_or_default();
    let resumed_work = import_u64(statistics, "resumed_work_units").unwrap_or_default();
    let checkpoint_work_reconciles = produced_work
        .checked_add(resumed_work)
        .is_some_and(|completed| completed == durable_work);
    let receipt_wall_time = import_u64(statistics, "primary_wall_time_ns").unwrap_or_default();
    let receipt_cpu_time = import_u64(statistics, "primary_cpu_time_ns").unwrap_or_default();
    let stage_evidence = import_receipt_stage_evidence(statistics);
    let expected_stages = gate
        .required_receipt_stage_names
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let stages_reconcile = stage_evidence.is_some_and(|(names, wall_time, cpu_time)| {
        names == expected_stages && wall_time <= receipt_wall_time && cpu_time <= receipt_cpu_time
    });
    if !stages_reconcile {
        reasons.insert("import_receipt_stage_evidence_mismatch".to_owned());
    }
    let working_bytes = import_u64(statistics, "peak_working_bytes").unwrap_or_default();
    let core_counters_reconcile = native_by_stage == Some(native_decoded)
        && object_reads_by_stage == Some(object_reads)
        && import_u64(statistics, "scientific_payload_object_reads")
            .zip(import_u64(statistics, "scientific_object_reads"))
            .is_some_and(|(payload, scientific)| payload <= scientific)
        && peak_fds == sampled_fds.max(structural_fds)
        && checkpoint_work_reconciles
        && import_u64(statistics, "sync_time_ns")
            .is_some_and(|sync_time| sync_time <= receipt_wall_time)
        && import_u64(workflow, "maximum_peak_working_bytes") == Some(working_bytes)
        && (!require_open_ready
            || clocks.is_some_and(|clock| {
                receipt_wall_time <= clock.primary_wall_time_ns
                    && receipt_cpu_time <= clock.primary_cpu_time_ns
            }));
    if !core_counters_reconcile {
        reasons.insert("import_receipt_counter_reconciliation_failed".to_owned());
    }

    let preflight_temporary =
        import_u64(statistics, "preflight_temporary_bytes_bound").unwrap_or_default();
    let peak_temporary = import_u64(statistics, "peak_temporary_bytes").unwrap_or_default();
    let peak_process_rss = import_u64(statistics, "peak_process_rss_bytes").unwrap_or_default();
    let sync_calls = import_u64(statistics, "sync_calls").unwrap_or_default();
    let limits = &gate.limits;
    if working_bytes == 0
        || peak_process_rss == 0
        || structural_fds == 0
        || preflight_temporary == 0
        || peak_temporary == 0
        || sync_calls == 0
    {
        reasons.insert("import_receipt_resource_operand_invalid".to_owned());
    } else if working_bytes > limits.maximum_peak_working_bytes
        || peak_process_rss > limits.maximum_peak_process_rss_bytes
        || peak_fds > limits.maximum_product_peak_open_file_descriptors
        || structural_fds > limits.maximum_open_file_descriptor_structural_bound
        || preflight_temporary > limits.maximum_preflight_temporary_bytes_bound
        || peak_temporary > limits.maximum_peak_temporary_bytes
        || peak_temporary > preflight_temporary
        || sync_calls > limits.maximum_sync_calls
    {
        reasons.insert("import_receipt_resource_limit_exceeded".to_owned());
    }
    if receipt_wall_time == 0 || receipt_cpu_time == 0 {
        reasons.insert("import_receipt_primary_clock_operand_invalid".to_owned());
    } else if receipt_wall_time > limits.maximum_receipt_primary_wall_time_ns
        || receipt_cpu_time > limits.maximum_receipt_primary_cpu_time_ns
    {
        reasons.insert("import_receipt_primary_clock_limit_exceeded".to_owned());
    }

    let elapsed_reconciles = import_u64(workflow, "maximum_projected_elapsed_ms")
        .zip(import_u64(workflow, "maximum_elapsed_ms"))
        .is_some_and(|(projected_ms, maximum_ms)| {
            let app_primary_ceiling_ms = clocks.map_or(0, |clock| {
                clock.primary_wall_time_ns.saturating_add(999_999) / 1_000_000
            });
            projected_ms <= maximum_ms
                && receipt_wall_time / 1_000_000 <= maximum_ms
                && (!require_open_ready || maximum_ms <= app_primary_ceiling_ms)
        });
    if !elapsed_reconciles {
        reasons.insert("import_elapsed_evidence_does_not_reconcile".to_owned());
    }
}

fn validate_import_receipt_binding(
    report: &Value,
    receipt: &Value,
    clocks: Option<ImportClockEvidence>,
    require_open_ready: bool,
    import_primary_wall_ns: u64,
    reasons: &mut BTreeSet<String>,
) {
    let destination = receipt
        .get("destination")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let review_id = import_u64(receipt, "review_id").filter(|value| *value > 0);
    let reviewed_source_fingerprint = receipt
        .get("reviewed_source_fingerprint_sha256")
        .and_then(Value::as_str)
        .filter(|fingerprint| {
            require_sha256(fingerprint, "IP receipt reviewed-source fingerprint").is_ok()
        });
    let reviewed_source_bytes =
        import_u64(receipt, "reviewed_source_bytes").filter(|bytes| *bytes > 0);
    let identities_present = ["package_id", "scientific_content_id"].iter().all(|field| {
        receipt
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty() && value.len() <= 256)
    });
    let receipt_token = receipt
        .get("operation_token")
        .filter(|value| value.is_object());
    let token_fields_present = receipt_token.is_some_and(|token| {
        token.get("kind").and_then(Value::as_str) == Some("Import")
            && ["operation_id", "task_id", "source_session_generation"]
            .iter()
            .all(|field| import_u64(token, field).is_some_and(|value| value > 0))
            && import_u64(token, "currentness_generation").is_some()
    });
    let start_details = unique_passed_event_details(report, "start_reviewed_import");
    let open_ready_details = unique_imported_open_ready_observation(report);
    let start_matches = start_details.is_some_and(|details| {
        import_u64(details, "review_id") == review_id
            && details.get("destination").and_then(Value::as_str) == destination
            && details
                .get("reviewed_source_fingerprint_sha256")
                .and_then(Value::as_str)
                == reviewed_source_fingerprint
            && import_u64(details, "reviewed_source_bytes") == reviewed_source_bytes
            && details.get("operation_token") == receipt_token
            && details
                .get("primary_clock_start_boundary")
                .and_then(Value::as_str)
                == Some("accepted_start_import_command_immediately_before_worker_spawn")
            && details
                .get("normal_review_command_path")
                .and_then(Value::as_bool)
                == Some(true)
            && if require_open_ready {
                clocks.is_some_and(|clock| {
                    import_u64(details, "primary_clock_started_at_epoch_ms")
                        == Some(clock.primary_started_at_epoch_ms)
                })
            } else {
                import_u64(details, "primary_clock_started_at_epoch_ms").is_some()
            }
    });
    let expected_outcome = if require_open_ready {
        ProductGateStatus::Passed
    } else {
        ProductGateStatus::Failed
    };
    let open_ready_matches = open_ready_details.is_some_and(|details| {
        imported_open_ready_details_match(details, expected_outcome, import_primary_wall_ns)
    });
    let published_event = receipt
        .get("published_event")
        .filter(|value| value.is_object());
    let publication_matches = published_event.is_some_and(|published| {
        import_u64(published, "process_cpu_time_ns").is_some()
            && if require_open_ready {
                clocks.is_some_and(|clock| {
                    import_u64(published, "published_at_epoch_ms")
                        == Some(clock.published_at_epoch_ms)
                        && clock.primary_started_at_epoch_ms <= clock.published_at_epoch_ms
                        && clock.published_at_epoch_ms <= clock.open_ready_at_epoch_ms
                })
            } else {
                start_details
                    .and_then(|details| import_u64(details, "primary_clock_started_at_epoch_ms"))
                    .zip(import_u64(published, "published_at_epoch_ms"))
                    .is_some_and(|(started, published)| started <= published)
            }
    });
    if destination.is_none()
        || review_id.is_none()
        || reviewed_source_fingerprint.is_none()
        || reviewed_source_bytes.is_none()
        || !identities_present
        || !token_fields_present
        || !start_matches
        || !open_ready_matches
        || !publication_matches
    {
        reasons.insert("import_receipt_start_or_open_ready_binding_mismatch".to_owned());
    }
}

fn imported_open_ready_details_match(
    details: &Value,
    outcome: ProductGateStatus,
    import_primary_wall_ns: u64,
) -> bool {
    let Some(details) = details.as_object() else {
        return false;
    };
    if details.keys().map(String::as_str).collect::<BTreeSet<_>>()
        != BTreeSet::from([
            "observation_index",
            "gate_id",
            "condition",
            "deadline_authority",
            "deadline_after_origin_ns",
            "outcome",
            "condition_met",
            "timed_out",
            "observed_after_origin_ns",
        ])
        || details.get("condition").and_then(Value::as_str) != Some(IMPORTED_OPEN_READY_CONDITION)
        || details.get("deadline_authority").and_then(Value::as_str) != Some("import_primary_wall")
        || details
            .get("deadline_after_origin_ns")
            .and_then(Value::as_u64)
            != Some(import_primary_wall_ns)
        || details.get("outcome").and_then(Value::as_str) != Some(outcome.report_label())
        || details
            .get("gate_id")
            .and_then(Value::as_str)
            .is_none_or(|gate_id| validate_product_gate_id(gate_id).is_err())
    {
        return false;
    }
    let condition_met = details.get("condition_met").and_then(Value::as_bool);
    let timed_out = details.get("timed_out").and_then(Value::as_bool);
    let observed_after_origin_ns = details
        .get("observed_after_origin_ns")
        .and_then(Value::as_u64);
    match outcome {
        ProductGateStatus::Passed => {
            condition_met == Some(true)
                && timed_out == Some(false)
                && observed_after_origin_ns
                    .is_some_and(|observed| observed < import_primary_wall_ns)
        }
        ProductGateStatus::Failed => {
            condition_met.is_some()
                && timed_out == Some(true)
                && observed_after_origin_ns
                    .is_some_and(|observed| observed >= import_primary_wall_ns)
        }
    }
}

fn unique_imported_open_ready_observation(report: &Value) -> Option<&Value> {
    unique_import_batch_observation(report, IMPORTED_OPEN_READY_CONDITION)
}

fn unique_import_batch_observation<'a>(report: &'a Value, condition: &str) -> Option<&'a Value> {
    let mut matches = report
        .get("events")?
        .as_array()?
        .iter()
        .filter(|event| {
            event.get("command").and_then(Value::as_str) == Some("observe_gate_batch")
                && event.get("status").and_then(Value::as_str) == Some("passed")
                && event.pointer("/details/schema").and_then(Value::as_str)
                    == Some(PRODUCT_GATE_OBSERVATION_SCHEMA)
                && event
                    .pointer("/details/origin/kind")
                    .and_then(Value::as_str)
                    == Some("import_primary_started")
        })
        .flat_map(|event| {
            event
                .pointer("/details/observations")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|observation| {
            observation.get("condition").and_then(Value::as_str) == Some(condition)
        });
    let observation = matches.next()?;
    (matches.next().is_none()).then_some(observation)
}

fn unique_import_batch_observation_status(
    report: &Value,
    condition: &str,
) -> Option<ProductGateStatus> {
    match unique_import_batch_observation(report, condition)?
        .get("outcome")?
        .as_str()?
    {
        "passed" => Some(ProductGateStatus::Passed),
        "failed" => Some(ProductGateStatus::Failed),
        _ => None,
    }
}

fn unique_passed_event_details<'a>(report: &'a Value, command: &str) -> Option<&'a Value> {
    let events = report.get("events")?.as_array()?;
    let mut matches = events.iter().filter(|event| {
        event.get("command").and_then(Value::as_str) == Some(command)
            && event.get("status").and_then(Value::as_str) == Some("passed")
    });
    let details = matches.next()?.get("details")?;
    (details.is_object() && matches.next().is_none()).then_some(details)
}

fn import_receipt_stage_evidence(statistics: &Value) -> Option<(BTreeSet<&str>, u64, u64)> {
    let stages = statistics.get("stages")?.as_array()?;
    if stages.is_empty() {
        return None;
    }
    let mut names = BTreeSet::new();
    let mut wall_time = 0_u64;
    let mut cpu_time = 0_u64;
    for stage in stages {
        let name = stage.get("stage")?.as_str()?;
        names.insert(name);
        wall_time = wall_time.checked_add(import_u64(stage, "wall_time_ns")?)?;
        cpu_time = cpu_time.checked_add(import_u64(stage, "cpu_time_ns")?)?;
    }
    Some((names, wall_time, cpu_time))
}

fn validate_e1_checkpoint(checkpoint: &Value, reasons: &mut BTreeSet<String>) {
    if checkpoint
        .pointer("/input_evidence/automation_level")
        .and_then(Value::as_str)
        != Some("E1_semantic_application_commands")
        || checkpoint
            .pointer("/input_evidence/os_input_injected")
            .and_then(Value::as_bool)
            != Some(false)
        || checkpoint
            .pointer("/input_evidence/os_input_claimed")
            .and_then(Value::as_bool)
            != Some(false)
    {
        reasons.insert("e1_input_evidence_missing_or_mislabeled".to_owned());
    }
    let diagnostics = checkpoint.get("diagnostics");
    if diagnostics.and_then(|value| {
        value
            .pointer("/render/display_coordination/os_input_injected")
            .and_then(Value::as_bool)
    }) != Some(false)
        || diagnostics.and_then(|value| {
            value
                .pointer("/render/display_coordination/os_input_claimed")
                .and_then(Value::as_bool)
        }) != Some(false)
    {
        reasons.insert("os_input_claim_boundary_missing".to_owned());
    }
}

fn validate_phase_script_binding(
    report: &Value,
    template: &AutomationScriptTemplate,
    phase: &ScriptPhase,
    expected: &PhaseStateBinding,
    reasons: &mut BTreeSet<String>,
) {
    let Some(end_index) = diagnostic_command_index(&template.commands, &phase.end_diagnostic_label)
    else {
        reasons.insert("phase_state_script_checkpoint_missing".to_owned());
        return;
    };
    let latest = |command_name: &str| {
        template.commands[..end_index]
            .iter()
            .enumerate()
            .rev()
            .find(|(_, command)| {
                command.get("command").and_then(Value::as_str) == Some(command_name)
            })
            .map(|(index, command)| (Some(index), command))
            .or_else(|| {
                template.startup_bootstrap.as_ref().and_then(|bootstrap| {
                    bootstrap.commands.iter().rev().find_map(|command| {
                        (command.get("command").and_then(Value::as_str) == Some(command_name))
                            .then_some((None, command))
                    })
                })
            })
    };
    let mapped = latest("set_mapped_client_pixels");
    if mapped.as_ref().is_none_or(|(_, command)| {
        command.get("width").and_then(Value::as_u64)
            != Some(u64::from(expected.mapped_client_extent.width))
            || command.get("height").and_then(Value::as_u64)
                != Some(u64::from(expected.mapped_client_extent.height))
    }) {
        reasons.insert("phase_mapped_client_extent_script_binding_mismatch".to_owned());
    }
    let render = latest("set_render_target_size").or_else(|| latest("set_four_panel_viewports"));
    if render.as_ref().is_none_or(|(_, command)| {
        let (width, height) =
            if command.get("command").and_then(Value::as_str) == Some("set_four_panel_viewports") {
                (
                    command.get("three_d_render_width"),
                    command.get("three_d_render_height"),
                )
            } else {
                (command.get("width"), command.get("height"))
            };
        width.and_then(Value::as_u64) != Some(u64::from(expected.render_extent.width))
            || height.and_then(Value::as_u64) != Some(u64::from(expected.render_extent.height))
    }) {
        reasons.insert("phase_render_extent_script_binding_mismatch".to_owned());
    }
    if latest("set_viewer_layout")
        .and_then(|(_, command)| command.get("layout"))
        .and_then(Value::as_str)
        != Some(match expected.layout {
            ExpectedViewerLayout::Single3d => "single3d",
            ExpectedViewerLayout::FourPanel => "four_panel",
        })
    {
        reasons.insert("phase_layout_script_binding_mismatch".to_owned());
    }
    let scripted_time_index = latest("set_time_index")
        .and_then(|(_, command)| command.get("time_index"))
        .and_then(Value::as_u64);
    if scripted_time_index.is_some() && scripted_time_index != Some(u64::from(expected.time_index))
        || scripted_time_index.is_none() && expected.time_index != 0
    {
        reasons.insert("phase_time_index_script_binding_mismatch".to_owned());
    }
    if latest("set_projection")
        .or_else(|| latest("set_camera_view"))
        .and_then(|(_, command)| command.get("projection"))
        .and_then(Value::as_str)
        != Some(match expected.camera.projection {
            ExpectedProjection::Orthographic => "orthographic",
            ExpectedProjection::Perspective => "perspective",
        })
    {
        reasons.insert("phase_projection_script_binding_mismatch".to_owned());
    }
    if expected.active_view != ViewerPanel::ThreeD
        && latest("set_active_cross_section_panel")
            .and_then(|(_, command)| command.get("panel"))
            .and_then(Value::as_str)
            != Some(match expected.active_view {
                ViewerPanel::Xy => "xy",
                ViewerPanel::Xz => "xz",
                ViewerPanel::Yz => "yz",
                ViewerPanel::ThreeD => unreachable!("3D has no cross-section panel command"),
            })
    {
        reasons.insert("phase_active_view_script_binding_mismatch".to_owned());
    }
    if let Some((mapped_index, _)) = mapped {
        let later_commands = mapped_index.map_or(&template.commands[..], |index| {
            &template.commands[index + 1..]
        });
        let later_mapped_request = later_commands.iter().any(|command| {
            command.get("command").and_then(Value::as_str) == Some("set_mapped_client_pixels")
        });
        if !later_mapped_request {
            let observed = report.pointer("/viewport_evidence/requested_mapped_client_pixels");
            let width = observed
                .and_then(|value| value.get("width"))
                .and_then(Value::as_u64);
            let height = observed
                .and_then(|value| value.get("height"))
                .and_then(Value::as_u64);
            if width.is_none() || height.is_none() {
                reasons.insert("mapped_client_extent_fact_missing_or_mismatched".to_owned());
            } else if width != Some(u64::from(expected.mapped_client_extent.width))
                || height != Some(u64::from(expected.mapped_client_extent.height))
            {
                reasons.insert("product_gate_mapped_client_extent_mismatch".to_owned());
            }
        }
    }
}

fn validate_phase_state_facts(
    diagnostics: &Value,
    expected: &PhaseStateBinding,
    contract: &NumericalContract,
    reasons: &mut BTreeSet<String>,
) {
    let time_index = diagnostics
        .pointer("/dataset/current_time_index")
        .and_then(Value::as_u64);
    if time_index.is_none() {
        reasons.insert("phase_time_index_fact_missing".to_owned());
    } else if time_index != Some(u64::from(expected.time_index)) {
        reasons.insert("product_gate_phase_time_index_mismatch".to_owned());
    }
    let camera = diagnostics.get("camera");
    let canonical_source = camera
        .and_then(|value| value.get("canonical_source"))
        .and_then(Value::as_str);
    let camera_projection = camera
        .and_then(|value| value.get("projection"))
        .and_then(Value::as_str);
    let render_projection = diagnostics
        .pointer("/render/projection")
        .and_then(Value::as_str);
    if canonical_source != Some("ApplicationSnapshot_ViewState_camera")
        || camera_projection.is_none()
        || render_projection.is_none()
    {
        reasons.insert("canonical_camera_identity_missing_or_mismatched".to_owned());
    } else if camera_projection != Some(expected.camera.projection.report_label())
        || render_projection != Some(expected.camera.projection.report_label())
    {
        reasons.insert("product_gate_canonical_camera_identity_mismatch".to_owned());
    }
    let camera_geometry_valid = camera.is_some_and(|camera| {
        finite_numeric_array(camera.get("target_world"), 3)
            && finite_numeric_array(camera.get("orientation_xyzw"), 4)
            && finite_numeric_field(camera.get("orthographic_world_per_screen_point"))
            && finite_numeric_field(camera.get("perspective_focal_length_screen_points"))
            && finite_numeric_field(camera.get("perspective_view_distance_world"))
    });
    let camera_geometry_matches = camera.is_some_and(|camera| {
        numeric_array_matches(
            camera.get("target_world"),
            &expected.camera.target_world,
            contract.world_position_absolute_tolerance,
            0.0,
        ) && numeric_array_matches(
            camera.get("orientation_xyzw"),
            &expected.camera.orientation_xyzw,
            contract.scalar_absolute_tolerance,
            contract.scalar_relative_tolerance,
        ) && numeric_field_matches(
            camera.get("orthographic_world_per_screen_point"),
            expected.camera.orthographic_world_per_screen_point,
            contract.scalar_absolute_tolerance,
            contract.scalar_relative_tolerance,
        ) && numeric_field_matches(
            camera.get("perspective_focal_length_screen_points"),
            expected.camera.perspective_focal_length_screen_points,
            contract.scalar_absolute_tolerance,
            contract.scalar_relative_tolerance,
        ) && numeric_field_matches(
            camera.get("perspective_view_distance_world"),
            expected.camera.perspective_view_distance_world,
            contract.world_position_absolute_tolerance,
            contract.scalar_relative_tolerance,
        )
    });
    if !camera_geometry_valid {
        reasons.insert("canonical_camera_geometry_missing_or_outside_contract".to_owned());
    } else if !camera_geometry_matches {
        reasons.insert("product_gate_canonical_camera_geometry_outside_contract".to_owned());
    }
    let viewport_width = camera
        .and_then(|value| value.pointer("/viewport/width"))
        .and_then(Value::as_u64);
    let viewport_height = camera
        .and_then(|value| value.pointer("/viewport/height"))
        .and_then(Value::as_u64);
    if viewport_width.is_none() || viewport_height.is_none() {
        reasons.insert("canonical_camera_viewport_missing_or_mismatched".to_owned());
    } else if viewport_width != Some(u64::from(expected.render_extent.width))
        || viewport_height != Some(u64::from(expected.render_extent.height))
    {
        reasons.insert("product_gate_canonical_camera_viewport_mismatch".to_owned());
    }

    let cross = diagnostics.get("cross_section");
    let cross_schema = cross
        .and_then(|value| value.get("schema"))
        .and_then(Value::as_str);
    let cross_version = cross
        .and_then(|value| value.get("schema_version"))
        .and_then(Value::as_u64);
    let cross_layout = cross
        .and_then(|value| value.get("layout"))
        .and_then(Value::as_str);
    if cross_schema != Some("mirante4d-cross-section-panel-diagnostics")
        || cross_version != Some(1)
        || cross_layout.is_none()
    {
        reasons.insert("canonical_cross_section_schema_or_layout_mismatch".to_owned());
    } else if cross_layout != Some(expected.layout.report_label()) {
        reasons.insert("product_gate_canonical_cross_section_schema_or_layout_mismatch".to_owned());
    }
    if expected.active_view != ViewerPanel::ThreeD {
        let observed_active_panel = cross
            .and_then(|value| value.get("active_panel"))
            .and_then(Value::as_str);
        if observed_active_panel.is_none() {
            reasons.insert("canonical_active_view_missing_or_mismatched".to_owned());
        } else if observed_active_panel != Some(expected.active_view.report_label()) {
            reasons.insert("product_gate_canonical_active_view_mismatch".to_owned());
        }
    }
    let linked = cross.and_then(|value| value.get("canonical_linked_view"));
    let linked_valid = linked.is_some_and(|linked| {
        linked.get("source").and_then(Value::as_str).is_some()
            && finite_numeric_array(linked.get("center_world"), 3)
            && finite_numeric_array(linked.get("orientation_xyzw"), 4)
            && finite_numeric_field(linked.get("world_per_screen_point"))
            && finite_numeric_field(linked.get("depth_world"))
    });
    let linked_matches = linked.is_some_and(|linked| {
        linked.get("source").and_then(Value::as_str)
            == Some("ApplicationSnapshot_ViewState_cross_section")
            && numeric_array_matches(
                linked.get("center_world"),
                &expected.cross_section.center_world,
                contract.world_position_absolute_tolerance,
                0.0,
            )
            && numeric_array_matches(
                linked.get("orientation_xyzw"),
                &expected.cross_section.orientation_xyzw,
                contract.scalar_absolute_tolerance,
                contract.scalar_relative_tolerance,
            )
            && numeric_field_matches(
                linked.get("world_per_screen_point"),
                expected.cross_section.world_per_screen_point,
                contract.scalar_absolute_tolerance,
                contract.scalar_relative_tolerance,
            )
            && numeric_field_matches(
                linked.get("depth_world"),
                expected.cross_section.depth_world,
                contract.ray_distance_absolute_tolerance,
                contract.scalar_relative_tolerance,
            )
    });
    if !linked_valid
        || linked
            .and_then(|linked| linked.get("source"))
            .and_then(Value::as_str)
            != Some("ApplicationSnapshot_ViewState_cross_section")
    {
        reasons.insert("canonical_cross_section_geometry_missing_or_outside_contract".to_owned());
    } else if !linked_matches {
        reasons.insert("product_gate_canonical_cross_section_geometry_outside_contract".to_owned());
    }
    validate_canonical_plane_geometry(cross, &expected.cross_section.planes, contract, reasons);
}

fn validate_canonical_plane_geometry(
    cross: Option<&Value>,
    expected: &[ExpectedCrossSectionPlane],
    contract: &NumericalContract,
    reasons: &mut BTreeSet<String>,
) {
    let Some(panels) = cross
        .and_then(|value| value.get("panels"))
        .and_then(Value::as_array)
    else {
        reasons.insert("canonical_cross_section_plane_facts_missing".to_owned());
        return;
    };
    for expected in expected {
        let geometry = panels
            .iter()
            .find(|panel| {
                panel.get("panel_id").and_then(Value::as_str) == Some(expected.panel.report_label())
            })
            .and_then(|panel| panel.get("canonical_plane_geometry"));
        let valid = geometry.is_some_and(|geometry| {
            geometry.get("source").and_then(Value::as_str).is_some()
                && finite_numeric_array(geometry.get("plane_origin_world"), 3)
                && finite_numeric_array(geometry.get("u_axis_world"), 3)
                && finite_numeric_array(geometry.get("v_axis_world"), 3)
                && finite_numeric_array(geometry.get("normal_away_world"), 3)
                && finite_numeric_field(geometry.get("world_per_screen_point"))
        });
        let matches = geometry.is_some_and(|geometry| {
            geometry.get("source").and_then(Value::as_str)
                == Some("canonical_linked_cross_section_view")
                && numeric_array_matches(
                    geometry.get("plane_origin_world"),
                    &expected.plane_origin_world,
                    contract.world_position_absolute_tolerance,
                    0.0,
                )
                && numeric_array_matches(
                    geometry.get("u_axis_world"),
                    &expected.u_axis_world,
                    contract.scalar_absolute_tolerance,
                    contract.scalar_relative_tolerance,
                )
                && numeric_array_matches(
                    geometry.get("v_axis_world"),
                    &expected.v_axis_world,
                    contract.scalar_absolute_tolerance,
                    contract.scalar_relative_tolerance,
                )
                && numeric_array_matches(
                    geometry.get("normal_away_world"),
                    &expected.normal_away_world,
                    contract.scalar_absolute_tolerance,
                    contract.scalar_relative_tolerance,
                )
                && numeric_field_matches(
                    geometry.get("world_per_screen_point"),
                    expected.world_per_screen_point,
                    contract.scalar_absolute_tolerance,
                    contract.scalar_relative_tolerance,
                )
        });
        if !valid
            || geometry
                .and_then(|geometry| geometry.get("source"))
                .and_then(Value::as_str)
                != Some("canonical_linked_cross_section_view")
        {
            reasons.insert(format!(
                "canonical_{}_plane_geometry_missing_or_outside_contract",
                expected.panel.report_label().to_ascii_lowercase()
            ));
        } else if !matches {
            reasons.insert(format!(
                "product_gate_canonical_{}_plane_geometry_outside_contract",
                expected.panel.report_label().to_ascii_lowercase()
            ));
        }
    }
}

fn finite_numeric_array(value: Option<&Value>, expected_len: usize) -> bool {
    value.and_then(Value::as_array).is_some_and(|values| {
        values.len() == expected_len
            && values
                .iter()
                .all(|value| value.as_f64().is_some_and(f64::is_finite))
    })
}

fn finite_numeric_field(value: Option<&Value>) -> bool {
    value.and_then(Value::as_f64).is_some_and(f64::is_finite)
}

fn numeric_array_matches(
    observed: Option<&Value>,
    expected: &[f64],
    absolute_tolerance: f64,
    relative_tolerance: f64,
) -> bool {
    let Some(observed) = observed.and_then(Value::as_array) else {
        return false;
    };
    observed.len() == expected.len()
        && observed.iter().zip(expected).all(|(observed, expected)| {
            numeric_field_matches(
                Some(observed),
                *expected,
                absolute_tolerance,
                relative_tolerance,
            )
        })
}

fn numeric_field_matches(
    observed: Option<&Value>,
    expected: f64,
    absolute_tolerance: f64,
    relative_tolerance: f64,
) -> bool {
    let Some(observed) = observed.and_then(Value::as_f64) else {
        return false;
    };
    if !observed.is_finite() || !expected.is_finite() {
        return false;
    }
    let allowed = absolute_tolerance.max(relative_tolerance * observed.abs().max(expected.abs()));
    (observed - expected).abs() <= allowed
}

fn validate_interaction_metrics(
    start_diagnostics: &Value,
    end_diagnostics: &Value,
    profile: &ViewerQualificationProfile,
    reasons: &mut BTreeSet<String>,
) {
    let start_display = start_diagnostics.pointer("/render/display_coordination");
    let end_display = end_diagnostics.pointer("/render/display_coordination");
    let (Some(start_display), Some(display)) = (start_display, end_display) else {
        reasons.insert("interaction_metrics_missing".to_owned());
        return;
    };
    let admitted_latency = phase_timing_samples(
        start_display,
        display,
        "/admitted_generation_latency",
        "resident_input_latency",
        reasons,
    );
    let presentation_gap = phase_timing_samples(
        start_display,
        display,
        "/active_input_presentation_gap_ns/samples",
        "presentation_gap",
        reasons,
    );
    let main_loop_gap = phase_timing_samples(
        start_display,
        display,
        "/active_input_main_loop_gap_ns/samples",
        "main_loop_gap",
        reasons,
    );
    let ui_update = phase_timing_samples(
        start_display,
        display,
        "/active_ui_update_duration/samples",
        "interaction_task",
        reasons,
    );
    match admitted_latency.as_deref() {
        Some([]) => {
            // An exact empty completion population after a declared resident
            // interaction is an authoritative negative product result: no
            // admitted generation reached current presentation. It is not a
            // missing metric when the bounded ring itself is present and
            // reconciled.
            reasons.insert("resident_input_latency_gate_exceeded".to_owned());
        }
        Some(samples) => check_max_gate(
            sample_p95(samples),
            profile
                .absolute_gates
                .resident_input_to_current_presentation_p95_ns,
            "resident_input_latency_metric_missing",
            "resident_input_latency_gate_exceeded",
            reasons,
        ),
        None => check_max_gate(
            None,
            profile
                .absolute_gates
                .resident_input_to_current_presentation_p95_ns,
            "resident_input_latency_metric_missing",
            "resident_input_latency_gate_exceeded",
            reasons,
        ),
    }
    check_max_gate(
        admitted_latency
            .as_deref()
            .map(phase_maximum_or_zero)
            .into_iter()
            .chain(presentation_gap.as_deref().map(phase_maximum_or_zero))
            .max(),
        profile.absolute_gates.maximum_current_presentation_gap_ns,
        "presentation_gap_metric_missing",
        "presentation_gap_gate_exceeded",
        reasons,
    );
    check_max_gate(
        main_loop_gap.as_deref().map(phase_maximum_or_zero),
        profile.absolute_gates.maximum_main_loop_heartbeat_gap_ns,
        "main_loop_gap_metric_missing",
        "main_loop_gap_gate_exceeded",
        reasons,
    );
    check_max_gate(
        ui_update
            .as_deref()
            .and_then(|samples| samples.iter().copied().max()),
        profile.absolute_gates.maximum_ui_thread_interaction_task_ns,
        "interaction_task_metric_missing",
        "interaction_task_gate_exceeded",
        reasons,
    );
    if display
        .pointer("/active_ui_update_duration/claim_bearing_2ms_gate")
        .and_then(Value::as_bool)
        != Some(true)
    {
        reasons.insert("ui_update_gate_scope_missing".to_owned());
    }
    for update in [
        start_display.get("active_ui_update_duration"),
        display.get("active_ui_update_duration"),
    ] {
        if update
            .and_then(|update| update.get("qualification_only_automation_overhead_excluded"))
            .and_then(Value::as_bool)
            != Some(true)
            || update
                .and_then(|update| update.get("qualification_only_automation_commands_excluded"))
                != Some(&json!([
                    "sample_diagnostics",
                    "copy_diagnostics",
                    "await_active_view_gpu_timing"
                ]))
            || update
                .and_then(|update| update.get("subtraction_method"))
                .and_then(Value::as_str)
                != Some(
                    "saturating_subtract_exact_monotonic_elapsed_interval_from_enclosing_ui_callback",
                )
        {
            reasons
                .insert("ui_update_qualification_automation_exclusion_contract_missing".to_owned());
        }
    }
}

/// An exact empty active-gap population means that every admitted generation
/// became current before another main-loop heartbeat was observed. The
/// maximum active gap is therefore zero; only an absent, malformed, or
/// overwritten ring is missing evidence.
fn phase_maximum_or_zero(samples: &[u64]) -> u64 {
    samples.iter().copied().max().unwrap_or(0)
}

/// Returns only the samples recorded after the start checkpoint. Ring
/// overwrite is a hard evidence failure: a retained-window percentile cannot
/// be substituted for the declared phase population.
fn phase_timing_samples(
    start_display: &Value,
    end_display: &Value,
    pointer: &str,
    label: &str,
    reasons: &mut BTreeSet<String>,
) -> Option<Vec<u64>> {
    let Some(start) = start_display.pointer(pointer) else {
        reasons.insert(format!("{label}_start_sample_ring_missing"));
        return None;
    };
    let Some(end) = end_display.pointer(pointer) else {
        reasons.insert(format!("{label}_end_sample_ring_missing"));
        return None;
    };
    let start_total = start.get("total_count").and_then(Value::as_u64);
    let end_total = end.get("total_count").and_then(Value::as_u64);
    let retained_count = end
        .get("retained_count")
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok());
    let capacity = end
        .get("capacity")
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok());
    let retained = end
        .get("retained_samples_ns_oldest_first")
        .and_then(Value::as_array)
        .and_then(|samples| {
            samples
                .iter()
                .map(Value::as_u64)
                .collect::<Option<Vec<_>>>()
        });
    let (Some(start_total), Some(end_total), Some(retained_count), Some(capacity), Some(retained)) =
        (start_total, end_total, retained_count, capacity, retained)
    else {
        reasons.insert(format!("{label}_sample_ring_invalid"));
        return None;
    };
    if end_total < start_total
        || retained_count > capacity
        || retained.len() != retained_count
        || u64::try_from(retained_count)
            .ok()
            .is_none_or(|count| count > end_total)
    {
        reasons.insert(format!("{label}_sample_ring_invalid"));
        return None;
    }
    let retained_start = end_total.saturating_sub(retained_count as u64);
    if start_total < retained_start {
        reasons.insert(format!("{label}_phase_samples_overwritten"));
        return None;
    }
    let phase_count = end_total - start_total;
    let Some(phase_count) = usize::try_from(phase_count).ok() else {
        reasons.insert(format!("{label}_sample_ring_invalid"));
        return None;
    };
    let Some(offset) = usize::try_from(start_total - retained_start).ok() else {
        reasons.insert(format!("{label}_sample_ring_invalid"));
        return None;
    };
    if offset > retained.len() || retained.len().saturating_sub(offset) != phase_count {
        reasons.insert(format!("{label}_phase_sample_count_mismatch"));
        return None;
    }
    Some(retained[offset..].to_vec())
}

fn sample_p95(samples: &[u64]) -> Option<u64> {
    if samples.is_empty() {
        return None;
    }
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let rank = ordered.len().saturating_mul(95).div_ceil(100);
    ordered.get(rank.saturating_sub(1)).copied()
}

fn check_max_gate(
    observed: Option<u64>,
    maximum: u64,
    missing_reason: &str,
    exceeded_reason: &str,
    reasons: &mut BTreeSet<String>,
) {
    match observed {
        Some(value) if value <= maximum => {}
        Some(_) => {
            reasons.insert(exceeded_reason.to_owned());
        }
        None => {
            reasons.insert(missing_reason.to_owned());
        }
    }
}

fn validate_current_complete(diagnostics: &Value, reasons: &mut BTreeSet<String>) {
    let display = diagnostics.pointer("/render/display_coordination");
    let input_generation = display.and_then(|value| value.get("input_generation"));
    let presentation_generation =
        display.and_then(|value| value.get("current_presentation_generation"));
    match (
        input_generation.and_then(Value::as_u64),
        presentation_generation,
    ) {
        (Some(_), Some(presentation)) if presentation.is_null() => {
            // Explicit null is the product's truthful statement that no
            // current presentation exists. Missing or malformed fields remain
            // integrity failures below.
            reasons.insert("product_gate_current_presentation_generation_mismatch".to_owned());
        }
        (Some(input), Some(presentation)) if presentation.as_u64() == Some(input) => {}
        (Some(_), Some(presentation)) if presentation.as_u64().is_some() => {
            reasons.insert("product_gate_current_presentation_generation_mismatch".to_owned());
        }
        _ => {
            reasons.insert("current_presentation_generation_mismatch_or_missing".to_owned());
        }
    }
    let completeness = diagnostics
        .pointer("/render/frame_fidelity/completeness")
        .and_then(Value::as_str);
    let freshness = diagnostics
        .pointer("/render/frame_fidelity/display_freshness")
        .and_then(Value::as_str);
    let last_failure = diagnostics.pointer("/render/frame_fidelity/last_failure_kind");
    let last_capacity_error = diagnostics.pointer("/render/frame_fidelity/last_capacity_error");
    if completeness.is_none()
        || freshness.is_none()
        || last_failure.is_none()
        || last_capacity_error.is_none()
    {
        reasons.insert("current_complete_fidelity_fact_missing_or_false".to_owned());
    } else if completeness != Some("Complete")
        || freshness != Some("Current")
        || !last_failure.is_some_and(Value::is_null)
        || !last_capacity_error.is_some_and(Value::is_null)
    {
        reasons.insert("product_gate_current_complete_fidelity_false".to_owned());
    }
}

fn validate_scale(diagnostics: &Value, expected: u32, reasons: &mut BTreeSet<String>) {
    let expected = u64::from(expected);
    let optional_scale = |pointer| match diagnostics.pointer(pointer) {
        Some(value) if value.is_null() => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or(()),
        None => Err(()),
    };
    match (
        optional_scale("/render/frame_fidelity/target_scale_level"),
        optional_scale("/render/frame_fidelity/displayed_scale_level"),
    ) {
        (Ok(Some(target)), Ok(Some(displayed))) if target == expected && displayed == expected => {}
        (Ok(_), Ok(_)) => {
            // These are Option<u32> product facts. Explicit null means that
            // the requested scale did not reach presentation; absence or a
            // non-Option-shaped value remains invalid evidence below.
            reasons.insert("product_gate_target_or_displayed_scale_mismatch".to_owned());
        }
        _ => {
            reasons.insert("target_or_displayed_scale_mismatch_or_missing".to_owned());
        }
    }
}

fn validate_cross_section_layers(
    diagnostics: &Value,
    expected: &[ExpectedCrossSectionLayer],
    reasons: &mut BTreeSet<String>,
) {
    if expected.is_empty() {
        return;
    }
    let Some(panels) = diagnostics
        .pointer("/cross_section/panels")
        .and_then(Value::as_array)
    else {
        reasons.insert("cross_section_layer_facts_missing".to_owned());
        return;
    };
    for expected in expected {
        let layer = panels
            .iter()
            .find(|panel| {
                panel.get("panel_id").and_then(Value::as_str) == Some(expected.panel.report_label())
            })
            .and_then(|panel| panel.get("layers"))
            .and_then(Value::as_array)
            .and_then(|layers| {
                layers.iter().find(|layer| {
                    layer.get("layer_ordinal").and_then(Value::as_u64)
                        == u64::try_from(expected.layer_ordinal).ok()
                })
            });
        let Some(layer) = layer else {
            reasons.insert("cross_section_layer_facts_missing".to_owned());
            continue;
        };
        let scale = u64::from(expected.scale_level);
        let expected_scale = layer.get("expected_scale_level").and_then(Value::as_u64);
        let displayed_scale = layer.get("displayed_scale_level").and_then(Value::as_u64);
        let current = layer.get("current").and_then(Value::as_bool);
        let available = layer.get("available_requirements").and_then(Value::as_u64);
        let total = layer.get("total_requirements").and_then(Value::as_u64);
        if expected_scale.is_none()
            || displayed_scale.is_none()
            || current.is_none()
            || available.is_none()
            || total.is_none()
        {
            reasons.insert("cross_section_layer_facts_missing".to_owned());
        } else if expected_scale != Some(scale)
            || displayed_scale != Some(scale)
            || current != Some(true)
            || available != total
        {
            reasons
                .insert("product_gate_cross_section_layer_scale_or_coverage_mismatch".to_owned());
        }
    }
}

fn validate_gpu_gate(
    diagnostics: &Value,
    gate: GpuGate,
    expected_panel: ViewerPanel,
    profile: &ViewerQualificationProfile,
    expected_unavailable_authority: Option<&Value>,
    reasons: &mut BTreeSet<String>,
) {
    let expected_pass_kind = match gate {
        GpuGate::Plane => "Plane",
        GpuGate::Mip | GpuGate::Dvr | GpuGate::Iso => "Volume",
    };
    let expected_generation = diagnostics
        .pointer("/render/display_coordination/input_generation")
        .and_then(Value::as_u64);
    if let Some(valid_unavailable) = validate_unavailable_gpu_timing_checkpoint(
        diagnostics,
        expected_panel,
        expected_pass_kind,
        expected_generation,
        expected_unavailable_authority,
        reasons,
    ) {
        if valid_unavailable {
            reasons.insert(
                "product_gate_gpu_timing_unavailable_without_expected_current_presentation"
                    .to_owned(),
            );
        }
        return;
    }
    let target_fact = diagnostics
        .pointer("/render/display_coordination/detailed_counters/per_target_renderer_facts")
        .and_then(Value::as_array)
        .and_then(|targets| {
            targets.iter().rev().find(|target| {
                target.get("panel").and_then(Value::as_str) == Some(expected_panel.report_label())
                    && target.get("last_execution").is_some_and(|execution| {
                        execution.get("pass_kind").and_then(Value::as_str)
                            == Some(expected_pass_kind)
                            && execution.get("generation").and_then(Value::as_u64)
                                == expected_generation
                    })
            })
        });
    let Some(target_fact) = target_fact else {
        reasons.insert("per_target_gpu_execution_fact_missing_for_current_generation".to_owned());
        return;
    };
    if target_fact.get("panel").and_then(Value::as_str) != Some(expected_panel.report_label()) {
        reasons.insert("per_target_gpu_panel_identity_missing".to_owned());
    }
    let current_execution = target_fact
        .get("last_execution")
        .expect("target selection required a last execution");
    if current_execution.get("gpu_upload_ns").is_some()
        || current_execution.get("gpu_volume_pass_ns").is_some()
    {
        reasons.insert("removed_gpu_timing_alias_present".to_owned());
    }
    if current_execution
        .get("target")
        .and_then(Value::as_u64)
        .is_none_or(|target| target == 0)
        || current_execution.get("generation").and_then(Value::as_u64) != expected_generation
        || current_execution
            .get("renderer_frame")
            .and_then(Value::as_u64)
            .is_none_or(|frame| frame == 0)
        || current_execution.get("pass_kind").and_then(Value::as_str) != Some(expected_pass_kind)
    {
        reasons.insert("per_target_current_execution_identity_missing_or_mismatched".to_owned());
    }

    // The current execution may advance while its asynchronous timestamp is
    // resolving. Qualification freezes the exact current ticket once, then
    // completes that same presented-interval record without requiring a later
    // stable frame. This checkpoint is published atomically with the phase
    // diagnostic and is the GPU gate's timing authority.
    let checkpoint = diagnostics.pointer("/render/qualification_gpu_timing_checkpoint");
    let checkpoint_authoritative = checkpoint.is_some_and(|checkpoint| {
        checkpoint.get("available").and_then(Value::as_bool) == Some(true)
            && checkpoint.get("derivation").and_then(Value::as_str)
                == Some(
                    "identity_frozen_from_current_execution_then_completed_by_exact_presented_interval_ticket",
                )
            && checkpoint
                .get("exact_presented_interval_timing_complete")
                .and_then(Value::as_bool)
                == Some(true)
            && checkpoint.get("panel").and_then(Value::as_str)
                == Some(expected_panel.report_label())
            && checkpoint.get("pass_kind").and_then(Value::as_str)
                == Some(expected_pass_kind)
            && checkpoint
                .get("display_generation")
                .and_then(Value::as_u64)
                == expected_generation
    });
    if !checkpoint_authoritative {
        reasons.insert("qualification_gpu_timing_checkpoint_missing_or_invalid".to_owned());
    }
    let execution_id = checkpoint
        .and_then(|checkpoint| checkpoint.get("execution_id"))
        .and_then(Value::as_u64);
    let target = checkpoint
        .and_then(|checkpoint| checkpoint.get("target"))
        .and_then(Value::as_u64);
    let generation = checkpoint
        .and_then(|checkpoint| checkpoint.get("display_generation"))
        .and_then(Value::as_u64);
    let renderer_frame = checkpoint
        .and_then(|checkpoint| checkpoint.get("renderer_frame"))
        .and_then(Value::as_u64);
    if execution_id.is_none_or(|execution_id| execution_id == 0)
        || target.is_none_or(|target| target == 0)
        || generation != expected_generation
        || renderer_frame.is_none_or(|frame| frame == 0)
    {
        reasons.insert("gpu_timing_ticket_identity_missing_or_mismatched".to_owned());
    }
    let interval = diagnostics
        .pointer("/render/progressive_presentation/presented_frame_intervals/samples")
        .and_then(Value::as_array)
        .and_then(|samples| {
            samples.iter().rev().find(|sample| {
                sample.get("gpu_timing_complete").and_then(Value::as_bool) == Some(true)
                    && sample.get("panel").and_then(Value::as_str)
                        == Some(expected_panel.report_label())
                    && sample.get("gpu_pass_kind").and_then(Value::as_str)
                        == Some(expected_pass_kind)
                    && sample.get("gpu_execution_id").and_then(Value::as_u64) == execution_id
                    && sample.get("gpu_target").and_then(Value::as_u64) == target
                    && sample.get("gpu_generation").and_then(Value::as_u64) == generation
                    && sample.get("gpu_renderer_frame").and_then(Value::as_u64) == renderer_frame
            })
        });
    match interval {
        Some(interval)
            if checkpoint.is_some_and(|checkpoint| {
                [
                    "gpu_batch_envelope_ns",
                    "gpu_payload_copy_ns",
                    "gpu_render_pass_ns",
                ]
                .into_iter()
                .all(|field| checkpoint.get(field) == interval.get(field))
            }) => {}
        Some(_) => {
            reasons.insert("presented_interval_gpu_ticket_identity_mismatch".to_owned());
        }
        None => {
            reasons.insert("presented_interval_gpu_ticket_missing".to_owned());
        }
    }
    let render_pass = checkpoint
        .and_then(|checkpoint| checkpoint.get("gpu_render_pass_ns"))
        .and_then(Value::as_u64);
    let envelope = checkpoint
        .and_then(|checkpoint| checkpoint.get("gpu_batch_envelope_ns"))
        .and_then(Value::as_u64);
    let payload_copy = checkpoint.and_then(|checkpoint| checkpoint.get("gpu_payload_copy_ns"));
    if payload_copy.is_none() {
        reasons.insert("gpu_payload_copy_availability_fact_missing".to_owned());
    } else if !payload_copy.is_some_and(|value| value.is_null() || value.as_u64().is_some()) {
        reasons.insert("gpu_payload_copy_availability_fact_invalid".to_owned());
    }
    match (render_pass, envelope) {
        (Some(render_pass), Some(envelope)) if envelope >= render_pass => {
            if let Some(copy) = payload_copy.and_then(Value::as_u64)
                && envelope < copy
            {
                reasons.insert("gpu_batch_envelope_does_not_contain_copy".to_owned());
            }
        }
        (Some(_), Some(_)) => {
            reasons.insert("gpu_batch_envelope_does_not_contain_pass".to_owned());
        }
        _ => {
            reasons.insert("gpu_batch_envelope_metric_missing".to_owned());
        }
    }
    let maximum = match gate {
        GpuGate::Plane => profile.absolute_gates.maximum_plane_gpu_ns,
        GpuGate::Mip => profile.absolute_gates.maximum_mip_gpu_ns,
        GpuGate::Dvr => profile.absolute_gates.maximum_dvr_gpu_ns,
        GpuGate::Iso => profile.absolute_gates.maximum_iso_gpu_ns,
    };
    check_max_gate(
        envelope,
        maximum,
        "gpu_batch_envelope_metric_missing",
        "gpu_batch_envelope_gate_exceeded",
        reasons,
    );
    let active_mode = diagnostics
        .pointer("/render/active_render_mode")
        .and_then(Value::as_str);
    let expected_mode = match gate {
        GpuGate::Plane => None,
        GpuGate::Mip => Some("Mip"),
        GpuGate::Dvr => Some("Dvr"),
        GpuGate::Iso => Some("Isosurface"),
    };
    if matches!(gate, GpuGate::Plane) && expected_panel == ViewerPanel::ThreeD
        || expected_mode.is_some() && active_mode.is_none()
    {
        reasons.insert("gpu_gate_render_mode_mismatch_or_missing".to_owned());
    } else if expected_mode.is_some() && active_mode != expected_mode {
        reasons.insert("product_gate_gpu_render_mode_mismatch".to_owned());
    }
    if !matches!(gate, GpuGate::Plane) && expected_panel != ViewerPanel::ThreeD {
        reasons.insert("volume_gpu_gate_panel_mismatch".to_owned());
    }
}

fn validate_unavailable_gpu_timing_checkpoint(
    diagnostics: &Value,
    expected_panel: ViewerPanel,
    expected_pass_kind: &str,
    expected_generation: Option<u64>,
    expected_unavailable_authority: Option<&Value>,
    reasons: &mut BTreeSet<String>,
) -> Option<bool> {
    let checkpoint = diagnostics.pointer("/render/qualification_gpu_timing_checkpoint")?;
    if checkpoint.get("available").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    let fields = checkpoint
        .as_object()
        .map(|object| object.keys().map(String::as_str).collect::<BTreeSet<_>>())
        .unwrap_or_default();
    let authority = checkpoint.get("unavailable_authority");
    let authority_fields = authority
        .and_then(Value::as_object)
        .map(|object| object.keys().map(String::as_str).collect::<BTreeSet<_>>())
        .unwrap_or_default();
    let current_presentation =
        diagnostics.pointer("/render/display_coordination/current_presentation_generation");
    let current_is_valid_noncurrent = expected_generation.is_some_and(|expected| {
        current_presentation.is_some_and(|value| {
            value.is_null() || value.as_u64().is_some_and(|current| current != expected)
        })
    });
    let valid = fields
        == BTreeSet::from([
            "available",
            "derivation",
            "reason",
            "presented_interval_sequence",
            "panel",
            "execution_id",
            "target",
            "display_generation",
            "current_presentation_generation",
            "renderer_frame",
            "pass_kind",
            "gpu_batch_envelope_ns",
            "gpu_payload_copy_ns",
            "gpu_render_pass_ns",
            "identity_frozen_before_completion",
            "exact_presented_interval_timing_complete",
            "unavailable_authority",
            "waited_ns",
        ])
        && checkpoint.get("derivation").and_then(Value::as_str)
            == Some(GPU_TIMING_UNAVAILABLE_DERIVATION)
        && checkpoint.get("reason").and_then(Value::as_str) == Some(GPU_TIMING_UNAVAILABLE_REASON)
        && checkpoint.get("panel").and_then(Value::as_str) == Some(expected_panel.report_label())
        && checkpoint.get("pass_kind").and_then(Value::as_str) == Some(expected_pass_kind)
        && checkpoint.get("display_generation").and_then(Value::as_u64) == expected_generation
        && checkpoint.get("current_presentation_generation") == current_presentation
        && current_is_valid_noncurrent
        && [
            "presented_interval_sequence",
            "execution_id",
            "target",
            "renderer_frame",
            "gpu_batch_envelope_ns",
            "gpu_payload_copy_ns",
            "gpu_render_pass_ns",
        ]
        .into_iter()
        .all(|field| checkpoint.get(field).is_some_and(Value::is_null))
        && checkpoint
            .get("identity_frozen_before_completion")
            .and_then(Value::as_bool)
            == Some(false)
        && checkpoint
            .get("exact_presented_interval_timing_complete")
            .and_then(Value::as_bool)
            == Some(false)
        && checkpoint
            .get("waited_ns")
            .and_then(Value::as_u64)
            .is_some_and(|waited| waited <= GPU_TIMING_AWAIT_TIMEOUT_MS * 1_000_000)
        && expected_unavailable_authority.is_some()
        && authority == expected_unavailable_authority
        && authority_fields
            == BTreeSet::from([
                "command_index",
                "batch_id",
                "phase_id",
                "observation_index",
                "gate_id",
                "condition",
                "deadline_authority",
                "deadline_after_origin_ns",
                "outcome",
                "condition_met",
                "timed_out",
                "observed_after_origin_ns",
            ])
        && authority
            .and_then(|authority| authority.get("command_index"))
            .and_then(Value::as_u64)
            .is_some()
        && authority
            .and_then(|authority| authority.get("observation_index"))
            .and_then(Value::as_u64)
            .is_some()
        && authority
            .and_then(|authority| authority.get("batch_id"))
            .and_then(Value::as_str)
            .is_some_and(|value| validate_product_gate_id(value).is_ok())
        && authority
            .and_then(|authority| authority.get("phase_id"))
            .and_then(Value::as_str)
            .is_some_and(|value| validate_product_gate_id(value).is_ok())
        && authority
            .and_then(|authority| authority.get("gate_id"))
            .and_then(Value::as_str)
            .is_some_and(|value| validate_product_gate_id(value).is_ok())
        && authority
            .and_then(|authority| authority.get("condition"))
            .and_then(Value::as_str)
            == Some("coordinated_presentation_settled")
        && authority
            .and_then(|authority| authority.get("deadline_authority"))
            .and_then(Value::as_str)
            .is_some_and(|value| validate_deadline_authority(value).is_ok())
        && authority
            .and_then(|authority| authority.get("deadline_after_origin_ns"))
            .and_then(Value::as_u64)
            .is_some_and(|value| value > 0 && value <= PRODUCT_GATE_DEADLINE_MAX_NS)
        && authority
            .and_then(|authority| authority.get("outcome"))
            .and_then(Value::as_str)
            == Some("failed")
        && authority
            .and_then(|authority| authority.get("condition_met"))
            .and_then(Value::as_bool)
            == Some(false)
        && authority
            .and_then(|authority| authority.get("timed_out"))
            .and_then(Value::as_bool)
            == Some(true)
        && authority
            .and_then(|authority| authority.get("observed_after_origin_ns"))
            .and_then(Value::as_u64)
            .zip(
                authority
                    .and_then(|authority| authority.get("deadline_after_origin_ns"))
                    .and_then(Value::as_u64),
            )
            .is_some_and(|(observed, deadline)| observed >= deadline);
    if !valid {
        reasons.insert("qualification_gpu_timing_checkpoint_missing_or_invalid".to_owned());
    }
    Some(valid)
}

fn validate_settlement_gate(
    diagnostics: &Value,
    gate: SettlementGate,
    expected_state: &PhaseStateBinding,
    profile: &ViewerQualificationProfile,
    reasons: &mut BTreeSet<String>,
) {
    let checks: &[(&str, u64)] = match gate {
        SettlementGate::ColdTarget => &[
            (
                "first_useful_frame_ms",
                profile.absolute_gates.cold_first_useful_ns,
            ),
            (
                "complete_coarse_ms",
                profile.absolute_gates.cold_complete_coarse_ns,
            ),
            (
                "target_settled_ms",
                profile.absolute_gates.cold_target_settlement_ns,
            ),
        ],
        SettlementGate::NonresidentTarget => &[(
            "target_settled_ms",
            profile.absolute_gates.nonresident_target_settlement_ns,
        )],
    };
    let milestones = diagnostics.pointer("/render/performance_milestones");
    let display_generation = diagnostics
        .pointer("/render/display_coordination/input_generation")
        .and_then(Value::as_u64);
    let milestone_scope = milestones
        .and_then(|value| value.get("scope"))
        .and_then(Value::as_str);
    let milestone_generation = milestones
        .and_then(|value| value.get("input_generation"))
        .and_then(Value::as_u64);
    if milestone_scope.is_none()
        || milestone_generation.is_none()
        || display_generation.is_none()
        || milestone_scope != Some("coordinated_visible_layout")
        || milestone_generation != display_generation
    {
        reasons.insert("coordinated_milestone_scope_or_generation_mismatch".to_owned());
    }
    for (field, maximum) in checks {
        check_settlement_gate(
            milestones.and_then(|value| value.get(*field)),
            *maximum,
            "coordinated_settlement_milestone_missing",
            "coordinated_settlement_gate_exceeded",
            reasons,
        );
    }
    let Some(visible_panels) = milestones
        .and_then(|value| value.get("visible_panels"))
        .and_then(Value::as_array)
    else {
        reasons.insert("visible_panel_milestones_missing".to_owned());
        return;
    };
    let observed_panels = visible_panels
        .iter()
        .map(|panel| panel.get("panel").and_then(Value::as_str))
        .collect::<Option<BTreeSet<_>>>();
    let expected_panels = expected_state
        .layout
        .visible_panels()
        .iter()
        .map(|panel| panel.report_label())
        .collect::<BTreeSet<_>>();
    if observed_panels.is_none() {
        reasons.insert("visible_panel_milestones_missing".to_owned());
    } else if observed_panels.as_ref() != Some(&expected_panels)
        || visible_panels.len() != expected_panels.len()
    {
        reasons.insert("product_gate_visible_panel_milestone_set_mismatch".to_owned());
    }
    let expected_layer_ordinals = expected_state
        .layers
        .iter()
        .filter(|layer| layer.visible)
        .map(|layer| u64::from(layer.layer_ordinal))
        .collect::<BTreeSet<_>>();
    for expected_panel in expected_state.layout.visible_panels() {
        let Some(panel) = visible_panels.iter().find(|panel| {
            panel.get("panel").and_then(Value::as_str) == Some(expected_panel.report_label())
        }) else {
            continue;
        };
        for (field, maximum) in checks {
            check_settlement_gate(
                panel.get(*field),
                *maximum,
                "visible_panel_settlement_milestone_missing",
                "visible_panel_settlement_gate_exceeded",
                reasons,
            );
        }
        let overflow = panel.get("visible_layer_overflow").and_then(Value::as_bool);
        if overflow.is_none() {
            reasons.insert("visible_layer_milestone_overflow_or_fact_missing".to_owned());
        } else if overflow != Some(false) {
            reasons.insert("product_gate_visible_layer_milestone_overflow".to_owned());
        }
        let Some(layers) = panel.get("visible_layers").and_then(Value::as_array) else {
            reasons.insert("visible_layer_milestones_missing".to_owned());
            continue;
        };
        let observed_ordinals = layers
            .iter()
            .map(|layer| layer.get("layer_ordinal").and_then(Value::as_u64))
            .collect::<Option<BTreeSet<_>>>();
        if observed_ordinals.is_none() {
            reasons.insert("visible_layer_milestones_missing".to_owned());
        } else if observed_ordinals.as_ref() != Some(&expected_layer_ordinals)
            || layers.len() != expected_layer_ordinals.len()
        {
            reasons.insert("product_gate_visible_layer_milestone_set_mismatch".to_owned());
        }
        for layer in layers {
            for (field, maximum) in checks {
                check_settlement_gate(
                    layer.get(*field),
                    *maximum,
                    "visible_layer_settlement_milestone_missing",
                    "visible_layer_settlement_gate_exceeded",
                    reasons,
                );
            }
        }
    }
}

fn check_settlement_gate(
    observed: Option<&Value>,
    maximum: u64,
    missing_reason: &str,
    exceeded_reason: &str,
    reasons: &mut BTreeSet<String>,
) {
    if observed.is_some_and(Value::is_null) {
        // Milestones are Option<f64> facts. An explicit null is an
        // authoritative statement that the product never reached the
        // milestone, so it is a failed gate rather than missing evidence.
        reasons.insert(exceeded_reason.to_owned());
        return;
    }
    check_max_gate(
        milestone_ns(observed),
        maximum,
        missing_reason,
        exceeded_reason,
        reasons,
    );
}

fn milestone_ns(value: Option<&Value>) -> Option<u64> {
    let milliseconds = value?.as_f64()?;
    if !milliseconds.is_finite() || milliseconds < 0.0 {
        return None;
    }
    let nanoseconds = milliseconds * 1_000_000.0;
    (nanoseconds.is_finite() && nanoseconds <= u64::MAX as f64).then(|| nanoseconds.round() as u64)
}

fn validate_observed_exact_resource_union<'a>(
    checkpoint: &'a Value,
    checkpoint_label: &str,
    position: &str,
    expected: &ExactResourceUnion,
    reasons: &mut BTreeSet<String>,
) -> Option<&'a Value> {
    let union = checkpoint.pointer("/resource_accounting/exact_cross_scope_union");
    if union
        .and_then(|value| value.get("available"))
        .and_then(Value::as_bool)
        != Some(true)
        || union
            .and_then(|value| value.get("raw_keys_serialized"))
            .and_then(Value::as_bool)
            != Some(false)
        || union
            .and_then(|value| value.get("derivation"))
            .and_then(Value::as_str)
            != Some(
                "DatasetCatalog_resource_payload_descriptor_for_sorted_deduplicated_visible_prepared_scope_keys",
            )
        || union
            .and_then(|value| value.get("canonical_entries_sha256_derivation"))
            .and_then(Value::as_str)
            != Some("sha256_domain_mirante4d_ep00_resource_union_v1_sorted_binary_le")
        || union
            .and_then(|value| value.get("label"))
            .and_then(Value::as_str)
            != Some(checkpoint_label)
    {
        reasons.insert(format!(
            "exact_cross_scope_{position}_union_authority_missing_or_mismatched"
        ));
    }
    let canonical_entries = union
        .and_then(|union| union.get("canonical_entries_sha256"))
        .and_then(Value::as_str)
        .filter(|digest| require_sha256(digest, "observed exact resource union").is_ok());
    if canonical_entries.is_none() {
        reasons.insert(format!(
            "exact_cross_scope_{position}_union_canonical_entries_sha256_mismatch_or_missing"
        ));
    } else if canonical_entries != Some(expected.canonical_entries_sha256.as_str()) {
        reasons.insert(format!(
            "product_gate_exact_cross_scope_{position}_union_canonical_entries_sha256_mismatch"
        ));
    }
    for (field, value) in [
        ("unique_keys", expected.unique_keys),
        ("unique_payload_bytes", expected.unique_payload_bytes),
        (
            "summed_scope_payload_bytes",
            expected.summed_scope_payload_bytes,
        ),
    ] {
        let observed = union
            .and_then(|union| union.get(field))
            .and_then(Value::as_u64);
        if observed.is_none() {
            reasons.insert(format!(
                "exact_cross_scope_{position}_union_{field}_mismatch_or_missing"
            ));
        } else if observed != Some(value) {
            reasons.insert(format!(
                "product_gate_exact_cross_scope_{position}_union_{field}_mismatch"
            ));
        }
    }
    union
}

fn validate_unique_work(
    start_checkpoint: &Value,
    end_checkpoint: &Value,
    start_label: &str,
    end_label: &str,
    residency_baseline_checkpoint: Option<&Value>,
    expected: &UniqueWorkExpectation,
    reasons: &mut BTreeSet<String>,
) {
    let start_union = validate_observed_exact_resource_union(
        start_checkpoint,
        start_label,
        "start",
        &expected.start_union,
        reasons,
    );
    let end_union = validate_observed_exact_resource_union(
        end_checkpoint,
        end_label,
        "target",
        &expected.target_union,
        reasons,
    );
    if let Some(baseline) = &expected.residency_baseline {
        if let Some(checkpoint) = residency_baseline_checkpoint {
            validate_observed_exact_resource_union(
                checkpoint,
                &baseline.checkpoint_label,
                "residency_baseline",
                &baseline.union,
                reasons,
            );
        } else {
            reasons.insert("exact_residency_baseline_checkpoint_missing".to_owned());
        }
    }
    let delta = end_union.and_then(|union| union.get("delta_from_previous_label"));
    // These fields bind the partition evidence to the two checkpoints and its
    // derivation.  A missing or different value makes the evidence
    // unauthoritative rather than proving a product regression.
    for (field, value) in [
        ("previous_label", Some(start_label)),
        (
            "previous_union_sha256",
            start_union
                .and_then(|union| union.get("canonical_entries_sha256"))
                .and_then(Value::as_str)
                .filter(|digest| {
                    require_sha256(digest, "observed start exact resource union").is_ok()
                }),
        ),
        ("current_label", Some(end_label)),
        (
            "current_union_sha256",
            end_union
                .and_then(|union| union.get("canonical_entries_sha256"))
                .and_then(Value::as_str)
                .filter(|digest| {
                    require_sha256(digest, "observed target exact resource union").is_ok()
                }),
        ),
        (
            "partition_derivation",
            Some("sorted_DatasetResourceKey_payload_descriptor_three_way_merge"),
        ),
    ] {
        let observed = delta
            .and_then(|delta| delta.get(field))
            .and_then(Value::as_str);
        if value.is_none() || observed != value {
            reasons.insert(format!(
                "exact_resource_union_delta_{field}_mismatch_or_missing"
            ));
        }
    }
    // Once the digest fact is well formed, a different exact partition is a
    // product-oracle failure.  Absence or a non-string value remains an
    // integrity failure.
    for (field, value) in [
        (
            "retained_entries_sha256",
            expected.delta_union.retained_entries_sha256.as_str(),
        ),
        (
            "added_entries_sha256",
            expected.delta_union.added_entries_sha256.as_str(),
        ),
        (
            "removed_entries_sha256",
            expected.delta_union.removed_entries_sha256.as_str(),
        ),
    ] {
        let observed = delta
            .and_then(|delta| delta.get(field))
            .and_then(Value::as_str);
        if observed.is_none() {
            reasons.insert(format!(
                "exact_resource_union_delta_{field}_mismatch_or_missing"
            ));
        } else if observed != Some(value) {
            reasons.insert(format!(
                "product_gate_exact_resource_union_delta_{field}_mismatch"
            ));
        }
    }
    let partitions_pairwise_disjoint = delta
        .and_then(|delta| delta.get("partitions_pairwise_disjoint"))
        .and_then(Value::as_bool);
    if partitions_pairwise_disjoint.is_none() {
        reasons.insert(
            "exact_resource_union_delta_partitions_pairwise_disjoint_mismatch_or_missing"
                .to_owned(),
        );
    } else if partitions_pairwise_disjoint
        != Some(expected.delta_union.partitions_pairwise_disjoint)
    {
        reasons.insert(
            "product_gate_exact_resource_union_delta_partitions_pairwise_disjoint_mismatch"
                .to_owned(),
        );
    }
    let retained_payload_bytes_match = delta
        .and_then(|delta| delta.get("retained_payload_bytes_match"))
        .and_then(Value::as_bool);
    if retained_payload_bytes_match.is_none() {
        reasons.insert(
            "exact_resource_union_delta_retained_payload_bytes_match_missing_or_false".to_owned(),
        );
    } else if retained_payload_bytes_match != Some(true) {
        reasons.insert(
            "product_gate_exact_resource_union_delta_retained_payload_bytes_mismatch".to_owned(),
        );
    }
    for (field, value) in [
        (
            "retained_unique_keys",
            expected.delta_union.retained_unique_keys,
        ),
        (
            "retained_unique_payload_bytes",
            expected.delta_union.retained_unique_payload_bytes,
        ),
        ("added_unique_keys", expected.delta_union.added_unique_keys),
        (
            "added_unique_payload_bytes",
            expected.delta_union.added_unique_payload_bytes,
        ),
        (
            "removed_unique_keys",
            expected.delta_union.removed_unique_keys,
        ),
        (
            "removed_unique_payload_bytes",
            expected.delta_union.removed_unique_payload_bytes,
        ),
    ] {
        let observed = delta
            .and_then(|delta| delta.get(field))
            .and_then(Value::as_u64);
        if observed.is_none() {
            reasons.insert(format!(
                "exact_resource_union_delta_{field}_mismatch_or_missing"
            ));
        } else if observed != Some(value) {
            reasons.insert(format!(
                "product_gate_exact_resource_union_delta_{field}_mismatch"
            ));
        }
    }
    let Some(start) = start_checkpoint.get("diagnostics") else {
        reasons.insert("unique_work_start_diagnostics_missing".to_owned());
        return;
    };
    let Some(end) = end_checkpoint.get("diagnostics") else {
        reasons.insert("unique_work_end_diagnostics_missing".to_owned());
        return;
    };
    for (pointer, range, label) in [
        (
            "/dataset_source_io/reader/physical_range_read_operations",
            &expected.physical_range_read_operations,
            "physical_range_read_operations",
        ),
        (
            "/dataset_source_io/reader/physical_encoded_bytes_read",
            &expected.physical_encoded_bytes_read,
            "physical_encoded_bytes_read",
        ),
        (
            "/dataset_source_io/reader/codec_decode_operations",
            &expected.codec_decode_operations,
            "codec_decode_operations",
        ),
        (
            "/dataset_source_io/reader/codec_decoded_bytes",
            &expected.codec_decoded_bytes,
            "codec_decoded_bytes",
        ),
        (
            "/dataset_runtime/counters/submitted_requests",
            &expected.dataset_submitted_requests,
            "dataset_submitted_requests",
        ),
        (
            "/dataset_runtime/counters/started_decodes",
            &expected.dataset_started_decodes,
            "dataset_started_decodes",
        ),
        (
            "/dataset_runtime/performance/decoded_output_bytes",
            &expected.runtime_decoded_output_bytes,
            "runtime_decoded_output_bytes",
        ),
        (
            "/gpu_adapter/uploads/resources",
            &expected.gpu_uploaded_resources,
            "gpu_uploaded_resources",
        ),
        (
            "/gpu_adapter/uploads/payload_bytes",
            &expected.gpu_uploaded_payload_bytes,
            "gpu_uploaded_payload_bytes",
        ),
        (
            "/gpu_adapter/control/dynamic_updates",
            &expected.gpu_control_dynamic_updates,
            "gpu_control_dynamic_updates",
        ),
        (
            "/gpu_adapter/control/dynamic_upload_bytes",
            &expected.gpu_control_dynamic_upload_bytes,
            "gpu_control_dynamic_upload_bytes",
        ),
        (
            "/gpu_adapter/control/publication_writes",
            &expected.gpu_control_publication_writes,
            "gpu_control_publication_writes",
        ),
    ] {
        check_counter_delta_range(
            start.pointer(pointer).and_then(Value::as_u64),
            end.pointer(pointer).and_then(Value::as_u64),
            range,
            label,
            reasons,
        );
    }
}

fn check_counter_delta_range(
    start: Option<u64>,
    end: Option<u64>,
    expected: &InclusiveU64Range,
    label: &str,
    reasons: &mut BTreeSet<String>,
) {
    match (start, end) {
        (Some(start), Some(end)) if end >= start => {
            let delta = end - start;
            if !(expected.minimum..=expected.maximum).contains(&delta) {
                reasons.insert(format!("unique_work_{label}_delta_outside_oracle"));
            }
        }
        (Some(_), Some(_)) => {
            reasons.insert(format!("unique_work_{label}_counter_regressed"));
        }
        _ => {
            reasons.insert(format!("unique_work_{label}_fact_missing"));
        }
    }
}

fn validate_verification_evidence(
    start_checkpoint: &Value,
    end_checkpoint: &Value,
    expected: &VerificationGate,
    reasons: &mut BTreeSet<String>,
) {
    let start = start_checkpoint.pointer("/diagnostics/source_verification");
    let end = end_checkpoint.pointer("/diagnostics/source_verification");
    for (position, observed, expected_checkpoint) in
        [("start", start, expected.start), ("end", end, expected.end)]
    {
        let state = observed
            .and_then(|value| value.get("state"))
            .and_then(Value::as_str);
        let active_operation = observed
            .and_then(|value| value.get("active_operation"))
            .and_then(Value::as_bool);
        if state.is_none() || active_operation.is_none() {
            reasons.insert(format!(
                "verification_{position}_state_or_active_operation_mismatch_or_missing"
            ));
        } else if state != Some(expected_checkpoint.state.report_label())
            || active_operation != Some(expected_checkpoint.active_operation)
        {
            reasons.insert(format!(
                "product_gate_verification_{position}_state_or_active_operation_mismatch"
            ));
        }
        for (field, value) in [
            ("started_runs", expected_checkpoint.started_runs),
            ("cancelled_runs", expected_checkpoint.cancelled_runs),
            ("failed_runs", expected_checkpoint.failed_runs),
            ("accepted_successes", expected_checkpoint.accepted_successes),
            (
                "completed_reader_runs",
                expected_checkpoint.completed_reader_runs,
            ),
        ] {
            let observed_value = observed
                .and_then(|value| value.pointer(&format!("/service/{field}")))
                .and_then(Value::as_u64);
            if observed_value.is_none() {
                reasons.insert(format!(
                    "verification_{position}_service_{field}_mismatch_or_missing"
                ));
            } else if observed_value != Some(value) {
                reasons.insert(format!(
                    "product_gate_verification_{position}_service_{field}_mismatch"
                ));
            }
        }
    }
    let start_progress = start
        .and_then(|value| value.pointer("/service/accepted_progress_updates"))
        .and_then(Value::as_u64);
    let end_progress = end
        .and_then(|value| value.pointer("/service/accepted_progress_updates"))
        .and_then(Value::as_u64);
    let progress_delta = start_progress
        .zip(end_progress)
        .and_then(|(start, end)| end.checked_sub(start));
    if progress_delta.is_none() {
        reasons.insert("verification_accepted_progress_delta_missing_or_below_gate".to_owned());
    } else if progress_delta < Some(expected.minimum_accepted_progress_updates_delta) {
        reasons.insert("product_gate_verification_accepted_progress_delta_below_gate".to_owned());
    }
    if let Some(reader) = &expected.completed_reader_work {
        for (position, observed) in [("start", start), ("end", end)] {
            if observed
                .and_then(|value| value.pointer("/service/completed_reader_scope"))
                .and_then(Value::as_str)
                != Some("completed_separate_strict_verification_readers")
                || observed
                    .and_then(|value| {
                        value.pointer(
                            "/service/completed_reader_counters_include_only_completed_runs",
                        )
                    })
                    .and_then(Value::as_bool)
                    != Some(true)
            {
                reasons.insert(format!(
                    "verification_{position}_completed_reader_authority_missing_or_mismatched"
                ));
            }
        }
        for (field, range) in [
            ("object_open_operations", &reader.object_open_operations),
            (
                "physical_range_read_operations",
                &reader.physical_range_read_operations,
            ),
            (
                "physical_encoded_bytes_read",
                &reader.physical_encoded_bytes_read,
            ),
            ("codec_decode_operations", &reader.codec_decode_operations),
            ("codec_decoded_bytes", &reader.codec_decoded_bytes),
        ] {
            check_counter_delta_range(
                start
                    .and_then(|value| value.pointer(&format!("/service/completed_reader/{field}")))
                    .and_then(Value::as_u64),
                end.and_then(|value| value.pointer(&format!("/service/completed_reader/{field}")))
                    .and_then(Value::as_u64),
                range,
                &format!("verification_reader_{field}"),
                reasons,
            );
        }
    }
}

fn validate_nonresident_target_residency(
    start_checkpoint: &Value,
    end_checkpoint: &Value,
    start_label: &str,
    expected: &PhaseStartTargetResidencyExpectation,
    reasons: &mut BTreeSet<String>,
) {
    let start_residency = start_checkpoint.pointer("/resource_accounting/exact_gpu_resident_union");
    let start_residency_sha256 = start_residency
        .and_then(|value| value.get("canonical_entries_sha256"))
        .and_then(Value::as_str);
    if start_residency
        .and_then(|value| value.get("available"))
        .and_then(Value::as_bool)
        != Some(true)
        || start_residency
            .and_then(|value| value.get("label"))
            .and_then(Value::as_str)
            != Some(start_label)
        || start_residency
            .and_then(|value| value.get("raw_keys_serialized"))
            .and_then(Value::as_bool)
            != Some(false)
        || start_residency
            .and_then(|value| value.get("canonical_entries_sha256_derivation"))
            .and_then(Value::as_str)
            != Some("sha256_domain_mirante4d_ep00_resource_union_v1_sorted_binary_le")
        || start_residency_sha256.is_none_or(|digest| {
            digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
    {
        reasons.insert("phase_start_exact_gpu_resident_union_authority_missing".to_owned());
    }
    let proof = end_checkpoint.pointer("/resource_accounting/target_residency_at_phase_start");
    let observed_target_sha256 = end_checkpoint
        .pointer("/resource_accounting/exact_cross_scope_union/canonical_entries_sha256")
        .and_then(Value::as_str)
        .filter(|digest| require_sha256(digest, "observed endpoint target union").is_ok());
    let proof_target_sha256 = proof
        .and_then(|value| value.get("target_union_sha256"))
        .and_then(Value::as_str)
        .filter(|digest| require_sha256(digest, "nonresident target union proof").is_ok());
    if proof
        .and_then(|value| value.get("available"))
        .and_then(Value::as_bool)
        != Some(true)
        || proof
            .and_then(|value| value.get("phase_start_label"))
            .and_then(Value::as_str)
            != Some(start_label)
        || proof
            .and_then(|value| value.get("phase_start_resident_union_sha256"))
            .and_then(Value::as_str)
            != start_residency_sha256
        || proof_target_sha256.is_none()
        || proof_target_sha256 != observed_target_sha256
        || proof
            .and_then(|value| value.get("partitions_pairwise_disjoint"))
            .and_then(Value::as_bool)
            != Some(true)
        || proof
            .and_then(|value| value.get("target_union_reconciles"))
            .and_then(Value::as_bool)
            != Some(true)
        || proof
            .and_then(|value| value.get("derivation"))
            .and_then(Value::as_str)
            != Some("sorted_target_union_partition_by_phase_start_gpu_residency")
    {
        reasons.insert("nonresident_target_phase_start_residency_authority_missing".to_owned());
    }
    for (partition, digest, keys, bytes) in [
        (
            "resident_target_intersection",
            expected
                .resident_target_intersection
                .canonical_entries_sha256
                .as_str(),
            expected.resident_target_intersection.unique_keys,
            expected.resident_target_intersection.unique_payload_bytes,
        ),
        (
            "nonresident_target_difference",
            expected
                .nonresident_target_difference
                .canonical_entries_sha256
                .as_str(),
            expected.nonresident_target_difference.unique_keys,
            expected.nonresident_target_difference.unique_payload_bytes,
        ),
    ] {
        let observed = proof.and_then(|value| value.get(partition));
        let observed_digest = observed
            .and_then(|value| value.get("canonical_entries_sha256"))
            .and_then(Value::as_str)
            .filter(|digest| require_sha256(digest, "nonresident target partition proof").is_ok());
        let observed_keys = observed
            .and_then(|value| value.get("unique_keys"))
            .and_then(Value::as_u64);
        let observed_bytes = observed
            .and_then(|value| value.get("unique_payload_bytes"))
            .and_then(Value::as_u64);
        if observed_digest.is_none() || observed_keys.is_none() || observed_bytes.is_none() {
            reasons.insert(format!(
                "nonresident_target_{partition}_mismatch_or_missing"
            ));
        } else if observed_digest != Some(digest)
            || observed_keys != Some(keys)
            || observed_bytes != Some(bytes)
        {
            reasons.insert(format!(
                "product_gate_nonresident_target_{partition}_mismatch"
            ));
        }
    }
}

fn validate_zero_work(
    start: &Value,
    end: &Value,
    counters: &[ZeroWorkCounter],
    cancellation_waste_authority: CancellationWasteAuthority,
    reasons: &mut BTreeSet<String>,
) {
    for counter in counters {
        if matches!(
            counter,
            ZeroWorkCounter::CancellationWasteEncodedBytes
                | ZeroWorkCounter::CancellationWasteUploadedBytes
        ) && cancellation_waste_authority == CancellationWasteAuthority::PredecessorUnattributed
        {
            if !predecessor_cancellation_unavailable_fact(end, *counter) {
                reasons.insert(format!(
                    "structural_{}_predecessor_authority_fact_missing",
                    counter.reason_label()
                ));
            }
            continue;
        }
        let start_value = counter.value(start);
        let end_value = counter.value(end);
        match (start_value, end_value) {
            (Some(start), Some(end)) if end == start => {}
            (Some(start), Some(end)) if end > start => {
                reasons.insert(format!(
                    "structural_{}_counter_changed",
                    counter.reason_label()
                ));
            }
            (Some(_), Some(_)) => {
                reasons.insert(format!(
                    "structural_{}_counter_regressed",
                    counter.reason_label()
                ));
            }
            _ => {
                reasons.insert(format!(
                    "structural_{}_counter_missing",
                    counter.reason_label()
                ));
            }
        }
    }
}

fn validate_structural_ceilings(
    start: &Value,
    end: &Value,
    display_batch_authority: DisplayBatchAuthority,
    cancellation_waste_authority: CancellationWasteAuthority,
    ceilings: &StructuralCeilings,
    reasons: &mut BTreeSet<String>,
) {
    let pending_peak =
        end.pointer("/render/display_coordination/detailed_counters/pending_display_batches_peak");
    match display_batch_authority {
        DisplayBatchAuthority::SynchronousUiThreadPredecessor => {
            if pending_peak
                .and_then(|value| value.get("available"))
                .and_then(Value::as_bool)
                != Some(false)
                || pending_peak
                    .and_then(|value| value.get("reason"))
                    .and_then(Value::as_str)
                    != Some("no_display_batch_coordinator")
            {
                reasons.insert(
                    "structural_pending_display_batches_predecessor_authority_fact_missing"
                        .to_owned(),
                );
            }
            if end
                .pointer("/render/display_coordination/detailed_counters/display_batch_ownership")
                .and_then(Value::as_str)
                != Some("synchronous_ui_thread_encode_submit_no_replaceable_queue")
            {
                reasons.insert("structural_predecessor_display_batch_ownership_missing".to_owned());
            }
        }
        DisplayBatchAuthority::CoordinatedDisplayBatch => check_counter_peak(
            pending_peak.and_then(Value::as_u64),
            ceilings.pending_display_batches_peak_maximum,
            "pending_display_batches_peak",
            reasons,
        ),
    }
    check_counter_peak(
        end.pointer("/gpu_adapter/peak_in_flight_submissions")
            .and_then(Value::as_u64),
        ceilings.in_flight_display_batches_peak_maximum,
        "in_flight_display_batches_peak",
        reasons,
    );
    check_counter_delta(
        per_target_counter_sum(start, "command_buffers"),
        per_target_counter_sum(end, "command_buffers"),
        ceilings.command_encoders_delta_maximum,
        "command_encoders",
        reasons,
    );
    check_pointer_delta(
        start,
        end,
        "/render/display_coordination/detailed_counters/color_passes",
        ceilings.color_passes_delta_maximum,
        "color_passes",
        reasons,
    );
    check_counter_delta(
        per_target_counter_sum(start, "queue_submissions"),
        per_target_counter_sum(end, "queue_submissions"),
        ceilings.renderer_submissions_delta_maximum,
        "renderer_submissions",
        reasons,
    );
    check_pointer_delta(
        start,
        end,
        "/render/display_coordination/detailed_counters/completion_notifications",
        ceilings.completion_notifications_delta_maximum,
        "completion_notifications",
        reasons,
    );
    check_counter_delta(
        per_target_counter_sum(start, "backpressure_deferrals"),
        per_target_counter_sum(end, "backpressure_deferrals"),
        ceilings.backpressure_deferrals_delta_maximum,
        "backpressure_deferrals",
        reasons,
    );
    for (pointer, maximum, label) in [
        (
            "/render/display_coordination/detailed_counters/encoded_display_batches",
            ceilings.encoded_display_batches_delta_maximum,
            "encoded_display_batches",
        ),
        (
            "/render/display_coordination/detailed_counters/encoded_but_dropped_batches",
            ceilings.encoded_but_dropped_delta_maximum,
            "encoded_but_dropped_batches",
        ),
        (
            "/render/display_coordination/detailed_counters/sealed_obsolete_submitted_batches",
            ceilings.sealed_obsolete_submitted_delta_maximum,
            "sealed_obsolete_submitted_batches",
        ),
        (
            "/render/progressive_presentation/stale_frames_rejected",
            ceilings.stale_presentations_delta_maximum,
            "stale_presentations",
        ),
        (
            "/render/display_coordination/detailed_counters/current_presentations",
            ceilings.current_presentations_delta_maximum,
            "current_presentations",
        ),
        (
            "/dataset_demand/planned_scope_accounting/demand_work",
            ceilings.demand_work_delta_maximum,
            "demand_work",
        ),
        (
            "/dataset_runtime/performance/cancelled_decode_executions",
            ceilings.cancellation_waste_count_delta_maximum,
            "cancellation_waste_started_decode_count",
        ),
        (
            "/dataset_runtime/performance/cancelled_decode_bytes",
            ceilings.cancellation_waste_decoded_bytes_delta_maximum,
            "cancellation_waste_decoded_bytes",
        ),
        (
            "/dataset_runtime/performance/cancelled_decode_time_ns",
            ceilings.cancellation_waste_cpu_time_ns_delta_maximum,
            "cancellation_waste_cpu_time_ns",
        ),
    ] {
        check_pointer_delta(start, end, pointer, maximum, label, reasons);
    }
    match cancellation_waste_authority {
        CancellationWasteAuthority::PredecessorUnattributed => {
            for counter in [
                ZeroWorkCounter::CancellationWasteEncodedBytes,
                ZeroWorkCounter::CancellationWasteUploadedBytes,
            ] {
                if !predecessor_cancellation_unavailable_fact(end, counter) {
                    reasons.insert(format!(
                        "structural_{}_predecessor_authority_fact_missing",
                        counter.reason_label()
                    ));
                }
            }
        }
        CancellationWasteAuthority::GenerationBoundSharedBrick => {
            check_pointer_delta(
                start,
                end,
                "/dataset_source_io/reader/cancelled_encoded_bytes",
                ceilings.cancellation_waste_encoded_bytes_delta_maximum,
                "cancellation_waste_encoded_bytes",
                reasons,
            );
            check_pointer_delta(
                start,
                end,
                "/gpu_adapter/uploads/cancelled_payload_bytes",
                ceilings.cancellation_waste_uploaded_bytes_delta_maximum,
                "cancellation_waste_uploaded_bytes",
                reasons,
            );
        }
    }
}

fn predecessor_cancellation_unavailable_fact(
    diagnostics: &Value,
    counter: ZeroWorkCounter,
) -> bool {
    let (pointer, reason) = match counter {
        ZeroWorkCounter::CancellationWasteEncodedBytes => (
            "/dataset_source_io/reader/cancelled_encoded_bytes",
            "physical_range_cohort_has_no_per_sink_cancellation_ownership",
        ),
        ZeroWorkCounter::CancellationWasteUploadedBytes => (
            "/gpu_adapter/uploads/cancelled_payload_bytes",
            "renderer_uploads_have_no_sealed_generation_cancellation_outcome",
        ),
        _ => return false,
    };
    diagnostics
        .pointer(pointer)
        .and_then(|value| value.get("available"))
        .and_then(Value::as_bool)
        == Some(false)
        && diagnostics
            .pointer(pointer)
            .and_then(|value| value.get("reason"))
            .and_then(Value::as_str)
            == Some(reason)
}

fn per_target_counter_sum(diagnostics: &Value, field: &str) -> Option<u64> {
    let detailed = diagnostics.pointer("/render/display_coordination/detailed_counters")?;
    let targets = detailed
        .get("per_target_renderer_facts")
        .and_then(Value::as_array)?;
    let visible_total = targets.iter().try_fold(0_u64, |total, target| {
        total.checked_add(target.get(field)?.as_u64()?)
    })?;
    match detailed.get("staging_3d_renderer_facts")? {
        Value::Null => Some(visible_total),
        staging
            if staging.get("purpose").and_then(Value::as_str)
                == Some("hidden_staging_3d_fallback_target") =>
        {
            visible_total.checked_add(staging.get(field)?.as_u64()?)
        }
        _ => None,
    }
}

fn check_pointer_delta(
    start: &Value,
    end: &Value,
    pointer: &str,
    maximum: u64,
    label: &str,
    reasons: &mut BTreeSet<String>,
) {
    check_counter_delta(
        start.pointer(pointer).and_then(Value::as_u64),
        end.pointer(pointer).and_then(Value::as_u64),
        maximum,
        label,
        reasons,
    );
}

fn check_counter_delta(
    start: Option<u64>,
    end: Option<u64>,
    maximum: u64,
    label: &str,
    reasons: &mut BTreeSet<String>,
) {
    match (start, end) {
        (Some(start), Some(end)) if end >= start && end - start <= maximum => {}
        (Some(start), Some(end)) if end < start => {
            reasons.insert(format!("structural_{label}_counter_regressed"));
        }
        (Some(_), Some(_)) => {
            reasons.insert(format!("structural_{label}_ceiling_exceeded"));
        }
        _ => {
            reasons.insert(format!("structural_{label}_fact_missing"));
        }
    }
}

fn check_counter_peak(
    observed: Option<u64>,
    maximum: u64,
    label: &str,
    reasons: &mut BTreeSet<String>,
) {
    match observed {
        Some(observed) if observed <= maximum => {}
        Some(_) => {
            reasons.insert(format!("structural_{label}_ceiling_exceeded"));
        }
        None => {
            reasons.insert(format!("structural_{label}_fact_missing"));
        }
    }
}

fn validate_sequence_commit_events(
    report: &Value,
    template: &AutomationScriptTemplate,
    phase: &ScriptPhase,
    start_diagnostics: &Value,
    end_diagnostics: &Value,
    expected_commits: u64,
    reasons: &mut BTreeSet<String>,
) {
    let Some(start_label) = phase.start_diagnostic_label.as_deref() else {
        reasons.insert("sequence_phase_start_checkpoint_missing".to_owned());
        return;
    };
    let start_index = diagnostic_command_index(&template.commands, start_label);
    let end_index = diagnostic_command_index(&template.commands, &phase.end_diagnostic_label);
    let startup_bootstrap_start = template
        .startup_bootstrap
        .as_ref()
        .is_some_and(|bootstrap| {
            bootstrap.capture_start_checkpoint
                && bootstrap.start_diagnostic_label.as_deref() == Some(start_label)
        });
    let Some(end_index) = end_index else {
        reasons.insert("sequence_phase_command_bounds_missing".to_owned());
        return;
    };
    if start_index.is_none() && !startup_bootstrap_start {
        reasons.insert("sequence_phase_command_bounds_missing".to_owned());
        return;
    }
    let sequence_commands = template
        .commands
        .iter()
        .enumerate()
        .filter(|(index, command)| {
            start_index.is_none_or(|start_index| *index > start_index)
                && *index < end_index
                && command
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(is_input_sequence_command)
        })
        .collect::<Vec<_>>();
    let expected_phase_commits = u64::try_from(sequence_commands.len())
        .unwrap_or(u64::MAX)
        .saturating_mul(expected_commits);
    let start_commits = start_diagnostics
        .pointer("/render/display_coordination/durable_gesture_commits")
        .and_then(Value::as_u64);
    let end_commits = end_diagnostics
        .pointer("/render/display_coordination/durable_gesture_commits")
        .and_then(Value::as_u64);
    match (start_commits, end_commits) {
        (Some(start), Some(end)) if end < start => {
            reasons.insert("phase_durable_gesture_commit_counter_regressed".to_owned());
        }
        (Some(start), Some(end)) if end - start == expected_phase_commits => {}
        (Some(_), Some(_)) => {
            reasons.insert("product_gate_phase_durable_gesture_commit_delta_mismatch".to_owned());
        }
        _ => {
            reasons.insert("phase_durable_gesture_commit_counter_missing".to_owned());
        }
    }
    let start_currentness = start_diagnostics
        .pointer("/application_state/currentness_generation")
        .and_then(Value::as_u64);
    let end_currentness = end_diagnostics
        .pointer("/application_state/currentness_generation")
        .and_then(Value::as_u64);
    match (start_currentness, end_currentness) {
        (Some(start), Some(end)) if end < start => {
            reasons.insert("phase_application_currentness_counter_regressed".to_owned());
        }
        (Some(start), Some(end)) if end - start == expected_phase_commits => {}
        (Some(_), Some(_)) => {
            reasons.insert("product_gate_phase_application_currentness_delta_mismatch".to_owned());
        }
        _ => {
            reasons.insert("phase_application_currentness_counter_missing".to_owned());
        }
    }
    for diagnostics in [start_diagnostics, end_diagnostics] {
        if diagnostics
            .pointer("/application_state/currentness_derivation")
            .and_then(Value::as_str)
            != Some("ApplicationSnapshot_currentness_generation")
        {
            reasons.insert("phase_application_currentness_authority_missing".to_owned());
        }
    }

    let start_bound = start_diagnostics
        .pointer("/project_state/bound")
        .and_then(Value::as_bool);
    let end_bound = end_diagnostics
        .pointer("/project_state/bound")
        .and_then(Value::as_bool);
    match (start_bound, end_bound) {
        (Some(true), Some(true)) => {
            for (counter, pointer) in [
                (
                    "durable_project_revision",
                    "/project_state/revision_high_water_sequence",
                ),
                (
                    "undo_history_entry",
                    "/project_state/history_entry_high_water_sequence",
                ),
            ] {
                let start = start_diagnostics.pointer(pointer).and_then(Value::as_u64);
                let end = end_diagnostics.pointer(pointer).and_then(Value::as_u64);
                match (start, end) {
                    (Some(start), Some(end)) if end < start => {
                        reasons.insert(format!("phase_{counter}_counter_regressed"));
                    }
                    (Some(start), Some(end)) if end - start == expected_phase_commits => {}
                    (Some(_), Some(_)) => {
                        reasons.insert(format!("product_gate_phase_{counter}_delta_mismatch"));
                    }
                    _ => {
                        reasons.insert(format!("phase_{counter}_counter_missing"));
                    }
                }
            }
            for diagnostics in [start_diagnostics, end_diagnostics] {
                if diagnostics
                    .pointer("/project_state/history_entry_high_water_derivation")
                    .and_then(Value::as_str)
                    != Some("one_BoundWorkspace_history_push_per_allocated_durable_revision")
                {
                    reasons.insert("phase_undo_history_authority_missing".to_owned());
                }
            }
        }
        (Some(false), Some(false)) => {
            for diagnostics in [start_diagnostics, end_diagnostics] {
                let project_state = diagnostics.get("project_state");
                if [
                    "current_revision",
                    "saved_revision",
                    "revision_high_water_sequence",
                    "retained_history_entries",
                    "history_entry_high_water_sequence",
                ]
                .into_iter()
                .any(|field| {
                    !project_state
                        .and_then(|project_state| project_state.get(field))
                        .is_some_and(Value::is_null)
                }) {
                    reasons.insert(
                        "phase_unbound_project_revision_or_history_fact_not_explicit_null"
                            .to_owned(),
                    );
                }
            }
        }
        (Some(_), Some(_)) => {
            reasons.insert("phase_project_binding_changed".to_owned());
        }
        _ => {
            reasons.insert("phase_project_binding_fact_missing".to_owned());
        }
    }
    let Some(events) = report.get("events").and_then(Value::as_array) else {
        reasons.insert("automation_sequence_events_missing".to_owned());
        return;
    };
    for (command_index, command) in sequence_commands {
        let matches = events
            .iter()
            .filter(|event| {
                event.get("command_index").and_then(Value::as_u64)
                    == u64::try_from(command_index).ok()
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            reasons.insert("gesture_sequence_event_missing_or_duplicated".to_owned());
            continue;
        }
        let event = matches[0];
        let expected_samples = command.get("samples").and_then(Value::as_u64);
        let status = event.get("status").and_then(Value::as_str);
        let detailed_counters_enabled = event
            .pointer("/details/observed_counter_delta/detailed_counters_enabled")
            .and_then(Value::as_bool);
        let durable_gesture_commits = event
            .pointer("/details/observed_counter_delta/durable_gesture_commits")
            .and_then(Value::as_u64);
        let raw_input_samples = event
            .pointer("/details/observed_counter_delta/raw_input_samples")
            .and_then(Value::as_u64);
        if status != Some("passed")
            || detailed_counters_enabled != Some(true)
            || expected_samples.is_none()
            || durable_gesture_commits.is_none()
            || raw_input_samples.is_none()
        {
            reasons.insert("gesture_sequence_durable_commit_or_sample_delta_mismatch".to_owned());
        } else if durable_gesture_commits != Some(expected_commits)
            || raw_input_samples != expected_samples
        {
            reasons.insert(
                "product_gate_gesture_sequence_durable_commit_or_sample_delta_mismatch".to_owned(),
            );
        }
    }
}

fn diagnostic_command_index(commands: &[Value], label: &str) -> Option<usize> {
    commands.iter().position(|command| {
        command.get("command").and_then(Value::as_str) == Some("sample_diagnostics")
            && command.get("label").and_then(Value::as_str) == Some(label)
    })
}

fn verification_completion_observation_index(commands: &[Value]) -> Option<usize> {
    let request = commands.iter().position(|command| {
        command.get("command").and_then(Value::as_str) == Some("request_source_verification")
    })?;
    commands.iter().enumerate().find_map(|(index, command)| {
        (index > request
            && command.get("command").and_then(Value::as_str) == Some("observe_gate_batch")
            && command
                .get("observations")
                .and_then(Value::as_array)
                .is_some_and(|observations| {
                    observations.iter().any(|observation| {
                        observation
                            .pointer("/target/condition")
                            .and_then(Value::as_str)
                            == Some("source_verification_verified")
                    })
                }))
        .then_some(index)
    })
}

fn is_input_sequence_command(command: &str) -> bool {
    matches!(
        command,
        "camera_zoom_sequence"
            | "cross_section_zoom_sequence"
            | "cross_section_rotate_sequence"
            | "cross_section_slice_sequence"
            | "cross_section_pan_sequence"
            | "camera_orbit_sequence"
            | "camera_pan_sequence"
    )
}

#[allow(clippy::too_many_arguments)]
fn raw_report(
    args: &RunArgs,
    profile: &LoadedProfile,
    workload: &LoadedBundle<WorkloadBundle>,
    scripts: &LoadedBundle<ScriptBundle>,
    oracle: &LoadedBundle<OracleBundle>,
    app_binary: &Path,
    app_binary_sha256: &str,
    app_binary_sha256_end: &str,
    result_root: &Path,
    observations: &PreflightObservations,
    binding_reasons: &BTreeSet<String>,
    conformance: Option<&ConformanceEvidence>,
    samples: &[SampleEvidence],
    population: PopulationEvidence,
    instrumentation_overhead_populations: &[InstrumentationOverheadPopulationEvidence],
    all_reasons: &BTreeSet<String>,
    repository_start: &RepositoryIdentity,
    repository_end: &RepositoryIdentity,
    source_repository_start: &RepositoryIdentity,
    source_repository_end: &RepositoryIdentity,
    xtask_build: &QualificationBuildProvenance,
) -> Value {
    let binding_reason_refs = binding_reasons
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let bundle_commitments = BundleCommitments {
        workload_bundle_sha256: workload.sha256.clone(),
        interaction_script_bundle_sha256: scripts.sha256.clone(),
        independent_oracle_sha256: oracle.sha256.clone(),
        ep01_trace_geometry_sha256: ep01_trace_geometry_sha256(&workload.value.ep01_trace_geometry),
    };
    let product_gate_outcomes = raw_product_gate_outcome_rows(samples);
    let product_gate_failures = classified_product_gate_failure_rows(samples);
    json!({
        "schema": RAW_REPORT_SCHEMA,
        "evidence_status": evidence_status(all_reasons),
        "product_gate_status": product_gate_status(
            !has_integrity_reasons(all_reasons),
            product_gate_outcomes.iter().any(|row| {
                row.get("outcome").and_then(Value::as_str) == Some("failed")
            }) || !product_gate_failures.is_empty() || has_product_gate_failures(all_reasons),
        ),
        "claim_status": "development_E1_semantic_automation_non_OS_input_non_E4_no_product_claim",
        "build_binding": {
            "profile": profile.profile.build,
            "xtask": qualification_build_provenance_evidence(xtask_build),
            "repository_start": {
                "revision": repository_start.commit,
                "dirty_worktree": repository_start.dirty_worktree,
                "root_resolved": repository_start.root.is_some(),
            },
            "repository_end": {
                "revision": repository_end.commit,
                "dirty_worktree": repository_end.dirty_worktree,
                "root_resolved": repository_end.root.is_some(),
            },
            "repository_unchanged_and_clean": repository_identity_unchanged_and_clean(
                repository_start,
                repository_end,
            ),
            "immutable_source_start": {
                "revision": source_repository_start.commit,
                "dirty_worktree": source_repository_start.dirty_worktree,
                "root_resolved": source_repository_start.root.is_some(),
            },
            "immutable_source_end": {
                "revision": source_repository_end.commit,
                "dirty_worktree": source_repository_end.dirty_worktree,
                "root_resolved": source_repository_end.root.is_some(),
            },
            "immutable_source_unchanged_and_clean": repository_identity_unchanged_and_clean(
                source_repository_start,
                source_repository_end,
            ),
        },
        "protocol": {
            "development_samples": profile.profile.protocol.development_samples,
            "fresh_process_per_role_per_scenario_sample": true,
            "automatic_retries": 0,
            "process_timeouts_are_script_derived": true,
            "startup_admission_grace_ns": PROCESS_STARTUP_ADMISSION_GRACE_NS,
            "closeout_grace_ns": PROCESS_CLOSEOUT_GRACE_NS,
            "cache_condition_attestation": args.attestation.cache_condition,
            "competing_activity_attestation": args.attestation.competing_activity,
            "power_state_attestation": args.attestation.power_state,
            "compositor_scale_milli_attestation": args.attestation.compositor_scale_milli,
            "instrumented_control_order_balanced_by_sample_and_scenario": true,
        },
        "private_paths": {
            "qualification_profile": profile_path(profile, args),
            "workload_bundle": workload.path,
            "interaction_script_bundle": scripts.path,
            "independent_oracle": oracle.path,
            "app_binary": app_binary,
            "result_root": result_root,
            "immutable_source_root": source_repository_start.root,
            "representative_package": profile.profile.workload.representative_package.root,
        },
        "commitments": {
            "qualification_profile_sha256": profile.sha256,
            "owner_accepted_profile_contract_sha256": profile_contract_sha256(&profile.profile),
            "ep01_selection_authority_sha256": profile.profile.ep01_selection_authority_sha256,
            "workload_bundle_sha256": workload.sha256,
            "ep01_trace_geometry_sha256": bundle_commitments.ep01_trace_geometry_sha256,
            "interaction_script_bundle_sha256": scripts.sha256,
            "independent_oracle_sha256": oracle.sha256,
            "app_binary_sha256_before_run": app_binary_sha256,
            "app_binary_sha256_after_run": app_binary_sha256_end,
            "app_binary_unchanged": app_binary_sha256 == app_binary_sha256_end,
            "representative_package_fingerprint_sha256": commitment_fingerprint(
                "representative-package",
                &profile.profile.workload.representative_package.root_manifest_sha256,
            ),
            "supporting_temporal_package_fingerprint_sha256": commitment_fingerprint(
                "supporting-temporal-package",
                &workload.value.supporting_temporal_package_root_manifest_sha256,
            ),
            "build_binding_fingerprint_sha256": super::build_binding_fingerprint(
                &profile.profile.build,
            ),
        },
        "bindings": {
            "binding_reason_codes": binding_reasons,
            "preflight": preflight_report(
                profile,
                &bundle_commitments,
                observations,
                &binding_reason_refs,
            ),
            "blocking_extent": profile.profile.extents.blocking_qualification,
            "exercise_extent": profile.profile.extents.required_exercise,
            "resource_policy": {
                "cpu_dataset_budget_bytes": profile.profile.resources.max_cpu_total_bytes,
                "gpu_budget_bytes": profile.profile.resources.gpu_budget_bytes,
                "gpu_payload_capacity_bytes": profile.profile.resources.max_gpu_resident_bytes,
                "gpu_transfer_capacity_bytes": profile.profile.resources.max_gpu_in_flight_bytes,
            },
            "absolute_gates": profile.profile.absolute_gates,
        },
        "executable_conformance": conformance.map(ConformanceEvidence::raw_json),
        "population": population_json(population),
        "instrumentation_overhead_populations": instrumentation_overhead_population_rows(
            instrumentation_overhead_populations,
        ),
        "product_gate_outcomes": product_gate_outcomes,
        "product_gate_failures": product_gate_failures,
        "attempts": samples.iter().map(sample_json).collect::<Vec<_>>(),
        "integrity_reason_codes": integrity_reason_codes(all_reasons),
        "limitations": [
            "E1 semantic application commands do not inject or claim OS input",
            "development samples do not establish a product performance claim",
            "E4 real-display OS-input validation remains separate",
        ],
    })
}

fn profile_path<'a>(_profile: &'a LoadedProfile, args: &'a RunArgs) -> &'a Path {
    &args.profile
}

fn sample_json(sample: &SampleEvidence) -> Value {
    let evidence_valid = !has_integrity_reasons(&sample.reasons)
        && !has_integrity_reasons(&sample.instrumented.reasons)
        && sample
            .control
            .as_ref()
            .is_some_and(|control| !has_integrity_reasons(&control.reasons))
        && sample
            .phases
            .iter()
            .all(|phase| !has_integrity_reasons(&phase.reasons));
    let has_failed_product_gate = sample
        .instrumented
        .product_gate_outcomes
        .iter()
        .chain(
            sample
                .control
                .iter()
                .flat_map(|control| &control.product_gate_outcomes),
        )
        .any(|outcome| outcome.outcome == ProductGateStatus::Failed)
        || has_product_gate_failures(&sample.reasons)
        || has_product_gate_failures(&sample.instrumented.reasons)
        || sample
            .control
            .as_ref()
            .is_some_and(|control| has_product_gate_failures(&control.reasons))
        || sample
            .phases
            .iter()
            .any(|phase| has_product_gate_failures(&phase.reasons));
    json!({
        "sample_index": sample.sample_index,
        "scenario": sample.scenario,
        "instrumented": role_json(&sample.instrumented),
        "instrumentation_control": sample.control.as_ref().map(role_json),
        "paired_overhead": {
            "evaluation_scope": "per_pair_observation_only_gate_applies_to_complete_balanced_scenario_population",
            "instrumented_raw_app_wall_time_ns": sample.instrumented.app_wall_time_ns,
            "instrumented_qualification_gpu_timing_await_wall_time_ns": sample.instrumented_qualification_wait_wall_ns,
            "instrumented_adjusted_app_wall_time_ns": sample.instrumented_adjusted_wall_time_ns,
            "control_app_wall_time_ns": sample.control.as_ref().and_then(|control| control.app_wall_time_ns),
            "wall_basis_points": sample.wall_overhead_basis_points,
            "process_cpu_basis_points": sample.process_cpu_overhead_basis_points,
        },
        "phases": sample.phases.iter().map(|phase| json!({
            "name": phase.name,
            "integrity_reason_codes": integrity_reason_codes(&phase.reasons),
            "evidence_status": evidence_status(&phase.reasons),
            "product_gate_status": product_gate_status(
                !has_integrity_reasons(&phase.reasons),
                has_product_gate_failures(&phase.reasons),
            ),
        })).collect::<Vec<_>>(),
        "integrity_reason_codes": integrity_reason_codes(&sample.reasons),
        "evidence_status": if evidence_valid { "valid_complete" } else { "invalid_or_incomplete" },
        "product_gate_status": product_gate_status(evidence_valid, has_failed_product_gate),
    })
}

fn role_json(role: &RoleEvidence) -> Value {
    json!({
        "role": role.role.directory_name(),
        "root": role.root,
        "paths": {
            "expanded_script": role.root.join("automation-script.json"),
            "automation_report": role.root.join("automation-report.json"),
            "stdout": role.root.join("stdout.log"),
            "stderr": role.root.join("stderr.log"),
            "app_log": role.root.join("app.log"),
            "resource_settings": role.root.join("config/mirante4d/settings.json"),
        },
        "commitments": {
            "template_script_sha256": role.template_script_sha256,
            "expanded_script_sha256": role.expanded_script_sha256,
            "cleaned_imported_package_root_manifest_sha256": role.cleanup_manifest_sha256,
            "automation_report_sha256": role.automation_report_sha256,
        },
        "process": {
            "launch_attempted": role.process.launch_attempted,
            "exit_code": role.process.status.and_then(|status| status.code()),
            "signal": role.process.status.and_then(|status| status.signal()),
            "external_wall_time_ns": role.process.external_wall_time_ns,
            "timed_out": role.process.timed_out,
            "spawn_error": role.process.spawn_error,
            "app_wall_time_ns": role.app_wall_time_ns,
            "process_cpu_time_ns": role.process_cpu_time_ns,
            "derived_process_timeout_ns": role.derived_process_timeout_ns,
            "static_wait_bound_ns": role.static_wait_bound_ns,
            "gate_batch_count": role.gate_batch_count,
            "gate_observation_count": role.gate_observation_count,
        },
        "import_source_inventory": {
            "before": role.source_inventory_before.as_ref().map(import_source_inventory_json),
            "after": role.source_inventory_after.as_ref().map(import_source_inventory_json),
            "unchanged": match (
                role.source_inventory_before.as_ref(),
                role.source_inventory_after.as_ref(),
            ) {
                (Some(before), Some(after)) => Some(before == after),
                (None, None) => None,
                _ => Some(false),
            },
        },
        "cleanup": {
            "completed": role.cleanup_completed,
            "target_was_exact_attempt_local_path": role.cleanup_completed,
        },
        "product_gate_outcomes": role.product_gate_outcomes.iter().map(raw_product_gate_outcome_json).collect::<Vec<_>>(),
        "integrity_reason_codes": integrity_reason_codes(&role.reasons),
        "evidence_status": evidence_status(&role.reasons),
        "product_gate_status": product_gate_status(
            !has_integrity_reasons(&role.reasons),
            role.product_gate_outcomes.iter().any(|outcome| outcome.outcome == ProductGateStatus::Failed)
                || has_product_gate_failures(&role.reasons),
        ),
    })
}

fn evidence_status(reasons: &BTreeSet<String>) -> &'static str {
    if !has_integrity_reasons(reasons) {
        "valid_complete"
    } else {
        "invalid_or_incomplete"
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReasonAxis {
    Integrity,
    ProductGate,
}

fn reason_axis(reason: &str) -> ReasonAxis {
    if is_known_product_gate_reason(reason) {
        ReasonAxis::ProductGate
    } else {
        // Every product failure is intentionally enumerated below. Unknown
        // codes, including deceptively named prefix/suffix lookalikes, fail
        // closed on the integrity axis.
        ReasonAxis::Integrity
    }
}

fn is_known_product_gate_reason(reason: &str) -> bool {
    matches!(
        reason,
        "coordinated_settlement_gate_exceeded"
            | "cpu_decoded_residency_gate_exceeded"
            | "cpu_resource_gate_exceeded"
            | "cpu_upload_staging_gate_exceeded"
            | "exact_useful_sample_bytes_below_oracle"
            | "gpu_batch_envelope_gate_exceeded"
            | "gpu_resource_gate_exceeded"
            | "gpu_transfer_gate_exceeded"
            | "import_clock_limit_exceeded"
            | "import_receipt_expected_count_mismatch"
            | "import_receipt_source_read_amplification_exceeded"
            | "import_receipt_resource_limit_exceeded"
            | "import_receipt_primary_clock_limit_exceeded"
            | "imported_root_manifest_identity_mismatch"
            | "interaction_task_gate_exceeded"
            | "main_loop_gap_gate_exceeded"
            | "open_object_gate_exceeded"
            | "presentation_gap_gate_exceeded"
            | "resident_input_latency_gate_exceeded"
            | "runtime_queue_gate_exceeded"
            | "visible_layer_settlement_gate_exceeded"
            | "visible_panel_settlement_gate_exceeded"
            | "product_gate_coordinated_layout_complete_false"
            | "product_gate_import_workflow_run_counts_or_progress_claim_mismatch"
            | "product_gate_import_required_stage_or_progress_mismatch"
            | "product_gate_import_publication_currentness_observation_mismatch"
            | "product_gate_import_ordinary_source_verifier_activity_observed"
            | "product_gate_mapped_client_extent_mismatch"
            | "product_gate_canonical_camera_identity_mismatch"
            | "product_gate_canonical_camera_geometry_outside_contract"
            | "product_gate_canonical_camera_viewport_mismatch"
            | "product_gate_canonical_cross_section_schema_or_layout_mismatch"
            | "product_gate_canonical_active_view_mismatch"
            | "product_gate_canonical_cross_section_geometry_outside_contract"
            | "product_gate_phase_time_index_mismatch"
            | "product_gate_current_presentation_generation_mismatch"
            | "product_gate_current_complete_fidelity_false"
            | "product_gate_target_or_displayed_scale_mismatch"
            | "product_gate_gpu_timing_unavailable_without_expected_current_presentation"
            | "product_gate_cross_section_layer_scale_or_coverage_mismatch"
            | "product_gate_gpu_render_mode_mismatch"
            | "product_gate_visible_panel_milestone_set_mismatch"
            | "product_gate_visible_layer_milestone_overflow"
            | "product_gate_visible_layer_milestone_set_mismatch"
            | "product_gate_exact_resource_union_delta_partitions_pairwise_disjoint_mismatch"
            | "product_gate_exact_resource_union_delta_retained_payload_bytes_mismatch"
            | "product_gate_verification_accepted_progress_delta_below_gate"
            | "product_gate_nonresident_target_resident_target_intersection_mismatch"
            | "product_gate_nonresident_target_nonresident_target_difference_mismatch"
            | "product_gate_phase_durable_gesture_commit_delta_mismatch"
            | "product_gate_phase_application_currentness_delta_mismatch"
            | "product_gate_phase_durable_project_revision_delta_mismatch"
            | "product_gate_phase_undo_history_entry_delta_mismatch"
            | "product_gate_gesture_sequence_durable_commit_or_sample_delta_mismatch"
    ) || is_known_canonical_plane_product_reason(reason)
        || is_known_conformance_product_reason(reason)
        || is_known_exact_cross_scope_product_reason(reason)
        || is_known_exact_resource_delta_product_reason(reason)
        || is_known_verification_product_reason(reason)
        || is_known_unique_work_product_reason(reason)
        || is_known_structural_product_reason(reason)
}

fn is_known_conformance_product_reason(reason: &str) -> bool {
    if matches!(
        reason,
        "conformance_primary_numerical_result_not_passed"
            | "conformance_dvr_frozen_world_distance_oracle_failed"
            | "conformance_dvr_observed_coverage_mismatch"
            | "conformance_dvr_observed_validity_mismatch"
    ) {
        return true;
    }
    const CASES: [&str; 6] = [
        "plane_smooth_valid",
        "plane_smooth_invalid",
        "perspective_mip",
        "perspective_dvr_world_distance",
        "perspective_iso",
        "perspective_iso_depth_order",
    ];
    const OBSERVATIONS: [&str; 13] = [
        "pixel_mismatch",
        "rgba8_mismatch",
        "premultiplied_rgba_mismatch",
        "coverage_mismatch",
        "validity_mismatch",
        "authored_order_mismatch",
        "source_order_mismatch",
        "hit_depth_mismatch",
        "pick_kind_mismatch",
        "pick_completeness_mismatch",
        "pick_value_mismatch",
        "pick_world_mismatch",
        "pick_distance_mismatch",
    ];
    CASES.iter().any(|case| {
        let prefix = format!("conformance_{case}_");
        reason
            .strip_prefix(&prefix)
            .is_some_and(|suffix| OBSERVATIONS.contains(&suffix))
    })
}

fn is_known_canonical_plane_product_reason(reason: &str) -> bool {
    let Some(panel) = reason
        .strip_prefix("product_gate_canonical_")
        .and_then(|value| value.strip_suffix("_plane_geometry_outside_contract"))
    else {
        return false;
    };
    matches!(panel, "xy" | "xz" | "yz")
}

fn is_known_exact_cross_scope_product_reason(reason: &str) -> bool {
    let Some(rest) = reason.strip_prefix("product_gate_exact_cross_scope_") else {
        return false;
    };
    let Some(field) = ["start", "target", "residency_baseline"]
        .iter()
        .find_map(|position| rest.strip_prefix(&format!("{position}_union_")))
    else {
        return false;
    };
    matches!(
        field,
        "canonical_entries_sha256_mismatch"
            | "unique_keys_mismatch"
            | "unique_payload_bytes_mismatch"
            | "summed_scope_payload_bytes_mismatch"
    )
}

fn is_known_exact_resource_delta_product_reason(reason: &str) -> bool {
    let Some(field) = reason
        .strip_prefix("product_gate_exact_resource_union_delta_")
        .and_then(|value| value.strip_suffix("_mismatch"))
    else {
        return false;
    };
    matches!(
        field,
        "retained_entries_sha256"
            | "added_entries_sha256"
            | "removed_entries_sha256"
            | "retained_unique_keys"
            | "retained_unique_payload_bytes"
            | "added_unique_keys"
            | "added_unique_payload_bytes"
            | "removed_unique_keys"
            | "removed_unique_payload_bytes"
            | "partitions_pairwise_disjoint"
            | "retained_payload_bytes"
    )
}

fn is_known_verification_product_reason(reason: &str) -> bool {
    let Some(rest) = reason.strip_prefix("product_gate_verification_") else {
        return false;
    };
    ["start", "end"].iter().any(|position| {
        rest.strip_prefix(&format!("{position}_"))
            .is_some_and(|field| {
                matches!(
                    field,
                    "state_or_active_operation_mismatch"
                        | "service_started_runs_mismatch"
                        | "service_cancelled_runs_mismatch"
                        | "service_failed_runs_mismatch"
                        | "service_accepted_successes_mismatch"
                        | "service_completed_reader_runs_mismatch"
                )
            })
    })
}

fn is_known_unique_work_product_reason(reason: &str) -> bool {
    let Some(label) = reason
        .strip_prefix("unique_work_")
        .and_then(|value| value.strip_suffix("_delta_outside_oracle"))
    else {
        return false;
    };
    matches!(
        label,
        "physical_range_read_operations"
            | "physical_encoded_bytes_read"
            | "codec_decode_operations"
            | "codec_decoded_bytes"
            | "dataset_submitted_requests"
            | "dataset_started_decodes"
            | "runtime_decoded_output_bytes"
            | "gpu_uploaded_resources"
            | "gpu_uploaded_payload_bytes"
            | "gpu_control_dynamic_updates"
            | "gpu_control_dynamic_upload_bytes"
            | "gpu_control_publication_writes"
    )
}

fn is_known_structural_product_reason(reason: &str) -> bool {
    let Some(rest) = reason.strip_prefix("structural_") else {
        return false;
    };
    if let Some(label) = rest.strip_suffix("_counter_changed") {
        return is_known_zero_work_counter_label(label);
    }
    rest.strip_suffix("_ceiling_exceeded")
        .is_some_and(is_known_structural_ceiling_label)
}

fn is_known_zero_work_counter_label(label: &str) -> bool {
    matches!(
        label,
        "physical_range_reads"
            | "codec_decodes"
            | "object_opens"
            | "dataset_requests"
            | "dataset_decodes"
            | "cancelled_requests"
            | "payload_uploads"
            | "residency_evictions"
            | "static_control_rebuilds"
            | "dense_control_fallbacks"
            | "queue_submissions"
            | "payload_reuploads"
            | "arena_allocator_plans"
            | "gpu_control_buffer_allocations"
            | "gpu_bind_group_creations"
            | "gpu_pipeline_creations"
            | "residency_directory_updates"
            | "page_layout_constructions"
            | "page_table_updates"
            | "full_demand_traversals"
            | "planner_candidate_visits"
            | "ui_thread_candidate_visits"
            | "ui_wait_for_demand_preparation"
            | "renderer_static_preparations"
            | "cancellation_churn"
            | "cancellation_waste_encoded_bytes"
            | "cancellation_waste_decoded_bytes"
            | "cancellation_waste_uploaded_bytes"
            | "cancellation_waste_cpu_time_ns"
            | "durable_project_revisions"
            | "undo_history_entries"
            | "encoded_display_batches"
            | "encoded_but_dropped_batches"
            | "sealed_obsolete_submitted_batches"
            | "stale_presented_batches"
            | "renderer_submissions"
            | "presentation_churn"
            | "demand_work"
    )
}

fn is_known_structural_ceiling_label(label: &str) -> bool {
    matches!(
        label,
        "pending_display_batches_peak"
            | "in_flight_display_batches_peak"
            | "command_encoders"
            | "color_passes"
            | "renderer_submissions"
            | "completion_notifications"
            | "backpressure_deferrals"
            | "encoded_display_batches"
            | "encoded_but_dropped_batches"
            | "sealed_obsolete_submitted_batches"
            | "stale_presentations"
            | "current_presentations"
            | "demand_work"
            | "cancellation_waste_started_decode_count"
            | "cancellation_waste_decoded_bytes"
            | "cancellation_waste_cpu_time_ns"
            | "cancellation_waste_encoded_bytes"
            | "cancellation_waste_uploaded_bytes"
    )
}

fn integrity_reason_codes(reasons: &BTreeSet<String>) -> Vec<&str> {
    reasons
        .iter()
        .filter(|reason| reason_axis(reason) == ReasonAxis::Integrity)
        .map(String::as_str)
        .collect()
}

fn has_integrity_reasons(reasons: &BTreeSet<String>) -> bool {
    reasons
        .iter()
        .any(|reason| reason_axis(reason) == ReasonAxis::Integrity)
}

fn has_product_gate_failures(reasons: &BTreeSet<String>) -> bool {
    reasons
        .iter()
        .any(|reason| reason_axis(reason) == ReasonAxis::ProductGate)
}

fn product_gate_status(evidence_valid: bool, has_failed_gate: bool) -> &'static str {
    if !evidence_valid {
        "not_authoritative"
    } else if has_failed_gate {
        "failed"
    } else {
        "passed"
    }
}

fn raw_product_gate_outcome_json(outcome: &ProductGateOutcome) -> Value {
    json!({
        "schema": PRODUCT_GATE_OBSERVATION_SCHEMA,
        "command_index": outcome.command_index,
        "batch_id": outcome.batch_id,
        "phase_id": outcome.phase_id,
        "observation_index": outcome.observation_index,
        "gate_id": outcome.gate_id,
        "condition": outcome.condition,
        "deadline_authority": outcome.deadline_authority,
        "deadline_after_origin_ns": outcome.deadline_after_origin_ns,
        "origin": {
            "kind": outcome.origin_kind,
            "command_index": outcome.origin_command_index,
        },
        "outcome": outcome.outcome.report_label(),
        "condition_met": outcome.condition_met,
        "timed_out": outcome.timed_out,
        "observed_after_origin_ns": outcome.observed_after_origin_ns,
    })
}

fn raw_product_gate_outcome_rows(samples: &[SampleEvidence]) -> Vec<Value> {
    let mut rows = Vec::new();
    for sample in samples {
        for role in std::iter::once(&sample.instrumented).chain(sample.control.iter()) {
            for outcome in &role.product_gate_outcomes {
                let mut row = raw_product_gate_outcome_json(outcome);
                let object = row
                    .as_object_mut()
                    .expect("product gate outcome JSON is always an object");
                object.insert("sample_index".to_owned(), json!(sample.sample_index));
                object.insert("scenario".to_owned(), json!(sample.scenario));
                object.insert("role".to_owned(), json!(role.role.directory_name()));
                rows.push(row);
            }
        }
    }
    rows
}

fn sanitized_product_gate_outcome_rows(samples: &[SampleEvidence]) -> Vec<Value> {
    let mut rows = Vec::new();
    for sample in samples {
        for role in std::iter::once(&sample.instrumented).chain(sample.control.iter()) {
            for outcome in &role.product_gate_outcomes {
                rows.push(json!({
                    "sample_index": sample.sample_index,
                    "scenario": sample.scenario,
                    "role": role.role.directory_name(),
                    "batch_id": outcome.batch_id,
                    "phase_id": outcome.phase_id,
                    "observation_index": outcome.observation_index,
                    "gate_id": outcome.gate_id,
                    "condition": outcome.condition,
                    "deadline_authority": outcome.deadline_authority,
                    "deadline_after_origin_ns": outcome.deadline_after_origin_ns,
                    "outcome": outcome.outcome.report_label(),
                }));
            }
        }
    }
    rows
}

fn sanitized_role_schedule_rows(samples: &[SampleEvidence]) -> Vec<Value> {
    let mut rows = Vec::new();
    for sample in samples {
        for role in std::iter::once(&sample.instrumented).chain(sample.control.iter()) {
            rows.push(json!({
                "sample_index": sample.sample_index,
                "scenario": sample.scenario,
                "role": role.role.directory_name(),
                "launch_attempted": role.process.launch_attempted,
                "gate_batch_count": role.gate_batch_count,
                "gate_observation_count": role.gate_observation_count,
                "static_wait_bound_ns": role.static_wait_bound_ns,
                "derived_process_timeout_ns": role.derived_process_timeout_ns,
            }));
        }
    }
    rows
}

fn classified_product_gate_failure_rows(samples: &[SampleEvidence]) -> Vec<Value> {
    let mut rows = Vec::new();
    for sample in samples {
        push_classified_product_gate_failures(
            &mut rows,
            sample,
            "instrumentation_pair",
            "sample",
            &sample.reasons,
        );
        push_classified_product_gate_failures(
            &mut rows,
            sample,
            sample.instrumented.role.directory_name(),
            "role",
            &sample.instrumented.reasons,
        );
        if let Some(control) = &sample.control {
            push_classified_product_gate_failures(
                &mut rows,
                sample,
                control.role.directory_name(),
                "role",
                &control.reasons,
            );
        }
        for phase in &sample.phases {
            push_classified_product_gate_failures(
                &mut rows,
                sample,
                AttemptRole::Instrumented.directory_name(),
                &phase.name,
                &phase.reasons,
            );
        }
    }
    rows
}

fn push_classified_product_gate_failures(
    rows: &mut Vec<Value>,
    sample: &SampleEvidence,
    role: &str,
    scope: &str,
    reasons: &BTreeSet<String>,
) {
    for reason in reasons
        .iter()
        .filter(|reason| reason_axis(reason) == ReasonAxis::ProductGate)
    {
        let identity = format!("{}.{}.{}", sample.scenario, scope, reason);
        rows.push(json!({
            "sample_index": sample.sample_index,
            "scenario": sample.scenario,
            "role": role,
            "gate_id": bounded_product_gate_id(&identity),
            "condition": "accepted_gate_satisfied",
            "outcome": "failed",
        }));
    }
}

fn bounded_product_gate_id(reason: &str) -> String {
    if validate_product_gate_id(reason).is_ok() {
        reason.to_owned()
    } else {
        format!("gate.{}", Sha256Hasher::digest(reason.as_bytes()))
    }
}

fn population_evidence_is_exact(population: PopulationEvidence) -> bool {
    population.expected_sample_records == population.observed_sample_records
        && population.expected_role_attempts == population.observed_role_attempts
        && population.expected_role_attempts == population.completed_role_reports
        && population.expected_phase_evaluations == population.observed_phase_evaluations
        && population.expected_product_gate_observations
            == population.observed_product_gate_observations
        && population.sample_identities_exact
        && population.sample_order_exact
        && population.role_identities_exact
        && population.role_order_exact
        && population.phase_identities_exact
        && population.product_gate_bijections_exact
}

fn instrumentation_overhead_population_rows(
    populations: &[InstrumentationOverheadPopulationEvidence],
) -> Vec<Value> {
    populations
        .iter()
        .map(|population| {
            json!({
                "scenario": population.scenario,
                "evaluation_scope": "complete_balanced_development_sample_population",
                "automatic_retries": 0,
                "sample_filtering": "none",
                "expected_sample_pairs": population.expected_sample_pairs,
                "observed_sample_pairs": population.observed_sample_pairs,
                "wall_adjustment_authority": "exact_sum_of_successful_qualification_only_await_active_view_gpu_timing_waited_ns",
                "instrumented_raw_app_wall_time_ns": population.instrumented_raw_app_wall_time_ns,
                "instrumented_qualification_gpu_timing_await_wall_time_ns": population.instrumented_qualification_wait_wall_ns,
                "instrumented_adjusted_app_wall_time_ns": population.instrumented_adjusted_app_wall_time_ns,
                "control_app_wall_time_ns": population.control_app_wall_time_ns,
                "wall_overhead_basis_points": population.wall_overhead_basis_points,
                "instrumented_process_cpu_time_ns": population.instrumented_process_cpu_time_ns,
                "control_process_cpu_time_ns": population.control_process_cpu_time_ns,
                "process_cpu_overhead_basis_points": population.process_cpu_overhead_basis_points,
                "maximum_overhead_basis_points": population.maximum_overhead_basis_points,
                "population_complete": population.population_complete,
                "gate_evaluable": population.gate_evaluable,
                "gate_passed": population.gate_passed,
            })
        })
        .collect()
}

fn population_json(population: PopulationEvidence) -> Value {
    json!({
        "expected_sample_records": population.expected_sample_records,
        "observed_sample_records": population.observed_sample_records,
        "expected_role_attempts": population.expected_role_attempts,
        "observed_role_attempts": population.observed_role_attempts,
        "completed_role_reports": population.completed_role_reports,
        "expected_phase_evaluations": population.expected_phase_evaluations,
        "observed_phase_evaluations": population.observed_phase_evaluations,
        "expected_product_gate_observations": population.expected_product_gate_observations,
        "observed_product_gate_observations": population.observed_product_gate_observations,
        "sample_identities_exact": population.sample_identities_exact,
        "sample_order_exact": population.sample_order_exact,
        "role_identities_exact": population.role_identities_exact,
        "phase_identities_exact": population.phase_identities_exact,
        "product_gate_bijections_exact": population.product_gate_bijections_exact,
        "exact": population_evidence_is_exact(population),
    })
}

fn import_source_inventory_json(facts: &super::source_inventory::InventoryFacts) -> Value {
    json!({
        "regular_files": facts.regular_files,
        "source_bytes": facts.source_bytes,
        "inventory_sha256": facts.sha256,
    })
}

#[allow(clippy::too_many_arguments)]
fn sanitized_receipt(
    profile: &LoadedProfile,
    workload: &LoadedBundle<WorkloadBundle>,
    scripts: &LoadedBundle<ScriptBundle>,
    oracle: &LoadedBundle<OracleBundle>,
    app_binary_sha256: &str,
    raw_sha256: &str,
    conformance: Option<&ConformanceEvidence>,
    samples: &[SampleEvidence],
    population: PopulationEvidence,
    instrumentation_overhead_populations: &[InstrumentationOverheadPopulationEvidence],
    reasons: &BTreeSet<String>,
) -> Value {
    let product_gate_outcomes = sanitized_product_gate_outcome_rows(samples);
    let product_gate_failures = classified_product_gate_failure_rows(samples);
    json!({
        "schema": RECEIPT_SCHEMA,
        "evidence_status": evidence_status(reasons),
        "product_gate_status": product_gate_status(
            !has_integrity_reasons(reasons),
            product_gate_outcomes.iter().any(|row| {
                row.get("outcome").and_then(Value::as_str) == Some("failed")
            }) || !product_gate_failures.is_empty() || has_product_gate_failures(reasons),
        ),
        "claim_status": "development_E1_non_OS_input_non_E4_no_product_claim",
        "commitments": {
            "qualification_profile_sha256": profile.sha256,
            "owner_accepted_profile_contract_sha256": profile_contract_sha256(&profile.profile),
            "ep01_selection_authority_sha256": profile.profile.ep01_selection_authority_sha256,
            "workload_bundle_sha256": workload.sha256,
            "ep01_trace_geometry_sha256": ep01_trace_geometry_sha256(&workload.value.ep01_trace_geometry),
            "interaction_script_bundle_sha256": scripts.sha256,
            "independent_oracle_sha256": oracle.sha256,
            "app_binary_sha256": app_binary_sha256,
            "private_raw_report_sha256": raw_sha256,
            "representative_package_fingerprint_sha256": commitment_fingerprint(
                "representative-package",
                &profile.profile.workload.representative_package.root_manifest_sha256,
            ),
            "supporting_temporal_package_fingerprint_sha256": commitment_fingerprint(
                "supporting-temporal-package",
                &workload.value.supporting_temporal_package_root_manifest_sha256,
            ),
            "build_binding_fingerprint_sha256": super::build_binding_fingerprint(
                &profile.profile.build,
            ),
        },
        "executable_conformance": conformance.map(ConformanceEvidence::sanitized_json),
        "population": population_json(population),
        "instrumentation_overhead_populations": instrumentation_overhead_population_rows(
            instrumentation_overhead_populations,
        ),
        "product_gate_outcomes": product_gate_outcomes,
        "role_schedule_bounds": sanitized_role_schedule_rows(samples),
        "product_gate_failures": product_gate_failures,
        "integrity_reason_codes": integrity_reason_codes(reasons),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep01_trace_geometry() -> Ep01TraceGeometry {
        Ep01TraceGeometry {
            derivation_contract: EP01_TRACE_DERIVATION_CONTRACT.to_owned(),
            package_role: EP01_TRACE_PACKAGE_ROLE.to_owned(),
            whole_layer: vec![
                Ep01WholeLayerTrace {
                    logical_layer_ordinal: 0,
                    time_index: 0,
                    scale_level: 0,
                },
                Ep01WholeLayerTrace {
                    logical_layer_ordinal: 1,
                    time_index: 3,
                    scale_level: 2,
                },
            ],
            numeric_boxes: vec![
                Ep01NumericBoxTrace {
                    logical_layer_ordinal: 0,
                    time_index: 0,
                    scale_level: 0,
                    start_zyx: [0, 1, 2],
                    end_zyx_exclusive: [1, 5, 8],
                },
                Ep01NumericBoxTrace {
                    logical_layer_ordinal: 1,
                    time_index: 3,
                    scale_level: 2,
                    start_zyx: [4, 5, 6],
                    end_zyx_exclusive: [9, 10, 11],
                },
            ],
        }
    }

    fn timing_ring(total_count: u64, retained: &[u64]) -> Value {
        json!({
            "capacity": 256,
            "total_count": total_count,
            "retained_count": retained.len(),
            "overwritten_count": total_count.saturating_sub(retained.len() as u64),
            "maximum_ns": retained.iter().copied().max().unwrap_or_default(),
            "p95_ns": sample_p95(retained),
            "retained_samples_ns_oldest_first": retained,
        })
    }

    fn numerical_contract() -> NumericalContract {
        NumericalContract {
            scalar_absolute_tolerance: 1.0e-6,
            scalar_relative_tolerance: f64::from(f32::EPSILON) * 4.0,
            premultiplied_rgba_absolute_tolerance: 2.0e-6,
            world_position_absolute_tolerance: 1.0e-5,
            ray_distance_absolute_tolerance: 1.0e-5,
            rgba8_channel_tolerance: 1,
            coverage_exact: true,
            validity_exact: true,
            source_order_exact: true,
            sample_ordinal_exact: true,
            pick_kind_exact: true,
            pick_completeness_exact: true,
        }
    }

    fn phase_state() -> PhaseStateBinding {
        let plane = |panel, origin, u, v, normal| ExpectedCrossSectionPlane {
            panel,
            plane_origin_world: origin,
            u_axis_world: u,
            v_axis_world: v,
            normal_away_world: normal,
            world_per_screen_point: 1.0,
        };
        PhaseStateBinding {
            checkpoint_label: "end".to_owned(),
            render_extent: super::super::PixelExtent {
                width: 1280,
                height: 720,
            },
            mapped_client_extent: super::super::PixelExtent {
                width: 1280,
                height: 720,
            },
            layout: ExpectedViewerLayout::FourPanel,
            active_view: ViewerPanel::Xy,
            time_index: 0,
            camera: ExpectedCameraGeometry {
                projection: ExpectedProjection::Orthographic,
                target_world: [0.0, 0.0, 0.0],
                orientation_xyzw: [0.0, 0.0, 0.0, 1.0],
                orthographic_world_per_screen_point: 1.0,
                perspective_focal_length_screen_points: 1000.0,
                perspective_view_distance_world: 1000.0,
            },
            cross_section: ExpectedCrossSectionGeometry {
                center_world: [0.0, 0.0, 0.0],
                orientation_xyzw: [0.0, 0.0, 0.0, 1.0],
                world_per_screen_point: 1.0,
                depth_world: 0.0,
                planes: vec![
                    plane(
                        CrossSectionPanel::Xy,
                        [0.0, 0.0, 0.0],
                        [1.0, 0.0, 0.0],
                        [0.0, 1.0, 0.0],
                        [0.0, 0.0, 1.0],
                    ),
                    plane(
                        CrossSectionPanel::Xz,
                        [0.0, 0.0, 0.0],
                        [1.0, 0.0, 0.0],
                        [0.0, 0.0, 1.0],
                        [0.0, -1.0, 0.0],
                    ),
                    plane(
                        CrossSectionPanel::Yz,
                        [0.0, 0.0, 0.0],
                        [0.0, 1.0, 0.0],
                        [0.0, 0.0, 1.0],
                        [1.0, 0.0, 0.0],
                    ),
                ],
            },
            layers: vec![ExpectedLayerState {
                layer_ordinal: 0,
                source_order: 0,
                visible: true,
                scale_level: 0,
                sampling: "voxel_exact".to_owned(),
                mode: "mip".to_owned(),
                window: [0.0, 255.0],
                gamma: 1.0,
                inverted: false,
                opacity: 1.0,
                color_rgba: [1.0, 1.0, 1.0, 1.0],
            }],
            ray_step_rule: ExpectedRayStepRule {
                rule: "one_voxel_world".to_owned(),
                step_world: 1.0,
                maximum_steps: 4096,
            },
            dvr_density_scale: None,
            iso_display_level: None,
            iso_shading: None,
            iso_light: None,
        }
    }

    fn resident_oracle_phase() -> OraclePhase {
        OraclePhase {
            name: "resident_3d_zoom".to_owned(),
            phase_state: phase_state(),
            require_interaction_metrics: true,
            require_current_complete: true,
            require_coordinated_layout_complete: true,
            expected_scale_level: Some(0),
            expected_cross_section_layers: Vec::new(),
            gpu_gate: Some(GpuGate::Mip),
            settlement_gate: None,
            verification_gate: None,
            phase_start_target_residency: None,
            structural_gate: StructuralGate {
                kind: StructuralGateKind::ResidentGesture,
                display_batch_authority: DisplayBatchAuthority::SynchronousUiThreadPredecessor,
                cancellation_waste_authority: CancellationWasteAuthority::PredecessorUnattributed,
                ceilings: Some(structural_ceilings()),
            },
            zero_work_counters: ZeroWorkCounter::RESIDENT_MANDATORY.to_vec(),
            unique_work: unique_work_expectation(1),
            minimum_exact_useful_sample_bytes: Some(1),
            expected_imported_root_manifest_sha256: None,
            import_gate: None,
        }
    }

    fn unique_work_expectation(value: u64) -> UniqueWorkExpectation {
        let range = || InclusiveU64Range {
            minimum: value,
            maximum: value,
            authority: IndependentRangeAuthority {
                kind: IndependentRangeAuthorityKind::ExactIndependentEnumeration,
                fact_id: "test-exact-work-enumeration".to_owned(),
                independent_fact_sha256: "44".repeat(32),
            },
        };
        UniqueWorkExpectation {
            start_union: ExactResourceUnion {
                canonical_entries_sha256: "11".repeat(32),
                unique_keys: 0,
                unique_payload_bytes: 0,
                summed_scope_payload_bytes: 0,
            },
            target_union: ExactResourceUnion {
                canonical_entries_sha256: "22".repeat(32),
                unique_keys: value,
                unique_payload_bytes: value,
                summed_scope_payload_bytes: value,
            },
            residency_baseline: None,
            delta_union: ExactResourceUnionDelta {
                partitions_pairwise_disjoint: true,
                retained_entries_sha256: "11".repeat(32),
                retained_unique_keys: 0,
                retained_unique_payload_bytes: 0,
                added_entries_sha256: "22".repeat(32),
                added_unique_keys: value,
                added_unique_payload_bytes: value,
                removed_entries_sha256: "11".repeat(32),
                removed_unique_keys: 0,
                removed_unique_payload_bytes: 0,
            },
            physical_range_read_operations: range(),
            physical_encoded_bytes_read: range(),
            codec_decode_operations: range(),
            codec_decoded_bytes: range(),
            dataset_submitted_requests: range(),
            dataset_started_decodes: range(),
            runtime_decoded_output_bytes: range(),
            gpu_uploaded_resources: range(),
            gpu_uploaded_payload_bytes: range(),
            gpu_control_dynamic_updates: range(),
            gpu_control_dynamic_upload_bytes: range(),
            gpu_control_publication_writes: range(),
        }
    }

    fn template(instrumented: bool, commands: Vec<Value>) -> AutomationScriptTemplate {
        AutomationScriptTemplate {
            schema: AUTOMATION_SCRIPT_SCHEMA.to_owned(),
            schema_version: AUTOMATION_SCRIPT_SCHEMA_VERSION,
            scenario: "RZ".to_owned(),
            gpu_timing: instrumented,
            diagnostic_counters: instrumented,
            startup_bootstrap: None,
            hard_safety_limits: AutomationHardSafetyLimits::default(),
            commands,
        }
    }

    fn dataset_contract_scenario(id: &str) -> ScriptScenario {
        let mut instrumented_script = template(
            true,
            vec![
                json!({ "command": "open_dataset", "path": "/private/representative.m4d" }),
                json!({ "command": "cancel_active_source_verification" }),
                json!({
                    "command": "wait_for",
                    "condition": "source_verification_inactive",
                    "timeout_ms": SOURCE_VERIFICATION_QUIESCENCE_TIMEOUT_MS,
                }),
                json!({ "command": "switch_dataset", "path": "/private/temporal.m4d" }),
                json!({ "command": "cancel_active_source_verification" }),
                json!({
                    "command": "wait_for",
                    "condition": "source_verification_inactive",
                    "timeout_ms": SOURCE_VERIFICATION_QUIESCENCE_TIMEOUT_MS,
                }),
                json!({ "command": "sample_diagnostics", "label": "pt-advance-start" }),
                json!({ "command": "quit" }),
            ],
        );
        instrumented_script.scenario = id.to_owned();
        ScriptScenario {
            id: id.to_owned(),
            phases: vec![ScriptPhase {
                name: "advance_timepoint".to_owned(),
                start_diagnostic_label: Some("pt-advance-start".to_owned()),
                end_diagnostic_label: "pt-advance-end".to_owned(),
            }],
            instrumented_script,
            instrumentation_control_script: None,
            cleanup: AttemptCleanup::default(),
        }
    }

    fn ip_action_scenario() -> ScriptScenario {
        let mut instrumented_script = template(
            true,
            vec![
                json!({ "command": "open_dataset", "path": "/private/representative.m4d" }),
                json!({ "command": "sample_diagnostics", "label": "ip-start" }),
                json!({
                    "command": "begin_tiff_import_setup",
                    "source": "/private/source.tif",
                    "output_parent": "${ATTEMPT_ROOT}/output",
                }),
                json!({ "command": "start_reviewed_import" }),
                json!({
                    "command": "observe_gate_batch",
                    "batch_id": "IP.batch.000",
                    "phase_id": "preprocess_publish.checkpoint.000",
                    "origin": { "kind": "import_primary_started" },
                    "observations": [
                        {
                            "gate_id": "IP.acceptance.000.import_idle",
                            "deadline_authority": "import_primary_wall",
                            "deadline_after_origin_ns": 1_200_000_000_000_u64,
                            "target": { "kind": "condition", "condition": "import_idle" },
                        },
                        {
                            "gate_id": "IP.acceptance.001.imported_open_ready",
                            "deadline_authority": "import_primary_wall",
                            "deadline_after_origin_ns": 1_200_000_000_000_u64,
                            "target": {
                                "kind": "imported_open_ready",
                                "path": "${ATTEMPT_ROOT}/output/source.m4d",
                            },
                        },
                        {
                            "gate_id": "IP.acceptance.002.runtime_idle",
                            "deadline_authority": "import_primary_wall",
                            "deadline_after_origin_ns": 1_200_000_000_000_u64,
                            "target": { "kind": "condition", "condition": "runtime_idle" },
                        },
                    ],
                }),
                json!({ "command": "sample_diagnostics", "label": "ip-end" }),
                json!({ "command": "quit" }),
            ],
        );
        instrumented_script.scenario = "IP".to_owned();
        ScriptScenario {
            id: "IP".to_owned(),
            phases: vec![ScriptPhase {
                name: "preprocess_publish".to_owned(),
                start_diagnostic_label: Some("ip-start".to_owned()),
                end_diagnostic_label: "ip-end".to_owned(),
            }],
            instrumented_script,
            instrumentation_control_script: None,
            cleanup: AttemptCleanup {
                enabled: true,
                imported_package_relative_path: Some(PathBuf::from("output/source.m4d")),
            },
        }
    }

    fn set_ip_source(scenario: &mut ScriptScenario, source: &Path) {
        let command = scenario
            .instrumented_script
            .commands
            .iter_mut()
            .find(|command| {
                command.get("command").and_then(Value::as_str) == Some("begin_tiff_import_setup")
            })
            .expect("the IP test script has one setup command");
        command["source"] = json!(
            source
                .to_str()
                .expect("temporary test source paths are valid UTF-8")
        );
    }

    fn import_source_binding(template: &AutomationScriptTemplate) -> ImportSourceBinding {
        let facts = capture_import_source(template).unwrap();
        ImportSourceBinding {
            inventory_sha256: facts.sha256,
            reviewed_source_fingerprint_sha256: "44".repeat(32),
            regular_files: facts.regular_files,
            source_bytes: facts.source_bytes,
        }
    }

    fn ip_oracle_scenario() -> OracleScenario {
        OracleScenario {
            id: "IP".to_owned(),
            phases: vec![matrix_phase("IP", "preprocess_publish")],
        }
    }

    fn product_gate_outcome(gate_id: &str, outcome: ProductGateStatus) -> ProductGateOutcome {
        let failed = outcome == ProductGateStatus::Failed;
        let scenario = gate_id.split('.').next().unwrap_or("RZ");
        ProductGateOutcome {
            command_index: 1,
            batch_id: format!("{scenario}.batch.000"),
            phase_id: format!("{scenario}-phase.checkpoint.000"),
            observation_index: 0,
            gate_id: gate_id.to_owned(),
            condition: "coordinated_presentation_settled".to_owned(),
            deadline_authority: "maximum_current_presentation_gap_plus_poll_grace".to_owned(),
            deadline_after_origin_ns: 66_666_668,
            origin_kind: "command_completed".to_owned(),
            origin_command_index: Some(0),
            outcome,
            condition_met: !failed,
            timed_out: failed,
            observed_after_origin_ns: if failed { 66_666_668 } else { 1_000_000 },
        }
    }

    fn production_population_contract(scenario: &str) -> (usize, usize) {
        match scenario {
            "RZ" => (2, 16),
            "ZB" => (2, 24),
            "RO" => (1, 16),
            "ST" => (1, 8),
            "NO" => (1, 8),
            "FC" => (2, 12),
            "VM" => (4, 32),
            "PT" => (2, 12),
            "VV" => (2, 14),
            "IP" => (1, 6),
            _ => panic!("unknown production viewer scenario"),
        }
    }

    fn population_role(role: AttemptRole, scenario: &str, gate_count: usize) -> RoleEvidence {
        RoleEvidence {
            role,
            root: PathBuf::from("/private/secret/attempt"),
            expanded_script_sha256: "11".repeat(32),
            template_script_sha256: "22".repeat(32),
            process: ProcessObservation {
                launch_attempted: true,
                status: None,
                external_wall_time_ns: 1,
                timed_out: false,
                spawn_error: None,
            },
            automation_report: None,
            automation_report_sha256: Some("33".repeat(32)),
            app_wall_time_ns: Some(1),
            process_cpu_time_ns: Some(1),
            derived_process_timeout_ns: 10_066_666_668,
            static_wait_bound_ns: 66_666_668,
            gate_batch_count: 1,
            gate_observation_count: 1,
            source_inventory_before: None,
            source_inventory_after: None,
            cleanup_manifest_sha256: None,
            cleanup_completed: false,
            product_gate_outcomes: (0..gate_count)
                .map(|observation_index| {
                    let mut outcome = product_gate_outcome(
                        &format!("{scenario}.acceptance.{observation_index:03}.settled"),
                        ProductGateStatus::Passed,
                    );
                    outcome.observation_index = observation_index;
                    outcome
                })
                .collect(),
            reasons: BTreeSet::new(),
        }
    }

    fn population_scripts() -> ScriptBundle {
        ScriptBundle {
            schema: SCRIPT_BUNDLE_SCHEMA.to_owned(),
            scenarios: REQUIRED_SCENARIOS
                .into_iter()
                .map(|id| {
                    let (phase_count, total_gate_count) = production_population_contract(id);
                    let gate_count = total_gate_count / 2;
                    let gate_command = json!({
                        "command": "observe_gate_batch",
                        "batch_id": format!("{id}.batch.000"),
                        "phase_id": format!("{id}-phase.checkpoint.000"),
                        "origin": { "kind": "command_completed", "command_index": 0 },
                        "observations": (0..gate_count)
                            .map(|observation_index| json!({
                                "gate_id": format!(
                                    "{id}.acceptance.{observation_index:03}.settled"
                                ),
                                "deadline_authority": "maximum_current_presentation_gap_plus_poll_grace",
                                "deadline_after_origin_ns": 66_666_668_u64,
                                "target": {
                                    "kind": "condition",
                                    "condition": "coordinated_presentation_settled",
                                },
                            }))
                            .collect::<Vec<_>>(),
                    });
                    let predecessor = json!({ "command": "sleep_frames", "frames": 1 });
                    let mut instrumented =
                        template(true, vec![predecessor.clone(), gate_command.clone()]);
                    instrumented.scenario = id.to_owned();
                    let mut control = template(false, vec![predecessor, gate_command]);
                    control.scenario = id.to_owned();
                    ScriptScenario {
                        id: id.to_owned(),
                        phases: (0..phase_count)
                            .map(|phase_index| ScriptPhase {
                                name: format!("{id}-phase-{phase_index}"),
                                start_diagnostic_label: Some(format!(
                                    "{id}-start-{phase_index}"
                                )),
                                end_diagnostic_label: format!("{id}-end-{phase_index}"),
                            })
                            .collect(),
                        instrumented_script: instrumented,
                        instrumentation_control_script: Some(control),
                        cleanup: AttemptCleanup::default(),
                    }
                })
                .collect(),
        }
    }

    fn complete_population_samples() -> Vec<SampleEvidence> {
        (1..=3)
            .flat_map(|sample_index| {
                REQUIRED_SCENARIOS.into_iter().map(move |id| {
                    let (phase_count, total_gate_count) = production_population_contract(id);
                    let role_gate_count = total_gate_count / 2;
                    SampleEvidence {
                        sample_index,
                        scenario: id.to_owned(),
                        role_launch_order: balanced_role_order(
                            sample_index,
                            REQUIRED_SCENARIOS
                                .iter()
                                .position(|scenario| *scenario == id)
                                .unwrap(),
                        )
                        .to_vec(),
                        instrumented: population_role(
                            AttemptRole::Instrumented,
                            id,
                            role_gate_count,
                        ),
                        control: Some(population_role(
                            AttemptRole::InstrumentationControl,
                            id,
                            role_gate_count,
                        )),
                        phases: (0..phase_count)
                            .map(|phase_index| PhaseEvaluation {
                                name: format!("{id}-phase-{phase_index}"),
                                reasons: BTreeSet::new(),
                            })
                            .collect(),
                        instrumented_qualification_wait_wall_ns: Some(0),
                        instrumented_adjusted_wall_time_ns: Some(1),
                        wall_overhead_basis_points: Some(0),
                        process_cpu_overhead_basis_points: Some(0),
                        reasons: BTreeSet::new(),
                    }
                })
            })
            .collect()
    }

    fn population_oracle_scenario(scenario: &ScriptScenario) -> OracleScenario {
        OracleScenario {
            id: scenario.id.clone(),
            phases: scenario
                .phases
                .iter()
                .map(|phase| {
                    let mut oracle = matrix_phase(&scenario.id, "resident_3d_zoom");
                    oracle.name = phase.name.clone();
                    oracle
                })
                .collect(),
        }
    }

    #[test]
    fn role_integrity_stops_the_balanced_mate_without_suppressing_product_failures() {
        let profile = profile();
        let numerical = numerical_contract();
        let scenario = population_scripts()
            .scenarios
            .into_iter()
            .find(|scenario| scenario.id == "RZ")
            .unwrap();
        let oracle = population_oracle_scenario(&scenario);
        let result_root = Path::new("/private/result");

        let mut control_first_calls = Vec::new();
        let control_first = execute_sample_with_role_executor(
            &profile,
            &numerical,
            1,
            &scenario,
            &oracle,
            result_root,
            |role, _| {
                control_first_calls.push(role);
                let mut evidence = population_role(role, "RZ", 8);
                evidence
                    .reasons
                    .insert("automation_report_missing_or_invalid".to_owned());
                evidence
            },
        );
        assert_eq!(
            control_first_calls,
            vec![AttemptRole::InstrumentationControl]
        );
        assert!(
            control_first
                .instrumented
                .reasons
                .contains("population_aborted_after_integrity_failure")
        );

        let mut instrumented_first_calls = Vec::new();
        let instrumented_first = execute_sample_with_role_executor(
            &profile,
            &numerical,
            2,
            &scenario,
            &oracle,
            result_root,
            |role, _| {
                instrumented_first_calls.push(role);
                population_role(role, "RZ", 8)
            },
        );
        assert_eq!(instrumented_first_calls, vec![AttemptRole::Instrumented]);
        assert!(
            instrumented_first
                .control
                .as_ref()
                .unwrap()
                .reasons
                .contains("population_aborted_after_integrity_failure")
        );

        let mut product_failure_calls = Vec::new();
        execute_sample_with_role_executor(
            &profile,
            &numerical,
            1,
            &scenario,
            &oracle,
            result_root,
            |role, _| {
                product_failure_calls.push(role);
                let mut evidence = population_role(role, "RZ", 8);
                if role == AttemptRole::InstrumentationControl {
                    evidence
                        .reasons
                        .insert("main_loop_gap_gate_exceeded".to_owned());
                }
                evidence
            },
        );
        assert_eq!(
            product_failure_calls,
            vec![
                AttemptRole::InstrumentationControl,
                AttemptRole::Instrumented
            ]
        );
    }

    #[test]
    fn import_source_inventory_accepts_the_exact_workload_binding() {
        let source_root = tempfile::tempdir().unwrap();
        let source = source_root.path().join("source.tif");
        fs::write(&source, b"pixels").unwrap();
        let mut scenario = ip_action_scenario();
        set_ip_source(&mut scenario, &source);
        let binding = import_source_binding(&scenario.instrumented_script);

        let facts = capture_bound_import_source(&scenario.instrumented_script, &binding).unwrap();
        assert!(import_source_inventory_matches_binding(&facts, &binding));
    }

    #[test]
    fn import_source_inventory_rejects_each_mismatched_workload_fact() {
        let source_root = tempfile::tempdir().unwrap();
        let source = source_root.path().join("source.tif");
        fs::write(&source, b"pixels").unwrap();
        let mut scenario = ip_action_scenario();
        set_ip_source(&mut scenario, &source);
        let binding = import_source_binding(&scenario.instrumented_script);

        let mut wrong_digest = binding.clone();
        wrong_digest.inventory_sha256 = "00".repeat(32);
        assert!(capture_bound_import_source(&scenario.instrumented_script, &wrong_digest).is_err());

        let mut wrong_files = binding.clone();
        wrong_files.regular_files += 1;
        assert!(capture_bound_import_source(&scenario.instrumented_script, &wrong_files).is_err());

        let mut wrong_bytes = binding;
        wrong_bytes.source_bytes += 1;
        assert!(capture_bound_import_source(&scenario.instrumented_script, &wrong_bytes).is_err());
    }

    #[test]
    fn execute_role_rejects_a_source_changed_before_preflight_without_spawning() {
        let source_root = tempfile::tempdir().unwrap();
        let source = source_root.path().join("source.tif");
        fs::write(&source, b"pixels").unwrap();
        let mut scenario = ip_action_scenario();
        set_ip_source(&mut scenario, &source);
        let binding = import_source_binding(&scenario.instrumented_script);
        fs::write(&source, b"mutated-pixels").unwrap();

        let result_root = tempfile::tempdir().unwrap();
        let evidence = execute_role_with_prelaunch_check(
            &profile(),
            Some(&binding),
            1,
            &scenario,
            &ip_oracle_scenario(),
            &scenario.instrumented_script,
            AttemptRole::Instrumented,
            &result_root.path().join("must-not-run"),
            result_root.path(),
            BTreeSet::new,
        );

        assert_eq!(
            evidence.reasons,
            BTreeSet::from(["import_source_inventory_preflight_failed".to_owned()])
        );
        assert_eq!(
            evidence.process.spawn_error.as_deref(),
            Some("import source inventory preflight was rejected")
        );
        assert!(!evidence.process.launch_attempted);
        assert!(evidence.source_inventory_before.is_none());
        assert!(evidence.source_inventory_after.is_none());
        assert!(!evidence.root.join("stdout.log").exists());
    }

    #[test]
    fn execute_role_setup_failure_is_not_counted_as_a_process_launch() {
        let scenario = ip_action_scenario();
        let result_root = tempfile::tempdir().unwrap();
        fs::create_dir_all(result_root.path().join("sample-01/IP/instrumented")).unwrap();

        let evidence = execute_role_with_prelaunch_check(
            &profile(),
            None,
            1,
            &scenario,
            &ip_oracle_scenario(),
            &scenario.instrumented_script,
            AttemptRole::Instrumented,
            &result_root.path().join("must-not-run"),
            result_root.path(),
            BTreeSet::new,
        );

        assert_eq!(
            evidence.reasons,
            BTreeSet::from(["attempt_setup_failed".to_owned()])
        );
        assert!(!evidence.process.launch_attempted);
        assert!(!evidence.root.join("stdout.log").exists());
    }

    #[test]
    fn prelaunch_immutability_rejection_creates_no_attempt_or_process() {
        let live_repository = RepositoryIdentity {
            root: Some(PathBuf::from("/live")),
            commit: Some("a".repeat(40)),
            dirty_worktree: Some(false),
        };
        let source_repository = RepositoryIdentity {
            root: Some(PathBuf::from("/immutable")),
            commit: Some("a".repeat(40)),
            dirty_worktree: Some(false),
        };
        let binding = RunImmutabilityBinding {
            live_repository: live_repository.clone(),
            source_repository: source_repository.clone(),
            source_root: PathBuf::from("/immutable"),
            app_binary_sha256: "b".repeat(64),
        };
        assert!(
            prelaunch_immutability_reason_codes_from(
                &binding,
                &live_repository,
                &source_repository,
                Some(&"b".repeat(64)),
            )
            .is_empty()
        );

        let mut changed_live = live_repository;
        changed_live.dirty_worktree = Some(true);
        let mut changed_source = source_repository;
        changed_source.commit = Some("c".repeat(40));
        let reasons = prelaunch_immutability_reason_codes_from(
            &binding,
            &changed_live,
            &changed_source,
            Some(&"d".repeat(64)),
        );
        assert_eq!(
            reasons,
            BTreeSet::from([
                "app_binary_changed_before_role_launch".to_owned(),
                "immutable_source_changed_before_role_launch".to_owned(),
                "repository_changed_or_dirty_before_role_launch".to_owned(),
            ])
        );

        let scenario = ip_action_scenario();
        let result_root = tempfile::tempdir().unwrap();
        let evidence = execute_role_with_prelaunch_check(
            &profile(),
            None,
            1,
            &scenario,
            &ip_oracle_scenario(),
            &scenario.instrumented_script,
            AttemptRole::Instrumented,
            &result_root.path().join("must-not-run"),
            result_root.path(),
            || reasons,
        );
        assert!(!evidence.process.launch_attempted);
        assert!(!evidence.root.exists());
        assert_eq!(
            evidence.process.spawn_error.as_deref(),
            Some("prelaunch immutability binding rejected")
        );
    }

    #[test]
    fn output_setup_failure_is_not_counted_as_a_process_launch() {
        let role_root = tempfile::tempdir().unwrap();
        fs::write(role_root.path().join("stdout.log"), b"already owned").unwrap();

        let process = run_app_process(
            &role_root.path().join("must-not-run"),
            &role_root.path().join("unused-dataset"),
            role_root.path(),
            Duration::ZERO,
        );

        assert!(!process.launch_attempted);
        assert_eq!(
            process.spawn_error.as_deref(),
            Some("failed to create attempt stdout/stderr")
        );
    }

    #[test]
    fn execute_role_records_path_free_facts_when_the_source_changes_after_launch() {
        let source_root = tempfile::tempdir().unwrap();
        let source = source_root.path().join("source.tif");
        fs::write(&source, b"pixels").unwrap();
        let app = source_root.path().join("mutate-source.sh");
        fs::write(
            &app,
            b"#!/bin/sh\nsource_dir=${0%/*}\nprintf changed-source > \"$source_dir/source.tif\"\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&app).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&app, permissions).unwrap();

        let mut scenario = ip_action_scenario();
        set_ip_source(&mut scenario, &source);
        let binding = import_source_binding(&scenario.instrumented_script);
        let result_root = tempfile::tempdir().unwrap();
        let evidence = execute_role_with_prelaunch_check(
            &profile(),
            Some(&binding),
            1,
            &scenario,
            &ip_oracle_scenario(),
            &scenario.instrumented_script,
            AttemptRole::Instrumented,
            &app,
            result_root.path(),
            BTreeSet::new,
        );

        assert!(evidence.reasons.contains("import_source_inventory_changed"));
        assert!(
            !evidence
                .reasons
                .contains("import_source_inventory_postflight_failed")
        );
        assert!(
            evidence
                .process
                .status
                .is_some_and(|status| status.success())
        );
        assert_ne!(
            evidence.source_inventory_before,
            evidence.source_inventory_after
        );
        assert!(evidence.source_inventory_before.is_some());
        assert!(evidence.source_inventory_after.is_some());
        let after_json = import_source_inventory_json(
            evidence
                .source_inventory_after
                .as_ref()
                .expect("postflight retained safe inventory facts"),
        );
        assert_eq!(after_json.as_object().unwrap().len(), 3);
        assert!(
            !serde_json::to_string(&after_json)
                .unwrap()
                .contains(source.to_str().unwrap())
        );
    }

    #[test]
    fn ip_preprocessing_checkpoint_precedes_the_required_open_ready_gate() {
        let scenario = ip_action_scenario();
        validate_ip_action_contract(&scenario).unwrap();

        let mut premature = scenario.clone();
        premature.instrumented_script.commands.swap(4, 5);
        assert!(validate_ip_action_contract(&premature).is_err());

        let mut ambiguous = scenario;
        ambiguous.instrumented_script.commands.insert(
            5,
            json!({
                "command": "observe_gate",
                "gate_id": "IP.acceptance.999.import_idle",
                "condition": "import_idle",
                "timeout_ms": 1_200_000,
            }),
        );
        assert!(validate_ip_action_contract(&ambiguous).is_err());

        let mut legacy = ip_action_scenario();
        legacy.instrumented_script.commands[6] = json!({
            "command": "wait_for_imported_open_ready",
            "path": "${ATTEMPT_ROOT}/output/source.m4d",
            "timeout_ms": 1_200_000,
        });
        assert!(validate_ip_action_contract(&legacy).is_err());
    }

    #[test]
    fn ip_attempt_paths_cross_bind_before_any_launch() {
        let scenario = ip_action_scenario();
        validate_ip_attempt_path_binding(&scenario).unwrap();

        let mut wrong_target = scenario.clone();
        let observation = wrong_target
            .instrumented_script
            .commands
            .iter_mut()
            .find_map(|command| {
                command
                    .get_mut("observations")
                    .and_then(Value::as_array_mut)
                    .and_then(|observations| {
                        observations.iter_mut().find(|observation| {
                            observation.pointer("/target/kind").and_then(Value::as_str)
                                == Some("imported_open_ready")
                        })
                    })
            })
            .unwrap();
        observation["target"]["path"] = json!("${ATTEMPT_ROOT}/output/other.m4d");
        assert!(validate_ip_attempt_path_binding(&wrong_target).is_err());

        let mut wrong_parent = scenario.clone();
        let setup = wrong_parent
            .instrumented_script
            .commands
            .iter_mut()
            .find(|command| {
                command.get("command").and_then(Value::as_str) == Some("begin_tiff_import_setup")
            })
            .unwrap();
        setup["output_parent"] = json!("${ATTEMPT_ROOT}/other");
        assert!(validate_ip_attempt_path_binding(&wrong_parent).is_err());

        let mut wrong_cleanup = scenario;
        wrong_cleanup.cleanup.imported_package_relative_path =
            Some(PathBuf::from("output/other.m4d"));
        assert!(validate_ip_attempt_path_binding(&wrong_cleanup).is_err());
    }

    #[test]
    fn fc_cold_and_runtime_batches_bracket_verifier_quiescence() {
        let commands = vec![
            json!({ "command": "sleep_frames", "frames": 1 }),
            json!({ "command": "observe_gate_batch" }),
            json!({
                "command": "wait_for",
                "condition": "source_verification_inactive",
                "timeout_ms": SOURCE_VERIFICATION_QUIESCENCE_TIMEOUT_MS,
            }),
            json!({ "command": "sleep_frames", "frames": 1 }),
            json!({ "command": "observe_gate_batch" }),
        ];
        let batch = |command_index, phase_id| ExpectedProductGateBatch {
            command_index,
            batch_id: "FC.batch",
            phase_id,
            origin: ProductGateOrigin::AutomationStarted,
            observations: Vec::new(),
        };
        let valid = vec![
            batch(1, "blocking_target_settled.checkpoint.000"),
            batch(4, "blocking_target_settled.checkpoint.001"),
        ];
        validate_fc_source_verification_isolation_order(&commands, &valid).unwrap();

        let late_cold = vec![
            batch(2, "blocking_target_settled.checkpoint.000"),
            batch(4, "blocking_target_settled.checkpoint.001"),
        ];
        assert!(validate_fc_source_verification_isolation_order(&commands, &late_cold).is_err());

        let early_idle = vec![
            batch(1, "blocking_target_settled.checkpoint.000"),
            batch(2, "blocking_target_settled.checkpoint.001"),
        ];
        assert!(validate_fc_source_verification_isolation_order(&commands, &early_idle).is_err());
    }

    #[test]
    fn role_watchdog_accounts_startup_admission_separately_from_declared_waits() {
        let script = template(
            false,
            vec![
                json!({
                    "command": "wait_for",
                    "condition": "window_ready",
                    "timeout_ms": 5_000,
                }),
                json!({
                    "command": "camera_zoom_sequence",
                    "duration_ms": 2_000,
                    "samples": 120,
                    "scroll_y_points": -120.0,
                }),
                json!({
                    "command": "await_active_view_gpu_timing",
                    "target": "three_d",
                    "pass_kind": "volume",
                    "timeout_ms": GPU_TIMING_AWAIT_TIMEOUT_MS,
                }),
            ],
        );
        let schedule = role_schedule_bound("RZ", &script, &profile(), &ip_oracle_scenario())
            .expect("the bounded schedule must be derivable");
        assert_eq!(schedule.prerequisite_wait_bound_ns, 10_000_000_000);
        assert_eq!(schedule.action_duration_bound_ns, 2_000_000_000);
        assert_eq!(schedule.static_wait_bound_ns, 10_000_000_000);
        assert_eq!(
            schedule.derived_process_timeout_ns,
            10_000_000_000
                + 2_000_000_000
                + PROCESS_STARTUP_ADMISSION_GRACE_NS
                + PROCESS_CLOSEOUT_GRACE_NS
        );
    }

    #[test]
    fn non_vv_scenarios_require_bounded_verifier_quiescence_before_measurement() {
        let pair = || {
            vec![
                json!({ "command": "cancel_active_source_verification" }),
                json!({
                    "command": "wait_for",
                    "condition": "source_verification_inactive",
                    "timeout_ms": SOURCE_VERIFICATION_QUIESCENCE_TIMEOUT_MS,
                }),
            ]
        };
        for id in ["RZ", "ZB", "RO", "ST", "NO", "VM", "IP"] {
            let mut commands = pair();
            commands.push(json!({ "command": "sample_diagnostics", "label": "start" }));
            commands.push(json!({ "command": "observe_gate_batch" }));
            validate_source_verification_isolation_contract(id, &commands).unwrap();

            let mut serialized = commands.clone();
            serialized[1]["condition"] = json!("source_verification_verified");
            assert!(validate_source_verification_isolation_contract(id, &serialized).is_err());

            let mut nonadjacent = commands.clone();
            nonadjacent.insert(1, json!({ "command": "sleep_frames", "frames": 1 }));
            assert!(validate_source_verification_isolation_contract(id, &nonadjacent).is_err());

            let mut after_measurement = commands;
            after_measurement.rotate_left(2);
            assert!(
                validate_source_verification_isolation_contract(id, &after_measurement).is_err()
            );
        }

        let mut pt = pair();
        pt.push(json!({ "command": "switch_dataset" }));
        pt.extend(pair());
        pt.push(json!({ "command": "sample_diagnostics", "label": "start" }));
        pt.push(json!({ "command": "observe_gate_batch" }));
        validate_source_verification_isolation_contract("PT", &pt).unwrap();

        let mut fc = vec![json!({ "command": "observe_gate_batch" })];
        fc.extend(pair());
        fc.push(json!({ "command": "observe_gate_batch" }));
        validate_source_verification_isolation_contract("FC", &fc).unwrap();

        let mut vv = pair();
        vv.push(json!({ "command": "observe_gate_batch" }));
        validate_source_verification_isolation_contract("VV", &vv).unwrap();
        let unquiesced_vv = vec![json!({ "command": "observe_gate_batch" })];
        assert!(validate_source_verification_isolation_contract("VV", &unquiesced_vv).is_err());
        let mut serialized_vv = vv;
        serialized_vv.insert(
            2,
            json!({
                "command": "wait_for",
                "condition": "source_verification_verified",
                "timeout_ms": 30_000,
            }),
        );
        assert!(validate_source_verification_isolation_contract("VV", &serialized_vv).is_err());
    }

    fn profile() -> ViewerQualificationProfile {
        serde_json::from_value(json!({
            "schema": super::super::PROFILE_SCHEMA,
            "hardware_class": "test-hardware",
            "ep01_selection_authority_sha256": super::super::ep01_selection::authority_fingerprint_sha256(),
            "build": {
                "repository_revision": "0".repeat(40),
                "profile": "release",
                "compiler": "rustc 1.90.0 (1159e78c4 2025-09-14)\nbinary: rustc\ncommit-hash: 1159e78c4747b02ef996e55082b704c09b970588\ncommit-date: 2025-09-14\nhost: x86_64-unknown-linux-gnu\nrelease: 1.90.0\nLLVM version: 20.1.8",
                "target_mode": "fresh-private-target",
            },
            "workload": {
                "representative_package": {
                    "root": "/private/package.m4d",
                    "root_manifest_sha256": "11".repeat(32),
                },
                "workload_bundle_sha256": "22".repeat(32),
                "interaction_script_bundle_sha256": "33".repeat(32),
                "independent_oracle_sha256": "44".repeat(32),
                "scenarios": REQUIRED_SCENARIOS,
            },
            "host": {
                "os": "linux",
                "arch": "x86_64",
                "cpu_model": "test",
                "logical_cpu_count": 8,
                "mem_total_kib": 16_000_000,
                "storage": {
                    "filesystem_type": "ext4",
                    "filesystem_source": "/dev/test",
                    "filesystem_uuid": "test",
                    "device_major_minor": "1:2",
                },
            },
            "graphics": {
                "backend": "vulkan",
                "adapter_name": "test",
                "vendor_id": 1,
                "device_id": 2,
                "device_type": "DiscreteGpu",
                "api_version": "1",
                "driver_version": "1",
                "driver_name": "test",
                "driver_info": "test",
                "dedicated_vram_bytes": 8_589_934_592_u64,
                "wgpu_version": "29.0.3",
                "naga_version": "29.0.3",
                "requested_features": ["TIMESTAMP_QUERY"],
                "device_memory_hint": "MemoryUsage",
            },
            "display": {
                "session_type": "x11",
                "compositor": "test",
                "output_name": "test",
                "current_mode": { "width": 1920, "height": 1080 },
                "physical_width_mm": 500,
                "physical_height_mm": 300,
                "refresh_millihz": 60_000,
                "compositor_scale_milli": 1000,
                "presentation_mode": "fifo",
            },
            "protocol": {
                "application_cold": true,
                "empty_product_residency": true,
                "os_cache_condition": "warm",
                "competing_activity": "none",
                "power_state": "balanced",
                "automatic_retries": 0,
                "development_samples": 3,
            },
            "extents": {
                "blocking_qualification": { "width": 1280, "height": 720 },
                "required_exercise": { "width": 1920, "height": 1080 },
            },
            "resources": {
                "max_cpu_total_bytes": 4_294_967_296_u64,
                "max_cpu_decoded_residency_bytes": 2_147_483_648_u64,
                "max_cpu_upload_staging_bytes": 536_870_912_u64,
                "gpu_budget_bytes": 4_294_967_296_u64,
                "max_gpu_resident_bytes": 3_221_225_472_u64,
                "max_gpu_in_flight_bytes": 429_496_729_u64,
                "max_open_objects": 64,
                "max_queued_requests": 1024,
            },
            "absolute_gates": {
                "resident_input_to_current_presentation_p95_ns": 16_666_667,
                "maximum_current_presentation_gap_ns": 33_333_334,
                "maximum_main_loop_heartbeat_gap_ns": 33_333_334,
                "maximum_ui_thread_interaction_task_ns": 2_000_000,
                "maximum_plane_gpu_ns": 16_666_667,
                "maximum_mip_gpu_ns": 16_666_667,
                "maximum_dvr_gpu_ns": 16_666_667,
                "maximum_iso_gpu_ns": 16_666_667,
                "cold_first_useful_ns": 100_000_000,
                "cold_complete_coarse_ns": 250_000_000,
                "cold_target_settlement_ns": 2_000_000_000_u64,
                "nonresident_target_settlement_ns": 1_000_000_000_u64,
                "source_verification_completion_ns": 30_000_000_000_u64,
                "maximum_instrumentation_overhead_basis_points": 200,
            },
        }))
        .unwrap()
    }

    #[test]
    fn hard_safety_limits_are_exact_profile_derived_caps_and_echo_without_legacy_aliases() {
        let profile = profile();
        let exact = expected_hard_safety_limits(&profile).unwrap();
        assert_eq!(
            exact.max_cpu_total_bytes,
            Some(profile.resources.max_cpu_total_bytes)
        );
        assert_eq!(
            exact.max_cpu_decoded_residency_bytes,
            Some(profile.resources.max_cpu_total_bytes)
        );
        assert_eq!(
            exact.max_cpu_upload_staging_bytes,
            Some(profile.resources.max_cpu_total_bytes)
        );
        assert_eq!(
            exact.max_runtime_queued_requests,
            profile.resources.max_queued_requests.checked_mul(2)
        );
        validate_hard_safety_limits(&exact, &profile).unwrap();

        let mut qualification_category_caps = exact;
        qualification_category_caps.max_cpu_decoded_residency_bytes =
            Some(profile.resources.max_cpu_decoded_residency_bytes);
        qualification_category_caps.max_cpu_upload_staging_bytes =
            Some(profile.resources.max_cpu_upload_staging_bytes);
        assert!(validate_hard_safety_limits(&qualification_category_caps, &profile).is_err());

        let mut forbidden_optional_cap = exact;
        forbidden_optional_cap.max_runtime_pending_completions = Some(1);
        assert!(validate_hard_safety_limits(&forbidden_optional_cap, &profile).is_err());

        let mut wrong_queue_cap = exact;
        wrong_queue_cap.max_runtime_queued_requests =
            profile.resources.max_queued_requests.checked_mul(3);
        assert!(validate_hard_safety_limits(&wrong_queue_cap, &profile).is_err());

        let mut template = template(true, Vec::new());
        template.hard_safety_limits = exact;
        let exact_json = serde_json::to_value(exact).unwrap();
        let mut report = json!({ "hard_safety_limits": exact_json });
        assert!(automation_report_hard_safety_limits_match(
            &report, &template
        ));
        report["hard_safety_limits"]["max_cpu_total_bytes"] = json!(1);
        assert!(!automation_report_hard_safety_limits_match(
            &report, &template
        ));
        report["hard_safety_limits"] = serde_json::to_value(exact).unwrap();
        report["hard_safety_limits"]["unknown"] = json!(null);
        assert!(!automation_report_hard_safety_limits_match(
            &report, &template
        ));
        report.as_object_mut().unwrap().remove("hard_safety_limits");
        assert!(!automation_report_hard_safety_limits_match(
            &report, &template
        ));
        report["limits"] = serde_json::to_value(exact).unwrap();
        assert!(!automation_report_hard_safety_limits_match(
            &report, &template
        ));

        let mut legacy_template = serde_json::to_value(&template).unwrap();
        let limits = legacy_template
            .as_object_mut()
            .unwrap()
            .remove("hard_safety_limits")
            .unwrap();
        legacy_template["limits"] = limits;
        assert!(serde_json::from_value::<AutomationScriptTemplate>(legacy_template).is_err());
    }

    #[test]
    fn automation_report_build_provenance_is_exactly_profile_bound() {
        let profile = profile();
        let mut report = json!({
            "build_provenance": {
                "repository_revision": profile.build.repository_revision,
                "profile": profile.build.profile,
                "compiler": profile.build.compiler,
                "target_mode": profile.build.target_mode,
                "opt_level": "3",
                "debug": "false",
                "custom_rustflags": "false",
                "rustc_wrapper": "false",
            },
        });
        let mut reasons = BTreeSet::new();
        validate_app_build_provenance(&report, &profile, &mut reasons);
        assert!(reasons.is_empty());

        report["build_provenance"]["profile"] = json!("dev");
        validate_app_build_provenance(&report, &profile, &mut reasons);
        assert!(reasons.contains("automation_report_build_provenance_mismatch"));
    }

    #[test]
    fn runner_arguments_require_every_external_binding_and_reject_removed_timeout() {
        let arguments = [
            "--qualification-profile",
            "/private/profile.json",
            "--workload-bundle",
            "/private/workload.json",
            "--interaction-script-bundle",
            "/private/scripts.json",
            "--independent-oracle",
            "/private/oracle.json",
            "--result-directory",
            "/private/result",
            "--cache-condition",
            "warm",
            "--competing-activity",
            "none",
            "--power-state",
            "balanced",
            "--compositor-scale-milli",
            "1000",
        ]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect();
        let parsed = parse_args(arguments).unwrap();
        assert_eq!(parsed.result_directory, Path::new("/private/result"));

        let error = parse_args(Vec::new()).unwrap_err().to_string();
        assert!(error.contains(" is required"));
        assert!(
            parse_args(vec!["--timeout-seconds".to_owned(), "90".to_owned()])
                .unwrap_err()
                .to_string()
                .contains("unknown viewer performance runner argument")
        );
    }

    #[test]
    fn automation_template_rejects_unaccounted_waits_and_fatal_product_assertions() {
        let hidden_wait = template(
            true,
            vec![
                json!({
                    "command": "wait_for_import_progress",
                    "stage": "base-production",
                    "minimum_completed_work_units": 1,
                    "timeout_ms": 900_000,
                }),
                json!({ "command": "quit" }),
            ],
        );
        assert!(validate_automation_template("RZ", &hidden_wait, true, &[]).is_err());

        let accounted_wait = template(
            true,
            vec![
                json!({
                    "command": "wait_for",
                    "condition": "runtime_idle",
                    "timeout_ms": 30_000,
                }),
                json!({ "command": "quit" }),
            ],
        );
        validate_automation_template("RZ", &accounted_wait, true, &[]).unwrap();

        let fatal_product_assertion = template(
            true,
            vec![
                json!({ "command": "assert", "condition": "no_render_error" }),
                json!({ "command": "quit" }),
            ],
        );
        let error = validate_automation_template("RZ", &fatal_product_assertion, true, &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("fatal product assertion"));
    }

    #[test]
    fn bundle_schemas_reject_unknown_fields() {
        let workload = json!({
            "schema": WORKLOAD_SCHEMA,
            "representative_package_root_manifest_sha256": "11".repeat(32),
            "supporting_temporal_package_root_manifest_sha256": "22".repeat(32),
            "import_source": {
                "inventory_sha256": "33".repeat(32),
                "reviewed_source_fingerprint_sha256": "44".repeat(32),
                "regular_files": 1,
                "source_bytes": 1,
            },
            "ep01_trace_geometry": ep01_trace_geometry(),
            "scenarios": [],
            "legacy": true,
        });
        assert!(serde_json::from_value::<WorkloadBundle>(workload).is_err());

        let mut missing_geometry = json!({
            "schema": WORKLOAD_SCHEMA,
            "representative_package_root_manifest_sha256": "11".repeat(32),
            "supporting_temporal_package_root_manifest_sha256": "22".repeat(32),
            "import_source": {
                "inventory_sha256": "33".repeat(32),
                "reviewed_source_fingerprint_sha256": "44".repeat(32),
                "regular_files": 1,
                "source_bytes": 1,
            },
            "ep01_trace_geometry": ep01_trace_geometry(),
            "scenarios": [],
        });
        missing_geometry
            .as_object_mut()
            .unwrap()
            .remove("ep01_trace_geometry");
        assert!(serde_json::from_value::<WorkloadBundle>(missing_geometry).is_err());
        assert!(
            validate_workload_schema("mirante4d-viewer-performance-workload-bundle-3").is_err()
        );

        let mut geometry = serde_json::to_value(ep01_trace_geometry()).unwrap();
        geometry["unknown"] = json!(true);
        assert!(serde_json::from_value::<Ep01TraceGeometry>(geometry).is_err());

        let mut oracle_phase = serde_json::to_value(resident_oracle_phase()).unwrap();
        oracle_phase["unknown"] = json!(1);
        assert!(serde_json::from_value::<OraclePhase>(oracle_phase).is_err());

        let mut import_gate = serde_json::to_value(test_import_gate()).unwrap();
        import_gate["limits"]["standalone_import_open_file_limit"] = json!(64);
        assert!(serde_json::from_value::<ImportGate>(import_gate).is_err());
    }

    #[test]
    fn ep01_trace_geometry_digest_is_frozen_and_json_format_independent() {
        let geometry = ep01_trace_geometry();
        assert_eq!(
            ep01_trace_geometry_sha256(&geometry),
            "2ca301dc3334df8ec6b5082efa9bca08b7083e657d2c646d5c68808f768099ad"
        );

        let compact = serde_json::to_string(&geometry).unwrap();
        let pretty = serde_json::to_string_pretty(&geometry).unwrap();
        let reordered: Ep01TraceGeometry = serde_json::from_str(
            r#"{
                "numeric_boxes": [
                    {
                        "end_zyx_exclusive": [1, 5, 8],
                        "start_zyx": [0, 1, 2],
                        "scale_level": 0,
                        "time_index": 0,
                        "logical_layer_ordinal": 0
                    },
                    {
                        "end_zyx_exclusive": [9, 10, 11],
                        "start_zyx": [4, 5, 6],
                        "scale_level": 2,
                        "time_index": 3,
                        "logical_layer_ordinal": 1
                    }
                ],
                "package_role": "representative_package",
                "whole_layer": [
                    {"scale_level": 0, "time_index": 0, "logical_layer_ordinal": 0},
                    {"scale_level": 2, "time_index": 3, "logical_layer_ordinal": 1}
                ],
                "derivation_contract": "mirante4d-ep01-brickkey-trace-projection-1"
            }"#,
        )
        .unwrap();
        assert_ne!(compact, pretty);
        let compact: Ep01TraceGeometry = serde_json::from_str(&compact).unwrap();
        let pretty: Ep01TraceGeometry = serde_json::from_str(&pretty).unwrap();
        assert_eq!(reordered, geometry);
        assert_eq!(
            ep01_trace_geometry_sha256(&compact),
            ep01_trace_geometry_sha256(&pretty)
        );
        assert_eq!(
            ep01_trace_geometry_sha256(&reordered),
            ep01_trace_geometry_sha256(&pretty)
        );
    }

    #[test]
    fn ep01_trace_geometry_digest_changes_for_every_semantic_field() {
        let geometry = ep01_trace_geometry();
        let expected = ep01_trace_geometry_sha256(&geometry);
        let mut mutations = Vec::new();

        let mut mutation = geometry.clone();
        mutation.derivation_contract.push_str("-changed");
        mutations.push(mutation);
        let mut mutation = geometry.clone();
        mutation.package_role.push_str("-changed");
        mutations.push(mutation);
        for mutate in [
            |trace: &mut Ep01WholeLayerTrace| trace.logical_layer_ordinal += 1,
            |trace: &mut Ep01WholeLayerTrace| trace.time_index += 1,
            |trace: &mut Ep01WholeLayerTrace| trace.scale_level += 1,
        ] {
            let mut mutation = geometry.clone();
            mutate(&mut mutation.whole_layer[0]);
            mutations.push(mutation);
        }
        for mutate in [
            |trace: &mut Ep01NumericBoxTrace| trace.logical_layer_ordinal += 1,
            |trace: &mut Ep01NumericBoxTrace| trace.time_index += 1,
            |trace: &mut Ep01NumericBoxTrace| trace.scale_level += 1,
        ] {
            let mut mutation = geometry.clone();
            mutate(&mut mutation.numeric_boxes[0]);
            mutations.push(mutation);
        }
        for axis in 0..3 {
            let mut mutation = geometry.clone();
            mutation.numeric_boxes[0].start_zyx[axis] += 1;
            mutations.push(mutation);

            let mut mutation = geometry.clone();
            mutation.numeric_boxes[0].end_zyx_exclusive[axis] += 1;
            mutations.push(mutation);
        }
        let mut mutation = geometry.clone();
        mutation.whole_layer.pop();
        mutations.push(mutation);
        let mut mutation = geometry;
        mutation.numeric_boxes.pop();
        mutations.push(mutation);

        for mutation in mutations {
            assert_ne!(ep01_trace_geometry_sha256(&mutation), expected);
        }
    }

    #[test]
    fn ep01_trace_geometry_validation_is_strict_and_bounded() {
        let geometry = ep01_trace_geometry();
        validate_ep01_trace_geometry(&geometry).unwrap();

        let mut invalid = geometry.clone();
        invalid.derivation_contract = "predecessor".to_owned();
        assert!(validate_ep01_trace_geometry(&invalid).is_err());
        let mut invalid = geometry.clone();
        invalid.package_role = "supporting_temporal_package".to_owned();
        assert!(validate_ep01_trace_geometry(&invalid).is_err());

        let mut invalid = geometry.clone();
        invalid.whole_layer.clear();
        assert!(validate_ep01_trace_geometry(&invalid).is_err());
        let mut invalid = geometry.clone();
        invalid.numeric_boxes.clear();
        assert!(validate_ep01_trace_geometry(&invalid).is_err());

        let mut invalid = geometry.clone();
        invalid.whole_layer = (0..=EP01_TRACE_ENTRIES_MAX)
            .map(|time_index| Ep01WholeLayerTrace {
                logical_layer_ordinal: 0,
                time_index: u64::try_from(time_index).unwrap(),
                scale_level: 0,
            })
            .collect();
        assert!(validate_ep01_trace_geometry(&invalid).is_err());
        let mut invalid = geometry.clone();
        invalid.numeric_boxes = (0..=EP01_TRACE_ENTRIES_MAX)
            .map(|time_index| Ep01NumericBoxTrace {
                logical_layer_ordinal: 0,
                time_index: u64::try_from(time_index).unwrap(),
                scale_level: 0,
                start_zyx: [0; 3],
                end_zyx_exclusive: [1; 3],
            })
            .collect();
        assert!(validate_ep01_trace_geometry(&invalid).is_err());

        let mut invalid = geometry.clone();
        invalid.whole_layer.swap(0, 1);
        assert!(validate_ep01_trace_geometry(&invalid).is_err());
        let mut invalid = geometry.clone();
        invalid.whole_layer[1] = invalid.whole_layer[0].clone();
        assert!(validate_ep01_trace_geometry(&invalid).is_err());
        let mut invalid = geometry.clone();
        invalid.numeric_boxes.swap(0, 1);
        assert!(validate_ep01_trace_geometry(&invalid).is_err());
        let mut invalid = geometry.clone();
        invalid.numeric_boxes[1] = invalid.numeric_boxes[0].clone();
        assert!(validate_ep01_trace_geometry(&invalid).is_err());

        for mutate in [
            |trace: &mut Ep01WholeLayerTrace| {
                trace.logical_layer_ordinal = EP01_TRACE_LAYER_ORDINAL_MAX + 1;
            },
            |trace: &mut Ep01WholeLayerTrace| {
                trace.time_index = EP01_TRACE_TIME_INDEX_MAX + 1;
            },
            |trace: &mut Ep01WholeLayerTrace| {
                trace.scale_level = EP01_TRACE_SCALE_LEVEL_MAX + 1;
            },
        ] {
            let mut invalid = geometry.clone();
            mutate(&mut invalid.whole_layer[0]);
            assert!(validate_ep01_trace_geometry(&invalid).is_err());
        }
        for mutate in [
            |trace: &mut Ep01NumericBoxTrace| {
                trace.logical_layer_ordinal = EP01_TRACE_LAYER_ORDINAL_MAX + 1;
            },
            |trace: &mut Ep01NumericBoxTrace| {
                trace.time_index = EP01_TRACE_TIME_INDEX_MAX + 1;
            },
            |trace: &mut Ep01NumericBoxTrace| {
                trace.scale_level = EP01_TRACE_SCALE_LEVEL_MAX + 1;
            },
        ] {
            let mut invalid = geometry.clone();
            mutate(&mut invalid.numeric_boxes[0]);
            assert!(validate_ep01_trace_geometry(&invalid).is_err());
        }
        let mut invalid = geometry.clone();
        invalid.numeric_boxes[0].start_zyx[0] = EP01_TRACE_SPATIAL_COORDINATE_MAX + 1;
        assert!(validate_ep01_trace_geometry(&invalid).is_err());
        let mut invalid = geometry.clone();
        invalid.numeric_boxes[0].end_zyx_exclusive[0] = EP01_TRACE_SPATIAL_COORDINATE_MAX + 1;
        assert!(validate_ep01_trace_geometry(&invalid).is_err());

        for axis in 0..3 {
            let mut invalid = geometry.clone();
            invalid.numeric_boxes[0].end_zyx_exclusive[axis] =
                invalid.numeric_boxes[0].start_zyx[axis];
            assert!(validate_ep01_trace_geometry(&invalid).is_err());

            let mut invalid = geometry.clone();
            invalid.numeric_boxes[0].start_zyx[axis] =
                invalid.numeric_boxes[0].end_zyx_exclusive[axis] + 1;
            assert!(validate_ep01_trace_geometry(&invalid).is_err());
        }
    }

    #[test]
    fn preflight_oracle_source_commitments_match_the_exact_repository_files() {
        let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let mut oracle = OracleBundle {
            schema: ORACLE_SCHEMA.to_owned(),
            independent_sources: IndependentOracleSources {
                lod_oracle_source_sha256: digest_regular_file(
                    &repository_root.join("crates/mirante4d-render-reference/src/lod_oracle.rs"),
                    "test LOD oracle source",
                )
                .unwrap(),
                numerical_oracle_source_sha256: digest_regular_file(
                    &repository_root
                        .join("crates/mirante4d-render-reference/src/numerical_oracle.rs"),
                    "test numerical oracle source",
                )
                .unwrap(),
            },
            numerical_contract: numerical_contract(),
            conformance_cases: Vec::new(),
            scenarios: Vec::new(),
        };
        validate_oracle_source_commitments(&oracle, &repository_root).unwrap();

        let lod = oracle.independent_sources.lod_oracle_source_sha256.clone();
        oracle.independent_sources.lod_oracle_source_sha256 = "00".repeat(32);
        assert!(validate_oracle_source_commitments(&oracle, &repository_root).is_err());
        oracle.independent_sources.lod_oracle_source_sha256 = lod;
        oracle.independent_sources.numerical_oracle_source_sha256 = "00".repeat(32);
        assert!(validate_oracle_source_commitments(&oracle, &repository_root).is_err());
    }

    #[test]
    fn resident_oracle_cannot_omit_any_mandatory_structural_gate() {
        let mut phase = resident_oracle_phase();
        validate_structural_gate("RZ", "resident_3d_zoom", &phase).unwrap();
        phase.zero_work_counters.pop();
        let error = validate_structural_gate("RZ", "resident_3d_zoom", &phase)
            .unwrap_err()
            .to_string();
        assert!(error.contains("complete mandatory structural zero-work counter set"));
    }

    #[test]
    fn frozen_scenario_phase_matrix_requires_every_claim_gate() {
        let phases = [
            ("RZ", "resident_cross_section_zoom"),
            ("RZ", "resident_3d_zoom"),
            ("ZB", "resident_boundary_crossing"),
            ("ZB", "nonresident_boundary_crossing"),
            ("RO", "resident_compound_plane_rotation"),
            ("ST", "resident_axis_slice_translation"),
            ("NO", "nonresident_rotation_pan"),
            ("FC", "blocking_target_settled"),
            ("FC", "exercise_extent_settled"),
            ("VM", "resident_mip"),
            ("VM", "resident_dvr"),
            ("VM", "resident_iso"),
            ("VM", "exercise_mip"),
            ("PT", "advance_timepoint"),
            ("PT", "return_timepoint"),
            ("VV", "verification_active_resident"),
            ("VV", "verification_complete_nonresident"),
            ("IP", "preprocess_publish"),
        ];
        for (id, name) in phases {
            validate_required_phase_gate_matrix(id, &matrix_phase(id, name))
                .unwrap_or_else(|error| panic!("{id}/{name}: {error:#}"));
        }

        let mut rz = matrix_phase("RZ", "resident_cross_section_zoom");
        rz.require_interaction_metrics = false;
        assert!(
            validate_required_phase_gate_matrix("RZ", &rz)
                .unwrap_err()
                .to_string()
                .contains("interaction-metric")
        );
        let mut rz = matrix_phase("RZ", "resident_cross_section_zoom");
        rz.gpu_gate = None;
        assert!(
            validate_required_phase_gate_matrix("RZ", &rz)
                .unwrap_err()
                .to_string()
                .contains("GPU gate")
        );

        let mut fc = matrix_phase("FC", "blocking_target_settled");
        fc.settlement_gate = None;
        assert!(
            validate_required_phase_gate_matrix("FC", &fc)
                .unwrap_err()
                .to_string()
                .contains("settlement gate")
        );
        let mut fc = matrix_phase("FC", "blocking_target_settled");
        fc.expected_cross_section_layers.clear();
        assert!(
            validate_required_phase_gate_matrix("FC", &fc)
                .unwrap_err()
                .to_string()
                .contains("every visible cross-section layer")
        );

        for (id, name) in [
            ("ZB", "nonresident_boundary_crossing"),
            ("NO", "nonresident_rotation_pan"),
        ] {
            let mut phase = matrix_phase(id, name);
            phase.settlement_gate = None;
            assert!(
                validate_required_phase_gate_matrix(id, &phase)
                    .unwrap_err()
                    .to_string()
                    .contains("settlement gate")
            );
        }

        let mut vv = matrix_phase("VV", "verification_active_resident");
        vv.verification_gate = None;
        assert!(
            validate_required_phase_gate_matrix("VV", &vv)
                .unwrap_err()
                .to_string()
                .contains("verification gate")
        );
        let mut ro = matrix_phase("RO", "resident_compound_plane_rotation");
        ro.gpu_gate = None;
        assert!(
            validate_required_phase_gate_matrix("RO", &ro)
                .unwrap_err()
                .to_string()
                .contains("GPU gate")
        );
        let mut ip = matrix_phase("IP", "preprocess_publish");
        ip.expected_imported_root_manifest_sha256 = None;
        assert!(
            validate_required_phase_gate_matrix("IP", &ip)
                .unwrap_err()
                .to_string()
                .contains("import-identity")
        );
        let mut ip = matrix_phase("IP", "preprocess_publish");
        ip.import_gate = None;
        assert!(
            validate_required_phase_gate_matrix("IP", &ip)
                .unwrap_err()
                .to_string()
                .contains("import-workflow")
        );
        let mut non_import = matrix_phase("RZ", "resident_3d_zoom");
        non_import.import_gate = Some(test_import_gate());
        assert!(
            validate_required_phase_gate_matrix("RZ", &non_import)
                .unwrap_err()
                .to_string()
                .contains("import-workflow")
        );
    }

    fn matrix_phase(id: &str, name: &str) -> OraclePhase {
        let mut phase = resident_oracle_phase();
        phase.name = name.to_owned();
        let four_panel = matches!(
            (id, name),
            ("RZ", "resident_cross_section_zoom")
                | ("RO", _)
                | ("ST", _)
                | ("NO", _)
                | ("FC", _)
                | ("VV", _)
        );
        phase.phase_state.layout = if four_panel {
            ExpectedViewerLayout::FourPanel
        } else {
            ExpectedViewerLayout::Single3d
        };
        phase.phase_state.active_view = if four_panel {
            ViewerPanel::Xy
        } else {
            ViewerPanel::ThreeD
        };
        phase.phase_state.layers[0].mode = match name {
            "resident_dvr" => "dvr",
            "resident_iso" => "iso",
            _ => "mip",
        }
        .to_owned();
        phase.require_interaction_metrics = !matches!(id, "FC" | "IP");
        phase.require_current_complete = id != "IP";
        phase.require_coordinated_layout_complete = id != "IP";
        phase.expected_scale_level = (id != "IP").then_some(0);
        phase.expected_cross_section_layers = if id != "IP" && four_panel {
            [
                CrossSectionPanel::Xy,
                CrossSectionPanel::Xz,
                CrossSectionPanel::Yz,
            ]
            .into_iter()
            .map(|panel| ExpectedCrossSectionLayer {
                panel,
                layer_ordinal: 0,
                scale_level: 0,
            })
            .collect()
        } else {
            Vec::new()
        };
        phase.gpu_gate = if id == "IP" {
            None
        } else {
            fixed_required_gpu_gate(id, &phase).or(Some(
                match phase.phase_state.layers[0].mode.as_str() {
                    "dvr" => GpuGate::Dvr,
                    "iso" => GpuGate::Iso,
                    _ => GpuGate::Mip,
                },
            ))
        };
        phase.settlement_gate = match (id, name) {
            ("FC", "blocking_target_settled") => Some(SettlementGate::ColdTarget),
            ("ZB", "nonresident_boundary_crossing")
            | ("NO", "nonresident_rotation_pan")
            | ("VV", "verification_complete_nonresident") => {
                Some(SettlementGate::NonresidentTarget)
            }
            _ => None,
        };
        phase.verification_gate = match (id, name) {
            ("VV", "verification_active_resident") => Some(test_verification_gate(
                VerificationGateKind::ActiveThroughout,
            )),
            ("VV", "verification_complete_nonresident") => {
                Some(test_verification_gate(VerificationGateKind::Completes))
            }
            _ => None,
        };
        phase.minimum_exact_useful_sample_bytes = (id != "IP").then_some(1);
        phase.expected_imported_root_manifest_sha256 = (id == "IP").then(|| "55".repeat(32));
        phase.import_gate = (id == "IP").then(test_import_gate);
        phase
    }

    fn test_import_gate() -> ImportGate {
        ImportGate {
            required_worker_stage_names: vec!["base-production".to_owned()],
            required_projected_stage_names: vec!["base-production".to_owned()],
            required_receipt_stage_names: vec!["base-production".to_owned()],
            required_progress: vec![ImportProgressExpectation {
                stage: "base-production".to_owned(),
                minimum_completed_work_units: 1,
            }],
            expected: ImportExpectedCounts {
                successful_runs: 1,
                published_events: 1,
                failed_runs: 0,
                cancelled_runs: 0,
                resumed_work_units: 0,
                checkpoint_pending_work_units: 0,
                produced_work_units: 1,
                checkpoint_durable_work_units: 1,
                scientific_brick_reads: 1,
                staged_structure_object_reads: 1,
                staged_exact_object_reads: 1,
                scientific_object_reads: 1,
                scientific_payload_object_reads: 1,
                object_reads: 3,
                tiff_open_count: 1,
                native_chunk_decode_count: 1,
                peak_checkpoint_regular_files: 6,
                minimum_progress_updates: 1,
            },
            limits: ImportLimits {
                maximum_peak_working_bytes: 256 * 1024 * 1024,
                maximum_peak_process_rss_bytes: 512 * 1024 * 1024,
                maximum_product_peak_open_file_descriptors: 96,
                maximum_open_file_descriptor_structural_bound: 35,
                maximum_preflight_temporary_bytes_bound: 256 * 1024 * 1024,
                maximum_peak_temporary_bytes: 256 * 1024 * 1024,
                maximum_sync_calls: 5_000,
                maximum_app_primary_wall_time_ns: 1_200_000_000_000,
                maximum_app_primary_cpu_time_ns: 1_200_000_000_000,
                maximum_publication_to_open_ready_wall_time_ns: 60_000_000_000,
                maximum_publication_to_open_ready_cpu_time_ns: 60_000_000_000,
                maximum_receipt_primary_wall_time_ns: 1_200_000_000_000,
                maximum_receipt_primary_cpu_time_ns: 1_200_000_000_000,
                maximum_source_read_amplification_numerator: 5,
                maximum_source_read_amplification_denominator: 2,
            },
            publication_currentness: ImportPublicationCurrentnessExpectation {
                contract_id: "test-publication-currentness".to_owned(),
                expected_snapshot_object_reads: 1,
                first_inventory_object_reads: 1,
                observed_snapshot_object_reads: 1,
                second_inventory_object_reads: 1,
                observed_total_object_reads: 3,
                observed_codec_decode_calls: 0,
            },
        }
    }

    fn valid_import_workflow_report() -> Value {
        let operation_token = json!({
            "operation_id": 1,
            "task_id": 2,
            "kind": "Import",
            "source_session_generation": 3,
            "currentness_generation": 4,
        });
        let primary_clock = json!({
            "start_boundary": "accepted_start_import_command_immediately_before_worker_spawn",
            "end_boundary": "published_destination_verified_and_open_ready_for_normal_product_use",
            "clock": "std_instant_monotonic",
            "started_at_epoch_ms": 1_100,
            "open_ready_at_epoch_ms": 2_000,
            "wall_time_ns": 1_000_000_000,
            "process_cpu_time_ns": 500_000_000,
            "inspection_and_human_review_excluded": true,
            "published_capability_transfer_and_runtime_open_included": true,
        });
        let mut statistics = json!({
            "source_bytes_read": 200,
            "source_revalidation_bytes_read": 100,
            "native_decoded_bytes": 100,
            "base_native_decoded_bytes": 100,
            "scientific_identity_native_decoded_bytes": 0,
            "tiff_open_count": 1,
            "native_chunk_decode_count": 1,
            "logical_output_bytes": 50,
            "checkpoint_payload_bytes": 60,
            "checkpoint_journal_bytes": 10,
            "checkpoint_watermark_bytes": 1,
            "checkpoint_durable_work_units": 1,
            "checkpoint_pending_work_units": 0,
            "checkpoint_committed_batches": 1,
            "codec_encode_calls": 1,
            "codec_encode_time_ns": 10,
            "codec_decode_calls": 1,
            "codec_decode_time_ns": 10,
            "sync_calls": 10,
            "sync_time_ns": 10_000_000,
        });
        let mut scientific_statistics = json!({
            "scientific_brick_reads": 1,
            "staged_structure_object_reads": 1,
            "staged_exact_object_reads": 1,
            "scientific_object_reads": 1,
            "scientific_payload_object_reads": 1,
            "scientific_range_requests": 1,
            "scientific_encoded_bytes_read": 10,
            "scientific_decoded_bytes": 20,
            "object_reads": 3,
            "sampled_peak_open_file_descriptors": 10,
            "open_file_descriptor_structural_bound": 35,
            "peak_open_file_descriptors": 35,
            "preflight_temporary_bytes_bound": 1_000,
            "peak_temporary_bytes": 900,
        });
        let mut resource_statistics = json!({
            "peak_checkpoint_regular_files": 6,
            "peak_working_bytes": 1_024,
            "peak_process_rss_bytes": 2_048,
            "resumed_work_units": 0,
            "produced_work_units": 1,
            "primary_wall_time_ns": 800_000_000,
            "primary_cpu_time_ns": 400_000_000,
            "stages": [{
                "stage": "base-production",
                "wall_time_ns": 700_000_000,
                "cpu_time_ns": 300_000_000,
            }],
        });
        statistics
            .as_object_mut()
            .unwrap()
            .append(scientific_statistics.as_object_mut().unwrap());
        statistics
            .as_object_mut()
            .unwrap()
            .append(resource_statistics.as_object_mut().unwrap());
        let receipt = json!({
            "review_id": 7,
            "operation_token": operation_token.clone(),
            "destination": "/private/attempt/imported.m4d",
            "reviewed_source_fingerprint_sha256": "11".repeat(32),
            "reviewed_source_bytes": 100,
            "published_event": {
                "published_at_epoch_ms": 1_700,
                "process_cpu_time_ns": 777,
            },
            "package_id": "test-package-id",
            "scientific_content_id": "test-scientific-content-id",
            "statistics": statistics,
        });
        let inspection_clock = json!({
            "start_boundary": "normal_import_setup_command_dispatch",
            "end_boundary": "reviewed_start_import_command_dispatch",
            "wall_clock": "std_instant_monotonic",
            "cpu_clock": "process_cpu_time",
            "started_at_epoch_ms": 1_000,
            "start_command_at_epoch_ms": 1_050,
            "wall_time_ns": 50_000_000,
            "process_cpu_time_ns": 5_000_000,
            "excluded_from_primary_clock": true,
            "human_review_interval_included_when_present": true,
        });
        let publication_clock = json!({
            "start_boundary": "import_worker_published_event",
            "end_boundary": "published_destination_verified_and_open_ready_for_normal_product_use",
            "wall_clock": "std_instant_monotonic",
            "cpu_clock": "process_cpu_time",
            "published_at_epoch_ms": 1_700,
            "open_ready_at_epoch_ms": 2_000,
            "wall_time_ns": 300_000_000,
            "process_cpu_time_ns": 50_000_000,
            "included_in_primary_clock": true,
            "transfer_mode": "staged_verified_capability",
            "publication_currentness_execution": {
                "contract_id": "test-publication-currentness",
                "expected_snapshot_object_reads": 1,
                "first_inventory_object_reads": 1,
                "observed_snapshot_object_reads": 1,
                "second_inventory_object_reads": 1,
                "observed_total_object_reads": 3,
                "observed_codec_decode_calls": 0,
            },
            "source_verification_started_runs": 0,
            "source_verification_progress_updates": 0,
            "source_verification_cancelled_runs": 0,
            "source_verification_failed_runs": 0,
            "source_verification_successes": 0,
        });
        let workflow = json!({
            "worker_emitted_stage_names": ["base-production"],
            "projected_named_stage_observations": ["base-production"],
            "maximum_projected_elapsed_ms": 700,
            "maximum_completed_by_stage": [{
                "stage": "base-production",
                "completed_work_units": 1,
            }],
            "progress_updates": 1,
            "published_events": 1,
            "cancelled_runs": 0,
            "successful_runs": 1,
            "failed_runs": 0,
            "maximum_resumed_work_units": 0,
            "maximum_peak_working_bytes": 1_024,
            "maximum_elapsed_ms": 900,
            "fabricated_global_percentage_or_eta_observed": false,
            "inspection_and_review_clock": inspection_clock,
            "primary_clock": primary_clock.clone(),
            "publication_to_open_ready_clock": publication_clock,
            "last_successful_receipt": receipt,
        });
        let open_ready_command = observe_gate_batch_command(
            "IP.batch.000",
            "IP-preprocess.checkpoint.000",
            json!({ "kind": "import_primary_started" }),
            &[(
                "IP.imported_open_ready",
                IMPORTED_OPEN_READY_CONDITION,
                "import_primary_wall",
                1_200_000_000_000,
            )],
        );
        json!({
            "events": [
                {
                    "command": "start_reviewed_import",
                    "status": "passed",
                    "details": {
                        "review_id": 7,
                        "destination": "/private/attempt/imported.m4d",
                        "operation_token": operation_token.clone(),
                        "reviewed_source_fingerprint_sha256": "11".repeat(32),
                        "reviewed_source_bytes": 100,
                        "working_memory_bytes": 1_024,
                        "primary_clock_started_at_epoch_ms": 1_100,
                        "primary_clock_start_boundary": "accepted_start_import_command_immediately_before_worker_spawn",
                        "normal_review_command_path": true,
                    },
                },
                observe_gate_batch_event(
                    1,
                    &open_ready_command,
                    &[ProductGateStatus::Passed],
                ),
            ],
            "import_workflow_evidence": workflow,
        })
    }

    fn prepublication_import_workflow_report(terminal_failure: bool) -> Value {
        let mut report = valid_import_workflow_report();
        let workflow = &mut report["import_workflow_evidence"];
        workflow["successful_runs"] = json!(0);
        workflow["published_events"] = json!(0);
        workflow["failed_runs"] = json!(u64::from(terminal_failure));
        workflow["primary_clock"] = Value::Null;
        workflow["publication_to_open_ready_clock"] = Value::Null;
        workflow["last_successful_receipt"] = Value::Null;
        let command = observe_gate_batch_command(
            "IP.batch.000",
            "preprocess_publish.checkpoint.000",
            json!({ "kind": "import_primary_started" }),
            &[
                (
                    "IP.acceptance.000.import_idle",
                    "import_idle",
                    "import_primary_wall",
                    1_200_000_000_000,
                ),
                (
                    "IP.acceptance.001.imported_open_ready",
                    IMPORTED_OPEN_READY_CONDITION,
                    "import_primary_wall",
                    1_200_000_000_000,
                ),
                (
                    "IP.acceptance.002.runtime_idle",
                    "runtime_idle",
                    "import_primary_wall",
                    1_200_000_000_000,
                ),
            ],
        );
        report["events"][1] = observe_gate_batch_event(
            1,
            &command,
            &[
                ProductGateStatus::Failed,
                ProductGateStatus::Failed,
                if terminal_failure {
                    ProductGateStatus::Passed
                } else {
                    ProductGateStatus::Failed
                },
            ],
        );
        report
    }

    fn import_workflow_gate_reasons(report: &Value) -> BTreeSet<String> {
        import_workflow_gate_reasons_with_gate(report, &test_import_gate())
    }

    fn import_workflow_gate_reasons_with_gate(
        report: &Value,
        gate: &ImportGate,
    ) -> BTreeSet<String> {
        import_workflow_gate_reasons_with_outcome(report, gate, ProductGateStatus::Passed)
    }

    fn import_workflow_gate_reasons_with_outcome(
        report: &Value,
        gate: &ImportGate,
        outcome: ProductGateStatus,
    ) -> BTreeSet<String> {
        let mut reasons = BTreeSet::new();
        validate_import_workflow_gate(report, gate, Some(outcome), &mut reasons);
        reasons
    }

    fn import_statistics_mut(report: &mut Value) -> &mut Value {
        &mut report["import_workflow_evidence"]["last_successful_receipt"]["statistics"]
    }

    fn all_stage_repeated_receipt_report() -> (ImportGate, Value) {
        let mut gate = test_import_gate();
        gate.required_worker_stage_names = vec![
            "planning-and-preflight".to_owned(),
            "base-production".to_owned(),
            "commit".to_owned(),
        ];
        gate.required_receipt_stage_names = gate.required_worker_stage_names.clone();

        let mut report = valid_import_workflow_report();
        report["import_workflow_evidence"]["worker_emitted_stage_names"] =
            json!(["planning-and-preflight", "base-production", "commit",]);
        report["import_workflow_evidence"]["maximum_completed_by_stage"] = json!([
            {
                "stage": "planning-and-preflight",
                "completed_work_units": 0,
            },
            {
                "stage": "base-production",
                "completed_work_units": 1,
            },
            {
                "stage": "base-production",
                "completed_work_units": 0,
            },
            {
                "stage": "commit",
                "completed_work_units": 0,
            },
        ]);
        import_statistics_mut(&mut report)["stages"] = json!([
            {
                "stage": "planning-and-preflight",
                "wall_time_ns": 100_000_000,
                "cpu_time_ns": 40_000_000,
            },
            {
                "stage": "base-production",
                "wall_time_ns": 300_000_000,
                "cpu_time_ns": 120_000_000,
            },
            {
                "stage": "base-production",
                "wall_time_ns": 200_000_000,
                "cpu_time_ns": 80_000_000,
            },
            {
                "stage": "commit",
                "wall_time_ns": 100_000_000,
                "cpu_time_ns": 40_000_000,
            },
        ]);
        (gate, report)
    }

    #[test]
    fn import_workflow_gate_accepts_all_worker_and_duplicate_progress_rows_and_repeated_receipt_stages()
     {
        let (gate, report) = all_stage_repeated_receipt_report();
        assert!(import_workflow_gate_reasons_with_gate(&report, &gate).is_empty());
    }

    #[test]
    fn import_workflow_gate_rejects_below_minimum_duplicate_progress_and_bad_repeated_stage_totals()
    {
        let (gate, mut below_minimum_progress) = all_stage_repeated_receipt_report();
        for entry in
            below_minimum_progress["import_workflow_evidence"]["maximum_completed_by_stage"]
                .as_array_mut()
                .unwrap()
        {
            if entry.get("stage").and_then(Value::as_str) == Some("base-production") {
                entry["completed_work_units"] = json!(0);
            }
        }
        assert!(
            import_workflow_gate_reasons_with_gate(&below_minimum_progress, &gate)
                .contains("product_gate_import_required_stage_or_progress_mismatch")
        );

        let (_, mut unreconciled_receipt) = all_stage_repeated_receipt_report();
        import_statistics_mut(&mut unreconciled_receipt)["stages"][2]["wall_time_ns"] =
            json!(500_000_001);
        assert!(
            import_workflow_gate_reasons_with_gate(&unreconciled_receipt, &gate)
                .contains("import_receipt_stage_evidence_mismatch")
        );
    }

    #[test]
    fn import_workflow_gate_accepts_reconciled_receipt_and_product_fd_authority() {
        let report = valid_import_workflow_report();
        assert!(import_workflow_gate_reasons(&report).is_empty());

        let mut above_standalone_fd_limit = report;
        let statistics = &mut above_standalone_fd_limit["import_workflow_evidence"]["last_successful_receipt"]
            ["statistics"];
        statistics["sampled_peak_open_file_descriptors"] = json!(70);
        statistics["peak_open_file_descriptors"] = json!(70);
        assert!(import_workflow_gate_reasons(&above_standalone_fd_limit).is_empty());
    }

    #[test]
    fn passed_imported_open_ready_clocks_and_currentness_require_exact_shapes() {
        let authentic = valid_import_workflow_report();
        assert!(import_workflow_gate_reasons(&authentic).is_empty());
        assert_eq!(
            reason_axis("import_clock_evidence_shape_invalid"),
            ReasonAxis::Integrity
        );

        for (label, pointer, required_field) in [
            (
                "inspection clock",
                "/import_workflow_evidence/inspection_and_review_clock",
                "wall_time_ns",
            ),
            (
                "primary clock",
                "/import_workflow_evidence/primary_clock",
                "wall_time_ns",
            ),
            (
                "publication clock",
                "/import_workflow_evidence/publication_to_open_ready_clock",
                "wall_time_ns",
            ),
            (
                "publication currentness",
                "/import_workflow_evidence/publication_to_open_ready_clock/publication_currentness_execution",
                "observed_total_object_reads",
            ),
        ] {
            let assert_shape_invalid = |report: &Value, mutation: &str| {
                let reasons = import_workflow_gate_reasons(report);
                assert!(
                    reasons.contains("import_clock_evidence_shape_invalid"),
                    "{label} {mutation} mutation did not fail exact shape: {reasons:?}"
                );
                assert!(has_integrity_reasons(&reasons));
            };

            let mut extra = authentic.clone();
            extra
                .pointer_mut(pointer)
                .and_then(Value::as_object_mut)
                .expect("test clock object exists")
                .insert("unexpected".to_owned(), json!(true));
            assert_shape_invalid(&extra, "extra-key");

            let mut missing = authentic.clone();
            missing
                .pointer_mut(pointer)
                .and_then(Value::as_object_mut)
                .expect("test clock object exists")
                .remove(required_field);
            assert_shape_invalid(&missing, "missing-key");

            let mut null = authentic.clone();
            null.pointer_mut(pointer)
                .and_then(Value::as_object_mut)
                .expect("test clock object exists")
                .insert(required_field.to_owned(), Value::Null);
            assert_shape_invalid(&null, "null-key");
        }
    }

    #[test]
    fn imported_open_ready_failure_skips_only_pass_dependent_clock_evidence() {
        let mut report = valid_import_workflow_report();
        let observation = &mut report["events"][1]["details"]["observations"][0];
        observation["outcome"] = json!("failed");
        observation["condition_met"] = json!(false);
        observation["timed_out"] = json!(true);
        observation["observed_after_origin_ns"] = json!(1_200_000_000_000_u64);
        assert!(
            import_workflow_gate_reasons_with_outcome(
                &report,
                &test_import_gate(),
                ProductGateStatus::Failed,
            )
            .contains("failed_imported_open_ready_pass_only_evidence_present")
        );

        report["import_workflow_evidence"]["primary_clock"] = Value::Null;
        for field in [
            "start_boundary",
            "end_boundary",
            "wall_clock",
            "cpu_clock",
            "published_at_epoch_ms",
            "open_ready_at_epoch_ms",
            "wall_time_ns",
            "process_cpu_time_ns",
            "included_in_primary_clock",
            "transfer_mode",
        ] {
            report["import_workflow_evidence"]["publication_to_open_ready_clock"]
                .as_object_mut()
                .unwrap()
                .remove(field);
        }
        assert!(
            import_workflow_gate_reasons_with_outcome(
                &report,
                &test_import_gate(),
                ProductGateStatus::Failed,
            )
            .is_empty()
        );

        let authentic_failed_report = report.clone();

        let mut missing_primary_clock = authentic_failed_report.clone();
        missing_primary_clock["import_workflow_evidence"]
            .as_object_mut()
            .unwrap()
            .remove("primary_clock");
        assert!(
            import_workflow_gate_reasons_with_outcome(
                &missing_primary_clock,
                &test_import_gate(),
                ProductGateStatus::Failed,
            )
            .contains("failed_imported_open_ready_evidence_shape_invalid")
        );

        let mut null_pass_clock = authentic_failed_report.clone();
        null_pass_clock["import_workflow_evidence"]["publication_to_open_ready_clock"]["wall_time_ns"] =
            Value::Null;
        let null_pass_clock_reasons = import_workflow_gate_reasons_with_outcome(
            &null_pass_clock,
            &test_import_gate(),
            ProductGateStatus::Failed,
        );
        assert!(
            null_pass_clock_reasons.contains("failed_imported_open_ready_evidence_shape_invalid")
        );
        assert!(
            null_pass_clock_reasons
                .contains("failed_imported_open_ready_pass_only_evidence_present")
        );

        let mut extra_partial_field = authentic_failed_report.clone();
        extra_partial_field["import_workflow_evidence"]["publication_to_open_ready_clock"]["unexpected"] =
            json!(true);
        assert!(
            import_workflow_gate_reasons_with_outcome(
                &extra_partial_field,
                &test_import_gate(),
                ProductGateStatus::Failed,
            )
            .contains("failed_imported_open_ready_evidence_shape_invalid")
        );

        let mut extra_currentness_field = authentic_failed_report.clone();
        extra_currentness_field["import_workflow_evidence"]["publication_to_open_ready_clock"]["publication_currentness_execution"]
            ["unexpected"] = json!(true);
        assert!(
            import_workflow_gate_reasons_with_outcome(
                &extra_currentness_field,
                &test_import_gate(),
                ProductGateStatus::Failed,
            )
            .contains("failed_imported_open_ready_evidence_shape_invalid")
        );

        let mut missing_partial_field = authentic_failed_report.clone();
        missing_partial_field["import_workflow_evidence"]["publication_to_open_ready_clock"]
            .as_object_mut()
            .unwrap()
            .remove("source_verification_successes");
        assert!(
            import_workflow_gate_reasons_with_outcome(
                &missing_partial_field,
                &test_import_gate(),
                ProductGateStatus::Failed,
            )
            .contains("failed_imported_open_ready_evidence_shape_invalid")
        );

        report["import_workflow_evidence"]["publication_to_open_ready_clock"]["publication_currentness_execution"] =
            Value::Null;
        assert!(
            import_workflow_gate_reasons_with_outcome(
                &report,
                &test_import_gate(),
                ProductGateStatus::Failed,
            )
            .contains("import_publication_currentness_evidence_mismatch")
        );
    }

    #[test]
    fn prepublication_import_timeout_accepts_only_the_two_coherent_worker_states() {
        for terminal_failure in [true, false] {
            let report = prepublication_import_workflow_report(terminal_failure);
            let reasons = import_workflow_gate_reasons_with_outcome(
                &report,
                &test_import_gate(),
                ProductGateStatus::Failed,
            );
            assert!(reasons.is_empty(), "{terminal_failure}: {reasons:?}");
        }

        let mut initial_currentness = prepublication_import_workflow_report(true);
        initial_currentness["events"][0]["details"]["operation_token"]
            ["currentness_generation"] = json!(0);
        let reasons = import_workflow_gate_reasons_with_outcome(
            &initial_currentness,
            &test_import_gate(),
            ProductGateStatus::Failed,
        );
        assert!(reasons.is_empty(), "{reasons:?}");

        let mut mixed = prepublication_import_workflow_report(true);
        mixed["import_workflow_evidence"]["failed_runs"] = json!(0);
        let reasons = import_workflow_gate_reasons_with_outcome(
            &mixed,
            &test_import_gate(),
            ProductGateStatus::Failed,
        );
        assert!(reasons.contains("prepublication_import_failure_evidence_shape_invalid"));
        assert!(has_integrity_reasons(&reasons));

        let mut cancelled = prepublication_import_workflow_report(false);
        cancelled["import_workflow_evidence"]["cancelled_runs"] = json!(1);
        assert!(
            import_workflow_gate_reasons_with_outcome(
                &cancelled,
                &test_import_gate(),
                ProductGateStatus::Failed,
            )
            .contains("prepublication_import_failure_evidence_shape_invalid")
        );
    }

    #[test]
    fn published_import_receipt_accepts_the_authentic_initial_currentness_generation() {
        let mut report = valid_import_workflow_report();
        report["events"][0]["details"]["operation_token"]["currentness_generation"] =
            json!(0);
        report["import_workflow_evidence"]["last_successful_receipt"]["operation_token"]
            ["currentness_generation"] = json!(0);
        let reasons = import_workflow_gate_reasons(&report);
        assert!(reasons.is_empty(), "{reasons:?}");
    }

    #[test]
    fn prepublication_import_source_binding_uses_start_event_without_a_receipt() {
        let report = prepublication_import_workflow_report(false);
        let binding = ImportSourceBinding {
            regular_files: 1,
            source_bytes: 100,
            inventory_sha256: "22".repeat(32),
            reviewed_source_fingerprint_sha256: "11".repeat(32),
        };
        let mut reasons = BTreeSet::new();
        validate_import_report_source_binding(&report, &binding, &mut reasons);
        assert!(reasons.is_empty());

        let mut mismatched = report;
        mismatched["events"][0]["details"]["reviewed_source_bytes"] = json!(99);
        validate_import_report_source_binding(&mismatched, &binding, &mut reasons);
        assert!(reasons.contains("import_receipt_workload_source_binding_mismatch"));
    }

    #[test]
    fn cleanup_accepts_a_verified_absent_attempt_local_import_target() {
        let role_root = tempfile::tempdir().unwrap();
        let cleanup = AttemptCleanup {
            enabled: true,
            imported_package_relative_path: Some(PathBuf::from("output/imported.m4d")),
        };
        prepare_attempt_import_parent(role_root.path(), &cleanup).unwrap();
        assert!(role_root.path().join("output").is_dir());
        assert!(!role_root.path().join("output/imported.m4d").exists());
        assert_eq!(
            cleanup_attempt_package(role_root.path(), &cleanup).unwrap(),
            None
        );
    }

    #[test]
    fn import_workflow_gate_rejects_run_clock_currentness_and_binding_mutations() {
        let mut report = valid_import_workflow_report();
        report["import_workflow_evidence"]["successful_runs"] = json!(2);
        assert!(
            import_workflow_gate_reasons(&report)
                .contains("product_gate_import_workflow_run_counts_or_progress_claim_mismatch")
        );

        let mut report = valid_import_workflow_report();
        report["import_workflow_evidence"]["fabricated_global_percentage_or_eta_observed"] =
            json!(true);
        assert!(
            import_workflow_gate_reasons(&report)
                .contains("product_gate_import_workflow_run_counts_or_progress_claim_mismatch")
        );

        let mut report = valid_import_workflow_report();
        report["import_workflow_evidence"]["primary_clock"]["start_boundary"] = json!("wrong");
        assert!(
            import_workflow_gate_reasons(&report)
                .contains("import_clock_boundaries_or_order_mismatch")
        );

        let mut report = valid_import_workflow_report();
        report["import_workflow_evidence"]["publication_to_open_ready_clock"]["source_verification_started_runs"] =
            json!(1);
        assert!(
            import_workflow_gate_reasons(&report)
                .contains("product_gate_import_ordinary_source_verifier_activity_observed")
        );

        let mut report = valid_import_workflow_report();
        report["import_workflow_evidence"]["publication_to_open_ready_clock"]["publication_currentness_execution"]
            ["observed_total_object_reads"] = json!(4);
        assert!(
            import_workflow_gate_reasons(&report)
                .contains("product_gate_import_publication_currentness_observation_mismatch")
        );

        let mut report = valid_import_workflow_report();
        report["events"][1]["details"]["observations"][0]["condition"] = json!("wrong");
        assert!(
            import_workflow_gate_reasons(&report)
                .contains("import_receipt_start_or_open_ready_binding_mismatch")
        );
    }

    #[test]
    fn import_workflow_gate_rejects_receipt_counter_and_resource_mutations() {
        let mut report = valid_import_workflow_report();
        import_statistics_mut(&mut report)
            .as_object_mut()
            .unwrap()
            .remove("sync_calls");
        assert!(
            import_workflow_gate_reasons(&report).contains("import_receipt_statistics_missing")
        );

        let mut report = valid_import_workflow_report();
        import_statistics_mut(&mut report)["produced_work_units"] = json!(2);
        assert!(
            import_workflow_gate_reasons(&report)
                .contains("import_receipt_expected_count_mismatch")
        );

        let mut report = valid_import_workflow_report();
        import_statistics_mut(&mut report)["source_bytes_read"] = json!(251);
        assert!(
            import_workflow_gate_reasons(&report)
                .contains("import_receipt_source_read_amplification_exceeded")
        );

        let mut report = valid_import_workflow_report();
        import_statistics_mut(&mut report)["open_file_descriptor_structural_bound"] = json!(36);
        import_statistics_mut(&mut report)["peak_open_file_descriptors"] = json!(36);
        assert!(
            import_workflow_gate_reasons(&report)
                .contains("import_receipt_resource_limit_exceeded")
        );

        let mut report = valid_import_workflow_report();
        import_statistics_mut(&mut report)["peak_temporary_bytes"] = json!(1_001);
        assert!(
            import_workflow_gate_reasons(&report)
                .contains("import_receipt_resource_limit_exceeded")
        );

        let mut report = valid_import_workflow_report();
        import_statistics_mut(&mut report)["native_decoded_bytes"] = json!(101);
        assert!(
            import_workflow_gate_reasons(&report)
                .contains("import_receipt_counter_reconciliation_failed")
        );

        let mut report = valid_import_workflow_report();
        import_statistics_mut(&mut report)["stages"][0]["stage"] = json!("wrong-stage");
        assert!(
            import_workflow_gate_reasons(&report)
                .contains("import_receipt_stage_evidence_mismatch")
        );
    }

    #[test]
    fn import_workflow_gate_rejects_each_clock_and_receipt_limit_class() {
        let mut report = valid_import_workflow_report();
        report["import_workflow_evidence"]["primary_clock"]["wall_time_ns"] =
            json!(1_200_000_000_001_u64);
        assert!(import_workflow_gate_reasons(&report).contains("import_clock_limit_exceeded"));

        let mut report = valid_import_workflow_report();
        report["import_workflow_evidence"]["publication_to_open_ready_clock"]["process_cpu_time_ns"] =
            json!(60_000_000_001_u64);
        assert!(import_workflow_gate_reasons(&report).contains("import_clock_limit_exceeded"));

        let mut report = valid_import_workflow_report();
        import_statistics_mut(&mut report)["primary_cpu_time_ns"] = json!(1_200_000_000_001_u64);
        assert!(
            import_workflow_gate_reasons(&report)
                .contains("import_receipt_primary_clock_limit_exceeded")
        );

        let mut report = valid_import_workflow_report();
        import_statistics_mut(&mut report)["peak_working_bytes"] = json!(256 * 1024 * 1024 + 1);
        report["import_workflow_evidence"]["maximum_peak_working_bytes"] =
            json!(256 * 1024 * 1024 + 1);
        assert!(
            import_workflow_gate_reasons(&report)
                .contains("import_receipt_resource_limit_exceeded")
        );

        let mut report = valid_import_workflow_report();
        import_statistics_mut(&mut report)["peak_process_rss_bytes"] = json!(512 * 1024 * 1024 + 1);
        assert!(
            import_workflow_gate_reasons(&report)
                .contains("import_receipt_resource_limit_exceeded")
        );

        let mut report = valid_import_workflow_report();
        import_statistics_mut(&mut report)["sync_calls"] = json!(5_001);
        assert!(
            import_workflow_gate_reasons(&report)
                .contains("import_receipt_resource_limit_exceeded")
        );
    }

    #[test]
    fn import_gate_contract_requires_distinct_and_reconciled_oracle_authorities() {
        validate_import_gate_contract(&test_import_gate()).unwrap();

        let mut gate = test_import_gate();
        gate.limits.maximum_open_file_descriptor_structural_bound =
            gate.limits.maximum_product_peak_open_file_descriptors;
        assert!(validate_import_gate_contract(&gate).is_err());

        let mut gate = test_import_gate();
        gate.expected.checkpoint_durable_work_units += 1;
        assert!(validate_import_gate_contract(&gate).is_err());

        let mut gate = test_import_gate();
        gate.publication_currentness.observed_total_object_reads += 1;
        assert!(validate_import_gate_contract(&gate).is_err());
    }

    fn test_verification_gate(kind: VerificationGateKind) -> VerificationGate {
        let checkpoint = VerificationCheckpointExpectation {
            state: ExpectedSourceVerificationState::Verifying,
            active_operation: true,
            started_runs: 1,
            cancelled_runs: 0,
            failed_runs: 0,
            accepted_successes: 0,
            completed_reader_runs: 0,
        };
        VerificationGate {
            kind,
            start: checkpoint,
            end: checkpoint,
            minimum_accepted_progress_updates_delta: 0,
            completed_reader_work: None,
        }
    }

    #[test]
    fn attempt_root_is_the_only_allowed_substitution_and_stays_relative() {
        validate_placeholder_string("${ATTEMPT_ROOT}/output/demo.m4d").unwrap();
        assert!(validate_placeholder_string("$HOME/output").is_err());
        assert!(validate_placeholder_string("${ATTEMPT_ROOT}/../escape").is_err());
        assert!(validate_placeholder_string("prefix/${ATTEMPT_ROOT}").is_err());

        let expanded = expand_script_template(
            json!({ "path": "${ATTEMPT_ROOT}/output/demo.m4d" }),
            Path::new("/private/result/sample-01/IP/instrumented"),
        )
        .unwrap();
        assert_eq!(
            expanded["path"],
            "/private/result/sample-01/IP/instrumented/output/demo.m4d"
        );
    }

    #[test]
    fn dataset_actions_bind_one_representative_startup_open_and_one_pt_switch() {
        let representative = Path::new("/private/representative.m4d");
        let valid = dataset_contract_scenario("PT");
        assert_eq!(
            validate_dataset_action_contract("PT", &valid, representative).unwrap(),
            Some(PathBuf::from("/private/temporal.m4d"))
        );

        let mut wrong_open = dataset_contract_scenario("PT");
        wrong_open.instrumented_script.commands[0]["path"] = json!("/private/wrong.m4d");
        assert!(validate_dataset_action_contract("PT", &wrong_open, representative).is_err());

        let mut nonfirst_open = dataset_contract_scenario("PT");
        nonfirst_open.instrumented_script.commands.swap(0, 1);
        assert!(validate_dataset_action_contract("PT", &nonfirst_open, representative).is_err());

        let mut duplicate_open = dataset_contract_scenario("PT");
        duplicate_open.instrumented_script.commands.insert(
            1,
            json!({ "command": "open_dataset", "path": "/private/representative.m4d" }),
        );
        assert!(validate_dataset_action_contract("PT", &duplicate_open, representative).is_err());

        let mut switch_with_timeout = dataset_contract_scenario("PT");
        switch_with_timeout.instrumented_script.commands[3]["timeout_ms"] = json!(120_000);
        assert!(
            validate_dataset_action_contract("PT", &switch_with_timeout, representative).is_err()
        );

        let mut duplicate_switch = dataset_contract_scenario("PT");
        duplicate_switch.instrumented_script.commands.insert(
            4,
            json!({ "command": "switch_dataset", "path": "/private/other.m4d" }),
        );
        assert!(validate_dataset_action_contract("PT", &duplicate_switch, representative).is_err());

        let mut same_target = dataset_contract_scenario("PT");
        same_target.instrumented_script.commands[3]["path"] = json!("/private/representative.m4d");
        assert!(validate_dataset_action_contract("PT", &same_target, representative).is_err());

        let mut relative_target = dataset_contract_scenario("PT");
        relative_target.instrumented_script.commands[3]["path"] = json!("temporal.m4d");
        assert!(validate_dataset_action_contract("PT", &relative_target, representative).is_err());

        let mut missing_post_switch_verification = dataset_contract_scenario("PT");
        missing_post_switch_verification
            .instrumented_script
            .commands
            .remove(5);
        assert!(
            validate_dataset_action_contract(
                "PT",
                &missing_post_switch_verification,
                representative,
            )
            .unwrap_err()
            .to_string()
            .contains("after switch_dataset")
        );

        let mut non_pt = dataset_contract_scenario("RZ");
        non_pt.phases[0].start_diagnostic_label = None;
        assert!(validate_dataset_action_contract("RZ", &non_pt, representative).is_err());
        non_pt.instrumented_script.commands.remove(3);
        assert_eq!(
            validate_dataset_action_contract("RZ", &non_pt, representative).unwrap(),
            None
        );
    }

    #[test]
    fn supporting_temporal_package_is_external_nonsymlink_and_content_bound() {
        let repository = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let package = external.path().join("temporal.m4d");
        fs::create_dir_all(package.join("m4d/manifest")).unwrap();
        let root_manifest = br#"{"schema":"test-temporal-root"}"#;
        fs::write(package.join("m4d/manifest/root.json"), root_manifest).unwrap();
        let expected = Sha256Hasher::digest(root_manifest).to_string();
        validate_supporting_temporal_package(
            package.to_str().unwrap(),
            &expected,
            repository.path(),
        )
        .unwrap();
        assert!(
            validate_supporting_temporal_package(
                package.to_str().unwrap(),
                &"00".repeat(32),
                repository.path(),
            )
            .is_err()
        );
        assert!(
            validate_supporting_temporal_package(
                package.to_str().unwrap(),
                &expected,
                external.path(),
            )
            .unwrap_err()
            .to_string()
            .contains("outside the repository")
        );
        let linked = external.path().join("linked-temporal.m4d");
        std::os::unix::fs::symlink(&package, &linked).unwrap();
        assert!(
            validate_supporting_temporal_package(
                linked.to_str().unwrap(),
                &expected,
                repository.path(),
            )
            .unwrap_err()
            .to_string()
            .contains("symbolic links")
        );
    }

    #[test]
    fn instrumentation_control_must_preserve_semantic_commands() {
        let instrumented = template(
            true,
            vec![
                json!({ "command": "camera_zoom_sequence", "samples": 3, "duration_ms": 10, "scroll_y_points_per_sample": 1.0 }),
                json!({ "command": "sample_diagnostics", "label": "end" }),
                json!({ "command": "quit" }),
            ],
        );
        let control = template(
            false,
            vec![
                json!({ "command": "camera_zoom_sequence", "samples": 3, "duration_ms": 10, "scroll_y_points_per_sample": 1.0 }),
                json!({ "command": "quit" }),
            ],
        );
        assert_eq!(
            normalized_semantic_script(&instrumented),
            normalized_semantic_script(&control)
        );
        let mut changed = control;
        changed.commands[0]["samples"] = json!(4);
        assert_ne!(
            normalized_semantic_script(&instrumented),
            normalized_semantic_script(&changed)
        );
    }

    #[test]
    fn paired_overhead_never_invents_a_missing_or_zero_control() {
        let mut reasons = BTreeSet::new();
        assert_eq!(
            paired_overhead_basis_points(Some(1_020), Some(1_000), "missing", &mut reasons),
            Some(200)
        );
        assert!(reasons.is_empty());
        assert_eq!(
            paired_overhead_basis_points(Some(1), None, "missing", &mut reasons),
            None
        );
        assert!(reasons.contains("missing"));
    }

    #[test]
    fn qualification_gpu_timing_await_wall_uses_only_exact_completed_variants() {
        let script = template(
            true,
            vec![
                json!({
                    "command": "await_active_view_gpu_timing",
                    "target": "three_d",
                    "pass_kind": "volume",
                    "timeout_ms": GPU_TIMING_AWAIT_TIMEOUT_MS,
                }),
                json!({ "command": "sample_diagnostics", "label": "three-d-end" }),
                json!({ "command": "sleep_frames", "frames": 1 }),
                json!({
                    "command": "await_active_view_gpu_timing",
                    "target": "xy",
                    "pass_kind": "plane",
                    "timeout_ms": GPU_TIMING_AWAIT_TIMEOUT_MS,
                }),
                json!({ "command": "sample_diagnostics", "label": "xy-end" }),
            ],
        );
        let diagnostic = |label,
                          panel,
                          pass_kind,
                          execution_id,
                          renderer_target,
                          frame,
                          waited_ns| {
            json!({
                "label": label,
                "diagnostics": {
                    "render": {
                        "qualification_gpu_timing_checkpoint": {
                            "available": true,
                            "derivation": "identity_frozen_from_current_execution_then_completed_by_exact_presented_interval_ticket",
                            "reason": null,
                            "presented_interval_sequence": 1,
                            "panel": panel,
                            "execution_id": execution_id,
                            "target": renderer_target,
                            "display_generation": 3,
                            "current_presentation_generation": 3,
                            "renderer_frame": frame,
                            "pass_kind": pass_kind,
                            "gpu_batch_envelope_ns": 100,
                            "gpu_payload_copy_ns": null,
                            "gpu_render_pass_ns": 80,
                            "identity_frozen_before_completion": true,
                            "exact_presented_interval_timing_complete": true,
                            "unavailable_authority": null,
                            "waited_ns": waited_ns,
                        },
                    },
                },
            })
        };
        let three_d_diagnostic =
            diagnostic("three-d-end", "3D", "Volume", 1, 2, 4, 3_500_000_000_u64);
        let xy_diagnostic = diagnostic("xy-end", "XY", "Plane", 5, 6, 7, 7_u64);
        let await_event = |command_index,
                           target,
                           pass_kind,
                           execution_id,
                           renderer_target,
                           frame,
                           waited_ns,
                           waited_ms| {
            json!({
                "command_index": command_index,
                "command": "await_active_view_gpu_timing",
                "status": "passed",
                "event_epoch_ms": 1,
                "duration_ms": 0.01,
                "details": {
                    "available": true,
                    "unavailable_reason": null,
                    "target": target,
                    "pass_kind": pass_kind,
                    "display_generation": 3,
                    "current_presentation_generation": 3,
                    "execution_id": execution_id,
                    "renderer_target": renderer_target,
                    "renderer_frame": frame,
                    "identity_frozen_before_completion": true,
                    "exact_presented_interval_timing_complete": true,
                    "unavailable_authority": null,
                    "waited_ns": waited_ns,
                    "waited_ms": waited_ms,
                },
            })
        };
        let diagnostic_event = |command_index, details: Value| {
            json!({
                "command_index": command_index,
                "command": "sample_diagnostics",
                "status": "passed",
                "event_epoch_ms": 1,
                "duration_ms": 0.01,
                "details": details,
            })
        };
        let mut report = json!({
            "events": [
                await_event(0, "three_d", "volume", 1, 2, 4, 3_500_000_000_u64, 3_500.0),
                diagnostic_event(1, three_d_diagnostic.clone()),
                {
                    "command_index": 2,
                    "command": "sleep_frames",
                    "status": "passed",
                    "event_epoch_ms": 1,
                    "duration_ms": 0.01,
                    "details": {},
                },
                await_event(3, "xy", "plane", 5, 6, 7, 7_u64, 0.000_007),
                diagnostic_event(4, xy_diagnostic.clone()),
            ],
            "diagnostics": [three_d_diagnostic, xy_diagnostic],
        });
        report["events"][3]["details"]["current_presentation_generation"] = json!(2);
        report["events"][4]["details"]["diagnostics"]["render"]["qualification_gpu_timing_checkpoint"]
            ["current_presentation_generation"] = json!(2);
        report["diagnostics"][1]["diagnostics"]["render"]["qualification_gpu_timing_checkpoint"]
            ["current_presentation_generation"] = json!(2);
        let mut reasons = BTreeSet::new();
        assert_eq!(
            qualification_gpu_timing_await_wall_ns(Some(&report), &script, &mut reasons),
            Some(3_500_000_007)
        );
        assert!(reasons.is_empty());

        let mut malformed = report;
        malformed["events"][3]["details"]["waited_ns"] = Value::Null;
        assert_eq!(
            qualification_gpu_timing_await_wall_ns(Some(&malformed), &script, &mut reasons),
            None
        );
        assert!(reasons.contains("qualification_gpu_timing_await_evidence_missing_or_invalid"));
    }

    #[test]
    fn qualification_gpu_timing_await_accepts_exact_terminal_unavailable_evidence() {
        let gate = json!({
            "command": "observe_gate_batch",
            "batch_id": "NO.batch.001",
            "phase_id": "nonresident_rotation_pan.checkpoint.000",
            "origin": { "kind": "automation_started" },
            "observations": [{
                "gate_id": "NO.acceptance.002.coordinated_presentation_settled",
                "deadline_authority": "cold_target_settlement",
                "deadline_after_origin_ns": 100,
                "target": {
                    "kind": "condition",
                    "condition": "coordinated_presentation_settled",
                },
            }],
        });
        let script = template(
            true,
            vec![
                gate,
                json!({
                    "command": "await_active_view_gpu_timing",
                    "target": "xy",
                    "pass_kind": "plane",
                    "timeout_ms": GPU_TIMING_AWAIT_TIMEOUT_MS,
                }),
                json!({ "command": "sample_diagnostics", "label": "no-end" }),
            ],
        );
        let authority = json!({
            "command_index": 0,
            "batch_id": "NO.batch.001",
            "phase_id": "nonresident_rotation_pan.checkpoint.000",
            "observation_index": 0,
            "gate_id": "NO.acceptance.002.coordinated_presentation_settled",
            "condition": "coordinated_presentation_settled",
            "deadline_authority": "cold_target_settlement",
            "deadline_after_origin_ns": 100,
            "outcome": "failed",
            "condition_met": false,
            "timed_out": true,
            "observed_after_origin_ns": 100,
        });
        let diagnostic = json!({
            "label": "no-end",
            "diagnostics": { "render": {
                "display_coordination": {
                    "input_generation": 7,
                    "current_presentation_generation": null,
                },
                "qualification_gpu_timing_checkpoint": {
                    "available": false,
                    "derivation": GPU_TIMING_UNAVAILABLE_DERIVATION,
                    "reason": GPU_TIMING_UNAVAILABLE_REASON,
                    "presented_interval_sequence": null,
                    "panel": "XY",
                    "execution_id": null,
                    "target": null,
                    "display_generation": 7,
                    "current_presentation_generation": null,
                    "renderer_frame": null,
                    "pass_kind": "Plane",
                    "gpu_batch_envelope_ns": null,
                    "gpu_payload_copy_ns": null,
                    "gpu_render_pass_ns": null,
                    "identity_frozen_before_completion": false,
                    "exact_presented_interval_timing_complete": false,
                    "unavailable_authority": authority.clone(),
                    "waited_ns": 0,
                },
            }},
        });
        let mut report = json!({
            "events": [{
                "command_index": 0,
                "command": "observe_gate_batch",
                "status": "passed",
                "event_epoch_ms": 1,
                "duration_ms": 0.0,
                "details": {
                    "schema": PRODUCT_GATE_OBSERVATION_SCHEMA,
                    "batch_id": "NO.batch.001",
                    "phase_id": "nonresident_rotation_pan.checkpoint.000",
                    "origin": { "kind": "automation_started" },
                    "completed_after_origin_ns": 100,
                    "observations": [{
                        "observation_index": 0,
                        "gate_id": "NO.acceptance.002.coordinated_presentation_settled",
                        "condition": "coordinated_presentation_settled",
                        "deadline_authority": "cold_target_settlement",
                        "deadline_after_origin_ns": 100,
                        "outcome": "failed",
                        "condition_met": false,
                        "timed_out": true,
                        "observed_after_origin_ns": 100,
                    }],
                },
            }, {
                "command_index": 1,
                "command": "await_active_view_gpu_timing",
                "status": "passed",
                "event_epoch_ms": 1,
                "duration_ms": 0.0,
                "details": {
                    "available": false,
                    "unavailable_reason": GPU_TIMING_UNAVAILABLE_REASON,
                    "target": "xy",
                    "pass_kind": "plane",
                    "display_generation": 7,
                    "current_presentation_generation": null,
                    "execution_id": null,
                    "renderer_target": null,
                    "renderer_frame": null,
                    "identity_frozen_before_completion": false,
                    "exact_presented_interval_timing_complete": false,
                    "unavailable_authority": authority,
                    "waited_ns": 0,
                    "waited_ms": 0.0,
                },
            }, {
                "command_index": 2,
                "command": "sample_diagnostics",
                "status": "passed",
                "event_epoch_ms": 1,
                "duration_ms": 0.0,
                "details": diagnostic.clone(),
            }],
            "diagnostics": [diagnostic],
        });
        let mut reasons = BTreeSet::new();
        assert_eq!(
            qualification_gpu_timing_await_wall_ns(Some(&report), &script, &mut reasons),
            Some(0)
        );
        assert!(reasons.is_empty());

        let mut ambiguous_script = script.clone();
        ambiguous_script.commands[0]["observations"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "gate_id": "NO.acceptance.003.second_coordinated_presentation",
                "deadline_authority": "cold_target_settlement",
                "deadline_after_origin_ns": 100,
                "target": {
                    "kind": "condition",
                    "condition": "coordinated_presentation_settled",
                },
            }));
        let mut ambiguous_report = report.clone();
        ambiguous_report["events"][0]["details"]["observations"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "observation_index": 1,
                "gate_id": "NO.acceptance.003.second_coordinated_presentation",
                "condition": "coordinated_presentation_settled",
                "deadline_authority": "cold_target_settlement",
                "deadline_after_origin_ns": 100,
                "outcome": "passed",
                "condition_met": true,
                "timed_out": false,
                "observed_after_origin_ns": 50,
            }));
        let mut ambiguous_reasons = BTreeSet::new();
        assert_eq!(
            qualification_gpu_timing_await_wall_ns(
                Some(&ambiguous_report),
                &ambiguous_script,
                &mut ambiguous_reasons,
            ),
            None
        );
        assert!(
            ambiguous_reasons
                .contains("qualification_gpu_timing_await_evidence_missing_or_invalid")
        );

        report["events"][1]["details"]["unavailable_authority"]["gate_id"] = json!("wrong");
        assert_eq!(
            qualification_gpu_timing_await_wall_ns(Some(&report), &script, &mut reasons),
            None
        );
        assert!(reasons.contains("qualification_gpu_timing_await_evidence_missing_or_invalid"));
    }

    #[test]
    fn instrumentation_overhead_gate_uses_complete_balanced_scenario_populations() {
        let profile = profile();
        let scripts = population_scripts();
        let mut samples = complete_population_samples();
        for sample in samples.iter_mut().filter(|sample| sample.scenario == "RZ") {
            let adjusted: u64 = if sample.sample_index == 1 { 120 } else { 90 };
            let wait = u64::from(sample.sample_index) * 10;
            sample.instrumented.app_wall_time_ns = adjusted.checked_add(wait);
            sample.instrumented_adjusted_wall_time_ns = Some(adjusted);
            sample.instrumented_qualification_wait_wall_ns = Some(wait);
            sample.instrumented.process_cpu_time_ns = Some(100);
            let control = sample.control.as_mut().unwrap();
            control.app_wall_time_ns = Some(100);
            control.process_cpu_time_ns = Some(100);
        }
        let mut reasons = BTreeSet::new();
        let population = validate_attempt_population(&profile, &scripts, &samples, &mut reasons);
        assert!(population_evidence_is_exact(population));
        let rows = validate_population_instrumentation_overhead(
            &profile,
            &samples,
            population,
            &mut reasons,
        );
        let rz = rows.iter().find(|row| row.scenario == "RZ").unwrap();
        assert_eq!(rz.instrumented_raw_app_wall_time_ns, Some(360));
        assert_eq!(rz.instrumented_qualification_wait_wall_ns, Some(60));
        assert_eq!(rz.instrumented_adjusted_app_wall_time_ns, Some(300));
        assert_eq!(rz.wall_overhead_basis_points, Some(0));
        assert!(rz.population_complete);
        assert!(rz.gate_evaluable);
        assert_eq!(rz.gate_passed, Some(true));
        assert!(!reasons.contains("instrumentation_overhead_gate_exceeded"));

        let third = samples
            .iter_mut()
            .find(|sample| sample.scenario == "RZ" && sample.sample_index == 3)
            .unwrap();
        third.instrumented.app_wall_time_ns = Some(150);
        third.instrumented_adjusted_wall_time_ns = Some(120);
        reasons.clear();
        let population = validate_attempt_population(&profile, &scripts, &samples, &mut reasons);
        let rows = validate_population_instrumentation_overhead(
            &profile,
            &samples,
            population,
            &mut reasons,
        );
        let rz = rows.iter().find(|row| row.scenario == "RZ").unwrap();
        assert_eq!(rz.wall_overhead_basis_points, Some(1_000));
        assert!(rz.gate_evaluable);
        assert_eq!(rz.gate_passed, Some(false));
        assert!(reasons.contains("instrumentation_overhead_gate_exceeded"));
    }

    #[test]
    fn instrumentation_overhead_population_fails_closed_on_missing_wait_evidence() {
        let profile = profile();
        let scripts = population_scripts();
        let mut samples = complete_population_samples();
        samples
            .iter_mut()
            .find(|sample| sample.scenario == "RZ" && sample.sample_index == 2)
            .unwrap()
            .instrumented_qualification_wait_wall_ns = None;
        let mut reasons = BTreeSet::new();
        let population = validate_attempt_population(&profile, &scripts, &samples, &mut reasons);
        let rows = validate_population_instrumentation_overhead(
            &profile,
            &samples,
            population,
            &mut reasons,
        );
        let rz = rows.iter().find(|row| row.scenario == "RZ").unwrap();
        assert!(rz.population_complete);
        assert!(!rz.gate_evaluable);
        assert_eq!(rz.gate_passed, None);
        assert!(
            reasons.contains("instrumentation_adjusted_wall_time_population_reconciliation_failed")
        );
        assert!(reasons.contains("instrumentation_wall_overhead_population_fact_missing"));
    }

    #[test]
    fn instrumentation_overhead_population_rejects_overflow_and_inexact_population() {
        let profile = profile();
        let scripts = population_scripts();
        let mut samples = complete_population_samples();
        let first = samples
            .iter_mut()
            .find(|sample| sample.scenario == "RZ" && sample.sample_index == 1)
            .unwrap();
        first.instrumented.app_wall_time_ns = Some(u64::MAX);
        first.instrumented_adjusted_wall_time_ns = Some(u64::MAX);
        let mut reasons = BTreeSet::new();
        let population = validate_attempt_population(&profile, &scripts, &samples, &mut reasons);
        let rows = validate_population_instrumentation_overhead(
            &profile,
            &samples,
            population,
            &mut reasons,
        );
        let rz = rows.iter().find(|row| row.scenario == "RZ").unwrap();
        assert!(rz.population_complete);
        assert!(!rz.gate_evaluable);
        assert_eq!(rz.gate_passed, None);
        assert!(
            reasons.contains("instrumentation_adjusted_wall_time_population_reconciliation_failed")
        );
        assert!(reasons.contains("instrumentation_wall_overhead_population_fact_missing"));

        let mut reordered = complete_population_samples();
        reordered.swap(0, 1);
        reasons.clear();
        let population = validate_attempt_population(&profile, &scripts, &reordered, &mut reasons);
        let rows = validate_population_instrumentation_overhead(
            &profile,
            &reordered,
            population,
            &mut reasons,
        );
        assert!(rows.iter().all(|row| !row.population_complete));
        assert!(rows.iter().all(|row| !row.gate_evaluable));
        assert!(rows.iter().all(|row| row.gate_passed.is_none()));
        assert!(reasons.contains("sample_population_order_mismatch"));
        assert!(!reasons.contains("instrumentation_overhead_gate_exceeded"));

        let incomplete = complete_population_samples()
            .into_iter()
            .skip(1)
            .collect::<Vec<_>>();
        reasons.clear();
        let population = validate_attempt_population(&profile, &scripts, &incomplete, &mut reasons);
        let rows = validate_population_instrumentation_overhead(
            &profile,
            &incomplete,
            population,
            &mut reasons,
        );
        assert!(rows.iter().all(|row| !row.population_complete));
        assert!(rows.iter().all(|row| !row.gate_evaluable));
        assert!(reasons.contains("instrumentation_overhead_population_missing"));
    }

    #[test]
    fn zero_work_requires_present_monotonic_equal_counters() {
        let start = json!({
            "dataset_source_io": { "reader": { "physical_range_read_operations": 7 } },
        });
        let equal = start.clone();
        let changed = json!({
            "dataset_source_io": { "reader": { "physical_range_read_operations": 8 } },
        });
        let mut reasons = BTreeSet::new();
        validate_zero_work(
            &start,
            &equal,
            &[ZeroWorkCounter::PhysicalRangeReads],
            CancellationWasteAuthority::GenerationBoundSharedBrick,
            &mut reasons,
        );
        assert!(reasons.is_empty());
        validate_zero_work(
            &start,
            &changed,
            &[ZeroWorkCounter::PhysicalRangeReads],
            CancellationWasteAuthority::GenerationBoundSharedBrick,
            &mut reasons,
        );
        assert!(reasons.contains("structural_physical_range_reads_counter_changed"));
    }

    #[test]
    fn missing_metrics_fail_instead_of_becoming_zero() {
        let profile = profile();
        let mut reasons = BTreeSet::new();
        validate_interaction_metrics(&json!({}), &json!({}), &profile, &mut reasons);
        assert!(reasons.contains("interaction_metrics_missing"));

        let mut diagnostics = complete_milestones();
        diagnostics["render"]["performance_milestones"]["target_settled_ms"] = Value::Null;
        reasons.clear();
        validate_settlement_gate(
            &diagnostics,
            SettlementGate::NonresidentTarget,
            &phase_state(),
            &profile,
            &mut reasons,
        );
        assert!(reasons.contains("coordinated_settlement_gate_exceeded"));
        assert!(!reasons.contains("coordinated_settlement_milestone_missing"));

        diagnostics["render"]["performance_milestones"]
            .as_object_mut()
            .unwrap()
            .remove("target_settled_ms");
        reasons.clear();
        validate_settlement_gate(
            &diagnostics,
            SettlementGate::NonresidentTarget,
            &phase_state(),
            &profile,
            &mut reasons,
        );
        assert!(reasons.contains("coordinated_settlement_milestone_missing"));
    }

    #[test]
    fn explicit_absent_current_presentation_is_a_product_failure_not_missing_evidence() {
        let mut diagnostics = json!({
            "render": {
                "display_coordination": {
                    "input_generation": 7,
                    "current_presentation_generation": null,
                },
                "frame_fidelity": {
                    "completeness": "Complete",
                    "display_freshness": "Current",
                    "last_failure_kind": null,
                    "last_capacity_error": null,
                },
            },
        });
        let mut reasons = BTreeSet::new();
        validate_current_complete(&diagnostics, &mut reasons);
        assert_eq!(
            reasons,
            BTreeSet::from(["product_gate_current_presentation_generation_mismatch".to_owned()])
        );

        diagnostics["render"]["display_coordination"]
            .as_object_mut()
            .unwrap()
            .remove("current_presentation_generation");
        reasons.clear();
        validate_current_complete(&diagnostics, &mut reasons);
        assert!(reasons.contains("current_presentation_generation_mismatch_or_missing"));
    }

    #[test]
    fn explicit_absent_scale_is_a_product_failure_but_malformed_scale_is_integrity() {
        let diagnostics = |target: Value, displayed: Value| {
            json!({ "render": { "frame_fidelity": {
                "target_scale_level": target,
                "displayed_scale_level": displayed,
            }}})
        };
        let mut reasons = BTreeSet::new();
        validate_scale(&diagnostics(json!(3), json!(3)), 3, &mut reasons);
        assert!(reasons.is_empty());

        validate_scale(&diagnostics(json!(3), Value::Null), 3, &mut reasons);
        assert_eq!(
            reasons,
            BTreeSet::from(["product_gate_target_or_displayed_scale_mismatch".to_owned()])
        );

        reasons.clear();
        validate_scale(&diagnostics(json!(2), json!(3)), 3, &mut reasons);
        assert_eq!(
            reasons,
            BTreeSet::from(["product_gate_target_or_displayed_scale_mismatch".to_owned()])
        );

        let mut missing = diagnostics(json!(3), json!(3));
        missing["render"]["frame_fidelity"]
            .as_object_mut()
            .unwrap()
            .remove("displayed_scale_level");
        reasons.clear();
        validate_scale(&missing, 3, &mut reasons);
        assert_eq!(
            reasons,
            BTreeSet::from(["target_or_displayed_scale_mismatch_or_missing".to_owned()])
        );

        reasons.clear();
        validate_scale(&diagnostics(Value::Null, json!("3")), 3, &mut reasons);
        assert_eq!(
            reasons,
            BTreeSet::from(["target_or_displayed_scale_mismatch_or_missing".to_owned()])
        );
    }

    #[test]
    fn interaction_gate_uses_only_the_claim_bearing_active_ui_update_metric() {
        let profile = profile();
        let start = json!({
            "render": { "display_coordination": {
                "admitted_generation_latency": timing_ring(0, &[]),
                "active_input_presentation_gap_ns": { "samples": timing_ring(0, &[]) },
                "active_input_main_loop_gap_ns": { "samples": timing_ring(0, &[]) },
                "active_ui_update_duration": {
                    "claim_bearing_2ms_gate": true,
                    "qualification_only_automation_overhead_excluded": true,
                    "qualification_only_automation_commands_excluded": [
                        "sample_diagnostics", "copy_diagnostics", "await_active_view_gpu_timing"
                    ],
                    "subtraction_method": "saturating_subtract_exact_monotonic_elapsed_interval_from_enclosing_ui_callback",
                    "samples": timing_ring(0, &[]),
                },
            }}
        });
        let valid = json!({
            "render": { "display_coordination": {
                "admitted_generation_latency": timing_ring(1, &[1]),
                "active_input_presentation_gap_ns": { "samples": timing_ring(1, &[1]) },
                "active_input_main_loop_gap_ns": { "samples": timing_ring(1, &[1]) },
                "semantic_interaction_task_duration": {
                    "claim_bearing_2ms_gate": false,
                    "samples": { "maximum_ns": 99_000_000 },
                },
                "active_ui_update_duration": {
                    "claim_bearing_2ms_gate": true,
                    "qualification_only_automation_overhead_excluded": true,
                    "qualification_only_automation_commands_excluded": [
                        "sample_diagnostics", "copy_diagnostics", "await_active_view_gpu_timing"
                    ],
                    "subtraction_method": "saturating_subtract_exact_monotonic_elapsed_interval_from_enclosing_ui_callback",
                    "samples": timing_ring(1, &[2_000_000]),
                },
            }}
        });
        let mut reasons = BTreeSet::new();
        validate_interaction_metrics(&start, &valid, &profile, &mut reasons);
        assert!(reasons.is_empty());

        let mut exact_empty_completion_population = valid.clone();
        exact_empty_completion_population["render"]["display_coordination"]["admitted_generation_latency"] =
            timing_ring(0, &[]);
        validate_interaction_metrics(
            &start,
            &exact_empty_completion_population,
            &profile,
            &mut reasons,
        );
        assert!(reasons.contains("resident_input_latency_gate_exceeded"));
        assert!(!reasons.contains("resident_input_latency_metric_missing"));

        let mut exact_empty_active_gap_populations = valid.clone();
        exact_empty_active_gap_populations["render"]["display_coordination"]["active_input_presentation_gap_ns"]
            ["samples"] = timing_ring(0, &[]);
        exact_empty_active_gap_populations["render"]["display_coordination"]["active_input_main_loop_gap_ns"]
            ["samples"] = timing_ring(0, &[]);
        reasons.clear();
        validate_interaction_metrics(
            &start,
            &exact_empty_active_gap_populations,
            &profile,
            &mut reasons,
        );
        assert!(reasons.is_empty());

        let mut semantic_only = valid;
        semantic_only["render"]["display_coordination"]["active_ui_update_duration"] = Value::Null;
        reasons.clear();
        validate_interaction_metrics(&start, &semantic_only, &profile, &mut reasons);
        assert!(reasons.contains("interaction_task_metric_missing"));
        assert!(reasons.contains("ui_update_gate_scope_missing"));
        assert!(reasons.contains("ui_update_qualification_automation_exclusion_contract_missing"));

        let mut missing_exclusion_contract = start.clone();
        missing_exclusion_contract["render"]["display_coordination"]["active_ui_update_duration"]
            ["qualification_only_automation_commands_excluded"] = json!(["sample_diagnostics"]);
        reasons.clear();
        validate_interaction_metrics(&missing_exclusion_contract, &start, &profile, &mut reasons);
        assert!(reasons.contains("ui_update_qualification_automation_exclusion_contract_missing"));
    }

    #[test]
    fn gpu_gate_accepts_only_the_frozen_exact_presented_ticket_and_current_pass() {
        let profile = profile();
        let mut diagnostics = json!({
            "render": {
                "active_render_mode": "Mip",
                "display_coordination": {
                    "input_generation": 7,
                    "detailed_counters": {
                        "per_target_renderer_facts": [{
                            "panel": "3D",
                            "last_execution": {
                                "execution_id": null,
                                "target": 10,
                                "generation": 7,
                                "renderer_frame": 12,
                                "pass_kind": "Volume",
                                "gpu_timing_available": false,
                            },
                        }],
                    },
                },
                "qualification_gpu_timing_checkpoint": {
                    "available": true,
                    "derivation": "identity_frozen_from_current_execution_then_completed_by_exact_presented_interval_ticket",
                    "presented_interval_sequence": 3,
                    "panel": "3D",
                    "execution_id": 5,
                    "target": 9,
                    "display_generation": 7,
                    "renderer_frame": 11,
                    "pass_kind": "Volume",
                    "gpu_batch_envelope_ns": 100,
                    "gpu_payload_copy_ns": null,
                    "gpu_render_pass_ns": 80,
                    "exact_presented_interval_timing_complete": true,
                },
                "progressive_presentation": {
                    "presented_frame_intervals": { "samples": [{
                        "gpu_timing_complete": true,
                        "panel": "3D",
                        "gpu_execution_id": 5,
                        "gpu_target": 9,
                        "gpu_generation": 7,
                        "gpu_renderer_frame": 11,
                        "gpu_pass_kind": "Volume",
                        "gpu_batch_envelope_ns": 100,
                        "gpu_payload_copy_ns": null,
                        "gpu_render_pass_ns": 80,
                    }] },
                },
            },
        });
        let mut reasons = BTreeSet::new();
        validate_gpu_gate(
            &diagnostics,
            GpuGate::Mip,
            ViewerPanel::ThreeD,
            &profile,
            None,
            &mut reasons,
        );
        assert!(reasons.is_empty());

        diagnostics["render"]["progressive_presentation"]["presented_frame_intervals"]["samples"]
            [0]["gpu_execution_id"] = json!(6);
        validate_gpu_gate(
            &diagnostics,
            GpuGate::Mip,
            ViewerPanel::ThreeD,
            &profile,
            None,
            &mut reasons,
        );
        assert!(reasons.contains("presented_interval_gpu_ticket_missing"));

        reasons.clear();
        diagnostics["render"]["display_coordination"]["detailed_counters"]["per_target_renderer_facts"]
            [0]["last_execution"]["generation"] = json!(6);
        validate_gpu_gate(
            &diagnostics,
            GpuGate::Mip,
            ViewerPanel::ThreeD,
            &profile,
            None,
            &mut reasons,
        );
        assert!(reasons.contains("per_target_gpu_execution_fact_missing_for_current_generation"));
    }

    #[test]
    fn gpu_gate_preserves_exact_unavailable_timing_as_a_product_failure() {
        let authority = json!({
            "command_index": 8,
            "batch_id": "NO.batch.001",
            "phase_id": "nonresident_rotation_pan.checkpoint.000",
            "observation_index": 0,
            "gate_id": "NO.acceptance.002.coordinated_presentation_settled",
            "condition": "coordinated_presentation_settled",
            "deadline_authority": "nonresident_target_settlement",
            "deadline_after_origin_ns": 5_000_000_000_u64,
            "outcome": "failed",
            "condition_met": false,
            "timed_out": true,
            "observed_after_origin_ns": 5_000_000_000_u64,
        });
        let expected_authority = authority.clone();
        let mut diagnostics = json!({
            "render": {
                "display_coordination": {
                    "input_generation": 7,
                    "current_presentation_generation": null,
                },
                "qualification_gpu_timing_checkpoint": {
                    "available": false,
                    "derivation": GPU_TIMING_UNAVAILABLE_DERIVATION,
                    "reason": GPU_TIMING_UNAVAILABLE_REASON,
                    "presented_interval_sequence": null,
                    "panel": "XY",
                    "execution_id": null,
                    "target": null,
                    "display_generation": 7,
                    "current_presentation_generation": null,
                    "renderer_frame": null,
                    "pass_kind": "Plane",
                    "gpu_batch_envelope_ns": null,
                    "gpu_payload_copy_ns": null,
                    "gpu_render_pass_ns": null,
                    "identity_frozen_before_completion": false,
                    "exact_presented_interval_timing_complete": false,
                    "unavailable_authority": authority,
                    "waited_ns": 0,
                },
            },
        });
        let mut reasons = BTreeSet::new();
        validate_gpu_gate(
            &diagnostics,
            GpuGate::Plane,
            ViewerPanel::Xy,
            &profile(),
            Some(&expected_authority),
            &mut reasons,
        );
        assert_eq!(
            reasons,
            BTreeSet::from([
                "product_gate_gpu_timing_unavailable_without_expected_current_presentation"
                    .to_owned(),
            ])
        );

        diagnostics["render"]["display_coordination"]["current_presentation_generation"] = json!(7);
        reasons.clear();
        validate_gpu_gate(
            &diagnostics,
            GpuGate::Plane,
            ViewerPanel::Xy,
            &profile(),
            Some(&expected_authority),
            &mut reasons,
        );
        assert_eq!(
            reasons,
            BTreeSet::from(["qualification_gpu_timing_checkpoint_missing_or_invalid".to_owned()])
        );

        diagnostics["render"]["display_coordination"]["current_presentation_generation"] =
            Value::Null;
        diagnostics["render"]["display_coordination"]["input_generation"] = Value::Null;
        diagnostics["render"]["qualification_gpu_timing_checkpoint"]["display_generation"] =
            Value::Null;
        reasons.clear();
        validate_gpu_gate(
            &diagnostics,
            GpuGate::Plane,
            ViewerPanel::Xy,
            &profile(),
            Some(&expected_authority),
            &mut reasons,
        );
        assert_eq!(
            reasons,
            BTreeSet::from(["qualification_gpu_timing_checkpoint_missing_or_invalid".to_owned()])
        );

        diagnostics["render"]["display_coordination"]["input_generation"] = json!(7);
        diagnostics["render"]["qualification_gpu_timing_checkpoint"]["display_generation"] =
            json!(7);
        reasons.clear();
        validate_gpu_gate(
            &diagnostics,
            GpuGate::Plane,
            ViewerPanel::Xy,
            &profile(),
            None,
            &mut reasons,
        );
        assert_eq!(
            reasons,
            BTreeSet::from(["qualification_gpu_timing_checkpoint_missing_or_invalid".to_owned()])
        );

        diagnostics["render"]["qualification_gpu_timing_checkpoint"]["unavailable_authority"]["gate_id"] =
            json!("NO.acceptance.999.unlinked");
        reasons.clear();
        validate_gpu_gate(
            &diagnostics,
            GpuGate::Plane,
            ViewerPanel::Xy,
            &profile(),
            Some(&expected_authority),
            &mut reasons,
        );
        assert_eq!(
            reasons,
            BTreeSet::from(["qualification_gpu_timing_checkpoint_missing_or_invalid".to_owned()])
        );
    }

    #[test]
    fn gpu_timing_await_is_exactly_before_every_gpu_gated_phase_endpoint() {
        let commands = vec![
            json!({ "command": "sample_diagnostics", "label": "start" }),
            json!({ "command": "camera_zoom_sequence", "samples": 3 }),
            json!({
                "command": "await_active_view_gpu_timing",
                "target": "xy",
                "pass_kind": "volume",
                "timeout_ms": GPU_TIMING_AWAIT_TIMEOUT_MS,
            }),
            json!({ "command": "sample_diagnostics", "label": "end" }),
            json!({ "command": "quit" }),
        ];
        let scenario = ScriptScenario {
            id: "RZ".to_owned(),
            phases: vec![ScriptPhase {
                name: "resident_3d_zoom".to_owned(),
                start_diagnostic_label: Some("start".to_owned()),
                end_diagnostic_label: "end".to_owned(),
            }],
            instrumented_script: template(true, commands),
            instrumentation_control_script: None,
            cleanup: AttemptCleanup::default(),
        };
        let oracle = OracleScenario {
            id: "RZ".to_owned(),
            phases: vec![resident_oracle_phase()],
        };
        validate_gpu_timing_await_schedule(&scenario, &scenario.instrumented_script, &oracle)
            .unwrap();

        let mut late = scenario.instrumented_script.clone();
        late.commands.swap(2, 3);
        assert!(validate_gpu_timing_await_schedule(&scenario, &late, &oracle).is_err());

        let mut missing = scenario.instrumented_script.clone();
        missing.commands.remove(2);
        assert!(validate_gpu_timing_await_schedule(&scenario, &missing, &oracle).is_err());

        let mut wrong_pass = scenario.instrumented_script.clone();
        wrong_pass.commands[2]["pass_kind"] = json!("plane");
        assert!(validate_gpu_timing_await_schedule(&scenario, &wrong_pass, &oracle).is_err());
    }

    #[test]
    fn phase_timing_samples_slice_exact_population_and_reject_overwrite() {
        let ring = |total: u64, retained: &[u64], capacity: usize| {
            json!({
                "capacity": capacity,
                "total_count": total,
                "retained_count": retained.len(),
                "overwritten_count": total.saturating_sub(retained.len() as u64),
                "maximum_ns": retained.iter().copied().max().unwrap_or_default(),
                "p95_ns": null,
                "retained_samples_ns_oldest_first": retained,
            })
        };
        let start = json!({ "metric": ring(3, &[10, 20, 30], 4) });
        let end = json!({ "metric": ring(5, &[20, 30, 40, 50], 4) });
        let mut reasons = BTreeSet::new();
        assert_eq!(
            phase_timing_samples(&start, &end, "/metric", "metric", &mut reasons),
            Some(vec![40, 50])
        );
        assert!(reasons.is_empty());
        assert_eq!(sample_p95(&[40, 50]), Some(50));

        let overwritten = json!({ "metric": ring(8, &[50, 60, 70, 80], 4) });
        assert_eq!(
            phase_timing_samples(&start, &overwritten, "/metric", "metric", &mut reasons,),
            None
        );
        assert!(reasons.contains("metric_phase_samples_overwritten"));
    }

    fn complete_milestones() -> Value {
        let panel = |label: &str| {
            json!({
                "panel": label,
                "first_current_presented_ms": 1.0,
                "first_useful_frame_ms": 1.0,
                "complete_coarse_ms": 1.0,
                "complete_replacement_ms": 1.0,
                "target_settled_ms": 1.0,
                "visible_layer_overflow": false,
                "visible_layers": [{
                    "layer_ordinal": 0,
                    "first_current_presented_ms": 1.0,
                    "first_useful_frame_ms": 1.0,
                    "complete_coarse_ms": 1.0,
                    "complete_replacement_ms": 1.0,
                    "target_settled_ms": 1.0,
                }],
            })
        };
        json!({
            "render": {
                "display_coordination": { "input_generation": 7 },
                "performance_milestones": {
                    "scope": "coordinated_visible_layout",
                    "input_generation": 7,
                    "first_current_presented_ms": 1.0,
                    "first_useful_frame_ms": 1.0,
                    "complete_coarse_ms": 1.0,
                    "complete_replacement_ms": 1.0,
                    "target_settled_ms": 1.0,
                    "visible_panels": [panel("3D"), panel("XY"), panel("XZ"), panel("YZ")],
                },
            },
        })
    }

    #[test]
    fn settlement_gate_checks_coordinated_panel_and_visible_layer_milestones() {
        let profile = profile();
        let state = phase_state();
        let mut diagnostics = complete_milestones();
        let mut reasons = BTreeSet::new();
        validate_settlement_gate(
            &diagnostics,
            SettlementGate::ColdTarget,
            &state,
            &profile,
            &mut reasons,
        );
        assert!(reasons.is_empty());

        diagnostics["render"]["performance_milestones"]["visible_panels"]
            .as_array_mut()
            .unwrap()
            .pop();
        validate_settlement_gate(
            &diagnostics,
            SettlementGate::ColdTarget,
            &state,
            &profile,
            &mut reasons,
        );
        assert!(reasons.contains("product_gate_visible_panel_milestone_set_mismatch"));

        let mut diagnostics = complete_milestones();
        diagnostics["render"]["performance_milestones"]["target_settled_ms"] = Value::Null;
        reasons.clear();
        validate_settlement_gate(
            &diagnostics,
            SettlementGate::NonresidentTarget,
            &state,
            &profile,
            &mut reasons,
        );
        assert!(reasons.contains("coordinated_settlement_gate_exceeded"));
        assert!(!reasons.contains("coordinated_settlement_milestone_missing"));

        let mut diagnostics = complete_milestones();
        diagnostics["render"]["performance_milestones"]["visible_panels"][0]["target_settled_ms"] =
            Value::Null;
        reasons.clear();
        validate_settlement_gate(
            &diagnostics,
            SettlementGate::NonresidentTarget,
            &state,
            &profile,
            &mut reasons,
        );
        assert!(reasons.contains("visible_panel_settlement_gate_exceeded"));
        assert!(!reasons.contains("visible_panel_settlement_milestone_missing"));

        let mut diagnostics = complete_milestones();
        diagnostics["render"]["performance_milestones"]["visible_panels"][0]["visible_layers"][0]
            ["target_settled_ms"] = Value::Null;
        reasons.clear();
        validate_settlement_gate(
            &diagnostics,
            SettlementGate::NonresidentTarget,
            &state,
            &profile,
            &mut reasons,
        );
        assert!(reasons.contains("visible_layer_settlement_gate_exceeded"));
        assert!(!reasons.contains("visible_layer_settlement_milestone_missing"));

        diagnostics["render"]["performance_milestones"]["visible_panels"][0]["visible_layers"][0]
            .as_object_mut()
            .unwrap()
            .remove("target_settled_ms");
        reasons.clear();
        validate_settlement_gate(
            &diagnostics,
            SettlementGate::NonresidentTarget,
            &state,
            &profile,
            &mut reasons,
        );
        assert!(reasons.contains("visible_layer_settlement_milestone_missing"));
        assert!(!reasons.contains("visible_layer_settlement_gate_exceeded"));

        let mut malformed = complete_milestones();
        malformed["render"]["performance_milestones"]["target_settled_ms"] = json!(-1.0);
        reasons.clear();
        validate_settlement_gate(
            &malformed,
            SettlementGate::NonresidentTarget,
            &state,
            &profile,
            &mut reasons,
        );
        assert!(reasons.contains("coordinated_settlement_milestone_missing"));
        assert!(!reasons.contains("coordinated_settlement_gate_exceeded"));
    }

    fn canonical_state_diagnostics(state: &PhaseStateBinding) -> Value {
        let planes = state
            .cross_section
            .planes
            .iter()
            .map(|plane| {
                json!({
                    "panel_id": plane.panel.report_label(),
                    "canonical_plane_geometry": {
                        "source": "canonical_linked_cross_section_view",
                        "plane_origin_world": plane.plane_origin_world,
                        "u_axis_world": plane.u_axis_world,
                        "v_axis_world": plane.v_axis_world,
                        "normal_away_world": plane.normal_away_world,
                        "world_per_screen_point": plane.world_per_screen_point,
                    },
                })
            })
            .collect::<Vec<_>>();
        json!({
            "dataset": { "current_time_index": state.time_index },
            "render": { "projection": state.camera.projection.report_label() },
            "camera": {
                "projection": state.camera.projection.report_label(),
                "canonical_source": "ApplicationSnapshot_ViewState_camera",
                "target_world": state.camera.target_world,
                "orientation_xyzw": state.camera.orientation_xyzw,
                "orthographic_world_per_screen_point": state.camera.orthographic_world_per_screen_point,
                "perspective_focal_length_screen_points": state.camera.perspective_focal_length_screen_points,
                "perspective_view_distance_world": state.camera.perspective_view_distance_world,
                "viewport": {
                    "width": state.render_extent.width,
                    "height": state.render_extent.height,
                },
            },
            "cross_section": {
                "schema": "mirante4d-cross-section-panel-diagnostics",
                "schema_version": 1,
                "layout": state.layout.report_label(),
                "active_panel": state.active_view.report_label(),
                "canonical_linked_view": {
                    "source": "ApplicationSnapshot_ViewState_cross_section",
                    "center_world": state.cross_section.center_world,
                    "orientation_xyzw": state.cross_section.orientation_xyzw,
                    "world_per_screen_point": state.cross_section.world_per_screen_point,
                    "depth_world": state.cross_section.depth_world,
                },
                "panels": planes,
            },
        })
    }

    #[test]
    fn canonical_camera_and_cross_section_geometry_are_tolerance_bound_and_fail_closed() {
        let state = phase_state();
        let mut diagnostics = canonical_state_diagnostics(&state);
        let mut reasons = BTreeSet::new();
        validate_phase_state_facts(&diagnostics, &state, &numerical_contract(), &mut reasons);
        assert!(reasons.is_empty());

        diagnostics["dataset"]["current_time_index"] = json!(1);
        diagnostics["camera"]["target_world"][0] = json!(0.01);
        diagnostics["cross_section"]["panels"][0]["canonical_plane_geometry"]["source"] =
            json!("derived_copy");
        validate_phase_state_facts(&diagnostics, &state, &numerical_contract(), &mut reasons);
        assert!(reasons.contains("product_gate_phase_time_index_mismatch"));
        assert!(reasons.contains("product_gate_canonical_camera_geometry_outside_contract"));
        assert!(reasons.contains("canonical_xy_plane_geometry_missing_or_outside_contract"));
    }

    #[test]
    fn phase_state_is_cross_bound_to_script_and_final_mapped_client_evidence() {
        let state = phase_state();
        let mut template = template(
            true,
            vec![
                json!({ "command": "set_mapped_client_pixels", "width": 1280, "height": 720 }),
                json!({ "command": "set_render_target_size", "width": 1280, "height": 720 }),
                json!({ "command": "set_viewer_layout", "layout": "four_panel" }),
                json!({ "command": "set_time_index", "time_index": 0 }),
                json!({ "command": "set_projection", "projection": "orthographic" }),
                json!({ "command": "set_active_cross_section_panel", "panel": "xy" }),
                json!({ "command": "sample_diagnostics", "label": "end" }),
                json!({ "command": "quit" }),
            ],
        );
        let phase = ScriptPhase {
            name: "resident_cross_section_zoom".to_owned(),
            start_diagnostic_label: None,
            end_diagnostic_label: "end".to_owned(),
        };
        let report = json!({ "viewport_evidence": { "requested_mapped_client_pixels": {
            "width": 1280,
            "height": 720,
        }}});
        let mut reasons = BTreeSet::new();
        validate_phase_script_binding(&report, &template, &phase, &state, &mut reasons);
        assert!(reasons.is_empty());

        let mut canonical_zero_default = template.clone();
        canonical_zero_default.commands.remove(3);
        validate_phase_script_binding(
            &report,
            &canonical_zero_default,
            &phase,
            &state,
            &mut reasons,
        );
        assert!(reasons.is_empty());

        template.commands[3]["time_index"] = json!(1);
        validate_phase_script_binding(&report, &template, &phase, &state, &mut reasons);
        assert!(reasons.contains("phase_time_index_script_binding_mismatch"));
    }

    #[test]
    fn cold_phase_state_treats_startup_bootstrap_as_the_command_prefix() {
        let state = phase_state();
        let mut template = template(
            true,
            vec![
                json!({ "command": "sample_diagnostics", "label": "end" }),
                json!({ "command": "quit" }),
            ],
        );
        template.startup_bootstrap = Some(AutomationStartupBootstrap {
            capture_start_checkpoint: true,
            start_diagnostic_label: Some("start".to_owned()),
            commands: vec![
                json!({ "command": "set_mapped_client_pixels", "width": 1280, "height": 720 }),
                json!({
                    "command": "set_four_panel_viewports",
                    "presentation_width_points": 317.0,
                    "presentation_height_points": 287.5,
                    "three_d_render_width": 1280,
                    "three_d_render_height": 720,
                    "linked_render_width": 317,
                    "linked_render_height": 288,
                }),
                json!({ "command": "set_viewer_layout", "layout": "four_panel" }),
                json!({ "command": "set_time_index", "time_index": 0 }),
                json!({ "command": "set_projection", "projection": "orthographic" }),
                json!({ "command": "set_active_cross_section_panel", "panel": "xy" }),
            ],
        });
        let phase = ScriptPhase {
            name: "blocking_target_settled".to_owned(),
            start_diagnostic_label: Some("start".to_owned()),
            end_diagnostic_label: "end".to_owned(),
        };
        let report = json!({ "viewport_evidence": { "requested_mapped_client_pixels": {
            "width": 1280,
            "height": 720,
        }}});
        let mut reasons = BTreeSet::new();
        validate_phase_script_binding(&report, &template, &phase, &state, &mut reasons);
        assert!(reasons.is_empty());
    }

    #[test]
    fn setup_only_bootstrap_has_no_checkpoint_and_proves_one_zero_payload_commit() {
        let bootstrap = AutomationStartupBootstrap {
            capture_start_checkpoint: false,
            start_diagnostic_label: None,
            commands: vec![json!({
                "command": "set_camera_view",
                "projection": "orthographic",
                "target_world": [1.0, 2.0, 3.0],
                "orientation_xyzw": [0.0, 0.0, 0.0, 1.0],
                "orthographic_world_per_screen_point": 1.0,
                "perspective_focal_length_screen_points": 1.0,
                "perspective_view_distance_world": 1.0,
            })],
        };
        validate_startup_bootstrap(&bootstrap).unwrap();
        let mut template = template(false, vec![json!({ "command": "quit" })]);
        template.startup_bootstrap = Some(bootstrap);
        let report = json!({
            "startup_bootstrap": {
                "qualification_only": true,
                "payload_requests_submitted": false,
                "intermediate_view_reconciliations": 0,
                "canonical_commit_reconciliations": 1,
                "observed_work": {
                    "before": {
                        "runtime_submitted_requests": 0,
                        "runtime_started_decodes": 0,
                        "source_physical_range_reads": 0,
                        "source_codec_decodes": 0,
                        "gpu_uploaded_resources": 0,
                        "gpu_uploaded_payload_bytes": 0,
                        "gpu_queue_submissions": 0,
                        "gpu_frames_executed": 0,
                        "demand_jobs_submitted": 0,
                        "demand_jobs_completed": 0,
                    },
                    "after": {
                        "runtime_submitted_requests": 0,
                        "runtime_started_decodes": 0,
                        "source_physical_range_reads": 0,
                        "source_codec_decodes": 0,
                        "gpu_uploaded_resources": 0,
                        "gpu_uploaded_payload_bytes": 0,
                        "gpu_queue_submissions": 0,
                        "gpu_frames_executed": 0,
                        "demand_jobs_submitted": 0,
                        "demand_jobs_completed": 0,
                    },
                    "delta": {
                        "runtime_submitted_requests": 0,
                        "runtime_started_decodes": 0,
                        "source_physical_range_reads": 0,
                        "source_codec_decodes": 0,
                        "gpu_uploaded_resources": 0,
                        "gpu_uploaded_payload_bytes": 0,
                        "gpu_queue_submissions": 0,
                        "gpu_frames_executed": 0,
                        "demand_jobs_submitted": 0,
                        "demand_jobs_completed": 0,
                    },
                    "zero_payload_or_demand_work": true,
                    "counter_scope": "runtime_source_renderer_and_demand_planner_monotonic_counters",
                },
                "duration_ns": 1,
                "commands": [{ "command": "set_camera_view", "details": {} }],
                "capture_start_checkpoint": false,
                "start_diagnostic_label": null,
                "start_checkpoint_captured_in_diagnostics": false,
            },
            "diagnostics": [],
        });
        let mut reasons = BTreeSet::new();
        validate_startup_bootstrap_report(&report, &template, &mut reasons);
        assert!(reasons.is_empty());
    }

    #[test]
    fn open_handle_gate_requires_the_flat_peak_and_exact_gauge_contract() {
        let profile = profile();
        let mut report = json!({
            "final_diagnostics": { "dataset_source_io": { "reader": {
                "peak_open_object_handles": 8,
                "open_object_handle_gauge": {
                    "available": true,
                    "scope": "active_reader_root_cached_and_transient_object_descriptors",
                    "current": 4,
                    "peak": 8,
                    "retained_cache_current": 3,
                    "retained_cache_peak": 6,
                    "operation_counts_used_as_concurrency": false,
                },
            }}},
        });
        let mut reasons = BTreeSet::new();
        validate_resource_policy(&report, &profile, &mut reasons);
        assert!(!reasons.contains("open_object_peak_missing"));
        assert!(!reasons.contains("open_object_handle_gauge_contract_missing_or_mismatched"));
        assert!(!reasons.contains("open_object_handle_gauge_values_missing_or_incoherent"));

        report["final_diagnostics"]["dataset_source_io"]["reader"]["open_object_handle_gauge"]["operation_counts_used_as_concurrency"] =
            json!(true);
        report["final_diagnostics"]["dataset_source_io"]["reader"]["open_object_handle_gauge"]["peak"] =
            json!(7);
        validate_resource_policy(&report, &profile, &mut reasons);
        assert!(reasons.contains("open_object_handle_gauge_contract_missing_or_mismatched"));
        assert!(reasons.contains("open_object_handle_gauge_values_missing_or_incoherent"));
    }

    fn structural_ceilings() -> StructuralCeilings {
        StructuralCeilings {
            durable_gesture_commits_per_sequence_exact: 1,
            pending_display_batches_peak_maximum: 2,
            in_flight_display_batches_peak_maximum: 2,
            command_encoders_delta_maximum: 2,
            color_passes_delta_maximum: 2,
            renderer_submissions_delta_maximum: 2,
            completion_notifications_delta_maximum: 2,
            backpressure_deferrals_delta_maximum: 2,
            encoded_display_batches_delta_maximum: 2,
            encoded_but_dropped_delta_maximum: 2,
            sealed_obsolete_submitted_delta_maximum: 2,
            stale_presentations_delta_maximum: 2,
            current_presentations_delta_maximum: 2,
            demand_work_delta_maximum: 2,
            cancellation_waste_count_delta_maximum: 2,
            cancellation_waste_encoded_bytes_delta_maximum: 2,
            cancellation_waste_decoded_bytes_delta_maximum: 2,
            cancellation_waste_uploaded_bytes_delta_maximum: 2,
            cancellation_waste_cpu_time_ns_delta_maximum: 2,
        }
    }

    fn structural_diagnostics(value: u64) -> Value {
        json!({
            "render": {
                "progressive_presentation": { "stale_frames_rejected": value },
                "display_coordination": { "detailed_counters": {
                    "pending_display_batches_peak": value,
                    "color_passes": value,
                    "completion_notifications": value,
                    "encoded_display_batches": value,
                    "encoded_but_dropped_batches": value,
                    "sealed_obsolete_submitted_batches": value,
                    "current_presentations": value,
                    "per_target_renderer_facts": [{
                        "command_buffers": value,
                        "queue_submissions": value,
                        "backpressure_deferrals": value,
                    }],
                    "staging_3d_renderer_facts": {
                        "purpose": "hidden_staging_3d_fallback_target",
                        "command_buffers": 0,
                        "queue_submissions": 0,
                        "backpressure_deferrals": 0,
                    },
                }},
            },
            "gpu_adapter": {
                "peak_in_flight_submissions": value,
                "uploads": { "cancelled_payload_bytes": value },
            },
            "dataset_demand": { "planned_scope_accounting": { "demand_work": value } },
            "dataset_runtime": {
                "counters": { "cancelled_requests": value },
                "performance": {
                    "cancelled_decode_executions": value,
                    "cancelled_decode_bytes": value,
                    "cancelled_decode_time_ns": value,
                },
            },
            "dataset_source_io": { "reader": { "cancelled_encoded_bytes": value } },
        })
    }

    #[test]
    fn structural_ceilings_use_per_target_sums_and_name_missing_facts() {
        let start = structural_diagnostics(0);
        let mut end = structural_diagnostics(1);
        let ceilings = structural_ceilings();
        let mut reasons = BTreeSet::new();
        validate_structural_ceilings(
            &start,
            &end,
            DisplayBatchAuthority::CoordinatedDisplayBatch,
            CancellationWasteAuthority::GenerationBoundSharedBrick,
            &ceilings,
            &mut reasons,
        );
        assert!(reasons.is_empty());

        end["render"]["display_coordination"]["detailed_counters"]["completion_notifications"] =
            Value::Null;
        validate_structural_ceilings(
            &start,
            &end,
            DisplayBatchAuthority::CoordinatedDisplayBatch,
            CancellationWasteAuthority::GenerationBoundSharedBrick,
            &ceilings,
            &mut reasons,
        );
        assert!(reasons.contains("structural_completion_notifications_fact_missing"));

        let mut start_without_staging = structural_diagnostics(0);
        let mut end_without_staging = structural_diagnostics(1);
        start_without_staging["render"]["display_coordination"]["detailed_counters"]["staging_3d_renderer_facts"] =
            Value::Null;
        end_without_staging["render"]["display_coordination"]["detailed_counters"]["staging_3d_renderer_facts"] =
            Value::Null;
        reasons.clear();
        validate_structural_ceilings(
            &start_without_staging,
            &end_without_staging,
            DisplayBatchAuthority::CoordinatedDisplayBatch,
            CancellationWasteAuthority::GenerationBoundSharedBrick,
            &ceilings,
            &mut reasons,
        );
        assert!(reasons.is_empty());

        let mut explicit_cold_start = structural_diagnostics(0);
        explicit_cold_start["render"]["display_coordination"]["detailed_counters"]["per_target_renderer_facts"] =
            json!([]);
        explicit_cold_start["render"]["display_coordination"]["detailed_counters"]["staging_3d_renderer_facts"] =
            Value::Null;
        reasons.clear();
        validate_structural_ceilings(
            &explicit_cold_start,
            &end_without_staging,
            DisplayBatchAuthority::CoordinatedDisplayBatch,
            CancellationWasteAuthority::GenerationBoundSharedBrick,
            &ceilings,
            &mut reasons,
        );
        assert!(
            reasons.is_empty(),
            "an explicit zero-target cold baseline contributes zero renderer work"
        );

        end_without_staging["render"]["display_coordination"]["detailed_counters"]
            .as_object_mut()
            .unwrap()
            .remove("staging_3d_renderer_facts");
        validate_structural_ceilings(
            &start_without_staging,
            &end_without_staging,
            DisplayBatchAuthority::CoordinatedDisplayBatch,
            CancellationWasteAuthority::GenerationBoundSharedBrick,
            &ceilings,
            &mut reasons,
        );
        assert_eq!(
            reasons,
            BTreeSet::from([
                "structural_backpressure_deferrals_fact_missing".to_owned(),
                "structural_command_encoders_fact_missing".to_owned(),
                "structural_renderer_submissions_fact_missing".to_owned(),
            ])
        );
    }

    #[test]
    fn predecessor_cancellation_gaps_require_exact_unavailable_authority_facts() {
        let diagnostics = json!({
            "dataset_source_io": { "reader": { "cancelled_encoded_bytes": {
                "available": false,
                "reason": "physical_range_cohort_has_no_per_sink_cancellation_ownership",
            }}},
            "gpu_adapter": { "uploads": { "cancelled_payload_bytes": {
                "available": false,
                "reason": "renderer_uploads_have_no_sealed_generation_cancellation_outcome",
            }}},
        });
        let counters = [
            ZeroWorkCounter::CancellationWasteEncodedBytes,
            ZeroWorkCounter::CancellationWasteUploadedBytes,
        ];
        let mut reasons = BTreeSet::new();
        validate_zero_work(
            &diagnostics,
            &diagnostics,
            &counters,
            CancellationWasteAuthority::PredecessorUnattributed,
            &mut reasons,
        );
        assert!(reasons.is_empty());

        let mut wrong = diagnostics;
        wrong["gpu_adapter"]["uploads"]["cancelled_payload_bytes"]["reason"] = json!("unknown");
        validate_zero_work(
            &wrong,
            &wrong,
            &counters,
            CancellationWasteAuthority::PredecessorUnattributed,
            &mut reasons,
        );
        assert!(reasons.contains(
            "structural_cancellation_waste_uploaded_bytes_predecessor_authority_fact_missing"
        ));
    }

    #[test]
    fn unique_work_reconciles_exact_union_and_bounded_authority_deltas() {
        let diagnostic_counters = |value| {
            json!({
                "dataset_source_io": { "reader": {
                    "physical_range_read_operations": value,
                    "physical_encoded_bytes_read": value,
                    "codec_decode_operations": value,
                    "codec_decoded_bytes": value,
                }},
                "dataset_runtime": {
                    "counters": {
                        "submitted_requests": value,
                        "started_decodes": value,
                    },
                    "performance": { "decoded_output_bytes": value },
                },
                "gpu_adapter": { "uploads": {
                    "resources": value,
                    "payload_bytes": value,
                }, "control": {
                    "dynamic_updates": value,
                    "dynamic_upload_bytes": value,
                    "publication_writes": value,
                }},
            })
        };
        let start = json!({
            "resource_accounting": { "exact_cross_scope_union": {
                "available": true,
                "label": "start",
                "canonical_entries_sha256": "11".repeat(32),
                "unique_keys": 0,
                "unique_payload_bytes": 0,
                "summed_scope_payload_bytes": 0,
                "delta_from_previous_label": null,
                "raw_keys_serialized": false,
                "derivation": "DatasetCatalog_resource_payload_descriptor_for_sorted_deduplicated_visible_prepared_scope_keys",
                "canonical_entries_sha256_derivation": "sha256_domain_mirante4d_ep00_resource_union_v1_sorted_binary_le",
            }},
            "diagnostics": diagnostic_counters(10),
        });
        let mut end = json!({
            "resource_accounting": { "exact_cross_scope_union": {
                "available": true,
                "label": "end",
                "canonical_entries_sha256": "22".repeat(32),
                "unique_keys": 1,
                "unique_payload_bytes": 1,
                "summed_scope_payload_bytes": 1,
                "delta_from_previous_label": {
                    "previous_label": "start",
                    "previous_union_sha256": "11".repeat(32),
                    "current_label": "end",
                    "current_union_sha256": "22".repeat(32),
                    "partition_derivation": "sorted_DatasetResourceKey_payload_descriptor_three_way_merge",
                    "partitions_pairwise_disjoint": true,
                    "retained_payload_bytes_match": true,
                    "retained_entries_sha256": "11".repeat(32),
                    "retained_unique_keys": 0,
                    "retained_unique_payload_bytes": 0,
                    "added_entries_sha256": "22".repeat(32),
                    "added_unique_keys": 1,
                    "added_unique_payload_bytes": 1,
                    "removed_entries_sha256": "11".repeat(32),
                    "removed_unique_keys": 0,
                    "removed_unique_payload_bytes": 0,
                },
                "raw_keys_serialized": false,
                "derivation": "DatasetCatalog_resource_payload_descriptor_for_sorted_deduplicated_visible_prepared_scope_keys",
                "canonical_entries_sha256_derivation": "sha256_domain_mirante4d_ep00_resource_union_v1_sorted_binary_le",
            }},
            "diagnostics": diagnostic_counters(11),
        });
        let expected = unique_work_expectation(1);
        let mut reasons = BTreeSet::new();
        validate_unique_work(&start, &end, "start", "end", None, &expected, &mut reasons);
        assert!(reasons.is_empty());

        let mut different_oracle_unions = expected.clone();
        different_oracle_unions.start_union.canonical_entries_sha256 = "33".repeat(32);
        different_oracle_unions
            .target_union
            .canonical_entries_sha256 = "44".repeat(32);
        reasons.clear();
        validate_unique_work(
            &start,
            &end,
            "start",
            "end",
            None,
            &different_oracle_unions,
            &mut reasons,
        );
        assert!(reasons.contains(
            "product_gate_exact_cross_scope_start_union_canonical_entries_sha256_mismatch"
        ));
        assert!(reasons.contains(
            "product_gate_exact_cross_scope_target_union_canonical_entries_sha256_mismatch"
        ));
        assert!(
            !reasons
                .contains("exact_resource_union_delta_previous_union_sha256_mismatch_or_missing")
        );
        assert!(
            !reasons
                .contains("exact_resource_union_delta_current_union_sha256_mismatch_or_missing")
        );

        let mut misbound_delta = end.clone();
        misbound_delta["resource_accounting"]["exact_cross_scope_union"]["delta_from_previous_label"]
            ["current_union_sha256"] = json!("55".repeat(32));
        reasons.clear();
        validate_unique_work(
            &start,
            &misbound_delta,
            "start",
            "end",
            None,
            &expected,
            &mut reasons,
        );
        assert!(
            reasons.contains("exact_resource_union_delta_current_union_sha256_mismatch_or_missing")
        );

        let mut malformed_start = start.clone();
        malformed_start["resource_accounting"]["exact_cross_scope_union"]["canonical_entries_sha256"] =
            json!("not-a-sha256");
        let mut malformed_bound_end = end.clone();
        malformed_bound_end["resource_accounting"]["exact_cross_scope_union"]["delta_from_previous_label"]
            ["previous_union_sha256"] = json!("not-a-sha256");
        reasons.clear();
        validate_unique_work(
            &malformed_start,
            &malformed_bound_end,
            "start",
            "end",
            None,
            &expected,
            &mut reasons,
        );
        assert!(reasons.contains(
            "exact_cross_scope_start_union_canonical_entries_sha256_mismatch_or_missing"
        ));
        assert!(
            reasons
                .contains("exact_resource_union_delta_previous_union_sha256_mismatch_or_missing")
        );

        end["diagnostics"]["dataset_source_io"]["reader"]["physical_encoded_bytes_read"] =
            json!(12);
        reasons.clear();
        validate_unique_work(&start, &end, "start", "end", None, &expected, &mut reasons);
        assert!(reasons.contains("unique_work_physical_encoded_bytes_read_delta_outside_oracle"));

        let mut incoherent = unique_work_expectation(1);
        incoherent.delta_union.removed_unique_keys = 1;
        assert!(validate_unique_work_expectation(&incoherent).is_err());
        let mut unauthorised_range = unique_work_expectation(1);
        unauthorised_range.physical_encoded_bytes_read.maximum = 2;
        assert!(validate_unique_work_expectation(&unauthorised_range).is_err());
    }

    #[test]
    fn nonresident_target_is_partitioned_against_the_exact_phase_start_residency() {
        let demand = unique_work_expectation(1);
        let expected = PhaseStartTargetResidencyExpectation {
            resident_target_intersection: ExactResourcePartition {
                canonical_entries_sha256: "cc".repeat(32),
                unique_keys: 0,
                unique_payload_bytes: 0,
            },
            nonresident_target_difference: ExactResourcePartition {
                canonical_entries_sha256: "dd".repeat(32),
                unique_keys: 1,
                unique_payload_bytes: 1,
            },
        };
        validate_phase_start_target_residency(&expected, &demand.target_union).unwrap();
        let start = json!({
            "resource_accounting": { "exact_gpu_resident_union": {
                "available": true,
                "label": "start",
                "canonical_entries_sha256": "aa".repeat(32),
                "canonical_entries_sha256_derivation": "sha256_domain_mirante4d_ep00_resource_union_v1_sorted_binary_le",
                "unique_keys": 0,
                "unique_payload_bytes": 0,
                "raw_keys_serialized": false,
            }},
        });
        let mut end = json!({
            "resource_accounting": {
                "exact_cross_scope_union": {
                    "canonical_entries_sha256": "22".repeat(32),
                },
                "target_residency_at_phase_start": {
                    "available": true,
                    "phase_start_label": "start",
                    "phase_start_resident_union_sha256": "aa".repeat(32),
                    "target_union_sha256": "22".repeat(32),
                    "resident_target_intersection": {
                        "canonical_entries_sha256": "cc".repeat(32),
                        "unique_keys": 0,
                        "unique_payload_bytes": 0,
                    },
                    "nonresident_target_difference": {
                        "canonical_entries_sha256": "dd".repeat(32),
                        "unique_keys": 1,
                        "unique_payload_bytes": 1,
                    },
                    "partitions_pairwise_disjoint": true,
                    "target_union_reconciles": true,
                    "derivation": "sorted_target_union_partition_by_phase_start_gpu_residency",
                },
            },
        });
        let mut reasons = BTreeSet::new();
        validate_nonresident_target_residency(&start, &end, "start", &expected, &mut reasons);
        assert!(reasons.is_empty());

        end["resource_accounting"]["exact_cross_scope_union"]["canonical_entries_sha256"] =
            json!("44".repeat(32));
        end["resource_accounting"]["target_residency_at_phase_start"]["target_union_sha256"] =
            json!("44".repeat(32));
        end["resource_accounting"]["target_residency_at_phase_start"]["resident_target_intersection"]
            ["canonical_entries_sha256"] = json!("ee".repeat(32));
        end["resource_accounting"]["target_residency_at_phase_start"]["nonresident_target_difference"]
            ["canonical_entries_sha256"] = json!("ff".repeat(32));
        reasons.clear();
        validate_nonresident_target_residency(&start, &end, "start", &expected, &mut reasons);
        assert!(!reasons.contains("nonresident_target_phase_start_residency_authority_missing"));
        assert!(
            reasons
                .contains("product_gate_nonresident_target_resident_target_intersection_mismatch")
        );
        assert!(
            reasons
                .contains("product_gate_nonresident_target_nonresident_target_difference_mismatch")
        );
        assert_eq!(evidence_status(&reasons), "valid_complete");

        end["resource_accounting"]["target_residency_at_phase_start"]["target_union_sha256"] =
            json!("55".repeat(32));
        reasons.clear();
        validate_nonresident_target_residency(&start, &end, "start", &expected, &mut reasons);
        assert!(reasons.contains("nonresident_target_phase_start_residency_authority_missing"));

        end["resource_accounting"]["target_residency_at_phase_start"]["target_union_sha256"] =
            json!("44".repeat(32));
        end["resource_accounting"]["target_residency_at_phase_start"]["phase_start_resident_union_sha256"] =
            json!("bb".repeat(32));
        reasons.clear();
        validate_nonresident_target_residency(&start, &end, "start", &expected, &mut reasons);
        assert!(reasons.contains("nonresident_target_phase_start_residency_authority_missing"));
    }

    #[test]
    fn verification_active_gate_requires_presence_not_progress() {
        let checkpoint = |state: &str, active, progress| {
            json!({ "diagnostics": { "source_verification": {
                "state": state,
                "active_operation": active,
                "service": {
                    "started_runs": 2,
                    "accepted_progress_updates": progress,
                    "cancelled_runs": 1,
                    "failed_runs": 0,
                    "accepted_successes": 0,
                    "completed_reader_runs": 0,
                },
            }}})
        };
        let gate = VerificationGate {
            kind: VerificationGateKind::ActiveThroughout,
            start: VerificationCheckpointExpectation {
                state: ExpectedSourceVerificationState::Verifying,
                active_operation: true,
                started_runs: 2,
                cancelled_runs: 1,
                failed_runs: 0,
                accepted_successes: 0,
                completed_reader_runs: 0,
            },
            end: VerificationCheckpointExpectation {
                state: ExpectedSourceVerificationState::Verifying,
                active_operation: true,
                started_runs: 2,
                cancelled_runs: 1,
                failed_runs: 0,
                accepted_successes: 0,
                completed_reader_runs: 0,
            },
            minimum_accepted_progress_updates_delta: 0,
            completed_reader_work: None,
        };
        validate_verification_gate(&gate).unwrap();
        let start = checkpoint("Verifying", true, 3);
        let mut end = checkpoint("Verifying", true, 3);
        let mut reasons = BTreeSet::new();
        validate_verification_evidence(&start, &end, &gate, &mut reasons);
        assert!(reasons.is_empty());
        end["diagnostics"]["source_verification"]["active_operation"] = json!(false);
        validate_verification_evidence(&start, &end, &gate, &mut reasons);
        assert!(
            reasons.contains("product_gate_verification_end_state_or_active_operation_mismatch")
        );

        let regressed = checkpoint("Verifying", true, 2);
        reasons.clear();
        validate_verification_evidence(&start, &regressed, &gate, &mut reasons);
        assert!(reasons.contains("verification_accepted_progress_delta_missing_or_below_gate"));
    }

    #[test]
    fn gesture_events_require_one_durable_commit_and_all_raw_samples() {
        let template = template(
            true,
            vec![
                json!({ "command": "sample_diagnostics", "label": "start" }),
                json!({ "command": "camera_zoom_sequence", "samples": 3 }),
                json!({ "command": "sample_diagnostics", "label": "end" }),
                json!({ "command": "quit" }),
            ],
        );
        let phase = ScriptPhase {
            name: "resident_3d_zoom".to_owned(),
            start_diagnostic_label: Some("start".to_owned()),
            end_diagnostic_label: "end".to_owned(),
        };
        let mut report = json!({ "events": [{
            "command_index": 1,
            "status": "passed",
            "details": { "observed_counter_delta": {
                "detailed_counters_enabled": true,
                "durable_gesture_commits": 1,
                "raw_input_samples": 3,
            }},
        }] });
        let start_diagnostics = json!({
            "render": { "display_coordination": { "durable_gesture_commits": 4 } },
            "application_state": {
                "currentness_generation": 20,
                "currentness_derivation": "ApplicationSnapshot_currentness_generation",
            },
            "project_state": {
                "bound": true,
                "revision_high_water_sequence": 10,
                "retained_history_entries": 7,
                "history_entry_high_water_sequence": 10,
                "history_entry_high_water_derivation": "one_BoundWorkspace_history_push_per_allocated_durable_revision",
            },
        });
        let end_diagnostics = json!({
            "render": { "display_coordination": { "durable_gesture_commits": 5 } },
            "application_state": {
                "currentness_generation": 21,
                "currentness_derivation": "ApplicationSnapshot_currentness_generation",
            },
            "project_state": {
                "bound": true,
                "revision_high_water_sequence": 11,
                "retained_history_entries": 8,
                "history_entry_high_water_sequence": 11,
                "history_entry_high_water_derivation": "one_BoundWorkspace_history_push_per_allocated_durable_revision",
            },
        });
        let mut reasons = BTreeSet::new();
        validate_sequence_commit_events(
            &report,
            &template,
            &phase,
            &start_diagnostics,
            &end_diagnostics,
            1,
            &mut reasons,
        );
        assert!(reasons.is_empty());

        let unbound_start = json!({
            "render": { "display_coordination": { "durable_gesture_commits": 4 } },
            "application_state": {
                "currentness_generation": 20,
                "currentness_derivation": "ApplicationSnapshot_currentness_generation",
            },
            "project_state": {
                "bound": false,
                "current_revision": null,
                "saved_revision": null,
                "revision_high_water_sequence": null,
                "retained_history_entries": null,
                "history_entry_high_water_sequence": null,
            },
        });
        let unbound_end = json!({
            "render": { "display_coordination": { "durable_gesture_commits": 5 } },
            "application_state": {
                "currentness_generation": 21,
                "currentness_derivation": "ApplicationSnapshot_currentness_generation",
            },
            "project_state": {
                "bound": false,
                "current_revision": null,
                "saved_revision": null,
                "revision_high_water_sequence": null,
                "retained_history_entries": null,
                "history_entry_high_water_sequence": null,
            },
        });
        reasons.clear();
        validate_sequence_commit_events(
            &report,
            &template,
            &phase,
            &unbound_start,
            &unbound_end,
            1,
            &mut reasons,
        );
        assert!(reasons.is_empty());

        let mut invalid_unbound = unbound_end;
        invalid_unbound["project_state"]["revision_high_water_sequence"] = json!(1);
        validate_sequence_commit_events(
            &report,
            &template,
            &phase,
            &unbound_start,
            &invalid_unbound,
            1,
            &mut reasons,
        );
        assert!(
            reasons.contains("phase_unbound_project_revision_or_history_fact_not_explicit_null")
        );

        report["events"][0]["details"]["observed_counter_delta"]["durable_gesture_commits"] =
            json!(0);
        validate_sequence_commit_events(
            &report,
            &template,
            &phase,
            &start_diagnostics,
            &end_diagnostics,
            1,
            &mut reasons,
        );
        assert!(
            reasons
                .contains("product_gate_gesture_sequence_durable_commit_or_sample_delta_mismatch")
        );
    }

    #[test]
    fn cold_bootstrap_checkpoint_is_the_sequence_phase_prefix() {
        let mut template = template(
            true,
            vec![
                json!({ "command": "sample_diagnostics", "label": "cold-end" }),
                json!({ "command": "quit" }),
            ],
        );
        template.startup_bootstrap = Some(AutomationStartupBootstrap {
            capture_start_checkpoint: true,
            start_diagnostic_label: Some("cold-start".to_owned()),
            commands: vec![json!({
                "command": "set_viewer_layout",
                "layout": "four_panel",
            })],
        });
        let phase = ScriptPhase {
            name: "blocking_target_settled".to_owned(),
            start_diagnostic_label: Some("cold-start".to_owned()),
            end_diagnostic_label: "cold-end".to_owned(),
        };
        let diagnostics = json!({
            "render": { "display_coordination": { "durable_gesture_commits": 0 } },
            "application_state": {
                "currentness_generation": 7,
                "currentness_derivation": "ApplicationSnapshot_currentness_generation",
            },
            "project_state": {
                "bound": false,
                "current_revision": null,
                "saved_revision": null,
                "revision_high_water_sequence": null,
                "retained_history_entries": null,
                "history_entry_high_water_sequence": null,
            },
        });
        let mut reasons = BTreeSet::new();
        validate_sequence_commit_events(
            &json!({ "events": [] }),
            &template,
            &phase,
            &diagnostics,
            &diagnostics,
            1,
            &mut reasons,
        );
        assert!(reasons.is_empty());
    }

    fn gate_target(condition: &str) -> Value {
        if condition == IMPORTED_OPEN_READY_CONDITION {
            json!({
                "kind": "imported_open_ready",
                "path": "${ATTEMPT_ROOT}/output/imported.m4d",
            })
        } else {
            json!({ "kind": "condition", "condition": condition })
        }
    }

    fn observe_gate_batch_command(
        batch_id: &str,
        phase_id: &str,
        origin: Value,
        observations: &[(&str, &str, &str, u64)],
    ) -> Value {
        json!({
            "command": "observe_gate_batch",
            "batch_id": batch_id,
            "phase_id": phase_id,
            "origin": origin,
            "observations": observations.iter().map(|(gate_id, condition, authority, deadline)| json!({
                "gate_id": gate_id,
                "deadline_authority": authority,
                "deadline_after_origin_ns": deadline,
                "target": gate_target(condition),
            })).collect::<Vec<_>>(),
        })
    }

    fn observe_gate_batch_event(
        command_index: usize,
        command: &Value,
        outcomes: &[ProductGateStatus],
    ) -> Value {
        let observations = command["observations"].as_array().unwrap();
        assert_eq!(observations.len(), outcomes.len());
        let rows = observations
            .iter()
            .zip(outcomes)
            .enumerate()
            .map(|(observation_index, (observation, outcome))| {
                let failed = *outcome == ProductGateStatus::Failed;
                let deadline = observation["deadline_after_origin_ns"].as_u64().unwrap();
                let condition = observation
                    .pointer("/target/condition")
                    .and_then(Value::as_str)
                    .unwrap_or(IMPORTED_OPEN_READY_CONDITION);
                json!({
                    "observation_index": observation_index,
                    "gate_id": observation["gate_id"],
                    "condition": condition,
                    "deadline_authority": observation["deadline_authority"],
                    "deadline_after_origin_ns": deadline,
                    "outcome": outcome.report_label(),
                    "condition_met": !failed,
                    "timed_out": failed,
                    "observed_after_origin_ns": if failed { deadline } else { deadline - 1 },
                })
            })
            .collect::<Vec<_>>();
        json!({
            "command_index": command_index,
            "command": "observe_gate_batch",
            "status": "passed",
            "event_epoch_ms": 1_000_u64,
            "duration_ms": 1.0,
            "details": {
                "schema": PRODUCT_GATE_OBSERVATION_SCHEMA,
                "batch_id": command["batch_id"],
                "phase_id": command["phase_id"],
                "origin": command["origin"],
                "completed_after_origin_ns": rows.iter()
                    .filter_map(|row| row["observed_after_origin_ns"].as_u64())
                    .max()
                    .unwrap(),
                "observations": rows,
            },
        })
    }

    fn frozen_product_gate_inventory_commands(id: &str) -> Vec<Value> {
        let mut commands = Vec::new();
        for (condition, count) in expected_fatal_wait_condition_multiset(id) {
            for _ in 0..count {
                let timeout_ms = match condition {
                    "window_ready" => 5_000,
                    "source_verification_inactive" => SOURCE_VERIFICATION_QUIESCENCE_TIMEOUT_MS,
                    "source_verification_verified" | "source_verification_required" => 30_000,
                    "runtime_idle" => 30_000,
                    "import_review_ready" => 60_000,
                    _ => unreachable!(),
                };
                commands.push(json!({
                    "command": "wait_for",
                    "condition": condition,
                    "timeout_ms": timeout_ms,
                }));
            }
        }
        let conditions = expected_acceptance_condition_multiset(id)
            .into_iter()
            .flat_map(|(condition, count)| std::iter::repeat_n(condition, count))
            .collect::<Vec<_>>();
        let phase_ids = expected_product_gate_phase_ids(id);
        let mut condition_offset = 0_usize;
        for (batch_ordinal, phase_id) in phase_ids.iter().enumerate() {
            commands.push(json!({ "command": "sleep_frames", "frames": 1 }));
            let batches_left = phase_ids.len() - batch_ordinal;
            let conditions_left = conditions.len() - condition_offset;
            let take = conditions_left.div_ceil(batches_left);
            let observations = conditions[condition_offset..condition_offset + take]
                .iter()
                .enumerate()
                .map(|(local, condition)| {
                    let ordinal = condition_offset + local;
                    (
                        format!("{id}.acceptance.{ordinal:03}.{condition}"),
                        *condition,
                        if id == "IP" {
                            "import_primary_wall"
                        } else {
                            "maximum_current_presentation_gap_plus_poll_grace"
                        },
                        if id == "IP" {
                            1_200_000_000_000
                        } else {
                            66_666_668
                        },
                    )
                })
                .collect::<Vec<_>>();
            let borrowed = observations
                .iter()
                .map(|(gate_id, condition, authority, deadline)| {
                    (gate_id.as_str(), *condition, *authority, *deadline)
                })
                .collect::<Vec<_>>();
            let origin = if id == "IP" {
                json!({ "kind": "import_primary_started" })
            } else {
                json!({
                    "kind": "command_completed",
                    "command_index": commands.len() - 1,
                })
            };
            commands.push(observe_gate_batch_command(
                &format!("{id}.batch.{batch_ordinal:03}"),
                phase_id,
                origin,
                &borrowed,
            ));
            condition_offset += take;
        }
        commands
    }

    #[test]
    fn v5_gate_batch_events_form_an_exact_hierarchical_bijection() {
        let command = observe_gate_batch_command(
            "RZ.batch.000",
            "resident_cross_section_zoom.checkpoint.000",
            json!({ "kind": "command_completed", "command_index": 0 }),
            &[
                (
                    "RZ.acceptance.000.coordinated_presentation_settled",
                    "coordinated_presentation_settled",
                    "maximum_current_presentation_gap_plus_poll_grace",
                    66_666_668,
                ),
                (
                    "RZ.acceptance.001.runtime_idle",
                    "runtime_idle",
                    "maximum_current_presentation_gap_plus_poll_grace",
                    66_666_668,
                ),
            ],
        );
        let template = template(
            true,
            vec![
                json!({ "command": "sleep_frames", "frames": 1 }),
                command.clone(),
            ],
        );
        let report = json!({
            "events": [observe_gate_batch_event(1, &command, &[ProductGateStatus::Passed, ProductGateStatus::Failed])],
        });
        let outcomes = product_gate_outcomes_from_report(&report, &template).unwrap();
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes[0].outcome, ProductGateStatus::Passed);
        assert_eq!(outcomes[1].outcome, ProductGateStatus::Failed);
        assert_eq!(outcomes[1].observation_index, 1);

        let mut missing = report.clone();
        missing["events"][0]["details"]["observations"]
            .as_array_mut()
            .unwrap()
            .pop();
        assert!(product_gate_outcomes_from_report(&missing, &template).is_err());

        let mut reordered = report.clone();
        reordered["events"][0]["details"]["observations"]
            .as_array_mut()
            .unwrap()
            .swap(0, 1);
        assert!(product_gate_outcomes_from_report(&reordered, &template).is_err());

        let mut duplicate = report.clone();
        duplicate["events"][0]["details"]["observations"][1]["observation_index"] = json!(0);
        assert!(product_gate_outcomes_from_report(&duplicate, &template).is_err());

        let mut mismatched = report.clone();
        mismatched["events"][0]["details"]["observations"][0]["condition"] = json!("runtime_idle");
        assert!(product_gate_outcomes_from_report(&mismatched, &template).is_err());

        let mut legacy_event = report;
        legacy_event["events"].as_array_mut().unwrap().push(json!({
            "command": "observe_gate",
            "command_index": 99,
        }));
        assert!(product_gate_outcomes_from_report(&legacy_event, &template).is_err());
    }

    #[test]
    fn gate_batch_event_shape_and_deadline_wins_fail_closed() {
        let command = observe_gate_batch_command(
            "RZ.batch.000",
            "resident_cross_section_zoom.checkpoint.000",
            json!({ "kind": "command_completed", "command_index": 0 }),
            &[(
                "RZ.acceptance.000.coordinated_presentation_settled",
                "coordinated_presentation_settled",
                "maximum_current_presentation_gap_plus_poll_grace",
                66_666_668,
            )],
        );
        let template = template(
            true,
            vec![
                json!({ "command": "sleep_frames", "frames": 1 }),
                command.clone(),
            ],
        );
        let report = json!({
            "events": [observe_gate_batch_event(1, &command, &[ProductGateStatus::Failed])],
        });
        assert!(product_gate_outcomes_from_report(&report, &template).is_ok());

        let mut extra = report.clone();
        extra["events"][0]["details"]["observations"][0]["reason"] = json!("private free text");
        assert!(product_gate_outcomes_from_report(&extra, &template).is_err());

        let mut late_true = report.clone();
        late_true["events"][0]["details"]["observations"][0]["condition_met"] = json!(true);
        assert!(product_gate_outcomes_from_report(&late_true, &template).is_ok());

        let mut incoherent = report.clone();
        incoherent["events"][0]["details"]["observations"][0]["timed_out"] = json!(false);
        assert!(product_gate_outcomes_from_report(&incoherent, &template).is_err());

        let mut early = report.clone();
        early["events"][0]["details"]["observations"][0]["observed_after_origin_ns"] =
            json!(66_666_667_u64);
        assert!(product_gate_outcomes_from_report(&early, &template).is_err());

        let mut passed_at_deadline = report.clone();
        let row = &mut passed_at_deadline["events"][0]["details"]["observations"][0];
        row["outcome"] = json!("passed");
        row["condition_met"] = json!(true);
        row["timed_out"] = json!(false);
        assert!(product_gate_outcomes_from_report(&passed_at_deadline, &template).is_err());

        let mut passed_before_deadline = passed_at_deadline.clone();
        passed_before_deadline["events"][0]["details"]["observations"][0]["observed_after_origin_ns"] =
            json!(66_666_667_u64);
        assert!(product_gate_outcomes_from_report(&passed_before_deadline, &template).is_ok());

        let mut missing_outer = report.clone();
        missing_outer["events"][0]
            .as_object_mut()
            .unwrap()
            .remove("event_epoch_ms");
        assert!(product_gate_outcomes_from_report(&missing_outer, &template).is_err());

        let mut extra_outer = report.clone();
        extra_outer["events"][0]["private_path"] = json!("/private/source");
        assert!(product_gate_outcomes_from_report(&extra_outer, &template).is_err());

        let mut bad_schema = report.clone();
        bad_schema["events"][0]["details"]["schema"] = json!("legacy");
        assert!(product_gate_outcomes_from_report(&bad_schema, &template).is_err());
    }

    #[test]
    fn gate_batch_commands_are_strict_bounded_and_v4_is_rejected() {
        let valid = observe_gate_batch_command(
            "RZ.batch.000",
            "resident_cross_section_zoom.checkpoint.000",
            json!({ "kind": "command_completed", "command_index": 0 }),
            &[(
                "RZ.acceptance.000.coordinated_presentation_settled",
                "coordinated_presentation_settled",
                "maximum_current_presentation_gap_plus_poll_grace",
                PRODUCT_GATE_DEADLINE_MAX_NS,
            )],
        );
        assert!(expected_product_gate_batches(std::slice::from_ref(&valid)).is_ok());
        let mut excessive = valid.clone();
        excessive["observations"][0]["deadline_after_origin_ns"] =
            json!(PRODUCT_GATE_DEADLINE_MAX_NS + 1);
        assert!(expected_product_gate_batches(&[excessive]).is_err());
        let mut empty_path = valid.clone();
        empty_path["observations"][0]["target"] =
            json!({ "kind": "imported_open_ready", "path": "" });
        assert!(expected_product_gate_batches(&[empty_path]).is_err());
        let mut unsafe_id = valid.clone();
        unsafe_id["observations"][0]["gate_id"] = json!("/private/gate");
        assert!(expected_product_gate_batches(&[unsafe_id]).is_err());
        let mut duplicate = valid.clone();
        let duplicate_observation = duplicate["observations"][0].clone();
        duplicate["observations"]
            .as_array_mut()
            .unwrap()
            .push(duplicate_observation);
        assert!(expected_product_gate_batches(&[duplicate]).is_err());
        assert!(
            expected_product_gate_batches(&[json!({
                "command": "observe_gate",
                "gate_id": "RZ.removed",
                "condition": "runtime_idle",
                "timeout_ms": 30_000,
            })])
            .is_err()
        );

        let valid_commands = vec![
            json!({ "command": "sample_diagnostics", "label": "rz-start" }),
            json!({ "command": "sample_diagnostics", "label": "rz-end" }),
            valid,
            json!({ "command": "quit" }),
        ];
        let script = template(true, valid_commands);
        assert!(validate_automation_template("RZ", &script, true, &["rz-start", "rz-end"]).is_ok());

        let mut after_quit = script;
        after_quit.commands.swap(2, 3);
        assert!(
            validate_automation_template("RZ", &after_quit, true, &["rz-start", "rz-end"]).is_err()
        );
    }

    #[test]
    fn v5_product_gate_and_fatal_wait_inventories_are_exact_and_nonempty() {
        let mut total_gates = 0_usize;
        let mut total_batches = 0_usize;
        let mut total_fatal_waits = 0_usize;
        for id in REQUIRED_SCENARIOS {
            let commands = frozen_product_gate_inventory_commands(id);
            validate_product_gate_inventory(id, &commands).unwrap();
            total_gates += expected_product_gate_observations(&commands).unwrap().len();
            total_batches += expected_product_gate_batches(&commands).unwrap().len();
            total_fatal_waits += commands
                .iter()
                .filter(|command| {
                    command.get("command").and_then(Value::as_str) == Some("wait_for")
                })
                .count();
        }
        assert_eq!(total_gates, 74);
        assert_eq!(total_batches, 37);
        assert_eq!(total_fatal_waits, 24);
        assert_eq!(total_gates * 3 * 2, 444);

        let mut missing = frozen_product_gate_inventory_commands("RZ");
        missing.retain(|command| {
            command.get("command").and_then(Value::as_str) != Some("observe_gate_batch")
        });
        assert!(validate_product_gate_inventory("RZ", &missing).is_err());

        let mut wrong_id = frozen_product_gate_inventory_commands("RZ");
        let gate = wrong_id
            .iter_mut()
            .find(|command| {
                command.get("command").and_then(Value::as_str) == Some("observe_gate_batch")
            })
            .unwrap();
        gate["gate_id"] = json!("RZ.acceptance.999.runtime_idle");
        assert!(validate_product_gate_inventory("RZ", &wrong_id).is_err());

        let mut legacy = frozen_product_gate_inventory_commands("RZ");
        let gate_index = legacy
            .iter()
            .position(|command| {
                command.get("command").and_then(Value::as_str) == Some("observe_gate_batch")
            })
            .unwrap();
        let condition = legacy[gate_index]["observations"][0]["target"]["condition"].clone();
        legacy[gate_index] = json!({
            "command": "wait_for",
            "condition": condition,
            "timeout_ms": 30_000,
        });
        assert!(validate_product_gate_inventory("RZ", &legacy).is_err());
    }

    #[test]
    fn legacy_failed_automation_reports_remain_fatal_and_never_become_gate_outcomes() {
        let template = template(
            false,
            vec![json!({
                "command": "wait_for_imported_open_ready",
                "path": "/private/attempt/imported.m4d",
                "timeout_ms": 30_000,
            })],
        );
        let report = json!({
            "schema": AUTOMATION_REPORT_SCHEMA,
            "schema_version": AUTOMATION_REPORT_SCHEMA_VERSION,
            "status": "failed",
            "failure_reason": "timed out waiting for imported package open-ready",
            "events": [],
        });
        let mut outcomes = Vec::new();
        let mut reasons = BTreeSet::new();
        validate_basic_automation_report(
            &report,
            Path::new("/unavailable/app"),
            Path::new("/unavailable/script"),
            &template,
            &profile(),
            AttemptRole::InstrumentationControl,
            &mut outcomes,
            &mut reasons,
        );
        assert!(reasons.contains("automation_report_failed"));
        assert!(outcomes.is_empty());
    }

    #[test]
    fn exact_population_requires_all_thirty_samples_sixty_roles_and_gate_rows() {
        let profile = profile();
        let scripts = population_scripts();
        let samples = complete_population_samples();
        let mut reasons = BTreeSet::new();
        let population = validate_attempt_population(&profile, &scripts, &samples, &mut reasons);
        assert!(reasons.is_empty(), "{reasons:#?}");
        assert_eq!(population.expected_sample_records, 30);
        assert_eq!(population.observed_sample_records, 30);
        assert_eq!(population.expected_role_attempts, 60);
        assert_eq!(population.observed_role_attempts, 60);
        assert_eq!(population.completed_role_reports, 60);
        assert_eq!(population.expected_phase_evaluations, 54);
        assert_eq!(population.observed_phase_evaluations, 54);
        assert_eq!(population.expected_product_gate_observations, 444);
        assert_eq!(population.observed_product_gate_observations, 444);
        assert!(population.sample_order_exact);
        assert!(population.role_order_exact);
        assert_eq!(
            samples
                .iter()
                .filter(|sample| {
                    sample.role_launch_order.first() == Some(&AttemptRole::Instrumented)
                })
                .count(),
            15
        );
        assert_eq!(
            samples
                .iter()
                .filter(|sample| {
                    sample.role_launch_order.first() == Some(&AttemptRole::InstrumentationControl)
                })
                .count(),
            15
        );
        assert_eq!(population_json(population)["exact"], json!(true));

        let mut filtered = complete_population_samples();
        filtered.pop();
        let mut reasons = BTreeSet::new();
        validate_attempt_population(&profile, &scripts, &filtered, &mut reasons);
        assert!(reasons.contains("sample_population_cardinality_mismatch"));
        assert!(reasons.contains("sample_population_identity_mismatch"));
        assert!(reasons.contains("role_attempt_population_cardinality_mismatch"));
        assert!(reasons.contains("completed_role_report_population_mismatch"));
        assert!(reasons.contains("phase_evaluation_population_mismatch"));
        assert!(reasons.contains("product_gate_observation_population_mismatch"));

        let mut missing_gate = complete_population_samples();
        missing_gate[0].instrumented.product_gate_outcomes.clear();
        let mut reasons = BTreeSet::new();
        validate_attempt_population(&profile, &scripts, &missing_gate, &mut reasons);
        assert!(reasons.contains("product_gate_observation_population_mismatch"));
        assert!(reasons.contains("product_gate_observation_identity_mismatch"));

        let mut duplicate_gate = complete_population_samples();
        let duplicated = duplicate_gate[0].instrumented.product_gate_outcomes[0].clone();
        duplicate_gate[0]
            .instrumented
            .product_gate_outcomes
            .push(duplicated);
        let mut reasons = BTreeSet::new();
        validate_attempt_population(&profile, &scripts, &duplicate_gate, &mut reasons);
        assert!(reasons.contains("product_gate_observation_population_mismatch"));
        assert!(reasons.contains("product_gate_observation_identity_mismatch"));

        let mut reordered_gates = complete_population_samples();
        reordered_gates[0]
            .instrumented
            .product_gate_outcomes
            .swap(0, 1);
        let mut reasons = BTreeSet::new();
        validate_attempt_population(&profile, &scripts, &reordered_gates, &mut reasons);
        assert!(reasons.contains("product_gate_observation_identity_mismatch"));

        let mut incoherent_gate = complete_population_samples();
        incoherent_gate[0].instrumented.product_gate_outcomes[0].condition_met = false;
        let mut reasons = BTreeSet::new();
        validate_attempt_population(&profile, &scripts, &incoherent_gate, &mut reasons);
        assert!(reasons.contains("product_gate_observation_identity_mismatch"));

        let mut duplicate_identity = complete_population_samples();
        duplicate_identity[29].sample_index = duplicate_identity[0].sample_index;
        duplicate_identity[29].scenario = duplicate_identity[0].scenario.clone();
        let mut reasons = BTreeSet::new();
        let population =
            validate_attempt_population(&profile, &scripts, &duplicate_identity, &mut reasons);
        assert_eq!(population.observed_sample_records, 30);
        assert_eq!(population.observed_role_attempts, 60);
        assert!(!population.sample_identities_exact);
        assert_eq!(population_json(population)["exact"], json!(false));

        let mut reordered = complete_population_samples();
        reordered.swap(0, 1);
        let mut reasons = BTreeSet::new();
        let population = validate_attempt_population(&profile, &scripts, &reordered, &mut reasons);
        assert!(population.sample_identities_exact);
        assert!(!population.sample_order_exact);
        assert!(reasons.contains("sample_population_order_mismatch"));
        assert_eq!(population_json(population)["exact"], json!(false));

        let mut wrong_role_order = complete_population_samples();
        wrong_role_order[0].role_launch_order.reverse();
        let mut reasons = BTreeSet::new();
        let population =
            validate_attempt_population(&profile, &scripts, &wrong_role_order, &mut reasons);
        assert!(!population.role_order_exact);
        assert!(reasons.contains("role_attempt_order_mismatch"));
        assert_eq!(population_json(population)["exact"], json!(false));

        let mut unlaunched = complete_population_samples();
        unlaunched[0]
            .control
            .as_mut()
            .unwrap()
            .process
            .launch_attempted = false;
        let mut reasons = BTreeSet::new();
        let population = validate_attempt_population(&profile, &scripts, &unlaunched, &mut reasons);
        assert_eq!(population.observed_role_attempts, 59);
        assert!(reasons.contains("role_attempt_population_cardinality_mismatch"));
        assert_eq!(
            sanitized_role_schedule_rows(&unlaunched)[1]["launch_attempted"],
            json!(false)
        );

        let mut wrong_role = complete_population_samples();
        wrong_role[0].instrumented.role = AttemptRole::InstrumentationControl;
        let mut reasons = BTreeSet::new();
        let population = validate_attempt_population(&profile, &scripts, &wrong_role, &mut reasons);
        assert!(!population.role_identities_exact);
        assert_eq!(population_json(population)["exact"], json!(false));

        let mut wrong_phase = complete_population_samples();
        wrong_phase[0].phases[0].name = "wrong-phase".to_owned();
        let mut reasons = BTreeSet::new();
        let population =
            validate_attempt_population(&profile, &scripts, &wrong_phase, &mut reasons);
        assert!(!population.phase_identities_exact);
        assert_eq!(population_json(population)["exact"], json!(false));

        let mut wrong_gate = complete_population_samples();
        wrong_gate[0].instrumented.product_gate_outcomes[0].command_index = 2;
        let mut reasons = BTreeSet::new();
        let population = validate_attempt_population(&profile, &scripts, &wrong_gate, &mut reasons);
        assert_eq!(population.observed_product_gate_observations, 444);
        assert!(!population.product_gate_bijections_exact);
        assert_eq!(population_json(population)["exact"], json!(false));
    }

    #[test]
    fn native_exit_depends_only_on_evidence_integrity() {
        assert!(require_valid_evidence(&BTreeSet::new()).is_ok());
        let valid_negative = BTreeSet::from([
            "product_gate_visible_panel_milestone_set_mismatch".to_owned(),
            "product_gate_gpu_timing_unavailable_without_expected_current_presentation".to_owned(),
            "import_receipt_source_read_amplification_exceeded".to_owned(),
            "product_gate_exact_cross_scope_target_union_unique_keys_mismatch".to_owned(),
            "unique_work_gpu_uploaded_payload_bytes_delta_outside_oracle".to_owned(),
            "structural_queue_submissions_counter_changed".to_owned(),
            "structural_command_encoders_ceiling_exceeded".to_owned(),
            "conformance_dvr_frozen_world_distance_oracle_failed".to_owned(),
            "conformance_perspective_iso_pick_value_mismatch".to_owned(),
        ]);
        assert!(require_valid_evidence(&valid_negative).is_ok());
        assert_eq!(evidence_status(&valid_negative), "valid_complete");
        assert!(has_product_gate_failures(&valid_negative));
        assert_eq!(
            product_gate_status(true, true),
            "failed",
            "a valid failed product gate remains an authoritative negative outcome"
        );
        assert!(
            require_valid_evidence(&BTreeSet::from(["phase_end_checkpoint_missing".to_owned()]))
                .is_err()
        );
        assert_eq!(product_gate_status(false, true), "not_authoritative");

        for reason in [
            "product_gate_observation_event_set_invalid",
            "product_gate_observation_identity_mismatch",
            "product_gate_observation_population_mismatch",
            "instrumentation_overhead_gate_exceeded",
            "failed_imported_open_ready_evidence_shape_invalid",
            "import_clock_evidence_shape_invalid",
            "import_receipt_source_read_amplification_operand_invalid",
            "product_gate_unknown_future_integrity_fault",
            "unknown_delta_outside_oracle",
            "structural_unknown_counter_changed",
            "structural_unknown_ceiling_exceeded",
            "new_unclassified_reason",
        ] {
            assert_eq!(reason_axis(reason), ReasonAxis::Integrity, "{reason}");
            assert!(require_valid_evidence(&BTreeSet::from([reason.to_owned()])).is_err());
        }
    }

    #[test]
    fn sanitized_receipt_contains_commitments_but_no_private_paths() {
        let profile = LoadedProfile {
            profile: profile(),
            sha256: "55".repeat(32),
        };
        let workload = LoadedBundle {
            value: WorkloadBundle {
                schema: WORKLOAD_SCHEMA.to_owned(),
                representative_package_root_manifest_sha256: "11".repeat(32),
                supporting_temporal_package_root_manifest_sha256: "aa".repeat(32),
                import_source: ImportSourceBinding {
                    inventory_sha256: "bb".repeat(32),
                    reviewed_source_fingerprint_sha256: "cc".repeat(32),
                    regular_files: 1,
                    source_bytes: 1,
                },
                ep01_trace_geometry: ep01_trace_geometry(),
                scenarios: Vec::new(),
            },
            sha256: "22".repeat(32),
            path: PathBuf::from("/private/secret/workload.json"),
        };
        let scripts = LoadedBundle {
            value: population_scripts(),
            sha256: "33".repeat(32),
            path: PathBuf::from("/private/secret/scripts.json"),
        };
        let oracle = LoadedBundle {
            value: OracleBundle {
                schema: ORACLE_SCHEMA.to_owned(),
                independent_sources: IndependentOracleSources {
                    lod_oracle_source_sha256: "88".repeat(32),
                    numerical_oracle_source_sha256: "99".repeat(32),
                },
                numerical_contract: numerical_contract(),
                conformance_cases: Vec::new(),
                scenarios: Vec::new(),
            },
            sha256: "44".repeat(32),
            path: PathBuf::from("/private/secret/oracle.json"),
        };
        let mut samples = complete_population_samples();
        let failed_outcome = &mut samples[0].instrumented.product_gate_outcomes[0];
        failed_outcome.outcome = ProductGateStatus::Failed;
        failed_outcome.condition_met = false;
        failed_outcome.timed_out = true;
        failed_outcome.observed_after_origin_ns = failed_outcome.deadline_after_origin_ns;
        samples[0].phases[0]
            .reasons
            .insert("product_gate_visible_panel_milestone_set_mismatch".to_owned());
        let mut reasons = BTreeSet::new();
        let population =
            validate_attempt_population(&profile.profile, &scripts.value, &samples, &mut reasons);
        let instrumentation_overhead = validate_population_instrumentation_overhead(
            &profile.profile,
            &samples,
            population,
            &mut reasons,
        );
        assert!(reasons.is_empty());
        let receipt = sanitized_receipt(
            &profile,
            &workload,
            &scripts,
            &oracle,
            &"66".repeat(32),
            &"77".repeat(32),
            None,
            &samples,
            population,
            &instrumentation_overhead,
            &BTreeSet::new(),
        );
        let encoded = serde_json::to_string(&receipt).unwrap();
        assert!(encoded.contains(&"55".repeat(32)));
        assert_eq!(
            receipt["commitments"]["ep01_selection_authority_sha256"],
            super::super::ep01_selection::authority_fingerprint_sha256()
        );
        assert_eq!(
            receipt["commitments"]["ep01_trace_geometry_sha256"],
            ep01_trace_geometry_sha256(&workload.value.ep01_trace_geometry)
        );
        assert!(!encoded.contains("/private"));
        assert!(!encoded.contains("secret"));
        assert!(!encoded.contains("temporal.m4d"));
        assert!(!encoded.contains("failure_reason"));
        assert!(!encoded.contains("private_path"));
        assert_eq!(receipt["evidence_status"], "valid_complete");
        assert_eq!(receipt["product_gate_status"], "failed");
        assert_eq!(receipt["population"]["exact"], true);
        assert_eq!(
            receipt["product_gate_outcomes"].as_array().unwrap().len(),
            60
        );
        let overhead_rows = receipt["instrumentation_overhead_populations"]
            .as_array()
            .unwrap();
        assert_eq!(overhead_rows.len(), REQUIRED_SCENARIOS.len());
        assert!(overhead_rows.iter().all(|row| {
            row["expected_sample_pairs"] == json!(3)
                && row["observed_sample_pairs"] == json!(3)
                && row["instrumented_raw_app_wall_time_ns"] == json!(3)
                && row["instrumented_qualification_gpu_timing_await_wall_time_ns"] == json!(0)
                && row["instrumented_adjusted_app_wall_time_ns"] == json!(3)
                && row["control_app_wall_time_ns"] == json!(3)
                && row["maximum_overhead_basis_points"] == json!(200)
                && row["population_complete"] == json!(true)
                && row["gate_evaluable"] == json!(true)
                && row["gate_passed"] == json!(true)
        }));
        let public_gate_fields = BTreeSet::from([
            "sample_index",
            "scenario",
            "role",
            "batch_id",
            "phase_id",
            "observation_index",
            "gate_id",
            "condition",
            "deadline_authority",
            "deadline_after_origin_ns",
            "outcome",
        ]);
        for row in receipt["product_gate_outcomes"].as_array().unwrap() {
            assert_eq!(
                row.as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>(),
                public_gate_fields,
            );
            for private_field in [
                "schema",
                "command_index",
                "condition_met",
                "timed_out",
                "observed_after_origin_ns",
                "origin",
            ] {
                assert!(row.get(private_field).is_none());
            }
        }
        assert!(
            !receipt["product_gate_failures"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            receipt["role_schedule_bounds"].as_array().unwrap().len(),
            60
        );
        assert!(receipt.get("status").is_none());
        assert!(receipt.get("reason_codes").is_none());
        assert!(encoded.contains("non_OS_input_non_E4"));
    }
}

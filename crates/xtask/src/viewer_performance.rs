use std::{
    collections::BTreeSet,
    env, fs,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};
use mirante4d_identity::Sha256Hasher;
use rustix::fs::{CWD, Mode, OFlags, ResolveFlags, openat2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::host::{
    QualificationBuildProvenance, RepositoryIdentity, host_hardware_identity,
    qualification_build_provenance, qualification_build_reason_codes, repository_identity,
};

const PROFILE_SCHEMA: &str = "mirante4d-viewer-performance-qualification-profile-5";
const PROFILE_AUTHORITY_SCHEMA: &str = "mirante4d-viewer-performance-profile-authority";
const PROFILE_AUTHORITY_SCHEMA_VERSION: u64 = 1;
const PROFILE_AUTHORITY_BYTES: &[u8] =
    include_bytes!("../../../verification/viewer-performance-profile.json");
const REPORT_SCHEMA: &str = "mirante4d-viewer-performance-preflight-report-1";
const PROFILE_MAX_BYTES: u64 = 64 * 1024;
const PACKAGE_ROOT_MANIFEST_MAX_BYTES: u64 = 1024 * 1024;
const REQUIRED_SCENARIOS: [&str; 10] = ["RZ", "ZB", "RO", "ST", "NO", "FC", "VM", "PT", "VV", "IP"];
const BLOCKING_WIDTH: u32 = 1280;
const BLOCKING_HEIGHT: u32 = 720;
const EXERCISE_WIDTH: u32 = 1920;
const EXERCISE_HEIGHT: u32 = 1080;

mod conformance_receipt;
mod ep01_selection;
mod runner;
mod source_inventory;

pub(crate) const USAGE: &str = "usage: cargo xtask viewer-performance-preflight \
    --qualification-profile ABSOLUTE_EXTERNAL_PROFILE.json \
    --workload-bundle ABSOLUTE_EXTERNAL_WORKLOAD.json \
    --interaction-script-bundle ABSOLUTE_EXTERNAL_SCRIPTS.json \
    --independent-oracle ABSOLUTE_EXTERNAL_ORACLE.json \
    --cache-condition warm|cold --competing-activity DESCRIPTION \
    --power-state DESCRIPTION --compositor-scale-milli INTEGER";

pub(crate) use runner::run_measurement;

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreflightArgs {
    profile: PathBuf,
    workload_bundle: PathBuf,
    script_bundle: PathBuf,
    oracle_bundle: PathBuf,
    attestation: ProtocolAttestation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProtocolAttestation {
    cache_condition: String,
    competing_activity: String,
    power_state: String,
    compositor_scale_milli: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ViewerQualificationProfile {
    schema: String,
    hardware_class: String,
    ep01_selection_authority_sha256: String,
    build: BuildBinding,
    workload: WorkloadBinding,
    host: HostBinding,
    graphics: GraphicsBinding,
    display: DisplayBinding,
    protocol: ProtocolBinding,
    extents: ExtentBinding,
    resources: ResourceBinding,
    absolute_gates: AbsoluteGates,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ViewerProfileAuthority {
    schema: String,
    schema_version: u64,
    owner_accepted_profile_contract_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct BuildBinding {
    repository_revision: String,
    profile: String,
    compiler: String,
    target_mode: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct WorkloadBinding {
    representative_package: RepresentativePackageBinding,
    workload_bundle_sha256: String,
    interaction_script_bundle_sha256: String,
    independent_oracle_sha256: String,
    scenarios: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RepresentativePackageBinding {
    root: PathBuf,
    root_manifest_sha256: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HostBinding {
    os: String,
    arch: String,
    cpu_model: String,
    logical_cpu_count: u64,
    mem_total_kib: u64,
    storage: StorageBinding,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StorageBinding {
    filesystem_type: String,
    filesystem_source: String,
    filesystem_uuid: String,
    device_major_minor: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct GraphicsBinding {
    backend: String,
    adapter_name: String,
    vendor_id: u32,
    device_id: u32,
    device_type: String,
    api_version: String,
    driver_version: String,
    driver_name: String,
    driver_info: String,
    dedicated_vram_bytes: u64,
    wgpu_version: String,
    naga_version: String,
    requested_features: Vec<String>,
    device_memory_hint: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DisplayBinding {
    session_type: String,
    compositor: String,
    output_name: String,
    current_mode: PixelExtent,
    physical_width_mm: u32,
    physical_height_mm: u32,
    refresh_millihz: u32,
    compositor_scale_milli: u32,
    presentation_mode: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PixelExtent {
    width: u32,
    height: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ProtocolBinding {
    application_cold: bool,
    empty_product_residency: bool,
    os_cache_condition: String,
    competing_activity: String,
    power_state: String,
    automatic_retries: u32,
    development_samples: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ExtentBinding {
    blocking_qualification: PixelExtent,
    required_exercise: PixelExtent,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ResourceBinding {
    max_cpu_total_bytes: u64,
    max_cpu_decoded_residency_bytes: u64,
    max_cpu_upload_staging_bytes: u64,
    gpu_budget_bytes: u64,
    max_gpu_resident_bytes: u64,
    max_gpu_in_flight_bytes: u64,
    max_open_objects: u64,
    max_queued_requests: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AbsoluteGates {
    resident_input_to_current_presentation_p95_ns: u64,
    maximum_current_presentation_gap_ns: u64,
    maximum_main_loop_heartbeat_gap_ns: u64,
    maximum_ui_thread_interaction_task_ns: u64,
    maximum_plane_gpu_ns: u64,
    maximum_mip_gpu_ns: u64,
    maximum_dvr_gpu_ns: u64,
    maximum_iso_gpu_ns: u64,
    cold_first_useful_ns: u64,
    cold_complete_coarse_ns: u64,
    cold_target_settlement_ns: u64,
    nonresident_target_settlement_ns: u64,
    source_verification_completion_ns: u64,
    maximum_instrumentation_overhead_basis_points: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StorageObservation {
    filesystem_type: String,
    filesystem_source: String,
    filesystem_uuid: String,
    device_major_minor: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GraphicsObservation {
    adapter_name: String,
    vendor_id: u32,
    device_id: u32,
    device_type: String,
    api_version: String,
    driver_version: String,
    driver_name: String,
    driver_info: String,
    dedicated_vram_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DisplayObservation {
    session_type: Option<String>,
    compositor: Option<String>,
    output_name: String,
    current_mode: PixelExtent,
    physical_width_mm: u32,
    physical_height_mm: u32,
    refresh_millihz: u32,
    supported_extents: BTreeSet<(u32, u32)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreflightObservations {
    repository_revision: Option<String>,
    repository_dirty_worktree: Option<bool>,
    compiler: Option<String>,
    cpu_model: Option<String>,
    logical_cpu_count: Option<u64>,
    mem_total_kib: Option<u64>,
    storage: Option<StorageObservation>,
    graphics: Option<GraphicsObservation>,
    display: Option<DisplayObservation>,
    power_state: Option<String>,
    wgpu_version: Option<String>,
    naga_version: Option<String>,
    package_root_manifest_sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LoadedProfile {
    profile: ViewerQualificationProfile,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct CargoLockPackages {
    package: Vec<CargoLockedPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoLockedPackage {
    name: String,
    version: String,
}

pub(crate) fn run(arguments: Vec<String>) -> anyhow::Result<()> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "help" | "--help" | "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let arguments = parse_args(arguments)?;
    let repository = repository_identity();
    let repository_root = repository
        .root
        .as_deref()
        .context("viewer preflight could not identify the repository root")?;
    let repository_root = fs::canonicalize(repository_root)
        .context("viewer preflight could not resolve the repository root")?;
    let loaded = load_external_profile(&arguments.profile, &repository_root)?;
    validate_owner_accepted_profile(&loaded.profile)?;
    let build_provenance = qualification_build_provenance();
    require_exact_release_build_binding(&loaded.profile, &repository, &build_provenance)?;
    let bundle_commitments = runner::load_and_validate_preflight_bundles(
        &loaded.profile,
        &arguments.workload_bundle,
        &arguments.script_bundle,
        &arguments.oracle_bundle,
        &repository_root,
    )?;

    let observations = observe(&loaded.profile, &repository_root);
    let mut reasons = binding_reasons(&loaded.profile, &arguments.attestation, &observations);
    reasons.sort_unstable();
    reasons.dedup();
    let report = sanitized_report(&loaded, &bundle_commitments, &observations, &reasons);
    println!(
        "{}",
        serde_json::to_string_pretty(&report)
            .context("failed to encode viewer preflight report")?
    );
    if !reasons.is_empty() {
        bail!("viewer performance preflight binding did not match; inspect reason_codes")
    }
    Ok(())
}

fn parse_args(arguments: Vec<String>) -> anyhow::Result<PreflightArgs> {
    let mut profile = None;
    let mut workload_bundle = None;
    let mut script_bundle = None;
    let mut oracle_bundle = None;
    let mut cache_condition = None;
    let mut competing_activity = None;
    let mut power_state = None;
    let mut compositor_scale_milli = None;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let value = match argument.as_str() {
            "--qualification-profile"
            | "--workload-bundle"
            | "--interaction-script-bundle"
            | "--independent-oracle"
            | "--cache-condition"
            | "--competing-activity"
            | "--power-state"
            | "--compositor-scale-milli" => arguments
                .next()
                .with_context(|| format!("{argument} requires a value; {USAGE}"))?,
            "help" | "--help" | "-h" => bail!("{USAGE}"),
            other => bail!("unknown viewer preflight argument {other:?}; {USAGE}"),
        };
        match argument.as_str() {
            "--qualification-profile" => set_once(&mut profile, PathBuf::from(value), &argument)?,
            "--workload-bundle" => set_once(&mut workload_bundle, PathBuf::from(value), &argument)?,
            "--interaction-script-bundle" => {
                set_once(&mut script_bundle, PathBuf::from(value), &argument)?
            }
            "--independent-oracle" => {
                set_once(&mut oracle_bundle, PathBuf::from(value), &argument)?
            }
            "--cache-condition" => set_once(&mut cache_condition, value, &argument)?,
            "--competing-activity" => set_once(&mut competing_activity, value, &argument)?,
            "--power-state" => set_once(&mut power_state, value, &argument)?,
            "--compositor-scale-milli" => {
                let parsed = value.parse::<u32>().with_context(|| {
                    format!("--compositor-scale-milli must be an unsigned integer; {USAGE}")
                })?;
                set_once(&mut compositor_scale_milli, parsed, &argument)?;
            }
            _ => unreachable!("the accepted argument list is exhaustive"),
        }
    }
    Ok(PreflightArgs {
        profile: profile
            .with_context(|| format!("--qualification-profile is required; {USAGE}"))?,
        workload_bundle: workload_bundle
            .with_context(|| format!("--workload-bundle is required; {USAGE}"))?,
        script_bundle: script_bundle
            .with_context(|| format!("--interaction-script-bundle is required; {USAGE}"))?,
        oracle_bundle: oracle_bundle
            .with_context(|| format!("--independent-oracle is required; {USAGE}"))?,
        attestation: ProtocolAttestation {
            cache_condition: cache_condition
                .with_context(|| format!("--cache-condition is required; {USAGE}"))?,
            competing_activity: competing_activity
                .with_context(|| format!("--competing-activity is required; {USAGE}"))?,
            power_state: power_state
                .with_context(|| format!("--power-state is required; {USAGE}"))?,
            compositor_scale_milli: compositor_scale_milli
                .with_context(|| format!("--compositor-scale-milli is required; {USAGE}"))?,
        },
    })
}

fn set_once<T>(slot: &mut Option<T>, value: T, name: &str) -> anyhow::Result<()> {
    if slot.replace(value).is_some() {
        bail!("{name} may be supplied only once; {USAGE}")
    }
    Ok(())
}

fn load_external_profile(path: &Path, repository_root: &Path) -> anyhow::Result<LoadedProfile> {
    if !path.is_absolute() {
        bail!("viewer qualification profile path must be absolute")
    }
    require_nonsymlink_components(path, "viewer qualification profile")?;
    let canonical = fs::canonicalize(path)
        .context("viewer qualification profile is unavailable or unreadable")?;
    if canonical.starts_with(repository_root) {
        bail!("viewer qualification profile must be outside the repository")
    }
    let bytes = read_bounded_regular_file(
        &canonical,
        PROFILE_MAX_BYTES,
        "viewer qualification profile",
    )?;
    let sha256 = Sha256Hasher::digest(&bytes).to_string();
    let profile = serde_json::from_slice::<ViewerQualificationProfile>(&bytes)
        .context("viewer qualification profile is not strict valid JSON")?;
    Ok(LoadedProfile { profile, sha256 })
}

fn require_nonsymlink_components(path: &Path, label: &str) -> anyhow::Result<()> {
    if !path.is_absolute() {
        bail!("{label} path must be absolute")
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Normal(_) => current.push(component.as_os_str()),
            Component::Prefix(_) => current.push(component.as_os_str()),
            Component::CurDir | Component::ParentDir => {
                bail!("{label} path must not contain relative components")
            }
        }
        let metadata = fs::symlink_metadata(&current)
            .with_context(|| format!("{label} path is unavailable or unreadable"))?;
        if metadata.file_type().is_symlink() {
            bail!("{label} path must not contain symbolic links")
        }
    }
    Ok(())
}

fn read_bounded_regular_file(path: &Path, max_bytes: u64, label: &str) -> anyhow::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("{label} is unavailable or unreadable"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label} must be a nonsymlink regular file")
    }
    if metadata.len() > max_bytes {
        bail!("{label} exceeds its {max_bytes}-byte bound")
    }
    let descriptor = openat2(
        CWD,
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
    )
    .with_context(|| format!("{label} is unavailable, unreadable, or contains a symbolic link"))?;
    let file = File::from(descriptor);
    let opened_metadata = file
        .metadata()
        .with_context(|| format!("{label} is unavailable or unreadable"))?;
    if !opened_metadata.is_file() {
        bail!("{label} must be a nonsymlink regular file")
    }
    if opened_metadata.len() > max_bytes {
        bail!("{label} exceeds its {max_bytes}-byte bound")
    }
    let mut bytes = Vec::with_capacity(usize::try_from(opened_metadata.len()).unwrap_or(0));
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("{label} is unavailable or unreadable"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        bail!("{label} exceeds its {max_bytes}-byte bound")
    }
    Ok(bytes)
}

fn validate_profile(profile: &ViewerQualificationProfile) -> anyhow::Result<()> {
    ep01_selection::validate_committed_authority()?;
    if profile.schema != PROFILE_SCHEMA {
        bail!("viewer qualification profile schema must be {PROFILE_SCHEMA:?}")
    }
    require_text(&profile.hardware_class, 64, "hardware_class")?;
    require_sha256(
        &profile.ep01_selection_authority_sha256,
        "ep01_selection_authority_sha256",
    )?;
    if profile.ep01_selection_authority_sha256 != ep01_selection::authority_fingerprint_sha256() {
        bail!("viewer qualification profile does not bind the committed EP-01 selection authority")
    }
    validate_build(&profile.build)?;
    validate_workload(&profile.workload)?;
    validate_host(&profile.host)?;
    validate_graphics(&profile.graphics)?;
    validate_display(&profile.display)?;
    validate_protocol(&profile.protocol)?;
    validate_extents(&profile.extents)?;
    validate_resources(&profile.resources, &profile.graphics)?;
    validate_gates(&profile.absolute_gates)?;
    Ok(())
}

fn validate_owner_accepted_profile(profile: &ViewerQualificationProfile) -> anyhow::Result<()> {
    validate_profile(profile)?;
    let expected = owner_accepted_profile_contract_sha256()?;
    validate_profile_contract(profile, &expected)
}

fn exact_release_build_binding_reason_codes(
    profile: &ViewerQualificationProfile,
    repository: &RepositoryIdentity,
    provenance: &QualificationBuildProvenance,
    debug_assertions: bool,
) -> Vec<&'static str> {
    let mut reasons = qualification_build_reason_codes(provenance, repository);
    if debug_assertions {
        reasons.push("runner_build_not_release");
    }
    if repository.dirty_worktree != Some(false) {
        reasons.push("repository_worktree_dirty_or_unavailable");
    }
    if repository.commit.as_deref() != Some(profile.build.repository_revision.as_str()) {
        reasons.push("profile_repository_revision_mismatch");
    }
    if provenance.git_head != profile.build.repository_revision {
        reasons.push("xtask_revision_mismatch");
    }
    if provenance.compiler != profile.build.compiler {
        reasons.push("xtask_compiler_mismatch");
    }
    reasons.sort_unstable();
    reasons.dedup();
    reasons
}

fn require_exact_release_build_binding(
    profile: &ViewerQualificationProfile,
    repository: &RepositoryIdentity,
    provenance: &QualificationBuildProvenance,
) -> anyhow::Result<()> {
    let reasons = exact_release_build_binding_reason_codes(
        profile,
        repository,
        provenance,
        cfg!(debug_assertions),
    );
    if !reasons.is_empty() {
        bail!(
            "viewer performance command requires the exact clean immutable release build: {}",
            reasons.join(", ")
        )
    }
    Ok(())
}

fn validate_profile_contract(
    profile: &ViewerQualificationProfile,
    expected: &str,
) -> anyhow::Result<()> {
    require_sha256(expected, "owner-accepted viewer profile contract")?;
    let observed = profile_contract_sha256(profile);
    if observed != expected {
        bail!(
            "viewer qualification profile contract is not the owner-accepted repository authority (observed {observed}, expected {expected})"
        )
    }
    Ok(())
}

fn owner_accepted_profile_contract_sha256() -> anyhow::Result<String> {
    let authority = serde_json::from_slice::<ViewerProfileAuthority>(PROFILE_AUTHORITY_BYTES)
        .context("repository viewer-performance profile authority is not strict valid JSON")?;
    if authority.schema != PROFILE_AUTHORITY_SCHEMA
        || authority.schema_version != PROFILE_AUTHORITY_SCHEMA_VERSION
    {
        bail!(
            "repository viewer-performance profile authority must use schema {PROFILE_AUTHORITY_SCHEMA:?} version {PROFILE_AUTHORITY_SCHEMA_VERSION}"
        )
    }
    require_sha256(
        &authority.owner_accepted_profile_contract_sha256,
        "owner-accepted viewer profile contract",
    )?;
    Ok(authority.owner_accepted_profile_contract_sha256)
}

/// Commits to the complete owner-reviewed qualification contract while leaving
/// the immutable source revision and the private representative-package path
/// as run-local bindings. The length-prefixed domain-separated encoding avoids
/// JSON formatting dependence and normalizes the two set-valued profile lists.
fn profile_contract_sha256(profile: &ViewerQualificationProfile) -> String {
    let mut scenarios = profile.workload.scenarios.clone();
    scenarios.sort_unstable();
    let mut requested_features = profile.graphics.requested_features.clone();
    requested_features.sort_unstable();

    let mut fields = vec![
        profile.schema.clone(),
        profile.hardware_class.clone(),
        profile.ep01_selection_authority_sha256.clone(),
        profile.build.profile.clone(),
        profile.build.compiler.clone(),
        profile.build.target_mode.clone(),
        profile
            .workload
            .representative_package
            .root_manifest_sha256
            .clone(),
        profile.workload.workload_bundle_sha256.clone(),
        profile.workload.interaction_script_bundle_sha256.clone(),
        profile.workload.independent_oracle_sha256.clone(),
        scenarios.len().to_string(),
    ];
    fields.extend(scenarios);
    fields.extend([
        profile.host.os.clone(),
        profile.host.arch.clone(),
        profile.host.cpu_model.clone(),
        profile.host.logical_cpu_count.to_string(),
        profile.host.mem_total_kib.to_string(),
        profile.host.storage.filesystem_type.clone(),
        profile.host.storage.filesystem_source.clone(),
        profile.host.storage.filesystem_uuid.clone(),
        profile.host.storage.device_major_minor.clone(),
        profile.graphics.backend.clone(),
        profile.graphics.adapter_name.clone(),
        profile.graphics.vendor_id.to_string(),
        profile.graphics.device_id.to_string(),
        profile.graphics.device_type.clone(),
        profile.graphics.api_version.clone(),
        profile.graphics.driver_version.clone(),
        profile.graphics.driver_name.clone(),
        profile.graphics.driver_info.clone(),
        profile.graphics.dedicated_vram_bytes.to_string(),
        profile.graphics.wgpu_version.clone(),
        profile.graphics.naga_version.clone(),
        requested_features.len().to_string(),
    ]);
    fields.extend(requested_features);
    fields.extend([
        profile.graphics.device_memory_hint.clone(),
        profile.display.session_type.clone(),
        profile.display.compositor.clone(),
        profile.display.output_name.clone(),
        profile.display.current_mode.width.to_string(),
        profile.display.current_mode.height.to_string(),
        profile.display.physical_width_mm.to_string(),
        profile.display.physical_height_mm.to_string(),
        profile.display.refresh_millihz.to_string(),
        profile.display.compositor_scale_milli.to_string(),
        profile.display.presentation_mode.clone(),
        profile.protocol.application_cold.to_string(),
        profile.protocol.empty_product_residency.to_string(),
        profile.protocol.os_cache_condition.clone(),
        profile.protocol.competing_activity.clone(),
        profile.protocol.power_state.clone(),
        profile.protocol.automatic_retries.to_string(),
        profile.protocol.development_samples.to_string(),
        profile.extents.blocking_qualification.width.to_string(),
        profile.extents.blocking_qualification.height.to_string(),
        profile.extents.required_exercise.width.to_string(),
        profile.extents.required_exercise.height.to_string(),
        profile.resources.max_cpu_total_bytes.to_string(),
        profile
            .resources
            .max_cpu_decoded_residency_bytes
            .to_string(),
        profile.resources.max_cpu_upload_staging_bytes.to_string(),
        profile.resources.gpu_budget_bytes.to_string(),
        profile.resources.max_gpu_resident_bytes.to_string(),
        profile.resources.max_gpu_in_flight_bytes.to_string(),
        profile.resources.max_open_objects.to_string(),
        profile.resources.max_queued_requests.to_string(),
        profile
            .absolute_gates
            .resident_input_to_current_presentation_p95_ns
            .to_string(),
        profile
            .absolute_gates
            .maximum_current_presentation_gap_ns
            .to_string(),
        profile
            .absolute_gates
            .maximum_main_loop_heartbeat_gap_ns
            .to_string(),
        profile
            .absolute_gates
            .maximum_ui_thread_interaction_task_ns
            .to_string(),
        profile.absolute_gates.maximum_plane_gpu_ns.to_string(),
        profile.absolute_gates.maximum_mip_gpu_ns.to_string(),
        profile.absolute_gates.maximum_dvr_gpu_ns.to_string(),
        profile.absolute_gates.maximum_iso_gpu_ns.to_string(),
        profile.absolute_gates.cold_first_useful_ns.to_string(),
        profile.absolute_gates.cold_complete_coarse_ns.to_string(),
        profile.absolute_gates.cold_target_settlement_ns.to_string(),
        profile
            .absolute_gates
            .nonresident_target_settlement_ns
            .to_string(),
        profile
            .absolute_gates
            .source_verification_completion_ns
            .to_string(),
        profile
            .absolute_gates
            .maximum_instrumentation_overhead_basis_points
            .to_string(),
    ]);
    fingerprint(b"mirante4d-viewer-profile-contract-1\0", &fields)
}

fn validate_build(build: &BuildBinding) -> anyhow::Result<()> {
    if !valid_git_object_id(&build.repository_revision) {
        bail!("build.repository_revision must be a full lowercase Git object ID")
    }
    if build.profile != "release" {
        bail!("build.profile must be release")
    }
    require_text(&build.compiler, 8 * 1024, "build.compiler")?;
    if !build.compiler.starts_with("rustc ")
        || !build
            .compiler
            .lines()
            .any(|line| line.starts_with("host: "))
    {
        bail!("build.compiler must be exact rustc --version --verbose output")
    }
    if build.target_mode != "fresh-private-target" {
        bail!("build.target_mode must be fresh-private-target")
    }
    Ok(())
}

fn valid_git_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_workload(workload: &WorkloadBinding) -> anyhow::Result<()> {
    if !workload.representative_package.root.is_absolute() {
        bail!("representative_package.root must be absolute")
    }
    require_sha256(
        &workload.representative_package.root_manifest_sha256,
        "representative_package.root_manifest_sha256",
    )?;
    require_sha256(&workload.workload_bundle_sha256, "workload_bundle_sha256")?;
    require_sha256(
        &workload.interaction_script_bundle_sha256,
        "interaction_script_bundle_sha256",
    )?;
    require_sha256(
        &workload.independent_oracle_sha256,
        "independent_oracle_sha256",
    )?;
    let expected = REQUIRED_SCENARIOS.into_iter().collect::<BTreeSet<_>>();
    let observed = workload
        .scenarios
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if workload.scenarios.len() != REQUIRED_SCENARIOS.len() || observed != expected {
        bail!("workload scenarios must contain RZ,ZB,RO,ST,NO,FC,VM,PT,VV,IP exactly once")
    }
    Ok(())
}

fn validate_host(host: &HostBinding) -> anyhow::Result<()> {
    if host.os != "linux" || host.arch != "x86_64" {
        bail!("viewer qualification host must be linux/x86_64")
    }
    require_text(&host.cpu_model, 256, "host.cpu_model")?;
    if host.logical_cpu_count == 0 || host.mem_total_kib == 0 {
        bail!("host CPU count and memory must be nonzero")
    }
    require_text(
        &host.storage.filesystem_type,
        64,
        "host.storage.filesystem_type",
    )?;
    require_text(
        &host.storage.filesystem_source,
        1024,
        "host.storage.filesystem_source",
    )?;
    require_text(
        &host.storage.filesystem_uuid,
        256,
        "host.storage.filesystem_uuid",
    )?;
    if !valid_major_minor(&host.storage.device_major_minor) {
        bail!("host.storage.device_major_minor must use unsigned MAJOR:MINOR syntax")
    }
    Ok(())
}

fn validate_graphics(graphics: &GraphicsBinding) -> anyhow::Result<()> {
    if graphics.backend != "vulkan" {
        bail!("graphics.backend must be vulkan")
    }
    for (value, label) in [
        (&graphics.adapter_name, "graphics.adapter_name"),
        (&graphics.device_type, "graphics.device_type"),
        (&graphics.api_version, "graphics.api_version"),
        (&graphics.driver_version, "graphics.driver_version"),
        (&graphics.driver_name, "graphics.driver_name"),
        (&graphics.driver_info, "graphics.driver_info"),
        (&graphics.wgpu_version, "graphics.wgpu_version"),
        (&graphics.naga_version, "graphics.naga_version"),
        (&graphics.device_memory_hint, "graphics.device_memory_hint"),
    ] {
        require_text(value, 256, label)?;
    }
    if graphics.vendor_id == 0 || graphics.device_id == 0 || graphics.dedicated_vram_bytes == 0 {
        bail!("graphics identifiers and dedicated VRAM must be nonzero")
    }
    if graphics.device_memory_hint != "MemoryUsage" {
        bail!("graphics.device_memory_hint must bind wgpu MemoryUsage")
    }
    if graphics.requested_features.is_empty() || graphics.requested_features.len() > 64 {
        bail!("graphics.requested_features must contain between 1 and 64 entries")
    }
    let mut unique = BTreeSet::new();
    for feature in &graphics.requested_features {
        require_text(feature, 128, "graphics.requested_features entry")?;
        if !unique.insert(feature) {
            bail!("graphics.requested_features must not contain duplicates")
        }
    }
    Ok(())
}

fn validate_display(display: &DisplayBinding) -> anyhow::Result<()> {
    if display.session_type != "x11" {
        bail!("display.session_type must be x11 for the accepted Linux qualification profile")
    }
    for (value, label) in [
        (&display.compositor, "display.compositor"),
        (&display.output_name, "display.output_name"),
        (&display.presentation_mode, "display.presentation_mode"),
    ] {
        require_text(value, 128, label)?;
    }
    if display.presentation_mode != "fifo" {
        bail!("display.presentation_mode must bind the configured fifo product mode")
    }
    if display.current_mode.width == 0
        || display.current_mode.height == 0
        || display.physical_width_mm == 0
        || display.physical_height_mm == 0
        || display.refresh_millihz == 0
        || display.compositor_scale_milli == 0
        || display.compositor_scale_milli > 4000
    {
        bail!("display geometry, refresh, and scale must be finite nonzero bounded integers")
    }
    Ok(())
}

fn validate_protocol(protocol: &ProtocolBinding) -> anyhow::Result<()> {
    if !protocol.application_cold || !protocol.empty_product_residency {
        bail!("protocol must require a fresh process with empty Mirante CPU/GPU residency")
    }
    if !matches!(protocol.os_cache_condition.as_str(), "warm" | "cold") {
        bail!("protocol.os_cache_condition must be warm or cold")
    }
    require_controlled_text(
        &protocol.competing_activity,
        128,
        "protocol.competing_activity",
    )?;
    require_controlled_text(&protocol.power_state, 128, "protocol.power_state")?;
    if protocol.automatic_retries != 0 {
        bail!("protocol.automatic_retries must be zero")
    }
    if protocol.development_samples != 3 {
        bail!("protocol.development_samples must be exactly three")
    }
    Ok(())
}

fn validate_extents(extents: &ExtentBinding) -> anyhow::Result<()> {
    if extents.blocking_qualification
        != (PixelExtent {
            width: BLOCKING_WIDTH,
            height: BLOCKING_HEIGHT,
        })
        || extents.required_exercise
            != (PixelExtent {
                width: EXERCISE_WIDTH,
                height: EXERCISE_HEIGHT,
            })
    {
        bail!("viewer extents must bind blocking 1280x720 and required exercise 1920x1080")
    }
    Ok(())
}

fn validate_resources(
    resources: &ResourceBinding,
    graphics: &GraphicsBinding,
) -> anyhow::Result<()> {
    const GIB: u64 = 1024 * 1024 * 1024;
    if resources.max_cpu_total_bytes == 0
        || resources.max_cpu_decoded_residency_bytes == 0
        || resources.max_cpu_upload_staging_bytes == 0
        || resources.gpu_budget_bytes == 0
        || resources.max_gpu_resident_bytes == 0
        || resources.max_gpu_in_flight_bytes == 0
        || resources.max_open_objects == 0
        || resources.max_queued_requests == 0
    {
        bail!("all viewer resource budgets must be nonzero")
    }
    if !(2 * GIB..=32 * GIB).contains(&resources.max_cpu_total_bytes)
        || !(GIB..=8 * GIB).contains(&resources.gpu_budget_bytes)
    {
        bail!("viewer total CPU/GPU budgets must be valid Mirante settings resource policies")
    }
    if resources.max_cpu_decoded_residency_bytes > resources.max_cpu_total_bytes
        || resources.max_cpu_upload_staging_bytes > resources.max_cpu_total_bytes
        || resources.gpu_budget_bytes > graphics.dedicated_vram_bytes
        || resources.max_gpu_resident_bytes > resources.gpu_budget_bytes
        || resources.max_gpu_in_flight_bytes > resources.gpu_budget_bytes
        || resources
            .max_gpu_resident_bytes
            .checked_add(resources.max_gpu_in_flight_bytes)
            .is_none_or(|used| used > resources.gpu_budget_bytes)
    {
        bail!("viewer resource sub-budgets must fit their CPU/GPU authorities")
    }
    Ok(())
}

fn validate_gates(gates: &AbsoluteGates) -> anyhow::Result<()> {
    let duration_gates = [
        gates.resident_input_to_current_presentation_p95_ns,
        gates.maximum_current_presentation_gap_ns,
        gates.maximum_main_loop_heartbeat_gap_ns,
        gates.maximum_ui_thread_interaction_task_ns,
        gates.maximum_plane_gpu_ns,
        gates.maximum_mip_gpu_ns,
        gates.maximum_dvr_gpu_ns,
        gates.maximum_iso_gpu_ns,
        gates.cold_first_useful_ns,
        gates.cold_complete_coarse_ns,
        gates.cold_target_settlement_ns,
        gates.nonresident_target_settlement_ns,
        gates.source_verification_completion_ns,
    ];
    if duration_gates.contains(&0) {
        bail!("every absolute duration gate must be nonzero")
    }
    if gates.resident_input_to_current_presentation_p95_ns
        > gates.maximum_current_presentation_gap_ns
        || gates.maximum_main_loop_heartbeat_gap_ns > gates.maximum_current_presentation_gap_ns
        || gates.maximum_ui_thread_interaction_task_ns
            > gates.resident_input_to_current_presentation_p95_ns
        || [
            gates.maximum_plane_gpu_ns,
            gates.maximum_mip_gpu_ns,
            gates.maximum_dvr_gpu_ns,
            gates.maximum_iso_gpu_ns,
        ]
        .into_iter()
        .any(|gpu_gate| gpu_gate > gates.resident_input_to_current_presentation_p95_ns)
        || gates.cold_first_useful_ns > gates.cold_complete_coarse_ns
        || gates.cold_complete_coarse_ns > gates.cold_target_settlement_ns
    {
        bail!("absolute gate ordering is incoherent")
    }
    const MAX_LONG_VIEWER_DEADLINE_NS: u64 = 30_000_000_000;
    if [
        gates.cold_first_useful_ns,
        gates.cold_complete_coarse_ns,
        gates.cold_target_settlement_ns,
        gates.nonresident_target_settlement_ns,
        gates.source_verification_completion_ns,
    ]
    .into_iter()
    .any(|deadline| deadline > MAX_LONG_VIEWER_DEADLINE_NS)
    {
        bail!("cold, nonresident, and source-verification gates must not exceed 30 seconds")
    }
    gates
        .maximum_current_presentation_gap_ns
        .checked_mul(2)
        .context("resident presentation deadline plus one poll grace overflows")?;
    if gates.maximum_instrumentation_overhead_basis_points == 0
        || gates.maximum_instrumentation_overhead_basis_points > 10_000
    {
        bail!("instrumentation overhead gate must be between 1 and 10000 basis points")
    }
    Ok(())
}

fn require_text(value: &str, max_bytes: usize, label: &str) -> anyhow::Result<()> {
    if value.is_empty() || value.trim() != value || value.len() > max_bytes || value.contains('\0')
    {
        bail!("{label} must be a nonempty trimmed bounded string")
    }
    Ok(())
}

fn require_controlled_text(value: &str, max_bytes: usize, label: &str) -> anyhow::Result<()> {
    require_text(value, max_bytes, label)?;
    if value == "uncontrolled" || value == "unknown" {
        bail!("{label} must name a controlled exact condition")
    }
    Ok(())
}

fn require_sha256(value: &str, label: &str) -> anyhow::Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{label} must be a lowercase SHA-256 digest")
    }
    Ok(())
}

fn valid_major_minor(value: &str) -> bool {
    let Some((major, minor)) = value.split_once(':') else {
        return false;
    };
    !major.is_empty()
        && !minor.is_empty()
        && major.bytes().all(|byte| byte.is_ascii_digit())
        && minor.bytes().all(|byte| byte.is_ascii_digit())
}

fn observe(profile: &ViewerQualificationProfile, repository_root: &Path) -> PreflightObservations {
    let hardware = host_hardware_identity();
    let repository = repository_identity();
    let package_root = &profile.workload.representative_package.root;
    let package_root_manifest_sha256 =
        inspect_representative_package(package_root, repository_root).ok();
    PreflightObservations {
        repository_revision: repository.commit,
        repository_dirty_worktree: repository.dirty_worktree,
        compiler: compiler_description(),
        cpu_model: hardware.cpu_model,
        logical_cpu_count: hardware.logical_cpu_count,
        mem_total_kib: hardware.mem_total_kib,
        storage: observe_storage(package_root).ok(),
        graphics: observe_graphics(&profile.graphics.adapter_name),
        display: observe_display(&profile.display.output_name),
        power_state: command_stdout(Path::new("/usr/bin/powerprofilesctl"), &["get"])
            .or_else(|| command_stdout(Path::new("/bin/powerprofilesctl"), &["get"])),
        wgpu_version: locked_package_version(repository_root, "wgpu"),
        naga_version: locked_package_version(repository_root, "naga"),
        package_root_manifest_sha256,
    }
}

fn inspect_representative_package(
    package_root: &Path,
    repository_root: &Path,
) -> anyhow::Result<String> {
    require_nonsymlink_components(package_root, "representative package")?;
    let canonical = fs::canonicalize(package_root)
        .context("representative package is unavailable or unreadable")?;
    if canonical.starts_with(repository_root) {
        bail!("representative package must be outside the repository")
    }
    let metadata = fs::symlink_metadata(&canonical)
        .context("representative package is unavailable or unreadable")?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("representative package must be a nonsymlink directory")
    }
    let manifest = canonical.join("m4d/manifest/root.json");
    require_nonsymlink_components(&manifest, "representative package root manifest")?;
    let bytes = read_bounded_regular_file(
        &manifest,
        PACKAGE_ROOT_MANIFEST_MAX_BYTES,
        "representative package root manifest",
    )?;
    Ok(Sha256Hasher::digest(&bytes).to_string())
}

fn observe_storage(path: &Path) -> anyhow::Result<StorageObservation> {
    Ok(StorageObservation {
        filesystem_type: findmnt_value(path, "FSTYPE")?,
        filesystem_source: findmnt_value(path, "SOURCE")?,
        filesystem_uuid: findmnt_value(path, "UUID")?,
        device_major_minor: findmnt_value(path, "MAJ:MIN")?,
    })
}

fn findmnt_value(path: &Path, field: &str) -> anyhow::Result<String> {
    let findmnt = [Path::new("/usr/bin/findmnt"), Path::new("/bin/findmnt")]
        .into_iter()
        .find(|candidate| candidate.is_file())
        .context("findmnt is unavailable")?;
    let output = Command::new(findmnt)
        .args(["--noheadings", "--raw", "--output", field, "--target"])
        .arg(path)
        .output()
        .context("findmnt could not inspect representative package storage")?;
    if !output.status.success() {
        bail!("findmnt could not inspect representative package storage")
    }
    let value = String::from_utf8(output.stdout)
        .context("findmnt returned non-UTF-8 output")?
        .trim()
        .to_owned();
    require_text(&value, 1024, "observed storage identity")?;
    Ok(value)
}

fn observe_graphics(expected_adapter: &str) -> Option<GraphicsObservation> {
    let vulkaninfo = [
        Path::new("/usr/bin/vulkaninfo"),
        Path::new("/bin/vulkaninfo"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())?;
    let output = Command::new(vulkaninfo).arg("--summary").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let summary = String::from_utf8(output.stdout).ok()?;
    let mut candidates = parse_vulkan_summary(&summary)
        .into_iter()
        .filter(|candidate| candidate.adapter_name == expected_adapter);
    let mut selected = candidates.next()?;
    if candidates.next().is_some() {
        return None;
    }
    selected.dedicated_vram_bytes = observe_nvidia_vram(expected_adapter);
    Some(selected)
}

fn parse_vulkan_summary(summary: &str) -> Vec<GraphicsObservation> {
    let mut devices = Vec::new();
    let mut fields = std::collections::BTreeMap::<String, String>::new();
    let flush = |fields: &mut std::collections::BTreeMap<String, String>,
                 devices: &mut Vec<GraphicsObservation>| {
        let parsed = (|| {
            Some(GraphicsObservation {
                adapter_name: fields.remove("deviceName")?,
                vendor_id: parse_prefixed_hex(&fields.remove("vendorID")?)?,
                device_id: parse_prefixed_hex(&fields.remove("deviceID")?)?,
                device_type: fields.remove("deviceType")?,
                api_version: fields.remove("apiVersion")?,
                driver_version: fields.remove("driverVersion")?,
                driver_name: fields.remove("driverName")?,
                driver_info: fields.remove("driverInfo")?,
                dedicated_vram_bytes: None,
            })
        })();
        if let Some(parsed) = parsed {
            devices.push(parsed);
        }
        fields.clear();
    };
    let mut in_device = false;
    for line in summary.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("GPU") && trimmed.ends_with(':') {
            if in_device {
                flush(&mut fields, &mut devices);
            }
            in_device = true;
            continue;
        }
        if !in_device {
            continue;
        }
        if let Some((name, value)) = trimmed.split_once('=') {
            fields.insert(name.trim().to_owned(), value.trim().to_owned());
        }
    }
    if in_device {
        flush(&mut fields, &mut devices);
    }
    devices
}

fn parse_prefixed_hex(value: &str) -> Option<u32> {
    u32::from_str_radix(value.strip_prefix("0x")?, 16).ok()
}

fn observe_nvidia_vram(expected_adapter: &str) -> Option<u64> {
    let nvidia_smi = [
        Path::new("/usr/bin/nvidia-smi"),
        Path::new("/bin/nvidia-smi"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())?;
    let output = Command::new(nvidia_smi)
        .args([
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let mut matching = stdout.lines().filter_map(|line| {
        let (name, memory_mib) = line.split_once(',')?;
        (name.trim() == expected_adapter)
            .then(|| memory_mib.trim().parse::<u64>().ok())
            .flatten()
            .and_then(|memory_mib| memory_mib.checked_mul(1024 * 1024))
    });
    let selected = matching.next()?;
    matching.next().is_none().then_some(selected)
}

fn observe_display(expected_output: &str) -> Option<DisplayObservation> {
    let xrandr = [Path::new("/usr/bin/xrandr"), Path::new("/bin/xrandr")]
        .into_iter()
        .find(|candidate| candidate.is_file())?;
    let output = Command::new(xrandr).arg("--current").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let mut lines = stdout.lines().peekable();
    while let Some(line) = lines.next() {
        if line.starts_with(' ') {
            continue;
        }
        let mut header = line.split_whitespace();
        if header.next()? != expected_output || header.next()? != "connected" {
            continue;
        }
        let (physical_width_mm, physical_height_mm) = parse_physical_mm(line)?;
        let mut current_mode = None;
        let mut refresh_millihz = None;
        let mut supported_extents = BTreeSet::new();
        while let Some(mode_line) = lines.peek().copied() {
            if !mode_line.starts_with(' ') {
                break;
            }
            lines.next();
            let mut tokens = mode_line.split_whitespace();
            let Some((width, height)) = tokens.next().and_then(parse_extent_token) else {
                continue;
            };
            supported_extents.insert((width, height));
            for rate in tokens {
                if rate.contains('*') {
                    current_mode = Some(PixelExtent { width, height });
                    refresh_millihz = parse_refresh_millihz(rate);
                }
            }
        }
        return Some(DisplayObservation {
            session_type: env::var("XDG_SESSION_TYPE").ok(),
            compositor: observe_compositor(),
            output_name: expected_output.to_owned(),
            current_mode: current_mode?,
            physical_width_mm,
            physical_height_mm,
            refresh_millihz: refresh_millihz?,
            supported_extents,
        });
    }
    None
}

fn parse_physical_mm(line: &str) -> Option<(u32, u32)> {
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    tokens.windows(3).find_map(|window| {
        let width = window[0].strip_suffix("mm")?.parse::<u32>().ok()?;
        (window[1] == "x")
            .then(|| window[2].strip_suffix("mm")?.parse::<u32>().ok())
            .flatten()
            .map(|height| (width, height))
    })
}

fn parse_extent_token(value: &str) -> Option<(u32, u32)> {
    let (width, height) = value.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

fn parse_refresh_millihz(value: &str) -> Option<u32> {
    let value = value.trim_end_matches(['*', '+']);
    let refresh_hz = value.parse::<f64>().ok()?;
    if !refresh_hz.is_finite() || refresh_hz <= 0.0 {
        return None;
    }
    u32::try_from((refresh_hz * 1000.0).round() as u64).ok()
}

fn observe_compositor() -> Option<String> {
    let wmctrl = [Path::new("/usr/bin/wmctrl"), Path::new("/bin/wmctrl")]
        .into_iter()
        .find(|candidate| candidate.is_file())?;
    let output = Command::new(wmctrl).arg("-m").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("Name:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn locked_package_version(repository_root: &Path, package_name: &str) -> Option<String> {
    let lock = fs::read_to_string(repository_root.join("Cargo.lock")).ok()?;
    let parsed = toml::from_str::<CargoLockPackages>(&lock).ok()?;
    let mut matches = parsed
        .package
        .into_iter()
        .filter(|package| package.name == package_name)
        .map(|package| package.version);
    let version = matches.next()?;
    matches.next().is_none().then_some(version)
}

fn command_stdout(command: &Path, arguments: &[&str]) -> Option<String> {
    if !command.is_file() {
        return None;
    }
    let output = Command::new(command).args(arguments).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn compiler_description() -> Option<String> {
    let output = Command::new("rustc")
        .args(["--version", "--verbose"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn binding_reasons(
    profile: &ViewerQualificationProfile,
    attestation: &ProtocolAttestation,
    observations: &PreflightObservations,
) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    match observations.repository_revision.as_deref() {
        Some(revision) if revision == profile.build.repository_revision => {}
        Some(_) => reasons.push("repository_revision_mismatch"),
        None => reasons.push("repository_revision_unavailable"),
    }
    match observations.repository_dirty_worktree {
        Some(false) => {}
        Some(true) => reasons.push("repository_worktree_dirty"),
        None => reasons.push("repository_worktree_state_unavailable"),
    }
    match observations.compiler.as_deref() {
        Some(compiler) if compiler == profile.build.compiler => {}
        Some(_) => reasons.push("build_compiler_mismatch"),
        None => reasons.push("build_compiler_unavailable"),
    }
    match (
        observations.cpu_model.as_deref(),
        observations.logical_cpu_count,
        observations.mem_total_kib,
    ) {
        (Some(cpu), Some(logical), Some(memory)) => {
            if cpu != profile.host.cpu_model {
                reasons.push("host_cpu_model_mismatch");
            }
            if logical != profile.host.logical_cpu_count {
                reasons.push("host_logical_cpu_count_mismatch");
            }
            if memory != profile.host.mem_total_kib {
                reasons.push("host_memory_mismatch");
            }
        }
        _ => reasons.push("host_observation_unavailable"),
    }
    if env::consts::OS != profile.host.os || env::consts::ARCH != profile.host.arch {
        reasons.push("host_platform_mismatch");
    }
    match &observations.storage {
        Some(storage)
            if storage.filesystem_type == profile.host.storage.filesystem_type
                && storage.filesystem_source == profile.host.storage.filesystem_source
                && storage.filesystem_uuid == profile.host.storage.filesystem_uuid
                && storage.device_major_minor == profile.host.storage.device_major_minor => {}
        Some(_) => reasons.push("package_storage_mismatch"),
        None => reasons.push("package_storage_observation_unavailable"),
    }
    match &observations.graphics {
        Some(graphics) => {
            if graphics.adapter_name != profile.graphics.adapter_name
                || graphics.vendor_id != profile.graphics.vendor_id
                || graphics.device_id != profile.graphics.device_id
                || graphics.device_type != profile.graphics.device_type
                || graphics.api_version != profile.graphics.api_version
                || graphics.driver_version != profile.graphics.driver_version
                || graphics.driver_name != profile.graphics.driver_name
                || graphics.driver_info != profile.graphics.driver_info
            {
                reasons.push("graphics_adapter_or_driver_mismatch");
            }
            match graphics.dedicated_vram_bytes {
                Some(bytes) if bytes == profile.graphics.dedicated_vram_bytes => {}
                Some(_) => reasons.push("graphics_vram_mismatch"),
                None => reasons.push("graphics_vram_observation_unavailable"),
            }
        }
        None => reasons.push("graphics_observation_unavailable"),
    }
    match &observations.display {
        Some(display) => {
            if display.session_type.as_deref() != Some(profile.display.session_type.as_str())
                || display.compositor.as_deref() != Some(profile.display.compositor.as_str())
                || display.output_name != profile.display.output_name
                || display.current_mode != profile.display.current_mode
                || display.physical_width_mm != profile.display.physical_width_mm
                || display.physical_height_mm != profile.display.physical_height_mm
                || display.refresh_millihz != profile.display.refresh_millihz
            {
                reasons.push("display_tuple_mismatch");
            }
            if !display
                .supported_extents
                .contains(&(BLOCKING_WIDTH, BLOCKING_HEIGHT))
                || !display
                    .supported_extents
                    .contains(&(EXERCISE_WIDTH, EXERCISE_HEIGHT))
            {
                reasons.push("display_required_extents_unavailable");
            }
        }
        None => reasons.push("display_observation_unavailable"),
    }
    if observations.power_state.as_deref() != Some(profile.protocol.power_state.as_str()) {
        reasons.push("observed_power_state_mismatch_or_unavailable");
    }
    if attestation.cache_condition != profile.protocol.os_cache_condition {
        reasons.push("cache_condition_attestation_mismatch");
    }
    if attestation.competing_activity != profile.protocol.competing_activity {
        reasons.push("competing_activity_attestation_mismatch");
    }
    if attestation.power_state != profile.protocol.power_state {
        reasons.push("power_state_attestation_mismatch");
    }
    if attestation.compositor_scale_milli != profile.display.compositor_scale_milli {
        reasons.push("compositor_scale_attestation_mismatch");
    }
    if observations.wgpu_version.as_deref() != Some(profile.graphics.wgpu_version.as_str()) {
        reasons.push("wgpu_version_mismatch_or_unavailable");
    }
    if observations.naga_version.as_deref() != Some(profile.graphics.naga_version.as_str()) {
        reasons.push("naga_version_mismatch_or_unavailable");
    }
    match observations.package_root_manifest_sha256.as_deref() {
        Some(digest) if digest == profile.workload.representative_package.root_manifest_sha256 => {}
        Some(_) => reasons.push("representative_package_manifest_mismatch"),
        None => reasons.push("representative_package_unavailable_or_unsafe"),
    }
    reasons
}

fn sanitized_report(
    loaded: &LoadedProfile,
    bundle_commitments: &runner::BundleCommitments,
    observations: &PreflightObservations,
    reasons: &[&str],
) -> Value {
    let profile = &loaded.profile;
    json!({
        "schema": REPORT_SCHEMA,
        "binding_status": if reasons.is_empty() { "matched" } else { "mismatch" },
        "reason_codes": reasons,
        "claim_status": "diagnostic_preflight_only_no_performance_claim",
        "profile": {
            "schema": PROFILE_SCHEMA,
            "sha256": loaded.sha256,
            "owner_accepted_contract_sha256": profile_contract_sha256(profile),
            "ep01_selection_authority_sha256": profile.ep01_selection_authority_sha256,
            "maximum_bytes": PROFILE_MAX_BYTES,
            "external_nonsymlink_required": true,
        },
        "validated_bundle_commitments": bundle_commitments,
        "bindings": {
            "hardware_class": profile.hardware_class,
            "required_scenarios": REQUIRED_SCENARIOS,
            "extents": profile.extents,
            "absolute_gates": profile.absolute_gates,
            "profile_build_fingerprint_sha256": build_binding_fingerprint(&profile.build),
            "observed_build_fingerprint_sha256": observed_build_fingerprint(observations),
            "profile_host_fingerprint_sha256": host_binding_fingerprint(&profile.host),
            "observed_host_fingerprint_sha256": observed_host_fingerprint(observations),
            "profile_storage_fingerprint_sha256": storage_binding_fingerprint(&profile.host.storage),
            "observed_storage_fingerprint_sha256": observations.storage.as_ref().map(storage_observation_fingerprint),
            "profile_graphics_fingerprint_sha256": graphics_binding_fingerprint(&profile.graphics),
            "observed_graphics_fingerprint_sha256": observations.graphics.as_ref().map(graphics_observation_fingerprint),
            "graphics_contract_fingerprint_sha256": graphics_contract_fingerprint(&profile.graphics),
            "profile_display_fingerprint_sha256": display_binding_fingerprint(&profile.display),
            "observed_display_fingerprint_sha256": observations.display.as_ref().map(display_observation_fingerprint),
            "display_contract_fingerprint_sha256": display_contract_fingerprint(&profile.display),
            "representative_package_fingerprint_sha256": package_binding_fingerprint(&profile.workload.representative_package),
            "observed_representative_package_fingerprint_sha256": observations.package_root_manifest_sha256.as_deref().map(|digest| commitment_fingerprint("representative-package", digest)),
            "workload_bundle_fingerprint_sha256": commitment_fingerprint("workload-bundle", &profile.workload.workload_bundle_sha256),
            "interaction_script_bundle_fingerprint_sha256": commitment_fingerprint("interaction-script-bundle", &profile.workload.interaction_script_bundle_sha256),
            "independent_oracle_fingerprint_sha256": commitment_fingerprint("independent-oracle", &profile.workload.independent_oracle_sha256),
            "resource_budget_fingerprint_sha256": resource_fingerprint(&profile.resources),
            "absolute_gate_fingerprint_sha256": gate_fingerprint(&profile.absolute_gates),
            "ep01_selection_authority_fingerprint_sha256": ep01_selection::authority_fingerprint_sha256(),
        },
        "redaction": {
            "local_paths_omitted": true,
            "raw_package_metadata_omitted": true,
            "raw_host_storage_graphics_display_values_omitted": true,
        },
        "limitations": [
            "preflight does not execute or qualify the workload",
            "cache, competing activity, power state, and compositor scale include explicit operator attestations",
            "workload, interaction-script, and oracle content was loaded and validated but execution requires later evidence receipts",
        ],
    })
}

fn build_binding_fingerprint(build: &BuildBinding) -> String {
    fingerprint(
        b"mirante4d-viewer-build-binding-1\0",
        &[
            build.repository_revision.clone(),
            build.profile.clone(),
            build.compiler.clone(),
            build.target_mode.clone(),
        ],
    )
}

fn observed_build_fingerprint(observations: &PreflightObservations) -> Option<String> {
    Some(fingerprint(
        b"mirante4d-viewer-build-binding-1\0",
        &[
            observations.repository_revision.clone()?,
            "release".to_owned(),
            observations.compiler.clone()?,
            "fresh-private-target".to_owned(),
        ],
    ))
}

fn host_binding_fingerprint(host: &HostBinding) -> String {
    fingerprint(
        b"mirante4d-viewer-host-binding-1\0",
        &[
            host.os.clone(),
            host.arch.clone(),
            host.cpu_model.clone(),
            host.logical_cpu_count.to_string(),
            host.mem_total_kib.to_string(),
        ],
    )
}

fn observed_host_fingerprint(observations: &PreflightObservations) -> Option<String> {
    Some(fingerprint(
        b"mirante4d-viewer-host-binding-1\0",
        &[
            env::consts::OS.to_owned(),
            env::consts::ARCH.to_owned(),
            observations.cpu_model.clone()?,
            observations.logical_cpu_count?.to_string(),
            observations.mem_total_kib?.to_string(),
        ],
    ))
}

fn storage_binding_fingerprint(storage: &StorageBinding) -> String {
    fingerprint(
        b"mirante4d-viewer-storage-binding-1\0",
        &[
            storage.filesystem_type.clone(),
            storage.filesystem_source.clone(),
            storage.filesystem_uuid.clone(),
            storage.device_major_minor.clone(),
        ],
    )
}

fn storage_observation_fingerprint(storage: &StorageObservation) -> String {
    fingerprint(
        b"mirante4d-viewer-storage-binding-1\0",
        &[
            storage.filesystem_type.clone(),
            storage.filesystem_source.clone(),
            storage.filesystem_uuid.clone(),
            storage.device_major_minor.clone(),
        ],
    )
}

fn graphics_binding_fingerprint(graphics: &GraphicsBinding) -> String {
    fingerprint(
        b"mirante4d-viewer-graphics-binding-1\0",
        &[
            graphics.adapter_name.clone(),
            graphics.vendor_id.to_string(),
            graphics.device_id.to_string(),
            graphics.device_type.clone(),
            graphics.api_version.clone(),
            graphics.driver_version.clone(),
            graphics.driver_name.clone(),
            graphics.driver_info.clone(),
            graphics.dedicated_vram_bytes.to_string(),
        ],
    )
}

fn graphics_contract_fingerprint(graphics: &GraphicsBinding) -> String {
    let mut fields = vec![
        graphics.backend.clone(),
        graphics.adapter_name.clone(),
        graphics.vendor_id.to_string(),
        graphics.device_id.to_string(),
        graphics.device_type.clone(),
        graphics.api_version.clone(),
        graphics.driver_version.clone(),
        graphics.driver_name.clone(),
        graphics.driver_info.clone(),
        graphics.dedicated_vram_bytes.to_string(),
        graphics.wgpu_version.clone(),
        graphics.naga_version.clone(),
        graphics.device_memory_hint.clone(),
    ];
    fields.extend(graphics.requested_features.iter().cloned());
    fingerprint(b"mirante4d-viewer-graphics-contract-1\0", &fields)
}

fn graphics_observation_fingerprint(graphics: &GraphicsObservation) -> String {
    fingerprint(
        b"mirante4d-viewer-graphics-binding-1\0",
        &[
            graphics.adapter_name.clone(),
            graphics.vendor_id.to_string(),
            graphics.device_id.to_string(),
            graphics.device_type.clone(),
            graphics.api_version.clone(),
            graphics.driver_version.clone(),
            graphics.driver_name.clone(),
            graphics.driver_info.clone(),
            graphics
                .dedicated_vram_bytes
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unavailable".to_owned()),
        ],
    )
}

fn display_binding_fingerprint(display: &DisplayBinding) -> String {
    fingerprint(
        b"mirante4d-viewer-display-binding-1\0",
        &[
            display.session_type.clone(),
            display.compositor.clone(),
            display.output_name.clone(),
            display.current_mode.width.to_string(),
            display.current_mode.height.to_string(),
            display.physical_width_mm.to_string(),
            display.physical_height_mm.to_string(),
            display.refresh_millihz.to_string(),
        ],
    )
}

fn display_contract_fingerprint(display: &DisplayBinding) -> String {
    fingerprint(
        b"mirante4d-viewer-display-contract-1\0",
        &[
            display_binding_fingerprint(display),
            display.compositor_scale_milli.to_string(),
            display.presentation_mode.clone(),
        ],
    )
}

fn display_observation_fingerprint(display: &DisplayObservation) -> String {
    fingerprint(
        b"mirante4d-viewer-display-binding-1\0",
        &[
            display
                .session_type
                .clone()
                .unwrap_or_else(|| "unavailable".to_owned()),
            display
                .compositor
                .clone()
                .unwrap_or_else(|| "unavailable".to_owned()),
            display.output_name.clone(),
            display.current_mode.width.to_string(),
            display.current_mode.height.to_string(),
            display.physical_width_mm.to_string(),
            display.physical_height_mm.to_string(),
            display.refresh_millihz.to_string(),
        ],
    )
}

fn package_binding_fingerprint(package: &RepresentativePackageBinding) -> String {
    commitment_fingerprint("representative-package", &package.root_manifest_sha256)
}

fn commitment_fingerprint(role: &str, digest: &str) -> String {
    fingerprint(
        b"mirante4d-viewer-opaque-commitment-1\0",
        &[role.to_owned(), digest.to_owned()],
    )
}

fn resource_fingerprint(resources: &ResourceBinding) -> String {
    fingerprint(
        b"mirante4d-viewer-resource-binding-1\0",
        &[
            resources.max_cpu_total_bytes.to_string(),
            resources.max_cpu_decoded_residency_bytes.to_string(),
            resources.max_cpu_upload_staging_bytes.to_string(),
            resources.gpu_budget_bytes.to_string(),
            resources.max_gpu_resident_bytes.to_string(),
            resources.max_gpu_in_flight_bytes.to_string(),
            resources.max_open_objects.to_string(),
            resources.max_queued_requests.to_string(),
        ],
    )
}

fn gate_fingerprint(gates: &AbsoluteGates) -> String {
    fingerprint(
        b"mirante4d-viewer-absolute-gate-binding-2\0",
        &[
            gates
                .resident_input_to_current_presentation_p95_ns
                .to_string(),
            gates.maximum_current_presentation_gap_ns.to_string(),
            gates.maximum_main_loop_heartbeat_gap_ns.to_string(),
            gates.maximum_ui_thread_interaction_task_ns.to_string(),
            gates.maximum_plane_gpu_ns.to_string(),
            gates.maximum_mip_gpu_ns.to_string(),
            gates.maximum_dvr_gpu_ns.to_string(),
            gates.maximum_iso_gpu_ns.to_string(),
            gates.cold_first_useful_ns.to_string(),
            gates.cold_complete_coarse_ns.to_string(),
            gates.cold_target_settlement_ns.to_string(),
            gates.nonresident_target_settlement_ns.to_string(),
            gates.source_verification_completion_ns.to_string(),
            gates
                .maximum_instrumentation_overhead_basis_points
                .to_string(),
        ],
    )
}

fn fingerprint(domain: &[u8], fields: &[String]) -> String {
    let mut hasher = Sha256Hasher::new();
    hasher.update(domain);
    for field in fields {
        hasher.update(
            u64::try_from(field.len())
                .expect("an in-memory fingerprint field length fits u64")
                .to_le_bytes(),
        );
        hasher.update(field.as_bytes());
    }
    hasher.finalize().to_string()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn matching_build_binding(
        profile: &ViewerQualificationProfile,
    ) -> (RepositoryIdentity, QualificationBuildProvenance) {
        (
            RepositoryIdentity {
                root: Some(PathBuf::from("/repository")),
                commit: Some(profile.build.repository_revision.clone()),
                dirty_worktree: Some(false),
            },
            QualificationBuildProvenance {
                git_head: profile.build.repository_revision.clone(),
                git_dirty: false,
                cargo_profile: "release".to_owned(),
                opt_level: "3".to_owned(),
                debug: "false".to_owned(),
                compiler: profile.build.compiler.clone(),
                custom_rustflags: false,
                rustc_wrapper: false,
            },
        )
    }

    #[test]
    fn preflight_and_runner_share_the_exact_clean_release_build_gate() {
        let profile: ViewerQualificationProfile =
            serde_json::from_value(test_profile_value(Path::new("/private"))).unwrap();
        let (repository, provenance) = matching_build_binding(&profile);
        assert!(
            exact_release_build_binding_reason_codes(&profile, &repository, &provenance, false,)
                .is_empty()
        );

        let mut stale = provenance.clone();
        stale.git_head = "f".repeat(40);
        assert!(
            exact_release_build_binding_reason_codes(&profile, &repository, &stale, false)
                .contains(&"xtask_revision_mismatch")
        );
        assert!(
            exact_release_build_binding_reason_codes(&profile, &repository, &provenance, true)
                .contains(&"runner_build_not_release")
        );
    }

    #[test]
    fn argument_parser_requires_explicit_protocol_attestation() {
        let parsed = parse_args(
            [
                "--qualification-profile",
                "/private/viewer.json",
                "--workload-bundle",
                "/private/workload.json",
                "--interaction-script-bundle",
                "/private/scripts.json",
                "--independent-oracle",
                "/private/oracle.json",
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
            .collect(),
        )
        .unwrap();
        assert_eq!(parsed.profile, Path::new("/private/viewer.json"));
        assert_eq!(parsed.attestation.cache_condition, "warm");
        assert_eq!(parsed.attestation.compositor_scale_milli, 1000);

        let error = parse_args(
            [
                "--qualification-profile",
                "/private/viewer.json",
                "--workload-bundle",
                "/private/workload.json",
                "--interaction-script-bundle",
                "/private/scripts.json",
                "--independent-oracle",
                "/private/oracle.json",
            ]
            .into_iter()
            .map(ToOwned::to_owned)
            .collect(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("--cache-condition is required"));
    }

    #[test]
    fn external_profile_is_strict_bounded_and_rejects_symlinks() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("repository");
        let outside = temporary.path().join("private");
        fs::create_dir(&repository).unwrap();
        fs::create_dir(&outside).unwrap();
        let repository = fs::canonicalize(repository).unwrap();
        let profile_path = outside.join("profile.json");
        fs::write(
            &profile_path,
            serde_json::to_vec(&test_profile_value(temporary.path())).unwrap(),
        )
        .unwrap();
        let loaded = load_external_profile(&profile_path, &repository).unwrap();
        assert_eq!(loaded.profile.schema, PROFILE_SCHEMA);

        let inside = repository.join("profile.json");
        fs::write(
            &inside,
            serde_json::to_vec(&test_profile_value(temporary.path())).unwrap(),
        )
        .unwrap();
        assert!(
            load_external_profile(&inside, &repository)
                .unwrap_err()
                .to_string()
                .contains("outside the repository")
        );

        let linked = outside.join("linked.json");
        symlink(&profile_path, &linked).unwrap();
        assert!(
            load_external_profile(&linked, &repository)
                .unwrap_err()
                .to_string()
                .contains("symbolic links")
        );

        let oversized = outside.join("oversized.json");
        fs::write(
            &oversized,
            vec![b' '; usize::try_from(PROFILE_MAX_BYTES + 1).unwrap()],
        )
        .unwrap();
        assert!(
            load_external_profile(&oversized, &repository)
                .unwrap_err()
                .to_string()
                .contains("exceeds")
        );

        let mut unknown = test_profile_value(temporary.path());
        unknown["unexpected"] = json!(true);
        let unknown_path = outside.join("unknown.json");
        fs::write(&unknown_path, serde_json::to_vec(&unknown).unwrap()).unwrap();
        assert!(
            load_external_profile(&unknown_path, &repository)
                .unwrap_err()
                .to_string()
                .contains("strict valid JSON")
        );

        let mut predecessor = test_profile_value(temporary.path());
        predecessor["schema"] = json!("mirante4d-viewer-performance-qualification-profile-4");
        let predecessor: ViewerQualificationProfile = serde_json::from_value(predecessor).unwrap();
        assert!(validate_profile(&predecessor).is_err());

        let mut missing_authority = test_profile_value(temporary.path());
        missing_authority
            .as_object_mut()
            .unwrap()
            .remove("ep01_selection_authority_sha256");
        assert!(serde_json::from_value::<ViewerQualificationProfile>(missing_authority).is_err());
    }

    #[test]
    fn profile_validation_freezes_scenarios_extents_resources_and_gates() {
        let mut value = test_profile_value(Path::new("/private"));
        let profile: ViewerQualificationProfile = serde_json::from_value(value.clone()).unwrap();
        validate_profile(&profile).unwrap();

        value["workload"]["scenarios"] = json!(["RZ", "ZB"]);
        let profile: ViewerQualificationProfile = serde_json::from_value(value.clone()).unwrap();
        assert!(
            validate_profile(&profile)
                .unwrap_err()
                .to_string()
                .contains("scenarios")
        );

        value = test_profile_value(Path::new("/private"));
        value["build"]["repository_revision"] = json!("ABC");
        let profile: ViewerQualificationProfile = serde_json::from_value(value.clone()).unwrap();
        assert!(
            validate_profile(&profile)
                .unwrap_err()
                .to_string()
                .contains("Git object ID")
        );

        value = test_profile_value(Path::new("/private"));
        value["ep01_selection_authority_sha256"] = json!("f".repeat(64));
        let profile: ViewerQualificationProfile = serde_json::from_value(value.clone()).unwrap();
        assert!(
            validate_profile(&profile)
                .unwrap_err()
                .to_string()
                .contains("committed EP-01 selection authority")
        );

        value = test_profile_value(Path::new("/private"));
        value["build"]["profile"] = json!("dev");
        let profile: ViewerQualificationProfile = serde_json::from_value(value.clone()).unwrap();
        assert!(
            validate_profile(&profile)
                .unwrap_err()
                .to_string()
                .contains("must be release")
        );

        for samples in [1, 2, 4, 5] {
            value = test_profile_value(Path::new("/private"));
            value["protocol"]["development_samples"] = json!(samples);
            let profile: ViewerQualificationProfile =
                serde_json::from_value(value.clone()).unwrap();
            assert!(
                validate_profile(&profile)
                    .unwrap_err()
                    .to_string()
                    .contains("exactly three")
            );
        }

        value = test_profile_value(Path::new("/private"));
        value["extents"]["blocking_qualification"]["width"] = json!(1279);
        let profile: ViewerQualificationProfile = serde_json::from_value(value.clone()).unwrap();
        assert!(
            validate_profile(&profile)
                .unwrap_err()
                .to_string()
                .contains("1280x720")
        );

        value = test_profile_value(Path::new("/private"));
        value["absolute_gates"]["cold_complete_coarse_ns"] = json!(2_000_000_000_u64);
        value["absolute_gates"]["cold_target_settlement_ns"] = json!(1_000_000_000_u64);
        let profile: ViewerQualificationProfile = serde_json::from_value(value).unwrap();
        assert!(
            validate_profile(&profile)
                .unwrap_err()
                .to_string()
                .contains("ordering")
        );
    }

    #[test]
    fn profile_contract_excludes_only_revision_and_private_representative_root() {
        let value = test_profile_value(Path::new("/private/representative-one.m4d"));
        let profile: ViewerQualificationProfile = serde_json::from_value(value.clone()).unwrap();
        let expected = profile_contract_sha256(&profile);

        let mut run_local = value.clone();
        run_local["build"]["repository_revision"] = json!("f".repeat(40));
        run_local["workload"]["representative_package"]["root"] =
            json!("/another/private/location/representative-two.m4d");
        let run_local: ViewerQualificationProfile = serde_json::from_value(run_local).unwrap();
        assert_eq!(profile_contract_sha256(&run_local), expected);

        let mutations = [
            ("/schema", json!("changed-schema")),
            ("/hardware_class", json!("changed-hardware")),
            ("/ep01_selection_authority_sha256", json!("e".repeat(64))),
            ("/build/profile", json!("changed-profile")),
            ("/build/compiler", json!("changed-compiler")),
            ("/build/target_mode", json!("changed-target")),
            (
                "/workload/representative_package/root_manifest_sha256",
                json!("e".repeat(64)),
            ),
            ("/workload/workload_bundle_sha256", json!("e".repeat(64))),
            (
                "/workload/interaction_script_bundle_sha256",
                json!("e".repeat(64)),
            ),
            ("/workload/independent_oracle_sha256", json!("e".repeat(64))),
            ("/workload/scenarios/0", json!("changed-scenario")),
            ("/host/os", json!("changed-os")),
            ("/host/arch", json!("changed-arch")),
            ("/host/cpu_model", json!("changed-cpu")),
            ("/host/logical_cpu_count", json!(17)),
            ("/host/mem_total_kib", json!(16_000_001)),
            ("/host/storage/filesystem_type", json!("changed-fs")),
            ("/host/storage/filesystem_source", json!("changed-source")),
            ("/host/storage/filesystem_uuid", json!("changed-uuid")),
            ("/host/storage/device_major_minor", json!("changed-device")),
            ("/graphics/backend", json!("changed-backend")),
            ("/graphics/adapter_name", json!("changed-adapter")),
            ("/graphics/vendor_id", json!(4319)),
            ("/graphics/device_id", json!(9441)),
            ("/graphics/device_type", json!("changed-type")),
            ("/graphics/api_version", json!("changed-api")),
            ("/graphics/driver_version", json!("changed-version")),
            ("/graphics/driver_name", json!("changed-driver")),
            ("/graphics/driver_info", json!("changed-info")),
            ("/graphics/dedicated_vram_bytes", json!(8_589_934_593_u64)),
            ("/graphics/wgpu_version", json!("changed-wgpu")),
            ("/graphics/naga_version", json!("changed-naga")),
            ("/graphics/requested_features/0", json!("CHANGED_FEATURE")),
            ("/graphics/device_memory_hint", json!("changed-memory-hint")),
            ("/display/session_type", json!("changed-session")),
            ("/display/compositor", json!("changed-compositor")),
            ("/display/output_name", json!("changed-output")),
            ("/display/current_mode/width", json!(1921)),
            ("/display/current_mode/height", json!(1081)),
            ("/display/physical_width_mm", json!(481)),
            ("/display/physical_height_mm", json!(271)),
            ("/display/refresh_millihz", json!(60_001)),
            ("/display/compositor_scale_milli", json!(1001)),
            ("/display/presentation_mode", json!("changed-present-mode")),
            ("/protocol/application_cold", json!(false)),
            ("/protocol/empty_product_residency", json!(false)),
            ("/protocol/os_cache_condition", json!("changed-cache")),
            ("/protocol/competing_activity", json!("changed-activity")),
            ("/protocol/power_state", json!("changed-power")),
            ("/protocol/automatic_retries", json!(1)),
            ("/protocol/development_samples", json!(4)),
            ("/extents/blocking_qualification/width", json!(1281)),
            ("/extents/blocking_qualification/height", json!(721)),
            ("/extents/required_exercise/width", json!(1921)),
            ("/extents/required_exercise/height", json!(1081)),
            ("/resources/max_cpu_total_bytes", json!(4_294_967_297_u64)),
            (
                "/resources/max_cpu_decoded_residency_bytes",
                json!(2_147_483_649_u64),
            ),
            (
                "/resources/max_cpu_upload_staging_bytes",
                json!(536_870_913_u64),
            ),
            ("/resources/gpu_budget_bytes", json!(4_294_967_297_u64)),
            (
                "/resources/max_gpu_resident_bytes",
                json!(3_221_225_473_u64),
            ),
            ("/resources/max_gpu_in_flight_bytes", json!(429_496_730_u64)),
            ("/resources/max_open_objects", json!(65)),
            ("/resources/max_queued_requests", json!(1025)),
            (
                "/absolute_gates/resident_input_to_current_presentation_p95_ns",
                json!(16_666_668_u64),
            ),
            (
                "/absolute_gates/maximum_current_presentation_gap_ns",
                json!(33_333_335_u64),
            ),
            (
                "/absolute_gates/maximum_main_loop_heartbeat_gap_ns",
                json!(33_333_335_u64),
            ),
            (
                "/absolute_gates/maximum_ui_thread_interaction_task_ns",
                json!(2_000_001_u64),
            ),
            (
                "/absolute_gates/maximum_plane_gpu_ns",
                json!(16_666_668_u64),
            ),
            ("/absolute_gates/maximum_mip_gpu_ns", json!(16_666_668_u64)),
            ("/absolute_gates/maximum_dvr_gpu_ns", json!(16_666_668_u64)),
            ("/absolute_gates/maximum_iso_gpu_ns", json!(16_666_668_u64)),
            (
                "/absolute_gates/cold_first_useful_ns",
                json!(100_000_001_u64),
            ),
            (
                "/absolute_gates/cold_complete_coarse_ns",
                json!(250_000_001_u64),
            ),
            (
                "/absolute_gates/cold_target_settlement_ns",
                json!(2_000_000_001_u64),
            ),
            (
                "/absolute_gates/nonresident_target_settlement_ns",
                json!(1_000_000_001_u64),
            ),
            (
                "/absolute_gates/source_verification_completion_ns",
                json!(2_000_000_001_u64),
            ),
            (
                "/absolute_gates/maximum_instrumentation_overhead_basis_points",
                json!(201),
            ),
        ];
        for (pointer, replacement) in mutations {
            let mut mutated = value.clone();
            *mutated
                .pointer_mut(pointer)
                .unwrap_or_else(|| panic!("missing test profile pointer {pointer}")) = replacement;
            let mutated: ViewerQualificationProfile = serde_json::from_value(mutated).unwrap();
            assert_ne!(
                profile_contract_sha256(&mutated),
                expected,
                "profile contract omitted {pointer}"
            );
        }
    }

    #[test]
    fn profile_contract_canonicalizes_set_valued_lists_and_authority_is_strict() {
        let mut first = test_profile_value(Path::new("/private/package.m4d"));
        first["graphics"]["requested_features"] = json!(["B", "A"]);
        let mut second = first.clone();
        second["graphics"]["requested_features"] = json!(["A", "B"]);
        second["workload"]["scenarios"]
            .as_array_mut()
            .unwrap()
            .reverse();
        let first: ViewerQualificationProfile = serde_json::from_value(first).unwrap();
        let second: ViewerQualificationProfile = serde_json::from_value(second).unwrap();
        assert_eq!(
            profile_contract_sha256(&first),
            profile_contract_sha256(&second)
        );
        let exact = profile_contract_sha256(&first);
        validate_profile_contract(&first, &exact).unwrap();
        assert!(
            validate_profile_contract(&first, &"f".repeat(64))
                .unwrap_err()
                .to_string()
                .contains("not the owner-accepted repository authority")
        );

        let accepted = owner_accepted_profile_contract_sha256().unwrap();
        require_sha256(&accepted, "test owner authority").unwrap();
        assert_eq!(accepted.len(), 64);
    }

    #[test]
    fn binding_reasons_are_exact_and_report_redacts_private_values() {
        let profile: ViewerQualificationProfile =
            serde_json::from_value(test_profile_value(Path::new("/secret/package.m4d"))).unwrap();
        let observations = matching_observations(&profile);
        let attestation = matching_attestation(&profile);
        assert!(binding_reasons(&profile, &attestation, &observations).is_empty());
        let matched_report = sanitized_report(
            &LoadedProfile {
                profile: profile.clone(),
                sha256: "1".repeat(64),
            },
            &runner::BundleCommitments {
                workload_bundle_sha256: "2".repeat(64),
                interaction_script_bundle_sha256: "3".repeat(64),
                independent_oracle_sha256: "4".repeat(64),
                ep01_trace_geometry_sha256: "5".repeat(64),
            },
            &observations,
            &[],
        );
        assert_eq!(
            matched_report["bindings"]["profile_host_fingerprint_sha256"],
            matched_report["bindings"]["observed_host_fingerprint_sha256"]
        );
        assert_eq!(
            matched_report["bindings"]["profile_storage_fingerprint_sha256"],
            matched_report["bindings"]["observed_storage_fingerprint_sha256"]
        );
        assert_eq!(
            matched_report["bindings"]["profile_graphics_fingerprint_sha256"],
            matched_report["bindings"]["observed_graphics_fingerprint_sha256"]
        );
        assert_eq!(
            matched_report["bindings"]["profile_display_fingerprint_sha256"],
            matched_report["bindings"]["observed_display_fingerprint_sha256"]
        );
        assert_eq!(
            matched_report["bindings"]["profile_build_fingerprint_sha256"],
            matched_report["bindings"]["observed_build_fingerprint_sha256"]
        );
        assert_eq!(
            matched_report["bindings"]["ep01_selection_authority_fingerprint_sha256"],
            ep01_selection::authority_fingerprint_sha256()
        );
        assert_eq!(
            matched_report["validated_bundle_commitments"]["ep01_trace_geometry_sha256"],
            "5".repeat(64)
        );

        let mut mismatch = observations.clone();
        mismatch.cpu_model = Some("Other private CPU".to_owned());
        mismatch.package_root_manifest_sha256 = Some("f".repeat(64));
        mismatch.repository_dirty_worktree = Some(true);
        mismatch.compiler = Some("rustc mismatched\nhost: x86_64-unknown-linux-gnu".to_owned());
        let reasons = binding_reasons(&profile, &attestation, &mismatch);
        assert!(reasons.contains(&"host_cpu_model_mismatch"));
        assert!(reasons.contains(&"representative_package_manifest_mismatch"));
        assert!(reasons.contains(&"repository_worktree_dirty"));
        assert!(reasons.contains(&"build_compiler_mismatch"));

        let report = sanitized_report(
            &LoadedProfile {
                profile,
                sha256: "1".repeat(64),
            },
            &runner::BundleCommitments {
                workload_bundle_sha256: "2".repeat(64),
                interaction_script_bundle_sha256: "3".repeat(64),
                independent_oracle_sha256: "4".repeat(64),
                ep01_trace_geometry_sha256: "5".repeat(64),
            },
            &mismatch,
            &reasons,
        )
        .to_string();
        for private in [
            "/secret/package.m4d",
            "Private CPU",
            "Private GPU",
            "/dev/private",
            "private-uuid",
            "HDMI-PRIVATE",
            "Other private CPU",
        ] {
            assert!(!report.contains(private), "report leaked {private:?}");
        }
        assert!(report.contains("diagnostic_preflight_only_no_performance_claim"));
    }

    #[test]
    fn vulkan_and_display_parsers_extract_exact_observable_tuples() {
        let vulkan = r#"
GPU0:
    apiVersion         = 1.4.312
    driverVersion      = 580.159.3.0
    vendorID           = 0x10de
    deviceID           = 0x24e0
    deviceType         = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU
    deviceName         = Private GPU
    driverName         = NVIDIA
    driverInfo         = 580.159.03
GPU1:
    apiVersion         = 1.4.318
    driverVersion      = 25.2.8
    vendorID           = 0x1002
    deviceID           = 0x1681
    deviceType         = PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU
    deviceName         = Other GPU
    driverName         = radv
    driverInfo         = Mesa 25.2.8
"#;
        let parsed = parse_vulkan_summary(vulkan);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].adapter_name, "Private GPU");
        assert_eq!(parsed[0].vendor_id, 0x10de);
        assert_eq!(parse_extent_token("1920x1080"), Some((1920, 1080)));
        assert_eq!(parse_refresh_millihz("60.00*+"), Some(60_000));
        assert_eq!(
            parse_physical_mm("HDMI-0 connected primary 1920x1080 480mm x 270mm"),
            Some((480, 270))
        );
    }

    #[test]
    fn renderer_dependency_versions_are_observed_from_the_workspace_lockfile() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        assert_eq!(
            locked_package_version(repository, "wgpu").as_deref(),
            Some("29.0.3")
        );
        assert_eq!(
            locked_package_version(repository, "naga").as_deref(),
            Some("29.0.3")
        );
    }

    fn test_profile_value(package_root: &Path) -> Value {
        json!({
            "schema": PROFILE_SCHEMA,
            "hardware_class": "HW-2-VIEWER",
            "ep01_selection_authority_sha256": ep01_selection::authority_fingerprint_sha256(),
            "build": {
                "repository_revision": "0".repeat(40),
                "profile": "release",
                "compiler": "rustc 1.90.0 (1159e78c4 2025-09-14)\nbinary: rustc\ncommit-hash: 1159e78c4747b02ef996e55082b704c09b970588\ncommit-date: 2025-09-14\nhost: x86_64-unknown-linux-gnu\nrelease: 1.90.0\nLLVM version: 20.1.8",
                "target_mode": "fresh-private-target",
            },
            "workload": {
                "representative_package": {
                    "root": package_root,
                    "root_manifest_sha256": "a".repeat(64),
                },
                "workload_bundle_sha256": "b".repeat(64),
                "interaction_script_bundle_sha256": "c".repeat(64),
                "independent_oracle_sha256": "d".repeat(64),
                "scenarios": REQUIRED_SCENARIOS,
            },
            "host": {
                "os": "linux",
                "arch": "x86_64",
                "cpu_model": "Private CPU",
                "logical_cpu_count": 16,
                "mem_total_kib": 16_000_000,
                "storage": {
                    "filesystem_type": "ext4",
                    "filesystem_source": "/dev/private",
                    "filesystem_uuid": "private-uuid",
                    "device_major_minor": "259:5",
                },
            },
            "graphics": {
                "backend": "vulkan",
                "adapter_name": "Private GPU",
                "vendor_id": 4318,
                "device_id": 9440,
                "device_type": "PHYSICAL_DEVICE_TYPE_DISCRETE_GPU",
                "api_version": "1.4.312",
                "driver_version": "580.159.3.0",
                "driver_name": "NVIDIA",
                "driver_info": "580.159.03",
                "dedicated_vram_bytes": 8_589_934_592_u64,
                "wgpu_version": "29.0.3",
                "naga_version": "29.0.3",
                "requested_features": ["TIMESTAMP_QUERY"],
                "device_memory_hint": "MemoryUsage",
            },
            "display": {
                "session_type": "x11",
                "compositor": "Private Compositor",
                "output_name": "HDMI-PRIVATE",
                "current_mode": { "width": 1920, "height": 1080 },
                "physical_width_mm": 480,
                "physical_height_mm": 270,
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
                "max_cpu_decoded_residency_bytes": 268_435_456_u64,
                "max_cpu_upload_staging_bytes": 67_108_864_u64,
                "gpu_budget_bytes": 4_294_967_296_u64,
                "max_gpu_resident_bytes": 2_147_483_648_u64,
                "max_gpu_in_flight_bytes": 536_870_912_u64,
                "max_open_objects": 128,
                "max_queued_requests": 1024,
            },
            "absolute_gates": {
                "resident_input_to_current_presentation_p95_ns": 16_670_000,
                "maximum_current_presentation_gap_ns": 33_340_000,
                "maximum_main_loop_heartbeat_gap_ns": 33_340_000,
                "maximum_ui_thread_interaction_task_ns": 2_000_000,
                "maximum_plane_gpu_ns": 16_670_000,
                "maximum_mip_gpu_ns": 16_670_000,
                "maximum_dvr_gpu_ns": 16_670_000,
                "maximum_iso_gpu_ns": 16_670_000,
                "cold_first_useful_ns": 100_000_000,
                "cold_complete_coarse_ns": 250_000_000,
                "cold_target_settlement_ns": 2_000_000_000_u64,
                "nonresident_target_settlement_ns": 2_000_000_000_u64,
                "source_verification_completion_ns": 2_000_000_000_u64,
                "maximum_instrumentation_overhead_basis_points": 500,
            },
        })
    }

    fn matching_attestation(profile: &ViewerQualificationProfile) -> ProtocolAttestation {
        ProtocolAttestation {
            cache_condition: profile.protocol.os_cache_condition.clone(),
            competing_activity: profile.protocol.competing_activity.clone(),
            power_state: profile.protocol.power_state.clone(),
            compositor_scale_milli: profile.display.compositor_scale_milli,
        }
    }

    fn matching_observations(profile: &ViewerQualificationProfile) -> PreflightObservations {
        PreflightObservations {
            repository_revision: Some(profile.build.repository_revision.clone()),
            repository_dirty_worktree: Some(false),
            compiler: Some(profile.build.compiler.clone()),
            cpu_model: Some(profile.host.cpu_model.clone()),
            logical_cpu_count: Some(profile.host.logical_cpu_count),
            mem_total_kib: Some(profile.host.mem_total_kib),
            storage: Some(StorageObservation {
                filesystem_type: profile.host.storage.filesystem_type.clone(),
                filesystem_source: profile.host.storage.filesystem_source.clone(),
                filesystem_uuid: profile.host.storage.filesystem_uuid.clone(),
                device_major_minor: profile.host.storage.device_major_minor.clone(),
            }),
            graphics: Some(GraphicsObservation {
                adapter_name: profile.graphics.adapter_name.clone(),
                vendor_id: profile.graphics.vendor_id,
                device_id: profile.graphics.device_id,
                device_type: profile.graphics.device_type.clone(),
                api_version: profile.graphics.api_version.clone(),
                driver_version: profile.graphics.driver_version.clone(),
                driver_name: profile.graphics.driver_name.clone(),
                driver_info: profile.graphics.driver_info.clone(),
                dedicated_vram_bytes: Some(profile.graphics.dedicated_vram_bytes),
            }),
            display: Some(DisplayObservation {
                session_type: Some(profile.display.session_type.clone()),
                compositor: Some(profile.display.compositor.clone()),
                output_name: profile.display.output_name.clone(),
                current_mode: profile.display.current_mode,
                physical_width_mm: profile.display.physical_width_mm,
                physical_height_mm: profile.display.physical_height_mm,
                refresh_millihz: profile.display.refresh_millihz,
                supported_extents: [(1280, 720), (1920, 1080)].into_iter().collect(),
            }),
            power_state: Some(profile.protocol.power_state.clone()),
            wgpu_version: Some(profile.graphics.wgpu_version.clone()),
            naga_version: Some(profile.graphics.naga_version.clone()),
            package_root_manifest_sha256: Some(
                profile
                    .workload
                    .representative_package
                    .root_manifest_sha256
                    .clone(),
            ),
        }
    }
}

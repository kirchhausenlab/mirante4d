use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsStr,
    fs,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use mirante4d_identity::{ScientificContentId, Sha256Digest, Sha256Hasher};
use mirante4d_import_pipeline::{TiffSource, deterministic_tiff_destination};
use mirante4d_storage::{
    LocalPackageCatalog, OmeLevelTransform, PackedIndexCoordinates, ProfileKind,
    ProfileValidityMode, ShardProfileKind,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use signal_hook::{
    SigId,
    consts::signal::{SIGINT, SIGTERM},
    flag,
};

use crate::{
    host::{
        HostHardwareIdentity, IMPORT_QUALIFICATION_HARDWARE_CLASS,
        IMPORT_QUALIFICATION_PROFILE_MAX_BYTES, IMPORT_QUALIFICATION_PROFILE_SCHEMA,
        ImportQualificationAssessment, ImportQualificationProtocol,
        OWNER_ACCEPTED_IMPORT_QUALIFICATION_PROFILE_SHA256, QualificationBuildProvenance,
        RepositoryIdentity, assess_import_qualification_profile, host_hardware_identity,
        import_qualification_assessment_evidence, qualification_build_provenance,
        qualification_build_provenance_evidence, qualification_build_reason_codes,
        repository_identity,
    },
    process::{
        BoundedOutputPolicy, cargo_command, isolate_process_tree, run_command_with_bounded_output,
        terminate_process_tree,
    },
    product_automation_progress::{
        FILE_POLL_INTERVAL, ProductAutomationProgressLaunch, ProductAutomationProgressMonitor,
        ProductAutomationProgressPlan, ProgressFailure, ProgressMonitorAction,
        SafeProgressSnapshot, safe_automation_progress_line,
    },
    product_validate::{
        IMPORT_OPEN_READY_COMPLETE_STATUS, PRODUCT_AUTOMATION_SCRIPT_SCHEMA, SCRIPT_SCHEMA_VERSION,
        canonical_product_automation_hard_safety_limits,
        validate_product_automation_report_contract,
    },
    t5_sentinel_oracle::{self, FACT_AUTHORITY, OracleFacts, OracleRequest},
    target_fixture::extract_target_u16_fixture,
};

#[cfg(test)]
use crate::product_automation_progress::SafeProgressState;
#[cfg(test)]
use crate::product_validate::{
    PRODUCT_AUTOMATION_HARD_SAFETY_LIMIT_FIELDS, PRODUCT_AUTOMATION_REPORT_SCHEMA,
    REPORT_SCHEMA_VERSION,
};

const CONFIG_SCHEMA: &str = "mirante4d-private-import-performance-t5-2";
const RAW_REPORT_SCHEMA: &str = "mirante4d-private-import-performance-t5-raw-5";
const SANITIZED_REPORT_SCHEMA: &str = "mirante4d-import-performance-t5-summary-5";
const ORACLE_AUDIT_REPORT_SCHEMA: &str = "mirante4d-import-performance-t5-oracle-audit-1";
const PUBLICATION_CURRENTNESS_CONTRACT_ID: &str =
    "mirante4d-publication-currentness-inventory-snapshot-inventory-1";
const SCALE_DIGEST_SCHEME: &str = "mirante4d-t5-canonical-scale-voxels-1";
const CONFIG_BYTES_MAX: u64 = 1024 * 1024;
const RAW_REPORT_BYTES_MAX: u64 = 32 * 1024 * 1024;
const WORKING_MEMORY_BYTES: u64 = 256 * 1024 * 1024;
const RSS_DELTA_BYTES_MAX: u64 = 384 * 1024 * 1024;
const PRIMARY_MEDIAN_NS_MAX: u64 = 15 * 60 * 1_000_000_000;
const PRIMARY_MEDIAN_STRETCH_NS: u64 = 10 * 60 * 1_000_000_000;
const CORRECTNESS_SAMPLES: usize = 1;
const PERFORMANCE_SAMPLES: usize = 3;
const T5_SCALE_COUNT: usize = 7;
const RSS_SAMPLE_INTERVAL: Duration = Duration::from_millis(10);
const T5_X11_AUTOMATION_OUTPUT_POLICY: BoundedOutputPolicy = BoundedOutputPolicy {
    scope: "t5_x11_automation",
    inactivity_timeout: Duration::from_secs(2),
    absolute_timeout: Duration::from_secs(3),
    progress_interval: Duration::from_secs(1),
    max_stdout_bytes: 64 * 1024,
    max_stderr_bytes: 64 * 1024,
};
// Linux can tear down a process memory map before making its exit waitable.
// Only a real exit observed within this small terminal window is accepted.
const RSS_TERMINAL_EXIT_CONFIRM_TIMEOUT: Duration = Duration::from_secs(1);
const INSPECTION_TIMEOUT_SECONDS: u64 = 30 * 60;
const POST_PRIMARY_TIMEOUT_SECONDS: u64 = 10 * 60;
const MAPPED_WIDTH: u32 = 1280;
const MAPPED_HEIGHT: u32 = 720;
const SOURCE_ENTRY_MAX: usize = 4_096;
const T5_SAMPLE_GATE_NAMES: [&str; 25] = [
    "base_decode_amplification",
    "canonical_base_pixel_bytes",
    "generation_only_source_revalidation",
    "counter_reconciliation",
    "durability_calls",
    "durable_checkpoint_prefix",
    "exact_correctness",
    "external_rss_delta",
    "bounded_final_layout_checkpoint_files",
    "fresh_checkpoint_not_resumed",
    "frozen_centered_transforms",
    "independent_scientific_locality",
    "normal_product_navigation",
    "open_file_bound",
    "per_scale_semantic_regression",
    "progress_truthfulness",
    "real_mapped_display",
    "reviewed_source_workload_binding",
    "scientific_correctness",
    "source_scientific_traversal",
    "staged_scientific_locality",
    "storage_binding_stable",
    "incremental_headroom_reported",
    "timed_source_traffic",
    "working_memory",
];

/// The predecessor T5 configuration named an aggregate DS profile and its
/// whole-dataset checkpoint contract. The compositional storage/import
/// cutover deliberately invalidates that product qualification until the
/// owner reviews and pins a new current-format configuration.
pub(crate) const OWNER_ACCEPTED_T5_CONFIG_SHA256: Option<&str> = None;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QualificationKind {
    Correctness,
    Performance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SummaryPublicationMode {
    InProcess,
    FinalizedRawReplay,
}

impl SummaryPublicationMode {
    const fn label(self) -> &'static str {
        match self {
            Self::InProcess => "in_process_after_finalized_raw",
            Self::FinalizedRawReplay => "finalized_raw_replay",
        }
    }

    const fn is_recovery(self) -> bool {
        matches!(self, Self::FinalizedRawReplay)
    }
}

impl QualificationKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Correctness => "correctness",
            Self::Performance => "performance",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunArgs {
    config: PathBuf,
    samples: usize,
    diagnostic: bool,
    qualification_kind: QualificationKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OracleAuditArgs {
    config: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PublishArgs {
    config: PathBuf,
    raw_report: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct T5Config {
    schema: String,
    workload_id: String,
    source: PathBuf,
    scratch_root: PathBuf,
    qualification_profile: PathBuf,
    expected_profile: String,
    spacing_zyx_um: [f64; 3],
    time_step_seconds: Option<f64>,
    no_data_sentinel: u8,
    working_memory_bytes: u64,
    primary_timeout_seconds: u64,
    cache_condition: String,
    competing_activity: String,
    expected: ExpectedFacts,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExpectedFacts {
    expected_fact_authority: String,
    source_inventory_sha256: String,
    reviewed_source_fingerprint_sha256: String,
    canonical_source_pixel_bytes: u64,
    scientific_content_id: String,
    scientific_layer_roots: Vec<ExpectedLayerRoot>,
    scale_digest_scheme: String,
    scales: Vec<ExpectedScaleFact>,
    transforms: Vec<ExpectedTransformFact>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct ExpectedLayerRoot {
    logical_layer: u32,
    digest_sha256: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct ExpectedScaleFact {
    image_ordinal: u32,
    scale_ordinal: u32,
    digest_sha256: String,
    brick_reads: u64,
    logical_voxels: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ExpectedTransformFact {
    scale_ordinal: u32,
    scale_zyx: [f64; 3],
    translation_zyx: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfigInput {
    path: PathBuf,
    bytes: Vec<u8>,
    sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InventoryFacts {
    regular_files: u64,
    source_bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RssSample {
    epoch_ms: u128,
    rss_bytes: u64,
}

#[derive(Debug)]
struct SampleEvidence {
    sample_index: usize,
    primary_wall_time_ns: u64,
    package_id: String,
    scientific_content_id: String,
    failed_gates: Vec<String>,
    all_gates_passed: bool,
    raw: Value,
}

#[derive(Debug)]
struct IndependentValidation {
    package_id: String,
    scientific_content_id: String,
    layer_roots: Vec<Value>,
    scale_facts: Vec<Value>,
    transform_facts: Vec<Value>,
    scale_facts_match: bool,
    layer_roots_match: bool,
    scientific_id_matches: bool,
    config_transform_facts_match: bool,
    scientific_brick_reads: u64,
    canonical_base_pixel_bytes: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct ObservedTransformFact {
    scale_ordinal: u32,
    scale_zyx: [f64; 3],
    translation_zyx: [f64; 3],
}

struct OwnedOracleScratch {
    path: PathBuf,
    device: u64,
    inode: u64,
    armed: bool,
}

struct CommandCancellation {
    flag: Arc<AtomicBool>,
    // signal-hook cannot safely restore the prior/default disposition when an
    // action is unregistered. Keep these registrations live until this
    // one-command process exits; every remaining long path observes `flag`.
    _registrations: Vec<SigId>,
}

impl CommandCancellation {
    fn install() -> anyhow::Result<Self> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut registrations = Vec::with_capacity(4);
        for (signal, exit_status) in [(SIGINT, 130), (SIGTERM, 143)] {
            // Registration order is normative: the first signal sees false in
            // the conditional shutdown action, then the flag action requests
            // cooperative cleanup. A repeated signal terminates immediately.
            registrations.push(flag::register_conditional_shutdown(
                signal,
                exit_status,
                Arc::clone(&cancelled),
            )?);
            registrations.push(flag::register(signal, Arc::clone(&cancelled))?);
        }
        Ok(Self {
            flag: cancelled,
            _registrations: registrations,
        })
    }

    fn token(&self) -> &AtomicBool {
        &self.flag
    }

    fn check(&self, phase: &str) -> anyhow::Result<()> {
        if self.flag.load(Ordering::Acquire) {
            bail!("T5 command cancelled by SIGINT or SIGTERM during {phase}");
        }
        Ok(())
    }
}

impl OwnedOracleScratch {
    fn create(path: PathBuf) -> anyhow::Result<Self> {
        create_private_directory(&path).with_context(|| {
            format!(
                "failed to create private T5 oracle scratch {}",
                path.display()
            )
        })?;
        let created = fs::symlink_metadata(&path)?;
        let scratch = Self {
            path,
            device: created.dev(),
            inode: created.ino(),
            armed: true,
        };
        fs::set_permissions(&scratch.path, fs::Permissions::from_mode(0o700))?;
        let metadata = fs::symlink_metadata(&scratch.path)?;
        if !scratch.path_still_names_created_directory(&metadata)
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            bail!("private T5 oracle scratch is not a mode-0700 real directory");
        }
        Ok(scratch)
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn remove_and_report_empty(mut self) -> anyhow::Result<bool> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.armed = false;
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
        };
        if !self.path_still_names_created_directory(&metadata)
            || fs::read_dir(&self.path)?.next().is_some()
        {
            self.armed = false;
            return Ok(false);
        }
        fs::remove_dir(&self.path)?;
        let absent_after_removal = fs::symlink_metadata(&self.path)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound);
        self.armed = false;
        Ok(absent_after_removal)
    }

    fn path_still_names_created_directory(&self, metadata: &fs::Metadata) -> bool {
        !metadata.file_type().is_symlink()
            && metadata.is_dir()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
    }
}

impl Drop for OwnedOracleScratch {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return;
        };
        if self.path_still_names_created_directory(&metadata)
            && fs::read_dir(&self.path).is_ok_and(|mut entries| entries.next().is_none())
        {
            let _ = fs::remove_dir(&self.path);
        }
    }
}

pub(crate) fn run(args: Vec<String>) -> anyhow::Result<PathBuf> {
    let args = parse_args(args)?;
    let build_provenance = qualification_build_provenance();
    require_release_xtask(&build_provenance)?;
    require_standard_app_build_environment(&build_provenance)?;
    require_owner_attested_x11_display()?;

    let repository_start = repository_identity();
    require_clean_repository(&repository_start)?;
    let build_reason_codes_start =
        qualification_build_reason_codes(&build_provenance, &repository_start);
    if !build_reason_codes_start.is_empty() {
        bail!(
            "T5 xtask build provenance is not the exact clean release revision: {}",
            build_reason_codes_start.join(", ")
        );
    }
    let repository_root = repository_start
        .root
        .as_deref()
        .context("T5 qualification could not resolve the repository root")?;
    require_no_external_cargo_configuration(repository_root)?;
    let xtask_executable =
        env::current_exe().context("failed to resolve the T5 xtask executable")?;
    validate_release_executable(&xtask_executable, "T5 xtask")?;
    let xtask_digest_start = sha256_file(&xtask_executable)?;
    let xtask_metadata_start = fs::metadata(&xtask_executable)?;
    let hardware_start = host_hardware_identity();
    let config_input = read_config_input(&args.config, repository_root)?;
    let config: T5Config = serde_json::from_slice(&config_input.bytes)
        .context("private T5 configuration is not strict valid JSON")?;
    validate_config(&config)?;
    let qualification_protocol = ImportQualificationProtocol::new(
        config.cache_condition.clone(),
        config.competing_activity.clone(),
    );

    let source = canonical_external_source(&config.source, repository_root)?;
    let scratch_root = canonical_external_directory(&config.scratch_root, repository_root)?;
    let qualification_profile = canonical_external_regular_file(
        &config.qualification_profile,
        repository_root,
        IMPORT_QUALIFICATION_PROFILE_MAX_BYTES,
        "qualification profile",
    )?;
    if source.starts_with(&scratch_root) || scratch_root.starts_with(&source) {
        bail!("private T5 source and scratch root must be disjoint");
    }

    let config_binding_status = config_binding_status(&config_input.sha256);
    let source_binding_start = assess_import_qualification_profile(
        Some(&qualification_profile),
        &repository_start,
        &source,
        &hardware_start,
        &qualification_protocol,
    );
    let scratch_binding_start = assess_import_qualification_profile(
        Some(&qualification_profile),
        &repository_start,
        &scratch_root,
        &hardware_start,
        &qualification_protocol,
    );
    let profile_matched_at_start = qualification_matched(&source_binding_start)
        && qualification_matched(&scratch_binding_start);
    let qualification_eligible_at_start =
        config_binding_status == "matched" && profile_matched_at_start;
    if !qualification_eligible_at_start && !args.diagnostic {
        bail!(
            "T5 qualification is not owner-bound; --diagnostic may inspect the private binding without making a qualification claim"
        );
    }
    if matches!(source_binding_start.status, "invalid" | "rejected")
        || matches!(scratch_binding_start.status, "invalid" | "rejected")
    {
        bail!("unsafe or unreadable T5 qualification-profile/path binding");
    }

    let session_id = session_id()?;
    let session_root = scratch_root.join(&session_id);
    create_private_directory(&session_root).with_context(|| {
        format!(
            "failed to create private T5 session {}",
            session_root.display()
        )
    })?;
    let session_binding_start = assess_import_qualification_profile(
        Some(&qualification_profile),
        &repository_start,
        &session_root,
        &hardware_start,
        &qualification_protocol,
    );
    if !qualification_matched(&session_binding_start) && !args.diagnostic {
        bail!("T5 session storage is outside the owner-accepted profile");
    }
    if matches!(session_binding_start.status, "invalid" | "rejected") {
        bail!("unsafe T5 session storage binding");
    }

    let build_target = session_root.join("fresh-release-target");
    create_private_directory(&build_target)?;
    let executable = release_app_binary(&build_target);
    build_standard_release_app(
        repository_root,
        &build_target,
        repository_start
            .commit
            .as_deref()
            .context("T5 release app build lacks its repository revision")?,
        &build_provenance.compiler,
    )?;
    let repository_after_build = repository_identity();
    if !repository_same_clean_revision(&repository_start, &repository_after_build) {
        bail!("repository identity changed while building the T5 release app");
    }

    let cancellation = CommandCancellation::install()?;
    cancellation.check("source inventory setup")?;
    let source_before = source_inventory(&source, cancellation.token())?;
    let frozen_source_matched = source_before.sha256 == config.expected.source_inventory_sha256;
    if !frozen_source_matched && !args.diagnostic {
        bail!("private T5 source inventory does not match the owner-frozen workload");
    }
    validate_release_executable(&executable, "T5 release app")?;
    let executable_digest_start = sha256_file(&executable)?;
    let executable_metadata_start = fs::metadata(&executable)?;

    let startup_package = extract_target_u16_fixture(
        &repository_root.join("target/mirante4d/import-performance-t5/startup"),
    )?;
    let mut samples = Vec::with_capacity(args.samples);
    for sample_index in 0..args.samples {
        samples.push(run_sample(
            sample_index,
            &config,
            &source,
            &source_before,
            &session_root,
            &startup_package,
            &executable,
            &qualification_profile,
            &repository_start,
            &hardware_start,
            &qualification_protocol,
            &build_provenance.compiler,
            args.diagnostic,
            cancellation.token(),
        )?);
        if sha256_file(&executable)? != executable_digest_start
            || !same_file_metadata(&executable_metadata_start, &fs::metadata(&executable)?)
        {
            bail!("release app executable changed during the T5 sample set");
        }
    }

    let source_after = source_inventory(&source, cancellation.token())?;
    if source_after != source_before {
        bail!("private T5 source changed during the sample set");
    }
    cancellation.check("post-sample invariant validation")?;
    let xtask_digest_end = sha256_file(&xtask_executable)?;
    let xtask_unchanged = xtask_digest_start == xtask_digest_end
        && same_file_metadata(&xtask_metadata_start, &fs::metadata(&xtask_executable)?);
    let executable_digest_end = sha256_file(&executable)?;
    let config_end = read_config_input(&config_input.path, repository_root)?;
    let repository_end = repository_identity();
    let hardware_end = host_hardware_identity();
    let source_binding_end = assess_import_qualification_profile(
        Some(&qualification_profile),
        &repository_end,
        &source,
        &hardware_end,
        &qualification_protocol,
    );
    let scratch_binding_end = assess_import_qualification_profile(
        Some(&qualification_profile),
        &repository_end,
        &scratch_root,
        &hardware_end,
        &qualification_protocol,
    );
    let session_binding_end = assess_import_qualification_profile(
        Some(&qualification_profile),
        &repository_end,
        &session_root,
        &hardware_end,
        &qualification_protocol,
    );
    let build_reason_codes_end =
        qualification_build_reason_codes(&build_provenance, &repository_end);

    let source_preserved = source_before == source_after;
    let repository_unchanged = repository_same_clean_revision(&repository_start, &repository_end);
    let hardware_unchanged = hardware_start == hardware_end;
    let executable_unchanged = executable_digest_start == executable_digest_end
        && same_file_metadata(&executable_metadata_start, &fs::metadata(&executable)?);
    let config_unchanged = config_input.bytes == config_end.bytes
        && config_input.sha256 == config_end.sha256
        && config_input.path == config_end.path;
    let profile_matched_at_end = qualification_matched(&source_binding_end)
        && qualification_matched(&scratch_binding_end)
        && qualification_matched(&session_binding_end);
    let profile_assessment_unchanged = source_binding_start == source_binding_end
        && scratch_binding_start == scratch_binding_end
        && session_binding_start == session_binding_end;

    let mut primary_times = samples
        .iter()
        .map(|sample| sample.primary_wall_time_ns)
        .collect::<Vec<_>>();
    primary_times.sort_unstable();
    let median_primary_wall_time_ns = primary_times[primary_times.len() / 2];
    let cross_sample_ids_consistent = (samples.len() > 1).then(|| {
        samples.windows(2).all(|pair| {
            pair[0].package_id == pair[1].package_id
                && pair[0].scientific_content_id == pair[1].scientific_content_id
        })
    });
    let samples_passed = samples.iter().all(|sample| sample.all_gates_passed);
    let performance_timing_gate_passed = args.qualification_kind == QualificationKind::Performance
        && args.samples == PERFORMANCE_SAMPLES
        && median_primary_wall_time_ns <= PRIMARY_MEDIAN_NS_MAX;
    let binding_eligible = qualification_eligible_at_start
        && config_binding_status == "matched"
        && profile_matched_at_end
        && profile_assessment_unchanged;
    let invariant_gates_passed = frozen_source_matched
        && source_preserved
        && repository_unchanged
        && hardware_unchanged
        && xtask_unchanged
        && executable_unchanged
        && config_unchanged
        && build_reason_codes_end.is_empty();
    let correctness_qualification_passed = correctness_qualification_claim_passed(
        args.diagnostic,
        binding_eligible,
        samples_passed,
        invariant_gates_passed,
    );
    let performance_qualification_passed = performance_qualification_claim_passed(
        args.qualification_kind == QualificationKind::Performance,
        correctness_qualification_passed,
        performance_timing_gate_passed,
        cross_sample_ids_consistent,
    );
    let qualification_passed = match args.qualification_kind {
        QualificationKind::Correctness => correctness_qualification_passed,
        QualificationKind::Performance => performance_qualification_passed,
    };
    let diagnostic_reason_codes = qualification_diagnostic_reason_codes(
        &args,
        config_binding_status,
        &source_binding_start,
        &scratch_binding_start,
        &session_binding_start,
        &source_binding_end,
        &scratch_binding_end,
        &session_binding_end,
        &build_reason_codes_start,
        &build_reason_codes_end,
        &samples,
        performance_timing_gate_passed,
        frozen_source_matched,
        source_preserved,
        repository_unchanged,
        hardware_unchanged,
        xtask_unchanged,
        executable_unchanged,
        config_unchanged,
        cross_sample_ids_consistent,
    );

    let raw_report = json!({
        "schema": RAW_REPORT_SCHEMA,
        "schema_version": 4,
        "session_id": session_id,
        "configuration": {
            "path": config_input.path,
            "sha256": config_input.sha256,
            "owner_accepted_sha256": OWNER_ACCEPTED_T5_CONFIG_SHA256,
            "binding_status": config_binding_status,
            "parsed": config,
        },
        "protocol": {
            "qualification_kind": args.qualification_kind.label(),
            "sample_count": args.samples,
            "diagnostic_requested": args.diagnostic,
            "fresh_release_process_per_sample": true,
            "fresh_checkpoint_and_absent_destination_per_sample": true,
            "normal_product_import_route": true,
            "primary_clock": "worker_spawn_origin_through_product_admitted_open_ready",
            "rss_sample_interval_ms": RSS_SAMPLE_INTERVAL.as_millis(),
        },
        "build": {
            "xtask_provenance": qualification_build_provenance_evidence(&build_provenance),
            "provenance_reason_codes_start": build_reason_codes_start,
            "provenance_reason_codes_end": build_reason_codes_end,
            "repository_start": repository_evidence(&repository_start),
            "repository_end": repository_evidence(&repository_end),
            "repository_unchanged": repository_unchanged,
            "hardware_unchanged": hardware_unchanged,
            "xtask_executable_path": xtask_executable,
            "xtask_executable_sha256_start": xtask_digest_start,
            "xtask_executable_sha256_end": xtask_digest_end,
            "xtask_executable_unchanged": xtask_unchanged,
            "executable_path": executable,
            "executable_sha256_start": executable_digest_start,
            "executable_sha256_end": executable_digest_end,
            "executable_unchanged": executable_unchanged,
            "fresh_private_build_target": true,
            "embedded_app_build_provenance_required": true,
            "toolchain": command_text("rustc", &["--version", "--verbose"]),
        },
        "qualification_profile": {
            "required_schema": IMPORT_QUALIFICATION_PROFILE_SCHEMA,
            "required_hardware_class": IMPORT_QUALIFICATION_HARDWARE_CLASS,
            "owner_accepted_sha256": OWNER_ACCEPTED_IMPORT_QUALIFICATION_PROFILE_SHA256,
            "source_start": import_qualification_assessment_evidence(&source_binding_start),
            "scratch_start": import_qualification_assessment_evidence(&scratch_binding_start),
            "session_start": import_qualification_assessment_evidence(&session_binding_start),
            "source_end": import_qualification_assessment_evidence(&source_binding_end),
            "scratch_end": import_qualification_assessment_evidence(&scratch_binding_end),
            "session_end": import_qualification_assessment_evidence(&session_binding_end),
            "unchanged": profile_assessment_unchanged,
        },
        "source_preservation": {
            "source_path": source,
            "before": inventory_json(&source_before),
            "after": inventory_json(&source_after),
            "matches_frozen_configuration": frozen_source_matched,
            "unchanged": source_preserved,
        },
        "frozen_source_facts": {
            "fact_authority": FACT_AUTHORITY,
            "source_oracle_recomputed": false,
            "oracle_recomputation_policy": "explicit_offline_audit_only",
            "package_validated_against_frozen_facts": true,
            "owner_pinned_fact_freeze_binding": config_binding_status == "matched",
        },
        "samples": samples.iter().map(|sample| sample.raw.clone()).collect::<Vec<_>>(),
        "summary": {
            "performance_median_primary_wall_time_ns": if args.qualification_kind == QualificationKind::Performance { Some(median_primary_wall_time_ns) } else { None },
            "absolute_gate_ns": PRIMARY_MEDIAN_NS_MAX,
            "stretch_target_ns": PRIMARY_MEDIAN_STRETCH_NS,
            "performance_timing_gate_evaluated": args.qualification_kind == QualificationKind::Performance,
            "performance_timing_gate_passed": performance_timing_gate_passed,
            "samples_passed": samples_passed,
            "cross_sample_ids_consistent": cross_sample_ids_consistent,
            "frozen_source_matched": frozen_source_matched,
            "source_preserved": source_preserved,
            "config_unchanged": config_unchanged,
            "binding_eligible": binding_eligible,
            "diagnostic_reason_codes": diagnostic_reason_codes,
            "correctness_qualification_passed": correctness_qualification_passed,
            "performance_qualification_passed": performance_qualification_passed,
            "qualification_passed": qualification_passed,
        },
    });
    let raw_report_path = session_root.join("raw-private-report.json");
    write_new_synced_json(&raw_report_path, &raw_report)?;
    cancellation.check("sanitized evidence publication")?;
    let sanitized_path = publish_finalized_raw_report(
        repository_root,
        &config_input,
        &config,
        &raw_report_path,
        SummaryPublicationMode::InProcess,
    )?;
    cancellation.check("command completion")?;
    Ok(sanitized_path)
}

pub(crate) fn publish(args: Vec<String>) -> anyhow::Result<PathBuf> {
    let args = parse_publish_args(args)?;
    let build_provenance = qualification_build_provenance();
    require_release_xtask(&build_provenance)?;
    require_standard_app_build_environment(&build_provenance)?;
    let repository = repository_identity();
    require_clean_repository(&repository)?;
    let build_reasons = qualification_build_reason_codes(&build_provenance, &repository);
    if !build_reasons.is_empty() {
        bail!(
            "T5 report publication requires an exact clean release xtask: {}",
            build_reasons.join(", ")
        );
    }
    let repository_root = repository
        .root
        .as_deref()
        .context("T5 report publication could not resolve the repository root")?;
    require_no_external_cargo_configuration(repository_root)?;
    let config_input = read_config_input(&args.config, repository_root)?;
    let config: T5Config = serde_json::from_slice(&config_input.bytes)
        .context("private T5 configuration is not strict valid JSON")?;
    validate_config(&config)?;
    if config_binding_status(&config_input.sha256) != "matched" {
        bail!("T5 report publication requires the owner-pinned private configuration");
    }
    publish_finalized_raw_report(
        repository_root,
        &config_input,
        &config,
        &args.raw_report,
        SummaryPublicationMode::FinalizedRawReplay,
    )
}

fn publish_finalized_raw_report(
    repository_root: &Path,
    config_input: &ConfigInput,
    config: &T5Config,
    raw_report_path: &Path,
    publication_mode: SummaryPublicationMode,
) -> anyhow::Result<PathBuf> {
    let (raw_report, raw_report_sha256) =
        read_finalized_raw_report(raw_report_path, repository_root, RAW_REPORT_BYTES_MAX)?;
    validate_raw_config_binding(&raw_report, config_input, config)?;
    let publisher_repository = repository_identity();
    require_clean_repository(&publisher_repository)?;
    let publisher_build_provenance = qualification_build_provenance();
    require_release_xtask(&publisher_build_provenance)?;
    let publisher_build_reasons =
        qualification_build_reason_codes(&publisher_build_provenance, &publisher_repository);
    if !publisher_build_reasons.is_empty() {
        bail!(
            "T5 summary publisher is not bound to its clean repository revision: {}",
            publisher_build_reasons.join(", ")
        );
    }
    let publisher_revision = publisher_repository
        .commit
        .as_deref()
        .context("T5 summary publisher lacks a repository revision")?;
    let publisher_executable = env::current_exe()?;
    validate_release_executable(&publisher_executable, "T5 summary publisher")?;
    let publisher_executable_sha256 = sha256_file(&publisher_executable)?;
    let sanitized_report = sanitized_report_from_raw(
        &raw_report,
        &raw_report_sha256,
        config_input,
        config,
        publisher_revision,
        &publisher_executable_sha256,
        publication_mode,
    )?;
    let private_strings = private_strings_from_raw(
        &raw_report,
        raw_report_path,
        repository_root,
        config_input,
        config,
    );
    validate_sanitized_report(&sanitized_report, &private_strings)?;

    let evidence_id = required_string(&raw_report, "/session_id")?;
    if !evidence_id.starts_with("t5-")
        || evidence_id.len() > 96
        || !evidence_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-' || byte.is_ascii_lowercase())
    {
        bail!("finalized T5 raw report has an unsafe evidence identifier");
    }
    let sanitized_root = repository_root.join("target/mirante4d/import-performance-t5");
    fs::create_dir_all(&sanitized_root)?;
    let sanitized_path = sanitized_root.join(format!("{evidence_id}-summary.json"));
    write_new_synced_json(&sanitized_path, &sanitized_report)?;
    let reread = read_bounded_json(&sanitized_path, RAW_REPORT_BYTES_MAX)?;
    if reread != sanitized_report {
        bail!("published T5 summary changed while reread");
    }
    validate_sanitized_report(&reread, &private_strings)?;
    if reread
        .pointer("/bindings/private_raw_report_sha256")
        .and_then(Value::as_str)
        != Some(raw_report_sha256.as_str())
    {
        bail!("published T5 summary lost its finalized raw-report binding");
    }
    Ok(sanitized_path)
}

fn read_finalized_raw_report(
    path: &Path,
    repository_root: &Path,
    maximum_bytes: u64,
) -> anyhow::Result<(Value, String)> {
    let finalized = crate::private_evidence::read_finalized_private_file(
        path,
        repository_root,
        maximum_bytes,
        "finalized T5 raw report",
    )?;
    let report =
        serde_json::from_slice(&finalized.bytes).context("finalized T5 raw report is malformed")?;
    Ok((report, finalized.sha256))
}

fn validate_raw_config_binding(
    raw: &Value,
    config_input: &ConfigInput,
    config: &T5Config,
) -> anyhow::Result<()> {
    require_exact_object_keys(
        raw,
        &[
            "build",
            "configuration",
            "frozen_source_facts",
            "protocol",
            "qualification_profile",
            "samples",
            "schema",
            "schema_version",
            "session_id",
            "source_preservation",
            "summary",
        ],
        "finalized T5 raw report",
    )?;
    if required_string(raw, "/schema")? != RAW_REPORT_SCHEMA
        || required_pointer_u64(raw, "/schema_version")? != 4
    {
        bail!("finalized T5 raw report schema mismatch");
    }
    let parsed_config = serde_json::to_value(config)?;
    if required_string(raw, "/configuration/sha256")? != config_input.sha256
        || required_string(raw, "/configuration/owner_accepted_sha256")?
            != OWNER_ACCEPTED_T5_CONFIG_SHA256.context("T5 config pin is absent")?
        || required_string(raw, "/configuration/binding_status")? != "matched"
        || raw.pointer("/configuration/parsed") != Some(&parsed_config)
        || raw.pointer("/configuration/path") != Some(&json!(config_input.path))
    {
        bail!("finalized T5 raw report does not match the pinned configuration bytes");
    }
    Ok(())
}

fn sanitized_report_from_raw(
    raw: &Value,
    raw_report_sha256: &str,
    config_input: &ConfigInput,
    config: &T5Config,
    publisher_revision: &str,
    publisher_executable_sha256: &str,
    publication_mode: SummaryPublicationMode,
) -> anyhow::Result<Value> {
    let qualification_kind = required_string(raw, "/protocol/qualification_kind")?;
    let diagnostic_requested = required_bool(raw, "/protocol/diagnostic_requested")?;
    let sample_count = usize::try_from(required_pointer_u64(raw, "/protocol/sample_count")?)?;
    let performance_requested = match qualification_kind {
        "correctness" => {
            if sample_count != CORRECTNESS_SAMPLES {
                bail!("correctness raw report must contain exactly one sample");
            }
            false
        }
        "performance" => {
            if sample_count != PERFORMANCE_SAMPLES {
                bail!("performance raw report must contain exactly three samples");
            }
            true
        }
        _ => bail!("finalized T5 raw report has an unknown qualification kind"),
    };
    let raw_samples = raw
        .pointer("/samples")
        .and_then(Value::as_array)
        .context("finalized T5 raw report lacks samples")?;
    if raw_samples.len() != sample_count {
        bail!("finalized T5 raw report sample count is inconsistent");
    }
    for pointer in [
        "/protocol/fresh_checkpoint_and_absent_destination_per_sample",
        "/protocol/fresh_release_process_per_sample",
        "/protocol/normal_product_import_route",
        "/frozen_source_facts/package_validated_against_frozen_facts",
        "/frozen_source_facts/owner_pinned_fact_freeze_binding",
    ] {
        if !required_bool(raw, pointer)? {
            bail!("finalized T5 raw report lost a mandatory fact at {pointer}");
        }
    }
    if required_bool(raw, "/frozen_source_facts/source_oracle_recomputed")?
        || required_string(raw, "/frozen_source_facts/fact_authority")? != FACT_AUTHORITY
        || required_string(raw, "/frozen_source_facts/oracle_recomputation_policy")?
            != "explicit_offline_audit_only"
    {
        bail!("finalized T5 raw report has an invalid frozen-fact authority");
    }

    let mut sanitized_samples = Vec::with_capacity(sample_count);
    let mut primary_times = Vec::with_capacity(sample_count);
    let mut identities = Vec::with_capacity(sample_count);
    let mut samples_passed = true;
    for (expected_index, sample) in raw_samples.iter().enumerate() {
        if usize::try_from(required_pointer_u64(sample, "/sample_index")?)? != expected_index {
            bail!("finalized T5 raw report sample indices are not contiguous");
        }
        let gates = sample
            .pointer("/gates")
            .and_then(Value::as_object)
            .context("finalized T5 raw sample lacks gates")?;
        let expected_gates = T5_SAMPLE_GATE_NAMES.into_iter().collect::<BTreeSet<_>>();
        let actual_gates = gates.keys().map(String::as_str).collect::<BTreeSet<_>>();
        if actual_gates != expected_gates || gates.values().any(|value| !value.is_boolean()) {
            bail!("finalized T5 raw sample gate set or value type is invalid");
        }
        let all_gates_passed = gates.values().all(|value| value.as_bool() == Some(true));
        if required_bool(sample, "/all_gates_passed")? != all_gates_passed {
            bail!("finalized T5 raw sample gate aggregate is inconsistent");
        }
        samples_passed &= all_gates_passed;
        let primary_wall_time_ns = required_pointer_u64(sample, "/timing/primary_wall_time_ns")?;
        primary_times.push(primary_wall_time_ns);
        identities.push((
            required_string(sample, "/receipt/package_id")?.to_owned(),
            required_string(sample, "/receipt/scientific_content_id")?.to_owned(),
        ));
        sanitized_samples.push(json!({
            "sample_index": expected_index,
            "inspection_and_review_wall_time_ns": required_value(sample, "/timing/inspection_and_review_wall_time_ns")?,
            "inspection_and_review_process_cpu_time_ns": required_value(sample, "/timing/inspection_and_review_process_cpu_time_ns")?,
            "primary_wall_time_ns": primary_wall_time_ns,
            "primary_process_cpu_time_ns": required_value(sample, "/timing/primary_process_cpu_time_ns")?,
            "publication_to_open_ready_wall_time_ns": required_value(sample, "/timing/publication_to_open_ready_wall_time_ns")?,
            "publication_to_open_ready_process_cpu_time_ns": required_value(sample, "/timing/publication_to_open_ready_process_cpu_time_ns")?,
            "external_rss_delta_bytes": required_value(sample, "/rss/primary_delta_bytes")?,
            "external_rss_delta_minus_ledger_peak_bytes": required_value(sample, "/rss/external_delta_minus_ledger_peak_bytes")?,
            "peak_working_bytes": required_value(sample, "/receipt/statistics/peak_working_bytes")?,
            "peak_open_file_descriptors": required_value(sample, "/receipt/statistics/peak_open_file_descriptors")?,
            "peak_temporary_bytes": required_value(sample, "/receipt/statistics/peak_temporary_bytes")?,
            "peak_checkpoint_regular_files": required_value(sample, "/receipt/statistics/peak_checkpoint_regular_files")?,
            "sync_calls": required_value(sample, "/receipt/statistics/sync_calls")?,
            "gates": gates,
            "all_gates_passed": all_gates_passed,
        }));
    }
    primary_times.sort_unstable();
    let median_primary_wall_time_ns = primary_times[primary_times.len() / 2];
    let cross_sample_ids_consistent =
        (sample_count > 1).then(|| identities.windows(2).all(|pair| pair[0] == pair[1]));
    let performance_timing_gate_passed =
        performance_requested && median_primary_wall_time_ns <= PRIMARY_MEDIAN_NS_MAX;

    let profile_statuses_match = [
        "/qualification_profile/source_start/status",
        "/qualification_profile/scratch_start/status",
        "/qualification_profile/session_start/status",
        "/qualification_profile/source_end/status",
        "/qualification_profile/scratch_end/status",
        "/qualification_profile/session_end/status",
    ]
    .into_iter()
    .all(|pointer| raw.pointer(pointer).and_then(Value::as_str) == Some("matched"));
    let profile_unchanged = required_bool(raw, "/qualification_profile/unchanged")?
        && raw.pointer("/qualification_profile/source_start")
            == raw.pointer("/qualification_profile/source_end")
        && raw.pointer("/qualification_profile/scratch_start")
            == raw.pointer("/qualification_profile/scratch_end")
        && raw.pointer("/qualification_profile/session_start")
            == raw.pointer("/qualification_profile/session_end");
    let binding_eligible = required_string(raw, "/configuration/binding_status")? == "matched"
        && profile_statuses_match
        && profile_unchanged;
    let frozen_source_matched =
        required_bool(raw, "/source_preservation/matches_frozen_configuration")?;
    let source_preserved = required_bool(raw, "/source_preservation/unchanged")?
        && raw.pointer("/source_preservation/before") == raw.pointer("/source_preservation/after")
        && raw
            .pointer("/source_preservation/before/inventory_sha256")
            .and_then(Value::as_str)
            == Some(config.expected.source_inventory_sha256.as_str());
    let repository_unchanged = required_bool(raw, "/build/repository_unchanged")?;
    let hardware_unchanged = required_bool(raw, "/build/hardware_unchanged")?;
    let xtask_unchanged = required_bool(raw, "/build/xtask_executable_unchanged")?
        && raw.pointer("/build/xtask_executable_sha256_start")
            == raw.pointer("/build/xtask_executable_sha256_end");
    let executable_unchanged = required_bool(raw, "/build/executable_unchanged")?
        && raw.pointer("/build/executable_sha256_start")
            == raw.pointer("/build/executable_sha256_end");
    let config_unchanged = required_bool(raw, "/summary/config_unchanged")?;
    let build_start_clean = raw
        .pointer("/build/provenance_reason_codes_start")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty);
    let build_end_clean = raw
        .pointer("/build/provenance_reason_codes_end")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty);
    let invariant_gates_passed = frozen_source_matched
        && source_preserved
        && repository_unchanged
        && hardware_unchanged
        && xtask_unchanged
        && executable_unchanged
        && config_unchanged
        && build_start_clean
        && build_end_clean;
    let correctness_qualification_passed = correctness_qualification_claim_passed(
        diagnostic_requested,
        binding_eligible,
        samples_passed,
        invariant_gates_passed,
    );
    let performance_qualification_passed = performance_qualification_claim_passed(
        performance_requested,
        correctness_qualification_passed,
        performance_timing_gate_passed,
        cross_sample_ids_consistent,
    );
    let qualification_passed = if performance_requested {
        performance_qualification_passed
    } else {
        correctness_qualification_passed
    };

    for (pointer, expected) in [
        ("/summary/absolute_gate_ns", json!(PRIMARY_MEDIAN_NS_MAX)),
        (
            "/summary/stretch_target_ns",
            json!(PRIMARY_MEDIAN_STRETCH_NS),
        ),
        ("/summary/samples_passed", json!(samples_passed)),
        (
            "/summary/cross_sample_ids_consistent",
            json!(cross_sample_ids_consistent),
        ),
        (
            "/summary/frozen_source_matched",
            json!(frozen_source_matched),
        ),
        ("/summary/source_preserved", json!(source_preserved)),
        ("/summary/binding_eligible", json!(binding_eligible)),
        (
            "/summary/performance_timing_gate_evaluated",
            json!(performance_requested),
        ),
        (
            "/summary/performance_timing_gate_passed",
            json!(performance_timing_gate_passed),
        ),
        (
            "/summary/correctness_qualification_passed",
            json!(correctness_qualification_passed),
        ),
        (
            "/summary/performance_qualification_passed",
            json!(performance_qualification_passed),
        ),
        ("/summary/qualification_passed", json!(qualification_passed)),
    ] {
        if raw.pointer(pointer) != Some(&expected) {
            bail!("finalized T5 raw report summary is internally inconsistent at {pointer}");
        }
    }
    let expected_median = if performance_requested {
        json!(median_primary_wall_time_ns)
    } else {
        Value::Null
    };
    if raw.pointer("/summary/performance_median_primary_wall_time_ns") != Some(&expected_median) {
        bail!("finalized T5 raw report median is internally inconsistent");
    }
    if qualification_passed
        && !raw
            .pointer("/summary/diagnostic_reason_codes")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    {
        bail!("qualifying T5 raw report retained diagnostic failures");
    }

    let measurement_revision = required_string(raw, "/build/repository_start/commit")?;
    if raw
        .pointer("/build/repository_end/commit")
        .and_then(Value::as_str)
        != Some(measurement_revision)
        || required_bool(raw, "/build/repository_start/dirty_worktree")?
        || required_bool(raw, "/build/repository_end/dirty_worktree")?
    {
        bail!("finalized T5 raw report is not bound to one clean measurement revision");
    }
    let filesystem_class = raw
        .pointer("/qualification_profile/source_start/observed_filesystem_type")
        .and_then(Value::as_str)
        .unwrap_or("unavailable");
    let mut remaining_risks = vec![
        "no_relative_speedup_or_timing_tail_claim",
        "cache_and_competing_activity_are_declared_not_os_enforced",
        "physical_display_attachment_is_owner_attested_not_cryptographically_proved",
        "retained_private_configuration_commitment_can_confirm_a_guessed_configuration",
    ];
    if performance_requested && median_primary_wall_time_ns > PRIMARY_MEDIAN_STRETCH_NS {
        remaining_risks.push("nonblocking_10_minute_stretch_target_not_met");
    }
    if !performance_requested {
        remaining_risks.push("restored_policy_three_sample_performance_not_qualified");
    }
    let diagnostic_reason_codes = required_value(raw, "/summary/diagnostic_reason_codes")?;
    Ok(json!({
        "schema": SANITIZED_REPORT_SCHEMA,
        "schema_version": 4,
        "evidence_id": required_string(raw, "/session_id")?,
        "workload_id": config.workload_id,
        "evidence_class": if qualification_passed {
            if performance_requested { "performance_qualification" } else { "correctness_qualification" }
        } else { "diagnostic" },
        "status": if qualification_passed { "passed" } else { "not_qualified" },
        "publication": {
            "mode": publication_mode.label(),
            "measurement_revision": measurement_revision,
            "publisher_revision": publisher_revision,
            "publisher_executable_sha256": publisher_executable_sha256,
            "measurement_reexecuted_for_publication": false,
            "reporting_only_recovery": publication_mode.is_recovery(),
        },
        "protocol": {
            "qualification_kind": qualification_kind,
            "sample_count": sample_count,
            "diagnostic_requested": diagnostic_requested,
            "fresh_release_processes": true,
            "normal_product_import_route": true,
            "source_oracle_recomputed": false,
            "oracle_recomputation_policy": "explicit_offline_audit_only",
            "frozen_fact_authority": FACT_AUTHORITY,
            "package_validated_against_frozen_facts": true,
            "real_external_mapped_window_required": true,
            "physical_display_attachment_owner_attested": true,
            "external_display_proof_scope": "mapped_native_x11_client_geometry",
            "working_memory_bytes": WORKING_MEMORY_BYTES,
            "cache_condition": config.cache_condition,
            "competing_activity": config.competing_activity,
            "required_hardware_class": IMPORT_QUALIFICATION_HARDWARE_CLASS,
            "qualified_hardware_class": if profile_statuses_match { Some(IMPORT_QUALIFICATION_HARDWARE_CLASS) } else { None },
            "filesystem_class": filesystem_class,
        },
        "bindings": {
            "private_configuration": required_string(raw, "/configuration/binding_status")?,
            "private_configuration_sha256": config_input.sha256,
            "private_raw_report_sha256": raw_report_sha256,
            "host_and_storage_profile": if profile_statuses_match { "matched" } else { "not_matched" },
            "qualification_profile_sha256": required_value(raw, "/qualification_profile/source_start/profile_sha256")?,
            "observed_host_fingerprint_sha256": required_value(raw, "/qualification_profile/source_start/observed_host_fingerprint_sha256")?,
            "observed_storage_fingerprint_sha256": required_value(raw, "/qualification_profile/source_start/observed_storage_fingerprint_sha256")?,
            "configuration_unchanged": config_unchanged,
            "repository_unchanged": repository_unchanged,
            "xtask_executable_unchanged": xtask_unchanged,
            "release_executable_unchanged": executable_unchanged,
        },
        "build": {
            "repository_revision": measurement_revision,
            "xtask_executable_sha256": required_value(raw, "/build/xtask_executable_sha256_start")?,
            "app_executable_sha256": required_value(raw, "/build/executable_sha256_start")?,
            "app_build_target_mode": "fresh-private-target",
            "xtask": required_value(raw, "/build/xtask_provenance")?,
            "toolchain": required_value(raw, "/build/xtask_provenance/compiler")?,
        },
        "commands": [
            if diagnostic_requested {
                "cargo run --release -p xtask -- import-performance-t5 --config <private-config> --diagnostic"
            } else if performance_requested {
                "cargo run --release -p xtask -- import-performance-t5 --config <private-config> --performance"
            } else {
                "cargo run --release -p xtask -- import-performance-t5 --config <private-config>"
            },
            "normal native app setup/review/start/open-ready/render/navigation route",
        ],
        "samples": sanitized_samples,
        "gates": {
            "performance_median_primary_wall_time_ns": if performance_requested { Some(median_primary_wall_time_ns) } else { None },
            "performance_timing_gate_evaluated": performance_requested,
            "median_at_most_15_minutes": if performance_requested { Some(performance_timing_gate_passed) } else { None },
            "median_stretch_at_most_10_minutes": if performance_requested { Some(median_primary_wall_time_ns <= PRIMARY_MEDIAN_STRETCH_NS) } else { None },
            "all_per_sample_gates": samples_passed,
            "cross_sample_ids_consistent": cross_sample_ids_consistent,
            "frozen_source_matched": frozen_source_matched,
            "source_preserved": source_preserved,
            "all_invariants": invariant_gates_passed,
            "correctness_qualification_passed": correctness_qualification_passed,
            "performance_qualification_passed": performance_qualification_passed,
            "qualification_passed": qualification_passed,
        },
        "failures": diagnostic_reason_codes,
        "skips": if performance_requested { Vec::<&str>::new() } else { vec!["three_sample_performance_qualification_not_requested"] },
        "waivers": [],
        "remaining_risks": remaining_risks,
        "private_values_redacted": {
            "paths": true,
            "dataset_labels": true,
            "filenames": true,
            "dimensions": true,
            "source_and_package_identities": true,
            "private_source_scientific_and_package_digests": true,
            "opaque_configuration_raw_report_qualification_and_executable_binding_digests_retained": true,
        },
    }))
}

fn private_strings_from_raw(
    raw: &Value,
    raw_report_path: &Path,
    repository_root: &Path,
    config_input: &ConfigInput,
    config: &T5Config,
) -> Vec<String> {
    let mut private_strings = vec![
        raw_report_path.display().to_string(),
        raw_report_path
            .parent()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        config.source.display().to_string(),
        config.scratch_root.display().to_string(),
        config.qualification_profile.display().to_string(),
        config_input.path.display().to_string(),
        repository_root.display().to_string(),
        config.expected.source_inventory_sha256.clone(),
        config.expected.reviewed_source_fingerprint_sha256.clone(),
        config.expected.scientific_content_id.clone(),
    ];
    private_strings.extend(
        config
            .expected
            .scientific_layer_roots
            .iter()
            .map(|root| root.digest_sha256.clone()),
    );
    private_strings.extend(
        config
            .expected
            .scales
            .iter()
            .map(|scale| scale.digest_sha256.clone()),
    );
    if let Some(samples) = raw.pointer("/samples").and_then(Value::as_array) {
        for sample in samples {
            for pointer in [
                "/receipt/package_id",
                "/receipt/scientific_content_id",
                "/receipt/reviewed_source_fingerprint_sha256",
                "/independent_validation/package_id",
                "/independent_validation/scientific_content_id",
            ] {
                if let Some(value) = sample.pointer(pointer).and_then(Value::as_str) {
                    private_strings.push(value.to_owned());
                }
            }
            for pointer in [
                "/independent_validation/layer_roots",
                "/independent_validation/scales",
            ] {
                if let Some(facts) = sample.pointer(pointer).and_then(Value::as_array) {
                    private_strings.extend(facts.iter().filter_map(|fact| {
                        fact.get("digest_sha256")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    }));
                }
            }
        }
    }
    private_strings
}

fn required_value(value: &Value, pointer: &str) -> anyhow::Result<Value> {
    value
        .pointer(pointer)
        .cloned()
        .with_context(|| format!("finalized T5 raw report lacks {pointer}"))
}

fn required_string<'a>(value: &'a Value, pointer: &str) -> anyhow::Result<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .with_context(|| format!("finalized T5 raw report lacks string {pointer}"))
}

fn required_bool(value: &Value, pointer: &str) -> anyhow::Result<bool> {
    value
        .pointer(pointer)
        .and_then(Value::as_bool)
        .with_context(|| format!("finalized T5 raw report lacks Boolean {pointer}"))
}

fn required_pointer_u64(value: &Value, pointer: &str) -> anyhow::Result<u64> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .with_context(|| format!("finalized T5 raw report lacks unsigned integer {pointer}"))
}

fn require_exact_object_keys(value: &Value, expected: &[&str], label: &str) -> anyhow::Result<()> {
    let object = value
        .as_object()
        .with_context(|| format!("{label} is not an object"))?;
    let actual = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    if actual != expected {
        bail!("{label} has an unexpected key set");
    }
    Ok(())
}

pub(crate) fn run_oracle_audit(args: Vec<String>) -> anyhow::Result<()> {
    let args = parse_oracle_audit_args(args)?;
    let build_provenance = qualification_build_provenance();
    require_release_xtask(&build_provenance)?;
    require_standard_app_build_environment(&build_provenance)?;

    let repository_start = repository_identity();
    require_clean_repository(&repository_start)?;
    let build_reasons = qualification_build_reason_codes(&build_provenance, &repository_start);
    if !build_reasons.is_empty() {
        bail!(
            "T5 oracle audit build provenance is not the exact clean release revision: {}",
            build_reasons.join(", ")
        );
    }
    let repository_root = repository_start
        .root
        .as_deref()
        .context("T5 oracle audit could not resolve the repository root")?;
    require_no_external_cargo_configuration(repository_root)?;
    let xtask_executable =
        env::current_exe().context("failed to resolve the T5 oracle xtask executable")?;
    validate_release_executable(&xtask_executable, "T5 oracle xtask")?;
    let xtask_digest_start = sha256_file(&xtask_executable)?;
    let xtask_metadata_start = fs::metadata(&xtask_executable)?;

    let input = read_config_input(&args.config, repository_root)?;
    if config_binding_status(&input.sha256) != "matched" {
        bail!("T5 oracle audit accepts only the exact owner-pinned v2 configuration");
    }
    let config: T5Config = serde_json::from_slice(&input.bytes)
        .context("private T5 oracle-audit configuration is not strict valid JSON")?;
    validate_config(&config)?;

    let protocol = ImportQualificationProtocol::new(
        config.cache_condition.clone(),
        config.competing_activity.clone(),
    );
    let source = canonical_external_source(&config.source, repository_root)?;
    let scratch_root = canonical_external_directory(&config.scratch_root, repository_root)?;
    let qualification_profile = canonical_external_regular_file(
        &config.qualification_profile,
        repository_root,
        IMPORT_QUALIFICATION_PROFILE_MAX_BYTES,
        "qualification profile",
    )?;
    if source.starts_with(&scratch_root) || scratch_root.starts_with(&source) {
        bail!("T5 oracle audit source and scratch paths are not safely disjoint");
    }
    let hardware = host_hardware_identity();
    let source_binding = assess_import_qualification_profile(
        Some(&qualification_profile),
        &repository_start,
        &source,
        &hardware,
        &protocol,
    );
    let scratch_binding = assess_import_qualification_profile(
        Some(&qualification_profile),
        &repository_start,
        &scratch_root,
        &hardware,
        &protocol,
    );
    if !qualification_matched(&source_binding) || !qualification_matched(&scratch_binding) {
        bail!("T5 oracle audit requires the owner-accepted source and scratch profile");
    }

    let cancellation = CommandCancellation::install()?;
    cancellation.check("source-oracle audit setup")?;
    let source_before = source_inventory(&source, cancellation.token())?;
    if source_before.sha256 != config.expected.source_inventory_sha256 {
        bail!("T5 oracle audit source inventory differs from the pinned v2 workload");
    }
    let oracle_scratch =
        OwnedOracleScratch::create(scratch_root.join(format!("oracle-audit-{}", session_id()?)))?;
    let oracle_binding = assess_import_qualification_profile(
        Some(&qualification_profile),
        &repository_start,
        oracle_scratch.path(),
        &hardware,
        &protocol,
    );
    if !qualification_matched(&oracle_binding) {
        bail!("T5 oracle audit scratch is outside the owner-accepted profile");
    }
    let oracle = t5_sentinel_oracle::derive(OracleRequest {
        source: &source,
        scratch: oracle_scratch.path(),
        sentinel: config.no_data_sentinel,
        spacing_zyx_um: config.spacing_zyx_um,
        time_step_seconds: config.time_step_seconds,
        working_memory_bytes: config.working_memory_bytes,
        scale_digest_scheme: &config.expected.scale_digest_scheme,
        cancelled: cancellation.token(),
    })
    .context("private T5 source oracle audit failed")?;
    cancellation.check("source-oracle audit derivation")?;
    let scratch_removed_empty = oracle_scratch.remove_and_report_empty()?;
    let source_after = source_inventory(&source, cancellation.token())?;
    let mut gates = source_oracle_gates(
        &oracle,
        &config.expected,
        &source_before,
        &source_after,
        scratch_removed_empty,
        config.working_memory_bytes,
    );
    gates.insert(
        "accepted_v2_configuration".to_owned(),
        config_binding_status(&input.sha256) == "matched",
    );

    let repository_end = repository_identity();
    let hardware_end = host_hardware_identity();
    let source_binding_end = assess_import_qualification_profile(
        Some(&qualification_profile),
        &repository_end,
        &source,
        &hardware_end,
        &protocol,
    );
    let scratch_binding_end = assess_import_qualification_profile(
        Some(&qualification_profile),
        &repository_end,
        &scratch_root,
        &hardware_end,
        &protocol,
    );
    gates.insert(
        "qualification_profile_unchanged".to_owned(),
        source_binding == source_binding_end && scratch_binding == scratch_binding_end,
    );
    gates.insert(
        "repository_and_hardware_unchanged".to_owned(),
        repository_same_clean_revision(&repository_start, &repository_end)
            && hardware == hardware_end
            && qualification_build_reason_codes(&build_provenance, &repository_end).is_empty(),
    );
    gates.insert(
        "xtask_executable_unchanged".to_owned(),
        sha256_file(&xtask_executable)? == xtask_digest_start
            && same_file_metadata(&xtask_metadata_start, &fs::metadata(&xtask_executable)?),
    );
    gates.insert(
        "configuration_unchanged".to_owned(),
        read_config_input(&input.path, repository_root)? == input,
    );

    let all_gates_passed = gates.values().all(|passed| *passed);
    let report = json!({
        "schema": ORACLE_AUDIT_REPORT_SCHEMA,
        "status": if all_gates_passed { "passed" } else { "failed" },
        "gates": gates,
        "all_gates_passed": all_gates_passed,
        "private_values_redacted": true,
    });
    let mut private_strings = vec![
        source.display().to_string(),
        scratch_root.display().to_string(),
        qualification_profile.display().to_string(),
        input.path.display().to_string(),
        repository_root.display().to_string(),
        source_before.sha256.clone(),
        source_after.sha256.clone(),
        input.sha256.clone(),
        config.expected.source_inventory_sha256.clone(),
        config.expected.reviewed_source_fingerprint_sha256.clone(),
        config.expected.scientific_content_id.clone(),
        oracle.scientific_content_id.clone(),
    ];
    private_strings.extend(
        config
            .expected
            .scientific_layer_roots
            .iter()
            .map(|root| root.digest_sha256.clone()),
    );
    private_strings.extend(
        config
            .expected
            .scales
            .iter()
            .map(|scale| scale.digest_sha256.clone()),
    );
    private_strings.extend(
        oracle
            .layer_roots
            .iter()
            .map(|root| root.digest_sha256.clone()),
    );
    private_strings.extend(
        oracle
            .scales
            .iter()
            .map(|scale| scale.digest_sha256.clone()),
    );
    validate_sanitized_report(&report, &private_strings)?;
    cancellation.check("source-oracle audit report")?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !all_gates_passed {
        bail!("T5 source-oracle audit failed one or more sanitized gates");
    }
    cancellation.check("source-oracle audit completion")?;
    Ok(())
}

fn parse_oracle_audit_args(args: Vec<String>) -> anyhow::Result<OracleAuditArgs> {
    let mut config = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--config" => {
                index += 1;
                let value = PathBuf::from(
                    args.get(index)
                        .context("--config requires an absolute path")?,
                );
                if !value.is_absolute() {
                    bail!("import-performance-t5-oracle-audit requires an absolute --config path");
                }
                if config.replace(value).is_some() {
                    bail!("import-performance-t5-oracle-audit accepts --config exactly once");
                }
            }
            "--help" | "-h" | "help" => bail!(
                "usage: cargo run --release -p xtask -- import-performance-t5-oracle-audit --config /absolute/private/config.json"
            ),
            option => bail!("unknown import-performance-t5-oracle-audit option {option:?}"),
        }
        index += 1;
    }
    Ok(OracleAuditArgs {
        config: config.context("import-performance-t5-oracle-audit requires --config PATH")?,
    })
}

fn parse_publish_args(args: Vec<String>) -> anyhow::Result<PublishArgs> {
    let mut config = None;
    let mut raw_report = None;
    let mut index = 0;
    while index < args.len() {
        let (slot, label) = match args[index].as_str() {
            "--config" => (&mut config, "--config"),
            "--raw-report" => (&mut raw_report, "--raw-report"),
            "--help" | "-h" | "help" => bail!(
                "usage: cargo run --release -p xtask -- import-performance-t5-publish --config /absolute/private/config.json --raw-report /absolute/private/raw-private-report.json"
            ),
            option => bail!("unknown import-performance-t5-publish option {option:?}"),
        };
        index += 1;
        let value = PathBuf::from(
            args.get(index)
                .with_context(|| format!("{label} requires an absolute path"))?,
        );
        if !value.is_absolute() {
            bail!("import-performance-t5-publish requires an absolute {label} path");
        }
        if slot.replace(value).is_some() {
            bail!("import-performance-t5-publish accepts {label} exactly once");
        }
        index += 1;
    }
    Ok(PublishArgs {
        config: config.context("import-performance-t5-publish requires --config PATH")?,
        raw_report: raw_report
            .context("import-performance-t5-publish requires --raw-report PATH")?,
    })
}

fn parse_args(args: Vec<String>) -> anyhow::Result<RunArgs> {
    let mut config = None;
    let mut diagnostic = false;
    let mut performance = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--config" => {
                index += 1;
                let value = PathBuf::from(
                    args.get(index)
                        .context("--config requires an absolute path")?,
                );
                if !value.is_absolute() {
                    bail!("import-performance-t5 requires an absolute --config path");
                }
                if config.replace(value).is_some() {
                    bail!("import-performance-t5 accepts --config exactly once");
                }
            }
            "--diagnostic" => diagnostic = true,
            "--performance" => performance = true,
            "--help" | "-h" | "help" => bail!(
                "usage: cargo run --release -p xtask -- import-performance-t5 --config /absolute/private/config.json [--performance | --diagnostic]"
            ),
            option => bail!("unknown import-performance-t5 option {option:?}"),
        }
        index += 1;
    }
    let config = config.context("import-performance-t5 requires --config PATH")?;
    if diagnostic && performance {
        bail!("--diagnostic and --performance are mutually exclusive");
    }
    let qualification_kind = if performance {
        QualificationKind::Performance
    } else {
        QualificationKind::Correctness
    };
    let samples = if performance {
        PERFORMANCE_SAMPLES
    } else {
        CORRECTNESS_SAMPLES
    };
    Ok(RunArgs {
        config,
        samples,
        diagnostic,
        qualification_kind,
    })
}

fn validate_config(config: &T5Config) -> anyhow::Result<()> {
    if config.schema != CONFIG_SCHEMA {
        bail!("private T5 configuration schema mismatch");
    }
    if !valid_workload_id(&config.workload_id) {
        bail!("T5 workload_id must be `t5-` followed by exactly 32 lowercase hex digits");
    }
    for (field, path) in [
        ("source", &config.source),
        ("scratch_root", &config.scratch_root),
        ("qualification_profile", &config.qualification_profile),
    ] {
        if !path.is_absolute() {
            bail!("private T5 configuration {field} must be absolute");
        }
    }
    if config.expected_profile != ProfileKind::Current.name() {
        bail!(
            "private T5 qualification requires expected_profile {}",
            ProfileKind::Current.name()
        );
    }
    if config.working_memory_bytes != WORKING_MEMORY_BYTES {
        bail!("private T5 qualification requires the exact 256 MiB import budget");
    }
    if !(60..=7_200).contains(&config.primary_timeout_seconds) {
        bail!("private T5 primary_timeout_seconds must be between 60 and 7200");
    }
    if config
        .spacing_zyx_um
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        bail!("private T5 spacing must contain finite positive micrometers");
    }
    if config
        .time_step_seconds
        .is_some_and(|value| !value.is_finite() || value <= 0.0)
    {
        bail!("private T5 time step must be finite and positive when present");
    }
    if config.cache_condition != "warm" {
        bail!(
            "private T5 cache_condition must be `warm`; source-inventory proof prewarms the workload"
        );
    }
    if config.competing_activity != "none" {
        bail!("private T5 competing_activity must be the public declaration `none`");
    }
    if config.expected.expected_fact_authority != FACT_AUTHORITY {
        bail!("private T5 expected-fact authority is not the source sentinel oracle");
    }
    if config.expected.canonical_source_pixel_bytes == 0 {
        bail!("private T5 expected canonical source pixel bytes must be nonzero");
    }
    Sha256Digest::parse(&config.expected.source_inventory_sha256)
        .context("private T5 expected source-inventory digest is invalid")?;
    Sha256Digest::parse(&config.expected.reviewed_source_fingerprint_sha256)
        .context("private T5 expected reviewed-source fingerprint is invalid")?;
    ScientificContentId::parse(&config.expected.scientific_content_id)
        .context("private T5 expected scientific content ID is invalid")?;
    if config.expected.scale_digest_scheme != SCALE_DIGEST_SCHEME {
        bail!("private T5 scale digest scheme is not the frozen supported scheme");
    }
    if config.expected.scientific_layer_roots.is_empty() || config.expected.scales.is_empty() {
        bail!("private T5 expected scientific and per-scale facts are mandatory");
    }
    let mut layer_keys = BTreeSet::new();
    for root in &config.expected.scientific_layer_roots {
        if !layer_keys.insert(root.logical_layer) {
            bail!("private T5 expected layer roots contain a duplicate logical layer");
        }
        Sha256Digest::parse(&root.digest_sha256)
            .context("private T5 expected layer-root digest is invalid")?;
    }
    let mut scale_keys = BTreeSet::new();
    for scale in &config.expected.scales {
        if !scale_keys.insert((scale.image_ordinal, scale.scale_ordinal)) {
            bail!("private T5 expected scale facts contain a duplicate image/scale");
        }
        if scale.brick_reads == 0 || scale.logical_voxels == 0 {
            bail!("private T5 expected scale facts must contain nonzero counts");
        }
        Sha256Digest::parse(&scale.digest_sha256)
            .context("private T5 expected scale digest is invalid")?;
    }
    let required_scale_keys = (0..T5_SCALE_COUNT)
        .map(|scale| (0_u32, u32::try_from(scale).expect("seven scales fit u32")))
        .collect::<BTreeSet<_>>();
    if scale_keys != required_scale_keys {
        bail!("private T5 expected facts require exactly image zero scales zero through six");
    }
    if config.expected.transforms.len() != T5_SCALE_COUNT {
        bail!("private T5 expected facts require exactly seven centered transforms");
    }
    let mut transform_ordinals = BTreeSet::new();
    for transform in &config.expected.transforms {
        if !transform_ordinals.insert(transform.scale_ordinal) {
            bail!("private T5 expected transforms contain a duplicate scale ordinal");
        }
        if transform
            .scale_zyx
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
            || transform.translation_zyx.iter().any(|value| {
                !value.is_finite() || *value < 0.0 || value.to_bits() == (-0.0_f64).to_bits()
            })
        {
            bail!("private T5 expected transforms contain invalid finite coordinates");
        }
    }
    let required_transform_ordinals = (0..T5_SCALE_COUNT)
        .map(|scale| u32::try_from(scale).expect("seven scales fit u32"))
        .collect::<BTreeSet<_>>();
    if transform_ordinals != required_transform_ordinals {
        bail!("private T5 expected transforms require unique ordinals zero through six");
    }
    Ok(())
}

fn valid_workload_id(value: &str) -> bool {
    value.len() == 35
        && value.starts_with("t5-")
        && value[3..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn require_release_xtask(provenance: &QualificationBuildProvenance) -> anyhow::Result<()> {
    if cfg!(debug_assertions)
        || provenance.cargo_profile != "release"
        || provenance.opt_level != "3"
        || provenance.debug != "false"
    {
        bail!(
            "T5 evidence requires the standard release xtask: cargo run --release -p xtask -- import-performance-t5 ..."
        );
    }
    Ok(())
}

pub(crate) fn require_standard_app_build_environment(
    provenance: &QualificationBuildProvenance,
) -> anyhow::Result<()> {
    for variable in [
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTC_BOOTSTRAP",
    ] {
        if env::var_os(variable).is_some_and(|value| !value.is_empty()) {
            bail!("T5 release app build rejects custom compiler flags or wrappers ({variable})");
        }
    }
    for (name, value) in env::vars_os() {
        let name = name.to_string_lossy();
        if is_compiler_or_profile_override(&name, &value, provenance.custom_rustflags) {
            bail!("T5 release app build rejects compiler/profile override {name}");
        }
        if !value.is_empty()
            && (matches!(
                name.as_ref(),
                "CARGO_BUILD_RUSTC"
                    | "CARGO_BUILD_RUSTC_WRAPPER"
                    | "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"
                    | "CARGO_BUILD_TARGET"
            ) || (name.starts_with("CARGO_TARGET_")
                && (name.ends_with("_LINKER") || name.ends_with("_RUNNER"))))
        {
            bail!("T5 release app build rejects Cargo toolchain override {name}");
        }
    }
    let output = Command::new("rustc")
        .args(["--version", "--verbose"])
        .output()
        .context("T5 could not identify the release-app compiler")?;
    let compiler = output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .context("T5 could not identify the release-app compiler")?;
    if compiler != provenance.compiler.trim() {
        bail!("T5 release app and xtask must use the same recorded compiler");
    }
    Ok(())
}

fn is_compiler_or_profile_override(
    name: &str,
    value: &OsStr,
    embedded_custom_rustflags: bool,
) -> bool {
    if value.is_empty() {
        return false;
    }
    // Cargo exposes `cargo:rustc-env` values to a `cargo run` child. This
    // exact matching value is immutable xtask provenance, not a user build
    // flag; the embedded boolean is checked independently before any app build.
    let matching_provenance_marker = name == "MIRANTE4D_XTASK_BUILD_CUSTOM_RUSTFLAGS"
        && value
            == OsStr::new(if embedded_custom_rustflags {
                "true"
            } else {
                "false"
            });
    !matching_provenance_marker
        && (name == "RUSTC"
            || name == "RUSTFLAGS"
            || name.ends_with("_RUSTFLAGS")
            || name.starts_with("CARGO_PROFILE_RELEASE_"))
}

pub(crate) fn require_no_external_cargo_configuration(
    repository_root: &Path,
) -> anyhow::Result<()> {
    let repository_root = fs::canonicalize(repository_root)?;
    if let Some(parent) = repository_root.parent() {
        for ancestor in parent.ancestors() {
            reject_cargo_config_at(&ancestor.join(".cargo"))?;
        }
    }
    if let Some(cargo_home) = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
    {
        reject_cargo_config_at(&cargo_home)?;
    }
    Ok(())
}

fn reject_cargo_config_at(directory: &Path) -> anyhow::Result<()> {
    for name in ["config", "config.toml"] {
        let path = directory.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => bail!(
                "T5 release app build rejects external Cargo configuration at {}",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn require_owner_attested_x11_display() -> anyhow::Result<()> {
    if env::var_os("DISPLAY").is_none() || env::var_os("WAYLAND_DISPLAY").is_some() {
        bail!("T5 qualification requires owner-attested native X11 mapped-window evidence");
    }
    if env_flag("CI")
        || env_flag("GITHUB_ACTIONS")
        || ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "VNCDESKTOP"]
            .into_iter()
            .any(|name| env::var_os(name).is_some_and(|value| !value.is_empty()))
    {
        bail!("T5 qualification rejects CI and obvious remote or VNC display sessions");
    }
    if env::var("MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS")
        .ok()
        .as_deref()
        != Some("real_display")
    {
        bail!(
            "declare MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display; external xdotool/xwininfo proof remains mandatory"
        );
    }
    for (program, argument) in [("xdotool", "version"), ("xwininfo", "-version")] {
        let output = Command::new(program)
            .arg(argument)
            .output()
            .with_context(|| format!("T5 mapped X11 proof requires {program}"))?;
        if !output.status.success() {
            bail!("T5 mapped X11 proof tool {program} is unavailable");
        }
    }
    Ok(())
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .ok()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

fn require_clean_repository(repository: &RepositoryIdentity) -> anyhow::Result<()> {
    if repository.root.is_none()
        || repository.commit.is_none()
        || repository.dirty_worktree != Some(false)
    {
        bail!("T5 evidence requires one named clean repository revision");
    }
    Ok(())
}

fn read_config_input(path: &Path, repository_root: &Path) -> anyhow::Result<ConfigInput> {
    let canonical = canonical_external_regular_file(
        path,
        repository_root,
        CONFIG_BYTES_MAX,
        "private T5 configuration",
    )?;
    let mut file = File::open(&canonical)?;
    let expected = file.metadata()?.len();
    let mut bytes = Vec::with_capacity(usize::try_from(expected).unwrap_or(0));
    Read::by_ref(&mut file)
        .take(CONFIG_BYTES_MAX + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > CONFIG_BYTES_MAX
        || u64::try_from(bytes.len()).ok() != Some(expected)
        || file.metadata()?.len() != expected
    {
        bail!("private T5 configuration changed while read or exceeds its bound");
    }
    let sha256 = sha256_bytes(&bytes);
    Ok(ConfigInput {
        path: canonical,
        bytes,
        sha256,
    })
}

fn canonical_external_regular_file(
    path: &Path,
    repository_root: &Path,
    maximum_bytes: u64,
    description: &str,
) -> anyhow::Result<PathBuf> {
    let link_metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {description} at {}", path.display()))?;
    if link_metadata.file_type().is_symlink() || !link_metadata.is_file() {
        bail!("{description} must be a nonsymlink regular file");
    }
    if link_metadata.len() == 0 || link_metadata.len() > maximum_bytes {
        bail!("{description} has an invalid bounded length");
    }
    let canonical = fs::canonicalize(path)?;
    let repository_root = fs::canonicalize(repository_root)?;
    if canonical.starts_with(repository_root) {
        bail!("{description} must remain outside the repository");
    }
    let metadata = fs::metadata(&canonical)?;
    if !metadata.is_file() || metadata.len() > maximum_bytes {
        bail!("{description} changed type or length while resolved");
    }
    Ok(canonical)
}

fn canonical_external_directory(path: &Path, repository_root: &Path) -> anyhow::Result<PathBuf> {
    let link_metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect external directory {}", path.display()))?;
    if link_metadata.file_type().is_symlink() || !link_metadata.is_dir() {
        bail!("T5 scratch root must be a nonsymlink directory");
    }
    let canonical = fs::canonicalize(path)?;
    if canonical.starts_with(fs::canonicalize(repository_root)?) {
        bail!("T5 scratch root must remain outside the repository");
    }
    Ok(canonical)
}

fn canonical_external_source(path: &Path, repository_root: &Path) -> anyhow::Result<PathBuf> {
    let link_metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect private T5 source {}", path.display()))?;
    if link_metadata.file_type().is_symlink()
        || (!link_metadata.is_file() && !link_metadata.is_dir())
    {
        bail!("private T5 source must be a nonsymlink regular file or directory");
    }
    let canonical = fs::canonicalize(path)?;
    if canonical.starts_with(fs::canonicalize(repository_root)?) {
        bail!("private T5 source must remain outside the repository");
    }
    Ok(canonical)
}

fn config_binding_status(digest: &str) -> &'static str {
    match OWNER_ACCEPTED_T5_CONFIG_SHA256 {
        Some(accepted) if accepted == digest => "matched",
        Some(_) => "owner_digest_mismatch",
        None => "pending_owner_acceptance",
    }
}

fn qualification_matched(assessment: &ImportQualificationAssessment) -> bool {
    assessment.status == "matched"
}

fn qualification_tuple_matched(assessment: &ImportQualificationAssessment) -> bool {
    matches!(
        assessment.status,
        "matched"
            | "binding_matched_pending_owner_acceptance"
            | "binding_matched_but_owner_digest_mismatch"
    )
}

const fn correctness_qualification_claim_passed(
    diagnostic: bool,
    binding_eligible: bool,
    samples_passed: bool,
    invariant_gates_passed: bool,
) -> bool {
    !diagnostic && binding_eligible && samples_passed && invariant_gates_passed
}

const fn performance_qualification_claim_passed(
    performance_requested: bool,
    correctness_passed: bool,
    timing_gate_passed: bool,
    cross_sample_ids_consistent: Option<bool>,
) -> bool {
    performance_requested
        && correctness_passed
        && timing_gate_passed
        && matches!(cross_sample_ids_consistent, Some(true))
}

#[allow(clippy::too_many_arguments)]
fn qualification_diagnostic_reason_codes(
    args: &RunArgs,
    config_binding_status: &str,
    source_start: &ImportQualificationAssessment,
    scratch_start: &ImportQualificationAssessment,
    session_start: &ImportQualificationAssessment,
    source_end: &ImportQualificationAssessment,
    scratch_end: &ImportQualificationAssessment,
    session_end: &ImportQualificationAssessment,
    build_start: &[&'static str],
    build_end: &[&'static str],
    samples: &[SampleEvidence],
    performance_timing_gate_passed: bool,
    frozen_source_matched: bool,
    source_preserved: bool,
    repository_unchanged: bool,
    hardware_unchanged: bool,
    xtask_unchanged: bool,
    executable_unchanged: bool,
    config_unchanged: bool,
    cross_sample_ids_consistent: Option<bool>,
) -> Vec<String> {
    let mut reasons = BTreeSet::new();
    if args.diagnostic {
        reasons.insert("diagnostic_mode_explicitly_requested".to_owned());
    }
    match config_binding_status {
        "matched" => {}
        "pending_owner_acceptance" => {
            reasons.insert("t5_configuration_pending_owner_acceptance".to_owned());
        }
        _ => {
            reasons.insert("t5_configuration_owner_digest_mismatch".to_owned());
        }
    }
    let assessments = [
        source_start,
        scratch_start,
        session_start,
        source_end,
        scratch_end,
        session_end,
    ];
    if assessments
        .iter()
        .any(|assessment| !qualification_matched(assessment))
    {
        reasons.insert("qualification_profile_not_matched_at_every_boundary".to_owned());
    }
    for assessment in assessments {
        reasons.extend(
            assessment
                .reason_codes
                .iter()
                .map(|reason| (*reason).to_owned()),
        );
    }
    reasons.extend(build_start.iter().map(|reason| (*reason).to_owned()));
    reasons.extend(build_end.iter().map(|reason| (*reason).to_owned()));
    for sample in samples {
        for gate in &sample.failed_gates {
            reasons.insert(format!("sample_{}_{gate}_failed", sample.sample_index));
        }
    }
    if args.qualification_kind == QualificationKind::Performance && !performance_timing_gate_passed
    {
        reasons.insert("performance_absolute_time_gate_failed".to_owned());
    }
    if args.qualification_kind == QualificationKind::Performance
        && cross_sample_ids_consistent != Some(true)
    {
        reasons.insert("performance_sample_identities_not_deterministic".to_owned());
    }
    for (passed, reason) in [
        (
            frozen_source_matched,
            "source_inventory_does_not_match_frozen_workload",
        ),
        (source_preserved, "source_inventory_changed"),
        (repository_unchanged, "repository_identity_changed"),
        (hardware_unchanged, "hardware_identity_changed"),
        (xtask_unchanged, "xtask_executable_changed"),
        (executable_unchanged, "release_executable_changed"),
        (config_unchanged, "private_configuration_changed"),
    ] {
        if !passed {
            reasons.insert(reason.to_owned());
        }
    }
    reasons.into_iter().collect()
}

fn require_not_cancelled(cancelled: &AtomicBool, phase: &str) -> anyhow::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        bail!("T5 command cancelled by SIGINT or SIGTERM during {phase}");
    }
    Ok(())
}

fn source_inventory(root: &Path, cancelled: &AtomicBool) -> anyhow::Result<InventoryFacts> {
    require_not_cancelled(cancelled, "source inventory")?;
    let root_metadata = fs::symlink_metadata(root)?;
    let mut pending = Vec::new();
    let mut files = Vec::<(Vec<u8>, PathBuf)>::new();
    if root_metadata.is_file() {
        let relative = root
            .file_name()
            .context("private T5 source file has no name")?
            .as_bytes()
            .to_vec();
        files.push((relative, root.to_path_buf()));
    } else if root_metadata.is_dir() {
        pending.push(root.to_path_buf());
    } else {
        bail!("private T5 source inventory root changed type");
    }
    while let Some(directory) = pending.pop() {
        require_not_cancelled(cancelled, "source inventory")?;
        for entry in fs::read_dir(&directory)? {
            require_not_cancelled(cancelled, "source inventory")?;
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                bail!("private T5 source inventory contains a symbolic link");
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                let relative = path.strip_prefix(root)?.as_os_str().as_bytes().to_vec();
                files.push((relative, path));
                if files.len() > SOURCE_ENTRY_MAX {
                    bail!("private T5 source inventory exceeds its entry bound");
                }
            } else {
                bail!("private T5 source inventory contains a special entry");
            }
        }
    }
    if files.is_empty() {
        bail!("private T5 source inventory is empty");
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256Hasher::new();
    hasher.update(b"mirante4d-t5-source-inventory-1\0");
    let mut source_bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    for (relative, path) in &files {
        require_not_cancelled(cancelled, "source inventory")?;
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("private T5 source changed type during inventory");
        }
        let length = metadata.len();
        source_bytes = source_bytes
            .checked_add(length)
            .context("private T5 source byte count overflowed")?;
        hasher.update(u64::try_from(relative.len())?.to_le_bytes());
        hasher.update(relative);
        hasher.update(length.to_le_bytes());
        let mut file = File::open(path)?;
        let mut consumed = 0_u64;
        loop {
            require_not_cancelled(cancelled, "source inventory")?;
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            consumed = consumed
                .checked_add(u64::try_from(count)?)
                .context("private T5 inventory byte count overflowed")?;
            hasher.update(&buffer[..count]);
        }
        if consumed != length || fs::symlink_metadata(path)?.len() != length {
            bail!("private T5 source changed while its inventory was captured");
        }
    }
    Ok(InventoryFacts {
        regular_files: u64::try_from(files.len())?,
        source_bytes,
        sha256: hasher.finalize().to_string(),
    })
}

fn successful_publication_to_open_ready_clock(report: &Value) -> anyhow::Result<&Value> {
    let clock = report
        .pointer("/import_workflow_evidence/publication_to_open_ready_clock")
        .context("T5 automation report lacks publication-to-open-ready clock evidence")?;
    if clock.get("status").and_then(Value::as_str) != Some(IMPORT_OPEN_READY_COMPLETE_STATUS) {
        bail!("T5 successful workflow does not carry complete open-ready transfer evidence");
    }
    Ok(clock)
}

#[allow(clippy::too_many_arguments)]
fn run_sample(
    sample_index: usize,
    config: &T5Config,
    source: &Path,
    source_inventory: &InventoryFacts,
    session_root: &Path,
    startup_package: &Path,
    executable: &Path,
    qualification_profile: &Path,
    repository: &RepositoryIdentity,
    hardware: &HostHardwareIdentity,
    protocol: &ImportQualificationProtocol,
    compiler: &str,
    diagnostic: bool,
    cancelled: &AtomicBool,
) -> anyhow::Result<SampleEvidence> {
    require_not_cancelled(cancelled, "T5 sample setup")?;
    let sample_root = session_root.join(format!("sample-{sample_index:02}"));
    create_private_directory(&sample_root)?;
    let output_parent = sample_root.join("output");
    create_private_directory(&output_parent)?;
    let destination =
        deterministic_tiff_destination(&TiffSource::single_3d(source), &output_parent);
    let checkpoint = checkpoint_directory(&destination)?;
    if destination.exists() || checkpoint.exists() {
        bail!("T5 sample destination or checkpoint was stale before launch");
    }
    let sample_root_binding_start = assess_import_qualification_profile(
        Some(qualification_profile),
        repository,
        &sample_root,
        hardware,
        protocol,
    );
    let output_parent_binding_start = assess_import_qualification_profile(
        Some(qualification_profile),
        repository,
        &output_parent,
        hardware,
        protocol,
    );
    if matches!(sample_root_binding_start.status, "invalid" | "rejected")
        || matches!(output_parent_binding_start.status, "invalid" | "rejected")
    {
        bail!("unsafe T5 per-sample storage binding");
    }
    if (!qualification_matched(&sample_root_binding_start)
        || !qualification_matched(&output_parent_binding_start)
        || sample_root_binding_start != output_parent_binding_start)
        && !diagnostic
    {
        bail!("T5 per-sample storage does not match the owner-accepted profile");
    }

    let script_path = sample_root.join("automation-script.json");
    let automation_report_path = sample_root.join("automation-report.json");
    let stdout_path = sample_root.join("stdout.log");
    let stderr_path = sample_root.join("stderr.log");
    let app_log_path = sample_root.join("app.log");
    let config_home = sample_root.join("config-home");
    let cache_home = sample_root.join("cache-home");
    let data_home = sample_root.join("data-home");
    let state_home = sample_root.join("state-home");
    let temp_home = sample_root.join("tmp");
    for directory in [
        &config_home,
        &cache_home,
        &data_home,
        &state_home,
        &temp_home,
    ] {
        create_private_directory(directory)?;
    }
    let script = automation_script(
        config,
        startup_package,
        source,
        &output_parent,
        &destination,
    )?;
    let progress_plan = ProductAutomationProgressPlan::from_script(&script)
        .context("T5 automation progress plan is invalid")?;
    let progress_launch = ProductAutomationProgressLaunch::new(&sample_root)
        .context("T5 automation progress launch is invalid")?;
    write_new_synced_json(&script_path, &script)?;

    let observation = run_app_process(
        AppRun {
            executable,
            startup_package,
            script: &script_path,
            automation_report: &automation_report_path,
            stdout: &stdout_path,
            stderr: &stderr_path,
            app_log: &app_log_path,
            config_home: &config_home,
            cache_home: &cache_home,
            data_home: &data_home,
            state_home: &state_home,
            temp_home: &temp_home,
            timeout: Duration::from_secs(
                config
                    .primary_timeout_seconds
                    .checked_add(INSPECTION_TIMEOUT_SECONDS)
                    .and_then(|seconds| seconds.checked_add(POST_PRIMARY_TIMEOUT_SECONDS))
                    .context("T5 process timeout overflowed")?,
            ),
            progress_plan,
            progress_launch,
        },
        cancelled,
    )?;
    require_not_cancelled(cancelled, "T5 application process")?;
    if !observation.exit_success {
        bail!("T5 app process did not exit successfully");
    }
    let mapped_window = observation
        .mapped_window
        .context("T5 app never exposed the exact externally observed mapped client area")?;
    let automation_report = read_bounded_json(&automation_report_path, 32 * 1024 * 1024)?;
    validate_product_automation_report_contract(&automation_report, &script, &script_path)
        .context("T5 product automation report contract is invalid")?;
    let reported_binary = automation_report
        .get("binary")
        .and_then(Value::as_str)
        .context("T5 automation report lacks its running executable path")?;
    if fs::canonicalize(reported_binary)? != fs::canonicalize(executable)? {
        bail!("T5 automation report came from an unexpected app executable");
    }
    validate_app_build_provenance(&automation_report, repository, compiler)?;
    validate_navigation_evidence(&automation_report)?;

    let primary = automation_report
        .pointer("/import_workflow_evidence/primary_clock")
        .context("T5 automation report lacks exact primary-clock evidence")?;
    let inspection_and_review = automation_report
        .pointer("/import_workflow_evidence/inspection_and_review_clock")
        .context("T5 automation report lacks separate inspection/review-clock evidence")?;
    let publication_to_open_ready = successful_publication_to_open_ready_clock(&automation_report)?;
    if inspection_and_review
        .get("start_boundary")
        .and_then(Value::as_str)
        != Some("normal_import_setup_command_dispatch")
        || inspection_and_review
            .get("end_boundary")
            .and_then(Value::as_str)
            != Some("reviewed_start_import_command_dispatch")
        || inspection_and_review
            .get("excluded_from_primary_clock")
            .and_then(Value::as_bool)
            != Some(true)
    {
        bail!("T5 app inspection/review-clock boundaries drifted");
    }
    let inspection_and_review_wall_time_ns = required_u64(inspection_and_review, "wall_time_ns")?;
    let inspection_and_review_cpu_time_ns =
        required_u64(inspection_and_review, "process_cpu_time_ns")?;
    let primary_wall_time_ns = required_u64(primary, "wall_time_ns")?;
    let primary_cpu_time_ns = required_u64(primary, "process_cpu_time_ns")?;
    let publication_to_open_ready_wall_time_ns =
        required_u64(publication_to_open_ready, "wall_time_ns")?;
    let publication_to_open_ready_cpu_time_ns =
        required_u64(publication_to_open_ready, "process_cpu_time_ns")?;
    let primary_started_epoch_ms = u128::from(required_u64(primary, "started_at_epoch_ms")?);
    let open_ready_epoch_ms = u128::from(required_u64(primary, "open_ready_at_epoch_ms")?);
    let inspection_started_epoch_ms =
        u128::from(required_u64(inspection_and_review, "started_at_epoch_ms")?);
    let start_command_epoch_ms = u128::from(required_u64(
        inspection_and_review,
        "start_command_at_epoch_ms",
    )?);
    let published_epoch_ms = u128::from(required_u64(
        publication_to_open_ready,
        "published_at_epoch_ms",
    )?);
    let post_open_ready_epoch_ms = u128::from(required_u64(
        publication_to_open_ready,
        "open_ready_at_epoch_ms",
    )?);
    if start_command_epoch_ms < inspection_started_epoch_ms
        || start_command_epoch_ms > primary_started_epoch_ms
    {
        bail!("T5 inspection/review and primary clock epochs are not ordered");
    }
    if primary.get("start_boundary").and_then(Value::as_str)
        != Some("accepted_start_import_command_immediately_before_worker_spawn")
        || primary.get("end_boundary").and_then(Value::as_str)
            != Some("published_destination_admitted_and_open_ready_for_normal_product_use")
    {
        bail!("T5 app primary-clock boundaries drifted");
    }
    validate_publication_currentness_execution(publication_to_open_ready)?;
    for field in [
        "package_integrity_audit_started_runs",
        "package_integrity_audit_progress_updates",
        "package_integrity_audit_cancelled_runs",
        "package_integrity_audit_failed_runs",
        "package_integrity_audit_completed_runs",
    ] {
        if required_u64(publication_to_open_ready, field)? != 0 {
            bail!("T5 imported capability unexpectedly performed {field}");
        }
    }
    if publication_to_open_ready
        .get("start_boundary")
        .and_then(Value::as_str)
        != Some("import_worker_published_event")
        || publication_to_open_ready
            .get("end_boundary")
            .and_then(Value::as_str)
            != Some("published_destination_admitted_and_open_ready_for_normal_product_use")
        || publication_to_open_ready
            .get("included_in_primary_clock")
            .and_then(Value::as_bool)
            != Some(true)
        || publication_to_open_ready
            .get("transfer_mode")
            .and_then(Value::as_str)
            != Some("staged_self_consistent_capability")
        || published_epoch_ms < primary_started_epoch_ms
        || published_epoch_ms > open_ready_epoch_ms
        || post_open_ready_epoch_ms != open_ready_epoch_ms
        || publication_to_open_ready_wall_time_ns > primary_wall_time_ns
        || publication_to_open_ready_cpu_time_ns > primary_cpu_time_ns
    {
        bail!("T5 app publication-to-open-ready clock is outside its primary clock");
    }
    let (idle_rss_bytes, primary_peak_rss_bytes, external_rss_delta_bytes) = classify_rss_samples(
        &observation.rss_samples,
        primary_started_epoch_ms,
        open_ready_epoch_ms,
    )?;

    let receipt = automation_report
        .pointer("/import_workflow_evidence/last_successful_receipt")
        .context("T5 automation report lacks the full successful receipt")?;
    let (reviewed_source_fingerprint_sha256, reviewed_source_bytes) =
        validate_successful_receipt_binding(
            &automation_report,
            publication_to_open_ready,
            receipt,
            &destination,
        )?;
    let receipt_package_id = receipt
        .get("package_id")
        .and_then(Value::as_str)
        .context("T5 receipt lacks package ID")?
        .to_owned();
    let receipt_scientific_id = receipt
        .get("scientific_content_id")
        .and_then(Value::as_str)
        .context("T5 receipt lacks scientific content ID")?
        .to_owned();
    let statistics = receipt
        .get("statistics")
        .context("T5 receipt lacks import statistics")?;
    validate_required_statistics(statistics)?;

    require_not_cancelled(cancelled, "independent package validation")?;
    let independent = independently_validate_package(&destination, &config.expected, cancelled)?;
    require_not_cancelled(cancelled, "independent package validation")?;
    let destination_binding = assess_import_qualification_profile(
        Some(qualification_profile),
        repository,
        &destination,
        hardware,
        protocol,
    );
    let sample_root_binding_end = assess_import_qualification_profile(
        Some(qualification_profile),
        repository,
        &sample_root,
        hardware,
        protocol,
    );
    if matches!(destination_binding.status, "invalid" | "rejected")
        || matches!(sample_root_binding_end.status, "invalid" | "rejected")
    {
        bail!("unsafe T5 published-destination storage binding");
    }
    if (!qualification_matched(&destination_binding)
        || destination_binding != output_parent_binding_start
        || sample_root_binding_end != sample_root_binding_start)
        && !diagnostic
    {
        bail!("T5 published destination drifted from the owner-accepted storage profile");
    }
    if independent.package_id != receipt_package_id
        || independent.scientific_content_id != receipt_scientific_id
    {
        bail!("T5 receipt identities differ from independent post-open validation");
    }

    let base_bricks = config
        .expected
        .scales
        .iter()
        .filter(|fact| fact.scale_ordinal == 0)
        .try_fold(0_u64, |total, fact| total.checked_add(fact.brick_reads))
        .context("T5 expected base-brick count overflowed")?;
    if base_bricks == 0 {
        bail!("T5 expected facts contain no base-scale bricks");
    }
    let source_bytes_read = required_u64(statistics, "source_bytes_read")?;
    let source_revalidation_bytes_read =
        required_u64(statistics, "source_revalidation_bytes_read")?;
    let base_native_decoded_bytes = required_u64(statistics, "base_native_decoded_bytes")?;
    let no_data_detection_native_decoded_bytes =
        required_u64(statistics, "no_data_detection_native_decoded_bytes")?;
    let scientific_identity_native_decoded_bytes =
        required_u64(statistics, "scientific_identity_native_decoded_bytes")?;
    let sync_calls = required_u64(statistics, "sync_calls")?;
    let scientific_brick_reads = required_u64(statistics, "scientific_brick_reads")?;
    let peak_open_file_descriptors = required_u64(statistics, "peak_open_file_descriptors")?;
    let preflight_required_headroom_bytes =
        required_u64(statistics, "preflight_required_headroom_bytes")?;
    let peak_checkpoint_regular_files = required_u64(statistics, "peak_checkpoint_regular_files")?;
    let peak_working_bytes = required_u64(statistics, "peak_working_bytes")?;
    let stages_truthful =
        progress_truthfulness_present(&automation_report, statistics, &config.expected.scales)?;
    let counters_reconciled = import_counters_reconciled(&automation_report, statistics)?;
    let rss_minus_ledger = i128::from(external_rss_delta_bytes) - i128::from(peak_working_bytes);
    let rss_minus_ledger = i64::try_from(rss_minus_ledger)
        .context("T5 external RSS-to-ledger reconciliation exceeds the evidence schema")?;

    let mut gates = BTreeMap::<String, bool>::new();
    gates.insert(
        "base_decode_amplification".to_owned(),
        ratio_at_most(
            base_native_decoded_bytes,
            independent.canonical_base_pixel_bytes,
            110,
            100,
        ),
    );
    gates.insert(
        "canonical_base_pixel_bytes".to_owned(),
        independent.canonical_base_pixel_bytes == config.expected.canonical_source_pixel_bytes,
    );
    gates.insert(
        "source_scientific_traversal".to_owned(),
        scientific_identity_native_decoded_bytes == 0
            && no_data_detection_native_decoded_bytes == 0,
    );
    gates.insert(
        "reviewed_source_workload_binding".to_owned(),
        reviewed_source_fingerprint_sha256 == config.expected.reviewed_source_fingerprint_sha256
            && reviewed_source_bytes == source_inventory.source_bytes,
    );
    gates.insert(
        "timed_source_traffic".to_owned(),
        ratio_at_most(source_bytes_read, source_inventory.source_bytes, 250, 100),
    );
    gates.insert(
        "generation_only_source_revalidation".to_owned(),
        source_revalidation_bytes_read == 0,
    );
    gates.insert("durability_calls".to_owned(), sync_calls < 5_000);
    gates.insert(
        "durable_checkpoint_prefix".to_owned(),
        required_u64(statistics, "checkpoint_pending_work_units")? == 0,
    );
    gates.insert(
        "fresh_checkpoint_not_resumed".to_owned(),
        required_u64(statistics, "resumed_work_units")? == 0,
    );
    gates.insert("counter_reconciliation".to_owned(), counters_reconciled);
    gates.insert(
        "working_memory".to_owned(),
        peak_working_bytes <= WORKING_MEMORY_BYTES,
    );
    gates.insert(
        "external_rss_delta".to_owned(),
        external_rss_delta_bytes <= RSS_DELTA_BYTES_MAX,
    );
    gates.insert(
        "open_file_bound".to_owned(),
        peak_open_file_descriptors <= 64,
    );
    gates.insert(
        "incremental_headroom_reported".to_owned(),
        preflight_required_headroom_bytes > 0,
    );
    gates.insert(
        "bounded_final_layout_checkpoint_files".to_owned(),
        (1..=16).contains(&peak_checkpoint_regular_files),
    );
    gates.insert(
        "staged_scientific_locality".to_owned(),
        scientific_brick_reads == base_bricks,
    );
    gates.insert(
        "independent_scientific_locality".to_owned(),
        independent.scientific_brick_reads == base_bricks,
    );
    gates.insert(
        "scientific_correctness".to_owned(),
        independent.scientific_id_matches && independent.layer_roots_match,
    );
    gates.insert(
        "per_scale_semantic_regression".to_owned(),
        independent.scale_facts_match,
    );
    gates.insert(
        "frozen_centered_transforms".to_owned(),
        independent.config_transform_facts_match,
    );
    gates.insert("exact_correctness".to_owned(), true);
    gates.insert("progress_truthfulness".to_owned(), stages_truthful);
    gates.insert("normal_product_navigation".to_owned(), true);
    gates.insert("real_mapped_display".to_owned(), true);
    gates.insert(
        "storage_binding_stable".to_owned(),
        qualification_tuple_matched(&sample_root_binding_start)
            && qualification_tuple_matched(&output_parent_binding_start)
            && qualification_tuple_matched(&destination_binding)
            && qualification_tuple_matched(&sample_root_binding_end)
            && sample_root_binding_start == output_parent_binding_start
            && output_parent_binding_start == destination_binding
            && sample_root_binding_start == sample_root_binding_end,
    );
    let all_gates_passed = gates.values().all(|passed| *passed);
    let failed_gates = gates
        .iter()
        .filter_map(|(gate, passed)| (!passed).then_some(gate.clone()))
        .collect::<Vec<_>>();

    let raw = json!({
        "sample_index": sample_index,
        "paths": {
            "sample_root": sample_root,
            "output_parent": output_parent,
            "destination": destination,
            "checkpoint": checkpoint,
            "automation_script": script_path,
            "automation_report": automation_report_path,
            "stdout": stdout_path,
            "stderr": stderr_path,
            "app_log": app_log_path,
            "config_home": config_home,
            "cache_home": cache_home,
            "data_home": data_home,
            "state_home": state_home,
            "temp_home": temp_home,
        },
        "freshness": {
            "destination_absent_before_start": true,
            "checkpoint_absent_before_start": true,
            "fresh_process_session": true,
        },
        "storage_binding": {
            "sample_root_before": import_qualification_assessment_evidence(&sample_root_binding_start),
            "output_parent_before": import_qualification_assessment_evidence(&output_parent_binding_start),
            "published_destination_after": import_qualification_assessment_evidence(&destination_binding),
            "sample_root_after": import_qualification_assessment_evidence(&sample_root_binding_end),
            "unchanged": sample_root_binding_start == output_parent_binding_start
                && output_parent_binding_start == destination_binding
                && sample_root_binding_start == sample_root_binding_end,
        },
        "mapped_window": mapped_window,
        "rss": {
            "sampling_interval_ms": RSS_SAMPLE_INTERVAL.as_millis(),
            "sample_count": observation.rss_samples.len(),
            "idle_baseline_bytes": idle_rss_bytes,
            "primary_peak_bytes": primary_peak_rss_bytes,
            "primary_delta_bytes": external_rss_delta_bytes,
            "independent_ledger_peak_bytes": peak_working_bytes,
            "external_delta_minus_ledger_peak_bytes": rss_minus_ledger,
            "reconciliation_basis": "signed external primary RSS delta minus independently ledgered peak working bytes",
        },
        "timing": {
            "inspection_and_review_wall_time_ns": inspection_and_review_wall_time_ns,
            "inspection_and_review_process_cpu_time_ns": inspection_and_review_cpu_time_ns,
            "primary_wall_time_ns": primary_wall_time_ns,
            "primary_process_cpu_time_ns": primary_cpu_time_ns,
            "publication_to_open_ready_wall_time_ns": publication_to_open_ready_wall_time_ns,
            "publication_to_open_ready_process_cpu_time_ns": publication_to_open_ready_cpu_time_ns,
        },
        "receipt": receipt,
        "reviewed_source_binding": {
            "fingerprint_sha256": reviewed_source_fingerprint_sha256,
            "reviewed_source_bytes": reviewed_source_bytes,
            "matches_frozen_fingerprint": reviewed_source_fingerprint_sha256
                == config.expected.reviewed_source_fingerprint_sha256,
            "matches_external_inventory_bytes": reviewed_source_bytes
                == source_inventory.source_bytes,
        },
        "independent_validation": {
            "package_id": independent.package_id,
            "scientific_content_id": independent.scientific_content_id,
            "layer_roots": independent.layer_roots,
            "scales": independent.scale_facts,
            "transforms": independent.transform_facts,
            "layer_roots_match": independent.layer_roots_match,
            "scale_facts_match": independent.scale_facts_match,
            "scientific_id_matches": independent.scientific_id_matches,
            "config_transform_facts_match": independent.config_transform_facts_match,
            "scientific_brick_reads": independent.scientific_brick_reads,
            "canonical_base_pixel_bytes": independent.canonical_base_pixel_bytes,
            "canonical_base_pixel_bytes_match": independent.canonical_base_pixel_bytes
                == config.expected.canonical_source_pixel_bytes,
        },
        "automation_report": automation_report,
        "gates": gates,
        "all_gates_passed": all_gates_passed,
    });
    Ok(SampleEvidence {
        sample_index,
        primary_wall_time_ns,
        package_id: receipt_package_id,
        scientific_content_id: receipt_scientific_id,
        failed_gates,
        all_gates_passed,
        raw,
    })
}

fn automation_script(
    config: &T5Config,
    startup_package: &Path,
    source: &Path,
    output_parent: &Path,
    destination: &Path,
) -> anyhow::Result<Value> {
    let primary_timeout_ms = config
        .primary_timeout_seconds
        .checked_mul(1_000)
        .context("T5 automation timeout overflowed")?;
    let inspection_timeout_ms = INSPECTION_TIMEOUT_SECONDS
        .checked_mul(1_000)
        .context("T5 inspection timeout overflowed")?;
    let hard_safety_limits = canonical_product_automation_hard_safety_limits(&json!({
        "max_cpu_total_bytes": 1024_u64 * 1024 * 1024,
        "max_cpu_import_working_set_bytes": WORKING_MEMORY_BYTES,
        "max_runtime_queued_requests": 192,
    }))?;
    Ok(json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": "import_performance_t5_private",
        "hard_safety_limits": hard_safety_limits,
        "commands": [
            { "command": "open_dataset", "path": startup_package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 10_000 },
            { "command": "set_mapped_client_pixels", "width": MAPPED_WIDTH, "height": MAPPED_HEIGHT },
            { "command": "set_render_target_size", "width": MAPPED_WIDTH, "height": MAPPED_HEIGHT },
            { "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 60_000 },
            { "command": "begin_tiff_import_setup", "source": source, "output_parent": output_parent, "source_kind": "folder_of_3d_tiffs" },
            { "command": "wait_for", "condition": "import_review_ready", "timeout_ms": inspection_timeout_ms },
            { "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 60_000 },
            {
                "command": "start_reviewed_import",
                "spacing_zyx_um": config.spacing_zyx_um,
                "time_step_seconds": config.time_step_seconds,
                "no_data_value_rule": {
                    "kind": "manual_uint8",
                    "value": config.no_data_sentinel
                },
                "hide_constant_z_planes": false
            },
            { "command": "wait_for_imported_open_ready", "path": destination, "timeout_ms": primary_timeout_ms },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 120_000 },
            { "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 120_000 },
            { "command": "camera_fit_data" },
            { "command": "camera_orbit", "yaw_points": 48.0, "pitch_points": 16.0 },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 120_000 },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "target": "three_d", "name": "t5-open-ready-navigation" },
            { "command": "copy_diagnostics" },
            { "command": "quit" }
        ]
    }))
}

struct AppRun<'a> {
    executable: &'a Path,
    startup_package: &'a Path,
    script: &'a Path,
    automation_report: &'a Path,
    stdout: &'a Path,
    stderr: &'a Path,
    app_log: &'a Path,
    config_home: &'a Path,
    cache_home: &'a Path,
    data_home: &'a Path,
    state_home: &'a Path,
    temp_home: &'a Path,
    timeout: Duration,
    progress_plan: ProductAutomationProgressPlan,
    progress_launch: ProductAutomationProgressLaunch,
}

struct ProcessObservation {
    exit_success: bool,
    rss_samples: Vec<RssSample>,
    mapped_window: Option<Value>,
}

#[derive(Debug, PartialEq, Eq)]
enum ProcessRssPoll<T> {
    Exited(T),
    Sample(u64),
    ExitPending,
}

#[derive(Debug, PartialEq, Eq)]
enum TerminalRssExitConfirmation<T> {
    Exited(T),
    RssGraceExpired,
    ProcessDeadlineReached,
}

fn run_app_process(run: AppRun<'_>, cancelled: &AtomicBool) -> anyhow::Result<ProcessObservation> {
    require_not_cancelled(cancelled, "T5 application launch")?;
    let stdout = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(run.stdout)?;
    let stderr = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(run.stderr)?;
    let mut command = Command::new(run.executable);
    command
        .env("MIRANTE4D_DEV_DATASET", run.startup_package)
        .env("MIRANTE4D_ENABLE_AUTOMATION", "1")
        .env("MIRANTE4D_AUTOMATION_SCRIPT", run.script)
        .env("MIRANTE4D_AUTOMATION_REPORT", run.automation_report)
        .env("MIRANTE4D_LOG_FILE", run.app_log)
        .env("XDG_CONFIG_HOME", run.config_home)
        .env("XDG_CACHE_HOME", run.cache_home)
        .env("XDG_DATA_HOME", run.data_home)
        .env("XDG_STATE_HOME", run.state_home)
        .env("TMPDIR", run.temp_home)
        .env("MESA_SHADER_CACHE_DIR", run.cache_home.join("mesa"))
        .env("__GL_SHADER_DISK_CACHE_PATH", run.cache_home.join("nvidia"))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    isolate_process_tree(&mut command);
    run.progress_launch.apply_to_command(&mut command);
    let started = Instant::now();
    let mut child = command
        .spawn()
        .context("failed to spawn release Mirante4D app for T5")?;
    let pid = child.id();
    let deadline = started + run.timeout;
    let mut progress_monitor = run.progress_launch.monitor(run.progress_plan, started);
    let mut next_progress_poll = started;
    let mut rss_samples = Vec::new();
    let mut mapped_window = None;
    let mut next_geometry_probe = started;
    loop {
        if cancelled.load(Ordering::Acquire) {
            terminate_t5_child(&mut child);
            bail!("T5 command cancelled by SIGINT or SIGTERM while the app child was active");
        }
        if let Some(status) = try_wait_t5_child(&mut child)? {
            return finish_t5_process_observation(
                &mut child,
                status,
                rss_samples,
                mapped_window,
                &mut progress_monitor,
                Instant::now(),
            );
        }
        let now = Instant::now();
        if now >= next_progress_poll {
            match progress_monitor.poll_at(now) {
                ProgressMonitorAction::Continue => {}
                ProgressMonitorAction::Emit(snapshot) => {
                    eprintln!("{}", t5_safe_progress_line(&snapshot));
                }
                ProgressMonitorAction::Terminate(failure) => {
                    terminate_t5_child(&mut child);
                    return Err(t5_progress_failure(failure));
                }
            }
            next_progress_poll = now + FILE_POLL_INTERVAL;
        }
        let rss_result = read_process_rss_bytes(pid);
        let exit_status = try_wait_t5_child(&mut child)?;
        let rss_poll = match reconcile_process_rss_poll(rss_result, exit_status) {
            Ok(rss_poll) => rss_poll,
            Err(error) => {
                terminate_t5_child(&mut child);
                return Err(error);
            }
        };
        match rss_poll {
            ProcessRssPoll::Exited(status) => {
                return finish_t5_process_observation(
                    &mut child,
                    status,
                    rss_samples,
                    mapped_window,
                    &mut progress_monitor,
                    Instant::now(),
                );
            }
            ProcessRssPoll::Sample(rss_bytes) => rss_samples.push(RssSample {
                epoch_ms: epoch_ms(),
                rss_bytes,
            }),
            ProcessRssPoll::ExitPending => {
                let confirmation = confirm_process_exit_after_rss_disappeared(
                    || {
                        require_not_cancelled(cancelled, "T5 child exit confirmation")?;
                        child.try_wait().map_err(Into::into)
                    },
                    || thread::sleep(RSS_SAMPLE_INTERVAL),
                    Instant::now,
                    deadline,
                    RSS_TERMINAL_EXIT_CONFIRM_TIMEOUT,
                );
                let status = match confirmation {
                    Ok(TerminalRssExitConfirmation::Exited(status)) => status,
                    Ok(TerminalRssExitConfirmation::RssGraceExpired) => {
                        terminate_t5_child(&mut child);
                        bail!(
                            "Linux process RSS status disappeared without a bounded T5 child exit confirmation"
                        );
                    }
                    Ok(TerminalRssExitConfirmation::ProcessDeadlineReached) => {
                        terminate_t5_child(&mut child);
                        bail!("T5 app process exceeded its declared timeout");
                    }
                    Err(error) => {
                        terminate_t5_child(&mut child);
                        return Err(error);
                    }
                };
                return finish_t5_process_observation(
                    &mut child,
                    status,
                    rss_samples,
                    mapped_window,
                    &mut progress_monitor,
                    Instant::now(),
                );
            }
        }
        if mapped_window.is_none() && Instant::now() >= next_geometry_probe {
            mapped_window = match probe_x11_client_geometry(pid, MAPPED_WIDTH, MAPPED_HEIGHT) {
                Ok(mapped_window) => mapped_window,
                Err(error) => {
                    terminate_t5_child(&mut child);
                    return Err(error);
                }
            };
            next_geometry_probe = Instant::now() + FILE_POLL_INTERVAL;
        }
        if let Some(status) = try_wait_t5_child(&mut child)? {
            return finish_t5_process_observation(
                &mut child,
                status,
                rss_samples,
                mapped_window,
                &mut progress_monitor,
                Instant::now(),
            );
        }
        if Instant::now() >= deadline {
            terminate_t5_child(&mut child);
            bail!("T5 app process exceeded its declared timeout");
        }
        thread::sleep(RSS_SAMPLE_INTERVAL);
    }
}

fn finish_t5_process_observation(
    child: &mut Child,
    status: ExitStatus,
    rss_samples: Vec<RssSample>,
    mapped_window: Option<Value>,
    progress_monitor: &mut ProductAutomationProgressMonitor,
    now: Instant,
) -> anyhow::Result<ProcessObservation> {
    if let Some(failure) = progress_monitor.finalize_at_exit(now) {
        terminate_t5_child(child);
        return Err(t5_progress_failure(failure));
    }
    Ok(ProcessObservation {
        exit_success: status.success(),
        rss_samples,
        mapped_window,
    })
}

fn t5_safe_progress_line(snapshot: &SafeProgressSnapshot) -> String {
    safe_automation_progress_line("t5", "import_preprocessing", snapshot)
        .expect("the static T5 automation progress context is a safe token")
}

fn t5_progress_failure(failure: ProgressFailure) -> anyhow::Error {
    anyhow::Error::new(failure)
}

fn reconcile_process_rss_poll<T>(
    rss_result: anyhow::Result<Option<u64>>,
    exit_status: Option<T>,
) -> anyhow::Result<ProcessRssPoll<T>> {
    let rss_result = rss_result?;
    if let Some(status) = exit_status {
        return Ok(ProcessRssPoll::Exited(status));
    }
    Ok(match rss_result {
        Some(rss_bytes) => ProcessRssPoll::Sample(rss_bytes),
        None => ProcessRssPoll::ExitPending,
    })
}

fn try_wait_t5_child(child: &mut Child) -> anyhow::Result<Option<ExitStatus>> {
    match child.try_wait() {
        Ok(status) => Ok(status),
        Err(error) => {
            terminate_t5_child(child);
            Err(error.into())
        }
    }
}

fn terminate_t5_child(child: &mut Child) {
    terminate_process_tree(child);
    let _ = child.wait();
}

fn confirm_process_exit_after_rss_disappeared<T>(
    mut try_wait: impl FnMut() -> anyhow::Result<Option<T>>,
    mut pause: impl FnMut(),
    mut now: impl FnMut() -> Instant,
    process_deadline: Instant,
    confirmation_timeout: Duration,
) -> anyhow::Result<TerminalRssExitConfirmation<T>> {
    let confirmation_started = now();
    let confirmation_deadline = confirmation_started
        .checked_add(confirmation_timeout)
        .unwrap_or(process_deadline);
    let (terminal_deadline, timeout_outcome) = if process_deadline <= confirmation_deadline {
        (
            process_deadline,
            TerminalRssExitConfirmation::ProcessDeadlineReached,
        )
    } else {
        (
            confirmation_deadline,
            TerminalRssExitConfirmation::RssGraceExpired,
        )
    };
    if confirmation_started >= terminal_deadline {
        return Ok(timeout_outcome);
    }
    if let Some(status) = try_wait()? {
        return Ok(TerminalRssExitConfirmation::Exited(status));
    }
    loop {
        if now() >= terminal_deadline {
            return Ok(timeout_outcome);
        }
        pause();
        if now() >= terminal_deadline {
            return Ok(timeout_outcome);
        }
        if let Some(status) = try_wait()? {
            return Ok(TerminalRssExitConfirmation::Exited(status));
        }
    }
}

fn read_process_rss_bytes(pid: u32) -> anyhow::Result<Option<u64>> {
    let path = PathBuf::from(format!("/proc/{pid}/status"));
    let status = match fs::read_to_string(&path) {
        Ok(status) => status,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    parse_process_rss_bytes(&status)
}

fn parse_process_rss_bytes(status: &str) -> anyhow::Result<Option<u64>> {
    let Some(value) = status.lines().find_map(|line| line.strip_prefix("VmRSS:")) else {
        // Linux zombie status documents and a process that exits while its
        // procfs file is read legitimately omit VmRSS. The caller accepts
        // this only after bounded exit confirmation. Other surviving memory
        // fields instead indicate a malformed or truncated live status.
        if status.lines().any(|line| {
            line.split_once(':')
                .is_some_and(|(name, _)| name.starts_with("Vm") || name.starts_with("Rss"))
        }) {
            bail!("Linux process status omitted VmRSS while other memory fields remained");
        }
        return Ok(None);
    };
    let mut fields = value.split_ascii_whitespace();
    let kib = fields
        .next()
        .context("Linux VmRSS omitted its numeric value during T5 sampling")?
        .parse::<u64>()
        .context("Linux VmRSS was not an unsigned integer during T5 sampling")?;
    if fields.next() != Some("kB") || fields.next().is_some() {
        bail!("Linux VmRSS did not use the exact `<integer> kB` form during T5 sampling");
    }
    Ok(Some(
        kib.checked_mul(1024)
            .context("T5 RSS byte count overflowed")?,
    ))
}

fn probe_x11_client_geometry(
    pid: u32,
    expected_width: u32,
    expected_height: u32,
) -> anyhow::Result<Option<Value>> {
    let mut search = Command::new("xdotool");
    search.args(["search", "--onlyvisible", "--pid", &pid.to_string()]);
    let search = run_command_with_bounded_output(&mut search, T5_X11_AUTOMATION_OUTPUT_POLICY)
        .context("failed to run xdotool for T5 mapped-window proof")?;
    if !search.status.success() {
        return Ok(None);
    }
    let window_ids = String::from_utf8(search.stdout).context("xdotool returned non-UTF-8")?;
    for raw_id in window_ids
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Ok(window_id) = raw_id.parse::<u64>() else {
            continue;
        };
        let id_hex = format!("0x{window_id:x}");
        let mut info = Command::new("xwininfo");
        info.args(["-id", &id_hex]);
        let info = run_command_with_bounded_output(&mut info, T5_X11_AUTOMATION_OUTPUT_POLICY)
            .context("failed to run xwininfo for T5 mapped-window proof")?;
        if !info.status.success() {
            continue;
        }
        let encoded = String::from_utf8(info.stdout).context("xwininfo returned non-UTF-8")?;
        let Some((width, height, viewable)) = parse_xwininfo_geometry(&encoded) else {
            continue;
        };
        if width == expected_width && height == expected_height && viewable {
            return Ok(Some(json!({
                "width": width,
                "height": height,
                "window_id": id_hex,
                "map_state": "is_viewable",
                "observation": "xdotool_pid_search_plus_xwininfo_client_geometry",
                "observed_at_epoch_ms": epoch_ms(),
            })));
        }
    }
    Ok(None)
}

fn parse_xwininfo_geometry(output: &str) -> Option<(u32, u32, bool)> {
    let width = output
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("Width:"))?
        .trim()
        .parse()
        .ok()?;
    let height = output
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("Height:"))?
        .trim()
        .parse()
        .ok()?;
    let viewable = output
        .lines()
        .map(str::trim)
        .any(|line| line == "Map State: IsViewable");
    Some((width, height, viewable))
}

fn classify_rss_samples(
    samples: &[RssSample],
    primary_start_epoch_ms: u128,
    primary_end_epoch_ms: u128,
) -> anyhow::Result<(u64, u64, u64)> {
    if primary_end_epoch_ms <= primary_start_epoch_ms {
        bail!("T5 primary-clock epoch bounds are not ordered");
    }
    let idle = samples
        .iter()
        .rev()
        .find(|sample| sample.epoch_ms < primary_start_epoch_ms)
        .map(|sample| sample.rss_bytes)
        .context("T5 external RSS sampler captured no pre-import idle baseline")?;
    let peak = samples
        .iter()
        .filter(|sample| {
            sample.epoch_ms >= primary_start_epoch_ms && sample.epoch_ms <= primary_end_epoch_ms
        })
        .map(|sample| sample.rss_bytes)
        .max()
        .context("T5 external RSS sampler captured no primary-clock sample")?;
    Ok((idle, peak, peak.saturating_sub(idle)))
}

fn validate_app_build_provenance(
    report: &Value,
    repository: &RepositoryIdentity,
    compiler: &str,
) -> anyhow::Result<()> {
    let provenance = report
        .get("build_provenance")
        .context("T5 automation report lacks embedded app build provenance")?;
    if provenance
        .get("repository_revision")
        .and_then(Value::as_str)
        != repository.commit.as_deref()
        || provenance.get("profile").and_then(Value::as_str) != Some("release")
        || provenance.get("compiler").and_then(Value::as_str) != Some(compiler)
        || provenance.get("target_mode").and_then(Value::as_str) != Some("fresh-private-target")
    {
        bail!("T5 app embedded build provenance differs from the qualification build");
    }
    Ok(())
}

fn validate_navigation_evidence(report: &Value) -> anyhow::Result<()> {
    let events = report
        .get("events")
        .and_then(Value::as_array)
        .context("T5 automation report lacks events")?;
    for command in [
        "set_mapped_client_pixels",
        "set_render_target_size",
        "wait_for_imported_open_ready",
        "camera_fit_data",
        "camera_orbit",
        "capture_screenshot",
        "quit",
    ] {
        if !events.iter().any(|event| {
            event.get("command").and_then(Value::as_str) == Some(command)
                && event.get("status").and_then(Value::as_str) == Some("passed")
        }) {
            bail!("T5 automation lacks passed product command {command}");
        }
    }
    let requested = report
        .pointer("/viewport_evidence/requested_mapped_client_pixels")
        .context("T5 automation lacks requested mapped client pixels")?;
    if required_u64(requested, "width")? != u64::from(MAPPED_WIDTH)
        || required_u64(requested, "height")? != u64::from(MAPPED_HEIGHT)
    {
        bail!("T5 automation requested the wrong mapped client geometry");
    }
    Ok(())
}

fn validate_successful_receipt_binding(
    report: &Value,
    open_ready_clock: &Value,
    receipt: &Value,
    destination: &Path,
) -> anyhow::Result<(String, u64)> {
    let destination_text = destination.to_string_lossy();
    if receipt.get("destination").and_then(Value::as_str) != Some(destination_text.as_ref()) {
        bail!("T5 successful receipt is not bound to the published destination");
    }
    let receipt_review_id = required_u64(receipt, "review_id")?;
    let reviewed_source_fingerprint = receipt
        .get("reviewed_source_fingerprint_sha256")
        .and_then(Value::as_str)
        .context("T5 successful receipt lacks its reviewed-source fingerprint")?;
    Sha256Digest::parse(reviewed_source_fingerprint)
        .context("T5 successful receipt contains an invalid reviewed-source fingerprint")?;
    let reviewed_source_bytes = required_u64(receipt, "reviewed_source_bytes")?;
    if reviewed_source_bytes == 0 {
        bail!("T5 successful receipt contains an empty reviewed source");
    }
    let published_event = receipt
        .get("published_event")
        .context("T5 successful receipt lacks its Published-event timing")?;
    let published_epoch_ms = required_u64(published_event, "published_at_epoch_ms")?;
    required_u64(published_event, "process_cpu_time_ns")?;
    if required_u64(open_ready_clock, "published_at_epoch_ms")? != published_epoch_ms {
        bail!("T5 receipt Published event differs from its open-ready-clock origin");
    }
    let receipt_token = receipt
        .get("operation_token")
        .context("T5 successful receipt lacks its operation-token binding")?;
    if receipt_token.get("kind").and_then(Value::as_str) != Some("Import") {
        bail!("T5 successful receipt is not bound to an import operation token");
    }
    let events = report
        .get("events")
        .and_then(Value::as_array)
        .context("T5 automation report lacks events for receipt binding")?;
    let start_details = events
        .iter()
        .find(|event| {
            event.get("command").and_then(Value::as_str) == Some("start_reviewed_import")
                && event.get("status").and_then(Value::as_str) == Some("passed")
        })
        .and_then(|event| event.get("details"))
        .context("T5 successful receipt lacks its passed Start Import event")?;
    let start_token = start_details
        .get("operation_token")
        .context("T5 Start Import event lacks its operation-token binding")?;
    if required_u64(start_details, "review_id")? != receipt_review_id
        || start_details.get("destination").and_then(Value::as_str)
            != Some(destination_text.as_ref())
        || start_details
            .get("reviewed_source_fingerprint_sha256")
            .and_then(Value::as_str)
            != Some(reviewed_source_fingerprint)
        || required_u64(start_details, "reviewed_source_bytes")? != reviewed_source_bytes
    {
        bail!("T5 Start Import review, source, or destination differs from the successful receipt");
    }
    for field in [
        "operation_id",
        "task_id",
        "source_session_generation",
        "currentness_generation",
    ] {
        if required_u64(start_token, field)? != required_u64(receipt_token, field)? {
            bail!("T5 Start Import token differs from the successful receipt token");
        }
    }
    if start_token.get("kind").and_then(Value::as_str)
        != receipt_token.get("kind").and_then(Value::as_str)
    {
        bail!("T5 Start Import token kind differs from the successful receipt token");
    }
    let open_ready_path = events
        .iter()
        .find(|event| {
            event.get("command").and_then(Value::as_str) == Some("wait_for_imported_open_ready")
                && event.get("status").and_then(Value::as_str) == Some("passed")
        })
        .and_then(|event| event.pointer("/details/path"))
        .and_then(Value::as_str)
        .context("T5 receipt lacks its passed product open-ready destination")?;
    if open_ready_path != destination_text {
        bail!("T5 product open-ready destination differs from the successful receipt");
    }
    Ok((
        reviewed_source_fingerprint.to_owned(),
        reviewed_source_bytes,
    ))
}

fn validate_required_statistics(statistics: &Value) -> anyhow::Result<()> {
    for field in [
        "source_bytes_read",
        "source_revalidation_bytes_read",
        "native_decoded_bytes",
        "base_native_decoded_bytes",
        "no_data_detection_native_decoded_bytes",
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
        "preflight_required_headroom_bytes",
        "peak_temporary_bytes",
        "peak_checkpoint_regular_files",
        "peak_working_bytes",
        "peak_process_rss_bytes",
        "resumed_work_units",
        "produced_work_units",
        "primary_wall_time_ns",
        "primary_cpu_time_ns",
    ] {
        required_u64(statistics, field)?;
    }
    statistics
        .get("stages")
        .and_then(Value::as_array)
        .filter(|stages| !stages.is_empty())
        .context("T5 receipt lacks nonempty stage timing evidence")?;
    Ok(())
}

fn required_stage_evidence_present(
    statistics: &Value,
    scales: &[ExpectedScaleFact],
) -> anyhow::Result<bool> {
    let stages = statistics
        .get("stages")
        .and_then(Value::as_array)
        .context("T5 statistics stages are missing")?;
    let names = stages
        .iter()
        .map(|stage| {
            let name = stage
                .get("stage")
                .and_then(Value::as_str)
                .context("T5 stage lacks its unique name")?;
            required_u64(stage, "wall_time_ns")?;
            required_u64(stage, "cpu_time_ns")?;
            Ok(name.to_owned())
        })
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    if names.len() != stages.len() {
        bail!("T5 statistics contain duplicate stage timing names");
    }
    let mut required = BTreeSet::from([
        "planning-and-preflight".to_owned(),
        "source-revalidation-1".to_owned(),
        "checkpoint-open-or-resume".to_owned(),
        "source-ingest".to_owned(),
        "base-production".to_owned(),
        "source-revalidation-2".to_owned(),
        "shard-publication".to_owned(),
        "staged-structure-validation".to_owned(),
        "staged-exact-validation".to_owned(),
        "staged-scientific-validation".to_owned(),
        "commit".to_owned(),
    ]);
    for scale in scales
        .iter()
        .map(|fact| fact.scale_ordinal)
        .filter(|scale| *scale > 0)
    {
        required.insert(format!("pyramid-production-{scale}"));
    }
    Ok(required.is_subset(&names))
}

fn progress_truthfulness_present(
    report: &Value,
    statistics: &Value,
    scales: &[ExpectedScaleFact],
) -> anyhow::Result<bool> {
    if !required_stage_evidence_present(statistics, scales)? {
        return Ok(false);
    }
    let evidence = report
        .get("import_workflow_evidence")
        .context("T5 automation report lacks import workflow evidence")?;
    let emitted = string_set(evidence, "worker_emitted_stage_names")?;
    let projected = string_set(evidence, "projected_named_stage_observations")?;
    let mut required_emitted = BTreeSet::from([
        "planning-and-preflight".to_owned(),
        "source-revalidation".to_owned(),
        "checkpoint-open-or-resume".to_owned(),
        "source-ingest".to_owned(),
        "base-production".to_owned(),
        "shard-publication".to_owned(),
        "staged-structure-validation".to_owned(),
        "staged-exact-validation".to_owned(),
        "staged-scientific-validation".to_owned(),
        "commit".to_owned(),
    ]);
    let has_pyramid = scales.iter().any(|scale| scale.scale_ordinal > 0);
    if has_pyramid {
        required_emitted.insert("pyramid-production".to_owned());
    }
    let mut required_projected =
        BTreeSet::from(["source-ingest".to_owned(), "base-production".to_owned()]);
    if has_pyramid {
        required_projected.insert("pyramid-production".to_owned());
    }
    let progressing = evidence
        .get("maximum_completed_by_stage")
        .and_then(Value::as_array)
        .context("T5 automation report lacks named progress maxima")?
        .iter()
        .any(|entry| {
            entry
                .get("completed_work_units")
                .and_then(Value::as_u64)
                .is_some_and(|completed| completed > 0)
        });
    Ok(required_emitted.is_subset(&emitted)
        && required_projected.is_subset(&projected)
        && progressing
        && required_u64(evidence, "progress_updates")? > 0
        && required_u64(evidence, "published_events")? == 1
        && required_u64(evidence, "successful_runs")? == 1
        && required_u64(evidence, "cancelled_runs")? == 0
        && required_u64(evidence, "failed_runs")? == 0
        && evidence
            .get("fabricated_global_percentage_or_eta_observed")
            .and_then(Value::as_bool)
            == Some(false))
}

fn import_counters_reconciled(report: &Value, statistics: &Value) -> anyhow::Result<bool> {
    let native_decoded = required_u64(statistics, "native_decoded_bytes")?;
    let detection_decoded = required_u64(statistics, "no_data_detection_native_decoded_bytes")?;
    let scientific_decoded = required_u64(statistics, "scientific_identity_native_decoded_bytes")?;
    let decoded_by_stage = required_u64(statistics, "base_native_decoded_bytes")?
        .checked_add(detection_decoded)
        .and_then(|value| value.checked_add(scientific_decoded))
        .context("T5 native decoded-byte counter overflowed")?;
    let object_reads = required_u64(statistics, "object_reads")?;
    let object_reads_by_stage = required_u64(statistics, "staged_structure_object_reads")?
        .checked_add(required_u64(statistics, "staged_exact_object_reads")?)
        .and_then(|value| {
            value.checked_add(
                statistics
                    .get("scientific_object_reads")
                    .and_then(Value::as_u64)?,
            )
        })
        .context("T5 package object-read counters overflowed")?;
    let stages = statistics
        .get("stages")
        .and_then(Value::as_array)
        .context("T5 receipt lacks stage timings for reconciliation")?;
    let (stage_wall_time_ns, stage_cpu_time_ns) = stages
        .iter()
        .try_fold((0_u64, 0_u64), |totals, stage| {
            Some((
                totals.0.checked_add(stage.get("wall_time_ns")?.as_u64()?)?,
                totals.1.checked_add(stage.get("cpu_time_ns")?.as_u64()?)?,
            ))
        })
        .context("T5 stage timing counters are missing or overflowed")?;
    let workflow = report
        .get("import_workflow_evidence")
        .context("T5 automation report lacks import evidence for reconciliation")?;
    let app_primary = workflow
        .get("primary_clock")
        .context("T5 import evidence lacks its app primary clock")?;
    Ok(native_decoded == decoded_by_stage
        && required_u64(statistics, "source_revalidation_bytes_read")?
            <= required_u64(statistics, "source_bytes_read")?
        && object_reads == object_reads_by_stage
        && required_u64(statistics, "scientific_payload_object_reads")?
            <= required_u64(statistics, "scientific_object_reads")?
        && required_u64(statistics, "peak_open_file_descriptors")?
            == required_u64(statistics, "sampled_peak_open_file_descriptors")?.max(required_u64(
                statistics,
                "open_file_descriptor_structural_bound",
            )?)
        && stage_wall_time_ns <= required_u64(statistics, "primary_wall_time_ns")?
        && stage_cpu_time_ns <= required_u64(statistics, "primary_cpu_time_ns")?
        && required_u64(statistics, "primary_wall_time_ns")?
            <= required_u64(app_primary, "wall_time_ns")?
        && required_u64(statistics, "primary_cpu_time_ns")?
            <= required_u64(app_primary, "process_cpu_time_ns")?
        && required_u64(workflow, "maximum_peak_working_bytes")?
            == required_u64(statistics, "peak_working_bytes")?)
}

fn string_set(object: &Value, field: &str) -> anyhow::Result<BTreeSet<String>> {
    object
        .get(field)
        .and_then(Value::as_array)
        .with_context(|| format!("T5 evidence lacks string array {field}"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .with_context(|| format!("T5 evidence {field} contains a non-string"))
        })
        .collect()
}

fn validate_publication_currentness_execution(transfer: &Value) -> anyhow::Result<()> {
    let execution = transfer
        .get("publication_currentness_execution")
        .context("T5 publication transfer lacks storage execution evidence")?;
    if execution.get("contract_id").and_then(Value::as_str)
        != Some(PUBLICATION_CURRENTNESS_CONTRACT_ID)
    {
        bail!("T5 publication transfer used an unknown currentness execution contract");
    }
    let expected = required_u64(execution, "expected_snapshot_object_reads")?;
    let first_inventory = required_u64(execution, "first_inventory_object_reads")?;
    let observed_snapshot = required_u64(execution, "observed_snapshot_object_reads")?;
    let second_inventory = required_u64(execution, "second_inventory_object_reads")?;
    let observed_total = required_u64(execution, "observed_total_object_reads")?;
    let codec_decodes = required_u64(execution, "observed_codec_decode_calls")?;
    let reconciled_total = first_inventory
        .checked_add(observed_snapshot)
        .and_then(|value| value.checked_add(second_inventory));
    if expected == 0
        || first_inventory == 0
        || first_inventory != second_inventory
        || observed_snapshot != expected
        || reconciled_total != Some(observed_total)
        || codec_decodes != 0
    {
        bail!(
            "T5 publication transfer execution disagreed with the inventory/snapshot/inventory contract"
        );
    }
    Ok(())
}

fn required_u64(object: &Value, field: &str) -> anyhow::Result<u64> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .with_context(|| format!("T5 evidence lacks unsigned counter {field}"))
}

fn ratio_at_most(numerator: u64, denominator: u64, max_numerator: u128, scale: u128) -> bool {
    denominator > 0
        && u128::from(numerator).saturating_mul(scale)
            <= u128::from(denominator).saturating_mul(max_numerator)
}

fn source_oracle_gates(
    oracle: &OracleFacts,
    expected: &ExpectedFacts,
    source_before: &InventoryFacts,
    source_after: &InventoryFacts,
    scratch_removed_empty: bool,
    working_memory_bytes: u64,
) -> BTreeMap<String, bool> {
    let mut gates = BTreeMap::new();
    gates.insert(
        "fact_authority".to_owned(),
        expected.expected_fact_authority == FACT_AUTHORITY,
    );
    gates.insert(
        "frozen_source_inventory".to_owned(),
        source_before.sha256 == expected.source_inventory_sha256,
    );
    gates.insert(
        "frozen_scientific_facts".to_owned(),
        oracle_matches_expected(oracle, expected),
    );
    gates.insert(
        "frozen_scale_keys_and_counts".to_owned(),
        oracle_scale_keys_and_counts_match(oracle, &expected.scales),
    );
    gates.insert(
        "frozen_centered_transforms".to_owned(),
        expected_oracle_transform_facts_match(&expected.transforms, &oracle.transforms),
    );
    gates.insert(
        "canonical_base_pixel_bytes".to_owned(),
        oracle.canonical_base_pixel_bytes == expected.canonical_source_pixel_bytes,
    );
    gates.insert(
        "working_memory_bound".to_owned(),
        oracle.resources.calculated_peak_working_bytes <= working_memory_bytes,
    );
    gates.insert(
        "scratch_capacity".to_owned(),
        oracle.resources.required_scratch_bytes <= oracle.resources.scratch_free_bytes,
    );
    gates.insert(
        "scratch_file_bound".to_owned(),
        oracle.resources.peak_scratch_regular_files <= 2,
    );
    gates.insert(
        "open_file_bound".to_owned(),
        oracle.resources.calculated_open_files_bound <= 64,
    );
    gates.insert(
        "zero_scratch_remnants".to_owned(),
        oracle.resources.scratch_files_remaining == 0 && scratch_removed_empty,
    );
    gates.insert(
        "deterministic_filename_order".to_owned(),
        oracle.resources.deterministic_filename_order,
    );
    gates.insert(
        "nonempty_source_planes".to_owned(),
        oracle.resources.source_planes > 0,
    );
    gates.insert(
        "source_file_count".to_owned(),
        oracle.resources.source_files == source_before.regular_files,
    );
    gates.insert(
        "source_inventory_names".to_owned(),
        oracle.resources.source_inventory_name_bytes > 0,
    );
    gates.insert(
        "transform_coverage".to_owned(),
        oracle_transform_coverage(oracle),
    );
    gates.insert(
        "source_preservation".to_owned(),
        oracle.resources.source_unchanged && source_before == source_after,
    );
    gates
}

fn oracle_transform_coverage(oracle: &OracleFacts) -> bool {
    let scale_ordinals = oracle
        .scales
        .iter()
        .map(|scale| (scale.image_ordinal, scale.scale_ordinal))
        .collect::<BTreeSet<_>>();
    let transform_ordinals = oracle
        .transforms
        .iter()
        .map(|transform| transform.scale_ordinal)
        .collect::<BTreeSet<_>>();
    scale_ordinals.len() == oracle.scales.len()
        && scale_ordinals.iter().all(|(image, _)| *image == 0)
        && transform_ordinals.len() == oracle.transforms.len()
        && transform_ordinals
            == scale_ordinals
                .iter()
                .map(|(_, scale)| *scale)
                .collect::<BTreeSet<_>>()
}

fn oracle_matches_expected(oracle: &OracleFacts, expected: &ExpectedFacts) -> bool {
    if oracle.scientific_content_id != expected.scientific_content_id
        || oracle.canonical_base_pixel_bytes != expected.canonical_source_pixel_bytes
        || !expected_oracle_transform_facts_match(&expected.transforms, &oracle.transforms)
    {
        return false;
    }
    let mut oracle_roots = oracle
        .layer_roots
        .iter()
        .map(|root| (root.logical_layer, root.digest_sha256.as_str()))
        .collect::<Vec<_>>();
    oracle_roots.sort_unstable();
    let mut expected_roots = expected
        .scientific_layer_roots
        .iter()
        .map(|root| (root.logical_layer, root.digest_sha256.as_str()))
        .collect::<Vec<_>>();
    expected_roots.sort_unstable();

    let mut oracle_scales = oracle
        .scales
        .iter()
        .map(|scale| {
            (
                scale.image_ordinal,
                scale.scale_ordinal,
                scale.digest_sha256.as_str(),
                scale.brick_reads,
                scale.logical_voxels,
            )
        })
        .collect::<Vec<_>>();
    oracle_scales.sort_unstable();
    let mut expected_scales = expected
        .scales
        .iter()
        .map(|scale| {
            (
                scale.image_ordinal,
                scale.scale_ordinal,
                scale.digest_sha256.as_str(),
                scale.brick_reads,
                scale.logical_voxels,
            )
        })
        .collect::<Vec<_>>();
    expected_scales.sort_unstable();
    oracle_roots == expected_roots && oracle_scales == expected_scales
}

fn expected_oracle_transform_facts_match(
    expected: &[ExpectedTransformFact],
    oracle: &[t5_sentinel_oracle::OracleTransformFact],
) -> bool {
    let mut expected = expected
        .iter()
        .map(|fact| {
            (
                fact.scale_ordinal,
                fact.scale_zyx.map(f64::to_bits),
                fact.translation_zyx.map(f64::to_bits),
            )
        })
        .collect::<Vec<_>>();
    expected.sort_unstable_by_key(|fact| fact.0);
    let mut oracle = oracle
        .iter()
        .map(|fact| {
            (
                fact.scale_ordinal,
                fact.scale_zyx.map(f64::to_bits),
                fact.translation_zyx.map(f64::to_bits),
            )
        })
        .collect::<Vec<_>>();
    oracle.sort_unstable_by_key(|fact| fact.0);
    expected == oracle
}

fn oracle_scale_keys_and_counts_match(
    oracle: &OracleFacts,
    expected: &[ExpectedScaleFact],
) -> bool {
    let mut oracle_counts = oracle
        .scales
        .iter()
        .map(|scale| {
            (
                scale.image_ordinal,
                scale.scale_ordinal,
                scale.brick_reads,
                scale.logical_voxels,
            )
        })
        .collect::<Vec<_>>();
    oracle_counts.sort_unstable();
    let mut expected_counts = expected
        .iter()
        .map(|scale| {
            (
                scale.image_ordinal,
                scale.scale_ordinal,
                scale.brick_reads,
                scale.logical_voxels,
            )
        })
        .collect::<Vec<_>>();
    expected_counts.sort_unstable();
    oracle_counts == expected_counts
}

fn independently_validate_package(
    destination: &Path,
    expected: &ExpectedFacts,
    cancelled: &AtomicBool,
) -> anyhow::Result<IndependentValidation> {
    require_not_cancelled(cancelled, "independent exact-package validation")?;
    let catalog = LocalPackageCatalog::open(destination)
        .context("T5 independent post-open package catalog rejected the destination")?;
    let exact = catalog
        .validate_exact_package(ProfileKind::Current, || cancelled.load(Ordering::Acquire))
        .context("T5 independent exact package validation failed")?;
    let package_id = exact.package_id().to_string();
    let self_consistent = exact
        .validate_scientific_content(|| cancelled.load(Ordering::Acquire))
        .context("T5 independent scientific package validation failed")?;
    let scientific_content_id = self_consistent.scientific_content_id().to_string();
    let scientific_id_matches = scientific_content_id == expected.scientific_content_id;

    let mut actual_roots = self_consistent
        .layer_roots()
        .iter()
        .map(|root| ExpectedLayerRoot {
            logical_layer: root.layer().ordinal(),
            digest_sha256: root.digest().to_string(),
        })
        .collect::<Vec<_>>();
    actual_roots.sort();
    let mut expected_roots = expected.scientific_layer_roots.clone();
    expected_roots.sort();
    let layer_roots_match = actual_roots == expected_roots;
    let layer_roots = actual_roots
        .iter()
        .map(|root| {
            json!({
                "logical_layer": root.logical_layer,
                "digest_sha256": root.digest_sha256,
            })
        })
        .collect();

    let mut actual_scales = Vec::new();
    let mut canonical_base_pixel_bytes = 0_u64;
    for image in self_consistent.catalog().profile().images() {
        for level in image.levels() {
            require_not_cancelled(cancelled, "independent per-scale validation")?;
            let (fact, canonical_pixel_bytes) =
                scale_fact(&self_consistent, image.image_ordinal(), level, cancelled)?;
            if level.scale_ordinal() == 0 {
                canonical_base_pixel_bytes = canonical_base_pixel_bytes
                    .checked_add(canonical_pixel_bytes)
                    .context("T5 canonical base pixel byte count overflowed")?;
            }
            actual_scales.push(fact);
        }
    }
    if canonical_base_pixel_bytes == 0 {
        bail!("T5 independent package has no nonempty base scale");
    }
    actual_scales.sort_by_key(|fact| (fact.image_ordinal, fact.scale_ordinal));
    let mut expected_scales = expected.scales.clone();
    expected_scales.sort();
    let scale_facts_match = actual_scales == expected_scales;
    let scale_facts = actual_scales
        .iter()
        .map(|fact| {
            json!({
                "image_ordinal": fact.image_ordinal,
                "scale_ordinal": fact.scale_ordinal,
                "digest_sha256": fact.digest_sha256,
                "brick_reads": fact.brick_reads,
                "logical_voxels": fact.logical_voxels,
            })
        })
        .collect();
    let actual_transforms = package_transform_facts(&self_consistent)?;
    let config_transform_facts_match =
        expected_transform_facts_match(&actual_transforms, &expected.transforms);
    let transform_facts = actual_transforms
        .iter()
        .map(|fact| serde_json::to_value(fact).expect("transform fact is JSON-safe"))
        .collect();
    let scientific_brick_reads = self_consistent.validation_report().brick_reads();
    self_consistent
        .revalidate_complete(|| cancelled.load(Ordering::Acquire))
        .context("T5 package changed during independent per-scale readback")?;
    require_not_cancelled(cancelled, "independent package validation")?;

    Ok(IndependentValidation {
        package_id,
        scientific_content_id,
        layer_roots,
        scale_facts,
        transform_facts,
        scale_facts_match,
        layer_roots_match,
        scientific_id_matches,
        config_transform_facts_match,
        scientific_brick_reads,
        canonical_base_pixel_bytes,
    })
}

fn package_transform_facts(
    self_consistent: &mirante4d_storage::SelfConsistentPackageCapability,
) -> anyhow::Result<Vec<ObservedTransformFact>> {
    let images = self_consistent.catalog().profile().images();
    if images.len() != 1 || images[0].image_ordinal() != 0 {
        bail!("T5 source-oracle transform authority requires exactly physical image zero");
    }
    let image = &images[0];
    let metadata_path =
        mirante4d_storage::PackagePath::parse(&format!("{}/zarr.json", image.image_group_path()))?;
    let ome = self_consistent
        .catalog()
        .ome_image(&metadata_path)
        .context("T5 package lacks its OME image-group transform metadata")?;
    if ome.level_transforms().len() != image.levels().len() {
        bail!("T5 package OME transform count differs from its scale count");
    }
    ome.level_transforms()
        .iter()
        .enumerate()
        .map(|(ordinal, transform)| match transform {
            OmeLevelTransform::DiagonalMicrometer {
                scale_zyx,
                translation_zyx,
            } => Ok(ObservedTransformFact {
                scale_ordinal: u32::try_from(ordinal)?,
                scale_zyx: scale_zyx.map(|value| value.value()),
                translation_zyx: translation_zyx.map(|value| value.value()),
            }),
            OmeLevelTransform::UnitlessIdentity => {
                bail!("T5 source-oracle transform authority requires micrometer calibration")
            }
        })
        .collect()
}

#[cfg(test)]
fn transform_facts_match(
    actual: &[ObservedTransformFact],
    expected: &[t5_sentinel_oracle::OracleTransformFact],
) -> bool {
    actual.len() == expected.len()
        && actual.iter().zip(expected).all(|(actual, expected)| {
            actual.scale_ordinal == expected.scale_ordinal
                && actual
                    .scale_zyx
                    .iter()
                    .zip(expected.scale_zyx)
                    .all(|(actual, expected)| actual.to_bits() == expected.to_bits())
                && actual
                    .translation_zyx
                    .iter()
                    .zip(expected.translation_zyx)
                    .all(|(actual, expected)| actual.to_bits() == expected.to_bits())
        })
}

fn expected_transform_facts_match(
    actual: &[ObservedTransformFact],
    expected: &[ExpectedTransformFact],
) -> bool {
    let mut actual = actual
        .iter()
        .map(|fact| {
            (
                fact.scale_ordinal,
                fact.scale_zyx.map(f64::to_bits),
                fact.translation_zyx.map(f64::to_bits),
            )
        })
        .collect::<Vec<_>>();
    actual.sort_unstable_by_key(|fact| fact.0);
    let mut expected = expected
        .iter()
        .map(|fact| {
            (
                fact.scale_ordinal,
                fact.scale_zyx.map(f64::to_bits),
                fact.translation_zyx.map(f64::to_bits),
            )
        })
        .collect::<Vec<_>>();
    expected.sort_unstable_by_key(|fact| fact.0);
    actual == expected
}

fn scale_fact(
    self_consistent: &mirante4d_storage::SelfConsistentPackageCapability,
    image_ordinal: u32,
    level: &mirante4d_storage::ProfileLevel,
    cancelled: &AtomicBool,
) -> anyhow::Result<(ExpectedScaleFact, u64)> {
    let metadata_path =
        mirante4d_storage::PackagePath::parse(&format!("{}/zarr.json", level.pixel_path()))?;
    let array = self_consistent
        .catalog()
        .zarr_array(&metadata_path)
        .context("T5 profile level lacks its opened pixel array")?;
    let shape: [u64; 5] = array
        .shape()
        .try_into()
        .map_err(|_| anyhow::anyhow!("T5 pixel scale is not t,c,z,y,x"))?;
    let (brick_shape, sample_bytes, kind_tag) = pixel_kind_facts(array.kind())?;
    let logical_voxels = shape
        .into_iter()
        .try_fold(1_u64, |product, dimension| product.checked_mul(dimension))
        .context("T5 scale logical voxel count overflowed")?;
    let canonical_pixel_bytes = logical_voxels
        .checked_mul(u64::try_from(sample_bytes)?)
        .context("T5 scale canonical pixel byte count overflowed")?;
    let chunk_counts = [
        ceil_div(shape[2], brick_shape[0])?,
        ceil_div(shape[3], brick_shape[1])?,
        ceil_div(shape[4], brick_shape[2])?,
    ];
    let brick_reads = [
        shape[0],
        shape[1],
        chunk_counts[0],
        chunk_counts[1],
        chunk_counts[2],
    ]
    .into_iter()
    .try_fold(1_u64, |product, dimension| product.checked_mul(dimension))
    .context("T5 scale brick count overflowed")?;

    let mut hasher = Sha256Hasher::new();
    hasher.update(b"mirante4d-t5-canonical-scale-voxels-1\0");
    hasher.update(image_ordinal.to_le_bytes());
    hasher.update(level.scale_ordinal().to_le_bytes());
    for dimension in shape {
        hasher.update(dimension.to_le_bytes());
    }
    hasher.update([kind_tag]);
    hasher.update([match level.validity_mode() {
        ProfileValidityMode::AllValid => 0,
        ProfileValidityMode::Explicit => 1,
    }]);

    let full_brick_voxels = brick_shape
        .into_iter()
        .try_fold(1_u64, |product, dimension| product.checked_mul(dimension))
        .context("T5 full brick voxel count overflowed")?;
    let expected_pixel_bytes = usize::try_from(
        full_brick_voxels
            .checked_mul(u64::try_from(sample_bytes)?)
            .context("T5 full brick byte count overflowed")?,
    )?;
    let expected_validity_bytes = usize::try_from(full_brick_voxels.div_ceil(8))?;
    let mut observed_bricks = 0_u64;
    let mut row = Vec::new();
    for t in 0..shape[0] {
        for c in 0..shape[1] {
            for z_chunk in 0..chunk_counts[0] {
                for y_chunk in 0..chunk_counts[1] {
                    for x_chunk in 0..chunk_counts[2] {
                        require_not_cancelled(cancelled, "independent scale-brick traversal")?;
                        let coordinates = PackedIndexCoordinates::new(
                            image_ordinal,
                            level.scale_ordinal(),
                            u32::try_from(t)?,
                            u32::try_from(c)?,
                            u32::try_from(z_chunk)?,
                            u32::try_from(y_chunk)?,
                            u32::try_from(x_chunk)?,
                        );
                        let brick = self_consistent
                            .read_brick(coordinates, || cancelled.load(Ordering::Acquire))
                            .context("T5 independent per-scale brick read failed")?;
                        observed_bricks = observed_bricks
                            .checked_add(1)
                            .context("T5 observed brick count overflowed")?;
                        if let Some(pixel) = brick.pixel_payload()
                            && pixel.len() != expected_pixel_bytes
                        {
                            bail!("T5 scale brick pixel payload has the wrong fixed length");
                        }
                        if let Some(validity) = brick.validity_payload()
                            && validity.len() != expected_validity_bytes
                        {
                            bail!("T5 scale brick validity payload has the wrong fixed length");
                        }
                        if brick.record().explicit_validity()
                            != (level.validity_mode() == ProfileValidityMode::Explicit)
                        {
                            bail!("T5 scale brick validity mode differs from the profile");
                        }
                        for coordinate in [
                            u64::from(image_ordinal),
                            u64::from(level.scale_ordinal()),
                            t,
                            c,
                            z_chunk,
                            y_chunk,
                            x_chunk,
                        ] {
                            hasher.update(coordinate.to_le_bytes());
                        }
                        let extent = brick.logical_extent_zyx();
                        for dimension in extent {
                            hasher.update(dimension.to_le_bytes());
                        }
                        let row_capacity = usize::try_from(extent[2])?
                            .checked_mul(sample_bytes + 1)
                            .context("T5 canonical row byte count overflowed")?;
                        row.clear();
                        row.reserve(row_capacity.saturating_sub(row.capacity()));
                        for z in 0..extent[0] {
                            require_not_cancelled(
                                cancelled,
                                "independent scale-brick row traversal",
                            )?;
                            for y in 0..extent[1] {
                                row.clear();
                                for x in 0..extent[2] {
                                    let source_index = linear_3d([z, y, x], brick_shape)?;
                                    let valid = effective_validity(&brick, source_index)?;
                                    row.push(u8::from(valid));
                                    append_canonical_sample(
                                        &mut row,
                                        brick.pixel_payload(),
                                        source_index,
                                        sample_bytes,
                                        valid,
                                    )?;
                                }
                                hasher.update(&row);
                            }
                        }
                    }
                }
            }
        }
    }
    if observed_bricks != brick_reads {
        bail!("T5 independent per-scale traversal missed a brick");
    }
    Ok((
        ExpectedScaleFact {
            image_ordinal,
            scale_ordinal: level.scale_ordinal(),
            digest_sha256: hasher.finalize().to_string(),
            brick_reads,
            logical_voxels,
        },
        canonical_pixel_bytes,
    ))
}

fn append_canonical_sample(
    row: &mut Vec<u8>,
    pixel_payload: Option<&[u8]>,
    source_index: usize,
    sample_bytes: usize,
    valid: bool,
) -> anyhow::Result<()> {
    let sample = if let Some(pixel) = pixel_payload {
        let start = source_index
            .checked_mul(sample_bytes)
            .context("T5 scale pixel offset overflowed")?;
        let end = start
            .checked_add(sample_bytes)
            .context("T5 scale pixel end overflowed")?;
        Some(
            pixel
                .get(start..end)
                .context("T5 scale pixel payload is shorter than its declared brick")?,
        )
    } else {
        None
    };
    if !valid && sample.is_some_and(|bytes| bytes.iter().any(|byte| *byte != 0)) {
        bail!("T5 invalid voxel is not stored with canonical-zero pixel bytes");
    }
    if valid {
        if let Some(sample) = sample {
            row.extend_from_slice(sample);
        } else {
            row.resize(row.len() + sample_bytes, 0);
        }
    } else {
        row.resize(row.len() + sample_bytes, 0);
    }
    Ok(())
}

fn pixel_kind_facts(kind: ShardProfileKind) -> anyhow::Result<([u64; 3], usize, u8)> {
    Ok(match kind {
        ShardProfileKind::Pixel3dUint8 => ([64, 64, 64], 1, 1),
        ShardProfileKind::Pixel3dUint16 => ([64, 64, 64], 2, 2),
        ShardProfileKind::Pixel3dFloat32 => ([64, 64, 64], 4, 3),
        ShardProfileKind::Pixel2dUint8 => ([1, 256, 256], 1, 4),
        ShardProfileKind::Pixel2dUint16 => ([1, 256, 256], 2, 5),
        ShardProfileKind::Pixel2dFloat32 => ([1, 256, 256], 4, 6),
        ShardProfileKind::Validity3d
        | ShardProfileKind::Validity2d
        | ShardProfileKind::PackedIndex => bail!("T5 profile level names a non-pixel array"),
    })
}

fn effective_validity(
    brick: &mirante4d_storage::LocalBrickRead,
    source_index: usize,
) -> anyhow::Result<bool> {
    if !brick.record().explicit_validity() {
        return Ok(true);
    }
    if brick.record().statistics().valid_voxel_count() == 0 {
        return Ok(false);
    }
    let validity = brick
        .validity_payload()
        .context("T5 partially valid brick has no validity payload")?;
    Ok(validity[source_index / 8] & (1 << (source_index % 8)) != 0)
}

fn linear_3d(coordinate: [u64; 3], shape: [u64; 3]) -> anyhow::Result<usize> {
    let index = coordinate[0]
        .checked_mul(shape[1])
        .and_then(|value| value.checked_add(coordinate[1]))
        .and_then(|value| value.checked_mul(shape[2]))
        .and_then(|value| value.checked_add(coordinate[2]))
        .context("T5 brick coordinate overflowed")?;
    usize::try_from(index).context("T5 brick coordinate exceeds this platform")
}

fn ceil_div(value: u64, divisor: u64) -> anyhow::Result<u64> {
    if value == 0 || divisor == 0 {
        bail!("T5 scale contains a zero dimension");
    }
    Ok(value
        .checked_add(divisor - 1)
        .context("T5 scale ceiling division overflowed")?
        / divisor)
}

fn checkpoint_directory(destination: &Path) -> anyhow::Result<PathBuf> {
    let parent = destination
        .parent()
        .context("T5 destination has no parent")?;
    let name = destination
        .file_name()
        .context("T5 destination has no package name")?
        .to_string_lossy();
    Ok(parent.join(format!(".{name}.import-checkpoint")))
}

fn release_app_binary(target_directory: &Path) -> PathBuf {
    target_directory
        .join("release")
        .join(format!("mirante4d-app{}", env::consts::EXE_SUFFIX))
}

fn build_standard_release_app(
    repository_root: &Path,
    target_directory: &Path,
    revision: &str,
    compiler: &str,
) -> anyhow::Result<()> {
    let mut command = cargo_command();
    command
        .current_dir(repository_root)
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
        .args(["build", "--locked", "--release", "--target-dir"])
        .arg(target_directory)
        .args(["-p", "mirante4d-app"]);
    let status = command
        .status()
        .context("failed to spawn the fresh T5 release app build")?;
    if !status.success() {
        bail!("fresh T5 release app build failed with status {status}");
    }
    Ok(())
}

fn validate_release_executable(path: &Path, description: &str) -> anyhow::Result<()> {
    let link = fs::symlink_metadata(path)
        .with_context(|| format!("{description} is absent at {}", path.display()))?;
    if link.file_type().is_symlink() || !link.is_file() {
        bail!("{description} must be a nonsymlink regular file");
    }
    if link.permissions().mode() & 0o111 == 0 {
        bail!("{description} is not executable");
    }
    let expected_parent = path
        .parent()
        .context("T5 release app binary has no profile directory")?;
    if expected_parent.file_name().and_then(|name| name.to_str()) != Some("release") {
        bail!("{description} is not from the release profile");
    }
    Ok(())
}

fn same_file_metadata(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}

fn repository_same_clean_revision(start: &RepositoryIdentity, end: &RepositoryIdentity) -> bool {
    start.root.is_some()
        && start.commit.is_some()
        && start.dirty_worktree == Some(false)
        && end.dirty_worktree == Some(false)
        && start.root == end.root
        && start.commit == end.commit
}

fn repository_evidence(identity: &RepositoryIdentity) -> Value {
    json!({
        "root": identity.root,
        "commit": identity.commit,
        "dirty_worktree": identity.dirty_worktree,
    })
}

fn inventory_json(inventory: &InventoryFacts) -> Value {
    json!({
        "regular_files": inventory.regular_files,
        "source_bytes": inventory.source_bytes,
        "inventory_sha256": inventory.sha256,
    })
}

fn read_bounded_json(path: &Path, maximum_bytes: u64) -> anyhow::Result<Value> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > maximum_bytes
    {
        bail!("T5 JSON evidence file has an invalid type or bounded length");
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    File::open(path)?
        .take(maximum_bytes + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).ok() != Some(metadata.len()) {
        bail!("T5 JSON evidence changed while read");
    }
    serde_json::from_slice(&bytes).context("T5 JSON evidence is malformed")
}

fn write_new_synced_json(path: &Path, value: &Value) -> anyhow::Result<()> {
    let parent = path.parent().context("T5 evidence path has no parent")?;
    fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn create_private_directory(path: &Path) -> std::io::Result<()> {
    fs::DirBuilder::new().mode(0o700).create(path)
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let expected = file.metadata()?.len();
    let mut hasher = Sha256Hasher::new();
    let mut consumed = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        consumed = consumed
            .checked_add(u64::try_from(count)?)
            .context("T5 file digest byte count overflowed")?;
        hasher.update(&buffer[..count]);
    }
    if consumed != expected || file.metadata()?.len() != expected {
        bail!("T5 file changed while its digest was captured");
    }
    Ok(hasher.finalize().to_string())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256Hasher::new();
    hasher.update(bytes);
    hasher.finalize().to_string()
}

fn session_id() -> anyhow::Result<String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time precedes the Unix epoch")?
        .as_nanos();
    Ok(format!("t5-{nanos}-{}", std::process::id()))
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn command_text(program: &str, args: &[&str]) -> Value {
    match Command::new(program).args(args).output() {
        Ok(output) => json!({
            "program": program,
            "arguments": args,
            "success": output.status.success(),
            "stdout": String::from_utf8_lossy(&output.stdout).trim(),
            "stderr": String::from_utf8_lossy(&output.stderr).trim(),
        }),
        Err(error) => json!({
            "program": program,
            "arguments": args,
            "success": false,
            "error": error.to_string(),
        }),
    }
}

fn validate_sanitized_report(report: &Value, private_strings: &[String]) -> anyhow::Result<()> {
    fn visit(value: &Value, private_strings: &[String]) -> anyhow::Result<()> {
        match value {
            Value::String(string) => {
                let is_raw_identity = string.starts_with("m4d-sc-v1-sha256:")
                    || string.starts_with("m4d-package-v1-sha256:");
                let is_path = Path::new(string).is_absolute();
                let contains_private = private_strings
                    .iter()
                    .filter(|private| !private.is_empty())
                    .any(|private| string.contains(private));
                if is_raw_identity || is_path || contains_private {
                    bail!("sanitized T5 summary retained a private path, identity, or digest");
                }
            }
            Value::Array(values) => {
                for value in values {
                    visit(value, private_strings)?;
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    if matches!(
                        key.as_str(),
                        "scale_zyx"
                            | "translation_zyx"
                            | "logical_voxels"
                            | "brick_reads"
                            | "canonical_base_pixel_bytes"
                            | "source_planes"
                            | "source_files"
                            | "source_inventory_name_bytes"
                            | "required_scratch_bytes"
                            | "scratch_free_bytes"
                    ) && !matches!(value, Value::Bool(_))
                    {
                        bail!("sanitized T5 summary retained a private numeric oracle fact");
                    }
                    visit(value, private_strings)?;
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
        Ok(())
    }
    visit(report, private_strings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_oracle_facts() -> OracleFacts {
        let scales = (0..T5_SCALE_COUNT)
            .map(|scale| t5_sentinel_oracle::OracleScaleFact {
                image_ordinal: 0,
                scale_ordinal: u32::try_from(scale).unwrap(),
                digest_sha256: "2".repeat(64),
                brick_reads: 1,
                logical_voxels: if scale == 0 { 16 } else { 1 },
            })
            .collect();
        let transforms = (0..T5_SCALE_COUNT)
            .map(|scale| {
                let factor = f64::from(1_u32 << scale);
                t5_sentinel_oracle::OracleTransformFact {
                    scale_ordinal: u32::try_from(scale).unwrap(),
                    scale_zyx: [factor; 3],
                    translation_zyx: [(factor - 1.0) / 2.0; 3],
                }
            })
            .collect();
        OracleFacts {
            scientific_content_id: format!("m4d-sc-v1-sha256:{}", "0".repeat(64)),
            layer_roots: vec![t5_sentinel_oracle::OracleLayerRoot {
                logical_layer: 0,
                digest_sha256: "1".repeat(64),
            }],
            scales,
            transforms,
            canonical_base_pixel_bytes: 16,
            resources: t5_sentinel_oracle::OracleResources {
                calculated_peak_working_bytes: 1024,
                required_scratch_bytes: 2048,
                scratch_free_bytes: 4096,
                peak_scratch_regular_files: 2,
                scratch_files_remaining: 0,
                calculated_open_files_bound: 4,
                source_planes: 1,
                source_files: 1,
                source_inventory_name_bytes: 8,
                deterministic_filename_order: true,
                source_unchanged: true,
            },
        }
    }

    fn valid_config_json(root: &Path) -> Value {
        let scales = (0..T5_SCALE_COUNT)
            .map(|scale| {
                json!({
                    "image_ordinal": 0,
                    "scale_ordinal": scale,
                    "digest_sha256": "2".repeat(64),
                    "brick_reads": 1,
                    "logical_voxels": if scale == 0 { 16 } else { 1 },
                })
            })
            .collect::<Vec<_>>();
        let transforms = (0..T5_SCALE_COUNT)
            .map(|scale| {
                let factor = f64::from(1_u32 << scale);
                let translation = (factor - 1.0) / 2.0;
                json!({
                    "scale_ordinal": scale,
                    "scale_zyx": [factor, factor, factor],
                    "translation_zyx": [translation, translation, translation],
                })
            })
            .collect::<Vec<_>>();
        json!({
            "schema": CONFIG_SCHEMA,
            "workload_id": "t5-0123456789abcdef0123456789abcdef",
            "source": root.join("source"),
            "scratch_root": root.join("scratch"),
            "qualification_profile": root.join("profile.json"),
            "expected_profile": ProfileKind::Current.name(),
            "spacing_zyx_um": [1.0, 1.0, 1.0],
            "time_step_seconds": null,
            "no_data_sentinel": 255,
            "working_memory_bytes": WORKING_MEMORY_BYTES,
            "primary_timeout_seconds": 1200,
            "cache_condition": "warm",
            "competing_activity": "none",
            "expected": {
                "expected_fact_authority": FACT_AUTHORITY,
                "source_inventory_sha256": "3".repeat(64),
                "reviewed_source_fingerprint_sha256": "4".repeat(64),
                "canonical_source_pixel_bytes": 16,
                "scientific_content_id": format!("m4d-sc-v1-sha256:{}", "0".repeat(64)),
                "scientific_layer_roots": [{
                    "logical_layer": 0,
                    "digest_sha256": "1".repeat(64),
                }],
                "scale_digest_scheme": SCALE_DIGEST_SCHEME,
                "scales": scales,
                "transforms": transforms,
            },
        })
    }

    #[test]
    fn strict_config_requires_frozen_private_facts() {
        let root = Path::new("/private");
        let config: T5Config = serde_json::from_value(valid_config_json(root)).unwrap();
        validate_config(&config).unwrap();

        let mut missing = valid_config_json(root);
        missing["expected"]["scales"] = json!([]);
        let config: T5Config = serde_json::from_value(missing).unwrap();
        assert!(validate_config(&config).is_err());

        let mut invalid_source_inventory = valid_config_json(root);
        invalid_source_inventory["expected"]["source_inventory_sha256"] = json!("not-a-digest");
        let config: T5Config = serde_json::from_value(invalid_source_inventory).unwrap();
        assert!(validate_config(&config).is_err());

        let mut private_protocol_label = valid_config_json(root);
        private_protocol_label["competing_activity"] = json!("private-dataset-label");
        let config: T5Config = serde_json::from_value(private_protocol_label).unwrap();
        assert!(validate_config(&config).is_err());

        let mut extra = valid_config_json(root);
        extra["unexpected"] = json!(true);
        assert!(serde_json::from_value::<T5Config>(extra).is_err());

        let mut legacy_schema = valid_config_json(root);
        legacy_schema["schema"] = json!("mirante4d-private-import-performance-t5-1");
        let config: T5Config = serde_json::from_value(legacy_schema).unwrap();
        assert!(validate_config(&config).is_err());

        let mut missing_authority = valid_config_json(root);
        missing_authority["expected"]
            .as_object_mut()
            .unwrap()
            .remove("expected_fact_authority");
        assert!(serde_json::from_value::<T5Config>(missing_authority).is_err());

        let mut missing_sentinel = valid_config_json(root);
        missing_sentinel["no_data_sentinel"] = Value::Null;
        assert!(serde_json::from_value::<T5Config>(missing_sentinel).is_err());

        let mut duplicate_transform: T5Config =
            serde_json::from_value(valid_config_json(root)).unwrap();
        duplicate_transform.expected.transforms[1].scale_ordinal = 0;
        assert!(validate_config(&duplicate_transform).is_err());

        let mut nonfinite_transform: T5Config =
            serde_json::from_value(valid_config_json(root)).unwrap();
        nonfinite_transform.expected.transforms[1].scale_zyx[0] = f64::INFINITY;
        assert!(validate_config(&nonfinite_transform).is_err());
    }

    #[test]
    fn t5_automation_v11_binds_the_exact_canonical_hard_safety_echo() {
        let tempdir = tempfile::tempdir().unwrap();
        let config: T5Config = serde_json::from_value(valid_config_json(tempdir.path())).unwrap();
        let script = automation_script(
            &config,
            &tempdir.path().join("startup.m4d"),
            &tempdir.path().join("source"),
            &tempdir.path().join("output"),
            &tempdir.path().join("output/imported.m4d"),
        )
        .unwrap();
        assert_eq!(script["schema"], PRODUCT_AUTOMATION_SCRIPT_SCHEMA);
        assert_eq!(SCRIPT_SCHEMA_VERSION, 11);
        assert_eq!(script["schema_version"], SCRIPT_SCHEMA_VERSION);
        assert!(script.get("limits").is_none());
        let hard_safety_limits = script["hard_safety_limits"].as_object().unwrap();
        assert_eq!(
            hard_safety_limits
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            PRODUCT_AUTOMATION_HARD_SAFETY_LIMIT_FIELDS
                .into_iter()
                .collect::<BTreeSet<_>>()
        );
        assert_eq!(
            hard_safety_limits["max_cpu_total_bytes"],
            1024 * 1024 * 1024
        );
        assert_eq!(
            hard_safety_limits["max_cpu_import_working_set_bytes"],
            WORKING_MEMORY_BYTES
        );
        assert_eq!(hard_safety_limits["max_runtime_queued_requests"], 192);
        assert_eq!(hard_safety_limits["max_cpu_prefetch_bytes"], Value::Null);

        let script_path = tempdir.path().join("automation-script.json");
        write_new_synced_json(&script_path, &script).unwrap();
        let report = json!({
            "schema": PRODUCT_AUTOMATION_REPORT_SCHEMA,
            "schema_version": REPORT_SCHEMA_VERSION,
            "status": "passed",
            "failure_reason": null,
            "script": {
                "path": script_path,
                "schema": script["schema"],
                "schema_version": script["schema_version"],
                "scenario": script["scenario"],
                "command_count": script["commands"].as_array().unwrap().len(),
            },
            "hard_safety_limits": script["hard_safety_limits"],
        });
        validate_product_automation_report_contract(&report, &script, &script_path).unwrap();

        let mut predecessor = report.clone();
        predecessor["schema_version"] = json!(4);
        assert!(
            validate_product_automation_report_contract(&predecessor, &script, &script_path)
                .is_err()
        );
        let mut legacy = report.clone();
        legacy["limits"] = json!({});
        assert!(
            validate_product_automation_report_contract(&legacy, &script, &script_path).is_err()
        );
        let mut missing_limit = report.clone();
        missing_limit["hard_safety_limits"]
            .as_object_mut()
            .unwrap()
            .remove("max_cpu_prefetch_bytes");
        assert!(
            validate_product_automation_report_contract(&missing_limit, &script, &script_path)
                .is_err()
        );
        let mut wrong_count = report;
        wrong_count["script"]["command_count"] = json!(0);
        assert!(
            validate_product_automation_report_contract(&wrong_count, &script, &script_path)
                .is_err()
        );
    }

    #[test]
    fn t5_app_run_binds_shared_progress_plan_safe_output_and_typed_failures() {
        let config: T5Config =
            serde_json::from_value(valid_config_json(Path::new("/private"))).unwrap();
        let script = automation_script(
            &config,
            Path::new("/private/startup.m4d"),
            Path::new("/private/source"),
            Path::new("/private/output"),
            Path::new("/private/output/source.m4d"),
        )
        .unwrap();
        let plan = ProductAutomationProgressPlan::from_script(&script).unwrap();
        assert_eq!(plan.command_count(), 20);
        assert_eq!(
            plan.command_budget(9),
            Some(Duration::from_secs(config.primary_timeout_seconds))
        );
        assert_eq!(
            plan.command_budget(6),
            Some(Duration::from_secs(INSPECTION_TIMEOUT_SECONDS))
        );

        let line = t5_safe_progress_line(&SafeProgressSnapshot {
            heartbeat_sequence: 7,
            command_count: plan.command_count(),
            state: SafeProgressState::Command {
                index: 9,
                command_kind: "wait_for_imported_open_ready",
                elapsed_ms: 123,
            },
        });
        assert_eq!(
            line,
            "automation_progress scope=t5 scenario=import_preprocessing heartbeat_sequence=7 command_count=20 state=command command_index=9 command_kind=wait_for_imported_open_ready elapsed_ms=123"
        );
        assert!(!line.contains("/private"));

        let failure = t5_progress_failure(ProgressFailure::HeartbeatStale);
        assert_eq!(failure.to_string(), "progress_heartbeat_stale");
        assert_eq!(
            failure.downcast_ref::<ProgressFailure>(),
            Some(&ProgressFailure::HeartbeatStale)
        );
    }

    #[test]
    fn t5_x11_probes_have_short_silence_absolute_and_output_bounds() {
        assert_eq!(
            T5_X11_AUTOMATION_OUTPUT_POLICY.inactivity_timeout,
            Duration::from_secs(2)
        );
        assert_eq!(
            T5_X11_AUTOMATION_OUTPUT_POLICY.absolute_timeout,
            Duration::from_secs(3)
        );
        assert_eq!(
            T5_X11_AUTOMATION_OUTPUT_POLICY.progress_interval,
            Duration::from_secs(1)
        );
        assert!(
            T5_X11_AUTOMATION_OUTPUT_POLICY.inactivity_timeout
                < T5_X11_AUTOMATION_OUTPUT_POLICY.absolute_timeout
        );
        assert_eq!(T5_X11_AUTOMATION_OUTPUT_POLICY.max_stdout_bytes, 64 * 1024);
        assert_eq!(T5_X11_AUTOMATION_OUTPUT_POLICY.max_stderr_bytes, 64 * 1024);
    }

    #[test]
    fn argument_parser_separates_correctness_from_optional_performance() {
        let parsed = parse_args(vec!["--config".into(), "/private/config.json".into()]).unwrap();
        assert_eq!(parsed.samples, CORRECTNESS_SAMPLES);
        assert!(!parsed.diagnostic);
        assert_eq!(parsed.qualification_kind, QualificationKind::Correctness);
        let performance = parse_args(vec![
            "--config".into(),
            "/private/config.json".into(),
            "--performance".into(),
        ])
        .unwrap();
        assert_eq!(performance.samples, PERFORMANCE_SAMPLES);
        assert_eq!(
            performance.qualification_kind,
            QualificationKind::Performance
        );
        assert!(
            parse_args(vec![
                "--config".into(),
                "/private/config.json".into(),
                "--samples".into(),
                "3".into(),
            ])
            .is_err()
        );
        assert!(
            parse_args(vec![
                "--config".into(),
                "/private/config.json".into(),
                "--diagnostic".into(),
                "--performance".into(),
            ])
            .is_err()
        );
        assert!(parse_args(vec!["--config".into(), "relative.json".into()]).is_err());
    }

    #[test]
    fn oracle_audit_parser_requires_exactly_one_absolute_v2_config() {
        assert_eq!(
            parse_oracle_audit_args(vec!["--config".into(), "/private/current.json".into()])
                .unwrap(),
            OracleAuditArgs {
                config: PathBuf::from("/private/current.json"),
            }
        );
        assert!(parse_oracle_audit_args(Vec::new()).is_err());
        assert!(parse_oracle_audit_args(vec!["--config".into(), "relative.json".into()]).is_err());
        assert!(
            parse_oracle_audit_args(vec![
                "--config".into(),
                "/private/current.json".into(),
                "--output".into(),
                "/private/other.json".into(),
            ])
            .is_err()
        );
        assert!(
            parse_oracle_audit_args(vec![
                "--config".into(),
                "/private/current.json".into(),
                "--config".into(),
                "/private/other.json".into(),
            ])
            .is_err()
        );
    }

    #[test]
    fn finalized_raw_publisher_parser_requires_two_absolute_paths() {
        assert_eq!(
            parse_publish_args(vec![
                "--config".into(),
                "/private/current.json".into(),
                "--raw-report".into(),
                "/private/raw-private-report.json".into(),
            ])
            .unwrap(),
            PublishArgs {
                config: PathBuf::from("/private/current.json"),
                raw_report: PathBuf::from("/private/raw-private-report.json"),
            }
        );
        assert!(parse_publish_args(Vec::new()).is_err());
        assert!(
            parse_publish_args(vec![
                "--config".into(),
                "/private/current.json".into(),
                "--raw-report".into(),
                "relative.json".into(),
            ])
            .is_err()
        );
        assert!(
            parse_publish_args(vec![
                "--config".into(),
                "/private/current.json".into(),
                "--raw-report".into(),
                "/private/raw.json".into(),
                "--raw-report".into(),
                "/private/other.json".into(),
            ])
            .is_err()
        );
    }

    #[test]
    fn source_oracle_gate_binds_facts_resources_and_source_preservation() {
        let config: T5Config =
            serde_json::from_value(valid_config_json(Path::new("/private"))).unwrap();
        let inventory = InventoryFacts {
            regular_files: 1,
            source_bytes: 123,
            sha256: "3".repeat(64),
        };
        let oracle = stub_oracle_facts();
        let gates = source_oracle_gates(
            &oracle,
            &config.expected,
            &inventory,
            &inventory,
            true,
            WORKING_MEMORY_BYTES,
        );
        assert!(gates.values().all(|passed| *passed));

        let mut changed_inventory = inventory.clone();
        changed_inventory.sha256 = "5".repeat(64);
        assert!(
            !source_oracle_gates(
                &oracle,
                &config.expected,
                &changed_inventory,
                &changed_inventory,
                true,
                WORKING_MEMORY_BYTES,
            )["frozen_source_inventory"]
        );

        let mut bit_different_expected = config.expected.clone();
        bit_different_expected.transforms[0].translation_zyx[0] = -0.0;
        assert!(
            !source_oracle_gates(
                &oracle,
                &bit_different_expected,
                &inventory,
                &inventory,
                true,
                WORKING_MEMORY_BYTES,
            )["frozen_centered_transforms"]
        );

        let mut oversized = oracle.clone();
        oversized.resources.calculated_peak_working_bytes = WORKING_MEMORY_BYTES + 1;
        assert!(
            !source_oracle_gates(
                &oversized,
                &config.expected,
                &inventory,
                &inventory,
                true,
                WORKING_MEMORY_BYTES,
            )["working_memory_bound"]
        );
        assert!(
            !source_oracle_gates(
                &oracle,
                &config.expected,
                &inventory,
                &inventory,
                false,
                WORKING_MEMORY_BYTES,
            )["zero_scratch_remnants"]
        );
    }

    #[test]
    fn package_transform_gate_is_bit_exact_for_config_and_oracle() {
        let oracle = stub_oracle_facts();
        let config: T5Config =
            serde_json::from_value(valid_config_json(Path::new("/private"))).unwrap();
        let observed = oracle
            .transforms
            .iter()
            .map(|fact| ObservedTransformFact {
                scale_ordinal: fact.scale_ordinal,
                scale_zyx: fact.scale_zyx,
                translation_zyx: fact.translation_zyx,
            })
            .collect::<Vec<_>>();
        assert!(transform_facts_match(&observed, &oracle.transforms));
        assert!(expected_transform_facts_match(
            &observed,
            &config.expected.transforms
        ));

        let mut drifted = config.expected.transforms.clone();
        drifted[0].translation_zyx[0] = -0.0;
        assert!(!expected_transform_facts_match(&observed, &drifted));
    }

    #[test]
    fn package_readback_rejects_nonzero_bytes_beneath_invalidity() {
        let mut row = Vec::new();
        append_canonical_sample(&mut row, Some(&[7, 8, 0, 0]), 0, 2, true).unwrap();
        append_canonical_sample(&mut row, Some(&[7, 8, 0, 0]), 1, 2, false).unwrap();
        assert_eq!(row, vec![7, 8, 0, 0]);

        assert!(append_canonical_sample(&mut Vec::new(), Some(&[255]), 0, 1, false).is_err());
        let mut implicit_zero = Vec::new();
        append_canonical_sample(&mut implicit_zero, None, 0, 1, true).unwrap();
        assert_eq!(implicit_zero, vec![0]);
    }

    #[test]
    fn build_environment_filter_exempts_only_matching_embedded_rustflags_provenance() {
        assert!(!is_compiler_or_profile_override(
            "MIRANTE4D_XTASK_BUILD_CUSTOM_RUSTFLAGS",
            OsStr::new("false"),
            false,
        ));
        assert!(!is_compiler_or_profile_override(
            "MIRANTE4D_XTASK_BUILD_CUSTOM_RUSTFLAGS",
            OsStr::new("true"),
            true,
        ));
        assert!(is_compiler_or_profile_override(
            "MIRANTE4D_XTASK_BUILD_CUSTOM_RUSTFLAGS",
            OsStr::new("true"),
            false,
        ));
        assert!(is_compiler_or_profile_override(
            "MIRANTE4D_XTASK_BUILD_CUSTOM_RUSTFLAGS",
            OsStr::new("false"),
            true,
        ));
        for variable in [
            "RUSTC",
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
            "PRIVATE_RUSTFLAGS",
            "CARGO_PROFILE_RELEASE_OPT_LEVEL",
        ] {
            assert!(
                is_compiler_or_profile_override(variable, OsStr::new("set"), false),
                "{variable} must remain rejected"
            );
            assert!(!is_compiler_or_profile_override(
                variable,
                OsStr::new(""),
                false,
            ));
        }
        for variable in [
            "MIRANTE4D_XTASK_BUILD_GIT_HEAD",
            "MIRANTE4D_XTASK_BUILD_RUSTC_WRAPPER",
            "CARGO_PROFILE_DEV_OPT_LEVEL",
        ] {
            assert!(
                !is_compiler_or_profile_override(variable, OsStr::new("set"), false),
                "{variable} is not a release compiler/profile override"
            );
        }
    }

    #[test]
    fn owner_bindings_pin_exact_reviewed_commitments() {
        assert_eq!(
            OWNER_ACCEPTED_IMPORT_QUALIFICATION_PROFILE_SHA256,
            Some("50d18c8d3f695a90ff879fc6cdea210b273cc52f97f63350931e42fdd2b38abe")
        );
        assert_eq!(OWNER_ACCEPTED_T5_CONFIG_SHA256, None);
        assert_eq!(
            config_binding_status("unreviewed-current-format-configuration"),
            "pending_owner_acceptance"
        );
    }

    #[test]
    fn correctness_and_performance_claims_have_distinct_gates() {
        assert!(correctness_qualification_claim_passed(
            false, true, true, true
        ));
        assert!(!correctness_qualification_claim_passed(
            true, true, true, true
        ));
        assert!(!correctness_qualification_claim_passed(
            false, false, true, true
        ));
        assert!(!correctness_qualification_claim_passed(
            false, true, false, true
        ));
        assert!(!correctness_qualification_claim_passed(
            false, true, true, false
        ));

        assert!(performance_qualification_claim_passed(
            true,
            true,
            true,
            Some(true)
        ));
        assert!(!performance_qualification_claim_passed(
            false,
            true,
            true,
            Some(true)
        ));
        assert!(!performance_qualification_claim_passed(
            true, true, true, None
        ));
        assert!(!performance_qualification_claim_passed(
            true,
            true,
            false,
            Some(true)
        ));
    }

    #[test]
    fn rss_classification_uses_last_prestart_sample_and_primary_peak() {
        let samples = [
            RssSample {
                epoch_ms: 9,
                rss_bytes: 100,
            },
            RssSample {
                epoch_ms: 10,
                rss_bytes: 120,
            },
            RssSample {
                epoch_ms: 11,
                rss_bytes: 180,
            },
            RssSample {
                epoch_ms: 12,
                rss_bytes: 150,
            },
        ];
        assert_eq!(
            classify_rss_samples(&samples, 10, 12).unwrap(),
            (100, 180, 80)
        );
    }

    #[test]
    fn linux_rss_parser_and_exit_reconciliation_reject_malformed_live_status() {
        assert_eq!(
            parse_process_rss_bytes("Name:\tapp\nVmRSS:\t123 kB\n").unwrap(),
            Some(125_952)
        );
        assert_eq!(
            parse_process_rss_bytes("Name:\tapp\nState:\tZ (zombie)\n").unwrap(),
            None
        );
        assert_eq!(
            parse_process_rss_bytes("Name:\tapp\nState:\tR (running)\n").unwrap(),
            None
        );
        for malformed in [
            "Name:\tapp\nVmSize:\t123 kB\n",
            "Name:\tapp\nRssAnon:\t123 kB\n",
            "VmRSS:\n",
            "VmRSS:\tnot-a-number kB\n",
            "VmRSS:\t123 bytes\n",
            "VmRSS:\t123 kB trailing\n",
            "VmRSS:\t18446744073709551615 kB\n",
        ] {
            assert!(parse_process_rss_bytes(malformed).is_err());
        }

        assert_eq!(
            reconcile_process_rss_poll(Ok(None), Some("exited")).unwrap(),
            ProcessRssPoll::Exited("exited")
        );
        assert_eq!(
            reconcile_process_rss_poll(Ok(Some(42)), Some("exited")).unwrap(),
            ProcessRssPoll::Exited("exited")
        );
        assert_eq!(
            reconcile_process_rss_poll(Ok(Some(42)), None::<&str>).unwrap(),
            ProcessRssPoll::Sample(42)
        );
        assert_eq!(
            reconcile_process_rss_poll(Ok(None), None::<&str>).unwrap(),
            ProcessRssPoll::ExitPending
        );
        let terminal_error =
            reconcile_process_rss_poll(Err(anyhow::anyhow!("terminal race")), Some("exited"))
                .unwrap_err();
        assert_eq!(terminal_error.to_string(), "terminal race");
        let live_error =
            reconcile_process_rss_poll::<&str>(Err(anyhow::anyhow!("malformed live status")), None)
                .unwrap_err();
        assert_eq!(live_error.to_string(), "malformed live status");
    }

    #[test]
    fn missing_terminal_rss_requires_bounded_confirmed_exit() {
        let start = Instant::now();
        let mut statuses = std::collections::VecDeque::from([None, None, Some("exited")]);
        let now = std::cell::Cell::new(start);
        let pauses = std::cell::Cell::new(0_u64);
        assert_eq!(
            confirm_process_exit_after_rss_disappeared(
                || Ok(statuses.pop_front().flatten()),
                || {
                    pauses.set(pauses.get() + 1);
                    now.set(now.get() + Duration::from_millis(100));
                },
                || now.get(),
                start + Duration::from_secs(10),
                Duration::from_secs(1),
            )
            .unwrap(),
            TerminalRssExitConfirmation::Exited("exited")
        );
        assert_eq!(pauses.get(), 2);

        let polls = std::cell::Cell::new(0_u64);
        let pauses = std::cell::Cell::new(0_u64);
        let now = std::cell::Cell::new(start);
        let live_outcome = confirm_process_exit_after_rss_disappeared::<&str>(
            || {
                polls.set(polls.get() + 1);
                Ok(None)
            },
            || {
                pauses.set(pauses.get() + 1);
                now.set(now.get() + Duration::from_millis(500));
            },
            || now.get(),
            start + Duration::from_secs(10),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(live_outcome, TerminalRssExitConfirmation::RssGraceExpired);
        assert_eq!(polls.get(), 2);
        assert_eq!(pauses.get(), 2);

        let mut statuses = std::collections::VecDeque::from([None, Some("too late")]);
        let now = std::cell::Cell::new(start);
        assert_eq!(
            confirm_process_exit_after_rss_disappeared(
                || Ok(statuses.pop_front().flatten()),
                || now.set(now.get() + Duration::from_secs(1)),
                || now.get(),
                start + Duration::from_secs(10),
                Duration::from_secs(1),
            )
            .unwrap(),
            TerminalRssExitConfirmation::RssGraceExpired
        );
        assert_eq!(statuses.len(), 1);

        let polls = std::cell::Cell::new(0_u64);
        assert_eq!(
            confirm_process_exit_after_rss_disappeared(
                || {
                    polls.set(polls.get() + 1);
                    Ok(Some("too late"))
                },
                || {},
                || start,
                start,
                Duration::from_secs(1),
            )
            .unwrap(),
            TerminalRssExitConfirmation::ProcessDeadlineReached
        );
        assert_eq!(polls.get(), 0);

        let now = std::cell::Cell::new(start);
        assert_eq!(
            confirm_process_exit_after_rss_disappeared::<&str>(
                || Ok(None),
                || now.set(now.get() + Duration::from_millis(250)),
                || now.get(),
                start + Duration::from_millis(250),
                Duration::from_secs(1),
            )
            .unwrap(),
            TerminalRssExitConfirmation::ProcessDeadlineReached
        );

        let poll_error = confirm_process_exit_after_rss_disappeared::<&str>(
            || Err(anyhow::anyhow!("wait failed")),
            || unreachable!("a failed exit poll must not pause"),
            || start,
            start + Duration::from_secs(10),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(poll_error.to_string(), "wait failed");
    }

    #[test]
    fn t5_validator_binds_publication_transfer_to_observed_storage_execution() {
        let transfer = json!({
            "publication_currentness_execution": {
                "contract_id": PUBLICATION_CURRENTNESS_CONTRACT_ID,
                "expected_snapshot_object_reads": 13,
                "first_inventory_object_reads": 19,
                "observed_snapshot_object_reads": 13,
                "second_inventory_object_reads": 19,
                "observed_total_object_reads": 51,
                "observed_codec_decode_calls": 0,
            },
        });
        validate_publication_currentness_execution(&transfer).unwrap();

        let mut wrong_contract = transfer.clone();
        wrong_contract["publication_currentness_execution"]["contract_id"] = json!("unknown");
        assert!(validate_publication_currentness_execution(&wrong_contract).is_err());

        let mut extra_decode = transfer;
        extra_decode["publication_currentness_execution"]["observed_codec_decode_calls"] = json!(1);
        assert!(validate_publication_currentness_execution(&extra_decode).is_err());
    }

    #[test]
    fn xwininfo_parser_requires_a_viewable_exact_client() {
        let output = "Width: 1280\nHeight: 720\nMap State: IsViewable\n";
        assert_eq!(parse_xwininfo_geometry(output), Some((1280, 720, true)));
        assert_eq!(parse_xwininfo_geometry("Width: 1\n"), None);
    }

    #[test]
    fn source_inventory_streams_sorted_bytes_and_rejects_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let cancelled = AtomicBool::new(false);
        fs::write(root.path().join("b.tif"), b"b").unwrap();
        fs::write(root.path().join("a.tif"), b"a").unwrap();
        let before = source_inventory(root.path(), &cancelled).unwrap();
        assert_eq!(before.regular_files, 2);
        assert_eq!(before.source_bytes, 2);
        assert_eq!(source_inventory(root.path(), &cancelled).unwrap(), before);

        std::os::unix::fs::symlink(root.path().join("a.tif"), root.path().join("link.tif"))
            .unwrap();
        assert!(source_inventory(root.path(), &cancelled).is_err());
    }

    #[test]
    fn private_evidence_creation_removes_group_and_world_access() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("session");
        create_private_directory(&directory).unwrap();
        let report = directory.join("raw.json");
        write_new_synced_json(&report, &json!({"private": true})).unwrap();

        assert_eq!(
            fs::metadata(directory).unwrap().permissions().mode() & 0o077,
            0
        );
        assert_eq!(
            fs::metadata(report).unwrap().permissions().mode() & 0o077,
            0
        );
    }

    #[test]
    fn oracle_scratch_removes_only_its_unchanged_empty_directory() {
        let root = tempfile::tempdir().unwrap();
        let empty = root.path().join("empty");
        let guard = OwnedOracleScratch::create(empty.clone()).unwrap();
        assert!(guard.remove_and_report_empty().unwrap());
        assert!(!empty.exists());

        let remnants = root.path().join("remnants");
        let guard = OwnedOracleScratch::create(remnants.clone()).unwrap();
        fs::write(remnants.join("unexpected"), b"retain for inspection").unwrap();
        assert!(!guard.remove_and_report_empty().unwrap());
        assert!(remnants.join("unexpected").is_file());

        let replaced = root.path().join("replaced");
        let original = root.path().join("original-moved");
        let guard = OwnedOracleScratch::create(replaced.clone()).unwrap();
        fs::rename(&replaced, &original).unwrap();
        fs::create_dir(&replaced).unwrap();
        assert!(!guard.remove_and_report_empty().unwrap());
        assert!(replaced.is_dir());
        assert!(original.is_dir());
    }

    #[test]
    fn cooperative_cancellation_preserves_source_and_removes_empty_oracle_scratch() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.tif");
        let source_bytes = b"immutable-source";
        fs::write(&source, source_bytes).unwrap();
        let scratch_path = root.path().join("oracle-scratch");
        let scratch = OwnedOracleScratch::create(scratch_path.clone()).unwrap();
        let cancelled = AtomicBool::new(true);

        assert!(source_inventory(&source, &cancelled).is_err());
        assert!(scratch.remove_and_report_empty().unwrap());
        assert_eq!(fs::read(source).unwrap(), source_bytes);
        assert!(!scratch_path.exists());
    }

    #[test]
    fn external_cargo_configuration_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        reject_cargo_config_at(root.path()).unwrap();
        fs::write(root.path().join("config.toml"), "[build]\nrustflags=[]\n").unwrap();
        assert!(reject_cargo_config_at(root.path()).is_err());
    }

    #[test]
    fn destination_and_checkpoint_match_the_normal_app_policy() {
        let destination = deterministic_tiff_destination(
            &TiffSource::single_3d("/private/My Cells.ome.tif"),
            Path::new("/scratch/sample"),
        );
        assert_eq!(destination, Path::new("/scratch/sample/my-cells-ome.m4d"));
        assert_eq!(
            checkpoint_directory(&destination).unwrap(),
            Path::new("/scratch/sample/.my-cells-ome.m4d.import-checkpoint")
        );
    }

    #[test]
    fn successful_receipt_must_match_the_accepted_review_token_and_destination() {
        let destination = Path::new("/scratch/sample/source.m4d");
        let source_fingerprint = "4".repeat(64);
        let source_bytes = 123;
        let token = json!({
            "operation_id": 11,
            "task_id": 12,
            "kind": "Import",
            "source_session_generation": 13,
            "currentness_generation": 14,
        });
        let receipt = json!({
            "review_id": 10,
            "operation_token": token,
            "destination": destination,
            "reviewed_source_fingerprint_sha256": source_fingerprint,
            "reviewed_source_bytes": source_bytes,
            "published_event": {
                "published_at_epoch_ms": 20,
                "process_cpu_time_ns": 30,
            },
        });
        let report = json!({
            "import_workflow_evidence": {
                "publication_to_open_ready_clock": {
                    "status": IMPORT_OPEN_READY_COMPLETE_STATUS,
                    "published_at_epoch_ms": 20,
                },
            },
            "events": [
                {
                    "command": "start_reviewed_import",
                    "status": "passed",
                    "details": {
                        "review_id": 10,
                        "operation_token": token,
                        "destination": destination,
                        "reviewed_source_fingerprint_sha256": source_fingerprint,
                        "reviewed_source_bytes": source_bytes,
                    },
                },
                {
                    "command": "wait_for_imported_open_ready",
                    "status": "passed",
                    "details": { "path": destination },
                }
            ],
        });
        let open_ready = successful_publication_to_open_ready_clock(&report).unwrap();
        assert_eq!(
            validate_successful_receipt_binding(&report, open_ready, &receipt, destination)
                .unwrap(),
            (source_fingerprint.clone(), source_bytes),
        );

        let mut unavailable = report.clone();
        unavailable["import_workflow_evidence"]["publication_to_open_ready_clock"] = json!({
            "status": "open_ready_deadline_failed_before_transfer",
        });
        assert!(successful_publication_to_open_ready_clock(&unavailable).is_err());

        let mut stale = receipt.clone();
        stale["operation_token"]["task_id"] = json!(99);
        assert!(
            validate_successful_receipt_binding(&report, open_ready, &stale, destination).is_err()
        );
    }

    #[test]
    fn sanitized_report_validator_rejects_paths_identities_and_digests() {
        validate_sanitized_report(&json!({"status": "diagnostic"}), &[]).unwrap();
        validate_sanitized_report(&json!({"app_executable_sha256": "0".repeat(64)}), &[]).unwrap();
        validate_sanitized_report(
            &json!({
                "samples": [{"gates": {"canonical_base_pixel_bytes": true}}],
                "oracle_gates": {"canonical_base_pixel_bytes": false},
            }),
            &[],
        )
        .unwrap();
        assert!(validate_sanitized_report(&json!({"value": "/private/source"}), &[]).is_err());
        assert!(
            validate_sanitized_report(
                &json!({"value": format!("m4d-sc-v1-sha256:{}", "0".repeat(64))}),
                &[],
            )
            .is_err()
        );
        assert!(
            validate_sanitized_report(
                &json!({"value": "prefix-private-name-suffix"}),
                &["private-name".to_owned()],
            )
            .is_err()
        );
        assert!(
            validate_sanitized_report(&json!({"value": "0".repeat(64)}), &["0".repeat(64)],)
                .is_err()
        );
        for private_numeric_key in [
            "scale_zyx",
            "translation_zyx",
            "logical_voxels",
            "brick_reads",
            "canonical_base_pixel_bytes",
            "source_planes",
            "source_files",
            "source_inventory_name_bytes",
            "required_scratch_bytes",
            "scratch_free_bytes",
        ] {
            let mut report = serde_json::Map::new();
            report.insert(private_numeric_key.to_owned(), json!(1));
            assert!(
                validate_sanitized_report(&Value::Object(report), &[]).is_err(),
                "{private_numeric_key} must remain private",
            );
        }
    }
}

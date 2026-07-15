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
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use mirante4d_identity::{ScientificContentId, Sha256Digest, Sha256Hasher};
use mirante4d_storage::{
    LocalPackageCatalog, PackedIndexCoordinates, ProfileKind, ProfileValidityMode, ShardProfileKind,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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
    process::cargo_command,
    target_fixture::extract_target_u16_fixture,
};

const CONFIG_SCHEMA: &str = "mirante4d-private-import-performance-t5-1";
const RAW_REPORT_SCHEMA: &str = "mirante4d-private-import-performance-t5-raw-3";
const SANITIZED_REPORT_SCHEMA: &str = "mirante4d-import-performance-t5-summary-3";
const PRODUCT_AUTOMATION_REPORT_SCHEMA: &str = "mirante4d-product-automation-report";
const PRODUCT_AUTOMATION_SCHEMA_VERSION: u64 = 3;
const PUBLICATION_CURRENTNESS_CONTRACT_ID: &str =
    "mirante4d-publication-currentness-inventory-snapshot-inventory-1";
const SCALE_DIGEST_SCHEME: &str = "mirante4d-t5-canonical-scale-voxels-1";
const CONFIG_BYTES_MAX: u64 = 1024 * 1024;
const WORKING_MEMORY_BYTES: u64 = 256 * 1024 * 1024;
const RSS_DELTA_BYTES_MAX: u64 = 384 * 1024 * 1024;
const PRIMARY_MEDIAN_NS_MAX: u64 = 15 * 60 * 1_000_000_000;
const PRIMARY_MEDIAN_STRETCH_NS: u64 = 10 * 60 * 1_000_000_000;
const DEFAULT_SAMPLES: usize = 3;
const RSS_SAMPLE_INTERVAL: Duration = Duration::from_millis(10);
const INSPECTION_TIMEOUT_SECONDS: u64 = 30 * 60;
const POST_PRIMARY_TIMEOUT_SECONDS: u64 = 10 * 60;
const MAPPED_WIDTH: u32 = 1280;
const MAPPED_HEIGHT: u32 = 720;
const SOURCE_ENTRY_MAX: usize = 4_096;

/// Private facts cannot bless themselves. The owner must review one exact
/// external configuration and pin its opaque digest in a repository change.
pub(crate) const OWNER_ACCEPTED_T5_CONFIG_SHA256: Option<&str> = None;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunArgs {
    config: PathBuf,
    samples: usize,
    diagnostic: bool,
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
    no_data_sentinel: Option<u8>,
    working_memory_bytes: u64,
    primary_timeout_seconds: u64,
    cache_condition: String,
    competing_activity: String,
    expected: ExpectedFacts,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExpectedFacts {
    source_inventory_sha256: String,
    reviewed_source_fingerprint_sha256: String,
    canonical_source_pixel_bytes: u64,
    scientific_content_id: String,
    scientific_layer_roots: Vec<ExpectedLayerRoot>,
    scale_digest_scheme: String,
    scales: Vec<ExpectedScaleFact>,
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
    reviewed_source_fingerprint_sha256: String,
    failed_gates: Vec<String>,
    all_gates_passed: bool,
    raw: Value,
    sanitized: Value,
}

#[derive(Debug)]
struct IndependentValidation {
    package_id: String,
    scientific_content_id: String,
    layer_roots: Vec<Value>,
    scale_facts: Vec<Value>,
    scale_facts_match: bool,
    layer_roots_match: bool,
    scientific_id_matches: bool,
    scientific_brick_reads: u64,
    canonical_base_pixel_bytes: u64,
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
            "T5 qualification is not owner-bound; rerun with --diagnostic only to collect private candidate evidence for owner review"
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

    let source_before = source_inventory(&source)?;
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
    let mut latest_source_inventory = None;
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
        )?);
        if sha256_file(&executable)? != executable_digest_start
            || !same_file_metadata(&executable_metadata_start, &fs::metadata(&executable)?)
        {
            bail!("release app executable changed during the T5 sample set");
        }
        let current_source_inventory = source_inventory(&source)?;
        if current_source_inventory != source_before {
            bail!("private T5 source changed between independent samples");
        }
        latest_source_inventory = Some(current_source_inventory);
    }

    let source_after =
        latest_source_inventory.context("T5 sample protocol captured no final source inventory")?;
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
    let deterministic_ids = samples.windows(2).all(|pair| {
        pair[0].package_id == pair[1].package_id
            && pair[0].scientific_content_id == pair[1].scientific_content_id
    });
    let samples_passed = samples.iter().all(|sample| sample.all_gates_passed);
    let absolute_gate_passed =
        args.samples == DEFAULT_SAMPLES && median_primary_wall_time_ns <= PRIMARY_MEDIAN_NS_MAX;
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
        && build_reason_codes_end.is_empty()
        && deterministic_ids;
    let qualification_passed = qualification_claim_passed(
        args.diagnostic,
        binding_eligible,
        samples_passed,
        absolute_gate_passed,
        invariant_gates_passed,
    );
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
        absolute_gate_passed,
        frozen_source_matched,
        source_preserved,
        repository_unchanged,
        hardware_unchanged,
        xtask_unchanged,
        executable_unchanged,
        config_unchanged,
        deterministic_ids,
    );

    let raw_report = json!({
        "schema": RAW_REPORT_SCHEMA,
        "schema_version": 2,
        "session_id": session_id,
        "configuration": {
            "path": config_input.path,
            "sha256": config_input.sha256,
            "owner_accepted_sha256": OWNER_ACCEPTED_T5_CONFIG_SHA256,
            "binding_status": config_binding_status,
            "parsed": config,
        },
        "protocol": {
            "sample_count": args.samples,
            "diagnostic_requested": args.diagnostic,
            "fresh_release_process_per_sample": true,
            "fresh_checkpoint_and_absent_destination_per_sample": true,
            "normal_product_import_route": true,
            "primary_clock": "worker_spawn_origin_through_product_verified_open_ready",
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
        "samples": samples.iter().map(|sample| sample.raw.clone()).collect::<Vec<_>>(),
        "summary": {
            "median_primary_wall_time_ns": median_primary_wall_time_ns,
            "absolute_gate_ns": PRIMARY_MEDIAN_NS_MAX,
            "stretch_target_ns": PRIMARY_MEDIAN_STRETCH_NS,
            "absolute_gate_passed": absolute_gate_passed,
            "samples_passed": samples_passed,
            "deterministic_package_and_scientific_ids": deterministic_ids,
            "frozen_source_matched": frozen_source_matched,
            "source_preserved": source_preserved,
            "config_unchanged": config_unchanged,
            "binding_eligible": binding_eligible,
            "diagnostic_reason_codes": diagnostic_reason_codes,
            "qualification_passed": qualification_passed,
        },
    });
    let raw_report_path = session_root.join("raw-private-report.json");
    write_new_synced_json(&raw_report_path, &raw_report)?;
    let raw_report_sha256 = sha256_file(&raw_report_path)?;

    let filesystem_class = scratch_binding_start
        .filesystem_type
        .clone()
        .unwrap_or_else(|| "unavailable".to_owned());
    let mut remaining_risks = vec![
        "no_relative_speedup_or_timing_tail_claim",
        "cache_and_competing_activity_are_declared_not_os_enforced",
        "physical_display_attachment_is_owner_attested_not_cryptographically_proved",
        "retained_private_configuration_commitment_can_confirm_a_guessed_configuration",
    ];
    if median_primary_wall_time_ns > PRIMARY_MEDIAN_STRETCH_NS {
        remaining_risks.push("nonblocking_10_minute_stretch_target_not_met");
    }
    let sanitized_report = json!({
        "schema": SANITIZED_REPORT_SCHEMA,
        "schema_version": 2,
        "evidence_id": session_id,
        "workload_id": config.workload_id,
        "evidence_class": if qualification_passed { "qualification" } else { "diagnostic" },
        "status": if qualification_passed { "passed" } else { "not_qualified" },
        "protocol": {
            "sample_count": args.samples,
            "diagnostic_requested": args.diagnostic,
            "fresh_release_processes": true,
            "normal_product_import_route": true,
            "real_external_mapped_window_required": true,
            "physical_display_attachment_owner_attested": true,
            "external_display_proof_scope": "mapped_native_x11_client_geometry",
            "working_memory_bytes": WORKING_MEMORY_BYTES,
            "cache_condition": config.cache_condition,
            "competing_activity": config.competing_activity,
            "required_hardware_class": IMPORT_QUALIFICATION_HARDWARE_CLASS,
            "qualified_hardware_class": if profile_matched_at_start && profile_matched_at_end { Some(IMPORT_QUALIFICATION_HARDWARE_CLASS) } else { None },
            "filesystem_class": filesystem_class,
        },
        "bindings": {
            "private_configuration": config_binding_status,
            "private_configuration_sha256": config_input.sha256,
            "private_raw_report_sha256": raw_report_sha256,
            "host_and_storage_profile": if profile_matched_at_start && profile_matched_at_end { "matched" } else { "not_matched" },
            "qualification_profile_sha256": source_binding_start.profile_sha256,
            "observed_host_fingerprint_sha256": source_binding_start.host_fingerprint_sha256,
            "observed_storage_fingerprint_sha256": source_binding_start.storage_fingerprint_sha256,
            "configuration_unchanged": config_unchanged,
            "repository_unchanged": repository_unchanged,
            "xtask_executable_unchanged": xtask_unchanged,
            "release_executable_unchanged": executable_unchanged,
        },
        "build": {
            "repository_revision": repository_start.commit,
            "xtask_executable_sha256": xtask_digest_start,
            "app_executable_sha256": executable_digest_start,
            "app_build_target_mode": "fresh-private-target",
            "xtask": qualification_build_provenance_evidence(&build_provenance),
            "toolchain": build_provenance.compiler,
        },
        "commands": [
            if args.diagnostic {
                "cargo run --release -p xtask -- import-performance-t5 --config <private-config> --diagnostic"
            } else {
                "cargo run --release -p xtask -- import-performance-t5 --config <private-config>"
            },
            "normal native app setup/review/start/open-ready/render/navigation route",
        ],
        "samples": samples.iter().map(|sample| sample.sanitized.clone()).collect::<Vec<_>>(),
        "gates": {
            "median_primary_wall_time_ns": median_primary_wall_time_ns,
            "median_at_most_15_minutes": absolute_gate_passed,
            "median_stretch_at_most_10_minutes": median_primary_wall_time_ns <= PRIMARY_MEDIAN_STRETCH_NS,
            "all_per_sample_gates": samples_passed,
            "deterministic_output": deterministic_ids,
            "frozen_source_matched": frozen_source_matched,
            "source_preserved": source_preserved,
            "all_invariants": invariant_gates_passed,
            "qualification_passed": qualification_passed,
        },
        "failures": diagnostic_reason_codes,
        "skips": [],
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
    });
    let mut private_strings = vec![
        source.display().to_string(),
        scratch_root.display().to_string(),
        qualification_profile.display().to_string(),
        config_input.path.display().to_string(),
        repository_root.display().to_string(),
        source_before.sha256.clone(),
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
    for sample in &samples {
        private_strings.push(sample.package_id.clone());
        private_strings.push(sample.scientific_content_id.clone());
        private_strings.push(sample.reviewed_source_fingerprint_sha256.clone());
        for pointer in [
            "/independent_validation/layer_roots",
            "/independent_validation/scales",
        ] {
            if let Some(facts) = sample.raw.pointer(pointer).and_then(Value::as_array) {
                private_strings.extend(facts.iter().filter_map(|fact| {
                    fact.get("digest_sha256")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                }));
            }
        }
    }
    validate_sanitized_report(&sanitized_report, &private_strings)?;
    let sanitized_root = repository_root.join("target/mirante4d/import-performance-t5");
    fs::create_dir_all(&sanitized_root)?;
    let sanitized_path = sanitized_root.join(format!("{session_id}-summary.json"));
    write_new_synced_json(&sanitized_path, &sanitized_report)?;
    Ok(sanitized_path)
}

fn parse_args(args: Vec<String>) -> anyhow::Result<RunArgs> {
    let mut config = None;
    let mut samples = DEFAULT_SAMPLES;
    let mut diagnostic = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--config" => {
                index += 1;
                config = Some(PathBuf::from(
                    args.get(index)
                        .context("--config requires an absolute path")?,
                ));
            }
            "--samples" => {
                index += 1;
                samples = args
                    .get(index)
                    .context("--samples requires a value")?
                    .parse::<usize>()
                    .context("--samples must be an integer")?;
            }
            "--diagnostic" => diagnostic = true,
            "--help" | "-h" | "help" => bail!(
                "usage: cargo run --release -p xtask -- import-performance-t5 --config /absolute/private/config.json [--diagnostic] [--samples 3]"
            ),
            option => bail!("unknown import-performance-t5 option {option:?}"),
        }
        index += 1;
    }
    let config = config.context("import-performance-t5 requires --config PATH")?;
    if samples == 0 || samples > 9 {
        bail!("T5 sample count must be between one and nine");
    }
    if samples != DEFAULT_SAMPLES && !diagnostic {
        bail!("qualification evidence requires exactly three independent process sessions");
    }
    Ok(RunArgs {
        config,
        samples,
        diagnostic,
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
    if config.expected_profile != "DS-3" {
        bail!("private T5 qualification is frozen to expected_profile DS-3");
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

fn require_standard_app_build_environment(
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

fn require_no_external_cargo_configuration(repository_root: &Path) -> anyhow::Result<()> {
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

const fn qualification_claim_passed(
    diagnostic: bool,
    binding_eligible: bool,
    samples_passed: bool,
    absolute_gate_passed: bool,
    invariant_gates_passed: bool,
) -> bool {
    !diagnostic
        && binding_eligible
        && samples_passed
        && absolute_gate_passed
        && invariant_gates_passed
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
    absolute_gate_passed: bool,
    frozen_source_matched: bool,
    source_preserved: bool,
    repository_unchanged: bool,
    hardware_unchanged: bool,
    xtask_unchanged: bool,
    executable_unchanged: bool,
    config_unchanged: bool,
    deterministic_ids: bool,
) -> Vec<String> {
    let mut reasons = BTreeSet::new();
    if args.diagnostic {
        reasons.insert("diagnostic_mode_explicitly_requested".to_owned());
    }
    if args.samples != DEFAULT_SAMPLES {
        reasons.insert("sample_count_not_three".to_owned());
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
    for (passed, reason) in [
        (absolute_gate_passed, "absolute_time_gate_failed"),
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
        (deterministic_ids, "sample_identities_not_deterministic"),
    ] {
        if !passed {
            reasons.insert(reason.to_owned());
        }
    }
    reasons.into_iter().collect()
}

fn source_inventory(root: &Path) -> anyhow::Result<InventoryFacts> {
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
        for entry in fs::read_dir(&directory)? {
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
) -> anyhow::Result<SampleEvidence> {
    let sample_root = session_root.join(format!("sample-{sample_index:02}"));
    create_private_directory(&sample_root)?;
    let output_parent = sample_root.join("output");
    create_private_directory(&output_parent)?;
    let destination = tiff_destination(source, &output_parent);
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
    write_new_synced_json(&script_path, &script)?;

    let observation = run_app_process(AppRun {
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
    })?;
    if !observation.exit_success {
        bail!("T5 app process did not exit successfully");
    }
    let mapped_window = observation
        .mapped_window
        .context("T5 app never exposed the exact externally observed mapped client area")?;
    let automation_report = read_bounded_json(&automation_report_path, 32 * 1024 * 1024)?;
    if automation_report.get("schema").and_then(Value::as_str)
        != Some(PRODUCT_AUTOMATION_REPORT_SCHEMA)
        || automation_report
            .get("schema_version")
            .and_then(Value::as_u64)
            != Some(PRODUCT_AUTOMATION_SCHEMA_VERSION)
        || automation_report.get("status").and_then(Value::as_str) != Some("passed")
    {
        bail!("T5 product automation report did not pass");
    }
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
    let publication_to_open_ready = automation_report
        .pointer("/import_workflow_evidence/publication_to_open_ready_clock")
        .context("T5 automation report lacks publication-to-open-ready clock evidence")?;
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
            != Some("published_destination_verified_and_open_ready_for_normal_product_use")
    {
        bail!("T5 app primary-clock boundaries drifted");
    }
    validate_publication_currentness_execution(publication_to_open_ready)?;
    for field in [
        "source_verification_started_runs",
        "source_verification_progress_updates",
        "source_verification_cancelled_runs",
        "source_verification_failed_runs",
        "source_verification_successes",
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
            != Some("published_destination_verified_and_open_ready_for_normal_product_use")
        || publication_to_open_ready
            .get("included_in_primary_clock")
            .and_then(Value::as_bool)
            != Some(true)
        || publication_to_open_ready
            .get("transfer_mode")
            .and_then(Value::as_str)
            != Some("staged_verified_capability")
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
        validate_successful_receipt_binding(&automation_report, receipt, &destination)?;
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

    let independent = independently_validate_package(&destination, &config.expected)?;
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
    let scientific_identity_native_decoded_bytes =
        required_u64(statistics, "scientific_identity_native_decoded_bytes")?;
    let sync_calls = required_u64(statistics, "sync_calls")?;
    let scientific_brick_reads = required_u64(statistics, "scientific_brick_reads")?;
    let peak_open_file_descriptors = required_u64(statistics, "peak_open_file_descriptors")?;
    let peak_temporary_bytes = required_u64(statistics, "peak_temporary_bytes")?;
    let preflight_temporary_bytes_bound =
        required_u64(statistics, "preflight_temporary_bytes_bound")?;
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
        scientific_identity_native_decoded_bytes == 0,
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
        "complete_source_revalidation".to_owned(),
        source_revalidation_bytes_read == reviewed_source_bytes,
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
        "temporary_byte_bound".to_owned(),
        preflight_temporary_bytes_bound > 0
            && peak_temporary_bytes <= preflight_temporary_bytes_bound,
    );
    gates.insert(
        "temporary_object_bound".to_owned(),
        peak_checkpoint_regular_files <= 8,
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
            "layer_roots_match": independent.layer_roots_match,
            "scale_facts_match": independent.scale_facts_match,
            "scientific_id_matches": independent.scientific_id_matches,
            "scientific_brick_reads": independent.scientific_brick_reads,
            "canonical_base_pixel_bytes": independent.canonical_base_pixel_bytes,
            "canonical_base_pixel_bytes_match": independent.canonical_base_pixel_bytes
                == config.expected.canonical_source_pixel_bytes,
        },
        "automation_report": automation_report,
        "gates": gates,
        "all_gates_passed": all_gates_passed,
    });
    let sanitized = json!({
        "sample_index": sample_index,
        "inspection_and_review_wall_time_ns": inspection_and_review_wall_time_ns,
        "inspection_and_review_process_cpu_time_ns": inspection_and_review_cpu_time_ns,
        "primary_wall_time_ns": primary_wall_time_ns,
        "primary_process_cpu_time_ns": primary_cpu_time_ns,
        "publication_to_open_ready_wall_time_ns": publication_to_open_ready_wall_time_ns,
        "publication_to_open_ready_process_cpu_time_ns": publication_to_open_ready_cpu_time_ns,
        "external_rss_delta_bytes": external_rss_delta_bytes,
        "external_rss_delta_minus_ledger_peak_bytes": rss_minus_ledger,
        "peak_working_bytes": peak_working_bytes,
        "peak_open_file_descriptors": peak_open_file_descriptors,
        "peak_temporary_bytes": peak_temporary_bytes,
        "peak_checkpoint_regular_files": peak_checkpoint_regular_files,
        "sync_calls": sync_calls,
        "gates": gates,
        "all_gates_passed": all_gates_passed,
    });
    Ok(SampleEvidence {
        sample_index,
        primary_wall_time_ns,
        package_id: receipt_package_id,
        scientific_content_id: receipt_scientific_id,
        reviewed_source_fingerprint_sha256,
        failed_gates,
        all_gates_passed,
        raw,
        sanitized,
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
    Ok(json!({
        "schema": "mirante4d-product-automation-script",
        "schema_version": PRODUCT_AUTOMATION_SCHEMA_VERSION,
        "scenario": "import_performance_t5_private",
        "limits": {
            "max_cpu_total_bytes": 1024_u64 * 1024 * 1024,
            "max_cpu_import_working_set_bytes": WORKING_MEMORY_BYTES,
            "max_runtime_queued_requests": 192,
        },
        "commands": [
            { "command": "open_dataset", "path": startup_package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 10_000 },
            { "command": "set_mapped_client_pixels", "width": MAPPED_WIDTH, "height": MAPPED_HEIGHT },
            { "command": "set_render_target_size", "width": MAPPED_WIDTH, "height": MAPPED_HEIGHT },
            { "command": "wait_for", "condition": "source_verification_verified", "timeout_ms": 60_000 },
            { "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 60_000 },
            { "command": "begin_tiff_import_setup", "source": source, "output_parent": output_parent },
            { "command": "wait_for", "condition": "import_review_ready", "timeout_ms": inspection_timeout_ms },
            { "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 60_000 },
            {
                "command": "start_reviewed_import",
                "spacing_zyx_um": config.spacing_zyx_um,
                "time_step_seconds": config.time_step_seconds,
                "no_data_sentinel": config.no_data_sentinel,
                "working_memory_bytes": WORKING_MEMORY_BYTES
            },
            { "command": "wait_for_imported_open_ready", "path": destination, "timeout_ms": primary_timeout_ms },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 120_000 },
            { "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 120_000 },
            { "command": "camera_fit_data" },
            { "command": "camera_orbit", "yaw_points": 48.0, "pitch_points": 16.0 },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 120_000 },
            { "command": "assert", "condition": "nonblank_frame" },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "name": "t5-open-ready-navigation" },
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
}

struct ProcessObservation {
    exit_success: bool,
    rss_samples: Vec<RssSample>,
    mapped_window: Option<Value>,
}

fn run_app_process(run: AppRun<'_>) -> anyhow::Result<ProcessObservation> {
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
    let mut child = Command::new(run.executable)
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
        .stderr(Stdio::from(stderr))
        .spawn()
        .context("failed to spawn release Mirante4D app for T5")?;
    let pid = child.id();
    let deadline = Instant::now() + run.timeout;
    let mut rss_samples = Vec::new();
    let mut mapped_window = None;
    let mut next_geometry_probe = Instant::now();
    loop {
        if let Some(rss_bytes) = read_process_rss_bytes(pid)? {
            rss_samples.push(RssSample {
                epoch_ms: epoch_ms(),
                rss_bytes,
            });
        }
        if mapped_window.is_none() && Instant::now() >= next_geometry_probe {
            mapped_window = probe_x11_client_geometry(pid, MAPPED_WIDTH, MAPPED_HEIGHT)?;
            next_geometry_probe = Instant::now() + Duration::from_millis(100);
        }
        if let Some(status) = child.try_wait()? {
            return Ok(ProcessObservation {
                exit_success: status.success(),
                rss_samples,
                mapped_window,
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("T5 app process exceeded its declared timeout");
        }
        thread::sleep(RSS_SAMPLE_INTERVAL);
    }
}

fn read_process_rss_bytes(pid: u32) -> anyhow::Result<Option<u64>> {
    let path = PathBuf::from(format!("/proc/{pid}/status"));
    let status = match fs::read_to_string(&path) {
        Ok(status) => status,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let Some(kib) = status.lines().find_map(|line| {
        let value = line.strip_prefix("VmRSS:")?;
        value.split_ascii_whitespace().next()?.parse::<u64>().ok()
    }) else {
        bail!("Linux process status omitted VmRSS during T5 sampling");
    };
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
    let search = Command::new("xdotool")
        .args(["search", "--onlyvisible", "--pid", &pid.to_string()])
        .output()
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
        let info = Command::new("xwininfo")
            .args(["-id", &id_hex])
            .output()
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
    let open_ready_clock = report
        .pointer("/import_workflow_evidence/publication_to_open_ready_clock")
        .context("T5 successful receipt lacks its publication-to-open-ready clock")?;
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
        "base-production".to_owned(),
        "source-scientific-identity".to_owned(),
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
        "base-production".to_owned(),
        "source-scientific-identity".to_owned(),
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
    let mut required_projected = BTreeSet::from(["base-production".to_owned()]);
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
    let decoded_by_stage = required_u64(statistics, "base_native_decoded_bytes")?
        .checked_add(required_u64(
            statistics,
            "scientific_identity_native_decoded_bytes",
        )?)
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

fn independently_validate_package(
    destination: &Path,
    expected: &ExpectedFacts,
) -> anyhow::Result<IndependentValidation> {
    let catalog = LocalPackageCatalog::open(destination)
        .context("T5 independent post-open package catalog rejected the destination")?;
    let exact = catalog
        .validate_exact_package(ProfileKind::Ds3, || false)
        .context("T5 independent exact package validation failed")?;
    let package_id = exact.package_id().to_string();
    let verified = exact
        .validate_scientific_content(|| false)
        .context("T5 independent scientific package validation failed")?;
    let scientific_content_id = verified.scientific_content_id().to_string();
    let scientific_id_matches = scientific_content_id == expected.scientific_content_id;

    let mut actual_roots = verified
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
    for image in verified.catalog().profile().images() {
        for level in image.levels() {
            let (fact, canonical_pixel_bytes) =
                scale_fact(&verified, image.image_ordinal(), level)?;
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
    let scientific_brick_reads = verified.validation_report().brick_reads();
    verified
        .revalidate_complete(|| false)
        .context("T5 package changed during independent per-scale readback")?;

    Ok(IndependentValidation {
        package_id,
        scientific_content_id,
        layer_roots,
        scale_facts,
        scale_facts_match,
        layer_roots_match,
        scientific_id_matches,
        scientific_brick_reads,
        canonical_base_pixel_bytes,
    })
}

fn scale_fact(
    verified: &mirante4d_storage::VerifiedScientificPackageCapability,
    image_ordinal: u32,
    level: &mirante4d_storage::ProfileLevel,
) -> anyhow::Result<(ExpectedScaleFact, u64)> {
    let metadata_path =
        mirante4d_storage::PackagePath::parse(&format!("{}/zarr.json", level.pixel_path()))?;
    let array = verified
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
                        let coordinates = PackedIndexCoordinates::new(
                            image_ordinal,
                            level.scale_ordinal(),
                            u32::try_from(t)?,
                            u32::try_from(c)?,
                            u32::try_from(z_chunk)?,
                            u32::try_from(y_chunk)?,
                            u32::try_from(x_chunk)?,
                        );
                        let brick = verified
                            .read_brick(coordinates, || false)
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
                            for y in 0..extent[1] {
                                row.clear();
                                for x in 0..extent[2] {
                                    let source_index = linear_3d([z, y, x], brick_shape)?;
                                    let valid = effective_validity(&brick, source_index)?;
                                    row.push(u8::from(valid));
                                    if valid {
                                        if let Some(pixel) = brick.pixel_payload() {
                                            let start = source_index
                                                .checked_mul(sample_bytes)
                                                .context("T5 scale pixel offset overflowed")?;
                                            row.extend_from_slice(
                                                &pixel[start..start + sample_bytes],
                                            );
                                        } else {
                                            row.resize(row.len() + sample_bytes, 0);
                                        }
                                    } else {
                                        row.resize(row.len() + sample_bytes, 0);
                                    }
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

fn tiff_destination(source: &Path, output_parent: &Path) -> PathBuf {
    let name = source
        .file_stem()
        .or_else(|| source.file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("imported-dataset");
    let slug = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() {
        "imported-dataset"
    } else {
        slug
    };
    output_parent.join(format!("{slug}.m4d"))
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
                for value in values.values() {
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

    fn valid_config_json(root: &Path) -> Value {
        json!({
            "schema": CONFIG_SCHEMA,
            "workload_id": "t5-0123456789abcdef0123456789abcdef",
            "source": root.join("source"),
            "scratch_root": root.join("scratch"),
            "qualification_profile": root.join("profile.json"),
            "expected_profile": "DS-3",
            "spacing_zyx_um": [1.0, 1.0, 1.0],
            "time_step_seconds": null,
            "no_data_sentinel": null,
            "working_memory_bytes": WORKING_MEMORY_BYTES,
            "primary_timeout_seconds": 1200,
            "cache_condition": "warm",
            "competing_activity": "none",
            "expected": {
                "source_inventory_sha256": "3".repeat(64),
                "reviewed_source_fingerprint_sha256": "4".repeat(64),
                "canonical_source_pixel_bytes": 16,
                "scientific_content_id": format!("m4d-sc-v1-sha256:{}", "0".repeat(64)),
                "scientific_layer_roots": [{
                    "logical_layer": 0,
                    "digest_sha256": "1".repeat(64),
                }],
                "scale_digest_scheme": SCALE_DIGEST_SCHEME,
                "scales": [{
                    "image_ordinal": 0,
                    "scale_ordinal": 0,
                    "digest_sha256": "2".repeat(64),
                    "brick_reads": 1,
                    "logical_voxels": 16,
                }],
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
        private_protocol_label["competing_activity"] = json!("private-cell-dataset");
        let config: T5Config = serde_json::from_value(private_protocol_label).unwrap();
        assert!(validate_config(&config).is_err());

        let mut extra = valid_config_json(root);
        extra["unexpected"] = json!(true);
        assert!(serde_json::from_value::<T5Config>(extra).is_err());
    }

    #[test]
    fn argument_parser_keeps_three_sessions_as_the_qualification_protocol() {
        let parsed = parse_args(vec!["--config".into(), "/private/config.json".into()]).unwrap();
        assert_eq!(parsed.samples, 3);
        assert!(!parsed.diagnostic);
        assert!(
            parse_args(vec![
                "--config".into(),
                "/private/config.json".into(),
                "--samples".into(),
                "2".into(),
            ])
            .is_err()
        );
        assert_eq!(
            parse_args(vec![
                "--config".into(),
                "/private/config.json".into(),
                "--samples".into(),
                "2".into(),
                "--diagnostic".into(),
            ])
            .unwrap()
            .samples,
            2
        );
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
    fn qualification_claim_requires_every_gate_and_never_accepts_diagnostic_mode() {
        assert!(qualification_claim_passed(false, true, true, true, true));
        assert!(!qualification_claim_passed(true, true, true, true, true));
        assert!(!qualification_claim_passed(false, false, true, true, true));
        assert!(!qualification_claim_passed(false, true, false, true, true));
        assert!(!qualification_claim_passed(false, true, true, false, true));
        assert!(!qualification_claim_passed(false, true, true, true, false));
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
        fs::write(root.path().join("b.tif"), b"b").unwrap();
        fs::write(root.path().join("a.tif"), b"a").unwrap();
        let before = source_inventory(root.path()).unwrap();
        assert_eq!(before.regular_files, 2);
        assert_eq!(before.source_bytes, 2);
        assert_eq!(source_inventory(root.path()).unwrap(), before);

        std::os::unix::fs::symlink(root.path().join("a.tif"), root.path().join("link.tif"))
            .unwrap();
        assert!(source_inventory(root.path()).is_err());
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
    fn external_cargo_configuration_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        reject_cargo_config_at(root.path()).unwrap();
        fs::write(root.path().join("config.toml"), "[build]\nrustflags=[]\n").unwrap();
        assert!(reject_cargo_config_at(root.path()).is_err());
    }

    #[test]
    fn destination_and_checkpoint_match_the_normal_app_policy() {
        let destination = tiff_destination(
            Path::new("/private/My Cells.ome.tif"),
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
        assert_eq!(
            validate_successful_receipt_binding(&report, &receipt, destination).unwrap(),
            (source_fingerprint.clone(), source_bytes),
        );

        let mut stale = receipt.clone();
        stale["operation_token"]["task_id"] = json!(99);
        assert!(validate_successful_receipt_binding(&report, &stale, destination).is_err());
    }

    #[test]
    fn sanitized_report_validator_rejects_paths_identities_and_digests() {
        validate_sanitized_report(&json!({"status": "diagnostic"}), &[]).unwrap();
        validate_sanitized_report(&json!({"app_executable_sha256": "0".repeat(64)}), &[]).unwrap();
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
    }
}

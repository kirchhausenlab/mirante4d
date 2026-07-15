use std::{
    collections::BTreeSet,
    env, fs,
    fs::File,
    io::Read,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use mirante4d_dataset::{CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError};
use mirante4d_domain::{GridToWorld, LogicalLayerKey, Shape4D};
use mirante4d_identity::{
    SCIENTIFIC_TILE_SHAPE_TZYX, ScientificDatasetHasher, ScientificLayerDescriptor,
    ScientificLayerHasher, ScientificTemporalCalibration, ScientificTile, Sha256Hasher,
};
use mirante4d_import_pipeline::{
    ImportCancellation, ImportOptions, ImportStage, ImportStatistics, NoDataPolicy,
    SpatialCalibration, TiffSource, import_tiff, inspect_tiff, select_supported_profile,
};
use mirante4d_storage::{PackedIndexCoordinates, ProfileKind, VerifiedScientificPackageCapability};
use rustix::time::{ClockId, clock_gettime};
use serde_json::{Value, json};
use tiff::encoder::{TiffEncoder, colortype};

use crate::{
    host::{
        IMPORT_QUALIFICATION_PROFILE_MAX_BYTES, IMPORT_QUALIFICATION_PROFILE_SCHEMA,
        ImportQualificationAssessment, ImportQualificationProtocol,
        OWNER_ACCEPTED_IMPORT_QUALIFICATION_PROFILE_SHA256, QualificationBuildProvenance,
        RepositoryIdentity, assess_import_qualification_profile, benchmark_host_context_from,
        host_hardware_identity, import_qualification_assessment_evidence,
        qualification_build_provenance, qualification_build_provenance_evidence,
        qualification_build_reason_codes, repository_identity,
    },
    reports::write_json_file,
};

const EVIDENCE_SCHEMA: &str = "mirante4d-import-performance-evidence-4";
const WORKER_SCHEMA: &str = "mirante4d-import-performance-sample-4";
const PUBLICATION_CURRENTNESS_CONTRACT_ID: &str =
    "mirante4d-publication-currentness-inventory-snapshot-inventory-1";
const T2_Z: u32 = 65;
const T2_Y: u32 = 1_025;
const T2_X: u32 = 2_049;
const T2_SENTINEL: u8 = 255;
const T2_EXPECTED_SCIENTIFIC_CONTENT_ID: &str =
    "m4d-sc-v1-sha256:757c8677c4ba2f28f6f10da63a2d03f891e8879c71b68d926439c2352c19c931";
const T2_EXPECTED_SCALE_FACTS: [([u64; 3], &str); 5] = [
    (
        [65, 1_025, 2_049],
        "f8a70a9ebf4dd44fc09d8a1f9b0407129debe68f56381b7676c641b99a6eb65f",
    ),
    (
        [33, 513, 1_025],
        "d20d89e2ae4c1230371fcbf586e4eb74a94525e70c9964e4907f288e925f55a8",
    ),
    (
        [17, 257, 513],
        "ed7179b972f79d53e676e72e70f90db6448dda7bd3e40943fde8b1a1a46c05a0",
    ),
    (
        [9, 129, 257],
        "fcf4a90832f08546420475b5562d391d52650120651ceb4d7abec14368d751c2",
    ),
    (
        [5, 65, 129],
        "0742532d6e422ab08a394d219399c33117702cb9899e0a3ac0d4084d7169e66b",
    ),
];
const WORKING_MEMORY_BYTES: u64 = 256 * 1024 * 1024;
const RSS_DELTA_BYTES_MAX: u64 = 384 * 1024 * 1024;
const DEFAULT_SAMPLES: usize = 5;
const RSS_SAMPLE_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunArgs {
    samples: usize,
    scratch: PathBuf,
    qualification_profile: Option<PathBuf>,
    cache_condition: String,
    competing_activity: String,
    keep_outputs: bool,
}

pub(crate) fn run(args: Vec<String>) -> anyhow::Result<PathBuf> {
    let args = parse_args(args)?;
    let build_provenance = qualification_build_provenance();
    require_release_build(&build_provenance)?;
    let protocol = ImportQualificationProtocol::new(
        args.cache_condition.clone(),
        args.competing_activity.clone(),
    );
    let repository_start = repository_identity();
    let hardware_start = host_hardware_identity();
    let mut host = benchmark_host_context_from(&repository_start, &hardware_start);
    if let Some(object) = host.as_object_mut() {
        object.remove("name");
        object.remove("cpu_model");
        object.remove("logical_cpu_count");
        object.remove("mem_total_kib");
    }
    let clean_at_start = repository_is_clean(&repository_start);
    if !clean_at_start && env::var_os("MIRANTE4D_IMPORT_PERF_ALLOW_DIRTY").is_none() {
        bail!(
            "import performance evidence requires a clean worktree; set \
             MIRANTE4D_IMPORT_PERF_ALLOW_DIRTY=1 only for explicitly diagnostic runs"
        );
    }

    fs::create_dir_all(&args.scratch)
        .with_context(|| format!("failed to create {}", args.scratch.display()))?;
    let scratch_root = fs::canonicalize(&args.scratch)
        .with_context(|| format!("failed to resolve {}", args.scratch.display()))?;
    let qualification_start = assess_import_qualification_profile(
        args.qualification_profile.as_deref(),
        &repository_start,
        &scratch_root,
        &hardware_start,
        &protocol,
    );
    let executable = env::current_exe().context("failed to resolve the xtask executable")?;
    let executable_digest_start = sha256_file(&executable)?;
    let session_id = session_id()?;
    let session_root = scratch_root.join(&session_id);
    fs::create_dir(&session_root)
        .with_context(|| format!("failed to create {}", session_root.display()))?;
    let session_root = fs::canonicalize(&session_root)
        .with_context(|| format!("failed to resolve {}", session_root.display()))?;
    let qualification_session_start = assess_import_qualification_profile(
        args.qualification_profile.as_deref(),
        &repository_start,
        &session_root,
        &hardware_start,
        &protocol,
    );
    let source = session_root.join("t2-source");
    let generation_started = Instant::now();
    generate_t2_source(&source, T2_Z, T2_Y, T2_X)?;
    let source_preservation_before = source_inventory_facts(&source)?;
    let expected_facts = t2_expected_facts(T2_Z, T2_Y, T2_X)?;
    let generation_wall_time_ns = duration_ns(generation_started.elapsed())?;

    let mut samples = Vec::with_capacity(args.samples);
    for sample_index in 0..args.samples {
        let sample_root = session_root.join(format!("sample-{sample_index:02}"));
        fs::create_dir(&sample_root)
            .with_context(|| format!("failed to create {}", sample_root.display()))?;
        let destination = sample_root.join("t2.m4d");
        let checkpoint = sample_root.join("checkpoint");
        let result = sample_root.join("worker.json");
        let baseline = sample_root.join("rss-baseline");
        let primary_marker = sample_root.join("primary-clock-active");
        let mut command = Command::new(&executable);
        command
            .arg("__import-performance-t2-worker")
            .arg(&source)
            .arg(&destination)
            .arg(&checkpoint)
            .arg(&result)
            .arg(&baseline)
            .arg(&primary_marker)
            .arg(sample_index.to_string())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to launch T2 sample worker from {}",
                executable.display()
            )
        })?;
        let pid = child.id();
        let mut external_peak_rss_bytes = 0_u64;
        let mut primary_rss_sample_count = 0_u64;
        loop {
            if primary_marker.exists()
                && let Some(rss) = linux_process_rss_bytes(pid)
                && rss > 0
            {
                primary_rss_sample_count = primary_rss_sample_count
                    .checked_add(1)
                    .context("T2 primary RSS sample count overflowed")?;
                external_peak_rss_bytes = external_peak_rss_bytes.max(rss);
            }
            if let Some(status) = child.try_wait().context("failed to poll T2 worker")? {
                if !status.success() {
                    bail!("T2 sample {sample_index} failed with {status}");
                }
                break;
            }
            thread::sleep(RSS_SAMPLE_INTERVAL);
        }
        let baseline_rss_bytes = fs::read_to_string(&baseline)
            .with_context(|| format!("failed to read {}", baseline.display()))?
            .trim()
            .parse::<u64>()
            .context("T2 worker RSS baseline is not an integer")?;
        require_t2_rss_observations(baseline_rss_bytes, primary_rss_sample_count)
            .with_context(|| format!("T2 sample {sample_index} RSS evidence is incomplete"))?;
        external_peak_rss_bytes = external_peak_rss_bytes.max(baseline_rss_bytes);
        let mut sample: Value = serde_json::from_slice(
            &fs::read(&result).with_context(|| format!("failed to read {}", result.display()))?,
        )
        .context("T2 worker result is not valid JSON")?;
        if sample.get("schema").and_then(Value::as_str) != Some(WORKER_SCHEMA)
            || sample.get("sample_index").and_then(Value::as_u64)
                != Some(u64::try_from(sample_index)?)
            || sample
                .pointer("/counter_reconciliation/all_passed")
                .and_then(Value::as_bool)
                != Some(true)
            || sample
                .pointer("/runtime_gates/all_passed")
                .and_then(Value::as_bool)
                != Some(true)
        {
            bail!("T2 sample {sample_index} omitted its exact successful worker evidence");
        }
        validate_publication_capability_transfer(
            sample
                .get("publication_capability_transfer")
                .context("T2 worker result omitted publication capability-transfer evidence")?,
        )
        .with_context(|| {
            format!("T2 sample {sample_index} publication capability-transfer evidence failed")
        })?;
        let ledger_peak_bytes = sample
            .pointer("/ledger/peak_bytes")
            .and_then(Value::as_u64)
            .context("T2 worker result omitted its independent ledger peak")?;
        let object = sample
            .as_object_mut()
            .context("T2 worker result must be a JSON object")?;
        let rss_delta_bytes = external_peak_rss_bytes.saturating_sub(baseline_rss_bytes);
        let rss_minus_ledger = i128::from(rss_delta_bytes) - i128::from(ledger_peak_bytes);
        let rss_minus_ledger = i64::try_from(rss_minus_ledger)
            .context("external RSS-to-ledger reconciliation exceeds the evidence schema")?;
        object.insert(
            "external_rss".to_owned(),
            json!({
                "idle_baseline_bytes": baseline_rss_bytes,
                "primary_peak_bytes": external_peak_rss_bytes,
                "delta_bytes": rss_delta_bytes,
                "sample_interval_ms": RSS_SAMPLE_INTERVAL.as_millis(),
                "primary_sample_count": primary_rss_sample_count,
                "gate_max_delta_bytes": RSS_DELTA_BYTES_MAX,
                "gate_passed": rss_delta_bytes <= RSS_DELTA_BYTES_MAX,
                "independent_ledger_peak_bytes": ledger_peak_bytes,
                "external_delta_minus_ledger_peak_bytes": rss_minus_ledger,
                "reconciliation_basis": "signed external primary RSS delta minus independently ledgered peak working bytes",
            }),
        );
        samples.push(sample);
        if !args.keep_outputs {
            fs::remove_dir_all(&destination)
                .with_context(|| format!("failed to remove {}", destination.display()))?;
        }
    }

    let source_preservation_after = source_inventory_facts(&source)?;
    let source_preserved = source_preservation_before == source_preservation_after;
    let executable_digest_end = sha256_file(&executable)?;
    let repository_end = repository_identity();
    let hardware_end = host_hardware_identity();
    let qualification_end = assess_import_qualification_profile(
        args.qualification_profile.as_deref(),
        &repository_end,
        &scratch_root,
        &hardware_end,
        &protocol,
    );
    let qualification_session_end = assess_import_qualification_profile(
        args.qualification_profile.as_deref(),
        &repository_end,
        &session_root,
        &hardware_end,
        &protocol,
    );
    let mut primary_times = samples
        .iter()
        .map(|sample| {
            sample["primary_clock"]["wall_time_ns"]
                .as_u64()
                .context("sample primary wall time is missing")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    primary_times.sort_unstable();
    let median_primary_wall_time_ns = primary_times[primary_times.len() / 2];
    let absolute_gate_passed = median_primary_wall_time_ns <= 60_000_000_000;
    let runtime_gates_passed = samples.iter().all(|sample| {
        sample
            .pointer("/external_rss/gate_passed")
            .and_then(Value::as_bool)
            == Some(true)
            && sample
                .pointer("/counter_reconciliation/all_passed")
                .and_then(Value::as_bool)
                == Some(true)
            && sample
                .pointer("/runtime_gates/all_passed")
                .and_then(Value::as_bool)
                == Some(true)
    });
    let package_ids = samples
        .iter()
        .filter_map(|sample| sample.get("package_id").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    let scientific_ids = samples
        .iter()
        .filter_map(|sample| sample.get("scientific_content_id").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    let deterministic_across_samples = package_ids.len() == 1
        && scientific_ids.len() == 1
        && samples.iter().all(|sample| {
            sample.get("package_id").and_then(Value::as_str).is_some()
                && sample
                    .get("scientific_content_id")
                    .and_then(Value::as_str)
                    .is_some()
        });
    let repository_unchanged = repository_identity_unchanged(&repository_start, &repository_end);
    let clean_at_end = repository_is_clean(&repository_end);
    let workers_used_expected_executable = samples.iter().all(|sample| {
        sample
            .pointer("/executable_identity/start_sha256")
            .and_then(Value::as_str)
            == Some(executable_digest_start.as_str())
            && sample
                .pointer("/executable_identity/end_sha256")
                .and_then(Value::as_str)
                == Some(executable_digest_start.as_str())
            && sample
                .pointer("/executable_identity/unchanged")
                .and_then(Value::as_bool)
                == Some(true)
    });
    let build_provenance_evidence = qualification_build_provenance_evidence(&build_provenance);
    let workers_used_expected_build = samples
        .iter()
        .all(|sample| sample.get("build_provenance") == Some(&build_provenance_evidence));
    let executable_unchanged =
        executable_digest_start == executable_digest_end && workers_used_expected_executable;
    let qualification_assessment_unchanged =
        qualification_assessments_unchanged(&qualification_start, &qualification_end)
            && qualification_assessments_unchanged(
                &qualification_session_start,
                &qualification_session_end,
            );
    let qualification_profile_matched = qualification_start.status == "matched"
        && qualification_end.status == "matched"
        && qualification_session_start.status == "matched"
        && qualification_session_end.status == "matched"
        && qualification_assessment_unchanged;
    let build_reason_codes_start =
        qualification_build_reason_codes(&build_provenance, &repository_start);
    let build_reason_codes_end =
        qualification_build_reason_codes(&build_provenance, &repository_end);
    let mut diagnostic_reason_codes = BTreeSet::new();
    if !clean_at_start {
        diagnostic_reason_codes.insert("repository_not_clean_at_start");
    }
    if !clean_at_end {
        diagnostic_reason_codes.insert("repository_not_clean_at_end");
    }
    if !repository_unchanged {
        diagnostic_reason_codes.insert("repository_identity_changed");
    }
    if !executable_unchanged {
        diagnostic_reason_codes.insert("executable_identity_changed");
    }
    if !workers_used_expected_build {
        diagnostic_reason_codes.insert("worker_build_provenance_mismatch");
    }
    if args.samples != DEFAULT_SAMPLES {
        diagnostic_reason_codes.insert("sample_count_not_five");
    }
    if args.cache_condition == "uncontrolled" {
        diagnostic_reason_codes.insert("cache_condition_uncontrolled");
    }
    if args.competing_activity == "uncontrolled" {
        diagnostic_reason_codes.insert("competing_activity_uncontrolled");
    }
    if !qualification_profile_matched {
        diagnostic_reason_codes.insert("qualification_profile_not_bound");
    }
    for reason in qualification_start
        .reason_codes
        .iter()
        .chain(&qualification_session_start.reason_codes)
        .chain(&qualification_end.reason_codes)
        .chain(&qualification_session_end.reason_codes)
    {
        diagnostic_reason_codes.insert(*reason);
    }
    for reason in build_reason_codes_start
        .iter()
        .chain(&build_reason_codes_end)
    {
        diagnostic_reason_codes.insert(*reason);
    }
    if !absolute_gate_passed {
        diagnostic_reason_codes.insert("absolute_time_gate_failed");
    }
    if !runtime_gates_passed {
        diagnostic_reason_codes.insert("runtime_gate_failed");
    }
    if !deterministic_across_samples {
        diagnostic_reason_codes.insert("sample_identities_not_deterministic");
    }
    if !source_preserved {
        diagnostic_reason_codes.insert("source_inventory_changed");
    }
    let diagnostic_reason_codes = diagnostic_reason_codes.into_iter().collect::<Vec<_>>();
    let qualification_eligible = diagnostic_reason_codes.is_empty();
    let filesystem_class = qualification_start
        .filesystem_type
        .clone()
        .or_else(|| filesystem_class(&session_root));
    let reported_hardware_class = if qualification_profile_matched {
        qualification_start.hardware_class.as_deref()
    } else {
        None
    };
    let report = json!({
        "schema": EVIDENCE_SCHEMA,
        "session_id": session_id,
        "workload": {
            "id": "T2",
            "dtype": "uint8",
            "shape_tczyx": [1, 1, T2_Z, T2_Y, T2_X],
            "files": T2_Z,
            "native_layout": "one-uncompressed-full-image-strip-per-plane",
            "validity": {"kind": "u8-sentinel", "value": T2_SENTINEL},
            "generator": "mirante4d-t2-values-1",
            "independent_expected_facts": expected_facts,
        },
        "protocol": {
            "sample_count": args.samples,
            "fixture_generation_outside_primary_clock": true,
            "fresh_checkpoint_and_absent_destination_per_sample": true,
            "working_memory_bytes": WORKING_MEMORY_BYTES,
            "cache_condition": args.cache_condition,
            "competing_activity": args.competing_activity,
            "filesystem_class": filesystem_class,
            "hardware_class": reported_hardware_class,
            "rss_sampling_interval_ms": RSS_SAMPLE_INTERVAL.as_millis(),
        },
        "build": {
            "host": host,
            "provenance": build_provenance_evidence,
            "provenance_reason_codes_at_start": build_reason_codes_start,
            "provenance_reason_codes_at_end": build_reason_codes_end,
            "all_workers_match_build_provenance": workers_used_expected_build,
            "repository": {
                "start": repository_evidence(&repository_start),
                "end": repository_evidence(&repository_end),
                "clean_at_start": clean_at_start,
                "clean_at_end": clean_at_end,
                "unchanged": repository_unchanged,
            },
            "executable": {
                "start_sha256": executable_digest_start,
                "end_sha256": executable_digest_end,
                "all_workers_match": workers_used_expected_executable,
                "unchanged": executable_unchanged,
            },
        },
        "qualification_profile": {
            "required_schema": IMPORT_QUALIFICATION_PROFILE_SCHEMA,
            "maximum_bytes": IMPORT_QUALIFICATION_PROFILE_MAX_BYTES,
            "owner_accepted_profile_sha256": OWNER_ACCEPTED_IMPORT_QUALIFICATION_PROFILE_SHA256,
            "start": import_qualification_assessment_evidence(&qualification_start),
            "session_start": import_qualification_assessment_evidence(&qualification_session_start),
            "end": import_qualification_assessment_evidence(&qualification_end),
            "session_end": import_qualification_assessment_evidence(&qualification_session_end),
            "assessment_unchanged": qualification_assessment_unchanged,
            "matched_at_both_boundaries": qualification_profile_matched,
        },
        "fixture_generation_wall_time_ns": generation_wall_time_ns,
        "source_preservation": {
            "before": source_preservation_before,
            "after": source_preservation_after,
            "unchanged": source_preserved,
        },
        "samples": samples,
        "summary": {
            "median_primary_wall_time_ns": median_primary_wall_time_ns,
            "absolute_gate_ns": 60_000_000_000_u64,
            "absolute_gate_passed": absolute_gate_passed,
            "all_runtime_gates_passed": runtime_gates_passed,
            "deterministic_package_and_scientific_ids": deterministic_across_samples,
            "qualification_eligible": qualification_eligible,
            "diagnostic_reason_codes": diagnostic_reason_codes,
            "diagnostic_reason": if qualification_eligible {
                Value::Null
            } else {
                Value::String("this run is diagnostic because one or more fail-closed qualification requirements did not hold; inspect diagnostic_reason_codes".to_owned())
            },
        },
    });
    let report_path = session_root.join("report.json");
    write_json_file(&report_path, &report)?;
    Ok(report_path)
}

pub(crate) fn run_worker(args: Vec<String>) -> anyhow::Result<()> {
    let build_provenance = qualification_build_provenance();
    require_release_build(&build_provenance)?;
    if args.len() != 7 {
        bail!("internal T2 worker received an invalid argument count");
    }
    let source = PathBuf::from(&args[0]);
    let destination = PathBuf::from(&args[1]);
    let checkpoint = PathBuf::from(&args[2]);
    let result = PathBuf::from(&args[3]);
    let baseline = PathBuf::from(&args[4]);
    let primary_marker = PathBuf::from(&args[5]);
    let sample_index = args[6]
        .parse::<usize>()
        .context("internal T2 sample index is invalid")?;
    let executable = env::current_exe().context("failed to resolve the T2 worker executable")?;
    let executable_digest_start = sha256_file(&executable)?;

    let inspection_started = Instant::now();
    let inspection_cpu_started_ns = process_cpu_time_ns()?;
    let inspection = inspect_tiff(TiffSource::auto(&source))?;
    let inspection_wall_time_ns = duration_ns(inspection_started.elapsed())?;
    let inspection_cpu_time_ns = process_cpu_time_ns()?
        .checked_sub(inspection_cpu_started_ns)
        .context("inspection process CPU clock moved backwards")?;
    if inspection.shape.dimensions() != [1, u64::from(T2_Z), u64::from(T2_Y), u64::from(T2_X)]
        || inspection.channels != 1
    {
        bail!("generated T2 inspection disagrees with its frozen geometry");
    }
    let mut options = ImportOptions {
        inspection,
        destination: destination.clone(),
        checkpoint_directory: checkpoint,
        profile: ProfileKind::Ds0,
        calibration: SpatialCalibration::new([0.4, 0.2, 0.1]),
        time_step_seconds: None,
        no_data: Some(NoDataPolicy::U8Sentinel(T2_SENTINEL)),
        working_memory_bytes: WORKING_MEMORY_BYTES,
    };
    options.profile = select_supported_profile(&options)?;
    let profile = options.profile;
    let source_bytes = options.inspection.source_bytes;
    let ledger = MeasurementLedger::new(WORKING_MEMORY_BYTES);
    let baseline_rss_bytes = linux_process_rss_bytes(std::process::id())
        .context("T2 worker could not capture its pre-import RSS baseline")?;
    if baseline_rss_bytes == 0 {
        bail!("T2 worker captured a zero-byte pre-import RSS baseline");
    }
    fs::write(&baseline, format!("{baseline_rss_bytes}\n"))
        .with_context(|| format!("failed to write {}", baseline.display()))?;
    fs::write(&primary_marker, b"active\n")
        .with_context(|| format!("failed to write {}", primary_marker.display()))?;

    let primary_started = Instant::now();
    let primary_cpu_started_ns = process_cpu_time_ns()?;
    let published = import_tiff(options, &ledger, &ImportCancellation::new(), |_| {})?;
    let (receipt, transfer) = published.into_parts();
    if receipt.scientific_content_id.to_string() != T2_EXPECTED_SCIENTIFIC_CONTENT_ID {
        bail!("T2 import differs from the independently frozen scientific identity");
    }
    let counter_reconciliation =
        validate_counter_reconciliation(&receipt.statistics, ledger.peak_bytes())?;
    let runtime_gates = validate_t2_runtime_gates(&receipt.statistics, source_bytes)?;
    let publication_transfer_started = Instant::now();
    let publication_transfer_cpu_started_ns = process_cpu_time_ns()?;
    let (verified, publication_currentness) = transfer.consume(|| false)?;
    let publication_transfer_wall_time_ns = duration_ns(publication_transfer_started.elapsed())?;
    let publication_transfer_cpu_time_ns = process_cpu_time_ns()?
        .checked_sub(publication_transfer_cpu_started_ns)
        .context("publication transfer process CPU clock moved backwards")?;
    let primary_wall_time_ns = duration_ns(primary_started.elapsed())?;
    let primary_cpu_time_ns = process_cpu_time_ns()?
        .checked_sub(primary_cpu_started_ns)
        .context("primary process CPU clock moved backwards")?;
    fs::remove_file(&primary_marker)
        .with_context(|| format!("failed to remove {}", primary_marker.display()))?;
    if verified.package_id() != receipt.package_id
        || verified.scientific_content_id() != receipt.scientific_content_id
    {
        bail!("published capability transfer disagrees with the import receipt");
    }
    let independent_pyramid_validation = validate_imported_t2_scales(&verified)?;
    if ledger.current_bytes() != 0 {
        bail!("the T2 import leaked a CPU-ledger charge");
    }
    let executable_digest_end = sha256_file(&executable)?;
    if executable_digest_start != executable_digest_end {
        bail!("the T2 worker executable changed during its sample");
    }
    let publication_capability_transfer = json!({
        "wall_time_ns": publication_transfer_wall_time_ns,
        "cpu_time_ns": publication_transfer_cpu_time_ns,
        "publication_currentness_execution": {
            "contract_id": publication_currentness.contract_id(),
            "expected_snapshot_object_reads": publication_currentness.expected_snapshot_object_reads(),
            "first_inventory_object_reads": publication_currentness.first_inventory_object_reads(),
            "observed_snapshot_object_reads": publication_currentness.observed_snapshot_object_reads(),
            "second_inventory_object_reads": publication_currentness.second_inventory_object_reads(),
            "observed_total_object_reads": publication_currentness.observed_total_object_reads(),
            "observed_codec_decode_calls": publication_currentness.observed_codec_decode_calls(),
        },
    });
    validate_publication_capability_transfer(&publication_capability_transfer)?;
    let value = json!({
        "schema": WORKER_SCHEMA,
        "sample_index": sample_index,
        "build_provenance": qualification_build_provenance_evidence(&build_provenance),
        "executable_identity": {
            "start_sha256": executable_digest_start,
            "end_sha256": executable_digest_end,
            "unchanged": true,
        },
        "inspection": {
            "wall_time_ns": inspection_wall_time_ns,
            "cpu_time_ns": inspection_cpu_time_ns,
            "source_bytes": source_bytes,
        },
        "primary_clock": {
            "wall_time_ns": primary_wall_time_ns,
            "cpu_time_ns": primary_cpu_time_ns,
            "boundary": "import-entry-through-published-capability-currentness-and-open-ready-authority",
        },
        "profile": profile.name(),
        "package_id": receipt.package_id.to_string(),
        "scientific_content_id": receipt.scientific_content_id.to_string(),
        "import": statistics_json(&receipt.statistics),
        "counter_reconciliation": counter_reconciliation,
        "runtime_gates": runtime_gates,
        "publication_capability_transfer": publication_capability_transfer,
        "independent_pyramid_validation": independent_pyramid_validation,
        "ledger": {
            "budget_bytes": WORKING_MEMORY_BYTES,
            "peak_bytes": ledger.peak_bytes(),
        },
    });
    write_json_file(&result, &value)
}

fn validate_publication_capability_transfer(transfer: &Value) -> anyhow::Result<()> {
    transfer
        .get("wall_time_ns")
        .and_then(Value::as_u64)
        .context("publication capability transfer omitted wall time")?;
    transfer
        .get("cpu_time_ns")
        .and_then(Value::as_u64)
        .context("publication capability transfer omitted process CPU time")?;
    let execution = transfer
        .get("publication_currentness_execution")
        .context("publication capability transfer omitted storage execution evidence")?;
    if execution.get("contract_id").and_then(Value::as_str)
        != Some(PUBLICATION_CURRENTNESS_CONTRACT_ID)
    {
        bail!("publication capability transfer used an unknown currentness execution contract");
    }
    let expected = execution
        .get("expected_snapshot_object_reads")
        .and_then(Value::as_u64)
        .context("publication capability transfer omitted expected snapshot object reads")?;
    let first_inventory = execution
        .get("first_inventory_object_reads")
        .and_then(Value::as_u64)
        .context("publication capability transfer omitted first-inventory object reads")?;
    let observed_snapshot = execution
        .get("observed_snapshot_object_reads")
        .and_then(Value::as_u64)
        .context("publication capability transfer omitted observed snapshot object reads")?;
    let second_inventory = execution
        .get("second_inventory_object_reads")
        .and_then(Value::as_u64)
        .context("publication capability transfer omitted second-inventory object reads")?;
    let observed_total = execution
        .get("observed_total_object_reads")
        .and_then(Value::as_u64)
        .context("publication capability transfer omitted total observed object reads")?;
    let codec_decodes = execution
        .get("observed_codec_decode_calls")
        .and_then(Value::as_u64)
        .context("publication capability transfer omitted observed codec decode calls")?;
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
            "publication capability transfer execution disagreed with the inventory/snapshot/inventory contract"
        );
    }
    Ok(())
}

fn validate_imported_t2_scales(
    verified: &VerifiedScientificPackageCapability,
) -> anyhow::Result<Value> {
    const INNER: u64 = 64;
    let mut facts = Vec::with_capacity(T2_EXPECTED_SCALE_FACTS.len());
    for (scale, (shape, expected_digest)) in T2_EXPECTED_SCALE_FACTS.iter().enumerate() {
        let voxel_count = shape
            .iter()
            .try_fold(1_u64, |total, value| total.checked_mul(*value))
            .context("T2 scale voxel count overflow")?;
        let mut values = vec![0_u8; usize::try_from(voxel_count)?];
        let mut validity = vec![0_u8; usize::try_from(voxel_count.div_ceil(8))?];
        let grid = shape.map(|value| value.div_ceil(INNER));
        let mut brick_reads = 0_u64;
        for z_chunk in 0..grid[0] {
            for y_chunk in 0..grid[1] {
                for x_chunk in 0..grid[2] {
                    let brick = verified.read_brick(
                        PackedIndexCoordinates::new(
                            0,
                            u32::try_from(scale)?,
                            0,
                            0,
                            u32::try_from(z_chunk)?,
                            u32::try_from(y_chunk)?,
                            u32::try_from(x_chunk)?,
                        ),
                        || false,
                    )?;
                    brick_reads = brick_reads.checked_add(1).context("brick read overflow")?;
                    let extent = brick.logical_extent_zyx();
                    let record = brick.record();
                    for local_z in 0..extent[0] {
                        for local_y in 0..extent[1] {
                            for local_x in 0..extent[2] {
                                let padded_index = (local_z * INNER + local_y) * INNER + local_x;
                                let global_z = z_chunk * INNER + local_z;
                                let global_y = y_chunk * INNER + local_y;
                                let global_x = x_chunk * INNER + local_x;
                                let global_index =
                                    (global_z * shape[1] + global_y) * shape[2] + global_x;
                                let global_index = usize::try_from(global_index)?;
                                values[global_index] = brick.pixel_payload().map_or(0, |pixels| {
                                    pixels[usize::try_from(padded_index)
                                        .expect("profile-sized padded index fits usize")]
                                });
                                let valid = if record.all_voxels_valid() {
                                    true
                                } else if record.all_voxels_invalid() {
                                    false
                                } else {
                                    let packed = brick.validity_payload().ok_or_else(|| {
                                        anyhow::anyhow!(
                                            "T2 explicit-validity brick omitted its effective mask"
                                        )
                                    })?;
                                    let index = usize::try_from(padded_index)?;
                                    packed[index / 8] & (1 << (index % 8)) != 0
                                };
                                if valid {
                                    validity[global_index / 8] |= 1 << (global_index % 8);
                                } else if values[global_index] != 0 {
                                    bail!(
                                        "T2 invalid voxel is not canonical zero at scale {scale}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        let expected_bricks = grid
            .into_iter()
            .try_fold(1_u64, |total, value| total.checked_mul(value))
            .context("T2 brick count overflow")?;
        if brick_reads != expected_bricks {
            bail!("T2 scale {scale} was not scanned exactly once per logical brick");
        }
        let mut hasher = Sha256Hasher::new();
        hasher.update(b"mirante4d-t2-scale-fact-1\0");
        hasher.update(u32::try_from(scale)?.to_le_bytes());
        for dimension in shape {
            hasher.update(dimension.to_le_bytes());
        }
        for (index, value) in values.into_iter().enumerate() {
            let valid = validity[index / 8] & (1 << (index % 8)) != 0;
            hasher.update([value, u8::from(valid)]);
        }
        let actual_digest = hasher.finalize().to_string();
        if actual_digest != *expected_digest {
            bail!("imported T2 pyramid scale {scale} differs from its frozen expected fact");
        }
        facts.push(json!({
            "scale": scale,
            "shape_zyx": shape,
            "value_validity_digest": actual_digest,
            "brick_reads": brick_reads,
        }));
    }
    Ok(json!({
        "scheme": "mirante4d-t2-imported-scale-validation-1",
        "reader": "exact-capability-brick-reader",
        "scales": facts,
    }))
}

fn statistics_json(statistics: &ImportStatistics) -> Value {
    let Value::Object(mut value) = json!({
        "source_bytes_read": statistics.source_bytes_read,
        "source_revalidation_bytes_read": statistics.source_revalidation_bytes_read,
        "native_decoded_bytes": statistics.native_decoded_bytes,
        "base_native_decoded_bytes": statistics.base_native_decoded_bytes,
        "scientific_identity_native_decoded_bytes": statistics.scientific_identity_native_decoded_bytes,
        "tiff_open_count": statistics.tiff_open_count,
        "native_chunk_decode_count": statistics.native_chunk_decode_count,
        "logical_output_bytes": statistics.logical_output_bytes,
        "checkpoint_payload_bytes": statistics.checkpoint_payload_bytes,
        "checkpoint_journal_bytes": statistics.checkpoint_journal_bytes,
        "checkpoint_watermark_bytes": statistics.checkpoint_watermark_bytes,
        "checkpoint_durable_work_units": statistics.checkpoint_durable_work_units,
        "checkpoint_pending_work_units": statistics.checkpoint_pending_work_units,
        "checkpoint_committed_batches": statistics.checkpoint_committed_batches,
        "codec_encode_calls": statistics.codec_encode_calls,
        "codec_encode_time_ns": statistics.codec_encode_time_ns,
        "codec_decode_calls": statistics.codec_decode_calls,
        "codec_decode_time_ns": statistics.codec_decode_time_ns,
        "sync_calls": statistics.sync_calls,
        "sync_time_ns": statistics.sync_time_ns,
    }) else {
        unreachable!("a JSON object literal produces an object");
    };
    let Value::Object(observations) = json!({
        "scientific_brick_reads": statistics.scientific_brick_reads,
        "staged_structure_object_reads": statistics.staged_structure_object_reads,
        "staged_exact_object_reads": statistics.staged_exact_object_reads,
        "scientific_object_reads": statistics.scientific_object_reads,
        "scientific_payload_object_reads": statistics.scientific_payload_object_reads,
        "scientific_range_requests": statistics.scientific_range_requests,
        "scientific_encoded_bytes_read": statistics.scientific_encoded_bytes_read,
        "scientific_decoded_bytes": statistics.scientific_decoded_bytes,
        "object_reads": statistics.object_reads,
        "sampled_peak_open_file_descriptors": statistics.sampled_peak_open_file_descriptors,
        "open_file_descriptor_structural_bound": statistics.open_file_descriptor_structural_bound,
        "peak_open_file_descriptors": statistics.peak_open_file_descriptors,
        "preflight_temporary_bytes_bound": statistics.preflight_temporary_bytes_bound,
        "peak_temporary_bytes": statistics.peak_temporary_bytes,
        "peak_checkpoint_regular_files": statistics.peak_checkpoint_regular_files,
        "peak_working_bytes": statistics.peak_working_bytes,
        "peak_process_rss_bytes": statistics.peak_process_rss_bytes,
        "resumed_work_units": statistics.resumed_work_units,
        "produced_work_units": statistics.produced_work_units,
        "primary_wall_time_ns": statistics.primary_wall_time_ns,
        "primary_cpu_time_ns": statistics.primary_cpu_time_ns,
        "stages": statistics.stages.iter().map(|timing| json!({
            "stage": stage_name(timing.stage),
            "wall_time_ns": timing.wall_time_ns,
            "cpu_time_ns": timing.cpu_time_ns,
        })).collect::<Vec<_>>(),
    }) else {
        unreachable!("a JSON object literal produces an object");
    };
    value.extend(observations);
    Value::Object(value)
}

fn validate_counter_reconciliation(
    statistics: &ImportStatistics,
    ledger_peak_bytes: u64,
) -> anyhow::Result<Value> {
    if statistics.native_decoded_bytes
        != statistics
            .base_native_decoded_bytes
            .checked_add(statistics.scientific_identity_native_decoded_bytes)
            .context("native decoded byte counters overflow")?
    {
        bail!("native decoded byte stage counters do not reconcile");
    }
    if statistics.source_bytes_read < statistics.source_revalidation_bytes_read {
        bail!("source traffic is smaller than its integrity-traffic subset");
    }
    let reconciled_object_reads = statistics
        .staged_structure_object_reads
        .checked_add(statistics.staged_exact_object_reads)
        .and_then(|value| value.checked_add(statistics.scientific_object_reads))
        .context("staged object-read counters overflow")?;
    if statistics.object_reads != reconciled_object_reads {
        bail!("staged object-read counters do not reconcile");
    }
    if statistics.scientific_payload_object_reads > statistics.scientific_object_reads {
        bail!("scientific payload-object reads exceed all scientific object reads");
    }
    let enforced_open_file_descriptors = statistics
        .sampled_peak_open_file_descriptors
        .max(statistics.open_file_descriptor_structural_bound);
    if statistics.peak_open_file_descriptors != enforced_open_file_descriptors {
        bail!("the enforced open-file peak does not reconcile with sampled and structural bounds");
    }
    let stage_wall = statistics.stages.iter().try_fold(0_u64, |total, timing| {
        total.checked_add(timing.wall_time_ns)
    });
    let stage_cpu = statistics
        .stages
        .iter()
        .try_fold(0_u64, |total, timing| total.checked_add(timing.cpu_time_ns));
    if stage_wall.context("stage wall-time counters overflow")? > statistics.primary_wall_time_ns
        || stage_cpu.context("stage CPU-time counters overflow")? > statistics.primary_cpu_time_ns
    {
        bail!("completed stage timings exceed the primary import timing");
    }
    if statistics.peak_working_bytes > WORKING_MEMORY_BYTES
        || ledger_peak_bytes > WORKING_MEMORY_BYTES
    {
        bail!("import working-set accounting exceeds the selected memory budget");
    }
    if statistics.peak_working_bytes != ledger_peak_bytes {
        bail!("the import receipt and independent CPU ledger disagree on peak working bytes");
    }
    Ok(json!({
        "all_passed": true,
        "native_decoded_bytes_reconciled": true,
        "source_revalidation_is_source_traffic_subset": true,
        "staged_object_reads_reconciled": true,
        "scientific_payload_reads_are_subset": true,
        "open_file_peak_reconciled": true,
        "stage_timings_within_primary_clock": true,
        "receipt_peak_working_bytes": statistics.peak_working_bytes,
        "independent_ledger_peak_bytes": ledger_peak_bytes,
        "receipt_and_ledger_peak_equal": true,
        "working_memory_budget_bytes": WORKING_MEMORY_BYTES,
        "working_memory_budget_passed": true,
    }))
}

fn validate_t2_runtime_gates(
    statistics: &ImportStatistics,
    source_bytes: u64,
) -> anyhow::Result<Value> {
    let canonical_bytes = u64::from(T2_Z)
        .checked_mul(u64::from(T2_Y))
        .and_then(|value| value.checked_mul(u64::from(T2_X)))
        .context("T2 canonical byte count overflow")?;
    let base_native_decoded_bytes_max = canonical_bytes
        .checked_mul(110)
        .and_then(|value| value.checked_div(100))
        .context("T2 decode-amplification bound overflow")?;
    if statistics.base_native_decoded_bytes > base_native_decoded_bytes_max {
        bail!("T2 native decode amplification exceeds 1.10x");
    }
    if statistics.scientific_identity_native_decoded_bytes != 0 {
        bail!("T2 scientific identity performed an additional TIFF native decode");
    }
    let source_bytes_read_max = source_bytes
        .checked_mul(5)
        .and_then(|value| value.checked_div(2))
        .context("T2 source-traffic bound overflow")?;
    if statistics.source_bytes_read > source_bytes_read_max {
        bail!("T2 primary source traffic exceeds 2.50x unique source bytes");
    }
    if statistics.sync_calls >= 5_000 {
        bail!("T2 import exceeded the durability sync-call ceiling");
    }
    if statistics.checkpoint_pending_work_units != 0 {
        bail!("T2 publication began with a nondurable checkpoint suffix");
    }
    if statistics.peak_open_file_descriptors > 64 {
        bail!("T2 import exceeded the open-file bound");
    }
    if statistics.peak_temporary_bytes > statistics.preflight_temporary_bytes_bound {
        bail!("T2 import exceeded its preflight temporary-byte bound");
    }
    if statistics.peak_checkpoint_regular_files > 8 {
        bail!("T2 checkpoint exceeded the eight-file object bound");
    }
    let expected_base_bricks =
        u64::from(T2_Z).div_ceil(64) * u64::from(T2_Y).div_ceil(64) * u64::from(T2_X).div_ceil(64);
    if statistics.scientific_brick_reads != expected_base_bricks {
        bail!("T2 staged science did not read every base brick exactly once");
    }
    Ok(json!({
        "all_passed": true,
        "base_decode_amplification": {
            "canonical_source_pixel_bytes": canonical_bytes,
            "actual_native_decoded_bytes": statistics.base_native_decoded_bytes,
            "maximum_native_decoded_bytes": base_native_decoded_bytes_max,
            "passed": true,
        },
        "source_scientific_traversal": {
            "additional_native_decoded_bytes": statistics.scientific_identity_native_decoded_bytes,
            "required": 0,
            "passed": true,
        },
        "source_traffic": {
            "unique_source_bytes": source_bytes,
            "actual_bytes": statistics.source_bytes_read,
            "maximum_bytes": source_bytes_read_max,
            "passed": true,
        },
        "durability_calls": {
            "actual_sync_calls": statistics.sync_calls,
            "exclusive_maximum": 5_000_u64,
            "passed": true,
        },
        "durable_checkpoint_prefix": {
            "pending_work_units": statistics.checkpoint_pending_work_units,
            "required": 0,
            "passed": true,
        },
        "open_files": {
            "actual_peak": statistics.peak_open_file_descriptors,
            "maximum": 64_u64,
            "passed": true,
        },
        "temporary_bytes": {
            "actual_peak": statistics.peak_temporary_bytes,
            "preflight_bound": statistics.preflight_temporary_bytes_bound,
            "passed": true,
        },
        "checkpoint_objects": {
            "actual_peak_regular_files": statistics.peak_checkpoint_regular_files,
            "maximum": 8_u64,
            "passed": true,
        },
        "scientific_validation_locality": {
            "actual_base_brick_reads": statistics.scientific_brick_reads,
            "expected_base_brick_reads": expected_base_bricks,
            "passed": true,
        },
    }))
}

fn stage_name(stage: ImportStage) -> String {
    match stage {
        ImportStage::SourceRevalidation { pass } => format!("{}-{pass}", stage.name()),
        ImportStage::PyramidProduction { scale } => format!("{}-{scale}", stage.name()),
        _ => stage.name().to_owned(),
    }
}

fn parse_args(args: Vec<String>) -> anyhow::Result<RunArgs> {
    let mut parsed = RunArgs {
        samples: DEFAULT_SAMPLES,
        scratch: PathBuf::from("target/mirante4d/import-performance"),
        qualification_profile: None,
        cache_condition: "uncontrolled".to_owned(),
        competing_activity: "uncontrolled".to_owned(),
        keep_outputs: false,
    };
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--samples" => {
                parsed.samples = args
                    .next()
                    .context("--samples requires a value")?
                    .parse()
                    .context("--samples must be an integer")?;
            }
            "--scratch" => {
                parsed.scratch = PathBuf::from(args.next().context("--scratch requires a path")?);
            }
            "--qualification-profile" => {
                parsed.qualification_profile = Some(PathBuf::from(
                    args.next()
                        .context("--qualification-profile requires a path")?,
                ));
            }
            "--cache-condition" => {
                parsed.cache_condition =
                    args.next().context("--cache-condition requires a value")?;
            }
            "--competing-activity" => {
                parsed.competing_activity = args
                    .next()
                    .context("--competing-activity requires a value")?;
            }
            "--keep-outputs" => parsed.keep_outputs = true,
            "--help" | "-h" | "help" => {
                bail!(
                    "usage: cargo run --release -p xtask -- import-performance-t2 \
                     [--samples 5] [--scratch PATH] \
                     [--qualification-profile NON_REPOSITORY_PROFILE.json] \
                     [--cache-condition cold|warm|uncontrolled] \
                     [--competing-activity DESCRIPTION] [--keep-outputs]; \
                     qualification requires a matching local profile with schema \
                     {IMPORT_QUALIFICATION_PROFILE_SCHEMA}; absence or mismatch produces \
                     diagnostic-only evidence"
                );
            }
            _ => bail!("unknown import-performance-t2 argument {argument:?}"),
        }
    }
    if parsed.samples == 0 || parsed.samples > 20 {
        bail!("--samples must be between 1 and 20");
    }
    if !matches!(
        parsed.cache_condition.as_str(),
        "cold" | "warm" | "uncontrolled"
    ) {
        bail!("--cache-condition must be cold, warm, or uncontrolled");
    }
    if parsed.competing_activity.trim().is_empty() {
        bail!("--competing-activity must not be empty");
    }
    Ok(parsed)
}

pub(crate) fn generate_t2_source(root: &Path, z: u32, y: u32, x: u32) -> anyhow::Result<()> {
    if root.exists() {
        bail!("generated T2 source already exists: {}", root.display());
    }
    if z == 0 || y == 0 || x == 0 {
        bail!("generated T2 dimensions must be positive");
    }
    fs::create_dir(root).with_context(|| format!("failed to create {}", root.display()))?;
    let planes = root.join("channel-000");
    fs::create_dir(&planes).with_context(|| format!("failed to create {}", planes.display()))?;
    let plane_len = usize::try_from(u64::from(y) * u64::from(x))
        .context("generated T2 plane does not fit this platform")?;
    let mut plane = vec![0_u8; plane_len];
    for plane_z in 0..z {
        for plane_y in 0..y {
            let row_start = usize::try_from(u64::from(plane_y) * u64::from(x))?;
            for plane_x in 0..x {
                plane[row_start + usize::try_from(plane_x)?] = t2_value(plane_z, plane_y, plane_x);
            }
        }
        let path = planes.join(format!("plane-z{plane_z:05}.tif"));
        let file =
            File::create(&path).with_context(|| format!("failed to create {}", path.display()))?;
        let mut encoder = TiffEncoder::new(file)
            .with_context(|| format!("failed to initialize {}", path.display()))?;
        let mut image = encoder
            .new_image::<colortype::Gray8>(x, y)
            .with_context(|| format!("failed to describe {}", path.display()))?;
        image
            .rows_per_strip(y)
            .with_context(|| format!("failed to set one strip for {}", path.display()))?;
        image
            .write_data(&plane)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(())
}

fn t2_value(z: u32, y: u32, x: u32) -> u8 {
    let z = u64::from(z);
    let y = u64::from(y);
    let x = u64::from(x);
    if (x + 3 * y + 5 * z).is_multiple_of(1_021) {
        T2_SENTINEL
    } else {
        ((31 * z + 17 * y + 13 * x + (x * y) % 251) % 255) as u8
    }
}

fn t2_expected_facts(z: u32, y: u32, x: u32) -> anyhow::Result<Value> {
    let shape = Shape4D::new(1, u64::from(z), u64::from(y), u64::from(x))?;
    let transform = GridToWorld::scale(0.1, 0.2, 0.4)?;
    let descriptor = ScientificLayerDescriptor::new(
        LogicalLayerKey::new(0),
        mirante4d_domain::IntensityDType::Uint8,
        shape,
        ScientificTemporalCalibration::Unknown,
        transform,
    )?;
    let mut layer = ScientificLayerHasher::new(descriptor)?;
    for tile_z in (0..u64::from(z)).step_by(SCIENTIFIC_TILE_SHAPE_TZYX[1] as usize) {
        for tile_y in (0..u64::from(y)).step_by(SCIENTIFIC_TILE_SHAPE_TZYX[2] as usize) {
            for tile_x in (0..u64::from(x)).step_by(SCIENTIFIC_TILE_SHAPE_TZYX[3] as usize) {
                let extent = [
                    (u64::from(z) - tile_z).min(SCIENTIFIC_TILE_SHAPE_TZYX[1]),
                    (u64::from(y) - tile_y).min(SCIENTIFIC_TILE_SHAPE_TZYX[2]),
                    (u64::from(x) - tile_x).min(SCIENTIFIC_TILE_SHAPE_TZYX[3]),
                ];
                let voxels = usize::try_from(extent[0] * extent[1] * extent[2])?;
                let mut values = Vec::with_capacity(voxels);
                let mut validity = vec![0_u8; voxels.div_ceil(8)];
                let mut index = 0_usize;
                for local_z in 0..extent[0] {
                    for local_y in 0..extent[1] {
                        for local_x in 0..extent[2] {
                            let raw = t2_value(
                                u32::try_from(tile_z + local_z)?,
                                u32::try_from(tile_y + local_y)?,
                                u32::try_from(tile_x + local_x)?,
                            );
                            let valid = raw != T2_SENTINEL;
                            values.push(if valid { raw } else { 0 });
                            if valid {
                                validity[index / 8] |= 1 << (index % 8);
                            }
                            index += 1;
                        }
                    }
                }
                layer.push_tile(ScientificTile::new(
                    [0, tile_z, tile_y, tile_x],
                    [1, extent[0], extent[1], extent[2]],
                    &validity,
                    &values,
                ))?;
            }
        }
    }
    let mut dataset = ScientificDatasetHasher::new(1)?;
    dataset.push_layer(layer.finalize()?)?;
    let scientific_content_id = dataset.finalize()?;
    let is_full_t2 = (z, y, x) == (T2_Z, T2_Y, T2_X);
    if is_full_t2 && scientific_content_id.to_string() != T2_EXPECTED_SCIENTIFIC_CONTENT_ID {
        bail!("analytic T2 scientific identity differs from its frozen expected fact");
    }

    let scales = t2_pyramid_shapes(z, y, x)
        .into_iter()
        .enumerate()
        .map(|(scale, shape)| {
            let mut hasher = Sha256Hasher::new();
            hasher.update(b"mirante4d-t2-scale-fact-1\0");
            hasher.update(u32::try_from(scale)?.to_le_bytes());
            for dimension in shape {
                hasher.update(dimension.to_le_bytes());
            }
            let source_step = 1_u64
                .checked_shl(u32::try_from(scale)?)
                .context("T2 pyramid scale exceeds the expected shift")?;
            for scale_z in 0..shape[0] {
                for scale_y in 0..shape[1] {
                    for scale_x in 0..shape[2] {
                        let raw = t2_value(
                            u32::try_from(scale_z * source_step)?,
                            u32::try_from(scale_y * source_step)?,
                            u32::try_from(scale_x * source_step)?,
                        );
                        let valid = raw != T2_SENTINEL;
                        hasher.update([if valid { raw } else { 0 }, u8::from(valid)]);
                    }
                }
            }
            let digest = hasher.finalize().to_string();
            if is_full_t2 {
                let Some((expected_shape, expected_digest)) = T2_EXPECTED_SCALE_FACTS.get(scale)
                else {
                    bail!("analytic T2 produced more pyramid scales than its frozen facts");
                };
                if shape != *expected_shape || digest != *expected_digest {
                    bail!("analytic T2 scale {scale} differs from its frozen expected fact");
                }
            }
            Ok(json!({
                "scale": scale,
                "shape_zyx": shape,
                "value_validity_digest": digest,
            }))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    if is_full_t2 && scales.len() != T2_EXPECTED_SCALE_FACTS.len() {
        bail!("analytic T2 produced fewer pyramid scales than its frozen facts");
    }
    Ok(json!({
        "scheme": "mirante4d-t2-expected-facts-1",
        "producer": "analytic-generator-independent-of-import-output",
        "scientific_content_id": scientific_content_id.to_string(),
        "pyramid_scales": scales,
    }))
}

fn t2_pyramid_shapes(z: u32, y: u32, x: u32) -> Vec<[u64; 3]> {
    let mut shapes = vec![[u64::from(z), u64::from(y), u64::from(x)]];
    if u64::from(z.max(y).max(x)) <= 256 {
        return shapes;
    }
    loop {
        let previous = *shapes.last().unwrap();
        let next = previous.map(|dimension| dimension.div_ceil(2));
        shapes.push(next);
        let maximum = next.into_iter().max().unwrap();
        let voxels = next[0] * next[1] * next[2];
        if maximum <= 64 || voxels <= 262_144 {
            return shapes;
        }
    }
}

fn qualification_assessments_unchanged(
    start: &ImportQualificationAssessment,
    end: &ImportQualificationAssessment,
) -> bool {
    start == end
}

fn repository_identity_unchanged(start: &RepositoryIdentity, end: &RepositoryIdentity) -> bool {
    start.root.is_some()
        && start.commit.is_some()
        && start.root == end.root
        && start.commit == end.commit
}

fn repository_is_clean(identity: &RepositoryIdentity) -> bool {
    identity.root.is_some() && identity.commit.is_some() && identity.dirty_worktree == Some(false)
}

fn repository_evidence(identity: &RepositoryIdentity) -> Value {
    json!({
        "commit": identity.commit,
        "dirty_worktree": identity.dirty_worktree,
        "repository_root_resolved": identity.root.is_some(),
    })
}

fn require_release_build(provenance: &QualificationBuildProvenance) -> anyhow::Result<()> {
    if cfg!(debug_assertions)
        || provenance.cargo_profile != "release"
        || provenance.opt_level != "3"
        || provenance.debug != "false"
    {
        bail!(
            "T2 timing evidence requires the standard release profile (opt-level 3, debug false); use \
             `cargo run --release -p xtask -- import-performance-t2`"
        );
    }
    Ok(())
}

fn require_t2_rss_observations(
    baseline_rss_bytes: u64,
    primary_rss_sample_count: u64,
) -> anyhow::Result<()> {
    if baseline_rss_bytes == 0 {
        bail!("T2 worker RSS baseline must be greater than zero");
    }
    if primary_rss_sample_count == 0 {
        bail!("T2 parent captured no successful RSS samples during the primary interval");
    }
    Ok(())
}

fn session_id() -> anyhow::Result<String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time is before the Unix epoch")?
        .as_nanos();
    Ok(format!("t2-{nanos}-{}", std::process::id()))
}

fn source_inventory_facts(root: &Path) -> anyhow::Result<Value> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::<(Vec<u8>, PathBuf)>::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("failed to inventory {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                bail!("generated T2 source inventory contains a symbolic link");
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .context("generated T2 inventory escaped its root")?
                    .as_os_str()
                    .as_bytes()
                    .to_vec();
                files.push((relative, path));
            } else {
                bail!("generated T2 source inventory contains a special entry");
            }
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256Hasher::new();
    hasher.update(b"mirante4d-t2-source-inventory-1\0");
    let mut source_bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    for (relative, path) in &files {
        let metadata = fs::symlink_metadata(path)?;
        let length = metadata.len();
        source_bytes = source_bytes
            .checked_add(length)
            .context("generated T2 source byte count overflowed")?;
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
                .context("generated T2 source inventory read count overflowed")?;
            hasher.update(&buffer[..count]);
        }
        if consumed != length || fs::symlink_metadata(path)?.len() != length {
            bail!("generated T2 source changed while its inventory was captured");
        }
    }
    Ok(json!({
        "regular_files": files.len(),
        "source_bytes": source_bytes,
        "inventory_sha256": hasher.finalize().to_string(),
    }))
}

fn duration_ns(duration: Duration) -> anyhow::Result<u64> {
    u64::try_from(duration.as_nanos()).context("duration exceeds the evidence schema")
}

fn process_cpu_time_ns() -> anyhow::Result<u64> {
    let time = clock_gettime(ClockId::ProcessCPUTime);
    let seconds = u64::try_from(time.tv_sec).context("process CPU seconds are negative")?;
    let nanoseconds =
        u64::try_from(time.tv_nsec).context("process CPU nanoseconds are negative")?;
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(nanoseconds))
        .context("process CPU clock overflowed")
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)
        .with_context(|| format!("failed to open executable identity at {}", path.display()))?;
    let expected_length = file.metadata()?.len();
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
            .context("executable identity byte count overflowed")?;
        hasher.update(&buffer[..count]);
    }
    if consumed != expected_length || file.metadata()?.len() != expected_length {
        bail!("executable changed while its identity was captured");
    }
    Ok(hasher.finalize().to_string())
}

fn filesystem_class(path: &Path) -> Option<String> {
    let findmnt = Command::new("findmnt")
        .args(["--noheadings", "--output", "FSTYPE", "--target"])
        .arg(path)
        .output()
        .ok();
    if let Some(output) = findmnt.filter(|output| output.status.success()) {
        let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !value.is_empty() {
            return Some(value);
        }
    }
    let output = Command::new("stat")
        .args(["-f", "-c", "%T"])
        .arg(path)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn linux_process_rss_bytes(pid: u32) -> Option<u64> {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|kib| kib.checked_mul(1_024))
}

#[derive(Debug)]
struct LedgerCore {
    budget: u64,
    state: Mutex<LedgerState>,
}

#[derive(Clone, Copy, Debug, Default)]
struct LedgerState {
    current: u64,
    peak: u64,
}

#[derive(Clone, Debug)]
struct MeasurementLedger(Arc<LedgerCore>);

impl MeasurementLedger {
    fn new(budget: u64) -> Self {
        Self(Arc::new(LedgerCore {
            budget,
            state: Mutex::new(LedgerState::default()),
        }))
    }

    fn current_bytes(&self) -> u64 {
        self.0.state.lock().unwrap().current
    }

    fn peak_bytes(&self) -> u64 {
        self.0.state.lock().unwrap().peak
    }
}

impl CpuByteLedger for MeasurementLedger {
    fn try_acquire(
        &self,
        category: CpuLedgerCategory,
        bytes: u64,
    ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
        if bytes == 0 {
            return Err(CpuLedgerError::ZeroByteReservation);
        }
        let mut state = self.0.state.lock().unwrap();
        let available = self.0.budget.saturating_sub(state.current);
        if bytes > available {
            return Err(CpuLedgerError::CapacityExceeded {
                category,
                requested_bytes: bytes,
                available_bytes: available,
            });
        }
        state.current += bytes;
        state.peak = state.peak.max(state.current);
        drop(state);
        Ok(Box::new(MeasurementLease {
            core: Arc::clone(&self.0),
            category,
            bytes,
        }))
    }
}

#[derive(Debug)]
struct MeasurementLease {
    core: Arc<LedgerCore>,
    category: CpuLedgerCategory,
    bytes: u64,
}

impl CpuByteLease for MeasurementLease {
    fn category(&self) -> CpuLedgerCategory {
        self.category
    }

    fn reserved_bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for MeasurementLease {
    fn drop(&mut self) {
        let mut state = self.core.state.lock().unwrap();
        state.current = state
            .current
            .checked_sub(self.bytes)
            .expect("a measurement ledger lease releases exactly once");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiff::decoder::Decoder;

    #[test]
    fn t2_value_is_deterministic_and_reserves_only_the_explicit_sentinel() {
        assert_eq!(t2_value(0, 0, 0), T2_SENTINEL);
        assert_eq!(t2_value(1, 2, 3), 110);
        assert_eq!(t2_value(1, 2, 3), t2_value(1, 2, 3));
        for x in 1..1_021 {
            assert_ne!(t2_value(0, 0, x), T2_SENTINEL);
        }
    }

    #[test]
    fn generated_source_is_a_plane_series_with_exactly_one_strip_per_file() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        generate_t2_source(&source, 3, 5, 7).unwrap();

        let inspection = inspect_tiff(TiffSource::auto(&source)).unwrap();
        assert_eq!(inspection.shape.dimensions(), [1, 3, 5, 7]);
        assert_eq!(inspection.channels, 1);
        assert_eq!(fs::read_dir(&source).unwrap().count(), 1);
        let planes = source.join("channel-000");
        assert_eq!(fs::read_dir(&planes).unwrap().count(), 3);
        for path in fs::read_dir(&planes)
            .unwrap()
            .map(|entry| entry.unwrap().path())
        {
            let mut decoder = Decoder::new(File::open(path).unwrap()).unwrap();
            assert_eq!(decoder.dimensions().unwrap(), (7, 5));
            assert_eq!(decoder.strip_count().unwrap(), 1);
            assert_eq!(decoder.chunk_dimensions(), (7, 5));
        }
    }

    #[test]
    fn source_inventory_facts_detect_same_length_byte_changes() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        generate_t2_source(&source, 2, 3, 4).unwrap();
        let before = source_inventory_facts(&source).unwrap();
        let plane = source.join("channel-000/plane-z00000.tif");
        let mut bytes = fs::read(&plane).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        fs::write(&plane, bytes).unwrap();
        let after = source_inventory_facts(&source).unwrap();

        assert_eq!(before["regular_files"], after["regular_files"]);
        assert_eq!(before["source_bytes"], after["source_bytes"]);
        assert_ne!(before["inventory_sha256"], after["inventory_sha256"]);
    }

    #[test]
    fn args_bound_samples_and_mark_uncontrolled_runs_explicitly() {
        let defaults = parse_args(Vec::new()).unwrap();
        assert_eq!(defaults.samples, 5);
        assert_eq!(defaults.cache_condition, "uncontrolled");
        assert_eq!(defaults.qualification_profile, None);
        let configured = parse_args(vec![
            "--qualification-profile".into(),
            "/local/profile.json".into(),
        ])
        .unwrap();
        assert_eq!(
            configured.qualification_profile,
            Some(PathBuf::from("/local/profile.json"))
        );
        assert!(parse_args(vec!["--samples".into(), "0".into()]).is_err());
        assert!(parse_args(vec!["--cache-condition".into(), "maybe".into()]).is_err());
    }

    #[test]
    fn repository_and_qualification_boundaries_fail_closed_on_endpoint_changes() {
        let start = RepositoryIdentity {
            root: Some(PathBuf::from("/repository")),
            commit: Some("commit-a".to_owned()),
            dirty_worktree: Some(false),
        };
        let mut end = start.clone();
        assert!(repository_is_clean(&start));
        assert!(repository_identity_unchanged(&start, &end));
        end.dirty_worktree = Some(true);
        assert!(!repository_is_clean(&end));
        end.dirty_worktree = Some(false);
        end.commit = Some("commit-b".to_owned());
        assert!(!repository_identity_unchanged(&start, &end));

        let assessment = ImportQualificationAssessment {
            status: "matched",
            reason_codes: Vec::new(),
            profile_sha256: Some("profile-a".to_owned()),
            hardware_class: Some("HW-2".to_owned()),
            host_fingerprint_sha256: Some("host-a".to_owned()),
            storage_fingerprint_sha256: Some("storage-a".to_owned()),
            filesystem_type: Some("ext4".to_owned()),
        };
        let mut changed = assessment.clone();
        assert!(qualification_assessments_unchanged(&assessment, &changed));
        changed.storage_fingerprint_sha256 = Some("storage-b".to_owned());
        assert!(!qualification_assessments_unchanged(&assessment, &changed));
    }

    #[test]
    fn t2_rss_evidence_requires_a_baseline_and_primary_sample() {
        require_t2_rss_observations(1, 1).unwrap();
        assert!(require_t2_rss_observations(0, 1).is_err());
        assert!(require_t2_rss_observations(1, 0).is_err());
    }

    #[test]
    fn executable_digest_changes_with_same_length_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let executable = temporary.path().join("executable");
        fs::write(&executable, b"version-a").unwrap();
        let before = sha256_file(&executable).unwrap();
        fs::write(&executable, b"version-b").unwrap();
        let after = sha256_file(&executable).unwrap();

        assert_ne!(before, after);
    }

    #[test]
    fn object_read_and_file_descriptor_counters_reconcile_exactly() {
        let statistics = ImportStatistics {
            staged_structure_object_reads: 2,
            staged_exact_object_reads: 3,
            scientific_object_reads: 5,
            scientific_payload_object_reads: 4,
            object_reads: 10,
            sampled_peak_open_file_descriptors: 6,
            open_file_descriptor_structural_bound: 8,
            peak_open_file_descriptors: 8,
            ..ImportStatistics::default()
        };
        validate_counter_reconciliation(&statistics, 0).unwrap();

        let mut wrong_total = statistics.clone();
        wrong_total.object_reads = 9;
        assert!(validate_counter_reconciliation(&wrong_total, 0).is_err());
        let mut wrong_peak = statistics;
        wrong_peak.peak_open_file_descriptors = 7;
        assert!(validate_counter_reconciliation(&wrong_peak, 0).is_err());

        let mismatched_working_peaks = ImportStatistics {
            peak_working_bytes: 1,
            ..ImportStatistics::default()
        };
        assert!(validate_counter_reconciliation(&mismatched_working_peaks, 2).is_err());
    }

    #[test]
    fn t2_parent_validates_publication_transfer_execution_instead_of_trusting_zero_literals() {
        let transfer = json!({
            "wall_time_ns": 10,
            "cpu_time_ns": 7,
            "publication_currentness_execution": {
                "contract_id": PUBLICATION_CURRENTNESS_CONTRACT_ID,
                "expected_snapshot_object_reads": 11,
                "first_inventory_object_reads": 17,
                "observed_snapshot_object_reads": 11,
                "second_inventory_object_reads": 17,
                "observed_total_object_reads": 45,
                "observed_codec_decode_calls": 0,
            },
        });
        validate_publication_capability_transfer(&transfer).unwrap();

        let mut extra_reads = transfer.clone();
        extra_reads["publication_currentness_execution"]["observed_snapshot_object_reads"] =
            json!(22);
        extra_reads["publication_currentness_execution"]["observed_total_object_reads"] = json!(56);
        assert!(validate_publication_capability_transfer(&extra_reads).is_err());

        let mut decoded = transfer;
        decoded["publication_currentness_execution"]["observed_codec_decode_calls"] = json!(1);
        assert!(validate_publication_capability_transfer(&decoded).is_err());
    }

    #[test]
    fn measurement_ledger_enforces_and_releases_the_selected_budget() {
        let ledger = MeasurementLedger::new(10);
        let lease = ledger
            .try_acquire(CpuLedgerCategory::ImportWorkingSet, 7)
            .unwrap();
        assert_eq!(ledger.current_bytes(), 7);
        assert!(
            ledger
                .try_acquire(CpuLedgerCategory::ImportWorkingSet, 4)
                .is_err()
        );
        drop(lease);
        assert_eq!(ledger.current_bytes(), 0);
        assert_eq!(ledger.peak_bytes(), 7);
    }
}

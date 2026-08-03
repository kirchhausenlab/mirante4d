use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use mirante4d_identity::Sha256Hasher;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    private_evidence::{read_finalized_private_file, write_new_synced_private_file},
    process::{BoundedOutputPolicy, ensure_nextest, run_command_with_bounded_output},
    reports::{read_json_file, write_json_file},
    verification::registry,
};

const CONFIG_SCHEMA: &str = "mirante4d-gpu-performance-config";
const BASELINE_SCHEMA: &str = "mirante4d-gpu-performance-baseline";
const RAW_REPORT_SCHEMA: &str = "mirante4d-gpu-performance-raw-report";
const PUBLIC_REPORT_SCHEMA: &str = "mirante4d-gpu-performance-report";
const CALIBRATION_PROPOSAL_SCHEMA: &str = "mirante4d-gpu-performance-calibration-proposal";
const PRESENTATION_REPORT_SCHEMA: &str = "mirante4d-vulkan-present-timing-observer-report";
const PRESENTATION_REPORT_VERSION: u64 = 2;
const PRESENTATION_AUTHORITY: &str = "vulkan_ext_present_timing_first_pixel_out_marker_v2";
const MEASUREMENT_VERSION: &str = "gpu-performance-v3";
const CONFIG_VERSION: u32 = 2;
const BASELINE_VERSION: u32 = 2;
const REPORT_VERSION: u32 = 1;
const COMPONENT_MARKER: &str = "M4D_GPU_PERFORMANCE_V1 ";
const COMPONENT_GATE_MARKER: &str = "M4D_GPU_PERFORMANCE_GATE_V1 ";
const EXPECTED_COMPONENT_MEASUREMENTS: usize = 39;
const EXPECTED_WARMUPS: u64 = 30;
const EXPECTED_SAMPLES: usize = 120;
const CALIBRATION_RUNS: usize = 10;
const COMPARISON_PAIRS: [(RevisionRole, RevisionRole); 3] = [
    (RevisionRole::Baseline, RevisionRole::Candidate),
    (RevisionRole::Candidate, RevisionRole::Baseline),
    (RevisionRole::Baseline, RevisionRole::Candidate),
];
const COMPONENT_ABSOLUTE_NS: u64 = 33_300_000;
const COMPONENT_RELATIVE_NUMERATOR: u64 = 105;
const COMPONENT_RELATIVE_DENOMINATOR: u64 = 100;
const COMPONENT_STABILITY_LIMIT: f64 = 0.025;
const PRODUCT_RELATIVE_NUMERATOR: u64 = 110;
const PRODUCT_RELATIVE_DENOMINATOR: u64 = 100;
const PRODUCT_STABILITY_LIMIT: f64 = 0.05;
const ONE_MILLISECOND_NS: u64 = 1_000_000;
const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const MAX_BASELINE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PRIVATE_REPORT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_COMPONENT_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_PRODUCT_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const PUBLIC_REPORT_PATH: &str = "target/mirante4d/gpu-performance/gpu-performance-report.json";
const BASELINE_RELATIVE_PATH: &str = "verification/gpu-performance-baseline.json";
const PRODUCT_SCENARIO: &str = "representative_gpu_interaction";
const PACKAGE_MANIFEST_ROOT_RELATIVE_PATH: &str = "m4d/manifest/root.json";
const EXPECTED_PRODUCT_MEASUREMENTS: usize = 8;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Calibrate,
    Compare,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum RevisionRole {
    Baseline,
    Candidate,
}

impl RevisionRole {
    const fn name(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Candidate => "candidate",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema: String,
    schema_version: u32,
    measurement_version: String,
    operation: Operation,
    baseline: RevisionConfig,
    candidate: Option<RevisionConfig>,
    private_output_directory: PathBuf,
    raw_report: PathBuf,
    representative_package: PathBuf,
    representative_package_manifest_sha256: String,
    workload_profile_id: String,
    presentation_probe_receipt: PathBuf,
    supersedes_baseline_id: Option<String>,
    environment: EnvironmentConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RevisionConfig {
    checkout: PathBuf,
    target_directory: PathBuf,
    revision: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct EnvironmentConfig {
    platform: String,
    kernel_release: String,
    release_profile: String,
    adapter: String,
    backend: String,
    driver: String,
    display_output: String,
    display_profile_id: String,
    compositor_session_id: String,
    display_refresh_millihz: u64,
    target_width_pixels: u32,
    target_height_pixels: u32,
    power_policy_id: String,
    ac_online_path: PathBuf,
    ac_online_expected: String,
    power_policy_path: PathBuf,
    power_policy_expected: String,
    thermal_status_path: PathBuf,
    thermal_status_expected: String,
    quiescence_policy_id: String,
    cargo_lock_sha256: String,
    presentation_authority: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct MetricSample {
    metric_id: String,
    class: MetricClass,
    value_ns: u64,
    absolute_limit_ns: u64,
    work_identity: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum MetricClass {
    Component,
    Product,
}

#[derive(Debug, Clone, Copy)]
enum CampaignDisposition {
    Fail,
    Invalid,
    Unevaluated,
}

#[derive(Debug)]
struct CampaignBoundaryError {
    disposition: CampaignDisposition,
    message: String,
}

impl std::fmt::Display for CampaignBoundaryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CampaignBoundaryError {}

fn boundary_error(
    disposition: CampaignDisposition,
    context: &str,
    error: impl std::fmt::Display,
) -> anyhow::Error {
    CampaignBoundaryError {
        disposition,
        message: format!("{context}: {error}"),
    }
    .into()
}

fn campaign_status(error: &anyhow::Error) -> &'static str {
    match error
        .downcast_ref::<CampaignBoundaryError>()
        .map(|error| error.disposition)
        .unwrap_or(CampaignDisposition::Fail)
    {
        CampaignDisposition::Fail => "fail",
        CampaignDisposition::Invalid => "invalid",
        CampaignDisposition::Unevaluated => "unevaluated",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunRecord {
    sequence: usize,
    pair_index: Option<usize>,
    role: RevisionRole,
    revision: String,
    component_measurements: Vec<Value>,
    component_gate: Value,
    product_validation_status: String,
    presentation_authority_status: String,
    exact_cadence_status: String,
    product_script_contract_sha256: String,
    product_automation_report_sha256: String,
    presentation_report_sha256: String,
    product_structural_evidence: Value,
    metrics: Vec<MetricSample>,
    duration_ms: u64,
}

#[derive(Debug, Serialize)]
struct RawReport<'a> {
    schema: &'static str,
    schema_version: u32,
    measurement_version: &'static str,
    status: &'a str,
    visibility_and_stall_status: &'a str,
    exact_cadence_status: &'a str,
    operation: Operation,
    started_at_epoch_ms: u128,
    completed_at_epoch_ms: u128,
    workload_profile_id: &'a str,
    environment: &'a EnvironmentConfig,
    baseline_revision: &'a str,
    candidate_revision: Option<&'a str>,
    runs: &'a [RunRecord],
    evaluation: &'a Value,
    failure: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BaselineManifest {
    schema: String,
    schema_version: u32,
    measurement_version: String,
    status: String,
    baseline_id: String,
    accepted_revision: Option<String>,
    environment: Option<BaselineEnvironment>,
    workload_profile_id: Option<String>,
    metrics: BTreeMap<String, BaselineMetric>,
    accepted_at: Option<String>,
    owner_acceptance: Option<String>,
    supersedes: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct BaselineEnvironment {
    platform: String,
    kernel_release: String,
    release_profile: String,
    adapter: String,
    backend: String,
    driver: String,
    display_output: String,
    display_profile_id: String,
    compositor_session_id: String,
    display_refresh_millihz: u64,
    target_width_pixels: u32,
    target_height_pixels: u32,
    power_policy_id: String,
    quiescence_policy_id: String,
    cargo_lock_sha256: String,
    presentation_authority: String,
}

impl From<&EnvironmentConfig> for BaselineEnvironment {
    fn from(value: &EnvironmentConfig) -> Self {
        Self {
            platform: value.platform.clone(),
            kernel_release: value.kernel_release.clone(),
            release_profile: value.release_profile.clone(),
            adapter: value.adapter.clone(),
            backend: value.backend.clone(),
            driver: value.driver.clone(),
            display_output: value.display_output.clone(),
            display_profile_id: value.display_profile_id.clone(),
            compositor_session_id: value.compositor_session_id.clone(),
            display_refresh_millihz: value.display_refresh_millihz,
            target_width_pixels: value.target_width_pixels,
            target_height_pixels: value.target_height_pixels,
            power_policy_id: value.power_policy_id.clone(),
            quiescence_policy_id: value.quiescence_policy_id.clone(),
            cargo_lock_sha256: value.cargo_lock_sha256.clone(),
            presentation_authority: value.presentation_authority.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BaselineMetric {
    class: MetricClass,
    baseline_value_ns: u64,
    calibration_min_ns: u64,
    calibration_max_ns: u64,
    maximum_proportional_deviation: f64,
    stability_limit: f64,
    relative_numerator: u64,
    relative_denominator: u64,
    absolute_limit_ns: u64,
    work_identity: Value,
}

pub(crate) fn run(args: Vec<String>) -> anyhow::Result<PathBuf> {
    let config_path = parse_args(args)?;
    let repository_root = repository_root()?;
    let private = read_finalized_private_file(
        &config_path,
        &repository_root,
        MAX_CONFIG_BYTES,
        "GPU performance private configuration",
    )?;
    let config: Config = serde_json::from_slice(&private.bytes)
        .context("GPU performance private configuration is not valid JSON")?;
    validate_config(&config, &repository_root)?;
    ensure_nextest()?;

    let started_at_epoch_ms = epoch_ms();
    let mut runs = Vec::new();
    let campaign = execute_campaign(&config, &repository_root, &mut runs);
    let (status, visibility_status, exact_cadence_status, evaluation, failure) = match campaign {
        Ok(evaluation) => ("pass", "pass", "pass", evaluation, None),
        Err(error) => {
            let status = campaign_status(&error);
            (
                status,
                "not_passed",
                status,
                json!({ "result": status, "reason": error.to_string() }),
                Some(error.to_string()),
            )
        }
    };
    let completed_at_epoch_ms = epoch_ms();
    let candidate_revision = config
        .candidate
        .as_ref()
        .map(|value| value.revision.as_str());
    let raw = RawReport {
        schema: RAW_REPORT_SCHEMA,
        schema_version: REPORT_VERSION,
        measurement_version: MEASUREMENT_VERSION,
        status,
        visibility_and_stall_status: visibility_status,
        exact_cadence_status,
        operation: config.operation,
        started_at_epoch_ms,
        completed_at_epoch_ms,
        workload_profile_id: &config.workload_profile_id,
        environment: &config.environment,
        baseline_revision: &config.baseline.revision,
        candidate_revision,
        runs: &runs,
        evaluation: &evaluation,
        failure: failure.as_deref(),
    };
    let raw_bytes = serde_json::to_vec_pretty(&raw)?;
    write_new_synced_private_file(
        &config.raw_report,
        &raw_bytes,
        MAX_PRIVATE_REPORT_BYTES,
        "GPU performance private raw report",
    )?;

    let public_path = repository_root.join(PUBLIC_REPORT_PATH);
    write_json_file(
        &public_path,
        &json!({
            "schema": PUBLIC_REPORT_SCHEMA,
            "schema_version": REPORT_VERSION,
            "measurement_version": MEASUREMENT_VERSION,
            "status": status,
            "visibility_and_stall_status": visibility_status,
            "exact_cadence_status": exact_cadence_status,
            "operation": config.operation,
            "baseline_revision": config.baseline.revision,
            "candidate_revision": candidate_revision,
            "workload_profile_id": config.workload_profile_id,
            "platform": config.environment.platform,
            "kernel_release": config.environment.kernel_release,
            "release_profile": config.environment.release_profile,
            "adapter": config.environment.adapter,
            "backend": config.environment.backend,
            "driver": config.environment.driver,
            "display_output": config.environment.display_output,
            "display_profile_id": config.environment.display_profile_id,
            "compositor_session_id": config.environment.compositor_session_id,
            "display_refresh_millihz": config.environment.display_refresh_millihz,
            "target_extent": [config.environment.target_width_pixels, config.environment.target_height_pixels],
            "power_policy_id": config.environment.power_policy_id,
            "quiescence_policy_id": config.environment.quiescence_policy_id,
            "cargo_lock_sha256": config.environment.cargo_lock_sha256,
            "presentation_authority": config.environment.presentation_authority,
            "zero_retries": true,
            "run_count": runs.len(),
            "evaluation": evaluation,
            "private_raw_report_sha256": Sha256Hasher::digest(&raw_bytes).to_string(),
            "private_paths_published": false,
        }),
    )?;

    if let Some(failure) = failure {
        bail!(
            "GPU performance campaign is {status}: {failure}; reports: {} and {}",
            public_path.display(),
            config.raw_report.display()
        );
    }
    Ok(public_path)
}

fn parse_args(args: Vec<String>) -> anyhow::Result<PathBuf> {
    if args.len() != 2 || args[0] != "--config" {
        bail!("usage: cargo xtask gpu-performance --config /absolute/private/config.json");
    }
    Ok(PathBuf::from(&args[1]))
}

fn execute_campaign(
    config: &Config,
    repository_root: &Path,
    runs: &mut Vec<RunRecord>,
) -> anyhow::Result<Value> {
    guard_environment(config).map_err(|error| {
        boundary_error(
            CampaignDisposition::Invalid,
            "trusted workstation qualification failed",
            error,
        )
    })?;
    qualify_revision(&config.baseline, &config.environment).map_err(|error| {
        boundary_error(
            CampaignDisposition::Invalid,
            "baseline revision qualification failed",
            error,
        )
    })?;
    if let Some(candidate) = config.candidate.as_ref() {
        qualify_revision(candidate, &config.environment).map_err(|error| {
            boundary_error(
                CampaignDisposition::Invalid,
                "candidate revision qualification failed",
                error,
            )
        })?;
    }
    build_revision(&config.baseline)?;
    if let Some(candidate) = config.candidate.as_ref() {
        build_revision(candidate)?;
    }
    ensure_presentation_probe(config, repository_root).map_err(|error| {
        boundary_error(
            CampaignDisposition::Unevaluated,
            "presentation-boundary activation is unavailable",
            error,
        )
    })?;

    let registry = registry::read_registry()?;
    let (selector, pair_timeout_secs) = registry::gpu_performance_policy(&registry)?;
    match config.operation {
        Operation::Calibrate => {
            for sequence in 0..CALIBRATION_RUNS {
                let deadline = Instant::now() + Duration::from_secs(pair_timeout_secs);
                runs.push(run_revision(
                    config,
                    &config.baseline,
                    RevisionRole::Baseline,
                    sequence,
                    None,
                    &selector,
                    deadline,
                )?);
            }
            calibration_evaluation(config, runs)
        }
        Operation::Compare => {
            let candidate = config
                .candidate
                .as_ref()
                .context("comparison requires a candidate revision")?;
            let baseline = read_accepted_baseline(config, repository_root).map_err(|error| {
                boundary_error(
                    CampaignDisposition::Unevaluated,
                    "accepted relative baseline is unavailable",
                    error,
                )
            })?;
            let mut sequence = 0;
            for (pair_index, pair) in COMPARISON_PAIRS.into_iter().enumerate() {
                let deadline = Instant::now() + Duration::from_secs(pair_timeout_secs);
                for role in [pair.0, pair.1] {
                    let revision = match role {
                        RevisionRole::Baseline => &config.baseline,
                        RevisionRole::Candidate => candidate,
                    };
                    runs.push(run_revision(
                        config,
                        revision,
                        role,
                        sequence,
                        Some(pair_index),
                        &selector,
                        deadline,
                    )?);
                    sequence += 1;
                }
            }
            comparison_evaluation(config, &baseline, runs)
        }
    }
}

fn run_revision(
    config: &Config,
    revision: &RevisionConfig,
    role: RevisionRole,
    sequence: usize,
    pair_index: Option<usize>,
    selector: &str,
    deadline: Instant,
) -> anyhow::Result<RunRecord> {
    guard_environment(config).map_err(|error| {
        boundary_error(
            CampaignDisposition::Invalid,
            "pre-run environment qualification failed",
            error,
        )
    })?;
    let started = Instant::now();
    let wall_started = SystemTime::now();
    let measured = (|| {
        let component = run_component_benchmarks(config, revision, selector, deadline)?;
        let product = run_product_measurement(config, revision, role, sequence, deadline)?;
        Ok::<_, anyhow::Error>((component, product))
    })();
    guard_environment(config).map_err(|error| {
        boundary_error(
            CampaignDisposition::Invalid,
            "post-run environment qualification failed",
            error,
        )
    })?;
    let monotonic_elapsed = started.elapsed();
    let wall_elapsed = wall_started.elapsed().map_err(|error| {
        boundary_error(
            CampaignDisposition::Invalid,
            "wall clock moved backwards during a run",
            error,
        )
    })?;
    if wall_elapsed > monotonic_elapsed.saturating_add(Duration::from_secs(2)) {
        return Err(boundary_error(
            CampaignDisposition::Invalid,
            "run clock qualification failed",
            "wall time advanced more than two seconds beyond monotonic time; suspend or clock discontinuity is possible",
        ));
    }
    let (component, product) = measured?;
    let mut metrics = component
        .measurements
        .iter()
        .map(component_metric)
        .collect::<anyhow::Result<Vec<_>>>()?;
    metrics.extend(product.metrics);
    ensure_unique_metric_ids(&metrics)?;
    Ok(RunRecord {
        sequence,
        pair_index,
        role,
        revision: revision.revision.clone(),
        component_measurements: component.measurements,
        component_gate: component.gate,
        product_validation_status: product.validation_status,
        presentation_authority_status: product.presentation_status,
        exact_cadence_status: product.exact_cadence_status,
        product_script_contract_sha256: product.script_contract_sha256,
        product_automation_report_sha256: product.automation_report_sha256,
        presentation_report_sha256: product.presentation_report_sha256,
        product_structural_evidence: product.structural_evidence,
        metrics,
        duration_ms: duration_ms(monotonic_elapsed),
    })
}

struct ComponentRun {
    measurements: Vec<Value>,
    gate: Value,
}

fn run_component_benchmarks(
    config: &Config,
    revision: &RevisionConfig,
    selector: &str,
    deadline: Instant,
) -> anyhow::Result<ComponentRun> {
    let remaining = remaining(deadline, "component benchmark")?;
    let mut command = cargo_for_revision(revision);
    command.args([
        "nextest",
        "run",
        "--workspace",
        "--release",
        "--frozen",
        "--profile",
        "gpu-performance",
        "--run-ignored",
        "only",
        "--no-fail-fast",
        "--retries",
        "0",
        "--flaky-result",
        "fail",
        "--no-tests",
        "fail",
        "--success-output",
        "immediate",
        "--failure-output",
        "immediate",
        "--no-output-indent",
        "-E",
        selector,
    ]);
    command.env(
        "MIRANTE4D_TRUSTED_GPU_ADAPTER_NAME",
        &config.environment.adapter,
    );
    let output = run_command_with_bounded_output(
        &mut command,
        BoundedOutputPolicy {
            scope: "gpu_component",
            inactivity_timeout: remaining,
            absolute_timeout: remaining,
            progress_interval: Duration::from_secs(30),
            max_stdout_bytes: MAX_COMPONENT_OUTPUT_BYTES,
            max_stderr_bytes: MAX_COMPONENT_OUTPUT_BYTES,
        },
    )?;
    if !output.status.success() {
        bail!("component benchmark process failed with {}", output.status);
    }
    let mut bytes = output.stdout;
    bytes.push(b'\n');
    bytes.extend_from_slice(&output.stderr);
    parse_component_output(&bytes, &config.environment)
}

fn parse_component_output(
    bytes: &[u8],
    environment: &EnvironmentConfig,
) -> anyhow::Result<ComponentRun> {
    let output = String::from_utf8(bytes.to_vec()).context("component output is not UTF-8")?;
    let mut measurements = Vec::new();
    let mut gates = Vec::new();
    for line in output.lines() {
        if let Some(offset) = line.find(COMPONENT_MARKER) {
            measurements.push(
                serde_json::from_str::<Value>(&line[offset + COMPONENT_MARKER.len()..])
                    .context("component measurement marker is invalid JSON")?,
            );
        }
        if let Some(offset) = line.find(COMPONENT_GATE_MARKER) {
            gates.push(
                serde_json::from_str::<Value>(&line[offset + COMPONENT_GATE_MARKER.len()..])
                    .context("component gate marker is invalid JSON")?,
            );
        }
    }
    if measurements.len() != EXPECTED_COMPONENT_MEASUREMENTS {
        bail!(
            "component run emitted {} measurements, expected {EXPECTED_COMPONENT_MEASUREMENTS}",
            measurements.len()
        );
    }
    if gates.len() != 1 {
        bail!(
            "component run emitted {} shape gates, expected one",
            gates.len()
        );
    }
    let mut ids = BTreeSet::new();
    let mut families = BTreeMap::<&str, usize>::new();
    for measurement in &measurements {
        let object = measurement
            .as_object()
            .context("component measurement must be an object")?;
        let id = required_string(object, "measurement_id", "component measurement")?;
        if !ids.insert(id.to_owned()) {
            bail!("duplicate component measurement {id:?}");
        }
        let family = required_string(object, "family", "component measurement")?;
        *families.entry(family).or_default() += 1;
        if required_string(object, "adapter", "component measurement")? != environment.adapter
            || required_string(object, "backend", "component measurement")? != environment.backend
            || required_string(object, "driver", "component measurement")? != environment.driver
        {
            bail!("component environment differs from the pinned configuration");
        }
        let warmups = object
            .get("warmups")
            .or_else(|| object.get("case_warmups"))
            .and_then(Value::as_u64)
            .context("component measurement omitted warmups")?;
        if warmups != EXPECTED_WARMUPS
            || required_u64(object, "samples", "component measurement")? != EXPECTED_SAMPLES as u64
            || required_u64(object, "uploads_during_samples", "component measurement")? != 0
            || required_u64(object, "validation_errors", "component measurement")? != 0
            || object.get("absolute_met").and_then(Value::as_bool) != Some(true)
            || required_u64(object, "absolute_limit_ns", "component measurement")?
                != COMPONENT_ABSOLUTE_NS
        {
            bail!("component measurement {id:?} violated its fixed work or absolute contract");
        }
        let raw = object
            .get("raw_render_pass_ns")
            .and_then(Value::as_array)
            .context("component measurement omitted its raw vector")?;
        if raw.len() != EXPECTED_SAMPLES || raw.iter().any(|value| value.as_u64().is_none()) {
            bail!("component measurement {id:?} has an invalid raw vector");
        }
        let recomputed = nearest_rank(
            &raw.iter()
                .map(|value| value.as_u64().unwrap())
                .collect::<Vec<_>>(),
            95,
        )?;
        if recomputed != required_u64(object, "p95_ns", "component measurement")? {
            bail!("component measurement {id:?} reports a non-reproducible p95");
        }
        if nearest_rank(
            &raw.iter()
                .map(|value| value.as_u64().unwrap())
                .collect::<Vec<_>>(),
            50,
        )? != required_u64(object, "median_ns", "component measurement")?
        {
            bail!("component measurement {id:?} reports a non-reproducible median");
        }
        if family == "fixed_lod_multichannel" {
            for (raw_field, summary_field) in [
                ("raw_cpu_planning_ns", "cpu_planning_p95_ns"),
                ("raw_cpu_submit_ns", "cpu_submit_p95_ns"),
            ] {
                let values = object
                    .get(raw_field)
                    .and_then(Value::as_array)
                    .with_context(|| format!("component measurement {id:?} omitted {raw_field}"))?;
                if values.len() != EXPECTED_SAMPLES
                    || values.iter().any(|value| value.as_u64().is_none())
                    || nearest_rank(
                        &values
                            .iter()
                            .map(|value| value.as_u64().unwrap())
                            .collect::<Vec<_>>(),
                        95,
                    )? != required_u64(object, summary_field, "component measurement")?
                {
                    bail!("component measurement {id:?} has invalid {raw_field}");
                }
            }
        }
    }
    if families
        != BTreeMap::from([
            ("fixed_lod_multichannel", 32),
            ("native_terminal", 6),
            ("resident_coordinated", 1),
        ])
    {
        bail!("component measurement topology drifted: {families:?}");
    }
    let gate = gates.pop().unwrap();
    if gate.get("gate").and_then(Value::as_str) != Some("fixed_lod_homogeneous_linear_ratio")
        || gate.get("threshold").and_then(Value::as_f64) != Some(1.2)
        || gate.get("met").and_then(Value::as_bool) != Some(true)
    {
        bail!("fixed-LOD scaling-shape gate failed or drifted");
    }
    Ok(ComponentRun { measurements, gate })
}

fn component_metric(value: &Value) -> anyhow::Result<MetricSample> {
    let object = value
        .as_object()
        .context("component measurement must be an object")?;
    let metric_id = required_string(object, "measurement_id", "component measurement")?.to_owned();
    Ok(MetricSample {
        metric_id,
        class: MetricClass::Component,
        value_ns: required_u64(object, "p95_ns", "component measurement")?,
        absolute_limit_ns: COMPONENT_ABSOLUTE_NS,
        work_identity: component_work_identity(object)?,
    })
}

fn component_work_identity(object: &serde_json::Map<String, Value>) -> anyhow::Result<Value> {
    let family = required_string(object, "family", "component measurement")?;
    let fields: &[&str] = match family {
        "native_terminal" => &["family", "shape", "extent", "mode", "samples"],
        "fixed_lod_multichannel" => &[
            "family",
            "shape",
            "extent",
            "kernel",
            "sampling",
            "compatibility",
            "channels",
            "resources",
            "payload_bytes",
            "samples",
        ],
        "resident_coordinated" => &["family", "workload", "extent", "samples"],
        _ => bail!("unknown component family {family:?}"),
    };
    let mut identity = serde_json::Map::new();
    for field in fields {
        identity.insert(
            (*field).to_owned(),
            object
                .get(*field)
                .with_context(|| format!("component work identity omitted {field}"))?
                .clone(),
        );
    }
    Ok(Value::Object(identity))
}

struct ProductRun {
    validation_status: String,
    presentation_status: String,
    exact_cadence_status: String,
    script_contract_sha256: String,
    automation_report_sha256: String,
    presentation_report_sha256: String,
    structural_evidence: Value,
    metrics: Vec<MetricSample>,
}

fn run_product_measurement(
    config: &Config,
    revision: &RevisionConfig,
    role: RevisionRole,
    sequence: usize,
    deadline: Instant,
) -> anyhow::Result<ProductRun> {
    let remaining = remaining(deadline, "representative product measurement")?;
    let presentation_path = config
        .private_output_directory
        .join(format!("presentation-{}-{sequence}.json", role.name()));
    if presentation_path.exists() {
        bail!("presentation output already exists; refusing to overwrite private evidence");
    }
    let xtask = release_binary(&revision.target_directory, "xtask");
    let app = release_binary(&revision.target_directory, "mirante4d-app");
    let validation_root = revision
        .target_directory
        .join("mirante4d/product-validation");
    let mut command = Command::new(&xtask);
    command
        .current_dir(&revision.checkout)
        .args([
            "product-validate",
            config
                .representative_package
                .to_str()
                .context("representative package path is not UTF-8")?,
            PRODUCT_SCENARIO,
        ])
        .env("MIRANTE4D_PRODUCT_VALIDATE_APP_BINARY", &app)
        .env("MIRANTE4D_PRODUCT_VALIDATE_SKIP_RELEASE_BUILD", "1")
        .env("MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS", "real_display")
        .env("MIRANTE4D_PRODUCT_VALIDATE_OUTPUT_DIR", &validation_root)
        .env(
            "MIRANTE4D_PRODUCT_VALIDATE_TIMEOUT_SECS",
            remaining.as_secs().to_string(),
        )
        .env("MIRANTE4D_PRESENTATION_OBSERVER_REPORT", &presentation_path)
        .env(
            "MIRANTE4D_TRUSTED_GPU_ADAPTER_NAME",
            &config.environment.adapter,
        );
    let output = run_command_with_bounded_output(
        &mut command,
        BoundedOutputPolicy {
            scope: "gpu_product",
            inactivity_timeout: remaining,
            absolute_timeout: remaining,
            progress_interval: Duration::from_secs(30),
            max_stdout_bytes: MAX_PRODUCT_OUTPUT_BYTES,
            max_stderr_bytes: MAX_PRODUCT_OUTPUT_BYTES,
        },
    )?;
    if !output.status.success() {
        bail!(
            "representative product process failed with {}",
            output.status
        );
    }
    let output_directory = validation_root.join(PRODUCT_SCENARIO);
    let wrapper_path = output_directory.join("product-validation-report.json");
    let script_path = output_directory.join("product-automation-script.json");
    let automation_report_path = output_directory.join("product-automation-report.json");
    let wrapper = read_json_file(&wrapper_path)?;
    if wrapper.get("status").and_then(Value::as_str) != Some("passed") {
        bail!("representative product validation did not pass");
    }
    let script = read_json_file(&script_path)?;
    crate::product_validate::validate_product_automation_script(&script)?;
    validate_product_script_contract(&script)?;
    let automation = read_json_file(&automation_report_path)?;
    crate::product_validate::validate_product_automation_report_contract(
        &automation,
        &script,
        &script_path,
    )?;
    let structural_evidence = validate_product_structural_evidence(&automation)?;
    let script_contract_sha256 = sanitized_product_script_sha256(&script)?;
    let automation_bytes = fs::read(&automation_report_path)
        .with_context(|| format!("failed to read {}", automation_report_path.display()))?;
    if automation_bytes.is_empty() || automation_bytes.len() > MAX_PRIVATE_REPORT_BYTES as usize {
        bail!("product automation report is empty or exceeds its bound");
    }
    let private_presentation = read_finalized_private_file(
        &presentation_path,
        &revision.checkout,
        MAX_PRIVATE_REPORT_BYTES,
        "presentation observer report",
    )?;
    let presentation: Value = serde_json::from_slice(&private_presentation.bytes)
        .context("presentation observer report is invalid JSON")?;
    let product_work_identity = json!({
        "workload_profile_id": config.workload_profile_id,
        "script_contract_sha256": script_contract_sha256,
        "scenario": PRODUCT_SCENARIO,
        "mapped_client_extent": [
            config.environment.target_width_pixels,
            config.environment.target_height_pixels,
        ],
        "layouts": ["single3d", "four_panel"],
        "modes": ["mip", "dvr", "iso", "mixed", "pick"],
        "sampling": ["voxel_exact", "smooth_linear"],
        "cache_conditions": [
            "fresh_application_warm_os_cache",
            "resident",
            "cached_or_prepared_nonresident",
        ],
        "scientific_work_changes_forbidden": true,
        "presentation_measurement_scope": [
            "marker_qualified_visibility",
            "first_pixel_out_scanout_cadence",
            "maximum_active_visible_gap_host_upper_bound",
            "coarse_exact_settlement_host_upper_bound",
            "resident_input_response_host_upper_bound",
        ],
        "exact_cadence_evaluated": true,
        "physical_photon_visibility_measured": false,
    });
    let metrics = parse_product_metrics(&presentation, config, revision, &product_work_identity)?;
    Ok(ProductRun {
        validation_status: "pass".to_owned(),
        presentation_status: "pass".to_owned(),
        exact_cadence_status: "pass".to_owned(),
        script_contract_sha256,
        automation_report_sha256: Sha256Hasher::digest(&automation_bytes).to_string(),
        presentation_report_sha256: Sha256Hasher::digest(&private_presentation.bytes).to_string(),
        structural_evidence,
        metrics,
    })
}

fn parse_product_metrics(
    report: &Value,
    config: &Config,
    revision: &RevisionConfig,
    product_work_identity: &Value,
) -> anyhow::Result<Vec<MetricSample>> {
    match report.get("status").and_then(Value::as_str) {
        Some("invalid") => {
            return Err(boundary_error(
                CampaignDisposition::Invalid,
                "presentation observer invalidated the product session",
                report
                    .get("first_error")
                    .and_then(Value::as_str)
                    .unwrap_or("focus, occlusion, surface, clock, or event authority changed"),
            ));
        }
        Some("unevaluated") | None => {
            return Err(boundary_error(
                CampaignDisposition::Unevaluated,
                "presentation observer could not evaluate the product session",
                report
                    .get("first_error")
                    .and_then(Value::as_str)
                    .unwrap_or("required presentation evidence is incomplete"),
            ));
        }
        _ => {}
    }
    if report.get("schema").and_then(Value::as_str) != Some(PRESENTATION_REPORT_SCHEMA)
        || report.get("schema_version").and_then(Value::as_u64) != Some(PRESENTATION_REPORT_VERSION)
        || report.get("status").and_then(Value::as_str) != Some("pass")
        || report.get("authority").and_then(Value::as_str)
            != Some(config.environment.presentation_authority.as_str())
        || report
            .pointer("/environment/window_width_pixels")
            .and_then(Value::as_u64)
            != Some(u64::from(config.environment.target_width_pixels))
        || report
            .pointer("/environment/window_height_pixels")
            .and_then(Value::as_u64)
            != Some(u64::from(config.environment.target_height_pixels))
    {
        bail!("presentation observer report is unavailable, invalid, or incomparable");
    }
    validate_exact_presentation_evidence(report)?;
    let observed_environment = report
        .pointer("/environment/qualified_identity")
        .and_then(Value::as_object)
        .context("presentation report omitted its qualified renderer environment")?;
    if required_string(observed_environment, "adapter", "presentation environment")?
        != config.environment.adapter
        || required_string(observed_environment, "backend", "presentation environment")?
            != config.environment.backend
        || required_string(observed_environment, "driver", "presentation environment")?
            != config.environment.driver
        || required_string(
            observed_environment,
            "repository_revision",
            "presentation environment",
        )? != revision.revision
        || required_string(
            observed_environment,
            "build_profile",
            "presentation environment",
        )? != "release"
        || required_string(
            observed_environment,
            "target_mode",
            "presentation environment",
        )? != "trusted-gpu-performance"
    {
        bail!("presentation report renderer, build, or revision identity drifted");
    }
    let proof = report
        .get("boundary_proof")
        .and_then(Value::as_object)
        .context("presentation report omitted boundary proof")?;
    for field in [
        "marker_decode_after_present_only",
        "internal_publication_alone_cannot_advance",
        "target_extent_matched",
        "mapped_window_confirmed",
        "first_pixel_out_timing_observed",
        "first_pixel_out_correlated_to_marker",
        "timing_queue_drained",
        "stable_time_domain",
        "stable_timing_properties",
    ] {
        if proof.get(field).and_then(Value::as_bool) != Some(true) {
            bail!("presentation authority proof {field:?} is not established");
        }
    }
    let summaries = report
        .get("metrics")
        .and_then(Value::as_object)
        .context("presentation report omitted product metrics")?;
    let raw_vectors = summaries
        .get("raw_vectors")
        .and_then(Value::as_object)
        .context("presentation report omitted raw product vectors")?;
    let recomputed_standalone = recompute_scanout_intervals(report, "standalone_interaction")?;
    let recomputed_four_panel = recompute_scanout_intervals(report, "four_panel_interaction")?;
    let recomputed_input_response = recompute_resident_input_response(report)?;
    for (vector_id, recomputed) in [
        ("standalone_present_intervals_ns", &recomputed_standalone),
        ("four_panel_present_intervals_ns", &recomputed_four_panel),
        ("resident_input_response_ns", &recomputed_input_response),
    ] {
        let reported = required_u64_vector(raw_vectors, vector_id, "presentation raw vectors")?;
        if reported != *recomputed {
            bail!("presentation raw vector {vector_id:?} is not reproducible from raw records");
        }
    }
    for (metric_id, values, percentile) in [
        (
            "standalone_present_interval_p95_ns",
            &recomputed_standalone,
            95,
        ),
        (
            "four_panel_present_interval_p95_ns",
            &recomputed_four_panel,
            95,
        ),
        (
            "resident_input_response_p99_ns",
            &recomputed_input_response,
            99,
        ),
    ] {
        let recomputed = nearest_rank(values, percentile)?;
        if summaries.get(metric_id).and_then(Value::as_u64) != Some(recomputed) {
            bail!("product metric {metric_id:?} is not a reproducible nearest-rank summary");
        }
    }
    for (metric_id, vector_id) in [
        (
            "resident_exact_settlement_ns",
            "resident_exact_settlement_ns",
        ),
        (
            "prepared_nonresident_exact_replacement_ns",
            "prepared_nonresident_exact_replacement_ns",
        ),
        (
            "maximum_active_visible_gap_ns",
            "maximum_active_visible_gap_ns",
        ),
        ("startup_complete_coarse_ns", "startup_complete_coarse_ns"),
        ("startup_exact_settlement_ns", "startup_exact_settlement_ns"),
    ] {
        let values = raw_vectors
            .get(vector_id)
            .and_then(Value::as_array)
            .with_context(|| format!("presentation report omitted raw vector {vector_id}"))?;
        if values.is_empty() || values.iter().any(|value| value.as_u64().is_none()) {
            bail!("presentation raw vector {vector_id:?} is empty or invalid");
        }
        let values = values
            .iter()
            .map(|value| value.as_u64().unwrap())
            .collect::<Vec<_>>();
        let recomputed = *values.iter().max().unwrap();
        if summaries.get(metric_id).and_then(Value::as_u64) != Some(recomputed) {
            bail!("product metric {metric_id:?} is not reproducible from {vector_id:?}");
        }
    }
    let contracts = product_metric_contracts();
    let mut metrics = Vec::with_capacity(contracts.len());
    for (metric_id, absolute_limit_ns) in contracts {
        let value_ns = summaries
            .get(metric_id)
            .and_then(Value::as_u64)
            .with_context(|| format!("presentation report omitted {metric_id}"))?;
        if value_ns > absolute_limit_ns {
            bail!(
                "product metric {metric_id} measured {value_ns} ns, exceeding {absolute_limit_ns} ns"
            );
        }
        metrics.push(MetricSample {
            metric_id: metric_id.to_owned(),
            class: MetricClass::Product,
            value_ns,
            absolute_limit_ns,
            work_identity: product_work_identity.clone(),
        });
    }
    Ok(metrics)
}

fn validate_exact_presentation_evidence(report: &Value) -> anyhow::Result<()> {
    if report
        .get("presentation_visibility_claimed")
        .and_then(Value::as_bool)
        != Some(true)
        || report.get("exact_cadence_claimed").and_then(Value::as_bool) != Some(true)
        || report
            .get("scanout_cadence_claimed")
            .and_then(Value::as_bool)
            != Some(true)
        || report
            .get("click_to_photon_claimed")
            .and_then(Value::as_bool)
            != Some(false)
        || report
            .pointer("/exact_cadence/status")
            .and_then(Value::as_str)
            != Some("pass")
        || report
            .pointer("/exact_cadence/metric")
            .and_then(Value::as_str)
            != Some("first_pixel_out_scanout_cadence")
        || report
            .pointer("/exact_cadence/physical_photon_visibility_claimed")
            .and_then(Value::as_bool)
            != Some(false)
    {
        bail!("presentation report misstated its first-pixel-out measurement scope");
    }
    if report
        .pointer("/clocks/coarse_visibility")
        .and_then(Value::as_str)
        != Some("std_instant_monotonic_vulkan_present_wait_completion")
        || report
            .pointer("/clocks/exact_scanout_cadence")
            .and_then(Value::as_str)
            != Some("vulkan_ext_present_timing_image_first_pixel_out")
        || report
            .pointer("/clocks/cross_clock_subtraction_permitted")
            .and_then(Value::as_bool)
            != Some(false)
    {
        bail!("presentation report omitted or mixed its declared clock authorities");
    }

    let submitted = report
        .pointer("/event_counts/vulkan_present_submitted")
        .and_then(Value::as_u64)
        .context("presentation report omitted submitted-present count")?;
    if submitted == 0
        || report
            .pointer("/event_counts/vulkan_present_wait_completed")
            .and_then(Value::as_u64)
            != Some(submitted)
        || report
            .pointer("/event_counts/vulkan_present_timing_completed")
            .and_then(Value::as_u64)
            != Some(submitted)
        || report
            .pointer("/event_counts/vulkan_timing_configured_swapchains")
            .and_then(Value::as_u64)
            .is_none_or(|count| count == 0)
    {
        bail!("presentation report did not completely drain all submitted presents");
    }
    let rejected = report
        .pointer("/event_counts/vulkan_present_rejected")
        .and_then(Value::as_u64)
        .context("presentation report omitted rejected-present count")?;
    let out_of_date = report
        .pointer("/event_counts/vulkan_present_rejected_out_of_date")
        .and_then(Value::as_u64)
        .context("presentation report omitted out-of-date present count")?;
    if rejected != out_of_date
        || report
            .pointer("/event_counts/vulkan_present_rejected_fatal")
            .and_then(Value::as_u64)
            != Some(0)
    {
        bail!("presentation report contains a fatal or unclassified queue-present rejection");
    }
    for field in [
        "vulkan_present_rejected_fatal",
        "vulkan_present_wait_timeout",
        "vulkan_present_wait_failure",
        "vulkan_present_timing_incomplete",
        "vulkan_present_timing_duplicate",
        "vulkan_present_timing_unknown_id",
        "vulkan_present_timing_out_of_order",
        "vulkan_present_timing_zero_or_missing_stage",
        "vulkan_present_timing_zero_time",
        "vulkan_present_timing_failure",
        "vulkan_present_timing_timeout",
        "vulkan_present_timing_queue_full",
        "vulkan_timing_properties_changes",
        "vulkan_time_domain_changes",
        "ambiguous_completion",
        "pending_at_close",
        "present_wait_bindings_at_close",
        "present_timing_bindings_at_close",
        "early_present_timing_records_at_close",
        "present_timing_records_at_close",
        "marker_outcomes_at_close",
        "dropped_records",
    ] {
        if report
            .pointer(&format!("/event_counts/{field}"))
            .and_then(Value::as_u64)
            != Some(0)
        {
            bail!("presentation report has a nonzero failure or drain counter {field:?}");
        }
    }

    if report
        .pointer("/present_timing/extension")
        .and_then(Value::as_str)
        != Some("VK_EXT_present_timing")
        || report
            .pointer("/present_timing/extension_revision")
            .and_then(Value::as_u64)
            != Some(3)
        || report
            .pointer("/present_timing/stage")
            .and_then(Value::as_str)
            != Some("VK_PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT_BIT_EXT")
        || report
            .pointer("/present_timing/queue_size")
            .and_then(Value::as_u64)
            != Some(256)
    {
        bail!("presentation report omitted the required revision-3 timing capability");
    }
    let history = report
        .pointer("/present_timing/configured_swapchain_history")
        .and_then(Value::as_array)
        .context("presentation report omitted swapchain timing capability history")?;
    if history.is_empty() {
        bail!("presentation report has no configured timing swapchain");
    }
    let mut configured_domains = BTreeMap::new();
    for entry in history {
        let entry = entry
            .as_object()
            .context("presentation swapchain timing history entry is not an object")?;
        let generation = required_u64(entry, "generation", "swapchain timing history")?;
        let time_domain = entry
            .get("time_domain")
            .and_then(Value::as_i64)
            .context("swapchain timing history omitted its time-domain kind")?;
        let time_domain_id = required_u64(entry, "time_domain_id", "swapchain timing history")?;
        if generation == 0
            || entry.get("present_id2_supported").and_then(Value::as_bool) != Some(true)
            || entry
                .get("present_timing_supported")
                .and_then(Value::as_bool)
                != Some(true)
            || required_u64(entry, "present_stage_queries", "swapchain timing history")? & 4 == 0
            || configured_domains
                .insert(generation, (time_domain, time_domain_id))
                .is_some()
        {
            bail!("presentation swapchain timing capability history is invalid");
        }
    }

    let records = report
        .get("presented_records")
        .and_then(Value::as_array)
        .context("presentation report omitted marker-correlated timing records")?;
    if records.is_empty()
        || report
            .pointer("/event_counts/correct_changed_presented")
            .and_then(Value::as_u64)
            != Some(records.len() as u64)
    {
        bail!("presentation report has no complete marker-correlated timing records");
    }
    let mut previous_present_id = 0;
    let mut previous_marker = 0;
    let mut previous_segment = None;
    let mut observed_generations = BTreeSet::new();
    for record in records {
        let record = record
            .as_object()
            .context("presentation timing record is not an object")?;
        let present_id = required_u64(record, "present_id", "presentation timing record")?;
        let marker_sequence =
            required_u64(record, "marker_sequence", "presentation timing record")?;
        let generation =
            required_u64(record, "swapchain_generation", "presentation timing record")?;
        let first_pixel_out =
            required_u64(record, "first_pixel_out_ns", "presentation timing record")?;
        let time_domain = record
            .get("time_domain")
            .and_then(Value::as_i64)
            .context("presentation timing record omitted its time-domain kind")?;
        let time_domain_id = required_u64(record, "time_domain_id", "presentation timing record")?;
        if present_id <= previous_present_id
            || marker_sequence <= previous_marker
            || generation == 0
            || first_pixel_out == 0
            || configured_domains.get(&generation) != Some(&(time_domain, time_domain_id))
        {
            bail!("presentation timing records are zero, unordered, or cross-generation");
        }
        let segment = (generation, time_domain, time_domain_id);
        if let Some((previous, previous_time)) = previous_segment
            && previous == segment
            && first_pixel_out <= previous_time
        {
            bail!("first-pixel-out time did not advance inside one clock segment");
        }
        previous_present_id = present_id;
        previous_marker = marker_sequence;
        previous_segment = Some((segment, first_pixel_out));
        observed_generations.insert(generation);
    }
    let counters = report
        .pointer("/present_timing/per_swapchain_generation_counters")
        .and_then(Value::as_object)
        .context("presentation report omitted per-generation timing counters")?;
    for generation in observed_generations {
        let generation = generation.to_string();
        let entry = counters
            .get(&generation)
            .and_then(Value::as_object)
            .with_context(|| {
                format!("presentation report omitted timing counters for generation {generation}")
            })?;
        if entry
            .get("timing_properties_counter")
            .and_then(Value::as_u64)
            .is_none()
            || entry
                .get("time_domains_counter")
                .and_then(Value::as_u64)
                .is_none()
        {
            bail!("presentation timing generation {generation} omitted clock counters");
        }
    }
    Ok(())
}

fn recompute_scanout_intervals(report: &Value, phase: &str) -> anyhow::Result<Vec<u64>> {
    let transitions = report
        .get("state_transitions")
        .and_then(Value::as_array)
        .context("presentation report omitted state transitions")?;
    let mut active_start = None;
    let mut intervals = Vec::new();
    let mut previous_transition = 0;
    for transition in transitions {
        let transition = transition
            .as_object()
            .context("presentation state transition is not an object")?;
        let observed = required_u64(transition, "observed_at_ns", "state transition")?;
        if observed < previous_transition {
            bail!("presentation state transitions are out of order");
        }
        previous_transition = observed;
        let matches_active = required_string(transition, "phase", "state transition")? == phase
            && transition.get("active_input").and_then(Value::as_bool) == Some(true);
        if matches_active {
            active_start.get_or_insert(observed);
        } else if let Some(start) = active_start.take() {
            intervals.push((start, observed));
        }
    }
    if active_start.is_some() || intervals.is_empty() {
        bail!("presentation report has no closed active interval for {phase:?}");
    }

    let records = report
        .get("presented_records")
        .and_then(Value::as_array)
        .context("presentation report omitted timing records")?;
    let mut result = Vec::new();
    for (start, end) in intervals {
        let active = records
            .iter()
            .filter_map(Value::as_object)
            .filter(|record| {
                record.get("phase").and_then(Value::as_str) == Some(phase)
                    && record.get("active_input").and_then(Value::as_bool) == Some(true)
                    && record
                        .get("observed_at_ns")
                        .and_then(Value::as_u64)
                        .is_some_and(|observed| observed >= start && observed <= end)
            })
            .collect::<Vec<_>>();
        for pair in active.windows(2) {
            let left_segment = (
                required_u64(
                    pair[0],
                    "swapchain_generation",
                    "presentation timing record",
                )?,
                pair[0]
                    .get("time_domain")
                    .and_then(Value::as_i64)
                    .context("presentation timing record omitted time domain")?,
                required_u64(pair[0], "time_domain_id", "presentation timing record")?,
            );
            let right_segment = (
                required_u64(
                    pair[1],
                    "swapchain_generation",
                    "presentation timing record",
                )?,
                pair[1]
                    .get("time_domain")
                    .and_then(Value::as_i64)
                    .context("presentation timing record omitted time domain")?,
                required_u64(pair[1], "time_domain_id", "presentation timing record")?,
            );
            if left_segment != right_segment {
                continue;
            }
            let left = required_u64(pair[0], "first_pixel_out_ns", "presentation timing record")?;
            let right = required_u64(pair[1], "first_pixel_out_ns", "presentation timing record")?;
            let interval = right
                .checked_sub(left)
                .filter(|interval| *interval > 0)
                .context("first-pixel-out interval is zero or moved backwards")?;
            result.push(interval);
        }
    }
    if result.is_empty() {
        bail!("presentation report has no first-pixel-out intervals for {phase:?}");
    }
    Ok(result)
}

fn recompute_resident_input_response(report: &Value) -> anyhow::Result<Vec<u64>> {
    let records = report
        .get("presented_records")
        .and_then(Value::as_array)
        .context("presentation report omitted timing records")?;
    let mut first_for_generation = BTreeMap::new();
    for record in records {
        let record = record
            .as_object()
            .context("presentation timing record is not an object")?;
        if record.get("active_input").and_then(Value::as_bool) != Some(true)
            || !matches!(
                record.get("phase").and_then(Value::as_str),
                Some("standalone_interaction" | "four_panel_interaction")
            )
        {
            continue;
        }
        let generation = required_u64(record, "input_generation", "presentation timing record")?;
        let response = required_u64(
            record,
            "input_to_visible_coarse_ns",
            "presentation timing record",
        )?;
        if generation == 0 || response == 0 {
            bail!("resident input-response record is zero");
        }
        first_for_generation.entry(generation).or_insert(response);
    }
    if first_for_generation.is_empty() {
        bail!("presentation report has no resident input-response samples");
    }
    Ok(first_for_generation.into_values().collect())
}

fn required_u64_vector(
    object: &serde_json::Map<String, Value>,
    field: &str,
    context: &str,
) -> anyhow::Result<Vec<u64>> {
    let values = object
        .get(field)
        .and_then(Value::as_array)
        .with_context(|| format!("{context} omitted vector {field:?}"))?;
    if values.is_empty() {
        bail!("{context} vector {field:?} is empty");
    }
    values
        .iter()
        .map(|value| {
            value
                .as_u64()
                .filter(|value| *value > 0)
                .with_context(|| format!("{context} vector {field:?} contains an invalid value"))
        })
        .collect()
}

fn product_metric_contracts() -> BTreeMap<&'static str, u64> {
    BTreeMap::from([
        ("four_panel_present_interval_p95_ns", 33_300_000),
        ("maximum_active_visible_gap_ns", 100_000_000),
        ("prepared_nonresident_exact_replacement_ns", 1_000_000_000),
        ("resident_input_response_p99_ns", 50_000_000),
        ("resident_exact_settlement_ns", 250_000_000),
        ("standalone_present_interval_p95_ns", 33_300_000),
        ("startup_complete_coarse_ns", 250_000_000),
        ("startup_exact_settlement_ns", 2_000_000_000),
    ])
}

fn validate_product_script_contract(script: &Value) -> anyhow::Result<()> {
    if script.get("scenario").and_then(Value::as_str) != Some(PRODUCT_SCENARIO)
        || script.get("gpu_timing").and_then(Value::as_bool) != Some(true)
    {
        bail!("representative GPU script scenario or timing contract drifted");
    }
    let commands = script
        .get("commands")
        .and_then(Value::as_array)
        .context("representative GPU script omitted commands")?;
    let phases = commands
        .iter()
        .filter(|command| {
            command.get("command").and_then(Value::as_str) == Some("set_gpu_performance_phase")
        })
        .filter_map(|command| command.get("phase").and_then(Value::as_str))
        .collect::<Vec<_>>();
    if phases
        != [
            "startup",
            "standalone_interaction",
            "resident_settlement",
            "four_panel_interaction",
            "resident_settlement",
            "prepared_nonresident_replacement",
            "mode_sampling_matrix",
            "interrupted_refinement",
            "settled_idle",
        ]
    {
        bail!("representative GPU phase topology drifted: {phases:?}");
    }
    let checkpoints = commands
        .iter()
        .filter(|command| {
            command.get("command").and_then(Value::as_str) == Some("copy_diagnostics")
        })
        .map(|command| {
            command
                .get("checkpoint")
                .and_then(Value::as_str)
                .context("GPU diagnostics command omitted its checkpoint")
        })
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    let expected_checkpoints = BTreeSet::from([
        "resident_standalone_before",
        "resident_standalone_after",
        "resident_four_panel_before",
        "resident_four_panel_after",
        "prepared_nonresident_before",
        "prepared_nonresident_after",
        "interrupted_refinement_before",
        "interrupted_refinement_after",
        "settled_idle_before",
        "settled_idle_after",
    ]);
    if checkpoints != expected_checkpoints {
        bail!("representative GPU diagnostics checkpoint topology drifted");
    }
    let exact_sequence = |name: &str, samples: u64, duration_ms: u64| {
        commands
            .iter()
            .filter(|command| {
                command.get("command").and_then(Value::as_str) == Some(name)
                    && command.get("samples").and_then(Value::as_u64) == Some(samples)
                    && command.get("duration_ms").and_then(Value::as_u64) == Some(duration_ms)
            })
            .count()
    };
    if exact_sequence("camera_orbit_sequence", 900, 15_000) != 1
        || exact_sequence("camera_zoom_sequence", 900, 15_000) != 1
        || exact_sequence("cross_section_pan_sequence", 900, 15_000) != 1
        || exact_sequence("cross_section_rotate_sequence", 900, 15_000) != 1
    {
        bail!("representative GPU 30-second interaction workloads drifted");
    }
    let values_for = |command_name: &str, field: &str| {
        commands
            .iter()
            .filter(|command| command.get("command").and_then(Value::as_str) == Some(command_name))
            .filter_map(|command| command.get(field).and_then(Value::as_str))
            .collect::<BTreeSet<_>>()
    };
    if values_for("set_viewer_layout", "layout") != BTreeSet::from(["single3d", "four_panel"])
        || values_for("set_render_mode", "mode") != BTreeSet::from(["mip", "dvr", "iso"])
        || values_for("set_layer_sampling", "sampling")
            != BTreeSet::from(["voxel_exact", "smooth_linear"])
        || !commands.iter().any(|command| {
            command.get("command").and_then(Value::as_str) == Some("set_layer_render_mode")
                && command.get("layer_index").and_then(Value::as_u64) == Some(0)
                && command.get("mode").and_then(Value::as_str) == Some("mip")
        })
        || !commands.iter().any(|command| {
            command.get("command").and_then(Value::as_str) == Some("set_layer_render_mode")
                && command.get("layer_index").and_then(Value::as_u64) == Some(1)
                && command.get("mode").and_then(Value::as_str) == Some("iso")
        })
        || !commands
            .iter()
            .any(|command| command.get("command").and_then(Value::as_str) == Some("probe_hover"))
        || !commands
            .iter()
            .any(|command| command.get("command").and_then(Value::as_str) == Some("primary_click"))
    {
        bail!("representative GPU layout, mode, sampling, Mixed, or Pick coverage drifted");
    }
    Ok(())
}

fn sanitized_product_script_sha256(script: &Value) -> anyhow::Result<String> {
    let mut sanitized = script.clone();
    let commands = sanitized
        .get_mut("commands")
        .and_then(Value::as_array_mut)
        .context("representative GPU script omitted commands")?;
    for command in commands {
        if command.get("command").and_then(Value::as_str) == Some("open_dataset") {
            command["path"] = Value::String("<private-representative-package>".to_owned());
        }
    }
    Ok(Sha256Hasher::digest(&serde_json::to_vec(&sanitized)?).to_string())
}

fn validate_product_structural_evidence(report: &Value) -> anyhow::Result<Value> {
    let events = report
        .get("events")
        .and_then(Value::as_array)
        .context("product automation report omitted command events")?;
    let mut checkpoints = BTreeMap::<&str, &Value>::new();
    for event in events.iter().filter(|event| {
        event.get("command").and_then(Value::as_str) == Some("copy_diagnostics")
            && event.get("status").and_then(Value::as_str) == Some("passed")
    }) {
        let details = event
            .get("details")
            .context("copy_diagnostics event omitted details")?;
        let checkpoint = details
            .get("checkpoint")
            .and_then(Value::as_str)
            .context("copy_diagnostics event omitted checkpoint identity")?;
        if checkpoints.insert(checkpoint, details).is_some() {
            bail!("duplicate GPU diagnostics checkpoint {checkpoint:?}");
        }
    }
    let required = [
        "resident_standalone_before",
        "resident_standalone_after",
        "resident_four_panel_before",
        "resident_four_panel_after",
        "prepared_nonresident_before",
        "prepared_nonresident_after",
        "interrupted_refinement_before",
        "interrupted_refinement_after",
        "settled_idle_before",
        "settled_idle_after",
    ];
    if checkpoints.keys().copied().collect::<BTreeSet<_>>() != BTreeSet::from(required) {
        bail!("product automation report has incomplete GPU diagnostics checkpoints");
    }

    const RESIDENT_ZERO: &[&str] = &[
        "/dataset_runtime/counters/submitted_requests",
        "/dataset_runtime/counters/started_decodes",
        "/dataset_runtime/counters/completed_decodes",
        "/dataset_source_io/physical_bricks/requests",
        "/dataset_source_io/physical_bricks/unique_decodes",
        "/dataset_source_io/reader/physical_range_read_operations",
        "/dataset_source_io/reader/codec_decode_operations",
        "/gpu_adapter/uploads/resources",
        "/gpu_adapter/uploads/payload_bytes",
        "/gpu_adapter/residency/evictions",
        "/gpu_adapter/residency/allocator_plans",
        "/gpu_adapter/payload_placeability/compactions",
        "/gpu_adapter/target_control/buffer_allocations",
        "/gpu_adapter/gpu_objects/bind_group_creations",
        "/gpu_adapter/gpu_objects/usable_pipeline_handles",
    ];
    const IDLE_ZERO: &[&str] = &[
        "/dataset_runtime/counters/submitted_requests",
        "/dataset_runtime/counters/started_decodes",
        "/dataset_runtime/counters/completed_decodes",
        "/dataset_source_io/reader/physical_range_read_operations",
        "/dataset_source_io/reader/codec_decode_operations",
        "/gpu_adapter/uploads/resources",
        "/gpu_adapter/uploads/payload_bytes",
        "/gpu_adapter/frames_executed",
        "/gpu_adapter/queue_submissions",
        "/gpu_adapter/hidden_refinement/batches",
        "/gpu_adapter/hidden_refinement/rows",
        "/render/progressive_presentation/total_coordinated_color_submissions",
    ];
    let standalone = counter_deltas(
        checkpoints["resident_standalone_before"],
        checkpoints["resident_standalone_after"],
        RESIDENT_ZERO,
    )?;
    let four_panel = counter_deltas(
        checkpoints["resident_four_panel_before"],
        checkpoints["resident_four_panel_after"],
        RESIDENT_ZERO,
    )?;
    require_all_zero("resident standalone", &standalone)?;
    require_all_zero("resident four-panel", &four_panel)?;
    let standalone_color = counter_delta(
        checkpoints["resident_standalone_before"],
        checkpoints["resident_standalone_after"],
        "/render/progressive_presentation/total_coordinated_color_submissions",
    )?;
    let four_panel_color = counter_delta(
        checkpoints["resident_four_panel_before"],
        checkpoints["resident_four_panel_after"],
        "/render/progressive_presentation/total_coordinated_color_submissions",
    )?;
    if standalone_color == 0 || four_panel_color == 0 {
        bail!("resident interaction performed no observable color work");
    }

    let prepared_before = checkpoints["prepared_nonresident_before"];
    let prepared_after = checkpoints["prepared_nonresident_after"];
    let prepared_requests = counter_delta(
        prepared_before,
        prepared_after,
        "/dataset_runtime/counters/submitted_requests",
    )?;
    let prepared_source_requests = counter_delta(
        prepared_before,
        prepared_after,
        "/dataset_source_io/physical_bricks/requests",
    )?;
    let prepared_reads = counter_delta(
        prepared_before,
        prepared_after,
        "/dataset_source_io/reader/physical_range_read_operations",
    )?;
    let prepared_decodes = counter_delta(
        prepared_before,
        prepared_after,
        "/dataset_source_io/reader/codec_decode_operations",
    )?;
    let prepared_uploads = counter_delta(
        prepared_before,
        prepared_after,
        "/gpu_adapter/uploads/resources",
    )?;
    let prepared_upload_bytes = counter_delta(
        prepared_before,
        prepared_after,
        "/gpu_adapter/uploads/payload_bytes",
    )?;
    if prepared_requests == 0
        || prepared_source_requests == 0
        || prepared_uploads == 0
        || prepared_upload_bytes == 0
    {
        bail!("prepared nonresident phase did not prove request, source, and renderer-upload work");
    }

    let interrupted_before = checkpoints["interrupted_refinement_before"];
    let interrupted_after = checkpoints["interrupted_refinement_after"];
    let cancelled_refinement = counter_delta(
        interrupted_before,
        interrupted_after,
        "/gpu_adapter/hidden_refinement/jobs_cancelled",
    )?;
    let stale_rejected = counter_delta(
        interrupted_before,
        interrupted_after,
        "/render/progressive_presentation/stale_frames_rejected",
    )?;
    if cancelled_refinement == 0 && stale_rejected == 0 {
        bail!("interrupted refinement did not prove cancellation or stale-result rejection");
    }

    let idle = counter_deltas(
        checkpoints["settled_idle_before"],
        checkpoints["settled_idle_after"],
        IDLE_ZERO,
    )?;
    require_all_zero("settled idle", &idle)?;
    let final_diagnostics = report
        .get("final_diagnostics")
        .context("product automation report omitted final diagnostics")?;
    if final_diagnostics
        .pointer("/render/frame_fidelity/completeness")
        .and_then(Value::as_str)
        != Some("Exact")
        || final_diagnostics
            .pointer("/render/frame_fidelity/display_freshness")
            .and_then(Value::as_str)
            != Some("Current")
        || final_diagnostics
            .pointer("/render/last_error")
            .is_none_or(|value| !value.is_null())
        || counter(final_diagnostics, "/gpu_adapter/validation_error_count")? != 0
        || counter(final_diagnostics, "/gpu_adapter/picks/submissions")? < 2
        || counter(final_diagnostics, "/gpu_adapter/picks/completed")? < 2
    {
        bail!("final exact/current, validation, or Pick structural evidence failed");
    }
    Ok(json!({
        "resident_standalone_zero_work_deltas": standalone,
        "resident_standalone_color_submissions": standalone_color,
        "resident_four_panel_zero_work_deltas": four_panel,
        "resident_four_panel_color_submissions": four_panel_color,
        "prepared_nonresident": {
            "dataset_requests": prepared_requests,
            "source_requests": prepared_source_requests,
            "physical_reads": prepared_reads,
            "codec_decodes": prepared_decodes,
            "uploaded_resources": prepared_uploads,
            "uploaded_payload_bytes": prepared_upload_bytes,
        },
        "interrupted_refinement": {
            "cancelled_jobs": cancelled_refinement,
            "stale_results_rejected": stale_rejected,
        },
        "settled_idle_zero_work_deltas": idle,
        "final_exact_current": true,
        "validation_errors": 0,
        "pick_submissions_minimum_met": true,
    }))
}

fn counter(snapshot: &Value, pointer: &str) -> anyhow::Result<u64> {
    snapshot
        .pointer(pointer)
        .and_then(Value::as_u64)
        .with_context(|| format!("GPU diagnostics omitted unsigned counter {pointer}"))
}

fn counter_delta(before: &Value, after: &Value, pointer: &str) -> anyhow::Result<u64> {
    let before = counter(before, pointer)?;
    let after = counter(after, pointer)?;
    after
        .checked_sub(before)
        .with_context(|| format!("GPU diagnostics counter {pointer} moved backwards"))
}

fn counter_deltas(
    before: &Value,
    after: &Value,
    pointers: &[&str],
) -> anyhow::Result<BTreeMap<String, u64>> {
    pointers
        .iter()
        .map(|pointer| {
            Ok((
                (*pointer).to_owned(),
                counter_delta(before, after, pointer)?,
            ))
        })
        .collect()
}

fn require_all_zero(label: &str, deltas: &BTreeMap<String, u64>) -> anyhow::Result<()> {
    if let Some((pointer, value)) = deltas.iter().find(|(_, value)| **value != 0) {
        bail!("{label} changed zero-work counter {pointer} by {value}");
    }
    Ok(())
}

fn calibration_evaluation(config: &Config, runs: &[RunRecord]) -> anyhow::Result<Value> {
    if runs.len() != CALIBRATION_RUNS {
        bail!("calibration requires exactly {CALIBRATION_RUNS} complete fresh runs");
    }
    let grouped = group_metrics(runs)?;
    let mut metrics = BTreeMap::new();
    for (metric_id, samples) in grouped {
        if samples.len() != CALIBRATION_RUNS {
            bail!("calibration metric {metric_id:?} has an incomplete run vector");
        }
        require_matching_work(&metric_id, &samples)?;
        let values = samples
            .iter()
            .map(|sample| sample.value_ns)
            .collect::<Vec<_>>();
        let median = median(&values)?;
        let minimum = *values.iter().min().unwrap();
        let maximum = *values.iter().max().unwrap();
        let deviation = maximum_proportional_deviation(&values, median)?;
        let template = samples[0];
        let (stability_limit, relative_numerator, relative_denominator) =
            relative_contract(template.class);
        if deviation > stability_limit {
            bail!(
                "calibration metric {metric_id:?} is unstable: deviation {deviation:.6} exceeds {stability_limit:.6}"
            );
        }
        metrics.insert(
            metric_id,
            BaselineMetric {
                class: template.class,
                baseline_value_ns: median,
                calibration_min_ns: minimum,
                calibration_max_ns: maximum,
                maximum_proportional_deviation: deviation,
                stability_limit,
                relative_numerator,
                relative_denominator,
                absolute_limit_ns: template.absolute_limit_ns,
                work_identity: template.work_identity.clone(),
            },
        );
    }
    let calibration_proposal = json!({
        "schema": CALIBRATION_PROPOSAL_SCHEMA,
        "schema_version": 1,
        "measurement_version": MEASUREMENT_VERSION,
        "status": "proposed",
        "revision": config.baseline.revision,
        "environment": BaselineEnvironment::from(&config.environment),
        "workload_profile_id": config.workload_profile_id,
        "metrics": metrics,
        "visibility_and_stall_evaluated": true,
        "exact_cadence": {
            "status": "pass",
            "metric": "first_pixel_out_scanout_cadence",
            "physical_photon_visibility_claimed": false,
            "accepted_as_full_baseline": false,
        },
        "owner_acceptance_required": true,
    });
    let proposal_path = config
        .private_output_directory
        .join("gpu-performance-baseline-proposal.json");
    let bytes = serde_json::to_vec_pretty(&calibration_proposal)?;
    write_new_synced_private_file(
        &proposal_path,
        &bytes,
        MAX_BASELINE_BYTES,
        "GPU performance baseline proposal",
    )?;
    Ok(json!({
        "result": "calibration_pass",
        "calibration_runs": CALIBRATION_RUNS,
        "metrics_stable": true,
        "baseline_rewritten": false,
        "full_baseline_proposed": true,
        "owner_acceptance_required": true,
        "exact_cadence_status": "pass",
        "calibration_proposal_sha256": Sha256Hasher::digest(&bytes).to_string(),
    }))
}

fn comparison_evaluation(
    config: &Config,
    baseline: &BaselineManifest,
    runs: &[RunRecord],
) -> anyhow::Result<Value> {
    if runs.len() != COMPARISON_PAIRS.len() * 2 {
        bail!("comparison requires three complete revision pairs");
    }
    let accepted_metric_ids = baseline
        .metrics
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if runs.iter().any(|run| {
        run.metrics
            .iter()
            .map(|metric| metric.metric_id.as_str())
            .collect::<BTreeSet<_>>()
            != accepted_metric_ids
    }) {
        bail!("campaign metric topology differs from the accepted baseline");
    }
    let mut pair_ratios = BTreeMap::<String, Vec<f64>>::new();
    let mut evaluated = BTreeMap::new();
    for pair_index in 0..COMPARISON_PAIRS.len() {
        let pair = runs
            .iter()
            .filter(|run| run.pair_index == Some(pair_index))
            .collect::<Vec<_>>();
        if pair.len() != 2 {
            bail!("comparison pair {pair_index} is incomplete");
        }
        let baseline_run = pair
            .iter()
            .find(|run| run.role == RevisionRole::Baseline)
            .context("comparison pair omitted baseline")?;
        let candidate_run = pair
            .iter()
            .find(|run| run.role == RevisionRole::Candidate)
            .context("comparison pair omitted candidate")?;
        let baseline_metrics = metric_map(&baseline_run.metrics)?;
        let candidate_metrics = metric_map(&candidate_run.metrics)?;
        if baseline_metrics.keys().collect::<Vec<_>>()
            != candidate_metrics.keys().collect::<Vec<_>>()
        {
            bail!("comparison pair {pair_index} metric topology differs");
        }
        for (metric_id, baseline_sample) in baseline_metrics {
            let candidate_sample = candidate_metrics[metric_id];
            if baseline_sample.work_identity != candidate_sample.work_identity {
                bail!("metric {metric_id:?} changed scientific work or quality");
            }
            let accepted = baseline
                .metrics
                .get(metric_id)
                .with_context(|| format!("accepted baseline omitted metric {metric_id:?}"))?;
            if accepted.work_identity != baseline_sample.work_identity
                || accepted.class != baseline_sample.class
                || accepted.absolute_limit_ns != candidate_sample.absolute_limit_ns
            {
                bail!("metric {metric_id:?} is incomparable with the accepted baseline");
            }
            pair_ratios
                .entry(metric_id.to_owned())
                .or_default()
                .push(candidate_sample.value_ns as f64 / baseline_sample.value_ns.max(1) as f64);
        }
    }
    for (metric_id, ratios) in pair_ratios {
        if ratios.len() != 3 {
            bail!("metric {metric_id:?} does not have three pair ratios");
        }
        let accepted = &baseline.metrics[&metric_id];
        let median_ratio = median_f64(&ratios)?;
        let ratio_limit = match accepted.class {
            MetricClass::Component => {
                accepted.relative_numerator as f64 / accepted.relative_denominator as f64
            }
            MetricClass::Product => {
                let proportional =
                    accepted.relative_numerator as f64 / accepted.relative_denominator as f64;
                let additive = accepted
                    .baseline_value_ns
                    .saturating_add(ONE_MILLISECOND_NS) as f64
                    / accepted.baseline_value_ns as f64;
                proportional.max(additive)
            }
        };
        let candidate_values = runs
            .iter()
            .filter(|run| run.role == RevisionRole::Candidate)
            .map(|run| metric_map(&run.metrics).map(|metrics| metrics[metric_id.as_str()].value_ns))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let candidate_median = median(&candidate_values)?;
        let scalar_limit = match accepted.class {
            MetricClass::Component => accepted
                .baseline_value_ns
                .saturating_mul(COMPONENT_RELATIVE_NUMERATOR)
                .div_ceil(COMPONENT_RELATIVE_DENOMINATOR),
            MetricClass::Product => accepted
                .baseline_value_ns
                .saturating_mul(PRODUCT_RELATIVE_NUMERATOR)
                .div_ceil(PRODUCT_RELATIVE_DENOMINATOR)
                .max(
                    accepted
                        .baseline_value_ns
                        .saturating_add(ONE_MILLISECOND_NS),
                ),
        };
        let passed = median_ratio <= ratio_limit
            && candidate_median <= scalar_limit
            && candidate_values
                .iter()
                .all(|value| *value <= accepted.absolute_limit_ns);
        evaluated.insert(
            metric_id.clone(),
            json!({
                "pair_ratios": ratios,
                "median_pair_ratio": median_ratio,
                "ratio_limit": ratio_limit,
                "candidate_values_ns": candidate_values,
                "candidate_median_ns": candidate_median,
                "accepted_baseline_ns": accepted.baseline_value_ns,
                "relative_scalar_limit_ns": scalar_limit,
                "absolute_limit_ns": accepted.absolute_limit_ns,
                "passed": passed,
            }),
        );
        if !passed {
            bail!("GPU performance metric {metric_id:?} failed its absolute or relative gate");
        }
    }
    Ok(json!({
        "result": "pass",
        "pair_order": ["baseline/candidate", "candidate/baseline", "baseline/candidate"],
        "zero_retries": true,
        "baseline_id": baseline.baseline_id,
        "baseline_rewritten": false,
        "metrics": evaluated,
        "candidate_revision": config.candidate.as_ref().map(|value| value.revision.as_str()),
    }))
}

fn read_accepted_baseline(
    config: &Config,
    repository_root: &Path,
) -> anyhow::Result<BaselineManifest> {
    let path = repository_root.join(BASELINE_RELATIVE_PATH);
    let bytes = fs::read(&path)
        .with_context(|| format!("accepted GPU baseline is missing: {}", path.display()))?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_BASELINE_BYTES {
        bail!("accepted GPU baseline is empty or exceeds its bound");
    }
    let baseline: BaselineManifest =
        serde_json::from_slice(&bytes).context("accepted GPU baseline is invalid JSON")?;
    if baseline.schema != BASELINE_SCHEMA
        || baseline.schema_version != BASELINE_VERSION
        || baseline.measurement_version != MEASUREMENT_VERSION
        || baseline.status != "accepted"
        || baseline.accepted_revision.as_deref() != Some(config.baseline.revision.as_str())
        || baseline.environment.as_ref() != Some(&BaselineEnvironment::from(&config.environment))
        || baseline.workload_profile_id.as_deref() != Some(config.workload_profile_id.as_str())
        || baseline.metrics.len() != EXPECTED_COMPONENT_MEASUREMENTS + EXPECTED_PRODUCT_MEASUREMENTS
        || baseline.accepted_at.as_deref().is_none_or(str::is_empty)
        || baseline
            .owner_acceptance
            .as_deref()
            .is_none_or(str::is_empty)
        || baseline
            .supersedes
            .as_deref()
            .is_some_and(|value| !safe_token(value))
    {
        bail!("accepted GPU baseline is pending, incomplete, or incomparable");
    }
    let product_ids = baseline
        .metrics
        .iter()
        .filter_map(|(metric_id, metric)| {
            (metric.class == MetricClass::Product).then_some(metric_id.as_str())
        })
        .collect::<BTreeSet<_>>();
    if product_ids
        != product_metric_contracts()
            .into_keys()
            .collect::<BTreeSet<_>>()
        || baseline
            .metrics
            .values()
            .filter(|metric| metric.class == MetricClass::Component)
            .count()
            != EXPECTED_COMPONENT_MEASUREMENTS
    {
        bail!("accepted GPU baseline metric topology drifted");
    }
    for (metric_id, metric) in &baseline.metrics {
        let (stability, numerator, denominator) = relative_contract(metric.class);
        if metric_id.is_empty()
            || metric.baseline_value_ns == 0
            || metric.calibration_min_ns == 0
            || metric.calibration_max_ns < metric.calibration_min_ns
            || metric.maximum_proportional_deviation > stability
            || metric.stability_limit != stability
            || metric.relative_numerator != numerator
            || metric.relative_denominator != denominator
            || metric.absolute_limit_ns == 0
            || metric.work_identity.is_null()
        {
            bail!("accepted GPU baseline metric {metric_id:?} is invalid or unstable");
        }
    }
    Ok(baseline)
}

fn ensure_presentation_probe(config: &Config, repository_root: &Path) -> anyhow::Result<()> {
    if !config.presentation_probe_receipt.exists() {
        let revision = config.candidate.as_ref().unwrap_or(&config.baseline);
        run_presentation_probe(config, revision)?;
    }
    validate_presentation_probe_receipt(config, repository_root)
}

fn run_presentation_probe(config: &Config, revision: &RevisionConfig) -> anyhow::Result<()> {
    let xtask = release_binary(&revision.target_directory, "xtask");
    let app = release_binary(&revision.target_directory, "mirante4d-app");
    let validation_root = revision
        .target_directory
        .join("mirante4d/product-validation");
    let mut command = Command::new(&xtask);
    command
        .current_dir(&revision.checkout)
        .args([
            "product-validate",
            config
                .representative_package
                .to_str()
                .context("representative package path is not UTF-8")?,
            "representative_gpu_presentation_probe",
        ])
        .env("MIRANTE4D_PRODUCT_VALIDATE_APP_BINARY", &app)
        .env("MIRANTE4D_PRODUCT_VALIDATE_SKIP_RELEASE_BUILD", "1")
        .env("MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS", "real_display")
        .env("MIRANTE4D_PRODUCT_VALIDATE_OUTPUT_DIR", &validation_root)
        .env("MIRANTE4D_PRODUCT_VALIDATE_TIMEOUT_SECS", "180")
        .env(
            "MIRANTE4D_PRESENTATION_OBSERVER_REPORT",
            &config.presentation_probe_receipt,
        )
        .env(
            "MIRANTE4D_TRUSTED_GPU_ADAPTER_NAME",
            &config.environment.adapter,
        );
    let output = run_command_with_bounded_output(
        &mut command,
        BoundedOutputPolicy {
            scope: "gpu_presentation_probe",
            inactivity_timeout: Duration::from_secs(180),
            absolute_timeout: Duration::from_secs(180),
            progress_interval: Duration::from_secs(15),
            max_stdout_bytes: MAX_PRODUCT_OUTPUT_BYTES,
            max_stderr_bytes: MAX_PRODUCT_OUTPUT_BYTES,
        },
    )?;
    if !output.status.success() {
        bail!(
            "controlled presentation probe failed with {}",
            output.status
        );
    }
    Ok(())
}

fn validate_presentation_probe_receipt(
    config: &Config,
    repository_root: &Path,
) -> anyhow::Result<()> {
    let receipt = read_finalized_private_file(
        &config.presentation_probe_receipt,
        repository_root,
        MAX_BASELINE_BYTES,
        "presentation-boundary probe receipt",
    )?;
    let report: Value = serde_json::from_slice(&receipt.bytes)
        .context("presentation-boundary probe receipt is invalid JSON")?;
    if report.get("schema").and_then(Value::as_str) != Some(PRESENTATION_REPORT_SCHEMA)
        || report.get("schema_version").and_then(Value::as_u64) != Some(PRESENTATION_REPORT_VERSION)
        || report.get("status").and_then(Value::as_str) != Some("pass")
        || report.get("authority").and_then(Value::as_str)
            != Some(config.environment.presentation_authority.as_str())
        || report.get("probe_kind").and_then(Value::as_str)
            != Some("controlled_boundary_activation")
    {
        bail!("presentation-boundary probe receipt is not an accepted live proof");
    }
    validate_exact_presentation_evidence(&report)?;
    let expected_revision = config
        .candidate
        .as_ref()
        .unwrap_or(&config.baseline)
        .revision
        .as_str();
    let observed_environment = report
        .pointer("/environment/qualified_identity")
        .and_then(Value::as_object)
        .context("presentation-boundary probe omitted renderer environment identity")?;
    if required_string(observed_environment, "adapter", "presentation probe")?
        != config.environment.adapter
        || required_string(observed_environment, "backend", "presentation probe")?
            != config.environment.backend
        || required_string(observed_environment, "driver", "presentation probe")?
            != config.environment.driver
        || required_string(
            observed_environment,
            "repository_revision",
            "presentation probe",
        )? != expected_revision
        || report
            .pointer("/environment/window_width_pixels")
            .and_then(Value::as_u64)
            != Some(u64::from(config.environment.target_width_pixels))
        || report
            .pointer("/environment/window_height_pixels")
            .and_then(Value::as_u64)
            != Some(u64::from(config.environment.target_height_pixels))
    {
        bail!("presentation-boundary probe environment or revision is incomparable");
    }
    let proof = report
        .get("boundary_proof")
        .and_then(Value::as_object)
        .context("presentation-boundary probe receipt omitted its proof")?;
    if [
        "controlled_unmapped_gap_not_counted",
        "unchanged_repeat_not_counted",
        "queued_distinct_not_collapsed",
        "resize_detected",
        "occlusion_detected",
        "focus_loss_detected",
        "surface_recreation_detected",
        "target_extent_matched",
        "mapped_window_confirmed",
        "first_pixel_out_timing_observed",
        "first_pixel_out_correlated_to_marker",
        "timing_queue_drained",
        "stable_time_domain",
        "stable_timing_properties",
    ]
    .iter()
    .any(|field| proof.get(*field).and_then(Value::as_bool) != Some(true))
    {
        bail!("presentation-boundary probe receipt has an incomplete proof");
    }
    Ok(())
}

fn validate_config(config: &Config, repository_root: &Path) -> anyhow::Result<()> {
    if config.schema != CONFIG_SCHEMA
        || config.schema_version != CONFIG_VERSION
        || config.measurement_version != MEASUREMENT_VERSION
        || !safe_token(&config.workload_profile_id)
    {
        bail!("GPU performance configuration schema or workload identity is invalid");
    }
    match config.operation {
        Operation::Calibrate if config.candidate.is_some() => {
            bail!("calibration must not name a candidate revision")
        }
        Operation::Compare if config.candidate.is_none() => {
            bail!("comparison requires a candidate revision")
        }
        _ => {}
    }
    validate_revision_config(&config.baseline, "baseline")?;
    if let Some(candidate) = config.candidate.as_ref() {
        validate_revision_config(candidate, "candidate")?;
        if candidate.revision == config.baseline.revision
            || candidate.checkout == config.baseline.checkout
            || candidate.target_directory == config.baseline.target_directory
        {
            bail!(
                "baseline and candidate must be distinct revisions, checkouts, and target directories"
            );
        }
    }
    require_absolute_normal(&config.private_output_directory, "private output directory")?;
    let output_metadata = fs::symlink_metadata(&config.private_output_directory)
        .context("private output directory is unavailable")?;
    if !output_metadata.is_dir()
        || output_metadata.file_type().is_symlink()
        || output_metadata.permissions().mode() & 0o077 != 0
    {
        bail!("private output directory must be one existing mode-0700 nonsymlink directory");
    }
    require_absolute_normal(&config.raw_report, "private raw report")?;
    if config.raw_report.parent() != Some(config.private_output_directory.as_path())
        || config.raw_report.exists()
    {
        bail!("private raw report must be absent and directly inside the private output directory");
    }
    require_absolute_normal(
        &config.presentation_probe_receipt,
        "presentation probe receipt",
    )?;
    if config.presentation_probe_receipt.parent() != Some(config.private_output_directory.as_path())
    {
        bail!("presentation probe receipt must be directly inside the private output directory");
    }
    let canonical_output = fs::canonicalize(&config.private_output_directory)?;
    let canonical_repository = fs::canonicalize(repository_root)?;
    if canonical_output.starts_with(&canonical_repository)
        || canonical_repository.starts_with(&canonical_output)
    {
        bail!("private output directory must remain outside the repository");
    }
    for revision in std::iter::once(&config.baseline).chain(config.candidate.iter()) {
        let checkout = fs::canonicalize(&revision.checkout)?;
        if canonical_output.starts_with(&checkout) || checkout.starts_with(&canonical_output) {
            bail!("private output directory and revision checkouts must be disjoint");
        }
    }
    require_absolute_normal(&config.representative_package, "representative package")?;
    let package_manifest = config
        .representative_package
        .join(PACKAGE_MANIFEST_ROOT_RELATIVE_PATH);
    if !config.representative_package.is_dir()
        || !package_manifest.is_file()
        || sha256_file(&package_manifest)? != config.representative_package_manifest_sha256
    {
        bail!("representative package identity differs from the private configuration");
    }
    require_sha256(
        &config.representative_package_manifest_sha256,
        "representative package manifest SHA-256",
    )?;
    let catalog = mirante4d_storage::LocalPackageCatalog::open(&config.representative_package)
        .context("representative package failed the production package-open contract")?;
    if catalog.science().layers().len() < 2 {
        bail!(
            "representative GPU workload requires at least two layers for the declared Mixed evidence"
        );
    }
    if config
        .supersedes_baseline_id
        .as_deref()
        .is_some_and(|value| !safe_token(value))
    {
        bail!("supersedes_baseline_id must be null or one safe baseline token");
    }
    validate_environment_config(&config.environment)?;
    Ok(())
}

fn validate_revision_config(revision: &RevisionConfig, label: &str) -> anyhow::Result<()> {
    require_absolute_normal(&revision.checkout, &format!("{label} checkout"))?;
    require_absolute_normal(
        &revision.target_directory,
        &format!("{label} target directory"),
    )?;
    require_revision(&revision.revision, label)?;
    if !revision.checkout.is_dir() {
        bail!("{label} checkout is not a directory");
    }
    if revision.target_directory.exists() {
        let metadata = fs::symlink_metadata(&revision.target_directory)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            bail!("{label} target directory is not a real directory");
        }
    } else {
        let parent = revision
            .target_directory
            .parent()
            .context("revision target directory has no parent")?;
        if !parent.is_dir() {
            bail!("{label} target directory parent does not exist");
        }
    }
    if revision.target_directory.starts_with(&revision.checkout) {
        bail!("{label} target directory must remain outside its checkout");
    }
    Ok(())
}

fn validate_environment_config(environment: &EnvironmentConfig) -> anyhow::Result<()> {
    if environment.platform != "linux-x86_64"
        || environment.kernel_release.trim().is_empty()
        || environment.kernel_release.len() > 128
        || environment.kernel_release.chars().any(char::is_control)
        || environment.release_profile != "release"
        || environment.backend != "Vulkan"
        || environment.adapter != "NVIDIA GeForce RTX 3070 Ti Laptop GPU"
        || !safe_display_output(&environment.display_output)
        || environment.target_width_pixels != 1920
        || environment.target_height_pixels != 1080
        || environment.display_refresh_millihz < 30_000
        || environment.driver.trim().is_empty()
        || !safe_token(&environment.display_profile_id)
        || !safe_token(&environment.compositor_session_id)
        || !safe_token(&environment.power_policy_id)
        || !safe_token(&environment.quiescence_policy_id)
        || environment.presentation_authority != PRESENTATION_AUTHORITY
    {
        bail!("GPU performance environment is outside the approved trusted-workstation profile");
    }
    require_sha256(&environment.cargo_lock_sha256, "Cargo.lock SHA-256")?;
    for (path, expected, label) in [
        (
            &environment.ac_online_path,
            &environment.ac_online_expected,
            "AC power",
        ),
        (
            &environment.power_policy_path,
            &environment.power_policy_expected,
            "power policy",
        ),
        (
            &environment.thermal_status_path,
            &environment.thermal_status_expected,
            "thermal status",
        ),
    ] {
        require_absolute_normal(path, label)?;
        if expected.trim().is_empty() || expected.len() > 128 {
            bail!("{label} expected value is invalid");
        }
    }
    Ok(())
}

fn guard_environment(config: &Config) -> anyhow::Result<()> {
    if env::var("GITHUB_ACTIONS").as_deref() == Ok("true") {
        bail!("GPU performance must not run in GitHub Actions");
    }
    if env::var("MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL").as_deref() != Ok("1") {
        bail!("GPU performance requires MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1");
    }
    if env::consts::OS != "linux" || env::consts::ARCH != "x86_64" {
        bail!("GPU performance requires Linux x86_64");
    }
    if env::var_os("DISPLAY").is_none() || env::var_os("WAYLAND_DISPLAY").is_some() {
        bail!("the qualified presentation observer requires a direct X11 session");
    }
    for (name, expected, label) in [
        (
            "MIRANTE4D_GPU_DISPLAY_PROFILE_ID",
            config.environment.display_profile_id.as_str(),
            "display profile",
        ),
        (
            "MIRANTE4D_GPU_COMPOSITOR_SESSION_ID",
            config.environment.compositor_session_id.as_str(),
            "compositor session",
        ),
        (
            "MIRANTE4D_GPU_POWER_POLICY_ID",
            config.environment.power_policy_id.as_str(),
            "power policy identity",
        ),
        (
            "MIRANTE4D_GPU_QUIESCENCE_POLICY_ID",
            config.environment.quiescence_policy_id.as_str(),
            "quiescence policy",
        ),
    ] {
        if env::var(name).as_deref() != Ok(expected) {
            bail!("{label} environment attestation differs from the private configuration");
        }
    }
    if env::var("MIRANTE4D_GPU_ENVIRONMENT_QUIESCENT").as_deref() != Ok("1") {
        bail!("GPU performance requires an explicit quiescent-environment attestation");
    }
    let kernel = command_stdout(Command::new("uname").arg("-r"), "kernel release")?;
    if kernel.trim() != config.environment.kernel_release {
        bail!("kernel release differs from the pinned environment");
    }
    let (width, height, refresh_millihz) = current_x11_mode(&config.environment.display_output)?;
    if width != config.environment.target_width_pixels
        || height != config.environment.target_height_pixels
        || refresh_millihz != config.environment.display_refresh_millihz
    {
        bail!(
            "active X11 output mode differs from the pinned {}x{}@{} mHz profile",
            config.environment.target_width_pixels,
            config.environment.target_height_pixels,
            config.environment.display_refresh_millihz,
        );
    }
    for (path, expected, label) in [
        (
            &config.environment.ac_online_path,
            &config.environment.ac_online_expected,
            "AC power",
        ),
        (
            &config.environment.power_policy_path,
            &config.environment.power_policy_expected,
            "power policy",
        ),
        (
            &config.environment.thermal_status_path,
            &config.environment.thermal_status_expected,
            "thermal status",
        ),
    ] {
        let actual =
            fs::read_to_string(path).with_context(|| format!("failed to read {label} evidence"))?;
        if actual.trim() != expected.trim() {
            bail!("{label} evidence differs from the pinned value");
        }
    }
    Ok(())
}

fn current_x11_mode(output_name: &str) -> anyhow::Result<(u32, u32, u64)> {
    let output = command_stdout(
        Command::new("xrandr").args(["--current"]),
        "xrandr current mode",
    )?;
    parse_xrandr_current_mode(&output, output_name)
}

fn parse_xrandr_current_mode(output: &str, output_name: &str) -> anyhow::Result<(u32, u32, u64)> {
    let mut selected_output = false;
    for line in output.lines() {
        if !line.chars().next().is_some_and(char::is_whitespace) {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            selected_output = fields.first().copied() == Some(output_name)
                && fields.get(1).copied() == Some("connected");
            continue;
        }
        if !selected_output {
            continue;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let Some(mode) = fields.first().copied() else {
            continue;
        };
        let Some(refresh) = fields.iter().find(|field| field.contains('*')) else {
            continue;
        };
        let (width, height) = mode
            .split_once('x')
            .context("active X11 output mode has no WxH geometry")?;
        let refresh = refresh
            .trim_matches(|character: char| !character.is_ascii_digit() && character != '.')
            .parse::<f64>()
            .context("active X11 refresh is not numeric")?;
        if !refresh.is_finite() || refresh <= 0.0 {
            bail!("active X11 refresh is invalid");
        }
        return Ok((
            width.parse().context("active X11 width is invalid")?,
            height.parse().context("active X11 height is invalid")?,
            (refresh * 1_000.0).round() as u64,
        ));
    }
    bail!("configured X11 output {output_name:?} is disconnected or has no active mode")
}

fn qualify_revision(
    revision: &RevisionConfig,
    environment: &EnvironmentConfig,
) -> anyhow::Result<()> {
    let head = command_stdout(
        Command::new("git")
            .current_dir(&revision.checkout)
            .args(["rev-parse", "HEAD"]),
        "git revision",
    )?;
    if head.trim() != revision.revision {
        bail!(
            "{} checkout HEAD differs from its pinned revision",
            revision.revision
        );
    }
    let status = command_stdout(
        Command::new("git").current_dir(&revision.checkout).args([
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
        ]),
        "git status",
    )?;
    if !status.trim().is_empty() {
        bail!("GPU performance requires clean immutable revision checkouts");
    }
    if sha256_file(&revision.checkout.join("Cargo.lock"))? != environment.cargo_lock_sha256 {
        bail!("revision Cargo.lock differs from the pinned comparable environment");
    }
    Ok(())
}

fn build_revision(revision: &RevisionConfig) -> anyhow::Result<()> {
    fs::create_dir_all(&revision.target_directory)?;
    let mut app = cargo_for_revision(revision);
    app.args([
        "build",
        "--release",
        "--frozen",
        "--package",
        "xtask",
        "--package",
        "mirante4d-app",
    ]);
    let output = run_command_with_bounded_output(
        &mut app,
        BoundedOutputPolicy {
            scope: "gpu_build",
            inactivity_timeout: Duration::from_secs(10 * 60),
            absolute_timeout: Duration::from_secs(30 * 60),
            progress_interval: Duration::from_secs(30),
            max_stdout_bytes: MAX_PRODUCT_OUTPUT_BYTES,
            max_stderr_bytes: MAX_PRODUCT_OUTPUT_BYTES,
        },
    )?;
    if !output.status.success() {
        bail!(
            "release application/xtask build failed with {}",
            output.status
        );
    }
    let mut tests = cargo_for_revision(revision);
    tests.args([
        "nextest",
        "run",
        "--workspace",
        "--release",
        "--frozen",
        "--profile",
        "gpu-performance",
        "--run-ignored",
        "only",
        "--no-run",
    ]);
    let output = run_command_with_bounded_output(
        &mut tests,
        BoundedOutputPolicy {
            scope: "gpu_test_build",
            inactivity_timeout: Duration::from_secs(10 * 60),
            absolute_timeout: Duration::from_secs(30 * 60),
            progress_interval: Duration::from_secs(30),
            max_stdout_bytes: MAX_PRODUCT_OUTPUT_BYTES,
            max_stderr_bytes: MAX_PRODUCT_OUTPUT_BYTES,
        },
    )?;
    if !output.status.success() {
        bail!("release benchmark build failed with {}", output.status);
    }
    Ok(())
}

fn cargo_for_revision(revision: &RevisionConfig) -> Command {
    let mut command = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .current_dir(&revision.checkout)
        .env("CARGO_TARGET_DIR", &revision.target_directory)
        .env("NEXTEST_USER_CONFIG_FILE", "none")
        .env("MIRANTE4D_T5_BUILD_REVISION", &revision.revision)
        .env("MIRANTE4D_T5_BUILD_PROFILE", "release")
        .env("MIRANTE4D_T5_BUILD_TARGET_MODE", "trusted-gpu-performance");
    command
}

fn release_binary(target: &Path, name: &str) -> PathBuf {
    target
        .join("release")
        .join(format!("{name}{}", env::consts::EXE_SUFFIX))
}

fn group_metrics(runs: &[RunRecord]) -> anyhow::Result<BTreeMap<String, Vec<&MetricSample>>> {
    let mut grouped = BTreeMap::<String, Vec<&MetricSample>>::new();
    for run in runs {
        for metric in &run.metrics {
            grouped
                .entry(metric.metric_id.clone())
                .or_default()
                .push(metric);
        }
    }
    let expected = runs
        .first()
        .context("campaign produced no runs")?
        .metrics
        .iter()
        .map(|metric| metric.metric_id.as_str())
        .collect::<BTreeSet<_>>();
    if runs.iter().any(|run| {
        run.metrics
            .iter()
            .map(|metric| metric.metric_id.as_str())
            .collect::<BTreeSet<_>>()
            != expected
    }) {
        bail!("campaign metric topology changed between runs");
    }
    Ok(grouped)
}

fn metric_map(metrics: &[MetricSample]) -> anyhow::Result<BTreeMap<&str, &MetricSample>> {
    let mut map = BTreeMap::new();
    for metric in metrics {
        if map.insert(metric.metric_id.as_str(), metric).is_some() {
            bail!("duplicate metric ID {:?}", metric.metric_id);
        }
    }
    Ok(map)
}

fn ensure_unique_metric_ids(metrics: &[MetricSample]) -> anyhow::Result<()> {
    let _ = metric_map(metrics)?;
    if metrics.len() != EXPECTED_COMPONENT_MEASUREMENTS + product_metric_contracts().len() {
        bail!("complete run metric count drifted");
    }
    Ok(())
}

fn require_matching_work(metric_id: &str, samples: &[&MetricSample]) -> anyhow::Result<()> {
    let first = samples.first().context("metric has no samples")?;
    if samples.iter().any(|sample| {
        sample.class != first.class
            || sample.absolute_limit_ns != first.absolute_limit_ns
            || sample.work_identity != first.work_identity
    }) {
        bail!("calibration metric {metric_id:?} changed work or quality");
    }
    Ok(())
}

fn relative_contract(class: MetricClass) -> (f64, u64, u64) {
    match class {
        MetricClass::Component => (
            COMPONENT_STABILITY_LIMIT,
            COMPONENT_RELATIVE_NUMERATOR,
            COMPONENT_RELATIVE_DENOMINATOR,
        ),
        MetricClass::Product => (
            PRODUCT_STABILITY_LIMIT,
            PRODUCT_RELATIVE_NUMERATOR,
            PRODUCT_RELATIVE_DENOMINATOR,
        ),
    }
}

fn nearest_rank(values: &[u64], percentile: usize) -> anyhow::Result<u64> {
    if values.is_empty() || !(1..=100).contains(&percentile) {
        bail!("nearest-rank percentile requires values and a percentile in 1..=100");
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = (percentile * sorted.len()).div_ceil(100);
    Ok(sorted[rank.saturating_sub(1)])
}

fn median(values: &[u64]) -> anyhow::Result<u64> {
    nearest_rank(values, 50)
}

fn median_f64(values: &[f64]) -> anyhow::Result<f64> {
    if values.is_empty()
        || values
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
    {
        bail!("median requires finite nonnegative values");
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    Ok(sorted[(sorted.len() - 1) / 2])
}

fn maximum_proportional_deviation(values: &[u64], median: u64) -> anyhow::Result<f64> {
    if median == 0 || values.is_empty() {
        bail!("calibration variation requires a positive median and nonempty values");
    }
    Ok(values
        .iter()
        .map(|value| value.abs_diff(median) as f64 / median as f64)
        .fold(0.0_f64, f64::max))
}

fn required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
    context: &str,
) -> anyhow::Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("{context} omitted string field {field:?}"))
}

fn required_u64(
    object: &serde_json::Map<String, Value>,
    field: &str,
    context: &str,
) -> anyhow::Result<u64> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .with_context(|| format!("{context} omitted unsigned field {field:?}"))
}

fn command_stdout(command: &mut Command, label: &str) -> anyhow::Result<String> {
    let output = command
        .output()
        .with_context(|| format!("failed to execute {label}"))?;
    if !output.status.success() {
        bail!("{label} failed with {}", output.status);
    }
    String::from_utf8(output.stdout).with_context(|| format!("{label} output is not UTF-8"))
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(Sha256Hasher::digest(&bytes).to_string())
}

fn require_sha256(value: &str, label: &str) -> anyhow::Result<()> {
    if value.len() != 64
        || value
            .bytes()
            .any(|byte| !(byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        bail!("{label} must be one lowercase SHA-256 digest");
    }
    Ok(())
}

fn require_revision(value: &str, label: &str) -> anyhow::Result<()> {
    if value.len() != 40
        || value
            .bytes()
            .any(|byte| !(byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        bail!("{label} revision must be one full lowercase Git object ID");
    }
    Ok(())
}

fn safe_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
}

fn safe_display_output(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn require_absolute_normal(path: &Path, label: &str) -> anyhow::Result<()> {
    if !path.is_absolute()
        || path.components().any(|component| {
            !matches!(
                component,
                Component::RootDir | Component::Normal(_) | Component::Prefix(_)
            )
        })
    {
        bail!("{label} path must be absolute and contain only normal components");
    }
    Ok(())
}

fn remaining(deadline: Instant, label: &str) -> anyhow::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        bail!("GPU performance pair exhausted its 30-minute budget before {label}");
    }
    Ok(remaining)
}

fn repository_root() -> anyhow::Result<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    fs::canonicalize(root).context("failed to resolve repository root")
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment() -> EnvironmentConfig {
        EnvironmentConfig {
            platform: "linux-x86_64".to_owned(),
            kernel_release: "trusted-kernel".to_owned(),
            release_profile: "release".to_owned(),
            adapter: "NVIDIA GeForce RTX 3070 Ti Laptop GPU".to_owned(),
            backend: "Vulkan".to_owned(),
            driver: "trusted-driver".to_owned(),
            display_output: "DP-1".to_owned(),
            display_profile_id: "primary-1080p".to_owned(),
            compositor_session_id: "trusted-x11".to_owned(),
            display_refresh_millihz: 60_000,
            target_width_pixels: 1920,
            target_height_pixels: 1080,
            power_policy_id: "ac-performance".to_owned(),
            ac_online_path: PathBuf::from("/sys/ac-online"),
            ac_online_expected: "1".to_owned(),
            power_policy_path: PathBuf::from("/sys/power-policy"),
            power_policy_expected: "performance".to_owned(),
            thermal_status_path: PathBuf::from("/sys/thermal-status"),
            thermal_status_expected: "qualified".to_owned(),
            quiescence_policy_id: "operator-quiet-v1".to_owned(),
            cargo_lock_sha256: "a".repeat(64),
            presentation_authority: PRESENTATION_AUTHORITY.to_owned(),
        }
    }

    fn component_measurement(family: &str, id: &str) -> Value {
        let mut value = json!({
            "family": family,
            "measurement_id": id,
            "adapter": "NVIDIA GeForce RTX 3070 Ti Laptop GPU",
            "backend": "Vulkan",
            "driver": "trusted-driver",
            "extent": "1920x1080",
            "warmups": 30,
            "samples": 120,
            "raw_render_pass_ns": vec![1_000_000_u64; 120],
            "median_ns": 1_000_000,
            "p95_ns": 1_000_000,
            "absolute_limit_ns": 33_300_000,
            "absolute_met": true,
            "uploads_during_samples": 0,
            "validation_errors": 0,
        });
        match family {
            "native_terminal" => {
                value["shape"] = json!("64x64x64");
                value["mode"] = json!("MIP-VoxelExact");
            }
            "fixed_lod_multichannel" => {
                value["shape"] = json!("64x64x64");
                value["kernel"] = json!("MIP");
                value["sampling"] = json!("VoxelExact");
                value["compatibility"] = json!("co-registered-homogeneous");
                value["channels"] = json!(1);
                value["resources"] = json!(1);
                value["payload_bytes"] = json!(1_048_576);
                value["raw_cpu_planning_ns"] = json!(vec![100_000_u64; 120]);
                value["raw_cpu_submit_ns"] = json!(vec![50_000_u64; 120]);
                value["cpu_planning_p95_ns"] = json!(100_000_u64);
                value["cpu_submit_p95_ns"] = json!(50_000_u64);
            }
            "resident_coordinated" => {
                value["workload"] = json!("1280x720_mip_9x64cubed_resident");
                value["extent"] = json!("1280x720");
            }
            _ => unreachable!(),
        }
        value
    }

    fn presentation_record(
        present_id: u64,
        phase: &str,
        timing: (u64, u64),
        flags: (bool, bool),
        input: (u64, u64),
    ) -> Value {
        let (observed_at_ns, first_pixel_out_ns) = timing;
        let (active_input, exact) = flags;
        let (input_generation, input_to_visible_coarse_ns) = input;
        json!({
            "marker_sequence": present_id,
            "present_id": present_id,
            "observed_at_ns": observed_at_ns,
            "phase": phase,
            "active_input": active_input,
            "exact": exact,
            "input_generation": input_generation,
            "input_to_visible_coarse_ns": input_to_visible_coarse_ns,
            "swapchain_generation": 1,
            "first_pixel_out_ns": first_pixel_out_ns,
            "time_domain": 1,
            "time_domain_id": 7,
        })
    }

    #[test]
    fn nearest_rank_and_stability_use_the_fixed_noninterpolated_contract() {
        assert_eq!(nearest_rank(&[5, 1, 4, 2, 3], 50).unwrap(), 3);
        assert_eq!(nearest_rank(&[5, 1, 4, 2, 3], 95).unwrap(), 5);
        assert_eq!(
            median(&[100, 101, 99, 100, 100, 100, 100, 100, 100, 100]).unwrap(),
            100
        );
        assert_eq!(
            maximum_proportional_deviation(&[98, 100, 102], 100).unwrap(),
            0.02
        );
        assert!(maximum_proportional_deviation(&[0], 0).is_err());
    }

    #[test]
    fn component_parser_requires_all_39_raw_vectors_and_the_shape_gate() {
        let mut lines = Vec::new();
        for index in 0..6 {
            let value =
                component_measurement("native_terminal", &format!("native_terminal::{index}"));
            lines.push(format!("{COMPONENT_MARKER}{value}"));
        }
        for index in 0..32 {
            let value = component_measurement(
                "fixed_lod_multichannel",
                &format!("fixed_lod_multichannel::{index}"),
            );
            lines.push(format!("{COMPONENT_MARKER}{value}"));
        }
        let resident =
            component_measurement("resident_coordinated", "resident_coordinated::resident");
        lines.push(format!("{COMPONENT_MARKER}{resident}"));
        lines.push(format!(
            "{COMPONENT_GATE_MARKER}{}",
            json!({
                "gate": "fixed_lod_homogeneous_linear_ratio",
                "threshold": 1.2,
                "met": true,
            })
        ));
        let output = lines.join("\n");
        assert_eq!(
            parse_component_output(output.as_bytes(), &environment())
                .unwrap()
                .measurements
                .len(),
            39
        );
        lines.remove(0);
        assert!(parse_component_output(lines.join("\n").as_bytes(), &environment()).is_err());
    }

    #[test]
    fn component_parser_rejects_short_vectors_wrong_work_and_environment_drift() {
        let mut measurement = component_measurement("native_terminal", "native_terminal::one");
        measurement["raw_render_pass_ns"] = json!(vec![1_u64; 5]);
        let line = format!("{COMPONENT_MARKER}{measurement}");
        assert!(parse_component_output(line.as_bytes(), &environment()).is_err());

        measurement = component_measurement("native_terminal", "native_terminal::one");
        measurement["uploads_during_samples"] = json!(1);
        let line = format!("{COMPONENT_MARKER}{measurement}");
        assert!(parse_component_output(line.as_bytes(), &environment()).is_err());

        measurement = component_measurement("native_terminal", "native_terminal::one");
        measurement["driver"] = json!("other-driver");
        let line = format!("{COMPONENT_MARKER}{measurement}");
        assert!(parse_component_output(line.as_bytes(), &environment()).is_err());
    }

    #[test]
    fn product_parser_accepts_reproducible_first_pixel_out_and_rejects_overstatement() {
        let config = Config {
            schema: CONFIG_SCHEMA.to_owned(),
            schema_version: CONFIG_VERSION,
            measurement_version: MEASUREMENT_VERSION.to_owned(),
            operation: Operation::Calibrate,
            baseline: RevisionConfig {
                checkout: PathBuf::from("/tmp/baseline"),
                target_directory: PathBuf::from("/tmp/target"),
                revision: "a".repeat(40),
            },
            candidate: None,
            private_output_directory: PathBuf::from("/tmp/private"),
            raw_report: PathBuf::from("/tmp/private/raw.json"),
            representative_package: PathBuf::from("/tmp/package.m4d"),
            representative_package_manifest_sha256: "b".repeat(64),
            workload_profile_id: "cell-v1".to_owned(),
            presentation_probe_receipt: PathBuf::from("/tmp/private/probe.json"),
            supersedes_baseline_id: None,
            environment: environment(),
        };
        let revision = config.baseline.clone();
        let mut report = json!({
            "schema": PRESENTATION_REPORT_SCHEMA,
            "schema_version": PRESENTATION_REPORT_VERSION,
            "authority": PRESENTATION_AUTHORITY,
            "status": "pass",
            "presentation_visibility_claimed": true,
            "exact_cadence_claimed": true,
            "scanout_cadence_claimed": true,
            "click_to_photon_claimed": false,
            "exact_cadence": {
                "status": "pass",
                "metric": "first_pixel_out_scanout_cadence",
                "physical_photon_visibility_claimed": false,
            },
            "clocks": {
                "coarse_visibility": "std_instant_monotonic_vulkan_present_wait_completion",
                "exact_scanout_cadence": "vulkan_ext_present_timing_image_first_pixel_out",
                "cross_clock_subtraction_permitted": false,
            },
            "environment": {
                "window_width_pixels": 1920,
                "window_height_pixels": 1080,
                "qualified_identity": {
                    "adapter": "NVIDIA GeForce RTX 3070 Ti Laptop GPU",
                    "backend": "Vulkan",
                    "driver": "trusted-driver",
                    "repository_revision": "a".repeat(40),
                    "build_profile": "release",
                    "target_mode": "trusted-gpu-performance",
                },
            },
            "boundary_proof": {
                "marker_decode_after_present_only": true,
                "internal_publication_alone_cannot_advance": true,
                "target_extent_matched": true,
                "mapped_window_confirmed": true,
                "first_pixel_out_timing_observed": true,
                "first_pixel_out_correlated_to_marker": true,
                "timing_queue_drained": true,
                "stable_time_domain": true,
                "stable_timing_properties": true,
            },
            "event_counts": {
                "vulkan_present_submitted": 10,
                "vulkan_present_wait_completed": 10,
                "vulkan_present_timing_completed": 10,
                "vulkan_timing_configured_swapchains": 1,
                "correct_changed_presented": 10,
                "vulkan_present_rejected": 0,
                "vulkan_present_rejected_out_of_date": 0,
                "vulkan_present_rejected_fatal": 0,
                "vulkan_present_wait_timeout": 0,
                "vulkan_present_wait_failure": 0,
                "vulkan_present_timing_incomplete": 0,
                "vulkan_present_timing_duplicate": 0,
                "vulkan_present_timing_unknown_id": 0,
                "vulkan_present_timing_rejected_result_ignored": 0,
                "vulkan_present_timing_out_of_order": 0,
                "vulkan_present_timing_zero_or_missing_stage": 0,
                "vulkan_present_timing_zero_time": 0,
                "vulkan_present_timing_failure": 0,
                "vulkan_present_timing_timeout": 0,
                "vulkan_present_timing_queue_full": 0,
                "vulkan_timing_properties_changes": 0,
                "vulkan_time_domain_changes": 0,
                "ambiguous_completion": 0,
                "pending_at_close": 0,
                "present_wait_bindings_at_close": 0,
                "rejected_present_tombstones_at_close": 0,
                "present_timing_bindings_at_close": 0,
                "early_present_timing_records_at_close": 0,
                "present_timing_records_at_close": 0,
                "marker_outcomes_at_close": 0,
                "dropped_records": 0,
            },
            "present_timing": {
                "extension": "VK_EXT_present_timing",
                "extension_revision": 3,
                "stage": "VK_PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT_BIT_EXT",
                "queue_size": 256,
                "configured_swapchain_history": [{
                    "generation": 1,
                    "present_id2_supported": true,
                    "present_timing_supported": true,
                    "present_stage_queries": 4,
                    "time_domain": 1,
                    "time_domain_id": 7,
                }],
                "per_swapchain_generation_counters": {
                    "1": {
                        "timing_properties_counter": 1,
                        "time_domains_counter": 1,
                        "refresh_duration_ns": 16_666_667,
                        "refresh_interval_ns": 16_666_667,
                        "refresh_properties_counter": 1,
                    },
                },
            },
            "presented_records": [
                presentation_record(1, "startup", (10_000_000, 10_000_000), (false, false), (1, 200_000_000)),
                presentation_record(2, "startup", (20_000_000, 26_667_000), (false, true), (2, 1_000_000_000)),
                presentation_record(3, "standalone_interaction", (110_000_000, 100_000_000), (true, false), (10, 10_000_000)),
                presentation_record(4, "standalone_interaction", (126_000_000, 116_667_000), (true, false), (11, 20_000_000)),
                presentation_record(5, "standalone_interaction", (143_000_000, 133_334_000), (true, false), (12, 30_000_000)),
                presentation_record(6, "four_panel_interaction", (200_000_000, 150_000_000), (true, false), (20, 15_000_000)),
                presentation_record(7, "four_panel_interaction", (217_000_000, 166_667_000), (true, false), (21, 25_000_000)),
                presentation_record(8, "four_panel_interaction", (250_000_000, 199_967_000), (true, false), (22, 40_000_000)),
                presentation_record(9, "resident_settlement", (350_000_000, 216_634_000), (false, true), (22, 60_000_000)),
                presentation_record(10, "prepared_nonresident_replacement", (500_000_000, 233_301_000), (false, true), (23, 100_000_000)),
            ],
            "state_transitions": [
                {"observed_at_ns": 90_000_000, "phase": "standalone_interaction", "active_input": true},
                {"observed_at_ns": 170_000_000, "phase": "standalone_interaction", "active_input": false},
                {"observed_at_ns": 190_000_000, "phase": "four_panel_interaction", "active_input": true},
                {"observed_at_ns": 290_000_000, "phase": "four_panel_interaction", "active_input": false},
                {"observed_at_ns": 300_000_000, "phase": "resident_settlement", "active_input": false},
                {"observed_at_ns": 400_000_000, "phase": "prepared_nonresident_replacement", "active_input": false},
            ],
            "metrics": {
                "standalone_present_interval_p95_ns": 16_667_000,
                "four_panel_present_interval_p95_ns": 33_300_000,
                "resident_input_response_p99_ns": 40_000_000,
                "maximum_active_visible_gap_ns": 40_000_000,
                "resident_exact_settlement_ns": 180_000_000,
                "prepared_nonresident_exact_replacement_ns": 100_000_000,
                "startup_complete_coarse_ns": 200_000_000,
                "startup_exact_settlement_ns": 1_000_000_000,
                "raw_vectors": {
                    "standalone_present_intervals_ns": [16_667_000, 16_667_000],
                    "four_panel_present_intervals_ns": [16_667_000, 33_300_000],
                    "resident_input_response_ns": [10_000_000, 20_000_000, 30_000_000, 15_000_000, 25_000_000, 40_000_000],
                    "maximum_active_visible_gap_ns": [40_000_000],
                    "resident_exact_settlement_ns": [180_000_000, 60_000_000],
                    "prepared_nonresident_exact_replacement_ns": [100_000_000],
                    "startup_complete_coarse_ns": [200_000_000],
                    "startup_exact_settlement_ns": [1_000_000_000],
                },
            },
        });
        let metrics =
            parse_product_metrics(&report, &config, &revision, &json!({"unit": true})).unwrap();
        assert_eq!(metrics.len(), EXPECTED_PRODUCT_MEASUREMENTS);
        assert!(
            metrics
                .iter()
                .any(|metric| metric.metric_id == "standalone_present_interval_p95_ns")
        );
        let valid = report.clone();

        report["click_to_photon_claimed"] = json!(true);
        assert!(
            parse_product_metrics(&report, &config, &revision, &json!({"unit": true})).is_err()
        );

        report = valid.clone();
        *report
            .pointer_mut("/metrics/raw_vectors/standalone_present_intervals_ns")
            .unwrap() = Value::Null;
        assert!(
            parse_product_metrics(&report, &config, &revision, &json!({"unit": true})).is_err()
        );

        report = valid.clone();
        *report
            .pointer_mut("/presented_records/0/first_pixel_out_ns")
            .unwrap() = json!(0);
        assert!(
            parse_product_metrics(&report, &config, &revision, &json!({"unit": true})).is_err()
        );

        report = valid.clone();
        *report
            .pointer_mut("/present_timing/configured_swapchain_history/0/present_id2_supported")
            .unwrap() = json!(false);
        assert!(
            parse_product_metrics(&report, &config, &revision, &json!({"unit": true})).is_err()
        );

        report = valid.clone();
        *report
            .pointer_mut("/boundary_proof/first_pixel_out_correlated_to_marker")
            .unwrap() = json!(false);
        assert!(
            parse_product_metrics(&report, &config, &revision, &json!({"unit": true})).is_err()
        );

        report = valid.clone();
        *report
            .pointer_mut("/present_timing/per_swapchain_generation_counters")
            .unwrap() = json!({});
        assert!(
            parse_product_metrics(&report, &config, &revision, &json!({"unit": true})).is_err()
        );

        report = valid;
        report["authority"] = json!("vulkan_khr_present_wait_marker_v1");
        assert!(
            parse_product_metrics(&report, &config, &revision, &json!({"unit": true})).is_err()
        );
    }

    #[test]
    fn relative_thresholds_keep_component_and_product_rules_separate() {
        assert_eq!(relative_contract(MetricClass::Component), (0.025, 105, 100));
        assert_eq!(relative_contract(MetricClass::Product), (0.05, 110, 100));
        let product_baseline = 5_000_000_u64;
        let ratio = product_baseline * 110 / 100;
        assert_eq!(ratio.max(product_baseline + ONE_MILLISECOND_NS), 6_000_000);
    }

    #[test]
    fn baseline_parser_does_not_accept_the_pending_repository_placeholder() {
        let root = repository_root().unwrap();
        let config = Config {
            schema: CONFIG_SCHEMA.to_owned(),
            schema_version: CONFIG_VERSION,
            measurement_version: MEASUREMENT_VERSION.to_owned(),
            operation: Operation::Compare,
            baseline: RevisionConfig {
                checkout: root.clone(),
                target_directory: PathBuf::from("/tmp/baseline-target"),
                revision: "a".repeat(40),
            },
            candidate: Some(RevisionConfig {
                checkout: PathBuf::from("/tmp/candidate"),
                target_directory: PathBuf::from("/tmp/candidate-target"),
                revision: "b".repeat(40),
            }),
            private_output_directory: PathBuf::from("/tmp/private"),
            raw_report: PathBuf::from("/tmp/private/raw.json"),
            representative_package: PathBuf::from("/tmp/package.m4d"),
            representative_package_manifest_sha256: "c".repeat(64),
            workload_profile_id: "cell-v1".to_owned(),
            presentation_probe_receipt: PathBuf::from("/tmp/private/probe.json"),
            supersedes_baseline_id: None,
            environment: environment(),
        };
        assert!(read_accepted_baseline(&config, &root).is_err());
    }

    #[test]
    fn representative_product_script_is_schema_valid_fixed_and_path_redacted() {
        let first = crate::product_validate::representative_gpu_interaction_script(Path::new(
            "/private/first-cell-package",
        ));
        let second = crate::product_validate::representative_gpu_interaction_script(Path::new(
            "/different/private/cell-package",
        ));
        crate::product_validate::validate_product_automation_script(&first).unwrap();
        validate_product_script_contract(&first).unwrap();
        assert_eq!(
            sanitized_product_script_sha256(&first).unwrap(),
            sanitized_product_script_sha256(&second).unwrap()
        );
        let probe = crate::product_validate::representative_gpu_presentation_probe_script(
            Path::new("/private/cell-package"),
        );
        crate::product_validate::validate_product_automation_script(&probe).unwrap();
    }

    fn diagnostic_snapshot() -> Value {
        json!({
            "render": {
                "last_error": Value::Null,
                "frame_fidelity": {
                    "completeness": "Exact",
                    "display_freshness": "Current",
                },
                "progressive_presentation": {
                    "total_coordinated_color_submissions": 0,
                    "stale_frames_rejected": 0,
                },
            },
            "dataset_runtime": {
                "counters": {
                    "submitted_requests": 0,
                    "started_decodes": 0,
                    "completed_decodes": 0,
                },
            },
            "dataset_source_io": {
                "physical_bricks": {
                    "requests": 0,
                    "unique_decodes": 0,
                },
                "reader": {
                    "physical_range_read_operations": 0,
                    "codec_decode_operations": 0,
                },
            },
            "gpu_adapter": {
                "uploads": { "resources": 0, "payload_bytes": 0 },
                "residency": { "evictions": 0, "allocator_plans": 0 },
                "payload_placeability": { "compactions": 0 },
                "target_control": { "buffer_allocations": 0 },
                "gpu_objects": { "bind_group_creations": 1, "usable_pipeline_handles": 3 },
                "frames_executed": 0,
                "queue_submissions": 0,
                "hidden_refinement": { "jobs_cancelled": 0, "batches": 0, "rows": 0 },
                "picks": { "submissions": 2, "completed": 2 },
                "validation_error_count": 0,
            },
        })
    }

    fn structural_report(resident_upload: u64, prepared_upload: u64, idle_frames: u64) -> Value {
        let checkpoint = |name: &str, mut snapshot: Value| {
            snapshot["checkpoint"] = json!(name);
            json!({
                "command": "copy_diagnostics",
                "status": "passed",
                "details": snapshot,
            })
        };
        let standalone_before = diagnostic_snapshot();
        let mut standalone_after = standalone_before.clone();
        standalone_after["render"]["progressive_presentation"]["total_coordinated_color_submissions"] =
            json!(2);
        standalone_after["gpu_adapter"]["uploads"]["resources"] = json!(resident_upload);
        let four_before = diagnostic_snapshot();
        let mut four_after = four_before.clone();
        four_after["render"]["progressive_presentation"]["total_coordinated_color_submissions"] =
            json!(4);
        let prepared_before = diagnostic_snapshot();
        let mut prepared_after = prepared_before.clone();
        prepared_after["dataset_runtime"]["counters"]["submitted_requests"] = json!(1);
        prepared_after["dataset_source_io"]["physical_bricks"]["requests"] = json!(1);
        prepared_after["dataset_source_io"]["reader"]["physical_range_read_operations"] = json!(1);
        prepared_after["dataset_source_io"]["reader"]["codec_decode_operations"] = json!(1);
        prepared_after["gpu_adapter"]["uploads"]["resources"] = json!(prepared_upload);
        prepared_after["gpu_adapter"]["uploads"]["payload_bytes"] =
            json!(prepared_upload.saturating_mul(4096));
        let interrupted_before = diagnostic_snapshot();
        let mut interrupted_after = interrupted_before.clone();
        interrupted_after["gpu_adapter"]["hidden_refinement"]["jobs_cancelled"] = json!(1);
        let idle_before = diagnostic_snapshot();
        let mut idle_after = idle_before.clone();
        idle_after["gpu_adapter"]["frames_executed"] = json!(idle_frames);
        json!({
            "events": [
                checkpoint("resident_standalone_before", standalone_before),
                checkpoint("resident_standalone_after", standalone_after),
                checkpoint("resident_four_panel_before", four_before),
                checkpoint("resident_four_panel_after", four_after),
                checkpoint("prepared_nonresident_before", prepared_before),
                checkpoint("prepared_nonresident_after", prepared_after),
                checkpoint("interrupted_refinement_before", interrupted_before),
                checkpoint("interrupted_refinement_after", interrupted_after),
                checkpoint("settled_idle_before", idle_before),
                checkpoint("settled_idle_after", idle_after),
            ],
            "final_diagnostics": diagnostic_snapshot(),
        })
    }

    #[test]
    fn product_structural_evidence_rejects_fast_wrong_work() {
        assert!(validate_product_structural_evidence(&structural_report(0, 1, 0)).is_ok());
        assert!(validate_product_structural_evidence(&structural_report(1, 1, 0)).is_err());
        assert!(validate_product_structural_evidence(&structural_report(0, 0, 0)).is_err());
        assert!(validate_product_structural_evidence(&structural_report(0, 1, 1)).is_err());
    }

    #[test]
    fn xrandr_mode_parser_binds_the_named_output_extent_and_refresh() {
        let output = concat!(
            "Screen 0: minimum 8 x 8, current 1920 x 1080, maximum 32767 x 32767\n",
            "DP-1 connected primary 1920x1080+0+0 (normal) 344mm x 194mm\n",
            "   1920x1080     60.00*+  59.94\n",
            "HDMI-0 disconnected (normal left inverted right x axis y axis)\n",
        );
        assert_eq!(
            parse_xrandr_current_mode(output, "DP-1").unwrap(),
            (1920, 1080, 60_000)
        );
        assert!(parse_xrandr_current_mode(output, "HDMI-0").is_err());
    }
}

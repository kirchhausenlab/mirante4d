use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use serde::Serialize;
use serde_json::Value;

use crate::process::{cargo_command, ensure_nextest, run_command_with_timeout};

use super::{fixtures, registry};

const FORMAT_TIMEOUT: Duration = Duration::from_secs(60);
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const PR_TEST_UNION_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const DOCTEST_PROCESS_TIMEOUT: Duration = Duration::from_secs(120);
const SOURCE_FIXTURE_VALIDATION_TIMEOUT: Duration = Duration::from_secs(120);
const TARGET_FIXTURE_VALIDATION_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Leaf {
    Policy,
    Lint,
    Unit,
    Contract,
    Ui,
    Doctest,
}

impl Leaf {
    pub(crate) fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "policy" => Ok(Self::Policy),
            "lint" => Ok(Self::Lint),
            "unit" => Ok(Self::Unit),
            "contract" => Ok(Self::Contract),
            "ui" => Ok(Self::Ui),
            "doctest" => Ok(Self::Doctest),
            _ => bail!(
                "unknown verification leaf {value:?}; expected policy|lint|unit|contract|ui|doctest"
            ),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Policy => "policy",
            Self::Lint => "lint",
            Self::Unit => "unit",
            Self::Contract => "contract",
            Self::Ui => "ui",
            Self::Doctest => "doctest",
        }
    }
}

pub(crate) fn verify_leaf(leaf: Leaf) -> anyhow::Result<()> {
    let started = Instant::now();
    println!("verification leaf {}: starting", leaf.name());
    let identity = RunIdentity::gather()?;
    let mut phases = PhaseCollector::default();
    phases.record_identity(&identity);
    match leaf {
        Leaf::Policy => run_policy_phases(&mut phases),
        Leaf::Lint => {
            phases.run("lint", lint_command(), run_lint);
        }
        Leaf::Unit | Leaf::Contract | Leaf::Ui => {
            phases.run(leaf.name(), nextest_leaf_command(leaf), || {
                run_nextest_leaf(leaf)
            });
        }
        Leaf::Doctest => {
            phases.run("doctest", doctest_command(), run_doctest);
        }
    }
    let report_path = verification_report_path(&format!("verify-leaf-{}", leaf.name()))?;
    let report_result = phases.write_report(&report_path, leaf.name(), &identity);
    let result = phases.finish(leaf.name()).and(report_result);
    println!(
        "verification leaf {}: {} after {:.3}s",
        leaf.name(),
        if result.is_ok() { "passed" } else { "failed" },
        started.elapsed().as_secs_f64()
    );
    result
}

pub(crate) fn verify_pr(group: Option<&str>) -> anyhow::Result<()> {
    match group {
        Some("policy") => verify_pr_policy(),
        Some("rust") => verify_pr_rust(),
        Some(other) => bail!("unknown verify-pr group {other:?}; expected policy|rust"),
        None => {
            let policy = verify_pr_policy();
            let rust = verify_pr_rust();
            match (policy, rust) {
                (Ok(()), Ok(())) => Ok(()),
                (policy, rust) => bail!(
                    "verify-pr failed: policy={}; rust={}",
                    outcome_text(&policy),
                    outcome_text(&rust)
                ),
            }
        }
    }
}

pub(crate) fn verify_local(lane: &str) -> anyhow::Result<()> {
    match lane {
        "format-lifecycle" => verify_format_lifecycle(),
        "trusted-gpu" => verify_trusted_gpu(),
        _ => {
            bail!("unknown local verification lane {lane:?}; expected format-lifecycle|trusted-gpu")
        }
    }
}

fn verify_trusted_gpu() -> anyhow::Result<()> {
    if env::var("GITHUB_ACTIONS").as_deref() == Ok("true") {
        bail!("trusted-gpu verification must not run in GitHub Actions");
    }
    if env::var("MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL").as_deref() != Ok("1") {
        bail!(
            "trusted-gpu verification requires MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 on the trusted machine"
        );
    }
    ensure_nextest()?;
    let registry = registry::read_registry()?;
    let (selector, timeout_secs) = registry::trusted_gpu_policy(&registry)?;
    let identity = RunIdentity::gather()?;
    if !identity.qualifying {
        bail!(
            "trusted-gpu verification requires a qualifying clean revision: {}",
            identity.qualification_issues.join("; ")
        );
    }
    let report_path = verification_report_path("verify-local-trusted-gpu")?;
    let output_path = report_path
        .parent()
        .context("trusted-GPU report path has no parent")?
        .join("trusted-gpu-output.log");
    let mut phases = PhaseCollector::default();
    phases.record_identity(&identity);
    let mut wp09a_evidence = None;
    let command_text = format!(
        "NEXTEST_USER_CONFIG_FILE=none cargo nextest run --workspace --frozen --profile trusted-gpu --run-ignored only --no-fail-fast --retries 0 --flaky-result fail --no-tests fail --success-output final --no-output-indent -E '{selector}'"
    );
    phases.run("trusted-gpu", command_text, || {
        let mut command = isolated_nextest_command();
        command.args([
            "nextest",
            "run",
            "--workspace",
            "--frozen",
            "--profile",
            "trusted-gpu",
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
            "final",
            "--no-output-indent",
            "-E",
            &selector,
        ]);
        let output = fs::File::create(&output_path)
            .with_context(|| format!("failed to create {}", output_path.display()))?;
        command.stdout(Stdio::from(output.try_clone()?));
        command.stderr(Stdio::from(output));
        let command_result =
            run_command_with_timeout(&mut command, Duration::from_secs(timeout_secs));
        let encoded = fs::read_to_string(&output_path)
            .with_context(|| format!("failed to read {}", output_path.display()))?;
        print!("{encoded}");
        command_result?;
        wp09a_evidence = Some(parse_wp09a_evidence_output(&encoded)?);
        Ok(())
    });
    if let Some(evidence) = wp09a_evidence {
        phases.record_evidence("wp09a", evidence);
    }
    let report_result = phases.write_report(&report_path, "trusted-gpu", &identity);
    phases.finish("trusted-gpu").and(report_result)
}

fn parse_wp09a_evidence_output(output: &str) -> anyhow::Result<Value> {
    const PREFIX: &str = "wp09a-evidence-json:";
    let lines = output
        .lines()
        .filter_map(|line| line.trim().strip_prefix(PREFIX))
        .collect::<Vec<_>>();
    if lines.len() != 1 {
        bail!(
            "trusted-GPU output must contain exactly one {PREFIX:?} line; found {}",
            lines.len()
        );
    }
    let evidence: Value =
        serde_json::from_str(lines[0]).context("WP-09A evidence line is not valid JSON")?;
    validate_wp09a_evidence(&evidence)?;
    Ok(evidence)
}

fn validate_wp09a_evidence(evidence: &Value) -> anyhow::Result<()> {
    let root = exact_object(
        evidence,
        &[
            "adapter",
            "capacity_counters",
            "capacity_ledger",
            "cases",
            "counters",
            "ledger",
            "readback",
            "result",
            "schema",
            "schema_version",
            "validation_errors",
        ],
        "WP-09A evidence",
    )?;
    require_string(root, "schema", "WP-09A evidence")?
        .eq("mirante4d-wp09a-trusted-gpu-evidence")
        .then_some(())
        .context("WP-09A evidence schema drifted")?;
    if require_u64(root, "schema_version", "WP-09A evidence")? != 1
        || require_string(root, "result", "WP-09A evidence")? != "passed"
    {
        bail!("WP-09A evidence version or result drifted");
    }

    let adapter = exact_object(
        root.get("adapter")
            .context("WP-09A evidence lacks adapter")?,
        &[
            "backend",
            "driver",
            "max_buffer_size_bytes",
            "max_storage_buffer_binding_size_bytes",
            "max_storage_buffers_per_shader_stage",
            "name",
        ],
        "WP-09A adapter",
    )?;
    for field in ["name", "driver"] {
        require_sanitized_text(adapter, field, "WP-09A adapter")?;
    }
    if require_string(adapter, "backend", "WP-09A adapter")? != "Vulkan"
        || require_u64(adapter, "max_buffer_size_bytes", "WP-09A adapter")? < 268_435_456
        || require_u64(
            adapter,
            "max_storage_buffer_binding_size_bytes",
            "WP-09A adapter",
        )? < 268_435_456
        || require_u64(
            adapter,
            "max_storage_buffers_per_shader_stage",
            "WP-09A adapter",
        )? < 8
    {
        bail!("WP-09A evidence did not use a qualifying Vulkan adapter");
    }

    let configured = validate_wp09a_ledger(
        root.get("ledger").context("WP-09A evidence lacks ledger")?,
        "WP-09A ledger",
    )?;
    let capacity_configured = validate_wp09a_ledger(
        root.get("capacity_ledger")
            .context("WP-09A evidence lacks capacity_ledger")?,
        "WP-09A capacity ledger",
    )?;
    if configured != 4 * 1024 * 1024 * 1024
        || capacity_configured != 11 * 1024 * 1024
        || capacity_configured >= configured
    {
        bail!("WP-09A main and capacity ledgers are not the two accepted distinct budgets");
    }

    let counters_value = root
        .get("counters")
        .context("WP-09A evidence lacks counters")?;
    let capacity_counters_value = root
        .get("capacity_counters")
        .context("WP-09A evidence lacks capacity_counters")?;
    let frames = validate_wp09a_counters(counters_value, "WP-09A counters")?;
    validate_wp09a_counters(capacity_counters_value, "WP-09A capacity counters")?;
    if counters_value == capacity_counters_value {
        bail!("WP-09A main and capacity counters must be reported independently");
    }
    validate_wp09a_counter_case_facts(counters_value, capacity_counters_value)?;
    validate_wp09a_cases(root.get("cases").context("WP-09A evidence lacks cases")?)?;

    let readback = exact_object(
        root.get("readback")
            .context("WP-09A evidence lacks readback")?,
        &[
            "captures",
            "coverage_exact",
            "rgba8_max_delta",
            "selected_hand_facts_exact",
            "validity_exact",
        ],
        "WP-09A readback",
    )?;
    if require_u64(readback, "captures", "WP-09A readback")? != frames
        || require_u64(readback, "rgba8_max_delta", "WP-09A readback")? > 1
        || !require_bool(readback, "coverage_exact", "WP-09A readback")?
        || !require_bool(readback, "validity_exact", "WP-09A readback")?
        || !require_bool(readback, "selected_hand_facts_exact", "WP-09A readback")?
    {
        bail!("WP-09A readback facts are outside the accepted tolerance");
    }

    let validation_errors = root
        .get("validation_errors")
        .and_then(Value::as_array)
        .context("WP-09A validation_errors must be an array")?;
    if !validation_errors.is_empty() {
        bail!("WP-09A evidence contains GPU validation errors");
    }
    Ok(())
}

fn validate_wp09a_ledger(value: &Value, context: &str) -> anyhow::Result<u64> {
    let ledger = exact_object(
        value,
        &[
            "configured_bytes",
            "display_page_table_scratch_capacity_bytes",
            "peak_display_target_bytes",
            "peak_page_table_bytes",
            "peak_payload_residency_bytes",
            "peak_scratch_bytes",
            "peak_transfer_staging_bytes",
            "payload_residency_capacity_bytes",
            "transfer_staging_capacity_bytes",
        ],
        context,
    )?;
    let configured = require_u64(ledger, "configured_bytes", context)?;
    let payload_capacity = require_u64(ledger, "payload_residency_capacity_bytes", context)?;
    let transfer_capacity = require_u64(ledger, "transfer_staging_capacity_bytes", context)?;
    let other_capacity = require_u64(ledger, "display_page_table_scratch_capacity_bytes", context)?;
    let peak_payload = require_u64(ledger, "peak_payload_residency_bytes", context)?;
    let peak_transfer = require_u64(ledger, "peak_transfer_staging_bytes", context)?;
    let peak_display = require_u64(ledger, "peak_display_target_bytes", context)?;
    let peak_page_table = require_u64(ledger, "peak_page_table_bytes", context)?;
    let peak_scratch = require_u64(ledger, "peak_scratch_bytes", context)?;
    let combined_other_peak = peak_display
        .checked_add(peak_page_table)
        .and_then(|sum| sum.checked_add(peak_scratch));
    if configured == 0
        || payload_capacity
            .checked_add(transfer_capacity)
            .and_then(|sum| sum.checked_add(other_capacity))
            != Some(configured)
        || u128::from(payload_capacity) * 100 > u128::from(configured) * 75
        || u128::from(transfer_capacity) * 100 > u128::from(configured) * 10
        || u128::from(other_capacity) * 100 < u128::from(configured) * 15
        || peak_payload == 0
        || peak_transfer == 0
        || peak_display == 0
        || peak_payload > payload_capacity
        || peak_transfer > transfer_capacity
        || combined_other_peak.is_none_or(|peak| peak > other_capacity)
    {
        bail!("{context} configured or peak GPU categories are invalid");
    }
    Ok(configured)
}

fn validate_wp09a_counters(value: &Value, context: &str) -> anyhow::Result<u64> {
    let counters = exact_object(
        value,
        &[
            "command_buffers",
            "control_upload_bytes",
            "frames",
            "max_command_buffers",
            "max_control_upload_bytes",
            "max_payload_upload_bytes",
            "max_queue_submissions",
            "max_resources_uploaded",
            "max_resources_visited",
            "payload_upload_bytes",
            "queue_submissions",
            "resources_uploaded",
            "resources_visited",
        ],
        context,
    )?;
    let frames = require_u64(counters, "frames", context)?;
    if frames == 0 {
        bail!("{context} must record at least one successful frame");
    }
    for (total_field, max_field, accepted_maximum) in [
        ("resources_visited", "max_resources_visited", 128_u64),
        ("resources_uploaded", "max_resources_uploaded", 8),
        (
            "payload_upload_bytes",
            "max_payload_upload_bytes",
            8 * 1024 * 1024,
        ),
        (
            "control_upload_bytes",
            "max_control_upload_bytes",
            64 * 1024,
        ),
        ("command_buffers", "max_command_buffers", 1),
        ("queue_submissions", "max_queue_submissions", 1),
    ] {
        let total = require_u64(counters, total_field, context)?;
        let observed_maximum = require_u64(counters, max_field, context)?;
        if observed_maximum > accepted_maximum
            || observed_maximum > total
            || u128::from(total) > u128::from(frames) * u128::from(observed_maximum)
            || u128::from(total) > u128::from(frames) * u128::from(accepted_maximum)
        {
            bail!("{context} field {total_field} is incoherent or exceeds its per-frame ceiling");
        }
    }
    if require_u64(counters, "command_buffers", context)? != frames
        || require_u64(counters, "queue_submissions", context)? != frames
        || require_u64(counters, "max_command_buffers", context)? != 1
        || require_u64(counters, "max_queue_submissions", context)? != 1
    {
        bail!("{context} must record exactly one command buffer and submission per accepted frame");
    }
    Ok(frames)
}

fn validate_wp09a_counter_case_facts(main: &Value, capacity: &Value) -> anyhow::Result<()> {
    let main = main
        .as_object()
        .context("WP-09A counters must be an object")?;
    let capacity = capacity
        .as_object()
        .context("WP-09A capacity counters must be an object")?;
    for (field, expected) in [
        ("max_resources_visited", 128_u64),
        ("max_resources_uploaded", 8),
        ("max_payload_upload_bytes", 8 * 1024 * 1024),
    ] {
        if require_u64(main, field, "WP-09A counters")? != expected {
            bail!("WP-09A counters.{field} does not prove the accepted boundary case");
        }
    }
    for (field, expected) in [
        ("max_resources_visited", 1_u64),
        ("max_resources_uploaded", 1),
        ("max_payload_upload_bytes", 1024 * 1024),
    ] {
        if require_u64(capacity, field, "WP-09A capacity counters")? != expected {
            bail!("WP-09A capacity counters.{field} drifted from the eviction fixture");
        }
    }
    for (field, minimum) in [
        ("resources_visited", 129_u64),
        ("resources_uploaded", 9),
        ("payload_upload_bytes", 9 * 1024 * 1024),
    ] {
        if require_u64(main, field, "WP-09A counters")? < minimum {
            bail!("WP-09A counters.{field} cannot cover the accepted boundary facts");
        }
    }
    for (field, minimum) in [
        ("frames", 10_u64),
        ("resources_visited", 10),
        ("resources_uploaded", 10),
        ("payload_upload_bytes", 10 * 1024 * 1024),
    ] {
        if require_u64(capacity, field, "WP-09A capacity counters")? < minimum {
            bail!("WP-09A capacity counters.{field} cannot cover eviction and re-upload");
        }
    }
    Ok(())
}

fn validate_wp09a_cases(value: &Value) -> anyhow::Result<()> {
    let cases = exact_object(
        value,
        &[
            "cancellation_proved",
            "capacity_rejected_without_submit",
            "eviction_reupload_proved",
            "lease_release_render_proved",
            "qualification_extents",
            "semantic_fixture_decoded_bytes_with_validity",
            "semantic_fixture_resources",
            "semantic_modes_and_dtypes",
            "stale_capture_rejected",
            "stale_frame_rejected_without_submit",
            "upload_first_bytes",
            "upload_first_resources",
            "upload_second_bytes",
            "upload_second_resources",
            "work_first_visits",
            "work_second_visits",
        ],
        "WP-09A cases",
    )?;
    for field in [
        "cancellation_proved",
        "stale_capture_rejected",
        "stale_frame_rejected_without_submit",
        "eviction_reupload_proved",
        "capacity_rejected_without_submit",
        "lease_release_render_proved",
    ] {
        if !require_bool(cases, field, "WP-09A cases")? {
            bail!("WP-09A cases.{field} must be true");
        }
    }
    for (field, expected) in [
        ("semantic_fixture_resources", 24_u64),
        ("semantic_fixture_decoded_bytes_with_validity", 241_664),
        ("upload_first_resources", 8),
        ("upload_first_bytes", 8 * 1024 * 1024),
        ("upload_second_resources", 1),
        ("upload_second_bytes", 1024 * 1024),
        ("work_first_visits", 128),
        ("work_second_visits", 1),
    ] {
        if require_u64(cases, field, "WP-09A cases")? != expected {
            bail!("WP-09A cases.{field} drifted from the accepted fixture fact");
        }
    }
    let modes = cases
        .get("semantic_modes_and_dtypes")
        .and_then(Value::as_array)
        .context("WP-09A cases.semantic_modes_and_dtypes must be an array")?;
    let expected_modes = ["mip-u8", "dvr-u16", "iso-f32", "cross-section-u8"];
    if modes.len() != expected_modes.len()
        || modes
            .iter()
            .zip(expected_modes)
            .any(|(actual, expected)| actual.as_str() != Some(expected))
    {
        bail!("WP-09A semantic mode/dtype cases drifted");
    }
    let extents = cases
        .get("qualification_extents")
        .and_then(Value::as_array)
        .context("WP-09A cases.qualification_extents must be an array")?;
    let expected_extents = [[1280_u64, 720_u64], [1920, 1080]];
    if extents.len() != expected_extents.len()
        || extents
            .iter()
            .zip(expected_extents)
            .any(|(actual, expected)| {
                actual.as_array().is_none_or(|values| {
                    values.len() != 2
                        || values[0].as_u64() != Some(expected[0])
                        || values[1].as_u64() != Some(expected[1])
                })
            })
    {
        bail!("WP-09A qualification extents drifted");
    }
    Ok(())
}

fn exact_object<'a>(
    value: &'a Value,
    expected: &[&str],
    context: &str,
) -> anyhow::Result<&'a serde_json::Map<String, Value>> {
    let object = value
        .as_object()
        .with_context(|| format!("{context} must be an object"))?;
    let actual = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    if actual != expected {
        bail!("{context} fields drifted: expected={expected:?}, actual={actual:?}");
    }
    Ok(object)
}

fn require_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
    context: &str,
) -> anyhow::Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("{context}.{field} must be a string"))
}

fn require_sanitized_text(
    object: &serde_json::Map<String, Value>,
    field: &str,
    context: &str,
) -> anyhow::Result<()> {
    let text = require_string(object, field, context)?;
    if text.is_empty()
        || text.len() > 256
        || text.contains('/')
        || text.contains('\\')
        || text.chars().any(char::is_control)
    {
        bail!("{context}.{field} is empty, path-like, oversized, or contains control text");
    }
    Ok(())
}

fn require_u64(
    object: &serde_json::Map<String, Value>,
    field: &str,
    context: &str,
) -> anyhow::Result<u64> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .with_context(|| format!("{context}.{field} must be an unsigned integer"))
}

fn require_bool(
    object: &serde_json::Map<String, Value>,
    field: &str,
    context: &str,
) -> anyhow::Result<bool> {
    object
        .get(field)
        .and_then(Value::as_bool)
        .with_context(|| format!("{context}.{field} must be a boolean"))
}

fn verify_format_lifecycle() -> anyhow::Result<()> {
    let started = Instant::now();
    ensure_nextest()?;
    let registry = registry::read_registry()?;
    let timeout_secs = registry::format_lifecycle_timeout(&registry)?;
    let deadline = started
        .checked_add(Duration::from_secs(timeout_secs))
        .context("format-lifecycle aggregate deadline overflowed")?;
    let identity = RunIdentity::gather()?;
    let mut phases = PhaseCollector::default();
    phases.record_identity(&identity);
    phases.run(
        "fixture-registry",
        "in-process fixture registry validation",
        || {
            format_lifecycle_remaining(deadline)?;
            fixtures::validate_fixture_registry()?;
            format_lifecycle_remaining(deadline)?;
            Ok(())
        },
    );
    phases.run(
        "target-fixture-validation",
        "python3 tools/target-fixtures/t1/validate.py --manifest fixtures/target/manifest.json --self-test",
        || {
            let mut command = Command::new("python3");
            command.args([
                "tools/target-fixtures/t1/validate.py",
                "--manifest",
                "fixtures/target/manifest.json",
                "--self-test",
            ]);
            run_command_with_timeout(
                &mut command,
                format_lifecycle_remaining(deadline)?.min(TARGET_FIXTURE_VALIDATION_TIMEOUT),
            )
        },
    );
    phases.run(
        "target-conformance",
        format_lifecycle_test_command(),
        || {
            let mut command = isolated_nextest_command();
            command.args([
                "nextest",
                "run",
                "--package",
                "mirante4d-storage",
                "--test",
                "target_conformance",
                "--test",
                "target_mutation_conformance",
                "--frozen",
                "--profile",
                "leaf",
                "--no-fail-fast",
                "--retries",
                "0",
                "--flaky-result",
                "fail",
                "--no-tests",
                "fail",
            ]);
            run_command_with_timeout(&mut command, format_lifecycle_remaining(deadline)?)
        },
    );
    phases.run(
        "representative-metadata-scalability",
        format_lifecycle_scalability_command(),
        || {
            let mut command = cargo_command();
            command.args([
                "test",
                "-p",
                "mirante4d-storage",
                "--lib",
                "package_catalog::tests::representative_large_manifest_open_stays_inside_the_metadata_working_set",
                "--frozen",
                "--",
                "--exact",
                "--ignored",
                "--nocapture",
            ]);
            run_command_with_timeout(&mut command, format_lifecycle_remaining(deadline)?)
        },
    );
    phases.run(
        "writer-independent-reader-conformance",
        production_writer_conformance_command(),
        || {
            let mut command = Command::new("python3");
            command.arg("tools/target-fixtures/production-conformance/run.py");
            run_command_with_timeout(&mut command, format_lifecycle_remaining(deadline)?)
        },
    );
    phases.run(
        "aggregate-deadline",
        "in-process 900-second aggregate deadline check",
        || format_lifecycle_remaining(deadline).map(|_| ()),
    );
    let report_path = verification_report_path("verify-local-format-lifecycle")?;
    let report_result = phases.write_report(&report_path, "format-lifecycle", &identity);
    phases.finish("format-lifecycle").and(report_result)
}

fn format_lifecycle_remaining(deadline: Instant) -> anyhow::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .context("format-lifecycle exhausted its aggregate timeout")
}

fn verify_pr_policy() -> anyhow::Result<()> {
    let identity = RunIdentity::gather()?;
    let mut phases = PhaseCollector::default();
    phases.record_identity(&identity);
    run_policy_phases(&mut phases);
    let report_path = verification_report_path("verify-pr-policy")?;
    let report_result = phases.write_report(&report_path, "policy", &identity);
    phases.finish("policy").and(report_result)
}

fn run_policy_phases(phases: &mut PhaseCollector) {
    phases.run("format", "cargo fmt --all --check", || {
        let mut command = cargo_command();
        command.args(["fmt", "--all", "--check"]);
        run_command_with_timeout(&mut command, FORMAT_TIMEOUT)
    });
    phases.run(
        "verification-registry",
        "cargo xtask verification-sync --check",
        || registry::sync_generated(true),
    );
    phases.run(
        "fixture-registry",
        "in-process fixture registry validation",
        fixtures::validate_fixture_registry,
    );
    phases.run(
        "source-fixture-validation",
        "python3 tools/source-fixtures/validate.py --self-test --reader-report fixtures/source/independent-reader-report.json",
        || {
            let mut command = Command::new("python3");
            command.args([
                "tools/source-fixtures/validate.py",
                "--self-test",
                "--reader-report",
                "fixtures/source/independent-reader-report.json",
            ]);
            run_command_with_timeout(&mut command, SOURCE_FIXTURE_VALIDATION_TIMEOUT)
        },
    );
    phases.run(
        "target-fixture-validation",
        "python3 tools/target-fixtures/t1/validate.py --manifest fixtures/target/manifest.json --self-test",
        || {
            let mut command = Command::new("python3");
            command.args([
                "tools/target-fixtures/t1/validate.py",
                "--manifest",
                "fixtures/target/manifest.json",
                "--self-test",
            ]);
            run_command_with_timeout(&mut command, TARGET_FIXTURE_VALIDATION_TIMEOUT)
        },
    );
    phases.run(
        "architecture",
        "in-process architecture self-check",
        crate::arch::architecture_self_check,
    );
    phases.run(
        "documentation",
        "cargo xtask docs-check",
        crate::documentation::docs_check,
    );
    phases.run(
        "dependencies",
        "cargo xtask verify-deps",
        crate::deps::verify_deps,
    );
    phases.run("workflow", "cargo xtask workflow-audit", || {
        crate::workflow_audit::workflow_audit().map(drop)
    });
    phases.run("command-surface", "cargo xtask command-audit", || {
        crate::command_audit::command_audit().map(drop)
    });
}

fn run_lint() -> anyhow::Result<()> {
    let registry = registry::read_registry()?;
    let timeout = lane_timeout(&registry, "lint")?;
    let diagnostics_path = verification_report_path("clippy-diagnostics")?;
    let diagnostics_file = fs::File::create(&diagnostics_path)
        .with_context(|| format!("failed to create {}", diagnostics_path.display()))?;
    let mut command = cargo_command();
    command.args([
        "clippy",
        "--workspace",
        "--lib",
        "--bins",
        "--tests",
        "--frozen",
        "--keep-going",
        "--message-format=json",
    ]);
    command.stdout(Stdio::from(diagnostics_file));
    let command_result = run_command_with_timeout(&mut command, timeout);
    let encoded = fs::read_to_string(&diagnostics_path)
        .with_context(|| format!("failed to read {}", diagnostics_path.display()))?;
    let _ = fs::remove_file(&diagnostics_path);
    if let Err(error) = command_result {
        emit_rendered_compiler_diagnostics(&encoded);
        return Err(error).context("Clippy compilation or invocation failed");
    }

    let actual = parse_clippy_warnings(&encoded)?;
    let expected = registry
        .lint_exceptions
        .iter()
        .map(registry::LintException::key)
        .collect::<BTreeSet<_>>();
    compare_lint_warnings(&actual, &expected)?;
    println!(
        "lint policy: Clippy emitted exactly {} registered inherited warnings",
        actual.len()
    );
    for exception in &registry.lint_exceptions {
        println!(
            "lint exception matched: {} {}:{}; deletion_gate={}; reason={}",
            exception.code,
            exception.path,
            exception.primary_line,
            exception.deletion_gate(),
            exception.reason(),
        );
    }
    Ok(())
}

fn run_nextest_leaf(leaf: Leaf) -> anyhow::Result<()> {
    ensure_nextest()?;
    let registry = registry::read_registry()?;
    let timeout = lane_timeout(&registry, leaf.name())?;
    let filter = format!("group({})", leaf.name());
    let mut command = isolated_nextest_command();
    command.args([
        "nextest",
        "run",
        "--workspace",
        "--frozen",
        "--profile",
        "leaf",
        "--no-fail-fast",
        "--retries",
        "0",
        "--flaky-result",
        "fail",
        "--no-tests",
        "fail",
        "-E",
        &filter,
    ]);
    run_command_with_timeout(&mut command, timeout)
}

fn run_doctest() -> anyhow::Result<()> {
    let mut command = cargo_command();
    command.args(["test", "--workspace", "--doc", "--frozen", "--no-fail-fast"]);
    run_command_with_timeout(&mut command, DOCTEST_PROCESS_TIMEOUT)
}

fn verify_pr_rust() -> anyhow::Result<()> {
    ensure_nextest()?;
    let registry = registry::read_registry()?;
    let identity = RunIdentity::gather()?;
    let report_path = verification_report_path("verify-pr-rust")?;
    let discovery_path = report_path
        .parent()
        .context("verification report path has no parent")?
        .join("nextest-discovery.json");
    let mut phases = PhaseCollector::default();
    phases.record_identity(&identity);

    phases.run("lint", lint_command(), run_lint);

    let discovery_ok = phases.run("test-build-and-discovery", discovery_command(), || {
        if let Some(parent) = discovery_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let output = fs::File::create(&discovery_path)
            .with_context(|| format!("failed to create {}", discovery_path.display()))?;
        let mut command = isolated_nextest_command();
        command.args([
            "nextest",
            "list",
            "--workspace",
            "--frozen",
            "--profile",
            "pr",
            "--run-ignored",
            "all",
            "--message-format",
            "json",
        ]);
        command.stdout(Stdio::from(output));
        run_command_with_timeout(&mut command, DISCOVERY_TIMEOUT)
    });

    let mut discovered_counts = BTreeMap::new();
    let audit_ok = if discovery_ok {
        phases.run(
            "discovery-audit",
            "in-process exact discovery audit",
            || {
                discovered_counts = registry::audit_discovery(&discovery_path, &registry)?;
                for (lane, count) in &discovered_counts {
                    println!("verification discovery: lane={lane} cases={count}");
                }
                Ok(())
            },
        )
    } else {
        phases.block("discovery-audit", "test build/discovery failed");
        false
    };

    let union_ok = if audit_ok {
        phases.run("unit-contract-ui", pr_union_command(), || {
            let mut command = isolated_nextest_command();
            command.args([
                "nextest",
                "run",
                "--workspace",
                "--frozen",
                "--profile",
                "pr",
                "--no-fail-fast",
                "--retries",
                "0",
                "--flaky-result",
                "fail",
                "--no-tests",
                "fail",
                "-E",
                "group(unit) | group(contract) | group(ui)",
            ]);
            run_command_with_timeout(&mut command, PR_TEST_UNION_TIMEOUT)
        })
    } else {
        phases.block(
            "unit-contract-ui",
            "discovery did not prove an exact lane assignment",
        );
        false
    };
    for lane in ["unit", "contract", "ui"] {
        phases.record_lane_section(
            lane,
            discovered_counts.get(lane).copied().unwrap_or(0),
            audit_ok,
            union_ok,
        );
    }

    phases.run("doctest", doctest_command(), run_doctest);
    let report_result = phases.write_report(&report_path, "rust", &identity);
    phases.finish("rust").and(report_result)
}

fn lint_command() -> &'static str {
    "cargo clippy --workspace --lib --bins --tests --frozen --keep-going --message-format=json; require exact verification/registry.json warning set"
}

fn doctest_command() -> &'static str {
    "cargo test --workspace --doc --frozen --no-fail-fast"
}

fn discovery_command() -> &'static str {
    "NEXTEST_USER_CONFIG_FILE=none cargo nextest list --workspace --frozen --profile pr --run-ignored all --message-format json"
}

fn pr_union_command() -> &'static str {
    "NEXTEST_USER_CONFIG_FILE=none cargo nextest run --workspace --frozen --profile pr --no-fail-fast --retries 0 --flaky-result fail --no-tests fail -E 'group(unit) | group(contract) | group(ui)'"
}

fn nextest_leaf_command(leaf: Leaf) -> String {
    format!(
        "NEXTEST_USER_CONFIG_FILE=none cargo nextest run --workspace --frozen --profile leaf --no-fail-fast --retries 0 --flaky-result fail --no-tests fail -E 'group({})'",
        leaf.name()
    )
}

fn format_lifecycle_test_command() -> &'static str {
    "NEXTEST_USER_CONFIG_FILE=none cargo nextest run --package mirante4d-storage --test target_conformance --test target_mutation_conformance --frozen --profile leaf --no-fail-fast --retries 0 --flaky-result fail --no-tests fail"
}

fn format_lifecycle_scalability_command() -> &'static str {
    "cargo test -p mirante4d-storage --lib package_catalog::tests::representative_large_manifest_open_stays_inside_the_metadata_working_set --frozen -- --exact --ignored --nocapture"
}

fn production_writer_conformance_command() -> &'static str {
    "python3 tools/target-fixtures/production-conformance/run.py"
}

fn isolated_nextest_command() -> Command {
    let mut command = cargo_command();
    command.env("NEXTEST_USER_CONFIG_FILE", "none");
    command
}

fn parse_clippy_warnings(encoded: &str) -> anyhow::Result<BTreeSet<registry::LintWarningKey>> {
    let mut warnings = BTreeSet::new();
    for (index, line) in encoded.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: Value = serde_json::from_str(line)
            .with_context(|| format!("Clippy JSON line {} was invalid", index + 1))?;
        if event.get("reason").and_then(Value::as_str) != Some("compiler-message") {
            continue;
        }
        let message = event
            .get("message")
            .and_then(Value::as_object)
            .context("Clippy compiler-message event was missing message")?;
        if message.get("level").and_then(Value::as_str) != Some("warning") {
            continue;
        }
        let code = message
            .get("code")
            .and_then(Value::as_object)
            .and_then(|code| code.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("uncoded")
            .to_owned();
        let primary_spans = message
            .get("spans")
            .and_then(Value::as_array)
            .context("Clippy warning was missing spans")?
            .iter()
            .filter(|span| span.get("is_primary").and_then(Value::as_bool) == Some(true))
            .collect::<Vec<_>>();
        if primary_spans.is_empty() {
            bail!("Clippy warning {code:?} had no primary source span");
        }
        for span in primary_spans {
            let raw_path = span
                .get("file_name")
                .and_then(Value::as_str)
                .context("Clippy primary warning span was missing file_name")?;
            let primary_line = span
                .get("line_start")
                .and_then(Value::as_u64)
                .filter(|line| *line > 0)
                .context("Clippy primary warning span had no positive line_start")?;
            warnings.insert(registry::LintWarningKey {
                code: code.clone(),
                path: normalize_clippy_repo_path(raw_path)?,
                primary_line,
            });
        }
    }
    Ok(warnings)
}

fn normalize_clippy_repo_path(raw: &str) -> anyhow::Result<String> {
    let path = Path::new(raw);
    let relative = if path.is_absolute() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .context("failed to canonicalize repository root")?;
        let absolute = path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize Clippy path {raw:?}"))?;
        absolute
            .strip_prefix(&root)
            .with_context(|| format!("Clippy warning path is outside the repository: {raw:?}"))?
            .to_owned()
    } else {
        path.to_owned()
    };
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(
                part.to_str()
                    .with_context(|| format!("Clippy warning path was not UTF-8: {raw:?}"))?,
            ),
            std::path::Component::CurDir => {}
            _ => bail!("Clippy warning path was not normalized: {raw:?}"),
        }
    }
    if parts.is_empty() {
        bail!("Clippy warning path was empty: {raw:?}");
    }
    let normalized = parts.join("/");
    if !registry::repo_path(&normalized).is_file() {
        bail!("Clippy warning path is not a repository file: {normalized:?}");
    }
    Ok(normalized)
}

fn compare_lint_warnings(
    actual: &BTreeSet<registry::LintWarningKey>,
    expected: &BTreeSet<registry::LintWarningKey>,
) -> anyhow::Result<()> {
    let unknown = actual.difference(expected).cloned().collect::<Vec<_>>();
    let missing = expected.difference(actual).cloned().collect::<Vec<_>>();
    if unknown.is_empty() && missing.is_empty() {
        Ok(())
    } else {
        bail!(
            "Clippy warning set did not exactly match the inherited registry: unknown={unknown:?}; registered_but_missing={missing:?}"
        )
    }
}

fn emit_rendered_compiler_diagnostics(encoded: &str) {
    for event in encoded
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    {
        if event.get("reason").and_then(Value::as_str) == Some("compiler-message")
            && let Some(rendered) = event
                .get("message")
                .and_then(|message| message.get("rendered"))
                .and_then(Value::as_str)
        {
            eprint!("{rendered}");
        }
    }
}

fn lane_timeout(registry: &registry::Registry, lane_id: &str) -> anyhow::Result<Duration> {
    registry
        .lanes
        .iter()
        .find(|lane| lane.id == lane_id)
        .map(|lane| Duration::from_secs(lane.aggregate_timeout_secs))
        .with_context(|| format!("verification registry is missing lane {lane_id:?}"))
}

fn verification_report_path(command: &str) -> anyhow::Result<PathBuf> {
    let run_id = std::env::var("GITHUB_RUN_ID").unwrap_or_else(|_| "local".to_owned());
    let attempt = std::env::var("GITHUB_RUN_ATTEMPT").unwrap_or_else(|_| "1".to_owned());
    let identity = format!("{run_id}-{attempt}-{}", std::process::id());
    let root = Path::new("target")
        .join("mirante4d")
        .join("verification")
        .join(identity);
    fs::create_dir_all(&root).with_context(|| format!("failed to create {}", root.display()))?;
    Ok(root.join(format!("{command}.json")))
}

fn outcome_text(result: &anyhow::Result<()>) -> String {
    match result {
        Ok(()) => "passed".to_owned(),
        Err(error) => format!("failed ({error:#})"),
    }
}

#[derive(Debug, Default, Serialize)]
struct PhaseCollector {
    phases: Vec<PhaseResult>,
    evidence: BTreeMap<String, Value>,
}

impl PhaseCollector {
    fn record_identity(&mut self, identity: &RunIdentity) {
        if identity.enforced && !identity.qualifying {
            self.phases.push(PhaseResult {
                name: "identity",
                command: "git and executable identity validation".to_owned(),
                status: "failed",
                outcome_code: Some(1),
                duration_ms: 0.0,
                discovered_cases: None,
                reason: Some(identity.qualification_issues.join("; ")),
            });
        }
    }

    fn record_evidence(&mut self, name: &str, evidence: Value) {
        self.evidence.insert(name.to_owned(), evidence);
    }

    fn run(
        &mut self,
        name: &'static str,
        command: impl Into<String>,
        action: impl FnOnce() -> anyhow::Result<()>,
    ) -> bool {
        let command = command.into();
        let started = Instant::now();
        println!("verification phase {name}: starting: {command}");
        let result = action();
        let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
        let (status, reason) = match result {
            Ok(()) => ("passed", None),
            Err(error) => ("failed", Some(format!("{error:#}"))),
        };
        println!(
            "verification phase {name}: {status} after {:.3}s{}",
            duration_ms / 1000.0,
            reason
                .as_deref()
                .map(|reason| format!("; {reason}"))
                .unwrap_or_default()
        );
        self.phases.push(PhaseResult {
            name,
            command,
            status,
            outcome_code: Some(if status == "passed" { 0 } else { 1 }),
            duration_ms,
            discovered_cases: None,
            reason,
        });
        status == "passed"
    }

    fn block(&mut self, name: &'static str, reason: &'static str) {
        println!("verification phase {name}: blocked; {reason}");
        self.phases.push(PhaseResult {
            name,
            command: "not executed".to_owned(),
            status: "blocked",
            outcome_code: None,
            duration_ms: 0.0,
            discovered_cases: None,
            reason: Some(reason.to_owned()),
        });
    }

    fn record_lane_section(
        &mut self,
        name: &'static str,
        discovered_cases: u64,
        audit_ok: bool,
        union_ok: bool,
    ) {
        let (status, outcome_code, reason) = if audit_ok && union_ok {
            ("passed", Some(0), None)
        } else if !audit_ok {
            (
                "blocked",
                None,
                Some("discovery did not prove the lane assignment".to_owned()),
            )
        } else {
            (
                "blocked",
                None,
                Some(
                    "shared categorized union failed; lane-specific result is inconclusive"
                        .to_owned(),
                ),
            )
        };
        self.phases.push(PhaseResult {
            name,
            command: format!("result section for Nextest group({name})"),
            status,
            outcome_code,
            duration_ms: 0.0,
            discovered_cases: Some(discovered_cases),
            reason,
        });
    }

    fn finish(&self, group: &str) -> anyhow::Result<()> {
        let failures = self
            .phases
            .iter()
            .filter(|phase| phase.status != "passed")
            .map(|phase| phase.name)
            .collect::<Vec<_>>();
        if failures.is_empty() {
            Ok(())
        } else {
            bail!(
                "verification {group} failed or blocked phases: {}",
                failures.join(", ")
            )
        }
    }

    fn write_report(&self, path: &Path, group: &str, identity: &RunIdentity) -> anyhow::Result<()> {
        let report = VerificationReport {
            schema: "mirante4d-verification-run",
            schema_version: 1,
            group,
            native_status: if self.phases.iter().all(|phase| phase.status == "passed") {
                "passed"
            } else {
                "failed"
            },
            generated_at_epoch_ms: epoch_ms(),
            identity,
            phases: &self.phases,
            evidence: &self.evidence,
        };
        let mut encoded = serde_json::to_vec_pretty(&report)?;
        encoded.push(b'\n');
        fs::write(path, &encoded).with_context(|| format!("failed to write {}", path.display()))?;
        println!("verification report: {}", path.display());
        println!(
            "verification-report-json: {}",
            String::from_utf8_lossy(&encoded).trim_end()
        );
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct PhaseResult {
    name: &'static str,
    command: String,
    status: &'static str,
    outcome_code: Option<i32>,
    duration_ms: f64,
    discovered_cases: Option<u64>,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct VerificationReport<'a> {
    schema: &'static str,
    schema_version: u32,
    group: &'a str,
    native_status: &'static str,
    generated_at_epoch_ms: u128,
    identity: &'a RunIdentity,
    phases: &'a [PhaseResult],
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    evidence: &'a BTreeMap<String, Value>,
}

#[derive(Debug, Serialize)]
struct RunIdentity {
    commit: String,
    tree: String,
    clean: bool,
    status_sha256: String,
    executable: String,
    executable_sha256: String,
    rustc: String,
    cargo: String,
    nextest: Option<String>,
    fixtures: Vec<fixtures::FixtureIdentity>,
    runner: BTreeMap<String, String>,
    enforced: bool,
    qualifying: bool,
    qualification_issues: Vec<String>,
}

impl RunIdentity {
    fn gather() -> anyhow::Result<Self> {
        let commit = command_line("git", &["rev-parse", "HEAD"])?;
        let tree = command_line("git", &["show", "-s", "--format=%T", "HEAD"])?;
        let status = Command::new("git")
            .args(["status", "--porcelain=v1", "--untracked-files=all"])
            .output()
            .context("failed to inspect verification worktree identity")?;
        if !status.status.success() {
            bail!("git status failed while gathering verification identity");
        }
        let clean = status.stdout.is_empty();
        let status_sha256 = sha256_bytes(&status.stdout)?;
        let executable_path = env::current_exe().context("failed to locate xtask executable")?;
        let executable = executable_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("xtask")
            .to_owned();
        let executable_sha256 = registry::sha256_file(&executable_path)?;
        let rustc = command_line("rustc", &["--version"])?;
        let cargo = command_line("cargo", &["--version"])?;
        let nextest = command_line("cargo", &["nextest", "--version"]).ok();
        let runner = [
            "GITHUB_REPOSITORY",
            "GITHUB_RUN_ID",
            "GITHUB_RUN_ATTEMPT",
            "GITHUB_JOB",
            "GITHUB_WORKFLOW",
            "GITHUB_EVENT_NAME",
            "GITHUB_SHA",
            "GITHUB_REF",
            "RUNNER_OS",
            "RUNNER_ARCH",
            "RUNNER_NAME",
        ]
        .into_iter()
        .filter_map(|key| env::var(key).ok().map(|value| (key.to_owned(), value)))
        .collect::<BTreeMap<_, _>>();
        let enforced = env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
            || env::var("CI").as_deref() == Ok("true");
        let mut qualification_issues = Vec::new();
        if !clean {
            qualification_issues.push("worktree is dirty".to_owned());
        }
        if let Some(expected) = runner.get("GITHUB_SHA")
            && expected != &commit
        {
            qualification_issues.push(format!(
                "GITHUB_SHA does not match HEAD: expected {expected}, found {commit}"
            ));
        }
        let qualifying = qualification_issues.is_empty();
        Ok(Self {
            commit,
            tree,
            clean,
            status_sha256,
            executable,
            executable_sha256,
            rustc,
            cargo,
            nextest,
            fixtures: fixtures::fixture_identities()?,
            runner,
            enforced,
            qualifying,
            qualification_issues,
        })
    }
}

fn command_line(program: &str, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {program} {}", args.join(" ")))?;
    if !output.status.success() {
        bail!("{program} {} failed", args.join(" "));
    }
    String::from_utf8(output.stdout)
        .context("tool identity output was not UTF-8")
        .map(|text| text.trim().to_owned())
}

fn sha256_bytes(bytes: &[u8]) -> anyhow::Result<String> {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to start sha256sum for identity")?;
    child
        .stdin
        .take()
        .context("sha256sum stdin unavailable")?
        .write_all(bytes)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!("sha256sum failed for identity bytes");
    }
    String::from_utf8(output.stdout)?
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .context("sha256sum returned no identity digest")
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn leaf_parser_accepts_exact_surface() {
        for name in ["policy", "lint", "unit", "contract", "ui", "doctest"] {
            assert_eq!(Leaf::parse(name).unwrap().name(), name);
        }
        assert!(Leaf::parse("fast").is_err());
    }

    fn valid_wp09a_evidence() -> Value {
        json!({
            "schema": "mirante4d-wp09a-trusted-gpu-evidence",
            "schema_version": 1,
            "adapter": {
                "name": "NVIDIA GeForce RTX 3070 Ti Laptop GPU",
                "backend": "Vulkan",
                "driver": "580.159.03",
                "max_buffer_size_bytes": 268435456,
                "max_storage_buffer_binding_size_bytes": 268435456,
                "max_storage_buffers_per_shader_stage": 8
            },
            "ledger": {
                "configured_bytes": 4294967296_u64,
                "payload_residency_capacity_bytes": 3221225472_u64,
                "transfer_staging_capacity_bytes": 429496729_u64,
                "display_page_table_scratch_capacity_bytes": 644245095_u64,
                "peak_payload_residency_bytes": 8388608,
                "peak_transfer_staging_bytes": 8388608,
                "peak_display_target_bytes": 16588800,
                "peak_page_table_bytes": 0,
                "peak_scratch_bytes": 0
            },
            "capacity_ledger": {
                "configured_bytes": 11534336,
                "payload_residency_capacity_bytes": 8650752,
                "transfer_staging_capacity_bytes": 1153433,
                "display_page_table_scratch_capacity_bytes": 1730151,
                "peak_payload_residency_bytes": 8388608,
                "peak_transfer_staging_bytes": 1048576,
                "peak_display_target_bytes": 8,
                "peak_page_table_bytes": 0,
                "peak_scratch_bytes": 0
            },
            "counters": {
                "frames": 12,
                "resources_visited": 196,
                "resources_uploaded": 41,
                "payload_upload_bytes": 9699456,
                "control_upload_bytes": 3072,
                "command_buffers": 12,
                "queue_submissions": 12,
                "max_resources_visited": 128,
                "max_resources_uploaded": 8,
                "max_payload_upload_bytes": 8388608,
                "max_control_upload_bytes": 256,
                "max_command_buffers": 1,
                "max_queue_submissions": 1
            },
            "capacity_counters": {
                "frames": 10,
                "resources_visited": 10,
                "resources_uploaded": 10,
                "payload_upload_bytes": 10485760,
                "control_upload_bytes": 2560,
                "command_buffers": 10,
                "queue_submissions": 10,
                "max_resources_visited": 1,
                "max_resources_uploaded": 1,
                "max_payload_upload_bytes": 1048576,
                "max_control_upload_bytes": 256,
                "max_command_buffers": 1,
                "max_queue_submissions": 1
            },
            "cases": {
                "semantic_modes_and_dtypes": [
                    "mip-u8",
                    "dvr-u16",
                    "iso-f32",
                    "cross-section-u8"
                ],
                "semantic_fixture_resources": 24,
                "semantic_fixture_decoded_bytes_with_validity": 241664,
                "upload_first_resources": 8,
                "upload_first_bytes": 8388608,
                "upload_second_resources": 1,
                "upload_second_bytes": 1048576,
                "work_first_visits": 128,
                "work_second_visits": 1,
                "cancellation_proved": true,
                "stale_capture_rejected": true,
                "stale_frame_rejected_without_submit": true,
                "eviction_reupload_proved": true,
                "capacity_rejected_without_submit": true,
                "lease_release_render_proved": true,
                "qualification_extents": [[1280, 720], [1920, 1080]]
            },
            "readback": {
                "captures": 12,
                "rgba8_max_delta": 1,
                "coverage_exact": true,
                "validity_exact": true,
                "selected_hand_facts_exact": true
            },
            "validation_errors": [],
            "result": "passed"
        })
    }

    #[test]
    fn wp09a_evidence_line_is_exact_and_sanitized() {
        let evidence = valid_wp09a_evidence();
        let output = format!(
            "nextest noise\nwp09a-evidence-json:{}\n",
            serde_json::to_string(&evidence).unwrap()
        );
        assert_eq!(parse_wp09a_evidence_output(&output).unwrap(), evidence);
        assert!(parse_wp09a_evidence_output("no evidence").is_err());
        assert!(parse_wp09a_evidence_output(&format!("{output}{output}")).is_err());

        let mut path_leak = evidence;
        path_leak["adapter"]["driver"] = Value::String("/private/driver".into());
        let output = format!(
            "wp09a-evidence-json:{}",
            serde_json::to_string(&path_leak).unwrap()
        );
        assert!(parse_wp09a_evidence_output(&output).is_err());
    }

    #[test]
    fn wp09a_evidence_rejects_budget_and_case_incoherence() {
        let mut excessive_maximum = valid_wp09a_evidence();
        excessive_maximum["counters"]["max_resources_visited"] = json!(129);
        assert!(validate_wp09a_evidence(&excessive_maximum).is_err());

        let mut impossible_total = valid_wp09a_evidence();
        impossible_total["capacity_counters"]["payload_upload_bytes"] = json!(10485761);
        assert!(validate_wp09a_evidence(&impossible_total).is_err());

        let mut shared_capacity_overflow = valid_wp09a_evidence();
        shared_capacity_overflow["ledger"]["peak_page_table_bytes"] = json!(644245095_u64);
        assert!(validate_wp09a_evidence(&shared_capacity_overflow).is_err());

        let mut duplicate_ledgers = valid_wp09a_evidence();
        duplicate_ledgers["capacity_ledger"] = duplicate_ledgers["ledger"].clone();
        assert!(validate_wp09a_evidence(&duplicate_ledgers).is_err());

        let mut case_drift = valid_wp09a_evidence();
        case_drift["cases"]["work_first_visits"] = json!(127);
        assert!(validate_wp09a_evidence(&case_drift).is_err());

        let mut capture_drift = valid_wp09a_evidence();
        capture_drift["readback"]["captures"] = json!(11);
        assert!(validate_wp09a_evidence(&capture_drift).is_err());
    }

    #[test]
    fn phase_collector_retains_failures_before_returning() {
        let mut phases = PhaseCollector::default();
        assert!(!phases.run("first", "test command one", || bail!("expected")));
        assert!(phases.run("second", "test command two", || Ok(())));
        assert_eq!(phases.phases.len(), 2);
        assert!(phases.finish("test").is_err());
    }

    #[test]
    fn clippy_json_parser_normalizes_and_deduplicates_warnings() {
        let event = json!({
            "reason": "compiler-message",
            "message": {
                "level": "warning",
                "code": { "code": "clippy::example" },
                "spans": [{
                    "file_name": "./crates/xtask/src/verification/runner.rs",
                    "line_start": 1,
                    "is_primary": true
                }]
            }
        });
        let encoded = format!("{event}\n{event}\n");

        let actual = parse_clippy_warnings(&encoded).unwrap();

        assert_eq!(
            actual,
            BTreeSet::from([registry::LintWarningKey {
                code: "clippy::example".to_owned(),
                path: "crates/xtask/src/verification/runner.rs".to_owned(),
                primary_line: 1,
            }])
        );
    }

    #[test]
    fn lint_warning_comparison_requires_an_exact_registered_set() {
        let warning = registry::LintWarningKey {
            code: "clippy::example".to_owned(),
            path: "crates/xtask/src/verification/runner.rs".to_owned(),
            primary_line: 1,
        };
        let expected = BTreeSet::from([warning.clone()]);
        assert!(compare_lint_warnings(&expected, &expected).is_ok());
        assert!(
            compare_lint_warnings(&BTreeSet::new(), &expected)
                .unwrap_err()
                .to_string()
                .contains("registered_but_missing")
        );
        assert!(
            compare_lint_warnings(&expected, &BTreeSet::new())
                .unwrap_err()
                .to_string()
                .contains("unknown")
        );
    }

    #[test]
    fn nextest_commands_disable_user_configuration() {
        let command = isolated_nextest_command();
        let configured = command
            .get_envs()
            .find(|(key, _)| *key == "NEXTEST_USER_CONFIG_FILE")
            .and_then(|(_, value)| value)
            .and_then(|value| value.to_str());
        assert_eq!(configured, Some("none"));
    }

    #[test]
    fn format_lifecycle_targets_only_declared_conformance_work() {
        let command = format_lifecycle_test_command();
        assert!(command.contains("--package mirante4d-storage"));
        assert!(command.contains("--test target_conformance"));
        assert!(command.contains("--test target_mutation_conformance"));
        assert!(!command.contains("--test target_writer_conformance"));
        assert!(command.contains("--no-tests fail"));
        assert!(!command.contains("--workspace"));
        assert_eq!(
            format_lifecycle_scalability_command(),
            "cargo test -p mirante4d-storage --lib package_catalog::tests::representative_large_manifest_open_stays_inside_the_metadata_working_set --frozen -- --exact --ignored --nocapture"
        );
        assert_eq!(
            production_writer_conformance_command(),
            "python3 tools/target-fixtures/production-conformance/run.py"
        );
    }
}

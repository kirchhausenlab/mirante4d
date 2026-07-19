//! Exact, source-bound EP-00 conformance harness execution.
//!
//! These receipts deliberately execute the production renderer's ignored
//! release harnesses from the qualification runner's fresh target directory.
//! A successful schema-only bundle validation is not scientific evidence:
//! the observed marker facts must agree with the independently committed
//! oracle facts, and the test process must also complete successfully.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt},
        process::{CommandExt, ExitStatusExt},
    },
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use mirante4d_identity::Sha256Hasher;
use serde::Serialize;
use serde_json::{Value, json};

use crate::process::cargo_command;

const MAX_CAPTURE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_HARNESS_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

const NUMERICAL_MARKER: &str = "mirante4d-ep00-numerical-gpu-conformance-json:";
const DVR_MARKER: &str = "mirante4d-ep00-numerical-gpu-conformance-gap-json:";
const SHADER_MARKER: &str = "mirante4d-ep00-production-shader-structural-audit-json:";
const TIMESTAMP_MARKER: &str = "mirante4d-ep00-resident-volume-gpu-timing-json:";
const QUEUE_WRITE_MARKER: &str = "mirante4d-ep00-queue-write-envelope-control-json:";
const EXPECTED_FACT_DOMAIN: &[u8] = b"mirante4d-ep00-numerical-expected-fact-v1\0";

const SOURCE_FILES: [&str; 8] = [
    "Cargo.lock",
    "crates/mirante4d-render-reference/src/numerical_oracle.rs",
    "crates/mirante4d-render-wgpu/src/gpu_tests.rs",
    "crates/mirante4d-render-wgpu/src/lib.rs",
    "crates/mirante4d-render-wgpu/src/pick_shader.wgsl",
    "crates/mirante4d-render-wgpu/src/runtime.rs",
    "crates/mirante4d-render-wgpu/src/shader.wgsl",
    "crates/mirante4d-render-wgpu/src/shader_audit.rs",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HarnessKind {
    NumericalPlaneMipIso,
    NumericalPerspectiveDvr,
    ProductionShaderAudit,
    TimestampPlacementControls,
    QueueWriteEnvelopeControl,
}

impl HarnessKind {
    const fn id(self) -> &'static str {
        match self {
            Self::NumericalPlaneMipIso => "numerical_plane_mip_iso_depth_pick",
            Self::NumericalPerspectiveDvr => "numerical_perspective_dvr_world_distance",
            Self::ProductionShaderAudit => "production_shader_structural_audit",
            Self::TimestampPlacementControls => "timestamp_known_copy_and_empty_pass_controls",
            Self::QueueWriteEnvelopeControl => "queue_write_batch_envelope_control",
        }
    }

    const fn test_name(self) -> &'static str {
        match self {
            Self::NumericalPlaneMipIso => {
                "gpu_tests::ep00_plane_mip_iso_depth_and_pick_match_independent_numerical_oracle"
            }
            Self::NumericalPerspectiveDvr => {
                "gpu_tests::ep00_off_axis_perspective_dvr_uses_physical_world_distance"
            }
            Self::ProductionShaderAudit => {
                "shader_audit::emit_production_shader_structural_audit_json"
            }
            Self::TimestampPlacementControls => "gpu_tests::resident_volume_gpu_timing",
            Self::QueueWriteEnvelopeControl => {
                "gpu_tests::queue_write_batch_envelope_gpu_timing_control"
            }
        }
    }

    const fn marker(self) -> &'static str {
        match self {
            Self::NumericalPlaneMipIso => NUMERICAL_MARKER,
            Self::NumericalPerspectiveDvr => DVR_MARKER,
            Self::ProductionShaderAudit => SHADER_MARKER,
            Self::TimestampPlacementControls => TIMESTAMP_MARKER,
            Self::QueueWriteEnvelopeControl => QUEUE_WRITE_MARKER,
        }
    }

    const fn expected_schema(self) -> &'static str {
        match self {
            Self::NumericalPlaneMipIso => "mirante4d-ep00-numerical-gpu-conformance",
            Self::NumericalPerspectiveDvr => "mirante4d-ep00-numerical-gpu-conformance-gap",
            Self::ProductionShaderAudit => "mirante4d-ep00-production-shader-structural-audit-1",
            Self::TimestampPlacementControls => "mirante4d-resident-volume-gpu-timing-v3",
            Self::QueueWriteEnvelopeControl => "mirante4d-queue-write-envelope-control-v1",
        }
    }
}

const HARNESSES: [HarnessKind; 5] = [
    HarnessKind::NumericalPlaneMipIso,
    HarnessKind::NumericalPerspectiveDvr,
    HarnessKind::ProductionShaderAudit,
    HarnessKind::TimestampPlacementControls,
    HarnessKind::QueueWriteEnvelopeControl,
];

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceCommitment {
    repository_relative_path: &'static str,
    sha256: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct CaseCommitment {
    id: String,
    input_fact_sha256: String,
    expected_fact_sha256: String,
}

#[derive(Clone, Debug)]
pub(super) struct ReplayCommitmentAuthority {
    source_commitments: Vec<(String, String)>,
    case_commitments: Vec<(String, String, String)>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ProcessReceipt {
    exit_code: Option<i32>,
    signal: Option<i32>,
    timed_out: bool,
    wall_time_ns: u64,
    spawn_failed: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ParsedMarkerReceipt {
    schema: Option<String>,
    result: Option<String>,
    canonical_sha256: Option<String>,
    parsed: bool,
}

#[derive(Clone, Debug)]
struct HarnessReceipt {
    kind: HarnessKind,
    process: ProcessReceipt,
    stdout_sha256: Option<String>,
    stderr_sha256: Option<String>,
    marker: Option<Value>,
    marker_receipt: ParsedMarkerReceipt,
    reasons: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub(super) struct ConformanceEvidence {
    source_commitments: Vec<SourceCommitment>,
    case_commitments: Vec<CaseCommitment>,
    harnesses: Vec<HarnessReceipt>,
    bound_case_ids: BTreeSet<String>,
    reasons: BTreeSet<String>,
}

impl ConformanceEvidence {
    pub(super) fn reason_codes(&self) -> &BTreeSet<String> {
        &self.reasons
    }

    pub(super) fn raw_json(&self) -> Value {
        json!({
            "scope": "exact_release_harnesses_built_in_fresh_private_target",
            "automatic_retries": 0,
            "source_commitments": self.source_commitments,
            "oracle_binding": {
                "bound_case_ids": self.bound_case_ids,
                "all_six_frozen_cases_bound": self.bound_case_ids.len() == 6,
                "case_commitments": self.case_commitments,
            },
            "harnesses": self.harnesses.iter().map(harness_raw_json).collect::<Vec<_>>(),
            "reason_codes": self.reasons,
            "status": if self.reasons.is_empty() { "passed" } else { "failed" },
        })
    }

    #[cfg(test)]
    pub(super) fn sanitized_json(&self) -> Value {
        sanitized_json_from_raw(&self.raw_json(), false, &test_replay_authority(self))
            .expect("in-memory conformance evidence always projects to its sanitized receipt")
    }
}

pub(super) fn replay_commitment_authority(
    repository_root: &Path,
    revision: &str,
    oracle_cases: &[Value],
) -> anyhow::Result<ReplayCommitmentAuthority> {
    let source_commitments = SOURCE_FILES
        .into_iter()
        .map(|path| {
            let bytes = super::git_blob_at_revision(
                repository_root,
                revision,
                path,
                MAX_CAPTURE_BYTES,
                "historical EP-00 conformance source",
            )?;
            Ok((path.to_owned(), Sha256Hasher::digest(&bytes).to_string()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let case_commitments = oracle_cases
        .iter()
        .map(|case| {
            let commitment = case_commitment(case)?;
            Ok((
                commitment.id,
                commitment.input_fact_sha256,
                commitment.expected_fact_sha256,
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(ReplayCommitmentAuthority {
        source_commitments,
        case_commitments,
    })
}

pub(super) fn sanitized_json_from_raw(
    raw: &Value,
    require_complete: bool,
    authority: &ReplayCommitmentAuthority,
) -> anyhow::Result<Value> {
    require_object_keys(
        raw,
        &[
            "scope",
            "automatic_retries",
            "source_commitments",
            "oracle_binding",
            "harnesses",
            "reason_codes",
            "status",
        ],
        "raw conformance evidence",
    )?;
    if raw.get("scope").and_then(Value::as_str)
        != Some("exact_release_harnesses_built_in_fresh_private_target")
        || raw.get("automatic_retries").and_then(Value::as_u64) != Some(0)
    {
        bail!("raw conformance evidence lost its execution authority");
    }
    validate_replayed_commitments(raw, require_complete, authority)?;
    let harnesses = raw
        .get("harnesses")
        .and_then(Value::as_array)
        .context("raw conformance evidence lacks its harness array")?;
    if harnesses.len() != HARNESSES.len() {
        bail!("raw conformance evidence has the wrong harness population");
    }
    let mut sanitized = Vec::with_capacity(harnesses.len());
    for (harness, kind) in harnesses.iter().zip(HARNESSES) {
        let expected_id = kind.id();
        require_object_keys(
            harness,
            &[
                "id",
                "exact_command",
                "captures",
                "process",
                "marker_receipt",
                "parsed_marker",
                "reason_codes",
                "status",
            ],
            "raw conformance harness",
        )?;
        if harness.get("id").and_then(Value::as_str) != Some(expected_id) {
            bail!("raw conformance harness order or identity changed");
        }
        if harness.get("exact_command") != Some(&exact_command_json(kind)) {
            bail!("raw conformance harness command changed");
        }
        let captures = harness
            .get("captures")
            .context("raw conformance harness lacks captures")?;
        require_object_keys(
            captures,
            &[
                "stdout_file",
                "stderr_file",
                "stdout_sha256",
                "stderr_sha256",
            ],
            "raw conformance captures",
        )?;
        let stdout_file = format!("conformance/{expected_id}.stdout.log");
        let stderr_file = format!("conformance/{expected_id}.stderr.log");
        if captures.get("stdout_file").and_then(Value::as_str) != Some(stdout_file.as_str())
            || captures.get("stderr_file").and_then(Value::as_str) != Some(stderr_file.as_str())
        {
            bail!("raw conformance capture locations changed");
        }
        for field in ["stdout_sha256", "stderr_sha256"] {
            if let Some(digest) = captures.get(field).and_then(Value::as_str)
                && (digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
            {
                bail!("raw conformance capture digest is malformed");
            }
            if !matches!(captures.get(field), Some(Value::Null | Value::String(_))) {
                bail!("raw conformance capture digest has the wrong type");
            }
        }
        let reasons = harness
            .get("reason_codes")
            .and_then(Value::as_array)
            .context("raw conformance harness lacks reason codes")?;
        if reasons.iter().any(|reason| {
            reason
                .as_str()
                .is_none_or(|reason| !is_path_free_id(reason))
        }) || harness.get("status").and_then(Value::as_str)
            != Some(if reasons.is_empty() {
                "passed"
            } else {
                "failed"
            })
        {
            bail!("raw conformance harness status is inconsistent");
        }
        validate_replayed_harness(kind, harness, captures, require_complete)?;
        let explicit_oracle_gap = expected_id == HarnessKind::NumericalPerspectiveDvr.id()
            && harness
                .pointer("/parsed_marker/result")
                .and_then(Value::as_str)
                == Some("failed");
        sanitized.push(json!({
            "id": expected_id,
            "exact_command": harness.get("exact_command").cloned().context("raw conformance harness lacks exact command")?,
            "stdout_sha256": captures.get("stdout_sha256").cloned().context("raw conformance harness lacks stdout digest")?,
            "stderr_sha256": captures.get("stderr_sha256").cloned().context("raw conformance harness lacks stderr digest")?,
            "process": harness.get("process").cloned().context("raw conformance harness lacks process receipt")?,
            "marker": harness.get("marker_receipt").cloned().context("raw conformance harness lacks marker receipt")?,
            "explicit_frozen_oracle_gap_observed": explicit_oracle_gap,
            "reason_codes": harness.get("reason_codes").cloned().context("raw conformance harness lacks reason codes")?,
            "status": harness.get("status").cloned().context("raw conformance harness lacks status")?,
        }));
    }
    let reasons = raw
        .get("reason_codes")
        .and_then(Value::as_array)
        .context("raw conformance evidence lacks reason codes")?;
    if reasons.iter().any(|reason| {
        reason
            .as_str()
            .is_none_or(|reason| !is_path_free_id(reason))
    }) || raw.get("status").and_then(Value::as_str)
        != Some(if reasons.is_empty() {
            "passed"
        } else {
            "failed"
        })
    {
        bail!("raw conformance evidence status is inconsistent");
    }
    let mut expected_reasons = harnesses
        .iter()
        .filter_map(|harness| harness.get("reason_codes").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();
    if raw
        .pointer("/oracle_binding/all_six_frozen_cases_bound")
        .and_then(Value::as_bool)
        != Some(true)
    {
        expected_reasons.insert("conformance_not_all_frozen_oracle_cases_bound".to_owned());
    }
    let observed_reasons = reasons
        .iter()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();
    if expected_reasons != observed_reasons {
        bail!("raw conformance aggregate reasons do not reconcile with its harnesses");
    }
    Ok(json!({
        "scope": raw.get("scope").cloned().expect("scope was validated"),
        "automatic_retries": 0,
        "source_commitments": raw.get("source_commitments").cloned().context("raw conformance evidence lacks source commitments")?,
        "oracle_binding": raw.get("oracle_binding").cloned().context("raw conformance evidence lacks oracle binding")?,
        "harnesses": sanitized,
        "reason_codes": raw.get("reason_codes").cloned().context("raw conformance evidence lacks reason codes")?,
        "status": raw.get("status").cloned().context("raw conformance evidence lacks status")?,
    }))
}

fn validate_replayed_commitments(
    raw: &Value,
    require_complete: bool,
    authority: &ReplayCommitmentAuthority,
) -> anyhow::Result<()> {
    let sources = raw
        .get("source_commitments")
        .and_then(Value::as_array)
        .context("raw conformance evidence lacks source commitments")?;
    if authority.source_commitments.len() != SOURCE_FILES.len()
        || sources.len() != authority.source_commitments.len()
    {
        bail!("raw conformance source commitment population changed");
    }
    for ((source, fixed_path), (expected_path, expected_sha256)) in sources
        .iter()
        .zip(SOURCE_FILES)
        .zip(&authority.source_commitments)
    {
        require_object_keys(
            source,
            &["repository_relative_path", "sha256"],
            "raw conformance source commitment",
        )?;
        if source
            .get("repository_relative_path")
            .and_then(Value::as_str)
            != Some(fixed_path)
            || expected_path != fixed_path
            || source.get("sha256").and_then(Value::as_str) != Some(expected_sha256.as_str())
        {
            bail!("raw conformance source commitment differs from its measurement revision");
        }
    }
    let oracle = raw
        .get("oracle_binding")
        .context("raw conformance evidence lacks oracle binding")?;
    require_object_keys(
        oracle,
        &[
            "bound_case_ids",
            "all_six_frozen_cases_bound",
            "case_commitments",
        ],
        "raw conformance oracle binding",
    )?;
    let bound = oracle
        .get("bound_case_ids")
        .and_then(Value::as_array)
        .context("raw conformance bound-case IDs are malformed")?;
    let bound_ids = bound
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|id| is_path_free_id(id))
                .context("raw conformance bound-case ID is malformed")
        })
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    if bound_ids.len() != bound.len() {
        bail!("raw conformance bound-case IDs are duplicated");
    }
    let commitments = oracle
        .get("case_commitments")
        .and_then(Value::as_array)
        .context("raw conformance case commitments are malformed")?;
    if commitments.len() != authority.case_commitments.len() {
        bail!("raw conformance case commitment population changed");
    }
    let mut commitment_ids = BTreeSet::new();
    for (commitment, (expected_id, expected_input, expected_output)) in
        commitments.iter().zip(&authority.case_commitments)
    {
        require_object_keys(
            commitment,
            &["id", "input_fact_sha256", "expected_fact_sha256"],
            "raw conformance case commitment",
        )?;
        let id = commitment
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| is_path_free_id(id))
            .context("raw conformance case commitment ID is malformed")?;
        if !commitment_ids.insert(id)
            || id != expected_id
            || commitment.get("input_fact_sha256").and_then(Value::as_str)
                != Some(expected_input.as_str())
            || commitment
                .get("expected_fact_sha256")
                .and_then(Value::as_str)
                != Some(expected_output.as_str())
        {
            bail!("raw conformance case commitment differs from its external oracle");
        }
    }
    let authority_ids = authority
        .case_commitments
        .iter()
        .map(|(id, _, _)| id.as_str())
        .collect::<BTreeSet<_>>();
    if commitment_ids != authority_ids || !bound_ids.is_subset(&authority_ids) {
        bail!("raw conformance oracle binding differs from its external authority");
    }
    let all_six = authority_ids.len() == 6 && bound_ids == authority_ids;
    if oracle
        .get("all_six_frozen_cases_bound")
        .and_then(Value::as_bool)
        != Some(all_six)
        || (require_complete && !all_six)
    {
        bail!("raw conformance oracle binding is internally inconsistent");
    }
    Ok(())
}

fn validate_replayed_harness(
    kind: HarnessKind,
    harness: &Value,
    captures: &Value,
    require_complete: bool,
) -> anyhow::Result<()> {
    let process = harness
        .get("process")
        .context("raw conformance harness lacks process receipt")?;
    require_object_keys(
        process,
        &[
            "exit_code",
            "signal",
            "timed_out",
            "wall_time_ns",
            "spawn_failed",
        ],
        "raw conformance process receipt",
    )?;
    if !matches!(
        process.get("exit_code"),
        Some(Value::Null | Value::Number(_))
    ) || !matches!(process.get("signal"), Some(Value::Null | Value::Number(_)))
        || process.get("timed_out").and_then(Value::as_bool).is_none()
        || process
            .get("wall_time_ns")
            .and_then(Value::as_u64)
            .is_none()
        || process
            .get("spawn_failed")
            .and_then(Value::as_bool)
            .is_none()
    {
        bail!("raw conformance process receipt is malformed");
    }
    let marker = harness
        .get("parsed_marker")
        .context("raw conformance marker is absent")?;
    let marker_receipt = harness
        .get("marker_receipt")
        .context("raw conformance marker receipt is absent")?;
    require_object_keys(
        marker_receipt,
        &["schema", "result", "canonical_sha256", "parsed"],
        "raw conformance marker receipt",
    )?;
    let derived = match marker {
        Value::Null => json!({
            "schema": null,
            "result": null,
            "canonical_sha256": null,
            "parsed": false,
        }),
        marker => json!({
            "schema": marker.get("schema").and_then(Value::as_str),
            "result": marker.get("result").and_then(Value::as_str),
            "canonical_sha256": Sha256Hasher::digest(&serde_json::to_vec(marker)?).to_string(),
            "parsed": true,
        }),
    };
    if marker_receipt != &derived {
        bail!("raw conformance marker receipt is not derived from its parsed marker");
    }
    if require_complete {
        let marker = marker
            .as_object()
            .context("complete raw conformance harness lacks its parsed marker")?;
        let marker_result = marker.get("result").and_then(Value::as_str);
        let expected_exit = match kind {
            HarnessKind::NumericalPlaneMipIso if marker_result == Some("passed") => 0,
            HarnessKind::NumericalPerspectiveDvr if marker_result == Some("passed") => 0,
            HarnessKind::NumericalPerspectiveDvr if marker_result == Some("failed") => 101,
            HarnessKind::ProductionShaderAudit if marker_result.is_none() => 0,
            HarnessKind::TimestampPlacementControls | HarnessKind::QueueWriteEnvelopeControl
                if marker_result == Some("measured") =>
            {
                0
            }
            _ => bail!("complete raw conformance harness marker result is incoherent"),
        };
        let reasons = harness
            .get("reason_codes")
            .and_then(Value::as_array)
            .expect("conformance reason codes were validated");
        if reasons.iter().any(|reason| {
            reason
                .as_str()
                .is_none_or(|reason| !is_known_product_reason(reason))
        }) || (kind == HarnessKind::NumericalPerspectiveDvr
            && marker_result == Some("failed")
            && !reasons.iter().any(|reason| {
                reason.as_str() == Some("conformance_dvr_frozen_world_distance_oracle_failed")
            }))
        {
            bail!("complete raw conformance harness contains incoherent reason codes");
        }
        if marker.get("schema").and_then(Value::as_str) != Some(kind.expected_schema())
            || process.get("signal") != Some(&Value::Null)
            || process.get("timed_out").and_then(Value::as_bool) != Some(false)
            || process.get("spawn_failed").and_then(Value::as_bool) != Some(false)
            || process.get("exit_code").and_then(Value::as_i64) != Some(expected_exit)
            || !is_sha256(captures.get("stdout_sha256"))
            || !is_sha256(captures.get("stderr_sha256"))
        {
            bail!("complete raw conformance harness process or marker is incoherent");
        }
    }
    Ok(())
}

pub(super) fn is_known_product_reason(reason: &str) -> bool {
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

fn is_sha256(value: Option<&Value>) -> bool {
    value.and_then(Value::as_str).is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn is_path_free_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn require_object_keys(value: &Value, expected: &[&str], label: &str) -> anyhow::Result<()> {
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

pub(super) fn execute(
    repository_root: &Path,
    fresh_target: &Path,
    result_root: &Path,
    run_timeout: Duration,
    oracle_cases: &[Value],
    numerical_contract: &Value,
) -> anyhow::Result<ConformanceEvidence> {
    let source_commitments = source_commitments(repository_root)?;
    let case_commitments = oracle_cases
        .iter()
        .map(case_commitment)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let source_map = source_commitments
        .iter()
        .map(|source| (source.repository_relative_path, source.sha256.as_str()))
        .collect::<BTreeMap<_, _>>();
    let conformance_root = result_root.join("conformance");
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(&conformance_root)
        .context("failed to create private conformance evidence directory")?;

    let timeout = run_timeout.min(MAX_HARNESS_TIMEOUT);
    let mut harnesses = Vec::with_capacity(HARNESSES.len());
    let mut bound_case_ids = BTreeSet::new();
    let mut reasons = BTreeSet::new();
    for kind in HARNESSES {
        let mut receipt = execute_harness(
            kind,
            repository_root,
            fresh_target,
            &conformance_root,
            timeout,
        )?;
        validate_harness(
            &mut receipt,
            oracle_cases,
            numerical_contract,
            &source_map,
            &mut bound_case_ids,
        );
        reasons.extend(receipt.reasons.iter().cloned());
        harnesses.push(receipt);
    }
    if bound_case_ids.len() != 6 {
        reasons.insert("conformance_not_all_frozen_oracle_cases_bound".to_owned());
    }
    Ok(ConformanceEvidence {
        source_commitments,
        case_commitments,
        harnesses,
        bound_case_ids,
        reasons,
    })
}

fn case_commitment(case: &Value) -> anyhow::Result<CaseCommitment> {
    Ok(CaseCommitment {
        id: case
            .get("id")
            .and_then(Value::as_str)
            .context("numerical oracle case ID is unavailable")?
            .to_owned(),
        input_fact_sha256: case
            .get("input_fact_sha256")
            .and_then(Value::as_str)
            .context("numerical oracle input commitment is unavailable")?
            .to_owned(),
        expected_fact_sha256: case
            .get("expected_fact_sha256")
            .and_then(Value::as_str)
            .context("numerical oracle expected commitment is unavailable")?
            .to_owned(),
    })
}

fn source_commitments(repository_root: &Path) -> anyhow::Result<Vec<SourceCommitment>> {
    SOURCE_FILES
        .into_iter()
        .map(|relative| {
            let bytes = super::read_bounded_regular_file(
                &repository_root.join(relative),
                MAX_CAPTURE_BYTES,
                "EP-00 conformance source",
            )?;
            Ok(SourceCommitment {
                repository_relative_path: relative,
                sha256: Sha256Hasher::digest(&bytes).to_string(),
            })
        })
        .collect()
}

fn execute_harness(
    kind: HarnessKind,
    repository_root: &Path,
    fresh_target: &Path,
    conformance_root: &Path,
    timeout: Duration,
) -> anyhow::Result<HarnessReceipt> {
    let stdout_path = conformance_root.join(format!("{}.stdout.log", kind.id()));
    let stderr_path = conformance_root.join(format!("{}.stderr.log", kind.id()));
    let stdout = create_capture(&stdout_path)?;
    let stderr = create_capture(&stderr_path)?;
    let mut command = exact_release_test_command(kind, repository_root, fresh_target);
    command
        .process_group(0)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let process = run_bounded(&mut command, timeout);
    let stdout = read_capture(&stdout_path)?;
    let stderr = read_capture(&stderr_path)?;
    let stdout_sha256 = Some(Sha256Hasher::digest(&stdout).to_string());
    let stderr_sha256 = Some(Sha256Hasher::digest(&stderr).to_string());
    let mut reasons = BTreeSet::new();
    if process.spawn_failed {
        reasons.insert(format!("conformance_{}_spawn_failed", kind.id()));
    }
    if process.timed_out {
        reasons.insert(format!("conformance_{}_timed_out", kind.id()));
    }
    let marker = match extract_unique_marker(&stdout, kind.marker()) {
        Ok(marker) => Some(marker),
        Err(_) => {
            reasons.insert(format!(
                "conformance_{}_marker_missing_or_invalid",
                kind.id()
            ));
            None
        }
    };
    let marker_receipt = marker_receipt(marker.as_ref());
    Ok(HarnessReceipt {
        kind,
        process,
        stdout_sha256,
        stderr_sha256,
        marker,
        marker_receipt,
        reasons,
    })
}

fn exact_release_test_command(
    kind: HarnessKind,
    repository_root: &Path,
    fresh_target: &Path,
) -> Command {
    let mut command = cargo_command();
    command
        .current_dir(repository_root)
        .env("RUSTC", "rustc")
        .env("RUSTFLAGS", "")
        .env("CARGO_ENCODED_RUSTFLAGS", "")
        .env("RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "")
        .env("CARGO_TERM_COLOR", "never")
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
        .args(["test", "--locked", "--release", "--target-dir"])
        .arg(fresh_target)
        .args([
            "-p",
            "mirante4d-render-wgpu",
            "--lib",
            kind.test_name(),
            "--",
            "--ignored",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ]);
    command
}

fn create_capture(path: &Path) -> anyhow::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .context("failed to create private conformance process capture")
}

fn read_capture(path: &Path) -> anyhow::Result<Vec<u8>> {
    super::read_bounded_regular_file(path, MAX_CAPTURE_BYTES, "EP-00 conformance process capture")
}

fn run_bounded(command: &mut Command, timeout: Duration) -> ProcessReceipt {
    let started = Instant::now();
    let Ok(mut child) = command.spawn() else {
        return ProcessReceipt {
            exit_code: None,
            signal: None,
            timed_out: false,
            wall_time_ns: elapsed_ns(started),
            spawn_failed: true,
        };
    };
    let deadline = started + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return process_receipt(status, false, started),
            Ok(None) => {}
            Err(_) => {
                terminate_process_group(&mut child);
                let status = child.wait().ok();
                return status.map_or(
                    ProcessReceipt {
                        exit_code: None,
                        signal: None,
                        timed_out: false,
                        wall_time_ns: elapsed_ns(started),
                        spawn_failed: true,
                    },
                    |status| process_receipt(status, false, started),
                );
            }
        }
        if Instant::now() >= deadline {
            terminate_process_group(&mut child);
            let status = child.wait().ok();
            return status.map_or(
                ProcessReceipt {
                    exit_code: None,
                    signal: None,
                    timed_out: true,
                    wall_time_ns: elapsed_ns(started),
                    spawn_failed: false,
                },
                |status| process_receipt(status, true, started),
            );
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn process_receipt(status: ExitStatus, timed_out: bool, started: Instant) -> ProcessReceipt {
    ProcessReceipt {
        exit_code: status.code(),
        signal: status.signal(),
        timed_out,
        wall_time_ns: elapsed_ns(started),
        spawn_failed: false,
    }
}

fn terminate_process_group(child: &mut Child) {
    let group = -(i32::try_from(child.id()).unwrap_or(i32::MAX));
    // SAFETY: every harness command is placed in a new process group before
    // spawning, so the negative PID targets only that command tree.
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

fn extract_unique_marker(stdout: &[u8], prefix: &str) -> anyhow::Result<Value> {
    let stdout = std::str::from_utf8(stdout).context("conformance stdout was not UTF-8")?;
    let mut matches = stdout.match_indices(prefix);
    let (offset, _) = matches
        .next()
        .context("conformance marker prefix was absent")?;
    if matches.next().is_some() {
        bail!("conformance marker prefix occurred more than once")
    }
    let payload = &stdout[offset + prefix.len()..];
    serde_json::Deserializer::from_str(payload)
        .into_iter::<Value>()
        .next()
        .context("conformance marker had no JSON value")?
        .context("conformance marker JSON was malformed")
}

fn marker_receipt(marker: Option<&Value>) -> ParsedMarkerReceipt {
    ParsedMarkerReceipt {
        schema: marker
            .and_then(|value| value.get("schema"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        result: marker
            .and_then(|value| value.get("result"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        canonical_sha256: marker.and_then(|marker| {
            serde_json::to_vec(marker)
                .ok()
                .map(|bytes| Sha256Hasher::digest(&bytes).to_string())
        }),
        parsed: marker.is_some(),
    }
}

fn validate_harness(
    receipt: &mut HarnessReceipt,
    oracle_cases: &[Value],
    numerical_contract: &Value,
    source_map: &BTreeMap<&str, &str>,
    bound_case_ids: &mut BTreeSet<String>,
) {
    let id = receipt.kind.id();
    let Some(marker) = receipt.marker.as_ref() else {
        if receipt.process.exit_code != Some(0) {
            receipt
                .reasons
                .insert(format!("conformance_{id}_process_failed"));
        }
        return;
    };
    if marker.get("schema").and_then(Value::as_str) != Some(receipt.kind.expected_schema()) {
        receipt
            .reasons
            .insert(format!("conformance_{id}_marker_schema_mismatch"));
    }
    match receipt.kind {
        HarnessKind::NumericalPlaneMipIso => validate_primary_numerical_marker(
            marker,
            oracle_cases,
            numerical_contract,
            &mut receipt.reasons,
            bound_case_ids,
        ),
        HarnessKind::NumericalPerspectiveDvr => validate_dvr_marker(
            marker,
            oracle_cases,
            numerical_contract,
            &mut receipt.reasons,
            bound_case_ids,
        ),
        HarnessKind::ProductionShaderAudit => {
            validate_shader_marker(marker, source_map, &mut receipt.reasons)
        }
        HarnessKind::TimestampPlacementControls => {
            validate_timestamp_marker(marker, &mut receipt.reasons)
        }
        HarnessKind::QueueWriteEnvelopeControl => {
            validate_queue_write_marker(marker, &mut receipt.reasons)
        }
    }
    let exact_numerical_product_failure = matches!(
        receipt.kind,
        HarnessKind::NumericalPlaneMipIso | HarnessKind::NumericalPerspectiveDvr
    ) && receipt.process.exit_code == Some(101)
        && !receipt.process.timed_out
        && !receipt.process.spawn_failed
        && marker.get("schema").and_then(Value::as_str) == Some(receipt.kind.expected_schema())
        && marker.get("result").and_then(Value::as_str) == Some("failed");
    if receipt.process.exit_code != Some(0) && !exact_numerical_product_failure {
        receipt
            .reasons
            .insert(format!("conformance_{id}_process_failed"));
    }
}

fn validate_primary_numerical_marker(
    marker: &Value,
    cases: &[Value],
    contract: &Value,
    reasons: &mut BTreeSet<String>,
    bound: &mut BTreeSet<String>,
) {
    if marker.get("result").and_then(Value::as_str) != Some("passed") {
        reasons.insert("conformance_primary_numerical_result_not_passed".to_owned());
    }
    for field in [
        "scalar_absolute_tolerance",
        "scalar_relative_tolerance",
        "premultiplied_rgba_absolute_tolerance",
        "world_position_absolute_tolerance",
        "ray_distance_absolute_tolerance",
        "rgba8_channel_tolerance",
    ] {
        check_equal(
            marker.get(field),
            contract.get(field),
            &format!("conformance_contract_{field}_mismatch"),
            reasons,
        );
    }
    for spec in [
        CaseSpec::plane("plane_smooth_valid"),
        CaseSpec::plane("plane_smooth_invalid"),
        CaseSpec::volume("perspective_mip", "mip"),
        CaseSpec::volume("perspective_iso", "iso"),
        CaseSpec::volume("perspective_iso_depth_order", "iso"),
    ] {
        bind_case(marker, cases, contract, spec, reasons, bound);
    }
}

fn validate_dvr_marker(
    marker: &Value,
    cases: &[Value],
    contract: &Value,
    reasons: &mut BTreeSet<String>,
    bound: &mut BTreeSet<String>,
) {
    if marker.get("case").and_then(Value::as_str) != Some("perspective_dvr_world_distance") {
        reasons.insert("conformance_dvr_marker_case_mismatch".to_owned());
        return;
    }
    bind_case(marker, cases, contract, CaseSpec::dvr(), reasons, bound);
    if marker.get("result").and_then(Value::as_str) != Some("passed") {
        reasons.insert("conformance_dvr_frozen_world_distance_oracle_failed".to_owned());
    }
    if marker.get("coverage_matches").and_then(Value::as_bool) != Some(true) {
        reasons.insert("conformance_dvr_observed_coverage_mismatch".to_owned());
    }
    if marker.get("validity_matches").and_then(Value::as_bool) != Some(true) {
        reasons.insert("conformance_dvr_observed_validity_mismatch".to_owned());
    }
}

#[derive(Clone, Copy)]
struct CaseSpec {
    id: &'static str,
    marker_prefix: &'static str,
    sampling: &'static str,
    mode: &'static str,
    dvr_gap_shape: bool,
}

impl CaseSpec {
    const fn plane(id: &'static str) -> Self {
        Self {
            id,
            marker_prefix: id,
            sampling: "smooth_linear",
            mode: "plane",
            dvr_gap_shape: false,
        }
    }

    const fn volume(id: &'static str, mode: &'static str) -> Self {
        Self {
            id,
            marker_prefix: id,
            sampling: "voxel_exact",
            mode,
            dvr_gap_shape: false,
        }
    }

    const fn dvr() -> Self {
        Self {
            id: "perspective_dvr_world_distance",
            marker_prefix: "expected",
            sampling: "voxel_exact",
            mode: "dvr",
            dvr_gap_shape: true,
        }
    }
}

fn bind_case(
    marker: &Value,
    cases: &[Value],
    contract: &Value,
    spec: CaseSpec,
    reasons: &mut BTreeSet<String>,
    bound: &mut BTreeSet<String>,
) {
    let Some(case) = cases
        .iter()
        .find(|case| case.get("id").and_then(Value::as_str) == Some(spec.id))
    else {
        reasons.insert(format!("conformance_{}_oracle_case_missing", spec.id));
        return;
    };
    let before = reasons.len();
    match expected_case_sha256(case) {
        Some(observed)
            if case.get("expected_fact_sha256").and_then(Value::as_str)
                == Some(observed.as_str()) => {}
        _ => {
            reasons.insert(format!(
                "conformance_{}_expected_fact_commitment_mismatch",
                spec.id
            ));
        }
    }
    check_equal(
        case.get("sampling"),
        Some(&Value::String(spec.sampling.to_owned())),
        &format!("conformance_{}_sampling_mismatch", spec.id),
        reasons,
    );
    check_equal(
        case.get("mode"),
        Some(&Value::String(spec.mode.to_owned())),
        &format!("conformance_{}_mode_mismatch", spec.id),
        reasons,
    );

    let prefix = spec.marker_prefix;
    let field = |suffix: &str| format!("{prefix}_{suffix}");
    let marker_pixel = if spec.dvr_gap_shape {
        marker.get("pixel")
    } else {
        marker.get(field("pixel"))
    };
    check_equal(
        marker_pixel,
        case.get("pixel"),
        &format!("conformance_{}_pixel_mismatch", spec.id),
        reasons,
    );
    check_equal(
        marker.get(field("rgba8")),
        case.get("expected_rgba8"),
        &format!("conformance_{}_rgba8_mismatch", spec.id),
        reasons,
    );
    check_float_array(
        marker.get(field("premultiplied_rgba")),
        case.get("expected_premultiplied_rgba"),
        contract
            .get("premultiplied_rgba_absolute_tolerance")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        &format!("conformance_{}_premultiplied_rgba_mismatch", spec.id),
        reasons,
    );
    check_equal(
        marker.get(field("covered")),
        case.get("covered"),
        &format!("conformance_{}_coverage_mismatch", spec.id),
        reasons,
    );
    check_equal(
        marker.get(field("valid")),
        case.get("valid"),
        &format!("conformance_{}_validity_mismatch", spec.id),
        reasons,
    );

    if spec.id == "perspective_iso_depth_order" {
        check_equal(
            marker.get(field("authored_layers")),
            case.get("authored_order"),
            "conformance_perspective_iso_depth_order_authored_order_mismatch",
            reasons,
        );
        check_equal(
            marker.get(field("source_order")),
            case.get("source_order"),
            "conformance_perspective_iso_depth_order_source_order_mismatch",
            reasons,
        );
    } else {
        check_equal(
            case.get("authored_order"),
            Some(&json!([0])),
            &format!(
                "conformance_{}_single_layer_authored_order_mismatch",
                spec.id
            ),
            reasons,
        );
        check_equal(
            case.get("source_order"),
            Some(&json!([0])),
            &format!("conformance_{}_single_layer_source_order_mismatch", spec.id),
            reasons,
        );
    }

    let marker_depth = marker.get(field("hit_depth_world"));
    check_optional_float(
        marker_depth,
        case.get("hit_depth_world"),
        contract
            .get("ray_distance_absolute_tolerance")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        &format!("conformance_{}_hit_depth_mismatch", spec.id),
        reasons,
    );

    match case.get("pick") {
        Some(Value::Object(pick)) => {
            let pick_prefix = if spec.id == "perspective_iso_depth_order" {
                "perspective_iso_near_pick"
            } else if spec.dvr_gap_shape {
                "expected_pick"
            } else {
                return bind_regular_pick(marker, case, contract, spec, before, reasons, bound);
            };
            bind_pick_fields(marker, pick, pick_prefix, contract, spec.id, reasons);
            if reasons.len() == before {
                bound.insert(spec.id.to_owned());
            }
        }
        Some(Value::Null) | None => {
            if spec.id != "plane_smooth_valid" && spec.id != "plane_smooth_invalid" {
                reasons.insert(format!("conformance_{}_pick_missing", spec.id));
            }
            if reasons.len() == before {
                bound.insert(spec.id.to_owned());
            }
        }
        Some(_) => {
            reasons.insert(format!("conformance_{}_pick_malformed", spec.id));
        }
    }
}

fn expected_case_sha256(case: &Value) -> Option<String> {
    let expected = json!({
        "id": case.get("id")?,
        "pixel": case.get("pixel")?,
        "sampling": case.get("sampling")?,
        "mode": case.get("mode")?,
        "rgba8": case.get("expected_rgba8")?,
        "premultiplied_rgba": case.get("expected_premultiplied_rgba")?,
        "covered": case.get("covered")?,
        "valid": case.get("valid")?,
        "hit_depth_world": case.get("hit_depth_world")?,
        "pick": case.get("pick")?,
        "authored_order": case.get("authored_order")?,
        "source_order": case.get("source_order")?,
    });
    let mut bytes = EXPECTED_FACT_DOMAIN.to_vec();
    write_canonical_json(&expected, &mut bytes).ok()?;
    Some(Sha256Hasher::digest(&bytes).to_string())
}

fn write_canonical_json(value: &Value, output: &mut Vec<u8>) -> anyhow::Result<()> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => {
            output.extend_from_slice(
                serde_json::to_string(value)
                    .context("failed to encode a canonical numerical fact scalar")?
                    .as_bytes(),
            );
        }
        Value::Number(number) => write_canonical_number(number, output)?,
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_canonical_json(value, output)?;
            }
            output.push(b']');
        }
        Value::Object(values) => {
            output.push(b'{');
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                output.extend_from_slice(
                    serde_json::to_string(key)
                        .context("failed to encode a canonical numerical fact key")?
                        .as_bytes(),
                );
                output.push(b':');
                write_canonical_json(&values[key], output)?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}

fn write_canonical_number(number: &serde_json::Number, output: &mut Vec<u8>) -> anyhow::Result<()> {
    if number.is_i64() || number.is_u64() {
        output.extend_from_slice(number.to_string().as_bytes());
        return Ok(());
    }
    let value = number
        .as_f64()
        .context("canonical numerical fact contains an unsupported number")?;
    if !value.is_finite() {
        bail!("canonical numerical fact contains a non-finite number")
    }
    if value == 0.0 {
        output.push(b'0');
    } else if value.fract() == 0.0 && value.abs() < 1.0e21 {
        output.extend_from_slice(format!("{value:.0}").as_bytes());
    } else {
        output.extend_from_slice(number.to_string().as_bytes());
    }
    Ok(())
}

pub(super) fn validate_oracle_case_commitments(cases: &[Value]) -> anyhow::Result<()> {
    for case in cases {
        let expected = case
            .get("expected_fact_sha256")
            .and_then(Value::as_str)
            .context("numerical oracle expected commitment is unavailable")?;
        if expected_case_sha256(case).as_deref() != Some(expected) {
            bail!("numerical oracle expected fact commitment is not canonical")
        }
    }
    Ok(())
}

fn bind_regular_pick(
    marker: &Value,
    case: &Value,
    contract: &Value,
    spec: CaseSpec,
    before: usize,
    reasons: &mut BTreeSet<String>,
    bound: &mut BTreeSet<String>,
) {
    let pick = case
        .get("pick")
        .and_then(Value::as_object)
        .expect("the caller matched an object pick");
    bind_pick_fields(
        marker,
        pick,
        &format!("{}_pick", spec.marker_prefix),
        contract,
        spec.id,
        reasons,
    );
    if reasons.len() == before {
        bound.insert(spec.id.to_owned());
    }
}

fn bind_pick_fields(
    marker: &Value,
    pick: &serde_json::Map<String, Value>,
    prefix: &str,
    contract: &Value,
    id: &str,
    reasons: &mut BTreeSet<String>,
) {
    check_equal(
        marker.get(format!("{prefix}_kind")),
        pick.get("kind"),
        &format!("conformance_{id}_pick_kind_mismatch"),
        reasons,
    );
    check_equal(
        marker.get(format!("{prefix}_complete")),
        pick.get("complete"),
        &format!("conformance_{id}_pick_completeness_mismatch"),
        reasons,
    );
    check_float(
        marker.get(format!("{prefix}_value")),
        pick.get("value"),
        contract
            .get("scalar_absolute_tolerance")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        contract
            .get("scalar_relative_tolerance")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        &format!("conformance_{id}_pick_value_mismatch"),
        reasons,
    );
    check_float_array(
        marker.get(format!("{prefix}_world")),
        pick.get("world"),
        contract
            .get("world_position_absolute_tolerance")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        &format!("conformance_{id}_pick_world_mismatch"),
        reasons,
    );
    check_float(
        marker.get(format!("{prefix}_distance_world")),
        pick.get("distance_world"),
        contract
            .get("ray_distance_absolute_tolerance")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        0.0,
        &format!("conformance_{id}_pick_distance_mismatch"),
        reasons,
    );
}

fn validate_shader_marker(
    marker: &Value,
    source_map: &BTreeMap<&str, &str>,
    reasons: &mut BTreeSet<String>,
) {
    if marker.get("source_module").and_then(Value::as_str) != Some("production_render_wgsl")
        || marker.get("source_sha256").and_then(Value::as_str)
            != source_map
                .get("crates/mirante4d-render-wgpu/src/shader.wgsl")
                .copied()
    {
        reasons.insert("conformance_shader_audit_source_binding_mismatch".to_owned());
    }
    if marker.get("fragment_entry_count").and_then(Value::as_u64) != Some(4)
        || marker
            .get("fragment_entries")
            .and_then(Value::as_array)
            .is_none_or(|entries| entries.len() != 4)
    {
        reasons.insert("conformance_shader_audit_entry_inventory_mismatch".to_owned());
    }
    let required_unavailable = "unavailable_without_external_vendor_profiler";
    for fact in [
        "registers",
        "private_memory_spills",
        "occupancy",
        "cache_behavior",
        "divergence",
    ] {
        if marker
            .pointer(&format!("/vendor_profiler_facts/{fact}"))
            .and_then(Value::as_str)
            != Some(required_unavailable)
        {
            reasons.insert(format!(
                "conformance_shader_audit_vendor_profiler_{fact}_rationale_missing"
            ));
        }
    }
}

fn validate_timestamp_marker(marker: &Value, reasons: &mut BTreeSet<String>) {
    for (field, expected) in [
        ("gpu_timestamps", Value::Bool(true)),
        ("payload_copy_timestamps", Value::Bool(true)),
        (
            "known_copy_timestamp_placement_control",
            Value::String("passed".to_owned()),
        ),
        (
            "empty_pass_unavailable_interval_control",
            Value::String("passed".to_owned()),
        ),
        ("result", Value::String("measured".to_owned())),
    ] {
        if marker.get(field) != Some(&expected) {
            reasons.insert(format!("conformance_timestamp_{field}_mismatch"));
        }
    }
    if marker
        .get("cold_payload_copy_ns")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
    {
        reasons.insert("conformance_timestamp_known_copy_interval_missing".to_owned());
    }
}

fn validate_queue_write_marker(marker: &Value, reasons: &mut BTreeSet<String>) {
    if marker.get("result").and_then(Value::as_str) != Some("measured")
        || marker
            .get("enclosing_interval_detected_queue_write")
            .and_then(Value::as_bool)
            != Some(true)
    {
        reasons.insert("conformance_queue_write_envelope_control_failed".to_owned());
    }
    let without = marker.get("no_control_p50_ns").and_then(Value::as_u64);
    let with = marker.get("queue_write_p50_ns").and_then(Value::as_u64);
    if without
        .zip(with)
        .is_none_or(|(without, with)| with <= without)
    {
        reasons.insert("conformance_queue_write_not_visible_in_enclosing_interval".to_owned());
    }
}

fn check_equal(
    observed: Option<&Value>,
    expected: Option<&Value>,
    reason: &str,
    reasons: &mut BTreeSet<String>,
) {
    if observed != expected {
        reasons.insert(reason.to_owned());
    }
}

fn check_optional_float(
    observed: Option<&Value>,
    expected: Option<&Value>,
    absolute_tolerance: f64,
    reason: &str,
    reasons: &mut BTreeSet<String>,
) {
    match (observed, expected) {
        (None, None | Some(Value::Null)) | (Some(Value::Null), None | Some(Value::Null)) => {}
        _ => check_float(observed, expected, absolute_tolerance, 0.0, reason, reasons),
    }
}

fn check_float(
    observed: Option<&Value>,
    expected: Option<&Value>,
    absolute_tolerance: f64,
    relative_tolerance: f64,
    reason: &str,
    reasons: &mut BTreeSet<String>,
) {
    let Some((observed, expected)) = observed
        .and_then(Value::as_f64)
        .zip(expected.and_then(Value::as_f64))
    else {
        reasons.insert(reason.to_owned());
        return;
    };
    let tolerance = absolute_tolerance.max(relative_tolerance * expected.abs());
    if !observed.is_finite() || !expected.is_finite() || (observed - expected).abs() > tolerance {
        reasons.insert(reason.to_owned());
    }
}

fn check_float_array(
    observed: Option<&Value>,
    expected: Option<&Value>,
    tolerance: f64,
    reason: &str,
    reasons: &mut BTreeSet<String>,
) {
    let Some((observed, expected)) = observed
        .and_then(Value::as_array)
        .zip(expected.and_then(Value::as_array))
    else {
        reasons.insert(reason.to_owned());
        return;
    };
    if observed.len() != expected.len()
        || observed.iter().zip(expected).any(|(observed, expected)| {
            observed
                .as_f64()
                .zip(expected.as_f64())
                .is_none_or(|(observed, expected)| {
                    !observed.is_finite()
                        || !expected.is_finite()
                        || (observed - expected).abs() > tolerance
                })
        })
    {
        reasons.insert(reason.to_owned());
    }
}

fn harness_raw_json(receipt: &HarnessReceipt) -> Value {
    json!({
        "id": receipt.kind.id(),
        "exact_command": exact_command_json(receipt.kind),
        "captures": {
            "stdout_file": format!("conformance/{}.stdout.log", receipt.kind.id()),
            "stderr_file": format!("conformance/{}.stderr.log", receipt.kind.id()),
            "stdout_sha256": receipt.stdout_sha256,
            "stderr_sha256": receipt.stderr_sha256,
        },
        "process": receipt.process,
        "marker_receipt": receipt.marker_receipt,
        "parsed_marker": receipt.marker,
        "reason_codes": receipt.reasons,
        "status": if receipt.reasons.is_empty() { "passed" } else { "failed" },
    })
}

#[cfg(test)]
fn harness_sanitized_json(receipt: &HarnessReceipt) -> Value {
    let explicit_oracle_gap = receipt.kind == HarnessKind::NumericalPerspectiveDvr
        && receipt
            .marker
            .as_ref()
            .and_then(|marker| marker.get("result"))
            .and_then(Value::as_str)
            == Some("failed");
    json!({
        "id": receipt.kind.id(),
        "exact_command": exact_command_json(receipt.kind),
        "stdout_sha256": receipt.stdout_sha256,
        "stderr_sha256": receipt.stderr_sha256,
        "process": receipt.process,
        "marker": receipt.marker_receipt,
        "explicit_frozen_oracle_gap_observed": explicit_oracle_gap,
        "reason_codes": receipt.reasons,
        "status": if receipt.reasons.is_empty() { "passed" } else { "failed" },
    })
}

fn exact_command_json(kind: HarnessKind) -> Value {
    json!({
        "cargo_subcommand": "test",
        "cargo_lock_mode": "locked",
        "profile": "release",
        "standard_release_environment": true,
        "package": "mirante4d-render-wgpu",
        "target_directory": "fresh-private-target",
        "test": kind.test_name(),
        "ignored": true,
        "exact": true,
        "nocapture": true,
        "test_threads": 1,
    })
}

#[cfg(test)]
pub(super) fn valid_test_evidence() -> ConformanceEvidence {
    let harnesses = HARNESSES
        .into_iter()
        .map(|kind| {
            let marker = match kind {
                HarnessKind::NumericalPlaneMipIso | HarnessKind::NumericalPerspectiveDvr => {
                    json!({"schema": kind.expected_schema(), "result": "passed"})
                }
                HarnessKind::ProductionShaderAudit => {
                    json!({"schema": kind.expected_schema()})
                }
                HarnessKind::TimestampPlacementControls
                | HarnessKind::QueueWriteEnvelopeControl => {
                    json!({"schema": kind.expected_schema(), "result": "measured"})
                }
            };
            HarnessReceipt {
                kind,
                process: ProcessReceipt {
                    exit_code: Some(0),
                    signal: None,
                    timed_out: false,
                    wall_time_ns: 1,
                    spawn_failed: false,
                },
                stdout_sha256: Some("11".repeat(32)),
                stderr_sha256: Some("22".repeat(32)),
                marker_receipt: marker_receipt(Some(&marker)),
                marker: Some(marker),
                reasons: BTreeSet::new(),
            }
        })
        .collect();
    let source_commitments = SOURCE_FILES
        .into_iter()
        .map(|repository_relative_path| SourceCommitment {
            repository_relative_path,
            sha256: "44".repeat(32),
        })
        .collect();
    let case_commitments = (0..6)
        .map(|index| CaseCommitment {
            id: format!("case-{index}"),
            input_fact_sha256: "55".repeat(32),
            expected_fact_sha256: "66".repeat(32),
        })
        .collect::<Vec<_>>();
    ConformanceEvidence {
        source_commitments,
        bound_case_ids: case_commitments
            .iter()
            .map(|commitment| commitment.id.clone())
            .collect(),
        case_commitments,
        harnesses,
        reasons: BTreeSet::new(),
    }
}

#[cfg(test)]
pub(super) fn test_replay_authority(evidence: &ConformanceEvidence) -> ReplayCommitmentAuthority {
    ReplayCommitmentAuthority {
        source_commitments: evidence
            .source_commitments
            .iter()
            .map(|commitment| {
                (
                    commitment.repository_relative_path.to_owned(),
                    commitment.sha256.clone(),
                )
            })
            .collect(),
        case_commitments: evidence
            .case_commitments
            .iter()
            .map(|commitment| {
                (
                    commitment.id.clone(),
                    commitment.input_fact_sha256.clone(),
                    commitment.expected_fact_sha256.clone(),
                )
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixed_pretty_json_is_parsed_without_scraping_test_noise() {
        let stdout = b"running 1 test\nmarker:{\n  \"schema\": \"case\",\n  \"result\": \"passed\"\n}\ntest result: ok\n";
        let marker = extract_unique_marker(stdout, "marker:").unwrap();
        assert_eq!(marker["schema"], "case");
        assert_eq!(marker["result"], "passed");
    }

    #[test]
    fn duplicate_or_missing_markers_fail_closed() {
        assert!(extract_unique_marker(b"no evidence", "marker:").is_err());
        assert!(extract_unique_marker(b"marker:{} marker:{}", "marker:").is_err());
    }

    #[test]
    fn expected_fact_canonicalization_is_independent_of_integer_float_json_spelling() {
        let integer_spelling = json!({
            "id": "case",
            "pixel": [0, 1],
            "sampling": "voxel_exact",
            "mode": "mip",
            "expected_rgba8": [0, 1, 2, 255],
            "expected_premultiplied_rgba": [0, 1, 0.5, 1],
            "covered": true,
            "valid": true,
            "hit_depth_world": 1,
            "pick": {
                "kind": "voxel",
                "value": 1,
                "world": [0, 1, 2],
                "distance_world": 1,
                "complete": true,
            },
            "authored_order": [0],
            "source_order": [0],
        });
        let mut float_spelling = integer_spelling.clone();
        float_spelling["expected_premultiplied_rgba"] = json!([0.0, 1.0, 0.5, 1.0]);
        float_spelling["hit_depth_world"] = json!(1.0);
        float_spelling["pick"]["value"] = json!(1.0);
        float_spelling["pick"]["world"] = json!([0.0, 1.0, 2.0]);
        float_spelling["pick"]["distance_world"] = json!(1.0);
        assert_eq!(
            expected_case_sha256(&integer_spelling),
            expected_case_sha256(&float_spelling)
        );
    }

    #[test]
    fn harness_inventory_is_exact_and_uses_one_test_thread() {
        assert_eq!(HARNESSES.len(), 5);
        let names = HARNESSES
            .iter()
            .map(|kind| kind.test_name())
            .collect::<BTreeSet<_>>();
        assert_eq!(names.len(), HARNESSES.len());
        assert!(names.contains(
            "gpu_tests::ep00_plane_mip_iso_depth_and_pick_match_independent_numerical_oracle"
        ));
        assert!(
            names.contains("gpu_tests::ep00_off_axis_perspective_dvr_uses_physical_world_distance")
        );
        assert!(names.contains("shader_audit::emit_production_shader_structural_audit_json"));
        assert!(names.contains("gpu_tests::resident_volume_gpu_timing"));
        assert!(names.contains("gpu_tests::queue_write_batch_envelope_gpu_timing_control"));
    }

    #[test]
    fn numerical_markers_bind_all_six_frozen_oracle_cases() {
        let contract = json!({
            "scalar_absolute_tolerance": 1.0e-6,
            "scalar_relative_tolerance": 4.76837158203125e-7,
            "premultiplied_rgba_absolute_tolerance": 2.0e-6,
            "world_position_absolute_tolerance": 1.0e-5,
            "ray_distance_absolute_tolerance": 1.0e-5,
            "rgba8_channel_tolerance": 1,
        });
        let pick = || {
            json!({
                "kind": "voxel",
                "value": 0.75,
                "world": [1.0, 2.0, 3.0],
                "distance_world": 4.0,
                "complete": true,
            })
        };
        let case = |id: &str,
                    sampling: &str,
                    mode: &str,
                    pixel: [u32; 2],
                    depth: Option<f64>,
                    pick: Value,
                    authored: Value,
                    source: Value| {
            let mut case = json!({
                "id": id,
                "sampling": sampling,
                "mode": mode,
                "pixel": pixel,
                "expected_rgba8": [1, 2, 3, 255],
                "expected_premultiplied_rgba": [0.1, 0.2, 0.3, 1.0],
                "covered": true,
                "valid": true,
                "hit_depth_world": depth,
                "pick": pick,
                "authored_order": authored,
                "source_order": source,
            });
            case["input_fact_sha256"] = Value::String("11".repeat(32));
            case["expected_fact_sha256"] = Value::String(expected_case_sha256(&case).unwrap());
            case
        };
        let cases = vec![
            case(
                "plane_smooth_valid",
                "smooth_linear",
                "plane",
                [31, 31],
                None,
                Value::Null,
                json!([0]),
                json!([0]),
            ),
            case(
                "plane_smooth_invalid",
                "smooth_linear",
                "plane",
                [1, 63],
                None,
                Value::Null,
                json!([0]),
                json!([0]),
            ),
            case(
                "perspective_mip",
                "voxel_exact",
                "mip",
                [48, 32],
                None,
                pick(),
                json!([0]),
                json!([0]),
            ),
            case(
                "perspective_iso",
                "voxel_exact",
                "iso",
                [48, 32],
                Some(4.0),
                pick(),
                json!([0]),
                json!([0]),
            ),
            case(
                "perspective_iso_depth_order",
                "voxel_exact",
                "iso",
                [24, 16],
                Some(4.0),
                pick(),
                json!([5, 7]),
                json!([1, 0]),
            ),
            case(
                "perspective_dvr_world_distance",
                "voxel_exact",
                "dvr",
                [48, 32],
                None,
                pick(),
                json!([0]),
                json!([0]),
            ),
        ];
        let mut primary_fields = serde_json::Map::new();
        for part in [
            json!({
                "schema": "mirante4d-ep00-numerical-gpu-conformance",
                "result": "passed",
                "scalar_absolute_tolerance": 1.0e-6,
                "scalar_relative_tolerance": 4.76837158203125e-7,
                "premultiplied_rgba_absolute_tolerance": 2.0e-6,
                "world_position_absolute_tolerance": 1.0e-5,
                "ray_distance_absolute_tolerance": 1.0e-5,
                "rgba8_channel_tolerance": 1,
                "plane_smooth_valid_pixel": [31, 31],
                "plane_smooth_valid_rgba8": [1, 2, 3, 255],
                "plane_smooth_valid_premultiplied_rgba": [0.1, 0.2, 0.3, 1.0],
                "plane_smooth_valid_covered": true,
                "plane_smooth_valid_valid": true,
                "plane_smooth_invalid_pixel": [1, 63],
                "plane_smooth_invalid_rgba8": [1, 2, 3, 255],
                "plane_smooth_invalid_premultiplied_rgba": [0.1, 0.2, 0.3, 1.0],
                "plane_smooth_invalid_covered": true,
                "plane_smooth_invalid_valid": true,
            }),
            json!({
                "perspective_mip_pixel": [48, 32],
                "perspective_mip_rgba8": [1, 2, 3, 255],
                "perspective_mip_premultiplied_rgba": [0.1, 0.2, 0.3, 1.0],
                "perspective_mip_covered": true,
                "perspective_mip_valid": true,
                "perspective_mip_pick_kind": "voxel",
                "perspective_mip_pick_complete": true,
                "perspective_mip_pick_value": 0.75,
                "perspective_mip_pick_world": [1.0, 2.0, 3.0],
                "perspective_mip_pick_distance_world": 4.0,
                "perspective_iso_pixel": [48, 32],
                "perspective_iso_rgba8": [1, 2, 3, 255],
                "perspective_iso_premultiplied_rgba": [0.1, 0.2, 0.3, 1.0],
                "perspective_iso_covered": true,
                "perspective_iso_valid": true,
                "perspective_iso_hit_depth_world": 4.0,
                "perspective_iso_pick_kind": "voxel",
                "perspective_iso_pick_complete": true,
                "perspective_iso_pick_value": 0.75,
                "perspective_iso_pick_world": [1.0, 2.0, 3.0],
                "perspective_iso_pick_distance_world": 4.0,
            }),
            json!({
                "perspective_iso_depth_order_pixel": [24, 16],
                "perspective_iso_depth_order_rgba8": [1, 2, 3, 255],
                "perspective_iso_depth_order_premultiplied_rgba": [0.1, 0.2, 0.3, 1.0],
                "perspective_iso_depth_order_covered": true,
                "perspective_iso_depth_order_valid": true,
                "perspective_iso_depth_order_hit_depth_world": 4.0,
                "perspective_iso_depth_order_authored_layers": [5, 7],
                "perspective_iso_depth_order_source_order": [1, 0],
                "perspective_iso_near_pick_kind": "voxel",
                "perspective_iso_near_pick_complete": true,
                "perspective_iso_near_pick_value": 0.75,
                "perspective_iso_near_pick_world": [1.0, 2.0, 3.0],
                "perspective_iso_near_pick_distance_world": 4.0,
            }),
        ] {
            primary_fields.extend(part.as_object().unwrap().clone());
        }
        let primary = Value::Object(primary_fields);
        let dvr = json!({
            "schema": "mirante4d-ep00-numerical-gpu-conformance-gap",
            "case": "perspective_dvr_world_distance",
            "pixel": [48, 32],
            "expected_rgba8": [1, 2, 3, 255],
            "expected_premultiplied_rgba": [0.1, 0.2, 0.3, 1.0],
            "expected_covered": true,
            "expected_valid": true,
            "expected_pick_kind": "voxel",
            "expected_pick_complete": true,
            "expected_pick_value": 0.75,
            "expected_pick_world": [1.0, 2.0, 3.0],
            "expected_pick_distance_world": 4.0,
            "coverage_matches": true,
            "validity_matches": true,
            "result": "passed",
        });
        let mut reasons = BTreeSet::new();
        let mut bound = BTreeSet::new();
        validate_primary_numerical_marker(&primary, &cases, &contract, &mut reasons, &mut bound);
        validate_dvr_marker(&dvr, &cases, &contract, &mut reasons, &mut bound);
        assert!(reasons.is_empty(), "{reasons:#?}");
        assert_eq!(bound.len(), 6);
    }

    #[test]
    fn sanitized_harness_receipt_contains_no_capture_path() {
        let receipt = HarnessReceipt {
            kind: HarnessKind::ProductionShaderAudit,
            process: ProcessReceipt {
                exit_code: Some(0),
                signal: None,
                timed_out: false,
                wall_time_ns: 1,
                spawn_failed: false,
            },
            stdout_sha256: Some("11".repeat(32)),
            stderr_sha256: Some("22".repeat(32)),
            marker: Some(json!({
                "schema": "mirante4d-ep00-production-shader-structural-audit-1"
            })),
            marker_receipt: ParsedMarkerReceipt {
                schema: Some("mirante4d-ep00-production-shader-structural-audit-1".to_owned()),
                result: None,
                canonical_sha256: Some("33".repeat(32)),
                parsed: true,
            },
            reasons: BTreeSet::new(),
        };
        let encoded = serde_json::to_string(&harness_sanitized_json(&receipt)).unwrap();
        assert!(!encoded.contains("stdout.log"));
        assert!(!encoded.contains("stderr.log"));
        assert!(!encoded.contains("/private"));
        assert!(encoded.contains("exact_command"));
    }

    #[test]
    fn raw_conformance_projection_matches_the_in_memory_schema_5_receipt() {
        let evidence = valid_test_evidence();
        let authority = test_replay_authority(&evidence);
        let raw = evidence.raw_json();
        assert_eq!(
            sanitized_json_from_raw(&raw, true, &authority).unwrap(),
            evidence.sanitized_json()
        );

        let mut tampered_process = raw.clone();
        tampered_process["harnesses"][0]["process"]["timed_out"] = json!(true);
        assert!(sanitized_json_from_raw(&tampered_process, true, &authority).is_err());
        let mut tampered_source = raw.clone();
        tampered_source["source_commitments"][0]["sha256"] = json!("77".repeat(32));
        assert!(sanitized_json_from_raw(&tampered_source, true, &authority).is_err());
        let mut tampered_case = raw.clone();
        tampered_case["oracle_binding"]["case_commitments"][0]["expected_fact_sha256"] =
            json!("77".repeat(32));
        assert!(sanitized_json_from_raw(&tampered_case, true, &authority).is_err());
        let mut tampered_marker = raw;
        tampered_marker["harnesses"][0]["marker_receipt"]["canonical_sha256"] =
            json!("77".repeat(32));
        assert!(sanitized_json_from_raw(&tampered_marker, true, &authority).is_err());

        let mut wrong_kind_result = valid_test_evidence().raw_json();
        let marker = json!({
            "schema": HarnessKind::TimestampPlacementControls.expected_schema(),
            "result": "passed",
        });
        wrong_kind_result["harnesses"][3]["parsed_marker"] = marker.clone();
        wrong_kind_result["harnesses"][3]["marker_receipt"] =
            serde_json::to_value(marker_receipt(Some(&marker))).unwrap();
        assert!(sanitized_json_from_raw(&wrong_kind_result, true, &authority).is_err());

        let mut explicit_dvr_gap = valid_test_evidence().raw_json();
        let marker = json!({
            "schema": HarnessKind::NumericalPerspectiveDvr.expected_schema(),
            "result": "failed",
        });
        explicit_dvr_gap["harnesses"][1]["parsed_marker"] = marker.clone();
        explicit_dvr_gap["harnesses"][1]["marker_receipt"] =
            serde_json::to_value(marker_receipt(Some(&marker))).unwrap();
        explicit_dvr_gap["harnesses"][1]["process"]["exit_code"] = json!(101);
        explicit_dvr_gap["harnesses"][1]["reason_codes"] =
            json!(["conformance_dvr_frozen_world_distance_oracle_failed"]);
        explicit_dvr_gap["harnesses"][1]["status"] = json!("failed");
        explicit_dvr_gap["reason_codes"] =
            json!(["conformance_dvr_frozen_world_distance_oracle_failed"]);
        explicit_dvr_gap["status"] = json!("failed");
        assert!(sanitized_json_from_raw(&explicit_dvr_gap, true, &authority).is_ok());
        explicit_dvr_gap["harnesses"][1]["reason_codes"] = json!([]);
        explicit_dvr_gap["harnesses"][1]["status"] = json!("passed");
        explicit_dvr_gap["reason_codes"] = json!([]);
        explicit_dvr_gap["status"] = json!("passed");
        assert!(sanitized_json_from_raw(&explicit_dvr_gap, true, &authority).is_err());
    }
}

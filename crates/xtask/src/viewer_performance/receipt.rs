use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::private_evidence::{read_finalized_private_file, write_new_synced_private_file};

use super::conformance_receipt;

pub(super) const RAW_REPORT_SCHEMA: &str = "mirante4d-viewer-performance-raw-private-report-5";
pub(super) const RECEIPT_SCHEMA: &str = "mirante4d-viewer-performance-development-receipt-5";
pub(super) const RAW_REPORT_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub(super) const RAW_REPORT_FILE_NAME: &str = "raw-private-report.json";
pub(super) const RECEIPT_FILE_NAME: &str = "development-receipt.json";
const PRODUCT_GATE_OBSERVATION_SCHEMA: &str = "mirante4d-product-gate-batch-observation-1";
const PRODUCT_GATE_ID_MAX_BYTES: usize = 128;
const PRODUCT_GATE_CONDITION_MAX_BYTES: usize = 128;
const PRODUCT_GATE_DEADLINE_MAX_NS: u64 = 7_200_000_000_000;

#[derive(Clone, Debug)]
pub(super) struct ReceiptBindings {
    pub(super) qualification_profile_sha256: String,
    pub(super) owner_accepted_profile_contract_sha256: String,
    pub(super) ep01_selection_authority_sha256: String,
    pub(super) workload_bundle_sha256: String,
    pub(super) ep01_trace_geometry_sha256: String,
    pub(super) interaction_script_bundle_sha256: String,
    pub(super) independent_oracle_sha256: String,
    pub(super) representative_package_fingerprint_sha256: String,
    pub(super) supporting_temporal_package_fingerprint_sha256: String,
    pub(super) build_binding_fingerprint_sha256: String,
    pub(super) development_samples: u64,
    pub(super) expected_sample_records: u64,
    pub(super) expected_role_attempts: u64,
    pub(super) expected_phase_evaluations: u64,
    pub(super) expected_product_gate_observations: u64,
    pub(super) maximum_overhead_basis_points: u64,
    pub(super) expected_attempts: Vec<ExpectedAttemptAuthority>,
    pub(super) conformance_commitments: conformance_receipt::ReplayCommitmentAuthority,
    pub(super) private_strings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ExpectedAttemptAuthority {
    pub(super) sample_index: u64,
    pub(super) scenario: String,
    pub(super) phases: Vec<String>,
    pub(super) instrumented: ExpectedRoleAuthority,
    pub(super) instrumentation_control: ExpectedRoleAuthority,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ExpectedRoleAuthority {
    pub(super) role: String,
    pub(super) gate_batch_count: u64,
    pub(super) gate_observation_count: u64,
    pub(super) static_wait_bound_ns: u64,
    pub(super) derived_process_timeout_ns: u64,
    pub(super) product_gate_outcomes: Vec<ExpectedGateAuthority>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ExpectedGateAuthority {
    pub(super) command_index: u64,
    pub(super) batch_id: String,
    pub(super) phase_id: String,
    pub(super) observation_index: u64,
    pub(super) gate_id: String,
    pub(super) condition: String,
    pub(super) deadline_authority: String,
    pub(super) deadline_after_origin_ns: u64,
    pub(super) origin_kind: String,
    pub(super) origin_command_index: Option<u64>,
}

pub(super) fn publish_finalized_raw_report(
    raw_report_path: &Path,
    repository_root: &Path,
    bindings: &ReceiptBindings,
) -> anyhow::Result<(PathBuf, Value)> {
    let finalized = read_finalized_private_file(
        raw_report_path,
        repository_root,
        RAW_REPORT_MAX_BYTES,
        "viewer raw private report",
    )?;
    if finalized
        .canonical_path
        .file_name()
        .and_then(|name| name.to_str())
        != Some(RAW_REPORT_FILE_NAME)
    {
        bail!("viewer raw private report has the wrong fixed filename");
    }
    let raw: Value = serde_json::from_slice(&finalized.bytes)
        .context("viewer raw private report is malformed")?;
    require_canonical_json_bytes(&raw, &finalized.bytes, "viewer raw private report")?;
    let receipt = project_development_receipt(&raw, &finalized.sha256, bindings)?;
    validate_receipt_privacy(&receipt, &raw, bindings)?;
    let bytes = canonical_json_bytes(&receipt)?;
    let receipt_path = finalized
        .canonical_path
        .parent()
        .context("viewer raw private report has no parent")?
        .join(RECEIPT_FILE_NAME);
    write_new_synced_private_file(
        &receipt_path,
        &bytes,
        RAW_REPORT_MAX_BYTES,
        "viewer development receipt",
    )?;
    let reread = read_finalized_private_file(
        &receipt_path,
        repository_root,
        RAW_REPORT_MAX_BYTES,
        "viewer development receipt",
    )?;
    if reread.bytes != bytes {
        bail!("viewer development receipt changed while finalized");
    }
    Ok((receipt_path, receipt))
}

pub(super) fn admit_finalized_receipt(
    raw_report_path: &Path,
    receipt_path: &Path,
    repository_root: &Path,
    bindings: &ReceiptBindings,
) -> anyhow::Result<Value> {
    let raw = read_finalized_private_file(
        raw_report_path,
        repository_root,
        RAW_REPORT_MAX_BYTES,
        "viewer raw private report",
    )?;
    let receipt = read_finalized_private_file(
        receipt_path,
        repository_root,
        RAW_REPORT_MAX_BYTES,
        "viewer development receipt",
    )?;
    let expected_receipt_path = raw
        .canonical_path
        .parent()
        .context("viewer raw private report has no parent")?
        .join(RECEIPT_FILE_NAME);
    if raw
        .canonical_path
        .file_name()
        .and_then(|name| name.to_str())
        != Some(RAW_REPORT_FILE_NAME)
        || receipt.canonical_path != expected_receipt_path
    {
        bail!("viewer EP-00 admission requires the fixed raw/receipt sibling names");
    }
    let raw_value: Value =
        serde_json::from_slice(&raw.bytes).context("viewer raw private report is malformed")?;
    require_canonical_json_bytes(&raw_value, &raw.bytes, "viewer raw private report")?;
    let expected = project_development_receipt(&raw_value, &raw.sha256, bindings)?;
    validate_receipt_privacy(&expected, &raw_value, bindings)?;
    let expected_bytes = canonical_json_bytes(&expected)?;
    if receipt.bytes != expected_bytes {
        bail!("viewer development receipt is not the exact projection of its finalized raw report");
    }
    if expected.get("evidence_status").and_then(Value::as_str) != Some("valid_complete")
        || expected
            .pointer("/population/exact")
            .and_then(Value::as_bool)
            != Some(true)
        || !expected
            .get("integrity_reason_codes")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        || !matches!(
            expected.get("product_gate_status").and_then(Value::as_str),
            Some("passed" | "failed")
        )
    {
        bail!("viewer EP-00 evidence is not authoritative and complete");
    }
    Ok(json!({
        "schema": "mirante4d-viewer-performance-ep01-preflight-report-1",
        "ep00_evidence_admitted": true,
        "evidence_status": expected["evidence_status"],
        "product_gate_status": expected["product_gate_status"],
        "population": expected["population"],
        "private_raw_report_sha256": raw.sha256,
    }))
}

pub(super) fn project_development_receipt(
    raw: &Value,
    raw_sha256: &str,
    bindings: &ReceiptBindings,
) -> anyhow::Result<Value> {
    require_sha256(raw_sha256, "viewer raw report digest")?;
    require_object_keys(
        raw,
        &[
            "schema",
            "evidence_status",
            "product_gate_status",
            "claim_status",
            "build_binding",
            "protocol",
            "private_paths",
            "commitments",
            "bindings",
            "executable_conformance",
            "population",
            "instrumentation_overhead_populations",
            "product_gate_outcomes",
            "product_gate_failures",
            "attempts",
            "integrity_reason_codes",
            "limitations",
        ],
        "viewer raw private report",
    )?;
    if raw.get("schema").and_then(Value::as_str) != Some(RAW_REPORT_SCHEMA)
        || raw.get("claim_status").and_then(Value::as_str)
            != Some("development_E1_semantic_automation_non_OS_input_non_E4_no_product_claim")
    {
        bail!("viewer raw private report schema or claim boundary changed");
    }
    let commitments = raw
        .get("commitments")
        .context("viewer raw private report lacks commitments")?;
    require_object_keys(
        commitments,
        &[
            "qualification_profile_sha256",
            "owner_accepted_profile_contract_sha256",
            "ep01_selection_authority_sha256",
            "workload_bundle_sha256",
            "ep01_trace_geometry_sha256",
            "interaction_script_bundle_sha256",
            "independent_oracle_sha256",
            "app_binary_sha256_before_run",
            "app_binary_sha256_after_run",
            "app_binary_unchanged",
            "representative_package_fingerprint_sha256",
            "supporting_temporal_package_fingerprint_sha256",
            "build_binding_fingerprint_sha256",
        ],
        "viewer raw commitments",
    )?;
    for (field, expected) in [
        (
            "qualification_profile_sha256",
            bindings.qualification_profile_sha256.as_str(),
        ),
        (
            "owner_accepted_profile_contract_sha256",
            bindings.owner_accepted_profile_contract_sha256.as_str(),
        ),
        (
            "ep01_selection_authority_sha256",
            bindings.ep01_selection_authority_sha256.as_str(),
        ),
        (
            "workload_bundle_sha256",
            bindings.workload_bundle_sha256.as_str(),
        ),
        (
            "ep01_trace_geometry_sha256",
            bindings.ep01_trace_geometry_sha256.as_str(),
        ),
        (
            "interaction_script_bundle_sha256",
            bindings.interaction_script_bundle_sha256.as_str(),
        ),
        (
            "independent_oracle_sha256",
            bindings.independent_oracle_sha256.as_str(),
        ),
        (
            "representative_package_fingerprint_sha256",
            bindings.representative_package_fingerprint_sha256.as_str(),
        ),
        (
            "supporting_temporal_package_fingerprint_sha256",
            bindings
                .supporting_temporal_package_fingerprint_sha256
                .as_str(),
        ),
        (
            "build_binding_fingerprint_sha256",
            bindings.build_binding_fingerprint_sha256.as_str(),
        ),
    ] {
        if commitments.get(field).and_then(Value::as_str) != Some(expected) {
            bail!("viewer raw commitment {field:?} differs from its external authority");
        }
    }
    let app_before = commitments
        .get("app_binary_sha256_before_run")
        .and_then(Value::as_str)
        .context("viewer raw report lacks its app digest")?;
    require_sha256(app_before, "viewer app digest")?;
    if commitments
        .get("app_binary_sha256_after_run")
        .and_then(Value::as_str)
        != Some(app_before)
        || commitments
            .get("app_binary_unchanged")
            .and_then(Value::as_bool)
            != Some(true)
    {
        bail!("viewer app binary changed during the measurement");
    }

    let population = raw
        .get("population")
        .cloned()
        .context("viewer raw report lacks population evidence")?;
    require_population_shape(&population, bindings)?;
    let integrity = raw
        .get("integrity_reason_codes")
        .and_then(Value::as_array)
        .context("viewer raw report lacks integrity reason codes")?;
    if integrity.iter().any(|reason| reason.as_str().is_none()) {
        bail!("viewer raw integrity reason codes are malformed");
    }
    for reason in integrity.iter().filter_map(Value::as_str) {
        require_path_free_identifier(reason, PRODUCT_GATE_ID_MAX_BYTES, "viewer integrity reason")?;
    }
    let evidence_valid =
        integrity.is_empty() && population.get("exact").and_then(Value::as_bool) == Some(true);
    let expected_evidence_status = if evidence_valid {
        "valid_complete"
    } else {
        "invalid_or_incomplete"
    };
    if raw.get("evidence_status").and_then(Value::as_str) != Some(expected_evidence_status) {
        bail!("viewer raw evidence status is inconsistent");
    }

    let overhead = raw
        .get("instrumentation_overhead_populations")
        .cloned()
        .context("viewer raw report lacks overhead populations")?;
    validate_overhead_rows(&overhead, raw, bindings, evidence_valid)?;
    let product_gate_outcomes = sanitized_product_gate_outcomes(raw, bindings)?;
    if evidence_valid
        && u64::try_from(product_gate_outcomes.len())?
            != bindings.expected_product_gate_observations
    {
        bail!("viewer raw product-gate population differs from its external authority");
    }
    let product_gate_failures = product_gate_failure_rows(raw, bindings)?;
    let conformance = match raw.get("executable_conformance") {
        Some(Value::Null) if !evidence_valid => Value::Null,
        Some(Value::Null) => {
            bail!("complete viewer evidence lacks executable conformance evidence")
        }
        Some(value) => conformance_receipt::sanitized_json_from_raw(
            value,
            evidence_valid,
            &bindings.conformance_commitments,
        )?,
        None => bail!("viewer raw report lacks executable conformance evidence"),
    };
    let raw_product_status = raw
        .get("product_gate_status")
        .and_then(Value::as_str)
        .context("viewer raw report lacks its product status")?;
    let failed_product_gate = product_gate_outcomes
        .iter()
        .any(|row| row.get("outcome").and_then(Value::as_str) == Some("failed"))
        || product_gate_failures
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
        || conformance.get("status").and_then(Value::as_str) == Some("failed");
    let expected_product_status = if !evidence_valid {
        "not_authoritative"
    } else if failed_product_gate {
        "failed"
    } else {
        "passed"
    };
    if raw_product_status != expected_product_status {
        bail!("viewer raw product status is inconsistent with its evidence");
    }
    let role_schedule_bounds = role_schedule_rows(raw, bindings, evidence_valid)?;

    Ok(json!({
        "schema": RECEIPT_SCHEMA,
        "evidence_status": expected_evidence_status,
        "product_gate_status": raw_product_status,
        "claim_status": "development_E1_non_OS_input_non_E4_no_product_claim",
        "commitments": {
            "qualification_profile_sha256": bindings.qualification_profile_sha256,
            "owner_accepted_profile_contract_sha256": bindings.owner_accepted_profile_contract_sha256,
            "ep01_selection_authority_sha256": bindings.ep01_selection_authority_sha256,
            "workload_bundle_sha256": bindings.workload_bundle_sha256,
            "ep01_trace_geometry_sha256": bindings.ep01_trace_geometry_sha256,
            "interaction_script_bundle_sha256": bindings.interaction_script_bundle_sha256,
            "independent_oracle_sha256": bindings.independent_oracle_sha256,
            "app_binary_sha256": app_before,
            "private_raw_report_sha256": raw_sha256,
            "representative_package_fingerprint_sha256": bindings.representative_package_fingerprint_sha256,
            "supporting_temporal_package_fingerprint_sha256": bindings.supporting_temporal_package_fingerprint_sha256,
            "build_binding_fingerprint_sha256": bindings.build_binding_fingerprint_sha256,
        },
        "executable_conformance": conformance,
        "population": population,
        "instrumentation_overhead_populations": overhead,
        "product_gate_outcomes": product_gate_outcomes,
        "role_schedule_bounds": role_schedule_bounds,
        "product_gate_failures": product_gate_failures,
        "integrity_reason_codes": integrity,
    }))
}

fn sanitized_product_gate_outcomes(
    raw: &Value,
    bindings: &ReceiptBindings,
) -> anyhow::Result<Vec<Value>> {
    let rows = raw
        .get("product_gate_outcomes")
        .and_then(Value::as_array)
        .context("viewer raw report lacks product-gate outcomes")?;
    let sanitized = rows
        .iter()
        .map(|row| {
            require_object_keys(
                row,
                &[
                    "schema",
                    "command_index",
                    "batch_id",
                    "phase_id",
                    "observation_index",
                    "gate_id",
                    "condition",
                    "deadline_authority",
                    "deadline_after_origin_ns",
                    "origin",
                    "outcome",
                    "condition_met",
                    "timed_out",
                    "observed_after_origin_ns",
                    "sample_index",
                    "scenario",
                    "role",
                ],
                "viewer raw product-gate outcome",
            )?;
            for field in [
                "sample_index",
                "observation_index",
                "deadline_after_origin_ns",
                "command_index",
                "observed_after_origin_ns",
            ] {
                if row.get(field).and_then(Value::as_u64).is_none() {
                    bail!("viewer raw product-gate outcome field {field:?} is malformed");
                }
            }
            for field in [
                "scenario",
                "role",
                "batch_id",
                "phase_id",
                "gate_id",
                "condition",
                "deadline_authority",
                "outcome",
            ] {
                if row.get(field).and_then(Value::as_str).is_none() {
                    bail!("viewer raw product-gate outcome field {field:?} is malformed");
                }
            }
            if !matches!(
                row.get("outcome").and_then(Value::as_str),
                Some("passed" | "failed")
            ) {
                bail!("viewer raw product-gate outcome has an unknown status");
            }
            validate_public_gate_scope(row, bindings)?;
            if row.get("schema").and_then(Value::as_str) != Some(PRODUCT_GATE_OBSERVATION_SCHEMA) {
                bail!("viewer raw product-gate outcome schema changed");
            }
            for field in ["batch_id", "phase_id", "gate_id"] {
                require_path_free_identifier(
                    row.get(field)
                        .and_then(Value::as_str)
                        .expect("product-gate string fields were validated"),
                    PRODUCT_GATE_ID_MAX_BYTES,
                    "viewer product-gate identifier",
                )?;
            }
            let condition = row
                .get("condition")
                .and_then(Value::as_str)
                .expect("product-gate string fields were validated");
            if condition.is_empty()
                || condition.len() > PRODUCT_GATE_CONDITION_MAX_BYTES
                || !condition
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            {
                bail!("viewer raw product-gate condition is not path-free snake case");
            }
            let deadline = row
                .get("deadline_after_origin_ns")
                .and_then(Value::as_u64)
                .expect("product-gate number fields were validated");
            let observed = row
                .get("observed_after_origin_ns")
                .and_then(Value::as_u64)
                .expect("product-gate number fields were validated");
            let condition_met = row
                .get("condition_met")
                .and_then(Value::as_bool)
                .context("viewer raw product-gate condition result is malformed")?;
            let timed_out = row
                .get("timed_out")
                .and_then(Value::as_bool)
                .context("viewer raw product-gate timeout result is malformed")?;
            let ordinary_outcome = matches!(
                row.get("outcome").and_then(Value::as_str),
                Some("passed") if condition_met && !timed_out && observed < deadline
            ) || matches!(
                row.get("outcome").and_then(Value::as_str),
                Some("failed") if timed_out && observed >= deadline
            );
            if deadline == 0
                || deadline > PRODUCT_GATE_DEADLINE_MAX_NS
                || !matches!(
                    row.get("deadline_authority").and_then(Value::as_str),
                    Some(
                        "maximum_current_presentation_gap_plus_poll_grace"
                            | "cold_first_useful"
                            | "cold_complete_coarse"
                            | "cold_target_settlement"
                            | "nonresident_target_settlement"
                            | "source_verification_completion"
                            | "import_primary_wall"
                    )
                )
                || (!ordinary_outcome && !is_ip_terminal_prepublication_early_failure(row))
            {
                bail!("viewer raw product-gate outcome facts are incoherent");
            }
            let origin = row
                .get("origin")
                .context("viewer raw product-gate outcome lacks its origin")?;
            require_object_keys(
                origin,
                &["kind", "command_index"],
                "viewer raw product-gate origin",
            )?;
            match origin.get("kind").and_then(Value::as_str) {
                Some("command_completed")
                    if origin
                        .get("command_index")
                        .and_then(Value::as_u64)
                        .is_some() => {}
                Some("automation_started" | "import_primary_started")
                    if origin.get("command_index") == Some(&Value::Null) => {}
                _ => bail!("viewer raw product-gate origin is malformed"),
            }
            Ok(json!({
                "sample_index": row["sample_index"],
                "scenario": row["scenario"],
                "role": row["role"],
                "batch_id": row["batch_id"],
                "phase_id": row["phase_id"],
                "observation_index": row["observation_index"],
                "gate_id": row["gate_id"],
                "condition": row["condition"],
                "deadline_authority": row["deadline_authority"],
                "deadline_after_origin_ns": row["deadline_after_origin_ns"],
                "outcome": row["outcome"],
            }))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    validate_ip_terminal_prepublication_early_failure_pairs(rows)?;
    Ok(sanitized)
}

fn is_ip_terminal_prepublication_early_failure(row: &Value) -> bool {
    row.get("scenario").and_then(Value::as_str) == Some("IP")
        && row.get("batch_id").and_then(Value::as_str) == Some("IP.batch.000")
        && row.get("phase_id").and_then(Value::as_str) == Some("preprocess_publish.checkpoint.000")
        && matches!(
            row.get("condition").and_then(Value::as_str),
            Some("import_idle" | "imported_open_ready")
        )
        && row.get("deadline_authority").and_then(Value::as_str) == Some("import_primary_wall")
        && row.pointer("/origin/kind").and_then(Value::as_str) == Some("import_primary_started")
        && row.pointer("/origin/command_index") == Some(&Value::Null)
        && row.get("outcome").and_then(Value::as_str) == Some("failed")
        && row.get("condition_met").and_then(Value::as_bool) == Some(false)
        && row.get("timed_out").and_then(Value::as_bool) == Some(false)
        && matches!(
            (
                row.get("observed_after_origin_ns").and_then(Value::as_u64),
                row.get("deadline_after_origin_ns").and_then(Value::as_u64),
            ),
            (Some(observed), Some(deadline)) if observed < deadline
        )
}

fn validate_ip_terminal_prepublication_early_failure_pairs(rows: &[Value]) -> anyhow::Result<()> {
    let mut groups = BTreeMap::<(u64, &str), Vec<&Value>>::new();
    for row in rows.iter().filter(|row| {
        row.get("outcome").and_then(Value::as_str) == Some("failed")
            && row.get("timed_out").and_then(Value::as_bool) == Some(false)
    }) {
        if !is_ip_terminal_prepublication_early_failure(row) {
            bail!("viewer raw report contains an unauthorized early product-gate failure");
        }
        let key = (
            row.get("sample_index")
                .and_then(Value::as_u64)
                .expect("product-gate sample indices were validated"),
            row.get("role")
                .and_then(Value::as_str)
                .expect("product-gate roles were validated"),
        );
        groups.entry(key).or_default().push(row);
    }
    for ((sample_index, role), failures) in groups {
        let conditions = failures
            .iter()
            .filter_map(|row| row.get("condition").and_then(Value::as_str))
            .collect::<BTreeSet<_>>();
        if failures.len() != 2
            || conditions != BTreeSet::from(["import_idle", "imported_open_ready"])
        {
            bail!("viewer IP terminal-prepublication failure is not one exact paired outcome");
        }
        let batch_rows = rows
            .iter()
            .filter(|row| {
                row.get("sample_index").and_then(Value::as_u64) == Some(sample_index)
                    && row.get("role").and_then(Value::as_str) == Some(role)
                    && row.get("scenario").and_then(Value::as_str) == Some("IP")
                    && row.get("batch_id").and_then(Value::as_str) == Some("IP.batch.000")
                    && row.get("phase_id").and_then(Value::as_str)
                        == Some("preprocess_publish.checkpoint.000")
                    && row.get("deadline_authority").and_then(Value::as_str)
                        == Some("import_primary_wall")
                    && row.pointer("/origin/kind").and_then(Value::as_str)
                        == Some("import_primary_started")
                    && row.pointer("/origin/command_index") == Some(&Value::Null)
            })
            .collect::<Vec<_>>();
        let runtime_idle = batch_rows
            .iter()
            .filter(|row| row.get("condition").and_then(Value::as_str) == Some("runtime_idle"))
            .collect::<Vec<_>>();
        let shared_deadline = failures[0]
            .get("deadline_after_origin_ns")
            .and_then(Value::as_u64)
            .expect("product-gate deadlines were validated");
        if batch_rows.len() != 3
            || runtime_idle.len() != 1
            || failures.iter().any(|row| {
                row.get("deadline_after_origin_ns").and_then(Value::as_u64) != Some(shared_deadline)
            })
            || runtime_idle[0]
                .get("deadline_after_origin_ns")
                .and_then(Value::as_u64)
                != Some(shared_deadline)
            || runtime_idle[0].get("outcome").and_then(Value::as_str) != Some("passed")
            || runtime_idle[0]
                .get("condition_met")
                .and_then(Value::as_bool)
                != Some(true)
            || runtime_idle[0].get("timed_out").and_then(Value::as_bool) != Some(false)
            || !matches!(
                runtime_idle[0]
                    .get("observed_after_origin_ns")
                    .and_then(Value::as_u64),
                Some(observed) if observed < shared_deadline
            )
        {
            bail!("viewer IP terminal-prepublication failure lacks its exact runtime-idle sibling");
        }
    }
    Ok(())
}

fn product_gate_failure_rows(raw: &Value, bindings: &ReceiptBindings) -> anyhow::Result<Value> {
    let rows = raw
        .get("product_gate_failures")
        .and_then(Value::as_array)
        .context("viewer raw report lacks product-gate failures")?;
    for row in rows {
        require_object_keys(
            row,
            &[
                "sample_index",
                "scenario",
                "role",
                "gate_id",
                "condition",
                "outcome",
            ],
            "viewer raw product-gate failure",
        )?;
        if row.get("sample_index").and_then(Value::as_u64).is_none()
            || row.get("outcome").and_then(Value::as_str) != Some("failed")
            || ["scenario", "role", "gate_id", "condition"]
                .into_iter()
                .any(|field| row.get(field).and_then(Value::as_str).is_none())
        {
            bail!("viewer raw product-gate failure is malformed");
        }
        validate_public_gate_scope(row, bindings)?;
        require_path_free_identifier(
            row.get("gate_id")
                .and_then(Value::as_str)
                .expect("product-gate failure strings were validated"),
            PRODUCT_GATE_ID_MAX_BYTES,
            "viewer product-gate failure identifier",
        )?;
        let condition = row
            .get("condition")
            .and_then(Value::as_str)
            .expect("product-gate failure strings were validated");
        if condition.is_empty()
            || condition.len() > PRODUCT_GATE_CONDITION_MAX_BYTES
            || !condition
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            bail!("viewer product-gate failure condition is malformed");
        }
    }
    Ok(Value::Array(rows.clone()))
}

fn validate_public_gate_scope(row: &Value, bindings: &ReceiptBindings) -> anyhow::Result<()> {
    let sample_index = row
        .get("sample_index")
        .and_then(Value::as_u64)
        .context("viewer product-gate row lacks a sample index")?;
    if !(1..=bindings.development_samples).contains(&sample_index)
        || !super::REQUIRED_SCENARIOS
            .contains(&row.get("scenario").and_then(Value::as_str).unwrap_or(""))
        || !matches!(
            row.get("role").and_then(Value::as_str),
            Some("instrumented" | "instrumentation-control")
        )
    {
        bail!("viewer product-gate row has an out-of-authority sample, scenario, or role");
    }
    Ok(())
}

fn require_path_free_identifier(
    value: &str,
    maximum_bytes: usize,
    label: &str,
) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > maximum_bytes
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        bail!("{label} is malformed");
    }
    Ok(())
}

fn role_schedule_rows(
    raw: &Value,
    bindings: &ReceiptBindings,
    evidence_valid: bool,
) -> anyhow::Result<Vec<Value>> {
    let attempts = raw
        .get("attempts")
        .and_then(Value::as_array)
        .context("viewer raw report lacks attempts")?;
    if u64::try_from(bindings.expected_attempts.len())? != bindings.expected_sample_records {
        bail!("viewer external receipt authority has an inconsistent attempt population");
    }
    if evidence_valid {
        return exact_role_schedule_rows(raw, attempts, bindings);
    }
    if u64::try_from(attempts.len())? > bindings.expected_sample_records
        || (evidence_valid && u64::try_from(attempts.len())? != bindings.expected_sample_records)
    {
        bail!("viewer raw attempt population differs from its external authority");
    }
    let mut rows = Vec::with_capacity(attempts.len().saturating_mul(2));
    let mut observed_gate_outcomes = 0_u64;
    let scenario_count = super::REQUIRED_SCENARIOS.len();
    for (attempt_index, attempt) in attempts.iter().enumerate() {
        let sample_index = attempt
            .get("sample_index")
            .and_then(Value::as_u64)
            .context("viewer raw attempt lacks sample index")?;
        let scenario = attempt
            .get("scenario")
            .and_then(Value::as_str)
            .context("viewer raw attempt lacks scenario")?;
        if evidence_valid {
            let expected_sample_index = u64::try_from(attempt_index / scenario_count)? + 1;
            let expected_scenario = super::REQUIRED_SCENARIOS[attempt_index % scenario_count];
            if sample_index != expected_sample_index || scenario != expected_scenario {
                bail!("viewer raw attempt order or identity differs from its external authority");
            }
        }
        for (field, expected_role) in [
            ("instrumented", "instrumented"),
            ("instrumentation_control", "instrumentation-control"),
        ] {
            let Some(role_value) = attempt.get(field) else {
                bail!("viewer raw attempt lacks {field}");
            };
            if role_value.is_null() && !evidence_valid && field == "instrumentation_control" {
                continue;
            }
            let role = role_value
                .as_object()
                .with_context(|| format!("viewer raw attempt has malformed {field}"))?;
            let role_identity = role
                .get("role")
                .and_then(Value::as_str)
                .context("viewer raw role lacks identity")?;
            if evidence_valid && role_identity != expected_role {
                bail!("viewer raw role identity differs from its external authority");
            }
            let process = role
                .get("process")
                .and_then(Value::as_object)
                .with_context(|| format!("viewer raw attempt {field} lacks process evidence"))?;
            let launch_attempted = process
                .get("launch_attempted")
                .and_then(Value::as_bool)
                .context("viewer raw role lacks launch fact")?;
            let gate_observation_count = process
                .get("gate_observation_count")
                .and_then(Value::as_u64)
                .context("viewer raw role lacks gate-observation count")?;
            let role_gate_outcomes = role
                .get("product_gate_outcomes")
                .and_then(Value::as_array)
                .context("viewer raw role lacks product-gate outcomes")?;
            if evidence_valid
                && (!launch_attempted
                    || u64::try_from(role_gate_outcomes.len())? != gate_observation_count)
            {
                bail!("viewer raw role is incomplete or has inconsistent gate counts");
            }
            observed_gate_outcomes = observed_gate_outcomes
                .checked_add(u64::try_from(role_gate_outcomes.len())?)
                .context("viewer raw role gate population overflowed")?;
            rows.push(json!({
                "sample_index": sample_index,
                "scenario": scenario,
                "role": role_identity,
                "launch_attempted": launch_attempted,
                "gate_batch_count": process.get("gate_batch_count").and_then(Value::as_u64).context("viewer raw role lacks gate-batch count")?,
                "gate_observation_count": gate_observation_count,
                "static_wait_bound_ns": process.get("static_wait_bound_ns").and_then(Value::as_u64).context("viewer raw role lacks static wait bound")?,
                "derived_process_timeout_ns": process.get("derived_process_timeout_ns").and_then(Value::as_u64).context("viewer raw role lacks process timeout")?,
            }));
        }
    }
    let global_gate_outcomes = u64::try_from(
        raw.get("product_gate_outcomes")
            .and_then(Value::as_array)
            .context("viewer raw report lacks product-gate outcomes")?
            .len(),
    )?;
    if evidence_valid
        && (u64::try_from(rows.len())? != bindings.expected_role_attempts
            || observed_gate_outcomes != bindings.expected_product_gate_observations
            || global_gate_outcomes != observed_gate_outcomes)
    {
        bail!("viewer raw role schedule does not reconcile with its complete population");
    }
    Ok(rows)
}

fn exact_role_schedule_rows(
    raw: &Value,
    attempts: &[Value],
    bindings: &ReceiptBindings,
) -> anyhow::Result<Vec<Value>> {
    if attempts.len() != bindings.expected_attempts.len() {
        bail!("viewer raw attempt population differs from its external authority");
    }
    let mut schedule_rows = Vec::with_capacity(bindings.expected_attempts.len() * 2);
    let mut flattened_gate_rows = Vec::with_capacity(
        usize::try_from(bindings.expected_product_gate_observations).unwrap_or(0),
    );
    let mut observed_phases = 0_u64;
    for (attempt, expected) in attempts.iter().zip(&bindings.expected_attempts) {
        require_object_keys(
            attempt,
            &[
                "sample_index",
                "scenario",
                "instrumented",
                "instrumentation_control",
                "paired_overhead",
                "phases",
                "integrity_reason_codes",
                "evidence_status",
                "product_gate_status",
            ],
            "viewer raw attempt",
        )?;
        if attempt.get("sample_index").and_then(Value::as_u64) != Some(expected.sample_index)
            || attempt.get("scenario").and_then(Value::as_str) != Some(expected.scenario.as_str())
            || attempt.get("evidence_status").and_then(Value::as_str) != Some("valid_complete")
            || !is_empty_string_array(attempt.get("integrity_reason_codes"))
            || !matches!(
                attempt.get("product_gate_status").and_then(Value::as_str),
                Some("passed" | "failed")
            )
        {
            bail!("viewer raw attempt identity or status differs from its complete authority");
        }
        let phases = attempt
            .get("phases")
            .and_then(Value::as_array)
            .context("viewer raw attempt phases are malformed")?;
        if phases.len() != expected.phases.len() {
            bail!("viewer raw phase population differs from its external authority");
        }
        for (phase, expected_name) in phases.iter().zip(&expected.phases) {
            require_object_keys(
                phase,
                &[
                    "name",
                    "integrity_reason_codes",
                    "evidence_status",
                    "product_gate_status",
                ],
                "viewer raw phase",
            )?;
            if phase.get("name").and_then(Value::as_str) != Some(expected_name.as_str())
                || phase.get("evidence_status").and_then(Value::as_str) != Some("valid_complete")
                || !is_empty_string_array(phase.get("integrity_reason_codes"))
                || !matches!(
                    phase.get("product_gate_status").and_then(Value::as_str),
                    Some("passed" | "failed")
                )
            {
                bail!("viewer raw phase identity, order, or status is inconsistent");
            }
        }
        observed_phases = observed_phases
            .checked_add(u64::try_from(phases.len())?)
            .context("viewer raw phase population overflowed")?;
        for (field, expected_role) in [
            ("instrumented", &expected.instrumented),
            ("instrumentation_control", &expected.instrumentation_control),
        ] {
            let role = attempt
                .get(field)
                .context("viewer raw attempt lacks one role")?;
            let (schedule, gates) = exact_role_evidence(
                role,
                expected.sample_index,
                &expected.scenario,
                expected_role,
            )?;
            schedule_rows.push(schedule);
            flattened_gate_rows.extend(gates);
        }
    }
    if observed_phases != bindings.expected_phase_evaluations
        || u64::try_from(schedule_rows.len())? != bindings.expected_role_attempts
        || u64::try_from(flattened_gate_rows.len())? != bindings.expected_product_gate_observations
        || raw.get("product_gate_outcomes").and_then(Value::as_array) != Some(&flattened_gate_rows)
    {
        bail!("viewer raw local/global populations are not one exact externally bound bijection");
    }
    Ok(schedule_rows)
}

fn exact_role_evidence(
    role: &Value,
    sample_index: u64,
    scenario: &str,
    expected: &ExpectedRoleAuthority,
) -> anyhow::Result<(Value, Vec<Value>)> {
    require_object_keys(
        role,
        &[
            "role",
            "root",
            "paths",
            "commitments",
            "process",
            "import_source_inventory",
            "cleanup",
            "product_gate_outcomes",
            "integrity_reason_codes",
            "evidence_status",
            "product_gate_status",
        ],
        "viewer raw role",
    )?;
    if role.get("role").and_then(Value::as_str) != Some(expected.role.as_str())
        || role.get("evidence_status").and_then(Value::as_str) != Some("valid_complete")
        || !is_empty_string_array(role.get("integrity_reason_codes"))
        || !matches!(
            role.get("product_gate_status").and_then(Value::as_str),
            Some("passed" | "failed")
        )
    {
        bail!("viewer raw role identity or status differs from its complete authority");
    }
    let process = role
        .get("process")
        .context("viewer raw role lacks process evidence")?;
    require_object_keys(
        process,
        &[
            "launch_attempted",
            "exit_code",
            "signal",
            "external_wall_time_ns",
            "timed_out",
            "spawn_error",
            "app_wall_time_ns",
            "process_cpu_time_ns",
            "derived_process_timeout_ns",
            "static_wait_bound_ns",
            "gate_batch_count",
            "gate_observation_count",
        ],
        "viewer raw role process",
    )?;
    let number = |field: &str| process.get(field).and_then(Value::as_u64);
    if process.get("launch_attempted").and_then(Value::as_bool) != Some(true)
        || number("exit_code") != Some(0)
        || process.get("signal") != Some(&Value::Null)
        || process.get("timed_out").and_then(Value::as_bool) != Some(false)
        || process.get("spawn_error") != Some(&Value::Null)
        || number("gate_batch_count") != Some(expected.gate_batch_count)
        || number("gate_observation_count") != Some(expected.gate_observation_count)
        || number("static_wait_bound_ns") != Some(expected.static_wait_bound_ns)
        || number("derived_process_timeout_ns") != Some(expected.derived_process_timeout_ns)
    {
        bail!("viewer raw role process differs from its complete schedule authority");
    }
    let outcomes = role
        .get("product_gate_outcomes")
        .and_then(Value::as_array)
        .context("viewer raw role product-gate outcomes are malformed")?;
    if outcomes.len() != expected.product_gate_outcomes.len() {
        bail!("viewer raw role product-gate population differs from its external authority");
    }
    let contextual = outcomes
        .iter()
        .zip(&expected.product_gate_outcomes)
        .map(|(row, authority)| {
            require_local_gate_authority(row, authority)?;
            let mut contextual = row.clone();
            let object = contextual
                .as_object_mut()
                .expect("local gate authority requires an object");
            object.insert("sample_index".to_owned(), json!(sample_index));
            object.insert("scenario".to_owned(), json!(scenario));
            object.insert("role".to_owned(), json!(expected.role));
            Ok(contextual)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok((
        json!({
            "sample_index": sample_index,
            "scenario": scenario,
            "role": expected.role,
            "launch_attempted": true,
            "gate_batch_count": expected.gate_batch_count,
            "gate_observation_count": expected.gate_observation_count,
            "static_wait_bound_ns": expected.static_wait_bound_ns,
            "derived_process_timeout_ns": expected.derived_process_timeout_ns,
        }),
        contextual,
    ))
}

fn require_local_gate_authority(
    row: &Value,
    expected: &ExpectedGateAuthority,
) -> anyhow::Result<()> {
    require_object_keys(
        row,
        &[
            "schema",
            "command_index",
            "batch_id",
            "phase_id",
            "observation_index",
            "gate_id",
            "condition",
            "deadline_authority",
            "deadline_after_origin_ns",
            "origin",
            "outcome",
            "condition_met",
            "timed_out",
            "observed_after_origin_ns",
        ],
        "viewer raw local product-gate outcome",
    )?;
    if row.get("schema").and_then(Value::as_str) != Some(PRODUCT_GATE_OBSERVATION_SCHEMA)
        || row.get("command_index").and_then(Value::as_u64) != Some(expected.command_index)
        || row.get("batch_id").and_then(Value::as_str) != Some(expected.batch_id.as_str())
        || row.get("phase_id").and_then(Value::as_str) != Some(expected.phase_id.as_str())
        || row.get("observation_index").and_then(Value::as_u64) != Some(expected.observation_index)
        || row.get("gate_id").and_then(Value::as_str) != Some(expected.gate_id.as_str())
        || row.get("condition").and_then(Value::as_str) != Some(expected.condition.as_str())
        || row.get("deadline_authority").and_then(Value::as_str)
            != Some(expected.deadline_authority.as_str())
        || row.get("deadline_after_origin_ns").and_then(Value::as_u64)
            != Some(expected.deadline_after_origin_ns)
        || row.pointer("/origin/kind").and_then(Value::as_str)
            != Some(expected.origin_kind.as_str())
        || row.pointer("/origin/command_index").and_then(Value::as_u64)
            != expected.origin_command_index
        || (expected.origin_command_index.is_none()
            && row.pointer("/origin/command_index") != Some(&Value::Null))
    {
        bail!("viewer raw local product-gate identity differs from its external script authority");
    }
    Ok(())
}

fn is_empty_string_array(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_array)
        .is_some_and(|values| values.is_empty())
}

fn require_population_shape(population: &Value, bindings: &ReceiptBindings) -> anyhow::Result<()> {
    if bindings
        .development_samples
        .checked_mul(u64::try_from(super::REQUIRED_SCENARIOS.len())?)
        != Some(bindings.expected_sample_records)
    {
        bail!("viewer external receipt authority has inconsistent sample bounds");
    }
    require_object_keys(
        population,
        &[
            "expected_sample_records",
            "observed_sample_records",
            "expected_role_attempts",
            "observed_role_attempts",
            "completed_role_reports",
            "expected_phase_evaluations",
            "observed_phase_evaluations",
            "expected_product_gate_observations",
            "observed_product_gate_observations",
            "sample_identities_exact",
            "sample_order_exact",
            "role_identities_exact",
            "phase_identities_exact",
            "product_gate_bijections_exact",
            "exact",
        ],
        "viewer raw population",
    )?;
    for field in [
        "expected_sample_records",
        "observed_sample_records",
        "expected_role_attempts",
        "observed_role_attempts",
        "completed_role_reports",
        "expected_phase_evaluations",
        "observed_phase_evaluations",
        "expected_product_gate_observations",
        "observed_product_gate_observations",
    ] {
        if population.get(field).and_then(Value::as_u64).is_none() {
            bail!("viewer raw population field {field:?} is malformed");
        }
    }
    for field in [
        "sample_identities_exact",
        "sample_order_exact",
        "role_identities_exact",
        "phase_identities_exact",
        "product_gate_bijections_exact",
        "exact",
    ] {
        if population.get(field).and_then(Value::as_bool).is_none() {
            bail!("viewer raw population field {field:?} is malformed");
        }
    }
    let number = |field: &str| {
        population
            .get(field)
            .and_then(Value::as_u64)
            .expect("population number fields were validated")
    };
    for (field, expected) in [
        ("expected_sample_records", bindings.expected_sample_records),
        ("expected_role_attempts", bindings.expected_role_attempts),
        (
            "expected_phase_evaluations",
            bindings.expected_phase_evaluations,
        ),
        (
            "expected_product_gate_observations",
            bindings.expected_product_gate_observations,
        ),
    ] {
        if number(field) != expected {
            bail!("viewer raw population field {field:?} differs from its external authority");
        }
    }
    let exact = number("expected_sample_records") == number("observed_sample_records")
        && number("expected_role_attempts") == number("observed_role_attempts")
        && number("expected_role_attempts") == number("completed_role_reports")
        && number("expected_phase_evaluations") == number("observed_phase_evaluations")
        && number("expected_product_gate_observations")
            == number("observed_product_gate_observations")
        && [
            "sample_identities_exact",
            "sample_order_exact",
            "role_identities_exact",
            "phase_identities_exact",
            "product_gate_bijections_exact",
        ]
        .into_iter()
        .all(|field| population.get(field).and_then(Value::as_bool) == Some(true));
    if population.get("exact").and_then(Value::as_bool) != Some(exact) {
        bail!("viewer raw population exactness is internally inconsistent");
    }
    Ok(())
}

fn validate_overhead_rows(
    overhead: &Value,
    raw: &Value,
    bindings: &ReceiptBindings,
    evidence_valid: bool,
) -> anyhow::Result<()> {
    let rows = overhead
        .as_array()
        .context("viewer overhead populations are not an array")?;
    if rows.len() != super::REQUIRED_SCENARIOS.len() {
        bail!("viewer overhead population has the wrong scenario count");
    }
    for (row, scenario) in rows.iter().zip(super::REQUIRED_SCENARIOS) {
        require_object_keys(
            row,
            &[
                "scenario",
                "evaluation_scope",
                "automatic_retries",
                "sample_filtering",
                "expected_sample_pairs",
                "observed_sample_pairs",
                "wall_adjustment_authority",
                "instrumented_raw_app_wall_time_ns",
                "instrumented_qualification_gpu_timing_await_wall_time_ns",
                "instrumented_adjusted_app_wall_time_ns",
                "control_app_wall_time_ns",
                "wall_overhead_basis_points",
                "instrumented_process_cpu_time_ns",
                "control_process_cpu_time_ns",
                "process_cpu_overhead_basis_points",
                "maximum_overhead_basis_points",
                "population_complete",
                "gate_evaluable",
                "gate_passed",
            ],
            "viewer overhead population",
        )?;
        for field in [
            "instrumented_raw_app_wall_time_ns",
            "instrumented_qualification_gpu_timing_await_wall_time_ns",
            "instrumented_adjusted_app_wall_time_ns",
            "control_app_wall_time_ns",
            "wall_overhead_basis_points",
            "instrumented_process_cpu_time_ns",
            "control_process_cpu_time_ns",
            "process_cpu_overhead_basis_points",
            "gate_passed",
        ] {
            if !matches!(
                row.get(field),
                Some(Value::Null | Value::Number(_) | Value::Bool(_))
            ) || (field == "gate_passed"
                && !matches!(row.get(field), Some(Value::Null | Value::Bool(_))))
                || (field != "gate_passed"
                    && row.get(field) != Some(&Value::Null)
                    && row.get(field).and_then(Value::as_u64).is_none())
            {
                bail!("viewer overhead population field {field:?} is malformed");
            }
        }
        let observed_pairs = row
            .get("observed_sample_pairs")
            .and_then(Value::as_u64)
            .context("viewer overhead observed sample-pair count is malformed")?;
        let gate_evaluable = row
            .get("gate_evaluable")
            .and_then(Value::as_bool)
            .context("viewer overhead evaluability fact is malformed")?;
        if row.get("scenario").and_then(Value::as_str) != Some(scenario)
            || row.get("evaluation_scope").and_then(Value::as_str)
                != Some("complete_balanced_development_sample_population")
            || row.get("automatic_retries").and_then(Value::as_u64) != Some(0)
            || row.get("sample_filtering").and_then(Value::as_str) != Some("none")
            || row.get("expected_sample_pairs").and_then(Value::as_u64)
                != Some(bindings.development_samples)
            || observed_pairs > bindings.development_samples
            || row.get("wall_adjustment_authority").and_then(Value::as_str)
                != Some(
                    "exact_sum_of_successful_qualification_only_await_active_view_gpu_timing_waited_ns",
                )
            || row
                .get("maximum_overhead_basis_points")
                .and_then(Value::as_u64)
                != Some(bindings.maximum_overhead_basis_points)
            || row
                .get("population_complete")
                .and_then(Value::as_bool)
                .is_none()
            || row.get("gate_passed").and_then(Value::as_bool).is_some() != gate_evaluable
            || (evidence_valid
                && (observed_pairs != bindings.development_samples
                    || row.get("population_complete").and_then(Value::as_bool) != Some(true)
                    || !gate_evaluable
                    || row.get("gate_passed").and_then(Value::as_bool) != Some(true)))
        {
            bail!("viewer overhead population is incomplete or inconsistent");
        }
    }
    if !evidence_valid {
        return Ok(());
    }
    let attempts = raw
        .get("attempts")
        .and_then(Value::as_array)
        .context("viewer raw attempts are unavailable for overhead reconciliation")?;
    for (row, scenario) in rows.iter().zip(super::REQUIRED_SCENARIOS) {
        let scenario_attempts = attempts
            .iter()
            .filter(|attempt| attempt.get("scenario").and_then(Value::as_str) == Some(scenario))
            .collect::<Vec<_>>();
        if u64::try_from(scenario_attempts.len())? != bindings.development_samples {
            bail!("viewer overhead attempt population differs from its external authority");
        }
        let mut instrumented_raw = 0_u64;
        let mut qualification_wait = 0_u64;
        let mut instrumented_adjusted = 0_u64;
        let mut control_wall = 0_u64;
        let mut instrumented_cpu = 0_u64;
        let mut control_cpu = 0_u64;
        for attempt in scenario_attempts {
            let paired = attempt
                .get("paired_overhead")
                .context("viewer raw attempt lacks paired overhead")?;
            require_object_keys(
                paired,
                &[
                    "evaluation_scope",
                    "instrumented_raw_app_wall_time_ns",
                    "instrumented_qualification_gpu_timing_await_wall_time_ns",
                    "instrumented_adjusted_app_wall_time_ns",
                    "control_app_wall_time_ns",
                    "wall_basis_points",
                    "process_cpu_basis_points",
                ],
                "viewer raw paired overhead",
            )?;
            if paired.get("evaluation_scope").and_then(Value::as_str)
                != Some(
                    "per_pair_observation_only_gate_applies_to_complete_balanced_scenario_population",
                )
            {
                bail!("viewer raw paired-overhead scope changed");
            }
            let paired_number = |field: &str| {
                paired.get(field).and_then(Value::as_u64).with_context(|| {
                    format!("viewer raw paired-overhead field {field:?} is malformed")
                })
            };
            let raw_wall = paired_number("instrumented_raw_app_wall_time_ns")?;
            let wait = paired_number("instrumented_qualification_gpu_timing_await_wall_time_ns")?;
            let adjusted = paired_number("instrumented_adjusted_app_wall_time_ns")?;
            let control = paired_number("control_app_wall_time_ns")?;
            let instrumented_role_wall = attempt
                .pointer("/instrumented/process/app_wall_time_ns")
                .and_then(Value::as_u64)
                .context("viewer raw instrumented app wall time is malformed")?;
            let control_role_wall = attempt
                .pointer("/instrumentation_control/process/app_wall_time_ns")
                .and_then(Value::as_u64)
                .context("viewer raw control app wall time is malformed")?;
            let instrumented_role_cpu = attempt
                .pointer("/instrumented/process/process_cpu_time_ns")
                .and_then(Value::as_u64)
                .context("viewer raw instrumented CPU time is malformed")?;
            let control_role_cpu = attempt
                .pointer("/instrumentation_control/process/process_cpu_time_ns")
                .and_then(Value::as_u64)
                .context("viewer raw control CPU time is malformed")?;
            if raw_wall != instrumented_role_wall
                || control != control_role_wall
                || raw_wall.checked_sub(wait) != Some(adjusted)
                || paired_number("wall_basis_points")? != overhead_basis_points(adjusted, control)?
                || paired_number("process_cpu_basis_points")?
                    != overhead_basis_points(instrumented_role_cpu, control_role_cpu)?
            {
                bail!("viewer raw paired-overhead operands do not reconcile");
            }
            instrumented_raw = instrumented_raw
                .checked_add(raw_wall)
                .context("viewer overhead raw-wall sum overflowed")?;
            qualification_wait = qualification_wait
                .checked_add(wait)
                .context("viewer overhead wait sum overflowed")?;
            instrumented_adjusted = instrumented_adjusted
                .checked_add(adjusted)
                .context("viewer overhead adjusted-wall sum overflowed")?;
            control_wall = control_wall
                .checked_add(control)
                .context("viewer overhead control-wall sum overflowed")?;
            instrumented_cpu = instrumented_cpu
                .checked_add(instrumented_role_cpu)
                .context("viewer overhead instrumented-CPU sum overflowed")?;
            control_cpu = control_cpu
                .checked_add(control_role_cpu)
                .context("viewer overhead control-CPU sum overflowed")?;
        }
        if instrumented_raw.checked_sub(qualification_wait) != Some(instrumented_adjusted) {
            bail!("viewer overhead population wait subtraction does not reconcile");
        }
        let wall_basis_points = overhead_basis_points(instrumented_adjusted, control_wall)?;
        let cpu_basis_points = overhead_basis_points(instrumented_cpu, control_cpu)?;
        let gate_passed = wall_basis_points <= bindings.maximum_overhead_basis_points
            && cpu_basis_points <= bindings.maximum_overhead_basis_points;
        if !gate_passed {
            bail!("viewer complete evidence exceeds its instrumentation-overhead authority");
        }
        let expected = json!({
            "scenario": scenario,
            "evaluation_scope": "complete_balanced_development_sample_population",
            "automatic_retries": 0,
            "sample_filtering": "none",
            "expected_sample_pairs": bindings.development_samples,
            "observed_sample_pairs": bindings.development_samples,
            "wall_adjustment_authority": "exact_sum_of_successful_qualification_only_await_active_view_gpu_timing_waited_ns",
            "instrumented_raw_app_wall_time_ns": instrumented_raw,
            "instrumented_qualification_gpu_timing_await_wall_time_ns": qualification_wait,
            "instrumented_adjusted_app_wall_time_ns": instrumented_adjusted,
            "control_app_wall_time_ns": control_wall,
            "wall_overhead_basis_points": wall_basis_points,
            "instrumented_process_cpu_time_ns": instrumented_cpu,
            "control_process_cpu_time_ns": control_cpu,
            "process_cpu_overhead_basis_points": cpu_basis_points,
            "maximum_overhead_basis_points": bindings.maximum_overhead_basis_points,
            "population_complete": true,
            "gate_evaluable": true,
            "gate_passed": true,
        });
        if row != &expected {
            bail!("viewer overhead population is not the exact recomputation of its attempts");
        }
    }
    Ok(())
}

fn overhead_basis_points(instrumented: u64, control: u64) -> anyhow::Result<u64> {
    if control == 0 {
        bail!("viewer overhead control operand must be nonzero");
    }
    let basis_points = u128::from(instrumented.saturating_sub(control))
        .saturating_mul(10_000)
        .div_ceil(u128::from(control));
    u64::try_from(basis_points).context("viewer overhead basis points overflowed")
}

fn validate_receipt_privacy(
    receipt: &Value,
    raw: &Value,
    bindings: &ReceiptBindings,
) -> anyhow::Result<()> {
    let mut private = bindings.private_strings.clone();
    collect_strings(
        raw.get("private_paths").unwrap_or(&Value::Null),
        &mut private,
    );
    fn visit(value: &Value, private: &[String]) -> anyhow::Result<()> {
        match value {
            Value::String(string) => {
                if Path::new(string).is_absolute()
                    || string.starts_with("m4d-sc-v1-sha256:")
                    || string.starts_with("m4d-package-v1-sha256:")
                    || private
                        .iter()
                        .filter(|private| !private.is_empty())
                        .any(|private| string.contains(private))
                {
                    bail!("viewer development receipt retained a private value");
                }
            }
            Value::Array(values) => {
                for value in values {
                    visit(value, private)?;
                }
            }
            Value::Object(values) => {
                for value in values.values() {
                    visit(value, private)?;
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
        Ok(())
    }
    visit(receipt, &private)
}

fn collect_strings(value: &Value, destination: &mut Vec<String>) {
    match value {
        Value::String(string) => destination.push(string.clone()),
        Value::Array(values) => {
            for value in values {
                collect_strings(value, destination);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                collect_strings(value, destination);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

pub(super) fn canonical_json_bytes(value: &Value) -> anyhow::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .context("failed to encode canonical viewer evidence JSON")?;
    bytes.push(b'\n');
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > RAW_REPORT_MAX_BYTES {
        bail!("viewer evidence JSON exceeds its aggregate bound");
    }
    Ok(bytes)
}

fn require_canonical_json_bytes(value: &Value, bytes: &[u8], label: &str) -> anyhow::Result<()> {
    if canonical_json_bytes(value)? != bytes {
        bail!("{label} is not the exact canonical pretty-JSON encoding");
    }
    Ok(())
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

fn require_sha256(value: &str, label: &str) -> anyhow::Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{label} must be one lowercase SHA-256 digest");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    fn bindings() -> ReceiptBindings {
        let conformance = conformance_receipt::valid_test_evidence();
        let expected_attempts = (1_u64..=3)
            .flat_map(|sample_index| {
                super::super::REQUIRED_SCENARIOS
                    .into_iter()
                    .map(move |scenario| ExpectedAttemptAuthority {
                        sample_index,
                        scenario: scenario.to_owned(),
                        phases: Vec::new(),
                        instrumented: empty_role_authority("instrumented"),
                        instrumentation_control: empty_role_authority("instrumentation-control"),
                    })
            })
            .collect();
        ReceiptBindings {
            qualification_profile_sha256: "1".repeat(64),
            owner_accepted_profile_contract_sha256: "2".repeat(64),
            ep01_selection_authority_sha256: "3".repeat(64),
            workload_bundle_sha256: "4".repeat(64),
            ep01_trace_geometry_sha256: "5".repeat(64),
            interaction_script_bundle_sha256: "6".repeat(64),
            independent_oracle_sha256: "7".repeat(64),
            representative_package_fingerprint_sha256: "8".repeat(64),
            supporting_temporal_package_fingerprint_sha256: "9".repeat(64),
            build_binding_fingerprint_sha256: "a".repeat(64),
            development_samples: 3,
            expected_sample_records: 30,
            expected_role_attempts: 60,
            expected_phase_evaluations: 0,
            expected_product_gate_observations: 0,
            maximum_overhead_basis_points: 200,
            expected_attempts,
            conformance_commitments: conformance_receipt::test_replay_authority(&conformance),
            private_strings: vec!["private-label".to_owned()],
        }
    }

    fn empty_role_authority(role: &str) -> ExpectedRoleAuthority {
        ExpectedRoleAuthority {
            role: role.to_owned(),
            gate_batch_count: 0,
            gate_observation_count: 0,
            static_wait_bound_ns: 1,
            derived_process_timeout_ns: 2,
            product_gate_outcomes: Vec::new(),
        }
    }

    fn valid_failed_raw(bindings: &ReceiptBindings) -> Value {
        let attempts = (1_u64..=bindings.development_samples)
            .flat_map(|sample_index| {
                super::super::REQUIRED_SCENARIOS
                    .into_iter()
                    .map(move |scenario| {
                        let role = |identity: &str| {
                            json!({
                                "role": identity,
                                "root": "/private/evidence/attempt",
                                "paths": {},
                                "commitments": {},
                                "process": {
                                    "launch_attempted": true,
                                    "exit_code": 0,
                                    "signal": null,
                                    "external_wall_time_ns": 1,
                                    "timed_out": false,
                                    "spawn_error": null,
                                    "app_wall_time_ns": 1,
                                    "process_cpu_time_ns": 1,
                                    "gate_batch_count": 0,
                                    "gate_observation_count": 0,
                                    "static_wait_bound_ns": 1,
                                    "derived_process_timeout_ns": 2,
                                },
                                "import_source_inventory": {},
                                "cleanup": {},
                                "product_gate_outcomes": [],
                                "integrity_reason_codes": [],
                                "evidence_status": "valid_complete",
                                "product_gate_status": "passed",
                            })
                        };
                        json!({
                            "sample_index": sample_index,
                            "scenario": scenario,
                            "instrumented": role("instrumented"),
                            "instrumentation_control": role("instrumentation-control"),
                            "paired_overhead": {
                                "evaluation_scope": "per_pair_observation_only_gate_applies_to_complete_balanced_scenario_population",
                                "instrumented_raw_app_wall_time_ns": 1,
                                "instrumented_qualification_gpu_timing_await_wall_time_ns": 0,
                                "instrumented_adjusted_app_wall_time_ns": 1,
                                "control_app_wall_time_ns": 1,
                                "wall_basis_points": 0,
                                "process_cpu_basis_points": 0,
                            },
                            "phases": [],
                            "integrity_reason_codes": [],
                            "evidence_status": "valid_complete",
                            "product_gate_status": "failed",
                        })
                    })
            })
            .collect::<Vec<_>>();
        let overhead = super::super::REQUIRED_SCENARIOS
            .into_iter()
            .map(|scenario| {
                json!({
                    "scenario": scenario,
                    "evaluation_scope": "complete_balanced_development_sample_population",
                    "automatic_retries": 0,
                    "sample_filtering": "none",
                    "expected_sample_pairs": 3,
                    "observed_sample_pairs": 3,
                    "wall_adjustment_authority": "exact_sum_of_successful_qualification_only_await_active_view_gpu_timing_waited_ns",
                    "instrumented_raw_app_wall_time_ns": 3,
                    "instrumented_qualification_gpu_timing_await_wall_time_ns": 0,
                    "instrumented_adjusted_app_wall_time_ns": 3,
                    "control_app_wall_time_ns": 3,
                    "wall_overhead_basis_points": 0,
                    "instrumented_process_cpu_time_ns": 3,
                    "control_process_cpu_time_ns": 3,
                    "process_cpu_overhead_basis_points": 0,
                    "maximum_overhead_basis_points": 200,
                    "population_complete": true,
                    "gate_evaluable": true,
                    "gate_passed": true,
                })
            })
            .collect::<Vec<_>>();
        json!({
            "schema": RAW_REPORT_SCHEMA,
            "evidence_status": "valid_complete",
            "product_gate_status": "failed",
            "claim_status": "development_E1_semantic_automation_non_OS_input_non_E4_no_product_claim",
            "build_binding": {},
            "protocol": {},
            "private_paths": {"root": "/private/evidence"},
            "commitments": {
                "qualification_profile_sha256": bindings.qualification_profile_sha256,
                "owner_accepted_profile_contract_sha256": bindings.owner_accepted_profile_contract_sha256,
                "ep01_selection_authority_sha256": bindings.ep01_selection_authority_sha256,
                "workload_bundle_sha256": bindings.workload_bundle_sha256,
                "ep01_trace_geometry_sha256": bindings.ep01_trace_geometry_sha256,
                "interaction_script_bundle_sha256": bindings.interaction_script_bundle_sha256,
                "independent_oracle_sha256": bindings.independent_oracle_sha256,
                "app_binary_sha256_before_run": "b".repeat(64),
                "app_binary_sha256_after_run": "b".repeat(64),
                "app_binary_unchanged": true,
                "representative_package_fingerprint_sha256": bindings.representative_package_fingerprint_sha256,
                "supporting_temporal_package_fingerprint_sha256": bindings.supporting_temporal_package_fingerprint_sha256,
                "build_binding_fingerprint_sha256": bindings.build_binding_fingerprint_sha256,
            },
            "bindings": {},
            "executable_conformance": conformance_receipt::valid_test_evidence().raw_json(),
            "population": {
                "expected_sample_records": 30,
                "observed_sample_records": 30,
                "expected_role_attempts": 60,
                "observed_role_attempts": 60,
                "completed_role_reports": 60,
                "expected_phase_evaluations": 0,
                "observed_phase_evaluations": 0,
                "expected_product_gate_observations": 0,
                "observed_product_gate_observations": 0,
                "sample_identities_exact": true,
                "sample_order_exact": true,
                "role_identities_exact": true,
                "phase_identities_exact": true,
                "product_gate_bijections_exact": true,
                "exact": true,
            },
            "instrumentation_overhead_populations": overhead,
            "product_gate_outcomes": [],
            "product_gate_failures": [{
                "sample_index": 1,
                "scenario": "RZ",
                "role": "instrumented",
                "gate_id": "gate.expected_failure",
                "condition": "accepted_gate_satisfied",
                "outcome": "failed",
            }],
            "attempts": attempts,
            "integrity_reason_codes": [],
            "limitations": [],
        })
    }

    #[test]
    fn canonical_receipt_bytes_reject_trailing_or_reformatted_json() {
        let value = json!({"schema": "test", "value": 1});
        let canonical = canonical_json_bytes(&value).unwrap();
        require_canonical_json_bytes(&value, &canonical, "test").unwrap();
        let mut changed = canonical;
        changed.push(b'\n');
        assert!(require_canonical_json_bytes(&value, &changed, "test").is_err());
    }

    #[test]
    fn privacy_validator_rejects_absolute_and_scientific_identifiers() {
        let bindings = bindings();
        for receipt in [
            json!({"value": "/private/path"}),
            json!({"value": format!("m4d-sc-v1-sha256:{}", "0".repeat(64))}),
            json!({"value": "contains-private-label"}),
        ] {
            assert!(validate_receipt_privacy(&receipt, &json!({}), &bindings).is_err());
        }
    }

    fn ip_gate_row(condition: &str, outcome: &str, observed: u64) -> Value {
        json!({
            "schema": PRODUCT_GATE_OBSERVATION_SCHEMA,
            "command_index": 4,
            "batch_id": "IP.batch.000",
            "phase_id": "preprocess_publish.checkpoint.000",
            "observation_index": match condition {
                "import_idle" => 0,
                "imported_open_ready" => 1,
                "runtime_idle" => 2,
                _ => 3,
            },
            "gate_id": format!("IP.acceptance.{condition}"),
            "condition": condition,
            "deadline_authority": "import_primary_wall",
            "deadline_after_origin_ns": 1_200_000_000_000_u64,
            "origin": {"kind": "import_primary_started", "command_index": null},
            "outcome": outcome,
            "condition_met": outcome == "passed",
            "timed_out": false,
            "observed_after_origin_ns": observed,
            "sample_index": 1,
            "scenario": "IP",
            "role": "instrumented",
        })
    }

    #[test]
    fn ip_terminal_prepublication_failure_requires_the_exact_three_row_batch() {
        let pair = vec![
            ip_gate_row("import_idle", "failed", 10),
            ip_gate_row("imported_open_ready", "failed", 10),
            ip_gate_row("runtime_idle", "passed", 10),
        ];
        validate_ip_terminal_prepublication_early_failure_pairs(&pair).unwrap();

        assert!(
            validate_ip_terminal_prepublication_early_failure_pairs(&pair[..1]).is_err(),
            "a singleton early failure is not terminal-prepublication evidence"
        );
        let mut mixed_deadline = pair.clone();
        mixed_deadline[1]["deadline_after_origin_ns"] = json!(1_199_999_999_999_u64);
        assert!(
            validate_ip_terminal_prepublication_early_failure_pairs(&mixed_deadline).is_err(),
            "the paired failures must share the exact import-primary deadline"
        );
        let mut failed_runtime_idle = pair;
        failed_runtime_idle[2]["outcome"] = json!("failed");
        failed_runtime_idle[2]["condition_met"] = json!(false);
        assert!(
            validate_ip_terminal_prepublication_early_failure_pairs(&failed_runtime_idle).is_err(),
            "the runtime-idle sibling must be a coherent pass"
        );
    }

    #[test]
    fn finalized_valid_failed_raw_is_published_and_admitted_without_rerunning() {
        let repository = tempfile::tempdir().unwrap();
        let evidence = tempfile::tempdir().unwrap();
        fs::set_permissions(evidence.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let bindings = bindings();
        let raw = valid_failed_raw(&bindings);
        let raw_path = evidence.path().join(RAW_REPORT_FILE_NAME);
        write_new_synced_private_file(
            &raw_path,
            &canonical_json_bytes(&raw).unwrap(),
            RAW_REPORT_MAX_BYTES,
            "viewer raw private report",
        )
        .unwrap();

        let (receipt_path, receipt) =
            publish_finalized_raw_report(&raw_path, repository.path(), &bindings).unwrap();
        assert_eq!(
            receipt_path.file_name().and_then(|name| name.to_str()),
            Some(RECEIPT_FILE_NAME)
        );
        assert_eq!(receipt["product_gate_status"], "failed");
        assert!(
            !serde_json::to_string(&receipt)
                .unwrap()
                .contains("/private")
        );

        let admission =
            admit_finalized_receipt(&raw_path, &receipt_path, repository.path(), &bindings)
                .unwrap();
        assert_eq!(admission["ep00_evidence_admitted"], true);
        assert_eq!(admission["product_gate_status"], "failed");
    }
}

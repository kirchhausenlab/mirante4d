use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Context;
use serde_json::{Value, json};

use crate::{
    external_ci_evidence::external_ci_completion_plan_json,
    product_validate::t5_qual_001_product_validation_completion_plan_json,
    reports::{read_json_file, write_json_file},
};

mod completion_waivers;
mod policy;

use policy::{
    baseline_audit_report_policy_failure, baseline_refresh_plan_report_policy_failure,
    command_audit_report_policy_failure, external_ci_evidence_report_policy_failure,
    product_validation_report_policy_failure, product_validation_scenario_name,
    verify_e2e_report_policy_failure, verify_render_report_policy_failure,
    verify_ui_report_policy_failure, workflow_audit_report_policy_failure,
};

use completion_waivers::{
    COMPLETION_SCOPE, COMPLETION_WAIVERS_RELATIVE_PATH, apply_completion_waivers,
    completion_waiver_evidence_entry,
};

const TARGET_ROOT: &str = "target/mirante4d";
const REPORT_AUDIT_OUTPUT_DIR: &str = "target/mirante4d/report-audit";
const REPORT_AUDIT_JSON: &str = "target/mirante4d/report-audit/report-audit-report.json";
const REPORT_AUDIT_MD: &str = "target/mirante4d/report-audit/report-audit-report.md";
const REPORT_AUDIT_SCHEMA: &str = "mirante4d-report-audit";
const REPORT_AUDIT_SCHEMA_VERSION: u32 = 1;
const EXTERNAL_CI_EVIDENCE_RELATIVE_PATH: &str = "external-ci/external-ci-evidence.json";
const EXTERNAL_CI_EVIDENCE_SCHEMA: &str = "mirante4d-external-ci-evidence";
const T5_QUAL_001_PRODUCT_VALIDATION_SCENARIOS: [&str; 3] = [
    "t5_qual_001_interaction_mip",
    "t5_qual_001_interaction_render_modes",
    "t5_qual_001_interaction_continuous",
];

#[derive(Debug, Clone, Copy)]
struct CurrentEvidencePath {
    relative_path: &'static str,
    family: &'static str,
    evidence_role: &'static str,
    expected_schema: Option<&'static str>,
    expected_command: Option<&'static str>,
    expected_scenario: Option<&'static str>,
    notes: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct LegacyEvidencePath {
    relative_path: &'static str,
    family: &'static str,
    quarantine_reason: &'static str,
}

const CURRENT_EVIDENCE_PATHS: &[CurrentEvidencePath] = &[
    CurrentEvidencePath {
        relative_path: "command-audit/command-audit-report.json",
        family: "command_surface",
        evidence_role: "current_command_audit_report",
        expected_schema: Some("mirante4d-xtask-command-audit"),
        expected_command: Some("command-audit"),
        expected_scenario: None,
        notes: "Command-surface classification and smoke/heavy quarantine evidence.",
    },
    CurrentEvidencePath {
        relative_path: "command-audit/command-audit-report.md",
        family: "command_surface",
        evidence_role: "current_command_audit_markdown",
        expected_schema: None,
        expected_command: None,
        expected_scenario: None,
        notes: "Human-readable command-surface audit companion.",
    },
    CurrentEvidencePath {
        relative_path: "baseline-audit/baseline-audit-report.json",
        family: "baseline_surface",
        evidence_role: "current_baseline_audit_report",
        expected_schema: Some("mirante4d-baseline-audit"),
        expected_command: Some("baseline-audit"),
        expected_scenario: None,
        notes: "Curated benchmark baseline policy and refresh-state audit.",
    },
    CurrentEvidencePath {
        relative_path: "baseline-audit/baseline-audit-report.md",
        family: "baseline_surface",
        evidence_role: "current_baseline_audit_markdown",
        expected_schema: None,
        expected_command: None,
        expected_scenario: None,
        notes: "Human-readable baseline policy audit companion.",
    },
    CurrentEvidencePath {
        relative_path: "baseline-refresh/baseline-refresh-plan.json",
        family: "baseline_surface",
        evidence_role: "current_baseline_refresh_plan_report",
        expected_schema: Some("mirante4d-baseline-refresh-plan"),
        expected_command: Some("baseline-refresh-plan"),
        expected_scenario: None,
        notes: "Non-mutating stale-baseline-to-report refresh plan.",
    },
    CurrentEvidencePath {
        relative_path: "workflow-audit/workflow-audit-report.json",
        family: "ci_surface",
        evidence_role: "current_workflow_audit_report",
        expected_schema: Some("mirante4d-workflow-audit"),
        expected_command: Some("workflow-audit"),
        expected_scenario: None,
        notes: "Static GitHub Actions workflow evidence-surface audit.",
    },
    CurrentEvidencePath {
        relative_path: "workflow-audit/workflow-audit-report.md",
        family: "ci_surface",
        evidence_role: "current_workflow_audit_markdown",
        expected_schema: None,
        expected_command: None,
        expected_scenario: None,
        notes: "Human-readable workflow audit companion.",
    },
    CurrentEvidencePath {
        relative_path: EXTERNAL_CI_EVIDENCE_RELATIVE_PATH,
        family: "ci_surface",
        evidence_role: "current_external_ci_run_evidence",
        expected_schema: Some(EXTERNAL_CI_EVIDENCE_SCHEMA),
        expected_command: Some("external-ci-evidence"),
        expected_scenario: None,
        notes: "Externally inspected hosted CPU and self-hosted GPU CI run evidence.",
    },
    CurrentEvidencePath {
        relative_path: "report-audit/report-audit-report.json",
        family: "report_surface",
        evidence_role: "current_report_audit_report",
        expected_schema: Some(REPORT_AUDIT_SCHEMA),
        expected_command: Some("report-audit"),
        expected_scenario: None,
        notes: "Previous report-audit output, if this command has run before.",
    },
    CurrentEvidencePath {
        relative_path: "report-audit/report-audit-report.md",
        family: "report_surface",
        evidence_role: "current_report_audit_markdown",
        expected_schema: None,
        expected_command: None,
        expected_scenario: None,
        notes: "Previous human-readable report-audit companion, if present.",
    },
    CurrentEvidencePath {
        relative_path: "verify-e2e/verify-e2e-report.json",
        family: "verify",
        evidence_role: "current_e2e_report",
        expected_schema: Some("mirante4d-verify-e2e-report"),
        expected_command: Some("verify-e2e"),
        expected_scenario: None,
        notes: "Sectioned library, virtual automation, and real-window product-validation report.",
    },
    CurrentEvidencePath {
        relative_path: "verify-render/verify-render-report.json",
        family: "verify",
        evidence_role: "current_gpu_render_report",
        expected_schema: Some("mirante4d-verify-render-report"),
        expected_command: Some("verify-render"),
        expected_scenario: None,
        notes: "GPU render report with adapter/resource/test inventory evidence.",
    },
    CurrentEvidencePath {
        relative_path: "verify-ui/verify-ui-report.json",
        family: "verify",
        evidence_role: "current_ui_report",
        expected_schema: Some("mirante4d-verify-ui-report"),
        expected_command: Some("verify-ui"),
        expected_scenario: None,
        notes: "UI semantic and visual snapshot inventory report.",
    },
    CurrentEvidencePath {
        relative_path: "product-validation/generated_fixture_camera_smoke/product-validation-report.json",
        family: "product_validation",
        evidence_role: "current_generated_fixture_camera_product_report",
        expected_schema: Some("mirante4d-product-validation-report"),
        expected_command: Some("product-validate"),
        expected_scenario: Some("generated_fixture_camera_smoke"),
        notes: "Scenario-scoped generated-fixture camera automation report.",
    },
    CurrentEvidencePath {
        relative_path: "product-validation/generated_fixture_camera_smoke/product-automation-script.json",
        family: "product_validation",
        evidence_role: "current_generated_fixture_camera_script",
        expected_schema: Some("mirante4d-product-automation-script"),
        expected_command: None,
        expected_scenario: None,
        notes: "Script used by generated-fixture camera automation.",
    },
    CurrentEvidencePath {
        relative_path: "product-validation/generated_fixture_render_modes/product-validation-report.json",
        family: "product_validation",
        evidence_role: "current_generated_fixture_render_modes_product_report",
        expected_schema: Some("mirante4d-product-validation-report"),
        expected_command: Some("product-validate"),
        expected_scenario: Some("generated_fixture_render_modes"),
        notes: "Scenario-scoped generated-fixture MIP/DVR/ISO automation report.",
    },
    CurrentEvidencePath {
        relative_path: "product-validation/generated_fixture_render_modes/product-automation-script.json",
        family: "product_validation",
        evidence_role: "current_generated_fixture_render_modes_script",
        expected_schema: Some("mirante4d-product-automation-script"),
        expected_command: None,
        expected_scenario: None,
        notes: "Script used by generated-fixture MIP/DVR/ISO automation.",
    },
    CurrentEvidencePath {
        relative_path: "product-validation/t5_qual_001_interaction_mip/product-validation-report.json",
        family: "product_validation",
        evidence_role: "current_t5_qual_001_interaction_mip_product_report",
        expected_schema: Some("mirante4d-product-validation-report"),
        expected_command: Some("product-validate"),
        expected_scenario: Some("t5_qual_001_interaction_mip"),
        notes: "Scenario-scoped heavy-gated T5Qual001 MIP interaction report.",
    },
    CurrentEvidencePath {
        relative_path: "product-validation/t5_qual_001_interaction_mip/product-automation-script.json",
        family: "product_validation",
        evidence_role: "current_t5_qual_001_interaction_mip_script",
        expected_schema: Some("mirante4d-product-automation-script"),
        expected_command: None,
        expected_scenario: None,
        notes: "Script used by heavy-gated T5Qual001 MIP automation.",
    },
    CurrentEvidencePath {
        relative_path: "product-validation/t5_qual_001_interaction_render_modes/product-validation-report.json",
        family: "product_validation",
        evidence_role: "current_t5_qual_001_interaction_render_modes_product_report",
        expected_schema: Some("mirante4d-product-validation-report"),
        expected_command: Some("product-validate"),
        expected_scenario: Some("t5_qual_001_interaction_render_modes"),
        notes: "Scenario-scoped heavy-gated T5Qual001 MIP/DVR/ISO interaction report.",
    },
    CurrentEvidencePath {
        relative_path: "product-validation/t5_qual_001_interaction_render_modes/product-automation-script.json",
        family: "product_validation",
        evidence_role: "current_t5_qual_001_interaction_render_modes_script",
        expected_schema: Some("mirante4d-product-automation-script"),
        expected_command: None,
        expected_scenario: None,
        notes: "Script used by heavy-gated T5Qual001 render-mode automation.",
    },
    CurrentEvidencePath {
        relative_path: "product-validation/t5_qual_001_interaction_continuous/product-validation-report.json",
        family: "product_validation",
        evidence_role: "current_t5_qual_001_interaction_continuous_product_report",
        expected_schema: Some("mirante4d-product-validation-report"),
        expected_command: Some("product-validate"),
        expected_scenario: Some("t5_qual_001_interaction_continuous"),
        notes: "Scenario-scoped heavy-gated T5Qual001 continuous interaction report.",
    },
    CurrentEvidencePath {
        relative_path: "product-validation/t5_qual_001_interaction_continuous/product-automation-script.json",
        family: "product_validation",
        evidence_role: "current_t5_qual_001_interaction_continuous_script",
        expected_schema: Some("mirante4d-product-automation-script"),
        expected_command: None,
        expected_scenario: None,
        notes: "Script used by heavy-gated T5Qual001 continuous interaction automation.",
    },
    CurrentEvidencePath {
        relative_path: "product-validation/custom_script/product-validation-report.json",
        family: "product_validation",
        evidence_role: "current_custom_script_product_report",
        expected_schema: Some("mirante4d-product-validation-report"),
        expected_command: Some("product-validate"),
        expected_scenario: Some("custom_script"),
        notes: "Scenario-scoped custom product automation report.",
    },
    CurrentEvidencePath {
        relative_path: "product-validation/custom_script/product-automation-script.json",
        family: "product_validation",
        evidence_role: "current_custom_script",
        expected_schema: Some("mirante4d-product-automation-script"),
        expected_command: None,
        expected_scenario: None,
        notes: "Custom automation script copied into the scenario-scoped evidence directory.",
    },
];

const LEGACY_EVIDENCE_PATHS: &[LegacyEvidencePath] = &[
    LegacyEvidencePath {
        relative_path: "product-validation/product-validation-report.json",
        family: "product_validation",
        quarantine_reason: "legacy_root_product_validation_report",
    },
    LegacyEvidencePath {
        relative_path: "product-validation/product-automation-report.json",
        family: "product_validation",
        quarantine_reason: "legacy_root_product_automation_report",
    },
    LegacyEvidencePath {
        relative_path: "product-validation/product-automation-script.json",
        family: "product_validation",
        quarantine_reason: "legacy_root_product_automation_script",
    },
    LegacyEvidencePath {
        relative_path: "product-validation/mirante4d-app.stdout.log",
        family: "product_validation",
        quarantine_reason: "legacy_root_product_validation_log",
    },
    LegacyEvidencePath {
        relative_path: "product-validation/mirante4d-app.stderr.log",
        family: "product_validation",
        quarantine_reason: "legacy_root_product_validation_log",
    },
];

const CURRENT_BENCHMARK_NAMES: &[&str] = &[
    "bench-smoke",
    "bench-native-package",
    "bench-phase11-large-view",
    "bench-phase11-interaction",
    "bench-phase11-viewport-matrix",
    "bench-phase11-synthetic-matrix",
    "bench-phase13-renderer",
    "bench-phase13-viewport-matrix",
    "bench-runtime-stress",
    "bench-import-sample",
    "bench-phase14-multichannel",
    "bench-phase15-analysis",
];

pub(crate) fn report_audit() -> anyhow::Result<PathBuf> {
    fs::create_dir_all(REPORT_AUDIT_OUTPUT_DIR)
        .with_context(|| format!("failed to create {REPORT_AUDIT_OUTPUT_DIR}"))?;
    let root = Path::new(TARGET_ROOT);
    let report_path = Path::new(REPORT_AUDIT_JSON);
    let markdown_path = Path::new(REPORT_AUDIT_MD);

    let first_pass = report_audit_report_json(root)?;
    write_json_file(report_path, &first_pass)?;

    let report = report_audit_report_json(root)?;
    write_json_file(report_path, &report)?;
    fs::write(markdown_path, report_audit_markdown(&report))
        .with_context(|| format!("failed to write {}", markdown_path.display()))?;
    if report["status"] == "failed" {
        anyhow::bail!(
            "report audit found blocking stale or malformed evidence; see {}",
            report_path.display()
        );
    }
    Ok(report_path.to_path_buf())
}

fn report_audit_report_json(root: &Path) -> anyhow::Result<Value> {
    let mut entries = Vec::new();
    let mut known_relative_paths = BTreeSet::new();

    for spec in CURRENT_EVIDENCE_PATHS {
        known_relative_paths.insert(spec.relative_path.to_owned());
        entries.push(current_evidence_entry(root, spec));
    }

    for spec in LEGACY_EVIDENCE_PATHS {
        known_relative_paths.insert(spec.relative_path.to_owned());
        entries.push(legacy_evidence_entry(root, spec));
    }

    for relative_path in discover_auditable_relative_paths(root)? {
        if known_relative_paths.contains(&relative_path) || should_skip_discovered(&relative_path) {
            continue;
        }
        if relative_path == COMPLETION_WAIVERS_RELATIVE_PATH {
            entries.push(completion_waiver_evidence_entry(root, &relative_path));
            continue;
        }
        entries.push(discovered_evidence_entry(root, &relative_path));
    }

    entries.sort_by(|left, right| {
        left["path"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["path"].as_str().unwrap_or_default())
    });

    let blocking_count = entries
        .iter()
        .filter(|entry| entry["blocking"].as_bool().unwrap_or(false))
        .count();
    let current_present_count = entries
        .iter()
        .filter(|entry| entry["classification"] == "current_evidence")
        .filter(|entry| entry["presence"] == "present")
        .count();
    let current_missing_count = entries
        .iter()
        .filter(|entry| entry["classification"] == "current_evidence")
        .filter(|entry| entry["presence"] == "missing")
        .count();
    let stale_legacy_present_count = entries
        .iter()
        .filter(|entry| entry["classification"] == "stale_legacy_evidence")
        .filter(|entry| entry["presence"] == "present")
        .count();
    let quarantined_discovered_count = entries
        .iter()
        .filter(|entry| entry["classification"] == "quarantined_discovered_evidence")
        .count();

    let completion_readiness = completion_readiness_report(root);

    Ok(json!({
        "schema": REPORT_AUDIT_SCHEMA,
        "schema_version": REPORT_AUDIT_SCHEMA_VERSION,
        "command": "report-audit",
        "status": if blocking_count == 0 { "passed" } else { "failed" },
        "failure_reason": if blocking_count == 0 {
            Value::Null
        } else {
            json!("blocking stale or malformed evidence paths were found")
        },
        "target_root": display_path(TARGET_ROOT),
        "summary": {
            "entry_count": entries.len(),
            "current_present_count": current_present_count,
            "current_missing_count": current_missing_count,
            "stale_legacy_present_count": stale_legacy_present_count,
            "quarantined_discovered_count": quarantined_discovered_count,
            "blocking_count": blocking_count,
        },
        "requirements": {
            "current_report_paths_are_schema_checked": true,
            "legacy_root_product_validation_reports_are_blocking": true,
            "unknown_target_reports_are_quarantined_not_silent": true,
            "missing_optional_evidence_is_not_treated_as_passed": true,
        },
        "completion_readiness": completion_readiness,
        "entries": entries,
    }))
}

#[derive(Debug, Clone)]
struct GitCheckoutState {
    head_sha: Option<String>,
    dirty_worktree: Option<bool>,
}

fn completion_readiness_report(root: &Path) -> Value {
    completion_readiness_report_with_git_state(root, &current_git_checkout_state())
}

fn completion_readiness_report_with_git_state(root: &Path, git_state: &GitCheckoutState) -> Value {
    let mut blockers = Vec::new();

    match read_json_file(&root.join("baseline-audit/baseline-audit-report.json")) {
        Ok(report)
            if report.get("status").and_then(Value::as_str) == Some("passed")
                && baseline_audit_report_policy_failure(&report).is_none() => {}
        Ok(report) => blockers.push(baseline_refresh_pending_blocker(root, &report)),
        Err(err) => blockers.push(json!({
            "code": "baseline_audit_missing",
            "path": display_target_relative("baseline-audit/baseline-audit-report.json"),
            "reason": err.to_string(),
            "required_action": "run cargo xtask baseline-audit",
        })),
    }

    match read_json_file(&root.join("workflow-audit/workflow-audit-report.json")) {
        Ok(report) if workflow_audit_report_policy_failure(&report).is_none() => {}
        Ok(report) => blockers.push(json!({
            "code": "workflow_audit_not_current",
            "path": display_target_relative("workflow-audit/workflow-audit-report.json"),
            "status": report.get("status").and_then(Value::as_str),
            "reason": workflow_audit_report_policy_failure(&report).unwrap_or_else(|| "workflow-audit report is not policy current".to_owned()),
            "required_action": "run cargo xtask workflow-audit and inspect static CI workflow policy results",
        })),
        Err(err) => blockers.push(json!({
            "code": "workflow_audit_missing",
            "path": display_target_relative("workflow-audit/workflow-audit-report.json"),
            "reason": err.to_string(),
            "required_action": "run cargo xtask workflow-audit",
        })),
    }

    match read_json_file(&root.join(EXTERNAL_CI_EVIDENCE_RELATIVE_PATH)) {
        Ok(report) => {
            let failure = external_ci_evidence_report_policy_failure(&report)
                .or_else(|| external_ci_evidence_checkout_failure(&report, git_state));
            if let Some(reason) = failure {
                blockers.push(external_ci_evidence_blocker(
                    "external_ci_run_evidence_pending",
                    Some(&report),
                    reason,
                ));
            }
        }
        Err(err) => blockers.push(external_ci_evidence_blocker(
            "external_ci_run_evidence_missing",
            None,
            err.to_string(),
        )),
    }

    for scenario in T5_QUAL_001_PRODUCT_VALIDATION_SCENARIOS {
        let relative_path = format!("product-validation/{scenario}/product-validation-report.json");
        match read_json_file(&root.join(&relative_path)) {
            Ok(report) if t5_qual_001_product_validation_readiness_failure(scenario, &report).is_none() => {}
            Ok(report) => blockers.push(json!({
                "code": "t5_qual_001_product_open_validation_not_current",
                "path": display_target_relative(&relative_path),
                "scenario": scenario,
                "status": report.get("status").and_then(Value::as_str),
                "reason": t5_qual_001_product_validation_readiness_failure(scenario, &report)
                    .unwrap_or_else(|| "current T5Qual001 scenario evidence is not a passed real native-window product validation report".to_owned()),
                "required_action": "run the bounded heavy T5Qual001 product-validation scenario on a real display, or explicitly waive current T5Qual001 product-open validation for this milestone",
                "t5_qual_001_product_validation_completion_plan": t5_qual_001_product_validation_completion_plan_json(scenario, Some(&report)),
            })),
            Err(err) => blockers.push(json!({
                "code": "t5_qual_001_product_validation_report_missing",
                "path": display_target_relative(&relative_path),
                "scenario": scenario,
                "reason": err.to_string(),
                "required_action": "run cargo xtask product-validate <t5-qual-001-package.m4d> for the T5Qual001 scenario, or generate an explicit unsupported preflight report",
                "t5_qual_001_product_validation_completion_plan": t5_qual_001_product_validation_completion_plan_json(scenario, None),
            })),
        }
    }

    let (blockers, waived_blockers, waiver_status) = apply_completion_waivers(root, blockers);

    json!({
        "scope": COMPLETION_SCOPE,
        "status": if blockers.is_empty() { "ready" } else { "not_ready" },
        "blocking_count": blockers.len(),
        "blockers": blockers,
        "waived_blocking_count": waived_blockers.len(),
        "waived_blockers": waived_blockers,
        "waiver_status": waiver_status,
    })
}

fn current_git_checkout_state() -> GitCheckoutState {
    GitCheckoutState {
        head_sha: git_command_output(&["rev-parse", "HEAD"]),
        dirty_worktree: git_dirty_worktree(),
    }
}

fn git_command_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn git_dirty_worktree() -> Option<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(!output.stdout.is_empty())
}

fn external_ci_evidence_checkout_failure(
    report: &Value,
    git_state: &GitCheckoutState,
) -> Option<String> {
    let Some(head_sha) = git_state
        .head_sha
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        return Some(
            "external CI evidence cannot be proven current because current git HEAD is unavailable"
                .to_owned(),
        );
    };

    match git_state.dirty_worktree {
        Some(false) => {}
        Some(true) => {
            return Some(
                "external CI evidence cannot prove uncommitted dirty-worktree changes".to_owned(),
            );
        }
        None => {
            return Some(
                "external CI evidence cannot be proven current because dirty-worktree status is unavailable"
                    .to_owned(),
            );
        }
    }

    if report.get("git_sha").and_then(Value::as_str) != Some(head_sha) {
        return Some("external CI evidence git_sha must match current git HEAD".to_owned());
    }

    None
}

fn t5_qual_001_product_validation_readiness_failure(
    scenario: &str,
    report: &Value,
) -> Option<String> {
    if report.get("status").and_then(Value::as_str) != Some("passed") {
        return Some(
            "current T5Qual001 scenario evidence is not a passed real native-window product validation report"
                .to_owned(),
        );
    }
    if let Some(reason) = product_validation_report_policy_failure(report) {
        return Some(reason);
    }
    if product_validation_scenario_name(report).as_deref() != Some(scenario) {
        return Some(format!(
            "T5Qual001 product-validation report scenario must be {scenario:?}"
        ));
    }
    if report
        .pointer("/environment/display_class")
        .and_then(Value::as_str)
        != Some("real_display")
    {
        return Some(
            "T5Qual001 product-validation completion requires real_display evidence".to_owned(),
        );
    }
    if report
        .pointer("/environment/product_validate_preflight_only")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Some(
            "T5Qual001 product-validation completion requires a non-preflight product-open run"
                .to_owned(),
        );
    }
    if report.pointer("/dataset/id").and_then(Value::as_str) != Some("phase20-extreme-T5-QUAL-001")
    {
        return Some(
            "T5Qual001 product-validation completion requires the phase20-extreme-T5-QUAL-001 dataset"
                .to_owned(),
        );
    }

    None
}

fn baseline_refresh_pending_blocker(root: &Path, report: &Value) -> Value {
    let mut blocker = json!({
        "code": "baseline_refresh_pending",
        "path": display_target_relative("baseline-audit/baseline-audit-report.json"),
        "status": report.get("status").and_then(Value::as_str),
        "reason": "curated benchmark baselines are not all current-policy usable as hard regression gates",
        "required_action": "rerun and promote refreshed baselines from a clean worktree with cargo xtask baseline-promote or cargo xtask baseline-promote-manifest, or explicitly waive baseline refresh for this milestone",
    });

    let plan_relative_path = "baseline-refresh/baseline-refresh-plan.json";
    match read_json_file(&root.join(plan_relative_path)) {
        Ok(plan) => {
            let script_artifact = baseline_refresh_clean_rerun_script_artifact(root, &plan);
            let mut policy_failure = baseline_refresh_plan_report_policy_failure(&plan);
            if policy_failure.is_none() {
                policy_failure =
                    baseline_refresh_clean_rerun_script_artifact_policy_failure(&script_artifact);
            }
            let policy_current = policy_failure.is_none();
            let plan_status = plan.get("status").and_then(Value::as_str);
            if let Some(object) = blocker.as_object_mut() {
                object.insert(
                    "baseline_refresh_plan".to_owned(),
                    json!({
                    "path": display_target_relative(plan_relative_path),
                    "status": plan_status,
                        "policy_current": policy_current,
                        "policy_failure": policy_failure,
                        "generated_clean_rerun_script": plan.get("generated_clean_rerun_script"),
                        "generated_clean_rerun_script_artifact": script_artifact,
                        "summary": plan.get("summary"),
                    }),
                );
                if policy_current
                    && let Some(next_action) = plan
                        .pointer("/summary/remediation/next_action")
                        .and_then(Value::as_str)
                {
                    let action = plan
	                        .get("generated_clean_rerun_script")
	                        .and_then(Value::as_str)
	                        .filter(|script| !script.trim().is_empty())
	                        .map(|script| {
	                            format!(
	                                "run {script} from a clean worktree; when the refreshed plan is ready, promote the generated manifest or explicitly waive baseline refresh for this milestone"
	                            )
	                        })
	                        .unwrap_or_else(|| {
	                            format!(
	                                "{next_action}; when the refreshed plan is ready, promote the generated manifest or explicitly waive baseline refresh for this milestone"
	                            )
	                        });
                    object.insert("required_action".to_owned(), json!(action));
                }
            }
        }
        Err(err) => {
            if let Some(object) = blocker.as_object_mut() {
                object.insert(
                    "baseline_refresh_plan".to_owned(),
                    json!({
                        "path": display_target_relative(plan_relative_path),
                        "status": "missing",
                        "policy_current": false,
                        "policy_failure": err.to_string(),
                    }),
                );
            }
        }
    }

    blocker
}

fn baseline_refresh_clean_rerun_script_artifact(root: &Path, plan: &Value) -> Value {
    let Some(script_path) = plan
        .get("generated_clean_rerun_script")
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
    else {
        return Value::Null;
    };
    let path = target_path_from_report_path(root, script_path);
    let mut artifact = json!({
        "path": script_path,
    });
    let Some(object) = artifact.as_object_mut() else {
        return artifact;
    };
    match fs::read_to_string(&path) {
        Ok(script) => {
            object.insert("status".to_owned(), json!("present"));
            object.insert("byte_count".to_owned(), json!(script.len()));
            object.insert(
                "rerun_command_count".to_owned(),
                json!(
                    script
                        .lines()
                        .filter(|line| line.contains("cargo run --release -p xtask --"))
                        .count()
                ),
            );
            object.insert(
                "baseline_section_count".to_owned(),
                json!(
                    script
                        .lines()
                        .filter(|line| line.contains("baseline-refresh: rerunning"))
                        .count()
                ),
            );
            object.insert(
                "has_clean_worktree_preflight".to_owned(),
                json!(script.contains("git status --porcelain")),
            );
            object.insert(
                "regenerates_plan".to_owned(),
                json!(script.contains("cargo xtask baseline-refresh-plan")),
            );
            object.insert("executable".to_owned(), json!(path_is_executable(&path)));
        }
        Err(err) => {
            object.insert("status".to_owned(), json!("missing"));
            object.insert("failure_reason".to_owned(), json!(err.to_string()));
        }
    }
    artifact
}

fn baseline_refresh_clean_rerun_script_artifact_policy_failure(artifact: &Value) -> Option<String> {
    if artifact.is_null() {
        return None;
    }
    if artifact.get("status").and_then(Value::as_str) != Some("present") {
        return Some("baseline-refresh-plan generated clean-rerun script is missing".to_owned());
    }
    if artifact
        .get("has_clean_worktree_preflight")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Some(
            "baseline-refresh-plan generated clean-rerun script missing clean-worktree preflight"
                .to_owned(),
        );
    }
    if artifact.get("regenerates_plan").and_then(Value::as_bool) != Some(true) {
        return Some(
            "baseline-refresh-plan generated clean-rerun script does not regenerate the plan"
                .to_owned(),
        );
    }
    if artifact
        .get("rerun_command_count")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        == 0
    {
        return Some(
            "baseline-refresh-plan generated clean-rerun script has no rerun commands".to_owned(),
        );
    }
    None
}

fn target_path_from_report_path(root: &Path, report_path: &str) -> PathBuf {
    let normalized = report_path.trim().replace('\\', "/");
    if let Some(relative) = normalized.strip_prefix(&format!("{TARGET_ROOT}/")) {
        root.join(relative)
    } else if normalized.starts_with('/') {
        PathBuf::from(normalized)
    } else {
        root.join(normalized)
    }
}

fn path_is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn external_ci_evidence_blocker(code: &str, report: Option<&Value>, reason: String) -> Value {
    json!({
        "code": code,
        "path": display_target_relative(EXTERNAL_CI_EVIDENCE_RELATIVE_PATH),
        "status": report.and_then(|report| report.get("status")).and_then(Value::as_str),
        "external_run_evidence": report
            .and_then(|report| report.pointer("/summary/external_run_evidence"))
            .and_then(Value::as_str),
        "reason": reason,
        "required_action": "run the hosted CPU CI surface and self-hosted GPU CI surface, then run the External CI Evidence finalizer with both run ids or explicitly waive external CI evidence for this milestone",
        "external_ci_completion_plan": external_ci_completion_plan_json(),
    })
}

fn current_evidence_entry(root: &Path, spec: &CurrentEvidencePath) -> Value {
    let path = root.join(spec.relative_path);
    if !path.exists() {
        return json!({
            "path": display_target_relative(spec.relative_path),
            "family": spec.family,
            "evidence_role": spec.evidence_role,
            "classification": "current_evidence",
            "presence": "missing",
            "audit_status": "not_present",
            "blocking": false,
            "expected_schema": spec.expected_schema,
            "expected_command": spec.expected_command,
            "expected_scenario": spec.expected_scenario,
            "notes": spec.notes,
        });
    }

    if spec.expected_schema.is_none() {
        return json!({
            "path": display_target_relative(spec.relative_path),
            "family": spec.family,
            "evidence_role": spec.evidence_role,
            "classification": "current_evidence",
            "presence": "present",
            "audit_status": "present_current_supporting_artifact",
            "blocking": false,
            "expected_schema": spec.expected_schema,
            "expected_command": spec.expected_command,
            "expected_scenario": spec.expected_scenario,
            "notes": spec.notes,
        });
    }

    match read_json_file(&path) {
        Ok(value) => classify_current_json(spec, &value),
        Err(err) => json!({
            "path": display_target_relative(spec.relative_path),
            "family": spec.family,
            "evidence_role": spec.evidence_role,
            "classification": "current_evidence",
            "presence": "present",
            "audit_status": "present_unreadable_json",
            "blocking": true,
            "expected_schema": spec.expected_schema,
            "expected_command": spec.expected_command,
            "expected_scenario": spec.expected_scenario,
            "failure_reason": err.to_string(),
            "notes": spec.notes,
        }),
    }
}

fn classify_current_json(spec: &CurrentEvidencePath, value: &Value) -> Value {
    let schema = value.get("schema").and_then(Value::as_str);
    let command = value.get("command").and_then(Value::as_str);
    let scenario = product_validation_scenario_name(value);
    let status = value.get("status").and_then(Value::as_str);
    let mut failure_reason = None;
    let mut audit_status = match status {
        Some("passed") => "present_current_passed",
        Some("unsupported") => "present_current_unsupported",
        Some("failed") => "present_current_failed_evidence",
        Some("timed_out") => "present_current_timed_out_evidence",
        Some("skipped") => "present_current_skipped_evidence",
        Some("ready") => "present_current_ready",
        Some("not_ready") => "present_current_not_ready",
        Some("needs_refresh") => "present_current_needs_refresh",
        Some("nothing_to_refresh") => "present_current_nothing_to_refresh",
        Some(_) => "present_current_unknown_evidence_status",
        None => "present_current_no_evidence_status",
    };
    let mut blocking = false;

    if schema != spec.expected_schema {
        blocking = true;
        audit_status = "present_schema_mismatch";
        failure_reason = Some(format!(
            "expected schema {:?}, found {:?}",
            spec.expected_schema, schema
        ));
    } else if spec.expected_command.is_some() && command != spec.expected_command {
        blocking = true;
        audit_status = "present_command_mismatch";
        failure_reason = Some(format!(
            "expected command {:?}, found {:?}",
            spec.expected_command, command
        ));
    } else if spec.expected_scenario.is_some() && scenario.as_deref() != spec.expected_scenario {
        blocking = true;
        audit_status = "present_scenario_mismatch";
        failure_reason = Some(format!(
            "expected scenario {:?}, found {:?}",
            spec.expected_scenario, scenario
        ));
    } else if let Some(policy_failure) = current_json_policy_failure(spec, value) {
        blocking = true;
        audit_status = "present_policy_mismatch";
        failure_reason = Some(policy_failure);
    }

    json!({
        "path": display_target_relative(spec.relative_path),
        "family": spec.family,
        "evidence_role": spec.evidence_role,
        "classification": "current_evidence",
        "presence": "present",
        "audit_status": audit_status,
        "blocking": blocking,
        "expected_schema": spec.expected_schema,
        "actual_schema": schema,
        "expected_command": spec.expected_command,
        "actual_command": command,
        "expected_scenario": spec.expected_scenario,
        "actual_scenario": scenario,
        "evidence_status": status,
        "failure_reason": failure_reason,
        "notes": spec.notes,
    })
}

fn current_json_policy_failure(spec: &CurrentEvidencePath, value: &Value) -> Option<String> {
    match spec.evidence_role {
        "current_command_audit_report" => command_audit_report_policy_failure(value),
        "current_baseline_audit_report" => baseline_audit_report_policy_failure(value),
        "current_baseline_refresh_plan_report" => {
            baseline_refresh_plan_report_policy_failure(value)
        }
        "current_workflow_audit_report" => workflow_audit_report_policy_failure(value),
        "current_external_ci_run_evidence" => external_ci_evidence_report_policy_failure(value),
        "current_e2e_report" => verify_e2e_report_policy_failure(value),
        "current_gpu_render_report" => verify_render_report_policy_failure(value),
        "current_ui_report" => verify_ui_report_policy_failure(value),
        role if spec.family == "product_validation" && role.ends_with("_product_report") => {
            product_validation_report_policy_failure(value)
        }
        _ => None,
    }
}

fn legacy_evidence_entry(root: &Path, spec: &LegacyEvidencePath) -> Value {
    let path = root.join(spec.relative_path);
    let present = path.exists();
    json!({
        "path": display_target_relative(spec.relative_path),
        "family": spec.family,
        "evidence_role": "legacy_root_artifact",
        "classification": "stale_legacy_evidence",
        "presence": if present { "present" } else { "missing" },
        "audit_status": if present { "stale_legacy_present" } else { "legacy_absent" },
        "blocking": present,
        "quarantine_reason": spec.quarantine_reason,
    })
}

fn discovered_evidence_entry(root: &Path, relative_path: &str) -> Value {
    let path = root.join(relative_path);
    let extension = path.extension().and_then(|value| value.to_str());
    if extension == Some("log") {
        return discovered_log_entry(relative_path);
    }
    if extension == Some("md") {
        return json!({
            "path": display_target_relative(relative_path),
            "family": discovered_family(relative_path),
            "evidence_role": "discovered_markdown_artifact",
            "classification": "quarantined_discovered_evidence",
            "presence": "present",
            "audit_status": "quarantined_discovered_markdown",
            "blocking": false,
            "quarantine_reason": "not_a_current_exact_report_path",
        });
    }

    match read_json_file(&path) {
        Ok(value) => discovered_json_entry(relative_path, &value),
        Err(err) => json!({
            "path": display_target_relative(relative_path),
            "family": discovered_family(relative_path),
            "evidence_role": "discovered_json_artifact",
            "classification": "quarantined_discovered_evidence",
            "presence": "present",
            "audit_status": "quarantined_unreadable_json",
            "blocking": false,
            "failure_reason": err.to_string(),
            "quarantine_reason": "not_a_current_exact_report_path",
        }),
    }
}

fn discovered_json_entry(relative_path: &str, value: &Value) -> Value {
    let schema = value.get("schema").and_then(Value::as_str);
    let command = value.get("command").and_then(Value::as_str);
    let benchmark = value.get("benchmark").and_then(Value::as_str);
    let (evidence_role, audit_status, quarantine_reason) = if relative_path
        .starts_with("benchmarks/")
        && relative_path
            .rsplit('/')
            .next()
            .is_some_and(|file| file.starts_with("app-smoke-"))
    {
        (
            "smoke_only_report",
            "quarantined_smoke_only_report",
            "app_smoke_is_supporting_evidence_only",
        )
    } else if let Some(benchmark) = benchmark {
        if CURRENT_BENCHMARK_NAMES.contains(&benchmark) {
            (
                "benchmark_supporting_report",
                "quarantined_current_benchmark_report",
                "benchmarks_are_not_product_validation",
            )
        } else {
            (
                "historical_or_unregistered_benchmark_report",
                "quarantined_historical_benchmark_report",
                "benchmark_name_is_not_in_current_command_surface",
            )
        }
    } else if schema == Some("mirante4d-product-automation-report") {
        (
            "product_automation_app_report",
            "quarantined_optional_product_automation_report",
            "optional_app_report_must_be_read_through_wrapper_report",
        )
    } else if schema == Some("mirante4d-product-automation-script") {
        (
            "product_automation_script",
            "quarantined_discovered_product_automation_script",
            "script_is_not_a_validation_report",
        )
    } else if command.is_some() || schema.is_some() {
        (
            "unregistered_report_like_json",
            "quarantined_unregistered_report_like_json",
            "not_a_current_exact_report_path",
        )
    } else {
        (
            "unregistered_json_artifact",
            "quarantined_unregistered_json",
            "not_a_current_exact_report_path",
        )
    };

    json!({
        "path": display_target_relative(relative_path),
        "family": discovered_family(relative_path),
        "evidence_role": evidence_role,
        "classification": "quarantined_discovered_evidence",
        "presence": "present",
        "audit_status": audit_status,
        "blocking": false,
        "actual_schema": schema,
        "actual_command": command,
        "benchmark": benchmark,
        "evidence_status": value.get("status").and_then(Value::as_str),
        "quarantine_reason": quarantine_reason,
    })
}

fn discovered_log_entry(relative_path: &str) -> Value {
    let (evidence_role, audit_status, quarantine_reason) = if relative_path
        .starts_with("product-open")
        || relative_path.starts_with("product-open/")
        || relative_path.starts_with("investigation/")
    {
        (
            "historical_manual_product_log",
            "quarantined_historical_product_log",
            "manual_or_investigation_logs_are_not_current_product_validation_reports",
        )
    } else if relative_path.contains("app-smoke") {
        (
            "smoke_only_log",
            "quarantined_smoke_only_log",
            "app_smoke_is_supporting_evidence_only",
        )
    } else {
        (
            "unregistered_log_artifact",
            "quarantined_unregistered_log",
            "not_a_current_exact_report_path",
        )
    };

    json!({
        "path": display_target_relative(relative_path),
        "family": discovered_family(relative_path),
        "evidence_role": evidence_role,
        "classification": "quarantined_discovered_evidence",
        "presence": "present",
        "audit_status": audit_status,
        "blocking": false,
        "quarantine_reason": quarantine_reason,
    })
}

fn report_audit_markdown(report: &Value) -> String {
    let mut markdown = String::from("# Mirante4D Report Audit\n\n");
    if let Some(readiness) = report.get("completion_readiness") {
        markdown.push_str(&format!(
            "- completion readiness: `{}`\n",
            readiness["status"].as_str().unwrap_or("unknown")
        ));
        markdown.push_str(&format!(
            "- completion blockers: `{}`\n\n",
            readiness["blocking_count"].as_u64().unwrap_or(0)
        ));
        if let Some(blockers) = readiness["blockers"].as_array() {
            for blocker in blockers {
                append_completion_blocker_markdown(&mut markdown, blocker);
            }
            if !blockers.is_empty() {
                markdown.push('\n');
            }
        }
    }

    markdown.push_str(
        "| Path | Classification | Evidence Status | Audit Status | Blocking |\n\
         |---|---|---|---|---|\n",
    );
    if let Some(entries) = report["entries"].as_array() {
        for entry in entries {
            markdown.push_str(&format!(
                "| `{}` | `{}` | `{}` | `{}` | `{}` |\n",
                entry["path"].as_str().unwrap_or(""),
                entry["classification"].as_str().unwrap_or(""),
                entry["evidence_status"].as_str().unwrap_or(""),
                entry["audit_status"].as_str().unwrap_or(""),
                entry["blocking"].as_bool().unwrap_or(false),
            ));
        }
    }
    markdown
}

fn append_completion_blocker_markdown(markdown: &mut String, blocker: &Value) {
    markdown.push_str(&format!(
        "  - `{}`",
        blocker["code"].as_str().unwrap_or("unknown")
    ));
    if let Some(scenario) = blocker.get("scenario").and_then(Value::as_str) {
        markdown.push_str(&format!(" (`{scenario}`)"));
    }
    markdown.push_str(&format!(
        ": {}\n",
        blocker["reason"].as_str().unwrap_or("inspect blocker")
    ));
    if let Some(required_action) = blocker.get("required_action").and_then(Value::as_str) {
        markdown.push_str(&format!("    - action: {required_action}\n"));
    }
    if let Some(plan) = blocker.get("baseline_refresh_plan") {
        append_baseline_refresh_blocker_markdown(markdown, plan);
    }
    if let Some(plan) = blocker.get("external_ci_completion_plan") {
        append_external_ci_blocker_markdown(markdown, plan);
    }
    if let Some(plan) = blocker.get("t5_qual_001_product_validation_completion_plan") {
        append_t5_qual_001_product_validation_blocker_markdown(markdown, plan);
    }
}

fn append_baseline_refresh_blocker_markdown(markdown: &mut String, plan: &Value) {
    let summary = plan.get("summary").unwrap_or(&Value::Null);
    let remediation = summary.get("remediation").unwrap_or(&Value::Null);
    markdown.push_str(&format!(
        "    - baseline plan: status=`{}`, stale=`{}`, candidates=`{}`, ready=`{}`, clean-reruns=`{}`, rerun-commands=`{}`\n",
        plan["status"].as_str().unwrap_or("unknown"),
        summary["refresh_baseline_count"].as_u64().unwrap_or(0),
        summary["candidate_report_count"].as_u64().unwrap_or(0),
        summary["ready_count"].as_u64().unwrap_or(0),
        remediation["clean_rerun_required_count"].as_u64().unwrap_or(0),
        remediation["rerun_command_available_count"].as_u64().unwrap_or(0),
    ));
    if let Some(next_action) = remediation.get("next_action").and_then(Value::as_str) {
        markdown.push_str(&format!("    - baseline next: {next_action}\n"));
    }
    if let Some(script) = plan
        .get("generated_clean_rerun_script")
        .and_then(Value::as_str)
    {
        markdown.push_str(&format!("    - baseline clean-rerun script: `{script}`\n"));
    }
    if let Some(artifact) = plan.get("generated_clean_rerun_script_artifact") {
        markdown.push_str(&format!(
            "    - baseline clean-rerun artifact: status=`{}`, commands=`{}`, sections=`{}`, executable=`{}`\n",
            artifact["status"].as_str().unwrap_or("unknown"),
            artifact["rerun_command_count"].as_u64().unwrap_or(0),
            artifact["baseline_section_count"].as_u64().unwrap_or(0),
            artifact["executable"].as_bool().unwrap_or(false),
        ));
    }
}

fn append_external_ci_blocker_markdown(markdown: &mut String, plan: &Value) {
    markdown.push_str(&format!(
        "    - external CI plan: required surfaces=`{}`, final evidence=`{}`\n",
        plan["required_surface_count"].as_u64().unwrap_or(0),
        plan["final_evidence_path"].as_str().unwrap_or("unknown"),
    ));
    if let Some(surfaces) = plan.get("surfaces").and_then(Value::as_array) {
        for surface in surfaces {
            markdown.push_str(&format!(
                "    - CI surface `{}`: workflow=`{}`, artifact=`{}`, checks=`{}`\n",
                surface["name"].as_str().unwrap_or("unknown"),
                surface["workflow_path"].as_str().unwrap_or("unknown"),
                surface["artifact_name"].as_str().unwrap_or("unknown"),
                surface["required_check_count"].as_u64().unwrap_or(0),
            ));
        }
    }
    if let Some(command) = plan
        .pointer("/finalizer/merge_command")
        .and_then(Value::as_str)
    {
        markdown.push_str(&format!("    - CI merge: `{command}`\n"));
    }
}

fn append_t5_qual_001_product_validation_blocker_markdown(markdown: &mut String, plan: &Value) {
    let current = plan.get("current_evidence").unwrap_or(&Value::Null);
    markdown.push_str(&format!(
        "    - T5Qual001 plan: scenario=`{}`, dataset=`{}`, preflight=`{}`, timeout=`{}s`, rss-limit=`{}`\n",
        plan["scenario"].as_str().unwrap_or("unknown"),
        current["dataset_id"].as_str().unwrap_or("unknown"),
        current["preflight_only"].as_bool().unwrap_or(false),
        plan["default_timeout_secs"].as_u64().unwrap_or(0),
        plan["default_process_rss_limit_bytes"].as_u64().unwrap_or(0),
    ));
    if let Some(command) = plan
        .pointer("/product_open_command/shell_command")
        .and_then(Value::as_str)
    {
        markdown.push_str(&format!("    - T5Qual001 product-open: `{command}`\n"));
    }
}

fn discover_auditable_relative_paths(root: &Path) -> anyhow::Result<Vec<String>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    collect_auditable_relative_paths(root, root, 0, &mut paths)?;
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn collect_auditable_relative_paths(
    root: &Path,
    current: &Path,
    depth: usize,
    paths: &mut Vec<String>,
) -> anyhow::Result<()> {
    if depth > 6 {
        return Ok(());
    }
    for entry in
        fs::read_dir(current).with_context(|| format!("failed to read {}", current.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read entry in {}", current.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if file_type.is_dir() {
            collect_auditable_relative_paths(root, &path, depth + 1, paths)?;
        } else if file_type.is_file()
            && is_auditable_extension(path.extension().and_then(|value| value.to_str()))
        {
            let relative = path
                .strip_prefix(root)
                .with_context(|| {
                    format!(
                        "failed to strip target root {} from {}",
                        root.display(),
                        path.display()
                    )
                })?
                .to_string_lossy()
                .replace('\\', "/");
            paths.push(relative);
        }
    }
    Ok(())
}

fn is_auditable_extension(extension: Option<&str>) -> bool {
    matches!(extension, Some("json" | "md" | "log"))
}

fn should_skip_discovered(relative_path: &str) -> bool {
    relative_path.starts_with("fixtures/")
}

fn discovered_family(relative_path: &str) -> &'static str {
    if relative_path.starts_with("benchmarks/") {
        "benchmark"
    } else if relative_path.starts_with("product-validation/") {
        "product_validation"
    } else if relative_path.starts_with("verify-") {
        "verify"
    } else if relative_path.starts_with("command-audit/") {
        "command_surface"
    } else if relative_path.starts_with("workflow-audit/")
        || relative_path.starts_with("external-ci/")
    {
        "ci_surface"
    } else if relative_path.starts_with("completion-waivers/") {
        "completion_readiness"
    } else if relative_path.starts_with("report-audit/") {
        "report_surface"
    } else if relative_path.starts_with("phase") {
        "phase_audit"
    } else if relative_path.starts_with("product-open")
        || relative_path.starts_with("product-open/")
        || relative_path.starts_with("investigation/")
    {
        "historical_manual_product_open"
    } else {
        "unknown"
    }
}

fn display_target_relative(relative_path: &str) -> String {
    format!("{TARGET_ROOT}/{relative_path}")
}

fn display_path(path: &str) -> String {
    path.replace('\\', "/")
}

#[cfg(test)]
mod tests;

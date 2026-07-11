use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::{
    baseline_audit::{ACCEPTED_BASELINE_CLASSES, ACCEPTED_BASELINE_STATUSES, COMPATIBILITY_FIELDS},
    command_audit::COMMAND_AUDIT_ENTRIES,
};

const BASELINE_REFRESH_CLEAN_RERUN_SCRIPT: &str =
    "target/mirante4d/baseline-refresh/baseline-clean-reruns.sh";

pub(super) const EXPECTED_WORKFLOW_AUDIT_ENTRIES: &[(&str, &str)] = &[
    (".github/workflows/ci.yml", "hosted_cpu_ci_evidence_surface"),
    (
        ".github/workflows/platform-ci.yml",
        "manual_scheduled_platform_cpu_ci_surface",
    ),
    (
        ".github/workflows/gpu-render.yml",
        "manual_self_hosted_gpu_render_ci_surface",
    ),
    (
        ".github/workflows/external-ci-evidence.yml",
        "manual_external_ci_evidence_finalizer",
    ),
];

pub(super) fn external_ci_evidence_report_policy_failure(value: &Value) -> Option<String> {
    if value.get("status").and_then(Value::as_str) != Some("passed") {
        return Some("external CI evidence report missing passed status".to_owned());
    }
    if !matches!(
        value.get("evidence_source").and_then(Value::as_str),
        Some("operator_supplied_external_run_metadata" | "merged_ci_surface_reports")
    ) {
        return Some("external CI evidence report has unsupported evidence_source".to_owned());
    }
    if value
        .pointer("/summary/external_run_evidence")
        .and_then(Value::as_str)
        != Some("checked_by_external_ci")
    {
        return Some(
            "external CI evidence report must state that external CI runs were checked".to_owned(),
        );
    }

    let Some(surfaces) = value.get("surfaces").and_then(Value::as_array) else {
        return Some("external CI evidence report missing surfaces inventory".to_owned());
    };
    if surfaces.len() != 2 {
        return Some("external CI evidence report must include exactly two CI surfaces".to_owned());
    }
    if value
        .pointer("/summary/required_surface_count")
        .and_then(Value::as_u64)
        != Some(2)
    {
        return Some(
            "external CI evidence report summary required_surface_count is stale".to_owned(),
        );
    }

    let passed_count = surfaces
        .iter()
        .filter(|surface| surface.get("status").and_then(Value::as_str) == Some("passed"))
        .count() as u64;
    if value
        .pointer("/summary/passed_surface_count")
        .and_then(Value::as_u64)
        != Some(passed_count)
    {
        return Some(
            "external CI evidence report summary passed_surface_count is stale".to_owned(),
        );
    }
    if value
        .pointer("/summary/blocking_count")
        .and_then(Value::as_u64)
        != Some(2 - passed_count)
    {
        return Some("external CI evidence report summary blocking_count is stale".to_owned());
    }

    let expected_git_sha = value.get("git_sha").and_then(Value::as_str);
    for (name, workflow_path, expected_checks) in [
        (
            "hosted_cpu_ci",
            ".github/workflows/ci.yml",
            &[
                "command-audit",
                "baseline-audit",
                "workflow-audit",
                "report-audit",
                "verify-fast",
                "verify-full",
                "package-linux-release",
                "bench-smoke",
            ][..],
        ),
        (
            "self_hosted_gpu_ci",
            ".github/workflows/gpu-render.yml",
            &["verify-render"][..],
        ),
    ] {
        let Some(surface) = surfaces
            .iter()
            .find(|surface| surface.get("name").and_then(Value::as_str) == Some(name))
        else {
            return Some(format!(
                "external CI evidence report missing surface {name:?}"
            ));
        };
        if surface.get("status").and_then(Value::as_str) != Some("passed") {
            return Some(format!(
                "external CI evidence report surface {name:?} is not passed"
            ));
        }
        if surface.get("workflow_path").and_then(Value::as_str) != Some(workflow_path) {
            return Some(format!(
                "external CI evidence report surface {name:?} has stale workflow_path"
            ));
        }
        for field in ["run_url", "run_id", "git_sha"] {
            if surface
                .get(field)
                .and_then(Value::as_str)
                .is_none_or(|value| value.trim().is_empty())
            {
                return Some(format!(
                    "external CI evidence report surface {name:?} missing {field}"
                ));
            }
        }
        if surface.get("git_sha").and_then(Value::as_str) != expected_git_sha {
            return Some(
                "external CI evidence report surfaces must match top-level git_sha".to_owned(),
            );
        }
        let Some(required_checks) = surface.get("required_checks").and_then(Value::as_array) else {
            return Some(format!(
                "external CI evidence report surface {name:?} missing required_checks"
            ));
        };
        if required_checks.is_empty() {
            return Some(format!(
                "external CI evidence report surface {name:?} has empty required_checks"
            ));
        }
        if required_checks
            .iter()
            .any(|check| check.as_str().is_none_or(|value| value.trim().is_empty()))
        {
            return Some(format!(
                "external CI evidence report surface {name:?} has malformed required_checks"
            ));
        }
        if surface.get("required_checks") != Some(&json!(expected_checks)) {
            return Some(format!(
                "external CI evidence report surface {name:?} has stale required_checks"
            ));
        }
        let Some(check_results) = surface.get("check_results").and_then(Value::as_array) else {
            return Some(format!(
                "external CI evidence report surface {name:?} missing check_results"
            ));
        };
        let check_names = check_results
            .iter()
            .map(|result| result.get("name").and_then(Value::as_str))
            .collect::<Option<Vec<_>>>();
        if check_names.as_deref() != Some(expected_checks) {
            return Some(format!(
                "external CI evidence report surface {name:?} has stale check_results names"
            ));
        }
        if check_results
            .iter()
            .any(|result| result.get("status").and_then(Value::as_str) != Some("passed"))
        {
            return Some(format!(
                "external CI evidence report surface {name:?} has non-passed check_results"
            ));
        }
        if surface
            .pointer("/check_summary/required_check_count")
            .and_then(Value::as_u64)
            != Some(expected_checks.len() as u64)
            || surface
                .pointer("/check_summary/passed_check_count")
                .and_then(Value::as_u64)
                != Some(expected_checks.len() as u64)
            || surface
                .pointer("/check_summary/blocking_count")
                .and_then(Value::as_u64)
                != Some(0)
        {
            return Some(format!(
                "external CI evidence report surface {name:?} has stale check_summary"
            ));
        }
    }

    None
}

pub(super) fn workflow_audit_report_policy_failure(value: &Value) -> Option<String> {
    if value.get("status").and_then(Value::as_str) != Some("passed") {
        return Some("workflow-audit report missing passed status".to_owned());
    }
    if value
        .pointer("/summary/external_run_evidence")
        .and_then(Value::as_str)
        != Some("not_checked_by_static_workflow_audit")
    {
        return Some(
            "workflow-audit report must state that external CI runs are not checked".to_owned(),
        );
    }

    let Some(entries) = value.get("entries").and_then(Value::as_array) else {
        return Some("workflow-audit report missing entries inventory".to_owned());
    };
    if entries.len() != EXPECTED_WORKFLOW_AUDIT_ENTRIES.len() {
        return Some("workflow-audit report workflow_count is stale".to_owned());
    }
    if value
        .pointer("/summary/workflow_count")
        .and_then(Value::as_u64)
        != Some(EXPECTED_WORKFLOW_AUDIT_ENTRIES.len() as u64)
    {
        return Some("workflow-audit report summary workflow_count is stale".to_owned());
    }

    let blocking_count = entries
        .iter()
        .filter(|entry| entry.get("blocking").and_then(Value::as_bool) == Some(true))
        .count() as u64;
    if value
        .pointer("/summary/blocking_count")
        .and_then(Value::as_u64)
        != Some(blocking_count)
    {
        return Some("workflow-audit report summary blocking_count is stale".to_owned());
    }
    if blocking_count > 0 {
        return Some("workflow-audit report contains blocking workflow policy gaps".to_owned());
    }

    let mut seen = BTreeSet::new();
    for entry in entries {
        let Some(path) = entry.get("path").and_then(Value::as_str) else {
            return Some("workflow-audit report entry missing path".to_owned());
        };
        if !seen.insert(path) {
            return Some(format!(
                "workflow-audit report contains duplicate path {path:?}"
            ));
        }
    }

    for (path, evidence_role) in EXPECTED_WORKFLOW_AUDIT_ENTRIES {
        let Some(entry) = entries
            .iter()
            .find(|entry| entry.get("path").and_then(Value::as_str) == Some(*path))
        else {
            return Some(format!("workflow-audit report missing workflow {path:?}"));
        };
        if entry.get("evidence_role").and_then(Value::as_str) != Some(*evidence_role) {
            return Some(format!(
                "workflow-audit report workflow {path:?} has stale evidence_role"
            ));
        }
        if entry.get("presence").and_then(Value::as_str) != Some("present") {
            return Some(format!(
                "workflow-audit report workflow {path:?} is not present"
            ));
        }
        if entry.get("audit_status").and_then(Value::as_str) != Some("workflow_policy_compliant") {
            return Some(format!(
                "workflow-audit report workflow {path:?} is not policy compliant"
            ));
        }
        if entry.get("blocking").and_then(Value::as_bool) != Some(false) {
            return Some(format!(
                "workflow-audit report workflow {path:?} has blocking policy gaps"
            ));
        }
        let Some(required) = entry.get("required").and_then(Value::as_array) else {
            return Some(format!(
                "workflow-audit report workflow {path:?} missing required checks"
            ));
        };
        if required.is_empty() {
            return Some(format!(
                "workflow-audit report workflow {path:?} has empty required checks"
            ));
        }
        if required
            .iter()
            .any(|check| check.get("ok").and_then(Value::as_bool) != Some(true))
        {
            return Some(format!(
                "workflow-audit report workflow {path:?} has failing required checks"
            ));
        }
        let Some(forbidden) = entry.get("forbidden").and_then(Value::as_array) else {
            return Some(format!(
                "workflow-audit report workflow {path:?} missing forbidden checks"
            ));
        };
        if forbidden.is_empty() {
            return Some(format!(
                "workflow-audit report workflow {path:?} has empty forbidden checks"
            ));
        }
        if forbidden
            .iter()
            .any(|check| check.get("ok").and_then(Value::as_bool) != Some(true))
        {
            return Some(format!(
                "workflow-audit report workflow {path:?} has forbidden CI content"
            ));
        }
    }

    None
}

pub(super) fn baseline_audit_report_policy_failure(value: &Value) -> Option<String> {
    let status = value.get("status").and_then(Value::as_str);
    if !matches!(status, Some("passed" | "needs_refresh")) {
        return Some("baseline-audit report status must be passed or needs_refresh".to_owned());
    }

    if value.pointer("/policy/accepted_baseline_classes") != Some(&json!(ACCEPTED_BASELINE_CLASSES))
    {
        return Some("baseline-audit report accepted baseline classes are stale".to_owned());
    }
    if value.pointer("/policy/accepted_baseline_statuses")
        != Some(&json!(ACCEPTED_BASELINE_STATUSES))
    {
        return Some("baseline-audit report accepted baseline statuses are stale".to_owned());
    }
    if value.pointer("/policy/compatibility_fields") != Some(&json!(COMPATIBILITY_FIELDS)) {
        return Some("baseline-audit report compatibility fields are stale".to_owned());
    }
    if value
        .pointer("/policy/readme_current_baseline_list_must_match_committed_json_files")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Some("baseline-audit report missing README baseline-list policy".to_owned());
    }

    let Some(entries) = value.get("entries").and_then(Value::as_array) else {
        return Some("baseline-audit report missing baseline entries".to_owned());
    };
    let Some(documentation) = value.get("documentation") else {
        return Some("baseline-audit report missing documentation policy result".to_owned());
    };
    let blocking_count = entries
        .iter()
        .filter(|entry| entry.get("blocking").and_then(Value::as_bool) == Some(true))
        .count() as u64
        + documentation
            .get("blocking_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
    let needs_refresh_count = entries
        .iter()
        .filter(|entry| entry.get("needs_refresh").and_then(Value::as_bool) == Some(true))
        .count() as u64;

    if documentation.get("blocking").and_then(Value::as_bool) != Some(false) {
        return Some("baseline-audit report contains README baseline-list mismatch".to_owned());
    }
    if documentation.get("audit_status").and_then(Value::as_str)
        != Some("readme_current_baseline_list_matches_files")
    {
        return Some("baseline-audit report documentation audit status is stale".to_owned());
    }

    if value
        .pointer("/summary/baseline_count")
        .and_then(Value::as_u64)
        != Some(entries.len() as u64)
    {
        return Some("baseline-audit report baseline_count is stale".to_owned());
    }
    if value
        .pointer("/summary/blocking_count")
        .and_then(Value::as_u64)
        != Some(blocking_count)
    {
        return Some("baseline-audit report blocking_count is stale".to_owned());
    }
    if value
        .pointer("/summary/needs_refresh_count")
        .and_then(Value::as_u64)
        != Some(needs_refresh_count)
    {
        return Some("baseline-audit report needs_refresh_count is stale".to_owned());
    }
    if blocking_count > 0 {
        return Some("baseline-audit report contains blocking baseline policy entries".to_owned());
    }
    if status == Some("passed") && needs_refresh_count > 0 {
        return Some(
            "baseline-audit report status passed despite refresh-needed baselines".to_owned(),
        );
    }
    if status == Some("needs_refresh") && needs_refresh_count == 0 {
        return Some(
            "baseline-audit report status needs_refresh without refresh-needed baselines"
                .to_owned(),
        );
    }

    for entry in entries {
        let Some(audit_status) = entry.get("audit_status").and_then(Value::as_str) else {
            return Some("baseline-audit report entry missing audit_status".to_owned());
        };
        if !matches!(
            audit_status,
            "current_policy_compliant"
                | "stale_timing_needs_refresh"
                | "legacy_missing_baseline_class"
        ) {
            return Some(format!(
                "baseline-audit report contains unacceptable audit_status {audit_status:?}"
            ));
        }
        if entry.get("policy_action").and_then(Value::as_str).is_none() {
            return Some("baseline-audit report entry missing policy_action".to_owned());
        }
        if entry.get("compatibility_fields").is_none() {
            return Some("baseline-audit report entry missing compatibility fields".to_owned());
        }
        let baseline_class = entry.get("baseline_class").and_then(Value::as_str);
        if baseline_class.is_some_and(|class| !ACCEPTED_BASELINE_CLASSES.contains(&class)) {
            return Some("baseline-audit report entry has unknown baseline_class".to_owned());
        }
        let baseline_status = entry.get("baseline_status").and_then(Value::as_str);
        if baseline_status.is_some_and(|status| !ACCEPTED_BASELINE_STATUSES.contains(&status)) {
            return Some("baseline-audit report entry has unknown baseline_status".to_owned());
        }
    }

    None
}

pub(super) fn baseline_refresh_plan_report_policy_failure(value: &Value) -> Option<String> {
    let status = value.get("status").and_then(Value::as_str);
    if !matches!(status, Some("ready" | "not_ready" | "nothing_to_refresh")) {
        return Some(
            "baseline-refresh-plan report status must be ready, not_ready, or nothing_to_refresh"
                .to_owned(),
        );
    }
    let Some(entries) = value.get("entries").and_then(Value::as_array) else {
        return Some("baseline-refresh-plan report missing entries".to_owned());
    };
    let ready_count = entries
        .iter()
        .filter(|entry| entry.get("status").and_then(Value::as_str) == Some("ready"))
        .count() as u64;
    let not_ready_count = entries.len() as u64 - ready_count;
    if value
        .pointer("/summary/refresh_baseline_count")
        .and_then(Value::as_u64)
        != Some(entries.len() as u64)
    {
        return Some("baseline-refresh-plan report refresh count is stale".to_owned());
    }
    if value
        .pointer("/summary/ready_count")
        .and_then(Value::as_u64)
        != Some(ready_count)
    {
        return Some("baseline-refresh-plan report ready count is stale".to_owned());
    }
    if value
        .pointer("/summary/not_ready_count")
        .and_then(Value::as_u64)
        != Some(not_ready_count)
    {
        return Some("baseline-refresh-plan report not-ready count is stale".to_owned());
    }
    if value
        .pointer("/summary/writes_curated_baselines")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Some("baseline-refresh-plan report must be non-mutating".to_owned());
    }
    let Some(summary_remediation) = value.pointer("/summary/remediation") else {
        return Some("baseline-refresh-plan report missing summary remediation".to_owned());
    };
    let clean_rerun_required_count = entries
        .iter()
        .filter(|entry| {
            entry
                .pointer("/remediation/requires_clean_rerun")
                .and_then(Value::as_bool)
                == Some(true)
        })
        .count() as u64;
    if summary_remediation
        .get("clean_rerun_required_count")
        .and_then(Value::as_u64)
        != Some(clean_rerun_required_count)
    {
        return Some("baseline-refresh-plan remediation clean-rerun count is stale".to_owned());
    }
    for (field, entry_status) in [
        ("no_matching_candidate_count", "no_matching_candidate"),
        (
            "not_promotable_candidate_count",
            "matched_candidates_not_promotable",
        ),
        ("ambiguous_candidate_count", "ambiguous_eligible_candidates"),
        (
            "duplicate_source_entry_count",
            "duplicate_selected_source_report",
        ),
    ] {
        let count = entries
            .iter()
            .filter(|entry| entry.get("status").and_then(Value::as_str) == Some(entry_status))
            .count() as u64;
        if summary_remediation.get(field).and_then(Value::as_u64) != Some(count) {
            return Some(format!(
                "baseline-refresh-plan remediation {field} is stale"
            ));
        }
    }
    let rerun_command_available_count = entries
        .iter()
        .filter(|entry| entry.pointer("/rerun/available").and_then(Value::as_bool) == Some(true))
        .count() as u64;
    let clean_rerun_command_available_count = entries
        .iter()
        .filter(|entry| {
            entry
                .pointer("/remediation/requires_clean_rerun")
                .and_then(Value::as_bool)
                == Some(true)
                && entry.pointer("/rerun/available").and_then(Value::as_bool) == Some(true)
        })
        .count() as u64;
    if summary_remediation
        .get("rerun_command_available_count")
        .and_then(Value::as_u64)
        != Some(rerun_command_available_count)
    {
        return Some(
            "baseline-refresh-plan remediation rerun-command available count is stale".to_owned(),
        );
    }
    if summary_remediation
        .get("rerun_command_unavailable_count")
        .and_then(Value::as_u64)
        != Some(entries.len() as u64 - rerun_command_available_count)
    {
        return Some(
            "baseline-refresh-plan remediation rerun-command unavailable count is stale".to_owned(),
        );
    }
    if summary_remediation
        .get("next_action")
        .and_then(Value::as_str)
        .is_none_or(|action| action.trim().is_empty())
    {
        return Some("baseline-refresh-plan remediation missing next_action".to_owned());
    }
    for field in [
        "source_worktree_required_clean",
        "source_report_dirty_worktree_required_false",
        "current_worktree_must_be_clean_for_promotion",
    ] {
        if summary_remediation.get(field).and_then(Value::as_bool) != Some(true) {
            return Some(format!(
                "baseline-refresh-plan remediation missing true {field}"
            ));
        }
    }
    if status == Some("ready") && not_ready_count != 0 {
        return Some("baseline-refresh-plan ready status has not-ready entries".to_owned());
    }
    if status == Some("not_ready") && not_ready_count == 0 {
        return Some("baseline-refresh-plan not_ready status has no blockers".to_owned());
    }
    if status == Some("nothing_to_refresh") && !entries.is_empty() {
        return Some("baseline-refresh-plan nothing_to_refresh status has entries".to_owned());
    }
    let manifest = value.get("generated_promotion_manifest");
    if status == Some("ready") {
        if manifest.and_then(Value::as_str).is_none() {
            return Some(
                "baseline-refresh-plan ready report missing promotion manifest".to_owned(),
            );
        }
        if value
            .pointer("/summary/promotion_command")
            .and_then(Value::as_str)
            .is_none()
        {
            return Some("baseline-refresh-plan ready report missing promotion command".to_owned());
        }
    } else if !manifest.unwrap_or(&Value::Null).is_null() {
        return Some(
            "baseline-refresh-plan non-ready report must not expose a promotion manifest"
                .to_owned(),
        );
    }
    let clean_rerun_script = value
        .get("generated_clean_rerun_script")
        .unwrap_or(&Value::Null);
    if clean_rerun_required_count > 0
        && clean_rerun_command_available_count == clean_rerun_required_count
    {
        if clean_rerun_script.as_str() != Some(BASELINE_REFRESH_CLEAN_RERUN_SCRIPT) {
            return Some(
                "baseline-refresh-plan with complete clean-rerun coverage missing generated clean-rerun script"
                    .to_owned(),
            );
        }
    } else if !clean_rerun_script.is_null() {
        return Some(
            "baseline-refresh-plan must not expose a clean-rerun script when clean reruns are incomplete or unnecessary"
                .to_owned(),
        );
    }

    for entry in entries {
        let Some(entry_status) = entry.get("status").and_then(Value::as_str) else {
            return Some("baseline-refresh-plan entry missing status".to_owned());
        };
        if !matches!(
            entry_status,
            "ready"
                | "no_matching_candidate"
                | "matched_candidates_not_promotable"
                | "ambiguous_eligible_candidates"
                | "duplicate_selected_source_report"
        ) {
            return Some(format!(
                "baseline-refresh-plan entry has unknown status {entry_status:?}"
            ));
        }
        if entry.get("baseline_name").and_then(Value::as_str).is_none() {
            return Some("baseline-refresh-plan entry missing baseline_name".to_owned());
        }
        if entry.get("signature").is_none() {
            return Some("baseline-refresh-plan entry missing signature".to_owned());
        }
        let matched = entry
            .get("matched_candidates")
            .and_then(Value::as_array)
            .map_or(0, Vec::len) as u64;
        if entry.get("matched_candidate_count").and_then(Value::as_u64) != Some(matched) {
            return Some("baseline-refresh-plan entry matched count is stale".to_owned());
        }
        let Some(rerun) = entry.get("rerun") else {
            return Some("baseline-refresh-plan entry missing rerun plan".to_owned());
        };
        let Some(rerun_available) = rerun.get("available").and_then(Value::as_bool) else {
            return Some("baseline-refresh-plan entry rerun plan missing availability".to_owned());
        };
        if rerun.get("release_build_required").and_then(Value::as_bool) != Some(true) {
            return Some(
                "baseline-refresh-plan entry rerun plan must require release build".to_owned(),
            );
        }
        if rerun_available {
            if let Some(reason) = rerun.get("unavailable_reason")
                && !reason.is_null()
            {
                return Some(
                    "baseline-refresh-plan available rerun plan has unavailable_reason".to_owned(),
                );
            }
            let Some(primary_command) = rerun.get("primary_command") else {
                return Some(
                    "baseline-refresh-plan available rerun plan missing primary command".to_owned(),
                );
            };
            if let Some(failure) = baseline_refresh_rerun_command_policy_failure(primary_command) {
                return Some(failure);
            }
        } else if rerun
            .get("unavailable_reason")
            .and_then(Value::as_str)
            .is_none_or(|reason| reason.trim().is_empty())
        {
            return Some("baseline-refresh-plan unavailable rerun plan missing reason".to_owned());
        }
        let Some(prerequisites) = rerun.get("prerequisite_commands").and_then(Value::as_array)
        else {
            return Some(
                "baseline-refresh-plan entry rerun plan missing prerequisite_commands".to_owned(),
            );
        };
        for prerequisite in prerequisites {
            if let Some(failure) = baseline_refresh_rerun_command_policy_failure(prerequisite) {
                return Some(failure);
            }
        }
        let Some(remediation) = entry.get("remediation") else {
            return Some("baseline-refresh-plan entry missing remediation".to_owned());
        };
        if remediation
            .get("action")
            .and_then(Value::as_str)
            .is_none_or(|action| action.trim().is_empty())
        {
            return Some("baseline-refresh-plan entry remediation missing action".to_owned());
        }
        if remediation
            .get("requires_clean_rerun")
            .and_then(Value::as_bool)
            .is_none()
        {
            return Some(
                "baseline-refresh-plan entry remediation missing requires_clean_rerun".to_owned(),
            );
        }
        let candidate_error_count = entry
            .get("matched_candidates")
            .and_then(Value::as_array)
            .map(|candidates| {
                candidates
                    .iter()
                    .filter(|candidate| {
                        candidate
                            .get("promotion_error")
                            .and_then(Value::as_str)
                            .is_some()
                    })
                    .count()
            })
            .unwrap_or(0) as u64;
        let Some(candidate_errors) = remediation
            .get("candidate_errors")
            .and_then(Value::as_array)
        else {
            return Some(
                "baseline-refresh-plan entry remediation missing candidate_errors".to_owned(),
            );
        };
        if remediation
            .get("candidate_error_count")
            .and_then(Value::as_u64)
            != Some(candidate_errors.len() as u64)
        {
            return Some(
                "baseline-refresh-plan entry remediation candidate_error_count is stale".to_owned(),
            );
        }
        if candidate_errors.len() as u64 != candidate_error_count {
            return Some(
                "baseline-refresh-plan entry remediation candidate_errors are stale".to_owned(),
            );
        }
        if entry_status == "ready"
            && entry
                .get("selected_source_report")
                .and_then(Value::as_str)
                .is_none()
        {
            return Some("baseline-refresh-plan ready entry missing selected source".to_owned());
        }
        if entry_status != "ready"
            && !entry
                .get("selected_source_report")
                .unwrap_or(&Value::Null)
                .is_null()
        {
            return Some(
                "baseline-refresh-plan non-ready entry must not select a source".to_owned(),
            );
        }
    }

    None
}

fn baseline_refresh_rerun_command_policy_failure(command: &Value) -> Option<String> {
    let shell_command = command.get("shell_command").and_then(Value::as_str);
    if shell_command.is_none_or(|command| command.trim().is_empty()) {
        return Some("baseline-refresh-plan rerun command missing shell_command".to_owned());
    }
    let Some(env) = command.get("env").and_then(Value::as_object) else {
        return Some("baseline-refresh-plan rerun command missing env object".to_owned());
    };
    let Some(argv) = command.get("argv").and_then(Value::as_array) else {
        return Some("baseline-refresh-plan rerun command missing argv".to_owned());
    };
    let prefix = ["cargo", "run", "--release", "-p", "xtask", "--"];
    if argv.len() < prefix.len() + 1 {
        return Some("baseline-refresh-plan rerun command argv is too short".to_owned());
    }
    for (index, expected) in prefix.iter().enumerate() {
        if argv.get(index).and_then(Value::as_str) != Some(*expected) {
            return Some(
                "baseline-refresh-plan rerun command must use cargo run --release -p xtask --"
                    .to_owned(),
            );
        }
    }
    if argv
        .iter()
        .any(|arg| arg.as_str().is_none_or(|arg| arg.trim().is_empty()))
    {
        return Some("baseline-refresh-plan rerun command argv contains empty item".to_owned());
    }
    let benchmark = argv
        .get(prefix.len())
        .and_then(Value::as_str)
        .unwrap_or_default();
    if command_requires_heavy_opt_in(benchmark) {
        if env
            .get("MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK")
            .and_then(Value::as_str)
            != Some("1")
        {
            return Some(
                "baseline-refresh-plan heavy rerun command missing MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1"
                    .to_owned(),
            );
        }
        if !shell_command
            .unwrap_or_default()
            .contains("MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1")
        {
            return Some(
                "baseline-refresh-plan heavy rerun shell_command missing MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1"
                    .to_owned(),
            );
        }
    }
    None
}

fn command_requires_heavy_opt_in(command: &str) -> bool {
    COMMAND_AUDIT_ENTRIES
        .iter()
        .any(|entry| entry.command == command && entry.requires_heavy_opt_in)
}

pub(super) fn command_audit_report_policy_failure(value: &Value) -> Option<String> {
    if value.get("status").and_then(Value::as_str) != Some("passed") {
        return Some("command-audit report missing passed status".to_owned());
    }

    let Some(entries) = value.get("entries").and_then(Value::as_array) else {
        return Some("command-audit report missing entries inventory".to_owned());
    };
    if entries.len() != COMMAND_AUDIT_ENTRIES.len() {
        return Some(format!(
            "command-audit report entry count {} does not match current command surface {}",
            entries.len(),
            COMMAND_AUDIT_ENTRIES.len()
        ));
    }
    if value
        .pointer("/summary/command_count")
        .and_then(Value::as_u64)
        != Some(COMMAND_AUDIT_ENTRIES.len() as u64)
    {
        return Some("command-audit report summary command_count is stale".to_owned());
    }

    let expected_heavy = COMMAND_AUDIT_ENTRIES
        .iter()
        .filter(|entry| entry.requires_heavy_opt_in)
        .count() as u64;
    if value
        .pointer("/summary/heavy_opt_in_count")
        .and_then(Value::as_u64)
        != Some(expected_heavy)
    {
        return Some("command-audit report summary heavy_opt_in_count is stale".to_owned());
    }

    let expected_product = COMMAND_AUDIT_ENTRIES
        .iter()
        .filter(|entry| entry.evidence_class == "product_automation_validation")
        .count() as u64;
    if value
        .pointer("/summary/product_validation_count")
        .and_then(Value::as_u64)
        != Some(expected_product)
    {
        return Some("command-audit report summary product_validation_count is stale".to_owned());
    }

    let mut seen = BTreeSet::new();
    for entry in entries {
        let Some(command) = entry.get("command").and_then(Value::as_str) else {
            return Some("command-audit report entry missing command".to_owned());
        };
        if !seen.insert(command) {
            return Some(format!(
                "command-audit report contains duplicate command {command:?}"
            ));
        }
    }

    for expected in COMMAND_AUDIT_ENTRIES {
        let Some(actual) = entries
            .iter()
            .find(|entry| entry.get("command").and_then(Value::as_str) == Some(expected.command))
        else {
            return Some(format!(
                "command-audit report missing command {:?}",
                expected.command
            ));
        };

        for (field, expected_value) in [
            ("family", expected.family),
            ("evidence_class", expected.evidence_class),
            ("default_safety", expected.default_safety),
            ("product_evidence_role", expected.product_evidence_role),
            ("stale_or_unsafe_status", expected.stale_or_unsafe_status),
        ] {
            if actual.get(field).and_then(Value::as_str) != Some(expected_value) {
                return Some(format!(
                    "command-audit report command {:?} has stale {field}",
                    expected.command
                ));
            }
        }
        if actual.get("requires_heavy_opt_in").and_then(Value::as_bool)
            != Some(expected.requires_heavy_opt_in)
        {
            return Some(format!(
                "command-audit report command {:?} has stale requires_heavy_opt_in",
                expected.command
            ));
        }
        if actual.get("report_paths") != Some(&json!(expected.report_paths)) {
            return Some(format!(
                "command-audit report command {:?} has stale report_paths",
                expected.command
            ));
        }
    }

    None
}

pub(super) fn product_validation_report_policy_failure(value: &Value) -> Option<String> {
    if matches!(
        product_validation_scenario_name(value).as_deref(),
        Some("t5_qual_001_interaction_mip" | "t5_qual_001_interaction_render_modes")
    ) && value
        .pointer("/limits/process_rss_limit_bytes")
        .and_then(Value::as_u64)
        .is_none()
    {
        return Some("T5Qual001 product-validation report missing process RSS limit".to_owned());
    }
    if value
        .pointer("/logs/stdout")
        .and_then(Value::as_str)
        .is_none()
        || value
            .pointer("/logs/stderr")
            .and_then(Value::as_str)
            .is_none()
    {
        return Some("product-validation report missing top-level stdout/stderr logs".to_owned());
    }
    match value.get("status").and_then(Value::as_str) {
        Some("passed") => product_validation_passed_report_policy_failure(value),
        Some("unsupported") => product_validation_unsupported_report_policy_failure(value),
        _ => None,
    }
}

fn product_validation_unsupported_report_policy_failure(value: &Value) -> Option<String> {
    if value
        .get("failure_reason")
        .and_then(Value::as_str)
        .is_none()
    {
        return Some("unsupported product-validation report missing failure_reason".to_owned());
    }
    if value
        .pointer("/environment/display_class")
        .and_then(Value::as_str)
        != Some("unsupported")
    {
        return Some(
            "unsupported product-validation report missing unsupported display_class".to_owned(),
        );
    }
    if value
        .pointer("/environment/display_class_source")
        .and_then(Value::as_str)
        .is_none()
    {
        return Some(
            "unsupported product-validation report missing display_class_source".to_owned(),
        );
    }
    if value
        .pointer("/dataset/manifest_status")
        .and_then(Value::as_str)
        != Some("loaded")
    {
        return Some(
            "unsupported product-validation report missing loaded dataset context".to_owned(),
        );
    }
    if value
        .pointer("/scenario/automation_script")
        .and_then(Value::as_str)
        .is_none()
    {
        return Some(
            "unsupported product-validation report missing automation script path".to_owned(),
        );
    }

    None
}

fn product_validation_passed_report_policy_failure(value: &Value) -> Option<String> {
    let display_class = value
        .pointer("/environment/display_class")
        .and_then(Value::as_str);
    if !matches!(display_class, Some("real_display" | "virtual_display")) {
        return Some(
            "passed product-validation report missing real/virtual display_class".to_owned(),
        );
    }
    if value
        .pointer("/environment/display_class_source")
        .and_then(Value::as_str)
        .is_none()
    {
        return Some("passed product-validation report missing display_class_source".to_owned());
    }
    if value
        .pointer("/scenario/automation_status")
        .and_then(Value::as_str)
        != Some("passed")
    {
        return Some(
            "passed product-validation report missing passed automation_status".to_owned(),
        );
    }
    if value
        .pointer("/process/exit_success")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Some("passed product-validation report missing process success".to_owned());
    }
    if value
        .pointer("/dataset/manifest_status")
        .and_then(Value::as_str)
        != Some("loaded")
    {
        return Some("passed product-validation report missing loaded dataset context".to_owned());
    }

    for field in [
        "/metrics/app_update_timing_summary/sample_count",
        "/metrics/display_refresh_timing_summary/sample_count",
        "/metrics/input_to_present_timing_summary/sample_count",
    ] {
        if value.pointer(field).and_then(Value::as_u64).unwrap_or(0) == 0 {
            return Some(format!(
                "passed product-validation report missing positive timing summary field {field}"
            ));
        }
    }
    if value
        .pointer("/presentation_timing/status")
        .and_then(Value::as_str)
        != Some("app_proxy_available_os_compositor_timestamp_unavailable")
    {
        return Some(
            "passed product-validation report missing explicit presentation timing status"
                .to_owned(),
        );
    }
    if value
        .pointer("/presentation_timing/os_compositor_present_timestamp/status")
        .and_then(Value::as_str)
        != Some("unsupported_by_current_eframe_wgpu_integration")
    {
        return Some(
            "passed product-validation report missing OS compositor timestamp status".to_owned(),
        );
    }
    if value
        .pointer("/metrics/presentation_timing/available_measurements/input_to_present_proxy/sample_summary_field")
        .and_then(Value::as_str)
        != Some("input_to_present_timing_summary")
    {
        return Some(
            "passed product-validation report missing input-to-present proxy presentation mapping"
                .to_owned(),
        );
    }

    for field in [
        "/limits/decoded_byte_limit_enforced",
        "/limits/gpu_upload_byte_limit_enforced",
        "/limits/gpu_resident_byte_limit_enforced",
    ] {
        if value.pointer(field).and_then(Value::as_bool) != Some(true) {
            return Some(format!(
                "passed product-validation report missing enforced resource limit {field}"
            ));
        }
    }

    let Some(artifacts) = value
        .pointer("/artifacts/automation_artifacts")
        .and_then(Value::as_array)
    else {
        return Some(
            "passed product-validation report missing automation artifact inventory".to_owned(),
        );
    };
    if !artifacts.iter().any(nonblank_viewport_capture_artifact) {
        return Some(
            "passed product-validation report missing nonblank viewport_capture artifact"
                .to_owned(),
        );
    }

    if value
        .pointer("/gpu_adapter/name")
        .and_then(Value::as_str)
        .is_none()
    {
        return Some("passed product-validation report missing GPU adapter name".to_owned());
    }
    if value
        .pointer("/gpu_timestamp_timing/status")
        .and_then(Value::as_str)
        .is_none()
    {
        return Some("passed product-validation report missing GPU timestamp status".to_owned());
    }
    if value
        .pointer("/environment/product_validate_gpu_timestamps_requested")
        .and_then(Value::as_bool)
        == Some(true)
    {
        if value
            .pointer("/gpu_timestamp_timing/status")
            .and_then(Value::as_str)
            != Some("enabled")
        {
            return Some(
                "timestamp-requested product-validation report missing enabled GPU timestamp status"
                    .to_owned(),
            );
        }
        if value
            .pointer("/metrics/display_refresh_timing_summary/phases_ms/gpu_compute/sample_count")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            == 0
        {
            return Some(
                "timestamp-requested product-validation report missing gpu_compute samples"
                    .to_owned(),
            );
        }
    }

    None
}

fn nonblank_viewport_capture_artifact(artifact: &Value) -> bool {
    if artifact.get("kind").and_then(Value::as_str) != Some("viewport_capture") {
        return false;
    }
    if artifact
        .get("path")
        .and_then(Value::as_str)
        .is_none_or(|path| path.trim().is_empty())
    {
        return false;
    }
    let Some(pixel_stats) = artifact.get("pixel_stats") else {
        return false;
    };
    pixel_stats
        .get("pixel_count")
        .and_then(Value::as_u64)
        .is_some_and(|count| count > 0)
        && pixel_stats
            .get("nonzero_rgb_pixels")
            .and_then(Value::as_u64)
            .is_some_and(|count| count > 0)
        && pixel_stats
            .get("max_rgb")
            .and_then(Value::as_u64)
            .is_some_and(|value| value > 0)
}

pub(super) fn verify_e2e_report_policy_failure(value: &Value) -> Option<String> {
    if value.get("status").and_then(Value::as_str) != Some("passed") {
        return None;
    }

    for field in [
        "/requirements/library_workflow_coverage",
        "/requirements/virtual_window_product_automation_coverage",
        "/requirements/real_window_product_automation_is_sectioned",
        "/requirements/unsupported_display_is_explicit",
        "/requirements/failed_product_scenario_fails_gate",
    ] {
        if value.pointer(field).and_then(Value::as_bool) != Some(true) {
            return Some(format!(
                "passed verify-e2e report missing required coverage flag {field}"
            ));
        }
    }

    if value
        .pointer("/portions/library_workflow_tests/evidence_type")
        .and_then(Value::as_str)
        != Some("library_e2e")
        || value
            .pointer("/portions/library_workflow_tests/status")
            .and_then(Value::as_str)
            != Some("passed")
        || value
            .pointer("/portions/library_workflow_tests/test_count")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            == 0
    {
        return Some("passed verify-e2e report missing passed library workflow portion".to_owned());
    }

    if value
        .pointer("/portions/virtual_window_product_automation/evidence_type")
        .and_then(Value::as_str)
        != Some("virtual_window_product_automation")
        || value
            .pointer("/portions/virtual_window_product_automation/status")
            .and_then(Value::as_str)
            != Some("passed")
        || value
            .pointer("/portions/virtual_window_product_automation/test_count")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            == 0
    {
        return Some(
            "passed verify-e2e report missing passed virtual-window automation portion".to_owned(),
        );
    }

    let Some(real_window) = value.pointer("/portions/real_window_product_automation") else {
        return Some(
            "passed verify-e2e report missing real-window product automation portion".to_owned(),
        );
    };
    if real_window.get("evidence_type").and_then(Value::as_str)
        != Some("real_window_product_automation")
    {
        return Some(
            "passed verify-e2e report missing real-window product automation evidence type"
                .to_owned(),
        );
    }
    if real_window
        .get("failed")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        != 0
    {
        return Some("passed verify-e2e report contains failed product scenarios".to_owned());
    }
    let real_status = real_window.get("status").and_then(Value::as_str);
    if !matches!(real_status, Some("passed" | "unsupported")) {
        return Some(
            "passed verify-e2e report missing passed/unsupported real-window status".to_owned(),
        );
    }
    let Some(scenarios) = real_window.get("scenarios").and_then(Value::as_array) else {
        return Some("passed verify-e2e report missing real-window scenarios".to_owned());
    };
    for scenario_name in [
        "generated_fixture_camera_smoke",
        "generated_fixture_render_modes",
        "custom_script",
    ] {
        let Some(scenario) = scenarios.iter().find(|scenario| {
            scenario.get("scenario").and_then(Value::as_str) == Some(scenario_name)
        }) else {
            return Some(format!(
                "passed verify-e2e report missing real-window scenario {scenario_name}"
            ));
        };
        match scenario.get("status").and_then(Value::as_str) {
            Some("passed") => {
                if scenario
                    .get("product_validation_report")
                    .and_then(Value::as_str)
                    .is_none()
                {
                    return Some(format!(
                        "passed verify-e2e scenario {scenario_name} missing product-validation report path"
                    ));
                }
            }
            Some("unsupported") => {
                if scenario
                    .get("failure_reason")
                    .and_then(Value::as_str)
                    .is_none()
                {
                    return Some(format!(
                        "unsupported verify-e2e scenario {scenario_name} missing explicit reason"
                    ));
                }
            }
            _ => {
                return Some(format!(
                    "passed verify-e2e report has invalid status for scenario {scenario_name}"
                ));
            }
        }
    }

    None
}

pub(super) fn verify_render_report_policy_failure(value: &Value) -> Option<String> {
    if value.get("status").and_then(Value::as_str) != Some("passed") {
        return None;
    }

    for field in [
        "/requirements/requires_non_cpu_gpu",
        "/requirements/fails_without_adapter",
        "/requirements/evidence_must_include_pixels_or_resources",
        "/requirements/product_device_limit_coverage",
        "/requirements/display_texture_coverage",
        "/requirements/app_backend_coverage",
        "/requirements/timestamp_capability_reported",
    ] {
        if value.pointer(field).and_then(Value::as_bool) != Some(true) {
            return Some(format!(
                "passed verify-render report missing required coverage flag {field}"
            ));
        }
    }

    if value
        .pointer("/gpu_adapter/name")
        .and_then(Value::as_str)
        .is_none()
        || value
            .pointer("/gpu_adapter/backend")
            .and_then(Value::as_str)
            .is_none()
        || value
            .pointer("/gpu_adapter/device_type")
            .and_then(Value::as_str)
            .is_none()
        || value
            .pointer("/gpu_adapter/timestamp_queries_supported")
            .and_then(Value::as_bool)
            .is_none()
    {
        return Some("passed verify-render report missing GPU adapter diagnostics".to_owned());
    }
    if value
        .pointer("/gpu_adapter/device_type")
        .and_then(Value::as_str)
        == Some("Cpu")
    {
        return Some("passed verify-render report used a CPU adapter".to_owned());
    }

    let test_count = value
        .pointer("/tests/test_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if test_count == 0
        || value.pointer("/tests/passed").and_then(Value::as_u64) != Some(test_count)
        || value.pointer("/tests/failed").and_then(Value::as_u64) != Some(0)
    {
        return Some("passed verify-render report missing all-passed test inventory".to_owned());
    }
    let Some(items) = value.pointer("/tests/items").and_then(Value::as_array) else {
        return Some("passed verify-render report missing test item inventory".to_owned());
    };
    for required in [
        "existing_device_limit_rejection",
        "existing_device_product_limits",
        "resident_brick_pixel_parity",
        "resident_float32_pixel_parity",
        "display_texture_pixel_and_resource_parity",
        "display_texture_dvr_parity",
        "display_texture_iso_parity",
        "app_dense_gpu_backend",
        "app_resident_gpu_backend",
    ] {
        if !items.iter().any(|item| {
            item.get("evidence_type").and_then(Value::as_str) == Some(required)
                && item.get("status").and_then(Value::as_str) == Some("passed")
        }) {
            return Some(format!(
                "passed verify-render report missing passed evidence type {required}"
            ));
        }
    }

    None
}

pub(super) fn verify_ui_report_policy_failure(value: &Value) -> Option<String> {
    for field in [
        "/coverage_summary/semantic_ui_tree_tests",
        "/coverage_summary/visual_snapshot_tests",
        "/coverage_summary/high_dpi_tests",
        "/coverage_summary/narrow_layout_tests",
        "/coverage_summary/long_label_tests",
    ] {
        if value.pointer(field).and_then(Value::as_u64).is_none() {
            return Some(format!("verify-ui report missing numeric field {field}"));
        }
    }
    if value
        .pointer("/artifacts/snapshot_artifacts")
        .and_then(Value::as_array)
        .is_none()
    {
        return Some("verify-ui report missing snapshot artifact inventory".to_owned());
    }
    let Some(items) = value.pointer("/tests/items").and_then(Value::as_array) else {
        return Some("verify-ui report missing test item inventory".to_owned());
    };
    for item in items {
        let Some(evidence_layer) = item.get("evidence_layer").and_then(Value::as_str) else {
            return Some("verify-ui test item missing evidence_layer".to_owned());
        };
        if !matches!(evidence_layer, "semantic_ui_tree" | "visual_snapshot") {
            return Some(format!(
                "verify-ui test item has unknown evidence_layer {evidence_layer:?}"
            ));
        }
        if item
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            == 0
        {
            return Some("verify-ui test item missing positive timeout_secs".to_owned());
        }
        if evidence_layer == "visual_snapshot"
            && (item.get("snapshot_name").and_then(Value::as_str).is_none()
                || item
                    .get("snapshot_artifacts")
                    .and_then(Value::as_object)
                    .is_none())
        {
            return Some("verify-ui visual snapshot item missing artifact metadata".to_owned());
        }
    }
    None
}

pub(super) fn product_validation_scenario_name(value: &Value) -> Option<String> {
    value.get("scenario").and_then(|scenario| {
        scenario.as_str().map(str::to_owned).or_else(|| {
            scenario
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    })
}

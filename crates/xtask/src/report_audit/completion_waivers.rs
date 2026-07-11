use std::{collections::BTreeSet, path::Path};

use serde_json::{Value, json};

use crate::reports::read_json_file;

pub(super) const COMPLETION_WAIVERS_RELATIVE_PATH: &str =
    "completion-waivers/completion-waivers.json";
pub(super) const COMPLETION_WAIVERS_SCHEMA: &str = "mirante4d-completion-waivers";
pub(super) const COMPLETION_SCOPE: &str = "product_validation_testing_refactor_completion";

pub(super) fn apply_completion_waivers(
    root: &Path,
    mut blockers: Vec<Value>,
) -> (Vec<Value>, Vec<Value>, Value) {
    let waiver_path = root.join(COMPLETION_WAIVERS_RELATIVE_PATH);
    let report = match read_json_file(&waiver_path) {
        Ok(report) => report,
        Err(_) => {
            return (
                blockers,
                Vec::new(),
                json!({
                    "status": "no_completion_waivers_recorded",
                    "path": super::display_target_relative(COMPLETION_WAIVERS_RELATIVE_PATH),
                }),
            );
        }
    };

    if let Some(reason) = completion_waiver_report_policy_failure(&report) {
        blockers.push(json!({
            "code": "completion_waiver_report_invalid",
            "path": super::display_target_relative(COMPLETION_WAIVERS_RELATIVE_PATH),
            "reason": reason,
            "required_action": "remove the invalid waiver report or regenerate it with cargo xtask completion-waiver after explicit user approval",
        }));
        return (
            blockers,
            Vec::new(),
            json!({
                "status": "invalid_completion_waivers",
                "path": super::display_target_relative(COMPLETION_WAIVERS_RELATIVE_PATH),
            }),
        );
    }

    let waivers = report
        .get("waivers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut remaining = Vec::new();
    let mut waived = Vec::new();
    for mut blocker in blockers {
        if let Some(waiver) = waivers
            .iter()
            .find(|waiver| waiver_matches_blocker(waiver, &blocker))
        {
            if let Some(object) = blocker.as_object_mut() {
                object.insert("waiver".to_owned(), waiver_summary_json(waiver));
            }
            waived.push(blocker);
        } else {
            remaining.push(blocker);
        }
    }

    let status = if waived.is_empty() {
        "waivers_present_but_no_current_blockers_matched"
    } else if remaining.is_empty() {
        "all_current_blockers_waived"
    } else {
        "some_current_blockers_waived"
    };

    (
        remaining,
        waived,
        json!({
            "status": status,
            "path": super::display_target_relative(COMPLETION_WAIVERS_RELATIVE_PATH),
            "waiver_count": waivers.len(),
        }),
    )
}

pub(super) fn completion_waiver_evidence_entry(root: &Path, relative_path: &str) -> Value {
    let path = root.join(relative_path);
    match read_json_file(&path) {
        Ok(value) => {
            let schema = value.get("schema").and_then(Value::as_str);
            let command = value.get("command").and_then(Value::as_str);
            let status = value.get("status").and_then(Value::as_str);
            let mut failure_reason = None;
            let mut audit_status = match status {
                Some("passed") => "present_current_passed",
                Some(_) => "present_current_unknown_evidence_status",
                None => "present_current_no_evidence_status",
            };
            let mut blocking = false;

            if schema != Some(COMPLETION_WAIVERS_SCHEMA) {
                blocking = true;
                audit_status = "present_schema_mismatch";
                failure_reason = Some(format!(
                    "expected schema {:?}, found {:?}",
                    Some(COMPLETION_WAIVERS_SCHEMA),
                    schema
                ));
            } else if command != Some("completion-waiver") {
                blocking = true;
                audit_status = "present_command_mismatch";
                failure_reason = Some(format!(
                    "expected command {:?}, found {:?}",
                    Some("completion-waiver"),
                    command
                ));
            } else if let Some(policy_failure) = completion_waiver_report_policy_failure(&value) {
                blocking = true;
                audit_status = "present_policy_mismatch";
                failure_reason = Some(policy_failure);
            }

            json!({
                "path": super::display_target_relative(relative_path),
                "family": "completion_readiness",
                "evidence_role": "optional_completion_waivers",
                "classification": "completion_waiver_evidence",
                "presence": "present",
                "audit_status": audit_status,
                "blocking": blocking,
                "expected_schema": COMPLETION_WAIVERS_SCHEMA,
                "actual_schema": schema,
                "expected_command": "completion-waiver",
                "actual_command": command,
                "evidence_status": status,
                "failure_reason": failure_reason,
                "notes": "Explicit user-approved completion-readiness waivers; not product or CI evidence.",
            })
        }
        Err(err) => json!({
            "path": super::display_target_relative(relative_path),
            "family": "completion_readiness",
            "evidence_role": "optional_completion_waivers",
            "classification": "completion_waiver_evidence",
            "presence": "present",
            "audit_status": "present_unreadable_json",
            "blocking": true,
            "expected_schema": COMPLETION_WAIVERS_SCHEMA,
            "expected_command": "completion-waiver",
            "failure_reason": err.to_string(),
            "notes": "Explicit user-approved completion-readiness waivers; not product or CI evidence.",
        }),
    }
}

fn completion_waiver_report_policy_failure(value: &Value) -> Option<String> {
    if value.get("status").and_then(Value::as_str) != Some("passed") {
        return Some("completion waiver report missing passed status".to_owned());
    }
    if value.get("scope").and_then(Value::as_str) != Some(COMPLETION_SCOPE) {
        return Some("completion waiver report has wrong scope".to_owned());
    }
    if value
        .pointer("/summary/explicit_user_approval")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Some("completion waiver report missing explicit user approval summary".to_owned());
    }

    let Some(waivers) = value.get("waivers").and_then(Value::as_array) else {
        return Some("completion waiver report missing waivers array".to_owned());
    };
    if waivers.is_empty() {
        return Some("completion waiver report must contain at least one waiver".to_owned());
    }
    if value
        .pointer("/summary/waiver_count")
        .and_then(Value::as_u64)
        != Some(waivers.len() as u64)
    {
        return Some("completion waiver report waiver_count is stale".to_owned());
    }

    let mut seen = BTreeSet::new();
    for waiver in waivers {
        let Some(code) = waiver.get("code").and_then(Value::as_str) else {
            return Some("completion waiver missing blocker code".to_owned());
        };
        if !is_known_completion_blocker_code(code) {
            return Some(format!(
                "completion waiver has unknown blocker code {code:?}"
            ));
        }
        if waiver.get("scope").and_then(Value::as_str) != Some(COMPLETION_SCOPE) {
            return Some(format!("completion waiver {code:?} has wrong scope"));
        }
        if waiver
            .get("explicit_user_approval")
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Some(format!(
                "completion waiver {code:?} missing explicit user approval"
            ));
        }
        for field in ["approved_by", "reason", "milestone"] {
            if waiver
                .get(field)
                .and_then(Value::as_str)
                .is_none_or(|value| value.trim().is_empty())
            {
                return Some(format!("completion waiver {code:?} missing {field}"));
            }
        }

        let scenario = waiver.get("scenario").and_then(Value::as_str);
        if completion_blocker_requires_scenario(code) {
            if !matches!(
                scenario,
                Some(
                    "t5_qual_001_interaction_mip"
                        | "t5_qual_001_interaction_render_modes"
                        | "t5_qual_001_interaction_continuous"
                )
            ) {
                return Some(format!(
                    "completion waiver {code:?} requires an exact T5Qual001 scenario"
                ));
            }
        } else if scenario.is_some() {
            return Some(format!(
                "completion waiver {code:?} must not include a scenario"
            ));
        }

        let identity = (code, scenario.unwrap_or(""));
        if !seen.insert(identity) {
            return Some(format!("completion waiver {code:?} is duplicated"));
        }
    }

    None
}

fn waiver_matches_blocker(waiver: &Value, blocker: &Value) -> bool {
    if waiver.get("code").and_then(Value::as_str) != blocker.get("code").and_then(Value::as_str) {
        return false;
    }
    let waiver_scenario = waiver.get("scenario").and_then(Value::as_str);
    let blocker_scenario = blocker.get("scenario").and_then(Value::as_str);
    match (blocker_scenario, waiver_scenario) {
        (Some(blocker), Some(waiver)) => blocker == waiver,
        (None, None) => true,
        _ => false,
    }
}

fn waiver_summary_json(waiver: &Value) -> Value {
    json!({
        "code": waiver.get("code").and_then(Value::as_str),
        "scenario": waiver.get("scenario").and_then(Value::as_str),
        "approved_by": waiver.get("approved_by").and_then(Value::as_str),
        "milestone": waiver.get("milestone").and_then(Value::as_str),
        "reason": waiver.get("reason").and_then(Value::as_str),
    })
}

fn is_known_completion_blocker_code(code: &str) -> bool {
    matches!(
        code,
        "baseline_refresh_pending"
            | "baseline_audit_missing"
            | "workflow_audit_not_current"
            | "workflow_audit_missing"
            | "external_ci_run_evidence_pending"
            | "external_ci_run_evidence_missing"
            | "t5_qual_001_product_open_validation_not_current"
            | "t5_qual_001_product_validation_report_missing"
    )
}

fn completion_blocker_requires_scenario(code: &str) -> bool {
    matches!(
        code,
        "t5_qual_001_product_open_validation_not_current"
            | "t5_qual_001_product_validation_report_missing"
    )
}

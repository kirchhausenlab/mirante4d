use super::policy::EXPECTED_WORKFLOW_AUDIT_ENTRIES;
use super::*;
use crate::{
    baseline_audit::{ACCEPTED_BASELINE_CLASSES, ACCEPTED_BASELINE_STATUSES, COMPATIBILITY_FIELDS},
    command_audit::COMMAND_AUDIT_ENTRIES,
};

mod evidence_fixtures;

use evidence_fixtures::*;

fn write_json(path: &std::path::Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn clean_test_git_state() -> GitCheckoutState {
    GitCheckoutState {
        head_sha: Some("abc123".to_owned()),
        dirty_worktree: Some(false),
    }
}

fn write_full_completion_evidence(root: &std::path::Path, external_ci_report: &Value) {
    write_json(
        &root.join("baseline-audit/baseline-audit-report.json"),
        &valid_passed_baseline_audit_report(),
    );
    write_json(
        &root.join("workflow-audit/workflow-audit-report.json"),
        &valid_workflow_audit_report(),
    );
    write_json(
        &root.join(EXTERNAL_CI_EVIDENCE_RELATIVE_PATH),
        external_ci_report,
    );
    for scenario in T5_QUAL_001_PRODUCT_VALIDATION_SCENARIOS {
        write_json(
            &root.join(format!(
                "product-validation/{scenario}/product-validation-report.json"
            )),
            &valid_t5_qual_001_product_validation_report(scenario),
        );
    }
}

#[test]
fn completion_readiness_reports_missing_completion_evidence() {
    let tempdir = tempfile::tempdir().unwrap();

    let readiness =
        completion_readiness_report_with_git_state(tempdir.path(), &clean_test_git_state());

    assert_eq!(readiness["status"], "not_ready");
    assert_eq!(readiness["blocking_count"], 6);
    let codes = readiness["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|blocker| blocker["code"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"baseline_audit_missing"));
    assert!(codes.contains(&"workflow_audit_missing"));
    assert!(codes.contains(&"external_ci_run_evidence_missing"));
    assert!(codes.contains(&"t5_qual_001_product_validation_report_missing"));
}

#[test]
fn completion_readiness_includes_external_ci_completion_plan() {
    let tempdir = tempfile::tempdir().unwrap();

    let readiness =
        completion_readiness_report_with_git_state(tempdir.path(), &clean_test_git_state());
    let external_ci_blocker = readiness["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|blocker| blocker["code"] == "external_ci_run_evidence_missing")
        .unwrap();

    assert_eq!(
        external_ci_blocker["external_ci_completion_plan"]["required_surface_count"],
        2
    );
    assert_eq!(
        external_ci_blocker["external_ci_completion_plan"]["surfaces"][0]["name"],
        "hosted_cpu_ci"
    );
    assert_eq!(
        external_ci_blocker["external_ci_completion_plan"]["surfaces"][1]["name"],
        "self_hosted_gpu_ci"
    );
    assert!(
        external_ci_blocker["required_action"]
            .as_str()
            .unwrap()
            .contains("External CI Evidence finalizer")
    );
}

#[test]
fn completion_readiness_includes_baseline_refresh_remediation() {
    let tempdir = tempfile::tempdir().unwrap();
    write_json(
        &tempdir
            .path()
            .join("baseline-audit/baseline-audit-report.json"),
        &valid_baseline_audit_report(),
    );
    write_json(
        &tempdir
            .path()
            .join("baseline-refresh/baseline-refresh-plan.json"),
        &valid_baseline_refresh_plan_report(),
    );
    let script_path = tempdir
        .path()
        .join("baseline-refresh/baseline-clean-reruns.sh");
    std::fs::write(
        &script_path,
        "#!/usr/bin/env bash\n\
         set -euo pipefail\n\
         git status --porcelain\n\
         echo 'baseline-refresh: rerunning runtime.json'\n\
         cargo run --release -p xtask -- bench-runtime-stress\n\
         cargo xtask baseline-refresh-plan\n",
    )
    .unwrap();

    let readiness =
        completion_readiness_report_with_git_state(tempdir.path(), &clean_test_git_state());
    let baseline_blocker = readiness["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|blocker| blocker["code"] == "baseline_refresh_pending")
        .unwrap();

    assert_eq!(
        baseline_blocker["baseline_refresh_plan"]["policy_current"],
        true
    );
    assert_eq!(
        baseline_blocker["baseline_refresh_plan"]["summary"]["remediation"]["clean_rerun_required_count"],
        1
    );
    assert_eq!(
        baseline_blocker["baseline_refresh_plan"]["generated_clean_rerun_script"],
        "target/mirante4d/baseline-refresh/baseline-clean-reruns.sh"
    );
    assert_eq!(
        baseline_blocker["baseline_refresh_plan"]["generated_clean_rerun_script_artifact"]["status"],
        "present"
    );
    assert_eq!(
        baseline_blocker["baseline_refresh_plan"]["generated_clean_rerun_script_artifact"]["rerun_command_count"],
        1
    );
    assert!(
        baseline_blocker["required_action"]
            .as_str()
            .unwrap()
            .contains("baseline-clean-reruns.sh")
    );
}

#[test]
fn completion_readiness_marks_missing_baseline_clean_rerun_script_not_current() {
    let tempdir = tempfile::tempdir().unwrap();
    write_json(
        &tempdir
            .path()
            .join("baseline-audit/baseline-audit-report.json"),
        &valid_baseline_audit_report(),
    );
    write_json(
        &tempdir
            .path()
            .join("baseline-refresh/baseline-refresh-plan.json"),
        &valid_baseline_refresh_plan_report(),
    );

    let readiness =
        completion_readiness_report_with_git_state(tempdir.path(), &clean_test_git_state());
    let baseline_blocker = readiness["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|blocker| blocker["code"] == "baseline_refresh_pending")
        .unwrap();

    assert_eq!(
        baseline_blocker["baseline_refresh_plan"]["policy_current"],
        false
    );
    assert!(
        baseline_blocker["baseline_refresh_plan"]["policy_failure"]
            .as_str()
            .unwrap()
            .contains("clean-rerun script is missing")
    );
    assert_eq!(
        baseline_blocker["baseline_refresh_plan"]["generated_clean_rerun_script_artifact"]["status"],
        "missing"
    );
}

#[test]
fn completion_readiness_includes_t5_qual_001_product_validation_completion_plan() {
    let tempdir = tempfile::tempdir().unwrap();
    write_json(
        &tempdir
            .path()
            .join("product-validation/t5_qual_001_interaction_mip/product-validation-report.json"),
        &json!({
            "status": "unsupported",
            "environment": {
                "product_validate_preflight_only": true,
                "display_class": "unsupported"
            },
            "dataset": {
                "package_path": "/samples/T5-QUAL-001.m4d",
                "id": "phase20-extreme-T5-QUAL-001",
                "name": "Phase 20 Extreme T5Qual001",
                "active_layer": {
                    "shape": {"t": 1, "z": 2563, "y": 2240, "x": 4183},
                    "dtype": {"stored": "uint8"}
                }
            },
            "scenario": {
                "command_count": 31,
                "render_modes": ["mip"],
                "viewport": {"width": 1280, "height": 720},
                "automation_limits": {"max_decoded_bytes": 1073741824}
            },
            "process": {
                "rss_limit_bytes": 8589934592u64
            },
            "logs": {
                "stdout": "stdout.log",
                "stderr": "stderr.log"
            }
        }),
    );

    let readiness =
        completion_readiness_report_with_git_state(tempdir.path(), &clean_test_git_state());
    let t5_qual_001_blocker = readiness["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|blocker| {
            blocker["code"] == "t5_qual_001_product_open_validation_not_current"
                && blocker["scenario"] == "t5_qual_001_interaction_mip"
        })
        .unwrap();

    assert_eq!(
        t5_qual_001_blocker["t5_qual_001_product_validation_completion_plan"]["scenario"],
        "t5_qual_001_interaction_mip"
    );
    assert_eq!(
        t5_qual_001_blocker["t5_qual_001_product_validation_completion_plan"]["current_evidence"]["dataset_id"],
        "phase20-extreme-T5-QUAL-001"
    );
    assert!(
        t5_qual_001_blocker["t5_qual_001_product_validation_completion_plan"]["product_open_command"]
            ["shell_command"]
            .as_str()
            .unwrap()
            .contains("product-validate /samples/T5-QUAL-001.m4d t5_qual_001_interaction_mip")
    );
}

#[test]
fn report_audit_markdown_includes_completion_blocker_plans() {
    let report = json!({
        "completion_readiness": {
            "status": "not_ready",
            "blocking_count": 3,
            "blockers": [
                {
                    "code": "baseline_refresh_pending",
                    "reason": "baseline refresh pending",
                    "required_action": "rerun baselines",
                    "baseline_refresh_plan": {
                        "status": "not_ready",
                        "generated_clean_rerun_script": "target/mirante4d/baseline-refresh/baseline-clean-reruns.sh",
                        "generated_clean_rerun_script_artifact": {
                            "status": "present",
                            "rerun_command_count": 1,
                            "baseline_section_count": 1,
                            "executable": true
                        },
                        "summary": valid_baseline_refresh_plan_report()["summary"].clone()
                    }
                },
                {
                    "code": "external_ci_run_evidence_missing",
                    "reason": "external CI missing",
                    "required_action": "run external CI",
                    "external_ci_completion_plan": crate::external_ci_evidence::external_ci_completion_plan_json()
                },
                {
                    "code": "t5_qual_001_product_open_validation_not_current",
                    "scenario": "t5_qual_001_interaction_mip",
                    "reason": "T5Qual001 product-open missing",
                    "required_action": "run T5Qual001 validation",
                    "t5_qual_001_product_validation_completion_plan": crate::product_validate::t5_qual_001_product_validation_completion_plan_json(
                        "t5_qual_001_interaction_mip",
                        Some(&json!({
                            "environment": {"product_validate_preflight_only": true},
                            "dataset": {
                                "package_path": "/samples/T5-QUAL-001.m4d",
                                "id": "phase20-extreme-T5-QUAL-001"
                            }
                        }))
                    )
                }
            ]
        },
        "entries": [
            {
                "path": "target/mirante4d/baseline-audit/baseline-audit-report.json",
                "classification": "current_evidence",
                "evidence_status": "needs_refresh",
                "audit_status": "present_current_needs_refresh",
                "blocking": false
            }
        ]
    });

    let markdown = report_audit_markdown(&report);

    assert!(markdown.contains("baseline plan:"));
    assert!(markdown.contains("rerun-commands=`1`"));
    assert!(markdown.contains("baseline-clean-reruns.sh"));
    assert!(markdown.contains("baseline clean-rerun artifact: status=`present`, commands=`1`"));
    assert!(markdown.contains("CI surface `hosted_cpu_ci`"));
    assert!(markdown.contains("CI merge:"));
    assert!(markdown.contains("T5Qual001 plan: scenario=`t5_qual_001_interaction_mip`"));
    assert!(markdown.contains("T5Qual001 product-open:"));
    assert!(
        markdown.contains("| Path | Classification | Evidence Status | Audit Status | Blocking |")
    );
    assert!(markdown.contains("`needs_refresh`"));
}

#[test]
fn completion_readiness_accepts_full_completion_evidence() {
    let tempdir = tempfile::tempdir().unwrap();
    write_full_completion_evidence(tempdir.path(), &valid_external_ci_evidence_report());

    let readiness =
        completion_readiness_report_with_git_state(tempdir.path(), &clean_test_git_state());

    assert_eq!(readiness["status"], "ready");
    assert_eq!(readiness["blocking_count"], 0);
}

#[test]
fn completion_readiness_rejects_stale_external_ci_git_sha() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut stale_external_ci = valid_external_ci_evidence_report();
    stale_external_ci["git_sha"] = json!("older-sha");
    for surface in stale_external_ci["surfaces"].as_array_mut().unwrap() {
        surface["git_sha"] = json!("older-sha");
    }
    write_full_completion_evidence(tempdir.path(), &stale_external_ci);

    let readiness =
        completion_readiness_report_with_git_state(tempdir.path(), &clean_test_git_state());

    assert_eq!(readiness["status"], "not_ready");
    assert_eq!(readiness["blocking_count"], 1);
    assert_eq!(
        readiness["blockers"][0]["code"],
        "external_ci_run_evidence_pending"
    );
    assert!(
        readiness["blockers"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("current git HEAD")
    );
}

#[test]
fn completion_readiness_rejects_external_ci_when_worktree_is_dirty() {
    let tempdir = tempfile::tempdir().unwrap();
    write_full_completion_evidence(tempdir.path(), &valid_external_ci_evidence_report());
    let dirty_git_state = GitCheckoutState {
        head_sha: Some("abc123".to_owned()),
        dirty_worktree: Some(true),
    };

    let readiness = completion_readiness_report_with_git_state(tempdir.path(), &dirty_git_state);

    assert_eq!(readiness["status"], "not_ready");
    assert_eq!(readiness["blocking_count"], 1);
    assert_eq!(
        readiness["blockers"][0]["code"],
        "external_ci_run_evidence_pending"
    );
    assert!(
        readiness["blockers"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("dirty-worktree")
    );
}

#[test]
fn completion_readiness_rejects_weak_baseline_pass_report() {
    let tempdir = tempfile::tempdir().unwrap();
    write_json(
        &tempdir
            .path()
            .join("baseline-audit/baseline-audit-report.json"),
        &json!({
            "status": "passed"
        }),
    );
    write_json(
        &tempdir
            .path()
            .join("workflow-audit/workflow-audit-report.json"),
        &valid_workflow_audit_report(),
    );
    write_json(
        &tempdir.path().join(EXTERNAL_CI_EVIDENCE_RELATIVE_PATH),
        &valid_external_ci_evidence_report(),
    );
    for scenario in T5_QUAL_001_PRODUCT_VALIDATION_SCENARIOS {
        write_json(
            &tempdir.path().join(format!(
                "product-validation/{scenario}/product-validation-report.json"
            )),
            &valid_t5_qual_001_product_validation_report(scenario),
        );
    }

    let readiness =
        completion_readiness_report_with_git_state(tempdir.path(), &clean_test_git_state());

    assert_eq!(readiness["status"], "not_ready");
    assert_eq!(readiness["blocking_count"], 1);
    assert_eq!(readiness["blockers"][0]["code"], "baseline_refresh_pending");
    assert_eq!(readiness["blockers"][0]["status"], "passed");
}

#[test]
fn completion_readiness_rejects_weak_t5_qual_001_pass_reports() {
    let tempdir = tempfile::tempdir().unwrap();
    write_json(
        &tempdir
            .path()
            .join("baseline-audit/baseline-audit-report.json"),
        &valid_passed_baseline_audit_report(),
    );
    write_json(
        &tempdir
            .path()
            .join("workflow-audit/workflow-audit-report.json"),
        &valid_workflow_audit_report(),
    );
    write_json(
        &tempdir.path().join(EXTERNAL_CI_EVIDENCE_RELATIVE_PATH),
        &valid_external_ci_evidence_report(),
    );
    for scenario in T5_QUAL_001_PRODUCT_VALIDATION_SCENARIOS {
        write_json(
            &tempdir.path().join(format!(
                "product-validation/{scenario}/product-validation-report.json"
            )),
            &json!({
                "status": "passed"
            }),
        );
    }

    let readiness =
        completion_readiness_report_with_git_state(tempdir.path(), &clean_test_git_state());

    assert_eq!(readiness["status"], "not_ready");
    assert_eq!(readiness["blocking_count"], 3);
    for blocker in readiness["blockers"].as_array().unwrap() {
        assert_eq!(
            blocker["code"],
            "t5_qual_001_product_open_validation_not_current"
        );
        assert!(
            blocker["reason"]
                .as_str()
                .unwrap()
                .contains("stdout/stderr logs")
        );
    }
}

#[test]
fn completion_readiness_rejects_virtual_or_preflight_t5_qual_001_reports() {
    let tempdir = tempfile::tempdir().unwrap();
    write_json(
        &tempdir
            .path()
            .join("baseline-audit/baseline-audit-report.json"),
        &valid_passed_baseline_audit_report(),
    );
    write_json(
        &tempdir
            .path()
            .join("workflow-audit/workflow-audit-report.json"),
        &valid_workflow_audit_report(),
    );
    write_json(
        &tempdir.path().join(EXTERNAL_CI_EVIDENCE_RELATIVE_PATH),
        &valid_external_ci_evidence_report(),
    );
    let mut virtual_display =
        valid_t5_qual_001_product_validation_report("t5_qual_001_interaction_mip");
    virtual_display["environment"]["display_class"] = json!("virtual_display");
    let mut preflight =
        valid_t5_qual_001_product_validation_report("t5_qual_001_interaction_render_modes");
    preflight["environment"]["product_validate_preflight_only"] = json!(true);
    for (scenario, report) in [
        ("t5_qual_001_interaction_mip", virtual_display),
        ("t5_qual_001_interaction_render_modes", preflight),
        (
            "t5_qual_001_interaction_continuous",
            valid_t5_qual_001_product_validation_report("t5_qual_001_interaction_continuous"),
        ),
    ] {
        write_json(
            &tempdir.path().join(format!(
                "product-validation/{scenario}/product-validation-report.json"
            )),
            &report,
        );
    }

    let readiness =
        completion_readiness_report_with_git_state(tempdir.path(), &clean_test_git_state());

    assert_eq!(readiness["status"], "not_ready");
    assert_eq!(readiness["blocking_count"], 2);
    let reasons = readiness["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|blocker| blocker["reason"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(reasons.iter().any(|reason| reason.contains("real_display")));
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("non-preflight"))
    );
}

#[test]
fn completion_readiness_applies_explicit_matching_waivers() {
    let tempdir = tempfile::tempdir().unwrap();
    write_json(
        &tempdir
            .path()
            .join("baseline-audit/baseline-audit-report.json"),
        &valid_baseline_audit_report(),
    );
    write_json(
        &tempdir
            .path()
            .join("workflow-audit/workflow-audit-report.json"),
        &valid_workflow_audit_report(),
    );
    for scenario in T5_QUAL_001_PRODUCT_VALIDATION_SCENARIOS {
        write_json(
            &tempdir.path().join(format!(
                "product-validation/{scenario}/product-validation-report.json"
            )),
            &json!({
                "status": "unsupported"
            }),
        );
    }
    write_json(
        &tempdir.path().join(COMPLETION_WAIVERS_RELATIVE_PATH),
        &valid_completion_waivers_report(&[
            ("baseline_refresh_pending", None),
            ("external_ci_run_evidence_missing", None),
            (
                "t5_qual_001_product_open_validation_not_current",
                Some("t5_qual_001_interaction_mip"),
            ),
            (
                "t5_qual_001_product_open_validation_not_current",
                Some("t5_qual_001_interaction_render_modes"),
            ),
            (
                "t5_qual_001_product_open_validation_not_current",
                Some("t5_qual_001_interaction_continuous"),
            ),
        ]),
    );

    let readiness = completion_readiness_report(tempdir.path());

    assert_eq!(readiness["status"], "ready");
    assert_eq!(readiness["blocking_count"], 0);
    assert_eq!(readiness["waived_blocking_count"], 5);
    assert_eq!(
        readiness["waiver_status"]["status"],
        "all_current_blockers_waived"
    );
}

#[test]
fn completion_readiness_rejects_invalid_waiver_report() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut invalid =
        valid_completion_waivers_report(&[("external_ci_run_evidence_missing", None)]);
    invalid["waivers"][0]["explicit_user_approval"] = json!(false);
    write_json(
        &tempdir.path().join(COMPLETION_WAIVERS_RELATIVE_PATH),
        &invalid,
    );

    let readiness = completion_readiness_report(tempdir.path());

    assert_eq!(readiness["status"], "not_ready");
    assert_eq!(
        readiness["waiver_status"]["status"],
        "invalid_completion_waivers"
    );
    let codes = readiness["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|blocker| blocker["code"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"completion_waiver_report_invalid"));
}

#[test]
fn current_unsupported_product_report_is_current_but_not_passed() {
    let report = json!({
        "schema": "mirante4d-product-validation-report",
        "command": "product-validate",
        "status": "unsupported",
        "failure_reason": "product validation requires DISPLAY or WAYLAND_DISPLAY",
        "scenario": {
            "name": "generated_fixture_camera_smoke",
            "automation_script": "target/mirante4d/product-validation/generated_fixture_camera_smoke/product-automation-script.json"
        },
        "environment": {
            "display_class": "unsupported",
            "display_class_source": "no_display_environment"
        },
        "dataset": {
            "manifest_status": "loaded"
        },
        "logs": {
            "stdout": "target/mirante4d/product-validation/generated_fixture_camera_smoke/mirante4d-app.stdout.log",
            "stderr": "target/mirante4d/product-validation/generated_fixture_camera_smoke/mirante4d-app.stderr.log"
        }
    });
    let entry = classify_current_json(
        &CurrentEvidencePath {
            relative_path: "product-validation/generated_fixture_camera_smoke/product-validation-report.json",
            family: "product_validation",
            evidence_role: "current_generated_fixture_camera_product_report",
            expected_schema: Some("mirante4d-product-validation-report"),
            expected_command: Some("product-validate"),
            expected_scenario: Some("generated_fixture_camera_smoke"),
            notes: "",
        },
        &report,
    );

    assert_eq!(entry["audit_status"], "present_current_unsupported");
    assert_eq!(entry["blocking"], false);
    assert_eq!(entry["evidence_status"], "unsupported");
}

#[test]
fn current_unsupported_product_report_requires_no_display_evidence_fields() {
    let report = json!({
        "schema": "mirante4d-product-validation-report",
        "command": "product-validate",
        "status": "unsupported",
        "scenario": {
            "name": "generated_fixture_camera_smoke"
        },
        "logs": {
            "stdout": "target/mirante4d/product-validation/generated_fixture_camera_smoke/mirante4d-app.stdout.log",
            "stderr": "target/mirante4d/product-validation/generated_fixture_camera_smoke/mirante4d-app.stderr.log"
        }
    });
    let entry = classify_current_json(
        &CurrentEvidencePath {
            relative_path: "product-validation/generated_fixture_camera_smoke/product-validation-report.json",
            family: "product_validation",
            evidence_role: "current_generated_fixture_camera_product_report",
            expected_schema: Some("mirante4d-product-validation-report"),
            expected_command: Some("product-validate"),
            expected_scenario: Some("generated_fixture_camera_smoke"),
            notes: "",
        },
        &report,
    );

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("failure_reason")
    );
}

#[test]
fn current_product_validation_report_requires_top_level_logs() {
    let report = json!({
        "schema": "mirante4d-product-validation-report",
        "command": "product-validate",
        "status": "unsupported",
        "failure_reason": "product validation requires DISPLAY or WAYLAND_DISPLAY",
        "scenario": {
            "name": "generated_fixture_camera_smoke",
            "automation_script": "target/mirante4d/product-validation/generated_fixture_camera_smoke/product-automation-script.json"
        },
        "environment": {
            "display_class": "unsupported",
            "display_class_source": "no_display_environment"
        },
        "dataset": {
            "manifest_status": "loaded"
        }
    });
    let entry = classify_current_json(&current_generated_fixture_camera_product_spec(), &report);

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("stdout/stderr logs")
    );
}

#[test]
fn current_report_schema_mismatch_is_blocking() {
    let report = json!({
        "schema": "old-product-validation-report",
        "command": "product-validate",
        "status": "passed",
        "scenario": { "name": "generated_fixture_camera_smoke" }
    });
    let entry = classify_current_json(
        &CurrentEvidencePath {
            relative_path: "product-validation/generated_fixture_camera_smoke/product-validation-report.json",
            family: "product_validation",
            evidence_role: "current_generated_fixture_camera_product_report",
            expected_schema: Some("mirante4d-product-validation-report"),
            expected_command: Some("product-validate"),
            expected_scenario: Some("generated_fixture_camera_smoke"),
            notes: "",
        },
        &report,
    );

    assert_eq!(entry["audit_status"], "present_schema_mismatch");
    assert_eq!(entry["blocking"], true);
}

#[test]
fn current_verify_ui_report_requires_coverage_and_artifact_metadata() {
    let report = json!({
        "schema": "mirante4d-verify-ui-report",
        "command": "verify-ui",
        "status": "failed",
        "tests": {
            "items": [
                {
                    "package": "mirante4d-app",
                    "filter": "workbench_shell_image_snapshot_matches_baseline",
                    "evidence_type": "workbench_shell_visual_snapshot",
                    "run_ignored": true,
                    "status": "failed"
                }
            ]
        }
    });
    let entry = classify_current_json(&current_ui_report_spec(), &report);

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("coverage_summary")
    );
}

#[test]
fn current_verify_ui_timeout_report_is_schema_checked_current_evidence() {
    let report = json!({
        "schema": "mirante4d-verify-ui-report",
        "command": "verify-ui",
        "status": "failed",
        "coverage_summary": {
            "semantic_ui_tree_tests": 11,
            "visual_snapshot_tests": 1,
            "high_dpi_tests": 1,
            "narrow_layout_tests": 3,
            "long_label_tests": 1
        },
        "artifacts": {
            "snapshot_artifacts": [
                {
                    "snapshot_name": "workbench_shell_basic",
                    "baseline": "crates/mirante4d-app/tests/snapshots/workbench_shell_basic.png",
                    "new": "crates/mirante4d-app/tests/snapshots/workbench_shell_basic.new.png",
                    "diff": "crates/mirante4d-app/tests/snapshots/workbench_shell_basic.diff.png"
                }
            ]
        },
        "tests": {
            "items": [
                {
                    "package": "mirante4d-app",
                    "filter": "workbench_shell_exposes_primary_regions_at_high_dpi",
                    "evidence_type": "high_dpi_shell_semantic_layout",
                    "evidence_layer": "semantic_ui_tree",
                    "run_ignored": false,
                    "timeout_secs": 60,
                    "status": "passed"
                },
                {
                    "package": "mirante4d-app",
                    "filter": "workbench_shell_image_snapshot_matches_baseline",
                    "evidence_type": "workbench_shell_visual_snapshot",
                    "evidence_layer": "visual_snapshot",
                    "run_ignored": true,
                    "timeout_secs": 1,
                    "snapshot_name": "workbench_shell_basic",
                    "snapshot_artifacts": {
                        "snapshot_name": "workbench_shell_basic",
                        "baseline": "crates/mirante4d-app/tests/snapshots/workbench_shell_basic.png",
                        "new": "crates/mirante4d-app/tests/snapshots/workbench_shell_basic.new.png",
                        "diff": "crates/mirante4d-app/tests/snapshots/workbench_shell_basic.diff.png"
                    },
                    "status": "failed"
                }
            ]
        }
    });
    let entry = classify_current_json(&current_ui_report_spec(), &report);

    assert_eq!(entry["audit_status"], "present_current_failed_evidence");
    assert_eq!(entry["blocking"], false);
}

#[test]
fn current_product_validation_pass_requires_product_open_evidence_fields() {
    let report = json!({
        "schema": "mirante4d-product-validation-report",
        "command": "product-validate",
        "status": "passed",
        "scenario": {
            "name": "generated_fixture_camera_smoke",
            "automation_status": "passed"
        },
        "environment": {
            "display_class": "virtual_display",
            "display_class_source": "MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS",
            "product_validate_gpu_timestamps_requested": true
        },
        "process": {
            "exit_success": true
        },
        "dataset": {
            "manifest_status": "loaded"
        },
        "metrics": {
            "app_update_timing_summary": {
                "sample_count": 2
            },
            "display_refresh_timing_summary": {
                "sample_count": 1,
                "phases_ms": {
                    "gpu_compute": {
                        "sample_count": 1
                    }
                }
            },
            "input_to_present_timing_summary": {
                "sample_count": 1
            },
            "presentation_timing": {
                "available_measurements": {
                    "input_to_present_proxy": {
                        "sample_summary_field": "input_to_present_timing_summary"
                    }
                }
            }
        },
        "presentation_timing": {
            "status": "app_proxy_available_os_compositor_timestamp_unavailable",
            "os_compositor_present_timestamp": {
                "status": "unsupported_by_current_eframe_wgpu_integration"
            }
        },
        "limits": {
            "decoded_byte_limit_enforced": true,
            "gpu_upload_byte_limit_enforced": true,
            "gpu_resident_byte_limit_enforced": true
        },
        "artifacts": {
            "automation_artifacts": [
                {
                    "kind": "viewport_capture",
                    "format": "ppm",
                    "path": "target/mirante4d/product-validation/generated_fixture_camera_smoke/artifacts/post-camera-sequence.ppm",
                    "width": 664,
                    "height": 596,
                    "capture_source": "resident_brick_cpu_snapshot",
                    "pixel_stats": {
                        "pixel_count": 395744,
                        "nonzero_rgb_pixels": 4096,
                        "min_rgb": 0,
                        "max_rgb": 255,
                        "mean_rgb": 5.25
                    }
                }
            ]
        },
        "logs": {
            "stdout": "target/mirante4d/product-validation/generated_fixture_camera_smoke/mirante4d-app.stdout.log",
            "stderr": "target/mirante4d/product-validation/generated_fixture_camera_smoke/mirante4d-app.stderr.log"
        },
        "gpu_adapter": {
            "name": "unit adapter"
        },
        "gpu_timestamp_timing": {
            "status": "enabled"
        }
    });
    let entry = classify_current_json(&current_generated_fixture_camera_product_spec(), &report);

    assert_eq!(entry["audit_status"], "present_current_passed");
    assert_eq!(entry["blocking"], false);
}

#[test]
fn current_product_validation_pass_missing_display_metadata_is_blocking() {
    let report = json!({
        "schema": "mirante4d-product-validation-report",
        "command": "product-validate",
        "status": "passed",
        "scenario": {
            "name": "generated_fixture_camera_smoke",
            "automation_status": "passed"
        },
        "logs": {
            "stdout": "target/mirante4d/product-validation/generated_fixture_camera_smoke/mirante4d-app.stdout.log",
            "stderr": "target/mirante4d/product-validation/generated_fixture_camera_smoke/mirante4d-app.stderr.log"
        }
    });
    let entry = classify_current_json(&current_generated_fixture_camera_product_spec(), &report);

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("display_class")
    );
}

#[test]
fn current_product_validation_timestamp_request_requires_gpu_compute_samples() {
    let report = json!({
        "schema": "mirante4d-product-validation-report",
        "command": "product-validate",
        "status": "passed",
        "scenario": {
            "name": "generated_fixture_camera_smoke",
            "automation_status": "passed"
        },
        "environment": {
            "display_class": "virtual_display",
            "display_class_source": "MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS",
            "product_validate_gpu_timestamps_requested": true
        },
        "process": {
            "exit_success": true
        },
        "dataset": {
            "manifest_status": "loaded"
        },
        "metrics": {
            "app_update_timing_summary": {
                "sample_count": 2
            },
            "display_refresh_timing_summary": {
                "sample_count": 1,
                "phases_ms": {
                    "gpu_compute": {
                        "sample_count": 0
                    }
                }
            },
            "input_to_present_timing_summary": {
                "sample_count": 1
            },
            "presentation_timing": {
                "available_measurements": {
                    "input_to_present_proxy": {
                        "sample_summary_field": "input_to_present_timing_summary"
                    }
                }
            }
        },
        "presentation_timing": {
            "status": "app_proxy_available_os_compositor_timestamp_unavailable",
            "os_compositor_present_timestamp": {
                "status": "unsupported_by_current_eframe_wgpu_integration"
            }
        },
        "limits": {
            "decoded_byte_limit_enforced": true,
            "gpu_upload_byte_limit_enforced": true,
            "gpu_resident_byte_limit_enforced": true
        },
        "artifacts": {
            "automation_artifacts": [
                {
                    "kind": "viewport_capture",
                    "format": "ppm",
                    "path": "target/mirante4d/product-validation/generated_fixture_camera_smoke/artifacts/post-camera-sequence.ppm",
                    "width": 664,
                    "height": 596,
                    "capture_source": "resident_brick_cpu_snapshot",
                    "pixel_stats": {
                        "pixel_count": 395744,
                        "nonzero_rgb_pixels": 4096,
                        "min_rgb": 0,
                        "max_rgb": 255,
                        "mean_rgb": 5.25
                    }
                }
            ]
        },
        "logs": {
            "stdout": "target/mirante4d/product-validation/generated_fixture_camera_smoke/mirante4d-app.stdout.log",
            "stderr": "target/mirante4d/product-validation/generated_fixture_camera_smoke/mirante4d-app.stderr.log"
        },
        "gpu_adapter": {
            "name": "unit adapter"
        },
        "gpu_timestamp_timing": {
            "status": "enabled"
        }
    });
    let entry = classify_current_json(&current_generated_fixture_camera_product_spec(), &report);

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("gpu_compute samples")
    );
}

#[test]
fn current_product_validation_pass_rejects_blank_viewport_capture() {
    let mut report = valid_t5_qual_001_product_validation_report("t5_qual_001_interaction_mip");
    report["artifacts"]["automation_artifacts"][0]["pixel_stats"] = json!({
        "pixel_count": 921600,
        "nonzero_rgb_pixels": 0,
        "min_rgb": 0,
        "max_rgb": 0,
        "mean_rgb": 0.0
    });
    let entry = classify_current_json(
        &CurrentEvidencePath {
            relative_path: "product-validation/t5_qual_001_interaction_mip/product-validation-report.json",
            family: "product_validation",
            evidence_role: "current_t5_qual_001_interaction_mip_product_report",
            expected_schema: Some("mirante4d-product-validation-report"),
            expected_command: Some("product-validate"),
            expected_scenario: Some("t5_qual_001_interaction_mip"),
            notes: "",
        },
        &report,
    );

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("nonblank viewport_capture")
    );
}

#[test]
fn current_t5_qual_001_product_report_requires_process_rss_limit() {
    let report = json!({
        "schema": "mirante4d-product-validation-report",
        "command": "product-validate",
        "status": "unsupported",
        "failure_reason": "product validation requires DISPLAY or WAYLAND_DISPLAY",
        "scenario": {
            "name": "t5_qual_001_interaction_mip",
            "automation_script": "target/mirante4d/product-validation/t5_qual_001_interaction_mip/product-automation-script.json"
        },
        "environment": {
            "display_class": "unsupported",
            "display_class_source": "no_display_environment"
        },
        "dataset": {
            "manifest_status": "loaded"
        },
        "logs": {
            "stdout": "target/mirante4d/product-validation/t5_qual_001_interaction_mip/mirante4d-app.stdout.log",
            "stderr": "target/mirante4d/product-validation/t5_qual_001_interaction_mip/mirante4d-app.stderr.log"
        },
        "limits": {}
    });
    let entry = classify_current_json(
        &CurrentEvidencePath {
            relative_path: "product-validation/t5_qual_001_interaction_mip/product-validation-report.json",
            family: "product_validation",
            evidence_role: "current_t5_qual_001_interaction_mip_product_report",
            expected_schema: Some("mirante4d-product-validation-report"),
            expected_command: Some("product-validate"),
            expected_scenario: Some("t5_qual_001_interaction_mip"),
            notes: "",
        },
        &report,
    );

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("process RSS limit")
    );
}

#[test]
fn legacy_root_product_validation_artifact_fails_audit() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = tempdir.path().join("target").join("mirante4d");
    let legacy = root
        .join("product-validation")
        .join("product-validation-report.json");
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::write(&legacy, "{}").unwrap();

    let report = report_audit_report_json(&root).unwrap();

    assert_eq!(report["status"], "failed");
    assert_eq!(report["summary"]["stale_legacy_present_count"], 1);
    assert_eq!(report["summary"]["blocking_count"], 1);
}

#[test]
fn unregistered_benchmark_report_is_quarantined_not_current() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = tempdir.path().join("target").join("mirante4d");
    let report_path = root
        .join("benchmarks")
        .join("bench-slice-content-interaction-phase20-extreme-T5-QUAL-001.json");
    fs::create_dir_all(report_path.parent().unwrap()).unwrap();
    fs::write(
        &report_path,
        serde_json::to_vec(&json!({
            "benchmark": "bench-slice-content-interaction",
            "benchmark_schema_version": 3
        }))
        .unwrap(),
    )
    .unwrap();

    let report = report_audit_report_json(&root).unwrap();
    let entry = report["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| {
                entry["path"]
                    == "target/mirante4d/benchmarks/bench-slice-content-interaction-phase20-extreme-T5-QUAL-001.json"
            })
            .unwrap();

    assert_eq!(entry["classification"], "quarantined_discovered_evidence");
    assert_eq!(
        entry["audit_status"],
        "quarantined_historical_benchmark_report"
    );
    assert_eq!(entry["blocking"], false);
    assert_eq!(report["status"], "passed");
}

#[test]
fn malformed_completion_waiver_report_is_blocking_when_present() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = tempdir.path().join("target").join("mirante4d");
    let mut invalid = valid_completion_waivers_report(&[("baseline_refresh_pending", None)]);
    invalid["waivers"][0]["code"] = json!("unknown_blocker");
    write_json(&root.join(COMPLETION_WAIVERS_RELATIVE_PATH), &invalid);

    let report = report_audit_report_json(&root).unwrap();
    let entry = report["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["path"] == "target/mirante4d/completion-waivers/completion-waivers.json"
        })
        .unwrap();

    assert_eq!(entry["classification"], "completion_waiver_evidence");
    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert_eq!(report["status"], "failed");
}

#[test]
fn duplicate_completion_waiver_report_is_blocking_when_present() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = tempdir.path().join("target").join("mirante4d");
    let invalid = valid_completion_waivers_report(&[
        ("baseline_refresh_pending", None),
        ("baseline_refresh_pending", None),
    ]);
    write_json(&root.join(COMPLETION_WAIVERS_RELATIVE_PATH), &invalid);

    let report = report_audit_report_json(&root).unwrap();
    let entry = report["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["path"] == "target/mirante4d/completion-waivers/completion-waivers.json"
        })
        .unwrap();

    assert_eq!(entry["classification"], "completion_waiver_evidence");
    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("duplicated")
    );
    assert_eq!(report["status"], "failed");
}

#[test]
fn current_exact_report_paths_are_schema_checked() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = tempdir.path().join("target").join("mirante4d");
    let report_path = root.join("verify-e2e").join("verify-e2e-report.json");
    fs::create_dir_all(report_path.parent().unwrap()).unwrap();
    fs::write(
        &report_path,
        serde_json::to_vec(&valid_verify_e2e_report()).unwrap(),
    )
    .unwrap();

    let report = report_audit_report_json(&root).unwrap();
    let entry = report["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == "target/mirante4d/verify-e2e/verify-e2e-report.json")
        .unwrap();

    assert_eq!(entry["classification"], "current_evidence");
    assert_eq!(entry["audit_status"], "present_current_passed");
    assert_eq!(entry["blocking"], false);
}

#[test]
fn current_command_audit_report_requires_current_command_inventory() {
    let entry = classify_current_json(
        &current_command_audit_report_spec(),
        &valid_command_audit_report(),
    );

    assert_eq!(entry["audit_status"], "present_current_passed");
    assert_eq!(entry["blocking"], false);

    let mut missing_help = valid_command_audit_report();
    let entries = missing_help["entries"].as_array_mut().unwrap();
    entries.retain(|entry| entry["command"] != "help");
    let entry = classify_current_json(&current_command_audit_report_spec(), &missing_help);

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("command surface")
    );

    let mut stale_product_role = valid_command_audit_report();
    let product_validate = stale_product_role["entries"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|entry| entry["command"] == "product-validate")
        .unwrap();
    product_validate["product_evidence_role"] = json!("benchmark_supporting_evidence_only");
    let entry = classify_current_json(&current_command_audit_report_spec(), &stale_product_role);

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("product_evidence_role")
    );
}

#[test]
fn current_baseline_audit_report_requires_policy_consistent_inventory() {
    let entry = classify_current_json(
        &current_baseline_audit_report_spec(),
        &valid_baseline_audit_report(),
    );

    assert_eq!(entry["audit_status"], "present_current_needs_refresh");
    assert_eq!(entry["evidence_status"], "needs_refresh");
    assert_eq!(entry["blocking"], false);

    let mut stale_summary = valid_baseline_audit_report();
    stale_summary["summary"]["needs_refresh_count"] = json!(0);
    let entry = classify_current_json(&current_baseline_audit_report_spec(), &stale_summary);

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("needs_refresh_count")
    );

    let mut bad_class = valid_baseline_audit_report();
    bad_class["entries"][0]["baseline_class"] = json!("unknown_gpu");
    let entry = classify_current_json(&current_baseline_audit_report_spec(), &bad_class);

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("baseline_class")
    );

    let mut readme_mismatch = valid_baseline_audit_report();
    readme_mismatch["documentation"]["blocking"] = json!(true);
    readme_mismatch["documentation"]["blocking_count"] = json!(1);
    readme_mismatch["documentation"]["audit_status"] =
        json!("readme_current_baseline_list_mismatch");
    readme_mismatch["documentation"]["missing_referenced_baselines"] = json!(["missing.json"]);
    readme_mismatch["summary"]["blocking_count"] = json!(1);
    let entry = classify_current_json(&current_baseline_audit_report_spec(), &readme_mismatch);

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("README baseline-list mismatch")
    );
}

#[test]
fn current_baseline_refresh_plan_report_requires_non_mutating_consistent_counts() {
    let entry = classify_current_json(
        &current_baseline_refresh_plan_report_spec(),
        &valid_baseline_refresh_plan_report(),
    );

    assert_eq!(entry["audit_status"], "present_current_not_ready");
    assert_eq!(entry["blocking"], false);

    let mut stale_count = valid_baseline_refresh_plan_report();
    stale_count["summary"]["not_ready_count"] = json!(0);
    let entry = classify_current_json(&current_baseline_refresh_plan_report_spec(), &stale_count);

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("not-ready count")
    );

    let mut mutating = valid_baseline_refresh_plan_report();
    mutating["summary"]["writes_curated_baselines"] = json!(true);
    let entry = classify_current_json(&current_baseline_refresh_plan_report_spec(), &mutating);

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("non-mutating")
    );

    let mut stale_remediation = valid_baseline_refresh_plan_report();
    stale_remediation["summary"]["remediation"]["clean_rerun_required_count"] = json!(0);
    let entry = classify_current_json(
        &current_baseline_refresh_plan_report_spec(),
        &stale_remediation,
    );

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("clean-rerun count")
    );

    let mut stale_rerun_count = valid_baseline_refresh_plan_report();
    stale_rerun_count["summary"]["remediation"]["rerun_command_available_count"] = json!(0);
    let entry = classify_current_json(
        &current_baseline_refresh_plan_report_spec(),
        &stale_rerun_count,
    );

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("rerun-command available count")
    );

    let mut missing_clean_rerun_script = valid_baseline_refresh_plan_report();
    missing_clean_rerun_script["generated_clean_rerun_script"] = Value::Null;
    let entry = classify_current_json(
        &current_baseline_refresh_plan_report_spec(),
        &missing_clean_rerun_script,
    );

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("clean-rerun script")
    );
}

#[test]
fn current_baseline_refresh_plan_report_requires_heavy_rerun_opt_in() {
    let mut missing_env = valid_baseline_refresh_plan_report();
    missing_env["entries"][0]["signature"]["benchmark"] = json!("bench-phase11-interaction");
    missing_env["entries"][0]["rerun"]["primary_command"]["argv"] = json!([
        "cargo",
        "run",
        "--release",
        "-p",
        "xtask",
        "--",
        "bench-phase11-interaction",
        "target/mirante4d/benchmarks/import-sample/t5_qual_003.m4d"
    ]);
    missing_env["entries"][0]["rerun"]["primary_command"]["shell_command"] = json!(
        "MIRANTE4D_BENCH_BASELINE_CLASS=local_gpu MIRANTE4D_BENCH_DATASET_CLASS=synthetic_runtime_stress MIRANTE4D_BENCH_HARDWARE_CLASS=hw2-linux-vulkan-reference MIRANTE4D_BENCH_HARDWARE_NAME=hw2-linux-vulkan-reference cargo run --release -p xtask -- bench-phase11-interaction target/mirante4d/benchmarks/import-sample/t5_qual_003.m4d"
    );
    let entry = classify_current_json(&current_baseline_refresh_plan_report_spec(), &missing_env);

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("heavy rerun command missing")
    );

    let mut missing_shell_env = missing_env;
    missing_shell_env["entries"][0]["rerun"]["primary_command"]["env"]["MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK"] =
        json!("1");
    let entry = classify_current_json(
        &current_baseline_refresh_plan_report_spec(),
        &missing_shell_env,
    );

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("heavy rerun shell_command missing")
    );
}

#[test]
fn current_workflow_audit_report_requires_policy_consistent_inventory() {
    let entry = classify_current_json(
        &current_workflow_audit_report_spec(),
        &valid_workflow_audit_report(),
    );

    assert_eq!(entry["audit_status"], "present_current_passed");
    assert_eq!(entry["blocking"], false);

    let mut stale_external_evidence = valid_workflow_audit_report();
    stale_external_evidence["summary"]["external_run_evidence"] = json!("checked_by_ci");
    let entry = classify_current_json(
        &current_workflow_audit_report_spec(),
        &stale_external_evidence,
    );

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("external CI runs are not checked")
    );

    let mut failing_required_check = valid_workflow_audit_report();
    failing_required_check["entries"][0]["required"][0]["ok"] = json!(false);
    let entry = classify_current_json(
        &current_workflow_audit_report_spec(),
        &failing_required_check,
    );

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("failing required checks")
    );

    let mut missing_workflow = valid_workflow_audit_report();
    let entries = missing_workflow["entries"].as_array_mut().unwrap();
    entries.retain(|entry| entry["path"] != ".github/workflows/gpu-render.yml");
    missing_workflow["summary"]["workflow_count"] = json!(entries.len());
    let entry = classify_current_json(&current_workflow_audit_report_spec(), &missing_workflow);

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("workflow_count")
    );
}

#[test]
fn current_external_ci_evidence_report_requires_checked_cpu_and_gpu_runs() {
    let entry = classify_current_json(
        &current_external_ci_evidence_report_spec(),
        &valid_external_ci_evidence_report(),
    );

    assert_eq!(entry["audit_status"], "present_current_passed");
    assert_eq!(entry["blocking"], false);

    let mut missing_gpu = valid_external_ci_evidence_report();
    missing_gpu["surfaces"] = json!([
        {
            "name": "hosted_cpu_ci",
            "status": "passed",
            "workflow_path": ".github/workflows/ci.yml",
            "run_url": "https://github.com/example/repo/actions/runs/1",
            "run_id": "1",
            "git_sha": "abc123",
            "required_checks": ["verify-fast"]
        }
    ]);
    missing_gpu["summary"]["required_surface_count"] = json!(1);
    missing_gpu["summary"]["passed_surface_count"] = json!(1);
    missing_gpu["summary"]["blocking_count"] = json!(0);
    let entry = classify_current_json(&current_external_ci_evidence_report_spec(), &missing_gpu);

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("exactly two CI surfaces")
    );

    let mut stale_marker = valid_external_ci_evidence_report();
    stale_marker["summary"]["external_run_evidence"] = json!("not_checked");
    let entry = classify_current_json(&current_external_ci_evidence_report_spec(), &stale_marker);

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("external CI runs were checked")
    );
}

#[test]
fn current_verify_e2e_report_requires_portioned_product_evidence() {
    let entry = classify_current_json(&current_e2e_report_spec(), &valid_verify_e2e_report());

    assert_eq!(entry["audit_status"], "present_current_passed");
    assert_eq!(entry["blocking"], false);

    let mut missing_scenario = valid_verify_e2e_report();
    missing_scenario["portions"]["real_window_product_automation"]["scenarios"] = json!([
        {
            "scenario": "generated_fixture_camera_smoke",
            "status": "passed",
            "product_validation_report": "target/mirante4d/product-validation/generated_fixture_camera_smoke/product-validation-report.json"
        },
        {
            "scenario": "generated_fixture_render_modes",
            "status": "passed",
            "product_validation_report": "target/mirante4d/product-validation/generated_fixture_render_modes/product-validation-report.json"
        }
    ]);
    let entry = classify_current_json(&current_e2e_report_spec(), &missing_scenario);

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("custom_script")
    );
}

#[test]
fn current_verify_render_report_requires_gpu_and_product_relevant_tests() {
    let entry = classify_current_json(
        &current_verify_render_report_spec(),
        &valid_verify_render_report(),
    );

    assert_eq!(entry["audit_status"], "present_current_passed");
    assert_eq!(entry["blocking"], false);

    let mut missing_backend = valid_verify_render_report();
    missing_backend["tests"]["items"] = json!([
        {
            "evidence_type": "existing_device_limit_rejection",
            "status": "passed"
        }
    ]);
    let entry = classify_current_json(&current_verify_render_report_spec(), &missing_backend);

    assert_eq!(entry["audit_status"], "present_policy_mismatch");
    assert_eq!(entry["blocking"], true);
    assert!(
        entry["failure_reason"]
            .as_str()
            .unwrap()
            .contains("existing_device_product_limits")
    );
}

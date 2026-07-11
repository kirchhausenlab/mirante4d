use super::*;

pub(super) fn valid_verify_e2e_report() -> Value {
    json!({
        "schema": "mirante4d-verify-e2e-report",
        "command": "verify-e2e",
        "status": "passed",
        "requirements": {
            "library_workflow_coverage": true,
            "virtual_window_product_automation_coverage": true,
            "real_window_product_automation_is_sectioned": true,
            "unsupported_display_is_explicit": true,
            "failed_product_scenario_fails_gate": true
        },
        "portions": {
            "library_workflow_tests": {
                "evidence_type": "library_e2e",
                "status": "passed",
                "test_count": 1
            },
            "virtual_window_product_automation": {
                "evidence_type": "virtual_window_product_automation",
                "status": "passed",
                "test_count": 1
            },
            "real_window_product_automation": {
                "evidence_type": "real_window_product_automation",
                "status": "passed",
                "failed": 0,
                "scenarios": [
                    {
                        "scenario": "generated_fixture_camera_smoke",
                        "status": "passed",
                        "product_validation_report": "target/mirante4d/product-validation/generated_fixture_camera_smoke/product-validation-report.json"
                    },
                    {
                        "scenario": "generated_fixture_render_modes",
                        "status": "passed",
                        "product_validation_report": "target/mirante4d/product-validation/generated_fixture_render_modes/product-validation-report.json"
                    },
                    {
                        "scenario": "custom_script",
                        "status": "passed",
                        "product_validation_report": "target/mirante4d/product-validation/custom_script/product-validation-report.json"
                    }
                ]
            }
        }
    })
}

pub(super) fn valid_command_audit_report() -> Value {
    let heavy_opt_in_count = COMMAND_AUDIT_ENTRIES
        .iter()
        .filter(|entry| entry.requires_heavy_opt_in)
        .count();
    let smoke_only_count = COMMAND_AUDIT_ENTRIES
        .iter()
        .filter(|entry| entry.evidence_class.contains("smoke"))
        .count();
    let product_validation_count = COMMAND_AUDIT_ENTRIES
        .iter()
        .filter(|entry| entry.evidence_class == "product_automation_validation")
        .count();
    let entries = COMMAND_AUDIT_ENTRIES
        .iter()
        .map(|entry| {
            json!({
                "command": entry.command,
                "family": entry.family,
                "evidence_class": entry.evidence_class,
                "default_safety": entry.default_safety,
                "requires_heavy_opt_in": entry.requires_heavy_opt_in,
                "product_evidence_role": entry.product_evidence_role,
                "stale_or_unsafe_status": entry.stale_or_unsafe_status,
                "report_paths": entry.report_paths,
                "notes": entry.notes,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "schema": "mirante4d-xtask-command-audit",
        "schema_version": 1,
        "command": "command-audit",
        "status": "passed",
        "summary": {
            "command_count": COMMAND_AUDIT_ENTRIES.len(),
            "heavy_opt_in_count": heavy_opt_in_count,
            "smoke_only_count": smoke_only_count,
            "product_validation_count": product_validation_count,
        },
        "entries": entries,
    })
}

pub(super) fn valid_baseline_audit_report() -> Value {
    json!({
        "schema": "mirante4d-baseline-audit",
        "schema_version": 1,
        "command": "baseline-audit",
        "status": "needs_refresh",
        "policy": {
            "accepted_baseline_classes": ACCEPTED_BASELINE_CLASSES,
            "compatibility_fields": COMPATIBILITY_FIELDS,
            "missing_baseline_class_is_blocking": false,
            "missing_baseline_class_status": "legacy_missing_baseline_class",
            "unknown_baseline_class_is_blocking": true,
            "accepted_baseline_statuses": ACCEPTED_BASELINE_STATUSES,
            "stale_timing_status": "stale_timing_needs_refresh",
            "readme_current_baseline_list_must_match_committed_json_files": true,
        },
        "summary": {
            "baseline_count": 2,
            "blocking_count": 0,
            "needs_refresh_count": 1,
            "baseline_class_counts": {
                "local_gpu": 2
            }
        },
        "documentation": {
            "path": "docs/benchmarks/baselines/README.md",
            "audit_status": "readme_current_baseline_list_matches_files",
            "blocking": false,
            "blocking_count": 0,
            "current_baseline_reference_count": 2,
            "missing_referenced_baselines": [],
            "unlisted_baselines": [],
        },
        "entries": [
            {
                "path": "docs/benchmarks/baselines/current.json",
                "audit_status": "current_policy_compliant",
                "blocking": false,
                "needs_refresh": false,
                "policy_action": "compare_with_bench_check_when_context_matches",
                "benchmark": "bench-smoke",
                "baseline_class": "local_gpu",
                "baseline_status": "current",
                "compatibility_fields": {
                    "present": ["benchmark_schema_version", "baseline_class"],
                    "missing": []
                }
            },
            {
                "path": "docs/benchmarks/baselines/stale.json",
                "audit_status": "stale_timing_needs_refresh",
                "blocking": false,
                "needs_refresh": true,
                "policy_action": "refresh_from_clean_worktree_before_using_as_hard_gate",
                "benchmark": "bench-smoke",
                "baseline_class": "local_gpu",
                "baseline_status": "stale_timing_needs_refresh",
                "compatibility_fields": {
                    "present": ["benchmark_schema_version", "baseline_class"],
                    "missing": []
                }
            }
        ]
    })
}

pub(super) fn valid_passed_baseline_audit_report() -> Value {
    let mut report = valid_baseline_audit_report();
    report["status"] = json!("passed");
    report["summary"]["baseline_count"] = json!(1);
    report["summary"]["needs_refresh_count"] = json!(0);
    report["entries"] = json!([report["entries"][0].clone()]);
    report
}

pub(super) fn valid_baseline_refresh_plan_report() -> Value {
    json!({
        "schema": "mirante4d-baseline-refresh-plan",
        "schema_version": 1,
        "command": "baseline-refresh-plan",
        "status": "not_ready",
        "generated_promotion_manifest": null,
        "generated_clean_rerun_script": "target/mirante4d/baseline-refresh/baseline-clean-reruns.sh",
        "summary": {
            "refresh_baseline_count": 1,
            "candidate_report_count": 1,
            "ready_count": 0,
            "not_ready_count": 1,
            "duplicate_selected_source_count": 0,
            "writes_curated_baselines": false,
            "promotion_command": null,
            "remediation": {
                "next_action": "rerun the matching release benchmarks from a clean worktree, then rerun cargo xtask baseline-refresh-plan",
                "source_worktree_required_clean": true,
                "source_report_dirty_worktree_required_false": true,
                "current_worktree_must_be_clean_for_promotion": true,
                "clean_rerun_required_count": 1,
                "rerun_command_available_count": 1,
                "rerun_command_unavailable_count": 0,
                "no_matching_candidate_count": 0,
                "not_promotable_candidate_count": 1,
                "ambiguous_candidate_count": 0,
                "duplicate_source_entry_count": 0
            }
        },
        "entries": [
            {
                "baseline_name": "runtime.json",
                "status": "matched_candidates_not_promotable",
                "selected_source_report": null,
                "signature": {
                    "benchmark": "bench-runtime-stress",
                    "baseline_class": "local_gpu"
                },
                "matched_candidate_count": 1,
                "eligible_candidate_count": 0,
                "matched_candidates": [
                    {
                        "source_report": "target/mirante4d/benchmarks/runtime-stress/bench-runtime-stress.json",
                        "promotable": false,
                        "promotion_error": "baseline promotion refuses source reports with hardware.dirty_worktree=true"
                    }
                ],
                "remediation": {
                    "action": "rerun the matched benchmark from a clean release worktree, then rerun baseline-refresh-plan",
                    "requires_clean_rerun": true,
                    "candidate_error_count": 1,
                    "candidate_errors": [
                        "baseline promotion refuses source reports with hardware.dirty_worktree=true"
                    ]
                },
                "rerun": {
                    "available": true,
                    "release_build_required": true,
                    "primary_command": {
                        "env": {
                            "MIRANTE4D_BENCH_BASELINE_CLASS": "local_gpu",
                            "MIRANTE4D_BENCH_DATASET_CLASS": "synthetic_runtime_stress",
                            "MIRANTE4D_BENCH_HARDWARE_CLASS": "hw2-linux-vulkan-reference",
                            "MIRANTE4D_BENCH_HARDWARE_NAME": "hw2-linux-vulkan-reference"
                        },
                        "argv": [
                            "cargo",
                            "run",
                            "--release",
                            "-p",
                            "xtask",
                            "--",
                            "bench-runtime-stress"
                        ],
                        "shell_command": "MIRANTE4D_BENCH_BASELINE_CLASS=local_gpu MIRANTE4D_BENCH_DATASET_CLASS=synthetic_runtime_stress MIRANTE4D_BENCH_HARDWARE_CLASS=hw2-linux-vulkan-reference MIRANTE4D_BENCH_HARDWARE_NAME=hw2-linux-vulkan-reference cargo run --release -p xtask -- bench-runtime-stress"
                    },
                    "prerequisite_commands": [],
                    "unavailable_reason": null
                }
            }
        ]
    })
}

pub(super) fn valid_t5_qual_001_product_validation_report(scenario: &str) -> Value {
    json!({
        "schema": "mirante4d-product-validation-report",
        "schema_version": 1,
        "command": "product-validate",
        "status": "passed",
        "scenario": {
            "name": scenario,
            "automation_status": "passed",
            "render_modes": if scenario == "t5_qual_001_interaction_render_modes" {
                json!(["mip", "dvr", "iso"])
            } else {
                json!(["mip"])
            },
            "viewport": {
                "width": 1280,
                "height": 720
            },
            "automation_limits": {
                "max_decoded_bytes": 1073741824u64
            }
        },
        "environment": {
            "display_class": "real_display",
            "display_class_source": "detected_environment",
            "product_validate_preflight_only": false,
            "product_validate_gpu_timestamps_requested": false
        },
        "process": {
            "exit_success": true,
            "rss_limit_bytes": 8589934592u64
        },
        "dataset": {
            "manifest_status": "loaded",
            "id": "phase20-extreme-T5-QUAL-001",
            "name": "Phase 20 Extreme T5Qual001",
            "package_path": "/samples/phase20-extreme-T5-QUAL-001.m4d",
            "active_layer": {
                "dtype": {
                    "stored": "uint8"
                },
                "shape": {
                    "t": 1,
                    "z": 2563,
                    "y": 2240,
                    "x": 4183
                }
            }
        },
        "metrics": {
            "app_update_timing_summary": {
                "sample_count": 2
            },
            "display_refresh_timing_summary": {
                "sample_count": 1
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
            "process_rss_limit_bytes": 8589934592u64,
            "decoded_byte_limit_enforced": true,
            "gpu_upload_byte_limit_enforced": true,
            "gpu_resident_byte_limit_enforced": true
        },
        "artifacts": {
            "automation_artifacts": [
                {
                    "kind": "viewport_capture",
                    "format": "ppm",
                    "path": format!(
                        "target/mirante4d/product-validation/{scenario}/artifacts/final.ppm"
                    ),
                    "width": 1280,
                    "height": 720,
                    "capture_source": "resident_brick_cpu_snapshot",
                    "pixel_stats": {
                        "pixel_count": 921600,
                        "nonzero_rgb_pixels": 65536,
                        "min_rgb": 0,
                        "max_rgb": 255,
                        "mean_rgb": 7.5
                    }
                }
            ]
        },
        "logs": {
            "stdout": format!(
                "target/mirante4d/product-validation/{scenario}/mirante4d-app.stdout.log"
            ),
            "stderr": format!(
                "target/mirante4d/product-validation/{scenario}/mirante4d-app.stderr.log"
            )
        },
        "gpu_adapter": {
            "name": "unit adapter"
        },
        "gpu_timestamp_timing": {
            "status": "not_requested"
        }
    })
}

pub(super) fn valid_workflow_audit_report() -> Value {
    let entries = EXPECTED_WORKFLOW_AUDIT_ENTRIES
        .iter()
        .map(|(path, evidence_role)| {
            json!({
                "path": path,
                "evidence_role": evidence_role,
                "presence": "present",
                "audit_status": "workflow_policy_compliant",
                "blocking": false,
                "required": [
                    {
                        "name": "required workflow policy",
                        "ok": true
                    }
                ],
                "forbidden": [
                    {
                        "name": "forbidden workflow policy",
                        "ok": true
                    }
                ]
            })
        })
        .collect::<Vec<_>>();

    json!({
        "schema": "mirante4d-workflow-audit",
        "schema_version": 1,
        "command": "workflow-audit",
        "status": "passed",
        "summary": {
            "workflow_count": entries.len(),
            "blocking_count": 0,
            "external_run_evidence": "not_checked_by_static_workflow_audit"
        },
        "entries": entries,
    })
}

pub(super) fn valid_external_ci_evidence_report() -> Value {
    json!({
        "schema": "mirante4d-external-ci-evidence",
        "schema_version": 1,
        "command": "external-ci-evidence",
        "status": "passed",
        "evidence_source": "operator_supplied_external_run_metadata",
        "git_sha": "abc123",
        "summary": {
            "external_run_evidence": "checked_by_external_ci",
            "required_surface_count": 2,
            "passed_surface_count": 2,
            "blocking_count": 0
        },
        "surfaces": [
            {
                "name": "hosted_cpu_ci",
                "status": "passed",
                "workflow_path": ".github/workflows/ci.yml",
                "run_url": "https://github.com/example/repo/actions/runs/1",
                "run_id": "1",
                "git_sha": "abc123",
                "required_checks": [
                    "command-audit",
                    "baseline-audit",
                    "workflow-audit",
                    "report-audit",
                    "verify-fast",
                    "verify-full",
                    "package-linux-release",
                    "bench-smoke"
                ],
                "check_results": [
                    { "name": "command-audit", "status": "passed", "raw_status": "success" },
                    { "name": "baseline-audit", "status": "passed", "raw_status": "success" },
                    { "name": "workflow-audit", "status": "passed", "raw_status": "success" },
                    { "name": "report-audit", "status": "passed", "raw_status": "success" },
                    { "name": "verify-fast", "status": "passed", "raw_status": "success" },
                    { "name": "verify-full", "status": "passed", "raw_status": "success" },
                    { "name": "package-linux-release", "status": "passed", "raw_status": "success" },
                    { "name": "bench-smoke", "status": "passed", "raw_status": "success" }
                ],
                "check_summary": {
                    "required_check_count": 8,
                    "passed_check_count": 8,
                    "blocking_count": 0
                }
            },
            {
                "name": "self_hosted_gpu_ci",
                "status": "passed",
                "workflow_path": ".github/workflows/gpu-render.yml",
                "run_url": "https://github.com/example/repo/actions/runs/2",
                "run_id": "2",
                "git_sha": "abc123",
                "required_checks": ["verify-render"],
                "check_results": [
                    { "name": "verify-render", "status": "passed", "raw_status": "success" }
                ],
                "check_summary": {
                    "required_check_count": 1,
                    "passed_check_count": 1,
                    "blocking_count": 0
                }
            }
        ]
    })
}

pub(super) fn valid_completion_waivers_report(items: &[(&str, Option<&str>)]) -> Value {
    let waivers = items
        .iter()
        .map(|(code, scenario)| {
            json!({
                "scope": COMPLETION_SCOPE,
                "code": code,
                "scenario": scenario,
                "reason": "user accepted weaker completion evidence for this milestone",
                "approved_by": "project-owner",
                "milestone": "testing-refactor",
                "explicit_user_approval": true,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "schema": "mirante4d-completion-waivers",
        "schema_version": 1,
        "command": "completion-waiver",
        "status": "passed",
        "scope": COMPLETION_SCOPE,
        "summary": {
            "waiver_count": waivers.len(),
            "explicit_user_approval": true
        },
        "waivers": waivers
    })
}

pub(super) fn valid_verify_render_report() -> Value {
    let required_evidence = [
        "existing_device_limit_rejection",
        "existing_device_product_limits",
        "resident_brick_pixel_parity",
        "resident_float32_pixel_parity",
        "display_texture_pixel_and_resource_parity",
        "display_texture_dvr_parity",
        "display_texture_iso_parity",
        "app_dense_gpu_backend",
        "app_resident_gpu_backend",
    ];
    let items: Vec<_> = required_evidence
        .iter()
        .map(|evidence_type| {
            json!({
                "evidence_type": evidence_type,
                "status": "passed"
            })
        })
        .collect();

    json!({
        "schema": "mirante4d-verify-render-report",
        "command": "verify-render",
        "status": "passed",
        "requirements": {
            "requires_non_cpu_gpu": true,
            "fails_without_adapter": true,
            "evidence_must_include_pixels_or_resources": true,
            "product_device_limit_coverage": true,
            "display_texture_coverage": true,
            "app_backend_coverage": true,
            "timestamp_capability_reported": true
        },
        "gpu_adapter": {
            "name": "unit gpu",
            "backend": "Vulkan",
            "device_type": "DiscreteGpu",
            "timestamp_queries_supported": true
        },
        "tests": {
            "test_count": items.len(),
            "passed": items.len(),
            "failed": 0,
            "items": items
        }
    })
}

pub(super) fn current_ui_report_spec() -> CurrentEvidencePath {
    CurrentEvidencePath {
        relative_path: "verify-ui/verify-ui-report.json",
        family: "verify",
        evidence_role: "current_ui_report",
        expected_schema: Some("mirante4d-verify-ui-report"),
        expected_command: Some("verify-ui"),
        expected_scenario: None,
        notes: "",
    }
}

pub(super) fn current_command_audit_report_spec() -> CurrentEvidencePath {
    CurrentEvidencePath {
        relative_path: "command-audit/command-audit-report.json",
        family: "command_surface",
        evidence_role: "current_command_audit_report",
        expected_schema: Some("mirante4d-xtask-command-audit"),
        expected_command: Some("command-audit"),
        expected_scenario: None,
        notes: "",
    }
}

pub(super) fn current_baseline_audit_report_spec() -> CurrentEvidencePath {
    CurrentEvidencePath {
        relative_path: "baseline-audit/baseline-audit-report.json",
        family: "baseline_surface",
        evidence_role: "current_baseline_audit_report",
        expected_schema: Some("mirante4d-baseline-audit"),
        expected_command: Some("baseline-audit"),
        expected_scenario: None,
        notes: "",
    }
}

pub(super) fn current_baseline_refresh_plan_report_spec() -> CurrentEvidencePath {
    CurrentEvidencePath {
        relative_path: "baseline-refresh/baseline-refresh-plan.json",
        family: "baseline_surface",
        evidence_role: "current_baseline_refresh_plan_report",
        expected_schema: Some("mirante4d-baseline-refresh-plan"),
        expected_command: Some("baseline-refresh-plan"),
        expected_scenario: None,
        notes: "",
    }
}

pub(super) fn current_workflow_audit_report_spec() -> CurrentEvidencePath {
    CurrentEvidencePath {
        relative_path: "workflow-audit/workflow-audit-report.json",
        family: "ci_surface",
        evidence_role: "current_workflow_audit_report",
        expected_schema: Some("mirante4d-workflow-audit"),
        expected_command: Some("workflow-audit"),
        expected_scenario: None,
        notes: "",
    }
}

pub(super) fn current_external_ci_evidence_report_spec() -> CurrentEvidencePath {
    CurrentEvidencePath {
        relative_path: EXTERNAL_CI_EVIDENCE_RELATIVE_PATH,
        family: "ci_surface",
        evidence_role: "current_external_ci_run_evidence",
        expected_schema: Some(EXTERNAL_CI_EVIDENCE_SCHEMA),
        expected_command: Some("external-ci-evidence"),
        expected_scenario: None,
        notes: "",
    }
}

pub(super) fn current_e2e_report_spec() -> CurrentEvidencePath {
    CurrentEvidencePath {
        relative_path: "verify-e2e/verify-e2e-report.json",
        family: "verify",
        evidence_role: "current_e2e_report",
        expected_schema: Some("mirante4d-verify-e2e-report"),
        expected_command: Some("verify-e2e"),
        expected_scenario: None,
        notes: "",
    }
}

pub(super) fn current_verify_render_report_spec() -> CurrentEvidencePath {
    CurrentEvidencePath {
        relative_path: "verify-render/verify-render-report.json",
        family: "verify",
        evidence_role: "current_gpu_render_report",
        expected_schema: Some("mirante4d-verify-render-report"),
        expected_command: Some("verify-render"),
        expected_scenario: None,
        notes: "",
    }
}

pub(super) fn current_generated_fixture_camera_product_spec() -> CurrentEvidencePath {
    CurrentEvidencePath {
        relative_path: "product-validation/generated_fixture_camera_smoke/product-validation-report.json",
        family: "product_validation",
        evidence_role: "current_generated_fixture_camera_product_report",
        expected_schema: Some("mirante4d-product-validation-report"),
        expected_command: Some("product-validate"),
        expected_scenario: Some("generated_fixture_camera_smoke"),
        notes: "",
    }
}

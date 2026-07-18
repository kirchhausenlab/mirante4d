use super::capture::{
    ProductAutomationArtifact, ProductAutomationImageStats, color_image_from_rgba,
    sanitize_artifact_label, write_color_image_ppm,
};
use super::diagnostics::{dataset_runtime_diagnostics_json, local_dataset_source_diagnostics_json};
use super::*;
use std::{fs, path::PathBuf};

use mirante4d_dataset_runtime::{DatasetRuntimeConfig, DatasetRuntimeDiagnostics};

#[test]
fn freshness_wait_never_certifies_a_stale_exact_frame() {
    let mut fidelity = crate::FrameFidelityStatus::new_with_presentation(
        mirante4d_render_api::RenderExtent::new(1920, 1080).unwrap(),
        mirante4d_render_api::PresentationViewport::new(1920.0, 1080.0).unwrap(),
    );
    fidelity.completeness = crate::FrameCompleteness::Exact;
    fidelity.display_freshness = crate::DisplayedFrameFreshness::Stale;
    assert!(!frame_freshness_is_current(&fidelity));

    fidelity.display_freshness = crate::DisplayedFrameFreshness::Current;
    assert!(frame_freshness_is_current(&fidelity));
}

#[test]
fn runtime_idle_wait_includes_async_camera_demand_planning_and_install() {
    assert!(automation_runtime_is_idle(false, false, false));
    assert!(!automation_runtime_is_idle(true, false, false));
    assert!(!automation_runtime_is_idle(false, true, false));
    assert!(!automation_runtime_is_idle(false, false, true));
}

#[test]
fn schema_v5_strictly_parses_observe_gate_batch_and_rejects_v4_commands() {
    let raw = json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": 5,
        "hard_safety_limits": {},
        "scenario": "gate_protocol",
        "commands": [
            { "command": "set_viewport_size", "width": 1, "height": 1 },
            {
                "command": "observe_gate_batch",
                "batch_id": "RZ.resident_3d",
                "phase_id": "resident_3d",
                "origin": { "kind": "command_completed", "command_index": 0 },
                "observations": [{
                    "gate_id": "RZ.resident_3d-settled",
                    "deadline_authority": "maximum_current_presentation_gap_plus_poll_grace",
                    "deadline_after_origin_ns": MAX_GATE_DEADLINE_AFTER_ORIGIN_NS,
                    "target": {
                        "kind": "condition",
                        "condition": "coordinated_presentation_settled"
                    }
                }]
            },
            { "command": "quit" }
        ]
    });
    let script: ProductAutomationScript = serde_json::from_value(raw.clone()).unwrap();
    script.validate().unwrap();
    assert_eq!(AUTOMATION_SCHEMA_VERSION, 5);
    assert!(matches!(
        &script.commands[1],
        ProductAutomationCommand::ObserveGateBatch {
            batch_id,
            phase_id,
            origin: ProductAutomationGateBatchOrigin::CommandCompleted { command_index: 0 },
            observations,
        } if batch_id == "RZ.resident_3d"
            && phase_id == "resident_3d"
            && observations[0].gate_id == "RZ.resident_3d-settled"
            && observations[0].target.condition_name() == "coordinated_presentation_settled"
            && observations[0].deadline_after_origin_ns == MAX_GATE_DEADLINE_AFTER_ORIGIN_NS
    ));

    let mut legacy = raw;
    legacy["schema_version"] = json!(4);
    let legacy: ProductAutomationScript = serde_json::from_value(legacy).unwrap();
    assert!(
        legacy
            .validate()
            .unwrap_err()
            .to_string()
            .contains("expected 5")
    );

    let predecessor_command = json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "v4_is_not_a_compatibility_input",
        "commands": [{
            "command": "observe_gate",
            "gate_id": "RZ.settled",
            "condition": "coordinated_presentation_settled",
            "timeout_ms": 1
        }]
    });
    assert!(serde_json::from_value::<ProductAutomationScript>(predecessor_command).is_err());
}

#[test]
fn schema_v5_requires_exact_hard_safety_limits_without_legacy_alias() {
    let script_without_limits = json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "scenario": "missing_hard_safety_limits",
        "commands": [{ "command": "quit" }]
    });
    assert!(serde_json::from_value::<ProductAutomationScript>(script_without_limits).is_err());

    let legacy_limits = json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "scenario": "legacy_limits_are_not_an_alias",
        "limits": {},
        "commands": [{ "command": "quit" }]
    });
    assert!(serde_json::from_value::<ProductAutomationScript>(legacy_limits).is_err());

    let exact = json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "scenario": "exact_hard_safety_limits",
        "hard_safety_limits": {},
        "commands": [{ "command": "quit" }]
    });
    serde_json::from_value::<ProductAutomationScript>(exact)
        .unwrap()
        .validate()
        .unwrap();
}

#[test]
fn observe_gate_batch_validates_safe_bounded_identity_and_deadline() {
    assert_eq!(MAX_GATE_DEADLINE_AFTER_ORIGIN_NS, 7_200_000_000_000);
    for (batch_id, phase_id, gate_id, deadline_after_origin_ns) in [
        (String::new(), "phase".to_owned(), "gate".to_owned(), 1),
        ("batch".to_owned(), "a".repeat(129), "gate".to_owned(), 1),
        (
            "batch".to_owned(),
            "phase".to_owned(),
            "contains space".to_owned(),
            1,
        ),
        (
            "batch".to_owned(),
            "phase".to_owned(),
            "non-ascii-é".to_owned(),
            1,
        ),
        (
            "batch".to_owned(),
            "phase".to_owned(),
            "valid".to_owned(),
            0,
        ),
        (
            "batch".to_owned(),
            "phase".to_owned(),
            "valid".to_owned(),
            MAX_GATE_DEADLINE_AFTER_ORIGIN_NS + 1,
        ),
    ] {
        let script: ProductAutomationScript = serde_json::from_value(json!({
            "schema": AUTOMATION_SCRIPT_SCHEMA,
            "schema_version": AUTOMATION_SCHEMA_VERSION,
            "hard_safety_limits": {},
            "scenario": "invalid_gate_protocol",
            "commands": [{
                "command": "observe_gate_batch",
                "batch_id": batch_id,
                "phase_id": phase_id,
                "origin": { "kind": "automation_started" },
                "observations": [{
                    "gate_id": gate_id,
                    "deadline_authority": "cold_target_settlement",
                    "deadline_after_origin_ns": deadline_after_origin_ns,
                    "target": { "kind": "condition", "condition": "runtime_idle" }
                }]
            }]
        }))
        .unwrap();
        assert!(script.validate().is_err());
    }

    for (gate_id, deadline_after_origin_ns) in [
        ("x".to_owned(), 1),
        ("x".repeat(128), MAX_GATE_DEADLINE_AFTER_ORIGIN_NS),
    ] {
        let script: ProductAutomationScript = serde_json::from_value(json!({
            "schema": AUTOMATION_SCRIPT_SCHEMA,
            "schema_version": AUTOMATION_SCHEMA_VERSION,
            "hard_safety_limits": {},
            "scenario": "valid_gate_protocol_boundaries",
            "commands": [{
                "command": "observe_gate_batch",
                "batch_id": "batch",
                "phase_id": "phase",
                "origin": { "kind": "automation_started" },
                "observations": [{
                    "gate_id": gate_id,
                    "deadline_authority": "cold_target_settlement",
                    "deadline_after_origin_ns": deadline_after_origin_ns,
                    "target": { "kind": "condition", "condition": "runtime_idle" }
                }]
            }]
        }))
        .unwrap();
        script.validate().unwrap();
    }
}

#[test]
fn observe_gate_batch_requires_bounded_unique_script_identities_and_past_origins() {
    let observation = |gate_id: &str| {
        json!({
            "gate_id": gate_id,
            "deadline_authority": "cold_target_settlement",
            "deadline_after_origin_ns": 1,
            "target": { "kind": "condition", "condition": "runtime_idle" }
        })
    };
    let batch = |batch_id: &str, gate_id: &str| {
        json!({
            "command": "observe_gate_batch",
            "batch_id": batch_id,
            "phase_id": "phase",
            "origin": { "kind": "automation_started" },
            "observations": [observation(gate_id)]
        })
    };

    for commands in [
        vec![batch("batch", "gate-a"), batch("batch", "gate-b")],
        vec![batch("batch-a", "gate"), batch("batch-b", "gate")],
        vec![json!({
            "command": "observe_gate_batch",
            "batch_id": "future-origin",
            "phase_id": "phase",
            "origin": { "kind": "command_completed", "command_index": 0 },
            "observations": [observation("gate")]
        })],
    ] {
        let script: ProductAutomationScript = serde_json::from_value(json!({
            "schema": AUTOMATION_SCRIPT_SCHEMA,
            "schema_version": AUTOMATION_SCHEMA_VERSION,
            "hard_safety_limits": {},
            "scenario": "invalid_batch_identity",
            "commands": commands,
        }))
        .unwrap();
        assert!(script.validate().is_err());
    }

    let observations = (0..=MAX_GATE_BATCH_OBSERVATIONS)
        .map(|index| observation(&format!("gate-{index}")))
        .collect::<Vec<_>>();
    let script: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "too_many_batch_observations",
        "commands": [{
            "command": "observe_gate_batch",
            "batch_id": "batch",
            "phase_id": "phase",
            "origin": { "kind": "automation_started" },
            "observations": observations,
        }],
    }))
    .unwrap();
    assert!(script.validate().is_err());
}

#[test]
fn gate_deadline_authorities_require_their_closed_origin_class() {
    let condition = |gate_id: &str, deadline_authority: &str| {
        json!({
            "gate_id": gate_id,
            "deadline_authority": deadline_authority,
            "deadline_after_origin_ns": 1,
            "target": { "kind": "condition", "condition": "runtime_idle" }
        })
    };
    let valid: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "closed_origin_classes",
        "commands": [
            {
                "command": "observe_gate_batch",
                "batch_id": "cold",
                "phase_id": "cold",
                "origin": { "kind": "automation_started" },
                "observations": [
                    condition("cold-first", "cold_first_useful"),
                    condition("cold-coarse", "cold_complete_coarse"),
                    condition("cold-target", "cold_target_settlement")
                ]
            },
            { "command": "set_viewport_size", "width": 1, "height": 1 },
            {
                "command": "observe_gate_batch",
                "batch_id": "resident",
                "phase_id": "resident",
                "origin": { "kind": "command_completed", "command_index": 1 },
                "observations": [
                    condition("resident-gap", "maximum_current_presentation_gap_plus_poll_grace"),
                    condition("nonresident-target", "nonresident_target_settlement"),
                    condition("verification", "source_verification_completion")
                ]
            },
            {
                "command": "observe_gate_batch",
                "batch_id": "import",
                "phase_id": "import",
                "origin": { "kind": "import_primary_started" },
                "observations": [condition("import-wall", "import_primary_wall")]
            }
        ]
    }))
    .unwrap();
    valid.validate().unwrap();

    let invalid: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "mismatched_origin_class",
        "commands": [{
            "command": "observe_gate_batch",
            "batch_id": "invalid",
            "phase_id": "invalid",
            "origin": { "kind": "automation_started" },
            "observations": [condition(
                "resident-with-cold-origin",
                "maximum_current_presentation_gap_plus_poll_grace"
            )]
        }]
    }))
    .unwrap();
    assert!(invalid.validate().is_err());
}

#[test]
fn gate_batch_origin_resolution_uses_only_the_exact_declared_instant() {
    let automation_started_at = Instant::now();
    let command_completed_at = automation_started_at + Duration::from_nanos(11);
    let import_primary_started_at = automation_started_at + Duration::from_nanos(23);
    let completions = [Some(command_completed_at), None];

    assert_eq!(
        resolve_product_gate_batch_origin_at(
            ProductAutomationGateBatchOrigin::AutomationStarted,
            automation_started_at,
            &completions,
            Some(import_primary_started_at),
        )
        .unwrap(),
        automation_started_at
    );
    assert_eq!(
        resolve_product_gate_batch_origin_at(
            ProductAutomationGateBatchOrigin::CommandCompleted { command_index: 0 },
            automation_started_at,
            &completions,
            Some(import_primary_started_at),
        )
        .unwrap(),
        command_completed_at
    );
    assert!(
        resolve_product_gate_batch_origin_at(
            ProductAutomationGateBatchOrigin::CommandCompleted { command_index: 1 },
            automation_started_at,
            &completions,
            Some(import_primary_started_at),
        )
        .is_err()
    );
    assert_eq!(
        resolve_product_gate_batch_origin_at(
            ProductAutomationGateBatchOrigin::ImportPrimaryStarted,
            automation_started_at,
            &completions,
            Some(import_primary_started_at),
        )
        .unwrap(),
        import_primary_started_at
    );
    assert!(
        resolve_product_gate_batch_origin_at(
            ProductAutomationGateBatchOrigin::ImportPrimaryStarted,
            automation_started_at,
            &completions,
            None,
        )
        .is_err()
    );

    let script: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "recorded_command_origin",
        "commands": [{ "command": "quit" }, { "command": "quit" }]
    }))
    .unwrap();
    script.validate().unwrap();
    let mut controller = ProductAutomationController::new(
        script,
        PathBuf::from("gate-script.json"),
        PathBuf::from("gate-report.json"),
    );
    assert!(
        controller
            .product_gate_batch_origin_at(ProductAutomationGateBatchOrigin::CommandCompleted {
                command_index: 0
            })
            .is_err()
    );
    controller.record_successful_command(0, "quit", Duration::ZERO, json!({}));
    let recorded = controller.command_completed_at[0].unwrap();
    assert_eq!(
        controller
            .product_gate_batch_origin_at(ProductAutomationGateBatchOrigin::CommandCompleted {
                command_index: 0
            })
            .unwrap(),
        recorded
    );
}

#[test]
fn observe_gate_batch_latches_concurrently_and_completes_at_the_maximum_deadline() {
    let observations = vec![
        ProductAutomationGateObservation {
            gate_id: "first".to_owned(),
            deadline_authority:
                ProductAutomationGateDeadlineAuthority::MaximumCurrentPresentationGapPlusPollGrace,
            deadline_after_origin_ns: 10,
            target: ProductAutomationGateTarget::Condition {
                condition: ProductAutomationWaitCondition::RuntimeIdle,
            },
        },
        ProductAutomationGateObservation {
            gate_id: "second".to_owned(),
            deadline_authority: ProductAutomationGateDeadlineAuthority::NonresidentTargetSettlement,
            deadline_after_origin_ns: 20,
            target: ProductAutomationGateTarget::Condition {
                condition: ProductAutomationWaitCondition::CoordinatedPresentationSettled,
            },
        },
    ];
    let mut outcomes = vec![None; observations.len()];

    assert!(
        !latch_product_gate_batch_observations(&observations, &[false, false], 9, &mut outcomes,)
            .unwrap()
    );
    assert_eq!(outcomes, [None, None]);

    assert!(
        !latch_product_gate_batch_observations(&observations, &[false, false], 10, &mut outcomes,)
            .unwrap()
    );
    assert_eq!(
        outcomes[0],
        Some(LatchedProductGateObservation {
            outcome: ProductGateObservationOutcome::Failed,
            condition_met: false,
            timed_out: true,
            observed_after_origin_ns: 10,
        })
    );
    assert_eq!(outcomes[1], None);

    assert!(
        latch_product_gate_batch_observations(&observations, &[true, true], 19, &mut outcomes,)
            .unwrap()
    );
    assert_eq!(outcomes[0].unwrap().observed_after_origin_ns, 10);
    assert_eq!(
        outcomes[1].unwrap().outcome,
        ProductGateObservationOutcome::Passed
    );
    assert_eq!(outcomes[1].unwrap().observed_after_origin_ns, 19);
    let serial_deadline_sum = observations
        .iter()
        .map(|observation| observation.deadline_after_origin_ns)
        .sum::<u64>();
    assert!(
        outcomes[1].unwrap().observed_after_origin_ns < serial_deadline_sum,
        "batch time is max-like, never a serial sum"
    );
}

#[test]
fn observe_gate_batch_deadline_wins_at_equality_and_records_late_true() {
    let observation = ProductAutomationGateObservation {
        gate_id: "runtime.ready".to_owned(),
        deadline_authority:
            ProductAutomationGateDeadlineAuthority::MaximumCurrentPresentationGapPlusPollGrace,
        deadline_after_origin_ns: 100,
        target: ProductAutomationGateTarget::Condition {
            condition: ProductAutomationWaitCondition::RuntimeIdle,
        },
    };
    assert_eq!(
        latch_product_gate_observation(&observation, true, 99),
        Some(LatchedProductGateObservation {
            outcome: ProductGateObservationOutcome::Passed,
            condition_met: true,
            timed_out: false,
            observed_after_origin_ns: 99,
        })
    );
    assert_eq!(
        latch_product_gate_observation(&observation, true, 100),
        Some(LatchedProductGateObservation {
            outcome: ProductGateObservationOutcome::Failed,
            condition_met: true,
            timed_out: true,
            observed_after_origin_ns: 100,
        })
    );
}

#[test]
fn failed_gate_batch_is_a_successful_report_event_and_continues() {
    let script: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "gate_continue",
        "commands": [
            { "command": "set_viewport_size", "width": 1, "height": 1 },
            {
                "command": "observe_gate_batch",
                "batch_id": "RZ.resident_3d",
                "phase_id": "resident_3d",
                "origin": { "kind": "command_completed", "command_index": 0 },
                "observations": [{
                    "gate_id": "RZ.resident_3d-settled",
                    "deadline_authority": "maximum_current_presentation_gap_plus_poll_grace",
                    "deadline_after_origin_ns": 10,
                    "target": {
                        "kind": "condition",
                        "condition": "coordinated_presentation_settled"
                    }
                }]
            },
            { "command": "quit" }
        ]
    }))
    .unwrap();
    script.validate().unwrap();
    let mut controller = ProductAutomationController::new(
        script,
        PathBuf::from("gate-script.json"),
        PathBuf::from("gate-report.json"),
    );
    controller.record_successful_command(0, "set_viewport_size", Duration::ZERO, json!({}));
    let details = serde_json::to_value(ProductGateBatchDetails {
        schema: AUTOMATION_GATE_BATCH_OBSERVATION_SCHEMA,
        batch_id: "RZ.resident_3d",
        phase_id: "resident_3d",
        origin: ProductAutomationGateBatchOrigin::CommandCompleted { command_index: 0 },
        completed_after_origin_ns: 10,
        observations: vec![ProductGateBatchObservationDetails {
            observation_index: 0,
            gate_id: "RZ.resident_3d-settled",
            condition: "coordinated_presentation_settled",
            deadline_authority:
                ProductAutomationGateDeadlineAuthority::MaximumCurrentPresentationGapPlusPollGrace,
            deadline_after_origin_ns: 10,
            outcome: ProductGateObservationOutcome::Failed,
            condition_met: false,
            timed_out: true,
            observed_after_origin_ns: 10,
        }],
    })
    .unwrap();

    controller.record_successful_command(
        1,
        "observe_gate_batch",
        Duration::from_nanos(10),
        details,
    );

    assert_eq!(controller.command_index, 2);
    assert!(controller.active_wait_started.is_none());
    assert_eq!(controller.events.len(), 2);
    let report_events = serde_json::to_value(&controller.events).unwrap();
    assert_eq!(report_events[1]["command_index"], 1);
    assert_eq!(report_events[1]["command"], "observe_gate_batch");
    assert_eq!(report_events[1]["status"], "passed");
    assert_eq!(
        report_events[1]["details"]["schema"],
        "mirante4d-product-gate-batch-observation-1"
    );
    assert_eq!(
        report_events[1]["details"]["observations"][0]["outcome"],
        "failed"
    );
    assert_eq!(
        report_events[1]["details"]["observations"][0]["timed_out"],
        true
    );

    controller.record_successful_command(2, "quit", Duration::ZERO, json!({}));
    assert_eq!(controller.command_index, 3);
    assert!(
        controller
            .events
            .iter()
            .all(|event| event.status == "passed")
    );
}

#[test]
fn schema_v5_strictly_parses_imported_open_ready_batch_target() {
    let raw = json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "ip_acceptance_gate",
        "commands": [
            {
                "command": "observe_gate_batch",
                "batch_id": "IP.primary",
                "phase_id": "primary",
                "origin": { "kind": "import_primary_started" },
                "observations": [{
                    "gate_id": "IP.imported-open-ready",
                    "deadline_authority": "import_primary_wall",
                    "deadline_after_origin_ns": 7_200_000_000_000_u64,
                    "target": {
                        "kind": "imported_open_ready",
                        "path": "/tmp/test-published.m4d"
                    }
                }]
            },
            { "command": "quit" }
        ]
    });
    let script: ProductAutomationScript = serde_json::from_value(raw).unwrap();
    script.validate().unwrap();
    assert!(matches!(
        &script.commands[0],
        ProductAutomationCommand::ObserveGateBatch { observations, .. }
            if matches!(
                &observations[0].target,
                ProductAutomationGateTarget::ImportedOpenReady { path }
                    if path == &PathBuf::from("/tmp/test-published.m4d")
            )
    ));
    assert_eq!(script.commands[0].name(), "observe_gate_batch");

    for target in [
        json!({
            "kind": "imported_open_ready",
            "path": "/tmp/test-published.m4d",
            "unexpected": true
        }),
        json!({ "kind": "imported_open_ready" }),
    ] {
        let candidate = json!({
            "schema": AUTOMATION_SCRIPT_SCHEMA,
            "schema_version": AUTOMATION_SCHEMA_VERSION,
            "hard_safety_limits": {},
            "scenario": "strict_ip_acceptance_gate",
            "commands": [{
                "command": "observe_gate_batch",
                "batch_id": "IP.primary",
                "phase_id": "primary",
                "origin": { "kind": "import_primary_started" },
                "observations": [{
                    "gate_id": "IP.valid",
                    "deadline_authority": "import_primary_wall",
                    "deadline_after_origin_ns": 1,
                    "target": target
                }]
            }]
        });
        assert!(serde_json::from_value::<ProductAutomationScript>(candidate).is_err());
    }
}

#[test]
fn imported_open_ready_batch_target_validates_path_identity_and_deadline() {
    for (gate_id, path, deadline_after_origin_ns) in [
        ("".to_owned(), "/tmp/test-published.m4d".to_owned(), 1),
        (
            "contains space".to_owned(),
            "/tmp/test-published.m4d".to_owned(),
            1,
        ),
        ("IP.valid".to_owned(), String::new(), 1),
        (
            "IP.valid".to_owned(),
            "/tmp/test-published.m4d".to_owned(),
            0,
        ),
        (
            "IP.valid".to_owned(),
            "/tmp/test-published.m4d".to_owned(),
            MAX_GATE_DEADLINE_AFTER_ORIGIN_NS + 1,
        ),
    ] {
        let script: ProductAutomationScript = serde_json::from_value(json!({
            "schema": AUTOMATION_SCRIPT_SCHEMA,
            "schema_version": AUTOMATION_SCHEMA_VERSION,
            "hard_safety_limits": {},
            "scenario": "invalid_ip_acceptance_gate",
            "commands": [{
                "command": "observe_gate_batch",
                "batch_id": "IP.primary",
                "phase_id": "primary",
                "origin": { "kind": "import_primary_started" },
                "observations": [{
                    "gate_id": gate_id,
                    "deadline_authority": "import_primary_wall",
                    "deadline_after_origin_ns": deadline_after_origin_ns,
                    "target": { "kind": "imported_open_ready", "path": path }
                }]
            }]
        }))
        .unwrap();
        assert!(script.validate().is_err());
    }

    let wrong_authority: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "imported_open_ready_wrong_authority",
        "commands": [{
            "command": "observe_gate_batch",
            "batch_id": "IP.wrong-authority",
            "phase_id": "wrong-authority",
            "origin": { "kind": "automation_started" },
            "observations": [{
                "gate_id": "IP.imported-open-ready-wrong-authority",
                "deadline_authority": "cold_target_settlement",
                "deadline_after_origin_ns": 1,
                "target": {
                    "kind": "imported_open_ready",
                    "path": "/tmp/test-published.m4d"
                }
            }]
        }]
    }))
    .unwrap();
    assert!(wrong_authority.validate().is_err());
}

#[test]
fn imported_open_ready_batch_target_reports_typed_pass_and_timeout() {
    let observation = ProductAutomationGateObservation {
        gate_id: "IP.imported-open-ready".to_owned(),
        deadline_authority: ProductAutomationGateDeadlineAuthority::ImportPrimaryWall,
        deadline_after_origin_ns: 100,
        target: ProductAutomationGateTarget::ImportedOpenReady {
            path: PathBuf::from("/tmp/test-published.m4d"),
        },
    };
    assert_eq!(
        latch_product_gate_observation(&observation, false, 99),
        None
    );
    assert_eq!(
        latch_product_gate_observation(&observation, true, 99),
        Some(LatchedProductGateObservation {
            outcome: ProductGateObservationOutcome::Passed,
            condition_met: true,
            timed_out: false,
            observed_after_origin_ns: 99,
        })
    );
    assert_eq!(
        latch_product_gate_observation(&observation, true, 100),
        Some(LatchedProductGateObservation {
            outcome: ProductGateObservationOutcome::Failed,
            condition_met: true,
            timed_out: true,
            observed_after_origin_ns: 100,
        })
    );
    assert_eq!(
        latch_product_gate_observation(&observation, false, 100),
        Some(LatchedProductGateObservation {
            outcome: ProductGateObservationOutcome::Failed,
            condition_met: false,
            timed_out: true,
            observed_after_origin_ns: 100,
        })
    );
}

#[test]
fn imported_open_ready_full_measurement_is_admitted_only_by_a_latched_pass() {
    let observation = ProductAutomationGateObservation {
        gate_id: "IP.imported-open-ready".to_owned(),
        deadline_authority: ProductAutomationGateDeadlineAuthority::ImportPrimaryWall,
        deadline_after_origin_ns: 100,
        target: ProductAutomationGateTarget::ImportedOpenReady {
            path: PathBuf::from("/tmp/test-published.m4d"),
        },
    };
    assert_eq!(
        latch_product_gate_observation(&observation, true, 99).map(|outcome| outcome.outcome),
        Some(ProductGateObservationOutcome::Passed)
    );
    assert_eq!(
        latch_product_gate_observation(&observation, true, 100).map(|outcome| outcome.outcome),
        Some(ProductGateObservationOutcome::Failed)
    );
    assert_eq!(
        latch_product_gate_observation(&observation, false, 99),
        None
    );
}

#[test]
fn imported_open_ready_reuses_every_existing_readiness_fact() {
    assert!(
        ImportedOpenReadyReadiness {
            selected_matches: true,
            verified: true,
            import_idle: true,
            problem_absent: true,
        }
        .condition_met()
    );
    for readiness in [
        ImportedOpenReadyReadiness {
            selected_matches: false,
            verified: true,
            import_idle: true,
            problem_absent: true,
        },
        ImportedOpenReadyReadiness {
            selected_matches: true,
            verified: false,
            import_idle: true,
            problem_absent: true,
        },
        ImportedOpenReadyReadiness {
            selected_matches: true,
            verified: true,
            import_idle: false,
            problem_absent: true,
        },
        ImportedOpenReadyReadiness {
            selected_matches: true,
            verified: true,
            import_idle: true,
            problem_absent: false,
        },
    ] {
        assert!(!readiness.condition_met());
    }
}

#[test]
fn imported_open_ready_measurement_commit_preserves_all_side_effects() {
    let primary = ImportPrimaryMeasurement {
        started_at_epoch_ms: 11,
        open_ready_at_epoch_ms: 23,
        wall_time_ns: 29,
        process_cpu_time_ns: 31,
    };
    let mut active_origin = Some("exact-worker-origin");
    let mut active_verification_origin = Some("exact-verification-origin");
    let mut completed_primary = None;
    let mut completed_publication = None;
    let mut captured_publication_evidence = None;

    ImportedOpenReadyCommitState {
        active_origin: &mut active_origin,
        active_verification_origin: &mut active_verification_origin,
        completed_primary: &mut completed_primary,
        completed_publication: &mut completed_publication,
        captured_publication_evidence: &mut captured_publication_evidence,
    }
    .commit(
        primary,
        "publication-to-open-ready-measurement",
        "publication-currentness-and-verifier-evidence",
    );

    assert!(active_origin.is_none());
    assert!(active_verification_origin.is_none());
    assert_eq!(completed_primary, Some(primary));
    assert_eq!(
        completed_publication,
        Some("publication-to-open-ready-measurement")
    );
    assert_eq!(
        captured_publication_evidence,
        Some("publication-currentness-and-verifier-evidence")
    );
}

fn test_import_publication_evidence_snapshot() -> ImportPublicationEvidenceSnapshot {
    ImportPublicationEvidenceSnapshot {
        publication_currentness: ImportPublicationCurrentnessExecution {
            contract_id: "test-publication-currentness",
            expected_snapshot_object_reads: 1,
            first_inventory_object_reads: 2,
            observed_snapshot_object_reads: 3,
            second_inventory_object_reads: 4,
            observed_total_object_reads: 10,
            observed_codec_decode_calls: 5,
        },
        source_verification_started_runs: 6,
        source_verification_progress_updates: 7,
        source_verification_cancelled_runs: 8,
        source_verification_failed_runs: 9,
        source_verification_successes: 10,
    }
}

#[test]
fn failed_imported_open_ready_serializes_only_authentic_currentness_and_verifier_evidence() {
    let evidence = test_import_publication_evidence_snapshot();

    assert_eq!(
        import_publication_to_open_ready_measurement_json(None, None),
        Value::Null
    );
    let partial = import_publication_to_open_ready_measurement_json(None, Some(evidence));

    assert_eq!(
        partial,
        json!({
            "publication_currentness_execution": {
                "contract_id": "test-publication-currentness",
                "expected_snapshot_object_reads": 1,
                "first_inventory_object_reads": 2,
                "observed_snapshot_object_reads": 3,
                "second_inventory_object_reads": 4,
                "observed_total_object_reads": 10,
                "observed_codec_decode_calls": 5,
            },
            "source_verification_started_runs": 6,
            "source_verification_progress_updates": 7,
            "source_verification_cancelled_runs": 8,
            "source_verification_failed_runs": 9,
            "source_verification_successes": 10,
        })
    );
    assert_eq!(partial.as_object().map(serde_json::Map::len), Some(6));
    for pass_only in [
        "start_boundary",
        "end_boundary",
        "wall_clock",
        "cpu_clock",
        "published_at_epoch_ms",
        "open_ready_at_epoch_ms",
        "wall_time_ns",
        "process_cpu_time_ns",
        "included_in_primary_clock",
        "transfer_mode",
    ] {
        assert!(partial.get(pass_only).is_none());
    }
}

#[test]
fn passed_imported_open_ready_adds_full_clocks_to_the_same_publication_evidence() {
    let evidence = test_import_publication_evidence_snapshot();
    let timing = ImportPublicationToOpenReadyMeasurement {
        published_at_epoch_ms: 11,
        open_ready_at_epoch_ms: 12,
        wall_time_ns: 13,
        process_cpu_time_ns: 14,
    };

    let full = import_publication_to_open_ready_measurement_json(Some(timing), Some(evidence));

    assert_eq!(
        full,
        json!({
            "start_boundary": "import_worker_published_event",
            "end_boundary": "published_destination_verified_and_open_ready_for_normal_product_use",
            "wall_clock": "std_instant_monotonic",
            "cpu_clock": "process_cpu_time",
            "published_at_epoch_ms": 11,
            "open_ready_at_epoch_ms": 12,
            "wall_time_ns": 13,
            "process_cpu_time_ns": 14,
            "included_in_primary_clock": true,
            "transfer_mode": "staged_verified_capability",
            "publication_currentness_execution": {
                "contract_id": "test-publication-currentness",
                "expected_snapshot_object_reads": 1,
                "first_inventory_object_reads": 2,
                "observed_snapshot_object_reads": 3,
                "second_inventory_object_reads": 4,
                "observed_total_object_reads": 10,
                "observed_codec_decode_calls": 5,
            },
            "source_verification_started_runs": 6,
            "source_verification_progress_updates": 7,
            "source_verification_cancelled_runs": 8,
            "source_verification_failed_runs": 9,
            "source_verification_successes": 10,
        })
    );
    assert_eq!(full.as_object().map(serde_json::Map::len), Some(16));
}

#[test]
fn failed_imported_open_ready_batch_continues_without_serializing_path() {
    let private_path_marker = "/tmp/sensitive-path-marker/published.m4d";
    let script: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "ip_gate_continue",
        "commands": [
            {
                "command": "observe_gate_batch",
                "batch_id": "IP.primary",
                "phase_id": "primary",
                "origin": { "kind": "import_primary_started" },
                "observations": [{
                    "gate_id": "IP.imported-open-ready",
                    "deadline_authority": "import_primary_wall",
                    "deadline_after_origin_ns": 10,
                    "target": {
                        "kind": "imported_open_ready",
                        "path": private_path_marker
                    }
                }]
            },
            { "command": "quit" }
        ]
    }))
    .unwrap();
    script.validate().unwrap();
    let mut controller = ProductAutomationController::new(
        script,
        PathBuf::from("gate-script.json"),
        PathBuf::from("gate-report.json"),
    );
    let observations = match &controller.script.commands[0] {
        ProductAutomationCommand::ObserveGateBatch { observations, .. } => observations.clone(),
        _ => panic!("test script must begin with its imported-open-ready batch"),
    };
    let outcome = latch_product_gate_observation(&observations[0], false, 10)
        .expect("the imported-open-ready deadline must latch a failed outcome");
    let details = product_gate_batch_details_value(
        "IP.primary",
        "primary",
        ProductAutomationGateBatchOrigin::ImportPrimaryStarted,
        10,
        &observations,
        &[Some(outcome)],
    )
    .unwrap();
    assert_eq!(details["observations"][0]["outcome"], "failed");
    assert_eq!(details["observations"][0]["timed_out"], true);
    assert_eq!(import_primary_measurement_json(None), Value::Null);
    assert_eq!(
        import_publication_to_open_ready_measurement_json(None, None),
        Value::Null
    );
    assert!(
        authentic_failed_import_publication_evidence(Err("not published".to_owned())).is_none()
    );
    let partial = authentic_failed_import_publication_evidence(Ok(
        test_import_publication_evidence_snapshot(),
    ))
    .expect("authentic currentness evidence must remain available on a failed deadline");
    let partial = import_publication_to_open_ready_measurement_json(None, Some(partial));
    assert_eq!(partial.as_object().map(serde_json::Map::len), Some(6));

    controller.record_successful_command(
        0,
        "observe_gate_batch",
        Duration::from_nanos(10),
        details,
    );

    assert_eq!(controller.command_index, 1);
    assert!(controller.active_wait_started.is_none());
    let report_events = serde_json::to_value(&controller.events).unwrap();
    assert_eq!(report_events[0]["status"], "passed");
    assert_eq!(
        report_events[0]["details"]["observations"][0]["outcome"],
        "failed"
    );
    assert!(
        !serde_json::to_string(&report_events[0]["details"])
            .unwrap()
            .contains(private_path_marker)
    );
    assert!(
        !serde_json::to_string(&partial)
            .unwrap()
            .contains(private_path_marker)
    );

    controller.record_successful_command(1, "quit", Duration::ZERO, json!({}));
    assert_eq!(controller.command_index, 2);
    assert!(
        controller
            .events
            .iter()
            .all(|event| event.status == "passed")
    );
}

#[test]
fn fatal_automation_exit_does_not_synthesize_gate_batch_outcomes() {
    let script: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "fatal_partial_batch",
        "commands": [{
            "command": "observe_gate_batch",
            "batch_id": "FC.partial",
            "phase_id": "blocking",
            "origin": { "kind": "automation_started" },
            "observations": [
                {
                    "gate_id": "FC.first",
                    "deadline_authority": "cold_first_useful",
                    "deadline_after_origin_ns": 10,
                    "target": { "kind": "condition", "condition": "first_frame" }
                },
                {
                    "gate_id": "FC.settled",
                    "deadline_authority": "cold_target_settlement",
                    "deadline_after_origin_ns": 20,
                    "target": {
                        "kind": "condition",
                        "condition": "coordinated_presentation_settled"
                    }
                }
            ]
        }]
    }))
    .unwrap();
    script.validate().unwrap();
    let mut controller = ProductAutomationController::new(
        script,
        PathBuf::from("gate-script.json"),
        PathBuf::from("gate-report.json"),
    );
    controller.active_gate_batch = Some(ActiveProductGateBatch {
        command_index: 0,
        origin_at: controller.started_at,
        outcomes: vec![
            Some(LatchedProductGateObservation {
                outcome: ProductGateObservationOutcome::Passed,
                condition_met: true,
                timed_out: false,
                observed_after_origin_ns: 9,
            }),
            None,
        ],
    });

    let status = controller.record_fatal_command_failure(
        0,
        "observe_gate_batch",
        Duration::from_nanos(17),
        "hard-safety limit or external cancellation".to_owned(),
    );
    assert!(matches!(status, AutomationStatus::Failed(_)));
    assert!(controller.active_gate_batch.is_none());
    assert_eq!(controller.events.len(), 1);
    let value = serde_json::to_value(&controller.events[0]).unwrap();
    assert_eq!(value["command"], "observe_gate_batch");
    assert_eq!(value["status"], "failed");
    assert_eq!(
        value["details"].as_object().map(serde_json::Map::len),
        Some(1)
    );
    assert!(value["details"].get("observations").is_none());
    let serialized = serde_json::to_string(&controller.events).unwrap();
    assert!(!serialized.contains(AUTOMATION_GATE_BATCH_OBSERVATION_SCHEMA));
    assert!(!serialized.contains("FC.first"));
}

#[test]
fn refinement_handoff_diagnostics_name_an_uninitialized_hidden_target() {
    assert_eq!(
        refinement_handoff_phase(false, false, false, false),
        RefinementHandoffPhase::Inactive
    );
    assert_eq!(
        refinement_handoff_phase(true, false, false, false),
        RefinementHandoffPhase::AwaitingHiddenTargetRegistration
    );
    assert_eq!(
        refinement_handoff_phase(true, true, false, false),
        RefinementHandoffPhase::AwaitingHiddenTargetRequest
    );
    assert_eq!(
        refinement_handoff_phase(true, true, true, false),
        RefinementHandoffPhase::HiddenTargetStreaming
    );
    assert_eq!(
        refinement_handoff_phase(true, true, true, true),
        RefinementHandoffPhase::HiddenTargetPresented
    );
}

#[test]
fn target_renderer_facts_expose_request_and_progress_authority() {
    let mut target = crate::native_presentation::ProductPresentationTarget::new(
        mirante4d_render_api::PresentationToken::new(17).unwrap(),
        mirante4d_render_api::RenderExtent::new(64, 64).unwrap(),
    );
    target.last_renderer_available_resources = 3;
    target.set_progressive_lease_probe_state(
        crate::native_presentation::ProgressiveLeaseProbeState {
            next_requirement: 2,
            requirements_remaining: 5,
            render_requested: false,
        },
    );

    let facts = product_target_renderer_facts_json("3D", &target);

    assert_eq!(facts["request_bound"], false);
    assert_eq!(facts["presentation_present"], false);
    assert_eq!(facts["presentation_matches_request"], false);
    assert_eq!(facts["last_renderer_available_resources"], 3);
    assert_eq!(facts["progressive_probe"]["next_requirement"], 2);
    assert_eq!(facts["progressive_probe"]["requirements_remaining"], 5);
    assert_eq!(facts["progressive_probe"]["render_requested"], false);
}

#[test]
fn dataset_switch_protocol_starts_once_waits_and_fails_closed() {
    let before_timeout = AUTOMATION_DATASET_SWITCH_TIMEOUT - Duration::from_millis(1);
    assert_eq!(
        dataset_switch_protocol_decision(
            true,
            false,
            false,
            Duration::ZERO,
            AUTOMATION_DATASET_SWITCH_TIMEOUT,
        ),
        DatasetSwitchProtocolDecision::TargetAlreadySelected
    );
    assert_eq!(
        dataset_switch_protocol_decision(
            false,
            false,
            false,
            Duration::ZERO,
            AUTOMATION_DATASET_SWITCH_TIMEOUT,
        ),
        DatasetSwitchProtocolDecision::Start
    );
    assert_eq!(
        dataset_switch_protocol_decision(
            false,
            true,
            true,
            before_timeout,
            AUTOMATION_DATASET_SWITCH_TIMEOUT,
        ),
        DatasetSwitchProtocolDecision::Waiting
    );
    assert_eq!(
        dataset_switch_protocol_decision(
            true,
            true,
            false,
            before_timeout,
            AUTOMATION_DATASET_SWITCH_TIMEOUT,
        ),
        DatasetSwitchProtocolDecision::Installed
    );
    assert_eq!(
        dataset_switch_protocol_decision(
            false,
            true,
            false,
            before_timeout,
            AUTOMATION_DATASET_SWITCH_TIMEOUT,
        ),
        DatasetSwitchProtocolDecision::Failed
    );
    assert_eq!(
        dataset_switch_protocol_decision(
            false,
            true,
            true,
            AUTOMATION_DATASET_SWITCH_TIMEOUT,
            AUTOMATION_DATASET_SWITCH_TIMEOUT,
        ),
        DatasetSwitchProtocolDecision::TimedOut
    );
    assert_eq!(AUTOMATION_DATASET_SWITCH_TIMEOUT.as_millis(), 120_000);
}

#[test]
fn switch_dataset_command_is_distinct_from_the_startup_assertion() {
    let script: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "temporal_switch",
        "commands": [
            { "command": "open_dataset", "path": "/tmp/representative.m4d" },
            { "command": "switch_dataset", "path": "/tmp/temporal.m4d" },
            { "command": "quit" }
        ]
    }))
    .unwrap();
    script.validate().unwrap();
    assert!(matches!(
        &script.commands[0],
        ProductAutomationCommand::OpenDataset { path }
            if path == &PathBuf::from("/tmp/representative.m4d")
    ));
    assert!(matches!(
        &script.commands[1],
        ProductAutomationCommand::SwitchDataset { path }
            if path == &PathBuf::from("/tmp/temporal.m4d")
    ));
    assert_eq!(script.commands[0].name(), "open_dataset");
    assert_eq!(script.commands[1].name(), "switch_dataset");

    let empty_target: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "invalid_temporal_switch",
        "commands": [{ "command": "switch_dataset", "path": "" }]
    }))
    .unwrap();
    assert!(empty_target.validate().is_err());
}

#[test]
fn ep00_script_parses_time_distributed_camera_cross_section_and_pt_commands() {
    let script: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "ep00_workloads",
        "diagnostic_counters": true,
        "commands": [
            { "command": "camera_zoom_sequence", "samples": 24, "duration_ms": 240, "scroll_y_points_per_sample": 1.0 },
            { "command": "camera_orbit_sequence", "samples": 12, "duration_ms": 120, "yaw_points_per_sample": 2.0, "pitch_points_per_sample": -1.0 },
            { "command": "set_active_cross_section_panel", "panel": "xy" },
            { "command": "cross_section_rotate_sequence", "panel": "xy", "samples": 12, "duration_ms": 120, "x_points_per_sample": 1.0, "y_points_per_sample": 0.5, "radians_per_point": 0.01 },
            { "command": "cross_section_pan_sequence", "panel": "xz", "samples": 12, "duration_ms": 120, "x_points_per_sample": 1.0, "y_points_per_sample": -1.0 },
            { "command": "cross_section_zoom_sequence", "panel": "yz", "samples": 12, "duration_ms": 120, "x_fraction": 0.5, "y_fraction": 0.5, "factor_per_sample": 0.98 },
            { "command": "cross_section_slice_sequence", "panel": "yz", "samples": 12, "duration_ms": 120, "distance_world_per_sample": 0.25 },
            { "command": "set_time_index", "time_index": 1 },
            { "command": "set_layer_visibility", "layer_index": 1, "visible": true },
            { "command": "set_layer_order", "layer_indices": [1, 0] },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 30000 },
            { "command": "sample_diagnostics", "label": "after-no" },
            { "command": "assert", "condition": { "cross_section_panel_schedule": { "panel": "yz" } } },
            { "command": "quit" }
        ]
    }))
    .unwrap();

    script.validate().unwrap();
    assert!(script.requires_diagnostic_counters());
    assert!(matches!(
        script.commands[0],
        ProductAutomationCommand::CameraZoomSequence { samples: 24, .. }
    ));
    assert_eq!(PanelId::from(ProductAutomationPanelId::Xy), PanelId::Xy);
    assert_eq!(PanelId::from(ProductAutomationPanelId::Yz), PanelId::Yz);
}

#[test]
fn ep00_detailed_counters_require_the_explicit_script_flag() {
    let script: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "instrumentation_control",
        "diagnostic_counters": false,
        "gpu_timing": false,
        "commands": [
            { "command": "camera_pan_sequence", "samples": 2, "duration_ms": 10, "x_points_per_sample": 1.0, "y_points_per_sample": 0.0 },
            { "command": "sample_diagnostics", "label": "disabled-control" }
        ]
    }))
    .unwrap();

    script.validate().unwrap();
    assert!(!script.requires_diagnostic_counters());
    assert!(!script.requires_gpu_timing());
}

#[test]
fn ep00_startup_bootstrap_accepts_only_bounded_pre_demand_state() {
    let script: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "cold_four_panel",
        "startup_bootstrap": {
            "capture_start_checkpoint": true,
            "start_diagnostic_label": "fc-cold-zero-demand",
            "commands": [
                { "command": "set_mapped_client_pixels", "width": 1280, "height": 720 },
                { "command": "set_four_panel_viewports", "presentation_width_points": 317.0, "presentation_height_points": 287.5, "three_d_render_width": 1280, "three_d_render_height": 720, "linked_render_width": 317, "linked_render_height": 288 },
                { "command": "set_viewer_layout", "layout": "four_panel" },
                { "command": "set_time_index", "time_index": 0 },
                { "command": "set_active_cross_section_panel", "panel": "xy" },
                { "command": "set_layer_render_mode", "layer_index": 0, "mode": "mip" },
                { "command": "set_layer_sampling", "layer_index": 0, "sampling": "voxel_exact" },
                { "command": "set_layer_window", "layer_index": 0, "low": 0.0, "high": 255.0 },
                { "command": "set_layer_opacity", "layer_index": 0, "opacity": 1.0 },
                { "command": "set_projection", "projection": "orthographic" },
                { "command": "camera_fit_data" },
                { "command": "cross_section_zoom_sequence", "panel": "xy", "samples": 1, "duration_ms": 1, "x_fraction": 0.5, "y_fraction": 0.5, "factor_per_sample": 0.0625 }
            ]
        },
        "commands": [{ "command": "quit" }]
    }))
    .unwrap();

    script.validate().unwrap();
    assert_eq!(
        script
            .startup_bootstrap
            .as_ref()
            .unwrap()
            .start_diagnostic_label
            .as_deref(),
        Some("fc-cold-zero-demand")
    );

    for rejected in [
        json!({ "command": "open_dataset", "path": "/tmp/not-bootstrap-state" }),
        json!({ "command": "cross_section_zoom_sequence", "panel": "xy", "samples": 2, "duration_ms": 1, "x_fraction": 0.5, "y_fraction": 0.5, "factor_per_sample": 0.5 }),
    ] {
        let candidate: ProductAutomationScript = serde_json::from_value(json!({
            "schema": AUTOMATION_SCRIPT_SCHEMA,
            "schema_version": AUTOMATION_SCHEMA_VERSION,
            "hard_safety_limits": {},
            "scenario": "rejected_bootstrap",
            "startup_bootstrap": {
                "capture_start_checkpoint": true,
                "start_diagnostic_label": "start",
                "commands": [rejected]
            },
            "commands": [{ "command": "quit" }]
        }))
        .unwrap();
        assert!(candidate.validate().is_err());
    }
}

#[test]
fn ep00_setup_only_bootstrap_accepts_exact_camera_and_plane_without_checkpoint() {
    let script: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "nonresident_setup",
        "startup_bootstrap": {
            "capture_start_checkpoint": false,
            "commands": [
                { "command": "set_viewer_layout", "layout": "four_panel" },
                { "command": "set_time_index", "time_index": 0 },
                {
                    "command": "set_camera_view",
                    "projection": "orthographic",
                    "target_world": [1.0, 2.0, 3.0],
                    "orientation_xyzw": [0.0, 0.0, 0.0, 1.0],
                    "orthographic_world_per_screen_point": 54.0,
                    "perspective_focal_length_screen_points": 500.0,
                    "perspective_view_distance_world": 1000.0
                },
                {
                    "command": "set_cross_section_view",
                    "center_world": [1.0, 2.0, 3.0],
                    "orientation_xyzw": [0.0, 0.0, 0.0, 1.0],
                    "scale_world_per_screen_point": 8.0,
                    "depth_world": 64.0
                }
            ]
        },
        "commands": [{ "command": "quit" }]
    }))
    .unwrap();
    script.validate().unwrap();
    let bootstrap = script.startup_bootstrap.as_ref().unwrap();
    assert!(!bootstrap.capture_start_checkpoint);
    assert!(bootstrap.start_diagnostic_label.is_none());

    let mismatched: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "invalid_setup",
        "startup_bootstrap": {
            "capture_start_checkpoint": false,
            "start_diagnostic_label": "must-not-exist",
            "commands": [{ "command": "set_time_index", "time_index": 0 }]
        },
        "commands": [{ "command": "quit" }]
    }))
    .unwrap();
    assert!(mismatched.validate().is_err());
}

#[test]
fn ep00_exact_cross_section_view_command_is_strictly_validated() {
    let script = serde_json::json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "exact_cross_section_reset",
        "commands": [
            {
                "command": "set_cross_section_view",
                "center_world": [1.0, 2.0, 3.0],
                "orientation_xyzw": [0.0, 0.0, 0.0, 1.0],
                "scale_world_per_screen_point": 2.0,
                "depth_world": 4.0
            },
            { "command": "quit" }
        ]
    });
    let parsed: ProductAutomationScript = serde_json::from_value(script).unwrap();
    parsed.validate().unwrap();

    let invalid = serde_json::json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "invalid_cross_section_reset",
        "commands": [
            {
                "command": "set_cross_section_view",
                "center_world": [1.0, 2.0, 3.0],
                "orientation_xyzw": [0.0, 0.0, 0.0, 1.0],
                "scale_world_per_screen_point": 0.0,
                "depth_world": 4.0
            }
        ]
    });
    let parsed: ProductAutomationScript = serde_json::from_value(invalid).unwrap();
    assert!(parsed.validate().is_err());
}

#[test]
fn ep00_sequence_bounds_and_diagnostic_labels_are_validated() {
    for command in [
        json!({ "command": "camera_zoom_sequence", "samples": 0, "duration_ms": 1, "scroll_y_points_per_sample": 1.0 }),
        json!({ "command": "camera_zoom_sequence", "samples": MAX_INPUT_SEQUENCE_SAMPLES + 1, "duration_ms": 1, "scroll_y_points_per_sample": 1.0 }),
        json!({ "command": "camera_zoom_sequence", "samples": 1, "duration_ms": 0, "scroll_y_points_per_sample": 1.0 }),
        json!({ "command": "camera_zoom_sequence", "samples": 1, "duration_ms": MAX_INPUT_SEQUENCE_DURATION_MS + 1, "scroll_y_points_per_sample": 1.0 }),
        json!({ "command": "camera_pan_sequence", "samples": 1, "duration_ms": 1, "x_points_per_sample": 0.0, "y_points_per_sample": -0.0 }),
        json!({ "command": "cross_section_zoom_sequence", "panel": "xy", "samples": 1, "duration_ms": 1, "x_fraction": 0.5, "y_fraction": 0.5, "factor_per_sample": 1.0 }),
        json!({ "command": "sample_diagnostics", "label": "" }),
        json!({ "command": "set_layer_order", "layer_indices": (0..=mirante4d_render_api::MAX_RENDER_LAYERS).collect::<Vec<_>>() }),
    ] {
        let script: ProductAutomationScript = serde_json::from_value(json!({
            "schema": AUTOMATION_SCRIPT_SCHEMA,
            "schema_version": AUTOMATION_SCHEMA_VERSION,
            "hard_safety_limits": {},
            "scenario": "invalid_ep00_bound",
            "commands": [command]
        }))
        .unwrap();
        assert!(script.validate().is_err());
    }
}

#[test]
fn ep00_sequence_schedule_spans_the_requested_monotonic_duration() {
    assert_eq!(sequence_sample_target_ns(0, 5, 100), 0);
    assert_eq!(sequence_sample_target_ns(1, 5, 100), 25_000_000);
    assert_eq!(sequence_sample_target_ns(4, 5, 100), 100_000_000);
    assert_eq!(sequence_sample_target_ns(0, 1, 100), 0);
}

#[test]
fn ep00_labeled_union_delta_reconciles_retained_added_and_removed_bytes() {
    use mirante4d_dataset::{
        DatasetResourceIdentity, DatasetResourceKey, DatasetSourceId, ResourceRegion,
    };
    use mirante4d_domain::{LogicalLayerKey, ScaleLevel, Shape3D, TimeIndex};

    let key = |origin_x| {
        DatasetResourceKey::new(
            DatasetResourceIdentity::Unverified(DatasetSourceId::new(9)),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, origin_x], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
        )
    };
    let previous = vec![(key(0), 10), (key(1), 20)];
    let current = vec![(key(1), 20), (key(2), 30)];

    let delta =
        diagnostic_resource_union_delta("before-motion", &previous, "after-motion", &current);

    assert_eq!(delta["previous_label"], "before-motion");
    assert_eq!(delta["current_label"], "after-motion");
    assert_eq!(
        delta["previous_union_sha256"],
        canonical_resource_union_sha256(&previous).to_string()
    );
    assert_eq!(
        delta["current_union_sha256"],
        canonical_resource_union_sha256(&current).to_string()
    );
    assert_eq!(delta["retained_unique_keys"], 1);
    assert_eq!(delta["retained_unique_payload_bytes"], 20);
    assert_eq!(delta["added_unique_keys"], 1);
    assert_eq!(delta["added_unique_payload_bytes"], 30);
    assert_eq!(delta["removed_unique_keys"], 1);
    assert_eq!(delta["removed_unique_payload_bytes"], 10);
    assert_eq!(delta["retained_payload_bytes_match"], true);
    assert_eq!(delta["partitions_pairwise_disjoint"], true);

    let phase_start_resident = vec![(key(1), 20), (key(7), 70)];
    let partition = target_residency_partition_json("phase-start", &phase_start_resident, &current);
    assert_eq!(partition["available"], true);
    assert_eq!(partition["phase_start_label"], "phase-start");
    assert_eq!(
        partition["phase_start_resident_union_sha256"],
        canonical_resource_union_sha256(&phase_start_resident).to_string()
    );
    assert_eq!(
        partition["resident_target_intersection"]["canonical_entries_sha256"],
        canonical_resource_union_sha256(&[(key(1), 20)]).to_string()
    );
    assert_eq!(partition["resident_target_intersection"]["unique_keys"], 1);
    assert_eq!(
        partition["nonresident_target_difference"]["canonical_entries_sha256"],
        canonical_resource_union_sha256(&[(key(2), 30)]).to_string()
    );
    assert_eq!(partition["nonresident_target_difference"]["unique_keys"], 1);
    assert_eq!(partition["target_union_reconciles"], true);
}

#[test]
fn ep00_display_batch_aggregates_include_hidden_staging_target_work() {
    use crate::native_presentation::ProductTargetDiagnosticCounters;

    let first = ProductTargetDiagnosticCounters {
        color_passes: 2,
        completion_notifications: 3,
        encoded_display_batches: 3,
        control_static_rebuilds: 1,
        ..ProductTargetDiagnosticCounters::default()
    };
    let hidden_staging = ProductTargetDiagnosticCounters {
        color_passes: 5,
        completion_notifications: 7,
        encoded_display_batches: 7,
        control_static_rebuilds: 11,
        ..ProductTargetDiagnosticCounters::default()
    };

    let total = aggregate_product_target_diagnostic_counters([first, hidden_staging]);

    assert_eq!(total.color_passes, 7);
    assert_eq!(total.completion_notifications, 10);
    assert_eq!(total.encoded_display_batches, 10);
    assert_eq!(total.control_static_rebuilds, 12);
}

#[test]
fn automation_alone_does_not_enable_validation_capture() {
    let navigation: ProductAutomationScript = serde_json::from_str(
        r#"{
          "schema": "mirante4d-product-automation-script",
          "schema_version": 5,
          "hard_safety_limits": {},
          "scenario": "performance_navigation",
          "commands": [
            { "command": "camera_orbit", "yaw_points": 10.0, "pitch_points": 5.0 },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "quit" }
          ]
        }"#,
    )
    .unwrap();
    navigation.validate().unwrap();
    assert!(!navigation.requires_validation_capture());

    for pixel_command in [
        r#"{ "command": "capture_screenshot", "name": "mode-proof" }"#,
        r#"{ "command": "assert", "condition": "nonblank_frame" }"#,
        r#"{ "command": "assert", "condition": {
          "four_panel_images_distinct": { "min_different_pixels": 1 }
        } }"#,
    ] {
        let raw = format!(
            r#"{{
              "schema": "mirante4d-product-automation-script",
              "schema_version": 5,
              "hard_safety_limits": {{}},
              "scenario": "render_correctness",
              "commands": [{pixel_command}]
            }}"#
        );
        let script: ProductAutomationScript = serde_json::from_str(&raw).unwrap();
        script.validate().unwrap();
        assert!(script.requires_validation_capture());
    }
}

#[test]
fn import_raw_evidence_serializes_the_complete_statistics_surface() {
    let statistics = ImportStatistics::default();
    let evidence = import_statistics_json(&statistics);

    for field in [
        "source_bytes_read",
        "source_revalidation_bytes_read",
        "native_decoded_bytes",
        "base_native_decoded_bytes",
        "scientific_identity_native_decoded_bytes",
        "tiff_open_count",
        "native_chunk_decode_count",
        "logical_output_bytes",
        "checkpoint_payload_bytes",
        "checkpoint_journal_bytes",
        "checkpoint_watermark_bytes",
        "checkpoint_durable_work_units",
        "checkpoint_pending_work_units",
        "checkpoint_committed_batches",
        "codec_encode_calls",
        "codec_encode_time_ns",
        "codec_decode_calls",
        "codec_decode_time_ns",
        "sync_calls",
        "sync_time_ns",
        "scientific_brick_reads",
        "staged_structure_object_reads",
        "staged_exact_object_reads",
        "scientific_object_reads",
        "scientific_payload_object_reads",
        "scientific_range_requests",
        "scientific_encoded_bytes_read",
        "scientific_decoded_bytes",
        "object_reads",
        "sampled_peak_open_file_descriptors",
        "open_file_descriptor_structural_bound",
        "peak_open_file_descriptors",
        "preflight_temporary_bytes_bound",
        "peak_temporary_bytes",
        "peak_checkpoint_regular_files",
        "peak_working_bytes",
        "peak_process_rss_bytes",
        "resumed_work_units",
        "produced_work_units",
        "primary_wall_time_ns",
        "primary_cpu_time_ns",
        "stages",
    ] {
        assert!(evidence.get(field).is_some(), "missing raw field {field}");
    }

    let primary = import_primary_measurement_json(Some(ImportPrimaryMeasurement {
        started_at_epoch_ms: 10,
        open_ready_at_epoch_ms: 20,
        wall_time_ns: 30,
        process_cpu_time_ns: 40,
    }));
    assert_eq!(primary["wall_time_ns"], 30);
    assert_eq!(
        primary["end_boundary"],
        "published_destination_verified_and_open_ready_for_normal_product_use"
    );

    let inspection = import_pre_start_measurement_json(Some(ImportPreStartMeasurement {
        started_at_epoch_ms: 1,
        start_command_at_epoch_ms: 2,
        wall_time_ns: 3,
        process_cpu_time_ns: 4,
    }));
    assert_eq!(inspection["wall_time_ns"], 3);
    assert_eq!(inspection["process_cpu_time_ns"], 4);
    assert_eq!(inspection["excluded_from_primary_clock"], true);

    let build = t5_build_provenance_json();
    for field in ["repository_revision", "profile", "compiler", "target_mode"] {
        assert!(build.get(field).is_some(), "missing build field {field}");
    }
}

#[test]
fn automation_script_parses_the_b4_project_store_contract() {
    let raw = r#"
        {
          "schema": "mirante4d-product-automation-script",
          "schema_version": 5,
          "hard_safety_limits": {},
          "scenario": "b4_project_store",
          "commands": [
            { "command": "set_mapped_client_pixels", "width": 1280, "height": 720 },
            { "command": "new_project" },
            { "command": "initial_save_with_edit", "path": "/tmp/original.m4dproj" },
            { "command": "wait_for", "condition": "project_store_idle", "timeout_ms": 1000 },
            { "command": "wait_for", "condition": "project_autosaved", "timeout_ms": 31000 },
            { "command": "open_project", "path": "/tmp/original.m4dproj" },
            { "command": "wait_for", "condition": "recovery_review_required", "timeout_ms": 1000 },
            { "command": "recover_automatic_autosave" },
            { "command": "save_project_as", "path": "/tmp/recovered.m4dproj" },
            { "command": "close_project_store" },
            { "command": "wait_for", "condition": "project_store_closed", "timeout_ms": 1000 },
            { "command": "write_external_kill_checkpoint", "path": "/tmp/checkpoint.json", "stage": "autosaved" },
            { "command": "assert", "condition": { "project_state": {
              "bound": true,
              "dirty": true,
              "lifecycle": "established",
              "can_save": true,
              "can_save_as": true,
              "manual": true,
              "autosave": true
            } } },
            { "command": "hold_for_external_kill" },
            { "command": "quit" }
          ]
        }"#;

    let script: ProductAutomationScript = serde_json::from_str(raw).unwrap();
    script.validate().unwrap();

    assert_eq!(script.commands.len(), 15);
    assert_eq!(script.commands[0].name(), "set_mapped_client_pixels");
    assert_eq!(script.commands[1].name(), "new_project");
    assert_eq!(script.commands[2].name(), "initial_save_with_edit");
    assert_eq!(script.commands[5].name(), "open_project");
    assert_eq!(script.commands[7].name(), "recover_automatic_autosave");
    assert_eq!(script.commands[8].name(), "save_project_as");
    assert_eq!(script.commands[9].name(), "close_project_store");
    assert_eq!(script.commands[11].name(), "write_external_kill_checkpoint");
    assert_eq!(script.commands[13].name(), "hold_for_external_kill");
    for index in [3, 4, 6, 10] {
        let ProductAutomationCommand::WaitFor { condition, .. } = script.commands[index] else {
            panic!("command {index} is not a wait");
        };
        assert!(condition.is_passive());
    }
    let ProductAutomationCommand::Assert {
        condition:
            ProductAutomationAssertCondition::ProjectState {
                bound,
                dirty,
                lifecycle,
                can_save,
                can_save_as,
                manual,
                autosave,
            },
    } = script.commands[12]
    else {
        panic!("expected the structured project-state assertion");
    };
    assert!(bound && dirty && can_save && can_save_as && manual && autosave);
    assert_eq!(
        lifecycle,
        ProductAutomationProjectStoreLifecycle::Established
    );
}

#[test]
fn b4_project_evidence_helpers_keep_exact_typed_facts() {
    let project_id = mirante4d_project_model::ProjectId::from_bytes([7; 16]);
    let revision = ProjectRevisionId::new(project_id, 42);
    assert_eq!(
        project_revision_json(Some(revision))["project_id"],
        project_id.to_string()
    );
    assert_eq!(project_revision_json(Some(revision))["sequence"], 42);
    assert_eq!(project_revision_json(None), Value::Null);

    assert_eq!(
        project_store_lifecycle(ProductAutomationProjectStoreLifecycle::RecoverySelected),
        ProjectStoreLifecycle::RecoverySelected
    );
    assert_eq!(
        project_store_lifecycle_name(ProjectStoreLifecycle::RecoveryOnly),
        "recovery_only"
    );

    let close = recorded_result_json(Some(&crate::ProjectStoreRecordedResult::Succeeded), "fault");
    assert_eq!(close["status"], "succeeded");
    assert_eq!(close["fault"], Value::Null);
    let join = recorded_result_json(
        Some(&crate::ProjectStoreRecordedResult::Failed(
            "join failed".to_owned(),
        )),
        "error",
    );
    assert_eq!(join["status"], "failed");
    assert_eq!(join["error"], "join failed");
    assert!(join.get("failure_key").is_none());
}

#[test]
fn external_kill_checkpoint_writer_syncs_once_without_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("checkpoint.json");
    let checkpoint = json!({
        "schema": "mirante4d-product-external-kill-checkpoint",
        "schema_version": 1,
        "stage": "autosaved",
    });

    write_synced_json_no_replace(&path, &checkpoint).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
        checkpoint
    );
    assert!(
        write_synced_json_no_replace(&path, &json!({ "stage": "replacement" }))
            .unwrap_err()
            .contains("already exists")
    );
}

#[test]
fn automation_script_parses_semantic_camera_commands() {
    let raw = r#"
        {
          "schema": "mirante4d-product-automation-script",
          "schema_version": 5,
          "hard_safety_limits": {
            "max_cpu_total_bytes": 1024,
            "max_runtime_queued_requests": 128
          },
          "scenario": "unit",
          "commands": [
            { "command": "open_dataset", "path": "/tmp/demo.m4d" },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 1000 },
            { "command": "set_iso_display_level", "display_level": 0.05 },
            { "command": "set_dvr_density_scale", "density_scale": 12.0 },
            { "command": "set_layer_render_mode", "layer_index": 1, "mode": "dvr" },
            { "command": "set_projection", "projection": "perspective" },
            { "command": "set_layer_sampling", "layer_index": 1, "sampling": "smooth_linear" },
            { "command": "set_layer_render_mode", "layer_index": 1, "mode": "iso" },
            { "command": "set_layer_iso_shading", "layer_index": 1, "shading": "gradient_lighting" },
            { "command": "set_iso_light", "light": { "kind": "detached_screen", "x": 0.25, "y": -0.5 } },
            { "command": "set_layer_window", "layer_index": 0, "low": 0.0, "high": 4096.0 },
            { "command": "set_layer_opacity", "layer_index": 0, "opacity": 1.0 },
            { "command": "camera_orbit", "yaw_points": 100.0, "pitch_points": 25.0 },
            { "command": "camera_pan", "x_points": 10.0, "y_points": -5.0 },
            { "command": "camera_zoom", "scroll_y_points": -120.0 },
            { "command": "set_active_tool", "tool": "inspect" },
            { "command": "probe_hover", "x_fraction": 0.5, "y_fraction": 0.5 },
            { "command": "primary_click", "x_fraction": 0.5, "y_fraction": 0.5 },
            { "command": "capture_screenshot", "name": "unit screenshot" },
            { "command": "assert", "condition": { "projection": { "projection": "perspective" } } },
            { "command": "assert", "condition": { "layer_sampling": { "layer_index": 1, "sampling": "smooth_linear" } } },
            { "command": "assert", "condition": { "layer_iso_shading": { "layer_index": 1, "shading": "gradient_lighting" } } },
            { "command": "assert", "condition": { "iso_light": { "light": { "kind": "detached_screen", "x": 0.25, "y": -0.5 } } } },
            { "command": "assert", "condition": { "active_tool": { "tool": "inspect" } } },
            { "command": "assert", "condition": "crosshair_linked" },
            { "command": "assert", "condition": "roi_committed" },
            { "command": "assert", "condition": "distance_committed" },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "quit" }
          ]
        }"#;

    let script: ProductAutomationScript = serde_json::from_str(raw).unwrap();

    script.validate().unwrap();
    assert_eq!(script.commands.len(), 29);
    assert_eq!(script.hard_safety_limits.max_cpu_total_bytes, Some(1024));
    assert_eq!(
        script.hard_safety_limits.max_runtime_queued_requests,
        Some(128)
    );
    assert_eq!(script.commands[2].name(), "set_iso_display_level");
    assert_eq!(script.commands[3].name(), "set_dvr_density_scale");
    assert_eq!(script.commands[4].name(), "set_layer_render_mode");
    assert_eq!(script.commands[5].name(), "set_projection");
    assert_eq!(script.commands[6].name(), "set_layer_sampling");
    assert_eq!(script.commands[8].name(), "set_layer_iso_shading");
    assert_eq!(script.commands[9].name(), "set_iso_light");
    assert_eq!(script.commands[10].name(), "set_layer_window");
    assert_eq!(script.commands[11].name(), "set_layer_opacity");
    assert_eq!(script.commands[12].name(), "camera_orbit");
    assert_eq!(script.commands[15].name(), "set_active_tool");
    assert_eq!(script.commands[16].name(), "probe_hover");
    assert_eq!(script.commands[17].name(), "primary_click");
    assert_eq!(script.commands[18].name(), "capture_screenshot");
    for index in 19..=27 {
        assert_eq!(script.commands[index].name(), "assert");
    }
}

#[test]
fn automation_script_parses_source_verification_evidence_workflow() {
    let raw = r#"
        {
          "schema": "mirante4d-product-automation-script",
          "schema_version": 5,
          "hard_safety_limits": {},
          "scenario": "b3_source_verification",
          "commands": [
            { "command": "set_render_target_size", "width": 1280, "height": 720 },
            { "command": "cancel_source_verification" },
            { "command": "wait_for", "condition": "source_verification_required", "timeout_ms": 1000 },
            { "command": "request_source_verification" },
            { "command": "wait_for", "condition": "source_verification_verified", "timeout_ms": 1000 },
            { "command": "assert", "condition": { "source_verification_evidence": {
              "min_accepted_progress_updates": 1,
              "min_cancelled_runs": 1,
              "min_accepted_successes": 1
            } } },
            { "command": "assert", "condition": { "render_target_pixels": {
              "width": 1280,
              "height": 720
            } } },
            { "command": "quit" }
          ]
        }"#;

    let script: ProductAutomationScript = serde_json::from_str(raw).unwrap();

    script.validate().unwrap();
    assert_eq!(script.commands[0].name(), "set_render_target_size");
    assert_eq!(script.commands[1].name(), "cancel_source_verification");
    assert_eq!(script.commands[3].name(), "request_source_verification");
    let ProductAutomationCommand::Assert { condition } = &script.commands[5] else {
        panic!("expected source-verification evidence assertion");
    };
    assert_eq!(condition.name(), "source_verification_evidence");
    let ProductAutomationCommand::Assert { condition } = &script.commands[6] else {
        panic!("expected render-target assertion");
    };
    assert_eq!(condition.name(), "render_target_pixels");
}

#[test]
fn automation_script_parses_normal_import_cancel_resume_workflow() {
    let raw = r#"
        {
          "schema": "mirante4d-product-automation-script",
          "schema_version": 5,
          "hard_safety_limits": {},
          "scenario": "import_preprocessing",
          "commands": [
            { "command": "begin_tiff_import_setup", "source": "/tmp/source", "output_parent": "/tmp/output" },
            { "command": "wait_for", "condition": "import_review_ready", "timeout_ms": 1000 },
            { "command": "start_reviewed_import", "spacing_zyx_um": [0.4, 0.2, 0.1], "time_step_seconds": null, "no_data_sentinel": 255, "working_memory_bytes": 268435456 },
            { "command": "wait_for_import_progress", "stage": "base-production", "minimum_completed_work_units": 512, "timeout_ms": 1000 },
            { "command": "cancel_import" },
            { "command": "wait_for", "condition": "import_idle", "timeout_ms": 1000 },
            { "command": "wait_for_imported_open_ready", "path": "/tmp/output/source.m4d", "timeout_ms": 1000 },
            { "command": "assert", "condition": { "import_workflow_evidence": {
              "required_stage_names": ["base-production", "commit"],
              "min_projected_named_stages": 1,
              "min_cancelled_runs": 1,
              "min_successful_runs": 1,
              "min_resumed_work_units": 512,
              "min_elapsed_ms": 1,
              "min_projected_elapsed_ms": 1,
              "max_peak_working_bytes": 268435456
            } } },
            { "command": "quit" }
          ]
        }"#;

    let script: ProductAutomationScript = serde_json::from_str(raw).unwrap();
    script.validate().unwrap();
    assert_eq!(script.commands[0].name(), "begin_tiff_import_setup");
    assert_eq!(script.commands[2].name(), "start_reviewed_import");
    assert_eq!(script.commands[3].name(), "wait_for_import_progress");
    assert_eq!(script.commands[4].name(), "cancel_import");
    assert_eq!(script.commands[6].name(), "wait_for_imported_open_ready");
    let ProductAutomationCommand::Assert { condition } = &script.commands[7] else {
        panic!("expected import evidence assertion");
    };
    assert_eq!(condition.name(), "import_workflow_evidence");
}

#[test]
fn automation_script_parses_retained_four_panel_assertions() {
    let raw = r#"
        {
          "schema": "mirante4d-product-automation-script",
          "schema_version": 5,
          "hard_safety_limits": {},
          "scenario": "unit_four_panel",
          "commands": [
            { "command": "set_viewer_layout", "layout": "four_panel" },
            { "command": "assert", "condition": { "viewer_layout": { "layout": "four_panel" } } },
            { "command": "assert", "condition": { "cross_section_panel_schedule": {
              "panel": "xz",
              "min_generation": 1,
              "min_selected_resources": 1
            } } },
            { "command": "assert", "condition": { "four_panel_images_distinct": {
              "min_different_pixels": 1
            } } },
            { "command": "set_viewer_layout", "layout": "single3d" },
            { "command": "assert", "condition": "cross_section_retired" },
            { "command": "quit" }
          ]
        }"#;

    let script: ProductAutomationScript = serde_json::from_str(raw).unwrap();

    script.validate().unwrap();
    assert_eq!(script.commands.len(), 7);
    assert_eq!(script.commands[0].name(), "set_viewer_layout");
    for (index, expected) in [
        (1, "viewer_layout"),
        (2, "cross_section_panel_schedule"),
        (3, "four_panel_images_distinct"),
        (5, "cross_section_retired"),
    ] {
        let ProductAutomationCommand::Assert { condition } = &script.commands[index] else {
            panic!("command {index} is not an assertion");
        };
        assert_eq!(condition.name(), expected);
    }
    assert_eq!(script.commands[4].name(), "set_viewer_layout");
    assert_eq!(script.commands[6].name(), "quit");
}

#[test]
fn automation_script_rejects_removed_model_inputs() {
    for command in [
        json!({ "command": "set_viewer_layout", "layout": "single_3d" }),
        json!({
            "command": "assert",
            "condition": { "cross_section_panel_schedule": {
                "panel": "three_d",
                "min_generation": 1,
                "min_selected_resources": 1
            } }
        }),
        json!({ "command": "set_render_mode", "mode": "isosurface" }),
        json!({ "command": "sleep_or_frames", "frames": 1 }),
        json!({ "command": "sleep_frames", "millis": 1 }),
        json!({
            "command": "camera_orbit",
            "yaw_points": 1.0,
            "pitch_points": 1.0,
            "viewport_height_points": 800.0
        }),
        json!({
            "command": "camera_pan",
            "x_points": 1.0,
            "y_points": 1.0,
            "viewport_height_points": 800.0
        }),
    ] {
        let script = json!({
            "schema": AUTOMATION_SCRIPT_SCHEMA,
            "schema_version": AUTOMATION_SCHEMA_VERSION,
            "hard_safety_limits": {},
            "scenario": "removed_model_spelling",
            "commands": [command]
        });
        assert!(serde_json::from_value::<ProductAutomationScript>(script).is_err());
    }
}

#[test]
fn automation_script_rejects_wrong_schema_version() {
    let script = ProductAutomationScript {
        schema: AUTOMATION_SCRIPT_SCHEMA.to_owned(),
        schema_version: 3,
        scenario: "unit".to_owned(),
        gpu_timing: false,
        diagnostic_counters: false,
        startup_bootstrap: None,
        hard_safety_limits: ProductAutomationHardSafetyLimits::default(),
        commands: vec![ProductAutomationCommand::Quit],
    };

    let err = script.validate().unwrap_err().to_string();

    assert!(err.contains("unsupported automation script schema version"));
}

#[test]
fn automation_hard_safety_limits_reject_exceeded_runtime_bytes_and_work() {
    let limits = ProductAutomationHardSafetyLimits {
        max_cpu_total_bytes: Some(100),
        ..ProductAutomationHardSafetyLimits::default()
    };
    let diagnostics = runtime_diagnostics([101, 0, 0, 0, 0, 0, 0], 3, 1, 1, 2);

    assert!(
        limits
            .check_dataset_runtime(diagnostics)
            .unwrap_err()
            .contains("cpu_total_bytes")
    );

    let limits = ProductAutomationHardSafetyLimits {
        max_runtime_queued_requests: Some(2),
        ..ProductAutomationHardSafetyLimits::default()
    };
    assert!(
        limits
            .check_dataset_runtime(diagnostics)
            .unwrap_err()
            .contains("runtime_queued_requests")
    );
}

#[test]
fn qualification_peak_above_conceptual_gate_remains_observational_below_hard_safety_cap() {
    let conceptual_qualification_gate = 80;
    let diagnostics = runtime_diagnostics([81, 0, 0, 0, 0, 0, 0], 3, 1, 1, 2);
    let hard_safety_limits = ProductAutomationHardSafetyLimits {
        max_cpu_total_bytes: Some(100),
        ..ProductAutomationHardSafetyLimits::default()
    };
    let mut observations = ProductAutomationLimitObservations::default();

    observations.observe_dataset_runtime(diagnostics);

    assert!(observations.max_cpu_total_bytes > conceptual_qualification_gate);
    hard_safety_limits
        .check_dataset_runtime(diagnostics)
        .unwrap();
}

#[test]
fn automation_limit_observations_track_maxima() {
    let mut observations = ProductAutomationLimitObservations::default();

    observations.observe_dataset_runtime(runtime_diagnostics([50, 20, 10, 5, 4, 3, 2], 3, 1, 2, 4));
    observations.observe_dataset_runtime(runtime_diagnostics([40, 25, 8, 6, 7, 1, 3], 7, 2, 1, 3));

    assert_eq!(observations.max_cpu_total_bytes, 94);
    assert_eq!(observations.max_cpu_decoded_residency_bytes, 50);
    assert_eq!(observations.max_cpu_upload_staging_bytes, 25);
    assert_eq!(observations.max_cpu_queues_and_results_bytes, 7);
    assert_eq!(observations.max_runtime_queued_requests, 7);
    assert_eq!(observations.max_runtime_in_flight_decodes, 2);
    assert_eq!(observations.max_runtime_pending_completions, 2);
    assert_eq!(observations.max_runtime_resident_resources, 4);
}

#[test]
fn dataset_runtime_diagnostics_json_names_capacity_usage_and_bounds() {
    let diagnostics = runtime_diagnostics([50, 20, 10, 5, 4, 3, 2], 3, 1, 2, 4);

    let value = dataset_runtime_diagnostics_json(diagnostics);

    assert_eq!(value["capacity"]["total_cpu_bytes"], 1_000);
    assert_eq!(value["capacity"]["worker_limit"], 4);
    assert_eq!(value["capacity"]["request_queue_limit"], 16);
    assert_eq!(value["capacity"]["completion_queue_limit"], 16);
    assert_eq!(value["used"]["total_cpu_bytes"], 94);
    assert_eq!(value["used"]["category_bytes"]["decoded_residency"], 50);
    assert_eq!(value["work"]["queued_requests"], 3);
    assert_eq!(value["work"]["in_flight_decodes"], 1);
    assert_eq!(value["work"]["pending_completions"], 2);
    assert_eq!(value["work"]["resident_resources"], 4);
    assert_eq!(value["performance"]["cache_hits"], 0);
    assert_eq!(value["counters"]["decode_cohorts"], 0);
    assert_eq!(value["counters"]["decode_cohort_members"], 0);
    assert_eq!(value["counters"]["peak_decode_cohort_members"], 0);
    assert_eq!(value["performance"]["decode_time_ns"], 0);
    assert_eq!(value["performance"]["cancelled_decode_executions"], 0);
    assert_eq!(value["performance"]["cancelled_decode_time_ns"], 0);
    assert_eq!(value["performance"]["cancelled_decode_bytes"], 0);
}

#[test]
fn local_source_diagnostics_json_names_physical_reuse_and_io_costs() {
    let value = local_dataset_source_diagnostics_json(
        mirante4d_storage::LocalDatasetSourceDiagnostics::default(),
    );

    assert_eq!(value["physical_bricks"]["cache_hits"], 0);
    assert_eq!(value["physical_bricks"]["unique_decoded_bytes"], 0);
    assert_eq!(value["aligned_direct"]["deliveries"], 0);
    assert_eq!(value["aligned_direct"]["sink_span_bytes"], 0);
    assert_eq!(value["aligned_direct"]["post_decode_copy_bytes"], 0);
    assert_eq!(value["reader"]["physical_range_read_operations"], 0);
    assert_eq!(value["reader"]["currentness"]["pre_use_batches"], 0);
    assert_eq!(value["reader"]["currentness"]["time_ns"], 0);
    assert_eq!(value["reader"]["codec_decode_time_ns"], 0);
    assert_eq!(
        value["reader"]["cancelled_encoded_bytes"],
        json!({
            "available": false,
            "reason": "physical_range_cohort_has_no_per_sink_cancellation_ownership",
        })
    );
}

fn runtime_diagnostics(
    category_used: [u64; 7],
    queued_requests: usize,
    in_flight_decodes: usize,
    pending_completions: usize,
    resident_resources: usize,
) -> DatasetRuntimeDiagnostics {
    let config = DatasetRuntimeConfig::new(1_000, 4, 16, 16).unwrap();
    let completed_decodes = 10;
    let started_decodes = completed_decodes + in_flight_decodes as u64;
    let ready_requests = 10;
    let submitted_requests = ready_requests + queued_requests as u64 + in_flight_decodes as u64;
    DatasetRuntimeDiagnostics::new(
        config,
        category_used,
        queued_requests,
        in_flight_decodes,
        pending_completions,
        resident_resources,
        submitted_requests,
        started_decodes,
        completed_decodes,
        ready_requests,
        0,
        0,
    )
    .unwrap()
}

#[test]
fn artifact_label_sanitizer_is_path_safe() {
    assert_eq!(
        sanitize_artifact_label("post camera/sequence.ppm"),
        "post-camera-sequence-ppm"
    );
    assert_eq!(sanitize_artifact_label("already_ok-1"), "already_ok-1");
}

#[test]
fn color_image_stats_detect_blank_and_nonblank_rgb_content() {
    let blank = egui::ColorImage {
        size: [2, 1],
        pixels: vec![egui::Color32::BLACK, egui::Color32::BLACK],
        source_size: egui::Vec2::new(2.0, 1.0),
    };
    let blank_stats = ProductAutomationImageStats::from_color_image(&blank);
    assert!(blank_stats.is_blank());
    assert_eq!(blank_stats.pixel_count, 2);
    assert_eq!(blank_stats.nonzero_rgb_pixels, 0);
    assert_eq!(blank_stats.max_rgb, 0);

    let nonblank = egui::ColorImage {
        size: [2, 1],
        pixels: vec![egui::Color32::BLACK, egui::Color32::from_rgb(10, 20, 30)],
        source_size: egui::Vec2::new(2.0, 1.0),
    };
    let nonblank_stats = ProductAutomationImageStats::from_color_image(&nonblank);
    assert!(!nonblank_stats.is_blank());
    assert_eq!(nonblank_stats.pixel_count, 2);
    assert_eq!(nonblank_stats.nonzero_rgb_pixels, 1);
    assert_eq!(nonblank_stats.min_rgb, 0);
    assert_eq!(nonblank_stats.max_rgb, 30);
    assert_eq!(nonblank_stats.mean_rgb, 10.0);
}

#[test]
fn viewport_artifact_json_includes_capture_source_and_pixel_stats() {
    let artifact = ProductAutomationArtifact {
        kind: "viewport_capture",
        format: "ppm",
        path: PathBuf::from("target/mirante4d/product-validation/unit/artifacts/unit.ppm"),
        width: 2,
        height: 1,
        command_index: 3,
        capture_source: "loading_reference_color_image",
        pixel_stats: ProductAutomationImageStats {
            pixel_count: 2,
            nonzero_rgb_pixels: 1,
            min_rgb: 0,
            max_rgb: 30,
            mean_rgb: 10.0,
        },
    };

    let value = artifact.json();

    assert_eq!(value["capture_source"], "loading_reference_color_image");
    assert_eq!(value["pixel_stats"]["pixel_count"], 2);
    assert_eq!(value["pixel_stats"]["nonzero_rgb_pixels"], 1);
    assert_eq!(value["pixel_stats"]["max_rgb"], 30);
}

#[test]
fn color_image_from_rgba_rejects_mismatched_readback_size() {
    let image = color_image_from_rgba(2, 1, &[1, 2, 3, 255, 4, 5, 6, 128]).unwrap();
    assert_eq!(image.size, [2, 1]);
    assert_eq!(image.pixels[0], egui::Color32::from_rgb(1, 2, 3));
    assert_eq!(
        image.pixels[1],
        egui::Color32::from_rgba_unmultiplied(4, 5, 6, 128)
    );

    let err = color_image_from_rgba(2, 1, &[1, 2, 3, 255])
        .unwrap_err()
        .to_string();
    assert!(err.contains("expected 8"));
}

#[test]
fn color_image_ppm_writer_emits_binary_rgb_image() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("capture.ppm");
    let image = egui::ColorImage {
        size: [2, 1],
        pixels: vec![
            egui::Color32::from_rgb(1, 2, 3),
            egui::Color32::from_rgb(4, 5, 6),
        ],
        source_size: egui::Vec2::new(2.0, 1.0),
    };

    write_color_image_ppm(&path, &image).unwrap();
    let bytes = fs::read(path).unwrap();

    assert_eq!(&bytes[..11], b"P6\n2 1\n255\n");
    assert_eq!(&bytes[11..], &[1, 2, 3, 4, 5, 6]);
}

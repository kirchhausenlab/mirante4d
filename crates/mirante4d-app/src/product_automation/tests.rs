use super::capture::{
    ProductAutomationArtifact, ProductAutomationImageStats, color_image_from_rgba,
    sanitize_artifact_label, write_color_image_ppm,
};
use super::diagnostics::{dataset_runtime_diagnostics_json, local_dataset_source_diagnostics_json};
use super::*;
use std::{fs, path::PathBuf};

use crate::tests::{
    open_dataset_and_render_first_frame, test_workbench_app_without_background_runtime,
    write_target_fixture,
};
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
fn native_pick_evidence_requires_a_sampled_exact_hit_with_the_mode_policy() {
    let evidence = json!({
        "purpose": "probe_hover",
        "status": "sampled",
        "kind": "voxel",
        "completeness": "exact",
        "policy": "mip_argmax",
        "value": { "dtype": "uint16", "value": 1234 },
        "world_position": [1.0, 2.0, 3.0],
        "render_pixel": [10.0, 20.0],
        "placeholder_sampled": false,
        "native_gpu_pick": true,
    });

    assert!(assert_native_pick_evidence(&evidence, ProductAutomationPickPolicy::MipArgmax).is_ok());
    assert!(
        assert_native_pick_evidence(
            &evidence,
            ProductAutomationPickPolicy::MaximumOpacityContribution
        )
        .unwrap_err()
        .contains("pick policy")
    );

    for (field, invalid) in [
        ("native_gpu_pick", json!(false)),
        ("placeholder_sampled", json!(true)),
        ("status", json!("empty")),
        ("completeness", json!("incomplete")),
        ("value", Value::Null),
    ] {
        let mut invalid_evidence = evidence.clone();
        invalid_evidence[field] = invalid;
        assert!(
            assert_native_pick_evidence(&invalid_evidence, ProductAutomationPickPolicy::MipArgmax)
                .is_err(),
            "{field} must remain part of the pick acceptance contract"
        );
    }
}

#[test]
fn runtime_idle_wait_includes_async_camera_demand_planning_and_install() {
    assert!(automation_runtime_is_idle(false, false, false));
    assert!(!automation_runtime_is_idle(true, false, false));
    assert!(!automation_runtime_is_idle(false, true, false));
    assert!(!automation_runtime_is_idle(false, false, true));
}

#[test]
fn schema_v8_requires_exact_hard_safety_limits_without_legacy_alias() {
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
    let mut open_ready_outcome = None;

    ImportedOpenReadyCommitState {
        active_origin: &mut active_origin,
        active_verification_origin: &mut active_verification_origin,
        completed_primary: &mut completed_primary,
        open_ready_outcome: &mut open_ready_outcome,
    }
    .commit(primary, "complete-open-ready-outcome");

    assert!(active_origin.is_none());
    assert!(active_verification_origin.is_none());
    assert_eq!(completed_primary, Some(primary));
    assert_eq!(open_ready_outcome, Some("complete-open-ready-outcome"));
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
fn passed_imported_open_ready_adds_full_clocks_to_the_same_publication_evidence() {
    let evidence = test_import_publication_evidence_snapshot();
    let timing = ImportPublicationToOpenReadyMeasurement {
        published_at_epoch_ms: 11,
        open_ready_at_epoch_ms: 12,
        wall_time_ns: 13,
        process_cpu_time_ns: 14,
    };

    let full = import_publication_to_open_ready_measurement_json(Some(ImportedOpenReadyOutcome {
        measurement: timing,
        evidence,
    }));

    assert_eq!(
        full,
        json!({
            "status": "open_ready_complete",
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
    assert_eq!(full.as_object().map(serde_json::Map::len), Some(17));
}

#[test]
fn refinement_handoff_diagnostics_follow_the_dataset_cutoff() {
    assert_eq!(
        refinement_handoff_phase(false),
        RefinementHandoffPhase::Inactive
    );
    assert_eq!(
        refinement_handoff_phase(true),
        RefinementHandoffPhase::AwaitingCoordinatedExactPresentation
    );
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
fn exact_cross_section_view_command_is_strictly_validated() {
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
fn input_sequence_bounds_are_validated() {
    for command in [
        json!({ "command": "camera_zoom_sequence", "samples": 0, "duration_ms": 1, "scroll_y_points_per_sample": 1.0 }),
        json!({ "command": "camera_zoom_sequence", "samples": MAX_INPUT_SEQUENCE_SAMPLES + 1, "duration_ms": 1, "scroll_y_points_per_sample": 1.0 }),
        json!({ "command": "camera_zoom_sequence", "samples": 1, "duration_ms": 0, "scroll_y_points_per_sample": 1.0 }),
        json!({ "command": "camera_zoom_sequence", "samples": 1, "duration_ms": MAX_INPUT_SEQUENCE_DURATION_MS + 1, "scroll_y_points_per_sample": 1.0 }),
        json!({ "command": "camera_pan_sequence", "samples": 1, "duration_ms": 1, "x_points_per_sample": 0.0, "y_points_per_sample": -0.0 }),
        json!({ "command": "cross_section_zoom_sequence", "panel": "xy", "samples": 1, "duration_ms": 1, "x_fraction": 0.5, "y_fraction": 0.5, "factor_per_sample": 1.0 }),
        json!({ "command": "set_layer_order", "layer_indices": (0..=mirante4d_render_api::MAX_RENDER_LAYERS).collect::<Vec<_>>() }),
    ] {
        let script: ProductAutomationScript = serde_json::from_value(json!({
            "schema": AUTOMATION_SCRIPT_SCHEMA,
            "schema_version": AUTOMATION_SCHEMA_VERSION,
            "hard_safety_limits": {},
            "scenario": "invalid_input_bound",
            "commands": [command]
        }))
        .unwrap();
        assert!(script.validate().is_err());
    }
}

#[test]
fn input_sequence_schedule_spans_the_requested_monotonic_duration() {
    assert_eq!(sequence_sample_target_ns(0, 5, 100), 20_000_000);
    assert_eq!(sequence_sample_target_ns(1, 5, 100), 40_000_000);
    assert_eq!(sequence_sample_target_ns(4, 5, 100), 100_000_000);
    assert_eq!(sequence_sample_target_ns(0, 1, 100), 100_000_000);
    assert_eq!(sequence_sample_target_ns(0, 300, 5_000), 16_666_666);
    assert_eq!(sequence_sample_target_ns(299, 300, 5_000), 5_000_000_000);
}

#[test]
fn input_sequence_deadline_reaches_eframe_without_egui_frame_time_subtraction() {
    use std::sync::{Arc, Mutex};

    let context = egui::Context::default();
    let _ = context.run_ui(egui::RawInput::default(), |_| {});
    let callback_delays = Arc::new(Mutex::new(Vec::new()));
    let observed_delays = Arc::clone(&callback_delays);
    context.set_request_repaint_callback(move |request| {
        observed_delays.lock().unwrap().push(request.delay);
    });
    let input = egui::RawInput {
        predicted_dt: 0.020,
        ..Default::default()
    };
    let expected_deadline = Duration::from_millis(7);
    let _ = context.run_ui(input, |ui| {
        ui.ctx()
            .request_repaint_after(egui_deadline_repaint_delay(ui.ctx(), expected_deadline));
    });

    assert_eq!(
        callback_delays.lock().unwrap().last().copied(),
        Some(expected_deadline),
        "the backend must receive the absolute input deadline, not an immediate animation wake"
    );
}

#[test]
fn camera_sequence_uses_three_mailbox_samples_and_one_durable_commit() {
    use std::sync::{Arc, Mutex};

    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let script: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "mailbox_sequence_test",
        "commands": [{
            "command": "camera_pan_sequence",
            "samples": 3,
            "duration_ms": 1,
            "x_points_per_sample": 1.0,
            "y_points_per_sample": 0.0
        }]
    }))
    .unwrap();
    script.validate().unwrap();
    let command = script.commands[0].clone();
    let mut controller = ProductAutomationController::new(
        script,
        temp.path().join("script.json"),
        temp.path().join("report.json"),
    );
    let context = egui::Context::default();
    let _ = context.run_ui(egui::RawInput::default(), |_| {});
    let repaint_delays = Arc::new(Mutex::new(Vec::new()));
    let observed_repaint_delays = Arc::clone(&repaint_delays);
    context.set_request_repaint_callback(move |request| {
        observed_repaint_delays.lock().unwrap().push(request.delay);
    });
    let original_camera = *application_view(&app.application.snapshot()).camera();
    let expected_camera = (0..3).fold(original_camera, |camera, _| pan_camera(camera, [1.0, 0.0]));
    let currentness_before = app.application.snapshot().currentness();
    let commits_before = app
        .render_coordination
        .display_generation()
        .durable_gesture_commits;

    assert!(matches!(
        controller.execute_command(&mut app, &context, &command),
        Ok(CommandProgress::PassiveWaiting(_))
    ));
    controller
        .active_input_sequence
        .as_mut()
        .unwrap()
        .started_at = Instant::now() - Duration::from_millis(2);
    assert!(matches!(
        controller.execute_command(&mut app, &context, &command),
        Ok(CommandProgress::PassiveWaiting(_))
    ));
    assert_eq!(
        app.render_intent_mailbox.snapshot().raw_samples,
        0,
        "egui multipass re-entry in one frame must not dispatch a newly due sample"
    );

    assert_eq!(
        *application_view(&app.application.snapshot()).camera(),
        original_camera
    );
    assert_eq!(app.application.snapshot().currentness(), currentness_before);
    assert_eq!(
        app.render_coordination
            .display_generation()
            .durable_gesture_commits,
        commits_before
    );

    controller.automation_frame_nr += 1;
    assert!(matches!(
        controller.execute_command(&mut app, &context, &command),
        Ok(CommandProgress::PassiveWaiting(_))
    ));
    assert!(
        repaint_delays.lock().unwrap().is_empty(),
        "a synchronously published resident sample must not request a redundant immediate frame"
    );
    controller.automation_frame_nr += 1;
    assert!(matches!(
        controller.execute_command(&mut app, &context, &command),
        Ok(CommandProgress::PassiveWaiting(_))
    ));
    controller.automation_frame_nr += 1;
    assert!(matches!(
        controller.execute_command(&mut app, &context, &command),
        Ok(CommandProgress::PassiveWaiting(_))
    ));
    assert_eq!(
        *application_view(&app.application.snapshot()).camera(),
        original_camera,
        "the final transient sample must not commit durable state in its dispatch update"
    );
    assert_eq!(
        effective_interaction_camera(&app),
        expected_camera,
        "the mailbox must retain the accumulated transient result while finish waits"
    );
    assert_eq!(
        app.render_coordination
            .display_generation()
            .durable_gesture_commits,
        commits_before,
        "the durable commit must remain separate from the final sample"
    );
    let transient_mailbox = app.render_intent_mailbox.snapshot();
    assert_eq!(transient_mailbox.raw_samples, 3);
    assert_eq!(transient_mailbox.coalesced_samples, 2);
    assert_eq!(transient_mailbox.finished_gestures, 0);
    assert!(transient_mailbox.active_gesture.is_some());
    let transient_generation = app
        .render_coordination
        .display_generation()
        .input_generation;
    assert_eq!(
        app.render_coordination
            .coordinated_publication_diagnostics()
            .current_presentation_generation(),
        None
    );

    controller.automation_frame_nr += 1;
    assert!(matches!(
        controller.execute_command(&mut app, &context, &command),
        Ok(CommandProgress::PassiveWaiting(_))
    ));
    assert_eq!(
        app.render_coordination
            .display_generation()
            .durable_gesture_commits,
        commits_before,
        "finish must keep waiting while the final transient generation is not current"
    );
    let now_ns = app.display_instrumentation_now_ns();
    app.render_coordination.record_current_presentation(now_ns);
    app.render_coordination
        .record_current_group_presentation(now_ns);
    assert_eq!(
        app.render_coordination
            .coordinated_publication_diagnostics()
            .current_presentation_generation(),
        Some(transient_generation)
    );

    controller.automation_frame_nr += 1;
    let details = match controller.execute_command(&mut app, &context, &command) {
        Ok(CommandProgress::Done(details)) => details,
        _ => panic!("a separate current-generation update must finish the automation sequence"),
    };

    assert_eq!(
        *application_view(&app.application.snapshot()).camera(),
        expected_camera,
        "automation must accumulate every sample before committing the latest camera"
    );
    assert_eq!(
        app.render_coordination
            .display_generation()
            .durable_gesture_commits,
        commits_before + 1
    );
    let mailbox = app.render_intent_mailbox.snapshot();
    assert_eq!(mailbox.raw_samples, 3);
    assert_eq!(mailbox.coalesced_samples, 2);
    assert_eq!(mailbox.finished_gestures, 1);
    assert!(mailbox.active_gesture.is_none());
    assert_eq!(details["samples"], 3);
    assert_eq!(details["observed_counter_delta"]["input_generations"], 4);
    assert_eq!(
        details["observed_counter_delta"]["durable_gesture_commits"],
        1
    );
    assert_eq!(
        details["observed_counter_delta"]["render_intent_revisions"], 3,
        "durable finish commits the latest transient body without allocating a second frame"
    );
    assert_eq!(
        details["observed_counter_delta"]["render_intent_samples"],
        3
    );
    assert_eq!(
        details["observed_counter_delta"]["render_intent_coalesced_samples"],
        2
    );
    assert_eq!(
        details["observed_counter_delta"]["render_intent_finished_gestures"],
        1
    );
    assert_eq!(
        details["input_evidence"]["dispatch_timing"]["dispatched_samples"],
        3
    );
    assert_eq!(
        details["input_evidence"]["dispatch_timing"]["distinct_app_updates"], 3,
        "each sample must belong to a distinct egui frame"
    );
    assert_eq!(
        details["input_evidence"]["dispatch_timing"]["same_update_dispatches"],
        0
    );
    assert!(
        details["input_evidence"]["dispatch_timing"]["maximum_dispatch_lateness_ns"]
            .as_u64()
            .unwrap()
            > 0,
        "dispatch timing must be derived from the delayed samples, not a static claim"
    );
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn automation_command_updates_the_same_ui_turn_snapshot() {
    use egui_kittest::kittest;

    fn accesskit_tree_contains_label(node: kittest::AccessKitNode<'_>, expected: &str) -> bool {
        node.label().as_deref() == Some(expected)
            || node
                .children()
                .any(|child| accesskit_tree_contains_label(child, expected))
    }

    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let script: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "same_ui_turn_snapshot",
        "commands": [{
            "command": "set_viewer_layout",
            "layout": "four_panel"
        }]
    }))
    .unwrap();
    script.validate().unwrap();
    let controller = ProductAutomationController::new(
        script,
        temp.path().join("script.json"),
        temp.path().join("report.json"),
    );
    let context = egui::Context::default();
    mirante4d_ui_egui::configure_visuals(&context);
    context.enable_accesskit();
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 720.0),
        )),
        ..Default::default()
    };
    input
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
        .unwrap()
        .native_pixels_per_point = Some(1.0);
    let mut app = test_workbench_app_without_background_runtime(opened);
    assert_eq!(
        application_view(&app.application.snapshot()).layout(),
        ViewerLayout::Single3d
    );
    for slot in [
        PresentationSlot::Xy,
        PresentationSlot::Xz,
        PresentationSlot::Yz,
    ] {
        assert!(
            app.render_coordination
                .surface(slot)
                .presentation_viewport()
                .is_none(),
            "the Single3d control must begin without a {slot:?} panel viewport"
        );
    }
    app.product_automation = Some(controller);
    let mut frame = eframe::Frame::_new_kittest();

    let mut output = context.run_ui(input, |ui| {
        eframe::App::logic(&mut app, ui.ctx(), &mut frame);
        #[expect(deprecated)]
        eframe::App::update(&mut app, ui.ctx(), &mut frame);
        eframe::App::ui(&mut app, ui, &mut frame);
    });

    assert_eq!(
        application_view(&app.application.snapshot()).layout(),
        ViewerLayout::FourPanel,
        "the automation command must execute in the sole UI turn"
    );
    let accesskit = kittest::State::new(
        output
            .platform_output
            .accesskit_update
            .take()
            .expect("the sole UI pass must publish an AccessKit tree"),
    );
    for (slot, label) in [
        (PresentationSlot::Xy, "XY cross-section panel"),
        (PresentationSlot::Xz, "XZ cross-section panel"),
        (PresentationSlot::Yz, "YZ cross-section panel"),
    ] {
        assert!(
            app.render_coordination
                .surface(slot)
                .presentation_viewport()
                .is_some(),
            "the sole UI pass must observe the new {slot:?} panel viewport"
        );
        assert!(
            accesskit_tree_contains_label(accesskit.root(), label),
            "the sole UI pass must expose {label}"
        );
    }
}

#[test]
fn automation_alone_does_not_enable_validation_capture() {
    let navigation: ProductAutomationScript = serde_json::from_str(
        r#"{
          "schema": "mirante4d-product-automation-script",
          "schema_version": 8,
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
        r#"{ "command": "capture_screenshot", "target": "three_d", "name": "mode-proof" }"#,
        r#"{ "command": "assert", "condition": {
          "nonblank_panel": { "target": "three_d" }
        } }"#,
        r#"{ "command": "assert", "condition": {
          "four_panel_images_distinct": { "min_different_pixels": 1 }
        } }"#,
    ] {
        let raw = format!(
            r#"{{
              "schema": "mirante4d-product-automation-script",
              "schema_version": 8,
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
fn automation_v8_rejects_implicit_capture_and_legacy_nonblank_frame() {
    let implicit_capture = serde_json::from_value::<ProductAutomationScript>(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "scenario": "implicit_capture",
        "hard_safety_limits": {},
        "commands": [
            { "command": "capture_screenshot", "name": "missing-target" }
        ]
    }));
    assert!(implicit_capture.is_err());

    let legacy_nonblank = serde_json::from_value::<ProductAutomationScript>(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "scenario": "legacy_nonblank",
        "hard_safety_limits": {},
        "commands": [
            { "command": "assert", "condition": "nonblank_frame" }
        ]
    }));
    assert!(legacy_nonblank.is_err());
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
          "schema_version": 8,
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
fn automation_script_parses_exposed_provisional_autosave_recovery() {
    let raw = r#"
        {
          "schema": "mirante4d-product-automation-script",
          "schema_version": 8,
          "hard_safety_limits": {},
          "scenario": "pre_alpha_recovery",
          "commands": [
            { "command": "wait_for", "condition": "unsaved_autosave_recovery_exposed", "timeout_ms": 1000 },
            { "command": "recover_exposed_unsaved_autosave" },
            { "command": "assert", "condition": { "project_state": {
              "bound": true,
              "dirty": true,
              "lifecycle": "provisional",
              "can_save": true,
              "can_save_as": false,
              "manual": false,
              "autosave": true
            } } },
            { "command": "quit" }
          ]
        }"#;

    let script: ProductAutomationScript = serde_json::from_str(raw).unwrap();
    script.validate().unwrap();
    assert_eq!(
        script.commands[1].name(),
        "recover_exposed_unsaved_autosave"
    );
    let ProductAutomationCommand::WaitFor { condition, .. } = script.commands[0] else {
        panic!("expected recovery-exposure wait");
    };
    assert!(matches!(
        condition,
        ProductAutomationWaitCondition::UnsavedAutosaveRecoveryExposed
    ));
    let ProductAutomationCommand::Assert {
        condition: ProductAutomationAssertCondition::ProjectState { lifecycle, .. },
    } = script.commands[2]
    else {
        panic!("expected provisional project-state assertion");
    };
    assert_eq!(
        lifecycle,
        ProductAutomationProjectStoreLifecycle::Provisional
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
        project_store_lifecycle(ProductAutomationProjectStoreLifecycle::Provisional),
        ProjectStoreLifecycle::Provisional
    );
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
          "schema_version": 8,
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
            { "command": "capture_screenshot", "target": "three_d", "name": "unit screenshot" },
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
fn automation_script_parses_exact_fidelity_layer_mode_and_pick_assertions() {
    let script: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "kernel_acceptance",
        "commands": [
            { "command": "assert", "condition": {
                "frame_fidelity": {
                    "scale_level": 0,
                    "complete": true,
                    "exact": true
                }
            } },
            { "command": "assert", "condition": {
                "layer_render_mode": { "layer_index": 1, "mode": "dvr" }
            } },
            { "command": "assert", "condition": {
                "pick_evidence": { "policy": "maximum_opacity_contribution" }
            } }
        ]
    }))
    .unwrap();

    script.validate().unwrap();
    let ProductAutomationCommand::Assert {
        condition:
            ProductAutomationAssertCondition::FrameFidelity {
                exact, complete, ..
            },
    } = &script.commands[0]
    else {
        panic!("expected exact frame-fidelity assertion");
    };
    assert!(*exact && *complete);
    let ProductAutomationCommand::Assert {
        condition: ProductAutomationAssertCondition::LayerRenderMode { layer_index, mode },
    } = &script.commands[1]
    else {
        panic!("expected layer-mode assertion");
    };
    assert_eq!(*layer_index, 1);
    assert!(matches!(mode, ProductAutomationRenderMode::Dvr));
    let ProductAutomationCommand::Assert {
        condition: ProductAutomationAssertCondition::PickEvidence { policy },
    } = &script.commands[2]
    else {
        panic!("expected pick-evidence assertion");
    };
    assert_eq!(
        *policy,
        ProductAutomationPickPolicy::MaximumOpacityContribution
    );
}

#[test]
fn automation_script_parses_source_verification_evidence_workflow() {
    let raw = r#"
        {
          "schema": "mirante4d-product-automation-script",
          "schema_version": 8,
          "hard_safety_limits": {},
          "scenario": "b3_source_verification",
          "commands": [
            { "command": "set_render_target_size", "width": 1280, "height": 720 },
            { "command": "cancel_active_source_verification" },
            { "command": "wait_for", "condition": "source_verification_inactive", "timeout_ms": 1000 },
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
    assert_eq!(
        script.commands[1].name(),
        "cancel_active_source_verification"
    );
    assert!(matches!(
        script.commands[2],
        ProductAutomationCommand::WaitFor {
            condition: ProductAutomationWaitCondition::SourceVerificationInactive,
            ..
        }
    ));
    assert_eq!(script.commands[3].name(), "cancel_source_verification");
    assert_eq!(script.commands[5].name(), "request_source_verification");
    let ProductAutomationCommand::Assert { condition } = &script.commands[7] else {
        panic!("expected source-verification evidence assertion");
    };
    assert_eq!(condition.name(), "source_verification_evidence");
    let ProductAutomationCommand::Assert { condition } = &script.commands[8] else {
        panic!("expected render-target assertion");
    };
    assert_eq!(condition.name(), "render_target_pixels");
}

#[test]
fn automation_script_parses_normal_import_cancel_resume_workflow() {
    let raw = r#"
        {
          "schema": "mirante4d-product-automation-script",
          "schema_version": 8,
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
          "schema_version": 8,
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
fn automation_script_bounds_sleep_frames_for_supervisor_budgeting() {
    for frames in [0, MAX_SLEEP_FRAMES + 1] {
        let script: ProductAutomationScript = serde_json::from_value(json!({
            "schema": AUTOMATION_SCRIPT_SCHEMA,
            "schema_version": AUTOMATION_SCHEMA_VERSION,
            "hard_safety_limits": {},
            "scenario": "bounded_sleep_frames",
            "commands": [{ "command": "sleep_frames", "frames": frames }]
        }))
        .unwrap();
        assert!(script.validate().is_err(), "frames={frames}");
    }

    let script: ProductAutomationScript = serde_json::from_value(json!({
        "schema": AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": AUTOMATION_SCHEMA_VERSION,
        "hard_safety_limits": {},
        "scenario": "bounded_sleep_frames",
        "commands": [{ "command": "sleep_frames", "frames": MAX_SLEEP_FRAMES }]
    }))
    .unwrap();
    script.validate().unwrap();
}

#[test]
fn automation_script_rejects_wrong_schema_version() {
    let script = ProductAutomationScript {
        schema: AUTOMATION_SCRIPT_SCHEMA.to_owned(),
        schema_version: 3,
        scenario: "unit".to_owned(),
        gpu_timing: false,
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
fn observed_peak_below_the_hard_safety_cap_is_accepted() {
    let diagnostics = runtime_diagnostics([81, 0, 0, 0, 0, 0, 0], 3, 1, 1, 2);
    let hard_safety_limits = ProductAutomationHardSafetyLimits {
        max_cpu_total_bytes: Some(100),
        ..ProductAutomationHardSafetyLimits::default()
    };
    let mut observations = ProductAutomationLimitObservations::default();

    observations.observe_dataset_runtime(diagnostics);

    assert_eq!(observations.max_cpu_total_bytes, 81);
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
fn panel_image_comparison_rejects_blank_targets_and_compares_unequal_extents() {
    let rgba = |pixels: &[[u8; 4]]| {
        pixels
            .iter()
            .flat_map(|pixel| pixel.iter().copied())
            .collect::<Vec<_>>()
    };
    let small = rgba(&[[255, 0, 0, 255], [0, 0, 255, 255]]);
    let normalized_equal = rgba(&[
        [255, 0, 0, 255],
        [255, 0, 0, 255],
        [0, 0, 255, 255],
        [0, 0, 255, 255],
        [255, 0, 0, 255],
        [255, 0, 0, 255],
        [0, 0, 255, 255],
        [0, 0, 255, 255],
    ]);
    let equal_error = assert_gpu_display_images_distinct(
        "unequal",
        &[
            ("small".to_owned(), 2, 1, small.clone()),
            ("large".to_owned(), 4, 2, normalized_equal.clone()),
        ],
        1,
    )
    .unwrap_err();
    assert!(equal_error.contains("0 normalized-coordinate pixels"));

    let mut normalized_different = normalized_equal;
    normalized_different[(1 * 4 + 3) * 4] = 255;
    assert!(
        assert_gpu_display_images_distinct(
            "unequal",
            &[
                ("small".to_owned(), 2, 1, small.clone()),
                ("large".to_owned(), 4, 2, normalized_different),
            ],
            1,
        )
        .is_ok()
    );

    let blank_error = assert_gpu_display_images_distinct(
        "blank",
        &[
            ("visible".to_owned(), 2, 1, small),
            ("blank-target".to_owned(), 1, 1, vec![0, 0, 0, 255]),
        ],
        1,
    )
    .unwrap_err();
    assert!(blank_error.contains("blank-target is RGB-blank"));

    let alpha_only_error = assert_gpu_display_images_distinct(
        "visible-rgb",
        &[
            ("opaque".to_owned(), 1, 1, vec![255, 0, 0, 255]),
            ("transparent".to_owned(), 1, 1, vec![255, 0, 0, 1]),
        ],
        1,
    )
    .unwrap_err();
    assert!(alpha_only_error.contains("0 direct pixels"));

    let final_pair_error = assert_gpu_display_images_distinct(
        "six-pairs",
        &[
            ("3D".to_owned(), 1, 1, vec![255, 0, 0, 255]),
            ("XY".to_owned(), 1, 1, vec![0, 255, 0, 255]),
            ("XZ".to_owned(), 1, 1, vec![0, 0, 255, 255]),
            ("YZ".to_owned(), 1, 1, vec![0, 0, 255, 255]),
        ],
        1,
    )
    .unwrap_err();
    assert!(final_pair_error.contains("XZ and YZ"));
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
        target: "xy",
        frame_identity: 42,
        surface_generation: 9,
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
    assert_eq!(value["target"], "xy");
    assert_eq!(value["frame_identity"], 42);
    assert_eq!(value["surface_generation"], 9);
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

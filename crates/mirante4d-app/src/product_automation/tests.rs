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
          "schema_version": 3,
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
              "schema_version": 3,
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
          "schema_version": 3,
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
          "schema_version": 3,
          "scenario": "unit",
          "limits": {
            "max_cpu_total_bytes": 1024,
            "max_runtime_queued_requests": 128
          },
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
    assert_eq!(script.limits.max_cpu_total_bytes, Some(1024));
    assert_eq!(script.limits.max_runtime_queued_requests, Some(128));
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
          "schema_version": 3,
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
          "schema_version": 3,
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
          "schema_version": 3,
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
        schema_version: 1,
        scenario: "unit".to_owned(),
        gpu_timing: false,
        diagnostic_counters: false,
        startup_bootstrap: None,
        limits: ProductAutomationLimits::default(),
        commands: vec![ProductAutomationCommand::Quit],
    };

    let err = script.validate().unwrap_err().to_string();

    assert!(err.contains("unsupported automation script schema version"));
}

#[test]
fn automation_limits_reject_exceeded_runtime_bytes_and_work() {
    let limits = ProductAutomationLimits {
        max_cpu_total_bytes: Some(100),
        ..ProductAutomationLimits::default()
    };
    let diagnostics = runtime_diagnostics([101, 0, 0, 0, 0, 0, 0], 3, 1, 1, 2);

    assert!(
        limits
            .check_dataset_runtime(diagnostics)
            .unwrap_err()
            .contains("cpu_total_bytes")
    );

    let limits = ProductAutomationLimits {
        max_runtime_queued_requests: Some(2),
        ..ProductAutomationLimits::default()
    };
    assert!(
        limits
            .check_dataset_runtime(diagnostics)
            .unwrap_err()
            .contains("runtime_queued_requests")
    );
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

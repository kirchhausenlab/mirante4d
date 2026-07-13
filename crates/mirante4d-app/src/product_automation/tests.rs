use super::capture::{
    ProductAutomationArtifact, ProductAutomationImageStats, color_image_from_rgba,
    sanitize_artifact_label, write_color_image_ppm,
};
use super::diagnostics::{
    dataset_runtime_diagnostics_json, gpu_adapter_diagnostics_json, gpu_timestamp_timing_json,
};
use super::timing::{
    ProductAutomationAppUpdatePhases, ProductAutomationAppUpdateSample,
    ProductAutomationCrossSectionLatencySample, ProductAutomationDisplayRefreshSample,
    ProductAutomationInputToPresentSample, app_update_timing_json, app_update_timing_summary_json,
    cross_section_latency_summary_json, display_refresh_timing_json,
    display_refresh_timing_summary_json, input_to_present_timing_summary_json,
    presentation_timing_json, timing_values_summary_json,
};
use super::*;
use std::{fs, path::PathBuf};

use mirante4d_dataset_runtime::{DatasetRuntimeConfig, DatasetRuntimeDiagnostics};
use mirante4d_renderer::gpu::{AdapterDiagnostics, GpuLimitDiagnostics};

#[test]
fn automation_script_parses_the_b4_project_store_contract() {
    let raw = r#"
        {
          "schema": "mirante4d-product-automation-script",
          "schema_version": 2,
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
fn cross_section_hover_readout_json_records_generation_semantics() {
    let readout = CrossSectionHoverReadout {
        text: "XY ch0 t0 s0 stale".to_owned(),
        panel_id: PanelId::Xy,
        layer_id: "ch0".to_owned(),
        timepoint: 0,
        scale_level: Some(0),
        target_generation: 2,
        displayed_generation: Some(1),
        schedule_generation: Some(2),
        display_current: false,
        generation_status:
            crate::cross_section_readout::CrossSectionHoverGenerationStatus::RetainedStale,
        world_position: None,
        grid_position: None,
        nearest_grid_index: None,
        value: None,
        status: CrossSectionHoverStatus::Stale,
    };

    let json = cross_section_hover_readout_json(&readout);

    assert_eq!(json["target_generation"], 2);
    assert_eq!(json["displayed_generation"], 1);
    assert_eq!(json["schedule_generation"], 2);
    assert_eq!(json["display_current"], false);
    assert_eq!(json["generation_status"], "retained_stale");
    assert_eq!(json["status"], "stale");
    assert_eq!(json["logical_layer_id"], "ch0");
    assert!(json.get("layer_id").is_none());
}

#[test]
fn automation_script_parses_semantic_camera_commands() {
    let raw = r#"
        {
          "schema": "mirante4d-product-automation-script",
          "schema_version": 2,
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
            { "command": "set_layer_window", "layer_index": 0, "low": 0.0, "high": 4096.0 },
            { "command": "set_layer_opacity", "layer_index": 0, "opacity": 1.0 },
            { "command": "camera_orbit", "yaw_points": 100.0, "pitch_points": 25.0 },
            { "command": "camera_pan", "x_points": 10.0, "y_points": -5.0 },
            { "command": "camera_zoom", "scroll_y_points": -120.0 },
            { "command": "probe_hover", "x_fraction": 0.5, "y_fraction": 0.5 },
            { "command": "capture_screenshot", "name": "unit screenshot" },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "quit" }
          ]
        }"#;

    let script: ProductAutomationScript = serde_json::from_str(raw).unwrap();

    script.validate().unwrap();
    assert_eq!(script.commands.len(), 14);
    assert_eq!(script.limits.max_cpu_total_bytes, Some(1024));
    assert_eq!(script.limits.max_runtime_queued_requests, Some(128));
    assert_eq!(script.commands[2].name(), "set_iso_display_level");
    assert_eq!(script.commands[3].name(), "set_dvr_density_scale");
    assert_eq!(script.commands[4].name(), "set_layer_render_mode");
    assert_eq!(script.commands[5].name(), "set_layer_window");
    assert_eq!(script.commands[6].name(), "set_layer_opacity");
    assert_eq!(script.commands[7].name(), "camera_orbit");
    assert_eq!(script.commands[10].name(), "probe_hover");
    assert_eq!(script.commands[11].name(), "capture_screenshot");
}

#[test]
fn automation_script_parses_source_verification_evidence_workflow() {
    let raw = r#"
        {
          "schema": "mirante4d-product-automation-script",
          "schema_version": 2,
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
fn automation_script_parses_four_panel_cross_section_commands_and_assertions() {
    let raw = r#"
        {
          "schema": "mirante4d-product-automation-script",
          "schema_version": 2,
          "scenario": "unit_four_panel",
          "commands": [
            { "command": "set_viewer_layout", "layout": "four_panel" },
            { "command": "cross_section_pan", "panel": "xy", "x_points": 12.0, "y_points": -4.0 },
            { "command": "cross_section_slice_step", "panel": "xz", "notches": 2.0, "fast": true },
            { "command": "cross_section_zoom", "panel": "yz", "x_fraction": 0.25, "y_fraction": 0.75, "scroll_y_points": -80.0 },
            { "command": "cross_section_rotate", "panel": "xy", "x_points": 5.0, "y_points": 3.0 },
            { "command": "probe_panel_hover", "panel": "xz", "x_fraction": 0.5, "y_fraction": 0.5, "expected_status": "value", "expect_value": true },
            { "command": "assert", "condition": { "viewer_layout": { "layout": "four_panel" } } },
            { "command": "assert", "condition": { "cross_section_active_panel": { "panel": "xy" } } },
            { "command": "assert", "condition": { "cross_section_panel_schedule": {
              "panel": "xy",
              "status": "loading",
              "min_generation": 1,
              "target_scale_level": 0,
              "render_scale_level": 1,
              "min_selected_resources": 1,
              "max_missing_occupied_resources": 8,
              "display_current": false
            } } },
            { "command": "assert", "condition": { "active_lease_cohort": {
              "min_required": 1,
              "min_retained": 1,
              "max_missing": 0,
              "complete": true
            } } },
            { "command": "assert", "condition": { "cross_section_panel_nonblank": {
              "panel": "xz",
              "min_nonzero_rgb_pixels": 1
            } } },
            { "command": "assert", "condition": { "cross_section_panel_images_distinct": {
              "min_different_pixels": 1
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
    assert_eq!(script.commands.len(), 16);
    assert_eq!(script.commands[0].name(), "set_viewer_layout");
    assert_eq!(script.commands[1].name(), "cross_section_pan");
    assert_eq!(script.commands[2].name(), "cross_section_slice_step");
    assert_eq!(script.commands[3].name(), "cross_section_zoom");
    assert_eq!(script.commands[4].name(), "cross_section_rotate");
    assert_eq!(script.commands[5].name(), "probe_panel_hover");
    let ProductAutomationCommand::Assert { condition } = &script.commands[6] else {
        panic!("expected viewer layout assertion");
    };
    assert_eq!(condition.name(), "viewer_layout");
    let ProductAutomationCommand::Assert { condition } = &script.commands[9] else {
        panic!("expected lease cohort assertion");
    };
    assert_eq!(condition.name(), "active_lease_cohort");
    let ProductAutomationCommand::Assert { condition } = &script.commands[10] else {
        panic!("expected panel nonblank assertion");
    };
    assert_eq!(condition.name(), "cross_section_panel_nonblank");
    let ProductAutomationCommand::Assert { condition } = &script.commands[11] else {
        panic!("expected panel image distinctness assertion");
    };
    assert_eq!(condition.name(), "cross_section_panel_images_distinct");
    let ProductAutomationCommand::Assert { condition } = &script.commands[12] else {
        panic!("expected four-panel image distinctness assertion");
    };
    assert_eq!(condition.name(), "four_panel_images_distinct");
    let ProductAutomationCommand::Assert { condition } = &script.commands[14] else {
        panic!("expected retired assertion");
    };
    assert_eq!(condition.name(), "cross_section_retired");
}

#[test]
fn automation_script_rejects_removed_label_probe_fields() {
    let direct_probe = r#"
        {
          "schema": "mirante4d-product-automation-script",
          "schema_version": 2,
          "scenario": "removed_direct_probe_field",
          "commands": [
            {
              "command": "probe_panel_hover",
              "panel": "xz",
              "x_fraction": 0.5,
              "y_fraction": 0.5,
              "expected_label": 7
            }
          ]
        }"#;
    let nested_probe = r#"
        {
          "schema": "mirante4d-product-automation-script",
          "schema_version": 2,
          "scenario": "removed_nested_probe_field",
          "commands": [
            {
              "command": "cross_section_pan",
              "panel": "xy",
              "x_points": 1.0,
              "y_points": 1.0,
              "probe_after": {
                "x_fraction": 0.5,
                "y_fraction": 0.5,
                "expect_label": true,
                "expected_label_status": "value"
              }
            }
          ]
        }"#;

    for (raw, removed_field) in [
        (direct_probe, "expected_label"),
        (nested_probe, "expect_label"),
    ] {
        let error = serde_json::from_str::<ProductAutomationScript>(raw).unwrap_err();
        assert!(error.to_string().contains(removed_field), "{error}");
    }
}

#[test]
fn automation_script_parses_timepoint_commands_and_assertion() {
    let raw = r#"
        {
          "schema": "mirante4d-product-automation-script",
          "schema_version": 2,
          "scenario": "unit_timepoint",
          "commands": [
            { "command": "set_timepoint", "timepoint": 1 },
            { "command": "step_timepoint", "delta": -1 },
            { "command": "set_playback", "playing": true },
            { "command": "assert", "condition": { "active_timepoint": { "timepoint": 0 } } },
            { "command": "assert", "condition": { "playback": { "playing": true } } },
            { "command": "assert", "condition": { "observed_timepoints": { "min_distinct": 2 } } },
            { "command": "assert", "condition": { "active_lease_cohort": {
              "min_required": 1,
              "min_retained": 1,
              "max_missing": 0,
              "complete": true
            } } },
            { "command": "quit" }
          ]
        }"#;

    let script: ProductAutomationScript = serde_json::from_str(raw).unwrap();

    script.validate().unwrap();
    assert_eq!(script.commands.len(), 8);
    assert_eq!(script.commands[0].name(), "set_timepoint");
    assert_eq!(script.commands[1].name(), "step_timepoint");
    assert_eq!(script.commands[2].name(), "set_playback");
    let ProductAutomationCommand::Assert { condition } = &script.commands[3] else {
        panic!("expected active timepoint assertion");
    };
    assert_eq!(condition.name(), "active_timepoint");
    let ProductAutomationCommand::Assert { condition } = &script.commands[4] else {
        panic!("expected playback assertion");
    };
    assert_eq!(condition.name(), "playback");
    let ProductAutomationCommand::Assert { condition } = &script.commands[5] else {
        panic!("expected observed timepoints assertion");
    };
    assert_eq!(condition.name(), "observed_timepoints");
    let ProductAutomationCommand::Assert { condition } = &script.commands[6] else {
        panic!("expected active lease cohort assertion");
    };
    assert_eq!(condition.name(), "active_lease_cohort");
}

#[test]
fn automation_script_rejects_wrong_schema_version() {
    let script = ProductAutomationScript {
        schema: AUTOMATION_SCRIPT_SCHEMA.to_owned(),
        schema_version: 1,
        scenario: "unit".to_owned(),
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
fn gpu_timestamp_timing_json_names_support_request_and_enablement() {
    let mut adapter = AdapterDiagnostics {
        name: "adapter".to_owned(),
        backend: "Vulkan".to_owned(),
        device_type: "DiscreteGpu".to_owned(),
        driver: "driver".to_owned(),
        driver_info: "driver-info".to_owned(),
        timestamp_queries_supported: true,
        timestamp_queries_requested: false,
        timestamp_queries_enabled: false,
        adapter_limits: GpuLimitDiagnostics {
            max_buffer_size: 1024,
            max_storage_buffer_binding_size: 2048,
            max_storage_buffers_per_shader_stage: 8,
        },
        requested_limits: GpuLimitDiagnostics {
            max_buffer_size: 1024,
            max_storage_buffer_binding_size: 2048,
            max_storage_buffers_per_shader_stage: 8,
        },
    };

    assert_eq!(
        gpu_adapter_diagnostics_json(&adapter)["timestamp_queries_supported"],
        true
    );
    assert_eq!(
        gpu_timestamp_timing_json(&adapter)["status"],
        "supported_not_requested"
    );
    adapter.timestamp_queries_requested = true;
    assert_eq!(
        gpu_timestamp_timing_json(&adapter)["status"],
        "requested_but_device_feature_missing"
    );
    adapter.timestamp_queries_enabled = true;
    assert_eq!(gpu_timestamp_timing_json(&adapter)["status"], "enabled");
}

#[test]
fn presentation_timing_json_names_proxy_and_compositor_timestamp_status() {
    let value = presentation_timing_json();

    assert_eq!(value["kind"], "presentation_timing");
    assert_eq!(value["taxonomy_version"], 1);
    assert_eq!(
        value["status"],
        "app_proxy_available_os_compositor_timestamp_unavailable"
    );
    assert_eq!(
        value["available_measurements"]["input_to_present_proxy"]["measurement_scope"],
        "automation_command_start_to_app_display_refresh_complete"
    );
    assert_eq!(
        value["available_measurements"]["gpu_compute_timestamp"]["sample_field"],
        "gpu_compute_ms"
    );
    assert_eq!(
        value["available_measurements"]["gpu_upload_wall_clock"]["sample_field"],
        "gpu_upload_ms"
    );
    assert_eq!(
        value["os_compositor_present_timestamp"]["status"],
        "unsupported_by_current_eframe_wgpu_integration"
    );
    assert_eq!(
        value["os_compositor_present_timestamp"]["winit_pre_present_notify"]["is_timestamp"],
        false
    );
}

#[test]
fn display_refresh_timing_json_uses_stable_phase_taxonomy() {
    let timing = DisplayRefreshTiming {
        path: crate::display_refresh::DisplayRefreshPath::GpuResidentDisplay,
        render_ms: 4.0,
        gpu_upload_ms: Some(1.5),
        gpu_compute_ms: Some(9.5),
        egui_texture_ms: 1.25,
        visible_brick_request_ms: 2.0,
        cpu_texture_update_ms: 0.5,
        total_ms: 14.0,
    };

    let value = display_refresh_timing_json(timing);

    assert_eq!(value["kind"], "display_refresh_timing");
    assert_eq!(value["taxonomy_version"], 1);
    assert_eq!(
        value["measurement_scope"],
        "app_display_refresh_cpu_wall_clock_with_optional_gpu_timestamp_query"
    );
    assert_eq!(
        value["phase_measurement_scopes"]["gpu_compute"],
        "wgpu_timestamp_query_elapsed_when_enabled"
    );
    assert_eq!(
        value["phase_measurement_scopes"]["gpu_upload"],
        "renderer_upload_cpu_wall_clock_subset_of_render"
    );
    assert_eq!(value["gpu_compute_timing_status"], "measured");
    assert_eq!(value["gpu_upload_timing_status"], "measured");
    assert_eq!(value["dominant_non_total_phase"], "gpu_compute");
    assert_eq!(value["phases_ms"]["render"], 4.0);
    assert_eq!(value["phases_ms"]["gpu_upload"], 1.5);
    assert_eq!(value["phases_ms"]["gpu_compute"], 9.5);
    assert_eq!(value["phases_ms"]["visible_brick_request"], 2.0);
    assert_eq!(value["phases_ms"]["total_refresh"], 14.0);
}

#[test]
fn app_update_timing_json_uses_stable_phase_taxonomy() {
    let timing = ProductAutomationAppUpdatePhases {
        setup_ms: 0.5,
        task_drain_ms: 1.0,
        playback_ms: 0.25,
        ui_build_ms: 6.0,
        histogram_ui_ms: 1.25,
        command_apply_ms: 0.75,
        display_refresh_trigger_ms: 4.0,
        import_action_ms: 0.0,
        brick_result_drain_ms: 2.0,
        background_repaint_request_ms: 0.1,
        automation_step_ms: 3.0,
        total_update_ms: 18.0,
    };

    let value = app_update_timing_json(timing);

    assert_eq!(value["kind"], "app_update_timing");
    assert_eq!(value["taxonomy_version"], 1);
    assert_eq!(
        value["measurement_scope"],
        "cpu_wall_clock_duration_inside_eframe_app_update"
    );
    assert_eq!(
        value["phase_measurement_scopes"]["display_refresh_trigger"],
        "cpu_wall_clock"
    );
    assert_eq!(value["dominant_non_total_phase"], "ui_build");
    assert_eq!(value["phases_ms"]["setup"], 0.5);
    assert_eq!(value["phases_ms"]["ui_build"], 6.0);
    assert_eq!(value["phases_ms"]["histogram_ui"], 1.25);
    assert_eq!(value["phases_ms"]["display_refresh_trigger"], 4.0);
    assert_eq!(value["phases_ms"]["automation_step"], 3.0);
    assert_eq!(value["phases_ms"]["total_update"], 18.0);
}

#[test]
fn app_update_timing_summary_reports_phase_percentiles() {
    let samples = vec![
        ProductAutomationAppUpdateSample {
            sample_index: 0,
            command_index: 2,
            event_epoch_ms: 10,
            timing: ProductAutomationAppUpdatePhases {
                setup_ms: 0.5,
                task_drain_ms: 1.0,
                playback_ms: 0.1,
                ui_build_ms: 5.0,
                histogram_ui_ms: 1.0,
                command_apply_ms: 1.0,
                display_refresh_trigger_ms: 3.0,
                import_action_ms: 0.0,
                brick_result_drain_ms: 2.0,
                background_repaint_request_ms: 0.1,
                automation_step_ms: 4.0,
                total_update_ms: 17.0,
            },
            background_work_active: true,
            active_timepoint: 0,
            render_mode: RenderMode::Mip,
            display_freshness: DisplayedFrameFreshness::Current,
            target_scale_level: 0,
            displayed_scale_level: Some(0),
            visible_bricks: 4,
            resident_bricks: 4,
        },
        ProductAutomationAppUpdateSample {
            sample_index: 1,
            command_index: 3,
            event_epoch_ms: 20,
            timing: ProductAutomationAppUpdatePhases {
                setup_ms: 0.25,
                task_drain_ms: 8.0,
                playback_ms: 0.2,
                ui_build_ms: 4.0,
                histogram_ui_ms: 2.0,
                command_apply_ms: 1.5,
                display_refresh_trigger_ms: 12.0,
                import_action_ms: 0.0,
                brick_result_drain_ms: 6.0,
                background_repaint_request_ms: 0.1,
                automation_step_ms: 3.0,
                total_update_ms: 37.0,
            },
            background_work_active: false,
            active_timepoint: 1,
            render_mode: RenderMode::Dvr,
            display_freshness: DisplayedFrameFreshness::Stale,
            target_scale_level: 1,
            displayed_scale_level: Some(2),
            visible_bricks: 8,
            resident_bricks: 3,
        },
    ];

    let summary = app_update_timing_summary_json(&samples);

    assert_eq!(summary["kind"], "app_update_timing_summary");
    assert_eq!(summary["taxonomy_version"], 1);
    assert_eq!(
        summary["measurement_scope"],
        "cpu_wall_clock_duration_inside_eframe_app_update"
    );
    assert_eq!(summary["sample_count"], 2);
    assert_eq!(summary["background_work_active_samples"], 1);
    assert_eq!(summary["phases_ms"]["ui_build"]["p50"], 4.0);
    assert_eq!(summary["phases_ms"]["ui_build"]["p95"], 5.0);
    assert_eq!(summary["phases_ms"]["histogram_ui"]["max"], 2.0);
    assert_eq!(summary["phases_ms"]["display_refresh_trigger"]["max"], 12.0);
    assert_eq!(
        summary["dominant_non_total_phase_counts"]["display_refresh_trigger"],
        1
    );
    assert_eq!(summary["dominant_non_total_phase_counts"]["ui_build"], 1);
    assert_eq!(
        summary["dominant_non_total_phase_by_p95"],
        "display_refresh_trigger"
    );
}

#[test]
fn display_refresh_timing_summary_reports_phase_percentiles() {
    let samples = vec![
        ProductAutomationDisplayRefreshSample {
            command_index: 2,
            command: "camera_orbit",
            event_epoch_ms: 10,
            timing: DisplayRefreshTiming {
                path: crate::display_refresh::DisplayRefreshPath::GpuResidentDisplay,
                render_ms: 4.0,
                gpu_upload_ms: Some(1.0),
                gpu_compute_ms: Some(7.0),
                egui_texture_ms: 1.0,
                visible_brick_request_ms: 2.0,
                cpu_texture_update_ms: 0.0,
                total_ms: 9.0,
            },
        },
        ProductAutomationDisplayRefreshSample {
            command_index: 3,
            command: "camera_pan",
            event_epoch_ms: 20,
            timing: DisplayRefreshTiming {
                path: crate::display_refresh::DisplayRefreshPath::CpuTexture,
                render_ms: 11.0,
                gpu_upload_ms: None,
                gpu_compute_ms: None,
                egui_texture_ms: 0.5,
                visible_brick_request_ms: 3.0,
                cpu_texture_update_ms: 5.0,
                total_ms: 19.0,
            },
        },
        ProductAutomationDisplayRefreshSample {
            command_index: 4,
            command: "camera_zoom",
            event_epoch_ms: 30,
            timing: DisplayRefreshTiming {
                path: crate::display_refresh::DisplayRefreshPath::GpuResidentDisplay,
                render_ms: 6.0,
                gpu_upload_ms: Some(2.0),
                gpu_compute_ms: Some(12.0),
                egui_texture_ms: 1.5,
                visible_brick_request_ms: 4.0,
                cpu_texture_update_ms: 0.0,
                total_ms: 17.0,
            },
        },
    ];

    let summary = display_refresh_timing_summary_json(&samples);

    assert_eq!(summary["kind"], "display_refresh_timing_summary");
    assert_eq!(summary["taxonomy_version"], 1);
    assert_eq!(
        summary["measurement_scope"],
        "app_display_refresh_cpu_wall_clock_with_optional_gpu_timestamp_query"
    );
    assert_eq!(
        summary["phase_measurement_scopes"]["gpu_compute"],
        "wgpu_timestamp_query_elapsed_when_enabled"
    );
    assert_eq!(
        summary["phase_measurement_scopes"]["gpu_upload"],
        "renderer_upload_cpu_wall_clock_subset_of_render"
    );
    assert_eq!(
        summary["gpu_upload_timing_status"],
        "measured_in_some_samples"
    );
    assert_eq!(
        summary["gpu_compute_timing_status"],
        "measured_in_some_samples"
    );
    assert_eq!(summary["sample_count"], 3);
    assert_eq!(summary["path_counts"]["gpu display"], 2);
    assert_eq!(summary["path_counts"]["cpu texture"], 1);
    assert_eq!(summary["phases_ms"]["render"]["p50"], 6.0);
    assert_eq!(summary["phases_ms"]["render"]["p95"], 11.0);
    assert_eq!(summary["phases_ms"]["gpu_upload"]["sample_count"], 2);
    assert_eq!(summary["phases_ms"]["gpu_upload"]["p95"], 2.0);
    assert_eq!(summary["phases_ms"]["gpu_compute"]["sample_count"], 2);
    assert_eq!(summary["phases_ms"]["gpu_compute"]["max"], 12.0);
    assert_eq!(summary["dominant_non_total_phase_counts"]["gpu_compute"], 2);
    assert_eq!(summary["dominant_non_total_phase_counts"]["render"], 1);
    assert_eq!(summary["dominant_non_total_phase_by_p95"], "gpu_compute");
}

#[test]
fn input_to_present_timing_summary_reports_proxy_latency_percentiles() {
    let samples = vec![
        ProductAutomationInputToPresentSample {
            command_index: 2,
            command: "camera_orbit",
            event_epoch_ms: 10,
            latency_ms: 18.0,
            display_refresh_timing: DisplayRefreshTiming {
                path: crate::display_refresh::DisplayRefreshPath::GpuResidentDisplay,
                render_ms: 4.0,
                gpu_upload_ms: Some(1.0),
                gpu_compute_ms: Some(7.0),
                egui_texture_ms: 1.0,
                visible_brick_request_ms: 2.0,
                cpu_texture_update_ms: 0.0,
                total_ms: 9.0,
            },
        },
        ProductAutomationInputToPresentSample {
            command_index: 3,
            command: "camera_pan",
            event_epoch_ms: 20,
            latency_ms: 32.0,
            display_refresh_timing: DisplayRefreshTiming {
                path: crate::display_refresh::DisplayRefreshPath::CpuTexture,
                render_ms: 11.0,
                gpu_upload_ms: None,
                gpu_compute_ms: None,
                egui_texture_ms: 0.5,
                visible_brick_request_ms: 3.0,
                cpu_texture_update_ms: 5.0,
                total_ms: 19.0,
            },
        },
        ProductAutomationInputToPresentSample {
            command_index: 4,
            command: "camera_orbit",
            event_epoch_ms: 30,
            latency_ms: 24.0,
            display_refresh_timing: DisplayRefreshTiming {
                path: crate::display_refresh::DisplayRefreshPath::GpuResidentDisplay,
                render_ms: 6.0,
                gpu_upload_ms: Some(2.0),
                gpu_compute_ms: Some(12.0),
                egui_texture_ms: 1.5,
                visible_brick_request_ms: 4.0,
                cpu_texture_update_ms: 0.0,
                total_ms: 17.0,
            },
        },
    ];

    let sample = samples[0].json();
    let summary = input_to_present_timing_summary_json(&samples);

    assert_eq!(sample["kind"], "input_to_present_proxy_timing");
    assert_eq!(sample["taxonomy_version"], 1);
    assert_eq!(
        sample["measurement_scope"],
        "automation_command_start_to_app_display_refresh_complete"
    );
    assert_eq!(sample["presentation_proxy"], "app_display_refresh_complete");
    assert_eq!(summary["kind"], "input_to_present_proxy_timing_summary");
    assert_eq!(summary["taxonomy_version"], 1);
    assert_eq!(summary["sample_count"], 3);
    assert_eq!(summary["latency_ms"]["p50"], 24.0);
    assert_eq!(summary["latency_ms"]["p95"], 32.0);
    assert_eq!(summary["path_counts"]["gpu display"], 2);
    assert_eq!(summary["path_counts"]["cpu texture"], 1);
    assert_eq!(summary["command_counts"]["camera_orbit"], 2);
    assert_eq!(summary["command_counts"]["camera_pan"], 1);
}

#[test]
fn cross_section_latency_summary_reports_operation_gate_rows() {
    let samples = vec![
        ProductAutomationCrossSectionLatencySample {
            command_index: 10,
            command: "cross_section_pan",
            operation: "pan",
            panel_id: PanelId::Xz,
            event_epoch_ms: 10,
            latency_ms: 18.0,
            target_generation: 2,
            displayed_generation: 2,
            active_timepoint: 0,
            target_scale_level: Some(0),
            render_scale_level: Some(0),
            missing_occupied_chunks: 0,
        },
        ProductAutomationCrossSectionLatencySample {
            command_index: 11,
            command: "cross_section_zoom",
            operation: "zoom",
            panel_id: PanelId::Xz,
            event_epoch_ms: 20,
            latency_ms: 35.0,
            target_generation: 3,
            displayed_generation: 4,
            active_timepoint: 0,
            target_scale_level: Some(0),
            render_scale_level: Some(0),
            missing_occupied_chunks: 1,
        },
        ProductAutomationCrossSectionLatencySample {
            command_index: 12,
            command: "cross_section_pan",
            operation: "pan",
            panel_id: PanelId::Xy,
            event_epoch_ms: 30,
            latency_ms: 42.0,
            target_generation: 5,
            displayed_generation: 5,
            active_timepoint: 0,
            target_scale_level: Some(1),
            render_scale_level: Some(1),
            missing_occupied_chunks: 0,
        },
    ];

    let sample = samples[0].json();
    let summary = cross_section_latency_summary_json(&samples, 1);

    assert_eq!(
        sample["kind"],
        "cross_section_command_to_current_partial_latency"
    );
    assert_eq!(
        sample["presentation_proxy"],
        "panel_displayed_generation_with_gpu_display_frame"
    );
    assert_eq!(sample["operation"], "pan");
    assert_eq!(sample["panel"], "XZ");
    assert_eq!(summary["kind"], "cross_section_latency_summary");
    assert_eq!(summary["sample_count"], 3);
    assert_eq!(summary["pending_sample_count"], 1);
    assert_eq!(summary["latency_ms"]["p50"], 35.0);
    assert_eq!(summary["latency_ms"]["p95"], 42.0);
    assert_eq!(summary["operation_counts"]["pan"], 2);
    assert_eq!(summary["operation_counts"]["zoom"], 1);
    assert_eq!(summary["panel_counts"]["XZ"], 2);
    assert_eq!(summary["by_operation"]["pan"]["latency_ms"]["p95"], 42.0);
    assert_eq!(
        summary["by_operation"]["pan"]["warm_interaction_gate"]["status"],
        "passed"
    );
    assert_eq!(summary["by_panel"]["XZ"]["p50"], 18.0);
}

#[test]
fn timing_values_summary_ignores_missing_or_nonfinite_samples() {
    let summary = timing_values_summary_json(vec![4.0, f64::NAN, 12.0, 6.0]);

    assert_eq!(summary["sample_count"], 3);
    assert_eq!(summary["p50"], 6.0);
    assert_eq!(summary["p95"], 12.0);
    assert_eq!(summary["p99"], 12.0);
    assert_eq!(summary["max"], 12.0);
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

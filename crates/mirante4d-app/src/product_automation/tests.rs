use super::capture::{
    ProductAutomationArtifact, ProductAutomationImageStats, color_image_from_rgba,
    sanitize_artifact_label, write_color_image_ppm,
};
use super::diagnostics::dataset_runtime_diagnostics_json;
use super::*;
use std::{fs, path::PathBuf};

use mirante4d_dataset_runtime::{DatasetRuntimeConfig, DatasetRuntimeDiagnostics};

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
fn automation_script_parses_retained_four_panel_assertions() {
    let raw = r#"
        {
          "schema": "mirante4d-product-automation-script",
          "schema_version": 2,
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

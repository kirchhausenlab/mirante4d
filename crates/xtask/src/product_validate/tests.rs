use super::*;

#[test]
fn x11_automation_helpers_have_short_silence_absolute_and_output_bounds() {
    assert_eq!(
        X11_AUTOMATION_OUTPUT_POLICY.inactivity_timeout,
        Duration::from_secs(2)
    );
    assert_eq!(
        X11_AUTOMATION_OUTPUT_POLICY.absolute_timeout,
        Duration::from_secs(3)
    );
    assert!(
        X11_AUTOMATION_OUTPUT_POLICY.inactivity_timeout
            < X11_AUTOMATION_OUTPUT_POLICY.absolute_timeout
    );
    assert_eq!(X11_AUTOMATION_OUTPUT_POLICY.max_stdout_bytes, 64 * 1024);
    assert_eq!(X11_AUTOMATION_OUTPUT_POLICY.max_stderr_bytes, 64 * 1024);
}

fn assert_dataset_runtime_limits(script: &Value, total_bytes: u64, resident_resources: u64) {
    assert_eq!(SCRIPT_SCHEMA_VERSION, 8);
    assert_eq!(REPORT_SCHEMA_VERSION, 8);
    assert_eq!(script["schema_version"], 8);
    assert_eq!(
        script["hard_safety_limits"]["max_cpu_total_bytes"],
        total_bytes
    );
    assert_eq!(
        script["hard_safety_limits"]["max_cpu_decoded_residency_bytes"],
        total_bytes / 2
    );
    assert_eq!(
        script["hard_safety_limits"]["max_cpu_in_flight_decode_bytes"],
        (total_bytes / 8)
            .saturating_add(PACKAGE_VALIDATION_WORKING_BYTES)
            .min(total_bytes)
    );
    assert_eq!(
        script["hard_safety_limits"]["max_runtime_queued_requests"],
        1_024
    );
    assert_eq!(
        script["hard_safety_limits"]["max_runtime_in_flight_decodes"],
        8
    );
    assert_eq!(
        script["hard_safety_limits"]["max_runtime_pending_completions"],
        1_024
    );
    assert_eq!(
        script["hard_safety_limits"]["max_runtime_resident_resources"],
        resident_resources
    );
}

fn bind_report_to_script(report: &mut Value, script: &Value, script_path: &Path) {
    report["schema"] = json!(PRODUCT_AUTOMATION_REPORT_SCHEMA);
    report["schema_version"] = json!(REPORT_SCHEMA_VERSION);
    report["status"] = json!("passed");
    report["failure_reason"] = Value::Null;
    report["script"] = json!({
        "path": script_path,
        "schema": script["schema"],
        "schema_version": script["schema_version"],
        "scenario": script["scenario"],
        "command_count": script["commands"].as_array().unwrap().len(),
    });
    report["hard_safety_limits"] =
        canonical_product_automation_hard_safety_limits(&script["hard_safety_limits"]).unwrap();
}

fn bound_automation_report(mut report: Value) -> (tempfile::TempDir, Value, PathBuf, Value) {
    let tempdir = tempfile::tempdir().unwrap();
    let script_path = tempdir.path().join("product-automation-script.json");
    let script = target_fixture_camera_smoke_script(&tempdir.path().join("fixture.m4d"));
    write_json_file(&script_path, &script).unwrap();
    bind_report_to_script(&mut report, &script, &script_path);
    (tempdir, script, script_path, report)
}

fn b3_exact_capture_report(second_width: u64) -> Value {
    json!({
        "status": "passed",
        "artifacts": [
            {
                "kind": "viewport_capture",
                "capture_source": "gpu_display_frame_readback",
                "path": format!("artifacts/{B3_PRIMARY_E1_CAPTURE}.ppm"),
                "width": B3_VIEWPORT_WIDTH,
                "height": B3_VIEWPORT_HEIGHT,
                "command_index": 20,
                "target": "three_d",
                "frame_identity": 20,
                "surface_generation": 2,
                "pixel_stats": {
                    "pixel_count": u64::from(B3_VIEWPORT_WIDTH) * u64::from(B3_VIEWPORT_HEIGHT),
                    "nonzero_rgb_pixels": 1,
                    "max_rgb": 255
                }
            },
            {
                "kind": "viewport_capture",
                "capture_source": "gpu_display_frame_readback",
                "path": format!("artifacts/{B3_SECONDARY_E1_CAPTURE}.ppm"),
                "width": second_width,
                "height": B3_SECOND_VIEWPORT_HEIGHT,
                "command_index": 30,
                "target": "three_d",
                "frame_identity": 30,
                "surface_generation": 3,
                "pixel_stats": {
                    "pixel_count": second_width * u64::from(B3_SECOND_VIEWPORT_HEIGHT),
                    "nonzero_rgb_pixels": 1,
                    "max_rgb": 255
                }
            }
        ]
    })
}

fn b4_valid_checkpoint() -> Value {
    json!({
        "schema": B4_CHECKPOINT_SCHEMA,
        "schema_version": 1,
        "stage": B4_CHECKPOINT_STAGE,
        "written_at_epoch_ms": 1,
        "viewport_evidence": {
            "requested_mapped_client_pixels": {
                "width": B4_PRIMARY_CLIENT_WIDTH,
                "height": B4_PRIMARY_CLIENT_HEIGHT
            }
        },
        "project_state": {
            "bound": true,
            "dirty": true,
            "current_revision": {"project_id": "project", "sequence": 3},
            "saved_revision": {"project_id": "project", "sequence": 2},
            "lifecycle": "established",
            "can_save": true,
            "can_save_as": true,
            "manual": true,
            "autosave": true,
            "current_manual": "manual-generation",
            "current_autosave": "autosave-generation"
        },
        "project_evidence": {
            "initial_save_captured_revision": {"project_id": "project", "sequence": 2},
            "latest_autosave_captured_revision": {"project_id": "project", "sequence": 3},
            "autosave_elapsed_from_durable_edit_ms": B4_AUTOSAVE_MIN_ELAPSED_MS,
            "autosave_wait_mode": "scheduled_deadline_no_busy_poll",
            "close_result": null,
            "actor_join": null
        }
    })
}

fn b4_normal_automation_report(width: u32, height: u32) -> Value {
    json!({
        "status": "passed",
        "viewport_evidence": {
            "requested_mapped_client_pixels": {"width": width, "height": height},
            "observed_client_area_pixels": null
        },
        "project_store_evidence": {
            "close_result": {"status": "succeeded", "fault": null},
            "actor_join": {"status": "succeeded", "error": null}
        },
        "artifacts": [{
            "kind": "viewport_capture",
            "capture_source": "gpu_display_frame_readback",
            "path": "capture.ppm",
            "width": 16,
            "height": 16,
            "target": "three_d",
            "frame_identity": 1,
            "surface_generation": 1,
            "pixel_stats": {
                "pixel_count": 256,
                "nonzero_rgb_pixels": 1,
                "max_rgb": 255
            }
        }]
    })
}

fn b4_valid_attempt(number: u64, root: &Path) -> Value {
    let (width, height) = if number == 1 {
        (B4_PRIMARY_CLIENT_WIDTH, B4_PRIMARY_CLIENT_HEIGHT)
    } else {
        (B4_SECONDARY_CLIENT_WIDTH, B4_SECONDARY_CLIENT_HEIGHT)
    };
    let package = root.join("fixture.m4d");
    let original = root.join("original.m4dproj");
    let save_as = root.join("save-as.m4dproj");
    let checkpoint_path = root.join("external-kill-checkpoint.json");
    let script = match number {
        1 => b4_launch_one_script(&package, &original, &checkpoint_path),
        2 => b4_launch_two_script(&package, &original, &save_as),
        3 => b4_launch_three_script(&package, &save_as),
        _ => panic!("unsupported B4 test attempt"),
    };
    let script_path = root.join(format!("launch-{number}-script.json"));
    write_json_file(&script_path, &script).unwrap();
    let (signal, exit_success, external_sigkill_sent, checkpoint, mut automation_report) =
        if number == 1 {
            (
                json!(9),
                json!(false),
                true,
                b4_valid_checkpoint(),
                Value::Null,
            )
        } else {
            (
                Value::Null,
                json!(true),
                false,
                Value::Null,
                b4_normal_automation_report(width, height),
            )
        };
    if number != 1 {
        bind_report_to_script(&mut automation_report, &script, &script_path);
    }
    json!({
        "attempt": number,
        "phase": format!("launch-{number}"),
        "retry_index": 0,
        "status": "passed",
        "failure_reason": null,
        "script": script_path,
        "requested_client_area_pixels": {"width": width, "height": height},
        "process": {
            "timed_out": false,
            "exit_status": if number == 1 { "signal: 9" } else { "exit status: 0" },
            "exit_success": exit_success,
            "signal": signal,
            "external_sigkill_sent": external_sigkill_sent,
            "checkpoint": checkpoint,
            "observed_client_area_pixels": {
                "width": width,
                "height": height,
                "window_id": "0x1",
                "map_state": "is_viewable",
                "observation": "xdotool_pid_search_plus_xwininfo_client_geometry"
            },
            "fullscreen_action": null,
            "control_failure": null
        },
        "automation_report": automation_report,
        "source_closure_evidence": {"byte_identical": true},
        "project_package_evidence": {"exists": true, "is_directory": true}
    })
}

#[test]
fn product_validation_binary_override_selects_existing_file_and_skips_build() {
    let tempdir = tempfile::tempdir().unwrap();
    let packaged_binary = tempdir.path().join("mirante4d-app");
    fs::write(&packaged_binary, b"packaged executable").unwrap();

    let resolved = ProductValidationAppBinary::resolve(Some(packaged_binary.clone())).unwrap();

    assert_eq!(resolved.path(), packaged_binary);
    assert!(resolved.overridden);
    assert!(!resolved.should_build_default_release());
    resolved.validate_for_launch().unwrap();
}

#[test]
fn product_validation_binary_override_rejects_missing_or_directory_paths() {
    let tempdir = tempfile::tempdir().unwrap();
    let missing = tempdir.path().join("missing-mirante4d-app");

    let missing_error = ProductValidationAppBinary::resolve(Some(missing))
        .unwrap_err()
        .to_string();
    assert!(missing_error.contains(APP_BINARY_ENV));
    assert!(missing_error.contains("failed to inspect"));

    let directory_error = ProductValidationAppBinary::resolve(Some(tempdir.path().to_path_buf()))
        .unwrap_err()
        .to_string();
    assert!(directory_error.contains(APP_BINARY_ENV));
    assert!(directory_error.contains("is not a file"));
}

#[test]
fn target_fixture_product_automation_script_uses_semantic_commands() {
    let script = target_fixture_camera_smoke_script(Path::new("/tmp/demo.m4d"));
    let commands = script["commands"].as_array().unwrap();

    assert_eq!(script["schema"], PRODUCT_AUTOMATION_SCRIPT_SCHEMA);
    assert_eq!(script["scenario"], GENERATED_FIXTURE_SCENARIO);
    assert_dataset_runtime_limits(&script, 128 * MIB, 128);
    assert!(
        commands
            .iter()
            .any(|command| command["command"] == "camera_orbit")
    );
    assert!(
        commands
            .iter()
            .any(|command| command["command"] == "probe_hover")
    );
    assert!(
        commands
            .iter()
            .any(|command| command["command"] == "copy_diagnostics")
    );
    assert!(commands.iter().any(|command| {
        command["command"] == "capture_screenshot" && command["target"] == "three_d"
    }));
    assert_eq!(commands.last().unwrap()["command"], "quit");
}

#[test]
fn representative_native_navigation_script_exercises_the_real_navigation_cut() {
    let script = representative_native_navigation_script(Path::new("/tmp/cell.m4d"));
    let commands = script["commands"].as_array().unwrap();

    assert_eq!(script["schema"], PRODUCT_AUTOMATION_SCRIPT_SCHEMA);
    assert_eq!(
        script["scenario"],
        REPRESENTATIVE_NATIVE_NAVIGATION_SCENARIO
    );
    assert_dataset_runtime_limits(&script, 2_048 * MIB, 16_384);
    assert_eq!(
        script_render_modes_json(&script),
        json!(["mip", "dvr", "iso"])
    );
    assert!(commands.iter().any(|command| {
        command["command"] == "set_layer_sampling" && command["sampling"] == "smooth_linear"
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "set_viewer_layout" && command["layout"] == "four_panel"
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "set_viewer_layout" && command["layout"] == "single3d"
    }));
    let orbit = commands
        .iter()
        .position(|command| command["command"] == "camera_orbit_sequence")
        .unwrap();
    let linked = commands
        .iter()
        .enumerate()
        .skip(orbit + 1)
        .find(|(_, command)| command["command"] == "cross_section_rotate_sequence")
        .map(|(index, _)| index)
        .unwrap();
    assert!(
        orbit < linked,
        "linked-only input must follow unfinished 3D settlement"
    );
    assert!(commands[orbit + 1..linked].iter().all(|command| {
        command["command"] != "wait_for"
            || command["condition"] != "coordinated_presentation_settled"
    }));
    assert_eq!(commands.last().unwrap()["command"], "quit");
    validate_product_automation_script(&script).unwrap();
}

#[test]
fn target_fixture_render_modes_script_switches_supported_modes() {
    let script = target_fixture_render_modes_script(Path::new("/tmp/demo.m4d"));
    let commands = script["commands"].as_array().unwrap();
    let command_names: Vec<_> = commands
        .iter()
        .filter_map(|command| command["command"].as_str())
        .collect();

    assert_eq!(script["schema"], PRODUCT_AUTOMATION_SCRIPT_SCHEMA);
    assert_eq!(script["scenario"], GENERATED_RENDER_MODES_SCENARIO);
    assert_eq!(script["gpu_timing"], false);
    assert_dataset_runtime_limits(&script, 128 * MIB, 192);
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["width"],
        GENERATED_VIEWPORT_WIDTH
    );
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["height"],
        GENERATED_VIEWPORT_HEIGHT
    );
    assert_eq!(
        script_render_modes_json(&script),
        json!(["mip", "dvr", "iso"])
    );
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "set_render_mode")
            .count(),
        5
    );
    assert!(commands.iter().any(|command| {
        command["command"] == "set_layer_window"
            && command["layer_index"].as_u64() == Some(0)
            && command["low"].as_f64() == Some(0.0)
            && command["high"].as_f64() == Some(4096.0)
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "set_layer_window"
            && command["layer_index"].as_u64() == Some(1)
            && command["low"].as_f64() == Some(20000.0)
            && command["high"].as_f64() == Some(24096.0)
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "set_layer_opacity"
            && command["layer_index"].as_u64() == Some(0)
            && command["opacity"].as_f64() == Some(1.0)
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "set_projection" && command["projection"] == "perspective"
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "set_layer_sampling"
            && command["layer_index"].as_u64() == Some(0)
            && command["sampling"] == "smooth_linear"
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "set_layer_iso_shading"
            && command["layer_index"].as_u64() == Some(0)
            && command["shading"] == "gradient_lighting"
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "set_layer_opacity"
            && command["layer_index"].as_u64() == Some(1)
            && command["opacity"].as_f64() == Some(1.0)
    }));
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "set_layer_render_mode")
            .count(),
        5
    );
    assert!(commands.iter().any(|command| {
        command["command"] == "set_layer_render_mode"
            && command["layer_index"].as_u64() == Some(1)
            && command["mode"].as_str() == Some("dvr")
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "set_dvr_density_scale"
            && command["density_scale"].as_f64() == Some(12.0)
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "set_iso_display_level"
            && command["display_level"].as_f64() == Some(0.05)
    }));
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "copy_diagnostics")
            .count(),
        5
    );
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "probe_hover")
            .count(),
        4
    );
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "set_iso_light")
            .count(),
        2
    );
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "primary_click")
            .count(),
        5
    );
    assert!(
        commands
            .iter()
            .any(|command| command["name"] == "generated-mip")
    );
    assert!(
        commands
            .iter()
            .any(|command| command["name"] == "generated-dvr")
    );
    assert!(
        commands
            .iter()
            .any(|command| command["name"] == "generated-iso-detached-light")
    );
    assert!(
        commands
            .iter()
            .any(|command| command["name"] == "generated-mixed-mip-dvr")
    );
    for (name, target) in [
        ("generated-linked-panels-3d", "three_d"),
        ("generated-linked-panels-xy", "xy"),
        ("generated-linked-panels-xz", "xz"),
        ("generated-linked-panels-yz", "yz"),
    ] {
        assert!(commands.iter().any(|command| {
            command["command"] == "capture_screenshot"
                && command["name"] == name
                && command["target"] == target
        }));
    }
    let mixed_capture = commands
        .iter()
        .position(|command| command["name"] == "generated-mixed-mip-dvr")
        .unwrap();
    let previous_homogeneous_capture = commands
        .iter()
        .position(|command| command["name"] == "generated-iso-attached-light")
        .unwrap();
    let mixed_commands = &commands[previous_homogeneous_capture + 1..=mixed_capture];
    assert!(
        mixed_commands
            .iter()
            .any(|command| { command["command"] == "set_render_mode" && command["mode"] == "mip" })
    );
    assert!(mixed_commands.iter().any(|command| {
        command["command"] == "set_layer_render_mode"
            && command["layer_index"] == 1
            && command["mode"] == "dvr"
    }));
    assert!(mixed_commands.iter().any(|command| {
        command["command"] == "wait_for"
            && command["condition"] == "coordinated_presentation_settled"
    }));
    assert!(
        mixed_commands
            .iter()
            .any(|command| { command["condition"]["nonblank_panel"]["target"] == "three_d" })
    );
    assert!(
        mixed_commands
            .iter()
            .any(|command| command["condition"] == "no_render_error")
    );
    assert!(mixed_commands.iter().any(|command| {
        command["condition"]["frame_fidelity"]["scale_level"] == 0
            && command["condition"]["frame_fidelity"]["complete"] == true
            && command["condition"]["frame_fidelity"]["exact"] == true
    }));
    for policy in [
        "mip_argmax",
        "maximum_opacity_contribution",
        "first_threshold_hit",
    ] {
        assert!(commands.iter().any(|command| {
            command["condition"]["pick_evidence"]["policy"].as_str() == Some(policy)
        }));
    }
    let validated_probe_policies = commands
        .windows(2)
        .filter(|pair| pair[0]["command"] == "probe_hover")
        .map(|pair| {
            pair[1]["condition"]["pick_evidence"]["policy"]
                .as_str()
                .expect("every probe is immediately validated")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        validated_probe_policies,
        vec![
            "mip_argmax",
            "maximum_opacity_contribution",
            "first_threshold_hit",
            "mip_argmax",
        ]
    );
    assert!(commands.iter().all(|command| {
        command["command"] != "sleep_frames" || command["frames"].as_u64() != Some(8)
    }));
    let four_panel = commands
        .iter()
        .position(|command| {
            command["command"] == "set_viewer_layout" && command["layout"] == "four_panel"
        })
        .unwrap();
    let distinct = commands
        .iter()
        .position(|command| {
            command["condition"]
                .get("four_panel_images_distinct")
                .is_some()
        })
        .unwrap();
    assert!(commands[four_panel + 1..distinct].iter().any(|command| {
        command["command"] == "wait_for"
            && command["condition"] == "coordinated_presentation_settled"
    }));
    for condition in ["crosshair_linked", "roi_committed", "distance_committed"] {
        assert!(
            commands
                .iter()
                .any(|command| command["condition"] == condition)
        );
    }
    assert!(commands.iter().any(|command| {
        command["condition"]["viewer_layout"]["layout"].as_str() == Some("four_panel")
    }));
    assert!(commands.iter().any(|command| {
        command["condition"]["cross_section_panel_schedule"]["panel"].as_str() == Some("xz")
    }));
    assert!(commands.iter().any(|command| {
        command["condition"]["render_target_pixels"]["width"].as_u64()
            == Some(u64::from(GENERATED_RESIZED_VIEWPORT_WIDTH))
            && command["condition"]["render_target_pixels"]["height"].as_u64()
                == Some(u64::from(GENERATED_RESIZED_VIEWPORT_HEIGHT))
    }));
    assert!(
        commands
            .iter()
            .any(|command| { command["condition"]["render_mode"]["mode"].as_str() == Some("iso") })
    );
    assert_eq!(commands.last().unwrap()["command"], "quit");
}

#[test]
fn target_source_verification_script_proves_cancel_progress_success_and_both_sizes() {
    let script = target_source_verification_script(Path::new("/tmp/demo.m4d"));
    let commands = script["commands"].as_array().unwrap();

    assert_eq!(script["scenario"], B3_SOURCE_VERIFICATION_SCENARIO);
    assert_eq!(commands[0]["command"], "open_dataset");
    let initial_verified_wait = commands
        .iter()
        .position(|command| {
            command["command"] == "wait_for"
                && command["condition"] == "source_verification_verified"
        })
        .unwrap();
    let cancellation = commands
        .iter()
        .position(|command| command["command"] == "cancel_source_verification")
        .unwrap();
    let retry = commands
        .iter()
        .position(|command| command["command"] == "request_source_verification")
        .unwrap();
    assert!(initial_verified_wait < cancellation);
    assert!(
        commands
            .iter()
            .any(|command| command["command"] == "cancel_source_verification")
    );
    assert!(
        commands
            .iter()
            .any(|command| command["command"] == "request_source_verification")
    );
    assert!(commands.iter().any(|command| {
        command["condition"]["source_verification_evidence"]["min_accepted_progress_updates"] == 1
            && command["condition"]["source_verification_evidence"]["min_cancelled_runs"] == 1
            && command["condition"]["source_verification_evidence"]["min_accepted_successes"] == 1
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "set_viewport_size"
            && command["width"] == B3_VIEWPORT_WIDTH
            && command["height"] == B3_VIEWPORT_HEIGHT
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "set_render_target_size"
            && command["width"] == B3_VIEWPORT_WIDTH
            && command["height"] == B3_VIEWPORT_HEIGHT
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "set_render_target_size"
            && command["width"] == B3_SECOND_VIEWPORT_WIDTH
            && command["height"] == B3_SECOND_VIEWPORT_HEIGHT
    }));
    assert!(commands.iter().any(|command| {
        command["condition"]["render_target_pixels"]["width"] == B3_VIEWPORT_WIDTH
            && command["condition"]["render_target_pixels"]["height"] == B3_VIEWPORT_HEIGHT
    }));
    assert!(commands.iter().any(|command| {
        command["condition"]["render_target_pixels"]["width"] == B3_SECOND_VIEWPORT_WIDTH
            && command["condition"]["render_target_pixels"]["height"] == B3_SECOND_VIEWPORT_HEIGHT
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "set_viewport_size"
            && command["width"] == B3_SECOND_VIEWPORT_WIDTH
            && command["height"] == B3_SECOND_VIEWPORT_HEIGHT
    }));
    assert!(commands[cancellation + 1..retry].iter().all(|command| {
        command["command"] != "capture_screenshot"
            && command["condition"].get("nonblank_panel").is_none()
            && command["condition"].get("render_target_pixels").is_none()
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "capture_screenshot"
            && command["name"] == "b3-after-success-1920x1080"
            && command["target"] == "three_d"
    }));
    assert_eq!(commands.last().unwrap()["command"], "quit");
    validate_product_automation_script(&script).unwrap();
}

#[test]
fn import_preprocessing_script_uses_normal_cancel_resume_open_ready_path() {
    let script = import_preprocessing_script(
        Path::new("/tmp/startup.m4d"),
        Path::new("/tmp/source"),
        Path::new("/tmp/output"),
        Path::new("/tmp/output/source.m4d"),
    );
    let commands = script["commands"].as_array().unwrap();
    assert_eq!(script["scenario"], IMPORT_PREPROCESSING_SCENARIO);
    assert_dataset_runtime_limits(&script, 512 * MIB, IMPORT_RESIDENT_RESOURCE_LIMIT);
    validate_product_automation_script(&script).unwrap();

    let starts = commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| {
            (command["command"] == "start_reviewed_import").then_some(index)
        })
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 2);
    let progress = commands
        .iter()
        .position(|command| command["command"] == "wait_for_import_progress")
        .unwrap();
    let cancel = commands
        .iter()
        .position(|command| command["command"] == "cancel_import")
        .unwrap();
    let open_ready = commands
        .iter()
        .position(|command| command["command"] == "wait_for_imported_open_ready")
        .unwrap();
    assert!(starts[0] < progress && progress < cancel && cancel < starts[1]);
    assert!(starts[1] < open_ready);
    assert!(
        commands[open_ready + 1..]
            .iter()
            .all(|command| command["command"] != "open_dataset"),
        "the imported verified source must render directly without an external reopen"
    );
    assert_eq!(
        commands[progress]["minimum_completed_work_units"],
        IMPORT_DURABLE_PREFIX_WORK_UNITS
    );
    assert!(commands.iter().any(|command| {
        command["condition"]["import_workflow_evidence"]["required_stage_names"]
            .as_array()
            .is_some_and(|stages| stages.iter().any(|stage| stage == "commit"))
            && command["condition"]["import_workflow_evidence"]["min_cancelled_runs"] == 1
            && command["condition"]["import_workflow_evidence"]["min_successful_runs"] == 1
            && command["condition"]["import_workflow_evidence"]["min_resumed_work_units"]
                == IMPORT_DURABLE_PREFIX_WORK_UNITS
            && command["condition"]["import_workflow_evidence"]["min_projected_elapsed_ms"] == 1
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "capture_screenshot"
            && command["name"] == "import-preprocessing-open-ready-navigation"
            && command["target"] == "three_d"
    }));
    assert_eq!(commands.last().unwrap()["command"], "quit");
}

#[test]
fn import_preprocessing_evidence_requires_named_progress_resume_and_open_ready() {
    let report = json!({
        "schema": PRODUCT_AUTOMATION_REPORT_SCHEMA,
        "schema_version": REPORT_SCHEMA_VERSION,
        "import_workflow_evidence": {
            "worker_emitted_stage_names": [
                "planning-and-preflight",
                "source-revalidation",
                "checkpoint-open-or-resume",
                "base-production",
                "pyramid-production",
                "source-scientific-identity",
                "shard-publication",
                "staged-structure-validation",
                "staged-exact-validation",
                "staged-scientific-validation",
                "commit"
            ],
            "projected_named_stage_observations": ["base-production", "pyramid-production"],
            "cancelled_runs": 1,
            "successful_runs": 1,
            "failed_runs": 0,
            "published_events": 1,
            "maximum_resumed_work_units": IMPORT_DURABLE_PREFIX_WORK_UNITS,
            "maximum_peak_working_bytes": IMPORT_WORKING_MEMORY_BYTES,
            "maximum_elapsed_ms": 2,
            "maximum_projected_elapsed_ms": 1,
            "publication_to_open_ready_clock": {
                "status": IMPORT_OPEN_READY_COMPLETE_STATUS,
                "transfer_mode": "staged_verified_capability",
                "included_in_primary_clock": true,
                "publication_currentness_execution": {
                    "contract_id": PUBLICATION_CURRENTNESS_CONTRACT_ID,
                    "expected_snapshot_object_reads": 7,
                    "first_inventory_object_reads": 11,
                    "observed_snapshot_object_reads": 7,
                    "second_inventory_object_reads": 11,
                    "observed_total_object_reads": 29,
                    "observed_codec_decode_calls": 0
                },
                "source_verification_started_runs": 0,
                "source_verification_progress_updates": 0,
                "source_verification_cancelled_runs": 0,
                "source_verification_failed_runs": 0,
                "source_verification_successes": 0
            }
        },
        "events": [{
            "command": "wait_for_imported_open_ready",
            "status": "passed",
            "details": {
                "verified": true,
                "normal_product_open_path": true
            }
        }]
    });
    assert!(import_preprocessing_evidence(Some(&report)).is_ok());

    let mut unavailable_transfer = report.clone();
    unavailable_transfer["import_workflow_evidence"]["publication_to_open_ready_clock"] = json!({
        "status": "open_ready_deadline_failed_before_transfer",
    });
    assert!(import_preprocessing_evidence(Some(&unavailable_transfer)).is_err());

    let mut unexpected_verifier = report.clone();
    unexpected_verifier["import_workflow_evidence"]["publication_to_open_ready_clock"]["source_verification_successes"] =
        json!(1);
    assert!(import_preprocessing_evidence(Some(&unexpected_verifier)).is_err());

    let mut hidden_object_work = report.clone();
    hidden_object_work["import_workflow_evidence"]["publication_to_open_ready_clock"]["publication_currentness_execution"]
        ["observed_snapshot_object_reads"] = json!(14);
    hidden_object_work["import_workflow_evidence"]["publication_to_open_ready_clock"]["publication_currentness_execution"]
        ["observed_total_object_reads"] = json!(36);
    assert!(import_preprocessing_evidence(Some(&hidden_object_work)).is_err());

    let mut hidden_decode_work = report.clone();
    hidden_decode_work["import_workflow_evidence"]["publication_to_open_ready_clock"]["publication_currentness_execution"]
        ["observed_codec_decode_calls"] = json!(1);
    assert!(import_preprocessing_evidence(Some(&hidden_decode_work)).is_err());

    let mut missing_commit = report;
    missing_commit["import_workflow_evidence"]["worker_emitted_stage_names"]
        .as_array_mut()
        .unwrap()
        .retain(|stage| stage != "commit");
    assert!(import_preprocessing_evidence(Some(&missing_commit)).is_err());
}

#[test]
fn b3_source_closure_evidence_compares_exact_entries_and_bytes() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("empty")).unwrap();
    fs::write(temp.path().join("payload"), b"before").unwrap();
    let before = SourceClosureSnapshot::capture(temp.path()).unwrap();
    assert_eq!(
        before.compare_json(temp.path()).unwrap()["byte_identical"],
        true
    );

    fs::write(temp.path().join("payload"), b"after!").unwrap();
    assert_eq!(
        before.compare_json(temp.path()).unwrap()["byte_identical"],
        false
    );
}

#[test]
fn b4_scripts_lock_the_fixed_three_launch_cutover() {
    let package = Path::new("/tmp/source.m4d");
    let original = Path::new("/tmp/original.m4dproj");
    let save_as = Path::new("/tmp/save-as.m4dproj");
    let checkpoint = Path::new("/tmp/checkpoint.json");
    let first = b4_launch_one_script(package, original, checkpoint);
    let second = b4_launch_two_script(package, original, save_as);
    let third = b4_launch_three_script(package, save_as);
    for script in [&first, &second, &third] {
        validate_product_automation_script(script).unwrap();
        assert!(
            script["commands"]
                .as_array()
                .unwrap()
                .iter()
                .all(|command| {
                    command.get("command").and_then(Value::as_str) != Some("sleep_frames")
                })
        );
    }

    let first_commands = first["commands"].as_array().unwrap();
    let initial_save = first_commands
        .iter()
        .position(|command| command["command"] == "initial_save_with_edit")
        .unwrap();
    let autosave = first_commands
        .iter()
        .position(|command| {
            command["command"] == "wait_for" && command["condition"] == "project_autosaved"
        })
        .unwrap();
    let checkpoint_command = first_commands
        .iter()
        .position(|command| command["command"] == "write_external_kill_checkpoint")
        .unwrap();
    assert!(initial_save < autosave && autosave < checkpoint_command);
    assert_eq!(first_commands[autosave]["timeout_ms"], 45_000);
    assert_eq!(
        first_commands.last().unwrap()["command"],
        "hold_for_external_kill"
    );
    assert_eq!(
        first_commands[checkpoint_command]["stage"],
        B4_CHECKPOINT_STAGE
    );
    assert!(first_commands.iter().any(|command| {
        command["condition"]["project_state"]["dirty"] == true
            && command["condition"]["project_state"]["manual"] == true
            && command["condition"]["project_state"]["autosave"] == true
    }));

    let second_commands = second["commands"].as_array().unwrap();
    assert!(second_commands.iter().any(|command| {
        command["command"] == "wait_for" && command["condition"] == "recovery_review_required"
    }));
    assert!(second_commands.iter().any(|command| {
        command["condition"]["project_state"]["lifecycle"] == "recovery_selected"
            && command["condition"]["project_state"]["dirty"] == true
            && command["condition"]["project_state"]["can_save"] == false
            && command["condition"]["project_state"]["can_save_as"] == true
    }));
    assert!(second_commands.iter().any(|command| {
        command["command"] == "save_project_as" && command["path"].as_str() == save_as.to_str()
    }));
    assert_eq!(second_commands.last().unwrap()["command"], "quit");

    let third_commands = third["commands"].as_array().unwrap();
    assert!(third_commands.iter().any(|command| {
        command["condition"]["project_state"]["lifecycle"] == "established"
            && command["condition"]["project_state"]["dirty"] == false
    }));
    for commands in [second_commands, third_commands] {
        let close = commands
            .iter()
            .position(|command| command["command"] == "close_project_store")
            .unwrap();
        let joined = commands
            .iter()
            .position(|command| {
                command["command"] == "wait_for" && command["condition"] == "project_store_closed"
            })
            .unwrap();
        assert!(close < joined);
    }
}

#[test]
fn pre_alpha_reliability_scripts_lock_recovery_exposure_and_native_close() {
    let package = Path::new("/tmp/source.m4d");
    let provisional_checkpoint = Path::new("/tmp/provisional-checkpoint.json");
    let native_close_checkpoint = Path::new("/tmp/native-close-checkpoint.json");
    let first = pre_alpha_provisional_launch_script(package, provisional_checkpoint);
    let second = pre_alpha_recovery_launch_script(package);
    let third = pre_alpha_native_close_launch_script(package, native_close_checkpoint);
    for script in [&first, &second, &third] {
        validate_product_automation_script(script).unwrap();
        let commands = script["commands"].as_array().unwrap();
        assert!(commands.iter().any(|command| {
            command["command"] == "set_mapped_client_pixels"
                && command["width"] == B4_PRIMARY_CLIENT_WIDTH
                && command["height"] == B4_PRIMARY_CLIENT_HEIGHT
        }));
    }

    let first_commands = first["commands"].as_array().unwrap();
    let autosave = first_commands
        .iter()
        .position(|command| {
            command["command"] == "wait_for" && command["condition"] == "project_autosaved"
        })
        .unwrap();
    let checkpoint = first_commands
        .iter()
        .position(|command| command["command"] == "write_external_kill_checkpoint")
        .unwrap();
    assert!(autosave < checkpoint);
    assert_eq!(first_commands[autosave]["timeout_ms"], 45_000);
    assert_eq!(
        first_commands[checkpoint]["stage"],
        PRE_ALPHA_PROVISIONAL_CHECKPOINT_STAGE
    );
    assert_eq!(
        first_commands.last().unwrap()["command"],
        "hold_for_external_kill"
    );

    let second_commands = second["commands"].as_array().unwrap();
    let exposed = second_commands
        .iter()
        .position(|command| {
            command["command"] == "wait_for"
                && command["condition"] == "unsaved_autosave_recovery_exposed"
        })
        .unwrap();
    let recovered = second_commands
        .iter()
        .position(|command| command["command"] == "recover_exposed_unsaved_autosave")
        .unwrap();
    assert!(exposed < recovered);
    validate_pre_alpha_recovery_script(&second).unwrap();
    assert_eq!(second_commands.last().unwrap()["command"], "quit");

    let third_commands = third["commands"].as_array().unwrap();
    let checkpoint = third_commands
        .iter()
        .find(|command| command["command"] == "write_external_kill_checkpoint")
        .unwrap();
    assert_eq!(checkpoint["stage"], PRE_ALPHA_NATIVE_CLOSE_CHECKPOINT_STAGE);
    assert_eq!(
        third_commands.last().unwrap()["command"],
        "hold_for_external_kill"
    );
}

#[test]
fn pre_alpha_recovery_and_stderr_evidence_are_bounded_and_strict() {
    let state_home = tempfile::tempdir().unwrap();
    let recovery_root = state_home.path().join("mirante4d").join("recovery");
    fs::create_dir_all(recovery_root.join("12345678-1234-1234-1234-123456789abc.m4dproj")).unwrap();
    let evidence = pre_alpha_recovery_store_evidence(state_home.path()).unwrap();
    assert_eq!(evidence["entries"], 1);
    assert_eq!(evidence["canonical_store_directories"], 1);
    assert!(is_canonical_project_store_directory_name(
        "12345678-1234-1234-1234-123456789abc.m4dproj"
    ));
    assert!(!is_canonical_project_store_directory_name(
        "12345678-1234-1234-1234-123456789ABC.m4dproj"
    ));

    let stderr = state_home.path().join("stderr.log");
    fs::write(&stderr, "normal diagnostic\n").unwrap();
    assert_eq!(
        pre_alpha_stderr_evidence(&stderr).unwrap()["panic_free"],
        true
    );
    fs::write(&stderr, "thread 'main' panicked at source.rs:1\n").unwrap();
    assert_eq!(
        pre_alpha_stderr_evidence(&stderr).unwrap()["panic_free"],
        false
    );
}

#[test]
fn pre_alpha_checkpoints_require_provisional_autosave_and_clean_unbound_close() {
    let provisional = json!({
        "schema": B4_CHECKPOINT_SCHEMA,
        "schema_version": 1,
        "stage": PRE_ALPHA_PROVISIONAL_CHECKPOINT_STAGE,
        "viewport_evidence": {
            "requested_mapped_client_pixels": {
                "width": B4_PRIMARY_CLIENT_WIDTH,
                "height": B4_PRIMARY_CLIENT_HEIGHT,
            }
        },
        "project_state": {
            "bound": true,
            "dirty": true,
            "current_revision": {"project_id": "project", "sequence": 2},
            "saved_revision": null,
            "lifecycle": "provisional",
            "can_save": true,
            "can_save_as": false,
            "manual": false,
            "autosave": true,
            "current_manual": null,
            "current_autosave": "autosave-generation",
        },
        "project_evidence": {
            "latest_autosave_captured_revision": {
                "project_id": "project",
                "sequence": 2,
            }
        }
    });
    validate_pre_alpha_checkpoint(&provisional, PRE_ALPHA_PROVISIONAL_CHECKPOINT_STAGE, 1).unwrap();

    let clean = json!({
        "schema": B4_CHECKPOINT_SCHEMA,
        "schema_version": 1,
        "stage": PRE_ALPHA_NATIVE_CLOSE_CHECKPOINT_STAGE,
        "viewport_evidence": {
            "requested_mapped_client_pixels": {
                "width": B4_PRIMARY_CLIENT_WIDTH,
                "height": B4_PRIMARY_CLIENT_HEIGHT,
            }
        },
        "project_state": {
            "bound": false,
            "dirty": false,
            "current_revision": null,
            "saved_revision": null,
            "lifecycle": "unbound",
            "can_save": true,
            "can_save_as": false,
            "manual": false,
            "autosave": false,
            "current_manual": null,
            "current_autosave": null,
        }
    });
    validate_pre_alpha_checkpoint(&clean, PRE_ALPHA_NATIVE_CLOSE_CHECKPOINT_STAGE, 3).unwrap();

    let mut unsafe_clean = clean;
    unsafe_clean["project_state"]["dirty"] = json!(true);
    assert!(
        validate_pre_alpha_checkpoint(&unsafe_clean, PRE_ALPHA_NATIVE_CLOSE_CHECKPOINT_STAGE, 3)
            .is_err()
    );
}

#[test]
fn b4_checkpoint_requires_real_passive_autosave_and_revision_order() {
    let checkpoint = b4_valid_checkpoint();
    validate_b4_checkpoint(&checkpoint, B4_CHECKPOINT_STAGE).unwrap();

    let mut too_early = checkpoint.clone();
    too_early["project_evidence"]["autosave_elapsed_from_durable_edit_ms"] = json!(29_999);
    assert!(validate_b4_checkpoint(&too_early, B4_CHECKPOINT_STAGE).is_err());

    let mut wrong_capture = checkpoint.clone();
    wrong_capture["project_evidence"]["latest_autosave_captured_revision"]["sequence"] = json!(2);
    assert!(validate_b4_checkpoint(&wrong_capture, B4_CHECKPOINT_STAGE).is_err());

    let mut missing_pixels = checkpoint;
    missing_pixels["viewport_evidence"] = json!({});
    assert!(validate_b4_checkpoint(&missing_pixels, B4_CHECKPOINT_STAGE).is_err());
}

#[test]
fn b4_xwininfo_parser_requires_exact_viewable_client_facts() {
    let output = "\
xwininfo: Window id: 0x123 \"Mirante4D\"\n\
  Width: 1920\n\
  Height: 1080\n\
  Map State: IsViewable\n";
    assert_eq!(
        parse_xwininfo_client_geometry(output),
        Some((1920, 1080, true))
    );
    assert_eq!(
        parse_xwininfo_client_geometry(&output.replace("IsViewable", "IsUnMapped")),
        Some((1920, 1080, false))
    );
    assert!(parse_xwininfo_client_geometry("Width: 1920\n").is_none());
}

#[test]
fn b4_aggregate_requires_signal_nine_normal_joins_and_zero_retries() {
    let tempdir = tempfile::tempdir().unwrap();
    let attempts = vec![
        b4_valid_attempt(1, tempdir.path()),
        b4_valid_attempt(2, tempdir.path()),
        b4_valid_attempt(3, tempdir.path()),
    ];
    validate_b4_aggregate_attempts(&attempts).unwrap();

    let mut wrong_signal = attempts.clone();
    wrong_signal[0]["process"]["signal"] = json!(15);
    assert!(validate_b4_aggregate_attempts(&wrong_signal).is_err());

    let mut missing_join = attempts.clone();
    missing_join[1]["automation_report"]["project_store_evidence"]["actor_join"]["status"] =
        json!("failed");
    assert!(validate_b4_aggregate_attempts(&missing_join).is_err());

    let mut retried = attempts;
    retried[2]["retry_index"] = json!(1);
    assert!(validate_b4_aggregate_attempts(&retried).is_err());
}

#[test]
fn b4_trusted_report_must_match_revision_and_durability_facts() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("trusted.json");
    let commit = "a".repeat(40);
    let tree = "b".repeat(40);
    let identity = json!({
        "commit": commit,
        "tree": tree,
        "clean": true,
    });
    let lifecycle = json!({
        "schema": "mirante4d-wp10b-project-store-lifecycle-evidence",
        "schema_version": 1,
        "result": "passed",
        "failures": [],
        "identity": {"commit": commit, "tree": tree, "clean": true},
        "harness": {"retries": 0},
        "counters": {
            "incremental_unchanged_artifact_bytes_rewritten": 0
        }
    });
    let report = json!({
        "schema": "mirante4d-verification-run",
        "schema_version": 1,
        "group": "project-store-lifecycle",
        "native_status": "passed",
        "identity": {
            "commit": commit,
            "tree": tree,
            "clean": true,
            "qualifying": true
        },
        "phases": [
            {"status": "passed"},
            {"status": "passed"},
            {"status": "passed"}
        ],
        "evidence": {"wp10b_project_store_lifecycle": lifecycle}
    });
    write_json_file(&path, &report).unwrap();
    let accepted = load_b4_trusted_project_store_evidence(&path, &identity).unwrap();
    assert_eq!(
        accepted["lifecycle_evidence"]["counters"]["incremental_unchanged_artifact_bytes_rewritten"],
        0
    );

    let mut failed = report;
    failed["evidence"]["wp10b_project_store_lifecycle"]["harness"]["retries"] = json!(1);
    write_json_file(&path, &failed).unwrap();
    assert!(load_b4_trusted_project_store_evidence(&path, &identity).is_err());
}

#[test]
fn product_validation_scenario_resolution_is_strict() {
    assert_eq!(
        ProductValidationScenario::resolve(None, None).unwrap(),
        ProductValidationScenario::GeneratedFixtureCameraSmoke
    );
    for (name, expected) in [
        (
            GENERATED_FIXTURE_SCENARIO,
            ProductValidationScenario::GeneratedFixtureCameraSmoke,
        ),
        (
            GENERATED_RENDER_MODES_SCENARIO,
            ProductValidationScenario::GeneratedFixtureRenderModes,
        ),
        (
            REPRESENTATIVE_NATIVE_NAVIGATION_SCENARIO,
            ProductValidationScenario::RepresentativeNativeNavigation,
        ),
        (
            B3_SOURCE_VERIFICATION_SCENARIO,
            ProductValidationScenario::B3SourceVerification,
        ),
        (
            IMPORT_PREPROCESSING_SCENARIO,
            ProductValidationScenario::ImportPreprocessing,
        ),
        (
            B4_PROJECT_PERSISTENCE_SCENARIO,
            ProductValidationScenario::B4ProjectPersistence,
        ),
        (
            PRE_ALPHA_RELIABILITY_SCENARIO,
            ProductValidationScenario::PreAlphaReliability,
        ),
    ] {
        assert_eq!(
            ProductValidationScenario::resolve(Some(name), None).unwrap(),
            expected
        );
        assert!(ProductValidationScenario::is_named_scenario(name));
    }
    assert_eq!(
        ProductValidationScenario::resolve(None, Some(B3_SOURCE_VERIFICATION_SCENARIO)).unwrap(),
        ProductValidationScenario::B3SourceVerification
    );
    for removed_alias in [
        "target-fixture-camera-smoke",
        "target",
        "target-fixture-render-modes",
        "render-modes",
        "target-source-verification",
        "b4-project-persistence",
    ] {
        assert!(ProductValidationScenario::resolve(Some(removed_alias), None).is_err());
        assert!(!ProductValidationScenario::is_named_scenario(removed_alias));
    }
}

#[test]
fn product_validation_output_dirs_are_scenario_scoped() {
    assert_eq!(
        product_validation_output_dir(&ProductValidationScenario::GeneratedFixtureCameraSmoke),
        Path::new(OUTPUT_DIR).join(GENERATED_FIXTURE_SCENARIO)
    );
    assert_eq!(
        product_validation_output_dir(&ProductValidationScenario::GeneratedFixtureRenderModes),
        Path::new(OUTPUT_DIR).join(GENERATED_RENDER_MODES_SCENARIO)
    );
    assert_eq!(
        product_validation_output_dir(&ProductValidationScenario::RepresentativeNativeNavigation),
        Path::new(OUTPUT_DIR).join(REPRESENTATIVE_NATIVE_NAVIGATION_SCENARIO)
    );
    assert_eq!(
        product_validation_output_dir(&ProductValidationScenario::B3SourceVerification),
        Path::new(OUTPUT_DIR).join(B3_SOURCE_VERIFICATION_SCENARIO)
    );
    assert_eq!(
        product_validation_output_dir(&ProductValidationScenario::ImportPreprocessing),
        Path::new(OUTPUT_DIR).join(IMPORT_PREPROCESSING_SCENARIO)
    );
    assert_eq!(
        product_validation_output_dir(&ProductValidationScenario::B4ProjectPersistence),
        Path::new(OUTPUT_DIR).join(B4_PROJECT_PERSISTENCE_SCENARIO)
    );
    assert_eq!(
        product_validation_output_dir(&ProductValidationScenario::PreAlphaReliability),
        Path::new(OUTPUT_DIR).join(PRE_ALPHA_RELIABILITY_SCENARIO)
    );
}

#[test]
fn fixed_product_automation_script_validation_rejects_wrong_schema() {
    let script = json!({
        "schema": "wrong",
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": "unit",
        "commands": [
            { "command": "open_dataset", "path": "/tmp/demo.m4d" },
            { "command": "quit" }
        ]
    });

    let err = validate_product_automation_script(&script)
        .unwrap_err()
        .to_string();

    assert!(err.contains(PRODUCT_AUTOMATION_SCRIPT_SCHEMA));

    let predecessor_version = json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": 5,
        "scenario": "unit",
        "commands": [
            { "command": "open_dataset", "path": "/tmp/demo.m4d" },
            { "command": "quit" }
        ]
    });
    assert!(
        validate_product_automation_script(&predecessor_version)
            .unwrap_err()
            .to_string()
            .contains("schema_version must be 8")
    );
}

#[test]
fn product_automation_v8_rejects_implicit_capture_and_nonblank_targets() {
    let implicit_capture = json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": "unit",
        "hard_safety_limits": {},
        "commands": [
            { "command": "open_dataset", "path": "/tmp/demo.m4d" },
            { "command": "capture_screenshot", "name": "implicit" },
            { "command": "quit" }
        ]
    });
    assert!(
        validate_product_automation_script(&implicit_capture)
            .unwrap_err()
            .to_string()
            .contains("explicit target")
    );

    let removed_nonblank = json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": "unit",
        "hard_safety_limits": {},
        "commands": [
            { "command": "open_dataset", "path": "/tmp/demo.m4d" },
            { "command": "assert", "condition": "nonblank_frame" },
            { "command": "quit" }
        ]
    });
    assert!(
        validate_product_automation_script(&removed_nonblank)
            .unwrap_err()
            .to_string()
            .contains("was removed")
    );
}

#[test]
fn fixed_product_automation_script_validation_requires_open_dataset_and_quit() {
    let missing_open = json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": "unit",
        "commands": [
            { "command": "quit" }
        ]
    });
    let missing_quit = json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": "unit",
        "commands": [
            { "command": "open_dataset", "path": "/tmp/demo.m4d" }
        ]
    });

    assert!(
        validate_product_automation_script(&missing_open)
            .unwrap_err()
            .to_string()
            .contains("open_dataset")
    );
    assert!(
        validate_product_automation_script(&missing_quit)
            .unwrap_err()
            .to_string()
            .contains("quit")
    );
}

#[test]
fn fixed_product_automation_script_validation_rejects_bad_hard_safety_limits() {
    let missing = json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": "unit",
        "commands": [
            { "command": "open_dataset", "path": "/tmp/demo.m4d" },
            { "command": "quit" }
        ]
    });
    let unknown = json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": "unit",
        "hard_safety_limits": {
            "max_surprise_bytes": 1
        },
        "commands": [
            { "command": "open_dataset", "path": "/tmp/demo.m4d" },
            { "command": "quit" }
        ]
    });
    let non_integer = json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": "unit",
        "hard_safety_limits": {
            "max_cpu_total_bytes": "lots"
        },
        "commands": [
            { "command": "open_dataset", "path": "/tmp/demo.m4d" },
            { "command": "quit" }
        ]
    });

    assert!(
        validate_product_automation_script(&missing)
            .unwrap_err()
            .to_string()
            .contains("hard_safety_limits must be present")
    );
    assert!(
        validate_product_automation_script(&unknown)
            .unwrap_err()
            .to_string()
            .contains("unknown automation script hard-safety limit")
    );
    assert!(
        validate_product_automation_script(&non_integer)
            .unwrap_err()
            .to_string()
            .contains("must be null or an unsigned integer")
    );
}

#[test]
fn product_automation_script_shape_rejects_legacy_and_unknown_top_level_fields() {
    let valid = json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": "unit",
        "hard_safety_limits": {
            "max_cpu_total_bytes": 1024,
            "max_cpu_prefetch_bytes": null
        },
        "commands": [
            { "command": "open_dataset", "path": "/tmp/demo.m4d" },
            { "command": "quit" }
        ]
    });
    validate_product_automation_script(&valid).unwrap();
    let canonical =
        canonical_product_automation_hard_safety_limits(&valid["hard_safety_limits"]).unwrap();
    assert_eq!(
        canonical.as_object().unwrap().len(),
        PRODUCT_AUTOMATION_HARD_SAFETY_LIMIT_FIELDS.len()
    );
    assert_eq!(canonical["max_cpu_total_bytes"], 1024);
    assert_eq!(canonical["max_cpu_prefetch_bytes"], Value::Null);
    assert_eq!(canonical["max_runtime_resident_resources"], Value::Null);

    let mut legacy = valid.clone();
    legacy["limits"] = json!({});
    assert!(validate_product_automation_script(&legacy).is_err());

    let mut unknown = valid;
    unknown["unexpected"] = json!(true);
    assert!(validate_product_automation_script(&unknown).is_err());
}

#[test]
fn product_automation_report_contract_requires_v8_script_and_report_binding() {
    let report = json!({
        "status": "passed",
        "artifacts": []
    });
    let (tempdir, script, script_path, report) = bound_automation_report(report);
    validate_product_automation_report_contract(&report, &script, &script_path).unwrap();

    let mut old_schema = report.clone();
    old_schema["schema_version"] = json!(REPORT_SCHEMA_VERSION - 1);
    assert!(
        validate_product_automation_report_contract(&old_schema, &script, &script_path).is_err()
    );

    let mut failed_status = report.clone();
    failed_status["status"] = json!("failed");
    failed_status["failure_reason"] = json!("hard safety cap exceeded");
    assert!(
        validate_product_automation_report_contract(&failed_status, &script, &script_path).is_err()
    );

    let mut missing_failure = report.clone();
    missing_failure
        .as_object_mut()
        .unwrap()
        .remove("failure_reason");
    assert!(
        validate_product_automation_report_contract(&missing_failure, &script, &script_path)
            .is_err()
    );

    let mut wrong_script = report.clone();
    wrong_script["script"]["scenario"] = json!("other");
    assert!(
        validate_product_automation_report_contract(&wrong_script, &script, &script_path).is_err()
    );

    let other_script_path = tempdir.path().join("other-script.json");
    write_json_file(&other_script_path, &script).unwrap();
    let mut wrong_path = report.clone();
    wrong_path["script"]["path"] = json!(other_script_path);
    assert!(
        validate_product_automation_report_contract(&wrong_path, &script, &script_path).is_err()
    );

    let mut incomplete_limits = report.clone();
    incomplete_limits["hard_safety_limits"]
        .as_object_mut()
        .unwrap()
        .remove("max_cpu_prefetch_bytes");
    assert!(
        validate_product_automation_report_contract(&incomplete_limits, &script, &script_path)
            .is_err()
    );

    let mut legacy_limits = report;
    legacy_limits["limits"] = json!({});
    assert!(
        validate_product_automation_report_contract(&legacy_limits, &script, &script_path).is_err()
    );
    let (status, _) =
        completed_product_validation_outcome(true, Some(&legacy_limits), &script, &script_path);
    assert_eq!(status, ProductValidationStatus::Failed);
}

#[test]
fn display_status_names_are_report_stable() {
    assert_eq!(DisplayClass::RealDisplay.name(), "real_display");
    assert_eq!(DisplayClass::VirtualDisplay.name(), "virtual_display");
    assert_eq!(DisplayClass::Unsupported.name(), "unsupported");
}

#[test]
fn display_classification_distinguishes_missing_real_and_virtual_displays() {
    assert_eq!(
        classify_display(false, false, None, false),
        DisplayClassification {
            class: DisplayClass::Unsupported,
            source: "no_display_environment",
        }
    );
    assert_eq!(
        classify_display(true, false, Some("virtual_display"), false),
        DisplayClassification {
            class: DisplayClass::VirtualDisplay,
            source: DISPLAY_CLASS_ENV,
        }
    );
    assert_eq!(
        classify_display(true, false, None, true),
        DisplayClassification {
            class: DisplayClass::VirtualDisplay,
            source: "ci_x11_heuristic",
        }
    );
    assert_eq!(
        classify_display(false, true, None, false),
        DisplayClassification {
            class: DisplayClass::RealDisplay,
            source: "display_environment_heuristic",
        }
    );
}

#[test]
fn product_validation_status_labels_and_failures_are_report_stable() {
    assert_eq!(ProductValidationStatus::Passed.name(), "passed");
    assert_eq!(ProductValidationStatus::Unsupported.name(), "unsupported");
    assert_eq!(ProductValidationStatus::Failed.name(), "failed");
    assert_eq!(ProductValidationStatus::TimedOut.name(), "timed_out");
    assert!(!ProductValidationStatus::Passed.is_failure());
    assert!(!ProductValidationStatus::Unsupported.is_failure());
    assert!(ProductValidationStatus::Failed.is_failure());
    assert!(ProductValidationStatus::TimedOut.is_failure());
}

#[test]
fn completed_product_validation_fails_without_viewport_capture() {
    let automation_report = json!({
        "status": "passed",
        "artifacts": []
    });
    let (_tempdir, script, script_path, automation_report) =
        bound_automation_report(automation_report);

    let (status, failure_reason) =
        completed_product_validation_outcome(true, Some(&automation_report), &script, &script_path);

    assert_eq!(status, ProductValidationStatus::Failed);
    assert!(
        failure_reason
            .as_deref()
            .unwrap()
            .contains("missing a nonblank GPU viewport_capture artifact")
    );
}

#[test]
fn completed_product_validation_fails_with_blank_viewport_capture() {
    let automation_report = json!({
        "status": "passed",
        "artifacts": [{
            "kind": "viewport_capture",
            "capture_source": "gpu_display_frame_readback",
            "path": "blank.ppm",
            "width": 2,
            "height": 2,
            "target": "three_d",
            "frame_identity": 1,
            "surface_generation": 1,
            "pixel_stats": {
                "pixel_count": 4,
                "nonzero_rgb_pixels": 0,
                "max_rgb": 0
            }
        }]
    });
    let (_tempdir, script, script_path, automation_report) =
        bound_automation_report(automation_report);

    let (status, failure_reason) =
        completed_product_validation_outcome(true, Some(&automation_report), &script, &script_path);

    assert_eq!(status, ProductValidationStatus::Failed);
    assert!(failure_reason.is_some());
}

#[test]
fn completed_product_validation_passes_with_nonblank_viewport_capture() {
    let automation_report = json!({
        "status": "passed",
        "artifacts": [{
            "kind": "viewport_capture",
            "capture_source": "gpu_display_frame_readback",
            "path": "nonblank.ppm",
            "width": 2,
            "height": 2,
            "target": "three_d",
            "frame_identity": 1,
            "surface_generation": 1,
            "pixel_stats": {
                "pixel_count": 4,
                "nonzero_rgb_pixels": 1,
                "max_rgb": 255
            }
        }]
    });
    let (_tempdir, script, script_path, automation_report) =
        bound_automation_report(automation_report);

    let (status, failure_reason) =
        completed_product_validation_outcome(true, Some(&automation_report), &script, &script_path);

    assert_eq!(status, ProductValidationStatus::Passed);
    assert_eq!(failure_reason, None);
}

#[test]
fn completed_product_validation_rejects_nonblank_loading_reference_capture() {
    let automation_report = json!({
        "status": "passed",
        "artifacts": [{
            "kind": "viewport_capture",
            "capture_source": "loading_reference_color_image",
            "path": "loading.ppm",
            "width": 2,
            "height": 2,
            "target": "three_d",
            "frame_identity": 1,
            "surface_generation": 1,
            "pixel_stats": {
                "pixel_count": 4,
                "nonzero_rgb_pixels": 1,
                "max_rgb": 255
            }
        }]
    });
    let (_tempdir, script, script_path, automation_report) =
        bound_automation_report(automation_report);

    let (status, failure_reason) =
        completed_product_validation_outcome(true, Some(&automation_report), &script, &script_path);

    assert_eq!(status, ProductValidationStatus::Failed);
    assert!(failure_reason.unwrap().contains("GPU viewport_capture"));
}

#[test]
fn b3_e1_acceptance_requires_two_distinct_exact_gpu_render_targets() {
    let automation_report = b3_exact_capture_report(u64::from(B3_SECOND_VIEWPORT_WIDTH));

    let evidence = b3_exact_e1_capture_evidence(Some(&automation_report)).unwrap();

    assert_eq!(evidence["accepted"], true);
    assert_eq!(evidence["evidence_level"], "E1");
    assert_eq!(evidence["e4_product_open_satisfied"], false);
    assert_eq!(evidence["captures"].as_array().unwrap().len(), 2);
    assert_eq!(evidence["captures"][0]["width"], B3_VIEWPORT_WIDTH);
    assert_eq!(evidence["captures"][1]["width"], B3_SECOND_VIEWPORT_WIDTH);
}

#[test]
fn b3_e1_acceptance_rejects_a_mislabeled_second_render_target() {
    let automation_report = b3_exact_capture_report(1280);

    let error = b3_exact_e1_capture_evidence(Some(&automation_report)).unwrap_err();

    assert!(error.contains("expected exact 1920x1080 render-target pixels"));
}

#[test]
fn unix_epoch_ms_to_utc_rfc3339_formats_report_timestamps() {
    assert_eq!(unix_epoch_ms_to_utc_rfc3339(0), "1970-01-01T00:00:00.000Z");
    assert_eq!(
        unix_epoch_ms_to_utc_rfc3339(1_782_316_800_123),
        "2026-06-24T16:00:00.123Z"
    );
}

#[test]
fn wrapper_report_includes_dataset_context_and_automation_artifacts() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = extract_target_u16_fixture(tempdir.path()).unwrap();
    let script = target_fixture_camera_smoke_script(&package);
    let automation_report = json!({
        "status": "passed",
        "viewport_evidence": {
            "requested_window_inner_size_points": {"width": 960, "height": 720},
            "pixels_per_point": 1.5,
            "observed_client_area_pixels": null,
            "render_target_pixels": {"width": 16, "height": 16}
        },
        "artifacts": [
            {
                "kind": "viewport_capture",
                "format": "ppm",
                "path": "target/mirante4d/product-validation/artifacts/post-camera-sequence.ppm",
                "width": 16,
                "height": 16,
                "target": "three_d",
                "frame_identity": 1,
                "surface_generation": 1,
                "capture_source": "gpu_display_frame_readback",
                "pixel_stats": {
                    "pixel_count": 256,
                    "nonzero_rgb_pixels": 32,
                    "min_rgb": 0,
                    "max_rgb": 255,
                    "mean_rgb": 12.0
                }
            }
        ],
        "diagnostics": [
            {
                "dataset_runtime": {
                    "capacity": {
                        "total_cpu_bytes": 134217728,
                        "worker_limit": 8,
                        "request_queue_limit": 1024,
                        "completion_queue_limit": 1024
                    },
                    "used": { "total_cpu_bytes": 4096 },
                    "work": {
                        "queued_requests": 0,
                        "in_flight_decodes": 0,
                        "pending_completions": 0,
                        "resident_resources": 3
                    }
                },
                "lease_bridge": {
                    "required": 3,
                    "retained": 3,
                    "missing": 0,
                    "complete": true
                }
            }
        ],
        "events": [
            {
                "command": "assert",
                "details": {
                    "condition": "cross_section_panel_schedule",
                    "cross_section_snapshot": {
                        "schema": "mirante4d-cross-section-panel-diagnostics",
                        "schema_version": 1,
                        "layout": "FourPanel",
                        "demand_scopes": {"xy": 1, "xz": 1, "yz": 1},
                        "active_lease_cohort": {
                            "required": 3,
                            "retained": 3,
                            "missing": 0,
                            "complete": true
                        },
                        "panels": []
                    }
                }
            },
            {
                "command": "assert",
                "details": {
                    "condition": "cross_section_retired",
                    "cross_section_snapshot": {
                        "schema": "mirante4d-cross-section-panel-diagnostics",
                        "schema_version": 1,
                        "layout": "Single3d",
                        "demand_scopes": {"xy": 0, "xz": 0, "yz": 0},
                        "active_lease_cohort": {
                            "required": 3,
                            "retained": 3,
                            "missing": 0,
                            "complete": true
                        },
                        "panels": []
                    }
                }
            }
        ],
        "final_diagnostics": {
            "dataset_runtime": {
                "capacity": {
                    "total_cpu_bytes": 134217728,
                    "worker_limit": 8,
                    "request_queue_limit": 1024,
                    "completion_queue_limit": 1024
                },
                "used": { "total_cpu_bytes": 4096 },
                "work": {
                    "queued_requests": 0,
                    "in_flight_decodes": 0,
                    "pending_completions": 0,
                    "resident_resources": 3
                }
            },
            "lease_bridge": {
                "required": 3,
                "retained": 3,
                "missing": 0,
                "complete": true
            },
            "cross_section": {
                "schema": "mirante4d-cross-section-panel-diagnostics",
                "schema_version": 1,
                "layout": "Single3d",
                "demand_scopes": {"xy": 0, "xz": 0, "yz": 0},
                "active_lease_cohort": {
                    "required": 3,
                    "retained": 3,
                    "missing": 0,
                    "complete": true
                },
                "panels": []
            },
            "gpu_adapter": {
                "name": "unit adapter",
                "backend": "Vulkan"
            }
        }
    });
    let wrapper_path = tempdir.path().join("product-validation-report.json");
    let script_path = tempdir.path().join("product-automation-script.json");
    let automation_report_path = tempdir.path().join("product-automation-report.json");
    let stdout_path = tempdir.path().join("stdout.log");
    let stderr_path = tempdir.path().join("stderr.log");

    let report = wrapper_report_json(WrapperReport {
        path: &wrapper_path,
        scenario_name: GENERATED_FIXTURE_SCENARIO,
        status: ProductValidationStatus::Passed,
        failure_reason: None,
        started_at_epoch_ms: 0,
        duration_ms: 1.0,
        timeout_secs: 60,
        package: &package,
        binary: Path::new("/tmp/packaged-mirante4d-app"),
        script: &script_path,
        script_value: &script,
        automation_report: &automation_report_path,
        automation_report_value: Some(&automation_report),
        stdout: &stdout_path,
        stderr: &stderr_path,
        display: DisplayClassification {
            class: DisplayClass::RealDisplay,
            source: "unit",
        },
        preflight_only: false,
        source_closure_evidence: Value::Null,
        automation_status: Some("passed".to_owned()),
        exit_status: Some("0".to_owned()),
        exit_success: Some(true),
    });

    assert_eq!(report["dataset"]["manifest_status"], "loaded");
    assert_eq!(report["binary"], "/tmp/packaged-mirante4d-app");
    assert!(
        report["dataset"]["package_id"]
            .as_str()
            .unwrap()
            .starts_with("m4d-package-v1-sha256:")
    );
    assert_eq!(
        report["artifacts"]["automation_artifacts"][0]["kind"],
        "viewport_capture"
    );
    assert_eq!(
        report["artifacts"]["automation_artifacts"][0]["pixel_stats"]["nonzero_rgb_pixels"],
        32
    );
    assert_eq!(
        report["logs"]["stdout"],
        stdout_path.to_string_lossy().as_ref()
    );
    assert_eq!(
        report["logs"]["stderr"],
        stderr_path.to_string_lossy().as_ref()
    );
    assert_eq!(
        report["metrics"]["dataset_runtime"]["kind"],
        "dataset_runtime_metrics"
    );
    assert_eq!(report["metrics"]["dataset_runtime"]["snapshot_count"], 1);
    assert_eq!(
        report["metrics"]["dataset_runtime"]["final"]["capacity"]["worker_limit"],
        8
    );
    assert_eq!(
        report["metrics"]["lease_bridge"]["kind"],
        "lease_bridge_metrics"
    );
    assert_eq!(report["metrics"]["lease_bridge"]["final"]["missing"], 0);
    assert_eq!(
        report["metrics"]["cross_section_panels"]["kind"],
        "cross_section_panel_metrics"
    );
    assert_eq!(
        report["metrics"]["cross_section_panels"]["snapshot_count"],
        2
    );
    assert_eq!(
        report["metrics"]["cross_section_panels"]["final"]["layout"],
        "Single3d"
    );
    assert_eq!(
        report["metrics"]["cross_section_panels"]["latest_assertion"]["layout"],
        "Single3d"
    );
    assert_eq!(report["gpu_adapter"]["name"], "unit adapter");
    assert_eq!(report["environment"]["display"], "real_display");
    assert_eq!(report["environment"]["display_class"], "real_display");
    assert_eq!(report["environment"]["display_class_source"], "unit");
    assert_eq!(
        report["environment"]["product_validate_preflight_only"],
        false
    );
    assert_eq!(report["evidence_level"], "E1");
    assert_eq!(
        report["claim_boundary"]["evidence_type"],
        "internal_native_window_product_automation"
    );
    assert_eq!(
        report["claim_boundary"]["closure_authority"],
        "integration_support_only_not_black_box_product_open"
    );
    assert_eq!(report["claim_boundary"]["e4_product_open_satisfied"], false);
    assert_eq!(
        report["scenario"]["requested_window_inner_size_points"]["width"],
        GENERATED_VIEWPORT_WIDTH
    );
    assert_eq!(report["scenario"]["pixels_per_point"], 1.5);
    assert_eq!(report["scenario"]["render_target_pixels"]["width"], 16);
    assert!(report["scenario"].get("viewport").is_none());
    assert!(report["limits"].get("viewport").is_none());
    for removed in [
        "command_count",
        "observed_client_area_pixels",
        "frame_wait_count",
        "millis_wait_count",
        "wait_timeout_ms_total",
        "automation_limits",
    ] {
        assert!(report["scenario"].get(removed).is_none());
        assert!(report["limits"].get(removed).is_none());
    }
    assert!(report["limits"].get("render_modes").is_none());
    assert_eq!(report["scenario"]["name"], GENERATED_FIXTURE_SCENARIO);
    assert_eq!(
        report["scenario"]["automation_script_scenario"],
        GENERATED_FIXTURE_SCENARIO
    );
    assert_eq!(report["scenario"]["render_modes"], json!(["mip"]));
    assert_eq!(report["limits"]["cpu_byte_limit_enforced"], true);
    assert_eq!(report["limits"]["runtime_work_limit_enforced"], true);
    assert_eq!(
        report["limits"]["cpu_total_byte_limit_bytes"],
        script["hard_safety_limits"]["max_cpu_total_bytes"]
    );
    assert_eq!(
        report["limits"]["cpu_category_byte_limits"]["decoded_residency"],
        script["hard_safety_limits"]["max_cpu_decoded_residency_bytes"]
    );
    assert_eq!(
        report["limits"]["runtime_work_limits"]["queued_requests"],
        script["hard_safety_limits"]["max_runtime_queued_requests"]
    );
}

#[test]
fn wrapper_report_marks_preflight_as_non_launch_unsupported_evidence() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = extract_target_u16_fixture(tempdir.path()).unwrap();
    let script = target_fixture_camera_smoke_script(&package);
    let wrapper_path = tempdir.path().join("product-validation-report.json");
    let script_path = tempdir.path().join("product-automation-script.json");
    let automation_report_path = tempdir.path().join("product-automation-report.json");
    let stdout_path = tempdir.path().join("stdout.log");
    let stderr_path = tempdir.path().join("stderr.log");

    let report = wrapper_report_json(WrapperReport {
        path: &wrapper_path,
        scenario_name: GENERATED_FIXTURE_SCENARIO,
        status: ProductValidationStatus::Unsupported,
        failure_reason: Some("product validation preflight requested".to_owned()),
        started_at_epoch_ms: 0,
        duration_ms: 1.0,
        timeout_secs: 60,
        package: &package,
        binary: Path::new("/tmp/packaged-mirante4d-app"),
        script: &script_path,
        script_value: &script,
        automation_report: &automation_report_path,
        automation_report_value: None,
        stdout: &stdout_path,
        stderr: &stderr_path,
        display: DisplayClassification {
            class: DisplayClass::Unsupported,
            source: PREFLIGHT_ONLY_DISPLAY_SOURCE,
        },
        preflight_only: true,
        source_closure_evidence: Value::Null,
        automation_status: None,
        exit_status: None,
        exit_success: None,
    });

    assert_eq!(report["status"], "unsupported");
    assert_eq!(
        report["failure_reason"],
        "product validation preflight requested"
    );
    assert_eq!(report["dataset"]["manifest_status"], "loaded");
    assert_eq!(report["scenario"]["name"], GENERATED_FIXTURE_SCENARIO);
    assert_eq!(
        report["scenario"]["automation_script"],
        script_path.to_string_lossy().as_ref()
    );
    assert_eq!(report["environment"]["display_class"], "unsupported");
    assert_eq!(
        report["environment"]["display_class_source"],
        PREFLIGHT_ONLY_DISPLAY_SOURCE
    );
    assert_eq!(
        report["environment"]["product_validate_preflight_only"],
        true
    );
    assert!(report["process"]["exit_success"].is_null());
    assert!(report["process"]["exit_status"].is_null());
}

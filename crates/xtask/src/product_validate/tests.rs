use super::*;
use mirante4d_format::{FixtureKind, write_fixture};

#[test]
fn generated_fixture_product_automation_script_uses_semantic_commands() {
    let script = generated_fixture_camera_smoke_script(Path::new("/tmp/demo.m4d"));
    let commands = script["commands"].as_array().unwrap();

    assert_eq!(script["schema"], PRODUCT_AUTOMATION_SCRIPT_SCHEMA);
    assert_eq!(script["scenario"], GENERATED_FIXTURE_SCENARIO);
    assert_eq!(script["limits"]["max_decoded_bytes"], 8 * MIB);
    assert_eq!(
        script["limits"]["max_gpu_brick_atlas_uploaded_bytes"],
        64 * MIB
    );
    assert_eq!(script["limits"]["max_gpu_brick_atlas_resident_bytes"], GIB);
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
    assert!(
        commands
            .iter()
            .any(|command| command["command"] == "capture_screenshot")
    );
    assert_eq!(commands.last().unwrap()["command"], "quit");
}

#[test]
fn generated_fixture_render_modes_script_switches_supported_modes() {
    let script = generated_fixture_render_modes_script(Path::new("/tmp/demo.m4d"));
    let commands = script["commands"].as_array().unwrap();
    let command_names: Vec<_> = commands
        .iter()
        .filter_map(|command| command["command"].as_str())
        .collect();

    assert_eq!(script["schema"], PRODUCT_AUTOMATION_SCRIPT_SCHEMA);
    assert_eq!(script["scenario"], GENERATED_RENDER_MODES_SCENARIO);
    assert_eq!(script["limits"]["max_decoded_bytes"], 12 * MIB);
    assert_eq!(
        script["limits"]["max_gpu_brick_atlas_uploaded_bytes"],
        96 * MIB
    );
    assert_eq!(script["limits"]["max_gpu_brick_atlas_resident_bytes"], GIB);
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
        3
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
        command["command"] == "set_layer_opacity"
            && command["layer_index"].as_u64() == Some(1)
            && command["opacity"].as_f64() == Some(1.0)
    }));
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "set_layer_render_mode")
            .count(),
        3
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
        4
    );
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "probe_hover")
            .count(),
        3
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
            .any(|command| command["name"] == "generated-iso")
    );
    assert!(
        commands
            .iter()
            .any(|command| { command["condition"]["render_mode"]["mode"].as_str() == Some("iso") })
    );
    assert_eq!(commands.last().unwrap()["command"], "quit");
}

#[test]
fn t5_qual_001_interaction_mip_script_records_bounded_mip_camera_sequence() {
    let script = t5_qual_001_interaction_mip_script(Path::new("/tmp/T5-QUAL-001.m4d"));
    let commands = script["commands"].as_array().unwrap();
    let command_names: Vec<_> = commands
        .iter()
        .filter_map(|command| command["command"].as_str())
        .collect();

    assert_eq!(script["scenario"], T5_QUAL_001_INTERACTION_MIP_SCENARIO);
    assert_eq!(script["limits"]["max_decoded_bytes"], GIB);
    assert_eq!(script["limits"]["max_gpu_brick_atlas_uploaded_bytes"], GIB);
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["width"],
        T5_QUAL_001_VIEWPORT_WIDTH
    );
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["height"],
        T5_QUAL_001_VIEWPORT_HEIGHT
    );
    assert_eq!(script_render_modes_json(&script), json!(["mip"]));
    assert_eq!(script_frame_wait_count(&script), 6);
    assert!(
        commands
            .iter()
            .any(|command| command["name"] == "t5-qual-001-first-orbit-cache-miss")
    );
    assert!(
        commands
            .iter()
            .any(|command| command["name"] == "t5-qual-001-return-orbit-cache-reuse")
    );
    assert!(
        command_names
            .iter()
            .filter(|&&name| name == "copy_diagnostics")
            .count()
            >= 4
    );
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "probe_hover")
            .count(),
        2
    );
    assert!(commands.iter().all(|command| command["mode"] != "dvr"));
    assert!(commands.iter().all(|command| command["mode"] != "iso"));
    assert_eq!(commands.last().unwrap()["command"], "quit");
}

#[test]
fn t5_qual_001_interaction_render_modes_script_records_bounded_mode_sequence() {
    let script = t5_qual_001_interaction_render_modes_script(Path::new("/tmp/T5-QUAL-001.m4d"));
    let commands = script["commands"].as_array().unwrap();
    let command_names: Vec<_> = commands
        .iter()
        .filter_map(|command| command["command"].as_str())
        .collect();

    assert_eq!(
        script["scenario"],
        T5_QUAL_001_INTERACTION_RENDER_MODES_SCENARIO
    );
    assert_eq!(script["limits"]["max_decoded_bytes"], 2 * GIB);
    assert_eq!(
        script["limits"]["max_gpu_brick_atlas_uploaded_bytes"],
        2 * GIB
    );
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["width"],
        T5_QUAL_001_VIEWPORT_WIDTH
    );
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["height"],
        T5_QUAL_001_VIEWPORT_HEIGHT
    );
    assert_eq!(
        script_render_modes_json(&script),
        json!(["mip", "dvr", "iso"])
    );
    let mode_sequence: Vec<_> = commands
        .iter()
        .filter(|command| command["command"] == "set_render_mode")
        .filter_map(|command| command["mode"].as_str())
        .collect();
    assert_eq!(mode_sequence, ["mip", "dvr", "iso", "mip"]);
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "set_render_mode")
            .count(),
        4
    );
    assert!(commands.iter().any(|command| {
        command["command"] == "set_dvr_density_scale"
            && command["density_scale"].as_f64() == Some(8.0)
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "set_iso_display_level"
            && command["display_level"].as_f64() == Some(0.02)
    }));
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "probe_hover")
            .count(),
        3
    );
    assert!(
        commands
            .iter()
            .any(|command| command["name"] == "t5-qual-001-render-modes-dvr-orbit")
    );
    assert!(
        commands
            .iter()
            .any(|command| command["name"] == "t5-qual-001-render-modes-iso-pan")
    );
    assert!(
        commands
            .iter()
            .any(|command| { command["condition"]["render_mode"]["mode"].as_str() == Some("dvr") })
    );
    assert!(
        commands
            .iter()
            .any(|command| { command["condition"]["render_mode"]["mode"].as_str() == Some("iso") })
    );
    assert_eq!(commands.last().unwrap()["command"], "quit");
}

#[test]
fn t5_qual_001_four_panel_cross_section_script_records_layout_and_2d_interactions() {
    let script = t5_qual_001_four_panel_cross_section_script(Path::new("/tmp/T5-QUAL-001.m4d"));
    let commands = script["commands"].as_array().unwrap();
    let command_names: Vec<_> = commands
        .iter()
        .filter_map(|command| command["command"].as_str())
        .collect();

    assert_eq!(
        script["scenario"],
        T5_QUAL_001_FOUR_PANEL_CROSS_SECTION_SCENARIO
    );
    assert_eq!(script["limits"]["max_decoded_bytes"], 2 * GIB);
    assert_eq!(
        script["limits"]["max_gpu_brick_atlas_uploaded_bytes"],
        2 * GIB
    );
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["width"],
        T5_QUAL_001_VIEWPORT_WIDTH
    );
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["height"],
        T5_QUAL_001_VIEWPORT_HEIGHT
    );
    assert_eq!(script_render_modes_json(&script), json!(["mip"]));
    assert!(
        commands
            .iter()
            .any(|command| command["command"] == "set_viewer_layout"
                && command["layout"] == "four_panel")
    );
    assert!(commands.iter().any(
        |command| command["command"] == "set_viewer_layout" && command["layout"] == "single3d"
    ));
    for expected in [
        "cross_section_pan",
        "cross_section_slice_step",
        "cross_section_zoom",
        "cross_section_rotate",
        "probe_panel_hover",
    ] {
        assert!(command_names.contains(&expected), "missing {expected}");
    }
    let xz_stream_assert = commands
        .iter()
        .find(|command| command["condition"]["cross_section_stream"]["panel"] == "xz")
        .expect("missing xz stream assertion");
    assert_eq!(
        xz_stream_assert["condition"]["cross_section_stream"]["priority"],
        "current_frame"
    );
    assert_eq!(
        xz_stream_assert["condition"]["cross_section_stream"]["min_requested"],
        1
    );
    assert_eq!(
        xz_stream_assert["condition"]["cross_section_stream"]["min_completed"],
        1
    );
    assert_eq!(
        xz_stream_assert["condition"]["cross_section_stream"]["min_visible_chunks"],
        1
    );
    assert!(xz_stream_assert["condition"]["cross_section_stream"]["min_selected_bricks"].is_null());
    assert!(
        xz_stream_assert["condition"]["cross_section_stream"]["min_queued_current_frame"].is_null()
    );
    for panel in ["xy", "yz"] {
        let stream_assert = commands
            .iter()
            .find(|command| command["condition"]["cross_section_stream"]["panel"] == panel)
            .unwrap_or_else(|| panic!("missing {panel} stream assertion"));
        assert!(stream_assert["condition"]["cross_section_stream"]["priority"].is_null());
        assert_eq!(
            stream_assert["condition"]["cross_section_stream"]["active_panel_at_submission"],
            "xz"
        );
        assert_eq!(
            stream_assert["condition"]["cross_section_stream"]["min_completed"],
            1
        );
    }
    let panel_distinct_assert_count = commands
        .iter()
        .filter(|command| {
            command["condition"]["cross_section_panel_images_distinct"]["min_different_pixels"] == 1
        })
        .count();
    assert_eq!(panel_distinct_assert_count, 2);
    let four_panel_distinct_assert_count = commands
        .iter()
        .filter(|command| {
            command["condition"]["four_panel_images_distinct"]["min_different_pixels"] == 1
        })
        .count();
    assert_eq!(four_panel_distinct_assert_count, 2);
    assert!(
        commands
            .iter()
            .any(|command| command["condition"] == "cross_section_retired")
    );
    let hover_probe = commands
        .iter()
        .find(|command| command["command"] == "probe_panel_hover")
        .expect("missing panel hover probe");
    assert_eq!(hover_probe["panel"], "xz");
    assert_eq!(hover_probe["expected_status"], "value");
    assert_eq!(hover_probe["expect_value"], true);
    assert!(
        commands
            .iter()
            .any(|command| command["name"] == "t5-qual-001-four-panel-after-oblique-interaction")
    );
    assert_eq!(commands.last().unwrap()["command"], "quit");
    validate_product_automation_script(&script).unwrap();
}

#[test]
fn t5_qual_001_four_panel_fine_scale_script_records_zoomed_s0_gate() {
    let script = t5_qual_001_four_panel_fine_scale_script(Path::new("/tmp/T5-QUAL-001.m4d"));
    let commands = script["commands"].as_array().unwrap();
    let command_names: Vec<_> = commands
        .iter()
        .filter_map(|command| command["command"].as_str())
        .collect();

    assert_eq!(
        script["scenario"],
        T5_QUAL_001_FOUR_PANEL_FINE_SCALE_SCENARIO
    );
    assert_eq!(script["limits"]["max_decoded_bytes"], 2 * GIB);
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["width"],
        T5_QUAL_001_VIEWPORT_WIDTH
    );
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["height"],
        T5_QUAL_001_VIEWPORT_HEIGHT
    );
    assert_eq!(script_render_modes_json(&script), json!(["mip"]));
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "cross_section_zoom")
            .count(),
        3
    );
    assert!(commands.iter().any(|command| {
        command["command"] == "cross_section_zoom"
            && command["panel"] == "xz"
            && command["scroll_y_points"]
                .as_f64()
                .is_some_and(|value| value > 0.0)
    }));
    for panel in ["xy", "xz", "yz"] {
        let schedule_assert = commands
            .iter()
            .find(|command| {
                command["condition"]["cross_section_panel_schedule"]["panel"] == panel
                    && command["condition"]["cross_section_panel_schedule"]["target_scale_level"]
                        == 0
                    && command["condition"]["cross_section_panel_schedule"]["render_scale_level"]
                        == 0
            })
            .unwrap_or_else(|| panic!("missing fine-scale schedule assertion for {panel}"));
        assert_eq!(
            schedule_assert["condition"]["cross_section_panel_schedule"]["display_current"],
            true
        );
        let stream_assert = commands
            .iter()
            .find(|command| {
                command["condition"]["cross_section_stream"]["panel"] == panel
                    && command["condition"]["cross_section_stream"]["min_visible_chunks"] == 1
                    && command["condition"]["cross_section_stream"]["max_failed"] == 0
            })
            .unwrap_or_else(|| panic!("missing fine-scale stream assertion for {panel}"));
        assert_eq!(
            stream_assert["condition"]["cross_section_stream"]["timepoint"],
            0
        );
    }
    assert!(
        commands
            .iter()
            .any(|command| command["name"] == "t5-qual-001-four-panel-fine-scale-s0")
    );
    assert!(
        commands
            .iter()
            .any(|command| command["condition"] == "cross_section_retired")
    );
    assert_eq!(commands.last().unwrap()["command"], "quit");
    validate_product_automation_script(&script).unwrap();
}

#[test]
fn t5_qual_001_four_panel_continuous_cross_section_script_records_nonblank_stress_gate() {
    let script =
        t5_qual_001_four_panel_continuous_cross_section_script(Path::new("/tmp/T5-QUAL-001.m4d"));
    let commands = script["commands"].as_array().unwrap();
    let command_names: Vec<_> = commands
        .iter()
        .filter_map(|command| command["command"].as_str())
        .collect();

    assert_eq!(
        script["scenario"],
        T5_QUAL_001_FOUR_PANEL_CONTINUOUS_CROSS_SECTION_SCENARIO
    );
    assert_eq!(script["limits"]["max_decoded_bytes"], 2 * GIB);
    assert_eq!(
        script["limits"]["max_gpu_brick_atlas_uploaded_bytes"],
        2 * GIB
    );
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["width"],
        T5_QUAL_001_VIEWPORT_WIDTH
    );
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["height"],
        T5_QUAL_001_VIEWPORT_HEIGHT
    );
    assert_eq!(script_render_modes_json(&script), json!(["mip"]));
    for command_name in [
        "cross_section_rotate",
        "cross_section_slice_step",
        "cross_section_pan",
        "cross_section_zoom",
    ] {
        assert_eq!(
            command_names
                .iter()
                .filter(|&&name| name == command_name)
                .count(),
            6,
            "unexpected count for {command_name}"
        );
    }
    assert!(
        commands.iter().any(|command| {
            command["command"] == "cross_section_slice_step"
                && command["fast"].as_bool() == Some(true)
        }),
        "continuous 2D stress should include fast slice stepping"
    );
    assert!(commands.iter().any(|command| {
        command["command"] == "cross_section_zoom"
            && command["scroll_y_points"]
                .as_f64()
                .is_some_and(|value| value > 0.0)
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "cross_section_zoom"
            && command["scroll_y_points"]
                .as_f64()
                .is_some_and(|value| value < 0.0)
    }));
    let nonblank_assert_count = commands
        .iter()
        .filter(|command| {
            command["condition"]["cross_section_panel_nonblank"]["min_nonzero_rgb_pixels"].as_u64()
                == Some(1)
        })
        .count();
    assert_eq!(nonblank_assert_count, 24);
    for panel in ["xy", "xz", "yz"] {
        let schedule_assert = commands
            .iter()
            .find(|command| {
                command["condition"]["cross_section_panel_schedule"]["panel"] == panel
                    && command["condition"]["cross_section_panel_schedule"]
                        ["max_missing_occupied_bricks"]
                        == 0
                    && command["condition"]["cross_section_panel_schedule"]["display_current"]
                        == true
            })
            .unwrap_or_else(|| panic!("missing settled schedule assertion for {panel}"));
        assert!(
            schedule_assert["condition"]["cross_section_panel_schedule"]["min_generation"]
                .as_u64()
                .is_some_and(|value| value >= 4)
        );
        assert!(commands.iter().any(|command| {
            command["condition"]["cross_section_stream"]["panel"] == panel
                && command["condition"]["cross_section_stream"]["min_visible_chunks"] == 1
                && command["condition"]["cross_section_stream"]["max_failed"] == 0
        }));
    }
    assert!(
        commands
            .iter()
            .any(|command| command["name"] == "t5-qual-001-four-panel-continuous-settled")
    );
    assert!(
        commands
            .iter()
            .any(|command| command["condition"] == "cross_section_retired")
    );
    assert_eq!(commands.last().unwrap()["command"], "quit");
    validate_product_automation_script(&script).unwrap();
}

#[test]
fn t5_qual_002_four_panel_timepoint_script_records_2d_timepoint_updates() {
    let script = t5_qual_002_four_panel_timepoint_script(Path::new("/tmp/t5_qual_002.m4d"));
    let commands = script["commands"].as_array().unwrap();
    let command_names: Vec<_> = commands
        .iter()
        .filter_map(|command| command["command"].as_str())
        .collect();

    assert_eq!(
        script["scenario"],
        T5_QUAL_002_FOUR_PANEL_TIMEPOINT_SCENARIO
    );
    assert_eq!(script["limits"]["max_decoded_bytes"], 2 * GIB);
    assert_eq!(
        script["limits"]["max_gpu_brick_atlas_uploaded_bytes"],
        2 * GIB
    );
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["width"],
        T5_QUAL_002_VIEWPORT_WIDTH
    );
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["height"],
        T5_QUAL_002_VIEWPORT_HEIGHT
    );
    assert_eq!(script_render_modes_json(&script), json!(["mip"]));
    assert!(
        commands
            .iter()
            .any(|command| command["command"] == "set_viewer_layout"
                && command["layout"] == "four_panel")
    );
    assert!(commands.iter().any(
        |command| command["command"] == "set_viewer_layout" && command["layout"] == "single3d"
    ));
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "set_timepoint")
            .count(),
        1
    );
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "step_timepoint")
            .count(),
        1
    );
    for timepoint in [0, 1, 2] {
        assert!(
            commands.iter().any(|command| {
                command["condition"]["active_timepoint"]["timepoint"].as_u64() == Some(timepoint)
            }),
            "missing active timepoint assertion for {timepoint}"
        );
        let stream_assert_count = commands
            .iter()
            .filter(|command| {
                command["condition"]["cross_section_stream"]["timepoint"].as_u64()
                    == Some(timepoint)
                    && command["condition"]["cross_section_stream"]["min_visible_chunks"].as_u64()
                        == Some(1)
                    && command["condition"]["cross_section_stream"]["max_failed"].as_u64()
                        == Some(0)
            })
            .count();
        assert_eq!(stream_assert_count, 3);
    }
    assert_eq!(
        commands
            .iter()
            .filter(|command| {
                command["condition"]["cross_section_panel_schedule"]["display_current"].as_bool()
                    == Some(true)
                    && command["condition"]["cross_section_panel_schedule"]
                        ["max_missing_occupied_bricks"]
                        .as_u64()
                        == Some(0)
            })
            .count(),
        9
    );
    assert!(
        commands
            .iter()
            .any(|command| command["name"] == "t5_qual_002-four-panel-timepoint-2")
    );
    assert!(
        commands
            .iter()
            .any(|command| command["condition"] == "cross_section_retired")
    );
    assert_eq!(commands.last().unwrap()["command"], "quit");
    validate_product_automation_script(&script).unwrap();
}

#[test]
fn t5_qual_002_four_panel_autoplay_script_records_2d_playback_updates() {
    let script = t5_qual_002_four_panel_autoplay_script(Path::new("/tmp/t5_qual_002.m4d"));
    let commands = script["commands"].as_array().unwrap();
    let command_names: Vec<_> = commands
        .iter()
        .filter_map(|command| command["command"].as_str())
        .collect();

    assert_eq!(script["scenario"], T5_QUAL_002_FOUR_PANEL_AUTOPLAY_SCENARIO);
    assert_eq!(script["limits"]["max_decoded_bytes"], 2 * GIB);
    assert_eq!(
        script["limits"]["max_gpu_brick_atlas_uploaded_bytes"],
        2 * GIB
    );
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["width"],
        T5_QUAL_002_VIEWPORT_WIDTH
    );
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["height"],
        T5_QUAL_002_VIEWPORT_HEIGHT
    );
    assert_eq!(script_render_modes_json(&script), json!(["mip"]));
    assert!(
        commands
            .iter()
            .any(|command| command["command"] == "set_viewer_layout"
                && command["layout"] == "four_panel")
    );
    assert!(commands.iter().any(
        |command| command["command"] == "set_viewer_layout" && command["layout"] == "single3d"
    ));
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "set_timepoint")
            .count(),
        0
    );
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "step_timepoint")
            .count(),
        0
    );
    assert_eq!(
        commands
            .iter()
            .filter(|command| command["command"] == "set_playback")
            .count(),
        2
    );
    assert!(
        commands
            .iter()
            .any(|command| { command["condition"]["playback"]["playing"].as_bool() == Some(true) })
    );
    assert!(
        commands.iter().any(|command| {
            command["condition"]["playback"]["playing"].as_bool() == Some(false)
        })
    );
    assert!(commands.iter().any(|command| {
        command["condition"]["observed_timepoints"]["min_distinct"].as_u64() == Some(2)
    }));
    assert!(commands.iter().any(|command| {
        command["condition"]["cross_section_streams_match_active_timepoint"]["min_completed"]
            .as_u64()
            == Some(1)
            && command["condition"]["cross_section_streams_match_active_timepoint"]
                ["min_visible_chunks"]
                .as_u64()
                == Some(1)
            && command["condition"]["cross_section_streams_match_active_timepoint"]["max_failed"]
                .as_u64()
                == Some(0)
    }));
    for panel in ["xy", "xz", "yz"] {
        assert!(commands.iter().any(|command| {
            command["condition"]["cross_section_panel_nonblank"]["panel"] == panel
                && command["condition"]["cross_section_panel_nonblank"]["min_nonzero_rgb_pixels"]
                    .as_u64()
                    == Some(1)
        }));
    }
    assert!(
        commands
            .iter()
            .any(|command| command["name"] == "t5_qual_002-four-panel-autoplay-settled")
    );
    assert!(
        commands
            .iter()
            .any(|command| command["condition"] == "cross_section_retired")
    );
    assert_eq!(commands.last().unwrap()["command"], "quit");
    validate_product_automation_script(&script).unwrap();
}

#[test]
fn t5_qual_001_interaction_continuous_script_records_paced_mode_sequences() {
    let script = t5_qual_001_interaction_continuous_script(Path::new("/tmp/T5-QUAL-001.m4d"));
    let commands = script["commands"].as_array().unwrap();
    let command_names: Vec<_> = commands
        .iter()
        .filter_map(|command| command["command"].as_str())
        .collect();

    assert_eq!(
        script["scenario"],
        T5_QUAL_001_INTERACTION_CONTINUOUS_SCENARIO
    );
    assert_eq!(script["limits"]["max_decoded_bytes"], 2 * GIB);
    assert_eq!(
        script["limits"]["max_gpu_brick_atlas_uploaded_bytes"],
        2 * GIB
    );
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["width"],
        T5_QUAL_001_VIEWPORT_WIDTH
    );
    assert_eq!(
        script_requested_window_inner_size_points_json(&script)["height"],
        T5_QUAL_001_VIEWPORT_HEIGHT
    );
    assert_eq!(
        script_render_modes_json(&script),
        json!(["mip", "dvr", "iso"])
    );
    assert_eq!(script_frame_wait_count(&script), 4);
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "camera_orbit")
            .count(),
        54
    );
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "camera_pan")
            .count(),
        18
    );
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "camera_zoom")
            .count(),
        9
    );
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "copy_diagnostics")
            .count(),
        7
    );
    assert_eq!(
        command_names
            .iter()
            .filter(|&&name| name == "capture_screenshot")
            .count(),
        3
    );
    assert!(commands.iter().all(|command| {
        command["command"] != "copy_diagnostics"
            || command.as_object().is_some_and(|object| object.len() == 1)
    }));
    validate_product_automation_script(&script).unwrap();
    assert_eq!(commands.last().unwrap()["command"], "quit");
}

#[test]
fn product_validation_scenario_resolution_is_strict() {
    assert_eq!(
        ProductValidationScenario::resolve(None, None, None).unwrap(),
        ProductValidationScenario::GeneratedFixtureCameraSmoke
    );
    assert_eq!(
        ProductValidationScenario::resolve(Some("t5-qual-001-interaction-mip"), None, None)
            .unwrap(),
        ProductValidationScenario::T5Qual001InteractionMip
    );
    assert_eq!(
        ProductValidationScenario::resolve(Some("render-modes"), None, None).unwrap(),
        ProductValidationScenario::GeneratedFixtureRenderModes
    );
    assert_eq!(
        ProductValidationScenario::resolve(Some("t5-qual-001-render-modes"), None, None).unwrap(),
        ProductValidationScenario::T5Qual001InteractionRenderModes
    );
    assert_eq!(
        ProductValidationScenario::resolve(Some("t5-qual-001-continuous"), None, None).unwrap(),
        ProductValidationScenario::T5Qual001InteractionContinuous
    );
    assert_eq!(
        ProductValidationScenario::resolve(Some("t5-qual-001-four-panel"), None, None).unwrap(),
        ProductValidationScenario::T5Qual001FourPanelCrossSection
    );
    assert_eq!(
        ProductValidationScenario::resolve(Some("t5-qual-001-fine-scale"), None, None).unwrap(),
        ProductValidationScenario::T5Qual001FourPanelFineScale
    );
    assert_eq!(
        ProductValidationScenario::resolve(Some("t5-qual-001-four-panel-continuous"), None, None)
            .unwrap(),
        ProductValidationScenario::T5Qual001FourPanelContinuousCrossSection
    );
    assert_eq!(
        ProductValidationScenario::resolve(Some("t5_qual_002-timepoint"), None, None).unwrap(),
        ProductValidationScenario::T5Qual002FourPanelTimepoint
    );
    assert_eq!(
        ProductValidationScenario::resolve(Some("t5_qual_002-autoplay"), None, None).unwrap(),
        ProductValidationScenario::T5Qual002FourPanelAutoplay
    );
    assert!(
        ProductValidationScenario::resolve(Some("unknown"), None, None)
            .unwrap_err()
            .to_string()
            .contains("unknown product validation scenario")
    );
    assert!(
        ProductValidationScenario::resolve(
            Some(T5_QUAL_001_INTERACTION_MIP_SCENARIO),
            None,
            Some(PathBuf::from("/tmp/script.json")),
        )
        .unwrap_err()
        .to_string()
        .contains(CUSTOM_SCRIPT_ENV)
    );
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
        product_validation_output_dir(&ProductValidationScenario::T5Qual001InteractionMip),
        Path::new(OUTPUT_DIR).join(T5_QUAL_001_INTERACTION_MIP_SCENARIO)
    );
    assert_eq!(
        product_validation_output_dir(&ProductValidationScenario::T5Qual001InteractionRenderModes),
        Path::new(OUTPUT_DIR).join(T5_QUAL_001_INTERACTION_RENDER_MODES_SCENARIO)
    );
    assert_eq!(
        product_validation_output_dir(&ProductValidationScenario::T5Qual001InteractionContinuous),
        Path::new(OUTPUT_DIR).join(T5_QUAL_001_INTERACTION_CONTINUOUS_SCENARIO)
    );
    assert_eq!(
        product_validation_output_dir(&ProductValidationScenario::T5Qual001FourPanelCrossSection),
        Path::new(OUTPUT_DIR).join(T5_QUAL_001_FOUR_PANEL_CROSS_SECTION_SCENARIO)
    );
    assert_eq!(
        product_validation_output_dir(&ProductValidationScenario::T5Qual001FourPanelFineScale),
        Path::new(OUTPUT_DIR).join(T5_QUAL_001_FOUR_PANEL_FINE_SCALE_SCENARIO)
    );
    assert_eq!(
        product_validation_output_dir(
            &ProductValidationScenario::T5Qual001FourPanelContinuousCrossSection
        ),
        Path::new(OUTPUT_DIR).join(T5_QUAL_001_FOUR_PANEL_CONTINUOUS_CROSS_SECTION_SCENARIO)
    );
    assert_eq!(
        product_validation_output_dir(&ProductValidationScenario::T5Qual002FourPanelTimepoint),
        Path::new(OUTPUT_DIR).join(T5_QUAL_002_FOUR_PANEL_TIMEPOINT_SCENARIO)
    );
    assert_eq!(
        product_validation_output_dir(&ProductValidationScenario::T5Qual002FourPanelAutoplay),
        Path::new(OUTPUT_DIR).join(T5_QUAL_002_FOUR_PANEL_AUTOPLAY_SCENARIO)
    );
    assert_eq!(
        product_validation_output_dir(&ProductValidationScenario::CustomScript(PathBuf::from(
            "/tmp/script.json"
        ))),
        Path::new(OUTPUT_DIR).join(CUSTOM_SCRIPT_SCENARIO)
    );
}

#[test]
fn product_validation_cleanup_removes_legacy_root_artifacts_only() {
    let tempdir = tempfile::tempdir().unwrap();
    let base = tempdir.path();
    for artifact in LEGACY_ROOT_PRODUCT_VALIDATION_ARTIFACTS {
        fs::write(base.join(artifact), "stale").unwrap();
    }
    let scenario_dir = base.join(GENERATED_FIXTURE_SCENARIO);
    fs::create_dir(&scenario_dir).unwrap();
    let scenario_report = scenario_dir.join("product-validation-report.json");
    fs::write(&scenario_report, "current").unwrap();

    remove_legacy_root_product_validation_artifacts(base).unwrap();

    for artifact in LEGACY_ROOT_PRODUCT_VALIDATION_ARTIFACTS {
        assert!(
            !base.join(artifact).exists(),
            "{artifact} should be removed"
        );
    }
    assert_eq!(fs::read_to_string(scenario_report).unwrap(), "current");
}

#[test]
fn heavy_local_sample_scenarios_require_package_before_heavy_work() {
    for scenario in [
        ProductValidationScenario::T5Qual001InteractionMip,
        ProductValidationScenario::T5Qual001InteractionRenderModes,
        ProductValidationScenario::T5Qual001InteractionContinuous,
        ProductValidationScenario::T5Qual001FourPanelCrossSection,
        ProductValidationScenario::T5Qual001FourPanelFineScale,
        ProductValidationScenario::T5Qual001FourPanelContinuousCrossSection,
        ProductValidationScenario::T5Qual002FourPanelTimepoint,
        ProductValidationScenario::T5Qual002FourPanelAutoplay,
    ] {
        assert!(
            scenario
                .validate_package_arg(None)
                .unwrap_err()
                .to_string()
                .contains("requires <native-package.m4d>")
        );
    }
}

#[test]
fn custom_product_automation_script_validation_rejects_wrong_schema() {
    let script = json!({
        "schema": "wrong",
        "schema_version": PRODUCT_AUTOMATION_SCHEMA_VERSION,
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
}

#[test]
fn custom_product_automation_script_validation_requires_open_dataset_and_quit() {
    let missing_open = json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": PRODUCT_AUTOMATION_SCHEMA_VERSION,
        "scenario": "unit",
        "commands": [
            { "command": "quit" }
        ]
    });
    let missing_quit = json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": PRODUCT_AUTOMATION_SCHEMA_VERSION,
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
fn custom_product_automation_script_validation_rejects_bad_limits() {
    let unknown = json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": PRODUCT_AUTOMATION_SCHEMA_VERSION,
        "scenario": "unit",
        "limits": {
            "max_surprise_bytes": 1
        },
        "commands": [
            { "command": "open_dataset", "path": "/tmp/demo.m4d" },
            { "command": "quit" }
        ]
    });
    let non_integer = json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": PRODUCT_AUTOMATION_SCHEMA_VERSION,
        "scenario": "unit",
        "limits": {
            "max_decoded_bytes": "lots"
        },
        "commands": [
            { "command": "open_dataset", "path": "/tmp/demo.m4d" },
            { "command": "quit" }
        ]
    });

    assert!(
        validate_product_automation_script(&unknown)
            .unwrap_err()
            .to_string()
            .contains("unknown automation script limit")
    );
    assert!(
        validate_product_automation_script(&non_integer)
            .unwrap_err()
            .to_string()
            .contains("must be an unsigned integer")
    );
}

#[test]
fn display_status_names_are_report_stable() {
    assert_eq!(DisplayClass::RealDisplay.name(), "real_display");
    assert_eq!(DisplayClass::VirtualDisplay.name(), "virtual_display");
    assert_eq!(DisplayClass::Unsupported.name(), "unsupported");
}

#[test]
fn linux_status_rss_parser_reports_bytes() {
    let status = "Name:\tmirante4d-app\nVmRSS:\t  12345 kB\nThreads:\t1\n";

    assert_eq!(parse_linux_status_rss_bytes(status), Some(12_641_280));
    assert_eq!(parse_linux_status_rss_bytes("Name:\tmissing\n"), None);
}

#[test]
fn process_rss_guard_defaults_only_for_t5_qual_001_scenarios() {
    assert_eq!(
        ProductValidationScenario::GeneratedFixtureCameraSmoke.default_process_rss_limit_bytes(),
        None
    );
    assert_eq!(
        ProductValidationScenario::T5Qual001InteractionMip.default_process_rss_limit_bytes(),
        Some(8 * GIB)
    );
    assert_eq!(
        ProductValidationScenario::T5Qual001InteractionRenderModes
            .default_process_rss_limit_bytes(),
        Some(8 * GIB)
    );
    assert_eq!(
        ProductValidationScenario::T5Qual001InteractionContinuous.default_process_rss_limit_bytes(),
        Some(8 * GIB)
    );
    assert_eq!(
        ProductValidationScenario::T5Qual001FourPanelCrossSection.default_process_rss_limit_bytes(),
        Some(8 * GIB)
    );
    assert_eq!(
        ProductValidationScenario::T5Qual001FourPanelFineScale.default_process_rss_limit_bytes(),
        Some(8 * GIB)
    );
    assert_eq!(
        ProductValidationScenario::T5Qual001FourPanelContinuousCrossSection
            .default_process_rss_limit_bytes(),
        Some(8 * GIB)
    );
    assert_eq!(
        ProductValidationScenario::T5Qual002FourPanelTimepoint.default_process_rss_limit_bytes(),
        Some(8 * GIB)
    );
    assert_eq!(
        ProductValidationScenario::T5Qual002FourPanelAutoplay.default_process_rss_limit_bytes(),
        Some(8 * GIB)
    );
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

    let (status, failure_reason) =
        completed_product_validation_outcome(true, Some("passed"), None, Some(&automation_report));

    assert_eq!(status, ProductValidationStatus::Failed);
    assert!(
        failure_reason
            .as_deref()
            .unwrap()
            .contains("missing a nonblank viewport_capture artifact")
    );
}

#[test]
fn completed_product_validation_fails_with_blank_viewport_capture() {
    let automation_report = json!({
        "status": "passed",
        "artifacts": [{
            "kind": "viewport_capture",
            "path": "blank.ppm",
            "width": 2,
            "height": 2,
            "pixel_stats": {
                "pixel_count": 4,
                "nonzero_rgb_pixels": 0,
                "max_rgb": 0
            }
        }]
    });

    let (status, failure_reason) =
        completed_product_validation_outcome(true, Some("passed"), None, Some(&automation_report));

    assert_eq!(status, ProductValidationStatus::Failed);
    assert!(failure_reason.is_some());
}

#[test]
fn completed_product_validation_passes_with_nonblank_viewport_capture() {
    let automation_report = json!({
        "status": "passed",
        "artifacts": [{
            "kind": "viewport_capture",
            "path": "nonblank.ppm",
            "width": 2,
            "height": 2,
            "pixel_stats": {
                "pixel_count": 4,
                "nonzero_rgb_pixels": 1,
                "max_rgb": 255
            }
        }]
    });

    let (status, failure_reason) =
        completed_product_validation_outcome(true, Some("passed"), None, Some(&automation_report));

    assert_eq!(status, ProductValidationStatus::Passed);
    assert_eq!(failure_reason, None);
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
    let package = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let script = generated_fixture_camera_smoke_script(&package);
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
                "capture_source": "resident_brick_cpu_snapshot",
                "pixel_stats": {
                    "pixel_count": 256,
                    "nonzero_rgb_pixels": 32,
                    "min_rgb": 0,
                    "max_rgb": 255,
                    "mean_rgb": 12.0
                }
            }
        ],
        "app_update_timing_summary": {
            "kind": "app_update_timing_summary",
            "sample_count": 2
        },
        "display_refresh_timing_summary": {
            "kind": "display_refresh_timing_summary",
            "sample_count": 1
        },
        "input_to_present_timing_summary": {
            "kind": "input_to_present_proxy_timing_summary",
            "sample_count": 1,
            "measurement_scope": "automation_command_start_to_app_display_refresh_complete"
        },
        "cross_section_latency_summary": {
            "kind": "cross_section_latency_summary",
            "taxonomy_version": 1,
            "measurement_scope": "automation_cross_section_command_start_to_panel_displayed_generation",
            "presentation_proxy": "panel_displayed_generation_with_gpu_display_frame",
            "sample_count": 3,
            "pending_sample_count": 0,
            "latency_ms": {
                "sample_count": 3,
                "p50": 24.0,
                "p95": 48.0,
                "p99": 48.0,
                "max": 48.0
            },
            "warm_interaction_gate": {
                "threshold_ms": 250.0,
                "status": "passed"
            },
            "by_operation": {
                "pan": {
                    "latency_ms": {
                        "sample_count": 2,
                        "p50": 18.0,
                        "p95": 24.0,
                        "p99": 24.0,
                        "max": 24.0
                    },
                    "warm_interaction_gate": {
                        "threshold_ms": 250.0,
                        "status": "passed"
                    }
                },
                "oblique_rotation": {
                    "latency_ms": {
                        "sample_count": 1,
                        "p50": 48.0,
                        "p95": 48.0,
                        "p99": 48.0,
                        "max": 48.0
                    },
                    "warm_interaction_gate": {
                        "threshold_ms": 250.0,
                        "status": "passed"
                    }
                }
            }
        },
        "presentation_timing": {
            "kind": "presentation_timing",
            "status": "app_proxy_available_os_compositor_timestamp_unavailable",
            "os_compositor_present_timestamp": {
                "status": "unsupported_by_current_eframe_wgpu_integration"
            }
        },
        "events": [
            {
                "command": "assert",
                "details": {
                    "condition": "cross_section_panel_schedule",
                    "cross_section_snapshot": {
                        "schema": "mirante4d-cross-section-runtime-diagnostics",
                        "schema_version": 1,
                        "layout": "FourPanel",
                        "old_path_fallback_used": false,
                        "runtime": {
                            "visible_work": true,
                            "state_counts": {"cpu_resident": 3},
                            "cpu_resident_bytes": 4096
                        }
                    }
                }
            },
            {
                "command": "assert",
                "details": {
                    "condition": "cross_section_retired",
                    "cross_section_snapshot": {
                        "schema": "mirante4d-cross-section-runtime-diagnostics",
                        "schema_version": 1,
                        "layout": "Single3d",
                        "old_path_fallback_used": false,
                        "runtime": {
                            "visible_work": false,
                            "state_counts": {"cpu_resident": 3},
                            "cpu_resident_bytes": 4096
                        }
                    }
                }
            }
        ],
        "final_diagnostics": {
            "cross_section": {
                "schema": "mirante4d-cross-section-runtime-diagnostics",
                "schema_version": 1,
                "layout": "Single3d",
                "old_path_fallback_used": false,
                "runtime": {
                    "visible_work": false,
                    "state_counts": {"cpu_resident": 3},
                    "cpu_resident_bytes": 4096
                }
            },
            "gpu_adapter": {
                "name": "unit adapter",
                "backend": "Vulkan",
                "timestamp_queries_supported": true,
                "timestamp_queries_requested": true,
                "timestamp_queries_enabled": true
            },
            "gpu_timestamp_timing": {
                "kind": "gpu_timestamp_timing",
                "status": "enabled",
                "measurement_scope": "renderer_compute_pass_elapsed_time_from_wgpu_timestamp_queries",
                "sample_field": "gpu_compute_ms"
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
        gpu_timestamps_requested: true,
        preflight_only: false,
        process_rss_limit_bytes: Some(8 * GIB),
        process_peak_rss_bytes: Some(64 * MIB),
        process_rss_limit_exceeded: false,
        automation_status: Some("passed".to_owned()),
        exit_status: Some("0".to_owned()),
        exit_success: Some(true),
    });

    assert_eq!(report["dataset"]["manifest_status"], "loaded");
    assert_eq!(report["dataset"]["id"], "fixture-basic-u16-16cube");
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
        report["metrics"]["display_refresh_timing_summary"]["kind"],
        "display_refresh_timing_summary"
    );
    assert_eq!(
        report["metrics"]["app_update_timing_summary"]["kind"],
        "app_update_timing_summary"
    );
    assert_eq!(
        report["metrics"]["input_to_present_timing_summary"]["kind"],
        "input_to_present_proxy_timing_summary"
    );
    assert_eq!(
        report["metrics"]["input_to_present_timing_summary"]["measurement_scope"],
        "automation_command_start_to_app_display_refresh_complete"
    );
    assert_eq!(
        report["metrics"]["cross_section_latency_summary"]["kind"],
        "cross_section_latency_summary"
    );
    assert_eq!(
        report["metrics"]["cross_section_latency_summary"]["sample_count"],
        3
    );
    assert_eq!(
        report["metrics"]["cross_section_performance_gate_table"]["kind"],
        "cross_section_performance_gate_table"
    );
    assert_eq!(
        report["metrics"]["cross_section_performance_gate_table"]["pending_sample_count"],
        0
    );
    assert_eq!(
        report["metrics"]["cross_section_performance_gate_table"]["rows"][0]["operation"],
        "pan"
    );
    assert_eq!(
        report["metrics"]["cross_section_performance_gate_table"]["rows"][0]["p95_ms"],
        24.0
    );
    assert_eq!(
        report["metrics"]["cross_section_performance_gate_table"]["rows"][0]["status"],
        "passed"
    );
    assert_eq!(
        report["metrics"]["cross_section_performance_gate_table"]["rows"][3]["operation"],
        "oblique_rotation"
    );
    assert_eq!(
        report["metrics"]["cross_section_performance_gate_table"]["rows"][3]["p95_ms"],
        48.0
    );
    assert_eq!(
        report["metrics"]["cross_section_performance_gate_table"]["rows"][4]["status"],
        "missing_samples"
    );
    assert_eq!(
        report["metrics"]["cross_section_runtime"]["kind"],
        "cross_section_runtime_metrics"
    );
    assert_eq!(
        report["metrics"]["cross_section_runtime"]["snapshot_count"],
        2
    );
    assert_eq!(
        report["metrics"]["cross_section_runtime"]["final"]["layout"],
        "Single3d"
    );
    assert_eq!(
        report["metrics"]["cross_section_runtime"]["latest_assertion"]["layout"],
        "Single3d"
    );
    assert_eq!(
        report["metrics"]["cross_section_runtime"]["latest_visible_work_assertion"]["layout"],
        "FourPanel"
    );
    assert_eq!(
        report["metrics"]["cross_section_runtime"]["latest_visible_work_assertion"]["old_path_fallback_used"],
        false
    );
    assert_eq!(
        report["metrics"]["cross_section_runtime"]["latest_visible_work_assertion"]["runtime"]["cpu_resident_bytes"],
        4096
    );
    assert_eq!(report["gpu_adapter"]["name"], "unit adapter");
    assert_eq!(report["gpu_timestamp_timing"]["status"], "enabled");
    assert_eq!(
        report["metrics"]["gpu_timestamp_timing"]["sample_field"],
        "gpu_compute_ms"
    );
    assert_eq!(
        report["presentation_timing"]["status"],
        "app_proxy_available_os_compositor_timestamp_unavailable"
    );
    assert_eq!(
        report["metrics"]["presentation_timing"]["os_compositor_present_timestamp"]["status"],
        "unsupported_by_current_eframe_wgpu_integration"
    );
    assert_eq!(report["environment"]["display"], "real_display");
    assert_eq!(report["environment"]["display_class"], "real_display");
    assert_eq!(report["environment"]["display_class_source"], "unit");
    assert_eq!(
        report["environment"]["product_validate_gpu_timestamps_requested"],
        true
    );
    assert_eq!(
        report["environment"]["product_validate_preflight_only"],
        false
    );
    assert_eq!(report["limits"]["process_rss_limit_bytes"], 8 * GIB);
    assert_eq!(report["process"]["rss_limit_bytes"], 8 * GIB);
    assert_eq!(report["process"]["peak_rss_bytes"], 64 * MIB);
    assert_eq!(report["process"]["rss_limit_exceeded"], false);
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
    assert!(report["scenario"]["observed_client_area_pixels"].is_null());
    assert_eq!(report["scenario"]["render_target_pixels"]["width"], 16);
    assert!(report["scenario"].get("viewport").is_none());
    assert!(report["limits"].get("viewport").is_none());
    assert_eq!(report["scenario"]["name"], GENERATED_FIXTURE_SCENARIO);
    assert_eq!(
        report["scenario"]["automation_script_scenario"],
        GENERATED_FIXTURE_SCENARIO
    );
    assert_eq!(report["scenario"]["render_modes"], json!(["mip"]));
    assert_eq!(report["limits"]["heavy_local_evidence"], false);
    assert_eq!(report["limits"]["decoded_byte_limit_enforced"], true);
    assert_eq!(report["limits"]["gpu_upload_byte_limit_enforced"], true);
    assert_eq!(report["limits"]["gpu_resident_byte_limit_enforced"], true);
    assert_eq!(
        report["limits"]["decoded_byte_limit_bytes"],
        script["limits"]["max_decoded_bytes"]
    );
    assert_eq!(
        report["limits"]["gpu_upload_byte_limit_bytes"],
        script["limits"]["max_gpu_brick_atlas_uploaded_bytes"]
    );
    assert_eq!(
        report["limits"]["gpu_resident_byte_limit_bytes"],
        script["limits"]["max_gpu_brick_atlas_resident_bytes"]
    );

    let custom_script_report = wrapper_report_json(WrapperReport {
        path: &wrapper_path,
        scenario_name: CUSTOM_SCRIPT_SCENARIO,
        status: ProductValidationStatus::Passed,
        failure_reason: None,
        started_at_epoch_ms: 0,
        duration_ms: 1.0,
        timeout_secs: 60,
        package: &package,
        script: &script_path,
        script_value: &script,
        automation_report: &automation_report_path,
        automation_report_value: Some(&automation_report),
        stdout: &stdout_path,
        stderr: &stderr_path,
        display: DisplayClassification {
            class: DisplayClass::VirtualDisplay,
            source: DISPLAY_CLASS_ENV,
        },
        gpu_timestamps_requested: false,
        preflight_only: false,
        process_rss_limit_bytes: None,
        process_peak_rss_bytes: None,
        process_rss_limit_exceeded: false,
        automation_status: Some("passed".to_owned()),
        exit_status: Some("0".to_owned()),
        exit_success: Some(true),
    });

    assert_eq!(
        custom_script_report["scenario"]["name"],
        CUSTOM_SCRIPT_SCENARIO
    );
    assert_eq!(
        custom_script_report["scenario"]["automation_script_scenario"],
        GENERATED_FIXTURE_SCENARIO
    );
}

#[test]
fn wrapper_report_marks_preflight_as_non_launch_unsupported_evidence() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let script = t5_qual_001_interaction_mip_script(&package);
    let wrapper_path = tempdir.path().join("product-validation-report.json");
    let script_path = tempdir.path().join("product-automation-script.json");
    let automation_report_path = tempdir.path().join("product-automation-report.json");
    let stdout_path = tempdir.path().join("stdout.log");
    let stderr_path = tempdir.path().join("stderr.log");

    let report = wrapper_report_json(WrapperReport {
        path: &wrapper_path,
        scenario_name: T5_QUAL_001_INTERACTION_MIP_SCENARIO,
        status: ProductValidationStatus::Unsupported,
        failure_reason: Some("product validation preflight requested".to_owned()),
        started_at_epoch_ms: 0,
        duration_ms: 1.0,
        timeout_secs: 180,
        package: &package,
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
        gpu_timestamps_requested: false,
        preflight_only: true,
        process_rss_limit_bytes: Some(8 * GIB),
        process_peak_rss_bytes: None,
        process_rss_limit_exceeded: false,
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
    assert_eq!(
        report["scenario"]["name"],
        T5_QUAL_001_INTERACTION_MIP_SCENARIO
    );
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
    assert_eq!(report["limits"]["heavy_local_evidence"], true);
    assert_eq!(report["limits"]["process_rss_limit_bytes"], 8 * GIB);
    assert!(report["process"]["exit_success"].is_null());
    assert!(report["process"]["exit_status"].is_null());
}

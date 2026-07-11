use super::*;
use mirante4d_renderer::RenderError;

#[test]
fn phase13_renderer_mode_cases_cover_supported_modes() {
    let display_window = DisplayWindow::new(0.0, f32::from(u16::MAX)).unwrap();
    let iso_policy = Phase13IsoThresholdPolicy {
        threshold: 123,
        threshold_source_value: 123.0,
        source: "test",
        display_window,
        resident_min: None,
        resident_max: None,
        occupied_bricks: 0,
    };
    let cases = phase13_render_mode_cases(
        Shape3D::new(8, 8, 8).unwrap(),
        GridToWorld::identity(),
        &iso_policy,
    );
    let labels = cases.iter().map(|case| case.label).collect::<Vec<_>>();

    assert_eq!(labels, vec!["mip", "dvr", "iso"]);
    assert!(!cases[0].order_dependent);
    assert!(cases[1].order_dependent);
    assert!(cases[2].order_dependent);
    match cases[2].integer_mode {
        CameraRenderMode::Isosurface { parameters } => {
            assert_eq!(parameters.transfer.window, display_window);
            assert_eq!(parameters.transfer.curve, TransferCurve::Linear);
            assert!(!parameters.transfer.invert);
            assert_eq!(
                parameters.display_level,
                f32::from(123_u16) / f32::from(u16::MAX)
            );
        }
        _ => panic!("expected ISO mode"),
    }
    match cases[2].f32_mode {
        CameraRenderModeF32::Isosurface { parameters } => {
            assert_eq!(parameters.transfer.window, display_window);
            assert_eq!(parameters.transfer.curve, TransferCurve::Linear);
            assert!(!parameters.transfer.invert);
            assert_eq!(
                parameters.display_level,
                f32::from(123_u16) / f32::from(u16::MAX)
            );
        }
        _ => panic!("expected Float32 ISO mode"),
    }
}

#[test]
fn phase13_float32_mode_cases_use_normalized_transfers() {
    let display_window = DisplayWindow::new(0.0, 1.0).unwrap();
    let iso_policy = Phase13IsoThresholdPolicy {
        threshold: 1,
        threshold_source_value: 0.5,
        source: "test",
        display_window,
        resident_min: Some(0.0),
        resident_max: Some(1.0),
        occupied_bricks: 1,
    };
    let cases = phase13_render_mode_cases(
        Shape3D::new(8, 8, 8).unwrap(),
        GridToWorld::identity(),
        &iso_policy,
    );

    match cases[1].f32_mode {
        CameraRenderModeF32::Dvr { parameters } => {
            assert_eq!(
                parameters.color_transfer,
                ScalarDisplayTransfer::identity_f32()
            );
            assert_eq!(
                parameters.opacity_transfer,
                ScalarDisplayTransfer::identity_f32()
            );
        }
        _ => panic!("expected Float32 DVR mode"),
    }
    match cases[2].f32_mode {
        CameraRenderModeF32::Isosurface { parameters } => {
            assert_eq!(parameters.transfer.window, display_window);
            assert_eq!(parameters.display_level, 0.5);
        }
        _ => panic!("expected Float32 ISO mode"),
    }
}

#[test]
fn phase13_iso_threshold_prefers_meaningful_display_window() {
    let display = LayerDisplay::new(true, DisplayWindow::new(23.0, 156.0).unwrap(), 1.0).unwrap();

    let (threshold, source) = phase13_iso_threshold_from_range(display, Some(0.0), Some(161.0));

    assert_eq!(threshold, 90);
    assert_eq!(source, "display_window_midpoint");
}

#[test]
fn phase13_iso_threshold_uses_resident_range_when_display_window_is_generic() {
    let display = LayerDisplay::new(true, DisplayWindow::new(0.0, 65_535.0).unwrap(), 1.0).unwrap();

    let (threshold, source) = phase13_iso_threshold_from_range(display, Some(0.0), Some(1_925.0));

    assert_eq!(threshold, 963);
    assert_eq!(source, "resident_max_midpoint");
}

#[test]
fn phase13_iso_threshold_handles_empty_resident_signal() {
    let display = LayerDisplay::new(true, DisplayWindow::new(0.0, 1.0).unwrap(), 1.0).unwrap();

    let (threshold, source) = phase13_iso_threshold_from_range(display, None, None);

    assert_eq!(threshold, 1);
    assert_eq!(source, "no_positive_resident_signal_default");
}

#[test]
fn phase13_renderer_error_kind_classifies_gpu_and_cpu_failures() {
    assert_eq!(
        phase13_gpu_error_kind(&GpuRenderError::BudgetExceeded {
            resource: "brick atlas",
            required_bytes: 32,
            budget_bytes: 16,
        }),
        "budget_exceeded"
    );
    assert_eq!(
        phase13_gpu_error_kind(&GpuRenderError::BufferTooLarge {
            resource: "brick atlas",
            required_bytes: 32,
            limit_bytes: 16,
        }),
        "backend_limit"
    );
    assert_eq!(
        phase13_gpu_error_kind(&GpuRenderError::UnsupportedCameraMode("mode")),
        "invalid_mode_parameter"
    );
    assert_eq!(
        phase13_render_error_kind(&RenderError::DimensionTooLarge {
            axis: "x",
            value: u64::from(u32::MAX) + 1,
        }),
        "backend_limit"
    );
    assert_eq!(
        phase13_render_error_kind(&RenderError::InvalidViewport {
            width: 0,
            height: 1,
        }),
        "invalid_mode_parameter"
    );
    assert_eq!(
        phase13_render_error_kind(&RenderError::InvalidPixelCoverageBuffer {
            width: 2,
            height: 2,
            expected: 4,
            actual: 3,
        }),
        "invalid_mode_parameter"
    );
    assert_eq!(
        phase13_render_error_kind(&RenderError::InvalidPixelCoverageValue { index: 1, value: 2 }),
        "invalid_mode_parameter"
    );
}

#[test]
fn phase13_capacity_probe_error_json_records_policy_and_error_kind() {
    let mode_case = Phase13RenderModeCase {
        label: "dvr",
        integer_mode: CameraRenderMode::Dvr {
            parameters: phase13_dvr_parameters_u16(),
        },
        f32_mode: CameraRenderModeF32::Dvr {
            parameters: phase13_dvr_parameters_f32(),
        },
        order_dependent: true,
    };
    let error = GpuRenderError::BudgetExceeded {
        resource: "brick atlas packed uint16 values",
        required_bytes: 32,
        budget_bytes: 1,
    };

    let report = phase13_capacity_probe_error_json(&mode_case, 1.25, &error);

    assert_eq!(report["render_mode"], "dvr");
    assert_eq!(report["order_dependent"], true);
    assert_eq!(report["ok"], false);
    assert_eq!(report["error_kind"], "budget_exceeded");
    assert_eq!(report["batched_fallback_attempted"], false);
    assert!(
        report["expected_policy"]
            .as_str()
            .unwrap()
            .contains("must downgrade or fail visibly")
    );
}

#[test]
fn phase13_failure_policy_probe_records_backend_limits_and_downgrade_policy() {
    let report = phase13_failure_policy_probe_report();

    assert_eq!(report["ok"], true);
    assert_eq!(report["summary"]["cases"], 8);
    assert_eq!(report["summary"]["user_visible_cases"], 8);
    assert_eq!(report["summary"]["backend_limit_cases"], 3);
    assert_eq!(report["summary"]["valid_lod_downgrade_cases"], 5);

    let cases = report["cases"].as_array().unwrap();
    let backend_limit = cases
        .iter()
        .find(|case| case["label"] == "backend_limit_buffer_too_large")
        .unwrap();
    assert_eq!(backend_limit["error_kind"], "backend_limit");
    assert_eq!(backend_limit["valid_lod_downgrade"], true);
    assert_eq!(backend_limit["hidden_dense_fallback_allowed"], false);

    let invalid_transform = cases
        .iter()
        .find(|case| case["label"] == "invalid_transform_empty_volume")
        .unwrap();
    assert_eq!(invalid_transform["error_kind"], "invalid_transform");
    assert_eq!(invalid_transform["valid_lod_downgrade"], false);
}

#[test]
fn phase13_refinement_rejection_reasons_prioritize_capacity_then_responsiveness() {
    assert!(phase13_refinement_rejection_reasons(true, true, true).is_empty());
    assert_eq!(
        phase13_refinement_rejection_reasons(false, true, false),
        vec!["visible_brick_budget_limited"]
    );
    assert_eq!(
        phase13_refinement_rejection_reasons(true, false, false),
        vec!["decoded_byte_budget_limited"]
    );
    assert_eq!(
        phase13_refinement_rejection_reasons(true, true, false),
        vec!["responsive_current_frame_budget_limited"]
    );
    assert_eq!(
        phase13_refinement_rejection_reasons(false, false, false),
        vec![
            "visible_brick_budget_limited",
            "decoded_byte_budget_limited"
        ]
    );
}

#[test]
fn phase13_cache_expectation_distinguishes_reuse_from_identity_change() {
    let reuse_delta = Phase13GpuStatsDelta {
        brick_atlas_cache_hits: 1,
        brick_atlas_cache_misses: 0,
        brick_atlas_uploads: 0,
        brick_atlas_uploaded_bytes: 0,
    };
    assert!(phase13_cache_expectation_met(
        Phase13CacheExpectation::ReuseExistingAtlas,
        Some(reuse_delta)
    ));
    assert!(!phase13_cache_expectation_met(
        Phase13CacheExpectation::DistinctAtlasForIdentityChange,
        Some(reuse_delta)
    ));

    let identity_change_delta = Phase13GpuStatsDelta {
        brick_atlas_cache_hits: 0,
        brick_atlas_cache_misses: 1,
        brick_atlas_uploads: 3,
        brick_atlas_uploaded_bytes: 4096,
    };
    assert!(phase13_cache_expectation_met(
        Phase13CacheExpectation::DistinctAtlasForIdentityChange,
        Some(identity_change_delta)
    ));
    assert!(!phase13_cache_expectation_met(
        Phase13CacheExpectation::ReuseExistingAtlas,
        Some(identity_change_delta)
    ));
}

#[test]
fn phase13_viewport_matrix_uses_phase11_default_scenarios() {
    let scenarios = phase11_viewport_matrix_for_shape(Shape3D {
        z: 64,
        y: 768,
        x: 1536,
    })
    .unwrap();
    let labels = scenarios
        .iter()
        .map(|scenario| scenario.label.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        labels,
        vec![
            "square_512",
            "hd_720p",
            "full_hd_1080p",
            "default_package_capped"
        ]
    );
}

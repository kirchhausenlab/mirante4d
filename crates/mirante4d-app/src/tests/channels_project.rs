#[test]
fn workbench_shell_exposes_channel_display_controls() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();

    let harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 720.0))
        .with_pixels_per_point(1.0)
        .build_eframe(|cc| test_workbench_app_for_ui_harness(cc, state));

    harness.get_by_label("Layers");
    harness.get_by_label("channel visible");
    harness.get_by_label("channel opacity");
    harness.get_by_label("display window");
    harness.get_by_label("channel color");
    harness.get_by_label("transfer gamma");
    harness.get_by_label("invert LUT");
    harness.get_by_label("transfer preset");
    harness.get_by_label("Channel Presets");
}

#[test]
fn active_layer_histogram_is_exact_for_loaded_dense_volume() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();

    let histogram = active_layer_histogram_summary(&mut state);
    let window = auto_dense_window_from_histogram(&histogram).unwrap();

    assert_eq!(histogram.status, HistogramStatus::Exact);
    assert_eq!(histogram.bin_count, 32);
    assert_eq!(histogram.sample_count, 16 * 16 * 16);
    assert_eq!(histogram.min_value, 0.0);
    assert_eq!(
        histogram.max_value,
        state.active_intensity_summary.max as f32
    );
    assert_eq!(histogram.bins.iter().sum::<u64>(), histogram.sample_count);
    assert!(window.low >= histogram.min_value);
    assert!(window.high <= histogram.max_value);
    assert!(window.high > window.low);
}

#[test]
fn active_layer_histogram_uses_data_engine_sample_when_no_dense_or_resident_data() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let dense_max = state.active_intensity_summary.max as f32;
    state.active_volume = None;
    state.active_volume_f32 = None;
    state.resident_bricks.clear();
    state.resident_bricks_by_layer.clear();
    state.resident_bricks_f32.clear();
    state.resident_bricks_f32_by_layer.clear();

    let histogram = active_layer_histogram_summary(&mut state);
    let window = auto_dense_window_from_histogram(&histogram).unwrap();

    match &histogram.status {
        HistogramStatus::Sampled { source } => {
            assert!(source.contains("data engine s0"));
            assert!(source.contains("1/1br"));
        }
        other => panic!("expected data-engine sampled histogram, got {other:?}"),
    }
    assert_eq!(histogram.bin_count, 32);
    assert_eq!(histogram.sample_count, 16 * 16 * 16);
    assert_eq!(histogram.min_value, 0.0);
    assert_eq!(histogram.max_value, dense_max);
    assert_eq!(histogram.bins.iter().sum::<u64>(), histogram.sample_count);
    assert!(window.low >= histogram.min_value);
    assert!(window.high <= histogram.max_value);
    assert!(window.high > window.low);
}

#[test]
fn active_layer_histogram_caches_data_engine_sample() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.active_volume = None;
    state.active_volume_f32 = None;
    state.resident_bricks.clear();
    state.resident_bricks_by_layer.clear();
    state.resident_bricks_f32.clear();
    state.resident_bricks_f32_by_layer.clear();

    let first = active_layer_histogram_summary(&mut state);
    let stats_after_first = state.dataset.stats().unwrap();
    let second = active_layer_histogram_summary(&mut state);
    let stats_after_second = state.dataset.stats().unwrap();

    assert!(matches!(first.status, HistogramStatus::Sampled { .. }));
    assert_eq!(first, second);
    assert!(stats_after_first.brick_reads > 0);
    assert_eq!(
        stats_after_second.brick_reads,
        stats_after_first.brick_reads
    );
}

#[test]
fn active_layer_histogram_uses_data_engine_sample_for_float32_layer() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_float32_app_fixture(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.active_volume = None;
    state.active_volume_f32 = None;
    state.resident_bricks.clear();
    state.resident_bricks_by_layer.clear();
    state.resident_bricks_f32.clear();
    state.resident_bricks_f32_by_layer.clear();

    let histogram = active_layer_histogram_summary(&mut state);
    let window = auto_dense_window_from_histogram(&histogram).unwrap();

    match &histogram.status {
        HistogramStatus::Sampled { source } => {
            assert!(source.contains("data engine s0"));
            assert!(source.contains("br"));
        }
        other => panic!("expected data-engine sampled histogram, got {other:?}"),
    }
    assert_eq!(histogram.bin_count, 32);
    assert!(histogram.sample_count > 0);
    assert!(histogram.max_value > histogram.min_value);
    assert_eq!(histogram.bins.iter().sum::<u64>(), histogram.sample_count);
    assert!(window.low >= histogram.min_value);
    assert!(window.high <= histogram.max_value);
    assert!(window.high > window.low);
}

#[test]
fn active_layer_histogram_reports_unavailable_for_data_engine_failure() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.active_volume = None;
    state.active_volume_f32 = None;
    state.resident_bricks.clear();
    state.resident_bricks_by_layer.clear();
    state.active_layer_id = "missing-layer".to_owned();

    let histogram = active_layer_histogram_summary(&mut state);
    let err = auto_dense_window_from_histogram(&histogram).unwrap_err();

    match &histogram.status {
        HistogramStatus::Unavailable { reason } => {
            assert!(reason.contains("sampled histogram read failed"));
            assert!(reason.contains("missing-layer"));
        }
        other => panic!("expected unavailable histogram, got {other:?}"),
    }
    assert_eq!(histogram.sample_count, 0);
    assert!(err.to_string().contains("cannot auto-window"));
}

#[test]
fn active_layer_histogram_is_sampled_for_resident_u16_bricks() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let dense_max = state.active_intensity_summary.max as f32;
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 4).unwrap();

    let submission = submit_visible_bricks_to_pool(&mut state, &pool);
    assert!(submission.queued_current);
    for _ in 0..submission.current_tickets.len() {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(apply_brick_read_outcome(&mut state, outcome));
    }
    assert!(state.brick_stream_complete);
    state.active_volume = None;
    state.active_volume_f32 = None;
    assert!(!state.resident_histogram_samples.is_empty());

    let histogram = active_layer_histogram_summary(&mut state);
    let window = auto_dense_window_from_histogram(&histogram).unwrap();

    match &histogram.status {
        HistogramStatus::Sampled { source } => {
            assert!(source.contains("resident s0"));
            assert!(source.contains("1br"));
        }
        other => panic!("expected sampled histogram, got {other:?}"),
    }
    assert_eq!(histogram.bin_count, 32);
    assert_eq!(histogram.sample_count, 16 * 16 * 16);
    assert_eq!(histogram.min_value, 0.0);
    assert_eq!(histogram.max_value, dense_max);
    assert_eq!(histogram.bins.iter().sum::<u64>(), histogram.sample_count);
    assert!(window.low >= histogram.min_value);
    assert!(window.high <= histogram.max_value);
    assert!(window.high > window.low);
}

#[test]
fn resident_histogram_without_worker_samples_is_pending_not_scanned_from_ui() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 4).unwrap();

    let submission = submit_visible_bricks_to_pool(&mut state, &pool);
    assert!(submission.queued_current);
    for _ in 0..submission.current_tickets.len() {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(apply_brick_read_outcome(&mut state, outcome));
    }
    assert!(state.brick_stream_complete);
    assert!(!state.resident_bricks.is_empty());
    state.active_volume = None;
    state.active_volume_f32 = None;
    state.resident_histogram_samples.clear();
    state.resident_histogram_generation = state.resident_histogram_generation.saturating_add(1);
    state.active_histogram_cache = None;

    let histogram = active_layer_histogram_summary(&mut state);

    match &histogram.status {
        HistogramStatus::Pending { reason } => {
            assert!(reason.contains("resident histogram samples pending"));
        }
        other => panic!("expected pending resident histogram, got {other:?}"),
    }
    assert_eq!(histogram.sample_count, 0);
    assert!(histogram.bins.is_empty());
    assert!(!histogram_can_auto_window(&histogram));
}

#[test]
fn active_layer_histogram_is_sampled_for_resident_f32_bricks() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_float32_app_fixture(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 32).unwrap();

    let submission = submit_visible_bricks_to_pool(&mut state, &pool);
    assert!(submission.queued_current);
    for _ in 0..submission.current_tickets.len() {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(apply_brick_read_outcome(&mut state, outcome));
    }
    assert!(state.brick_stream_complete);
    state.active_volume = None;
    state.active_volume_f32 = None;

    let histogram = active_layer_histogram_summary(&mut state);
    let window = auto_dense_window_from_histogram(&histogram).unwrap();

    match &histogram.status {
        HistogramStatus::Sampled { source } => {
            assert!(source.contains("resident s0"));
            assert!(source.contains("br"));
        }
        other => panic!("expected sampled histogram, got {other:?}"),
    }
    assert_eq!(histogram.bin_count, 32);
    assert!(histogram.sample_count > 0);
    assert!(histogram.max_value > histogram.min_value);
    assert_eq!(histogram.bins.iter().sum::<u64>(), histogram.sample_count);
    assert!(window.low >= histogram.min_value);
    assert!(window.high <= histogram.max_value);
    assert!(window.high > window.low);
}

#[test]
fn workbench_shell_exposes_histogram_and_auto_window_controls() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();

    let harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 720.0))
        .with_pixels_per_point(1.0)
        .build_eframe(|cc| test_workbench_app_for_ui_harness(cc, state));

    harness.get_by_label("histogram");
    harness.get_by_label("histogram bins");
    harness.get_by_label("Auto Dense");
    harness.get_by_label("Auto Signal");
}

#[test]
fn auto_signal_window_ignores_dominant_low_background_bin() {
    let histogram = LayerHistogramSummary {
        status: HistogramStatus::Exact,
        bin_count: 5,
        sample_count: 100,
        min_value: 0.0,
        max_value: 100.0,
        bins: vec![80, 0, 5, 10, 5],
    };

    let dense = auto_dense_window_from_histogram(&histogram).unwrap();
    let signal = auto_signal_window_from_histogram(&histogram).unwrap();

    assert!(dense.low < 20.0);
    assert!(signal.low >= 40.0);
    assert!(signal.high > signal.low);
    assert!(signal.high <= histogram.max_value);
}

#[test]
fn histogram_bins_label_is_plain_product_text_not_ascii_art() {
    let histogram = LayerHistogramSummary {
        status: HistogramStatus::Exact,
        bin_count: 4,
        sample_count: 10,
        min_value: 0.0,
        max_value: 3.0,
        bins: vec![0, 2, 8, 0],
    };

    let label = histogram_bins_label(&histogram);

    assert_eq!(label, "4 bins, peak count 8");
    assert!(!label.contains('@'));
    assert!(!label.contains('#'));
}

#[test]
fn playback_commands_step_timepoints_and_toggle_state() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    let ctx = egui::Context::default();

    let outcome =
        app.apply_workbench_command(WorkbenchCommand::SetPlayback { playing: true }, &ctx);
    assert!(!outcome.rerender_requested);
    assert!(app.playback.playing);
    assert!(app.playback.last_step_at.is_some());
    assert_eq!(
        playback_status_label(
            app.playback,
            app.state.active_timepoint,
            app.state.timepoint_count
        ),
        "playback playing | t 1/3"
    );

    app.apply_workbench_command(WorkbenchCommand::StepTimepoint { delta: 1 }, &ctx);
    assert_eq!(app.state.active_timepoint, TimeIndex(1));
    app.apply_workbench_command(WorkbenchCommand::StepTimepoint { delta: 1 }, &ctx);
    assert_eq!(app.state.active_timepoint, TimeIndex(2));
    app.apply_workbench_command(WorkbenchCommand::StepTimepoint { delta: 1 }, &ctx);
    assert_eq!(app.state.active_timepoint, TimeIndex(0));
    app.apply_workbench_command(WorkbenchCommand::StepTimepoint { delta: -1 }, &ctx);
    assert_eq!(app.state.active_timepoint, TimeIndex(2));

    app.apply_workbench_command(WorkbenchCommand::SetPlayback { playing: false }, &ctx);
    assert!(!app.playback.playing);
    assert!(app.playback.last_step_at.is_none());
    assert_eq!(
        playback_status_label(
            app.playback,
            app.state.active_timepoint,
            app.state.timepoint_count
        ),
        "playback stopped | t 3/3"
    );
}

#[test]
fn playback_stays_stopped_for_single_timepoint_dataset() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    let ctx = egui::Context::default();

    app.apply_workbench_command(WorkbenchCommand::SetPlayback { playing: true }, &ctx);

    assert!(!app.playback.playing);
    assert!(app.playback.last_step_at.is_none());
    assert_eq!(app.state.active_timepoint, TimeIndex(0));
    assert_eq!(
        playback_status_label(
            app.playback,
            app.state.active_timepoint,
            app.state.timepoint_count
        ),
        "playback stopped | t 1/1"
    );
}

#[test]
fn streaming_timepoint_command_dirties_cross_section_panels_without_dirtying_3d_panel() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.active_source_shape = Shape3D::new(512, 512, 512).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    let ctx = egui::Context::default();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    app.apply_workbench_command(WorkbenchCommand::SetViewerLayout(ViewerLayout::FourPanel), &ctx);
    for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::ThreeD, PanelId::Yz] {
        assert!(
            app.state
                .viewer_layout
                .record_panel_viewports(panel_id, presentation, render)
        );
        assert!(app.state.viewer_layout.mark_panel_displayed(panel_id, 1));
    }

    app.apply_workbench_command(WorkbenchCommand::StepTimepoint { delta: 1 }, &ctx);

    assert_eq!(app.state.active_timepoint, TimeIndex(1));
    let runtime = app.state.viewer_layout.four_panel_runtime().unwrap();
    for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        let panel = runtime.panel(panel_id).unwrap();
        assert_eq!(panel.generation, 2);
        assert!(
            !panel.display_current(),
            "{} should be dirty after a streaming timepoint change",
            panel_id.label()
        );
    }
    let three_d = runtime.panel(PanelId::ThreeD).unwrap();
    assert_eq!(three_d.generation, 1);
    assert!(three_d.display_current());
}

#[test]
fn streaming_timepoint_switch_preserves_last_nonblank_frame_while_loading() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let previous_pixels = state.frame.pixels().to_vec();

    assert!(previous_pixels.iter().any(|value| *value > 0));

    activate_streaming_timepoint_preserving_frame(&mut state, TimeIndex(1)).unwrap();

    assert_eq!(state.active_timepoint, TimeIndex(1));
    assert_eq!(state.frame.pixels(), previous_pixels.as_slice());
    assert_eq!(
        state.frame_fidelity.completeness,
        FrameCompleteness::Loading
    );
}

#[test]
fn playback_smoke_helper_renders_multiple_nonblank_timepoints() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();

    let frames =
        render_playback_steps_for_smoke(&mut state, None, 2, Duration::from_secs(2)).unwrap();

    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].timepoint, 1);
    assert_eq!(frames[1].timepoint, 2);
    assert!(frames.iter().all(|frame| frame.nonzero_pixels > 0));
}

#[test]
fn workbench_shell_exposes_playback_controls_for_time_series() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();

    let harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 720.0))
        .with_pixels_per_point(1.0)
        .build_eframe(|cc| test_workbench_app_for_ui_harness(cc, state));

    harness.get_by_label("First");
    harness.get_by_label("Prev");
    harness.get_by_label("Play");
    harness.get_by_label("Next");
    harness.get_by_label("Last");
    harness.get_by_label("playback stopped | t 1/3");
}

#[test]
fn dataset_reference_paths_are_relative_to_project_package_when_possible() {
    let tempdir = tempfile::tempdir().unwrap();
    let project_path = tempdir.path().join("projects/viewer.m4dproj");
    let dataset_path = tempdir.path().join("datasets/experiment.m4d");

    let manifest_path = dataset_reference_path_for_manifest(&project_path, &dataset_path);
    let resolved_path = dataset_reference_path_from_manifest(&project_path, &manifest_path);

    assert_eq!(
        manifest_path,
        PathBuf::from("../../datasets/experiment.m4d")
    );
    assert_eq!(resolved_path, dataset_path);
}

#[test]
fn project_package_roundtrip_preserves_active_no_data_status() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_uint8_no_data_app_fixture(tempdir.path());
    let state = open_dataset_and_render_first_frame(&root).unwrap();
    let session_path = tempdir.path().join("nodata-session.m4dproj");

    write_session_file(&session_path, &session_from_state(&state)).unwrap();
    let restored =
        open_state_from_session(&read_session_file(&session_path).unwrap(), None).unwrap();

    assert_eq!(
        active_layer_no_data_policy_label(&state),
        Some("value 255 (uint8), dilated 1 voxel".to_owned())
    );
    assert_eq!(
        active_layer_no_data_policy_label(&restored),
        Some("value 255 (uint8), dilated 1 voxel".to_owned())
    );
    assert_eq!(restored.dataset_path, root);
    assert_eq!(restored.active_layer_id, "ch0");
}

#[test]
fn project_package_roundtrip_restores_single3d_without_hidden_four_panel_runtime() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    let saved_cross_section = mirante4d_renderer::CrossSectionViewState::new(
        DVec3::new(1.25, 2.5, 3.75),
        glam::DQuat::from_rotation_z(0.25),
        0.125,
        2.5,
    );
    state.viewer_layout.cross_section = saved_cross_section;
    assert_eq!(state.viewer_layout.layout(), ViewerLayout::Single3d);
    assert!(!state.viewer_layout.has_four_panel_runtime());
    let session_path = tempdir.path().join("single3d-layout.m4dproj");

    write_session_file(&session_path, &session_from_state(&state)).unwrap();
    let project_json = fs::read_to_string(project_json_path(&session_path)).unwrap();
    assert!(project_json.contains("\"format\": \"mirante4d-project-v14\""));
    assert!(project_json.contains("\"viewer_layout\""));
    assert!(project_json.contains("\"layout\": \"single3d\""));
    assert!(project_json.contains("\"cross_section\""));
    for forbidden in [
        "presentation_viewport",
        "render_viewport",
        "displayed_generation",
        "active_cross_section_panel",
        "cross_section_schedule",
    ] {
        assert!(!project_json.contains(forbidden), "{forbidden} was persisted");
    }

    let restored =
        open_state_from_session(&read_session_file(&session_path).unwrap(), None).unwrap();

    assert_eq!(restored.viewer_layout.layout(), ViewerLayout::Single3d);
    assert!(!restored.viewer_layout.has_four_panel_runtime());
    assert_cross_section_state_approx_eq(
        restored.viewer_layout.cross_section,
        saved_cross_section,
    );
}

#[test]
fn project_package_roundtrip_restores_four_panel_cross_section_without_runtime_state() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    let saved_cross_section = mirante4d_renderer::CrossSectionViewState::new(
        DVec3::new(4.0, 5.0, 6.0),
        glam::DQuat::from_rotation_x(0.35) * glam::DQuat::from_rotation_z(-0.2),
        0.25,
        1.75,
    );
    state.viewer_layout.switch_to_four_panel();
    state.viewer_layout.cross_section = saved_cross_section;
    assert!(state.viewer_layout.record_panel_viewports(
        PanelId::Xz,
        PresentationViewport::new(240.0, 180.0).unwrap(),
        RenderViewport::new(480, 360).unwrap(),
    ));
    assert!(state.viewer_layout.mark_panel_displayed(PanelId::Xz, 1));
    assert!(state.viewer_layout.mark_active_cross_section_panel(PanelId::Xz));
    assert_eq!(
        state.viewer_layout.active_cross_section_panel(),
        Some(PanelId::Xz)
    );
    let session_path = tempdir.path().join("four-panel-layout.m4dproj");

    write_session_file(&session_path, &session_from_state(&state)).unwrap();
    let project_json = fs::read_to_string(project_json_path(&session_path)).unwrap();
    assert!(project_json.contains("\"layout\": \"four_panel\""));
    for forbidden in [
        "presentation_viewport",
        "render_viewport",
        "displayed_generation",
        "active_cross_section_panel",
        "cross_section_schedule",
    ] {
        assert!(!project_json.contains(forbidden), "{forbidden} was persisted");
    }
    let restored =
        open_state_from_session(&read_session_file(&session_path).unwrap(), None).unwrap();

    assert_eq!(restored.viewer_layout.layout(), ViewerLayout::FourPanel);
    assert_cross_section_state_approx_eq(
        restored.viewer_layout.cross_section,
        saved_cross_section,
    );
    let runtime = restored.viewer_layout.four_panel_runtime().unwrap();
    assert_eq!(runtime.active_cross_section_panel(), None);
    for panel in runtime.panels() {
        assert_eq!(panel.presentation_viewport, None);
        assert_eq!(panel.render_viewport, None);
        assert_eq!(panel.generation, 0);
        assert_eq!(panel.displayed_generation, None);
    }
}

#[test]
fn project_package_rejects_invalid_cross_section_session_state() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(&root).unwrap();
    let mut session = session_from_state(&state);

    session.viewer_layout.cross_section.orientation_xyzw = [0.0, 0.0, 0.0, 0.0];
    let err = open_state_from_session(&session, None).unwrap_err();
    assert!(
        err.to_string().contains("nonzero quaternion"),
        "unexpected error: {err:?}"
    );

    session = session_from_state(&state);
    session
        .viewer_layout
        .cross_section
        .scale_world_per_screen_point = 0.0;
    let err = open_state_from_session(&session, None).unwrap_err();
    assert!(
        err.to_string().contains("finite and positive"),
        "unexpected error: {err:?}"
    );
}

#[test]
fn project_package_roundtrip_restores_scene_artifacts() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    state.scene_artifacts = sample_scene_artifacts();
    let session_path = tempdir.path().join("scene-session.m4dproj");

    let session = session_from_state(&state);
    write_session_file(&session_path, &session).unwrap();
    let decoded = read_session_file(&session_path).unwrap();
    let restored = open_state_from_session(&decoded, None).unwrap();

    assert_eq!(decoded.format, SESSION_FORMAT);
    assert_eq!(restored.scene_artifacts.tracks().count(), 1);
    assert_eq!(restored.scene_artifacts.rois().count(), 1);
    assert_eq!(restored.scene_artifacts.annotations().count(), 1);
    assert_eq!(restored.scene_artifacts.measurements().count(), 1);
    assert!(
        restored
            .scene_artifacts
            .track(&SceneArtifactId::new("track", "track-a").unwrap())
            .is_some()
    );
    assert!(
        restored
            .scene_artifacts
            .roi(&SceneArtifactId::new("roi", "roi-a").unwrap())
            .is_some()
    );
}

#[test]
fn project_package_roundtrip_restores_analysis_results() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    compute_full_time_series_analysis(&mut state).unwrap();
    let session_path = tempdir.path().join("analysis-session.m4dproj");

    let session = session_from_state(&state);
    write_session_file(&session_path, &session).unwrap();
    let project_json = fs::read_to_string(project_json_path(&session_path)).unwrap();
    assert!(project_json.contains("artifacts/tables/0000-full-intensity-summary.m4dtable.json"));
    assert!(project_json.contains("artifacts/plots/0000-full-intensity-mean-trace.m4dplot.json"));
    assert!(!project_json.contains("\"rows\""));
    assert!(!project_json.contains("\"series\""));
    assert!(
        project_artifact_dir(&session_path, PROJECT_TABLES_DIR)
            .join("0000-full-intensity-summary.m4dtable.json")
            .is_file()
    );
    assert!(
        project_artifact_dir(&session_path, PROJECT_PLOTS_DIR)
            .join("0000-full-intensity-mean-trace.m4dplot.json")
            .is_file()
    );
    let restored =
        open_state_from_session(&read_session_file(&session_path).unwrap(), None).unwrap();

    assert_eq!(restored.analysis_tables.len(), 1);
    assert_eq!(restored.analysis_plots.len(), 1);
    assert_eq!(restored.analysis_operations.len(), 1);
    assert_eq!(
        restored.analysis_operations[0].kind,
        AnalysisOperationKind::FullIntensitySummary
    );
    assert_eq!(
        restored.analysis_tables[0].provenance.operation,
        "full_intensity_summary"
    );
    assert_eq!(
        restored.analysis_tables[0].state,
        AnalysisResultState::Complete
    );
}

#[test]
fn project_package_rejects_analysis_artifact_id_mismatch() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    compute_full_time_series_analysis(&mut state).unwrap();
    let session_path = tempdir.path().join("bad-analysis-session.m4dproj");

    let session = session_from_state(&state);
    write_session_file(&session_path, &session).unwrap();
    let manifest_json = fs::read_to_string(project_json_path(&session_path)).unwrap();
    let manifest: AppSessionManifest = serde_json::from_str(&manifest_json).unwrap();
    let table_artifact_path = session_path.join(&manifest.analysis_tables[0].artifact_path);
    let mut payload: AnalysisTableArtifactPayload =
        serde_json::from_str(&fs::read_to_string(&table_artifact_path).unwrap()).unwrap();
    payload.artifact_id = "wrong-table-id".to_owned();
    fs::write(
        &table_artifact_path,
        serde_json::to_string_pretty(&payload).unwrap(),
    )
    .unwrap();

    let err = read_session_file(&session_path).unwrap_err();

    assert!(err.to_string().contains("does not match project reference"));
}

#[test]
fn project_package_rejects_dataset_manifest_fingerprint_mismatch() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(&root).unwrap();
    let session_path = tempdir.path().join("wrong-dataset-session.m4dproj");

    write_session_file(&session_path, &session_from_state(&state)).unwrap();
    let manifest_json = fs::read_to_string(project_json_path(&session_path)).unwrap();
    let mut manifest: AppSessionManifest = serde_json::from_str(&manifest_json).unwrap();
    manifest.dataset.manifest_fingerprint_blake3 =
        "0000000000000000000000000000000000000000000000000000000000000000".to_owned();
    fs::write(
        project_json_path(&session_path),
        format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()),
    )
    .unwrap();
    let decoded = read_session_file(&session_path).unwrap();

    let err = open_state_from_session(&decoded, None).unwrap_err();

    assert!(
        err.to_string()
            .contains("project dataset manifest fingerprint")
    );
}

#[test]
fn project_dataset_relocation_opens_matching_dataset_and_marks_project_dirty() {
    let tempdir = tempfile::tempdir().unwrap();
    let dataset_root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let original_state = open_dataset_and_render_first_frame(dataset_root.clone()).unwrap();
    let mut session = session_from_state(&original_state);
    session.dataset.path = tempdir.path().join("missing-dataset.m4d");

    let relocated_state = open_state_from_session_with_relocated_dataset(
        &session,
        &dataset_root,
        None,
        &AppPreferences::default(),
    )
    .unwrap();

    assert_eq!(relocated_state.dataset_path, dataset_root);

    let project_path = tempdir.path().join("relocated-project.m4dproj");
    let clean_snapshot = ProjectDirtySnapshot::from_session_and_state(session, &relocated_state);
    let mut app = test_workbench_app_without_background_runtime(original_state);

    app.install_opened_project_state(project_path.clone(), relocated_state, clean_snapshot, None);

    assert!(app.project_dirty());
    assert!(app.save_project_to_path(project_path.clone()));
    assert!(!app.project_dirty());

    let saved_session = read_session_file(project_path).unwrap();
    assert_eq!(saved_session.dataset.path, app.state.dataset_path);
}

#[test]
fn project_dataset_relocation_rejects_identity_mismatch() {
    let tempdir = tempfile::tempdir().unwrap();
    let original_root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let other_root = write_fixture(FixtureKind::AnisotropicU16_16Cube, tempdir.path()).unwrap();
    let original_state = open_dataset_and_render_first_frame(original_root).unwrap();
    let mut session = session_from_state(&original_state);
    session.dataset.path = tempdir.path().join("missing-dataset.m4d");

    let err = open_state_from_session_with_relocated_dataset(
        &session,
        other_root,
        None,
        &AppPreferences::default(),
    )
    .unwrap_err();

    assert!(err.to_string().contains("project dataset id"), "{err}");
}

#[test]
fn project_autosave_snapshot_restores_recovery_state_without_authoritative_project_json() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    compute_full_time_series_analysis(&mut state).unwrap();
    let session_path = tempdir.path().join("recovery-session.m4dproj");

    let autosave_path = write_autosave_snapshot_for_state(&session_path, &state).unwrap();

    assert_eq!(autosave_path, autosave_project_json_path(&session_path));
    assert!(autosave_path.is_file());
    assert!(!project_json_path(&session_path).exists());
    let autosave_json = fs::read_to_string(&autosave_path).unwrap();
    assert!(autosave_json.contains("autosave/tables/0000-full-intensity-summary.m4dtable.json"));
    assert!(autosave_json.contains("autosave/plots/0000-full-intensity-mean-trace.m4dplot.json"));
    assert!(!autosave_json.contains("artifacts/tables/"));
    let autosave_manifest: AppSessionManifest = serde_json::from_str(&autosave_json).unwrap();
    assert_eq!(
        autosave_manifest.dataset.path,
        dataset_reference_path_for_manifest(&session_path, &root)
    );
    assert!(!autosave_manifest.dataset.path.is_absolute());

    let recovery = read_autosave_snapshot(&session_path).unwrap();
    let restored = open_state_from_recovery_session(&recovery, None).unwrap();

    assert_eq!(recovery.autosave_path, autosave_path);
    assert!(
        restored
            .last_workflow_message
            .as_deref()
            .unwrap()
            .contains("Opened autosave recovery")
    );
    assert_eq!(restored.analysis_tables.len(), 1);
    assert_eq!(restored.analysis_plots.len(), 1);
    assert_eq!(restored.analysis_operations.len(), 1);
}

#[cfg(unix)]
#[derive(Debug)]
struct M4dsegSentinels {
    directory: PathBuf,
    directory_payload: PathBuf,
    file: PathBuf,
    link: PathBuf,
    outside_payload: PathBuf,
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct M4dsegSentinelSnapshot {
    directory_identity: (u64, u64),
    directory_payload_identity: (u64, u64),
    directory_payload_bytes: Vec<u8>,
    file_identity: (u64, u64),
    file_bytes: Vec<u8>,
    link_identity: (u64, u64),
    link_target: PathBuf,
    outside_payload_identity: (u64, u64),
    outside_payload_bytes: Vec<u8>,
}

#[cfg(unix)]
fn create_m4dseg_sentinels(project_path: &std::path::Path) -> M4dsegSentinels {
    use std::os::unix::fs::symlink;

    let legacy_root = project_path.join("artifacts/segmentation");
    let directory = legacy_root.join("legacy-directory.m4dseg");
    let directory_payload = directory.join("payload.bin");
    let file = project_path.join("legacy-file.m4dseg");
    let link = legacy_root.join("legacy-link.m4dseg");
    let outside_root = project_path
        .parent()
        .unwrap()
        .join("outside-m4dseg-target");
    let outside_payload = outside_root.join("outside.bin");

    fs::create_dir_all(&directory).unwrap();
    fs::create_dir_all(&outside_root).unwrap();
    fs::write(&directory_payload, b"inside directory sentinel\0\xff").unwrap();
    fs::write(&file, b"inside file sentinel\0\xfe").unwrap();
    fs::write(&outside_payload, b"outside target sentinel\0\xfd").unwrap();
    symlink(&outside_root, &link).unwrap();

    M4dsegSentinels {
        directory,
        directory_payload,
        file,
        link,
        outside_payload,
    }
}

#[cfg(unix)]
fn m4dseg_sentinel_snapshot(sentinels: &M4dsegSentinels) -> M4dsegSentinelSnapshot {
    use std::os::unix::fs::MetadataExt;

    fn identity(metadata: fs::Metadata) -> (u64, u64) {
        (metadata.dev(), metadata.ino())
    }

    M4dsegSentinelSnapshot {
        directory_identity: identity(fs::symlink_metadata(&sentinels.directory).unwrap()),
        directory_payload_identity: identity(
            fs::symlink_metadata(&sentinels.directory_payload).unwrap(),
        ),
        directory_payload_bytes: fs::read(&sentinels.directory_payload).unwrap(),
        file_identity: identity(fs::symlink_metadata(&sentinels.file).unwrap()),
        file_bytes: fs::read(&sentinels.file).unwrap(),
        link_identity: identity(fs::symlink_metadata(&sentinels.link).unwrap()),
        link_target: fs::read_link(&sentinels.link).unwrap(),
        outside_payload_identity: identity(
            fs::symlink_metadata(&sentinels.outside_payload).unwrap(),
        ),
        outside_payload_bytes: fs::read(&sentinels.outside_payload).unwrap(),
    }
}

#[cfg(unix)]
#[test]
fn project_v13_and_autosave_rejection_leave_m4dseg_sentinels_untouched() {
    let tempdir = tempfile::tempdir().unwrap();
    let project_path = tempdir.path().join("legacy-project.m4dproj");
    let sentinels = create_m4dseg_sentinels(&project_path);
    let legacy_manifest = serde_json::json!({
        "format": "mirante4d-project-v13",
        "active_segmentation": {
            "artifact_path": sentinels.link.clone(),
            "artifact_id": "legacy",
            "source_layer_id": "ch0",
            "source_timepoint": 0
        }
    });
    fs::write(
        project_json_path(&project_path),
        serde_json::to_vec_pretty(&legacy_manifest).unwrap(),
    )
    .unwrap();
    fs::create_dir_all(project_path.join(PROJECT_AUTOSAVE_DIR)).unwrap();
    fs::write(
        autosave_project_json_path(&project_path),
        serde_json::to_vec_pretty(&legacy_manifest).unwrap(),
    )
    .unwrap();
    let before = m4dseg_sentinel_snapshot(&sentinels);

    let project_error = read_session_file(&project_path).unwrap_err();
    let autosave_error = read_autosave_snapshot(&project_path).unwrap_err();

    assert_eq!(
        project_error.to_string(),
        "unsupported session format \"mirante4d-project-v13\""
    );
    assert_eq!(
        autosave_error.to_string(),
        "unsupported session format \"mirante4d-project-v13\""
    );
    assert_eq!(m4dseg_sentinel_snapshot(&sentinels), before);
}

#[test]
fn project_v14_rejects_removed_active_segmentation_field() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let project_path = tempdir.path().join("strict-v14.m4dproj");
    write_session_file(&project_path, &session_from_state(&state)).unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(project_json_path(&project_path)).unwrap())
            .unwrap();
    manifest["active_segmentation"] = serde_json::Value::Null;
    fs::write(
        project_json_path(&project_path),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let error = read_session_file(&project_path).unwrap_err();

    assert!(error.to_string().contains("unknown field `active_segmentation`"));
}

#[cfg(unix)]
#[test]
fn project_v14_save_autosave_and_recovery_leave_m4dseg_sentinels_untouched() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let project_path = tempdir.path().join("sentinel-v14.m4dproj");
    let sentinels = create_m4dseg_sentinels(&project_path);
    let before = m4dseg_sentinel_snapshot(&sentinels);

    write_session_file_for_state(&project_path, &mut state).unwrap();
    assert_eq!(m4dseg_sentinel_snapshot(&sentinels), before);
    let restored =
        open_state_from_session(&read_session_file(&project_path).unwrap(), None).unwrap();
    assert_eq!(restored.dataset_path, state.dataset_path);
    assert_eq!(m4dseg_sentinel_snapshot(&sentinels), before);

    write_autosave_snapshot_for_state(&project_path, &state).unwrap();
    assert_eq!(m4dseg_sentinel_snapshot(&sentinels), before);
    let recovery = read_autosave_snapshot(&project_path).unwrap();
    let recovered = open_state_from_recovery_session(&recovery, None).unwrap();
    assert_eq!(recovered.dataset_path, state.dataset_path);
    assert_eq!(m4dseg_sentinel_snapshot(&sentinels), before);
}

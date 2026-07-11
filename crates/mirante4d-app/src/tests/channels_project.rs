use crate::smoke::render_playback_steps_for_smoke;
use crate::viewer_layout::PanelId;

fn active_layer_histogram_for_test(
    application: &ApplicationState,
    analysis: &mut current_runtime::analysis::CurrentAnalysisRuntime,
    dataset: &current_runtime::dataset::CurrentDatasetRuntime,
) -> LayerHistogramSummary {
    let snapshot = application.snapshot();
    let view = application_view(&snapshot);
    let layer = snapshot
        .catalog()
        .layer(view.active_layer())
        .expect("the canonical view must close over the test catalog");
    let layer_id = current_physical_layer_id(dataset, view.active_layer())
        .expect("the test catalog must map to the current physical dataset");
    active_layer_histogram_summary(
        analysis,
        dataset,
        histogram::ActiveLayerHistogramInput {
            layer_id: &layer_id,
            layer_name: layer.label(),
            dtype: layer.dtype(),
            timepoint: view.timepoint(),
        },
    )
}

#[test]
fn workbench_shell_exposes_channel_display_controls() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(root).unwrap();

    let harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 720.0))
        .with_pixels_per_point(1.0)
        .build_eframe(|cc| test_workbench_app_for_ui_harness(cc, opened));

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
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let application = test_application_for_opened_source(&opened);
    let dense_max = opened.analysis_runtime.active_intensity_summary.max as f32;

    let histogram = active_layer_histogram_for_test(
        &application,
        &mut opened.analysis_runtime,
        &opened.dataset_runtime,
    );
    let window = auto_dense_window_from_histogram(&histogram).unwrap();

    assert_eq!(histogram.status, HistogramStatus::Exact);
    assert_eq!(histogram.bin_count, 32);
    assert_eq!(histogram.sample_count, 16 * 16 * 16);
    assert_eq!(histogram.min_value, 0.0);
    assert_eq!(histogram.max_value, dense_max);
    assert_eq!(histogram.bins.iter().sum::<u64>(), histogram.sample_count);
    assert!(window.low() >= histogram.min_value);
    assert!(window.high() <= histogram.max_value);
    assert!(window.high() > window.low());
}

#[test]
fn active_layer_histogram_uses_data_engine_sample_when_no_dense_or_resident_data() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let application = test_application_for_opened_source(&opened);
    let dense_max = opened.analysis_runtime.active_intensity_summary.max as f32;
    opened.dataset_runtime.active_volume_u8 = None;
    opened.dataset_runtime.active_volume = None;
    opened.dataset_runtime.active_volume_f32 = None;
    opened.dataset_runtime.resident_bricks_u8.clear();
    opened.dataset_runtime.resident_bricks_u8_by_layer.clear();
    opened.dataset_runtime.resident_bricks.clear();
    opened.dataset_runtime.resident_bricks_by_layer.clear();
    opened.dataset_runtime.resident_bricks_f32.clear();
    opened.dataset_runtime.resident_bricks_f32_by_layer.clear();

    let histogram = active_layer_histogram_for_test(
        &application,
        &mut opened.analysis_runtime,
        &opened.dataset_runtime,
    );
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
    assert!(window.low() >= histogram.min_value);
    assert!(window.high() <= histogram.max_value);
    assert!(window.high() > window.low());
}

#[test]
fn active_layer_histogram_caches_data_engine_sample() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let application = test_application_for_opened_source(&opened);
    opened.dataset_runtime.active_volume_u8 = None;
    opened.dataset_runtime.active_volume = None;
    opened.dataset_runtime.active_volume_f32 = None;
    opened.dataset_runtime.resident_bricks_u8.clear();
    opened.dataset_runtime.resident_bricks_u8_by_layer.clear();
    opened.dataset_runtime.resident_bricks.clear();
    opened.dataset_runtime.resident_bricks_by_layer.clear();
    opened.dataset_runtime.resident_bricks_f32.clear();
    opened.dataset_runtime.resident_bricks_f32_by_layer.clear();

    let first = active_layer_histogram_for_test(
        &application,
        &mut opened.analysis_runtime,
        &opened.dataset_runtime,
    );
    let stats_after_first = opened.dataset_runtime.dataset.stats().unwrap();
    let second = active_layer_histogram_for_test(
        &application,
        &mut opened.analysis_runtime,
        &opened.dataset_runtime,
    );
    let stats_after_second = opened.dataset_runtime.dataset.stats().unwrap();

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
    let root = write_fixture(FixtureKind::BasicF32_8Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let application = test_application_for_opened_source(&opened);
    opened.dataset_runtime.active_volume_u8 = None;
    opened.dataset_runtime.active_volume = None;
    opened.dataset_runtime.active_volume_f32 = None;
    opened.dataset_runtime.resident_bricks_u8.clear();
    opened.dataset_runtime.resident_bricks_u8_by_layer.clear();
    opened.dataset_runtime.resident_bricks.clear();
    opened.dataset_runtime.resident_bricks_by_layer.clear();
    opened.dataset_runtime.resident_bricks_f32.clear();
    opened.dataset_runtime.resident_bricks_f32_by_layer.clear();

    let histogram = active_layer_histogram_for_test(
        &application,
        &mut opened.analysis_runtime,
        &opened.dataset_runtime,
    );
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
    assert!(window.low() >= histogram.min_value);
    assert!(window.high() <= histogram.max_value);
    assert!(window.high() > window.low());
}

#[test]
fn active_layer_histogram_reports_unavailable_for_data_engine_failure() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    opened.dataset_runtime.active_volume_u8 = None;
    opened.dataset_runtime.active_volume = None;
    opened.dataset_runtime.active_volume_f32 = None;
    opened.dataset_runtime.resident_bricks_u8.clear();
    opened.dataset_runtime.resident_bricks_u8_by_layer.clear();
    opened.dataset_runtime.resident_bricks.clear();
    opened.dataset_runtime.resident_bricks_by_layer.clear();
    opened.dataset_runtime.resident_bricks_f32.clear();
    opened.dataset_runtime.resident_bricks_f32_by_layer.clear();
    let missing_layer = LayerId::new("missing-layer").unwrap();

    let histogram = active_layer_histogram_summary(
        &mut opened.analysis_runtime,
        &opened.dataset_runtime,
        histogram::ActiveLayerHistogramInput {
            layer_id: &missing_layer,
            layer_name: "missing-layer",
            dtype: IntensityDType::Uint16,
            timepoint: TimeIndex::new(0),
        },
    );
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
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let dense_max = app.analysis_runtime.active_intensity_summary.max as f32;
    let pool = BrickReadPool::new(app.dataset_runtime.dataset.clone(), 1, 4).unwrap();
    let snapshot = app.application.snapshot();

    let submission = submit_visible_bricks_to_pool(
        &snapshot,
        &mut app.dataset_runtime,
        &mut app.analysis_runtime,
        &app.render_runtime,
        &pool,
    );
    assert!(submission.queued_current);
    for _ in 0..submission.current_tickets.len() {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(apply_brick_read_outcome(
            &snapshot,
            &mut app.dataset_runtime,
            &mut app.analysis_runtime,
            &app.render_runtime,
            outcome,
        ));
    }
    assert!(app.dataset_runtime.brick_stream_complete);
    app.dataset_runtime.active_volume_u8 = None;
    app.dataset_runtime.active_volume = None;
    app.dataset_runtime.active_volume_f32 = None;
    assert!(!app.analysis_runtime.resident_histogram_samples.is_empty());

    let histogram = active_layer_histogram_for_test(
        &app.application,
        &mut app.analysis_runtime,
        &app.dataset_runtime,
    );
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
    assert!(window.low() >= histogram.min_value);
    assert!(window.high() <= histogram.max_value);
    assert!(window.high() > window.low());
}

#[test]
fn resident_histogram_without_worker_samples_is_pending_not_scanned_from_ui() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let pool = BrickReadPool::new(app.dataset_runtime.dataset.clone(), 1, 4).unwrap();
    let snapshot = app.application.snapshot();

    let submission = submit_visible_bricks_to_pool(
        &snapshot,
        &mut app.dataset_runtime,
        &mut app.analysis_runtime,
        &app.render_runtime,
        &pool,
    );
    assert!(submission.queued_current);
    for _ in 0..submission.current_tickets.len() {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(apply_brick_read_outcome(
            &snapshot,
            &mut app.dataset_runtime,
            &mut app.analysis_runtime,
            &app.render_runtime,
            outcome,
        ));
    }
    assert!(app.dataset_runtime.brick_stream_complete);
    assert!(!app.dataset_runtime.resident_bricks.is_empty());
    app.dataset_runtime.active_volume_u8 = None;
    app.dataset_runtime.active_volume = None;
    app.dataset_runtime.active_volume_f32 = None;
    app.analysis_runtime.resident_histogram_samples.clear();
    app.analysis_runtime.resident_histogram_generation = app
        .analysis_runtime
        .resident_histogram_generation
        .saturating_add(1);
    app.analysis_runtime.active_histogram_cache = None;

    let histogram = active_layer_histogram_for_test(
        &app.application,
        &mut app.analysis_runtime,
        &app.dataset_runtime,
    );

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
    let root = write_fixture(FixtureKind::BasicF32_8Cube, tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let pool = BrickReadPool::new(app.dataset_runtime.dataset.clone(), 1, 32).unwrap();
    let snapshot = app.application.snapshot();

    let submission = submit_visible_bricks_to_pool(
        &snapshot,
        &mut app.dataset_runtime,
        &mut app.analysis_runtime,
        &app.render_runtime,
        &pool,
    );
    assert!(submission.queued_current);
    for _ in 0..submission.current_tickets.len() {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(apply_brick_read_outcome(
            &snapshot,
            &mut app.dataset_runtime,
            &mut app.analysis_runtime,
            &app.render_runtime,
            outcome,
        ));
    }
    assert!(app.dataset_runtime.brick_stream_complete);
    app.dataset_runtime.active_volume_u8 = None;
    app.dataset_runtime.active_volume = None;
    app.dataset_runtime.active_volume_f32 = None;

    let histogram = active_layer_histogram_for_test(
        &app.application,
        &mut app.analysis_runtime,
        &app.dataset_runtime,
    );
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
    assert!(window.low() >= histogram.min_value);
    assert!(window.high() <= histogram.max_value);
    assert!(window.high() > window.low());
}

#[test]
fn workbench_shell_exposes_histogram_and_auto_window_controls() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(root).unwrap();

    let harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 720.0))
        .with_pixels_per_point(1.0)
        .build_eframe(|cc| test_workbench_app_for_ui_harness(cc, opened));

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

    assert!(dense.low() < 20.0);
    assert!(signal.low() >= 40.0);
    assert!(signal.high() > signal.low());
    assert!(signal.high() <= histogram.max_value);
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
fn application_playback_commands_reconcile_transient_state_and_timepoint() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let ctx = egui::Context::default();

    assert_eq!(
        app.apply_application_command(ApplicationCommand::SetPlaybackActive(true), &ctx),
        Ok(CommandEffect::Changed)
    );
    let snapshot = app.application.snapshot();
    assert!(snapshot.transient().playback_active());
    assert_eq!(snapshot.transient().last_playback_tick(), None);
    assert!(app.render_runtime.playback_lod_downshift_active);
    assert_eq!(
        playback_status_label(
            snapshot.transient().playback_active(),
            application_view(&snapshot).timepoint(),
            workbench_playback_runtime::catalog_timepoint_count(&snapshot),
        ),
        "playback playing | t 1/3"
    );

    app.apply_application_command(ApplicationCommand::AdvancePlaybackTick(10), &ctx)
        .unwrap();
    assert_eq!(
        application_view(&app.application.snapshot()).timepoint(),
        TimeIndex::new(0)
    );
    for (tick, expected) in [(11, 1), (12, 2), (13, 0)] {
        app.apply_application_command(ApplicationCommand::AdvancePlaybackTick(tick), &ctx)
            .unwrap();
        assert_eq!(
            application_view(&app.application.snapshot()).timepoint(),
            TimeIndex::new(expected)
        );
    }
    let snapshot = app.application.snapshot();
    let previous = stepped_timepoint(
        application_view(&snapshot).timepoint(),
        workbench_playback_runtime::catalog_timepoint_count(&snapshot),
        -1,
    );
    app.apply_application_command(ApplicationCommand::SetTimepoint(previous), &ctx)
        .unwrap();

    app.apply_application_command(ApplicationCommand::SetPlaybackActive(false), &ctx)
        .unwrap();
    let snapshot = app.application.snapshot();
    assert!(!snapshot.transient().playback_active());
    assert_eq!(snapshot.transient().last_playback_tick(), None);
    assert!(!app.render_runtime.playback_lod_downshift_active);
    assert_eq!(
        playback_status_label(
            snapshot.transient().playback_active(),
            application_view(&snapshot).timepoint(),
            workbench_playback_runtime::catalog_timepoint_count(&snapshot),
        ),
        "playback stopped | t 3/3"
    );
}

#[test]
fn streaming_timepoint_command_dirties_cross_section_panels_without_dirtying_3d_panel() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let ctx = egui::Context::default();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    let initial_snapshot = app.application.snapshot();
    let cross_section = *application_view(&initial_snapshot).cross_section();
    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section,
        },
        &ctx,
    )
    .unwrap();
    for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::ThreeD, PanelId::Yz] {
        assert!(
            app.render_runtime
                .cross_section_runtime
                .record_panel_viewports(panel_id, presentation, render)
        );
        let generation = app
            .render_runtime
            .cross_section_runtime
            .panel(panel_id)
            .unwrap()
            .generation;
        assert!(
            app.render_runtime
                .cross_section_runtime
                .mark_panel_displayed(panel_id, generation)
        );
    }
    let generations_before =
        [PanelId::Xy, PanelId::Xz, PanelId::ThreeD, PanelId::Yz].map(|panel_id| {
            app.render_runtime
                .cross_section_runtime
                .panel(panel_id)
                .unwrap()
                .generation
        });

    app.apply_application_command(ApplicationCommand::SetTimepoint(TimeIndex::new(1)), &ctx)
        .unwrap();

    assert_eq!(
        application_view(&app.application.snapshot()).timepoint(),
        TimeIndex::new(1)
    );
    let runtime = &app.render_runtime.cross_section_runtime;
    for (panel_id, generation_before) in [PanelId::Xy, PanelId::Xz, PanelId::Yz].into_iter().zip([
        generations_before[0],
        generations_before[1],
        generations_before[3],
    ]) {
        let panel = runtime.panel(panel_id).unwrap();
        assert!(panel.generation > generation_before);
        assert!(
            !panel.display_current(),
            "{} should be dirty after a streaming timepoint change",
            panel_id.label()
        );
    }
    let three_d = runtime.panel(PanelId::ThreeD).unwrap();
    assert_eq!(three_d.generation, generations_before[2]);
    assert!(three_d.display_current());
}

#[test]
fn streaming_timepoint_switch_preserves_last_nonblank_frame_while_loading() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let ctx = egui::Context::default();
    let previous_pixels = app.render_runtime.frame.pixels().to_vec();

    assert!(previous_pixels.iter().any(|value| *value > 0));

    app.apply_application_command(ApplicationCommand::SetTimepoint(TimeIndex::new(1)), &ctx)
        .unwrap();

    assert_eq!(
        application_view(&app.application.snapshot()).timepoint(),
        TimeIndex::new(1)
    );
    assert_eq!(
        app.render_runtime.frame.pixels(),
        previous_pixels.as_slice()
    );
    assert_eq!(
        app.render_runtime.frame_fidelity.completeness,
        FrameCompleteness::Loading
    );
}

#[test]
fn playback_smoke_helper_renders_multiple_nonblank_timepoints() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let ui_runtime = current_runtime::ui::CurrentUiRuntime::new(ResourcePolicy::default(), None);

    let frames = render_playback_steps_for_smoke(
        &mut application,
        &mut opened.dataset_runtime,
        &mut opened.render_runtime,
        &mut opened.analysis_runtime,
        &ui_runtime,
        None,
        2,
        Duration::from_secs(2),
    )
    .unwrap();

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
    let opened = open_dataset_and_render_first_frame(root).unwrap();

    let harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 720.0))
        .with_pixels_per_point(1.0)
        .build_eframe(|cc| test_workbench_app_for_ui_harness(cc, opened));

    harness.get_by_label("First");
    harness.get_by_label("Prev");
    harness.get_by_label("Play");
    harness.get_by_label("Next");
    harness.get_by_label("Last");
    harness.get_by_label("playback stopped | t 1/3");
}

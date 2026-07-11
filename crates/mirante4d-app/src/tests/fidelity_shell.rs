#[test]
fn frame_fidelity_label_names_currently_shown_lod() {
    let mut fidelity = FrameFidelityStatus::new_with_presentation(
        RenderViewport::new(1920, 1080).unwrap(),
        crate::viewport::default_presentation_viewport(),
    );
    fidelity.displayed_scale_level = Some(2);
    fidelity.target_scale_level = 0;
    fidelity.completeness = FrameCompleteness::BudgetLimited;
    fidelity.reason = LodDecisionReason::GpuBudgetLimited;
    fidelity.backend = RenderBackend::GpuResidentBricks;
    fidelity.display_freshness = DisplayedFrameFreshness::Current;
    fidelity.frame_time_ms = Some(12.5);

    let label = frame_fidelity_label(&fidelity);

    assert!(label.contains("shown s2 / target s0"));
    assert!(label.contains("budget-limited"));
    assert!(label.contains("GPU budget"));
    assert!(label.contains("GPU bricks"));
    assert!(label.contains("1920x1080"));
    assert!(label.contains("display current"));
    assert!(label.contains("render 12.5 ms"));
    assert!(!label.contains("FPS"));
}

#[test]
fn frame_fidelity_label_keeps_exact_source_lod_concise() {
    let mut fidelity = FrameFidelityStatus::new_with_presentation(
        RenderViewport::new(998, 1024).unwrap(),
        crate::viewport::default_presentation_viewport(),
    );
    fidelity.displayed_scale_level = Some(0);
    fidelity.target_scale_level = 0;
    fidelity.completeness = FrameCompleteness::Exact;
    fidelity.reason = LodDecisionReason::ExactS0;
    fidelity.backend = RenderBackend::GpuResidentBricks;

    assert_eq!(
        frame_fidelity_label(&fidelity),
        "shown s0 exact | GPU bricks | 998x1024 px; 512x512 pt | render pending"
    );
}

#[test]
fn frame_fidelity_labels_cover_status_reason_and_failure_vocabularies() {
    for (value, expected) in [
        (FrameCompleteness::Exact, "exact"),
        (FrameCompleteness::Complete, "complete"),
        (FrameCompleteness::Loading, "loading"),
        (FrameCompleteness::Incomplete, "incomplete"),
        (FrameCompleteness::BudgetLimited, "budget-limited"),
    ] {
        assert_eq!(frame_completeness_label(value), expected);
    }
    for (value, expected) in [
        (LodDecisionReason::ExactS0, "exact s0"),
        (
            LodDecisionReason::ScreenEquivalentCoarserScale,
            "screen-equivalent LOD",
        ),
        (LodDecisionReason::PlaybackDownshift, "playback LOD"),
        (LodDecisionReason::LoadingTargetScale, "loading target LOD"),
        (LodDecisionReason::FrameBudgetLimited, "frame budget"),
        (LodDecisionReason::GpuBudgetLimited, "GPU budget"),
        (LodDecisionReason::CpuBudgetLimited, "CPU budget"),
        (LodDecisionReason::BackendLimit, "backend limit"),
        (LodDecisionReason::AllocationFailed, "allocation failed"),
        (
            LodDecisionReason::IncompleteResidency,
            "incomplete residency",
        ),
        (
            LodDecisionReason::InvalidModeParameter,
            "invalid mode parameter",
        ),
        (LodDecisionReason::UnsupportedDtype, "unsupported dtype"),
        (LodDecisionReason::InvalidTransform, "invalid transform"),
    ] {
        assert_eq!(frame_reason_label(value), expected);
    }
    for (value, expected) in [
        (FrameFailureKind::BudgetExceeded, "budget exceeded"),
        (FrameFailureKind::BackendLimit, "backend limit"),
        (FrameFailureKind::AllocationFailed, "allocation failed"),
        (
            FrameFailureKind::IncompleteResidency,
            "incomplete residency",
        ),
        (
            FrameFailureKind::InvalidModeParameter,
            "invalid mode parameter",
        ),
        (FrameFailureKind::UnsupportedDtype, "unsupported dtype"),
        (FrameFailureKind::InvalidTransform, "invalid transform"),
    ] {
        assert_eq!(frame_failure_kind_label(value), expected);
    }
}

#[test]
fn f32_iso_display_conversion_preserves_surface_payload() {
    let window = DisplayWindow::new(0.0, 4.0).unwrap();
    let surface = IsoSurfaceFrameF32::try_new(
        1,
        1,
        vec![2.0],
        vec![0.25],
        vec![0.75],
        vec![3.0],
        vec![IsoSurfaceNormal::ZERO],
        vec![123],
        vec![45],
        PixelCoverage::All,
    )
    .unwrap();
    let frame =
        MipImageF32::try_new_with_iso_surface(1, 1, vec![0.5], PixelCoverage::All, Some(surface))
            .unwrap();

    let converted =
        f32_frame_to_display_u16_for_mode(&frame, RenderMode::Isosurface, window).unwrap();

    assert_eq!(converted.pixels(), &[32768]);
    let converted_surface = converted.iso_surface().unwrap();
    assert_eq!(converted_surface.source_values(), &[32768]);
    assert_eq!(converted_surface.display_scalars(), &[16384]);
    assert_eq!(converted_surface.material_scalars(), &[49151]);
    assert_eq!(converted_surface.hit_depth(), &[3.0]);
    assert_eq!(converted_surface.normals(), &[IsoSurfaceNormal::ZERO]);
    assert_eq!(converted_surface.diffuse_lighting(), &[123]);
    assert_eq!(converted_surface.specular_lighting(), &[45]);
    assert_eq!(converted_surface.coverage(), &PixelCoverage::All);
}

#[test]
fn f32_dvr_display_conversion_preserves_normalized_rgba() {
    let dvr_rgba = mirante4d_renderer::DvrRgbaFrame::try_new(
        2,
        1,
        vec![[0.25, 0.25, 0.25, 0.5], [0.0, 0.0, 0.0, 0.0]],
        PixelCoverage::Mask(vec![1, 0]),
    )
    .unwrap();
    let frame = MipImageF32::try_new_with_mode_frames(
        2,
        1,
        vec![0.5, 0.0],
        PixelCoverage::Mask(vec![1, 0]),
        None,
        Some(dvr_rgba.clone()),
    )
    .unwrap();

    let converted = f32_frame_to_display_u16_for_mode(
        &frame,
        RenderMode::Dvr,
        DisplayWindow::new(97.0, 111.0).unwrap(),
    )
    .unwrap();

    assert_eq!(converted.pixels(), &[32768, 0]);
    assert_eq!(converted.coverage(), &PixelCoverage::Mask(vec![1, 0]));
    assert_eq!(converted.dvr_rgba(), Some(&dvr_rgba));
}

#[test]
fn iso_placeholder_frame_carries_empty_surface_payload() {
    let frame =
        placeholder_frame_for_mode(RenderViewport::new(2, 1).unwrap(), RenderMode::Isosurface);

    assert_eq!(frame.pixels(), &[0, 0]);
    assert_eq!(frame.coverage(), &PixelCoverage::Mask(vec![0, 0]));
    let surface = frame.iso_surface().unwrap();
    assert_eq!(surface.source_values(), &[0, 0]);
    assert_eq!(surface.coverage(), &PixelCoverage::Mask(vec![0, 0]));
}

#[test]
fn display_size_maps_to_physical_render_viewport_and_clamps_aspect() {
    assert_eq!(
        render_viewport_for_display_size(egui::vec2(640.2, 360.2), 2.0, 2048).unwrap(),
        RenderViewport::new(1280, 720).unwrap()
    );
    assert_eq!(
        render_viewport_for_display_size(egui::vec2(1000.0, 2000.0), 2.0, 2048).unwrap(),
        RenderViewport::new(1024, 2048).unwrap()
    );
    assert!(render_viewport_for_display_size(egui::Vec2::ZERO, 2.0, 2048).is_none());
    assert!(render_viewport_for_display_size(egui::vec2(640.0, 360.0), 0.0, 2048).is_none());
    assert!(render_viewport_for_display_size(egui::vec2(640.0, 360.0), 2.0, 0).is_none());
}

#[test]
fn app_renders_to_explicit_viewport_not_volume_xy_size() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let application = test_application_for_opened_source(&opened);
    let ui_runtime = current_runtime::ui::CurrentUiRuntime::new(ResourcePolicy::default(), None);
    let active_shape = opened
        .catalog
        .layer(opened.workspace.view().active_layer())
        .unwrap()
        .shape()
        .spatial();

    assert_eq!(
        crate::viewport::default_render_viewport_for_shape(active_shape).unwrap(),
        RenderViewport::new(512, 512).unwrap()
    );
    assert_eq!(
        opened.render_runtime.frame.width,
        TEST_INITIAL_RENDER_VIEWPORT_SIDE
    );
    assert!(set_render_viewport(
        &mut opened.render_runtime,
        RenderViewport::new(80, 45).unwrap()
    ));
    rerender_state_with_backend(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &mut opened.render_runtime,
        None,
    )
    .unwrap();

    assert_eq!(opened.render_runtime.frame.width, 80);
    assert_eq!(opened.render_runtime.frame.height, 45);
    assert_eq!(opened.render_runtime.diagnostics.output_pixels, 80 * 45);
}

#[test]
fn workbench_shell_exposes_primary_regions_at_high_dpi() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(root).unwrap();

    let harness = Harness::builder()
        .with_size(egui::vec2(1440.0, 900.0))
        .with_pixels_per_point(2.0)
        .build_eframe(|cc| test_workbench_app_for_ui_harness(cc, opened));

    for label in [
        "Mirante4D",
        "Dataset",
        "Layers",
        "Inspector",
        "Viewer Tools",
        "Analysis",
        "Workspace",
        "Runtime Diagnostics",
        "Fit Data",
        "Reset View",
        "ready",
    ] {
        harness.get_by_label(label);
    }
}

#[test]
fn workbench_runtime_diagnostics_exposes_data_budgets_and_cross_sections() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    app.dataset_runtime.brick_read_pool =
        Some(BrickReadPool::new(app.dataset_runtime.dataset.clone(), 3, 7).unwrap());

    let harness = Harness::builder()
        .with_size(egui::vec2(1440.0, 900.0))
        .with_pixels_per_point(1.0)
        .build_ui(|ui| {
            ui_kit::configure_visuals(ui.ctx());
            app.show_runtime_diagnostics_body(ui);
        });

    for label in [
        "volume cache budget",
        "brick cache budget",
        "upload staging budget",
        "decoded in-flight budget",
        "payload bytes",
        "brick workers",
        "brick queue capacity",
        "brick queue depth",
        "2D global runtime",
        "2D chunk states",
    ] {
        harness.get_by_label(label);
    }
}

#[test]
fn tiff_import_setup_state_is_visible_immediately_after_output_selection() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let source = tempdir.path().join("raw.tif");
    let output_parent = tempdir.path().join("output");
    fs::create_dir(&output_parent).unwrap();
    let (_sender, receiver) = mpsc::channel();
    let mut app = test_workbench_app_without_background_runtime(opened);

    app.enter_tiff_import_setup_waiting_state(
        TiffImportSource::SingleFile(source.clone()),
        output_parent.clone(),
        receiver,
    );

    let task = app.import_runtime.tiff_import_setup_task.as_ref().unwrap();
    assert_eq!(task.source.path(), source.as_path());
    assert_eq!(task.output_parent, output_parent);
    assert!(app.import_runtime.pending_tiff_import.is_none());
    assert!(app.import_runtime.tiff_import_setup_error.is_none());
}

use mirante4d_render_api::RenderExtent;

#[test]
fn frame_fidelity_label_names_currently_shown_lod() {
    let mut fidelity = FrameFidelityStatus::new_with_presentation(
        RenderExtent::new(1920, 1080).unwrap(),
        crate::viewport::default_presentation_viewport(),
    );
    fidelity.displayed_scale_level = Some(2);
    fidelity.target_scale_level = 0;
    fidelity.completeness = FrameCompleteness::BudgetLimited;
    fidelity.reason = LodDecisionReason::GpuBudgetLimited;
    fidelity.backend = RenderBackend::GpuCameraMip;
    fidelity.display_freshness = DisplayedFrameFreshness::Current;
    fidelity.frame_time_ms = Some(12.5);

    let label = frame_fidelity_label(&fidelity);

    assert!(label.contains("shown s2 / target s0"));
    assert!(label.contains("budget-limited"));
    assert!(label.contains("GPU budget"));
    assert!(label.contains("GPU MIP"));
    assert!(label.contains("1920x1080"));
    assert!(label.contains("display current"));
    assert!(label.contains("render 12.5 ms"));
    assert!(!label.contains("FPS"));
}

#[test]
fn frame_fidelity_label_keeps_exact_source_lod_concise() {
    let mut fidelity = FrameFidelityStatus::new_with_presentation(
        RenderExtent::new(998, 1024).unwrap(),
        crate::viewport::default_presentation_viewport(),
    );
    fidelity.displayed_scale_level = Some(0);
    fidelity.target_scale_level = 0;
    fidelity.completeness = FrameCompleteness::Exact;
    fidelity.reason = LodDecisionReason::ExactS0;
    fidelity.backend = RenderBackend::GpuCameraMip;

    assert_eq!(
        frame_fidelity_label(&fidelity),
        "shown s0 exact | GPU MIP | 998x1024 px; 512x512 pt | render pending"
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
        (LodDecisionReason::NoVisibleData, "outside selected data"),
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
    assert_eq!(
        crate::fidelity::render_backend_label(RenderBackend::Empty),
        "empty"
    );
}

#[test]
fn display_size_maps_to_physical_render_viewport_and_clamps_aspect() {
    assert_eq!(
        render_viewport_for_display_size(egui::vec2(640.2, 360.2), 2.0, 2048).unwrap(),
        RenderExtent::new(1280, 720).unwrap()
    );
    assert_eq!(
        render_viewport_for_display_size(egui::vec2(1000.0, 2000.0), 2.0, 2048).unwrap(),
        RenderExtent::new(1024, 2048).unwrap()
    );
    assert!(render_viewport_for_display_size(egui::Vec2::ZERO, 2.0, 2048).is_none());
    assert!(render_viewport_for_display_size(egui::vec2(640.0, 360.0), 0.0, 2048).is_none());
    assert!(render_viewport_for_display_size(egui::vec2(640.0, 360.0), 2.0, 0).is_none());
}

#[test]
fn workbench_shell_exposes_primary_regions_at_high_dpi() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_target_fixture(tempdir.path()).unwrap();
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
fn workbench_runtime_diagnostics_exposes_unified_runtime_bounds_and_leases() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_target_fixture(tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let app = test_workbench_app_without_background_runtime(opened);

    let harness = Harness::builder()
        .with_size(egui::vec2(1440.0, 900.0))
        .with_pixels_per_point(1.0)
        .build_ui(|ui| {
            ui_kit::configure_visuals(ui.ctx());
            app.show_runtime_diagnostics_body(ui);
        });

    for label in [
        "dataset CPU",
        "decoded residency",
        "upload staging",
        "in-flight decode",
        "metadata/indexes",
        "queues/results",
        "requests",
        "queue bounds",
        "resident resources",
        "renderer leases",
        "active 2D panel",
    ] {
        harness.get_by_label(label);
    }
}

#[test]
fn ui_snapshot_projects_visible_surfaces_without_native_handles() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_target_fixture(tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let app = test_workbench_app_without_background_runtime(opened);

    let snapshot = app.application_snapshot_for_ui();
    let three_d = snapshot
        .presentations()
        .get(PresentationSlot::ThreeD)
        .unwrap();
    assert_eq!(
        three_d.viewport(),
        app.render_runtime.presentation_viewport
    );
    assert_eq!(three_d.frame(), None);
    for slot in [
        PresentationSlot::Xy,
        PresentationSlot::Xz,
        PresentationSlot::Yz,
    ] {
        assert!(snapshot.presentations().get(slot).is_none());
    }
}

#[test]
fn tiff_import_setup_state_is_visible_immediately_after_output_selection() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_target_fixture(tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let source = tempdir.path().join("raw.tif");
    let output_parent = tempdir.path().join("output");
    fs::create_dir(&output_parent).unwrap();
    let (_sender, receiver) = mpsc::channel();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let tiff_source = TiffSource::auto(source.clone());
    let destination = tiff_destination(&tiff_source, &output_parent);

    app.enter_tiff_import_setup_waiting_state(
        tiff_source,
        destination.clone(),
        ImportCancellation::new(),
        receiver,
        None,
    );

    let task = app.import_runtime.tiff_import_setup_task.as_ref().unwrap();
    assert_eq!(task.source.path, source);
    assert_eq!(task.destination, destination);
    assert!(app.import_runtime.pending_tiff_import.is_none());
    assert!(app.import_runtime.tiff_import_setup_error.is_none());
}

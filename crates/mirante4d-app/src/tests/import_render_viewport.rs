#[test]
fn import_options_create_strict_native_output_package_path() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("Synthetic source");
    let output = tempdir.path().join("out");

    let options = import_tiff_source_options(TiffImportSource::Directory(input), &output).unwrap();

    assert_eq!(options.dataset_id, "synthetic-source");
    assert_eq!(options.dataset_name, "Synthetic source");
    assert_eq!(
        options.output_package,
        output.join("synthetic-source.m4d")
    );
    assert_eq!(options.voxel_spacing_um, [1.0, 1.0, 1.0]);
    assert_eq!(options.existing_policy, ExistingPackagePolicy::Fail);
}

#[test]
fn import_options_use_single_tiff_file_stem_for_output_package_path() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("Synthetic source.tif");
    let output = tempdir.path().join("out");

    let options = import_tiff_source_options(TiffImportSource::SingleFile(input), &output).unwrap();

    assert_eq!(options.dataset_id, "synthetic-source");
    assert_eq!(options.dataset_name, "Synthetic source");
    assert_eq!(
        options.output_package,
        output.join("synthetic-source.m4d")
    );
}

#[test]
fn prepare_tiff_import_reports_inspection_errors() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("empty-input");
    let output = tempdir.path().join("out");
    fs::create_dir(&input).unwrap();

    let err = prepare_tiff_source_import(TiffImportSource::Directory(input), &output).unwrap_err();

    assert!(err.to_string().contains("contains no .tif/.tiff files"));
}

#[test]
fn prepare_tiff_import_prefills_ome_voxel_spacing_without_confirming_review() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("ome-source.tif");
    let output = tempdir.path().join("out");
    write_app_ome_stack(&input).unwrap();

    let (options, inspection) =
        prepare_tiff_source_import(TiffImportSource::SingleFile(input), &output).unwrap();
    let pending = PendingTiffImport {
        options: options.clone(),
        inspection,
        voxel_spacing_confirmed: false,
        grouping_confirmed: true,
    };

    assert_eq!(options.voxel_spacing_um, [0.25, 0.5, 0.75]);
    assert!(!pending_tiff_import_ready_to_start(&pending));
    assert!(
        validate_pending_tiff_import(&pending)
            .unwrap_err()
            .to_string()
            .contains("voxel spacing must be reviewed")
    );
}

#[test]
fn import_options_validation_requires_positive_finite_spacing_and_name() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut options = import_tiff_source_options(
        TiffImportSource::Directory(tempdir.path().join("Synthetic source")),
        &tempdir.path().join("out"),
    )
    .unwrap();

    assert!(validate_tiff_import_options(&options).is_ok());

    options.voxel_spacing_um[0] = 0.0;
    assert!(
        validate_tiff_import_options(&options)
            .unwrap_err()
            .to_string()
            .contains("voxel spacing x")
    );

    options.voxel_spacing_um[0] = f64::INFINITY;
    assert!(
        validate_tiff_import_options(&options)
            .unwrap_err()
            .to_string()
            .contains("voxel spacing x")
    );

    options.voxel_spacing_um[0] = 1.0;
    options.dataset_name = "   ".to_owned();
    assert!(
        validate_tiff_import_options(&options)
            .unwrap_err()
            .to_string()
            .contains("dataset name")
    );

    options.dataset_name = "Synthetic source".to_owned();
    options
        .channel_metadata
        .insert(0, default_tiff_channel_metadata_override(0));
    options.channel_metadata.get_mut(&0).unwrap().name = " ".to_owned();
    assert!(
        validate_tiff_import_options(&options)
            .unwrap_err()
            .to_string()
            .contains("channel 0 name")
    );

    options.channel_metadata.get_mut(&0).unwrap().name = "channel".to_owned();
    options.channel_metadata.get_mut(&0).unwrap().color_rgba[1] = f32::NAN;
    assert!(
        validate_tiff_import_options(&options)
            .unwrap_err()
            .to_string()
            .contains("channel 0 color")
    );
}

#[test]
fn pending_tiff_import_requires_reviewed_voxel_spacing() {
    let tempdir = tempfile::tempdir().unwrap();
    let source = tempdir.path().join("source.tif");
    write_app_ome_stack(&source).unwrap();
    let options = import_tiff_source_options(
        TiffImportSource::SingleFile(source),
        &tempdir.path().join("out"),
    )
    .unwrap();
    let inspection = TiffDirectoryInspection {
        input_dir: options.source.path().to_path_buf(),
        source_profile: TiffSourceProfile::StackSeriesMovie,
        file_count: 1,
        channel_count: 1,
        timepoint_count: 1,
        shape: TiffStackShape { z: 1, y: 2, x: 3 },
        source_dtype: IntensityDType::Uint16,
        source_metadata: Default::default(),
        metadata_confidence: TiffMetadataConfidence::MissingSpatialCalibration,
        value_range: TiffValueRangeSummary { min: 0.0, max: 5.0 },
        files: Vec::new(),
        channels: vec![TiffChannelInspection {
            channel: 0,
            timepoint_count: 1,
        }],
    };
    let mut pending = PendingTiffImport {
        options,
        inspection,
        voxel_spacing_confirmed: false,
        grouping_confirmed: true,
    };

    let err = validate_pending_tiff_import(&pending).unwrap_err();
    assert!(err.to_string().contains("voxel spacing must be reviewed"));

    pending.voxel_spacing_confirmed = true;
    pending.grouping_confirmed = false;
    let err = validate_pending_tiff_import(&pending).unwrap_err();
    assert!(err.to_string().contains("source layout must be reviewed"));

    pending.grouping_confirmed = true;
    assert!(validate_pending_tiff_import(&pending).is_ok());
}

#[test]
fn pending_tiff_import_no_data_policy_is_explicit_uint8_review() {
    let tempdir = tempfile::tempdir().unwrap();
    let options = import_tiff_source_options(
        TiffImportSource::SingleFile(tempdir.path().join("source-u8.tif")),
        &tempdir.path().join("out"),
    )
    .unwrap();
    let inspection = TiffDirectoryInspection {
        input_dir: options.source.path().to_path_buf(),
        source_profile: TiffSourceProfile::StackSeriesMovie,
        file_count: 1,
        channel_count: 1,
        timepoint_count: 1,
        shape: TiffStackShape { z: 3, y: 2, x: 3 },
        source_dtype: IntensityDType::Uint8,
        source_metadata: Default::default(),
        metadata_confidence: TiffMetadataConfidence::MissingSpatialCalibration,
        value_range: TiffValueRangeSummary {
            min: 0.0,
            max: 255.0,
        },
        files: Vec::new(),
        channels: vec![TiffChannelInspection {
            channel: 0,
            timepoint_count: 1,
        }],
    };
    let mut pending = PendingTiffImport {
        options,
        inspection,
        voxel_spacing_confirmed: true,
        grouping_confirmed: true,
    };

    set_pending_tiff_no_data_policy(&mut pending, true);

    assert_eq!(
        pending.options.reviewed_plan.no_data_policy,
        Some(TiffNoDataPolicyReview {
            source_dtype: IntensityDType::Uint8,
            source_value_uint8: 255,
        })
    );
    let accepted_plan = accepted_reviewed_plan_for_pending_tiff_import(&pending);
    assert_eq!(
        accepted_plan.review_status,
        TiffImportReviewStatus::Accepted
    );
    assert_eq!(
        accepted_plan.no_data_policy,
        Some(TiffNoDataPolicyReview {
            source_dtype: IntensityDType::Uint8,
            source_value_uint8: 255,
        })
    );

    pending.inspection.source_dtype = IntensityDType::Uint16;
    normalize_pending_tiff_no_data_policy(&mut pending);

    assert_eq!(pending.options.reviewed_plan.no_data_policy, None);
}

#[test]
fn no_data_policy_label_reports_value_dtype_and_dilation() {
    let label = no_data_policy_label(&NoDataPolicy {
        kind: NoDataPolicyKind::SentinelValue,
        source_value: 255.0,
        source_dtype: IntensityDType::Uint8,
        visibility_policy: NoDataVisibilityPolicy::InvisibleWith1VoxelInvalidDilation,
    });

    assert_eq!(label, "value 255 (uint8), dilated 1 voxel");
}

#[test]
fn background_work_repaint_request_wakes_ui_immediately() {
    let ctx = egui::Context::default();

    let output = ctx.run_ui(egui::RawInput::default(), |ui| {
        request_background_work_repaint(ui.ctx());
    });

    let root_output = output.viewport_output.get(&egui::ViewportId::ROOT).unwrap();
    assert_eq!(root_output.repaint_delay, Duration::ZERO);
}

#[test]
fn background_thread_repaint_request_wakes_ui_immediately() {
    let ctx = egui::Context::default();
    let repaint_ctx = ctx.clone();

    thread::spawn(move || {
        request_background_work_repaint(&repaint_ctx);
    })
    .join()
    .unwrap();
    let output = ctx.run_ui(egui::RawInput::default(), |_ui| {});

    let root_output = output.viewport_output.get(&egui::ViewportId::ROOT).unwrap();
    assert_eq!(root_output.repaint_delay, Duration::ZERO);
}

#[test]
fn import_progress_messages_are_stable_for_ui_status() {
    let output = PathBuf::from("/tmp/imported.m4d");

    assert_eq!(
        import_progress_message(&ImportProgressEvent::DiscoveredInput { file_count: 8 }),
        "Discovered 8 TIFF file(s)"
    );
    assert_eq!(
        import_progress_message(&ImportProgressEvent::EstimatedStorage {
            estimate: TiffImportStorageEstimate {
                source_payload_bytes: 96,
                derived_multiscale_payload_bytes: 0,
                estimated_metadata_bytes: 1_179_648,
                estimated_total_bytes: 1_179_744,
                peak_working_stack_bytes: 24,
            },
        }),
        "Estimated native package size 1.13 MiB"
    );
    assert_eq!(
        import_progress_message(&ImportProgressEvent::ReadStack {
            completed: 3,
            total: 8,
            path: PathBuf::from("/tmp/ch0.tif"),
        }),
        "Reading TIFF stack 3/8"
    );
    assert_eq!(
        import_progress_message(&ImportProgressEvent::BuiltScale {
            channel: 2,
            level: 1,
        }),
        "Built channel 2 scale s1"
    );
    assert_eq!(
        import_progress_message(&ImportProgressEvent::WritingPackage {
            output_package: output.clone(),
        }),
        "Writing /tmp/imported.m4d"
    );
    assert_eq!(
        import_progress_message(&ImportProgressEvent::Finished {
            output_package: output,
        }),
        "Finished /tmp/imported.m4d"
    );
}

#[test]
fn import_progress_fraction_is_monotonic_by_phase() {
    let discovered = import_progress_fraction(Some(&ImportProgressEvent::DiscoveredInput {
        file_count: 4,
    }))
    .unwrap();
    let estimated = import_progress_fraction(Some(&ImportProgressEvent::EstimatedStorage {
        estimate: TiffImportStorageEstimate {
            source_payload_bytes: 96,
            derived_multiscale_payload_bytes: 0,
            estimated_metadata_bytes: 1_179_648,
            estimated_total_bytes: 1_179_744,
            peak_working_stack_bytes: 24,
        },
    }))
    .unwrap();
    let read_half = import_progress_fraction(Some(&ImportProgressEvent::ReadStack {
        completed: 2,
        total: 4,
        path: PathBuf::from("/tmp/ch0.tif"),
    }))
    .unwrap();
    let built = import_progress_fraction(Some(&ImportProgressEvent::BuiltScale {
        channel: 0,
        level: 2,
    }))
    .unwrap();
    let writing = import_progress_fraction(Some(&ImportProgressEvent::WritingPackage {
        output_package: PathBuf::from("/tmp/imported.m4d"),
    }))
    .unwrap();
    let finished = import_progress_fraction(Some(&ImportProgressEvent::Finished {
        output_package: PathBuf::from("/tmp/imported.m4d"),
    }))
    .unwrap();

    assert!(discovered < read_half);
    assert!(discovered < estimated);
    assert!(estimated < read_half);
    assert!(read_half < built);
    assert!(built < writing);
    assert!(writing < finished);
    assert_eq!(finished, 1.0);
}

fn set_test_cross_section_layout(application: &mut ApplicationState, scale: Option<f64>) {
    let snapshot = application.snapshot();
    let current = *application_view(&snapshot).cross_section();
    let cross_section = CrossSectionView::new(
        current.center_world(),
        current.orientation(),
        scale.unwrap_or_else(|| current.scale_world_per_screen_point()),
        current.depth_world(),
    )
    .unwrap();
    application
        .dispatch(ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section,
        })
        .unwrap();
}

fn set_test_active_cross_section_panel(application: &mut ApplicationState, panel_id: PanelId) {
    let panel_id = match panel_id {
        PanelId::Xy => mirante4d_application::CrossSectionPanelId::Xy,
        PanelId::Xz => mirante4d_application::CrossSectionPanelId::Xz,
        PanelId::Yz => mirante4d_application::CrossSectionPanelId::Yz,
        PanelId::ThreeD => panic!("the 3D panel is not a cross-section panel"),
    };
    application
        .dispatch(ApplicationCommand::SetActiveCrossSectionPanel(Some(
            panel_id,
        )))
        .unwrap();
}

fn test_cross_section_layer(
    snapshot: &ApplicationSnapshot,
    dataset: &current_runtime::dataset::CurrentDatasetRuntime,
) -> (LayerId, IntensityDType) {
    let view = application_view(snapshot);
    let layer_id = current_physical_layer_id(dataset, view.active_layer()).unwrap();
    let dtype = snapshot
        .catalog()
        .layer(view.active_layer())
        .expect("the active application layer must exist in the catalog")
        .dtype();
    (layer_id, dtype)
}

fn schedule_test_cross_section_panel(
    snapshot: &ApplicationSnapshot,
    dataset: &current_runtime::dataset::CurrentDatasetRuntime,
    render: &mut current_runtime::render::CurrentRenderRuntime,
    panel_id: PanelId,
    gpu_display_available: bool,
) -> anyhow::Result<crate::cross_section_scheduler::CrossSectionPanelSchedulePlan> {
    let view = application_view(snapshot);
    let (active_layer_id, dtype) = test_cross_section_layer(snapshot, dataset);
    let layers = [crate::cross_section_runtime::CrossSectionLayerInput {
        id: &active_layer_id,
        dtype,
    }];
    crate::cross_section_scheduler::schedule_cross_section_panel(
        dataset,
        render,
        crate::cross_section_scheduler::CrossSectionScheduleInput {
            view,
            active_layer_id: &active_layer_id,
            layers: &layers,
            active_panel: snapshot.transient().active_cross_section_panel(),
            gpu_budget_bytes: snapshot.resource_policy().gpu_budget_bytes(),
        },
        panel_id,
        gpu_display_available,
    )
}

fn submit_test_cross_section_panel_bricks(
    snapshot: &ApplicationSnapshot,
    dataset: &current_runtime::dataset::CurrentDatasetRuntime,
    render: &mut current_runtime::render::CurrentRenderRuntime,
    panel_id: PanelId,
    pool: &BrickReadPool,
) -> anyhow::Result<crate::cross_section_streaming::CrossSectionBrickSubmissionResult> {
    let view = application_view(snapshot);
    let (active_layer_id, dtype) = test_cross_section_layer(snapshot, dataset);
    let layers = [crate::cross_section_runtime::CrossSectionLayerInput {
        id: &active_layer_id,
        dtype,
    }];
    crate::cross_section_streaming::submit_cross_section_panel_bricks_to_pool(
        dataset,
        render,
        crate::cross_section_streaming::CrossSectionStreamingInput {
            view,
            active_layer_id: &active_layer_id,
            layers: &layers,
            active_panel: snapshot.transient().active_cross_section_panel(),
            gpu_budget_bytes: snapshot.resource_policy().gpu_budget_bytes(),
        },
        panel_id,
        pool,
    )
}

fn submit_test_cross_section_visible_chunks(
    snapshot: &ApplicationSnapshot,
    dataset: &current_runtime::dataset::CurrentDatasetRuntime,
    render: &mut current_runtime::render::CurrentRenderRuntime,
    pool: &BrickReadPool,
) -> anyhow::Result<crate::cross_section_streaming::CrossSectionBrickSubmissionResult> {
    let view = application_view(snapshot);
    let (active_layer_id, dtype) = test_cross_section_layer(snapshot, dataset);
    let layers = [crate::cross_section_runtime::CrossSectionLayerInput {
        id: &active_layer_id,
        dtype,
    }];
    crate::cross_section_streaming::submit_cross_section_visible_chunks_to_pool(
        dataset,
        render,
        crate::cross_section_streaming::CrossSectionStreamingInput {
            view,
            active_layer_id: &active_layer_id,
            layers: &layers,
            active_panel: snapshot.transient().active_cross_section_panel(),
            gpu_budget_bytes: snapshot.resource_policy().gpu_budget_bytes(),
        },
        pool,
    )
}

fn submit_test_cross_section_visible_chunks_to_read_queue(
    snapshot: &ApplicationSnapshot,
    dataset: &current_runtime::dataset::CurrentDatasetRuntime,
    render: &mut current_runtime::render::CurrentRenderRuntime,
    read_queue: &impl crate::cross_section_read_queue::CrossSectionReadBackend,
) -> anyhow::Result<crate::cross_section_streaming::CrossSectionBrickSubmissionResult> {
    let view = application_view(snapshot);
    let (active_layer_id, dtype) = test_cross_section_layer(snapshot, dataset);
    let layers = [crate::cross_section_runtime::CrossSectionLayerInput {
        id: &active_layer_id,
        dtype,
    }];
    crate::cross_section_streaming::submit_cross_section_visible_chunks_to_read_queue(
        dataset,
        render,
        crate::cross_section_streaming::CrossSectionStreamingInput {
            view,
            active_layer_id: &active_layer_id,
            layers: &layers,
            active_panel: snapshot.transient().active_cross_section_panel(),
            gpu_budget_bytes: snapshot.resource_policy().gpu_budget_bytes(),
        },
        read_queue,
    )
}

fn test_cross_section_hover_readout(
    snapshot: &ApplicationSnapshot,
    dataset: &current_runtime::dataset::CurrentDatasetRuntime,
    render: &current_runtime::render::CurrentRenderRuntime,
    panel_id: PanelId,
    x_points: f64,
    y_points: f64,
    presentation: PresentationViewport,
) -> Option<crate::cross_section_readout::CrossSectionHoverReadout> {
    let view = application_view(snapshot);
    let (active_layer_id, active_layer_dtype) = test_cross_section_layer(snapshot, dataset);
    crate::cross_section_readout::cross_section_hover_readout_for_panel_point(
        dataset,
        render,
        crate::cross_section_readout::CrossSectionReadoutInput {
            view,
            active_layer_id: &active_layer_id,
            active_layer_dtype,
        },
        panel_id,
        x_points,
        y_points,
        presentation,
    )
}

#[test]
fn cross_section_panel_scheduler_records_missing_viewport_status() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);

    set_test_cross_section_layout(&mut application, None);
    let snapshot = application.snapshot();
    let plan = schedule_test_cross_section_panel(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        true,
    )
    .unwrap();

    assert_eq!(
        plan.schedule.status,
        crate::viewer_layout::CrossSectionPanelScheduleStatus::MissingViewport
    );
    let panel = opened
        .render_runtime
        .cross_section_runtime
        .panel(PanelId::Xy)
        .unwrap();
    assert_eq!(panel.cross_section_schedule, Some(plan.schedule));
    assert_eq!(
        panel.cross_section_schedule.unwrap().status_label(),
        "waiting for panel size"
    );
}

#[test]
fn cross_section_scheduler_selects_lod_from_cross_section_scale() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_app_dataset(tempdir.path());
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);

    opened.dataset_runtime.brick_stream_scale_level = 0;
    set_test_cross_section_layout(&mut application, Some(2.1));
    let snapshot = application.snapshot();
    let view = application_view(&snapshot);
    let layer_id = current_physical_layer_id(&opened.dataset_runtime, view.active_layer()).unwrap();

    let target = crate::cross_section_scheduler::cross_section_target_scale(
        &opened.dataset_runtime,
        view,
        &layer_id,
    )
    .unwrap();

    assert_eq!(target, 1);
    assert_eq!(
        opened.dataset_runtime.brick_stream_scale_level, 0,
        "2D LOD selection must not mutate the current 3D stream scale"
    );
}

#[test]
fn cross_section_scheduler_biases_render_scale_until_interaction_settles() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_above_minimum_cap_app_dataset(tempdir.path());
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    opened.dataset_runtime.brick_stream_scale_level = 0;
    set_test_cross_section_layout(&mut application, Some(2.1));
    let snapshot = application.snapshot();
    let view = application_view(&snapshot);
    let layer_id = current_physical_layer_id(&opened.dataset_runtime, view.active_layer()).unwrap();
    let target_scale = crate::cross_section_scheduler::cross_section_target_scale(
        &opened.dataset_runtime,
        view,
        &layer_id,
    )
    .unwrap();
    let scale_count = opened
        .dataset_runtime
        .dataset
        .scale_count(&layer_id)
        .unwrap() as u32;
    assert!(
        target_scale + 1 < scale_count,
        "test fixture must leave one coarser scale for interaction bias"
    );
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    opened.dataset_runtime.cross_section_last_interaction_at = Some(std::time::Instant::now());

    let recent_plan = schedule_test_cross_section_panel(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        true,
    )
    .unwrap();

    assert_eq!(recent_plan.schedule.target_scale_level, Some(target_scale));
    assert_eq!(
        recent_plan.schedule.render_scale_level,
        Some(target_scale + 1)
    );
    assert_eq!(
        recent_plan.schedule.fallback_scale_level,
        Some(target_scale + 1)
    );
    assert!(!crate::cross_section_scheduler::cross_section_panel_refinement_due(
        &opened.dataset_runtime,
        &opened.render_runtime,
        PanelId::Xy
    ));
    assert_eq!(
        opened.dataset_runtime.brick_stream_scale_level, 0,
        "2D interaction LOD bias must not mutate the current 3D stream scale"
    );

    opened.dataset_runtime.cross_section_last_interaction_at = Some(
        std::time::Instant::now() - CROSS_SECTION_INTERACTION_SETTLE_DURATION
            - Duration::from_millis(1),
    );
    assert!(crate::cross_section_scheduler::cross_section_panel_refinement_due(
        &opened.dataset_runtime,
        &opened.render_runtime,
        PanelId::Xy
    ));
    assert!(
        crate::workbench_playback_runtime::background_work_active(
            &snapshot,
            &current_runtime::import::CurrentImportRuntime::idle(),
            &opened.analysis_runtime,
            &opened.dataset_runtime,
            &opened.render_runtime,
        ),
        "settled coarse cross-section panels should keep runtime work active until refinement runs"
    );

    let settled_plan = schedule_test_cross_section_panel(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        true,
    )
    .unwrap();

    assert_eq!(settled_plan.schedule.target_scale_level, Some(target_scale));
    assert_eq!(settled_plan.schedule.render_scale_level, Some(target_scale));
    assert_eq!(settled_plan.schedule.fallback_scale_level, None);
    assert!(!crate::cross_section_scheduler::cross_section_panel_refinement_due(
        &opened.dataset_runtime,
        &opened.render_runtime,
        PanelId::Xy
    ));
}

#[test]
fn cross_section_display_refresh_records_unavailable_status_without_gpu() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    let ctx = egui::Context::default();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    let snapshot = app.application.snapshot();
    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section: *application_view(&snapshot).cross_section(),
        },
        &ctx,
    )
    .unwrap();
    assert!(
        app.render_runtime
            .cross_section_runtime
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    let expected_generation = app
        .render_runtime
        .cross_section_runtime
        .panel(PanelId::Xy)
        .unwrap()
        .generation;

    let timing = app
        .render_cross_section_panel_for_display_if_needed(PanelId::Xy)
        .unwrap();

    assert!(timing.is_none());
    assert!(
        app.render_runtime
            .cross_section_gpu_display_frames
            .is_empty()
    );
    let schedule = app
        .render_runtime
        .cross_section_runtime
        .panel(PanelId::Xy)
        .unwrap()
        .cross_section_schedule
        .unwrap();
    assert_eq!(
        schedule.status,
        crate::viewer_layout::CrossSectionPanelScheduleStatus::Unavailable
    );
    assert_eq!(
        schedule.reason,
        crate::viewer_layout::CrossSectionPanelScheduleReason::GpuUnavailable
    );
    assert_eq!(schedule.generation, expected_generation);
    assert!(schedule.target_scale_level.is_some());
}

#[test]
fn cross_section_brick_submission_does_not_mutate_3d_stream_state() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();
    let visible_bricks_before = opened.render_runtime.visible_bricks.clone();
    let brick_stream_scale_before = opened.dataset_runtime.brick_stream_scale_level;
    let brick_stream_generation_before = opened.dataset_runtime.brick_stream_generation;
    let brick_stream_requested_before = opened.dataset_runtime.brick_stream_requested;

    set_test_cross_section_layout(&mut application, None);
    let snapshot = application.snapshot();
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    schedule_test_cross_section_panel(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        true,
    )
    .unwrap();
    let submission = submit_test_cross_section_panel_bricks(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        &pool,
    )
    .unwrap();

    assert!(submission.request_changed);
    assert!(submission.queued);
    assert_eq!(
        opened
            .render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        1
    );
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .chunks
            .values()
            .any(|entry| entry.state
                == crate::cross_section_runtime::CrossSectionChunkState::Decoding)
    );
    assert_eq!(opened.render_runtime.visible_bricks, visible_bricks_before);
    assert_eq!(
        opened.dataset_runtime.brick_stream_scale_level,
        brick_stream_scale_before
    );
    assert_eq!(
        opened.dataset_runtime.brick_stream_generation,
        brick_stream_generation_before
    );
    assert_eq!(
        opened.dataset_runtime.brick_stream_requested,
        brick_stream_requested_before
    );
    assert!(opened.dataset_runtime.resident_bricks_by_layer.is_empty());

    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    let partition = crate::cross_section_streaming::apply_cross_section_brick_read_outcomes(
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        application_view(&snapshot),
        vec![outcome],
    );

    assert!(partition.resident_changed);
    assert!(partition.unhandled.is_empty());
    assert_eq!(
        opened
            .render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        0
    );
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .chunks
            .values()
            .any(|entry| entry.state
                == crate::cross_section_runtime::CrossSectionChunkState::CpuResident)
    );
    assert_eq!(
        opened.dataset_runtime.brick_stream_requested,
        brick_stream_requested_before
    );
    assert!(opened.dataset_runtime.resident_bricks_by_layer.is_empty());

    let plan = schedule_test_cross_section_panel(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        true,
    )
    .unwrap();
    assert_eq!(
        plan.schedule.status,
        crate::viewer_layout::CrossSectionPanelScheduleStatus::Ready
    );
    assert_eq!(plan.schedule.missing_occupied_bricks, 0);
}

#[test]
fn cross_section_submission_batches_global_visible_chunks_without_queue_explosion() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_many_xy_chunks_app_dataset(tempdir.path());
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 8, 256).unwrap();
    let presentation = PresentationViewport::new(9.0, 9.0).unwrap();
    let render = RenderViewport::new(90, 90).unwrap();
    let cap =
        crate::cross_section_streaming::CROSS_SECTION_CHUNK_READ_SUBMISSIONS_PER_PANEL_CALL;

    set_test_cross_section_layout(&mut application, Some(1.0));
    let snapshot = application.snapshot();
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    schedule_test_cross_section_panel(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        true,
    )
    .unwrap();
    let visible_chunks = opened.render_runtime.cross_section_runtime.panels[&PanelId::Xy]
        .visible_chunks
        .len();
    assert!(
        visible_chunks > cap,
        "test fixture must exceed the per-call cross-section chunk submission cap"
    );

    let first_submission = submit_test_cross_section_panel_bricks(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        &pool,
    )
    .unwrap();

    assert!(first_submission.request_changed);
    assert!(first_submission.queued);
    assert_eq!(first_submission.queued_current_frame, cap);
    assert_eq!(
        opened
            .render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        cap
    );
    let first_stream = opened
        .render_runtime
        .cross_section_runtime
        .panel_streams
        .get(&PanelId::Xy)
        .unwrap();
    assert_eq!(first_stream.visible_chunks, visible_chunks);
    assert_eq!(first_stream.requested, cap);
    assert_eq!(first_stream.deferred, visible_chunks - cap);
    assert!(!first_stream.complete);
    assert!(crate::cross_section_streaming::cross_section_runtime_work_active(
        &opened.render_runtime.cross_section_runtime
    ));

    let outcomes = (0..cap)
        .map(|_| pool.recv_timeout(Duration::from_secs(2)).unwrap())
        .collect::<Vec<_>>();
    let partition = crate::cross_section_streaming::apply_cross_section_brick_read_outcomes(
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        application_view(&snapshot),
        outcomes,
    );
    assert!(partition.resident_changed);
    assert!(partition.unhandled.is_empty());
    assert_eq!(
        opened
            .render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        0
    );

    let second_submission = submit_test_cross_section_panel_bricks(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        &pool,
    )
    .unwrap();

    assert!(!second_submission.request_changed);
    assert!(second_submission.queued);
    assert_eq!(second_submission.queued_current_frame, visible_chunks - cap);
    assert_eq!(
        opened
            .render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        visible_chunks - cap
    );
    let second_stream = opened
        .render_runtime
        .cross_section_runtime
        .panel_streams
        .get(&PanelId::Xy)
        .unwrap();
    assert_eq!(second_stream.requested, visible_chunks);
    assert_eq!(second_stream.completed, cap);
    assert_eq!(second_stream.deferred, 0);
    assert!(crate::cross_section_streaming::cross_section_runtime_work_active(
        &opened.render_runtime.cross_section_runtime
    ));
}

#[test]
fn cross_section_hover_readout_samples_panel_resident_value() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    opened.dataset_runtime.resident_bricks.clear();
    opened.dataset_runtime.resident_bricks_by_layer.clear();
    set_test_cross_section_layout(&mut application, None);
    let snapshot = application.snapshot();
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    schedule_test_cross_section_panel(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        true,
    )
    .unwrap();
    submit_test_cross_section_panel_bricks(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        &pool,
    )
    .unwrap();
    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    let partition = crate::cross_section_streaming::apply_cross_section_brick_read_outcomes(
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        application_view(&snapshot),
        vec![outcome],
    );
    assert!(partition.resident_changed);
    let plan = schedule_test_cross_section_panel(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        true,
    )
    .unwrap();
    assert_eq!(plan.schedule.render_scale_level, Some(0));
    assert_eq!(
        plan.schedule.status,
        crate::viewer_layout::CrossSectionPanelScheduleStatus::Ready
    );
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .mark_panel_displayed(PanelId::Xy, plan.schedule.generation)
    );

    let readout = test_cross_section_hover_readout(
        &snapshot,
        &opened.dataset_runtime,
        &opened.render_runtime,
        PanelId::Xy,
        120.0,
        90.0,
        presentation,
    )
    .unwrap();

    assert_eq!(
        readout.status,
        crate::cross_section_readout::CrossSectionHoverStatus::Value
    );
    assert_eq!(readout.target_generation, 1);
    assert_eq!(readout.displayed_generation, Some(1));
    assert_eq!(readout.schedule_generation, Some(1));
    assert!(readout.display_current);
    assert_eq!(
        readout.generation_status,
        crate::cross_section_readout::CrossSectionHoverGenerationStatus::CurrentDisplayed
    );
    assert_eq!(readout.scale_level, Some(0));
    assert_eq!(
        readout.nearest_grid_index,
        Some(crate::cross_section_readout::CrossSectionGridIndex { x: 8, y: 8, z: 8 })
    );
    assert_eq!(
        readout.value,
        Some(crate::cross_section_readout::CrossSectionHoverValue::U16(
            expected_fixture_value(0, 8, 8, 8)
        ))
    );
    assert!(readout.text.contains("XY ch0 t0 s0 nearest"));
    assert!(readout.text.contains("value=2200"));
    assert!(
        opened.dataset_runtime.resident_bricks.is_empty(),
        "2D readout must not fall back to the current 3D resident brick set"
    );
}

#[test]
fn cross_section_hover_readout_reports_retained_stale_display_generation() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    set_test_cross_section_layout(&mut application, None);
    let snapshot = application.snapshot();
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    schedule_test_cross_section_panel(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        true,
    )
    .unwrap();
    submit_test_cross_section_panel_bricks(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        &pool,
    )
    .unwrap();
    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    crate::cross_section_streaming::apply_cross_section_brick_read_outcomes(
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        application_view(&snapshot),
        vec![outcome],
    );
    let plan = schedule_test_cross_section_panel(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        true,
    )
    .unwrap();
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .mark_panel_displayed(PanelId::Xy, plan.schedule.generation)
    );

    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .mark_cross_section_panels_dirty()
    );
    let readout = test_cross_section_hover_readout(
        &snapshot,
        &opened.dataset_runtime,
        &opened.render_runtime,
        PanelId::Xy,
        120.0,
        90.0,
        presentation,
    )
    .unwrap();

    assert_eq!(
        readout.status,
        crate::cross_section_readout::CrossSectionHoverStatus::Stale
    );
    assert_eq!(readout.value, None);
    assert_eq!(readout.target_generation, 2);
    assert_eq!(readout.displayed_generation, Some(1));
    assert_eq!(readout.schedule_generation, Some(2));
    assert!(!readout.display_current);
    assert_eq!(
        readout.generation_status,
        crate::cross_section_readout::CrossSectionHoverGenerationStatus::RetainedStale
    );
    assert!(readout.text.contains("retained displayed generation"));
}

#[test]
fn cross_section_hover_readout_reports_missing_resident_without_requests() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    set_test_cross_section_layout(&mut application, None);
    let snapshot = application.snapshot();
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    let plan = schedule_test_cross_section_panel(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        true,
    )
    .unwrap();
    assert_eq!(
        plan.schedule.status,
        crate::viewer_layout::CrossSectionPanelScheduleStatus::Incomplete
    );
    let requested_before = opened.dataset_runtime.brick_stream_requested;
    let panel_streams_before = opened
        .render_runtime
        .cross_section_runtime
        .panel_streams
        .clone();

    let readout = test_cross_section_hover_readout(
        &snapshot,
        &opened.dataset_runtime,
        &opened.render_runtime,
        PanelId::Xy,
        120.0,
        90.0,
        presentation,
    )
    .unwrap();

    assert_eq!(
        readout.status,
        crate::cross_section_readout::CrossSectionHoverStatus::Incomplete
    );
    assert_eq!(readout.displayed_generation, None);
    assert_eq!(
        readout.generation_status,
        crate::cross_section_readout::CrossSectionHoverGenerationStatus::CurrentUndisplayed
    );
    assert_eq!(readout.value, None);
    assert!(readout.text.contains("incomplete (resident brick unavailable)"));
    assert_eq!(opened.dataset_runtime.brick_stream_requested, requested_before);
    assert_eq!(
        opened.render_runtime.cross_section_runtime.panel_streams,
        panel_streams_before
    );
}

#[test]
fn active_cross_section_panel_priority_does_not_mutate_3d_stream_state() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let pool = BrickReadPool::new(app.dataset_runtime.dataset.clone(), 1, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();
    let brick_stream_scale_before = app.dataset_runtime.brick_stream_scale_level;
    let brick_stream_generation_before = app.dataset_runtime.brick_stream_generation;
    let brick_stream_requested_before = app.dataset_runtime.brick_stream_requested;

    set_test_cross_section_layout(&mut app.application, None);
    assert!(
        app.render_runtime
            .cross_section_runtime
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    assert!(
        app.render_runtime
            .cross_section_runtime
            .record_panel_viewports(PanelId::Xz, presentation, render)
    );
    set_test_active_cross_section_panel(&mut app.application, PanelId::Xz);
    let snapshot = app.application.snapshot();
    assert_eq!(
        snapshot.transient().active_cross_section_panel(),
        Some(mirante4d_application::CrossSectionPanelId::Xz)
    );
    for panel_id in [PanelId::Xy, PanelId::Xz] {
        schedule_test_cross_section_panel(
            &snapshot,
            &app.dataset_runtime,
            &mut app.render_runtime,
            panel_id,
            true,
        )
        .unwrap();
    }
    let submission = submit_test_cross_section_visible_chunks(
        &snapshot,
        &app.dataset_runtime,
        &mut app.render_runtime,
        &pool,
    )
    .unwrap();

    assert!(submission.request_changed);
    assert!(submission.queued);
    assert!(submission.queued_current_frame > 0);
    assert_eq!(submission.queued_prefetch, 0);
    assert_eq!(
        app.render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        1
    );
    assert_eq!(
        app.render_runtime
            .cross_section_runtime
            .read_tickets
            .first()
            .map(|ticket| ticket.panel_id),
        Some(PanelId::Xz)
    );

    let active_stream = app
        .render_runtime
        .cross_section_runtime
        .panel_streams
        .get(&PanelId::Xz)
        .unwrap();
    assert_eq!(
        active_stream.priority,
        mirante4d_data::BrickRequestPriority::CurrentFrame
    );
    assert!(active_stream.queued_current_frame > 0);
    assert_eq!(active_stream.queued_prefetch, 0);
    assert!(!active_stream.fairness_promoted);

    assert_eq!(
        app.dataset_runtime.brick_stream_scale_level,
        brick_stream_scale_before
    );
    assert_eq!(
        app.dataset_runtime.brick_stream_generation,
        brick_stream_generation_before
    );
    assert_eq!(
        app.dataset_runtime.brick_stream_requested,
        brick_stream_requested_before
    );

    let diagnostics = app.diagnostics_summary_text();
    assert!(diagnostics.contains("cross_section_active_panel: XZ"));
    assert!(diagnostics.contains("cross_section_panel_XY_target_scale_level: 0"));
    assert!(diagnostics.contains("cross_section_panel_XY_render_scale_level: 0"));
    assert!(diagnostics.contains("cross_section_panel_XZ_target_scale_level: 0"));
    assert!(diagnostics.contains("cross_section_panel_XZ_render_scale_level: 0"));
    assert!(diagnostics.contains("cross_section_panel_XZ_selected_bricks:"));
    assert!(diagnostics.contains("cross_section_global_panels: 4"));
    assert!(diagnostics.contains("cross_section_global_panel_XY_priority_tier: VisibleLinked"));
    assert!(diagnostics.contains("cross_section_global_panel_XZ_priority_tier: VisibleActive"));
    assert!(diagnostics.contains("cross_section_stream_XZ_priority: CurrentFrame"));
}

#[test]
fn cross_section_submission_resubmits_stale_queued_chunk_without_live_ticket() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    set_test_cross_section_layout(&mut application, None);
    let snapshot = application.snapshot();
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    schedule_test_cross_section_panel(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        true,
    )
    .unwrap();
    let chunk_key = opened.render_runtime.cross_section_runtime.panels[&PanelId::Xy]
        .visible_chunks[0]
        .clone();
    let metadata = opened
        .dataset_runtime
        .dataset
        .brick_metadata_at_scale(
            &chunk_key.layer_id,
            chunk_key.scale_level,
            chunk_key.timepoint,
            chunk_key.brick_index,
        )
        .unwrap();
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .mark_chunk_queued(chunk_key.clone(), metadata.region)
    );

    let submission = submit_test_cross_section_visible_chunks(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        &pool,
    )
    .unwrap();

    assert!(submission.queued);
    assert_eq!(
        opened
            .render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        1
    );
    assert_eq!(
        opened.render_runtime.cross_section_runtime.read_tickets[0].brick_index,
        chunk_key.brick_index
    );
    assert_eq!(
        opened.render_runtime.cross_section_runtime.read_tickets[0].region,
        metadata.region
    );
}

#[test]
fn global_cross_section_submission_waits_for_active_panel_visible_work() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 2, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    set_test_cross_section_layout(&mut application, None);
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .record_panel_viewports(PanelId::Xz, presentation, render)
    );
    set_test_active_cross_section_panel(&mut application, PanelId::Xz);
    let snapshot = application.snapshot();
    schedule_test_cross_section_panel(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        true,
    )
    .unwrap();

    let early_submission = submit_test_cross_section_visible_chunks(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        &pool,
    )
    .unwrap();

    assert!(!early_submission.queued);
    assert_eq!(
        opened
            .render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        0
    );
    assert!(!opened
        .render_runtime
        .cross_section_runtime
        .panel_streams
        .contains_key(&PanelId::Xy));

    schedule_test_cross_section_panel(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xz,
        true,
    )
    .unwrap();
    let active_submission = submit_test_cross_section_visible_chunks(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        &pool,
    )
    .unwrap();

    assert!(active_submission.queued);
    assert_eq!(
        opened
            .render_runtime
            .cross_section_runtime
            .read_tickets
            .first()
            .map(|ticket| ticket.panel_id),
        Some(PanelId::Xz)
    );
    assert_eq!(
        opened
            .render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        1
    );
    let active_stream = opened
        .render_runtime
        .cross_section_runtime
        .panel_streams
        .get(&PanelId::Xz)
        .unwrap();
    assert_eq!(
        active_stream.priority,
        mirante4d_data::BrickRequestPriority::CurrentFrame
    );
    assert!(active_stream.queued_current_frame > 0);
}

#[test]
fn global_cross_section_submission_queues_worker_reads_in_runtime_order() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_many_xy_chunks_app_dataset(tempdir.path());
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 256).unwrap();
    let presentation = PresentationViewport::new(9.0, 9.0).unwrap();
    let render = RenderViewport::new(90, 90).unwrap();

    set_test_cross_section_layout(&mut application, Some(1.0));
    let snapshot = application.snapshot();
    for panel_id in [PanelId::Xy, PanelId::Xz] {
        assert!(
            opened
                .render_runtime
                .cross_section_runtime
                .record_panel_viewports(panel_id, presentation, render)
        );
        schedule_test_cross_section_panel(
            &snapshot,
            &opened.dataset_runtime,
            &mut opened.render_runtime,
            panel_id,
            true,
        )
        .unwrap();
    }

    let expected_order = opened
        .render_runtime
        .cross_section_runtime
        .download_promotion_entries_for_panels([PanelId::Xy, PanelId::Xz])
        .into_iter()
        .take(crate::cross_section_streaming::CROSS_SECTION_CHUNK_READ_SUBMISSIONS_PER_REFRESH)
        .map(|entry| (entry.panel_id.unwrap(), entry.key.brick_index))
        .collect::<Vec<_>>();
    assert!(
        expected_order
            .windows(2)
            .any(|pair| pair[0].0 != pair[1].0),
        "fixture should expose interleaved runtime queue entries across panels"
    );

    let submission = submit_test_cross_section_visible_chunks(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        &pool,
    )
    .unwrap();

    assert!(submission.queued);
    let actual_order = opened
        .render_runtime
        .cross_section_runtime
        .read_tickets
        .iter()
        .map(|ticket| (ticket.panel_id, ticket.brick_index))
        .collect::<Vec<_>>();
    assert_eq!(actual_order, expected_order);
}

#[test]
fn global_cross_section_submission_uses_2d_read_backend_boundary() {
    use std::cell::RefCell;

    use mirante4d_data::{BrickReadTicket, DataError, DataGenerationId, DataRequestId};

    #[derive(Debug)]
    struct RecordingReadBackend {
        generation: DataGenerationId,
        submissions: RefCell<Vec<crate::cross_section_read_queue::CrossSectionChunkReadSubmission>>,
    }

    impl crate::cross_section_read_queue::CrossSectionReadBackend for RecordingReadBackend {
        fn active_generation(&self) -> DataGenerationId {
            self.generation
        }

        fn submit_cross_section_chunk_read(
            &self,
            generation_id: DataGenerationId,
            submission: crate::cross_section_read_queue::CrossSectionChunkReadSubmission,
        ) -> Result<BrickReadTicket, DataError> {
            let request_id = DataRequestId(self.submissions.borrow().len() as u64 + 1);
            let scale_level = submission.scale_level;
            let cancellation = submission.cancellation.clone();
            self.submissions.borrow_mut().push(submission);
            Ok(BrickReadTicket {
                request_id,
                generation_id,
                scale_level,
                cancellation,
            })
        }
    }

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_many_xy_chunks_app_dataset(tempdir.path());
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let backend = RecordingReadBackend {
        generation: DataGenerationId(7),
        submissions: RefCell::new(Vec::new()),
    };
    let presentation = PresentationViewport::new(9.0, 9.0).unwrap();
    let render = RenderViewport::new(90, 90).unwrap();

    set_test_cross_section_layout(&mut application, Some(1.0));
    let snapshot = application.snapshot();
    for panel_id in [PanelId::Xy, PanelId::Xz] {
        assert!(
            opened
                .render_runtime
                .cross_section_runtime
                .record_panel_viewports(panel_id, presentation, render)
        );
        schedule_test_cross_section_panel(
            &snapshot,
            &opened.dataset_runtime,
            &mut opened.render_runtime,
            panel_id,
            true,
        )
        .unwrap();
    }

    let expected_admissions = crate::cross_section_read_queue::cross_section_read_admissions_for_refresh(
        &opened.render_runtime.cross_section_runtime,
        [PanelId::Xy, PanelId::Xz],
        crate::cross_section_streaming::CROSS_SECTION_CHUNK_READ_SUBMISSIONS_PER_REFRESH,
    );
    assert!(
        expected_admissions
            .windows(2)
            .any(|pair| pair[0].queue_entry.panel_id != pair[1].queue_entry.panel_id),
        "fixture should expose interleaved runtime queue entries across panels"
    );

    let submission = submit_test_cross_section_visible_chunks_to_read_queue(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        &backend,
    )
    .unwrap();

    assert!(submission.queued);
    let submissions = backend.submissions.borrow();
    assert_eq!(submissions.len(), expected_admissions.len());
    for (submitted, expected) in submissions.iter().zip(expected_admissions.iter()) {
        assert_eq!(submitted.layer_id, expected.queue_entry.key.layer_id);
        assert_eq!(submitted.scale_level, expected.queue_entry.key.scale_level);
        assert_eq!(submitted.timepoint, expected.queue_entry.key.timepoint);
        assert_eq!(submitted.brick_index, expected.queue_entry.key.brick_index);
        assert_eq!(submitted.queue_priority, expected.worker_queue_priority);
    }
    assert_eq!(
        opened
            .render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        expected_admissions.len()
    );
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .read_tickets
            .iter()
            .all(|ticket| ticket.ticket.generation_id == backend.generation)
    );
}

#[test]
fn app_drains_cross_section_read_pool_without_3d_brick_pool() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_many_xy_chunks_app_dataset(tempdir.path());
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    app.dataset_runtime.cross_section_read_pool = Some(
        mirante4d_data::CrossSectionChunkReadPool::new(
            app.dataset_runtime.dataset.clone(),
            1,
            16,
        )
        .unwrap(),
    );
    let presentation = PresentationViewport::new(9.0, 9.0).unwrap();
    let render = RenderViewport::new(90, 90).unwrap();

    set_test_cross_section_layout(&mut app.application, Some(1.0));
    let snapshot = app.application.snapshot();
    assert!(
        app.render_runtime
            .cross_section_runtime
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    schedule_test_cross_section_panel(
        &snapshot,
        &app.dataset_runtime,
        &mut app.render_runtime,
        PanelId::Xy,
        true,
    )
    .unwrap();
    let pool = app.dataset_runtime.cross_section_read_pool.take().unwrap();
    let submission = submit_test_cross_section_visible_chunks_to_read_queue(
        &snapshot,
        &app.dataset_runtime,
        &mut app.render_runtime,
        &pool,
    )
    .unwrap();
    app.dataset_runtime.cross_section_read_pool = Some(pool);

    assert!(submission.queued);
    assert!(app.dataset_runtime.brick_read_pool.is_none());
    assert!(
        app.render_runtime
            .cross_section_runtime
            .pending_read_ticket_count()
            > 0
    );

    let ctx = egui::Context::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while app
        .render_runtime
        .cross_section_runtime
        .pending_read_ticket_count()
        > 0
    {
        app.drain_brick_results(&ctx);
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    assert_eq!(
        app.render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        0
    );
    assert!(
        app.render_runtime
            .cross_section_runtime
            .chunks
            .values()
            .any(|entry| entry.state
                == crate::cross_section_runtime::CrossSectionChunkState::CpuResident)
    );
    assert_eq!(
        app.dataset_runtime
            .brick_result_drain_last_repaint_reason
            .as_deref(),
        Some("cross_section_panel_resident_pending")
    );
}

#[test]
fn sustained_active_cross_section_work_promotes_one_linked_inactive_panel_for_fairness() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_many_xy_chunks_app_dataset(tempdir.path());
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let pool = BrickReadPool::new(app.dataset_runtime.dataset.clone(), 8, 256).unwrap();
    let presentation = PresentationViewport::new(9.0, 9.0).unwrap();
    let render = RenderViewport::new(90, 90).unwrap();

    set_test_cross_section_layout(&mut app.application, Some(1.0));
    let scheduling_snapshot = app.application.snapshot();
    for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        assert!(
            app.render_runtime
                .cross_section_runtime
                .record_panel_viewports(panel_id, presentation, render)
        );
        schedule_test_cross_section_panel(
            &scheduling_snapshot,
            &app.dataset_runtime,
            &mut app.render_runtime,
            panel_id,
            true,
        )
        .unwrap();
    }
    set_test_active_cross_section_panel(&mut app.application, PanelId::Xz);
    let snapshot = app.application.snapshot();

    let active_submission = submit_test_cross_section_panel_bricks(
        &snapshot,
        &app.dataset_runtime,
        &mut app.render_runtime,
        PanelId::Xz,
        &pool,
    )
    .unwrap();
    assert!(active_submission.queued);
    assert!(active_submission.queued_current_frame > 0);
    assert!(!active_submission.fairness_promoted);

    let fairness_submission = submit_test_cross_section_panel_bricks(
        &snapshot,
        &app.dataset_runtime,
        &mut app.render_runtime,
        PanelId::Xy,
        &pool,
    )
    .unwrap();
    assert!(fairness_submission.queued);
    assert!(fairness_submission.queued_current_frame > 0);
    assert_eq!(fairness_submission.queued_prefetch, 0);
    assert!(fairness_submission.fairness_promoted);

    let bounded_inactive_submission = submit_test_cross_section_panel_bricks(
        &snapshot,
        &app.dataset_runtime,
        &mut app.render_runtime,
        PanelId::Yz,
        &pool,
    )
    .unwrap();
    assert!(bounded_inactive_submission.queued);
    assert_eq!(bounded_inactive_submission.queued_current_frame, 0);
    assert!(bounded_inactive_submission.queued_prefetch > 0);
    assert!(!bounded_inactive_submission.fairness_promoted);

    let xy_stream = app
        .render_runtime
        .cross_section_runtime
        .panel_streams
        .get(&PanelId::Xy)
        .unwrap();
    assert_eq!(xy_stream.priority, mirante4d_data::BrickRequestPriority::CurrentFrame);
    assert!(xy_stream.fairness_promoted);
    assert_eq!(xy_stream.active_panel_at_submission, Some(PanelId::Xz));
    let yz_stream = app
        .render_runtime
        .cross_section_runtime
        .panel_streams
        .get(&PanelId::Yz)
        .unwrap();
    assert_eq!(yz_stream.priority, mirante4d_data::BrickRequestPriority::Prefetch);
    assert!(!yz_stream.fairness_promoted);

    let diagnostics = app.diagnostics_summary_text();
    assert!(diagnostics.contains("cross_section_stream_XY_fairness_promoted: true"));
    assert!(diagnostics.contains("cross_section_stream_YZ_fairness_promoted: false"));
}

#[test]
fn cross_section_stale_generation_outcome_does_not_update_global_runtime() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    set_test_cross_section_layout(&mut application, None);
    let snapshot = application.snapshot();
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    schedule_test_cross_section_panel(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        true,
    )
    .unwrap();
    let submission = submit_test_cross_section_panel_bricks(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        &pool,
    )
    .unwrap();
    assert!(submission.queued);
    assert_eq!(
        opened
            .render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        1
    );

    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .mark_cross_section_panels_dirty()
    );
    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    let partition = crate::cross_section_streaming::apply_cross_section_brick_read_outcomes(
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        application_view(&snapshot),
        vec![outcome],
    );

    assert!(!partition.resident_changed);
    assert!(partition.unhandled.is_empty());
    assert_eq!(
        opened
            .render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        0
    );
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .chunks
            .values()
            .all(|entry| entry.state
                != crate::cross_section_runtime::CrossSectionChunkState::CpuResident)
    );
    assert!(
        opened
            .render_runtime
            .cross_section_runtime
            .chunks
            .values()
            .all(|entry| entry.state
                != crate::cross_section_runtime::CrossSectionChunkState::Decoding)
    );
}

#[test]
fn cross_section_request_change_keeps_shared_global_chunk_ticket_live() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    set_test_cross_section_layout(&mut application, None);
    let snapshot = application.snapshot();
    for panel_id in [PanelId::Xy, PanelId::Xz] {
        assert!(
            opened
                .render_runtime
                .cross_section_runtime
                .record_panel_viewports(panel_id, presentation, render)
        );
        schedule_test_cross_section_panel(
            &snapshot,
            &opened.dataset_runtime,
            &mut opened.render_runtime,
            panel_id,
            true,
        )
        .unwrap();
    }

    let shared_chunk = opened.render_runtime.cross_section_runtime.panels[&PanelId::Xy]
        .visible_chunks[0]
        .clone();
    assert!(
        opened.render_runtime.cross_section_runtime.panels[&PanelId::Xz]
            .visible_chunks
            .contains(&shared_chunk),
        "fixture must expose the same chunk through the linked panel"
    );

    let first_submission = submit_test_cross_section_panel_bricks(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        &pool,
    )
    .unwrap();
    assert!(first_submission.queued);
    assert_eq!(
        opened
            .render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        1
    );

    assert!(
        opened.render_runtime.cross_section_runtime.record_panel_viewports(
            PanelId::Xy,
            PresentationViewport::new(241.0, 180.0).unwrap(),
            render,
        ),
        "changing only the submitting panel should stale that panel generation"
    );
    schedule_test_cross_section_panel(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        true,
    )
    .unwrap();

    let changed_submission = submit_test_cross_section_panel_bricks(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        &pool,
    )
    .unwrap();

    assert!(changed_submission.request_changed);
    assert!(!changed_submission.queued);
    assert_eq!(
        opened
            .render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        1,
        "the old ticket remains the single live global read because XZ still needs the chunk"
    );
    let stream = opened
        .render_runtime
        .cross_section_runtime
        .panel_streams
        .get(&PanelId::Xy)
        .unwrap();
    assert_eq!(stream.deferred, 1);
}

#[test]
fn cross_section_stale_panel_ticket_updates_global_runtime_when_chunk_is_still_visible() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    set_test_cross_section_layout(&mut application, None);
    let snapshot = application.snapshot();
    for panel_id in [PanelId::Xy, PanelId::Xz] {
        assert!(
            opened
                .render_runtime
                .cross_section_runtime
                .record_panel_viewports(panel_id, presentation, render)
        );
        schedule_test_cross_section_panel(
            &snapshot,
            &opened.dataset_runtime,
            &mut opened.render_runtime,
            panel_id,
            true,
        )
        .unwrap();
    }

    let shared_chunk = opened.render_runtime.cross_section_runtime.panels[&PanelId::Xy]
        .visible_chunks[0]
        .clone();
    assert!(
        opened.render_runtime.cross_section_runtime.panels[&PanelId::Xz]
            .visible_chunks
            .contains(&shared_chunk),
        "fixture must expose the same chunk through the linked panel"
    );

    let submission = submit_test_cross_section_panel_bricks(
        &snapshot,
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        PanelId::Xy,
        &pool,
    )
    .unwrap();
    assert!(submission.queued);
    assert_eq!(
        opened
            .render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        1
    );

    assert!(
        opened.render_runtime.cross_section_runtime.record_panel_viewports(
            PanelId::Xy,
            PresentationViewport::new(241.0, 180.0).unwrap(),
            render,
        ),
        "changing only the submitting panel should stale that panel generation"
    );

    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    let partition = crate::cross_section_streaming::apply_cross_section_brick_read_outcomes(
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        application_view(&snapshot),
        vec![outcome],
    );

    assert!(partition.resident_changed);
    assert!(partition.unhandled.is_empty());
    assert_eq!(
        opened
            .render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        0
    );
    assert_eq!(
        opened.render_runtime.cross_section_runtime.chunks[&shared_chunk].state,
        crate::cross_section_runtime::CrossSectionChunkState::CpuResident
    );
    let stale_xy_stream = opened
        .render_runtime
        .cross_section_runtime
        .panel_streams
        .get(&PanelId::Xy)
        .unwrap();
    assert_eq!(
        stale_xy_stream.completed, 0,
        "stale panel stream accounting must not claim the completion"
    );
}

fn write_sparse_spatially_chunked_app_dataset(output_root: &Path, all_empty: bool) -> PathBuf {
    let package_root = if all_empty {
        output_root.join("app-empty-spatially-chunked.m4d")
    } else {
        output_root.join("app-sparse-spatially-chunked.m4d")
    };
    let shape = Shape4D::new(1, 4, 4, 4).unwrap();
    let mut values = vec![0u16; shape.element_count().unwrap() as usize];
    if !all_empty {
        values[0] = 10_000;
    }
    write_native_u16_dataset(
        &package_root,
        NativeU16Dataset {
            id: if all_empty {
                "app-empty-spatially-chunked-fixture".to_owned()
            } else {
                "app-sparse-spatially-chunked-fixture".to_owned()
            },
            name: if all_empty {
                "App empty spatially chunked fixture".to_owned()
            } else {
                "App sparse spatially chunked fixture".to_owned()
            },
            world_space: mirante4d_format::WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_format::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape,
                brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
                display: default_u16_display(),
                values_tzyx: values,
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn write_many_xy_chunks_app_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("app-many-xy-chunks.m4d");
    let shape = Shape4D::new(1, 3, 9, 9).unwrap();
    write_native_u16_dataset(
        &package_root,
        NativeU16Dataset {
            id: "app-many-xy-chunks-fixture".to_owned(),
            name: "App many XY chunks fixture".to_owned(),
            world_space: mirante4d_format::WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_format::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape,
                brick_shape: Shape4D::new(1, 1, 1, 1).unwrap(),
                grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
                display: default_u16_display(),
                values_tzyx: vec![1; shape.element_count().unwrap() as usize],
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn write_time_spatially_chunked_app_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("app-time-spatially-chunked.m4d");
    let shape = Shape4D::new(3, 4, 4, 4).unwrap();
    write_native_u16_dataset(
        &package_root,
        NativeU16Dataset {
            id: "app-time-spatially-chunked-fixture".to_owned(),
            name: "App time spatially chunked fixture".to_owned(),
            world_space: mirante4d_format::WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_format::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape,
                brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
                display: default_u16_display(),
                values_tzyx: fixture_values(shape),
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn write_warm_spatially_chunked_app_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("app-warm-spatially-chunked.m4d");
    let shape = Shape4D::new(1, 6, 6, 6).unwrap();
    write_native_u16_dataset(
        &package_root,
        NativeU16Dataset {
            id: "app-warm-spatially-chunked-fixture".to_owned(),
            name: "App warm spatially chunked fixture".to_owned(),
            world_space: mirante4d_format::WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_format::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape,
                brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
                display: default_u16_display(),
                values_tzyx: fixture_values(shape),
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn write_multiscale_app_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("app-multiscale.m4d");
    let s0_shape = Shape4D::new(1, 4, 4, 4).unwrap();
    let s1_shape = Shape4D::new(1, 2, 2, 2).unwrap();
    let s0_grid_to_world = mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0);
    let s1_grid_to_world = s0_grid_to_world
        .downsampled_integer_centered(2, 2, 2)
        .unwrap();
    write_native_u16_multiscale_dataset(
        &package_root,
        NativeU16MultiscaleDataset {
            id: "app-multiscale-fixture".to_owned(),
            name: "App multiscale fixture".to_owned(),
            world_space: mirante4d_format::WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_format::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16MultiscaleLayer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape: s0_shape,
                grid_to_world: s0_grid_to_world,
                display: default_u16_display(),
                scales: vec![
                    DenseU16Scale {
                        level: 0,
                        shape: s0_shape,
                        brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                        grid_to_world: s0_grid_to_world,
                        source_scale: None,
                        reduction: ScaleReduction::Source,
                        values_tzyx: fixture_values(s0_shape),
                    },
                    DenseU16Scale {
                        level: 1,
                        shape: s1_shape,
                        brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                        grid_to_world: s1_grid_to_world,
                        source_scale: Some(0),
                        reduction: ScaleReduction::Mean,
                        values_tzyx: app_multiscale_s1_values(s1_shape),
                    },
                ],
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn write_large_multiscale_app_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("app-large-multiscale.m4d");
    let s0_shape = Shape4D::new(1, 33, 33, 33).unwrap();
    let s1_shape = Shape4D::new(1, 5, 5, 5).unwrap();
    let s0_grid_to_world = mirante4d_format::grid_to_world_scale_um(8.0, 8.0, 8.0);
    let s1_grid_to_world = s0_grid_to_world
        .downsampled_integer_centered(8, 8, 8)
        .unwrap();
    write_native_u16_multiscale_dataset(
        &package_root,
        NativeU16MultiscaleDataset {
            id: "app-large-multiscale-fixture".to_owned(),
            name: "App large multiscale fixture".to_owned(),
            world_space: mirante4d_format::WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_format::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16MultiscaleLayer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape: s0_shape,
                grid_to_world: s0_grid_to_world,
                display: default_u16_display(),
                scales: vec![
                    DenseU16Scale {
                        level: 0,
                        shape: s0_shape,
                        brick_shape: Shape4D::new(1, 4, 4, 4).unwrap(),
                        grid_to_world: s0_grid_to_world,
                        source_scale: None,
                        reduction: ScaleReduction::Source,
                        values_tzyx: vec![1; s0_shape.element_count().unwrap() as usize],
                    },
                    DenseU16Scale {
                        level: 1,
                        shape: s1_shape,
                        brick_shape: Shape4D::new(1, 5, 5, 5).unwrap(),
                        grid_to_world: s1_grid_to_world,
                        source_scale: Some(0),
                        reduction: ScaleReduction::Mean,
                        values_tzyx: vec![1; s1_shape.element_count().unwrap() as usize],
                    },
                ],
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn write_three_scale_budgeted_app_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("app-three-scale-budgeted.m4d");
    let s0_shape = Shape4D::new(1, 36, 36, 36).unwrap();
    let s1_shape = Shape4D::new(1, 18, 18, 18).unwrap();
    let s2_shape = Shape4D::new(1, 9, 9, 9).unwrap();
    let s0_grid_to_world = mirante4d_format::grid_to_world_scale_um(8.0, 8.0, 8.0);
    let s1_grid_to_world = s0_grid_to_world
        .downsampled_integer_centered(2, 2, 2)
        .unwrap();
    let s2_grid_to_world = s1_grid_to_world
        .downsampled_integer_centered(2, 2, 2)
        .unwrap();
    write_native_u16_multiscale_dataset(
        &package_root,
        NativeU16MultiscaleDataset {
            id: "app-three-scale-budgeted-fixture".to_owned(),
            name: "App three-scale budgeted fixture".to_owned(),
            world_space: mirante4d_format::WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_format::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16MultiscaleLayer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape: s0_shape,
                grid_to_world: s0_grid_to_world,
                display: default_u16_display(),
                scales: vec![
                    DenseU16Scale {
                        level: 0,
                        shape: s0_shape,
                        brick_shape: Shape4D::new(1, 3, 3, 3).unwrap(),
                        grid_to_world: s0_grid_to_world,
                        source_scale: None,
                        reduction: ScaleReduction::Source,
                        values_tzyx: vec![1; s0_shape.element_count().unwrap() as usize],
                    },
                    DenseU16Scale {
                        level: 1,
                        shape: s1_shape,
                        brick_shape: Shape4D::new(1, 3, 3, 3).unwrap(),
                        grid_to_world: s1_grid_to_world,
                        source_scale: Some(0),
                        reduction: ScaleReduction::Mean,
                        values_tzyx: vec![1; s1_shape.element_count().unwrap() as usize],
                    },
                    DenseU16Scale {
                        level: 2,
                        shape: s2_shape,
                        brick_shape: Shape4D::new(1, 3, 3, 3).unwrap(),
                        grid_to_world: s2_grid_to_world,
                        source_scale: Some(1),
                        reduction: ScaleReduction::Mean,
                        values_tzyx: vec![1; s2_shape.element_count().unwrap() as usize],
                    },
                ],
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn write_above_minimum_cap_app_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("app-above-minimum-cap.m4d");
    let s0_shape = Shape4D::new(1, 48, 48, 48).unwrap();
    let s1_shape = Shape4D::new(1, 24, 24, 24).unwrap();
    let s2_shape = Shape4D::new(1, 12, 12, 12).unwrap();
    let s0_grid_to_world = mirante4d_format::grid_to_world_scale_um(6.0, 6.0, 6.0);
    let s1_grid_to_world = s0_grid_to_world
        .downsampled_integer_centered(2, 2, 2)
        .unwrap();
    let s2_grid_to_world = s1_grid_to_world
        .downsampled_integer_centered(2, 2, 2)
        .unwrap();
    write_native_u16_multiscale_dataset(
        &package_root,
        NativeU16MultiscaleDataset {
            id: "app-above-minimum-cap-fixture".to_owned(),
            name: "App above minimum cap fixture".to_owned(),
            world_space: mirante4d_format::WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_format::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16MultiscaleLayer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape: s0_shape,
                grid_to_world: s0_grid_to_world,
                display: default_u16_display(),
                scales: vec![
                    DenseU16Scale {
                        level: 0,
                        shape: s0_shape,
                        brick_shape: Shape4D::new(1, 3, 3, 3).unwrap(),
                        grid_to_world: s0_grid_to_world,
                        source_scale: None,
                        reduction: ScaleReduction::Source,
                        values_tzyx: vec![1; s0_shape.element_count().unwrap() as usize],
                    },
                    DenseU16Scale {
                        level: 1,
                        shape: s1_shape,
                        brick_shape: Shape4D::new(1, 3, 3, 3).unwrap(),
                        grid_to_world: s1_grid_to_world,
                        source_scale: Some(0),
                        reduction: ScaleReduction::Mean,
                        values_tzyx: vec![1; s1_shape.element_count().unwrap() as usize],
                    },
                    DenseU16Scale {
                        level: 2,
                        shape: s2_shape,
                        brick_shape: Shape4D::new(1, 6, 6, 6).unwrap(),
                        grid_to_world: s2_grid_to_world,
                        source_scale: Some(1),
                        reduction: ScaleReduction::Mean,
                        values_tzyx: vec![1; s2_shape.element_count().unwrap() as usize],
                    },
                ],
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn fixture_values(shape: Shape4D) -> Vec<u16> {
    let mut values = Vec::with_capacity(shape.element_count().unwrap() as usize);
    for t in 0..shape.t() {
        for z in 0..shape.z() {
            for y in 0..shape.y() {
                for x in 0..shape.x() {
                    values.push(expected_fixture_value(t, z, y, x));
                }
            }
        }
    }
    values
}

fn write_app_ome_stack(path: &Path) -> Result<(), tiff::TiffError> {
    let file = File::create(path)?;
    let mut encoder = TiffEncoder::new(file)?;
    let ome_xml = r#"<OME xmlns="http://www.openmicroscopy.org/Schemas/OME/2016-06"><Image ID="Image:0"><Pixels ID="Pixels:0" DimensionOrder="XYZCT" Type="uint16" SizeX="3" SizeY="2" SizeZ="2" SizeC="1" SizeT="1" PhysicalSizeX="0.25" PhysicalSizeXUnit="um" PhysicalSizeY="0.5" PhysicalSizeYUnit="um" PhysicalSizeZ="0.75" PhysicalSizeZUnit="um"><Channel ID="Channel:0:0" SamplesPerPixel="1"/></Pixels></Image></OME>"#;
    for z in 0..2 {
        let values = (0..2)
            .flat_map(|y| (0..3).map(move |x| (z * 10 + y * 3 + x) as u16))
            .collect::<Vec<_>>();
        let mut image = encoder.new_image::<colortype::Gray16>(3, 2)?;
        if z == 0 {
            image.encoder().write_tag(Tag::ImageDescription, ome_xml)?;
        }
        image.write_data(&values)?;
    }
    Ok(())
}

fn app_multiscale_s1_values(shape: Shape4D) -> Vec<u16> {
    let mut values = Vec::with_capacity(shape.element_count().unwrap() as usize);
    for t in 0..shape.t() {
        for z in 0..shape.z() {
            for y in 0..shape.y() {
                for x in 0..shape.x() {
                    values.push((30_000 + t * 1_000 + z * 100 + y * 10 + x) as u16);
                }
            }
        }
    }
    values
}

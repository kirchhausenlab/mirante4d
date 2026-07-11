fn iso_level_for_u16_threshold(threshold: u16) -> f32 {
    f32::from(threshold) / f32::from(u16::MAX)
}

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
fn frame_fidelity_label_reports_display_staleness_when_known() {
    let mut fidelity = FrameFidelityStatus::new_with_presentation(
        RenderViewport::new(998, 1024).unwrap(),
        crate::viewport::default_presentation_viewport(),
    );
    fidelity.displayed_scale_level = Some(0);
    fidelity.target_scale_level = 0;
    fidelity.completeness = FrameCompleteness::Exact;
    fidelity.reason = LodDecisionReason::ExactS0;
    fidelity.backend = RenderBackend::GpuResidentBricks;

    fidelity.display_freshness = DisplayedFrameFreshness::Current;
    assert_eq!(
        frame_fidelity_label(&fidelity),
        "shown s0 exact | GPU bricks | 998x1024 px; 512x512 pt | display current | render pending"
    );

    fidelity.display_freshness = DisplayedFrameFreshness::Stale;
    assert_eq!(
        frame_fidelity_label(&fidelity),
        "shown s0 exact | GPU bricks | 998x1024 px; 512x512 pt | display stale | render pending"
    );
}

#[test]
fn frame_fidelity_status_labels_cover_phase12_states() {
    let completeness_cases = [
        (FrameCompleteness::Exact, "exact"),
        (FrameCompleteness::Complete, "complete"),
        (FrameCompleteness::Loading, "loading"),
        (FrameCompleteness::Incomplete, "incomplete"),
        (FrameCompleteness::BudgetLimited, "budget-limited"),
    ];
    for (completeness, expected) in completeness_cases {
        assert_eq!(frame_completeness_label(completeness), expected);
    }

    let reason_cases = [
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
    ];
    for (reason, expected) in reason_cases {
        assert_eq!(frame_reason_label(reason), expected);
    }

    let failure_cases = [
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
    ];
    for (kind, expected) in failure_cases {
        assert_eq!(frame_failure_kind_label(kind), expected);
    }
}

#[test]
fn frame_fidelity_label_exposes_loading_complete_and_failure_reasons() {
    let mut fidelity = FrameFidelityStatus::new_with_presentation(
        RenderViewport::new(1280, 720).unwrap(),
        crate::viewport::default_presentation_viewport(),
    );
    fidelity.displayed_scale_level = Some(1);
    fidelity.target_scale_level = 0;
    fidelity.backend = RenderBackend::GpuResidentBricks;

    fidelity.completeness = FrameCompleteness::Loading;
    fidelity.reason = LodDecisionReason::LoadingTargetScale;
    let loading = frame_fidelity_label(&fidelity);
    assert!(loading.contains("loading"));
    assert!(loading.contains("loading target LOD"));

    fidelity.completeness = FrameCompleteness::Complete;
    fidelity.reason = LodDecisionReason::ScreenEquivalentCoarserScale;
    let complete = frame_fidelity_label(&fidelity);
    assert!(complete.contains("complete"));
    assert!(complete.contains("screen-equivalent LOD"));

    fidelity.completeness = FrameCompleteness::Incomplete;
    fidelity.reason = LodDecisionReason::BackendLimit;
    let backend_limit = frame_fidelity_label(&fidelity);
    assert!(backend_limit.contains("incomplete"));
    assert!(backend_limit.contains("backend limit"));

    fidelity.completeness = FrameCompleteness::BudgetLimited;
    fidelity.reason = LodDecisionReason::AllocationFailed;
    let allocation_failed = frame_fidelity_label(&fidelity);
    assert!(allocation_failed.contains("budget-limited"));
    assert!(allocation_failed.contains("allocation failed"));
}

fn test_analysis_table(rows: Vec<(&str, u64, f64)>) -> AnalysisTable {
    test_analysis_table_named("test-table", "test table", rows)
}

fn test_analysis_table_named(id: &str, name: &str, rows: Vec<(&str, u64, f64)>) -> AnalysisTable {
    AnalysisTable {
        id: id.to_owned(),
        name: name.to_owned(),
        state: AnalysisResultState::Complete,
        provenance: test_analysis_provenance(),
        columns: vec![
            AnalysisColumn::new("name", "name", None),
            AnalysisColumn::new("count", "count", None),
            AnalysisColumn::new("mean", "mean", None),
        ],
        rows: rows
            .into_iter()
            .map(|(name, count, mean)| {
                AnalysisTableRow::new([
                    ("name", AnalysisCell::Text(name.to_owned())),
                    ("count", AnalysisCell::Integer(count)),
                    ("mean", AnalysisCell::Float(mean)),
                ])
            })
            .collect(),
    }
}

fn test_analysis_plot(id: &str, name: &str, points: usize) -> AnalysisPlot {
    AnalysisPlot {
        id: id.to_owned(),
        name: name.to_owned(),
        state: AnalysisResultState::Complete,
        provenance: test_analysis_provenance(),
        x_label: "t".to_owned(),
        y_label: "mean".to_owned(),
        series: vec![AnalysisPlotSeries {
            name: "mean".to_owned(),
            points: (0..points)
                .map(|index| AnalysisPlotPoint {
                    x: index as f64,
                    y: index as f64,
                })
                .collect(),
        }],
    }
}

fn test_analysis_provenance() -> AnalysisProvenance {
    AnalysisProvenance {
        source_dataset_id: "test-dataset".to_owned(),
        source_dataset: "test-dataset".to_owned(),
        native_format: "mirante4d-v1".to_owned(),
        native_schema_version: 1,
        app_version: "0.1.0-test".to_owned(),
        created_at_utc: "test-clock".to_owned(),
        source_layer_id: "ch0".to_owned(),
        timepoint_start: 0,
        timepoint_end_exclusive: 1,
        scale_level: 0,
        operation: "test".to_owned(),
        operation_version: 1,
        parameters: BTreeMap::new(),
        scope: "test".to_owned(),
        execution_class: AnalysisExecutionClass::RoiLocalExact,
        result_state: AnalysisResultState::Complete,
        data_source: "test".to_owned(),
        compute_precision: "f64".to_owned(),
    }
}

fn write_float32_app_fixture(root: &std::path::Path) -> PathBuf {
    let package = root.join("app-f32.m4d");
    let shape = Shape4D::new(1, 4, 4, 4).unwrap();
    let display = LayerDisplay::new(
        true,
        mirante4d_core::DisplayWindow::new(-2.0, 8.0).unwrap(),
        1.0,
    )
    .unwrap();
    write_native_f32_dataset(
        &package,
        NativeF32Dataset {
            id: "app-f32".to_owned(),
            name: "App Float32 Fixture".to_owned(),
            world_space: mirante4d_core::WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_core::WorldUnit::Micrometer,
            },
            layers: vec![DenseF32Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [1.0, 1.0, 1.0, 1.0],
                },
                shape,
                brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                grid_to_world: GridToWorld::scale_um(1.0, 1.0, 1.0),
                display,
                values_tzyx: {
                    let mut values = vec![2.25; shape.element_count().unwrap() as usize];
                    values[0] = -1.5;
                    values
                },
            }],
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();
    package
}

fn write_uint8_no_data_app_fixture(root: &std::path::Path) -> PathBuf {
    let input_file = root.join("app-u8-nodata-source.tif");
    write_app_u8_stack_with_no_data_corner(&input_file).unwrap();
    let output = root.join("app-u8-nodata.m4d");
    let source = TiffImportSource::SingleFile(input_file);
    let inspection = inspect_tiff_source_for_review(&source).unwrap();
    let mut reviewed_plan = accepted_tiff_reviewed_import_plan(&inspection, [1.0, 1.0, 1.0], true);
    reviewed_plan.no_data_policy = Some(TiffNoDataPolicyReview {
        source_dtype: IntensityDType::Uint8,
        source_value_uint8: 255,
    });
    mirante4d_import::import_tiff_source(TiffSourceImportOptions {
        source,
        output_package: output.clone(),
        dataset_id: "app-u8-nodata".to_owned(),
        dataset_name: "App U8 No Data".to_owned(),
        voxel_spacing_um: [1.0, 1.0, 1.0],
        channel_metadata: BTreeMap::new(),
        file_grouping: None,
        existing_policy: ExistingPackagePolicy::Fail,
        storage: Default::default(),
        reviewed_plan,
    })
    .unwrap();
    output
}

fn write_app_u8_stack_with_no_data_corner(path: &Path) -> Result<(), tiff::TiffError> {
    let file = File::create(path)?;
    let mut encoder = TiffEncoder::new(file)?;
    for z in 0..3 {
        let values = (0..3)
            .flat_map(|y| {
                (0..3).map(move |x| {
                    if z == 0 && y == 0 && x == 0 {
                        255
                    } else {
                        (z * 9 + y * 3 + x) as u8
                    }
                })
            })
            .collect::<Vec<_>>();
        encoder.write_image::<colortype::Gray8>(3, 3, &values)?;
    }
    Ok(())
}

#[test]
fn app_shell_opens_fixture_and_renders_first_frame() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();

    let mut state = open_dataset_and_render_first_frame(root).unwrap();

    assert_eq!(state.dataset_name, "Basic uint16 16 cube fixture");
    assert_eq!(state.layer_count, 1);
    assert_eq!(state.viewer_layout.layout(), ViewerLayout::Single3d);
    assert!(state.viewer_layout.is_single_3d());
    assert!(!state.viewer_layout.has_four_panel_runtime());
    assert!(state.viewer_layout.four_panel_runtime().is_none());
    let expected_cross_section_center =
        state
            .active_source_grid_to_world
            .transform_point(DVec3::new(
                (state.active_source_shape.x.saturating_sub(1)) as f64 * 0.5,
                (state.active_source_shape.y.saturating_sub(1)) as f64 * 0.5,
                (state.active_source_shape.z.saturating_sub(1)) as f64 * 0.5,
            ));
    assert_eq!(
        state.viewer_layout.cross_section.center_world,
        expected_cross_section_center
    );
    assert_eq!(state.active_projection, Projection::Orthographic);
    assert_eq!(state.camera.projection, Projection::Orthographic);
    assert_eq!(state.active_render_mode, RenderMode::Mip);
    assert_eq!(state.active_layer_display, default_u16_display());
    assert_eq!(state.render_backend, RenderBackend::CpuReference);
    assert_eq!(state.frame.width, TEST_INITIAL_RENDER_VIEWPORT_SIDE);
    assert_eq!(state.frame.height, TEST_INITIAL_RENDER_VIEWPORT_SIDE);
    assert!(state.diagnostics.nonzero_pixels > 0);
    assert_eq!(state.active_intensity_summary.voxel_count, 4096);
    assert_eq!(state.active_intensity_summary.max, 4125);
    assert_eq!(state.visible_brick_count, 1);
    assert_eq!(state.visible_brick_plan_stride, 1);
    assert!(state.visible_brick_plan_error.is_none());

    state.viewer_layout.switch_to_four_panel();
    let panels = state.viewer_layout.four_panel_runtime().unwrap().panels();
    assert_eq!(
        panels.iter().map(|panel| panel.panel_id).collect::<Vec<_>>(),
        vec![PanelId::Xy, PanelId::Xz, PanelId::ThreeD, PanelId::Yz]
    );
    assert_eq!(
        panels.iter().map(|panel| panel.kind).collect::<Vec<_>>(),
        vec![
            PanelKind::CrossSectionXy,
            PanelKind::CrossSectionXz,
            PanelKind::ThreeD,
            PanelKind::CrossSectionYz,
        ]
    );

    state.viewer_layout.switch_to_single_3d();
    assert!(state.viewer_layout.is_single_3d());
    assert!(!state.viewer_layout.has_four_panel_runtime());
}

#[test]
fn app_shell_opens_float32_dataset_and_preserves_exact_pick_readout() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_float32_app_fixture(tempdir.path());

    let state = open_dataset_and_render_first_frame(&root).unwrap();

    assert_eq!(state.dataset_name, "App Float32 Fixture");
    assert_eq!(state.active_layer_dtype, IntensityDType::Float32);
    assert_eq!(state.active_projection, Projection::Orthographic);
    assert_eq!(state.render_backend, RenderBackend::CpuReference);
    assert_eq!(state.frame.width, TEST_INITIAL_RENDER_VIEWPORT_SIDE);
    assert_eq!(state.frame.height, TEST_INITIAL_RENDER_VIEWPORT_SIDE);
    assert!(state.frame_f32.is_some());
    assert!(state.diagnostics_f32.is_some());
    assert!(state.frame_f32.as_ref().unwrap().pixels().contains(&2.25));
    assert!(state.diagnostics_f32.unwrap().nonzero_pixels > 0);

    let color = color_image_for_state(&state);
    assert_eq!(color.size, [TEST_INITIAL_RENDER_VIEWPORT_SIDE as usize; 2]);
    assert!(color.pixels.iter().any(|pixel| pixel.a() != 0));

    let hit = pick_hit_from_viewport_hover(
        &state,
        ViewportHover {
            x: state.frame.width / 2,
            y: state.frame.height / 2,
            intensity: ViewportIntensity::F32(2.25),
        },
    )
    .unwrap();

    assert_eq!(hit.kind, PickHitKind::Voxel);
    assert_eq!(hit.value, Some(PickValue::IntensityF32(2.25)));
    assert_eq!(hit.policy, PickPolicy::MipArgmax);
    assert_eq!(hit.completeness, PickCompleteness::Exact);
}

#[test]
fn app_streams_float32_bricks_into_cpu_resident_renderer() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_float32_app_fixture(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    let dense_frame = state.frame_f32.clone().unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 32).unwrap();

    let submission = submit_visible_bricks_to_pool(&mut state, &pool);
    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), state.visible_brick_count);
    for _ in 0..submission.current_tickets.len() {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.priority, BrickRequestPriority::CurrentFrame);
        assert!(apply_brick_read_outcome(&mut state, outcome));
    }
    render_state_from_resident_bricks(&mut state).unwrap();

    assert_eq!(state.render_backend, RenderBackend::CpuResidentBricks);
    assert_eq!(state.resident_bricks.len(), 0);
    assert_eq!(state.resident_bricks_f32.len(), state.visible_brick_count);
    assert_eq!(state.resident_bricks_f32_by_layer.len(), 1);
    assert!(state.brick_stream_complete);
    assert_eq!(
        state.frame_f32.as_ref().unwrap().pixels(),
        dense_frame.pixels()
    );
    assert_eq!(
        state.frame.pixels(),
        f32_frame_to_display_u16(&dense_frame, state.active_layer_display).pixels()
    );
}

#[test]
fn f32_iso_display_conversion_preserves_surface_payload() {
    let display = LayerDisplay::new(true, DisplayWindow::new(0.0, 4.0).unwrap(), 1.0).unwrap();
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
        f32_frame_to_display_u16_for_mode(&frame, RenderMode::Isosurface, display).unwrap();

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
fn f32_dvr_display_conversion_uses_normalized_alpha_not_source_window() {
    let display = LayerDisplay::new(true, DisplayWindow::new(97.0, 111.0).unwrap(), 1.0).unwrap();
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

    let converted = f32_frame_to_display_u16_for_mode(&frame, RenderMode::Dvr, display).unwrap();

    assert_eq!(converted.pixels(), &[32768, 0]);
    assert_eq!(converted.coverage(), &PixelCoverage::Mask(vec![1, 0]));
    assert_eq!(converted.dvr_rgba(), Some(&dvr_rgba));
}

#[test]
fn iso_placeholder_frame_carries_empty_surface_payload() {
    let viewport = RenderViewport::new(2, 1).unwrap();

    let frame = placeholder_frame_for_mode(viewport, RenderMode::Isosurface);

    assert_eq!(frame.pixels(), &[0, 0]);
    assert_eq!(frame.coverage(), &PixelCoverage::Mask(vec![0, 0]));
    let surface = frame.iso_surface().unwrap();
    assert_eq!(surface.source_values(), &[0, 0]);
    assert_eq!(surface.coverage(), &PixelCoverage::Mask(vec![0, 0]));
    assert!(!surface.is_covered_index(0));
    assert!(!surface.is_covered_index(1));
}

#[test]
fn full_time_series_analysis_uses_exact_float32_source_values() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_float32_app_fixture(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();

    compute_full_time_series_analysis(&mut state).unwrap();

    let table = state.analysis_tables.last().unwrap();
    let row = table.rows.first().unwrap();
    let expected_sum = -1.5 + (63.0 * 2.25);
    let expected_mean = expected_sum / 64.0;
    assert_eq!(
        table.provenance.parameters.get("compute_path"),
        Some(&"cpu_exact_f32_brick_streaming_reference".to_owned())
    );
    assert_eq!(
        table.provenance.compute_precision,
        "CPU f64 accumulation over source float32 values"
    );
    assert_eq!(state.analysis_operations.len(), 1);
    assert_eq!(
        state.analysis_operations[0].kind,
        AnalysisOperationKind::FullIntensitySummary
    );
    assert_eq!(
        table
            .columns
            .iter()
            .find(|column| column.key == "sum")
            .and_then(|column| column.unit.as_deref()),
        Some("float32")
    );
    assert_eq!(
        row.cells.get("voxel_count"),
        Some(&AnalysisCell::Integer(64))
    );
    assert_eq!(
        row.cells.get("nonzero_count"),
        Some(&AnalysisCell::Integer(64))
    );
    assert_eq!(row.cells.get("min"), Some(&AnalysisCell::Float(-1.5)));
    assert_eq!(row.cells.get("max"), Some(&AnalysisCell::Float(2.25)));
    assert_eq!(
        row.cells.get("sum"),
        Some(&AnalysisCell::Float(expected_sum))
    );
    assert_eq!(
        row.cells.get("mean"),
        Some(&AnalysisCell::Float(expected_mean))
    );
    assert_eq!(
        state.analysis_plots.last().unwrap().series[0].points[0].y,
        expected_mean
    );
}

#[test]
fn full_time_series_analysis_records_no_data_policy_and_exclusion() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_uint8_no_data_app_fixture(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();

    compute_full_time_series_analysis(&mut state).unwrap();

    let table = state.analysis_tables.last().unwrap();
    let row = table.rows.first().unwrap();
    let policy = "value 255 (uint8), dilated 1 voxel".to_owned();
    assert_eq!(
        active_layer_no_data_policy_label(&state),
        Some(policy.clone())
    );
    assert_eq!(
        table.provenance.parameters.get("no_data_policy"),
        Some(&policy)
    );
    assert_eq!(
        table.provenance.parameters.get("invalid_voxels"),
        Some(&"render_invalid_excluded".to_owned())
    );
    assert_eq!(
        row.cells.get("voxel_count"),
        Some(&AnalysisCell::Integer(19))
    );
    assert_eq!(
        row.cells.get("geometric_voxel_count"),
        Some(&AnalysisCell::Integer(27))
    );
    assert_eq!(row.cells.get("max"), Some(&AnalysisCell::Integer(26)));

    let operation = state.analysis_operations.last().unwrap();
    assert_eq!(
        operation.parameters.get("no_data_policy"),
        Some(&AnalysisParameterValue::Text(policy))
    );
    assert_eq!(
        operation.parameters.get("invalid_voxels"),
        Some(&AnalysisParameterValue::Text(
            "render_invalid_excluded".to_owned()
        ))
    );
}

#[test]
fn roi_analysis_uses_exact_float32_source_values() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_float32_app_fixture(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    let roi = RoiArtifact::new(
        SceneArtifactId::new("roi", "single-f32-voxel").unwrap(),
        "single f32 voxel",
        AnalysisWorldGeometry::Box3D {
            min: DVec3::new(0.0, 0.0, 0.0),
            max: DVec3::new(1.0, 1.0, 1.0),
        },
        SceneArtifactTime::Static,
    )
    .unwrap();
    state
        .scene_artifacts
        .apply(SceneEditCommand::PutRoi { artifact: roi })
        .unwrap();

    compute_active_roi_analysis(&mut state).unwrap();

    let table = state.analysis_tables.last().unwrap();
    let row = table.rows.first().unwrap();
    assert_eq!(
        table.provenance.parameters.get("compute_path"),
        Some(&"cpu_exact_f32_region_streaming_reference".to_owned())
    );
    assert_eq!(state.analysis_operations.len(), 1);
    assert_eq!(
        state.analysis_operations[0].kind,
        AnalysisOperationKind::RoiIntensityStatistics
    );
    assert_eq!(
        table
            .columns
            .iter()
            .find(|column| column.key == "min")
            .and_then(|column| column.unit.as_deref()),
        Some("float32")
    );
    assert_eq!(
        row.cells.get("roi_id"),
        Some(&AnalysisCell::Text("single-f32-voxel".to_owned()))
    );
    assert_eq!(
        row.cells.get("voxel_count"),
        Some(&AnalysisCell::Integer(1))
    );
    assert_eq!(row.cells.get("min"), Some(&AnalysisCell::Float(-1.5)));
    assert_eq!(row.cells.get("max"), Some(&AnalysisCell::Float(-1.5)));
    assert_eq!(row.cells.get("sum"), Some(&AnalysisCell::Float(-1.5)));
    assert_eq!(row.cells.get("mean"), Some(&AnalysisCell::Float(-1.5)));
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn app_backend_uses_gpu_float32_full_time_series_analysis() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_float32_app_fixture(tempdir.path());
    let state = open_dataset_and_render_first_frame(&root).unwrap();
    let renderer = Arc::new(GpuRenderer::new_blocking().unwrap());
    let context = AnalysisJobContext::from_state(&state, Some(renderer));

    let output =
        run_full_time_series_analysis_job(&context, &CancellationToken::new(), |_| Ok(())).unwrap();
    let row = output.table.rows.first().unwrap();
    let expected_sum = -1.5 + (63.0 * 2.25);
    let expected_mean = expected_sum / 64.0;

    assert_eq!(
        output.table.provenance.parameters.get("compute_path"),
        Some(&"cpu_exact_f32_brick_streaming_reference".to_owned())
    );
    assert_eq!(
        output.table.provenance.compute_precision,
        "CPU f64 accumulation over source float32 values"
    );
    assert_eq!(
        row.cells.get("sum"),
        Some(&AnalysisCell::Float(expected_sum))
    );
    assert_eq!(
        row.cells.get("mean"),
        Some(&AnalysisCell::Float(expected_mean))
    );
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn app_backend_uses_gpu_float32_roi_analysis() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_float32_app_fixture(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    let roi = RoiArtifact::new(
        SceneArtifactId::new("roi", "single-f32-voxel").unwrap(),
        "single f32 voxel",
        AnalysisWorldGeometry::Box3D {
            min: DVec3::new(0.0, 0.0, 0.0),
            max: DVec3::new(1.0, 1.0, 1.0),
        },
        SceneArtifactTime::Static,
    )
    .unwrap();
    state
        .scene_artifacts
        .apply(SceneEditCommand::PutRoi { artifact: roi })
        .unwrap();
    let renderer = Arc::new(GpuRenderer::new_blocking().unwrap());
    let context = AnalysisJobContext::from_state(&state, Some(renderer));

    let output =
        run_roi_intensity_analysis_job(&context, &CancellationToken::new(), |_| Ok(())).unwrap();
    let row = output.table.rows.first().unwrap();

    assert_eq!(
        output.table.provenance.parameters.get("compute_path"),
        Some(&"cpu_exact_f32_region_streaming_reference".to_owned())
    );
    assert_eq!(
        output.table.provenance.compute_precision,
        "CPU f64 accumulation over source float32 ROI region values"
    );
    assert_eq!(row.cells.get("min"), Some(&AnalysisCell::Float(-1.5)));
    assert_eq!(row.cells.get("max"), Some(&AnalysisCell::Float(-1.5)));
    assert_eq!(row.cells.get("sum"), Some(&AnalysisCell::Float(-1.5)));
    assert_eq!(row.cells.get("mean"), Some(&AnalysisCell::Float(-1.5)));
}

#[test]
fn workbench_shell_exposes_primary_regions_at_high_dpi() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();

    let harness = Harness::builder()
        .with_size(egui::vec2(1440.0, 900.0))
        .with_pixels_per_point(2.0)
        .build_eframe(|cc| MiranteWorkbenchApp::new(cc, state));

    harness.get_by_label("Mirante4D");
    harness.get_by_label("Dataset");
    harness.get_by_label("Layers");
    harness.get_by_label("Inspector");
    harness.get_by_label("Viewer Tools");
    harness.get_by_label("Analysis");
    harness.get_by_label("Workspace");
    harness.get_by_label("Runtime Diagnostics");
    harness.get_by_label("Fit Data");
    harness.get_by_label("Reset View");
    harness.get_by_label("ready");
}

#[test]
fn workbench_shell_exposes_four_panel_layout_shell() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.viewer_layout.switch_to_four_panel();

    let harness = Harness::builder()
        .with_size(egui::vec2(1440.0, 900.0))
        .with_pixels_per_point(1.0)
        .build_eframe(|cc| MiranteWorkbenchApp::new(cc, state));

    harness.get_by_label("Layout");
    harness.get_by_label("4 Panel");
    harness.get_by_label("XY");
    harness.get_by_label("XZ");
    assert!(harness.get_all_by_label("3D").count() >= 2);
    harness.get_by_label("YZ");
    harness.get_by_label("XY cross-section panel");
    harness.get_by_label("XZ cross-section panel");
    harness.get_by_label("YZ cross-section panel");
}

#[test]
fn workbench_shell_handles_long_dataset_name_in_narrow_layout() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.dataset_name =
        "Very long microscopy dataset name with acquisition metadata and timepoint descriptors"
            .to_owned();
    state.last_workflow_message =
        Some("Loaded dataset with a deliberately long status message for layout coverage".to_owned());

    let harness = Harness::builder()
        .with_size(egui::vec2(520.0, 360.0))
        .with_pixels_per_point(1.5)
        .build_eframe(|cc| MiranteWorkbenchApp::new(cc, state));

    harness.get_by_label("Mirante4D");
    harness.get_by_label("Dataset");
    harness.get_by_label("Layers");
    harness.get_by_label("Inspector");
    harness.get_by_label("Fit Data");
    harness.get_by_label("Reset View");
    harness.get_by_label("ready");
}

#[test]
fn workbench_runtime_diagnostics_exposes_data_runtime_budgets() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    app.brick_read_pool = Some(BrickReadPool::new(app.state.dataset.clone(), 3, 7).unwrap());
    app.state.viewer_layout.switch_to_four_panel();

    let harness = Harness::builder()
        .with_size(egui::vec2(1440.0, 900.0))
        .with_pixels_per_point(1.0)
        .build_ui(|ui| {
            ui_kit::configure_visuals(ui.ctx());
            app.show_runtime_diagnostics_body(ui);
        });

    harness.get_by_label("volume cache budget");
    harness.get_by_label("brick cache budget");
    harness.get_by_label("upload staging budget");
    harness.get_by_label("decoded in-flight budget");
    harness.get_by_label("payload bytes");
    harness.get_by_label("brick workers");
    harness.get_by_label("brick queue capacity");
    harness.get_by_label("brick queue depth");
    harness.get_by_label("2D panel XY");
    harness.get_by_label("2D panel XZ");
    harness.get_by_label("2D panel YZ");
    harness.get_by_label("2D global runtime");
    harness.get_by_label("2D chunk states");
}

#[test]
fn app_preferences_round_trip_and_apply_runtime_config() {
    let tempdir = tempfile::tempdir().unwrap();
    let preferences_path = tempdir.path().join("config/preferences.json");
    let preferences = AppPreferences {
        format: PREFERENCES_FORMAT.to_owned(),
        runtime: AppRuntimePreferences {
            volume_cache_budget_bytes: 32 * APP_MIB,
            brick_cache_budget_bytes: 64 * APP_MIB,
            gpu_volume_cache_budget_bytes: 128 * APP_MIB,
            gpu_brick_cache_budget_bytes: 256 * APP_MIB,
        },
    };

    write_app_preferences(&preferences_path, &preferences).unwrap();

    assert_eq!(
        load_app_preferences(&preferences_path).unwrap(),
        preferences
    );

    let dataset_root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state =
        open_dataset_with_preferences_and_render_first_frame(dataset_root, &preferences).unwrap();
    assert_eq!(
        state.dataset.diagnostics().unwrap().config,
        preferences.runtime_config(),
    );
}

#[test]
fn workbench_settings_panel_exposes_runtime_budget_controls() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    app.preferences_path = Some(tempdir.path().join("preferences.json"));

    let harness = Harness::builder()
        .with_size(egui::vec2(640.0, 360.0))
        .with_pixels_per_point(1.0)
        .build_ui(|ui| {
            ui_kit::configure_visuals(ui.ctx());
            app.show_settings_body(ui);
        });

    harness.get_by_label("volume cache MiB");
    harness.get_by_label("brick cache MiB");
    harness.get_by_label("GPU dense cache MiB");
    harness.get_by_label("GPU brick cache MiB");
    harness.get_by_label("Save Settings");
    harness.get_by_label("Reset Settings");
    harness.get_by_label("settings file");
}

#[test]
fn workbench_settings_save_persists_runtime_preferences() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let preferences_path = tempdir.path().join("config/preferences.json");
    let mut app = test_workbench_app_without_background_runtime(state);
    app.preferences_path = Some(preferences_path.clone());
    app.settings_runtime_draft = AppRuntimePreferences {
        volume_cache_budget_bytes: 48 * APP_MIB,
        brick_cache_budget_bytes: 96 * APP_MIB,
        gpu_volume_cache_budget_bytes: 192 * APP_MIB,
        gpu_brick_cache_budget_bytes: 384 * APP_MIB,
    };

    app.save_preferences_from_settings();

    let loaded = load_app_preferences(&preferences_path).unwrap();
    assert_eq!(loaded.runtime, app.settings_runtime_draft);
    assert_eq!(app.preferences.runtime, app.settings_runtime_draft);
    assert_eq!(app.settings_message.as_deref(), Some("saved"));
    assert_eq!(
        app.state.last_workflow_message.as_deref(),
        Some("Saved settings"),
    );
}

#[test]
fn project_dirty_snapshot_tracks_viewer_changes_and_successful_save() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);

    assert!(!app.project_dirty());

    app.state.camera.orthographic_world_per_screen_point *= 1.25;

    assert!(app.project_dirty());

    let project_path = tempdir.path().join("viewer-state.m4dproj");
    assert!(app.save_project_to_path(project_path.clone()));

    assert_eq!(app.current_project_path, Some(project_path));
    assert!(!app.project_dirty());

    app.state.viewer_layout.switch_to_four_panel();

    assert!(app.project_dirty());

    let project_path = tempdir.path().join("viewer-layout-state.m4dproj");
    assert!(app.save_project_to_path(project_path.clone()));

    assert_eq!(app.current_project_path, Some(project_path));
    assert!(!app.project_dirty());

    app.state
        .viewer_layout
        .cross_section
        .pan_by_panel_points(CrossSectionPanel::Xz, 8.0, -4.0);

    assert!(app.project_dirty());
}

#[test]
fn dirty_project_close_prompt_exposes_save_discard_and_cancel() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    app.close_prompt_open = true;

    let harness = Harness::builder()
        .with_size(egui::vec2(480.0, 240.0))
        .with_pixels_per_point(1.0)
        .build_ui(|ui| {
            ui_kit::configure_visuals(ui.ctx());
            app.show_dirty_project_close_prompt(ui.ctx());
        });

    harness.get_by_label("Unsaved Project");
    harness.get_by_label("Project changes have not been saved.");
    harness.get_by_label("Save");
    harness.get_by_label("Discard");
    harness.get_by_label("Cancel");
}

#[test]
fn tiff_import_setup_state_is_visible_immediately_after_output_selection() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let source = tempdir.path().join("raw.tif");
    let output_parent = tempdir.path().join("output");
    fs::create_dir(&output_parent).unwrap();
    let (_sender, receiver) = mpsc::channel();
    let mut app = test_workbench_app_without_background_runtime(state);

    app.enter_tiff_import_setup_waiting_state(
        TiffImportSource::SingleFile(source.clone()),
        output_parent.clone(),
        receiver,
    );

    let task = app.tiff_import_setup_task.as_ref().unwrap();
    assert_eq!(task.source.path(), source.as_path());
    assert_eq!(task.output_parent, output_parent);
    assert!(app.pending_tiff_import.is_none());
    assert!(app.tiff_import_setup_error.is_none());
    assert_eq!(
        app.state.last_workflow_message.as_deref(),
        Some("Inspecting TIFF input before package creation")
    );
}

#[test]
fn tiff_import_setup_window_exposes_inspection_state() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let source = tempdir.path().join("raw.tif");
    let output_parent = tempdir.path().join("output");
    fs::create_dir(&output_parent).unwrap();
    let (_sender, receiver) = mpsc::channel();
    let mut app = test_workbench_app_without_background_runtime(state);
    app.enter_tiff_import_setup_waiting_state(
        TiffImportSource::SingleFile(source),
        output_parent,
        receiver,
    );
    let mut start_pending_tiff_import = false;
    let mut cancel_pending_tiff_import = false;
    let mut dismiss_setup_error = false;

    {
        let harness = Harness::builder()
            .with_size(egui::vec2(960.0, 640.0))
            .with_pixels_per_point(1.0)
            .build_ui(|ui| {
                ui_kit::configure_visuals(ui.ctx());
                app.show_tiff_import_setup_window(
                    ui.ctx(),
                    &mut start_pending_tiff_import,
                    &mut cancel_pending_tiff_import,
                    &mut dismiss_setup_error,
                );
            });

        harness.get_by_label("TIFF Import");
        harness.get_by_label("inspecting input");
        harness.get_by_label("output parent");
        harness.get_by_label("package");
        harness.get_by_label("created after review");
        harness.get_by_label("Cancel Setup");
    }
    assert!(!start_pending_tiff_import);
    assert!(!cancel_pending_tiff_import);
    assert!(!dismiss_setup_error);
}

#[test]
fn analysis_workspace_window_exposes_selected_results() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    compute_full_time_series_analysis(&mut state).unwrap();

    let harness = Harness::builder()
        .with_size(egui::vec2(1440.0, 900.0))
        .with_pixels_per_point(1.0)
        .build_eframe(|cc| {
            let mut app = MiranteWorkbenchApp::new(cc, state);
            app.analysis_workspace_open = true;
            app
        });

    harness.get_by_label("Analysis Workspace");
}

#[test]
fn display_size_maps_to_physical_render_viewport() {
    let viewport = render_viewport_for_display_size(egui::vec2(640.2, 360.2), 2.0, 2048).unwrap();

    assert_eq!(viewport, RenderViewport::new(1280, 720).unwrap());
    assert!(render_viewport_for_display_size(egui::Vec2::ZERO, 2.0, 2048).is_none());
    assert!(render_viewport_for_display_size(egui::vec2(640.0, 360.0), 0.0, 2048).is_none());
    assert!(render_viewport_for_display_size(egui::vec2(640.0, 360.0), 2.0, 0).is_none());
}

#[test]
fn display_size_clamps_to_max_texture_side_without_changing_aspect() {
    let viewport = render_viewport_for_display_size(egui::vec2(1000.0, 2000.0), 2.0, 2048).unwrap();

    assert_eq!(viewport, RenderViewport::new(1024, 2048).unwrap());
}

#[test]
fn app_renders_to_explicit_viewport_not_volume_xy_size() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();

    assert_eq!(
        crate::viewport::default_render_viewport_for_shape(state.active_source_shape).unwrap(),
        RenderViewport::new(512, 512).unwrap(),
        "the production initial viewport remains unchanged"
    );
    assert_eq!(state.frame.width, TEST_INITIAL_RENDER_VIEWPORT_SIDE);
    assert_eq!(state.frame.height, TEST_INITIAL_RENDER_VIEWPORT_SIDE);
    assert!(set_render_viewport(
        &mut state,
        RenderViewport::new(80, 45).unwrap()
    ));
    rerender_state_with_backend(&mut state, None).unwrap();

    assert_eq!(state.frame.width, 80);
    assert_eq!(state.frame.height, 45);
    assert_eq!(state.diagnostics.output_pixels, 80 * 45);
}

#[test]
fn app_shell_exposes_time_and_channel_metadata() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();

    let state = open_dataset_and_render_first_frame(root).unwrap();

    assert_eq!(state.layer_count, 2);
    assert_eq!(state.layers.len(), 2);
    assert_eq!(state.active_layer_index, 0);
    assert_eq!(state.active_layer_id, "ch0");
    assert_eq!(state.layers[0].display, default_u16_display());
    assert_eq!(state.active_timepoint, TimeIndex(0));
    assert_eq!(state.timepoint_count, 3);
    assert_eq!(state.frame.width, TEST_INITIAL_RENDER_VIEWPORT_SIDE);
    assert_eq!(state.frame.height, TEST_INITIAL_RENDER_VIEWPORT_SIDE);
    assert_eq!(state.visible_brick_count, 1);
}

#[test]
fn app_can_switch_channel_and_timepoint_and_rerender() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let initial_pixels = state.frame.pixels().to_vec();

    activate_layer_timepoint(&mut state, 1, TimeIndex(2)).unwrap();

    assert_eq!(state.active_layer_index, 1);
    assert_eq!(state.active_layer_id, "ch1");
    assert_eq!(state.active_layer_display, state.layers[1].display);
    assert_eq!(state.active_timepoint, TimeIndex(2));
    assert_eq!(state.timepoint_count, 3);
    assert_ne!(state.frame.pixels(), initial_pixels.as_slice());
    assert!(state.diagnostics.max_value > 20_000);
    assert_eq!(state.visible_brick_count, 1);
    assert!(state.visible_brick_plan_error.is_none());
}

#[test]
fn layer_display_state_update_syncs_active_and_rendered_channel_state() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let display =
        LayerDisplay::new(true, DisplayWindow::new(100.0, 2_000.0).unwrap(), 0.35).unwrap();
    let color = ChannelColor::new([0.25, 0.5, 1.0, 0.75]).unwrap();

    assert!(set_layer_display_state(&mut state, 0, display, color).unwrap());

    assert_eq!(state.layers[0].display, display);
    assert_eq!(state.layers[0].color, color);
    assert_eq!(state.active_layer_display, display);
    assert_eq!(state.active_layer_color, color);
    assert_eq!(state.rendered_channels[0].transfer.display, display);
    assert_eq!(state.rendered_channels[0].transfer.color, color);
}

#[test]
fn layer_invert_update_syncs_active_and_rendered_channel_state() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();

    assert!(set_layer_transfer_invert(&mut state, 0, true).unwrap());

    assert!(state.layers[0].invert);
    assert!(state.active_layer_transfer.invert);
    assert!(state.rendered_channels[0].transfer.invert);

    let display =
        LayerDisplay::new(true, DisplayWindow::new(100.0, 2_000.0).unwrap(), 0.35).unwrap();
    let color = ChannelColor::new([0.25, 0.5, 1.0, 0.75]).unwrap();
    assert!(set_layer_display_state(&mut state, 0, display, color).unwrap());

    assert!(state.layers[0].invert);
    assert!(state.active_layer_transfer.invert);
    assert!(state.rendered_channels[0].transfer.invert);
}

#[test]
fn hidden_non_active_layer_is_removed_from_stream_layer_set() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let hidden = LayerDisplay::new(false, state.layers[1].display.window, 1.0).unwrap();
    let color = state.layers[1].color;

    set_layer_display_state(&mut state, 1, hidden, color).unwrap();
    let layer_ids = stream_layer_ids_for_state(&state).unwrap();

    assert_eq!(layer_ids, vec![LayerId::new("ch0").unwrap()]);
}

#[test]
fn hidden_active_layer_is_removed_from_stream_layer_set() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let hidden = LayerDisplay::new(false, state.layers[0].display.window, 1.0).unwrap();
    let color = state.layers[0].color;

    set_layer_display_state(&mut state, 0, hidden, color).unwrap();
    let layer_ids = stream_layer_ids_for_state(&state).unwrap();

    assert_eq!(state.active_layer_id, "ch0");
    assert_eq!(layer_ids, vec![LayerId::new("ch1").unwrap()]);
}

#[test]
fn hidden_active_layer_does_not_block_visible_channel_resident_rendering() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let hidden = LayerDisplay::new(false, state.layers[0].display.window, 1.0).unwrap();
    let color = state.layers[0].color;
    set_layer_display_state(&mut state, 0, hidden, color).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 2).unwrap();

    let submission = submit_visible_bricks_to_pool(&mut state, &pool);

    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 1);
    assert_eq!(submission.prefetch_tickets.len(), 1);
    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(outcome.layer_id.as_str(), "ch1");
    assert_eq!(outcome.priority, BrickRequestPriority::CurrentFrame);
    assert!(apply_brick_read_outcome(&mut state, outcome));
    assert!(state.brick_stream_complete);

    render_state_from_resident_bricks(&mut state).unwrap();

    assert_eq!(state.rendered_channels.len(), 1);
    assert_eq!(state.rendered_channels[0].layer_id, "ch1");
    assert_eq!(state.frame.pixels().iter().copied().max(), Some(0));
    assert!(
        color_image_for_state(&state)
            .pixels
            .iter()
            .any(|pixel| pixel.a() != 0)
    );
    assert_eq!(
        state
            .channel_fidelity
            .iter()
            .find(|channel| channel.layer_id == "ch0")
            .unwrap()
            .resident_bricks,
        0
    );
    assert_eq!(
        state
            .channel_fidelity
            .iter()
            .find(|channel| channel.layer_id == "ch1")
            .unwrap()
            .resident_bricks,
        1
    );
}

#[test]
fn hidden_active_layer_suppresses_intensity_pick_readout() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let hidden = LayerDisplay::new(false, state.layers[0].display.window, 1.0).unwrap();
    let color = state.layers[0].color;
    set_layer_display_state(&mut state, 0, hidden, color).unwrap();
    let hover = ViewportHover {
        x: state.frame.width / 2,
        y: state.frame.height / 2,
        intensity: ViewportIntensity::U16(123),
    };

    let hidden_pick = pick_hit_from_viewport_hover(&state, hover).unwrap();

    assert_eq!(hidden_pick.kind, PickHitKind::Empty);
    assert_eq!(hidden_pick.value, None);
    assert_eq!(hidden_pick.source_layer_id, None);
}

#[test]
fn channel_display_preset_applies_full_transfer_and_rejects_stale_layer_identity() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let preset = ChannelDisplayPreset {
        preset_id: "phase14-test".to_owned(),
        name: "Phase 14 Test".to_owned(),
        entries: state
            .layers
            .iter()
            .enumerate()
            .map(|(index, layer)| ChannelDisplayPresetEntry {
                layer_id: layer.id.clone(),
                layer_name: layer.name.clone(),
                dvr_opacity_transfer: layer.dvr_opacity_transfer,
                render_state: layer.render_state,
                transfer: ChannelTransferFunction::new(
                    LayerDisplay::new(
                        index == 1,
                        DisplayWindow::new(10.0, 1000.0 + index as f32).unwrap(),
                        0.4 + index as f32 * 0.1,
                    )
                    .unwrap(),
                    fluorescence_palette_color(index),
                    TransferCurve::gamma(2.0).unwrap(),
                    TransferPresetId::new("bright_gamma").unwrap(),
                )
                .unwrap()
                .with_invert(index == 1),
            })
            .collect(),
    };
    let preset_index = state.channel_presets.len();
    state.channel_presets.push(preset);

    assert!(apply_channel_display_preset(&mut state, preset_index).unwrap());

    assert!(!state.layers[0].display.visible);
    assert!(state.layers[1].display.visible);
    assert_eq!(state.layers[1].curve, TransferCurve::gamma(2.0).unwrap());
    assert!(state.layers[1].invert);
    assert_eq!(
        state.active_layer_transfer.curve,
        TransferCurve::gamma(2.0).unwrap()
    );
    assert!(!state.active_layer_transfer.invert);

    let mut stale = state.channel_presets[preset_index].clone();
    stale.entries[0].layer_name = "renamed elsewhere".to_owned();
    let stale_index = state.channel_presets.len();
    state.channel_presets.push(stale);
    let err = apply_channel_display_preset(&mut state, stale_index).unwrap_err();

    assert!(err.to_string().contains("stale"));
    assert!(
        state
            .channel_preset_warnings
            .iter()
            .any(|warning| warning.contains("stale name"))
    );
}

#[test]
fn project_package_roundtrip_restores_layer_display_states() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    let ch0_display =
        LayerDisplay::new(true, DisplayWindow::new(25.0, 3_000.0).unwrap(), 0.5).unwrap();
    let ch0_color = ChannelColor::new([1.0, 0.25, 0.1, 0.8]).unwrap();
    let ch1_display =
        LayerDisplay::new(false, DisplayWindow::new(50.0, 7_000.0).unwrap(), 0.25).unwrap();
    let ch1_color = ChannelColor::new([0.1, 0.8, 1.0, 0.5]).unwrap();
    set_layer_display_state(&mut state, 0, ch0_display, ch0_color).unwrap();
    set_layer_display_state(&mut state, 1, ch1_display, ch1_color).unwrap();
    set_layer_transfer_curve(
        &mut state,
        1,
        TransferCurve::gamma(2.0).unwrap(),
        TransferPresetId::new("bright_gamma").unwrap(),
    )
    .unwrap();
    set_layer_transfer_invert(&mut state, 1, true).unwrap();
    activate_layer_timepoint(&mut state, 1, TimeIndex(2)).unwrap();
    state.iso_light_state = IsoLightState::detached_screen(0.25, -0.5).unwrap();
    let session_path = tempdir.path().join("display-state.m4dproj");

    write_session_file(&session_path, &session_from_state(&state)).unwrap();
    let decoded = read_session_file(&session_path).unwrap();
    let restored = open_state_from_session(&decoded, None).unwrap();

    assert_eq!(decoded.layer_display_states.len(), 2);
    assert_eq!(restored.layers[0].display, ch0_display);
    assert_eq!(restored.layers[0].color, ch0_color);
    assert_eq!(restored.layers[1].display, ch1_display);
    assert_eq!(restored.layers[1].color, ch1_color);
    assert_eq!(
        decoded.layer_display_states[1].transfer.curve,
        TransferCurve::gamma(2.0).unwrap()
    );
    assert!(decoded.layer_display_states[1].transfer.invert);
    assert_eq!(restored.active_layer_id, "ch1");
    assert_eq!(restored.active_layer_display, ch1_display);
    assert_eq!(restored.active_layer_color, ch1_color);
    assert_eq!(
        restored.active_layer_transfer.curve,
        TransferCurve::gamma(2.0).unwrap()
    );
    assert!(restored.active_layer_transfer.invert);
    assert_eq!(restored.active_timepoint, TimeIndex(2));
    assert_eq!(
        decoded.iso_light_state,
        IsoLightState::detached_screen(0.25, -0.5).unwrap()
    );
    assert_eq!(restored.iso_light_state, decoded.iso_light_state);
}

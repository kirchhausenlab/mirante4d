fn begin_test_analysis_operation(application: &mut ApplicationState) -> OperationToken {
    application
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
        .expect("the canonical application must admit the test analysis operation");
    application
        .drain_events(usize::MAX)
        .into_iter()
        .rev()
        .find_map(|event| match event {
            ApplicationEvent::OperationStarted { token }
                if token.kind() == OperationKind::Analysis =>
            {
                Some(token)
            }
            _ => None,
        })
        .expect("beginning analysis must emit its operation token")
}

fn complete_test_analysis_operation(
    application: &mut ApplicationState,
    analysis: &current_runtime::analysis::CurrentAnalysisRuntime,
    token: OperationToken,
    table_start: usize,
    plot_start: usize,
) {
    let tables = analysis.analysis_tables[table_start..]
        .iter()
        .enumerate()
        .map(|(slot, table)| {
            let slot = u16::try_from(slot).expect("test analysis table slots fit in u16");
            AnalysisTableDescriptor::new(
                AnalysisTableId::from_operation(token.operation_id(), slot),
                u64::try_from(table.rows.len()).expect("test table row counts fit in u64"),
            )
        })
        .collect();
    let plots = analysis.analysis_plots[plot_start..]
        .iter()
        .enumerate()
        .map(|(slot, plot)| {
            let slot = u16::try_from(slot).expect("test analysis plot slots fit in u16");
            AnalysisPlotDescriptor::new(
                AnalysisPlotId::from_operation(token.operation_id(), slot),
                plot.series
                    .iter()
                    .map(|series| {
                        u64::try_from(series.points.len())
                            .expect("test plot point counts fit in u64")
                    })
                    .collect(),
            )
            .expect("test analysis plots must satisfy descriptor bounds")
        })
        .collect();
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::AnalysisReady { tables, plots },
        })
        .expect("the canonical application must admit matching analysis descriptors");
}

fn compute_full_time_series_analysis_for_test(
    application: &mut ApplicationState,
    dataset: &current_runtime::dataset::CurrentDatasetRuntime,
    analysis: &mut current_runtime::analysis::CurrentAnalysisRuntime,
) -> anyhow::Result<()> {
    let token = begin_test_analysis_operation(application);
    let table_start = analysis.analysis_tables.len();
    let plot_start = analysis.analysis_plots.len();
    let snapshot = application.snapshot();
    let view = application_view(&snapshot);
    let layer = snapshot
        .catalog()
        .layer(view.active_layer())
        .expect("the canonical test view must close over its catalog");
    let layer_id = current_physical_layer_id(dataset, view.active_layer())?;
    compute_full_time_series_analysis(
        dataset,
        analysis,
        AnalysisJobInput {
            dataset_name: snapshot.catalog().label(),
            active_layer_id: layer_id.as_str(),
            active_layer_dtype: layer.dtype(),
            active_timepoint: view.timepoint(),
            timepoint_count: layer.shape().t(),
        },
    )?;
    complete_test_analysis_operation(application, analysis, token, table_start, plot_start);
    Ok(())
}

fn compute_active_roi_analysis_for_test(
    application: &mut ApplicationState,
    dataset: &current_runtime::dataset::CurrentDatasetRuntime,
    analysis: &mut current_runtime::analysis::CurrentAnalysisRuntime,
) -> anyhow::Result<()> {
    let token = begin_test_analysis_operation(application);
    let table_start = analysis.analysis_tables.len();
    let plot_start = analysis.analysis_plots.len();
    let snapshot = application.snapshot();
    let view = application_view(&snapshot);
    let layer = snapshot
        .catalog()
        .layer(view.active_layer())
        .expect("the canonical test view must close over its catalog");
    let layer_id = current_physical_layer_id(dataset, view.active_layer())?;
    compute_active_roi_analysis(
        dataset,
        analysis,
        AnalysisJobInput {
            dataset_name: snapshot.catalog().label(),
            active_layer_id: layer_id.as_str(),
            active_layer_dtype: layer.dtype(),
            active_timepoint: view.timepoint(),
            timepoint_count: layer.shape().t(),
        },
    )?;
    complete_test_analysis_operation(application, analysis, token, table_start, plot_start);
    Ok(())
}

fn analysis_job_context_for_test(
    application: &ApplicationState,
    dataset: &current_runtime::dataset::CurrentDatasetRuntime,
    analysis: &current_runtime::analysis::CurrentAnalysisRuntime,
) -> AnalysisJobContext {
    let snapshot = application.snapshot();
    let view = application_view(&snapshot);
    let layer = snapshot
        .catalog()
        .layer(view.active_layer())
        .expect("the canonical test view must close over its catalog");
    let layer_id = current_physical_layer_id(dataset, view.active_layer()).unwrap();
    AnalysisJobContext::from_runtime(
        dataset,
        analysis,
        AnalysisJobInput {
            dataset_name: snapshot.catalog().label(),
            active_layer_id: layer_id.as_str(),
            active_layer_dtype: layer.dtype(),
            active_timepoint: view.timepoint(),
            timepoint_count: layer.shape().t(),
        },
    )
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

fn analysis_scene_ui_runtime() -> current_runtime::ui::CurrentUiRuntime {
    current_runtime::ui::CurrentUiRuntime::new(ResourcePolicy::default(), None)
}

fn analysis_test_scene_artifacts() -> SceneArtifactStore {
    let mut store = SceneArtifactStore::default();
    let track = TrackArtifact::new(
        SceneArtifactId::new("track", "track-a").unwrap(),
        "track a",
        Some(LayerId::new("ch0").unwrap()),
        vec![
            TrackPoint::new(TimeIndex::new(0), DVec3::ZERO).unwrap(),
            TrackPoint::new(TimeIndex::new(1), DVec3::new(1.0, 0.0, 0.0)).unwrap(),
            TrackPoint::new(TimeIndex::new(3), DVec3::new(3.0, 0.0, 0.0)).unwrap(),
        ],
    )
    .unwrap();
    let roi = RoiArtifact::new(
        SceneArtifactId::new("roi", "roi-a").unwrap(),
        "roi a",
        AnalysisWorldGeometry::Box3D {
            min: DVec3::new(-1.0, -1.0, -1.0),
            max: DVec3::new(1.0, 1.0, 1.0),
        },
        SceneArtifactTime::interval(TimeIndex::new(0), TimeIndex::new(4)).unwrap(),
    )
    .unwrap();
    let annotation = AnnotationArtifact::new(
        SceneArtifactId::new("annotation", "note-a").unwrap(),
        "note a",
        AnalysisWorldGeometry::Point {
            position: DVec3::new(0.0, 2.0, 0.0),
            radius_px: 4.0,
        },
        Some("interesting".to_owned()),
        SceneArtifactTime::Static,
    )
    .unwrap();
    let measurement = MeasurementArtifact::distance(
        SceneArtifactId::new("measurement", "distance-a").unwrap(),
        "distance a",
        DVec3::ZERO,
        DVec3::new(0.0, 3.0, 4.0),
        MeasurementProvenance {
            source: "manual".to_owned(),
            scope: "world".to_owned(),
        },
        SceneArtifactTime::Static,
    )
    .unwrap();
    store
        .apply(SceneEditCommand::PutTrack { artifact: track })
        .unwrap();
    store
        .apply(SceneEditCommand::PutRoi { artifact: roi })
        .unwrap();
    store
        .apply(SceneEditCommand::PutAnnotation {
            artifact: annotation,
        })
        .unwrap();
    store
        .apply(SceneEditCommand::PutMeasurement {
            artifact: measurement,
        })
        .unwrap();
    store
}

fn analysis_test_scene_handle_pick_value(
    artifact_kind: EditableSceneArtifactKind,
    handle: SceneEditHandle,
) -> PickValue {
    PickValue::ObjectMetadata(
        SceneEditHandleId {
            artifact_kind,
            artifact_id: "test".to_owned(),
            handle,
        }
        .metadata_value(),
    )
}

fn scene_draw_list_for_test(
    application: &ApplicationState,
    dataset: &current_runtime::dataset::CurrentDatasetRuntime,
    analysis: &current_runtime::analysis::CurrentAnalysisRuntime,
    ui_runtime: &current_runtime::ui::CurrentUiRuntime,
) -> anyhow::Result<mirante4d_renderer::SceneDrawList> {
    let snapshot = application.snapshot();
    let view = application_view(&snapshot);
    let active_layer_id = current_physical_layer_id(dataset, view.active_layer())?;
    scene_draw_list(
        analysis,
        ui_runtime,
        scene_extraction::SceneViewInput {
            active_layer_id: &active_layer_id,
            active_timepoint: view.timepoint(),
            active_source_grid_to_world: snapshot
                .catalog()
                .layer(view.active_layer())
                .expect("the canonical test view must close over its catalog")
                .grid_to_world(),
            camera: *view.camera(),
        },
    )
}

fn selected_scene_handle_targets_for_test(
    application: &ApplicationState,
    dataset: &current_runtime::dataset::CurrentDatasetRuntime,
    analysis: &current_runtime::analysis::CurrentAnalysisRuntime,
    ui_runtime: &current_runtime::ui::CurrentUiRuntime,
    render: &current_runtime::render::CurrentRenderRuntime,
) -> anyhow::Result<Vec<mirante4d_renderer::ScenePickTarget>> {
    let snapshot = application.snapshot();
    let view = application_view(&snapshot);
    let active_layer_id = current_physical_layer_id(dataset, view.active_layer())?;
    selected_scene_handle_pick_targets(
        analysis,
        ui_runtime,
        render,
        scene_extraction::SceneViewInput {
            active_layer_id: &active_layer_id,
            active_timepoint: view.timepoint(),
            active_source_grid_to_world: snapshot
                .catalog()
                .layer(view.active_layer())
                .expect("the canonical test view must close over its catalog")
                .grid_to_world(),
            camera: *view.camera(),
        },
    )
}

#[test]
fn app_full_time_series_analysis_uses_data_engine_and_exports_csv() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(&root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let before_stats = opened.dataset_runtime.dataset.stats().unwrap();

    compute_full_time_series_analysis_for_test(
        &mut application,
        &opened.dataset_runtime,
        &mut opened.analysis_runtime,
    )
    .unwrap();
    let snapshot = application.snapshot();
    export_selected_analysis_table(
        &mut opened.analysis_runtime,
        AnalysisTableExportInput {
            table_descriptors: snapshot.transient().analysis_tables(),
            selected_table: snapshot.transient().selected_analysis_table(),
        },
    )
    .unwrap();

    let after_stats = opened.dataset_runtime.dataset.stats().unwrap();
    let table = opened.analysis_runtime.analysis_tables.last().unwrap();
    assert!(after_stats.subset_reads > before_stats.subset_reads);
    assert_eq!(table.rows.len(), 3);
    assert_eq!(table.state, AnalysisResultState::Complete);
    assert_eq!(
        table.provenance.execution_class,
        AnalysisExecutionClass::FullScopeBatch
    );
    assert_eq!(table.provenance.timepoint_start, 0);
    assert_eq!(table.provenance.timepoint_end_exclusive, 3);
    assert_eq!(table.provenance.data_source, "data_engine_volume_reads");
    assert_eq!(
        opened
            .analysis_runtime
            .analysis_plots
            .last()
            .unwrap()
            .series[0]
            .points
            .len(),
        3
    );
    let csv = opened
        .analysis_runtime
        .last_analysis_export_csv
        .as_ref()
        .unwrap();
    assert!(csv.contains("# analysis_state,complete"));
    assert!(csv.contains("timepoint,voxel_count,geometric_voxel_count,nonzero_count,min,max,mean"));
}

#[test]
fn export_uses_selected_analysis_table_not_latest_table() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    opened
        .analysis_runtime
        .analysis_tables
        .push(test_analysis_table_named(
            "alpha-table",
            "alpha table",
            vec![("alpha-row", 1, 1.0)],
        ));
    opened
        .analysis_runtime
        .analysis_tables
        .push(test_analysis_table_named(
            "beta-table",
            "beta table",
            vec![("beta-row", 2, 2.0)],
        ));

    let token = begin_test_analysis_operation(&mut application);
    complete_test_analysis_operation(&mut application, &opened.analysis_runtime, token, 0, 0);
    let descriptors = application
        .snapshot()
        .transient()
        .analysis_tables()
        .to_vec();

    application
        .dispatch(ApplicationCommand::SelectAnalysisTable(Some(
            descriptors[0].id(),
        )))
        .unwrap();
    let snapshot = application.snapshot();
    export_selected_analysis_table(
        &mut opened.analysis_runtime,
        AnalysisTableExportInput {
            table_descriptors: snapshot.transient().analysis_tables(),
            selected_table: snapshot.transient().selected_analysis_table(),
        },
    )
    .unwrap();
    let alpha_csv = opened
        .analysis_runtime
        .last_analysis_export_csv
        .as_ref()
        .unwrap();
    assert!(alpha_csv.contains("alpha-row"));
    assert!(!alpha_csv.contains("beta-row"));

    application
        .dispatch(ApplicationCommand::SelectAnalysisTable(Some(
            descriptors[1].id(),
        )))
        .unwrap();
    let snapshot = application.snapshot();
    export_selected_analysis_table(
        &mut opened.analysis_runtime,
        AnalysisTableExportInput {
            table_descriptors: snapshot.transient().analysis_tables(),
            selected_table: snapshot.transient().selected_analysis_table(),
        },
    )
    .unwrap();
    let beta_csv = opened
        .analysis_runtime
        .last_analysis_export_csv
        .as_ref()
        .unwrap();
    assert!(beta_csv.contains("beta-row"));
    assert!(!beta_csv.contains("alpha-row"));
}

#[test]
fn roi_analysis_records_exact_cpu_compute_path_without_gpu_renderer() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let roi = RoiArtifact::new(
        SceneArtifactId::new("roi", "roi-a").unwrap(),
        "roi-a",
        AnalysisWorldGeometry::Box3D {
            min: DVec3::new(0.0, 0.0, 0.0),
            max: DVec3::new(0.4, 0.4, 0.4),
        },
        SceneArtifactTime::Static,
    )
    .unwrap();
    opened
        .analysis_runtime
        .scene_artifacts
        .apply(SceneEditCommand::PutRoi { artifact: roi })
        .unwrap();

    compute_active_roi_analysis_for_test(
        &mut application,
        &opened.dataset_runtime,
        &mut opened.analysis_runtime,
    )
    .unwrap();

    let table = opened.analysis_runtime.analysis_tables.last().unwrap();
    assert_eq!(
        table.provenance.parameters.get("compute_path"),
        Some(&"cpu_exact_u16_region_streaming_reference".to_owned())
    );
    assert_eq!(
        table.provenance.compute_precision,
        "CPU f64 accumulation over source uint16 ROI region values"
    );
}

#[test]
fn full_time_series_analysis_job_reports_progress_and_cancels() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&root).unwrap();
    let application = test_application_for_opened_source(&opened);
    let context = analysis_job_context_for_test(
        &application,
        &opened.dataset_runtime,
        &opened.analysis_runtime,
    );
    let cancellation = CancellationToken::new();
    let mut progress_events = Vec::new();

    let err = run_full_time_series_analysis_job(&context, &cancellation, |progress| {
        progress_events.push(progress);
        cancellation.cancel();
        Ok(())
    })
    .unwrap_err();

    assert!(err.to_string().contains("analysis was cancelled"));
    assert_eq!(progress_events.len(), 1);
    assert_eq!(progress_events[0].completed, 1);
    assert_eq!(progress_events[0].total, 3);
}

#[test]
fn analysis_progress_fraction_handles_empty_scope() {
    assert_eq!(
        analysis_progress_fraction(Some(&AnalysisProgress {
            completed: 0,
            total: 0,
            label: "empty".to_owned(),
        })),
        Some(1.0)
    );
    assert_eq!(
        analysis_progress_fraction(Some(&AnalysisProgress {
            completed: 1,
            total: 4,
            label: "one".to_owned(),
        })),
        Some(0.25)
    );
}

#[test]
fn analysis_table_filter_matches_text_and_numbers() {
    let table = test_analysis_table(vec![
        ("roi-a", 7, 11.5),
        ("roi-b", 42, 19.25),
        ("control", 100, 4.0),
    ]);

    let text_preview = analysis_table_preview_rows(&table, "roi-b", None);
    assert_eq!(text_preview.total_rows, 3);
    assert_eq!(text_preview.matched_rows, 1);
    assert_eq!(text_preview.shown_indices, vec![1]);

    let numeric_preview = analysis_table_preview_rows(&table, "19.250", None);
    assert_eq!(numeric_preview.matched_rows, 1);
    assert_eq!(numeric_preview.shown_indices, vec![1]);
}

#[test]
fn analysis_table_sort_orders_numeric_cells() {
    let table = test_analysis_table(vec![
        ("middle", 10, 5.0),
        ("last", 100, 2.0),
        ("first", 2, 9.0),
    ]);
    let ascending = AnalysisTableSort {
        column_key: "count".to_owned(),
        ascending: true,
    };
    let descending = AnalysisTableSort {
        column_key: "count".to_owned(),
        ascending: false,
    };

    let asc_preview = analysis_table_preview_rows(&table, "", Some(&ascending));
    assert_eq!(asc_preview.shown_indices, vec![2, 0, 1]);

    let desc_preview = analysis_table_preview_rows(&table, "", Some(&descending));
    assert_eq!(desc_preview.shown_indices, vec![1, 0, 2]);
}

#[test]
fn analysis_table_preview_indexes_all_matched_rows() {
    let table = test_analysis_table(
        (0..250)
            .map(|index| ("row", index, index as f64))
            .collect::<Vec<_>>(),
    );

    let preview = analysis_table_preview_rows(&table, "row", None);

    assert_eq!(preview.total_rows, 250);
    assert_eq!(preview.matched_rows, 250);
    assert_eq!(preview.shown_indices.len(), 250);
    assert_eq!(preview.shown_indices[0], 0);
    assert_eq!(preview.shown_indices[249], 249);
}

#[test]
fn analysis_table_preview_indexes_all_sorted_matches_without_cap() {
    let table = test_analysis_table(
        (0..250)
            .map(|index| ("row", index, (250 - index) as f64))
            .collect::<Vec<_>>(),
    );
    let sort = AnalysisTableSort {
        column_key: "mean".to_owned(),
        ascending: true,
    };

    let preview = analysis_table_preview_rows(&table, "row", Some(&sort));

    assert_eq!(preview.total_rows, 250);
    assert_eq!(preview.matched_rows, 250);
    assert_eq!(preview.shown_indices.len(), 250);
    assert_eq!(preview.shown_indices[0], 249);
    assert_eq!(preview.shown_indices[249], 0);
}

#[test]
fn analysis_plot_bounds_ignore_nonfinite_or_empty_points() {
    let empty = AnalysisPlot {
        id: "empty".to_owned(),
        name: "empty".to_owned(),
        state: AnalysisResultState::Complete,
        provenance: test_analysis_provenance(),
        x_label: "t".to_owned(),
        y_label: "mean".to_owned(),
        series: vec![AnalysisPlotSeries {
            name: "mean".to_owned(),
            points: vec![
                AnalysisPlotPoint {
                    x: f64::NAN,
                    y: 1.0,
                },
                AnalysisPlotPoint {
                    x: 1.0,
                    y: f64::INFINITY,
                },
            ],
        }],
    };
    assert_eq!(analysis_plot_bounds(&empty), None);

    let plot = AnalysisPlot {
        series: vec![AnalysisPlotSeries {
            name: "mean".to_owned(),
            points: vec![
                AnalysisPlotPoint { x: 2.0, y: 5.0 },
                AnalysisPlotPoint {
                    x: f64::NAN,
                    y: 999.0,
                },
                AnalysisPlotPoint { x: 4.0, y: 1.0 },
            ],
        }],
        ..empty
    };
    assert_eq!(
        analysis_plot_bounds(&plot),
        Some(AnalysisPlotBounds {
            min_x: 2.0,
            max_x: 4.0,
            min_y: 1.0,
            max_y: 5.0,
        })
    );
}

#[test]
fn analysis_plot_nearest_point_uses_finite_points_in_plot_rect() {
    let plot = AnalysisPlot {
        id: "plot".to_owned(),
        name: "plot".to_owned(),
        state: AnalysisResultState::Complete,
        provenance: test_analysis_provenance(),
        x_label: "t".to_owned(),
        y_label: "mean".to_owned(),
        series: vec![AnalysisPlotSeries {
            name: "mean".to_owned(),
            points: vec![
                AnalysisPlotPoint { x: 0.0, y: 0.0 },
                AnalysisPlotPoint { x: 5.0, y: 10.0 },
                AnalysisPlotPoint {
                    x: f64::NAN,
                    y: 100.0,
                },
                AnalysisPlotPoint { x: 10.0, y: 0.0 },
            ],
        }],
    };
    let bounds = analysis_plot_bounds(&plot).unwrap();
    let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0));
    let target = plot_screen_position(5.0, 10.0, bounds, rect) + egui::vec2(2.0, 2.0);

    let nearest = nearest_analysis_plot_point(&plot, bounds, rect, target).unwrap();
    assert_eq!(nearest.series_index, 0);
    assert_eq!(nearest.point_index, 1);
    assert_eq!(nearest.series_name, "mean");
    assert_eq!(nearest.x, 5.0);
    assert_eq!(nearest.y, 10.0);
    assert!(nearest.distance_sq > 0.0);
    assert_eq!(
        nearest_analysis_plot_point(&plot, bounds, rect, egui::pos2(-1.0, 50.0)),
        None
    );
}

#[test]
fn analysis_plot_view_zoom_and_pan_are_clamped_to_full_bounds() {
    let full_bounds = AnalysisPlotBounds {
        min_x: 0.0,
        max_x: 100.0,
        min_y: -50.0,
        max_y: 50.0,
    };
    let mut view = None;

    zoom_analysis_plot_view(&mut view, 3, full_bounds, 0.5);
    assert_eq!(
        view,
        Some(AnalysisPlotViewRange {
            plot_index: 3,
            min_x: 25.0,
            max_x: 75.0,
            min_y: -25.0,
            max_y: 25.0,
        })
    );

    pan_analysis_plot_view(&mut view, 3, full_bounds, 1.0, 1.0);
    assert_eq!(
        view,
        Some(AnalysisPlotViewRange {
            plot_index: 3,
            min_x: 50.0,
            max_x: 100.0,
            min_y: 0.0,
            max_y: 50.0,
        })
    );

    zoom_analysis_plot_view(&mut view, 3, full_bounds, 10.0);
    assert_eq!(
        analysis_plot_visible_bounds(3, full_bounds, view.as_ref()),
        full_bounds
    );
}

#[test]
fn analysis_plot_view_normalization_clears_invalid_or_foreign_ranges() {
    let full_bounds = AnalysisPlotBounds {
        min_x: 0.0,
        max_x: 10.0,
        min_y: 0.0,
        max_y: 10.0,
    };
    let mut foreign = Some(AnalysisPlotViewRange {
        plot_index: 2,
        min_x: 1.0,
        max_x: 2.0,
        min_y: 1.0,
        max_y: 2.0,
    });
    normalize_analysis_plot_view_for_plot(1, full_bounds, &mut foreign);
    assert_eq!(foreign, None);

    let mut invalid = Some(AnalysisPlotViewRange {
        plot_index: 1,
        min_x: 5.0,
        max_x: 5.0,
        min_y: 1.0,
        max_y: 2.0,
    });
    normalize_analysis_plot_view_for_plot(1, full_bounds, &mut invalid);
    assert_eq!(invalid, None);

    let mut outside = Some(AnalysisPlotViewRange {
        plot_index: 1,
        min_x: 8.0,
        max_x: 12.0,
        min_y: -4.0,
        max_y: 4.0,
    });
    normalize_analysis_plot_view_for_plot(1, full_bounds, &mut outside);
    assert_eq!(
        outside,
        Some(AnalysisPlotViewRange {
            plot_index: 1,
            min_x: 6.0,
            max_x: 10.0,
            min_y: 0.0,
            max_y: 8.0,
        })
    );
}

#[test]
fn app_roi_analysis_uses_roi_artifacts_and_data_engine_volume_reads() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(&root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let roi = RoiArtifact::new(
        SceneArtifactId::new("roi", "roi-a").unwrap(),
        "roi-a",
        AnalysisWorldGeometry::Box3D {
            min: DVec3::new(0.0, 0.0, 0.0),
            max: DVec3::new(0.4, 0.4, 0.4),
        },
        SceneArtifactTime::Timepoint(TimeIndex::new(0)),
    )
    .unwrap();
    opened
        .analysis_runtime
        .scene_artifacts
        .apply(SceneEditCommand::PutRoi { artifact: roi })
        .unwrap();
    let before_stats = opened.dataset_runtime.dataset.stats().unwrap();

    compute_active_roi_analysis_for_test(
        &mut application,
        &opened.dataset_runtime,
        &mut opened.analysis_runtime,
    )
    .unwrap();

    let after_stats = opened.dataset_runtime.dataset.stats().unwrap();
    let table = opened.analysis_runtime.analysis_tables.last().unwrap();
    let row = &table.rows[0];
    assert!(after_stats.subset_reads > before_stats.subset_reads);
    assert_eq!(after_stats.decoded_values - before_stats.decoded_values, 8);
    assert_eq!(table.rows.len(), 1);
    assert_eq!(table.state, AnalysisResultState::Complete);
    assert_eq!(
        table.provenance.execution_class,
        AnalysisExecutionClass::RoiLocalExact
    );
    assert_eq!(table.provenance.data_source, "data_engine_volume_reads");
    assert_eq!(
        row.cells.get("roi_id"),
        Some(&AnalysisCell::Text("roi-a".to_owned()))
    );
    assert_eq!(
        row.cells.get("voxel_count"),
        Some(&AnalysisCell::Integer(8))
    );
    assert_eq!(row.cells.get("mean"), Some(&AnalysisCell::Float(137.5)));
}

#[test]
fn app_extracts_time_aware_scene_layers_from_persistent_artifacts() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(&root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    let ui_runtime = analysis_scene_ui_runtime();
    opened.analysis_runtime.scene_artifacts = analysis_test_scene_artifacts();
    application
        .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(2)))
        .unwrap();

    let draw_list = scene_draw_list_for_test(
        &application,
        &opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
    )
    .unwrap();

    assert_eq!(draw_list.len(), 4);
    assert!(draw_list.items().iter().any(|item| {
        item.layer_id.as_str() == "tracks" && item.object_id.as_str() == "track-a_seg_1_3"
    }));
    assert!(
        draw_list
            .items()
            .iter()
            .any(|item| item.layer_id.as_str() == "rois" && item.object_id.as_str() == "roi-a")
    );
    assert!(draw_list.items().iter().any(|item| {
        item.layer_id.as_str() == "annotations" && item.object_id.as_str() == "note-a"
    }));
    assert!(draw_list.items().iter().any(|item| {
        item.layer_id.as_str() == "measurements" && item.object_id.as_str() == "distance-a"
    }));
}

#[test]
fn app_extracts_selected_scene_artifact_viewport_handles() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(&root).unwrap();
    let application = test_application_for_opened_source(&opened);
    let mut ui_runtime = analysis_scene_ui_runtime();
    opened.analysis_runtime.scene_artifacts = analysis_test_scene_artifacts();
    let roi_id = SceneArtifactId::new("roi", "roi-a").unwrap();
    select_scene_artifact(&mut ui_runtime, EditableSceneArtifactKind::Roi, &roi_id);

    let draw_list = scene_draw_list_for_test(
        &application,
        &opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
    )
    .unwrap();
    let handles = selected_scene_handle_targets_for_test(
        &application,
        &opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &opened.render_runtime,
    )
    .unwrap();

    assert_eq!(handles.len(), 2);
    assert_eq!(
        draw_list
            .items()
            .iter()
            .filter(|item| item.layer_id.as_str() == SCENE_HANDLE_LAYER_ID)
            .count(),
        2
    );
    assert!(handles.iter().any(|target| {
        target.hit.value
            == Some(analysis_test_scene_handle_pick_value(
                EditableSceneArtifactKind::Roi,
                SceneEditHandle::WorldBoxMin,
            ))
    }));
    assert!(handles.iter().any(|target| {
        target.hit.value
            == Some(analysis_test_scene_handle_pick_value(
                EditableSceneArtifactKind::Roi,
                SceneEditHandle::WorldBoxMax,
            ))
    }));
}

#[test]
fn app_picks_selected_scene_artifact_viewport_handle_before_volume() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(&root).unwrap();
    let application = test_application_for_opened_source(&opened);
    let mut ui_runtime = analysis_scene_ui_runtime();
    opened.analysis_runtime.scene_artifacts = analysis_test_scene_artifacts();
    let measurement_id = SceneArtifactId::new("measurement", "distance-a").unwrap();
    select_scene_artifact(
        &mut ui_runtime,
        EditableSceneArtifactKind::Measurement,
        &measurement_id,
    );
    let target = selected_scene_handle_targets_for_test(
        &application,
        &opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &opened.render_runtime,
    )
    .unwrap()
    .into_iter()
    .find(|target| {
        target.hit.value
            == Some(analysis_test_scene_handle_pick_value(
                EditableSceneArtifactKind::Measurement,
                SceneEditHandle::MeasurementStart,
            ))
    })
    .unwrap();
    let screen = target.hit.screen_position.unwrap();

    let snapshot = application.snapshot();
    let hit = pick_hit_from_viewport_hover(
        &snapshot,
        &opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &opened.render_runtime,
        ViewportHover {
            x: screen.x.round() as u64,
            y: screen.y.round() as u64,
            intensity: ViewportIntensity::U16(0),
        },
    )
    .unwrap();

    assert_eq!(hit.kind, PickHitKind::AnnotationHandle);
    assert_eq!(
        hit.value,
        Some(analysis_test_scene_handle_pick_value(
            EditableSceneArtifactKind::Measurement,
            SceneEditHandle::MeasurementStart,
        ))
    );
    assert_eq!(
        hit.object_id.as_ref().map(|id| id.as_str()),
        Some("distance-a")
    );
}

#[test]
fn app_viewport_handle_commit_updates_scene_artifact_through_command_store() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(&root).unwrap();
    let application = test_application_for_opened_source(&opened);
    let mut ui_runtime = analysis_scene_ui_runtime();
    opened.analysis_runtime.scene_artifacts = analysis_test_scene_artifacts();
    let measurement_id = SceneArtifactId::new("measurement", "distance-a").unwrap();
    select_scene_artifact(
        &mut ui_runtime,
        EditableSceneArtifactKind::Measurement,
        &measurement_id,
    );
    let handle = selected_scene_handle_targets_for_test(
        &application,
        &opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &opened.render_runtime,
    )
    .unwrap()
    .into_iter()
    .find(|target| {
        target.hit.value
            == Some(analysis_test_scene_handle_pick_value(
                EditableSceneArtifactKind::Measurement,
                SceneEditHandle::MeasurementEnd,
            ))
    })
    .unwrap()
    .hit;
    let current = world_tool_hit(DVec3::new(0.0, 0.0, 12.0), 10.0, 10.0);

    let outcome = apply_viewer_tool_commands(
        &application.snapshot(),
        &mut opened.analysis_runtime,
        &mut ui_runtime,
        vec![ViewerToolCommand::CommitSceneHandleDrag { handle, current }],
    )
    .unwrap();

    assert!(outcome.rerender_requested);
    let measurement = opened
        .analysis_runtime
        .scene_artifacts
        .measurement(&measurement_id)
        .unwrap();
    assert_eq!(measurement.result.as_ref().unwrap().value, 12.0);
    assert!(opened.analysis_runtime.scene_artifacts.can_undo());
    opened.analysis_runtime.scene_artifacts.undo().unwrap();
    let restored = opened
        .analysis_runtime
        .scene_artifacts
        .measurement(&measurement_id)
        .unwrap();
    assert_eq!(restored.result.as_ref().unwrap().value, 5.0);
}

#[test]
fn world_geometry_viewport_handles_cover_all_geometry_variants() {
    let artifact_kind = EditableSceneArtifactKind::Annotation;
    let artifact_id = "artifact-a";
    let cases = [
        (
            AnalysisWorldGeometry::Point {
                position: DVec3::ZERO,
                radius_px: 2.0,
            },
            vec![SceneEditHandle::WorldPointPosition],
        ),
        (
            AnalysisWorldGeometry::LineSegment {
                start: DVec3::ZERO,
                end: DVec3::X,
                width_px: 2.0,
            },
            vec![
                SceneEditHandle::WorldLineStart,
                SceneEditHandle::WorldLineEnd,
            ],
        ),
        (
            AnalysisWorldGeometry::Polyline {
                points: vec![DVec3::ZERO, DVec3::X, DVec3::Y],
                width_px: 2.0,
            },
            vec![
                SceneEditHandle::WorldPolylinePoint { index: 0 },
                SceneEditHandle::WorldPolylinePoint { index: 1 },
                SceneEditHandle::WorldPolylinePoint { index: 2 },
            ],
        ),
        (
            AnalysisWorldGeometry::Box3D {
                min: DVec3::ZERO,
                max: DVec3::ONE,
            },
            vec![SceneEditHandle::WorldBoxMin, SceneEditHandle::WorldBoxMax],
        ),
        (
            AnalysisWorldGeometry::Ellipsoid {
                center: DVec3::ZERO,
                radii: DVec3::ONE,
            },
            vec![
                SceneEditHandle::WorldEllipsoidCenter,
                SceneEditHandle::WorldEllipsoidRadiusX,
                SceneEditHandle::WorldEllipsoidRadiusY,
                SceneEditHandle::WorldEllipsoidRadiusZ,
            ],
        ),
    ];

    for (geometry, expected_handles) in cases {
        let handles = world_geometry_edit_handles(artifact_kind, artifact_id, &geometry);
        let actual_handles = handles
            .into_iter()
            .map(|handle| handle.handle)
            .collect::<Vec<_>>();

        assert_eq!(actual_handles, expected_handles);
    }
}

#[test]
fn world_geometry_viewport_handle_updates_cover_all_geometry_variants() {
    let mut point = AnalysisWorldGeometry::Point {
        position: DVec3::ZERO,
        radius_px: 2.0,
    };
    update_world_geometry_from_handle(
        &mut point,
        &SceneEditHandle::WorldPointPosition,
        DVec3::new(1.0, 2.0, 3.0),
    )
    .unwrap();
    assert_eq!(
        point,
        AnalysisWorldGeometry::Point {
            position: DVec3::new(1.0, 2.0, 3.0),
            radius_px: 2.0,
        }
    );

    let mut line = AnalysisWorldGeometry::LineSegment {
        start: DVec3::ZERO,
        end: DVec3::X,
        width_px: 2.0,
    };
    update_world_geometry_from_handle(
        &mut line,
        &SceneEditHandle::WorldLineEnd,
        DVec3::new(0.0, 5.0, 0.0),
    )
    .unwrap();
    assert_eq!(
        line,
        AnalysisWorldGeometry::LineSegment {
            start: DVec3::ZERO,
            end: DVec3::new(0.0, 5.0, 0.0),
            width_px: 2.0,
        }
    );

    let mut polyline = AnalysisWorldGeometry::Polyline {
        points: vec![DVec3::ZERO, DVec3::X, DVec3::Y],
        width_px: 2.0,
    };
    update_world_geometry_from_handle(
        &mut polyline,
        &SceneEditHandle::WorldPolylinePoint { index: 1 },
        DVec3::new(4.0, 5.0, 6.0),
    )
    .unwrap();
    assert_eq!(polyline.world_points()[1], DVec3::new(4.0, 5.0, 6.0));

    let mut box_geometry = AnalysisWorldGeometry::Box3D {
        min: DVec3::ZERO,
        max: DVec3::ONE,
    };
    update_world_geometry_from_handle(
        &mut box_geometry,
        &SceneEditHandle::WorldBoxMin,
        DVec3::new(2.0, 2.0, 2.0),
    )
    .unwrap();
    assert_eq!(
        box_geometry,
        AnalysisWorldGeometry::Box3D {
            min: DVec3::ONE,
            max: DVec3::new(2.0, 2.0, 2.0),
        }
    );

    let mut ellipsoid = AnalysisWorldGeometry::Ellipsoid {
        center: DVec3::ZERO,
        radii: DVec3::ONE,
    };
    update_world_geometry_from_handle(
        &mut ellipsoid,
        &SceneEditHandle::WorldEllipsoidRadiusX,
        DVec3::new(7.0, 9.0, 9.0),
    )
    .unwrap();
    assert_eq!(
        ellipsoid,
        AnalysisWorldGeometry::Ellipsoid {
            center: DVec3::ZERO,
            radii: DVec3::new(7.0, 1.0, 1.0),
        }
    );
}

#[test]
fn app_viewport_handle_commit_updates_track_point_artifact() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(&root).unwrap();
    let application = test_application_for_opened_source(&opened);
    let mut ui_runtime = analysis_scene_ui_runtime();
    opened.analysis_runtime.scene_artifacts = analysis_test_scene_artifacts();
    let track_id = SceneArtifactId::new("track", "track-a").unwrap();
    select_scene_artifact(&mut ui_runtime, EditableSceneArtifactKind::Track, &track_id);
    let handle = selected_scene_handle_targets_for_test(
        &application,
        &opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &opened.render_runtime,
    )
    .unwrap()
    .into_iter()
    .find(|target| {
        target.hit.value
            == Some(analysis_test_scene_handle_pick_value(
                EditableSceneArtifactKind::Track,
                SceneEditHandle::TrackPoint { index: 1 },
            ))
    })
    .unwrap()
    .hit;
    let current = world_tool_hit(DVec3::new(5.0, 6.0, 7.0), 10.0, 10.0);

    let outcome = apply_viewer_tool_commands(
        &application.snapshot(),
        &mut opened.analysis_runtime,
        &mut ui_runtime,
        vec![ViewerToolCommand::CommitSceneHandleDrag { handle, current }],
    )
    .unwrap();

    assert!(outcome.rerender_requested);
    assert_eq!(
        opened
            .analysis_runtime
            .scene_artifacts
            .track(&track_id)
            .unwrap()
            .points[1]
            .position_world,
        DVec3::new(5.0, 6.0, 7.0)
    );
    assert!(opened.analysis_runtime.scene_artifacts.can_undo());
}

#[test]
fn app_viewport_handle_commit_updates_annotation_geometry_artifact() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(&root).unwrap();
    let application = test_application_for_opened_source(&opened);
    let mut ui_runtime = analysis_scene_ui_runtime();
    opened.analysis_runtime.scene_artifacts = analysis_test_scene_artifacts();
    let annotation_id = SceneArtifactId::new("annotation", "note-a").unwrap();
    select_scene_artifact(
        &mut ui_runtime,
        EditableSceneArtifactKind::Annotation,
        &annotation_id,
    );
    let handle = selected_scene_handle_targets_for_test(
        &application,
        &opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &opened.render_runtime,
    )
    .unwrap()
    .into_iter()
    .find(|target| {
        target.hit.value
            == Some(analysis_test_scene_handle_pick_value(
                EditableSceneArtifactKind::Annotation,
                SceneEditHandle::WorldPointPosition,
            ))
    })
    .unwrap()
    .hit;
    let current = world_tool_hit(DVec3::new(3.0, 4.0, 5.0), 10.0, 10.0);

    apply_viewer_tool_commands(
        &application.snapshot(),
        &mut opened.analysis_runtime,
        &mut ui_runtime,
        vec![ViewerToolCommand::CommitSceneHandleDrag { handle, current }],
    )
    .unwrap();

    let annotation = opened
        .analysis_runtime
        .scene_artifacts
        .annotation(&annotation_id)
        .unwrap();
    assert_eq!(
        annotation.geometry,
        AnalysisWorldGeometry::Point {
            position: DVec3::new(3.0, 4.0, 5.0),
            radius_px: 4.0,
        }
    );
}

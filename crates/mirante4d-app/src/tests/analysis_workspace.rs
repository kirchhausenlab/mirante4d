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

fn test_analysis_table(rows: Vec<(&str, u64, f64)>) -> AnalysisTable {
    test_analysis_table_named("test-table", "test table", rows)
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

fn register_passive_tables(
    application: &mut ApplicationState,
    tables: &[AnalysisTable],
) -> Vec<AnalysisTableDescriptor> {
    application
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
        .unwrap();
    let token = application
        .drain_events(usize::MAX)
        .into_iter()
        .find_map(|event| match event {
            ApplicationEvent::OperationStarted { token } => Some(token),
            _ => None,
        })
        .unwrap();
    let descriptors = tables
        .iter()
        .enumerate()
        .map(|(slot, table)| {
            let slot = u16::try_from(slot).expect("test table slot count is bounded");
            let row_count =
                u64::try_from(table.rows.len()).expect("test table row count fits in u64");
            AnalysisTableDescriptor::new(
                AnalysisTableId::from_operation(token.operation_id(), slot),
                row_count,
            )
        })
        .collect::<Vec<_>>();
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::AnalysisReady {
                tables: descriptors.clone(),
                plots: Vec::new(),
            },
        })
        .unwrap();
    descriptors
}

#[test]
fn passive_analysis_export_uses_the_selected_table() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut application = test_application_for_opened_source(&opened);
    opened.analysis_runtime.analysis_tables = vec![
        test_analysis_table_named("alpha-table", "alpha table", vec![("alpha-row", 1, 1.0)]),
        test_analysis_table_named("beta-table", "beta table", vec![("beta-row", 2, 2.0)]),
    ];
    let descriptors = register_passive_tables(
        &mut application,
        opened.analysis_runtime.analysis_tables.as_slice(),
    );

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

    let csv = opened
        .analysis_runtime
        .last_analysis_export_csv
        .as_deref()
        .unwrap();
    assert!(csv.contains("alpha-row"));
    assert!(!csv.contains("beta-row"));
    opened.dataset.request_shutdown().unwrap();
}

#[test]
fn analysis_execution_is_explicitly_deferred_until_wp12() {
    assert_eq!(
        current_runtime::analysis::ANALYSIS_EXECUTION_DEFERRED_MESSAGE,
        "Analysis execution is deferred until WP-12."
    );
}

#[test]
fn passive_analysis_table_filter_and_sort_use_all_matching_rows() {
    let table = test_analysis_table(vec![
        ("roi-a", 7, 11.5),
        ("roi-b", 42, 19.25),
        ("control", 100, 4.0),
    ]);

    let filtered = analysis_table_preview_rows(&table, "roi", None);
    assert_eq!(filtered.total_rows, 3);
    assert_eq!(filtered.matched_rows, 2);
    assert_eq!(filtered.shown_indices, vec![0, 1]);

    let sorted = analysis_table_preview_rows(
        &table,
        "",
        Some(&AnalysisTableSort {
            column_key: "count".to_owned(),
            ascending: false,
        }),
    );
    assert_eq!(sorted.shown_indices, vec![2, 1, 0]);
}

fn test_analysis_plot() -> AnalysisPlot {
    AnalysisPlot {
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
    }
}

#[test]
fn passive_analysis_plot_bounds_and_nearest_point_ignore_nonfinite_values() {
    let plot = test_analysis_plot();
    let bounds = analysis_plot_bounds(&plot).unwrap();
    assert_eq!(
        bounds,
        AnalysisPlotBounds {
            min_x: 0.0,
            max_x: 10.0,
            min_y: 0.0,
            max_y: 10.0,
        }
    );

    let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0));
    let target = plot_screen_position(5.0, 10.0, bounds, rect) + egui::vec2(2.0, 2.0);
    let nearest = nearest_analysis_plot_point(&plot, bounds, rect, target).unwrap();
    assert_eq!((nearest.series_index, nearest.point_index), (0, 1));
    assert_eq!((nearest.x, nearest.y), (5.0, 10.0));
    assert_eq!(
        nearest_analysis_plot_point(&plot, bounds, rect, egui::pos2(-1.0, 50.0)),
        None
    );
}

#[test]
fn passive_analysis_plot_view_stays_within_full_bounds() {
    let full = AnalysisPlotBounds {
        min_x: 0.0,
        max_x: 100.0,
        min_y: -50.0,
        max_y: 50.0,
    };
    let mut view = None;

    zoom_analysis_plot_view(&mut view, 3, full, 0.5);
    pan_analysis_plot_view(&mut view, 3, full, 1.0, 1.0);
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

    normalize_analysis_plot_view_for_plot(2, full, &mut view);
    assert_eq!(view, None);
    assert_eq!(analysis_plot_visible_bounds(3, full, view.as_ref()), full);
}

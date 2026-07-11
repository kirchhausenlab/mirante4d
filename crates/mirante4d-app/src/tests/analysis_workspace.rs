#[test]
fn app_full_time_series_analysis_uses_data_engine_and_exports_csv() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    let before_stats = state.dataset.stats().unwrap();

    compute_full_time_series_analysis(&mut state).unwrap();
    export_selected_analysis_table(&mut state).unwrap();

    let after_stats = state.dataset.stats().unwrap();
    let table = state.analysis_tables.last().unwrap();
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
        state.analysis_plots.last().unwrap().series[0].points.len(),
        3
    );
    let csv = state.last_analysis_export_csv.as_ref().unwrap();
    assert!(csv.contains("# analysis_state,complete"));
    assert!(csv.contains("timepoint,voxel_count,geometric_voxel_count,nonzero_count,min,max,mean"));
}

#[test]
fn analysis_selection_defaults_to_latest_and_clamps_out_of_range() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();

    normalize_analysis_selection(&mut state);
    assert_eq!(state.selected_analysis_table_index, None);
    assert_eq!(state.selected_analysis_plot_index, None);

    state.analysis_tables.push(test_analysis_table_named(
        "first-table",
        "first table",
        vec![("first", 1, 1.0)],
    ));
    state.analysis_tables.push(test_analysis_table_named(
        "second-table",
        "second table",
        vec![("second", 2, 2.0)],
    ));
    state
        .analysis_plots
        .push(test_analysis_plot("first-plot", "first plot", 2));

    normalize_analysis_selection(&mut state);
    assert_eq!(state.selected_analysis_table_index, Some(1));
    assert_eq!(state.selected_analysis_plot_index, Some(0));

    state.selected_analysis_table_index = Some(99);
    state.selected_analysis_plot_index = Some(99);
    normalize_analysis_selection(&mut state);
    assert_eq!(state.selected_analysis_table_index, Some(1));
    assert_eq!(state.selected_analysis_plot_index, Some(0));

    state.analysis_tables.clear();
    state.analysis_plots.clear();
    normalize_analysis_selection(&mut state);
    assert_eq!(state.selected_analysis_table_index, None);
    assert_eq!(state.selected_analysis_plot_index, None);
}

#[test]
fn analysis_plot_point_selection_is_cleared_when_out_of_scope() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state
        .analysis_plots
        .push(test_analysis_plot("first-plot", "first plot", 2));
    state
        .analysis_plots
        .push(test_analysis_plot("second-plot", "second plot", 1));
    state.selected_analysis_plot_index = Some(0);
    state.selected_analysis_plot_point = Some(AnalysisPlotPointSelection {
        plot_index: 0,
        series_index: 0,
        point_index: 1,
    });

    normalize_analysis_selection(&mut state);
    assert_eq!(
        state.selected_analysis_plot_point,
        Some(AnalysisPlotPointSelection {
            plot_index: 0,
            series_index: 0,
            point_index: 1,
        })
    );

    state.selected_analysis_plot_index = Some(1);
    normalize_analysis_selection(&mut state);
    assert_eq!(state.selected_analysis_plot_point, None);

    state.selected_analysis_plot_point = Some(AnalysisPlotPointSelection {
        plot_index: 1,
        series_index: 0,
        point_index: 10,
    });
    normalize_analysis_selection(&mut state);
    assert_eq!(state.selected_analysis_plot_point, None);
}

#[test]
fn export_uses_selected_analysis_table_not_latest_table() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.analysis_tables.push(test_analysis_table_named(
        "alpha-table",
        "alpha table",
        vec![("alpha-row", 1, 1.0)],
    ));
    state.analysis_tables.push(test_analysis_table_named(
        "beta-table",
        "beta table",
        vec![("beta-row", 2, 2.0)],
    ));

    state.selected_analysis_table_index = Some(0);
    export_selected_analysis_table(&mut state).unwrap();
    let alpha_csv = state.last_analysis_export_csv.as_ref().unwrap();
    assert!(alpha_csv.contains("alpha-row"));
    assert!(!alpha_csv.contains("beta-row"));

    state.selected_analysis_table_index = Some(1);
    export_selected_analysis_table(&mut state).unwrap();
    let beta_csv = state.last_analysis_export_csv.as_ref().unwrap();
    assert!(beta_csv.contains("beta-row"));
    assert!(!beta_csv.contains("alpha-row"));
}

#[test]
fn computed_analysis_outputs_select_new_table_and_plot() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();

    compute_full_time_series_analysis(&mut state).unwrap();
    assert_eq!(state.selected_analysis_table_index, Some(0));
    assert_eq!(state.selected_analysis_plot_index, Some(0));

    compute_active_roi_analysis(&mut state).unwrap();
    assert_eq!(state.selected_analysis_table_index, Some(1));
    assert_eq!(state.selected_analysis_plot_index, Some(0));
}

#[test]
fn roi_analysis_records_exact_cpu_compute_path_without_gpu_renderer() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
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
    state
        .scene_artifacts
        .apply(SceneEditCommand::PutRoi { artifact: roi })
        .unwrap();

    compute_active_roi_analysis(&mut state).unwrap();

    let table = state.analysis_tables.last().unwrap();
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
    let state = open_dataset_and_render_first_frame(&root).unwrap();
    let context = AnalysisJobContext::from_state(&state, None);
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
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    let roi = RoiArtifact::new(
        SceneArtifactId::new("roi", "roi-a").unwrap(),
        "roi-a",
        AnalysisWorldGeometry::Box3D {
            min: DVec3::new(0.0, 0.0, 0.0),
            max: DVec3::new(0.4, 0.4, 0.4),
        },
        SceneArtifactTime::Timepoint(TimeIndex(0)),
    )
    .unwrap();
    state
        .scene_artifacts
        .apply(SceneEditCommand::PutRoi { artifact: roi })
        .unwrap();
    let before_stats = state.dataset.stats().unwrap();

    compute_active_roi_analysis(&mut state).unwrap();

    let after_stats = state.dataset.stats().unwrap();
    let table = state.analysis_tables.last().unwrap();
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
fn project_package_roundtrip_does_not_persist_viewer_tool_state() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    state.viewer_tools.set_active_tool(ViewerTool::RoiBox);
    state.viewer_tools.crosshair = Some(world_tool_hit(DVec3::ZERO, 1.0, 1.0));
    state.viewer_tools.selection = Some(ToolSelection::SceneObject {
        kind: PickHitKind::Roi,
        object_id: "roi-a".to_owned(),
    });
    let session_path = tempdir.path().join("tool-state-session.m4dproj");

    let session = session_from_state(&state);
    write_session_file(&session_path, &session).unwrap();
    let restored =
        open_state_from_session(&read_session_file(&session_path).unwrap(), None).unwrap();

    assert_eq!(restored.viewer_tools.active_tool, ViewerTool::Navigate);
    assert!(restored.viewer_tools.crosshair.is_none());
    assert!(restored.viewer_tools.selection.is_none());
}

#[test]
fn app_extracts_time_aware_scene_layers_from_persistent_artifacts() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    state.scene_artifacts = sample_scene_artifacts();
    state.active_timepoint = TimeIndex(2);

    let draw_list = scene_draw_list_for_state(&state).unwrap();

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
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    state.scene_artifacts = sample_scene_artifacts();
    let roi_id = SceneArtifactId::new("roi", "roi-a").unwrap();
    select_scene_artifact(&mut state, EditableSceneArtifactKind::Roi, &roi_id);

    let draw_list = scene_draw_list_for_state(&state).unwrap();
    let handles = selected_scene_handle_pick_targets(&state).unwrap();

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
            == Some(scene_handle_pick_value(
                EditableSceneArtifactKind::Roi,
                SceneEditHandle::WorldBoxMin,
            ))
    }));
    assert!(handles.iter().any(|target| {
        target.hit.value
            == Some(scene_handle_pick_value(
                EditableSceneArtifactKind::Roi,
                SceneEditHandle::WorldBoxMax,
            ))
    }));
}

#[test]
fn app_picks_selected_scene_artifact_viewport_handle_before_volume() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    state.scene_artifacts = sample_scene_artifacts();
    let measurement_id = SceneArtifactId::new("measurement", "distance-a").unwrap();
    select_scene_artifact(
        &mut state,
        EditableSceneArtifactKind::Measurement,
        &measurement_id,
    );
    let target = selected_scene_handle_pick_targets(&state)
        .unwrap()
        .into_iter()
        .find(|target| {
            target.hit.value
                == Some(scene_handle_pick_value(
                    EditableSceneArtifactKind::Measurement,
                    SceneEditHandle::MeasurementStart,
                ))
        })
        .unwrap();
    let screen = target.hit.screen_position.unwrap();

    let hit = pick_hit_from_viewport_hover(
        &state,
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
        Some(scene_handle_pick_value(
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
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    state.scene_artifacts = sample_scene_artifacts();
    let measurement_id = SceneArtifactId::new("measurement", "distance-a").unwrap();
    select_scene_artifact(
        &mut state,
        EditableSceneArtifactKind::Measurement,
        &measurement_id,
    );
    let handle = selected_scene_handle_pick_targets(&state)
        .unwrap()
        .into_iter()
        .find(|target| {
            target.hit.value
                == Some(scene_handle_pick_value(
                    EditableSceneArtifactKind::Measurement,
                    SceneEditHandle::MeasurementEnd,
                ))
        })
        .unwrap()
        .hit;
    let current = world_tool_hit(DVec3::new(0.0, 0.0, 12.0), 10.0, 10.0);

    let outcome = apply_viewer_tool_commands(
        &mut state,
        vec![ViewerToolCommand::CommitSceneHandleDrag { handle, current }],
    )
    .unwrap();

    assert!(outcome.rerender_requested);
    let measurement = state.scene_artifacts.measurement(&measurement_id).unwrap();
    assert_eq!(measurement.result.as_ref().unwrap().value, 12.0);
    assert!(state.scene_artifacts.can_undo());
    state.scene_artifacts.undo().unwrap();
    let restored = state.scene_artifacts.measurement(&measurement_id).unwrap();
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
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    state.scene_artifacts = sample_scene_artifacts();
    let track_id = SceneArtifactId::new("track", "track-a").unwrap();
    select_scene_artifact(&mut state, EditableSceneArtifactKind::Track, &track_id);
    let handle = selected_scene_handle_pick_targets(&state)
        .unwrap()
        .into_iter()
        .find(|target| {
            target.hit.value
                == Some(scene_handle_pick_value(
                    EditableSceneArtifactKind::Track,
                    SceneEditHandle::TrackPoint { index: 1 },
                ))
        })
        .unwrap()
        .hit;
    let current = world_tool_hit(DVec3::new(5.0, 6.0, 7.0), 10.0, 10.0);

    let outcome = apply_viewer_tool_commands(
        &mut state,
        vec![ViewerToolCommand::CommitSceneHandleDrag { handle, current }],
    )
    .unwrap();

    assert!(outcome.rerender_requested);
    assert_eq!(
        state.scene_artifacts.track(&track_id).unwrap().points[1].position_world,
        DVec3::new(5.0, 6.0, 7.0)
    );
    assert!(state.scene_artifacts.can_undo());
}

#[test]
fn app_viewport_handle_commit_updates_annotation_geometry_artifact() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    state.scene_artifacts = sample_scene_artifacts();
    let annotation_id = SceneArtifactId::new("annotation", "note-a").unwrap();
    select_scene_artifact(
        &mut state,
        EditableSceneArtifactKind::Annotation,
        &annotation_id,
    );
    let handle = selected_scene_handle_pick_targets(&state)
        .unwrap()
        .into_iter()
        .find(|target| {
            target.hit.value
                == Some(scene_handle_pick_value(
                    EditableSceneArtifactKind::Annotation,
                    SceneEditHandle::WorldPointPosition,
                ))
        })
        .unwrap()
        .hit;
    let current = world_tool_hit(DVec3::new(3.0, 4.0, 5.0), 10.0, 10.0);

    apply_viewer_tool_commands(
        &mut state,
        vec![ViewerToolCommand::CommitSceneHandleDrag { handle, current }],
    )
    .unwrap();

    let annotation = state.scene_artifacts.annotation(&annotation_id).unwrap();
    assert_eq!(
        annotation.geometry,
        AnalysisWorldGeometry::Point {
            position: DVec3::new(3.0, 4.0, 5.0),
            radius_px: 4.0,
        }
    );
}

#[test]
fn analysis_plot_view_stays_within_full_bounds() {
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

#[test]
fn analysis_plot_projection_uses_time_left_to_right_and_mean_bottom_to_top() {
    let bounds = AnalysisPlotBounds {
        min_x: 0.0,
        max_x: 10.0,
        min_y: -5.0,
        max_y: 5.0,
    };
    let rect = egui::Rect::from_min_max(egui::pos2(20.0, 30.0), egui::pos2(120.0, 80.0));

    assert_eq!(
        plot_screen_position(0.0, -5.0, bounds, rect),
        rect.left_bottom()
    );
    assert_eq!(
        plot_screen_position(10.0, 5.0, bounds, rect),
        rect.right_top()
    );
}

#[test]
fn workspace_helpers_are_typed_to_exact_core_results() {
    use mirante4d_analysis_core::{AnalysisPlot, AnalysisTable};

    let _: fn(&AnalysisPlot) -> Option<AnalysisPlotBounds> = analysis_plot_bounds;
    let _: fn(
        &AnalysisPlot,
        AnalysisPlotBounds,
        egui::Rect,
        egui::Pos2,
    ) -> Option<analysis_workspace::AnalysisPlotNearestPoint> = nearest_analysis_plot_point;
    let _: fn(
        &AnalysisTable,
        &str,
        Option<&AnalysisTableSort>,
    ) -> analysis_workspace::AnalysisTablePreviewRows = analysis_table_preview_rows;
}

#[test]
fn exact_analysis_controls_fit_both_supported_window_sizes() {
    use egui_kittest::{Harness, kittest::Queryable};

    for size in [egui::vec2(1280.0, 720.0), egui::vec2(1920.0, 1080.0)] {
        let temp = tempfile::tempdir().unwrap();
        let root = write_target_fixture(temp.path()).unwrap();
        let opened = open_dataset_and_render_first_frame(root).unwrap();
        let harness = Harness::builder()
            .with_size(size)
            .with_pixels_per_point(1.0)
            .build_eframe(|cc| test_workbench_app_for_ui_harness(cc, opened));

        harness.get_by_label("Analyze Time");
        harness.get_by_label("Analyze Box");
        harness.get_by_label("Workspace");
        harness.get_by_label("Copy CSV");
        harness.get_by_label("Box coordinates (z, y, x voxels)");
    }
}

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

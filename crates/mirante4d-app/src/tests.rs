use std::{
    fs::{self, File},
    path::PathBuf,
    time::Duration,
};

use crate::brick_streaming::{
    apply_brick_read_outcome, apply_brick_read_outcomes, current_resident_frame_ready,
    stream_layer_ids_for_state, submit_visible_bricks_to_pool,
};
use crate::image_compositing::{color_image_for_state, missing_typed_payload_is_reportable_error};
use eframe::egui;
use mirante4d_analysis::{AnalysisColumn, AnalysisPlotPoint, AnalysisPlotSeries, AnalysisTableRow};
use mirante4d_format::{
    ChannelMetadata, DenseF32Layer, DenseU16Layer, DenseU16MultiscaleLayer, DenseU16Scale,
    ExistingPackagePolicy, FixtureKind, NativeF32Dataset, NativeU16Dataset,
    NativeU16MultiscaleDataset, ScaleReduction, default_u16_display, expected_fixture_value,
    write_fixture, write_native_f32_dataset, write_native_u16_dataset,
    write_native_u16_multiscale_dataset,
};
use tiff::encoder::{TiffEncoder, colortype};
use tiff::tags::Tag;

use crate::dataset_opening::{TEST_DENSE_STARTUP_VOXEL_LIMIT, TEST_INITIAL_RENDER_VIEWPORT_SIDE};

use super::*;

fn test_initial_render_viewport() -> RenderViewport {
    RenderViewport::new(
        TEST_INITIAL_RENDER_VIEWPORT_SIDE,
        TEST_INITIAL_RENDER_VIEWPORT_SIDE,
    )
    .expect("the fixed test viewport is valid")
}

fn open_dataset_and_render_first_frame(
    path: impl AsRef<std::path::Path>,
) -> anyhow::Result<AppState> {
    open_dataset_with_preferences_and_render_first_frame(path, &AppPreferences::default())
}

fn open_dataset_with_preferences_and_render_first_frame(
    path: impl AsRef<std::path::Path>,
    preferences: &AppPreferences,
) -> anyhow::Result<AppState> {
    crate::dataset_opening::open_test_dataset_with_preferences_and_render_first_frame(
        path,
        preferences,
        test_initial_render_viewport(),
        TEST_DENSE_STARTUP_VOXEL_LIMIT,
    )
}

fn test_workbench_app_without_background_runtime(state: AppState) -> MiranteWorkbenchApp {
    let clean_project_snapshot = ProjectDirtySnapshot::from_state(&state);
    MiranteWorkbenchApp {
        state,
        current_project_path: None,
        clean_project_snapshot,
        close_prompt_open: false,
        allow_close_without_prompt: false,
        preferences: AppPreferences::default(),
        preferences_path: None,
        settings_runtime_draft: AppRuntimePreferences::default(),
        settings_message: None,
        texture: None,
        gpu_display_frame: None,
        gpu_display_frame_identity: None,
        gpu_display_texture_id: None,
        cross_section_gpu_display_frames: BTreeMap::new(),
        retired_gpu_display_texture_ids: Vec::new(),
        wgpu_texture_renderer: None,
        last_display_refresh_timing: None,
        gpu_renderer: None,
        brick_read_pool: None,
        cross_section_read_pool: None,
        current_brick_tickets: Vec::new(),
        prefetch_brick_tickets: Vec::new(),
        warm_brick_tickets: Vec::new(),
        tiff_import_setup_task: None,
        tiff_import_setup_error: None,
        pending_tiff_import: None,
        import_task: None,
        analysis_task: None,
        analysis_workspace_open: false,
        product_automation: None,
        playback: PlaybackState::default(),
    }
}

fn world_tool_hit(world: DVec3, screen_x: f32, screen_y: f32) -> PickHit {
    PickHit {
        kind: PickHitKind::Voxel,
        layer_id: None,
        object_id: None,
        source_layer_id: None,
        timepoint: TimeIndex(0),
        world_position: Some(world),
        grid_position: None,
        screen_position: Some(ScreenPosition::new(screen_x, screen_y)),
        value: None,
        policy: PickPolicy::SceneObject,
        completeness: PickCompleteness::Exact,
    }
}

fn assert_cross_section_state_approx_eq(
    actual: mirante4d_renderer::CrossSectionViewState,
    expected: mirante4d_renderer::CrossSectionViewState,
) {
    let epsilon = 1.0e-12;
    assert!(
        (actual.center_world - expected.center_world).length() <= epsilon,
        "center mismatch: actual={:?}, expected={:?}",
        actual.center_world,
        expected.center_world
    );

    let actual_orientation = actual.orientation.to_array();
    let expected_orientation = expected.orientation.to_array();
    let same_orientation = actual_orientation
        .iter()
        .zip(expected_orientation.iter())
        .all(|(actual, expected)| (actual - expected).abs() <= epsilon);
    let opposite_orientation = actual_orientation
        .iter()
        .zip(expected_orientation.iter())
        .all(|(actual, expected)| (actual + expected).abs() <= epsilon);
    assert!(
        same_orientation || opposite_orientation,
        "orientation mismatch: actual={:?}, expected={:?}",
        actual.orientation,
        expected.orientation
    );

    assert!(
        (actual.scale_world_per_screen_point - expected.scale_world_per_screen_point).abs()
            <= epsilon,
        "scale mismatch: actual={}, expected={}",
        actual.scale_world_per_screen_point,
        expected.scale_world_per_screen_point
    );
    assert!(
        (actual.depth_world - expected.depth_world).abs() <= epsilon,
        "depth mismatch: actual={}, expected={}",
        actual.depth_world,
        expected.depth_world
    );
}

include!("tests/fidelity_shell.rs");
include!("tests/architecture.rs");
include!("tests/channels_project.rs");
include!("tests/analysis_workspace.rs");
include!("tests/scene_tools.rs");
include!("tests/streaming_lod.rs");
include!("tests/import_render_viewport.rs");

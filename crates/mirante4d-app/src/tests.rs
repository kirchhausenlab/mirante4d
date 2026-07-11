use std::{
    fs::{self, File},
    path::PathBuf,
    time::Duration,
};

use crate::brick_streaming::{
    apply_brick_read_outcome, apply_brick_read_outcomes, current_resident_frame_ready,
    submit_visible_bricks_to_pool,
};
use crate::image_compositing::color_image_for_snapshot;
use eframe::egui;
use mirante4d_analysis::{AnalysisColumn, AnalysisPlotPoint, AnalysisPlotSeries, AnalysisTableRow};
use mirante4d_format::{
    ChannelMetadata, DenseU16Layer, DenseU16MultiscaleLayer, DenseU16Scale, ExistingPackagePolicy,
    FixtureKind, NativeU16Dataset, NativeU16MultiscaleDataset, ScaleReduction, default_u16_display,
    expected_fixture_value, write_fixture, write_native_u16_dataset,
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
) -> anyhow::Result<dataset_opening::OpenedCurrentSource> {
    crate::dataset_opening::open_test_dataset_with_resource_policy_and_render_first_frame(
        path,
        ResourcePolicy::default(),
        test_initial_render_viewport(),
        TEST_DENSE_STARTUP_VOXEL_LIMIT,
    )
}

fn test_application_for_opened_source(
    opened: &dataset_opening::OpenedCurrentSource,
) -> ApplicationState {
    ApplicationState::new_unbound(
        SourceSessionGeneration::new(1),
        opened.catalog.as_ref().clone(),
        opened.workspace.clone(),
        ResourcePolicy::default(),
    )
    .expect("the opened test source must satisfy the canonical application model")
}

fn test_workbench_app_without_background_runtime(
    opened: dataset_opening::OpenedCurrentSource,
) -> MiranteWorkbenchApp {
    let application = test_application_for_opened_source(&opened);
    let dataset_opening::OpenedCurrentSource {
        startup_diagnostics,
        catalog: _,
        workspace: _,
        mut dataset_runtime,
        mut render_runtime,
        analysis_runtime,
    } = opened;
    let resource_policy = ResourcePolicy::default();
    let ui_runtime = current_runtime::ui::CurrentUiRuntime::new(resource_policy, None);
    rerender_state_with_backend(
        &application.snapshot(),
        &mut dataset_runtime,
        &analysis_runtime,
        &ui_runtime,
        &mut render_runtime,
        None,
    )
    .expect("the opened test source must render through the explicit test runtime owners");
    let (mut settings_connection, _) =
        current_settings_connection::CurrentSettingsConnection::start();
    settings_connection
        .shutdown()
        .expect("the test settings connection must stop before the harness starts");

    MiranteWorkbenchApp {
        application,
        startup_diagnostics,
        dataset_runtime,
        render_runtime,
        ui_runtime,
        project_runtime: current_runtime::project::CurrentProjectRuntime::unbound(),
        import_runtime: current_runtime::import::CurrentImportRuntime::idle(),
        analysis_runtime,
        validation_runtime: current_runtime::validation::CurrentValidationRuntime {
            product_automation: None,
            test_render_viewport_max_side: None,
        },
        project_persistence: None,
        settings_connection,
        source_open_service: None,
    }
}

fn test_workbench_app_for_ui_harness(
    cc: &eframe::CreationContext<'_>,
    opened: dataset_opening::OpenedCurrentSource,
) -> MiranteWorkbenchApp {
    ui_kit::configure_visuals(&cc.egui_ctx);
    let mut app = test_workbench_app_without_background_runtime(opened);
    app.validation_runtime.test_render_viewport_max_side =
        Some(TEST_INITIAL_RENDER_VIEWPORT_SIDE as usize);
    app
}

fn world_tool_hit(world: DVec3, screen_x: f32, screen_y: f32) -> PickHit {
    PickHit {
        kind: PickHitKind::Voxel,
        layer_id: None,
        object_id: None,
        source_layer_id: None,
        timepoint: TimeIndex::new(0),
        world_position: Some(world),
        grid_position: None,
        screen_position: Some(ScreenPosition::new(screen_x, screen_y)),
        value: None,
        policy: PickPolicy::SceneObject,
        completeness: PickCompleteness::Exact,
    }
}

include!("tests/fidelity_shell.rs");
include!("tests/architecture.rs");
include!("tests/channels_project.rs");
include!("tests/analysis_workspace.rs");
include!("tests/scene_tools.rs");
include!("tests/streaming_lod.rs");
include!("tests/import_render_viewport.rs");

use std::{fs, time::Duration};

use eframe::egui;
use mirante4d_analysis::{AnalysisColumn, AnalysisPlotPoint, AnalysisPlotSeries, AnalysisTableRow};
use mirante4d_format::{FixtureKind, write_fixture};

const TEST_INITIAL_RENDER_VIEWPORT_SIDE: u64 = 32;

use super::*;

fn open_dataset_and_render_first_frame(
    path: impl AsRef<std::path::Path>,
) -> anyhow::Result<unified_source_open::UnifiedOpenedSource> {
    crate::unified_source_open::open(path, ResourcePolicy::default(), DatasetSourceId::new(1))
}

fn test_application_for_opened_source(
    opened: &unified_source_open::UnifiedOpenedSource,
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
    opened: unified_source_open::UnifiedOpenedSource,
) -> MiranteWorkbenchApp {
    let application = test_application_for_opened_source(&opened);
    let unified_source_open::UnifiedOpenedSource {
        startup_diagnostics,
        catalog: _,
        workspace: _,
        dataset,
        render_runtime,
        analysis_runtime,
    } = opened;
    let resource_policy = ResourcePolicy::default();
    let ui_runtime = current_runtime::ui::CurrentUiRuntime::new(resource_policy, None);
    let (mut settings_connection, _) =
        current_settings_connection::CurrentSettingsConnection::start();
    settings_connection
        .shutdown()
        .expect("the test settings connection must stop before the harness starts");

    MiranteWorkbenchApp {
        application,
        startup_diagnostics,
        dataset,
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
        source_verification_service: None,
        pending_automatic_source_verification: None,
    }
}

fn test_workbench_app_for_ui_harness(
    cc: &eframe::CreationContext<'_>,
    opened: unified_source_open::UnifiedOpenedSource,
) -> MiranteWorkbenchApp {
    ui_kit::configure_visuals(&cc.egui_ctx);
    let mut app = test_workbench_app_without_background_runtime(opened);
    app.validation_runtime.test_render_viewport_max_side =
        Some(TEST_INITIAL_RENDER_VIEWPORT_SIDE as usize);
    app
}

include!("tests/fidelity_shell.rs");
include!("tests/architecture.rs");
include!("tests/channels_project.rs");
include!("tests/analysis_workspace.rs");
include!("tests/unified_runtime.rs");

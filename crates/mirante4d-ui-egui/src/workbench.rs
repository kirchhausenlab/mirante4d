use eframe::egui;
use mirante4d_application::{AnalysisWorkspaceSnapshot, viewer_tools::ViewerTool};

use crate::{
    DirtyProjectCloseView, EguiUiState, InspectorWorkbenchView, LeftWorkbenchView,
    ProjectRecoveryView, TopToolbarView, ViewerWorkbenchView, WorkbenchLayoutSpec,
    WorkbenchUiOutput, show_analysis_workspace_window, show_dirty_project_close_prompt,
    show_import_workflow_window, show_left_workbench_panel, show_project_recovery_ui,
    show_top_toolbar, show_workbench_inspector, show_workbench_viewer,
};

pub struct WorkbenchView<'a> {
    pub toolbar: TopToolbarView<'a>,
    pub left: LeftWorkbenchView<'a>,
    pub inspector: InspectorWorkbenchView<'a>,
    pub viewer: ViewerWorkbenchView<'a>,
    pub analysis_workspace: &'a AnalysisWorkspaceSnapshot,
    pub project_recovery: &'a ProjectRecoveryView,
    pub dirty_project_close: DirtyProjectCloseView,
}

pub fn show_workbench(
    ui: &mut egui::Ui,
    view: WorkbenchView<'_>,
    state: &mut EguiUiState,
) -> WorkbenchUiOutput {
    synchronize_viewer_tool(view.toolbar.application, state);

    let mut output = WorkbenchUiOutput::default();
    let layout = WorkbenchLayoutSpec::default();
    show_top_toolbar(ui, view.toolbar, &mut output);
    show_left_workbench_panel(ui, view.left, state, layout, &mut output);
    show_workbench_inspector(ui, view.inspector, state, layout, &mut output);
    show_workbench_viewer(ui, &view.viewer, state, &mut output);

    output
        .application_commands
        .extend(show_analysis_workspace_window(
            ui.ctx(),
            view.analysis_workspace,
            state,
        ));
    output.import_commands.extend(show_import_workflow_window(
        ui.ctx(),
        state,
        view.toolbar.application.import_workflow(),
    ));
    show_project_recovery_ui(ui.ctx(), view.project_recovery, &mut output.actions);
    show_dirty_project_close_prompt(ui.ctx(), view.dirty_project_close, &mut output.actions);
    output
}

fn synchronize_viewer_tool(
    application: &mirante4d_application::ApplicationSnapshot,
    state: &mut EguiUiState,
) {
    let canonical = match application.transient().active_tool() {
        mirante4d_application::ToolKind::Navigate => ViewerTool::Navigate,
        mirante4d_application::ToolKind::Inspect => ViewerTool::Inspect,
        mirante4d_application::ToolKind::Crosshair => ViewerTool::Crosshair,
        mirante4d_application::ToolKind::RoiBox => ViewerTool::RoiBox,
        mirante4d_application::ToolKind::MeasureDistance => ViewerTool::MeasureDistance,
    };
    if state.viewer_tools.active_tool != canonical {
        state.viewer_tools.set_active_tool(canonical);
    }
}

use eframe::egui;
use mirante4d_application::FrameFidelityStatus;

use crate::{WorkbenchUiAction, property_row, show_frame_fidelity_property_rows, toolbar_button};

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeDiagnosticsView {
    rows: Vec<(String, String)>,
    frame_fidelity: FrameFidelityStatus,
}

impl RuntimeDiagnosticsView {
    pub const fn new(rows: Vec<(String, String)>, frame_fidelity: FrameFidelityStatus) -> Self {
        Self {
            rows,
            frame_fidelity,
        }
    }
}

pub(crate) fn show_runtime_diagnostics_body(
    view: &RuntimeDiagnosticsView,
    ui: &mut egui::Ui,
    actions: &mut Vec<WorkbenchUiAction>,
) {
    if toolbar_button(ui, "Copy Diagnostics", true).clicked() {
        actions.push(WorkbenchUiAction::CopyDiagnostics);
    }
    for (label, value) in &view.rows {
        property_row(ui, label, value);
    }
    show_frame_fidelity_property_rows(ui, &view.frame_fidelity);
}

use anyhow::{Context, Result, ensure};
use mirante4d_application::{
    AnalysisPlotDescriptor, AnalysisPlotId, AnalysisPlotPointSelection, AnalysisPlotSnapshot,
    AnalysisTableDescriptor, AnalysisTableId, AnalysisTableSnapshot, AnalysisWorkspaceSnapshot,
};

use crate::analysis_session::AnalysisProductRuntime;

#[derive(Debug, Clone, Copy)]
pub(crate) struct AnalysisWorkspaceSnapshotInput<'a> {
    pub(crate) table_descriptors: &'a [AnalysisTableDescriptor],
    pub(crate) plot_descriptors: &'a [AnalysisPlotDescriptor],
    pub(crate) selected_table: Option<AnalysisTableId>,
    pub(crate) selected_plot: Option<AnalysisPlotId>,
    pub(crate) selected_plot_point: Option<AnalysisPlotPointSelection>,
}

pub(crate) fn analysis_workspace_snapshot(
    analysis: &AnalysisProductRuntime,
    input: AnalysisWorkspaceSnapshotInput<'_>,
) -> AnalysisWorkspaceSnapshot {
    AnalysisWorkspaceSnapshot::new(
        analysis.status_text().to_owned(),
        analysis
            .progress()
            .map(|progress| (progress.completed_blocks(), progress.total_blocks())),
        input
            .table_descriptors
            .iter()
            .map(|descriptor| {
                AnalysisTableSnapshot::new(descriptor.id(), analysis.table_handle(descriptor.id()))
            })
            .collect(),
        input
            .plot_descriptors
            .iter()
            .map(|descriptor| {
                AnalysisPlotSnapshot::new(descriptor.id(), analysis.plot_handle(descriptor.id()))
            })
            .collect(),
        input.selected_table,
        input.selected_plot,
        input.selected_plot_point,
        analysis.last_export_csv().map(str::len),
    )
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AnalysisTableExportInput<'a> {
    pub(crate) table_descriptors: &'a [AnalysisTableDescriptor],
    pub(crate) selected_table: Option<AnalysisTableId>,
}

pub(crate) fn export_selected_analysis_table(
    analysis: &mut AnalysisProductRuntime,
    input: AnalysisTableExportInput<'_>,
) -> Result<usize> {
    let selected = input
        .selected_table
        .context("no analysis table is selected for export")?;
    ensure!(
        input
            .table_descriptors
            .iter()
            .any(|descriptor| descriptor.id() == selected),
        "selected analysis table is not in the durable project catalog"
    );
    analysis
        .export_selected_table_csv(Some(selected))
        .map(str::len)
}

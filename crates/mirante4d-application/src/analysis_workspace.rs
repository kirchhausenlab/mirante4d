use std::sync::Arc;

use mirante4d_analysis_core::{AnalysisPlot, AnalysisTable};

use crate::{AnalysisPlotId, AnalysisPlotPointSelection, AnalysisTableId};

/// One immutable analysis table available to the visible workbench.
#[derive(Debug, Clone)]
pub struct AnalysisTableSnapshot {
    id: AnalysisTableId,
    table: Option<Arc<AnalysisTable>>,
}

impl AnalysisTableSnapshot {
    pub const fn new(id: AnalysisTableId, table: Option<Arc<AnalysisTable>>) -> Self {
        Self { id, table }
    }

    pub const fn id(&self) -> AnalysisTableId {
        self.id
    }

    pub fn table(&self) -> Option<&AnalysisTable> {
        self.table.as_deref()
    }
}

/// One immutable analysis plot available to the visible workbench.
#[derive(Debug, Clone)]
pub struct AnalysisPlotSnapshot {
    id: AnalysisPlotId,
    plot: Option<Arc<AnalysisPlot>>,
}

impl AnalysisPlotSnapshot {
    pub const fn new(id: AnalysisPlotId, plot: Option<Arc<AnalysisPlot>>) -> Self {
        Self { id, plot }
    }

    pub const fn id(&self) -> AnalysisPlotId {
        self.id
    }

    pub fn plot(&self) -> Option<&AnalysisPlot> {
        self.plot.as_deref()
    }
}

/// Immutable, zero-copy analysis results presented by one workbench frame.
#[derive(Debug, Clone)]
pub struct AnalysisWorkspaceSnapshot {
    status_text: String,
    progress_blocks: Option<(u64, u64)>,
    tables: Vec<AnalysisTableSnapshot>,
    plots: Vec<AnalysisPlotSnapshot>,
    selected_table: Option<AnalysisTableId>,
    selected_plot: Option<AnalysisPlotId>,
    selected_plot_point: Option<AnalysisPlotPointSelection>,
    last_export_csv_bytes: Option<usize>,
}

impl AnalysisWorkspaceSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        status_text: String,
        progress_blocks: Option<(u64, u64)>,
        tables: Vec<AnalysisTableSnapshot>,
        plots: Vec<AnalysisPlotSnapshot>,
        selected_table: Option<AnalysisTableId>,
        selected_plot: Option<AnalysisPlotId>,
        selected_plot_point: Option<AnalysisPlotPointSelection>,
        last_export_csv_bytes: Option<usize>,
    ) -> Self {
        Self {
            status_text,
            progress_blocks,
            tables,
            plots,
            selected_table,
            selected_plot,
            selected_plot_point,
            last_export_csv_bytes,
        }
    }

    pub fn status_text(&self) -> &str {
        &self.status_text
    }

    pub const fn progress_blocks(&self) -> Option<(u64, u64)> {
        self.progress_blocks
    }

    pub fn tables(&self) -> &[AnalysisTableSnapshot] {
        &self.tables
    }

    pub fn plots(&self) -> &[AnalysisPlotSnapshot] {
        &self.plots
    }

    pub const fn selected_table(&self) -> Option<AnalysisTableId> {
        self.selected_table
    }

    pub const fn selected_plot(&self) -> Option<AnalysisPlotId> {
        self.selected_plot
    }

    pub const fn selected_plot_point(&self) -> Option<AnalysisPlotPointSelection> {
        self.selected_plot_point
    }

    pub const fn last_export_csv_bytes(&self) -> Option<usize> {
        self.last_export_csv_bytes
    }

    pub fn table(&self, id: AnalysisTableId) -> Option<&AnalysisTable> {
        self.tables
            .iter()
            .find(|entry| entry.id() == id)
            .and_then(AnalysisTableSnapshot::table)
    }

    pub fn plot(&self, id: AnalysisPlotId) -> Option<&AnalysisPlot> {
        self.plots
            .iter()
            .find(|entry| entry.id() == id)
            .and_then(AnalysisPlotSnapshot::plot)
    }
}

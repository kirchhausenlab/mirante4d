//! Passive analysis results retained until the WP-12 runtime rebuild.

use mirante4d_analysis::{
    AnalysisOperationRecord, AnalysisPlot, AnalysisTable, IntensitySummary, SceneArtifactStore,
};

pub(crate) const ANALYSIS_EXECUTION_DEFERRED_MESSAGE: &str =
    "Analysis execution is deferred until WP-12.";

/// Passive analysis results and scene state retained until WP-12.
pub(crate) struct CurrentAnalysisRuntime {
    pub(crate) active_intensity_summary: IntensitySummary,
    pub(crate) analysis_tables: Vec<AnalysisTable>,
    pub(crate) analysis_plots: Vec<AnalysisPlot>,
    pub(crate) analysis_operations: Vec<AnalysisOperationRecord>,
    pub(crate) scene_artifacts: SceneArtifactStore,
    pub(crate) last_analysis_export_csv: Option<String>,
}

impl CurrentAnalysisRuntime {
    pub(crate) fn empty(active_intensity_summary: IntensitySummary) -> Self {
        Self {
            active_intensity_summary,
            analysis_tables: Vec::new(),
            analysis_plots: Vec::new(),
            analysis_operations: Vec::new(),
            scene_artifacts: SceneArtifactStore::default(),
            last_analysis_export_csv: None,
        }
    }
}

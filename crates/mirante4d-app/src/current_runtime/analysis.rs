//! Current analysis payload/execution facts retained only until WP-12.

use std::collections::HashMap;

use mirante4d_analysis::{
    AnalysisOperationRecord, AnalysisPlot, AnalysisTable, IntensitySummary, SceneArtifactStore,
};
use mirante4d_data::BrickHistogramSample;

use crate::{AnalysisTask, LayerHistogramCache, ResidentHistogramSampleKey};

/// Exact ten-field temporary owner frozen by the WP-07B entry.
pub(crate) struct CurrentAnalysisRuntime {
    pub(crate) resident_histogram_generation: u64,
    pub(crate) resident_histogram_samples:
        HashMap<ResidentHistogramSampleKey, BrickHistogramSample>,
    pub(crate) active_histogram_cache: Option<LayerHistogramCache>,
    pub(crate) active_intensity_summary: IntensitySummary,
    pub(crate) analysis_tables: Vec<AnalysisTable>,
    pub(crate) analysis_plots: Vec<AnalysisPlot>,
    pub(crate) analysis_operations: Vec<AnalysisOperationRecord>,
    pub(crate) scene_artifacts: SceneArtifactStore,
    pub(crate) last_analysis_export_csv: Option<String>,
    pub(crate) analysis_task: Option<AnalysisTask>,
}

impl CurrentAnalysisRuntime {
    pub(crate) fn empty(active_intensity_summary: IntensitySummary) -> Self {
        Self {
            resident_histogram_generation: 0,
            resident_histogram_samples: HashMap::new(),
            active_histogram_cache: None,
            active_intensity_summary,
            analysis_tables: Vec::new(),
            analysis_plots: Vec::new(),
            analysis_operations: Vec::new(),
            scene_artifacts: SceneArtifactStore::default(),
            last_analysis_export_csv: None,
            analysis_task: None,
        }
    }
}

use mirante4d_data::{DenseVolumeF32, DenseVolumeU8, DenseVolumeU16};
use thiserror::Error;

mod operations;
mod results;
mod scene_artifacts;

pub use operations::{
    ANALYSIS_OPERATION_SCHEMA_VERSION, AnalysisExecutionState, AnalysisOperationInput,
    AnalysisOperationKind, AnalysisOperationRecord, AnalysisParameterValue, AnalysisSpatialScope,
    AnalysisTimeScope, FULL_INTENSITY_SUMMARY_OPERATION_VERSION, ROI_INTENSITY_OPERATION_VERSION,
};
pub use results::{
    AnalysisCell, AnalysisColumn, AnalysisExecutionClass, AnalysisPlot, AnalysisPlotPoint,
    AnalysisPlotSeries, AnalysisProvenance, AnalysisResultState, AnalysisTable,
    AnalysisTableExportMetadata, AnalysisTableExportPolicy, AnalysisTableRow,
    IntensitySummaryAccumulator, IntensitySummaryF32Accumulator, RoiIntensityStatistics,
    RoiIntensityStatisticsF32, box_roi_f32_grid_region, box_roi_grid_region,
    box_roi_u16_grid_region, empty_roi_intensity_statistics, empty_roi_intensity_statistics_f32,
    export_plot_svg, export_table_csv, export_table_metadata_json,
    final_intensity_summary_f32_table, final_intensity_summary_table,
    final_roi_intensity_f32_table, final_roi_intensity_table, intensity_summary_columns,
    intensity_summary_f32_columns, intensity_summary_f32_row, intensity_summary_row,
    measure_box_roi_f32, measure_box_roi_u16, roi_intensity_columns, roi_intensity_f32_columns,
    roi_intensity_f32_row, roi_intensity_row, summarize_f32_volume_as_roi,
    summarize_f32_volume_for_analysis, summarize_u8_volume_as_roi, summarize_u16_volume_as_roi,
    summarize_volume_for_analysis, table_export_metadata, time_trace_plot_from_table,
    write_table_csv_with_metadata,
};
pub use scene_artifacts::{
    AnnotationArtifact, MeasurementArtifact, MeasurementGeometry, MeasurementProvenance,
    MeasurementResult, RoiArtifact, SceneArtifactId, SceneArtifactStore, SceneArtifactTime,
    SceneEditCommand, SceneStyleRgba, TrackArtifact, TrackPoint, TrackSegment, TrackTrailWindow,
    WorldBounds, WorldGeometry,
};

#[derive(Debug, Error, PartialEq)]
pub enum AnalysisError {
    #[error(transparent)]
    Space(#[from] mirante4d_core::SpaceError),
    #[error("no scene edit is available to undo")]
    UndoUnavailable,
    #[error("no scene edit is available to redo")]
    RedoUnavailable,
    #[error(
        "{kind} scene artifact id must contain only ASCII letters, digits, '-' or '_', got {value:?}"
    )]
    InvalidSceneArtifactId { kind: &'static str, value: String },
    #[error("{kind} scene artifact {id:?} already exists")]
    DuplicateSceneArtifact { kind: &'static str, id: String },
    #[error("{kind} scene artifact {id:?} was not found")]
    MissingSceneArtifact { kind: &'static str, id: String },
    #[error("scene time interval must satisfy start < end, got start={start}, end={end_exclusive}")]
    InvalidSceneTimeInterval { start: u64, end_exclusive: u64 },
    #[error("track artifact {id:?} must contain at least one point")]
    EmptyTrackArtifact { id: String },
    #[error("track artifact {id:?} point times must be strictly increasing")]
    NonMonotonicTrackTimes { id: String },
    #[error("scene geometry is invalid: {0}")]
    InvalidSceneGeometry(&'static str),
    #[error("scene color components must be finite values in [0, 1], got {0:?}")]
    InvalidSceneColor([f32; 4]),
    #[error("analysis geometry is unsupported: {0}")]
    UnsupportedAnalysisGeometry(&'static str),
    #[error("analysis table is invalid: {0}")]
    InvalidAnalysisTable(&'static str),
    #[error("analysis operation is invalid: {0}")]
    InvalidAnalysisOperation(String),
    #[error("analysis export path already exists: {0}")]
    AnalysisExportExists(String),
    #[error("analysis export failed: {0}")]
    AnalysisExportFailed(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IntensitySummary {
    pub voxel_count: u64,
    pub geometric_voxel_count: u64,
    pub nonzero_count: u64,
    pub min: u16,
    pub max: u16,
    pub mean: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IntensitySummaryF32 {
    pub voxel_count: u64,
    pub geometric_voxel_count: u64,
    pub nonzero_count: u64,
    pub min: f32,
    pub max: f32,
    pub sum: f64,
    pub mean: f64,
}

pub fn summarize_u16_volume(volume: &DenseVolumeU16) -> IntensitySummary {
    let mut accumulator = IntensitySummaryAccumulator::default();
    accumulator.include_volume(volume);
    accumulator.finish()
}

pub fn summarize_u8_volume(volume: &DenseVolumeU8) -> IntensitySummary {
    let mut accumulator = IntensitySummaryAccumulator::default();
    accumulator.include_u8_volume(volume);
    accumulator.finish()
}

pub fn summarize_f32_volume(volume: &DenseVolumeF32) -> IntensitySummaryF32 {
    let mut accumulator = IntensitySummaryF32Accumulator::default();
    accumulator.include_volume(volume);
    accumulator.finish()
}

#[cfg(test)]
mod tests;

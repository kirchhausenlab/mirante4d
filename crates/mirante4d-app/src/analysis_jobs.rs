use std::{
    collections::BTreeMap,
    sync::mpsc::{self, Receiver},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use mirante4d_analysis::{
    AnalysisExecutionClass, AnalysisExecutionState, AnalysisOperationInput, AnalysisOperationKind,
    AnalysisOperationRecord, AnalysisParameterValue, AnalysisPlot, AnalysisProvenance,
    AnalysisResultState, AnalysisSpatialScope, AnalysisTable, AnalysisTimeScope,
    FULL_INTENSITY_SUMMARY_OPERATION_VERSION, IntensitySummary, IntensitySummaryAccumulator,
    IntensitySummaryF32, IntensitySummaryF32Accumulator, ROI_INTENSITY_OPERATION_VERSION,
    RoiArtifact, box_roi_grid_region, empty_roi_intensity_statistics,
    empty_roi_intensity_statistics_f32, final_intensity_summary_f32_table,
    final_intensity_summary_table, final_roi_intensity_f32_table, final_roi_intensity_table,
    intensity_summary_f32_row, intensity_summary_row, roi_intensity_f32_row, roi_intensity_row,
    summarize_f32_volume_as_roi, summarize_u8_volume_as_roi, summarize_u16_volume_as_roi,
    time_trace_plot_from_table,
};
use mirante4d_application::OperationToken;
use mirante4d_data::{CancellationToken, DatasetHandle, SpatialBrickIndex};
use mirante4d_domain::{IntensityDType, TimeIndex};
use mirante4d_format::LayerId;

use crate::{
    SOURCE_ANALYSIS_SCALE_LEVEL,
    current_runtime::{analysis::CurrentAnalysisRuntime, dataset::CurrentDatasetRuntime},
    import_ui::no_data_policy_label,
};

pub(crate) struct AnalysisTask {
    pub(crate) token: OperationToken,
    pub(crate) kind: AnalysisTaskKind,
    pub(crate) cancellation: CancellationToken,
    pub(crate) receiver: Receiver<AnalysisTaskMessage>,
    pub(crate) latest_progress: Option<AnalysisProgress>,
}

impl Drop for AnalysisTask {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnalysisTaskKind {
    FullTimeSeries,
    RoiIntensity,
}

pub(crate) enum AnalysisTaskMessage {
    Progress(AnalysisProgress),
    Finished(Box<Result<AnalysisTaskOutput, String>>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnalysisProgress {
    pub(crate) completed: u64,
    pub(crate) total: u64,
    pub(crate) label: String,
}

#[derive(Debug, Clone)]
pub(crate) struct AnalysisTaskOutput {
    pub(crate) operation: AnalysisOperationRecord,
    pub(crate) table: AnalysisTable,
    pub(crate) plot: Option<AnalysisPlot>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AnalysisJobInput<'a> {
    pub(crate) dataset_name: &'a str,
    pub(crate) active_layer_id: &'a str,
    pub(crate) active_layer_dtype: IntensityDType,
    pub(crate) active_timepoint: TimeIndex,
    pub(crate) timepoint_count: u64,
}

#[derive(Clone)]
pub(crate) struct AnalysisJobContext {
    dataset_id: String,
    dataset_name: String,
    native_format: String,
    native_schema_version: u32,
    app_version: String,
    dataset: DatasetHandle,
    active_layer_id: String,
    active_no_data_policy_label: Option<String>,
    active_layer_dtype: IntensityDType,
    active_timepoint: TimeIndex,
    timepoint_count: u64,
    scene_artifacts: mirante4d_analysis::SceneArtifactStore,
}

impl AnalysisJobContext {
    pub(crate) fn from_runtime(
        dataset: &CurrentDatasetRuntime,
        analysis: &CurrentAnalysisRuntime,
        input: AnalysisJobInput<'_>,
    ) -> Self {
        let manifest = dataset.dataset.manifest();
        let active_no_data_policy_label = manifest
            .layers
            .iter()
            .find(|layer| layer.id == input.active_layer_id)
            .and_then(|layer| layer.no_data_policy.as_ref())
            .map(no_data_policy_label);
        Self {
            dataset_id: manifest.dataset.id.clone(),
            dataset_name: input.dataset_name.to_owned(),
            native_format: manifest.format.clone(),
            native_schema_version: manifest.schema_version,
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            dataset: dataset.dataset.clone(),
            active_layer_id: input.active_layer_id.to_owned(),
            active_no_data_policy_label,
            active_layer_dtype: input.active_layer_dtype,
            active_timepoint: input.active_timepoint,
            timepoint_count: input.timepoint_count,
            scene_artifacts: analysis.scene_artifacts.clone(),
        }
    }
}

pub(crate) fn spawn_analysis_task(
    token: OperationToken,
    dataset: &CurrentDatasetRuntime,
    analysis: &CurrentAnalysisRuntime,
    input: AnalysisJobInput<'_>,
    kind: AnalysisTaskKind,
) -> AnalysisTask {
    let context = AnalysisJobContext::from_runtime(dataset, analysis, input);
    let cancellation = CancellationToken::new();
    let worker_cancellation = cancellation.clone();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = match kind {
            AnalysisTaskKind::FullTimeSeries => {
                run_full_time_series_analysis_job(&context, &worker_cancellation, |progress| {
                    sender
                        .send(AnalysisTaskMessage::Progress(progress))
                        .map_err(|_| anyhow::anyhow!("analysis receiver closed"))
                })
            }
            AnalysisTaskKind::RoiIntensity => {
                run_roi_intensity_analysis_job(&context, &worker_cancellation, |progress| {
                    sender
                        .send(AnalysisTaskMessage::Progress(progress))
                        .map_err(|_| anyhow::anyhow!("analysis receiver closed"))
                })
            }
        }
        .map_err(|err| err.to_string());
        let _ = sender.send(AnalysisTaskMessage::Finished(Box::new(result)));
    });
    AnalysisTask {
        token,
        kind,
        cancellation,
        receiver,
        latest_progress: None,
    }
}

pub(crate) fn run_full_time_series_analysis_job<F>(
    context: &AnalysisJobContext,
    cancellation: &CancellationToken,
    progress: F,
) -> anyhow::Result<AnalysisTaskOutput>
where
    F: FnMut(AnalysisProgress) -> anyhow::Result<()>,
{
    let layer_id = LayerId::new(context.active_layer_id.clone())?;
    match context.active_layer_dtype {
        IntensityDType::Uint8 | IntensityDType::Uint16 => {
            run_u16_full_time_series_analysis_job(context, cancellation, progress, &layer_id)
        }
        IntensityDType::Float32 => {
            run_f32_full_time_series_analysis_job(context, cancellation, progress, &layer_id)
        }
    }
}

fn run_u16_full_time_series_analysis_job<F>(
    context: &AnalysisJobContext,
    cancellation: &CancellationToken,
    mut progress: F,
    layer_id: &LayerId,
) -> anyhow::Result<AnalysisTaskOutput>
where
    F: FnMut(AnalysisProgress) -> anyhow::Result<()>,
{
    let mut rows = Vec::with_capacity(context.timepoint_count as usize);
    for timepoint in 0..context.timepoint_count {
        check_analysis_cancelled(cancellation)?;
        rows.push(intensity_summary_row(
            timepoint,
            stream_u16_timepoint_summary(
                context,
                layer_id,
                TimeIndex::new(timepoint),
                cancellation,
            )?,
        ));
        progress(AnalysisProgress {
            completed: timepoint + 1,
            total: context.timepoint_count,
            label: format!(
                "Analyzed timepoint {}/{}",
                timepoint + 1,
                context.timepoint_count
            ),
        })?;
    }
    check_analysis_cancelled(cancellation)?;
    let mut provenance = analysis_provenance_from_context(
        context,
        "full_intensity_summary",
        format!("all timepoints 0..{}", context.timepoint_count),
        AnalysisExecutionClass::FullScopeBatch,
        AnalysisResultState::Complete,
        BTreeMap::from([
            (
                "timepoint_count".to_owned(),
                context.timepoint_count.to_string(),
            ),
            (
                "stored_dtype".to_owned(),
                format!("{:?}", context.active_layer_dtype),
            ),
            (
                "compute_path".to_owned(),
                u16_streaming_time_series_compute_path().to_owned(),
            ),
        ]),
    );
    provenance.timepoint_start = 0;
    provenance.timepoint_end_exclusive = context.timepoint_count;
    provenance.scale_level = SOURCE_ANALYSIS_SCALE_LEVEL;
    provenance.compute_precision = u16_streaming_analysis_compute_precision().to_owned();
    let table = final_intensity_summary_table(
        "full-intensity-summary",
        "Full Intensity Summary",
        provenance.clone(),
        rows,
    )?;
    let plot = time_trace_plot_from_table(
        "full-intensity-mean-trace",
        "Mean Intensity Trace",
        &table,
        "timepoint",
        "mean",
    )?;
    Ok(AnalysisTaskOutput {
        operation: complete_analysis_operation_record(
            context,
            "full-intensity-summary",
            FULL_INTENSITY_SUMMARY_OPERATION_VERSION,
            AnalysisOperationKind::FullIntensitySummary,
            AnalysisSpatialScope::WholeVolume,
            provenance,
        )?,
        table,
        plot: Some(plot),
    })
}

fn run_f32_full_time_series_analysis_job<F>(
    context: &AnalysisJobContext,
    cancellation: &CancellationToken,
    mut progress: F,
    layer_id: &LayerId,
) -> anyhow::Result<AnalysisTaskOutput>
where
    F: FnMut(AnalysisProgress) -> anyhow::Result<()>,
{
    let mut rows = Vec::with_capacity(context.timepoint_count as usize);
    for timepoint in 0..context.timepoint_count {
        check_analysis_cancelled(cancellation)?;
        rows.push(intensity_summary_f32_row(
            timepoint,
            stream_f32_timepoint_summary(
                context,
                layer_id,
                TimeIndex::new(timepoint),
                cancellation,
            )?,
        ));
        progress(AnalysisProgress {
            completed: timepoint + 1,
            total: context.timepoint_count,
            label: format!(
                "Analyzed timepoint {}/{}",
                timepoint + 1,
                context.timepoint_count
            ),
        })?;
    }
    check_analysis_cancelled(cancellation)?;
    let mut provenance = analysis_provenance_from_context(
        context,
        "full_intensity_summary",
        format!("all timepoints 0..{}", context.timepoint_count),
        AnalysisExecutionClass::FullScopeBatch,
        AnalysisResultState::Complete,
        BTreeMap::from([
            (
                "timepoint_count".to_owned(),
                context.timepoint_count.to_string(),
            ),
            (
                "stored_dtype".to_owned(),
                format!("{:?}", context.active_layer_dtype),
            ),
            (
                "compute_path".to_owned(),
                f32_streaming_time_series_compute_path().to_owned(),
            ),
        ]),
    );
    provenance.timepoint_start = 0;
    provenance.timepoint_end_exclusive = context.timepoint_count;
    provenance.scale_level = SOURCE_ANALYSIS_SCALE_LEVEL;
    provenance.compute_precision = f32_streaming_analysis_compute_precision().to_owned();
    let table = final_intensity_summary_f32_table(
        "full-intensity-summary",
        "Full Intensity Summary",
        provenance.clone(),
        rows,
    )?;
    let plot = time_trace_plot_from_table(
        "full-intensity-mean-trace",
        "Mean Intensity Trace",
        &table,
        "timepoint",
        "mean",
    )?;
    Ok(AnalysisTaskOutput {
        operation: complete_analysis_operation_record(
            context,
            "full-intensity-summary",
            FULL_INTENSITY_SUMMARY_OPERATION_VERSION,
            AnalysisOperationKind::FullIntensitySummary,
            AnalysisSpatialScope::WholeVolume,
            provenance,
        )?,
        table,
        plot: Some(plot),
    })
}

pub(crate) fn run_roi_intensity_analysis_job<F>(
    context: &AnalysisJobContext,
    cancellation: &CancellationToken,
    progress: F,
) -> anyhow::Result<AnalysisTaskOutput>
where
    F: FnMut(AnalysisProgress) -> anyhow::Result<()>,
{
    check_analysis_cancelled(cancellation)?;
    let layer_id = LayerId::new(context.active_layer_id.clone())?;
    match context.active_layer_dtype {
        IntensityDType::Uint8 | IntensityDType::Uint16 => {
            run_u16_roi_intensity_analysis_job(context, cancellation, progress, &layer_id)
        }
        IntensityDType::Float32 => {
            run_f32_roi_intensity_analysis_job(context, cancellation, progress, &layer_id)
        }
    }
}

fn run_u16_roi_intensity_analysis_job<F>(
    context: &AnalysisJobContext,
    cancellation: &CancellationToken,
    mut progress: F,
    layer_id: &LayerId,
) -> anyhow::Result<AnalysisTaskOutput>
where
    F: FnMut(AnalysisProgress) -> anyhow::Result<()>,
{
    let rois = context
        .scene_artifacts
        .rois()
        .filter(|roi| roi.visible && roi.time.is_visible_at(context.active_timepoint))
        .cloned()
        .collect::<Vec<_>>();
    let total = rois.len() as u64;
    let mut rows = Vec::new();
    for (index, roi) in rois.iter().enumerate() {
        check_analysis_cancelled(cancellation)?;
        rows.push(roi_intensity_row(
            context.active_timepoint.get(),
            measure_u16_roi_with_data_engine(context, layer_id, roi)?,
        ));
        progress(AnalysisProgress {
            completed: index as u64 + 1,
            total,
            label: format!("Analyzed ROI {}/{}", index + 1, rois.len()),
        })?;
    }
    if rois.is_empty() {
        progress(AnalysisProgress {
            completed: 0,
            total: 0,
            label: "Analyzed 0 visible ROIs".to_owned(),
        })?;
    }
    check_analysis_cancelled(cancellation)?;
    let mut provenance = analysis_provenance_from_context(
        context,
        "roi_intensity_statistics",
        format!(
            "visible ROI artifacts at timepoint {}",
            context.active_timepoint.get()
        ),
        AnalysisExecutionClass::RoiLocalExact,
        AnalysisResultState::Complete,
        BTreeMap::from([
            ("roi_count".to_owned(), rows.len().to_string()),
            (
                "stored_dtype".to_owned(),
                format!("{:?}", context.active_layer_dtype),
            ),
            (
                "compute_path".to_owned(),
                u16_roi_analysis_compute_path().to_owned(),
            ),
        ]),
    );
    provenance.scale_level = SOURCE_ANALYSIS_SCALE_LEVEL;
    provenance.compute_precision = u16_roi_analysis_compute_precision().to_owned();
    let table = final_roi_intensity_table(
        "roi-intensity-statistics",
        "ROI Intensity Statistics",
        provenance.clone(),
        rows,
    )?;
    Ok(AnalysisTaskOutput {
        operation: complete_analysis_operation_record(
            context,
            "roi-intensity-statistics",
            ROI_INTENSITY_OPERATION_VERSION,
            AnalysisOperationKind::RoiIntensityStatistics,
            AnalysisSpatialScope::Roi {
                roi_id: format!("visible_rois_at_t{}", context.active_timepoint.get()),
            },
            provenance,
        )?,
        table,
        plot: None,
    })
}

fn run_f32_roi_intensity_analysis_job<F>(
    context: &AnalysisJobContext,
    cancellation: &CancellationToken,
    mut progress: F,
    layer_id: &LayerId,
) -> anyhow::Result<AnalysisTaskOutput>
where
    F: FnMut(AnalysisProgress) -> anyhow::Result<()>,
{
    let rois = context
        .scene_artifacts
        .rois()
        .filter(|roi| roi.visible && roi.time.is_visible_at(context.active_timepoint))
        .cloned()
        .collect::<Vec<_>>();
    let total = rois.len() as u64;
    let mut rows = Vec::new();
    for (index, roi) in rois.iter().enumerate() {
        check_analysis_cancelled(cancellation)?;
        rows.push(roi_intensity_f32_row(
            context.active_timepoint.get(),
            measure_f32_roi_with_data_engine(context, layer_id, roi)?,
        ));
        progress(AnalysisProgress {
            completed: index as u64 + 1,
            total,
            label: format!("Analyzed ROI {}/{}", index + 1, rois.len()),
        })?;
    }
    if rois.is_empty() {
        progress(AnalysisProgress {
            completed: 0,
            total: 0,
            label: "Analyzed 0 visible ROIs".to_owned(),
        })?;
    }
    check_analysis_cancelled(cancellation)?;
    let mut provenance = analysis_provenance_from_context(
        context,
        "roi_intensity_statistics",
        format!(
            "visible ROI artifacts at timepoint {}",
            context.active_timepoint.get()
        ),
        AnalysisExecutionClass::RoiLocalExact,
        AnalysisResultState::Complete,
        BTreeMap::from([
            ("roi_count".to_owned(), rows.len().to_string()),
            (
                "stored_dtype".to_owned(),
                format!("{:?}", context.active_layer_dtype),
            ),
            (
                "compute_path".to_owned(),
                f32_roi_analysis_compute_path().to_owned(),
            ),
        ]),
    );
    provenance.scale_level = SOURCE_ANALYSIS_SCALE_LEVEL;
    provenance.compute_precision = f32_roi_analysis_compute_precision().to_owned();
    let table = final_roi_intensity_f32_table(
        "roi-intensity-statistics",
        "ROI Intensity Statistics",
        provenance.clone(),
        rows,
    )?;
    Ok(AnalysisTaskOutput {
        operation: complete_analysis_operation_record(
            context,
            "roi-intensity-statistics",
            ROI_INTENSITY_OPERATION_VERSION,
            AnalysisOperationKind::RoiIntensityStatistics,
            AnalysisSpatialScope::Roi {
                roi_id: format!("visible_rois_at_t{}", context.active_timepoint.get()),
            },
            provenance,
        )?,
        table,
        plot: None,
    })
}

fn stream_u16_timepoint_summary(
    context: &AnalysisJobContext,
    layer_id: &LayerId,
    timepoint: TimeIndex,
    cancellation: &CancellationToken,
) -> anyhow::Result<IntensitySummary> {
    let brick_grid = context
        .dataset
        .brick_grid_shape_at_scale(layer_id, SOURCE_ANALYSIS_SCALE_LEVEL)?;
    let mut accumulator = IntensitySummaryAccumulator::default();
    for z in 0..brick_grid.z() {
        for y in 0..brick_grid.y() {
            for x in 0..brick_grid.x() {
                check_analysis_cancelled(cancellation)?;
                match context.active_layer_dtype {
                    IntensityDType::Uint8 => {
                        let brick = context.dataset.read_u8_brick_at_scale(
                            layer_id,
                            SOURCE_ANALYSIS_SCALE_LEVEL,
                            timepoint,
                            SpatialBrickIndex::new(z, y, x),
                        )?;
                        accumulator.include_u8_volume(&brick.volume);
                    }
                    IntensityDType::Uint16 => {
                        let brick = context.dataset.read_u16_brick_at_scale(
                            layer_id,
                            SOURCE_ANALYSIS_SCALE_LEVEL,
                            timepoint,
                            SpatialBrickIndex::new(z, y, x),
                        )?;
                        accumulator.include_volume(&brick.volume);
                    }
                    IntensityDType::Float32 => unreachable!("float32 uses the f32 summary path"),
                }
            }
        }
    }
    Ok(accumulator.finish())
}

fn stream_f32_timepoint_summary(
    context: &AnalysisJobContext,
    layer_id: &LayerId,
    timepoint: TimeIndex,
    cancellation: &CancellationToken,
) -> anyhow::Result<IntensitySummaryF32> {
    let brick_grid = context
        .dataset
        .brick_grid_shape_at_scale(layer_id, SOURCE_ANALYSIS_SCALE_LEVEL)?;
    let mut accumulator = IntensitySummaryF32Accumulator::default();
    for z in 0..brick_grid.z() {
        for y in 0..brick_grid.y() {
            for x in 0..brick_grid.x() {
                check_analysis_cancelled(cancellation)?;
                let brick = context.dataset.read_f32_brick_at_scale(
                    layer_id,
                    SOURCE_ANALYSIS_SCALE_LEVEL,
                    timepoint,
                    SpatialBrickIndex::new(z, y, x),
                )?;
                accumulator.include_volume(&brick.volume);
            }
        }
    }
    Ok(accumulator.finish())
}

fn measure_u16_roi_with_data_engine(
    context: &AnalysisJobContext,
    layer_id: &LayerId,
    roi: &RoiArtifact,
) -> anyhow::Result<mirante4d_analysis::RoiIntensityStatistics> {
    let shape = context
        .dataset
        .scale_shape(layer_id, SOURCE_ANALYSIS_SCALE_LEVEL)?;
    let grid_to_world = context
        .dataset
        .scale_grid_to_world(layer_id, SOURCE_ANALYSIS_SCALE_LEVEL)?;
    let Some(region) = box_roi_grid_region(grid_to_world, shape, roi)? else {
        return Ok(empty_roi_intensity_statistics(roi));
    };
    let volume = match context.active_layer_dtype {
        IntensityDType::Uint8 => {
            let volume =
                context
                    .dataset
                    .read_u8_region(layer_id, context.active_timepoint, region)?;
            return Ok(summarize_u8_volume_as_roi(&volume, roi.id.as_str()));
        }
        IntensityDType::Uint16 => {
            context
                .dataset
                .read_u16_region(layer_id, context.active_timepoint, region)?
        }
        IntensityDType::Float32 => unreachable!("float32 uses the f32 ROI path"),
    };
    Ok(summarize_u16_volume_as_roi(&volume, roi.id.as_str()))
}

fn measure_f32_roi_with_data_engine(
    context: &AnalysisJobContext,
    layer_id: &LayerId,
    roi: &RoiArtifact,
) -> anyhow::Result<mirante4d_analysis::RoiIntensityStatisticsF32> {
    let shape = context
        .dataset
        .scale_shape(layer_id, SOURCE_ANALYSIS_SCALE_LEVEL)?;
    let grid_to_world = context
        .dataset
        .scale_grid_to_world(layer_id, SOURCE_ANALYSIS_SCALE_LEVEL)?;
    let Some(region) = box_roi_grid_region(grid_to_world, shape, roi)? else {
        return Ok(empty_roi_intensity_statistics_f32(roi));
    };
    let volume = context
        .dataset
        .read_f32_region(layer_id, context.active_timepoint, region)?;
    Ok(summarize_f32_volume_as_roi(&volume, roi.id.as_str()))
}

fn u16_streaming_time_series_compute_path() -> &'static str {
    "cpu_exact_u16_brick_streaming_reference"
}

fn f32_streaming_time_series_compute_path() -> &'static str {
    "cpu_exact_f32_brick_streaming_reference"
}

fn u16_roi_analysis_compute_path() -> &'static str {
    "cpu_exact_u16_region_streaming_reference"
}

fn f32_roi_analysis_compute_path() -> &'static str {
    "cpu_exact_f32_region_streaming_reference"
}

pub(crate) fn u16_streaming_analysis_compute_precision() -> &'static str {
    "CPU f64 accumulation over source uint16 values"
}

pub(crate) fn f32_streaming_analysis_compute_precision() -> &'static str {
    "CPU f64 accumulation over source float32 values"
}

fn u16_roi_analysis_compute_precision() -> &'static str {
    "CPU f64 accumulation over source uint16 ROI region values"
}

fn f32_roi_analysis_compute_precision() -> &'static str {
    "CPU f64 accumulation over source float32 ROI region values"
}

pub(crate) fn store_analysis_task_output(
    analysis: &mut CurrentAnalysisRuntime,
    output: AnalysisTaskOutput,
) {
    let AnalysisTaskOutput {
        operation,
        table,
        plot,
    } = output;
    analysis.analysis_operations.push(operation);
    analysis.analysis_tables.push(table);
    if let Some(plot) = plot {
        analysis.analysis_plots.push(plot);
    }
}

#[cfg(test)]
pub(crate) fn compute_full_time_series_analysis(
    dataset: &CurrentDatasetRuntime,
    analysis: &mut CurrentAnalysisRuntime,
    input: AnalysisJobInput<'_>,
) -> anyhow::Result<()> {
    let context = AnalysisJobContext::from_runtime(dataset, analysis, input);
    let output =
        run_full_time_series_analysis_job(&context, &CancellationToken::new(), |_| Ok(()))?;
    store_analysis_task_output(analysis, output);
    Ok(())
}

#[cfg(test)]
pub(crate) fn compute_active_roi_analysis(
    dataset: &CurrentDatasetRuntime,
    analysis: &mut CurrentAnalysisRuntime,
    input: AnalysisJobInput<'_>,
) -> anyhow::Result<()> {
    let context = AnalysisJobContext::from_runtime(dataset, analysis, input);
    let output = run_roi_intensity_analysis_job(&context, &CancellationToken::new(), |_| Ok(()))?;
    store_analysis_task_output(analysis, output);
    Ok(())
}

pub(crate) fn analysis_provenance_from_context(
    context: &AnalysisJobContext,
    operation: impl Into<String>,
    scope: impl Into<String>,
    execution_class: AnalysisExecutionClass,
    result_state: AnalysisResultState,
    mut parameters: BTreeMap<String, String>,
) -> AnalysisProvenance {
    let operation = operation.into();
    let operation_version = operation_version_for_name(&operation);
    if let Some(policy) = &context.active_no_data_policy_label {
        parameters.insert("no_data_policy".to_owned(), policy.clone());
        parameters.insert(
            "invalid_voxels".to_owned(),
            "render_invalid_excluded".to_owned(),
        );
    } else {
        parameters.insert("no_data_policy".to_owned(), "none".to_owned());
        parameters.insert("invalid_voxels".to_owned(), "not_applicable".to_owned());
    }
    AnalysisProvenance {
        source_dataset_id: context.dataset_id.clone(),
        source_dataset: context.dataset_name.clone(),
        native_format: context.native_format.clone(),
        native_schema_version: context.native_schema_version,
        app_version: context.app_version.clone(),
        created_at_utc: analysis_created_at_utc(),
        source_layer_id: context.active_layer_id.clone(),
        timepoint_start: context.active_timepoint.get(),
        timepoint_end_exclusive: context.active_timepoint.get().saturating_add(1),
        scale_level: SOURCE_ANALYSIS_SCALE_LEVEL,
        operation,
        operation_version,
        parameters,
        scope: scope.into(),
        execution_class,
        result_state,
        data_source: "data_engine_volume_reads".to_owned(),
        compute_precision: "f64 accumulation for means/sums".to_owned(),
    }
}

pub(crate) fn complete_analysis_operation_record(
    context: &AnalysisJobContext,
    operation_id: &str,
    operation_version: u32,
    kind: AnalysisOperationKind,
    spatial_scope: AnalysisSpatialScope,
    mut provenance: AnalysisProvenance,
) -> anyhow::Result<AnalysisOperationRecord> {
    provenance.operation_version = operation_version;
    let input = AnalysisOperationInput {
        dataset_id: context.dataset_id.clone(),
        dataset_name: context.dataset_name.clone(),
        native_format: context.native_format.clone(),
        native_schema_version: context.native_schema_version,
        layer_id: context.active_layer_id.clone(),
        time_scope: AnalysisTimeScope::new(
            provenance.timepoint_start,
            provenance.timepoint_end_exclusive,
        )?,
        scale_level: provenance.scale_level,
        spatial_scope,
    };
    let parameters = provenance
        .parameters
        .iter()
        .map(|(key, value)| (key.clone(), AnalysisParameterValue::Text(value.clone())))
        .collect::<BTreeMap<_, _>>();
    let record = AnalysisOperationRecord::new(
        operation_id,
        operation_version,
        kind,
        input,
        parameters,
        provenance.result_state,
    )?
    .with_execution_state(AnalysisExecutionState::Complete)
    .with_provenance(provenance);
    record.validate()?;
    Ok(record)
}

fn operation_version_for_name(operation: &str) -> u32 {
    match operation {
        "full_intensity_summary" => FULL_INTENSITY_SUMMARY_OPERATION_VERSION,
        "roi_intensity_statistics" => ROI_INTENSITY_OPERATION_VERSION,
        _ => 1,
    }
}

fn analysis_created_at_utc() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("unix_seconds:{}", duration.as_secs()),
        Err(_) => "unix_seconds:0".to_owned(),
    }
}

fn check_analysis_cancelled(cancellation: &CancellationToken) -> anyhow::Result<()> {
    if cancellation.is_cancelled() {
        anyhow::bail!("analysis was cancelled");
    }
    Ok(())
}

pub(crate) fn analysis_task_label(kind: AnalysisTaskKind) -> &'static str {
    match kind {
        AnalysisTaskKind::FullTimeSeries => "time-series analysis",
        AnalysisTaskKind::RoiIntensity => "ROI analysis",
    }
}

pub(crate) fn analysis_task_status_text(task: &AnalysisTask) -> String {
    task.latest_progress
        .as_ref()
        .map(|progress| progress.label.clone())
        .unwrap_or_else(|| format!("Running {}", analysis_task_label(task.kind)))
}

pub(crate) fn analysis_progress_fraction(progress: Option<&AnalysisProgress>) -> Option<f32> {
    let progress = progress?;
    if progress.total == 0 {
        return Some(1.0);
    }
    Some((progress.completed as f32 / progress.total as f32).clamp(0.0, 1.0))
}

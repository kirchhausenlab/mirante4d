use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use glam::DVec3;
use mirante4d_analysis::{
    AnalysisExecutionClass, AnalysisOperationInput, AnalysisOperationKind, AnalysisOperationRecord,
    AnalysisParameterValue, AnalysisPlot, AnalysisProvenance, AnalysisResultState,
    AnalysisSpatialScope, AnalysisTable, AnalysisTableExportPolicy, AnalysisTimeScope,
    FULL_INTENSITY_SUMMARY_OPERATION_VERSION, IntensitySummaryAccumulator,
    ROI_INTENSITY_OPERATION_VERSION, RoiArtifact, SceneArtifactId, SceneArtifactTime,
    WorldGeometry, box_roi_grid_region, export_plot_svg, final_intensity_summary_table,
    final_roi_intensity_table, intensity_summary_row, roi_intensity_row,
    summarize_u16_volume_as_roi, table_export_metadata, time_trace_plot_from_table,
    write_table_csv_with_metadata,
};
use mirante4d_data::{DatasetHandle, SpatialBrickIndex};
use mirante4d_domain::TimeIndex;
use mirante4d_format::LayerId;
use serde_json::{Value, json};

use crate::fixtures::generate_fixture;
use crate::host::benchmark_host_context;

pub(crate) fn phase15_audit() -> anyhow::Result<PathBuf> {
    let output_root = PathBuf::from("target").join("mirante4d").join("phase15");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let report_json = phase15_analysis_report(&output_root)?;
    let report_json_path = output_root.join("phase15-audit-report.json");
    let report_md_path = output_root.join("phase15-audit-report.md");
    fs::write(
        &report_json_path,
        format!("{}\n", serde_json::to_string_pretty(&report_json)?),
    )
    .with_context(|| format!("failed to write {}", report_json_path.display()))?;
    fs::write(&report_md_path, phase15_audit_markdown(&report_json))
        .with_context(|| format!("failed to write {}", report_md_path.display()))?;
    Ok(report_md_path)
}

pub(crate) fn bench_phase15_analysis() -> anyhow::Result<PathBuf> {
    let output_root = PathBuf::from("target")
        .join("mirante4d")
        .join("benchmarks")
        .join("phase15-analysis");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let report_json = phase15_analysis_report(&output_root)?;
    let report_json_path = output_root
        .parent()
        .context("phase15 benchmark output has no parent")?
        .join("bench-phase15-analysis.json");
    fs::write(
        &report_json_path,
        format!("{}\n", serde_json::to_string_pretty(&report_json)?),
    )
    .with_context(|| format!("failed to write {}", report_json_path.display()))?;
    Ok(report_json_path)
}

fn phase15_analysis_report(output_root: &Path) -> anyhow::Result<Value> {
    println!("phase15 audit: generated time-multichannel fixture analysis workflow");
    let fixture = generate_fixture("time-multichannel-u16-8cube-3t-2c")?;
    let dataset = DatasetHandle::open(&fixture)?;
    let manifest = dataset.manifest();
    let layer_id = dataset.first_layer_id()?;
    let before_full = dataset.stats()?;
    let (full_table, full_plot, full_operation) =
        phase15_full_intensity_analysis(&dataset, &layer_id)?;
    let after_full = dataset.stats()?;
    let before_roi = after_full;
    let (roi_table, roi_operation) = phase15_roi_analysis(&dataset, &layer_id)?;
    let after_roi = dataset.stats()?;

    let full_csv_path = output_root.join("phase15-full-intensity-summary.csv");
    let full_metadata_path = write_table_csv_with_metadata(
        &full_csv_path,
        &full_table,
        AnalysisTableExportPolicy::Replace,
    )?;
    let roi_csv_path = output_root.join("phase15-roi-intensity-statistics.csv");
    let roi_metadata_path = write_table_csv_with_metadata(
        &roi_csv_path,
        &roi_table,
        AnalysisTableExportPolicy::Replace,
    )?;
    let plot_svg = export_plot_svg(&full_plot)?;
    let plot_svg_path = output_root.join("phase15-full-intensity-mean-trace.svg");
    fs::write(&plot_svg_path, plot_svg)
        .with_context(|| format!("failed to write {}", plot_svg_path.display()))?;
    let full_metadata = table_export_metadata(&full_table)?;
    let roi_metadata = table_export_metadata(&roi_table)?;

    Ok(json!({
        "benchmark": "bench-phase15-analysis",
        "benchmark_schema_version": 1,
        "phase": "Phase 15: Analysis Workbench",
        "hardware": benchmark_host_context(),
        "generated_fixture": {
            "name": "time-multichannel-u16-8cube-3t-2c",
            "package": fixture,
            "dataset_id": manifest.dataset.id,
            "dataset_name": manifest.dataset.name,
            "layer_id": layer_id.as_str(),
        },
        "operation_model": {
            "schema_version": full_operation.schema_version,
            "execution_states": ["queued", "running", "cancelling", "cancelled", "failed", "complete"],
            "result_states": ["preview", "approximate", "partial", "cancelled", "failed", "complete"],
            "final_results_use_display_values": false,
        },
        "full_time_series": {
            "operation": full_operation,
            "table_rows": full_table.rows.len(),
            "plot_series": full_plot.series.len(),
            "export_csv": full_csv_path,
            "export_metadata": full_metadata_path,
            "export_metadata_row_count": full_metadata.row_count,
            "export_svg": plot_svg_path,
            "data_engine_delta": phase15_stats_delta_json(before_full, after_full),
        },
        "roi_measurement": {
            "operation": roi_operation,
            "table_rows": roi_table.rows.len(),
            "export_csv": roi_csv_path,
            "export_metadata": roi_metadata_path,
            "export_metadata_row_count": roi_metadata.row_count,
            "standard_deviation_present": roi_table.columns.iter().any(|column| column.key == "standard_deviation"),
            "data_engine_delta": phase15_stats_delta_json(before_roi, after_roi),
        },
        "source_value_policy": {
            "source_scale": 0,
            "data_source": "data_engine_brick_and_region_reads",
            "display_transfer_used_for_final_values": false,
        },
        "findings": [
            {
                "classification": "no action",
                "surface": "typed operation records",
                "observation": "full time-series and ROI workflows produce complete typed operation records with validated input scope and provenance"
            },
            {
                "classification": "no action",
                "surface": "exports",
                "observation": "CSV exports write adjacent JSON metadata and plot export writes SVG with embedded provenance metadata"
            },
            {
                "classification": "no action",
                "surface": "out-of-core source access",
                "observation": "full time-series uses source brick reads and ROI measurement uses a source region read instead of renderer residency"
            }
        ],
    }))
}

fn phase15_full_intensity_analysis(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
) -> anyhow::Result<(AnalysisTable, AnalysisPlot, AnalysisOperationRecord)> {
    let timepoint_count = dataset
        .layer(layer_id)
        .context("phase15 full analysis requires a layer")?
        .shape
        .t();
    let mut rows = Vec::with_capacity(timepoint_count as usize);
    for timepoint in 0..timepoint_count {
        let brick_grid = dataset.brick_grid_shape_at_scale(layer_id, 0)?;
        let mut accumulator = IntensitySummaryAccumulator::default();
        for z in 0..brick_grid.z() {
            for y in 0..brick_grid.y() {
                for x in 0..brick_grid.x() {
                    let brick = dataset.read_u16_brick_at_scale(
                        layer_id,
                        0,
                        TimeIndex::new(timepoint),
                        SpatialBrickIndex::new(z, y, x),
                    )?;
                    accumulator.include_volume(&brick.volume);
                }
            }
        }
        rows.push(intensity_summary_row(timepoint, accumulator.finish()));
    }
    let provenance = phase15_provenance(
        dataset,
        layer_id,
        "full_intensity_summary",
        FULL_INTENSITY_SUMMARY_OPERATION_VERSION,
        0,
        timepoint_count,
        "all source timepoints",
        AnalysisExecutionClass::FullScopeBatch,
        BTreeMap::from([
            (
                "compute_path".to_owned(),
                "cpu_exact_u16_brick_streaming_reference".to_owned(),
            ),
            ("timepoint_count".to_owned(), timepoint_count.to_string()),
        ]),
        "CPU f64 accumulation over source uint16 values",
    )?;
    let table = final_intensity_summary_table(
        "phase15-full-intensity-summary",
        "Phase 15 Full Intensity Summary",
        provenance.clone(),
        rows,
    )?;
    let plot = time_trace_plot_from_table(
        "phase15-full-intensity-mean-trace",
        "Phase 15 Mean Intensity Trace",
        &table,
        "timepoint",
        "mean",
    )?;
    let operation = phase15_operation_record(
        dataset,
        layer_id,
        "phase15-full-intensity-summary",
        FULL_INTENSITY_SUMMARY_OPERATION_VERSION,
        AnalysisOperationKind::FullIntensitySummary,
        AnalysisSpatialScope::WholeVolume,
        provenance,
    )?;
    Ok((table, plot, operation))
}

fn phase15_roi_analysis(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
) -> anyhow::Result<(AnalysisTable, AnalysisOperationRecord)> {
    let roi = RoiArtifact::new(
        SceneArtifactId::new("roi", "phase15-roi")?,
        "Phase 15 ROI",
        WorldGeometry::Box3D {
            min: DVec3::new(0.0, 0.0, 0.0),
            max: DVec3::new(0.4, 0.4, 0.4),
        },
        SceneArtifactTime::Timepoint(TimeIndex::new(0)),
    )?;
    let shape = dataset.scale_shape(layer_id, 0)?;
    let grid_to_world = dataset.scale_grid_to_world(layer_id, 0)?;
    let region = box_roi_grid_region(grid_to_world, shape, &roi)?
        .context("phase15 ROI should intersect fixture source volume")?;
    let region_volume = dataset.read_u16_region(layer_id, TimeIndex::new(0), region)?;
    let statistics = summarize_u16_volume_as_roi(&region_volume, roi.id.as_str());
    let provenance = phase15_provenance(
        dataset,
        layer_id,
        "roi_intensity_statistics",
        ROI_INTENSITY_OPERATION_VERSION,
        0,
        1,
        "phase15 ROI at source timepoint 0",
        AnalysisExecutionClass::RoiLocalExact,
        BTreeMap::from([
            (
                "compute_path".to_owned(),
                "cpu_exact_u16_region_streaming_reference".to_owned(),
            ),
            ("roi_id".to_owned(), roi.id.as_str().to_owned()),
        ]),
        "CPU f64 accumulation over source uint16 ROI region values",
    )?;
    let table = final_roi_intensity_table(
        "phase15-roi-intensity-statistics",
        "Phase 15 ROI Intensity Statistics",
        provenance.clone(),
        vec![roi_intensity_row(0, statistics)],
    )?;
    let operation = phase15_operation_record(
        dataset,
        layer_id,
        "phase15-roi-intensity-statistics",
        ROI_INTENSITY_OPERATION_VERSION,
        AnalysisOperationKind::RoiIntensityStatistics,
        AnalysisSpatialScope::Roi {
            roi_id: roi.id.as_str().to_owned(),
        },
        provenance,
    )?;
    Ok((table, operation))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn phase15_provenance(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    operation: &str,
    operation_version: u32,
    timepoint_start: u64,
    timepoint_end_exclusive: u64,
    scope: &str,
    execution_class: AnalysisExecutionClass,
    parameters: BTreeMap<String, String>,
    compute_precision: &str,
) -> anyhow::Result<AnalysisProvenance> {
    let manifest = dataset.manifest();
    Ok(AnalysisProvenance {
        source_dataset_id: manifest.dataset.id.clone(),
        source_dataset: manifest.dataset.name.clone(),
        native_format: manifest.format.clone(),
        native_schema_version: manifest.schema_version,
        app_version: "xtask".to_owned(),
        created_at_utc: "xtask-phase15-deterministic".to_owned(),
        source_layer_id: layer_id.as_str().to_owned(),
        timepoint_start,
        timepoint_end_exclusive,
        scale_level: 0,
        operation: operation.to_owned(),
        operation_version,
        parameters,
        scope: scope.to_owned(),
        execution_class,
        result_state: AnalysisResultState::Complete,
        data_source: "data_engine_brick_and_region_reads".to_owned(),
        compute_precision: compute_precision.to_owned(),
    })
}

pub(crate) fn phase15_operation_record(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    operation_id: &str,
    operation_version: u32,
    kind: AnalysisOperationKind,
    spatial_scope: AnalysisSpatialScope,
    provenance: AnalysisProvenance,
) -> anyhow::Result<AnalysisOperationRecord> {
    let manifest = dataset.manifest();
    let parameters = provenance
        .parameters
        .iter()
        .map(|(key, value)| (key.clone(), AnalysisParameterValue::Text(value.clone())))
        .collect::<BTreeMap<_, _>>();
    let input = AnalysisOperationInput {
        dataset_id: manifest.dataset.id.clone(),
        dataset_name: manifest.dataset.name.clone(),
        native_format: manifest.format.clone(),
        native_schema_version: manifest.schema_version,
        layer_id: layer_id.as_str().to_owned(),
        time_scope: AnalysisTimeScope::new(
            provenance.timepoint_start,
            provenance.timepoint_end_exclusive,
        )?,
        scale_level: provenance.scale_level,
        spatial_scope,
    };
    let record = AnalysisOperationRecord::new(
        operation_id,
        operation_version,
        kind,
        input,
        parameters,
        AnalysisResultState::Complete,
    )?
    .with_execution_state(mirante4d_analysis::AnalysisExecutionState::Complete)
    .with_provenance(provenance);
    record.validate()?;
    Ok(record)
}

fn phase15_stats_delta_json(
    before: mirante4d_data::DataEngineStats,
    after: mirante4d_data::DataEngineStats,
) -> Value {
    json!({
        "subset_reads": after.subset_reads.saturating_sub(before.subset_reads),
        "brick_reads": after.brick_reads.saturating_sub(before.brick_reads),
        "decoded_values": after.decoded_values.saturating_sub(before.decoded_values),
        "decoded_bytes": after.decoded_bytes.saturating_sub(before.decoded_bytes),
        "decoded_brick_values": after.decoded_brick_values.saturating_sub(before.decoded_brick_values),
        "decoded_brick_bytes": after.decoded_brick_bytes.saturating_sub(before.decoded_brick_bytes),
        "encoded_payload_bytes_read": after.encoded_payload_bytes_read.saturating_sub(before.encoded_payload_bytes_read),
        "encoded_shard_payloads_read": after.encoded_shard_payloads_read.saturating_sub(before.encoded_shard_payloads_read),
        "shard_index_cache_hits": after.shard_index_cache_hits.saturating_sub(before.shard_index_cache_hits),
        "shard_index_cache_misses": after.shard_index_cache_misses.saturating_sub(before.shard_index_cache_misses),
        "shard_index_cache_entries_after": after.shard_index_cache_entries,
        "brick_cache_u8_bytes_after": after.brick_cache_u8_bytes,
        "brick_cache_u16_bytes_after": after.brick_cache_u16_bytes,
        "brick_cache_f32_bytes_after": after.brick_cache_f32_bytes,
    })
}

fn phase15_audit_markdown(report: &Value) -> String {
    let fixture = report
        .pointer("/generated_fixture/name")
        .and_then(Value::as_str)
        .unwrap_or("unknown fixture");
    let full_rows = report
        .pointer("/full_time_series/table_rows")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let roi_rows = report
        .pointer("/roi_measurement/table_rows")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let full_subset_reads = report
        .pointer("/full_time_series/data_engine_delta/subset_reads")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let roi_subset_reads = report
        .pointer("/roi_measurement/data_engine_delta/subset_reads")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut out = String::new();
    out.push_str("# Phase 15 Analysis Audit\n\n");
    out.push_str(&format!("- generated fixture: `{fixture}`\n"));
    out.push_str(&format!("- full time-series table rows: `{full_rows}`\n"));
    out.push_str(&format!("- ROI measurement table rows: `{roi_rows}`\n"));
    out.push_str(&format!(
        "- data-engine source reads: full workflow `{full_subset_reads}`, ROI workflow `{roi_subset_reads}`\n"
    ));
    out.push_str("- exports: CSV plus adjacent JSON metadata for tables; SVG for plot output\n");
    out.push_str("- result states: preview/approximate/partial/cancelled/failed/complete are represented distinctly; exported audit results are `complete`\n\n");
    out.push_str("## Findings\n\n");
    if let Some(findings) = report.get("findings").and_then(Value::as_array) {
        for finding in findings {
            let classification = finding
                .get("classification")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let surface = finding
                .get("surface")
                .and_then(Value::as_str)
                .unwrap_or("unknown surface");
            let observation = finding
                .get("observation")
                .and_then(Value::as_str)
                .unwrap_or("");
            out.push_str(&format!(
                "- `{classification}` `{surface}`: {observation}\n"
            ));
        }
    }
    out
}

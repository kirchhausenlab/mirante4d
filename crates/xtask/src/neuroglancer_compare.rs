use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::reports::{read_json_file, write_json_file};

const INPUT_SCHEMA: &str = "mirante4d-neuroglancer-comparison-input";
const INPUT_SCHEMA_VERSION: u64 = 1;
const NEUROGLANCER_MEASUREMENT_SCHEMA: &str = "neuroglancer-cross-section-performance-measurement";
const NEUROGLANCER_MEASUREMENT_SCHEMA_VERSION: u64 = 1;
const OUTPUT_SCHEMA: &str = "mirante4d-neuroglancer-cross-section-comparison";
const OUTPUT_SCHEMA_VERSION: u64 = 1;
const DEFAULT_OUTPUT: &str =
    "target/mirante4d/neuroglancer-comparison/neuroglancer-comparison-report.json";
const P95_RATIO_TARGET: f64 = 2.0;
const REQUIRED_OPERATIONS: &[&str] = &[
    "pan",
    "zoom",
    "slice_shift",
    "oblique_rotation",
    "timepoint_change",
];

#[derive(Debug, Clone, PartialEq)]
struct OperationMeasurement {
    operation: String,
    p50_ms: Option<f64>,
    p95_ms: f64,
    p99_ms: Option<f64>,
    max_ms: Option<f64>,
    sample_count: u64,
    status: Option<String>,
    source: String,
}

pub(crate) fn neuroglancer_compare(manifest_path: &Path) -> anyhow::Result<PathBuf> {
    let manifest = read_json_file(manifest_path)?;
    validate_schema(&manifest, INPUT_SCHEMA, INPUT_SCHEMA_VERSION, manifest_path)?;
    let manifest_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let output = manifest
        .get("output_report")
        .and_then(Value::as_str)
        .map(|path| resolve_manifest_path(manifest_dir, path))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_OUTPUT));
    let mirante_report_paths = manifest
        .get("mirante_reports")
        .and_then(Value::as_array)
        .context("comparison manifest must contain mirante_reports array")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(|path| resolve_manifest_path(manifest_dir, path))
                .context("mirante_reports entries must be strings")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    if mirante_report_paths.is_empty() {
        bail!("comparison manifest must name at least one Mirante product-validation report");
    }
    let neuroglancer_measurement_path = manifest
        .get("neuroglancer_measurement")
        .and_then(Value::as_str)
        .map(|path| resolve_manifest_path(manifest_dir, path))
        .context("comparison manifest must contain neuroglancer_measurement")?;

    let mut failures = Vec::new();
    let mirante_reports = mirante_report_paths
        .iter()
        .map(|path| read_json_file(path).map(|value| (path.clone(), value)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let neuroglancer_measurement = read_json_file(&neuroglancer_measurement_path)?;
    validate_schema(
        &neuroglancer_measurement,
        NEUROGLANCER_MEASUREMENT_SCHEMA,
        NEUROGLANCER_MEASUREMENT_SCHEMA_VERSION,
        &neuroglancer_measurement_path,
    )?;

    let mut mirante_operations = BTreeMap::new();
    let mut product_contexts = Vec::new();
    for (path, report) in &mirante_reports {
        if report.get("status").and_then(Value::as_str) != Some("passed") {
            failures.push(format!(
                "Mirante report {} did not pass product validation",
                path.display()
            ));
        }
        product_contexts.push(mirante_product_context(path, report));
        for measurement in mirante_operation_measurements(path, report)? {
            let key = measurement.operation.clone();
            if mirante_operations
                .insert(key.clone(), measurement)
                .is_some()
            {
                failures.push(format!(
                    "operation {key} has samples in more than one Mirante report"
                ));
            }
        }
    }
    let mirante_performance_contract = mirante_performance_contract_coverage(&mirante_reports);

    let neuroglancer_operations = neuroglancer_operation_measurements(
        &neuroglancer_measurement_path,
        &neuroglancer_measurement,
    )?;
    let required_operations = required_operations(&manifest);
    let mut rows = Vec::new();
    for operation in &required_operations {
        let mirante = mirante_operations.get(operation.as_str());
        let neuroglancer = neuroglancer_operations.get(operation.as_str());
        let row = comparison_row(operation, mirante, neuroglancer);
        if row.get("status").and_then(Value::as_str) != Some("passed") {
            failures.push(
                row.get("failure_reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown comparison failure")
                    .to_owned(),
            );
        }
        rows.push(row);
    }

    failures.extend(required_memory_failures(&neuroglancer_measurement));
    failures.extend(required_neuroglancer_performance_failures(
        &neuroglancer_measurement,
    ));
    failures.extend(required_mirante_performance_failures(
        &mirante_performance_contract,
    ));
    let status = if failures.is_empty() {
        "passed"
    } else {
        "failed"
    };
    let report = json!({
        "schema": OUTPUT_SCHEMA,
        "schema_version": OUTPUT_SCHEMA_VERSION,
        "command": "neuroglancer-compare",
        "status": status,
        "requirement_source": "docs/plans/active/foundation-refactor/VERIFICATION_EVIDENCE_BRIEF.md",
        "comparison_policy": {
            "required_operations": required_operations,
            "p95_ratio_target": P95_RATIO_TARGET,
            "ratio_definition": "mirante_p95_ms / neuroglancer_p95_ms",
            "memory_fields_required_for_neuroglancer_measurement": required_neuroglancer_memory_fields(),
            "performance_fields_required_for_neuroglancer_measurement": required_neuroglancer_performance_fields(),
            "performance_fields_required_for_mirante_reports": required_mirante_performance_fields(),
        },
        "inputs": {
            "manifest": manifest_path,
            "mirante_reports": mirante_report_paths,
            "neuroglancer_measurement": neuroglancer_measurement_path,
        },
        "mirante_product_contexts": product_contexts,
        "neuroglancer_context": neuroglancer_context(&neuroglancer_measurement),
        "rows": rows,
        "memory": {
            "mirante": mirante_memory_summary(&mirante_reports),
            "neuroglancer": neuroglancer_measurement.get("memory").cloned().unwrap_or(Value::Null),
        },
        "performance_contract": {
            "mirante": mirante_performance_contract,
            "neuroglancer": neuroglancer_measurement
                .get("performance")
                .cloned()
                .unwrap_or(Value::Null),
        },
        "failures": failures,
    });
    write_json_file(&output, &report)?;
    if status != "passed" {
        bail!(
            "Neuroglancer comparison failed; report written to {}",
            output.display()
        );
    }
    Ok(output)
}

fn validate_schema(
    value: &Value,
    schema: &str,
    schema_version: u64,
    path: &Path,
) -> anyhow::Result<()> {
    let actual_schema = value.get("schema").and_then(Value::as_str);
    let actual_version = value.get("schema_version").and_then(Value::as_u64);
    if actual_schema != Some(schema) || actual_version != Some(schema_version) {
        bail!(
            "{} must use schema {} v{}",
            path.display(),
            schema,
            schema_version
        );
    }
    Ok(())
}

fn resolve_manifest_path(manifest_dir: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        manifest_dir.join(path)
    }
}

fn required_operations(manifest: &Value) -> Vec<String> {
    manifest
        .get("required_operations")
        .and_then(Value::as_array)
        .map(|operations| {
            operations
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|operations| !operations.is_empty())
        .unwrap_or_else(|| {
            REQUIRED_OPERATIONS
                .iter()
                .map(|operation| (*operation).to_owned())
                .collect()
        })
}

fn mirante_operation_measurements(
    path: &Path,
    report: &Value,
) -> anyhow::Result<Vec<OperationMeasurement>> {
    let rows = report
        .get("metrics")
        .and_then(|metrics| metrics.get("cross_section_performance_gate_table"))
        .and_then(|table| table.get("rows"))
        .and_then(Value::as_array)
        .context("Mirante report has no metrics.cross_section_performance_gate_table.rows")?;
    let mut measurements = Vec::new();
    for row in rows {
        let Some(sample_count) = row.get("sample_count").and_then(Value::as_u64) else {
            continue;
        };
        if sample_count == 0 {
            continue;
        }
        let operation = row
            .get("operation")
            .and_then(Value::as_str)
            .context("Mirante performance row is missing operation")?
            .to_owned();
        let p95_ms = row
            .get("p95_ms")
            .and_then(Value::as_f64)
            .filter(|value| *value > 0.0)
            .with_context(|| format!("Mirante operation {operation} is missing positive p95_ms"))?;
        measurements.push(OperationMeasurement {
            operation,
            p50_ms: row.get("p50_ms").and_then(Value::as_f64),
            p95_ms,
            p99_ms: row.get("p99_ms").and_then(Value::as_f64),
            max_ms: row.get("max_ms").and_then(Value::as_f64),
            sample_count,
            status: row.get("status").and_then(Value::as_str).map(str::to_owned),
            source: path.display().to_string(),
        });
    }
    Ok(measurements)
}

fn neuroglancer_operation_measurements(
    path: &Path,
    measurement: &Value,
) -> anyhow::Result<BTreeMap<String, OperationMeasurement>> {
    let operations = measurement
        .get("operations")
        .and_then(Value::as_object)
        .context("Neuroglancer measurement must contain operations object")?;
    let mut output = BTreeMap::new();
    for (operation, value) in operations {
        let sample_count = value
            .get("sample_count")
            .and_then(Value::as_u64)
            .filter(|count| *count > 0)
            .with_context(|| {
                format!("Neuroglancer operation {operation} is missing positive sample_count")
            })?;
        let p95_ms = value
            .get("p95_ms")
            .and_then(Value::as_f64)
            .filter(|value| *value > 0.0)
            .with_context(|| {
                format!("Neuroglancer operation {operation} is missing positive p95_ms")
            })?;
        output.insert(
            operation.clone(),
            OperationMeasurement {
                operation: operation.clone(),
                p50_ms: value.get("p50_ms").and_then(Value::as_f64),
                p95_ms,
                p99_ms: value.get("p99_ms").and_then(Value::as_f64),
                max_ms: value.get("max_ms").and_then(Value::as_f64),
                sample_count,
                status: value
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                source: path.display().to_string(),
            },
        );
    }
    Ok(output)
}

fn comparison_row(
    operation: &str,
    mirante: Option<&OperationMeasurement>,
    neuroglancer: Option<&OperationMeasurement>,
) -> Value {
    let Some(mirante) = mirante else {
        return json!({
            "operation": operation,
            "status": "failed",
            "failure_reason": format!("missing Mirante samples for operation {operation}"),
        });
    };
    let Some(neuroglancer) = neuroglancer else {
        return json!({
            "operation": operation,
            "status": "failed",
            "failure_reason": format!("missing Neuroglancer samples for operation {operation}"),
            "mirante": operation_measurement_json(mirante),
        });
    };
    let ratio = mirante.p95_ms / neuroglancer.p95_ms;
    let mirante_gate_passed = mirante
        .status
        .as_deref()
        .is_none_or(|status| status == "passed");
    let passed = ratio <= P95_RATIO_TARGET && mirante_gate_passed;
    json!({
        "operation": operation,
        "status": if passed { "passed" } else { "failed" },
        "failure_reason": if passed {
            Value::Null
        } else if !mirante_gate_passed {
            json!(format!("Mirante operation {operation} did not pass its product latency gate"))
        } else {
            json!(format!(
                "Mirante operation {operation} p95 ratio {:.3} exceeded target {:.3}",
                ratio, P95_RATIO_TARGET
            ))
        },
        "mirante": operation_measurement_json(mirante),
        "neuroglancer": operation_measurement_json(neuroglancer),
        "p95_ratio": ratio,
        "p95_ratio_target": P95_RATIO_TARGET,
    })
}

fn operation_measurement_json(measurement: &OperationMeasurement) -> Value {
    json!({
        "source": measurement.source,
        "operation": measurement.operation,
        "sample_count": measurement.sample_count,
        "p50_ms": measurement.p50_ms,
        "p95_ms": measurement.p95_ms,
        "p99_ms": measurement.p99_ms,
        "max_ms": measurement.max_ms,
        "status": measurement.status,
    })
}

fn required_memory_failures(measurement: &Value) -> Vec<String> {
    let mut failures = Vec::new();
    let memory = measurement.get("memory");
    for field in required_neuroglancer_memory_fields() {
        if memory.and_then(|memory| memory.get(field)).is_none() {
            failures.push(format!(
                "Neuroglancer measurement is missing memory.{field}"
            ));
        }
    }
    failures
}

fn required_neuroglancer_performance_failures(measurement: &Value) -> Vec<String> {
    let performance = measurement.get("performance");
    required_neuroglancer_performance_fields()
        .into_iter()
        .filter(|field| {
            performance
                .and_then(|performance| performance.get(field))
                .is_none()
        })
        .map(|field| format!("Neuroglancer measurement is missing performance.{field}"))
        .collect()
}

fn required_neuroglancer_memory_fields() -> Vec<&'static str> {
    vec![
        "peak_rss_bytes",
        "gpu_resident_bytes",
        "chunk_cache_bytes",
        "measurement_notes",
    ]
}

fn required_neuroglancer_performance_fields() -> Vec<&'static str> {
    vec![
        "first_current_partial_p95_ms",
        "visible_chunk_planning_p95_ms",
        "candidate_chunks_max",
        "emitted_visible_chunks_max",
        "queue_depth_max",
        "queued_chunks_max",
        "decoding_chunks_max",
        "cpu_resident_chunks_max",
        "upload_queued_chunks_max",
        "gpu_resident_chunks_max",
        "missing_chunks_max",
        "upload_bytes_max",
        "upload_p95_ms",
        "render_p95_ms",
        "ui_frame_p95_ms",
        "cpu_rss_bytes_max",
        "gpu_resident_chunk_bytes_max",
        "panel_target_bytes_max",
        "eviction_count_max",
    ]
}

fn required_mirante_performance_fields() -> Vec<&'static str> {
    vec![
        "first_current_partial_p95_ms_max",
        "semantic_resource_planning_p95_ms_max",
        "gpu_upload_p95_ms_max",
        "render_p95_ms_max",
        "ui_frame_p95_ms_max",
        "cpu_total_used_bytes_max",
        "cpu_decoded_residency_bytes_max",
        "queued_requests_max",
        "in_flight_decodes_max",
        "pending_completions_max",
        "resident_resources_max",
        "lease_required_max",
        "lease_retained_max",
        "lease_missing_max",
        "cpu_rss_bytes_max",
        "panel_target_bytes_max",
    ]
}

fn required_mirante_performance_failures(coverage: &Value) -> Vec<String> {
    let mut failures = required_mirante_performance_field_pointers()
        .into_iter()
        .filter(|(_, pointer)| {
            coverage
                .pointer(pointer)
                .is_none_or(|value| value.is_null())
        })
        .map(|(field, _)| format!("Mirante reports are missing performance contract field {field}"))
        .collect::<Vec<_>>();

    if coverage
        .pointer("/product_path/all_unified_dataset_leases_to_gpu_renderer")
        .and_then(Value::as_bool)
        != Some(true)
    {
        failures.push(
            "Mirante reports do not all use product_display_path unified_dataset_leases_to_gpu_renderer"
                .to_owned(),
        );
    }

    failures
}

fn required_mirante_performance_field_pointers() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "first_current_partial_p95_ms_max",
            "/latency/first_current_partial_p95_ms_max",
        ),
        (
            "semantic_resource_planning_p95_ms_max",
            "/timing_ms/semantic_resource_planning_p95_ms_max",
        ),
        ("gpu_upload_p95_ms_max", "/timing_ms/gpu_upload_p95_ms_max"),
        ("render_p95_ms_max", "/timing_ms/render_p95_ms_max"),
        ("ui_frame_p95_ms_max", "/timing_ms/ui_frame_p95_ms_max"),
        (
            "cpu_total_used_bytes_max",
            "/dataset_runtime/cpu_total_used_bytes_max",
        ),
        (
            "cpu_decoded_residency_bytes_max",
            "/dataset_runtime/cpu_decoded_residency_bytes_max",
        ),
        (
            "queued_requests_max",
            "/dataset_runtime/queued_requests_max",
        ),
        (
            "in_flight_decodes_max",
            "/dataset_runtime/in_flight_decodes_max",
        ),
        (
            "pending_completions_max",
            "/dataset_runtime/pending_completions_max",
        ),
        (
            "resident_resources_max",
            "/dataset_runtime/resident_resources_max",
        ),
        ("lease_required_max", "/lease_bridge/required_max"),
        ("lease_retained_max", "/lease_bridge/retained_max"),
        ("lease_missing_max", "/lease_bridge/missing_max"),
        ("cpu_rss_bytes_max", "/memory/cpu_rss_bytes_max"),
        ("panel_target_bytes_max", "/memory/panel_target_bytes_max"),
    ]
}

fn mirante_product_context(path: &Path, report: &Value) -> Value {
    json!({
        "path": path,
        "status": report.get("status").cloned().unwrap_or(Value::Null),
        "display": report
            .get("environment")
            .and_then(|environment| environment.get("display"))
            .cloned()
            .unwrap_or(Value::Null),
        "dataset": report.get("dataset").cloned().unwrap_or(Value::Null),
        "scenario": report.get("scenario").cloned().unwrap_or(Value::Null),
        "product_display_path": report
            .get("metrics")
            .and_then(|metrics| metrics.get("cross_section_panels"))
            .and_then(|panels| panels.get("final"))
            .and_then(|diagnostics| diagnostics.get("product_display_path"))
            .cloned()
            .unwrap_or(Value::Null),
    })
}

fn mirante_performance_contract_coverage(reports: &[(PathBuf, Value)]) -> Value {
    let mut first_current_partial_p95_ms_max = None;
    let mut display_refresh_sample_count = 0_u64;
    let mut app_update_sample_count = 0_u64;
    let mut semantic_resource_planning_p95_ms_max = None;
    let mut gpu_upload_p95_ms_max = None;
    let mut render_p95_ms_max = None;
    let mut total_refresh_p95_ms_max = None;
    let mut ui_frame_p95_ms_max = None;
    let mut cpu_total_used_bytes_max = None;
    let mut cpu_decoded_residency_bytes_max = None;
    let mut queued_requests_max = None;
    let mut in_flight_decodes_max = None;
    let mut pending_completions_max = None;
    let mut resident_resources_max = None;
    let mut lease_required_max = None;
    let mut lease_retained_max = None;
    let mut lease_missing_max = None;
    let mut cpu_rss_bytes_max = None;
    let mut panel_target_bytes_max = None;
    let mut all_unified_dataset_leases_to_gpu_renderer = true;

    for (_, report) in reports {
        for measurement in
            mirante_operation_measurements(Path::new("<coverage>"), report).unwrap_or_default()
        {
            max_f64(
                &mut first_current_partial_p95_ms_max,
                Some(measurement.p95_ms),
            );
        }

        let display_summary = report
            .get("metrics")
            .and_then(|metrics| metrics.get("display_refresh_timing_summary"));
        display_refresh_sample_count = display_refresh_sample_count.saturating_add(
            display_summary
                .and_then(|summary| summary.get("sample_count"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
        );
        max_f64(
            &mut semantic_resource_planning_p95_ms_max,
            display_phase_p95(report, "visible_brick_request"),
        );
        max_f64(
            &mut gpu_upload_p95_ms_max,
            display_phase_p95(report, "gpu_upload"),
        );
        max_f64(&mut render_p95_ms_max, display_phase_p95(report, "render"));
        max_f64(
            &mut total_refresh_p95_ms_max,
            display_phase_p95(report, "total_refresh"),
        );

        let app_update_summary = report
            .get("metrics")
            .and_then(|metrics| metrics.get("app_update_timing_summary"));
        app_update_sample_count = app_update_sample_count.saturating_add(
            app_update_summary
                .and_then(|summary| summary.get("sample_count"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
        );
        max_f64(
            &mut ui_frame_p95_ms_max,
            app_update_phase_p95(report, "total_update"),
        );

        if let Some(value) = report
            .get("process")
            .and_then(|process| process.get("peak_rss_bytes"))
            .and_then(Value::as_u64)
        {
            max_u64(&mut cpu_rss_bytes_max, Some(value));
        }

        let metrics = report.get("metrics");
        if let Some(runtime) = metrics
            .and_then(|metrics| metrics.get("dataset_runtime"))
            .and_then(|runtime| runtime.get("final"))
        {
            max_u64(
                &mut cpu_total_used_bytes_max,
                runtime
                    .pointer("/used/total_cpu_bytes")
                    .and_then(Value::as_u64),
            );
            max_u64(
                &mut cpu_decoded_residency_bytes_max,
                runtime
                    .pointer("/used/category_bytes/decoded_residency")
                    .and_then(Value::as_u64),
            );
            let work = runtime.get("work");
            max_u64(
                &mut queued_requests_max,
                work.and_then(|work| work.get("queued_requests"))
                    .and_then(Value::as_u64),
            );
            max_u64(
                &mut in_flight_decodes_max,
                work.and_then(|work| work.get("in_flight_decodes"))
                    .and_then(Value::as_u64),
            );
            max_u64(
                &mut pending_completions_max,
                work.and_then(|work| work.get("pending_completions"))
                    .and_then(Value::as_u64),
            );
            max_u64(
                &mut resident_resources_max,
                work.and_then(|work| work.get("resident_resources"))
                    .and_then(Value::as_u64),
            );
        }
        if let Some(lease) = metrics
            .and_then(|metrics| metrics.get("lease_bridge"))
            .and_then(|lease| lease.get("final"))
        {
            max_u64(
                &mut lease_required_max,
                lease.get("required").and_then(Value::as_u64),
            );
            max_u64(
                &mut lease_retained_max,
                lease.get("retained").and_then(Value::as_u64),
            );
            max_u64(
                &mut lease_missing_max,
                lease.get("missing").and_then(Value::as_u64),
            );
        }
        let Some(panel_diagnostics) = metrics
            .and_then(|metrics| metrics.get("cross_section_panels"))
            .and_then(|panels| panels.get("final"))
        else {
            all_unified_dataset_leases_to_gpu_renderer = false;
            continue;
        };
        all_unified_dataset_leases_to_gpu_renderer &= panel_diagnostics
            .get("product_display_path")
            .and_then(Value::as_str)
            == Some("unified_dataset_leases_to_gpu_renderer");
        for panel in panel_diagnostics
            .get("panels")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            max_u64(
                &mut panel_target_bytes_max,
                panel_display_frame_bytes(panel),
            );
        }
    }

    json!({
        "latency": {
            "first_current_partial_p95_ms_max": first_current_partial_p95_ms_max,
        },
        "timing_ms": {
            "display_refresh_sample_count": display_refresh_sample_count,
            "app_update_sample_count": app_update_sample_count,
            "semantic_resource_planning_p95_ms_max": semantic_resource_planning_p95_ms_max,
            "gpu_upload_p95_ms_max": gpu_upload_p95_ms_max,
            "render_p95_ms_max": render_p95_ms_max,
            "total_refresh_p95_ms_max": total_refresh_p95_ms_max,
            "ui_frame_p95_ms_max": ui_frame_p95_ms_max,
        },
        "dataset_runtime": {
            "cpu_total_used_bytes_max": cpu_total_used_bytes_max,
            "cpu_decoded_residency_bytes_max": cpu_decoded_residency_bytes_max,
            "queued_requests_max": queued_requests_max,
            "in_flight_decodes_max": in_flight_decodes_max,
            "pending_completions_max": pending_completions_max,
            "resident_resources_max": resident_resources_max,
        },
        "lease_bridge": {
            "required_max": lease_required_max,
            "retained_max": lease_retained_max,
            "missing_max": lease_missing_max,
        },
        "memory": {
            "cpu_rss_bytes_max": cpu_rss_bytes_max,
            "panel_target_bytes_max": panel_target_bytes_max,
        },
        "product_path": {
            "all_unified_dataset_leases_to_gpu_renderer": all_unified_dataset_leases_to_gpu_renderer,
        },
    })
}

fn display_phase_p95(report: &Value, phase: &str) -> Option<f64> {
    report
        .get("metrics")
        .and_then(|metrics| metrics.get("display_refresh_timing_summary"))
        .and_then(|summary| summary.get("phases_ms"))
        .and_then(|phases| phases.get(phase))
        .and_then(|summary| summary.get("p95"))
        .and_then(Value::as_f64)
}

fn app_update_phase_p95(report: &Value, phase: &str) -> Option<f64> {
    report
        .get("metrics")
        .and_then(|metrics| metrics.get("app_update_timing_summary"))
        .and_then(|summary| summary.get("phases_ms"))
        .and_then(|phases| phases.get(phase))
        .and_then(|summary| summary.get("p95"))
        .and_then(Value::as_f64)
}

fn panel_display_frame_bytes(panel: &Value) -> Option<u64> {
    let display_frame = panel.get("display_frame")?;
    let output = display_frame
        .get("output_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let accumulator = display_frame
        .get("accumulator_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let texture = display_frame
        .get("texture_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some(output.saturating_add(accumulator).saturating_add(texture))
}

fn max_f64(slot: &mut Option<f64>, value: Option<f64>) {
    let Some(value) = value.filter(|value| value.is_finite()) else {
        return;
    };
    *slot = Some(slot.map_or(value, |current| current.max(value)));
}

fn max_u64(slot: &mut Option<u64>, value: Option<u64>) {
    let Some(value) = value else {
        return;
    };
    *slot = Some(slot.map_or(value, |current| current.max(value)));
}

fn neuroglancer_context(measurement: &Value) -> Value {
    let local_checkout = measurement
        .get("source")
        .and_then(|source| source.get("local_checkout"))
        .and_then(Value::as_str);
    json!({
        "dataset": measurement.get("dataset").cloned().unwrap_or(Value::Null),
        "source": measurement.get("source").cloned().unwrap_or(Value::Null),
        "local_checkout_exists": local_checkout
            .map(|path| Path::new(path).exists())
            .unwrap_or(false),
    })
}

fn mirante_memory_summary(reports: &[(PathBuf, Value)]) -> Value {
    let mut peak_rss_bytes = Vec::new();
    let mut dataset_cpu_used_bytes = Vec::new();
    let mut retained_resources = Vec::new();
    for (_, report) in reports {
        if let Some(value) = report
            .get("process")
            .and_then(|process| process.get("peak_rss_bytes"))
            .and_then(Value::as_u64)
        {
            peak_rss_bytes.push(value);
        }
        if let Some(value) = report
            .get("metrics")
            .and_then(|metrics| metrics.get("dataset_runtime"))
            .and_then(|runtime| runtime.get("final"))
            .and_then(|runtime| runtime.pointer("/used/total_cpu_bytes"))
            .and_then(Value::as_u64)
        {
            dataset_cpu_used_bytes.push(value);
        }
        if let Some(value) = report
            .get("metrics")
            .and_then(|metrics| metrics.get("lease_bridge"))
            .and_then(|lease| lease.get("final"))
            .and_then(|lease| lease.get("retained"))
            .and_then(Value::as_u64)
        {
            retained_resources.push(value);
        }
    }
    json!({
        "peak_rss_bytes_max": peak_rss_bytes.into_iter().max(),
        "dataset_cpu_used_bytes_max": dataset_cpu_used_bytes.into_iter().max(),
        "retained_resources_max": retained_resources.into_iter().max(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn product_report(rows: Value) -> Value {
        json!({
            "schema": "mirante4d-product-validation-report",
            "schema_version": 1,
            "status": "passed",
            "environment": {"display": "real_display"},
            "dataset": {"id": "T5-QUAL-001", "package_path": "/data/T5-QUAL-001.m4d"},
            "scenario": {"name": "t5_qual_001_four_panel_continuous_cross_section"},
            "process": {"peak_rss_bytes": 1234},
            "metrics": {
                "app_update_timing_summary": {
                    "sample_count": 3,
                    "phases_ms": {
                        "total_update": {"sample_count": 3, "p95": 16.0}
                    }
                },
                "display_refresh_timing_summary": {
                    "sample_count": 3,
                    "phases_ms": {
                        "visible_brick_request": {"sample_count": 3, "p95": 2.0},
                        "gpu_upload": {"sample_count": 3, "p95": 3.0},
                        "render": {"sample_count": 3, "p95": 10.0},
                        "total_refresh": {"sample_count": 3, "p95": 12.0}
                    }
                },
                "cross_section_performance_gate_table": {
                    "kind": "cross_section_performance_gate_table",
                    "rows": rows,
                },
                "dataset_runtime": {
                    "final": {
                        "used": {
                            "total_cpu_bytes": 4096,
                            "category_bytes": {
                                "decoded_residency": 2048
                            }
                        },
                        "work": {
                            "queued_requests": 1,
                            "in_flight_decodes": 2,
                            "pending_completions": 3,
                            "resident_resources": 5
                        }
                    }
                },
                "lease_bridge": {
                    "final": {
                        "required": 6,
                        "retained": 5,
                        "missing": 1
                    }
                },
                "cross_section_panels": {
                    "final": {
                        "product_display_path": "unified_dataset_leases_to_gpu_renderer",
                        "panels": [
                            {
                                "display_frame": {
                                    "output_bytes": 1024,
                                    "accumulator_bytes": 2048,
                                    "texture_bytes": 4096
                                }
                            }
                        ]
                    }
                }
            }
        })
    }

    fn row(operation: &str, p95_ms: f64) -> Value {
        json!({
            "operation": operation,
            "sample_count": 5,
            "p50_ms": p95_ms / 2.0,
            "p95_ms": p95_ms,
            "p99_ms": p95_ms,
            "max_ms": p95_ms,
            "status": "passed",
        })
    }

    fn neuroglancer_measurement(p95_ms: f64) -> Value {
        let mut operations = serde_json::Map::new();
        for operation in REQUIRED_OPERATIONS {
            operations.insert(
                (*operation).to_owned(),
                json!({
                    "sample_count": 7,
                    "p50_ms": p95_ms / 2.0,
                    "p95_ms": p95_ms,
                    "p99_ms": p95_ms,
                    "max_ms": p95_ms,
                }),
            );
        }
        json!({
            "schema": NEUROGLANCER_MEASUREMENT_SCHEMA,
            "schema_version": NEUROGLANCER_MEASUREMENT_SCHEMA_VERSION,
            "source": {
                "method": "manual_playwright_trace",
                "local_checkout": "/external/neuroglancer",
            },
            "dataset": {
                "class": "T5-QUAL-001",
                "chunk_layout": "current_3d_chunks",
            },
            "operations": Value::Object(operations),
            "memory": {
                "peak_rss_bytes": 1000,
                "gpu_resident_bytes": 2000,
                "chunk_cache_bytes": 3000,
                "measurement_notes": "test fixture",
            },
            "performance": {
                "first_current_partial_p95_ms": p95_ms,
                "visible_chunk_planning_p95_ms": 2.0,
                "candidate_chunks_max": 8,
                "emitted_visible_chunks_max": 6,
                "queue_depth_max": 3,
                "queued_chunks_max": 1,
                "decoding_chunks_max": 2,
                "cpu_resident_chunks_max": 3,
                "upload_queued_chunks_max": 4,
                "gpu_resident_chunks_max": 5,
                "missing_chunks_max": 1,
                "upload_bytes_max": 8192,
                "upload_p95_ms": 3.0,
                "render_p95_ms": 10.0,
                "ui_frame_p95_ms": 16.0,
                "cpu_rss_bytes_max": 1000,
                "gpu_resident_chunk_bytes_max": 2000,
                "panel_target_bytes_max": 7168,
                "eviction_count_max": 7,
            }
        })
    }

    #[test]
    fn comparison_report_passes_when_mirante_is_within_ratio_target() {
        let tempdir = tempfile::tempdir().unwrap();
        let mirante = tempdir.path().join("mirante.json");
        let neuroglancer = tempdir.path().join("ng.json");
        let manifest = tempdir.path().join("manifest.json");
        let output = tempdir.path().join("comparison.json");
        let rows = REQUIRED_OPERATIONS
            .iter()
            .map(|operation| row(operation, 30.0))
            .collect::<Vec<_>>();
        write_json_file(&mirante, &product_report(Value::Array(rows))).unwrap();
        write_json_file(&neuroglancer, &neuroglancer_measurement(20.0)).unwrap();
        write_json_file(
            &manifest,
            &json!({
                "schema": INPUT_SCHEMA,
                "schema_version": INPUT_SCHEMA_VERSION,
                "mirante_reports": ["mirante.json"],
                "neuroglancer_measurement": "ng.json",
                "output_report": "comparison.json",
            }),
        )
        .unwrap();

        let path = neuroglancer_compare(&manifest).unwrap();
        assert_eq!(path, output);
        let report = read_json_file(&path).unwrap();
        assert_eq!(report["status"], "passed");
        assert_eq!(report["rows"][0]["p95_ratio"], 1.5);
    }

    #[test]
    fn comparison_report_fails_when_required_neuroglancer_operation_is_missing() {
        let tempdir = tempfile::tempdir().unwrap();
        let mirante = tempdir.path().join("mirante.json");
        let neuroglancer = tempdir.path().join("ng.json");
        let manifest = tempdir.path().join("manifest.json");
        let output = tempdir.path().join("comparison.json");
        let rows = REQUIRED_OPERATIONS
            .iter()
            .map(|operation| row(operation, 30.0))
            .collect::<Vec<_>>();
        let mut measurement = neuroglancer_measurement(20.0);
        measurement
            .get_mut("operations")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("timepoint_change");
        write_json_file(&mirante, &product_report(Value::Array(rows))).unwrap();
        write_json_file(&neuroglancer, &measurement).unwrap();
        write_json_file(
            &manifest,
            &json!({
                "schema": INPUT_SCHEMA,
                "schema_version": INPUT_SCHEMA_VERSION,
                "mirante_reports": ["mirante.json"],
                "neuroglancer_measurement": "ng.json",
                "output_report": "comparison.json",
            }),
        )
        .unwrap();

        let err = neuroglancer_compare(&manifest).unwrap_err().to_string();
        assert!(err.contains("Neuroglancer comparison failed"));
        let report = read_json_file(&output).unwrap();
        assert_eq!(report["status"], "failed");
        assert!(
            report["failures"]
                .as_array()
                .unwrap()
                .iter()
                .any(|failure| failure.as_str().unwrap().contains("timepoint_change"))
        );
    }

    #[test]
    fn comparison_report_fails_when_mirante_ratio_exceeds_target() {
        let row = comparison_row(
            "pan",
            Some(&OperationMeasurement {
                operation: "pan".to_owned(),
                p50_ms: Some(100.0),
                p95_ms: 300.0,
                p99_ms: None,
                max_ms: None,
                sample_count: 5,
                status: Some("passed".to_owned()),
                source: "mirante".to_owned(),
            }),
            Some(&OperationMeasurement {
                operation: "pan".to_owned(),
                p50_ms: Some(50.0),
                p95_ms: 100.0,
                p99_ms: None,
                max_ms: None,
                sample_count: 5,
                status: None,
                source: "neuroglancer".to_owned(),
            }),
        );

        assert_eq!(row["status"], "failed");
        assert_eq!(row["p95_ratio"], 3.0);
    }

    #[test]
    fn comparison_report_requires_neuroglancer_memory_fields() {
        let mut measurement = neuroglancer_measurement(20.0);
        measurement
            .get_mut("memory")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("chunk_cache_bytes");

        let failures = required_memory_failures(&measurement);

        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("chunk_cache_bytes"));
    }

    #[test]
    fn comparison_report_requires_neuroglancer_performance_fields() {
        let mut measurement = neuroglancer_measurement(20.0);
        measurement
            .get_mut("performance")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("upload_p95_ms");

        let failures = required_neuroglancer_performance_failures(&measurement);

        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("upload_p95_ms"));
    }

    #[test]
    fn mirante_performance_coverage_reports_contract_fields() {
        let rows = REQUIRED_OPERATIONS
            .iter()
            .map(|operation| row(operation, 30.0))
            .collect::<Vec<_>>();
        let report = product_report(Value::Array(rows));
        let coverage =
            mirante_performance_contract_coverage(&[(PathBuf::from("mirante.json"), report)]);

        assert_eq!(
            required_mirante_performance_failures(&coverage),
            Vec::<String>::new()
        );
        assert_eq!(coverage["timing_ms"]["gpu_upload_p95_ms_max"], 3.0);
        assert_eq!(
            coverage["dataset_runtime"]["cpu_total_used_bytes_max"],
            4096
        );
        assert_eq!(coverage["lease_bridge"]["retained_max"], 5);
        assert_eq!(coverage["memory"]["panel_target_bytes_max"], 7168);
        assert_eq!(
            coverage["product_path"]["all_unified_dataset_leases_to_gpu_renderer"],
            true
        );
    }

    #[test]
    fn mirante_performance_coverage_fails_without_gpu_upload_timing() {
        let rows = REQUIRED_OPERATIONS
            .iter()
            .map(|operation| row(operation, 30.0))
            .collect::<Vec<_>>();
        let mut report = product_report(Value::Array(rows));
        report
            .pointer_mut("/metrics/display_refresh_timing_summary/phases_ms")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("gpu_upload");
        let coverage =
            mirante_performance_contract_coverage(&[(PathBuf::from("mirante.json"), report)]);

        let failures = required_mirante_performance_failures(&coverage);

        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("gpu_upload_p95_ms_max"))
        );
    }

    #[test]
    fn required_operations_can_be_manifest_scoped_for_partial_dry_runs() {
        let operations = required_operations(&json!({
            "required_operations": ["pan", "zoom"]
        }));

        assert_eq!(operations, vec!["pan".to_owned(), "zoom".to_owned()]);
    }
}

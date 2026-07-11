use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, bail};
use serde_json::Value;

use crate::reports::read_json_file;

mod config;
mod import_sample;
mod native;
mod phase11;
mod phase13;
mod samples;
mod smoke;

pub(crate) use config::{
    PHASE11_DEFAULT_MAX_RESPONSIVE_VISIBLE_BRICKS, PHASE11_GPU_MIP_BRICKS_PER_BATCH,
    benchmark_camera_for_shape, benchmark_camera_for_volume, env_f64, env_u64,
    phase11_benchmark_viewport_for_shape, phase11_brick_pixel_stride,
    phase11_gpu_brick_cache_budget_bytes, phase11_gpu_volume_cache_budget_bytes,
    phase11_interaction_steps_per_scenario, phase11_max_decoded_bytes, phase11_max_visible_bricks,
};
pub(crate) use import_sample::{bench_import_sample, bench_import_sample_with_limit};
pub(crate) use native::{
    NativePackageBenchmarkOverrides, bench_native_package, bench_native_package_with_overrides,
    bench_runtime_stress,
};
pub(crate) use phase11::{
    Phase11InteractionBenchmarkOptions, Phase11LodPlan, Phase11LodPlanningInput,
    Phase11ResidentBrickSet, Phase11ResidentReadInput, bench_phase11_interaction,
    bench_phase11_large_view, bench_phase11_synthetic_matrix, bench_phase11_viewport_matrix,
    phase11_interaction_report, phase11_read_resident_for_layer, phase11_select_lod_plan,
    phase11_stored_dtype_for_layer, phase11_viewport_matrix_for_shape,
    phase11_visible_bricks_at_scale,
};
pub(crate) use phase13::{
    Phase13RendererBenchmarkOptions, bench_phase13_renderer, bench_phase13_viewport_matrix,
    phase13_renderer_report,
};
pub(crate) use samples::{
    benchmark_sample_source, list_direct_child_dirs, list_direct_tiff_files, list_tiff_files,
    sample_import_file_limit, tiff_has_multiple_images,
};
pub(crate) use smoke::bench_smoke;

pub(crate) fn bench_check(current_path: &Path, baseline_path: &Path) -> anyhow::Result<()> {
    let current = read_json_file(current_path)?;
    let baseline = read_json_file(baseline_path)?;
    let warn_slowdown_pct = env_f64("MIRANTE4D_BENCH_WARN_SLOWDOWN_PCT")?.unwrap_or(10.0);
    let fail_slowdown_pct = env_f64("MIRANTE4D_BENCH_FAIL_SLOWDOWN_PCT")?.unwrap_or(20.0);
    let report =
        evaluate_benchmark_report(&current, &baseline, warn_slowdown_pct, fail_slowdown_pct)?;

    println!(
        "benchmark check {}: {} passed, {} warned, {} failed, {} missing",
        report.benchmark,
        report.pass_count(),
        report.warn_count(),
        report.fail_count(),
        report.missing_metrics.len()
    );
    for comparison in report
        .comparisons
        .iter()
        .filter(|comparison| comparison.status != BenchmarkComparisonStatus::Pass)
    {
        println!(
            "  {:?}: {} current={:.6}ms baseline={:.6}ms slowdown={:.2}%",
            comparison.status,
            comparison.metric,
            comparison.current_ms,
            comparison.baseline_ms,
            comparison.slowdown_pct
        );
    }
    for missing in &report.missing_metrics {
        println!("  MISSING: {missing}");
    }

    if !report.missing_metrics.is_empty() || report.fail_count() > 0 {
        bail!(
            "benchmark regression check failed for {} against {}",
            current_path.display(),
            baseline_path.display()
        );
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
struct BenchmarkCheckReport {
    benchmark: String,
    comparisons: Vec<BenchmarkTimingComparison>,
    missing_metrics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct BenchmarkTimingComparison {
    metric: String,
    baseline_ms: f64,
    current_ms: f64,
    slowdown_pct: f64,
    status: BenchmarkComparisonStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BenchmarkComparisonStatus {
    Pass,
    Warn,
    Fail,
}

impl BenchmarkCheckReport {
    fn pass_count(&self) -> usize {
        self.comparisons
            .iter()
            .filter(|comparison| comparison.status == BenchmarkComparisonStatus::Pass)
            .count()
    }

    fn warn_count(&self) -> usize {
        self.comparisons
            .iter()
            .filter(|comparison| comparison.status == BenchmarkComparisonStatus::Warn)
            .count()
    }

    fn fail_count(&self) -> usize {
        self.comparisons
            .iter()
            .filter(|comparison| comparison.status == BenchmarkComparisonStatus::Fail)
            .count()
    }
}

fn evaluate_benchmark_report(
    current: &Value,
    baseline: &Value,
    warn_slowdown_pct: f64,
    fail_slowdown_pct: f64,
) -> anyhow::Result<BenchmarkCheckReport> {
    if !(warn_slowdown_pct.is_finite()
        && fail_slowdown_pct.is_finite()
        && warn_slowdown_pct >= 0.0
        && fail_slowdown_pct >= warn_slowdown_pct)
    {
        bail!(
            "benchmark slowdown thresholds must be finite and ordered, got warn={warn_slowdown_pct}, fail={fail_slowdown_pct}"
        );
    }

    let current_benchmark = benchmark_name(current)?;
    let baseline_benchmark = benchmark_name(baseline)?;
    if current_benchmark != baseline_benchmark {
        bail!("benchmark mismatch: current={current_benchmark:?}, baseline={baseline_benchmark:?}");
    }
    ensure_compatible_benchmark_context(current, baseline)?;

    let current_timings = collect_timing_metrics(current)?;
    let baseline_timings = collect_timing_metrics(baseline)?;
    if baseline_timings.is_empty() {
        bail!("baseline benchmark {baseline_benchmark:?} contains no timing metrics");
    }

    let mut comparisons = Vec::new();
    let mut missing_metrics = Vec::new();
    for (metric, baseline_ms) in baseline_timings {
        let Some(current_ms) = current_timings.get(&metric).copied() else {
            missing_metrics.push(metric);
            continue;
        };
        let slowdown_pct = slowdown_pct(current_ms, baseline_ms);
        let status = if slowdown_pct > fail_slowdown_pct {
            BenchmarkComparisonStatus::Fail
        } else if slowdown_pct > warn_slowdown_pct {
            BenchmarkComparisonStatus::Warn
        } else {
            BenchmarkComparisonStatus::Pass
        };
        comparisons.push(BenchmarkTimingComparison {
            metric,
            baseline_ms,
            current_ms,
            slowdown_pct,
            status,
        });
    }

    Ok(BenchmarkCheckReport {
        benchmark: current_benchmark.to_owned(),
        comparisons,
        missing_metrics,
    })
}

fn ensure_compatible_benchmark_context(current: &Value, baseline: &Value) -> anyhow::Result<()> {
    for field in BENCHMARK_CONTEXT_COMPATIBILITY_FIELDS {
        let current_value = comparable_context_value(current, field);
        let baseline_value = comparable_context_value(baseline, field);
        match (current_value, baseline_value) {
            (Some(current_value), Some(baseline_value)) if current_value != baseline_value => {
                bail!(
                    "benchmark context mismatch at {field}: current={current_value:?}, baseline={baseline_value:?}"
                );
            }
            (Some(_), None) => {
                bail!("benchmark baseline is missing compatibility field {field:?}");
            }
            (None, Some(_)) => {
                bail!("current benchmark report is missing compatibility field {field:?}");
            }
            _ => {}
        }
    }
    Ok(())
}

const BENCHMARK_CONTEXT_COMPATIBILITY_FIELDS: &[&str] = &[
    "benchmark_schema_version",
    "schema_version",
    "scenario",
    "scenario.name",
    "scenario.label",
    "hardware.name",
    "host.name",
    "hardware_class",
    "baseline_class",
    "dataset.id",
    "dataset.name",
    "dataset_class",
];

fn comparable_context_value(value: &Value, path: &str) -> Option<String> {
    let value = value_at_path(value, path)?;
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn value_at_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(value, |current, segment| current.as_object()?.get(segment))
}

fn benchmark_name(value: &Value) -> anyhow::Result<&str> {
    value
        .get("benchmark")
        .and_then(Value::as_str)
        .context("benchmark JSON must contain a string \"benchmark\" field")
}

fn collect_timing_metrics(value: &Value) -> anyhow::Result<BTreeMap<String, f64>> {
    let mut metrics = BTreeMap::new();
    collect_timing_metrics_recursive(value, "", &mut metrics)?;
    Ok(metrics)
}

fn collect_timing_metrics_recursive(
    value: &Value,
    path: &str,
    metrics: &mut BTreeMap<String, f64>,
) -> anyhow::Result<()> {
    if let Some(array) = value.as_array() {
        for (index, child) in array.iter().enumerate() {
            let child_path = if path.is_empty() {
                format!("[{index}]")
            } else {
                format!("{path}[{index}]")
            };
            collect_timing_metrics_recursive(child, &child_path, metrics)?;
        }
        return Ok(());
    }

    let Some(object) = value.as_object() else {
        return Ok(());
    };

    if let Some(timings) = object.get("timings_ms") {
        let Some(timing_object) = timings.as_object() else {
            bail!("timings_ms at {path:?} must be an object");
        };
        for (name, timing_value) in timing_object {
            let metric_path = join_metric_path(path, "timings_ms", name);
            collect_timing_metric_value(timing_value, &metric_path, metrics)?;
        }
    }

    for (name, child) in object {
        if name == "timings_ms" {
            continue;
        }
        if child.is_object() || child.is_array() {
            let child_path = if path.is_empty() {
                name.to_owned()
            } else {
                format!("{path}.{name}")
            };
            collect_timing_metrics_recursive(child, &child_path, metrics)?;
        }
    }
    Ok(())
}

fn collect_timing_metric_value(
    value: &Value,
    path: &str,
    metrics: &mut BTreeMap<String, f64>,
) -> anyhow::Result<()> {
    if let Some(timing) = value.as_f64() {
        if !timing.is_finite() || timing < 0.0 {
            bail!("timing metric {path:?} must be finite and nonnegative");
        }
        metrics.insert(path.to_owned(), timing);
        return Ok(());
    }
    if value.is_null() {
        return Ok(());
    }
    if let Some(object) = value.as_object() {
        for (name, child) in object {
            collect_timing_metric_value(child, &format!("{path}.{name}"), metrics)?;
        }
        return Ok(());
    }
    bail!("timing metric {path:?} must be a number or nested timing object")
}

fn join_metric_path(prefix: &str, group: &str, name: &str) -> String {
    if prefix.is_empty() {
        format!("{group}.{name}")
    } else {
        format!("{prefix}.{group}.{name}")
    }
}

fn slowdown_pct(current_ms: f64, baseline_ms: f64) -> f64 {
    if baseline_ms == 0.0 {
        if current_ms == 0.0 {
            0.0
        } else {
            f64::INFINITY
        }
    } else {
        ((current_ms - baseline_ms) / baseline_ms) * 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn benchmark_check_classifies_pass_warn_and_fail_slowdowns() {
        let baseline = json!({
            "benchmark": "bench-native-package",
            "timings_ms": {
                "open": 100.0,
                "read_first_volume": 100.0,
                "cpu_dense_mip": 100.0
            }
        });
        let current = json!({
            "benchmark": "bench-native-package",
            "timings_ms": {
                "open": 105.0,
                "read_first_volume": 115.0,
                "cpu_dense_mip": 125.0
            }
        });

        let report = evaluate_benchmark_report(&current, &baseline, 10.0, 20.0).unwrap();

        assert_eq!(report.pass_count(), 1);
        assert_eq!(report.warn_count(), 1);
        assert_eq!(report.fail_count(), 1);
        assert_eq!(
            report
                .comparisons
                .iter()
                .find(|comparison| comparison.metric == "timings_ms.cpu_dense_mip")
                .unwrap()
                .status,
            BenchmarkComparisonStatus::Fail
        );
    }

    #[test]
    fn benchmark_check_collects_nested_gpu_timings() {
        let report = json!({
            "benchmark": "bench-native-package",
            "timings_ms": {
                "open": 10.0
            },
            "gpu": {
                "available": true,
                "timings_ms": {
                    "dense_first_mip": 2.5
                }
            }
        });

        let metrics = collect_timing_metrics(&report).unwrap();

        assert_eq!(metrics["timings_ms.open"], 10.0);
        assert_eq!(metrics["gpu.timings_ms.dense_first_mip"], 2.5);
    }

    #[test]
    fn benchmark_check_ignores_unavailable_null_optional_timings() {
        let report = json!({
            "benchmark": "bench-native-package",
            "gpu": {
                "resident_display_frame": {
                    "timings_ms": {
                        "upload": 1.25,
                        "gpu_compute": null
                    }
                }
            }
        });

        let metrics = collect_timing_metrics(&report).unwrap();

        assert_eq!(
            metrics["gpu.resident_display_frame.timings_ms.upload"],
            1.25
        );
        assert!(!metrics.contains_key("gpu.resident_display_frame.timings_ms.gpu_compute"));
    }

    #[test]
    fn benchmark_check_collects_matrix_array_timings() {
        let report = json!({
            "benchmark": "bench-phase11-viewport-matrix",
            "scenarios": [
                {
                    "label": "square_512",
                    "report": {
                        "summary": {
                            "timings_ms": {
                                "total_command": 10.0,
                                "interaction_frame": {
                                    "p50": 4.0,
                                    "p95": 8.0
                                }
                            }
                        }
                    }
                },
                {
                    "label": "hd_720p",
                    "report": {
                        "summary": {
                            "timings_ms": {
                                "total_command": 20.0
                            }
                        }
                    }
                }
            ]
        });

        let metrics = collect_timing_metrics(&report).unwrap();

        assert_eq!(
            metrics["scenarios[0].report.summary.timings_ms.total_command"],
            10.0
        );
        assert_eq!(
            metrics["scenarios[0].report.summary.timings_ms.interaction_frame.p50"],
            4.0
        );
        assert_eq!(
            metrics["scenarios[0].report.summary.timings_ms.interaction_frame.p95"],
            8.0
        );
        assert_eq!(
            metrics["scenarios[1].report.summary.timings_ms.total_command"],
            20.0
        );
    }

    #[test]
    fn benchmark_check_reports_missing_baseline_metrics() {
        let baseline = json!({
            "benchmark": "bench-smoke",
            "timings_ms": {
                "open": 1.0,
                "read_timepoint": 2.0
            }
        });
        let current = json!({
            "benchmark": "bench-smoke",
            "timings_ms": {
                "open": 1.0
            }
        });

        let report = evaluate_benchmark_report(&current, &baseline, 10.0, 20.0).unwrap();

        assert_eq!(report.missing_metrics, vec!["timings_ms.read_timepoint"]);
        assert_eq!(report.fail_count(), 0);
    }

    #[test]
    fn benchmark_check_rejects_benchmark_name_mismatch() {
        let baseline = json!({
            "benchmark": "bench-smoke",
            "timings_ms": {
                "open": 1.0
            }
        });
        let current = json!({
            "benchmark": "bench-native-package",
            "timings_ms": {
                "open": 1.0
            }
        });

        assert!(evaluate_benchmark_report(&current, &baseline, 10.0, 20.0).is_err());
    }

    #[test]
    fn benchmark_check_rejects_schema_mismatch() {
        let baseline = json!({
            "benchmark": "bench-phase13-renderer",
            "benchmark_schema_version": 1,
            "timings_ms": {
                "render": 1.0
            }
        });
        let current = json!({
            "benchmark": "bench-phase13-renderer",
            "benchmark_schema_version": 2,
            "timings_ms": {
                "render": 1.0
            }
        });

        let err = evaluate_benchmark_report(&current, &baseline, 10.0, 20.0)
            .unwrap_err()
            .to_string();

        assert!(err.contains("benchmark context mismatch at benchmark_schema_version"));
    }

    #[test]
    fn benchmark_check_rejects_scenario_mismatch() {
        let baseline = json!({
            "benchmark": "bench-phase11-interaction",
            "scenario": {
                "name": "square_512"
            },
            "timings_ms": {
                "interaction": 1.0
            }
        });
        let current = json!({
            "benchmark": "bench-phase11-interaction",
            "scenario": {
                "name": "hd_720p"
            },
            "timings_ms": {
                "interaction": 1.0
            }
        });

        let err = evaluate_benchmark_report(&current, &baseline, 10.0, 20.0)
            .unwrap_err()
            .to_string();

        assert!(err.contains("benchmark context mismatch at scenario.name"));
    }

    #[test]
    fn benchmark_check_rejects_hardware_context_mismatch() {
        let baseline = json!({
            "benchmark": "bench-phase11-interaction",
            "hardware": {
                "name": "lab-workstation"
            },
            "dataset": {
                "id": "T5-QUAL-001"
            },
            "timings_ms": {
                "interaction": 1.0
            }
        });
        let current = json!({
            "benchmark": "bench-phase11-interaction",
            "hardware": {
                "name": "laptop"
            },
            "dataset": {
                "id": "T5-QUAL-001"
            },
            "timings_ms": {
                "interaction": 1.0
            }
        });

        let err = evaluate_benchmark_report(&current, &baseline, 10.0, 20.0)
            .unwrap_err()
            .to_string();

        assert!(err.contains("benchmark context mismatch at hardware.name"));
    }

    #[test]
    fn benchmark_check_rejects_dataset_context_mismatch() {
        let baseline = json!({
            "benchmark": "bench-phase11-interaction",
            "hardware": {
                "name": "lab-workstation"
            },
            "dataset": {
                "id": "T5-QUAL-001"
            },
            "timings_ms": {
                "interaction": 1.0
            }
        });
        let current = json!({
            "benchmark": "bench-phase11-interaction",
            "hardware": {
                "name": "lab-workstation"
            },
            "dataset": {
                "id": "t5_qual_002"
            },
            "timings_ms": {
                "interaction": 1.0
            }
        });

        let err = evaluate_benchmark_report(&current, &baseline, 10.0, 20.0)
            .unwrap_err()
            .to_string();

        assert!(err.contains("benchmark context mismatch at dataset.id"));
    }

    #[test]
    fn benchmark_check_rejects_baseline_class_mismatch() {
        let baseline = json!({
            "benchmark": "bench-phase11-interaction",
            "baseline_class": "synthetic_ci",
            "timings_ms": {
                "interaction": 1.0
            }
        });
        let current = json!({
            "benchmark": "bench-phase11-interaction",
            "baseline_class": "private_local_heavy",
            "timings_ms": {
                "interaction": 1.0
            }
        });

        let err = evaluate_benchmark_report(&current, &baseline, 10.0, 20.0)
            .unwrap_err()
            .to_string();

        assert!(err.contains("benchmark context mismatch at baseline_class"));
    }
}

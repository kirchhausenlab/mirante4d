use std::{fs, path::Path};

use anyhow::Context;
use serde_json::{Value, json};

pub(crate) fn read_json_file(path: &Path) -> anyhow::Result<Value> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

pub(crate) fn write_json_file(path: &Path, value: &Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(value).context("failed to serialize JSON report")?;
    fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
}

pub(crate) fn timing_summary_json(values: &[f64]) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    let sum = values.iter().sum::<f64>();
    json!({
        "count": values.len(),
        "min": values.iter().copied().fold(f64::INFINITY, f64::min),
        "p50": percentile_nearest_rank(values, 0.50),
        "p95": percentile_nearest_rank(values, 0.95),
        "p99": percentile_nearest_rank(values, 0.99),
        "max": values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        "mean": sum / values.len() as f64,
    })
}

pub(crate) fn phase13_gpu_timings_json(render_ms: f64, upload_ms: Option<f64>) -> Value {
    let mut timings = serde_json::Map::new();
    timings.insert("render".to_owned(), json!(render_ms));
    if let Some(upload_ms) = upload_ms {
        timings.insert("upload".to_owned(), json!(upload_ms));
    }
    Value::Object(timings)
}

pub(crate) fn phase11_gpu_interaction_timings_json(
    render_ms: f64,
    upload_ms: Option<f64>,
) -> Value {
    let mut timings = serde_json::Map::new();
    timings.insert("resident_mip".to_owned(), json!(render_ms));
    if let Some(upload_ms) = upload_ms {
        timings.insert("resident_mip_upload".to_owned(), json!(upload_ms));
    }
    Value::Object(timings)
}

fn percentile_nearest_rank(values: &[f64], percentile: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|left, right| left.total_cmp(right));
    let percentile = percentile.clamp(0.0, 1.0);
    let rank = ((sorted.len() as f64 * percentile).ceil() as usize).saturating_sub(1);
    sorted.get(rank.min(sorted.len() - 1)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_nearest_rank_is_stable_for_benchmark_summaries() {
        let values = [10.0, 30.0, 20.0, 40.0];

        assert_eq!(percentile_nearest_rank(&values, 0.0), Some(10.0));
        assert_eq!(percentile_nearest_rank(&values, 0.50), Some(20.0));
        assert_eq!(percentile_nearest_rank(&values, 0.95), Some(40.0));
        assert_eq!(percentile_nearest_rank(&values, 1.0), Some(40.0));
        assert_eq!(percentile_nearest_rank(&[], 0.50), None);
    }

    #[test]
    fn phase13_gpu_timings_include_upload_only_when_measured() {
        let measured = phase13_gpu_timings_json(10.5, Some(2.25));
        assert_eq!(measured["render"], 10.5);
        assert_eq!(measured["upload"], 2.25);

        let unavailable = phase13_gpu_timings_json(10.5, None);
        assert_eq!(unavailable["render"], 10.5);
        assert!(unavailable.get("upload").is_none());
    }

    #[test]
    fn phase11_gpu_interaction_timings_preserve_resident_mip_metric() {
        let measured = phase11_gpu_interaction_timings_json(12.0, Some(3.5));
        assert_eq!(measured["resident_mip"], 12.0);
        assert_eq!(measured["resident_mip_upload"], 3.5);

        let unavailable = phase11_gpu_interaction_timings_json(12.0, None);
        assert_eq!(unavailable["resident_mip"], 12.0);
        assert!(unavailable.get("resident_mip_upload").is_none());
    }
}

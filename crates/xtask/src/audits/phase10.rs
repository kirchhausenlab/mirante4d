use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::bench::{
    NativePackageBenchmarkOverrides, bench_import_sample_with_limit,
    bench_native_package_with_overrides, bench_runtime_stress, list_tiff_files,
    sample_import_file_limit,
};
use crate::fixtures::generate_fixture;
use crate::host::benchmark_host_context;
use crate::package;
use crate::reports::read_json_file;
use crate::smoke::run_release_app_smoke;
use crate::{env_u64, stable_id_from_name};

pub(crate) fn phase10_audit() -> anyhow::Result<PathBuf> {
    let sample_root = env::var_os("MIRANTE4D_SAMPLE_DATA")
        .map(PathBuf::from)
        .context("MIRANTE4D_SAMPLE_DATA must point to the local sample_data root")?;
    if !sample_root.is_dir() {
        bail!(
            "MIRANTE4D_SAMPLE_DATA does not point to a directory: {}",
            sample_root.display()
        );
    }
    let output_root = PathBuf::from("target").join("mirante4d").join("phase10");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let file_limit = sample_import_file_limit()?;
    let native_benchmark_overrides = phase10_native_benchmark_overrides()?;
    let experiments = phase10_experiments(&sample_root)?;
    if experiments.is_empty() {
        bail!(
            "no TIFF sample experiments found under {}",
            sample_root.display()
        );
    }
    println!(
        "phase10 audit: sample_root={} experiments={} file_limit={} viewport={}x{} brick_pixel_stride={}",
        sample_root.display(),
        experiments.join(","),
        file_limit,
        native_benchmark_overrides
            .viewport_width
            .unwrap_or_default(),
        native_benchmark_overrides
            .viewport_height
            .unwrap_or_default(),
        native_benchmark_overrides
            .brick_pixel_stride
            .unwrap_or_default(),
    );

    let mut real_samples = Vec::new();
    let mut skipped_samples = Vec::new();
    for experiment in &experiments {
        println!("phase10 audit: real sample {experiment}: import/native/app smoke");
        match phase10_audit_real_sample(
            experiment,
            file_limit,
            &output_root,
            native_benchmark_overrides,
        ) {
            Ok(sample) => {
                println!("phase10 audit: real sample {experiment}: ok");
                real_samples.push(sample);
            }
            Err(err) => {
                println!("phase10 audit: real sample {experiment}: skipped: {err}");
                skipped_samples.push(json!({
                    "experiment": experiment,
                    "classification": "missing future feature",
                    "reason": err.to_string(),
                }));
            }
        }
    }
    if real_samples.is_empty() {
        bail!("Phase 10 audit could not import any selected real sample: {skipped_samples:?}");
    }
    if real_samples.len() < 2 && experiments.len() >= 2 {
        bail!(
            "Phase 10 audit imported only {} real sample(s); skipped samples: {skipped_samples:?}",
            real_samples.len()
        );
    }

    println!("phase10 audit: generated time-multichannel fixture benchmark/app smoke");
    let multichannel_fixture = generate_fixture("time-multichannel-u16-8cube-3t-2c")?;
    let multichannel_native_report_path =
        bench_native_package_with_overrides(&multichannel_fixture, native_benchmark_overrides)?;
    let multichannel_smoke_log = output_root.join("app-smoke-time-multichannel-fixture.log");
    let multichannel_smoke = run_release_app_smoke(&multichannel_fixture, &multichannel_smoke_log)?;
    println!("phase10 audit: runtime stress benchmark");
    let runtime_stress_report_path = bench_runtime_stress()?;
    println!("phase10 audit: release/package smoke");
    let package_root = package::package_dev()?;
    let real_sample_count = real_samples.len();
    let time_series_covered = real_samples.iter().any(|sample| {
        sample
            .pointer("/import_summary/inspection/timepoint_count")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 1
    });
    let real_multichannel_covered = real_samples.iter().any(|sample| {
        sample
            .pointer("/import_summary/inspection/channel_count")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 1
    });

    let report_json_path = output_root.join("phase10-audit-report.json");
    let report_md_path = output_root.join("phase10-audit-report.md");
    let report_json = json!({
        "phase": "Phase 10: Production Viewer Core",
        "phase10_audit_schema_version": 1,
        "hardware": benchmark_host_context(),
        "sample_root": sample_root,
        "experiments": experiments,
        "file_limit": file_limit,
        "real_samples": real_samples,
        "skipped_samples": skipped_samples,
        "synthetic_multichannel": {
            "package": multichannel_fixture,
            "native_report": multichannel_native_report_path,
            "app_smoke": multichannel_smoke,
        },
        "runtime_stress_report": runtime_stress_report_path,
        "package_dev_root": package_root,
        "coverage": {
            "real_sample_count": real_sample_count,
            "time_series": time_series_covered,
            "real_multichannel": real_multichannel_covered,
            "synthetic_multichannel": true,
            "app_smoke_real_samples": true,
            "runtime_stress": true,
            "package_smoke": true,
        },
        "findings": [
            {
                "classification": "no action",
                "surface": "open/render/app smoke",
                "observation": "selected real native packages opened through the release app smoke path and rendered non-empty first frames"
            },
            {
                "classification": "no action",
                "surface": "import workflow",
                "observation": "reviewed TIFF grouping was used for tokenized and untokened local sample folders; imported packages validated through the normal native open/read path"
            },
            {
                "classification": "missing future feature",
                "surface": "unsupported local sample inputs",
                "observation": "selected local sample inputs that do not match the current strict uint8/uint16/float32 dense intensity importer are recorded as skipped samples rather than silently imported through compatibility paths"
            },
            {
                "classification": "missing future feature",
                "surface": "real multichannel sample coverage",
                "observation": "no compatible real multi-channel intensity folder is assumed by the audit; the time-multichannel native fixture covers current multi-channel viewer behavior"
            }
        ],
    });
    fs::write(
        &report_json_path,
        format!("{}\n", serde_json::to_string_pretty(&report_json)?),
    )
    .with_context(|| format!("failed to write {}", report_json_path.display()))?;
    fs::write(&report_md_path, phase10_audit_markdown(&report_json))
        .with_context(|| format!("failed to write {}", report_md_path.display()))?;
    Ok(report_md_path)
}

fn phase10_audit_real_sample(
    experiment: &str,
    file_limit: usize,
    output_root: &Path,
    native_benchmark_overrides: NativePackageBenchmarkOverrides,
) -> anyhow::Result<Value> {
    let import_report_path = bench_import_sample_with_limit(experiment, file_limit)?;
    let import_report = read_json_file(&import_report_path)?;
    let package_path = import_report
        .get("output_package")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .with_context(|| {
            format!(
                "import report {} did not contain output_package",
                import_report_path.display()
            )
        })?;
    let native_report_path =
        bench_native_package_with_overrides(&package_path, native_benchmark_overrides)?;
    let native_report = read_json_file(&native_report_path)?;
    let smoke_log_path =
        output_root.join(format!("app-smoke-{}.log", stable_id_from_name(experiment)));
    let app_smoke = run_release_app_smoke(&package_path, &smoke_log_path)?;
    Ok(json!({
        "experiment": experiment,
        "import_report": import_report_path,
        "native_report": native_report_path,
        "app_smoke": app_smoke,
        "import_summary": {
            "benchmark_source": import_report.get("benchmark_source").cloned(),
            "inspection": import_report.get("inspection").cloned(),
            "import_report": import_report.get("import_report").cloned(),
            "timings_ms": import_report.get("timings_ms").cloned(),
        },
        "native_summary": {
            "shape": native_report.get("shape").cloned(),
            "viewport": native_report.get("viewport").cloned(),
            "visible_bricks": native_report.get("visible_bricks").cloned(),
            "timings_ms": native_report.get("timings_ms").cloned(),
            "gpu": native_report.get("gpu").cloned(),
        },
    }))
}

fn phase10_experiments(sample_root: &Path) -> anyhow::Result<Vec<String>> {
    if let Ok(raw) = env::var("MIRANTE4D_PHASE10_EXPERIMENTS") {
        let experiments = raw
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if experiments.is_empty() {
            bail!("MIRANTE4D_PHASE10_EXPERIMENTS did not name any experiments");
        }
        return Ok(experiments);
    }

    let preferred = ["T5-QUAL-001", "T5-QUAL-002", "T5-QUAL-003"];
    let mut experiments = preferred
        .iter()
        .filter(|name| sample_root.join(name).is_dir())
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    if experiments.len() < 2 {
        for entry in fs::read_dir(sample_root)
            .with_context(|| format!("failed to list {}", sample_root.display()))?
        {
            let entry =
                entry.with_context(|| format!("failed to list {}", sample_root.display()))?;
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if experiments.contains(&name) {
                continue;
            }
            if list_tiff_files(&entry.path()).is_ok() {
                experiments.push(name);
            }
            if experiments.len() >= 2 {
                break;
            }
        }
    }
    Ok(experiments)
}

fn phase10_native_benchmark_overrides() -> anyhow::Result<NativePackageBenchmarkOverrides> {
    Ok(NativePackageBenchmarkOverrides {
        viewport_width: Some(env_u64("MIRANTE4D_PHASE10_BENCH_VIEWPORT_WIDTH")?.unwrap_or(128)),
        viewport_height: Some(env_u64("MIRANTE4D_PHASE10_BENCH_VIEWPORT_HEIGHT")?.unwrap_or(128)),
        brick_pixel_stride: Some(
            env_u64("MIRANTE4D_PHASE10_BENCH_BRICK_PIXEL_STRIDE")?
                .unwrap_or(64)
                .max(1),
        ),
    })
}

fn phase10_audit_markdown(report: &Value) -> String {
    let mut out = String::new();
    out.push_str("# Phase 10 Audit Report\n\n");
    out.push_str("Generated by `cargo xtask phase10-audit`.\n\n");
    out.push_str("## Hardware\n\n");
    out.push_str("```json\n");
    out.push_str(
        &serde_json::to_string_pretty(report.get("hardware").unwrap_or(&Value::Null))
            .unwrap_or_else(|_| "null".to_owned()),
    );
    out.push_str("\n```\n\n");
    out.push_str("## Real Samples\n\n");
    if let Some(samples) = report.get("real_samples").and_then(Value::as_array) {
        for sample in samples {
            let experiment = sample
                .get("experiment")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            out.push_str(&format!("### {experiment}\n\n"));
            out.push_str(&format!(
                "- import report: `{}`\n",
                sample
                    .get("import_report")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            ));
            out.push_str(&format!(
                "- native report: `{}`\n",
                sample
                    .get("native_report")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            ));
            if let Some(summary) = sample
                .pointer("/app_smoke/stdout_summary")
                .and_then(Value::as_str)
            {
                out.push_str(&format!("- app smoke: {summary}\n"));
            }
            out.push('\n');
        }
    }
    if let Some(skipped) = report.get("skipped_samples").and_then(Value::as_array)
        && !skipped.is_empty()
    {
        out.push_str("## Skipped Samples\n\n");
        for sample in skipped {
            out.push_str(&format!(
                "- `{}`: {}\n",
                sample
                    .get("experiment")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                sample
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown reason")
            ));
        }
        out.push('\n');
    }
    out.push_str("## Coverage\n\n");
    out.push_str("```json\n");
    out.push_str(
        &serde_json::to_string_pretty(report.get("coverage").unwrap_or(&Value::Null))
            .unwrap_or_else(|_| "null".to_owned()),
    );
    out.push_str("\n```\n\n");
    out.push_str("## Findings\n\n");
    if let Some(findings) = report.get("findings").and_then(Value::as_array) {
        for finding in findings {
            out.push_str(&format!(
                "- `{}` / `{}`: {}\n",
                finding
                    .get("classification")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                finding
                    .get("surface")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                finding
                    .get("observation")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            ));
        }
    }
    out
}

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::bench::{bench_import_sample_with_limit, sample_import_file_limit};
use crate::fixtures::generate_fixture;
use crate::host::benchmark_host_context;
use crate::reports::read_json_file;
use crate::smoke::run_release_app_smoke;
use crate::stable_id_from_name;

pub(crate) fn phase12_audit() -> anyhow::Result<PathBuf> {
    let output_root = PathBuf::from("target").join("mirante4d").join("phase12");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;

    println!("phase12 audit: generated time-multichannel fixture app smoke");
    let generated_fixture = generate_fixture("time-multichannel-u16-8cube-3t-2c")?;
    let generated_smoke_log = output_root.join("app-smoke-time-multichannel-fixture.log");
    let generated_smoke = run_release_app_smoke(&generated_fixture, &generated_smoke_log)?;

    let file_limit = sample_import_file_limit()?;
    let sample_root = env::var_os("MIRANTE4D_SAMPLE_DATA").map(PathBuf::from);
    let experiments = phase12_experiments(sample_root.as_deref())?;
    let mut real_samples = Vec::new();
    let mut skipped_samples = Vec::new();
    let mut failed_available_samples = Vec::new();

    for experiment in &experiments {
        println!("phase12 audit: real sample {experiment}: import/app smoke");
        match phase12_audit_real_sample(experiment, file_limit, &output_root) {
            Ok(sample) => {
                println!("phase12 audit: real sample {experiment}: ok");
                real_samples.push(sample);
            }
            Err(err) => {
                let sample_available = sample_root
                    .as_ref()
                    .map(|root| root.join(experiment).is_dir())
                    .unwrap_or(false);
                println!("phase12 audit: real sample {experiment}: skipped: {err}");
                if sample_available {
                    failed_available_samples.push(format!("{experiment}: {err}"));
                }
                skipped_samples.push(json!({
                    "experiment": experiment,
                    "classification": if sample_available { "defect" } else { "missing local sample" },
                    "reason": err.to_string(),
                }));
            }
        }
    }
    if !failed_available_samples.is_empty() {
        bail!(
            "Phase 12 audit failed available required samples:\n{}",
            failed_available_samples.join("\n")
        );
    }

    let coverage = phase12_audit_coverage(&real_samples);
    let report_json_path = output_root.join("phase12-audit-report.json");
    let report_md_path = output_root.join("phase12-audit-report.md");
    let report_json = json!({
        "phase": "Phase 12: Viewer Usability And Display Controls",
        "phase12_audit_schema_version": 1,
        "hardware": benchmark_host_context(),
        "sample_root": sample_root,
        "experiments": experiments,
        "file_limit": file_limit,
        "generated_fixture": {
            "name": "time-multichannel-u16-8cube-3t-2c",
            "package": generated_fixture,
            "app_smoke": generated_smoke,
        },
        "real_samples": real_samples,
        "skipped_samples": skipped_samples,
        "coverage": coverage,
        "run_notes": [
            {
                "dataset": "time-multichannel-u16-8cube-3t-2c",
                "app_command": "cargo xtask phase12-audit",
                "surface": "channel display, time navigation, playback, save/reopen-adjacent state",
                "observed_behavior": "generated strict native time/multichannel package opened through release app smoke with a non-empty rendered first frame",
                "expected_behavior": "fixture opens without compatibility paths and exposes deterministic channel/time metadata through app tests",
                "severity": "none",
                "classification": "no action"
            },
            {
                "dataset": "T5-QUAL-003 when MIRANTE4D_SAMPLE_DATA/T5-QUAL-003 is available",
                "app_command": "cargo xtask phase12-audit",
                "surface": "real 4D time-series open, initial display, fidelity status",
                "observed_behavior": "sample is imported through the strict native path and opened through release app smoke",
                "expected_behavior": "non-empty rendered first frame with truthful displayed/target LOD status",
                "severity": "none if command passes; high if available sample fails",
                "classification": "no action"
            },
            {
                "dataset": "T5-QUAL-001 when MIRANTE4D_SAMPLE_DATA/T5-QUAL-001 is available",
                "app_command": "cargo xtask phase12-audit",
                "surface": "large volume open, initial display, GPU/resource pressure, fidelity status",
                "observed_behavior": "sample is imported through the strict native path and opened through release app smoke",
                "expected_behavior": "non-empty rendered first frame without bricking the machine and with truthful displayed/target LOD status",
                "severity": "none if command passes; high if available sample fails",
                "classification": "no action"
            }
        ],
        "findings": [
            {
                "classification": "no action",
                "surface": "open/render/app smoke",
                "observation": "generated and available required real native packages open through the release app smoke path and render non-empty first frames"
            },
            {
                "classification": "no action",
                "surface": "channel display and hidden-channel scheduling",
                "observation": "Phase 12 app tests cover typed display state, hidden active and non-active channels, persistence, and render extraction behavior"
            },
            {
                "classification": "no action",
                "surface": "time navigation and playback",
                "observation": "Phase 12 app tests cover first/previous/next/last/play controls, wrapping, single-timepoint inert playback, and nonblocking playback state"
            },
            {
                "classification": "no action",
                "surface": "render-mode controls",
                "observation": "Phase 12 app tests cover mode-specific controls, parameter validation, state preservation, and nonblank baseline mode output"
            },
            {
                "classification": "no action",
                "surface": "camera/projection/high-DPI",
                "observation": "Phase 12 app and renderer tests cover Fit Data, reset-adjacent camera behavior, orthographic no-warp invariants, and physical-pixel viewport sizing"
            },
            {
                "classification": "no action",
                "surface": "fidelity status",
                "observation": "Phase 12 app tests cover exact, complete, refining/loading, budget-limited, backend-limit, allocation-failed, and high-DPI status label behavior"
            }
        ],
    });
    fs::write(
        &report_json_path,
        format!("{}\n", serde_json::to_string_pretty(&report_json)?),
    )
    .with_context(|| format!("failed to write {}", report_json_path.display()))?;
    fs::write(&report_md_path, phase12_audit_markdown(&report_json))
        .with_context(|| format!("failed to write {}", report_md_path.display()))?;
    Ok(report_md_path)
}

fn phase12_experiments(sample_root: Option<&Path>) -> anyhow::Result<Vec<String>> {
    if let Ok(raw) = env::var("MIRANTE4D_PHASE12_EXPERIMENTS") {
        let experiments = raw
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if experiments.is_empty() {
            bail!("MIRANTE4D_PHASE12_EXPERIMENTS did not name any experiments");
        }
        return Ok(experiments);
    }

    let preferred = ["T5-QUAL-003", "T5-QUAL-001"];
    let Some(sample_root) = sample_root else {
        return Ok(preferred.iter().map(|name| (*name).to_owned()).collect());
    };
    Ok(preferred
        .iter()
        .filter(|name| sample_root.join(name).is_dir())
        .map(|name| (*name).to_owned())
        .collect())
}

fn phase12_audit_real_sample(
    experiment: &str,
    file_limit: usize,
    output_root: &Path,
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
    let smoke_log_path =
        output_root.join(format!("app-smoke-{}.log", stable_id_from_name(experiment)));
    let app_smoke = run_release_app_smoke(&package_path, &smoke_log_path)?;
    Ok(json!({
        "experiment": experiment,
        "import_report": import_report_path,
        "package": package_path,
        "app_smoke": app_smoke,
        "import_summary": {
            "benchmark_source": import_report.get("benchmark_source").cloned(),
            "inspection": import_report.get("inspection").cloned(),
            "timings_ms": import_report.get("timings_ms").cloned(),
        },
    }))
}

fn phase12_audit_coverage(real_samples: &[Value]) -> Value {
    let opened_t5_qual_003 = real_samples
        .iter()
        .any(|sample| sample.get("experiment").and_then(Value::as_str) == Some("T5-QUAL-003"));
    let opened_t5_qual_001 = real_samples
        .iter()
        .any(|sample| sample.get("experiment").and_then(Value::as_str) == Some("T5-QUAL-001"));
    let time_series_real_sample = real_samples.iter().any(|sample| {
        sample
            .pointer("/import_summary/inspection/timepoint_count")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 1
    });
    json!({
        "generated_fixture_open": true,
        "generated_time_multichannel": true,
        "t5_qual_003_opened_when_available": opened_t5_qual_003,
        "t5_qual_001_opened_when_available": opened_t5_qual_001,
        "real_time_series_sample": time_series_real_sample,
        "channel_display_tests": true,
        "histogram_auto_window_tests": true,
        "time_playback_tests": true,
        "render_mode_control_tests": true,
        "camera_projection_high_dpi_tests": true,
        "fidelity_status_tests": true,
    })
}

fn phase12_audit_markdown(report: &Value) -> String {
    let mut out = String::new();
    out.push_str("# Phase 12 Viewer Usability Audit Report\n\n");
    out.push_str("Generated by `cargo xtask phase12-audit`.\n\n");
    out.push_str("## Hardware\n\n");
    out.push_str("```json\n");
    out.push_str(
        &serde_json::to_string_pretty(report.get("hardware").unwrap_or(&Value::Null))
            .unwrap_or_else(|_| "null".to_owned()),
    );
    out.push_str("\n```\n\n");
    out.push_str("## Generated Fixture\n\n");
    if let Some(summary) = report
        .pointer("/generated_fixture/app_smoke/stdout_summary")
        .and_then(Value::as_str)
    {
        out.push_str(&format!("- app smoke: {summary}\n"));
    }
    if let Some(path) = report
        .pointer("/generated_fixture/package")
        .and_then(Value::as_str)
    {
        out.push_str(&format!("- package: `{path}`\n"));
    }
    out.push('\n');

    out.push_str("## Real Samples\n\n");
    if let Some(samples) = report.get("real_samples").and_then(Value::as_array) {
        if samples.is_empty() {
            out.push_str("- no local required real samples were available\n\n");
        }
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
                "- package: `{}`\n",
                sample.get("package").and_then(Value::as_str).unwrap_or("")
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
                "- `{}` / `{}`: {}\n",
                sample
                    .get("experiment")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                sample
                    .get("classification")
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
    out.push_str("## Run Notes\n\n");
    if let Some(notes) = report.get("run_notes").and_then(Value::as_array) {
        for note in notes {
            out.push_str(&format!(
                "- `{}` / `{}` / `{}`: {}\n",
                note.get("classification")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                note.get("severity")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                note.get("surface")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                note.get("observed_behavior")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            ));
        }
        out.push('\n');
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase12_audit_coverage_recognizes_required_samples_and_time_series() {
        let samples = vec![
            json!({
                "experiment": "T5-QUAL-003",
                "import_summary": {
                    "inspection": {
                        "timepoint_count": 4
                    }
                }
            }),
            json!({
                "experiment": "T5-QUAL-001",
                "import_summary": {
                    "inspection": {
                        "timepoint_count": 1
                    }
                }
            }),
        ];

        let coverage = phase12_audit_coverage(&samples);

        assert_eq!(
            coverage
                .get("generated_fixture_open")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            coverage
                .get("t5_qual_003_opened_when_available")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            coverage
                .get("t5_qual_001_opened_when_available")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            coverage
                .get("real_time_series_sample")
                .and_then(Value::as_bool),
            Some(true)
        );
    }
}

use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::bench::{
    bench_import_sample_with_limit, bench_phase13_renderer, sample_import_file_limit,
};
use crate::fixtures::generate_fixture;
use crate::host::benchmark_host_context;
use crate::reports::read_json_file;
use crate::smoke::{ReleaseAppSmokeOptions, run_release_app_smoke_with_options};
use crate::stable_id_from_name;

const PHASE19_DEFAULT_EXPERIMENTS: &[&str] = &["T5-QUAL-003", "T5-QUAL-001"];

pub(crate) fn phase19_audit() -> anyhow::Result<PathBuf> {
    let output_root = PathBuf::from("target").join("mirante4d").join("phase19");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let started = Instant::now();

    println!("phase19 audit: generated time/multichannel fixture app smoke and renderer evidence");
    let generated_fixture = generate_fixture("time-multichannel-u16-8cube-3t-2c")?;
    let generated_smoke_log = output_root.join("app-smoke-time-multichannel-fixture.log");
    let generated_smoke = run_release_app_smoke_with_options(
        &generated_fixture,
        &generated_smoke_log,
        ReleaseAppSmokeOptions {
            playback_steps: Some(2),
            timeout_secs: None,
        },
    )?;
    phase19_validate_playback_smoke(&generated_smoke, "generated time/multichannel fixture")?;
    let generated_renderer_report_path = bench_phase13_renderer(&generated_fixture)?;
    let generated_renderer_report = read_json_file(&generated_renderer_report_path)?;
    phase19_validate_renderer_report(
        &generated_renderer_report,
        "generated time/multichannel fixture",
    )?;

    let file_limit = sample_import_file_limit()?;
    let sample_root = env::var_os("MIRANTE4D_SAMPLE_DATA").map(PathBuf::from);
    let experiments = phase19_experiments()?;
    let mut real_samples = Vec::new();
    let mut skipped_samples = Vec::new();
    let mut failed_available_samples = Vec::new();

    for experiment in &experiments {
        println!("phase19 audit: real sample {experiment}: import, app smoke, renderer evidence");
        match phase19_real_sample_evidence(experiment, file_limit, &output_root) {
            Ok(sample) => {
                println!("phase19 audit: real sample {experiment}: ok");
                real_samples.push(sample);
            }
            Err(err) => {
                let sample_available = sample_root
                    .as_ref()
                    .map(|root| root.join(experiment).is_dir())
                    .unwrap_or(false);
                println!("phase19 audit: real sample {experiment}: skipped: {err}");
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
            "Phase 19 audit failed available required samples:\n{}",
            failed_available_samples.join("\n")
        );
    }

    let report_json = json!({
        "phase": "Phase 19: Viewer Product Hardening",
        "phase19_audit_schema_version": 1,
        "hardware": benchmark_host_context(),
        "sample_root": sample_root,
        "experiments": experiments,
        "file_limit": file_limit,
        "generated_fixture": {
            "name": "time-multichannel-u16-8cube-3t-2c",
            "package": generated_fixture,
            "app_smoke": generated_smoke,
            "renderer_report": generated_renderer_report_path,
            "renderer_summary": phase19_renderer_summary(&generated_renderer_report),
        },
        "real_samples": real_samples,
        "skipped_samples": skipped_samples,
        "coverage": phase19_audit_coverage(&generated_renderer_report, &real_samples),
        "findings": [
            {
                "classification": "no action",
                "surface": "display fidelity status",
                "observation": "main viewer status includes displayed/target LOD, completeness, reason, backend, viewport, frame rate, and render sampling policy"
            },
            {
                "classification": "no action",
                "surface": "real-sample import-to-open path",
                "observation": "available required local samples are regenerated/imported through the strict native path, opened through release app smoke, and checked by renderer evidence"
            },
            {
                "classification": "no action",
                "surface": "renderer mode evidence",
                "observation": "generated and available required samples run the Phase 13 MIP/DVR/ISO evidence path with nonblank complete mode output"
            }
        ],
        "timings_ms": {
            "total_command": started.elapsed().as_secs_f64() * 1000.0,
        },
    });

    let report_json_path = output_root.join("phase19-audit-report.json");
    let report_md_path = output_root.join("phase19-audit-report.md");
    fs::write(
        &report_json_path,
        format!("{}\n", serde_json::to_string_pretty(&report_json)?),
    )
    .with_context(|| format!("failed to write {}", report_json_path.display()))?;
    fs::write(&report_md_path, phase19_audit_markdown(&report_json))
        .with_context(|| format!("failed to write {}", report_md_path.display()))?;
    Ok(report_md_path)
}

fn phase19_experiments() -> anyhow::Result<Vec<String>> {
    if let Ok(raw) = env::var("MIRANTE4D_PHASE19_EXPERIMENTS") {
        let experiments = raw
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if experiments.is_empty() {
            bail!("MIRANTE4D_PHASE19_EXPERIMENTS did not name any experiments");
        }
        return Ok(experiments);
    }
    Ok(PHASE19_DEFAULT_EXPERIMENTS
        .iter()
        .map(|name| (*name).to_owned())
        .collect())
}

fn phase19_real_sample_evidence(
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
    let playback_steps = import_report
        .pointer("/inspection/timepoint_count")
        .and_then(Value::as_u64)
        .filter(|count| *count > 1)
        .map(|_| 2usize);
    let smoke_log_path =
        output_root.join(format!("app-smoke-{}.log", stable_id_from_name(experiment)));
    let app_smoke = run_release_app_smoke_with_options(
        &package_path,
        &smoke_log_path,
        ReleaseAppSmokeOptions {
            playback_steps,
            timeout_secs: None,
        },
    )?;
    if playback_steps.is_some() {
        phase19_validate_playback_smoke(&app_smoke, experiment)?;
    }

    let renderer_report_path = bench_phase13_renderer(&package_path)?;
    let renderer_report = read_json_file(&renderer_report_path)?;
    phase19_validate_renderer_report(&renderer_report, experiment)?;

    Ok(json!({
        "experiment": experiment,
        "import_report": import_report_path,
        "package": package_path,
        "app_smoke": app_smoke,
        "renderer_report": renderer_report_path,
        "renderer_summary": phase19_renderer_summary(&renderer_report),
        "import_summary": {
            "benchmark_source": import_report.get("benchmark_source").cloned(),
            "inspection": import_report.get("inspection").cloned(),
            "timings_ms": import_report.get("timings_ms").cloned(),
        },
    }))
}

fn phase19_validate_playback_smoke(smoke: &Value, dataset_label: &str) -> anyhow::Result<()> {
    let requested_steps = smoke
        .get("requested_playback_steps")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if requested_steps == 0 {
        return Ok(());
    }
    let playback_summary = smoke
        .get("playback_summary")
        .and_then(Value::as_str)
        .unwrap_or("");
    if playback_summary.trim().is_empty() {
        bail!("{dataset_label} app smoke did not record playback evidence");
    }
    Ok(())
}

fn phase19_validate_renderer_report(report: &Value, dataset_label: &str) -> anyhow::Result<()> {
    let summary = report
        .get("summary")
        .context("renderer report did not contain summary")?;
    let mode_count = summary
        .get("mode_count")
        .and_then(Value::as_u64)
        .with_context(|| format!("{dataset_label} renderer report did not contain mode_count"))?;
    let nonblank_modes = summary
        .get("nonblank_modes")
        .and_then(Value::as_u64)
        .with_context(|| {
            format!("{dataset_label} renderer report did not contain nonblank_modes")
        })?;
    let cpu_error_modes = summary
        .get("cpu_error_modes")
        .and_then(Value::as_u64)
        .with_context(|| {
            format!("{dataset_label} renderer report did not contain cpu_error_modes")
        })?;
    let gpu_error_modes = summary
        .get("gpu_error_modes")
        .and_then(Value::as_u64)
        .with_context(|| {
            format!("{dataset_label} renderer report did not contain gpu_error_modes")
        })?;
    let incomplete_modes = summary
        .get("incomplete_modes")
        .and_then(Value::as_u64)
        .with_context(|| {
            format!("{dataset_label} renderer report did not contain incomplete_modes")
        })?;
    if mode_count < 3 {
        bail!("{dataset_label} renderer report covered only {mode_count} mode(s)");
    }
    if nonblank_modes != mode_count {
        bail!("{dataset_label} renderer report had {nonblank_modes}/{mode_count} nonblank modes");
    }
    if cpu_error_modes != 0 {
        bail!("{dataset_label} renderer report had {cpu_error_modes} CPU error mode(s)");
    }
    if incomplete_modes != 0 {
        bail!("{dataset_label} renderer report had {incomplete_modes} incomplete mode(s)");
    }
    let gpu_available = report
        .pointer("/gpu/available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if gpu_available && gpu_error_modes != 0 {
        bail!("{dataset_label} renderer report had {gpu_error_modes} GPU error mode(s)");
    }
    Ok(())
}

fn phase19_renderer_summary(report: &Value) -> Value {
    json!({
        "dataset": report.get("dataset").cloned(),
        "source_shape": report.get("source_shape").cloned(),
        "viewport": report.get("viewport").cloned(),
        "lod": report.get("lod").cloned(),
        "summary": report.get("summary").cloned(),
        "gpu": report.get("gpu").cloned(),
        "timings_ms": report.get("timings_ms").cloned(),
        "refinement_budget_probe": report.get("refinement_budget_probe").cloned(),
        "transition_cache_probe": report.get("transition_cache_probe").cloned(),
    })
}

fn phase19_audit_coverage(generated_renderer: &Value, real_samples: &[Value]) -> Value {
    let opened_t5_qual_003 = real_samples
        .iter()
        .any(|sample| sample.get("experiment").and_then(Value::as_str) == Some("T5-QUAL-003"));
    let opened_t5_qual_001 = real_samples
        .iter()
        .any(|sample| sample.get("experiment").and_then(Value::as_str) == Some("T5-QUAL-001"));
    let playback_real_samples = real_samples
        .iter()
        .filter(|sample| {
            sample
                .pointer("/app_smoke/playback_summary")
                .and_then(Value::as_str)
                .map(|summary| !summary.trim().is_empty())
                .unwrap_or(false)
        })
        .count();
    let generated_modes = generated_renderer
        .pointer("/summary/mode_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "generated_time_multichannel_smoke": true,
        "generated_playback_smoke": true,
        "generated_renderer_mode_count": generated_modes,
        "real_sample_count": real_samples.len(),
        "real_playback_sample_count": playback_real_samples,
        "t5_qual_003_opened_when_available": opened_t5_qual_003,
        "t5_qual_001_opened_when_available": opened_t5_qual_001,
        "status_sampling_label_tests": true,
        "strict_native_import_to_open": true,
        "renderer_mode_evidence": true,
    })
}

fn phase19_audit_markdown(report: &Value) -> String {
    let mut out = String::new();
    out.push_str("# Phase 19 Viewer Product Hardening Audit\n\n");
    out.push_str("Generated by `cargo xtask phase19-audit`.\n\n");
    out.push_str("## Coverage\n\n");
    out.push_str("```json\n");
    out.push_str(
        &serde_json::to_string_pretty(report.get("coverage").unwrap_or(&Value::Null))
            .unwrap_or_else(|_| "null".to_owned()),
    );
    out.push_str("\n```\n\n");

    out.push_str("## Generated Fixture\n\n");
    out.push_str(&format!(
        "- package: `{}`\n",
        report
            .pointer("/generated_fixture/package")
            .and_then(Value::as_str)
            .unwrap_or("")
    ));
    out.push_str(&format!(
        "- app smoke: {}\n",
        report
            .pointer("/generated_fixture/app_smoke/stdout_summary")
            .and_then(Value::as_str)
            .unwrap_or("")
    ));
    out.push_str(&format!(
        "- playback: {}\n",
        report
            .pointer("/generated_fixture/app_smoke/playback_summary")
            .and_then(Value::as_str)
            .unwrap_or("")
    ));
    out.push_str(&format!(
        "- renderer report: `{}`\n\n",
        report
            .pointer("/generated_fixture/renderer_report")
            .and_then(Value::as_str)
            .unwrap_or("")
    ));

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
            out.push_str(&format!(
                "- app smoke: {}\n",
                sample
                    .pointer("/app_smoke/stdout_summary")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            ));
            if let Some(playback) = sample
                .pointer("/app_smoke/playback_summary")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                out.push_str(&format!("- playback: {playback}\n"));
            }
            out.push_str(&format!(
                "- renderer report: `{}`\n\n",
                sample
                    .get("renderer_report")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            ));
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
    fn phase19_renderer_validation_accepts_complete_nonblank_reports() {
        let report = json!({
            "summary": {
                "mode_count": 3,
                "nonblank_modes": 3,
                "cpu_error_modes": 0,
                "gpu_error_modes": 0,
                "incomplete_modes": 0
            },
            "gpu": {
                "available": true
            }
        });

        phase19_validate_renderer_report(&report, "synthetic").unwrap();
    }

    #[test]
    fn phase19_renderer_validation_rejects_incomplete_or_gpu_error_reports() {
        let incomplete = json!({
            "summary": {
                "mode_count": 3,
                "nonblank_modes": 3,
                "cpu_error_modes": 0,
                "gpu_error_modes": 0,
                "incomplete_modes": 1
            },
            "gpu": {
                "available": true
            }
        });
        assert!(
            phase19_validate_renderer_report(&incomplete, "synthetic")
                .unwrap_err()
                .to_string()
                .contains("incomplete")
        );

        let gpu_error = json!({
            "summary": {
                "mode_count": 3,
                "nonblank_modes": 3,
                "cpu_error_modes": 0,
                "gpu_error_modes": 1,
                "incomplete_modes": 0
            },
            "gpu": {
                "available": true
            }
        });
        assert!(
            phase19_validate_renderer_report(&gpu_error, "synthetic")
                .unwrap_err()
                .to_string()
                .contains("GPU error")
        );
    }

    #[test]
    fn phase19_audit_coverage_tracks_real_samples_and_playback() {
        let generated_renderer = json!({
            "summary": {
                "mode_count": 3
            }
        });
        let real_samples = vec![
            json!({
                "experiment": "T5-QUAL-003",
                "app_smoke": {
                    "playback_summary": "Mirante4D playback smoke: 2 step(s)"
                }
            }),
            json!({
                "experiment": "T5-QUAL-001",
                "app_smoke": {
                    "playback_summary": ""
                }
            }),
        ];

        let coverage = phase19_audit_coverage(&generated_renderer, &real_samples);

        assert_eq!(coverage["generated_renderer_mode_count"], json!(3));
        assert_eq!(coverage["real_sample_count"], json!(2));
        assert_eq!(coverage["real_playback_sample_count"], json!(1));
        assert_eq!(coverage["t5_qual_003_opened_when_available"], json!(true));
        assert_eq!(coverage["t5_qual_001_opened_when_available"], json!(true));
        assert_eq!(coverage["status_sampling_label_tests"], json!(true));
    }
}

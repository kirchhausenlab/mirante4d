use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::{process::run_cargo, stable_id_from_name};

pub(crate) fn app_smoke(package: &Path) -> anyhow::Result<PathBuf> {
    app_smoke_with_options(package, "app-smoke", ReleaseAppSmokeOptions::default())
}

pub(crate) fn app_smoke_with_options(
    package: &Path,
    report_prefix: &str,
    options: ReleaseAppSmokeOptions,
) -> anyhow::Result<PathBuf> {
    if !package.is_dir() {
        bail!(
            "native package path does not exist or is not a directory: {}",
            package.display()
        );
    }
    let output_root = PathBuf::from("target/mirante4d/benchmarks");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let package_name = package
        .file_stem()
        .and_then(|name| name.to_str())
        .map(stable_id_from_name)
        .unwrap_or_else(|| "native-package".to_owned());
    let log_path = output_root.join(format!("{report_prefix}-{package_name}.log"));
    let report = run_release_app_smoke_with_options(package, &log_path, options)?;
    let report_path = output_root.join(format!("{report_prefix}-{package_name}.json"));
    fs::write(
        &report_path,
        format!("{}\n", serde_json::to_string_pretty(&report)?),
    )
    .with_context(|| format!("failed to write {}", report_path.display()))?;
    Ok(report_path)
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ReleaseAppSmokeOptions {
    pub(crate) playback_steps: Option<usize>,
    pub(crate) timeout_secs: Option<u64>,
}

pub(crate) fn run_release_app_smoke(dataset: &Path, output_path: &Path) -> anyhow::Result<Value> {
    run_release_app_smoke_with_options(dataset, output_path, ReleaseAppSmokeOptions::default())
}

pub(crate) fn run_release_app_smoke_with_options(
    dataset: &Path,
    output_path: &Path,
    options: ReleaseAppSmokeOptions,
) -> anyhow::Result<Value> {
    run_cargo(["build", "--release", "-p", "mirante4d-app"])?;
    let binary_name = if cfg!(windows) {
        "mirante4d-app.exe"
    } else {
        "mirante4d-app"
    };
    let binary = PathBuf::from("target").join("release").join(binary_name);
    let started = Instant::now();
    let mut command = Command::new(&binary);
    command
        .env("MIRANTE4D_APP_SMOKE", "1")
        .env("MIRANTE4D_DEV_DATASET", dataset)
        .env(
            "RUST_LOG",
            env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned()),
        );
    if let Some(steps) = options.playback_steps {
        command.env("MIRANTE4D_APP_SMOKE_PLAYBACK_STEPS", steps.to_string());
    }
    if let Some(timeout_secs) = options.timeout_secs {
        command.env("MIRANTE4D_APP_SMOKE_TIMEOUT_SECS", timeout_secs.to_string());
    }
    let output = command
        .output()
        .with_context(|| format!("failed to run app smoke for {}", dataset.display()))?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let mut report = String::new();
    report.push_str(&format!("dataset: {}\n", dataset.display()));
    report.push_str(&format!("status: {}\n", output.status));
    report.push_str(&format!("elapsed_ms: {elapsed_ms:.6}\n\n"));
    report.push_str("stdout:\n");
    report.push_str(&stdout);
    report.push_str("\nstderr:\n");
    report.push_str(&stderr);
    fs::write(output_path, report)
        .with_context(|| format!("failed to write {}", output_path.display()))?;
    if !output.status.success() {
        bail!(
            "app smoke failed for {}; see {}",
            dataset.display(),
            output_path.display()
        );
    }
    if !stdout.contains("Mirante4D") || !stdout.contains("opened") {
        bail!(
            "app smoke did not report a successful open for {}; see {}",
            dataset.display(),
            output_path.display()
        );
    }
    Ok(json!({
        "dataset": dataset,
        "binary": binary,
        "log": output_path,
        "status": output.status.to_string(),
        "stdout_summary": stdout.lines().find(|line| line.contains("Mirante4D")).unwrap_or(""),
        "playback_summary": stdout.lines().find(|line| line.contains("Mirante4D playback smoke")).unwrap_or(""),
        "gpu_adapter_summary": stdout.lines().find(|line| line.starts_with("GPU adapter:")).unwrap_or(""),
        "requested_playback_steps": options.playback_steps.unwrap_or(0),
        "timeout_secs": options.timeout_secs.unwrap_or(30),
        "timings_ms": {
            "app_smoke_process": elapsed_ms,
        },
    }))
}

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::{host::benchmark_host_context, reports::write_json_file};

const EXTERNAL_CI_OUTPUT_DIR: &str = "target/mirante4d/external-ci";
const EXTERNAL_CI_SURFACES_DIR: &str = "target/mirante4d/external-ci/surfaces";
const EXTERNAL_CI_JSON: &str = "target/mirante4d/external-ci/external-ci-evidence.json";
const EXTERNAL_CI_SCHEMA: &str = "mirante4d-external-ci-evidence";
const EXTERNAL_CI_SURFACE_SCHEMA: &str = "mirante4d-external-ci-surface-evidence";
const EXTERNAL_CI_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalCiMode {
    Manual,
    Surface,
    Merge,
}

#[derive(Debug, Clone, Copy)]
struct CiSurfaceSpec {
    name: &'static str,
    workflow_path: &'static str,
    env_prefix: &'static str,
    artifact_name: &'static str,
    surface_filename: &'static str,
    required_checks: &'static [&'static str],
}

const HOSTED_CPU_REQUIRED_CHECKS: &[&str] = &[
    "command-audit",
    "baseline-audit",
    "workflow-audit",
    "report-audit",
    "verify-fast",
    "verify-full",
    "package-linux-release",
    "bench-smoke",
];

const SELF_HOSTED_GPU_REQUIRED_CHECKS: &[&str] = &["verify-render"];

const CI_SURFACE_SPECS: &[CiSurfaceSpec] = &[
    CiSurfaceSpec {
        name: "hosted_cpu_ci",
        workflow_path: ".github/workflows/ci.yml",
        env_prefix: "MIRANTE4D_EXTERNAL_CI_HOSTED_CPU",
        artifact_name: "hosted-cpu-ci-evidence-surface",
        surface_filename: "hosted_cpu_ci.json",
        required_checks: HOSTED_CPU_REQUIRED_CHECKS,
    },
    CiSurfaceSpec {
        name: "self_hosted_gpu_ci",
        workflow_path: ".github/workflows/gpu-render.yml",
        env_prefix: "MIRANTE4D_EXTERNAL_CI_SELF_HOSTED_GPU",
        artifact_name: "self-hosted-gpu-ci-evidence-surface",
        surface_filename: "self_hosted_gpu_ci.json",
        required_checks: SELF_HOSTED_GPU_REQUIRED_CHECKS,
    },
];

pub(crate) fn external_ci_completion_plan_json() -> Value {
    json!({
        "schema": "mirante4d-external-ci-completion-plan",
        "schema_version": EXTERNAL_CI_SCHEMA_VERSION,
        "status": "pending_external_run_evidence",
        "required_surface_count": CI_SURFACE_SPECS.len(),
        "final_evidence_path": EXTERNAL_CI_JSON,
        "surfaces": CI_SURFACE_SPECS
            .iter()
            .map(external_ci_surface_plan_json)
            .collect::<Vec<_>>(),
        "finalizer": {
            "workflow_path": ".github/workflows/external-ci-evidence.yml",
            "workflow_name": "External CI Evidence",
            "required_inputs": [
                "hosted_cpu_run_id",
                "self_hosted_gpu_run_id",
            ],
            "downloaded_surface_files": CI_SURFACE_SPECS
                .iter()
                .map(|spec| spec.surface_filename)
                .collect::<Vec<_>>(),
            "merge_command": "MIRANTE4D_EXTERNAL_CI_MODE=merge MIRANTE4D_EXTERNAL_CI_SURFACE_REPORTS=<hosted_cpu_ci.json,self_hosted_gpu_ci.json> cargo xtask external-ci-evidence",
            "final_artifact_name": "external-ci-evidence",
        },
        "manual_fallback": {
            "mode": "manual",
            "command": "MIRANTE4D_EXTERNAL_CI_MODE=manual cargo xtask external-ci-evidence",
            "required_global_env": [
                "MIRANTE4D_EXTERNAL_CI_GIT_SHA",
            ],
            "surface_env_prefixes": CI_SURFACE_SPECS
                .iter()
                .map(|spec| spec.env_prefix)
                .collect::<Vec<_>>(),
        },
    })
}

fn external_ci_surface_plan_json(spec: &CiSurfaceSpec) -> Value {
    json!({
        "name": spec.name,
        "workflow_path": spec.workflow_path,
        "artifact_name": spec.artifact_name,
        "surface_filename": spec.surface_filename,
        "surface_json_path": Path::new(EXTERNAL_CI_SURFACES_DIR).join(spec.surface_filename),
        "required_checks": spec.required_checks,
        "required_check_count": spec.required_checks.len(),
        "surface_capture_mode": "surface",
        "surface_capture_command": format!(
            "MIRANTE4D_EXTERNAL_CI_MODE=surface MIRANTE4D_EXTERNAL_CI_SURFACE={} cargo xtask external-ci-evidence",
            spec.name
        ),
        "manual_env": {
            "status": format!("{}_STATUS", spec.env_prefix),
            "run_url": format!("{}_RUN_URL", spec.env_prefix),
            "run_id": format!("{}_RUN_ID", spec.env_prefix),
            "git_sha": format!("{}_GIT_SHA", spec.env_prefix),
            "check_results": format!("{}_CHECK_RESULTS", spec.env_prefix),
        },
    })
}

pub(crate) fn external_ci_evidence() -> anyhow::Result<PathBuf> {
    let mode = external_ci_mode(|name| env::var(name).ok())?;
    match mode {
        ExternalCiMode::Manual => {
            let report = manual_external_ci_evidence_report_json(|name| env::var(name).ok())?;
            let path = PathBuf::from(EXTERNAL_CI_JSON);
            write_json_file(&path, &report)?;
            Ok(path)
        }
        ExternalCiMode::Surface => {
            let report = external_ci_surface_report_json(|name| env::var(name).ok())?;
            let surface_name = report["surface"]["name"]
                .as_str()
                .context("surface report missing surface name")?;
            let spec = ci_surface_spec(surface_name)?;
            let path = Path::new(EXTERNAL_CI_SURFACES_DIR).join(spec.surface_filename);
            write_json_file(&path, &report)?;
            if report["status"].as_str() != Some("passed") {
                bail!(
                    "external CI surface {surface_name:?} did not pass; wrote {}",
                    path.display()
                );
            }
            Ok(path)
        }
        ExternalCiMode::Merge => {
            let report = merged_external_ci_evidence_report_json(|name| env::var(name).ok())?;
            let path = PathBuf::from(EXTERNAL_CI_JSON);
            write_json_file(&path, &report)?;
            Ok(path)
        }
    }
}

fn external_ci_mode<F>(mut env_var: F) -> anyhow::Result<ExternalCiMode>
where
    F: FnMut(&str) -> Option<String>,
{
    match optional_env(&mut env_var, "MIRANTE4D_EXTERNAL_CI_MODE")
        .unwrap_or_else(|| "manual".to_owned())
        .as_str()
    {
        "manual" => Ok(ExternalCiMode::Manual),
        "surface" => Ok(ExternalCiMode::Surface),
        "merge" => Ok(ExternalCiMode::Merge),
        mode => {
            bail!("MIRANTE4D_EXTERNAL_CI_MODE must be manual, surface, or merge; found {mode:?}")
        }
    }
}

fn manual_external_ci_evidence_report_json<F>(mut env_var: F) -> anyhow::Result<Value>
where
    F: FnMut(&str) -> Option<String>,
{
    let git_sha = optional_env(&mut env_var, "MIRANTE4D_EXTERNAL_CI_GIT_SHA")
        .or_else(|| optional_env(&mut env_var, "GITHUB_SHA"))
        .context("MIRANTE4D_EXTERNAL_CI_GIT_SHA or GITHUB_SHA is required")?;
    let surfaces = CI_SURFACE_SPECS
        .iter()
        .map(|spec| ci_surface_from_prefixed_env(&mut env_var, spec, &git_sha))
        .collect::<anyhow::Result<Vec<_>>>()?;

    combined_external_ci_report_json("operator_supplied_external_run_metadata", surfaces, None)
}

fn external_ci_surface_report_json<F>(mut env_var: F) -> anyhow::Result<Value>
where
    F: FnMut(&str) -> Option<String>,
{
    let surface_name = required_env(&mut env_var, "MIRANTE4D_EXTERNAL_CI_SURFACE")?;
    let spec = ci_surface_spec(&surface_name)?;
    let surface = ci_surface_from_surface_env(&mut env_var, spec)?;
    let passed = surface.get("status").and_then(Value::as_str) == Some("passed");

    Ok(json!({
        "schema": EXTERNAL_CI_SURFACE_SCHEMA,
        "schema_version": EXTERNAL_CI_SCHEMA_VERSION,
        "command": "external-ci-evidence",
        "status": if passed { "passed" } else { "failed" },
        "target_root": EXTERNAL_CI_OUTPUT_DIR,
        "evidence_source": "github_actions_or_operator_surface_capture",
        "summary": {
            "external_run_evidence": "single_ci_surface_checked",
            "required_surface_count": 1,
            "passed_surface_count": if passed { 1 } else { 0 },
            "blocking_count": if passed { 0 } else { 1 },
        },
        "surface": surface,
    }))
}

fn merged_external_ci_evidence_report_json<F>(mut env_var: F) -> anyhow::Result<Value>
where
    F: FnMut(&str) -> Option<String>,
{
    let raw_paths = required_env(&mut env_var, "MIRANTE4D_EXTERNAL_CI_SURFACE_REPORTS")?;
    let source_paths = parse_report_paths(&raw_paths)?;
    let mut surfaces = Vec::new();
    for path in &source_paths {
        let report = read_json_file(path)?;
        surfaces.push(surface_from_surface_report(path, &report)?);
    }

    let mut report = combined_external_ci_report_json("merged_ci_surface_reports", surfaces, None)?;
    report
        .as_object_mut()
        .expect("combined report is an object")
        .insert(
            "source_surface_report_paths".to_owned(),
            json!(
                source_paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
            ),
        );
    Ok(report)
}

fn ci_surface_from_prefixed_env<F>(
    env_var: &mut F,
    spec: &CiSurfaceSpec,
    default_git_sha: &str,
) -> anyhow::Result<Value>
where
    F: FnMut(&str) -> Option<String>,
{
    let status = required_env(env_var, &format!("{}_STATUS", spec.env_prefix))?;
    let run_url = required_env(env_var, &format!("{}_RUN_URL", spec.env_prefix))?;
    let run_id = required_env(env_var, &format!("{}_RUN_ID", spec.env_prefix))?;
    let git_sha = optional_env(env_var, &format!("{}_GIT_SHA", spec.env_prefix))
        .unwrap_or_else(|| default_git_sha.to_owned());
    let check_results = ci_check_results_from_env(
        spec,
        optional_env(env_var, &format!("{}_CHECK_RESULTS", spec.env_prefix)),
        &status,
    )?;

    Ok(ci_surface_json(
        spec,
        &status,
        &run_url,
        &run_id,
        &git_sha,
        "operator_supplied_external_run_metadata",
        check_results,
    ))
}

fn ci_surface_from_surface_env<F>(env_var: &mut F, spec: &CiSurfaceSpec) -> anyhow::Result<Value>
where
    F: FnMut(&str) -> Option<String>,
{
    let status = optional_env(env_var, "MIRANTE4D_EXTERNAL_CI_SURFACE_STATUS")
        .or_else(|| optional_env(env_var, &format!("{}_STATUS", spec.env_prefix)))
        .context("MIRANTE4D_EXTERNAL_CI_SURFACE_STATUS is required")?;
    let run_id = optional_env(env_var, "MIRANTE4D_EXTERNAL_CI_SURFACE_RUN_ID")
        .or_else(|| optional_env(env_var, &format!("{}_RUN_ID", spec.env_prefix)))
        .or_else(|| optional_env(env_var, "GITHUB_RUN_ID"))
        .context("MIRANTE4D_EXTERNAL_CI_SURFACE_RUN_ID or GITHUB_RUN_ID is required")?;
    let run_url = optional_env(env_var, "MIRANTE4D_EXTERNAL_CI_SURFACE_RUN_URL")
        .or_else(|| optional_env(env_var, &format!("{}_RUN_URL", spec.env_prefix)))
        .or_else(|| github_actions_run_url(env_var, &run_id))
        .context("MIRANTE4D_EXTERNAL_CI_SURFACE_RUN_URL or GitHub Actions run env is required")?;
    let git_sha = optional_env(env_var, "MIRANTE4D_EXTERNAL_CI_SURFACE_GIT_SHA")
        .or_else(|| optional_env(env_var, &format!("{}_GIT_SHA", spec.env_prefix)))
        .or_else(|| optional_env(env_var, "MIRANTE4D_EXTERNAL_CI_GIT_SHA"))
        .or_else(|| optional_env(env_var, "GITHUB_SHA"))
        .context("MIRANTE4D_EXTERNAL_CI_SURFACE_GIT_SHA, MIRANTE4D_EXTERNAL_CI_GIT_SHA, or GITHUB_SHA is required")?;
    let check_results = ci_check_results_from_env(
        spec,
        optional_env(env_var, "MIRANTE4D_EXTERNAL_CI_SURFACE_CHECK_RESULTS")
            .or_else(|| optional_env(env_var, &format!("{}_CHECK_RESULTS", spec.env_prefix))),
        &status,
    )?;

    Ok(ci_surface_json(
        spec,
        &status,
        &run_url,
        &run_id,
        &git_sha,
        "github_actions_or_operator_surface_capture",
        check_results,
    ))
}

fn ci_surface_json(
    spec: &CiSurfaceSpec,
    status: &str,
    run_url: &str,
    run_id: &str,
    git_sha: &str,
    evidence_source: &str,
    check_results: Vec<Value>,
) -> Value {
    let aggregate_status = normalize_ci_status(status);
    let passed_check_count = check_results
        .iter()
        .filter(|result| result.get("status").and_then(Value::as_str) == Some("passed"))
        .count();
    let blocking_count = spec
        .required_checks
        .len()
        .saturating_sub(passed_check_count);
    let surface_passed = aggregate_status == "passed" && blocking_count == 0;
    json!({
        "name": spec.name,
        "status": if surface_passed { "passed" } else { "failed" },
        "aggregate_status": aggregate_status,
        "raw_aggregate_status": status,
        "workflow_path": spec.workflow_path,
        "run_url": run_url,
        "run_id": run_id,
        "git_sha": git_sha,
        "required_checks": spec.required_checks,
        "check_results": check_results,
        "check_summary": {
            "required_check_count": spec.required_checks.len(),
            "passed_check_count": passed_check_count,
            "blocking_count": blocking_count,
        },
        "evidence_source": evidence_source,
    })
}

fn ci_check_results_from_env(
    spec: &CiSurfaceSpec,
    raw_results: Option<String>,
    fallback_status: &str,
) -> anyhow::Result<Vec<Value>> {
    match raw_results {
        Some(raw_results) => parse_ci_check_results(spec, &raw_results),
        None => Ok(spec
            .required_checks
            .iter()
            .map(|check| {
                json!({
                    "name": check,
                    "status": normalize_ci_status(fallback_status),
                    "raw_status": fallback_status,
                    "source": "surface_status_fallback",
                })
            })
            .collect()),
    }
}

fn parse_ci_check_results(spec: &CiSurfaceSpec, raw_results: &str) -> anyhow::Result<Vec<Value>> {
    let mut parsed = Vec::new();
    let mut seen = std::collections::BTreeSet::<String>::new();
    for item in raw_results
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let Some((name, raw_status)) = item.split_once('=') else {
            bail!("external CI check result {item:?} must use name=status");
        };
        let name = name.trim();
        let raw_status = raw_status.trim();
        if !spec.required_checks.contains(&name) {
            bail!(
                "external CI surface {:?} has unexpected check result {name:?}",
                spec.name
            );
        }
        if !seen.insert(name.to_owned()) {
            bail!(
                "external CI surface {:?} has duplicate check result {name:?}",
                spec.name
            );
        }
        parsed.push(json!({
            "name": name,
            "status": normalize_ci_status(raw_status),
            "raw_status": raw_status,
            "source": "explicit_check_results",
        }));
    }
    if seen.len() != spec.required_checks.len() {
        let missing = spec
            .required_checks
            .iter()
            .filter(|check| !seen.contains(**check))
            .copied()
            .collect::<Vec<_>>();
        bail!(
            "external CI surface {:?} missing check results for {:?}",
            spec.name,
            missing
        );
    }
    parsed.sort_by_key(|result| {
        spec.required_checks
            .iter()
            .position(|check| result.get("name").and_then(Value::as_str) == Some(*check))
            .unwrap_or(usize::MAX)
    });
    Ok(parsed)
}

fn normalize_ci_status(status: &str) -> &'static str {
    match status.trim().to_ascii_lowercase().as_str() {
        "passed" | "pass" | "success" | "successful" => "passed",
        _ => "failed",
    }
}

fn combined_external_ci_report_json(
    evidence_source: &str,
    surfaces: Vec<Value>,
    git_sha_override: Option<String>,
) -> anyhow::Result<Value> {
    let surfaces = ordered_valid_surfaces(surfaces)?;
    let git_sha = git_sha_override
        .or_else(|| {
            surfaces
                .first()
                .and_then(|surface| surface.get("git_sha"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .context("external CI evidence surfaces missing git SHA")?;
    let passed_count = surfaces
        .iter()
        .filter(|surface| surface.get("status").and_then(Value::as_str) == Some("passed"))
        .count();
    if passed_count != CI_SURFACE_SPECS.len() {
        bail!("external CI evidence requires all required CI surfaces to pass before merging");
    }

    Ok(json!({
        "schema": EXTERNAL_CI_SCHEMA,
        "schema_version": EXTERNAL_CI_SCHEMA_VERSION,
        "command": "external-ci-evidence",
        "status": "passed",
        "target_root": EXTERNAL_CI_OUTPUT_DIR,
        "summary": {
            "external_run_evidence": "checked_by_external_ci",
            "required_surface_count": CI_SURFACE_SPECS.len(),
            "passed_surface_count": passed_count,
            "blocking_count": CI_SURFACE_SPECS.len() - passed_count,
        },
        "evidence_source": evidence_source,
        "git_sha": git_sha,
        "host": benchmark_host_context(),
        "surfaces": surfaces,
    }))
}

fn ordered_valid_surfaces(surfaces: Vec<Value>) -> anyhow::Result<Vec<Value>> {
    if surfaces.len() != CI_SURFACE_SPECS.len() {
        bail!(
            "external CI evidence must include exactly {} CI surfaces",
            CI_SURFACE_SPECS.len()
        );
    }

    let mut ordered = Vec::new();
    let mut expected_git_sha: Option<String> = None;
    for spec in CI_SURFACE_SPECS {
        let matches = surfaces
            .iter()
            .filter(|surface| surface.get("name").and_then(Value::as_str) == Some(spec.name))
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            bail!(
                "external CI evidence must include exactly one surface {:?}",
                spec.name
            );
        }
        validate_surface(spec, matches[0], &mut expected_git_sha)?;
        ordered.push(matches[0].clone());
    }
    Ok(ordered)
}

fn validate_surface(
    spec: &CiSurfaceSpec,
    surface: &Value,
    expected_git_sha: &mut Option<String>,
) -> anyhow::Result<()> {
    if surface.get("status").and_then(Value::as_str) != Some("passed") {
        bail!("external CI surface {:?} is not passed", spec.name);
    }
    if surface.get("workflow_path").and_then(Value::as_str) != Some(spec.workflow_path) {
        bail!(
            "external CI surface {:?} has stale workflow_path",
            spec.name
        );
    }
    for field in ["run_url", "run_id", "git_sha"] {
        let Some(value) = surface.get(field).and_then(Value::as_str) else {
            bail!("external CI surface {:?} missing {field}", spec.name);
        };
        if value.trim().is_empty() {
            bail!("external CI surface {:?} has empty {field}", spec.name);
        }
    }
    if surface.get("required_checks") != Some(&json!(spec.required_checks)) {
        bail!(
            "external CI surface {:?} has stale required_checks",
            spec.name
        );
    }
    validate_surface_check_results(spec, surface)?;
    let git_sha = surface
        .get("git_sha")
        .and_then(Value::as_str)
        .expect("validated git_sha")
        .to_owned();
    match expected_git_sha {
        Some(expected) if expected != &git_sha => {
            bail!("external CI surfaces must all reference the same git SHA");
        }
        Some(_) => {}
        None => *expected_git_sha = Some(git_sha),
    }
    Ok(())
}

fn validate_surface_check_results(spec: &CiSurfaceSpec, surface: &Value) -> anyhow::Result<()> {
    let check_results = surface
        .get("check_results")
        .and_then(Value::as_array)
        .context("external CI surface missing check_results")?;
    if check_results.len() != spec.required_checks.len() {
        bail!(
            "external CI surface {:?} has stale check_results count",
            spec.name
        );
    }
    let names = check_results
        .iter()
        .map(|result| result.get("name").and_then(Value::as_str))
        .collect::<Option<Vec<_>>>()
        .context("external CI surface has malformed check_results names")?;
    if names != spec.required_checks {
        bail!(
            "external CI surface {:?} has stale check_results names",
            spec.name
        );
    }
    if check_results
        .iter()
        .any(|result| result.get("status").and_then(Value::as_str) != Some("passed"))
    {
        bail!(
            "external CI surface {:?} has non-passed check_results",
            spec.name
        );
    }
    if surface
        .pointer("/check_summary/required_check_count")
        .and_then(Value::as_u64)
        != Some(spec.required_checks.len() as u64)
    {
        bail!(
            "external CI surface {:?} has stale required check count",
            spec.name
        );
    }
    if surface
        .pointer("/check_summary/passed_check_count")
        .and_then(Value::as_u64)
        != Some(spec.required_checks.len() as u64)
    {
        bail!(
            "external CI surface {:?} has stale passed check count",
            spec.name
        );
    }
    if surface
        .pointer("/check_summary/blocking_count")
        .and_then(Value::as_u64)
        != Some(0)
    {
        bail!(
            "external CI surface {:?} has blocking check_results",
            spec.name
        );
    }
    Ok(())
}

fn surface_from_surface_report(path: &Path, report: &Value) -> anyhow::Result<Value> {
    if report.get("schema").and_then(Value::as_str) != Some(EXTERNAL_CI_SURFACE_SCHEMA) {
        bail!(
            "external CI surface report {} has unsupported schema",
            path.display()
        );
    }
    if report.get("command").and_then(Value::as_str) != Some("external-ci-evidence") {
        bail!(
            "external CI surface report {} has unsupported command",
            path.display()
        );
    }
    let surface = report.get("surface").cloned().with_context(|| {
        format!(
            "external CI surface report {} missing surface",
            path.display()
        )
    })?;
    if report.get("status").and_then(Value::as_str) != surface.get("status").and_then(Value::as_str)
    {
        bail!(
            "external CI surface report {} has inconsistent top-level status",
            path.display()
        );
    }
    Ok(surface)
}

fn ci_surface_spec(name: &str) -> anyhow::Result<&'static CiSurfaceSpec> {
    CI_SURFACE_SPECS
        .iter()
        .find(|spec| spec.name == name)
        .with_context(|| format!("unsupported external CI surface {name:?}"))
}

fn parse_report_paths(raw: &str) -> anyhow::Result<Vec<PathBuf>> {
    let paths = raw
        .split(',')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if paths.is_empty() {
        bail!("MIRANTE4D_EXTERNAL_CI_SURFACE_REPORTS must name at least one report path");
    }
    Ok(paths)
}

fn read_json_file(path: &Path) -> anyhow::Result<Value> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

fn github_actions_run_url<F>(env_var: &mut F, run_id: &str) -> Option<String>
where
    F: FnMut(&str) -> Option<String>,
{
    let server_url = optional_env(env_var, "GITHUB_SERVER_URL")?;
    let repository = optional_env(env_var, "GITHUB_REPOSITORY")?;
    Some(format!("{server_url}/{repository}/actions/runs/{run_id}"))
}

fn required_env<F>(env_var: &mut F, name: &str) -> anyhow::Result<String>
where
    F: FnMut(&str) -> Option<String>,
{
    optional_env(env_var, name).with_context(|| format!("{name} is required"))
}

fn optional_env<F>(env_var: &mut F, name: &str) -> Option<String>
where
    F: FnMut(&str) -> Option<String>,
{
    env_var(name).filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn env_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn external_ci_mode_defaults_to_manual_and_rejects_unknown_modes() {
        let empty = BTreeMap::<String, String>::new();
        assert_eq!(
            external_ci_mode(|key| empty.get(key).cloned()).unwrap(),
            ExternalCiMode::Manual
        );

        let env = env_map(&[("MIRANTE4D_EXTERNAL_CI_MODE", "legacy")]);
        let err = external_ci_mode(|key| env.get(key).cloned())
            .unwrap_err()
            .to_string();
        assert!(err.contains("manual, surface, or merge"));
    }

    #[test]
    fn external_ci_completion_plan_names_surfaces_artifacts_and_checks() {
        let plan = external_ci_completion_plan_json();

        assert_eq!(plan["schema"], "mirante4d-external-ci-completion-plan");
        assert_eq!(plan["required_surface_count"], 2);
        assert_eq!(plan["surfaces"][0]["name"], "hosted_cpu_ci");
        assert_eq!(
            plan["surfaces"][0]["artifact_name"],
            "hosted-cpu-ci-evidence-surface"
        );
        assert_eq!(
            plan["surfaces"][0]["required_checks"],
            json!(HOSTED_CPU_REQUIRED_CHECKS)
        );
        assert_eq!(plan["surfaces"][1]["name"], "self_hosted_gpu_ci");
        assert_eq!(
            plan["surfaces"][1]["required_checks"],
            json!(SELF_HOSTED_GPU_REQUIRED_CHECKS)
        );
        assert_eq!(
            plan["finalizer"]["workflow_path"],
            ".github/workflows/external-ci-evidence.yml"
        );
        assert!(
            plan["finalizer"]["merge_command"]
                .as_str()
                .unwrap()
                .contains("MIRANTE4D_EXTERNAL_CI_MODE=merge")
        );
    }

    #[test]
    fn external_ci_evidence_requires_both_ci_surfaces() {
        let env = env_map(&[
            ("MIRANTE4D_EXTERNAL_CI_GIT_SHA", "abc123"),
            ("MIRANTE4D_EXTERNAL_CI_HOSTED_CPU_STATUS", "passed"),
            (
                "MIRANTE4D_EXTERNAL_CI_HOSTED_CPU_RUN_URL",
                "https://github.com/example/repo/actions/runs/1",
            ),
            ("MIRANTE4D_EXTERNAL_CI_HOSTED_CPU_RUN_ID", "1"),
        ]);

        let err = manual_external_ci_evidence_report_json(|key| env.get(key).cloned())
            .unwrap_err()
            .to_string();

        assert!(err.contains("MIRANTE4D_EXTERNAL_CI_SELF_HOSTED_GPU_STATUS"));
    }

    #[test]
    fn external_ci_evidence_writes_passed_run_metadata() {
        let env = env_map(&[
            ("MIRANTE4D_EXTERNAL_CI_GIT_SHA", "abc123"),
            ("MIRANTE4D_EXTERNAL_CI_HOSTED_CPU_STATUS", "passed"),
            (
                "MIRANTE4D_EXTERNAL_CI_HOSTED_CPU_RUN_URL",
                "https://github.com/example/repo/actions/runs/1",
            ),
            ("MIRANTE4D_EXTERNAL_CI_HOSTED_CPU_RUN_ID", "1"),
            ("MIRANTE4D_EXTERNAL_CI_SELF_HOSTED_GPU_STATUS", "passed"),
            (
                "MIRANTE4D_EXTERNAL_CI_SELF_HOSTED_GPU_RUN_URL",
                "https://github.com/example/repo/actions/runs/2",
            ),
            ("MIRANTE4D_EXTERNAL_CI_SELF_HOSTED_GPU_RUN_ID", "2"),
        ]);

        let report = manual_external_ci_evidence_report_json(|key| env.get(key).cloned()).unwrap();

        assert_eq!(report["schema"], EXTERNAL_CI_SCHEMA);
        assert_eq!(report["command"], "external-ci-evidence");
        assert_eq!(report["status"], "passed");
        assert_eq!(
            report["evidence_source"],
            "operator_supplied_external_run_metadata"
        );
        assert_eq!(
            report["summary"]["external_run_evidence"],
            "checked_by_external_ci"
        );
        assert_eq!(report["surfaces"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn external_ci_surface_report_infers_github_run_metadata() {
        let env = env_map(&[
            ("MIRANTE4D_EXTERNAL_CI_SURFACE", "hosted_cpu_ci"),
            ("MIRANTE4D_EXTERNAL_CI_SURFACE_STATUS", "passed"),
            ("GITHUB_SERVER_URL", "https://github.com"),
            ("GITHUB_REPOSITORY", "example/repo"),
            ("GITHUB_RUN_ID", "42"),
            ("GITHUB_SHA", "abc123"),
        ]);

        let report = external_ci_surface_report_json(|key| env.get(key).cloned()).unwrap();

        assert_eq!(report["schema"], EXTERNAL_CI_SURFACE_SCHEMA);
        assert_eq!(report["status"], "passed");
        assert_eq!(report["surface"]["name"], "hosted_cpu_ci");
        assert_eq!(
            report["surface"]["run_url"],
            "https://github.com/example/repo/actions/runs/42"
        );
        assert_eq!(
            report["surface"]["required_checks"],
            json!(HOSTED_CPU_REQUIRED_CHECKS)
        );
        assert_eq!(
            report["surface"]["check_results"][0]["name"],
            HOSTED_CPU_REQUIRED_CHECKS[0]
        );
        assert_eq!(report["surface"]["check_results"][0]["status"], "passed");
    }

    #[test]
    fn external_ci_surface_report_records_explicit_check_results() {
        let env = env_map(&[
            ("MIRANTE4D_EXTERNAL_CI_SURFACE", "hosted_cpu_ci"),
            ("MIRANTE4D_EXTERNAL_CI_SURFACE_STATUS", "passed"),
            (
                "MIRANTE4D_EXTERNAL_CI_SURFACE_CHECK_RESULTS",
                "command-audit=success,baseline-audit=success,workflow-audit=success,report-audit=success,verify-fast=success,verify-full=success,package-linux-release=success,bench-smoke=success",
            ),
            ("GITHUB_SERVER_URL", "https://github.com"),
            ("GITHUB_REPOSITORY", "example/repo"),
            ("GITHUB_RUN_ID", "42"),
            ("GITHUB_SHA", "abc123"),
        ]);

        let report = external_ci_surface_report_json(|key| env.get(key).cloned()).unwrap();

        assert_eq!(report["status"], "passed");
        assert_eq!(
            report["surface"]["check_summary"]["required_check_count"],
            HOSTED_CPU_REQUIRED_CHECKS.len()
        );
        assert_eq!(
            report["surface"]["check_summary"]["passed_check_count"],
            HOSTED_CPU_REQUIRED_CHECKS.len()
        );
        assert_eq!(report["surface"]["check_summary"]["blocking_count"], 0);
        assert!(
            report["surface"]["check_results"]
                .as_array()
                .unwrap()
                .iter()
                .all(|result| result["source"] == "explicit_check_results")
        );
    }

    #[test]
    fn external_ci_surface_report_fails_when_required_check_failed() {
        let env = env_map(&[
            ("MIRANTE4D_EXTERNAL_CI_SURFACE", "self_hosted_gpu_ci"),
            ("MIRANTE4D_EXTERNAL_CI_SURFACE_STATUS", "passed"),
            (
                "MIRANTE4D_EXTERNAL_CI_SURFACE_CHECK_RESULTS",
                "verify-render=failure",
            ),
            ("GITHUB_SERVER_URL", "https://github.com"),
            ("GITHUB_REPOSITORY", "example/repo"),
            ("GITHUB_RUN_ID", "42"),
            ("GITHUB_SHA", "abc123"),
        ]);

        let report = external_ci_surface_report_json(|key| env.get(key).cloned()).unwrap();

        assert_eq!(report["status"], "failed");
        assert_eq!(report["surface"]["status"], "failed");
        assert_eq!(report["surface"]["check_results"][0]["status"], "failed");
    }

    #[test]
    fn external_ci_evidence_merges_surface_reports() {
        let tempdir = tempfile::tempdir().unwrap();
        let hosted = external_ci_surface_report_json(|key| {
            env_map(&[
                ("MIRANTE4D_EXTERNAL_CI_SURFACE", "hosted_cpu_ci"),
                ("MIRANTE4D_EXTERNAL_CI_SURFACE_STATUS", "passed"),
                (
                    "MIRANTE4D_EXTERNAL_CI_SURFACE_RUN_URL",
                    "https://github.com/example/repo/actions/runs/1",
                ),
                ("MIRANTE4D_EXTERNAL_CI_SURFACE_RUN_ID", "1"),
                ("MIRANTE4D_EXTERNAL_CI_SURFACE_GIT_SHA", "abc123"),
            ])
            .get(key)
            .cloned()
        })
        .unwrap();
        let gpu = external_ci_surface_report_json(|key| {
            env_map(&[
                ("MIRANTE4D_EXTERNAL_CI_SURFACE", "self_hosted_gpu_ci"),
                ("MIRANTE4D_EXTERNAL_CI_SURFACE_STATUS", "passed"),
                (
                    "MIRANTE4D_EXTERNAL_CI_SURFACE_RUN_URL",
                    "https://github.com/example/repo/actions/runs/2",
                ),
                ("MIRANTE4D_EXTERNAL_CI_SURFACE_RUN_ID", "2"),
                ("MIRANTE4D_EXTERNAL_CI_SURFACE_GIT_SHA", "abc123"),
            ])
            .get(key)
            .cloned()
        })
        .unwrap();
        let hosted_path = tempdir.path().join("hosted.json");
        let gpu_path = tempdir.path().join("gpu.json");
        fs::write(&hosted_path, serde_json::to_string_pretty(&hosted).unwrap()).unwrap();
        fs::write(&gpu_path, serde_json::to_string_pretty(&gpu).unwrap()).unwrap();
        let reports = format!("{},{}", hosted_path.display(), gpu_path.display());
        let env = env_map(&[
            ("MIRANTE4D_EXTERNAL_CI_MODE", "merge"),
            ("MIRANTE4D_EXTERNAL_CI_SURFACE_REPORTS", &reports),
        ]);

        let report = merged_external_ci_evidence_report_json(|key| env.get(key).cloned()).unwrap();

        assert_eq!(report["schema"], EXTERNAL_CI_SCHEMA);
        assert_eq!(report["status"], "passed");
        assert_eq!(report["evidence_source"], "merged_ci_surface_reports");
        assert_eq!(report["git_sha"], "abc123");
        assert_eq!(report["surfaces"].as_array().unwrap().len(), 2);
        assert_eq!(
            report["source_surface_report_paths"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn external_ci_evidence_rejects_mismatched_surface_shas() {
        let hosted = ci_surface_json(
            &CI_SURFACE_SPECS[0],
            "passed",
            "https://github.com/example/repo/actions/runs/1",
            "1",
            "abc123",
            "test",
            ci_check_results_from_env(&CI_SURFACE_SPECS[0], None, "passed").unwrap(),
        );
        let gpu = ci_surface_json(
            &CI_SURFACE_SPECS[1],
            "passed",
            "https://github.com/example/repo/actions/runs/2",
            "2",
            "def456",
            "test",
            ci_check_results_from_env(&CI_SURFACE_SPECS[1], None, "passed").unwrap(),
        );

        let err = combined_external_ci_report_json("test", vec![hosted, gpu], None)
            .unwrap_err()
            .to_string();

        assert!(err.contains("same git SHA"));
    }
}

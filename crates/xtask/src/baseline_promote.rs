use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Component, Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::{
    baseline_audit::ACCEPTED_BASELINE_CLASSES,
    command_audit::COMMAND_AUDIT_ENTRIES,
    reports::{read_json_file, write_json_file},
};

const BASELINE_DIR: &str = "docs/benchmarks/baselines";
const BASELINE_PROMOTE_OUTPUT_DIR: &str = "target/mirante4d/baseline-promote";
const BASELINE_PROMOTE_JSON: &str =
    "target/mirante4d/baseline-promote/baseline-promote-report.json";
const BASELINE_PROMOTION_MANIFEST_SCHEMA: &str = "mirante4d-baseline-promotion-manifest";
const BASELINE_PROMOTION_REPORT_SCHEMA: &str = "mirante4d-baseline-promotion-report";
const BASELINE_PROMOTION_SCHEMA_VERSION: u32 = 1;
const BASELINE_REFRESH_PLAN_SCHEMA: &str = "mirante4d-baseline-refresh-plan";
const BASELINE_REFRESH_PLAN_OUTPUT_DIR: &str = "target/mirante4d/baseline-refresh";
const BASELINE_REFRESH_PLAN_JSON: &str =
    "target/mirante4d/baseline-refresh/baseline-refresh-plan.json";
const BASELINE_REFRESH_PROMOTION_MANIFEST_JSON: &str =
    "target/mirante4d/baseline-refresh/baseline-promotion-manifest.json";
const BASELINE_REFRESH_CLEAN_RERUN_SCRIPT: &str =
    "target/mirante4d/baseline-refresh/baseline-clean-reruns.sh";
const DEFAULT_BENCHMARK_REPORT_ROOT: &str = "target/mirante4d/benchmarks";
const STALE_TIMING_STATUS: &str = "stale_timing_needs_refresh";
const REPLACE_ENV: &str = "MIRANTE4D_BASELINE_PROMOTE_REPLACE";
const HEAVY_ENV: &str = "MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK";

pub(crate) fn baseline_promote(
    source_report: &Path,
    baseline_filename: &str,
) -> anyhow::Result<PathBuf> {
    ensure_clean_git_worktree()?;
    let source = read_json_file(source_report).with_context(|| {
        format!(
            "failed to read source benchmark report {}",
            source_report.display()
        )
    })?;
    let promoted = promoted_baseline_json(&source)?;
    if promoted.get("baseline_class").and_then(Value::as_str) == Some("private_local_heavy")
        && !env_flag(HEAVY_ENV)
    {
        bail!("promoting a private_local_heavy baseline requires {HEAVY_ENV}=1");
    }

    let destination = baseline_destination_path(baseline_filename)?;
    if destination.exists() && !env_flag(REPLACE_ENV) {
        bail!(
            "baseline {} already exists; set {REPLACE_ENV}=1 to replace it deliberately",
            destination.display()
        );
    }
    fs::create_dir_all(BASELINE_DIR).with_context(|| format!("failed to create {BASELINE_DIR}"))?;
    write_json_file(&destination, &promoted)?;
    Ok(destination)
}

pub(crate) fn baseline_promote_manifest(manifest_path: &Path) -> anyhow::Result<PathBuf> {
    ensure_clean_git_worktree()?;
    let manifest = read_json_file(manifest_path).with_context(|| {
        format!(
            "failed to read baseline promotion manifest {}",
            manifest_path.display()
        )
    })?;
    let plan = baseline_promotion_plan(&manifest)?;
    let replacing = env_flag(REPLACE_ENV);
    let needs_heavy_opt_in = plan.iter().any(|entry| {
        entry.promoted.get("baseline_class").and_then(Value::as_str) == Some("private_local_heavy")
    });
    if needs_heavy_opt_in && !env_flag(HEAVY_ENV) {
        bail!("promoting a private_local_heavy baseline requires {HEAVY_ENV}=1");
    }
    for entry in &plan {
        if entry.destination.exists() && !replacing {
            bail!(
                "baseline {} already exists; set {REPLACE_ENV}=1 to replace it deliberately",
                entry.destination.display()
            );
        }
    }

    fs::create_dir_all(BASELINE_DIR).with_context(|| format!("failed to create {BASELINE_DIR}"))?;
    for entry in &plan {
        write_json_file(&entry.destination, &entry.promoted)?;
    }

    let report = baseline_promotion_report_json(manifest_path, &plan);
    let report_path = Path::new(BASELINE_PROMOTE_JSON);
    fs::create_dir_all(BASELINE_PROMOTE_OUTPUT_DIR)
        .with_context(|| format!("failed to create {BASELINE_PROMOTE_OUTPUT_DIR}"))?;
    write_json_file(report_path, &report)?;
    Ok(report_path.to_path_buf())
}

pub(crate) fn baseline_refresh_plan(source_root: Option<&Path>) -> anyhow::Result<PathBuf> {
    let source_root = source_root.unwrap_or_else(|| Path::new(DEFAULT_BENCHMARK_REPORT_ROOT));
    fs::create_dir_all(BASELINE_REFRESH_PLAN_OUTPUT_DIR)
        .with_context(|| format!("failed to create {BASELINE_REFRESH_PLAN_OUTPUT_DIR}"))?;
    let (report, manifest) =
        baseline_refresh_plan_report_json(Path::new(BASELINE_DIR), source_root)?;
    let manifest_path = Path::new(BASELINE_REFRESH_PROMOTION_MANIFEST_JSON);
    let clean_rerun_script_path = Path::new(BASELINE_REFRESH_CLEAN_RERUN_SCRIPT);
    if let Some(manifest) = manifest {
        write_json_file(manifest_path, &manifest)?;
    } else if manifest_path.exists() {
        fs::remove_file(manifest_path)
            .with_context(|| format!("failed to remove stale {}", manifest_path.display()))?;
    }
    if baseline_refresh_clean_rerun_script_is_generated(&report) {
        write_baseline_refresh_clean_rerun_script(&report, clean_rerun_script_path)?;
    } else if clean_rerun_script_path.exists() {
        fs::remove_file(clean_rerun_script_path).with_context(|| {
            format!(
                "failed to remove stale {}",
                clean_rerun_script_path.display()
            )
        })?;
    }
    let report_path = Path::new(BASELINE_REFRESH_PLAN_JSON);
    write_json_file(report_path, &report)?;
    Ok(report_path.to_path_buf())
}

#[derive(Debug)]
struct PromotionPlanEntry {
    source_report: PathBuf,
    baseline_name: String,
    destination: PathBuf,
    promoted: Value,
}

fn baseline_promotion_plan(manifest: &Value) -> anyhow::Result<Vec<PromotionPlanEntry>> {
    if manifest.get("schema").and_then(Value::as_str) != Some(BASELINE_PROMOTION_MANIFEST_SCHEMA) {
        bail!("baseline promotion manifest has unsupported schema");
    }
    if manifest.get("schema_version").and_then(Value::as_u64)
        != Some(BASELINE_PROMOTION_SCHEMA_VERSION as u64)
    {
        bail!("baseline promotion manifest has unsupported schema_version");
    }
    let entries = manifest
        .get("entries")
        .and_then(Value::as_array)
        .context("baseline promotion manifest must contain entries array")?;
    if entries.is_empty() {
        bail!("baseline promotion manifest must contain at least one entry");
    }

    let mut seen_sources = BTreeSet::new();
    let mut seen_destinations = BTreeSet::new();
    let mut plan = Vec::new();
    for entry in entries {
        let source_report = require_manifest_string(entry, "source_report")?;
        let baseline_name = require_manifest_string(entry, "baseline_name")?;
        if !seen_sources.insert(source_report.clone()) {
            bail!("baseline promotion manifest contains duplicate source_report {source_report:?}");
        }
        if !seen_destinations.insert(baseline_name.clone()) {
            bail!("baseline promotion manifest contains duplicate baseline_name {baseline_name:?}");
        }
        let source_report = PathBuf::from(source_report);
        let destination = baseline_destination_path(&baseline_name)?;
        let source = read_json_file(&source_report).with_context(|| {
            format!(
                "failed to read source benchmark report {}",
                source_report.display()
            )
        })?;
        let promoted = promoted_baseline_json(&source)?;
        plan.push(PromotionPlanEntry {
            source_report,
            baseline_name,
            destination,
            promoted,
        });
    }
    Ok(plan)
}

fn require_manifest_string(value: &Value, field: &str) -> anyhow::Result<String> {
    let value = value
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("baseline promotion manifest entry missing {field}"))?;
    if value.trim().is_empty() {
        bail!("baseline promotion manifest entry has empty {field}");
    }
    Ok(value.to_owned())
}

fn baseline_promotion_report_json(manifest_path: &Path, plan: &[PromotionPlanEntry]) -> Value {
    let entries = plan
        .iter()
        .map(|entry| {
            json!({
                "source_report": entry.source_report,
                "baseline_name": entry.baseline_name,
                "destination": entry.destination,
                "baseline_class": entry.promoted.get("baseline_class").and_then(Value::as_str),
                "hardware_class": entry.promoted.get("hardware_class").and_then(Value::as_str),
                "dataset_class": entry.promoted.get("dataset_class").and_then(Value::as_str),
                "benchmark": entry.promoted.get("benchmark").and_then(Value::as_str),
                "timing_metric_count": entry.promoted.pointer("/baseline_promotion_policy/timing_metric_count").and_then(Value::as_u64),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema": BASELINE_PROMOTION_REPORT_SCHEMA,
        "schema_version": BASELINE_PROMOTION_SCHEMA_VERSION,
        "command": "baseline-promote-manifest",
        "status": "passed",
        "source_manifest": manifest_path,
        "summary": {
            "promotion_count": entries.len(),
            "source_worktree_required_clean": true,
            "source_report_dirty_worktree_required_false": true,
            "replacement_requires_env": REPLACE_ENV,
        },
        "entries": entries,
    })
}

#[derive(Debug, Clone)]
struct BaselineRefreshCandidate {
    path: PathBuf,
    signature: BaselineRefreshSignature,
    promotion_error: Option<String>,
    timing_metric_count: Option<u64>,
}

#[derive(Debug, Clone)]
struct BaselineRefreshEntry {
    baseline_path: PathBuf,
    baseline_name: String,
    signature: BaselineRefreshSignature,
    status: String,
    selected_source_report: Option<PathBuf>,
    matched_candidates: Vec<BaselineRefreshCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BaselineRefreshSignature {
    benchmark: Option<String>,
    baseline_class: Option<String>,
    hardware_name: Option<String>,
    hardware_class: Option<String>,
    dataset_class: Option<String>,
    benchmark_schema_version: Option<String>,
    experiment: Option<String>,
    package: Option<String>,
    input_dir: Option<String>,
    output_package: Option<String>,
    scenario: Option<String>,
    dataset_id: Option<String>,
    dataset_name: Option<String>,
}

fn baseline_refresh_plan_report_json(
    baseline_dir: &Path,
    source_root: &Path,
) -> anyhow::Result<(Value, Option<Value>)> {
    let stale_baselines = refresh_needed_baselines(baseline_dir)?;
    let candidates = baseline_refresh_candidates(source_root)?;
    let mut entries = stale_baselines
        .into_iter()
        .map(|(baseline_path, baseline_value)| {
            baseline_refresh_entry_for_baseline(&baseline_path, &baseline_value, &candidates)
        })
        .collect::<Vec<_>>();

    let mut selected_source_counts = BTreeMap::<PathBuf, usize>::new();
    for entry in &entries {
        if let Some(source_report) = &entry.selected_source_report {
            *selected_source_counts
                .entry(source_report.clone())
                .or_insert(0) += 1;
        }
    }
    for entry in &mut entries {
        if let Some(source_report) = &entry.selected_source_report
            && selected_source_counts
                .get(source_report)
                .copied()
                .unwrap_or(0)
                > 1
        {
            entry.status = "duplicate_selected_source_report".to_owned();
            entry.selected_source_report = None;
        }
    }

    let ready_entries = entries
        .iter()
        .filter(|entry| entry.status == "ready")
        .count();
    let not_ready_entries = entries.len().saturating_sub(ready_entries);
    let duplicate_selected_source_count = selected_source_counts
        .values()
        .filter(|count| **count > 1)
        .count();
    let status = if entries.is_empty() {
        "nothing_to_refresh"
    } else if not_ready_entries == 0 {
        "ready"
    } else {
        "not_ready"
    };
    let manifest_path = Path::new(BASELINE_REFRESH_PROMOTION_MANIFEST_JSON);
    let manifest = if status == "ready" {
        Some(baseline_refresh_promotion_manifest_json(&entries))
    } else {
        None
    };
    let clean_rerun_script = baseline_refresh_clean_rerun_script_path_json(&entries);
    let report = json!({
        "schema": BASELINE_REFRESH_PLAN_SCHEMA,
        "schema_version": BASELINE_PROMOTION_SCHEMA_VERSION,
        "command": "baseline-refresh-plan",
        "status": status,
        "baseline_dir": baseline_dir,
        "source_report_root": source_root,
        "generated_promotion_manifest": if manifest.is_some() { json!(manifest_path) } else { Value::Null },
        "generated_clean_rerun_script": clean_rerun_script,
        "summary": {
            "refresh_baseline_count": entries.len(),
            "candidate_report_count": candidates.len(),
            "ready_count": ready_entries,
            "not_ready_count": not_ready_entries,
            "duplicate_selected_source_count": duplicate_selected_source_count,
            "writes_curated_baselines": false,
            "promotion_command": if manifest.is_some() {
                json!(format!(
                    "MIRANTE4D_BASELINE_PROMOTE_REPLACE=1 cargo xtask baseline-promote-manifest {}",
                    manifest_path.display()
                ))
            } else {
                Value::Null
            },
            "remediation": baseline_refresh_summary_remediation_json(
                status,
                &entries,
                manifest_path,
            ),
        },
        "entries": entries
            .iter()
            .map(|entry| baseline_refresh_entry_json(entry, &candidates))
            .collect::<Vec<_>>(),
    });
    Ok((report, manifest))
}

fn refresh_needed_baselines(baseline_dir: &Path) -> anyhow::Result<Vec<(PathBuf, Value)>> {
    let mut paths = fs::read_dir(baseline_dir)
        .with_context(|| format!("failed to read {}", baseline_dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to list {}", baseline_dir.display()))?;
    paths.retain(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"));
    paths.sort();

    let mut baselines = Vec::new();
    for path in paths {
        let value = read_json_file(&path)
            .with_context(|| format!("failed to read baseline {}", path.display()))?;
        let baseline_status = string_at_path(&value, "baseline_status")
            .or_else(|| string_at_path(&value, "refresh_status"));
        let baseline_class = string_at_path(&value, "baseline_class");
        if baseline_status.as_deref() == Some(STALE_TIMING_STATUS) || baseline_class.is_none() {
            baselines.push((path, value));
        }
    }
    Ok(baselines)
}

fn baseline_refresh_candidates(
    source_root: &Path,
) -> anyhow::Result<Vec<BaselineRefreshCandidate>> {
    let mut paths = Vec::new();
    collect_json_files(source_root, &mut paths)?;
    paths.sort();
    let mut candidates = Vec::new();
    for path in paths {
        let Ok(value) = read_json_file(&path) else {
            continue;
        };
        if string_at_path(&value, "benchmark").is_none() {
            continue;
        }
        let promotion = promoted_baseline_json(&value);
        let (promotion_error, timing_metric_count) = match promotion {
            Ok(promoted) => (
                None,
                promoted
                    .pointer("/baseline_promotion_policy/timing_metric_count")
                    .and_then(Value::as_u64),
            ),
            Err(err) => (Some(err.to_string()), None),
        };
        candidates.push(BaselineRefreshCandidate {
            path,
            signature: baseline_refresh_signature(&value),
            promotion_error,
            timing_metric_count,
        });
    }
    Ok(candidates)
}

fn collect_json_files(path: &Path, paths: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_file() {
        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            paths.push(path.to_path_buf());
        }
        return Ok(());
    }
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let path = entry
            .with_context(|| format!("failed to list {}", path.display()))?
            .path();
        collect_json_files(&path, paths)?;
    }
    Ok(())
}

fn baseline_refresh_entry_for_baseline(
    baseline_path: &Path,
    baseline_value: &Value,
    candidates: &[BaselineRefreshCandidate],
) -> BaselineRefreshEntry {
    let signature = baseline_refresh_signature(baseline_value);
    let matched_candidates = candidates
        .iter()
        .filter(|candidate| baseline_refresh_signature_matches(&signature, &candidate.signature))
        .cloned()
        .collect::<Vec<_>>();
    let eligible = matched_candidates
        .iter()
        .filter(|candidate| candidate.promotion_error.is_none())
        .collect::<Vec<_>>();
    let (status, selected_source_report) = match eligible.as_slice() {
        [candidate] => ("ready".to_owned(), Some(candidate.path.clone())),
        [] if matched_candidates.is_empty() => ("no_matching_candidate".to_owned(), None),
        [] => ("matched_candidates_not_promotable".to_owned(), None),
        _ => ("ambiguous_eligible_candidates".to_owned(), None),
    };
    BaselineRefreshEntry {
        baseline_path: baseline_path.to_path_buf(),
        baseline_name: baseline_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned(),
        signature,
        status,
        selected_source_report,
        matched_candidates,
    }
}

fn baseline_refresh_signature(value: &Value) -> BaselineRefreshSignature {
    BaselineRefreshSignature {
        benchmark: string_at_path(value, "benchmark"),
        baseline_class: string_at_path(value, "baseline_class"),
        hardware_name: string_at_path(value, "hardware.name")
            .or_else(|| string_at_path(value, "host.name")),
        hardware_class: string_at_path(value, "hardware_class"),
        dataset_class: string_at_path(value, "dataset_class"),
        benchmark_schema_version: string_at_path(value, "benchmark_schema_version")
            .or_else(|| string_at_path(value, "schema_version")),
        experiment: string_at_path(value, "experiment"),
        package: string_at_path(value, "package"),
        input_dir: string_at_path(value, "input_dir"),
        output_package: string_at_path(value, "output_package"),
        scenario: string_at_path(value, "scenario")
            .or_else(|| string_at_path(value, "scenario.name"))
            .or_else(|| string_at_path(value, "scenario.label")),
        dataset_id: string_at_path(value, "dataset.id"),
        dataset_name: string_at_path(value, "dataset.name"),
    }
}

fn baseline_refresh_signature_matches(
    baseline: &BaselineRefreshSignature,
    candidate: &BaselineRefreshSignature,
) -> bool {
    baseline_signature_field_matches(&baseline.benchmark, &candidate.benchmark)
        && baseline_signature_field_matches(&baseline.baseline_class, &candidate.baseline_class)
        && baseline_signature_field_matches(&baseline.hardware_name, &candidate.hardware_name)
        && baseline_signature_field_matches(&baseline.hardware_class, &candidate.hardware_class)
        && baseline_signature_field_matches(&baseline.dataset_class, &candidate.dataset_class)
        && baseline_signature_field_matches(
            &baseline.benchmark_schema_version,
            &candidate.benchmark_schema_version,
        )
        && baseline_signature_field_matches(&baseline.experiment, &candidate.experiment)
        && baseline_signature_field_matches(&baseline.package, &candidate.package)
        && baseline_signature_field_matches(&baseline.input_dir, &candidate.input_dir)
        && baseline_signature_field_matches(&baseline.output_package, &candidate.output_package)
        && baseline_signature_field_matches(&baseline.scenario, &candidate.scenario)
        && baseline_signature_field_matches(&baseline.dataset_id, &candidate.dataset_id)
        && baseline_signature_field_matches(&baseline.dataset_name, &candidate.dataset_name)
}

fn baseline_signature_field_matches(baseline: &Option<String>, candidate: &Option<String>) -> bool {
    match baseline {
        Some(baseline) => candidate.as_deref() == Some(baseline.as_str()),
        None => true,
    }
}

fn baseline_refresh_promotion_manifest_json(entries: &[BaselineRefreshEntry]) -> Value {
    json!({
        "schema": BASELINE_PROMOTION_MANIFEST_SCHEMA,
        "schema_version": BASELINE_PROMOTION_SCHEMA_VERSION,
        "generated_by": "baseline-refresh-plan",
        "entries": entries
            .iter()
            .map(|entry| {
                json!({
                    "source_report": entry.selected_source_report,
                    "baseline_name": entry.baseline_name,
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn baseline_refresh_clean_rerun_script_path_json(entries: &[BaselineRefreshEntry]) -> Value {
    let clean_rerun_entries = entries
        .iter()
        .filter(|entry| entry_requires_clean_rerun(entry))
        .collect::<Vec<_>>();
    if clean_rerun_entries.is_empty()
        || clean_rerun_entries
            .iter()
            .any(|entry| rerun_command_for_signature(&entry.signature).is_none())
    {
        Value::Null
    } else {
        json!(Path::new(BASELINE_REFRESH_CLEAN_RERUN_SCRIPT))
    }
}

fn baseline_refresh_clean_rerun_script_is_generated(report: &Value) -> bool {
    report
        .get("generated_clean_rerun_script")
        .and_then(Value::as_str)
        .is_some_and(|path| !path.trim().is_empty())
}

fn write_baseline_refresh_clean_rerun_script(report: &Value, path: &Path) -> anyhow::Result<()> {
    let script = baseline_refresh_clean_rerun_script(report)?;
    fs::write(path, script)
        .with_context(|| format!("failed to write clean rerun script {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path)
            .with_context(|| format!("failed to stat clean rerun script {}", path.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).with_context(|| {
            format!(
                "failed to mark clean rerun script executable {}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn baseline_refresh_clean_rerun_script(report: &Value) -> anyhow::Result<String> {
    let entries = report
        .get("entries")
        .and_then(Value::as_array)
        .context("baseline-refresh-plan report missing entries for clean rerun script")?;
    let mut lines = vec![
        "#!/usr/bin/env bash".to_owned(),
        "set -euo pipefail".to_owned(),
        String::new(),
        "# Generated by `cargo xtask baseline-refresh-plan`.".to_owned(),
        "# Run from the repository root after committing or stashing all worktree changes."
            .to_owned(),
        "if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then".to_owned(),
        "  echo 'baseline clean reruns must be launched from a git worktree' >&2".to_owned(),
        "  exit 1".to_owned(),
        "fi".to_owned(),
        "if [[ -n \"$(git status --porcelain)\" ]]; then".to_owned(),
        "  echo 'baseline clean reruns require git status --porcelain to be empty' >&2".to_owned(),
        "  git status --short >&2".to_owned(),
        "  exit 1".to_owned(),
        "fi".to_owned(),
        String::new(),
    ];
    let mut command_count = 0usize;
    for entry in entries {
        if entry
            .pointer("/remediation/requires_clean_rerun")
            .and_then(Value::as_bool)
            != Some(true)
        {
            continue;
        }
        let baseline_name = entry
            .get("baseline_name")
            .and_then(Value::as_str)
            .unwrap_or("unknown-baseline")
            .replace('\n', " ");
        lines.push(format!(
            "echo {}",
            shell_quote(&format!("baseline-refresh: rerunning {baseline_name}"))
        ));
        let prerequisites = entry
            .pointer("/rerun/prerequisite_commands")
            .and_then(Value::as_array)
            .context("clean-rerun entry missing prerequisite commands")?;
        for prerequisite in prerequisites {
            if let Some(command) = prerequisite.get("shell_command").and_then(Value::as_str) {
                lines.push(command.to_owned());
                command_count += 1;
            }
        }
        let primary = entry
            .pointer("/rerun/primary_command/shell_command")
            .and_then(Value::as_str)
            .context("clean-rerun entry missing primary shell command")?;
        lines.push(primary.to_owned());
        command_count += 1;
        lines.push(String::new());
    }
    lines.push(format!(
        "echo 'baseline-refresh: completed {command_count} rerun command(s); regenerating plan'"
    ));
    lines.push("cargo xtask baseline-refresh-plan".to_owned());
    lines.push(String::new());
    Ok(lines.join("\n"))
}

fn baseline_refresh_entry_json(
    entry: &BaselineRefreshEntry,
    candidates: &[BaselineRefreshCandidate],
) -> Value {
    json!({
        "baseline_path": entry.baseline_path,
        "baseline_name": entry.baseline_name,
        "status": entry.status,
        "selected_source_report": entry.selected_source_report,
        "signature": baseline_refresh_signature_json(&entry.signature),
        "matched_candidate_count": entry.matched_candidates.len(),
        "eligible_candidate_count": entry
            .matched_candidates
            .iter()
            .filter(|candidate| candidate.promotion_error.is_none())
            .count(),
        "matched_candidates": entry
            .matched_candidates
            .iter()
            .map(baseline_refresh_candidate_json)
            .collect::<Vec<_>>(),
        "remediation": baseline_refresh_entry_remediation_json(entry),
        "rerun": baseline_refresh_entry_rerun_json(entry, candidates),
    })
}

fn baseline_refresh_candidate_json(candidate: &BaselineRefreshCandidate) -> Value {
    json!({
        "source_report": candidate.path,
        "promotable": candidate.promotion_error.is_none(),
        "promotion_error": candidate.promotion_error,
        "timing_metric_count": candidate.timing_metric_count,
        "signature": baseline_refresh_signature_json(&candidate.signature),
    })
}

fn baseline_refresh_summary_remediation_json(
    status: &str,
    entries: &[BaselineRefreshEntry],
    manifest_path: &Path,
) -> Value {
    let clean_rerun_required_count = entries
        .iter()
        .filter(|entry| entry_requires_clean_rerun(entry))
        .count();
    let rerun_command_available_count = entries
        .iter()
        .filter(|entry| rerun_command_for_signature(&entry.signature).is_some())
        .count();
    let no_matching_candidate_count = entries
        .iter()
        .filter(|entry| entry.status == "no_matching_candidate")
        .count();
    let not_promotable_candidate_count = entries
        .iter()
        .filter(|entry| entry.status == "matched_candidates_not_promotable")
        .count();
    let ambiguous_candidate_count = entries
        .iter()
        .filter(|entry| entry.status == "ambiguous_eligible_candidates")
        .count();
    let duplicate_source_entry_count = entries
        .iter()
        .filter(|entry| entry.status == "duplicate_selected_source_report")
        .count();
    let next_action = match status {
        "ready" => format!(
            "review {}, then run MIRANTE4D_BASELINE_PROMOTE_REPLACE=1 cargo xtask baseline-promote-manifest {} from a clean worktree",
            manifest_path.display(),
            manifest_path.display()
        ),
        "nothing_to_refresh" => "no baseline refresh action is required".to_owned(),
        _ if clean_rerun_required_count > 0 => {
            "run each entry.rerun prerequisite command and primary command from a clean worktree, then rerun cargo xtask baseline-refresh-plan".to_owned()
        }
        _ if no_matching_candidate_count > 0 => {
            "rerun the missing matching benchmarks, then rerun cargo xtask baseline-refresh-plan"
                .to_owned()
        }
        _ if ambiguous_candidate_count > 0 || duplicate_source_entry_count > 0 => {
            "remove ambiguous candidate reports or produce one canonical report per stale baseline, then rerun cargo xtask baseline-refresh-plan".to_owned()
        }
        _ => {
            "fix candidate benchmark reports until baseline promotion prevalidation passes, then rerun cargo xtask baseline-refresh-plan".to_owned()
        }
    };

    json!({
        "next_action": next_action,
        "source_worktree_required_clean": true,
        "source_report_dirty_worktree_required_false": true,
        "current_worktree_must_be_clean_for_promotion": true,
        "clean_rerun_required_count": clean_rerun_required_count,
        "rerun_command_available_count": rerun_command_available_count,
        "rerun_command_unavailable_count": entries.len().saturating_sub(rerun_command_available_count),
        "no_matching_candidate_count": no_matching_candidate_count,
        "not_promotable_candidate_count": not_promotable_candidate_count,
        "ambiguous_candidate_count": ambiguous_candidate_count,
        "duplicate_source_entry_count": duplicate_source_entry_count,
    })
}

fn baseline_refresh_entry_remediation_json(entry: &BaselineRefreshEntry) -> Value {
    let candidate_errors = entry
        .matched_candidates
        .iter()
        .filter_map(|candidate| candidate.promotion_error.clone())
        .collect::<Vec<_>>();
    let requires_clean_rerun = entry_requires_clean_rerun(entry);
    let action = match entry.status.as_str() {
        "ready" => "this baseline is ready for batch promotion once every stale baseline is ready",
        "no_matching_candidate" => {
            "rerun the benchmark that matches this baseline signature, then rerun baseline-refresh-plan"
        }
        "matched_candidates_not_promotable" if requires_clean_rerun => {
            "rerun the matched benchmark from a clean release worktree, then rerun baseline-refresh-plan"
        }
        "matched_candidates_not_promotable" => {
            "fix or rerun the matched benchmark report until promotion prevalidation passes"
        }
        "ambiguous_eligible_candidates" => {
            "remove ambiguous candidate reports or keep one canonical promotable source report"
        }
        "duplicate_selected_source_report" => {
            "produce a unique source report for this baseline instead of reusing another baseline's source"
        }
        _ => "inspect this baseline refresh entry",
    };
    json!({
        "action": action,
        "requires_clean_rerun": requires_clean_rerun,
        "candidate_error_count": candidate_errors.len(),
        "candidate_errors": candidate_errors,
    })
}

fn baseline_refresh_entry_rerun_json(
    entry: &BaselineRefreshEntry,
    candidates: &[BaselineRefreshCandidate],
) -> Value {
    let primary = rerun_command_for_signature(&entry.signature);
    json!({
        "available": primary.is_some(),
        "release_build_required": true,
        "primary_command": primary,
        "prerequisite_commands": rerun_prerequisite_commands_for_signature(&entry.signature, candidates),
        "unavailable_reason": if primary.is_some() {
            Value::Null
        } else {
            json!("no known xtask benchmark command mapping for this baseline signature")
        },
    })
}

fn rerun_prerequisite_commands_for_signature(
    signature: &BaselineRefreshSignature,
    candidates: &[BaselineRefreshCandidate],
) -> Vec<Value> {
    let mut commands = Vec::new();
    let Some(package) = signature.package.as_deref() else {
        return commands;
    };

    if let Some(import_signature) = candidates
        .iter()
        .map(|candidate| &candidate.signature)
        .find(|candidate| {
            candidate.benchmark.as_deref() == Some("bench-import-sample")
                && candidate.output_package.as_deref() == Some(package)
        })
        && let Some(command) = rerun_command_for_signature(import_signature)
    {
        commands.push(command);
        return commands;
    }

    if let Some(fixture_name) = fixture_name_from_generated_package(package) {
        commands.push(rerun_command_json(
            BTreeMap::new(),
            vec!["generate-fixture".to_owned(), fixture_name],
        ));
    }

    commands
}

fn rerun_command_for_signature(signature: &BaselineRefreshSignature) -> Option<Value> {
    let benchmark = signature.benchmark.as_deref()?;
    let mut env = benchmark_identity_env(signature);
    let args = match benchmark {
        "bench-runtime-stress" | "bench-phase11-synthetic-matrix" => {
            Some(vec![benchmark.to_owned()])
        }
        "bench-import-sample" => {
            let experiment = signature
                .experiment
                .clone()
                .or_else(|| path_file_name(signature.input_dir.as_deref()?))?;
            if let Some(sample_root) = signature.input_dir.as_deref().and_then(parent_path_string) {
                env.insert("MIRANTE4D_SAMPLE_DATA".to_owned(), sample_root);
            }
            Some(vec![benchmark.to_owned(), experiment])
        }
        "bench-native-package"
        | "bench-phase11-large-view"
        | "bench-phase11-interaction"
        | "bench-phase11-viewport-matrix"
        | "bench-phase13-renderer"
        | "bench-phase13-viewport-matrix" => {
            Some(vec![benchmark.to_owned(), signature.package.clone()?])
        }
        _ => None,
    }?;
    if signature.baseline_class.as_deref() == Some("private_local_heavy")
        || benchmark_requires_heavy_opt_in(benchmark)
    {
        env.insert(HEAVY_ENV.to_owned(), "1".to_owned());
    }
    Some(rerun_command_json(env, args))
}

fn benchmark_requires_heavy_opt_in(benchmark: &str) -> bool {
    COMMAND_AUDIT_ENTRIES
        .iter()
        .any(|entry| entry.command == benchmark && entry.requires_heavy_opt_in)
}

fn benchmark_identity_env(signature: &BaselineRefreshSignature) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    if let Some(value) = &signature.baseline_class {
        env.insert("MIRANTE4D_BENCH_BASELINE_CLASS".to_owned(), value.clone());
    }
    if let Some(value) = &signature.hardware_class {
        env.insert("MIRANTE4D_BENCH_HARDWARE_CLASS".to_owned(), value.clone());
    }
    if let Some(value) = &signature.hardware_name {
        env.insert("MIRANTE4D_BENCH_HARDWARE_NAME".to_owned(), value.clone());
    } else if let Some(value) = &signature.hardware_class {
        env.insert("MIRANTE4D_BENCH_HARDWARE_NAME".to_owned(), value.clone());
    }
    if let Some(value) = &signature.dataset_class {
        env.insert("MIRANTE4D_BENCH_DATASET_CLASS".to_owned(), value.clone());
    }
    env
}

fn rerun_command_json(env: BTreeMap<String, String>, xtask_args: Vec<String>) -> Value {
    let mut argv = vec![
        "cargo".to_owned(),
        "run".to_owned(),
        "--release".to_owned(),
        "-p".to_owned(),
        "xtask".to_owned(),
        "--".to_owned(),
    ];
    argv.extend(xtask_args);
    let env_prefix = env
        .iter()
        .map(|(name, value)| format!("{name}={}", shell_quote(value)))
        .collect::<Vec<_>>();
    let shell_command = env_prefix
        .iter()
        .cloned()
        .chain(argv.iter().map(|arg| shell_quote(arg)))
        .collect::<Vec<_>>()
        .join(" ");
    json!({
        "env": env,
        "argv": argv,
        "shell_command": shell_command,
    })
}

fn fixture_name_from_generated_package(package: &str) -> Option<String> {
    let normalized = package.replace('\\', "/");
    normalized
        .strip_prefix("target/mirante4d/fixtures/")?
        .strip_suffix(".m4d")
        .map(str::to_owned)
}

fn parent_path_string(path: &str) -> Option<String> {
    Path::new(path)
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
}

fn path_file_name(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':' | '='))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn entry_requires_clean_rerun(entry: &BaselineRefreshEntry) -> bool {
    entry.matched_candidates.iter().any(|candidate| {
        candidate
            .promotion_error
            .as_deref()
            .is_some_and(|error| error.contains("dirty_worktree"))
    })
}

fn baseline_refresh_signature_json(signature: &BaselineRefreshSignature) -> Value {
    json!({
        "benchmark": signature.benchmark,
        "baseline_class": signature.baseline_class,
        "hardware_name": signature.hardware_name,
        "hardware_class": signature.hardware_class,
        "dataset_class": signature.dataset_class,
        "benchmark_schema_version": signature.benchmark_schema_version,
        "experiment": signature.experiment,
        "package": signature.package,
        "input_dir": signature.input_dir,
        "output_package": signature.output_package,
        "scenario": signature.scenario,
        "dataset_id": signature.dataset_id,
        "dataset_name": signature.dataset_name,
    })
}

fn promoted_baseline_json(source: &Value) -> anyhow::Result<Value> {
    let object = source
        .as_object()
        .context("benchmark report root must be a JSON object")?;
    require_nonempty_string(source, "benchmark")?;
    let baseline_class = require_nonempty_string(source, "baseline_class")?;
    if !ACCEPTED_BASELINE_CLASSES.contains(&baseline_class.as_str()) {
        bail!("baseline_class {baseline_class:?} is not accepted");
    }
    require_nonempty_string(source, "hardware_class")?;
    require_nonempty_string(source, "dataset_class")?;
    require_git_commit(source)?;
    require_release_profile(source)?;
    require_clean_report_worktree(source)?;
    let timing_count = timing_metric_count(source)?;
    if timing_count == 0 {
        bail!("benchmark report contains no timing metrics under timings_ms");
    }

    let mut promoted = object.clone();
    promoted.remove("refresh_status");
    promoted.insert(
        "baseline_status".to_owned(),
        Value::String("current".to_owned()),
    );
    promoted.insert(
        "baseline_promoted_at_epoch_ms".to_owned(),
        json!(epoch_ms()),
    );
    promoted.insert(
        "baseline_promotion_policy".to_owned(),
        json!({
            "source_worktree_required_clean": true,
            "source_report_dirty_worktree_required_false": true,
            "timing_metric_count": timing_count,
        }),
    );
    Ok(Value::Object(promoted))
}

fn baseline_destination_path(filename: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(filename);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(name)), None) => {
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                bail!("baseline filename must end in .json");
            }
            Ok(Path::new(BASELINE_DIR).join(name))
        }
        _ => bail!("baseline filename must be a single path-safe file name under {BASELINE_DIR}"),
    }
}

fn require_nonempty_string(value: &Value, path: &str) -> anyhow::Result<String> {
    let value = string_at_path(value, path)
        .with_context(|| format!("benchmark report must contain nonempty string field {path:?}"))?;
    if value.trim().is_empty() {
        bail!("benchmark report field {path:?} must not be empty");
    }
    Ok(value)
}

fn require_git_commit(value: &Value) -> anyhow::Result<()> {
    for path in [
        "git_commit",
        "git.commit",
        "hardware.git_commit",
        "host.git_commit",
    ] {
        if string_at_path(value, path)
            .map(|commit| !commit.trim().is_empty())
            .unwrap_or(false)
        {
            return Ok(());
        }
    }
    bail!("baseline promotion requires an explicit git commit field");
}

fn require_release_profile(value: &Value) -> anyhow::Result<()> {
    for path in [
        "build_profile",
        "hardware.build_profile",
        "host.build_profile",
    ] {
        if let Some(profile) = string_at_path(value, path) {
            if profile == "release" {
                return Ok(());
            }
            bail!("baseline promotion requires release build_profile, got {profile:?}");
        }
    }
    bail!("baseline promotion requires an explicit release build_profile field");
}

fn require_clean_report_worktree(value: &Value) -> anyhow::Result<()> {
    for path in [
        "dirty_worktree",
        "git.dirty_worktree",
        "hardware.dirty_worktree",
        "host.dirty_worktree",
    ] {
        if let Some(dirty) = bool_at_path(value, path) {
            if dirty {
                bail!("baseline promotion refuses source reports with {path}=true");
            }
            return Ok(());
        }
    }
    bail!("baseline promotion requires an explicit dirty_worktree=false field");
}

fn timing_metric_count(value: &Value) -> anyhow::Result<usize> {
    let mut count = 0;
    count_timing_metrics_recursive(value, &mut count)?;
    Ok(count)
}

fn count_timing_metrics_recursive(value: &Value, count: &mut usize) -> anyhow::Result<()> {
    if let Some(array) = value.as_array() {
        for child in array {
            count_timing_metrics_recursive(child, count)?;
        }
        return Ok(());
    }
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    if let Some(timings) = object.get("timings_ms") {
        count_timing_metric_value(timings, count)?;
    }
    for (name, child) in object {
        if name == "timings_ms" {
            continue;
        }
        if child.is_object() || child.is_array() {
            count_timing_metrics_recursive(child, count)?;
        }
    }
    Ok(())
}

fn count_timing_metric_value(value: &Value, count: &mut usize) -> anyhow::Result<()> {
    if let Some(timing) = value.as_f64() {
        if !timing.is_finite() || timing < 0.0 {
            bail!("timing metrics must be finite and nonnegative");
        }
        *count += 1;
        return Ok(());
    }
    if value.is_null() {
        return Ok(());
    }
    let Some(object) = value.as_object() else {
        bail!("timings_ms values must be numbers, null, or nested timing objects");
    };
    for child in object.values() {
        count_timing_metric_value(child, count)?;
    }
    Ok(())
}

fn ensure_clean_git_worktree() -> anyhow::Result<()> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .context("failed to run git status for baseline promotion")?;
    if !output.status.success() {
        bail!("git status failed while checking baseline promotion preconditions");
    }
    if !output.stdout.is_empty() {
        bail!("baseline promotion requires a clean git worktree before writing curated baselines");
    }
    Ok(())
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            let trimmed = value.trim();
            trimmed == "1"
                || trimmed.eq_ignore_ascii_case("true")
                || trimmed.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

fn string_at_path(value: &Value, path: &str) -> Option<String> {
    match value_at_path(value, path)? {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn bool_at_path(value: &Value, path: &str) -> Option<bool> {
    value_at_path(value, path)?.as_bool()
}

fn value_at_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    value.get(path).or_else(|| {
        path.split('.')
            .try_fold(value, |current, key| current.get(key))
    })
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn promotable_report() -> Value {
        json!({
            "benchmark": "bench-runtime-stress",
            "benchmark_schema_version": 1,
            "baseline_class": "local_gpu",
            "hardware_class": "hw2-linux-vulkan-reference",
            "dataset_class": "synthetic_runtime_stress",
            "hardware": {
                "name": "test-gpu",
                "build_profile": "release",
                "git_commit": "abc123",
                "dirty_worktree": false
            },
            "timings_ms": {
                "total": 1.0,
                "nested": {
                    "gpu": null,
                    "cpu": 0.5
                }
            }
        })
    }

    fn stale_baseline_from(report: &Value) -> Value {
        let mut baseline = report.clone();
        baseline["baseline_status"] = json!(STALE_TIMING_STATUS);
        baseline
    }

    fn refresh_signature_for(benchmark: &str) -> BaselineRefreshSignature {
        BaselineRefreshSignature {
            benchmark: Some(benchmark.to_owned()),
            baseline_class: Some("local_gpu".to_owned()),
            hardware_name: Some("test-gpu".to_owned()),
            hardware_class: Some("hw2-linux-vulkan-reference".to_owned()),
            dataset_class: Some("imported_real_sample_package".to_owned()),
            benchmark_schema_version: None,
            experiment: None,
            package: Some("target/mirante4d/benchmarks/import-sample/t5_qual_003.m4d".to_owned()),
            input_dir: None,
            output_package: None,
            scenario: None,
            dataset_id: None,
            dataset_name: None,
        }
    }

    #[test]
    fn promotion_json_marks_baseline_current_and_records_policy() {
        let promoted = promoted_baseline_json(&promotable_report()).unwrap();

        assert_eq!(promoted["baseline_status"], "current");
        assert_eq!(
            promoted["baseline_promotion_policy"]["source_worktree_required_clean"],
            true
        );
        assert_eq!(
            promoted["baseline_promotion_policy"]["timing_metric_count"],
            2
        );
    }

    #[test]
    fn promotion_rejects_dirty_source_report() {
        let mut report = promotable_report();
        report["hardware"]["dirty_worktree"] = json!(true);

        let err = promoted_baseline_json(&report).unwrap_err().to_string();

        assert!(err.contains("dirty_worktree"));
    }

    #[test]
    fn promotion_rejects_non_release_reports() {
        let mut report = promotable_report();
        report["hardware"]["build_profile"] = json!("debug");

        let err = promoted_baseline_json(&report).unwrap_err().to_string();

        assert!(err.contains("release build_profile"));
    }

    #[test]
    fn promotion_rejects_reports_without_timing_metrics() {
        let mut report = promotable_report();
        report.as_object_mut().unwrap().remove("timings_ms");

        let err = promoted_baseline_json(&report).unwrap_err().to_string();

        assert!(err.contains("no timing metrics"));
    }

    #[test]
    fn promotion_destination_must_be_single_json_filename() {
        assert!(baseline_destination_path("current.json").is_ok());
        assert!(baseline_destination_path("../current.json").is_err());
        assert!(baseline_destination_path("nested/current.json").is_err());
        assert!(baseline_destination_path("current.txt").is_err());
    }

    #[test]
    fn promotion_manifest_rejects_duplicate_destinations_before_writing() {
        let tempdir = tempfile::tempdir().unwrap();
        let source_a = tempdir.path().join("a.json");
        let source_b = tempdir.path().join("b.json");
        write_json_file(&source_a, &promotable_report()).unwrap();
        write_json_file(&source_b, &promotable_report()).unwrap();
        let manifest = json!({
            "schema": BASELINE_PROMOTION_MANIFEST_SCHEMA,
            "schema_version": BASELINE_PROMOTION_SCHEMA_VERSION,
            "entries": [
                {
                    "source_report": source_a,
                    "baseline_name": "current.json"
                },
                {
                    "source_report": source_b,
                    "baseline_name": "current.json"
                }
            ]
        });

        let err = baseline_promotion_plan(&manifest).unwrap_err().to_string();

        assert!(err.contains("duplicate baseline_name"));
    }

    #[test]
    fn promotion_manifest_builds_batch_plan_from_release_reports() {
        let tempdir = tempfile::tempdir().unwrap();
        let source_a = tempdir.path().join("a.json");
        let source_b = tempdir.path().join("b.json");
        write_json_file(&source_a, &promotable_report()).unwrap();
        write_json_file(&source_b, &promotable_report()).unwrap();
        let manifest = json!({
            "schema": BASELINE_PROMOTION_MANIFEST_SCHEMA,
            "schema_version": BASELINE_PROMOTION_SCHEMA_VERSION,
            "entries": [
                {
                    "source_report": source_a,
                    "baseline_name": "a.json"
                },
                {
                    "source_report": source_b,
                    "baseline_name": "b.json"
                }
            ]
        });

        let plan = baseline_promotion_plan(&manifest).unwrap();
        let report = baseline_promotion_report_json(Path::new("manifest.json"), &plan);

        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].destination, Path::new(BASELINE_DIR).join("a.json"));
        assert_eq!(report["schema"], BASELINE_PROMOTION_REPORT_SCHEMA);
        assert_eq!(report["summary"]["promotion_count"], 2);
        assert_eq!(report["entries"][0]["baseline_class"], "local_gpu");
    }

    #[test]
    fn refresh_plan_writes_manifest_when_every_stale_baseline_has_unique_promotable_source() {
        let tempdir = tempfile::tempdir().unwrap();
        let baseline_dir = tempdir.path().join("baselines");
        let source_root = tempdir.path().join("reports");
        fs::create_dir_all(&baseline_dir).unwrap();
        fs::create_dir_all(&source_root).unwrap();
        let source = source_root.join("bench-runtime-stress.json");
        let baseline = baseline_dir.join("runtime-baseline.json");
        let report = promotable_report();
        write_json_file(&source, &report).unwrap();
        write_json_file(&baseline, &stale_baseline_from(&report)).unwrap();

        let (plan, manifest) =
            baseline_refresh_plan_report_json(&baseline_dir, &source_root).unwrap();
        let manifest = manifest.expect("ready refresh plan should emit a manifest");

        assert_eq!(plan["status"], "ready");
        assert!(plan["generated_clean_rerun_script"].is_null());
        assert_eq!(plan["summary"]["ready_count"], 1);
        assert_eq!(plan["summary"]["not_ready_count"], 0);
        assert!(
            plan["summary"]["remediation"]["next_action"]
                .as_str()
                .unwrap()
                .contains("baseline-promote-manifest")
        );
        assert_eq!(plan["entries"][0]["status"], "ready");
        assert_eq!(
            plan["entries"][0]["remediation"]["requires_clean_rerun"],
            false
        );
        assert_eq!(
            plan["entries"][0]["selected_source_report"],
            source.to_string_lossy().as_ref()
        );
        assert_eq!(manifest["schema"], BASELINE_PROMOTION_MANIFEST_SCHEMA);
        assert_eq!(
            manifest["entries"][0]["source_report"],
            source.to_string_lossy().as_ref()
        );
        assert_eq!(
            manifest["entries"][0]["baseline_name"],
            "runtime-baseline.json"
        );
    }

    #[test]
    fn refresh_plan_reports_matching_dirty_source_as_not_promotable() {
        let tempdir = tempfile::tempdir().unwrap();
        let baseline_dir = tempdir.path().join("baselines");
        let source_root = tempdir.path().join("reports");
        fs::create_dir_all(&baseline_dir).unwrap();
        fs::create_dir_all(&source_root).unwrap();
        let source = source_root.join("dirty-runtime.json");
        let baseline = baseline_dir.join("runtime-baseline.json");
        let mut dirty_report = promotable_report();
        dirty_report["hardware"]["dirty_worktree"] = json!(true);
        write_json_file(&source, &dirty_report).unwrap();
        write_json_file(&baseline, &stale_baseline_from(&dirty_report)).unwrap();

        let (plan, manifest) =
            baseline_refresh_plan_report_json(&baseline_dir, &source_root).unwrap();

        assert!(manifest.is_none());
        assert_eq!(plan["status"], "not_ready");
        assert_eq!(
            plan["generated_clean_rerun_script"],
            BASELINE_REFRESH_CLEAN_RERUN_SCRIPT
        );
        assert_eq!(
            plan["entries"][0]["status"],
            "matched_candidates_not_promotable"
        );
        assert_eq!(
            plan["entries"][0]["matched_candidates"][0]["promotable"],
            false
        );
        assert_eq!(
            plan["summary"]["remediation"]["clean_rerun_required_count"],
            1
        );
        assert_eq!(
            plan["summary"]["remediation"]["rerun_command_available_count"],
            1
        );
        assert_eq!(plan["entries"][0]["rerun"]["available"], true);
        assert_eq!(
            plan["entries"][0]["rerun"]["primary_command"]["argv"][0],
            "cargo"
        );
        assert_eq!(
            plan["entries"][0]["rerun"]["primary_command"]["argv"][2],
            "--release"
        );
        assert_eq!(
            plan["entries"][0]["rerun"]["primary_command"]["env"]["MIRANTE4D_BENCH_HARDWARE_NAME"],
            "test-gpu"
        );
        assert_eq!(
            plan["entries"][0]["remediation"]["requires_clean_rerun"],
            true
        );
        assert_eq!(
            plan["entries"][0]["remediation"]["candidate_error_count"],
            1
        );
        assert!(
            plan["entries"][0]["matched_candidates"][0]["promotion_error"]
                .as_str()
                .unwrap()
                .contains("dirty_worktree")
        );

        let script = baseline_refresh_clean_rerun_script(&plan).unwrap();
        assert!(script.contains("git status --porcelain"));
        assert!(script.contains("bench-runtime-stress"));
        assert!(script.contains("cargo xtask baseline-refresh-plan"));
    }

    #[test]
    fn refresh_plan_clean_rerun_script_is_written_with_clean_worktree_guard() {
        let tempdir = tempfile::tempdir().unwrap();
        let baseline_dir = tempdir.path().join("baselines");
        let source_root = tempdir.path().join("reports");
        let script_path = tempdir.path().join("baseline-clean-reruns.sh");
        fs::create_dir_all(&baseline_dir).unwrap();
        fs::create_dir_all(&source_root).unwrap();
        let source = source_root.join("dirty-runtime.json");
        let baseline = baseline_dir.join("runtime-baseline.json");
        let mut dirty_report = promotable_report();
        dirty_report["hardware"]["dirty_worktree"] = json!(true);
        write_json_file(&source, &dirty_report).unwrap();
        write_json_file(&baseline, &stale_baseline_from(&dirty_report)).unwrap();
        let (plan, _) = baseline_refresh_plan_report_json(&baseline_dir, &source_root).unwrap();

        write_baseline_refresh_clean_rerun_script(&plan, &script_path).unwrap();
        let script = fs::read_to_string(script_path).unwrap();

        assert!(script.starts_with("#!/usr/bin/env bash\nset -euo pipefail"));
        assert!(script.contains("git status --porcelain"));
        assert!(script.contains("baseline-refresh: rerunning runtime-baseline.json"));
        assert!(script.contains("cargo xtask baseline-refresh-plan"));
    }

    #[test]
    fn refresh_plan_records_release_rerun_commands_and_import_prerequisites() {
        let tempdir = tempfile::tempdir().unwrap();
        let baseline_dir = tempdir.path().join("baselines");
        let source_root = tempdir.path().join("reports");
        fs::create_dir_all(&baseline_dir).unwrap();
        fs::create_dir_all(&source_root).unwrap();

        let mut import_report = promotable_report();
        import_report["benchmark"] = json!("bench-import-sample");
        import_report["dataset_class"] = json!("bounded_real_sample_import");
        import_report["experiment"] = json!("T5-QUAL-003");
        import_report["input_dir"] = json!("/samples/T5-QUAL-003");
        import_report["output_package"] =
            json!("target/mirante4d/benchmarks/import-sample/t5_qual_003.m4d");
        write_json_file(
            &source_root.join("bench-import-sample-t5_qual_003.json"),
            &import_report,
        )
        .unwrap();

        let mut native_report = promotable_report();
        native_report["benchmark"] = json!("bench-native-package");
        native_report["dataset_class"] = json!("imported_real_sample_package");
        native_report["package"] =
            json!("target/mirante4d/benchmarks/import-sample/t5_qual_003.m4d");
        write_json_file(
            &source_root.join("bench-native-package-t5_qual_003.json"),
            &native_report,
        )
        .unwrap();
        write_json_file(
            &baseline_dir.join("bench-native-package-t5_qual_003.json"),
            &stale_baseline_from(&native_report),
        )
        .unwrap();

        let (plan, manifest) =
            baseline_refresh_plan_report_json(&baseline_dir, &source_root).unwrap();

        assert!(manifest.is_some());
        assert_eq!(plan["entries"][0]["rerun"]["available"], true);
        assert_eq!(
            plan["entries"][0]["rerun"]["primary_command"]["shell_command"],
            "MIRANTE4D_BENCH_BASELINE_CLASS=local_gpu MIRANTE4D_BENCH_DATASET_CLASS=imported_real_sample_package MIRANTE4D_BENCH_HARDWARE_CLASS=hw2-linux-vulkan-reference MIRANTE4D_BENCH_HARDWARE_NAME=test-gpu MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1 cargo run --release -p xtask -- bench-native-package target/mirante4d/benchmarks/import-sample/t5_qual_003.m4d"
        );
        assert_eq!(
            plan["entries"][0]["rerun"]["primary_command"]["env"][HEAVY_ENV],
            "1"
        );
        assert_eq!(
            plan["entries"][0]["rerun"]["prerequisite_commands"][0]["shell_command"],
            "MIRANTE4D_BENCH_BASELINE_CLASS=local_gpu MIRANTE4D_BENCH_DATASET_CLASS=bounded_real_sample_import MIRANTE4D_BENCH_HARDWARE_CLASS=hw2-linux-vulkan-reference MIRANTE4D_BENCH_HARDWARE_NAME=test-gpu MIRANTE4D_SAMPLE_DATA=/samples cargo run --release -p xtask -- bench-import-sample T5-QUAL-003"
        );
    }

    #[test]
    fn heavy_rerun_command_uses_opt_in_even_for_local_gpu_baselines() {
        let command =
            rerun_command_for_signature(&refresh_signature_for("bench-phase11-interaction"))
                .unwrap();

        assert_eq!(command["env"][HEAVY_ENV], "1");
        assert!(
            command["shell_command"]
                .as_str()
                .unwrap()
                .contains("MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1")
        );
    }

    #[test]
    fn bounded_rerun_command_does_not_use_heavy_opt_in() {
        let mut signature = refresh_signature_for("bench-runtime-stress");
        signature.package = None;
        signature.dataset_class = Some("synthetic_runtime_stress".to_owned());
        let command = rerun_command_for_signature(&signature).unwrap();

        assert!(command["env"].get(HEAVY_ENV).is_none());
        assert!(
            !command["shell_command"]
                .as_str()
                .unwrap()
                .contains("MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK")
        );
    }

    #[test]
    fn refresh_plan_rejects_duplicate_selected_source_reports() {
        let tempdir = tempfile::tempdir().unwrap();
        let baseline_dir = tempdir.path().join("baselines");
        let source_root = tempdir.path().join("reports");
        fs::create_dir_all(&baseline_dir).unwrap();
        fs::create_dir_all(&source_root).unwrap();
        let source = source_root.join("bench-runtime-stress.json");
        let report = promotable_report();
        write_json_file(&source, &report).unwrap();
        write_json_file(
            &baseline_dir.join("runtime-a.json"),
            &stale_baseline_from(&report),
        )
        .unwrap();
        write_json_file(
            &baseline_dir.join("runtime-b.json"),
            &stale_baseline_from(&report),
        )
        .unwrap();

        let (plan, manifest) =
            baseline_refresh_plan_report_json(&baseline_dir, &source_root).unwrap();

        assert!(manifest.is_none());
        assert_eq!(plan["status"], "not_ready");
        assert_eq!(plan["summary"]["duplicate_selected_source_count"], 1);
        assert_eq!(
            plan["entries"][0]["status"],
            "duplicate_selected_source_report"
        );
        assert_eq!(
            plan["entries"][1]["status"],
            "duplicate_selected_source_report"
        );
    }
}

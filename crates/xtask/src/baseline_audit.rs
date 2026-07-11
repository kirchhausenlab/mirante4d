use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use serde_json::{Value, json};

use crate::reports::{read_json_file, write_json_file};

const BASELINE_DIR: &str = "docs/benchmarks/baselines";
const BASELINE_README: &str = "README.md";
const BASELINE_AUDIT_OUTPUT_DIR: &str = "target/mirante4d/baseline-audit";
const BASELINE_AUDIT_JSON: &str = "target/mirante4d/baseline-audit/baseline-audit-report.json";
const BASELINE_AUDIT_MD: &str = "target/mirante4d/baseline-audit/baseline-audit-report.md";
const BASELINE_AUDIT_SCHEMA: &str = "mirante4d-baseline-audit";
const BASELINE_AUDIT_SCHEMA_VERSION: u32 = 1;

pub(crate) const ACCEPTED_BASELINE_CLASSES: &[&str] =
    &["synthetic_ci", "local_gpu", "private_local_heavy"];
pub(crate) const ACCEPTED_BASELINE_STATUSES: &[&str] = &["current", "stale_timing_needs_refresh"];
pub(crate) const COMPATIBILITY_FIELDS: &[&str] = &[
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

pub(crate) fn baseline_audit() -> anyhow::Result<PathBuf> {
    fs::create_dir_all(BASELINE_AUDIT_OUTPUT_DIR)
        .with_context(|| format!("failed to create {BASELINE_AUDIT_OUTPUT_DIR}"))?;
    let report_path = Path::new(BASELINE_AUDIT_JSON);
    let markdown_path = Path::new(BASELINE_AUDIT_MD);
    let report = baseline_audit_report_json(Path::new(BASELINE_DIR))?;
    write_json_file(report_path, &report)?;
    fs::write(markdown_path, baseline_audit_markdown(&report))
        .with_context(|| format!("failed to write {}", markdown_path.display()))?;
    if report["status"] == "failed" {
        anyhow::bail!(
            "baseline audit found malformed or policy-invalid baselines; see {}",
            report_path.display()
        );
    }
    Ok(report_path.to_path_buf())
}

fn baseline_audit_report_json(baseline_dir: &Path) -> anyhow::Result<Value> {
    let entries = baseline_audit_entries(baseline_dir)?;
    let documentation = baseline_documentation_audit(baseline_dir, &entries);
    let entry_blocking_count = entries
        .iter()
        .filter(|entry| entry.get("blocking").and_then(Value::as_bool) == Some(true))
        .count();
    let documentation_blocking_count = documentation
        .get("blocking_count")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let blocking_count = entry_blocking_count + documentation_blocking_count;
    let needs_refresh_count = entries
        .iter()
        .filter(|entry| entry.get("needs_refresh").and_then(Value::as_bool) == Some(true))
        .count();
    let class_counts = baseline_class_counts(&entries);
    let status = if blocking_count > 0 {
        "failed"
    } else if needs_refresh_count > 0 {
        "needs_refresh"
    } else {
        "passed"
    };
    Ok(json!({
        "schema": BASELINE_AUDIT_SCHEMA,
        "schema_version": BASELINE_AUDIT_SCHEMA_VERSION,
        "command": "baseline-audit",
        "status": status,
        "started_at_epoch_ms": epoch_ms(),
        "baseline_dir": baseline_dir,
        "policy": {
            "accepted_baseline_classes": ACCEPTED_BASELINE_CLASSES,
            "compatibility_fields": COMPATIBILITY_FIELDS,
            "missing_baseline_class_is_blocking": false,
            "missing_baseline_class_status": "legacy_missing_baseline_class",
            "unknown_baseline_class_is_blocking": true,
            "accepted_baseline_statuses": ACCEPTED_BASELINE_STATUSES,
            "stale_timing_status": "stale_timing_needs_refresh",
            "readme_current_baseline_list_must_match_committed_json_files": true,
        },
        "summary": {
            "baseline_count": entries.len(),
            "blocking_count": blocking_count,
            "needs_refresh_count": needs_refresh_count,
            "baseline_class_counts": class_counts,
        },
        "documentation": documentation,
        "entries": entries,
    }))
}

fn baseline_audit_entries(baseline_dir: &Path) -> anyhow::Result<Vec<Value>> {
    if !baseline_dir.exists() {
        return Ok(vec![json!({
            "path": baseline_dir,
            "audit_status": "missing_baseline_directory",
            "blocking": true,
            "needs_refresh": false,
            "policy_action": "restore_or_create_baseline_directory",
        })]);
    }
    let mut paths = fs::read_dir(baseline_dir)
        .with_context(|| format!("failed to read {}", baseline_dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to list {}", baseline_dir.display()))?;
    paths.retain(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"));
    paths.sort();

    let mut entries = Vec::with_capacity(paths.len());
    for path in paths {
        entries.push(baseline_audit_entry(&path));
    }
    Ok(entries)
}

fn baseline_audit_entry(path: &Path) -> Value {
    let file_size_bytes = path.metadata().ok().map(|metadata| metadata.len());
    let value = match read_json_file(path) {
        Ok(value) => value,
        Err(err) => {
            return json!({
                "path": path,
                "file_size_bytes": file_size_bytes,
                "audit_status": "malformed_json",
                "blocking": true,
                "needs_refresh": false,
                "policy_action": "repair_or_remove_baseline_json",
                "reason": err.to_string(),
            });
        }
    };
    let benchmark = string_at_path(&value, "benchmark");
    let baseline_class = string_at_path(&value, "baseline_class");
    let baseline_status = string_at_path(&value, "baseline_status")
        .or_else(|| string_at_path(&value, "refresh_status"));
    let audit_status = baseline_audit_status(
        benchmark.as_deref(),
        baseline_class.as_deref(),
        baseline_status.as_deref(),
    );
    let blocking = matches!(
        audit_status,
        "missing_benchmark" | "unknown_baseline_class" | "unknown_baseline_status"
    );
    let needs_refresh = matches!(
        audit_status,
        "legacy_missing_baseline_class" | "stale_timing_needs_refresh"
    );
    json!({
        "path": path,
        "file_size_bytes": file_size_bytes,
        "audit_status": audit_status,
        "blocking": blocking,
        "needs_refresh": needs_refresh,
        "policy_action": baseline_policy_action(audit_status),
        "benchmark": benchmark,
        "baseline_class": baseline_class,
        "baseline_status": baseline_status.unwrap_or_else(|| "current".to_owned()),
        "hardware_class": string_at_path(&value, "hardware_class"),
        "dataset_class": string_at_path(&value, "dataset_class"),
        "benchmark_schema_version": string_at_path(&value, "benchmark_schema_version"),
        "schema_version": string_at_path(&value, "schema_version"),
        "scenario": string_at_path(&value, "scenario")
            .or_else(|| string_at_path(&value, "scenario.name"))
            .or_else(|| string_at_path(&value, "scenario.label")),
        "hardware_name": string_at_path(&value, "hardware.name").or_else(|| string_at_path(&value, "host.name")),
        "dataset_id": string_at_path(&value, "dataset.id"),
        "dataset_name": string_at_path(&value, "dataset.name"),
        "compatibility_fields": compatibility_field_status(&value),
    })
}

fn baseline_audit_status(
    benchmark: Option<&str>,
    baseline_class: Option<&str>,
    baseline_status: Option<&str>,
) -> &'static str {
    if benchmark.is_none() {
        return "missing_benchmark";
    }
    let class_status = match baseline_class {
        Some(class) if ACCEPTED_BASELINE_CLASSES.contains(&class) => "accepted",
        Some(_) => "unknown_baseline_class",
        None => "legacy_missing_baseline_class",
    };
    if class_status != "accepted" {
        return class_status;
    }
    match baseline_status.unwrap_or("current") {
        "current" => "current_policy_compliant",
        "stale_timing_needs_refresh" => "stale_timing_needs_refresh",
        _ => "unknown_baseline_status",
    }
}

fn baseline_policy_action(audit_status: &str) -> &'static str {
    match audit_status {
        "current_policy_compliant" => "compare_with_bench_check_when_context_matches",
        "stale_timing_needs_refresh" => "refresh_from_clean_worktree_before_using_as_hard_gate",
        "legacy_missing_baseline_class" => {
            "refresh_or_repromote_with_baseline_class_before_using_as_hard_gate"
        }
        "missing_benchmark" => "repair_or_remove_baseline_json",
        "unknown_baseline_class" => "use_one_of_the_accepted_baseline_classes",
        "unknown_baseline_status" => "use_one_of_the_accepted_baseline_statuses",
        "malformed_json" => "repair_or_remove_baseline_json",
        _ => "inspect_baseline_policy_status",
    }
}

fn compatibility_field_status(value: &Value) -> Value {
    let present = COMPATIBILITY_FIELDS
        .iter()
        .copied()
        .filter(|field| string_at_path(value, field).is_some())
        .collect::<Vec<_>>();
    let missing = COMPATIBILITY_FIELDS
        .iter()
        .copied()
        .filter(|field| string_at_path(value, field).is_none())
        .collect::<Vec<_>>();
    json!({
        "present": present,
        "missing": missing,
    })
}

fn baseline_class_counts(entries: &[Value]) -> Value {
    let mut counts = serde_json::Map::new();
    for entry in entries {
        let class = entry
            .get("baseline_class")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");
        let current = counts.get(class).and_then(Value::as_u64).unwrap_or(0);
        counts.insert(class.to_owned(), json!(current + 1));
    }
    Value::Object(counts)
}

fn baseline_documentation_audit(baseline_dir: &Path, entries: &[Value]) -> Value {
    let readme_path = baseline_dir.join(BASELINE_README);
    let existing = entries
        .iter()
        .filter(|entry| {
            entry
                .get("path")
                .and_then(Value::as_str)
                .and_then(|path| Path::new(path).file_name())
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".json"))
        })
        .filter_map(|entry| {
            entry
                .get("path")
                .and_then(Value::as_str)
                .and_then(|path| Path::new(path).file_name())
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        })
        .collect::<BTreeSet<_>>();

    let readme = match fs::read_to_string(&readme_path) {
        Ok(readme) => readme,
        Err(err) => {
            return json!({
                "path": readme_path,
                "audit_status": "missing_or_unreadable_readme",
                "blocking": true,
                "blocking_count": 1,
                "current_baseline_reference_count": 0,
                "missing_referenced_baselines": [],
                "unlisted_baselines": existing.into_iter().collect::<Vec<_>>(),
                "reason": err.to_string(),
            });
        }
    };

    let referenced = readme_current_baseline_references(&readme);
    let missing_referenced = referenced
        .difference(&existing)
        .cloned()
        .collect::<Vec<_>>();
    let unlisted = existing
        .difference(&referenced)
        .cloned()
        .collect::<Vec<_>>();
    let blocking_count = usize::from(!missing_referenced.is_empty() || !unlisted.is_empty());

    json!({
        "path": readme_path,
        "audit_status": if blocking_count == 0 {
            "readme_current_baseline_list_matches_files"
        } else {
            "readme_current_baseline_list_mismatch"
        },
        "blocking": blocking_count > 0,
        "blocking_count": blocking_count,
        "current_baseline_reference_count": referenced.len(),
        "missing_referenced_baselines": missing_referenced,
        "unlisted_baselines": unlisted,
    })
}

fn readme_current_baseline_references(readme: &str) -> BTreeSet<String> {
    let mut references = BTreeSet::new();
    let mut in_current_baselines = false;
    for line in readme.lines() {
        let trimmed = line.trim();
        if trimmed == "## Current Baselines" {
            in_current_baselines = true;
            continue;
        }
        if in_current_baselines && trimmed.starts_with("## ") {
            break;
        }
        if !in_current_baselines || !trimmed.starts_with("- `") {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("- `") else {
            continue;
        };
        let Some(name) = rest.split('`').next() else {
            continue;
        };
        if name.ends_with(".json") && !name.contains('/') && !name.contains('\\') {
            references.insert(name.to_owned());
        }
    }
    references
}

fn string_at_path(value: &Value, path: &str) -> Option<String> {
    let value = value.get(path).or_else(|| {
        path.split('.')
            .try_fold(value, |current, segment| current.get(segment))
    })?;
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn baseline_audit_markdown(report: &Value) -> String {
    let summary = &report["summary"];
    let mut markdown = String::new();
    markdown.push_str("# Baseline Audit\n\n");
    markdown.push_str(&format!(
        "- status: `{}`\n",
        report["status"].as_str().unwrap_or("unknown")
    ));
    markdown.push_str(&format!(
        "- baseline count: `{}`\n",
        summary["baseline_count"].as_u64().unwrap_or(0)
    ));
    markdown.push_str(&format!(
        "- needs refresh: `{}`\n",
        summary["needs_refresh_count"].as_u64().unwrap_or(0)
    ));
    markdown.push_str(&format!(
        "- blocking: `{}`\n\n",
        summary["blocking_count"].as_u64().unwrap_or(0)
    ));
    if let Some(documentation) = report.get("documentation") {
        markdown.push_str(&format!(
            "- documentation: `{}`\n",
            documentation["audit_status"].as_str().unwrap_or("unknown")
        ));
        markdown.push_str(&format!(
            "- documented current baselines: `{}`\n\n",
            documentation["current_baseline_reference_count"]
                .as_u64()
                .unwrap_or(0)
        ));
    }
    markdown.push_str("| Baseline | Status | Class | Action |\n");
    markdown.push_str("|---|---|---|---|\n");
    if let Some(entries) = report["entries"].as_array() {
        for entry in entries {
            markdown.push_str(&format!(
                "| `{}` | `{}` | `{}` | `{}` |\n",
                entry["path"].as_str().unwrap_or("<unknown>"),
                entry["audit_status"].as_str().unwrap_or("unknown"),
                entry["baseline_class"].as_str().unwrap_or("<missing>"),
                entry["policy_action"].as_str().unwrap_or("inspect"),
            ));
        }
    }
    markdown
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

    fn write_readme(baseline_dir: &Path, names: &[&str]) {
        let mut readme = String::from("# Benchmark Baselines\n\n## Current Baselines\n\n");
        for name in names {
            readme.push_str(&format!("- `{name}` - test baseline\n"));
        }
        readme.push_str("\n## 2026-06-24 Local Rerun Evidence\n");
        fs::write(baseline_dir.join(BASELINE_README), readme).unwrap();
    }

    #[test]
    fn baseline_audit_marks_legacy_missing_class_as_refresh_needed() {
        let tempdir = tempfile::tempdir().unwrap();
        fs::write(
            tempdir.path().join("legacy.json"),
            serde_json::to_vec(&json!({
                "benchmark": "bench-smoke",
                "benchmark_schema_version": 1,
                "hardware": {
                    "name": "local"
                },
                "timings_ms": {
                    "total": 1.0
                }
            }))
            .unwrap(),
        )
        .unwrap();
        write_readme(tempdir.path(), &["legacy.json"]);

        let report = baseline_audit_report_json(tempdir.path()).unwrap();

        assert_eq!(report["schema"], BASELINE_AUDIT_SCHEMA);
        assert_eq!(report["status"], "needs_refresh");
        assert_eq!(report["summary"]["baseline_count"], 1);
        assert_eq!(report["summary"]["needs_refresh_count"], 1);
        assert_eq!(
            report["documentation"]["audit_status"],
            "readme_current_baseline_list_matches_files"
        );
        assert_eq!(
            report["entries"][0]["audit_status"],
            "legacy_missing_baseline_class"
        );
        assert_eq!(report["entries"][0]["blocking"], false);
        assert_eq!(report["entries"][0]["needs_refresh"], true);
    }

    #[test]
    fn baseline_audit_fails_unknown_baseline_class() {
        let tempdir = tempfile::tempdir().unwrap();
        fs::write(
            tempdir.path().join("bad-class.json"),
            serde_json::to_vec(&json!({
                "benchmark": "bench-smoke",
                "baseline_class": "mystery_machine",
                "timings_ms": {
                    "total": 1.0
                }
            }))
            .unwrap(),
        )
        .unwrap();
        write_readme(tempdir.path(), &["bad-class.json"]);

        let report = baseline_audit_report_json(tempdir.path()).unwrap();

        assert_eq!(report["status"], "failed");
        assert_eq!(report["summary"]["blocking_count"], 1);
        assert_eq!(
            report["entries"][0]["audit_status"],
            "unknown_baseline_class"
        );
        assert_eq!(
            report["entries"][0]["policy_action"],
            "use_one_of_the_accepted_baseline_classes"
        );
    }

    #[test]
    fn baseline_audit_accepts_current_policy_baseline() {
        let tempdir = tempfile::tempdir().unwrap();
        fs::write(
            tempdir.path().join("current.json"),
            serde_json::to_vec(&json!({
                "benchmark": "bench-smoke",
                "benchmark_schema_version": 1,
                "baseline_class": "synthetic_ci",
                "hardware_class": "hosted_cpu",
                "dataset_class": "generated_fixture",
                "timings_ms": {
                    "total": 1.0
                }
            }))
            .unwrap(),
        )
        .unwrap();
        write_readme(tempdir.path(), &["current.json"]);

        let report = baseline_audit_report_json(tempdir.path()).unwrap();

        assert_eq!(report["status"], "passed");
        assert_eq!(
            report["summary"]["baseline_class_counts"]["synthetic_ci"],
            1
        );
        assert_eq!(
            report["entries"][0]["audit_status"],
            "current_policy_compliant"
        );
        assert_eq!(
            report["entries"][0]["compatibility_fields"]["present"],
            json!([
                "benchmark_schema_version",
                "hardware_class",
                "baseline_class",
                "dataset_class"
            ])
        );
    }

    #[test]
    fn baseline_audit_marks_stale_timing_status_as_refresh_needed() {
        let tempdir = tempfile::tempdir().unwrap();
        fs::write(
            tempdir.path().join("stale.json"),
            serde_json::to_vec(&json!({
                "benchmark": "bench-runtime-stress",
                "baseline_class": "local_gpu",
                "baseline_status": "stale_timing_needs_refresh",
                "hardware_class": "hw2-linux-vulkan-reference",
                "dataset_class": "synthetic_runtime_stress",
                "timings_ms": {
                    "total": 1.0
                }
            }))
            .unwrap(),
        )
        .unwrap();
        write_readme(tempdir.path(), &["stale.json"]);

        let report = baseline_audit_report_json(tempdir.path()).unwrap();

        assert_eq!(report["status"], "needs_refresh");
        assert_eq!(report["summary"]["needs_refresh_count"], 1);
        assert_eq!(
            report["entries"][0]["audit_status"],
            "stale_timing_needs_refresh"
        );
        assert_eq!(
            report["entries"][0]["baseline_status"],
            "stale_timing_needs_refresh"
        );
        assert_eq!(
            report["entries"][0]["policy_action"],
            "refresh_from_clean_worktree_before_using_as_hard_gate"
        );
        assert_eq!(report["entries"][0]["blocking"], false);
    }

    #[test]
    fn baseline_audit_fails_readme_current_baseline_mismatch() {
        let tempdir = tempfile::tempdir().unwrap();
        fs::write(
            tempdir.path().join("current.json"),
            serde_json::to_vec(&json!({
                "benchmark": "bench-smoke",
                "baseline_class": "synthetic_ci",
                "timings_ms": {
                    "total": 1.0
                }
            }))
            .unwrap(),
        )
        .unwrap();
        write_readme(tempdir.path(), &["current.json", "missing.json"]);

        let report = baseline_audit_report_json(tempdir.path()).unwrap();

        assert_eq!(report["status"], "failed");
        assert_eq!(report["summary"]["blocking_count"], 1);
        assert_eq!(
            report["documentation"]["audit_status"],
            "readme_current_baseline_list_mismatch"
        );
        assert_eq!(
            report["documentation"]["missing_referenced_baselines"],
            json!(["missing.json"])
        );
    }

    #[test]
    fn readme_current_baseline_references_stop_at_next_section() {
        let references = readme_current_baseline_references(
            "# Benchmark Baselines\n\
             \n\
             ## Current Baselines\n\
             \n\
             - `current.json` - current baseline\n\
             \n\
             ## 2026-06-24 Local Rerun Evidence\n\
             \n\
             `target/mirante4d/benchmarks/current.json`\n\
             `future.json`\n",
        );

        assert_eq!(references, BTreeSet::from(["current.json".to_owned()]));
    }
}

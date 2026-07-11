use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use serde_json::{Value, json};
use yaml_rust2::YamlLoader;

use crate::reports::write_json_file;

const OUTPUT_DIR: &str = "target/mirante4d/workflow-audit";
const REPORT_JSON: &str = "target/mirante4d/workflow-audit/workflow-audit-report.json";
const REPORT_MD: &str = "target/mirante4d/workflow-audit/workflow-audit-report.md";
const CHECKOUT: &str = "actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683";

#[derive(Clone, Copy)]
struct Spec {
    path: &'static str,
    name: &'static str,
    jobs: &'static [(&'static str, &'static str, u64, &'static str, usize)],
    required: &'static [&'static str],
    forbidden: &'static [&'static str],
}

const COMMON_FORBIDDEN: &[&str] = &[
    "pull_request_target",
    "workflow_run:",
    "schedule:",
    "secrets.",
    "github.token",
    "GH_TOKEN",
    "actions/cache@",
    "actions/upload-artifact@",
    "actions/download-artifact@",
    "self-hosted",
    "runner.temp",
    ": write",
    "continue-on-error:",
    "  paths:",
    "  paths-ignore:",
    "    needs:",
    "    strategy:",
    "    services:",
    "    container:",
    "    environment:",
];

const BOOTSTRAP_FORBIDDEN: &[&str] = &[
    "pull_request_target",
    "workflow_run:",
    "  push:",
    "schedule:",
    "secrets.",
    "github.token",
    "GH_TOKEN",
    "actions/cache@",
    "actions/upload-artifact@",
    "actions/download-artifact@",
    "self-hosted",
    ": write",
    "continue-on-error:",
    "  paths:",
    "  paths-ignore:",
    "    needs:",
    "    strategy:",
    "    services:",
    "    container:",
    "    environment:",
];

const SPECS: &[Spec] = &[
    Spec {
        path: ".github/workflows/bootstrap.yml",
        name: "Bootstrap",
        jobs: &[(
            "bootstrap",
            "Bootstrap / required",
            15,
            "cargo xtask verify-bootstrap",
            6,
        )],
        required: &[
            "  pull_request:",
            "  workflow_dispatch:",
            "cancel-in-progress: true",
            "Guard the public-root bootstrap dispatch",
            "3793bf0c27607b196f502c39b2108f571de89fcda7586ae6beefa11ee177b216",
            "eb51e28ef9dff2b2d29b4527bc40123e840bb997dc8bae39d99496b898ee9f72",
        ],
        forbidden: BOOTSTRAP_FORBIDDEN,
    },
    Spec {
        path: ".github/workflows/pr.yml",
        name: "PR",
        jobs: &[
            (
                "policy",
                "PR / policy",
                6,
                "cargo xtask verify-pr policy",
                6,
            ),
            ("rust", "PR / rust", 15, "cargo xtask verify-pr rust", 7),
        ],
        required: &[
            "  pull_request:",
            "group: pr-${{ github.event.pull_request.number }}",
            "cancel-in-progress: true",
            "NEXTEST_USER_CONFIG_FILE: none",
            "Isolate Cargo state",
            "CARGO_HOME=${RUNNER_TEMP}/cargo-home-policy",
            "CARGO_HOME=${RUNNER_TEMP}/cargo-home-rust",
            "CARGO_TARGET_DIR=${RUNNER_TEMP}/target-policy",
            "CARGO_TARGET_DIR=${RUNNER_TEMP}/target-rust",
            "3793bf0c27607b196f502c39b2108f571de89fcda7586ae6beefa11ee177b216",
            "eb51e28ef9dff2b2d29b4527bc40123e840bb997dc8bae39d99496b898ee9f72",
            "9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f",
        ],
        forbidden: COMMON_FORBIDDEN,
    },
    Spec {
        path: ".github/workflows/main.yml",
        name: "Main",
        jobs: &[
            (
                "policy",
                "Main / policy",
                6,
                "cargo xtask verify-pr policy",
                7,
            ),
            ("rust", "Main / rust", 15, "cargo xtask verify-pr rust", 8),
        ],
        required: &[
            "  push:",
            "      - main",
            "  workflow_dispatch:",
            "test \"${GITHUB_REF}\" = \"refs/heads/main\"",
            "NEXTEST_USER_CONFIG_FILE: none",
            "Isolate Cargo state",
            "CARGO_HOME=${RUNNER_TEMP}/cargo-home-policy",
            "CARGO_HOME=${RUNNER_TEMP}/cargo-home-rust",
            "CARGO_TARGET_DIR=${RUNNER_TEMP}/target-policy",
            "CARGO_TARGET_DIR=${RUNNER_TEMP}/target-rust",
            "3793bf0c27607b196f502c39b2108f571de89fcda7586ae6beefa11ee177b216",
            "eb51e28ef9dff2b2d29b4527bc40123e840bb997dc8bae39d99496b898ee9f72",
            "9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f",
        ],
        forbidden: COMMON_FORBIDDEN,
    },
];

pub(crate) fn workflow_audit() -> anyhow::Result<PathBuf> {
    let output_dir = repo_path(OUTPUT_DIR);
    fs::create_dir_all(&output_dir).with_context(|| format!("failed to create {OUTPUT_DIR}"))?;
    let mut entries = SPECS.iter().map(audit_one).collect::<Vec<_>>();
    entries.extend(audit_inventory());
    let blocking = entries
        .iter()
        .filter(|entry| entry["blocking"] == true)
        .count();
    let report = json!({
        "schema": "mirante4d-workflow-audit",
        "schema_version": 1,
        "command": "workflow-audit",
        "status": if blocking == 0 { "passed" } else { "failed" },
        "started_at_epoch_ms": epoch_ms(),
        "summary": { "workflow_count": entries.len(), "blocking_count": blocking },
        "entries": entries,
    });
    let report_path = repo_path(REPORT_JSON);
    write_json_file(&report_path, &report)?;
    fs::write(
        repo_path(REPORT_MD),
        format!(
            "# Workflow audit\n\nStatus: `{}`\n\nBlocking findings: `{blocking}`\n",
            report["status"].as_str().unwrap_or("failed")
        ),
    )?;
    if blocking != 0 {
        bail!("workflow audit found {blocking} blocking gaps; see {REPORT_JSON}");
    }
    Ok(report_path)
}

fn audit_one(spec: &Spec) -> Value {
    let path = repo_path(spec.path);
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) => return finding(spec.path, vec![format!("unreadable: {error}")]),
    };
    let mut failures = Vec::new();
    match YamlLoader::load_from_str(&content) {
        Ok(documents) => {
            let job_count = documents
                .first()
                .and_then(|document| document["jobs"].as_hash())
                .map_or(0, |jobs| jobs.len());
            if job_count != spec.jobs.len() {
                failures.push(format!(
                    "expected exactly {} top-level jobs, found {job_count}",
                    spec.jobs.len()
                ));
            }
            if let Some(document) = documents.first() {
                for (id, _, _, _, expected_steps) in spec.jobs {
                    let actual_steps = document["jobs"][*id]["steps"].as_vec().map_or(0, Vec::len);
                    if actual_steps != *expected_steps {
                        failures.push(format!(
                            "job {id:?} expected {expected_steps} steps, found {actual_steps}"
                        ));
                    }
                }
            }
        }
        Err(_) => failures.push("invalid YAML".to_owned()),
    }
    if !content.starts_with(&format!("name: {}\n", spec.name)) {
        failures.push(format!("workflow name is not {:?}", spec.name));
    }
    for required in ["permissions:\n  contents: read", CHECKOUT] {
        if !content.contains(required) {
            failures.push(format!("missing {required:?}"));
        }
    }
    for required in spec.required {
        if !content.contains(required) {
            failures.push(format!("missing {required:?}"));
        }
    }
    for (id, name, timeout, command, _) in spec.jobs {
        for required in [
            format!("  {id}:\n"),
            format!("name: {name}"),
            "runs-on: ubuntu-24.04".to_owned(),
            format!("timeout-minutes: {timeout}"),
            (*command).to_owned(),
        ] {
            if !content.contains(&required) {
                failures.push(format!("job {id:?} missing {required:?}"));
            }
        }
    }
    let uses = content
        .lines()
        .filter_map(|line| line.trim().strip_prefix("uses: "))
        .collect::<Vec<_>>();
    if uses.len() != spec.jobs.len() || uses.iter().any(|action| *action != CHECKOUT) {
        failures.push("uses steps are not exactly one pinned checkout per job".to_owned());
    }
    for required in ["persist-credentials: false", "fetch-depth: 0"] {
        if content.matches(required).count() != spec.jobs.len() {
            failures.push(format!("{required:?} is not present exactly once per job"));
        }
    }
    if spec.name == "Main"
        && content
            .matches("test \"${GITHUB_REF}\" = \"refs/heads/main\"")
            .count()
            != spec.jobs.len()
    {
        failures.push("protected-main guard is not present exactly once per job".to_owned());
    }
    for forbidden in spec.forbidden {
        if content.contains(forbidden) {
            failures.push(format!("contains forbidden fragment {forbidden:?}"));
        }
    }
    if content.lines().any(|line| line.starts_with("    if:")) {
        failures.push("contains a job-level condition".to_owned());
    }
    if spec.name != "Bootstrap"
        && content
            .lines()
            .any(|line| line.trim_start().starts_with("if:"))
    {
        failures.push("contains a conditional step".to_owned());
    }
    finding(spec.path, failures)
}

fn audit_inventory() -> Vec<Value> {
    let expected = SPECS.iter().map(|spec| spec.path).collect::<BTreeSet<_>>();
    let directory = repo_path(".github/workflows");
    let actual = match fs::read_dir(&directory) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                (name.ends_with(".yml") || name.ends_with(".yaml"))
                    .then(|| format!(".github/workflows/{name}"))
            })
            .collect::<BTreeSet<_>>(),
        Err(error) => {
            return vec![finding(
                ".github/workflows",
                vec![format!("unreadable: {error}")],
            )];
        }
    };
    let actual_refs = actual.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if actual_refs == expected {
        Vec::new()
    } else {
        vec![finding(
            ".github/workflows",
            vec![format!("expected {expected:?}, found {actual_refs:?}")],
        )]
    }
}

fn finding(path: &str, failures: Vec<String>) -> Value {
    json!({
        "path": path,
        "blocking": !failures.is_empty(),
        "audit_status": if failures.is_empty() { "workflow_policy_compliant" } else { "workflow_policy_gap" },
        "failures": failures,
    })
}

fn repo_path(relative: &str) -> PathBuf {
    let cwd = PathBuf::from(relative);
    if cwd.exists() {
        cwd
    } else {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(relative)
    }
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

    #[test]
    fn workflow_audit_covers_current_workflow_surface() {
        workflow_audit().unwrap();
    }

    #[test]
    fn workflow_inventory_is_exact() {
        assert!(audit_inventory().is_empty());
    }
}

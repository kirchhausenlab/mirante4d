use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use serde_json::{Value, json};
use yaml_rust2::{Yaml, YamlLoader};

use crate::reports::write_json_file;

const WORKFLOW_AUDIT_OUTPUT_DIR: &str = "target/mirante4d/workflow-audit";
const WORKFLOW_AUDIT_JSON: &str = "target/mirante4d/workflow-audit/workflow-audit-report.json";
const WORKFLOW_AUDIT_MD: &str = "target/mirante4d/workflow-audit/workflow-audit-report.md";
const WORKFLOW_AUDIT_SCHEMA: &str = "mirante4d-workflow-audit";
const WORKFLOW_AUDIT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy)]
struct WorkflowSpec {
    path: &'static str,
    evidence_role: &'static str,
    expected_name: &'static str,
    required_triggers: &'static [&'static str],
    required_jobs: &'static [WorkflowJobSpec],
    forbidden_fragments: &'static [WorkflowFragment],
}

#[derive(Debug, Clone, Copy)]
struct WorkflowJobSpec {
    id: &'static str,
    name: &'static str,
    runs_on: RunsOnSpec,
    required_run_commands: &'static [&'static str],
    required_artifact_paths: &'static [&'static str],
    required_matrix_os: &'static [&'static str],
}

#[derive(Debug, Clone, Copy)]
enum RunsOnSpec {
    Exact(&'static str),
}

#[derive(Debug, Clone, Copy)]
struct WorkflowFragment {
    label: &'static str,
    text: &'static str,
}

const BOOTSTRAP_FORBIDDEN: &[WorkflowFragment] = &[
    WorkflowFragment {
        label: "no_private_sample_data_env",
        text: "MIRANTE4D_SAMPLE_DATA",
    },
    WorkflowFragment {
        label: "no_private_qualification_path",
        text: "/private-qualification",
    },
    WorkflowFragment {
        label: "no_pull_request_target",
        text: "pull_request_target",
    },
    WorkflowFragment {
        label: "no_privileged_workflow_run",
        text: "workflow_run:",
    },
    WorkflowFragment {
        label: "no_push_trigger",
        text: "  push:",
    },
    WorkflowFragment {
        label: "no_schedule_trigger",
        text: "  schedule:",
    },
    WorkflowFragment {
        label: "no_secrets",
        text: "secrets.",
    },
    WorkflowFragment {
        label: "no_implicit_github_token",
        text: "github.token",
    },
    WorkflowFragment {
        label: "no_explicit_gh_token",
        text: "GH_TOKEN",
    },
    WorkflowFragment {
        label: "no_cache_action",
        text: "actions/cache@",
    },
    WorkflowFragment {
        label: "no_artifact_upload",
        text: "actions/upload-artifact@",
    },
    WorkflowFragment {
        label: "no_artifact_download",
        text: "actions/download-artifact@",
    },
    WorkflowFragment {
        label: "no_self_hosted_runner",
        text: "self-hosted",
    },
    WorkflowFragment {
        label: "no_write_permission",
        text: ": write",
    },
    WorkflowFragment {
        label: "no_external_reusable_workflow",
        text: "uses: ./.github/workflows/",
    },
];

const BOOTSTRAP_JOBS: &[WorkflowJobSpec] = &[WorkflowJobSpec {
    id: "bootstrap",
    name: "Bootstrap / required",
    runs_on: RunsOnSpec::Exact("ubuntu-24.04"),
    required_run_commands: &[
        "test \"${GITHUB_REF}\" = \"refs/heads/main\"",
        "test \"${GITHUB_SHA}\" = \"${tag_commit}\"",
        "test \"$(sha256sum rust-toolchain.toml | cut -d ' ' -f1)\" = \"4d0674575b116a100c7d0ad754308dc6dea178b4808c760afc43136db967c7a6\"",
        "echo \"87eb76c53073e72b766083bed5530820694253b832a762d8385bda5759f03975  ${RUNNER_TEMP}/channel-rust-1.96.1.toml\" | sha256sum --check --strict",
        "test \"$(rustc --version --verbose | sed -n 's/^commit-hash: //p')\" = \"31fca3adb283cc9dfd56b49cdee9a96eb9c96ffd\"",
        "test \"$(cargo --version --verbose | sed -n 's/^commit-hash: //p')\" = \"356927216a2d746168cf76e5e88cc3f4b58e029d\"",
        "echo \"3793bf0c27607b196f502c39b2108f571de89fcda7586ae6beefa11ee177b216  ${RUNNER_TEMP}/cargo-nextest.tar.gz\" | sha256sum --check --strict",
        "echo \"eb51e28ef9dff2b2d29b4527bc40123e840bb997dc8bae39d99496b898ee9f72  ${RUNNER_TEMP}/rumdl.tar.gz\" | sha256sum --check --strict",
        "cargo xtask verify-bootstrap",
    ],
    required_artifact_paths: &[],
    required_matrix_os: &[],
}];

const WORKFLOW_SPECS: &[WorkflowSpec] = &[WorkflowSpec {
    path: ".github/workflows/bootstrap.yml",
    evidence_role: "temporary_public_source_bootstrap",
    expected_name: "Bootstrap",
    required_triggers: &["pull_request", "workflow_dispatch"],
    required_jobs: BOOTSTRAP_JOBS,
    forbidden_fragments: BOOTSTRAP_FORBIDDEN,
}];

pub(crate) fn workflow_audit() -> anyhow::Result<PathBuf> {
    fs::create_dir_all(WORKFLOW_AUDIT_OUTPUT_DIR)
        .with_context(|| format!("failed to create {WORKFLOW_AUDIT_OUTPUT_DIR}"))?;
    let report_path = Path::new(WORKFLOW_AUDIT_JSON);
    let markdown_path = Path::new(WORKFLOW_AUDIT_MD);
    let report = workflow_audit_report_json();
    write_json_file(report_path, &report)?;
    fs::write(markdown_path, workflow_audit_markdown(&report))
        .with_context(|| format!("failed to write {}", markdown_path.display()))?;
    if report["status"] == "failed" {
        anyhow::bail!(
            "workflow audit found blocking CI workflow policy gaps; see {}",
            report_path.display()
        );
    }
    Ok(report_path.to_path_buf())
}

fn workflow_audit_report_json() -> Value {
    let mut entries = WORKFLOW_SPECS
        .iter()
        .map(workflow_audit_entry)
        .collect::<Vec<_>>();
    entries.extend(unexpected_workflow_entries());
    let blocking_count = entries
        .iter()
        .filter(|entry| entry["blocking"].as_bool().unwrap_or(false))
        .count();
    json!({
        "schema": WORKFLOW_AUDIT_SCHEMA,
        "schema_version": WORKFLOW_AUDIT_SCHEMA_VERSION,
        "command": "workflow-audit",
        "status": if blocking_count == 0 { "passed" } else { "failed" },
        "started_at_epoch_ms": epoch_ms(),
        "summary": {
            "workflow_count": entries.len(),
            "blocking_count": blocking_count,
            "external_run_evidence": "not_checked_by_static_workflow_audit",
        },
        "entries": entries,
    })
}

fn unexpected_workflow_entries() -> Vec<Value> {
    let directory = workflow_file_path(".github/workflows");
    let read_dir = match fs::read_dir(&directory) {
        Ok(read_dir) => read_dir,
        Err(err) => {
            return vec![json!({
                "path": ".github/workflows",
                "evidence_role": "sole_workflow_inventory",
                "presence": "missing_or_unreadable",
                "audit_status": "workflow_directory_unreadable",
                "blocking": true,
                "required": [],
                "forbidden": [],
                "reason": err.to_string(),
            })];
        }
    };
    let mut paths = read_dir
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let extension = path.extension()?.to_str()?;
            if extension != "yml" && extension != "yaml" {
                return None;
            }
            let name = path.file_name()?.to_str()?;
            let relative = format!(".github/workflows/{name}");
            (relative != ".github/workflows/bootstrap.yml").then_some(relative)
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            json!({
                "path": path,
                "evidence_role": "sole_workflow_inventory",
                "presence": "present",
                "audit_status": "unexpected_workflow_file",
                "blocking": true,
                "required": [],
                "forbidden": [],
                "reason": "WP-03 permits exactly .github/workflows/bootstrap.yml",
            })
        })
        .collect()
}

fn workflow_audit_entry(spec: &WorkflowSpec) -> Value {
    let path = workflow_file_path(spec.path);
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) => {
            return json!({
                "path": spec.path,
                "evidence_role": spec.evidence_role,
                "presence": "missing_or_unreadable",
                "audit_status": "missing_workflow_file",
                "blocking": true,
                "required": [],
                "forbidden": [],
                "reason": err.to_string(),
            });
        }
    };
    let workflow = match parse_workflow_yaml(&content) {
        Ok(workflow) => workflow,
        Err(err) => {
            return json!({
                "path": spec.path,
                "evidence_role": spec.evidence_role,
                "presence": "present",
                "audit_status": "workflow_yaml_parse_error",
                "blocking": true,
                "required": [],
                "forbidden": [],
                "reason": err,
            });
        }
    };
    let required = workflow_policy_checks(spec, &workflow);
    let forbidden = fragment_presence(spec.forbidden_fragments, &content, false);
    let missing_required = required
        .iter()
        .filter(|entry| entry["ok"].as_bool() == Some(false))
        .count();
    let present_forbidden = forbidden
        .iter()
        .filter(|entry| entry["ok"].as_bool() == Some(false))
        .count();
    let blocking = missing_required > 0 || present_forbidden > 0;
    json!({
        "path": spec.path,
        "evidence_role": spec.evidence_role,
        "presence": "present",
        "audit_status": if blocking { "workflow_policy_gap" } else { "workflow_policy_compliant" },
        "blocking": blocking,
        "required": required,
        "forbidden": forbidden,
    })
}

fn parse_workflow_yaml(content: &str) -> Result<Yaml, String> {
    let documents = YamlLoader::load_from_str(content).map_err(|err| err.to_string())?;
    documents
        .into_iter()
        .next()
        .ok_or_else(|| "workflow YAML did not contain a document".to_owned())
}

fn workflow_policy_checks(spec: &WorkflowSpec, workflow: &Yaml) -> Vec<Value> {
    let mut checks = Vec::new();
    push_check(
        &mut checks,
        "workflow_name",
        spec.expected_name,
        yaml_get(workflow, "name").and_then(Yaml::as_str),
    );
    let trigger_mapping = yaml_get(workflow, "on").and_then(Yaml::as_hash);
    let trigger_count = trigger_mapping.map_or(0, |mapping| mapping.len());
    checks.push(json!({
        "label": "trigger_count",
        "kind": "trigger_count",
        "expected": 2,
        "actual": trigger_count,
        "ok": trigger_count == 2,
    }));
    let pull_request_config = yaml_get(workflow, "on").and_then(|on| yaml_get(on, "pull_request"));
    checks.push(json!({
        "label": "trigger:pull_request:unfiltered",
        "kind": "unfiltered_trigger",
        "expected": "null configuration",
        "actual": format!("{pull_request_config:?}"),
        "ok": matches!(pull_request_config, Some(Yaml::Null)),
    }));
    let permissions = yaml_get(workflow, "permissions").and_then(Yaml::as_hash);
    let contents_permission = yaml_get(workflow, "permissions")
        .and_then(|permissions| yaml_get(permissions, "contents"))
        .and_then(Yaml::as_str);
    checks.push(json!({
        "label": "permissions:contents_read_only",
        "kind": "permissions",
        "expected": {"contents": "read"},
        "actual": contents_permission,
        "ok": permissions.map_or(false, |permissions| permissions.len() == 1)
            && contents_permission == Some("read"),
    }));
    push_check(
        &mut checks,
        "concurrency:group",
        "bootstrap-${{ github.event.pull_request.number || github.ref }}",
        yaml_get(workflow, "concurrency")
            .and_then(|concurrency| yaml_get(concurrency, "group"))
            .and_then(Yaml::as_str),
    );
    let cancel_in_progress = yaml_get(workflow, "concurrency")
        .and_then(|concurrency| yaml_get(concurrency, "cancel-in-progress"))
        .and_then(Yaml::as_bool);
    checks.push(json!({
        "label": "concurrency:cancel_in_progress",
        "kind": "boolean",
        "expected": true,
        "actual": cancel_in_progress,
        "ok": cancel_in_progress == Some(true),
    }));
    for trigger in spec.required_triggers {
        checks.push(json!({
            "label": format!("trigger:{trigger}"),
            "kind": "trigger",
            "expected": trigger,
            "actual": workflow_has_trigger(workflow, trigger),
            "ok": workflow_has_trigger(workflow, trigger),
        }));
    }
    let jobs = yaml_get(workflow, "jobs");
    let job_count = jobs
        .and_then(Yaml::as_hash)
        .map_or(0, |mapping| mapping.len());
    checks.push(json!({
        "label": "job_count",
        "kind": "job_count",
        "expected": 1,
        "actual": job_count,
        "ok": job_count == 1,
    }));
    for job_spec in spec.required_jobs {
        let job = jobs.and_then(|jobs| yaml_get(jobs, job_spec.id));
        checks.push(json!({
            "label": format!("job:{}:present", job_spec.id),
            "kind": "job_present",
            "expected": job_spec.id,
            "actual": job.is_some(),
            "ok": job.is_some(),
        }));
        let Some(job) = job else {
            continue;
        };
        push_check(
            &mut checks,
            &format!("job:{}:name", job_spec.id),
            job_spec.name,
            yaml_get(job, "name").and_then(Yaml::as_str),
        );
        checks.push(json!({
            "label": format!("job:{}:runs_on", job_spec.id),
            "kind": "runs_on",
            "expected": runs_on_expected_json(job_spec.runs_on),
            "actual": runs_on_actual_json(yaml_get(job, "runs-on")),
            "ok": runs_on_matches(yaml_get(job, "runs-on"), job_spec.runs_on),
        }));
        let actual_if = yaml_get(job, "if").and_then(Yaml::as_str);
        checks.push(json!({
            "label": format!("job:{}:unconditional", job_spec.id),
            "kind": "job_condition",
            "expected": null,
            "actual": actual_if,
            "ok": actual_if.is_none(),
        }));
        let timeout_minutes = yaml_get(job, "timeout-minutes").and_then(Yaml::as_i64);
        checks.push(json!({
            "label": format!("job:{}:timeout", job_spec.id),
            "kind": "timeout_minutes",
            "expected": 15,
            "actual": timeout_minutes,
            "ok": timeout_minutes == Some(15),
        }));
        let job_permission_override = yaml_get(job, "permissions");
        checks.push(json!({
            "label": format!("job:{}:permission_override", job_spec.id),
            "kind": "permissions",
            "expected": null,
            "actual": format!("{job_permission_override:?}"),
            "ok": job_permission_override.is_none(),
        }));
        for forbidden_key in ["container", "environment", "needs", "services", "strategy"] {
            let present = yaml_get(job, forbidden_key).is_some();
            checks.push(json!({
                "label": format!("job:{}:no_{forbidden_key}", job_spec.id),
                "kind": "job_surface",
                "expected": false,
                "actual": present,
                "ok": !present,
            }));
        }
        let expected_step_names = [
            "Check out the candidate revision",
            "Guard the public-root bootstrap dispatch",
            "Verify the pinned Rust manifest and compiler commits",
            "Install checksum-bound bootstrap tools",
            "Install Linux build libraries",
            "Run the temporary local foundation check",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        let step_names = job_step_names(job);
        let step_names_ok = step_names == expected_step_names;
        checks.push(json!({
            "label": format!("job:{}:exact_steps", job_spec.id),
            "kind": "step_inventory",
            "expected": expected_step_names,
            "actual": step_names,
            "ok": step_names_ok,
        }));
        let uses = job_uses_steps(job);
        let uses_ok = uses.len() == 1
            && uses[0] == "actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683";
        checks.push(json!({
            "label": format!("job:{}:actions", job_spec.id),
            "kind": "actions",
            "expected": ["actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683"],
            "actual": uses,
            "ok": uses_ok,
        }));
        let checkout_step = yaml_get(job, "steps")
            .and_then(Yaml::as_vec)
            .into_iter()
            .flatten()
            .find(|step| {
                yaml_get(step, "uses").and_then(Yaml::as_str)
                    == Some("actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683")
            });
        let checkout_with = checkout_step.and_then(|step| yaml_get(step, "with"));
        let persist_credentials = checkout_with
            .and_then(|with| yaml_get(with, "persist-credentials"))
            .and_then(Yaml::as_bool);
        let fetch_depth = checkout_with
            .and_then(|with| yaml_get(with, "fetch-depth"))
            .and_then(Yaml::as_i64);
        checks.push(json!({
            "label": format!("job:{}:checkout_credentials", job_spec.id),
            "kind": "checkout_configuration",
            "expected": false,
            "actual": persist_credentials,
            "ok": persist_credentials == Some(false),
        }));
        checks.push(json!({
            "label": format!("job:{}:checkout_history", job_spec.id),
            "kind": "checkout_configuration",
            "expected": 0,
            "actual": fetch_depth,
            "ok": fetch_depth == Some(0),
        }));
        let dispatch_guard_if = yaml_get(job, "steps")
            .and_then(Yaml::as_vec)
            .into_iter()
            .flatten()
            .find(|step| {
                yaml_get(step, "run")
                    .and_then(Yaml::as_str)
                    .is_some_and(|run| run.contains("foundation-public-root-v1"))
            })
            .and_then(|step| yaml_get(step, "if"))
            .and_then(Yaml::as_str);
        checks.push(json!({
            "label": format!("job:{}:dispatch_guard", job_spec.id),
            "kind": "step_condition",
            "expected": "github.event_name == 'workflow_dispatch'",
            "actual": dispatch_guard_if,
            "ok": dispatch_guard_if == Some("github.event_name == 'workflow_dispatch'"),
        }));
        for command in job_spec.required_run_commands {
            let present = job_run_steps(job)
                .iter()
                .any(|run| run.lines().any(|line| line.trim() == *command));
            checks.push(json!({
                "label": format!("job:{}:run:{command}", job_spec.id),
                "kind": "run_command",
                "expected": command,
                "actual": present,
                "ok": present,
            }));
        }
        for path in job_spec.required_artifact_paths {
            let present = job_artifact_paths(job)
                .iter()
                .any(|artifact_path| artifact_path.lines().any(|line| line.trim() == *path));
            checks.push(json!({
                "label": format!("job:{}:artifact:{path}", job_spec.id),
                "kind": "artifact_path",
                "expected": path,
                "actual": present,
                "ok": present,
            }));
        }
        for os in job_spec.required_matrix_os {
            let present = job_matrix_os_values(job).iter().any(|value| value == os);
            checks.push(json!({
                "label": format!("job:{}:matrix_os:{os}", job_spec.id),
                "kind": "matrix_os",
                "expected": os,
                "actual": present,
                "ok": present,
            }));
        }
    }
    checks
}

fn push_check(checks: &mut Vec<Value>, label: &str, expected: &'static str, actual: Option<&str>) {
    checks.push(json!({
        "label": label,
        "kind": "scalar",
        "expected": expected,
        "actual": actual,
        "ok": actual == Some(expected),
    }));
}

fn yaml_get<'a>(value: &'a Yaml, key: &str) -> Option<&'a Yaml> {
    value.as_hash()?.get(&Yaml::String(key.to_owned()))
}

fn workflow_has_trigger(workflow: &Yaml, trigger: &str) -> bool {
    match yaml_get(workflow, "on") {
        Some(Yaml::Hash(mapping)) => mapping.contains_key(&Yaml::String(trigger.to_owned())),
        Some(Yaml::Array(sequence)) => sequence.iter().any(|value| value.as_str() == Some(trigger)),
        Some(Yaml::String(value)) => value == trigger,
        _ => false,
    }
}

fn job_run_steps(job: &Yaml) -> Vec<String> {
    yaml_get(job, "steps")
        .and_then(Yaml::as_vec)
        .into_iter()
        .flatten()
        .filter_map(|step| yaml_get(step, "run").and_then(Yaml::as_str))
        .map(str::to_owned)
        .collect()
}

fn job_step_names(job: &Yaml) -> Vec<String> {
    yaml_get(job, "steps")
        .and_then(Yaml::as_vec)
        .into_iter()
        .flatten()
        .filter_map(|step| yaml_get(step, "name").and_then(Yaml::as_str))
        .map(str::to_owned)
        .collect()
}

fn job_uses_steps(job: &Yaml) -> Vec<String> {
    yaml_get(job, "steps")
        .and_then(Yaml::as_vec)
        .into_iter()
        .flatten()
        .filter_map(|step| yaml_get(step, "uses").and_then(Yaml::as_str))
        .map(str::to_owned)
        .collect()
}

fn job_artifact_paths(job: &Yaml) -> Vec<String> {
    yaml_get(job, "steps")
        .and_then(Yaml::as_vec)
        .into_iter()
        .flatten()
        .filter(|step| {
            yaml_get(step, "uses")
                .and_then(Yaml::as_str)
                .map(|uses| uses.starts_with("actions/upload-artifact@"))
                .unwrap_or(false)
        })
        .filter_map(|step| {
            yaml_get(step, "with")
                .and_then(|with| yaml_get(with, "path"))
                .and_then(Yaml::as_str)
        })
        .map(str::to_owned)
        .collect()
}

fn job_matrix_os_values(job: &Yaml) -> Vec<String> {
    yaml_get(job, "strategy")
        .and_then(|strategy| yaml_get(strategy, "matrix"))
        .and_then(|matrix| yaml_get(matrix, "os"))
        .and_then(Yaml::as_vec)
        .into_iter()
        .flatten()
        .filter_map(Yaml::as_str)
        .map(str::to_owned)
        .collect()
}

fn runs_on_matches(value: Option<&Yaml>, requirement: RunsOnSpec) -> bool {
    match requirement {
        RunsOnSpec::Exact(expected) => value.and_then(Yaml::as_str) == Some(expected),
    }
}

fn runs_on_expected_json(requirement: RunsOnSpec) -> Value {
    match requirement {
        RunsOnSpec::Exact(expected) => json!(expected),
    }
}

fn runs_on_actual_json(value: Option<&Yaml>) -> Value {
    match value {
        Some(Yaml::String(value)) => json!(value),
        Some(Yaml::Array(values)) => {
            json!(values.iter().filter_map(Yaml::as_str).collect::<Vec<_>>())
        }
        _ => Value::Null,
    }
}

fn workflow_file_path(relative_path: &str) -> PathBuf {
    let cwd_path = PathBuf::from(relative_path);
    if cwd_path.exists() {
        return cwd_path;
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(relative_path)
}

fn fragment_presence(
    fragments: &[WorkflowFragment],
    content: &str,
    should_be_present: bool,
) -> Vec<Value> {
    fragments
        .iter()
        .map(|fragment| {
            let present = content.contains(fragment.text);
            json!({
                "label": fragment.label,
                "fragment": fragment.text,
                "present": present,
                "expected": if should_be_present { "present" } else { "absent" },
                "ok": if should_be_present { present } else { !present },
            })
        })
        .collect()
}

fn workflow_audit_markdown(report: &Value) -> String {
    let summary = &report["summary"];
    let mut markdown = String::new();
    markdown.push_str("# Workflow Audit\n\n");
    markdown.push_str(&format!(
        "- status: `{}`\n",
        report["status"].as_str().unwrap_or("unknown")
    ));
    markdown.push_str(&format!(
        "- workflow count: `{}`\n",
        summary["workflow_count"].as_u64().unwrap_or(0)
    ));
    markdown.push_str(&format!(
        "- blocking: `{}`\n\n",
        summary["blocking_count"].as_u64().unwrap_or(0)
    ));
    markdown.push_str("| Workflow | Evidence Role | Status | Blocking |\n");
    markdown.push_str("|---|---|---|---|\n");
    if let Some(entries) = report["entries"].as_array() {
        for entry in entries {
            markdown.push_str(&format!(
                "| `{}` | `{}` | `{}` | `{}` |\n",
                entry["path"].as_str().unwrap_or("<unknown>"),
                entry["evidence_role"].as_str().unwrap_or("unknown"),
                entry["audit_status"].as_str().unwrap_or("unknown"),
                entry["blocking"].as_bool().unwrap_or(false),
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

    #[test]
    fn workflow_audit_covers_current_workflow_surface() {
        let report = workflow_audit_report_json();

        assert_eq!(report["schema"], WORKFLOW_AUDIT_SCHEMA);
        assert_eq!(report["command"], "workflow-audit");
        assert_eq!(report["status"], "passed");
        assert_eq!(report["summary"]["workflow_count"], 1);
        assert_eq!(report["summary"]["blocking_count"], 0);
    }

    #[test]
    fn fragment_presence_distinguishes_required_and_forbidden_text() {
        let fragments = [WorkflowFragment {
            label: "sample",
            text: "cargo xtask verify-bootstrap",
        }];

        let required = fragment_presence(&fragments, "cargo xtask verify-bootstrap", true);
        let forbidden = fragment_presence(&fragments, "cargo xtask verify-bootstrap", false);

        assert_eq!(required[0]["ok"], true);
        assert_eq!(forbidden[0]["ok"], false);
    }

    #[test]
    fn workflow_policy_checks_ignore_commented_out_commands() {
        let workflow = parse_workflow_yaml(
            r#"
name: CI
on:
  workflow_dispatch:
jobs:
  verify-fast:
    name: verify-fast / linux
    runs-on: ubuntu-latest
    steps:
      - name: Comment only
        run: |
          # cargo xtask verify-fast
"#,
        )
        .unwrap();
        let spec = WorkflowSpec {
            path: ".github/workflows/test.yml",
            evidence_role: "test",
            expected_name: "CI",
            required_triggers: &["workflow_dispatch"],
            required_jobs: &[WorkflowJobSpec {
                id: "verify-fast",
                name: "verify-fast / linux",
                runs_on: RunsOnSpec::Exact("ubuntu-latest"),
                required_run_commands: &["cargo xtask verify-fast"],
                required_artifact_paths: &[],
                required_matrix_os: &[],
            }],
            forbidden_fragments: &[],
        };

        let checks = workflow_policy_checks(&spec, &workflow);
        let run_check = checks
            .iter()
            .find(|check| check["kind"] == "run_command")
            .unwrap();

        assert_eq!(run_check["ok"], false);
    }

    #[test]
    fn workflow_policy_checks_require_structured_artifact_paths() {
        let workflow = parse_workflow_yaml(
            r#"
name: GPU Render
on:
  workflow_dispatch:
jobs:
  verify-render:
    name: verify-render / test
    runs-on: ubuntu-24.04
    steps:
      - name: Run GPU render verification gate
        run: cargo xtask verify-render
      - name: Upload render artifacts
        uses: actions/upload-artifact@v4
        with:
          path: |
            target/mirante4d/verify-render/**
"#,
        )
        .unwrap();
        let spec = WorkflowSpec {
            path: ".github/workflows/gpu-render.yml",
            evidence_role: "test",
            expected_name: "GPU Render",
            required_triggers: &["workflow_dispatch"],
            required_jobs: &[WorkflowJobSpec {
                id: "verify-render",
                name: "verify-render / test",
                runs_on: RunsOnSpec::Exact("ubuntu-24.04"),
                required_run_commands: &["cargo xtask verify-render"],
                required_artifact_paths: &["target/mirante4d/verify-render/**"],
                required_matrix_os: &[],
            }],
            forbidden_fragments: &[],
        };

        let checks = workflow_policy_checks(&spec, &workflow);

        let artifact_check = checks
            .iter()
            .find(|check| check["kind"] == "artifact_path")
            .unwrap();
        assert_eq!(artifact_check["ok"], true);
    }
}

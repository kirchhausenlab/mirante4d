use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use serde_json::{Value, json};

use crate::reports::write_json_file;

const COMMAND_AUDIT_OUTPUT_DIR: &str = "target/mirante4d/command-audit";
const COMMAND_AUDIT_JSON: &str = "target/mirante4d/command-audit/command-audit-report.json";
const COMMAND_AUDIT_MD: &str = "target/mirante4d/command-audit/command-audit-report.md";
const COMMAND_AUDIT_SCHEMA: &str = "mirante4d-xtask-command-audit";
const COMMAND_AUDIT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommandAuditEntry {
    pub(crate) command: &'static str,
    pub(crate) family: &'static str,
    pub(crate) evidence_class: &'static str,
    pub(crate) default_safety: &'static str,
    pub(crate) requires_heavy_opt_in: bool,
    pub(crate) product_evidence_role: &'static str,
    pub(crate) stale_or_unsafe_status: &'static str,
    pub(crate) report_paths: &'static [&'static str],
    pub(crate) notes: &'static str,
}

pub(crate) const COMMAND_AUDIT_ENTRIES: &[CommandAuditEntry] = &[
    CommandAuditEntry {
        command: "help",
        family: "documentation",
        evidence_class: "command_help",
        default_safety: "routine_inert",
        requires_heavy_opt_in: false,
        product_evidence_role: "documentation_and_traceability_only",
        stale_or_unsafe_status: "current_inert_no_product_launch",
        report_paths: &[],
        notes: "Prints xtask help text only. Help aliases must not be interpreted as product-validation dataset paths.",
    },
    CommandAuditEntry {
        command: "verify-leaf",
        family: "verify",
        evidence_class: "nonrecursive_verification_leaf",
        default_safety: "routine",
        requires_heavy_opt_in: false,
        product_evidence_role: "automated_verification_only",
        stale_or_unsafe_status: "current_registry_owned",
        report_paths: &["target/mirante4d/verification/"],
        notes: "Runs exactly one registry-owned policy, lint, unit, contract, UI, or doctest leaf.",
    },
    CommandAuditEntry {
        command: "verify-pr",
        family: "verify",
        evidence_class: "public_verification_group",
        default_safety: "routine",
        requires_heavy_opt_in: false,
        product_evidence_role: "automated_verification_only",
        stale_or_unsafe_status: "current_nonrecursive",
        report_paths: &["target/mirante4d/verification/"],
        notes: "Runs the public policy and/or Rust verification group in-process without recursively invoking xtask.",
    },
    CommandAuditEntry {
        command: "verify-local",
        family: "verify",
        evidence_class: "trusted_local_verification",
        default_safety: "trusted_machine_opt_in_required",
        requires_heavy_opt_in: true,
        product_evidence_role: "trusted_gpu_support_only",
        stale_or_unsafe_status: "current_registry_owned",
        report_paths: &["target/mirante4d/verification/"],
        notes: "Runs the single registry-generated trusted-GPU ignored-case union; it is forbidden in GitHub Actions.",
    },
    CommandAuditEntry {
        command: "verification-sync",
        family: "verify",
        evidence_class: "verification_registry_generation",
        default_safety: "routine",
        requires_heavy_opt_in: false,
        product_evidence_role: "documentation_and_traceability_only",
        stale_or_unsafe_status: "current_registry_authority",
        report_paths: &["verification/generated/", ".config/nextest.toml"],
        notes: "Generates or checks the registry-derived selector manifest and Nextest configuration.",
    },
    CommandAuditEntry {
        command: "verify-deps",
        family: "verify",
        evidence_class: "dependency_gate",
        default_safety: "routine",
        requires_heavy_opt_in: false,
        product_evidence_role: "automated_verification_only",
        stale_or_unsafe_status: "current",
        report_paths: &[],
        notes: "Dependency source/license/advisory policy gate.",
    },
    CommandAuditEntry {
        command: "verify-coverage",
        family: "verify",
        evidence_class: "coverage_gate",
        default_safety: "routine_but_tool_dependent",
        requires_heavy_opt_in: false,
        product_evidence_role: "coverage_only",
        stale_or_unsafe_status: "current",
        report_paths: &["target/mirante4d/coverage/summary.json"],
        notes: "Coverage is a guardrail and not proof of product behavior.",
    },
    CommandAuditEntry {
        command: "generate-fixture",
        family: "fixture",
        evidence_class: "fixture_generation",
        default_safety: "routine",
        requires_heavy_opt_in: false,
        product_evidence_role: "fixture_preparation_only",
        stale_or_unsafe_status: "current",
        report_paths: &["target/mirante4d/fixtures/"],
        notes: "Generates deterministic native packages used by tests and product validation.",
    },
    CommandAuditEntry {
        command: "package-dev",
        family: "package",
        evidence_class: "packaging_gate",
        default_safety: "routine",
        requires_heavy_opt_in: false,
        product_evidence_role: "packaging_supporting_evidence",
        stale_or_unsafe_status: "current",
        report_paths: &["target/mirante4d/package/"],
        notes: "Builds Linux release packaging artifacts and packaged smoke evidence.",
    },
    CommandAuditEntry {
        command: "package-linux-release",
        family: "package",
        evidence_class: "packaging_gate",
        default_safety: "routine",
        requires_heavy_opt_in: false,
        product_evidence_role: "packaging_supporting_evidence",
        stale_or_unsafe_status: "current",
        report_paths: &["target/mirante4d/package/"],
        notes: "Release packaging gate; not a product-open substitute.",
    },
    CommandAuditEntry {
        command: "bench-smoke",
        family: "benchmark",
        evidence_class: "bounded_benchmark",
        default_safety: "routine",
        requires_heavy_opt_in: false,
        product_evidence_role: "benchmark_supporting_evidence_only",
        stale_or_unsafe_status: "current",
        report_paths: &["target/mirante4d/benchmarks/"],
        notes: "Generated-fixture smoke benchmark; cannot prove native-window behavior.",
    },
    CommandAuditEntry {
        command: "bench-native-package",
        family: "benchmark",
        evidence_class: "heavy_local_benchmark",
        default_safety: "heavy_opt_in_required",
        requires_heavy_opt_in: true,
        product_evidence_role: "benchmark_supporting_evidence_only",
        stale_or_unsafe_status: "quarantined_heavy_local",
        report_paths: &["target/mirante4d/benchmarks/"],
        notes: "Local package benchmark; unsafe for T5Qual001/T5Qual002 by default and guarded before package work starts.",
    },
    CommandAuditEntry {
        command: "bench-phase11-large-view",
        family: "benchmark",
        evidence_class: "heavy_local_benchmark",
        default_safety: "heavy_opt_in_required",
        requires_heavy_opt_in: true,
        product_evidence_role: "benchmark_supporting_evidence_only",
        stale_or_unsafe_status: "quarantined_heavy_local",
        report_paths: &["target/mirante4d/benchmarks/"],
        notes: "Streaming large-view benchmark; reports limits and pre-stream dense-read counters.",
    },
    CommandAuditEntry {
        command: "bench-phase11-interaction",
        family: "benchmark",
        evidence_class: "heavy_local_benchmark",
        default_safety: "heavy_opt_in_required",
        requires_heavy_opt_in: true,
        product_evidence_role: "benchmark_supporting_evidence_only",
        stale_or_unsafe_status: "quarantined_heavy_local",
        report_paths: &["target/mirante4d/benchmarks/"],
        notes: "Interaction timeline benchmark; typed resident reads landed but real T5Qual001 evidence remains opt-in.",
    },
    CommandAuditEntry {
        command: "bench-phase11-viewport-matrix",
        family: "benchmark",
        evidence_class: "heavy_local_benchmark",
        default_safety: "heavy_opt_in_required",
        requires_heavy_opt_in: true,
        product_evidence_role: "benchmark_supporting_evidence_only",
        stale_or_unsafe_status: "quarantined_heavy_local",
        report_paths: &["target/mirante4d/benchmarks/"],
        notes: "Runs Phase 11 interaction across viewport scenarios; heavy on large packages.",
    },
    CommandAuditEntry {
        command: "bench-phase11-synthetic-matrix",
        family: "benchmark",
        evidence_class: "bounded_benchmark",
        default_safety: "routine_generated_fixtures",
        requires_heavy_opt_in: false,
        product_evidence_role: "benchmark_supporting_evidence_only",
        stale_or_unsafe_status: "current",
        report_paths: &["target/mirante4d/benchmarks/"],
        notes: "Generated-fixture matrix; no private T5Qual001/T5Qual002 dependency.",
    },
    CommandAuditEntry {
        command: "bench-phase13-renderer",
        family: "benchmark",
        evidence_class: "heavy_local_benchmark",
        default_safety: "heavy_opt_in_required",
        requires_heavy_opt_in: true,
        product_evidence_role: "benchmark_supporting_evidence_only",
        stale_or_unsafe_status: "quarantined_heavy_local",
        report_paths: &["target/mirante4d/benchmarks/"],
        notes: "Renderer benchmark over package data; typed U8/U16/F32 paths are covered by generated fixtures.",
    },
    CommandAuditEntry {
        command: "bench-phase13-viewport-matrix",
        family: "benchmark",
        evidence_class: "heavy_local_benchmark",
        default_safety: "heavy_opt_in_required",
        requires_heavy_opt_in: true,
        product_evidence_role: "benchmark_supporting_evidence_only",
        stale_or_unsafe_status: "quarantined_heavy_local",
        report_paths: &["target/mirante4d/benchmarks/"],
        notes: "Phase 13 renderer benchmark matrix; heavy on large packages.",
    },
    CommandAuditEntry {
        command: "bench-runtime-stress",
        family: "benchmark",
        evidence_class: "bounded_benchmark",
        default_safety: "routine_generated_fixture",
        requires_heavy_opt_in: false,
        product_evidence_role: "benchmark_supporting_evidence_only",
        stale_or_unsafe_status: "current",
        report_paths: &["target/mirante4d/benchmarks/"],
        notes: "Generated runtime stress benchmark with explicit synthetic dimensions.",
    },
    CommandAuditEntry {
        command: "bench-import-sample",
        family: "benchmark",
        evidence_class: "local_sample_benchmark",
        default_safety: "bounded_by_file_limit",
        requires_heavy_opt_in: false,
        product_evidence_role: "benchmark_supporting_evidence_only",
        stale_or_unsafe_status: "local_sample_only",
        report_paths: &["target/mirante4d/benchmarks/import-sample/"],
        notes: "Local raw-sample import benchmark, bounded by MIRANTE4D_BENCH_IMPORT_MAX_FILES by default.",
    },
    CommandAuditEntry {
        command: "bench-check",
        family: "benchmark",
        evidence_class: "comparison_gate",
        default_safety: "routine",
        requires_heavy_opt_in: false,
        product_evidence_role: "comparison_only",
        stale_or_unsafe_status: "current",
        report_paths: &[],
        notes: "Compares reports only after schema/scenario/hardware/dataset compatibility checks pass.",
    },
    CommandAuditEntry {
        command: "neuroglancer-compare",
        family: "comparison",
        evidence_class: "neuroglancer_comparison_gate",
        default_safety: "routine_report_only",
        requires_heavy_opt_in: false,
        product_evidence_role: "comparison_only_requires_prior_product_validation",
        stale_or_unsafe_status: "current_requires_external_neuroglancer_measurement",
        report_paths: &[
            "target/mirante4d/neuroglancer-comparison/neuroglancer-comparison-report.json",
        ],
        notes: "Compares Mirante real-display product-validation reports against manually or externally measured Neuroglancer 2D latency/memory data; does not launch either viewer.",
    },
    CommandAuditEntry {
        command: "baseline-audit",
        family: "benchmark",
        evidence_class: "baseline_policy_audit",
        default_safety: "routine",
        requires_heavy_opt_in: false,
        product_evidence_role: "documentation_and_traceability_only",
        stale_or_unsafe_status: "current_needs_measured_refresh",
        report_paths: &["target/mirante4d/baseline-audit/baseline-audit-report.json"],
        notes: "Audits curated benchmark baselines for baseline_class policy and compatibility context; does not run benchmarks.",
    },
    CommandAuditEntry {
        command: "baseline-refresh-plan",
        family: "benchmark",
        evidence_class: "baseline_refresh_planning",
        default_safety: "routine_non_mutating",
        requires_heavy_opt_in: false,
        product_evidence_role: "documentation_and_traceability_only",
        stale_or_unsafe_status: "current_non_mutating_plan_only",
        report_paths: &["target/mirante4d/baseline-refresh/baseline-refresh-plan.json"],
        notes: "Matches stale curated baselines to available benchmark reports and emits a promotion manifest only when every refresh target has one unique promotable source; does not write curated baselines.",
    },
    CommandAuditEntry {
        command: "baseline-promote",
        family: "benchmark",
        evidence_class: "baseline_promotion_guard",
        default_safety: "clean_worktree_required",
        requires_heavy_opt_in: false,
        product_evidence_role: "documentation_and_traceability_only",
        stale_or_unsafe_status: "current_refuses_dirty_or_debug_reports",
        report_paths: &["docs/benchmarks/baselines/"],
        notes: "Promotes one clean release benchmark report into curated baselines after validating baseline class, hardware/dataset class, git commit, dirty-worktree status, and timing metrics.",
    },
    CommandAuditEntry {
        command: "baseline-promote-manifest",
        family: "benchmark",
        evidence_class: "baseline_batch_promotion_guard",
        default_safety: "clean_worktree_required",
        requires_heavy_opt_in: false,
        product_evidence_role: "documentation_and_traceability_only",
        stale_or_unsafe_status: "current_refuses_dirty_or_debug_reports",
        report_paths: &[
            "docs/benchmarks/baselines/",
            "target/mirante4d/baseline-promote/baseline-promote-report.json",
        ],
        notes: "Promotes a manifest-defined batch of clean release benchmark reports into curated baselines after prevalidating every source report and destination.",
    },
    CommandAuditEntry {
        command: "workflow-audit",
        family: "ci",
        evidence_class: "ci_workflow_policy_audit",
        default_safety: "routine",
        requires_heavy_opt_in: false,
        product_evidence_role: "ci_configuration_traceability_only",
        stale_or_unsafe_status: "current_static_workflow_audit_not_external_run_evidence",
        report_paths: &["target/mirante4d/workflow-audit/workflow-audit-report.json"],
        notes: "Audits GitHub Actions workflow definitions for named evidence jobs, xtask gates, artifact uploads, self-hosted GPU separation, and private-data exclusions.",
    },
    CommandAuditEntry {
        command: "docs-check",
        family: "documentation",
        evidence_class: "documentation_consistency_gate",
        default_safety: "routine_local",
        requires_heavy_opt_in: false,
        product_evidence_role: "documentation_and_traceability_only",
        stale_or_unsafe_status: "current",
        report_paths: &[],
        notes: "Checks the exact documentation inventory, authority ownership, read order, listing graph, local links, and anchors.",
    },
    CommandAuditEntry {
        command: "app-smoke",
        family: "smoke",
        evidence_class: "smoke_only",
        default_safety: "manual_supporting_evidence",
        requires_heavy_opt_in: false,
        product_evidence_role: "not_product_validation",
        stale_or_unsafe_status: "quarantined_smoke_only",
        report_paths: &["target/mirante4d/benchmarks/app-smoke-*.json"],
        notes: "Uses MIRANTE4D_APP_SMOKE; explicitly not a substitute for product-open validation.",
    },
    CommandAuditEntry {
        command: "product-validate",
        family: "product_automation",
        evidence_class: "internal_e1_automation",
        default_safety: "routine_generated_fixture_or_heavy_local_sample_opt_in",
        requires_heavy_opt_in: false,
        product_evidence_role: "e1_instrumented_support_only_not_product_open",
        stale_or_unsafe_status: "current_internal_automation_not_e3_or_e4",
        report_paths: &["target/mirante4d/product-validation/"],
        notes: "Launches the release app with env-gated semantic commands and internal state/readback. This is E1 support only; it cannot satisfy E3, E4, or product-open validation.",
    },
    CommandAuditEntry {
        command: "phase10-audit",
        family: "audit",
        evidence_class: "local_sample_audit",
        default_safety: "bounded_by_file_limit",
        requires_heavy_opt_in: false,
        product_evidence_role: "audit_supporting_evidence_only",
        stale_or_unsafe_status: "local_sample_only",
        report_paths: &["target/mirante4d/phase10/"],
        notes: "Local sample import/open audit bounded by sample availability and file limit.",
    },
    CommandAuditEntry {
        command: "phase12-audit",
        family: "audit",
        evidence_class: "local_sample_audit",
        default_safety: "bounded_by_file_limit",
        requires_heavy_opt_in: false,
        product_evidence_role: "audit_supporting_evidence_only",
        stale_or_unsafe_status: "local_sample_only",
        report_paths: &["target/mirante4d/phase12/"],
        notes: "Viewer-usability audit over generated and selected local samples.",
    },
    CommandAuditEntry {
        command: "phase14-audit",
        family: "audit",
        evidence_class: "audit",
        default_safety: "routine_generated_fixture_plus_optional_samples",
        requires_heavy_opt_in: false,
        product_evidence_role: "audit_supporting_evidence_only",
        stale_or_unsafe_status: "current",
        report_paths: &["target/mirante4d/phase14/"],
        notes: "Multi-channel fixture/sample inventory and hidden-channel resource evidence.",
    },
    CommandAuditEntry {
        command: "bench-phase14-multichannel",
        family: "benchmark",
        evidence_class: "bounded_benchmark",
        default_safety: "routine_generated_fixture",
        requires_heavy_opt_in: false,
        product_evidence_role: "benchmark_supporting_evidence_only",
        stale_or_unsafe_status: "current",
        report_paths: &["target/mirante4d/phase14/"],
        notes: "Synthetic multi-channel benchmark report.",
    },
    CommandAuditEntry {
        command: "phase15-audit",
        family: "audit",
        evidence_class: "audit",
        default_safety: "routine_generated_fixture",
        requires_heavy_opt_in: false,
        product_evidence_role: "audit_supporting_evidence_only",
        stale_or_unsafe_status: "current",
        report_paths: &["target/mirante4d/phase15/"],
        notes: "Analysis-workbench audit with operation records and export evidence.",
    },
    CommandAuditEntry {
        command: "bench-phase15-analysis",
        family: "benchmark",
        evidence_class: "bounded_benchmark",
        default_safety: "routine_generated_fixture",
        requires_heavy_opt_in: false,
        product_evidence_role: "benchmark_supporting_evidence_only",
        stale_or_unsafe_status: "current",
        report_paths: &["target/mirante4d/phase15/"],
        notes: "Deterministic analysis benchmark plus table/plot artifacts.",
    },
    CommandAuditEntry {
        command: "phase17-audit",
        family: "audit",
        evidence_class: "audit",
        default_safety: "routine_generated_fixture",
        requires_heavy_opt_in: false,
        product_evidence_role: "audit_supporting_evidence_only",
        stale_or_unsafe_status: "current",
        report_paths: &["target/mirante4d/phase17/"],
        notes: "Import metadata hardening audit.",
    },
    CommandAuditEntry {
        command: "phase19-audit",
        family: "audit",
        evidence_class: "local_sample_audit",
        default_safety: "bounded_by_file_limit",
        requires_heavy_opt_in: false,
        product_evidence_role: "audit_supporting_evidence_only",
        stale_or_unsafe_status: "local_sample_only",
        report_paths: &["target/mirante4d/phase19/"],
        notes: "Viewer product hardening audit with generated playback and optional local sample evidence.",
    },
    CommandAuditEntry {
        command: "phase20-smoke-audit",
        family: "audit",
        evidence_class: "bounded_smoke_audit",
        default_safety: "routine_generated_fixture",
        requires_heavy_opt_in: false,
        product_evidence_role: "smoke_supporting_evidence_only",
        stale_or_unsafe_status: "current",
        report_paths: &["target/mirante4d/phase20/"],
        notes: "Generated stack/plane-series import/open smoke evidence for Phase 20.",
    },
    CommandAuditEntry {
        command: "phase20-extreme-audit",
        family: "audit",
        evidence_class: "heavy_local_evidence",
        default_safety: "heavy_opt_in_required",
        requires_heavy_opt_in: true,
        product_evidence_role: "heavy_local_supporting_evidence_only",
        stale_or_unsafe_status: "quarantined_heavy_local",
        report_paths: &["target/mirante4d/phase20/"],
        notes: "Full local T5Qual002/T5Qual001 source evidence; intentionally heavy and not CI-safe.",
    },
    CommandAuditEntry {
        command: "phase20-extreme-sample",
        family: "audit",
        evidence_class: "heavy_local_evidence",
        default_safety: "heavy_opt_in_required",
        requires_heavy_opt_in: true,
        product_evidence_role: "heavy_local_supporting_evidence_only",
        stale_or_unsafe_status: "quarantined_heavy_local",
        report_paths: &["target/mirante4d/phase20/"],
        notes: "One local T5Qual002 or T5Qual001 extreme sample run; intentionally heavy and not CI-safe.",
    },
    CommandAuditEntry {
        command: "command-audit",
        family: "audit",
        evidence_class: "command_surface_audit",
        default_safety: "routine",
        requires_heavy_opt_in: false,
        product_evidence_role: "documentation_and_traceability_only",
        stale_or_unsafe_status: "current",
        report_paths: &["target/mirante4d/command-audit/command-audit-report.json"],
        notes: "Machine-readable inventory of xtask command classifications and quarantine status.",
    },
    CommandAuditEntry {
        command: "run-dev",
        family: "developer",
        evidence_class: "developer_helper",
        default_safety: "manual",
        requires_heavy_opt_in: false,
        product_evidence_role: "manual_development_only",
        stale_or_unsafe_status: "current",
        report_paths: &[],
        notes: "Developer app launch helper; not a verification artifact by itself.",
    },
];

pub(crate) fn command_audit() -> anyhow::Result<PathBuf> {
    fs::create_dir_all(COMMAND_AUDIT_OUTPUT_DIR)
        .with_context(|| format!("failed to create {COMMAND_AUDIT_OUTPUT_DIR}"))?;
    let json_path = Path::new(COMMAND_AUDIT_JSON);
    let markdown_path = Path::new(COMMAND_AUDIT_MD);
    let report = command_audit_report_json();
    write_json_file(json_path, &report)?;
    fs::write(markdown_path, command_audit_markdown())
        .with_context(|| format!("failed to write {}", markdown_path.display()))?;
    Ok(json_path.to_path_buf())
}

fn command_audit_report_json() -> Value {
    let heavy_opt_in_count = COMMAND_AUDIT_ENTRIES
        .iter()
        .filter(|entry| entry.requires_heavy_opt_in)
        .count();
    let smoke_only_count = COMMAND_AUDIT_ENTRIES
        .iter()
        .filter(|entry| entry.evidence_class.contains("smoke"))
        .count();
    let internal_e1_automation_count = COMMAND_AUDIT_ENTRIES
        .iter()
        .filter(|entry| entry.evidence_class == "internal_e1_automation")
        .count();

    json!({
        "schema": COMMAND_AUDIT_SCHEMA,
        "schema_version": COMMAND_AUDIT_SCHEMA_VERSION,
        "command": "command-audit",
        "status": "passed",
        "summary": {
            "command_count": COMMAND_AUDIT_ENTRIES.len(),
            "heavy_opt_in_count": heavy_opt_in_count,
            "smoke_only_count": smoke_only_count,
            "internal_e1_automation_count": internal_e1_automation_count,
        },
        "entries": COMMAND_AUDIT_ENTRIES
            .iter()
            .map(command_audit_entry_json)
            .collect::<Vec<_>>(),
    })
}

fn command_audit_entry_json(entry: &CommandAuditEntry) -> Value {
    json!({
        "command": entry.command,
        "family": entry.family,
        "evidence_class": entry.evidence_class,
        "default_safety": entry.default_safety,
        "requires_heavy_opt_in": entry.requires_heavy_opt_in,
        "product_evidence_role": entry.product_evidence_role,
        "stale_or_unsafe_status": entry.stale_or_unsafe_status,
        "report_paths": entry.report_paths,
        "notes": entry.notes,
    })
}

fn command_audit_markdown() -> String {
    let mut markdown = String::from(
        "# Mirante4D Xtask Command Audit\n\n\
         | Command | Class | Default Safety | Product Evidence Role | Status |\n\
         |---|---|---|---|---|\n",
    );
    for entry in COMMAND_AUDIT_ENTRIES {
        markdown.push_str(&format!(
            "| `{}` | `{}` | `{}` | `{}` | `{}` |\n",
            entry.command,
            entry.evidence_class,
            entry.default_safety,
            entry.product_evidence_role,
            entry.stale_or_unsafe_status
        ));
    }
    markdown
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    const HANDLED_COMMANDS: &[&str] = &[
        "help",
        "verify-leaf",
        "verify-pr",
        "verify-local",
        "verification-sync",
        "verify-deps",
        "verify-coverage",
        "generate-fixture",
        "package-dev",
        "package-linux-release",
        "bench-smoke",
        "bench-native-package",
        "bench-phase11-large-view",
        "bench-phase11-interaction",
        "bench-phase11-viewport-matrix",
        "bench-phase11-synthetic-matrix",
        "bench-phase13-renderer",
        "bench-phase13-viewport-matrix",
        "app-smoke",
        "product-validate",
        "bench-runtime-stress",
        "bench-import-sample",
        "phase10-audit",
        "phase12-audit",
        "phase14-audit",
        "bench-phase14-multichannel",
        "phase15-audit",
        "bench-phase15-analysis",
        "phase17-audit",
        "phase19-audit",
        "phase20-smoke-audit",
        "phase20-extreme-audit",
        "phase20-extreme-sample",
        "bench-check",
        "neuroglancer-compare",
        "baseline-audit",
        "baseline-refresh-plan",
        "baseline-promote",
        "baseline-promote-manifest",
        "workflow-audit",
        "docs-check",
        "command-audit",
        "run-dev",
    ];

    #[test]
    fn command_audit_covers_current_xtask_command_surface() {
        let audited = COMMAND_AUDIT_ENTRIES
            .iter()
            .map(|entry| entry.command)
            .collect::<BTreeSet<_>>();
        let handled = HANDLED_COMMANDS.iter().copied().collect::<BTreeSet<_>>();

        assert_eq!(audited, handled);
    }

    #[test]
    fn command_audit_quarantines_smoke_and_heavy_local_paths() {
        let app_smoke = entry("app-smoke");
        assert_eq!(app_smoke.evidence_class, "smoke_only");
        assert_eq!(app_smoke.product_evidence_role, "not_product_validation");
        assert_eq!(app_smoke.stale_or_unsafe_status, "quarantined_smoke_only");

        for command in [
            "bench-native-package",
            "bench-phase11-interaction",
            "bench-phase13-renderer",
            "phase20-extreme-audit",
            "phase20-extreme-sample",
        ] {
            let entry = entry(command);
            assert!(
                entry.requires_heavy_opt_in,
                "{command} must stay heavy-gated"
            );
            assert_eq!(entry.default_safety, "heavy_opt_in_required");
        }
    }

    #[test]
    fn command_audit_keeps_product_validation_distinct_from_benchmarks() {
        let product_validate = entry("product-validate");
        assert_eq!(product_validate.family, "product_automation");
        assert_eq!(product_validate.evidence_class, "internal_e1_automation");
        assert_eq!(
            product_validate.product_evidence_role,
            "e1_instrumented_support_only_not_product_open"
        );
        assert!(product_validate.notes.contains("cannot satisfy E3, E4"));

        for benchmark in ["bench-smoke", "bench-runtime-stress", "bench-check"] {
            let entry = entry(benchmark);
            assert_ne!(entry.evidence_class, "internal_e1_automation");
            assert_ne!(
                entry.product_evidence_role,
                "e1_instrumented_support_only_not_product_open"
            );
        }

        let neuroglancer_compare = entry("neuroglancer-compare");
        assert_eq!(
            neuroglancer_compare.product_evidence_role,
            "comparison_only_requires_prior_product_validation"
        );
        assert_eq!(
            neuroglancer_compare.stale_or_unsafe_status,
            "current_requires_external_neuroglancer_measurement"
        );
    }

    #[test]
    fn command_audit_tracks_baseline_audit_as_policy_hygiene() {
        let baseline_audit = entry("baseline-audit");

        assert_eq!(baseline_audit.evidence_class, "baseline_policy_audit");
        assert_eq!(
            baseline_audit.product_evidence_role,
            "documentation_and_traceability_only"
        );
        assert_eq!(
            baseline_audit.stale_or_unsafe_status,
            "current_needs_measured_refresh"
        );
        assert_eq!(
            baseline_audit.report_paths,
            &["target/mirante4d/baseline-audit/baseline-audit-report.json"]
        );
    }

    #[test]
    fn command_audit_tracks_baseline_refresh_plan_as_non_mutating_hygiene() {
        let refresh_plan = entry("baseline-refresh-plan");

        assert_eq!(refresh_plan.evidence_class, "baseline_refresh_planning");
        assert_eq!(refresh_plan.default_safety, "routine_non_mutating");
        assert_eq!(
            refresh_plan.product_evidence_role,
            "documentation_and_traceability_only"
        );
        assert_eq!(
            refresh_plan.stale_or_unsafe_status,
            "current_non_mutating_plan_only"
        );
        assert_eq!(
            refresh_plan.report_paths,
            &["target/mirante4d/baseline-refresh/baseline-refresh-plan.json"]
        );
    }

    #[test]
    fn command_audit_tracks_baseline_promote_as_guarded_policy_action() {
        let baseline_promote = entry("baseline-promote");

        assert_eq!(baseline_promote.evidence_class, "baseline_promotion_guard");
        assert_eq!(baseline_promote.default_safety, "clean_worktree_required");
        assert_eq!(
            baseline_promote.product_evidence_role,
            "documentation_and_traceability_only"
        );
        assert_eq!(
            baseline_promote.stale_or_unsafe_status,
            "current_refuses_dirty_or_debug_reports"
        );

        let batch_promote = entry("baseline-promote-manifest");
        assert_eq!(
            batch_promote.evidence_class,
            "baseline_batch_promotion_guard"
        );
        assert_eq!(batch_promote.default_safety, "clean_worktree_required");
        assert_eq!(
            batch_promote.product_evidence_role,
            "documentation_and_traceability_only"
        );
        assert_eq!(
            batch_promote.report_paths,
            &[
                "docs/benchmarks/baselines/",
                "target/mirante4d/baseline-promote/baseline-promote-report.json"
            ]
        );
    }

    #[test]
    fn command_audit_tracks_workflow_audit_as_ci_configuration_hygiene() {
        let workflow_audit = entry("workflow-audit");

        assert_eq!(workflow_audit.evidence_class, "ci_workflow_policy_audit");
        assert_eq!(
            workflow_audit.product_evidence_role,
            "ci_configuration_traceability_only"
        );
        assert_eq!(
            workflow_audit.stale_or_unsafe_status,
            "current_static_workflow_audit_not_external_run_evidence"
        );
        assert_eq!(
            workflow_audit.report_paths,
            &["target/mirante4d/workflow-audit/workflow-audit-report.json"]
        );
    }

    #[test]
    fn command_audit_report_counts_are_stable() {
        let report = command_audit_report_json();

        assert_eq!(report["schema"], COMMAND_AUDIT_SCHEMA);
        assert_eq!(report["command"], "command-audit");
        assert_eq!(
            report["summary"]["command_count"],
            COMMAND_AUDIT_ENTRIES.len()
        );
        assert_eq!(report["summary"]["internal_e1_automation_count"], 1);
        assert!(report["summary"].get("product_validation_count").is_none());
        assert!(
            report["summary"]["heavy_opt_in_count"].as_u64().unwrap() >= 8,
            "heavy local paths must remain visible in the audit"
        );
    }

    fn entry(command: &str) -> &'static CommandAuditEntry {
        COMMAND_AUDIT_ENTRIES
            .iter()
            .find(|entry| entry.command == command)
            .unwrap_or_else(|| panic!("missing command audit entry for {command}"))
    }
}

use std::{fs, path::PathBuf};

use anyhow::Context;
use serde_json::json;

use crate::reports::write_json_file;

const OUTPUT_DIR: &str = "target/mirante4d/command-audit";
const JSON_REPORT: &str = "target/mirante4d/command-audit/command-audit-report.json";
const MARKDOWN_REPORT: &str = "target/mirante4d/command-audit/command-audit-report.md";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommandAuditEntry {
    pub(crate) command: &'static str,
    pub(crate) family: &'static str,
    pub(crate) evidence_role: &'static str,
    pub(crate) restricted: bool,
}

pub(crate) const COMMAND_AUDIT_ENTRIES: &[CommandAuditEntry] = &[
    entry("help", "documentation", "none", false),
    entry("verify-leaf", "verification", "automated", false),
    entry("verify-pr", "verification", "automated", false),
    entry("verify-local", "verification", "trusted-local", true),
    entry("verification-sync", "verification", "configuration", false),
    entry("verify-deps", "verification", "automated", false),
    entry("verify-coverage", "verification", "diagnostic", false),
    entry("package-dev", "packaging", "supporting", false),
    entry("package-linux-release", "packaging", "release", false),
    entry("app-smoke", "product", "smoke", false),
    entry("product-validate", "product", "product-check", false),
    entry("bench-check", "comparison", "diagnostic", false),
    entry("neuroglancer-compare", "comparison", "diagnostic", false),
    entry("workflow-audit", "policy", "automated", false),
    entry("docs-check", "policy", "automated", false),
    entry("command-audit", "policy", "automated", false),
    entry("run-dev", "development", "none", false),
];

const fn entry(
    command: &'static str,
    family: &'static str,
    evidence_role: &'static str,
    restricted: bool,
) -> CommandAuditEntry {
    CommandAuditEntry {
        command,
        family,
        evidence_role,
        restricted,
    }
}

pub(crate) fn command_audit() -> anyhow::Result<PathBuf> {
    fs::create_dir_all(OUTPUT_DIR).with_context(|| format!("failed to create {OUTPUT_DIR}"))?;
    let report = json!({
        "schema": "mirante4d-xtask-command-audit",
        "schema_version": 2,
        "commands": COMMAND_AUDIT_ENTRIES
            .iter()
            .map(|entry| json!({
                "command": entry.command,
                "family": entry.family,
                "evidence_role": entry.evidence_role,
                "restricted": entry.restricted,
            }))
            .collect::<Vec<_>>(),
    });
    write_json_file(std::path::Path::new(JSON_REPORT), &report)?;

    let mut markdown = String::from("# Mirante4D xtask commands\n\n");
    markdown.push_str("| Command | Family | Evidence role | Restricted |\n");
    markdown.push_str("| --- | --- | --- | --- |\n");
    for entry in COMMAND_AUDIT_ENTRIES {
        markdown.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            entry.command, entry.family, entry.evidence_role, entry.restricted
        ));
    }
    fs::write(MARKDOWN_REPORT, markdown)
        .with_context(|| format!("failed to write {MARKDOWN_REPORT}"))?;
    Ok(PathBuf::from(JSON_REPORT))
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
        "package-dev",
        "package-linux-release",
        "app-smoke",
        "product-validate",
        "bench-check",
        "neuroglancer-compare",
        "workflow-audit",
        "docs-check",
        "command-audit",
        "run-dev",
    ];

    #[test]
    fn audit_covers_the_live_command_surface_once() {
        let audited = COMMAND_AUDIT_ENTRIES
            .iter()
            .map(|entry| entry.command)
            .collect::<BTreeSet<_>>();
        let handled = HANDLED_COMMANDS.iter().copied().collect::<BTreeSet<_>>();

        assert_eq!(audited.len(), COMMAND_AUDIT_ENTRIES.len());
        assert_eq!(audited, handled);
    }

    #[test]
    fn expired_predecessor_commands_are_absent() {
        let audited = COMMAND_AUDIT_ENTRIES
            .iter()
            .map(|entry| entry.command)
            .collect::<BTreeSet<_>>();

        for removed in [
            "generate-fixture",
            "bench-native-package",
            "bench-import-sample",
            "phase17-audit",
            "phase20-extreme-audit",
        ] {
            assert!(!audited.contains(removed));
        }
    }
}

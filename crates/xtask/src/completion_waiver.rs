use std::{
    collections::BTreeSet,
    env,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::{host::benchmark_host_context, reports::write_json_file};

const COMPLETION_WAIVER_JSON: &str = "target/mirante4d/completion-waivers/completion-waivers.json";
const COMPLETION_WAIVER_SCHEMA: &str = "mirante4d-completion-waivers";
const COMPLETION_WAIVER_SCHEMA_VERSION: u32 = 1;
const COMPLETION_SCOPE: &str = "product_validation_testing_refactor_completion";

pub(crate) fn completion_waiver() -> anyhow::Result<PathBuf> {
    let report = completion_waiver_report_json(|name| env::var(name).ok())?;
    let path = PathBuf::from(COMPLETION_WAIVER_JSON);
    write_json_file(&path, &report)?;
    Ok(path)
}

fn completion_waiver_report_json<F>(mut env_var: F) -> anyhow::Result<Value>
where
    F: FnMut(&str) -> Option<String>,
{
    let approval = required_env(&mut env_var, "MIRANTE4D_COMPLETION_WAIVER_USER_APPROVED")?;
    if approval != "1" {
        bail!("MIRANTE4D_COMPLETION_WAIVER_USER_APPROVED must be 1");
    }

    let approved_by = required_env(&mut env_var, "MIRANTE4D_COMPLETION_WAIVER_APPROVED_BY")?;
    let reason = required_env(&mut env_var, "MIRANTE4D_COMPLETION_WAIVER_REASON")?;
    let milestone = required_env(&mut env_var, "MIRANTE4D_COMPLETION_WAIVER_MILESTONE")?;
    let items = required_env(&mut env_var, "MIRANTE4D_COMPLETION_WAIVER_ITEMS")?;
    let items = parse_waiver_items(&items)?;
    validate_waiver_items(&items)?;
    let waivers = items
        .into_iter()
        .map(|item| {
            json!({
                "scope": COMPLETION_SCOPE,
                "code": item.code,
                "scenario": item.scenario,
                "reason": reason,
                "approved_by": approved_by,
                "milestone": milestone,
                "explicit_user_approval": true,
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "schema": COMPLETION_WAIVER_SCHEMA,
        "schema_version": COMPLETION_WAIVER_SCHEMA_VERSION,
        "command": "completion-waiver",
        "status": "passed",
        "scope": COMPLETION_SCOPE,
        "recorded_at_unix_ms": unix_time_ms(),
        "host": benchmark_host_context(),
        "summary": {
            "waiver_count": waivers.len(),
            "explicit_user_approval": true,
        },
        "waivers": waivers,
    }))
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedWaiverItem {
    code: String,
    scenario: Option<String>,
}

fn parse_waiver_items(raw: &str) -> anyhow::Result<Vec<ParsedWaiverItem>> {
    let mut items = Vec::new();
    for token in raw.split(',') {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (code, scenario) = trimmed
            .split_once(':')
            .map(|(code, scenario)| (code.trim(), Some(scenario.trim())))
            .unwrap_or((trimmed, None));
        if code.is_empty() {
            bail!("completion waiver item has empty blocker code");
        }
        let scenario = match scenario {
            Some("") => bail!("completion waiver item {code:?} has empty scenario"),
            Some(value) => Some(value.to_owned()),
            None => None,
        };
        items.push(ParsedWaiverItem {
            code: code.to_owned(),
            scenario,
        });
    }
    if items.is_empty() {
        bail!("MIRANTE4D_COMPLETION_WAIVER_ITEMS must name at least one blocker code");
    }
    Ok(items)
}

fn validate_waiver_items(items: &[ParsedWaiverItem]) -> anyhow::Result<()> {
    let mut seen = BTreeSet::new();
    for item in items {
        if !is_known_completion_blocker_code(&item.code) {
            bail!("completion waiver has unknown blocker code {:?}", item.code);
        }
        if completion_blocker_requires_scenario(&item.code) {
            if !matches!(
                item.scenario.as_deref(),
                Some(
                    "t5_qual_001_interaction_mip"
                        | "t5_qual_001_interaction_render_modes"
                        | "t5_qual_001_interaction_continuous"
                )
            ) {
                bail!(
                    "completion waiver {:?} requires exact scenario t5_qual_001_interaction_mip, t5_qual_001_interaction_render_modes, or t5_qual_001_interaction_continuous",
                    item.code
                );
            }
        } else if item.scenario.is_some() {
            bail!(
                "completion waiver {:?} must not include a scenario",
                item.code
            );
        }

        let identity = (&item.code, item.scenario.as_deref().unwrap_or(""));
        if !seen.insert(identity) {
            bail!("completion waiver {:?} is duplicated", item.code);
        }
    }
    Ok(())
}

fn is_known_completion_blocker_code(code: &str) -> bool {
    matches!(
        code,
        "baseline_refresh_pending"
            | "baseline_audit_missing"
            | "workflow_audit_not_current"
            | "workflow_audit_missing"
            | "external_ci_run_evidence_pending"
            | "external_ci_run_evidence_missing"
            | "t5_qual_001_product_open_validation_not_current"
            | "t5_qual_001_product_validation_report_missing"
    )
}

fn completion_blocker_requires_scenario(code: &str) -> bool {
    matches!(
        code,
        "t5_qual_001_product_open_validation_not_current"
            | "t5_qual_001_product_validation_report_missing"
    )
}

fn required_env<F>(env_var: &mut F, name: &str) -> anyhow::Result<String>
where
    F: FnMut(&str) -> Option<String>,
{
    let value = env_var(name)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("{name} is required"))?;
    Ok(value)
}

fn unix_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
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
    fn completion_waiver_requires_explicit_user_approval() {
        let env = env_map(&[
            ("MIRANTE4D_COMPLETION_WAIVER_USER_APPROVED", "0"),
            ("MIRANTE4D_COMPLETION_WAIVER_APPROVED_BY", "owner"),
            ("MIRANTE4D_COMPLETION_WAIVER_REASON", "milestone exception"),
            ("MIRANTE4D_COMPLETION_WAIVER_MILESTONE", "testing-refactor"),
            (
                "MIRANTE4D_COMPLETION_WAIVER_ITEMS",
                "external_ci_run_evidence_missing",
            ),
        ]);

        let err = completion_waiver_report_json(|key| env.get(key).cloned())
            .unwrap_err()
            .to_string();

        assert!(err.contains("MIRANTE4D_COMPLETION_WAIVER_USER_APPROVED must be 1"));
    }

    #[test]
    fn completion_waiver_report_records_code_and_scenario_items() {
        let env = env_map(&[
            ("MIRANTE4D_COMPLETION_WAIVER_USER_APPROVED", "1"),
            ("MIRANTE4D_COMPLETION_WAIVER_APPROVED_BY", "owner"),
            ("MIRANTE4D_COMPLETION_WAIVER_REASON", "milestone exception"),
            ("MIRANTE4D_COMPLETION_WAIVER_MILESTONE", "testing-refactor"),
            (
                "MIRANTE4D_COMPLETION_WAIVER_ITEMS",
                "baseline_refresh_pending,t5_qual_001_product_open_validation_not_current:t5_qual_001_interaction_mip",
            ),
        ]);

        let report = completion_waiver_report_json(|key| env.get(key).cloned()).unwrap();

        assert_eq!(report["schema"], COMPLETION_WAIVER_SCHEMA);
        assert_eq!(report["command"], "completion-waiver");
        assert_eq!(report["summary"]["waiver_count"], 2);
        assert_eq!(
            report["waivers"][1]["code"],
            "t5_qual_001_product_open_validation_not_current"
        );
        assert_eq!(
            report["waivers"][1]["scenario"],
            "t5_qual_001_interaction_mip"
        );
    }

    #[test]
    fn completion_waiver_item_parser_rejects_empty_items() {
        let err = parse_waiver_items(" , ").unwrap_err().to_string();

        assert!(err.contains("at least one blocker code"));
    }

    #[test]
    fn completion_waiver_rejects_unknown_blocker_codes_before_writing_report() {
        let env = env_map(&[
            ("MIRANTE4D_COMPLETION_WAIVER_USER_APPROVED", "1"),
            ("MIRANTE4D_COMPLETION_WAIVER_APPROVED_BY", "owner"),
            ("MIRANTE4D_COMPLETION_WAIVER_REASON", "milestone exception"),
            ("MIRANTE4D_COMPLETION_WAIVER_MILESTONE", "testing-refactor"),
            ("MIRANTE4D_COMPLETION_WAIVER_ITEMS", "not_a_blocker"),
        ]);

        let err = completion_waiver_report_json(|key| env.get(key).cloned())
            .unwrap_err()
            .to_string();

        assert!(err.contains("unknown blocker code"));
    }

    #[test]
    fn completion_waiver_validates_scenario_requirements_before_writing_report() {
        let missing_scenario = env_map(&[
            ("MIRANTE4D_COMPLETION_WAIVER_USER_APPROVED", "1"),
            ("MIRANTE4D_COMPLETION_WAIVER_APPROVED_BY", "owner"),
            ("MIRANTE4D_COMPLETION_WAIVER_REASON", "milestone exception"),
            ("MIRANTE4D_COMPLETION_WAIVER_MILESTONE", "testing-refactor"),
            (
                "MIRANTE4D_COMPLETION_WAIVER_ITEMS",
                "t5_qual_001_product_open_validation_not_current",
            ),
        ]);
        let err = completion_waiver_report_json(|key| missing_scenario.get(key).cloned())
            .unwrap_err()
            .to_string();
        assert!(err.contains("requires exact scenario"));

        let unexpected_scenario = env_map(&[
            ("MIRANTE4D_COMPLETION_WAIVER_USER_APPROVED", "1"),
            ("MIRANTE4D_COMPLETION_WAIVER_APPROVED_BY", "owner"),
            ("MIRANTE4D_COMPLETION_WAIVER_REASON", "milestone exception"),
            ("MIRANTE4D_COMPLETION_WAIVER_MILESTONE", "testing-refactor"),
            (
                "MIRANTE4D_COMPLETION_WAIVER_ITEMS",
                "external_ci_run_evidence_missing:t5_qual_001_interaction_mip",
            ),
        ]);
        let err = completion_waiver_report_json(|key| unexpected_scenario.get(key).cloned())
            .unwrap_err()
            .to_string();
        assert!(err.contains("must not include a scenario"));
    }

    #[test]
    fn completion_waiver_rejects_duplicate_items_before_writing_report() {
        let env = env_map(&[
            ("MIRANTE4D_COMPLETION_WAIVER_USER_APPROVED", "1"),
            ("MIRANTE4D_COMPLETION_WAIVER_APPROVED_BY", "owner"),
            ("MIRANTE4D_COMPLETION_WAIVER_REASON", "milestone exception"),
            ("MIRANTE4D_COMPLETION_WAIVER_MILESTONE", "testing-refactor"),
            (
                "MIRANTE4D_COMPLETION_WAIVER_ITEMS",
                "baseline_refresh_pending,baseline_refresh_pending",
            ),
        ]);

        let err = completion_waiver_report_json(|key| env.get(key).cloned())
            .unwrap_err()
            .to_string();

        assert!(err.contains("duplicated"));
    }
}

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};
use serde::Deserialize;
use serde_json::{Value, json};

const REGISTRY_PATH: &str = "verification/registry.json";
const SELECTORS_PATH: &str = "verification/generated/selectors.json";
const DOCTESTS_PATH: &str = "verification/generated/doctests.json";
const NEXTEST_CONFIG_PATH: &str = ".config/nextest.toml";

#[derive(Debug, Deserialize)]
pub(super) struct Registry {
    schema: String,
    schema_version: u32,
    tools: Vec<ToolPin>,
    property_groups: Vec<PropertyGroup>,
    pub(super) lint_exceptions: Vec<LintException>,
    #[serde(default)]
    test_timeout_overrides: Vec<TestTimeoutOverride>,
    pub(super) lanes: Vec<Lane>,
    non_pr_lanes: Vec<NonPrLane>,
    pub(super) selector_adapters: Vec<SelectorAdapter>,
}

#[derive(Debug, Deserialize)]
struct ToolPin {
    id: String,
    version: String,
    artifact_sha256: Option<String>,
    binary_sha256: Option<String>,
    action_commit: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PropertyGroup {
    id: String,
    owner: String,
    seed: String,
    cases: u64,
    max_shrink_iters: u64,
    persistence: bool,
    replay: String,
}

#[derive(Debug, Deserialize)]
struct TestTimeoutOverride {
    selector: String,
    warn_ms: u64,
    terminate_ms: u64,
    threads_required: Option<String>,
    reason: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct LintWarningKey {
    pub(super) code: String,
    pub(super) path: String,
    pub(super) primary_line: u64,
}

#[derive(Debug, Deserialize)]
pub(super) struct LintException {
    pub(super) code: String,
    pub(super) path: String,
    pub(super) primary_line: u64,
    reason: String,
    deletion_gate: String,
}

impl LintException {
    pub(super) fn key(&self) -> LintWarningKey {
        LintWarningKey {
            code: self.code.clone(),
            path: self.path.clone(),
            primary_line: self.primary_line,
        }
    }

    pub(super) fn reason(&self) -> &str {
        &self.reason
    }

    pub(super) fn deletion_gate(&self) -> &str {
        &self.deletion_gate
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct Lane {
    pub(super) id: String,
    kind: String,
    owner: String,
    requirements: Vec<String>,
    fixture_tier: String,
    capability: String,
    hosted_required: bool,
    pub(super) packages: Vec<String>,
    pub(super) selector: Option<String>,
    warn_ms: u64,
    terminate_ms: u64,
    pub(super) aggregate_timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
pub(super) struct SelectorAdapter {
    id: String,
    lane: String,
    owner: String,
    requirements: Vec<String>,
    fixture_tier: String,
    capability: String,
    matches: Vec<SelectorMatch>,
    expected_ignored_cases: u64,
    expiry: String,
    deletion_gate: String,
}

#[derive(Debug, Deserialize)]
struct SelectorMatch {
    package: String,
    test_prefix: String,
}

#[derive(Debug, Deserialize)]
struct NonPrLane {
    id: String,
    owner: String,
    capability: String,
    requirements: Vec<String>,
    fixture_tier: String,
    timeout_secs: u64,
    evidence_level: String,
    trigger: String,
    command: String,
    hosted_required: bool,
    activation_state: String,
}

pub(super) fn read_registry() -> anyhow::Result<Registry> {
    let path = repo_path(REGISTRY_PATH);
    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let registry: Registry = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    validate_registry(&registry)?;
    Ok(registry)
}

pub(super) fn sync_generated(check: bool) -> anyhow::Result<()> {
    let registry = read_registry()?;
    let selectors = generated_selectors(&registry)?;
    let doctests = generated_doctests()?;
    let nextest = generated_nextest(&registry)?;
    sync_one(SELECTORS_PATH, selectors.as_bytes(), check)?;
    sync_one(DOCTESTS_PATH, doctests.as_bytes(), check)?;
    sync_one(NEXTEST_CONFIG_PATH, nextest.as_bytes(), check)?;
    println!(
        "verification-sync: {} generated selectors, doctest inventory, and Nextest configuration",
        if check { "checked" } else { "wrote" }
    );
    Ok(())
}

fn generated_doctests() -> anyhow::Result<String> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args(["ls-files", "-z", "--", "crates/**/*.rs"])
        .output()
        .context("failed to inventory tracked Rust sources for doctests")?;
    if !output.status.success() {
        bail!("git ls-files failed while inventorying doctests");
    }
    let mut discovered = Vec::new();
    for encoded in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let relative = std::str::from_utf8(encoded).context("tracked Rust path was not UTF-8")?;
        let path = repo_root.join(relative);
        if !path.is_file() {
            continue;
        }
        let source = fs::read_to_string(path)?;
        if source.lines().any(|line| {
            let trimmed = line.trim_start();
            (trimmed.starts_with("///") || trimmed.starts_with("//!")) && trimmed.contains("```")
        }) {
            discovered.push(relative.to_owned());
        }
    }
    if !discovered.is_empty() {
        bail!(
            "tracked Rust doctest sources are not represented by the zero-case inventory: {discovered:?}"
        );
    }
    let value = json!({
        "schema": "mirante4d-verification-doctest-inventory",
        "schema_version": 1,
        "source": "tracked crates/**/*.rs doc comments",
        "cases": [],
    });
    let mut encoded = serde_json::to_string_pretty(&value)?;
    encoded.push('\n');
    Ok(encoded)
}

fn safe_repo_relative_path(relative: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        bail!("verification path must be a normalized repository-relative path: {relative:?}");
    }
    Ok(repo_path(relative))
}

pub(super) fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .with_context(|| format!("failed to hash {}", path.display()))?;
    if !output.status.success() {
        bail!("sha256sum failed for {}", path.display());
    }
    String::from_utf8(output.stdout)?
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .context("sha256sum returned no digest")
}

fn sync_one(relative: &str, expected: &[u8], check: bool) -> anyhow::Result<()> {
    let path = repo_path(relative);
    if check {
        let actual = fs::read(&path).with_context(|| {
            format!("generated verification file is missing: {}", path.display())
        })?;
        if actual != expected {
            bail!(
                "generated verification file is stale: {relative}; run `cargo xtask verification-sync`"
            );
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&path, expected).with_context(|| format!("failed to write {}", path.display()))
}

fn validate_registry(registry: &Registry) -> anyhow::Result<()> {
    if registry.schema != "mirante4d-verification-registry" || registry.schema_version != 1 {
        bail!("verification registry has unsupported schema identity");
    }
    validate_tool_pins(&registry.tools)?;
    validate_property_groups(&registry.property_groups)?;
    validate_lint_exceptions(&registry.lint_exceptions)?;
    validate_test_timeout_overrides(&registry.test_timeout_overrides)?;

    let required = ["policy", "lint", "unit", "contract", "ui", "doctest"];
    let allowed_fixture_tiers = [
        "none",
        "none-or-T2",
        "T1-or-T2",
        "T1-source",
        "T1-source-or-T2",
        "T1-target",
        "T2",
        "T5",
    ];
    let mut lane_ids = BTreeSet::new();
    let mut package_owners = BTreeMap::<&str, &str>::new();
    for lane in &registry.lanes {
        if !lane_ids.insert(lane.id.as_str()) {
            bail!("duplicate verification lane {:?}", lane.id);
        }
        if lane.owner.trim().is_empty()
            || lane.requirements.is_empty()
            || !allowed_fixture_tiers.contains(&lane.fixture_tier.as_str())
            || lane.capability.trim().is_empty()
            || lane.warn_ms == 0
            || lane.terminate_ms < lane.warn_ms
            || lane.aggregate_timeout_secs == 0
        {
            bail!(
                "verification lane {:?} has incomplete policy metadata",
                lane.id
            );
        }
        if lane.kind == "nextest" {
            if !lane.hosted_required || lane.packages.is_empty() || lane.selector.is_none() {
                bail!(
                    "Nextest lane {:?} is missing its structural selector",
                    lane.id
                );
            }
            for package in &lane.packages {
                if let Some(previous) = package_owners.insert(package, lane.id.as_str()) {
                    bail!(
                        "normal package {package:?} is assigned to both {previous:?} and {:?}",
                        lane.id
                    );
                }
            }
        } else if lane.selector.is_some() || !lane.packages.is_empty() {
            bail!("non-Nextest lane {:?} must not own test packages", lane.id);
        }
    }
    if lane_ids != required.into_iter().collect() {
        bail!("verification registry must contain exactly the six required leaves");
    }

    let mut all_lane_ids = lane_ids;
    for lane in &registry.non_pr_lanes {
        if !all_lane_ids.insert(lane.id.as_str())
            || lane.owner.trim().is_empty()
            || lane.capability.trim().is_empty()
            || lane.requirements.is_empty()
            || lane.fixture_tier.trim().is_empty()
            || lane.timeout_secs == 0
            || lane.evidence_level.trim().is_empty()
            || lane.trigger.trim().is_empty()
            || lane.command.trim().is_empty()
            || lane.hosted_required
            || lane.activation_state.trim().is_empty()
        {
            bail!(
                "non-PR verification lane {:?} is incomplete, duplicate, or marked required",
                lane.id
            );
        }
    }
    let expected_non_pr = BTreeSet::from([
        "developer-local",
        "format-lifecycle",
        "linux-release",
        "project-store-lifecycle",
        "trusted-gpu",
    ]);
    let actual_non_pr = registry
        .non_pr_lanes
        .iter()
        .map(|lane| lane.id.as_str())
        .collect::<BTreeSet<_>>();
    if actual_non_pr != expected_non_pr {
        bail!("verification registry non-PR lane set drifted");
    }

    let mut adapter_ids = BTreeSet::new();
    for adapter in &registry.selector_adapters {
        if !adapter_ids.insert(adapter.id.as_str())
            || adapter.lane.trim().is_empty()
            || adapter.owner.trim().is_empty()
            || adapter.requirements.is_empty()
            || !allowed_fixture_tiers.contains(&adapter.fixture_tier.as_str())
            || adapter.capability.trim().is_empty()
            || adapter.matches.is_empty()
            || adapter.expected_ignored_cases == 0
            || adapter.expiry.trim().is_empty()
            || adapter.deletion_gate.trim().is_empty()
        {
            bail!(
                "selector adapter {:?} has incomplete or duplicate metadata",
                adapter.id
            );
        }
        if !all_lane_ids.contains(adapter.lane.as_str()) {
            bail!("selector adapter {:?} names an unknown lane", adapter.id);
        }
    }
    Ok(())
}

fn validate_test_timeout_overrides(overrides: &[TestTimeoutOverride]) -> anyhow::Result<()> {
    let mut selectors = BTreeSet::new();
    for timeout in overrides {
        if !selectors.insert(timeout.selector.as_str())
            || !timeout.selector.contains("package(")
            || !timeout.selector.contains("test(=")
            || timeout.selector.contains('\'')
            || timeout.warn_ms == 0
            || timeout.terminate_ms < timeout.warn_ms
            || timeout.terminate_ms % timeout.warn_ms != 0
            || timeout
                .threads_required
                .as_deref()
                .is_some_and(|value| value != "num-test-threads")
            || timeout.reason.trim().is_empty()
        {
            bail!("per-test timeout override has incomplete policy metadata");
        }
    }
    Ok(())
}

fn validate_lint_exceptions(exceptions: &[LintException]) -> anyhow::Result<()> {
    let allowed_deletion_gates = BTreeSet::from(["WP-07B", "WP-09B", "WP-09C", "WP-14"]);
    let mut keys = BTreeSet::new();
    for exception in exceptions {
        let key = exception.key();
        if !keys.insert(key.clone()) {
            bail!("duplicate inherited lint exception: {key:?}");
        }
        if !exception.code.starts_with("clippy::")
            || exception.primary_line == 0
            || exception.reason.trim().is_empty()
            || !allowed_deletion_gates.contains(exception.deletion_gate.as_str())
        {
            bail!("inherited lint exception has incomplete policy metadata: {key:?}");
        }
        let source_path = safe_repo_relative_path(&exception.path)?;
        if source_path.extension().and_then(|value| value.to_str()) != Some("rs")
            || !source_path.is_file()
        {
            bail!(
                "inherited lint exception path is not a tracked Rust source: {:?}",
                exception.path
            );
        }
        let line_count = fs::read_to_string(&source_path)
            .with_context(|| format!("failed to read {}", source_path.display()))?
            .lines()
            .count() as u64;
        if exception.primary_line > line_count {
            bail!(
                "inherited lint exception line {} is outside {:?} ({line_count} lines)",
                exception.primary_line,
                exception.path
            );
        }
    }
    Ok(())
}

fn validate_tool_pins(tools: &[ToolPin]) -> anyhow::Result<()> {
    let expected = BTreeMap::from([
        (
            "actions-checkout",
            (
                "4.2.2",
                None,
                None,
                Some("11bd71901bbe5b1630ceea73d27597364c9af683"),
            ),
        ),
        (
            "cargo-deny",
            (
                "0.20.2",
                Some("9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f"),
                None,
                None,
            ),
        ),
        (
            "cargo-nextest",
            (
                "0.9.138",
                Some("3793bf0c27607b196f502c39b2108f571de89fcda7586ae6beefa11ee177b216"),
                None,
                None,
            ),
        ),
        (
            "rumdl",
            (
                "0.2.30",
                Some("eb51e28ef9dff2b2d29b4527bc40123e840bb997dc8bae39d99496b898ee9f72"),
                None,
                None,
            ),
        ),
        (
            "sourcemeta-jsonschema",
            (
                "16.1.0",
                Some("96b214be67bf25c6184f1d009a94e082d1eaa83787a8f1878607aebf3185668e"),
                Some("4aa8ba3f4bc0b1ef4f8d82b109676b186fa66603d1953be25fde22b2854190d5"),
                None,
            ),
        ),
    ]);
    if tools.len() != expected.len() {
        bail!("verification registry tool pin set drifted");
    }
    let mut ids = BTreeSet::new();
    for tool in tools {
        let pin = expected
            .get(tool.id.as_str())
            .with_context(|| format!("unknown verification tool pin {:?}", tool.id))?;
        if !ids.insert(tool.id.as_str())
            || tool.version != pin.0
            || tool.artifact_sha256.as_deref() != pin.1
            || tool.binary_sha256.as_deref() != pin.2
            || tool.action_commit.as_deref() != pin.3
        {
            bail!("verification tool pin {:?} drifted", tool.id);
        }
    }
    Ok(())
}

fn validate_property_groups(groups: &[PropertyGroup]) -> anyhow::Result<()> {
    let expected = BTreeMap::from([
        (
            "domain-display",
            ("mirante4d-domain", "0x4d34444f4d444953", 64),
        ),
        (
            "domain-geometry",
            ("mirante4d-domain", "0x4d34444f4d47454f", 64),
        ),
        (
            "domain-render",
            ("mirante4d-domain", "0x4d34444f4d52454e", 64),
        ),
        (
            "domain-shape",
            ("mirante4d-domain", "0x4d34444f4d534850", 64),
        ),
        (
            "domain-view",
            ("mirante4d-domain", "0x4d34444f4d564945", 64),
        ),
        (
            "identity-digest",
            ("mirante4d-identity", "0x4d34494444494731", 128),
        ),
        (
            "project-model",
            ("mirante4d-project-model", "0x4d3450524f4a4d4f", 128),
        ),
    ]);
    if groups.len() != expected.len() {
        bail!("verification property group set drifted");
    }
    let mut ids = BTreeSet::new();
    for group in groups {
        let Some((owner, seed, cases)) = expected.get(group.id.as_str()) else {
            bail!("unknown verification property group {:?}", group.id);
        };
        if !ids.insert(group.id.as_str())
            || group.owner != *owner
            || group.seed != *seed
            || group.cases != *cases
            || group.max_shrink_iters != 1024
            || group.persistence
            || group.replay != "fixed-seed"
        {
            bail!("verification property group {:?} drifted", group.id);
        }
    }
    Ok(())
}

fn generated_selectors(registry: &Registry) -> anyhow::Result<String> {
    let mut lanes = BTreeMap::new();
    for lane in required_test_lanes(registry)? {
        lanes.insert(
            lane.id.clone(),
            lane.selector
                .clone()
                .context("validated Nextest lane missing selector")?,
        );
    }
    let mut adapters = BTreeMap::new();
    for adapter in &registry.selector_adapters {
        adapters.insert(adapter.id.clone(), adapter_selector(adapter));
    }
    let value = json!({
        "schema": "mirante4d-verification-selectors",
        "schema_version": 1,
        "source": REGISTRY_PATH,
        "required_union": "group(unit) | group(contract) | group(ui)",
        "lanes": lanes,
        "selector_adapters": adapters,
    });
    let mut encoded = serde_json::to_string_pretty(&value)?;
    encoded.push('\n');
    Ok(encoded)
}

fn generated_nextest(registry: &Registry) -> anyhow::Result<String> {
    let mut text = String::from(
        "# Generated by `cargo xtask verification-sync`; edit verification/registry.json.\n\n\
         [profile.default]\n\
         retries = 0\n\
         flaky-result = \"fail\"\n\
         fail-fast = false\n\
         test-threads = 4\n\n\
         [test-groups]\n\
         unit = { max-threads = 4 }\n\
         contract = { max-threads = 2 }\n\
         ui = { max-threads = 1 }\n",
    );
    for timeout in &registry.test_timeout_overrides {
        text.push_str(&format!(
            "\n[[profile.default.overrides]]\nfilter = '{}'\n",
            timeout.selector,
        ));
        if let Some(threads_required) = &timeout.threads_required {
            text.push_str(&format!("threads-required = \"{threads_required}\"\n"));
        }
        text.push_str(&format!(
            "slow-timeout = {{ period = \"{}\", terminate-after = {} }}\n",
            duration_text(timeout.warn_ms),
            timeout.terminate_ms / timeout.warn_ms,
        ));
    }
    for lane in required_test_lanes(registry)? {
        let selector = lane
            .selector
            .as_deref()
            .context("validated Nextest lane missing selector")?;
        if selector.contains('\'') {
            bail!(
                "lane {:?} selector cannot contain a TOML literal quote",
                lane.id
            );
        }
        if lane.terminate_ms % lane.warn_ms != 0 {
            bail!(
                "lane {:?} timeout is not an exact warning-period multiple",
                lane.id
            );
        }
        text.push_str(&format!(
            "\n[[profile.default.overrides]]\nfilter = '{selector}'\ntest-group = '{}'\nslow-timeout = {{ period = \"{}\", terminate-after = {} }}\n",
            lane.id,
            duration_text(lane.warn_ms),
            lane.terminate_ms / lane.warn_ms,
        ));
    }
    text.push_str(
        "\n[profile.leaf]\n\
         inherits = \"default\"\n\
         global-timeout = \"12m\"\n\n\
         [profile.leaf.junit]\n\
         path = \"junit.xml\"\n\
         report-name = \"mirante4d-verification-leaf\"\n\
         store-success-output = false\n\
         store-failure-output = true\n\n\
         [profile.pr]\n\
         inherits = \"default\"\n\
         global-timeout = \"5m\"\n\n\
         [profile.pr.junit]\n\
         path = \"junit.xml\"\n\
         report-name = \"mirante4d-pr-rust\"\n\
         store-success-output = false\n\
         store-failure-output = true\n",
    );
    let trusted_selector = lane_adapter_selector(registry, "trusted-gpu")?;
    if trusted_selector.contains('\'') {
        bail!("trusted-gpu selector cannot contain a TOML literal quote");
    }
    text.push_str(&format!(
        "\n[profile.trusted-gpu]\n\
         inherits = \"default\"\n\
         test-threads = 1\n\
         global-timeout = \"15m\"\n\n\
         [[profile.trusted-gpu.overrides]]\n\
         filter = '{trusted_selector}'\n\
         slow-timeout = {{ period = \"20s\", terminate-after = 3 }}\n"
    ));
    Ok(text)
}

pub(super) fn trusted_gpu_policy(registry: &Registry) -> anyhow::Result<(String, u64)> {
    let lane = registry
        .non_pr_lanes
        .iter()
        .find(|lane| lane.id == "trusted-gpu")
        .context("verification registry is missing trusted-gpu lane")?;
    Ok((
        lane_adapter_selector(registry, "trusted-gpu")?,
        lane.timeout_secs,
    ))
}

pub(super) fn format_lifecycle_timeout(registry: &Registry) -> anyhow::Result<u64> {
    let lane = registry
        .non_pr_lanes
        .iter()
        .find(|lane| lane.id == "format-lifecycle")
        .context("verification registry is missing format-lifecycle lane")?;
    if lane.command != "cargo xtask verify-local format-lifecycle"
        || lane.fixture_tier != "T1-target"
        || lane.capability != "standard-public-cpu"
        || lane.timeout_secs != 900
        || lane.evidence_level != "E0"
        || lane.activation_state != "active-WP-10A"
    {
        bail!("format-lifecycle verification policy drifted");
    }
    Ok(lane.timeout_secs)
}

pub(super) fn project_store_lifecycle_timeout(registry: &Registry) -> anyhow::Result<u64> {
    let lane = registry
        .non_pr_lanes
        .iter()
        .find(|lane| lane.id == "project-store-lifecycle")
        .context("verification registry is missing project-store-lifecycle lane")?;
    if lane.command != "cargo xtask verify-local project-store-lifecycle"
        || lane.owner != "project-store"
        || lane.fixture_tier != "T1-project-or-T2"
        || lane.capability != "trusted-local-kvm-ext4"
        || lane.requirements != ["WP-10B-project-store"]
        || lane.timeout_secs != 900
        || lane.evidence_level != "E0"
        || lane.hosted_required
        || lane.activation_state != "active-WP-10B-B2"
    {
        bail!("project-store-lifecycle verification policy drifted");
    }
    Ok(lane.timeout_secs)
}

fn required_test_lanes(registry: &Registry) -> anyhow::Result<Vec<&Lane>> {
    ["unit", "contract", "ui"]
        .into_iter()
        .map(|id| {
            registry
                .lanes
                .iter()
                .find(|lane| lane.id == id)
                .with_context(|| format!("verification registry is missing {id:?}"))
        })
        .collect()
}

fn adapter_selector(adapter: &SelectorAdapter) -> String {
    let parts = adapter
        .matches
        .iter()
        .map(|matcher| {
            format!(
                "package({}) & test(/{}/)",
                matcher.package,
                regex_prefix(&matcher.test_prefix)
            )
        })
        .collect::<Vec<_>>();
    if parts.len() == 1 {
        parts[0].clone()
    } else {
        parts
            .into_iter()
            .map(|part| format!("({part})"))
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

fn lane_adapter_selector(registry: &Registry, lane: &str) -> anyhow::Result<String> {
    let selectors = registry
        .selector_adapters
        .iter()
        .filter(|adapter| adapter.lane == lane)
        .map(adapter_selector)
        .collect::<Vec<_>>();
    if selectors.is_empty() {
        bail!("verification registry is missing {lane} adapter");
    }
    Ok(selectors
        .into_iter()
        .map(|selector| format!("({selector})"))
        .collect::<Vec<_>>()
        .join(" | "))
}

fn regex_prefix(prefix: &str) -> String {
    let mut escaped = String::from("^");
    for character in prefix.chars() {
        if matches!(
            character,
            '\\' | '/'
                | '.'
                | '+'
                | '*'
                | '?'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '^'
                | '$'
                | '|'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn duration_text(milliseconds: u64) -> String {
    if milliseconds.is_multiple_of(1000) {
        format!("{}s", milliseconds / 1000)
    } else {
        format!("{milliseconds}ms")
    }
}

pub(super) fn audit_discovery(
    path: &Path,
    registry: &Registry,
) -> anyhow::Result<BTreeMap<String, u64>> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let discovery: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let suites = discovery
        .get("rust-suites")
        .and_then(Value::as_object)
        .context("Nextest discovery is missing rust-suites")?;
    let test_lanes = required_test_lanes(registry)?;
    let mut counts = BTreeMap::<String, u64>::new();
    let mut adapter_counts = BTreeMap::<String, u64>::new();

    for suite in suites.values() {
        let package = suite
            .get("package-name")
            .and_then(Value::as_str)
            .context("Nextest suite is missing package-name")?;
        let testcases = suite
            .get("testcases")
            .and_then(Value::as_object)
            .context("Nextest suite is missing testcases")?;
        for (test_name, testcase) in testcases {
            let ignored = testcase
                .get("ignored")
                .and_then(Value::as_bool)
                .context("Nextest testcase is missing ignored state")?;
            if ignored {
                let matched = registry
                    .selector_adapters
                    .iter()
                    .filter(|adapter| adapter_matches(adapter, package, test_name))
                    .collect::<Vec<_>>();
                if matched.len() != 1 {
                    bail!(
                        "ignored case {package}::{test_name} matched {} selector adapters, expected exactly one",
                        matched.len()
                    );
                }
                *counts.entry(matched[0].lane.clone()).or_default() += 1;
                *adapter_counts.entry(matched[0].id.clone()).or_default() += 1;
            } else {
                let matched = test_lanes
                    .iter()
                    .filter(|lane| lane.packages.iter().any(|candidate| candidate == package))
                    .collect::<Vec<_>>();
                if matched.len() != 1 {
                    bail!(
                        "normal case {package}::{test_name} matched {} required lanes, expected exactly one",
                        matched.len()
                    );
                }
                *counts.entry(matched[0].id.clone()).or_default() += 1;
            }
        }
    }

    for adapter in &registry.selector_adapters {
        let actual = adapter_counts.get(&adapter.id).copied().unwrap_or(0);
        if actual != adapter.expected_ignored_cases {
            bail!(
                "selector adapter {:?} expected {} ignored cases, discovered {actual}",
                adapter.id,
                adapter.expected_ignored_cases
            );
        }
    }
    for lane in test_lanes {
        if counts.get(&lane.id).copied().unwrap_or(0) == 0 {
            bail!("required lane {:?} discovered no cases", lane.id);
        }
    }
    Ok(counts)
}

fn adapter_matches(adapter: &SelectorAdapter, package: &str, test_name: &str) -> bool {
    adapter
        .matches
        .iter()
        .any(|matcher| matcher.package == package && test_name.starts_with(&matcher.test_prefix))
}

pub(super) fn repo_path(relative: &str) -> PathBuf {
    let cwd = PathBuf::from(relative);
    if cwd.exists() {
        return cwd;
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_exact_required_leaf_set() {
        let registry = read_registry().unwrap();
        let ids = registry
            .lanes
            .iter()
            .map(|lane| lane.id.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            ids,
            BTreeSet::from(["policy", "lint", "unit", "contract", "ui", "doctest"])
        );
    }

    #[test]
    fn format_lifecycle_is_an_active_target_authority_consumer() {
        let registry = read_registry().unwrap();
        assert_eq!(format_lifecycle_timeout(&registry).unwrap(), 900);
    }

    #[test]
    fn project_store_lifecycle_is_an_active_trusted_local_lane() {
        let registry = read_registry().unwrap();
        assert_eq!(project_store_lifecycle_timeout(&registry).unwrap(), 900);
    }

    #[test]
    fn generated_files_are_current() {
        sync_generated(true).unwrap();
    }

    #[test]
    fn adapter_selectors_are_anchored() {
        let registry = read_registry().unwrap();
        for adapter in &registry.selector_adapters {
            let selector = adapter_selector(adapter);
            assert!(selector.contains("test(/^"));
            assert!(!selector.contains("all()"));
        }
    }
}

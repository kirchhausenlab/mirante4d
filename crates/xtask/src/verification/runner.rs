use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use serde::Serialize;
use serde_json::Value;

use crate::process::{cargo_command, ensure_nextest, run_command_with_timeout};

use super::{fixtures, registry};

const FORMAT_TIMEOUT: Duration = Duration::from_secs(60);
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const PR_TEST_UNION_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const DOCTEST_PROCESS_TIMEOUT: Duration = Duration::from_secs(120);
const SOURCE_FIXTURE_VALIDATION_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Leaf {
    Policy,
    Lint,
    Unit,
    Contract,
    Ui,
    Doctest,
}

impl Leaf {
    pub(crate) fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "policy" => Ok(Self::Policy),
            "lint" => Ok(Self::Lint),
            "unit" => Ok(Self::Unit),
            "contract" => Ok(Self::Contract),
            "ui" => Ok(Self::Ui),
            "doctest" => Ok(Self::Doctest),
            _ => bail!(
                "unknown verification leaf {value:?}; expected policy|lint|unit|contract|ui|doctest"
            ),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Policy => "policy",
            Self::Lint => "lint",
            Self::Unit => "unit",
            Self::Contract => "contract",
            Self::Ui => "ui",
            Self::Doctest => "doctest",
        }
    }
}

pub(crate) fn verify_leaf(leaf: Leaf) -> anyhow::Result<()> {
    let started = Instant::now();
    println!("verification leaf {}: starting", leaf.name());
    let identity = RunIdentity::gather()?;
    let mut phases = PhaseCollector::default();
    phases.record_identity(&identity);
    match leaf {
        Leaf::Policy => run_policy_phases(&mut phases),
        Leaf::Lint => {
            phases.run("lint", lint_command(), run_lint);
        }
        Leaf::Unit | Leaf::Contract | Leaf::Ui => {
            phases.run(leaf.name(), nextest_leaf_command(leaf), || {
                run_nextest_leaf(leaf)
            });
        }
        Leaf::Doctest => {
            phases.run("doctest", doctest_command(), run_doctest);
        }
    }
    let report_path = verification_report_path(&format!("verify-leaf-{}", leaf.name()))?;
    let report_result = phases.write_report(&report_path, leaf.name(), &identity);
    let result = phases.finish(leaf.name()).and(report_result);
    println!(
        "verification leaf {}: {} after {:.3}s",
        leaf.name(),
        if result.is_ok() { "passed" } else { "failed" },
        started.elapsed().as_secs_f64()
    );
    result
}

pub(crate) fn verify_pr(group: Option<&str>) -> anyhow::Result<()> {
    match group {
        Some("policy") => verify_pr_policy(),
        Some("rust") => verify_pr_rust(),
        Some(other) => bail!("unknown verify-pr group {other:?}; expected policy|rust"),
        None => {
            let policy = verify_pr_policy();
            let rust = verify_pr_rust();
            match (policy, rust) {
                (Ok(()), Ok(())) => Ok(()),
                (policy, rust) => bail!(
                    "verify-pr failed: policy={}; rust={}",
                    outcome_text(&policy),
                    outcome_text(&rust)
                ),
            }
        }
    }
}

pub(crate) fn verify_local(lane: &str) -> anyhow::Result<()> {
    if lane != "trusted-gpu" {
        bail!("unknown local verification lane {lane:?}; expected trusted-gpu");
    }
    if env::var("GITHUB_ACTIONS").as_deref() == Ok("true") {
        bail!("trusted-gpu verification must not run in GitHub Actions");
    }
    if env::var("MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL").as_deref() != Ok("1") {
        bail!(
            "trusted-gpu verification requires MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 on the trusted machine"
        );
    }
    ensure_nextest()?;
    let registry = registry::read_registry()?;
    let (selector, timeout_secs) = registry::trusted_gpu_policy(&registry)?;
    let identity = RunIdentity::gather()?;
    let mut phases = PhaseCollector::default();
    phases.record_identity(&identity);
    let command_text = format!(
        "NEXTEST_USER_CONFIG_FILE=none cargo nextest run --workspace --frozen --profile trusted-gpu --run-ignored only --no-fail-fast --retries 0 --flaky-result fail --no-tests fail -E '{selector}'"
    );
    phases.run("trusted-gpu", command_text, || {
        let mut command = isolated_nextest_command();
        command.args([
            "nextest",
            "run",
            "--workspace",
            "--frozen",
            "--profile",
            "trusted-gpu",
            "--run-ignored",
            "only",
            "--no-fail-fast",
            "--retries",
            "0",
            "--flaky-result",
            "fail",
            "--no-tests",
            "fail",
            "-E",
            &selector,
        ]);
        run_command_with_timeout(&mut command, Duration::from_secs(timeout_secs))
    });
    let report_path = verification_report_path("verify-local-trusted-gpu")?;
    let report_result = phases.write_report(&report_path, "trusted-gpu", &identity);
    phases.finish("trusted-gpu").and(report_result)
}

fn verify_pr_policy() -> anyhow::Result<()> {
    let identity = RunIdentity::gather()?;
    let mut phases = PhaseCollector::default();
    phases.record_identity(&identity);
    run_policy_phases(&mut phases);
    let report_path = verification_report_path("verify-pr-policy")?;
    let report_result = phases.write_report(&report_path, "policy", &identity);
    phases.finish("policy").and(report_result)
}

fn run_policy_phases(phases: &mut PhaseCollector) {
    phases.run("format", "cargo fmt --all --check", || {
        let mut command = cargo_command();
        command.args(["fmt", "--all", "--check"]);
        run_command_with_timeout(&mut command, FORMAT_TIMEOUT)
    });
    phases.run(
        "verification-registry",
        "cargo xtask verification-sync --check",
        || registry::sync_generated(true),
    );
    phases.run(
        "fixture-registry",
        "in-process fixture registry validation",
        fixtures::validate_fixture_registry,
    );
    phases.run(
        "source-fixture-validation",
        "python3 tools/source-fixtures/validate.py --self-test --reader-report fixtures/source/independent-reader-report.json",
        || {
            let mut command = Command::new("python3");
            command.args([
                "tools/source-fixtures/validate.py",
                "--self-test",
                "--reader-report",
                "fixtures/source/independent-reader-report.json",
            ]);
            run_command_with_timeout(&mut command, SOURCE_FIXTURE_VALIDATION_TIMEOUT)
        },
    );
    phases.run(
        "architecture",
        "in-process architecture self-check",
        crate::arch::architecture_self_check,
    );
    phases.run(
        "documentation",
        "cargo xtask docs-check",
        crate::documentation::docs_check,
    );
    phases.run(
        "dependencies",
        "cargo xtask verify-deps",
        crate::deps::verify_deps,
    );
    phases.run("workflow", "cargo xtask workflow-audit", || {
        crate::workflow_audit::workflow_audit().map(drop)
    });
    phases.run("command-surface", "cargo xtask command-audit", || {
        crate::command_audit::command_audit().map(drop)
    });
}

fn run_lint() -> anyhow::Result<()> {
    let registry = registry::read_registry()?;
    let timeout = lane_timeout(&registry, "lint")?;
    let diagnostics_path = verification_report_path("clippy-diagnostics")?;
    let diagnostics_file = fs::File::create(&diagnostics_path)
        .with_context(|| format!("failed to create {}", diagnostics_path.display()))?;
    let mut command = cargo_command();
    command.args([
        "clippy",
        "--workspace",
        "--lib",
        "--bins",
        "--tests",
        "--frozen",
        "--keep-going",
        "--message-format=json",
    ]);
    command.stdout(Stdio::from(diagnostics_file));
    let command_result = run_command_with_timeout(&mut command, timeout);
    let encoded = fs::read_to_string(&diagnostics_path)
        .with_context(|| format!("failed to read {}", diagnostics_path.display()))?;
    let _ = fs::remove_file(&diagnostics_path);
    if let Err(error) = command_result {
        emit_rendered_compiler_diagnostics(&encoded);
        return Err(error).context("Clippy compilation or invocation failed");
    }

    let actual = parse_clippy_warnings(&encoded)?;
    let expected = registry
        .lint_exceptions
        .iter()
        .map(registry::LintException::key)
        .collect::<BTreeSet<_>>();
    compare_lint_warnings(&actual, &expected)?;
    println!(
        "lint policy: Clippy emitted exactly {} registered inherited warnings",
        actual.len()
    );
    for exception in &registry.lint_exceptions {
        println!(
            "lint exception matched: {} {}:{}; deletion_gate={}; reason={}",
            exception.code,
            exception.path,
            exception.primary_line,
            exception.deletion_gate(),
            exception.reason(),
        );
    }
    Ok(())
}

fn run_nextest_leaf(leaf: Leaf) -> anyhow::Result<()> {
    ensure_nextest()?;
    let registry = registry::read_registry()?;
    let timeout = lane_timeout(&registry, leaf.name())?;
    let filter = format!("group({})", leaf.name());
    let mut command = isolated_nextest_command();
    command.args([
        "nextest",
        "run",
        "--workspace",
        "--frozen",
        "--profile",
        "leaf",
        "--no-fail-fast",
        "--retries",
        "0",
        "--flaky-result",
        "fail",
        "--no-tests",
        "fail",
        "-E",
        &filter,
    ]);
    run_command_with_timeout(&mut command, timeout)
}

fn run_doctest() -> anyhow::Result<()> {
    let mut command = cargo_command();
    command.args(["test", "--workspace", "--doc", "--frozen", "--no-fail-fast"]);
    run_command_with_timeout(&mut command, DOCTEST_PROCESS_TIMEOUT)
}

fn verify_pr_rust() -> anyhow::Result<()> {
    ensure_nextest()?;
    let registry = registry::read_registry()?;
    let identity = RunIdentity::gather()?;
    let report_path = verification_report_path("verify-pr-rust")?;
    let discovery_path = report_path
        .parent()
        .context("verification report path has no parent")?
        .join("nextest-discovery.json");
    let mut phases = PhaseCollector::default();
    phases.record_identity(&identity);

    phases.run("lint", lint_command(), run_lint);

    let discovery_ok = phases.run("test-build-and-discovery", discovery_command(), || {
        if let Some(parent) = discovery_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let output = fs::File::create(&discovery_path)
            .with_context(|| format!("failed to create {}", discovery_path.display()))?;
        let mut command = isolated_nextest_command();
        command.args([
            "nextest",
            "list",
            "--workspace",
            "--frozen",
            "--profile",
            "pr",
            "--run-ignored",
            "all",
            "--message-format",
            "json",
        ]);
        command.stdout(Stdio::from(output));
        run_command_with_timeout(&mut command, DISCOVERY_TIMEOUT)
    });

    let mut discovered_counts = BTreeMap::new();
    let audit_ok = if discovery_ok {
        phases.run(
            "discovery-audit",
            "in-process exact discovery audit",
            || {
                discovered_counts = registry::audit_discovery(&discovery_path, &registry)?;
                for (lane, count) in &discovered_counts {
                    println!("verification discovery: lane={lane} cases={count}");
                }
                Ok(())
            },
        )
    } else {
        phases.block("discovery-audit", "test build/discovery failed");
        false
    };

    let union_ok = if audit_ok {
        phases.run("unit-contract-ui", pr_union_command(), || {
            let mut command = isolated_nextest_command();
            command.args([
                "nextest",
                "run",
                "--workspace",
                "--frozen",
                "--profile",
                "pr",
                "--no-fail-fast",
                "--retries",
                "0",
                "--flaky-result",
                "fail",
                "--no-tests",
                "fail",
                "-E",
                "group(unit) | group(contract) | group(ui)",
            ]);
            run_command_with_timeout(&mut command, PR_TEST_UNION_TIMEOUT)
        })
    } else {
        phases.block(
            "unit-contract-ui",
            "discovery did not prove an exact lane assignment",
        );
        false
    };
    for lane in ["unit", "contract", "ui"] {
        phases.record_lane_section(
            lane,
            discovered_counts.get(lane).copied().unwrap_or(0),
            audit_ok,
            union_ok,
        );
    }

    phases.run("doctest", doctest_command(), run_doctest);
    let report_result = phases.write_report(&report_path, "rust", &identity);
    phases.finish("rust").and(report_result)
}

fn lint_command() -> &'static str {
    "cargo clippy --workspace --lib --bins --tests --frozen --keep-going --message-format=json; require exact verification/registry.json warning set"
}

fn doctest_command() -> &'static str {
    "cargo test --workspace --doc --frozen --no-fail-fast"
}

fn discovery_command() -> &'static str {
    "NEXTEST_USER_CONFIG_FILE=none cargo nextest list --workspace --frozen --profile pr --run-ignored all --message-format json"
}

fn pr_union_command() -> &'static str {
    "NEXTEST_USER_CONFIG_FILE=none cargo nextest run --workspace --frozen --profile pr --no-fail-fast --retries 0 --flaky-result fail --no-tests fail -E 'group(unit) | group(contract) | group(ui)'"
}

fn nextest_leaf_command(leaf: Leaf) -> String {
    format!(
        "NEXTEST_USER_CONFIG_FILE=none cargo nextest run --workspace --frozen --profile leaf --no-fail-fast --retries 0 --flaky-result fail --no-tests fail -E 'group({})'",
        leaf.name()
    )
}

fn isolated_nextest_command() -> Command {
    let mut command = cargo_command();
    command.env("NEXTEST_USER_CONFIG_FILE", "none");
    command
}

fn parse_clippy_warnings(encoded: &str) -> anyhow::Result<BTreeSet<registry::LintWarningKey>> {
    let mut warnings = BTreeSet::new();
    for (index, line) in encoded.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: Value = serde_json::from_str(line)
            .with_context(|| format!("Clippy JSON line {} was invalid", index + 1))?;
        if event.get("reason").and_then(Value::as_str) != Some("compiler-message") {
            continue;
        }
        let message = event
            .get("message")
            .and_then(Value::as_object)
            .context("Clippy compiler-message event was missing message")?;
        if message.get("level").and_then(Value::as_str) != Some("warning") {
            continue;
        }
        let code = message
            .get("code")
            .and_then(Value::as_object)
            .and_then(|code| code.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("uncoded")
            .to_owned();
        let primary_spans = message
            .get("spans")
            .and_then(Value::as_array)
            .context("Clippy warning was missing spans")?
            .iter()
            .filter(|span| span.get("is_primary").and_then(Value::as_bool) == Some(true))
            .collect::<Vec<_>>();
        if primary_spans.is_empty() {
            bail!("Clippy warning {code:?} had no primary source span");
        }
        for span in primary_spans {
            let raw_path = span
                .get("file_name")
                .and_then(Value::as_str)
                .context("Clippy primary warning span was missing file_name")?;
            let primary_line = span
                .get("line_start")
                .and_then(Value::as_u64)
                .filter(|line| *line > 0)
                .context("Clippy primary warning span had no positive line_start")?;
            warnings.insert(registry::LintWarningKey {
                code: code.clone(),
                path: normalize_clippy_repo_path(raw_path)?,
                primary_line,
            });
        }
    }
    Ok(warnings)
}

fn normalize_clippy_repo_path(raw: &str) -> anyhow::Result<String> {
    let path = Path::new(raw);
    let relative = if path.is_absolute() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .context("failed to canonicalize repository root")?;
        let absolute = path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize Clippy path {raw:?}"))?;
        absolute
            .strip_prefix(&root)
            .with_context(|| format!("Clippy warning path is outside the repository: {raw:?}"))?
            .to_owned()
    } else {
        path.to_owned()
    };
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(
                part.to_str()
                    .with_context(|| format!("Clippy warning path was not UTF-8: {raw:?}"))?,
            ),
            std::path::Component::CurDir => {}
            _ => bail!("Clippy warning path was not normalized: {raw:?}"),
        }
    }
    if parts.is_empty() {
        bail!("Clippy warning path was empty: {raw:?}");
    }
    let normalized = parts.join("/");
    if !registry::repo_path(&normalized).is_file() {
        bail!("Clippy warning path is not a repository file: {normalized:?}");
    }
    Ok(normalized)
}

fn compare_lint_warnings(
    actual: &BTreeSet<registry::LintWarningKey>,
    expected: &BTreeSet<registry::LintWarningKey>,
) -> anyhow::Result<()> {
    let unknown = actual.difference(expected).cloned().collect::<Vec<_>>();
    let missing = expected.difference(actual).cloned().collect::<Vec<_>>();
    if unknown.is_empty() && missing.is_empty() {
        Ok(())
    } else {
        bail!(
            "Clippy warning set did not exactly match the inherited registry: unknown={unknown:?}; registered_but_missing={missing:?}"
        )
    }
}

fn emit_rendered_compiler_diagnostics(encoded: &str) {
    for event in encoded
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    {
        if event.get("reason").and_then(Value::as_str) == Some("compiler-message")
            && let Some(rendered) = event
                .get("message")
                .and_then(|message| message.get("rendered"))
                .and_then(Value::as_str)
        {
            eprint!("{rendered}");
        }
    }
}

fn lane_timeout(registry: &registry::Registry, lane_id: &str) -> anyhow::Result<Duration> {
    registry
        .lanes
        .iter()
        .find(|lane| lane.id == lane_id)
        .map(|lane| Duration::from_secs(lane.aggregate_timeout_secs))
        .with_context(|| format!("verification registry is missing lane {lane_id:?}"))
}

fn verification_report_path(command: &str) -> anyhow::Result<PathBuf> {
    let run_id = std::env::var("GITHUB_RUN_ID").unwrap_or_else(|_| "local".to_owned());
    let attempt = std::env::var("GITHUB_RUN_ATTEMPT").unwrap_or_else(|_| "1".to_owned());
    let identity = format!("{run_id}-{attempt}-{}", std::process::id());
    let root = Path::new("target")
        .join("mirante4d")
        .join("verification")
        .join(identity);
    fs::create_dir_all(&root).with_context(|| format!("failed to create {}", root.display()))?;
    Ok(root.join(format!("{command}.json")))
}

fn outcome_text(result: &anyhow::Result<()>) -> String {
    match result {
        Ok(()) => "passed".to_owned(),
        Err(error) => format!("failed ({error:#})"),
    }
}

#[derive(Debug, Default, Serialize)]
struct PhaseCollector {
    phases: Vec<PhaseResult>,
}

impl PhaseCollector {
    fn record_identity(&mut self, identity: &RunIdentity) {
        if identity.enforced && !identity.qualifying {
            self.phases.push(PhaseResult {
                name: "identity",
                command: "git and executable identity validation".to_owned(),
                status: "failed",
                outcome_code: Some(1),
                duration_ms: 0.0,
                discovered_cases: None,
                reason: Some(identity.qualification_issues.join("; ")),
            });
        }
    }

    fn run(
        &mut self,
        name: &'static str,
        command: impl Into<String>,
        action: impl FnOnce() -> anyhow::Result<()>,
    ) -> bool {
        let command = command.into();
        let started = Instant::now();
        println!("verification phase {name}: starting: {command}");
        let result = action();
        let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
        let (status, reason) = match result {
            Ok(()) => ("passed", None),
            Err(error) => ("failed", Some(format!("{error:#}"))),
        };
        println!(
            "verification phase {name}: {status} after {:.3}s{}",
            duration_ms / 1000.0,
            reason
                .as_deref()
                .map(|reason| format!("; {reason}"))
                .unwrap_or_default()
        );
        self.phases.push(PhaseResult {
            name,
            command,
            status,
            outcome_code: Some(if status == "passed" { 0 } else { 1 }),
            duration_ms,
            discovered_cases: None,
            reason,
        });
        status == "passed"
    }

    fn block(&mut self, name: &'static str, reason: &'static str) {
        println!("verification phase {name}: blocked; {reason}");
        self.phases.push(PhaseResult {
            name,
            command: "not executed".to_owned(),
            status: "blocked",
            outcome_code: None,
            duration_ms: 0.0,
            discovered_cases: None,
            reason: Some(reason.to_owned()),
        });
    }

    fn record_lane_section(
        &mut self,
        name: &'static str,
        discovered_cases: u64,
        audit_ok: bool,
        union_ok: bool,
    ) {
        let (status, outcome_code, reason) = if audit_ok && union_ok {
            ("passed", Some(0), None)
        } else if !audit_ok {
            (
                "blocked",
                None,
                Some("discovery did not prove the lane assignment".to_owned()),
            )
        } else {
            (
                "blocked",
                None,
                Some(
                    "shared categorized union failed; lane-specific result is inconclusive"
                        .to_owned(),
                ),
            )
        };
        self.phases.push(PhaseResult {
            name,
            command: format!("result section for Nextest group({name})"),
            status,
            outcome_code,
            duration_ms: 0.0,
            discovered_cases: Some(discovered_cases),
            reason,
        });
    }

    fn finish(&self, group: &str) -> anyhow::Result<()> {
        let failures = self
            .phases
            .iter()
            .filter(|phase| phase.status != "passed")
            .map(|phase| phase.name)
            .collect::<Vec<_>>();
        if failures.is_empty() {
            Ok(())
        } else {
            bail!(
                "verification {group} failed or blocked phases: {}",
                failures.join(", ")
            )
        }
    }

    fn write_report(&self, path: &Path, group: &str, identity: &RunIdentity) -> anyhow::Result<()> {
        let report = VerificationReport {
            schema: "mirante4d-verification-run",
            schema_version: 1,
            group,
            native_status: if self.phases.iter().all(|phase| phase.status == "passed") {
                "passed"
            } else {
                "failed"
            },
            generated_at_epoch_ms: epoch_ms(),
            identity,
            phases: &self.phases,
        };
        let mut encoded = serde_json::to_vec_pretty(&report)?;
        encoded.push(b'\n');
        fs::write(path, &encoded).with_context(|| format!("failed to write {}", path.display()))?;
        println!("verification report: {}", path.display());
        println!(
            "verification-report-json: {}",
            String::from_utf8_lossy(&encoded).trim_end()
        );
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct PhaseResult {
    name: &'static str,
    command: String,
    status: &'static str,
    outcome_code: Option<i32>,
    duration_ms: f64,
    discovered_cases: Option<u64>,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct VerificationReport<'a> {
    schema: &'static str,
    schema_version: u32,
    group: &'a str,
    native_status: &'static str,
    generated_at_epoch_ms: u128,
    identity: &'a RunIdentity,
    phases: &'a [PhaseResult],
}

#[derive(Debug, Serialize)]
struct RunIdentity {
    commit: String,
    tree: String,
    clean: bool,
    status_sha256: String,
    executable: String,
    executable_sha256: String,
    rustc: String,
    cargo: String,
    nextest: Option<String>,
    fixtures: Vec<fixtures::FixtureIdentity>,
    runner: BTreeMap<String, String>,
    enforced: bool,
    qualifying: bool,
    qualification_issues: Vec<String>,
}

impl RunIdentity {
    fn gather() -> anyhow::Result<Self> {
        let commit = command_line("git", &["rev-parse", "HEAD"])?;
        let tree = command_line("git", &["show", "-s", "--format=%T", "HEAD"])?;
        let status = Command::new("git")
            .args(["status", "--porcelain=v1", "--untracked-files=all"])
            .output()
            .context("failed to inspect verification worktree identity")?;
        if !status.status.success() {
            bail!("git status failed while gathering verification identity");
        }
        let clean = status.stdout.is_empty();
        let status_sha256 = sha256_bytes(&status.stdout)?;
        let executable_path = env::current_exe().context("failed to locate xtask executable")?;
        let executable = executable_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("xtask")
            .to_owned();
        let executable_sha256 = registry::sha256_file(&executable_path)?;
        let rustc = command_line("rustc", &["--version"])?;
        let cargo = command_line("cargo", &["--version"])?;
        let nextest = command_line("cargo", &["nextest", "--version"]).ok();
        let runner = [
            "GITHUB_REPOSITORY",
            "GITHUB_RUN_ID",
            "GITHUB_RUN_ATTEMPT",
            "GITHUB_JOB",
            "GITHUB_WORKFLOW",
            "GITHUB_EVENT_NAME",
            "GITHUB_SHA",
            "GITHUB_REF",
            "RUNNER_OS",
            "RUNNER_ARCH",
            "RUNNER_NAME",
        ]
        .into_iter()
        .filter_map(|key| env::var(key).ok().map(|value| (key.to_owned(), value)))
        .collect::<BTreeMap<_, _>>();
        let enforced = env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
            || env::var("CI").as_deref() == Ok("true");
        let mut qualification_issues = Vec::new();
        if !clean {
            qualification_issues.push("worktree is dirty".to_owned());
        }
        if let Some(expected) = runner.get("GITHUB_SHA")
            && expected != &commit
        {
            qualification_issues.push(format!(
                "GITHUB_SHA does not match HEAD: expected {expected}, found {commit}"
            ));
        }
        let qualifying = qualification_issues.is_empty();
        Ok(Self {
            commit,
            tree,
            clean,
            status_sha256,
            executable,
            executable_sha256,
            rustc,
            cargo,
            nextest,
            fixtures: fixtures::fixture_identities()?,
            runner,
            enforced,
            qualifying,
            qualification_issues,
        })
    }
}

fn command_line(program: &str, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {program} {}", args.join(" ")))?;
    if !output.status.success() {
        bail!("{program} {} failed", args.join(" "));
    }
    String::from_utf8(output.stdout)
        .context("tool identity output was not UTF-8")
        .map(|text| text.trim().to_owned())
}

fn sha256_bytes(bytes: &[u8]) -> anyhow::Result<String> {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to start sha256sum for identity")?;
    child
        .stdin
        .take()
        .context("sha256sum stdin unavailable")?
        .write_all(bytes)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!("sha256sum failed for identity bytes");
    }
    String::from_utf8(output.stdout)?
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .context("sha256sum returned no identity digest")
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

    #[test]
    fn leaf_parser_accepts_exact_surface() {
        for name in ["policy", "lint", "unit", "contract", "ui", "doctest"] {
            assert_eq!(Leaf::parse(name).unwrap().name(), name);
        }
        assert!(Leaf::parse("fast").is_err());
    }

    #[test]
    fn phase_collector_retains_failures_before_returning() {
        let mut phases = PhaseCollector::default();
        assert!(!phases.run("first", "test command one", || bail!("expected")));
        assert!(phases.run("second", "test command two", || Ok(())));
        assert_eq!(phases.phases.len(), 2);
        assert!(phases.finish("test").is_err());
    }

    #[test]
    fn clippy_json_parser_normalizes_and_deduplicates_warnings() {
        let event = json!({
            "reason": "compiler-message",
            "message": {
                "level": "warning",
                "code": { "code": "clippy::example" },
                "spans": [{
                    "file_name": "./crates/xtask/src/verification/runner.rs",
                    "line_start": 1,
                    "is_primary": true
                }]
            }
        });
        let encoded = format!("{event}\n{event}\n");

        let actual = parse_clippy_warnings(&encoded).unwrap();

        assert_eq!(
            actual,
            BTreeSet::from([registry::LintWarningKey {
                code: "clippy::example".to_owned(),
                path: "crates/xtask/src/verification/runner.rs".to_owned(),
                primary_line: 1,
            }])
        );
    }

    #[test]
    fn lint_warning_comparison_requires_an_exact_registered_set() {
        let warning = registry::LintWarningKey {
            code: "clippy::example".to_owned(),
            path: "crates/xtask/src/verification/runner.rs".to_owned(),
            primary_line: 1,
        };
        let expected = BTreeSet::from([warning.clone()]);
        assert!(compare_lint_warnings(&expected, &expected).is_ok());
        assert!(
            compare_lint_warnings(&BTreeSet::new(), &expected)
                .unwrap_err()
                .to_string()
                .contains("registered_but_missing")
        );
        assert!(
            compare_lint_warnings(&expected, &BTreeSet::new())
                .unwrap_err()
                .to_string()
                .contains("unknown")
        );
    }

    #[test]
    fn nextest_commands_disable_user_configuration() {
        let command = isolated_nextest_command();
        let configured = command
            .get_envs()
            .find(|(key, _)| *key == "NEXTEST_USER_CONFIG_FILE")
            .and_then(|(_, value)| value)
            .and_then(|value| value.to_str());
        assert_eq!(configured, Some("none"));
    }
}

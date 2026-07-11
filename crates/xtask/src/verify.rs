use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use serde_json::Value;

use crate::process::{
    cargo_command, ensure_cargo_subcommand, ensure_nextest, run_cargo, run_command_with_timeout,
};

const COVERAGE_OUTPUT_DIR: &str = "target/mirante4d/coverage";
const COVERAGE_HTML_DIR: &str = "target/mirante4d/coverage/html";
const COVERAGE_SUMMARY_JSON: &str = "target/mirante4d/coverage/summary.json";
const COVERAGE_LCOV: &str = "target/mirante4d/coverage/lcov.info";
const VERIFY_BOOTSTRAP_TARGET_DIR: &str = "target/mirante4d/verify-bootstrap";
const VERIFY_BOOTSTRAP_FORMAT_TIMEOUT: Duration = Duration::from_secs(60);
const VERIFY_BOOTSTRAP_CHECK_TIMEOUT: Duration = Duration::from_secs(480);
const VERIFY_BOOTSTRAP_TEST_TIMEOUT: Duration = Duration::from_secs(240);
const VERIFY_BOOTSTRAP_EXPECTED_TESTS: u64 = 169;
const VERIFY_BOOTSTRAP_RUSTC_VERSION: &str = "rustc 1.96.1 (31fca3adb 2026-06-26)";
const VERIFY_BOOTSTRAP_CARGO_VERSION: &str = "cargo 1.96.1 (356927216 2026-06-26)";
const VERIFY_BOOTSTRAP_NEXTEST_VERSION: &str = "cargo-nextest 0.9.138 (fc97e97bb 2026-06-21)";
const VERIFY_BOOTSTRAP_RUMDL_VERSION: &str = "rumdl 0.2.30";
const VERIFY_BOOTSTRAP_FILTER: &str = concat!(
    "package(mirante4d-core) | package(mirante4d-format) | ",
    "package(mirante4d-data) | package(mirante4d-import) | ",
    "(package(mirante4d-analysis) & (",
    "test(=operations::tests::operation_record_validates_and_serializes_exact_roi_scope) | ",
    "test(=results::tests::roi_box_intensity_measurement_uses_source_volume_values) | ",
    "test(=scene_artifacts::tests::scene_artifact_store_applies_undoes_and_redoes_commands))) | ",
    "(package(mirante4d-renderer) & (",
    "test(=brick_render::tests::complete_resident_bricks_match_dense_camera_mip) | ",
    "test(=cross_section::tests::oblique_slab_culls_to_local_brick_subset) | ",
    "test(=scene::tests::extraction_filters_hidden_layers_objects_and_timepoints))) | ",
    "(package(mirante4d-app) & (",
    "test(=tests::app_shell_opens_fixture_and_renders_first_frame) | ",
    "test(=tests::project_package_roundtrip_restores_layer_display_states) | ",
    "test(=tests::workbench_commands_update_core_viewer_state))) | ",
    "(package(xtask) & (",
    "test(=command_audit::tests::command_audit_covers_current_xtask_command_surface) | ",
    "test(=verify::tests::bootstrap_scope_is_explicit_and_bounded)))",
);
pub(crate) fn verify_bootstrap() -> anyhow::Result<()> {
    ensure_bootstrap_tool_versions()?;

    let cache_state = if Path::new(VERIFY_BOOTSTRAP_TARGET_DIR).exists() {
        "warm"
    } else {
        "cold"
    };
    println!(
        "verify-bootstrap: partial WP-01 feedback only; cache={cache_state}; target={VERIFY_BOOTSTRAP_TARGET_DIR}"
    );
    println!(
        "verify-bootstrap excludes the complete suite, Clippy, doctests, dependencies, GPU, UI snapshots, E2E, packaging, performance, real data, and product-open validation"
    );

    run_bootstrap_cargo(
        &["fmt", "--all", "--check"],
        VERIFY_BOOTSTRAP_FORMAT_TIMEOUT,
    )?;
    run_bootstrap_cargo(
        &["check", "--workspace", "--all-targets", "--frozen"],
        VERIFY_BOOTSTRAP_CHECK_TIMEOUT,
    )?;
    run_bootstrap_tests()?;

    crate::documentation::docs_check()?;

    println!(
        "verify-bootstrap passed its declared partial scope; deeper verification remains required"
    );
    Ok(())
}

fn run_bootstrap_cargo(args: &[&str], timeout: Duration) -> anyhow::Result<()> {
    let mut command = bootstrap_cargo_command(args);
    run_command_with_timeout(&mut command, timeout)
}

fn bootstrap_cargo_command(args: &[&str]) -> Command {
    let mut command = cargo_command();
    command
        .args(args)
        .env("CARGO_TARGET_DIR", VERIFY_BOOTSTRAP_TARGET_DIR)
        .env("CARGO_INCREMENTAL", "0");
    if args.first() == Some(&"nextest") {
        command.env("NEXTEST_USER_CONFIG_FILE", "none");
    }
    command
}

fn run_bootstrap_tests() -> anyhow::Result<()> {
    let started = Instant::now();
    let list_path = PathBuf::from(VERIFY_BOOTSTRAP_TARGET_DIR).join("selected-tests.json");
    if let Some(parent) = list_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let result = (|| {
        let list_output = fs::File::create(&list_path)
            .with_context(|| format!("failed to create {}", list_path.display()))?;
        let mut list = bootstrap_cargo_command(&[
            "nextest",
            "list",
            "--workspace",
            "--frozen",
            "--profile",
            "bootstrap",
            "-E",
            VERIFY_BOOTSTRAP_FILTER,
            "--message-format",
            "json",
        ]);
        list.stdout(Stdio::from(list_output));
        run_command_with_timeout(&mut list, VERIFY_BOOTSTRAP_TEST_TIMEOUT)?;

        let discovery: Value = serde_json::from_slice(
            &fs::read(&list_path)
                .with_context(|| format!("failed to read {}", list_path.display()))?,
        )
        .context("failed to parse verify-bootstrap Nextest discovery")?;
        let selected = discovery
            .get("rust-suites")
            .and_then(Value::as_object)
            .context("verify-bootstrap Nextest discovery is missing rust-suites")?
            .values()
            .map(|suite| {
                suite
                    .get("testcases")
                    .and_then(Value::as_object)
                    .context("verify-bootstrap Nextest suite is missing testcases")
                    .map(|testcases| {
                        testcases
                            .values()
                            .filter(|testcase| {
                                testcase
                                    .pointer("/filter-match/status")
                                    .and_then(Value::as_str)
                                    == Some("matches")
                            })
                            .count() as u64
                    })
            })
            .collect::<anyhow::Result<Vec<_>>>()?
            .into_iter()
            .sum::<u64>();
        if selected != VERIFY_BOOTSTRAP_EXPECTED_TESTS {
            bail!(
                "verify-bootstrap selector drift: expected {VERIFY_BOOTSTRAP_EXPECTED_TESTS} tests, found {selected}"
            );
        }
        println!("verify-bootstrap selector resolved to {selected} tests");

        let remaining = VERIFY_BOOTSTRAP_TEST_TIMEOUT
            .checked_sub(started.elapsed())
            .context("verify-bootstrap test discovery exhausted the 240 second phase timeout")?;
        let mut run = bootstrap_cargo_command(&[
            "nextest",
            "run",
            "--workspace",
            "--frozen",
            "--profile",
            "bootstrap",
            "--no-fail-fast",
            "-E",
            VERIFY_BOOTSTRAP_FILTER,
        ]);
        run_command_with_timeout(&mut run, remaining)
    })();
    let _ = fs::remove_file(&list_path);
    result
}

fn ensure_bootstrap_tool_versions() -> anyhow::Result<()> {
    let mut rustc = Command::new(env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()));
    rustc.arg("--version");
    require_exact_version(
        &mut rustc,
        "Rust",
        VERIFY_BOOTSTRAP_RUSTC_VERSION,
        "use the repository rust-toolchain.toml without an overriding RUSTUP_TOOLCHAIN",
    )?;

    let mut cargo = cargo_command();
    cargo.arg("--version");
    require_exact_version(
        &mut cargo,
        "Cargo",
        VERIFY_BOOTSTRAP_CARGO_VERSION,
        "use the repository rust-toolchain.toml without an overriding RUSTUP_TOOLCHAIN",
    )?;

    let mut nextest = cargo_command();
    nextest.args(["nextest", "--version"]);
    require_exact_version(
        &mut nextest,
        "cargo-nextest",
        VERIFY_BOOTSTRAP_NEXTEST_VERSION,
        "install cargo-nextest 0.9.138 with `cargo install cargo-nextest --version 0.9.138 --locked`",
    )?;

    let mut rumdl = rumdl_command();
    rumdl.arg("--version");
    require_exact_version(
        &mut rumdl,
        "rumdl",
        VERIFY_BOOTSTRAP_RUMDL_VERSION,
        "install rumdl 0.2.30 or set MIRANTE4D_RUMDL to its executable",
    )?;
    Ok(())
}

fn require_exact_version(
    command: &mut Command,
    tool: &str,
    expected: &str,
    remediation: &str,
) -> anyhow::Result<()> {
    let output = match command.output() {
        Ok(output) => output,
        Err(error) => {
            bail!("{tool} {expected:?} is required; {remediation}: {error}")
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let actual = stdout.lines().next().unwrap_or_default().trim();
    if !output.status.success() || actual != expected {
        bail!("{tool} version mismatch: expected {expected:?}, found {actual:?}; {remediation}")
    }
    println!("verify-bootstrap tool: {actual}");
    Ok(())
}

fn rumdl_command() -> Command {
    Command::new(env::var_os("MIRANTE4D_RUMDL").unwrap_or_else(|| "rumdl".into()))
}

pub(crate) fn verify_coverage() -> anyhow::Result<()> {
    ensure_nextest()?;
    ensure_cargo_subcommand(
        "llvm-cov",
        "cargo-llvm-cov",
        "cargo install cargo-llvm-cov --locked",
    )?;
    fs::create_dir_all(COVERAGE_OUTPUT_DIR)
        .with_context(|| format!("failed to create {COVERAGE_OUTPUT_DIR}"))?;
    run_cargo(["llvm-cov", "clean", "--workspace"])?;
    run_cargo([
        "llvm-cov",
        "nextest",
        "--workspace",
        "--html",
        "--output-dir",
        COVERAGE_HTML_DIR,
    ])?;
    run_cargo([
        "llvm-cov",
        "report",
        "--json",
        "--summary-only",
        "--output-path",
        COVERAGE_SUMMARY_JSON,
    ])?;
    run_cargo([
        "llvm-cov",
        "report",
        "--lcov",
        "--output-path",
        COVERAGE_LCOV,
    ])
}

pub(crate) fn verify_deps() -> anyhow::Result<()> {
    crate::deps::verify_deps()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_scope_is_explicit_and_bounded() {
        for package in [
            "mirante4d-core",
            "mirante4d-format",
            "mirante4d-data",
            "mirante4d-import",
            "mirante4d-analysis",
            "mirante4d-renderer",
            "mirante4d-app",
            "xtask",
        ] {
            assert!(VERIFY_BOOTSTRAP_FILTER.contains(package));
        }
        assert!(!VERIFY_BOOTSTRAP_FILTER.contains("all()"));
        assert_eq!(VERIFY_BOOTSTRAP_EXPECTED_TESTS, 169);

        let total_ceiling = VERIFY_BOOTSTRAP_FORMAT_TIMEOUT
            + VERIFY_BOOTSTRAP_CHECK_TIMEOUT
            + VERIFY_BOOTSTRAP_TEST_TIMEOUT
            + crate::documentation::DOCS_CHECK_TIMEOUT;
        assert_eq!(total_ceiling, Duration::from_secs(14 * 60 + 30));
    }
}

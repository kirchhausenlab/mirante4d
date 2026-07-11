use std::fs;

use anyhow::Context;

use crate::process::{ensure_cargo_subcommand, ensure_nextest, run_cargo};

const COVERAGE_OUTPUT_DIR: &str = "target/mirante4d/coverage";
const COVERAGE_HTML_DIR: &str = "target/mirante4d/coverage/html";
const COVERAGE_SUMMARY_JSON: &str = "target/mirante4d/coverage/summary.json";
const COVERAGE_LCOV: &str = "target/mirante4d/coverage/lcov.info";
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

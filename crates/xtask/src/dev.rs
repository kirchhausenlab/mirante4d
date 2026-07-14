use std::{env, fs, path::PathBuf};

use anyhow::Context;

use crate::process::{cargo_command, run_command};
use crate::target_fixture::extract_target_u16_fixture;

pub(crate) fn run_dev() -> anyhow::Result<()> {
    let dataset = extract_target_u16_fixture(&PathBuf::from("target/mirante4d/fixtures"))?;
    let log_dir = PathBuf::from("target/mirante4d/logs");
    fs::create_dir_all(&log_dir)
        .with_context(|| format!("failed to create {}", log_dir.display()))?;

    let mut command = cargo_command();
    command
        .args(["run", "-p", "mirante4d-app"])
        .env("MIRANTE4D_DEV_DATASET", &dataset)
        .env(
            "RUST_LOG",
            env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned()),
        );
    run_command(&mut command)
}

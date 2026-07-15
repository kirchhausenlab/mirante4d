use std::{
    env,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

pub(crate) fn ensure_nextest() -> anyhow::Result<()> {
    ensure_cargo_subcommand(
        "nextest",
        "cargo-nextest",
        "cargo install cargo-nextest --locked",
    )
}

pub(crate) fn ensure_cargo_subcommand(
    subcommand: &str,
    tool_name: &str,
    install_command: &str,
) -> anyhow::Result<()> {
    let status = cargo_command()
        .args([subcommand, "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to check {tool_name}"))?;
    if status.success() {
        Ok(())
    } else {
        bail!(
            "{}",
            missing_cargo_subcommand_message(tool_name, install_command)
        )
    }
}

pub(crate) fn run_cargo<const N: usize>(args: [&str; N]) -> anyhow::Result<()> {
    let mut command = cargo_command();
    command.args(args);
    run_command(&mut command)
}

pub(crate) fn cargo_command() -> Command {
    Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

pub(crate) fn run_command(command: &mut Command) -> anyhow::Result<()> {
    println!("running: {:?}", command);
    let status = command.status().context("failed to spawn command")?;
    if status.success() {
        Ok(())
    } else {
        bail!("command failed with status {status}: {:?}", command)
    }
}

pub(crate) fn run_command_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> anyhow::Result<()> {
    #[cfg(unix)]
    command.process_group(0);

    println!("running with timeout {timeout:?}: {:?}", command);
    let started = Instant::now();
    let mut child = command.spawn().context("failed to spawn command")?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().context("failed to poll command status")? {
            println!(
                "command finished after {:.3}s with {status}: {:?}",
                started.elapsed().as_secs_f64(),
                command
            );
            if status.success() {
                return Ok(());
            }
            bail!("command failed with status {status}: {:?}", command);
        }
        if Instant::now() >= deadline {
            terminate_process_tree(&mut child);
            let _ = child.wait();
            bail!("command timed out after {timeout:?}: {:?}", command);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(unix)]
fn terminate_process_tree(child: &mut std::process::Child) {
    const SIGKILL: i32 = 9;
    let process_group = -(child.id() as i32);
    // SAFETY: the command was placed in its own process group immediately
    // before spawning, so this signal is scoped to that command tree.
    unsafe {
        kill(process_group, SIGKILL);
    }
}

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

#[cfg(not(unix))]
fn terminate_process_tree(child: &mut std::process::Child) {
    let _ = child.kill();
}

fn missing_cargo_subcommand_message(tool_name: &str, install_command: &str) -> String {
    format!("{tool_name} is required; install it with `{install_command}`")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_cargo_subcommand_message_is_actionable() {
        let message = missing_cargo_subcommand_message(
            "cargo-llvm-cov",
            "cargo install cargo-llvm-cov --locked",
        );

        assert!(message.contains("cargo-llvm-cov is required"));
        assert!(message.contains("cargo install cargo-llvm-cov --locked"));
    }
}

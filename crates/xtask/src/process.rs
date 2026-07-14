use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const HEAVY_BENCHMARK_LOCK_ENV: &str = "MIRANTE4D_XTASK_HEAVY_BENCHMARK_LOCK";
const HEAVY_BENCHMARK_OPT_IN_ENV: &str = "MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK";

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

pub(crate) fn with_heavy_benchmark_guard<T>(
    command_name: &str,
    action: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    require_heavy_benchmark_opt_in(command_name)?;
    let _guard = HeavyBenchmarkGuard::acquire(command_name)?;
    action()
}

#[derive(Debug)]
struct HeavyBenchmarkGuard {
    path: PathBuf,
}

impl HeavyBenchmarkGuard {
    fn acquire(command_name: &str) -> anyhow::Result<Self> {
        Self::acquire_at(default_heavy_benchmark_lock_path(), command_name)
    }

    fn acquire_at(path: PathBuf, command_name: &str) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if remove_stale_heavy_benchmark_lock(&path)? {
                    return Self::acquire_at(path, command_name);
                }
                let holder = fs::read_to_string(&path).unwrap_or_else(|read_err| {
                    format!("<failed to read lock contents: {read_err}>")
                });
                bail!(
                    "another heavyweight xtask benchmark/evidence command is already running; \
                     lock={} holder={}",
                    path.display(),
                    holder.trim()
                );
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to create lock {}", path.display()));
            }
        };
        writeln!(file, "command={command_name}")?;
        writeln!(file, "pid={}", std::process::id())?;
        Ok(Self { path })
    }
}

impl Drop for HeavyBenchmarkGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn default_heavy_benchmark_lock_path() -> PathBuf {
    env::var_os(HEAVY_BENCHMARK_LOCK_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new("target")
                .join("mirante4d")
                .join("xtask-heavy-benchmark.lock")
        })
}

fn require_heavy_benchmark_opt_in(command_name: &str) -> anyhow::Result<()> {
    let raw = env::var(HEAVY_BENCHMARK_OPT_IN_ENV).ok();
    require_heavy_benchmark_opt_in_value(command_name, raw.as_deref())
}

fn require_heavy_benchmark_opt_in_value(
    command_name: &str,
    raw_value: Option<&str>,
) -> anyhow::Result<()> {
    if raw_value
        .map(heavy_benchmark_opt_in_enabled)
        .unwrap_or(false)
    {
        return Ok(());
    }
    bail!(
        "{command_name} is a heavyweight local benchmark/evidence command and is disabled by default; \
         set {HEAVY_BENCHMARK_OPT_IN_ENV}=1 to run it intentionally"
    )
}

fn heavy_benchmark_opt_in_enabled(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes"
    )
}

fn remove_stale_heavy_benchmark_lock(path: &Path) -> anyhow::Result<bool> {
    let contents = fs::read_to_string(path).unwrap_or_default();
    let Some(pid) = heavy_benchmark_lock_pid(&contents) else {
        return Ok(false);
    };
    if process_id_is_running(pid) {
        return Ok(false);
    }
    fs::remove_file(path)
        .with_context(|| format!("failed to remove stale lock {}", path.display()))?;
    Ok(true)
}

fn heavy_benchmark_lock_pid(contents: &str) -> Option<u32> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix("pid=")?.trim().parse::<u32>().ok())
}

#[cfg(target_os = "linux")]
fn process_id_is_running(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

#[cfg(not(target_os = "linux"))]
fn process_id_is_running(_pid: u32) -> bool {
    true
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

    #[test]
    fn heavy_benchmark_opt_in_accepts_explicit_truthy_values() {
        assert!(heavy_benchmark_opt_in_enabled("1"));
        assert!(heavy_benchmark_opt_in_enabled("true"));
        assert!(heavy_benchmark_opt_in_enabled("YES"));
        assert!(!heavy_benchmark_opt_in_enabled(""));
        assert!(!heavy_benchmark_opt_in_enabled("0"));
        assert!(!heavy_benchmark_opt_in_enabled("false"));
    }

    #[test]
    fn heavy_benchmark_opt_in_rejects_unset_command_before_locking() {
        let error = require_heavy_benchmark_opt_in_value("t5-product-validation", None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("t5-product-validation is a heavyweight local benchmark"));
        assert!(error.contains(HEAVY_BENCHMARK_OPT_IN_ENV));
        assert!(error.contains("disabled by default"));
    }

    #[test]
    fn heavy_benchmark_opt_in_allows_enabled_command() {
        require_heavy_benchmark_opt_in_value("t5-product-validation", Some("1")).unwrap();
    }

    #[test]
    fn heavy_benchmark_guard_rejects_second_holder_and_cleans_up() {
        let tempdir = tempfile::tempdir().unwrap();
        let lock_path = tempdir.path().join("heavy.lock");
        {
            let _guard =
                HeavyBenchmarkGuard::acquire_at(lock_path.clone(), "t5-product-validation")
                    .unwrap();

            let err =
                HeavyBenchmarkGuard::acquire_at(lock_path.clone(), "other-product-validation")
                    .unwrap_err()
                    .to_string();

            assert!(err.contains("another heavyweight xtask benchmark/evidence command"));
            assert!(err.contains("t5-product-validation"));
            assert!(lock_path.exists());
        }

        assert!(!lock_path.exists());
        HeavyBenchmarkGuard::acquire_at(lock_path.clone(), "t5-product-validation").unwrap();
    }

    #[test]
    fn heavy_benchmark_lock_pid_parses_holder_pid() {
        assert_eq!(
            heavy_benchmark_lock_pid("command=t5-product-validation\npid=12345\n"),
            Some(12345)
        );
        assert_eq!(heavy_benchmark_lock_pid("command=missing-pid\n"), None);
        assert_eq!(heavy_benchmark_lock_pid("pid=not-a-number\n"), None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn heavy_benchmark_guard_removes_stale_dead_pid_lock() {
        let tempdir = tempfile::tempdir().unwrap();
        let lock_path = tempdir.path().join("heavy.lock");
        fs::write(
            &lock_path,
            "command=t5-product-validation\npid=4294967295\n",
        )
        .unwrap();

        let _guard = HeavyBenchmarkGuard::acquire_at(lock_path.clone(), "t5-product-validation")
            .expect("dead pid lock should be treated as stale");

        let holder = fs::read_to_string(&lock_path).unwrap();
        assert!(holder.contains("command=t5-product-validation"));
        assert!(holder.contains(&format!("pid={}", std::process::id())));
    }
}

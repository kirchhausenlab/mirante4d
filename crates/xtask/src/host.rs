use std::{env, fs, process::Command};

use serde_json::{Value, json};

pub(crate) fn benchmark_host_context() -> Value {
    json!({
        "name": env::var("MIRANTE4D_BENCH_HARDWARE_NAME")
            .unwrap_or_else(|_| "local-dev-machine".to_owned()),
        "build_profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "git_commit": git_commit_hash(),
        "dirty_worktree": git_dirty_worktree(),
        "os": env::consts::OS,
        "arch": env::consts::ARCH,
        "cpu_model": linux_cpu_model(),
        "mem_total_kib": linux_mem_total_kib(),
    })
}

fn git_commit_hash() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!hash.is_empty()).then_some(hash)
}

fn git_dirty_worktree() -> Option<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()?;
    output.status.success().then_some(!output.stdout.is_empty())
}

fn linux_cpu_model() -> Option<String> {
    fs::read_to_string("/proc/cpuinfo")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("model name")
                .or_else(|| line.strip_prefix("Hardware"))
        })
        .and_then(|line| {
            line.split_once(':')
                .map(|(_, value)| value.trim().to_owned())
        })
}

fn linux_mem_total_kib() -> Option<u64> {
    fs::read_to_string("/proc/meminfo")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|line| line.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
}

use std::{
    env,
    io::{self, Read},
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, bail};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(50);
const CAPTURE_CLOSEOUT_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BoundedOutputPolicy {
    pub(crate) scope: &'static str,
    pub(crate) inactivity_timeout: Duration,
    pub(crate) absolute_timeout: Duration,
    pub(crate) progress_interval: Duration,
    pub(crate) max_stdout_bytes: usize,
    pub(crate) max_stderr_bytes: usize,
}

impl BoundedOutputPolicy {
    fn validate(self) -> anyhow::Result<Self> {
        if self.scope.is_empty()
            || self.scope.len() > 64
            || self.scope.bytes().any(|byte| {
                !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-'))
            })
        {
            bail!("bounded process scope must be a safe lowercase ASCII token");
        }
        if self.inactivity_timeout.is_zero()
            || self.absolute_timeout.is_zero()
            || self.progress_interval.is_zero()
            || self.inactivity_timeout > self.absolute_timeout
        {
            bail!(
                "bounded process timeouts must be positive and inactivity must not exceed the absolute timeout"
            );
        }
        if self.max_stdout_bytes == 0 || self.max_stderr_bytes == 0 {
            bail!("bounded process output limits must be positive");
        }
        Ok(self)
    }
}

#[derive(Clone, Copy)]
enum CaptureStream {
    Stdout,
    Stderr,
}

struct CaptureResult {
    stream: CaptureStream,
    bytes: Vec<u8>,
    overflowed: bool,
    read_error: Option<io::Error>,
}

struct CaptureThreads {
    receiver: Receiver<CaptureResult>,
    stdout_thread: JoinHandle<()>,
    stderr_thread: JoinHandle<()>,
    activity_sequence: Arc<AtomicU64>,
    stdout_bytes: Arc<AtomicU64>,
    stderr_bytes: Arc<AtomicU64>,
    capture_failed: Arc<AtomicBool>,
}

#[derive(Default)]
struct CapturedOutput {
    stdout: Option<Vec<u8>>,
    stderr: Option<Vec<u8>>,
    overflowed: bool,
    read_error: Option<io::Error>,
}

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
    isolate_process_tree(command);

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

pub(crate) fn run_command_with_bounded_output(
    command: &mut Command,
    policy: BoundedOutputPolicy,
) -> anyhow::Result<Output> {
    let policy = policy.validate()?;
    isolate_process_tree(command);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let started = Instant::now();
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn bounded process {}", policy.scope))?;
    let Some(stdout) = child.stdout.take() else {
        terminate_and_reap(&mut child);
        bail!("bounded process stdout pipe is unavailable");
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_and_reap(&mut child);
        bail!("bounded process stderr pipe is unavailable");
    };
    let captures = spawn_capture_threads(
        stdout,
        stderr,
        policy.max_stdout_bytes,
        policy.max_stderr_bytes,
    );
    let absolute_deadline = started + policy.absolute_timeout;
    let mut last_activity = started;
    let mut observed_activity_sequence = 0;
    let mut next_progress = started + policy.progress_interval;
    let mut captured = CapturedOutput::default();

    let status = loop {
        drain_capture_results(&captures.receiver, &mut captured);
        let now = Instant::now();
        let activity_sequence = captures.activity_sequence.load(Ordering::Acquire);
        if activity_sequence != observed_activity_sequence {
            observed_activity_sequence = activity_sequence;
            last_activity = now;
        }
        if captures.capture_failed.load(Ordering::Acquire) || captured.read_error.is_some() {
            terminate_and_reap(&mut child);
            finish_capture_threads(captures, &mut captured);
            bail!(
                "bounded process {} exceeded an output bound or its output could not be read",
                policy.scope
            );
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                terminate_and_reap(&mut child);
                finish_capture_threads(captures, &mut captured);
                return Err(error).context("failed to poll bounded process");
            }
        }
        if now >= absolute_deadline {
            terminate_and_reap(&mut child);
            finish_capture_threads(captures, &mut captured);
            bail!(
                "bounded process {} exceeded its {} ms absolute timeout",
                policy.scope,
                policy.absolute_timeout.as_millis()
            );
        }
        if now.duration_since(last_activity) >= policy.inactivity_timeout {
            terminate_and_reap(&mut child);
            finish_capture_threads(captures, &mut captured);
            bail!(
                "bounded process {} exceeded its {} ms output-inactivity timeout",
                policy.scope,
                policy.inactivity_timeout.as_millis()
            );
        }
        if now >= next_progress {
            eprintln!(
                "process_progress scope={} state=running elapsed_ms={} idle_ms={} stdout_bytes={} stderr_bytes={}",
                policy.scope,
                now.duration_since(started).as_millis(),
                now.duration_since(last_activity).as_millis(),
                captures.stdout_bytes.load(Ordering::Acquire),
                captures.stderr_bytes.load(Ordering::Acquire),
            );
            next_progress = now + policy.progress_interval;
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    };

    finish_capture_threads_after_exit(&mut child, captures, &mut captured)?;
    Ok(Output {
        status,
        stdout: captured.stdout.unwrap_or_default(),
        stderr: captured.stderr.unwrap_or_default(),
    })
}

fn spawn_capture_threads(
    stdout: impl Read + Send + 'static,
    stderr: impl Read + Send + 'static,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> CaptureThreads {
    let (sender, receiver) = mpsc::channel();
    let activity_sequence = Arc::new(AtomicU64::new(0));
    let stdout_bytes = Arc::new(AtomicU64::new(0));
    let stderr_bytes = Arc::new(AtomicU64::new(0));
    let capture_failed = Arc::new(AtomicBool::new(false));
    let stdout_thread = spawn_capture_thread(
        stdout,
        CaptureStream::Stdout,
        max_stdout_bytes,
        Arc::clone(&activity_sequence),
        Arc::clone(&stdout_bytes),
        Arc::clone(&capture_failed),
        sender.clone(),
    );
    let stderr_thread = spawn_capture_thread(
        stderr,
        CaptureStream::Stderr,
        max_stderr_bytes,
        Arc::clone(&activity_sequence),
        Arc::clone(&stderr_bytes),
        Arc::clone(&capture_failed),
        sender,
    );
    CaptureThreads {
        receiver,
        stdout_thread,
        stderr_thread,
        activity_sequence,
        stdout_bytes,
        stderr_bytes,
        capture_failed,
    }
}

fn spawn_capture_thread(
    mut reader: impl Read + Send + 'static,
    stream: CaptureStream,
    max_bytes: usize,
    activity_sequence: Arc<AtomicU64>,
    observed_bytes: Arc<AtomicU64>,
    capture_failed: Arc<AtomicBool>,
    sender: mpsc::Sender<CaptureResult>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut total_bytes = 0_usize;
        let mut overflowed = false;
        let mut scratch = [0_u8; 8 * 1024];
        let mut read_error = None;
        loop {
            match reader.read(&mut scratch) {
                Ok(0) => break,
                Ok(count) => {
                    total_bytes = total_bytes.saturating_add(count);
                    observed_bytes.store(
                        u64::try_from(total_bytes).unwrap_or(u64::MAX),
                        Ordering::Release,
                    );
                    activity_sequence.fetch_add(1, Ordering::AcqRel);
                    if total_bytes > max_bytes {
                        overflowed = true;
                        capture_failed.store(true, Ordering::Release);
                    } else if !overflowed {
                        bytes.extend_from_slice(&scratch[..count]);
                    }
                }
                Err(error) => {
                    capture_failed.store(true, Ordering::Release);
                    read_error = Some(error);
                    break;
                }
            }
        }
        let _ = sender.send(CaptureResult {
            stream,
            bytes,
            overflowed,
            read_error,
        });
    })
}

fn drain_capture_results(receiver: &Receiver<CaptureResult>, captured: &mut CapturedOutput) {
    while let Ok(result) = receiver.try_recv() {
        captured.overflowed |= result.overflowed;
        if captured.read_error.is_none() {
            captured.read_error = result.read_error;
        }
        match result.stream {
            CaptureStream::Stdout => captured.stdout = Some(result.bytes),
            CaptureStream::Stderr => captured.stderr = Some(result.bytes),
        }
    }
}

fn finish_capture_threads(captures: CaptureThreads, captured: &mut CapturedOutput) {
    let _ = captures.stdout_thread.join();
    let _ = captures.stderr_thread.join();
    drain_capture_results(&captures.receiver, captured);
}

fn finish_capture_threads_after_exit(
    child: &mut std::process::Child,
    captures: CaptureThreads,
    captured: &mut CapturedOutput,
) -> anyhow::Result<()> {
    let closeout_deadline = Instant::now() + CAPTURE_CLOSEOUT_TIMEOUT;
    while (captured.stdout.is_none() || captured.stderr.is_none())
        && Instant::now() < closeout_deadline
    {
        drain_capture_results(&captures.receiver, captured);
        if captured.stdout.is_some() && captured.stderr.is_some() {
            break;
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
    if captured.stdout.is_none() || captured.stderr.is_none() {
        // The direct child has exited, so an open capture pipe can only be
        // retained by a descendant. End that original isolated process tree
        // before joining the readers.
        terminate_process_tree(child);
    }
    finish_capture_threads(captures, captured);
    if let Some(error) = captured.read_error.take() {
        return Err(error).context("failed to capture bounded process output");
    }
    if captured.overflowed {
        bail!("bounded process exceeded its captured-output byte limit");
    }
    if captured.stdout.is_none() || captured.stderr.is_none() {
        bail!("bounded process capture did not close both output streams");
    }
    Ok(())
}

fn terminate_and_reap(child: &mut std::process::Child) {
    terminate_process_tree(child);
    let _ = child.wait();
}

pub(crate) fn isolate_process_tree(command: &mut Command) {
    #[cfg(unix)]
    command.process_group(0);
}

#[cfg(unix)]
pub(crate) fn terminate_process_tree(child: &mut std::process::Child) {
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
pub(crate) fn terminate_process_tree(child: &mut std::process::Child) {
    let _ = child.kill();
}

fn missing_cargo_subcommand_message(tool_name: &str, install_command: &str) -> String {
    format!("{tool_name} is required; install it with `{install_command}`")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounded_policy() -> BoundedOutputPolicy {
        BoundedOutputPolicy {
            scope: "test_process",
            inactivity_timeout: Duration::from_secs(1),
            absolute_timeout: Duration::from_secs(2),
            progress_interval: Duration::from_millis(100),
            max_stdout_bytes: 1024,
            max_stderr_bytes: 1024,
        }
    }

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
    fn bounded_output_captures_both_streams_and_preserves_status() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf stdout; printf stderr >&2"]);
        let output = run_command_with_bounded_output(&mut command, bounded_policy()).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"stdout");
        assert_eq!(output.stderr, b"stderr");
    }

    #[test]
    fn bounded_output_rejects_invalid_policy_and_output_overflow() {
        let invalid = BoundedOutputPolicy {
            scope: "private/path",
            ..bounded_policy()
        };
        assert!(
            run_command_with_bounded_output(&mut Command::new("missing-command"), invalid).is_err()
        );

        let mut command = Command::new("sh");
        command.args(["-c", "printf 12345"]);
        let error = run_command_with_bounded_output(
            &mut command,
            BoundedOutputPolicy {
                max_stdout_bytes: 4,
                ..bounded_policy()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("output"));
    }

    #[test]
    fn bounded_output_kills_a_silent_process_tree_at_inactivity() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        let started = Instant::now();
        let error = run_command_with_bounded_output(
            &mut command,
            BoundedOutputPolicy {
                inactivity_timeout: Duration::from_millis(100),
                absolute_timeout: Duration::from_secs(1),
                progress_interval: Duration::from_millis(50),
                ..bounded_policy()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("output-inactivity timeout"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn bounded_output_absolute_timeout_wins_while_output_remains_active() {
        let mut command = Command::new("sh");
        command.args(["-c", "while :; do printf x; sleep 0.02; done"]);
        let error = run_command_with_bounded_output(
            &mut command,
            BoundedOutputPolicy {
                inactivity_timeout: Duration::from_millis(500),
                absolute_timeout: Duration::from_millis(150),
                progress_interval: Duration::from_millis(50),
                ..bounded_policy()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("absolute timeout"));
    }
}

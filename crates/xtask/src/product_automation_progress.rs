use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::viewer_performance::read_bounded_regular_file;

pub(crate) const PROGRESS_PATH_ENV: &str = "MIRANTE4D_AUTOMATION_PROGRESS_PATH";
pub(crate) const PROGRESS_NONCE_ENV: &str = "MIRANTE4D_AUTOMATION_PROGRESS_NONCE";
pub(crate) const FILE_POLL_INTERVAL: Duration = Duration::from_millis(100);

const PROGRESS_SCHEMA: &str = "mirante4d-product-automation-progress";
const PROGRESS_SCHEMA_VERSION: u32 = 1;
const PROGRESS_FILE_NAME: &str = "automation-progress.json";
const MAX_PROGRESS_BYTES: u64 = 4 * 1024;
const NONCE_BYTES: usize = 16;
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(30);
const HEARTBEAT_STALE_TIMEOUT: Duration = Duration::from_secs(5);
const COMMAND_GRACE: Duration = Duration::from_millis(500);
const CLOSEOUT_TIMEOUT: Duration = Duration::from_secs(10);
const SAFE_PROGRESS_INTERVAL: Duration = Duration::from_secs(5);
const DEFAULT_COMMAND_BUDGET: Duration = Duration::from_secs(30);
const SEQUENCE_BASE_BUDGET: Duration = Duration::from_secs(1);
const MAX_SEQUENCE_DURATION_MS: u64 = 120_000;
const MAX_SLEEP_FRAMES: u64 = 600;

#[derive(Clone)]
struct CommandPlan {
    kind: &'static str,
    budget: Option<Duration>,
}

#[derive(Clone)]
pub(crate) struct ProductAutomationProgressPlan {
    commands: Vec<CommandPlan>,
}

impl ProductAutomationProgressPlan {
    pub(crate) fn from_commands(commands: &[Value]) -> anyhow::Result<Self> {
        let commands = commands
            .iter()
            .enumerate()
            .map(|(index, command)| parse_command_plan(index, command))
            .collect::<anyhow::Result<Vec<_>>>()?;
        if commands.is_empty() {
            bail!("product automation progress plan requires at least one command");
        }
        Ok(Self { commands })
    }

    pub(crate) fn set_command_budget(
        &mut self,
        index: usize,
        budget: Duration,
    ) -> anyhow::Result<()> {
        let command = self
            .commands
            .get_mut(index)
            .with_context(|| format!("progress budget command index {index} is out of range"))?;
        if command.kind != "observe_gate_batch" {
            bail!("only observe_gate_batch accepts a derived progress budget");
        }
        if budget.is_zero() {
            bail!("observe_gate_batch progress budget must be positive");
        }
        command.budget = Some(budget);
        Ok(())
    }

    pub(crate) fn command_count(&self) -> usize {
        self.commands.len()
    }

    #[cfg(test)]
    pub(crate) fn command_budget(&self, index: usize) -> Option<Duration> {
        self.commands.get(index).and_then(|command| command.budget)
    }
}

fn parse_command_plan(index: usize, value: &Value) -> anyhow::Result<CommandPlan> {
    let object = value
        .as_object()
        .with_context(|| format!("automation command {index} must be an object"))?;
    let raw_kind = object
        .get("command")
        .and_then(Value::as_str)
        .with_context(|| format!("automation command {index} requires a string command kind"))?;
    let kind = known_command_kind(raw_kind)
        .with_context(|| format!("automation command {index} has an unknown command kind"))?;

    let budget = match kind {
        "wait_for_import_progress"
        | "wait_for_imported_open_ready"
        | "wait_for"
        | "await_active_view_gpu_timing" => Some(Duration::from_millis(required_u64(
            object,
            "timeout_ms",
            index,
        )?)),
        "switch_dataset" => Some(Duration::from_secs(120)),
        "camera_orbit_sequence"
        | "camera_pan_sequence"
        | "camera_zoom_sequence"
        | "cross_section_rotate_sequence"
        | "cross_section_pan_sequence"
        | "cross_section_zoom_sequence"
        | "cross_section_slice_sequence" => {
            let duration_ms = required_u64(object, "duration_ms", index)?;
            if !(1..=MAX_SEQUENCE_DURATION_MS).contains(&duration_ms) {
                bail!(
                    "automation command {index} duration_ms must be in 1..={MAX_SEQUENCE_DURATION_MS}"
                );
            }
            Some(SEQUENCE_BASE_BUDGET.saturating_add(Duration::from_millis(duration_ms)))
        }
        "sleep_frames" => {
            let frames = required_u64(object, "frames", index)?;
            if !(1..=MAX_SLEEP_FRAMES).contains(&frames) {
                bail!("automation command {index} frames must be in 1..={MAX_SLEEP_FRAMES}");
            }
            Some(
                Duration::from_secs(1)
                    .saturating_add(Duration::from_millis(frames.saturating_mul(100))),
            )
        }
        "capture_screenshot" | "assert" | "probe_hover" | "primary_click" => {
            Some(Duration::from_secs(30))
        }
        "observe_gate_batch" | "hold_for_external_kill" => None,
        // Commands without a script-declared duration are still bounded. A
        // live heartbeat proves that the event loop is running; it does not
        // prove that one semantic command is making progress. Leaving these
        // commands unbounded would therefore recreate the static-window
        // failure that this protocol exists to stop.
        _ => Some(DEFAULT_COMMAND_BUDGET),
    };

    for reserved in ["timeout_ms", "duration_ms", "frames"] {
        let accepted = match reserved {
            "timeout_ms" => matches!(
                kind,
                "wait_for_import_progress"
                    | "wait_for_imported_open_ready"
                    | "wait_for"
                    | "await_active_view_gpu_timing"
            ),
            "duration_ms" => kind.ends_with("_sequence"),
            "frames" => kind == "sleep_frames",
            _ => false,
        };
        if object.contains_key(reserved) && !accepted {
            bail!("automation command {index} has unexpected {reserved}");
        }
    }

    Ok(CommandPlan { kind, budget })
}

fn required_u64(object: &Map<String, Value>, field: &str, index: usize) -> anyhow::Result<u64> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .with_context(|| format!("automation command {index} requires an unsigned {field}"))
}

fn known_command_kind(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "open_dataset" => "open_dataset",
        "switch_dataset" => "switch_dataset",
        "new_project" => "new_project",
        "initial_save_with_edit" => "initial_save_with_edit",
        "open_project" => "open_project",
        "recover_automatic_autosave" => "recover_automatic_autosave",
        "save_project_as" => "save_project_as",
        "close_project_store" => "close_project_store",
        "write_external_kill_checkpoint" => "write_external_kill_checkpoint",
        "hold_for_external_kill" => "hold_for_external_kill",
        "cancel_source_verification" => "cancel_source_verification",
        "cancel_active_source_verification" => "cancel_active_source_verification",
        "request_source_verification" => "request_source_verification",
        "begin_tiff_import_setup" => "begin_tiff_import_setup",
        "start_reviewed_import" => "start_reviewed_import",
        "wait_for_import_progress" => "wait_for_import_progress",
        "cancel_import" => "cancel_import",
        "wait_for_imported_open_ready" => "wait_for_imported_open_ready",
        "wait_for" => "wait_for",
        "await_active_view_gpu_timing" => "await_active_view_gpu_timing",
        "observe_gate_batch" => "observe_gate_batch",
        "set_viewport_size" => "set_viewport_size",
        "set_mapped_client_pixels" => "set_mapped_client_pixels",
        "set_render_target_size" => "set_render_target_size",
        "set_four_panel_viewports" => "set_four_panel_viewports",
        "set_viewer_layout" => "set_viewer_layout",
        "set_time_index" => "set_time_index",
        "set_layer_visibility" => "set_layer_visibility",
        "set_layer_order" => "set_layer_order",
        "set_render_mode" => "set_render_mode",
        "set_layer_render_mode" => "set_layer_render_mode",
        "set_projection" => "set_projection",
        "set_layer_sampling" => "set_layer_sampling",
        "set_layer_iso_shading" => "set_layer_iso_shading",
        "set_iso_light" => "set_iso_light",
        "set_iso_display_level" => "set_iso_display_level",
        "set_dvr_density_scale" => "set_dvr_density_scale",
        "set_layer_opacity" => "set_layer_opacity",
        "set_layer_window" => "set_layer_window",
        "set_camera_view" => "set_camera_view",
        "camera_fit_data" => "camera_fit_data",
        "camera_orbit" => "camera_orbit",
        "camera_pan" => "camera_pan",
        "camera_zoom" => "camera_zoom",
        "camera_orbit_sequence" => "camera_orbit_sequence",
        "camera_pan_sequence" => "camera_pan_sequence",
        "camera_zoom_sequence" => "camera_zoom_sequence",
        "set_active_cross_section_panel" => "set_active_cross_section_panel",
        "set_cross_section_view" => "set_cross_section_view",
        "cross_section_rotate_sequence" => "cross_section_rotate_sequence",
        "cross_section_pan_sequence" => "cross_section_pan_sequence",
        "cross_section_zoom_sequence" => "cross_section_zoom_sequence",
        "cross_section_slice_sequence" => "cross_section_slice_sequence",
        "set_active_tool" => "set_active_tool",
        "probe_hover" => "probe_hover",
        "primary_click" => "primary_click",
        "copy_diagnostics" => "copy_diagnostics",
        "sample_diagnostics" => "sample_diagnostics",
        "capture_screenshot" => "capture_screenshot",
        "assert" => "assert",
        "sleep_frames" => "sleep_frames",
        "quit" => "quit",
        _ => return None,
    })
}

pub(crate) struct ProductAutomationProgressLaunch {
    path: PathBuf,
    nonce: String,
}

impl ProductAutomationProgressLaunch {
    pub(crate) fn new(role_root: &Path) -> anyhow::Result<Self> {
        if !role_root.is_absolute() {
            bail!("product automation role root must be absolute");
        }
        let metadata = fs::symlink_metadata(role_root)
            .context("product automation role root is unavailable")?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("product automation role root must be a nonsymlink directory");
        }
        let path = role_root.join(PROGRESS_FILE_NAME);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => bail!("product automation progress sidecar must be absent before launch"),
            Err(_) => bail!("product automation progress sidecar could not be inspected"),
        }
        Ok(Self {
            path,
            nonce: generate_nonce()?,
        })
    }

    pub(crate) fn apply_to_command(&self, command: &mut Command) {
        command
            .env(PROGRESS_PATH_ENV, &self.path)
            .env(PROGRESS_NONCE_ENV, &self.nonce);
    }

    pub(crate) fn monitor(
        &self,
        plan: ProductAutomationProgressPlan,
        started_at: Instant,
    ) -> ProductAutomationProgressMonitor {
        ProductAutomationProgressMonitor::new(
            self.path.clone(),
            self.nonce.clone(),
            plan,
            started_at,
        )
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

fn generate_nonce() -> anyhow::Result<String> {
    let mut bytes = [0_u8; NONCE_BYTES];
    File::open("/dev/urandom")
        .context("failed to open the operating-system random source")?
        .read_exact(&mut bytes)
        .context("failed to read the operating-system random source")?;
    let mut nonce = String::with_capacity(NONCE_BYTES * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        nonce.push(char::from(HEX[usize::from(byte >> 4)]));
        nonce.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(nonce)
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressRecord {
    schema: String,
    schema_version: u32,
    nonce: String,
    heartbeat_sequence: u64,
    command_count: usize,
    state: ProgressState,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ProgressState {
    Command {
        index: usize,
        command_kind: String,
        elapsed_ms: u64,
    },
    Closeout {
        elapsed_ms: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SafeProgressSnapshot {
    pub(crate) heartbeat_sequence: u64,
    pub(crate) command_count: usize,
    pub(crate) state: SafeProgressState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SafeProgressState {
    Command {
        index: usize,
        command_kind: &'static str,
        elapsed_ms: u64,
    },
    Closeout {
        elapsed_ms: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SemanticProgress {
    Command { index: usize },
    Closeout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProgressFailure {
    AdmissionTimeout,
    MissingAfterAdmission,
    InvalidProgressRecord,
    NonceMismatch,
    InvalidHeartbeatSequence,
    HeartbeatSequenceRegressed,
    HeartbeatSequenceMutated,
    HeartbeatStale,
    CommandCountMismatch,
    CommandIndexOutOfRange,
    CommandIndexRegressed,
    CommandKindMismatch,
    CommandElapsedRegressed,
    CommandTimeout,
    StateRegressedAfterCloseout,
    CloseoutElapsedRegressed,
    CloseoutTimeout,
    MissingCloseoutAtExit,
}

impl ProgressFailure {
    pub(crate) fn reason_code(self) -> &'static str {
        match self {
            Self::AdmissionTimeout => "progress_admission_timeout",
            Self::MissingAfterAdmission => "progress_missing_after_admission",
            Self::InvalidProgressRecord => "progress_invalid_record",
            Self::NonceMismatch => "progress_nonce_mismatch",
            Self::InvalidHeartbeatSequence => "progress_invalid_heartbeat_sequence",
            Self::HeartbeatSequenceRegressed => "progress_heartbeat_sequence_regressed",
            Self::HeartbeatSequenceMutated => "progress_heartbeat_sequence_mutated",
            Self::HeartbeatStale => "progress_heartbeat_stale",
            Self::CommandCountMismatch => "progress_command_count_mismatch",
            Self::CommandIndexOutOfRange => "progress_command_index_out_of_range",
            Self::CommandIndexRegressed => "progress_command_index_regressed",
            Self::CommandKindMismatch => "progress_command_kind_mismatch",
            Self::CommandElapsedRegressed => "progress_command_elapsed_regressed",
            Self::CommandTimeout => "progress_command_timeout",
            Self::StateRegressedAfterCloseout => "progress_state_regressed_after_closeout",
            Self::CloseoutElapsedRegressed => "progress_closeout_elapsed_regressed",
            Self::CloseoutTimeout => "progress_closeout_timeout",
            Self::MissingCloseoutAtExit => "progress_missing_closeout_at_exit",
        }
    }
}

impl std::fmt::Display for ProgressFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.reason_code())
    }
}

impl std::error::Error for ProgressFailure {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProgressMonitorAction {
    Continue,
    Emit(SafeProgressSnapshot),
    Terminate(ProgressFailure),
}

pub(crate) struct ProductAutomationProgressMonitor {
    path: PathBuf,
    nonce: String,
    plan: ProductAutomationProgressPlan,
    started_at: Instant,
    admitted: bool,
    last_record: Option<ProgressRecord>,
    last_sequence_advance_at: Option<Instant>,
    command_deadline: Option<(Instant, Duration)>,
    closeout_deadline: Option<(Instant, Duration)>,
    last_emitted_at: Option<Instant>,
    last_emitted_semantic: Option<SemanticProgress>,
}

impl ProductAutomationProgressMonitor {
    fn new(
        path: PathBuf,
        nonce: String,
        plan: ProductAutomationProgressPlan,
        started_at: Instant,
    ) -> Self {
        Self {
            path,
            nonce,
            plan,
            started_at,
            admitted: false,
            last_record: None,
            last_sequence_advance_at: None,
            command_deadline: None,
            closeout_deadline: None,
            last_emitted_at: None,
            last_emitted_semantic: None,
        }
    }

    pub(crate) fn poll_at(&mut self, now: Instant) -> ProgressMonitorAction {
        let record = match self.read_record() {
            Ok(Some(record)) => record,
            Ok(None) if self.admitted => {
                return ProgressMonitorAction::Terminate(ProgressFailure::MissingAfterAdmission);
            }
            Ok(None) if elapsed_since(now, self.started_at) >= ADMISSION_TIMEOUT => {
                return ProgressMonitorAction::Terminate(ProgressFailure::AdmissionTimeout);
            }
            Ok(None) => return ProgressMonitorAction::Continue,
            Err(failure) => return ProgressMonitorAction::Terminate(failure),
        };

        if record.heartbeat_sequence == 0 {
            return ProgressMonitorAction::Terminate(ProgressFailure::InvalidHeartbeatSequence);
        }
        if let Some(previous) = self.last_record.as_ref() {
            if record.heartbeat_sequence < previous.heartbeat_sequence {
                return ProgressMonitorAction::Terminate(
                    ProgressFailure::HeartbeatSequenceRegressed,
                );
            }
            if record.heartbeat_sequence == previous.heartbeat_sequence {
                if record != *previous {
                    return ProgressMonitorAction::Terminate(
                        ProgressFailure::HeartbeatSequenceMutated,
                    );
                }
                return self.evaluate_deadlines_and_report(now);
            }
        }

        if let Err(failure) = self.validate_advanced_record(&record, now) {
            return ProgressMonitorAction::Terminate(failure);
        }
        self.admitted = true;
        self.last_sequence_advance_at = Some(now);
        self.last_record = Some(record);
        self.evaluate_deadlines_and_report(now)
    }

    pub(crate) fn finalize_at_exit(&mut self, now: Instant) -> Option<ProgressFailure> {
        if let ProgressMonitorAction::Terminate(failure) = self.poll_at(now) {
            return Some(failure);
        }
        if !self.admitted
            || !matches!(
                self.last_record.as_ref().map(|record| &record.state),
                Some(ProgressState::Closeout { .. })
            )
        {
            return Some(ProgressFailure::MissingCloseoutAtExit);
        }
        None
    }

    fn read_record(&self) -> Result<Option<ProgressRecord>, ProgressFailure> {
        match fs::symlink_metadata(&self.path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(ProgressFailure::InvalidProgressRecord),
            Ok(_) => {}
        }
        let bytes = read_bounded_regular_file(
            &self.path,
            MAX_PROGRESS_BYTES,
            "product automation progress sidecar",
        )
        .map_err(|_| ProgressFailure::InvalidProgressRecord)?;
        let record: ProgressRecord =
            serde_json::from_slice(&bytes).map_err(|_| ProgressFailure::InvalidProgressRecord)?;
        if record.schema != PROGRESS_SCHEMA || record.schema_version != PROGRESS_SCHEMA_VERSION {
            return Err(ProgressFailure::InvalidProgressRecord);
        }
        if record.nonce != self.nonce {
            return Err(ProgressFailure::NonceMismatch);
        }
        Ok(Some(record))
    }

    fn validate_advanced_record(
        &mut self,
        record: &ProgressRecord,
        now: Instant,
    ) -> Result<(), ProgressFailure> {
        if record.command_count != self.plan.commands.len() {
            return Err(ProgressFailure::CommandCountMismatch);
        }
        match &record.state {
            ProgressState::Command {
                index,
                command_kind,
                elapsed_ms,
            } => {
                let Some(command) = self.plan.commands.get(*index) else {
                    return Err(ProgressFailure::CommandIndexOutOfRange);
                };
                if command_kind != command.kind {
                    return Err(ProgressFailure::CommandKindMismatch);
                }
                let command_changed = match self.last_record.as_ref().map(|last| &last.state) {
                    Some(ProgressState::Command {
                        index: previous_index,
                        elapsed_ms: previous_elapsed,
                        ..
                    }) => {
                        if index < previous_index {
                            return Err(ProgressFailure::CommandIndexRegressed);
                        }
                        if index == previous_index && elapsed_ms < previous_elapsed {
                            return Err(ProgressFailure::CommandElapsedRegressed);
                        }
                        index != previous_index
                    }
                    Some(ProgressState::Closeout { .. }) => {
                        return Err(ProgressFailure::StateRegressedAfterCloseout);
                    }
                    None => true,
                };
                if command_changed {
                    self.closeout_deadline = None;
                    self.command_deadline = command.budget.map(|budget| {
                        let limit = budget.saturating_add(COMMAND_GRACE);
                        (
                            now,
                            limit.saturating_sub(Duration::from_millis(*elapsed_ms)),
                        )
                    });
                }
            }
            ProgressState::Closeout { elapsed_ms } => {
                let closeout_changed = match self.last_record.as_ref().map(|last| &last.state) {
                    Some(ProgressState::Closeout {
                        elapsed_ms: previous_elapsed,
                    }) => {
                        if elapsed_ms < previous_elapsed {
                            return Err(ProgressFailure::CloseoutElapsedRegressed);
                        }
                        false
                    }
                    Some(ProgressState::Command { .. }) | None => true,
                };
                if closeout_changed {
                    self.command_deadline = None;
                    self.closeout_deadline = Some((
                        now,
                        CLOSEOUT_TIMEOUT.saturating_sub(Duration::from_millis(*elapsed_ms)),
                    ));
                }
            }
        }
        Ok(())
    }

    fn evaluate_deadlines_and_report(&mut self, now: Instant) -> ProgressMonitorAction {
        let Some(record) = self.last_record.as_ref() else {
            return ProgressMonitorAction::Continue;
        };
        match &record.state {
            ProgressState::Command { .. } => {
                if self
                    .last_sequence_advance_at
                    .is_some_and(|at| elapsed_since(now, at) >= HEARTBEAT_STALE_TIMEOUT)
                {
                    return ProgressMonitorAction::Terminate(ProgressFailure::HeartbeatStale);
                }
                if self
                    .command_deadline
                    .is_some_and(|(at, duration)| elapsed_since(now, at) >= duration)
                {
                    return ProgressMonitorAction::Terminate(ProgressFailure::CommandTimeout);
                }
            }
            ProgressState::Closeout { .. } => {
                if self
                    .closeout_deadline
                    .is_some_and(|(at, duration)| elapsed_since(now, at) >= duration)
                {
                    return ProgressMonitorAction::Terminate(ProgressFailure::CloseoutTimeout);
                }
            }
        }

        let semantic = match &record.state {
            ProgressState::Command { index, .. } => SemanticProgress::Command { index: *index },
            ProgressState::Closeout { .. } => SemanticProgress::Closeout,
        };
        let should_emit = self.last_emitted_at.is_none()
            || self.last_emitted_semantic != Some(semantic)
            || self
                .last_emitted_at
                .is_some_and(|at| elapsed_since(now, at) >= SAFE_PROGRESS_INTERVAL);
        if !should_emit {
            return ProgressMonitorAction::Continue;
        }
        self.last_emitted_at = Some(now);
        self.last_emitted_semantic = Some(semantic);
        ProgressMonitorAction::Emit(self.safe_snapshot(record))
    }

    fn safe_snapshot(&self, record: &ProgressRecord) -> SafeProgressSnapshot {
        let state = match &record.state {
            ProgressState::Command {
                index, elapsed_ms, ..
            } => SafeProgressState::Command {
                index: *index,
                command_kind: self.plan.commands[*index].kind,
                elapsed_ms: *elapsed_ms,
            },
            ProgressState::Closeout { elapsed_ms } => SafeProgressState::Closeout {
                elapsed_ms: *elapsed_ms,
            },
        };
        SafeProgressSnapshot {
            heartbeat_sequence: record.heartbeat_sequence,
            command_count: record.command_count,
            state,
        }
    }
}

fn elapsed_since(now: Instant, earlier: Instant) -> Duration {
    now.checked_duration_since(earlier)
        .unwrap_or(Duration::ZERO)
}

pub(crate) fn safe_progress_line(
    sample: usize,
    scenario: &str,
    role: &str,
    snapshot: &SafeProgressSnapshot,
) -> anyhow::Result<String> {
    validate_safe_token(scenario, "scenario")?;
    validate_safe_token(role, "role")?;
    let state = match &snapshot.state {
        SafeProgressState::Command {
            index,
            command_kind,
            elapsed_ms,
        } => format!(
            "state=command command_index={index} command_kind={command_kind} elapsed_ms={elapsed_ms}"
        ),
        SafeProgressState::Closeout { elapsed_ms } => {
            format!("state=closeout elapsed_ms={elapsed_ms}")
        }
    };
    Ok(format!(
        "viewer_progress sample={sample} scenario={scenario} role={role} heartbeat_sequence={} command_count={} {state}",
        snapshot.heartbeat_sequence, snapshot.command_count,
    ))
}

fn validate_safe_token(value: &str, label: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("progress {label} must be a bounded safe ASCII token");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NONCE: &str = "0123456789abcdef0123456789abcdef";

    fn plan(commands: Vec<Value>) -> ProductAutomationProgressPlan {
        ProductAutomationProgressPlan::from_commands(&commands).unwrap()
    }

    fn make_monitor(
        root: &Path,
        commands: Vec<Value>,
        now: Instant,
    ) -> ProductAutomationProgressMonitor {
        ProductAutomationProgressMonitor::new(
            root.join(PROGRESS_FILE_NAME),
            NONCE.to_owned(),
            plan(commands),
            now,
        )
    }

    fn write_command(
        root: &Path,
        sequence: u64,
        count: usize,
        index: usize,
        kind: &str,
        elapsed_ms: u64,
    ) {
        fs::write(
            root.join(PROGRESS_FILE_NAME),
            serde_json::to_vec(&json!({
                "schema": PROGRESS_SCHEMA,
                "schema_version": PROGRESS_SCHEMA_VERSION,
                "nonce": NONCE,
                "heartbeat_sequence": sequence,
                "command_count": count,
                "state": {
                    "kind": "command",
                    "index": index,
                    "command_kind": kind,
                    "elapsed_ms": elapsed_ms,
                }
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn write_closeout(root: &Path, sequence: u64, count: usize, elapsed_ms: u64) {
        fs::write(
            root.join(PROGRESS_FILE_NAME),
            serde_json::to_vec(&json!({
                "schema": PROGRESS_SCHEMA,
                "schema_version": PROGRESS_SCHEMA_VERSION,
                "nonce": NONCE,
                "heartbeat_sequence": sequence,
                "command_count": count,
                "state": { "kind": "closeout", "elapsed_ms": elapsed_ms }
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn admission_timeout_is_exact() {
        let root = tempfile::tempdir().unwrap();
        let origin = Instant::now();
        let mut monitor = make_monitor(root.path(), vec![json!({ "command": "quit" })], origin);
        assert_eq!(
            monitor.poll_at(origin + ADMISSION_TIMEOUT - Duration::from_nanos(1)),
            ProgressMonitorAction::Continue
        );
        assert_eq!(
            monitor.poll_at(origin + ADMISSION_TIMEOUT),
            ProgressMonitorAction::Terminate(ProgressFailure::AdmissionTimeout)
        );
    }

    #[test]
    fn heartbeat_stale_boundary_resets_only_on_sequence_advance() {
        let root = tempfile::tempdir().unwrap();
        let origin = Instant::now();
        let mut monitor = make_monitor(root.path(), vec![json!({ "command": "quit" })], origin);
        write_command(root.path(), 1, 1, 0, "quit", 0);
        assert!(matches!(
            monitor.poll_at(origin),
            ProgressMonitorAction::Emit(_)
        ));
        assert_eq!(
            monitor.poll_at(origin + HEARTBEAT_STALE_TIMEOUT - Duration::from_nanos(1)),
            ProgressMonitorAction::Continue
        );
        write_command(root.path(), 2, 1, 0, "quit", 4_999);
        assert_eq!(
            monitor.poll_at(origin + HEARTBEAT_STALE_TIMEOUT - Duration::from_nanos(1)),
            ProgressMonitorAction::Continue
        );
        assert!(matches!(
            monitor.poll_at(origin + HEARTBEAT_STALE_TIMEOUT * 2 - Duration::from_nanos(2)),
            ProgressMonitorAction::Continue | ProgressMonitorAction::Emit(_)
        ));
        assert_eq!(
            monitor.poll_at(origin + HEARTBEAT_STALE_TIMEOUT * 2 - Duration::from_nanos(1)),
            ProgressMonitorAction::Terminate(ProgressFailure::HeartbeatStale)
        );
    }

    #[test]
    fn malformed_nonce_and_same_sequence_mutation_are_fatal() {
        let root = tempfile::tempdir().unwrap();
        let origin = Instant::now();
        let mut monitor = make_monitor(root.path(), vec![json!({ "command": "quit" })], origin);
        fs::write(root.path().join(PROGRESS_FILE_NAME), b"not-json").unwrap();
        assert_eq!(
            monitor.poll_at(origin),
            ProgressMonitorAction::Terminate(ProgressFailure::InvalidProgressRecord)
        );

        let root = tempfile::tempdir().unwrap();
        let mut monitor = make_monitor(root.path(), vec![json!({ "command": "quit" })], origin);
        write_command(root.path(), 1, 1, 0, "quit", 0);
        monitor.poll_at(origin);
        write_command(root.path(), 1, 1, 0, "quit", 1);
        assert_eq!(
            monitor.poll_at(origin + Duration::from_millis(1)),
            ProgressMonitorAction::Terminate(ProgressFailure::HeartbeatSequenceMutated)
        );

        let mut value: Value =
            serde_json::from_slice(&fs::read(root.path().join(PROGRESS_FILE_NAME)).unwrap())
                .unwrap();
        value["heartbeat_sequence"] = json!(2);
        value["nonce"] = json!("ffffffffffffffffffffffffffffffff");
        fs::write(
            root.path().join(PROGRESS_FILE_NAME),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert_eq!(
            monitor.poll_at(origin + Duration::from_millis(2)),
            ProgressMonitorAction::Terminate(ProgressFailure::NonceMismatch)
        );
    }

    #[test]
    fn command_identity_allows_forward_jumps_but_not_regression_or_wrong_kind() {
        let commands = vec![
            json!({ "command": "new_project" }),
            json!({ "command": "copy_diagnostics" }),
            json!({ "command": "quit" }),
        ];
        let root = tempfile::tempdir().unwrap();
        let origin = Instant::now();
        let mut monitor = make_monitor(root.path(), commands.clone(), origin);
        write_command(root.path(), 1, 3, 0, "new_project", 0);
        monitor.poll_at(origin);
        write_command(root.path(), 2, 3, 2, "quit", 0);
        assert!(matches!(
            monitor.poll_at(origin + Duration::from_secs(1)),
            ProgressMonitorAction::Emit(_)
        ));
        write_command(root.path(), 3, 3, 1, "copy_diagnostics", 0);
        assert_eq!(
            monitor.poll_at(origin + Duration::from_secs(2)),
            ProgressMonitorAction::Terminate(ProgressFailure::CommandIndexRegressed)
        );

        let root = tempfile::tempdir().unwrap();
        let mut monitor = make_monitor(root.path(), commands, origin);
        write_command(root.path(), 1, 3, 0, "quit", 0);
        assert_eq!(
            monitor.poll_at(origin),
            ProgressMonitorAction::Terminate(ProgressFailure::CommandKindMismatch)
        );
    }

    #[test]
    fn budgets_cover_instant_sleep_sequences_grace_and_unbounded_hold() {
        let instant_plan = plan(vec![json!({ "command": "open_dataset" })]);
        assert_eq!(
            instant_plan.commands[0].budget,
            Some(DEFAULT_COMMAND_BUDGET)
        );
        assert!(
            ProductAutomationProgressPlan::from_commands(&[json!({
                "command": "sleep_frames",
                "frames": 601
            })])
            .is_err()
        );
        let sleep_plan = plan(vec![json!({
            "command": "sleep_frames",
            "frames": 120
        })]);
        assert_eq!(sleep_plan.commands[0].budget, Some(Duration::from_secs(13)));
        let root = tempfile::tempdir().unwrap();
        let origin = Instant::now();
        let mut monitor = make_monitor(
            root.path(),
            vec![json!({ "command": "sleep_frames", "frames": 120 })],
            origin,
        );
        write_command(root.path(), 1, 1, 0, "sleep_frames", 0);
        monitor.poll_at(origin);
        assert_eq!(
            monitor.poll_at(origin + Duration::from_millis(13_499)),
            ProgressMonitorAction::Terminate(ProgressFailure::HeartbeatStale)
        );

        let root = tempfile::tempdir().unwrap();
        let mut monitor = make_monitor(
            root.path(),
            vec![json!({ "command": "wait_for", "timeout_ms": 100 })],
            origin,
        );
        write_command(root.path(), 1, 1, 0, "wait_for", 0);
        monitor.poll_at(origin);
        assert_eq!(
            monitor.poll_at(origin + Duration::from_millis(599)),
            ProgressMonitorAction::Continue
        );
        assert_eq!(
            monitor.poll_at(origin + Duration::from_millis(600)),
            ProgressMonitorAction::Terminate(ProgressFailure::CommandTimeout)
        );

        let root = tempfile::tempdir().unwrap();
        let mut monitor = make_monitor(
            root.path(),
            vec![json!({ "command": "hold_for_external_kill" })],
            origin,
        );
        write_command(root.path(), 1, 1, 0, "hold_for_external_kill", 0);
        monitor.poll_at(origin);
        write_command(root.path(), 2, 1, 0, "hold_for_external_kill", 99_000);
        assert!(matches!(
            monitor.poll_at(origin + Duration::from_secs(99)),
            ProgressMonitorAction::Continue | ProgressMonitorAction::Emit(_)
        ));
    }

    #[test]
    fn closeout_has_its_own_exact_deadline_and_is_required_at_exit() {
        let root = tempfile::tempdir().unwrap();
        let origin = Instant::now();
        let mut monitor = make_monitor(root.path(), vec![json!({ "command": "quit" })], origin);
        write_command(root.path(), 1, 1, 0, "quit", 0);
        monitor.poll_at(origin);
        assert_eq!(
            monitor.finalize_at_exit(origin + Duration::from_secs(1)),
            Some(ProgressFailure::MissingCloseoutAtExit)
        );
        write_closeout(root.path(), 2, 1, 0);
        assert!(matches!(
            monitor.poll_at(origin + Duration::from_secs(1)),
            ProgressMonitorAction::Emit(_)
        ));
        assert!(matches!(
            monitor.poll_at(origin + Duration::from_secs(11) - Duration::from_nanos(1)),
            ProgressMonitorAction::Continue | ProgressMonitorAction::Emit(_)
        ));
        assert_eq!(
            monitor.poll_at(origin + Duration::from_secs(11)),
            ProgressMonitorAction::Terminate(ProgressFailure::CloseoutTimeout)
        );

        let root = tempfile::tempdir().unwrap();
        let mut monitor = make_monitor(root.path(), vec![json!({ "command": "quit" })], origin);
        write_closeout(root.path(), 1, 1, 0);
        assert_eq!(monitor.finalize_at_exit(origin), None);
    }

    #[test]
    fn safe_reporter_is_bounded_and_contains_no_capability_or_path() {
        let snapshot = SafeProgressSnapshot {
            heartbeat_sequence: 7,
            command_count: 9,
            state: SafeProgressState::Command {
                index: 2,
                command_kind: "wait_for",
                elapsed_ms: 500,
            },
        };
        let line = safe_progress_line(3, "scenario-1", "cold", &snapshot).unwrap();
        assert!(line.contains("command_kind=wait_for"));
        assert!(!line.contains(NONCE));
        assert!(!line.contains('/'));
        assert!(safe_progress_line(3, "private scenario", "cold", &snapshot).is_err());
    }
}

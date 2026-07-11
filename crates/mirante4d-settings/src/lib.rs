//! Validated Mirante4D settings and their background filesystem owner.

#![forbid(unsafe_code)]

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SETTINGS_IDENTITY: &str = "mirante4d-settings-v1";
pub const SETTINGS_SCHEMA: &str = "mirante4d-settings";
pub const SETTINGS_SCHEMA_VERSION: u32 = 1;

pub const GIB: u64 = 1024 * 1024 * 1024;
pub const MIN_CPU_DATASET_BUDGET_BYTES: u64 = 2 * GIB;
pub const MAX_CPU_DATASET_BUDGET_BYTES: u64 = 32 * GIB;
pub const DEFAULT_UNKNOWN_CPU_DATASET_BUDGET_BYTES: u64 = 4 * GIB;
pub const MIN_GPU_BUDGET_BYTES: u64 = GIB;
pub const MAX_GPU_BUDGET_BYTES: u64 = 8 * GIB;
pub const DEFAULT_UNKNOWN_GPU_BUDGET_BYTES: u64 = GIB;
pub const MAX_SETTINGS_DOCUMENT_BYTES: usize = 64 * 1024;

const ACTOR_QUEUE_CAPACITY: usize = 8;
const EVENT_QUEUE_CAPACITY: usize = ACTOR_QUEUE_CAPACITY * 2 + 1;
const TEMPORARY_CREATE_ATTEMPTS: u64 = 16;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SettingsError {
    #[error(
        "CPU dataset budget {actual} is outside the inclusive range {minimum}..={maximum} bytes"
    )]
    CpuDatasetBudgetOutOfBounds {
        actual: u64,
        minimum: u64,
        maximum: u64,
    },
    #[error("GPU budget {actual} is outside the inclusive range {minimum}..={maximum} bytes")]
    GpuBudgetOutOfBounds {
        actual: u64,
        minimum: u64,
        maximum: u64,
    },
    #[error("unsupported settings schema {actual:?}; expected {expected:?}")]
    UnsupportedSchema {
        actual: String,
        expected: &'static str,
    },
    #[error("unsupported settings schema version {actual}; expected {expected}")]
    UnsupportedSchemaVersion { actual: u32, expected: u32 },
    #[error("settings document is invalid JSON at line {line}, column {column}: {detail}")]
    InvalidDocument {
        line: usize,
        column: usize,
        detail: String,
    },
    #[error("settings document exceeds the {maximum}-byte limit")]
    DocumentTooLarge { maximum: usize },
    #[error("neither XDG_CONFIG_HOME nor HOME is available for the Linux settings path")]
    SettingsPathUnavailable,
    #[error("settings I/O failed during {stage:?} with {kind:?}")]
    Io {
        stage: SettingsIoStage,
        kind: io::ErrorKind,
    },
    #[error(
        "settings replacement may be visible but directory durability is indeterminate ({kind:?})"
    )]
    CommitIndeterminate { kind: io::ErrorKind },
    #[error("settings actor request queue is full")]
    ActorQueueFull,
    #[error("settings actor is unavailable")]
    ActorUnavailable,
    #[error("settings actor event channel is disconnected")]
    ActorEventChannelDisconnected,
    #[error("the rejected settings file requires explicit replacement")]
    ExplicitReplacementRequired,
    #[error("settings actor thread panicked during shutdown")]
    ActorThreadPanicked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsIoStage {
    SpawnActor,
    Read,
    CreateDirectory,
    CreateTemporary,
    WriteTemporary,
    SyncTemporary,
    ReadBackTemporary,
    CommitReplacement,
    SyncDirectory,
    RemoveTemporary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourcePolicy {
    cpu_dataset_budget_bytes: u64,
    gpu_budget_bytes: u64,
}

impl ResourcePolicy {
    pub fn new(
        cpu_dataset_budget_bytes: u64,
        gpu_budget_bytes: u64,
    ) -> Result<Self, SettingsError> {
        if !(MIN_CPU_DATASET_BUDGET_BYTES..=MAX_CPU_DATASET_BUDGET_BYTES)
            .contains(&cpu_dataset_budget_bytes)
        {
            return Err(SettingsError::CpuDatasetBudgetOutOfBounds {
                actual: cpu_dataset_budget_bytes,
                minimum: MIN_CPU_DATASET_BUDGET_BYTES,
                maximum: MAX_CPU_DATASET_BUDGET_BYTES,
            });
        }
        if !(MIN_GPU_BUDGET_BYTES..=MAX_GPU_BUDGET_BYTES).contains(&gpu_budget_bytes) {
            return Err(SettingsError::GpuBudgetOutOfBounds {
                actual: gpu_budget_bytes,
                minimum: MIN_GPU_BUDGET_BYTES,
                maximum: MAX_GPU_BUDGET_BYTES,
            });
        }
        Ok(Self {
            cpu_dataset_budget_bytes,
            gpu_budget_bytes,
        })
    }

    pub fn recommended(
        installed_memory_bytes: Option<u64>,
        dedicated_gpu_memory_bytes: Option<u64>,
    ) -> Result<Self, SettingsError> {
        let cpu_dataset_budget_bytes = installed_memory_bytes.map_or(
            DEFAULT_UNKNOWN_CPU_DATASET_BUDGET_BYTES,
            recommended_cpu_dataset_budget_bytes,
        );
        let gpu_budget_bytes = dedicated_gpu_memory_bytes.map_or(
            DEFAULT_UNKNOWN_GPU_BUDGET_BYTES,
            recommended_gpu_budget_bytes,
        );
        Self::new(cpu_dataset_budget_bytes, gpu_budget_bytes)
    }

    pub const fn cpu_dataset_budget_bytes(self) -> u64 {
        self.cpu_dataset_budget_bytes
    }

    pub const fn gpu_budget_bytes(self) -> u64 {
        self.gpu_budget_bytes
    }

    pub const fn current_runtime_adapter(self) -> CurrentRuntimeResourcePolicy {
        CurrentRuntimeResourcePolicy {
            cpu_brick_cache_budget_bytes: self.cpu_dataset_budget_bytes / 2,
            cpu_whole_volume_cache_budget_bytes: self.cpu_dataset_budget_bytes / 8,
            gpu_brick_cache_budget_bytes: ((self.gpu_budget_bytes as u128 * 65) / 100) as u64,
            gpu_dense_cache_budget_bytes: self.gpu_budget_bytes / 10,
        }
    }
}

impl Default for ResourcePolicy {
    fn default() -> Self {
        Self {
            cpu_dataset_budget_bytes: DEFAULT_UNKNOWN_CPU_DATASET_BUDGET_BYTES,
            gpu_budget_bytes: DEFAULT_UNKNOWN_GPU_BUDGET_BYTES,
        }
    }
}

fn recommended_cpu_dataset_budget_bytes(installed_memory_bytes: u64) -> u64 {
    let forty_percent = ((u128::from(installed_memory_bytes) * 2) / 5) as u64;
    forty_percent.clamp(MIN_CPU_DATASET_BUDGET_BYTES, MAX_CPU_DATASET_BUDGET_BYTES)
}

fn recommended_gpu_budget_bytes(dedicated_gpu_memory_bytes: u64) -> u64 {
    MAX_GPU_BUDGET_BYTES
        .min(dedicated_gpu_memory_bytes / 2)
        .min(dedicated_gpu_memory_bytes.saturating_sub(2 * GIB))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrentRuntimeResourcePolicy {
    cpu_brick_cache_budget_bytes: u64,
    cpu_whole_volume_cache_budget_bytes: u64,
    gpu_brick_cache_budget_bytes: u64,
    gpu_dense_cache_budget_bytes: u64,
}

impl CurrentRuntimeResourcePolicy {
    pub const fn cpu_brick_cache_budget_bytes(self) -> u64 {
        self.cpu_brick_cache_budget_bytes
    }

    pub const fn cpu_whole_volume_cache_budget_bytes(self) -> u64 {
        self.cpu_whole_volume_cache_budget_bytes
    }

    pub const fn gpu_brick_cache_budget_bytes(self) -> u64 {
        self.gpu_brick_cache_budget_bytes
    }

    pub const fn gpu_dense_cache_budget_bytes(self) -> u64 {
        self.gpu_dense_cache_budget_bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsDocument {
    resource_policy: ResourcePolicy,
}

impl SettingsDocument {
    pub const fn new(resource_policy: ResourcePolicy) -> Self {
        Self { resource_policy }
    }

    pub const fn resource_policy(self) -> ResourcePolicy {
        self.resource_policy
    }

    pub fn to_json_pretty(self) -> Result<String, SettingsError> {
        let dto = SettingsDto::from(self);
        serde_json::to_string_pretty(&dto).map_err(invalid_document_error)
    }

    pub fn from_json(encoded: &str) -> Result<Self, SettingsError> {
        if encoded.len() > MAX_SETTINGS_DOCUMENT_BYTES {
            return Err(SettingsError::DocumentTooLarge {
                maximum: MAX_SETTINGS_DOCUMENT_BYTES,
            });
        }
        let dto: SettingsDto = serde_json::from_str(encoded).map_err(invalid_document_error)?;
        dto.try_into()
    }
}

impl Default for SettingsDocument {
    fn default() -> Self {
        Self::new(ResourcePolicy::default())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsDto {
    schema: String,
    schema_version: u32,
    resource_policy: ResourcePolicyDto,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourcePolicyDto {
    cpu_dataset_budget_bytes: u64,
    gpu_budget_bytes: u64,
}

impl From<SettingsDocument> for SettingsDto {
    fn from(document: SettingsDocument) -> Self {
        Self {
            schema: SETTINGS_SCHEMA.to_owned(),
            schema_version: SETTINGS_SCHEMA_VERSION,
            resource_policy: ResourcePolicyDto {
                cpu_dataset_budget_bytes: document.resource_policy.cpu_dataset_budget_bytes,
                gpu_budget_bytes: document.resource_policy.gpu_budget_bytes,
            },
        }
    }
}

impl TryFrom<SettingsDto> for SettingsDocument {
    type Error = SettingsError;

    fn try_from(dto: SettingsDto) -> Result<Self, Self::Error> {
        if dto.schema != SETTINGS_SCHEMA {
            return Err(SettingsError::UnsupportedSchema {
                actual: dto.schema,
                expected: SETTINGS_SCHEMA,
            });
        }
        if dto.schema_version != SETTINGS_SCHEMA_VERSION {
            return Err(SettingsError::UnsupportedSchemaVersion {
                actual: dto.schema_version,
                expected: SETTINGS_SCHEMA_VERSION,
            });
        }
        let policy = ResourcePolicy::new(
            dto.resource_policy.cpu_dataset_budget_bytes,
            dto.resource_policy.gpu_budget_bytes,
        )?;
        Ok(Self::new(policy))
    }
}

fn invalid_document_error(error: serde_json::Error) -> SettingsError {
    SettingsError::InvalidDocument {
        line: error.line(),
        column: error.column(),
        detail: error.to_string(),
    }
}

pub fn default_linux_settings_path() -> Result<PathBuf, SettingsError> {
    linux_settings_path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

pub fn linux_settings_path(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, SettingsError> {
    let root = xdg_config_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .map(|path| path.join(".config"))
        })
        .ok_or(SettingsError::SettingsPathUnavailable)?;
    Ok(root.join("mirante4d").join("settings.json"))
}

#[derive(Debug)]
pub enum SettingsLoadOutcome {
    Loaded {
        document: SettingsDocument,
    },
    DefaultsActiveMissing {
        document: SettingsDocument,
    },
    DefaultsActiveRejected {
        document: SettingsDocument,
        error: SettingsError,
    },
}

impl SettingsLoadOutcome {
    pub const fn active_document(&self) -> SettingsDocument {
        match self {
            Self::Loaded { document }
            | Self::DefaultsActiveMissing { document }
            | Self::DefaultsActiveRejected { document, .. } => *document,
        }
    }

    pub const fn requires_explicit_replacement(&self) -> bool {
        matches!(self, Self::DefaultsActiveRejected { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SettingsRequestId(u64);

impl SettingsRequestId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectedFileDisposition {
    Preserve,
    ReplaceExplicitly,
}

#[derive(Debug)]
pub enum SettingsEvent {
    Loaded(SettingsLoadOutcome),
    SavePending {
        request_id: SettingsRequestId,
    },
    SavePersisted {
        request_id: SettingsRequestId,
        document: SettingsDocument,
        restart_required: bool,
    },
    SaveRejected {
        request_id: SettingsRequestId,
        error: SettingsError,
    },
}

enum SettingsRequest {
    Save {
        request_id: SettingsRequestId,
        document: SettingsDocument,
        rejected_file_disposition: RejectedFileDisposition,
    },
    Shutdown,
}

pub struct SettingsActor {
    sender: SyncSender<SettingsRequest>,
    events: Option<Receiver<SettingsEvent>>,
    worker: Option<JoinHandle<()>>,
}

impl SettingsActor {
    pub fn spawn(path: PathBuf, defaults: SettingsDocument) -> Result<Self, SettingsError> {
        let (sender, requests) = mpsc::sync_channel(ACTOR_QUEUE_CAPACITY);
        let (event_sender, events) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
        let worker = thread::Builder::new()
            .name("mirante4d-settings".to_owned())
            .spawn(move || run_settings_actor(path, defaults, requests, event_sender))
            .map_err(|error| io_error(SettingsIoStage::SpawnActor, error))?;
        Ok(Self {
            sender,
            events: Some(events),
            worker: Some(worker),
        })
    }

    pub fn request_save(
        &self,
        request_id: SettingsRequestId,
        document: SettingsDocument,
        rejected_file_disposition: RejectedFileDisposition,
    ) -> Result<(), SettingsError> {
        self.sender
            .try_send(SettingsRequest::Save {
                request_id,
                document,
                rejected_file_disposition,
            })
            .map_err(map_try_send_error)
    }

    pub fn try_recv(&self) -> Result<Option<SettingsEvent>, SettingsError> {
        let events = self
            .events
            .as_ref()
            .ok_or(SettingsError::ActorEventChannelDisconnected)?;
        match events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(SettingsError::ActorEventChannelDisconnected),
        }
    }

    pub fn shutdown(mut self) -> Result<(), SettingsError> {
        // Release event backpressure before joining. A worker blocked while
        // reporting an event then exits instead of deadlocking shutdown.
        let _ = self.events.take();
        // Joined shutdown is a composition-root operation, not an interaction
        // callback. A blocking send also closes the full-request-queue edge:
        // it either enqueues shutdown or observes the worker receiver close.
        let _ = self.sender.send(SettingsRequest::Shutdown);
        join_worker(self.worker.take())
    }
}

impl Drop for SettingsActor {
    fn drop(&mut self) {
        let _ = self.events.take();
        let _ = self.sender.try_send(SettingsRequest::Shutdown);
        // Dropping a JoinHandle detaches. Explicit `shutdown` is the joined
        // composition-root path; Drop must never block an interaction thread.
        let _ = self.worker.take();
    }
}

fn map_try_send_error(error: TrySendError<SettingsRequest>) -> SettingsError {
    match error {
        TrySendError::Full(_) => SettingsError::ActorQueueFull,
        TrySendError::Disconnected(_) => SettingsError::ActorUnavailable,
    }
}

fn join_worker(worker: Option<JoinHandle<()>>) -> Result<(), SettingsError> {
    match worker {
        Some(worker) => worker
            .join()
            .map_err(|_| SettingsError::ActorThreadPanicked),
        None => Ok(()),
    }
}

fn run_settings_actor(
    path: PathBuf,
    defaults: SettingsDocument,
    requests: Receiver<SettingsRequest>,
    events: SyncSender<SettingsEvent>,
) {
    let load_outcome = load_document(&path, defaults);
    let mut rejected_file_present = load_outcome.requires_explicit_replacement();
    if events.send(SettingsEvent::Loaded(load_outcome)).is_err() {
        return;
    }

    while let Ok(request) = requests.recv() {
        match request {
            SettingsRequest::Shutdown => return,
            SettingsRequest::Save {
                request_id,
                document,
                rejected_file_disposition,
            } => {
                if events
                    .send(SettingsEvent::SavePending { request_id })
                    .is_err()
                {
                    return;
                }
                let result = match rejected_file_disposition {
                    RejectedFileDisposition::Preserve => {
                        match current_target_requires_explicit_replacement(&path) {
                            Ok(target_rejected) => {
                                if rejected_file_present || target_rejected {
                                    rejected_file_present = true;
                                    Err(SettingsError::ExplicitReplacementRequired)
                                } else {
                                    save_document_atomically(&path, document)
                                }
                            }
                            Err(error) => Err(error),
                        }
                    }
                    RejectedFileDisposition::ReplaceExplicitly => {
                        save_document_atomically(&path, document)
                    }
                };
                match result {
                    Ok(()) => {
                        rejected_file_present = false;
                        if events
                            .send(SettingsEvent::SavePersisted {
                                request_id,
                                document,
                                restart_required: true,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(error) => {
                        if matches!(error, SettingsError::CommitIndeterminate { .. }) {
                            // The replacement may be visible but cannot be
                            // treated as durable success. Require an explicit
                            // retry/reload decision before another overwrite.
                            rejected_file_present = true;
                        }
                        if events
                            .send(SettingsEvent::SaveRejected { request_id, error })
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
        }
    }
}

fn current_target_requires_explicit_replacement(path: &Path) -> Result<bool, SettingsError> {
    match read_bounded_document(path, SettingsIoStage::Read) {
        Ok(encoded) => Ok(SettingsDocument::from_json(&encoded).is_err()),
        Err(SettingsError::Io {
            kind: io::ErrorKind::NotFound,
            ..
        }) => Ok(false),
        Err(error) => Err(error),
    }
}

fn load_document(path: &Path, defaults: SettingsDocument) -> SettingsLoadOutcome {
    match read_bounded_document(path, SettingsIoStage::Read) {
        Ok(encoded) => match SettingsDocument::from_json(&encoded) {
            Ok(document) => SettingsLoadOutcome::Loaded { document },
            Err(error) => SettingsLoadOutcome::DefaultsActiveRejected {
                document: defaults,
                error,
            },
        },
        Err(SettingsError::Io {
            kind: io::ErrorKind::NotFound,
            ..
        }) => SettingsLoadOutcome::DefaultsActiveMissing { document: defaults },
        Err(error) => SettingsLoadOutcome::DefaultsActiveRejected {
            document: defaults,
            error,
        },
    }
}

fn save_document_atomically(path: &Path, document: SettingsDocument) -> Result<(), SettingsError> {
    save_document_atomically_with_commit(path, document, |temporary, target| {
        fs::rename(temporary, target)
    })
}

fn save_document_atomically_with_commit(
    path: &Path,
    document: SettingsDocument,
    commit: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> Result<(), SettingsError> {
    save_document_atomically_with_commit_and_sync(path, document, commit, sync_directory)
}

fn save_document_atomically_with_commit_and_sync(
    path: &Path,
    document: SettingsDocument,
    commit: impl FnOnce(&Path, &Path) -> io::Result<()>,
    sync_parent: impl FnOnce(&Path) -> Result<(), SettingsError>,
) -> Result<(), SettingsError> {
    let parent = path
        .parent()
        .ok_or(SettingsError::SettingsPathUnavailable)?;
    fs::create_dir_all(parent)
        .map_err(|error| io_error(SettingsIoStage::CreateDirectory, error))?;
    let encoded = format!("{}\n", document.to_json_pretty()?);
    let (temporary_path, mut temporary_file) = create_unique_temporary(path)?;
    let mut temporary = OwnedTemporary::new(temporary_path);

    let write_result = temporary_file
        .write_all(encoded.as_bytes())
        .map_err(|error| io_error(SettingsIoStage::WriteTemporary, error))
        .and_then(|()| {
            temporary_file
                .sync_all()
                .map_err(|error| io_error(SettingsIoStage::SyncTemporary, error))
        });
    drop(temporary_file);

    let result = (|| {
        write_result?;
        let read_back =
            read_bounded_document(temporary.path(), SettingsIoStage::ReadBackTemporary)?;
        let decoded = SettingsDocument::from_json(&read_back)?;
        if decoded != document {
            return Err(SettingsError::InvalidDocument {
                line: 0,
                column: 0,
                detail: "settings readback did not match the submitted document".to_owned(),
            });
        }

        commit(temporary.path(), path)
            .map_err(|error| io_error(SettingsIoStage::CommitReplacement, error))?;
        temporary.mark_committed();
        match sync_parent(parent) {
            Ok(()) => Ok(()),
            Err(SettingsError::Io { kind, .. }) => Err(SettingsError::CommitIndeterminate { kind }),
            Err(error) => Err(error),
        }
    })();
    if result.is_err() {
        temporary.cleanup_uncommitted()?;
    }
    result
}

fn create_unique_temporary(path: &Path) -> Result<(PathBuf, File), SettingsError> {
    let parent = path
        .parent()
        .ok_or(SettingsError::SettingsPathUnavailable)?;
    let file_name = path
        .file_name()
        .ok_or(SettingsError::SettingsPathUnavailable)?
        .to_string_lossy();
    for _ in 0..TEMPORARY_CREATE_ATTEMPTS {
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let candidate = parent.join(format!(".{file_name}.tmp-{timestamp:x}-{sequence:x}"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error(SettingsIoStage::CreateTemporary, error)),
        }
    }
    Err(SettingsError::Io {
        stage: SettingsIoStage::CreateTemporary,
        kind: io::ErrorKind::AlreadyExists,
    })
}

fn sync_directory(path: &Path) -> Result<(), SettingsError> {
    let directory =
        File::open(path).map_err(|error| io_error(SettingsIoStage::SyncDirectory, error))?;
    directory
        .sync_all()
        .map_err(|error| io_error(SettingsIoStage::SyncDirectory, error))
}

fn read_bounded_document(path: &Path, stage: SettingsIoStage) -> Result<String, SettingsError> {
    let file = File::open(path).map_err(|error| io_error(stage, error))?;
    let mut encoded = Vec::with_capacity(MAX_SETTINGS_DOCUMENT_BYTES.min(4096));
    file.take(MAX_SETTINGS_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut encoded)
        .map_err(|error| io_error(stage, error))?;
    if encoded.len() > MAX_SETTINGS_DOCUMENT_BYTES {
        return Err(SettingsError::DocumentTooLarge {
            maximum: MAX_SETTINGS_DOCUMENT_BYTES,
        });
    }
    String::from_utf8(encoded).map_err(|error| SettingsError::InvalidDocument {
        line: 0,
        column: 0,
        detail: format!("settings document is not UTF-8: {error}"),
    })
}

fn io_error(stage: SettingsIoStage, error: io::Error) -> SettingsError {
    SettingsError::Io {
        stage,
        kind: error.kind(),
    }
}

struct OwnedTemporary {
    path: PathBuf,
    committed: bool,
}

impl OwnedTemporary {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn mark_committed(&mut self) {
        self.committed = true;
    }

    fn cleanup_uncommitted(&mut self) -> Result<(), SettingsError> {
        if self.committed {
            return Ok(());
        }
        match fs::remove_file(&self.path) {
            Ok(()) => {
                self.committed = true;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.committed = true;
                Ok(())
            }
            Err(error) => Err(io_error(SettingsIoStage::RemoveTemporary, error)),
        }
    }
}

impl Drop for OwnedTemporary {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests;

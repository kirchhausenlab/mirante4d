//! Private, temporary project-v15 persistence bridge.
//!
//! The canonical project model remains serialization-neutral. This module is
//! the only interim owner of the experimental v15 wire DTO and its background
//! filesystem actor, and is deleted at WP-10B.

use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

use mirante4d_application::OperationToken;
use mirante4d_domain::{
    CameraView, CrossSectionView, DisplayWindow, DvrOpacityTransfer, IsoLightState,
    IsoShadingPolicy, LayerTransfer, LogicalLayerKey, Opacity, Projection, RenderMode, RenderState,
    RgbColor, SamplingPolicy, TimeIndex, TransferCurve, UnitQuaternion, ViewerLayout, WorldPoint3,
};
use mirante4d_identity::{
    ArtifactContentId, DerivationRecordId, ExactBytesDigest, MediaType, ObjectRole, PackageId,
    RawObjectDescriptor, RecipeId, ReleaseId, ScientificContentId,
};
use mirante4d_project_model::{
    ArtifactCompleteness, ArtifactHandleId, ArtifactRecoverability, ArtifactReference,
    ArtifactSchema, ChannelPreset, ChannelPresetEntry, ChannelPresetId, DatasetLocatorHint,
    DatasetReference, LayerViewState, ProjectGenerationProjection, ProjectId,
    ProjectRevisionHighWater, ProjectRevisionId, ProjectState, ViewState,
};
use serde::{Deserialize, Serialize};

pub(crate) const PROJECT_V15_SCHEMA: &str = "mirante4d-project-v15";
pub(crate) const PROJECT_V15_SCHEMA_VERSION: u32 = 1;
pub(crate) const MAX_PROJECT_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

const PROJECT_DOCUMENT_FILE_NAME: &str = "project.json";
const REQUEST_QUEUE_CAPACITY: usize = 4;
const EVENT_QUEUE_CAPACITY: usize = 8;
const READ_CHUNK_BYTES: usize = 64 * 1024;
const TEMPORARY_CREATE_ATTEMPTS: usize = 16;
const MAX_ERROR_DETAIL_BYTES: usize = 512;

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectIoStage {
    InspectExisting,
    ReadTarget,
    CreateDirectory,
    CreateTemporary,
    WriteTemporary,
    SyncTemporary,
    ReadBackTemporary,
    CommitReplacement,
    RemoveTemporary,
    SpawnActor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExistingProjectTargetKind {
    NonV15,
    MalformedV15,
    NotDirectoryPackage,
    MissingProjectDocument,
    DestinationAppeared,
}

#[derive(Debug)]
pub(crate) enum ProjectPersistenceError {
    ActorQueueFull,
    ActorUnavailable,
    ActorThreadPanicked,
    DuplicateOperationToken,
    UnknownOperationToken,
    ProjectPathUnavailable,
    UnsafeSymlink,
    DocumentTooLarge {
        maximum: usize,
    },
    UnsupportedSchema,
    UnsupportedSchemaVersion,
    InvalidDocument {
        detail: String,
    },
    InvalidValue {
        detail: String,
    },
    ExistingTargetRejected {
        kind: ExistingProjectTargetKind,
    },
    ReadbackMismatch,
    PreCommitIo {
        stage: ProjectIoStage,
        kind: io::ErrorKind,
    },
    CommitIndeterminate {
        kind: io::ErrorKind,
    },
}

impl fmt::Display for ProjectPersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActorQueueFull => formatter.write_str("project persistence queue is full"),
            Self::ActorUnavailable => {
                formatter.write_str("project persistence actor is unavailable")
            }
            Self::ActorThreadPanicked => formatter.write_str("project persistence actor panicked"),
            Self::DuplicateOperationToken => {
                formatter.write_str("project operation token is already registered")
            }
            Self::UnknownOperationToken => {
                formatter.write_str("project operation token is not registered")
            }
            Self::ProjectPathUnavailable => formatter.write_str("project path has no parent"),
            Self::UnsafeSymlink => {
                formatter.write_str("project package path contains a symbolic link")
            }
            Self::DocumentTooLarge { maximum } => {
                write!(formatter, "project document exceeds {maximum} bytes")
            }
            Self::UnsupportedSchema => formatter.write_str("unsupported project schema"),
            Self::UnsupportedSchemaVersion => {
                formatter.write_str("unsupported project schema version")
            }
            Self::InvalidDocument { detail } => {
                write!(formatter, "invalid project document: {detail}")
            }
            Self::InvalidValue { detail } => write!(formatter, "invalid project value: {detail}"),
            Self::ExistingTargetRejected { kind } => {
                write!(
                    formatter,
                    "existing project target is not replaceable: {kind:?}"
                )
            }
            Self::ReadbackMismatch => formatter
                .write_str("project temporary readback did not match the submitted projection"),
            Self::PreCommitIo { stage, kind } => {
                write!(
                    formatter,
                    "project I/O failed before commit at {stage:?}: {kind:?}"
                )
            }
            Self::CommitIndeterminate { kind } => write!(
                formatter,
                "project replacement completed but directory durability is indeterminate: {kind:?}"
            ),
        }
    }
}

impl std::error::Error for ProjectPersistenceError {}

#[derive(Debug)]
pub(crate) enum ProjectPersistenceEvent {
    OpenCompleted {
        token: OperationToken,
        path: PathBuf,
        result: Result<Box<ProjectGenerationProjection>, ProjectPersistenceError>,
    },
    SaveCompleted {
        token: OperationToken,
        path: PathBuf,
        result: Result<ProjectRevisionId, ProjectPersistenceError>,
    },
    Cancelled {
        token: OperationToken,
    },
}

enum ProjectPersistenceRequest {
    Open {
        token: OperationToken,
        path: PathBuf,
        cancellation: Arc<AtomicBool>,
    },
    Save {
        token: OperationToken,
        path: PathBuf,
        projection: Arc<ProjectGenerationProjection>,
        cancellation: Arc<AtomicBool>,
    },
    Cancel {
        token: OperationToken,
    },
    Shutdown,
}

type CancellationRegistry = Arc<Mutex<Vec<(OperationToken, Arc<AtomicBool>)>>>;

pub(crate) struct CurrentProjectPersistenceBridge {
    requests: SyncSender<ProjectPersistenceRequest>,
    events: Option<Receiver<ProjectPersistenceEvent>>,
    cancellations: CancellationRegistry,
    worker: Option<JoinHandle<()>>,
}

impl CurrentProjectPersistenceBridge {
    pub(crate) fn spawn() -> Result<Self, ProjectPersistenceError> {
        let (requests, request_receiver) = mpsc::sync_channel(REQUEST_QUEUE_CAPACITY);
        let (event_sender, events) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
        let cancellations = Arc::new(Mutex::new(Vec::with_capacity(REQUEST_QUEUE_CAPACITY + 1)));
        let worker_cancellations = Arc::clone(&cancellations);
        let worker = thread::Builder::new()
            .name("mirante4d-project-v15".to_owned())
            .spawn(move || {
                run_actor(request_receiver, event_sender, worker_cancellations);
            })
            .map_err(|error| pre_commit_io(ProjectIoStage::SpawnActor, error))?;
        Ok(Self {
            requests,
            events: Some(events),
            cancellations,
            worker: Some(worker),
        })
    }

    pub(crate) fn request_open(
        &self,
        token: OperationToken,
        path: PathBuf,
    ) -> Result<(), ProjectPersistenceError> {
        let cancellation = self.register(&token)?;
        let request = ProjectPersistenceRequest::Open {
            token: token.clone(),
            path,
            cancellation,
        };
        if let Err(error) = self.requests.try_send(request) {
            self.unregister(&token);
            return Err(map_try_send_error(error));
        }
        Ok(())
    }

    pub(crate) fn request_save(
        &self,
        token: OperationToken,
        path: PathBuf,
        projection: Arc<ProjectGenerationProjection>,
    ) -> Result<(), ProjectPersistenceError> {
        let cancellation = self.register(&token)?;
        let request = ProjectPersistenceRequest::Save {
            token: token.clone(),
            path,
            projection,
            cancellation,
        };
        if let Err(error) = self.requests.try_send(request) {
            self.unregister(&token);
            return Err(map_try_send_error(error));
        }
        Ok(())
    }

    pub(crate) fn cancel(&self, token: OperationToken) -> Result<(), ProjectPersistenceError> {
        let cancellation = self
            .cancellations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|(candidate, _)| candidate == &token)
            .map(|(_, cancellation)| Arc::clone(cancellation))
            .ok_or(ProjectPersistenceError::UnknownOperationToken)?;
        cancellation.store(true, Ordering::Release);
        match self
            .requests
            .try_send(ProjectPersistenceRequest::Cancel { token })
        {
            Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
            Err(TrySendError::Disconnected(_)) => Err(ProjectPersistenceError::ActorUnavailable),
        }
    }

    pub(crate) fn try_recv(
        &self,
    ) -> Result<Option<ProjectPersistenceEvent>, ProjectPersistenceError> {
        let events = self
            .events
            .as_ref()
            .ok_or(ProjectPersistenceError::ActorUnavailable)?;
        match events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(ProjectPersistenceError::ActorUnavailable),
        }
    }

    pub(crate) fn shutdown(mut self) -> Result<(), ProjectPersistenceError> {
        cancel_all(&self.cancellations);
        // Releasing event backpressure lets a worker finish even if the UI did
        // not drain its last bounded event. Explicit shutdown is a composition-
        // root operation; Drop below never joins.
        let _ = self.events.take();
        let _ = self.requests.send(ProjectPersistenceRequest::Shutdown);
        match self.worker.take() {
            Some(worker) => worker
                .join()
                .map_err(|_| ProjectPersistenceError::ActorThreadPanicked),
            None => Ok(()),
        }
    }

    fn register(&self, token: &OperationToken) -> Result<Arc<AtomicBool>, ProjectPersistenceError> {
        let mut registry = self
            .cancellations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if registry.iter().any(|(candidate, _)| candidate == token) {
            return Err(ProjectPersistenceError::DuplicateOperationToken);
        }
        if registry.len() > REQUEST_QUEUE_CAPACITY {
            return Err(ProjectPersistenceError::ActorQueueFull);
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        registry.push((token.clone(), Arc::clone(&cancellation)));
        Ok(cancellation)
    }

    fn unregister(&self, token: &OperationToken) {
        unregister_token(&self.cancellations, token);
    }
}

impl Drop for CurrentProjectPersistenceBridge {
    fn drop(&mut self) {
        cancel_all(&self.cancellations);
        let _ = self.events.take();
        let _ = self.requests.try_send(ProjectPersistenceRequest::Shutdown);
        // Dropping JoinHandle detaches. A UI-thread Drop must not wait for I/O.
        let _ = self.worker.take();
    }
}

fn run_actor(
    requests: Receiver<ProjectPersistenceRequest>,
    events: SyncSender<ProjectPersistenceEvent>,
    cancellations: CancellationRegistry,
) {
    while let Ok(request) = requests.recv() {
        let (token, event) = match request {
            ProjectPersistenceRequest::Open {
                token,
                path,
                cancellation,
            } => {
                let event = match open_projection(&path, &cancellation) {
                    Ok(projection) => ProjectPersistenceEvent::OpenCompleted {
                        token: token.clone(),
                        path: path.clone(),
                        result: Ok(Box::new(projection)),
                    },
                    Err(WorkFailure::Cancelled) => ProjectPersistenceEvent::Cancelled {
                        token: token.clone(),
                    },
                    Err(WorkFailure::Error(error)) => ProjectPersistenceEvent::OpenCompleted {
                        token: token.clone(),
                        path: path.clone(),
                        result: Err(error),
                    },
                };
                (Some(token), Some(event))
            }
            ProjectPersistenceRequest::Save {
                token,
                path,
                projection,
                cancellation,
            } => {
                let revision = projection.revision();
                let event = match save_projection(&path, &projection, &cancellation) {
                    Ok(()) => ProjectPersistenceEvent::SaveCompleted {
                        token: token.clone(),
                        path: path.clone(),
                        result: Ok(revision),
                    },
                    Err(WorkFailure::Cancelled) => ProjectPersistenceEvent::Cancelled {
                        token: token.clone(),
                    },
                    Err(WorkFailure::Error(error)) => ProjectPersistenceEvent::SaveCompleted {
                        token: token.clone(),
                        path: path.clone(),
                        result: Err(error),
                    },
                };
                (Some(token), Some(event))
            }
            ProjectPersistenceRequest::Cancel { token } => {
                if let Some(flag) = cancellation_for(&cancellations, &token) {
                    flag.store(true, Ordering::Release);
                }
                (None, None)
            }
            ProjectPersistenceRequest::Shutdown => return,
        };
        if let Some(token) = token {
            unregister_token(&cancellations, &token);
        }
        if let Some(event) = event
            && events.send(event).is_err()
        {
            return;
        }
    }
}

fn cancellation_for(
    registry: &CancellationRegistry,
    token: &OperationToken,
) -> Option<Arc<AtomicBool>> {
    registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|(candidate, _)| candidate == token)
        .map(|(_, cancellation)| Arc::clone(cancellation))
}

fn unregister_token(registry: &CancellationRegistry, token: &OperationToken) {
    registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|(candidate, _)| candidate != token);
}

fn cancel_all(registry: &CancellationRegistry) {
    for (_, cancellation) in registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
    {
        cancellation.store(true, Ordering::Release);
    }
}

fn map_try_send_error(error: TrySendError<ProjectPersistenceRequest>) -> ProjectPersistenceError {
    match error {
        TrySendError::Full(_) => ProjectPersistenceError::ActorQueueFull,
        TrySendError::Disconnected(_) => ProjectPersistenceError::ActorUnavailable,
    }
}

#[derive(Debug)]
enum WorkFailure {
    Cancelled,
    Error(ProjectPersistenceError),
}

impl From<ProjectPersistenceError> for WorkFailure {
    fn from(error: ProjectPersistenceError) -> Self {
        Self::Error(error)
    }
}

type WorkResult<T> = Result<T, WorkFailure>;

fn check_cancel(cancellation: &AtomicBool) -> WorkResult<()> {
    if cancellation.load(Ordering::Acquire) {
        Err(WorkFailure::Cancelled)
    } else {
        Ok(())
    }
}

fn open_projection(
    package_path: &Path,
    cancellation: &AtomicBool,
) -> WorkResult<ProjectGenerationProjection> {
    let document_path = inspect_package(package_path, ProjectIoStage::ReadTarget)?
        .ok_or_else(|| pre_commit_io(ProjectIoStage::ReadTarget, io::ErrorKind::NotFound.into()))?;
    let bytes = read_bounded(&document_path, ProjectIoStage::ReadTarget, cancellation)?;
    let projection = decode_document(&bytes, cancellation)?;
    check_cancel(cancellation)?;
    Ok(projection)
}

fn save_projection(
    path: &Path,
    projection: &ProjectGenerationProjection,
    cancellation: &AtomicBool,
) -> WorkResult<()> {
    save_projection_with_hooks(
        path,
        projection,
        cancellation,
        |temporary, target| fs::rename(temporary, target),
        sync_directory,
    )
}

fn save_projection_with_hooks(
    package_path: &Path,
    projection: &ProjectGenerationProjection,
    cancellation: &AtomicBool,
    commit: impl FnOnce(&Path, &Path) -> io::Result<()>,
    sync_parent: impl FnOnce(&Path) -> io::Result<()>,
) -> WorkResult<()> {
    check_cancel(cancellation)?;
    let existing_document = validate_existing_package(package_path, cancellation)?;
    let parent = package_path
        .parent()
        .ok_or(ProjectPersistenceError::ProjectPathUnavailable)?;
    reject_symlink_components(package_path, ProjectIoStage::InspectExisting)?;
    fs::create_dir_all(parent)
        .map_err(|error| pre_commit_io(ProjectIoStage::CreateDirectory, error))?;
    reject_symlink_components(parent, ProjectIoStage::CreateDirectory)?;

    let encoded = encode_document(projection, cancellation)?;
    match existing_document {
        Some(document_path) => save_existing_package(
            package_path,
            &document_path,
            projection,
            cancellation,
            &encoded,
            commit,
            sync_parent,
        ),
        None => save_new_package(
            package_path,
            parent,
            projection,
            cancellation,
            &encoded,
            commit,
            sync_parent,
        ),
    }
}

fn save_existing_package(
    package_path: &Path,
    document_path: &Path,
    projection: &ProjectGenerationProjection,
    cancellation: &AtomicBool,
    encoded: &[u8],
    commit: impl FnOnce(&Path, &Path) -> io::Result<()>,
    sync_package: impl FnOnce(&Path) -> io::Result<()>,
) -> WorkResult<()> {
    let (temporary_path, temporary_file) = create_unique_temporary_file(document_path)?;
    let mut temporary = OwnedTemporary::file(temporary_path);
    let staged = (|| -> WorkResult<()> {
        write_sync_readback(
            temporary_file,
            temporary.path(),
            encoded,
            projection,
            cancellation,
        )?;
        reject_symlink_components(document_path, ProjectIoStage::CommitReplacement)?;
        commit(temporary.path(), document_path)
            .map_err(|error| pre_commit_io(ProjectIoStage::CommitReplacement, error))?;
        temporary.mark_committed();
        sync_package(package_path).map_err(commit_indeterminate)?;
        Ok(())
    })();
    finish_temporary(staged, &mut temporary)
}

fn save_new_package(
    package_path: &Path,
    parent: &Path,
    projection: &ProjectGenerationProjection,
    cancellation: &AtomicBool,
    encoded: &[u8],
    commit: impl FnOnce(&Path, &Path) -> io::Result<()>,
    sync_parent: impl FnOnce(&Path) -> io::Result<()>,
) -> WorkResult<()> {
    let temporary_path = create_unique_temporary_package(package_path)?;
    let mut temporary = OwnedTemporary::directory(temporary_path);
    let staged = (|| -> WorkResult<()> {
        let document_path = temporary.path().join(PROJECT_DOCUMENT_FILE_NAME);
        let document_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&document_path)
            .map_err(|error| pre_commit_io(ProjectIoStage::CreateTemporary, error))?;
        write_sync_readback(
            document_file,
            &document_path,
            encoded,
            projection,
            cancellation,
        )?;
        sync_directory(temporary.path())
            .map_err(|error| pre_commit_io(ProjectIoStage::SyncTemporary, error))?;
        check_cancel(cancellation)?;
        match fs::symlink_metadata(package_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ProjectPersistenceError::UnsafeSymlink.into());
            }
            Ok(_) => {
                return Err(ProjectPersistenceError::ExistingTargetRejected {
                    kind: ExistingProjectTargetKind::DestinationAppeared,
                }
                .into());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(pre_commit_io(ProjectIoStage::CommitReplacement, error).into());
            }
        }
        reject_symlink_components(package_path, ProjectIoStage::CommitReplacement)?;
        commit(temporary.path(), package_path)
            .map_err(|error| pre_commit_io(ProjectIoStage::CommitReplacement, error))?;
        temporary.mark_committed();
        sync_parent(parent).map_err(commit_indeterminate)?;
        Ok(())
    })();
    finish_temporary(staged, &mut temporary)
}

fn write_sync_readback(
    mut temporary_file: File,
    temporary_path: &Path,
    encoded: &[u8],
    projection: &ProjectGenerationProjection,
    cancellation: &AtomicBool,
) -> WorkResult<()> {
    write_cancellable(&mut temporary_file, encoded, cancellation)?;
    temporary_file
        .sync_all()
        .map_err(|error| pre_commit_io(ProjectIoStage::SyncTemporary, error))?;
    drop(temporary_file);
    let readback = read_bounded(
        temporary_path,
        ProjectIoStage::ReadBackTemporary,
        cancellation,
    )?;
    if decode_document(&readback, cancellation)? != *projection {
        return Err(ProjectPersistenceError::ReadbackMismatch.into());
    }
    // Once publication succeeds, cancellation can no longer truthfully be
    // reported, so this is the final cancellation point.
    check_cancel(cancellation)
}

fn finish_temporary(result: WorkResult<()>, temporary: &mut OwnedTemporary) -> WorkResult<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if temporary.is_owned() => match temporary.cleanup() {
            Ok(()) => Err(error),
            Err(cleanup) => Err(WorkFailure::Error(cleanup)),
        },
        Err(error) => Err(error),
    }
}

fn commit_indeterminate(error: io::Error) -> WorkFailure {
    WorkFailure::Error(ProjectPersistenceError::CommitIndeterminate { kind: error.kind() })
}

fn validate_existing_package(
    package_path: &Path,
    cancellation: &AtomicBool,
) -> WorkResult<Option<PathBuf>> {
    let Some(document_path) = inspect_package(package_path, ProjectIoStage::InspectExisting)?
    else {
        return Ok(None);
    };
    let bytes = read_bounded(
        &document_path,
        ProjectIoStage::InspectExisting,
        cancellation,
    )?;
    match decode_document(&bytes, cancellation) {
        Ok(_) => Ok(Some(document_path)),
        Err(WorkFailure::Cancelled) => Err(WorkFailure::Cancelled),
        Err(WorkFailure::Error(
            ProjectPersistenceError::UnsupportedSchema
            | ProjectPersistenceError::UnsupportedSchemaVersion,
        )) => Err(ProjectPersistenceError::ExistingTargetRejected {
            kind: ExistingProjectTargetKind::NonV15,
        }
        .into()),
        Err(WorkFailure::Error(_)) => Err(ProjectPersistenceError::ExistingTargetRejected {
            kind: ExistingProjectTargetKind::MalformedV15,
        }
        .into()),
    }
}

fn inspect_package(package_path: &Path, stage: ProjectIoStage) -> WorkResult<Option<PathBuf>> {
    reject_symlink_components(package_path, stage)?;
    let package_metadata = match fs::symlink_metadata(package_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(pre_commit_io(stage, error).into()),
    };
    if !package_metadata.is_dir() {
        return Err(ProjectPersistenceError::ExistingTargetRejected {
            kind: ExistingProjectTargetKind::NotDirectoryPackage,
        }
        .into());
    }
    let document_path = package_path.join(PROJECT_DOCUMENT_FILE_NAME);
    reject_symlink_components(&document_path, stage)?;
    let document_metadata = match fs::symlink_metadata(&document_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ProjectPersistenceError::ExistingTargetRejected {
                kind: ExistingProjectTargetKind::MissingProjectDocument,
            }
            .into());
        }
        Err(error) => return Err(pre_commit_io(stage, error).into()),
    };
    if !document_metadata.is_file() {
        return Err(ProjectPersistenceError::ExistingTargetRejected {
            kind: ExistingProjectTargetKind::MalformedV15,
        }
        .into());
    }
    Ok(Some(document_path))
}

fn reject_symlink_components(path: &Path, stage: ProjectIoStage) -> WorkResult<()> {
    for candidate in path
        .ancestors()
        .filter(|candidate| !candidate.as_os_str().is_empty())
    {
        match fs::symlink_metadata(candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ProjectPersistenceError::UnsafeSymlink.into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(pre_commit_io(stage, error).into()),
        }
    }
    Ok(())
}

fn encode_document(
    projection: &ProjectGenerationProjection,
    cancellation: &AtomicBool,
) -> WorkResult<Vec<u8>> {
    check_cancel(cancellation)?;
    let dto = ProjectDocumentDto::from_projection(projection, cancellation)?;
    let mut encoded =
        serde_json::to_vec_pretty(&dto).map_err(|error| invalid_document(error.to_string()))?;
    encoded.push(b'\n');
    ensure_document_size(encoded.len())?;
    check_cancel(cancellation)?;
    Ok(encoded)
}

fn decode_document(
    bytes: &[u8],
    cancellation: &AtomicBool,
) -> WorkResult<ProjectGenerationProjection> {
    ensure_document_size(bytes.len())?;
    check_cancel(cancellation)?;
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| invalid_document(error.to_string()))?;
    if value.get("schema").and_then(serde_json::Value::as_str) != Some(PROJECT_V15_SCHEMA) {
        return Err(ProjectPersistenceError::UnsupportedSchema.into());
    }
    if value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(u64::from(PROJECT_V15_SCHEMA_VERSION))
    {
        return Err(ProjectPersistenceError::UnsupportedSchemaVersion.into());
    }
    let dto: ProjectDocumentDto =
        serde_json::from_value(value).map_err(|error| invalid_document(error.to_string()))?;
    check_cancel(cancellation)?;
    dto.into_projection(cancellation)
}

fn read_bounded(
    path: &Path,
    stage: ProjectIoStage,
    cancellation: &AtomicBool,
) -> WorkResult<Vec<u8>> {
    let mut file = File::open(path).map_err(|error| pre_commit_io(stage, error))?;
    let mut bytes = Vec::with_capacity(READ_CHUNK_BYTES);
    let mut chunk = [0_u8; READ_CHUNK_BYTES];
    loop {
        check_cancel(cancellation)?;
        let read = file
            .read(&mut chunk)
            .map_err(|error| pre_commit_io(stage, error))?;
        if read == 0 {
            break;
        }
        if bytes.len().saturating_add(read) > MAX_PROJECT_DOCUMENT_BYTES {
            return Err(ProjectPersistenceError::DocumentTooLarge {
                maximum: MAX_PROJECT_DOCUMENT_BYTES,
            }
            .into());
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(bytes)
}

fn ensure_document_size(length: usize) -> WorkResult<()> {
    if length > MAX_PROJECT_DOCUMENT_BYTES {
        Err(ProjectPersistenceError::DocumentTooLarge {
            maximum: MAX_PROJECT_DOCUMENT_BYTES,
        }
        .into())
    } else {
        Ok(())
    }
}

fn write_cancellable(file: &mut File, bytes: &[u8], cancellation: &AtomicBool) -> WorkResult<()> {
    for chunk in bytes.chunks(READ_CHUNK_BYTES) {
        check_cancel(cancellation)?;
        file.write_all(chunk)
            .map_err(|error| pre_commit_io(ProjectIoStage::WriteTemporary, error))?;
    }
    Ok(())
}

fn temporary_candidate(path: &Path) -> Result<PathBuf, ProjectPersistenceError> {
    let parent = path
        .parent()
        .ok_or(ProjectPersistenceError::ProjectPathUnavailable)?;
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    Ok(parent.join(format!(".{file_name}.tmp-{timestamp:x}-{sequence:x}")))
}

fn create_unique_temporary_file(path: &Path) -> Result<(PathBuf, File), ProjectPersistenceError> {
    for _ in 0..TEMPORARY_CREATE_ATTEMPTS {
        let candidate = temporary_candidate(path)?;
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(pre_commit_io(ProjectIoStage::CreateTemporary, error));
            }
        }
    }
    Err(ProjectPersistenceError::PreCommitIo {
        stage: ProjectIoStage::CreateTemporary,
        kind: io::ErrorKind::AlreadyExists,
    })
}

fn create_unique_temporary_package(path: &Path) -> Result<PathBuf, ProjectPersistenceError> {
    for _ in 0..TEMPORARY_CREATE_ATTEMPTS {
        let candidate = temporary_candidate(path)?;
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(pre_commit_io(ProjectIoStage::CreateTemporary, error));
            }
        }
    }
    Err(ProjectPersistenceError::PreCommitIo {
        stage: ProjectIoStage::CreateTemporary,
        kind: io::ErrorKind::AlreadyExists,
    })
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

fn pre_commit_io(stage: ProjectIoStage, error: io::Error) -> ProjectPersistenceError {
    ProjectPersistenceError::PreCommitIo {
        stage,
        kind: error.kind(),
    }
}

fn invalid_document(detail: impl fmt::Display) -> WorkFailure {
    WorkFailure::Error(ProjectPersistenceError::InvalidDocument {
        detail: bounded_detail(detail),
    })
}

fn invalid_value(detail: impl fmt::Display) -> WorkFailure {
    WorkFailure::Error(ProjectPersistenceError::InvalidValue {
        detail: bounded_detail(detail),
    })
}

fn bounded_detail(detail: impl fmt::Display) -> String {
    let detail = detail.to_string();
    if detail.len() <= MAX_ERROR_DETAIL_BYTES {
        return detail;
    }
    let mut end = MAX_ERROR_DETAIL_BYTES;
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &detail[..end])
}

struct OwnedTemporary {
    path: PathBuf,
    kind: OwnedTemporaryKind,
    owned: bool,
}

#[derive(Clone, Copy)]
enum OwnedTemporaryKind {
    File,
    Directory,
}

impl OwnedTemporary {
    fn file(path: PathBuf) -> Self {
        Self {
            path,
            kind: OwnedTemporaryKind::File,
            owned: true,
        }
    }

    fn directory(path: PathBuf) -> Self {
        Self {
            path,
            kind: OwnedTemporaryKind::Directory,
            owned: true,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn is_owned(&self) -> bool {
        self.owned
    }

    fn mark_committed(&mut self) {
        self.owned = false;
    }

    fn cleanup(&mut self) -> Result<(), ProjectPersistenceError> {
        if self.owned {
            remove_temporary(&self.path, self.kind)?;
            self.owned = false;
        }
        Ok(())
    }
}

impl Drop for OwnedTemporary {
    fn drop(&mut self) {
        if self.owned {
            let _ = match self.kind {
                OwnedTemporaryKind::File => fs::remove_file(&self.path),
                OwnedTemporaryKind::Directory => fs::remove_dir_all(&self.path),
            };
        }
    }
}

fn remove_temporary(path: &Path, kind: OwnedTemporaryKind) -> Result<(), ProjectPersistenceError> {
    match kind {
        OwnedTemporaryKind::File => fs::remove_file(path),
        OwnedTemporaryKind::Directory => fs::remove_dir_all(path),
    }
    .map_err(|error| pre_commit_io(ProjectIoStage::RemoveTemporary, error))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectDocumentDto {
    schema: String,
    schema_version: u32,
    generation: ProjectGenerationDto,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectGenerationDto {
    revision: ProjectRevisionDto,
    revision_high_water: ProjectRevisionDto,
    project: ProjectStateDto,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectRevisionDto {
    project_id: String,
    sequence: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectStateDto {
    project_id: String,
    dataset: DatasetReferenceDto,
    view: ViewStateDto,
    channel_presets: Vec<ChannelPresetDto>,
    artifacts: Vec<ArtifactReferenceDto>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DatasetReferenceDto {
    scientific_content_id: String,
    package_id: Option<String>,
    release_id: Option<String>,
    locator_hint: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewStateDto {
    layers: Vec<LayerViewStateDto>,
    active_layer: u32,
    timepoint: u64,
    camera: CameraViewDto,
    layout: ViewerLayoutDto,
    cross_section: CrossSectionViewDto,
    iso_light: IsoLightDto,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LayerViewStateDto {
    layer: u32,
    visible: bool,
    transfer: LayerTransferDto,
    render: RenderStateDto,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LayerTransferDto {
    window: DisplayWindowDto,
    color_rgb: [f32; 3],
    opacity: f32,
    curve: TransferCurveDto,
    invert: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayWindowDto {
    low: f32,
    high: f32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum TransferCurveDto {
    Linear,
    Gamma { value: f32 },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum RenderStateDto {
    Mip {
        sampling: SamplingPolicyDto,
    },
    Isosurface {
        sampling: SamplingPolicyDto,
        shading: IsoShadingPolicyDto,
        display_level: f32,
    },
    Dvr {
        sampling: SamplingPolicyDto,
        opacity_transfer: DvrOpacityTransferDto,
        density_scale: f64,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SamplingPolicyDto {
    SmoothLinear,
    VoxelExact,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum IsoShadingPolicyDto {
    GradientLighting,
    Flat,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DvrOpacityTransferDto {
    window: DisplayWindowDto,
    curve: TransferCurveDto,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CameraViewDto {
    projection: ProjectionDto,
    target: [f64; 3],
    orientation_xyzw: [f64; 4],
    orthographic_world_per_screen_point: f64,
    perspective_focal_length_screen_points: f64,
    perspective_view_distance_world: f64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProjectionDto {
    Perspective,
    Orthographic,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ViewerLayoutDto {
    Single3d,
    FourPanel,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrossSectionViewDto {
    center: [f64; 3],
    orientation_xyzw: [f64; 4],
    scale_world_per_screen_point: f64,
    depth_world: f64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum IsoLightDto {
    AttachedCamera,
    DetachedScreen { x: f32, y: f32 },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelPresetDto {
    id: String,
    label: String,
    entries: Vec<ChannelPresetEntryDto>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelPresetEntryDto {
    layer: u32,
    visible: bool,
    transfer: LayerTransferDto,
    render: RenderStateDto,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactReferenceDto {
    handle_id: String,
    schema: ArtifactSchemaDto,
    content_id: String,
    object: RawObjectDescriptorDto,
    derivation_id: Option<String>,
    recipe_id: Option<String>,
    source_layers: Vec<u32>,
    label: String,
    visible: bool,
    completeness: ArtifactCompletenessDto,
    recoverability: ArtifactRecoverabilityDto,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ArtifactSchemaDto {
    RoiV1,
    TrackV1,
    AnnotationV1,
    MeasurementV1,
    AnalysisTableV1,
    AnalysisPlotV1,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawObjectDescriptorDto {
    digest: String,
    byte_length: u64,
    media_type: String,
    role: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArtifactCompletenessDto {
    Partial,
    Complete,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArtifactRecoverabilityDto {
    Regenerable,
    NonRegenerable,
}

impl ProjectDocumentDto {
    fn from_projection(
        projection: &ProjectGenerationProjection,
        cancellation: &AtomicBool,
    ) -> WorkResult<Self> {
        Ok(Self {
            schema: PROJECT_V15_SCHEMA.to_owned(),
            schema_version: PROJECT_V15_SCHEMA_VERSION,
            generation: ProjectGenerationDto::from_projection(projection, cancellation)?,
        })
    }

    fn into_projection(self, cancellation: &AtomicBool) -> WorkResult<ProjectGenerationProjection> {
        if self.schema != PROJECT_V15_SCHEMA {
            return Err(ProjectPersistenceError::UnsupportedSchema.into());
        }
        if self.schema_version != PROJECT_V15_SCHEMA_VERSION {
            return Err(ProjectPersistenceError::UnsupportedSchemaVersion.into());
        }
        self.generation.into_projection(cancellation)
    }
}

impl ProjectGenerationDto {
    fn from_projection(
        projection: &ProjectGenerationProjection,
        cancellation: &AtomicBool,
    ) -> WorkResult<Self> {
        check_cancel(cancellation)?;
        Ok(Self {
            revision: ProjectRevisionDto::from_revision(projection.revision()),
            revision_high_water: ProjectRevisionDto::from_high_water(
                projection.revision_high_water(),
            ),
            project: ProjectStateDto::from_state(projection.state(), cancellation)?,
        })
    }

    fn into_projection(self, cancellation: &AtomicBool) -> WorkResult<ProjectGenerationProjection> {
        check_cancel(cancellation)?;
        let revision = self.revision.into_revision()?;
        let high_water = self.revision_high_water.into_high_water()?;
        let state = self.project.into_state(cancellation)?;
        ProjectGenerationProjection::new(revision, high_water, state).map_err(invalid_value)
    }
}

impl ProjectRevisionDto {
    fn from_revision(revision: ProjectRevisionId) -> Self {
        Self {
            project_id: revision.project_id().to_string(),
            sequence: revision.sequence(),
        }
    }

    fn from_high_water(high_water: &ProjectRevisionHighWater) -> Self {
        Self {
            project_id: high_water.project_id().to_string(),
            sequence: high_water.sequence(),
        }
    }

    fn into_revision(self) -> WorkResult<ProjectRevisionId> {
        let project_id = ProjectId::parse(&self.project_id).map_err(invalid_value)?;
        Ok(ProjectRevisionId::new(project_id, self.sequence))
    }

    fn into_high_water(self) -> WorkResult<ProjectRevisionHighWater> {
        let project_id = ProjectId::parse(&self.project_id).map_err(invalid_value)?;
        Ok(ProjectRevisionHighWater::new(project_id, self.sequence))
    }
}

impl ProjectStateDto {
    fn from_state(state: &ProjectState, cancellation: &AtomicBool) -> WorkResult<Self> {
        let mut channel_presets = Vec::with_capacity(state.channel_presets().len());
        for preset in state.channel_presets() {
            check_cancel(cancellation)?;
            channel_presets.push(ChannelPresetDto::from_preset(preset, cancellation)?);
        }
        let mut artifacts = Vec::with_capacity(state.artifacts().len());
        for artifact in state.artifacts() {
            check_cancel(cancellation)?;
            artifacts.push(ArtifactReferenceDto::from_artifact(artifact));
        }
        Ok(Self {
            project_id: state.project_id().to_string(),
            dataset: DatasetReferenceDto::from_reference(state.dataset()),
            view: ViewStateDto::from_view(state.view(), cancellation)?,
            channel_presets,
            artifacts,
        })
    }

    fn into_state(self, cancellation: &AtomicBool) -> WorkResult<ProjectState> {
        let project_id = ProjectId::parse(&self.project_id).map_err(invalid_value)?;
        let dataset = self.dataset.into_reference()?;
        let view = self.view.into_view(cancellation)?;
        let mut channel_presets = Vec::with_capacity(self.channel_presets.len());
        for preset in self.channel_presets {
            check_cancel(cancellation)?;
            channel_presets.push(preset.into_preset(cancellation)?);
        }
        let mut artifacts = Vec::with_capacity(self.artifacts.len());
        for artifact in self.artifacts {
            check_cancel(cancellation)?;
            artifacts.push(artifact.into_artifact()?);
        }
        ProjectState::new(project_id, dataset, view, channel_presets, artifacts)
            .map_err(invalid_value)
    }
}

impl DatasetReferenceDto {
    fn from_reference(reference: &DatasetReference) -> Self {
        Self {
            scientific_content_id: reference.scientific_content_id().to_string(),
            package_id: reference.package_id().map(ToString::to_string),
            release_id: reference.release_id().map(ToString::to_string),
            locator_hint: reference
                .locator_hint()
                .map(|hint| hint.as_str().to_owned()),
        }
    }

    fn into_reference(self) -> WorkResult<DatasetReference> {
        let scientific_content_id =
            ScientificContentId::parse(&self.scientific_content_id).map_err(invalid_value)?;
        let package_id = self
            .package_id
            .map(|value| PackageId::parse(&value).map_err(invalid_value))
            .transpose()?;
        let release_id = self
            .release_id
            .map(|value| ReleaseId::parse(&value).map_err(invalid_value))
            .transpose()?;
        let locator_hint = self
            .locator_hint
            .map(|value| DatasetLocatorHint::new(value).map_err(invalid_value))
            .transpose()?;
        Ok(DatasetReference::new(
            scientific_content_id,
            package_id,
            release_id,
            locator_hint,
        ))
    }
}

impl ViewStateDto {
    fn from_view(view: &ViewState, cancellation: &AtomicBool) -> WorkResult<Self> {
        let mut layers = Vec::with_capacity(view.layers().len());
        for layer in view.layers() {
            check_cancel(cancellation)?;
            layers.push(LayerViewStateDto::from_layer(layer));
        }
        Ok(Self {
            layers,
            active_layer: view.active_layer().ordinal(),
            timepoint: view.timepoint().get(),
            camera: CameraViewDto::from_camera(*view.camera()),
            layout: ViewerLayoutDto::from_layout(view.layout()),
            cross_section: CrossSectionViewDto::from_view(*view.cross_section()),
            iso_light: IsoLightDto::from_state(*view.iso_light()),
        })
    }

    fn into_view(self, cancellation: &AtomicBool) -> WorkResult<ViewState> {
        let mut layers = Vec::with_capacity(self.layers.len());
        for layer in self.layers {
            check_cancel(cancellation)?;
            layers.push(layer.into_layer()?);
        }
        ViewState::new(
            layers,
            LogicalLayerKey::new(self.active_layer),
            TimeIndex::new(self.timepoint),
            self.camera.into_camera()?,
            self.layout.into_layout(),
            self.cross_section.into_view()?,
            self.iso_light.into_state()?,
        )
        .map_err(invalid_value)
    }
}

impl LayerViewStateDto {
    fn from_layer(layer: &LayerViewState) -> Self {
        Self {
            layer: layer.layer_key().ordinal(),
            visible: layer.visible(),
            transfer: LayerTransferDto::from_transfer(layer.transfer()),
            render: RenderStateDto::from_state(*layer.render_state()),
        }
    }

    fn into_layer(self) -> WorkResult<LayerViewState> {
        Ok(LayerViewState::new(
            LogicalLayerKey::new(self.layer),
            self.visible,
            self.transfer.into_transfer()?,
            self.render.into_state()?,
        ))
    }
}

impl LayerTransferDto {
    fn from_transfer(transfer: &LayerTransfer) -> Self {
        Self {
            window: DisplayWindowDto::from_window(transfer.window()),
            color_rgb: transfer.color().rgb(),
            opacity: transfer.opacity().get(),
            curve: TransferCurveDto::from_curve(transfer.curve()),
            invert: transfer.invert(),
        }
    }

    fn into_transfer(self) -> WorkResult<LayerTransfer> {
        Ok(LayerTransfer::new(
            self.window.into_window()?,
            RgbColor::new(self.color_rgb).map_err(invalid_value)?,
            Opacity::new(self.opacity).map_err(invalid_value)?,
            self.curve.into_curve()?,
            self.invert,
        ))
    }
}

impl DisplayWindowDto {
    fn from_window(window: DisplayWindow) -> Self {
        Self {
            low: window.low(),
            high: window.high(),
        }
    }

    fn into_window(self) -> WorkResult<DisplayWindow> {
        DisplayWindow::new(self.low, self.high).map_err(invalid_value)
    }
}

impl TransferCurveDto {
    fn from_curve(curve: TransferCurve) -> Self {
        if curve.is_linear() {
            Self::Linear
        } else {
            Self::Gamma {
                value: curve.gamma_value(),
            }
        }
    }

    fn into_curve(self) -> WorkResult<TransferCurve> {
        match self {
            Self::Linear => Ok(TransferCurve::linear()),
            Self::Gamma { value } => TransferCurve::gamma(value).map_err(invalid_value),
        }
    }
}

impl RenderStateDto {
    fn from_state(state: RenderState) -> Self {
        match state.mode() {
            RenderMode::Mip => Self::Mip {
                sampling: SamplingPolicyDto::from_policy(state.sampling_policy()),
            },
            RenderMode::Isosurface => {
                let parameters = state
                    .iso_parameters()
                    .expect("isosurface state exposes isosurface parameters");
                Self::Isosurface {
                    sampling: SamplingPolicyDto::from_policy(parameters.sampling_policy()),
                    shading: IsoShadingPolicyDto::from_policy(parameters.shading_policy()),
                    display_level: parameters.display_level(),
                }
            }
            RenderMode::Dvr => {
                let parameters = state
                    .dvr_parameters()
                    .expect("DVR state exposes DVR parameters");
                Self::Dvr {
                    sampling: SamplingPolicyDto::from_policy(parameters.sampling_policy()),
                    opacity_transfer: DvrOpacityTransferDto::from_transfer(
                        parameters.opacity_transfer(),
                    ),
                    density_scale: parameters.density_scale(),
                }
            }
        }
    }

    fn into_state(self) -> WorkResult<RenderState> {
        match self {
            Self::Mip { sampling } => Ok(RenderState::mip(sampling.into_policy())),
            Self::Isosurface {
                sampling,
                shading,
                display_level,
            } => RenderState::iso(sampling.into_policy(), shading.into_policy(), display_level)
                .map_err(invalid_value),
            Self::Dvr {
                sampling,
                opacity_transfer,
                density_scale,
            } => RenderState::dvr(
                sampling.into_policy(),
                opacity_transfer.into_transfer()?,
                density_scale,
            )
            .map_err(invalid_value),
        }
    }
}

impl SamplingPolicyDto {
    fn from_policy(policy: SamplingPolicy) -> Self {
        match policy {
            SamplingPolicy::SmoothLinear => Self::SmoothLinear,
            SamplingPolicy::VoxelExact => Self::VoxelExact,
        }
    }

    fn into_policy(self) -> SamplingPolicy {
        match self {
            Self::SmoothLinear => SamplingPolicy::SmoothLinear,
            Self::VoxelExact => SamplingPolicy::VoxelExact,
        }
    }
}

impl IsoShadingPolicyDto {
    fn from_policy(policy: IsoShadingPolicy) -> Self {
        match policy {
            IsoShadingPolicy::GradientLighting => Self::GradientLighting,
            IsoShadingPolicy::Flat => Self::Flat,
        }
    }

    fn into_policy(self) -> IsoShadingPolicy {
        match self {
            Self::GradientLighting => IsoShadingPolicy::GradientLighting,
            Self::Flat => IsoShadingPolicy::Flat,
        }
    }
}

impl DvrOpacityTransferDto {
    fn from_transfer(transfer: DvrOpacityTransfer) -> Self {
        Self {
            window: DisplayWindowDto::from_window(transfer.window()),
            curve: TransferCurveDto::from_curve(transfer.curve()),
        }
    }

    fn into_transfer(self) -> WorkResult<DvrOpacityTransfer> {
        Ok(DvrOpacityTransfer::new(
            self.window.into_window()?,
            self.curve.into_curve()?,
        ))
    }
}

impl CameraViewDto {
    fn from_camera(camera: CameraView) -> Self {
        Self {
            projection: ProjectionDto::from_projection(camera.projection()),
            target: camera.target().components(),
            orientation_xyzw: camera.orientation().xyzw(),
            orthographic_world_per_screen_point: camera.orthographic_world_per_screen_point(),
            perspective_focal_length_screen_points: camera.perspective_focal_length_screen_points(),
            perspective_view_distance_world: camera.perspective_view_distance_world(),
        }
    }

    fn into_camera(self) -> WorkResult<CameraView> {
        CameraView::new(
            self.projection.into_projection(),
            world_point(self.target)?,
            unit_quaternion(self.orientation_xyzw)?,
            self.orthographic_world_per_screen_point,
            self.perspective_focal_length_screen_points,
            self.perspective_view_distance_world,
        )
        .map_err(invalid_value)
    }
}

impl ProjectionDto {
    fn from_projection(projection: Projection) -> Self {
        match projection {
            Projection::Perspective => Self::Perspective,
            Projection::Orthographic => Self::Orthographic,
        }
    }

    fn into_projection(self) -> Projection {
        match self {
            Self::Perspective => Projection::Perspective,
            Self::Orthographic => Projection::Orthographic,
        }
    }
}

impl ViewerLayoutDto {
    fn from_layout(layout: ViewerLayout) -> Self {
        match layout {
            ViewerLayout::Single3d => Self::Single3d,
            ViewerLayout::FourPanel => Self::FourPanel,
        }
    }

    fn into_layout(self) -> ViewerLayout {
        match self {
            Self::Single3d => ViewerLayout::Single3d,
            Self::FourPanel => ViewerLayout::FourPanel,
        }
    }
}

impl CrossSectionViewDto {
    fn from_view(view: CrossSectionView) -> Self {
        Self {
            center: view.center_world().components(),
            orientation_xyzw: view.orientation().xyzw(),
            scale_world_per_screen_point: view.scale_world_per_screen_point(),
            depth_world: view.depth_world(),
        }
    }

    fn into_view(self) -> WorkResult<CrossSectionView> {
        CrossSectionView::new(
            world_point(self.center)?,
            unit_quaternion(self.orientation_xyzw)?,
            self.scale_world_per_screen_point,
            self.depth_world,
        )
        .map_err(invalid_value)
    }
}

impl IsoLightDto {
    fn from_state(state: IsoLightState) -> Self {
        match state.detached_screen_position() {
            Some([x, y]) => Self::DetachedScreen { x, y },
            None => Self::AttachedCamera,
        }
    }

    fn into_state(self) -> WorkResult<IsoLightState> {
        match self {
            Self::AttachedCamera => Ok(IsoLightState::attached_camera()),
            Self::DetachedScreen { x, y } => {
                IsoLightState::detached_screen(x, y).map_err(invalid_value)
            }
        }
    }
}

impl ChannelPresetDto {
    fn from_preset(preset: &ChannelPreset, cancellation: &AtomicBool) -> WorkResult<Self> {
        let mut entries = Vec::with_capacity(preset.entries().len());
        for entry in preset.entries() {
            check_cancel(cancellation)?;
            entries.push(ChannelPresetEntryDto::from_entry(entry));
        }
        Ok(Self {
            id: preset.id().as_str().to_owned(),
            label: preset.label().to_owned(),
            entries,
        })
    }

    fn into_preset(self, cancellation: &AtomicBool) -> WorkResult<ChannelPreset> {
        let id = ChannelPresetId::new(self.id).map_err(invalid_value)?;
        let mut entries = Vec::with_capacity(self.entries.len());
        for entry in self.entries {
            check_cancel(cancellation)?;
            entries.push(entry.into_entry()?);
        }
        ChannelPreset::new(id, self.label, entries).map_err(invalid_value)
    }
}

impl ChannelPresetEntryDto {
    fn from_entry(entry: &ChannelPresetEntry) -> Self {
        Self {
            layer: entry.layer_key().ordinal(),
            visible: entry.visible(),
            transfer: LayerTransferDto::from_transfer(entry.transfer()),
            render: RenderStateDto::from_state(*entry.render_state()),
        }
    }

    fn into_entry(self) -> WorkResult<ChannelPresetEntry> {
        Ok(ChannelPresetEntry::new(
            LogicalLayerKey::new(self.layer),
            self.visible,
            self.transfer.into_transfer()?,
            self.render.into_state()?,
        ))
    }
}

impl ArtifactReferenceDto {
    fn from_artifact(artifact: &ArtifactReference) -> Self {
        Self {
            handle_id: artifact.handle_id().to_string(),
            schema: ArtifactSchemaDto::from_schema(artifact.schema()),
            content_id: artifact.content_id().to_string(),
            object: RawObjectDescriptorDto::from_descriptor(artifact.object()),
            derivation_id: artifact.derivation_id().map(ToString::to_string),
            recipe_id: artifact.recipe_id().map(ToString::to_string),
            source_layers: artifact
                .source_layers()
                .iter()
                .map(|layer| layer.ordinal())
                .collect(),
            label: artifact.label().to_owned(),
            visible: artifact.visible(),
            completeness: ArtifactCompletenessDto::from_completeness(artifact.completeness()),
            recoverability: ArtifactRecoverabilityDto::from_recoverability(
                artifact.recoverability(),
            ),
        }
    }

    fn into_artifact(self) -> WorkResult<ArtifactReference> {
        let handle_id = ArtifactHandleId::parse(&self.handle_id).map_err(invalid_value)?;
        let content_id = ArtifactContentId::parse(&self.content_id).map_err(invalid_value)?;
        let derivation_id = self
            .derivation_id
            .map(|value| DerivationRecordId::parse(&value).map_err(invalid_value))
            .transpose()?;
        let recipe_id = self
            .recipe_id
            .map(|value| RecipeId::parse(&value).map_err(invalid_value))
            .transpose()?;
        ArtifactReference::new(
            handle_id,
            self.schema.into_schema(),
            content_id,
            self.object.into_descriptor()?,
            derivation_id,
            recipe_id,
            self.source_layers
                .into_iter()
                .map(LogicalLayerKey::new)
                .collect(),
            self.label,
            self.visible,
            self.completeness.into_completeness(),
            self.recoverability.into_recoverability(),
        )
        .map_err(invalid_value)
    }
}

impl ArtifactSchemaDto {
    fn from_schema(schema: ArtifactSchema) -> Self {
        match schema {
            ArtifactSchema::RoiV1 => Self::RoiV1,
            ArtifactSchema::TrackV1 => Self::TrackV1,
            ArtifactSchema::AnnotationV1 => Self::AnnotationV1,
            ArtifactSchema::MeasurementV1 => Self::MeasurementV1,
            ArtifactSchema::AnalysisTableV1 => Self::AnalysisTableV1,
            ArtifactSchema::AnalysisPlotV1 => Self::AnalysisPlotV1,
        }
    }

    fn into_schema(self) -> ArtifactSchema {
        match self {
            Self::RoiV1 => ArtifactSchema::RoiV1,
            Self::TrackV1 => ArtifactSchema::TrackV1,
            Self::AnnotationV1 => ArtifactSchema::AnnotationV1,
            Self::MeasurementV1 => ArtifactSchema::MeasurementV1,
            Self::AnalysisTableV1 => ArtifactSchema::AnalysisTableV1,
            Self::AnalysisPlotV1 => ArtifactSchema::AnalysisPlotV1,
        }
    }
}

impl RawObjectDescriptorDto {
    fn from_descriptor(descriptor: &RawObjectDescriptor) -> Self {
        Self {
            digest: descriptor.digest().to_string(),
            byte_length: descriptor.byte_length(),
            media_type: descriptor.media_type().as_str().to_owned(),
            role: descriptor.role().as_str().to_owned(),
        }
    }

    fn into_descriptor(self) -> WorkResult<RawObjectDescriptor> {
        Ok(RawObjectDescriptor::new(
            ExactBytesDigest::parse(&self.digest).map_err(invalid_value)?,
            self.byte_length,
            MediaType::parse(&self.media_type).map_err(invalid_value)?,
            ObjectRole::parse(&self.role).map_err(invalid_value)?,
        ))
    }
}

impl ArtifactCompletenessDto {
    fn from_completeness(completeness: ArtifactCompleteness) -> Self {
        match completeness {
            ArtifactCompleteness::Partial => Self::Partial,
            ArtifactCompleteness::Complete => Self::Complete,
        }
    }

    fn into_completeness(self) -> ArtifactCompleteness {
        match self {
            Self::Partial => ArtifactCompleteness::Partial,
            Self::Complete => ArtifactCompleteness::Complete,
        }
    }
}

impl ArtifactRecoverabilityDto {
    fn from_recoverability(recoverability: ArtifactRecoverability) -> Self {
        match recoverability {
            ArtifactRecoverability::Regenerable => Self::Regenerable,
            ArtifactRecoverability::NonRegenerable => Self::NonRegenerable,
        }
    }

    fn into_recoverability(self) -> ArtifactRecoverability {
        match self {
            Self::Regenerable => ArtifactRecoverability::Regenerable,
            Self::NonRegenerable => ArtifactRecoverability::NonRegenerable,
        }
    }
}

fn world_point(components: [f64; 3]) -> WorkResult<WorldPoint3> {
    WorldPoint3::new(components[0], components[1], components[2]).map_err(invalid_value)
}

fn unit_quaternion(components: [f64; 4]) -> WorkResult<UnitQuaternion> {
    UnitQuaternion::new_xyzw(components[0], components[1], components[2], components[3])
        .map_err(invalid_value)
}

#[cfg(test)]
mod tests {
    use std::{
        fs, io,
        sync::{Arc, atomic::AtomicBool},
        thread,
        time::{Duration, Instant},
    };

    use mirante4d_application::{
        ApplicationCommand, ApplicationEvent, ApplicationState, MAX_PENDING_EVENTS, OperationKind,
        OperationToken, SourceSessionGeneration, UnboundWorkspace,
    };
    use mirante4d_dataset::{DatasetCatalog, DatasetLayer, ScientificIdentityStatus};
    use mirante4d_domain::{GridToWorld, IntensityDType, IsoShadingPolicy, Shape4D};
    use mirante4d_settings::ResourcePolicy;
    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::*;

    const PROJECT_ID: &str = "00000000-0000-4000-8000-000000000001";
    const ARTIFACT_ID: &str = "00000000-0000-4000-8000-000000000002";

    fn identity<T>(prefix: &str, digit: char, parse: impl FnOnce(&str) -> T) -> T {
        parse(&format!("{prefix}{}", digit.to_string().repeat(64)))
    }

    fn project_id() -> ProjectId {
        ProjectId::parse(PROJECT_ID).unwrap()
    }

    fn scientific_id(digit: char) -> ScientificContentId {
        identity(ScientificContentId::PREFIX, digit, |value| {
            ScientificContentId::parse(value).unwrap()
        })
    }

    fn transfer() -> LayerTransfer {
        LayerTransfer::new(
            DisplayWindow::new(0.0, 1.0).unwrap(),
            RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
            Opacity::new(1.0).unwrap(),
            TransferCurve::linear(),
            false,
        )
    }

    fn layer(render_state: RenderState) -> LayerViewState {
        LayerViewState::new(LogicalLayerKey::new(7), true, transfer(), render_state)
    }

    fn camera() -> CameraView {
        CameraView::new(
            Projection::Orthographic,
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            1.0,
            320.0,
            10.0,
        )
        .unwrap()
    }

    fn cross_section() -> CrossSectionView {
        CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 1.0, 1.0).unwrap()
    }

    fn view(render_state: RenderState) -> ViewState {
        ViewState::new(
            vec![layer(render_state)],
            LogicalLayerKey::new(7),
            TimeIndex::new(2),
            camera(),
            ViewerLayout::Single3d,
            cross_section(),
            IsoLightState::attached_camera(),
        )
        .unwrap()
    }

    fn minimal_projection(sequence: u64) -> ProjectGenerationProjection {
        let id = project_id();
        let state = ProjectState::new(
            id,
            DatasetReference::new(scientific_id('0'), None, None, None),
            view(RenderState::mip(SamplingPolicy::SmoothLinear)),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        ProjectGenerationProjection::new(
            ProjectRevisionId::new(id, sequence),
            ProjectRevisionHighWater::new(id, sequence + 4),
            state,
        )
        .unwrap()
    }

    fn full_projection() -> ProjectGenerationProjection {
        let id = project_id();
        let render = RenderState::dvr(
            SamplingPolicy::VoxelExact,
            DvrOpacityTransfer::new(
                DisplayWindow::new(0.1, 0.9).unwrap(),
                TransferCurve::gamma(0.5).unwrap(),
            ),
            12.0,
        )
        .unwrap();
        let preset = ChannelPreset::new(
            ChannelPresetId::new("preset_a").unwrap(),
            "Preset A",
            vec![ChannelPresetEntry::new(
                LogicalLayerKey::new(7),
                false,
                LayerTransfer::new(
                    DisplayWindow::new(2.0, 8.0).unwrap(),
                    RgbColor::new([0.25, 0.5, 0.75]).unwrap(),
                    Opacity::new(0.8).unwrap(),
                    TransferCurve::gamma(2.0).unwrap(),
                    true,
                ),
                RenderState::iso(
                    SamplingPolicy::SmoothLinear,
                    IsoShadingPolicy::GradientLighting,
                    0.4,
                )
                .unwrap(),
            )],
        )
        .unwrap();
        let schema = ArtifactSchema::AnalysisTableV1;
        let artifact = ArtifactReference::new(
            ArtifactHandleId::parse(ARTIFACT_ID).unwrap(),
            schema,
            identity(ArtifactContentId::PREFIX, '3', |value| {
                ArtifactContentId::parse(value).unwrap()
            }),
            RawObjectDescriptor::new(
                identity(ExactBytesDigest::PREFIX, '4', |value| {
                    ExactBytesDigest::parse(value).unwrap()
                }),
                1_234,
                MediaType::parse(schema.media_type()).unwrap(),
                ObjectRole::parse(schema.object_role()).unwrap(),
            ),
            Some(identity(DerivationRecordId::PREFIX, '5', |value| {
                DerivationRecordId::parse(value).unwrap()
            })),
            Some(identity(RecipeId::PREFIX, '6', |value| {
                RecipeId::parse(value).unwrap()
            })),
            vec![LogicalLayerKey::new(7)],
            "Table A",
            true,
            ArtifactCompleteness::Complete,
            ArtifactRecoverability::Regenerable,
        )
        .unwrap();
        let state = ProjectState::new(
            id,
            DatasetReference::new(
                scientific_id('0'),
                Some(identity(PackageId::PREFIX, '1', |value| {
                    PackageId::parse(value).unwrap()
                })),
                Some(identity(ReleaseId::PREFIX, '2', |value| {
                    ReleaseId::parse(value).unwrap()
                })),
                Some(DatasetLocatorHint::new("fixtures/bootstrap.m4d").unwrap()),
            ),
            ViewState::new(
                vec![layer(render)],
                LogicalLayerKey::new(7),
                TimeIndex::new(2),
                CameraView::new(
                    Projection::Perspective,
                    WorldPoint3::new(1.0, 2.0, 3.0).unwrap(),
                    UnitQuaternion::identity(),
                    0.25,
                    320.0,
                    40.0,
                )
                .unwrap(),
                ViewerLayout::FourPanel,
                CrossSectionView::new(
                    WorldPoint3::new(4.0, 5.0, 6.0).unwrap(),
                    UnitQuaternion::identity(),
                    0.5,
                    2.0,
                )
                .unwrap(),
                IsoLightState::detached_screen(0.25, -0.5).unwrap(),
            )
            .unwrap(),
            vec![preset],
            vec![artifact],
        )
        .unwrap();
        ProjectGenerationProjection::new(
            ProjectRevisionId::new(id, 3),
            ProjectRevisionHighWater::new(id, 7),
            state,
        )
        .unwrap()
    }

    fn uncancelled() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn error<T>(result: WorkResult<T>) -> ProjectPersistenceError {
        match result {
            Err(WorkFailure::Error(error)) => error,
            Err(WorkFailure::Cancelled) => panic!("unexpected cancellation"),
            Ok(_) => panic!("expected project persistence error"),
        }
    }

    fn assert_cancelled<T>(result: WorkResult<T>) {
        assert!(matches!(result, Err(WorkFailure::Cancelled)));
    }

    fn encode(projection: &ProjectGenerationProjection) -> Vec<u8> {
        encode_document(projection, &uncancelled()).unwrap()
    }

    fn project_document(package_path: &Path) -> PathBuf {
        package_path.join(PROJECT_DOCUMENT_FILE_NAME)
    }

    fn create_package(package_path: &Path, document: &[u8]) {
        fs::create_dir(package_path).unwrap();
        fs::write(project_document(package_path), document).unwrap();
    }

    fn temporary_entries(path: &Path) -> Vec<PathBuf> {
        let mut pending = vec![path.to_owned()];
        let mut temporary = Vec::new();
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains(".tmp-")
                {
                    temporary.push(path);
                } else if path.is_dir() {
                    pending.push(path);
                }
            }
        }
        temporary
    }

    fn operation_tokens(count: usize) -> Vec<OperationToken> {
        let workspace = UnboundWorkspace::new(
            project_id(),
            view(RenderState::mip(SamplingPolicy::SmoothLinear)),
            Vec::new(),
        )
        .unwrap();
        let catalog = DatasetCatalog::new(
            "bridge-test",
            ScientificIdentityStatus::Unverified,
            vec![
                DatasetLayer::new(
                    LogicalLayerKey::new(7),
                    "layer-7",
                    Shape4D::new(3, 2, 3, 4).unwrap(),
                    IntensityDType::Uint16,
                    GridToWorld::identity(),
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let mut application = ApplicationState::new_unbound(
            SourceSessionGeneration::new(1),
            catalog,
            workspace,
            ResourcePolicy::default(),
        )
        .unwrap();
        for _ in 0..count {
            application
                .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
                .unwrap();
        }
        application
            .drain_events(MAX_PENDING_EVENTS)
            .into_iter()
            .filter_map(|event| match event {
                ApplicationEvent::OperationStarted { token } => Some(token),
                _ => None,
            })
            .collect()
    }

    fn wait_for_event(bridge: &CurrentProjectPersistenceBridge) -> ProjectPersistenceEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(event) = bridge.try_recv().unwrap() {
                return event;
            }
            assert!(Instant::now() < deadline, "project actor event timed out");
            thread::yield_now();
        }
    }

    #[test]
    fn v15_json_is_exact_and_uses_fixed_typed_identities() {
        let encoded = encode(&minimal_projection(3));
        let byte_fingerprint = encoded.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
        assert_eq!(
            (encoded.len(), byte_fingerprint),
            (2_189, 0xdaeff4c7ddcf34df)
        );
        let actual: Value = serde_json::from_slice(&encoded).unwrap();
        let digest = format!("{}{}", ScientificContentId::PREFIX, "0".repeat(64));
        assert_eq!(
            actual,
            json!({
                "schema": "mirante4d-project-v15",
                "schema_version": 1,
                "generation": {
                    "revision": { "project_id": PROJECT_ID, "sequence": 3 },
                    "revision_high_water": { "project_id": PROJECT_ID, "sequence": 7 },
                    "project": {
                        "project_id": PROJECT_ID,
                        "dataset": {
                            "scientific_content_id": digest,
                            "package_id": null,
                            "release_id": null,
                            "locator_hint": null
                        },
                        "view": {
                            "layers": [{
                                "layer": 7,
                                "visible": true,
                                "transfer": {
                                    "window": { "low": 0.0, "high": 1.0 },
                                    "color_rgb": [1.0, 1.0, 1.0],
                                    "opacity": 1.0,
                                    "curve": { "kind": "linear" },
                                    "invert": false
                                },
                                "render": { "mode": "mip", "sampling": "smooth_linear" }
                            }],
                            "active_layer": 7,
                            "timepoint": 2,
                            "camera": {
                                "projection": "orthographic",
                                "target": [0.0, 0.0, 0.0],
                                "orientation_xyzw": [0.0, 0.0, 0.0, 1.0],
                                "orthographic_world_per_screen_point": 1.0,
                                "perspective_focal_length_screen_points": 320.0,
                                "perspective_view_distance_world": 10.0
                            },
                            "layout": "single3d",
                            "cross_section": {
                                "center": [0.0, 0.0, 0.0],
                                "orientation_xyzw": [0.0, 0.0, 0.0, 1.0],
                                "scale_world_per_screen_point": 1.0,
                                "depth_world": 1.0
                            },
                            "iso_light": { "kind": "attached_camera" }
                        },
                        "channel_presets": [],
                        "artifacts": []
                    }
                }
            })
        );
        assert!(encoded.ends_with(b"\n"));
    }

    #[test]
    fn full_projection_round_trips_through_only_validated_values() {
        let expected = full_projection();
        let decoded = decode_document(&encode(&expected), &uncancelled()).unwrap();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn closed_v15_document_rejects_unknown_forbidden_and_invalid_values() {
        let base: Value = serde_json::from_slice(&encode(&full_projection())).unwrap();
        let mut top_unknown = base.clone();
        top_unknown["unknown"] = json!(true);
        assert!(matches!(
            error(decode_document(
                &serde_json::to_vec(&top_unknown).unwrap(),
                &uncancelled()
            )),
            ProjectPersistenceError::InvalidDocument { .. }
        ));

        for forbidden in [
            "runtime",
            "gpu",
            "workers",
            "errors",
            "diagnostics",
            "dataset_path",
            "active_layer_index",
            "history",
            "autosave",
            "recovery",
        ] {
            let mut document = base.clone();
            document["generation"]["project"][forbidden] = json!("forbidden");
            assert!(matches!(
                error(decode_document(
                    &serde_json::to_vec(&document).unwrap(),
                    &uncancelled()
                )),
                ProjectPersistenceError::InvalidDocument { .. }
            ));
        }

        let mut payload = base.clone();
        payload["generation"]["project"]["artifacts"][0]["payload"] = json!([1, 2, 3]);
        assert!(matches!(
            error(decode_document(
                &serde_json::to_vec(&payload).unwrap(),
                &uncancelled()
            )),
            ProjectPersistenceError::InvalidDocument { .. }
        ));

        let mut invalid_window = base;
        invalid_window["generation"]["project"]["view"]["layers"][0]["transfer"]["window"] =
            json!({ "low": 9.0, "high": 1.0 });
        assert!(matches!(
            error(decode_document(
                &serde_json::to_vec(&invalid_window).unwrap(),
                &uncancelled()
            )),
            ProjectPersistenceError::InvalidValue { .. }
        ));
    }

    #[test]
    fn malformed_schema_and_versions_are_rejected_without_fallback() {
        assert!(matches!(
            error(decode_document(b"not-json", &uncancelled())),
            ProjectPersistenceError::InvalidDocument { .. }
        ));
        assert!(matches!(
            error(decode_document(b"{}", &uncancelled())),
            ProjectPersistenceError::UnsupportedSchema
        ));
        let wrong_version = json!({
            "schema": PROJECT_V15_SCHEMA,
            "schema_version": 2,
            "generation": {}
        });
        assert!(matches!(
            error(decode_document(
                &serde_json::to_vec(&wrong_version).unwrap(),
                &uncancelled()
            )),
            ProjectPersistenceError::UnsupportedSchemaVersion
        ));
    }

    #[test]
    fn unsupported_predecessor_document_is_never_opened_or_replaced_and_remains_byte_exact() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("legacy.m4dproj");
        let legacy = br#"{"format":"mirante4d-project-v14","dataset":{}}"#;
        create_package(&path, legacy);

        assert!(matches!(
            error(open_projection(&path, &uncancelled())),
            ProjectPersistenceError::UnsupportedSchema
        ));
        assert!(matches!(
            error(save_projection(
                &path,
                &minimal_projection(0),
                &uncancelled()
            )),
            ProjectPersistenceError::ExistingTargetRejected {
                kind: ExistingProjectTargetKind::NonV15
            }
        ));
        assert_eq!(fs::read(project_document(&path)).unwrap(), legacy);
        assert!(temporary_entries(directory.path()).is_empty());
    }

    #[test]
    fn malformed_and_non_v15_existing_targets_are_preserved() {
        let directory = tempdir().unwrap();
        for (name, bytes, expected_kind) in [
            (
                "malformed.m4dproj",
                br#"{"schema":"mirante4d-project-v15","schema_version":1}"#.as_slice(),
                ExistingProjectTargetKind::MalformedV15,
            ),
            (
                "other.m4dproj",
                br#"{"schema":"some-other-schema","schema_version":1}"#.as_slice(),
                ExistingProjectTargetKind::NonV15,
            ),
        ] {
            let path = directory.path().join(name);
            create_package(&path, bytes);
            assert!(matches!(
                error(save_projection(
                    &path,
                    &minimal_projection(0),
                    &uncancelled()
                )),
                ProjectPersistenceError::ExistingTargetRejected { kind } if kind == expected_kind
            ));
            assert_eq!(fs::read(project_document(&path)).unwrap(), bytes);
        }
        assert!(temporary_entries(directory.path()).is_empty());
    }

    #[test]
    fn file_and_incomplete_directory_targets_are_refused_unchanged() {
        let directory = tempdir().unwrap();
        let file_path = directory.path().join("file.m4dproj");
        let file_sentinel = b"not a package";
        fs::write(&file_path, file_sentinel).unwrap();
        assert!(matches!(
            error(save_projection(
                &file_path,
                &minimal_projection(0),
                &uncancelled()
            )),
            ProjectPersistenceError::ExistingTargetRejected {
                kind: ExistingProjectTargetKind::NotDirectoryPackage
            }
        ));
        assert_eq!(fs::read(file_path).unwrap(), file_sentinel);

        let incomplete_path = directory.path().join("incomplete.m4dproj");
        fs::create_dir(&incomplete_path).unwrap();
        let sentinel_path = incomplete_path.join("keep-me");
        fs::write(&sentinel_path, b"sentinel").unwrap();
        assert!(matches!(
            error(save_projection(
                &incomplete_path,
                &minimal_projection(0),
                &uncancelled()
            )),
            ProjectPersistenceError::ExistingTargetRejected {
                kind: ExistingProjectTargetKind::MissingProjectDocument
            }
        ));
        assert_eq!(fs::read(sentinel_path).unwrap(), b"sentinel");
        assert!(temporary_entries(directory.path()).is_empty());
    }

    #[test]
    fn new_package_commit_failure_never_publishes_an_incomplete_destination() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("new.m4dproj");
        let projection = minimal_projection(1);
        let failure = error(save_projection_with_hooks(
            &path,
            &projection,
            &uncancelled(),
            |temporary, destination| {
                assert!(temporary.is_dir());
                assert!(!destination.exists());
                assert_eq!(
                    decode_document(
                        &fs::read(project_document(temporary)).unwrap(),
                        &uncancelled()
                    )
                    .unwrap(),
                    projection
                );
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "failpoint"))
            },
            |_| Ok(()),
        ));
        assert!(matches!(
            failure,
            ProjectPersistenceError::PreCommitIo {
                stage: ProjectIoStage::CommitReplacement,
                kind: io::ErrorKind::PermissionDenied
            }
        ));
        assert!(!path.exists());
        assert!(temporary_entries(directory.path()).is_empty());
    }

    #[test]
    fn new_package_parent_sync_failure_is_commit_indeterminate() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("new.m4dproj");
        let projection = minimal_projection(1);
        let failure = error(save_projection_with_hooks(
            &path,
            &projection,
            &uncancelled(),
            |temporary, destination| fs::rename(temporary, destination),
            |_| Err(io::Error::other("failpoint")),
        ));
        assert!(matches!(
            failure,
            ProjectPersistenceError::CommitIndeterminate {
                kind: io::ErrorKind::Other
            }
        ));
        assert_eq!(open_projection(&path, &uncancelled()).unwrap(), projection);
        assert!(temporary_entries(directory.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_package_document_and_parent_are_rejected_without_touching_outside_data() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let outside_package = directory.path().join("outside.m4dproj");
        let outside_bytes = encode(&minimal_projection(0));
        create_package(&outside_package, &outside_bytes);
        let linked_package = directory.path().join("linked.m4dproj");
        symlink(&outside_package, &linked_package).unwrap();
        assert!(matches!(
            error(open_projection(&linked_package, &uncancelled())),
            ProjectPersistenceError::UnsafeSymlink
        ));
        assert!(matches!(
            error(save_projection(
                &linked_package,
                &minimal_projection(1),
                &uncancelled()
            )),
            ProjectPersistenceError::UnsafeSymlink
        ));
        assert_eq!(
            fs::read(project_document(&outside_package)).unwrap(),
            outside_bytes
        );

        let linked_document_package = directory.path().join("linked-document.m4dproj");
        fs::create_dir(&linked_document_package).unwrap();
        let outside_document = directory.path().join("outside-project.json");
        let document_sentinel = b"outside sentinel";
        fs::write(&outside_document, document_sentinel).unwrap();
        symlink(
            &outside_document,
            project_document(&linked_document_package),
        )
        .unwrap();
        assert!(matches!(
            error(save_projection(
                &linked_document_package,
                &minimal_projection(1),
                &uncancelled()
            )),
            ProjectPersistenceError::UnsafeSymlink
        ));
        assert_eq!(fs::read(outside_document).unwrap(), document_sentinel);

        let real_parent = directory.path().join("real-parent");
        fs::create_dir(&real_parent).unwrap();
        let linked_parent = directory.path().join("linked-parent");
        symlink(&real_parent, &linked_parent).unwrap();
        assert!(matches!(
            error(save_projection(
                &linked_parent.join("new.m4dproj"),
                &minimal_projection(1),
                &uncancelled()
            )),
            ProjectPersistenceError::UnsafeSymlink
        ));
        assert!(!real_parent.join("new.m4dproj").exists());
        assert!(temporary_entries(directory.path()).is_empty());
    }

    #[test]
    fn pre_commit_failure_preserves_old_v15_and_removes_owned_temporary() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("project.m4dproj");
        let old = encode(&minimal_projection(0));
        create_package(&path, &old);
        let error = error(save_projection_with_hooks(
            &path,
            &minimal_projection(1),
            &uncancelled(),
            |temporary, target| {
                assert_eq!(temporary.parent(), target.parent());
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "failpoint"))
            },
            |_| Ok(()),
        ));
        assert!(matches!(
            error,
            ProjectPersistenceError::PreCommitIo {
                stage: ProjectIoStage::CommitReplacement,
                kind: io::ErrorKind::PermissionDenied
            }
        ));
        assert_eq!(fs::read(project_document(&path)).unwrap(), old);
        assert!(temporary_entries(directory.path()).is_empty());
    }

    #[test]
    fn post_rename_directory_sync_failure_is_commit_indeterminate() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("project.m4dproj");
        create_package(&path, &encode(&minimal_projection(0)));
        let package_sentinel = path.join("keep-me");
        fs::write(&package_sentinel, b"unchanged").unwrap();
        let replacement = minimal_projection(1);
        let error = error(save_projection_with_hooks(
            &path,
            &replacement,
            &uncancelled(),
            |temporary, destination| fs::rename(temporary, destination),
            |_| Err(io::Error::other("failpoint")),
        ));
        assert!(matches!(
            error,
            ProjectPersistenceError::CommitIndeterminate {
                kind: io::ErrorKind::Other
            }
        ));
        assert_eq!(
            decode_document(&fs::read(project_document(&path)).unwrap(), &uncancelled()).unwrap(),
            replacement
        );
        assert_eq!(fs::read(package_sentinel).unwrap(), b"unchanged");
        assert!(temporary_entries(directory.path()).is_empty());
    }

    #[test]
    fn document_and_actor_inputs_are_bounded() {
        assert!(ensure_document_size(MAX_PROJECT_DOCUMENT_BYTES).is_ok());
        assert!(matches!(
            error(ensure_document_size(MAX_PROJECT_DOCUMENT_BYTES + 1)),
            ProjectPersistenceError::DocumentTooLarge {
                maximum: MAX_PROJECT_DOCUMENT_BYTES
            }
        ));

        let (requests, queued_requests) = mpsc::sync_channel(REQUEST_QUEUE_CAPACITY);
        let (event_sender, events) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
        let bridge = CurrentProjectPersistenceBridge {
            requests,
            events: Some(events),
            cancellations: Arc::new(Mutex::new(Vec::new())),
            worker: None,
        };
        let tokens = operation_tokens(REQUEST_QUEUE_CAPACITY + 1);
        bridge
            .request_open(tokens[0].clone(), PathBuf::from("not-consumed.m4dproj"))
            .unwrap();
        assert!(matches!(
            bridge.request_open(tokens[0].clone(), PathBuf::from("not-consumed.m4dproj")),
            Err(ProjectPersistenceError::DuplicateOperationToken)
        ));
        for token in tokens.iter().take(REQUEST_QUEUE_CAPACITY).skip(1) {
            bridge
                .request_open(token.clone(), PathBuf::from("not-consumed.m4dproj"))
                .unwrap();
        }
        assert!(matches!(
            bridge.request_open(
                tokens[REQUEST_QUEUE_CAPACITY].clone(),
                PathBuf::from("not-consumed.m4dproj")
            ),
            Err(ProjectPersistenceError::ActorQueueFull)
        ));
        assert_eq!(
            bridge
                .cancellations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            REQUEST_QUEUE_CAPACITY
        );
        drop(bridge);
        drop(event_sender);
        assert_eq!(queued_requests.try_iter().count(), REQUEST_QUEUE_CAPACITY);
    }

    #[test]
    fn cancellation_stops_open_and_save_before_visible_commit() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("project.m4dproj");
        let old = encode(&minimal_projection(0));
        create_package(&path, &old);
        let cancelled = AtomicBool::new(true);

        assert_cancelled(open_projection(&path, &cancelled));
        assert_cancelled(save_projection(&path, &minimal_projection(1), &cancelled));
        assert_eq!(fs::read(project_document(&path)).unwrap(), old);
        assert!(temporary_entries(directory.path()).is_empty());
    }

    #[test]
    fn actor_preserves_exact_tokens_for_open_save_and_cancel() {
        let directory = tempdir().unwrap();
        let open_path = directory.path().join("open.m4dproj");
        let save_path = directory.path().join("save.m4dproj");
        let projection = full_projection();
        create_package(&open_path, &encode(&projection));
        let mut tokens = operation_tokens(3).into_iter();
        let open_token = tokens.next().unwrap();
        let save_token = tokens.next().unwrap();
        let cancel_token = tokens.next().unwrap();
        let bridge = CurrentProjectPersistenceBridge::spawn().unwrap();

        bridge.request_open(open_token.clone(), open_path).unwrap();
        match wait_for_event(&bridge) {
            ProjectPersistenceEvent::OpenCompleted { token, result, .. } => {
                assert_eq!(token, open_token);
                assert_eq!(*result.unwrap(), projection);
            }
            event => panic!("unexpected event: {event:?}"),
        }

        bridge
            .request_save(
                save_token.clone(),
                save_path.clone(),
                Arc::new(minimal_projection(5)),
            )
            .unwrap();
        match wait_for_event(&bridge) {
            ProjectPersistenceEvent::SaveCompleted { token, result, .. } => {
                assert_eq!(token, save_token);
                assert_eq!(result.unwrap().sequence(), 5);
            }
            event => panic!("unexpected event: {event:?}"),
        }
        assert_eq!(
            decode_document(
                &fs::read(project_document(&save_path)).unwrap(),
                &uncancelled()
            )
            .unwrap()
            .revision()
            .sequence(),
            5
        );
        assert_eq!(fs::read_dir(&save_path).unwrap().count(), 1);

        let cancellation = bridge.register(&cancel_token).unwrap();
        cancellation.store(true, Ordering::Release);
        bridge.cancel(cancel_token.clone()).unwrap();
        bridge
            .requests
            .try_send(ProjectPersistenceRequest::Open {
                token: cancel_token.clone(),
                path: save_path,
                cancellation: Arc::clone(&cancellation),
            })
            .unwrap();
        assert!(cancellation.load(Ordering::Acquire));
        match wait_for_event(&bridge) {
            ProjectPersistenceEvent::Cancelled { token } => assert_eq!(token, cancel_token),
            event => panic!("unexpected event: {event:?}"),
        }
        assert!(matches!(
            bridge.cancel(operation_tokens(1).pop().unwrap()),
            Err(ProjectPersistenceError::UnknownOperationToken)
        ));
        bridge.shutdown().unwrap();
    }

    #[test]
    fn explicit_shutdown_joins_an_idle_actor() {
        CurrentProjectPersistenceBridge::spawn()
            .unwrap()
            .shutdown()
            .unwrap();
    }
}

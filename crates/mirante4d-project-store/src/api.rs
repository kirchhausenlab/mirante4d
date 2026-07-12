use std::{
    collections::{BTreeMap, btree_map::Entry},
    fmt,
    io::{self, Read},
    num::NonZeroU64,
    path::{Component, Path, PathBuf},
    str::FromStr,
};

use mirante4d_identity::{ExactBytesDigest, RawObjectDescriptor, Sha256Digest};
use mirante4d_project_model::ProjectRevisionHighWater;
use mirante4d_project_model::{
    MAX_ARTIFACTS, ProjectGenerationProjection, ProjectId, ProjectRevisionId,
};
use thiserror::Error;

const GENERATION_ID_PREFIX: &str = "m4d-project-generation-v1-sha256:";

/// The domain-separated identity of one immutable project generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectGenerationId(Sha256Digest);

impl ProjectGenerationId {
    pub const PREFIX: &'static str = GENERATION_ID_PREFIX;

    pub const fn from_digest(digest: Sha256Digest) -> Self {
        Self(digest)
    }

    pub const fn digest(self) -> Sha256Digest {
        self.0
    }

    pub fn parse(value: &str) -> Result<Self, ProjectStoreFault> {
        value.parse()
    }
}

impl FromStr for ProjectGenerationId {
    type Err = ProjectStoreFault;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let digest = value
            .strip_prefix(Self::PREFIX)
            .ok_or(ProjectStoreFault::Corruption {
                stage: "generation_id",
            })?;
        Sha256Digest::parse(digest)
            .map(Self)
            .map_err(|_| ProjectStoreFault::Corruption {
                stage: "generation_id",
            })
    }
}

impl fmt::Display for ProjectGenerationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}{}", Self::PREFIX, self.0)
    }
}

/// A validated path naming the root of one directory-backed project store.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProjectStorePath(PathBuf);

impl ProjectStorePath {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, ProjectStoreFault> {
        let path = path.into();
        let valid_name = path
            .file_name()
            .is_some_and(|name| !name.is_empty() && name != "." && name != "..");
        let safe_components = path.components().all(|component| {
            matches!(
                component,
                Component::RootDir | Component::CurDir | Component::Normal(_)
            )
        });
        if path.as_os_str().is_empty()
            || !valid_name
            || !safe_components
            || path.extension().and_then(|extension| extension.to_str()) != Some("m4dproj")
        {
            return Err(ProjectStoreFault::Corruption {
                stage: "project_path",
            });
        }
        Ok(Self(path))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

/// Monotonic identity for one bounded actor request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectStoreRequestId(NonZeroU64);

impl ProjectStoreRequestId {
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Requested access for a project session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectOpenMode {
    ReadOnly,
    PreferWritable,
}

/// Exact configurable bounds for project-store work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectStoreLimits {
    pub(crate) project_envelope_bytes_max: usize,
    pub(crate) ref_record_bytes_exact: usize,
    pub(crate) ref_record_bytes_max: usize,
    pub(crate) generation_bytes_max: u64,
    pub(crate) object_or_page_bytes_max: u64,
    pub(crate) stream_buffer_bytes_max: usize,
    pub(crate) artifact_records_per_generation_max: usize,
    pub(crate) reachable_objects_per_generation_max: usize,
    pub(crate) generations_scanned_max: usize,
    pub(crate) directory_fanout_entries_max: usize,
    pub(crate) physical_store_entries_max: usize,
    pub(crate) pin_refs_max: usize,
    pub(crate) recovery_candidates_max: usize,
    pub(crate) actor_request_queue_max: usize,
    pub(crate) actor_completion_queue_max: usize,
    pub(crate) active_transactions_max: usize,
    pub(crate) open_file_descriptors_max: usize,
    pub(crate) autosave_queued_requests_max: usize,
    pub(crate) autosave_idle_delay_seconds: u64,
    pub(crate) autosave_max_delay_seconds: u64,
    pub(crate) gc_batch_entries_max: usize,
    pub(crate) gc_batch_bytes_max: u64,
}

impl ProjectStoreLimits {
    pub fn validate(self) -> Result<Self, ProjectStoreFault> {
        let nonzero = [
            self.project_envelope_bytes_max,
            self.ref_record_bytes_exact,
            self.ref_record_bytes_max,
            self.stream_buffer_bytes_max,
            self.artifact_records_per_generation_max,
            self.reachable_objects_per_generation_max,
            self.generations_scanned_max,
            self.directory_fanout_entries_max,
            self.physical_store_entries_max,
            self.pin_refs_max,
            self.recovery_candidates_max,
            self.actor_request_queue_max,
            self.actor_completion_queue_max,
            self.active_transactions_max,
            self.open_file_descriptors_max,
            self.autosave_queued_requests_max,
            self.gc_batch_entries_max,
        ];
        if nonzero.contains(&0)
            || self.generation_bytes_max == 0
            || self.object_or_page_bytes_max == 0
            || self.gc_batch_bytes_max == 0
            || self.ref_record_bytes_exact > self.ref_record_bytes_max
            || self.stream_buffer_bytes_max as u64 > self.object_or_page_bytes_max
            || self.active_transactions_max != 1
            || self.autosave_queued_requests_max != 1
            || self.autosave_idle_delay_seconds == 0
            || self.autosave_idle_delay_seconds > self.autosave_max_delay_seconds
        {
            return Err(ProjectStoreFault::Capacity {
                stage: "configuration",
            });
        }
        Ok(self)
    }

    pub const fn object_or_page_bytes_max(self) -> u64 {
        self.object_or_page_bytes_max
    }

    pub const fn stream_buffer_bytes_max(self) -> usize {
        self.stream_buffer_bytes_max
    }

    pub const fn actor_request_queue_max(self) -> usize {
        self.actor_request_queue_max
    }

    pub const fn actor_completion_queue_max(self) -> usize {
        self.actor_completion_queue_max
    }
}

impl Default for ProjectStoreLimits {
    fn default() -> Self {
        Self {
            project_envelope_bytes_max: 16_384,
            ref_record_bytes_exact: 160,
            ref_record_bytes_max: 4_096,
            generation_bytes_max: 67_108_864,
            object_or_page_bytes_max: 16_777_216,
            stream_buffer_bytes_max: 1_048_576,
            artifact_records_per_generation_max: 16_384,
            reachable_objects_per_generation_max: 65_536,
            generations_scanned_max: 4_096,
            directory_fanout_entries_max: 4_096,
            physical_store_entries_max: 131_072,
            pin_refs_max: 64,
            recovery_candidates_max: 64,
            actor_request_queue_max: 8,
            actor_completion_queue_max: 8,
            active_transactions_max: 1,
            open_file_descriptors_max: 64,
            autosave_queued_requests_max: 1,
            autosave_idle_delay_seconds: 30,
            autosave_max_delay_seconds: 120,
            gc_batch_entries_max: 1_024,
            gc_batch_bytes_max: 268_435_456,
        }
    }
}

/// Immutable configuration supplied before a store actor is started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectStoreConfig {
    limits: ProjectStoreLimits,
    autosave_enabled: bool,
}

impl ProjectStoreConfig {
    pub fn new(
        limits: ProjectStoreLimits,
        autosave_enabled: bool,
    ) -> Result<Self, ProjectStoreFault> {
        Ok(Self {
            limits: limits.validate()?,
            autosave_enabled,
        })
    }

    pub const fn limits(self) -> ProjectStoreLimits {
        self.limits
    }

    pub const fn autosave_enabled(self) -> bool {
        self.autosave_enabled
    }
}

impl Default for ProjectStoreConfig {
    fn default() -> Self {
        Self {
            limits: ProjectStoreLimits::default(),
            autosave_enabled: true,
        }
    }
}

/// Cancellable-stream factory for one immutable logical artifact object.
///
/// Implementations retain ownership of their bytes and must return a fresh
/// stream for each publication or verification pass. Cancellation remains
/// owned by the command/actor rather than by this persistence-neutral source.
pub trait ProjectObjectSource: Send + Sync {
    fn descriptor(&self) -> &RawObjectDescriptor;

    fn open(&self) -> io::Result<Box<dyn Read + Send>>;
}

/// One object source bound to the descriptor observed when the commit was
/// captured. The actor must use this frozen descriptor as its expectation;
/// asking the source for its descriptor again would reopen a TOCTOU gap.
pub(crate) struct CapturedObjectSource {
    descriptor: RawObjectDescriptor,
    source: Box<dyn ProjectObjectSource>,
}

impl CapturedObjectSource {
    pub(crate) fn descriptor(&self) -> &RawObjectDescriptor {
        &self.descriptor
    }

    pub(crate) fn open(&self) -> io::Result<Box<dyn Read + Send>> {
        self.source.open()
    }
}

/// Private ownership transfer from the frozen public capture into the store
/// actor. Keeping this as one value prevents individual commit facts from being
/// silently dropped when transaction execution is added.
pub(crate) struct ProjectCommitCaptureParts {
    pub(crate) projection: ProjectGenerationProjection,
    pub(crate) expected_parent: Option<ProjectGenerationId>,
    pub(crate) autosave_base: Option<ProjectGenerationId>,
    pub(crate) forked_from: Option<(ProjectId, ProjectGenerationId)>,
    pub(crate) object_sources: Vec<CapturedObjectSource>,
}

/// One exact revision/projection capture and its complete logical-object input.
pub struct ProjectCommitCapture {
    projection: ProjectGenerationProjection,
    expected_parent: Option<ProjectGenerationId>,
    autosave_base: Option<ProjectGenerationId>,
    forked_from: Option<(ProjectId, ProjectGenerationId)>,
    object_sources: Vec<CapturedObjectSource>,
}

impl ProjectCommitCapture {
    pub fn new(
        projection: ProjectGenerationProjection,
        expected_parent: Option<ProjectGenerationId>,
        autosave_base: Option<ProjectGenerationId>,
        forked_from: Option<(ProjectId, ProjectGenerationId)>,
        object_sources: Vec<Box<dyn ProjectObjectSource>>,
    ) -> Result<Self, ProjectStoreFault> {
        validate_object_source_count(object_sources.len())?;
        let mut expected = BTreeMap::<ExactBytesDigest, RawObjectDescriptor>::new();
        for artifact in projection.state().artifacts() {
            insert_descriptor(&mut expected, artifact.object())?;
        }
        let mut actual = BTreeMap::<ExactBytesDigest, RawObjectDescriptor>::new();
        let mut captured_sources = Vec::with_capacity(object_sources.len());
        for source in object_sources {
            let descriptor = source.descriptor().clone();
            insert_source_descriptor(&mut actual, &descriptor)?;
            captured_sources.push(CapturedObjectSource { descriptor, source });
        }
        if expected != actual {
            return Err(ProjectStoreFault::Corruption {
                stage: "object_source_closure",
            });
        }
        Ok(Self {
            projection,
            expected_parent,
            autosave_base,
            forked_from,
            object_sources: captured_sources,
        })
    }

    pub fn projection(&self) -> &ProjectGenerationProjection {
        &self.projection
    }

    pub fn object_source_count(&self) -> usize {
        self.object_sources.len()
    }

    pub const fn expected_parent(&self) -> Option<ProjectGenerationId> {
        self.expected_parent
    }

    pub const fn autosave_base(&self) -> Option<ProjectGenerationId> {
        self.autosave_base
    }

    pub const fn forked_from(&self) -> Option<(ProjectId, ProjectGenerationId)> {
        self.forked_from
    }

    pub(crate) fn into_parts(self) -> ProjectCommitCaptureParts {
        ProjectCommitCaptureParts {
            projection: self.projection,
            expected_parent: self.expected_parent,
            autosave_base: self.autosave_base,
            forked_from: self.forked_from,
            object_sources: self.object_sources,
        }
    }
}

fn validate_object_source_count(count: usize) -> Result<(), ProjectStoreFault> {
    if count > MAX_ARTIFACTS {
        Err(ProjectStoreFault::Capacity {
            stage: "object_sources",
        })
    } else {
        Ok(())
    }
}

fn insert_source_descriptor(
    descriptors: &mut BTreeMap<ExactBytesDigest, RawObjectDescriptor>,
    descriptor: &RawObjectDescriptor,
) -> Result<(), ProjectStoreFault> {
    if descriptors
        .insert(descriptor.digest(), descriptor.clone())
        .is_some()
    {
        Err(ProjectStoreFault::Corruption {
            stage: "duplicate_object_source",
        })
    } else {
        Ok(())
    }
}

impl fmt::Debug for ProjectCommitCapture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectCommitCapture")
            .field("revision", &self.projection.revision())
            .field("object_source_count", &self.object_sources.len())
            .finish_non_exhaustive()
    }
}

fn insert_descriptor(
    descriptors: &mut BTreeMap<ExactBytesDigest, RawObjectDescriptor>,
    descriptor: &RawObjectDescriptor,
) -> Result<(), ProjectStoreFault> {
    match descriptors.entry(descriptor.digest()) {
        Entry::Vacant(entry) => {
            entry.insert(descriptor.clone());
            Ok(())
        }
        Entry::Occupied(entry) if entry.get() == descriptor => Ok(()),
        Entry::Occupied(_) => Err(ProjectStoreFault::Corruption {
            stage: "object_descriptor",
        }),
    }
}

/// One validated generation which may be offered explicitly for recovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRecoveryCandidate {
    generation_id: ProjectGenerationId,
    generation_sequence: u64,
    revision_sequence: u64,
    origin: RecoveryOrigin,
    classification: RecoveryClassification,
    base_manual_generation_id: Option<ProjectGenerationId>,
    current_manual_generation_id: Option<ProjectGenerationId>,
    artifact_count: u32,
    non_regenerable_artifact_count: u32,
}

#[allow(dead_code)] // Constructed by the B2 actor/recovery slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecoveryOrigin {
    AutosaveHead,
    AutosaveRecovery,
    ManualPrevious,
    ManualRecovery,
    OrphanScan,
}

#[allow(dead_code)] // Constructed by the B2 actor/recovery slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecoveryClassification {
    Provisional,
    Newer,
    Stale,
    Divergent,
}

impl ProjectRecoveryCandidate {
    pub const fn generation_id(&self) -> ProjectGenerationId {
        self.generation_id
    }

    pub const fn generation_sequence(&self) -> u64 {
        self.generation_sequence
    }

    pub const fn revision_sequence(&self) -> u64 {
        self.revision_sequence
    }

    pub const fn classification(&self) -> &'static str {
        match self.classification {
            RecoveryClassification::Provisional => "provisional",
            RecoveryClassification::Newer => "newer",
            RecoveryClassification::Stale => "stale",
            RecoveryClassification::Divergent => "divergent",
        }
    }

    pub const fn origin(&self) -> &'static str {
        match self.origin {
            RecoveryOrigin::AutosaveHead => "autosave_head",
            RecoveryOrigin::AutosaveRecovery => "autosave_recovery",
            RecoveryOrigin::ManualPrevious => "manual_previous",
            RecoveryOrigin::ManualRecovery => "manual_recovery",
            RecoveryOrigin::OrphanScan => "orphan_scan",
        }
    }

    pub const fn is_provisional(&self) -> bool {
        matches!(self.classification, RecoveryClassification::Provisional)
    }

    pub const fn is_newer(&self) -> bool {
        matches!(self.classification, RecoveryClassification::Newer)
    }

    pub const fn is_stale(&self) -> bool {
        matches!(self.classification, RecoveryClassification::Stale)
    }

    pub const fn is_divergent(&self) -> bool {
        matches!(self.classification, RecoveryClassification::Divergent)
    }

    pub const fn base_manual_generation_id(&self) -> Option<ProjectGenerationId> {
        self.base_manual_generation_id
    }

    pub const fn current_manual_generation_id(&self) -> Option<ProjectGenerationId> {
        self.current_manual_generation_id
    }

    pub const fn artifact_count(&self) -> u32 {
        self.artifact_count
    }

    pub const fn non_regenerable_artifact_count(&self) -> u32 {
        self.non_regenerable_artifact_count
    }
}

/// Observable bounded-work counters; never a second persistence authority.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProjectStoreDiagnostics {
    pub queued_requests: usize,
    pub queued_completions: usize,
    pub active_transactions: usize,
    pub open_file_descriptors: usize,
    pub streamed_bytes: u64,
    pub published_objects: u64,
}

/// Durable success facts for a manual or autosave generation publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectStoreReceipt {
    captured_revision: ProjectRevisionId,
    captured_revision_high_water: ProjectRevisionHighWater,
    new_generation_id: ProjectGenerationId,
    current_generation_id: ProjectGenerationId,
    previous_generation_id: Option<ProjectGenerationId>,
    autosave_base_generation_id: Option<ProjectGenerationId>,
    published_objects: u64,
    published_bytes: u64,
}

impl ProjectStoreReceipt {
    pub(crate) fn manual(
        captured_revision: ProjectRevisionId,
        captured_revision_high_water: ProjectRevisionHighWater,
        generation_id: ProjectGenerationId,
        previous_generation_id: Option<ProjectGenerationId>,
        published_objects: u64,
        published_bytes: u64,
    ) -> Self {
        Self::generation(
            captured_revision,
            captured_revision_high_water,
            generation_id,
            previous_generation_id,
            None,
            published_objects,
            published_bytes,
        )
    }

    pub(crate) fn autosave(
        captured_revision: ProjectRevisionId,
        captured_revision_high_water: ProjectRevisionHighWater,
        generation_id: ProjectGenerationId,
        previous_generation_id: Option<ProjectGenerationId>,
        autosave_base_generation_id: ProjectGenerationId,
        published_objects: u64,
        published_bytes: u64,
    ) -> Self {
        Self::generation(
            captured_revision,
            captured_revision_high_water,
            generation_id,
            previous_generation_id,
            Some(autosave_base_generation_id),
            published_objects,
            published_bytes,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn generation(
        captured_revision: ProjectRevisionId,
        captured_revision_high_water: ProjectRevisionHighWater,
        generation_id: ProjectGenerationId,
        previous_generation_id: Option<ProjectGenerationId>,
        autosave_base_generation_id: Option<ProjectGenerationId>,
        published_objects: u64,
        published_bytes: u64,
    ) -> Self {
        Self {
            captured_revision,
            captured_revision_high_water,
            new_generation_id: generation_id,
            current_generation_id: generation_id,
            previous_generation_id,
            autosave_base_generation_id,
            published_objects,
            published_bytes,
        }
    }

    pub const fn captured_revision(&self) -> ProjectRevisionId {
        self.captured_revision
    }

    pub const fn captured_revision_high_water(&self) -> &ProjectRevisionHighWater {
        &self.captured_revision_high_water
    }

    pub const fn new_generation_id(&self) -> ProjectGenerationId {
        self.new_generation_id
    }

    pub const fn current_generation_id(&self) -> ProjectGenerationId {
        self.current_generation_id
    }

    pub const fn previous_generation_id(&self) -> Option<ProjectGenerationId> {
        self.previous_generation_id
    }

    pub const fn autosave_base_generation_id(&self) -> Option<ProjectGenerationId> {
        self.autosave_base_generation_id
    }

    pub const fn published_objects(&self) -> u64 {
        self.published_objects
    }

    pub const fn published_bytes(&self) -> u64 {
        self.published_bytes
    }
}

/// Typed fail-closed outcomes at the public project-store boundary.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ProjectStoreFault {
    #[error("the bounded project-store {queue} queue is full")]
    QueueFull { queue: &'static str },
    #[error("the project session is read-only")]
    ReadOnly,
    #[error("another process owns the writer lease")]
    WriterContended,
    #[error("the project head no longer matches the expected parent")]
    StaleParent,
    #[error("the destination already exists")]
    DestinationExists,
    #[error("the project filesystem is not qualified for writable durability")]
    UnsupportedFilesystem,
    #[error("project-store capacity limit was reached during {stage}")]
    Capacity { stage: &'static str },
    #[error("the logical object source changed while it was being consumed")]
    SourceChanged,
    #[error("an exact object or generation digest did not match its bytes")]
    DigestMismatch,
    #[error("project-store data is corrupt during {stage}")]
    Corruption { stage: &'static str },
    #[error("project-store work was cancelled before authority changed")]
    Cancelled,
    #[error("an authority update may be visible but durability is indeterminate")]
    CommitIndeterminate,
}

/// Commands accepted by the bounded background store actor.
#[derive(Debug)]
pub enum ProjectStoreCommand {
    Create {
        request_id: ProjectStoreRequestId,
        destination: ProjectStorePath,
        capture: ProjectCommitCapture,
    },
    Open {
        request_id: ProjectStoreRequestId,
        path: ProjectStorePath,
        mode: ProjectOpenMode,
    },
    ManualSave {
        request_id: ProjectStoreRequestId,
        capture: ProjectCommitCapture,
    },
    SaveAs {
        request_id: ProjectStoreRequestId,
        destination: ProjectStorePath,
        source_generation: ProjectGenerationId,
        capture: ProjectCommitCapture,
    },
    Autosave {
        request_id: ProjectStoreRequestId,
        capture: ProjectCommitCapture,
    },
    InspectRecovery {
        request_id: ProjectStoreRequestId,
    },
    OpenRecovery {
        request_id: ProjectStoreRequestId,
        generation_id: ProjectGenerationId,
    },
    Pin {
        request_id: ProjectStoreRequestId,
        checkpoint_id: String,
        generation_id: ProjectGenerationId,
    },
    Unpin {
        request_id: ProjectStoreRequestId,
        checkpoint_id: String,
    },
    PlanCompaction {
        request_id: ProjectStoreRequestId,
    },
    Trash {
        request_id: ProjectStoreRequestId,
        generations: Vec<ProjectGenerationId>,
    },
    Purge {
        request_id: ProjectStoreRequestId,
    },
    FullVerify {
        request_id: ProjectStoreRequestId,
    },
    Cancel {
        request_id: ProjectStoreRequestId,
        target_request_id: ProjectStoreRequestId,
    },
    Close {
        request_id: ProjectStoreRequestId,
    },
}

impl ProjectStoreCommand {
    pub const fn request_id(&self) -> ProjectStoreRequestId {
        match self {
            Self::Create { request_id, .. }
            | Self::Open { request_id, .. }
            | Self::ManualSave { request_id, .. }
            | Self::Autosave { request_id, .. }
            | Self::SaveAs { request_id, .. }
            | Self::InspectRecovery { request_id }
            | Self::OpenRecovery { request_id, .. }
            | Self::Pin { request_id, .. }
            | Self::Unpin { request_id, .. }
            | Self::PlanCompaction { request_id }
            | Self::Trash { request_id, .. }
            | Self::Purge { request_id }
            | Self::FullVerify { request_id }
            | Self::Cancel { request_id, .. }
            | Self::Close { request_id } => *request_id,
        }
    }
}

/// Completion emitted for one request; stale request IDs remain observable.
#[derive(Debug)]
pub enum ProjectStoreCompletion {
    Created {
        request_id: ProjectStoreRequestId,
        result: Result<ProjectStoreSession, ProjectStoreFault>,
    },
    Opened {
        request_id: ProjectStoreRequestId,
        result: Result<ProjectStoreSession, ProjectStoreFault>,
    },
    ManualSaved {
        request_id: ProjectStoreRequestId,
        result: Result<ProjectStoreReceipt, ProjectStoreFault>,
    },
    SavedAs {
        request_id: ProjectStoreRequestId,
        result: Result<ProjectStoreReceipt, ProjectStoreFault>,
    },
    Autosaved {
        request_id: ProjectStoreRequestId,
        result: Result<ProjectStoreReceipt, ProjectStoreFault>,
    },
    RecoveryInspected {
        request_id: ProjectStoreRequestId,
        result: Result<Vec<ProjectRecoveryCandidate>, ProjectStoreFault>,
    },
    RecoveryOpened {
        request_id: ProjectStoreRequestId,
        result: Result<ProjectStoreSession, ProjectStoreFault>,
    },
    Pinned {
        request_id: ProjectStoreRequestId,
        result: Result<(), ProjectStoreFault>,
    },
    Unpinned {
        request_id: ProjectStoreRequestId,
        result: Result<(), ProjectStoreFault>,
    },
    CompactionPlanned {
        request_id: ProjectStoreRequestId,
        result: Result<Vec<ProjectRecoveryCandidate>, ProjectStoreFault>,
    },
    Trashed {
        request_id: ProjectStoreRequestId,
        result: Result<ProjectStoreDiagnostics, ProjectStoreFault>,
    },
    Purged {
        request_id: ProjectStoreRequestId,
        result: Result<ProjectStoreDiagnostics, ProjectStoreFault>,
    },
    Verified {
        request_id: ProjectStoreRequestId,
        result: Result<ProjectStoreDiagnostics, ProjectStoreFault>,
    },
    Cancelled {
        request_id: ProjectStoreRequestId,
        result: Result<(), ProjectStoreFault>,
    },
    Closed {
        request_id: ProjectStoreRequestId,
        result: Result<(), ProjectStoreFault>,
    },
}

impl ProjectStoreCompletion {
    pub const fn request_id(&self) -> ProjectStoreRequestId {
        match self {
            Self::Created { request_id, .. }
            | Self::Opened { request_id, .. }
            | Self::ManualSaved { request_id, .. }
            | Self::SavedAs { request_id, .. }
            | Self::Autosaved { request_id, .. }
            | Self::RecoveryInspected { request_id, .. }
            | Self::RecoveryOpened { request_id, .. }
            | Self::Pinned { request_id, .. }
            | Self::Unpinned { request_id, .. }
            | Self::CompactionPlanned { request_id, .. }
            | Self::Trashed { request_id, .. }
            | Self::Purged { request_id, .. }
            | Self::Verified { request_id, .. }
            | Self::Cancelled { request_id, .. }
            | Self::Closed { request_id, .. } => *request_id,
        }
    }
}

/// Opaque open-session capability. It cannot be forged outside this crate.
#[derive(Debug)]
pub struct ProjectStoreSession {
    path: ProjectStorePath,
    project_id: ProjectId,
    mode: ProjectOpenMode,
    current_manual_generation: Option<ProjectGenerationId>,
    current_autosave_generation: Option<ProjectGenerationId>,
}

impl ProjectStoreSession {
    pub fn path(&self) -> &ProjectStorePath {
        &self.path
    }

    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    pub const fn mode(&self) -> ProjectOpenMode {
        self.mode
    }

    pub const fn current_manual_generation(&self) -> Option<ProjectGenerationId> {
        self.current_manual_generation
    }

    pub const fn current_autosave_generation(&self) -> Option<ProjectGenerationId> {
        self.current_autosave_generation
    }
}

/// Opaque owner of the future bounded background actor.
///
/// B2 intentionally exposes no constructor until the transactional core can
/// establish its queues, leases, and shutdown protocol together.
#[derive(Debug)]
pub struct ProjectStoreActor {
    _private: ActorPrivate,
}

#[derive(Debug)]
struct ActorPrivate;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_id_round_trips_only_the_frozen_typed_form() {
        let digest = Sha256Digest::from_bytes([0xabu8; 32]);
        let id = ProjectGenerationId::from_digest(digest);
        assert_eq!(id.digest(), digest);
        assert_eq!(id.to_string().parse::<ProjectGenerationId>().unwrap(), id);
        assert!(digest.to_string().parse::<ProjectGenerationId>().is_err());
        assert!(
            format!("{}{}", ProjectGenerationId::PREFIX, "AB".repeat(32))
                .parse::<ProjectGenerationId>()
                .is_err()
        );
    }

    #[test]
    fn project_store_path_requires_the_product_suffix_without_touching_disk() {
        let path = ProjectStorePath::new("experiment.m4dproj").unwrap();
        assert_eq!(path.as_path(), Path::new("experiment.m4dproj"));
        for invalid in [
            "",
            ".",
            "../experiment.m4dproj",
            "experiment",
            "experiment.m4dproj.tmp",
        ] {
            assert!(
                ProjectStorePath::new(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn accepted_defaults_match_the_frozen_b2_work_bounds() {
        let limits = ProjectStoreLimits::default().validate().unwrap();
        assert_eq!(limits.object_or_page_bytes_max(), 16_777_216);
        assert_eq!(limits.stream_buffer_bytes_max(), 1_048_576);
        assert_eq!(limits.actor_request_queue_max(), 8);
        assert_eq!(limits.actor_completion_queue_max(), 8);
        assert!(ProjectStoreRequestId::new(0).is_none());
        assert_eq!(ProjectStoreRequestId::new(7).unwrap().get(), 7);
        assert!(validate_object_source_count(MAX_ARTIFACTS).is_ok());
        assert!(matches!(
            validate_object_source_count(MAX_ARTIFACTS + 1),
            Err(ProjectStoreFault::Capacity { .. })
        ));

        let descriptor = RawObjectDescriptor::new(
            mirante4d_identity::ExactBytesDigest::from_digest(Sha256Digest::from_bytes([7; 32])),
            0,
            mirante4d_identity::MediaType::parse("application/octet-stream").unwrap(),
            mirante4d_identity::ObjectRole::parse("project.object").unwrap(),
        );
        let mut sources = BTreeMap::new();
        insert_source_descriptor(&mut sources, &descriptor).unwrap();
        assert!(matches!(
            insert_source_descriptor(&mut sources, &descriptor),
            Err(ProjectStoreFault::Corruption {
                stage: "duplicate_object_source"
            })
        ));
    }
}

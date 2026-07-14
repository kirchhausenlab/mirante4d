//! Pure application state transitions for Mirante4D.
//!
//! This crate owns the framework-neutral command, reducer, event, snapshot,
//! operation, and fault boundary. It deliberately owns no filesystem I/O,
//! threads, UI values, renderer/runtime payloads, or serialization.

#![forbid(unsafe_code)]

pub mod import_workflow;
mod project_store_service;
pub mod render_coordination;
pub mod viewer_tools;
pub mod viewport_interaction;

pub use project_store_service::{
    MonotonicClock, ProjectRecoveryStoreLocator, ProjectStoreApplicationService,
    ProjectStoreLifecycle, ProjectStoreServiceError, ProjectStoreServiceEvent,
    ProjectStoreServiceStatus, SystemMonotonicClock,
};
pub use render_coordination::{
    CrossSectionPanelScheduleReason, CrossSectionPanelScheduleState,
    CrossSectionPanelScheduleStatus, DisplayedFrameFreshness, FrameCompleteness, FrameFailureKind,
    FrameFidelityStatus, LodDecisionReason, RenderBackend, RenderCoordinationState,
    RenderSurfaceState, ResidentRenderFailureStatus,
};

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

use mirante4d_dataset::{DatasetCatalog, DatasetSourceId, ScientificIdentityStatus};
use mirante4d_domain::{
    CameraView, CrossSectionView, IsoLightState, LogicalLayerKey, TimeIndex, ToolKind, ViewerLayout,
};
use mirante4d_identity::ScientificContentId;
use mirante4d_project_model::{
    ArtifactCompleteness, ArtifactHandleId, ArtifactRecoverability, ArtifactReference,
    ArtifactSchema, ChannelPreset, ChannelPresetId, DatasetReference, LayerViewState,
    MAX_CHANNEL_PRESETS, MAX_TOTAL_CHANNEL_PRESET_ENTRIES, ProjectGenerationProjection, ProjectId,
    ProjectRevisionHighWater, ProjectRevisionId, ProjectState, ViewState,
};
use mirante4d_render_api::PresentedFrame;
pub use mirante4d_render_api::{PresentationPaintRequest, PresentationViewport};
use mirante4d_settings::{RejectedFileDisposition, ResourcePolicy};

/// Maximum number of project revisions retained for undo/redo.
pub const MAX_HISTORY_ENTRIES: usize = 128;
/// Maximum number of events waiting for a consumer.
pub const MAX_PENDING_EVENTS: usize = 256;
/// Maximum number of concurrently registered background operations.
pub const MAX_ACTIVE_OPERATIONS: usize = 64;
/// Maximum number of transient analysis tables retained in one source session.
pub const MAX_ANALYSIS_TABLES: usize = 1_024;
/// Maximum number of transient analysis plots retained in one source session.
pub const MAX_ANALYSIS_PLOTS: usize = 1_024;
/// Maximum number of series described by one transient analysis plot.
pub const MAX_ANALYSIS_PLOT_SERIES: usize = 1_024;
/// Maximum number of points described by one transient analysis plot.
pub const MAX_ANALYSIS_PLOT_POINTS: u64 = 16_777_216;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceSessionGeneration(u64);

impl SourceSessionGeneration {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CurrentnessGeneration(u64);

impl CurrentnessGeneration {
    pub const fn initial() -> Self {
        Self(0)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OperationId(u64);

impl OperationId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Stable key for a durable analysis-table artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnalysisTableId([u8; 16]);

impl AnalysisTableId {
    pub fn from_artifact_handle(handle: &ArtifactHandleId) -> Self {
        Self(handle.bytes())
    }

    pub const fn artifact_handle_id(self) -> ArtifactHandleId {
        ArtifactHandleId::from_bytes(self.0)
    }
}

/// Stable key for a durable analysis-plot artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnalysisPlotId([u8; 16]);

impl AnalysisPlotId {
    pub fn from_artifact_handle(handle: &ArtifactHandleId) -> Self {
        Self(handle.bytes())
    }

    pub const fn artifact_handle_id(self) -> ArtifactHandleId {
        ArtifactHandleId::from_bytes(self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisDescriptorError {
    TooManySeries,
    TooManyPoints,
    PointCountOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisTableDescriptor {
    id: AnalysisTableId,
    row_count: u64,
}

impl AnalysisTableDescriptor {
    pub const fn new(id: AnalysisTableId, row_count: u64) -> Self {
        Self { id, row_count }
    }

    pub const fn id(&self) -> AnalysisTableId {
        self.id
    }

    pub const fn row_count(&self) -> u64 {
        self.row_count
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisPlotDescriptor {
    id: AnalysisPlotId,
    series_point_counts: Vec<u64>,
}

impl AnalysisPlotDescriptor {
    pub fn new(
        id: AnalysisPlotId,
        series_point_counts: Vec<u64>,
    ) -> Result<Self, AnalysisDescriptorError> {
        if series_point_counts.len() > MAX_ANALYSIS_PLOT_SERIES {
            return Err(AnalysisDescriptorError::TooManySeries);
        }
        let total = series_point_counts.iter().try_fold(0_u64, |total, count| {
            total
                .checked_add(*count)
                .ok_or(AnalysisDescriptorError::PointCountOverflow)
        })?;
        if total > MAX_ANALYSIS_PLOT_POINTS {
            return Err(AnalysisDescriptorError::TooManyPoints);
        }
        Ok(Self {
            id,
            series_point_counts,
        })
    }

    pub const fn id(&self) -> AnalysisPlotId {
        self.id
    }

    pub fn series_point_counts(&self) -> &[u64] {
        &self.series_point_counts
    }

    pub fn point_count(&self, series_index: u16) -> Option<u64> {
        self.series_point_counts
            .get(usize::from(series_index))
            .copied()
    }
}

/// One authenticated analysis result reconstructed from durable project artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedAnalysisDescriptorBundle {
    artifacts: Vec<ArtifactReference>,
    table: AnalysisTableDescriptor,
    plot: Option<AnalysisPlotDescriptor>,
}

impl LoadedAnalysisDescriptorBundle {
    pub const fn new(
        artifacts: Vec<ArtifactReference>,
        table: AnalysisTableDescriptor,
        plot: Option<AnalysisPlotDescriptor>,
    ) -> Self {
        Self {
            artifacts,
            table,
            plot,
        }
    }

    pub fn artifacts(&self) -> &[ArtifactReference] {
        &self.artifacts
    }

    pub const fn table(&self) -> &AnalysisTableDescriptor {
        &self.table
    }

    pub const fn plot(&self) -> Option<&AnalysisPlotDescriptor> {
        self.plot.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalysisPlotPointSelection {
    plot_id: AnalysisPlotId,
    series_index: u16,
    point_index: u64,
}

impl AnalysisPlotPointSelection {
    pub const fn new(plot_id: AnalysisPlotId, series_index: u16, point_index: u64) -> Self {
        Self {
            plot_id,
            series_index,
            point_index,
        }
    }

    pub const fn plot_id(self) -> AnalysisPlotId {
        self.plot_id
    }

    pub const fn series_index(self) -> u16 {
        self.series_index
    }

    pub const fn point_index(self) -> u64 {
        self.point_index
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(u64);

impl TaskId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SettingsChangeId(u64);

impl SettingsChangeId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsChangeToken {
    id: SettingsChangeId,
    policy: ResourcePolicy,
    rejected_file_disposition: RejectedFileDisposition,
}

impl SettingsChangeToken {
    pub const fn id(self) -> SettingsChangeId {
        self.id
    }

    pub const fn policy(self) -> ResourcePolicy {
        self.policy
    }

    pub const fn rejected_file_disposition(self) -> RejectedFileDisposition {
        self.rejected_file_disposition
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationKind {
    DatasetOpen,
    SourceVerification,
    ProjectOpen,
    ProjectSave,
    ProjectSaveAs,
    ProjectRecovery,
    Analysis,
    Import,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationToken {
    operation_id: OperationId,
    task_id: TaskId,
    kind: OperationKind,
    source_identity: Option<ScientificContentId>,
    source_session_generation: SourceSessionGeneration,
    project_id: Option<ProjectId>,
    project_revision: Option<ProjectRevisionId>,
    target_project_id: Option<ProjectId>,
    currentness_generation: CurrentnessGeneration,
}

impl OperationToken {
    pub const fn operation_id(&self) -> OperationId {
        self.operation_id
    }

    pub const fn task_id(&self) -> TaskId {
        self.task_id
    }

    pub const fn kind(&self) -> OperationKind {
        self.kind
    }

    pub const fn source_identity(&self) -> Option<ScientificContentId> {
        self.source_identity
    }

    pub const fn source_session_generation(&self) -> SourceSessionGeneration {
        self.source_session_generation
    }

    pub const fn project_id(&self) -> Option<ProjectId> {
        self.project_id
    }

    pub const fn project_revision(&self) -> Option<ProjectRevisionId> {
        self.project_revision
    }

    pub const fn target_project_id(&self) -> Option<ProjectId> {
        self.target_project_id
    }

    pub const fn currentness_generation(&self) -> CurrentnessGeneration {
        self.currentness_generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourcePolicyRejection {
    InvalidPolicy,
    UnsupportedDocument,
    DocumentTooLarge,
    AtomicWriteFailed,
    CommitIndeterminate,
    InvalidDocument,
    PermissionDenied,
    PathUnavailable,
    ExplicitReplacementRequired,
    ActorQueueFull,
    ActorUnavailable,
    ReadFailed,
}

/// Closed execution-failure vocabulary reported by operation workers.
///
/// Reducer rejection is represented separately by [`ApplicationFaultCode`].
/// This type describes work that was validly admitted and then failed outside
/// the pure application boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationFailureCode {
    DatasetNotFound,
    DatasetPermissionDenied,
    DatasetInvalid,
    DatasetUnsupported,
    DatasetCapacityExceeded,
    DatasetReadFailed,
    SourceChanged,
    SourceVerificationInvalid,
    SourceVerificationCapacityExceeded,
    SourceVerificationReadFailed,
    ProjectNotFound,
    ProjectPermissionDenied,
    ProjectInvalidDocument,
    ProjectUnsupportedSchema,
    ProjectReadFailed,
    ProjectWriteFailed,
    ProjectCommitIndeterminate,
    ProjectReadOnly,
    ProjectWriterContended,
    ProjectStaleParent,
    ProjectDestinationExists,
    ProjectUnsupportedFilesystem,
    ProjectCapacityExceeded,
    ProjectSourceChanged,
    ProjectDigestMismatch,
    ProjectCorrupt,
    ProjectBusy,
    AnalysisInvalidInput,
    AnalysisCapacityExceeded,
    AnalysisExecutionFailed,
    ImportInvalidInput,
    ImportCapacityExceeded,
    ImportExecutionFailed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OperationCompletion {
    Succeeded,
    Cancelled,
    Failed(OperationFailureCode),
    DatasetOpened {
        source_generation: SourceSessionGeneration,
        catalog: Arc<DatasetCatalog>,
        workspace: Box<UnboundWorkspace>,
    },
    SourceVerified {
        source_generation: SourceSessionGeneration,
        catalog: Arc<DatasetCatalog>,
        dataset: DatasetReference,
    },
    AnalysisCommitted {
        projection: Box<ProjectGenerationProjection>,
        table: AnalysisTableDescriptor,
        plot: Option<AnalysisPlotDescriptor>,
    },
    ProjectOpened(Box<ProjectGenerationProjection>),
    ProjectSaved(ProjectRevisionId),
    ProjectSavedAs(Box<ProjectGenerationProjection>),
    ProjectRecovered(Box<ProjectGenerationProjection>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourcePolicyPersistenceOutcome {
    Persisted,
    Rejected(ResourcePolicyRejection),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CrossSectionPanelId {
    Xy,
    Xz,
    Yz,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransientApplicationState {
    playback_active: bool,
    last_playback_tick: Option<u64>,
    active_tool: ToolKind,
    selected_channel_preset: Option<ChannelPresetId>,
    selected_artifact: Option<ArtifactHandleId>,
    active_cross_section_panel: Option<CrossSectionPanelId>,
    analysis_tables: Arc<[AnalysisTableDescriptor]>,
    analysis_plots: Arc<[AnalysisPlotDescriptor]>,
    selected_analysis_table: Option<AnalysisTableId>,
    selected_analysis_plot: Option<AnalysisPlotId>,
    selected_analysis_plot_point: Option<AnalysisPlotPointSelection>,
}

impl Default for TransientApplicationState {
    fn default() -> Self {
        Self {
            playback_active: false,
            last_playback_tick: None,
            active_tool: ToolKind::Navigate,
            selected_channel_preset: None,
            selected_artifact: None,
            active_cross_section_panel: None,
            analysis_tables: Arc::from([]),
            analysis_plots: Arc::from([]),
            selected_analysis_table: None,
            selected_analysis_plot: None,
            selected_analysis_plot_point: None,
        }
    }
}

impl TransientApplicationState {
    pub const fn playback_active(&self) -> bool {
        self.playback_active
    }

    pub const fn last_playback_tick(&self) -> Option<u64> {
        self.last_playback_tick
    }

    pub const fn active_tool(&self) -> ToolKind {
        self.active_tool
    }

    pub fn selected_channel_preset(&self) -> Option<&ChannelPresetId> {
        self.selected_channel_preset.as_ref()
    }

    pub fn selected_artifact(&self) -> Option<&ArtifactHandleId> {
        self.selected_artifact.as_ref()
    }

    pub const fn active_cross_section_panel(&self) -> Option<CrossSectionPanelId> {
        self.active_cross_section_panel
    }

    pub fn analysis_tables(&self) -> &[AnalysisTableDescriptor] {
        &self.analysis_tables
    }

    pub fn analysis_plots(&self) -> &[AnalysisPlotDescriptor] {
        &self.analysis_plots
    }

    pub const fn selected_analysis_table(&self) -> Option<AnalysisTableId> {
        self.selected_analysis_table
    }

    pub const fn selected_analysis_plot(&self) -> Option<AnalysisPlotId> {
        self.selected_analysis_plot
    }

    pub const fn selected_analysis_plot_point(&self) -> Option<AnalysisPlotPointSelection> {
        self.selected_analysis_plot_point
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApplicationCommand {
    RequestDatasetOpen,
    RequestSourceVerification,
    UpdateSourceVerificationProgress {
        token: OperationToken,
        completed_work: u64,
        total_work: u64,
    },
    InvalidateSourceVerification {
        source_generation: SourceSessionGeneration,
    },
    AttachVerifiedDataset,
    SetActiveLayer(LogicalLayerKey),
    SetTimepoint(TimeIndex),
    SetLayerView(LayerViewState),
    ReplaceView(ViewState),
    /// Commits one camera interaction. The UI bridge must not dispatch every
    /// raw pointer sample as a durable project revision.
    SetCamera(CameraView),
    SetLayout {
        layout: ViewerLayout,
        cross_section: CrossSectionView,
    },
    SetIsoLight(IsoLightState),
    SetLayerOrder(Vec<LogicalLayerKey>),
    UpsertChannelPreset(ChannelPreset),
    RemoveChannelPreset(ChannelPresetId),
    ApplyChannelPreset(ChannelPresetId),
    UpsertArtifact(ArtifactReference),
    RemoveArtifact(ArtifactHandleId),
    SetPlaybackActive(bool),
    AdvancePlaybackTick(u64),
    SetActiveTool(ToolKind),
    SelectChannelPreset(Option<ChannelPresetId>),
    SelectArtifact(Option<ArtifactHandleId>),
    SetActiveCrossSectionPanel(Option<CrossSectionPanelId>),
    SelectAnalysisTable(Option<AnalysisTableId>),
    SelectAnalysisPlot(Option<AnalysisPlotId>),
    SelectAnalysisPlotPoint(Option<AnalysisPlotPointSelection>),
    Undo,
    Redo,
    RequestProjectOpen,
    RequestProjectSave,
    RequestProjectSaveAs {
        new_project_id: ProjectId,
    },
    RequestProjectRecovery,
    BeginOperation(OperationKind),
    StageAnalysisBundle {
        token: OperationToken,
        artifacts: Vec<ArtifactReference>,
    },
    InstallLoadedAnalysisDescriptors {
        project_id: ProjectId,
        revision: ProjectRevisionId,
        currentness: CurrentnessGeneration,
        bundles: Vec<LoadedAnalysisDescriptorBundle>,
    },
    CompleteOperation {
        token: OperationToken,
        completion: OperationCompletion,
    },
    CancelOperation(OperationId),
    RequestResourcePolicyChange {
        policy: ResourcePolicy,
        rejected_file_disposition: RejectedFileDisposition,
    },
    CompleteResourcePolicyPersistence {
        token: SettingsChangeToken,
        outcome: ResourcePolicyPersistenceOutcome,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationCommandKind {
    RequestDatasetOpen,
    RequestSourceVerification,
    UpdateSourceVerificationProgress,
    InvalidateSourceVerification,
    AttachVerifiedDataset,
    SetActiveLayer,
    SetTimepoint,
    SetLayerView,
    ReplaceView,
    SetCamera,
    SetLayout,
    SetIsoLight,
    SetLayerOrder,
    UpsertChannelPreset,
    RemoveChannelPreset,
    ApplyChannelPreset,
    UpsertArtifact,
    RemoveArtifact,
    SetPlaybackActive,
    AdvancePlaybackTick,
    SetActiveTool,
    SelectChannelPreset,
    SelectArtifact,
    SetActiveCrossSectionPanel,
    SelectAnalysisTable,
    SelectAnalysisPlot,
    SelectAnalysisPlotPoint,
    Undo,
    Redo,
    RequestProjectOpen,
    RequestProjectSave,
    RequestProjectSaveAs,
    RequestProjectRecovery,
    BeginOperation,
    StageAnalysisBundle,
    InstallLoadedAnalysisDescriptors,
    CompleteOperation,
    CancelOperation,
    RequestResourcePolicyChange,
    CompleteResourcePolicyPersistence,
}

impl ApplicationCommand {
    pub const fn kind(&self) -> ApplicationCommandKind {
        match self {
            Self::RequestDatasetOpen => ApplicationCommandKind::RequestDatasetOpen,
            Self::RequestSourceVerification => ApplicationCommandKind::RequestSourceVerification,
            Self::UpdateSourceVerificationProgress { .. } => {
                ApplicationCommandKind::UpdateSourceVerificationProgress
            }
            Self::InvalidateSourceVerification { .. } => {
                ApplicationCommandKind::InvalidateSourceVerification
            }
            Self::AttachVerifiedDataset => ApplicationCommandKind::AttachVerifiedDataset,
            Self::SetActiveLayer(_) => ApplicationCommandKind::SetActiveLayer,
            Self::SetTimepoint(_) => ApplicationCommandKind::SetTimepoint,
            Self::SetLayerView(_) => ApplicationCommandKind::SetLayerView,
            Self::ReplaceView(_) => ApplicationCommandKind::ReplaceView,
            Self::SetCamera(_) => ApplicationCommandKind::SetCamera,
            Self::SetLayout { .. } => ApplicationCommandKind::SetLayout,
            Self::SetIsoLight(_) => ApplicationCommandKind::SetIsoLight,
            Self::SetLayerOrder(_) => ApplicationCommandKind::SetLayerOrder,
            Self::UpsertChannelPreset(_) => ApplicationCommandKind::UpsertChannelPreset,
            Self::RemoveChannelPreset(_) => ApplicationCommandKind::RemoveChannelPreset,
            Self::ApplyChannelPreset(_) => ApplicationCommandKind::ApplyChannelPreset,
            Self::UpsertArtifact(_) => ApplicationCommandKind::UpsertArtifact,
            Self::RemoveArtifact(_) => ApplicationCommandKind::RemoveArtifact,
            Self::SetPlaybackActive(_) => ApplicationCommandKind::SetPlaybackActive,
            Self::AdvancePlaybackTick(_) => ApplicationCommandKind::AdvancePlaybackTick,
            Self::SetActiveTool(_) => ApplicationCommandKind::SetActiveTool,
            Self::SelectChannelPreset(_) => ApplicationCommandKind::SelectChannelPreset,
            Self::SelectArtifact(_) => ApplicationCommandKind::SelectArtifact,
            Self::SetActiveCrossSectionPanel(_) => {
                ApplicationCommandKind::SetActiveCrossSectionPanel
            }
            Self::SelectAnalysisTable(_) => ApplicationCommandKind::SelectAnalysisTable,
            Self::SelectAnalysisPlot(_) => ApplicationCommandKind::SelectAnalysisPlot,
            Self::SelectAnalysisPlotPoint(_) => ApplicationCommandKind::SelectAnalysisPlotPoint,
            Self::Undo => ApplicationCommandKind::Undo,
            Self::Redo => ApplicationCommandKind::Redo,
            Self::RequestProjectOpen => ApplicationCommandKind::RequestProjectOpen,
            Self::RequestProjectSave => ApplicationCommandKind::RequestProjectSave,
            Self::RequestProjectSaveAs { .. } => ApplicationCommandKind::RequestProjectSaveAs,
            Self::RequestProjectRecovery => ApplicationCommandKind::RequestProjectRecovery,
            Self::BeginOperation(_) => ApplicationCommandKind::BeginOperation,
            Self::StageAnalysisBundle { .. } => ApplicationCommandKind::StageAnalysisBundle,
            Self::InstallLoadedAnalysisDescriptors { .. } => {
                ApplicationCommandKind::InstallLoadedAnalysisDescriptors
            }
            Self::CompleteOperation { .. } => ApplicationCommandKind::CompleteOperation,
            Self::CancelOperation(_) => ApplicationCommandKind::CancelOperation,
            Self::RequestResourcePolicyChange { .. } => {
                ApplicationCommandKind::RequestResourcePolicyChange
            }
            Self::CompleteResourcePolicyPersistence { .. } => {
                ApplicationCommandKind::CompleteResourcePolicyPersistence
            }
        }
    }

    fn mutates_durable_project(&self) -> bool {
        matches!(
            self,
            Self::AttachVerifiedDataset
                | Self::SetActiveLayer(_)
                | Self::SetTimepoint(_)
                | Self::SetLayerView(_)
                | Self::ReplaceView(_)
                | Self::SetCamera(_)
                | Self::SetLayout { .. }
                | Self::SetIsoLight(_)
                | Self::SetLayerOrder(_)
                | Self::UpsertChannelPreset(_)
                | Self::RemoveChannelPreset(_)
                | Self::ApplyChannelPreset(_)
                | Self::UpsertArtifact(_)
                | Self::RemoveArtifact(_)
                | Self::AdvancePlaybackTick(_)
                | Self::Undo
                | Self::Redo
                | Self::RequestProjectOpen
                | Self::RequestProjectSave
                | Self::RequestProjectSaveAs { .. }
                | Self::RequestProjectRecovery
                | Self::BeginOperation(_)
                | Self::StageAnalysisBundle { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationFaultCode {
    IdentityVerificationRequired,
    SourceSessionMismatch,
    SourceGenerationNotAdvanced,
    DatasetIdentityMismatch,
    WorkspaceAlreadyBound,
    WorkspaceUnbound,
    InvalidProjectTransition,
    DatasetLayerClosureMismatch,
    TimepointOutOfBounds,
    LayerNotFound,
    ChannelPresetNotFound,
    ArtifactNotFound,
    AnalysisTableNotFound,
    AnalysisPlotNotFound,
    AnalysisPointOutOfBounds,
    AnalysisRegistryFull,
    UndoUnavailable,
    RedoUnavailable,
    EventQueueFull,
    OperationRegistryFull,
    OperationNotFound,
    OperationTokenMismatch,
    StaleOperationCompletion,
    InvalidOperationCompletion,
    InvalidOperationProgress,
    OperationConflict,
    ResourcePolicyChangePending,
    ResourcePolicyCompletionMismatch,
    CounterOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationFault {
    code: ApplicationFaultCode,
    command: ApplicationCommandKind,
    operation_id: Option<OperationId>,
    task_id: Option<TaskId>,
    source_generation: SourceSessionGeneration,
    project_id: Option<ProjectId>,
}

impl ApplicationFault {
    pub const fn code(&self) -> ApplicationFaultCode {
        self.code
    }

    pub const fn command(&self) -> ApplicationCommandKind {
        self.command
    }

    pub const fn operation_id(&self) -> Option<OperationId> {
        self.operation_id
    }

    pub const fn task_id(&self) -> Option<TaskId> {
        self.task_id
    }

    pub const fn source_generation(&self) -> SourceSessionGeneration {
        self.source_generation
    }

    pub const fn project_id(&self) -> Option<ProjectId> {
        self.project_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandEffect {
    NoChange,
    Changed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApplicationEvent {
    DatasetOpenRequested {
        token: OperationToken,
    },
    CurrentSourceReplaced {
        source_generation: SourceSessionGeneration,
        provisional_project_id: ProjectId,
    },
    SourceVerificationRequested {
        token: OperationToken,
    },
    SourceVerificationProgress {
        token: OperationToken,
        completed_work: u64,
        total_work: u64,
    },
    SourceVerified {
        source_generation: SourceSessionGeneration,
        scientific_content_id: ScientificContentId,
    },
    SourceVerificationInvalidated {
        source_generation: SourceSessionGeneration,
    },
    WorkspaceChanged {
        currentness: CurrentnessGeneration,
    },
    TransientStateChanged,
    ProjectAttached {
        project_id: ProjectId,
        revision: ProjectRevisionId,
    },
    ProjectRevisionChanged {
        project_id: ProjectId,
        revision: ProjectRevisionId,
        dirty: bool,
    },
    ProjectOpenRequested {
        token: OperationToken,
    },
    ProjectSaveRequested {
        token: OperationToken,
        projection: Arc<ProjectGenerationProjection>,
    },
    ProjectSaveAsRequested {
        token: OperationToken,
        projection: Arc<ProjectGenerationProjection>,
    },
    ProjectRecoveryRequested {
        token: OperationToken,
    },
    AnalysisCommitRequested {
        token: OperationToken,
        projection: Arc<ProjectGenerationProjection>,
    },
    ProjectSaved {
        revision: ProjectRevisionId,
    },
    ProjectSavedAs {
        project_id: ProjectId,
        revision: ProjectRevisionId,
    },
    ProjectRecovered {
        project_id: ProjectId,
        revision: ProjectRevisionId,
    },
    OperationStarted {
        token: OperationToken,
    },
    OperationCancellationRequested {
        token: OperationToken,
    },
    OperationCompleted {
        token: OperationToken,
        outcome: OperationOutcome,
    },
    ResourcePolicyChangePending {
        token: SettingsChangeToken,
    },
    ResourcePolicyPersisted {
        token: SettingsChangeToken,
        restart_required: bool,
    },
    ResourcePolicyRejected {
        token: SettingsChangeToken,
        reason: ResourcePolicyRejection,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationOutcome {
    Succeeded,
    Cancelled,
    Failed(OperationFailureCode),
    DatasetOpened,
    SourceVerified,
    AnalysisCommitted,
    ProjectOpened,
    ProjectSaved,
    ProjectSavedAs,
    ProjectRecovered,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnboundWorkspace {
    provisional_project_id: ProjectId,
    view: ViewState,
    channel_presets: Vec<ChannelPreset>,
}

impl UnboundWorkspace {
    pub fn new(
        provisional_project_id: ProjectId,
        view: ViewState,
        channel_presets: Vec<ChannelPreset>,
    ) -> Result<Self, ApplicationFaultCode> {
        validate_unbound_presets(&view, &channel_presets)?;
        Ok(Self {
            provisional_project_id,
            view,
            channel_presets,
        })
    }

    pub const fn provisional_project_id(&self) -> ProjectId {
        self.provisional_project_id
    }

    pub fn view(&self) -> &ViewState {
        &self.view
    }

    pub fn channel_presets(&self) -> &[ChannelPreset] {
        &self.channel_presets
    }
}

#[derive(Debug, Clone, PartialEq)]
struct HistoryEntry {
    revision: ProjectRevisionId,
    state: Arc<ProjectState>,
}

#[derive(Debug, Clone, PartialEq)]
struct BoundWorkspace {
    history: VecDeque<HistoryEntry>,
    cursor: usize,
    high_water: ProjectRevisionHighWater,
    saved_revision: Option<ProjectRevisionId>,
}

impl BoundWorkspace {
    fn attached(state: ProjectState) -> Self {
        let project_id = state.project_id();
        let revision = ProjectRevisionId::initial(project_id);
        Self {
            history: VecDeque::from([HistoryEntry {
                revision,
                state: Arc::new(state),
            }]),
            cursor: 0,
            high_water: ProjectRevisionHighWater::initial(project_id),
            saved_revision: None,
        }
    }

    fn restored_for_verified_source(
        projection: ProjectGenerationProjection,
        verified_source: &DatasetReference,
    ) -> Result<(Self, Option<ProjectRevisionId>), ApplicationFaultCode> {
        let (saved_revision, mut high_water, state) = projection.into_parts();
        if !state.dataset().has_same_scientific_content(verified_source) {
            return Err(ApplicationFaultCode::DatasetIdentityMismatch);
        }
        if state.dataset() == verified_source {
            return Ok((
                Self {
                    history: VecDeque::from([HistoryEntry {
                        revision: saved_revision,
                        state: Arc::new(state),
                    }]),
                    cursor: 0,
                    high_water,
                    saved_revision: Some(saved_revision),
                },
                None,
            ));
        }

        // Locator/package/release differences are not scientific-identity
        // mismatches. Rebind to the verifier-owned current reference as one
        // dirty revision, without retaining an undo route to the persisted
        // non-current reference as a second live dataset authority.
        let rebound = ProjectState::new(
            state.project_id(),
            verified_source.clone(),
            state.view().clone(),
            state.channel_presets().to_vec(),
            state.artifacts().to_vec(),
        )
        .map_err(|_| ApplicationFaultCode::InvalidProjectTransition)?;
        let rebound_revision = high_water
            .allocate_after(saved_revision)
            .map_err(|_| ApplicationFaultCode::InvalidProjectTransition)?;
        Ok((
            Self {
                history: VecDeque::from([HistoryEntry {
                    revision: rebound_revision,
                    state: Arc::new(rebound),
                }]),
                cursor: 0,
                high_water,
                saved_revision: Some(saved_revision),
            },
            Some(rebound_revision),
        ))
    }

    fn replaced_by_durable_projection(projection: ProjectGenerationProjection) -> Self {
        let (revision, high_water, state) = projection.into_parts();
        Self {
            history: VecDeque::from([HistoryEntry {
                revision,
                state: Arc::new(state),
            }]),
            cursor: 0,
            high_water,
            saved_revision: Some(revision),
        }
    }

    fn recovered_for_verified_source(
        projection: ProjectGenerationProjection,
        verified_source: &DatasetReference,
        expected_project_id: Option<ProjectId>,
    ) -> Result<Self, ApplicationFaultCode> {
        let (recovered_revision, mut high_water, state) = projection.into_parts();
        if expected_project_id.is_some_and(|expected| state.project_id() != expected) {
            return Err(ApplicationFaultCode::InvalidProjectTransition);
        }
        if !state.dataset().has_same_scientific_content(verified_source) {
            return Err(ApplicationFaultCode::DatasetIdentityMismatch);
        }

        let (revision, state) = if state.dataset() == verified_source {
            (recovered_revision, state)
        } else {
            let rebound = ProjectState::new(
                state.project_id(),
                verified_source.clone(),
                state.view().clone(),
                state.channel_presets().to_vec(),
                state.artifacts().to_vec(),
            )
            .map_err(|_| ApplicationFaultCode::InvalidProjectTransition)?;
            let revision = high_water
                .allocate_after(recovered_revision)
                .map_err(|_| ApplicationFaultCode::InvalidProjectTransition)?;
            (revision, rebound)
        };

        Ok(Self {
            history: VecDeque::from([HistoryEntry {
                revision,
                state: Arc::new(state),
            }]),
            cursor: 0,
            high_water,
            // A selected recovery is an unsaved branch even when its dataset
            // reference, including its locator, is already exact.
            saved_revision: None,
        })
    }

    fn current(&self) -> &HistoryEntry {
        &self.history[self.cursor]
    }

    fn current_state(&self) -> &ProjectState {
        self.current().state.as_ref()
    }

    fn current_state_arc(&self) -> Arc<ProjectState> {
        Arc::clone(&self.current().state)
    }

    fn current_revision(&self) -> ProjectRevisionId {
        self.current().revision
    }

    fn dirty(&self) -> bool {
        self.saved_revision != Some(self.current_revision())
    }

    fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    fn can_redo(&self) -> bool {
        self.cursor + 1 < self.history.len()
    }

    fn push(&mut self, state: ProjectState) -> Result<ProjectRevisionId, ApplicationFaultCode> {
        let revision = self
            .high_water
            .allocate_after(self.current_revision())
            .map_err(|_| ApplicationFaultCode::InvalidProjectTransition)?;
        self.history.truncate(self.cursor + 1);
        self.history.push_back(HistoryEntry {
            revision,
            state: Arc::new(state),
        });
        self.cursor = self.history.len() - 1;
        if self.history.len() > MAX_HISTORY_ENTRIES {
            self.history.pop_front();
            self.cursor -= 1;
        }
        Ok(revision)
    }

    fn push_committed_projection(
        &mut self,
        projection: ProjectGenerationProjection,
    ) -> Result<ProjectRevisionId, ApplicationFaultCode> {
        let mut expected_high_water = self.high_water.clone();
        let expected_revision = expected_high_water
            .allocate_after(self.current_revision())
            .map_err(|_| ApplicationFaultCode::InvalidProjectTransition)?;
        if projection.revision() != expected_revision
            || projection.revision_high_water() != &expected_high_water
            || projection.state().project_id() != self.current_state().project_id()
        {
            return Err(ApplicationFaultCode::InvalidOperationCompletion);
        }

        let (revision, high_water, state) = projection.into_parts();
        self.history.truncate(self.cursor + 1);
        self.history.push_back(HistoryEntry {
            revision,
            state: Arc::new(state),
        });
        self.cursor = self.history.len() - 1;
        if self.history.len() > MAX_HISTORY_ENTRIES {
            self.history.pop_front();
            self.cursor -= 1;
        }
        self.high_water = high_water;
        self.saved_revision = Some(revision);
        Ok(revision)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Workspace {
    Unbound(Arc<UnboundWorkspace>),
    Bound(BoundWorkspace),
}

#[derive(Debug, Clone, PartialEq)]
struct ActiveOperation {
    token: OperationToken,
    retained_projection: Option<Arc<ProjectGenerationProjection>>,
    cancellation_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AnalysisBundleHandles {
    table: ArtifactHandleId,
    plot: Option<ArtifactHandleId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceVerificationProgressState {
    operation_id: OperationId,
    completed_work: u64,
    total_work: u64,
}

#[derive(Debug, PartialEq)]
pub struct ApplicationState {
    source_generation: SourceSessionGeneration,
    catalog: Arc<DatasetCatalog>,
    verified_source: Option<DatasetReference>,
    source_verification_progress: Option<SourceVerificationProgressState>,
    workspace: Workspace,
    transient: TransientApplicationState,
    analysis_table_catalog: BTreeMap<AnalysisTableId, AnalysisTableDescriptor>,
    analysis_plot_catalog: BTreeMap<AnalysisPlotId, AnalysisPlotDescriptor>,
    currentness: CurrentnessGeneration,
    operations: BTreeMap<OperationId, ActiveOperation>,
    next_operation_id: u64,
    next_task_id: u64,
    next_settings_change_id: u64,
    events: VecDeque<ApplicationEvent>,
    resource_policy: ResourcePolicy,
    pending_settings_change: Option<SettingsChangeToken>,
    latest_problem: Option<ApplicationEvent>,
}

impl ApplicationState {
    pub fn new_unbound(
        source_generation: SourceSessionGeneration,
        catalog: DatasetCatalog,
        workspace: UnboundWorkspace,
        resource_policy: ResourcePolicy,
    ) -> Result<Self, ApplicationFaultCode> {
        require_unverified_catalog(&catalog, source_generation)?;
        validate_view_against_catalog(&catalog, workspace.view())?;
        Ok(Self {
            source_generation,
            catalog: Arc::new(catalog),
            verified_source: None,
            source_verification_progress: None,
            workspace: Workspace::Unbound(Arc::new(workspace)),
            transient: TransientApplicationState::default(),
            analysis_table_catalog: BTreeMap::new(),
            analysis_plot_catalog: BTreeMap::new(),
            currentness: CurrentnessGeneration::initial(),
            operations: BTreeMap::new(),
            next_operation_id: 1,
            next_task_id: 1,
            next_settings_change_id: 1,
            events: VecDeque::new(),
            resource_policy,
            pending_settings_change: None,
            latest_problem: None,
        })
    }

    fn fork_for_dispatch(&self) -> Self {
        Self {
            source_generation: self.source_generation,
            catalog: Arc::clone(&self.catalog),
            verified_source: self.verified_source.clone(),
            source_verification_progress: self.source_verification_progress,
            workspace: self.workspace.clone(),
            transient: self.transient.clone(),
            analysis_table_catalog: self.analysis_table_catalog.clone(),
            analysis_plot_catalog: self.analysis_plot_catalog.clone(),
            currentness: self.currentness,
            operations: self.operations.clone(),
            next_operation_id: self.next_operation_id,
            next_task_id: self.next_task_id,
            next_settings_change_id: self.next_settings_change_id,
            events: self.events.clone(),
            resource_policy: self.resource_policy,
            pending_settings_change: self.pending_settings_change,
            latest_problem: self.latest_problem.clone(),
        }
    }

    /// Applies one command atomically. Rejected commands leave every field,
    /// including queues, counters, and revision high-water, unchanged.
    pub fn dispatch(
        &mut self,
        command: ApplicationCommand,
    ) -> Result<CommandEffect, ApplicationFault> {
        let command_kind = command.kind();
        let fault_token = match &command {
            ApplicationCommand::StageAnalysisBundle { token, .. }
            | ApplicationCommand::CompleteOperation { token, .. }
            | ApplicationCommand::UpdateSourceVerificationProgress { token, .. } => {
                Some(token.clone())
            }
            _ => None,
        };
        let mut candidate = self.fork_for_dispatch();
        match candidate.reduce(command, command_kind) {
            Ok(CommandEffect::NoChange) => Ok(CommandEffect::NoChange),
            Ok(CommandEffect::Changed) => {
                *self = candidate;
                Ok(CommandEffect::Changed)
            }
            Err(code) => Err(self.fault(code, command_kind, fault_token.as_ref())),
        }
    }

    pub fn snapshot(&self) -> ApplicationSnapshot {
        let workspace = match &self.workspace {
            Workspace::Unbound(workspace) => WorkspaceSnapshot::Unbound {
                workspace: Arc::clone(workspace),
            },
            Workspace::Bound(workspace) => WorkspaceSnapshot::Bound {
                project: workspace.current_state_arc(),
                revision: workspace.current_revision(),
                revision_high_water: workspace.high_water.clone(),
                saved_revision: workspace.saved_revision,
                dirty: workspace.dirty(),
                can_undo: workspace.can_undo(),
                can_redo: workspace.can_redo(),
                retained_history_entries: workspace.history.len(),
            },
        };
        let source = match self.source_verification_progress {
            Some(progress) => SourceVerificationSnapshot::Verifying {
                operation_id: progress.operation_id,
                completed_work: progress.completed_work,
                total_work: progress.total_work,
            },
            None => match &self.verified_source {
                Some(dataset) => SourceVerificationSnapshot::Verified(dataset.clone()),
                None => SourceVerificationSnapshot::Required,
            },
        };
        let operations = self
            .operations
            .values()
            .map(|operation| operation.token.clone())
            .collect();
        ApplicationSnapshot {
            source_generation: self.source_generation,
            catalog: Arc::clone(&self.catalog),
            source,
            workspace,
            transient: self.transient.clone(),
            currentness: self.currentness,
            active_operations: operations,
            resource_policy: self.resource_policy,
            pending_settings_change: self.pending_settings_change,
            pending_event_count: self.events.len(),
            latest_problem: self.latest_problem.clone(),
            presentations: PresentationSnapshot::default(),
            import_workflow: import_workflow::ImportWorkflowSnapshot::default(),
        }
    }

    pub fn drain_events(&mut self, limit: usize) -> Vec<ApplicationEvent> {
        let count = limit.min(self.events.len());
        self.events.drain(..count).collect()
    }

    fn reduce(
        &mut self,
        command: ApplicationCommand,
        command_kind: ApplicationCommandKind,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        let durable_project_freeze_active = self.operations.values().any(|operation| {
            operation.retained_projection.is_some()
                || matches!(
                    operation.token.kind,
                    OperationKind::DatasetOpen
                        | OperationKind::ProjectSaveAs
                        | OperationKind::ProjectRecovery
                )
        });
        if durable_project_freeze_active && command.mutates_durable_project() {
            return Err(ApplicationFaultCode::OperationConflict);
        }
        match command {
            ApplicationCommand::RequestDatasetOpen => self.request_dataset_open(),
            ApplicationCommand::RequestSourceVerification => self.request_source_verification(),
            ApplicationCommand::UpdateSourceVerificationProgress {
                token,
                completed_work,
                total_work,
            } => self.update_source_verification_progress(token, completed_work, total_work),
            ApplicationCommand::InvalidateSourceVerification { source_generation } => {
                self.invalidate_source_verification(source_generation)
            }
            ApplicationCommand::AttachVerifiedDataset => self.attach_verified_dataset(),
            ApplicationCommand::SetActiveLayer(layer) => self.update_view(command_kind, |view| {
                rebuild_view(view, ViewUpdate::Active(layer))
            }),
            ApplicationCommand::SetTimepoint(timepoint) => self.update_view(command_kind, |view| {
                rebuild_view(view, ViewUpdate::Timepoint(timepoint))
            }),
            ApplicationCommand::SetLayerView(layer) => self.update_view(command_kind, |view| {
                rebuild_view(view, ViewUpdate::Layer(layer))
            }),
            ApplicationCommand::ReplaceView(view) => self.update_view(command_kind, |_| Ok(view)),
            ApplicationCommand::SetCamera(camera) => self.update_view(command_kind, |view| {
                rebuild_view(view, ViewUpdate::Camera(camera))
            }),
            ApplicationCommand::SetLayout {
                layout,
                cross_section,
            } => self.update_view(command_kind, |view| {
                rebuild_view(view, ViewUpdate::Layout(layout, cross_section))
            }),
            ApplicationCommand::SetIsoLight(light) => self.update_view(command_kind, |view| {
                rebuild_view(view, ViewUpdate::IsoLight(light))
            }),
            ApplicationCommand::SetLayerOrder(order) => self.update_view(command_kind, |view| {
                view.with_layer_order(order)
                    .map_err(|_| ApplicationFaultCode::InvalidProjectTransition)
            }),
            ApplicationCommand::UpsertChannelPreset(preset) => {
                self.upsert_channel_preset(command_kind, preset)
            }
            ApplicationCommand::RemoveChannelPreset(id) => {
                self.remove_channel_preset(command_kind, &id)
            }
            ApplicationCommand::ApplyChannelPreset(id) => {
                self.apply_channel_preset(command_kind, &id)
            }
            ApplicationCommand::UpsertArtifact(artifact) => {
                self.upsert_artifact(command_kind, artifact)
            }
            ApplicationCommand::RemoveArtifact(id) => self.remove_artifact(command_kind, &id),
            ApplicationCommand::SetPlaybackActive(active) => self.set_playback_active(active),
            ApplicationCommand::AdvancePlaybackTick(tick) => {
                self.advance_playback_tick(command_kind, tick)
            }
            ApplicationCommand::SetActiveTool(tool) => self.set_active_tool(tool),
            ApplicationCommand::SelectChannelPreset(id) => self.select_channel_preset(id),
            ApplicationCommand::SelectArtifact(id) => self.select_artifact(id),
            ApplicationCommand::SetActiveCrossSectionPanel(panel) => {
                self.set_active_cross_section_panel(panel)
            }
            ApplicationCommand::SelectAnalysisTable(id) => self.select_analysis_table(id),
            ApplicationCommand::SelectAnalysisPlot(id) => self.select_analysis_plot(id),
            ApplicationCommand::SelectAnalysisPlotPoint(selection) => {
                self.select_analysis_plot_point(selection)
            }
            ApplicationCommand::Undo => self.move_history(command_kind, false),
            ApplicationCommand::Redo => self.move_history(command_kind, true),
            ApplicationCommand::RequestProjectOpen => self.request_project_open(),
            ApplicationCommand::RequestProjectSave => self.request_project_save(),
            ApplicationCommand::RequestProjectSaveAs { new_project_id } => {
                self.request_project_save_as(new_project_id)
            }
            ApplicationCommand::RequestProjectRecovery => self.request_project_recovery(),
            ApplicationCommand::BeginOperation(kind) => {
                if matches!(
                    kind,
                    OperationKind::DatasetOpen
                        | OperationKind::SourceVerification
                        | OperationKind::ProjectOpen
                        | OperationKind::ProjectSave
                        | OperationKind::ProjectSaveAs
                        | OperationKind::ProjectRecovery
                ) {
                    return Err(ApplicationFaultCode::InvalidProjectTransition);
                }
                self.begin_operation(kind).map(|_| CommandEffect::Changed)
            }
            ApplicationCommand::StageAnalysisBundle { token, artifacts } => {
                self.stage_analysis_bundle(token, artifacts)
            }
            ApplicationCommand::InstallLoadedAnalysisDescriptors {
                project_id,
                revision,
                currentness,
                bundles,
            } => {
                self.install_loaded_analysis_descriptors(project_id, revision, currentness, bundles)
            }
            ApplicationCommand::CompleteOperation { token, completion } => {
                self.complete_operation(command_kind, token, completion)
            }
            ApplicationCommand::CancelOperation(id) => self.cancel_operation(id),
            ApplicationCommand::RequestResourcePolicyChange {
                policy,
                rejected_file_disposition,
            } => self.request_resource_policy_change(policy, rejected_file_disposition),
            ApplicationCommand::CompleteResourcePolicyPersistence { token, outcome } => {
                self.complete_resource_policy_persistence(token, outcome)
            }
        }
    }

    fn admit_opened_source(
        &mut self,
        source_generation: SourceSessionGeneration,
        catalog: Arc<DatasetCatalog>,
        workspace: UnboundWorkspace,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        if source_generation.get() <= self.source_generation.get() {
            return Err(ApplicationFaultCode::SourceGenerationNotAdvanced);
        }
        require_unverified_catalog(&catalog, source_generation)?;
        validate_view_against_catalog(&catalog, workspace.view())?;
        let provisional_project_id = workspace.provisional_project_id();
        self.source_generation = source_generation;
        self.catalog = catalog;
        self.verified_source = None;
        self.source_verification_progress = None;
        self.workspace = Workspace::Unbound(Arc::new(workspace));
        self.transient = TransientApplicationState::default();
        self.analysis_table_catalog.clear();
        self.analysis_plot_catalog.clear();
        self.advance_currentness()?;
        self.push_event(ApplicationEvent::CurrentSourceReplaced {
            source_generation,
            provisional_project_id,
        })?;
        Ok(CommandEffect::Changed)
    }

    fn request_dataset_open(&mut self) -> Result<CommandEffect, ApplicationFaultCode> {
        if !self.operations.is_empty() {
            return Err(ApplicationFaultCode::OperationConflict);
        }
        let token = self.create_operation(OperationKind::DatasetOpen)?;
        self.push_event(ApplicationEvent::DatasetOpenRequested { token })?;
        Ok(CommandEffect::Changed)
    }

    fn request_source_verification(&mut self) -> Result<CommandEffect, ApplicationFaultCode> {
        if self.verified_source.is_some() {
            return Ok(CommandEffect::NoChange);
        }
        if self.source_verification_progress.is_some() || !self.operations.is_empty() {
            return Err(ApplicationFaultCode::OperationConflict);
        }
        require_unverified_catalog(&self.catalog, self.source_generation)?;
        let token = self.create_operation(OperationKind::SourceVerification)?;
        self.source_verification_progress = Some(SourceVerificationProgressState {
            operation_id: token.operation_id,
            completed_work: 0,
            total_work: 0,
        });
        self.push_event(ApplicationEvent::SourceVerificationRequested { token })?;
        Ok(CommandEffect::Changed)
    }

    fn update_source_verification_progress(
        &mut self,
        token: OperationToken,
        completed_work: u64,
        total_work: u64,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        self.validate_source_verification_token_current(&token)?;
        if total_work == 0 || completed_work > total_work {
            return Err(ApplicationFaultCode::InvalidOperationProgress);
        }
        let progress = self
            .source_verification_progress
            .as_mut()
            .ok_or(ApplicationFaultCode::InvalidOperationProgress)?;
        if progress.operation_id != token.operation_id
            || completed_work < progress.completed_work
            || (progress.total_work != 0 && progress.total_work != total_work)
        {
            return Err(ApplicationFaultCode::InvalidOperationProgress);
        }
        if progress.completed_work == completed_work && progress.total_work == total_work {
            return Ok(CommandEffect::NoChange);
        }
        progress.completed_work = completed_work;
        progress.total_work = total_work;
        self.push_event(ApplicationEvent::SourceVerificationProgress {
            token,
            completed_work,
            total_work,
        })?;
        Ok(CommandEffect::Changed)
    }

    fn invalidate_source_verification(
        &mut self,
        source_generation: SourceSessionGeneration,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        if source_generation != self.source_generation {
            return Err(ApplicationFaultCode::SourceSessionMismatch);
        }

        let retained_analysis_ids = self
            .operations
            .iter()
            .filter_map(|(operation_id, operation)| {
                (operation.token.kind == OperationKind::Analysis
                    && operation.retained_projection.is_some())
                .then_some(*operation_id)
            })
            .collect::<Vec<_>>();
        let mut cancellation_tokens = Vec::new();
        for operation_id in &retained_analysis_ids {
            let operation = self
                .operations
                .get_mut(operation_id)
                .expect("retained analysis operation was collected");
            if !operation.cancellation_requested {
                operation.cancellation_requested = true;
                cancellation_tokens.push(operation.token.clone());
            }
        }

        let cancelled_ids = self
            .operations
            .iter()
            .filter_map(|(operation_id, operation)| {
                matches!(
                    operation.token.kind,
                    OperationKind::DatasetOpen
                        | OperationKind::SourceVerification
                        | OperationKind::ProjectOpen
                        | OperationKind::ProjectSave
                        | OperationKind::ProjectSaveAs
                        | OperationKind::ProjectRecovery
                        | OperationKind::Analysis
                )
                .then_some(*operation_id)
                .filter(|operation_id| !retained_analysis_ids.contains(operation_id))
            })
            .collect::<Vec<_>>();
        let cancelled_operations = cancelled_ids
            .into_iter()
            .filter_map(|operation_id| self.operations.remove(&operation_id))
            .collect::<Vec<_>>();
        cancellation_tokens.extend(
            cancelled_operations
                .into_iter()
                .map(|operation| operation.token),
        );
        let was_verified = self.verified_source.take().is_some();
        if cancellation_tokens.is_empty() && !was_verified {
            return Ok(CommandEffect::NoChange);
        }

        self.catalog = Arc::new(catalog_with_identity(
            &self.catalog,
            ScientificIdentityStatus::Unverified(DatasetSourceId::new(source_generation.get())),
        )?);
        self.source_verification_progress = None;
        for token in cancellation_tokens {
            self.push_event(ApplicationEvent::OperationCancellationRequested { token })?;
        }
        self.advance_currentness()?;
        self.push_event(ApplicationEvent::SourceVerificationInvalidated { source_generation })?;
        Ok(CommandEffect::Changed)
    }

    fn attach_verified_dataset(&mut self) -> Result<CommandEffect, ApplicationFaultCode> {
        let dataset = self
            .verified_source
            .clone()
            .ok_or(ApplicationFaultCode::IdentityVerificationRequired)?;
        let Workspace::Unbound(unbound) = &self.workspace else {
            return Err(ApplicationFaultCode::WorkspaceAlreadyBound);
        };
        let project = ProjectState::new(
            unbound.provisional_project_id,
            dataset,
            unbound.view.clone(),
            unbound.channel_presets.clone(),
            Vec::new(),
        )
        .map_err(|_| ApplicationFaultCode::InvalidProjectTransition)?;
        let bound = BoundWorkspace::attached(project);
        let project_id = bound.current_state().project_id();
        let revision = bound.current_revision();
        self.advance_currentness()?;
        self.push_event(ApplicationEvent::ProjectAttached {
            project_id,
            revision,
        })?;
        self.workspace = Workspace::Bound(bound);
        self.normalize_transient_selections();
        Ok(CommandEffect::Changed)
    }

    fn update_view(
        &mut self,
        _command_kind: ApplicationCommandKind,
        update: impl FnOnce(&ViewState) -> Result<ViewState, ApplicationFaultCode>,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        match &self.workspace {
            Workspace::Unbound(unbound) => {
                let view = update(&unbound.view)?;
                validate_view_against_catalog(&self.catalog, &view)?;
                if view == unbound.view {
                    return Ok(CommandEffect::NoChange);
                }
                let mut next = unbound.as_ref().clone();
                next.view = view;
                validate_unbound_presets(&next.view, &next.channel_presets)?;
                self.advance_currentness()?;
                self.push_event(ApplicationEvent::WorkspaceChanged {
                    currentness: self.currentness,
                })?;
                self.workspace = Workspace::Unbound(Arc::new(next));
                self.normalize_transient_selections();
                Ok(CommandEffect::Changed)
            }
            Workspace::Bound(bound) => {
                let view = update(bound.current_state().view())?;
                validate_view_against_catalog(&self.catalog, &view)?;
                if &view == bound.current_state().view() {
                    return Ok(CommandEffect::NoChange);
                }
                let project = rebuild_project(bound.current_state(), Some(view), None, None)?;
                self.commit_project(project)
            }
        }
    }

    fn upsert_channel_preset(
        &mut self,
        _command_kind: ApplicationCommandKind,
        preset: ChannelPreset,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        let selected_id = preset.id().clone();
        let workspace_effect = match &self.workspace {
            Workspace::Unbound(unbound) => {
                let mut presets = unbound.channel_presets.clone();
                upsert_preset(&mut presets, preset);
                if presets == unbound.channel_presets {
                    CommandEffect::NoChange
                } else {
                    validate_unbound_presets(&unbound.view, &presets)?;
                    let mut next = unbound.as_ref().clone();
                    next.channel_presets = presets;
                    self.advance_currentness()?;
                    self.push_event(ApplicationEvent::WorkspaceChanged {
                        currentness: self.currentness,
                    })?;
                    self.workspace = Workspace::Unbound(Arc::new(next));
                    self.normalize_transient_selections();
                    CommandEffect::Changed
                }
            }
            Workspace::Bound(bound) => {
                let mut presets = bound.current_state().channel_presets().to_vec();
                upsert_preset(&mut presets, preset);
                if presets == bound.current_state().channel_presets() {
                    CommandEffect::NoChange
                } else {
                    let project =
                        rebuild_project(bound.current_state(), None, Some(presets), None)?;
                    self.commit_project(project)?
                }
            }
        };
        let selection_effect = self.select_channel_preset(Some(selected_id))?;
        Ok(combine_effects(workspace_effect, selection_effect))
    }

    fn remove_channel_preset(
        &mut self,
        _command_kind: ApplicationCommandKind,
        id: &ChannelPresetId,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        match &self.workspace {
            Workspace::Unbound(unbound) => {
                let mut presets = unbound.channel_presets.clone();
                let Some(index) = presets.iter().position(|preset| preset.id() == id) else {
                    return Err(ApplicationFaultCode::ChannelPresetNotFound);
                };
                presets.remove(index);
                let mut next = unbound.as_ref().clone();
                next.channel_presets = presets;
                self.advance_currentness()?;
                self.push_event(ApplicationEvent::WorkspaceChanged {
                    currentness: self.currentness,
                })?;
                self.workspace = Workspace::Unbound(Arc::new(next));
                self.normalize_transient_selections();
                Ok(CommandEffect::Changed)
            }
            Workspace::Bound(bound) => {
                let mut presets = bound.current_state().channel_presets().to_vec();
                let Some(index) = presets.iter().position(|preset| preset.id() == id) else {
                    return Err(ApplicationFaultCode::ChannelPresetNotFound);
                };
                presets.remove(index);
                let project = rebuild_project(bound.current_state(), None, Some(presets), None)?;
                self.commit_project(project)
            }
        }
    }

    fn apply_channel_preset(
        &mut self,
        command_kind: ApplicationCommandKind,
        id: &ChannelPresetId,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        let (view, preset) = match &self.workspace {
            Workspace::Unbound(workspace) => (
                workspace.view.clone(),
                workspace
                    .channel_presets
                    .iter()
                    .find(|preset| preset.id() == id)
                    .cloned(),
            ),
            Workspace::Bound(workspace) => (
                workspace.current_state().view().clone(),
                workspace.current_state().channel_preset(id).cloned(),
            ),
        };
        let preset = preset.ok_or(ApplicationFaultCode::ChannelPresetNotFound)?;
        let next_view = apply_preset_to_view(&view, &preset)?;
        let view_effect = self.update_view(command_kind, |_| Ok(next_view))?;
        let selection_effect = self.select_channel_preset(Some(id.clone()))?;
        Ok(combine_effects(view_effect, selection_effect))
    }

    fn upsert_artifact(
        &mut self,
        _command_kind: ApplicationCommandKind,
        artifact: ArtifactReference,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        let Workspace::Bound(bound) = &self.workspace else {
            return Err(ApplicationFaultCode::WorkspaceUnbound);
        };
        if is_analysis_schema(artifact.schema())
            || bound
                .current_state()
                .artifact(artifact.handle_id())
                .is_some_and(|existing| is_analysis_schema(existing.schema()))
        {
            return Err(ApplicationFaultCode::InvalidProjectTransition);
        }
        let mut artifacts = bound.current_state().artifacts().to_vec();
        upsert_artifact(&mut artifacts, artifact);
        if artifacts == bound.current_state().artifacts() {
            return Ok(CommandEffect::NoChange);
        }
        let project = rebuild_project(bound.current_state(), None, None, Some(artifacts))?;
        self.commit_project(project)
    }

    fn remove_artifact(
        &mut self,
        _command_kind: ApplicationCommandKind,
        id: &ArtifactHandleId,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        let Workspace::Bound(bound) = &self.workspace else {
            return Err(ApplicationFaultCode::WorkspaceUnbound);
        };
        let mut artifacts = bound.current_state().artifacts().to_vec();
        let Some(index) = artifacts
            .iter()
            .position(|artifact| artifact.handle_id() == id)
        else {
            return Err(ApplicationFaultCode::ArtifactNotFound);
        };
        if is_analysis_schema(artifacts[index].schema()) {
            return Err(ApplicationFaultCode::InvalidProjectTransition);
        }
        artifacts.remove(index);
        let project = rebuild_project(bound.current_state(), None, None, Some(artifacts))?;
        self.commit_project(project)
    }

    fn set_playback_active(&mut self, active: bool) -> Result<CommandEffect, ApplicationFaultCode> {
        let active = active && catalog_timepoint_count(&self.catalog) > 1;
        if self.transient.playback_active == active {
            return Ok(CommandEffect::NoChange);
        }
        self.transient.playback_active = active;
        self.transient.last_playback_tick = None;
        self.push_event(ApplicationEvent::TransientStateChanged)?;
        Ok(CommandEffect::Changed)
    }

    fn advance_playback_tick(
        &mut self,
        command_kind: ApplicationCommandKind,
        tick: u64,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        if !self.transient.playback_active {
            return Ok(CommandEffect::NoChange);
        }
        if self
            .transient
            .last_playback_tick
            .is_some_and(|last| tick <= last)
        {
            return Ok(CommandEffect::NoChange);
        }
        let Some(last_tick) = self.transient.last_playback_tick else {
            self.transient.last_playback_tick = Some(tick);
            self.push_event(ApplicationEvent::TransientStateChanged)?;
            return Ok(CommandEffect::Changed);
        };
        debug_assert!(tick > last_tick);

        let count = catalog_timepoint_count(&self.catalog);
        if count <= 1 {
            self.transient.playback_active = false;
            self.transient.last_playback_tick = None;
            self.push_event(ApplicationEvent::TransientStateChanged)?;
            return Ok(CommandEffect::Changed);
        }
        let current = match &self.workspace {
            Workspace::Unbound(workspace) => workspace.view.timepoint(),
            Workspace::Bound(workspace) => workspace.current_state().view().timepoint(),
        };
        let next = TimeIndex::new((current.get() + 1) % count);
        self.update_view(command_kind, |view| {
            rebuild_view(view, ViewUpdate::Timepoint(next))
        })?;
        self.transient.last_playback_tick = Some(tick);
        Ok(CommandEffect::Changed)
    }

    fn set_active_tool(&mut self, tool: ToolKind) -> Result<CommandEffect, ApplicationFaultCode> {
        if self.transient.active_tool == tool {
            return Ok(CommandEffect::NoChange);
        }
        self.transient.active_tool = tool;
        self.push_event(ApplicationEvent::TransientStateChanged)?;
        Ok(CommandEffect::Changed)
    }

    fn select_channel_preset(
        &mut self,
        id: Option<ChannelPresetId>,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        if let Some(id) = &id {
            let exists = match &self.workspace {
                Workspace::Unbound(workspace) => workspace
                    .channel_presets
                    .iter()
                    .any(|preset| preset.id() == id),
                Workspace::Bound(workspace) => {
                    workspace.current_state().channel_preset(id).is_some()
                }
            };
            if !exists {
                return Err(ApplicationFaultCode::ChannelPresetNotFound);
            }
        }
        if self.transient.selected_channel_preset == id {
            return Ok(CommandEffect::NoChange);
        }
        self.transient.selected_channel_preset = id;
        self.push_event(ApplicationEvent::TransientStateChanged)?;
        Ok(CommandEffect::Changed)
    }

    fn select_artifact(
        &mut self,
        id: Option<ArtifactHandleId>,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        if let Some(id) = &id {
            let Workspace::Bound(workspace) = &self.workspace else {
                return Err(ApplicationFaultCode::ArtifactNotFound);
            };
            if workspace.current_state().artifact(id).is_none() {
                return Err(ApplicationFaultCode::ArtifactNotFound);
            }
        }
        if self.transient.selected_artifact == id {
            return Ok(CommandEffect::NoChange);
        }
        self.transient.selected_artifact = id;
        self.push_event(ApplicationEvent::TransientStateChanged)?;
        Ok(CommandEffect::Changed)
    }

    fn set_active_cross_section_panel(
        &mut self,
        panel: Option<CrossSectionPanelId>,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        if self.transient.active_cross_section_panel == panel {
            return Ok(CommandEffect::NoChange);
        }
        self.transient.active_cross_section_panel = panel;
        self.push_event(ApplicationEvent::TransientStateChanged)?;
        Ok(CommandEffect::Changed)
    }

    fn select_analysis_table(
        &mut self,
        id: Option<AnalysisTableId>,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        if id.is_some_and(|id| {
            !self
                .transient
                .analysis_tables
                .iter()
                .any(|descriptor| descriptor.id == id)
        }) {
            return Err(ApplicationFaultCode::AnalysisTableNotFound);
        }
        if self.transient.selected_analysis_table == id {
            return Ok(CommandEffect::NoChange);
        }
        self.transient.selected_analysis_table = id;
        self.push_event(ApplicationEvent::TransientStateChanged)?;
        Ok(CommandEffect::Changed)
    }

    fn select_analysis_plot(
        &mut self,
        id: Option<AnalysisPlotId>,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        if id.is_some_and(|id| {
            !self
                .transient
                .analysis_plots
                .iter()
                .any(|descriptor| descriptor.id == id)
        }) {
            return Err(ApplicationFaultCode::AnalysisPlotNotFound);
        }
        if self.transient.selected_analysis_plot == id {
            return Ok(CommandEffect::NoChange);
        }
        self.transient.selected_analysis_plot = id;
        self.transient.selected_analysis_plot_point = None;
        self.push_event(ApplicationEvent::TransientStateChanged)?;
        Ok(CommandEffect::Changed)
    }

    fn select_analysis_plot_point(
        &mut self,
        selection: Option<AnalysisPlotPointSelection>,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        if let Some(selection) = selection {
            if self.transient.selected_analysis_plot != Some(selection.plot_id) {
                return Err(ApplicationFaultCode::AnalysisPlotNotFound);
            }
            let plot = self
                .transient
                .analysis_plots
                .iter()
                .find(|descriptor| descriptor.id == selection.plot_id)
                .ok_or(ApplicationFaultCode::AnalysisPlotNotFound)?;
            let point_count = plot
                .point_count(selection.series_index)
                .ok_or(ApplicationFaultCode::AnalysisPointOutOfBounds)?;
            if selection.point_index >= point_count {
                return Err(ApplicationFaultCode::AnalysisPointOutOfBounds);
            }
        }
        if self.transient.selected_analysis_plot_point == selection {
            return Ok(CommandEffect::NoChange);
        }
        self.transient.selected_analysis_plot_point = selection;
        self.push_event(ApplicationEvent::TransientStateChanged)?;
        Ok(CommandEffect::Changed)
    }

    fn normalize_transient_selections(&mut self) {
        let (analysis_tables, analysis_plots) = match &self.workspace {
            Workspace::Unbound(_) => (Vec::new(), Vec::new()),
            Workspace::Bound(workspace) => {
                let mut tables = Vec::new();
                let mut plots = Vec::new();
                for artifact in workspace.current_state().artifacts() {
                    match artifact.schema() {
                        ArtifactSchema::AnalysisTableV1 => {
                            let id = AnalysisTableId::from_artifact_handle(artifact.handle_id());
                            if let Some(descriptor) = self.analysis_table_catalog.get(&id) {
                                tables.push(descriptor.clone());
                            }
                        }
                        ArtifactSchema::AnalysisPlotV1 => {
                            let id = AnalysisPlotId::from_artifact_handle(artifact.handle_id());
                            if let Some(descriptor) = self.analysis_plot_catalog.get(&id) {
                                plots.push(descriptor.clone());
                            }
                        }
                        _ => {}
                    }
                }
                (tables, plots)
            }
        };
        self.transient.analysis_tables = Arc::from(analysis_tables);
        self.transient.analysis_plots = Arc::from(analysis_plots);

        let preset_exists = self
            .transient
            .selected_channel_preset
            .as_ref()
            .is_none_or(|id| match &self.workspace {
                Workspace::Unbound(workspace) => workspace
                    .channel_presets
                    .iter()
                    .any(|preset| preset.id() == id),
                Workspace::Bound(workspace) => {
                    workspace.current_state().channel_preset(id).is_some()
                }
            });
        if !preset_exists {
            self.transient.selected_channel_preset = None;
        }

        let artifact_exists =
            self.transient
                .selected_artifact
                .as_ref()
                .is_none_or(|id| match &self.workspace {
                    Workspace::Unbound(_) => false,
                    Workspace::Bound(workspace) => workspace.current_state().artifact(id).is_some(),
                });
        if !artifact_exists {
            self.transient.selected_artifact = None;
        }

        if self.transient.selected_analysis_table.is_some_and(|id| {
            !self
                .transient
                .analysis_tables
                .iter()
                .any(|descriptor| descriptor.id == id)
        }) {
            self.transient.selected_analysis_table = None;
        }
        if self.transient.selected_analysis_plot.is_some_and(|id| {
            !self
                .transient
                .analysis_plots
                .iter()
                .any(|descriptor| descriptor.id == id)
        }) {
            self.transient.selected_analysis_plot = None;
            self.transient.selected_analysis_plot_point = None;
        }
        if self
            .transient
            .selected_analysis_plot_point
            .is_some_and(|selection| {
                self.transient.selected_analysis_plot != Some(selection.plot_id)
                    || self
                        .transient
                        .analysis_plots
                        .iter()
                        .find(|descriptor| descriptor.id == selection.plot_id)
                        .and_then(|descriptor| descriptor.point_count(selection.series_index))
                        .is_none_or(|count| selection.point_index >= count)
            })
        {
            self.transient.selected_analysis_plot_point = None;
        }
    }

    fn commit_project(
        &mut self,
        project: ProjectState,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        let Workspace::Bound(bound) = &mut self.workspace else {
            return Err(ApplicationFaultCode::WorkspaceUnbound);
        };
        if &project == bound.current_state() {
            return Ok(CommandEffect::NoChange);
        }
        let revision = bound.push(project)?;
        let project_id = bound.current_state().project_id();
        let dirty = bound.dirty();
        self.normalize_transient_selections();
        self.advance_currentness()?;
        self.push_event(ApplicationEvent::ProjectRevisionChanged {
            project_id,
            revision,
            dirty,
        })?;
        Ok(CommandEffect::Changed)
    }

    fn move_history(
        &mut self,
        _command_kind: ApplicationCommandKind,
        redo: bool,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        let Workspace::Bound(bound) = &mut self.workspace else {
            return Err(ApplicationFaultCode::WorkspaceUnbound);
        };
        if redo {
            if !bound.can_redo() {
                return Err(ApplicationFaultCode::RedoUnavailable);
            }
            bound.cursor += 1;
        } else {
            if !bound.can_undo() {
                return Err(ApplicationFaultCode::UndoUnavailable);
            }
            bound.cursor -= 1;
        }
        let project_id = bound.current_state().project_id();
        let revision = bound.current_revision();
        let dirty = bound.dirty();
        self.normalize_transient_selections();
        self.advance_currentness()?;
        self.push_event(ApplicationEvent::ProjectRevisionChanged {
            project_id,
            revision,
            dirty,
        })?;
        Ok(CommandEffect::Changed)
    }

    fn request_project_open(&mut self) -> Result<CommandEffect, ApplicationFaultCode> {
        if !matches!(self.workspace, Workspace::Unbound(_)) {
            return Err(ApplicationFaultCode::WorkspaceAlreadyBound);
        }
        if self.verified_source.is_none() {
            return Err(ApplicationFaultCode::IdentityVerificationRequired);
        }
        if !self.operations.is_empty() {
            return Err(ApplicationFaultCode::OperationConflict);
        }
        let token = self.create_operation(OperationKind::ProjectOpen)?;
        self.push_event(ApplicationEvent::ProjectOpenRequested { token })?;
        Ok(CommandEffect::Changed)
    }

    fn request_project_save(&mut self) -> Result<CommandEffect, ApplicationFaultCode> {
        if self.verified_source.is_none() {
            return Err(ApplicationFaultCode::IdentityVerificationRequired);
        }
        if self.operations.values().any(|operation| {
            matches!(
                operation.token.kind,
                OperationKind::ProjectSave
                    | OperationKind::ProjectSaveAs
                    | OperationKind::ProjectRecovery
            )
        }) {
            return Err(ApplicationFaultCode::OperationConflict);
        }
        let Workspace::Bound(bound) = &self.workspace else {
            return Err(ApplicationFaultCode::IdentityVerificationRequired);
        };
        if !bound.dirty() {
            return Ok(CommandEffect::NoChange);
        }
        let projection = ProjectGenerationProjection::new(
            bound.current_revision(),
            bound.high_water.clone(),
            bound.current_state().clone(),
        )
        .map_err(|_| ApplicationFaultCode::InvalidProjectTransition)?;
        let token = self.create_operation(OperationKind::ProjectSave)?;
        self.push_event(ApplicationEvent::ProjectSaveRequested {
            token,
            projection: Arc::new(projection),
        })?;
        Ok(CommandEffect::Changed)
    }

    fn request_project_save_as(
        &mut self,
        new_project_id: ProjectId,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        let verified_source = self
            .verified_source
            .as_ref()
            .ok_or(ApplicationFaultCode::IdentityVerificationRequired)?;
        if !self.operations.is_empty() {
            return Err(ApplicationFaultCode::OperationConflict);
        }
        let Workspace::Bound(bound) = &self.workspace else {
            return Err(ApplicationFaultCode::WorkspaceUnbound);
        };
        if new_project_id == bound.current_state().project_id()
            || !bound
                .current_state()
                .dataset()
                .has_same_scientific_content(verified_source)
        {
            return Err(ApplicationFaultCode::InvalidProjectTransition);
        }
        let state = ProjectState::new(
            new_project_id,
            bound.current_state().dataset().clone(),
            bound.current_state().view().clone(),
            bound.current_state().channel_presets().to_vec(),
            bound.current_state().artifacts().to_vec(),
        )
        .map_err(|_| ApplicationFaultCode::InvalidProjectTransition)?;
        let projection = Arc::new(
            ProjectGenerationProjection::new(
                ProjectRevisionId::initial(new_project_id),
                ProjectRevisionHighWater::initial(new_project_id),
                state,
            )
            .map_err(|_| ApplicationFaultCode::InvalidProjectTransition)?,
        );
        let token = self.create_operation(OperationKind::ProjectSaveAs)?;
        let operation = self
            .operations
            .get_mut(&token.operation_id)
            .ok_or(ApplicationFaultCode::OperationNotFound)?;
        operation.token.target_project_id = Some(new_project_id);
        operation.retained_projection = Some(Arc::clone(&projection));
        let token = operation.token.clone();
        self.push_event(ApplicationEvent::ProjectSaveAsRequested { token, projection })?;
        Ok(CommandEffect::Changed)
    }

    fn request_project_recovery(&mut self) -> Result<CommandEffect, ApplicationFaultCode> {
        let verified_source = self
            .verified_source
            .as_ref()
            .ok_or(ApplicationFaultCode::IdentityVerificationRequired)?;
        if !self.operations.is_empty() {
            return Err(ApplicationFaultCode::OperationConflict);
        }
        if let Workspace::Bound(bound) = &self.workspace
            && !bound
                .current_state()
                .dataset()
                .has_same_scientific_content(verified_source)
        {
            return Err(ApplicationFaultCode::DatasetIdentityMismatch);
        }
        let token = self.create_operation(OperationKind::ProjectRecovery)?;
        self.push_event(ApplicationEvent::ProjectRecoveryRequested { token })?;
        Ok(CommandEffect::Changed)
    }

    fn begin_operation(
        &mut self,
        kind: OperationKind,
    ) -> Result<OperationToken, ApplicationFaultCode> {
        if kind == OperationKind::Import
            && self
                .operations
                .values()
                .any(|operation| operation.token.kind() == OperationKind::DatasetOpen)
        {
            return Err(ApplicationFaultCode::OperationConflict);
        }
        let token = self.create_operation(kind)?;
        self.push_event(ApplicationEvent::OperationStarted {
            token: token.clone(),
        })?;
        Ok(token)
    }

    fn create_operation(
        &mut self,
        kind: OperationKind,
    ) -> Result<OperationToken, ApplicationFaultCode> {
        if self.operations.len() >= MAX_ACTIVE_OPERATIONS {
            return Err(ApplicationFaultCode::OperationRegistryFull);
        }
        let operation_id = OperationId(self.next_operation_id);
        let task_id = TaskId(self.next_task_id);
        self.next_operation_id = self
            .next_operation_id
            .checked_add(1)
            .ok_or(ApplicationFaultCode::CounterOverflow)?;
        self.next_task_id = self
            .next_task_id
            .checked_add(1)
            .ok_or(ApplicationFaultCode::CounterOverflow)?;
        let (project_id, project_revision) = match &self.workspace {
            Workspace::Unbound(_) => (None, None),
            Workspace::Bound(bound) => (
                Some(bound.current_state().project_id()),
                Some(bound.current_revision()),
            ),
        };
        let token = OperationToken {
            operation_id,
            task_id,
            kind,
            source_identity: self
                .verified_source
                .as_ref()
                .map(|source| *source.scientific_content_id()),
            source_session_generation: self.source_generation,
            project_id,
            project_revision,
            target_project_id: None,
            currentness_generation: self.currentness,
        };
        self.operations.insert(
            operation_id,
            ActiveOperation {
                token: token.clone(),
                retained_projection: None,
                cancellation_requested: false,
            },
        );
        Ok(token)
    }

    fn stage_analysis_bundle(
        &mut self,
        token: OperationToken,
        artifacts: Vec<ArtifactReference>,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        let Some(active) = self.operations.get(&token.operation_id) else {
            return Err(ApplicationFaultCode::OperationNotFound);
        };
        if active.token != token {
            return Err(ApplicationFaultCode::OperationTokenMismatch);
        }
        if token.kind != OperationKind::Analysis || active.retained_projection.is_some() {
            return Err(ApplicationFaultCode::InvalidOperationCompletion);
        }
        self.validate_token_current(&token)?;
        if self.verified_source.is_none() {
            return Err(ApplicationFaultCode::IdentityVerificationRequired);
        }
        let Workspace::Bound(bound) = &self.workspace else {
            return Err(ApplicationFaultCode::WorkspaceUnbound);
        };
        let handles = validate_analysis_bundle(&artifacts)?;
        if artifacts.iter().any(|artifact| {
            bound
                .current_state()
                .artifact(artifact.handle_id())
                .is_some()
        }) {
            return Err(ApplicationFaultCode::InvalidOperationCompletion);
        }
        let table_id = AnalysisTableId::from_artifact_handle(&handles.table);
        let plot_id = handles
            .plot
            .as_ref()
            .map(AnalysisPlotId::from_artifact_handle);
        if (!self.analysis_table_catalog.contains_key(&table_id)
            && self.analysis_table_catalog.len() >= MAX_ANALYSIS_TABLES)
            || plot_id.is_some_and(|id| {
                !self.analysis_plot_catalog.contains_key(&id)
                    && self.analysis_plot_catalog.len() >= MAX_ANALYSIS_PLOTS
            })
        {
            return Err(ApplicationFaultCode::AnalysisRegistryFull);
        }

        let mut next_artifacts = bound.current_state().artifacts().to_vec();
        next_artifacts.extend(artifacts);
        next_artifacts.sort_by(|left, right| left.handle_id().cmp(right.handle_id()));
        let state = rebuild_project(bound.current_state(), None, None, Some(next_artifacts))?;
        let mut high_water = bound.high_water.clone();
        let revision = high_water
            .allocate_after(bound.current_revision())
            .map_err(|_| ApplicationFaultCode::InvalidProjectTransition)?;
        let projection = Arc::new(
            ProjectGenerationProjection::new(revision, high_water, state)
                .map_err(|_| ApplicationFaultCode::InvalidProjectTransition)?,
        );
        self.operations
            .get_mut(&token.operation_id)
            .expect("analysis operation was validated")
            .retained_projection = Some(Arc::clone(&projection));
        self.push_event(ApplicationEvent::AnalysisCommitRequested { token, projection })?;
        Ok(CommandEffect::Changed)
    }

    fn install_loaded_analysis_descriptors(
        &mut self,
        project_id: ProjectId,
        revision: ProjectRevisionId,
        currentness: CurrentnessGeneration,
        bundles: Vec<LoadedAnalysisDescriptorBundle>,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        let Workspace::Bound(bound) = &self.workspace else {
            return Err(ApplicationFaultCode::WorkspaceUnbound);
        };
        if bound.current_state().project_id() != project_id
            || bound.current_revision() != revision
            || self.currentness != currentness
        {
            return Err(ApplicationFaultCode::StaleOperationCompletion);
        }
        if bundles.is_empty() {
            return Ok(CommandEffect::NoChange);
        }

        let mut artifact_handles = BTreeSet::new();
        let mut table_ids = BTreeSet::new();
        let mut plot_ids = BTreeSet::new();
        for bundle in &bundles {
            let handles = validate_analysis_bundle(&bundle.artifacts)?;
            if bundle.artifacts.iter().any(|artifact| {
                bound.current_state().artifact(artifact.handle_id()) != Some(artifact)
                    || !artifact_handles.insert(artifact.handle_id().clone())
            }) || bundle.table.id != AnalysisTableId::from_artifact_handle(&handles.table)
                || !table_ids.insert(bundle.table.id)
                || match (&bundle.plot, &handles.plot) {
                    (Some(descriptor), Some(handle)) => {
                        descriptor.id != AnalysisPlotId::from_artifact_handle(handle)
                            || !plot_ids.insert(descriptor.id)
                    }
                    (None, None) => false,
                    _ => true,
                }
            {
                return Err(ApplicationFaultCode::InvalidOperationCompletion);
            }
            if self
                .analysis_table_catalog
                .get(&bundle.table.id)
                .is_some_and(|existing| existing != &bundle.table)
                || bundle.plot.as_ref().is_some_and(|descriptor| {
                    self.analysis_plot_catalog
                        .get(&descriptor.id)
                        .is_some_and(|existing| existing != descriptor)
                })
            {
                return Err(ApplicationFaultCode::InvalidOperationCompletion);
            }
        }

        let new_tables = table_ids
            .iter()
            .filter(|id| !self.analysis_table_catalog.contains_key(id))
            .count();
        let new_plots = plot_ids
            .iter()
            .filter(|id| !self.analysis_plot_catalog.contains_key(id))
            .count();
        if self.analysis_table_catalog.len().saturating_add(new_tables) > MAX_ANALYSIS_TABLES
            || self.analysis_plot_catalog.len().saturating_add(new_plots) > MAX_ANALYSIS_PLOTS
        {
            return Err(ApplicationFaultCode::AnalysisRegistryFull);
        }

        let selected_table = bundles.first().map(|bundle| bundle.table.id);
        let selected_plot = bundles
            .iter()
            .find_map(|bundle| bundle.plot.as_ref().map(AnalysisPlotDescriptor::id));
        for bundle in bundles {
            self.analysis_table_catalog
                .insert(bundle.table.id, bundle.table);
            if let Some(plot) = bundle.plot {
                self.analysis_plot_catalog.insert(plot.id, plot);
            }
        }
        self.normalize_transient_selections();
        if self.transient.selected_analysis_table.is_none() {
            self.transient.selected_analysis_table = selected_table;
        }
        if self.transient.selected_analysis_plot.is_none() {
            self.transient.selected_analysis_plot = selected_plot;
        }
        self.push_event(ApplicationEvent::TransientStateChanged)?;
        Ok(CommandEffect::Changed)
    }

    fn complete_operation(
        &mut self,
        _command_kind: ApplicationCommandKind,
        token: OperationToken,
        completion: OperationCompletion,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        let Some(active) = self.operations.get(&token.operation_id) else {
            return Err(ApplicationFaultCode::OperationNotFound);
        };
        if active.token != token {
            return Err(ApplicationFaultCode::OperationTokenMismatch);
        }
        let retires_stale_analysis = token.kind == OperationKind::Analysis
            && matches!(
                completion,
                OperationCompletion::Cancelled | OperationCompletion::Failed(_)
            );
        if retires_stale_analysis {
            if !completion_matches_kind(token.kind, &completion) {
                return Err(ApplicationFaultCode::InvalidOperationCompletion);
            }
        } else {
            if token.kind == OperationKind::Import {
                // Import reads an external TIFF source and publishes a new
                // package. Viewer/project edits do not make that result
                // stale, so only the exact active operation token above is
                // relevant to its terminal outcome.
            } else if token.kind == OperationKind::SourceVerification {
                self.validate_source_verification_token_current(&token)?;
            } else if token.kind == OperationKind::ProjectSave {
                self.validate_save_token_current(&token)?;
            } else {
                self.validate_token_current(&token)?;
            }
            if !completion_matches_kind(token.kind, &completion) {
                return Err(ApplicationFaultCode::InvalidOperationCompletion);
            }
        }
        let outcome = match completion {
            OperationCompletion::Succeeded => OperationOutcome::Succeeded,
            OperationCompletion::Cancelled => {
                if token.kind == OperationKind::SourceVerification {
                    self.source_verification_progress = None;
                }
                OperationOutcome::Cancelled
            }
            OperationCompletion::Failed(code) => {
                if token.kind == OperationKind::SourceVerification {
                    self.source_verification_progress = None;
                    if code == OperationFailureCode::SourceChanged {
                        let binding_changed = self.verified_source.take().is_some()
                            || self.catalog.scientific_identity().is_verified();
                        self.catalog = Arc::new(catalog_with_identity(
                            &self.catalog,
                            ScientificIdentityStatus::Unverified(DatasetSourceId::new(
                                self.source_generation.get(),
                            )),
                        )?);
                        if binding_changed {
                            self.advance_currentness()?;
                        }
                        self.push_event(ApplicationEvent::SourceVerificationInvalidated {
                            source_generation: self.source_generation,
                        })?;
                    }
                }
                OperationOutcome::Failed(code)
            }
            OperationCompletion::DatasetOpened {
                source_generation,
                catalog,
                workspace,
            } => {
                if token.kind != OperationKind::DatasetOpen {
                    return Err(ApplicationFaultCode::InvalidOperationCompletion);
                }
                self.admit_opened_source(source_generation, catalog, *workspace)?;
                OperationOutcome::DatasetOpened
            }
            OperationCompletion::SourceVerified {
                source_generation,
                catalog,
                dataset,
            } => {
                self.admit_verified_source(source_generation, catalog, dataset)?;
                OperationOutcome::SourceVerified
            }
            OperationCompletion::AnalysisCommitted {
                projection,
                table,
                plot,
            } => {
                self.admit_committed_analysis(&token, *projection, table, plot)?;
                OperationOutcome::AnalysisCommitted
            }
            OperationCompletion::ProjectOpened(projection) => {
                if token.kind != OperationKind::ProjectOpen {
                    return Err(ApplicationFaultCode::InvalidOperationCompletion);
                }
                let Workspace::Unbound(_) = self.workspace else {
                    return Err(ApplicationFaultCode::WorkspaceAlreadyBound);
                };
                let source = self
                    .verified_source
                    .as_ref()
                    .ok_or(ApplicationFaultCode::IdentityVerificationRequired)?;
                validate_view_against_catalog(&self.catalog, projection.state().view())?;
                let (bound, rebound_revision) =
                    BoundWorkspace::restored_for_verified_source(*projection, source)?;
                let project_id = bound.current_state().project_id();
                let dirty = bound.dirty();
                self.workspace = Workspace::Bound(bound);
                self.normalize_transient_selections();
                self.advance_currentness()?;
                if let Some(revision) = rebound_revision {
                    self.push_event(ApplicationEvent::ProjectRevisionChanged {
                        project_id,
                        revision,
                        dirty,
                    })?;
                }
                OperationOutcome::ProjectOpened
            }
            OperationCompletion::ProjectSaved(revision) => {
                if token.kind != OperationKind::ProjectSave
                    || token.project_revision != Some(revision)
                {
                    return Err(ApplicationFaultCode::InvalidOperationCompletion);
                }
                let Workspace::Bound(bound) = &mut self.workspace else {
                    return Err(ApplicationFaultCode::WorkspaceUnbound);
                };
                bound.saved_revision = Some(revision);
                self.push_event(ApplicationEvent::ProjectSaved { revision })?;
                OperationOutcome::ProjectSaved
            }
            OperationCompletion::ProjectSavedAs(projection) => {
                if token.kind != OperationKind::ProjectSaveAs {
                    return Err(ApplicationFaultCode::InvalidOperationCompletion);
                }
                let retained = self
                    .operations
                    .get(&token.operation_id)
                    .and_then(|operation| operation.retained_projection.as_ref())
                    .ok_or(ApplicationFaultCode::InvalidOperationCompletion)?;
                if retained.as_ref() != projection.as_ref() {
                    return Err(ApplicationFaultCode::InvalidOperationCompletion);
                }
                let source = self
                    .verified_source
                    .as_ref()
                    .ok_or(ApplicationFaultCode::IdentityVerificationRequired)?;
                if !projection
                    .state()
                    .dataset()
                    .has_same_scientific_content(source)
                {
                    return Err(ApplicationFaultCode::DatasetIdentityMismatch);
                }
                validate_view_against_catalog(&self.catalog, projection.state().view())?;
                let project_id = projection.state().project_id();
                let revision = projection.revision();
                self.workspace =
                    Workspace::Bound(BoundWorkspace::replaced_by_durable_projection(*projection));
                self.normalize_transient_selections();
                self.advance_currentness()?;
                self.push_event(ApplicationEvent::ProjectSavedAs {
                    project_id,
                    revision,
                })?;
                OperationOutcome::ProjectSavedAs
            }
            OperationCompletion::ProjectRecovered(projection) => {
                if !matches!(
                    token.kind,
                    OperationKind::ProjectOpen | OperationKind::ProjectRecovery
                ) {
                    return Err(ApplicationFaultCode::InvalidOperationCompletion);
                }
                let expected_project_id = match &self.workspace {
                    Workspace::Unbound(_) => None,
                    Workspace::Bound(current) => Some(current.current_state().project_id()),
                };
                let source = self
                    .verified_source
                    .as_ref()
                    .ok_or(ApplicationFaultCode::IdentityVerificationRequired)?;
                validate_view_against_catalog(&self.catalog, projection.state().view())?;
                let recovered = BoundWorkspace::recovered_for_verified_source(
                    *projection,
                    source,
                    expected_project_id,
                )?;
                let project_id = recovered.current_state().project_id();
                let revision = recovered.current_revision();
                self.workspace = Workspace::Bound(recovered);
                self.normalize_transient_selections();
                self.advance_currentness()?;
                self.push_event(ApplicationEvent::ProjectRecovered {
                    project_id,
                    revision,
                })?;
                OperationOutcome::ProjectRecovered
            }
        };
        self.operations.remove(&token.operation_id);
        self.push_event(ApplicationEvent::OperationCompleted { token, outcome })?;
        Ok(CommandEffect::Changed)
    }

    fn admit_verified_source(
        &mut self,
        source_generation: SourceSessionGeneration,
        catalog: Arc<DatasetCatalog>,
        dataset: DatasetReference,
    ) -> Result<(), ApplicationFaultCode> {
        if source_generation != self.source_generation {
            return Err(ApplicationFaultCode::SourceSessionMismatch);
        }
        let identity = *dataset.scientific_content_id();
        if catalog.scientific_identity().verified_id() != Some(&identity) {
            return Err(ApplicationFaultCode::DatasetIdentityMismatch);
        }
        let expected =
            catalog_with_identity(&self.catalog, ScientificIdentityStatus::Verified(identity))?;
        if catalog.as_ref() != &expected {
            return Err(ApplicationFaultCode::DatasetIdentityMismatch);
        }
        if let Workspace::Bound(bound) = &self.workspace
            && !bound
                .current_state()
                .dataset()
                .has_same_scientific_content(&dataset)
        {
            return Err(ApplicationFaultCode::DatasetIdentityMismatch);
        }

        self.catalog = catalog;
        self.verified_source = Some(dataset);
        self.source_verification_progress = None;
        self.advance_currentness()?;
        self.push_event(ApplicationEvent::SourceVerified {
            source_generation,
            scientific_content_id: identity,
        })?;
        Ok(())
    }

    fn admit_committed_analysis(
        &mut self,
        token: &OperationToken,
        projection: ProjectGenerationProjection,
        table: AnalysisTableDescriptor,
        plot: Option<AnalysisPlotDescriptor>,
    ) -> Result<(), ApplicationFaultCode> {
        if token.kind != OperationKind::Analysis {
            return Err(ApplicationFaultCode::InvalidOperationCompletion);
        }
        let retained = self
            .operations
            .get(&token.operation_id)
            .and_then(|operation| operation.retained_projection.as_ref())
            .ok_or(ApplicationFaultCode::InvalidOperationCompletion)?;
        if retained.as_ref() != &projection {
            return Err(ApplicationFaultCode::InvalidOperationCompletion);
        }
        let Workspace::Bound(bound) = &self.workspace else {
            return Err(ApplicationFaultCode::WorkspaceUnbound);
        };
        let staged_artifacts = projection
            .state()
            .artifacts()
            .iter()
            .filter(|artifact| {
                bound
                    .current_state()
                    .artifact(artifact.handle_id())
                    .is_none()
            })
            .cloned()
            .collect::<Vec<_>>();
        let handles = validate_analysis_bundle(&staged_artifacts)?;
        if table.id != AnalysisTableId::from_artifact_handle(&handles.table)
            || match (&plot, &handles.plot) {
                (Some(descriptor), Some(handle)) => {
                    descriptor.id != AnalysisPlotId::from_artifact_handle(handle)
                }
                (None, None) => false,
                _ => true,
            }
        {
            return Err(ApplicationFaultCode::InvalidOperationCompletion);
        }
        if (!self.analysis_table_catalog.contains_key(&table.id)
            && self.analysis_table_catalog.len() >= MAX_ANALYSIS_TABLES)
            || plot.as_ref().is_some_and(|descriptor| {
                !self.analysis_plot_catalog.contains_key(&descriptor.id)
                    && self.analysis_plot_catalog.len() >= MAX_ANALYSIS_PLOTS
            })
        {
            return Err(ApplicationFaultCode::AnalysisRegistryFull);
        }

        let table_id = table.id;
        let plot_id = plot.as_ref().map(AnalysisPlotDescriptor::id);
        let (revision, project_id, dirty) = {
            let Workspace::Bound(bound) = &mut self.workspace else {
                return Err(ApplicationFaultCode::WorkspaceUnbound);
            };
            let revision = bound.push_committed_projection(projection)?;
            (revision, bound.current_state().project_id(), bound.dirty())
        };
        self.analysis_table_catalog.insert(table_id, table);
        if let Some(plot) = plot {
            self.analysis_plot_catalog.insert(plot.id, plot);
        }
        self.normalize_transient_selections();
        self.transient.selected_analysis_table = Some(table_id);
        if let Some(plot_id) = plot_id {
            self.transient.selected_analysis_plot = Some(plot_id);
            self.transient.selected_analysis_plot_point = None;
        }
        self.advance_currentness()?;
        self.push_event(ApplicationEvent::ProjectRevisionChanged {
            project_id,
            revision,
            dirty,
        })?;
        self.push_event(ApplicationEvent::TransientStateChanged)?;
        Ok(())
    }

    fn cancel_operation(
        &mut self,
        operation_id: OperationId,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        if let Some(operation) = self.operations.get_mut(&operation_id)
            && operation.token.kind == OperationKind::Analysis
            && operation.retained_projection.is_some()
        {
            if operation.cancellation_requested {
                return Ok(CommandEffect::NoChange);
            }
            operation.cancellation_requested = true;
            let token = operation.token.clone();
            self.push_event(ApplicationEvent::OperationCancellationRequested { token })?;
            return Ok(CommandEffect::Changed);
        }
        let operation = self
            .operations
            .remove(&operation_id)
            .ok_or(ApplicationFaultCode::OperationNotFound)?;
        if operation.token.kind == OperationKind::SourceVerification {
            self.source_verification_progress = None;
        }
        self.push_event(ApplicationEvent::OperationCancellationRequested {
            token: operation.token,
        })?;
        Ok(CommandEffect::Changed)
    }

    fn validate_source_verification_token_current(
        &self,
        token: &OperationToken,
    ) -> Result<(), ApplicationFaultCode> {
        let Some(active) = self.operations.get(&token.operation_id) else {
            return Err(ApplicationFaultCode::OperationNotFound);
        };
        if active.token != *token {
            return Err(ApplicationFaultCode::OperationTokenMismatch);
        }
        if token.kind != OperationKind::SourceVerification
            || token.source_session_generation != self.source_generation
        {
            return Err(ApplicationFaultCode::StaleOperationCompletion);
        }
        Ok(())
    }

    fn validate_token_current(&self, token: &OperationToken) -> Result<(), ApplicationFaultCode> {
        let source_identity = self
            .verified_source
            .as_ref()
            .map(|source| *source.scientific_content_id());
        let (project_id, project_revision) = match &self.workspace {
            Workspace::Unbound(_) => (None, None),
            Workspace::Bound(bound) => (
                Some(bound.current_state().project_id()),
                Some(bound.current_revision()),
            ),
        };
        if token.source_session_generation != self.source_generation
            || token.source_identity != source_identity
            || token.project_id != project_id
            || token.project_revision != project_revision
            || token.currentness_generation != self.currentness
        {
            return Err(ApplicationFaultCode::StaleOperationCompletion);
        }
        Ok(())
    }

    fn validate_save_token_current(
        &self,
        token: &OperationToken,
    ) -> Result<(), ApplicationFaultCode> {
        let source_identity = self
            .verified_source
            .as_ref()
            .map(|source| *source.scientific_content_id());
        let Workspace::Bound(bound) = &self.workspace else {
            return Err(ApplicationFaultCode::StaleOperationCompletion);
        };
        if token.kind != OperationKind::ProjectSave
            || token.source_session_generation != self.source_generation
            || token.source_identity != source_identity
            || token.project_id != Some(bound.current_state().project_id())
        {
            return Err(ApplicationFaultCode::StaleOperationCompletion);
        }
        Ok(())
    }

    fn request_resource_policy_change(
        &mut self,
        policy: ResourcePolicy,
        rejected_file_disposition: RejectedFileDisposition,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        if let Some(pending) = self.pending_settings_change {
            return if pending.policy == policy
                && pending.rejected_file_disposition == rejected_file_disposition
            {
                Ok(CommandEffect::NoChange)
            } else {
                Err(ApplicationFaultCode::ResourcePolicyChangePending)
            };
        }
        // Persistence is part of this command: the active policy may still
        // need to create a missing settings document.
        let token = SettingsChangeToken {
            id: SettingsChangeId(self.next_settings_change_id),
            policy,
            rejected_file_disposition,
        };
        self.next_settings_change_id = self
            .next_settings_change_id
            .checked_add(1)
            .ok_or(ApplicationFaultCode::CounterOverflow)?;
        self.push_event(ApplicationEvent::ResourcePolicyChangePending { token })?;
        self.pending_settings_change = Some(token);
        Ok(CommandEffect::Changed)
    }

    fn complete_resource_policy_persistence(
        &mut self,
        token: SettingsChangeToken,
        outcome: ResourcePolicyPersistenceOutcome,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        if self.pending_settings_change != Some(token) {
            return Err(ApplicationFaultCode::ResourcePolicyCompletionMismatch);
        }
        match outcome {
            ResourcePolicyPersistenceOutcome::Persisted => {
                self.resource_policy = token.policy;
                self.pending_settings_change = None;
                self.push_event(ApplicationEvent::ResourcePolicyPersisted {
                    token,
                    restart_required: true,
                })?;
            }
            ResourcePolicyPersistenceOutcome::Rejected(reason) => {
                self.pending_settings_change = None;
                self.push_event(ApplicationEvent::ResourcePolicyRejected { token, reason })?;
            }
        }
        Ok(CommandEffect::Changed)
    }

    fn advance_currentness(&mut self) -> Result<(), ApplicationFaultCode> {
        self.currentness.0 = self
            .currentness
            .0
            .checked_add(1)
            .ok_or(ApplicationFaultCode::CounterOverflow)?;
        Ok(())
    }

    fn push_event(&mut self, event: ApplicationEvent) -> Result<(), ApplicationFaultCode> {
        if self.events.len() >= MAX_PENDING_EVENTS {
            return Err(ApplicationFaultCode::EventQueueFull);
        }
        match &event {
            ApplicationEvent::OperationCompleted {
                outcome: OperationOutcome::Failed(_),
                ..
            }
            | ApplicationEvent::ResourcePolicyRejected { .. } => {
                self.latest_problem = Some(event.clone());
            }
            ApplicationEvent::OperationStarted { .. }
            | ApplicationEvent::DatasetOpenRequested { .. }
            | ApplicationEvent::SourceVerificationRequested { .. }
            | ApplicationEvent::ProjectOpenRequested { .. }
            | ApplicationEvent::ProjectSaveRequested { .. }
            | ApplicationEvent::ProjectSaveAsRequested { .. }
            | ApplicationEvent::ProjectRecoveryRequested { .. }
            | ApplicationEvent::ResourcePolicyChangePending { .. } => {
                self.latest_problem = None;
            }
            _ => {}
        }
        self.events.push_back(event);
        Ok(())
    }

    fn fault(
        &self,
        code: ApplicationFaultCode,
        command: ApplicationCommandKind,
        token: Option<&OperationToken>,
    ) -> ApplicationFault {
        ApplicationFault {
            code,
            command,
            operation_id: token.map(OperationToken::operation_id),
            task_id: token.map(OperationToken::task_id),
            source_generation: self.source_generation,
            project_id: match &self.workspace {
                Workspace::Unbound(_) => None,
                Workspace::Bound(bound) => Some(bound.current_state().project_id()),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceVerificationSnapshot {
    Required,
    Verifying {
        operation_id: OperationId,
        completed_work: u64,
        total_work: u64,
    },
    Verified(DatasetReference),
}

#[derive(Debug, Clone, PartialEq)]
pub enum WorkspaceSnapshot {
    Unbound {
        workspace: Arc<UnboundWorkspace>,
    },
    Bound {
        project: Arc<ProjectState>,
        revision: ProjectRevisionId,
        revision_high_water: ProjectRevisionHighWater,
        saved_revision: Option<ProjectRevisionId>,
        dirty: bool,
        can_undo: bool,
        can_redo: bool,
        retained_history_entries: usize,
    },
}

/// One of the viewer's fixed presentation surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PresentationSlot {
    ThreeD,
    Xy,
    Xz,
    Yz,
}

impl PresentationSlot {
    pub const ALL: [Self; 4] = [Self::ThreeD, Self::Xy, Self::Xz, Self::Yz];

    pub const fn is_cross_section(self) -> bool {
        !matches!(self, Self::ThreeD)
    }

    const fn index(self) -> usize {
        match self {
            Self::ThreeD => 0,
            Self::Xy => 1,
            Self::Xz => 2,
            Self::Yz => 3,
        }
    }
}

/// Backend-neutral facts needed to paint one viewer surface.
#[derive(Debug, Clone, PartialEq)]
pub struct PresentationSurface {
    viewport: PresentationViewport,
    frame: Option<PresentedFrame>,
}

impl PresentationSurface {
    pub const fn new(viewport: PresentationViewport, frame: Option<PresentedFrame>) -> Self {
        Self { viewport, frame }
    }

    pub const fn viewport(&self) -> PresentationViewport {
        self.viewport
    }

    pub const fn frame(&self) -> Option<&PresentedFrame> {
        self.frame.as_ref()
    }

    pub fn paint_request(&self) -> Option<PresentationPaintRequest> {
        self.frame
            .as_ref()
            .map(|frame| PresentationPaintRequest::new(frame.token(), self.viewport))
    }
}

/// The fixed 3D and linked cross-section presentation projection.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PresentationSnapshot {
    surfaces: [Option<PresentationSurface>; 4],
}

impl PresentationSnapshot {
    /// Constructs the four fixed slots directly, so a slot cannot appear
    /// twice in one snapshot.
    pub const fn new(
        three_d: Option<PresentationSurface>,
        xy: Option<PresentationSurface>,
        xz: Option<PresentationSurface>,
        yz: Option<PresentationSurface>,
    ) -> Self {
        Self {
            surfaces: [three_d, xy, xz, yz],
        }
    }

    pub fn get(&self, slot: PresentationSlot) -> Option<&PresentationSurface> {
        self.surfaces[slot.index()].as_ref()
    }

    pub fn iter(&self) -> impl Iterator<Item = (PresentationSlot, &PresentationSurface)> {
        PresentationSlot::ALL
            .into_iter()
            .filter_map(|slot| self.get(slot).map(|surface| (slot, surface)))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApplicationSnapshot {
    source_generation: SourceSessionGeneration,
    catalog: Arc<DatasetCatalog>,
    source: SourceVerificationSnapshot,
    workspace: WorkspaceSnapshot,
    transient: TransientApplicationState,
    currentness: CurrentnessGeneration,
    active_operations: Vec<OperationToken>,
    resource_policy: ResourcePolicy,
    pending_settings_change: Option<SettingsChangeToken>,
    pending_event_count: usize,
    latest_problem: Option<ApplicationEvent>,
    presentations: PresentationSnapshot,
    import_workflow: import_workflow::ImportWorkflowSnapshot,
}

impl ApplicationSnapshot {
    pub const fn source_generation(&self) -> SourceSessionGeneration {
        self.source_generation
    }

    pub fn catalog(&self) -> &Arc<DatasetCatalog> {
        &self.catalog
    }

    pub fn source(&self) -> &SourceVerificationSnapshot {
        &self.source
    }

    pub fn workspace(&self) -> &WorkspaceSnapshot {
        &self.workspace
    }

    pub fn transient(&self) -> &TransientApplicationState {
        &self.transient
    }

    pub const fn currentness(&self) -> CurrentnessGeneration {
        self.currentness
    }

    pub fn active_operations(&self) -> &[OperationToken] {
        &self.active_operations
    }

    pub const fn resource_policy(&self) -> ResourcePolicy {
        self.resource_policy
    }

    pub const fn pending_settings_change(&self) -> Option<SettingsChangeToken> {
        self.pending_settings_change
    }

    pub fn pending_resource_policy(&self) -> Option<ResourcePolicy> {
        self.pending_settings_change
            .map(SettingsChangeToken::policy)
    }

    pub const fn pending_event_count(&self) -> usize {
        self.pending_event_count
    }

    /// Latest typed operation/settings problem retained after the event queue
    /// has been drained by composition services. A new retry clears it.
    pub fn latest_problem(&self) -> Option<&ApplicationEvent> {
        self.latest_problem.as_ref()
    }

    /// Returns the backend-neutral projection of the viewer's fixed surfaces.
    pub const fn presentations(&self) -> &PresentationSnapshot {
        &self.presentations
    }

    /// Returns native import facts projected for framework UI code.
    pub const fn import_workflow(&self) -> &import_workflow::ImportWorkflowSnapshot {
        &self.import_workflow
    }

    /// Composition attaches the current presentation projection after taking
    /// an immutable application snapshot. This does not mutate application or
    /// durable project state.
    pub fn with_presentations(mut self, presentations: PresentationSnapshot) -> Self {
        self.presentations = presentations;
        self
    }

    /// Composition attaches native import facts without mutating canonical state.
    pub fn with_import_workflow(
        mut self,
        workflow: import_workflow::ImportWorkflowSnapshot,
    ) -> Self {
        self.import_workflow = workflow;
        self
    }

    pub const fn is_bound(&self) -> bool {
        matches!(self.workspace, WorkspaceSnapshot::Bound { .. })
    }

    pub const fn dirty(&self) -> Option<bool> {
        match self.workspace {
            WorkspaceSnapshot::Unbound { .. } => None,
            WorkspaceSnapshot::Bound { dirty, .. } => Some(dirty),
        }
    }
}

enum ViewUpdate {
    Active(LogicalLayerKey),
    Timepoint(TimeIndex),
    Layer(LayerViewState),
    Camera(CameraView),
    Layout(ViewerLayout, CrossSectionView),
    IsoLight(IsoLightState),
}

fn rebuild_view(view: &ViewState, update: ViewUpdate) -> Result<ViewState, ApplicationFaultCode> {
    let mut layers = view.layers().to_vec();
    let mut active = view.active_layer();
    let mut timepoint = view.timepoint();
    let mut camera = *view.camera();
    let mut layout = view.layout();
    let mut cross_section = *view.cross_section();
    let mut iso_light = *view.iso_light();
    match update {
        ViewUpdate::Active(value) => {
            if view.layer(value).is_none() {
                return Err(ApplicationFaultCode::LayerNotFound);
            }
            active = value;
        }
        ViewUpdate::Timepoint(value) => timepoint = value,
        ViewUpdate::Layer(value) => {
            let Some(index) = layers
                .iter()
                .position(|layer| layer.layer_key() == value.layer_key())
            else {
                return Err(ApplicationFaultCode::LayerNotFound);
            };
            layers[index] = value;
        }
        ViewUpdate::Camera(value) => camera = value,
        ViewUpdate::Layout(next_layout, next_cross_section) => {
            layout = next_layout;
            cross_section = next_cross_section;
        }
        ViewUpdate::IsoLight(value) => iso_light = value,
    }
    ViewState::new(
        layers,
        active,
        timepoint,
        camera,
        layout,
        cross_section,
        iso_light,
    )
    .map_err(|_| ApplicationFaultCode::InvalidProjectTransition)
}

fn rebuild_project(
    project: &ProjectState,
    view: Option<ViewState>,
    presets: Option<Vec<ChannelPreset>>,
    artifacts: Option<Vec<ArtifactReference>>,
) -> Result<ProjectState, ApplicationFaultCode> {
    ProjectState::new(
        project.project_id(),
        project.dataset().clone(),
        view.unwrap_or_else(|| project.view().clone()),
        presets.unwrap_or_else(|| project.channel_presets().to_vec()),
        artifacts.unwrap_or_else(|| project.artifacts().to_vec()),
    )
    .map_err(|_| ApplicationFaultCode::InvalidProjectTransition)
}

fn validate_view_against_catalog(
    catalog: &DatasetCatalog,
    view: &ViewState,
) -> Result<(), ApplicationFaultCode> {
    if view.layers().len() != catalog.len()
        || view
            .layers()
            .iter()
            .any(|layer| catalog.layer(layer.layer_key()).is_none())
        || catalog
            .layers()
            .any(|layer| view.layer(layer.key()).is_none())
    {
        return Err(ApplicationFaultCode::DatasetLayerClosureMismatch);
    }
    if catalog
        .layers()
        .any(|layer| view.timepoint().get() >= layer.shape().t())
    {
        return Err(ApplicationFaultCode::TimepointOutOfBounds);
    }
    Ok(())
}

fn require_unverified_catalog(
    catalog: &DatasetCatalog,
    source_generation: SourceSessionGeneration,
) -> Result<(), ApplicationFaultCode> {
    match catalog.scientific_identity() {
        ScientificIdentityStatus::Unverified(source_id)
            if source_id.get() == source_generation.get() =>
        {
            Ok(())
        }
        ScientificIdentityStatus::Unverified(_) | ScientificIdentityStatus::Verified(_) => {
            Err(ApplicationFaultCode::DatasetIdentityMismatch)
        }
    }
}

fn catalog_with_identity(
    catalog: &DatasetCatalog,
    scientific_identity: ScientificIdentityStatus,
) -> Result<DatasetCatalog, ApplicationFaultCode> {
    DatasetCatalog::new(
        catalog.label(),
        scientific_identity,
        catalog.layers().cloned().collect(),
    )
    .map_err(|_| ApplicationFaultCode::InvalidProjectTransition)
}

fn catalog_timepoint_count(catalog: &DatasetCatalog) -> u64 {
    catalog
        .layers()
        .map(|layer| layer.shape().t())
        .min()
        .expect("DatasetCatalog is non-empty by construction")
}

fn apply_preset_to_view(
    view: &ViewState,
    preset: &ChannelPreset,
) -> Result<ViewState, ApplicationFaultCode> {
    let layers = view
        .layers()
        .iter()
        .map(|layer| {
            let entry = preset
                .entry(layer.layer_key())
                .ok_or(ApplicationFaultCode::InvalidProjectTransition)?;
            Ok(LayerViewState::new(
                layer.layer_key(),
                entry.visible(),
                entry.transfer().clone(),
                *entry.render_state(),
            ))
        })
        .collect::<Result<Vec<_>, ApplicationFaultCode>>()?;
    ViewState::new(
        layers,
        view.active_layer(),
        view.timepoint(),
        *view.camera(),
        view.layout(),
        *view.cross_section(),
        *view.iso_light(),
    )
    .map_err(|_| ApplicationFaultCode::InvalidProjectTransition)
}

const fn combine_effects(left: CommandEffect, right: CommandEffect) -> CommandEffect {
    if matches!(left, CommandEffect::Changed) || matches!(right, CommandEffect::Changed) {
        CommandEffect::Changed
    } else {
        CommandEffect::NoChange
    }
}

fn validate_unbound_presets(
    view: &ViewState,
    presets: &[ChannelPreset],
) -> Result<(), ApplicationFaultCode> {
    if presets.len() > MAX_CHANNEL_PRESETS
        || presets
            .iter()
            .map(|preset| preset.entries().len())
            .sum::<usize>()
            > MAX_TOTAL_CHANNEL_PRESET_ENTRIES
    {
        return Err(ApplicationFaultCode::InvalidProjectTransition);
    }
    let layer_keys = view
        .layers()
        .iter()
        .map(LayerViewState::layer_key)
        .collect::<BTreeSet<_>>();
    let mut preset_ids = BTreeSet::new();
    for preset in presets {
        if !preset_ids.insert(preset.id().clone()) {
            return Err(ApplicationFaultCode::InvalidProjectTransition);
        }
        let preset_keys = preset
            .entries()
            .iter()
            .map(|entry| entry.layer_key())
            .collect::<BTreeSet<_>>();
        if preset.entries().len() != layer_keys.len() || preset_keys != layer_keys {
            return Err(ApplicationFaultCode::InvalidProjectTransition);
        }
    }
    Ok(())
}

fn upsert_preset(presets: &mut Vec<ChannelPreset>, preset: ChannelPreset) {
    if let Some(index) = presets
        .iter()
        .position(|existing| existing.id() == preset.id())
    {
        presets[index] = preset;
    } else {
        presets.push(preset);
    }
    presets.sort_by(|left, right| left.id().cmp(right.id()));
}

fn upsert_artifact(artifacts: &mut Vec<ArtifactReference>, artifact: ArtifactReference) {
    if let Some(index) = artifacts
        .iter()
        .position(|existing| existing.handle_id() == artifact.handle_id())
    {
        artifacts[index] = artifact;
    } else {
        artifacts.push(artifact);
    }
    artifacts.sort_by(|left, right| left.handle_id().cmp(right.handle_id()));
}

const fn is_analysis_schema(schema: ArtifactSchema) -> bool {
    matches!(
        schema,
        ArtifactSchema::AnalysisTableV1 | ArtifactSchema::AnalysisPlotV1
    )
}

fn validate_analysis_bundle(
    artifacts: &[ArtifactReference],
) -> Result<AnalysisBundleHandles, ApplicationFaultCode> {
    let Some(first) = artifacts.first() else {
        return Err(ApplicationFaultCode::InvalidOperationCompletion);
    };
    let Some(recipe_id) = first.recipe_id() else {
        return Err(ApplicationFaultCode::InvalidOperationCompletion);
    };
    let Some(derivation_id) = first.derivation_id() else {
        return Err(ApplicationFaultCode::InvalidOperationCompletion);
    };
    if first.source_layers().is_empty() {
        return Err(ApplicationFaultCode::InvalidOperationCompletion);
    }

    let mut handles = BTreeSet::new();
    let mut table = None;
    let mut plot = None;
    for artifact in artifacts {
        if artifact.completeness() != ArtifactCompleteness::Complete
            || artifact.recoverability() != ArtifactRecoverability::Regenerable
            || artifact.recipe_id() != Some(recipe_id)
            || artifact.derivation_id() != Some(derivation_id)
            || artifact.source_layers() != first.source_layers()
            || !handles.insert(artifact.handle_id().clone())
        {
            return Err(ApplicationFaultCode::InvalidOperationCompletion);
        }
        match artifact.schema() {
            ArtifactSchema::AnalysisTableV1 if table.is_none() => {
                table = Some(artifact.handle_id().clone());
            }
            ArtifactSchema::AnalysisPlotV1 if plot.is_none() => {
                plot = Some(artifact.handle_id().clone());
            }
            _ => return Err(ApplicationFaultCode::InvalidOperationCompletion),
        }
    }

    Ok(AnalysisBundleHandles {
        table: table.ok_or(ApplicationFaultCode::InvalidOperationCompletion)?,
        plot,
    })
}

fn completion_matches_kind(kind: OperationKind, completion: &OperationCompletion) -> bool {
    if let OperationCompletion::Failed(code) = completion {
        return failure_code_matches_kind(kind, *code);
    }
    match kind {
        OperationKind::DatasetOpen => matches!(
            completion,
            OperationCompletion::DatasetOpened { .. } | OperationCompletion::Cancelled
        ),
        OperationKind::SourceVerification => matches!(
            completion,
            OperationCompletion::SourceVerified { .. } | OperationCompletion::Cancelled
        ),
        OperationKind::ProjectOpen => matches!(
            completion,
            OperationCompletion::ProjectOpened(_)
                | OperationCompletion::ProjectRecovered(_)
                | OperationCompletion::Cancelled
        ),
        OperationKind::ProjectSave => matches!(
            completion,
            OperationCompletion::ProjectSaved(_) | OperationCompletion::Cancelled
        ),
        OperationKind::ProjectSaveAs => matches!(
            completion,
            OperationCompletion::ProjectSavedAs(_) | OperationCompletion::Cancelled
        ),
        OperationKind::ProjectRecovery => matches!(
            completion,
            OperationCompletion::ProjectRecovered(_) | OperationCompletion::Cancelled
        ),
        OperationKind::Analysis => matches!(
            completion,
            OperationCompletion::AnalysisCommitted { .. } | OperationCompletion::Cancelled
        ),
        OperationKind::Import => matches!(
            completion,
            OperationCompletion::Succeeded | OperationCompletion::Cancelled
        ),
    }
}

const fn failure_code_matches_kind(kind: OperationKind, code: OperationFailureCode) -> bool {
    match kind {
        OperationKind::DatasetOpen => matches!(
            code,
            OperationFailureCode::DatasetNotFound
                | OperationFailureCode::DatasetPermissionDenied
                | OperationFailureCode::DatasetInvalid
                | OperationFailureCode::DatasetUnsupported
                | OperationFailureCode::DatasetCapacityExceeded
                | OperationFailureCode::DatasetReadFailed
        ),
        OperationKind::SourceVerification => matches!(
            code,
            OperationFailureCode::SourceChanged
                | OperationFailureCode::SourceVerificationInvalid
                | OperationFailureCode::SourceVerificationCapacityExceeded
                | OperationFailureCode::SourceVerificationReadFailed
        ),
        OperationKind::ProjectOpen => matches!(
            code,
            OperationFailureCode::ProjectNotFound
                | OperationFailureCode::ProjectPermissionDenied
                | OperationFailureCode::ProjectInvalidDocument
                | OperationFailureCode::ProjectUnsupportedSchema
                | OperationFailureCode::ProjectReadFailed
                | OperationFailureCode::ProjectCapacityExceeded
                | OperationFailureCode::ProjectDigestMismatch
                | OperationFailureCode::ProjectCorrupt
                | OperationFailureCode::ProjectBusy
        ),
        OperationKind::ProjectSave => matches!(
            code,
            OperationFailureCode::ProjectPermissionDenied
                | OperationFailureCode::ProjectInvalidDocument
                | OperationFailureCode::ProjectWriteFailed
                | OperationFailureCode::ProjectCommitIndeterminate
                | OperationFailureCode::ProjectReadOnly
                | OperationFailureCode::ProjectWriterContended
                | OperationFailureCode::ProjectStaleParent
                | OperationFailureCode::ProjectDestinationExists
                | OperationFailureCode::ProjectUnsupportedFilesystem
                | OperationFailureCode::ProjectCapacityExceeded
                | OperationFailureCode::ProjectSourceChanged
                | OperationFailureCode::ProjectDigestMismatch
                | OperationFailureCode::ProjectCorrupt
                | OperationFailureCode::ProjectBusy
        ),
        OperationKind::ProjectSaveAs => matches!(
            code,
            OperationFailureCode::ProjectPermissionDenied
                | OperationFailureCode::ProjectInvalidDocument
                | OperationFailureCode::ProjectWriteFailed
                | OperationFailureCode::ProjectCommitIndeterminate
                | OperationFailureCode::ProjectReadOnly
                | OperationFailureCode::ProjectWriterContended
                | OperationFailureCode::ProjectStaleParent
                | OperationFailureCode::ProjectDestinationExists
                | OperationFailureCode::ProjectUnsupportedFilesystem
                | OperationFailureCode::ProjectCapacityExceeded
                | OperationFailureCode::ProjectSourceChanged
                | OperationFailureCode::ProjectDigestMismatch
                | OperationFailureCode::ProjectCorrupt
                | OperationFailureCode::ProjectBusy
        ),
        OperationKind::ProjectRecovery => matches!(
            code,
            OperationFailureCode::ProjectNotFound
                | OperationFailureCode::ProjectPermissionDenied
                | OperationFailureCode::ProjectInvalidDocument
                | OperationFailureCode::ProjectUnsupportedSchema
                | OperationFailureCode::ProjectReadFailed
                | OperationFailureCode::ProjectCapacityExceeded
                | OperationFailureCode::ProjectDigestMismatch
                | OperationFailureCode::ProjectCorrupt
                | OperationFailureCode::ProjectBusy
        ),
        OperationKind::Analysis => matches!(
            code,
            OperationFailureCode::AnalysisInvalidInput
                | OperationFailureCode::AnalysisCapacityExceeded
                | OperationFailureCode::AnalysisExecutionFailed
        ),
        OperationKind::Import => matches!(
            code,
            OperationFailureCode::ImportInvalidInput
                | OperationFailureCode::ImportCapacityExceeded
                | OperationFailureCode::ImportExecutionFailed
        ),
    }
}

#[cfg(test)]
mod tests;

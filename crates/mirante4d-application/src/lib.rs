//! Pure application state transitions for Mirante4D.
//!
//! This crate owns the framework-neutral command, reducer, event, snapshot,
//! operation, and fault boundary. It deliberately owns no filesystem I/O,
//! threads, UI values, renderer/runtime payloads, or serialization.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

use mirante4d_domain::{
    CameraView, CrossSectionView, IsoLightState, LogicalLayerKey, TimeIndex, ToolKind, ViewerLayout,
};
use mirante4d_identity::ScientificContentId;
use mirante4d_project_model::{
    ArtifactHandleId, ArtifactReference, ChannelPreset, ChannelPresetId, DatasetReference,
    LayerViewState, MAX_CHANNEL_PRESETS, MAX_TOTAL_CHANNEL_PRESET_ENTRIES,
    ProjectGenerationProjection, ProjectId, ProjectRevisionHighWater, ProjectRevisionId,
    ProjectState, ViewState,
};
use mirante4d_settings::ResourcePolicy;

/// Maximum number of project revisions retained for undo/redo.
pub const MAX_HISTORY_ENTRIES: usize = 128;
/// Maximum number of events waiting for a consumer.
pub const MAX_PENDING_EVENTS: usize = 256;
/// Maximum number of concurrently registered background operations.
pub const MAX_ACTIVE_OPERATIONS: usize = 64;

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
}

impl SettingsChangeToken {
    pub const fn id(self) -> SettingsChangeId {
        self.id
    }

    pub const fn policy(self) -> ResourcePolicy {
        self.policy
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationKind {
    ProjectOpen,
    ProjectSave,
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

    pub const fn currentness_generation(&self) -> CurrentnessGeneration {
        self.currentness_generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourcePolicyRejection {
    AtomicWriteFailed,
    CommitIndeterminate,
    InvalidDocument,
    PermissionDenied,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OperationCompletion {
    Succeeded,
    Cancelled,
    Failed(ApplicationFaultCode),
    ArtifactReady(Box<ArtifactReference>),
    ProjectOpened(Box<ProjectGenerationProjection>),
    ProjectSaved(ProjectRevisionId),
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
    active_tool: ToolKind,
    selected_channel_preset: Option<ChannelPresetId>,
    selected_artifact: Option<ArtifactHandleId>,
    active_cross_section_panel: Option<CrossSectionPanelId>,
}

impl Default for TransientApplicationState {
    fn default() -> Self {
        Self {
            playback_active: false,
            active_tool: ToolKind::Navigate,
            selected_channel_preset: None,
            selected_artifact: None,
            active_cross_section_panel: None,
        }
    }
}

impl TransientApplicationState {
    pub const fn playback_active(&self) -> bool {
        self.playback_active
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
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApplicationCommand {
    ReplaceCurrentSource {
        source_generation: SourceSessionGeneration,
        workspace: UnboundWorkspace,
    },
    AttachVerifiedDataset,
    SetActiveLayer(LogicalLayerKey),
    SetTimepoint(TimeIndex),
    SetLayerView(LayerViewState),
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
    UpsertArtifact(ArtifactReference),
    RemoveArtifact(ArtifactHandleId),
    SetPlaybackActive(bool),
    SetActiveTool(ToolKind),
    SelectChannelPreset(Option<ChannelPresetId>),
    SelectArtifact(Option<ArtifactHandleId>),
    SetActiveCrossSectionPanel(Option<CrossSectionPanelId>),
    Undo,
    Redo,
    RequestProjectOpen,
    RequestProjectSave,
    BeginOperation(OperationKind),
    CompleteOperation {
        token: OperationToken,
        completion: OperationCompletion,
    },
    CancelOperation(OperationId),
    RequestResourcePolicyChange(ResourcePolicy),
    CompleteResourcePolicyPersistence {
        token: SettingsChangeToken,
        outcome: ResourcePolicyPersistenceOutcome,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationCommandKind {
    ReplaceCurrentSource,
    AttachVerifiedDataset,
    SetActiveLayer,
    SetTimepoint,
    SetLayerView,
    SetCamera,
    SetLayout,
    SetIsoLight,
    SetLayerOrder,
    UpsertChannelPreset,
    RemoveChannelPreset,
    UpsertArtifact,
    RemoveArtifact,
    SetPlaybackActive,
    SetActiveTool,
    SelectChannelPreset,
    SelectArtifact,
    SetActiveCrossSectionPanel,
    Undo,
    Redo,
    RequestProjectOpen,
    RequestProjectSave,
    BeginOperation,
    CompleteOperation,
    CancelOperation,
    RequestResourcePolicyChange,
    CompleteResourcePolicyPersistence,
}

impl ApplicationCommand {
    pub const fn kind(&self) -> ApplicationCommandKind {
        match self {
            Self::ReplaceCurrentSource { .. } => ApplicationCommandKind::ReplaceCurrentSource,
            Self::AttachVerifiedDataset => ApplicationCommandKind::AttachVerifiedDataset,
            Self::SetActiveLayer(_) => ApplicationCommandKind::SetActiveLayer,
            Self::SetTimepoint(_) => ApplicationCommandKind::SetTimepoint,
            Self::SetLayerView(_) => ApplicationCommandKind::SetLayerView,
            Self::SetCamera(_) => ApplicationCommandKind::SetCamera,
            Self::SetLayout { .. } => ApplicationCommandKind::SetLayout,
            Self::SetIsoLight(_) => ApplicationCommandKind::SetIsoLight,
            Self::SetLayerOrder(_) => ApplicationCommandKind::SetLayerOrder,
            Self::UpsertChannelPreset(_) => ApplicationCommandKind::UpsertChannelPreset,
            Self::RemoveChannelPreset(_) => ApplicationCommandKind::RemoveChannelPreset,
            Self::UpsertArtifact(_) => ApplicationCommandKind::UpsertArtifact,
            Self::RemoveArtifact(_) => ApplicationCommandKind::RemoveArtifact,
            Self::SetPlaybackActive(_) => ApplicationCommandKind::SetPlaybackActive,
            Self::SetActiveTool(_) => ApplicationCommandKind::SetActiveTool,
            Self::SelectChannelPreset(_) => ApplicationCommandKind::SelectChannelPreset,
            Self::SelectArtifact(_) => ApplicationCommandKind::SelectArtifact,
            Self::SetActiveCrossSectionPanel(_) => {
                ApplicationCommandKind::SetActiveCrossSectionPanel
            }
            Self::Undo => ApplicationCommandKind::Undo,
            Self::Redo => ApplicationCommandKind::Redo,
            Self::RequestProjectOpen => ApplicationCommandKind::RequestProjectOpen,
            Self::RequestProjectSave => ApplicationCommandKind::RequestProjectSave,
            Self::BeginOperation(_) => ApplicationCommandKind::BeginOperation,
            Self::CompleteOperation { .. } => ApplicationCommandKind::CompleteOperation,
            Self::CancelOperation(_) => ApplicationCommandKind::CancelOperation,
            Self::RequestResourcePolicyChange(_) => {
                ApplicationCommandKind::RequestResourcePolicyChange
            }
            Self::CompleteResourcePolicyPersistence { .. } => {
                ApplicationCommandKind::CompleteResourcePolicyPersistence
            }
        }
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
    LayerNotFound,
    ChannelPresetNotFound,
    ArtifactNotFound,
    UndoUnavailable,
    RedoUnavailable,
    EventQueueFull,
    OperationRegistryFull,
    OperationNotFound,
    OperationTokenMismatch,
    StaleOperationCompletion,
    InvalidOperationCompletion,
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
    CurrentSourceReplaced {
        source_generation: SourceSessionGeneration,
        provisional_project_id: ProjectId,
    },
    SourceVerified {
        source_generation: SourceSessionGeneration,
        scientific_content_id: ScientificContentId,
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
    ProjectSaved {
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
    Failed(ApplicationFaultCode),
    ArtifactAdmitted,
    ProjectOpened,
    ProjectSaved,
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
}

#[derive(Debug, Clone, PartialEq)]
enum Workspace {
    Unbound(Arc<UnboundWorkspace>),
    Bound(BoundWorkspace),
}

#[derive(Debug, Clone, PartialEq)]
struct ActiveOperation {
    token: OperationToken,
}

#[derive(Debug, PartialEq)]
pub struct ApplicationState {
    source_generation: SourceSessionGeneration,
    verified_source: Option<DatasetReference>,
    workspace: Workspace,
    transient: TransientApplicationState,
    currentness: CurrentnessGeneration,
    operations: BTreeMap<OperationId, ActiveOperation>,
    next_operation_id: u64,
    next_task_id: u64,
    next_settings_change_id: u64,
    events: VecDeque<ApplicationEvent>,
    resource_policy: ResourcePolicy,
    pending_settings_change: Option<SettingsChangeToken>,
}

impl ApplicationState {
    pub fn new_unbound(
        source_generation: SourceSessionGeneration,
        workspace: UnboundWorkspace,
        resource_policy: ResourcePolicy,
    ) -> Self {
        Self {
            source_generation,
            verified_source: None,
            workspace: Workspace::Unbound(Arc::new(workspace)),
            transient: TransientApplicationState::default(),
            currentness: CurrentnessGeneration::initial(),
            operations: BTreeMap::new(),
            next_operation_id: 1,
            next_task_id: 1,
            next_settings_change_id: 1,
            events: VecDeque::new(),
            resource_policy,
            pending_settings_change: None,
        }
    }

    fn fork_for_dispatch(&self) -> Self {
        Self {
            source_generation: self.source_generation,
            verified_source: self.verified_source.clone(),
            workspace: self.workspace.clone(),
            transient: self.transient.clone(),
            currentness: self.currentness,
            operations: self.operations.clone(),
            next_operation_id: self.next_operation_id,
            next_task_id: self.next_task_id,
            next_settings_change_id: self.next_settings_change_id,
            events: self.events.clone(),
            resource_policy: self.resource_policy,
            pending_settings_change: self.pending_settings_change,
        }
    }

    #[cfg(test)]
    fn admit_verified_source_for_test(
        &mut self,
        source_generation: SourceSessionGeneration,
        dataset: DatasetReference,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        if source_generation != self.source_generation {
            return Err(ApplicationFaultCode::SourceSessionMismatch);
        }
        if let Workspace::Bound(bound) = &self.workspace
            && bound.current_state().dataset() != &dataset
        {
            return Err(ApplicationFaultCode::DatasetIdentityMismatch);
        }
        if self.verified_source.as_ref() == Some(&dataset) {
            return Ok(CommandEffect::NoChange);
        }
        self.push_event(ApplicationEvent::SourceVerified {
            source_generation,
            scientific_content_id: *dataset.scientific_content_id(),
        })?;
        self.verified_source = Some(dataset);
        Ok(CommandEffect::Changed)
    }

    /// Applies one command atomically. Rejected commands leave every field,
    /// including queues, counters, and revision high-water, unchanged.
    pub fn dispatch(
        &mut self,
        command: ApplicationCommand,
    ) -> Result<CommandEffect, ApplicationFault> {
        let command_kind = command.kind();
        let fault_token = match &command {
            ApplicationCommand::CompleteOperation { token, .. } => Some(token.clone()),
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
        let source = match &self.verified_source {
            Some(dataset) => SourceVerificationSnapshot::Verified(dataset.clone()),
            None => SourceVerificationSnapshot::Required,
        };
        let operations = self
            .operations
            .values()
            .map(|operation| operation.token.clone())
            .collect();
        ApplicationSnapshot {
            source_generation: self.source_generation,
            source,
            workspace,
            transient: self.transient.clone(),
            currentness: self.currentness,
            active_operations: operations,
            resource_policy: self.resource_policy,
            pending_settings_change: self.pending_settings_change,
            pending_event_count: self.events.len(),
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
        match command {
            ApplicationCommand::ReplaceCurrentSource {
                source_generation,
                workspace,
            } => self.replace_current_source(source_generation, workspace),
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
            ApplicationCommand::UpsertArtifact(artifact) => {
                self.upsert_artifact(command_kind, artifact)
            }
            ApplicationCommand::RemoveArtifact(id) => self.remove_artifact(command_kind, &id),
            ApplicationCommand::SetPlaybackActive(active) => self.set_playback_active(active),
            ApplicationCommand::SetActiveTool(tool) => self.set_active_tool(tool),
            ApplicationCommand::SelectChannelPreset(id) => self.select_channel_preset(id),
            ApplicationCommand::SelectArtifact(id) => self.select_artifact(id),
            ApplicationCommand::SetActiveCrossSectionPanel(panel) => {
                self.set_active_cross_section_panel(panel)
            }
            ApplicationCommand::Undo => self.move_history(command_kind, false),
            ApplicationCommand::Redo => self.move_history(command_kind, true),
            ApplicationCommand::RequestProjectOpen => self.request_project_open(),
            ApplicationCommand::RequestProjectSave => self.request_project_save(),
            ApplicationCommand::BeginOperation(kind) => {
                if matches!(
                    kind,
                    OperationKind::ProjectOpen | OperationKind::ProjectSave
                ) {
                    return Err(ApplicationFaultCode::InvalidProjectTransition);
                }
                self.begin_operation(kind).map(|_| CommandEffect::Changed)
            }
            ApplicationCommand::CompleteOperation { token, completion } => {
                self.complete_operation(command_kind, token, completion)
            }
            ApplicationCommand::CancelOperation(id) => self.cancel_operation(id),
            ApplicationCommand::RequestResourcePolicyChange(policy) => {
                self.request_resource_policy_change(policy)
            }
            ApplicationCommand::CompleteResourcePolicyPersistence { token, outcome } => {
                self.complete_resource_policy_persistence(token, outcome)
            }
        }
    }

    fn replace_current_source(
        &mut self,
        source_generation: SourceSessionGeneration,
        workspace: UnboundWorkspace,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        if !self.operations.is_empty() {
            return Err(ApplicationFaultCode::OperationConflict);
        }
        if source_generation.get() <= self.source_generation.get() {
            return Err(ApplicationFaultCode::SourceGenerationNotAdvanced);
        }
        let provisional_project_id = workspace.provisional_project_id();
        self.source_generation = source_generation;
        self.verified_source = None;
        self.workspace = Workspace::Unbound(Arc::new(workspace));
        self.transient = TransientApplicationState::default();
        self.advance_currentness()?;
        self.push_event(ApplicationEvent::CurrentSourceReplaced {
            source_generation,
            provisional_project_id,
        })?;
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
        match &self.workspace {
            Workspace::Unbound(unbound) => {
                let mut presets = unbound.channel_presets.clone();
                upsert_preset(&mut presets, preset);
                if presets == unbound.channel_presets {
                    return Ok(CommandEffect::NoChange);
                }
                validate_unbound_presets(&unbound.view, &presets)?;
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
                upsert_preset(&mut presets, preset);
                if presets == bound.current_state().channel_presets() {
                    return Ok(CommandEffect::NoChange);
                }
                let project = rebuild_project(bound.current_state(), None, Some(presets), None)?;
                self.commit_project(project)
            }
        }
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

    fn upsert_artifact(
        &mut self,
        _command_kind: ApplicationCommandKind,
        artifact: ArtifactReference,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        let Workspace::Bound(bound) = &self.workspace else {
            return Err(ApplicationFaultCode::WorkspaceUnbound);
        };
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
        artifacts.remove(index);
        let project = rebuild_project(bound.current_state(), None, None, Some(artifacts))?;
        self.commit_project(project)
    }

    fn set_playback_active(&mut self, active: bool) -> Result<CommandEffect, ApplicationFaultCode> {
        if self.transient.playback_active == active {
            return Ok(CommandEffect::NoChange);
        }
        self.transient.playback_active = active;
        self.push_event(ApplicationEvent::TransientStateChanged)?;
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

    fn normalize_transient_selections(&mut self) {
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
        if self
            .operations
            .values()
            .any(|operation| operation.token.kind == OperationKind::ProjectSave)
        {
            return Err(ApplicationFaultCode::OperationConflict);
        }
        let Workspace::Bound(bound) = &self.workspace else {
            return Err(ApplicationFaultCode::IdentityVerificationRequired);
        };
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

    fn begin_operation(
        &mut self,
        kind: OperationKind,
    ) -> Result<OperationToken, ApplicationFaultCode> {
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
            currentness_generation: self.currentness,
        };
        self.operations.insert(
            operation_id,
            ActiveOperation {
                token: token.clone(),
            },
        );
        Ok(token)
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
        if token.kind == OperationKind::ProjectSave {
            self.validate_save_token_current(&token)?;
        } else {
            self.validate_token_current(&token)?;
        }
        if !completion_matches_kind(token.kind, &completion) {
            return Err(ApplicationFaultCode::InvalidOperationCompletion);
        }
        let outcome = match completion {
            OperationCompletion::Succeeded => OperationOutcome::Succeeded,
            OperationCompletion::Cancelled => OperationOutcome::Cancelled,
            OperationCompletion::Failed(code) => OperationOutcome::Failed(code),
            OperationCompletion::ArtifactReady(artifact) => {
                if token.kind != OperationKind::Analysis {
                    return Err(ApplicationFaultCode::InvalidOperationCompletion);
                }
                let Workspace::Bound(bound) = &self.workspace else {
                    return Err(ApplicationFaultCode::WorkspaceUnbound);
                };
                let mut artifacts = bound.current_state().artifacts().to_vec();
                upsert_artifact(&mut artifacts, *artifact);
                let project = rebuild_project(bound.current_state(), None, None, Some(artifacts))?;
                if project == *bound.current_state() {
                    OperationOutcome::ArtifactAdmitted
                } else {
                    self.commit_project(project)?;
                    OperationOutcome::ArtifactAdmitted
                }
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
        };
        self.operations.remove(&token.operation_id);
        self.push_event(ApplicationEvent::OperationCompleted { token, outcome })?;
        Ok(CommandEffect::Changed)
    }

    fn cancel_operation(
        &mut self,
        operation_id: OperationId,
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        let operation = self
            .operations
            .remove(&operation_id)
            .ok_or(ApplicationFaultCode::OperationNotFound)?;
        self.push_event(ApplicationEvent::OperationCancellationRequested {
            token: operation.token,
        })?;
        Ok(CommandEffect::Changed)
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
    ) -> Result<CommandEffect, ApplicationFaultCode> {
        if let Some(pending) = self.pending_settings_change {
            return if pending.policy == policy {
                Ok(CommandEffect::NoChange)
            } else {
                Err(ApplicationFaultCode::ResourcePolicyChangePending)
            };
        }
        if self.resource_policy == policy {
            return Ok(CommandEffect::NoChange);
        }
        let token = SettingsChangeToken {
            id: SettingsChangeId(self.next_settings_change_id),
            policy,
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

#[derive(Debug, Clone, PartialEq)]
pub struct ApplicationSnapshot {
    source_generation: SourceSessionGeneration,
    source: SourceVerificationSnapshot,
    workspace: WorkspaceSnapshot,
    transient: TransientApplicationState,
    currentness: CurrentnessGeneration,
    active_operations: Vec<OperationToken>,
    resource_policy: ResourcePolicy,
    pending_settings_change: Option<SettingsChangeToken>,
    pending_event_count: usize,
}

impl ApplicationSnapshot {
    pub const fn source_generation(&self) -> SourceSessionGeneration {
        self.source_generation
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

fn completion_matches_kind(kind: OperationKind, completion: &OperationCompletion) -> bool {
    match kind {
        OperationKind::ProjectOpen => matches!(
            completion,
            OperationCompletion::ProjectOpened(_)
                | OperationCompletion::Cancelled
                | OperationCompletion::Failed(_)
        ),
        OperationKind::ProjectSave => matches!(
            completion,
            OperationCompletion::ProjectSaved(_)
                | OperationCompletion::Cancelled
                | OperationCompletion::Failed(_)
        ),
        OperationKind::Analysis => matches!(
            completion,
            OperationCompletion::ArtifactReady(_)
                | OperationCompletion::Succeeded
                | OperationCompletion::Cancelled
                | OperationCompletion::Failed(_)
        ),
        OperationKind::Import => matches!(
            completion,
            OperationCompletion::Succeeded
                | OperationCompletion::Cancelled
                | OperationCompletion::Failed(_)
        ),
    }
}

#[cfg(test)]
mod tests;

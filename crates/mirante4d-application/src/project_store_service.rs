//! Product-facing WP-10B project-store orchestration.
//!
//! The pure reducer owns live project revisions. This service owns the single
//! project-store actor, its session facts, request correlation, autosave
//! scheduling, and typed persistence outcomes. It never performs filesystem
//! work itself and every interaction-frame poll is bounded and nonblocking.

#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use mirante4d_identity::ScientificContentId;
use mirante4d_project_model::{
    ProjectGenerationProjection, ProjectId, ProjectRevisionHighWater, ProjectRevisionId,
    ProjectState,
};
use mirante4d_project_store::{
    ProjectCommitCapture, ProjectGenerationId, ProjectObjectSource, ProjectOpenMode,
    ProjectRecoveryCandidate, ProjectStoreActor, ProjectStoreCommand, ProjectStoreCompletion,
    ProjectStoreConfig, ProjectStoreFault, ProjectStorePath, ProjectStoreReceipt,
    ProjectStoreRequestId, ProjectStoreSession,
};

use super::{
    ApplicationSnapshot, OperationId, OperationKind, OperationToken, SourceVerificationSnapshot,
    WorkspaceSnapshot,
};

const NANOS_PER_SECOND: u64 = 1_000_000_000;
const AUTOSAVE_IDLE_DELAY_TICKS: u64 = 30 * NANOS_PER_SECOND;
const AUTOSAVE_MAX_DELAY_TICKS: u64 = 120 * NANOS_PER_SECOND;
const COMPLETIONS_PER_DRIVE_MAX: usize = 4;
const RECOVERY_STORE_LOCATORS_MAX: usize = 64;

pub trait MonotonicClock {
    /// Returns elapsed monotonic nanoseconds from an arbitrary stable origin.
    fn now(&self) -> u64;
}

#[derive(Debug)]
pub struct SystemMonotonicClock {
    origin: Instant,
}

impl SystemMonotonicClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemMonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MonotonicClock for SystemMonotonicClock {
    fn now(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AutosaveObservation {
    project_id: Option<ProjectId>,
    revision: Option<ProjectRevisionId>,
    bound: bool,
    dirty: bool,
    verified: bool,
    writable: bool,
    commit_active: bool,
    writes_suspended: bool,
}

impl AutosaveObservation {
    fn from_snapshot(
        snapshot: &ApplicationSnapshot,
        writable: bool,
        commit_active: bool,
        writes_suspended: bool,
    ) -> Self {
        let (project_id, revision, bound, dirty) = match snapshot.workspace() {
            WorkspaceSnapshot::Unbound { .. } => (None, None, false, false),
            WorkspaceSnapshot::Bound {
                project,
                revision,
                dirty,
                ..
            } => (Some(project.project_id()), Some(*revision), true, *dirty),
        };
        Self {
            project_id,
            revision,
            bound,
            dirty,
            verified: matches!(snapshot.source(), SourceVerificationSnapshot::Verified(_)),
            writable,
            commit_active,
            writes_suspended,
        }
    }

    const fn eligible(self) -> bool {
        self.verified
            && self.bound
            && self.dirty
            && self.writable
            && !self.commit_active
            && !self.writes_suspended
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingAutosave {
    first_edit: u64,
    latest_edit: u64,
}

impl PendingAutosave {
    fn deadline(self) -> Result<u64, ProjectStoreServiceError> {
        let idle = self
            .latest_edit
            .checked_add(AUTOSAVE_IDLE_DELAY_TICKS)
            .ok_or(ProjectStoreServiceError::ClockOverflow)?;
        let maximum = self
            .first_edit
            .checked_add(AUTOSAVE_MAX_DELAY_TICKS)
            .ok_or(ProjectStoreServiceError::ClockOverflow)?;
        Ok(idle.min(maximum))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AutosaveDue {
    project_id: ProjectId,
    revision: ProjectRevisionId,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct AutosaveScheduler {
    last_tick: Option<u64>,
    observed_project: Option<ProjectId>,
    observed_revision: Option<ProjectRevisionId>,
    pending: Option<PendingAutosave>,
}

impl AutosaveScheduler {
    fn validate_tick(&self, tick: u64) -> Result<(), ProjectStoreServiceError> {
        if self.last_tick.is_some_and(|previous| tick < previous) {
            Err(ProjectStoreServiceError::ClockRegressed {
                previous: self.last_tick.expect("checked as present"),
                observed: tick,
            })
        } else {
            Ok(())
        }
    }

    fn observe(
        &mut self,
        tick: u64,
        observation: AutosaveObservation,
    ) -> Result<Option<AutosaveDue>, ProjectStoreServiceError> {
        self.validate_tick(tick)?;
        let mut next = self.clone();
        next.last_tick = Some(tick);

        if !observation.bound || !observation.dirty {
            next.observed_project = observation.project_id;
            next.observed_revision = observation.revision;
            next.pending = None;
            *self = next;
            return Ok(None);
        }

        let project_id = observation
            .project_id
            .ok_or(ProjectStoreServiceError::InvalidApplicationSnapshot)?;
        let revision = observation
            .revision
            .ok_or(ProjectStoreServiceError::InvalidApplicationSnapshot)?;
        if next.observed_project != Some(project_id) {
            next.observed_project = Some(project_id);
            next.observed_revision = None;
            next.pending = None;
        }
        if next.observed_revision != Some(revision) {
            match &mut next.pending {
                Some(pending) => pending.latest_edit = tick,
                None => {
                    next.pending = Some(PendingAutosave {
                        first_edit: tick,
                        latest_edit: tick,
                    });
                }
            }
            next.observed_revision = Some(revision);
        }

        let due = match next.pending {
            Some(pending) if observation.eligible() && tick >= pending.deadline()? => {
                next.pending = None;
                Some(AutosaveDue {
                    project_id,
                    revision,
                })
            }
            _ => None,
        };
        *self = next;
        Ok(due)
    }

    fn pending_deadline(&self) -> Result<Option<u64>, ProjectStoreServiceError> {
        self.pending.map(PendingAutosave::deadline).transpose()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectStoreLifecycle {
    Unbound,
    Provisional,
    Established,
    RecoveryOnly,
    RecoverySelected,
    Closing,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectStoreServiceStatus {
    lifecycle: ProjectStoreLifecycle,
    project_id: Option<ProjectId>,
    mode: Option<ProjectOpenMode>,
    current_manual: Option<ProjectGenerationId>,
    current_autosave: Option<ProjectGenerationId>,
    foreground_active: bool,
    autosave_active: bool,
    writable: bool,
    writes_suspended: bool,
}

pub struct ProjectRecoveryStoreLocator {
    project_id: ProjectId,
    path: ProjectStorePath,
}

impl ProjectRecoveryStoreLocator {
    pub fn new(
        project_id: ProjectId,
        path: ProjectStorePath,
    ) -> Result<Self, ProjectStoreServiceError> {
        let expected_name = format!("{project_id}.m4dproj");
        if path.as_path().file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str())
        {
            return Err(ProjectStoreServiceError::InvalidProjection);
        }
        Ok(Self { project_id, path })
    }

    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }
}

impl ProjectStoreServiceStatus {
    pub const fn lifecycle(&self) -> ProjectStoreLifecycle {
        self.lifecycle
    }

    pub const fn project_id(&self) -> Option<ProjectId> {
        self.project_id
    }

    pub const fn mode(&self) -> Option<ProjectOpenMode> {
        self.mode
    }

    pub const fn current_manual(&self) -> Option<ProjectGenerationId> {
        self.current_manual
    }

    pub const fn current_autosave(&self) -> Option<ProjectGenerationId> {
        self.current_autosave
    }

    pub const fn foreground_active(&self) -> bool {
        self.foreground_active
    }

    pub const fn autosave_active(&self) -> bool {
        self.autosave_active
    }

    pub const fn writes_suspended(&self) -> bool {
        self.writes_suspended
    }

    pub const fn writable(&self) -> bool {
        self.writable
    }
}

#[derive(Clone, Debug)]
struct SessionFacts {
    path: ProjectStorePath,
    project_id: ProjectId,
    mode: ProjectOpenMode,
    current_manual: Option<ProjectGenerationId>,
    current_autosave: Option<ProjectGenerationId>,
}

impl SessionFacts {
    fn from_session(session: ProjectStoreSession) -> Self {
        Self {
            path: session.path().clone(),
            project_id: session.project_id(),
            mode: session.mode(),
            current_manual: session.current_manual_generation(),
            current_autosave: session.current_autosave_generation(),
        }
    }

    fn validate_shape(&self) -> Result<(), ProjectStoreServiceError> {
        if self.current_manual.is_none() && self.current_autosave.is_none() {
            Err(ProjectStoreServiceError::UnexpectedCompletion)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
enum StoreBinding {
    Unbound {
        provisional_destination: Option<ProjectStorePath>,
    },
    Provisional(SessionFacts),
    Established(SessionFacts),
    RecoveryOnly,
    RecoverySelected {
        facts: SessionFacts,
        selected_generation: ProjectGenerationId,
    },
    Closed,
}

impl StoreBinding {
    fn from_opened(facts: SessionFacts) -> Result<(Self, bool), ProjectStoreServiceError> {
        facts.validate_shape()?;
        match facts.current_manual {
            Some(_) => Ok((Self::Established(facts), false)),
            None if facts.current_autosave.is_some() => Ok((Self::Provisional(facts), true)),
            None => Err(ProjectStoreServiceError::UnexpectedCompletion),
        }
    }

    fn lifecycle(&self) -> ProjectStoreLifecycle {
        match self {
            Self::Unbound { .. } => ProjectStoreLifecycle::Unbound,
            Self::Provisional(_) => ProjectStoreLifecycle::Provisional,
            Self::Established(_) => ProjectStoreLifecycle::Established,
            Self::RecoveryOnly => ProjectStoreLifecycle::RecoveryOnly,
            Self::RecoverySelected { .. } => ProjectStoreLifecycle::RecoverySelected,
            Self::Closed => ProjectStoreLifecycle::Closed,
        }
    }

    fn facts(&self) -> Option<&SessionFacts> {
        match self {
            Self::Provisional(facts)
            | Self::Established(facts)
            | Self::RecoverySelected { facts, .. } => Some(facts),
            Self::Unbound { .. } | Self::RecoveryOnly | Self::Closed => None,
        }
    }

    fn writable_for_autosave(&self) -> bool {
        match self {
            Self::Unbound {
                provisional_destination,
            } => provisional_destination.is_some(),
            Self::Provisional(facts) | Self::Established(facts) => {
                facts.mode == ProjectOpenMode::PreferWritable
            }
            Self::RecoveryOnly | Self::RecoverySelected { .. } | Self::Closed => false,
        }
    }

    fn autosave_capture_facts(
        &self,
        project_id: ProjectId,
    ) -> Result<AutosaveCaptureFacts, ProjectStoreServiceError> {
        match self {
            Self::Unbound {
                provisional_destination: Some(destination),
            } => Ok(AutosaveCaptureFacts {
                destination: Some(destination.clone()),
                expected_parent: None,
                autosave_base: None,
            }),
            Self::Unbound {
                provisional_destination: None,
            } => Err(ProjectStoreServiceError::ReadOnly),
            Self::Provisional(facts)
                if facts.project_id == project_id
                    && facts.mode == ProjectOpenMode::PreferWritable =>
            {
                Ok(AutosaveCaptureFacts {
                    destination: None,
                    expected_parent: facts.current_autosave,
                    autosave_base: None,
                })
            }
            Self::Established(facts)
                if facts.project_id == project_id
                    && facts.mode == ProjectOpenMode::PreferWritable =>
            {
                Ok(AutosaveCaptureFacts {
                    destination: None,
                    expected_parent: facts.current_autosave,
                    autosave_base: facts.current_manual,
                })
            }
            Self::Provisional(facts) | Self::Established(facts)
                if facts.project_id != project_id =>
            {
                Err(ProjectStoreServiceError::ProjectMismatch)
            }
            Self::Provisional(_) | Self::Established(_) => Err(ProjectStoreServiceError::ReadOnly),
            Self::RecoveryOnly | Self::RecoverySelected { .. } => {
                Err(ProjectStoreServiceError::SaveAsRequired)
            }
            Self::Closed => Err(ProjectStoreServiceError::Closing),
        }
    }

    fn accept_autosave(
        &mut self,
        project_id: ProjectId,
        receipt: &ProjectStoreReceipt,
    ) -> Result<(), ProjectStoreServiceError> {
        match self {
            Self::Unbound {
                provisional_destination: Some(destination),
            } => {
                *self = Self::Provisional(SessionFacts {
                    path: destination.clone(),
                    project_id,
                    mode: ProjectOpenMode::PreferWritable,
                    current_manual: None,
                    current_autosave: Some(receipt.current_generation_id()),
                });
                Ok(())
            }
            Self::Provisional(facts) | Self::Established(facts)
                if facts.project_id == project_id =>
            {
                facts.current_autosave = Some(receipt.current_generation_id());
                Ok(())
            }
            Self::Unbound { .. } => Err(ProjectStoreServiceError::ReadOnly),
            Self::Provisional(_) | Self::Established(_) => {
                Err(ProjectStoreServiceError::ProjectMismatch)
            }
            Self::RecoveryOnly | Self::RecoverySelected { .. } | Self::Closed => {
                Err(ProjectStoreServiceError::UnexpectedCompletion)
            }
        }
    }
}

#[derive(Clone, Debug)]
struct AutosaveCaptureFacts {
    destination: Option<ProjectStorePath>,
    expected_parent: Option<ProjectGenerationId>,
    autosave_base: Option<ProjectGenerationId>,
}

#[derive(Clone, Debug)]
struct ActiveAutosave {
    request_id: ProjectStoreRequestId,
    project_id: ProjectId,
    source_identity: ScientificContentId,
    revision: ProjectRevisionId,
    revision_high_water: ProjectRevisionHighWater,
    expected_parent: Option<ProjectGenerationId>,
    autosave_base: Option<ProjectGenerationId>,
    cancellation_request: Option<ProjectStoreRequestId>,
    stale_source_observed: bool,
}

#[derive(Clone, Debug)]
struct PendingNormalOpen {
    token: OperationToken,
    projection: ProjectGenerationProjection,
    candidates: Vec<ProjectRecoveryCandidate>,
    opens_dirty: bool,
}

#[derive(Debug)]
enum InspectionContext {
    HealthyOpen {
        token: OperationToken,
        projection: Box<ProjectGenerationProjection>,
        opens_dirty: bool,
    },
    FailedOpen {
        token: OperationToken,
        fault: ProjectStoreFault,
    },
    Explicit,
}

#[derive(Debug)]
enum ForegroundKind {
    Open {
        token: OperationToken,
        path: ProjectStorePath,
        mode: ProjectOpenMode,
    },
    Create {
        token: OperationToken,
        destination: ProjectStorePath,
        project_id: ProjectId,
        revision: ProjectRevisionId,
        revision_high_water: ProjectRevisionHighWater,
    },
    ManualSave {
        token: OperationToken,
        revision: ProjectRevisionId,
        revision_high_water: ProjectRevisionHighWater,
        expected_parent: ProjectGenerationId,
    },
    SaveAs {
        token: OperationToken,
        destination: ProjectStorePath,
        source_project_id: ProjectId,
        source_generation: ProjectGenerationId,
        projection: ProjectGenerationProjection,
    },
    InspectRecovery {
        context: InspectionContext,
    },
    OpenRecovery {
        token: OperationToken,
        generation_id: ProjectGenerationId,
        pending_normal: Option<PendingNormalOpen>,
    },
    SelectProvisionalOpen {
        token: OperationToken,
        generation_id: ProjectGenerationId,
        expected_projection: Box<ProjectGenerationProjection>,
    },
}

impl ForegroundKind {
    fn token(&self) -> Option<&OperationToken> {
        match self {
            Self::Open { token, .. }
            | Self::Create { token, .. }
            | Self::ManualSave { token, .. }
            | Self::SaveAs { token, .. }
            | Self::OpenRecovery { token, .. }
            | Self::SelectProvisionalOpen { token, .. } => Some(token),
            Self::InspectRecovery { context } => match context {
                InspectionContext::HealthyOpen { token, .. }
                | InspectionContext::FailedOpen { token, .. } => Some(token),
                InspectionContext::Explicit => None,
            },
        }
    }
}

#[derive(Debug)]
struct ActiveForeground {
    request_id: ProjectStoreRequestId,
    cancellation_request: Option<ProjectStoreRequestId>,
    kind: ForegroundKind,
}

#[derive(Debug)]
pub enum ProjectStoreServiceEvent {
    Opened {
        token: OperationToken,
        projection: Box<ProjectGenerationProjection>,
        candidates: Vec<ProjectRecoveryCandidate>,
        opens_dirty: bool,
    },
    RecoveryReviewRequired {
        token: OperationToken,
        candidates: Vec<ProjectRecoveryCandidate>,
        automatic_newer: ProjectGenerationId,
    },
    OpenFailed {
        token: OperationToken,
        fault: ProjectStoreFault,
        candidates: Vec<ProjectRecoveryCandidate>,
    },
    Created {
        token: OperationToken,
        saved_revision: ProjectRevisionId,
    },
    ManualSaved {
        token: OperationToken,
        receipt: ProjectStoreReceipt,
    },
    SavedAs {
        token: OperationToken,
        projection: Box<ProjectGenerationProjection>,
        receipt: ProjectStoreReceipt,
    },
    RecoveryCandidatesListed {
        candidates: Vec<ProjectRecoveryCandidate>,
    },
    RecoveryInspectionFailed {
        fault: ProjectStoreFault,
    },
    RecoveryOpened {
        token: OperationToken,
        generation_id: ProjectGenerationId,
        projection: Box<ProjectGenerationProjection>,
    },
    RecoverySelectionFailed {
        token: OperationToken,
        fault: ProjectStoreFault,
        normal_open_still_available: bool,
    },
    OperationFailed {
        token: OperationToken,
        fault: ProjectStoreFault,
    },
    AutosaveSubmitted {
        request_id: ProjectStoreRequestId,
        revision: ProjectRevisionId,
    },
    AutosaveFinished {
        request_id: ProjectStoreRequestId,
        revision: ProjectRevisionId,
        result: Result<ProjectStoreReceipt, ProjectStoreFault>,
    },
    CancellationAcknowledged {
        request_id: ProjectStoreRequestId,
        target_request_id: ProjectStoreRequestId,
    },
    Closed {
        request_id: ProjectStoreRequestId,
        result: Result<(), ProjectStoreFault>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectStoreServiceError {
    ClockRegressed { previous: u64, observed: u64 },
    ClockOverflow,
    RequestIdOverflow,
    InvalidApplicationSnapshot,
    InvalidProjection,
    InvalidOperationToken,
    OperationConflict,
    ProjectMismatch,
    ReadOnly,
    SaveAsRequired,
    RecoveryCandidateUnavailable,
    Closing,
    WritesSuspended,
    ActorPanicked,
    UnexpectedCompletion,
    Store(ProjectStoreFault),
}

impl From<ProjectStoreFault> for ProjectStoreServiceError {
    fn from(fault: ProjectStoreFault) -> Self {
        Self::Store(fault)
    }
}

pub struct ProjectStoreApplicationService<C> {
    actor: Option<ProjectStoreActor>,
    clock: C,
    scheduler: AutosaveScheduler,
    binding: StoreBinding,
    active_foreground: Option<ActiveForeground>,
    active_autosave: Option<ActiveAutosave>,
    pending_normal_open: Option<PendingNormalOpen>,
    recovery_candidates: Vec<ProjectRecoveryCandidate>,
    recovery_store_locators: Vec<ProjectRecoveryStoreLocator>,
    close_request: Option<ProjectStoreRequestId>,
    next_request_id: u64,
    writes_suspended: bool,
    cached_repaint_after: Option<Duration>,
}

impl<C: MonotonicClock> ProjectStoreApplicationService<C> {
    pub fn start(
        config: ProjectStoreConfig,
        clock: C,
        provisional_destination: Option<ProjectStorePath>,
    ) -> Result<Self, ProjectStoreServiceError> {
        Self::start_with_recovery_locators(config, clock, provisional_destination, Vec::new())
    }

    pub fn start_with_recovery_locators(
        config: ProjectStoreConfig,
        clock: C,
        provisional_destination: Option<ProjectStorePath>,
        mut recovery_store_locators: Vec<ProjectRecoveryStoreLocator>,
    ) -> Result<Self, ProjectStoreServiceError> {
        if recovery_store_locators.len() > RECOVERY_STORE_LOCATORS_MAX {
            return Err(ProjectStoreServiceError::Store(
                ProjectStoreFault::Capacity {
                    stage: "recovery_store_locators",
                },
            ));
        }
        recovery_store_locators.sort_by_key(ProjectRecoveryStoreLocator::project_id);
        if recovery_store_locators
            .windows(2)
            .any(|pair| pair[0].project_id == pair[1].project_id)
        {
            return Err(ProjectStoreServiceError::InvalidProjection);
        }
        Ok(Self {
            actor: Some(ProjectStoreActor::start(config)?),
            clock,
            scheduler: AutosaveScheduler::default(),
            binding: StoreBinding::Unbound {
                provisional_destination,
            },
            active_foreground: None,
            active_autosave: None,
            pending_normal_open: None,
            recovery_candidates: Vec::new(),
            recovery_store_locators,
            close_request: None,
            next_request_id: 1,
            writes_suspended: false,
            cached_repaint_after: None,
        })
    }

    pub fn status(&self) -> ProjectStoreServiceStatus {
        let facts = self.binding.facts();
        ProjectStoreServiceStatus {
            lifecycle: if self.close_request.is_some() {
                ProjectStoreLifecycle::Closing
            } else {
                self.binding.lifecycle()
            },
            project_id: facts.map(|facts| facts.project_id),
            mode: facts.map(|facts| facts.mode),
            current_manual: facts.and_then(|facts| facts.current_manual),
            current_autosave: facts.and_then(|facts| facts.current_autosave),
            foreground_active: self.active_foreground.is_some()
                || self.pending_normal_open.is_some(),
            autosave_active: self.active_autosave.is_some(),
            writable: self.binding.writable_for_autosave() && !self.writes_suspended,
            writes_suspended: self.writes_suspended,
        }
    }

    pub fn can_open(&self) -> bool {
        matches!(self.binding, StoreBinding::Unbound { .. })
            && self.active_foreground.is_none()
            && self.active_autosave.is_none()
            && self.pending_normal_open.is_none()
            && self.close_request.is_none()
    }

    pub fn can_save(&self) -> bool {
        !self.writes_suspended
            && self.active_foreground.is_none()
            && self.active_autosave.is_none()
            && self.pending_normal_open.is_none()
            && self.close_request.is_none()
            && match &self.binding {
                StoreBinding::Unbound { .. } => true,
                StoreBinding::Provisional(facts) | StoreBinding::Established(facts) => {
                    facts.mode == ProjectOpenMode::PreferWritable
                }
                StoreBinding::RecoveryOnly
                | StoreBinding::RecoverySelected { .. }
                | StoreBinding::Closed => false,
            }
    }

    pub fn can_save_as(&self) -> bool {
        !self.writes_suspended
            && self.active_foreground.is_none()
            && self.active_autosave.is_none()
            && self.pending_normal_open.is_none()
            && self.close_request.is_none()
            && matches!(
                self.binding,
                StoreBinding::Established(_) | StoreBinding::RecoverySelected { .. }
            )
    }

    pub fn recovery_candidates(&self) -> &[ProjectRecoveryCandidate] {
        &self.recovery_candidates
    }

    pub fn recovery_store_project_ids(&self) -> impl ExactSizeIterator<Item = ProjectId> + '_ {
        self.recovery_store_locators
            .iter()
            .map(ProjectRecoveryStoreLocator::project_id)
    }

    pub fn submit_open_recovery_store(
        &mut self,
        token: OperationToken,
        project_id: ProjectId,
        mode: ProjectOpenMode,
    ) -> Result<ProjectStoreRequestId, ProjectStoreServiceError> {
        let path = self
            .recovery_store_locators
            .iter()
            .find(|locator| locator.project_id == project_id)
            .map(|locator| locator.path.clone())
            .ok_or(ProjectStoreServiceError::RecoveryCandidateUnavailable)?;
        self.submit_open(token, path, mode)
    }

    pub fn submit_open(
        &mut self,
        token: OperationToken,
        path: ProjectStorePath,
        mode: ProjectOpenMode,
    ) -> Result<ProjectStoreRequestId, ProjectStoreServiceError> {
        self.require_idle()?;
        if token.kind() != OperationKind::ProjectOpen
            || !matches!(self.binding, StoreBinding::Unbound { .. })
        {
            return Err(ProjectStoreServiceError::InvalidOperationToken);
        }
        let request_id = self.allocate_request_id()?;
        self.actor()?.try_submit(ProjectStoreCommand::Open {
            request_id,
            path: path.clone(),
            mode,
        })?;
        self.active_foreground = Some(ActiveForeground {
            request_id,
            cancellation_request: None,
            kind: ForegroundKind::Open { token, path, mode },
        });
        Ok(request_id)
    }

    pub fn submit_save(
        &mut self,
        token: OperationToken,
        projection: ProjectGenerationProjection,
        initial_destination: Option<ProjectStorePath>,
        object_sources: Vec<Box<dyn ProjectObjectSource>>,
    ) -> Result<ProjectStoreRequestId, ProjectStoreServiceError> {
        self.require_writable_idle()?;
        if token.kind() != OperationKind::ProjectSave
            || token.project_id() != Some(projection.state().project_id())
            || token.project_revision() != Some(projection.revision())
        {
            return Err(ProjectStoreServiceError::InvalidOperationToken);
        }
        let project_id = projection.state().project_id();
        let revision = projection.revision();
        let revision_high_water = projection.revision_high_water().clone();
        let request_id = self.allocate_request_id()?;

        let (command, kind) = match &self.binding {
            StoreBinding::Unbound { .. } => {
                let destination =
                    initial_destination.ok_or(ProjectStoreServiceError::SaveAsRequired)?;
                let capture =
                    ProjectCommitCapture::new(projection, None, None, None, object_sources)?;
                (
                    ProjectStoreCommand::Create {
                        request_id,
                        destination: destination.clone(),
                        capture,
                    },
                    ForegroundKind::Create {
                        token,
                        destination,
                        project_id,
                        revision,
                        revision_high_water,
                    },
                )
            }
            StoreBinding::Provisional(facts) => {
                if facts.project_id != project_id {
                    return Err(ProjectStoreServiceError::ProjectMismatch);
                }
                let destination =
                    initial_destination.ok_or(ProjectStoreServiceError::SaveAsRequired)?;
                let capture =
                    ProjectCommitCapture::new(projection, None, None, None, object_sources)?;
                (
                    ProjectStoreCommand::Create {
                        request_id,
                        destination: destination.clone(),
                        capture,
                    },
                    ForegroundKind::Create {
                        token,
                        destination,
                        project_id,
                        revision,
                        revision_high_water,
                    },
                )
            }
            StoreBinding::Established(facts) => {
                if initial_destination.is_some() {
                    return Err(ProjectStoreServiceError::SaveAsRequired);
                }
                if facts.project_id != project_id {
                    return Err(ProjectStoreServiceError::ProjectMismatch);
                }
                if facts.mode != ProjectOpenMode::PreferWritable {
                    return Err(ProjectStoreServiceError::ReadOnly);
                }
                let expected_parent = facts
                    .current_manual
                    .ok_or(ProjectStoreServiceError::UnexpectedCompletion)?;
                let capture = ProjectCommitCapture::new(
                    projection,
                    Some(expected_parent),
                    None,
                    None,
                    object_sources,
                )?;
                (
                    ProjectStoreCommand::ManualSave {
                        request_id,
                        capture,
                    },
                    ForegroundKind::ManualSave {
                        token,
                        revision,
                        revision_high_water,
                        expected_parent,
                    },
                )
            }
            StoreBinding::RecoveryOnly | StoreBinding::RecoverySelected { .. } => {
                return Err(ProjectStoreServiceError::SaveAsRequired);
            }
            StoreBinding::Closed => return Err(ProjectStoreServiceError::Closing),
        };
        self.actor()?.try_submit(command)?;
        self.active_foreground = Some(ActiveForeground {
            request_id,
            cancellation_request: None,
            kind,
        });
        Ok(request_id)
    }

    pub fn submit_save_as(
        &mut self,
        snapshot: &ApplicationSnapshot,
        token: OperationToken,
        destination: ProjectStorePath,
        projection: ProjectGenerationProjection,
        object_sources: Vec<Box<dyn ProjectObjectSource>>,
    ) -> Result<ProjectStoreRequestId, ProjectStoreServiceError> {
        self.require_writable_idle()?;
        if token.kind() != OperationKind::ProjectSaveAs {
            return Err(ProjectStoreServiceError::InvalidOperationToken);
        }
        let expected_projection =
            save_as_projection_from_snapshot(snapshot, &token, projection.state().project_id())?;
        if projection != expected_projection {
            return Err(ProjectStoreServiceError::InvalidProjection);
        }
        let (source_project_id, source_generation) = match &self.binding {
            StoreBinding::Established(facts) => (
                facts.project_id,
                facts
                    .current_manual
                    .ok_or(ProjectStoreServiceError::UnexpectedCompletion)?,
            ),
            StoreBinding::RecoverySelected {
                facts,
                selected_generation,
            } => (facts.project_id, *selected_generation),
            StoreBinding::Provisional(_) | StoreBinding::Unbound { .. } => {
                return Err(ProjectStoreServiceError::OperationConflict);
            }
            StoreBinding::RecoveryOnly => {
                return Err(ProjectStoreServiceError::RecoveryCandidateUnavailable);
            }
            StoreBinding::Closed => return Err(ProjectStoreServiceError::Closing),
        };
        if projection.state().project_id() == source_project_id
            || token.project_id() != Some(source_project_id)
        {
            return Err(ProjectStoreServiceError::InvalidProjection);
        }
        let request_id = self.allocate_request_id()?;
        let capture = ProjectCommitCapture::new(
            projection.clone(),
            None,
            None,
            Some((source_project_id, source_generation)),
            object_sources,
        )?;
        self.actor()?.try_submit(ProjectStoreCommand::SaveAs {
            request_id,
            destination: destination.clone(),
            source_generation,
            capture,
        })?;
        self.active_foreground = Some(ActiveForeground {
            request_id,
            cancellation_request: None,
            kind: ForegroundKind::SaveAs {
                token,
                destination,
                source_project_id,
                source_generation,
                projection,
            },
        });
        Ok(request_id)
    }

    pub fn submit_inspect_recovery(
        &mut self,
    ) -> Result<ProjectStoreRequestId, ProjectStoreServiceError> {
        self.require_idle()?;
        if matches!(
            self.binding,
            StoreBinding::Unbound { .. } | StoreBinding::Closed
        ) {
            return Err(ProjectStoreServiceError::OperationConflict);
        }
        self.submit_inspection(InspectionContext::Explicit)
    }

    pub fn accept_normal_open(
        &mut self,
        operation_id: OperationId,
    ) -> Result<ProjectStoreServiceEvent, ProjectStoreServiceError> {
        if self.active_foreground.is_some() || self.close_request.is_some() {
            return Err(ProjectStoreServiceError::OperationConflict);
        }
        let pending = self
            .pending_normal_open
            .take()
            .ok_or(ProjectStoreServiceError::OperationConflict)?;
        if pending.token.kind() != OperationKind::ProjectOpen
            || pending.token.operation_id() != operation_id
        {
            self.pending_normal_open = Some(pending);
            return Err(ProjectStoreServiceError::InvalidOperationToken);
        }
        Ok(ProjectStoreServiceEvent::Opened {
            token: pending.token,
            projection: Box::new(pending.projection),
            candidates: pending.candidates,
            opens_dirty: pending.opens_dirty,
        })
    }

    pub fn submit_open_recovery(
        &mut self,
        token: OperationToken,
        generation_id: ProjectGenerationId,
    ) -> Result<ProjectStoreRequestId, ProjectStoreServiceError> {
        self.require_no_actor_work()?;
        if !matches!(
            token.kind(),
            OperationKind::ProjectOpen | OperationKind::ProjectRecovery
        ) {
            return Err(ProjectStoreServiceError::InvalidOperationToken);
        }
        if !self
            .recovery_candidates
            .iter()
            .any(|candidate| candidate.generation_id() == generation_id)
        {
            return Err(ProjectStoreServiceError::RecoveryCandidateUnavailable);
        }
        let pending_normal = match self.pending_normal_open.take() {
            Some(pending) if pending.token == token => Some(pending),
            Some(pending) => {
                self.pending_normal_open = Some(pending);
                return Err(ProjectStoreServiceError::InvalidOperationToken);
            }
            None => None,
        };
        let request_id = self.allocate_request_id()?;
        if let Err(fault) = self.actor()?.try_submit(ProjectStoreCommand::OpenRecovery {
            request_id,
            generation_id,
        }) {
            self.pending_normal_open = pending_normal;
            return Err(fault.into());
        }
        self.active_foreground = Some(ActiveForeground {
            request_id,
            cancellation_request: None,
            kind: ForegroundKind::OpenRecovery {
                token,
                generation_id,
                pending_normal,
            },
        });
        Ok(request_id)
    }

    pub fn cancel_operation(
        &mut self,
        operation_id: OperationId,
    ) -> Result<ProjectStoreRequestId, ProjectStoreServiceError> {
        let active = self
            .active_foreground
            .as_ref()
            .ok_or(ProjectStoreServiceError::OperationConflict)?;
        if active.kind.token().map(OperationToken::operation_id) != Some(operation_id)
            || active.cancellation_request.is_some()
        {
            return Err(ProjectStoreServiceError::OperationConflict);
        }
        let target_request_id = active.request_id;
        let request_id = self.allocate_request_id()?;
        self.actor()?.try_submit(ProjectStoreCommand::Cancel {
            request_id,
            target_request_id,
        })?;
        self.active_foreground
            .as_mut()
            .expect("foreground was checked")
            .cancellation_request = Some(request_id);
        Ok(request_id)
    }

    pub fn cancel_active_autosave(
        &mut self,
    ) -> Result<ProjectStoreRequestId, ProjectStoreServiceError> {
        let active = self
            .active_autosave
            .as_ref()
            .ok_or(ProjectStoreServiceError::OperationConflict)?;
        if active.cancellation_request.is_some() {
            return Err(ProjectStoreServiceError::OperationConflict);
        }
        let target_request_id = active.request_id;
        let request_id = self.allocate_request_id()?;
        self.actor()?.try_submit(ProjectStoreCommand::Cancel {
            request_id,
            target_request_id,
        })?;
        self.active_autosave
            .as_mut()
            .expect("autosave was checked")
            .cancellation_request = Some(request_id);
        Ok(request_id)
    }

    pub fn cancel_pending_open(
        &mut self,
        operation_id: OperationId,
    ) -> Result<ProjectStoreServiceEvent, ProjectStoreServiceError> {
        self.require_no_actor_work()?;
        let pending = self
            .pending_normal_open
            .as_ref()
            .ok_or(ProjectStoreServiceError::OperationConflict)?;
        if pending.token.operation_id() != operation_id {
            return Err(ProjectStoreServiceError::InvalidOperationToken);
        }
        let token = pending.token.clone();
        self.close()?;
        self.recovery_candidates.clear();
        Ok(ProjectStoreServiceEvent::OperationFailed {
            token,
            fault: ProjectStoreFault::Cancelled,
        })
    }

    pub fn close(&mut self) -> Result<ProjectStoreRequestId, ProjectStoreServiceError> {
        if self.close_request.is_some() || matches!(self.binding, StoreBinding::Closed) {
            return Err(ProjectStoreServiceError::Closing);
        }
        let request_id = self.allocate_request_id()?;
        self.actor()?
            .try_submit(ProjectStoreCommand::Close { request_id })?;
        self.close_request = Some(request_id);
        self.pending_normal_open = None;
        Ok(request_id)
    }

    pub fn drive<F>(
        &mut self,
        snapshot: &ApplicationSnapshot,
        source_factory: F,
    ) -> Result<Vec<ProjectStoreServiceEvent>, ProjectStoreServiceError>
    where
        F: FnOnce(
            &ProjectGenerationProjection,
        ) -> Result<Vec<Box<dyn ProjectObjectSource>>, ProjectStoreFault>,
    {
        let tick = self.clock.now();
        self.scheduler.validate_tick(tick)?;
        let mut events = Vec::with_capacity(COMPLETIONS_PER_DRIVE_MAX + 1);
        for _ in 0..COMPLETIONS_PER_DRIVE_MAX {
            let Some(completion) = self.actor()?.try_recv() else {
                break;
            };
            events.extend(self.handle_completion(completion)?);
        }
        if !events.is_empty() {
            // Any completion can change the reducer snapshot or retire work.
            // Re-observe on the next drive rather than schedule autosave from
            // the necessarily pre-completion snapshot supplied by this call.
            self.cached_repaint_after = Some(Duration::ZERO);
            return Ok(events);
        }

        self.cancel_stale_autosave_if_needed(snapshot)?;

        let commit_active = self.active_foreground.is_some()
            || self.active_autosave.is_some()
            || self.pending_normal_open.is_some()
            || self.close_request.is_some();
        let observation = AutosaveObservation::from_snapshot(
            snapshot,
            self.binding.writable_for_autosave(),
            commit_active,
            self.writes_suspended,
        );
        let due = self.scheduler.observe(tick, observation)?;
        if let Some(due) = due {
            let projection = projection_from_snapshot(snapshot)?;
            if projection.state().project_id() != due.project_id
                || projection.revision() != due.revision
            {
                return Err(ProjectStoreServiceError::InvalidApplicationSnapshot);
            }
            let facts = self.binding.autosave_capture_facts(due.project_id)?;
            let sources = source_factory(&projection)?;
            let source_identity = *projection.state().dataset().scientific_content_id();
            let revision_high_water = projection.revision_high_water().clone();
            let capture = ProjectCommitCapture::new(
                projection,
                facts.expected_parent,
                facts.autosave_base,
                None,
                sources,
            )?;
            let request_id = self.allocate_request_id()?;
            self.actor()?.try_submit(ProjectStoreCommand::Autosave {
                request_id,
                destination: facts.destination,
                capture,
            })?;
            self.active_autosave = Some(ActiveAutosave {
                request_id,
                project_id: due.project_id,
                source_identity,
                revision: due.revision,
                revision_high_water,
                expected_parent: facts.expected_parent,
                autosave_base: facts.autosave_base,
                cancellation_request: None,
                stale_source_observed: false,
            });
            events.push(ProjectStoreServiceEvent::AutosaveSubmitted {
                request_id,
                revision: due.revision,
            });
        }

        let final_observation = AutosaveObservation::from_snapshot(
            snapshot,
            self.binding.writable_for_autosave(),
            self.active_foreground.is_some()
                || self.active_autosave.is_some()
                || self.pending_normal_open.is_some()
                || self.close_request.is_some(),
            self.writes_suspended,
        );
        self.cached_repaint_after = if final_observation.eligible() {
            self.scheduler
                .pending_deadline()?
                .map(|deadline| Duration::from_nanos(deadline.saturating_sub(tick)))
        } else {
            None
        };
        Ok(events)
    }

    pub const fn has_pending_work(&self) -> bool {
        self.active_foreground.is_some()
            || self.active_autosave.is_some()
            || self.close_request.is_some()
    }

    pub const fn repaint_after(&self) -> Option<Duration> {
        self.cached_repaint_after
    }

    pub const fn writes_suspended(&self) -> bool {
        self.writes_suspended
    }

    pub fn join(mut self) -> Result<(), ProjectStoreServiceError> {
        self.actor
            .take()
            .expect("service owns one actor")
            .join()
            .map_err(|_| ProjectStoreServiceError::ActorPanicked)
    }

    fn actor(&self) -> Result<&ProjectStoreActor, ProjectStoreServiceError> {
        self.actor
            .as_ref()
            .ok_or(ProjectStoreServiceError::UnexpectedCompletion)
    }

    fn require_no_actor_work(&self) -> Result<(), ProjectStoreServiceError> {
        if self.active_foreground.is_some()
            || self.active_autosave.is_some()
            || self.close_request.is_some()
        {
            Err(ProjectStoreServiceError::OperationConflict)
        } else {
            Ok(())
        }
    }

    fn require_idle(&self) -> Result<(), ProjectStoreServiceError> {
        self.require_no_actor_work()?;
        if self.pending_normal_open.is_some() {
            Err(ProjectStoreServiceError::OperationConflict)
        } else {
            Ok(())
        }
    }

    fn require_writable_idle(&self) -> Result<(), ProjectStoreServiceError> {
        self.require_idle()?;
        if self.writes_suspended {
            Err(ProjectStoreServiceError::WritesSuspended)
        } else {
            Ok(())
        }
    }

    fn cancel_stale_autosave_if_needed(
        &mut self,
        snapshot: &ApplicationSnapshot,
    ) -> Result<(), ProjectStoreServiceError> {
        let should_cancel = self.active_autosave.as_ref().is_some_and(|active| {
            !active.stale_source_observed
                && active.cancellation_request.is_none()
                && !active_autosave_source_is_current(active, snapshot)
        });
        if should_cancel {
            self.cancel_active_autosave()?;
            self.active_autosave
                .as_mut()
                .expect("the active autosave accepted cancellation")
                .stale_source_observed = true;
        }
        Ok(())
    }

    fn submit_inspection(
        &mut self,
        context: InspectionContext,
    ) -> Result<ProjectStoreRequestId, ProjectStoreServiceError> {
        let request_id = self.allocate_request_id()?;
        self.actor()?
            .try_submit(ProjectStoreCommand::InspectRecovery { request_id })?;
        self.active_foreground = Some(ActiveForeground {
            request_id,
            cancellation_request: None,
            kind: ForegroundKind::InspectRecovery { context },
        });
        Ok(request_id)
    }

    fn handle_completion(
        &mut self,
        completion: ProjectStoreCompletion,
    ) -> Result<Vec<ProjectStoreServiceEvent>, ProjectStoreServiceError> {
        match completion {
            ProjectStoreCompletion::Cancelled { request_id, result } => {
                self.handle_cancellation_ack(request_id, result)
            }
            ProjectStoreCompletion::Autosaved { request_id, result } => {
                self.handle_autosave_completion(request_id, result)
            }
            ProjectStoreCompletion::Closed { request_id, result } => {
                if self.close_request != Some(request_id) {
                    return Err(ProjectStoreServiceError::UnexpectedCompletion);
                }
                self.close_request = None;
                self.binding = StoreBinding::Closed;
                self.active_foreground = None;
                self.active_autosave = None;
                self.recovery_candidates.clear();
                Ok(vec![ProjectStoreServiceEvent::Closed {
                    request_id,
                    result,
                }])
            }
            completion => self.handle_foreground_completion(completion),
        }
    }

    fn handle_cancellation_ack(
        &mut self,
        request_id: ProjectStoreRequestId,
        result: Result<(), ProjectStoreFault>,
    ) -> Result<Vec<ProjectStoreServiceEvent>, ProjectStoreServiceError> {
        result?;
        if let Some(active) = self.active_foreground.as_mut()
            && active.cancellation_request == Some(request_id)
        {
            active.cancellation_request = None;
            return Ok(vec![ProjectStoreServiceEvent::CancellationAcknowledged {
                request_id,
                target_request_id: active.request_id,
            }]);
        }
        if let Some(active) = self.active_autosave.as_mut()
            && active.cancellation_request == Some(request_id)
        {
            active.cancellation_request = None;
            return Ok(vec![ProjectStoreServiceEvent::CancellationAcknowledged {
                request_id,
                target_request_id: active.request_id,
            }]);
        }
        Err(ProjectStoreServiceError::UnexpectedCompletion)
    }

    fn handle_autosave_completion(
        &mut self,
        request_id: ProjectStoreRequestId,
        result: Result<ProjectStoreReceipt, ProjectStoreFault>,
    ) -> Result<Vec<ProjectStoreServiceEvent>, ProjectStoreServiceError> {
        let active = self
            .active_autosave
            .take()
            .ok_or(ProjectStoreServiceError::UnexpectedCompletion)?;
        if request_id != active.request_id {
            self.active_autosave = Some(active);
            return Err(ProjectStoreServiceError::UnexpectedCompletion);
        }
        if let Ok(receipt) = &result {
            if receipt.captured_revision() != active.revision
                || receipt.captured_revision_high_water() != &active.revision_high_water
                || receipt.previous_generation_id() != active.expected_parent
                || receipt.autosave_base_generation_id() != active.autosave_base
            {
                self.writes_suspended = true;
                return Err(ProjectStoreServiceError::UnexpectedCompletion);
            }
            self.binding.accept_autosave(active.project_id, receipt)?;
        } else if result == Err(ProjectStoreFault::CommitIndeterminate) {
            self.writes_suspended = true;
        }
        Ok(vec![ProjectStoreServiceEvent::AutosaveFinished {
            request_id,
            revision: active.revision,
            result,
        }])
    }

    fn handle_foreground_completion(
        &mut self,
        completion: ProjectStoreCompletion,
    ) -> Result<Vec<ProjectStoreServiceEvent>, ProjectStoreServiceError> {
        let request_id = completion.request_id();
        let active = self
            .active_foreground
            .take()
            .ok_or(ProjectStoreServiceError::UnexpectedCompletion)?;
        if request_id != active.request_id {
            self.active_foreground = Some(active);
            return Err(ProjectStoreServiceError::UnexpectedCompletion);
        }
        match (active.kind, completion) {
            (
                ForegroundKind::Open { token, path, mode },
                ProjectStoreCompletion::Opened { result, .. },
            ) => match result {
                Ok((session, projection)) => {
                    let facts = SessionFacts::from_session(session);
                    if facts.path != path || facts.project_id != projection.state().project_id() {
                        return Err(ProjectStoreServiceError::UnexpectedCompletion);
                    }
                    let mode_is_valid = match mode {
                        ProjectOpenMode::ReadOnly => facts.mode == ProjectOpenMode::ReadOnly,
                        ProjectOpenMode::PreferWritable => true,
                    };
                    if mode_is_valid {
                        let (binding, opens_dirty) = StoreBinding::from_opened(facts)?;
                        self.binding = binding;
                        if opens_dirty {
                            let generation_id = self
                                .binding
                                .facts()
                                .and_then(|facts| facts.current_autosave)
                                .ok_or(ProjectStoreServiceError::UnexpectedCompletion)?;
                            let request_id = self.allocate_request_id()?;
                            if let Err(fault) =
                                self.actor()?.try_submit(ProjectStoreCommand::OpenRecovery {
                                    request_id,
                                    generation_id,
                                })
                            {
                                self.binding = StoreBinding::RecoveryOnly;
                                return Ok(vec![ProjectStoreServiceEvent::OpenFailed {
                                    token,
                                    fault,
                                    candidates: Vec::new(),
                                }]);
                            }
                            self.active_foreground = Some(ActiveForeground {
                                request_id,
                                cancellation_request: None,
                                kind: ForegroundKind::SelectProvisionalOpen {
                                    token,
                                    generation_id,
                                    expected_projection: Box::new(projection),
                                },
                            });
                        } else {
                            self.submit_inspection(InspectionContext::HealthyOpen {
                                token,
                                projection: Box::new(projection),
                                opens_dirty: false,
                            })?;
                        }
                        Ok(Vec::new())
                    } else {
                        Err(ProjectStoreServiceError::UnexpectedCompletion)
                    }
                }
                Err(fault) => {
                    self.submit_inspection(InspectionContext::FailedOpen { token, fault })?;
                    Ok(Vec::new())
                }
            },
            (
                ForegroundKind::InspectRecovery { context },
                ProjectStoreCompletion::RecoveryInspected { result, .. },
            ) => self.finish_recovery_inspection(context, result),
            (
                ForegroundKind::Create {
                    token,
                    destination,
                    project_id,
                    revision,
                    revision_high_water,
                },
                ProjectStoreCompletion::Created { result, .. },
            ) => match result {
                Ok(session) => {
                    let facts = SessionFacts::from_session(session);
                    if facts.path != destination
                        || facts.project_id != project_id
                        || facts.mode != ProjectOpenMode::PreferWritable
                        || facts.current_manual.is_none()
                    {
                        self.writes_suspended = true;
                        return Err(ProjectStoreServiceError::UnexpectedCompletion);
                    }
                    let _ = revision_high_water;
                    self.binding = StoreBinding::Established(facts);
                    self.recovery_candidates.clear();
                    Ok(vec![ProjectStoreServiceEvent::Created {
                        token,
                        saved_revision: revision,
                    }])
                }
                Err(fault) => Ok(vec![self.failed_mutation_event(token, fault)]),
            },
            (
                ForegroundKind::ManualSave {
                    token,
                    revision,
                    revision_high_water,
                    expected_parent,
                },
                ProjectStoreCompletion::ManualSaved { result, .. },
            ) => match result {
                Ok(receipt) => {
                    if receipt.captured_revision() != revision
                        || receipt.captured_revision_high_water() != &revision_high_water
                        || receipt.previous_generation_id() != Some(expected_parent)
                        || receipt.autosave_base_generation_id().is_some()
                    {
                        self.writes_suspended = true;
                        return Err(ProjectStoreServiceError::UnexpectedCompletion);
                    }
                    let StoreBinding::Established(facts) = &mut self.binding else {
                        return Err(ProjectStoreServiceError::UnexpectedCompletion);
                    };
                    facts.current_manual = Some(receipt.current_generation_id());
                    facts.current_autosave = None;
                    self.recovery_candidates.clear();
                    Ok(vec![ProjectStoreServiceEvent::ManualSaved {
                        token,
                        receipt,
                    }])
                }
                Err(fault) => Ok(vec![self.failed_mutation_event(token, fault)]),
            },
            (
                ForegroundKind::SaveAs {
                    token,
                    destination,
                    source_project_id,
                    source_generation,
                    projection,
                },
                ProjectStoreCompletion::SavedAs { result, .. },
            ) => match result {
                Ok(receipt) => {
                    if receipt.captured_revision() != projection.revision()
                        || receipt.captured_revision_high_water()
                            != projection.revision_high_water()
                        || receipt.previous_generation_id().is_some()
                        || receipt.autosave_base_generation_id().is_some()
                        || projection.state().project_id() == source_project_id
                    {
                        self.writes_suspended = true;
                        return Err(ProjectStoreServiceError::UnexpectedCompletion);
                    }
                    let _ = source_generation;
                    self.binding = StoreBinding::Established(SessionFacts {
                        path: destination,
                        project_id: projection.state().project_id(),
                        mode: ProjectOpenMode::PreferWritable,
                        current_manual: Some(receipt.current_generation_id()),
                        current_autosave: None,
                    });
                    self.recovery_candidates.clear();
                    Ok(vec![ProjectStoreServiceEvent::SavedAs {
                        token,
                        projection: Box::new(projection),
                        receipt,
                    }])
                }
                Err(fault) => Ok(vec![self.failed_mutation_event(token, fault)]),
            },
            (
                ForegroundKind::OpenRecovery {
                    token,
                    generation_id,
                    pending_normal,
                },
                ProjectStoreCompletion::RecoveryOpened { result, .. },
            ) => match result {
                Ok((session, projection)) => {
                    let facts = SessionFacts::from_session(session);
                    facts.validate_shape()?;
                    if facts.project_id != projection.state().project_id() {
                        return Err(ProjectStoreServiceError::UnexpectedCompletion);
                    }
                    self.binding = StoreBinding::RecoverySelected {
                        facts,
                        selected_generation: generation_id,
                    };
                    Ok(vec![ProjectStoreServiceEvent::RecoveryOpened {
                        token,
                        generation_id,
                        projection: Box::new(projection),
                    }])
                }
                Err(fault) => {
                    let normal_open_still_available = pending_normal.is_some();
                    self.pending_normal_open = pending_normal;
                    Ok(vec![ProjectStoreServiceEvent::RecoverySelectionFailed {
                        token,
                        fault,
                        normal_open_still_available,
                    }])
                }
            },
            (
                ForegroundKind::SelectProvisionalOpen {
                    token,
                    generation_id,
                    expected_projection,
                },
                ProjectStoreCompletion::RecoveryOpened { result, .. },
            ) => match result {
                Ok((session, projection)) => {
                    let facts = SessionFacts::from_session(session);
                    facts.validate_shape()?;
                    if facts.project_id != projection.state().project_id()
                        || projection != *expected_projection
                    {
                        self.binding = StoreBinding::RecoveryOnly;
                        return Err(ProjectStoreServiceError::UnexpectedCompletion);
                    }
                    self.binding = StoreBinding::RecoverySelected {
                        facts,
                        selected_generation: generation_id,
                    };
                    self.submit_inspection(InspectionContext::HealthyOpen {
                        token,
                        projection: Box::new(projection),
                        opens_dirty: true,
                    })?;
                    Ok(Vec::new())
                }
                Err(fault) => {
                    self.submit_inspection(InspectionContext::FailedOpen { token, fault })?;
                    Ok(Vec::new())
                }
            },
            (kind, unexpected) => {
                self.active_foreground = Some(ActiveForeground {
                    request_id,
                    cancellation_request: active.cancellation_request,
                    kind,
                });
                let _ = unexpected;
                Err(ProjectStoreServiceError::UnexpectedCompletion)
            }
        }
    }

    fn finish_recovery_inspection(
        &mut self,
        context: InspectionContext,
        result: Result<Vec<ProjectRecoveryCandidate>, ProjectStoreFault>,
    ) -> Result<Vec<ProjectStoreServiceEvent>, ProjectStoreServiceError> {
        match (context, result) {
            (
                InspectionContext::HealthyOpen {
                    token,
                    projection,
                    opens_dirty,
                },
                Ok(candidates),
            ) => {
                self.recovery_candidates = candidates.clone();
                let automatic_newer = if opens_dirty {
                    None
                } else {
                    candidates
                        .iter()
                        .find(|candidate| candidate.is_newer())
                        .map(ProjectRecoveryCandidate::generation_id)
                };
                if let Some(automatic_newer) = automatic_newer {
                    self.pending_normal_open = Some(PendingNormalOpen {
                        token: token.clone(),
                        projection: *projection,
                        candidates: candidates.clone(),
                        opens_dirty,
                    });
                    Ok(vec![ProjectStoreServiceEvent::RecoveryReviewRequired {
                        token,
                        candidates,
                        automatic_newer,
                    }])
                } else {
                    Ok(vec![ProjectStoreServiceEvent::Opened {
                        token,
                        projection,
                        candidates,
                        opens_dirty,
                    }])
                }
            }
            (
                InspectionContext::HealthyOpen {
                    token,
                    projection,
                    opens_dirty,
                },
                Err(fault),
            ) => {
                self.recovery_candidates.clear();
                self.binding = StoreBinding::RecoveryOnly;
                let _ = (projection, opens_dirty);
                Ok(vec![ProjectStoreServiceEvent::OpenFailed {
                    token,
                    fault,
                    candidates: Vec::new(),
                }])
            }
            (InspectionContext::FailedOpen { token, fault }, Ok(candidates)) => {
                self.binding = StoreBinding::RecoveryOnly;
                self.recovery_candidates = candidates.clone();
                Ok(vec![ProjectStoreServiceEvent::OpenFailed {
                    token,
                    fault,
                    candidates,
                }])
            }
            (InspectionContext::FailedOpen { token, fault }, Err(inspection_fault)) => {
                self.recovery_candidates.clear();
                Ok(vec![
                    ProjectStoreServiceEvent::OpenFailed {
                        token,
                        fault,
                        candidates: Vec::new(),
                    },
                    ProjectStoreServiceEvent::RecoveryInspectionFailed {
                        fault: inspection_fault,
                    },
                ])
            }
            (InspectionContext::Explicit, Ok(candidates)) => {
                self.recovery_candidates = candidates.clone();
                Ok(vec![ProjectStoreServiceEvent::RecoveryCandidatesListed {
                    candidates,
                }])
            }
            (InspectionContext::Explicit, Err(fault)) => {
                self.recovery_candidates.clear();
                Ok(vec![ProjectStoreServiceEvent::RecoveryInspectionFailed {
                    fault,
                }])
            }
        }
    }

    fn failed_mutation_event(
        &mut self,
        token: OperationToken,
        fault: ProjectStoreFault,
    ) -> ProjectStoreServiceEvent {
        if fault == ProjectStoreFault::CommitIndeterminate {
            self.writes_suspended = true;
        }
        ProjectStoreServiceEvent::OperationFailed { token, fault }
    }

    fn allocate_request_id(&mut self) -> Result<ProjectStoreRequestId, ProjectStoreServiceError> {
        let request_id = ProjectStoreRequestId::new(self.next_request_id)
            .ok_or(ProjectStoreServiceError::RequestIdOverflow)?;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or(ProjectStoreServiceError::RequestIdOverflow)?;
        Ok(request_id)
    }
}

fn active_autosave_source_is_current(
    active: &ActiveAutosave,
    snapshot: &ApplicationSnapshot,
) -> bool {
    let SourceVerificationSnapshot::Verified(source) = snapshot.source() else {
        return false;
    };
    let WorkspaceSnapshot::Bound { project, .. } = snapshot.workspace() else {
        return false;
    };
    project.project_id() == active.project_id
        && project.dataset().scientific_content_id() == &active.source_identity
        && source.scientific_content_id() == &active.source_identity
        && project.dataset().has_same_scientific_content(source)
}

fn projection_from_snapshot(
    snapshot: &ApplicationSnapshot,
) -> Result<ProjectGenerationProjection, ProjectStoreServiceError> {
    if !matches!(snapshot.source(), SourceVerificationSnapshot::Verified(_)) {
        return Err(ProjectStoreServiceError::InvalidApplicationSnapshot);
    }
    let WorkspaceSnapshot::Bound {
        project,
        revision,
        revision_high_water,
        dirty: true,
        ..
    } = snapshot.workspace()
    else {
        return Err(ProjectStoreServiceError::InvalidApplicationSnapshot);
    };
    ProjectGenerationProjection::new(
        *revision,
        revision_high_water.clone(),
        project.as_ref().clone(),
    )
    .map_err(|_| ProjectStoreServiceError::InvalidProjection)
}

fn save_as_projection_from_snapshot(
    snapshot: &ApplicationSnapshot,
    token: &OperationToken,
    new_project_id: ProjectId,
) -> Result<ProjectGenerationProjection, ProjectStoreServiceError> {
    if !snapshot
        .active_operations()
        .iter()
        .any(|active| active == token)
    {
        return Err(ProjectStoreServiceError::InvalidOperationToken);
    }
    let WorkspaceSnapshot::Bound {
        project,
        revision,
        revision_high_water: _,
        ..
    } = snapshot.workspace()
    else {
        return Err(ProjectStoreServiceError::InvalidApplicationSnapshot);
    };
    if token.kind() != OperationKind::ProjectSaveAs
        || token.project_id() != Some(project.project_id())
        || token.project_revision() != Some(*revision)
        || token.target_project_id() != Some(new_project_id)
        || new_project_id == project.project_id()
    {
        return Err(ProjectStoreServiceError::InvalidOperationToken);
    }
    let SourceVerificationSnapshot::Verified(source) = snapshot.source() else {
        return Err(ProjectStoreServiceError::InvalidApplicationSnapshot);
    };
    if token.source_identity() != Some(*source.scientific_content_id())
        || !project.dataset().has_same_scientific_content(source)
    {
        return Err(ProjectStoreServiceError::InvalidApplicationSnapshot);
    }
    let state = ProjectState::new(
        new_project_id,
        project.dataset().clone(),
        project.view().clone(),
        project.channel_presets().to_vec(),
        project.artifacts().to_vec(),
    )
    .map_err(|_| ProjectStoreServiceError::InvalidProjection)?;
    ProjectGenerationProjection::new(
        ProjectRevisionId::initial(new_project_id),
        ProjectRevisionHighWater::initial(new_project_id),
        state,
    )
    .map_err(|_| ProjectStoreServiceError::InvalidProjection)
}

#[cfg(test)]
#[path = "../tests/support/project_store_service.rs"]
mod tests;

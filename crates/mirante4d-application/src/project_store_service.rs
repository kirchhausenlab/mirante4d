//! Compile/test-only WP-10B B3 service over the project-store actor.
//!
//! The product composition root must not construct or poll this module before
//! the atomic B4 persistence cutover.

#![cfg(target_os = "linux")]
#![cfg_attr(not(test), allow(dead_code))]

use mirante4d_project_model::{
    ProjectGenerationProjection, ProjectId, ProjectRevisionHighWater, ProjectRevisionId,
};
use mirante4d_project_store::{
    ProjectCommitCapture, ProjectGenerationId, ProjectObjectSource, ProjectOpenMode,
    ProjectStoreActor, ProjectStoreCommand, ProjectStoreCompletion, ProjectStoreConfig,
    ProjectStoreFault, ProjectStorePath, ProjectStoreReceipt, ProjectStoreRequestId,
};

use super::{ApplicationSnapshot, SourceVerificationSnapshot, WorkspaceSnapshot};

const NANOS_PER_SECOND: u64 = 1_000_000_000;
const AUTOSAVE_IDLE_DELAY_TICKS: u64 = 30 * NANOS_PER_SECOND;
const AUTOSAVE_MAX_DELAY_TICKS: u64 = 120 * NANOS_PER_SECOND;

pub(crate) trait MonotonicClock {
    /// Returns elapsed monotonic nanoseconds from an arbitrary stable origin.
    fn now(&self) -> u64;
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
}

#[derive(Debug)]
enum StoreBinding {
    Unbound {
        provisional_destination: Option<ProjectStorePath>,
    },
    Bound {
        project_id: ProjectId,
        mode: ProjectOpenMode,
        current_manual: Option<ProjectGenerationId>,
        current_autosave: Option<ProjectGenerationId>,
        forked_from: Option<(ProjectId, ProjectGenerationId)>,
    },
}

impl StoreBinding {
    fn writable(&self) -> bool {
        match self {
            Self::Unbound {
                provisional_destination,
            } => provisional_destination.is_some(),
            Self::Bound { mode, .. } => *mode == ProjectOpenMode::PreferWritable,
        }
    }

    fn capture_facts(
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
                forked_from: None,
            }),
            Self::Unbound {
                provisional_destination: None,
            } => Err(ProjectStoreServiceError::ReadOnly),
            Self::Bound {
                project_id: bound_project,
                mode: ProjectOpenMode::PreferWritable,
                current_manual,
                current_autosave,
                forked_from,
            } if *bound_project == project_id => Ok(AutosaveCaptureFacts {
                destination: None,
                expected_parent: *current_autosave,
                autosave_base: *current_manual,
                forked_from: *forked_from,
            }),
            Self::Bound {
                project_id: bound_project,
                ..
            } if *bound_project != project_id => Err(ProjectStoreServiceError::ProjectMismatch),
            Self::Bound { .. } => Err(ProjectStoreServiceError::ReadOnly),
        }
    }

    fn accept_autosave(
        &mut self,
        project_id: ProjectId,
        receipt: &ProjectStoreReceipt,
    ) -> Result<(), ProjectStoreServiceError> {
        match self {
            Self::Unbound { .. } => {
                *self = Self::Bound {
                    project_id,
                    mode: ProjectOpenMode::PreferWritable,
                    current_manual: None,
                    current_autosave: Some(receipt.current_generation_id()),
                    forked_from: None,
                };
                Ok(())
            }
            Self::Bound {
                project_id: bound_project,
                current_autosave,
                ..
            } if *bound_project == project_id => {
                *current_autosave = Some(receipt.current_generation_id());
                Ok(())
            }
            Self::Bound { .. } => Err(ProjectStoreServiceError::ProjectMismatch),
        }
    }
}

#[derive(Clone, Debug)]
struct AutosaveCaptureFacts {
    destination: Option<ProjectStorePath>,
    expected_parent: Option<ProjectGenerationId>,
    autosave_base: Option<ProjectGenerationId>,
    forked_from: Option<(ProjectId, ProjectGenerationId)>,
}

#[derive(Clone, Debug)]
struct ActiveAutosave {
    request_id: ProjectStoreRequestId,
    project_id: ProjectId,
    revision: ProjectRevisionId,
    revision_high_water: ProjectRevisionHighWater,
    expected_parent: Option<ProjectGenerationId>,
    autosave_base: Option<ProjectGenerationId>,
    cancellation_request: Option<ProjectStoreRequestId>,
}

#[derive(Debug)]
pub(crate) enum ProjectStoreServiceEvent {
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
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProjectStoreServiceError {
    ClockRegressed { previous: u64, observed: u64 },
    ClockOverflow,
    RequestIdOverflow,
    InvalidApplicationSnapshot,
    InvalidProjection,
    OperationConflict,
    ProjectMismatch,
    ReadOnly,
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

pub(crate) struct ProjectStoreApplicationService<C> {
    actor: Option<ProjectStoreActor>,
    clock: C,
    scheduler: AutosaveScheduler,
    binding: StoreBinding,
    active_autosave: Option<ActiveAutosave>,
    next_request_id: u64,
    writes_suspended: bool,
}

impl<C: MonotonicClock> ProjectStoreApplicationService<C> {
    pub(crate) fn start(
        config: ProjectStoreConfig,
        clock: C,
        provisional_destination: Option<ProjectStorePath>,
    ) -> Result<Self, ProjectStoreServiceError> {
        Ok(Self {
            actor: Some(ProjectStoreActor::start(config)?),
            clock,
            scheduler: AutosaveScheduler::default(),
            binding: StoreBinding::Unbound {
                provisional_destination,
            },
            active_autosave: None,
            next_request_id: 1,
            writes_suspended: false,
        })
    }

    pub(crate) fn drive<F>(
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
        let mut events = Vec::with_capacity(2);
        if let Some(completion) = self
            .actor
            .as_ref()
            .ok_or(ProjectStoreServiceError::UnexpectedCompletion)?
            .try_recv()
        {
            events.push(self.handle_completion(completion)?);
        }

        let observation = AutosaveObservation::from_snapshot(
            snapshot,
            self.binding.writable(),
            self.active_autosave.is_some(),
            self.writes_suspended,
        );
        let due = self.scheduler.observe(tick, observation)?;
        if let Some(due) = due {
            if self.writes_suspended {
                return Err(ProjectStoreServiceError::WritesSuspended);
            }
            let projection = projection_from_snapshot(snapshot)?;
            if projection.state().project_id() != due.project_id
                || projection.revision() != due.revision
            {
                return Err(ProjectStoreServiceError::InvalidApplicationSnapshot);
            }
            let facts = self.binding.capture_facts(due.project_id)?;
            let sources = source_factory(&projection)?;
            let revision_high_water = projection.revision_high_water().clone();
            let capture = ProjectCommitCapture::new(
                projection,
                facts.expected_parent,
                facts.autosave_base,
                facts.forked_from,
                sources,
            )?;
            let request_id = self.allocate_request_id()?;
            self.actor
                .as_ref()
                .ok_or(ProjectStoreServiceError::UnexpectedCompletion)?
                .try_submit(ProjectStoreCommand::Autosave {
                    request_id,
                    destination: facts.destination,
                    capture,
                })?;
            self.active_autosave = Some(ActiveAutosave {
                request_id,
                project_id: due.project_id,
                revision: due.revision,
                revision_high_water,
                expected_parent: facts.expected_parent,
                autosave_base: facts.autosave_base,
                cancellation_request: None,
            });
            events.push(ProjectStoreServiceEvent::AutosaveSubmitted {
                request_id,
                revision: due.revision,
            });
        }
        Ok(events)
    }

    pub(crate) fn cancel_active_autosave(
        &mut self,
    ) -> Result<ProjectStoreRequestId, ProjectStoreServiceError> {
        let target = self
            .active_autosave
            .as_ref()
            .ok_or(ProjectStoreServiceError::OperationConflict)?;
        if target.cancellation_request.is_some() {
            return Err(ProjectStoreServiceError::OperationConflict);
        }
        let target_request_id = target.request_id;
        let request_id = self.allocate_request_id()?;
        self.actor
            .as_ref()
            .ok_or(ProjectStoreServiceError::UnexpectedCompletion)?
            .try_submit(ProjectStoreCommand::Cancel {
                request_id,
                target_request_id,
            })?;
        self.active_autosave
            .as_mut()
            .expect("active autosave was checked")
            .cancellation_request = Some(request_id);
        Ok(request_id)
    }

    pub(crate) const fn writes_suspended(&self) -> bool {
        self.writes_suspended
    }

    pub(crate) fn join(mut self) -> Result<(), ProjectStoreServiceError> {
        self.actor
            .take()
            .expect("service owns one actor")
            .join()
            .map_err(|_| ProjectStoreServiceError::ActorPanicked)
    }

    fn handle_completion(
        &mut self,
        completion: ProjectStoreCompletion,
    ) -> Result<ProjectStoreServiceEvent, ProjectStoreServiceError> {
        match completion {
            ProjectStoreCompletion::Autosaved { request_id, result } => {
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
                Ok(ProjectStoreServiceEvent::AutosaveFinished {
                    request_id,
                    revision: active.revision,
                    result,
                })
            }
            ProjectStoreCompletion::Cancelled {
                request_id,
                result: Ok(()),
            } => {
                let active = self
                    .active_autosave
                    .as_mut()
                    .ok_or(ProjectStoreServiceError::UnexpectedCompletion)?;
                if active.cancellation_request != Some(request_id) {
                    return Err(ProjectStoreServiceError::UnexpectedCompletion);
                }
                active.cancellation_request = None;
                Ok(ProjectStoreServiceEvent::CancellationAcknowledged { request_id })
            }
            _ => Err(ProjectStoreServiceError::UnexpectedCompletion),
        }
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

#[cfg(test)]
#[path = "../tests/support/project_store_service.rs"]
mod tests;

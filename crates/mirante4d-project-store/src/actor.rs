//! Execution core for one public unbound-or-session project-store actor.

#![cfg(target_os = "linux")]
#![cfg_attr(not(test), allow(dead_code))]

use std::{
    collections::{BTreeSet, VecDeque},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

use mirante4d_identity::ExactBytesHasher;
use mirante4d_project_model::{ArtifactHandleId, ArtifactReference};

#[cfg(test)]
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

#[cfg(test)]
use crate::lease::GcTransitionInjector;

use crate::{
    LoadedProjectArtifact, ProjectCommitCapture, ProjectGenerationId, ProjectOpenMode,
    ProjectStoreCommand, ProjectStoreCompletion, ProjectStoreConfig, ProjectStoreDiagnostics,
    ProjectStoreFault, ProjectStorePath, ProjectStoreReceipt, ProjectStoreRequestId,
    ProjectStoreSession,
    generation::{ArtifactStorage, GenerationDocument, LogicalObjectBinding},
    inspection::{
        RecoveryInspection, StoreStateInspection, cleanup_dead_writer_staging,
        inspect_established_store, inspect_recovery, inspect_store_state, map_local_error,
        open_recovery,
    },
    lease::{LeaseError, ProjectStoreLeases},
    local::{LocalStoreRoot, valid_checkpoint_id},
    pin::{publish_pin, remove_pin},
    transaction::{
        InitialPackageMode, InstalledInitialPackage, ensure_authenticated_copy_destination,
        install_initial_manual_package, install_initial_manual_package_from_generation,
        install_initial_provisional_autosave_package, publish_established_autosave_generation,
        publish_established_manual_generation, publish_provisional_autosave_generation,
    },
    trash::{purge_trash, trash_generations},
};

/// One private worker which owns the store root, leases, and all session work.
pub(crate) struct EstablishedProjectActor {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    request_limit: usize,
    completion_limit: usize,
    trash_selection_limit: usize,
    artifact_selection_limit: usize,
    autosave_enabled: bool,
    #[cfg(test)]
    gc_transition_injector: OnceLock<Arc<GcTransitionInjector>>,
}

struct State {
    requests: VecDeque<Work>,
    completions: VecDeque<ProjectStoreCompletion>,
    active: Option<Active>,
    close_request: Option<ProjectStoreRequestId>,
    completion_reservations: usize,
    live_request_ids: BTreeSet<ProjectStoreRequestId>,
    last_accepted_request_id: Option<ProjectStoreRequestId>,
    accepting: bool,
    shutdown: bool,
    worker_exited: bool,
}

struct Active {
    request_id: ProjectStoreRequestId,
    cancelled: Arc<AtomicBool>,
}

enum Work {
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
    Autosave {
        request_id: ProjectStoreRequestId,
        destination: Option<ProjectStorePath>,
        capture: ProjectCommitCapture,
    },
    SaveAs {
        request_id: ProjectStoreRequestId,
        destination: ProjectStorePath,
        source_generation: ProjectGenerationId,
        capture: ProjectCommitCapture,
    },
    InspectRecovery {
        request_id: ProjectStoreRequestId,
    },
    OpenRecovery {
        request_id: ProjectStoreRequestId,
        generation_id: ProjectGenerationId,
    },
    LoadArtifacts {
        request_id: ProjectStoreRequestId,
        generation_id: ProjectGenerationId,
        artifact_handles: Vec<ArtifactHandleId>,
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
}

impl Work {
    const fn request_id(&self) -> ProjectStoreRequestId {
        match self {
            Self::Create { request_id, .. }
            | Self::Open { request_id, .. }
            | Self::ManualSave { request_id, .. }
            | Self::Autosave { request_id, .. }
            | Self::SaveAs { request_id, .. }
            | Self::InspectRecovery { request_id }
            | Self::OpenRecovery { request_id, .. }
            | Self::LoadArtifacts { request_id, .. }
            | Self::Pin { request_id, .. }
            | Self::Unpin { request_id, .. }
            | Self::PlanCompaction { request_id }
            | Self::Trash { request_id, .. }
            | Self::Purge { request_id }
            | Self::FullVerify { request_id } => *request_id,
        }
    }

    const fn is_autosave(&self) -> bool {
        matches!(self, Self::Autosave { .. })
    }

    fn cancelled_completion(self) -> ProjectStoreCompletion {
        match self {
            Self::Create { request_id, .. } => ProjectStoreCompletion::Created {
                request_id,
                result: Err(ProjectStoreFault::Cancelled),
            },
            Self::Open { request_id, .. } => ProjectStoreCompletion::Opened {
                request_id,
                result: Err(ProjectStoreFault::Cancelled),
            },
            Self::ManualSave { request_id, .. } => ProjectStoreCompletion::ManualSaved {
                request_id,
                result: Err(ProjectStoreFault::Cancelled),
            },
            Self::Autosave { request_id, .. } => ProjectStoreCompletion::Autosaved {
                request_id,
                result: Err(ProjectStoreFault::Cancelled),
            },
            Self::SaveAs { request_id, .. } => ProjectStoreCompletion::SavedAs {
                request_id,
                result: Err(ProjectStoreFault::Cancelled),
            },
            Self::InspectRecovery { request_id } => ProjectStoreCompletion::RecoveryInspected {
                request_id,
                result: Err(ProjectStoreFault::Cancelled),
            },
            Self::OpenRecovery { request_id, .. } => ProjectStoreCompletion::RecoveryOpened {
                request_id,
                result: Err(ProjectStoreFault::Cancelled),
            },
            Self::LoadArtifacts { request_id, .. } => ProjectStoreCompletion::ArtifactsLoaded {
                request_id,
                result: Err(ProjectStoreFault::Cancelled),
            },
            Self::Pin { request_id, .. } => ProjectStoreCompletion::Pinned {
                request_id,
                result: Err(ProjectStoreFault::Cancelled),
            },
            Self::Unpin { request_id, .. } => ProjectStoreCompletion::Unpinned {
                request_id,
                result: Err(ProjectStoreFault::Cancelled),
            },
            Self::PlanCompaction { request_id } => ProjectStoreCompletion::CompactionPlanned {
                request_id,
                result: Err(ProjectStoreFault::Cancelled),
            },
            Self::Trash { request_id, .. } => ProjectStoreCompletion::Trashed {
                request_id,
                result: Err(ProjectStoreFault::Cancelled),
            },
            Self::Purge { request_id } => ProjectStoreCompletion::Purged {
                request_id,
                result: Err(ProjectStoreFault::Cancelled),
            },
            Self::FullVerify { request_id } => ProjectStoreCompletion::Verified {
                request_id,
                result: Err(ProjectStoreFault::Cancelled),
            },
        }
    }
}

/// The worker's sole ownership of one recovery-capable session. Mutable
/// authority facts are always reinspected from `root` rather than shadowed.
struct SessionResources {
    path: ProjectStorePath,
    root: LocalStoreRoot,
    leases: ProjectStoreLeases,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NormalSessionKind {
    Established,
    Provisional,
}

enum ActorSession {
    Normal {
        resources: SessionResources,
        kind: NormalSessionKind,
    },
    RecoveryOnly {
        resources: SessionResources,
    },
    RecoverySelected {
        resources: SessionResources,
        selected_generation: ProjectGenerationId,
    },
}

impl ActorSession {
    fn resources(&self) -> &SessionResources {
        match self {
            Self::Normal { resources, .. }
            | Self::RecoveryOnly { resources }
            | Self::RecoverySelected { resources, .. } => resources,
        }
    }

    fn resources_mut(&mut self) -> &mut SessionResources {
        match self {
            Self::Normal { resources, .. }
            | Self::RecoveryOnly { resources }
            | Self::RecoverySelected { resources, .. } => resources,
        }
    }

    fn into_resources(self) -> SessionResources {
        match self {
            Self::Normal { resources, .. }
            | Self::RecoveryOnly { resources }
            | Self::RecoverySelected { resources, .. } => resources,
        }
    }
}

impl SessionResources {
    fn from_installed(
        path: ProjectStorePath,
        project_id: mirante4d_project_model::ProjectId,
        installed: InstalledInitialPackage,
        kind: NormalSessionKind,
    ) -> (ActorSession, ProjectStoreReceipt, ProjectStoreSession) {
        let (root, leases, receipt) = installed.into_parts();
        let session = ProjectStoreSession::new(
            path.clone(),
            project_id,
            leases.effective_mode(),
            (kind == NormalSessionKind::Established).then_some(receipt.current_generation_id()),
            (kind == NormalSessionKind::Provisional).then_some(receipt.current_generation_id()),
        );
        (
            ActorSession::Normal {
                resources: Self { path, root, leases },
                kind,
            },
            receipt,
            session,
        )
    }

    fn create_from_provisional<C>(
        &mut self,
        destination: ProjectStorePath,
        capture: ProjectCommitCapture,
        limits: crate::ProjectStoreLimits,
        mut is_cancelled: C,
    ) -> Result<ProjectStoreSession, ProjectStoreFault>
    where
        C: FnMut() -> bool,
    {
        ensure_authenticated_copy_destination(&destination, &self.root, limits)?;
        match self.leases.confirm_writer(&self.root) {
            Ok(true) => {}
            Ok(false) => return Err(ProjectStoreFault::ReadOnly),
            Err(LeaseError::Indeterminate) => return Err(ProjectStoreFault::CommitIndeterminate),
            Err(LeaseError::InvalidAnchor | LeaseError::Io { .. }) => {
                return Err(ProjectStoreFault::Corruption {
                    stage: "actor_session_lease",
                });
            }
        }
        let state = inspect_store_state(&self.root, limits, &mut is_cancelled)?;
        if !state.is_provisional() {
            return Err(ProjectStoreFault::Corruption {
                stage: "create_session_kind",
            });
        }
        let source = state.authority_generation();
        let projection = capture.projection();
        if projection.state().project_id() != state.project_id()
            || !projection
                .state()
                .dataset()
                .has_same_scientific_content(source.projection().state().dataset())
            || capture.forked_from().is_some()
            || source.forked_from().is_some()
            || projection.revision_high_water().sequence()
                < source.projection().revision_high_water().sequence()
        {
            return Err(ProjectStoreFault::Corruption {
                stage: "create_from_provisional",
            });
        }
        let project_id = projection.state().project_id();
        let installed = install_initial_manual_package_from_generation(
            &destination,
            InitialPackageMode::Create,
            capture,
            &self.root,
            source,
            limits,
            &mut is_cancelled,
        )?;
        let (next, _receipt, session) = Self::from_installed(
            destination,
            project_id,
            installed,
            NormalSessionKind::Established,
        );
        let ActorSession::Normal { resources, .. } = next else {
            unreachable!("an installed manual package is a normal session")
        };
        let old = std::mem::replace(self, resources);
        drop(old);
        Ok(session)
    }

    fn save_as<C>(
        &mut self,
        destination: ProjectStorePath,
        source_generation: ProjectGenerationId,
        capture: ProjectCommitCapture,
        limits: crate::ProjectStoreLimits,
        mut is_cancelled: C,
    ) -> Result<ProjectStoreReceipt, ProjectStoreFault>
    where
        C: FnMut() -> bool,
    {
        ensure_authenticated_copy_destination(&destination, &self.root, limits)?;
        match self.leases.confirm_writer(&self.root) {
            Ok(true) => {}
            Ok(false) => return Err(ProjectStoreFault::ReadOnly),
            Err(LeaseError::Indeterminate) => return Err(ProjectStoreFault::CommitIndeterminate),
            Err(LeaseError::InvalidAnchor | LeaseError::Io { .. }) => {
                return Err(ProjectStoreFault::Corruption {
                    stage: "actor_session_lease",
                });
            }
        }
        let inspection = inspect_established_store(&self.root, limits, &mut is_cancelled)?;
        if inspection.manual().head.current() != source_generation {
            return Err(ProjectStoreFault::StaleParent);
        }
        if !capture
            .projection()
            .state()
            .dataset()
            .has_same_scientific_content(
                inspection
                    .manual_generation()
                    .projection()
                    .state()
                    .dataset(),
            )
        {
            return Err(ProjectStoreFault::Corruption {
                stage: "save_as_scientific_identity",
            });
        }
        let source_project_id = inspection.project_id();
        let installed = install_initial_manual_package_from_generation(
            &destination,
            InitialPackageMode::SaveAs {
                source_project_id,
                source_generation_id: source_generation,
            },
            capture,
            &self.root,
            inspection.manual_generation(),
            limits,
            &mut is_cancelled,
        )?;
        let (root, leases, receipt) = installed.into_parts();
        let old = std::mem::replace(
            self,
            Self {
                path: destination,
                root,
                leases,
            },
        );
        drop(old);
        Ok(receipt)
    }

    fn save_as_selected_recovery<C>(
        &mut self,
        selected_generation: ProjectGenerationId,
        destination: ProjectStorePath,
        source_generation: ProjectGenerationId,
        capture: ProjectCommitCapture,
        limits: crate::ProjectStoreLimits,
        mut is_cancelled: C,
    ) -> Result<ProjectStoreReceipt, ProjectStoreFault>
    where
        C: FnMut() -> bool,
    {
        ensure_authenticated_copy_destination(&destination, &self.root, limits)?;
        if source_generation != selected_generation {
            return Err(ProjectStoreFault::StaleParent);
        }
        let opened = open_recovery(&self.root, selected_generation, limits, &mut is_cancelled)?;
        let (inspection, selected) = opened.into_document_parts();
        let source_project_id = inspection.project_id();
        if !capture
            .projection()
            .state()
            .dataset()
            .has_same_scientific_content(selected.projection().state().dataset())
        {
            return Err(ProjectStoreFault::Corruption {
                stage: "save_as_recovery_selection",
            });
        }
        let installed = install_initial_manual_package_from_generation(
            &destination,
            InitialPackageMode::SaveAs {
                source_project_id,
                source_generation_id: selected_generation,
            },
            capture,
            &self.root,
            &selected,
            limits,
            &mut is_cancelled,
        )?;
        let (root, leases, receipt) = installed.into_parts();
        let old = std::mem::replace(
            self,
            Self {
                path: destination,
                root,
                leases,
            },
        );
        drop(old);
        Ok(receipt)
    }

    fn inspect_recovery<C>(
        &self,
        limits: crate::ProjectStoreLimits,
        is_cancelled: C,
    ) -> Result<Vec<crate::ProjectRecoveryCandidate>, ProjectStoreFault>
    where
        C: FnMut() -> bool,
    {
        inspect_recovery(&self.root, limits, is_cancelled)
            .map(|inspection| inspection.candidates().to_vec())
    }

    fn open_recovery<C>(
        &self,
        generation_id: ProjectGenerationId,
        limits: crate::ProjectStoreLimits,
        is_cancelled: C,
    ) -> Result<
        (
            ProjectStoreSession,
            mirante4d_project_model::ProjectGenerationProjection,
        ),
        ProjectStoreFault,
    >
    where
        C: FnMut() -> bool,
    {
        let opened = open_recovery(&self.root, generation_id, limits, is_cancelled)?;
        let (inspection, projection) = opened.into_parts();
        Ok((self.session(&inspection), projection))
    }

    fn session(&self, inspection: &RecoveryInspection) -> ProjectStoreSession {
        ProjectStoreSession::new(
            self.path.clone(),
            inspection.project_id(),
            self.leases.effective_mode(),
            inspection.current_manual_generation(),
            inspection.current_autosave_generation(),
        )
    }

    fn session_from_state(&self, state: &StoreStateInspection) -> ProjectStoreSession {
        ProjectStoreSession::new(
            self.path.clone(),
            state.project_id(),
            self.leases.effective_mode(),
            state.manual().map(|lane| lane.head.current()),
            state.autosave().map(|lane| lane.head.current()),
        )
    }
}

impl EstablishedProjectActor {
    pub(crate) fn start_unbound(config: ProjectStoreConfig) -> Result<Self, ProjectStoreFault> {
        Self::start_inner(config, None)
    }

    fn start_inner(
        config: ProjectStoreConfig,
        initial_session: Option<ActorSession>,
    ) -> Result<Self, ProjectStoreFault> {
        let limits = config.limits().validate()?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                requests: VecDeque::new(),
                completions: VecDeque::new(),
                active: None,
                close_request: None,
                completion_reservations: 0,
                live_request_ids: BTreeSet::new(),
                last_accepted_request_id: None,
                accepting: true,
                shutdown: false,
                worker_exited: false,
            }),
            wake: Condvar::new(),
            request_limit: limits.actor_request_queue_max(),
            completion_limit: limits.actor_completion_queue_max(),
            trash_selection_limit: limits.recovery_candidates_max,
            artifact_selection_limit: limits.artifact_records_per_generation_max,
            autosave_enabled: config.autosave_enabled(),
            #[cfg(test)]
            gc_transition_injector: OnceLock::new(),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name(String::from("mirante4d-project-store"))
            .spawn(move || worker_main(worker_shared, initial_session, limits))
            .map_err(|_| ProjectStoreFault::Corruption {
                stage: "actor_spawn",
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    #[cfg(test)]
    fn start_bound(
        path: &ProjectStorePath,
        config: ProjectStoreConfig,
    ) -> Result<Self, ProjectStoreFault> {
        let limits = config.limits().validate()?;
        let outcome = open_resources(path, ProjectOpenMode::PreferWritable, limits, || false)?;
        let session = match outcome {
            OpenResources::Normal {
                resources, kind, ..
            } => ActorSession::Normal { resources, kind },
            OpenResources::RecoveryOnly { resources, .. } => {
                ActorSession::RecoveryOnly { resources }
            }
        };
        Self::start_inner(config, Some(session))
    }

    #[cfg(test)]
    fn start(
        path: &ProjectStorePath,
        config: ProjectStoreConfig,
    ) -> Result<Self, ProjectStoreFault> {
        Self::start_bound(path, config)
    }

    #[cfg(test)]
    fn start_with_gc_transition_injector(
        path: &ProjectStorePath,
        config: ProjectStoreConfig,
        injector: Arc<GcTransitionInjector>,
    ) -> Result<Self, ProjectStoreFault> {
        let actor = Self::start_bound(path, config)?;
        actor
            .shared
            .gc_transition_injector
            .set(injector)
            .expect("a test actor installs one GC transition injector");
        Ok(actor)
    }

    pub(crate) fn try_submit(&self, command: ProjectStoreCommand) -> Result<(), ProjectStoreFault> {
        match command {
            ProjectStoreCommand::Create {
                request_id,
                destination,
                capture,
            } => self.submit_work(Work::Create {
                request_id,
                destination,
                capture,
            }),
            ProjectStoreCommand::Open {
                request_id,
                path,
                mode,
            } => self.submit_work(Work::Open {
                request_id,
                path,
                mode,
            }),
            ProjectStoreCommand::ManualSave {
                request_id,
                capture,
            } => self.submit_work(Work::ManualSave {
                request_id,
                capture,
            }),
            ProjectStoreCommand::Autosave {
                request_id,
                destination,
                capture,
            } if self.shared.autosave_enabled => self.submit_work(Work::Autosave {
                request_id,
                destination,
                capture,
            }),
            ProjectStoreCommand::Autosave { .. } => Err(ProjectStoreFault::Corruption {
                stage: "autosave_disabled",
            }),
            ProjectStoreCommand::SaveAs {
                request_id,
                destination,
                source_generation,
                capture,
            } => self.submit_work(Work::SaveAs {
                request_id,
                destination,
                source_generation,
                capture,
            }),
            ProjectStoreCommand::InspectRecovery { request_id } => {
                self.submit_work(Work::InspectRecovery { request_id })
            }
            ProjectStoreCommand::OpenRecovery {
                request_id,
                generation_id,
            } => self.submit_work(Work::OpenRecovery {
                request_id,
                generation_id,
            }),
            ProjectStoreCommand::LoadArtifacts {
                request_id,
                generation_id,
                artifact_handles,
            } => {
                if artifact_handles.len() > self.shared.artifact_selection_limit {
                    return Err(ProjectStoreFault::Capacity {
                        stage: "artifact_load_selection",
                    });
                }
                if artifact_handles.is_empty()
                    || artifact_handles
                        .iter()
                        .cloned()
                        .collect::<BTreeSet<_>>()
                        .len()
                        != artifact_handles.len()
                {
                    return Err(ProjectStoreFault::Corruption {
                        stage: "artifact_load_selection",
                    });
                }
                self.submit_work(Work::LoadArtifacts {
                    request_id,
                    generation_id,
                    artifact_handles,
                })
            }
            ProjectStoreCommand::Pin {
                request_id,
                checkpoint_id,
                generation_id,
            } if valid_checkpoint_id(&checkpoint_id) => self.submit_work(Work::Pin {
                request_id,
                checkpoint_id,
                generation_id,
            }),
            ProjectStoreCommand::Pin { .. } => Err(ProjectStoreFault::Corruption {
                stage: "checkpoint_id",
            }),
            ProjectStoreCommand::Unpin {
                request_id,
                checkpoint_id,
            } if valid_checkpoint_id(&checkpoint_id) => self.submit_work(Work::Unpin {
                request_id,
                checkpoint_id,
            }),
            ProjectStoreCommand::Unpin { .. } => Err(ProjectStoreFault::Corruption {
                stage: "checkpoint_id",
            }),
            ProjectStoreCommand::PlanCompaction { request_id } => {
                self.submit_work(Work::PlanCompaction { request_id })
            }
            ProjectStoreCommand::Trash {
                request_id,
                generations,
            } => {
                if generations.len() > self.shared.trash_selection_limit {
                    return Err(ProjectStoreFault::Capacity {
                        stage: "trash_selection",
                    });
                }
                if generations.is_empty()
                    || generations.iter().copied().collect::<BTreeSet<_>>().len()
                        != generations.len()
                {
                    return Err(ProjectStoreFault::Corruption {
                        stage: "trash_selection",
                    });
                }
                self.submit_work(Work::Trash {
                    request_id,
                    generations,
                })
            }
            ProjectStoreCommand::Purge { request_id } => {
                self.submit_work(Work::Purge { request_id })
            }
            ProjectStoreCommand::FullVerify { request_id } => {
                self.submit_work(Work::FullVerify { request_id })
            }
            ProjectStoreCommand::Cancel {
                request_id,
                target_request_id,
            } => self.submit_cancel(request_id, target_request_id),
            ProjectStoreCommand::Close { request_id } => self.submit_close(request_id),
        }
    }

    pub(crate) fn try_recv(&self) -> Option<ProjectStoreCompletion> {
        let mut state = self.shared.lock();
        let completion = state.completions.pop_front()?;
        state.live_request_ids.remove(&completion.request_id());
        self.shared.wake.notify_all();
        Some(completion)
    }

    pub(crate) fn has_exited(&self) -> bool {
        self.shared.lock().worker_exited
    }

    #[cfg(test)]
    fn recv_timeout(&self, timeout: Duration) -> Option<ProjectStoreCompletion> {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.lock();
        loop {
            if let Some(completion) = state.completions.pop_front() {
                state.live_request_ids.remove(&completion.request_id());
                self.shared.wake.notify_all();
                return Some(completion);
            }
            let remaining = deadline.checked_duration_since(Instant::now())?;
            let (next, wait) = self
                .shared
                .wake
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
            if wait.timed_out() && state.completions.is_empty() {
                return None;
            }
        }
    }

    /// Joins the worker. If Close has not already completed, this first starts
    /// an internal cancellation shutdown which emits no further obligations.
    pub(crate) fn join(mut self) -> thread::Result<()> {
        self.request_shutdown();
        self.worker.take().expect("worker exists until join").join()
    }

    fn submit_work(&self, work: Work) -> Result<(), ProjectStoreFault> {
        let mut state = self.shared.lock();
        state.validate_new_request(work.request_id())?;
        let queued_autosave = work
            .is_autosave()
            .then(|| state.requests.iter().position(Work::is_autosave))
            .flatten();
        if state.requests.len() >= self.shared.request_limit && queued_autosave.is_none() {
            return Err(ProjectStoreFault::QueueFull { queue: "request" });
        }

        if let Some(index) = queued_autosave {
            state.require_completion_capacity(self.shared.completion_limit, 1)?;
            let superseded = state
                .requests
                .remove(index)
                .expect("autosave index is valid");
            state.push_unreserved(
                superseded.cancelled_completion(),
                self.shared.completion_limit,
            );
        }

        state.accept(work.request_id());
        state.requests.push_back(work);
        self.shared.wake.notify_all();
        Ok(())
    }

    fn submit_cancel(
        &self,
        request_id: ProjectStoreRequestId,
        target_request_id: ProjectStoreRequestId,
    ) -> Result<(), ProjectStoreFault> {
        let mut state = self.shared.lock();
        state.validate_new_request(request_id)?;

        let queued = state
            .requests
            .iter()
            .position(|work| work.request_id() == target_request_id);
        if queued.is_none()
            && state
                .active
                .as_ref()
                .is_none_or(|active| active.request_id != target_request_id)
        {
            return Err(ProjectStoreFault::Corruption {
                stage: "cancel_target",
            });
        }
        state.require_completion_capacity(
            self.shared.completion_limit,
            if queued.is_some() { 2 } else { 1 },
        )?;

        state.accept(request_id);
        if let Some(index) = queued {
            let cancelled = state.requests.remove(index).expect("queued target exists");
            state.push_unreserved(
                cancelled.cancelled_completion(),
                self.shared.completion_limit,
            );
        } else if let Some(active) = &state.active {
            active.cancelled.store(true, Ordering::Release);
        }
        state.push_unreserved(
            ProjectStoreCompletion::Cancelled {
                request_id,
                result: Ok(()),
            },
            self.shared.completion_limit,
        );
        self.shared.wake.notify_all();
        Ok(())
    }

    fn submit_close(&self, request_id: ProjectStoreRequestId) -> Result<(), ProjectStoreFault> {
        let mut state = self.shared.lock();
        state.validate_new_request(request_id)?;
        state.require_completion_capacity(
            self.shared.completion_limit,
            state.requests.len().saturating_add(1),
        )?;
        state.accept(request_id);
        state.completion_reservations += 1;
        state.accepting = false;
        state.close_request = Some(request_id);
        if let Some(active) = &state.active {
            active.cancelled.store(true, Ordering::Release);
        }
        while let Some(work) = state.requests.pop_front() {
            state.push_unreserved(work.cancelled_completion(), self.shared.completion_limit);
        }
        self.shared.wake.notify_all();
        Ok(())
    }

    fn request_shutdown(&self) {
        let mut state = self.shared.lock();
        if state.worker_exited {
            return;
        }
        state.accepting = false;
        state.shutdown = true;
        if let Some(active) = &state.active {
            active.cancelled.store(true, Ordering::Release);
        }
        state.requests.clear();
        state.completions.clear();
        state.close_request = None;
        state.live_request_ids.clear();
        state.completion_reservations = usize::from(state.active.is_some());
        self.shared.wake.notify_all();
    }
}

impl Drop for EstablishedProjectActor {
    fn drop(&mut self) {
        if self.worker.is_some() {
            self.request_shutdown();
            let _detached = self.worker.take();
        }
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl State {
    fn validate_new_request(
        &self,
        request_id: ProjectStoreRequestId,
    ) -> Result<(), ProjectStoreFault> {
        if !self.accepting {
            return Err(ProjectStoreFault::Corruption {
                stage: "actor_closed",
            });
        }
        if self
            .last_accepted_request_id
            .is_some_and(|last| request_id <= last)
            || self.live_request_ids.contains(&request_id)
        {
            return Err(ProjectStoreFault::Corruption {
                stage: "request_id",
            });
        }
        Ok(())
    }

    fn require_completion_capacity(
        &self,
        limit: usize,
        additional: usize,
    ) -> Result<(), ProjectStoreFault> {
        if self
            .completions
            .len()
            .saturating_add(self.completion_reservations)
            .saturating_add(additional)
            > limit
        {
            Err(ProjectStoreFault::QueueFull {
                queue: "completion",
            })
        } else {
            Ok(())
        }
    }

    fn accept(&mut self, request_id: ProjectStoreRequestId) {
        self.last_accepted_request_id = Some(request_id);
        let inserted = self.live_request_ids.insert(request_id);
        debug_assert!(inserted);
    }

    fn finish_reserved(&mut self, completion: ProjectStoreCompletion) {
        self.completion_reservations = self
            .completion_reservations
            .checked_sub(1)
            .expect("every completion consumes one reservation");
        self.completions.push_back(completion);
    }

    fn push_unreserved(&mut self, completion: ProjectStoreCompletion, limit: usize) {
        debug_assert!(self.completions.len() + self.completion_reservations < limit);
        self.completions.push_back(completion);
    }

    fn can_reserve_completion(&self, limit: usize) -> bool {
        self.completions.len() + self.completion_reservations < limit
    }
}

fn actor_diagnostics(
    shared: &Shared,
    leases: &ProjectStoreLeases,
    mut diagnostics: ProjectStoreDiagnostics,
) -> ProjectStoreDiagnostics {
    let state = shared.lock();
    diagnostics.queued_requests = state.requests.len();
    diagnostics.queued_completions = state.completions.len();
    diagnostics.active_transactions = usize::from(state.active.is_some());
    diagnostics.open_file_descriptors = 2 + usize::from(leases.has_writer());
    diagnostics
}

fn lifecycle_fault(stage: &'static str) -> ProjectStoreFault {
    ProjectStoreFault::Corruption { stage }
}

fn normal_session_mut(
    session: &mut Option<ActorSession>,
) -> Result<(&mut SessionResources, NormalSessionKind), ProjectStoreFault> {
    match session {
        Some(ActorSession::Normal { resources, kind }) => Ok((resources, *kind)),
        Some(ActorSession::RecoveryOnly { .. }) => Err(lifecycle_fault("actor_recovery_only")),
        Some(ActorSession::RecoverySelected { .. }) => {
            Err(lifecycle_fault("actor_recovery_selected"))
        }
        None => Err(lifecycle_fault("actor_unbound")),
    }
}

fn any_session_mut(
    session: &mut Option<ActorSession>,
) -> Result<&mut SessionResources, ProjectStoreFault> {
    session
        .as_mut()
        .map(ActorSession::resources_mut)
        .ok_or_else(|| lifecycle_fault("actor_unbound"))
}

fn execute_load_artifacts<C>(
    session: Option<&ActorSession>,
    generation_id: ProjectGenerationId,
    artifact_handles: &[ArtifactHandleId],
    limits: crate::ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<Vec<LoadedProjectArtifact>, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    match session.ok_or_else(|| lifecycle_fault("actor_unbound"))? {
        ActorSession::Normal { resources, kind } => {
            let state = inspect_store_state(&resources.root, limits, &mut is_cancelled)?;
            let document = match kind {
                NormalSessionKind::Established
                    if state.manual().map(|lane| lane.head.current()) == Some(generation_id) =>
                {
                    state.manual_generation()
                }
                NormalSessionKind::Established
                    if state.manual().is_some()
                        && state.autosave().map(|lane| lane.head.current())
                            == Some(generation_id) =>
                {
                    state.autosave_generation()
                }
                NormalSessionKind::Provisional
                    if state.is_provisional()
                        && state.autosave().map(|lane| lane.head.current())
                            == Some(generation_id) =>
                {
                    state.autosave_generation()
                }
                _ => None,
            };
            let document = document.ok_or(ProjectStoreFault::Corruption {
                stage: "artifact_load_generation",
            })?;
            load_artifact_bundle(
                &resources.root,
                document,
                artifact_handles,
                limits,
                &mut is_cancelled,
            )
        }
        ActorSession::RecoverySelected {
            resources,
            selected_generation,
        } => {
            if *selected_generation != generation_id {
                return Err(ProjectStoreFault::Corruption {
                    stage: "artifact_load_generation",
                });
            }
            let opened = open_recovery(
                &resources.root,
                *selected_generation,
                limits,
                &mut is_cancelled,
            )?;
            let (_, document) = opened.into_document_parts();
            load_artifact_bundle(
                &resources.root,
                &document,
                artifact_handles,
                limits,
                &mut is_cancelled,
            )
        }
        ActorSession::RecoveryOnly { .. } => Err(lifecycle_fault("actor_recovery_only")),
    }
}

fn load_artifact_bundle<C>(
    root: &LocalStoreRoot,
    document: &GenerationDocument,
    artifact_handles: &[ArtifactHandleId],
    limits: crate::ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<Vec<LoadedProjectArtifact>, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let artifacts = document.projection().state().artifacts();
    let mut selected = Vec::<ArtifactReference>::with_capacity(artifact_handles.len());
    let mut logical_bytes = 0_u64;
    for handle in artifact_handles {
        let artifact = artifacts
            .iter()
            .find(|artifact| artifact.handle_id() == handle)
            .ok_or(ProjectStoreFault::Corruption {
                stage: "artifact_load_selection",
            })?;
        logical_bytes = logical_bytes
            .checked_add(artifact.object().byte_length())
            .ok_or(ProjectStoreFault::Capacity {
                stage: "artifact_load_bytes",
            })?;
        if logical_bytes > limits.object_or_page_bytes_max() {
            return Err(ProjectStoreFault::Capacity {
                stage: "artifact_load_bytes",
            });
        }
        selected.push(artifact.clone());
    }
    let _bundle_capacity =
        usize::try_from(logical_bytes).map_err(|_| ProjectStoreFault::Capacity {
            stage: "artifact_load_bytes",
        })?;

    let mut loaded = Vec::with_capacity(selected.len());
    for artifact in selected {
        let bytes = read_artifact_bytes(root, document, &artifact, limits, &mut is_cancelled)?;
        loaded.push(LoadedProjectArtifact::new(artifact, bytes));
    }
    Ok(loaded)
}

fn read_artifact_bytes<C>(
    root: &LocalStoreRoot,
    document: &GenerationDocument,
    artifact: &ArtifactReference,
    limits: crate::ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<Vec<u8>, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let descriptor = artifact.object();
    let storage =
        document
            .bindings()
            .get(&descriptor.digest())
            .ok_or(ProjectStoreFault::Corruption {
                stage: "artifact_load_binding",
            })?;
    let capacity =
        usize::try_from(descriptor.byte_length()).map_err(|_| ProjectStoreFault::Capacity {
            stage: "artifact_load_bytes",
        })?;
    match storage {
        ArtifactStorage::Direct { object } => {
            if object.digest() != descriptor.digest()
                || object.byte_length() != descriptor.byte_length()
            {
                return Err(ProjectStoreFault::Corruption {
                    stage: "artifact_load_binding",
                });
            }
            let mut bytes = Vec::with_capacity(capacity);
            root.read_exact_object(
                descriptor.digest(),
                descriptor.byte_length(),
                limits.object_or_page_bytes_max(),
                &mut is_cancelled,
                |chunk| bytes.extend_from_slice(chunk),
            )
            .map_err(|error| map_local_error(error, "artifact_load_object"))?;
            Ok(bytes)
        }
        ArtifactStorage::Paged { binding_manifest } => {
            let binding_capacity =
                usize::try_from(binding_manifest.byte_length()).map_err(|_| {
                    ProjectStoreFault::Capacity {
                        stage: "artifact_load_binding",
                    }
                })?;
            let mut binding_bytes = Vec::with_capacity(binding_capacity);
            root.read_exact_object(
                binding_manifest.digest(),
                binding_manifest.byte_length(),
                limits.object_or_page_bytes_max(),
                &mut is_cancelled,
                |chunk| binding_bytes.extend_from_slice(chunk),
            )
            .map_err(|error| map_local_error(error, "artifact_load_binding"))?;
            let binding =
                LogicalObjectBinding::decode(&binding_bytes, descriptor, binding_manifest, limits)
                    .map_err(|_| ProjectStoreFault::Corruption {
                        stage: "artifact_load_binding",
                    })?;
            let mut bytes = Vec::with_capacity(capacity);
            let mut logical_hasher = ExactBytesHasher::new();
            let mut hash_failed = false;
            for page in binding.pages() {
                let page = page.object();
                root.read_exact_object(
                    page.digest(),
                    page.byte_length(),
                    limits.object_or_page_bytes_max(),
                    &mut is_cancelled,
                    |chunk| {
                        bytes.extend_from_slice(chunk);
                        hash_failed |= logical_hasher.update(chunk).is_err();
                    },
                )
                .map_err(|error| map_local_error(error, "artifact_load_object"))?;
            }
            if hash_failed {
                return Err(ProjectStoreFault::Capacity {
                    stage: "artifact_load_bytes",
                });
            }
            let facts = logical_hasher
                .finalize()
                .map_err(|_| ProjectStoreFault::Capacity {
                    stage: "artifact_load_bytes",
                })?;
            if facts.digest() != descriptor.digest()
                || facts.byte_length() != descriptor.byte_length()
            {
                return Err(ProjectStoreFault::DigestMismatch);
            }
            Ok(bytes)
        }
    }
}

fn execute_create<C>(
    session: &mut Option<ActorSession>,
    destination: ProjectStorePath,
    capture: ProjectCommitCapture,
    limits: crate::ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<ProjectStoreSession, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    match session.take() {
        None => {
            let project_id = capture.projection().state().project_id();
            let installed = install_initial_manual_package(
                &destination,
                InitialPackageMode::Create,
                capture,
                limits,
                &mut is_cancelled,
            )?;
            let (next, _receipt, opened) = SessionResources::from_installed(
                destination,
                project_id,
                installed,
                NormalSessionKind::Established,
            );
            *session = Some(next);
            Ok(opened)
        }
        Some(ActorSession::Normal {
            mut resources,
            kind: NormalSessionKind::Provisional,
        }) => {
            let result =
                resources.create_from_provisional(destination, capture, limits, &mut is_cancelled);
            let kind = if result.is_ok() {
                NormalSessionKind::Established
            } else {
                NormalSessionKind::Provisional
            };
            *session = Some(ActorSession::Normal { resources, kind });
            result
        }
        Some(other) => {
            *session = Some(other);
            Err(lifecycle_fault("create_session_bound"))
        }
    }
}

fn execute_open<C>(
    session: &mut Option<ActorSession>,
    path: ProjectStorePath,
    mode: ProjectOpenMode,
    limits: crate::ProjectStoreLimits,
    is_cancelled: C,
) -> Result<
    (
        ProjectStoreSession,
        mirante4d_project_model::ProjectGenerationProjection,
    ),
    ProjectStoreFault,
>
where
    C: FnMut() -> bool,
{
    if session.is_some() {
        return Err(lifecycle_fault("open_session_bound"));
    }
    match open_resources(&path, mode, limits, is_cancelled)? {
        OpenResources::Normal {
            resources,
            kind,
            opened,
            projection,
        } => {
            *session = Some(ActorSession::Normal { resources, kind });
            Ok((opened, *projection))
        }
        OpenResources::RecoveryOnly {
            resources,
            normal_fault,
        } => {
            *session = Some(ActorSession::RecoveryOnly { resources });
            Err(normal_fault)
        }
    }
}

fn execute_open_recovery<C>(
    session: &mut Option<ActorSession>,
    generation_id: ProjectGenerationId,
    limits: crate::ProjectStoreLimits,
    is_cancelled: C,
) -> Result<
    (
        ProjectStoreSession,
        mirante4d_project_model::ProjectGenerationProjection,
    ),
    ProjectStoreFault,
>
where
    C: FnMut() -> bool,
{
    let result = any_session_mut(session)?.open_recovery(generation_id, limits, is_cancelled);
    if result.is_ok() {
        let resources = session
            .take()
            .expect("a successful recovery open retains one actor session")
            .into_resources();
        *session = Some(ActorSession::RecoverySelected {
            resources,
            selected_generation: generation_id,
        });
    }
    result
}

fn execute_autosave<C>(
    session: &mut Option<ActorSession>,
    destination: Option<ProjectStorePath>,
    capture: ProjectCommitCapture,
    limits: crate::ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<ProjectStoreReceipt, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    match (session.take(), destination) {
        (None, Some(destination)) => {
            let project_id = capture.projection().state().project_id();
            let installed = install_initial_provisional_autosave_package(
                &destination,
                capture,
                limits,
                &mut is_cancelled,
            )?;
            let (next, receipt, _opened) = SessionResources::from_installed(
                destination,
                project_id,
                installed,
                NormalSessionKind::Provisional,
            );
            *session = Some(next);
            Ok(receipt)
        }
        (None, None) => Err(lifecycle_fault("autosave_destination")),
        (Some(ActorSession::Normal { resources, kind }), None) => {
            let result = match kind {
                NormalSessionKind::Established => publish_established_autosave_generation(
                    &resources.root,
                    &resources.leases,
                    capture,
                    limits,
                    &mut is_cancelled,
                ),
                NormalSessionKind::Provisional => publish_provisional_autosave_generation(
                    &resources.root,
                    &resources.leases,
                    capture,
                    limits,
                    &mut is_cancelled,
                ),
            };
            *session = Some(ActorSession::Normal { resources, kind });
            result
        }
        (
            Some(ActorSession::RecoverySelected {
                resources,
                selected_generation,
            }),
            _,
        ) => {
            *session = Some(ActorSession::RecoverySelected {
                resources,
                selected_generation,
            });
            Err(lifecycle_fault("actor_recovery_selected"))
        }
        (Some(other), _) => {
            *session = Some(other);
            Err(lifecycle_fault("autosave_destination"))
        }
    }
}

fn execute_save_as<C>(
    session: &mut Option<ActorSession>,
    destination: ProjectStorePath,
    source_generation: ProjectGenerationId,
    capture: ProjectCommitCapture,
    limits: crate::ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<ProjectStoreReceipt, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    match session.take() {
        Some(ActorSession::Normal {
            mut resources,
            kind: NormalSessionKind::Established,
        }) => {
            let result = resources.save_as(
                destination,
                source_generation,
                capture,
                limits,
                &mut is_cancelled,
            );
            *session = Some(ActorSession::Normal {
                resources,
                kind: NormalSessionKind::Established,
            });
            result
        }
        Some(ActorSession::RecoverySelected {
            mut resources,
            selected_generation,
        }) => {
            let result = resources.save_as_selected_recovery(
                selected_generation,
                destination,
                source_generation,
                capture,
                limits,
                &mut is_cancelled,
            );
            let next = if result.is_ok() {
                ActorSession::Normal {
                    resources,
                    kind: NormalSessionKind::Established,
                }
            } else {
                ActorSession::RecoverySelected {
                    resources,
                    selected_generation,
                }
            };
            *session = Some(next);
            result
        }
        Some(other) => {
            *session = Some(other);
            Err(lifecycle_fault("save_as_session_kind"))
        }
        None => Err(lifecycle_fault("actor_unbound")),
    }
}

fn worker_main(
    shared: Arc<Shared>,
    initial_session: Option<ActorSession>,
    limits: crate::ProjectStoreLimits,
) {
    let mut session = initial_session;
    loop {
        let (work, cancelled) = {
            let mut state = shared.lock();
            while !state.shutdown
                && state.close_request.is_none()
                && (state.requests.is_empty()
                    || !state.can_reserve_completion(shared.completion_limit))
            {
                state = shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            if state.shutdown {
                state.active = None;
                state.completion_reservations = 0;
                break;
            }
            if let Some(work) = state.requests.pop_front() {
                state.completion_reservations += 1;
                let cancelled = Arc::new(AtomicBool::new(false));
                state.active = Some(Active {
                    request_id: work.request_id(),
                    cancelled: Arc::clone(&cancelled),
                });
                shared.wake.notify_all();
                (Some(work), cancelled)
            } else {
                (None, Arc::new(AtomicBool::new(false)))
            }
        };

        let Some(work) = work else {
            let mut state = shared.lock();
            if state.shutdown {
                state.completion_reservations = 0;
                break;
            }
            let request_id = state
                .close_request
                .take()
                .expect("an idle worker wakes only for close or shutdown");
            drop(session.take());
            state.finish_reserved(ProjectStoreCompletion::Closed {
                request_id,
                result: Ok(()),
            });
            state.worker_exited = true;
            shared.wake.notify_all();
            return;
        };

        let completion = match work {
            Work::Create {
                request_id,
                destination,
                capture,
            } => ProjectStoreCompletion::Created {
                request_id,
                result: execute_create(&mut session, destination, capture, limits, || {
                    cancelled.load(Ordering::Acquire)
                }),
            },
            Work::Open {
                request_id,
                path,
                mode,
            } => ProjectStoreCompletion::Opened {
                request_id,
                result: execute_open(&mut session, path, mode, limits, || {
                    cancelled.load(Ordering::Acquire)
                }),
            },
            Work::ManualSave {
                request_id,
                capture,
            } => ProjectStoreCompletion::ManualSaved {
                request_id,
                result: normal_session_mut(&mut session).and_then(|(resources, kind)| {
                    if kind != NormalSessionKind::Established {
                        return Err(lifecycle_fault("manual_save_requires_create"));
                    }
                    publish_established_manual_generation(
                        &resources.root,
                        &resources.leases,
                        capture,
                        limits,
                        || cancelled.load(Ordering::Acquire),
                    )
                }),
            },
            Work::Autosave {
                request_id,
                destination,
                capture,
            } => ProjectStoreCompletion::Autosaved {
                request_id,
                result: execute_autosave(&mut session, destination, capture, limits, || {
                    cancelled.load(Ordering::Acquire)
                }),
            },
            Work::SaveAs {
                request_id,
                destination,
                source_generation,
                capture,
            } => ProjectStoreCompletion::SavedAs {
                request_id,
                result: execute_save_as(
                    &mut session,
                    destination,
                    source_generation,
                    capture,
                    limits,
                    || cancelled.load(Ordering::Acquire),
                ),
            },
            Work::InspectRecovery { request_id } => ProjectStoreCompletion::RecoveryInspected {
                request_id,
                result: any_session_mut(&mut session).and_then(|resources| {
                    resources.inspect_recovery(limits, || cancelled.load(Ordering::Acquire))
                }),
            },
            Work::OpenRecovery {
                request_id,
                generation_id,
            } => ProjectStoreCompletion::RecoveryOpened {
                request_id,
                result: execute_open_recovery(&mut session, generation_id, limits, || {
                    cancelled.load(Ordering::Acquire)
                }),
            },
            Work::LoadArtifacts {
                request_id,
                generation_id,
                artifact_handles,
            } => ProjectStoreCompletion::ArtifactsLoaded {
                request_id,
                result: execute_load_artifacts(
                    session.as_ref(),
                    generation_id,
                    &artifact_handles,
                    limits,
                    || cancelled.load(Ordering::Acquire),
                ),
            },
            Work::Pin {
                request_id,
                checkpoint_id,
                generation_id,
            } => {
                let result = normal_session_mut(&mut session).and_then(|(resources, _)| {
                    #[cfg(test)]
                    if let Some(injector) = shared.gc_transition_injector.get() {
                        resources
                            .leases
                            .set_gc_transition_injector(Arc::clone(injector));
                    }
                    publish_pin(
                        &resources.root,
                        &resources.leases,
                        &checkpoint_id,
                        generation_id,
                        limits,
                        || cancelled.load(Ordering::Acquire),
                    )
                });
                ProjectStoreCompletion::Pinned { request_id, result }
            }
            Work::Unpin {
                request_id,
                checkpoint_id,
            } => {
                let result = normal_session_mut(&mut session).and_then(|(resources, _)| {
                    #[cfg(test)]
                    if let Some(injector) = shared.gc_transition_injector.get() {
                        resources
                            .leases
                            .set_gc_transition_injector(Arc::clone(injector));
                    }
                    remove_pin(
                        &resources.root,
                        &resources.leases,
                        &checkpoint_id,
                        limits,
                        || cancelled.load(Ordering::Acquire),
                    )
                });
                ProjectStoreCompletion::Unpinned { request_id, result }
            }
            Work::PlanCompaction { request_id } => ProjectStoreCompletion::CompactionPlanned {
                request_id,
                result: normal_session_mut(&mut session).and_then(|(resources, _)| {
                    crate::inspection::plan_compaction(&resources.root, limits, || {
                        cancelled.load(Ordering::Acquire)
                    })
                }),
            },
            Work::Trash {
                request_id,
                generations,
            } => {
                let result = normal_session_mut(&mut session).and_then(|(resources, _)| {
                    #[cfg(test)]
                    if let Some(injector) = shared.gc_transition_injector.get() {
                        resources
                            .leases
                            .set_gc_transition_injector(Arc::clone(injector));
                    }
                    trash_generations(
                        &resources.root,
                        &mut resources.leases,
                        &generations,
                        limits,
                        || cancelled.load(Ordering::Acquire),
                    )
                    .map(|diagnostics| actor_diagnostics(&shared, &resources.leases, diagnostics))
                });
                ProjectStoreCompletion::Trashed { request_id, result }
            }
            Work::Purge { request_id } => {
                let result = normal_session_mut(&mut session).and_then(|(resources, _)| {
                    #[cfg(test)]
                    if let Some(injector) = shared.gc_transition_injector.get() {
                        resources
                            .leases
                            .set_gc_transition_injector(Arc::clone(injector));
                    }
                    purge_trash(&resources.root, &mut resources.leases, limits, || {
                        cancelled.load(Ordering::Acquire)
                    })
                    .map(|diagnostics| actor_diagnostics(&shared, &resources.leases, diagnostics))
                });
                ProjectStoreCompletion::Purged { request_id, result }
            }
            Work::FullVerify { request_id } => {
                let result = any_session_mut(&mut session).and_then(|resources| {
                    crate::full_verify::full_verify(&resources.root, limits, || {
                        cancelled.load(Ordering::Acquire)
                    })
                    .map(|diagnostics| actor_diagnostics(&shared, &resources.leases, diagnostics))
                });
                ProjectStoreCompletion::Verified { request_id, result }
            }
        };
        let maintenance_lost = session
            .as_ref()
            .is_some_and(|session| session.resources().leases.maintenance_lost());
        if maintenance_lost {
            let mut state = shared.lock();
            state.active = None;
            if state.shutdown {
                state.completion_reservations = 0;
                drop(session.take());
                state.worker_exited = true;
                shared.wake.notify_all();
                return;
            }
            state.accepting = false;
            drop(session.take());
            state.finish_reserved(completion);
            while !state.requests.is_empty() {
                while !state.can_reserve_completion(shared.completion_limit) {
                    state = shared
                        .wake
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                let cancelled = state
                    .requests
                    .pop_front()
                    .expect("a terminal session drains one queued request");
                state.push_unreserved(cancelled.cancelled_completion(), shared.completion_limit);
                shared.wake.notify_all();
            }
            if let Some(request_id) = state.close_request.take() {
                state.finish_reserved(ProjectStoreCompletion::Closed {
                    request_id,
                    result: Ok(()),
                });
            }
            state.worker_exited = true;
            shared.wake.notify_all();
            return;
        }

        let mut state = shared.lock();
        state.active = None;
        if state.shutdown {
            state.completion_reservations = 0;
            break;
        }
        state.finish_reserved(completion);
        shared.wake.notify_all();
    }

    drop(session.take());
    let mut state = shared.lock();
    state.worker_exited = true;
    shared.wake.notify_all();
}

enum OpenResources {
    Normal {
        resources: SessionResources,
        kind: NormalSessionKind,
        opened: ProjectStoreSession,
        projection: Box<mirante4d_project_model::ProjectGenerationProjection>,
    },
    RecoveryOnly {
        resources: SessionResources,
        normal_fault: ProjectStoreFault,
    },
}

fn open_resources<C>(
    path: &ProjectStorePath,
    mode: ProjectOpenMode,
    limits: crate::ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<OpenResources, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let root = LocalStoreRoot::open(path.as_path()).map_err(|_| ProjectStoreFault::Corruption {
        stage: "actor_open",
    })?;
    let leases = ProjectStoreLeases::acquire(&root, mode).map_err(|error| match error {
        LeaseError::Indeterminate => ProjectStoreFault::CommitIndeterminate,
        LeaseError::InvalidAnchor | LeaseError::Io { .. } => ProjectStoreFault::Corruption {
            stage: "actor_open_lease",
        },
    })?;
    inspect_recovery(&root, limits, &mut is_cancelled)?;
    if leases.has_writer() {
        cleanup_dead_writer_staging(&root, &leases, limits, &mut is_cancelled)?;
    }
    let resources = SessionResources {
        path: path.clone(),
        root,
        leases,
    };
    match inspect_store_state(&resources.root, limits, &mut is_cancelled) {
        Ok(state) => {
            let kind = if state.is_provisional() {
                NormalSessionKind::Provisional
            } else {
                NormalSessionKind::Established
            };
            let opened = resources.session_from_state(&state);
            let projection = Box::new(state.authority_generation().projection().clone());
            Ok(OpenResources::Normal {
                resources,
                kind,
                opened,
                projection,
            })
        }
        Err(ProjectStoreFault::Cancelled) => Err(ProjectStoreFault::Cancelled),
        Err(normal_fault) => Ok(OpenResources::RecoveryOnly {
            resources,
            normal_fault,
        }),
    }
}

#[cfg(test)]
mod tests {
    #[path = "durability_tests.rs"]
    mod durability_tests;
    #[path = "hosted_durability_tests.rs"]
    mod hosted_durability_tests;

    use std::{
        collections::BTreeMap,
        env, fs,
        io::{self, Cursor, Read},
        os::unix::process::ExitStatusExt,
        path::{Path, PathBuf},
        process::{Child, Command, ExitStatus, Stdio},
        sync::{
            Arc, Condvar, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use mirante4d_domain::LogicalLayerKey;
    use mirante4d_identity::{
        ArtifactContentId, ExactBytesDigest, ExactBytesHasher, MediaType, ObjectRole,
        RawObjectDescriptor, ScientificContentId,
    };
    use mirante4d_project_model::{
        ArtifactCompleteness, ArtifactHandleId, ArtifactRecoverability, ArtifactReference,
        ArtifactSchema, DatasetReference, ProjectGenerationProjection, ProjectId,
        ProjectRevisionHighWater, ProjectRevisionId, ProjectState,
    };

    use super::*;
    use crate::{
        ProjectGenerationId, ProjectObjectSource, ProjectStoreActor, ProjectStoreLimits,
        filesystem::TEST_REAL_POLICY_ENV,
        generation::{ArtifactStorage, GenerationDocument, LogicalObjectBinding},
        lease::{GcTransition, GcTransitionTarget, TransitionEdge},
        wire::ProjectEnvelope,
    };

    const TIMEOUT: Duration = Duration::from_secs(5);
    const HOSTED_ACTOR_TIMEOUT_ENV: &str = "MIRANTE4D_PROJECT_STORE_HOSTED_ACTOR_TIMEOUT_MS";
    const TRASH_PROCESS_ROLE: &str = "MIRANTE4D_TRASH_PROCESS_ROLE";
    const TRASH_PROCESS_ROOT: &str = "MIRANTE4D_TRASH_PROCESS_ROOT";
    const TRASH_PROCESS_GENERATION: &str = "MIRANTE4D_TRASH_PROCESS_GENERATION";
    const TRASH_PROCESS_TRANSITION: &str = "MIRANTE4D_TRASH_PROCESS_TRANSITION";
    const TRASH_PROCESS_EDGE: &str = "MIRANTE4D_TRASH_PROCESS_EDGE";
    const TRASH_PROCESS_OCCURRENCE: &str = "MIRANTE4D_TRASH_PROCESS_OCCURRENCE";
    const TRASH_PROCESS_MARKER: &str = "MIRANTE4D_TRASH_PROCESS_MARKER";
    const TRASH_PROCESS_TEST: &str = concat!(
        "actor::tests::",
        "trash_fresh_process_kill_and_retry_matrix"
    );
    const PURGE_PROCESS_ROLE: &str = "MIRANTE4D_PURGE_PROCESS_ROLE";
    const PURGE_PROCESS_ROOT: &str = "MIRANTE4D_PURGE_PROCESS_ROOT";
    const PURGE_PROCESS_TRANSITION: &str = "MIRANTE4D_PURGE_PROCESS_TRANSITION";
    const PURGE_PROCESS_EDGE: &str = "MIRANTE4D_PURGE_PROCESS_EDGE";
    const PURGE_PROCESS_OCCURRENCE: &str = "MIRANTE4D_PURGE_PROCESS_OCCURRENCE";
    const PURGE_PROCESS_MARKER: &str = "MIRANTE4D_PURGE_PROCESS_MARKER";
    const PURGE_PROCESS_TEST: &str = concat!(
        "actor::tests::",
        "purge_fresh_process_kill_and_retry_matrix"
    );
    const PIN_UNPIN_PROCESS_ROLE: &str = "MIRANTE4D_PIN_UNPIN_PROCESS_ROLE";
    const PIN_UNPIN_PROCESS_ROOT: &str = "MIRANTE4D_PIN_UNPIN_PROCESS_ROOT";
    const PIN_UNPIN_PROCESS_OPERATION: &str = "MIRANTE4D_PIN_UNPIN_PROCESS_OPERATION";
    const PIN_UNPIN_PROCESS_TRANSITION: &str = "MIRANTE4D_PIN_UNPIN_PROCESS_TRANSITION";
    const PIN_UNPIN_PROCESS_EDGE: &str = "MIRANTE4D_PIN_UNPIN_PROCESS_EDGE";
    const PIN_UNPIN_PROCESS_OCCURRENCE: &str = "MIRANTE4D_PIN_UNPIN_PROCESS_OCCURRENCE";
    const PIN_UNPIN_PROCESS_MARKER: &str = "MIRANTE4D_PIN_UNPIN_PROCESS_MARKER";
    const PIN_UNPIN_PROCESS_TEST: &str = concat!(
        "actor::tests::",
        "pin_and_unpin_fresh_process_kill_and_retry_matrix"
    );
    const UNQUALIFIED_COMMAND_DESTINATIONS_TEST: &str = concat!(
        "actor::tests::",
        "create_and_save_as_report_unqualified_destinations_before_source_reads"
    );
    const STALE_MANUAL: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "d5020fa3c69a493b34ffbbf3a67a249354e83e5a6d738479d46c7e301786d2ec"
    );
    const STALE_AUTOSAVE: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "c357ffd5f7c051bf22877ffcd6680bdcd0f7db4068af93587e4a1f5bed0542a0"
    );
    const PROVISIONAL_AUTOSAVE: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "a1a84e1b98686c1d9eda416177988e691695baed74244ff5b99136e839ab0cea"
    );
    const RECOVERABLE_G1: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "9cf3985edc9a7de3702029a4b32fd3e4188796ee8459deddd0c6cd7babf57d81"
    );
    const RECOVERABLE_G2: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "50fc92ea0e67a54336658f1638596642f17177ceb72c3afbc364c941e6a9b854"
    );
    const RECOVERABLE_AUTOSAVE: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "d9504b896fd6a3fb21e52d227fcd284df654d4f063ea8ee0ca49fce0155e9b73"
    );
    const RECOVERABLE_ORPHAN: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "cfd67414728bb345edb7d5eabffac2530f04ed3b768d720782efe88e2d7ca370"
    );
    const DIVERGENT_INITIAL: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "10011b8d7dce93c428e1d117b485746522b4ae1d4d8ee89e359739f2cffd3a10"
    );
    const DIVERGENT_NEXT: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "10447a78680ee73dcc5572d71d81f1ad99079fb1374979a8a7937453a149ae1c"
    );
    static TEST_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    struct TestProject(PathBuf);

    impl TestProject {
        fn extracted(label: &str) -> Self {
            Self::extracted_store(label, "stale.m4dproj")
        }

        fn extracted_store(label: &str, store: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "mirante4d-actor-{label}-{}-{nonce}-{}.m4dproj",
                std::process::id(),
                TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            let output = Command::new("tar")
                .arg("-xzf")
                .arg(fixture_archive())
                .arg("-C")
                .arg(&path)
                .arg("--strip-components=1")
                .arg(store)
                .output()
                .unwrap();
            assert!(output.status.success(), "failed to extract {store}");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn store_path(&self) -> ProjectStorePath {
            ProjectStorePath::new(self.0.clone()).unwrap()
        }
    }

    impl Drop for TestProject {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "mirante4d-actor-{label}-{}-{nonce}-{}",
                std::process::id(),
                TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn destination(&self, name: &str) -> ProjectStorePath {
            ProjectStorePath::new(self.0.join(name)).unwrap()
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct ChildGuard(Option<Child>);

    impl ChildGuard {
        fn new(child: Child) -> Self {
            Self(Some(child))
        }

        fn child_mut(&mut self) -> &mut Child {
            self.0.as_mut().expect("child is live")
        }

        fn kill_and_wait(&mut self) -> ExitStatus {
            let mut child = self.0.take().expect("child is live");
            child.kill().unwrap();
            child.wait().unwrap()
        }

        fn wait_timeout(&mut self, timeout: Duration) -> ExitStatus {
            let deadline = Instant::now() + timeout;
            loop {
                if let Some(status) = self.child_mut().try_wait().unwrap() {
                    self.0.take();
                    return status;
                }
                assert!(Instant::now() < deadline, "child process timed out");
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let Some(mut child) = self.0.take() else {
                return;
            };
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum PinProcessOperation {
        Pin,
        Unpin,
    }

    impl PinProcessOperation {
        const fn name(self) -> &'static str {
            match self {
                Self::Pin => "pin",
                Self::Unpin => "unpin",
            }
        }

        fn parse(name: &str) -> Option<Self> {
            match name {
                "pin" => Some(Self::Pin),
                "unpin" => Some(Self::Unpin),
                _ => None,
            }
        }

        fn command(self, id: u64) -> ProjectStoreCommand {
            match self {
                Self::Pin => ProjectStoreCommand::Pin {
                    request_id: request_id(id),
                    checkpoint_id: "checkpoint-a".to_owned(),
                    generation_id: generation_id(RECOVERABLE_ORPHAN),
                },
                Self::Unpin => ProjectStoreCommand::Unpin {
                    request_id: request_id(id),
                    checkpoint_id: "checkpoint-a".to_owned(),
                },
            }
        }

        fn completion_succeeded(self, completion: ProjectStoreCompletion, id: u64) -> bool {
            match (self, completion) {
                (
                    Self::Pin,
                    ProjectStoreCompletion::Pinned {
                        request_id: actual,
                        result: Ok(()),
                    },
                )
                | (
                    Self::Unpin,
                    ProjectStoreCompletion::Unpinned {
                        request_id: actual,
                        result: Ok(()),
                    },
                ) => actual == request_id(id),
                _ => false,
            }
        }
    }

    struct MemorySource {
        descriptor: RawObjectDescriptor,
        bytes: Arc<[u8]>,
    }

    impl ProjectObjectSource for MemorySource {
        fn descriptor(&self) -> &RawObjectDescriptor {
            &self.descriptor
        }

        fn open(&self) -> io::Result<Box<dyn Read + Send>> {
            Ok(Box::new(Cursor::new(Arc::clone(&self.bytes))))
        }
    }

    #[derive(Default)]
    struct Gate {
        state: Mutex<GateState>,
        wake: Condvar,
    }

    #[derive(Default)]
    struct GateState {
        started: bool,
        released: bool,
    }

    impl Gate {
        fn wait_started(&self) {
            let mut state = self.state.lock().unwrap();
            let deadline = std::time::Instant::now() + TIMEOUT;
            while !state.started {
                let remaining = deadline
                    .checked_duration_since(std::time::Instant::now())
                    .expect("actor did not begin the gated read");
                let (next, wait) = self.wake.wait_timeout(state, remaining).unwrap();
                state = next;
                assert!(!wait.timed_out() || state.started, "gated read timed out");
            }
        }

        fn release(&self) {
            let mut state = self.state.lock().unwrap();
            state.released = true;
            self.wake.notify_all();
        }
    }

    struct GatedSource {
        descriptor: RawObjectDescriptor,
        bytes: Arc<[u8]>,
        gate: Arc<Gate>,
    }

    impl ProjectObjectSource for GatedSource {
        fn descriptor(&self) -> &RawObjectDescriptor {
            &self.descriptor
        }

        fn open(&self) -> io::Result<Box<dyn Read + Send>> {
            Ok(Box::new(GatedReader {
                bytes: Cursor::new(Arc::clone(&self.bytes)),
                gate: Arc::clone(&self.gate),
            }))
        }
    }

    struct GatedReader {
        bytes: Cursor<Arc<[u8]>>,
        gate: Arc<Gate>,
    }

    impl Read for GatedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let mut state = self.gate.state.lock().unwrap();
            state.started = true;
            self.gate.wake.notify_all();
            while !state.released {
                state = self.gate.wake.wait(state).unwrap();
            }
            drop(state);
            self.bytes.read(buffer)
        }
    }

    #[derive(Clone)]
    enum ControlledRead {
        Normal,
    }

    struct ControlledSource {
        descriptor: RawObjectDescriptor,
        bytes: Arc<[u8]>,
        opens: Arc<AtomicUsize>,
        behavior: ControlledRead,
    }

    impl ProjectObjectSource for ControlledSource {
        fn descriptor(&self) -> &RawObjectDescriptor {
            &self.descriptor
        }

        fn open(&self) -> io::Result<Box<dyn Read + Send>> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            match &self.behavior {
                ControlledRead::Normal => Ok(Box::new(Cursor::new(Arc::clone(&self.bytes)))),
            }
        }
    }

    #[test]
    fn create_and_save_as_report_unqualified_destinations_before_source_reads() {
        if env::var_os(TEST_REAL_POLICY_ENV).is_none() {
            let status = Command::new(env::current_exe().unwrap())
                .arg(UNQUALIFIED_COMMAND_DESTINATIONS_TEST)
                .arg("--exact")
                .arg("--nocapture")
                .env(TEST_REAL_POLICY_ENV, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }

        let destination = |label: &str| {
            ProjectStorePath::new(format!(
                "/dev/shm/mirante4d-actor-unqualified-{label}-{}.m4dproj",
                std::process::id()
            ))
            .unwrap()
        };

        let create_destination = destination("create");
        let create_generation = frozen_generation(STALE_MANUAL);
        let (create_capture, create_opens) = controlled_fixture_capture(
            "stale.m4dproj",
            &create_generation,
            create_generation.projection().clone(),
            None,
            None,
            None,
            ControlledRead::Normal,
        );
        let creator = ProjectStoreActor::start(Default::default()).unwrap();
        creator
            .try_submit(ProjectStoreCommand::Create {
                request_id: request_id(1),
                destination: create_destination.clone(),
                capture: create_capture,
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&creator),
            ProjectStoreCompletion::Created {
                request_id: actual,
                result: Err(ProjectStoreFault::UnsupportedFilesystem),
            } if actual == request_id(1)
        ));
        assert_eq!(create_opens.load(Ordering::SeqCst), 0);
        assert!(!create_destination.as_path().exists());
        creator.try_submit(close_command(2)).unwrap();
        assert!(matches!(
            public_recv_timeout(&creator),
            ProjectStoreCompletion::Closed {
                request_id: actual,
                result: Ok(()),
            } if actual == request_id(2)
        ));
        creator.join().unwrap();

        let source =
            TestProject::extracted_store("unqualified-save-as-source", "recoverable.m4dproj");
        let save_as_destination = destination("save-as");
        let save_as_generation = frozen_generation_in("divergent.m4dproj", DIVERGENT_INITIAL);
        let expected_fork = Some((
            save_as_generation.forked_from().unwrap().0,
            generation_id(RECOVERABLE_G2),
        ));
        let (save_as_capture, save_as_opens) = controlled_fixture_capture(
            "divergent.m4dproj",
            &save_as_generation,
            save_as_generation.projection().clone(),
            None,
            None,
            expected_fork,
            ControlledRead::Normal,
        );
        let saver =
            EstablishedProjectActor::start(&source.store_path(), Default::default()).unwrap();
        saver
            .try_submit(ProjectStoreCommand::SaveAs {
                request_id: request_id(1),
                destination: save_as_destination.clone(),
                source_generation: generation_id(RECOVERABLE_G2),
                capture: save_as_capture,
            })
            .unwrap();
        assert!(matches!(
            saver.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::SavedAs {
                request_id: actual,
                result: Err(ProjectStoreFault::UnsupportedFilesystem),
            }) if actual == request_id(1)
        ));
        assert_eq!(save_as_opens.load(Ordering::SeqCst), 0);
        assert!(!save_as_destination.as_path().exists());
        saver.try_submit(close_command(2)).unwrap();
        assert!(matches!(
            saver.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Closed {
                request_id: actual,
                result: Ok(()),
            }) if actual == request_id(2)
        ));
        saver.join().unwrap();
    }

    #[test]
    fn public_actor_fresh_create_binds_exact_resources_and_rejects_incompatible_commands() {
        let parent = TestDirectory::new("public-create");
        let destination = parent.destination("created.m4dproj");
        let frozen = frozen_generation(STALE_MANUAL);
        let actor = ProjectStoreActor::start(Default::default()).unwrap();
        assert!(actor.try_recv().is_none());

        let provisional = frozen_generation_in("provisional.m4dproj", PROVISIONAL_AUTOSAVE);
        let (invalid_autosave, _) = controlled_fixture_capture(
            "provisional.m4dproj",
            &provisional,
            provisional.projection().clone(),
            None,
            None,
            None,
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::Autosave {
                request_id: request_id(1),
                destination: None,
                capture: invalid_autosave,
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::Autosaved {
                request_id: actual,
                result: Err(ProjectStoreFault::Corruption {
                    stage: "autosave_destination"
                }),
            } if actual == request_id(1)
        ));

        let (capture, _) = controlled_fixture_capture(
            "stale.m4dproj",
            &frozen,
            frozen.projection().clone(),
            None,
            None,
            None,
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::Create {
                request_id: request_id(2),
                destination: destination.clone(),
                capture,
            })
            .unwrap();
        let opened = match public_recv_timeout(&actor) {
            ProjectStoreCompletion::Created {
                request_id: actual,
                result: Ok(opened),
            } if actual == request_id(2) => opened,
            other => panic!("unexpected Create completion: {other:?}"),
        };
        assert_eq!(opened.path(), &destination);
        assert_eq!(
            opened.project_id(),
            frozen.projection().state().project_id()
        );
        assert_eq!(opened.mode(), ProjectOpenMode::PreferWritable);
        assert_eq!(
            opened.current_manual_generation(),
            Some(generation_id(STALE_MANUAL))
        );
        assert_eq!(opened.current_autosave_generation(), None);

        let root = LocalStoreRoot::open(destination.as_path()).unwrap();
        let contender =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(
            !contender.has_writer(),
            "Create must hand its held lease directly to the actor"
        );

        actor
            .try_submit(ProjectStoreCommand::Open {
                request_id: request_id(3),
                path: destination.clone(),
                mode: ProjectOpenMode::ReadOnly,
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::Opened {
                request_id: actual,
                result: Err(ProjectStoreFault::Corruption {
                    stage: "open_session_bound"
                }),
            } if actual == request_id(3)
        ));

        actor.try_submit(close_command(4)).unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::Closed {
                request_id: actual,
                result: Ok(()),
            } if actual == request_id(4)
        ));
        assert!(actor.has_exited());
        actor.join().unwrap();
        drop(contender);
        let _writer = acquire_writer_eventually(&root);
    }

    #[test]
    fn public_actor_opens_healthy_established_and_provisional_authority() {
        let established = TestProject::extracted("public-open-established");
        let actor = ProjectStoreActor::start(Default::default()).unwrap();
        actor
            .try_submit(ProjectStoreCommand::Open {
                request_id: request_id(1),
                path: established.store_path(),
                mode: ProjectOpenMode::ReadOnly,
            })
            .unwrap();
        match public_recv_timeout(&actor) {
            ProjectStoreCompletion::Opened {
                request_id: actual,
                result: Ok((session, projection)),
            } if actual == request_id(1) => {
                assert_eq!(session.mode(), ProjectOpenMode::ReadOnly);
                assert_eq!(
                    session.current_manual_generation(),
                    Some(generation_id(STALE_MANUAL))
                );
                assert_eq!(
                    session.current_autosave_generation(),
                    Some(generation_id(STALE_AUTOSAVE))
                );
                assert_eq!(projection, *frozen_generation(STALE_MANUAL).projection());
            }
            other => panic!("unexpected established Open completion: {other:?}"),
        }
        actor.try_submit(close_command(2)).unwrap();
        public_recv_timeout(&actor);
        actor.join().unwrap();

        let provisional =
            TestProject::extracted_store("public-open-provisional", "provisional.m4dproj");
        let actor = ProjectStoreActor::start(Default::default()).unwrap();
        actor
            .try_submit(ProjectStoreCommand::Open {
                request_id: request_id(1),
                path: provisional.store_path(),
                mode: ProjectOpenMode::PreferWritable,
            })
            .unwrap();
        match public_recv_timeout(&actor) {
            ProjectStoreCompletion::Opened {
                request_id: actual,
                result: Ok((session, projection)),
            } if actual == request_id(1) => {
                assert_eq!(session.mode(), ProjectOpenMode::PreferWritable);
                assert_eq!(session.current_manual_generation(), None);
                assert_eq!(
                    session.current_autosave_generation(),
                    Some(generation_id(PROVISIONAL_AUTOSAVE))
                );
                assert_eq!(
                    projection,
                    *frozen_generation_in("provisional.m4dproj", PROVISIONAL_AUTOSAVE).projection()
                );
            }
            other => panic!("unexpected provisional Open completion: {other:?}"),
        }
        actor.try_submit(close_command(2)).unwrap();
        public_recv_timeout(&actor);
        actor.join().unwrap();
    }

    #[test]
    fn artifact_load_selection_is_nonempty_and_unique_before_enqueue() {
        let actor = ProjectStoreActor::start(Default::default()).unwrap();
        let generation = frozen_generation(STALE_MANUAL);
        let handle = generation.projection().state().artifacts()[0]
            .handle_id()
            .clone();

        for artifact_handles in [Vec::new(), vec![handle.clone(), handle.clone()]] {
            assert_eq!(
                actor.try_submit(ProjectStoreCommand::LoadArtifacts {
                    request_id: request_id(1),
                    generation_id: generation_id(STALE_MANUAL),
                    artifact_handles,
                }),
                Err(ProjectStoreFault::Corruption {
                    stage: "artifact_load_selection",
                })
            );
        }
        assert!(actor.try_recv().is_none());

        actor.try_submit(close_command(1)).unwrap();
        public_recv_timeout(&actor);
        actor.join().unwrap();
    }

    #[test]
    fn public_actor_loads_current_and_selected_recovery_artifacts_in_request_order() {
        let project = TestProject::extracted_store("public-load-artifacts", "recoverable.m4dproj");
        let current_id = generation_id(RECOVERABLE_G2);
        let current = frozen_generation_in("recoverable.m4dproj", RECOVERABLE_G2);
        let direct = current
            .projection()
            .state()
            .artifacts()
            .iter()
            .find(|artifact| {
                matches!(
                    current.bindings().get(&artifact.object().digest()),
                    Some(ArtifactStorage::Direct { .. })
                )
            })
            .unwrap()
            .clone();
        let paged = current
            .projection()
            .state()
            .artifacts()
            .iter()
            .find(|artifact| {
                matches!(
                    current.bindings().get(&artifact.object().digest()),
                    Some(ArtifactStorage::Paged { .. })
                )
            })
            .unwrap()
            .clone();
        let bundle_limit = direct.object().byte_length() + paged.object().byte_length();
        let limits = ProjectStoreLimits {
            object_or_page_bytes_max: bundle_limit,
            ..ProjectStoreLimits::default()
        };
        let actor =
            ProjectStoreActor::start(ProjectStoreConfig::new(limits, true).unwrap()).unwrap();
        actor
            .try_submit(ProjectStoreCommand::Open {
                request_id: request_id(1),
                path: project.store_path(),
                mode: ProjectOpenMode::ReadOnly,
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::Opened {
                request_id: actual,
                result: Ok(_),
            } if actual == request_id(1)
        ));

        let requested = vec![paged.handle_id().clone(), direct.handle_id().clone()];
        actor
            .try_submit(ProjectStoreCommand::LoadArtifacts {
                request_id: request_id(2),
                generation_id: current_id,
                artifact_handles: requested.clone(),
            })
            .unwrap();
        let loaded = match public_recv_timeout(&actor) {
            ProjectStoreCompletion::ArtifactsLoaded {
                request_id: actual,
                result: Ok(loaded),
            } if actual == request_id(2) => loaded,
            other => panic!("unexpected artifact load completion: {other:?}"),
        };
        assert_eq!(loaded.len(), requested.len());
        for ((loaded, handle), expected) in loaded.iter().zip(&requested).zip([&paged, &direct]) {
            assert_eq!(loaded.reference().handle_id(), handle);
            assert_eq!(loaded.reference(), expected);
            assert_eq!(
                loaded.bytes(),
                fixture_artifact_bytes("recoverable.m4dproj", &current, expected)
            );
        }

        let autosave_id = generation_id(RECOVERABLE_AUTOSAVE);
        let autosave = frozen_generation_in("recoverable.m4dproj", RECOVERABLE_AUTOSAVE);
        let autosave_handle = autosave.projection().state().artifacts()[0]
            .handle_id()
            .clone();
        actor
            .try_submit(ProjectStoreCommand::LoadArtifacts {
                request_id: request_id(3),
                generation_id: autosave_id,
                artifact_handles: vec![autosave_handle],
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::ArtifactsLoaded {
                request_id: actual,
                result: Ok(loaded),
            } if actual == request_id(3) && loaded.len() == 1
        ));

        let recovery_id = autosave_id;
        let recovery_handle = autosave.projection().state().artifacts()[0]
            .handle_id()
            .clone();
        actor
            .try_submit(ProjectStoreCommand::OpenRecovery {
                request_id: request_id(4),
                generation_id: recovery_id,
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::RecoveryOpened {
                request_id: actual,
                result: Ok(_),
            } if actual == request_id(4)
        ));
        actor
            .try_submit(ProjectStoreCommand::LoadArtifacts {
                request_id: request_id(5),
                generation_id: recovery_id,
                artifact_handles: vec![recovery_handle],
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::ArtifactsLoaded {
                request_id: actual,
                result: Ok(loaded),
            } if actual == request_id(5) && loaded.len() == 1
        ));
        actor
            .try_submit(ProjectStoreCommand::LoadArtifacts {
                request_id: request_id(6),
                generation_id: current_id,
                artifact_handles: vec![direct.handle_id().clone()],
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::ArtifactsLoaded {
                request_id: actual,
                result: Err(ProjectStoreFault::Corruption {
                    stage: "artifact_load_generation"
                }),
            } if actual == request_id(6)
        ));

        actor.try_submit(close_command(7)).unwrap();
        public_recv_timeout(&actor);
        actor.join().unwrap();
    }

    #[test]
    fn artifact_load_returns_no_partial_bundle_when_a_later_object_is_corrupt() {
        let project = TestProject::extracted_store("atomic-load", "recoverable.m4dproj");
        let generation = frozen_generation_in("recoverable.m4dproj", RECOVERABLE_G2);
        let artifacts = generation.projection().state().artifacts();
        let direct = artifacts
            .iter()
            .find(|artifact| {
                matches!(
                    generation.bindings().get(&artifact.object().digest()),
                    Some(ArtifactStorage::Direct { .. })
                )
            })
            .unwrap();
        let paged = artifacts
            .iter()
            .find(|artifact| {
                matches!(
                    generation.bindings().get(&artifact.object().digest()),
                    Some(ArtifactStorage::Paged { .. })
                )
            })
            .unwrap();
        let ArtifactStorage::Paged { binding_manifest } =
            generation.bindings().get(&paged.object().digest()).unwrap()
        else {
            unreachable!()
        };
        let binding_bytes = fs::read(project_object_path(
            project.path(),
            binding_manifest.digest(),
        ))
        .unwrap();
        let limits = ProjectStoreLimits {
            object_or_page_bytes_max: direct.object().byte_length() + paged.object().byte_length(),
            ..ProjectStoreLimits::default()
        };
        let binding =
            LogicalObjectBinding::decode(&binding_bytes, paged.object(), binding_manifest, limits)
                .unwrap();
        let final_page = binding.pages().last().unwrap().object();
        let final_page_path = project_object_path(project.path(), final_page.digest());
        let mut corrupt = fs::read(&final_page_path).unwrap();
        corrupt[0] ^= 1;
        fs::write(final_page_path, corrupt).unwrap();

        let actor =
            ProjectStoreActor::start(ProjectStoreConfig::new(limits, true).unwrap()).unwrap();
        actor
            .try_submit(ProjectStoreCommand::Open {
                request_id: request_id(1),
                path: project.store_path(),
                mode: ProjectOpenMode::ReadOnly,
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::Opened { result: Ok(_), .. }
        ));
        actor
            .try_submit(ProjectStoreCommand::LoadArtifacts {
                request_id: request_id(2),
                generation_id: generation_id(RECOVERABLE_G2),
                artifact_handles: vec![direct.handle_id().clone(), paged.handle_id().clone()],
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::ArtifactsLoaded {
                request_id: actual,
                result: Err(ProjectStoreFault::Corruption {
                    stage: "artifact_load_object"
                }),
            } if actual == request_id(2)
        ));

        actor.try_submit(close_command(3)).unwrap();
        public_recv_timeout(&actor);
        actor.join().unwrap();
    }

    #[test]
    fn queued_artifact_load_cancellation_returns_no_bundle() {
        let project = TestProject::extracted("cancel-load");
        let actor =
            EstablishedProjectActor::start(&project.store_path(), Default::default()).unwrap();
        let (capture, gate) = gated_manual_capture();
        actor
            .try_submit(ProjectStoreCommand::ManualSave {
                request_id: request_id(1),
                capture,
            })
            .unwrap();
        gate.wait_started();

        let handle = frozen_generation(STALE_MANUAL)
            .projection()
            .state()
            .artifacts()[0]
            .handle_id()
            .clone();
        actor
            .try_submit(ProjectStoreCommand::LoadArtifacts {
                request_id: request_id(2),
                generation_id: generation_id(STALE_MANUAL),
                artifact_handles: vec![handle],
            })
            .unwrap();
        actor.try_submit(cancel_command(3, 2)).unwrap();
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 2);
        assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 3);

        actor.try_submit(cancel_command(4, 1)).unwrap();
        assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 4);
        gate.release();
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 1);
        actor.try_submit(close_command(5)).unwrap();
        actor.recv_timeout(TIMEOUT).unwrap();
        actor.join().unwrap();
    }

    #[test]
    fn public_actor_first_and_advancing_provisional_autosave_then_manual_handoff() {
        let recovery_parent = TestDirectory::new("public-provisional");
        let provisional_path = recovery_parent.destination("unsaved.m4dproj");
        let manual_parent = TestDirectory::new("public-provisional-manual");
        let manual_path = manual_parent.destination("saved.m4dproj");
        let frozen = frozen_generation_in("provisional.m4dproj", PROVISIONAL_AUTOSAVE);
        let first_id = generation_id(PROVISIONAL_AUTOSAVE);
        let actor = ProjectStoreActor::start(Default::default()).unwrap();

        let (first, _) = controlled_fixture_capture(
            "provisional.m4dproj",
            &frozen,
            frozen.projection().clone(),
            None,
            None,
            None,
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::Autosave {
                request_id: request_id(1),
                destination: Some(provisional_path.clone()),
                capture: first,
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::Autosaved {
                request_id: actual,
                result: Ok(receipt),
            } if actual == request_id(1)
                && receipt.current_generation_id() == first_id
                && receipt.previous_generation_id().is_none()
                && receipt.autosave_base_generation_id().is_none()
        ));

        let next_projection = next_revision_projection(&frozen);
        let (advance, _) = controlled_fixture_capture(
            "provisional.m4dproj",
            &frozen,
            next_projection.clone(),
            Some(first_id),
            None,
            None,
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::Autosave {
                request_id: request_id(2),
                destination: None,
                capture: advance,
            })
            .unwrap();
        let advanced_id = match public_recv_timeout(&actor) {
            ProjectStoreCompletion::Autosaved {
                request_id: actual,
                result: Ok(receipt),
            } if actual == request_id(2) => {
                assert_eq!(receipt.previous_generation_id(), Some(first_id));
                assert_eq!(receipt.autosave_base_generation_id(), None);
                receipt.current_generation_id()
            }
            other => panic!("unexpected advancing provisional Autosave: {other:?}"),
        };
        assert_ne!(advanced_id, first_id);

        let provisional_before = file_tree(provisional_path.as_path());
        let manual =
            ProjectCommitCapture::new(next_projection.clone(), None, None, None, Vec::new())
                .unwrap();
        actor
            .try_submit(ProjectStoreCommand::Create {
                request_id: request_id(3),
                destination: manual_path.clone(),
                capture: manual,
            })
            .unwrap();
        match public_recv_timeout(&actor) {
            ProjectStoreCompletion::Created {
                request_id: actual,
                result: Ok(session),
            } if actual == request_id(3) => {
                assert_eq!(session.path(), &manual_path);
                assert_eq!(session.project_id(), next_projection.state().project_id());
                assert!(session.current_manual_generation().is_some());
                assert_eq!(session.current_autosave_generation(), None);
            }
            other => panic!("unexpected provisional Create handoff: {other:?}"),
        }
        assert_eq!(file_tree(provisional_path.as_path()), provisional_before);
        let old_root = LocalStoreRoot::open(provisional_path.as_path()).unwrap();
        let _old_writer = acquire_writer_eventually(&old_root);
        let new_root = LocalStoreRoot::open(manual_path.as_path()).unwrap();
        let new_contender =
            ProjectStoreLeases::acquire(&new_root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(!new_contender.has_writer());

        actor.try_submit(close_command(4)).unwrap();
        public_recv_timeout(&actor);
        actor.join().unwrap();
    }

    #[test]
    fn public_actor_corrupt_open_is_recovery_only_until_selected_save_as() {
        let source = TestProject::extracted_store("public-recovery-only", "recoverable.m4dproj");
        let head = source.path().join("refs/head");
        let mut corrupt_head = fs::read(&head).unwrap();
        corrupt_head[0] ^= 1;
        fs::write(&head, &corrupt_head).unwrap();
        let actor = ProjectStoreActor::start(Default::default()).unwrap();
        actor
            .try_submit(ProjectStoreCommand::Open {
                request_id: request_id(1),
                path: source.store_path(),
                mode: ProjectOpenMode::PreferWritable,
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::Opened {
                request_id: actual,
                result: Err(ProjectStoreFault::Corruption { .. }),
            } if actual == request_id(1)
        ));

        let selected_id = generation_id(RECOVERABLE_G1);
        let selected = frozen_generation_in("recoverable.m4dproj", RECOVERABLE_G1);
        let (blocked, blocked_opens) = controlled_fixture_capture(
            "recoverable.m4dproj",
            &selected,
            selected.projection().clone(),
            Some(selected_id),
            None,
            selected.forked_from(),
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::ManualSave {
                request_id: request_id(2),
                capture: blocked,
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::ManualSaved {
                request_id: actual,
                result: Err(ProjectStoreFault::Corruption {
                    stage: "actor_recovery_only"
                }),
            } if actual == request_id(2)
        ));
        assert_eq!(blocked_opens.load(Ordering::SeqCst), 0);

        actor
            .try_submit(ProjectStoreCommand::InspectRecovery {
                request_id: request_id(3),
            })
            .unwrap();
        match public_recv_timeout(&actor) {
            ProjectStoreCompletion::RecoveryInspected {
                request_id: actual,
                result: Ok(candidates),
            } if actual == request_id(3) => {
                assert!(
                    candidates
                        .iter()
                        .any(|candidate| candidate.generation_id() == selected_id)
                );
            }
            other => panic!("unexpected recovery inspection: {other:?}"),
        }
        actor
            .try_submit(ProjectStoreCommand::OpenRecovery {
                request_id: request_id(4),
                generation_id: selected_id,
            })
            .unwrap();
        let selected_projection = match public_recv_timeout(&actor) {
            ProjectStoreCompletion::RecoveryOpened {
                request_id: actual,
                result: Ok((_session, projection)),
            } if actual == request_id(4) => projection,
            other => panic!("unexpected recovery selection: {other:?}"),
        };

        let source_before = file_tree(source.path());
        let destination_parent = TestDirectory::new("public-recovery-save-as");
        let destination = destination_parent.destination("recovered.m4dproj");
        let new_project_id = ProjectId::from_bytes([0x42; 16]);
        assert!(selected_projection.revision_high_water().sequence() > 0);
        let recovered_projection = fresh_fork_projection(
            &selected,
            new_project_id,
            selected_projection.state().dataset().clone(),
        );
        assert_eq!(recovered_projection.revision().sequence(), 0);
        assert_eq!(recovered_projection.revision_high_water().sequence(), 0);
        let source_project_id = selected_projection.state().project_id();
        let rejected_destination =
            ProjectStorePath::new(source.path().join("rejected-recovery-save-as.m4dproj")).unwrap();
        let rejected_capture = ProjectCommitCapture::new(
            recovered_projection.clone(),
            None,
            None,
            Some((source_project_id, selected_id)),
            Vec::new(),
        )
        .unwrap();
        actor
            .try_submit(ProjectStoreCommand::SaveAs {
                request_id: request_id(5),
                destination: rejected_destination.clone(),
                source_generation: selected_id,
                capture: rejected_capture,
            })
            .unwrap();
        assert_saved_as_fault(
            public_recv_timeout(&actor),
            5,
            ProjectStoreFault::Corruption {
                stage: "destination_source_overlap",
            },
        );
        assert!(!rejected_destination.as_path().exists());
        assert_eq!(file_tree(source.path()), source_before);

        let (capture, capture_opens) = controlled_fixture_capture(
            "recoverable.m4dproj",
            &selected,
            recovered_projection,
            None,
            None,
            Some((source_project_id, selected_id)),
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::SaveAs {
                request_id: request_id(6),
                destination: destination.clone(),
                source_generation: selected_id,
                capture,
            })
            .unwrap();
        let recovered_id = match public_recv_timeout(&actor) {
            ProjectStoreCompletion::SavedAs {
                request_id: actual,
                result: Ok(receipt),
            } if actual == request_id(6) => receipt.current_generation_id(),
            other => panic!("unexpected recovery Save As: {other:?}"),
        };
        assert_eq!(capture_opens.load(Ordering::SeqCst), 0);
        assert_eq!(file_tree(source.path()), source_before);
        let destination_root = LocalStoreRoot::open(destination.as_path()).unwrap();
        let recovered =
            inspect_established_store(&destination_root, ProjectStoreLimits::default(), || false)
                .unwrap();
        assert_eq!(recovered.manual().head.current(), recovered_id);
        assert_eq!(
            recovered.manual_generation().bindings(),
            selected.bindings()
        );
        assert_eq!(
            recovered.manual_generation().reachable_objects(),
            selected.reachable_objects()
        );
        assert_eq!(
            recovered.manual_generation().forked_from(),
            Some((source_project_id, selected_id))
        );

        actor.try_submit(close_command(7)).unwrap();
        public_recv_timeout(&actor);
        actor.join().unwrap();
    }

    #[test]
    fn public_actor_normal_recovery_selection_is_save_as_only() {
        let source =
            TestProject::extracted_store("public-normal-recovery-selection", "recoverable.m4dproj");
        let source_before = file_tree(source.path());
        let actor = ProjectStoreActor::start(Default::default()).unwrap();
        actor
            .try_submit(ProjectStoreCommand::Open {
                request_id: request_id(1),
                path: source.store_path(),
                mode: ProjectOpenMode::PreferWritable,
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::Opened {
                request_id: actual,
                result: Ok(_),
            } if actual == request_id(1)
        ));

        let selected_id = generation_id(RECOVERABLE_AUTOSAVE);
        let selected = frozen_generation_in("recoverable.m4dproj", RECOVERABLE_AUTOSAVE);
        actor
            .try_submit(ProjectStoreCommand::OpenRecovery {
                request_id: request_id(2),
                generation_id: selected_id,
            })
            .unwrap();
        let selected_projection = match public_recv_timeout(&actor) {
            ProjectStoreCompletion::RecoveryOpened {
                request_id: actual,
                result: Ok((_session, projection)),
            } if actual == request_id(2) => projection,
            other => panic!("unexpected normal recovery selection: {other:?}"),
        };
        assert_eq!(selected_projection, *selected.projection());

        let orphan = frozen_generation_in("recoverable.m4dproj", RECOVERABLE_ORPHAN);
        let (manual_capture, manual_opens) = controlled_fixture_capture(
            "recoverable.m4dproj",
            &orphan,
            orphan.projection().clone(),
            orphan.parent_generation_id(),
            orphan.base_manual_generation_id(),
            orphan.forked_from(),
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::ManualSave {
                request_id: request_id(3),
                capture: manual_capture,
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::ManualSaved {
                request_id: actual,
                result: Err(ProjectStoreFault::Corruption {
                    stage: "actor_recovery_selected"
                }),
            } if actual == request_id(3)
        ));
        assert_eq!(manual_opens.load(Ordering::SeqCst), 0);

        let (autosave_capture, autosave_opens) = controlled_fixture_capture(
            "recoverable.m4dproj",
            &orphan,
            orphan.projection().clone(),
            Some(selected_id),
            Some(generation_id(RECOVERABLE_G2)),
            orphan.forked_from(),
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::Autosave {
                request_id: request_id(4),
                destination: None,
                capture: autosave_capture,
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::Autosaved {
                request_id: actual,
                result: Err(ProjectStoreFault::Corruption {
                    stage: "actor_recovery_selected"
                }),
            } if actual == request_id(4)
        ));
        assert_eq!(autosave_opens.load(Ordering::SeqCst), 0);
        assert_eq!(file_tree(source.path()), source_before);

        actor
            .try_submit(ProjectStoreCommand::OpenRecovery {
                request_id: request_id(5),
                generation_id: generation_id(RECOVERABLE_ORPHAN),
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::RecoveryOpened {
                request_id: actual,
                result: Err(ProjectStoreFault::Corruption {
                    stage: "recovery_selection"
                }),
            } if actual == request_id(5)
        ));

        let destination_parent = TestDirectory::new("public-normal-recovery-save-as");
        let destination = destination_parent.destination("recovered.m4dproj");
        let source_project_id = selected_projection.state().project_id();
        assert!(selected_projection.revision_high_water().sequence() > 0);
        let recovered_projection = fresh_fork_projection(
            &selected,
            ProjectId::from_bytes([0x43; 16]),
            selected_projection.state().dataset().clone(),
        );
        assert_eq!(recovered_projection.revision().sequence(), 0);
        assert_eq!(recovered_projection.revision_high_water().sequence(), 0);
        let (capture, capture_opens) = controlled_fixture_capture(
            "recoverable.m4dproj",
            &selected,
            recovered_projection,
            None,
            None,
            Some((source_project_id, selected_id)),
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::SaveAs {
                request_id: request_id(6),
                destination: destination.clone(),
                source_generation: selected_id,
                capture,
            })
            .unwrap();
        let recovered_id = match public_recv_timeout(&actor) {
            ProjectStoreCompletion::SavedAs {
                request_id: actual,
                result: Ok(receipt),
            } if actual == request_id(6) => receipt.current_generation_id(),
            other => panic!("unexpected normal recovery Save As: {other:?}"),
        };
        assert_eq!(capture_opens.load(Ordering::SeqCst), 0);
        assert_eq!(file_tree(source.path()), source_before);

        let destination_root = LocalStoreRoot::open(destination.as_path()).unwrap();
        let recovered =
            inspect_established_store(&destination_root, ProjectStoreLimits::default(), || false)
                .unwrap();
        assert_eq!(recovered.manual().head.current(), recovered_id);
        assert_eq!(
            recovered.manual_generation().bindings(),
            selected.bindings()
        );
        assert_eq!(
            recovered.manual_generation().reachable_objects(),
            selected.reachable_objects()
        );
        assert_eq!(
            recovered.manual_generation().forked_from(),
            Some((source_project_id, selected_id))
        );

        actor.try_submit(close_command(7)).unwrap();
        public_recv_timeout(&actor);
        actor.join().unwrap();
    }

    #[test]
    fn established_commands_are_serialized_and_close_releases_the_writer_lease() {
        let project = TestProject::extracted("serial-close");
        let actor = EstablishedProjectActor::start_bound(&project.store_path(), Default::default())
            .unwrap();
        assert!(actor.try_recv().is_none());

        let contender_root = LocalStoreRoot::open(project.path()).unwrap();
        let contender =
            ProjectStoreLeases::acquire(&contender_root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(!contender.has_writer());

        actor.try_submit(autosave_command(1)).unwrap();
        actor.try_submit(manual_command(2)).unwrap();
        let first = actor.recv_timeout(TIMEOUT).unwrap();
        let second = actor.recv_timeout(TIMEOUT).unwrap();
        assert!(matches!(
            first,
            ProjectStoreCompletion::Autosaved {
                request_id: actual_id,
                result: Ok(_)
            } if actual_id == request_id(1)
        ));
        assert!(matches!(
            second,
            ProjectStoreCompletion::ManualSaved {
                request_id: actual_id,
                result: Ok(_)
            } if actual_id == request_id(2)
        ));

        actor.try_submit(close_command(3)).unwrap();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Closed {
                request_id: actual_id,
                result: Ok(())
            }) if actual_id == request_id(3)
        ));
        assert!(actor.has_exited());
        actor.join().unwrap();
        drop(contender);

        let _next = acquire_writer_eventually(&contender_root);
    }

    #[test]
    fn recovery_inspection_and_open_preserve_heads_and_bytes_across_session_modes() {
        let project = TestProject::extracted_store("recovery", "recoverable.m4dproj");
        let path = project.store_path();
        let before = file_tree(project.path());
        let actor = EstablishedProjectActor::start(&path, Default::default()).unwrap();

        actor
            .try_submit(ProjectStoreCommand::InspectRecovery {
                request_id: request_id(1),
            })
            .unwrap();
        let candidate = match actor.recv_timeout(TIMEOUT).unwrap() {
            ProjectStoreCompletion::RecoveryInspected {
                request_id: actual,
                result: Ok(candidates),
            } if actual == request_id(1) && candidates.len() == 1 => {
                candidates.into_iter().next().unwrap()
            }
            other => panic!("unexpected recovery inspection: {other:?}"),
        };
        assert_eq!(
            candidate.generation_id(),
            generation_id(RECOVERABLE_AUTOSAVE)
        );
        assert_eq!(candidate.origin(), "autosave_head");
        assert!(candidate.is_newer());
        assert_eq!(
            candidate.current_manual_generation_id(),
            Some(generation_id(RECOVERABLE_G2))
        );

        actor
            .try_submit(ProjectStoreCommand::OpenRecovery {
                request_id: request_id(2),
                generation_id: candidate.generation_id(),
            })
            .unwrap();
        let (session, projection) = match actor.recv_timeout(TIMEOUT).unwrap() {
            ProjectStoreCompletion::RecoveryOpened {
                request_id: actual,
                result: Ok(opened),
            } if actual == request_id(2) => opened,
            other => panic!("unexpected recovery open: {other:?}"),
        };
        assert_eq!(session.path(), &path);
        assert_eq!(session.project_id(), projection.state().project_id());
        assert_eq!(session.mode(), ProjectOpenMode::PreferWritable);
        assert_eq!(
            session.current_manual_generation(),
            Some(generation_id(RECOVERABLE_G2))
        );
        assert_eq!(
            session.current_autosave_generation(),
            Some(generation_id(RECOVERABLE_AUTOSAVE))
        );
        assert_eq!(
            projection.revision().sequence(),
            candidate.revision_sequence()
        );
        assert_eq!(file_tree(project.path()), before);

        actor
            .try_submit(ProjectStoreCommand::OpenRecovery {
                request_id: request_id(3),
                generation_id: generation_id(RECOVERABLE_ORPHAN),
            })
            .unwrap();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::RecoveryOpened {
                request_id: actual,
                result: Err(ProjectStoreFault::Corruption {
                    stage: "recovery_selection"
                }),
            }) if actual == request_id(3)
        ));
        assert_eq!(file_tree(project.path()), before);

        let orphan = frozen_generation_in("recoverable.m4dproj", RECOVERABLE_ORPHAN);
        let (capture, opens) = controlled_fixture_capture(
            "recoverable.m4dproj",
            &orphan,
            orphan.projection().clone(),
            orphan.parent_generation_id(),
            orphan.base_manual_generation_id(),
            orphan.forked_from(),
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::ManualSave {
                request_id: request_id(4),
                capture,
            })
            .unwrap();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::ManualSaved {
                request_id: actual,
                result: Err(ProjectStoreFault::Corruption {
                    stage: "actor_recovery_selected"
                }),
            }) if actual == request_id(4)
        ));
        assert_eq!(opens.load(Ordering::SeqCst), 0);
        assert_eq!(file_tree(project.path()), before);

        actor.try_submit(close_command(5)).unwrap();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Closed {
                request_id: actual,
                result: Ok(()),
            }) if actual == request_id(5)
        ));
        actor.join().unwrap();

        let corrupt = TestProject::extracted_store("recovery-corrupt", "recoverable.m4dproj");
        let head = corrupt.path().join("refs/head");
        let mut head_bytes = fs::read(&head).unwrap();
        head_bytes[0] ^= 1;
        fs::write(&head, &head_bytes).unwrap();
        let corrupt_actor =
            EstablishedProjectActor::start(&corrupt.store_path(), Default::default()).unwrap();
        corrupt_actor
            .try_submit(ProjectStoreCommand::InspectRecovery {
                request_id: request_id(1),
            })
            .unwrap();
        match corrupt_actor.recv_timeout(TIMEOUT).unwrap() {
            ProjectStoreCompletion::RecoveryInspected {
                request_id: actual,
                result: Ok(candidates),
            } if actual == request_id(1) => {
                assert_eq!(candidates.len(), 2);
                assert!(
                    candidates
                        .iter()
                        .any(|candidate| candidate.origin() == "manual_recovery")
                );
            }
            other => panic!("unexpected corrupt-head recovery inspection: {other:?}"),
        }
        assert_eq!(fs::read(&head).unwrap(), head_bytes);
        corrupt_actor.try_submit(close_command(2)).unwrap();
        corrupt_actor.recv_timeout(TIMEOUT).unwrap();
        corrupt_actor.join().unwrap();

        let contended = TestProject::extracted("recovery-read-only");
        let contended_path = contended.store_path();
        let contended_before = file_tree(contended.path());
        let writer = EstablishedProjectActor::start(&contended_path, Default::default()).unwrap();
        let reader = EstablishedProjectActor::start(&contended_path, Default::default()).unwrap();
        reader
            .try_submit(ProjectStoreCommand::InspectRecovery {
                request_id: request_id(1),
            })
            .unwrap();
        let recovery_id = match reader.recv_timeout(TIMEOUT).unwrap() {
            ProjectStoreCompletion::RecoveryInspected {
                request_id: actual,
                result: Ok(candidates),
            } if actual == request_id(1) && candidates.len() == 1 => candidates[0].generation_id(),
            other => panic!("unexpected read-only recovery inspection: {other:?}"),
        };
        reader
            .try_submit(ProjectStoreCommand::OpenRecovery {
                request_id: request_id(2),
                generation_id: recovery_id,
            })
            .unwrap();
        match reader.recv_timeout(TIMEOUT).unwrap() {
            ProjectStoreCompletion::RecoveryOpened {
                request_id: actual,
                result: Ok((session, _)),
            } if actual == request_id(2) => {
                assert_eq!(session.path(), &contended_path);
                assert_eq!(session.mode(), ProjectOpenMode::ReadOnly);
                assert_eq!(
                    session.current_manual_generation(),
                    Some(generation_id(STALE_MANUAL))
                );
                assert_eq!(
                    session.current_autosave_generation(),
                    Some(generation_id(STALE_AUTOSAVE))
                );
            }
            other => panic!("unexpected read-only recovery open: {other:?}"),
        }
        reader.try_submit(manual_command(3)).unwrap();
        assert!(matches!(
            reader.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::ManualSaved {
                request_id: actual,
                result: Err(ProjectStoreFault::Corruption {
                    stage: "actor_recovery_selected"
                }),
            }) if actual == request_id(3)
        ));
        assert_eq!(file_tree(contended.path()), contended_before);
        reader.try_submit(close_command(4)).unwrap();
        reader.recv_timeout(TIMEOUT).unwrap();
        reader.join().unwrap();
        writer.try_submit(close_command(1)).unwrap();
        writer.recv_timeout(TIMEOUT).unwrap();
        writer.join().unwrap();
    }

    #[test]
    fn save_as_switches_the_owned_session_and_transfers_writer_lease() {
        let source = TestProject::extracted_store("save-as-source", "recoverable.m4dproj");
        let destination_parent = TestDirectory::new("save-as-success");
        let destination = destination_parent.destination("fork.m4dproj");
        let actor =
            EstablishedProjectActor::start(&source.store_path(), Default::default()).unwrap();
        let source_before = file_tree(source.path());

        let initial = frozen_generation_in("divergent.m4dproj", DIVERGENT_INITIAL);
        let expected_fork = Some((
            initial.forked_from().unwrap().0,
            generation_id(RECOVERABLE_G2),
        ));
        assert_eq!(initial.forked_from(), expected_fork);
        let (capture, _) = controlled_fixture_capture(
            "divergent.m4dproj",
            &initial,
            initial.projection().clone(),
            None,
            None,
            expected_fork,
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::SaveAs {
                request_id: request_id(1),
                destination: destination.clone(),
                source_generation: generation_id(RECOVERABLE_G2),
                capture,
            })
            .unwrap();
        let receipt = match actor.recv_timeout(TIMEOUT).unwrap() {
            ProjectStoreCompletion::SavedAs {
                request_id: actual,
                result: Ok(receipt),
            } if actual == request_id(1) => receipt,
            other => panic!("unexpected Save As completion: {other:?}"),
        };
        assert_eq!(
            receipt.new_generation_id(),
            generation_id(DIVERGENT_INITIAL)
        );
        assert_eq!(
            receipt.current_generation_id(),
            generation_id(DIVERGENT_INITIAL)
        );
        assert_eq!(receipt.previous_generation_id(), None);
        assert_eq!(receipt.autosave_base_generation_id(), None);
        assert_eq!(file_tree(source.path()), source_before);

        let installed_root = LocalStoreRoot::open(destination.as_path()).unwrap();
        let installed =
            inspect_established_store(&installed_root, ProjectStoreLimits::default(), || false)
                .unwrap();
        assert_eq!(
            installed.project_id(),
            initial.projection().state().project_id()
        );
        assert_eq!(
            installed.manual().head.current(),
            generation_id(DIVERGENT_INITIAL)
        );
        assert_eq!(installed.manual_generation().forked_from(), expected_fork);

        let source_root = LocalStoreRoot::open(source.path()).unwrap();
        let _source_writer = acquire_writer_eventually(&source_root);
        let target_contender =
            ProjectStoreLeases::acquire(&installed_root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(!target_contender.has_writer());

        let next = frozen_generation_in("divergent.m4dproj", DIVERGENT_NEXT);
        let (next_capture, _) = controlled_fixture_capture(
            "divergent.m4dproj",
            &next,
            next.projection().clone(),
            next.parent_generation_id(),
            next.base_manual_generation_id(),
            next.forked_from(),
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::ManualSave {
                request_id: request_id(2),
                capture: next_capture,
            })
            .unwrap();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::ManualSaved {
                request_id: actual,
                result: Ok(receipt),
            }) if actual == request_id(2)
                && receipt.current_generation_id() == generation_id(DIVERGENT_NEXT)
        ));

        actor.try_submit(close_command(3)).unwrap();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Closed {
                request_id: actual,
                result: Ok(()),
            }) if actual == request_id(3)
        ));
        actor.join().unwrap();
        drop(target_contender);
        let _target_writer = acquire_writer_eventually(&installed_root);
    }

    #[test]
    fn public_actor_reopened_save_as_inherits_held_fork_for_manual_and_autosave() {
        let source = TestProject::extracted_store(
            "reopened-save-as-provenance-source",
            "recoverable.m4dproj",
        );
        let destination_parent = TestDirectory::new("reopened-save-as-provenance-target");
        let destination = destination_parent.destination("fork.m4dproj");
        let initial = frozen_generation_in("divergent.m4dproj", DIVERGENT_INITIAL);
        let expected_fork = Some((
            initial.forked_from().unwrap().0,
            generation_id(RECOVERABLE_G2),
        ));

        let actor = ProjectStoreActor::start(Default::default()).unwrap();
        actor
            .try_submit(ProjectStoreCommand::Open {
                request_id: request_id(1),
                path: source.store_path(),
                mode: ProjectOpenMode::PreferWritable,
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::Opened {
                request_id: actual,
                result: Ok(_),
            } if actual == request_id(1)
        ));
        let (capture, _) = controlled_fixture_capture(
            "divergent.m4dproj",
            &initial,
            initial.projection().clone(),
            None,
            None,
            expected_fork,
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::SaveAs {
                request_id: request_id(2),
                destination: destination.clone(),
                source_generation: generation_id(RECOVERABLE_G2),
                capture,
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&actor),
            ProjectStoreCompletion::SavedAs {
                request_id: actual,
                result: Ok(_),
            } if actual == request_id(2)
        ));
        actor.try_submit(close_command(3)).unwrap();
        public_recv_timeout(&actor);
        actor.join().unwrap();

        let reopened = ProjectStoreActor::start(Default::default()).unwrap();
        reopened
            .try_submit(ProjectStoreCommand::Open {
                request_id: request_id(1),
                path: destination.clone(),
                mode: ProjectOpenMode::PreferWritable,
            })
            .unwrap();
        match public_recv_timeout(&reopened) {
            ProjectStoreCompletion::Opened {
                request_id: actual,
                result: Ok((session, projection)),
            } if actual == request_id(1) => {
                assert_eq!(
                    session.current_manual_generation(),
                    Some(generation_id(DIVERGENT_INITIAL))
                );
                assert_eq!(projection, *initial.projection());
            }
            other => panic!("unexpected reopened Save As project: {other:?}"),
        }

        let next = frozen_generation_in("divergent.m4dproj", DIVERGENT_NEXT);
        let wrong_fork = Some((
            ProjectId::from_bytes([0x77; 16]),
            generation_id(RECOVERABLE_G1),
        ));
        assert_ne!(wrong_fork, expected_fork);
        let (wrong_capture, wrong_opens) = controlled_fixture_capture_with_all_sources(
            "divergent.m4dproj",
            &next,
            next.projection().clone(),
            next.parent_generation_id(),
            next.base_manual_generation_id(),
            wrong_fork,
            ControlledRead::Normal,
        );
        assert!(wrong_capture.object_source_count() > 0);
        let before_rejected_save = file_tree(destination.as_path());
        reopened
            .try_submit(ProjectStoreCommand::ManualSave {
                request_id: request_id(2),
                capture: wrong_capture,
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&reopened),
            ProjectStoreCompletion::ManualSaved {
                request_id: actual,
                result: Err(ProjectStoreFault::Corruption {
                    stage: "manual_generation_continuity"
                }),
            } if actual == request_id(2)
        ));
        assert_eq!(wrong_opens.load(Ordering::SeqCst), 0);
        assert_eq!(file_tree(destination.as_path()), before_rejected_save);

        let (manual_capture, _) = controlled_fixture_capture(
            "divergent.m4dproj",
            &next,
            next.projection().clone(),
            next.parent_generation_id(),
            next.base_manual_generation_id(),
            None,
            ControlledRead::Normal,
        );
        reopened
            .try_submit(ProjectStoreCommand::ManualSave {
                request_id: request_id(3),
                capture: manual_capture,
            })
            .unwrap();
        let manual_id = match public_recv_timeout(&reopened) {
            ProjectStoreCompletion::ManualSaved {
                request_id: actual,
                result: Ok(receipt),
            } if actual == request_id(3) => receipt.current_generation_id(),
            other => panic!("unexpected inherited-fork ManualSave: {other:?}"),
        };

        let autosave_projection = next_revision_projection(&next);
        let (wrong_autosave_capture, wrong_autosave_opens) =
            controlled_fixture_capture_with_all_sources(
                "divergent.m4dproj",
                &next,
                autosave_projection.clone(),
                None,
                Some(manual_id),
                wrong_fork,
                ControlledRead::Normal,
            );
        assert!(wrong_autosave_capture.object_source_count() > 0);
        let before_rejected_autosave = file_tree(destination.as_path());
        reopened
            .try_submit(ProjectStoreCommand::Autosave {
                request_id: request_id(4),
                destination: None,
                capture: wrong_autosave_capture,
            })
            .unwrap();
        assert!(matches!(
            public_recv_timeout(&reopened),
            ProjectStoreCompletion::Autosaved {
                request_id: actual,
                result: Err(ProjectStoreFault::Corruption {
                    stage: "manual_generation_continuity"
                }),
            } if actual == request_id(4)
        ));
        assert_eq!(wrong_autosave_opens.load(Ordering::SeqCst), 0);
        assert_eq!(file_tree(destination.as_path()), before_rejected_autosave);

        let autosave_capture =
            ProjectCommitCapture::new(autosave_projection, None, Some(manual_id), None, Vec::new())
                .unwrap();
        reopened
            .try_submit(ProjectStoreCommand::Autosave {
                request_id: request_id(5),
                destination: None,
                capture: autosave_capture,
            })
            .unwrap();
        let autosave_id = match public_recv_timeout(&reopened) {
            ProjectStoreCompletion::Autosaved {
                request_id: actual,
                result: Ok(receipt),
            } if actual == request_id(5) => receipt.current_generation_id(),
            other => panic!("unexpected inherited-fork Autosave: {other:?}"),
        };

        let root = LocalStoreRoot::open(destination.as_path()).unwrap();
        let inspection =
            inspect_established_store(&root, ProjectStoreLimits::default(), || false).unwrap();
        assert_eq!(inspection.manual().head.current(), manual_id);
        assert_eq!(inspection.manual_generation().forked_from(), expected_fork);
        assert_eq!(
            inspection.autosave().map(|lane| lane.head.current()),
            Some(autosave_id)
        );
        assert_eq!(
            inspection
                .autosave_generation()
                .expect("the inherited-fork autosave is current")
                .forked_from(),
            expected_fork
        );

        reopened.try_submit(close_command(6)).unwrap();
        public_recv_timeout(&reopened);
        reopened.join().unwrap();
    }

    #[test]
    fn save_as_authentication_rejects_stale_fork_target_and_science_before_reads() {
        {
            let source = TestProject::extracted_store("save-as-stale", "recoverable.m4dproj");
            let destinations = TestDirectory::new("save-as-stale-target");
            let destination = destinations.destination("stale.m4dproj");
            let actor =
                EstablishedProjectActor::start(&source.store_path(), Default::default()).unwrap();
            let orphan = frozen_generation_in("recoverable.m4dproj", RECOVERABLE_ORPHAN);
            let (orphan_capture, _) = controlled_fixture_capture(
                "recoverable.m4dproj",
                &orphan,
                orphan.projection().clone(),
                orphan.parent_generation_id(),
                orphan.base_manual_generation_id(),
                orphan.forked_from(),
                ControlledRead::Normal,
            );
            let target = frozen_generation_in("divergent.m4dproj", DIVERGENT_INITIAL);
            let (stale_capture, opens) = controlled_fixture_capture(
                "divergent.m4dproj",
                &target,
                target.projection().clone(),
                None,
                None,
                target.forked_from(),
                ControlledRead::Normal,
            );
            actor
                .try_submit(ProjectStoreCommand::ManualSave {
                    request_id: request_id(1),
                    capture: orphan_capture,
                })
                .unwrap();
            actor
                .try_submit(ProjectStoreCommand::SaveAs {
                    request_id: request_id(2),
                    destination: destination.clone(),
                    source_generation: generation_id(RECOVERABLE_G2),
                    capture: stale_capture,
                })
                .unwrap();
            assert!(matches!(
                actor.recv_timeout(TIMEOUT),
                Some(ProjectStoreCompletion::ManualSaved {
                    request_id: actual,
                    result: Ok(receipt),
                }) if actual == request_id(1)
                    && receipt.current_generation_id() == generation_id(RECOVERABLE_ORPHAN)
            ));
            assert!(matches!(
                actor.recv_timeout(TIMEOUT),
                Some(ProjectStoreCompletion::SavedAs {
                    request_id: actual,
                    result: Err(ProjectStoreFault::StaleParent),
                }) if actual == request_id(2)
            ));
            assert_eq!(opens.load(Ordering::SeqCst), 0);
            assert!(!destination.as_path().exists());
            actor.try_submit(close_command(3)).unwrap();
            actor.recv_timeout(TIMEOUT).unwrap();
            actor.join().unwrap();
        }

        let source = TestProject::extracted_store("save-as-invalid", "recoverable.m4dproj");
        let destinations = TestDirectory::new("save-as-invalid-targets");
        let actor =
            EstablishedProjectActor::start(&source.store_path(), Default::default()).unwrap();
        let source_generation = generation_id(RECOVERABLE_G2);
        let target = frozen_generation_in("divergent.m4dproj", DIVERGENT_INITIAL);
        let source_project_id = frozen_generation_in("recoverable.m4dproj", RECOVERABLE_G2)
            .projection()
            .state()
            .project_id();

        let wrong_fork_destination = destinations.destination("wrong-fork.m4dproj");
        let (wrong_fork, wrong_fork_opens) = controlled_fixture_capture(
            "divergent.m4dproj",
            &target,
            target.projection().clone(),
            None,
            None,
            Some((source_project_id, generation_id(RECOVERABLE_G1))),
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::SaveAs {
                request_id: request_id(1),
                destination: wrong_fork_destination.clone(),
                source_generation,
                capture: wrong_fork,
            })
            .unwrap();
        assert_saved_as_fault(
            actor.recv_timeout(TIMEOUT).unwrap(),
            1,
            ProjectStoreFault::Corruption {
                stage: "initial_manual_capture",
            },
        );
        assert_eq!(wrong_fork_opens.load(Ordering::SeqCst), 0);

        let same_id_destination = destinations.destination("same-id.m4dproj");
        let same_id_projection = retarget_projection(
            &target,
            source_project_id,
            target.projection().state().dataset().clone(),
        );
        let (same_id, same_id_opens) = controlled_fixture_capture(
            "divergent.m4dproj",
            &target,
            same_id_projection,
            None,
            None,
            Some((source_project_id, source_generation)),
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::SaveAs {
                request_id: request_id(2),
                destination: same_id_destination.clone(),
                source_generation,
                capture: same_id,
            })
            .unwrap();
        assert_saved_as_fault(
            actor.recv_timeout(TIMEOUT).unwrap(),
            2,
            ProjectStoreFault::Corruption {
                stage: "save_as_project_identity",
            },
        );
        assert_eq!(same_id_opens.load(Ordering::SeqCst), 0);

        let wrong_science_destination = destinations.destination("wrong-science.m4dproj");
        let wrong_science = DatasetReference::new(
            ScientificContentId::parse(&format!(
                "{}{}",
                ScientificContentId::PREFIX,
                "42".repeat(32)
            ))
            .unwrap(),
            None,
            None,
            None,
        );
        let wrong_science_projection = retarget_projection(
            &target,
            target.projection().state().project_id(),
            wrong_science,
        );
        let (wrong_science, wrong_science_opens) = controlled_fixture_capture(
            "divergent.m4dproj",
            &target,
            wrong_science_projection,
            None,
            None,
            Some((source_project_id, source_generation)),
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::SaveAs {
                request_id: request_id(3),
                destination: wrong_science_destination.clone(),
                source_generation,
                capture: wrong_science,
            })
            .unwrap();
        assert_saved_as_fault(
            actor.recv_timeout(TIMEOUT).unwrap(),
            3,
            ProjectStoreFault::Corruption {
                stage: "save_as_scientific_identity",
            },
        );
        assert_eq!(wrong_science_opens.load(Ordering::SeqCst), 0);
        for destination in [
            wrong_fork_destination,
            same_id_destination,
            wrong_science_destination,
        ] {
            assert!(!destination.as_path().exists());
        }
        let source_root = LocalStoreRoot::open(source.path()).unwrap();
        let contender =
            ProjectStoreLeases::acquire(&source_root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(!contender.has_writer());
        actor.try_submit(close_command(4)).unwrap();
        actor.recv_timeout(TIMEOUT).unwrap();
        actor.join().unwrap();
    }

    #[test]
    fn rejected_save_as_keeps_the_source_session_authoritative() {
        let source = TestProject::extracted_store("save-as-failure", "recoverable.m4dproj");
        let destinations = TestDirectory::new("save-as-failure-targets");
        let actor =
            EstablishedProjectActor::start(&source.store_path(), Default::default()).unwrap();
        let source_before = file_tree(source.path());
        let target = frozen_generation_in("divergent.m4dproj", DIVERGENT_INITIAL);
        let source_generation = generation_id(RECOVERABLE_G2);

        let collision = destinations.destination("collision.m4dproj");
        fs::create_dir(collision.as_path()).unwrap();
        fs::write(collision.as_path().join("marker"), b"unrelated").unwrap();
        let (collision_capture, collision_opens) = controlled_fixture_capture(
            "divergent.m4dproj",
            &target,
            target.projection().clone(),
            None,
            None,
            target.forked_from(),
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::SaveAs {
                request_id: request_id(1),
                destination: collision.clone(),
                source_generation,
                capture: collision_capture,
            })
            .unwrap();
        assert_saved_as_fault(
            actor.recv_timeout(TIMEOUT).unwrap(),
            1,
            ProjectStoreFault::DestinationExists,
        );
        assert_eq!(collision_opens.load(Ordering::SeqCst), 0);
        assert_eq!(
            fs::read(collision.as_path().join("marker")).unwrap(),
            b"unrelated"
        );

        let rejected = destinations.destination("redundant-source.m4dproj");
        let artifact = target.projection().state().artifacts().first().unwrap();
        let ArtifactStorage::Direct { object } =
            target.bindings().get(&artifact.object().digest()).unwrap()
        else {
            panic!("the tiny Save As fixture must remain direct");
        };
        let redundant_opens = Arc::new(AtomicUsize::new(0));
        let redundant_source: Box<dyn ProjectObjectSource> = Box::new(ControlledSource {
            descriptor: artifact.object().clone(),
            bytes: Arc::from(fixture_extract(&fixture_object_member_in(
                "divergent.m4dproj",
                object.digest(),
            ))),
            opens: Arc::clone(&redundant_opens),
            behavior: ControlledRead::Normal,
        });
        let rejected_capture = ProjectCommitCapture::new(
            target.projection().clone(),
            None,
            None,
            target.forked_from(),
            vec![redundant_source],
        )
        .unwrap();
        actor
            .try_submit(ProjectStoreCommand::SaveAs {
                request_id: request_id(2),
                destination: rejected.clone(),
                source_generation,
                capture: rejected_capture,
            })
            .unwrap();
        assert_saved_as_fault(
            actor.recv_timeout(TIMEOUT).unwrap(),
            2,
            ProjectStoreFault::Corruption {
                stage: "generation_closure",
            },
        );
        assert_eq!(redundant_opens.load(Ordering::SeqCst), 0);
        assert!(!rejected.as_path().exists());
        assert_eq!(stage_count(destinations.path()), 0);

        for (request, destination) in [
            (
                3,
                ProjectStorePath::new(source.path().join("nested-save-as.m4dproj")).unwrap(),
            ),
            (
                4,
                ProjectStorePath::new(
                    source
                        .path()
                        .join("objects/sha256/deeply-nested-save-as.m4dproj"),
                )
                .unwrap(),
            ),
        ] {
            let capture = ProjectCommitCapture::new(
                target.projection().clone(),
                None,
                None,
                target.forked_from(),
                Vec::new(),
            )
            .unwrap();
            actor
                .try_submit(ProjectStoreCommand::SaveAs {
                    request_id: request_id(request),
                    destination: destination.clone(),
                    source_generation,
                    capture,
                })
                .unwrap();
            assert_saved_as_fault(
                actor.recv_timeout(TIMEOUT).unwrap(),
                request,
                ProjectStoreFault::Corruption {
                    stage: "destination_source_overlap",
                },
            );
            assert!(!destination.as_path().exists());
            assert_eq!(file_tree(source.path()), source_before);
        }
        assert_eq!(file_tree(source.path()), source_before);

        let source_root = LocalStoreRoot::open(source.path()).unwrap();
        let contender =
            ProjectStoreLeases::acquire(&source_root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(!contender.has_writer());
        let orphan = frozen_generation_in("recoverable.m4dproj", RECOVERABLE_ORPHAN);
        let (orphan_capture, _) = controlled_fixture_capture(
            "recoverable.m4dproj",
            &orphan,
            orphan.projection().clone(),
            orphan.parent_generation_id(),
            orphan.base_manual_generation_id(),
            orphan.forked_from(),
            ControlledRead::Normal,
        );
        actor
            .try_submit(ProjectStoreCommand::ManualSave {
                request_id: request_id(5),
                capture: orphan_capture,
            })
            .unwrap();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::ManualSaved {
                request_id: actual,
                result: Ok(receipt),
            }) if actual == request_id(5)
                && receipt.current_generation_id() == generation_id(RECOVERABLE_ORPHAN)
        ));
        actor.try_submit(close_command(6)).unwrap();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Closed {
                request_id: actual,
                result: Ok(()),
            }) if actual == request_id(6)
        ));
        actor.join().unwrap();
    }

    #[test]
    fn pin_and_unpin_are_correlated_graph_safe_and_reject_read_only_sessions() {
        let project = TestProject::extracted_store("actor-pins", "recoverable.m4dproj");
        let actor =
            EstablishedProjectActor::start(&project.store_path(), Default::default()).unwrap();
        let orphan = generation_id(RECOVERABLE_ORPHAN);
        actor
            .try_submit(ProjectStoreCommand::Pin {
                request_id: request_id(1),
                checkpoint_id: "review".to_owned(),
                generation_id: orphan,
            })
            .unwrap();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Pinned {
                request_id: actual,
                result: Ok(()),
            }) if actual == request_id(1)
        ));
        let root = LocalStoreRoot::open(project.path()).unwrap();
        assert!(
            crate::inspection::inspect_store_graph(&root, ProjectStoreLimits::default(), || false)
                .unwrap()
                .orphan_generation_ids()
                .is_empty()
        );

        actor
            .try_submit(ProjectStoreCommand::Unpin {
                request_id: request_id(2),
                checkpoint_id: "review".to_owned(),
            })
            .unwrap();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Unpinned {
                request_id: actual,
                result: Ok(()),
            }) if actual == request_id(2)
        ));
        assert_eq!(
            crate::inspection::inspect_store_graph(&root, ProjectStoreLimits::default(), || false)
                .unwrap()
                .orphan_generation_ids(),
            [orphan]
        );

        assert_eq!(
            actor.try_submit(ProjectStoreCommand::Pin {
                request_id: request_id(3),
                checkpoint_id: "Invalid".to_owned(),
                generation_id: orphan,
            }),
            Err(ProjectStoreFault::Corruption {
                stage: "checkpoint_id"
            })
        );
        actor
            .try_submit(ProjectStoreCommand::Pin {
                request_id: request_id(4),
                checkpoint_id: "missing".to_owned(),
                generation_id: ProjectGenerationId::from_digest(
                    mirante4d_identity::Sha256Digest::from_bytes([0x77; 32]),
                ),
            })
            .unwrap();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Pinned {
                request_id: actual,
                result: Err(ProjectStoreFault::Corruption {
                    stage: "pin_generation"
                }),
            }) if actual == request_id(4)
        ));
        actor.try_submit(close_command(5)).unwrap();
        actor.recv_timeout(TIMEOUT).unwrap();
        actor.join().unwrap();

        let contended = TestProject::extracted_store("actor-pins-read-only", "recoverable.m4dproj");
        let held_root = LocalStoreRoot::open(contended.path()).unwrap();
        let held_writer =
            ProjectStoreLeases::acquire(&held_root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(held_writer.has_writer());
        let read_only =
            EstablishedProjectActor::start(&contended.store_path(), Default::default()).unwrap();
        read_only
            .try_submit(ProjectStoreCommand::Pin {
                request_id: request_id(1),
                checkpoint_id: "review".to_owned(),
                generation_id: orphan,
            })
            .unwrap();
        assert!(matches!(
            read_only.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Pinned {
                result: Err(ProjectStoreFault::ReadOnly),
                ..
            })
        ));
        read_only
            .try_submit(ProjectStoreCommand::Unpin {
                request_id: request_id(2),
                checkpoint_id: "checkpoint-a".to_owned(),
            })
            .unwrap();
        assert!(matches!(
            read_only.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Unpinned {
                result: Err(ProjectStoreFault::ReadOnly),
                ..
            })
        ));
        read_only.try_submit(close_command(3)).unwrap();
        read_only.recv_timeout(TIMEOUT).unwrap();
        read_only.join().unwrap();
    }

    #[test]
    fn compaction_plan_is_correlated_cancellable_and_available_read_only() {
        fn assert_exact_plan(completion: ProjectStoreCompletion) {
            let candidates = match completion {
                ProjectStoreCompletion::CompactionPlanned {
                    request_id: actual,
                    result: Ok(candidates),
                } if actual == request_id(1) => candidates,
                other => panic!("unexpected PlanCompaction completion: {other:?}"),
            };
            assert_eq!(candidates.len(), 1);
            let candidate = &candidates[0];
            assert_eq!(candidate.generation_id(), generation_id(RECOVERABLE_ORPHAN));
            assert_eq!(candidate.generation_sequence(), 4);
            assert_eq!(candidate.revision_sequence(), 5);
            assert_eq!(candidate.origin(), "orphan_scan");
            assert_eq!(candidate.classification(), "manual_branch");
            assert_eq!(candidate.base_manual_generation_id(), None);
            assert_eq!(
                candidate.current_manual_generation_id(),
                Some(generation_id(RECOVERABLE_G2))
            );
            assert_eq!(candidate.artifact_count(), 2);
            assert_eq!(candidate.non_regenerable_artifact_count(), 1);
        }

        let writable = TestProject::extracted_store("actor-compaction-plan", "recoverable.m4dproj");
        let before = file_tree(writable.path());
        let actor =
            EstablishedProjectActor::start(&writable.store_path(), Default::default()).unwrap();
        actor
            .try_submit(ProjectStoreCommand::PlanCompaction {
                request_id: request_id(1),
            })
            .unwrap();
        assert_exact_plan(actor.recv_timeout(TIMEOUT).unwrap());
        assert_eq!(file_tree(writable.path()), before);
        actor.try_submit(close_command(2)).unwrap();
        actor.recv_timeout(TIMEOUT).unwrap();
        actor.join().unwrap();

        let contended =
            TestProject::extracted_store("actor-compaction-plan-read-only", "recoverable.m4dproj");
        let before = file_tree(contended.path());
        let held_root = LocalStoreRoot::open(contended.path()).unwrap();
        let held_writer =
            ProjectStoreLeases::acquire(&held_root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(held_writer.has_writer());
        let read_only =
            EstablishedProjectActor::start(&contended.store_path(), Default::default()).unwrap();
        read_only
            .try_submit(ProjectStoreCommand::PlanCompaction {
                request_id: request_id(1),
            })
            .unwrap();
        assert_exact_plan(read_only.recv_timeout(TIMEOUT).unwrap());
        assert_eq!(file_tree(contended.path()), before);
        read_only.try_submit(manual_command(2)).unwrap();
        assert!(matches!(
            read_only.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::ManualSaved {
                request_id: actual,
                result: Err(ProjectStoreFault::ReadOnly),
            }) if actual == request_id(2)
        ));
        assert_eq!(file_tree(contended.path()), before);
        read_only.try_submit(close_command(3)).unwrap();
        read_only.recv_timeout(TIMEOUT).unwrap();
        read_only.join().unwrap();
    }

    #[test]
    fn trash_is_correlated_cancellable_bounded_and_rejects_read_only() {
        let writable = TestProject::extracted_store("actor-trash", "recoverable.m4dproj");
        let selected = crate::trash::tests::install_zero_non_regenerable_orphan(writable.path());
        let blocker_root = LocalStoreRoot::open(writable.path()).unwrap();
        let blocker =
            ProjectStoreLeases::acquire(&blocker_root, ProjectOpenMode::ReadOnly).unwrap();
        let actor =
            EstablishedProjectActor::start(&writable.store_path(), Default::default()).unwrap();
        let before = file_tree(writable.path());

        assert_eq!(
            actor.try_submit(ProjectStoreCommand::Trash {
                request_id: request_id(1),
                generations: Vec::new(),
            }),
            Err(ProjectStoreFault::Corruption {
                stage: "trash_selection"
            })
        );
        assert_eq!(
            actor.try_submit(ProjectStoreCommand::Trash {
                request_id: request_id(1),
                generations: vec![selected, selected],
            }),
            Err(ProjectStoreFault::Corruption {
                stage: "trash_selection"
            })
        );
        assert_eq!(
            actor.try_submit(ProjectStoreCommand::Trash {
                request_id: request_id(1),
                generations: vec![selected; 65],
            }),
            Err(ProjectStoreFault::Capacity {
                stage: "trash_selection"
            })
        );

        actor
            .try_submit(ProjectStoreCommand::Trash {
                request_id: request_id(1),
                generations: vec![selected],
            })
            .unwrap();
        wait_until_active(&actor, 1);
        actor.try_submit(cancel_command(2, 1)).unwrap();
        assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 2);
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 1);
        assert_eq!(file_tree(writable.path()), before);

        drop(blocker);
        actor
            .try_submit(ProjectStoreCommand::Trash {
                request_id: request_id(3),
                generations: vec![selected],
            })
            .unwrap();
        let diagnostics = match actor.recv_timeout(TIMEOUT) {
            Some(ProjectStoreCompletion::Trashed {
                request_id: actual,
                result: Ok(diagnostics),
            }) if actual == request_id(3) => diagnostics,
            other => panic!("unexpected Trash completion: {other:?}"),
        };
        assert_eq!(diagnostics.queued_requests, 0);
        assert_eq!(diagnostics.queued_completions, 0);
        assert_eq!(diagnostics.active_transactions, 1);
        assert_eq!(diagnostics.open_file_descriptors, 3);
        assert_eq!(diagnostics.streamed_bytes, 0);
        assert_eq!(diagnostics.published_objects, 1);
        assert!(
            !crate::inspection::inspect_store_graph(
                &blocker_root,
                ProjectStoreLimits::default(),
                || false,
            )
            .unwrap()
            .generation_ids()
            .contains(&selected)
        );
        actor.try_submit(close_command(4)).unwrap();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Closed {
                request_id: actual,
                result: Ok(()),
            }) if actual == request_id(4)
        ));
        actor.join().unwrap();

        let contended =
            TestProject::extracted_store("actor-trash-read-only", "recoverable.m4dproj");
        let selected = crate::trash::tests::install_zero_non_regenerable_orphan(contended.path());
        let held_root = LocalStoreRoot::open(contended.path()).unwrap();
        let held_writer =
            ProjectStoreLeases::acquire(&held_root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(held_writer.has_writer());
        let read_only =
            EstablishedProjectActor::start(&contended.store_path(), Default::default()).unwrap();
        let before = file_tree(contended.path());
        read_only
            .try_submit(ProjectStoreCommand::Trash {
                request_id: request_id(1),
                generations: vec![selected],
            })
            .unwrap();
        assert!(matches!(
            read_only.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Trashed {
                request_id: actual,
                result: Err(ProjectStoreFault::ReadOnly),
            }) if actual == request_id(1)
        ));
        assert_eq!(file_tree(contended.path()), before);
        read_only.try_submit(close_command(2)).unwrap();
        read_only.recv_timeout(TIMEOUT).unwrap();
        read_only.join().unwrap();
    }

    #[test]
    fn maintenance_restore_loss_terminates_and_drains_exact_completions() {
        fn restore_loss_injector() -> Arc<GcTransitionInjector> {
            GcTransitionInjector::gated(GcTransitionTarget {
                transition: GcTransition::MaintenanceRestore,
                edge: TransitionEdge::Before,
                occurrence: 0,
            })
        }

        let project =
            TestProject::extracted_store("actor-trash-restore-loss", "recoverable.m4dproj");
        let selected = crate::trash::tests::install_zero_non_regenerable_orphan(project.path());
        let injector = restore_loss_injector();
        let actor = EstablishedProjectActor::start_with_gc_transition_injector(
            &project.store_path(),
            Default::default(),
            Arc::clone(&injector),
        )
        .unwrap();
        actor
            .try_submit(ProjectStoreCommand::Trash {
                request_id: request_id(1),
                generations: vec![selected],
            })
            .unwrap();
        injector.wait_until_parked(TIMEOUT);
        actor
            .try_submit(ProjectStoreCommand::FullVerify {
                request_id: request_id(2),
            })
            .unwrap();
        actor
            .try_submit(ProjectStoreCommand::PlanCompaction {
                request_id: request_id(3),
            })
            .unwrap();
        injector.release();

        let mut completed = BTreeSet::new();
        for _ in 0..3 {
            let completion = actor.recv_timeout(TIMEOUT).unwrap();
            completed.insert(completion.request_id());
            match completion {
                ProjectStoreCompletion::Trashed {
                    request_id: actual,
                    result: Err(ProjectStoreFault::CommitIndeterminate),
                } if actual == request_id(1) => {}
                ProjectStoreCompletion::Verified {
                    request_id: actual,
                    result: Err(ProjectStoreFault::Cancelled),
                } if actual == request_id(2) => {}
                ProjectStoreCompletion::CompactionPlanned {
                    request_id: actual,
                    result: Err(ProjectStoreFault::Cancelled),
                } if actual == request_id(3) => {}
                other => panic!("unexpected maintenance-loss completion: {other:?}"),
            }
        }
        assert_eq!(completed, [1, 2, 3].into_iter().map(request_id).collect());
        assert!(actor.has_exited());
        assert_eq!(
            actor.try_submit(close_command(4)),
            Err(ProjectStoreFault::Corruption {
                stage: "actor_closed"
            })
        );
        assert!(actor.recv_timeout(Duration::from_millis(10)).is_none());
        actor.join().unwrap();

        let root = LocalStoreRoot::open(project.path()).unwrap();
        let mut leases = acquire_writer_eventually(&root);
        assert!(leases.has_writer());
        let retry = crate::trash::trash_generations(
            &root,
            &mut leases,
            &[selected],
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(retry.published_objects, 0);
        assert_eq!(retry.streamed_bytes, 0);

        let closing =
            TestProject::extracted_store("actor-trash-restore-close", "recoverable.m4dproj");
        let selected = crate::trash::tests::install_zero_non_regenerable_orphan(closing.path());
        let injector = restore_loss_injector();
        let actor = EstablishedProjectActor::start_with_gc_transition_injector(
            &closing.store_path(),
            Default::default(),
            Arc::clone(&injector),
        )
        .unwrap();
        actor
            .try_submit(ProjectStoreCommand::Trash {
                request_id: request_id(1),
                generations: vec![selected],
            })
            .unwrap();
        injector.wait_until_parked(TIMEOUT);
        actor.try_submit(close_command(2)).unwrap();
        injector.release();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Trashed {
                request_id: actual,
                result: Err(ProjectStoreFault::CommitIndeterminate),
            }) if actual == request_id(1)
        ));
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Closed {
                request_id: actual,
                result: Ok(()),
            }) if actual == request_id(2)
        ));
        assert!(actor.recv_timeout(Duration::from_millis(10)).is_none());
        actor.join().unwrap();

        let suspended =
            TestProject::extracted_store("actor-trash-suspended", "recoverable.m4dproj");
        let selected = crate::trash::tests::install_zero_non_regenerable_orphan(suspended.path());
        let injector = GcTransitionInjector::failing(GcTransitionTarget {
            transition: GcTransition::TrashDirectorySync,
            edge: TransitionEdge::Before,
            occurrence: 0,
        });
        let actor = EstablishedProjectActor::start_with_gc_transition_injector(
            &suspended.store_path(),
            Default::default(),
            injector,
        )
        .unwrap();
        actor
            .try_submit(ProjectStoreCommand::Trash {
                request_id: request_id(1),
                generations: vec![selected],
            })
            .unwrap();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Trashed {
                request_id: actual,
                result: Err(ProjectStoreFault::CommitIndeterminate),
            }) if actual == request_id(1)
        ));
        assert!(!actor.has_exited());
        actor
            .try_submit(ProjectStoreCommand::FullVerify {
                request_id: request_id(2),
            })
            .unwrap();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Verified {
                request_id: actual,
                result: Ok(_),
            }) if actual == request_id(2)
        ));
        actor
            .try_submit(ProjectStoreCommand::Trash {
                request_id: request_id(3),
                generations: vec![selected],
            })
            .unwrap();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Trashed {
                request_id: actual,
                result: Err(ProjectStoreFault::CommitIndeterminate),
            }) if actual == request_id(3)
        ));
        actor.try_submit(close_command(4)).unwrap();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Closed {
                request_id: actual,
                result: Ok(()),
            }) if actual == request_id(4)
        ));
        actor.join().unwrap();
    }

    #[test]
    fn purge_is_correlated_cancellable_read_only_and_indeterminate() {
        #[derive(Clone, Copy)]
        enum Case {
            CorrelatedCancellation,
            ReadOnly,
            PostUnlinkIndeterminate,
            MaintenanceLossDrain,
        }

        for case in [
            Case::CorrelatedCancellation,
            Case::ReadOnly,
            Case::PostUnlinkIndeterminate,
            Case::MaintenanceLossDrain,
        ] {
            match case {
                Case::CorrelatedCancellation => {
                    let project = TestProject::extracted_store(
                        "actor-purge-correlation",
                        "recoverable.m4dproj",
                    );
                    let snapshot = install_purge_snapshot(&project);
                    let refs_before = file_tree(&project.path().join("refs"));
                    let objects_before = file_tree(&project.path().join("objects"));
                    let blocker_root = LocalStoreRoot::open(project.path()).unwrap();
                    let blocker =
                        ProjectStoreLeases::acquire(&blocker_root, ProjectOpenMode::ReadOnly)
                            .unwrap();
                    let actor =
                        EstablishedProjectActor::start(&project.store_path(), Default::default())
                            .unwrap();

                    actor.try_submit(purge_command(1)).unwrap();
                    wait_until_active(&actor, 1);
                    actor.try_submit(purge_command(2)).unwrap();
                    actor.try_submit(cancel_command(3, 2)).unwrap();
                    assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 2);
                    assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 3);
                    actor.try_submit(cancel_command(4, 1)).unwrap();
                    assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 4);
                    assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 1);
                    assert!(snapshot.trash_generation.exists());
                    assert!(snapshot.trash_object.exists());

                    drop(blocker);
                    actor.try_submit(purge_command(5)).unwrap();
                    let diagnostics = match actor.recv_timeout(TIMEOUT) {
                        Some(ProjectStoreCompletion::Purged {
                            request_id: actual,
                            result: Ok(diagnostics),
                        }) if actual == request_id(5) => diagnostics,
                        other => panic!("unexpected Purge completion: {other:?}"),
                    };
                    assert_eq!(diagnostics.queued_requests, 0);
                    assert_eq!(diagnostics.queued_completions, 0);
                    assert_eq!(diagnostics.active_transactions, 1);
                    assert_eq!(diagnostics.open_file_descriptors, 3);
                    assert_eq!(diagnostics.published_objects, 2);
                    assert_eq!(
                        diagnostics.streamed_bytes,
                        2 * (snapshot.generation_bytes + snapshot.object_bytes)
                    );
                    assert!(!snapshot.trash_generation.exists());
                    assert!(!snapshot.trash_object.exists());
                    assert!(file_tree(&project.path().join("trash")).is_empty());
                    assert_eq!(file_tree(&project.path().join("refs")), refs_before);
                    assert_eq!(file_tree(&project.path().join("objects")), objects_before);

                    actor.try_submit(purge_command(6)).unwrap();
                    assert!(matches!(
                        actor.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Purged {
                            request_id: actual,
                            result: Ok(ProjectStoreDiagnostics {
                                published_objects: 0,
                                streamed_bytes: 0,
                                ..
                            }),
                        }) if actual == request_id(6)
                    ));
                    actor.try_submit(close_command(7)).unwrap();
                    assert!(matches!(
                        actor.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Closed {
                            request_id: actual,
                            result: Ok(()),
                        }) if actual == request_id(7)
                    ));
                    actor.join().unwrap();
                }
                Case::ReadOnly => {
                    let project = TestProject::extracted_store(
                        "actor-purge-read-only",
                        "recoverable.m4dproj",
                    );
                    install_purge_snapshot(&project);
                    let before = file_tree(project.path());
                    let held_root = LocalStoreRoot::open(project.path()).unwrap();
                    let held_writer =
                        ProjectStoreLeases::acquire(&held_root, ProjectOpenMode::PreferWritable)
                            .unwrap();
                    assert!(held_writer.has_writer());
                    let actor =
                        EstablishedProjectActor::start(&project.store_path(), Default::default())
                            .unwrap();
                    actor.try_submit(purge_command(1)).unwrap();
                    assert!(matches!(
                        actor.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Purged {
                            request_id: actual,
                            result: Err(ProjectStoreFault::ReadOnly),
                        }) if actual == request_id(1)
                    ));
                    assert_eq!(file_tree(project.path()), before);
                    actor.try_submit(close_command(2)).unwrap();
                    assert!(matches!(
                        actor.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Closed {
                            request_id: actual,
                            result: Ok(()),
                        }) if actual == request_id(2)
                    ));
                    actor.join().unwrap();
                    drop(held_writer);
                }
                Case::PostUnlinkIndeterminate => {
                    let project = TestProject::extracted_store(
                        "actor-purge-indeterminate",
                        "recoverable.m4dproj",
                    );
                    let snapshot = install_purge_snapshot(&project);
                    let injector = GcTransitionInjector::failing(GcTransitionTarget {
                        transition: GcTransition::PurgeDirectorySync,
                        edge: TransitionEdge::Before,
                        occurrence: 0,
                    });
                    let actor = EstablishedProjectActor::start_with_gc_transition_injector(
                        &project.store_path(),
                        Default::default(),
                        injector,
                    )
                    .unwrap();
                    actor.try_submit(purge_command(1)).unwrap();
                    assert!(matches!(
                        actor.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Purged {
                            request_id: actual,
                            result: Err(ProjectStoreFault::CommitIndeterminate),
                        }) if actual == request_id(1)
                    ));
                    assert!(!actor.has_exited());
                    assert!(!snapshot.trash_object.exists());
                    assert!(snapshot.trash_generation.exists());

                    actor
                        .try_submit(ProjectStoreCommand::FullVerify {
                            request_id: request_id(2),
                        })
                        .unwrap();
                    assert!(matches!(
                        actor.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Verified {
                            request_id: actual,
                            result: Ok(_),
                        }) if actual == request_id(2)
                    ));
                    actor.try_submit(purge_command(3)).unwrap();
                    assert!(matches!(
                        actor.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Purged {
                            request_id: actual,
                            result: Err(ProjectStoreFault::CommitIndeterminate),
                        }) if actual == request_id(3)
                    ));
                    actor.try_submit(close_command(4)).unwrap();
                    actor.recv_timeout(TIMEOUT).unwrap();
                    actor.join().unwrap();

                    let retry =
                        EstablishedProjectActor::start(&project.store_path(), Default::default())
                            .unwrap();
                    retry.try_submit(purge_command(1)).unwrap();
                    let diagnostics = match retry.recv_timeout(TIMEOUT) {
                        Some(ProjectStoreCompletion::Purged {
                            request_id: actual,
                            result: Ok(diagnostics),
                        }) if actual == request_id(1) => diagnostics,
                        other => panic!("unexpected Purge reopen completion: {other:?}"),
                    };
                    assert_eq!(diagnostics.published_objects, 1);
                    assert_eq!(diagnostics.streamed_bytes, 2 * snapshot.generation_bytes);
                    retry.try_submit(purge_command(2)).unwrap();
                    assert!(matches!(
                        retry.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Purged {
                            request_id: actual,
                            result: Ok(ProjectStoreDiagnostics {
                                published_objects: 0,
                                streamed_bytes: 0,
                                ..
                            }),
                        }) if actual == request_id(2)
                    ));
                    retry.try_submit(close_command(3)).unwrap();
                    retry.recv_timeout(TIMEOUT).unwrap();
                    retry.join().unwrap();
                }
                Case::MaintenanceLossDrain => {
                    let project = TestProject::extracted_store(
                        "actor-purge-maintenance-loss",
                        "recoverable.m4dproj",
                    );
                    install_purge_snapshot(&project);
                    let injector = GcTransitionInjector::gated(GcTransitionTarget {
                        transition: GcTransition::MaintenanceRestore,
                        edge: TransitionEdge::Before,
                        occurrence: 0,
                    });
                    let actor = EstablishedProjectActor::start_with_gc_transition_injector(
                        &project.store_path(),
                        Default::default(),
                        Arc::clone(&injector),
                    )
                    .unwrap();
                    actor.try_submit(purge_command(1)).unwrap();
                    injector.wait_until_parked(TIMEOUT);
                    actor
                        .try_submit(ProjectStoreCommand::FullVerify {
                            request_id: request_id(2),
                        })
                        .unwrap();
                    actor.try_submit(close_command(3)).unwrap();
                    injector.release();

                    let mut completed = BTreeSet::new();
                    for _ in 0..3 {
                        let completion = actor.recv_timeout(TIMEOUT).unwrap();
                        completed.insert(completion.request_id());
                        match completion {
                            ProjectStoreCompletion::Purged {
                                request_id: actual,
                                result: Err(ProjectStoreFault::CommitIndeterminate),
                            } if actual == request_id(1) => {}
                            ProjectStoreCompletion::Verified {
                                request_id: actual,
                                result: Err(ProjectStoreFault::Cancelled),
                            } if actual == request_id(2) => {}
                            ProjectStoreCompletion::Closed {
                                request_id: actual,
                                result: Ok(()),
                            } if actual == request_id(3) => {}
                            other => panic!("unexpected Purge drain completion: {other:?}"),
                        }
                    }
                    assert_eq!(completed, [1, 2, 3].into_iter().map(request_id).collect());
                    assert!(actor.has_exited());
                    assert_eq!(
                        actor.try_submit(purge_command(4)),
                        Err(ProjectStoreFault::Corruption {
                            stage: "actor_closed"
                        })
                    );
                    actor.join().unwrap();

                    let retry =
                        EstablishedProjectActor::start(&project.store_path(), Default::default())
                            .unwrap();
                    retry.try_submit(purge_command(1)).unwrap();
                    assert!(matches!(
                        retry.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Purged {
                            request_id: actual,
                            result: Ok(ProjectStoreDiagnostics {
                                published_objects: 0,
                                ..
                            }),
                        }) if actual == request_id(1)
                    ));
                    retry.try_submit(close_command(2)).unwrap();
                    retry.recv_timeout(TIMEOUT).unwrap();
                    retry.join().unwrap();
                }
            }
        }
    }

    #[test]
    fn pin_and_unpin_fresh_process_kill_and_retry_matrix() {
        if let Some(role) = env::var_os(PIN_UNPIN_PROCESS_ROLE) {
            let root_path = PathBuf::from(env::var_os(PIN_UNPIN_PROCESS_ROOT).unwrap());
            let store_path = ProjectStorePath::new(root_path).unwrap();
            let operation =
                PinProcessOperation::parse(env::var(PIN_UNPIN_PROCESS_OPERATION).unwrap().as_str())
                    .unwrap();
            match role.to_str().unwrap() {
                "mutator" => {
                    let transition = GcTransition::parse(
                        env::var(PIN_UNPIN_PROCESS_TRANSITION).unwrap().as_str(),
                    )
                    .unwrap();
                    let allowed = match operation {
                        PinProcessOperation::Pin => GcTransition::PIN.contains(&transition),
                        PinProcessOperation::Unpin => GcTransition::UNPIN.contains(&transition),
                    };
                    assert!(allowed, "transition does not belong to {operation:?}");
                    let edge =
                        TransitionEdge::parse(env::var(PIN_UNPIN_PROCESS_EDGE).unwrap().as_str())
                            .unwrap();
                    let occurrence = env::var(PIN_UNPIN_PROCESS_OCCURRENCE)
                        .unwrap()
                        .parse::<usize>()
                        .unwrap();
                    let marker = PathBuf::from(env::var_os(PIN_UNPIN_PROCESS_MARKER).unwrap());
                    let injector = GcTransitionInjector::parking(
                        GcTransitionTarget {
                            transition,
                            edge,
                            occurrence,
                        },
                        marker,
                    );
                    let actor = EstablishedProjectActor::start_with_gc_transition_injector(
                        &store_path,
                        Default::default(),
                        injector,
                    )
                    .unwrap();
                    actor.try_submit(operation.command(1)).unwrap();
                    panic!(
                        "{operation:?} mutator escaped its transition hook: {:?}",
                        actor.recv_timeout(Duration::from_secs(30))
                    );
                }
                "recover" => {
                    let actor =
                        EstablishedProjectActor::start(&store_path, Default::default()).unwrap();
                    actor.try_submit(operation.command(41)).unwrap();
                    assert!(
                        operation.completion_succeeded(actor.recv_timeout(TIMEOUT).unwrap(), 41,)
                    );
                    actor.try_submit(operation.command(42)).unwrap();
                    assert!(
                        operation.completion_succeeded(actor.recv_timeout(TIMEOUT).unwrap(), 42,)
                    );
                    actor.try_submit(close_command(43)).unwrap();
                    assert!(matches!(
                        actor.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Closed {
                            request_id: actual,
                            result: Ok(()),
                        }) if actual == request_id(43)
                    ));
                    actor.join().unwrap();
                    return;
                }
                other => panic!("unexpected Pin/Unpin process role {other}"),
            }
        }

        assert_eq!(
            GcTransition::PIN.map(GcTransition::name),
            [
                "pin_stage_create",
                "pin_write",
                "pin_file_sync",
                "pin_replace",
                "pin_directory_sync",
            ]
        );
        assert_eq!(
            GcTransition::UNPIN.map(GcTransition::name),
            ["unpin_remove", "unpin_directory_sync"]
        );

        let mut cases = Vec::new();
        for (operation, transitions) in [
            (PinProcessOperation::Pin, GcTransition::PIN.as_slice()),
            (PinProcessOperation::Unpin, GcTransition::UNPIN.as_slice()),
        ] {
            for &transition in transitions {
                let occurrences = match transition {
                    GcTransition::PinDirectorySync => 2,
                    _ => 1,
                };
                for edge in [TransitionEdge::Before, TransitionEdge::After] {
                    for occurrence in 0..occurrences {
                        cases.push((operation, transition, edge, occurrence));
                    }
                }
            }
        }
        assert_eq!(cases.len(), 16);

        let markers = TestDirectory::new("pin-unpin-kill-markers");
        let mut killed = 0_usize;
        let mut recovered = 0_usize;
        let mut idempotent_second_retries = 0_usize;
        let mut recovery_sync_required_cases = 0_usize;
        let mut staging_cleanup_cases = 0_usize;
        for (case, (operation, transition, edge, occurrence)) in cases.into_iter().enumerate() {
            let project = TestProject::extracted_store(
                &format!("pin-unpin-kill-{case}"),
                "recoverable.m4dproj",
            );
            let limits = ProjectStoreLimits::default();
            let initial_root = LocalStoreRoot::open(project.path()).unwrap();
            let initial_pin = initial_root
                .read_pin_ref("checkpoint-a", limits, || false)
                .unwrap()
                .unwrap();
            assert_ne!(initial_pin.current(), generation_id(RECOVERABLE_ORPHAN));
            drop(initial_root);

            let mut unrelated_refs_before = file_tree(&project.path().join("refs"));
            unrelated_refs_before.remove(Path::new("pins/checkpoint-a"));
            let generations_before = file_tree(&project.path().join("generations"));
            let objects_before = file_tree(&project.path().join("objects"));
            let marker = markers.path().join(format!(
                "{case}-{}-{}-{}-{occurrence}",
                operation.name(),
                transition.name(),
                edge.name()
            ));

            let mut mutator = ChildGuard::new(
                Command::new(env::current_exe().unwrap())
                    .arg(PIN_UNPIN_PROCESS_TEST)
                    .arg("--exact")
                    .arg("--nocapture")
                    .env(PIN_UNPIN_PROCESS_ROLE, "mutator")
                    .env(PIN_UNPIN_PROCESS_ROOT, project.path())
                    .env(PIN_UNPIN_PROCESS_OPERATION, operation.name())
                    .env(PIN_UNPIN_PROCESS_TRANSITION, transition.name())
                    .env(PIN_UNPIN_PROCESS_EDGE, edge.name())
                    .env(PIN_UNPIN_PROCESS_OCCURRENCE, occurrence.to_string())
                    .env(PIN_UNPIN_PROCESS_MARKER, &marker)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let deadline = Instant::now() + TIMEOUT;
            while !marker.exists() {
                if let Some(status) = mutator.child_mut().try_wait().unwrap() {
                    panic!(
                        "{operation:?} mutator exited before {} {} occurrence {occurrence}: {status}",
                        transition.name(),
                        edge.name()
                    );
                }
                if Instant::now() >= deadline {
                    panic!(
                        "{operation:?} mutator did not reach {} {} occurrence {occurrence}",
                        transition.name(),
                        edge.name()
                    );
                }
                thread::sleep(Duration::from_millis(1));
            }
            let killed_status = mutator.kill_and_wait();
            assert_eq!(killed_status.signal(), Some(9));
            killed += 1;

            let expected_staging_residue = operation == PinProcessOperation::Pin
                && !(transition == GcTransition::PinStageCreate && edge == TransitionEdge::Before);
            let staging_residue_before_reopen = private_pin_staging_residue(project.path());
            assert_eq!(
                staging_residue_before_reopen,
                usize::from(expected_staging_residue)
            );
            staging_cleanup_cases += staging_residue_before_reopen;

            let mut recovery = ChildGuard::new(
                Command::new(env::current_exe().unwrap())
                    .arg(PIN_UNPIN_PROCESS_TEST)
                    .arg("--exact")
                    .arg("--nocapture")
                    .env(PIN_UNPIN_PROCESS_ROLE, "recover")
                    .env(PIN_UNPIN_PROCESS_ROOT, project.path())
                    .env(PIN_UNPIN_PROCESS_OPERATION, operation.name())
                    .stdout(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let recovery_status = recovery.wait_timeout(Duration::from_secs(10));
            assert!(
                recovery_status.success(),
                "fresh {operation:?} retry failed after {} {} occurrence {occurrence}",
                transition.name(),
                edge.name()
            );
            recovered += 1;
            idempotent_second_retries += 1;

            let recovery_synced_existing_state = match operation {
                PinProcessOperation::Pin => {
                    (transition == GcTransition::PinReplace && edge == TransitionEdge::After)
                        || transition == GcTransition::PinDirectorySync
                }
                PinProcessOperation::Unpin => {
                    (transition == GcTransition::UnpinRemove && edge == TransitionEdge::After)
                        || transition == GcTransition::UnpinDirectorySync
                }
            };
            recovery_sync_required_cases += usize::from(recovery_synced_existing_state);

            assert_eq!(private_pin_staging_residue(project.path()), 0);

            let root = LocalStoreRoot::open(project.path()).unwrap();
            let final_pin = root.read_pin_ref("checkpoint-a", limits, || false).unwrap();
            match operation {
                PinProcessOperation::Pin => assert_eq!(
                    final_pin.unwrap().current(),
                    generation_id(RECOVERABLE_ORPHAN)
                ),
                PinProcessOperation::Unpin => assert!(final_pin.is_none()),
            }
            let graph = crate::inspection::inspect_store_graph(&root, limits, || false).unwrap();
            if operation == PinProcessOperation::Pin {
                assert!(
                    !graph
                        .orphan_generation_ids()
                        .contains(&generation_id(RECOVERABLE_ORPHAN))
                );
            }
            let mut unrelated_refs_after = file_tree(&project.path().join("refs"));
            unrelated_refs_after.remove(Path::new("pins/checkpoint-a"));
            assert_eq!(unrelated_refs_after, unrelated_refs_before);
            assert_eq!(
                file_tree(&project.path().join("generations")),
                generations_before
            );
            assert_eq!(file_tree(&project.path().join("objects")), objects_before);
        }
        assert_eq!(killed, 16);
        assert_eq!(recovered, 16);
        assert_eq!(idempotent_second_retries, 16);
        assert_eq!(recovery_sync_required_cases, 8);
        assert_eq!(staging_cleanup_cases, 11);
        eprintln!(
            "M4D_PIN_UNPIN_PROCESS_MATRIX_V1 cases=16 killed=16 fresh_reopens=16 retry_completed=16 idempotent_second_retries=16 recovery_sync_required_cases=8 staging_cleanup_cases=11 staging_residue_after_reopen=0 process_crash_only=true power_loss_simulated=false durability_claim=false"
        );
    }

    #[test]
    fn trash_fresh_process_kill_and_retry_matrix() {
        if let Some(role) = env::var_os(TRASH_PROCESS_ROLE) {
            let root_path = PathBuf::from(env::var_os(TRASH_PROCESS_ROOT).unwrap());
            let store_path = ProjectStorePath::new(root_path.clone()).unwrap();
            let selected =
                ProjectGenerationId::parse(env::var(TRASH_PROCESS_GENERATION).unwrap().as_str())
                    .unwrap();
            match role.to_str().unwrap() {
                "mutator" => {
                    let transition =
                        GcTransition::parse(env::var(TRASH_PROCESS_TRANSITION).unwrap().as_str())
                            .unwrap();
                    let edge =
                        TransitionEdge::parse(env::var(TRASH_PROCESS_EDGE).unwrap().as_str())
                            .unwrap();
                    let occurrence = env::var(TRASH_PROCESS_OCCURRENCE)
                        .unwrap()
                        .parse::<usize>()
                        .unwrap();
                    let marker = PathBuf::from(env::var_os(TRASH_PROCESS_MARKER).unwrap());
                    let injector = GcTransitionInjector::parking(
                        GcTransitionTarget {
                            transition,
                            edge,
                            occurrence,
                        },
                        marker,
                    );
                    let actor = EstablishedProjectActor::start_with_gc_transition_injector(
                        &store_path,
                        Default::default(),
                        injector,
                    )
                    .unwrap();
                    actor
                        .try_submit(ProjectStoreCommand::Trash {
                            request_id: request_id(1),
                            generations: vec![selected],
                        })
                        .unwrap();
                    panic!(
                        "mutator escaped its transition hook: {:?}",
                        actor.recv_timeout(Duration::from_secs(30))
                    );
                }
                "recover" => {
                    let actor =
                        EstablishedProjectActor::start(&store_path, Default::default()).unwrap();
                    actor
                        .try_submit(ProjectStoreCommand::Trash {
                            request_id: request_id(1),
                            generations: vec![selected],
                        })
                        .unwrap();
                    assert!(matches!(
                        actor.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Trashed {
                            request_id: actual,
                            result: Ok(_),
                        }) if actual == request_id(1)
                    ));
                    actor.try_submit(close_command(2)).unwrap();
                    assert!(matches!(
                        actor.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Closed {
                            request_id: actual,
                            result: Ok(()),
                        }) if actual == request_id(2)
                    ));
                    actor.join().unwrap();

                    let retry =
                        EstablishedProjectActor::start(&store_path, Default::default()).unwrap();
                    retry
                        .try_submit(ProjectStoreCommand::Trash {
                            request_id: request_id(1),
                            generations: vec![selected],
                        })
                        .unwrap();
                    assert!(matches!(
                        retry.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Trashed {
                            request_id: actual,
                            result: Ok(ProjectStoreDiagnostics {
                                published_objects: 0,
                                streamed_bytes: 0,
                                ..
                            }),
                        }) if actual == request_id(1)
                    ));
                    retry.try_submit(close_command(2)).unwrap();
                    assert!(matches!(
                        retry.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Closed {
                            request_id: actual,
                            result: Ok(()),
                        }) if actual == request_id(2)
                    ));
                    retry.join().unwrap();
                    return;
                }
                other => panic!("unexpected Trash process role {other}"),
            }
        }

        let markers = TestDirectory::new("trash-kill-markers");
        let mut killed = 0_usize;
        let mut recovered = 0_usize;
        let mut cases = Vec::new();
        for transition in GcTransition::ALL {
            let occurrences = match transition {
                GcTransition::TrashDirectoryCreate => 4,
                GcTransition::TrashDirectorySync => 5,
                _ => 1,
            };
            for edge in [TransitionEdge::Before, TransitionEdge::After] {
                for occurrence in 0..occurrences {
                    cases.push((transition, edge, occurrence));
                }
            }
        }
        assert_eq!(cases.len(), 34);
        for (case, (transition, edge, occurrence)) in cases.into_iter().enumerate() {
            let project =
                TestProject::extracted_store(&format!("trash-kill-{case}"), "recoverable.m4dproj");
            let selected = crate::trash::tests::install_zero_non_regenerable_orphan(project.path());
            let active = crate::trash::tests::active_generation_file(project.path(), selected);
            let trash = crate::trash::tests::trash_generation_file(project.path(), selected);
            let selected_bytes = fs::read(&active).unwrap();
            if matches!(
                transition,
                GcTransition::TrashCollisionFileSync | GcTransition::ActiveDeduplicateRemove
            ) {
                let root = LocalStoreRoot::open(project.path()).unwrap();
                let mut leases =
                    ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
                crate::trash::trash_generations(
                    &root,
                    &mut leases,
                    &[selected],
                    ProjectStoreLimits::default(),
                    || false,
                )
                .unwrap();
                fs::create_dir_all(active.parent().unwrap()).unwrap();
                fs::write(&active, &selected_bytes).unwrap();
            }
            let anonymous = crate::trash::tests::install_anonymous_object(project.path());
            let anonymous_bytes = fs::read(&anonymous).unwrap();
            let refs_before = file_tree(&project.path().join("refs"));
            let objects_before = file_tree(&project.path().join("objects"));
            let marker = markers.path().join(format!(
                "{case}-{}-{}-{occurrence}",
                transition.name(),
                edge.name()
            ));
            let mut mutator = ChildGuard::new(
                Command::new(env::current_exe().unwrap())
                    .arg(TRASH_PROCESS_TEST)
                    .arg("--exact")
                    .arg("--nocapture")
                    .env(TRASH_PROCESS_ROLE, "mutator")
                    .env(TRASH_PROCESS_ROOT, project.path())
                    .env(TRASH_PROCESS_GENERATION, selected.to_string())
                    .env(TRASH_PROCESS_TRANSITION, transition.name())
                    .env(TRASH_PROCESS_EDGE, edge.name())
                    .env(TRASH_PROCESS_OCCURRENCE, occurrence.to_string())
                    .env(TRASH_PROCESS_MARKER, &marker)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let deadline = Instant::now() + TIMEOUT;
            while !marker.exists() {
                if let Some(status) = mutator.child_mut().try_wait().unwrap() {
                    panic!(
                        "mutator exited before {} {} occurrence {occurrence}: {status}",
                        transition.name(),
                        edge.name()
                    );
                }
                if Instant::now() >= deadline {
                    panic!(
                        "mutator did not reach {} {} occurrence {occurrence}",
                        transition.name(),
                        edge.name()
                    );
                }
                thread::sleep(Duration::from_millis(1));
            }
            let killed_status = mutator.kill_and_wait();
            assert_eq!(killed_status.signal(), Some(9));
            killed += 1;

            let mut recovery = ChildGuard::new(
                Command::new(env::current_exe().unwrap())
                    .arg(TRASH_PROCESS_TEST)
                    .arg("--exact")
                    .arg("--nocapture")
                    .env(TRASH_PROCESS_ROLE, "recover")
                    .env(TRASH_PROCESS_ROOT, project.path())
                    .env(TRASH_PROCESS_GENERATION, selected.to_string())
                    .stdout(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let recovery_status = recovery.wait_timeout(Duration::from_secs(10));
            assert!(
                recovery_status.success(),
                "fresh recovery failed after {} {} occurrence {occurrence}",
                transition.name(),
                edge.name()
            );
            recovered += 1;

            let root = LocalStoreRoot::open(project.path()).unwrap();
            assert!(
                !crate::inspection::inspect_store_graph(
                    &root,
                    ProjectStoreLimits::default(),
                    || false,
                )
                .unwrap()
                .generation_ids()
                .contains(&selected)
            );
            assert!(!active.exists());
            assert_eq!(fs::read(&trash).unwrap(), selected_bytes);
            assert_eq!(file_tree(&project.path().join("refs")), refs_before);
            assert_eq!(file_tree(&project.path().join("objects")), objects_before);
            assert_eq!(fs::read(&anonymous).unwrap(), anonymous_bytes);
        }
        assert_eq!(killed, 34);
        assert_eq!(recovered, 34);
        eprintln!(
            "M4D_TRASH_PROCESS_MATRIX_V1 cases=34 killed=34 fresh_reopens=34 retry_completed=34 zero_mutation_sync_retries=34 process_crash_only=true power_loss_simulated=false durability_claim=false"
        );
    }

    #[test]
    fn purge_fresh_process_kill_and_retry_matrix() {
        if let Some(role) = env::var_os(PURGE_PROCESS_ROLE) {
            let root_path = PathBuf::from(env::var_os(PURGE_PROCESS_ROOT).unwrap());
            let store_path = ProjectStorePath::new(root_path).unwrap();
            match role.to_str().unwrap() {
                "mutator" => {
                    let transition_name = env::var(PURGE_PROCESS_TRANSITION).unwrap();
                    let transition = GcTransition::PURGE
                        .into_iter()
                        .find(|candidate| candidate.name() == transition_name)
                        .unwrap();
                    let edge =
                        TransitionEdge::parse(env::var(PURGE_PROCESS_EDGE).unwrap().as_str())
                            .unwrap();
                    let occurrence = env::var(PURGE_PROCESS_OCCURRENCE)
                        .unwrap()
                        .parse::<usize>()
                        .unwrap();
                    let marker = PathBuf::from(env::var_os(PURGE_PROCESS_MARKER).unwrap());
                    let injector = GcTransitionInjector::parking(
                        GcTransitionTarget {
                            transition,
                            edge,
                            occurrence,
                        },
                        marker,
                    );
                    let actor = EstablishedProjectActor::start_with_gc_transition_injector(
                        &store_path,
                        Default::default(),
                        injector,
                    )
                    .unwrap();
                    actor.try_submit(purge_command(1)).unwrap();
                    panic!(
                        "Purge mutator escaped its transition hook: {:?}",
                        actor.recv_timeout(Duration::from_secs(30))
                    );
                }
                "recover" => {
                    let actor =
                        EstablishedProjectActor::start(&store_path, Default::default()).unwrap();
                    actor.try_submit(purge_command(1)).unwrap();
                    assert!(matches!(
                        actor.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Purged {
                            request_id: actual,
                            result: Ok(_),
                        }) if actual == request_id(1)
                    ));
                    actor.try_submit(close_command(2)).unwrap();
                    assert!(matches!(
                        actor.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Closed {
                            request_id: actual,
                            result: Ok(()),
                        }) if actual == request_id(2)
                    ));
                    actor.join().unwrap();

                    let retry =
                        EstablishedProjectActor::start(&store_path, Default::default()).unwrap();
                    retry.try_submit(purge_command(1)).unwrap();
                    assert!(matches!(
                        retry.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Purged {
                            request_id: actual,
                            result: Ok(ProjectStoreDiagnostics {
                                published_objects: 0,
                                streamed_bytes: 0,
                                ..
                            }),
                        }) if actual == request_id(1)
                    ));
                    retry.try_submit(close_command(2)).unwrap();
                    assert!(matches!(
                        retry.recv_timeout(TIMEOUT),
                        Some(ProjectStoreCompletion::Closed {
                            request_id: actual,
                            result: Ok(()),
                        }) if actual == request_id(2)
                    ));
                    retry.join().unwrap();
                    return;
                }
                other => panic!("unexpected Purge process role {other}"),
            }
        }

        assert_eq!(
            GcTransition::PURGE.map(GcTransition::name),
            [
                "gc_maintenance_upgrade",
                "purge_remove",
                "purge_directory_sync",
                "gc_maintenance_restore",
            ]
        );
        let markers = TestDirectory::new("purge-kill-markers");
        let mut cases = Vec::new();
        for transition in GcTransition::PURGE {
            let occurrences = match transition {
                GcTransition::MaintenanceUpgrade | GcTransition::MaintenanceRestore => 1,
                GcTransition::PurgeRemove => 2,
                GcTransition::PurgeDirectorySync => 4,
                _ => panic!("the Purge transition inventory must stay separate from Trash"),
            };
            for edge in [TransitionEdge::Before, TransitionEdge::After] {
                for occurrence in 0..occurrences {
                    cases.push((transition, edge, occurrence));
                }
            }
        }
        assert_eq!(cases.len(), 16);

        let mut killed = 0_usize;
        let mut recovered = 0_usize;
        for (case, (transition, edge, occurrence)) in cases.into_iter().enumerate() {
            let project =
                TestProject::extracted_store(&format!("purge-kill-{case}"), "recoverable.m4dproj");
            let snapshot = install_purge_snapshot(&project);
            let active_generation =
                crate::trash::tests::active_generation_file(project.path(), snapshot.generation_id);
            assert!(!active_generation.exists());
            assert!(snapshot.trash_generation.exists());
            assert!(snapshot.trash_object.exists());
            let anonymous = crate::trash::tests::install_anonymous_object(project.path());
            let anonymous_bytes = fs::read(&anonymous).unwrap();
            let refs_before = file_tree(&project.path().join("refs"));
            let objects_before = file_tree(&project.path().join("objects"));
            let marker = markers.path().join(format!(
                "{case}-{}-{}-{occurrence}",
                transition.name(),
                edge.name()
            ));
            let mut mutator = ChildGuard::new(
                Command::new(env::current_exe().unwrap())
                    .arg(PURGE_PROCESS_TEST)
                    .arg("--exact")
                    .arg("--nocapture")
                    .env(PURGE_PROCESS_ROLE, "mutator")
                    .env(PURGE_PROCESS_ROOT, project.path())
                    .env(PURGE_PROCESS_TRANSITION, transition.name())
                    .env(PURGE_PROCESS_EDGE, edge.name())
                    .env(PURGE_PROCESS_OCCURRENCE, occurrence.to_string())
                    .env(PURGE_PROCESS_MARKER, &marker)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let deadline = Instant::now() + TIMEOUT;
            while !marker.exists() {
                if let Some(status) = mutator.child_mut().try_wait().unwrap() {
                    panic!(
                        "Purge mutator exited before {} {} occurrence {occurrence}: {status}",
                        transition.name(),
                        edge.name()
                    );
                }
                if Instant::now() >= deadline {
                    panic!(
                        "Purge mutator did not reach {} {} occurrence {occurrence}",
                        transition.name(),
                        edge.name()
                    );
                }
                thread::sleep(Duration::from_millis(1));
            }
            let killed_status = mutator.kill_and_wait();
            assert_eq!(killed_status.signal(), Some(9));
            killed += 1;

            let mut recovery = ChildGuard::new(
                Command::new(env::current_exe().unwrap())
                    .arg(PURGE_PROCESS_TEST)
                    .arg("--exact")
                    .arg("--nocapture")
                    .env(PURGE_PROCESS_ROLE, "recover")
                    .env(PURGE_PROCESS_ROOT, project.path())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let recovery_status = recovery.wait_timeout(Duration::from_secs(10));
            assert!(
                recovery_status.success(),
                "fresh Purge recovery failed after {} {} occurrence {occurrence}",
                transition.name(),
                edge.name()
            );
            recovered += 1;

            assert!(!snapshot.trash_generation.exists());
            assert!(!snapshot.trash_object.exists());
            assert!(file_tree(&project.path().join("trash")).is_empty());
            assert_eq!(file_tree(&project.path().join("refs")), refs_before);
            assert_eq!(file_tree(&project.path().join("objects")), objects_before);
            assert_eq!(fs::read(&anonymous).unwrap(), anonymous_bytes);
        }
        assert_eq!(killed, 16);
        assert_eq!(recovered, 16);
        eprintln!(
            "M4D_PURGE_PROCESS_MATRIX_V1 cases=16 killed=16 fresh_reopens=16 retry_completed=16 zero_removal_sync_retries=16 process_crash_only=true power_loss_simulated=false durability_claim=false"
        );
    }

    #[test]
    fn full_verify_is_correlated_cancellable_and_available_read_only() {
        let writable = TestProject::extracted("actor-full-verify");
        let actor =
            EstablishedProjectActor::start(&writable.store_path(), Default::default()).unwrap();
        actor
            .try_submit(ProjectStoreCommand::FullVerify {
                request_id: request_id(1),
            })
            .unwrap();
        let writable_diagnostics = match actor.recv_timeout(TIMEOUT) {
            Some(ProjectStoreCompletion::Verified {
                request_id: actual,
                result: Ok(diagnostics),
            }) if actual == request_id(1) => diagnostics,
            other => panic!("unexpected writable FullVerify completion: {other:?}"),
        };
        assert_eq!(writable_diagnostics.active_transactions, 1);
        assert_eq!(writable_diagnostics.open_file_descriptors, 3);
        assert_eq!(writable_diagnostics.published_objects, 0);
        actor.try_submit(close_command(2)).unwrap();
        actor.recv_timeout(TIMEOUT).unwrap();
        actor.join().unwrap();

        let contended = TestProject::extracted("actor-full-verify-read-only");
        let held_root = LocalStoreRoot::open(contended.path()).unwrap();
        let held_writer =
            ProjectStoreLeases::acquire(&held_root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(held_writer.has_writer());
        let read_only =
            EstablishedProjectActor::start(&contended.store_path(), Default::default()).unwrap();
        read_only
            .try_submit(ProjectStoreCommand::FullVerify {
                request_id: request_id(1),
            })
            .unwrap();
        let read_only_diagnostics = match read_only.recv_timeout(TIMEOUT) {
            Some(ProjectStoreCompletion::Verified {
                request_id: actual,
                result: Ok(diagnostics),
            }) if actual == request_id(1) => diagnostics,
            other => panic!("unexpected read-only FullVerify completion: {other:?}"),
        };
        assert_eq!(read_only_diagnostics.active_transactions, 1);
        assert_eq!(read_only_diagnostics.open_file_descriptors, 2);
        assert_eq!(read_only_diagnostics.published_objects, 0);
        read_only.try_submit(close_command(2)).unwrap();
        read_only.recv_timeout(TIMEOUT).unwrap();
        read_only.join().unwrap();

        let cancelled = TestProject::extracted("actor-full-verify-cancelled");
        let actor =
            EstablishedProjectActor::start(&cancelled.store_path(), Default::default()).unwrap();
        let (capture, gate) = gated_manual_capture();
        actor
            .try_submit(ProjectStoreCommand::ManualSave {
                request_id: request_id(1),
                capture,
            })
            .unwrap();
        gate.wait_started();
        actor
            .try_submit(ProjectStoreCommand::FullVerify {
                request_id: request_id(2),
            })
            .unwrap();
        actor.try_submit(cancel_command(3, 2)).unwrap();
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 2);
        assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 3);
        actor.try_submit(cancel_command(4, 1)).unwrap();
        assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 4);
        gate.release();
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 1);
        actor.try_submit(close_command(5)).unwrap();
        actor.recv_timeout(TIMEOUT).unwrap();
        actor.join().unwrap();
    }

    #[test]
    fn queued_autosaves_coalesce_and_completion_obligations_stay_bounded() {
        let project = TestProject::extracted("coalesce-bound");
        let actor =
            EstablishedProjectActor::start(&project.store_path(), Default::default()).unwrap();
        let (capture, gate) = gated_manual_capture();
        actor
            .try_submit(ProjectStoreCommand::ManualSave {
                request_id: request_id(1),
                capture,
            })
            .unwrap();
        gate.wait_started();

        actor.try_submit(autosave_command(2)).unwrap();
        actor.try_submit(autosave_command(3)).unwrap();
        actor.try_submit(cancel_command(4, 3)).unwrap();
        actor.try_submit(autosave_command(5)).unwrap();
        actor.try_submit(cancel_command(6, 5)).unwrap();
        actor.try_submit(autosave_command(7)).unwrap();
        actor.try_submit(cancel_command(8, 7)).unwrap();
        actor
            .try_submit(ProjectStoreCommand::Pin {
                request_id: request_id(9),
                checkpoint_id: "closing-pin".to_owned(),
                generation_id: generation_id(STALE_MANUAL),
            })
            .unwrap();
        actor
            .try_submit(ProjectStoreCommand::Unpin {
                request_id: request_id(10),
                checkpoint_id: "checkpoint-a".to_owned(),
            })
            .unwrap();
        assert_eq!(
            actor.try_submit(close_command(11)),
            Err(ProjectStoreFault::QueueFull {
                queue: "completion"
            })
        );
        for expected in [2, 3, 4, 5, 6, 7, 8] {
            let completion = actor.recv_timeout(TIMEOUT).unwrap();
            assert_eq!(completion.request_id(), request_id(expected));
        }

        actor.try_submit(close_command(11)).unwrap();
        assert!(matches!(
            actor.try_submit(manual_command(12)),
            Err(ProjectStoreFault::Corruption {
                stage: "actor_closed"
            })
        ));
        gate.release();

        let mut completed = BTreeSet::new();
        for _ in 0..4 {
            let completion = actor.recv_timeout(TIMEOUT).unwrap();
            completed.insert(completion.request_id());
            match completion {
                ProjectStoreCompletion::ManualSaved {
                    result: Err(ProjectStoreFault::Cancelled),
                    ..
                }
                | ProjectStoreCompletion::Autosaved {
                    result: Err(ProjectStoreFault::Cancelled),
                    ..
                }
                | ProjectStoreCompletion::Pinned {
                    result: Err(ProjectStoreFault::Cancelled),
                    ..
                }
                | ProjectStoreCompletion::Unpinned {
                    result: Err(ProjectStoreFault::Cancelled),
                    ..
                }
                | ProjectStoreCompletion::Closed { result: Ok(()), .. } => {}
                other => panic!("unexpected close-barrier completion: {other:?}"),
            }
        }
        assert_eq!(
            completed,
            [1, 9, 10, 11].into_iter().map(request_id).collect()
        );
        actor.join().unwrap();
    }

    #[test]
    fn active_and_queued_requests_cancel_immediately_with_exact_correlation() {
        let project = TestProject::extracted("cancel");
        let actor =
            EstablishedProjectActor::start(&project.store_path(), Default::default()).unwrap();
        let (capture, gate) = gated_manual_capture();
        actor
            .try_submit(ProjectStoreCommand::ManualSave {
                request_id: request_id(1),
                capture,
            })
            .unwrap();
        gate.wait_started();
        actor
            .try_submit(ProjectStoreCommand::Pin {
                request_id: request_id(2),
                checkpoint_id: "queued-pin".to_owned(),
                generation_id: generation_id(STALE_MANUAL),
            })
            .unwrap();
        actor.try_submit(cancel_command(3, 2)).unwrap();
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 2);
        assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 3);

        actor
            .try_submit(ProjectStoreCommand::Unpin {
                request_id: request_id(4),
                checkpoint_id: "checkpoint-a".to_owned(),
            })
            .unwrap();
        actor.try_submit(cancel_command(5, 4)).unwrap();
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 4);
        assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 5);

        actor
            .try_submit(ProjectStoreCommand::InspectRecovery {
                request_id: request_id(6),
            })
            .unwrap();
        actor.try_submit(cancel_command(7, 6)).unwrap();
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 6);
        assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 7);

        actor
            .try_submit(ProjectStoreCommand::OpenRecovery {
                request_id: request_id(8),
                generation_id: generation_id(STALE_AUTOSAVE),
            })
            .unwrap();
        actor.try_submit(cancel_command(9, 8)).unwrap();
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 8);
        assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 9);

        actor
            .try_submit(ProjectStoreCommand::PlanCompaction {
                request_id: request_id(10),
            })
            .unwrap();
        actor.try_submit(cancel_command(11, 10)).unwrap();
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 10);
        assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 11);

        actor
            .try_submit(ProjectStoreCommand::Trash {
                request_id: request_id(12),
                generations: vec![generation_id(STALE_MANUAL)],
            })
            .unwrap();
        actor.try_submit(cancel_command(13, 12)).unwrap();
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 12);
        assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 13);

        actor.try_submit(cancel_command(14, 1)).unwrap();
        assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 14);
        gate.release();
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 1);

        actor.try_submit(close_command(15)).unwrap();
        assert!(matches!(
            actor.recv_timeout(TIMEOUT),
            Some(ProjectStoreCompletion::Closed { result: Ok(()), .. })
        ));
        actor.join().unwrap();
    }

    #[test]
    fn the_eight_request_queue_rejects_a_ninth_queued_save() {
        let project = TestProject::extracted("request-bound");
        let actor =
            EstablishedProjectActor::start(&project.store_path(), Default::default()).unwrap();
        let (capture, gate) = gated_manual_capture();
        actor
            .try_submit(ProjectStoreCommand::ManualSave {
                request_id: request_id(1),
                capture,
            })
            .unwrap();
        gate.wait_started();
        actor.try_submit(autosave_command(2)).unwrap();
        for id in 3..=9 {
            actor.try_submit(manual_command(id)).unwrap();
        }
        actor.try_submit(autosave_command(10)).unwrap();
        assert_eq!(
            actor.try_submit(manual_command(11)),
            Err(ProjectStoreFault::QueueFull { queue: "request" })
        );
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 2);
        actor.try_submit(cancel_command(11, 3)).unwrap();
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 3);
        assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 11);
        actor.try_submit(cancel_command(12, 4)).unwrap();
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 4);
        assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 12);
        actor.try_submit(close_command(13)).unwrap();
        gate.release();
        for _ in 0..8 {
            actor.recv_timeout(TIMEOUT).unwrap();
        }
        actor.join().unwrap();
    }

    #[test]
    fn drop_is_nonblocking_and_joined_shutdown_releases_session_resources() {
        let project = TestProject::extracted("drop");
        let actor =
            EstablishedProjectActor::start(&project.store_path(), Default::default()).unwrap();
        let (capture, gate) = gated_manual_capture();
        actor
            .try_submit(ProjectStoreCommand::ManualSave {
                request_id: request_id(1),
                capture,
            })
            .unwrap();
        gate.wait_started();
        let started = std::time::Instant::now();
        drop(actor);
        assert!(started.elapsed() < Duration::from_secs(1));
        gate.release();

        let root = LocalStoreRoot::open(project.path()).unwrap();
        let deadline = std::time::Instant::now() + TIMEOUT;
        loop {
            let leases =
                ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
            if leases.has_writer() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "detached worker retained its lease"
            );
            thread::yield_now();
        }

        let joined_project = TestProject::extracted("joined");
        let joined =
            EstablishedProjectActor::start(&joined_project.store_path(), Default::default())
                .unwrap();
        joined.join().unwrap();
        let joined_root = LocalStoreRoot::open(joined_project.path()).unwrap();
        let leases =
            ProjectStoreLeases::acquire(&joined_root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(leases.has_writer());
    }

    fn manual_command(id: u64) -> ProjectStoreCommand {
        let frozen = frozen_generation(STALE_MANUAL);
        ProjectStoreCommand::ManualSave {
            request_id: request_id(id),
            capture: frozen_capture(
                &frozen,
                frozen.projection().clone(),
                Some(generation_id(STALE_MANUAL)),
                None,
                None,
            ),
        }
    }

    fn autosave_command(id: u64) -> ProjectStoreCommand {
        let frozen = frozen_generation(STALE_AUTOSAVE);
        ProjectStoreCommand::Autosave {
            request_id: request_id(id),
            destination: None,
            capture: frozen_capture(
                &frozen,
                frozen.projection().clone(),
                Some(generation_id(STALE_AUTOSAVE)),
                Some(generation_id(STALE_MANUAL)),
                None,
            ),
        }
    }

    fn close_command(id: u64) -> ProjectStoreCommand {
        ProjectStoreCommand::Close {
            request_id: request_id(id),
        }
    }

    fn cancel_command(id: u64, target: u64) -> ProjectStoreCommand {
        ProjectStoreCommand::Cancel {
            request_id: request_id(id),
            target_request_id: request_id(target),
        }
    }

    fn purge_command(id: u64) -> ProjectStoreCommand {
        ProjectStoreCommand::Purge {
            request_id: request_id(id),
        }
    }

    struct PurgeSnapshot {
        generation_id: ProjectGenerationId,
        generation_bytes: u64,
        trash_generation: PathBuf,
        trash_object: PathBuf,
        object_bytes: u64,
    }

    fn install_purge_snapshot(project: &TestProject) -> PurgeSnapshot {
        let selected = crate::trash::tests::install_zero_non_regenerable_orphan(project.path());
        let root = LocalStoreRoot::open(project.path()).unwrap();
        let mut leases =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(leases.has_writer());
        let diagnostics = crate::trash::trash_generations(
            &root,
            &mut leases,
            &[selected],
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(diagnostics.published_objects, 1);
        assert_eq!(diagnostics.streamed_bytes, 0);
        drop(leases);

        let active = crate::trash::tests::active_generation_file(project.path(), selected);
        let trash = crate::trash::tests::trash_generation_file(project.path(), selected);
        assert!(!active.exists());
        let generation_bytes = fs::read(&trash).unwrap();
        let document: serde_json::Value = serde_json::from_slice(&generation_bytes).unwrap();
        let digest = document["reachable_objects"][0]["digest"]
            .as_str()
            .unwrap()
            .strip_prefix("sha256:")
            .unwrap();
        let (fanout, suffix) = digest.split_at(2);
        let active_object = project
            .path()
            .join("objects/sha256")
            .join(fanout)
            .join(suffix);
        let trash_object = project
            .path()
            .join("trash/objects/sha256")
            .join(fanout)
            .join(suffix);
        assert!(active_object.exists());
        fs::create_dir_all(trash_object.parent().unwrap()).unwrap();
        fs::copy(&active_object, &trash_object).unwrap();

        PurgeSnapshot {
            generation_id: selected,
            generation_bytes: u64::try_from(generation_bytes.len()).unwrap(),
            trash_generation: trash,
            object_bytes: fs::metadata(&trash_object).unwrap().len(),
            trash_object,
        }
    }

    fn request_id(id: u64) -> ProjectStoreRequestId {
        ProjectStoreRequestId::new(id).unwrap()
    }

    fn public_recv_timeout(actor: &ProjectStoreActor) -> ProjectStoreCompletion {
        let timeout = match env::var(HOSTED_ACTOR_TIMEOUT_ENV) {
            Ok(value) => {
                assert_eq!(value, "15000", "invalid hosted actor timeout");
                Duration::from_secs(15)
            }
            Err(env::VarError::NotPresent) => TIMEOUT,
            Err(error) => panic!("invalid hosted actor timeout: {error}"),
        };
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(completion) = actor.try_recv() {
                return completion;
            }
            assert!(
                Instant::now() < deadline,
                "public actor completion timed out"
            );
            thread::yield_now();
        }
    }

    fn wait_until_active(actor: &EstablishedProjectActor, expected: u64) {
        let deadline = Instant::now() + TIMEOUT;
        let mut state = actor.shared.lock();
        loop {
            if state
                .active
                .as_ref()
                .is_some_and(|active| active.request_id == request_id(expected))
            {
                return;
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("request did not become active before the timeout");
            let (next, wait) = actor
                .shared
                .wake
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
            assert!(!wait.timed_out(), "request did not become active");
        }
    }

    fn generation_id(id: &str) -> ProjectGenerationId {
        ProjectGenerationId::parse(id).unwrap()
    }

    fn assert_cancel_ack(completion: ProjectStoreCompletion, expected: u64) {
        assert!(matches!(
            completion,
            ProjectStoreCompletion::Cancelled {
                request_id: actual_id,
                result: Ok(())
            } if actual_id == request_id(expected)
        ));
    }

    fn assert_cancelled_target(completion: ProjectStoreCompletion, expected: u64) {
        assert!(matches!(
            completion,
            ProjectStoreCompletion::ManualSaved {
                request_id: actual_id,
                result: Err(ProjectStoreFault::Cancelled)
            } | ProjectStoreCompletion::Autosaved {
                request_id: actual_id,
                result: Err(ProjectStoreFault::Cancelled)
            } | ProjectStoreCompletion::SavedAs {
                request_id: actual_id,
                result: Err(ProjectStoreFault::Cancelled)
            } | ProjectStoreCompletion::RecoveryInspected {
                request_id: actual_id,
                result: Err(ProjectStoreFault::Cancelled)
            } | ProjectStoreCompletion::RecoveryOpened {
                request_id: actual_id,
                result: Err(ProjectStoreFault::Cancelled)
            } | ProjectStoreCompletion::ArtifactsLoaded {
                request_id: actual_id,
                result: Err(ProjectStoreFault::Cancelled)
            } | ProjectStoreCompletion::Pinned {
                request_id: actual_id,
                result: Err(ProjectStoreFault::Cancelled)
            } | ProjectStoreCompletion::Unpinned {
                request_id: actual_id,
                result: Err(ProjectStoreFault::Cancelled)
            } | ProjectStoreCompletion::CompactionPlanned {
                request_id: actual_id,
                result: Err(ProjectStoreFault::Cancelled)
            } | ProjectStoreCompletion::Trashed {
                request_id: actual_id,
                result: Err(ProjectStoreFault::Cancelled)
            } | ProjectStoreCompletion::Purged {
                request_id: actual_id,
                result: Err(ProjectStoreFault::Cancelled)
            } | ProjectStoreCompletion::Verified {
                request_id: actual_id,
                result: Err(ProjectStoreFault::Cancelled)
            } if actual_id == request_id(expected)
        ));
    }

    fn assert_saved_as_fault(
        completion: ProjectStoreCompletion,
        expected_request: u64,
        expected_fault: ProjectStoreFault,
    ) {
        match completion {
            ProjectStoreCompletion::SavedAs { request_id, result } => {
                assert_eq!(request_id, self::request_id(expected_request));
                assert_eq!(result, Err(expected_fault));
            }
            other => panic!("unexpected Save As completion: {other:?}"),
        }
    }

    fn gated_manual_capture() -> (ProjectCommitCapture, Arc<Gate>) {
        let frozen = frozen_generation(STALE_MANUAL);
        let bytes = Arc::<[u8]>::from(b"bounded actor cancellation source".as_slice());
        let schema = ArtifactSchema::AnalysisTableV1;
        let facts = ExactBytesHasher::hash(&bytes[..]).unwrap();
        let descriptor = RawObjectDescriptor::new(
            facts.digest(),
            facts.byte_length(),
            MediaType::parse(schema.media_type()).unwrap(),
            ObjectRole::parse(schema.object_role()).unwrap(),
        );
        let source_layers = frozen.projection().state().artifacts().first().map_or_else(
            || vec![LogicalLayerKey::new(0)],
            |artifact| artifact.source_layers().to_vec(),
        );
        let artifact = ArtifactReference::new(
            ArtifactHandleId::from_bytes([0x99; 16]),
            schema,
            ArtifactContentId::parse(&format!("{}{}", ArtifactContentId::PREFIX, "99".repeat(32)))
                .unwrap(),
            descriptor.clone(),
            None,
            None,
            source_layers,
            "actor cancellation",
            true,
            ArtifactCompleteness::Complete,
            ArtifactRecoverability::NonRegenerable,
        )
        .unwrap();
        let old = frozen.projection();
        let project_id = old.state().project_id();
        let mut artifacts = old.state().artifacts().to_vec();
        artifacts.push(artifact);
        let state = ProjectState::new(
            project_id,
            old.state().dataset().clone(),
            old.state().view().clone(),
            old.state().channel_presets().to_vec(),
            artifacts,
        )
        .unwrap();
        let sequence = old.revision_high_water().sequence() + 1;
        let projection = ProjectGenerationProjection::new(
            ProjectRevisionId::new(project_id, sequence),
            ProjectRevisionHighWater::new(project_id, sequence),
            state,
        )
        .unwrap();
        let gate = Arc::new(Gate::default());
        let capture = frozen_capture(
            &frozen,
            projection,
            Some(generation_id(STALE_MANUAL)),
            None,
            Some((descriptor, Arc::clone(&bytes), Arc::clone(&gate))),
        );
        (capture, gate)
    }

    fn frozen_capture(
        frozen: &GenerationDocument,
        projection: ProjectGenerationProjection,
        expected_parent: Option<ProjectGenerationId>,
        autosave_base: Option<ProjectGenerationId>,
        gated: Option<(RawObjectDescriptor, Arc<[u8]>, Arc<Gate>)>,
    ) -> ProjectCommitCapture {
        let mut sources: Vec<Box<dyn ProjectObjectSource>> = Vec::new();
        let authority = expected_parent
            .or(autosave_base)
            .or_else(|| frozen.forked_from().map(|(_, generation_id)| generation_id));
        let reusable = authority
            .map(|generation_id| fixture_authority_descriptors(frozen, generation_id))
            .unwrap_or_default();
        for artifact in projection.state().artifacts() {
            if let Some((descriptor, bytes, gate)) = &gated
                && artifact.object() == descriptor
            {
                sources.push(Box::new(GatedSource {
                    descriptor: descriptor.clone(),
                    bytes: Arc::clone(bytes),
                    gate: Arc::clone(gate),
                }));
                continue;
            }
            if reusable.get(&artifact.object().digest()) == Some(artifact.object()) {
                continue;
            }
            let storage = frozen.bindings().get(&artifact.object().digest()).unwrap();
            let ArtifactStorage::Direct { object } = storage else {
                panic!("the tiny actor fixture must remain direct");
            };
            sources.push(Box::new(MemorySource {
                descriptor: artifact.object().clone(),
                bytes: Arc::from(fixture_extract(&fixture_object_member(object.digest()))),
            }));
        }
        ProjectCommitCapture::new(
            projection,
            expected_parent,
            autosave_base,
            frozen.forked_from(),
            sources,
        )
        .unwrap()
    }

    fn controlled_fixture_capture(
        store: &str,
        frozen: &GenerationDocument,
        projection: ProjectGenerationProjection,
        expected_parent: Option<ProjectGenerationId>,
        autosave_base: Option<ProjectGenerationId>,
        forked_from: Option<(ProjectId, ProjectGenerationId)>,
        behavior: ControlledRead,
    ) -> (ProjectCommitCapture, Arc<AtomicUsize>) {
        controlled_fixture_capture_inner(
            store,
            frozen,
            projection,
            expected_parent,
            autosave_base,
            forked_from,
            behavior,
            true,
        )
    }

    fn controlled_fixture_capture_with_all_sources(
        store: &str,
        frozen: &GenerationDocument,
        projection: ProjectGenerationProjection,
        expected_parent: Option<ProjectGenerationId>,
        autosave_base: Option<ProjectGenerationId>,
        forked_from: Option<(ProjectId, ProjectGenerationId)>,
        behavior: ControlledRead,
    ) -> (ProjectCommitCapture, Arc<AtomicUsize>) {
        controlled_fixture_capture_inner(
            store,
            frozen,
            projection,
            expected_parent,
            autosave_base,
            forked_from,
            behavior,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn controlled_fixture_capture_inner(
        store: &str,
        frozen: &GenerationDocument,
        projection: ProjectGenerationProjection,
        expected_parent: Option<ProjectGenerationId>,
        autosave_base: Option<ProjectGenerationId>,
        forked_from: Option<(ProjectId, ProjectGenerationId)>,
        behavior: ControlledRead,
        omit_reusable: bool,
    ) -> (ProjectCommitCapture, Arc<AtomicUsize>) {
        let opens = Arc::new(AtomicUsize::new(0));
        let mut sources: Vec<Box<dyn ProjectObjectSource>> = Vec::new();
        let authority = expected_parent
            .or(autosave_base)
            .or_else(|| forked_from.map(|(_, generation_id)| generation_id));
        let reusable = if omit_reusable {
            authority
                .map(|generation_id| fixture_authority_descriptors(frozen, generation_id))
                .unwrap_or_default()
        } else {
            BTreeMap::new()
        };
        for artifact in projection.state().artifacts() {
            if reusable.get(&artifact.object().digest()) == Some(artifact.object()) {
                continue;
            }
            let storage = frozen.bindings().get(&artifact.object().digest()).unwrap();
            let bytes = match storage {
                ArtifactStorage::Direct { object } => {
                    fixture_extract(&fixture_object_member_in(store, object.digest()))
                }
                ArtifactStorage::Paged { binding_manifest } => {
                    let binding_bytes = fixture_extract(&fixture_object_member_in(
                        store,
                        binding_manifest.digest(),
                    ));
                    let binding = LogicalObjectBinding::decode(
                        &binding_bytes,
                        artifact.object(),
                        binding_manifest,
                        ProjectStoreLimits::default(),
                    )
                    .unwrap();
                    let mut logical = Vec::new();
                    for page in binding.pages() {
                        logical.extend_from_slice(&fixture_extract(&fixture_object_member_in(
                            store,
                            page.object().digest(),
                        )));
                    }
                    logical
                }
            };
            sources.push(Box::new(ControlledSource {
                descriptor: artifact.object().clone(),
                bytes: Arc::from(bytes),
                opens: Arc::clone(&opens),
                behavior: behavior.clone(),
            }));
        }
        (
            ProjectCommitCapture::new(
                projection,
                expected_parent,
                autosave_base,
                forked_from,
                sources,
            )
            .unwrap(),
            opens,
        )
    }

    fn fixture_authority_descriptors(
        fallback: &GenerationDocument,
        generation_id: ProjectGenerationId,
    ) -> BTreeMap<ExactBytesDigest, RawObjectDescriptor> {
        let authority = match generation_id {
            id if id == self::generation_id(STALE_MANUAL) => frozen_generation(STALE_MANUAL),
            id if id == self::generation_id(STALE_AUTOSAVE) => frozen_generation(STALE_AUTOSAVE),
            id if id == self::generation_id(PROVISIONAL_AUTOSAVE) => {
                frozen_generation_in("provisional.m4dproj", PROVISIONAL_AUTOSAVE)
            }
            id if id == self::generation_id(RECOVERABLE_G1) => {
                frozen_generation_in("recoverable.m4dproj", RECOVERABLE_G1)
            }
            id if id == self::generation_id(RECOVERABLE_G2) => {
                frozen_generation_in("recoverable.m4dproj", RECOVERABLE_G2)
            }
            id if id == self::generation_id(RECOVERABLE_AUTOSAVE) => {
                frozen_generation_in("recoverable.m4dproj", RECOVERABLE_AUTOSAVE)
            }
            id if id == self::generation_id(RECOVERABLE_ORPHAN) => {
                frozen_generation_in("recoverable.m4dproj", RECOVERABLE_ORPHAN)
            }
            id if id == self::generation_id(DIVERGENT_INITIAL) => {
                frozen_generation_in("divergent.m4dproj", DIVERGENT_INITIAL)
            }
            id if id == self::generation_id(DIVERGENT_NEXT) => {
                frozen_generation_in("divergent.m4dproj", DIVERGENT_NEXT)
            }
            _ => fallback.clone(),
        };
        let mut descriptors = BTreeMap::new();
        for artifact in authority.projection().state().artifacts() {
            descriptors
                .entry(artifact.object().digest())
                .or_insert_with(|| artifact.object().clone());
        }
        descriptors
    }

    fn retarget_projection(
        frozen: &GenerationDocument,
        project_id: ProjectId,
        dataset: DatasetReference,
    ) -> ProjectGenerationProjection {
        let old = frozen.projection();
        let state = ProjectState::new(
            project_id,
            dataset,
            old.state().view().clone(),
            old.state().channel_presets().to_vec(),
            old.state().artifacts().to_vec(),
        )
        .unwrap();
        ProjectGenerationProjection::new(
            ProjectRevisionId::new(project_id, old.revision().sequence()),
            ProjectRevisionHighWater::new(project_id, old.revision_high_water().sequence()),
            state,
        )
        .unwrap()
    }

    fn fresh_fork_projection(
        frozen: &GenerationDocument,
        project_id: ProjectId,
        dataset: DatasetReference,
    ) -> ProjectGenerationProjection {
        let old = frozen.projection();
        let state = ProjectState::new(
            project_id,
            dataset,
            old.state().view().clone(),
            old.state().channel_presets().to_vec(),
            old.state().artifacts().to_vec(),
        )
        .unwrap();
        ProjectGenerationProjection::new(
            ProjectRevisionId::initial(project_id),
            ProjectRevisionHighWater::initial(project_id),
            state,
        )
        .unwrap()
    }

    fn next_revision_projection(frozen: &GenerationDocument) -> ProjectGenerationProjection {
        let old = frozen.projection();
        let project_id = old.state().project_id();
        let revision = old.revision().sequence().checked_add(1).unwrap();
        let high_water = old.revision_high_water().sequence().checked_add(1).unwrap();
        ProjectGenerationProjection::new(
            ProjectRevisionId::new(project_id, revision),
            ProjectRevisionHighWater::new(project_id, high_water),
            old.state().clone(),
        )
        .unwrap()
    }

    fn file_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
            let mut entries = fs::read_dir(directory)
                .unwrap()
                .map(|entry| entry.unwrap())
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let file_type = entry.file_type().unwrap();
                if file_type.is_dir() {
                    visit(root, &path, files);
                } else {
                    assert!(file_type.is_file(), "fixture contains an unexpected object");
                    files.insert(
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        fs::read(path).unwrap(),
                    );
                }
            }
        }

        let mut files = BTreeMap::new();
        visit(root, root, &mut files);
        files
    }

    fn private_pin_staging_residue(root: &Path) -> usize {
        let staging = root.join("staging");
        let mut entries = match fs::read_dir(&staging) {
            Ok(entries) => entries.map(|entry| entry.unwrap()).collect::<Vec<_>>(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return 0,
            Err(error) => panic!("failed to inspect private Pin staging residue: {error}"),
        };
        entries.sort_by_key(|entry| entry.file_name());
        for entry in &entries {
            let file_type = entry.file_type().unwrap();
            assert!(file_type.is_dir() && !file_type.is_symlink());
            assert!(
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("tx-"))
            );
            for (path, bytes) in file_tree(&entry.path()) {
                assert_eq!(path, Path::new("payload"));
                assert!(bytes.is_empty() || bytes.len() == 160);
            }
        }
        entries.len()
    }

    fn stage_count(parent: &Path) -> usize {
        fs::read_dir(parent)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(".mirante4d-project-stage-"))
            })
            .count()
    }

    fn acquire_writer_eventually(root: &LocalStoreRoot) -> ProjectStoreLeases {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let leases =
                ProjectStoreLeases::acquire(root, ProjectOpenMode::PreferWritable).unwrap();
            if leases.has_writer() {
                return leases;
            }
            assert!(
                Instant::now() < deadline,
                "writer lease remained unavailable after the parallel fork/exec window"
            );
            drop(leases);
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn frozen_generation(id: &str) -> GenerationDocument {
        frozen_generation_in("stale.m4dproj", id)
    }

    fn frozen_generation_in(store: &str, id: &str) -> GenerationDocument {
        let id = generation_id(id);
        let envelope =
            ProjectEnvelope::decode(&fixture_extract(&format!("{store}/project.json"))).unwrap();
        GenerationDocument::decode(
            id,
            envelope.project_id(),
            &fixture_extract(&fixture_generation_member_in(store, id)),
            ProjectStoreLimits::default(),
        )
        .unwrap()
    }

    fn fixture_artifact_bytes(
        store: &str,
        generation: &GenerationDocument,
        artifact: &ArtifactReference,
    ) -> Vec<u8> {
        match generation
            .bindings()
            .get(&artifact.object().digest())
            .unwrap()
        {
            ArtifactStorage::Direct { object } => {
                fixture_extract(&fixture_object_member_in(store, object.digest()))
            }
            ArtifactStorage::Paged { binding_manifest } => {
                let binding_bytes =
                    fixture_extract(&fixture_object_member_in(store, binding_manifest.digest()));
                let binding = LogicalObjectBinding::decode(
                    &binding_bytes,
                    artifact.object(),
                    binding_manifest,
                    ProjectStoreLimits::default(),
                )
                .unwrap();
                let mut bytes = Vec::new();
                for page in binding.pages() {
                    bytes.extend_from_slice(&fixture_extract(&fixture_object_member_in(
                        store,
                        page.object().digest(),
                    )));
                }
                bytes
            }
        }
    }

    fn project_object_path(root: &Path, digest: ExactBytesDigest) -> PathBuf {
        let digest = digest.digest().to_string();
        root.join("objects/sha256")
            .join(&digest[..2])
            .join(&digest[2..])
    }

    fn fixture_extract(member: &str) -> Vec<u8> {
        let output = Command::new("tar")
            .arg("-xzOf")
            .arg(fixture_archive())
            .arg(member)
            .output()
            .unwrap();
        assert!(output.status.success(), "failed to extract {member}");
        output.stdout
    }

    fn fixture_archive() -> PathBuf {
        env::var_os("MIRANTE4D_PROJECT_STORE_VM_FIXTURE")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../fixtures/project/project-store-v1.tar.gz")
            })
    }

    fn fixture_generation_member_in(store: &str, id: ProjectGenerationId) -> String {
        let digest = id.digest().to_string();
        format!(
            "{store}/generations/sha256/{}/{}.json",
            &digest[..2],
            &digest[2..]
        )
    }

    fn fixture_object_member(digest: ExactBytesDigest) -> String {
        fixture_object_member_in("stale.m4dproj", digest)
    }

    fn fixture_object_member_in(store: &str, digest: ExactBytesDigest) -> String {
        let digest = digest.digest().to_string();
        format!("{store}/objects/sha256/{}/{}", &digest[..2], &digest[2..])
    }
}

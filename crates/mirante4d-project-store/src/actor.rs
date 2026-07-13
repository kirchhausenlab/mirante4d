//! Crate-private execution core for one recovery-capable project session.
//!
//! The public actor remains deliberately non-constructible until open/create,
//! recovery, and product wiring can establish the complete session contract.

#![cfg(target_os = "linux")]
#![cfg_attr(not(test), allow(dead_code))]

use std::{
    collections::{BTreeSet, VecDeque},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

#[cfg(test)]
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

#[cfg(test)]
use crate::lease::GcTransitionInjector;

use crate::{
    ProjectCommitCapture, ProjectGenerationId, ProjectOpenMode, ProjectStoreCommand,
    ProjectStoreCompletion, ProjectStoreConfig, ProjectStoreDiagnostics, ProjectStoreFault,
    ProjectStorePath, ProjectStoreReceipt, ProjectStoreRequestId, ProjectStoreSession,
    inspection::{
        RecoveryInspection, cleanup_dead_writer_staging, inspect_established_store,
        inspect_recovery, open_recovery,
    },
    lease::{LeaseError, ProjectStoreLeases},
    local::{LocalStoreRoot, valid_checkpoint_id},
    pin::{publish_pin, remove_pin},
    transaction::{
        InitialPackageMode, install_initial_manual_package,
        publish_established_autosave_generation, publish_established_manual_generation,
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
    ManualSave {
        request_id: ProjectStoreRequestId,
        capture: ProjectCommitCapture,
    },
    Autosave {
        request_id: ProjectStoreRequestId,
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
            Self::ManualSave { request_id, .. }
            | Self::Autosave { request_id, .. }
            | Self::SaveAs { request_id, .. }
            | Self::InspectRecovery { request_id }
            | Self::OpenRecovery { request_id, .. }
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

impl SessionResources {
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
        let installed = install_initial_manual_package(
            &destination,
            InitialPackageMode::SaveAs {
                source_project_id,
                source_generation_id: source_generation,
            },
            capture,
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
}

impl EstablishedProjectActor {
    pub(crate) fn start(
        path: &ProjectStorePath,
        config: ProjectStoreConfig,
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
            autosave_enabled: config.autosave_enabled(),
            #[cfg(test)]
            gc_transition_injector: OnceLock::new(),
        });
        let worker_shared = Arc::clone(&shared);
        let worker_path = path.clone();
        let (startup_sender, startup_receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name(String::from("mirante4d-project-store"))
            .spawn(move || match open_resources(&worker_path, limits) {
                Ok(resources) => {
                    if startup_sender.send(Ok(())).is_ok() {
                        worker_main(worker_shared, resources, limits);
                    }
                }
                Err(error) => {
                    let _ = startup_sender.send(Err(error));
                    let mut state = worker_shared.lock();
                    state.worker_exited = true;
                    worker_shared.wake.notify_all();
                }
            })
            .map_err(|_| ProjectStoreFault::Corruption {
                stage: "actor_spawn",
            })?;
        match startup_receiver.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let _ = worker.join();
                return Err(error);
            }
            Err(_) => {
                let _ = worker.join();
                return Err(ProjectStoreFault::Corruption {
                    stage: "actor_startup",
                });
            }
        }
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    #[cfg(test)]
    fn start_with_gc_transition_injector(
        path: &ProjectStorePath,
        config: ProjectStoreConfig,
        injector: Arc<GcTransitionInjector>,
    ) -> Result<Self, ProjectStoreFault> {
        let actor = Self::start(path, config)?;
        actor
            .shared
            .gc_transition_injector
            .set(injector)
            .expect("a test actor installs one GC transition injector");
        Ok(actor)
    }

    /// Accepts only the established-session commands implemented by this core.
    pub(crate) fn try_submit(&self, command: ProjectStoreCommand) -> Result<(), ProjectStoreFault> {
        match command {
            ProjectStoreCommand::ManualSave {
                request_id,
                capture,
            } => self.submit_work(Work::ManualSave {
                request_id,
                capture,
            }),
            ProjectStoreCommand::Autosave {
                request_id,
                capture,
            } if self.shared.autosave_enabled => self.submit_work(Work::Autosave {
                request_id,
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
            _ => Err(ProjectStoreFault::Corruption {
                stage: "actor_command_unimplemented",
            }),
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

fn worker_main(
    shared: Arc<Shared>,
    resources: SessionResources,
    limits: crate::ProjectStoreLimits,
) {
    let mut resources = Some(resources);
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
            drop(resources.take());
            state.finish_reserved(ProjectStoreCompletion::Closed {
                request_id,
                result: Ok(()),
            });
            state.worker_exited = true;
            shared.wake.notify_all();
            return;
        };

        let session = resources.as_mut().expect("resources live until close");
        let completion = match work {
            Work::ManualSave {
                request_id,
                capture,
            } => ProjectStoreCompletion::ManualSaved {
                request_id,
                result: publish_established_manual_generation(
                    &session.root,
                    &session.leases,
                    capture,
                    limits,
                    || cancelled.load(Ordering::Acquire),
                ),
            },
            Work::Autosave {
                request_id,
                capture,
            } => ProjectStoreCompletion::Autosaved {
                request_id,
                result: publish_established_autosave_generation(
                    &session.root,
                    &session.leases,
                    capture,
                    limits,
                    || cancelled.load(Ordering::Acquire),
                ),
            },
            Work::SaveAs {
                request_id,
                destination,
                source_generation,
                capture,
            } => ProjectStoreCompletion::SavedAs {
                request_id,
                result: session.save_as(destination, source_generation, capture, limits, || {
                    cancelled.load(Ordering::Acquire)
                }),
            },
            Work::InspectRecovery { request_id } => ProjectStoreCompletion::RecoveryInspected {
                request_id,
                result: session.inspect_recovery(limits, || cancelled.load(Ordering::Acquire)),
            },
            Work::OpenRecovery {
                request_id,
                generation_id,
            } => ProjectStoreCompletion::RecoveryOpened {
                request_id,
                result: session
                    .open_recovery(generation_id, limits, || cancelled.load(Ordering::Acquire)),
            },
            Work::Pin {
                request_id,
                checkpoint_id,
                generation_id,
            } => {
                #[cfg(test)]
                if let Some(injector) = shared.gc_transition_injector.get() {
                    session
                        .leases
                        .set_gc_transition_injector(Arc::clone(injector));
                }
                ProjectStoreCompletion::Pinned {
                    request_id,
                    result: publish_pin(
                        &session.root,
                        &session.leases,
                        &checkpoint_id,
                        generation_id,
                        limits,
                        || cancelled.load(Ordering::Acquire),
                    ),
                }
            }
            Work::Unpin {
                request_id,
                checkpoint_id,
            } => {
                #[cfg(test)]
                if let Some(injector) = shared.gc_transition_injector.get() {
                    session
                        .leases
                        .set_gc_transition_injector(Arc::clone(injector));
                }
                ProjectStoreCompletion::Unpinned {
                    request_id,
                    result: remove_pin(
                        &session.root,
                        &session.leases,
                        &checkpoint_id,
                        limits,
                        || cancelled.load(Ordering::Acquire),
                    ),
                }
            }
            Work::PlanCompaction { request_id } => ProjectStoreCompletion::CompactionPlanned {
                request_id,
                result: crate::inspection::plan_compaction(&session.root, limits, || {
                    cancelled.load(Ordering::Acquire)
                }),
            },
            Work::Trash {
                request_id,
                generations,
            } => {
                #[cfg(test)]
                if let Some(injector) = shared.gc_transition_injector.get() {
                    session
                        .leases
                        .set_gc_transition_injector(Arc::clone(injector));
                }
                let result = trash_generations(
                    &session.root,
                    &mut session.leases,
                    &generations,
                    limits,
                    || cancelled.load(Ordering::Acquire),
                )
                .map(|diagnostics| actor_diagnostics(&shared, &session.leases, diagnostics));
                ProjectStoreCompletion::Trashed { request_id, result }
            }
            Work::Purge { request_id } => {
                #[cfg(test)]
                if let Some(injector) = shared.gc_transition_injector.get() {
                    session
                        .leases
                        .set_gc_transition_injector(Arc::clone(injector));
                }
                let result = purge_trash(&session.root, &mut session.leases, limits, || {
                    cancelled.load(Ordering::Acquire)
                })
                .map(|diagnostics| actor_diagnostics(&shared, &session.leases, diagnostics));
                ProjectStoreCompletion::Purged { request_id, result }
            }
            Work::FullVerify { request_id } => {
                let result = crate::full_verify::full_verify(&session.root, limits, || {
                    cancelled.load(Ordering::Acquire)
                })
                .map(|diagnostics| actor_diagnostics(&shared, &session.leases, diagnostics));
                ProjectStoreCompletion::Verified { request_id, result }
            }
        };
        let maintenance_lost = session.leases.maintenance_lost();
        if maintenance_lost {
            let mut state = shared.lock();
            state.active = None;
            if state.shutdown {
                state.completion_reservations = 0;
                drop(resources.take());
                state.worker_exited = true;
                shared.wake.notify_all();
                return;
            }
            state.accepting = false;
            drop(resources.take());
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

    drop(resources.take());
    let mut state = shared.lock();
    state.worker_exited = true;
    shared.wake.notify_all();
}

fn open_resources(
    path: &ProjectStorePath,
    limits: crate::ProjectStoreLimits,
) -> Result<SessionResources, ProjectStoreFault> {
    let root = LocalStoreRoot::open(path.as_path()).map_err(|_| ProjectStoreFault::Corruption {
        stage: "actor_startup_open",
    })?;
    let leases =
        ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).map_err(|error| {
            match error {
                LeaseError::Indeterminate => ProjectStoreFault::CommitIndeterminate,
                LeaseError::InvalidAnchor | LeaseError::Io { .. } => {
                    ProjectStoreFault::Corruption {
                        stage: "actor_startup_lease",
                    }
                }
            }
        })?;
    inspect_recovery(&root, limits, || false)?;
    if leases.has_writer() {
        cleanup_dead_writer_staging(&root, &leases, limits, || false)?;
    }
    Ok(SessionResources {
        path: path.clone(),
        root,
        leases,
    })
}

#[cfg(test)]
mod tests {
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
        ProjectGenerationId, ProjectObjectSource, ProjectStoreLimits,
        generation::{ArtifactStorage, GenerationDocument, LogicalObjectBinding},
        lease::{GcTransition, GcTransitionTarget, TransitionEdge},
        wire::ProjectEnvelope,
    };

    const TIMEOUT: Duration = Duration::from_secs(5);
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
    const STALE_MANUAL: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "d5020fa3c69a493b34ffbbf3a67a249354e83e5a6d738479d46c7e301786d2ec"
    );
    const STALE_AUTOSAVE: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "c357ffd5f7c051bf22877ffcd6680bdcd0f7db4068af93587e4a1f5bed0542a0"
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
        Fail,
        Gate(Arc<Gate>),
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
                ControlledRead::Fail => Err(io::Error::other("injected source failure")),
                ControlledRead::Gate(gate) => Ok(Box::new(GatedReader {
                    bytes: Cursor::new(Arc::clone(&self.bytes)),
                    gate: Arc::clone(gate),
                })),
            }
        }
    }

    #[test]
    fn established_commands_are_serialized_and_close_releases_the_writer_lease() {
        let project = TestProject::extracted("serial-close");
        let actor =
            EstablishedProjectActor::start(&project.store_path(), Default::default()).unwrap();
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
    fn recovery_inspection_and_open_are_read_only_and_keep_current_save_authority() {
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
        let (capture, _) = controlled_fixture_capture(
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
                result: Ok(receipt),
            }) if actual == request_id(4)
                && receipt.current_generation_id() == generation_id(RECOVERABLE_ORPHAN)
        ));

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
                result: Err(ProjectStoreFault::ReadOnly),
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
    fn failed_and_cancelled_save_as_keep_the_source_session_authoritative() {
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

        let failed = destinations.destination("source-failure.m4dproj");
        let (failed_capture, failed_opens) = controlled_fixture_capture(
            "divergent.m4dproj",
            &target,
            target.projection().clone(),
            None,
            None,
            target.forked_from(),
            ControlledRead::Fail,
        );
        actor
            .try_submit(ProjectStoreCommand::SaveAs {
                request_id: request_id(2),
                destination: failed.clone(),
                source_generation,
                capture: failed_capture,
            })
            .unwrap();
        assert_saved_as_fault(
            actor.recv_timeout(TIMEOUT).unwrap(),
            2,
            ProjectStoreFault::SourceChanged,
        );
        assert_eq!(failed_opens.load(Ordering::SeqCst), 1);
        assert!(!failed.as_path().exists());
        assert_eq!(stage_count(destinations.path()), 0);

        let cancelled = destinations.destination("cancelled.m4dproj");
        let gate = Arc::new(Gate::default());
        let (cancelled_capture, cancelled_opens) = controlled_fixture_capture(
            "divergent.m4dproj",
            &target,
            target.projection().clone(),
            None,
            None,
            target.forked_from(),
            ControlledRead::Gate(Arc::clone(&gate)),
        );
        actor
            .try_submit(ProjectStoreCommand::SaveAs {
                request_id: request_id(3),
                destination: cancelled.clone(),
                source_generation,
                capture: cancelled_capture,
            })
            .unwrap();
        gate.wait_started();
        actor.try_submit(cancel_command(4, 3)).unwrap();
        assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 4);
        gate.release();
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 3);
        assert_eq!(cancelled_opens.load(Ordering::SeqCst), 1);
        assert!(!cancelled.as_path().exists());
        assert_eq!(stage_count(destinations.path()), 0);
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
        let opens = Arc::new(AtomicUsize::new(0));
        let mut sources: Vec<Box<dyn ProjectObjectSource>> = Vec::new();
        for artifact in projection.state().artifacts() {
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

    fn fixture_extract(member: &str) -> Vec<u8> {
        let output = Command::new("tar")
            .arg("-xOf")
            .arg(fixture_archive())
            .arg(member)
            .output()
            .unwrap();
        assert!(output.status.success(), "failed to extract {member}");
        output.stdout
    }

    fn fixture_archive() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/project/project-store-v1.tar.gz")
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

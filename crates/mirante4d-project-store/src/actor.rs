//! Crate-private execution core for one established writable project session.
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
use std::time::{Duration, Instant};

use crate::{
    ProjectCommitCapture, ProjectGenerationId, ProjectOpenMode, ProjectStoreCommand,
    ProjectStoreCompletion, ProjectStoreConfig, ProjectStoreFault, ProjectStorePath,
    ProjectStoreReceipt, ProjectStoreRequestId,
    inspection::{inspect_established_store, open_established_store},
    lease::{LeaseError, ProjectStoreLeases},
    local::LocalStoreRoot,
    transaction::{
        InitialPackageMode, install_initial_manual_package,
        publish_established_autosave_generation, publish_established_manual_generation,
    },
};

/// One private worker which owns the store root, leases, and all write work.
pub(crate) struct EstablishedProjectActor {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    request_limit: usize,
    completion_limit: usize,
    autosave_enabled: bool,
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
}

impl Work {
    const fn request_id(&self) -> ProjectStoreRequestId {
        match self {
            Self::ManualSave { request_id, .. }
            | Self::Autosave { request_id, .. }
            | Self::SaveAs { request_id, .. } => *request_id,
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
        }
    }
}

/// The worker's sole ownership of one writable established session. Mutable
/// authority facts are always reinspected from `root` rather than shadowed.
struct SessionResources {
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
        let old = std::mem::replace(self, Self { root, leases });
        drop(old);
        Ok(receipt)
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
            autosave_enabled: config.autosave_enabled(),
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
        };
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
    let opened = open_established_store(path, ProjectOpenMode::PreferWritable, limits, || false)?;
    if opened.effective_mode() != ProjectOpenMode::PreferWritable {
        return Err(ProjectStoreFault::WriterContended);
    }
    let (root, leases) = opened.into_resources();
    Ok(SessionResources { root, leases })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        io::{self, Cursor, Read},
        path::{Path, PathBuf},
        process::Command,
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
        wire::ProjectEnvelope,
    };

    const TIMEOUT: Duration = Duration::from_secs(5);
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
        actor.try_submit(manual_command(9)).unwrap();
        actor.try_submit(manual_command(10)).unwrap();
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
        actor.try_submit(autosave_command(2)).unwrap();
        actor.try_submit(cancel_command(3, 2)).unwrap();
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 2);
        assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 3);

        actor.try_submit(cancel_command(4, 1)).unwrap();
        assert_cancel_ack(actor.recv_timeout(TIMEOUT).unwrap(), 4);
        gate.release();
        assert_cancelled_target(actor.recv_timeout(TIMEOUT).unwrap(), 1);

        actor.try_submit(close_command(5)).unwrap();
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

    fn request_id(id: u64) -> ProjectStoreRequestId {
        ProjectStoreRequestId::new(id).unwrap()
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

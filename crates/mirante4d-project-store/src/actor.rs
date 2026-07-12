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
    ProjectCommitCapture, ProjectOpenMode, ProjectStoreCommand, ProjectStoreCompletion,
    ProjectStoreConfig, ProjectStoreFault, ProjectStorePath, ProjectStoreRequestId,
    inspection::open_established_store,
    lease::ProjectStoreLeases,
    local::LocalStoreRoot,
    transaction::{publish_established_autosave_generation, publish_established_manual_generation},
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
}

impl Work {
    const fn request_id(&self) -> ProjectStoreRequestId {
        match self {
            Self::ManualSave { request_id, .. } | Self::Autosave { request_id, .. } => *request_id,
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
        }
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
                Ok((root, leases)) => {
                    if startup_sender.send(Ok(())).is_ok() {
                        worker_main(worker_shared, root, leases, limits);
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
    root: LocalStoreRoot,
    leases: ProjectStoreLeases,
    limits: crate::ProjectStoreLimits,
) {
    let mut resources = Some((root, leases));
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

        let (root, leases) = resources.as_ref().expect("resources live until close");
        let completion = match work {
            Work::ManualSave {
                request_id,
                capture,
            } => ProjectStoreCompletion::ManualSaved {
                request_id,
                result: publish_established_manual_generation(
                    root,
                    leases,
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
                    root,
                    leases,
                    capture,
                    limits,
                    || cancelled.load(Ordering::Acquire),
                ),
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
) -> Result<(LocalStoreRoot, ProjectStoreLeases), ProjectStoreFault> {
    let opened = open_established_store(path, ProjectOpenMode::PreferWritable, limits, || false)?;
    if opened.effective_mode() != ProjectOpenMode::PreferWritable {
        return Err(ProjectStoreFault::WriterContended);
    }
    Ok(opened.into_resources())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{self, Cursor, Read},
        path::{Path, PathBuf},
        process::Command,
        sync::{
            Arc, Condvar, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use mirante4d_domain::LogicalLayerKey;
    use mirante4d_identity::{
        ArtifactContentId, ExactBytesDigest, ExactBytesHasher, MediaType, ObjectRole,
        RawObjectDescriptor,
    };
    use mirante4d_project_model::{
        ArtifactCompleteness, ArtifactHandleId, ArtifactRecoverability, ArtifactReference,
        ArtifactSchema, ProjectGenerationProjection, ProjectRevisionHighWater, ProjectRevisionId,
        ProjectState,
    };

    use super::*;
    use crate::{
        ProjectGenerationId, ProjectObjectSource, ProjectStoreLimits,
        generation::{ArtifactStorage, GenerationDocument},
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
    static TEST_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    struct TestProject(PathBuf);

    impl TestProject {
        fn extracted(label: &str) -> Self {
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
                .arg("stale.m4dproj")
                .output()
                .unwrap();
            assert!(output.status.success(), "failed to extract stale fixture");
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

        let next =
            ProjectStoreLeases::acquire(&contender_root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(next.has_writer());
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
            } if actual_id == request_id(expected)
        ));
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

    fn frozen_generation(id: &str) -> GenerationDocument {
        let id = generation_id(id);
        let envelope =
            ProjectEnvelope::decode(&fixture_extract("stale.m4dproj/project.json")).unwrap();
        GenerationDocument::decode(
            id,
            envelope.project_id(),
            &fixture_extract(&fixture_generation_member(id)),
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

    fn fixture_generation_member(id: ProjectGenerationId) -> String {
        let digest = id.digest().to_string();
        format!(
            "stale.m4dproj/generations/sha256/{}/{}.json",
            &digest[..2],
            &digest[2..]
        )
    }

    fn fixture_object_member(digest: ExactBytesDigest) -> String {
        let digest = digest.digest().to_string();
        format!(
            "stale.m4dproj/objects/sha256/{}/{}",
            &digest[..2],
            &digest[2..]
        )
    }
}

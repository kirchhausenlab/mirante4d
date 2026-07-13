//! Held kernel leases for one prepared private project-store root.
//!
//! A session locks a duplicate of the project root shared for maintenance,
//! then optionally tries an exclusive lock on the immutable `project.json`
//! descriptor. Path presence, file contents, PIDs, and timestamps are never
//! lease authority; only the held kernel locks are.

#![cfg(target_os = "linux")]
#![cfg_attr(not(test), allow(dead_code))]

use std::{
    ffi::OsStr,
    io,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

#[cfg(test)]
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex, atomic::AtomicUsize},
    time::Instant,
};

use rustix::{
    fd::OwnedFd,
    fs::{AtFlags, FileType, FlockOperation, Mode, OFlags, flock, fstat, openat, statat},
    io::Errno,
};
use thiserror::Error;

use crate::{ProjectOpenMode, local::LocalStoreRoot};

const DIRECTORY_OPEN_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW);
const ANCHOR_OPEN_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK);
const MAINTENANCE_UPGRADE_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GcTransition {
    MaintenanceUpgrade,
    RootScan,
    CandidateListing,
    TrashDirectoryCreate,
    TrashCollisionFileSync,
    TrashMove,
    ActiveDeduplicateRemove,
    SourceDirectorySync,
    TrashDirectorySync,
    MaintenanceRestore,
}

impl GcTransition {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 10] = [
        Self::MaintenanceUpgrade,
        Self::RootScan,
        Self::CandidateListing,
        Self::TrashDirectoryCreate,
        Self::TrashCollisionFileSync,
        Self::TrashMove,
        Self::ActiveDeduplicateRemove,
        Self::SourceDirectorySync,
        Self::TrashDirectorySync,
        Self::MaintenanceRestore,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::MaintenanceUpgrade => "gc_maintenance_upgrade",
            Self::RootScan => "gc_root_scan",
            Self::CandidateListing => "gc_candidate_listing",
            Self::TrashDirectoryCreate => "gc_trash_directory_create",
            Self::TrashCollisionFileSync => "gc_trash_collision_file_sync",
            Self::TrashMove => "gc_trash_move",
            Self::ActiveDeduplicateRemove => "gc_active_deduplicate_remove",
            Self::SourceDirectorySync => "gc_source_directory_sync",
            Self::TrashDirectorySync => "gc_trash_directory_sync",
            Self::MaintenanceRestore => "gc_maintenance_restore",
        }
    }

    #[cfg(test)]
    pub(crate) fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|transition| transition.name() == name)
    }

    const fn index(self) -> usize {
        match self {
            Self::MaintenanceUpgrade => 0,
            Self::RootScan => 1,
            Self::CandidateListing => 2,
            Self::TrashDirectoryCreate => 3,
            Self::TrashCollisionFileSync => 4,
            Self::TrashMove => 5,
            Self::ActiveDeduplicateRemove => 6,
            Self::SourceDirectorySync => 7,
            Self::TrashDirectorySync => 8,
            Self::MaintenanceRestore => 9,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransitionEdge {
    Before,
    After,
}

#[cfg(test)]
impl TransitionEdge {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        match name {
            "before" => Some(Self::Before),
            "after" => Some(Self::After),
            _ => None,
        }
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct GcTransitionOccurrence {
    transition: GcTransition,
    occurrence: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct InjectedGcTransition;

#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) enum GcTransitionAction {
    Fail,
    Park { marker: Option<PathBuf> },
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GcTransitionTarget {
    pub(crate) transition: GcTransition,
    pub(crate) edge: TransitionEdge,
    pub(crate) occurrence: usize,
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct GcTransitionInjector {
    target: Option<GcTransitionTarget>,
    action: GcTransitionAction,
    attempts: [AtomicUsize; 10],
    fired: AtomicUsize,
    release: AtomicBool,
    parked_thread: Mutex<Option<thread::Thread>>,
}

#[cfg(test)]
impl GcTransitionInjector {
    pub(crate) fn recorder() -> Arc<Self> {
        Arc::new(Self::new(None, GcTransitionAction::Fail))
    }

    pub(crate) fn failing(target: GcTransitionTarget) -> Arc<Self> {
        Arc::new(Self::new(Some(target), GcTransitionAction::Fail))
    }

    pub(crate) fn parking(target: GcTransitionTarget, marker: PathBuf) -> Arc<Self> {
        Arc::new(Self::new(
            Some(target),
            GcTransitionAction::Park {
                marker: Some(marker),
            },
        ))
    }

    pub(crate) fn gated(target: GcTransitionTarget) -> Arc<Self> {
        Arc::new(Self::new(
            Some(target),
            GcTransitionAction::Park { marker: None },
        ))
    }

    fn new(target: Option<GcTransitionTarget>, action: GcTransitionAction) -> Self {
        Self {
            target,
            action,
            attempts: std::array::from_fn(|_| AtomicUsize::new(0)),
            fired: AtomicUsize::new(0),
            release: AtomicBool::new(false),
            parked_thread: Mutex::new(None),
        }
    }

    fn before(
        &self,
        transition: GcTransition,
    ) -> Result<GcTransitionOccurrence, InjectedGcTransition> {
        let occurrence = self.attempts[transition.index()].fetch_add(1, Ordering::AcqRel);
        let occurrence = GcTransitionOccurrence {
            transition,
            occurrence,
        };
        self.hit(occurrence, TransitionEdge::Before)?;
        Ok(occurrence)
    }

    fn after(&self, occurrence: GcTransitionOccurrence) -> Result<(), InjectedGcTransition> {
        self.hit(occurrence, TransitionEdge::After)
    }

    fn hit(
        &self,
        occurrence: GcTransitionOccurrence,
        edge: TransitionEdge,
    ) -> Result<(), InjectedGcTransition> {
        if self.target
            != Some(GcTransitionTarget {
                transition: occurrence.transition,
                edge,
                occurrence: occurrence.occurrence,
            })
        {
            return Ok(());
        }
        self.fired.fetch_add(1, Ordering::AcqRel);
        match &self.action {
            GcTransitionAction::Fail => Err(InjectedGcTransition),
            GcTransitionAction::Park { marker } => {
                if let Some(marker) = marker {
                    fs::write(marker, b"ready").expect("transition marker must be writable");
                }
                let current = thread::current();
                *self
                    .parked_thread
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(current.clone());
                while !self.release.load(Ordering::Acquire) {
                    thread::park();
                }
                Err(InjectedGcTransition)
            }
        }
    }

    pub(crate) fn attempts(&self, transition: GcTransition) -> usize {
        self.attempts[transition.index()].load(Ordering::Acquire)
    }

    pub(crate) fn fired(&self) -> usize {
        self.fired.load(Ordering::Acquire)
    }

    pub(crate) fn wait_until_parked(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if self
                .parked_thread
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_some()
            {
                return;
            }
            assert!(Instant::now() < deadline, "transition hook did not park");
            thread::yield_now();
        }
    }

    pub(crate) fn release(&self) {
        self.release.store(true, Ordering::Release);
        if let Some(thread) = self
            .parked_thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            thread.unpark();
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum LeaseError {
    #[error("the prior head publication has indeterminate durability")]
    Indeterminate,
    #[error("the immutable project envelope is not a private regular file")]
    InvalidAnchor,
    #[error("project-store lease I/O failed while attempting to {operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Error)]
pub(crate) enum MaintenanceTransitionError {
    #[error("exclusive maintenance requires the held writer lease")]
    ReadOnly,
    #[error("exclusive maintenance acquisition was cancelled")]
    Cancelled,
    #[error(transparent)]
    Lease(#[from] LeaseError),
    #[error("the shared maintenance lease could not be restored: {source}")]
    MaintenanceLost {
        #[source]
        source: LeaseError,
    },
    #[error("shared maintenance was restored but completion was indeterminate: {source}")]
    MaintenanceRestoredIndeterminate {
        #[source]
        source: LeaseError,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AnchorIdentity {
    device: u64,
    inode: u64,
    links: u64,
}

impl AnchorIdentity {
    fn from_stat(stat: rustix::fs::Stat) -> Result<Self, LeaseError> {
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
            return Err(LeaseError::InvalidAnchor);
        }
        Ok(Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            links: stat.st_nlink,
        })
    }
}

#[derive(Debug)]
struct WriterLease {
    anchor: OwnedFd,
    identity: AnchorIdentity,
}

impl WriterLease {
    fn confirm(&self, root: &LocalStoreRoot) -> Result<(), LeaseError> {
        let held = AnchorIdentity::from_stat(
            fstat(&self.anchor).map_err(|error| lease_io("identify held writer lease", error))?,
        )?;
        let named = AnchorIdentity::from_stat(
            statat(
                root.descriptor(),
                OsStr::new("project.json"),
                AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(|error| lease_io("identify visible project envelope", error))?,
        )?;
        if held == self.identity && named == self.identity {
            Ok(())
        } else {
            Err(LeaseError::InvalidAnchor)
        }
    }
}

/// Whole-session shared maintenance lease plus an optional exclusive writer
/// lease. Field ownership releases both automatically on drop or process exit.
#[derive(Debug)]
pub(crate) struct ProjectStoreLeases {
    maintenance: OwnedFd,
    writer: Option<WriterLease>,
    writes_suspended: AtomicBool,
    maintenance_lost: AtomicBool,
    #[cfg(test)]
    gc_transition_injector: Option<Arc<GcTransitionInjector>>,
}

impl ProjectStoreLeases {
    pub(crate) fn acquire(
        root: &LocalStoreRoot,
        requested: ProjectOpenMode,
    ) -> Result<Self, LeaseError> {
        let maintenance = duplicate_root(root)?;
        lock_blocking(
            &maintenance,
            FlockOperation::LockShared,
            "acquire maintenance lease",
        )?;

        let writer = if requested == ProjectOpenMode::PreferWritable {
            let anchor = openat(
                root.descriptor(),
                OsStr::new("project.json"),
                ANCHOR_OPEN_FLAGS,
                Mode::empty(),
            )
            .map_err(|error| lease_io("open immutable project envelope for writer lease", error))?;
            let identity = AnchorIdentity::from_stat(
                fstat(&anchor).map_err(|error| lease_io("identify writer lease anchor", error))?,
            )?;
            match flock(&anchor, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => {
                    let writer = WriterLease { anchor, identity };
                    writer.confirm(root)?;
                    Some(writer)
                }
                Err(error) if error == Errno::AGAIN => None,
                Err(error) => return Err(lease_io("try to acquire writer lease", error)),
            }
        } else {
            None
        };

        Ok(Self {
            maintenance,
            writer,
            writes_suspended: AtomicBool::new(false),
            maintenance_lost: AtomicBool::new(false),
            #[cfg(test)]
            gc_transition_injector: None,
        })
    }

    pub(crate) const fn effective_mode(&self) -> ProjectOpenMode {
        if self.writer.is_some() {
            ProjectOpenMode::PreferWritable
        } else {
            ProjectOpenMode::ReadOnly
        }
    }

    pub(crate) const fn has_writer(&self) -> bool {
        self.writer.is_some()
    }

    pub(crate) fn confirm_writer(&self, root: &LocalStoreRoot) -> Result<bool, LeaseError> {
        if self.writes_suspended.load(Ordering::Acquire) {
            return Err(LeaseError::Indeterminate);
        }
        match &self.writer {
            Some(writer) => {
                writer.confirm(root)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn suspend_writes(&self) {
        self.writes_suspended.store(true, Ordering::Release);
    }

    pub(crate) fn maintenance_lost(&self) -> bool {
        self.maintenance_lost.load(Ordering::Acquire)
    }

    fn mark_maintenance_lost(&self) {
        self.maintenance_lost.store(true, Ordering::Release);
        self.suspend_writes();
    }

    #[cfg(test)]
    pub(crate) fn set_gc_transition_injector(&mut self, injector: Arc<GcTransitionInjector>) {
        self.gc_transition_injector = Some(injector);
    }

    pub(crate) fn gc_transition_before(
        &self,
        transition: GcTransition,
    ) -> Result<GcTransitionOccurrence, InjectedGcTransition> {
        #[cfg(test)]
        if let Some(injector) = &self.gc_transition_injector {
            return injector.before(transition);
        }
        Ok(GcTransitionOccurrence {
            transition,
            occurrence: 0,
        })
    }

    pub(crate) fn gc_transition_after(
        &self,
        occurrence: GcTransitionOccurrence,
    ) -> Result<(), InjectedGcTransition> {
        #[cfg(test)]
        if let Some(injector) = &self.gc_transition_injector {
            return injector.after(occurrence);
        }
        let _ = occurrence;
        Ok(())
    }

    pub(crate) fn with_exclusive_maintenance<C, F, T, E>(
        &mut self,
        root: &LocalStoreRoot,
        is_cancelled: &mut C,
        operation: F,
    ) -> Result<Result<T, E>, MaintenanceTransitionError>
    where
        C: FnMut() -> bool,
        F: FnOnce(&ProjectStoreLeases, &mut C) -> Result<T, E>,
    {
        let guard = self.wait_for_exclusive_maintenance(root, is_cancelled)?;
        let result = operation(guard.leases, is_cancelled);
        guard.finish()?;
        Ok(result)
    }

    fn wait_for_exclusive_maintenance<C>(
        &mut self,
        root: &LocalStoreRoot,
        is_cancelled: &mut C,
    ) -> Result<ExclusiveMaintenanceGuard<'_>, MaintenanceTransitionError>
    where
        C: FnMut() -> bool,
    {
        if !self.confirm_writer(root)? {
            return Err(MaintenanceTransitionError::ReadOnly);
        }
        if is_cancelled() {
            return Err(MaintenanceTransitionError::Cancelled);
        }

        loop {
            let occurrence = self
                .gc_transition_before(GcTransition::MaintenanceUpgrade)
                .map_err(|_| {
                    MaintenanceTransitionError::Lease(injected_lease_io(
                        "inject failure before maintenance upgrade",
                    ))
                })?;
            let upgrade = flock(&self.maintenance, FlockOperation::NonBlockingLockExclusive);
            let injected_after = self.gc_transition_after(occurrence).is_err();
            match upgrade {
                Ok(()) => {
                    if injected_after {
                        self.restore_shared_or_suspend()?;
                        return Err(MaintenanceTransitionError::Lease(injected_lease_io(
                            "inject failure after maintenance upgrade",
                        )));
                    }
                    if let Err(error) = self.confirm_writer(root) {
                        self.restore_shared_or_suspend()?;
                        return Err(MaintenanceTransitionError::Lease(error));
                    }
                    if is_cancelled() {
                        self.restore_shared_or_suspend()?;
                        return Err(MaintenanceTransitionError::Cancelled);
                    }
                    return Ok(ExclusiveMaintenanceGuard {
                        leases: self,
                        exclusive: true,
                    });
                }
                Err(error) => {
                    // Linux may discard the existing shared flock when a
                    // nonblocking in-place conversion fails. Restore shared
                    // ownership before observing cancellation or waiting.
                    self.restore_shared_or_suspend()?;
                    if injected_after {
                        return Err(MaintenanceTransitionError::Lease(injected_lease_io(
                            "inject failure after maintenance upgrade",
                        )));
                    }
                    match error {
                        Errno::AGAIN => {
                            if is_cancelled() {
                                return Err(MaintenanceTransitionError::Cancelled);
                            }
                            thread::sleep(MAINTENANCE_UPGRADE_POLL_INTERVAL);
                        }
                        Errno::INTR => {
                            if is_cancelled() {
                                return Err(MaintenanceTransitionError::Cancelled);
                            }
                        }
                        error => {
                            return Err(MaintenanceTransitionError::Lease(lease_io(
                                "upgrade maintenance lease",
                                error,
                            )));
                        }
                    }
                }
            }
        }
    }

    fn restore_shared(&self) -> Result<(), LeaseError> {
        lock_blocking(
            &self.maintenance,
            FlockOperation::LockShared,
            "restore shared maintenance lease",
        )
    }

    fn restore_shared_or_suspend(&self) -> Result<(), MaintenanceTransitionError> {
        let occurrence = self
            .gc_transition_before(GcTransition::MaintenanceRestore)
            .map_err(|_| {
                let source = injected_lease_io("inject failure before maintenance restore");
                self.mark_maintenance_lost();
                MaintenanceTransitionError::MaintenanceLost { source }
            })?;
        let restored = self.restore_shared();
        let injected_after = self.gc_transition_after(occurrence).is_err();
        match (restored, injected_after) {
            (Ok(()), false) => Ok(()),
            (Ok(()), true) => {
                let source = injected_lease_io("inject failure after maintenance restore");
                self.suspend_writes();
                Err(MaintenanceTransitionError::MaintenanceRestoredIndeterminate { source })
            }
            (Err(source), _) => {
                self.mark_maintenance_lost();
                Err(MaintenanceTransitionError::MaintenanceLost { source })
            }
        }
    }
}

/// Exclusive maintenance capability over the session's existing descriptor.
/// Explicit restoration is the normal path; `Drop` is only a safety fallback.
#[must_use = "call finish() on every normal exit so restoration failures remain observable"]
#[derive(Debug)]
struct ExclusiveMaintenanceGuard<'a> {
    leases: &'a mut ProjectStoreLeases,
    exclusive: bool,
}

impl ExclusiveMaintenanceGuard<'_> {
    fn finish(mut self) -> Result<(), MaintenanceTransitionError> {
        match self.leases.restore_shared_or_suspend() {
            Ok(()) => {
                self.exclusive = false;
                Ok(())
            }
            Err(error @ MaintenanceTransitionError::MaintenanceRestoredIndeterminate { .. }) => {
                self.exclusive = false;
                Err(error)
            }
            Err(error) => Err(error),
        }
    }
}

impl Drop for ExclusiveMaintenanceGuard<'_> {
    fn drop(&mut self) {
        if self.exclusive {
            let _ = self.leases.restore_shared_or_suspend();
            self.exclusive = false;
        }
    }
}

fn duplicate_root(root: &LocalStoreRoot) -> Result<OwnedFd, LeaseError> {
    openat(
        root.descriptor(),
        Path::new("."),
        DIRECTORY_OPEN_FLAGS,
        Mode::empty(),
    )
    .map_err(|error| lease_io("duplicate project root for maintenance lease", error))
}

fn lock_blocking(
    descriptor: &OwnedFd,
    operation: FlockOperation,
    label: &'static str,
) -> Result<(), LeaseError> {
    loop {
        match flock(descriptor, operation) {
            Ok(()) => return Ok(()),
            Err(Errno::INTR) => {}
            Err(error) => return Err(lease_io(label, error)),
        }
    }
}

fn lease_io(operation: &'static str, error: Errno) -> LeaseError {
    LeaseError::Io {
        operation,
        source: io::Error::from(error),
    }
}

fn injected_lease_io(operation: &'static str) -> LeaseError {
    LeaseError::Io {
        operation,
        source: io::Error::other("injected project-store transition failure"),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        env, fs,
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        sync::atomic::{AtomicU64, Ordering},
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use mirante4d_project_model::ProjectId;

    use super::*;
    use crate::wire::ProjectEnvelope;

    const CHILD_ROOT: &str = "MIRANTE4D_LEASE_TEST_CHILD_ROOT";
    const CHILD_DEADLINE: Duration = Duration::from_secs(10);
    const TEST_NAME: &str = concat!(
        "lease::tests::",
        "process_contention_preserves_shared_maintenance_and_single_writer_ownership"
    );

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = env::temp_dir().join(format!(
                "mirante4d-project-lease-{}-{nonce}-{}",
                std::process::id(),
                TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            let project_id = ProjectId::parse("11111111-2222-4333-8444-555555555555").unwrap();
            fs::write(
                path.join("project.json"),
                ProjectEnvelope::new(project_id).encode().unwrap(),
            )
            .unwrap();
            Self(path)
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
        fn child_mut(&mut self) -> &mut Child {
            self.0.as_mut().unwrap()
        }

        fn terminate(&mut self) {
            let Some(mut child) = self.0.take() else {
                return;
            };
            if child.try_wait().unwrap().is_none() {
                child.kill().unwrap();
            }
            let _ = child.wait().unwrap();
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

    fn independent_root_lock_available(root: &LocalStoreRoot, operation: FlockOperation) -> bool {
        let descriptor = duplicate_root(root).unwrap();
        match flock(&descriptor, operation) {
            Ok(()) => true,
            Err(Errno::AGAIN) => false,
            Err(error) => panic!("independent root-lock probe failed: {error}"),
        }
    }

    fn independent_shared_available(root: &LocalStoreRoot) -> bool {
        independent_root_lock_available(root, FlockOperation::NonBlockingLockShared)
    }

    fn independent_exclusive_available(root: &LocalStoreRoot) -> bool {
        independent_root_lock_available(root, FlockOperation::NonBlockingLockExclusive)
    }

    fn independent_writer_available(root: &LocalStoreRoot) -> bool {
        let anchor = openat(
            root.descriptor(),
            OsStr::new("project.json"),
            ANCHOR_OPEN_FLAGS,
            Mode::empty(),
        )
        .unwrap();
        match flock(&anchor, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => true,
            Err(Errno::AGAIN) => false,
            Err(error) => panic!("independent writer-lock probe failed: {error}"),
        }
    }

    #[test]
    fn process_contention_preserves_shared_maintenance_and_single_writer_ownership() {
        if let Some(root) = env::var_os(CHILD_ROOT) {
            let root_path = PathBuf::from(root);
            let root = LocalStoreRoot::open(&root_path).unwrap();
            let leases =
                ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
            assert_eq!(leases.effective_mode(), ProjectOpenMode::ReadOnly);
            fs::write(root_path.join("child-ready"), b"ready").unwrap();
            let deadline = Instant::now() + CHILD_DEADLINE;
            while Instant::now() < deadline {
                thread::sleep(Duration::from_secs(1));
            }
            drop(leases);
            return;
        }

        let directory = TestDirectory::new();
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let mut leases =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        assert_eq!(leases.effective_mode(), ProjectOpenMode::PreferWritable);
        assert!(leases.confirm_writer(&root).unwrap());

        let mut child = ChildGuard(Some(
            Command::new(env::current_exe().unwrap())
                .arg(TEST_NAME)
                .arg("--exact")
                .arg("--nocapture")
                .env(CHILD_ROOT, directory.path())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        ));
        let ready = directory.path().join("child-ready");
        let deadline = Instant::now() + CHILD_DEADLINE;
        while !ready.exists() {
            assert!(child.child_mut().try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        let mut cancellation_checks = 0_u8;
        let mut cancel_after_contention = || {
            cancellation_checks += 1;
            cancellation_checks >= 2
        };
        assert!(matches!(
            leases.wait_for_exclusive_maintenance(&root, &mut cancel_after_contention),
            Err(MaintenanceTransitionError::Cancelled)
        ));
        assert!(child.child_mut().try_wait().unwrap().is_none());
        child.terminate();
        assert!(independent_shared_available(&root));
        assert!(!independent_exclusive_available(&root));
        assert!(!independent_writer_available(&root));
        assert!(leases.confirm_writer(&root).unwrap());

        let contender = duplicate_root(&root).unwrap();
        flock(&contender, FlockOperation::LockShared).unwrap();
        let mut contender = Some(contender);
        let mut wait_checks = 0_u8;
        let mut release_after_contention = || {
            wait_checks += 1;
            if wait_checks == 2 {
                drop(contender.take());
            }
            false
        };
        leases
            .with_exclusive_maintenance(&root, &mut release_after_contention, |_, _| {
                assert!(!independent_shared_available(&root));
                assert!(!independent_exclusive_available(&root));
                assert!(!independent_writer_available(&root));
                Ok::<(), ()>(())
            })
            .unwrap()
            .unwrap();
        assert!(wait_checks >= 3);
        assert!(independent_shared_available(&root));
        assert!(!independent_exclusive_available(&root));
        assert!(!independent_writer_available(&root));
        assert!(leases.confirm_writer(&root).unwrap());

        let mut never_cancel = || false;
        let operation_error = leases
            .with_exclusive_maintenance(&root, &mut never_cancel, |_, _| {
                Err::<(), _>("operation failed")
            })
            .unwrap();
        assert_eq!(operation_error, Err("operation failed"));
        assert!(independent_shared_available(&root));
        assert!(!independent_exclusive_available(&root));

        {
            let _guard = leases
                .wait_for_exclusive_maintenance(&root, &mut never_cancel)
                .unwrap();
            assert!(!independent_shared_available(&root));
            assert!(!independent_exclusive_available(&root));
        }
        assert!(independent_shared_available(&root));
        assert!(!independent_exclusive_available(&root));
        assert!(!independent_writer_available(&root));
        assert!(leases.confirm_writer(&root).unwrap());

        let mut cancel_now = || true;
        assert!(matches!(
            leases.wait_for_exclusive_maintenance(&root, &mut cancel_now),
            Err(MaintenanceTransitionError::Cancelled)
        ));
        assert!(independent_shared_available(&root));
        assert!(!independent_exclusive_available(&root));
        assert!(!independent_writer_available(&root));
        assert!(leases.confirm_writer(&root).unwrap());

        let mut post_upgrade_checks = 0_u8;
        let mut cancel_after_upgrade = || {
            post_upgrade_checks += 1;
            post_upgrade_checks >= 2
        };
        assert!(matches!(
            leases.wait_for_exclusive_maintenance(&root, &mut cancel_after_upgrade),
            Err(MaintenanceTransitionError::Cancelled)
        ));
        assert!(independent_shared_available(&root));
        assert!(!independent_exclusive_available(&root));
        assert!(leases.confirm_writer(&root).unwrap());

        leases.suspend_writes();
        assert!(matches!(
            leases.wait_for_exclusive_maintenance(&root, &mut never_cancel),
            Err(MaintenanceTransitionError::Lease(LeaseError::Indeterminate))
        ));
        assert!(independent_shared_available(&root));
        assert!(!independent_exclusive_available(&root));
        drop(leases);
        assert!(independent_exclusive_available(&root));
        assert!(independent_writer_available(&root));

        let mut read_only = ProjectStoreLeases::acquire(&root, ProjectOpenMode::ReadOnly).unwrap();
        assert!(matches!(
            read_only.wait_for_exclusive_maintenance(&root, &mut never_cancel),
            Err(MaintenanceTransitionError::ReadOnly)
        ));
        assert!(independent_shared_available(&root));
        assert!(!independent_exclusive_available(&root));
        drop(read_only);
        assert!(independent_exclusive_available(&root));
    }
}

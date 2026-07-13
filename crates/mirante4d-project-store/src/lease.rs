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

    pub(crate) fn with_exclusive_maintenance<C, F, T, E>(
        &mut self,
        root: &LocalStoreRoot,
        is_cancelled: &mut C,
        operation: F,
    ) -> Result<Result<T, E>, MaintenanceTransitionError>
    where
        C: FnMut() -> bool,
        F: FnOnce(&mut C) -> Result<T, E>,
    {
        let guard = self.wait_for_exclusive_maintenance(root, is_cancelled)?;
        let result = operation(is_cancelled);
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
            match flock(&self.maintenance, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => {
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
        self.restore_shared().map_err(|source| {
            self.suspend_writes();
            MaintenanceTransitionError::MaintenanceLost { source }
        })
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
            Err(error) => Err(error),
        }
    }
}

impl Drop for ExclusiveMaintenanceGuard<'_> {
    fn drop(&mut self) {
        if self.exclusive {
            if self.leases.restore_shared().is_err() {
                self.leases.suspend_writes();
            }
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
            .with_exclusive_maintenance(&root, &mut release_after_contention, |_| {
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
            .with_exclusive_maintenance(&root, &mut never_cancel, |_| {
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

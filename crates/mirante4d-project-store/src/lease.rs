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
    _maintenance: OwnedFd,
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
            _maintenance: maintenance,
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
}

/// Exclusive maintenance capability reserved for later compaction.
#[derive(Debug)]
pub(crate) struct ExclusiveMaintenanceLease {
    _root: OwnedFd,
}

impl ExclusiveMaintenanceLease {
    pub(crate) fn try_acquire(root: &LocalStoreRoot) -> Result<Option<Self>, LeaseError> {
        let anchor = duplicate_root(root)?;
        match flock(&anchor, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Some(Self { _root: anchor })),
            Err(error) if error == Errno::AGAIN => Ok(None),
            Err(error) => Err(lease_io(
                "try to acquire exclusive maintenance lease",
                error,
            )),
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
        process::{Command, Stdio},
        sync::atomic::{AtomicU64, Ordering},
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use mirante4d_project_model::ProjectId;

    use super::*;
    use crate::wire::ProjectEnvelope;

    const CHILD_ROOT: &str = "MIRANTE4D_LEASE_TEST_CHILD_ROOT";
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

    #[test]
    fn process_contention_preserves_shared_maintenance_and_single_writer_ownership() {
        if let Some(root) = env::var_os(CHILD_ROOT) {
            let root_path = PathBuf::from(root);
            let root = LocalStoreRoot::open(&root_path).unwrap();
            let leases =
                ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
            assert_eq!(leases.effective_mode(), ProjectOpenMode::ReadOnly);
            fs::write(root_path.join("child-ready"), b"ready").unwrap();
            loop {
                thread::sleep(Duration::from_secs(1));
            }
        }

        let directory = TestDirectory::new();
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let first = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        assert_eq!(first.effective_mode(), ProjectOpenMode::PreferWritable);
        assert!(first.confirm_writer(&root).unwrap());

        let mut child = Command::new(env::current_exe().unwrap())
            .arg(TEST_NAME)
            .arg("--exact")
            .arg("--nocapture")
            .env(CHILD_ROOT, directory.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let ready = directory.path().join("child-ready");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            assert!(
                Instant::now() < deadline,
                "lease child did not become ready"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            ExclusiveMaintenanceLease::try_acquire(&root)
                .unwrap()
                .is_none()
        );

        drop(first);
        assert!(
            ExclusiveMaintenanceLease::try_acquire(&root)
                .unwrap()
                .is_none(),
            "the child alone still holds shared maintenance"
        );
        child.kill().unwrap();
        let _ = child.wait().unwrap();
        let exclusive = ExclusiveMaintenanceLease::try_acquire(&root)
            .unwrap()
            .expect("all shared maintenance leases were released");
        drop(exclusive);
        let replacement =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(replacement.has_writer());
    }
}

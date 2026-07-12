//! Descriptor-relative publication of immutable project-store files.
//!
//! The caller supplies an already-open project root. Every operation below
//! that root is relative to a held directory descriptor and rejects symlink
//! traversal with `O_NOFOLLOW`. This first primitive publishes only exact
//! immutable objects; typed complete-generation publication is added with the
//! transaction layer. It owns no refs, leases, actor, recovery, or GC behavior.
//! The transaction caller must hold the writer lease; this primitive does not
//! claim protection from an out-of-protocol same-user process reparenting the
//! store namespace while its directory descriptors are open.

#![cfg(target_os = "linux")]
#![cfg_attr(not(test), allow(dead_code))]

use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use mirante4d_identity::{ExactBytesDigest, ExactBytesFacts, ExactBytesHasher};
use rustix::{
    fd::OwnedFd,
    fs::{
        AtFlags, CWD, FileType, Mode, OFlags, RenameFlags, fstat, fsync, mkdirat, openat,
        renameat_with, statat, unlinkat,
    },
    io::Errno,
};
use thiserror::Error;

const STREAM_BUFFER_BYTES: usize = 1024 * 1024;
const STAGE_CREATE_ATTEMPTS: u64 = 128;
const STAGING_DIRECTORY: &str = "staging";
const STAGED_FILE: &str = "payload";

static STAGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const DIRECTORY_OPEN_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW);
const FILE_READ_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW);
const FILE_CREATE_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW);

/// One held project-store root used for descriptor-relative immutable writes.
#[derive(Debug)]
pub(crate) struct LocalStoreRoot {
    root: OwnedFd,
}

/// Whether immutable publication created a file or validated an identical one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublicationDisposition {
    Created,
    AlreadyPresent,
}

/// Exact facts and collision disposition for one immutable publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ImmutablePublication {
    facts: ExactBytesFacts,
    disposition: PublicationDisposition,
}

impl ImmutablePublication {
    pub(crate) const fn facts(self) -> ExactBytesFacts {
        self.facts
    }

    pub(crate) const fn disposition(self) -> PublicationDisposition {
        self.disposition
    }
}

#[derive(Debug, Error)]
pub(crate) enum LocalPublicationError {
    #[error("immutable publication was cancelled")]
    Cancelled,
    #[error("the project-store root or immutable destination path is invalid")]
    InvalidPath,
    #[error("the declared file length {declared} exceeds the {maximum}-byte limit")]
    Capacity { declared: u64, maximum: u64 },
    #[error("the source length differs from its declared {expected} bytes")]
    SourceLength { expected: u64 },
    #[error("the source exact-byte digest differs from its declared identity")]
    SourceDigest,
    #[error("an existing immutable destination is not the declared exact file")]
    ExistingMismatch,
    #[error("the filesystem cannot publish an immutable file without replacement")]
    AtomicPublishUnsupported,
    #[error("project-store I/O failed while attempting to {operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
}

fn object_relative_path(digest: ExactBytesDigest) -> PathBuf {
    let digest = digest.digest().to_string();
    PathBuf::from("objects")
        .join("sha256")
        .join(&digest[..2])
        .join(&digest[2..])
}

impl LocalStoreRoot {
    /// Opens every component of an existing root through the previously held
    /// directory descriptor. `..` and platform prefixes are rejected.
    pub(crate) fn open(path: &Path) -> Result<Self, LocalPublicationError> {
        if path.as_os_str().is_empty() {
            return Err(LocalPublicationError::InvalidPath);
        }
        let mut current = if path.is_absolute() {
            openat(CWD, Path::new("/"), DIRECTORY_OPEN_FLAGS, Mode::empty())
                .map_err(|error| io_error("open the filesystem root", error))?
        } else {
            openat(CWD, Path::new("."), DIRECTORY_OPEN_FLAGS, Mode::empty())
                .map_err(|error| io_error("open the current directory", error))?
        };
        for component in path.components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(name) => {
                    current = openat(&current, name, DIRECTORY_OPEN_FLAGS, Mode::empty())
                        .map_err(|error| io_error("open a project-store root component", error))?;
                }
                Component::ParentDir | Component::Prefix(_) => {
                    return Err(LocalPublicationError::InvalidPath);
                }
            }
        }
        Ok(Self { root: current })
    }

    pub(crate) fn publish_object<R, C>(
        &self,
        source: &mut R,
        expected: ExactBytesFacts,
        maximum_bytes: u64,
        is_cancelled: C,
    ) -> Result<ImmutablePublication, LocalPublicationError>
    where
        R: Read + ?Sized,
        C: FnMut() -> bool,
    {
        self.publish(
            object_relative_path(expected.digest()),
            source,
            expected,
            maximum_bytes,
            is_cancelled,
        )
    }

    fn publish<R, C>(
        &self,
        destination: PathBuf,
        source: &mut R,
        expected: ExactBytesFacts,
        maximum_bytes: u64,
        mut is_cancelled: C,
    ) -> Result<ImmutablePublication, LocalPublicationError>
    where
        R: Read + ?Sized,
        C: FnMut() -> bool,
    {
        let expected_bytes = expected.byte_length();
        let expected_digest = expected.digest();
        if expected_bytes > maximum_bytes {
            return Err(LocalPublicationError::Capacity {
                declared: expected_bytes,
                maximum: maximum_bytes,
            });
        }
        check_cancelled(&mut is_cancelled)?;

        let (destination_parent, destination_name) = self.open_or_create_parent(&destination)?;
        if validate_existing_if_present(
            &destination_parent,
            &destination_name,
            expected,
            &mut is_cancelled,
        )? {
            return Ok(ImmutablePublication {
                facts: expected,
                disposition: PublicationDisposition::AlreadyPresent,
            });
        }
        let mut stage = Stage::begin(self)?;
        let mut staged_file = stage.create_file()?;
        let facts =
            write_exact_source(&mut staged_file, source, expected_bytes, &mut is_cancelled)?;
        if expected_digest != facts.digest() {
            return Err(LocalPublicationError::SourceDigest);
        }
        check_cancelled(&mut is_cancelled)?;
        fsync(&staged_file)
            .map_err(|error| io_error("synchronize a staged immutable file", error))?;
        drop(staged_file);
        check_cancelled(&mut is_cancelled)?;

        match renameat_with(
            &stage.directory,
            OsStr::new(STAGED_FILE),
            &destination_parent,
            &destination_name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {
                stage.file_owned = false;
                fsync(&destination_parent).map_err(|error| {
                    io_error("synchronize an immutable destination directory", error)
                })?;
                Ok(ImmutablePublication {
                    facts,
                    disposition: PublicationDisposition::Created,
                })
            }
            Err(Errno::EXIST) => {
                if !validate_existing_if_present(
                    &destination_parent,
                    &destination_name,
                    facts,
                    &mut is_cancelled,
                )? {
                    return Err(LocalPublicationError::ExistingMismatch);
                }
                stage.remove_file()?;
                Ok(ImmutablePublication {
                    facts,
                    disposition: PublicationDisposition::AlreadyPresent,
                })
            }
            Err(error)
                if error == Errno::NOSYS || error == Errno::INVAL || error == Errno::OPNOTSUPP =>
            {
                Err(LocalPublicationError::AtomicPublishUnsupported)
            }
            Err(error) => Err(io_error(
                "publish an immutable file without replacement",
                error,
            )),
        }
    }

    fn open_or_create_parent(
        &self,
        path: &Path,
    ) -> Result<(OwnedFd, OsString), LocalPublicationError> {
        let mut components = normal_components(path)?;
        let file_name = components.pop().ok_or(LocalPublicationError::InvalidPath)?;
        let mut current = duplicate_directory(&self.root)?;
        for component in components {
            current = open_or_create_directory(&current, &component)?;
        }
        Ok((current, file_name))
    }
}

struct Stage {
    parent: OwnedFd,
    name: OsString,
    directory: OwnedFd,
    identity: DirectoryIdentity,
    file_owned: bool,
}

impl Stage {
    fn begin(root: &LocalStoreRoot) -> Result<Self, LocalPublicationError> {
        let staging = open_or_create_directory(&root.root, OsStr::new(STAGING_DIRECTORY))?;
        for attempt in 0..STAGE_CREATE_ATTEMPTS {
            let name = unique_stage_name(attempt);
            match mkdirat(&staging, &name, Mode::RWXU) {
                Ok(()) => {
                    let directory = openat(&staging, &name, DIRECTORY_OPEN_FLAGS, Mode::empty())
                        .map_err(|error| io_error("open a private staging directory", error))?;
                    let identity =
                        DirectoryIdentity::from_stat(fstat(&directory).map_err(|error| {
                            io_error("identify a private staging directory", error)
                        })?)?;
                    return Ok(Self {
                        parent: staging,
                        name,
                        directory,
                        identity,
                        file_owned: false,
                    });
                }
                Err(Errno::EXIST) => continue,
                Err(error) => return Err(io_error("create a private staging directory", error)),
            }
        }
        Err(LocalPublicationError::Io {
            operation: "create a unique private staging directory",
            source: io::Error::new(
                io::ErrorKind::AlreadyExists,
                "all bounded staging-name attempts collided",
            ),
        })
    }

    fn create_file(&mut self) -> Result<File, LocalPublicationError> {
        let descriptor = openat(
            &self.directory,
            OsStr::new(STAGED_FILE),
            FILE_CREATE_FLAGS,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| io_error("create a staged immutable file", error))?;
        self.file_owned = true;
        Ok(File::from(descriptor))
    }

    fn remove_file(&mut self) -> Result<(), LocalPublicationError> {
        if self.file_owned {
            unlinkat(&self.directory, OsStr::new(STAGED_FILE), AtFlags::empty())
                .map_err(|error| io_error("remove a redundant staged immutable file", error))?;
            self.file_owned = false;
        }
        Ok(())
    }

    fn name_still_owned(&self) -> bool {
        statat(&self.parent, &self.name, AtFlags::SYMLINK_NOFOLLOW)
            .ok()
            .and_then(|stat| DirectoryIdentity::from_stat(stat).ok())
            == Some(self.identity)
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        if self.file_owned {
            let _ = unlinkat(&self.directory, OsStr::new(STAGED_FILE), AtFlags::empty());
        }
        if self.name_still_owned() {
            let _ = unlinkat(&self.parent, &self.name, AtFlags::REMOVEDIR);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

impl DirectoryIdentity {
    fn from_stat(stat: rustix::fs::Stat) -> Result<Self, LocalPublicationError> {
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
            return Err(LocalPublicationError::ExistingMismatch);
        }
        Ok(Self {
            device: stat.st_dev,
            inode: stat.st_ino,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    bytes: u64,
    links: u64,
}

impl FileIdentity {
    fn from_stat(stat: rustix::fs::Stat) -> Result<Self, LocalPublicationError> {
        let bytes =
            u64::try_from(stat.st_size).map_err(|_| LocalPublicationError::ExistingMismatch)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
            return Err(LocalPublicationError::ExistingMismatch);
        }
        Ok(Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            bytes,
            links: stat.st_nlink,
        })
    }
}

fn write_exact_source<R, C>(
    file: &mut File,
    source: &mut R,
    expected_bytes: u64,
    is_cancelled: &mut C,
) -> Result<ExactBytesFacts, LocalPublicationError>
where
    R: Read + ?Sized,
    C: FnMut() -> bool,
{
    let mut hasher = ExactBytesHasher::new();
    let mut observed = 0_u64;
    let mut buffer = vec![0_u8; STREAM_BUFFER_BYTES];
    loop {
        check_cancelled(is_cancelled)?;
        let read = match source.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(LocalPublicationError::Io {
                    operation: "read an immutable source",
                    source,
                });
            }
        };
        if read == 0 {
            break;
        }
        let added = u64::try_from(read).map_err(|_| LocalPublicationError::SourceLength {
            expected: expected_bytes,
        })?;
        observed = observed
            .checked_add(added)
            .ok_or(LocalPublicationError::SourceLength {
                expected: expected_bytes,
            })?;
        if observed > expected_bytes {
            return Err(LocalPublicationError::SourceLength {
                expected: expected_bytes,
            });
        }
        file.write_all(&buffer[..read])
            .map_err(|source| LocalPublicationError::Io {
                operation: "write a staged immutable file",
                source,
            })?;
        hasher
            .update(&buffer[..read])
            .map_err(|_| LocalPublicationError::SourceLength {
                expected: expected_bytes,
            })?;
    }
    if observed != expected_bytes {
        return Err(LocalPublicationError::SourceLength {
            expected: expected_bytes,
        });
    }
    hasher
        .finalize()
        .map_err(|_| LocalPublicationError::SourceLength {
            expected: expected_bytes,
        })
}

fn validate_existing_if_present<C>(
    parent: &OwnedFd,
    name: &OsStr,
    expected: ExactBytesFacts,
    is_cancelled: &mut C,
) -> Result<bool, LocalPublicationError>
where
    C: FnMut() -> bool,
{
    check_cancelled(is_cancelled)?;
    let descriptor = match openat(parent, name, FILE_READ_FLAGS, Mode::empty()) {
        Ok(descriptor) => descriptor,
        Err(Errno::NOENT) => return Ok(false),
        Err(_) => return Err(LocalPublicationError::ExistingMismatch),
    };
    let before = FileIdentity::from_stat(
        fstat(&descriptor).map_err(|_| LocalPublicationError::ExistingMismatch)?,
    )?;
    if before.bytes != expected.byte_length() {
        return Err(LocalPublicationError::ExistingMismatch);
    }

    let mut file = File::from(descriptor);
    let facts = hash_exact_file(&mut file, expected.byte_length(), is_cancelled)?;
    check_cancelled(is_cancelled)?;
    let after = FileIdentity::from_stat(
        fstat(&file).map_err(|_| LocalPublicationError::ExistingMismatch)?,
    )?;
    let named = FileIdentity::from_stat(
        statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| LocalPublicationError::ExistingMismatch)?,
    )?;
    if before != after
        || before != named
        || facts.byte_length() != expected.byte_length()
        || facts.digest() != expected.digest()
    {
        return Err(LocalPublicationError::ExistingMismatch);
    }
    Ok(true)
}

fn hash_exact_file<C>(
    file: &mut File,
    expected_bytes: u64,
    is_cancelled: &mut C,
) -> Result<ExactBytesFacts, LocalPublicationError>
where
    C: FnMut() -> bool,
{
    let mut hasher = ExactBytesHasher::new();
    let mut observed = 0_u64;
    let mut buffer = vec![0_u8; STREAM_BUFFER_BYTES];
    loop {
        check_cancelled(is_cancelled)?;
        let read = match file.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(LocalPublicationError::ExistingMismatch),
        };
        if read == 0 {
            break;
        }
        observed = observed
            .checked_add(u64::try_from(read).map_err(|_| LocalPublicationError::ExistingMismatch)?)
            .ok_or(LocalPublicationError::ExistingMismatch)?;
        if observed > expected_bytes {
            return Err(LocalPublicationError::ExistingMismatch);
        }
        hasher
            .update(&buffer[..read])
            .map_err(|_| LocalPublicationError::ExistingMismatch)?;
    }
    if observed != expected_bytes {
        return Err(LocalPublicationError::ExistingMismatch);
    }
    hasher
        .finalize()
        .map_err(|_| LocalPublicationError::ExistingMismatch)
}

fn duplicate_directory(directory: &OwnedFd) -> Result<OwnedFd, LocalPublicationError> {
    openat(
        directory,
        Path::new("."),
        DIRECTORY_OPEN_FLAGS,
        Mode::empty(),
    )
    .map_err(|error| io_error("duplicate a held project-store directory", error))
}

fn open_or_create_directory(
    parent: &OwnedFd,
    name: &OsStr,
) -> Result<OwnedFd, LocalPublicationError> {
    match openat(parent, name, DIRECTORY_OPEN_FLAGS, Mode::empty()) {
        Ok(directory) => Ok(directory),
        Err(Errno::NOENT) => {
            match mkdirat(parent, name, Mode::RWXU) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(error) => return Err(io_error("create a project-store directory", error)),
            }
            let directory = openat(parent, name, DIRECTORY_OPEN_FLAGS, Mode::empty())
                .map_err(|error| io_error("open a created project-store directory", error))?;
            fsync(parent)
                .map_err(|error| io_error("synchronize a project-store parent directory", error))?;
            Ok(directory)
        }
        Err(error) => Err(io_error("open a project-store directory", error)),
    }
}

fn normal_components(path: &Path) -> Result<Vec<OsString>, LocalPublicationError> {
    let components = path
        .components()
        .map(|component| match component {
            Component::Normal(value) => Ok(value.to_os_string()),
            _ => Err(LocalPublicationError::InvalidPath),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if components.is_empty() {
        Err(LocalPublicationError::InvalidPath)
    } else {
        Ok(components)
    }
}

fn unique_stage_name(attempt: u64) -> OsString {
    let sequence = STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "tx-{}-{timestamp:032x}-{sequence:016x}-{attempt:02x}",
        std::process::id()
    )
    .into()
}

fn check_cancelled(is_cancelled: &mut impl FnMut() -> bool) -> Result<(), LocalPublicationError> {
    if is_cancelled() {
        Err(LocalPublicationError::Cancelled)
    } else {
        Ok(())
    }
}

fn io_error(operation: &'static str, error: Errno) -> LocalPublicationError {
    LocalPublicationError::Io {
        operation,
        source: io::Error::from(error),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Cursor,
        os::unix::fs::symlink,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    struct MustNotRead;

    impl Read for MustNotRead {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            panic!("deduplicated source was read")
        }
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "mirante4d-project-local-{label}-{}-{nonce}-{}",
                std::process::id(),
                TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
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

    fn facts(bytes: &[u8]) -> ExactBytesFacts {
        ExactBytesHasher::hash(bytes).unwrap()
    }

    fn object_path(root: &Path, digest: ExactBytesDigest) -> PathBuf {
        root.join(object_relative_path(digest))
    }

    #[test]
    fn opens_every_root_component_without_following_symlinks() {
        let directory = TestDirectory::new("root-nofollow");
        let real = directory.path().join("real");
        let store = real.join("store");
        fs::create_dir(&real).unwrap();
        fs::create_dir(&store).unwrap();
        let linked = directory.path().join("linked");
        symlink(&real, &linked).unwrap();

        assert!(LocalStoreRoot::open(&store).is_ok());
        assert!(matches!(
            LocalStoreRoot::open(&linked.join("store")),
            Err(LocalPublicationError::Io { .. })
        ));
        assert!(matches!(
            LocalStoreRoot::open(Path::new("../outside")),
            Err(LocalPublicationError::InvalidPath)
        ));
    }

    #[test]
    fn publishes_and_deduplicates_an_exact_object_with_bounded_staging() {
        let directory = TestDirectory::new("object");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let bytes = (0..(STREAM_BUFFER_BYTES + 17))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let expected = facts(&bytes);

        let first = root
            .publish_object(&mut Cursor::new(&bytes), expected, 16 * 1024 * 1024, || {
                false
            })
            .unwrap();
        assert_eq!(first.facts(), expected);
        assert_eq!(first.disposition(), PublicationDisposition::Created);
        assert_eq!(
            fs::read(object_path(directory.path(), expected.digest())).unwrap(),
            bytes
        );

        let second = root
            .publish_object(&mut MustNotRead, expected, 16 * 1024 * 1024, || false)
            .unwrap();
        assert_eq!(second.disposition(), PublicationDisposition::AlreadyPresent);
        let mut polls = 0;
        assert!(matches!(
            root.publish_object(&mut MustNotRead, expected, 16 * 1024 * 1024, || {
                polls += 1;
                polls >= 3
            }),
            Err(LocalPublicationError::Cancelled)
        ));
        assert_eq!(
            fs::read_dir(directory.path().join(STAGING_DIRECTORY))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn refuses_changed_hardlinked_and_symlinked_existing_destinations() {
        let directory = TestDirectory::new("collisions");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let bytes = b"immutable object";
        let expected = facts(bytes);
        root.publish_object(&mut Cursor::new(bytes), expected, 16 * 1024 * 1024, || {
            false
        })
        .unwrap();
        let path = object_path(directory.path(), expected.digest());

        fs::write(&path, b"changed object!").unwrap();
        assert!(matches!(
            root.publish_object(&mut Cursor::new(bytes), expected, 16 * 1024 * 1024, || {
                false
            }),
            Err(LocalPublicationError::ExistingMismatch)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"changed object!");

        fs::remove_file(&path).unwrap();
        fs::write(&path, bytes).unwrap();
        let hardlink = directory.path().join("hardlink");
        fs::hard_link(&path, &hardlink).unwrap();
        assert!(matches!(
            root.publish_object(&mut Cursor::new(bytes), expected, 16 * 1024 * 1024, || {
                false
            }),
            Err(LocalPublicationError::ExistingMismatch)
        ));
        fs::remove_file(hardlink).unwrap();

        fs::remove_file(&path).unwrap();
        let outside = directory.path().join("outside");
        fs::write(&outside, b"outside sentinel").unwrap();
        symlink(&outside, &path).unwrap();
        assert!(matches!(
            root.publish_object(&mut Cursor::new(bytes), expected, 16 * 1024 * 1024, || {
                false
            }),
            Err(LocalPublicationError::ExistingMismatch)
        ));
        assert_eq!(fs::read(outside).unwrap(), b"outside sentinel");
    }

    #[test]
    fn source_capacity_length_digest_and_cancellation_fail_before_publication() {
        let directory = TestDirectory::new("source-failures");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let bytes = b"declared bytes";
        let expected = facts(bytes);
        let path = object_path(directory.path(), expected.digest());

        assert!(matches!(
            root.publish_object(&mut Cursor::new(bytes), expected, 1, || false),
            Err(LocalPublicationError::Capacity { .. })
        ));
        assert!(matches!(
            root.publish_object(
                &mut Cursor::new(&bytes[..bytes.len() - 1]),
                expected,
                1024,
                || false
            ),
            Err(LocalPublicationError::SourceLength { .. })
        ));
        let mut longer = bytes.to_vec();
        longer.push(0);
        assert!(matches!(
            root.publish_object(&mut Cursor::new(longer), expected, 1024, || false),
            Err(LocalPublicationError::SourceLength { .. })
        ));
        let wrong = facts(b"same byte len!");
        assert_eq!(wrong.byte_length(), expected.byte_length());
        assert!(matches!(
            root.publish_object(&mut Cursor::new(bytes), wrong, 1024, || false),
            Err(LocalPublicationError::SourceDigest)
        ));
        assert!(matches!(
            root.publish_object(&mut Cursor::new(bytes), expected, 1024, || true),
            Err(LocalPublicationError::Cancelled)
        ));
        assert!(!path.exists());
        assert_eq!(
            fs::read_dir(directory.path().join(STAGING_DIRECTORY))
                .unwrap()
                .count(),
            0
        );
    }
}

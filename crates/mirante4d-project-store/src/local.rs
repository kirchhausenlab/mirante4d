//! Descriptor-relative publication of immutable project-store files.
//!
//! The caller supplies an already-open project root. Every operation below
//! that root is relative to a held directory descriptor and rejects symlink
//! traversal with `O_NOFOLLOW`. These primitives publish exact immutable
//! objects and internally encoded complete generations. They own no refs,
//! leases, actor, recovery, or GC behavior.
//! The transaction caller must hold the writer lease; this primitive does not
//! claim protection from an out-of-protocol same-user process reparenting the
//! store namespace while its directory descriptors are open.

#![cfg(target_os = "linux")]
#![cfg_attr(not(test), allow(dead_code))]

use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Cursor, Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use mirante4d_identity::{ExactBytesDigest, ExactBytesFacts, ExactBytesHasher};
use rustix::{
    fd::OwnedFd,
    fs::{
        AtFlags, CWD, Dir, FileType, Mode, OFlags, RenameFlags, fstat, fsync, mkdirat, openat,
        renameat_with, statat, unlinkat,
    },
    io::Errno,
};
use thiserror::Error;

use crate::{
    ProjectGenerationId, ProjectStoreLimits,
    generation::EncodedGeneration,
    wire::{
        ProjectEnvelope, REF_BYTES, RefKind, RefRecord, generation_id_from_validated_canonical,
    },
};

const STREAM_BUFFER_BYTES: usize = 1024 * 1024;
const STAGE_CREATE_ATTEMPTS: u64 = 128;
const STAGING_DIRECTORY: &str = "staging";
const STAGED_FILE: &str = "payload";
const AUTHORITY_SYNC_ATTEMPTS: usize = 2;

static STAGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const DIRECTORY_OPEN_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW);
const FILE_READ_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DeclaredExactFile {
    digest: ExactBytesDigest,
    byte_length: u64,
}

impl DeclaredExactFile {
    const fn new(digest: ExactBytesDigest, byte_length: u64) -> Self {
        Self {
            digest,
            byte_length,
        }
    }

    const fn from_facts(facts: ExactBytesFacts) -> Self {
        Self::new(facts.digest(), facts.byte_length())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceExtent {
    /// The source is exactly one complete object and must end at the declared
    /// byte length.
    Complete,
    /// Consume exactly one declared segment from a larger logical stream.
    Segment,
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
    #[error("an internally encoded generation has an invalid framed identity")]
    InvalidGeneration,
    #[error("a project envelope or ref control record is invalid")]
    InvalidControl,
    #[error("the initial manual head appeared after the empty-ref check")]
    RefAlreadyPresent,
    #[error("the initial manual head may be visible but its directory durability is indeterminate")]
    RefCommitIndeterminate,
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

fn generation_relative_path(id: ProjectGenerationId) -> PathBuf {
    let digest = id.digest().to_string();
    PathBuf::from("generations")
        .join("sha256")
        .join(&digest[..2])
        .join(format!("{}.json", &digest[2..]))
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

    pub(crate) const fn descriptor(&self) -> &OwnedFd {
        &self.root
    }

    /// Reads the fixed project envelope without creating any path component.
    pub(crate) fn read_project_envelope<C>(
        &self,
        limits: ProjectStoreLimits,
        is_cancelled: C,
    ) -> Result<ProjectEnvelope, LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        let bytes = self
            .read_optional_small_file(
                Path::new("project.json"),
                u64::try_from(limits.project_envelope_bytes_max)
                    .map_err(|_| LocalPublicationError::InvalidControl)?,
                is_cancelled,
            )?
            .ok_or(LocalPublicationError::InvalidControl)?;
        ProjectEnvelope::decode(&bytes).map_err(|_| LocalPublicationError::InvalidControl)
    }

    /// Reads one exact lane ref through held descriptors. Missing refs remain
    /// distinct from invalid records and no directory is created.
    pub(crate) fn read_ref<C>(
        &self,
        kind: RefKind,
        limits: ProjectStoreLimits,
        is_cancelled: C,
    ) -> Result<Option<RefRecord>, LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        let path = ref_relative_path(kind)?;
        let Some(bytes) = self.read_optional_small_file(
            &path,
            u64::try_from(limits.ref_record_bytes_max)
                .map_err(|_| LocalPublicationError::InvalidControl)?,
            is_cancelled,
        )?
        else {
            return Ok(None);
        };
        if bytes.len() != limits.ref_record_bytes_exact || bytes.len() != REF_BYTES {
            return Err(LocalPublicationError::InvalidControl);
        }
        RefRecord::decode(kind, &bytes)
            .map(Some)
            .map_err(|_| LocalPublicationError::InvalidControl)
    }

    /// Requires the prepared store's ref namespace to contain no fixed,
    /// pinned, or unknown ref. An empty `refs/pins` directory is harmless;
    /// every visible entry is bounded and checked through held descriptors.
    pub(crate) fn require_empty_ref_namespace<C>(
        &self,
        limits: ProjectStoreLimits,
        mut is_cancelled: C,
    ) -> Result<(), LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        check_cancelled(&mut is_cancelled)?;
        let refs = match openat(
            &self.root,
            OsStr::new("refs"),
            DIRECTORY_OPEN_FLAGS,
            Mode::empty(),
        ) {
            Ok(refs) => refs,
            Err(Errno::NOENT) => return Ok(()),
            Err(_) => return Err(LocalPublicationError::InvalidControl),
        };
        let mut entries = 0_usize;
        for entry in Dir::read_from(&refs).map_err(|_| LocalPublicationError::InvalidControl)? {
            check_cancelled(&mut is_cancelled)?;
            let entry = entry.map_err(|_| LocalPublicationError::InvalidControl)?;
            let name = entry.file_name();
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            entries = entries
                .checked_add(1)
                .ok_or(LocalPublicationError::InvalidControl)?;
            if entries > limits.directory_fanout_entries_max {
                return Err(LocalPublicationError::Capacity {
                    declared: u64::try_from(entries).unwrap_or(u64::MAX),
                    maximum: u64::try_from(limits.directory_fanout_entries_max).unwrap_or(u64::MAX),
                });
            }
            if name.to_bytes() != b"pins" {
                return Err(LocalPublicationError::InvalidControl);
            }
            let pins = openat(&refs, name, DIRECTORY_OPEN_FLAGS, Mode::empty())
                .map_err(|_| LocalPublicationError::InvalidControl)?;
            let mut pin_entries = 0_usize;
            for pin in Dir::read_from(&pins).map_err(|_| LocalPublicationError::InvalidControl)? {
                check_cancelled(&mut is_cancelled)?;
                let pin = pin.map_err(|_| LocalPublicationError::InvalidControl)?;
                let pin_name = pin.file_name().to_bytes();
                if pin_name == b"." || pin_name == b".." {
                    continue;
                }
                pin_entries = pin_entries
                    .checked_add(1)
                    .ok_or(LocalPublicationError::InvalidControl)?;
                if pin_entries > limits.pin_refs_max {
                    return Err(LocalPublicationError::Capacity {
                        declared: u64::try_from(pin_entries).unwrap_or(u64::MAX),
                        maximum: u64::try_from(limits.pin_refs_max).unwrap_or(u64::MAX),
                    });
                }
                return Err(LocalPublicationError::InvalidControl);
            }
        }
        Ok(())
    }

    /// Publishes the first manual head exactly once. There is deliberately no
    /// replacement path here: established-head commits remain deferred until
    /// the frozen recovery/head crash-state rule is corrected.
    pub(crate) fn publish_initial_manual_head<C>(
        &self,
        record: RefRecord,
        limits: ProjectStoreLimits,
        mut is_cancelled: C,
    ) -> Result<(), LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        if record.kind() != RefKind::ManualHead
            || record.previous().is_some()
            || record.base().is_some()
            || limits.ref_record_bytes_exact != REF_BYTES
        {
            return Err(LocalPublicationError::InvalidControl);
        }
        check_cancelled(&mut is_cancelled)?;
        let destination = ref_relative_path(RefKind::ManualHead)?;
        let (destination_parent, destination_name) = self.open_or_create_parent(&destination)?;
        match statat(
            &destination_parent,
            &destination_name,
            AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Err(Errno::NOENT) => {}
            Ok(_) => return Err(LocalPublicationError::RefAlreadyPresent),
            Err(_) => return Err(LocalPublicationError::ExistingMismatch),
        }

        let mut stage = Stage::begin(self)?;
        let mut staged_file = stage.create_file()?;
        staged_file
            .write_all(&record.encode())
            .map_err(|source| LocalPublicationError::Io {
                operation: "write the initial manual head",
                source,
            })?;
        fsync(&staged_file)
            .map_err(|error| io_error("synchronize the staged initial manual head", error))?;
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
                if sync_with_retry(&stage.directory).is_err()
                    || sync_with_retry(&destination_parent).is_err()
                {
                    return Err(LocalPublicationError::RefCommitIndeterminate);
                }
                Ok(())
            }
            Err(Errno::EXIST) => Err(LocalPublicationError::RefAlreadyPresent),
            Err(error)
                if error == Errno::NOSYS || error == Errno::INVAL || error == Errno::OPNOTSUPP =>
            {
                Err(LocalPublicationError::AtomicPublishUnsupported)
            }
            Err(error) => Err(io_error("publish the initial manual head", error)),
        }
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
        self.publish_declared_object(
            source,
            expected.digest(),
            expected.byte_length(),
            maximum_bytes,
            is_cancelled,
        )
    }

    /// Validates and durabilizes a declared object if it already exists. A
    /// missing object or fanout directory is reported without creating it.
    pub(crate) fn durabilize_object_if_present<C>(
        &self,
        digest: ExactBytesDigest,
        byte_length: u64,
        maximum_bytes: u64,
        mut is_cancelled: C,
    ) -> Result<bool, LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        let expected = DeclaredExactFile::new(digest, byte_length);
        validate_capacity(expected, maximum_bytes)?;
        check_cancelled(&mut is_cancelled)?;
        let path = object_relative_path(digest);
        let Some((parent, name)) = self.open_existing_parent_if_present(&path)? else {
            return Ok(false);
        };
        Ok(validate_existing_if_present(&parent, &name, expected, &mut is_cancelled)?.is_some())
    }

    /// Publishes one closed, canonical generation at its internally derived
    /// domain-separated path. Callers cannot supply an arbitrary path identity.
    pub(crate) fn publish_generation<C>(
        &self,
        generation: &EncodedGeneration,
        maximum_bytes: u64,
        is_cancelled: C,
    ) -> Result<ImmutablePublication, LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        let id = generation_id_from_validated_canonical(generation.bytes())
            .map_err(|_| LocalPublicationError::InvalidGeneration)?;
        if id != generation.id() {
            return Err(LocalPublicationError::InvalidGeneration);
        }
        let facts = ExactBytesHasher::hash(generation.bytes())
            .map_err(|_| LocalPublicationError::InvalidGeneration)?;
        self.publish(
            generation_relative_path(id),
            &mut Cursor::new(generation.bytes()),
            DeclaredExactFile::from_facts(facts),
            maximum_bytes,
            SourceExtent::Complete,
            is_cancelled,
        )
    }

    /// Publishes one declared direct object from a source which must end at
    /// exactly `byte_length`. An already-present exact object is durabilized
    /// without opening or consuming `source`.
    pub(crate) fn publish_declared_object<R, C>(
        &self,
        source: &mut R,
        digest: ExactBytesDigest,
        byte_length: u64,
        maximum_bytes: u64,
        is_cancelled: C,
    ) -> Result<ImmutablePublication, LocalPublicationError>
    where
        R: Read + ?Sized,
        C: FnMut() -> bool,
    {
        self.publish(
            object_relative_path(digest),
            source,
            DeclaredExactFile::new(digest, byte_length),
            maximum_bytes,
            SourceExtent::Complete,
            is_cancelled,
        )
    }

    /// Publishes one known exact page while consuming exactly `byte_length`
    /// bytes from a larger source. Successful deduplication and no-replace
    /// races still consume and validate the complete declared segment, but do
    /// not require the larger source to be at EOF.
    pub(crate) fn publish_consuming_object<R, C>(
        &self,
        source: &mut R,
        digest: ExactBytesDigest,
        byte_length: u64,
        maximum_bytes: u64,
        is_cancelled: C,
    ) -> Result<ImmutablePublication, LocalPublicationError>
    where
        R: Read + ?Sized,
        C: FnMut() -> bool,
    {
        self.publish(
            object_relative_path(digest),
            source,
            DeclaredExactFile::new(digest, byte_length),
            maximum_bytes,
            SourceExtent::Segment,
            is_cancelled,
        )
    }

    /// Reads and validates one exact object through held directory
    /// descriptors. `on_chunk` receives slices no larger than 1 MiB and may be
    /// used to reconstruct a bounded binding manifest or a logical digest.
    /// This read-only operation never creates missing path components.
    pub(crate) fn read_exact_object<C, F>(
        &self,
        digest: ExactBytesDigest,
        byte_length: u64,
        maximum_bytes: u64,
        mut is_cancelled: C,
        mut on_chunk: F,
    ) -> Result<ExactBytesFacts, LocalPublicationError>
    where
        C: FnMut() -> bool,
        F: FnMut(&[u8]),
    {
        let expected = DeclaredExactFile::new(digest, byte_length);
        validate_capacity(expected, maximum_bytes)?;
        check_cancelled(&mut is_cancelled)?;
        let path = object_relative_path(digest);
        let (parent, name) = self.open_existing_parent(&path)?;
        validate_existing_if_present_with(
            &parent,
            &name,
            expected,
            &mut is_cancelled,
            &mut on_chunk,
        )?
        .ok_or(LocalPublicationError::ExistingMismatch)
    }

    fn publish<R, C>(
        &self,
        destination: PathBuf,
        source: &mut R,
        expected: DeclaredExactFile,
        maximum_bytes: u64,
        source_extent: SourceExtent,
        mut is_cancelled: C,
    ) -> Result<ImmutablePublication, LocalPublicationError>
    where
        R: Read + ?Sized,
        C: FnMut() -> bool,
    {
        validate_capacity(expected, maximum_bytes)?;
        check_cancelled(&mut is_cancelled)?;

        let (destination_parent, destination_name) = self.open_or_create_parent(&destination)?;
        if let Some(facts) = validate_existing_if_present(
            &destination_parent,
            &destination_name,
            expected,
            &mut is_cancelled,
        )? {
            if source_extent == SourceExtent::Segment {
                let consumed =
                    consume_exact_segment(source, expected.byte_length, &mut is_cancelled)?;
                validate_source_facts(consumed, expected)?;
            }
            return Ok(ImmutablePublication {
                facts,
                disposition: PublicationDisposition::AlreadyPresent,
            });
        }
        let mut stage = Stage::begin(self)?;
        let mut staged_file = stage.create_file()?;
        let facts = match source_extent {
            SourceExtent::Complete => write_exact_source(
                &mut staged_file,
                source,
                expected.byte_length,
                &mut is_cancelled,
            )?,
            SourceExtent::Segment => write_exact_segment(
                &mut staged_file,
                source,
                expected.byte_length,
                &mut is_cancelled,
            )?,
        };
        validate_source_facts(facts, expected)?;
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
                // A cross-directory rename is not durably complete until both
                // the source-name removal and destination-name addition have
                // been synchronized. Cancellation is deliberately not polled
                // in this post-rename critical section.
                sync_published_rename(&stage.directory, &destination_parent)?;
                Ok(ImmutablePublication {
                    facts,
                    disposition: PublicationDisposition::Created,
                })
            }
            Err(Errno::EXIST) => {
                if validate_existing_if_present(
                    &destination_parent,
                    &destination_name,
                    DeclaredExactFile::from_facts(facts),
                    &mut is_cancelled,
                )?
                .is_none()
                {
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

    fn open_existing_parent(
        &self,
        path: &Path,
    ) -> Result<(OwnedFd, OsString), LocalPublicationError> {
        let mut components = normal_components(path)?;
        let file_name = components.pop().ok_or(LocalPublicationError::InvalidPath)?;
        let mut current = duplicate_directory(&self.root)?;
        for component in components {
            current = openat(&current, &component, DIRECTORY_OPEN_FLAGS, Mode::empty())
                .map_err(|_| LocalPublicationError::ExistingMismatch)?;
        }
        Ok((current, file_name))
    }

    fn open_existing_parent_if_present(
        &self,
        path: &Path,
    ) -> Result<Option<(OwnedFd, OsString)>, LocalPublicationError> {
        let mut components = normal_components(path)?;
        let file_name = components.pop().ok_or(LocalPublicationError::InvalidPath)?;
        let mut current = duplicate_directory(&self.root)?;
        for component in components {
            current = match openat(&current, &component, DIRECTORY_OPEN_FLAGS, Mode::empty()) {
                Ok(directory) => directory,
                Err(Errno::NOENT) => return Ok(None),
                Err(_) => return Err(LocalPublicationError::ExistingMismatch),
            };
        }
        Ok(Some((current, file_name)))
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

    fn read_optional_small_file<C>(
        &self,
        path: &Path,
        maximum_bytes: u64,
        mut is_cancelled: C,
    ) -> Result<Option<Vec<u8>>, LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        check_cancelled(&mut is_cancelled)?;
        let Some((parent, name)) = self.open_existing_parent_if_present(path)? else {
            return Ok(None);
        };
        let descriptor = match openat(&parent, &name, FILE_READ_FLAGS, Mode::empty()) {
            Ok(descriptor) => descriptor,
            Err(Errno::NOENT) => return Ok(None),
            Err(_) => return Err(LocalPublicationError::ExistingMismatch),
        };
        let before = FileIdentity::from_stat(
            fstat(&descriptor).map_err(|_| LocalPublicationError::ExistingMismatch)?,
        )?;
        if before.bytes > maximum_bytes {
            return Err(LocalPublicationError::Capacity {
                declared: before.bytes,
                maximum: maximum_bytes,
            });
        }
        let length =
            usize::try_from(before.bytes).map_err(|_| LocalPublicationError::InvalidControl)?;
        let mut file = File::from(descriptor);
        let mut bytes = vec![0_u8; length];
        file.read_exact(&mut bytes)
            .map_err(|_| LocalPublicationError::ExistingMismatch)?;
        let mut extra = [0_u8; 1];
        if file
            .read(&mut extra)
            .map_err(|_| LocalPublicationError::ExistingMismatch)?
            != 0
        {
            return Err(LocalPublicationError::ExistingMismatch);
        }
        check_cancelled(&mut is_cancelled)?;
        let after = FileIdentity::from_stat(
            fstat(&file).map_err(|_| LocalPublicationError::ExistingMismatch)?,
        )?;
        let named = FileIdentity::from_stat(
            statat(&parent, &name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| LocalPublicationError::ExistingMismatch)?,
        )?;
        if before != after || before != named {
            return Err(LocalPublicationError::ExistingMismatch);
        }
        Ok(Some(bytes))
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

fn write_exact_segment<R, C>(
    file: &mut File,
    source: &mut R,
    expected_bytes: u64,
    is_cancelled: &mut C,
) -> Result<ExactBytesFacts, LocalPublicationError>
where
    R: Read + ?Sized,
    C: FnMut() -> bool,
{
    consume_declared_segment(source, expected_bytes, is_cancelled, |bytes| {
        file.write_all(bytes)
            .map_err(|source| LocalPublicationError::Io {
                operation: "write a staged immutable file segment",
                source,
            })
    })
}

fn consume_exact_segment<R, C>(
    source: &mut R,
    expected_bytes: u64,
    is_cancelled: &mut C,
) -> Result<ExactBytesFacts, LocalPublicationError>
where
    R: Read + ?Sized,
    C: FnMut() -> bool,
{
    consume_declared_segment(source, expected_bytes, is_cancelled, |_| Ok(()))
}

fn consume_declared_segment<R, C, F>(
    source: &mut R,
    expected_bytes: u64,
    is_cancelled: &mut C,
    mut on_chunk: F,
) -> Result<ExactBytesFacts, LocalPublicationError>
where
    R: Read + ?Sized,
    C: FnMut() -> bool,
    F: FnMut(&[u8]) -> Result<(), LocalPublicationError>,
{
    let mut hasher = ExactBytesHasher::new();
    let mut remaining = expected_bytes;
    let mut buffer = vec![0_u8; STREAM_BUFFER_BYTES];
    while remaining != 0 {
        check_cancelled(is_cancelled)?;
        let requested = usize::try_from(remaining.min(STREAM_BUFFER_BYTES as u64))
            .expect("the request is capped by a usize constant");
        let read = match source.read(&mut buffer[..requested]) {
            Ok(0) => {
                return Err(LocalPublicationError::SourceLength {
                    expected: expected_bytes,
                });
            }
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(LocalPublicationError::Io {
                    operation: "read an immutable source segment",
                    source,
                });
            }
        };
        on_chunk(&buffer[..read])?;
        hasher
            .update(&buffer[..read])
            .map_err(|_| LocalPublicationError::SourceLength {
                expected: expected_bytes,
            })?;
        remaining -= u64::try_from(read).expect("a read byte count fits u64");
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
    expected: DeclaredExactFile,
    is_cancelled: &mut C,
) -> Result<Option<ExactBytesFacts>, LocalPublicationError>
where
    C: FnMut() -> bool,
{
    validate_existing_if_present_with(parent, name, expected, is_cancelled, &mut |_| {})
}

fn validate_existing_if_present_with<C, F>(
    parent: &OwnedFd,
    name: &OsStr,
    expected: DeclaredExactFile,
    is_cancelled: &mut C,
    on_chunk: &mut F,
) -> Result<Option<ExactBytesFacts>, LocalPublicationError>
where
    C: FnMut() -> bool,
    F: FnMut(&[u8]),
{
    check_cancelled(is_cancelled)?;
    let descriptor = match openat(parent, name, FILE_READ_FLAGS, Mode::empty()) {
        Ok(descriptor) => descriptor,
        Err(Errno::NOENT) => return Ok(None),
        Err(_) => return Err(LocalPublicationError::ExistingMismatch),
    };
    let before = FileIdentity::from_stat(
        fstat(&descriptor).map_err(|_| LocalPublicationError::ExistingMismatch)?,
    )?;
    if before.bytes != expected.byte_length {
        return Err(LocalPublicationError::ExistingMismatch);
    }

    let mut file = File::from(descriptor);
    let facts = hash_exact_file(&mut file, expected.byte_length, is_cancelled, on_chunk)?;
    check_cancelled(is_cancelled)?;
    if facts.byte_length() != expected.byte_length || facts.digest() != expected.digest {
        return Err(LocalPublicationError::ExistingMismatch);
    }
    fsync(&file).map_err(|error| io_error("synchronize an existing immutable file", error))?;
    fsync(parent).map_err(|error| {
        io_error(
            "synchronize an existing immutable destination directory",
            error,
        )
    })?;
    let after = FileIdentity::from_stat(
        fstat(&file).map_err(|_| LocalPublicationError::ExistingMismatch)?,
    )?;
    let named = FileIdentity::from_stat(
        statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| LocalPublicationError::ExistingMismatch)?,
    )?;
    if before != after || before != named {
        return Err(LocalPublicationError::ExistingMismatch);
    }
    Ok(Some(facts))
}

fn hash_exact_file<C, F>(
    file: &mut File,
    expected_bytes: u64,
    is_cancelled: &mut C,
    on_chunk: &mut F,
) -> Result<ExactBytesFacts, LocalPublicationError>
where
    C: FnMut() -> bool,
    F: FnMut(&[u8]),
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
        on_chunk(&buffer[..read]);
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

fn validate_capacity(
    expected: DeclaredExactFile,
    maximum_bytes: u64,
) -> Result<(), LocalPublicationError> {
    if expected.byte_length > maximum_bytes {
        Err(LocalPublicationError::Capacity {
            declared: expected.byte_length,
            maximum: maximum_bytes,
        })
    } else {
        Ok(())
    }
}

fn validate_source_facts(
    actual: ExactBytesFacts,
    expected: DeclaredExactFile,
) -> Result<(), LocalPublicationError> {
    if actual.byte_length() != expected.byte_length {
        Err(LocalPublicationError::SourceLength {
            expected: expected.byte_length,
        })
    } else if actual.digest() != expected.digest {
        Err(LocalPublicationError::SourceDigest)
    } else {
        Ok(())
    }
}

fn sync_published_rename(
    source_directory: &OwnedFd,
    destination_directory: &OwnedFd,
) -> Result<(), LocalPublicationError> {
    fsync(source_directory)
        .map_err(|error| io_error("synchronize an immutable staging directory", error))?;
    fsync(destination_directory)
        .map_err(|error| io_error("synchronize an immutable destination directory", error))
}

fn sync_with_retry(directory: &OwnedFd) -> Result<(), Errno> {
    let mut last_error = None;
    for _ in 0..AUTHORITY_SYNC_ATTEMPTS {
        match fsync(directory) {
            Ok(()) => return Ok(()),
            Err(Errno::INTR) => {}
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or(Errno::INTR))
}

fn ref_relative_path(kind: RefKind) -> Result<PathBuf, LocalPublicationError> {
    let name = match kind {
        RefKind::ManualHead => "head",
        RefKind::ManualRecovery => "recovery",
        RefKind::AutosaveHead => "autosave-head",
        RefKind::AutosaveRecovery => "autosave-recovery",
        RefKind::Pin => return Err(LocalPublicationError::InvalidControl),
    };
    Ok(PathBuf::from("refs").join(name))
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
        Ok(directory) => {
            // A visible directory may be the remnant of an interrupted prior
            // publication. Re-sync its name before using it in a new durable
            // immutable path.
            fsync(parent)
                .map_err(|error| io_error("synchronize a project-store parent directory", error))?;
            Ok(directory)
        }
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
        process::Command,
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

    struct RacingSource {
        inner: Cursor<Vec<u8>>,
        destination: PathBuf,
        collision: Vec<u8>,
        installed: bool,
    }

    impl RacingSource {
        fn new(bytes: Vec<u8>, destination: PathBuf, collision: Vec<u8>) -> Self {
            Self {
                inner: Cursor::new(bytes),
                destination,
                collision,
                installed: false,
            }
        }
    }

    impl Read for RacingSource {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.installed {
                fs::write(&self.destination, &self.collision)?;
                self.installed = true;
            }
            self.inner.read(buffer)
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
    fn declared_and_consuming_publication_enforce_their_source_extents() {
        let directory = TestDirectory::new("declared-consuming");
        let root = LocalStoreRoot::open(directory.path()).unwrap();

        let direct = b"one complete direct object";
        let direct_facts = facts(direct);
        let direct_publication = root
            .publish_declared_object(
                &mut Cursor::new(direct),
                direct_facts.digest(),
                direct_facts.byte_length(),
                1024,
                || false,
            )
            .unwrap();
        assert_eq!(
            direct_publication.disposition(),
            PublicationDisposition::Created
        );

        let page = b"known page bytes";
        let tail = b"next page remains unread";
        let page_facts = facts(page);
        let mut combined = page.to_vec();
        combined.extend_from_slice(tail);
        let mut source = Cursor::new(combined.clone());
        let created = root
            .publish_consuming_object(
                &mut source,
                page_facts.digest(),
                page_facts.byte_length(),
                1024,
                || false,
            )
            .unwrap();
        assert_eq!(created.disposition(), PublicationDisposition::Created);
        let mut remainder = Vec::new();
        source.read_to_end(&mut remainder).unwrap();
        assert_eq!(remainder, tail);

        let mut existing_source = Cursor::new(combined);
        let existing = root
            .publish_consuming_object(
                &mut existing_source,
                page_facts.digest(),
                page_facts.byte_length(),
                1024,
                || false,
            )
            .unwrap();
        assert_eq!(
            existing.disposition(),
            PublicationDisposition::AlreadyPresent
        );
        let mut existing_remainder = Vec::new();
        existing_source
            .read_to_end(&mut existing_remainder)
            .unwrap();
        assert_eq!(existing_remainder, tail);

        let mut direct_with_tail = Cursor::new([direct.as_slice(), tail.as_slice()].concat());
        assert!(matches!(
            root.publish_declared_object(
                &mut direct_with_tail,
                facts(b"different direct path").digest(),
                direct_facts.byte_length(),
                1024,
                || false,
            ),
            Err(LocalPublicationError::SourceLength { .. })
        ));
    }

    #[test]
    fn consuming_publication_advances_the_source_across_a_noreplace_race() {
        let directory = TestDirectory::new("noreplace-race");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let page = b"raced exact page";
        let tail = b"unconsumed tail";
        let expected = facts(page);
        let path = object_path(directory.path(), expected.digest());
        let mut combined = page.to_vec();
        combined.extend_from_slice(tail);
        let mut source = RacingSource::new(combined, path.clone(), page.to_vec());

        let publication = root
            .publish_consuming_object(
                &mut source,
                expected.digest(),
                expected.byte_length(),
                1024,
                || false,
            )
            .unwrap();
        assert_eq!(
            publication.disposition(),
            PublicationDisposition::AlreadyPresent
        );
        assert_eq!(fs::read(path).unwrap(), page);
        let mut remainder = Vec::new();
        source.read_to_end(&mut remainder).unwrap();
        assert_eq!(remainder, tail);
    }

    #[test]
    fn descriptor_relative_object_read_is_bounded_exact_and_noncreating() {
        let directory = TestDirectory::new("bounded-read");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let bytes = (0..(STREAM_BUFFER_BYTES * 2 + 31))
            .map(|index| (index % 239) as u8)
            .collect::<Vec<_>>();
        let expected = facts(&bytes);
        root.publish_object(
            &mut Cursor::new(&bytes),
            expected,
            bytes.len() as u64,
            || false,
        )
        .unwrap();

        let mut observed = Vec::new();
        let mut largest_chunk = 0;
        let read = root
            .read_exact_object(
                expected.digest(),
                expected.byte_length(),
                bytes.len() as u64,
                || false,
                |chunk| {
                    largest_chunk = largest_chunk.max(chunk.len());
                    observed.extend_from_slice(chunk);
                },
            )
            .unwrap();
        assert_eq!(read, expected);
        assert_eq!(observed, bytes);
        assert!(largest_chunk <= STREAM_BUFFER_BYTES);

        let missing = facts(b"missing object");
        let empty = TestDirectory::new("missing-read");
        let empty_root = LocalStoreRoot::open(empty.path()).unwrap();
        assert!(matches!(
            empty_root.read_exact_object(
                missing.digest(),
                missing.byte_length(),
                1024,
                || false,
                |_| {}
            ),
            Err(LocalPublicationError::ExistingMismatch)
        ));
        assert!(!empty.path().join("objects").exists());
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

        fs::remove_file(&path).unwrap();
        assert!(
            Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        assert!(matches!(
            root.publish_object(&mut Cursor::new(bytes), expected, 16 * 1024 * 1024, || {
                false
            }),
            Err(LocalPublicationError::ExistingMismatch)
        ));
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

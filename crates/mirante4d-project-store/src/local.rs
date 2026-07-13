//! Descriptor-relative publication of immutable project-store files.
//!
//! The caller supplies an already-open project root. Every operation below
//! that root is relative to a held directory descriptor and rejects symlink
//! traversal with `O_NOFOLLOW`. These primitives publish exact immutable
//! objects, internally encoded complete generations, exact fixed-size refs,
//! and bounded physical-store inventory. They own no ref policy, leases,
//! actor, recovery selection, or GC behavior.
//! The transaction caller must hold the writer lease; this primitive does not
//! claim protection from an out-of-protocol same-user process reparenting the
//! store namespace while its directory descriptors are open.

#![cfg(target_os = "linux")]
#![cfg_attr(not(test), allow(dead_code))]

use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Cursor, Read, Write},
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use mirante4d_identity::{ExactBytesDigest, ExactBytesFacts, ExactBytesHasher, Sha256Digest};
use rustix::{
    fd::OwnedFd,
    fs::{
        AtFlags, CWD, Dir, FileType, Mode, OFlags, RenameFlags, fstat, fsync, mkdirat, openat,
        renameat, renameat_with, statat, unlinkat,
    },
    io::Errno,
};
use thiserror::Error;

#[cfg(test)]
use std::sync::atomic::AtomicUsize;

use crate::{
    ProjectGenerationId, ProjectStoreLimits, ProjectStorePath,
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
// LocalStoreRoot, maintenance lease, and writer-envelope lease remain held
// throughout a writable transaction.
const WRITABLE_SESSION_DESCRIPTORS: usize = 3;
// The ref peak additionally holds its destination parent, staging parent,
// private staging directory, and the separate parent/file pair used by the
// final exact predecessor recheck.
const REF_PUBLICATION_DESCRIPTORS: usize = WRITABLE_SESSION_DESCRIPTORS + 5;
// Package installation additionally holds the destination parent, one
// directory-enumeration descriptor, and one file descriptor at the walk peak.
const PACKAGE_INSTALL_DESCRIPTORS: usize = WRITABLE_SESSION_DESCRIPTORS + 3;
const PACKAGE_CLEANUP_DESCRIPTORS: usize = WRITABLE_SESSION_DESCRIPTORS + 2;
// A digest-namespace scan additionally holds the namespace, `sha256`, fanout,
// directory iterator, and one metadata descriptor at its peak.
const DIGEST_ENUMERATION_DESCRIPTORS: usize = WRITABLE_SESSION_DESCRIPTORS + 5;
// Beyond one file per bounded reachable object, the fixed envelope/ref/
// generation tree, 256 digest fan-out directories, and one transient private
// publication stage fit comfortably inside this reserve.
const PACKAGE_CLEANUP_ENTRY_OVERHEAD: usize = 1_024;

static STAGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const DIRECTORY_OPEN_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW);
const FILE_READ_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK);
const FILE_METADATA_FLAGS: OFlags = OFlags::PATH.union(OFlags::CLOEXEC).union(OFlags::NOFOLLOW);
const FILE_CREATE_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW);

/// One held project-store root used for descriptor-relative immutable writes.
#[derive(Debug)]
pub(crate) struct LocalStoreRoot {
    root: OwnedFd,
    external_held_descriptors: usize,
    #[cfg(test)]
    ref_commit_directory_sync_count: AtomicUsize,
    #[cfg(test)]
    ref_commit_directory_sync_failure: AtomicUsize,
}

/// One destination-local sibling package which is invisible until its final
/// no-replace rename. Drop removes only the still-owned staging tree.
pub(crate) struct SiblingPackageStage {
    parent: OwnedFd,
    destination_name: OsString,
    stage_name: OsString,
    stage_identity: DirectoryIdentity,
    root: Option<LocalStoreRoot>,
    cleanup_limits: PackageCleanupLimits,
    owns_stage: bool,
}

#[derive(Clone, Copy)]
struct PackageCleanupLimits {
    directory_fanout_entries_max: usize,
    physical_store_entries_max: usize,
    open_file_descriptors_max: usize,
}

impl PackageCleanupLimits {
    fn for_initial_package(limits: ProjectStoreLimits) -> Result<Self, LocalPublicationError> {
        let self_authored_entries = limits
            .reachable_objects_per_generation_max
            .checked_add(PACKAGE_CLEANUP_ENTRY_OVERHEAD)
            .ok_or_else(|| capacity_overflow(limits.physical_store_entries_max))?;
        let defaults = ProjectStoreLimits::default();
        Ok(Self {
            directory_fanout_entries_max: limits
                .directory_fanout_entries_max
                .max(defaults.directory_fanout_entries_max)
                .max(self_authored_entries),
            physical_store_entries_max: limits
                .physical_store_entries_max
                .max(defaults.physical_store_entries_max)
                .max(self_authored_entries),
            open_file_descriptors_max: limits
                .open_file_descriptors_max
                .max(defaults.open_file_descriptors_max),
        })
    }
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StoreInventory {
    entries: usize,
    root_entries: usize,
    refs_entries: Option<usize>,
    pins_entries: Option<usize>,
    staging_entries: Option<usize>,
    inspect_staging: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InventoryDirectory {
    Root,
    Refs,
    Pins,
    Staging,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DigestNamespace {
    Generations,
    Objects,
}

impl DigestNamespace {
    const fn directory(self) -> &'static str {
        match self {
            Self::Generations => "generations",
            Self::Objects => "objects",
        }
    }

    const fn suffix(self) -> &'static [u8] {
        match self {
            Self::Generations => b".json",
            Self::Objects => b"",
        }
    }

    const fn maximum_file_bytes(self, limits: ProjectStoreLimits) -> u64 {
        match self {
            Self::Generations => limits.generation_bytes_max,
            Self::Objects => limits.object_or_page_bytes_max,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EnumeratedDigestFile {
    digest: Sha256Digest,
    byte_length: u64,
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
    #[error("the package destination already exists")]
    DestinationExists,
    #[error("the filesystem cannot publish an immutable file without replacement")]
    AtomicPublishUnsupported,
    #[error("an internally encoded generation has an invalid framed identity")]
    InvalidGeneration,
    #[error("a project envelope or ref control record is invalid")]
    InvalidControl,
    #[error("the initial manual head appeared after the empty-ref check")]
    RefAlreadyPresent,
    #[error("a project ref changed after transaction validation")]
    RefChanged,
    #[error("a replaced project ref may be visible but its directory durability is indeterminate")]
    RefCommitIndeterminate,
    #[error("the installed package may be visible but parent durability is indeterminate")]
    PackageCommitIndeterminate,
    #[error("project-store I/O failed while attempting to {operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PinTransition {
    StageCreate,
    Write,
    FileSync,
    Replace,
    DirectorySync,
    UnpinRemove,
    UnpinDirectorySync,
}

pub(crate) trait PinTransitionObserver {
    type Occurrence;

    fn before(&self, transition: PinTransition) -> Result<Self::Occurrence, ()>;
    fn after(&self, occurrence: Self::Occurrence) -> Result<(), ()>;
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
        Ok(Self {
            root: current,
            external_held_descriptors: 0,
            #[cfg(test)]
            ref_commit_directory_sync_count: AtomicUsize::new(0),
            #[cfg(test)]
            ref_commit_directory_sync_failure: AtomicUsize::new(usize::MAX),
        })
    }

    pub(crate) const fn descriptor(&self) -> &OwnedFd {
        &self.root
    }

    /// Re-establishes destination-parent durability for an already visible
    /// package, but only while the named directory is still this held root.
    /// This is the narrow recovery operation used after an indeterminate
    /// no-clobber package install; it never creates or replaces a name.
    pub(crate) fn sync_existing_package_parent(
        &self,
        destination: &ProjectStorePath,
        limits: ProjectStoreLimits,
    ) -> Result<(), LocalPublicationError> {
        enforce_count(
            WRITABLE_SESSION_DESCRIPTORS
                .checked_add(1)
                .ok_or_else(|| capacity_overflow(limits.open_file_descriptors_max))?,
            limits.open_file_descriptors_max,
        )?;
        let path = destination.as_path();
        let destination_name = path
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or(LocalPublicationError::InvalidPath)?;
        let parent_path = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = LocalStoreRoot::open(parent_path)?;
        let held = DirectoryIdentity::from_stat(
            fstat(&self.root).map_err(|_| LocalPublicationError::ExistingMismatch)?,
        )?;
        let named = DirectoryIdentity::from_stat(
            statat(
                parent.descriptor(),
                destination_name,
                AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(|_| LocalPublicationError::ExistingMismatch)?,
        )?;
        if named != held {
            return Err(LocalPublicationError::ExistingMismatch);
        }
        sync_with_retry(parent.descriptor())
            .map_err(|_| LocalPublicationError::PackageCommitIndeterminate)?;
        let named_after = DirectoryIdentity::from_stat(
            statat(
                parent.descriptor(),
                destination_name,
                AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(|_| LocalPublicationError::ExistingMismatch)?,
        )?;
        if named_after != held {
            return Err(LocalPublicationError::ExistingMismatch);
        }
        Ok(())
    }

    /// Repeats the existing ref-directory durability step without replacing a
    /// ref. Callers must validate the exact visible authority before and after
    /// this recovery sync.
    pub(crate) fn sync_existing_ref_directory(&self) -> Result<(), LocalPublicationError> {
        let refs = openat(
            &self.root,
            OsStr::new("refs"),
            DIRECTORY_OPEN_FLAGS,
            Mode::empty(),
        )
        .map_err(|_| LocalPublicationError::ExistingMismatch)?;
        self.sync_ref_commit_directory(&refs)
            .map_err(|_| LocalPublicationError::RefCommitIndeterminate)
    }

    /// Creates the immutable project envelope in a private, unpublished
    /// package root.
    pub(crate) fn publish_project_envelope<C>(
        &self,
        envelope: ProjectEnvelope,
        limits: ProjectStoreLimits,
        mut is_cancelled: C,
    ) -> Result<(), LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        let bytes = envelope
            .encode()
            .map_err(|_| LocalPublicationError::InvalidControl)?;
        if bytes.len() > limits.project_envelope_bytes_max {
            return Err(LocalPublicationError::Capacity {
                declared: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                maximum: u64::try_from(limits.project_envelope_bytes_max).unwrap_or(u64::MAX),
            });
        }
        check_cancelled(&mut is_cancelled)?;
        let descriptor = openat(
            &self.root,
            OsStr::new("project.json"),
            FILE_CREATE_FLAGS,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| match error {
            Errno::EXIST => LocalPublicationError::ExistingMismatch,
            other => io_error("create the project envelope", other),
        })?;
        let mut file = File::from(descriptor);
        file.write_all(&bytes)
            .map_err(|source| LocalPublicationError::Io {
                operation: "write the project envelope",
                source,
            })?;
        fsync(&file).map_err(|error| io_error("synchronize the project envelope", error))?;
        check_cancelled(&mut is_cancelled)?;
        fsync(&self.root).map_err(|error| io_error("synchronize the project root", error))
    }

    #[cfg(test)]
    pub(crate) fn fail_ref_commit_directory_sync_at(&self, occurrence: usize) {
        self.ref_commit_directory_sync_failure
            .store(occurrence, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn ref_commit_directory_sync_attempts(&self) -> usize {
        self.ref_commit_directory_sync_count.load(Ordering::Acquire)
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

    /// Reads one immutable generation through its digest-derived path without
    /// creating any component. Typed generation validation remains with the
    /// transaction layer.
    pub(crate) fn read_generation_bytes<C>(
        &self,
        id: ProjectGenerationId,
        maximum_bytes: u64,
        is_cancelled: C,
    ) -> Result<Vec<u8>, LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        self.read_optional_small_file(&generation_relative_path(id), maximum_bytes, is_cancelled)?
            .ok_or(LocalPublicationError::ExistingMismatch)
    }

    /// Enumerates every canonical immutable generation name without reading
    /// generation payload bytes or creating any namespace component.
    pub(crate) fn enumerate_generation_ids<C>(
        &self,
        limits: ProjectStoreLimits,
        is_cancelled: C,
    ) -> Result<Vec<ProjectGenerationId>, LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        self.enumerate_digest_namespace(
            DigestNamespace::Generations,
            limits.generations_scanned_max,
            limits,
            is_cancelled,
        )
        .map(|files| {
            files
                .into_iter()
                .map(|file| ProjectGenerationId::from_digest(file.digest))
                .collect()
        })
    }

    /// Enumerates every canonical exact-object name and observed byte length
    /// without reading object bytes or creating any namespace component.
    pub(crate) fn enumerate_object_facts<C>(
        &self,
        limits: ProjectStoreLimits,
        is_cancelled: C,
    ) -> Result<Vec<(ExactBytesDigest, u64)>, LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        self.enumerate_digest_namespace(
            DigestNamespace::Objects,
            limits.physical_store_entries_max,
            limits,
            is_cancelled,
        )
        .map(|files| {
            files
                .into_iter()
                .map(|file| (ExactBytesDigest::from_digest(file.digest), file.byte_length))
                .collect()
        })
    }

    fn enumerate_digest_namespace<C>(
        &self,
        namespace: DigestNamespace,
        item_limit: usize,
        limits: ProjectStoreLimits,
        mut is_cancelled: C,
    ) -> Result<Vec<EnumeratedDigestFile>, LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        check_cancelled(&mut is_cancelled)?;
        let held_descriptors = DIGEST_ENUMERATION_DESCRIPTORS
            .checked_add(self.external_held_descriptors)
            .ok_or_else(|| capacity_overflow(limits.open_file_descriptors_max))?;
        enforce_count(held_descriptors, limits.open_file_descriptors_max)?;

        let Some((namespace_directory, namespace_identity)) =
            open_stable_directory_if_present(&self.root, OsStr::new(namespace.directory()))?
        else {
            return Ok(Vec::new());
        };
        let mut physical_entries = 1_usize;

        let mut namespace_fanout = 0_usize;
        let mut sha256_identity = None;
        for entry in Dir::read_from(&namespace_directory)
            .map_err(|_| LocalPublicationError::ExistingMismatch)?
        {
            check_cancelled(&mut is_cancelled)?;
            let entry = entry.map_err(|_| LocalPublicationError::ExistingMismatch)?;
            let name = entry.file_name();
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            namespace_fanout =
                checked_count_add(namespace_fanout, 1, limits.directory_fanout_entries_max)?;
            enforce_count(namespace_fanout, limits.directory_fanout_entries_max)?;
            physical_entries =
                checked_count_add(physical_entries, 1, limits.physical_store_entries_max)?;
            enforce_count(physical_entries, limits.physical_store_entries_max)?;
            if name.to_bytes() != b"sha256" || sha256_identity.is_some() {
                return Err(LocalPublicationError::ExistingMismatch);
            }
            sha256_identity = Some(DirectoryIdentity::from_stat(
                statat(&namespace_directory, name, AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(|_| LocalPublicationError::ExistingMismatch)?,
            )?);
        }

        let Some(expected_sha256_identity) = sha256_identity else {
            confirm_named_directory(
                &self.root,
                OsStr::new(namespace.directory()),
                namespace_identity,
            )?;
            return Ok(Vec::new());
        };
        let sha256_directory = open_stable_directory(
            &namespace_directory,
            OsStr::new("sha256"),
            expected_sha256_identity,
        )?;

        let mut sha256_fanout = 0_usize;
        let mut fanouts = Vec::new();
        for entry in Dir::read_from(&sha256_directory)
            .map_err(|_| LocalPublicationError::ExistingMismatch)?
        {
            check_cancelled(&mut is_cancelled)?;
            let entry = entry.map_err(|_| LocalPublicationError::ExistingMismatch)?;
            let name = entry.file_name();
            let bytes = name.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            sha256_fanout =
                checked_count_add(sha256_fanout, 1, limits.directory_fanout_entries_max)?;
            enforce_count(sha256_fanout, limits.directory_fanout_entries_max)?;
            physical_entries =
                checked_count_add(physical_entries, 1, limits.physical_store_entries_max)?;
            enforce_count(physical_entries, limits.physical_store_entries_max)?;
            if !is_exact_lower_hex(bytes, 2) {
                return Err(LocalPublicationError::ExistingMismatch);
            }
            fanouts.push((
                OsString::from_vec(bytes.to_vec()),
                DirectoryIdentity::from_stat(
                    statat(&sha256_directory, name, AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(|_| LocalPublicationError::ExistingMismatch)?,
                )?,
            ));
        }
        fanouts.sort_unstable_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));

        let mut files = Vec::new();
        for (fanout_name, expected_fanout_identity) in fanouts {
            check_cancelled(&mut is_cancelled)?;
            let fanout_directory =
                open_stable_directory(&sha256_directory, &fanout_name, expected_fanout_identity)?;
            let mut fanout_entries = 0_usize;
            for entry in Dir::read_from(&fanout_directory)
                .map_err(|_| LocalPublicationError::ExistingMismatch)?
            {
                check_cancelled(&mut is_cancelled)?;
                let entry = entry.map_err(|_| LocalPublicationError::ExistingMismatch)?;
                let name = entry.file_name();
                let name_bytes = name.to_bytes();
                if name_bytes == b"." || name_bytes == b".." {
                    continue;
                }
                fanout_entries =
                    checked_count_add(fanout_entries, 1, limits.directory_fanout_entries_max)?;
                enforce_count(fanout_entries, limits.directory_fanout_entries_max)?;
                physical_entries =
                    checked_count_add(physical_entries, 1, limits.physical_store_entries_max)?;
                enforce_count(physical_entries, limits.physical_store_entries_max)?;
                if files.len() >= item_limit {
                    return Err(LocalPublicationError::Capacity {
                        declared: u64::try_from(files.len().saturating_add(1)).unwrap_or(u64::MAX),
                        maximum: u64::try_from(item_limit).unwrap_or(u64::MAX),
                    });
                }
                let digest = parse_digest_namespace_name(
                    fanout_name.as_bytes(),
                    name_bytes,
                    namespace.suffix(),
                )?;
                let before = FileIdentity::from_stat(
                    statat(&fanout_directory, name, AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(|_| LocalPublicationError::ExistingMismatch)?,
                )?;
                if before.bytes > namespace.maximum_file_bytes(limits) {
                    return Err(LocalPublicationError::Capacity {
                        declared: before.bytes,
                        maximum: namespace.maximum_file_bytes(limits),
                    });
                }
                let descriptor =
                    openat(&fanout_directory, name, FILE_METADATA_FLAGS, Mode::empty())
                        .map_err(|_| LocalPublicationError::ExistingMismatch)?;
                let opened = FileIdentity::from_stat(
                    fstat(&descriptor).map_err(|_| LocalPublicationError::ExistingMismatch)?,
                )?;
                check_cancelled(&mut is_cancelled)?;
                let named = FileIdentity::from_stat(
                    statat(&fanout_directory, name, AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(|_| LocalPublicationError::ExistingMismatch)?,
                )?;
                if before != opened || before != named {
                    return Err(LocalPublicationError::ExistingMismatch);
                }
                files.push(EnumeratedDigestFile {
                    digest,
                    byte_length: before.bytes,
                });
            }
            confirm_named_directory(&sha256_directory, &fanout_name, expected_fanout_identity)?;
        }
        confirm_named_directory(
            &namespace_directory,
            OsStr::new("sha256"),
            expected_sha256_identity,
        )?;
        confirm_named_directory(
            &self.root,
            OsStr::new(namespace.directory()),
            namespace_identity,
        )?;
        files.sort_unstable_by_key(|file| file.digest);
        Ok(files)
    }

    /// Validates the complete fixed-ref namespace and returns every bounded
    /// named pin record. Unknown entries and malformed checkpoint names fail
    /// closed.
    pub(crate) fn read_pin_refs<C>(
        &self,
        limits: ProjectStoreLimits,
        mut is_cancelled: C,
    ) -> Result<Vec<(String, RefRecord)>, LocalPublicationError>
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
            Err(Errno::NOENT) => return Ok(Vec::new()),
            Err(_) => return Err(LocalPublicationError::InvalidControl),
        };
        let mut entries = 0_usize;
        let mut pins = Vec::new();
        for entry in Dir::read_from(&refs).map_err(|_| LocalPublicationError::InvalidControl)? {
            check_cancelled(&mut is_cancelled)?;
            let entry = entry.map_err(|_| LocalPublicationError::InvalidControl)?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
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
            if name == b"head"
                || name == b"recovery"
                || name == b"autosave-head"
                || name == b"autosave-recovery"
            {
                continue;
            }
            if name != b"pins" {
                return Err(LocalPublicationError::InvalidControl);
            }
            let pin_directory = openat(
                &refs,
                OsStr::new("pins"),
                DIRECTORY_OPEN_FLAGS,
                Mode::empty(),
            )
            .map_err(|_| LocalPublicationError::InvalidControl)?;
            for pin in
                Dir::read_from(&pin_directory).map_err(|_| LocalPublicationError::InvalidControl)?
            {
                check_cancelled(&mut is_cancelled)?;
                let pin = pin.map_err(|_| LocalPublicationError::InvalidControl)?;
                let pin_name = pin.file_name().to_bytes();
                if pin_name == b"." || pin_name == b".." {
                    continue;
                }
                if pins.len() >= limits.pin_refs_max {
                    return Err(LocalPublicationError::Capacity {
                        declared: u64::try_from(pins.len().saturating_add(1)).unwrap_or(u64::MAX),
                        maximum: u64::try_from(limits.pin_refs_max).unwrap_or(u64::MAX),
                    });
                }
                if !valid_checkpoint_name(pin_name) {
                    return Err(LocalPublicationError::InvalidControl);
                }
                let path = PathBuf::from("refs")
                    .join("pins")
                    .join(OsStr::from_bytes(pin_name));
                let bytes = self
                    .read_optional_small_file(
                        &path,
                        u64::try_from(limits.ref_record_bytes_max)
                            .map_err(|_| LocalPublicationError::InvalidControl)?,
                        &mut is_cancelled,
                    )?
                    .ok_or(LocalPublicationError::InvalidControl)?;
                if bytes.len() != limits.ref_record_bytes_exact || bytes.len() != REF_BYTES {
                    return Err(LocalPublicationError::InvalidControl);
                }
                let checkpoint_id = str::from_utf8(pin_name)
                    .map_err(|_| LocalPublicationError::InvalidControl)?
                    .to_owned();
                let record = RefRecord::decode(RefKind::Pin, &bytes)
                    .map_err(|_| LocalPublicationError::InvalidControl)?;
                pins.push((checkpoint_id, record));
            }
        }
        Ok(pins)
    }

    pub(crate) fn read_pin_ref<C>(
        &self,
        checkpoint_id: &str,
        limits: ProjectStoreLimits,
        is_cancelled: C,
    ) -> Result<Option<RefRecord>, LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        let path = pin_relative_path(checkpoint_id)?;
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
        RefRecord::decode(RefKind::Pin, &bytes)
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

    /// Bounds every physical entry and every directory's immediate fan-out
    /// through held descriptors. `pending_fixed_refs` reserves permanent ref
    /// entries which the caller will create after this scan. When a ref will
    /// be published, the projection also covers a missing persistent staging
    /// directory and the peak private transaction entries.
    pub(crate) fn validate_store_inventory<C>(
        &self,
        limits: ProjectStoreLimits,
        pending_fixed_refs: usize,
        replaces_fixed_ref: bool,
        is_cancelled: C,
    ) -> Result<(), LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        self.validate_store_inventory_mode(
            limits,
            pending_fixed_refs,
            0,
            replaces_fixed_ref,
            true,
            is_cancelled,
        )
    }

    /// Read-side inventory validation never follows the writer-private
    /// staging subtree. A live writer may remove a completed stage while a
    /// read-only session is opening; staging is not read authority.
    pub(crate) fn validate_read_inventory<C>(
        &self,
        limits: ProjectStoreLimits,
        is_cancelled: C,
    ) -> Result<(), LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        self.validate_store_inventory_mode(limits, 0, 0, false, false, is_cancelled)
    }

    /// Requires the exact root namespace produced by one initial package
    /// publication. Generation, object, and ref contents are validated by the
    /// transaction graph; this closes the remaining gap for foreign root
    /// entries and requires the writer-private staging directory to be empty.
    pub(crate) fn validate_initial_package_namespace<C>(
        &self,
        limits: ProjectStoreLimits,
        mut is_cancelled: C,
    ) -> Result<(), LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        check_cancelled(&mut is_cancelled)?;
        let mut seen = [false; 5];
        let mut entries = 0_usize;
        for entry in
            Dir::read_from(&self.root).map_err(|_| LocalPublicationError::ExistingMismatch)?
        {
            check_cancelled(&mut is_cancelled)?;
            let entry = entry.map_err(|_| LocalPublicationError::ExistingMismatch)?;
            let name = entry.file_name();
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            entries = checked_count_add(entries, 1, limits.directory_fanout_entries_max)?;
            let slot = match name.to_bytes() {
                b"project.json" => 0,
                b"refs" => 1,
                b"generations" => 2,
                b"objects" => 3,
                b"staging" => 4,
                _ => return Err(LocalPublicationError::ExistingMismatch),
            };
            if seen[slot] {
                return Err(LocalPublicationError::ExistingMismatch);
            }
            seen[slot] = true;
            let stat = statat(&self.root, name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| LocalPublicationError::ExistingMismatch)?;
            if slot == 0 {
                FileIdentity::from_stat(stat)?;
                continue;
            }
            let expected = DirectoryIdentity::from_stat(stat)?;
            let directory = openat(&self.root, name, DIRECTORY_OPEN_FLAGS, Mode::empty())
                .map_err(|_| LocalPublicationError::ExistingMismatch)?;
            let opened = DirectoryIdentity::from_stat(
                fstat(&directory).map_err(|_| LocalPublicationError::ExistingMismatch)?,
            )?;
            if opened != expected {
                return Err(LocalPublicationError::ExistingMismatch);
            }
            if slot == 4 {
                for staged in Dir::read_from(&directory)
                    .map_err(|_| LocalPublicationError::ExistingMismatch)?
                {
                    check_cancelled(&mut is_cancelled)?;
                    let staged = staged.map_err(|_| LocalPublicationError::ExistingMismatch)?;
                    let staged_name = staged.file_name().to_bytes();
                    if staged_name != b"." && staged_name != b".." {
                        return Err(LocalPublicationError::ExistingMismatch);
                    }
                }
            }
            let named = DirectoryIdentity::from_stat(
                statat(&self.root, name, AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(|_| LocalPublicationError::ExistingMismatch)?,
            )?;
            if named != opened {
                return Err(LocalPublicationError::ExistingMismatch);
            }
        }
        if entries != seen.len() || seen.into_iter().any(|present| !present) {
            return Err(LocalPublicationError::ExistingMismatch);
        }
        Ok(())
    }

    pub(crate) fn validate_pin_inventory<C>(
        &self,
        limits: ProjectStoreLimits,
        creates_pin: bool,
        is_cancelled: C,
    ) -> Result<(), LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        self.validate_store_inventory_mode(
            limits,
            0,
            usize::from(creates_pin),
            !creates_pin,
            true,
            is_cancelled,
        )
    }

    fn validate_store_inventory_mode<C>(
        &self,
        limits: ProjectStoreLimits,
        pending_fixed_refs: usize,
        pending_pin_refs: usize,
        replaces_fixed_ref: bool,
        inspect_staging: bool,
        mut is_cancelled: C,
    ) -> Result<(), LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        check_cancelled(&mut is_cancelled)?;
        let publishes_ref = pending_fixed_refs != 0 || pending_pin_refs != 0 || replaces_fixed_ref;
        if publishes_ref {
            let publication_descriptors = REF_PUBLICATION_DESCRIPTORS
                .checked_add(self.external_held_descriptors)
                .ok_or_else(|| capacity_overflow(limits.open_file_descriptors_max))?;
            enforce_count(publication_descriptors, limits.open_file_descriptors_max)?;
        }
        let root = duplicate_directory(&self.root)?;
        let mut inventory = StoreInventory {
            inspect_staging,
            ..StoreInventory::default()
        };
        inspect_store_directory(
            &root,
            InventoryDirectory::Root,
            0,
            self.external_held_descriptors,
            limits,
            &mut inventory,
            &mut is_cancelled,
        )?;

        let creates_refs_directory = usize::from(
            (pending_fixed_refs != 0 || pending_pin_refs != 0) && inventory.refs_entries.is_none(),
        );
        let creates_pins_directory =
            usize::from(pending_pin_refs != 0 && inventory.pins_entries.is_none());
        let creates_staging_directory =
            usize::from(publishes_ref && inventory.staging_entries.is_none());
        let private_transaction_entries = if publishes_ref {
            // One private transaction directory is always additional. A
            // replacement also holds its staged payload beside the existing
            // destination until rename, and its preceding recovery stage may
            // remain after best-effort cleanup. A create's payload is already
            // counted by its pending persistent ref.
            1_usize
                .checked_add(usize::from(replaces_fixed_ref) * 2)
                .ok_or_else(|| capacity_overflow(limits.physical_store_entries_max))?
        } else {
            0
        };
        let projected_additions = pending_fixed_refs
            .checked_add(pending_pin_refs)
            .and_then(|value| value.checked_add(creates_refs_directory))
            .and_then(|value| value.checked_add(creates_pins_directory))
            .and_then(|value| value.checked_add(creates_staging_directory))
            .and_then(|value| value.checked_add(private_transaction_entries))
            .ok_or_else(|| capacity_overflow(limits.physical_store_entries_max))?;
        enforce_count(
            checked_count_add(
                inventory.entries,
                projected_additions,
                limits.physical_store_entries_max,
            )?,
            limits.physical_store_entries_max,
        )?;
        enforce_count(
            checked_count_add(
                inventory.root_entries,
                creates_refs_directory
                    .checked_add(creates_staging_directory)
                    .ok_or_else(|| capacity_overflow(limits.directory_fanout_entries_max))?,
                limits.directory_fanout_entries_max,
            )?,
            limits.directory_fanout_entries_max,
        )?;
        enforce_count(
            checked_count_add(
                inventory.refs_entries.unwrap_or(0),
                pending_fixed_refs
                    .checked_add(creates_pins_directory)
                    .ok_or_else(|| capacity_overflow(limits.directory_fanout_entries_max))?,
                limits.directory_fanout_entries_max,
            )?,
            limits.directory_fanout_entries_max,
        )?;
        enforce_count(
            checked_count_add(
                inventory.pins_entries.unwrap_or(0),
                pending_pin_refs,
                limits.directory_fanout_entries_max,
            )?,
            limits.directory_fanout_entries_max,
        )?;
        enforce_count(
            checked_count_add(
                inventory.pins_entries.unwrap_or(0),
                pending_pin_refs,
                limits.pin_refs_max,
            )?,
            limits.pin_refs_max,
        )?;
        if publishes_ref {
            enforce_count(
                checked_count_add(
                    inventory.staging_entries.unwrap_or(0),
                    1_usize
                        .checked_add(usize::from(replaces_fixed_ref))
                        .ok_or_else(|| capacity_overflow(limits.directory_fanout_entries_max))?,
                    limits.directory_fanout_entries_max,
                )?,
                limits.directory_fanout_entries_max,
            )?;
        }
        Ok(())
    }

    /// Publishes the first manual head exactly once. There is deliberately no
    /// replacement path here; established commits use the separately checked
    /// recovery-before-head transaction.
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
                let staging_result = sync_with_retry(&stage.directory);
                let destination_result = sync_with_retry(&destination_parent);
                if staging_result.is_err() || destination_result.is_err() {
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

    /// Atomically replaces or creates one validated fixed ref after comparing
    /// the exact visible predecessor. The caller owns lane policy and holds the
    /// writer lease. Cancellation is ignored after rename until both affected
    /// directories have been synchronized.
    pub(crate) fn replace_ref<C>(
        &self,
        expected: Option<RefRecord>,
        next: RefRecord,
        limits: ProjectStoreLimits,
        mut is_cancelled: C,
    ) -> Result<(), LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        if next.kind() == RefKind::Pin
            || limits.ref_record_bytes_exact != REF_BYTES
            || expected.is_some_and(|record| {
                record.kind() != next.kind() || record.project_id() != next.project_id()
            })
        {
            return Err(LocalPublicationError::InvalidControl);
        }
        if self.read_ref(next.kind(), limits, &mut is_cancelled)? != expected {
            return Err(LocalPublicationError::RefChanged);
        }

        let destination = ref_relative_path(next.kind())?;
        let (destination_parent, destination_name) = self.open_or_create_parent(&destination)?;
        let mut stage = Stage::begin(self)?;
        let mut staged_file = stage.create_file()?;
        staged_file
            .write_all(&next.encode())
            .map_err(|source| LocalPublicationError::Io {
                operation: "write a staged project ref",
                source,
            })?;
        fsync(&staged_file).map_err(|error| io_error("synchronize a staged project ref", error))?;
        drop(staged_file);
        check_cancelled(&mut is_cancelled)?;
        if self.read_ref(next.kind(), limits, &mut is_cancelled)? != expected {
            return Err(LocalPublicationError::RefChanged);
        }

        let replaced = if expected.is_some() {
            renameat(
                &stage.directory,
                OsStr::new(STAGED_FILE),
                &destination_parent,
                &destination_name,
            )
        } else {
            renameat_with(
                &stage.directory,
                OsStr::new(STAGED_FILE),
                &destination_parent,
                &destination_name,
                RenameFlags::NOREPLACE,
            )
        };
        match replaced {
            Ok(()) => {
                stage.file_owned = false;
                let staging_result = self.sync_ref_commit_directory(&stage.directory);
                // Always attempt the authoritative destination sync even when
                // source-name removal could not be proven durable.
                let destination_result = self.sync_ref_commit_directory(&destination_parent);
                if staging_result.is_err() || destination_result.is_err() {
                    return Err(LocalPublicationError::RefCommitIndeterminate);
                }
                Ok(())
            }
            Err(Errno::EXIST) => Err(LocalPublicationError::RefChanged),
            Err(error)
                if error == Errno::NOSYS || error == Errno::INVAL || error == Errno::OPNOTSUPP =>
            {
                Err(LocalPublicationError::AtomicPublishUnsupported)
            }
            Err(error) => Err(io_error("replace a project ref", error)),
        }
    }

    pub(crate) fn replace_pin<C, O>(
        &self,
        checkpoint_id: &str,
        expected: Option<RefRecord>,
        next: RefRecord,
        limits: ProjectStoreLimits,
        observer: &O,
        mut is_cancelled: C,
    ) -> Result<(), LocalPublicationError>
    where
        C: FnMut() -> bool,
        O: PinTransitionObserver,
    {
        let destination = pin_relative_path(checkpoint_id)?;
        if next.kind() != RefKind::Pin
            || limits.ref_record_bytes_exact != REF_BYTES
            || expected.is_some_and(|record| {
                record.kind() != RefKind::Pin || record.project_id() != next.project_id()
            })
        {
            return Err(LocalPublicationError::InvalidControl);
        }
        if self.read_pin_ref(checkpoint_id, limits, &mut is_cancelled)? != expected {
            return Err(LocalPublicationError::RefChanged);
        }

        let stage_create = observer
            .before(PinTransition::StageCreate)
            .map_err(|()| injected_pin_transition("create a staged project pin"))?;
        let (destination_parent, destination_name) = self.open_or_create_parent(&destination)?;
        let mut stage = Stage::begin(self)?;
        let mut staged_file = stage.create_file()?;
        observer
            .after(stage_create)
            .map_err(|()| injected_pin_transition("create a staged project pin"))?;
        let write = observer
            .before(PinTransition::Write)
            .map_err(|()| injected_pin_transition("write a staged project pin"))?;
        staged_file
            .write_all(&next.encode())
            .map_err(|source| LocalPublicationError::Io {
                operation: "write a staged project pin",
                source,
            })?;
        observer
            .after(write)
            .map_err(|()| injected_pin_transition("write a staged project pin"))?;
        let file_sync = observer
            .before(PinTransition::FileSync)
            .map_err(|()| injected_pin_transition("synchronize a staged project pin"))?;
        fsync(&staged_file).map_err(|error| io_error("synchronize a staged project pin", error))?;
        observer
            .after(file_sync)
            .map_err(|()| injected_pin_transition("synchronize a staged project pin"))?;
        drop(staged_file);
        check_cancelled(&mut is_cancelled)?;
        if self.read_pin_ref(checkpoint_id, limits, &mut is_cancelled)? != expected {
            return Err(LocalPublicationError::RefChanged);
        }

        let replace = observer
            .before(PinTransition::Replace)
            .map_err(|()| injected_pin_transition("replace a project pin"))?;
        let replaced = if expected.is_some() {
            renameat(
                &stage.directory,
                OsStr::new(STAGED_FILE),
                &destination_parent,
                &destination_name,
            )
        } else {
            renameat_with(
                &stage.directory,
                OsStr::new(STAGED_FILE),
                &destination_parent,
                &destination_name,
                RenameFlags::NOREPLACE,
            )
        };
        match replaced {
            Ok(()) => {
                stage.file_owned = false;
                observer
                    .after(replace)
                    .map_err(|()| LocalPublicationError::RefCommitIndeterminate)?;
                let staging_result = self.sync_pin_transition_directory(
                    &stage.directory,
                    PinTransition::DirectorySync,
                    observer,
                );
                // The destination remains the authority: always attempt its
                // sync even if staging-name removal could not be established.
                let destination_result = self.sync_pin_transition_directory(
                    &destination_parent,
                    PinTransition::DirectorySync,
                    observer,
                );
                if staging_result.is_err() || destination_result.is_err() {
                    return Err(LocalPublicationError::RefCommitIndeterminate);
                }
                Ok(())
            }
            Err(Errno::EXIST) => Err(LocalPublicationError::RefChanged),
            Err(error)
                if error == Errno::NOSYS || error == Errno::INVAL || error == Errno::OPNOTSUPP =>
            {
                Err(LocalPublicationError::AtomicPublishUnsupported)
            }
            Err(error) => Err(io_error("replace a project pin", error)),
        }
    }

    pub(crate) fn remove_pin<C, O>(
        &self,
        checkpoint_id: &str,
        expected: RefRecord,
        limits: ProjectStoreLimits,
        observer: &O,
        mut is_cancelled: C,
    ) -> Result<(), LocalPublicationError>
    where
        C: FnMut() -> bool,
        O: PinTransitionObserver,
    {
        let path = pin_relative_path(checkpoint_id)?;
        if expected.kind() != RefKind::Pin
            || self.read_pin_ref(checkpoint_id, limits, &mut is_cancelled)? != Some(expected)
        {
            return Err(LocalPublicationError::RefChanged);
        }
        let Some((parent, name)) = self.open_existing_parent_if_present(&path)? else {
            return Err(LocalPublicationError::RefChanged);
        };
        check_cancelled(&mut is_cancelled)?;
        if self.read_pin_ref(checkpoint_id, limits, &mut is_cancelled)? != Some(expected) {
            return Err(LocalPublicationError::RefChanged);
        }
        let remove = observer
            .before(PinTransition::UnpinRemove)
            .map_err(|()| injected_pin_transition("remove a project pin"))?;
        match unlinkat(&parent, &name, AtFlags::empty()) {
            Ok(()) => {
                observer
                    .after(remove)
                    .map_err(|()| LocalPublicationError::RefCommitIndeterminate)?;
                self.sync_pin_transition_directory(
                    &parent,
                    PinTransition::UnpinDirectorySync,
                    observer,
                )
            }
            Err(Errno::NOENT) => Err(LocalPublicationError::RefChanged),
            Err(error) => Err(io_error("remove a project pin", error)),
        }
    }

    pub(crate) fn sync_pin_recovery<C, O>(
        &self,
        checkpoint_id: &str,
        expected: Option<RefRecord>,
        transition: PinTransition,
        limits: ProjectStoreLimits,
        observer: &O,
        mut is_cancelled: C,
    ) -> Result<(), LocalPublicationError>
    where
        C: FnMut() -> bool,
        O: PinTransitionObserver,
    {
        debug_assert!(matches!(
            transition,
            PinTransition::DirectorySync | PinTransition::UnpinDirectorySync
        ));
        let path = pin_relative_path(checkpoint_id)?;
        let Some((parent, _)) = self.open_existing_parent_if_present(&path)? else {
            return if expected.is_none() {
                Ok(())
            } else {
                Err(LocalPublicationError::RefChanged)
            };
        };
        if self.read_pin_ref(checkpoint_id, limits, &mut is_cancelled)? != expected {
            return Err(LocalPublicationError::RefChanged);
        }
        check_cancelled(&mut is_cancelled)?;
        self.sync_pin_transition_directory(&parent, transition, observer)?;
        if self.read_pin_ref(checkpoint_id, limits, &mut is_cancelled)? != expected {
            return Err(LocalPublicationError::RefChanged);
        }
        Ok(())
    }

    fn sync_pin_transition_directory<O>(
        &self,
        directory: &OwnedFd,
        transition: PinTransition,
        observer: &O,
    ) -> Result<(), LocalPublicationError>
    where
        O: PinTransitionObserver,
    {
        let occurrence = observer
            .before(transition)
            .map_err(|()| LocalPublicationError::RefCommitIndeterminate)?;
        self.sync_ref_commit_directory(directory)
            .map_err(|_| LocalPublicationError::RefCommitIndeterminate)?;
        observer
            .after(occurrence)
            .map_err(|()| LocalPublicationError::RefCommitIndeterminate)
    }

    fn sync_ref_commit_directory(&self, directory: &OwnedFd) -> Result<(), Errno> {
        #[cfg(test)]
        {
            let occurrence = self
                .ref_commit_directory_sync_count
                .fetch_add(1, Ordering::AcqRel);
            if occurrence
                == self
                    .ref_commit_directory_sync_failure
                    .load(Ordering::Acquire)
            {
                return Err(Errno::IO);
            }
        }
        sync_with_retry(directory)
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

    /// Streams and verifies one exact object without synchronizing or
    /// otherwise mutating the store. Every path component is opened relative
    /// to the held root, and the opened file must remain the same named,
    /// single-link regular file across the complete read.
    pub(crate) fn verify_exact_object<C, F>(
        &self,
        digest: ExactBytesDigest,
        byte_length: u64,
        maximum_bytes: u64,
        stream_buffer_bytes: usize,
        mut is_cancelled: C,
        mut on_chunk: F,
    ) -> Result<ExactBytesFacts, LocalPublicationError>
    where
        C: FnMut() -> bool,
        F: FnMut(&[u8]),
    {
        let expected = DeclaredExactFile::new(digest, byte_length);
        validate_capacity(expected, maximum_bytes)?;
        if stream_buffer_bytes == 0 {
            return Err(LocalPublicationError::Capacity {
                declared: 0,
                maximum: u64::try_from(STREAM_BUFFER_BYTES).expect("the stream limit fits u64"),
            });
        }
        let stream_buffer_bytes = stream_buffer_bytes.min(STREAM_BUFFER_BYTES);
        check_cancelled(&mut is_cancelled)?;

        let (parent, name) = self.open_existing_parent(&object_relative_path(digest))?;
        let named_before = FileIdentity::from_stat(
            statat(&parent, &name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| LocalPublicationError::ExistingMismatch)?,
        )?;
        let descriptor = openat(&parent, &name, FILE_READ_FLAGS, Mode::empty())
            .map_err(|_| LocalPublicationError::ExistingMismatch)?;
        let opened_before = FileIdentity::from_stat(
            fstat(&descriptor).map_err(|_| LocalPublicationError::ExistingMismatch)?,
        )?;
        if named_before != opened_before {
            return Err(LocalPublicationError::ExistingMismatch);
        }
        if opened_before.bytes != byte_length {
            return Err(LocalPublicationError::SourceLength {
                expected: byte_length,
            });
        }

        let mut file = File::from(descriptor);
        let mut hasher = ExactBytesHasher::new();
        let mut observed = 0_u64;
        let mut buffer = vec![0_u8; stream_buffer_bytes];
        loop {
            check_cancelled(&mut is_cancelled)?;
            let read = match file.read(&mut buffer) {
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(source) => {
                    return Err(LocalPublicationError::Io {
                        operation: "stream an exact project object",
                        source,
                    });
                }
            };
            if read == 0 {
                break;
            }
            observed = observed
                .checked_add(u64::try_from(read).map_err(|_| {
                    LocalPublicationError::SourceLength {
                        expected: byte_length,
                    }
                })?)
                .ok_or(LocalPublicationError::SourceLength {
                    expected: byte_length,
                })?;
            if observed > byte_length {
                return Err(LocalPublicationError::SourceLength {
                    expected: byte_length,
                });
            }
            on_chunk(&buffer[..read]);
            hasher
                .update(&buffer[..read])
                .map_err(|_| LocalPublicationError::SourceLength {
                    expected: byte_length,
                })?;
        }
        if observed != byte_length {
            return Err(LocalPublicationError::SourceLength {
                expected: byte_length,
            });
        }
        let facts = hasher
            .finalize()
            .map_err(|_| LocalPublicationError::SourceLength {
                expected: byte_length,
            })?;
        check_cancelled(&mut is_cancelled)?;

        let opened_after = FileIdentity::from_stat(
            fstat(&file).map_err(|_| LocalPublicationError::ExistingMismatch)?,
        )?;
        let named_after = FileIdentity::from_stat(
            statat(&parent, &name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| LocalPublicationError::ExistingMismatch)?,
        )?;
        if opened_before != opened_after || opened_before != named_after {
            return Err(LocalPublicationError::ExistingMismatch);
        }
        if facts.byte_length() != byte_length {
            return Err(LocalPublicationError::SourceLength {
                expected: byte_length,
            });
        }
        if facts.digest() != digest {
            return Err(LocalPublicationError::SourceDigest);
        }
        check_cancelled(&mut is_cancelled)?;
        Ok(facts)
    }

    /// Validates only the immutable object's descriptor-relative file
    /// identity and declared length. Bulk bytes are deliberately not read or
    /// hashed during eager store inspection.
    pub(crate) fn validate_exact_object_metadata<C>(
        &self,
        digest: ExactBytesDigest,
        byte_length: u64,
        maximum_bytes: u64,
        mut is_cancelled: C,
    ) -> Result<(), LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        let expected = DeclaredExactFile::new(digest, byte_length);
        validate_capacity(expected, maximum_bytes)?;
        check_cancelled(&mut is_cancelled)?;
        let (parent, name) = self.open_existing_parent(&object_relative_path(digest))?;
        let descriptor = openat(&parent, &name, FILE_METADATA_FLAGS, Mode::empty())
            .map_err(|_| LocalPublicationError::ExistingMismatch)?;
        let opened = FileIdentity::from_stat(
            fstat(&descriptor).map_err(|_| LocalPublicationError::ExistingMismatch)?,
        )?;
        if opened.bytes != byte_length {
            return Err(LocalPublicationError::ExistingMismatch);
        }
        let named = FileIdentity::from_stat(
            statat(&parent, &name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| LocalPublicationError::ExistingMismatch)?,
        )?;
        check_cancelled(&mut is_cancelled)?;
        if opened != named {
            return Err(LocalPublicationError::ExistingMismatch);
        }
        Ok(())
    }

    /// Reads one bounded control object and validates its exact digest without
    /// synchronizing or otherwise mutating the store.
    pub(crate) fn read_exact_object_bytes<C>(
        &self,
        digest: ExactBytesDigest,
        byte_length: u64,
        maximum_bytes: u64,
        is_cancelled: C,
    ) -> Result<Vec<u8>, LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        let expected = DeclaredExactFile::new(digest, byte_length);
        validate_capacity(expected, maximum_bytes)?;
        let bytes = self
            .read_optional_small_file(&object_relative_path(digest), maximum_bytes, is_cancelled)?
            .ok_or(LocalPublicationError::ExistingMismatch)?;
        if u64::try_from(bytes.len()).ok() != Some(byte_length)
            || ExactBytesHasher::hash(&bytes)
                .map_err(|_| LocalPublicationError::ExistingMismatch)?
                .digest()
                != digest
        {
            return Err(LocalPublicationError::ExistingMismatch);
        }
        Ok(bytes)
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

impl SiblingPackageStage {
    pub(crate) fn begin(
        destination: &ProjectStorePath,
        limits: ProjectStoreLimits,
    ) -> Result<Self, LocalPublicationError> {
        let cleanup_limits = PackageCleanupLimits::for_initial_package(limits)?;
        let path = destination.as_path();
        let destination_name = path
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or(LocalPublicationError::InvalidPath)?
            .to_os_string();
        let parent_path = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent_root = LocalStoreRoot::open(parent_path)?;
        let parent = duplicate_directory(parent_root.descriptor())?;
        match statat(&parent, &destination_name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => return Err(LocalPublicationError::DestinationExists),
            Err(Errno::NOENT) => {}
            Err(error) => return Err(io_error("inspect the package destination", error)),
        }

        for attempt in 0..STAGE_CREATE_ATTEMPTS {
            let stage_name = unique_package_stage_name(attempt);
            match mkdirat(&parent, &stage_name, Mode::RWXU) {
                Ok(()) => {
                    let stage =
                        match openat(&parent, &stage_name, DIRECTORY_OPEN_FLAGS, Mode::empty()) {
                            Ok(stage) => stage,
                            Err(error) => {
                                let _ = unlinkat(&parent, &stage_name, AtFlags::REMOVEDIR);
                                return Err(io_error("open the sibling package stage", error));
                            }
                        };
                    let stage_identity = match fstat(&stage)
                        .map_err(|error| io_error("identify the sibling package stage", error))
                        .and_then(DirectoryIdentity::from_stat)
                    {
                        Ok(identity) => identity,
                        Err(error) => {
                            drop(stage);
                            let _ = unlinkat(&parent, &stage_name, AtFlags::REMOVEDIR);
                            return Err(error);
                        }
                    };
                    let root = LocalStoreRoot {
                        root: stage,
                        external_held_descriptors: 1,
                        #[cfg(test)]
                        ref_commit_directory_sync_count: AtomicUsize::new(0),
                        #[cfg(test)]
                        ref_commit_directory_sync_failure: AtomicUsize::new(usize::MAX),
                    };
                    return Ok(Self {
                        parent,
                        destination_name,
                        stage_name,
                        stage_identity,
                        root: Some(root),
                        cleanup_limits,
                        owns_stage: true,
                    });
                }
                Err(Errno::EXIST) => continue,
                Err(error) => {
                    return Err(io_error("create the sibling package stage", error));
                }
            }
        }
        Err(LocalPublicationError::Io {
            operation: "create a unique sibling package stage",
            source: io::Error::new(
                io::ErrorKind::AlreadyExists,
                "all bounded package-stage names collided",
            ),
        })
    }

    pub(crate) fn root(&self) -> &LocalStoreRoot {
        self.root
            .as_ref()
            .expect("the staged root exists until install")
    }

    pub(crate) fn sync_tree<C>(
        &self,
        limits: ProjectStoreLimits,
        mut is_cancelled: C,
    ) -> Result<(), LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        let mut entries = 0_usize;
        sync_package_tree(
            self.root().descriptor(),
            0,
            limits,
            &mut entries,
            &mut is_cancelled,
        )
    }

    /// Installs the already-synchronized package. Cancellation is ignored
    /// after the successful rename until destination-parent durability is
    /// established.
    pub(crate) fn install<C>(self, is_cancelled: C) -> Result<LocalStoreRoot, LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        self.install_inner(is_cancelled, false)
    }

    #[cfg(test)]
    pub(crate) fn install_with_parent_sync_failure<C>(
        self,
        is_cancelled: C,
    ) -> Result<LocalStoreRoot, LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        self.install_inner(is_cancelled, true)
    }

    fn install_inner<C>(
        mut self,
        mut is_cancelled: C,
        fail_parent_sync: bool,
    ) -> Result<LocalStoreRoot, LocalPublicationError>
    where
        C: FnMut() -> bool,
    {
        check_cancelled(&mut is_cancelled)?;
        if !self.stage_name_still_owned()? {
            return Err(LocalPublicationError::ExistingMismatch);
        }
        match renameat_with(
            &self.parent,
            &self.stage_name,
            &self.parent,
            &self.destination_name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => self.owns_stage = false,
            Err(Errno::EXIST) => return Err(LocalPublicationError::DestinationExists),
            Err(error)
                if error == Errno::NOSYS || error == Errno::INVAL || error == Errno::OPNOTSUPP =>
            {
                return Err(LocalPublicationError::AtomicPublishUnsupported);
            }
            Err(error) => return Err(io_error("install the staged project package", error)),
        }
        if fail_parent_sync || sync_with_retry(&self.parent).is_err() {
            return Err(LocalPublicationError::PackageCommitIndeterminate);
        }
        let mut root = self
            .root
            .take()
            .expect("the installed root remains descriptor-held");
        root.external_held_descriptors = 0;
        Ok(root)
    }

    fn stage_name_still_owned(&self) -> Result<bool, LocalPublicationError> {
        match statat(&self.parent, &self.stage_name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => Ok(FileType::from_raw_mode(stat.st_mode) == FileType::Directory
                && DirectoryIdentity::from_stat(stat)? == self.stage_identity),
            Err(Errno::NOENT) => Ok(false),
            Err(error) => Err(io_error("revalidate the sibling package stage", error)),
        }
    }
}

impl Drop for SiblingPackageStage {
    fn drop(&mut self) {
        if !self.owns_stage {
            return;
        }
        if let Some(root) = &self.root {
            let mut entries = 0_usize;
            let _ =
                remove_directory_contents(root.descriptor(), 0, self.cleanup_limits, &mut entries);
        }
        if self.stage_name_still_owned().unwrap_or(false) {
            let _ = unlinkat(&self.parent, &self.stage_name, AtFlags::REMOVEDIR);
        }
    }
}

fn inspect_store_directory<C>(
    directory: &OwnedFd,
    role: InventoryDirectory,
    depth: usize,
    external_held_descriptors: usize,
    limits: ProjectStoreLimits,
    inventory: &mut StoreInventory,
    is_cancelled: &mut C,
) -> Result<(), LocalPublicationError>
where
    C: FnMut() -> bool,
{
    check_cancelled(is_cancelled)?;
    let held_descriptors = WRITABLE_SESSION_DESCRIPTORS
        .checked_add(external_held_descriptors)
        .and_then(|value| value.checked_add(depth))
        .and_then(|value| value.checked_add(2))
        .ok_or_else(|| capacity_overflow(limits.open_file_descriptors_max))?;
    enforce_count(held_descriptors, limits.open_file_descriptors_max)?;

    let mut fan_out = 0_usize;
    let mut child_directories = Vec::new();
    for entry in Dir::read_from(directory).map_err(|_| LocalPublicationError::ExistingMismatch)? {
        check_cancelled(is_cancelled)?;
        let entry = entry.map_err(|_| LocalPublicationError::ExistingMismatch)?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        fan_out = checked_count_add(fan_out, 1, limits.directory_fanout_entries_max)?;
        enforce_count(fan_out, limits.directory_fanout_entries_max)?;
        inventory.entries =
            checked_count_add(inventory.entries, 1, limits.physical_store_entries_max)?;
        enforce_count(inventory.entries, limits.physical_store_entries_max)?;

        let metadata = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| LocalPublicationError::ExistingMismatch)?;
        match FileType::from_raw_mode(metadata.st_mode) {
            FileType::Directory => {
                let child_role = if role == InventoryDirectory::Root {
                    match name.to_bytes() {
                        b"refs" => InventoryDirectory::Refs,
                        b"staging" => InventoryDirectory::Staging,
                        _ => InventoryDirectory::Other,
                    }
                } else if role == InventoryDirectory::Refs && name.to_bytes() == b"pins" {
                    InventoryDirectory::Pins
                } else {
                    InventoryDirectory::Other
                };
                if child_role != InventoryDirectory::Staging || inventory.inspect_staging {
                    child_directories.push((
                        name.to_owned(),
                        child_role,
                        DirectoryIdentity::from_stat(metadata)?,
                    ));
                }
            }
            FileType::RegularFile if metadata.st_nlink == 1 => {}
            _ => return Err(LocalPublicationError::ExistingMismatch),
        }
    }
    match role {
        InventoryDirectory::Root => inventory.root_entries = fan_out,
        InventoryDirectory::Refs => inventory.refs_entries = Some(fan_out),
        InventoryDirectory::Pins => inventory.pins_entries = Some(fan_out),
        InventoryDirectory::Staging => inventory.staging_entries = Some(fan_out),
        InventoryDirectory::Other => {}
    }

    for (name, child_role, expected_identity) in child_directories {
        check_cancelled(is_cancelled)?;
        let child = openat(directory, &name, DIRECTORY_OPEN_FLAGS, Mode::empty())
            .map_err(|_| LocalPublicationError::ExistingMismatch)?;
        let opened_identity = DirectoryIdentity::from_stat(
            fstat(&child).map_err(|_| LocalPublicationError::ExistingMismatch)?,
        )?;
        if opened_identity != expected_identity {
            return Err(LocalPublicationError::ExistingMismatch);
        }
        inspect_store_directory(
            &child,
            child_role,
            depth
                .checked_add(1)
                .ok_or_else(|| capacity_overflow(limits.open_file_descriptors_max))?,
            external_held_descriptors,
            limits,
            inventory,
            is_cancelled,
        )?;
    }
    Ok(())
}

/// Synchronizes every regular file and directory in a private package before
/// its name can become visible. The walk is descriptor-relative, bounded, and
/// rejects links or namespace changes instead of following them.
fn sync_package_tree<C>(
    directory: &OwnedFd,
    depth: usize,
    limits: ProjectStoreLimits,
    entries: &mut usize,
    is_cancelled: &mut C,
) -> Result<(), LocalPublicationError>
where
    C: FnMut() -> bool,
{
    check_cancelled(is_cancelled)?;
    let held_descriptors = PACKAGE_INSTALL_DESCRIPTORS
        .checked_add(depth)
        .ok_or_else(|| capacity_overflow(limits.open_file_descriptors_max))?;
    enforce_count(held_descriptors, limits.open_file_descriptors_max)?;

    let mut fan_out = 0_usize;
    let mut children = Vec::new();
    for entry in Dir::read_from(directory).map_err(|_| LocalPublicationError::ExistingMismatch)? {
        check_cancelled(is_cancelled)?;
        let entry = entry.map_err(|_| LocalPublicationError::ExistingMismatch)?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        fan_out = checked_count_add(fan_out, 1, limits.directory_fanout_entries_max)?;
        enforce_count(fan_out, limits.directory_fanout_entries_max)?;
        *entries = checked_count_add(*entries, 1, limits.physical_store_entries_max)?;
        enforce_count(*entries, limits.physical_store_entries_max)?;

        let metadata = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| LocalPublicationError::ExistingMismatch)?;
        match FileType::from_raw_mode(metadata.st_mode) {
            FileType::Directory => {
                children.push((name.to_owned(), DirectoryIdentity::from_stat(metadata)?))
            }
            FileType::RegularFile if metadata.st_nlink == 1 => {
                let expected = FileIdentity::from_stat(metadata)?;
                let file = openat(directory, name, FILE_READ_FLAGS, Mode::empty())
                    .map_err(|_| LocalPublicationError::ExistingMismatch)?;
                let before = FileIdentity::from_stat(
                    fstat(&file).map_err(|_| LocalPublicationError::ExistingMismatch)?,
                )?;
                if before != expected {
                    return Err(LocalPublicationError::ExistingMismatch);
                }
                fsync(&file).map_err(|error| io_error("synchronize a package file", error))?;
                check_cancelled(is_cancelled)?;
                let after = FileIdentity::from_stat(
                    fstat(&file).map_err(|_| LocalPublicationError::ExistingMismatch)?,
                )?;
                let named = FileIdentity::from_stat(
                    statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(|_| LocalPublicationError::ExistingMismatch)?,
                )?;
                if before != after || before != named {
                    return Err(LocalPublicationError::ExistingMismatch);
                }
            }
            _ => return Err(LocalPublicationError::ExistingMismatch),
        }
    }
    for (name, expected_identity) in children {
        check_cancelled(is_cancelled)?;
        let child = openat(directory, &name, DIRECTORY_OPEN_FLAGS, Mode::empty())
            .map_err(|_| LocalPublicationError::ExistingMismatch)?;
        let opened_identity = DirectoryIdentity::from_stat(
            fstat(&child).map_err(|_| LocalPublicationError::ExistingMismatch)?,
        )?;
        if opened_identity != expected_identity {
            return Err(LocalPublicationError::ExistingMismatch);
        }
        sync_package_tree(
            &child,
            depth
                .checked_add(1)
                .ok_or_else(|| capacity_overflow(limits.open_file_descriptors_max))?,
            limits,
            entries,
            is_cancelled,
        )?;
        let named_identity = DirectoryIdentity::from_stat(
            statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| LocalPublicationError::ExistingMismatch)?,
        )?;
        if opened_identity != named_identity {
            return Err(LocalPublicationError::ExistingMismatch);
        }
    }

    check_cancelled(is_cancelled)?;
    fsync(directory).map_err(|error| io_error("synchronize a package directory", error))
}

fn checked_count_add(
    current: usize,
    added: usize,
    maximum: usize,
) -> Result<usize, LocalPublicationError> {
    current
        .checked_add(added)
        .ok_or_else(|| capacity_overflow(maximum))
}

fn enforce_count(actual: usize, maximum: usize) -> Result<(), LocalPublicationError> {
    if actual > maximum {
        Err(LocalPublicationError::Capacity {
            declared: u64::try_from(actual).unwrap_or(u64::MAX),
            maximum: u64::try_from(maximum).unwrap_or(u64::MAX),
        })
    } else {
        Ok(())
    }
}

fn capacity_overflow(maximum: usize) -> LocalPublicationError {
    LocalPublicationError::Capacity {
        declared: u64::MAX,
        maximum: u64::try_from(maximum).unwrap_or(u64::MAX),
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

fn pin_relative_path(checkpoint_id: &str) -> Result<PathBuf, LocalPublicationError> {
    if !valid_checkpoint_id(checkpoint_id) {
        return Err(LocalPublicationError::InvalidControl);
    }
    Ok(PathBuf::from("refs").join("pins").join(checkpoint_id))
}

fn valid_checkpoint_name(name: &[u8]) -> bool {
    let Some((&first, rest)) = name.split_first() else {
        return false;
    };
    name.len() <= 64
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && rest.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_' || *byte == b'-'
        })
}

pub(crate) fn valid_checkpoint_id(checkpoint_id: &str) -> bool {
    valid_checkpoint_name(checkpoint_id.as_bytes())
}

fn open_stable_directory_if_present(
    parent: &OwnedFd,
    name: &OsStr,
) -> Result<Option<(OwnedFd, DirectoryIdentity)>, LocalPublicationError> {
    let expected = match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => DirectoryIdentity::from_stat(metadata)?,
        Err(Errno::NOENT) => return Ok(None),
        Err(_) => return Err(LocalPublicationError::ExistingMismatch),
    };
    open_stable_directory(parent, name, expected).map(|directory| Some((directory, expected)))
}

fn open_stable_directory(
    parent: &OwnedFd,
    name: &OsStr,
    expected: DirectoryIdentity,
) -> Result<OwnedFd, LocalPublicationError> {
    let directory = openat(parent, name, DIRECTORY_OPEN_FLAGS, Mode::empty())
        .map_err(|_| LocalPublicationError::ExistingMismatch)?;
    let opened = DirectoryIdentity::from_stat(
        fstat(&directory).map_err(|_| LocalPublicationError::ExistingMismatch)?,
    )?;
    if opened != expected {
        return Err(LocalPublicationError::ExistingMismatch);
    }
    confirm_named_directory(parent, name, expected)?;
    Ok(directory)
}

fn confirm_named_directory(
    parent: &OwnedFd,
    name: &OsStr,
    expected: DirectoryIdentity,
) -> Result<(), LocalPublicationError> {
    let named = DirectoryIdentity::from_stat(
        statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| LocalPublicationError::ExistingMismatch)?,
    )?;
    if named == expected {
        Ok(())
    } else {
        Err(LocalPublicationError::ExistingMismatch)
    }
}

fn is_exact_lower_hex(value: &[u8], length: usize) -> bool {
    value.len() == length
        && value
            .iter()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn parse_digest_namespace_name(
    fanout: &[u8],
    name: &[u8],
    suffix: &[u8],
) -> Result<Sha256Digest, LocalPublicationError> {
    if !is_exact_lower_hex(fanout, 2) {
        return Err(LocalPublicationError::ExistingMismatch);
    }
    let stem = name
        .strip_suffix(suffix)
        .ok_or(LocalPublicationError::ExistingMismatch)?;
    if !is_exact_lower_hex(stem, 62) {
        return Err(LocalPublicationError::ExistingMismatch);
    }
    let mut digest = [0_u8; 64];
    digest[..2].copy_from_slice(fanout);
    digest[2..].copy_from_slice(stem);
    let digest =
        std::str::from_utf8(&digest).map_err(|_| LocalPublicationError::ExistingMismatch)?;
    Sha256Digest::parse(digest).map_err(|_| LocalPublicationError::ExistingMismatch)
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

fn remove_directory_contents(
    directory: &OwnedFd,
    depth: usize,
    limits: PackageCleanupLimits,
    entries: &mut usize,
) -> Result<(), LocalPublicationError> {
    let held_descriptors = PACKAGE_CLEANUP_DESCRIPTORS
        .checked_add(depth)
        .ok_or_else(|| capacity_overflow(limits.open_file_descriptors_max))?;
    enforce_count(held_descriptors, limits.open_file_descriptors_max)?;

    let mut fan_out = 0_usize;
    let mut names = Vec::new();
    for entry in Dir::read_from(directory).map_err(|_| LocalPublicationError::ExistingMismatch)? {
        let entry = entry.map_err(|_| LocalPublicationError::ExistingMismatch)?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        fan_out = checked_count_add(fan_out, 1, limits.directory_fanout_entries_max)?;
        enforce_count(fan_out, limits.directory_fanout_entries_max)?;
        *entries = checked_count_add(*entries, 1, limits.physical_store_entries_max)?;
        enforce_count(*entries, limits.physical_store_entries_max)?;
        names.push(name.to_owned());
    }

    for name in names {
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        let metadata = statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| LocalPublicationError::ExistingMismatch)?;
        if FileType::from_raw_mode(metadata.st_mode) == FileType::Directory {
            let child = openat(directory, &name, DIRECTORY_OPEN_FLAGS, Mode::empty())
                .map_err(|_| LocalPublicationError::ExistingMismatch)?;
            remove_directory_contents(
                &child,
                depth
                    .checked_add(1)
                    .ok_or_else(|| capacity_overflow(limits.open_file_descriptors_max))?,
                limits,
                entries,
            )?;
            unlinkat(directory, &name, AtFlags::REMOVEDIR)
                .map_err(|error| io_error("remove an owned package-stage directory", error))?;
        } else {
            unlinkat(directory, &name, AtFlags::empty())
                .map_err(|error| io_error("remove an owned package-stage file", error))?;
        }
    }
    Ok(())
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

fn unique_package_stage_name(attempt: u64) -> OsString {
    let sequence = STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        ".mirante4d-project-stage-{}-{timestamp:032x}-{sequence:016x}-{attempt:02x}",
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

fn injected_pin_transition(operation: &'static str) -> LocalPublicationError {
    LocalPublicationError::Io {
        operation,
        source: io::Error::other("injected project-store pin transition failure"),
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

    fn extracted_fixture_store(label: &str, store: &str) -> (TestDirectory, PathBuf) {
        let directory = TestDirectory::new(label);
        let status = Command::new("tar")
            .args(["-xzf"])
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../fixtures/project/project-store-v1.tar.gz"),
            )
            .args(["-C"])
            .arg(directory.path())
            .arg(store)
            .status()
            .unwrap();
        assert!(status.success(), "failed to extract {store}");
        let path = directory.path().join(store);
        (directory, path)
    }

    #[test]
    fn canonical_fixture_namespaces_enumerate_exact_sorted_facts() {
        let (_directory, path) = extracted_fixture_store("enumerate-valid", "divergent.m4dproj");
        let root = LocalStoreRoot::open(&path).unwrap();

        let generations = root
            .enumerate_generation_ids(ProjectStoreLimits::default(), || false)
            .unwrap()
            .into_iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            generations,
            [
                "10011b8d7dce93c428e1d117b485746522b4ae1d4d8ee89e359739f2cffd3a10",
                "10447a78680ee73dcc5572d71d81f1ad99079fb1374979a8a7937453a149ae1c",
                "6b91b33dbaa378598269005b027db7a0643e14babe4b7522a5a415a461f6a497",
                "b9af2901b12b248533e53d2683fcf4db7d4b2eb33ef292413b8b5dc2cb8b951e",
            ]
            .map(|digest| format!("{}{}", ProjectGenerationId::PREFIX, digest))
        );
        assert_eq!(
            root.enumerate_object_facts(ProjectStoreLimits::default(), || false)
                .unwrap(),
            vec![(
                ExactBytesDigest::parse(concat!(
                    "sha256:",
                    "f317b2208b90efc088e10edda67cef73f8cedda059cb53538183fa94e12df94d"
                ))
                .unwrap(),
                50,
            )]
        );
    }

    #[test]
    fn namespace_enumeration_rejects_unknown_malformed_and_linked_entries() {
        let (_unknown_directory, unknown_path) =
            extracted_fixture_store("enumerate-unknown", "divergent.m4dproj");
        fs::write(
            unknown_path.join("objects/sha256/README"),
            b"unknown namespace entry",
        )
        .unwrap();
        let unknown_root = LocalStoreRoot::open(&unknown_path).unwrap();
        assert!(matches!(
            unknown_root.enumerate_object_facts(ProjectStoreLimits::default(), || false),
            Err(LocalPublicationError::ExistingMismatch)
        ));

        let (_malformed_directory, malformed_path) =
            extracted_fixture_store("enumerate-malformed", "divergent.m4dproj");
        fs::write(
            malformed_path.join("generations/sha256/10/not-a-generation.json"),
            b"malformed",
        )
        .unwrap();
        let malformed_root = LocalStoreRoot::open(&malformed_path).unwrap();
        assert!(matches!(
            malformed_root.enumerate_generation_ids(ProjectStoreLimits::default(), || false),
            Err(LocalPublicationError::ExistingMismatch)
        ));

        let (_symlink_directory, symlink_path) =
            extracted_fixture_store("enumerate-symlink", "divergent.m4dproj");
        let generation = symlink_path.join(concat!(
            "generations/sha256/10/",
            "011b8d7dce93c428e1d117b485746522b4ae1d4d8ee89e359739f2cffd3a10.json"
        ));
        let outside = symlink_path.join("outside-generation");
        fs::write(&outside, b"outside").unwrap();
        fs::remove_file(&generation).unwrap();
        symlink(&outside, &generation).unwrap();
        let symlink_root = LocalStoreRoot::open(&symlink_path).unwrap();
        assert!(matches!(
            symlink_root.enumerate_generation_ids(ProjectStoreLimits::default(), || false),
            Err(LocalPublicationError::ExistingMismatch)
        ));

        let (_hardlink_directory, hardlink_path) =
            extracted_fixture_store("enumerate-hardlink", "divergent.m4dproj");
        let object = hardlink_path.join(concat!(
            "objects/sha256/f3/",
            "17b2208b90efc088e10edda67cef73f8cedda059cb53538183fa94e12df94d"
        ));
        fs::hard_link(&object, hardlink_path.join("object-hardlink")).unwrap();
        let hardlink_root = LocalStoreRoot::open(&hardlink_path).unwrap();
        assert!(matches!(
            hardlink_root.enumerate_object_facts(ProjectStoreLimits::default(), || false),
            Err(LocalPublicationError::ExistingMismatch)
        ));
    }

    #[test]
    fn namespace_enumeration_enforces_all_work_bounds_and_cancellation() {
        let (_directory, path) = extracted_fixture_store("enumerate-bounds", "divergent.m4dproj");
        let root = LocalStoreRoot::open(&path).unwrap();

        let generation_limit = ProjectStoreLimits {
            generations_scanned_max: 3,
            ..ProjectStoreLimits::default()
        };
        assert!(matches!(
            root.enumerate_generation_ids(generation_limit, || false),
            Err(LocalPublicationError::Capacity { .. })
        ));
        let fanout_limit = ProjectStoreLimits {
            directory_fanout_entries_max: 2,
            ..ProjectStoreLimits::default()
        };
        assert!(matches!(
            root.enumerate_generation_ids(fanout_limit, || false),
            Err(LocalPublicationError::Capacity { .. })
        ));
        let descriptor_limit = ProjectStoreLimits {
            open_file_descriptors_max: DIGEST_ENUMERATION_DESCRIPTORS - 1,
            ..ProjectStoreLimits::default()
        };
        assert!(matches!(
            root.enumerate_object_facts(descriptor_limit, || false),
            Err(LocalPublicationError::Capacity { .. })
        ));
        let physical_limit = ProjectStoreLimits {
            physical_store_entries_max: 3,
            ..ProjectStoreLimits::default()
        };
        assert!(matches!(
            root.enumerate_object_facts(physical_limit, || false),
            Err(LocalPublicationError::Capacity { .. })
        ));
        assert!(matches!(
            root.enumerate_generation_ids(ProjectStoreLimits::default(), || true),
            Err(LocalPublicationError::Cancelled)
        ));
        assert!(matches!(
            root.enumerate_object_facts(ProjectStoreLimits::default(), || true),
            Err(LocalPublicationError::Cancelled)
        ));
        let object = path.join(concat!(
            "objects/sha256/f3/",
            "17b2208b90efc088e10edda67cef73f8cedda059cb53538183fa94e12df94d"
        ));
        fs::OpenOptions::new()
            .write(true)
            .open(object)
            .unwrap()
            .set_len(ProjectStoreLimits::default().object_or_page_bytes_max + 1)
            .unwrap();
        assert!(matches!(
            root.enumerate_object_facts(ProjectStoreLimits::default(), || false),
            Err(LocalPublicationError::Capacity { .. })
        ));
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
    fn package_stage_loses_destination_race_without_clobber_or_leak() {
        let parent = TestDirectory::new("package-race");
        let destination = ProjectStorePath::new(parent.path().join("winner.m4dproj")).unwrap();
        let stage =
            SiblingPackageStage::begin(&destination, ProjectStoreLimits::default()).unwrap();
        fs::write(destination.as_path(), b"racing winner").unwrap();

        assert!(matches!(
            stage.install(|| false),
            Err(LocalPublicationError::DestinationExists)
        ));
        assert_eq!(fs::read(destination.as_path()).unwrap(), b"racing winner");
        let remaining = fs::read_dir(parent.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(
            remaining,
            vec![destination.as_path().file_name().unwrap().to_os_string()]
        );
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
    fn store_inventory_bounds_entries_fanout_and_ref_staging_reservations() {
        let empty = TestDirectory::new("inventory-reservation");
        let empty_root = LocalStoreRoot::open(empty.path()).unwrap();
        let mut limits = ProjectStoreLimits {
            physical_store_entries_max: 4,
            directory_fanout_entries_max: 2,
            ..ProjectStoreLimits::default()
        };
        empty_root
            .validate_store_inventory(limits, 1, false, || false)
            .unwrap();
        limits.open_file_descriptors_max = REF_PUBLICATION_DESCRIPTORS;
        empty_root
            .validate_store_inventory(limits, 1, false, || false)
            .unwrap();
        limits.open_file_descriptors_max = REF_PUBLICATION_DESCRIPTORS - 1;
        assert!(matches!(
            empty_root.validate_store_inventory(limits, 1, false, || false),
            Err(LocalPublicationError::Capacity { .. })
        ));
        limits.open_file_descriptors_max = ProjectStoreLimits::default().open_file_descriptors_max;
        limits.physical_store_entries_max = 3;
        assert!(matches!(
            empty_root.validate_store_inventory(limits, 1, false, || false),
            Err(LocalPublicationError::Capacity { .. })
        ));
        limits.physical_store_entries_max = 4;
        limits.directory_fanout_entries_max = 1;
        assert!(matches!(
            empty_root.validate_store_inventory(limits, 1, false, || false),
            Err(LocalPublicationError::Capacity { .. })
        ));
        limits.physical_store_entries_max = 6;
        limits.directory_fanout_entries_max = 2;
        empty_root
            .validate_store_inventory(limits, 1, true, || false)
            .unwrap();
        limits.physical_store_entries_max = 5;
        assert!(matches!(
            empty_root.validate_store_inventory(limits, 1, true, || false),
            Err(LocalPublicationError::Capacity { .. })
        ));

        let populated = TestDirectory::new("inventory-actual");
        fs::create_dir(populated.path().join("branch")).unwrap();
        fs::write(populated.path().join("branch/a"), b"a").unwrap();
        fs::write(populated.path().join("branch/b"), b"b").unwrap();
        let populated_root = LocalStoreRoot::open(populated.path()).unwrap();
        limits.physical_store_entries_max = 3;
        limits.directory_fanout_entries_max = 2;
        populated_root
            .validate_store_inventory(limits, 0, false, || false)
            .unwrap();
        limits.physical_store_entries_max = 2;
        assert!(matches!(
            populated_root.validate_store_inventory(limits, 0, false, || false),
            Err(LocalPublicationError::Capacity { .. })
        ));
        limits.physical_store_entries_max = 3;
        limits.directory_fanout_entries_max = 1;
        assert!(matches!(
            populated_root.validate_store_inventory(limits, 0, false, || false),
            Err(LocalPublicationError::Capacity { .. })
        ));
    }

    #[test]
    fn read_inventory_does_not_follow_writer_private_staging() {
        let directory = TestDirectory::new("read-inventory-staging");
        let staging = directory.path().join("staging/live-transaction");
        fs::create_dir_all(&staging).unwrap();
        let outside = directory.path().join("outside");
        fs::write(&outside, b"outside sentinel").unwrap();
        symlink(&outside, staging.join("volatile-link")).unwrap();
        let root = LocalStoreRoot::open(directory.path()).unwrap();

        root.validate_read_inventory(ProjectStoreLimits::default(), || false)
            .unwrap();
        assert!(matches!(
            root.validate_store_inventory(ProjectStoreLimits::default(), 0, false, || false),
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

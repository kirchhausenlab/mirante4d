//! Linux-only create-once publication for a fully validated local package.
//!
//! The destination parent is opened once. All staging writes and the final
//! `RENAME_NOREPLACE` operation are relative to held directory descriptors.
//! Cleanup is best-effort through the held staging descriptor. The contract
//! excludes a hostile actor able to mutate the destination-parent namespace:
//! Unix directory rename and unlink operations cannot atomically bind a source
//! name to an already-open directory descriptor.

#![cfg(target_os = "linux")]
#![cfg_attr(not(test), allow(dead_code))]

use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::File,
    io,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use mirante4d_identity::PackageId;
use rustix::{
    fd::OwnedFd,
    fs::{
        AtFlags, CWD, Dir, FileType, Mode, OFlags, RenameFlags, fstat, fsync, mkdirat, openat,
        renameat_with, statat, syncfs, unlinkat,
    },
    io::Errno,
    system::uname,
};
use thiserror::Error;

use crate::package_science::PreparedScientificPublication;
use crate::range_io::PublishedPackageRootBinding;
use crate::{PackagePath, SelfConsistentPackageCapability};

const STAGE_CREATE_ATTEMPTS: u64 = 128;
const SYNCFS_WRITEBACK_ERROR_KERNEL_MINIMUM: (u64, u64) = (5, 8);
static STAGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const DIRECTORY_OPEN_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW);
const FILE_CREATE_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW);

/// One private sibling stage that can be published exactly once.
///
/// `commit` consumes a validated `PackageId` to make the validation boundary
/// explicit. This filesystem primitive does not serialize that identity.
pub(crate) struct LocalPublication {
    parent: OwnedFd,
    destination_path: PathBuf,
    destination_name: OsString,
    stage_path: PathBuf,
    stage_name: OsString,
    stage: OwnedFd,
    stage_identity: DirectoryIdentity,
    created_directories: BTreeSet<PathBuf>,
    owns_stage: bool,
    cleanup_on_drop: bool,
}

#[derive(Debug, Error)]
pub(crate) enum LocalPublicationError {
    #[error("local publication was cancelled before commit")]
    Cancelled,
    #[error("publication destination already exists")]
    DestinationExists,
    #[error("the local filesystem cannot atomically publish without replacement: {source}")]
    AtomicPublishUnsupported {
        #[source]
        source: io::Error,
    },
    #[error("filesystem-wide package durability requires Linux kernel 5.8 or newer")]
    FilesystemDurabilityUnsupported,
    #[error(
        "the package was renamed into place, but publication durability is indeterminate: {source}"
    )]
    CommitIndeterminate {
        #[source]
        source: io::Error,
    },
    #[error("local publication failed while attempting to {operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublicationCheckpoint {
    BeforeRename,
    AfterRenameBeforeParentSync,
}

/// Successful durability operations performed by one publication phase.
/// Nanosecond accounting saturates only at the `u64` evidence ceiling; the
/// filesystem operation itself is never made fallible by diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PublicationSyncReport {
    pub(crate) calls: u64,
    pub(crate) time_ns: u64,
}

impl PublicationSyncReport {
    fn record(&mut self, elapsed: Duration) {
        self.calls = self.calls.saturating_add(1);
        self.time_ns = self
            .time_ns
            .saturating_add(u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX));
    }
}

impl LocalPublication {
    pub(crate) fn begin(destination: impl AsRef<Path>) -> Result<Self, LocalPublicationError> {
        let destination = destination.as_ref().to_path_buf();
        let destination_name = destination
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| invalid_input("publication destination must name one package root"))?
            .to_os_string();
        let parent_path = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = openat(CWD, parent_path, DIRECTORY_OPEN_FLAGS, Mode::empty())
            .map_err(|error| io_error("open the destination parent", error))?;
        match statat(
            &parent,
            destination_name.as_os_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(_) => return Err(LocalPublicationError::DestinationExists),
            Err(Errno::NOENT) => {}
            Err(error) => return Err(io_error("inspect the publication destination", error)),
        }
        require_syncfs_writeback_error_reporting()?;

        for attempt in 0..STAGE_CREATE_ATTEMPTS {
            let stage_name = unique_stage_name(attempt);
            match mkdirat(&parent, stage_name.as_os_str(), Mode::RWXU) {
                Ok(()) => {
                    let stage_path = parent_path.join(&stage_name);
                    let stage = match openat(
                        &parent,
                        stage_name.as_os_str(),
                        DIRECTORY_OPEN_FLAGS,
                        Mode::empty(),
                    ) {
                        Ok(stage) => stage,
                        Err(error) => {
                            let _ = unlinkat(&parent, stage_name.as_os_str(), AtFlags::REMOVEDIR);
                            return Err(io_error("open the new staging directory", error));
                        }
                    };
                    let stage_identity = match fstat(&stage) {
                        Ok(stat) => DirectoryIdentity::from_stat(stat),
                        Err(error) => {
                            drop(stage);
                            let _ = unlinkat(&parent, stage_name.as_os_str(), AtFlags::REMOVEDIR);
                            return Err(io_error("identify the new staging directory", error));
                        }
                    };
                    return Ok(Self {
                        parent,
                        destination_path: destination,
                        destination_name,
                        stage_path,
                        stage_name,
                        stage,
                        stage_identity,
                        created_directories: BTreeSet::from([PathBuf::new()]),
                        owns_stage: true,
                        cleanup_on_drop: true,
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

    /// Opens or creates one caller-named private stage next to the final
    /// destination. Unlike `begin`, dropping this handle preserves the stage:
    /// it is the durable checkpoint authority of a resumable producer.
    pub(crate) fn open_or_create_persistent(
        destination: impl AsRef<Path>,
        stage_path: impl AsRef<Path>,
    ) -> Result<Self, LocalPublicationError> {
        let destination = destination.as_ref().to_path_buf();
        let stage_path = stage_path.as_ref().to_path_buf();
        let destination_name = destination
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| invalid_input("publication destination must name one package root"))?
            .to_os_string();
        let stage_name = stage_path
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| invalid_input("publication stage must name one private root"))?
            .to_os_string();
        let parent_path = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let stage_parent = stage_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if parent_path != stage_parent || destination_name == stage_name {
            return Err(invalid_input(
                "persistent publication stage and destination must be distinct siblings",
            ));
        }
        let parent = openat(CWD, parent_path, DIRECTORY_OPEN_FLAGS, Mode::empty())
            .map_err(|error| io_error("open the destination parent", error))?;
        match statat(
            &parent,
            destination_name.as_os_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(_) => return Err(LocalPublicationError::DestinationExists),
            Err(Errno::NOENT) => {}
            Err(error) => return Err(io_error("inspect the publication destination", error)),
        }
        require_syncfs_writeback_error_reporting()?;

        match mkdirat(&parent, stage_name.as_os_str(), Mode::RWXU) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(error) => return Err(io_error("create the persistent staging directory", error)),
        }
        let stage = openat(
            &parent,
            stage_name.as_os_str(),
            DIRECTORY_OPEN_FLAGS,
            Mode::empty(),
        )
        .map_err(|error| io_error("open the persistent staging directory", error))?;
        let stage_identity = DirectoryIdentity::from_stat(
            fstat(&stage).map_err(|error| io_error("identify the staging directory", error))?,
        );
        let created_directories = collect_directories(&stage)?;
        Ok(Self {
            parent,
            destination_path: destination,
            destination_name,
            stage_path,
            stage_name,
            stage,
            stage_identity,
            created_directories,
            owns_stage: true,
            cleanup_on_drop: false,
        })
    }

    pub(crate) fn stage_path(&self) -> &Path {
        &self.stage_path
    }

    pub(crate) fn refresh_created_directories(&mut self) -> Result<(), LocalPublicationError> {
        self.created_directories = collect_directories(&self.stage)?;
        Ok(())
    }

    /// Creates one new regular object under the private stage.
    ///
    /// Parent directories are created by this instance with mode `0700` and
    /// traversed only through held descriptors with `O_NOFOLLOW`. Existing or
    /// repeated files are rejected by `O_EXCL`.
    pub(crate) fn create_file(
        &mut self,
        path: &PackagePath,
    ) -> Result<File, LocalPublicationError> {
        let mut components = package_components(path)?;
        let file_name = components
            .pop()
            .ok_or_else(|| invalid_input("package object path has no file name"))?;
        let mut current = openat(
            &self.stage,
            Path::new("."),
            DIRECTORY_OPEN_FLAGS,
            Mode::empty(),
        )
        .map_err(|error| io_error("duplicate the staging root descriptor", error))?;
        let mut relative = PathBuf::new();

        for component in components {
            relative.push(&component);
            if !self.created_directories.contains(&relative) {
                mkdirat(&current, component.as_os_str(), Mode::RWXU)
                    .map_err(|error| io_error("create a staging subdirectory", error))?;
                self.created_directories.insert(relative.clone());
            }
            current = openat(
                &current,
                component.as_os_str(),
                DIRECTORY_OPEN_FLAGS,
                Mode::empty(),
            )
            .map_err(|error| io_error("open a staging subdirectory", error))?;
        }

        let descriptor = openat(
            &current,
            file_name.as_os_str(),
            FILE_CREATE_FLAGS,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| io_error("create a staging object", error))?;
        Ok(File::from(descriptor))
    }

    /// Flushes staged object data once, then fsyncs every created directory.
    ///
    /// Linux `syncfs` has the completion guarantees of calling `fsync` on
    /// every file in the filesystem. The private stage has no recoverable or
    /// publishable intermediate state, so one barrier after the complete
    /// object set replaces physical-object-proportional file synchronization
    /// without weakening the atomic-publication boundary. Explicit
    /// deepest-first directory fsyncs retain the existing namespace durability
    /// proof.
    pub(crate) fn sync_stage(
        &self,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<PublicationSyncReport, LocalPublicationError> {
        self.sync_directories_with(self.created_directories.clone(), is_cancelled, |stage| {
            syncfs(stage)
        })
    }

    /// Flushes the bounded resumable suffix through one filesystem barrier.
    /// Linux `syncfs` covers both object data and namespace metadata; the
    /// final publication phase still performs its explicit full directory
    /// walk once. Incremental commits therefore do not accumulate an
    /// object- or directory-proportional durability cost.
    pub(crate) fn sync_stage_paths<'a>(
        &self,
        _paths: impl IntoIterator<Item = &'a PackagePath>,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<PublicationSyncReport, LocalPublicationError> {
        self.sync_directories_with(BTreeSet::new(), is_cancelled, |stage| syncfs(stage))
    }

    #[cfg(test)]
    fn sync_stage_with(
        &self,
        is_cancelled: impl FnMut() -> bool,
        sync_filesystem: impl FnOnce(&OwnedFd) -> rustix::io::Result<()>,
    ) -> Result<PublicationSyncReport, LocalPublicationError> {
        self.sync_directories_with(
            self.created_directories.clone(),
            is_cancelled,
            sync_filesystem,
        )
    }

    fn sync_directories_with(
        &self,
        directories: BTreeSet<PathBuf>,
        mut is_cancelled: impl FnMut() -> bool,
        sync_filesystem: impl FnOnce(&OwnedFd) -> rustix::io::Result<()>,
    ) -> Result<PublicationSyncReport, LocalPublicationError> {
        let mut report = PublicationSyncReport::default();
        if is_cancelled() {
            return Err(LocalPublicationError::Cancelled);
        }
        let started = Instant::now();
        sync_filesystem(&self.stage)
            .map_err(|error| io_error("synchronize the staged package filesystem", error))?;
        report.record(started.elapsed());
        if is_cancelled() {
            return Err(LocalPublicationError::Cancelled);
        }
        let mut directories = directories.into_iter().collect::<Vec<_>>();
        directories.sort_by(|left, right| {
            right
                .components()
                .count()
                .cmp(&left.components().count())
                .then_with(|| left.cmp(right))
        });
        for relative in directories {
            if is_cancelled() {
                return Err(LocalPublicationError::Cancelled);
            }
            let directory = self.open_created_directory(&relative)?;
            let started = Instant::now();
            fsync(&directory)
                .map_err(|error| io_error("synchronize a staging directory", error))?;
            report.record(started.elapsed());
        }
        if is_cancelled() {
            return Err(LocalPublicationError::Cancelled);
        }
        Ok(report)
    }

    pub(crate) fn commit(
        self,
        package_id: PackageId,
    ) -> Result<PublicationSyncReport, LocalPublicationError> {
        self.commit_with_hook(package_id, |_, _| Ok(()))
    }

    /// Atomically publishes the exact directory owned by a prepared scientific
    /// capability, rebinds that capability to the destination, and returns it
    /// only after the destination parent is durable.
    pub(crate) fn commit_self_consistent(
        mut self,
        package_id: PackageId,
        prepared: PreparedScientificPublication,
        is_cancelled: &mut impl FnMut() -> bool,
        mut hook: impl FnMut(PublicationCheckpoint, PackageId) -> io::Result<()>,
    ) -> Result<(PublicationSyncReport, SelfConsistentPackageCapability), LocalPublicationError>
    {
        if prepared.package_id() != package_id
            || !prepared.root_matches(self.stage_identity.device, self.stage_identity.inode)
        {
            return Err(invalid_input(
                "the scientific capability is not bound to this publication stage",
            ));
        }
        hook(PublicationCheckpoint::BeforeRename, package_id).map_err(|source| {
            LocalPublicationError::Io {
                operation: "complete the self-consistent pre-commit checkpoint",
                source,
            }
        })?;
        if !self
            .stage_name_still_owned()
            .map_err(|error| io_error("revalidate the owned staging directory", error))?
        {
            return Err(LocalPublicationError::Io {
                operation: "revalidate the owned staging directory",
                source: io::Error::new(
                    io::ErrorKind::NotFound,
                    "the staging name no longer identifies the directory created by this publication",
                ),
            });
        }
        // This is the final cancellable point. Once the atomic rename below
        // succeeds, publication is visible and must run through root rebinding
        // and parent durability rather than report a false prepublication
        // cancellation.
        if is_cancelled() {
            return Err(LocalPublicationError::Cancelled);
        }

        if let Err(error) = renameat_with(
            &self.parent,
            self.stage_name.as_os_str(),
            &self.parent,
            self.destination_name.as_os_str(),
            RenameFlags::NOREPLACE,
        ) {
            return Err(classify_rename_error(error));
        }
        self.owns_stage = false;
        hook(
            PublicationCheckpoint::AfterRenameBeforeParentSync,
            package_id,
        )
        .map_err(|source| LocalPublicationError::CommitIndeterminate { source })?;

        let binding = self
            .prove_published_root(&prepared)
            .map_err(|source| LocalPublicationError::CommitIndeterminate { source })?;
        let capability = prepared
            .rebind_after_publication(binding)
            .map_err(|error| LocalPublicationError::CommitIndeterminate {
                source: io::Error::other(error),
            })?;

        let started = Instant::now();
        fsync(&self.parent).map_err(|error| LocalPublicationError::CommitIndeterminate {
            source: io::Error::from(error),
        })?;
        let mut report = PublicationSyncReport::default();
        report.record(started.elapsed());
        Ok((report, capability))
    }

    fn commit_with_hook(
        mut self,
        package_id: PackageId,
        mut hook: impl FnMut(PublicationCheckpoint, PackageId) -> io::Result<()>,
    ) -> Result<PublicationSyncReport, LocalPublicationError> {
        hook(PublicationCheckpoint::BeforeRename, package_id).map_err(|source| {
            LocalPublicationError::Io {
                operation: "complete the pre-commit checkpoint",
                source,
            }
        })?;
        if !self
            .stage_name_still_owned()
            .map_err(|error| io_error("revalidate the owned staging directory", error))?
        {
            return Err(LocalPublicationError::Io {
                operation: "revalidate the owned staging directory",
                source: io::Error::new(
                    io::ErrorKind::NotFound,
                    "the staging name no longer identifies the directory created by this publication",
                ),
            });
        }

        if let Err(error) = renameat_with(
            &self.parent,
            self.stage_name.as_os_str(),
            &self.parent,
            self.destination_name.as_os_str(),
            RenameFlags::NOREPLACE,
        ) {
            return Err(classify_rename_error(error));
        }

        // The point of no return is the successful rename. Drop must never
        // remove the published package, even when the following durability
        // operation fails.
        self.owns_stage = false;
        hook(
            PublicationCheckpoint::AfterRenameBeforeParentSync,
            package_id,
        )
        .map_err(|source| LocalPublicationError::CommitIndeterminate { source })?;
        let started = Instant::now();
        fsync(&self.parent).map_err(|error| LocalPublicationError::CommitIndeterminate {
            source: io::Error::from(error),
        })?;
        let mut report = PublicationSyncReport::default();
        report.record(started.elapsed());
        Ok(report)
    }

    fn open_created_directory(&self, relative: &Path) -> Result<OwnedFd, LocalPublicationError> {
        let mut current = openat(
            &self.stage,
            Path::new("."),
            DIRECTORY_OPEN_FLAGS,
            Mode::empty(),
        )
        .map_err(|error| io_error("duplicate the staging root descriptor", error))?;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(invalid_input(
                    "tracked staging directory was not a relative normal path",
                ));
            };
            current = openat(&current, name, DIRECTORY_OPEN_FLAGS, Mode::empty())
                .map_err(|error| io_error("open a tracked staging directory", error))?;
        }
        Ok(current)
    }

    fn stage_name_still_owned(&self) -> Result<bool, Errno> {
        let current = statat(
            &self.parent,
            self.stage_name.as_os_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        )?;
        Ok(
            FileType::from_raw_mode(current.st_mode) == FileType::Directory
                && DirectoryIdentity::from_stat(current) == self.stage_identity,
        )
    }

    fn prove_published_root(
        &self,
        prepared: &PreparedScientificPublication,
    ) -> io::Result<PublishedPackageRootBinding> {
        let held = fstat(&self.stage).map_err(io::Error::from)?;
        let named = statat(
            &self.parent,
            self.destination_name.as_os_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(io::Error::from)?;
        let opened = openat(
            &self.parent,
            self.destination_name.as_os_str(),
            DIRECTORY_OPEN_FLAGS,
            Mode::empty(),
        )
        .map_err(io::Error::from)?;
        let opened = fstat(&opened).map_err(io::Error::from)?;
        let held_identity = DirectoryIdentity::from_stat(held);
        let named_identity = DirectoryIdentity::from_stat(named);
        let opened_identity = DirectoryIdentity::from_stat(opened);
        if FileType::from_raw_mode(named.st_mode) != FileType::Directory
            || FileType::from_raw_mode(opened.st_mode) != FileType::Directory
            || held_identity != self.stage_identity
            || named_identity != self.stage_identity
            || opened_identity != self.stage_identity
            || !prepared.root_matches(held_identity.device, held_identity.inode)
        {
            return Err(io::Error::other(
                "the published destination does not name the validated staging directory",
            ));
        }
        PublishedPackageRootBinding::open(&self.destination_path, prepared.root_seal())
            .map_err(io::Error::other)
    }
}

impl Drop for LocalPublication {
    fn drop(&mut self) {
        if !self.owns_stage || !self.cleanup_on_drop {
            return;
        }

        // Recursive cleanup works from the held descriptor. The final name
        // removal follows an identity check; the documented threat model does
        // not claim atomic protection from a hostile parent-namespace race.
        let _ = remove_directory_contents(&self.stage);
        if self.stage_name_still_owned().unwrap_or(false) {
            let _ = unlinkat(
                &self.parent,
                self.stage_name.as_os_str(),
                AtFlags::REMOVEDIR,
            );
        }
    }
}

fn collect_directories(root: &OwnedFd) -> Result<BTreeSet<PathBuf>, LocalPublicationError> {
    fn visit(
        directory: &OwnedFd,
        relative: &Path,
        found: &mut BTreeSet<PathBuf>,
    ) -> Result<(), LocalPublicationError> {
        found.insert(relative.to_path_buf());
        let entries = Dir::read_from(directory)
            .map_err(|error| io_error("enumerate the persistent staging directory", error))?
            .map(|entry| entry.map(|entry| entry.file_name().to_owned()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| io_error("read the persistent staging directory", error))?;
        for name in entries {
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            let metadata = statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|error| io_error("inspect a persistent staging entry", error))?;
            match FileType::from_raw_mode(metadata.st_mode) {
                FileType::Directory => {
                    let child = openat(directory, &name, DIRECTORY_OPEN_FLAGS, Mode::empty())
                        .map_err(|error| {
                            io_error("open a persistent staging subdirectory", error)
                        })?;
                    let child_relative =
                        relative.join(OsString::from_vec(name.to_bytes().to_vec()));
                    visit(&child, &child_relative, found)?;
                }
                FileType::RegularFile => {}
                _ => {
                    return Err(invalid_input(
                        "persistent publication stage contains a non-regular entry",
                    ));
                }
            }
        }
        Ok(())
    }

    use std::os::unix::ffi::OsStringExt;
    let mut found = BTreeSet::new();
    visit(root, Path::new(""), &mut found)?;
    Ok(found)
}

impl DirectoryIdentity {
    fn from_stat(stat: rustix::fs::Stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
        }
    }
}

fn package_components(path: &PackagePath) -> Result<Vec<OsString>, LocalPublicationError> {
    Path::new(path.as_str())
        .components()
        .map(|component| match component {
            Component::Normal(value) => Ok(value.to_os_string()),
            _ => Err(invalid_input(
                "package object path was not a relative normal path",
            )),
        })
        .collect()
}

fn remove_directory_contents(directory: &OwnedFd) -> Result<(), Errno> {
    let entries = Dir::read_from(directory)?
        .map(|entry| entry.map(|entry| entry.file_name().to_owned()))
        .collect::<Result<Vec<_>, _>>()?;
    for name in entries {
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        let metadata = statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW)?;
        if FileType::from_raw_mode(metadata.st_mode) == FileType::Directory {
            let child = openat(directory, &name, DIRECTORY_OPEN_FLAGS, Mode::empty())?;
            remove_directory_contents(&child)?;
            unlinkat(directory, &name, AtFlags::REMOVEDIR)?;
        } else {
            unlinkat(directory, &name, AtFlags::empty())?;
        }
    }
    Ok(())
}

fn unique_stage_name(attempt: u64) -> OsString {
    let sequence = STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        ".mirante4d-stage-{}-{timestamp:032x}-{sequence:016x}-{attempt:02x}",
        std::process::id()
    )
    .into()
}

fn classify_rename_error(error: Errno) -> LocalPublicationError {
    if error == Errno::EXIST {
        LocalPublicationError::DestinationExists
    } else if error == Errno::NOSYS || error == Errno::INVAL || error == Errno::OPNOTSUPP {
        LocalPublicationError::AtomicPublishUnsupported {
            source: io::Error::from(error),
        }
    } else {
        io_error("atomically publish the staged package", error)
    }
}

fn io_error(operation: &'static str, error: Errno) -> LocalPublicationError {
    LocalPublicationError::Io {
        operation,
        source: io::Error::from(error),
    }
}

fn require_syncfs_writeback_error_reporting() -> Result<(), LocalPublicationError> {
    let identity = uname();
    let release = identity
        .release()
        .to_str()
        .map_err(|source| LocalPublicationError::Io {
            operation: "verify Linux syncfs writeback-error support",
            source: io::Error::new(io::ErrorKind::InvalidData, source),
        })?;
    match kernel_release_supports_syncfs_writeback_errors(release) {
        Ok(true) => Ok(()),
        Ok(false) => Err(LocalPublicationError::FilesystemDurabilityUnsupported),
        Err(source) => Err(LocalPublicationError::Io {
            operation: "verify Linux syncfs writeback-error support",
            source,
        }),
    }
}

fn kernel_release_supports_syncfs_writeback_errors(release: &str) -> io::Result<bool> {
    let mut components = release.split('.');
    let major = parse_kernel_release_component(components.next(), "major")?;
    let minor = parse_kernel_release_component(components.next(), "minor")?;
    Ok((major, minor) >= SYNCFS_WRITEBACK_ERROR_KERNEL_MINIMUM)
}

fn parse_kernel_release_component(component: Option<&str>, label: &str) -> io::Result<u64> {
    let component = component
        .filter(|value| !value.is_empty() && value.as_bytes().iter().all(u8::is_ascii_digit));
    component
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Linux kernel release omitted a decimal {label} version"),
            )
        })
}

fn invalid_input(message: &'static str) -> LocalPublicationError {
    LocalPublicationError::Io {
        operation: "validate the publication path",
        source: io::Error::new(io::ErrorKind::InvalidInput, message),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Write,
        os::unix::fs::{PermissionsExt, symlink},
    };

    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "mirante4d-local-publication-{label}-{}-{}",
                std::process::id(),
                STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn package_id() -> PackageId {
        PackageId::from_manifest_root_bytes(b"publication-test")
    }

    #[test]
    fn creates_fd_relative_tree_syncs_and_publishes_once() {
        let root = TestDirectory::new("success");
        let destination = root.0.join("package.m4d");
        let mut publication = LocalPublication::begin(&destination).unwrap();
        let stage = publication.stage_path().to_path_buf();
        assert_eq!(
            fs::metadata(&stage).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let path = PackagePath::parse("images/0/payload.bin").unwrap();
        let mut file = publication.create_file(&path).unwrap();
        file.write_all(b"payload").unwrap();
        drop(file);
        let second_path = PackagePath::parse("images/0/second.bin").unwrap();
        let mut second = publication.create_file(&second_path).unwrap();
        second.write_all(b"second payload").unwrap();
        drop(second);
        let stage_syncs = publication.sync_stage(|| false).unwrap();
        assert_eq!(stage_syncs.calls, 4);
        let commit_syncs = publication.commit(package_id()).unwrap();
        assert_eq!(commit_syncs.calls, 1);

        assert!(!stage.exists());
        assert_eq!(
            fs::read(destination.join("images/0/payload.bin")).unwrap(),
            b"payload"
        );
    }

    #[test]
    fn destination_symlink_is_a_collision_and_owned_stage_is_removed() {
        let root = TestDirectory::new("collision");
        let destination = root.0.join("package.m4d");
        let target = root.0.join("source-data");
        fs::write(&target, b"source").unwrap();
        let publication = LocalPublication::begin(&destination).unwrap();
        let stage = publication.stage_path().to_path_buf();
        symlink(&target, &destination).unwrap();
        let error = publication.commit(package_id()).unwrap_err();

        assert!(matches!(error, LocalPublicationError::DestinationExists));
        assert!(!stage.exists());
        assert_eq!(fs::read(target).unwrap(), b"source");
        assert!(
            fs::symlink_metadata(destination)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn precommit_failure_removes_stage_without_publishing() {
        let root = TestDirectory::new("precommit");
        let destination = root.0.join("package.m4d");
        let publication = LocalPublication::begin(&destination).unwrap();
        let stage = publication.stage_path().to_path_buf();

        let error = publication
            .commit_with_hook(package_id(), |checkpoint, _| match checkpoint {
                PublicationCheckpoint::BeforeRename => {
                    Err(io::Error::other("injected precommit failure"))
                }
                PublicationCheckpoint::AfterRenameBeforeParentSync => Ok(()),
            })
            .unwrap_err();

        assert!(matches!(error, LocalPublicationError::Io { .. }));
        assert!(!destination.exists());
        assert!(!stage.exists());
    }

    #[test]
    fn postrename_failure_reports_indeterminate_and_never_rolls_back() {
        let root = TestDirectory::new("indeterminate");
        let destination = root.0.join("package.m4d");
        let mut publication = LocalPublication::begin(&destination).unwrap();
        let path = PackagePath::parse("payload.bin").unwrap();
        let mut file = publication.create_file(&path).unwrap();
        file.write_all(b"complete").unwrap();
        drop(file);
        let stage_syncs = publication.sync_stage(|| false).unwrap();
        assert_eq!(stage_syncs.calls, 2);

        let error = publication
            .commit_with_hook(package_id(), |checkpoint, _| match checkpoint {
                PublicationCheckpoint::BeforeRename => Ok(()),
                PublicationCheckpoint::AfterRenameBeforeParentSync => {
                    Err(io::Error::other("injected parent sync failure"))
                }
            })
            .unwrap_err();

        assert!(matches!(
            error,
            LocalPublicationError::CommitIndeterminate { .. }
        ));
        assert_eq!(
            fs::read(destination.join("payload.bin")).unwrap(),
            b"complete"
        );
    }

    #[test]
    fn stage_sync_failure_and_cancellation_never_publish() {
        let root = TestDirectory::new("stage-sync-failure");
        let destination = root.0.join("package.m4d");
        let mut publication = LocalPublication::begin(&destination).unwrap();
        let stage = publication.stage_path().to_path_buf();
        let path = PackagePath::parse("payload.bin").unwrap();
        let mut file = publication.create_file(&path).unwrap();
        file.write_all(b"complete").unwrap();
        drop(file);

        let error = publication
            .sync_stage_with(|| false, |_| Err(Errno::IO))
            .unwrap_err();
        assert!(matches!(error, LocalPublicationError::Io { .. }));
        assert!(!destination.exists());
        drop(publication);
        assert!(!stage.exists());

        for (label, cancel_after_barrier) in [("before", false), ("after", true)] {
            let destination = root.0.join(format!("cancel-{label}.m4d"));
            let mut publication = LocalPublication::begin(&destination).unwrap();
            let stage = publication.stage_path().to_path_buf();
            let path = PackagePath::parse("payload.bin").unwrap();
            let mut file = publication.create_file(&path).unwrap();
            file.write_all(b"complete").unwrap();
            drop(file);
            let checks = std::cell::Cell::new(0_u64);
            let barrier_called = std::cell::Cell::new(false);
            let error = publication
                .sync_stage_with(
                    || {
                        let next = checks.get() + 1;
                        checks.set(next);
                        if cancel_after_barrier {
                            next == 2
                        } else {
                            next == 1
                        }
                    },
                    |_| {
                        barrier_called.set(true);
                        Ok(())
                    },
                )
                .unwrap_err();
            assert!(matches!(error, LocalPublicationError::Cancelled));
            assert_eq!(checks.get(), if cancel_after_barrier { 2 } else { 1 });
            assert_eq!(barrier_called.get(), cancel_after_barrier);
            assert!(!destination.exists());
            drop(publication);
            assert!(!stage.exists());
        }
    }

    #[test]
    fn cleanup_does_not_remove_a_replacement_at_the_stage_name() {
        let root = TestDirectory::new("replacement");
        let destination = root.0.join("package.m4d");
        let mut publication = LocalPublication::begin(destination).unwrap();
        let stage = publication.stage_path().to_path_buf();
        let moved = root.0.join("moved-owned-stage");
        let path = PackagePath::parse("owned.bin").unwrap();
        publication.create_file(&path).unwrap();

        fs::rename(&stage, &moved).unwrap();
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("replacement-marker"), b"keep").unwrap();
        drop(publication);

        assert_eq!(fs::read(stage.join("replacement-marker")).unwrap(), b"keep");
        assert!(moved.is_dir());
        assert!(!moved.join("owned.bin").exists());
    }

    #[test]
    fn rename_capability_errors_are_typed_without_fallback() {
        assert!(matches!(
            classify_rename_error(Errno::NOSYS),
            LocalPublicationError::AtomicPublishUnsupported { .. }
        ));
        assert!(matches!(
            classify_rename_error(Errno::INVAL),
            LocalPublicationError::AtomicPublishUnsupported { .. }
        ));
        assert!(matches!(
            classify_rename_error(Errno::EXIST),
            LocalPublicationError::DestinationExists
        ));
    }

    #[test]
    fn syncfs_batching_requires_reliable_linux_writeback_errors() {
        for release in ["5.8.0", "5.15.0-1", "6.17.0-35-generic", "10.0.0"] {
            assert!(kernel_release_supports_syncfs_writeback_errors(release).unwrap());
        }
        for release in ["4.19.0", "5.7.19"] {
            assert!(!kernel_release_supports_syncfs_writeback_errors(release).unwrap());
        }
        for release in ["", "6", "6.x.0", ".8.0"] {
            assert!(kernel_release_supports_syncfs_writeback_errors(release).is_err());
        }
    }
}

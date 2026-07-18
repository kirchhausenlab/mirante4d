use std::{
    collections::VecDeque,
    fs::{self, File, Metadata},
    io::{self, Read, Seek, SeekFrom},
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, TryLockError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use rustix::{
    fs::{Mode, OFlags, ResolveFlags, open as open_fd, openat2},
    io::Errno,
};
#[cfg(target_os = "linux")]
use std::os::unix::fs::{FileExt, MetadataExt};
use thiserror::Error;

use mirante4d_dataset::ReservedDecodeSink;
use mirante4d_identity::{ExactBytesFacts, ExactBytesHasher, IdentityHashError};

use crate::shard::{DirectInnerDecodeError, decode_inner_payload_direct};
use crate::{
    GLOBAL_ENCODED_OUTER_SHARD_BYTES_MAX, PackagePath, ShardCodecError, ShardIndex,
    ShardProfileKind, decode_inner_payload, decode_shard_index_tail,
};

pub const SHARD_INDEX_RANGE_READ_BYTES_MAX: u64 = 4_096;
/// Maximum number of strict object handles retained by one package reader.
///
/// A source generation owns one reader. Eviction is deterministic LRU and an
/// in-flight operation may retain at most one additional `Arc` per worker.
pub const LOCAL_OBJECT_HANDLE_CACHE_MAX: usize = 64;
/// Conservative ownership charge for the one retained package-root
/// descriptor used by secure relative resolution.
pub const LOCAL_PACKAGE_ROOT_HANDLE_ACCOUNTED_BYTES: u64 = 4 * 1_024;
/// Conservative byte charge per retained handle, decoded shard index, and
/// one decoded packed-index inner chunk.
///
/// This covers the maximum portable relative path, the largest admitted
/// decoded fixed index, and Rust/file-handle bookkeeping. Product dataset
/// sources reserve the complete cache ceiling from `MetadataAndIndexes`.
pub const LOCAL_OBJECT_CACHE_ENTRY_BYTES_MAX: u64 = 32 * 1_024;
pub const LOCAL_OBJECT_CACHE_ACCOUNTED_BYTES_MAX: u64 = LOCAL_OBJECT_HANDLE_CACHE_MAX as u64
    * LOCAL_OBJECT_CACHE_ENTRY_BYTES_MAX
    + LOCAL_PACKAGE_ROOT_HANDLE_ACCOUNTED_BYTES;
pub(crate) const FULL_OBJECT_HASH_BUFFER_BYTES: usize = 64 * 1_024;

/// Low-cost cumulative facts for the normal strict-reader hot path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalPackageReadDiagnostics {
    pub object_open_operations: u64,
    pub object_open_time_ns: u64,
    /// All descriptors currently owned by this reader, including its retained
    /// package-root descriptor, cached objects, and short-lived currentness
    /// descriptors.
    pub open_object_handles_current: u64,
    /// Exact high-water mark of `open_object_handles_current`.
    pub open_object_handles_peak: u64,
    /// Object descriptors currently retained by the bounded LRU. This is a
    /// gauge, not an operation count, and excludes the separately accounted
    /// package-root descriptor and short-lived currentness descriptors.
    pub object_handle_cache_entries: u64,
    /// High-water mark of `object_handle_cache_entries` for this reader.
    pub object_handle_cache_peak_entries: u64,
    pub object_handle_cache_hits: u64,
    pub object_handle_cache_misses: u64,
    pub object_handle_cache_evictions: u64,
    pub object_handle_cache_lock_acquisitions: u64,
    pub object_handle_cache_lock_contentions: u64,
    pub object_handle_cache_lock_wait_time_ns: u64,
    pub shard_index_cache_hits: u64,
    pub shard_index_cache_misses: u64,
    pub shard_index_decode_operations: u64,
    pub packed_inner_cache_hits: u64,
    pub packed_inner_cache_misses: u64,
    /// Batched pre-use currentness transactions for normal cached shard reads.
    pub currentness_pre_use_batches: u64,
    /// Batched post-use currentness commits for successful cached shard reads.
    pub currentness_post_use_batches: u64,
    /// One-pass batches used to revalidate already-decoded snapshots.
    pub currentness_snapshot_batches: u64,
    /// Package-root metadata snapshots taken by batched currentness work.
    pub currentness_root_metadata_checks: u64,
    /// Root-confined, no-symlink named-object resolutions performed by
    /// `openat2` during currentness work.
    pub currentness_named_object_resolutions: u64,
    /// Metadata snapshots of the descriptor returned by each secure named
    /// resolution.
    pub currentness_object_fd_metadata_checks: u64,
    /// Wall time spent in root and named-object currentness syscalls.
    pub currentness_time_ns: u64,
    pub physical_range_read_operations: u64,
    pub physical_encoded_bytes_read: u64,
    pub physical_range_read_time_ns: u64,
    pub codec_decode_operations: u64,
    pub codec_decoded_bytes: u64,
    pub codec_decode_time_ns: u64,
}

/// Metadata for one checked regular object in a local target package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalObjectInfo {
    bytes: u64,
}

#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct LocalShardChunkBytes {
    pub(crate) encoded: Option<Vec<u8>>,
    pub(crate) decoded: Option<Arc<[u8]>>,
    pub(crate) range_requests: u8,
    pub(crate) encoded_bytes_read: u64,
    pub(crate) decoded_index_bytes: u64,
    pub(crate) snapshot: LocalObjectSnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct LocalObjectSnapshot {
    path: PackagePath,
    bytes: u64,
    identity: FileIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LocalObjectGeneration {
    bytes: u64,
    identity: FileIdentity,
}

/// Opaque identity of the package-root directory that owns a validated local
/// package capability.
///
/// Directory rename preserves this node identity. Publication uses it to prove
/// that the create-only destination names the same directory that was
/// validated privately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LocalPackageRootSeal {
    device: u64,
    inode: u64,
}

/// Fully checked destination binding produced only after the publication
/// namespace names the sealed package-root directory.
#[derive(Debug)]
pub(crate) struct PublishedPackageRootBinding {
    root: PathBuf,
    root_identity: FileIdentity,
    seal: LocalPackageRootSeal,
}

impl LocalObjectSnapshot {
    #[cfg(target_os = "linux")]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_metadata(
        path: PackagePath,
        metadata: &Metadata,
    ) -> Result<Self, RangeReadError> {
        if !metadata.is_file() {
            return Err(RangeReadError::NonRegularObject {
                path: path.to_string(),
            });
        }
        if metadata.nlink() != 1 {
            return Err(RangeReadError::Hardlink {
                path: path.to_string(),
                links: metadata.nlink(),
            });
        }
        Ok(Self {
            path,
            bytes: metadata.len(),
            identity: FileIdentity::from_metadata(metadata),
        })
    }

    pub(crate) const fn path(&self) -> &PackagePath {
        &self.path
    }

    pub(crate) const fn generation(&self) -> LocalObjectGeneration {
        LocalObjectGeneration {
            bytes: self.bytes,
            identity: self.identity,
        }
    }
}

#[derive(Debug)]
pub(crate) struct LocalObjectHash {
    pub(crate) facts: ExactBytesFacts,
    pub(crate) snapshot: LocalObjectSnapshot,
}

#[derive(Debug)]
pub(crate) enum LocalObjectHashError {
    Range(RangeReadError),
    Identity(IdentityHashError),
    Cancelled,
    DeclaredLengthMismatch { expected: u64, actual: u64 },
}

#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum LocalShardChunkReadError {
    Range(RangeReadError),
    Shard(ShardCodecError),
    DeclaredLengthMismatch { expected: u64, actual: u64 },
}

impl LocalObjectInfo {
    pub const fn bytes(self) -> u64 {
        self.bytes
    }
}

/// Read-only, root-confined access to an immutable local package.
///
/// WP-10A currently claims Linux local storage only. Every object open
/// rejects symlinks, hardlinks, non-regular files, root escape, and identity
/// changes around the open before any bytes are returned. This is unverified
/// raw access: it does not authenticate a manifest digest or authorize a
/// declared package identity.
#[derive(Debug)]
pub struct LocalPackageReader {
    root: PathBuf,
    /// Stable descriptor for the exact root node admitted at open. Every
    /// package-object lookup is relative to this descriptor and uses
    /// `openat2` with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`.
    root_directory: File,
    root_identity: FileIdentity,
    object_handle_gauge: Arc<ObjectHandleGauge>,
    /// Successful OS opens of package objects through this reader. This is an
    /// operation count rather than a distinct-path count: whole-object reads,
    /// range reads, hashes, and snapshot-only revalidations each open and
    /// count the object they access.
    object_open_operations: AtomicU64,
    object_open_time_ns: AtomicU64,
    object_handle_cache: Mutex<VecDeque<Arc<CachedObject>>>,
    object_handle_cache_hits: AtomicU64,
    object_handle_cache_misses: AtomicU64,
    object_handle_cache_evictions: AtomicU64,
    object_handle_cache_peak_entries: AtomicU64,
    object_handle_cache_lock_acquisitions: AtomicU64,
    object_handle_cache_lock_contentions: AtomicU64,
    object_handle_cache_lock_wait_time_ns: AtomicU64,
    shard_index_cache_hits: AtomicU64,
    shard_index_cache_misses: AtomicU64,
    shard_index_decode_operations: AtomicU64,
    packed_inner_cache_hits: AtomicU64,
    packed_inner_cache_misses: AtomicU64,
    currentness_pre_use_batches: AtomicU64,
    currentness_post_use_batches: AtomicU64,
    currentness_snapshot_batches: AtomicU64,
    currentness_root_metadata_checks: AtomicU64,
    currentness_named_object_resolutions: AtomicU64,
    currentness_object_fd_metadata_checks: AtomicU64,
    currentness_time_ns: AtomicU64,
    physical_range_read_operations: AtomicU64,
    physical_encoded_bytes_read: AtomicU64,
    physical_range_read_time_ns: AtomicU64,
    codec_decode_operations: AtomicU64,
    codec_decoded_bytes: AtomicU64,
    codec_decode_time_ns: AtomicU64,
}

#[derive(Clone, Copy)]
enum CurrentnessBatchKind {
    Read,
    Snapshot,
}

#[derive(Clone, Copy, Default)]
struct CurrentnessBatchMetrics {
    pre_use_batches: u64,
    post_use_batches: u64,
    snapshot_batches: u64,
    root_metadata_checks: u64,
    named_object_resolutions: u64,
    object_fd_metadata_checks: u64,
    time_ns: u64,
}

/// One lock-free, thread-local currentness batch over cached object handles.
///
/// Its only shared synchronization is the short handle-cache lookup. All path
/// and descriptor checks, per-object deduplication, and counter accumulation
/// are local to this value.
pub(crate) struct LocalCurrentnessBatch<'a> {
    reader: &'a LocalPackageReader,
    objects: Vec<Arc<CachedObject>>,
    metrics: CurrentnessBatchMetrics,
}

impl LocalPackageReader {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, RangeReadError> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = root;
            return Err(RangeReadError::UnsupportedPlatform);
        }

        #[cfg(target_os = "linux")]
        {
            let root = root.as_ref();
            let metadata = symlink_metadata(root, "inspect package root", "<root>")?;
            if metadata.file_type().is_symlink() {
                return Err(RangeReadError::Symlink {
                    path: "<root>".to_owned(),
                });
            }
            if !metadata.is_dir() {
                return Err(RangeReadError::RootNotDirectory);
            }
            let canonical = fs::canonicalize(root)
                .map_err(|error| io_error("canonicalize package root", "<root>", error))?;
            let canonical_metadata =
                symlink_metadata(&canonical, "reinspect package root", "<root>")?;
            if canonical_metadata.file_type().is_symlink() || !canonical_metadata.is_dir() {
                return Err(RangeReadError::RootNotDirectory);
            }
            let root_directory = File::from(
                open_fd(
                    &canonical,
                    OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|error| {
                    io_error(
                        "retain package root descriptor",
                        "<root>",
                        io::Error::from(error),
                    )
                })?,
            );
            let descriptor_metadata = root_directory.metadata().map_err(|error| {
                io_error("inspect retained package root descriptor", "<root>", error)
            })?;
            if !descriptor_metadata.is_dir()
                || !FileIdentity::from_metadata(&descriptor_metadata)
                    .same_node(FileIdentity::from_metadata(&canonical_metadata))
            {
                return Err(RangeReadError::RootChanged);
            }
            let object_handle_gauge = Arc::new(ObjectHandleGauge::with_package_root());
            Ok(Self {
                root: canonical,
                root_directory,
                root_identity: FileIdentity::from_metadata(&canonical_metadata),
                object_handle_gauge,
                object_open_operations: AtomicU64::new(0),
                object_open_time_ns: AtomicU64::new(0),
                object_handle_cache: Mutex::new(VecDeque::with_capacity(
                    LOCAL_OBJECT_HANDLE_CACHE_MAX,
                )),
                object_handle_cache_hits: AtomicU64::new(0),
                object_handle_cache_misses: AtomicU64::new(0),
                object_handle_cache_evictions: AtomicU64::new(0),
                object_handle_cache_peak_entries: AtomicU64::new(0),
                object_handle_cache_lock_acquisitions: AtomicU64::new(0),
                object_handle_cache_lock_contentions: AtomicU64::new(0),
                object_handle_cache_lock_wait_time_ns: AtomicU64::new(0),
                shard_index_cache_hits: AtomicU64::new(0),
                shard_index_cache_misses: AtomicU64::new(0),
                shard_index_decode_operations: AtomicU64::new(0),
                packed_inner_cache_hits: AtomicU64::new(0),
                packed_inner_cache_misses: AtomicU64::new(0),
                currentness_pre_use_batches: AtomicU64::new(0),
                currentness_post_use_batches: AtomicU64::new(0),
                currentness_snapshot_batches: AtomicU64::new(0),
                currentness_root_metadata_checks: AtomicU64::new(0),
                currentness_named_object_resolutions: AtomicU64::new(0),
                currentness_object_fd_metadata_checks: AtomicU64::new(0),
                currentness_time_ns: AtomicU64::new(0),
                physical_range_read_operations: AtomicU64::new(0),
                physical_encoded_bytes_read: AtomicU64::new(0),
                physical_range_read_time_ns: AtomicU64::new(0),
                codec_decode_operations: AtomicU64::new(0),
                codec_decoded_bytes: AtomicU64::new(0),
                codec_decode_time_ns: AtomicU64::new(0),
            })
        }
    }

    pub(crate) fn object_open_operations(&self) -> u64 {
        self.object_open_operations.load(Ordering::Relaxed)
    }

    pub fn read_diagnostics(&self) -> LocalPackageReadDiagnostics {
        // Observing diagnostics must not perturb the cache-lock counters it
        // reports.
        let object_handle_cache_entries =
            u64::try_from(lock_unpoisoned(&self.object_handle_cache).len()).unwrap_or(u64::MAX);
        LocalPackageReadDiagnostics {
            object_open_operations: self.object_open_operations.load(Ordering::Relaxed),
            object_open_time_ns: self.object_open_time_ns.load(Ordering::Relaxed),
            open_object_handles_current: self.object_handle_gauge.current(),
            open_object_handles_peak: self.object_handle_gauge.peak(),
            object_handle_cache_entries,
            object_handle_cache_peak_entries: self
                .object_handle_cache_peak_entries
                .load(Ordering::Relaxed),
            object_handle_cache_hits: self.object_handle_cache_hits.load(Ordering::Relaxed),
            object_handle_cache_misses: self.object_handle_cache_misses.load(Ordering::Relaxed),
            object_handle_cache_evictions: self
                .object_handle_cache_evictions
                .load(Ordering::Relaxed),
            object_handle_cache_lock_acquisitions: self
                .object_handle_cache_lock_acquisitions
                .load(Ordering::Relaxed),
            object_handle_cache_lock_contentions: self
                .object_handle_cache_lock_contentions
                .load(Ordering::Relaxed),
            object_handle_cache_lock_wait_time_ns: self
                .object_handle_cache_lock_wait_time_ns
                .load(Ordering::Relaxed),
            shard_index_cache_hits: self.shard_index_cache_hits.load(Ordering::Relaxed),
            shard_index_cache_misses: self.shard_index_cache_misses.load(Ordering::Relaxed),
            shard_index_decode_operations: self
                .shard_index_decode_operations
                .load(Ordering::Relaxed),
            packed_inner_cache_hits: self.packed_inner_cache_hits.load(Ordering::Relaxed),
            packed_inner_cache_misses: self.packed_inner_cache_misses.load(Ordering::Relaxed),
            currentness_pre_use_batches: self.currentness_pre_use_batches.load(Ordering::Relaxed),
            currentness_post_use_batches: self.currentness_post_use_batches.load(Ordering::Relaxed),
            currentness_snapshot_batches: self.currentness_snapshot_batches.load(Ordering::Relaxed),
            currentness_root_metadata_checks: self
                .currentness_root_metadata_checks
                .load(Ordering::Relaxed),
            currentness_named_object_resolutions: self
                .currentness_named_object_resolutions
                .load(Ordering::Relaxed),
            currentness_object_fd_metadata_checks: self
                .currentness_object_fd_metadata_checks
                .load(Ordering::Relaxed),
            currentness_time_ns: self.currentness_time_ns.load(Ordering::Relaxed),
            physical_range_read_operations: self
                .physical_range_read_operations
                .load(Ordering::Relaxed),
            physical_encoded_bytes_read: self.physical_encoded_bytes_read.load(Ordering::Relaxed),
            physical_range_read_time_ns: self.physical_range_read_time_ns.load(Ordering::Relaxed),
            codec_decode_operations: self.codec_decode_operations.load(Ordering::Relaxed),
            codec_decoded_bytes: self.codec_decoded_bytes.load(Ordering::Relaxed),
            codec_decode_time_ns: self.codec_decode_time_ns.load(Ordering::Relaxed),
        }
    }

    /// Seals the current reader root for a create-only atomic publication.
    ///
    /// The expected path must resolve to this reader's exact canonical root at
    /// the time of the call. The returned value contains no path and cannot be
    /// redirected to another directory.
    pub(crate) fn seal_root_for_publication(
        &self,
        expected_root: &Path,
    ) -> Result<LocalPackageRootSeal, RangeReadError> {
        self.validate_root_identity()?;
        let canonical = fs::canonicalize(expected_root)
            .map_err(|error| io_error("canonicalize publication stage", "<root>", error))?;
        if canonical != self.root {
            return Err(RangeReadError::RootChanged);
        }
        let metadata = symlink_metadata(&canonical, "reinspect publication stage", "<root>")?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(RangeReadError::RootNotDirectory);
        }
        let identity = FileIdentity::from_metadata(&metadata);
        if !identity.same_node(self.root_identity) {
            return Err(RangeReadError::RootChanged);
        }
        Ok(LocalPackageRootSeal::from_identity(identity))
    }

    /// Rebinds this reader from its private staging name to a checked published
    /// name for the exact same directory node.
    pub(crate) fn rebind_published_root(
        &mut self,
        binding: PublishedPackageRootBinding,
    ) -> Result<(), RangeReadError> {
        if !binding.seal.matches_identity(self.root_identity) {
            return Err(RangeReadError::RootChanged);
        }
        self.lock_object_cache().clear();
        self.root = binding.root;
        self.root_identity = binding.root_identity;
        self.validate_root_identity()
    }

    pub(crate) fn codec_decode_operations(&self) -> u64 {
        self.codec_decode_operations.load(Ordering::Relaxed)
    }

    pub(crate) fn codec_decode_time_ns(&self) -> u64 {
        self.codec_decode_time_ns.load(Ordering::Relaxed)
    }

    pub(crate) fn decode_shard_index_tail_accounted(
        &self,
        kind: ShardProfileKind,
        tail: &[u8],
        payload_bytes: u64,
    ) -> Result<ShardIndex, ShardCodecError> {
        let started = Instant::now();
        let result = decode_shard_index_tail(kind, tail, payload_bytes);
        self.record_codec_decode(started.elapsed());
        self.shard_index_decode_operations
            .fetch_add(1, Ordering::Relaxed);
        if result.is_ok() {
            self.record_decoded_bytes(
                u64::try_from(tail.len().saturating_sub(4)).unwrap_or(u64::MAX),
            );
        }
        result
    }

    pub(crate) fn decode_inner_payload_accounted(
        &self,
        kind: ShardProfileKind,
        encoded: &[u8],
    ) -> Result<Vec<u8>, ShardCodecError> {
        let started = Instant::now();
        let result = decode_inner_payload(kind, encoded);
        self.record_codec_decode(started.elapsed());
        if let Ok(decoded) = &result {
            self.record_decoded_bytes(u64::try_from(decoded.len()).unwrap_or(u64::MAX));
        }
        result
    }

    pub(crate) fn decode_inner_payload_direct_accounted<E>(
        &self,
        kind: ShardProfileKind,
        encoded: &[u8],
        sink: &mut dyn ReservedDecodeSink,
        observe: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), DirectInnerDecodeError<E>> {
        let started = Instant::now();
        let result = decode_inner_payload_direct(kind, encoded, sink, observe);
        self.record_codec_decode(started.elapsed());
        if result.is_ok() {
            self.record_decoded_bytes(
                u64::try_from(kind.decoded_inner_bytes()).unwrap_or(u64::MAX),
            );
        }
        result
    }

    fn record_codec_decode(&self, elapsed: Duration) {
        let elapsed_ns = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        let _ = self.codec_decode_operations.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |calls| Some(calls.saturating_add(1)),
        );
        let _ =
            self.codec_decode_time_ns
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |time| {
                    Some(time.saturating_add(elapsed_ns))
                });
    }

    fn record_decoded_bytes(&self, bytes: u64) {
        let _ =
            self.codec_decoded_bytes
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |total| {
                    Some(total.saturating_add(bytes))
                });
    }

    pub fn object_info(
        &self,
        path: &PackagePath,
        object_bytes_max: u64,
    ) -> Result<LocalObjectInfo, RangeReadError> {
        let checked = self.open_object(path, object_bytes_max)?;
        Ok(LocalObjectInfo {
            bytes: checked.bytes,
        })
    }

    pub(crate) fn read_object(
        &self,
        path: &PackagePath,
        object_bytes_max: u64,
    ) -> Result<Vec<u8>, RangeReadError> {
        self.read_object_with_snapshot(path, object_bytes_max)
            .map(|(bytes, _snapshot)| bytes)
    }

    pub(crate) fn read_object_with_snapshot(
        &self,
        path: &PackagePath,
        object_bytes_max: u64,
    ) -> Result<(Vec<u8>, LocalObjectSnapshot), RangeReadError> {
        let mut checked = self.open_object(path, object_bytes_max)?;
        let bytes = read_exact_at(&mut checked.file, path, 0, checked.bytes)?;
        self.revalidate_open_object(path, &checked)?;
        let snapshot = LocalObjectSnapshot {
            path: path.clone(),
            bytes: checked.bytes,
            identity: checked.identity,
        };
        Ok((bytes, snapshot))
    }

    /// Streams one complete object through the exact-byte hasher.
    ///
    /// The object is opened once, no payload is retained, cancellation is
    /// checked before every bounded read, and the same open file is
    /// revalidated before its snapshot is returned.
    pub(crate) fn hash_object_with_snapshot(
        &self,
        path: &PackagePath,
        declared_bytes: u64,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<LocalObjectHash, LocalObjectHashError> {
        if is_cancelled() {
            return Err(LocalObjectHashError::Cancelled);
        }
        let mut checked = self
            .open_object(path, GLOBAL_ENCODED_OUTER_SHARD_BYTES_MAX)
            .map_err(LocalObjectHashError::Range)?;
        if checked.bytes != declared_bytes {
            return Err(LocalObjectHashError::DeclaredLengthMismatch {
                expected: declared_bytes,
                actual: checked.bytes,
            });
        }

        let mut remaining = checked.bytes;
        let mut buffer = vec![0_u8; FULL_OBJECT_HASH_BUFFER_BYTES];
        let mut hasher = ExactBytesHasher::new();
        while remaining != 0 {
            if is_cancelled() {
                return Err(LocalObjectHashError::Cancelled);
            }
            let requested = usize::try_from(remaining)
                .unwrap_or(usize::MAX)
                .min(buffer.len());
            let read = checked
                .file
                .read(&mut buffer[..requested])
                .map_err(|error| {
                    LocalObjectHashError::Range(io_error(
                        "stream package object",
                        path.as_str(),
                        error,
                    ))
                })?;
            if read == 0 {
                return Err(LocalObjectHashError::Range(RangeReadError::ShortRead {
                    path: path.to_string(),
                    expected: remaining,
                }));
            }
            hasher
                .update(&buffer[..read])
                .map_err(LocalObjectHashError::Identity)?;
            remaining -= u64::try_from(read)
                .map_err(|_| LocalObjectHashError::Range(RangeReadError::LengthOverflow))?;
        }
        if is_cancelled() {
            return Err(LocalObjectHashError::Cancelled);
        }
        let facts = hasher.finalize().map_err(LocalObjectHashError::Identity)?;
        self.revalidate_open_object(path, &checked)
            .map_err(LocalObjectHashError::Range)?;
        Ok(LocalObjectHash {
            facts,
            snapshot: LocalObjectSnapshot {
                path: path.clone(),
                bytes: checked.bytes,
                identity: checked.identity,
            },
        })
    }

    /// Reads one nonempty checked `(object, offset, length)` range.
    pub fn read_range(
        &self,
        path: &PackagePath,
        offset: u64,
        length: u64,
        object_bytes_max: u64,
    ) -> Result<Vec<u8>, RangeReadError> {
        if length == 0 {
            return Err(RangeReadError::EmptyRange);
        }
        let end = offset
            .checked_add(length)
            .ok_or(RangeReadError::RangeOverflow)?;
        let mut checked = self.open_object(path, object_bytes_max)?;
        if end > checked.bytes {
            return Err(RangeReadError::RangeOutOfBounds {
                offset,
                length,
                object_bytes: checked.bytes,
            });
        }
        let bytes = read_exact_at(&mut checked.file, path, offset, length)?;
        self.revalidate_open_object(path, &checked)?;
        Ok(bytes)
    }

    /// Reads the exact fixed shard-index tail without reading a whole shard.
    pub fn read_shard_index_tail(
        &self,
        path: &PackagePath,
        tail_bytes: u64,
        object_bytes_max: u64,
    ) -> Result<(Vec<u8>, u64), RangeReadError> {
        self.read_shard_index_tail_with_snapshot(path, tail_bytes, object_bytes_max)
            .map(|(tail, payload_bytes, _snapshot)| (tail, payload_bytes))
    }

    pub(crate) fn read_shard_index_tail_with_snapshot(
        &self,
        path: &PackagePath,
        tail_bytes: u64,
        object_bytes_max: u64,
    ) -> Result<(Vec<u8>, u64, LocalObjectSnapshot), RangeReadError> {
        if tail_bytes == 0 || tail_bytes > SHARD_INDEX_RANGE_READ_BYTES_MAX {
            return Err(RangeReadError::InvalidShardIndexRange {
                actual: tail_bytes,
                maximum: SHARD_INDEX_RANGE_READ_BYTES_MAX,
            });
        }
        let mut checked = self.open_object(path, object_bytes_max)?;
        if tail_bytes > checked.bytes {
            return Err(RangeReadError::RangeOutOfBounds {
                offset: 0,
                length: tail_bytes,
                object_bytes: checked.bytes,
            });
        }
        let payload_bytes = checked.bytes - tail_bytes;
        let bytes = read_exact_at(&mut checked.file, path, payload_bytes, tail_bytes)?;
        self.revalidate_open_object(path, &checked)?;
        let snapshot = LocalObjectSnapshot {
            path: path.clone(),
            bytes: checked.bytes,
            identity: checked.identity,
        };
        Ok((bytes, payload_bytes, snapshot))
    }

    /// Begins one cached brick-read currentness transaction.
    ///
    /// The transaction validates the package root once, validates each unique
    /// parent path prefix once before first use, and validates each consumed
    /// object once before use. `finish_read` repeats that set as one post-use
    /// batch before any decoded bytes may escape the caller.
    pub(crate) fn begin_cached_read_transaction(
        &self,
    ) -> Result<LocalCurrentnessBatch<'_>, RangeReadError> {
        self.begin_cached_read_transaction_with_capacity(0)
    }

    pub(crate) fn begin_cached_read_transaction_with_capacity(
        &self,
        object_capacity: usize,
    ) -> Result<LocalCurrentnessBatch<'_>, RangeReadError> {
        LocalCurrentnessBatch::begin(self, CurrentnessBatchKind::Read, object_capacity)
    }

    /// Begins a one-pass batch for already-decoded object snapshots.
    pub(crate) fn begin_cached_snapshot_revalidation(
        &self,
    ) -> Result<LocalCurrentnessBatch<'_>, RangeReadError> {
        LocalCurrentnessBatch::begin(self, CurrentnessBatchKind::Snapshot, 0)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn read_shard_chunk(
        &self,
        path: &PackagePath,
        kind: ShardProfileKind,
        chunk_index: usize,
        declared_bytes: u64,
    ) -> Result<LocalShardChunkBytes, LocalShardChunkReadError> {
        let checked = self
            .open_cached_object(
                path,
                u64::try_from(kind.encoded_shard_bytes_max()).map_err(|_| {
                    LocalShardChunkReadError::Shard(ShardCodecError::LengthOverflow)
                })?,
            )
            .map_err(LocalShardChunkReadError::Range)?;
        let result =
            self.read_shard_chunk_from_cached(&checked, path, kind, chunk_index, declared_bytes);
        self.revalidate_cached_object(path, &checked)
            .map_err(LocalShardChunkReadError::Range)?;
        result
    }

    fn read_shard_chunk_from_cached(
        &self,
        checked: &CachedObject,
        path: &PackagePath,
        kind: ShardProfileKind,
        chunk_index: usize,
        declared_bytes: u64,
    ) -> Result<LocalShardChunkBytes, LocalShardChunkReadError> {
        (|| {
            if checked.bytes != declared_bytes {
                return Err(LocalShardChunkReadError::DeclaredLengthMismatch {
                    expected: declared_bytes,
                    actual: checked.bytes,
                });
            }
            let tail_bytes = u64::try_from(kind.index_tail_bytes())
                .map_err(|_| LocalShardChunkReadError::Shard(ShardCodecError::LengthOverflow))?;
            if tail_bytes > checked.bytes {
                return Err(LocalShardChunkReadError::Range(
                    RangeReadError::RangeOutOfBounds {
                        offset: 0,
                        length: tail_bytes,
                        object_bytes: checked.bytes,
                    },
                ));
            }
            let payload_bytes = checked.bytes - tail_bytes;
            let (index, _index_was_read) = self
                .cached_shard_index(checked, path, kind, payload_bytes, tail_bytes)
                .map_err(|error| match error {
                    CachedShardIndexError::Range(error) => LocalShardChunkReadError::Range(error),
                    CachedShardIndexError::Shard(error) => LocalShardChunkReadError::Shard(error),
                })?;
            let entry = index
                .entry(chunk_index)
                .map_err(LocalShardChunkReadError::Shard)?;
            let (encoded, decoded) = match entry {
                Some(entry) if kind == ShardProfileKind::PackedIndex => (
                    None,
                    Some(self.cached_packed_inner(
                        checked,
                        path,
                        chunk_index,
                        payload_bytes,
                        entry.offset(),
                        entry.nbytes(),
                    )?),
                ),
                Some(entry) => {
                    let bytes = self
                        .read_cached_range(&checked.file, path, entry.offset(), entry.nbytes())
                        .map_err(LocalShardChunkReadError::Range)?;
                    (Some(bytes), None)
                }
                None => (None, None),
            };
            let encoded_payload_bytes = entry.map_or(0, |entry| entry.nbytes());
            let encoded_bytes_read = tail_bytes.checked_add(encoded_payload_bytes).ok_or(
                LocalShardChunkReadError::Shard(ShardCodecError::LengthOverflow),
            )?;
            let decoded_index_bytes = u64::try_from(kind.index_tail_bytes() - 4)
                .map_err(|_| LocalShardChunkReadError::Shard(ShardCodecError::LengthOverflow))?;
            Ok(LocalShardChunkBytes {
                encoded,
                decoded,
                // LocalBrickRead retains the frozen cold-amplification facts;
                // `read_diagnostics` reports actual cache-adjusted operations.
                range_requests: if entry.is_some() { 2 } else { 1 },
                encoded_bytes_read,
                decoded_index_bytes,
                snapshot: LocalObjectSnapshot {
                    path: path.clone(),
                    bytes: checked.bytes,
                    identity: checked.identity,
                },
            })
        })()
    }

    /// Validation-only cold read preserving the evidence contract that every
    /// component opens and decodes its own index. Normal dataset delivery uses
    /// `read_shard_chunk` and its guarded reuse path.
    pub(crate) fn read_shard_chunk_uncached(
        &self,
        path: &PackagePath,
        kind: ShardProfileKind,
        chunk_index: usize,
        declared_bytes: u64,
    ) -> Result<LocalShardChunkBytes, LocalShardChunkReadError> {
        let mut checked = self
            .open_object(
                path,
                u64::try_from(kind.encoded_shard_bytes_max()).map_err(|_| {
                    LocalShardChunkReadError::Shard(ShardCodecError::LengthOverflow)
                })?,
            )
            .map_err(LocalShardChunkReadError::Range)?;
        let result = (|| {
            if checked.bytes != declared_bytes {
                return Err(LocalShardChunkReadError::DeclaredLengthMismatch {
                    expected: declared_bytes,
                    actual: checked.bytes,
                });
            }
            let tail_bytes = u64::try_from(kind.index_tail_bytes())
                .map_err(|_| LocalShardChunkReadError::Shard(ShardCodecError::LengthOverflow))?;
            if tail_bytes > checked.bytes {
                return Err(LocalShardChunkReadError::Range(
                    RangeReadError::RangeOutOfBounds {
                        offset: 0,
                        length: tail_bytes,
                        object_bytes: checked.bytes,
                    },
                ));
            }
            let payload_bytes = checked.bytes - tail_bytes;
            let tail = read_exact_at(&mut checked.file, path, payload_bytes, tail_bytes)
                .map_err(LocalShardChunkReadError::Range)?;
            let index = self
                .decode_shard_index_tail_accounted(kind, &tail, payload_bytes)
                .map_err(LocalShardChunkReadError::Shard)?;
            let entry = index
                .entry(chunk_index)
                .map_err(LocalShardChunkReadError::Shard)?;
            let encoded = entry
                .map(|entry| {
                    read_exact_at(&mut checked.file, path, entry.offset(), entry.nbytes())
                        .map_err(LocalShardChunkReadError::Range)
                })
                .transpose()?;
            let encoded_payload_bytes = entry.map_or(0, |entry| entry.nbytes());
            let encoded_bytes_read = tail_bytes.checked_add(encoded_payload_bytes).ok_or(
                LocalShardChunkReadError::Shard(ShardCodecError::LengthOverflow),
            )?;
            let decoded_index_bytes = u64::try_from(kind.index_tail_bytes() - 4)
                .map_err(|_| LocalShardChunkReadError::Shard(ShardCodecError::LengthOverflow))?;
            Ok(LocalShardChunkBytes {
                encoded,
                decoded: None,
                range_requests: if entry.is_some() { 2 } else { 1 },
                encoded_bytes_read,
                decoded_index_bytes,
                snapshot: LocalObjectSnapshot {
                    path: path.clone(),
                    bytes: checked.bytes,
                    identity: checked.identity,
                },
            })
        })();
        self.revalidate_open_object(path, &checked)
            .map_err(LocalShardChunkReadError::Range)?;
        result
    }

    fn cached_shard_index(
        &self,
        object: &CachedObject,
        path: &PackagePath,
        kind: ShardProfileKind,
        payload_bytes: u64,
        tail_bytes: u64,
    ) -> Result<(Arc<ShardIndex>, bool), CachedShardIndexError> {
        let mut cached = lock_unpoisoned(&object.shard_index);
        if let Some(entry) = cached.as_ref() {
            if entry.kind == kind && entry.payload_bytes == payload_bytes {
                self.shard_index_cache_hits.fetch_add(1, Ordering::Relaxed);
                return Ok((Arc::clone(&entry.index), false));
            }
            return Err(CachedShardIndexError::Range(
                RangeReadError::ObjectChanged {
                    path: path.to_string(),
                },
            ));
        }
        self.shard_index_cache_misses
            .fetch_add(1, Ordering::Relaxed);
        let tail = self
            .read_cached_range(&object.file, path, payload_bytes, tail_bytes)
            .map_err(CachedShardIndexError::Range)?;
        let index = Arc::new(
            self.decode_shard_index_tail_accounted(kind, &tail, payload_bytes)
                .map_err(CachedShardIndexError::Shard)?,
        );
        *cached = Some(CachedShardIndex {
            kind,
            payload_bytes,
            index: Arc::clone(&index),
        });
        Ok((index, true))
    }

    /// Reuses exactly one decoded packed-index inner per open object. One
    /// 16-KiB inner contains 256 adjacent 64-byte brick records, so this small
    /// representation-specific cache removes their repeated range read,
    /// decompression, allocation, and copy without retaining image payloads.
    fn cached_packed_inner(
        &self,
        object: &CachedObject,
        path: &PackagePath,
        chunk_index: usize,
        payload_bytes: u64,
        offset: u64,
        encoded_bytes: u64,
    ) -> Result<Arc<[u8]>, LocalShardChunkReadError> {
        let mut cached = lock_unpoisoned(&object.packed_inner);
        if let Some(entry) = cached.as_ref()
            && entry.chunk_index == chunk_index
            && entry.payload_bytes == payload_bytes
        {
            self.packed_inner_cache_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(Arc::clone(&entry.decoded));
        }

        self.packed_inner_cache_misses
            .fetch_add(1, Ordering::Relaxed);
        let encoded = self
            .read_cached_range(&object.file, path, offset, encoded_bytes)
            .map_err(LocalShardChunkReadError::Range)?;
        let decoded: Arc<[u8]> = self
            .decode_inner_payload_accounted(ShardProfileKind::PackedIndex, &encoded)
            .map_err(LocalShardChunkReadError::Shard)?
            .into();
        *cached = Some(CachedPackedInner {
            chunk_index,
            payload_bytes,
            decoded: Arc::clone(&decoded),
        });
        Ok(decoded)
    }

    fn record_physical_range(&self, bytes: u64) {
        self.physical_range_read_operations
            .fetch_add(1, Ordering::Relaxed);
        let _ = self.physical_encoded_bytes_read.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |total| Some(total.saturating_add(bytes)),
        );
    }

    fn read_cached_range(
        &self,
        file: &File,
        path: &PackagePath,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>, RangeReadError> {
        let started = Instant::now();
        let result = read_exact_at_shared(file, path, offset, length);
        let elapsed = elapsed_ns(started.elapsed());
        if result.is_ok() {
            self.record_physical_range(length);
            saturating_atomic_add(&self.physical_range_read_time_ns, elapsed);
        }
        result
    }

    fn record_currentness_metrics(&self, metrics: CurrentnessBatchMetrics) {
        saturating_atomic_add(&self.currentness_pre_use_batches, metrics.pre_use_batches);
        saturating_atomic_add(&self.currentness_post_use_batches, metrics.post_use_batches);
        saturating_atomic_add(&self.currentness_snapshot_batches, metrics.snapshot_batches);
        saturating_atomic_add(
            &self.currentness_root_metadata_checks,
            metrics.root_metadata_checks,
        );
        saturating_atomic_add(
            &self.currentness_named_object_resolutions,
            metrics.named_object_resolutions,
        );
        saturating_atomic_add(
            &self.currentness_object_fd_metadata_checks,
            metrics.object_fd_metadata_checks,
        );
        saturating_atomic_add(&self.currentness_time_ns, metrics.time_ns);
    }

    #[cfg(target_os = "linux")]
    fn open_cached_object(
        &self,
        path: &PackagePath,
        object_bytes_max: u64,
    ) -> Result<Arc<CachedObject>, RangeReadError> {
        self.cached_object_handle(path, object_bytes_max, true)
    }

    #[cfg(target_os = "linux")]
    fn cached_object_handle(
        &self,
        path: &PackagePath,
        object_bytes_max: u64,
        revalidate_hit: bool,
    ) -> Result<Arc<CachedObject>, RangeReadError> {
        validate_object_limit(object_bytes_max)?;
        let mut cache = self.lock_object_cache();
        if let Some(index) = cache.iter().position(|entry| entry.path == *path) {
            let object = cache.remove(index).expect("a located cache entry exists");
            cache.push_back(Arc::clone(&object));
            self.object_handle_cache_hits
                .fetch_add(1, Ordering::Relaxed);
            drop(cache);
            if object.bytes > object_bytes_max {
                return Err(RangeReadError::ObjectTooLarge {
                    path: path.to_string(),
                    actual: object.bytes,
                    maximum: object_bytes_max,
                });
            }
            if revalidate_hit && let Err(error) = self.revalidate_cached_object(path, &object) {
                self.evict_cached_object(path, object.identity);
                return Err(error);
            }
            return Ok(object);
        }
        drop(cache);
        self.object_handle_cache_misses
            .fetch_add(1, Ordering::Relaxed);
        let checked = self.open_object(path, object_bytes_max)?;
        let opened = Arc::new(CachedObject {
            path: path.clone(),
            file: checked.file,
            bytes: checked.bytes,
            identity: checked.identity,
            shard_index: Mutex::new(None),
            packed_inner: Mutex::new(None),
        });
        // Opening and canonical/currentness checks can block on the OS. Never
        // serialize unrelated shard misses behind the global LRU mutex. If a
        // concurrent worker installed this same path first, retain that
        // checked generation and discard this redundant handle.
        let mut cache = self.lock_object_cache();
        if let Some(index) = cache.iter().position(|entry| entry.path == *path) {
            let object = cache.remove(index).expect("a located cache entry exists");
            cache.push_back(Arc::clone(&object));
            if object.bytes != opened.bytes || object.identity != opened.identity {
                return Err(RangeReadError::ObjectChanged {
                    path: path.to_string(),
                });
            }
            return Ok(object);
        }
        if cache.len() == LOCAL_OBJECT_HANDLE_CACHE_MAX {
            cache.pop_front();
            self.object_handle_cache_evictions
                .fetch_add(1, Ordering::Relaxed);
        }
        cache.push_back(Arc::clone(&opened));
        self.object_handle_cache_peak_entries.fetch_max(
            u64::try_from(cache.len()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        Ok(opened)
    }

    #[cfg(not(target_os = "linux"))]
    fn open_cached_object(
        &self,
        _path: &PackagePath,
        _object_bytes_max: u64,
    ) -> Result<Arc<CachedObject>, RangeReadError> {
        Err(RangeReadError::UnsupportedPlatform)
    }

    #[cfg(not(target_os = "linux"))]
    fn cached_object_handle(
        &self,
        _path: &PackagePath,
        _object_bytes_max: u64,
        _revalidate_hit: bool,
    ) -> Result<Arc<CachedObject>, RangeReadError> {
        Err(RangeReadError::UnsupportedPlatform)
    }

    fn lock_object_cache(&self) -> MutexGuard<'_, VecDeque<Arc<CachedObject>>> {
        self.object_handle_cache_lock_acquisitions
            .fetch_add(1, Ordering::Relaxed);
        match self.object_handle_cache.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => {
                self.object_handle_cache_lock_contentions
                    .fetch_add(1, Ordering::Relaxed);
                let started = Instant::now();
                let guard = lock_unpoisoned(&self.object_handle_cache);
                saturating_atomic_add(
                    &self.object_handle_cache_lock_wait_time_ns,
                    elapsed_ns(started.elapsed()),
                );
                guard
            }
        }
    }

    fn evict_cached_object(&self, path: &PackagePath, identity: FileIdentity) {
        let mut cache = self.lock_object_cache();
        if let Some(index) = cache
            .iter()
            .position(|entry| entry.path == *path && entry.identity == identity)
        {
            cache.remove(index);
            self.object_handle_cache_evictions
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn revalidate_cached_snapshots(
        &self,
        snapshots: &[LocalObjectSnapshot],
    ) -> Result<(), RangeReadError> {
        let mut batch = self.begin_cached_snapshot_revalidation()?;
        for snapshot in snapshots {
            batch.validate_snapshot(snapshot)?;
        }
        batch.finish_snapshot();
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn revalidate_cached_object(
        &self,
        path: &PackagePath,
        checked: &CachedObject,
    ) -> Result<(), RangeReadError> {
        self.validate_root_identity()?;
        let (_current, metadata) =
            self.open_named_regular(path, "securely resolve cached object generation")?;
        if FileIdentity::from_metadata(&metadata) != checked.identity
            || metadata.len() != checked.bytes
        {
            return Err(RangeReadError::ObjectChanged {
                path: path.to_string(),
            });
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn revalidate_cached_object(
        &self,
        _path: &PackagePath,
        _checked: &CachedObject,
    ) -> Result<(), RangeReadError> {
        Err(RangeReadError::UnsupportedPlatform)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn revalidate_snapshots(
        &self,
        snapshots: &[LocalObjectSnapshot],
    ) -> Result<(), RangeReadError> {
        for snapshot in snapshots {
            let checked = self.open_object(&snapshot.path, snapshot.bytes)?;
            if checked.bytes != snapshot.bytes || checked.identity != snapshot.identity {
                return Err(RangeReadError::ObjectChanged {
                    path: snapshot.path.to_string(),
                });
            }
            self.revalidate_open_object(&snapshot.path, &checked)?;
        }
        Ok(())
    }

    pub(crate) fn revalidate_snapshot(
        &self,
        snapshot: &LocalObjectSnapshot,
    ) -> Result<(), RangeReadError> {
        let checked = self.open_object(&snapshot.path, snapshot.bytes)?;
        if checked.bytes != snapshot.bytes || checked.identity != snapshot.identity {
            return Err(RangeReadError::ObjectChanged {
                path: snapshot.path.to_string(),
            });
        }
        self.revalidate_open_object(&snapshot.path, &checked)
    }

    #[cfg(target_os = "linux")]
    fn open_object(
        &self,
        path: &PackagePath,
        object_bytes_max: u64,
    ) -> Result<CheckedObject, RangeReadError> {
        validate_object_limit(object_bytes_max)?;
        let started = Instant::now();
        self.validate_root_identity()?;
        let (file, opened) = self.open_named_regular(path, "securely open package object")?;
        self.object_open_operations.fetch_add(1, Ordering::Relaxed);
        if opened.len() > object_bytes_max {
            return Err(RangeReadError::ObjectTooLarge {
                path: path.to_string(),
                actual: opened.len(),
                maximum: object_bytes_max,
            });
        }
        saturating_atomic_add(&self.object_open_time_ns, elapsed_ns(started.elapsed()));
        Ok(CheckedObject {
            file,
            bytes: opened.len(),
            identity: FileIdentity::from_metadata(&opened),
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn open_object(
        &self,
        _path: &PackagePath,
        _object_bytes_max: u64,
    ) -> Result<CheckedObject, RangeReadError> {
        Err(RangeReadError::UnsupportedPlatform)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn validate_root_identity(&self) -> Result<(), RangeReadError> {
        let metadata = symlink_metadata(&self.root, "reinspect package root", "<root>")?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || !FileIdentity::from_metadata(&metadata).same_node(self.root_identity)
        {
            return Err(RangeReadError::RootChanged);
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn validate_root_identity(&self) -> Result<(), RangeReadError> {
        Err(RangeReadError::UnsupportedPlatform)
    }

    pub(crate) fn root_path(&self) -> &Path {
        &self.root
    }

    pub(crate) const fn same_root_node(&self, other: &Self) -> bool {
        self.root_identity.same_node(other.root_identity)
    }

    #[cfg(target_os = "linux")]
    fn open_named_regular(
        &self,
        path: &PackagePath,
        operation: &'static str,
    ) -> Result<(TrackedFile, Metadata), RangeReadError> {
        let descriptor = openat2(
            &self.root_directory,
            path.as_str(),
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        )
        .map_err(|error| map_secure_resolution_error(path, operation, error))?;
        let file = TrackedFile::new(
            File::from(descriptor),
            Arc::clone(&self.object_handle_gauge),
        );
        let metadata = file
            .metadata()
            .map_err(|error| io_error(operation, path.as_str(), error))?;
        if !metadata.is_file() {
            return Err(RangeReadError::NonRegularObject {
                path: path.to_string(),
            });
        }
        if metadata.nlink() != 1 {
            return Err(RangeReadError::Hardlink {
                path: path.to_string(),
                links: metadata.nlink(),
            });
        }
        Ok((file, metadata))
    }

    #[cfg(target_os = "linux")]
    fn revalidate_open_object(
        &self,
        path: &PackagePath,
        checked: &CheckedObject,
    ) -> Result<(), RangeReadError> {
        self.validate_root_identity()?;
        let (_current, metadata) =
            self.open_named_regular(path, "securely revalidate package object")?;
        if FileIdentity::from_metadata(&metadata) != checked.identity
            || metadata.len() != checked.bytes
        {
            return Err(RangeReadError::ObjectChanged {
                path: path.to_string(),
            });
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn revalidate_open_object(
        &self,
        _path: &PackagePath,
        _checked: &CheckedObject,
    ) -> Result<(), RangeReadError> {
        Err(RangeReadError::UnsupportedPlatform)
    }
}

impl LocalCurrentnessBatch<'_> {
    fn begin(
        reader: &LocalPackageReader,
        kind: CurrentnessBatchKind,
        object_capacity: usize,
    ) -> Result<LocalCurrentnessBatch<'_>, RangeReadError> {
        let mut batch = LocalCurrentnessBatch {
            reader,
            objects: Vec::with_capacity(object_capacity),
            metrics: CurrentnessBatchMetrics {
                pre_use_batches: u64::from(matches!(kind, CurrentnessBatchKind::Read)),
                snapshot_batches: u64::from(matches!(kind, CurrentnessBatchKind::Snapshot)),
                ..CurrentnessBatchMetrics::default()
            },
        };
        batch.metrics.root_metadata_checks = 1;
        let started = Instant::now();
        batch.reader.validate_root_identity()?;
        batch.metrics.time_ns = batch
            .metrics
            .time_ns
            .saturating_add(elapsed_ns(started.elapsed()));
        Ok(batch)
    }

    pub(crate) fn read_shard_chunk(
        &mut self,
        path: &PackagePath,
        kind: ShardProfileKind,
        chunk_index: usize,
        declared_bytes: u64,
    ) -> Result<LocalShardChunkBytes, LocalShardChunkReadError> {
        let maximum = u64::try_from(kind.encoded_shard_bytes_max())
            .map_err(|_| LocalShardChunkReadError::Shard(ShardCodecError::LengthOverflow))?;
        let checked = self
            .validated_object(path, maximum)
            .map_err(LocalShardChunkReadError::Range)?;
        self.reader
            .read_shard_chunk_from_cached(&checked, path, kind, chunk_index, declared_bytes)
    }

    pub(crate) fn validate_snapshot(
        &mut self,
        snapshot: &LocalObjectSnapshot,
    ) -> Result<(), RangeReadError> {
        let checked = self.validated_object(&snapshot.path, snapshot.bytes)?;
        if checked.bytes != snapshot.bytes || checked.identity != snapshot.identity {
            return Err(RangeReadError::ObjectChanged {
                path: snapshot.path.to_string(),
            });
        }
        Ok(())
    }

    pub(crate) fn finish_read(mut self) -> Result<(), RangeReadError> {
        self.metrics.post_use_batches = 1;
        self.post_validate()
    }

    pub(crate) fn finish_snapshot(self) {}

    fn validated_object(
        &mut self,
        path: &PackagePath,
        object_bytes_max: u64,
    ) -> Result<Arc<CachedObject>, RangeReadError> {
        validate_object_limit(object_bytes_max)?;
        if let Some(object) = self.objects.iter().find(|object| object.path == *path) {
            if object.bytes > object_bytes_max {
                return Err(RangeReadError::ObjectTooLarge {
                    path: path.to_string(),
                    actual: object.bytes,
                    maximum: object_bytes_max,
                });
            }
            return Ok(Arc::clone(object));
        }
        let object = self
            .reader
            .cached_object_handle(path, object_bytes_max, false)?;
        self.prevalidate_object(path, &object)?;
        self.objects.push(Arc::clone(&object));
        Ok(object)
    }

    #[cfg(target_os = "linux")]
    fn prevalidate_object(
        &mut self,
        path: &PackagePath,
        object: &CachedObject,
    ) -> Result<(), RangeReadError> {
        if object.path != *path {
            return Err(RangeReadError::EscapedRoot {
                path: path.to_string(),
            });
        }
        self.validate_object_identity(path, object)
    }

    #[cfg(not(target_os = "linux"))]
    fn prevalidate_object(
        &mut self,
        _path: &PackagePath,
        _object: &CachedObject,
    ) -> Result<(), RangeReadError> {
        Err(RangeReadError::UnsupportedPlatform)
    }

    #[cfg(target_os = "linux")]
    fn validate_object_identity(
        &mut self,
        path: &PackagePath,
        object: &CachedObject,
    ) -> Result<(), RangeReadError> {
        let started = Instant::now();
        self.metrics.named_object_resolutions =
            self.metrics.named_object_resolutions.saturating_add(1);
        self.metrics.object_fd_metadata_checks =
            self.metrics.object_fd_metadata_checks.saturating_add(1);
        let (_current, metadata) = self
            .reader
            .open_named_regular(path, "securely resolve batched object generation")?;
        self.metrics.time_ns = self
            .metrics
            .time_ns
            .saturating_add(elapsed_ns(started.elapsed()));
        if FileIdentity::from_metadata(&metadata) != object.identity
            || metadata.len() != object.bytes
        {
            return Err(RangeReadError::ObjectChanged {
                path: path.to_string(),
            });
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn post_validate(&mut self) -> Result<(), RangeReadError> {
        self.metrics.root_metadata_checks = self.metrics.root_metadata_checks.saturating_add(1);
        let started = Instant::now();
        self.reader.validate_root_identity()?;
        self.metrics.time_ns = self
            .metrics
            .time_ns
            .saturating_add(elapsed_ns(started.elapsed()));
        for index in 0..self.objects.len() {
            let object = Arc::clone(&self.objects[index]);
            let path = object.path.clone();
            self.validate_object_identity(&path, &object)?;
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn post_validate(&mut self) -> Result<(), RangeReadError> {
        Err(RangeReadError::UnsupportedPlatform)
    }
}

impl Drop for LocalCurrentnessBatch<'_> {
    fn drop(&mut self) {
        self.reader.record_currentness_metrics(self.metrics);
    }
}

impl LocalPackageRootSeal {
    const fn from_identity(identity: FileIdentity) -> Self {
        Self {
            device: identity.device,
            inode: identity.inode,
        }
    }

    pub(crate) const fn matches_node(self, device: u64, inode: u64) -> bool {
        self.device == device && self.inode == inode
    }

    const fn matches_identity(self, identity: FileIdentity) -> bool {
        self.matches_node(identity.device, identity.inode)
    }
}

impl PublishedPackageRootBinding {
    /// Opens and checks the path-form destination after its descriptor-relative
    /// publication proof has succeeded.
    pub(crate) fn open(
        destination: &Path,
        seal: LocalPackageRootSeal,
    ) -> Result<Self, RangeReadError> {
        let metadata = symlink_metadata(destination, "inspect published package root", "<root>")?;
        if metadata.file_type().is_symlink() {
            return Err(RangeReadError::Symlink {
                path: "<root>".to_owned(),
            });
        }
        if !metadata.is_dir() {
            return Err(RangeReadError::RootNotDirectory);
        }
        let canonical = fs::canonicalize(destination)
            .map_err(|error| io_error("canonicalize published package root", "<root>", error))?;
        let canonical_metadata =
            symlink_metadata(&canonical, "reinspect published package root", "<root>")?;
        if canonical_metadata.file_type().is_symlink() || !canonical_metadata.is_dir() {
            return Err(RangeReadError::RootNotDirectory);
        }
        let root_identity = FileIdentity::from_metadata(&canonical_metadata);
        if !seal.matches_identity(root_identity) {
            return Err(RangeReadError::RootChanged);
        }
        Ok(Self {
            root: canonical,
            root_identity,
            seal,
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RangeReadError {
    #[error("strict local package reads are currently supported only on Linux")]
    UnsupportedPlatform,
    #[error("package root is not a real directory")]
    RootNotDirectory,
    #[error("package root changed after it was opened")]
    RootChanged,
    #[error("package path {path} contains a symlink")]
    Symlink { path: String },
    #[error("package path {path} contains a non-directory parent component")]
    NonDirectoryComponent { path: String },
    #[error("package object {path} is not a regular file")]
    NonRegularObject { path: String },
    #[error("package object {path} has {links} hardlinks; exactly one is required")]
    Hardlink { path: String, links: u64 },
    #[error("package object {path} escaped the package root")]
    EscapedRoot { path: String },
    #[error("package object {path} changed while it was being opened or read")]
    ObjectChanged { path: String },
    #[error("package object {path} has {actual} bytes; maximum is {maximum}")]
    ObjectTooLarge {
        path: String,
        actual: u64,
        maximum: u64,
    },
    #[error("object byte limit must be in 1 through {maximum}, observed {actual}")]
    InvalidObjectLimit { actual: u64, maximum: u64 },
    #[error("range reads must be nonempty")]
    EmptyRange,
    #[error("range offset plus length overflowed u64")]
    RangeOverflow,
    #[error("range ({offset}, {length}) exceeds the {object_bytes}-byte package object")]
    RangeOutOfBounds {
        offset: u64,
        length: u64,
        object_bytes: u64,
    },
    #[error("shard-index range has {actual} bytes; expected 1 through {maximum}")]
    InvalidShardIndexRange { actual: u64, maximum: u64 },
    #[error("range length cannot be represented as usize")]
    LengthOverflow,
    #[error("short range read for {path}: expected {expected} bytes")]
    ShortRead { path: String, expected: u64 },
    #[error("{operation} failed for {path}: {kind:?}")]
    Io {
        operation: &'static str,
        path: String,
        kind: io::ErrorKind,
    },
}

#[derive(Debug)]
struct CheckedObject {
    file: TrackedFile,
    bytes: u64,
    identity: FileIdentity,
}

#[derive(Debug)]
struct CachedObject {
    path: PackagePath,
    file: TrackedFile,
    bytes: u64,
    identity: FileIdentity,
    shard_index: Mutex<Option<CachedShardIndex>>,
    packed_inner: Mutex<Option<CachedPackedInner>>,
}

#[derive(Debug)]
struct ObjectHandleGauge {
    current: AtomicU64,
    peak: AtomicU64,
}

impl ObjectHandleGauge {
    fn with_package_root() -> Self {
        Self {
            current: AtomicU64::new(1),
            peak: AtomicU64::new(1),
        }
    }

    fn acquire(&self) {
        let previous = self.current.fetch_add(1, Ordering::Relaxed);
        self.peak
            .fetch_max(previous.saturating_add(1), Ordering::Relaxed);
    }

    fn release(&self) {
        let previous = self.current.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(previous > 1, "the package-root descriptor remains owned");
    }

    fn current(&self) -> u64 {
        self.current.load(Ordering::Relaxed)
    }

    fn peak(&self) -> u64 {
        self.peak.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
struct TrackedFile {
    file: File,
    gauge: Arc<ObjectHandleGauge>,
}

impl TrackedFile {
    fn new(file: File, gauge: Arc<ObjectHandleGauge>) -> Self {
        gauge.acquire();
        Self { file, gauge }
    }
}

impl Deref for TrackedFile {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        &self.file
    }
}

impl DerefMut for TrackedFile {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.file
    }
}

impl Drop for TrackedFile {
    fn drop(&mut self) {
        self.gauge.release();
    }
}

#[derive(Debug)]
struct CachedShardIndex {
    kind: ShardProfileKind,
    payload_bytes: u64,
    index: Arc<ShardIndex>,
}

#[derive(Debug)]
struct CachedPackedInner {
    chunk_index: usize,
    payload_bytes: u64,
    decoded: Arc<[u8]>,
}

enum CachedShardIndexError {
    Range(RangeReadError),
    Shard(ShardCodecError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FileIdentity {
    const fn same_node(self, other: Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }

    #[cfg(target_os = "linux")]
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

fn read_exact_at(
    file: &mut File,
    path: &PackagePath,
    offset: u64,
    length: u64,
) -> Result<Vec<u8>, RangeReadError> {
    let length = usize::try_from(length).map_err(|_| RangeReadError::LengthOverflow)?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| io_error("seek object", path.as_str(), error))?;
    let mut bytes = vec![0; length];
    match file.read_exact(&mut bytes) {
        Ok(()) => Ok(bytes),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            Err(RangeReadError::ShortRead {
                path: path.to_string(),
                expected: u64::try_from(length).map_err(|_| RangeReadError::LengthOverflow)?,
            })
        }
        Err(error) => Err(io_error("read object range", path.as_str(), error)),
    }
}

#[cfg(target_os = "linux")]
fn read_exact_at_shared(
    file: &File,
    path: &PackagePath,
    offset: u64,
    length: u64,
) -> Result<Vec<u8>, RangeReadError> {
    let length = usize::try_from(length).map_err(|_| RangeReadError::LengthOverflow)?;
    let mut bytes = vec![0; length];
    let mut read = 0_usize;
    while read != length {
        let current_offset = offset
            .checked_add(u64::try_from(read).map_err(|_| RangeReadError::LengthOverflow)?)
            .ok_or(RangeReadError::RangeOverflow)?;
        let count = file
            .read_at(&mut bytes[read..], current_offset)
            .map_err(|error| io_error("read cached object range", path.as_str(), error))?;
        if count == 0 {
            return Err(RangeReadError::ShortRead {
                path: path.to_string(),
                expected: u64::try_from(length - read)
                    .map_err(|_| RangeReadError::LengthOverflow)?,
            });
        }
        read = read
            .checked_add(count)
            .ok_or(RangeReadError::LengthOverflow)?;
    }
    Ok(bytes)
}

#[cfg(not(target_os = "linux"))]
fn read_exact_at_shared(
    _file: &File,
    _path: &PackagePath,
    _offset: u64,
    _length: u64,
) -> Result<Vec<u8>, RangeReadError> {
    Err(RangeReadError::UnsupportedPlatform)
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn saturating_atomic_add(counter: &AtomicU64, value: u64) {
    if value == 0 {
        return;
    }
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(value))
    });
}

fn elapsed_ns(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(target_os = "linux")]
fn map_secure_resolution_error(
    path: &PackagePath,
    operation: &'static str,
    error: Errno,
) -> RangeReadError {
    match error {
        Errno::LOOP => RangeReadError::Symlink {
            path: path.to_string(),
        },
        Errno::XDEV => RangeReadError::EscapedRoot {
            path: path.to_string(),
        },
        Errno::NOTDIR => RangeReadError::NonDirectoryComponent {
            path: path.to_string(),
        },
        _ => io_error(operation, path.as_str(), io::Error::from(error)),
    }
}

fn validate_object_limit(limit: u64) -> Result<(), RangeReadError> {
    if limit == 0 || limit > GLOBAL_ENCODED_OUTER_SHARD_BYTES_MAX {
        return Err(RangeReadError::InvalidObjectLimit {
            actual: limit,
            maximum: GLOBAL_ENCODED_OUTER_SHARD_BYTES_MAX,
        });
    }
    Ok(())
}

fn symlink_metadata(
    path: &Path,
    operation: &'static str,
    display_path: &str,
) -> Result<Metadata, RangeReadError> {
    fs::symlink_metadata(path).map_err(|error| io_error(operation, display_path, error))
}

fn io_error(operation: &'static str, path: &str, error: io::Error) -> RangeReadError {
    RangeReadError::Io {
        operation,
        path: path.to_owned(),
        kind: error.kind(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        sync::{Arc, Barrier},
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "mirante4d-range-{}-{nonce}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn write(&self, relative: &str, bytes: &[u8]) {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn one_chunk_shard(kind: ShardProfileKind, fill: u8) -> Vec<u8> {
        let payload = vec![fill; kind.decoded_inner_bytes()];
        let mut chunks = vec![None; kind.chunks_per_shard()];
        chunks[0] = Some(payload.as_slice());
        crate::shard::assemble_shard(kind, &chunks).unwrap()
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn cached_transaction_batches_shared_prefix_and_object_currentness_exactly_once_per_phase() {
        let root = TempRoot::new();
        let kind = ShardProfileKind::Pixel2dUint8;
        let first_bytes = one_chunk_shard(kind, 7);
        let second_bytes = one_chunk_shard(kind, 9);
        root.write("images/shared/c/0", &first_bytes);
        root.write("images/shared/c/1", &second_bytes);
        let reader = LocalPackageReader::open(&root.0).unwrap();
        let first = PackagePath::parse("images/shared/c/0").unwrap();
        let second = PackagePath::parse("images/shared/c/1").unwrap();

        let mut batch = reader.begin_cached_read_transaction().unwrap();
        batch
            .read_shard_chunk(&first, kind, 0, first_bytes.len() as u64)
            .unwrap();
        batch
            .read_shard_chunk(&second, kind, 0, second_bytes.len() as u64)
            .unwrap();
        batch.finish_read().unwrap();

        let diagnostics = reader.read_diagnostics();
        assert_eq!(diagnostics.currentness_pre_use_batches, 1);
        assert_eq!(diagnostics.currentness_post_use_batches, 1);
        assert_eq!(diagnostics.currentness_snapshot_batches, 0);
        assert_eq!(diagnostics.currentness_root_metadata_checks, 2);
        assert_eq!(diagnostics.currentness_named_object_resolutions, 4);
        assert_eq!(diagnostics.currentness_object_fd_metadata_checks, 4);
        assert!(diagnostics.currentness_time_ns > 0);

        let first_snapshot = LocalObjectSnapshot::from_metadata(
            first,
            &fs::metadata(root.0.join("images/shared/c/0")).unwrap(),
        )
        .unwrap();
        let second_snapshot = LocalObjectSnapshot::from_metadata(
            second,
            &fs::metadata(root.0.join("images/shared/c/1")).unwrap(),
        )
        .unwrap();
        reader
            .revalidate_cached_snapshots(&[first_snapshot, second_snapshot])
            .unwrap();
        let diagnostics = reader.read_diagnostics();
        assert_eq!(diagnostics.currentness_snapshot_batches, 1);
        assert_eq!(diagnostics.currentness_root_metadata_checks, 3);
        assert_eq!(diagnostics.currentness_named_object_resolutions, 6);
        assert_eq!(diagnostics.currentness_object_fd_metadata_checks, 6);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn cached_transaction_post_use_commit_rejects_in_place_object_mutation() {
        let root = TempRoot::new();
        let kind = ShardProfileKind::Pixel2dUint8;
        let shard = one_chunk_shard(kind, 7);
        root.write("images/shared/c/0", &shard);
        let reader = LocalPackageReader::open(&root.0).unwrap();
        let path = PackagePath::parse("images/shared/c/0").unwrap();
        let mut batch = reader.begin_cached_read_transaction().unwrap();
        batch
            .read_shard_chunk(&path, kind, 0, shard.len() as u64)
            .unwrap();
        fs::write(root.0.join(path.as_str()), one_chunk_shard(kind, 8)).unwrap();
        assert!(matches!(
            batch.finish_read(),
            Err(RangeReadError::ObjectChanged { .. })
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn cached_transaction_post_use_commit_rejects_atomic_object_replacement() {
        let root = TempRoot::new();
        let kind = ShardProfileKind::Pixel2dUint8;
        let shard = one_chunk_shard(kind, 7);
        root.write("images/shared/c/0", &shard);
        let reader = LocalPackageReader::open(&root.0).unwrap();
        let path = PackagePath::parse("images/shared/c/0").unwrap();
        let mut batch = reader.begin_cached_read_transaction().unwrap();
        batch
            .read_shard_chunk(&path, kind, 0, shard.len() as u64)
            .unwrap();
        let replacement = root.0.join("replacement");
        fs::write(&replacement, &shard).unwrap();
        fs::rename(replacement, root.0.join(path.as_str())).unwrap();
        assert!(matches!(
            batch.finish_read(),
            Err(RangeReadError::ObjectChanged { .. })
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn cached_transaction_post_use_commit_rejects_ancestor_symlink_substitution() {
        use std::os::unix::fs::symlink;

        let root = TempRoot::new();
        let kind = ShardProfileKind::Pixel2dUint8;
        let shard = one_chunk_shard(kind, 7);
        root.write("images/shared/c/0", &shard);
        root.write("alternate/c/0", &shard);
        let reader = LocalPackageReader::open(&root.0).unwrap();
        let path = PackagePath::parse("images/shared/c/0").unwrap();
        let mut batch = reader.begin_cached_read_transaction().unwrap();
        batch
            .read_shard_chunk(&path, kind, 0, shard.len() as u64)
            .unwrap();
        fs::rename(
            root.0.join("images/shared"),
            root.0.join("images/original-shared"),
        )
        .unwrap();
        symlink(root.0.join("alternate"), root.0.join("images/shared")).unwrap();
        assert!(matches!(
            batch.finish_read(),
            Err(RangeReadError::Symlink { .. } | RangeReadError::ObjectChanged { .. })
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn packed_inner_decode_is_coalesced_and_reused_by_concurrent_record_reads() {
        let root = TempRoot::new();
        let kind = ShardProfileKind::PackedIndex;
        let payload = (0..kind.decoded_inner_bytes())
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let mut chunks = vec![None; kind.chunks_per_shard()];
        chunks[0] = Some(payload.as_slice());
        let shard = crate::shard::assemble_shard(kind, &chunks).unwrap();
        root.write("indexes/i00000000-s00/c/0/0", &shard);
        let reader = Arc::new(LocalPackageReader::open(&root.0).unwrap());
        let path = PackagePath::parse("indexes/i00000000-s00/c/0/0").unwrap();
        let workers = 16;
        let barrier = Arc::new(Barrier::new(workers));
        let mut joins = Vec::new();
        for _ in 0..workers {
            let reader = Arc::clone(&reader);
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            let declared_bytes = shard.len() as u64;
            joins.push(thread::spawn(move || {
                barrier.wait();
                reader
                    .read_shard_chunk(&path, kind, 0, declared_bytes)
                    .unwrap()
                    .decoded
                    .unwrap()
            }));
        }
        let decoded = joins
            .into_iter()
            .map(|join| join.join().unwrap())
            .collect::<Vec<_>>();
        assert!(decoded.iter().all(|bytes| bytes.as_ref() == payload));
        assert!(
            decoded
                .windows(2)
                .all(|pair| Arc::ptr_eq(&pair[0], &pair[1]))
        );

        let diagnostics = reader.read_diagnostics();
        assert_eq!(diagnostics.packed_inner_cache_misses, 1);
        assert_eq!(diagnostics.packed_inner_cache_hits, workers as u64 - 1);
        assert_eq!(diagnostics.shard_index_decode_operations, 1);
        assert_eq!(diagnostics.codec_decode_operations, 2);
        assert_eq!(diagnostics.physical_range_read_operations, 2);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn object_handle_gauge_includes_root_cache_and_transient_currentness_handles() {
        let root = TempRoot::new();
        let kind = ShardProfileKind::Pixel2dUint8;
        let shard = one_chunk_shard(kind, 7);
        root.write("images/shared/c/0", &shard);
        let reader = LocalPackageReader::open(&root.0).unwrap();
        let path = PackagePath::parse("images/shared/c/0").unwrap();

        let initial = reader.read_diagnostics();
        assert_eq!(initial.open_object_handles_current, 1);
        assert_eq!(initial.open_object_handles_peak, 1);

        reader
            .read_shard_chunk(&path, kind, 0, shard.len() as u64)
            .unwrap();
        let cached = reader.read_diagnostics();
        assert_eq!(cached.object_handle_cache_entries, 1);
        assert_eq!(cached.open_object_handles_current, 2);
        assert_eq!(cached.open_object_handles_peak, 3);

        reader
            .read_shard_chunk(&path, kind, 0, shard.len() as u64)
            .unwrap();
        let revalidated = reader.read_diagnostics();
        assert_eq!(revalidated.open_object_handles_current, 2);
        assert_eq!(revalidated.open_object_handles_peak, 3);
    }

    /// Release-only physical I/O workload for 1,024 distinct 3D inner
    /// chunks across sixteen valid indexed shards. It isolates the normal
    /// root-confined handle/index/range/decode path from catalog planning and
    /// sink-side scientific-fact accumulation.
    #[test]
    #[cfg(target_os = "linux")]
    #[ignore = "release-only 1,024-chunk physical I/O diagnostic"]
    fn three_dimensional_1024_unique_chunk_io_scale_diagnostic() {
        const SHARDS: u64 = 16;
        const CHUNKS_PER_SHARD: u64 = 64;
        const CHUNKS: u64 = SHARDS * CHUNKS_PER_SHARD;

        let root = TempRoot::new();
        let kind = ShardProfileKind::Pixel3dUint16;
        let decoded = vec![0x5a; kind.decoded_inner_bytes()];
        let chunks = vec![Some(decoded.as_slice()); kind.chunks_per_shard()];
        let shard = crate::shard::assemble_shard(kind, &chunks).unwrap();
        let encoded_inner_bytes = {
            let tail_bytes = kind.index_tail_bytes();
            let index = decode_shard_index_tail(
                kind,
                &shard[shard.len() - tail_bytes..],
                u64::try_from(shard.len() - tail_bytes).unwrap(),
            )
            .unwrap();
            index.require_entry(0).unwrap().nbytes()
        };
        for shard_index in 0..SHARDS {
            root.write(&format!("images/i00000000/s00/c/0/0/{shard_index}"), &shard);
        }
        let reader = LocalPackageReader::open(&root.0).unwrap();

        let started = Instant::now();
        for shard_index in 0..SHARDS {
            let path =
                PackagePath::parse(&format!("images/i00000000/s00/c/0/0/{shard_index}")).unwrap();
            for chunk_index in 0..CHUNKS_PER_SHARD {
                let mut transaction = reader.begin_cached_read_transaction().unwrap();
                let read = transaction
                    .read_shard_chunk(
                        &path,
                        kind,
                        usize::try_from(chunk_index).unwrap(),
                        u64::try_from(shard.len()).unwrap(),
                    )
                    .unwrap();
                let encoded = read.encoded.unwrap();
                let result = reader
                    .decode_inner_payload_accounted(kind, &encoded)
                    .unwrap();
                std::hint::black_box(result);
                transaction.finish_read().unwrap();
            }
        }
        let wall_time_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let diagnostics = reader.read_diagnostics();
        let currentness_syscalls = diagnostics
            .currentness_root_metadata_checks
            .saturating_add(diagnostics.currentness_named_object_resolutions)
            .saturating_add(diagnostics.currentness_object_fd_metadata_checks);
        let expected_encoded_bytes = SHARDS
            .checked_mul(u64::try_from(kind.index_tail_bytes()).unwrap())
            .and_then(|bytes| bytes.checked_add(CHUNKS * encoded_inner_bytes))
            .unwrap();
        let expected_decoded_bytes = SHARDS
            .checked_mul(u64::try_from(kind.index_tail_bytes() - 4).unwrap())
            .and_then(|bytes| {
                bytes.checked_add(CHUNKS * u64::try_from(kind.decoded_inner_bytes()).unwrap())
            })
            .unwrap();

        assert_eq!(diagnostics.object_open_operations, SHARDS);
        assert_eq!(diagnostics.object_handle_cache_misses, SHARDS);
        assert_eq!(diagnostics.object_handle_cache_hits, CHUNKS - SHARDS);
        assert_eq!(diagnostics.object_handle_cache_evictions, 0);
        assert_eq!(diagnostics.shard_index_cache_misses, SHARDS);
        assert_eq!(diagnostics.shard_index_cache_hits, CHUNKS - SHARDS);
        assert_eq!(diagnostics.shard_index_decode_operations, SHARDS);
        assert_eq!(diagnostics.currentness_pre_use_batches, CHUNKS);
        assert_eq!(diagnostics.currentness_post_use_batches, CHUNKS);
        assert_eq!(diagnostics.currentness_snapshot_batches, 0);
        assert_eq!(diagnostics.currentness_root_metadata_checks, CHUNKS * 2);
        assert_eq!(diagnostics.currentness_named_object_resolutions, CHUNKS * 2);
        assert_eq!(
            diagnostics.currentness_object_fd_metadata_checks,
            CHUNKS * 2
        );
        assert_eq!(currentness_syscalls, CHUNKS * 6);
        assert_eq!(diagnostics.physical_range_read_operations, CHUNKS + SHARDS);
        assert_eq!(
            diagnostics.physical_encoded_bytes_read,
            expected_encoded_bytes
        );
        assert_eq!(diagnostics.codec_decode_operations, CHUNKS + SHARDS);
        assert_eq!(diagnostics.codec_decoded_bytes, expected_decoded_bytes);
        assert_eq!(diagnostics.object_handle_cache_lock_contentions, 0);

        eprintln!(
            "storage-physical-1024 wall_time_ns={wall_time_ns} \
             currentness_syscalls={currentness_syscalls} currentness_time_ns={} \
             object_opens={} object_open_time_ns={} range_reads={} encoded_bytes={} \
             range_read_time_ns={} codec_decodes={} codec_decoded_bytes={} \
             codec_decode_time_ns={} cache_lock_acquisitions={} \
             cache_lock_contentions={} cache_lock_wait_time_ns={}",
            diagnostics.currentness_time_ns,
            diagnostics.object_open_operations,
            diagnostics.object_open_time_ns,
            diagnostics.physical_range_read_operations,
            diagnostics.physical_encoded_bytes_read,
            diagnostics.physical_range_read_time_ns,
            diagnostics.codec_decode_operations,
            diagnostics.codec_decoded_bytes,
            diagnostics.codec_decode_time_ns,
            diagnostics.object_handle_cache_lock_acquisitions,
            diagnostics.object_handle_cache_lock_contentions,
            diagnostics.object_handle_cache_lock_wait_time_ns,
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn reads_only_the_requested_nonempty_range_and_index_tail() {
        let root = TempRoot::new();
        let bytes = (0_u16..8_192)
            .map(|value| (value % 251) as u8)
            .collect::<Vec<_>>();
        root.write("images/i00000000/s00/c/0/0/0/0/0", &bytes);
        let reader = LocalPackageReader::open(&root.0).unwrap();
        let path = PackagePath::parse("images/i00000000/s00/c/0/0/0/0/0").unwrap();
        let write_time_snapshot = LocalObjectSnapshot::from_metadata(
            path.clone(),
            &fs::metadata(root.0.join(path.as_str())).unwrap(),
        )
        .unwrap();

        assert_eq!(reader.object_open_operations(), 0);
        assert_eq!(reader.object_info(&path, 8_192).unwrap().bytes(), 8_192);
        reader.revalidate_snapshot(&write_time_snapshot).unwrap();
        assert_eq!(
            reader.read_range(&path, 17, 31, 8_192).unwrap(),
            bytes[17..48]
        );
        let (tail, payload_bytes) = reader.read_shard_index_tail(&path, 260, 8_192).unwrap();
        assert_eq!(payload_bytes, 7_932);
        assert_eq!(tail, bytes[7_932..]);

        let (_whole, snapshot) = reader.read_object_with_snapshot(&path, 8_192).unwrap();
        let replacement = root.0.join("replacement.bin");
        fs::write(&replacement, vec![7; 8_192]).unwrap();
        fs::rename(replacement, root.0.join(path.as_str())).unwrap();
        assert!(matches!(
            reader.revalidate_snapshot(&snapshot),
            Err(RangeReadError::ObjectChanged { .. })
        ));
        assert!(matches!(
            reader.revalidate_snapshot(&write_time_snapshot),
            Err(RangeReadError::ObjectChanged { .. })
        ));
        assert_eq!(reader.object_open_operations(), 7);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn streams_full_object_digest_with_bounded_cancellation_and_snapshot() {
        let root = TempRoot::new();
        let bytes = (0..(FULL_OBJECT_HASH_BUFFER_BYTES * 2 + 17))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        root.write("objects/payload.bin", &bytes);
        let reader = LocalPackageReader::open(&root.0).unwrap();
        let path = PackagePath::parse("objects/payload.bin").unwrap();

        let hashed = reader
            .hash_object_with_snapshot(&path, bytes.len() as u64, || false)
            .unwrap();
        assert_eq!(hashed.facts, ExactBytesHasher::hash(&bytes).unwrap());
        assert!(matches!(
            reader.hash_object_with_snapshot(&path, bytes.len() as u64 - 1, || false),
            Err(LocalObjectHashError::DeclaredLengthMismatch { .. })
        ));

        let mut polls = 0_u8;
        assert!(matches!(
            reader.hash_object_with_snapshot(&path, bytes.len() as u64, || {
                polls += 1;
                polls == 3
            }),
            Err(LocalObjectHashError::Cancelled)
        ));

        let replacement = root.0.join("replacement.bin");
        fs::write(&replacement, &bytes).unwrap();
        fs::rename(replacement, root.0.join(path.as_str())).unwrap();
        assert!(matches!(
            reader.revalidate_snapshot(&hashed.snapshot),
            Err(RangeReadError::ObjectChanged { .. })
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn rejects_empty_overflowing_out_of_bounds_and_oversized_reads() {
        let root = TempRoot::new();
        root.write("m4d/profile.json", &[1; 32]);
        let reader = LocalPackageReader::open(&root.0).unwrap();
        let path = PackagePath::parse("m4d/profile.json").unwrap();

        assert_eq!(
            reader.read_range(&path, 0, 0, 32),
            Err(RangeReadError::EmptyRange)
        );
        assert_eq!(
            reader.read_range(&path, u64::MAX, 2, 32),
            Err(RangeReadError::RangeOverflow)
        );
        assert!(matches!(
            reader.read_range(&path, 31, 2, 32),
            Err(RangeReadError::RangeOutOfBounds { .. })
        ));
        assert!(matches!(
            reader.object_info(&path, 31),
            Err(RangeReadError::ObjectTooLarge { .. })
        ));
        assert!(matches!(
            reader.object_info(&path, u64::MAX),
            Err(RangeReadError::InvalidObjectLimit { .. })
        ));
        assert!(matches!(
            reader.read_shard_index_tail(&path, 4_097, 32),
            Err(RangeReadError::InvalidShardIndexRange { .. })
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn rejects_symlink_hardlink_and_nonregular_objects() {
        use std::os::unix::fs::symlink;

        let root = TempRoot::new();
        root.write("outside.bin", &[7; 8]);
        fs::create_dir_all(root.0.join("m4d")).unwrap();
        symlink(root.0.join("outside.bin"), root.0.join("m4d/link.bin")).unwrap();
        fs::create_dir(root.0.join("real")).unwrap();
        fs::write(root.0.join("real/data.bin"), [9; 8]).unwrap();
        symlink(root.0.join("real"), root.0.join("linked")).unwrap();
        fs::hard_link(root.0.join("outside.bin"), root.0.join("m4d/hard.bin")).unwrap();
        fs::create_dir(root.0.join("m4d/directory.bin")).unwrap();
        let reader = LocalPackageReader::open(&root.0).unwrap();

        let link = PackagePath::parse("m4d/link.bin").unwrap();
        assert!(matches!(
            reader.object_info(&link, 8),
            Err(RangeReadError::Symlink { .. })
        ));
        let linked_parent = PackagePath::parse("linked/data.bin").unwrap();
        assert!(matches!(
            reader.object_info(&linked_parent, 8),
            Err(RangeReadError::Symlink { .. })
        ));
        let hard = PackagePath::parse("m4d/hard.bin").unwrap();
        assert!(matches!(
            reader.object_info(&hard, 8),
            Err(RangeReadError::Hardlink { .. })
        ));
        let directory = PackagePath::parse("m4d/directory.bin").unwrap();
        assert!(matches!(
            reader.object_info(&directory, 8),
            Err(RangeReadError::NonRegularObject { .. })
        ));
    }
}

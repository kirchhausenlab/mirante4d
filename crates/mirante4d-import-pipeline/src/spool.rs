//! Constant-file resumable import spool.

use std::{
    ffi::OsStr,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use mirante4d_domain::IntensityDType;
use mirante4d_identity::{Sha256Digest, Sha256Hasher};
#[cfg(test)]
use mirante4d_storage::encode_inner_payload;
use mirante4d_storage::{
    CanonicalEncodedInner, PackedIndexCoordinates, PackedIndexRecord, ShardProfileKind,
    decode_inner_payload,
};
use rustix::{
    fd::OwnedFd,
    fs::{AtFlags, CWD, FileType, Mode, OFlags, fstat, fsync, openat, statat, unlinkat},
    io::Errno,
};

use crate::ImportError;

const HEADER_FILE: &str = "header";
const JOURNAL_FILE: &str = "journal";
const PAYLOAD_FILE: &str = "payload";
const WATERMARK_FILE: &str = "watermark";
const SPOOL_SCHEMA: &[u8] = b"mirante4d-import-spool-2\n";
const HEADER_BYTES: usize = SPOOL_SCHEMA.len() + 32 + 32;

const JOURNAL_RECORD_BYTES: usize = 160;
const JOURNAL_BODY_BYTES: usize = 128;
const WATERMARK_RECORD_BYTES: usize = 96;
const WATERMARK_BODY_BYTES: usize = 64;
const PACKED_INDEX_BYTES: usize = 64;
const FLAG_PIXEL_PRESENT: u8 = 1 << 0;
const FLAG_VALIDITY_PRESENT: u8 = 1 << 1;
const KNOWN_FLAGS: u8 = FLAG_PIXEL_PRESENT | FLAG_VALIDITY_PRESENT;
const MISSING_OFFSET: u64 = u64::MAX;

// One lost process may force at most one of these bounded batches to be
// recomputed. The byte ceiling is deliberately below one outer float32 shard,
// while the work and age ceilings keep sparse or highly-compressible batches
// bounded independently of payload size.
const DURABILITY_BATCH_WORK_UNITS_MAX: u64 = 512;
const DURABILITY_BATCH_PAYLOAD_BYTES_MAX: u64 = 64 * 1024 * 1024;
const DURABILITY_BATCH_AGE_MAX: Duration = Duration::from_secs(15);

const DIRECTORY_OPEN_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW);
const FILE_READ_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK);
const FILE_APPEND_FLAGS: OFlags = OFlags::RDWR
    .union(OFlags::APPEND)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK);
const FILE_CREATE_FLAGS: OFlags = OFlags::RDWR
    .union(OFlags::APPEND)
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW);

/// Exact import-plan and source-generation binding for one checkpoint spool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SpoolBinding {
    plan_digest: Sha256Digest,
    source_fingerprint: Sha256Digest,
}

impl SpoolBinding {
    pub(crate) const fn new(plan_digest: Sha256Digest, source_fingerprint: Sha256Digest) -> Self {
        Self {
            plan_digest,
            source_fingerprint,
        }
    }
}

/// Canonical order key for one completed logical brick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SpoolWorkUnitKey {
    image_ordinal: u32,
    scale: u32,
    t: u32,
    c: u32,
    z_chunk: u32,
    y_chunk: u32,
    x_chunk: u32,
}

impl SpoolWorkUnitKey {
    pub(crate) const fn new(
        image_ordinal: u32,
        scale: u32,
        t: u32,
        c: u32,
        z_chunk: u32,
        y_chunk: u32,
        x_chunk: u32,
    ) -> Self {
        Self {
            image_ordinal,
            scale,
            t,
            c,
            z_chunk,
            y_chunk,
            x_chunk,
        }
    }

    pub(crate) const fn coordinates(self) -> PackedIndexCoordinates {
        PackedIndexCoordinates::new(
            self.image_ordinal,
            self.scale,
            self.t,
            self.c,
            self.z_chunk,
            self.y_chunk,
            self.x_chunk,
        )
    }

    #[cfg(test)]
    pub(crate) const fn from_coordinates(coordinates: PackedIndexCoordinates) -> Self {
        Self::new(
            coordinates.image_ordinal(),
            coordinates.scale(),
            coordinates.t(),
            coordinates.c(),
            coordinates.z_chunk(),
            coordinates.y_chunk(),
            coordinates.x_chunk(),
        )
    }
}

/// One decoded inner chunk supplied to the spool.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct SpoolChunkInput<'a> {
    pub(crate) kind: ShardProfileKind,
    pub(crate) decoded: &'a [u8],
}

#[cfg(test)]
impl<'a> SpoolChunkInput<'a> {
    pub(crate) const fn new(kind: ShardProfileKind, decoded: &'a [u8]) -> Self {
        Self { kind, decoded }
    }
}

/// One already codec-validated canonical inner chunk supplied by a CPU
/// worker. The spool preserves these exact bytes and does not encode again.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SpoolEncodedChunkInput<'a> {
    pub(crate) inner: &'a CanonicalEncodedInner,
}

impl<'a> SpoolEncodedChunkInput<'a> {
    pub(crate) const fn new(inner: &'a CanonicalEncodedInner) -> Self {
        Self { inner }
    }
}

/// One decoded inner chunk recovered from the spool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpoolChunk {
    pub(crate) kind: ShardProfileKind,
    pub(crate) decoded: Vec<u8>,
}

/// One complete recovered work unit.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpoolWorkUnit {
    pub(crate) key: SpoolWorkUnitKey,
    pub(crate) pixel: Option<SpoolChunk>,
    pub(crate) validity: Option<SpoolChunk>,
    pub(crate) packed_index: [u8; PACKED_INDEX_BYTES],
}

/// Immutable journal facts needed to recover one work unit through
/// positional payload reads in a CPU worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SpoolWorkUnitDescriptor {
    key: SpoolWorkUnitKey,
    pixel: Option<SpoolChunkDescriptor>,
    validity: Option<SpoolChunkDescriptor>,
    packed_index: [u8; PACKED_INDEX_BYTES],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SpoolChunkDescriptor {
    kind: ShardProfileKind,
    offset: u64,
    encoded_bytes: u64,
}

/// Read-only payload snapshot shared by pyramid workers.
///
/// The append owner may extend the payload while workers read immutable prior
/// ranges. `read_at` avoids sharing the append handle's file position.
pub(crate) struct SpoolPayloadReader {
    payload_path: PathBuf,
    payload: File,
    snapshot_bytes: u64,
}

/// One selectively decoded spool component and the codec work attributable to
/// that read. An absent component performs no payload I/O or codec operation.
#[derive(Debug)]
pub(crate) struct SpoolDecodedComponent {
    pub(crate) chunk: Option<SpoolChunk>,
    pub(crate) codec_decode_calls: u64,
    pub(crate) codec_decode_time_ns: u64,
}

/// Diagnostics for the current checkpoint and this process's durability calls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SpoolDiagnostics {
    pub(crate) checkpoint_payload_bytes: u64,
    pub(crate) checkpoint_journal_bytes: u64,
    pub(crate) checkpoint_watermark_bytes: u64,
    pub(crate) checkpoint_durable_work_units: u64,
    pub(crate) checkpoint_pending_work_units: u64,
    pub(crate) checkpoint_committed_batches: u64,
    pub(crate) codec_encode_calls: u64,
    pub(crate) codec_encode_time_ns: u64,
    pub(crate) codec_decode_calls: u64,
    pub(crate) codec_decode_time_ns: u64,
    pub(crate) sync_calls: u64,
    pub(crate) sync_time_ns: u64,
}

/// Four-file, append-only checkpoint spool with a separately durable prefix.
pub(crate) struct ImportSpool {
    directory_path: PathBuf,
    _directory: OwnedFd,
    header: File,
    journal: File,
    payload: File,
    watermark: File,
    records: Vec<JournalRecord>,
    maximum_records: u64,
    payload_bytes: u64,
    durable_records: u64,
    durable_payload_bytes: u64,
    committed_batches: u64,
    last_watermark_digest: [u8; 32],
    pending_started: Option<Instant>,
    batch_policy: DurabilityBatchPolicy,
    codec_metrics: CodecMetrics,
    sync_metrics: SyncMetrics,
    writable: bool,
    #[cfg(test)]
    failpoint: Option<SpoolFailpoint>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct JournalRecord {
    key: SpoolWorkUnitKey,
    pixel: Option<ChunkRecord>,
    validity: Option<ChunkRecord>,
    packed_index: [u8; PACKED_INDEX_BYTES],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChunkRecord {
    kind: ShardProfileKind,
    offset: u64,
    encoded_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DurablePrefix {
    sequence: u64,
    record_count: u64,
    journal_bytes: u64,
    payload_bytes: u64,
    previous_digest: [u8; 32],
}

#[derive(Clone, Copy, Debug)]
struct DurabilityBatchPolicy {
    work_units_max: u64,
    payload_bytes_max: u64,
    age_max: Duration,
}

impl DurabilityBatchPolicy {
    const PRODUCTION: Self = Self {
        work_units_max: DURABILITY_BATCH_WORK_UNITS_MAX,
        payload_bytes_max: DURABILITY_BATCH_PAYLOAD_BYTES_MAX,
        age_max: DURABILITY_BATCH_AGE_MAX,
    };
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SyncMetrics {
    calls: u64,
    elapsed_ns: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CodecMetrics {
    encode_calls: u64,
    encode_elapsed_ns: u64,
    decode_calls: u64,
    decode_elapsed_ns: u64,
}

struct RecoveredSpool {
    records: Vec<JournalRecord>,
    payload_bytes: u64,
    durable_records: u64,
    committed_batches: u64,
    last_watermark_digest: [u8; 32],
    codec_metrics: CodecMetrics,
}

struct RecoveryMetrics<'a> {
    codec: &'a mut CodecMetrics,
    sync: &'a mut SyncMetrics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpoolFailpoint {
    AfterPayloadAppend,
    AfterJournalAppend,
    BeforePayloadSync,
    AfterPayloadSync,
    BeforeJournalSync,
    AfterJournalSync,
    AfterWatermarkAppend,
    BeforeWatermarkSync,
    AfterWatermarkSync,
}

impl ImportSpool {
    pub(crate) fn commit_expired(&mut self) -> Result<(), ImportError> {
        if self
            .pending_started
            .is_some_and(|started| started.elapsed() >= self.batch_policy.age_max)
        {
            self.commit_pending()?;
        }
        Ok(())
    }

    /// Best-effort successful-import cleanup that unlinks only names still
    /// bound to the exact spool file descriptors opened by this instance.
    pub(crate) fn cleanup_owned_files(&self) {
        for (name, file) in [
            (HEADER_FILE, &self.header),
            (JOURNAL_FILE, &self.journal),
            (PAYLOAD_FILE, &self.payload),
            (WATERMARK_FILE, &self.watermark),
        ] {
            let _ = unlink_if_owned(&self._directory, name, file);
        }
    }

    /// Removes the now-empty checkpoint directory only when its pathname still
    /// resolves to the exact held directory descriptor. A renamed/replaced
    /// path is deliberately left untouched.
    pub(crate) fn cleanup_owned_directory(&self) {
        let Ok(held) = fstat(&self._directory) else {
            return;
        };
        let Ok(named) = statat(
            CWD,
            self.directory_path.as_path(),
            AtFlags::SYMLINK_NOFOLLOW,
        ) else {
            return;
        };
        if held.st_dev != named.st_dev
            || held.st_ino != named.st_ino
            || FileType::from_raw_mode(named.st_mode) != FileType::Directory
        {
            return;
        }
        let _ = unlinkat(CWD, self.directory_path.as_path(), AtFlags::REMOVEDIR);
    }

    /// Opens an exact matching checkpoint or creates the four fixed files in
    /// an existing caller-owned directory.
    pub(crate) fn open_or_create(
        directory: &Path,
        binding: SpoolBinding,
        maximum_records: u64,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<Self, ImportError> {
        Self::open_or_create_with_policy(
            directory,
            binding,
            maximum_records,
            DurabilityBatchPolicy::PRODUCTION,
            is_cancelled,
        )
    }

    fn open_or_create_with_policy(
        directory: &Path,
        binding: SpoolBinding,
        maximum_records: u64,
        batch_policy: DurabilityBatchPolicy,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<Self, ImportError> {
        if batch_policy.work_units_max == 0
            || batch_policy.work_units_max > DURABILITY_BATCH_WORK_UNITS_MAX
            || batch_policy.payload_bytes_max == 0
            || batch_policy.payload_bytes_max > DURABILITY_BATCH_PAYLOAD_BYTES_MAX
            || batch_policy.age_max > DURABILITY_BATCH_AGE_MAX
        {
            return Err(ImportError::InvalidRequest(
                "the checkpoint durability-batch policy exceeds its fixed bounds",
            ));
        }
        let directory_path = directory.to_path_buf();
        let directory_fd = openat(CWD, directory, DIRECTORY_OPEN_FLAGS, Mode::empty())
            .map_err(|source| io_error("open checkpoint directory", &directory_path, source))?;
        let mut sync_metrics = SyncMetrics::default();
        let mut codec_metrics = CodecMetrics::default();

        let states = [
            entry_state(&directory_fd, HEADER_FILE)?,
            entry_state(&directory_fd, JOURNAL_FILE)?,
            entry_state(&directory_fd, PAYLOAD_FILE)?,
            entry_state(&directory_fd, WATERMARK_FILE)?,
        ];
        let all_absent = states.iter().all(|state| *state == EntryState::Absent);
        let all_present = states.iter().all(|state| *state == EntryState::RegularFile);
        if !all_absent && !all_present {
            return invalid_checkpoint("the fixed spool file set is incomplete");
        }

        let (header, journal, payload, watermark) = if all_absent {
            create_files(&directory_fd, &directory_path, binding, &mut sync_metrics)?
        } else {
            (
                validate_header(&directory_fd, &directory_path, binding)?,
                open_file(
                    &directory_fd,
                    &directory_path,
                    JOURNAL_FILE,
                    FILE_APPEND_FLAGS,
                    "open checkpoint journal",
                )?,
                open_file(
                    &directory_fd,
                    &directory_path,
                    PAYLOAD_FILE,
                    FILE_APPEND_FLAGS,
                    "open checkpoint payload",
                )?,
                open_file(
                    &directory_fd,
                    &directory_path,
                    WATERMARK_FILE,
                    FILE_APPEND_FLAGS,
                    "open checkpoint watermark",
                )?,
            )
        };

        let recovered = {
            let mut recovery_metrics = RecoveryMetrics {
                codec: &mut codec_metrics,
                sync: &mut sync_metrics,
            };
            recover_durable_prefix(
                &directory_path,
                &journal,
                &payload,
                &watermark,
                maximum_records,
                &mut is_cancelled,
                &mut recovery_metrics,
            )?
        };
        Ok(Self {
            directory_path,
            _directory: directory_fd,
            header,
            journal,
            payload,
            watermark,
            records: recovered.records,
            maximum_records,
            payload_bytes: recovered.payload_bytes,
            durable_records: recovered.durable_records,
            durable_payload_bytes: recovered.payload_bytes,
            committed_batches: recovered.committed_batches,
            last_watermark_digest: recovered.last_watermark_digest,
            pending_started: None,
            batch_policy,
            codec_metrics: recovered.codec_metrics,
            sync_metrics,
            writable: true,
            #[cfg(test)]
            failpoint: None,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub(crate) fn contains(&self, key: SpoolWorkUnitKey) -> bool {
        self.find(key).is_ok()
    }

    pub(crate) fn keys(&self) -> impl ExactSizeIterator<Item = SpoolWorkUnitKey> + '_ {
        self.records.iter().map(|record| record.key)
    }

    pub(crate) fn diagnostics(&self) -> SpoolDiagnostics {
        let record_count = u64::try_from(self.records.len()).expect("record count was bounded");
        SpoolDiagnostics {
            checkpoint_payload_bytes: self.payload_bytes,
            checkpoint_journal_bytes: record_count
                .checked_mul(JOURNAL_RECORD_BYTES as u64)
                .expect("record count was bounded"),
            checkpoint_watermark_bytes: self
                .committed_batches
                .checked_mul(WATERMARK_RECORD_BYTES as u64)
                .expect("watermark count was bounded"),
            checkpoint_durable_work_units: self.durable_records,
            checkpoint_pending_work_units: record_count - self.durable_records,
            checkpoint_committed_batches: self.committed_batches,
            codec_encode_calls: self.codec_metrics.encode_calls,
            codec_encode_time_ns: self.codec_metrics.encode_elapsed_ns,
            codec_decode_calls: self.codec_metrics.decode_calls,
            codec_decode_time_ns: self.codec_metrics.decode_elapsed_ns,
            sync_calls: self.sync_metrics.calls,
            sync_time_ns: self.sync_metrics.elapsed_ns,
        }
    }

    /// Encodes and appends a new work unit to the current bounded durability
    /// batch. Existing keys are a no-op. New keys must follow canonical order.
    #[cfg(test)]
    pub(crate) fn append_if_absent(
        &mut self,
        key: SpoolWorkUnitKey,
        pixel: Option<SpoolChunkInput<'_>>,
        validity: Option<SpoolChunkInput<'_>>,
        packed_index: PackedIndexRecord,
    ) -> Result<bool, ImportError> {
        if !self.validate_new_key(key)? {
            return Ok(false);
        }
        validate_input(key, pixel, validity, packed_index)?;

        // Complete every fallible codec operation before touching the files.
        let encoded_pixel = pixel
            .map(|chunk| {
                timed_codec_encode(&mut self.codec_metrics, || {
                    encode_inner_payload(chunk.kind, chunk.decoded)
                })
            })
            .transpose()?;
        let encoded_validity = validity
            .map(|chunk| {
                timed_codec_encode(&mut self.codec_metrics, || {
                    encode_inner_payload(chunk.kind, chunk.decoded)
                })
            })
            .transpose()?;
        self.append_encoded_bytes(
            key,
            pixel.map(|chunk| (chunk.kind, encoded_pixel.as_deref().unwrap())),
            validity.map(|chunk| (chunk.kind, encoded_validity.as_deref().unwrap())),
            packed_index,
        )
    }

    /// Appends codec-validated worker output without decoding or encoding it
    /// again. New keys retain the same canonical order and v2 journal schema.
    pub(crate) fn append_encoded_if_absent(
        &mut self,
        key: SpoolWorkUnitKey,
        pixel: Option<SpoolEncodedChunkInput<'_>>,
        validity: Option<SpoolEncodedChunkInput<'_>>,
        packed_index: PackedIndexRecord,
        codec_encode_calls: u64,
        codec_encode_time_ns: u64,
    ) -> Result<bool, ImportError> {
        if !self.validate_new_key(key)? {
            return Ok(false);
        }
        let pixel_kind = pixel.map(|chunk| chunk.inner.kind());
        let validity_kind = validity.map(|chunk| chunk.inner.kind());
        validate_input_kinds(key, pixel_kind, validity_kind, packed_index)?;
        self.codec_metrics.encode_calls = self
            .codec_metrics
            .encode_calls
            .checked_add(codec_encode_calls)
            .ok_or(ImportError::Overflow)?;
        self.codec_metrics.encode_elapsed_ns = self
            .codec_metrics
            .encode_elapsed_ns
            .checked_add(codec_encode_time_ns)
            .ok_or(ImportError::Overflow)?;
        self.append_encoded_bytes(
            key,
            pixel.map(|chunk| (chunk.inner.kind(), chunk.inner.bytes())),
            validity.map(|chunk| (chunk.inner.kind(), chunk.inner.bytes())),
            packed_index,
        )
    }

    fn validate_new_key(&self, key: SpoolWorkUnitKey) -> Result<bool, ImportError> {
        if self.contains(key) {
            return Ok(false);
        }
        if !self.writable {
            return Err(ImportError::InvalidRequest(
                "the checkpoint spool is unusable after an incomplete append",
            ));
        }
        if self.records.last().is_some_and(|last| key <= last.key) {
            return Err(ImportError::InvalidRequest(
                "checkpoint work units must be appended in canonical order",
            ));
        }
        if u64::try_from(self.records.len()).map_err(|_| ImportError::Overflow)?
            >= self.maximum_records
        {
            return Err(ImportError::InvalidRequest(
                "checkpoint work units exceed the import plan's record bound",
            ));
        }
        Ok(true)
    }

    fn append_encoded_bytes(
        &mut self,
        key: SpoolWorkUnitKey,
        pixel: Option<(ShardProfileKind, &[u8])>,
        validity: Option<(ShardProfileKind, &[u8])>,
        packed_index: PackedIndexRecord,
    ) -> Result<bool, ImportError> {
        if self.pending_batch_expired() {
            self.commit_pending()?;
        }
        let encoded_bytes = pixel
            .map_or(0_u64, |chunk| chunk.1.len() as u64)
            .checked_add(validity.map_or(0_u64, |chunk| chunk.1.len() as u64))
            .ok_or(ImportError::Overflow)?;
        if encoded_bytes > DURABILITY_BATCH_PAYLOAD_BYTES_MAX {
            return Err(ImportError::InvalidRequest(
                "one checkpoint work unit exceeds the durability-batch byte bound",
            ));
        }
        if self.should_commit_before(encoded_bytes)? {
            self.commit_pending()?;
        }
        let (pixel_record, after_pixel) = plan_chunk(
            pixel.map(|chunk| chunk.0),
            pixel.map(|chunk| chunk.1),
            self.payload_bytes,
        )?;
        let (validity_record, after_validity) = plan_chunk(
            validity.map(|chunk| chunk.0),
            validity.map(|chunk| chunk.1),
            after_pixel,
        )?;
        let record = JournalRecord {
            key,
            pixel: pixel_record,
            validity: validity_record,
            packed_index: packed_index.encode(),
        };
        let journal_bytes = encode_journal_record(record);

        // Once bytes are appended, any failure poisons this handle. Recovery
        // trusts only the separately synchronized watermark prefix and drops
        // this bounded suffix.
        self.writable = false;
        append_all(
            &mut self.payload,
            pixel
                .map(|chunk| chunk.1)
                .into_iter()
                .chain(validity.map(|chunk| chunk.1)),
        )
        .map_err(|source| self.io("append checkpoint payload", PAYLOAD_FILE, source))?;
        self.inject_failure(SpoolFailpoint::AfterPayloadAppend)
            .map_err(|source| self.io("append checkpoint payload", PAYLOAD_FILE, source))?;

        self.journal
            .write_all(&journal_bytes)
            .map_err(|source| self.io("append checkpoint journal", JOURNAL_FILE, source))?;
        self.inject_failure(SpoolFailpoint::AfterJournalAppend)
            .map_err(|source| self.io("append checkpoint journal", JOURNAL_FILE, source))?;

        self.records.push(record);
        self.payload_bytes = after_validity;
        self.pending_started.get_or_insert_with(Instant::now);
        self.writable = true;
        if self.should_commit_after()? {
            self.commit_pending()?;
        }
        Ok(true)
    }

    /// Synchronizes the current batch and publishes its prefix watermark.
    /// A failure poisons this handle because durability is then indeterminate;
    /// reopening resolves it from the last complete valid watermark.
    pub(crate) fn commit_pending(&mut self) -> Result<(), ImportError> {
        let record_count = u64::try_from(self.records.len()).map_err(|_| ImportError::Overflow)?;
        if record_count == self.durable_records {
            return Ok(());
        }
        if !self.writable {
            return Err(ImportError::InvalidRequest(
                "the checkpoint spool is unusable after an incomplete append",
            ));
        }

        self.writable = false;
        self.inject_failure(SpoolFailpoint::BeforePayloadSync)
            .map_err(|source| {
                self.durability_error("synchronize checkpoint payload", PAYLOAD_FILE, source)
            })?;
        timed_sync_file(&self.payload, &mut self.sync_metrics).map_err(|source| {
            self.durability_error("synchronize checkpoint payload", PAYLOAD_FILE, source)
        })?;
        self.inject_failure(SpoolFailpoint::AfterPayloadSync)
            .map_err(|source| {
                self.durability_error("synchronize checkpoint payload", PAYLOAD_FILE, source)
            })?;

        self.inject_failure(SpoolFailpoint::BeforeJournalSync)
            .map_err(|source| {
                self.durability_error("synchronize checkpoint journal", JOURNAL_FILE, source)
            })?;
        timed_sync_file(&self.journal, &mut self.sync_metrics).map_err(|source| {
            self.durability_error("synchronize checkpoint journal", JOURNAL_FILE, source)
        })?;
        self.inject_failure(SpoolFailpoint::AfterJournalSync)
            .map_err(|source| {
                self.durability_error("synchronize checkpoint journal", JOURNAL_FILE, source)
            })?;

        let prefix = DurablePrefix {
            sequence: self
                .committed_batches
                .checked_add(1)
                .ok_or(ImportError::Overflow)?,
            record_count,
            journal_bytes: record_count
                .checked_mul(JOURNAL_RECORD_BYTES as u64)
                .ok_or(ImportError::Overflow)?,
            payload_bytes: self.payload_bytes,
            previous_digest: self.last_watermark_digest,
        };
        let watermark_bytes = encode_watermark_record(prefix);
        self.watermark
            .write_all(&watermark_bytes)
            .map_err(|source| self.io("append checkpoint watermark", WATERMARK_FILE, source))?;
        self.inject_failure(SpoolFailpoint::AfterWatermarkAppend)
            .map_err(|source| self.io("append checkpoint watermark", WATERMARK_FILE, source))?;
        self.inject_failure(SpoolFailpoint::BeforeWatermarkSync)
            .map_err(|source| {
                self.durability_error("synchronize checkpoint watermark", WATERMARK_FILE, source)
            })?;
        timed_sync_file(&self.watermark, &mut self.sync_metrics).map_err(|source| {
            self.durability_error("synchronize checkpoint watermark", WATERMARK_FILE, source)
        })?;
        self.inject_failure(SpoolFailpoint::AfterWatermarkSync)
            .map_err(|source| {
                self.durability_error("synchronize checkpoint watermark", WATERMARK_FILE, source)
            })?;

        self.durable_records = record_count;
        self.durable_payload_bytes = self.payload_bytes;
        self.committed_batches = prefix.sequence;
        self.last_watermark_digest
            .copy_from_slice(&watermark_bytes[WATERMARK_BODY_BYTES..WATERMARK_RECORD_BYTES]);
        self.pending_started = None;
        self.writable = true;
        Ok(())
    }

    /// Looks up and checksum-verifies one completed work unit.
    #[cfg(test)]
    pub(crate) fn read_work_unit(
        &mut self,
        key: SpoolWorkUnitKey,
    ) -> Result<Option<SpoolWorkUnit>, ImportError> {
        let index = match self.find(key) {
            Ok(index) => index,
            Err(_) => return Ok(None),
        };
        let record = self.records[index];
        let pixel = read_chunk(
            &self.directory_path,
            &mut self.payload,
            record.pixel,
            &mut self.codec_metrics,
        )?;
        let validity = read_chunk(
            &self.directory_path,
            &mut self.payload,
            record.validity,
            &mut self.codec_metrics,
        )?;
        Ok(Some(SpoolWorkUnit {
            key,
            pixel,
            validity,
            packed_index: record.packed_index,
        }))
    }

    pub(crate) fn payload_reader(&self) -> Result<SpoolPayloadReader, ImportError> {
        Ok(SpoolPayloadReader {
            payload_path: self.directory_path.join(PAYLOAD_FILE),
            payload: self
                .payload
                .try_clone()
                .map_err(|source| self.io("clone checkpoint payload", PAYLOAD_FILE, source))?,
            snapshot_bytes: self.payload_bytes,
        })
    }

    pub(crate) fn work_unit_descriptor(
        &self,
        key: SpoolWorkUnitKey,
    ) -> Option<SpoolWorkUnitDescriptor> {
        self.find(key).ok().map(|index| {
            let record = self.records[index];
            SpoolWorkUnitDescriptor {
                key,
                pixel: record.pixel.map(SpoolChunkDescriptor::from),
                validity: record.validity.map(SpoolChunkDescriptor::from),
                packed_index: record.packed_index,
            }
        })
    }

    pub(crate) fn record_worker_codec_decodes(
        &mut self,
        calls: u64,
        elapsed_ns: u64,
    ) -> Result<(), ImportError> {
        self.codec_metrics.decode_calls = self
            .codec_metrics
            .decode_calls
            .checked_add(calls)
            .ok_or(ImportError::Overflow)?;
        self.codec_metrics.decode_elapsed_ns = self
            .codec_metrics
            .decode_elapsed_ns
            .checked_add(elapsed_ns)
            .ok_or(ImportError::Overflow)?;
        Ok(())
    }

    /// Reads one already-encoded component through the storage crate's typed
    /// checksum/frame boundary. Publication can then assemble the outer shard
    /// without decoding and encoding the same inner payload again.
    pub(crate) fn read_encoded_component(
        &mut self,
        key: SpoolWorkUnitKey,
        validity: bool,
    ) -> Result<Option<CanonicalEncodedInner>, ImportError> {
        let index = match self.find(key) {
            Ok(index) => index,
            Err(_) => return Ok(None),
        };
        let record = if validity {
            self.records[index].validity
        } else {
            self.records[index].pixel
        };
        let Some(record) = record else {
            return Ok(None);
        };
        let encoded = read_encoded(&self.directory_path, &mut self.payload, record)?;
        let validated = timed_codec_decode(&mut self.codec_metrics, || {
            CanonicalEncodedInner::validate(record.kind, encoded)
        })?;
        Ok(Some(validated))
    }

    pub(crate) fn read_packed_index(
        &self,
        key: SpoolWorkUnitKey,
    ) -> Option<[u8; PACKED_INDEX_BYTES]> {
        self.find(key)
            .ok()
            .map(|index| self.records[index].packed_index)
    }

    fn find(&self, key: SpoolWorkUnitKey) -> Result<usize, usize> {
        self.records.binary_search_by_key(&key, |record| record.key)
    }

    fn should_commit_before(&self, next_payload_bytes: u64) -> Result<bool, ImportError> {
        let pending_work_units = u64::try_from(self.records.len())
            .map_err(|_| ImportError::Overflow)?
            .checked_sub(self.durable_records)
            .ok_or(ImportError::Overflow)?;
        if pending_work_units == 0 {
            return Ok(false);
        }
        let pending_payload_bytes = self
            .payload_bytes
            .checked_sub(self.durable_payload_bytes)
            .ok_or(ImportError::Overflow)?;
        let next_work_units = pending_work_units
            .checked_add(1)
            .ok_or(ImportError::Overflow)?;
        let next_batch_bytes = pending_payload_bytes
            .checked_add(next_payload_bytes)
            .ok_or(ImportError::Overflow)?;
        Ok(next_work_units > self.batch_policy.work_units_max
            || next_batch_bytes > self.batch_policy.payload_bytes_max
            || self.pending_batch_expired())
    }

    fn should_commit_after(&self) -> Result<bool, ImportError> {
        let pending_work_units = u64::try_from(self.records.len())
            .map_err(|_| ImportError::Overflow)?
            .checked_sub(self.durable_records)
            .ok_or(ImportError::Overflow)?;
        let pending_payload_bytes = self
            .payload_bytes
            .checked_sub(self.durable_payload_bytes)
            .ok_or(ImportError::Overflow)?;
        Ok(pending_work_units >= self.batch_policy.work_units_max
            || pending_payload_bytes >= self.batch_policy.payload_bytes_max
            || self.pending_batch_expired())
    }

    fn pending_batch_expired(&self) -> bool {
        self.pending_started
            .is_some_and(|started| started.elapsed() >= self.batch_policy.age_max)
    }

    fn io(&self, operation: &'static str, file_name: &str, source: io::Error) -> ImportError {
        ImportError::Io {
            operation,
            path: self.directory_path.join(file_name),
            source,
        }
    }

    fn durability_error(
        &self,
        operation: &'static str,
        file_name: &str,
        source: io::Error,
    ) -> ImportError {
        ImportError::CheckpointDurabilityIndeterminate {
            operation,
            path: self.directory_path.join(file_name),
            source,
        }
    }

    fn inject_failure(&mut self, point: SpoolFailpoint) -> Result<(), io::Error> {
        #[cfg(test)]
        if self.failpoint == Some(point) {
            self.failpoint = None;
            return Err(io::Error::other(format!(
                "injected checkpoint failure at {point:?}"
            )));
        }
        #[cfg(not(test))]
        let _ = point;
        Ok(())
    }

    #[cfg(test)]
    fn set_failpoint(&mut self, point: SpoolFailpoint) {
        self.failpoint = Some(point);
    }
}

impl From<ChunkRecord> for SpoolChunkDescriptor {
    fn from(record: ChunkRecord) -> Self {
        Self {
            kind: record.kind,
            offset: record.offset,
            encoded_bytes: record.encoded_bytes,
        }
    }
}

impl SpoolWorkUnitDescriptor {
    /// Strictly decodes the immutable packed-index facts associated with this
    /// descriptor. Workers can inspect the all-valid/all-invalid flags before
    /// deciding whether a validity payload decode is necessary.
    pub(crate) fn packed_index_record(
        self,
        dtype: IntensityDType,
        logical_brick_capacity: u64,
    ) -> Result<PackedIndexRecord, ImportError> {
        let record = PackedIndexRecord::decode(&self.packed_index, dtype, logical_brick_capacity)
            .map_err(|error| {
            ImportError::InvalidCheckpoint(format!("a packed-index record is invalid: {error}"))
        })?;
        if record.coordinates() != self.key.coordinates() {
            return Err(ImportError::InvalidCheckpoint(
                "a packed-index record does not match its spool work-unit key".to_owned(),
            ));
        }
        if record.pixel_payload_present() != self.pixel.is_some() {
            return Err(ImportError::InvalidCheckpoint(
                "a packed-index pixel-presence fact disagrees with its spool descriptor".to_owned(),
            ));
        }
        let validity_payload_required = record.explicit_validity() && !record.all_voxels_invalid();
        if validity_payload_required != self.validity.is_some() {
            return Err(ImportError::InvalidCheckpoint(
                "packed-index validity facts disagree with the spool descriptor".to_owned(),
            ));
        }
        Ok(record)
    }
}

impl SpoolPayloadReader {
    /// Reads and checksum-validates only the pixel component named by one
    /// immutable descriptor. The paired validity component is never touched.
    pub(crate) fn read_pixel_component(
        &self,
        descriptor: SpoolWorkUnitDescriptor,
    ) -> Result<SpoolDecodedComponent, ImportError> {
        self.read_component(descriptor.pixel)
    }

    /// Reads and checksum-validates only the validity component named by one
    /// immutable descriptor. The paired pixel component is never touched.
    pub(crate) fn read_validity_component(
        &self,
        descriptor: SpoolWorkUnitDescriptor,
    ) -> Result<SpoolDecodedComponent, ImportError> {
        self.read_component(descriptor.validity)
    }

    fn read_component(
        &self,
        descriptor: Option<SpoolChunkDescriptor>,
    ) -> Result<SpoolDecodedComponent, ImportError> {
        let Some(descriptor) = descriptor else {
            return Ok(SpoolDecodedComponent {
                chunk: None,
                codec_decode_calls: 0,
                codec_decode_time_ns: 0,
            });
        };
        let end = descriptor
            .offset
            .checked_add(descriptor.encoded_bytes)
            .ok_or(ImportError::Overflow)?;
        if end > self.snapshot_bytes {
            return Err(ImportError::InvalidCheckpoint(
                "a worker payload descriptor exceeds its immutable snapshot".to_owned(),
            ));
        }
        let length = usize::try_from(descriptor.encoded_bytes).map_err(|_| {
            ImportError::InvalidCheckpoint("a payload range is too large".to_owned())
        })?;
        let mut encoded = vec![0_u8; length];
        read_exact_at(
            &self.payload,
            &mut encoded,
            descriptor.offset,
            &self.payload_path,
        )?;
        let started = Instant::now();
        let decoded = decode_inner_payload(descriptor.kind, &encoded).map_err(|error| {
            ImportError::InvalidCheckpoint(format!("an encoded chunk is invalid: {error}"))
        })?;
        Ok(SpoolDecodedComponent {
            chunk: Some(SpoolChunk {
                kind: descriptor.kind,
                decoded,
            }),
            codec_decode_calls: 1,
            codec_decode_time_ns: elapsed_ns(started.elapsed()),
        })
    }
}

fn read_exact_at(
    file: &File,
    mut destination: &mut [u8],
    mut offset: u64,
    path: &Path,
) -> Result<(), ImportError> {
    while !destination.is_empty() {
        let read = file
            .read_at(destination, offset)
            .map_err(|source| ImportError::Io {
                operation: "read checkpoint payload",
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            return Err(ImportError::InvalidCheckpoint(
                "checkpoint payload is truncated".to_owned(),
            ));
        }
        offset = offset
            .checked_add(u64::try_from(read).map_err(|_| ImportError::Overflow)?)
            .ok_or(ImportError::Overflow)?;
        destination = &mut destination[read..];
    }
    Ok(())
}

pub(crate) fn record_memory_bytes(maximum_records: u64) -> Result<u64, ImportError> {
    maximum_records
        .checked_mul(
            u64::try_from(std::mem::size_of::<JournalRecord>())
                .map_err(|_| ImportError::Overflow)?,
        )
        .and_then(|value| {
            value.checked_add(u64::try_from(std::mem::size_of::<Vec<JournalRecord>>()).ok()?)
        })
        .ok_or(ImportError::Overflow)
}

fn create_files(
    directory: &OwnedFd,
    directory_path: &Path,
    binding: SpoolBinding,
    sync_metrics: &mut SyncMetrics,
) -> Result<(File, File, File, File), ImportError> {
    let mut header = create_file(
        directory,
        directory_path,
        HEADER_FILE,
        "create spool header",
    )?;
    header
        .write_all(&header_bytes(binding))
        .map_err(|source| ImportError::Io {
            operation: "write spool header",
            path: directory_path.join(HEADER_FILE),
            source,
        })?;
    timed_sync_file(&header, sync_metrics).map_err(|source| {
        ImportError::CheckpointDurabilityIndeterminate {
            operation: "synchronize spool header",
            path: directory_path.join(HEADER_FILE),
            source,
        }
    })?;

    let journal = create_file(
        directory,
        directory_path,
        JOURNAL_FILE,
        "create spool journal",
    )?;
    timed_sync_file(&journal, sync_metrics).map_err(|source| {
        ImportError::CheckpointDurabilityIndeterminate {
            operation: "synchronize spool journal",
            path: directory_path.join(JOURNAL_FILE),
            source,
        }
    })?;
    let payload = create_file(
        directory,
        directory_path,
        PAYLOAD_FILE,
        "create spool payload",
    )?;
    timed_sync_file(&payload, sync_metrics).map_err(|source| {
        ImportError::CheckpointDurabilityIndeterminate {
            operation: "synchronize spool payload",
            path: directory_path.join(PAYLOAD_FILE),
            source,
        }
    })?;
    let watermark = create_file(
        directory,
        directory_path,
        WATERMARK_FILE,
        "create spool watermark",
    )?;
    timed_sync_file(&watermark, sync_metrics).map_err(|source| {
        ImportError::CheckpointDurabilityIndeterminate {
            operation: "synchronize spool watermark",
            path: directory_path.join(WATERMARK_FILE),
            source,
        }
    })?;
    timed_sync_directory(directory, sync_metrics).map_err(|source| {
        ImportError::CheckpointDurabilityIndeterminate {
            operation: "synchronize checkpoint directory",
            path: directory_path.to_path_buf(),
            source,
        }
    })?;
    Ok((header, journal, payload, watermark))
}

fn validate_header(
    directory: &OwnedFd,
    directory_path: &Path,
    binding: SpoolBinding,
) -> Result<File, ImportError> {
    let mut file = open_file(
        directory,
        directory_path,
        HEADER_FILE,
        FILE_READ_FLAGS,
        "open spool header",
    )?;
    let metadata = file.metadata().map_err(|source| ImportError::Io {
        operation: "inspect spool header",
        path: directory_path.join(HEADER_FILE),
        source,
    })?;
    if metadata.len() != HEADER_BYTES as u64 {
        return invalid_checkpoint("the spool header has a noncanonical length");
    }
    let mut actual = [0_u8; HEADER_BYTES];
    file.read_exact(&mut actual)
        .map_err(|source| ImportError::Io {
            operation: "read spool header",
            path: directory_path.join(HEADER_FILE),
            source,
        })?;
    if actual != header_bytes(binding) {
        return invalid_checkpoint("the spool header does not match this plan and source");
    }
    Ok(file)
}

fn header_bytes(binding: SpoolBinding) -> [u8; HEADER_BYTES] {
    let mut bytes = [0_u8; HEADER_BYTES];
    let schema_end = SPOOL_SCHEMA.len();
    let plan_end = schema_end + 32;
    bytes[..schema_end].copy_from_slice(SPOOL_SCHEMA);
    bytes[schema_end..plan_end].copy_from_slice(binding.plan_digest.as_bytes());
    bytes[plan_end..].copy_from_slice(binding.source_fingerprint.as_bytes());
    bytes
}

fn recover_durable_prefix(
    directory_path: &Path,
    journal: &File,
    payload: &File,
    watermark: &File,
    maximum_records: u64,
    is_cancelled: &mut impl FnMut() -> bool,
    metrics: &mut RecoveryMetrics<'_>,
) -> Result<RecoveredSpool, ImportError> {
    let journal_bytes = regular_file_length(journal, "journal")?;
    let payload_bytes = regular_file_length(payload, "payload")?;
    let watermark_bytes = regular_file_length(watermark, "watermark")?;
    let (prefix, canonical_watermark_bytes, last_watermark_digest) = validate_watermarks(
        directory_path,
        watermark,
        watermark_bytes,
        maximum_records,
        is_cancelled,
    )?;
    if prefix.journal_bytes > journal_bytes {
        return invalid_checkpoint("the durable watermark exceeds the checkpoint journal");
    }
    if prefix.payload_bytes > payload_bytes {
        return invalid_checkpoint("the durable watermark exceeds the checkpoint payload");
    }

    let journal_suffix_bytes = journal_bytes - prefix.journal_bytes;
    let maximum_journal_suffix = DURABILITY_BATCH_WORK_UNITS_MAX
        .checked_mul(JOURNAL_RECORD_BYTES as u64)
        .ok_or(ImportError::Overflow)?;
    if journal_suffix_bytes > maximum_journal_suffix {
        return invalid_checkpoint("the uncommitted journal suffix exceeds one durability batch");
    }
    let payload_suffix_bytes = payload_bytes - prefix.payload_bytes;
    if payload_suffix_bytes > DURABILITY_BATCH_PAYLOAD_BYTES_MAX {
        return invalid_checkpoint("the uncommitted payload suffix exceeds one durability batch");
    }

    let capacity = usize::try_from(maximum_records)
        .map_err(|_| ImportError::InvalidCheckpoint("the journal is too large".to_owned()))?;
    let mut records = Vec::new();
    records
        .try_reserve_exact(capacity)
        .map_err(|_| ImportError::InvalidCheckpoint("the journal is too large".to_owned()))?;

    let mut journal = journal.try_clone().map_err(|source| ImportError::Io {
        operation: "duplicate checkpoint journal",
        path: directory_path.join(JOURNAL_FILE),
        source,
    })?;
    let mut payload = payload.try_clone().map_err(|source| ImportError::Io {
        operation: "duplicate checkpoint payload",
        path: directory_path.join(PAYLOAD_FILE),
        source,
    })?;
    journal
        .seek(SeekFrom::Start(0))
        .map_err(|source| ImportError::Io {
            operation: "seek checkpoint journal",
            path: directory_path.join(JOURNAL_FILE),
            source,
        })?;

    let mut expected_payload_offset = 0_u64;
    let mut previous_key = None;
    for _ in 0..prefix.record_count {
        if is_cancelled() {
            return Err(ImportError::Cancelled);
        }
        let mut bytes = [0_u8; JOURNAL_RECORD_BYTES];
        journal
            .read_exact(&mut bytes)
            .map_err(|source| ImportError::Io {
                operation: "read checkpoint journal",
                path: directory_path.join(JOURNAL_FILE),
                source,
            })?;
        let (record, next_offset) =
            decode_journal_record(&bytes, expected_payload_offset, prefix.payload_bytes)?;
        if previous_key.is_some_and(|previous| record.key <= previous) {
            return invalid_checkpoint("journal work-unit keys are not unique and ordered");
        }
        verify_chunk(directory_path, &mut payload, record.pixel, metrics.codec)?;
        verify_chunk(directory_path, &mut payload, record.validity, metrics.codec)?;
        expected_payload_offset = next_offset;
        previous_key = Some(record.key);
        records.push(record);
    }
    if is_cancelled() {
        return Err(ImportError::Cancelled);
    }
    if expected_payload_offset != prefix.payload_bytes {
        return invalid_checkpoint("the journal does not end at its durable payload watermark");
    }

    // Journal and payload bytes beyond the separately synchronized watermark
    // are one bounded uncommitted batch. They are never parsed or accepted.
    if prefix.journal_bytes != journal_bytes {
        truncate_and_sync(
            &journal,
            prefix.journal_bytes,
            directory_path,
            JOURNAL_FILE,
            "truncate uncommitted checkpoint journal suffix",
            "synchronize recovered checkpoint journal",
            metrics.sync,
        )?;
    }
    if prefix.payload_bytes != payload_bytes {
        truncate_and_sync(
            &payload,
            prefix.payload_bytes,
            directory_path,
            PAYLOAD_FILE,
            "truncate uncommitted checkpoint payload suffix",
            "synchronize recovered checkpoint payload",
            metrics.sync,
        )?;
    }
    if canonical_watermark_bytes != watermark_bytes {
        truncate_and_sync(
            watermark,
            canonical_watermark_bytes,
            directory_path,
            WATERMARK_FILE,
            "truncate interrupted checkpoint watermark append",
            "synchronize recovered checkpoint watermark",
            metrics.sync,
        )?;
    }
    Ok(RecoveredSpool {
        records,
        payload_bytes: prefix.payload_bytes,
        durable_records: prefix.record_count,
        committed_batches: prefix.sequence,
        last_watermark_digest,
        codec_metrics: *metrics.codec,
    })
}

fn validate_watermarks(
    directory_path: &Path,
    watermark: &File,
    watermark_bytes: u64,
    maximum_records: u64,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<(DurablePrefix, u64, [u8; 32]), ImportError> {
    let record_bytes = WATERMARK_RECORD_BYTES as u64;
    let maximum_bytes = maximum_records
        .checked_mul(record_bytes)
        .and_then(|value| value.checked_add(record_bytes - 1))
        .ok_or(ImportError::Overflow)?;
    if watermark_bytes > maximum_bytes {
        return invalid_checkpoint("the watermark exceeds this import plan's batch bound");
    }
    let complete_records = watermark_bytes / record_bytes;
    let mut canonical_bytes = complete_records
        .checked_mul(record_bytes)
        .ok_or(ImportError::Overflow)?;
    let mut file = watermark.try_clone().map_err(|source| ImportError::Io {
        operation: "duplicate checkpoint watermark",
        path: directory_path.join(WATERMARK_FILE),
        source,
    })?;
    file.seek(SeekFrom::Start(0))
        .map_err(|source| ImportError::Io {
            operation: "seek checkpoint watermark",
            path: directory_path.join(WATERMARK_FILE),
            source,
        })?;

    let mut previous = DurablePrefix {
        sequence: 0,
        record_count: 0,
        journal_bytes: 0,
        payload_bytes: 0,
        previous_digest: [0; 32],
    };
    let mut previous_digest = [0_u8; 32];
    for index in 0..complete_records {
        if is_cancelled() {
            return Err(ImportError::Cancelled);
        }
        let mut bytes = [0_u8; WATERMARK_RECORD_BYTES];
        file.read_exact(&mut bytes)
            .map_err(|source| ImportError::Io {
                operation: "read checkpoint watermark",
                path: directory_path.join(WATERMARK_FILE),
                source,
            })?;
        if !watermark_checksum_matches(&bytes) {
            if index + 1 == complete_records {
                canonical_bytes = index
                    .checked_mul(record_bytes)
                    .ok_or(ImportError::Overflow)?;
                break;
            }
            return invalid_checkpoint("a non-final watermark record has an invalid checksum");
        }
        let current = decode_watermark_record(&bytes)?;
        let expected_sequence = previous
            .sequence
            .checked_add(1)
            .ok_or(ImportError::Overflow)?;
        if current.sequence != expected_sequence || current.previous_digest != previous_digest {
            return invalid_checkpoint("watermark records are not a canonical digest chain");
        }
        let expected_journal_bytes = current
            .record_count
            .checked_mul(JOURNAL_RECORD_BYTES as u64)
            .ok_or(ImportError::Overflow)?;
        if current.journal_bytes != expected_journal_bytes {
            return invalid_checkpoint("a watermark journal length is noncanonical");
        }
        if current.record_count > maximum_records
            || current.record_count <= previous.record_count
            || current.record_count - previous.record_count > DURABILITY_BATCH_WORK_UNITS_MAX
        {
            return invalid_checkpoint("a watermark work-unit prefix is outside its batch bounds");
        }
        if current.payload_bytes < previous.payload_bytes
            || current.payload_bytes - previous.payload_bytes > DURABILITY_BATCH_PAYLOAD_BYTES_MAX
        {
            return invalid_checkpoint("a watermark payload prefix is outside its batch bounds");
        }
        previous = current;
        previous_digest.copy_from_slice(&bytes[WATERMARK_BODY_BYTES..WATERMARK_RECORD_BYTES]);
    }
    if is_cancelled() {
        return Err(ImportError::Cancelled);
    }
    Ok((previous, canonical_bytes, previous_digest))
}

fn encode_watermark_record(prefix: DurablePrefix) -> [u8; WATERMARK_RECORD_BYTES] {
    let mut bytes = [0_u8; WATERMARK_RECORD_BYTES];
    bytes[0..8].copy_from_slice(&prefix.sequence.to_le_bytes());
    bytes[8..16].copy_from_slice(&prefix.record_count.to_le_bytes());
    bytes[16..24].copy_from_slice(&prefix.journal_bytes.to_le_bytes());
    bytes[24..32].copy_from_slice(&prefix.payload_bytes.to_le_bytes());
    bytes[32..64].copy_from_slice(&prefix.previous_digest);
    let digest = Sha256Hasher::digest(&bytes[..WATERMARK_BODY_BYTES]);
    bytes[WATERMARK_BODY_BYTES..].copy_from_slice(digest.as_bytes());
    bytes
}

fn decode_watermark_record(
    bytes: &[u8; WATERMARK_RECORD_BYTES],
) -> Result<DurablePrefix, ImportError> {
    if !watermark_checksum_matches(bytes) {
        return invalid_checkpoint("a watermark-record checksum does not match");
    }
    let mut previous_digest = [0_u8; 32];
    previous_digest.copy_from_slice(&bytes[32..64]);
    Ok(DurablePrefix {
        sequence: read_u64(bytes, 0),
        record_count: read_u64(bytes, 8),
        journal_bytes: read_u64(bytes, 16),
        payload_bytes: read_u64(bytes, 24),
        previous_digest,
    })
}

fn watermark_checksum_matches(bytes: &[u8; WATERMARK_RECORD_BYTES]) -> bool {
    Sha256Hasher::digest(&bytes[..WATERMARK_BODY_BYTES]).as_bytes()
        == &bytes[WATERMARK_BODY_BYTES..]
}

fn encode_journal_record(record: JournalRecord) -> [u8; JOURNAL_RECORD_BYTES] {
    let mut bytes = [0_u8; JOURNAL_RECORD_BYTES];
    encode_key(record.key, &mut bytes[..28]);
    bytes[28] = record.pixel.map_or(0, |chunk| encode_kind(chunk.kind));
    bytes[29] = record.validity.map_or(0, |chunk| encode_kind(chunk.kind));
    bytes[30] = (u8::from(record.pixel.is_some()) * FLAG_PIXEL_PRESENT)
        | (u8::from(record.validity.is_some()) * FLAG_VALIDITY_PRESENT);
    encode_chunk_location(record.pixel, &mut bytes[32..48]);
    encode_chunk_location(record.validity, &mut bytes[48..64]);
    bytes[64..128].copy_from_slice(&record.packed_index);
    let digest = Sha256Hasher::digest(&bytes[..JOURNAL_BODY_BYTES]);
    bytes[JOURNAL_BODY_BYTES..].copy_from_slice(digest.as_bytes());
    bytes
}

fn decode_journal_record(
    bytes: &[u8; JOURNAL_RECORD_BYTES],
    expected_payload_offset: u64,
    payload_bytes: u64,
) -> Result<(JournalRecord, u64), ImportError> {
    let digest = Sha256Hasher::digest(&bytes[..JOURNAL_BODY_BYTES]);
    if digest.as_bytes() != &bytes[JOURNAL_BODY_BYTES..] {
        return invalid_checkpoint("a journal-record checksum does not match");
    }
    let flags = bytes[30];
    if flags & !KNOWN_FLAGS != 0 || bytes[31] != 0 {
        return invalid_checkpoint("a journal record contains noncanonical flag bits");
    }

    let key = decode_key(&bytes[..28]);
    if key != decode_packed_key(&bytes[64..128]) {
        return invalid_checkpoint("a packed-index row does not match its work-unit key");
    }
    let pixel_present = flags & FLAG_PIXEL_PRESENT != 0;
    let validity_present = flags & FLAG_VALIDITY_PRESENT != 0;
    let pixel_kind = decode_optional_kind(bytes[28], pixel_present, Component::Pixel)?;
    let validity_kind = decode_optional_kind(bytes[29], validity_present, Component::Validity)?;
    if pixel_kind
        .zip(validity_kind)
        .is_some_and(|(pixel, validity)| is_2d(pixel) != is_2d(validity))
    {
        return invalid_checkpoint("pixel and validity chunks use different dimensions");
    }

    let (pixel, after_pixel) = decode_chunk_location(
        pixel_kind,
        &bytes[32..48],
        expected_payload_offset,
        payload_bytes,
    )?;
    let (validity, after_validity) =
        decode_chunk_location(validity_kind, &bytes[48..64], after_pixel, payload_bytes)?;
    let mut packed_index = [0_u8; PACKED_INDEX_BYTES];
    packed_index.copy_from_slice(&bytes[64..128]);
    Ok((
        JournalRecord {
            key,
            pixel,
            validity,
            packed_index,
        },
        after_validity,
    ))
}

#[cfg(test)]
fn validate_input(
    key: SpoolWorkUnitKey,
    pixel: Option<SpoolChunkInput<'_>>,
    validity: Option<SpoolChunkInput<'_>>,
    packed_index: PackedIndexRecord,
) -> Result<(), ImportError> {
    validate_input_kinds(
        key,
        pixel.map(|chunk| chunk.kind),
        validity.map(|chunk| chunk.kind),
        packed_index,
    )
}

fn validate_input_kinds(
    key: SpoolWorkUnitKey,
    pixel: Option<ShardProfileKind>,
    validity: Option<ShardProfileKind>,
    packed_index: PackedIndexRecord,
) -> Result<(), ImportError> {
    if key.coordinates() != packed_index.coordinates() {
        return Err(ImportError::InvalidRequest(
            "the packed-index coordinates do not match the spool work-unit key",
        ));
    }
    if pixel.is_some() != packed_index.pixel_payload_present() {
        return Err(ImportError::InvalidRequest(
            "pixel payload presence does not match the packed-index record",
        ));
    }
    if validity.is_some() && !packed_index.explicit_validity() {
        return Err(ImportError::InvalidRequest(
            "a validity payload requires explicit validity in the packed-index record",
        ));
    }
    if pixel.is_some_and(|kind| !is_pixel_kind(kind)) {
        return Err(ImportError::InvalidRequest(
            "a spool pixel chunk must use a pixel storage kind",
        ));
    }
    if validity.is_some_and(|kind| !is_validity_kind(kind)) {
        return Err(ImportError::InvalidRequest(
            "a spool validity chunk must use a validity storage kind",
        ));
    }
    if pixel
        .zip(validity)
        .is_some_and(|(pixel, validity)| is_2d(pixel) != is_2d(validity))
    {
        return Err(ImportError::InvalidRequest(
            "pixel and validity chunks must use the same dimensionality",
        ));
    }
    Ok(())
}

fn plan_chunk(
    kind: Option<ShardProfileKind>,
    encoded: Option<&[u8]>,
    offset: u64,
) -> Result<(Option<ChunkRecord>, u64), ImportError> {
    let Some((kind, encoded)) = kind.zip(encoded) else {
        return Ok((None, offset));
    };
    let encoded_bytes = u64::try_from(encoded.len()).map_err(|_| ImportError::Overflow)?;
    let next = offset
        .checked_add(encoded_bytes)
        .ok_or(ImportError::Overflow)?;
    Ok((
        Some(ChunkRecord {
            kind,
            offset,
            encoded_bytes,
        }),
        next,
    ))
}

fn decode_chunk_location(
    kind: Option<ShardProfileKind>,
    bytes: &[u8],
    expected_offset: u64,
    payload_bytes: u64,
) -> Result<(Option<ChunkRecord>, u64), ImportError> {
    let offset = read_u64(bytes, 0);
    let encoded_bytes = read_u64(bytes, 8);
    let Some(kind) = kind else {
        if offset != MISSING_OFFSET || encoded_bytes != 0 {
            return invalid_checkpoint("an absent chunk has a noncanonical payload range");
        }
        return Ok((None, expected_offset));
    };
    if offset != expected_offset || encoded_bytes == 0 {
        return invalid_checkpoint("a present chunk has a noncanonical payload range");
    }
    let maximum =
        u64::try_from(kind.encoded_inner_bytes_max()).map_err(|_| ImportError::Overflow)?;
    if encoded_bytes > maximum {
        return invalid_checkpoint("an encoded chunk exceeds its storage-profile bound");
    }
    let end = offset
        .checked_add(encoded_bytes)
        .ok_or_else(|| ImportError::InvalidCheckpoint("a payload range overflows".to_owned()))?;
    if end > payload_bytes {
        return invalid_checkpoint("a journal payload range is out of bounds");
    }
    Ok((
        Some(ChunkRecord {
            kind,
            offset,
            encoded_bytes,
        }),
        end,
    ))
}

fn encode_chunk_location(record: Option<ChunkRecord>, bytes: &mut [u8]) {
    let (offset, encoded_bytes) = record
        .map(|record| (record.offset, record.encoded_bytes))
        .unwrap_or((MISSING_OFFSET, 0));
    bytes[..8].copy_from_slice(&offset.to_le_bytes());
    bytes[8..16].copy_from_slice(&encoded_bytes.to_le_bytes());
}

fn verify_chunk(
    directory_path: &Path,
    payload: &mut File,
    record: Option<ChunkRecord>,
    codec_metrics: &mut CodecMetrics,
) -> Result<(), ImportError> {
    let Some(record) = record else {
        return Ok(());
    };
    let encoded = read_encoded(directory_path, payload, record)?;
    timed_codec_decode(codec_metrics, || {
        decode_inner_payload(record.kind, &encoded)
    })
    .map_err(|error| {
        ImportError::InvalidCheckpoint(format!("an encoded chunk is invalid: {error}"))
    })?;
    Ok(())
}

#[cfg(test)]
fn read_chunk(
    directory_path: &Path,
    payload: &mut File,
    record: Option<ChunkRecord>,
    codec_metrics: &mut CodecMetrics,
) -> Result<Option<SpoolChunk>, ImportError> {
    let Some(record) = record else {
        return Ok(None);
    };
    let encoded = read_encoded(directory_path, payload, record)?;
    let decoded = timed_codec_decode(codec_metrics, || {
        decode_inner_payload(record.kind, &encoded)
    })
    .map_err(|error| {
        ImportError::InvalidCheckpoint(format!("an encoded chunk is invalid: {error}"))
    })?;
    Ok(Some(SpoolChunk {
        kind: record.kind,
        decoded,
    }))
}

fn read_encoded(
    directory_path: &Path,
    payload: &mut File,
    record: ChunkRecord,
) -> Result<Vec<u8>, ImportError> {
    let length = usize::try_from(record.encoded_bytes)
        .map_err(|_| ImportError::InvalidCheckpoint("a payload range is too large".to_owned()))?;
    let mut encoded = vec![0_u8; length];
    payload
        .seek(SeekFrom::Start(record.offset))
        .and_then(|_| payload.read_exact(&mut encoded))
        .map_err(|source| ImportError::Io {
            operation: "read checkpoint payload",
            path: directory_path.join(PAYLOAD_FILE),
            source,
        })?;
    Ok(encoded)
}

fn append_all<'a>(
    file: &mut File,
    chunks: impl Iterator<Item = &'a [u8]>,
) -> Result<(), io::Error> {
    for chunk in chunks {
        file.write_all(chunk)?;
    }
    Ok(())
}

#[cfg(test)]
fn timed_codec_encode<T, E>(
    metrics: &mut CodecMetrics,
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let started = Instant::now();
    let result = operation();
    metrics.encode_calls = metrics.encode_calls.saturating_add(1);
    metrics.encode_elapsed_ns = metrics
        .encode_elapsed_ns
        .saturating_add(elapsed_ns(started.elapsed()));
    result
}

fn timed_codec_decode<T, E>(
    metrics: &mut CodecMetrics,
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let started = Instant::now();
    let result = operation();
    metrics.decode_calls = metrics.decode_calls.saturating_add(1);
    metrics.decode_elapsed_ns = metrics
        .decode_elapsed_ns
        .saturating_add(elapsed_ns(started.elapsed()));
    result
}

fn timed_sync_file(file: &File, metrics: &mut SyncMetrics) -> Result<(), io::Error> {
    let started = Instant::now();
    let result = file.sync_all();
    record_sync(metrics, started.elapsed());
    result
}

fn timed_sync_directory(directory: &OwnedFd, metrics: &mut SyncMetrics) -> Result<(), io::Error> {
    let started = Instant::now();
    let result = fsync(directory).map_err(io::Error::from);
    record_sync(metrics, started.elapsed());
    result
}

fn record_sync(metrics: &mut SyncMetrics, elapsed: Duration) {
    metrics.calls = metrics.calls.saturating_add(1);
    metrics.elapsed_ns = metrics.elapsed_ns.saturating_add(elapsed_ns(elapsed));
}

fn elapsed_ns(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
}

#[allow(clippy::too_many_arguments)]
fn truncate_and_sync(
    file: &File,
    length: u64,
    directory_path: &Path,
    file_name: &str,
    truncate_operation: &'static str,
    sync_operation: &'static str,
    sync_metrics: &mut SyncMetrics,
) -> Result<(), ImportError> {
    file.set_len(length).map_err(|source| ImportError::Io {
        operation: truncate_operation,
        path: directory_path.join(file_name),
        source,
    })?;
    timed_sync_file(file, sync_metrics).map_err(|source| {
        ImportError::CheckpointDurabilityIndeterminate {
            operation: sync_operation,
            path: directory_path.join(file_name),
            source,
        }
    })
}

fn encode_key(key: SpoolWorkUnitKey, bytes: &mut [u8]) {
    for (index, value) in [
        key.image_ordinal,
        key.scale,
        key.t,
        key.c,
        key.z_chunk,
        key.y_chunk,
        key.x_chunk,
    ]
    .into_iter()
    .enumerate()
    {
        let start = index * 4;
        bytes[start..start + 4].copy_from_slice(&value.to_le_bytes());
    }
}

fn decode_key(bytes: &[u8]) -> SpoolWorkUnitKey {
    SpoolWorkUnitKey::new(
        read_u32(bytes, 0),
        read_u32(bytes, 4),
        read_u32(bytes, 8),
        read_u32(bytes, 12),
        read_u32(bytes, 16),
        read_u32(bytes, 20),
        read_u32(bytes, 24),
    )
}

fn decode_packed_key(bytes: &[u8]) -> SpoolWorkUnitKey {
    SpoolWorkUnitKey::new(
        read_u32(bytes, 4),
        read_u32(bytes, 8),
        read_u32(bytes, 12),
        read_u32(bytes, 16),
        read_u32(bytes, 20),
        read_u32(bytes, 24),
        read_u32(bytes, 28),
    )
}

fn encode_kind(kind: ShardProfileKind) -> u8 {
    match kind {
        ShardProfileKind::Pixel3dUint8 => 1,
        ShardProfileKind::Pixel3dUint16 => 2,
        ShardProfileKind::Pixel3dFloat32 => 3,
        ShardProfileKind::Pixel2dUint8 => 4,
        ShardProfileKind::Pixel2dUint16 => 5,
        ShardProfileKind::Pixel2dFloat32 => 6,
        ShardProfileKind::Validity3d => 7,
        ShardProfileKind::Validity2d => 8,
        ShardProfileKind::PackedIndex => 9,
    }
}

fn decode_optional_kind(
    encoded: u8,
    present: bool,
    component: Component,
) -> Result<Option<ShardProfileKind>, ImportError> {
    if !present {
        if encoded != 0 {
            return invalid_checkpoint("an absent chunk has a noncanonical storage kind");
        }
        return Ok(None);
    }
    let kind = match encoded {
        1 => ShardProfileKind::Pixel3dUint8,
        2 => ShardProfileKind::Pixel3dUint16,
        3 => ShardProfileKind::Pixel3dFloat32,
        4 => ShardProfileKind::Pixel2dUint8,
        5 => ShardProfileKind::Pixel2dUint16,
        6 => ShardProfileKind::Pixel2dFloat32,
        7 => ShardProfileKind::Validity3d,
        8 => ShardProfileKind::Validity2d,
        _ => return invalid_checkpoint("a journal record has an unknown storage kind"),
    };
    if component == Component::Pixel && !is_pixel_kind(kind)
        || component == Component::Validity && !is_validity_kind(kind)
    {
        return invalid_checkpoint("a journal chunk has the wrong component storage kind");
    }
    Ok(Some(kind))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Component {
    Pixel,
    Validity,
}

fn is_pixel_kind(kind: ShardProfileKind) -> bool {
    matches!(
        kind,
        ShardProfileKind::Pixel3dUint8
            | ShardProfileKind::Pixel3dUint16
            | ShardProfileKind::Pixel3dFloat32
            | ShardProfileKind::Pixel2dUint8
            | ShardProfileKind::Pixel2dUint16
            | ShardProfileKind::Pixel2dFloat32
    )
}

fn is_validity_kind(kind: ShardProfileKind) -> bool {
    matches!(
        kind,
        ShardProfileKind::Validity3d | ShardProfileKind::Validity2d
    )
}

fn is_2d(kind: ShardProfileKind) -> bool {
    matches!(
        kind,
        ShardProfileKind::Pixel2dUint8
            | ShardProfileKind::Pixel2dUint16
            | ShardProfileKind::Pixel2dFloat32
            | ShardProfileKind::Validity2d
    )
}

fn regular_file_length(file: &File, role: &str) -> Result<u64, ImportError> {
    let stat = fstat(file).map_err(|source| {
        ImportError::InvalidCheckpoint(format!("cannot inspect the checkpoint {role}: {source}"))
    })?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
        return invalid_checkpoint("spool entries must be singly linked regular files");
    }
    u64::try_from(stat.st_size).map_err(|_| {
        ImportError::InvalidCheckpoint(format!("the checkpoint {role} has a negative length"))
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryState {
    Absent,
    RegularFile,
}

fn entry_state(directory: &OwnedFd, name: &str) -> Result<EntryState, ImportError> {
    match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat)
            if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
                && stat.st_nlink == 1 =>
        {
            Ok(EntryState::RegularFile)
        }
        Ok(_) => invalid_checkpoint("spool entries must be singly linked regular files"),
        Err(Errno::NOENT) => Ok(EntryState::Absent),
        Err(error) => Err(ImportError::Io {
            operation: "inspect checkpoint entry",
            path: PathBuf::from(name),
            source: io::Error::from(error),
        }),
    }
}

fn create_file(
    directory: &OwnedFd,
    directory_path: &Path,
    name: &str,
    operation: &'static str,
) -> Result<File, ImportError> {
    open_file(
        directory,
        directory_path,
        name,
        FILE_CREATE_FLAGS,
        operation,
    )
}

fn open_file(
    directory: &OwnedFd,
    directory_path: &Path,
    name: &str,
    flags: OFlags,
    operation: &'static str,
) -> Result<File, ImportError> {
    let descriptor = openat(directory, OsStr::new(name), flags, Mode::RUSR | Mode::WUSR)
        .map_err(|source| io_error(operation, &directory_path.join(name), source))?;
    let stat = fstat(&descriptor).map_err(|source| io_error(operation, directory_path, source))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
        return invalid_checkpoint("spool entries must be singly linked regular files");
    }
    Ok(File::from(descriptor))
}

fn unlink_if_owned(directory: &OwnedFd, name: &str, file: &File) -> Result<(), ImportError> {
    let held = fstat(file)
        .map_err(|source| io_error("inspect owned checkpoint file", Path::new(name), source))?;
    let named = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|source| io_error("inspect named checkpoint file", Path::new(name), source))?;
    if held.st_dev != named.st_dev
        || held.st_ino != named.st_ino
        || FileType::from_raw_mode(named.st_mode) != FileType::RegularFile
        || named.st_nlink != 1
    {
        return invalid_checkpoint("checkpoint name no longer identifies its owned spool file");
    }
    unlinkat(directory, name, AtFlags::empty())
        .map_err(|source| io_error("remove owned checkpoint file", Path::new(name), source))
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("eight bytes"))
}

fn io_error(operation: &'static str, path: &Path, source: Errno) -> ImportError {
    ImportError::Io {
        operation,
        path: path.to_path_buf(),
        source: io::Error::from(source),
    }
}

fn invalid_checkpoint<T>(message: impl Into<String>) -> Result<T, ImportError> {
    Err(ImportError::InvalidCheckpoint(message.into()))
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        io::{Read, Seek, SeekFrom, Write},
    };

    use mirante4d_domain::IntensityDType;
    use mirante4d_storage::PackedIndexStatistics;
    use tempfile::TempDir;

    use super::*;

    fn binding(seed: u8) -> SpoolBinding {
        SpoolBinding::new(
            Sha256Digest::from_bytes([seed; 32]),
            Sha256Digest::from_bytes([seed.wrapping_add(1); 32]),
        )
    }

    fn checkpoint() -> (TempDir, PathBuf) {
        let temporary = TempDir::new().unwrap();
        let checkpoint = temporary.path().join("checkpoint");
        fs::create_dir(&checkpoint).unwrap();
        (temporary, checkpoint)
    }

    fn open(checkpoint: &Path, binding: SpoolBinding) -> Result<ImportSpool, ImportError> {
        ImportSpool::open_or_create(checkpoint, binding, 16, || false)
    }

    fn open_with_policy(
        checkpoint: &Path,
        binding: SpoolBinding,
        maximum_records: u64,
        batch_policy: DurabilityBatchPolicy,
    ) -> Result<ImportSpool, ImportError> {
        ImportSpool::open_or_create_with_policy(
            checkpoint,
            binding,
            maximum_records,
            batch_policy,
            || false,
        )
    }

    fn packed(
        key: SpoolWorkUnitKey,
        valid: u64,
        nonfill: u64,
        pixel_present: bool,
        explicit_validity: bool,
    ) -> PackedIndexRecord {
        PackedIndexRecord::new(
            key.coordinates(),
            PackedIndexStatistics::new(
                valid,
                nonfill,
                (valid != 0).then_some((0, u64::from(nonfill != 0))),
            ),
            pixel_present,
            explicit_validity,
            IntensityDType::Uint8,
            6,
        )
        .unwrap()
    }

    fn decoded(kind: ShardProfileKind, fill: u8) -> Vec<u8> {
        vec![fill; kind.decoded_inner_bytes()]
    }

    fn key(x: u32) -> SpoolWorkUnitKey {
        SpoolWorkUnitKey::new(0, 0, 0, 0, 0, 0, x)
    }

    fn append_elided(spool: &mut ImportSpool, key: SpoolWorkUnitKey) {
        spool
            .append_if_absent(key, None, None, packed(key, 6, 0, false, false))
            .unwrap();
    }

    fn append_pixel(
        spool: &mut ImportSpool,
        key: SpoolWorkUnitKey,
        kind: ShardProfileKind,
        bytes: &[u8],
    ) {
        spool
            .append_if_absent(
                key,
                Some(SpoolChunkInput::new(kind, bytes)),
                None,
                packed(key, 6, 1, true, false),
            )
            .unwrap();
    }

    #[test]
    fn appends_validated_worker_encoding_without_a_second_codec_call() {
        let (_temporary, checkpoint) = checkpoint();
        let mut spool = open(&checkpoint, binding(2)).unwrap();
        let key = key(0);
        let kind = ShardProfileKind::Pixel2dUint8;
        let decoded = decoded(kind, 11);
        let canonical = CanonicalEncodedInner::encode(kind, &decoded).unwrap();

        assert!(
            spool
                .append_encoded_if_absent(
                    key,
                    Some(SpoolEncodedChunkInput::new(&canonical)),
                    None,
                    packed(key, 6, 1, true, false),
                    1,
                    42,
                )
                .unwrap()
        );
        let diagnostics = spool.diagnostics();
        assert_eq!(diagnostics.codec_encode_calls, 1);
        assert_eq!(diagnostics.codec_encode_time_ns, 42);
        let persisted = spool.read_encoded_component(key, false).unwrap().unwrap();
        assert_eq!(persisted.kind(), kind);
        assert_eq!(persisted.bytes(), canonical.bytes());
        let diagnostics = spool.diagnostics();
        assert_eq!(diagnostics.codec_decode_calls, 1);
        assert!(diagnostics.codec_decode_time_ns > 0);
        assert_eq!(
            spool
                .read_work_unit(key)
                .unwrap()
                .unwrap()
                .pixel
                .unwrap()
                .decoded,
            decoded
        );
    }

    #[test]
    fn selective_component_reads_do_not_touch_the_paired_payload() {
        let (_temporary, checkpoint) = checkpoint();
        let mut spool = open(&checkpoint, binding(4)).unwrap();
        let key = key(0);
        let pixel_kind = ShardProfileKind::Pixel2dUint8;
        let validity_kind = ShardProfileKind::Validity2d;
        let pixels = decoded(pixel_kind, 23);
        let validity = decoded(validity_kind, 0x55);
        spool
            .append_if_absent(
                key,
                Some(SpoolChunkInput::new(pixel_kind, &pixels)),
                Some(SpoolChunkInput::new(validity_kind, &validity)),
                packed(key, 3, 1, true, true),
            )
            .unwrap();

        let descriptor = spool.work_unit_descriptor(key).unwrap();
        assert_eq!(descriptor.key, key);
        assert!(descriptor.pixel.is_some());
        assert!(descriptor.validity.is_some());
        let record = descriptor
            .packed_index_record(IntensityDType::Uint8, 6)
            .unwrap();
        assert!(!record.all_voxels_valid());
        assert!(!record.all_voxels_invalid());
        let reader = spool.payload_reader().unwrap();
        let decoded_validity = reader.read_validity_component(descriptor).unwrap();
        assert_eq!(decoded_validity.codec_decode_calls, 1);
        assert!(decoded_validity.codec_decode_time_ns > 0);
        assert_eq!(decoded_validity.chunk.unwrap().decoded, validity);

        // Corrupt only the pixel frame. A validity-only read must remain
        // successful, which is the direction used by halo-only parents.
        let pixel_offset = descriptor.pixel.unwrap().offset;
        let mut payload = OpenOptions::new()
            .read(true)
            .write(true)
            .open(checkpoint.join(PAYLOAD_FILE))
            .unwrap();
        payload.seek(SeekFrom::Start(pixel_offset)).unwrap();
        let mut original_pixel_byte = [0_u8; 1];
        payload.read_exact(&mut original_pixel_byte).unwrap();
        payload.seek(SeekFrom::Start(pixel_offset)).unwrap();
        payload.write_all(&[original_pixel_byte[0] ^ 0xff]).unwrap();
        assert_eq!(
            reader
                .read_validity_component(descriptor)
                .unwrap()
                .chunk
                .unwrap()
                .decoded,
            validity
        );
        assert!(matches!(
            reader.read_pixel_component(descriptor),
            Err(ImportError::InvalidCheckpoint(_))
        ));
        payload.seek(SeekFrom::Start(pixel_offset)).unwrap();
        payload.write_all(&original_pixel_byte).unwrap();

        // Corrupt only the validity frame after the immutable descriptor and
        // snapshot have been captured. A selective pixel read must still
        // succeed, proving that it neither reads nor decodes the paired mask.
        let validity_offset = descriptor.validity.unwrap().offset;
        payload.seek(SeekFrom::Start(validity_offset)).unwrap();
        let mut byte = [0_u8; 1];
        payload.read_exact(&mut byte).unwrap();
        byte[0] ^= 0xff;
        payload.seek(SeekFrom::Start(validity_offset)).unwrap();
        payload.write_all(&byte).unwrap();

        let pixel = reader.read_pixel_component(descriptor).unwrap();
        assert_eq!(pixel.codec_decode_calls, 1);
        assert!(pixel.codec_decode_time_ns > 0);
        assert_eq!(pixel.chunk.unwrap().decoded, pixels);
        assert!(matches!(
            reader.read_validity_component(descriptor),
            Err(ImportError::InvalidCheckpoint(_))
        ));
    }

    #[test]
    fn packed_index_uniform_facts_allow_payload_free_validity_reads() {
        let (_temporary, checkpoint) = checkpoint();
        let mut spool = open(&checkpoint, binding(5)).unwrap();
        let all_valid_key = key(0);
        let all_invalid_key = key(1);
        let pixel_kind = ShardProfileKind::Pixel2dUint8;
        let validity_kind = ShardProfileKind::Validity2d;
        let pixels = decoded(pixel_kind, 7);
        let validity = decoded(validity_kind, 0xff);
        spool
            .append_if_absent(
                all_valid_key,
                Some(SpoolChunkInput::new(pixel_kind, &pixels)),
                Some(SpoolChunkInput::new(validity_kind, &validity)),
                packed(all_valid_key, 6, 1, true, true),
            )
            .unwrap();
        spool
            .append_if_absent(
                all_invalid_key,
                None,
                None,
                packed(all_invalid_key, 0, 0, false, true),
            )
            .unwrap();

        let all_valid = spool.work_unit_descriptor(all_valid_key).unwrap();
        let valid_record = all_valid
            .packed_index_record(IntensityDType::Uint8, 6)
            .unwrap();
        assert!(valid_record.all_voxels_valid());
        assert!(!valid_record.all_voxels_invalid());
        // The canonical spool retains an all-valid mask component, but the
        // record proves a worker can synthesize it without reading that frame.
        assert!(all_valid.validity.is_some());

        let all_invalid = spool.work_unit_descriptor(all_invalid_key).unwrap();
        let invalid_record = all_invalid
            .packed_index_record(IntensityDType::Uint8, 6)
            .unwrap();
        assert!(!invalid_record.all_voxels_valid());
        assert!(invalid_record.all_voxels_invalid());
        assert!(all_invalid.pixel.is_none());
        assert!(all_invalid.validity.is_none());
        let absent = spool
            .payload_reader()
            .unwrap()
            .read_validity_component(all_invalid)
            .unwrap();
        assert!(absent.chunk.is_none());
        assert_eq!(absent.codec_decode_calls, 0);
        assert_eq!(absent.codec_decode_time_ns, 0);
    }

    #[test]
    fn creates_four_files_and_resumes_only_the_durable_ordered_prefix() {
        let (_temporary, checkpoint) = checkpoint();
        let binding = binding(3);
        let pixel_kind = ShardProfileKind::Pixel2dUint8;
        let validity_kind = ShardProfileKind::Validity2d;
        let pixel = decoded(pixel_kind, 7);
        let validity = decoded(validity_kind, 0xff);
        let first = SpoolWorkUnitKey::new(0, 0, 0, 0, 0, 0, 0);
        let second = SpoolWorkUnitKey::new(0, 0, 0, 0, 0, 0, 1);
        let third = SpoolWorkUnitKey::new(0, 1, 0, 0, 0, 0, 0);
        assert_eq!(
            SpoolWorkUnitKey::from_coordinates(first.coordinates()),
            first
        );

        let mut spool = open(&checkpoint, binding).unwrap();
        assert!(spool.is_empty());
        assert!(
            spool
                .append_if_absent(
                    first,
                    Some(SpoolChunkInput::new(pixel_kind, &pixel)),
                    None,
                    packed(first, 6, 1, true, false),
                )
                .unwrap()
        );
        assert!(
            spool
                .append_if_absent(second, None, None, packed(second, 6, 0, false, false))
                .unwrap()
        );
        assert!(
            spool
                .append_if_absent(
                    third,
                    Some(SpoolChunkInput::new(pixel_kind, &pixel)),
                    Some(SpoolChunkInput::new(validity_kind, &validity)),
                    packed(third, 3, 1, true, true),
                )
                .unwrap()
        );
        assert!(
            !spool
                .append_if_absent(first, None, None, packed(first, 6, 0, false, false))
                .unwrap()
        );
        let pending = spool.diagnostics();
        assert_eq!(pending.checkpoint_pending_work_units, 3);
        assert_eq!(pending.checkpoint_durable_work_units, 0);
        spool.commit_pending().unwrap();
        let committed = spool.diagnostics();
        assert_eq!(committed.checkpoint_pending_work_units, 0);
        assert_eq!(committed.checkpoint_durable_work_units, 3);
        assert_eq!(committed.checkpoint_committed_batches, 1);
        assert_eq!(committed.checkpoint_watermark_bytes, 96);
        assert_eq!(committed.codec_encode_calls, 3);
        assert_eq!(committed.codec_decode_calls, 0);
        assert_eq!(committed.sync_calls, 8);
        drop(spool);

        let entries = fs::read_dir(&checkpoint).unwrap().count();
        assert_eq!(entries, 4);
        assert_eq!(
            fs::read(checkpoint.join(HEADER_FILE)).unwrap(),
            header_bytes(binding)
        );

        let mut resumed = open(&checkpoint, binding).unwrap();
        assert_eq!(resumed.len(), 3);
        assert_eq!(
            resumed.keys().collect::<Vec<_>>(),
            vec![first, second, third]
        );
        assert!(resumed.contains(second));
        let recovered = resumed.read_work_unit(third).unwrap().unwrap();
        assert_eq!(recovered.key, third);
        assert_eq!(recovered.pixel.unwrap().decoded, pixel);
        assert_eq!(recovered.validity.unwrap().decoded, validity);
        assert_eq!(
            recovered.packed_index,
            packed(third, 3, 1, true, true).encode()
        );
        let elided = resumed.read_work_unit(second).unwrap().unwrap();
        assert!(elided.pixel.is_none());
        assert!(elided.validity.is_none());
        assert_eq!(resumed.diagnostics().codec_decode_calls, 5);
    }

    #[test]
    fn exact_binding_and_canonical_append_order_are_required() {
        let (_temporary, checkpoint) = checkpoint();
        let expected_binding = binding(9);
        let later = SpoolWorkUnitKey::new(0, 0, 0, 0, 0, 0, 2);
        let earlier = SpoolWorkUnitKey::new(0, 0, 0, 0, 0, 0, 1);
        let mut spool = open(&checkpoint, expected_binding).unwrap();
        spool
            .append_if_absent(later, None, None, packed(later, 6, 0, false, false))
            .unwrap();
        assert!(matches!(
            spool.append_if_absent(earlier, None, None, packed(earlier, 6, 0, false, false)),
            Err(ImportError::InvalidRequest(_))
        ));
        drop(spool);

        assert!(matches!(
            open(&checkpoint, binding(10)),
            Err(ImportError::InvalidCheckpoint(_))
        ));
    }

    #[test]
    fn reopen_rejects_committed_payload_corruption() {
        let (_temporary, checkpoint) = checkpoint();
        let binding = binding(20);
        let key = SpoolWorkUnitKey::new(0, 0, 0, 0, 0, 0, 0);
        let kind = ShardProfileKind::Pixel2dUint8;
        let bytes = decoded(kind, 4);
        let mut spool = open(&checkpoint, binding).unwrap();
        spool
            .append_if_absent(
                key,
                Some(SpoolChunkInput::new(kind, &bytes)),
                None,
                packed(key, 6, 1, true, false),
            )
            .unwrap();
        spool.commit_pending().unwrap();
        drop(spool);

        let path = checkpoint.join(PAYLOAD_FILE);
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&[0xff]).unwrap();
        file.sync_all().unwrap();

        assert!(matches!(
            open(&checkpoint, binding),
            Err(ImportError::InvalidCheckpoint(_))
        ));
    }

    #[test]
    fn reopen_recovers_only_the_complete_durable_prefix() {
        let (_temporary, checkpoint) = checkpoint();
        let binding = binding(30);
        let first = SpoolWorkUnitKey::new(0, 0, 0, 0, 0, 0, 0);
        let second = SpoolWorkUnitKey::new(0, 0, 0, 0, 0, 0, 1);
        let kind = ShardProfileKind::Pixel2dUint8;
        let bytes = decoded(kind, 5);
        let mut spool = open(&checkpoint, binding).unwrap();
        spool
            .append_if_absent(
                first,
                Some(SpoolChunkInput::new(kind, &bytes)),
                None,
                packed(first, 6, 1, true, false),
            )
            .unwrap();
        spool.commit_pending().unwrap();
        let first_payload_bytes = spool.diagnostics().checkpoint_payload_bytes;
        spool
            .append_if_absent(
                second,
                Some(SpoolChunkInput::new(kind, &bytes)),
                None,
                packed(second, 6, 1, true, false),
            )
            .unwrap();
        drop(spool);

        let journal_path = checkpoint.join(JOURNAL_FILE);
        let payload_path = checkpoint.join(PAYLOAD_FILE);
        let mut payload = fs::OpenOptions::new()
            .append(true)
            .open(&payload_path)
            .unwrap();
        payload.write_all(&[0xaa]).unwrap();
        payload.sync_all().unwrap();

        let resumed = open(&checkpoint, binding).unwrap();
        assert_eq!(resumed.keys().collect::<Vec<_>>(), vec![first]);
        assert_eq!(
            fs::metadata(journal_path).unwrap().len(),
            JOURNAL_RECORD_BYTES as u64
        );
        assert_eq!(
            fs::metadata(payload_path).unwrap().len(),
            first_payload_bytes
        );
    }

    #[test]
    fn reopen_rejects_checksum_invalid_and_reordered_journal() {
        for mutation in ["checksum", "reordered"] {
            let (_temporary, checkpoint) = checkpoint();
            let binding = binding(40);
            let first = SpoolWorkUnitKey::new(0, 0, 0, 0, 0, 0, 0);
            let second = SpoolWorkUnitKey::new(0, 0, 0, 0, 0, 0, 1);
            let mut spool = open(&checkpoint, binding).unwrap();
            spool
                .append_if_absent(first, None, None, packed(first, 6, 0, false, false))
                .unwrap();
            spool
                .append_if_absent(second, None, None, packed(second, 6, 0, false, false))
                .unwrap();
            spool.commit_pending().unwrap();
            drop(spool);

            let path = checkpoint.join(JOURNAL_FILE);
            let mut bytes = fs::read(&path).unwrap();
            match mutation {
                "partial" => {
                    bytes.pop();
                }
                "checksum" => bytes[JOURNAL_BODY_BYTES] ^= 1,
                "reordered" => {
                    let (first, second) = bytes.split_at_mut(JOURNAL_RECORD_BYTES);
                    first.swap_with_slice(second);
                }
                _ => unreachable!(),
            }
            fs::write(path, bytes).unwrap();
            assert!(matches!(
                open(&checkpoint, binding),
                Err(ImportError::InvalidCheckpoint(_))
            ));
        }
    }

    #[test]
    fn reopen_discards_a_torn_final_journal_row() {
        let (_temporary, checkpoint) = checkpoint();
        let binding = binding(45);
        let first = SpoolWorkUnitKey::new(0, 0, 0, 0, 0, 0, 0);
        let second = SpoolWorkUnitKey::new(0, 0, 0, 0, 0, 0, 1);
        let mut spool = open(&checkpoint, binding).unwrap();
        spool
            .append_if_absent(first, None, None, packed(first, 6, 0, false, false))
            .unwrap();
        spool.commit_pending().unwrap();
        spool
            .append_if_absent(second, None, None, packed(second, 6, 0, false, false))
            .unwrap();
        drop(spool);

        let journal_path = checkpoint.join(JOURNAL_FILE);
        let mut bytes = fs::read(&journal_path).unwrap();
        bytes[JOURNAL_RECORD_BYTES + JOURNAL_BODY_BYTES] ^= 1;
        fs::write(&journal_path, bytes).unwrap();

        let resumed = open(&checkpoint, binding).unwrap();
        assert_eq!(resumed.keys().collect::<Vec<_>>(), vec![first]);
        assert_eq!(
            fs::metadata(journal_path).unwrap().len(),
            JOURNAL_RECORD_BYTES as u64
        );
    }

    #[test]
    fn reopen_enforces_plan_bound_and_checks_cancellation() {
        let (_temporary, checkpoint) = checkpoint();
        let binding = binding(50);
        let first = SpoolWorkUnitKey::new(0, 0, 0, 0, 0, 0, 0);
        let second = SpoolWorkUnitKey::new(0, 0, 0, 0, 0, 0, 1);
        let mut spool = open(&checkpoint, binding).unwrap();
        for key in [first, second] {
            spool
                .append_if_absent(key, None, None, packed(key, 6, 0, false, false))
                .unwrap();
        }
        spool.commit_pending().unwrap();
        drop(spool);

        assert!(matches!(
            ImportSpool::open_or_create(&checkpoint, binding, 1, || false),
            Err(ImportError::InvalidCheckpoint(_))
        ));
        assert!(matches!(
            ImportSpool::open_or_create(&checkpoint, binding, 2, || true),
            Err(ImportError::Cancelled)
        ));
    }

    #[test]
    fn predecessor_three_file_checkpoint_is_rejected_without_migration() {
        let (_temporary, checkpoint) = checkpoint();
        fs::write(checkpoint.join(HEADER_FILE), b"mirante4d-import-spool-1\n").unwrap();
        fs::write(checkpoint.join(JOURNAL_FILE), []).unwrap();
        fs::write(checkpoint.join(PAYLOAD_FILE), []).unwrap();

        assert!(matches!(
            open(&checkpoint, binding(55)),
            Err(ImportError::InvalidCheckpoint(message))
                if message.contains("fixed spool file set")
        ));
        assert!(!checkpoint.join(WATERMARK_FILE).exists());
    }

    #[test]
    fn append_never_grows_past_the_plan_record_bound() {
        let (_temporary, checkpoint) = checkpoint();
        let mut spool = ImportSpool::open_or_create(&checkpoint, binding(56), 1, || false).unwrap();
        append_elided(&mut spool, key(0));
        assert!(matches!(
            spool.append_if_absent(key(1), None, None, packed(key(1), 6, 0, false, false)),
            Err(ImportError::InvalidRequest(_))
        ));
        assert_eq!(spool.len(), 1);
        assert_eq!(
            fs::metadata(checkpoint.join(JOURNAL_FILE)).unwrap().len(),
            JOURNAL_RECORD_BYTES as u64
        );
    }

    #[test]
    fn work_byte_and_age_bounds_commit_without_per_unit_syncs() {
        for (name, policy, pixel) in [
            (
                "work",
                DurabilityBatchPolicy {
                    work_units_max: 2,
                    payload_bytes_max: DURABILITY_BATCH_PAYLOAD_BYTES_MAX,
                    age_max: DURABILITY_BATCH_AGE_MAX,
                },
                false,
            ),
            (
                "age",
                DurabilityBatchPolicy {
                    work_units_max: DURABILITY_BATCH_WORK_UNITS_MAX,
                    payload_bytes_max: DURABILITY_BATCH_PAYLOAD_BYTES_MAX,
                    age_max: Duration::ZERO,
                },
                false,
            ),
            (
                "bytes",
                DurabilityBatchPolicy {
                    work_units_max: DURABILITY_BATCH_WORK_UNITS_MAX,
                    payload_bytes_max: u64::try_from(
                        encode_inner_payload(
                            ShardProfileKind::Pixel2dUint8,
                            &decoded(ShardProfileKind::Pixel2dUint8, 7),
                        )
                        .unwrap()
                        .len(),
                    )
                    .unwrap(),
                    age_max: DURABILITY_BATCH_AGE_MAX,
                },
                true,
            ),
        ] {
            let (_temporary, checkpoint) = checkpoint();
            let binding = binding(match name {
                "work" => 61,
                "age" => 62,
                "bytes" => 63,
                _ => unreachable!(),
            });
            let mut spool = open_with_policy(&checkpoint, binding, 16, policy).unwrap();
            if pixel {
                let kind = ShardProfileKind::Pixel2dUint8;
                append_pixel(&mut spool, key(0), kind, &decoded(kind, 7));
            } else {
                append_elided(&mut spool, key(0));
                if name == "work" {
                    assert_eq!(spool.diagnostics().checkpoint_durable_work_units, 0);
                    append_elided(&mut spool, key(1));
                }
            }
            let diagnostics = spool.diagnostics();
            assert_eq!(diagnostics.checkpoint_committed_batches, 1, "{name}");
            assert_eq!(diagnostics.checkpoint_pending_work_units, 0, "{name}");
            // Five create-time syncs plus exactly one three-sync batch.
            assert_eq!(diagnostics.sync_calls, 8, "{name}");
        }
    }

    #[test]
    fn owner_tick_commits_an_expired_idle_batch() {
        let (_temporary, checkpoint) = checkpoint();
        let mut spool = open_with_policy(
            &checkpoint,
            binding(73),
            4,
            DurabilityBatchPolicy {
                work_units_max: 4,
                payload_bytes_max: DURABILITY_BATCH_PAYLOAD_BYTES_MAX,
                age_max: Duration::from_millis(1),
            },
        )
        .unwrap();
        append_elided(&mut spool, key(0));
        assert_eq!(spool.diagnostics().checkpoint_pending_work_units, 1);
        std::thread::sleep(Duration::from_millis(3));
        spool.commit_expired().unwrap();
        assert_eq!(spool.diagnostics().checkpoint_pending_work_units, 0);
        assert_eq!(spool.diagnostics().checkpoint_durable_work_units, 1);
    }

    #[test]
    fn cleanup_does_not_unlink_a_replaced_spool_name() {
        let (_temporary, checkpoint) = checkpoint();
        let spool = open(&checkpoint, binding(74)).unwrap();
        let payload = checkpoint.join(PAYLOAD_FILE);
        let displaced = checkpoint.join("displaced-payload");
        fs::rename(&payload, &displaced).unwrap();
        fs::write(&payload, b"replacement").unwrap();

        spool.cleanup_owned_files();

        assert_eq!(fs::read(payload).unwrap(), b"replacement");
        assert!(displaced.exists());
        assert!(!checkpoint.join(HEADER_FILE).exists());
        assert!(!checkpoint.join(JOURNAL_FILE).exists());
        assert!(!checkpoint.join(WATERMARK_FILE).exists());
    }

    #[test]
    fn cleanup_does_not_remove_a_replaced_checkpoint_directory() {
        let (temporary, checkpoint) = checkpoint();
        let spool = open(&checkpoint, binding(75)).unwrap();
        let displaced = temporary.path().join("displaced-checkpoint");
        fs::rename(&checkpoint, &displaced).unwrap();
        fs::create_dir(&checkpoint).unwrap();

        spool.cleanup_owned_files();
        spool.cleanup_owned_directory();

        assert!(checkpoint.is_dir());
        assert!(displaced.is_dir());
        assert_eq!(fs::read_dir(displaced).unwrap().count(), 0);
    }

    #[test]
    fn every_append_and_commit_crash_boundary_recovers_an_exact_prefix() {
        let append_failpoints = [
            SpoolFailpoint::AfterPayloadAppend,
            SpoolFailpoint::AfterJournalAppend,
        ];
        for (ordinal, failpoint) in append_failpoints.into_iter().enumerate() {
            let (_temporary, checkpoint) = checkpoint();
            let binding = binding(70 + ordinal as u8);
            let kind = ShardProfileKind::Pixel2dUint8;
            let bytes = decoded(kind, 9);
            let mut spool = open(&checkpoint, binding).unwrap();
            append_pixel(&mut spool, key(0), kind, &bytes);
            spool.commit_pending().unwrap();
            spool.set_failpoint(failpoint);
            assert!(matches!(
                spool.append_if_absent(
                    key(1),
                    Some(SpoolChunkInput::new(kind, &bytes)),
                    None,
                    packed(key(1), 6, 1, true, false),
                ),
                Err(ImportError::Io { .. })
            ));
            drop(spool);

            let resumed = open(&checkpoint, binding).unwrap();
            assert_eq!(resumed.keys().collect::<Vec<_>>(), vec![key(0)]);
            assert_eq!(resumed.diagnostics().checkpoint_pending_work_units, 0);
        }

        let commit_failpoints = [
            (SpoolFailpoint::BeforePayloadSync, 1),
            (SpoolFailpoint::AfterPayloadSync, 1),
            (SpoolFailpoint::BeforeJournalSync, 1),
            (SpoolFailpoint::AfterJournalSync, 1),
            (SpoolFailpoint::AfterWatermarkAppend, 2),
            (SpoolFailpoint::BeforeWatermarkSync, 2),
            (SpoolFailpoint::AfterWatermarkSync, 2),
        ];
        for (ordinal, (failpoint, expected_records)) in commit_failpoints.into_iter().enumerate() {
            let (_temporary, checkpoint) = checkpoint();
            let binding = binding(80 + ordinal as u8);
            let mut spool = open(&checkpoint, binding).unwrap();
            append_elided(&mut spool, key(0));
            spool.commit_pending().unwrap();
            append_elided(&mut spool, key(1));
            spool.set_failpoint(failpoint);
            let error = spool.commit_pending().unwrap_err();
            if matches!(
                failpoint,
                SpoolFailpoint::BeforePayloadSync
                    | SpoolFailpoint::AfterPayloadSync
                    | SpoolFailpoint::BeforeJournalSync
                    | SpoolFailpoint::AfterJournalSync
                    | SpoolFailpoint::BeforeWatermarkSync
                    | SpoolFailpoint::AfterWatermarkSync
            ) {
                assert!(matches!(
                    error,
                    ImportError::CheckpointDurabilityIndeterminate { .. }
                ));
            } else {
                assert!(matches!(error, ImportError::Io { .. }));
            }
            drop(spool);

            let resumed = open(&checkpoint, binding).unwrap();
            let expected = if expected_records == 1 {
                vec![key(0)]
            } else {
                vec![key(0), key(1)]
            };
            assert_eq!(
                resumed.keys().collect::<Vec<_>>(),
                expected,
                "{failpoint:?}"
            );
        }
    }

    #[test]
    fn truncation_and_watermark_corruption_never_admit_ambiguous_bytes() {
        for mutation in [
            "journal_truncated",
            "payload_truncated",
            "watermark_truncated",
            "final_watermark_checksum",
            "nonfinal_watermark_checksum",
            "watermark_reordered",
        ] {
            let (_temporary, checkpoint) = checkpoint();
            let binding = binding(100);
            let kind = ShardProfileKind::Pixel2dUint8;
            let bytes = decoded(kind, 3);
            let mut spool = open(&checkpoint, binding).unwrap();
            append_pixel(&mut spool, key(0), kind, &bytes);
            spool.commit_pending().unwrap();
            let first_payload_bytes = spool.durable_payload_bytes;
            append_pixel(&mut spool, key(1), kind, &bytes);
            spool.commit_pending().unwrap();
            drop(spool);

            match mutation {
                "journal_truncated" => {
                    let path = checkpoint.join(JOURNAL_FILE);
                    let length = fs::metadata(&path).unwrap().len();
                    fs::OpenOptions::new()
                        .write(true)
                        .open(path)
                        .unwrap()
                        .set_len(length - 1)
                        .unwrap();
                }
                "payload_truncated" => {
                    let path = checkpoint.join(PAYLOAD_FILE);
                    let length = fs::metadata(&path).unwrap().len();
                    fs::OpenOptions::new()
                        .write(true)
                        .open(path)
                        .unwrap()
                        .set_len(length - 1)
                        .unwrap();
                }
                "watermark_truncated" => {
                    let path = checkpoint.join(WATERMARK_FILE);
                    let length = fs::metadata(&path).unwrap().len();
                    fs::OpenOptions::new()
                        .write(true)
                        .open(path)
                        .unwrap()
                        .set_len(length - 1)
                        .unwrap();
                }
                "final_watermark_checksum" => {
                    let path = checkpoint.join(WATERMARK_FILE);
                    let mut content = fs::read(&path).unwrap();
                    content[WATERMARK_RECORD_BYTES + WATERMARK_BODY_BYTES] ^= 1;
                    fs::write(path, content).unwrap();
                }
                "nonfinal_watermark_checksum" => {
                    let path = checkpoint.join(WATERMARK_FILE);
                    let mut content = fs::read(&path).unwrap();
                    content[WATERMARK_BODY_BYTES] ^= 1;
                    fs::write(path, content).unwrap();
                }
                "watermark_reordered" => {
                    let path = checkpoint.join(WATERMARK_FILE);
                    let mut content = fs::read(&path).unwrap();
                    let (first, second) = content.split_at_mut(WATERMARK_RECORD_BYTES);
                    first.swap_with_slice(second);
                    fs::write(path, content).unwrap();
                }
                _ => unreachable!(),
            }

            match mutation {
                "watermark_truncated" | "final_watermark_checksum" => {
                    let resumed = open(&checkpoint, binding).unwrap();
                    assert_eq!(resumed.keys().collect::<Vec<_>>(), vec![key(0)]);
                    assert_eq!(
                        fs::metadata(checkpoint.join(PAYLOAD_FILE)).unwrap().len(),
                        first_payload_bytes
                    );
                    assert_eq!(
                        fs::metadata(checkpoint.join(WATERMARK_FILE)).unwrap().len(),
                        WATERMARK_RECORD_BYTES as u64
                    );
                }
                _ => assert!(matches!(
                    open(&checkpoint, binding),
                    Err(ImportError::InvalidCheckpoint(_))
                )),
            }
        }
    }

    #[test]
    fn recovery_rejects_suffixes_larger_than_one_declared_batch() {
        for file_name in [JOURNAL_FILE, PAYLOAD_FILE] {
            let (_temporary, checkpoint) = checkpoint();
            let binding = binding(if file_name == JOURNAL_FILE { 120 } else { 121 });
            let mut spool = open(&checkpoint, binding).unwrap();
            append_elided(&mut spool, key(0));
            spool.commit_pending().unwrap();
            let durable = if file_name == JOURNAL_FILE {
                spool.diagnostics().checkpoint_journal_bytes
            } else {
                spool.diagnostics().checkpoint_payload_bytes
            };
            drop(spool);
            let maximum_suffix = if file_name == JOURNAL_FILE {
                DURABILITY_BATCH_WORK_UNITS_MAX * JOURNAL_RECORD_BYTES as u64
            } else {
                DURABILITY_BATCH_PAYLOAD_BYTES_MAX
            };
            fs::OpenOptions::new()
                .write(true)
                .open(checkpoint.join(file_name))
                .unwrap()
                .set_len(durable + maximum_suffix + 1)
                .unwrap();
            assert!(matches!(
                open(&checkpoint, binding),
                Err(ImportError::InvalidCheckpoint(_))
            ));
        }
    }

    #[test]
    fn append_rejects_component_and_packed_index_mismatches_without_writing() {
        let (_temporary, checkpoint) = checkpoint();
        let binding = binding(60);
        let key = SpoolWorkUnitKey::new(0, 0, 0, 0, 0, 0, 0);
        let other = SpoolWorkUnitKey::new(0, 0, 0, 0, 0, 0, 1);
        let bytes = decoded(ShardProfileKind::Validity2d, 0);
        let mut spool = open(&checkpoint, binding).unwrap();

        assert!(matches!(
            spool.append_if_absent(
                key,
                Some(SpoolChunkInput::new(ShardProfileKind::Validity2d, &bytes)),
                None,
                packed(key, 6, 1, true, false)
            ),
            Err(ImportError::InvalidRequest(_))
        ));
        assert!(matches!(
            spool.append_if_absent(key, None, None, packed(other, 6, 0, false, false)),
            Err(ImportError::InvalidRequest(_))
        ));
        assert_eq!(
            fs::metadata(checkpoint.join(JOURNAL_FILE)).unwrap().len(),
            0
        );
        assert_eq!(
            fs::metadata(checkpoint.join(PAYLOAD_FILE)).unwrap().len(),
            0
        );
    }
}

//! Fixed-object, crash-safe canonical base-volume checkpoint.
//!
//! Pixels are stored as canonical little-endian planes in `[c, t, z, y, x]`
//! order.  Only a separately durable, checksummed plane prefix is trusted on
//! recovery; an interrupted final batch is deliberately recomputed.

use std::{
    ffi::OsStr,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use mirante4d_domain::{IntensityDType, Shape4D};
use mirante4d_identity::{Sha256Digest, Sha256Hasher};
use rustix::{
    fd::OwnedFd,
    fs::{AtFlags, FileType, Mode, OFlags, fstat, fsync, openat, statat, unlinkat},
    io::{Errno, dup},
};

use crate::ImportError;

const DATA_FILE: &str = "canonical-data";
const STATE_FILE: &str = "canonical-state";
const SCHEMA: &[u8] = b"mirante4d-canonical-base-1\n";
const STATE_HEADER_BYTES: usize = SCHEMA.len() + 32 + 32 + 8 * 5 + 1;
const STATE_RECORD_BYTES: usize = 8 + 32;
const COPY_BUFFER_BYTES: usize = 1024 * 1024;
const BATCH_PLANES_MAX: u64 = 16;
const BATCH_BYTES_MAX: u64 = 64 * 1024 * 1024;
pub(crate) const CANONICAL_PLANE_BYTES_MAX: u64 = BATCH_BYTES_MAX;
const BATCH_AGE_MAX: Duration = Duration::from_secs(15);
const DIRECTORY_OPEN_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW);
const FILE_OPEN_FLAGS: OFlags = OFlags::RDWR.union(OFlags::CLOEXEC).union(OFlags::NOFOLLOW);
const FILE_CREATE_FLAGS: OFlags = FILE_OPEN_FLAGS.union(OFlags::CREATE).union(OFlags::EXCL);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CanonicalCacheBinding {
    plan_digest: Sha256Digest,
    source_fingerprint: Sha256Digest,
}

impl CanonicalCacheBinding {
    pub(crate) const fn new(plan_digest: Sha256Digest, source_fingerprint: Sha256Digest) -> Self {
        Self {
            plan_digest,
            source_fingerprint,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct CanonicalCacheDiagnostics {
    pub(crate) durable_planes: u64,
    pub(crate) pending_planes: u64,
    pub(crate) data_bytes: u64,
    pub(crate) state_bytes: u64,
    pub(crate) committed_batches: u64,
    pub(crate) sync_calls: u64,
    pub(crate) sync_time_ns: u64,
}

pub(crate) struct CanonicalBaseCache {
    _directory: OwnedFd,
    data_path: PathBuf,
    state_path: PathBuf,
    data: File,
    state: File,
    shape: Shape4D,
    channels: u32,
    dtype: IntensityDType,
    plane_bytes: u64,
    total_planes: u64,
    durable_planes: u64,
    completed_planes: u64,
    batch_digests: Vec<(u64, Sha256Digest)>,
    pending_started: Option<Instant>,
    committed_batches: u64,
    sync_calls: u64,
    sync_time_ns: u64,
}

/// Immutable positional reader for a complete canonical checkpoint.
///
/// Workers share this handle while the import thread remains the only owner
/// allowed to advance durable checkpoint state. Positional reads avoid a
/// shared seek cursor and therefore remain deterministic under concurrency.
pub(crate) struct CanonicalBaseReader {
    data_path: PathBuf,
    data: File,
    shape: Shape4D,
    channels: u32,
    dtype: IntensityDType,
    plane_bytes: u64,
}

impl CanonicalBaseCache {
    pub(crate) fn open_or_create(
        directory: &Path,
        binding: CanonicalCacheBinding,
        shape: Shape4D,
        channels: u32,
        dtype: IntensityDType,
    ) -> Result<Self, ImportError> {
        let directory_fd = openat(
            rustix::fs::CWD,
            directory,
            DIRECTORY_OPEN_FLAGS,
            Mode::empty(),
        )
        .map_err(|source| {
            rustix_io_error("open canonical checkpoint directory", directory, source)
        })?;
        Self::open_or_create_in(directory, &directory_fd, binding, shape, channels, dtype)
    }

    pub(crate) fn open_or_create_in(
        directory: &Path,
        directory_fd: &OwnedFd,
        binding: CanonicalCacheBinding,
        shape: Shape4D,
        channels: u32,
        dtype: IntensityDType,
    ) -> Result<Self, ImportError> {
        let data_path = directory.join(DATA_FILE);
        let state_path = directory.join(STATE_FILE);
        let data_exists = entry_state(directory_fd, &data_path, DATA_FILE)?;
        let state_exists = entry_state(directory_fd, &state_path, STATE_FILE)?;
        if data_exists != state_exists {
            return Err(ImportError::InvalidCheckpoint(
                "canonical checkpoint is only partially present".to_owned(),
            ));
        }

        let plane_bytes = shape
            .y()
            .checked_mul(shape.x())
            .and_then(|value| value.checked_mul(u64::from(dtype.bytes_per_sample())))
            .ok_or(ImportError::Overflow)?;
        if plane_bytes > CANONICAL_PLANE_BYTES_MAX {
            return Err(ImportError::UnsupportedSource(format!(
                "one canonical TIFF plane requires {plane_bytes} bytes, exceeding the fixed {CANONICAL_PLANE_BYTES_MAX}-byte checkpoint work-unit bound"
            )));
        }
        let total_planes = u64::from(channels)
            .checked_mul(shape.t())
            .and_then(|value| value.checked_mul(shape.z()))
            .ok_or(ImportError::Overflow)?;
        let total_bytes = total_planes
            .checked_mul(plane_bytes)
            .ok_or(ImportError::Overflow)?;
        let expected_header = state_header(binding, shape, channels, dtype);

        if !data_exists {
            let data = open_checkpoint_file(
                directory_fd,
                &data_path,
                DATA_FILE,
                FILE_CREATE_FLAGS,
                "create canonical data checkpoint",
            )?;
            data.set_len(total_bytes)
                .map_err(|source| io_error("size canonical data checkpoint", &data_path, source))?;
            let mut state = open_checkpoint_file(
                directory_fd,
                &state_path,
                STATE_FILE,
                FILE_CREATE_FLAGS,
                "create canonical state checkpoint",
            )?;
            state.write_all(&expected_header).map_err(|source| {
                io_error("write canonical checkpoint header", &state_path, source)
            })?;
            let mut cache = Self {
                _directory: dup(directory_fd).map_err(|source| {
                    rustix_io_error("retain canonical checkpoint directory", directory, source)
                })?,
                data_path,
                state_path,
                data,
                state,
                shape,
                channels,
                dtype,
                plane_bytes,
                total_planes,
                durable_planes: 0,
                completed_planes: 0,
                batch_digests: Vec::new(),
                pending_started: None,
                committed_batches: 0,
                sync_calls: 0,
                sync_time_ns: 0,
            };
            cache.sync_data()?;
            cache.sync_state()?;
            cache.sync_directory()?;
            return Ok(cache);
        }

        let mut data = open_checkpoint_file(
            directory_fd,
            &data_path,
            DATA_FILE,
            FILE_OPEN_FLAGS,
            "open canonical data checkpoint",
        )?;
        let mut state = open_checkpoint_file(
            directory_fd,
            &state_path,
            STATE_FILE,
            FILE_OPEN_FLAGS,
            "open canonical state checkpoint",
        )?;
        if data
            .metadata()
            .map_err(|source| io_error("stat canonical data checkpoint", &data_path, source))?
            .len()
            != total_bytes
        {
            return Err(ImportError::InvalidCheckpoint(
                "canonical data checkpoint has the wrong length".to_owned(),
            ));
        }

        let mut header = vec![0; STATE_HEADER_BYTES];
        state.read_exact(&mut header).map_err(|source| {
            invalid_or_io("read canonical checkpoint header", &state_path, source)
        })?;
        if header != expected_header {
            return Err(ImportError::InvalidCheckpoint(
                "canonical checkpoint belongs to different inputs or schema".to_owned(),
            ));
        }

        let state_len = state
            .metadata()
            .map_err(|source| io_error("stat canonical state checkpoint", &state_path, source))?
            .len();
        let header_bytes = u64::try_from(STATE_HEADER_BYTES).map_err(|_| ImportError::Overflow)?;
        let record_bytes = u64::try_from(STATE_RECORD_BYTES).map_err(|_| ImportError::Overflow)?;
        let complete_record_bytes = state_len.saturating_sub(header_bytes) / record_bytes;
        let mut durable_planes = 0_u64;
        let mut valid_records = 0_u64;
        let mut batch_digests = Vec::new();
        let mut scratch = vec![0; COPY_BUFFER_BYTES];
        for _ in 0..complete_record_bytes {
            let mut record = [0; STATE_RECORD_BYTES];
            state.read_exact(&mut record).map_err(|source| {
                invalid_or_io("read canonical checkpoint record", &state_path, source)
            })?;
            let completed = u64::from_le_bytes(record[..8].try_into().expect("fixed range"));
            if completed <= durable_planes
                || completed > total_planes
                || completed - durable_planes > BATCH_PLANES_MAX
                || (completed - durable_planes)
                    .checked_mul(plane_bytes)
                    .is_none_or(|bytes| bytes > BATCH_BYTES_MAX)
            {
                return Err(ImportError::InvalidCheckpoint(
                    "canonical checkpoint has an invalid complete state record".to_owned(),
                ));
            }
            let digest = hash_data_range(
                &mut data,
                durable_planes
                    .checked_mul(plane_bytes)
                    .ok_or(ImportError::Overflow)?,
                (completed - durable_planes)
                    .checked_mul(plane_bytes)
                    .ok_or(ImportError::Overflow)?,
                &mut scratch,
                &data_path,
            )?;
            if digest.as_bytes() != &record[8..] {
                return Err(ImportError::InvalidCheckpoint(
                    "canonical checkpoint committed data does not match its state record"
                        .to_owned(),
                ));
            }
            batch_digests.push((completed, digest));
            durable_planes = completed;
            valid_records = valid_records.checked_add(1).ok_or(ImportError::Overflow)?;
        }
        let valid_len = header_bytes
            .checked_add(
                valid_records
                    .checked_mul(record_bytes)
                    .ok_or(ImportError::Overflow)?,
            )
            .ok_or(ImportError::Overflow)?;
        if state_len != valid_len {
            state.set_len(valid_len).map_err(|source| {
                io_error("truncate canonical state suffix", &state_path, source)
            })?;
        }
        state
            .seek(SeekFrom::End(0))
            .map_err(|source| io_error("seek canonical state checkpoint", &state_path, source))?;
        Ok(Self {
            _directory: dup(directory_fd).map_err(|source| {
                rustix_io_error("retain canonical checkpoint directory", directory, source)
            })?,
            data_path,
            state_path,
            data,
            state,
            shape,
            channels,
            dtype,
            plane_bytes,
            total_planes,
            durable_planes,
            completed_planes: durable_planes,
            batch_digests,
            pending_started: None,
            committed_batches: valid_records,
            sync_calls: 0,
            sync_time_ns: 0,
        })
    }

    pub(crate) const fn durable_planes(&self) -> u64 {
        self.durable_planes
    }

    pub(crate) const fn total_planes(&self) -> u64 {
        self.total_planes
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.durable_planes == self.total_planes
    }

    /// Path-free identity of the exact canonical decoded source stream.
    /// Batch digests are produced by the cache's existing durability pass, so
    /// this adds no source decode or raw-data traversal.
    pub(crate) fn decoded_source_digest(&self) -> Result<Sha256Digest, ImportError> {
        if !self.is_complete() {
            return Err(ImportError::InvalidCheckpoint(
                "canonical source identity requested before ingest completed".to_owned(),
            ));
        }
        let mut hasher = Sha256Hasher::new();
        hasher.update(b"mirante4d-canonical-decoded-source-v1\0");
        for dimension in self.shape.dimensions() {
            hasher.update(dimension.to_le_bytes());
        }
        hasher.update(self.channels.to_le_bytes());
        hasher.update([match self.dtype {
            IntensityDType::Uint8 => 1,
            IntensityDType::Uint16 => 2,
            IntensityDType::Float32 => 3,
        }]);
        for (completed, digest) in &self.batch_digests {
            hasher.update(completed.to_le_bytes());
            hasher.update(digest.as_bytes());
        }
        Ok(hasher.finalize())
    }

    pub(crate) fn plane_ordinal(
        &self,
        channel: u32,
        timepoint: u64,
        z: u64,
    ) -> Result<u64, ImportError> {
        if channel >= self.channels || timepoint >= self.shape.t() || z >= self.shape.z() {
            return Err(ImportError::InvalidRequest(
                "canonical checkpoint plane coordinate is out of bounds",
            ));
        }
        u64::from(channel)
            .checked_mul(self.shape.t())
            .and_then(|value| value.checked_add(timepoint))
            .and_then(|value| value.checked_mul(self.shape.z()))
            .and_then(|value| value.checked_add(z))
            .ok_or(ImportError::Overflow)
    }

    pub(crate) fn write_row(
        &mut self,
        plane: u64,
        y: u64,
        x: u64,
        bytes_le: &[u8],
    ) -> Result<(), ImportError> {
        if plane != self.completed_planes || y >= self.shape.y() || x >= self.shape.x() {
            return Err(ImportError::InvalidCheckpoint(
                "canonical rows must be written to the current plane".to_owned(),
            ));
        }
        let width = u64::from(self.dtype.bytes_per_sample());
        let samples = u64::try_from(bytes_le.len())
            .map_err(|_| ImportError::Overflow)?
            .checked_div(width)
            .ok_or(ImportError::Overflow)?;
        if !(bytes_le.len() as u64).is_multiple_of(width)
            || samples == 0
            || x.checked_add(samples)
                .is_none_or(|end| end > self.shape.x())
        {
            return Err(ImportError::InvalidRequest(
                "canonical row write has invalid sample alignment or bounds",
            ));
        }
        let offset = plane
            .checked_mul(self.plane_bytes)
            .and_then(|value| {
                y.checked_mul(self.shape.x())
                    .and_then(|row| row.checked_add(x))
                    .and_then(|sample| sample.checked_mul(width))
                    .and_then(|byte| value.checked_add(byte))
            })
            .ok_or(ImportError::Overflow)?;
        self.commit_pending_before_plane()?;
        self.data
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.data.write_all(bytes_le))
            .map_err(|source| io_error("write canonical data checkpoint", &self.data_path, source))
    }

    pub(crate) fn complete_plane(&mut self, plane: u64) -> Result<(), ImportError> {
        if plane != self.completed_planes || plane >= self.total_planes {
            return Err(ImportError::InvalidCheckpoint(
                "canonical planes completed out of order".to_owned(),
            ));
        }
        self.commit_pending_before_plane()?;
        if self.completed_planes == self.durable_planes {
            self.pending_started = Some(Instant::now());
        }
        self.completed_planes = self
            .completed_planes
            .checked_add(1)
            .ok_or(ImportError::Overflow)?;
        let pending = self.completed_planes - self.durable_planes;
        let pending_bytes = pending
            .checked_mul(self.plane_bytes)
            .ok_or(ImportError::Overflow)?;
        if pending >= BATCH_PLANES_MAX || pending_bytes >= BATCH_BYTES_MAX {
            self.commit_pending()?;
        }
        Ok(())
    }

    /// Closes the existing completed prefix before any byte of the next plane
    /// is written or that plane is marked complete when the combined batch
    /// would exceed the fixed byte ceiling. The cache owner is the only caller
    /// with mutable access, so this preserves the single ordered commit
    /// authority while keeping one admitted plane atomic.
    fn commit_pending_before_plane(&mut self) -> Result<(), ImportError> {
        let pending_bytes = (self.completed_planes - self.durable_planes)
            .checked_mul(self.plane_bytes)
            .ok_or(ImportError::Overflow)?;
        let batch_bytes_with_plane = pending_bytes
            .checked_add(self.plane_bytes)
            .ok_or(ImportError::Overflow)?;
        if pending_bytes != 0 && batch_bytes_with_plane > BATCH_BYTES_MAX {
            self.commit_pending()?;
        }
        Ok(())
    }

    pub(crate) fn commit_expired(&mut self) -> Result<(), ImportError> {
        if self
            .pending_started
            .is_some_and(|started| started.elapsed() >= BATCH_AGE_MAX)
        {
            self.commit_pending()?;
        }
        Ok(())
    }

    pub(crate) fn commit_pending(&mut self) -> Result<(), ImportError> {
        if self.completed_planes == self.durable_planes {
            return Ok(());
        }
        let pending_planes = self.completed_planes - self.durable_planes;
        let start = self
            .durable_planes
            .checked_mul(self.plane_bytes)
            .ok_or(ImportError::Overflow)?;
        let len = pending_planes
            .checked_mul(self.plane_bytes)
            .ok_or(ImportError::Overflow)?;
        if pending_planes > BATCH_PLANES_MAX || len > BATCH_BYTES_MAX {
            return Err(ImportError::InvalidCheckpoint(
                "canonical pending batch exceeds its fixed durability bound".to_owned(),
            ));
        }
        self.sync_data()?;
        let mut scratch = vec![0; COPY_BUFFER_BYTES];
        let digest = hash_data_range(&mut self.data, start, len, &mut scratch, &self.data_path)?;
        self.state
            .seek(SeekFrom::End(0))
            .and_then(|_| self.state.write_all(&self.completed_planes.to_le_bytes()))
            .and_then(|_| self.state.write_all(digest.as_bytes()))
            .map_err(|source| {
                io_error(
                    "append canonical checkpoint record",
                    &self.state_path,
                    source,
                )
            })?;
        self.sync_state()?;
        self.durable_planes = self.completed_planes;
        self.pending_started = None;
        self.committed_batches = self
            .committed_batches
            .checked_add(1)
            .ok_or(ImportError::Overflow)?;
        self.batch_digests.push((self.completed_planes, digest));
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn read_region_into(
        &mut self,
        channel: u32,
        timepoint: u64,
        origin_zyx: [u64; 3],
        extent_zyx: [u64; 3],
        destination: &mut [u8],
    ) -> Result<(), ImportError> {
        if !self.is_complete() {
            return Err(ImportError::InvalidCheckpoint(
                "canonical checkpoint was read before its durable prefix was complete".to_owned(),
            ));
        }
        if channel >= self.channels
            || timepoint >= self.shape.t()
            || extent_zyx.contains(&0)
            || origin_zyx[0]
                .checked_add(extent_zyx[0])
                .is_none_or(|value| value > self.shape.z())
            || origin_zyx[1]
                .checked_add(extent_zyx[1])
                .is_none_or(|value| value > self.shape.y())
            || origin_zyx[2]
                .checked_add(extent_zyx[2])
                .is_none_or(|value| value > self.shape.x())
        {
            return Err(ImportError::InvalidRequest(
                "canonical checkpoint read region is out of bounds",
            ));
        }
        let width = u64::from(self.dtype.bytes_per_sample());
        let row_bytes = extent_zyx[2]
            .checked_mul(width)
            .ok_or(ImportError::Overflow)?;
        let expected = extent_zyx[0]
            .checked_mul(extent_zyx[1])
            .and_then(|value| value.checked_mul(row_bytes))
            .ok_or(ImportError::Overflow)?;
        if u64::try_from(destination.len()).map_err(|_| ImportError::Overflow)? != expected {
            return Err(ImportError::InvalidRequest(
                "canonical checkpoint read destination has the wrong length",
            ));
        }
        let mut output = 0_usize;
        for z in origin_zyx[0]..origin_zyx[0] + extent_zyx[0] {
            let plane = self.plane_ordinal(channel, timepoint, z)?;
            for y in origin_zyx[1]..origin_zyx[1] + extent_zyx[1] {
                let offset = plane
                    .checked_mul(self.plane_bytes)
                    .and_then(|value| {
                        y.checked_mul(self.shape.x())
                            .and_then(|row| row.checked_add(origin_zyx[2]))
                            .and_then(|sample| sample.checked_mul(width))
                            .and_then(|byte| value.checked_add(byte))
                    })
                    .ok_or(ImportError::Overflow)?;
                let end = output
                    .checked_add(usize::try_from(row_bytes).map_err(|_| ImportError::Overflow)?)
                    .ok_or(ImportError::Overflow)?;
                self.data
                    .seek(SeekFrom::Start(offset))
                    .and_then(|_| self.data.read_exact(&mut destination[output..end]))
                    .map_err(|source| {
                        io_error("read canonical data checkpoint", &self.data_path, source)
                    })?;
                output = end;
            }
        }
        Ok(())
    }

    pub(crate) fn reader(&self) -> Result<CanonicalBaseReader, ImportError> {
        if !self.is_complete() {
            return Err(ImportError::InvalidCheckpoint(
                "canonical checkpoint was shared before its durable prefix was complete".to_owned(),
            ));
        }
        Ok(CanonicalBaseReader {
            data_path: self.data_path.clone(),
            data: self.data.try_clone().map_err(|source| {
                io_error("clone canonical data checkpoint", &self.data_path, source)
            })?,
            shape: self.shape,
            channels: self.channels,
            dtype: self.dtype,
            plane_bytes: self.plane_bytes,
        })
    }

    #[cfg(test)]
    pub(crate) fn diagnostics(&self) -> Result<CanonicalCacheDiagnostics, ImportError> {
        Ok(CanonicalCacheDiagnostics {
            durable_planes: self.durable_planes,
            pending_planes: self.completed_planes - self.durable_planes,
            data_bytes: self
                .data
                .metadata()
                .map_err(|source| {
                    io_error("stat canonical data checkpoint", &self.data_path, source)
                })?
                .len(),
            state_bytes: self
                .state
                .metadata()
                .map_err(|source| {
                    io_error("stat canonical state checkpoint", &self.state_path, source)
                })?
                .len(),
            committed_batches: self.committed_batches,
            sync_calls: self.sync_calls,
            sync_time_ns: self.sync_time_ns,
        })
    }

    /// Removes only the two names that still resolve to the exact file
    /// descriptors owned by this cache. A replaced name is deliberately left
    /// untouched for a later explicit checkpoint reset.
    pub(crate) fn cleanup_owned_files(&self) {
        let _ = unlink_if_owned(&self._directory, DATA_FILE, &self.data);
        let _ = unlink_if_owned(&self._directory, STATE_FILE, &self.state);
    }

    fn sync_data(&mut self) -> Result<(), ImportError> {
        let started = Instant::now();
        self.data
            .sync_data()
            .map_err(|source| ImportError::CheckpointDurabilityIndeterminate {
                operation: "synchronize canonical data checkpoint",
                path: self.data_path.clone(),
                source,
            })?;
        self.record_sync(started)
    }

    fn sync_state(&mut self) -> Result<(), ImportError> {
        let started = Instant::now();
        self.state.sync_data().map_err(|source| {
            ImportError::CheckpointDurabilityIndeterminate {
                operation: "synchronize canonical state checkpoint",
                path: self.state_path.clone(),
                source,
            }
        })?;
        self.record_sync(started)
    }

    fn sync_directory(&mut self) -> Result<(), ImportError> {
        let started = Instant::now();
        fsync(&self._directory).map_err(|source| {
            ImportError::CheckpointDurabilityIndeterminate {
                operation: "synchronize canonical checkpoint directory",
                path: self
                    .data_path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .to_path_buf(),
                source: std::io::Error::from(source),
            }
        })?;
        self.record_sync(started)
    }

    fn record_sync(&mut self, started: Instant) -> Result<(), ImportError> {
        self.sync_calls = self
            .sync_calls
            .checked_add(1)
            .ok_or(ImportError::Overflow)?;
        self.sync_time_ns = self
            .sync_time_ns
            .checked_add(
                u64::try_from(started.elapsed().as_nanos()).map_err(|_| ImportError::Overflow)?,
            )
            .ok_or(ImportError::Overflow)?;
        Ok(())
    }
}

impl CanonicalBaseReader {
    pub(crate) fn read_region_into(
        &self,
        channel: u32,
        timepoint: u64,
        origin_zyx: [u64; 3],
        extent_zyx: [u64; 3],
        destination: &mut [u8],
    ) -> Result<(), ImportError> {
        if channel >= self.channels
            || timepoint >= self.shape.t()
            || extent_zyx.contains(&0)
            || origin_zyx[0]
                .checked_add(extent_zyx[0])
                .is_none_or(|value| value > self.shape.z())
            || origin_zyx[1]
                .checked_add(extent_zyx[1])
                .is_none_or(|value| value > self.shape.y())
            || origin_zyx[2]
                .checked_add(extent_zyx[2])
                .is_none_or(|value| value > self.shape.x())
        {
            return Err(ImportError::InvalidRequest(
                "canonical checkpoint read region is out of bounds",
            ));
        }
        let width = u64::from(self.dtype.bytes_per_sample());
        let row_bytes = extent_zyx[2]
            .checked_mul(width)
            .ok_or(ImportError::Overflow)?;
        let expected = extent_zyx[0]
            .checked_mul(extent_zyx[1])
            .and_then(|value| value.checked_mul(row_bytes))
            .ok_or(ImportError::Overflow)?;
        if u64::try_from(destination.len()).map_err(|_| ImportError::Overflow)? != expected {
            return Err(ImportError::InvalidRequest(
                "canonical checkpoint read destination has the wrong length",
            ));
        }
        let mut output = 0_usize;
        for z in origin_zyx[0]..origin_zyx[0] + extent_zyx[0] {
            let plane = u64::from(channel)
                .checked_mul(self.shape.t())
                .and_then(|value| value.checked_add(timepoint))
                .and_then(|value| value.checked_mul(self.shape.z()))
                .and_then(|value| value.checked_add(z))
                .ok_or(ImportError::Overflow)?;
            for y in origin_zyx[1]..origin_zyx[1] + extent_zyx[1] {
                let offset = plane
                    .checked_mul(self.plane_bytes)
                    .and_then(|value| {
                        y.checked_mul(self.shape.x())
                            .and_then(|row| row.checked_add(origin_zyx[2]))
                            .and_then(|sample| sample.checked_mul(width))
                            .and_then(|byte| value.checked_add(byte))
                    })
                    .ok_or(ImportError::Overflow)?;
                let count = usize::try_from(row_bytes).map_err(|_| ImportError::Overflow)?;
                let end = output.checked_add(count).ok_or(ImportError::Overflow)?;
                read_exact_at(
                    &self.data,
                    &mut destination[output..end],
                    offset,
                    &self.data_path,
                )?;
                output = end;
            }
        }
        Ok(())
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
            .map_err(|source| io_error("read canonical data checkpoint", path, source))?;
        if read == 0 {
            return Err(ImportError::InvalidCheckpoint(
                "canonical data checkpoint is truncated".to_owned(),
            ));
        }
        offset = offset
            .checked_add(u64::try_from(read).map_err(|_| ImportError::Overflow)?)
            .ok_or(ImportError::Overflow)?;
        destination = &mut destination[read..];
    }
    Ok(())
}

fn state_header(
    binding: CanonicalCacheBinding,
    shape: Shape4D,
    channels: u32,
    dtype: IntensityDType,
) -> Vec<u8> {
    let mut header = Vec::with_capacity(STATE_HEADER_BYTES);
    header.extend_from_slice(SCHEMA);
    header.extend_from_slice(binding.plan_digest.as_bytes());
    header.extend_from_slice(binding.source_fingerprint.as_bytes());
    for dimension in shape.dimensions() {
        header.extend_from_slice(&dimension.to_le_bytes());
    }
    header.extend_from_slice(&u64::from(channels).to_le_bytes());
    header.push(match dtype {
        IntensityDType::Uint8 => 1,
        IntensityDType::Uint16 => 2,
        IntensityDType::Float32 => 3,
    });
    debug_assert_eq!(header.len(), STATE_HEADER_BYTES);
    header
}

fn hash_data_range(
    data: &mut File,
    offset: u64,
    len: u64,
    scratch: &mut [u8],
    path: &Path,
) -> Result<Sha256Digest, ImportError> {
    data.seek(SeekFrom::Start(offset))
        .map_err(|source| io_error("seek canonical data checkpoint", path, source))?;
    let mut remaining = len;
    let mut hasher = Sha256Hasher::new();
    hasher.update(b"mirante4d-canonical-base-batch-1\0");
    hasher.update(offset.to_le_bytes());
    hasher.update(len.to_le_bytes());
    while remaining != 0 {
        let count = usize::try_from(remaining.min(scratch.len() as u64))
            .map_err(|_| ImportError::Overflow)?;
        data.read_exact(&mut scratch[..count])
            .map_err(|source| io_error("read canonical checkpoint batch", path, source))?;
        hasher.update(&scratch[..count]);
        remaining -= count as u64;
    }
    Ok(hasher.finalize())
}

fn entry_state(directory: &OwnedFd, path: &Path, name: &str) -> Result<bool, ImportError> {
    match statat(directory, OsStr::new(name), AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat)
            if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
                && stat.st_nlink == 1 =>
        {
            Ok(true)
        }
        Ok(_) => Err(ImportError::InvalidCheckpoint(
            "canonical checkpoint entries must be singly linked regular files".to_owned(),
        )),
        Err(Errno::NOENT) => Ok(false),
        Err(source) => Err(rustix_io_error(
            "inspect canonical checkpoint entry",
            path,
            source,
        )),
    }
}

fn open_checkpoint_file(
    directory: &OwnedFd,
    path: &Path,
    name: &str,
    flags: OFlags,
    operation: &'static str,
) -> Result<File, ImportError> {
    let descriptor = openat(directory, OsStr::new(name), flags, Mode::RUSR | Mode::WUSR)
        .map_err(|source| rustix_io_error(operation, path, source))?;
    let stat = fstat(&descriptor).map_err(|source| rustix_io_error(operation, path, source))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
        return Err(ImportError::InvalidCheckpoint(
            "canonical checkpoint entries must be singly linked regular files".to_owned(),
        ));
    }
    Ok(File::from(descriptor))
}

fn unlink_if_owned(directory: &OwnedFd, name: &str, file: &File) -> Result<(), ImportError> {
    let held = fstat(file).map_err(|source| {
        rustix_io_error(
            "inspect owned canonical checkpoint file",
            Path::new(name),
            source,
        )
    })?;
    let named =
        statat(directory, OsStr::new(name), AtFlags::SYMLINK_NOFOLLOW).map_err(|source| {
            rustix_io_error(
                "inspect named canonical checkpoint file",
                Path::new(name),
                source,
            )
        })?;
    if held.st_dev != named.st_dev
        || held.st_ino != named.st_ino
        || FileType::from_raw_mode(named.st_mode) != FileType::RegularFile
        || named.st_nlink != 1
    {
        return Err(ImportError::InvalidCheckpoint(
            "canonical checkpoint name no longer identifies its owned file".to_owned(),
        ));
    }
    unlinkat(directory, OsStr::new(name), AtFlags::empty()).map_err(|source| {
        rustix_io_error(
            "remove owned canonical checkpoint file",
            Path::new(name),
            source,
        )
    })
}

fn invalid_or_io(operation: &'static str, path: &Path, source: std::io::Error) -> ImportError {
    if source.kind() == std::io::ErrorKind::UnexpectedEof {
        ImportError::InvalidCheckpoint("canonical checkpoint is truncated".to_owned())
    } else {
        io_error(operation, path, source)
    }
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> ImportError {
    ImportError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

fn rustix_io_error(operation: &'static str, path: &Path, source: Errno) -> ImportError {
    io_error(operation, path, std::io::Error::from(source))
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        os::unix::fs::symlink,
    };

    use mirante4d_identity::Sha256Digest;
    use tempfile::tempdir;

    use super::*;

    fn digest(value: char) -> Sha256Digest {
        Sha256Digest::parse(&value.to_string().repeat(64)).unwrap()
    }

    fn two_forty_mib_planes() -> Shape4D {
        Shape4D::new(1, 2, 1, 40 * 1024 * 1024).unwrap()
    }

    #[test]
    fn resumes_only_a_checksummed_durable_plane_prefix() {
        let directory = tempdir().unwrap();
        let binding = CanonicalCacheBinding::new(digest('1'), digest('2'));
        let shape = Shape4D::new(1, 2, 2, 3).unwrap();
        {
            let mut cache = CanonicalBaseCache::open_or_create(
                directory.path(),
                binding,
                shape,
                1,
                IntensityDType::Uint8,
            )
            .unwrap();
            cache.write_row(0, 0, 0, &[1, 2, 3]).unwrap();
            cache.write_row(0, 1, 0, &[4, 5, 6]).unwrap();
            cache.complete_plane(0).unwrap();
            cache.commit_pending().unwrap();
            cache.write_row(1, 0, 0, &[7, 8, 9]).unwrap();
        }
        let cache = CanonicalBaseCache::open_or_create(
            directory.path(),
            binding,
            shape,
            1,
            IntensityDType::Uint8,
        )
        .unwrap();
        assert_eq!(cache.durable_planes(), 1);
    }

    #[test]
    fn complete_cache_reads_canonical_regions() {
        let directory = tempdir().unwrap();
        let binding = CanonicalCacheBinding::new(digest('3'), digest('4'));
        let shape = Shape4D::new(1, 1, 2, 3).unwrap();
        let mut cache = CanonicalBaseCache::open_or_create(
            directory.path(),
            binding,
            shape,
            1,
            IntensityDType::Uint16,
        )
        .unwrap();
        cache.write_row(0, 0, 0, &[1, 0, 2, 0, 3, 0]).unwrap();
        cache.write_row(0, 1, 0, &[4, 0, 5, 0, 6, 0]).unwrap();
        cache.complete_plane(0).unwrap();
        cache.commit_pending().unwrap();
        let mut result = vec![0; 8];
        cache
            .read_region_into(0, 0, [0, 0, 1], [1, 2, 2], &mut result)
            .unwrap();
        assert_eq!(result, [2, 0, 3, 0, 5, 0, 6, 0]);
    }

    #[test]
    fn partial_final_state_record_is_the_only_discarded_suffix() {
        let directory = tempdir().unwrap();
        let binding = CanonicalCacheBinding::new(digest('5'), digest('6'));
        let shape = Shape4D::new(1, 1, 1, 2).unwrap();
        {
            let mut cache = CanonicalBaseCache::open_or_create(
                directory.path(),
                binding,
                shape,
                1,
                IntensityDType::Uint8,
            )
            .unwrap();
            cache.write_row(0, 0, 0, &[8, 9]).unwrap();
            cache.complete_plane(0).unwrap();
            cache.commit_pending().unwrap();
        }
        OpenOptions::new()
            .append(true)
            .open(directory.path().join(STATE_FILE))
            .unwrap()
            .write_all(&[7; STATE_RECORD_BYTES - 1])
            .unwrap();

        let cache = CanonicalBaseCache::open_or_create(
            directory.path(),
            binding,
            shape,
            1,
            IntensityDType::Uint8,
        )
        .unwrap();
        assert_eq!(cache.durable_planes(), 1);
        assert_eq!(
            cache.diagnostics().unwrap().state_bytes,
            (STATE_HEADER_BYTES + STATE_RECORD_BYTES) as u64
        );
    }

    #[test]
    fn complete_committed_corruption_is_rejected_instead_of_recomputed() {
        let directory = tempdir().unwrap();
        let binding = CanonicalCacheBinding::new(digest('7'), digest('8'));
        let shape = Shape4D::new(1, 2, 1, 2).unwrap();
        {
            let mut cache = CanonicalBaseCache::open_or_create(
                directory.path(),
                binding,
                shape,
                1,
                IntensityDType::Uint8,
            )
            .unwrap();
            cache.write_row(0, 0, 0, &[1, 2]).unwrap();
            cache.complete_plane(0).unwrap();
            cache.commit_pending().unwrap();
            cache.write_row(1, 0, 0, &[3, 4]).unwrap();
            cache.complete_plane(1).unwrap();
            cache.commit_pending().unwrap();
        }
        let mut data = OpenOptions::new()
            .write(true)
            .open(directory.path().join(DATA_FILE))
            .unwrap();
        data.seek(SeekFrom::Start(0)).unwrap();
        data.write_all(&[99]).unwrap();
        data.sync_all().unwrap();

        assert!(matches!(
            CanonicalBaseCache::open_or_create(
                directory.path(),
                binding,
                shape,
                1,
                IntensityDType::Uint8,
            ),
            Err(ImportError::InvalidCheckpoint(reason))
                if reason.contains("committed data")
        ));
    }

    #[test]
    fn wrong_binding_is_rejected_without_a_compatibility_reader() {
        let directory = tempdir().unwrap();
        let shape = Shape4D::new(1, 1, 1, 1).unwrap();
        CanonicalBaseCache::open_or_create(
            directory.path(),
            CanonicalCacheBinding::new(digest('9'), digest('a')),
            shape,
            1,
            IntensityDType::Uint8,
        )
        .unwrap();

        assert!(matches!(
            CanonicalBaseCache::open_or_create(
                directory.path(),
                CanonicalCacheBinding::new(digest('9'), digest('b')),
                shape,
                1,
                IntensityDType::Uint8,
            ),
            Err(ImportError::InvalidCheckpoint(reason))
                if reason.contains("different inputs or schema")
        ));
    }

    #[test]
    fn canonical_names_never_follow_symlinks() {
        let directory = tempdir().unwrap();
        let outside = directory.path().join("outside");
        fs::write(&outside, b"outside").unwrap();
        symlink(&outside, directory.path().join(DATA_FILE)).unwrap();
        symlink(&outside, directory.path().join(STATE_FILE)).unwrap();

        assert!(matches!(
            CanonicalBaseCache::open_or_create(
                directory.path(),
                CanonicalCacheBinding::new(digest('c'), digest('d')),
                Shape4D::new(1, 1, 1, 1).unwrap(),
                1,
                IntensityDType::Uint8,
            ),
            Err(ImportError::InvalidCheckpoint(_))
        ));
        assert_eq!(fs::read(outside).unwrap(), b"outside");
    }

    #[test]
    fn plane_larger_than_one_durability_work_unit_is_rejected_before_creation() {
        let directory = tempdir().unwrap();
        let shape = Shape4D::new(1, 1, 8_193, 8_192).unwrap();
        assert!(matches!(
            CanonicalBaseCache::open_or_create(
                directory.path(),
                CanonicalCacheBinding::new(digest('a'), digest('b')),
                shape,
                1,
                IntensityDType::Uint8,
            ),
            Err(ImportError::UnsupportedSource(reason)) if reason.contains("checkpoint work-unit")
        ));
        assert!(!directory.path().join(DATA_FILE).exists());
        assert!(!directory.path().join(STATE_FILE).exists());
    }

    #[test]
    fn forty_mib_planes_are_split_before_the_second_plane_is_written() {
        let directory = tempdir().unwrap();
        let binding = CanonicalCacheBinding::new(digest('2'), digest('3'));
        let mut cache = CanonicalBaseCache::open_or_create(
            directory.path(),
            binding,
            two_forty_mib_planes(),
            1,
            IntensityDType::Uint8,
        )
        .unwrap();

        cache.write_row(0, 0, 0, &[11]).unwrap();
        let before_first_complete = cache.diagnostics().unwrap();
        assert_eq!(before_first_complete.durable_planes, 0);
        assert_eq!(before_first_complete.pending_planes, 0);
        assert_eq!(before_first_complete.committed_batches, 0);

        cache.complete_plane(0).unwrap();
        let after_first_complete = cache.diagnostics().unwrap();
        assert_eq!(after_first_complete.durable_planes, 0);
        assert_eq!(after_first_complete.pending_planes, 1);
        assert_eq!(after_first_complete.committed_batches, 0);

        // Starting the second plane closes the existing 40 MiB prefix before
        // any second-plane byte can join it. There is never an 80 MiB pending
        // or committed batch.
        cache.write_row(1, 0, 0, &[22]).unwrap();
        let after_write = cache.diagnostics().unwrap();
        assert_eq!(after_write.durable_planes, 1);
        assert_eq!(after_write.pending_planes, 0);
        assert_eq!(after_write.committed_batches, 1);

        cache.complete_plane(1).unwrap();
        let after_complete = cache.diagnostics().unwrap();
        assert_eq!(after_complete.durable_planes, 1);
        assert_eq!(after_complete.pending_planes, 1);
        assert_eq!(after_complete.committed_batches, 1);

        cache.commit_pending().unwrap();
        let complete = cache.diagnostics().unwrap();
        assert_eq!(complete.durable_planes, 2);
        assert_eq!(complete.pending_planes, 0);
        assert_eq!(complete.committed_batches, 2);

        let state = fs::read(directory.path().join(STATE_FILE)).unwrap();
        assert_eq!(state.len(), STATE_HEADER_BYTES + 2 * STATE_RECORD_BYTES);
        let first_end = STATE_HEADER_BYTES + 8;
        let second_start = STATE_HEADER_BYTES + STATE_RECORD_BYTES;
        assert_eq!(
            u64::from_le_bytes(state[STATE_HEADER_BYTES..first_end].try_into().unwrap()),
            1
        );
        assert_eq!(
            u64::from_le_bytes(state[second_start..second_start + 8].try_into().unwrap()),
            2
        );
    }

    #[test]
    fn recovery_discards_only_the_second_forty_mib_plane() {
        let directory = tempdir().unwrap();
        let binding = CanonicalCacheBinding::new(digest('4'), digest('5'));
        let shape = two_forty_mib_planes();
        {
            let mut cache = CanonicalBaseCache::open_or_create(
                directory.path(),
                binding,
                shape,
                1,
                IntensityDType::Uint8,
            )
            .unwrap();
            cache.write_row(0, 0, 0, &[31]).unwrap();
            cache.complete_plane(0).unwrap();
            cache.write_row(1, 0, 0, &[41]).unwrap();
            cache.complete_plane(1).unwrap();

            let before_crash = cache.diagnostics().unwrap();
            assert_eq!(before_crash.durable_planes, 1);
            assert_eq!(before_crash.pending_planes, 1);
            assert_eq!(before_crash.committed_batches, 1);
        }

        let mut recovered = CanonicalBaseCache::open_or_create(
            directory.path(),
            binding,
            shape,
            1,
            IntensityDType::Uint8,
        )
        .unwrap();
        assert_eq!(recovered.durable_planes(), 1);
        assert_eq!(recovered.diagnostics().unwrap().pending_planes, 0);

        recovered.write_row(1, 0, 0, &[51]).unwrap();
        recovered.complete_plane(1).unwrap();
        recovered.commit_pending().unwrap();
        assert_eq!(recovered.durable_planes(), 2);
        assert_eq!(recovered.diagnostics().unwrap().committed_batches, 2);
    }

    #[test]
    fn cleanup_does_not_unlink_a_replaced_canonical_name() {
        let directory = tempdir().unwrap();
        let cache = CanonicalBaseCache::open_or_create(
            directory.path(),
            CanonicalCacheBinding::new(digest('e'), digest('f')),
            Shape4D::new(1, 1, 1, 1).unwrap(),
            1,
            IntensityDType::Uint8,
        )
        .unwrap();
        let data = directory.path().join(DATA_FILE);
        let displaced = directory.path().join("displaced-data");
        fs::rename(&data, &displaced).unwrap();
        fs::write(&data, b"replacement").unwrap();

        cache.cleanup_owned_files();

        assert_eq!(fs::read(data).unwrap(), b"replacement");
        assert!(displaced.exists());
        assert!(!directory.path().join(STATE_FILE).exists());
    }

    #[test]
    fn owner_tick_commits_an_expired_canonical_batch() {
        let directory = tempdir().unwrap();
        let mut cache = CanonicalBaseCache::open_or_create(
            directory.path(),
            CanonicalCacheBinding::new(digest('1'), digest('0')),
            Shape4D::new(1, 1, 1, 1).unwrap(),
            1,
            IntensityDType::Uint8,
        )
        .unwrap();
        cache.write_row(0, 0, 0, &[7]).unwrap();
        cache.complete_plane(0).unwrap();
        cache.pending_started = Some(Instant::now() - BATCH_AGE_MAX);
        cache.commit_expired().unwrap();
        assert_eq!(cache.durable_planes(), 1);
        assert_eq!(cache.diagnostics().unwrap().pending_planes, 0);
    }
}

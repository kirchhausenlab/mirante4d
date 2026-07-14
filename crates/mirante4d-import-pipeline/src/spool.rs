//! Constant-file resumable import spool.

#![cfg_attr(not(test), allow(dead_code))]

use std::{
    ffi::OsStr,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use mirante4d_identity::{Sha256Digest, Sha256Hasher};
use mirante4d_storage::{
    PackedIndexCoordinates, PackedIndexRecord, ShardProfileKind, decode_inner_payload,
    encode_inner_payload,
};
use rustix::{
    fd::OwnedFd,
    fs::{AtFlags, CWD, FileType, Mode, OFlags, fstat, fsync, openat, statat},
    io::Errno,
};

use crate::ImportError;

const HEADER_FILE: &str = "header";
const JOURNAL_FILE: &str = "journal";
const PAYLOAD_FILE: &str = "payload";
const SPOOL_SCHEMA: &[u8] = b"mirante4d-import-spool-1\n";
const HEADER_BYTES: usize = SPOOL_SCHEMA.len() + 32 + 32;

const JOURNAL_RECORD_BYTES: usize = 160;
const JOURNAL_BODY_BYTES: usize = 128;
const PACKED_INDEX_BYTES: usize = 64;
const FLAG_PIXEL_PRESENT: u8 = 1 << 0;
const FLAG_VALIDITY_PRESENT: u8 = 1 << 1;
const KNOWN_FLAGS: u8 = FLAG_PIXEL_PRESENT | FLAG_VALIDITY_PRESENT;
const MISSING_OFFSET: u64 = u64::MAX;

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
#[derive(Clone, Copy, Debug)]
pub(crate) struct SpoolChunkInput<'a> {
    pub(crate) kind: ShardProfileKind,
    pub(crate) decoded: &'a [u8],
}

impl<'a> SpoolChunkInput<'a> {
    pub(crate) const fn new(kind: ShardProfileKind, decoded: &'a [u8]) -> Self {
        Self { kind, decoded }
    }
}

/// One decoded inner chunk recovered from the spool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpoolChunk {
    pub(crate) kind: ShardProfileKind,
    pub(crate) decoded: Vec<u8>,
}

/// One complete recovered work unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpoolWorkUnit {
    pub(crate) key: SpoolWorkUnitKey,
    pub(crate) pixel: Option<SpoolChunk>,
    pub(crate) validity: Option<SpoolChunk>,
    pub(crate) packed_index: [u8; PACKED_INDEX_BYTES],
}

/// Three-file, append-only checkpoint spool.
pub(crate) struct ImportSpool {
    directory_path: PathBuf,
    _directory: OwnedFd,
    journal: File,
    payload: File,
    records: Vec<JournalRecord>,
    payload_bytes: u64,
    writable: bool,
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

impl ImportSpool {
    /// Opens an exact matching checkpoint or creates the three fixed files in
    /// an existing caller-owned directory.
    pub(crate) fn open_or_create(
        directory: &Path,
        binding: SpoolBinding,
        maximum_records: u64,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<Self, ImportError> {
        let directory_path = directory.to_path_buf();
        let directory_fd = openat(CWD, directory, DIRECTORY_OPEN_FLAGS, Mode::empty())
            .map_err(|source| io_error("open checkpoint directory", &directory_path, source))?;

        let states = [
            entry_state(&directory_fd, HEADER_FILE)?,
            entry_state(&directory_fd, JOURNAL_FILE)?,
            entry_state(&directory_fd, PAYLOAD_FILE)?,
        ];
        let all_absent = states.iter().all(|state| *state == EntryState::Absent);
        let all_present = states.iter().all(|state| *state == EntryState::RegularFile);
        if !all_absent && !all_present {
            return invalid_checkpoint("the fixed spool file set is incomplete");
        }

        let (journal, payload) = if all_absent {
            create_files(&directory_fd, &directory_path, binding)?
        } else {
            validate_header(&directory_fd, &directory_path, binding)?;
            (
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
            )
        };

        let (records, payload_bytes) = validate_records(
            &directory_path,
            &journal,
            &payload,
            maximum_records,
            &mut is_cancelled,
        )?;
        Ok(Self {
            directory_path,
            _directory: directory_fd,
            journal,
            payload,
            records,
            payload_bytes,
            writable: true,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub(crate) fn contains(&self, key: SpoolWorkUnitKey) -> bool {
        self.find(key).is_ok()
    }

    pub(crate) fn keys(&self) -> impl ExactSizeIterator<Item = SpoolWorkUnitKey> + '_ {
        self.records.iter().map(|record| record.key)
    }

    /// Encodes and durably appends a new work unit. Existing keys are a no-op.
    /// New keys must follow the canonical order.
    pub(crate) fn append_if_absent(
        &mut self,
        key: SpoolWorkUnitKey,
        pixel: Option<SpoolChunkInput<'_>>,
        validity: Option<SpoolChunkInput<'_>>,
        packed_index: PackedIndexRecord,
    ) -> Result<bool, ImportError> {
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
        validate_input(key, pixel, validity, packed_index)?;

        // Complete every fallible codec operation before touching the files.
        let encoded_pixel = pixel
            .map(|chunk| encode_inner_payload(chunk.kind, chunk.decoded))
            .transpose()?;
        let encoded_validity = validity
            .map(|chunk| encode_inner_payload(chunk.kind, chunk.decoded))
            .transpose()?;
        let (pixel_record, after_pixel) = plan_chunk(
            pixel.map(|chunk| chunk.kind),
            encoded_pixel.as_deref(),
            self.payload_bytes,
        )?;
        let (validity_record, after_validity) = plan_chunk(
            validity.map(|chunk| chunk.kind),
            encoded_validity.as_deref(),
            after_pixel,
        )?;
        let record = JournalRecord {
            key,
            pixel: pixel_record,
            validity: validity_record,
            packed_index: packed_index.encode(),
        };
        let journal_bytes = encode_journal_record(record);

        // Once bytes are appended, any failure poisons this handle. Reopen
        // will either validate the complete unit or reject the checkpoint.
        self.writable = false;
        append_all(
            &mut self.payload,
            encoded_pixel
                .as_deref()
                .into_iter()
                .chain(encoded_validity.as_deref()),
        )
        .map_err(|source| self.io("append checkpoint payload", PAYLOAD_FILE, source))?;
        self.payload
            .sync_all()
            .map_err(|source| self.io("synchronize checkpoint payload", PAYLOAD_FILE, source))?;

        self.journal
            .write_all(&journal_bytes)
            .map_err(|source| self.io("append checkpoint journal", JOURNAL_FILE, source))?;
        self.journal
            .sync_all()
            .map_err(|source| self.io("synchronize checkpoint journal", JOURNAL_FILE, source))?;

        self.records.push(record);
        self.payload_bytes = after_validity;
        self.writable = true;
        Ok(true)
    }

    /// Looks up and checksum-verifies one completed work unit.
    pub(crate) fn read_work_unit(
        &mut self,
        key: SpoolWorkUnitKey,
    ) -> Result<Option<SpoolWorkUnit>, ImportError> {
        let index = match self.find(key) {
            Ok(index) => index,
            Err(_) => return Ok(None),
        };
        let record = self.records[index];
        let pixel = read_chunk(&self.directory_path, &mut self.payload, record.pixel)?;
        let validity = read_chunk(&self.directory_path, &mut self.payload, record.validity)?;
        Ok(Some(SpoolWorkUnit {
            key,
            pixel,
            validity,
            packed_index: record.packed_index,
        }))
    }

    pub(crate) fn read_component(
        &mut self,
        key: SpoolWorkUnitKey,
        validity: bool,
    ) -> Result<Option<SpoolChunk>, ImportError> {
        let index = match self.find(key) {
            Ok(index) => index,
            Err(_) => return Ok(None),
        };
        let record = self.records[index];
        read_chunk(
            &self.directory_path,
            &mut self.payload,
            if validity {
                record.validity
            } else {
                record.pixel
            },
        )
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

    fn io(&self, operation: &'static str, file_name: &str, source: io::Error) -> ImportError {
        ImportError::Io {
            operation,
            path: self.directory_path.join(file_name),
            source,
        }
    }
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
) -> Result<(File, File), ImportError> {
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
    header.sync_all().map_err(|source| ImportError::Io {
        operation: "synchronize spool header",
        path: directory_path.join(HEADER_FILE),
        source,
    })?;

    let journal = create_file(
        directory,
        directory_path,
        JOURNAL_FILE,
        "create spool journal",
    )?;
    journal.sync_all().map_err(|source| ImportError::Io {
        operation: "synchronize spool journal",
        path: directory_path.join(JOURNAL_FILE),
        source,
    })?;
    let payload = create_file(
        directory,
        directory_path,
        PAYLOAD_FILE,
        "create spool payload",
    )?;
    payload.sync_all().map_err(|source| ImportError::Io {
        operation: "synchronize spool payload",
        path: directory_path.join(PAYLOAD_FILE),
        source,
    })?;
    fsync(directory)
        .map_err(|source| io_error("synchronize checkpoint directory", directory_path, source))?;
    Ok((journal, payload))
}

fn validate_header(
    directory: &OwnedFd,
    directory_path: &Path,
    binding: SpoolBinding,
) -> Result<(), ImportError> {
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
    Ok(())
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

fn validate_records(
    directory_path: &Path,
    journal: &File,
    payload: &File,
    maximum_records: u64,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<(Vec<JournalRecord>, u64), ImportError> {
    let journal_bytes = regular_file_length(journal, "journal")?;
    let payload_bytes = regular_file_length(payload, "payload")?;
    let record_bytes = u64::try_from(JOURNAL_RECORD_BYTES).map_err(|_| ImportError::Overflow)?;
    let complete_journal_bytes = journal_bytes - journal_bytes % record_bytes;
    let mut validated_journal_bytes = complete_journal_bytes;
    let record_count = complete_journal_bytes / record_bytes;
    if record_count > maximum_records {
        return invalid_checkpoint("the journal exceeds this import plan's work-unit bound");
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
    for record_index in 0..record_count {
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
        let digest = Sha256Hasher::digest(&bytes[..JOURNAL_BODY_BYTES]);
        if digest.as_bytes() != &bytes[JOURNAL_BODY_BYTES..] {
            if record_index + 1 == record_count {
                validated_journal_bytes = record_index
                    .checked_mul(record_bytes)
                    .ok_or(ImportError::Overflow)?;
                break;
            }
            return invalid_checkpoint("a non-final journal record has an invalid checksum");
        }
        let (record, next_offset) =
            decode_journal_record(&bytes, expected_payload_offset, payload_bytes)?;
        if previous_key.is_some_and(|previous| record.key <= previous) {
            return invalid_checkpoint("journal work-unit keys are not unique and ordered");
        }
        verify_chunk(directory_path, &mut payload, record.pixel)?;
        verify_chunk(directory_path, &mut payload, record.validity)?;
        expected_payload_offset = next_offset;
        previous_key = Some(record.key);
        records.push(record);
    }
    if is_cancelled() {
        return Err(ImportError::Cancelled);
    }

    // Payload is synchronized before its journal record. A process loss can
    // therefore leave an incomplete journal row or an unreferenced payload
    // tail. Once the durable prefix has validated, discard only those tails.
    if validated_journal_bytes != journal_bytes {
        journal
            .set_len(validated_journal_bytes)
            .map_err(|source| ImportError::Io {
                operation: "truncate interrupted checkpoint journal append",
                path: directory_path.join(JOURNAL_FILE),
                source,
            })?;
        journal.sync_all().map_err(|source| ImportError::Io {
            operation: "synchronize recovered checkpoint journal",
            path: directory_path.join(JOURNAL_FILE),
            source,
        })?;
    }
    if expected_payload_offset < payload_bytes {
        payload
            .set_len(expected_payload_offset)
            .map_err(|source| ImportError::Io {
                operation: "truncate interrupted checkpoint payload append",
                path: directory_path.join(PAYLOAD_FILE),
                source,
            })?;
        payload.sync_all().map_err(|source| ImportError::Io {
            operation: "synchronize recovered checkpoint payload",
            path: directory_path.join(PAYLOAD_FILE),
            source,
        })?;
    }
    Ok((records, expected_payload_offset))
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

fn validate_input(
    key: SpoolWorkUnitKey,
    pixel: Option<SpoolChunkInput<'_>>,
    validity: Option<SpoolChunkInput<'_>>,
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
    if pixel.is_some_and(|chunk| !is_pixel_kind(chunk.kind)) {
        return Err(ImportError::InvalidRequest(
            "a spool pixel chunk must use a pixel storage kind",
        ));
    }
    if validity.is_some_and(|chunk| !is_validity_kind(chunk.kind)) {
        return Err(ImportError::InvalidRequest(
            "a spool validity chunk must use a validity storage kind",
        ));
    }
    if pixel
        .zip(validity)
        .is_some_and(|(pixel, validity)| is_2d(pixel.kind) != is_2d(validity.kind))
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
) -> Result<(), ImportError> {
    let Some(record) = record else {
        return Ok(());
    };
    let encoded = read_encoded(directory_path, payload, record)?;
    decode_inner_payload(record.kind, &encoded).map_err(|error| {
        ImportError::InvalidCheckpoint(format!("an encoded chunk is invalid: {error}"))
    })?;
    Ok(())
}

fn read_chunk(
    directory_path: &Path,
    payload: &mut File,
    record: Option<ChunkRecord>,
) -> Result<Option<SpoolChunk>, ImportError> {
    let Some(record) = record else {
        return Ok(None);
    };
    let encoded = read_encoded(directory_path, payload, record)?;
    let decoded = decode_inner_payload(record.kind, &encoded).map_err(|error| {
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
        fs,
        io::{Seek, SeekFrom, Write},
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

    #[test]
    fn creates_three_files_and_resumes_ordered_complete_work() {
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
        drop(spool);

        let entries = fs::read_dir(&checkpoint).unwrap().count();
        assert_eq!(entries, 3);
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
        for key in [first, second] {
            spool
                .append_if_absent(
                    key,
                    Some(SpoolChunkInput::new(kind, &bytes)),
                    None,
                    packed(key, 6, 1, true, false),
                )
                .unwrap();
        }
        drop(spool);

        let first_payload_bytes = {
            let journal = fs::read(checkpoint.join(JOURNAL_FILE)).unwrap();
            let first: [u8; JOURNAL_RECORD_BYTES] =
                journal[..JOURNAL_RECORD_BYTES].try_into().unwrap();
            decode_journal_record(
                &first,
                0,
                fs::metadata(checkpoint.join(PAYLOAD_FILE)).unwrap().len(),
            )
            .unwrap()
            .1
        };
        let journal_path = checkpoint.join(JOURNAL_FILE);
        let mut journal = fs::read(&journal_path).unwrap();
        journal.pop();
        fs::write(&journal_path, journal).unwrap();
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
        for key in [first, second] {
            spool
                .append_if_absent(key, None, None, packed(key, 6, 0, false, false))
                .unwrap();
        }
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

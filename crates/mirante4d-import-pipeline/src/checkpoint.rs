//! Compact control state for the final-layout import checkpoint.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
};

use mirante4d_identity::{Sha256Digest, Sha256Hasher};

use crate::{
    ImportError, ImportOptions, NoDataPolicy, NoDataValueRule,
    model::{ResolvedAutomaticNoDataMask, ResolvedNoDataPolicy, ResolvedNoDataValue},
};

pub(crate) const CONTROL_DIRECTORY: &str = ".mirante4d-import-control";
const UNIT_CACHE_DIRECTORY_PREFIX: &str = "unit-cache-";
const UNIT_SPOOL_DIRECTORY_PREFIX: &str = "unit-spool-";
const NO_DATA_FILE: &str = "no-data-policy";
const NO_DATA_PENDING_FILE: &str = "no-data-policy.pending";
const UNIT_JOURNAL_FILE: &str = "unit-journal";
const DECODED_DIGEST_FILE: &str = "decoded-digests";
const PACKED_RECORD_FILE: &str = "packed-records";
const NO_DATA_SCHEMA: &[u8] = b"mirante4d-resolved-no-data-v1\0";
const UNIT_JOURNAL_SCHEMA: &[u8] = b"mirante4d-temporal-unit-journal-v1\0";

pub(crate) fn control_directory(stage: &Path) -> PathBuf {
    stage.join(CONTROL_DIRECTORY)
}

pub(crate) fn unit_cache_directory(stage: &Path, ordinal: u64) -> PathBuf {
    control_directory(stage).join(format!("{UNIT_CACHE_DIRECTORY_PREFIX}{ordinal:016x}"))
}

pub(crate) fn unit_spool_directory(stage: &Path, ordinal: u64) -> PathBuf {
    control_directory(stage).join(format!("{UNIT_SPOOL_DIRECTORY_PREFIX}{ordinal:016x}"))
}

pub(crate) fn remove_completed_unit_scratch(
    stage: &Path,
    completed_units: u64,
) -> Result<(), ImportError> {
    let control = control_directory(stage);
    for entry in fs::read_dir(&control)
        .map_err(|source| io_error("enumerate import control", &control, source))?
    {
        let entry =
            entry.map_err(|source| io_error("read import control entry", &control, source))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return invalid_checkpoint("import control contains a non-UTF-8 entry");
        };
        let ordinal = [UNIT_CACHE_DIRECTORY_PREFIX, UNIT_SPOOL_DIRECTORY_PREFIX]
            .into_iter()
            .find_map(|prefix| name.strip_prefix(prefix))
            .and_then(|hex| u64::from_str_radix(hex, 16).ok());
        let Some(ordinal) = ordinal else {
            continue;
        };
        if ordinal >= completed_units {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|source| io_error("inspect completed unit scratch", &entry.path(), source))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return invalid_checkpoint("completed unit scratch is not a real directory");
        }
        fs::remove_dir_all(entry.path())
            .map_err(|source| io_error("remove completed unit scratch", &entry.path(), source))?;
    }
    Ok(())
}

pub(crate) fn store_no_data_policy(
    stage: &Path,
    options: &ImportOptions,
    policy: &ResolvedNoDataPolicy,
) -> Result<(), ImportError> {
    let control = control_directory(stage);
    let path = control.join(NO_DATA_FILE);
    let pending = control.join(NO_DATA_PENDING_FILE);
    let expected = encode_no_data(options, policy)?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            return validate_no_data_checkpoint(&path, &expected);
        }
        Ok(_) => return invalid_checkpoint("resolved no-data checkpoint is not a regular file"),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(io_error(
                "inspect resolved no-data checkpoint",
                &path,
                source,
            ));
        }
    }
    match fs::symlink_metadata(&pending) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(&pending).map_err(|source| {
                io_error(
                    "remove incomplete resolved no-data checkpoint",
                    &pending,
                    source,
                )
            })?;
        }
        Ok(_) => return invalid_checkpoint("pending no-data checkpoint is not a regular file"),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(io_error(
                "inspect pending resolved no-data checkpoint",
                &pending,
                source,
            ));
        }
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)
        .map_err(|source| {
            io_error(
                "create pending resolved no-data checkpoint",
                &pending,
                source,
            )
        })?;
    file.write_all(&expected).map_err(|source| {
        io_error(
            "write pending resolved no-data checkpoint",
            &pending,
            source,
        )
    })?;
    file.sync_data().map_err(|source| {
        durability_error(
            "synchronize pending resolved no-data checkpoint",
            &pending,
            source,
        )
    })?;
    drop(file);
    match fs::hard_link(&pending, &path) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            validate_no_data_checkpoint(&path, &expected)?;
        }
        Err(source) => {
            return Err(io_error(
                "install resolved no-data checkpoint",
                &path,
                source,
            ));
        }
    }
    sync_parent(&path)?;
    fs::remove_file(&pending).map_err(|source| {
        io_error(
            "remove installed no-data checkpoint staging file",
            &pending,
            source,
        )
    })?;
    sync_parent(&path)
}

fn validate_no_data_checkpoint(path: &Path, expected: &[u8]) -> Result<(), ImportError> {
    let actual = fs::read(path)
        .map_err(|source| io_error("read resolved no-data checkpoint", path, source))?;
    if actual == expected {
        Ok(())
    } else {
        Err(ImportError::InvalidCheckpoint(
            "resolved no-data checkpoint differs from this import".to_owned(),
        ))
    }
}

pub(crate) fn load_no_data_policy(
    stage: &Path,
    options: &ImportOptions,
) -> Result<Option<ResolvedNoDataPolicy>, ImportError> {
    let path = control_directory(stage).join(NO_DATA_FILE);
    match fs::read(&path) {
        Ok(bytes) => decode_no_data(options, &bytes).map(Some),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error("read resolved no-data checkpoint", &path, source)),
    }
}

fn encode_no_data(
    options: &ImportOptions,
    policy: &ResolvedNoDataPolicy,
) -> Result<Vec<u8>, ImportError> {
    if policy.request() != options.no_data {
        return Err(ImportError::InvalidRequest(
            "resolved no-data checkpoint request differs from import options",
        ));
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(NO_DATA_SCHEMA);
    bytes.extend_from_slice(options.inspection.source_fingerprint.as_bytes());
    for dimension in options.inspection.shape.dimensions() {
        bytes.extend_from_slice(&dimension.to_le_bytes());
    }
    bytes.extend_from_slice(&options.inspection.channels.to_le_bytes());
    bytes.push(dtype_tag(options));
    let request = policy.request();
    bytes.push(match request.and_then(NoDataPolicy::value_rule) {
        None => 0,
        Some(NoDataValueRule::Automatic) => 1,
        Some(NoDataValueRule::ManualUint8(_)) => 2,
    });
    bytes.push(u8::from(
        request.is_some_and(NoDataPolicy::hides_constant_z_planes),
    ));
    let value = policy.value();
    bytes.push(match value {
        None => 0,
        Some(ResolvedNoDataValue::Uint8(_)) => 1,
        Some(ResolvedNoDataValue::Uint16(_)) => 2,
        Some(ResolvedNoDataValue::Float32Bits(_)) => 3,
    });
    bytes.extend_from_slice(
        &value
            .map_or(0, ResolvedNoDataValue::canonical_bits)
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&policy.base_depth().to_le_bytes());
    bytes.extend_from_slice(
        &u64::try_from(policy.constant_z_planes().len())
            .map_err(|_| ImportError::Overflow)?
            .to_le_bytes(),
    );
    for z in policy.constant_z_planes() {
        bytes.extend_from_slice(&z.to_le_bytes());
    }
    if let Some(mask) = policy.automatic_mask() {
        bytes.push(1);
        for dimension in mask.shape_zyx() {
            bytes.extend_from_slice(&dimension.to_le_bytes());
        }
        bytes.extend_from_slice(
            &u64::try_from(mask.packed_bits().len())
                .map_err(|_| ImportError::Overflow)?
                .to_le_bytes(),
        );
        bytes.extend_from_slice(mask.packed_bits());
    } else {
        bytes.push(0);
    }
    let digest = Sha256Hasher::digest(&bytes);
    bytes.extend_from_slice(digest.as_bytes());
    Ok(bytes)
}

fn decode_no_data(
    options: &ImportOptions,
    bytes: &[u8],
) -> Result<ResolvedNoDataPolicy, ImportError> {
    if bytes.len() < NO_DATA_SCHEMA.len() + 32 + 32 || !bytes.starts_with(NO_DATA_SCHEMA) {
        return invalid_checkpoint("resolved no-data checkpoint schema is invalid");
    }
    let (body, checksum) = bytes.split_at(bytes.len() - 32);
    if Sha256Hasher::digest(body).as_bytes() != checksum {
        return invalid_checkpoint("resolved no-data checkpoint checksum failed");
    }
    let mut offset = NO_DATA_SCHEMA.len();
    if take(body, &mut offset, 32)? != options.inspection.source_fingerprint.as_bytes() {
        return invalid_checkpoint("resolved no-data checkpoint source differs");
    }
    for expected in options.inspection.shape.dimensions() {
        if take_u64(body, &mut offset)? != expected {
            return invalid_checkpoint("resolved no-data checkpoint shape differs");
        }
    }
    if take_u32(body, &mut offset)? != options.inspection.channels
        || take_u8(body, &mut offset)? != dtype_tag(options)
    {
        return invalid_checkpoint("resolved no-data checkpoint source type differs");
    }
    let rule_tag = take_u8(body, &mut offset)?;
    let hide = take_u8(body, &mut offset)? != 0;
    let value_tag = take_u8(body, &mut offset)?;
    let value_bits = take_u32(body, &mut offset)?;
    let base_depth = take_u64(body, &mut offset)?;
    let constant_count =
        usize::try_from(take_u64(body, &mut offset)?).map_err(|_| ImportError::Overflow)?;
    if constant_count > usize::try_from(base_depth).unwrap_or(usize::MAX) {
        return invalid_checkpoint("resolved no-data constant-plane count is invalid");
    }
    let mut constant_z_planes = Vec::with_capacity(constant_count);
    for _ in 0..constant_count {
        constant_z_planes.push(take_u64(body, &mut offset)?);
    }
    let mask = match take_u8(body, &mut offset)? {
        0 => None,
        1 => {
            let shape = [
                take_u64(body, &mut offset)?,
                take_u64(body, &mut offset)?,
                take_u64(body, &mut offset)?,
            ];
            let length =
                usize::try_from(take_u64(body, &mut offset)?).map_err(|_| ImportError::Overflow)?;
            let bits = take(body, &mut offset, length)?.to_vec();
            Some(
                ResolvedAutomaticNoDataMask::new(shape, bits)
                    .map_err(ImportError::InvalidRequest)?,
            )
        }
        _ => return invalid_checkpoint("resolved no-data mask tag is invalid"),
    };
    if offset != body.len() {
        return invalid_checkpoint("resolved no-data checkpoint has trailing bytes");
    }
    let request = match rule_tag {
        0 if !hide => None,
        0 => Some(NoDataPolicy::new(None, true)),
        1 => Some(NoDataPolicy::new(Some(NoDataValueRule::Automatic), hide)),
        2 => Some(NoDataPolicy::new(
            Some(NoDataValueRule::ManualUint8(value_bits as u8)),
            hide,
        )),
        _ => return invalid_checkpoint("resolved no-data request tag is invalid"),
    };
    if request != options.no_data {
        return invalid_checkpoint("resolved no-data request differs from reviewed options");
    }
    let value = match value_tag {
        0 => None,
        1 => Some(ResolvedNoDataValue::Uint8(value_bits as u8)),
        2 => Some(ResolvedNoDataValue::Uint16(value_bits as u16)),
        3 => Some(ResolvedNoDataValue::Float32Bits(value_bits)),
        _ => return invalid_checkpoint("resolved no-data value tag is invalid"),
    };
    ResolvedNoDataPolicy::new(request, value, mask, constant_z_planes, base_depth)
        .map_err(|reason| ImportError::InvalidCheckpoint(reason.to_owned()))
}

fn dtype_tag(options: &ImportOptions) -> u8 {
    match options.inspection.dtype {
        mirante4d_domain::IntensityDType::Uint8 => 1,
        mirante4d_domain::IntensityDType::Uint16 => 2,
        mirante4d_domain::IntensityDType::Float32 => 3,
    }
}

pub(crate) struct UnitCompletion {
    pub(crate) ordinal: u64,
    pub(crate) timepoint: u64,
    pub(crate) channel: u32,
    pub(crate) decoded_digest: Sha256Digest,
    pub(crate) scientific_checkpoint: Vec<u8>,
}

pub(crate) struct UnitJournal {
    path: PathBuf,
    file: File,
    decoded_digest_path: PathBuf,
    decoded_digests: File,
    timepoints: u64,
    channels: u32,
    record_count: u64,
    latest_scientific_checkpoints: BTreeMap<u32, Vec<u8>>,
}

impl UnitJournal {
    pub(crate) fn open_or_create(
        stage: &Path,
        binding: Sha256Digest,
        timepoints: u64,
        channels: u32,
    ) -> Result<Self, ImportError> {
        let total_units = timepoints
            .checked_mul(u64::from(channels))
            .ok_or(ImportError::Overflow)?;
        if total_units == 0 {
            return Err(ImportError::InvalidRequest(
                "temporal-unit journal requires positive T and C",
            ));
        }
        let path = control_directory(stage).join(UNIT_JOURNAL_FILE);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| io_error("open temporal-unit journal", &path, source))?;
        let mut header = Vec::new();
        header.extend_from_slice(UNIT_JOURNAL_SCHEMA);
        header.extend_from_slice(binding.as_bytes());
        header.extend_from_slice(Sha256Hasher::digest(&header).as_bytes());
        let length = file
            .metadata()
            .map_err(|source| io_error("inspect temporal-unit journal", &path, source))?
            .len();
        if length == 0 {
            file.write_all(&header)
                .map_err(|source| io_error("write temporal-unit journal header", &path, source))?;
            file.sync_data().map_err(|source| {
                durability_error("synchronize temporal-unit journal header", &path, source)
            })?;
            sync_parent(&path)?;
        } else {
            let mut actual = vec![0_u8; header.len()];
            file.read_exact(&mut actual)
                .map_err(|source| io_error("read temporal-unit journal header", &path, source))?;
            if actual != header {
                return invalid_checkpoint("temporal-unit journal belongs to different inputs");
            }
        }
        let decoded_digest_path = control_directory(stage).join(DECODED_DIGEST_FILE);
        let decoded_digest_bytes = total_units.checked_mul(32).ok_or(ImportError::Overflow)?;
        let decoded_digests = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&decoded_digest_path)
            .map_err(|source| {
                io_error(
                    "open decoded-unit digest store",
                    &decoded_digest_path,
                    source,
                )
            })?;
        let actual_digest_bytes = decoded_digests
            .metadata()
            .map_err(|source| {
                io_error(
                    "inspect decoded-unit digest store",
                    &decoded_digest_path,
                    source,
                )
            })?
            .len();
        if actual_digest_bytes == 0 {
            decoded_digests
                .set_len(decoded_digest_bytes)
                .map_err(|source| {
                    io_error(
                        "size decoded-unit digest store",
                        &decoded_digest_path,
                        source,
                    )
                })?;
            decoded_digests.sync_data().map_err(|source| {
                durability_error(
                    "synchronize decoded-unit digest store",
                    &decoded_digest_path,
                    source,
                )
            })?;
            sync_parent(&decoded_digest_path)?;
        } else if actual_digest_bytes != decoded_digest_bytes {
            return invalid_checkpoint("decoded-unit digest store has the wrong length");
        }
        let mut record_count = 0_u64;
        let mut latest_scientific_checkpoints = BTreeMap::new();
        let mut durable_end = u64::try_from(header.len()).map_err(|_| ImportError::Overflow)?;
        file.seek(SeekFrom::Start(durable_end))
            .map_err(|source| io_error("position temporal-unit journal", &path, source))?;
        loop {
            let mut prefix = [0_u8; 8 + 8 + 4 + 32 + 4];
            match file.read_exact(&mut prefix) {
                Ok(()) => {}
                Err(source) if source.kind() == io::ErrorKind::UnexpectedEof => {
                    file.set_len(durable_end).map_err(|source| {
                        io_error("truncate incomplete temporal-unit journal", &path, source)
                    })?;
                    break;
                }
                Err(source) => {
                    return Err(io_error("read temporal-unit journal", &path, source));
                }
            }
            let mut offset = 0;
            let ordinal = take_u64(&prefix, &mut offset)?;
            let timepoint = take_u64(&prefix, &mut offset)?;
            let channel = take_u32(&prefix, &mut offset)?;
            let decoded_digest = Sha256Digest::from_bytes(
                take(&prefix, &mut offset, 32)?
                    .try_into()
                    .expect("checked digest"),
            );
            let checkpoint_len = usize::try_from(take_u32(&prefix, &mut offset)?)
                .map_err(|_| ImportError::Overflow)?;
            let expected_timepoint = ordinal / u64::from(channels);
            let expected_channel =
                u32::try_from(ordinal % u64::from(channels)).map_err(|_| ImportError::Overflow)?;
            if ordinal != record_count
                || ordinal >= total_units
                || timepoint != expected_timepoint
                || channel != expected_channel
                || checkpoint_len > 4 * 1024 * 1024
            {
                return invalid_checkpoint("temporal-unit journal record is malformed");
            }
            let mut suffix = vec![0_u8; checkpoint_len + 32];
            if let Err(source) = file.read_exact(&mut suffix) {
                if source.kind() == io::ErrorKind::UnexpectedEof {
                    file.set_len(durable_end).map_err(|source| {
                        io_error("truncate incomplete temporal-unit journal", &path, source)
                    })?;
                    break;
                }
                return Err(io_error("read temporal-unit journal record", &path, source));
            }
            let mut framed = prefix.to_vec();
            framed.extend_from_slice(&suffix[..checkpoint_len]);
            if Sha256Hasher::digest(&framed).as_bytes() != &suffix[checkpoint_len..] {
                return invalid_checkpoint("temporal-unit journal checksum failed");
            }
            let mut stored_digest = [0_u8; 32];
            decoded_digests
                .read_exact_at(
                    &mut stored_digest,
                    decoded_digest_offset(timepoints, channel, timepoint)?,
                )
                .map_err(|source| {
                    io_error(
                        "read decoded-unit digest store",
                        &decoded_digest_path,
                        source,
                    )
                })?;
            if stored_digest != *decoded_digest.as_bytes() {
                return invalid_checkpoint(
                    "decoded-unit digest store disagrees with the temporal-unit journal",
                );
            }
            latest_scientific_checkpoints.insert(channel, suffix[..checkpoint_len].to_vec());
            record_count = record_count.checked_add(1).ok_or(ImportError::Overflow)?;
            durable_end = file
                .stream_position()
                .map_err(|source| io_error("locate temporal-unit journal", &path, source))?;
        }
        file.seek(SeekFrom::End(0))
            .map_err(|source| io_error("position temporal-unit journal", &path, source))?;
        Ok(Self {
            path,
            file,
            decoded_digest_path,
            decoded_digests,
            timepoints,
            channels,
            record_count,
            latest_scientific_checkpoints,
        })
    }

    pub(crate) const fn completed_units(&self) -> u64 {
        self.record_count
    }

    pub(crate) fn latest_scientific_checkpoint(&self, channel: u32) -> Option<&[u8]> {
        self.latest_scientific_checkpoints
            .get(&channel)
            .map(Vec::as_slice)
    }

    pub(crate) fn append(&mut self, completion: UnitCompletion) -> Result<(), ImportError> {
        if completion.ordinal != self.record_count {
            return Err(ImportError::InvalidCheckpoint(
                "temporal unit completed out of canonical order".to_owned(),
            ));
        }
        let expected_timepoint = completion.ordinal / u64::from(self.channels);
        let expected_channel = u32::try_from(completion.ordinal % u64::from(self.channels))
            .map_err(|_| ImportError::Overflow)?;
        if completion.timepoint != expected_timepoint
            || completion.channel != expected_channel
            || completion.timepoint >= self.timepoints
        {
            return Err(ImportError::InvalidCheckpoint(
                "temporal unit coordinates differ from canonical T/C order".to_owned(),
            ));
        }
        self.decoded_digests
            .write_all_at(
                completion.decoded_digest.as_bytes(),
                decoded_digest_offset(self.timepoints, completion.channel, completion.timepoint)?,
            )
            .map_err(|source| {
                io_error(
                    "write decoded-unit digest store",
                    &self.decoded_digest_path,
                    source,
                )
            })?;
        self.decoded_digests.sync_data().map_err(|source| {
            durability_error(
                "synchronize decoded-unit digest store",
                &self.decoded_digest_path,
                source,
            )
        })?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&completion.ordinal.to_le_bytes());
        bytes.extend_from_slice(&completion.timepoint.to_le_bytes());
        bytes.extend_from_slice(&completion.channel.to_le_bytes());
        bytes.extend_from_slice(completion.decoded_digest.as_bytes());
        bytes.extend_from_slice(
            &u32::try_from(completion.scientific_checkpoint.len())
                .map_err(|_| ImportError::Overflow)?
                .to_le_bytes(),
        );
        bytes.extend_from_slice(&completion.scientific_checkpoint);
        bytes.extend_from_slice(Sha256Hasher::digest(&bytes).as_bytes());
        self.file
            .write_all(&bytes)
            .map_err(|source| io_error("append temporal-unit journal", &self.path, source))?;
        self.file.sync_data().map_err(|source| {
            durability_error("synchronize temporal-unit journal", &self.path, source)
        })?;
        self.record_count = self
            .record_count
            .checked_add(1)
            .ok_or(ImportError::Overflow)?;
        self.latest_scientific_checkpoints
            .insert(completion.channel, completion.scientific_checkpoint);
        Ok(())
    }

    pub(crate) fn read_decoded_digest(
        &self,
        channel: u32,
        timepoint: u64,
    ) -> Result<Sha256Digest, ImportError> {
        if channel >= self.channels || timepoint >= self.timepoints {
            return Err(ImportError::Overflow);
        }
        let mut bytes = [0_u8; 32];
        self.decoded_digests
            .read_exact_at(
                &mut bytes,
                decoded_digest_offset(self.timepoints, channel, timepoint)?,
            )
            .map_err(|source| {
                io_error(
                    "read decoded-unit digest store",
                    &self.decoded_digest_path,
                    source,
                )
            })?;
        Ok(Sha256Digest::from_bytes(bytes))
    }
}

fn decoded_digest_offset(
    timepoints: u64,
    channel: u32,
    timepoint: u64,
) -> Result<u64, ImportError> {
    u64::from(channel)
        .checked_mul(timepoints)
        .and_then(|value| value.checked_add(timepoint))
        .and_then(|value| value.checked_mul(32))
        .ok_or(ImportError::Overflow)
}

pub(crate) struct PackedRecordStore {
    path: PathBuf,
    file: File,
    record_count: u64,
}

impl PackedRecordStore {
    pub(crate) fn open_or_create(stage: &Path, record_count: u64) -> Result<Self, ImportError> {
        let path = control_directory(stage).join(PACKED_RECORD_FILE);
        let length = record_count
            .checked_mul(mirante4d_storage::PACKED_INDEX_RECORD_BYTES)
            .ok_or(ImportError::Overflow)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| io_error("open packed-index control store", &path, source))?;
        let current = file
            .metadata()
            .map_err(|source| io_error("inspect packed-index control store", &path, source))?
            .len();
        if current == 0 {
            file.set_len(length)
                .map_err(|source| io_error("size packed-index control store", &path, source))?;
        } else if current != length {
            return invalid_checkpoint("packed-index control store has the wrong length");
        }
        Ok(Self {
            path,
            file,
            record_count,
        })
    }

    pub(crate) fn write(&self, ordinal: u64, bytes: &[u8; 64]) -> Result<(), ImportError> {
        if ordinal >= self.record_count {
            return Err(ImportError::Overflow);
        }
        self.file
            .write_all_at(bytes, ordinal.checked_mul(64).ok_or(ImportError::Overflow)?)
            .map_err(|source| io_error("write packed-index control record", &self.path, source))
    }

    pub(crate) fn read(&self, ordinal: u64, bytes: &mut [u8; 64]) -> Result<(), ImportError> {
        if ordinal >= self.record_count {
            return Err(ImportError::Overflow);
        }
        self.file
            .read_exact_at(bytes, ordinal.checked_mul(64).ok_or(ImportError::Overflow)?)
            .map_err(|source| io_error("read packed-index control record", &self.path, source))
    }

    pub(crate) fn sync(&self) -> Result<(), ImportError> {
        self.file.sync_data().map_err(|source| {
            durability_error("synchronize packed-index control store", &self.path, source)
        })
    }

    pub(crate) fn remove(self) {
        drop(self.file);
        let _ = fs::remove_file(self.path);
    }
}

fn take<'a>(bytes: &'a [u8], offset: &mut usize, count: usize) -> Result<&'a [u8], ImportError> {
    let end = offset.checked_add(count).ok_or(ImportError::Overflow)?;
    let value = bytes.get(*offset..end).ok_or_else(|| {
        ImportError::InvalidCheckpoint("checkpoint record is truncated".to_owned())
    })?;
    *offset = end;
    Ok(value)
}

fn take_u8(bytes: &[u8], offset: &mut usize) -> Result<u8, ImportError> {
    Ok(take(bytes, offset, 1)?[0])
}

fn take_u32(bytes: &[u8], offset: &mut usize) -> Result<u32, ImportError> {
    Ok(u32::from_le_bytes(
        take(bytes, offset, 4)?.try_into().expect("checked u32"),
    ))
}

fn take_u64(bytes: &[u8], offset: &mut usize) -> Result<u64, ImportError> {
    Ok(u64::from_le_bytes(
        take(bytes, offset, 8)?.try_into().expect("checked u64"),
    ))
}

fn invalid_checkpoint<T>(reason: &str) -> Result<T, ImportError> {
    Err(ImportError::InvalidCheckpoint(reason.to_owned()))
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> ImportError {
    ImportError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

fn durability_error(operation: &'static str, path: &Path, source: io::Error) -> ImportError {
    ImportError::CheckpointDurabilityIndeterminate {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

fn sync_parent(path: &Path) -> Result<(), ImportError> {
    let parent = path.parent().ok_or(ImportError::InvalidRequest(
        "checkpoint control file must have a parent",
    ))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| {
            durability_error("synchronize checkpoint control directory", parent, source)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_stage() -> (tempfile::TempDir, PathBuf) {
        let temporary = tempfile::tempdir().unwrap();
        let stage = temporary.path().join("stage");
        fs::create_dir_all(control_directory(&stage)).unwrap();
        (temporary, stage)
    }

    fn completion() -> UnitCompletion {
        UnitCompletion {
            ordinal: 0,
            timepoint: 0,
            channel: 0,
            decoded_digest: Sha256Hasher::digest(b"decoded unit"),
            scientific_checkpoint: b"bounded private hash state".to_vec(),
        }
    }

    #[test]
    fn unit_journal_reopens_a_durable_prefix_and_truncates_a_torn_suffix() {
        let (_temporary, stage) = test_stage();
        let binding = Sha256Hasher::digest(b"plan");
        let mut journal = UnitJournal::open_or_create(&stage, binding, 1, 1).unwrap();
        journal.append(completion()).unwrap();
        let path = journal.path.clone();
        drop(journal);
        let durable_length = fs::metadata(&path).unwrap().len();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"torn")
            .unwrap();

        let journal = UnitJournal::open_or_create(&stage, binding, 1, 1).unwrap();
        assert_eq!(journal.completed_units(), 1);
        assert_eq!(
            journal.read_decoded_digest(0, 0).unwrap(),
            completion().decoded_digest
        );
        assert_eq!(fs::metadata(path).unwrap().len(), durable_length);
    }

    #[test]
    fn unit_journal_rejects_checksum_corruption_and_binding_substitution() {
        let (_temporary, stage) = test_stage();
        let binding = Sha256Hasher::digest(b"plan");
        let mut journal = UnitJournal::open_or_create(&stage, binding, 1, 1).unwrap();
        journal.append(completion()).unwrap();
        let path = journal.path.clone();
        drop(journal);

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let offset = file.metadata().unwrap().len() - 1;
        let mut original = [0_u8; 1];
        file.read_exact_at(&mut original, offset).unwrap();
        file.write_all_at(&[original[0] ^ 0x5a], offset).unwrap();
        file.sync_data().unwrap();
        assert!(matches!(
            UnitJournal::open_or_create(&stage, binding, 1, 1),
            Err(ImportError::InvalidCheckpoint(_))
        ));

        let (_other_temporary, other_stage) = test_stage();
        let journal = UnitJournal::open_or_create(&other_stage, binding, 1, 1).unwrap();
        drop(journal);
        assert!(matches!(
            UnitJournal::open_or_create(
                &other_stage,
                Sha256Hasher::digest(b"different plan"),
                1,
                1,
            ),
            Err(ImportError::InvalidCheckpoint(_))
        ));
    }
}

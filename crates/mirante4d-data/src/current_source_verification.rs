//! Bounded D-009 verification for the temporary current-format source.
//!
//! The resulting proof is deliberately opaque. It binds a complete safe
//! source-tree inventory to the scientific identity and is consumed when a
//! verified runtime source is prepared. It is not a package identity, a
//! persistent cache record, or an authority derived from the current manifest.

use std::{
    fmt, fs,
    fs::File,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use mirante4d_dataset::{
    CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError, DatasetCatalog,
    DatasetResourceKey, DatasetSource, DatasetSourceFault, DecodeSinkError, ReservedDecodeSink,
    ResourcePayloadDescriptor, ResourceRegion, ResourceValidity, ScientificIdentityStatus,
};
use mirante4d_domain::{IntensityDType, LogicalLayerKey, ScaleLevel, Shape3D, Shape4D, TimeIndex};
use mirante4d_identity::{
    SCIENTIFIC_TILE_SHAPE_TZYX, ScientificContentId, ScientificDatasetHasher, ScientificHashError,
    ScientificLayerDescriptor, ScientificLayerHasher, ScientificTemporalCalibration,
    ScientificTile,
};
use thiserror::Error;

use crate::{CurrentDatasetSource, CurrentDatasetSourceOpenError};

const INVENTORY_HASH_BUFFER_BYTES: usize = 64 * 1024;
const INVENTORY_ENTRY_COUNT_MAX: usize = 131_072;
const INVENTORY_DIRECTORY_FANOUT_MAX: usize = 4_096;
const INVENTORY_DEPTH_MAX: usize = 64;
const INVENTORY_PATH_BYTES_MAX: usize = 4_096;
const INVENTORY_DOMAIN: &[u8] = b"M4D-CURRENT-SOURCE-INVENTORY-V1\0";
const _: () = assert!(INVENTORY_HASH_BUFFER_BYTES <= 1_048_576);

/// Stage whose monotonic progress is being reported by current-source
/// verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurrentSourceVerificationPhase {
    PreInventory,
    ScientificScan,
    PostInventory,
}

/// Bounded scalar progress for one verification stage.
///
/// Inventory stages count exact regular-file bytes. The scientific stage
/// counts fixed D-009 identity tiles. A phase change resets `completed_units`
/// to zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrentSourceVerificationProgress {
    phase: CurrentSourceVerificationPhase,
    completed_units: u64,
    total_units: u64,
}

impl CurrentSourceVerificationProgress {
    pub const fn phase(self) -> CurrentSourceVerificationPhase {
        self.phase
    }

    pub const fn completed_units(self) -> u64 {
        self.completed_units
    }

    pub const fn total_units(self) -> u64 {
        self.total_units
    }
}

/// Sanitized deterministic work facts from a successful verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrentSourceVerificationReport {
    regular_files: u64,
    source_bytes: u64,
    logical_layers: u32,
    identity_tiles: u64,
    canonical_value_bytes: u64,
    validity_bytes: u64,
    inventory_root_blake3: [u8; 32],
}

impl CurrentSourceVerificationReport {
    pub const fn regular_files(self) -> u64 {
        self.regular_files
    }

    pub const fn source_bytes(self) -> u64 {
        self.source_bytes
    }

    pub const fn logical_layers(self) -> u32 {
        self.logical_layers
    }

    pub const fn identity_tiles(self) -> u64 {
        self.identity_tiles
    }

    pub const fn canonical_value_bytes(self) -> u64 {
        self.canonical_value_bytes
    }

    pub const fn validity_bytes(self) -> u64 {
        self.validity_bytes
    }

    pub const fn inventory_root_blake3(self) -> [u8; 32] {
        self.inventory_root_blake3
    }
}

/// Opaque source-stability proof returned only after the pre-inventory,
/// scientific scan, and exact post-inventory all agree.
pub struct CurrentSourceVerification {
    scientific_content_id: ScientificContentId,
    catalog: Arc<DatasetCatalog>,
    inventory: InventorySnapshot,
    report: CurrentSourceVerificationReport,
}

impl fmt::Debug for CurrentSourceVerification {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CurrentSourceVerification")
            .field("scientific_content_id", &self.scientific_content_id)
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}

impl CurrentSourceVerification {
    pub const fn scientific_content_id(&self) -> ScientificContentId {
        self.scientific_content_id
    }

    pub const fn catalog(&self) -> &Arc<DatasetCatalog> {
        &self.catalog
    }

    pub const fn report(&self) -> CurrentSourceVerificationReport {
        self.report
    }
}

/// Typed failure before a verified current-source catalog can be issued.
#[derive(Debug, Error)]
pub enum CurrentSourceVerificationError {
    #[error("current-source verification was cancelled")]
    Cancelled,
    #[error("the current-source closure changed during verification")]
    SourceChanged,
    #[error("the current-source inventory exceeds its fixed safety bounds")]
    InventoryCapacity,
    #[error("the current-source inventory could not be read: {kind:?}")]
    InventoryIo { kind: io::ErrorKind },
    #[error(
        "the current source uses unsupported scientific/stored dtypes {scientific_dtype:?}/{stored_dtype:?}"
    )]
    UnsupportedDTypePair {
        scientific_dtype: IntensityDType,
        stored_dtype: IntensityDType,
    },
    #[error("the current source is scientifically invalid")]
    InvalidSource,
    #[error("current-source verification CPU admission failed: {0}")]
    Capacity(#[source] CpuLedgerError),
    #[error("the CPU byte ledger returned an invalid verification lease")]
    InvalidLedgerLease,
    #[error("D-009 scientific hashing failed: {0}")]
    Identity(#[from] ScientificHashError),
}

#[derive(Debug, PartialEq, Eq)]
struct InventorySnapshot {
    directories: Vec<Box<[u8]>>,
    files: Vec<InventoryFile>,
    source_bytes: u64,
    root: [u8; 32],
}

#[derive(Debug, PartialEq, Eq)]
struct InventoryFile {
    relative_path: Box<[u8]>,
    byte_length: u64,
    digest: [u8; 32],
}

struct PendingInventoryFile {
    relative_path: Box<[u8]>,
    absolute_path: PathBuf,
    identity: FileIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    byte_length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[derive(Default)]
struct ScientificScanReport {
    logical_layers: u32,
    identity_tiles: u64,
    canonical_value_bytes: u64,
    validity_bytes: u64,
}

struct DecodedScientificTile {
    validity: Vec<u8>,
    values: Vec<u8>,
    _lease: Box<dyn CpuByteLease>,
}

impl CurrentDatasetSource {
    /// Verifies the complete current-source closure and derives its D-009
    /// scientific identity from base-scale values and validity.
    pub fn verify_scientific_content(
        &self,
        is_cancelled: impl Fn() -> bool,
        mut report_progress: impl FnMut(CurrentSourceVerificationProgress),
    ) -> Result<CurrentSourceVerification, CurrentSourceVerificationError> {
        if !matches!(
            self.verification_catalog().scientific_identity(),
            ScientificIdentityStatus::Unverified(_)
        ) {
            return Err(CurrentSourceVerificationError::InvalidSource);
        }

        let root = self.verification_dataset().root();
        let pre = collect_inventory(
            root,
            CurrentSourceVerificationPhase::PreInventory,
            &is_cancelled,
            &mut report_progress,
        )?;
        let source_id = self
            .verification_catalog()
            .scientific_identity()
            .source_id()
            .ok_or(CurrentSourceVerificationError::InvalidSource)?;
        let scan_source = Self::open(root, source_id, Arc::clone(self.verification_ledger()))
            .map_err(map_verified_open_error)?;
        if scan_source.verification_dataset().manifest() != self.verification_dataset().manifest()
            || scan_source.verification_catalog().as_ref() != self.verification_catalog().as_ref()
        {
            return Err(CurrentSourceVerificationError::SourceChanged);
        }
        let (scientific_content_id, scan) =
            scan_scientific_content(&scan_source, &is_cancelled, &mut report_progress)?;
        drop(scan_source);
        let post = collect_inventory(
            root,
            CurrentSourceVerificationPhase::PostInventory,
            &is_cancelled,
            &mut report_progress,
        )?;
        if pre != post {
            return Err(CurrentSourceVerificationError::SourceChanged);
        }

        let current = self.verification_catalog();
        let catalog = Arc::new(
            DatasetCatalog::new(
                current.label(),
                ScientificIdentityStatus::Verified(scientific_content_id),
                current.layers().cloned().collect(),
            )
            .map_err(|_| CurrentSourceVerificationError::InvalidSource)?,
        );
        if catalog.label() != current.label() || !catalog.layers().eq(current.layers()) {
            return Err(CurrentSourceVerificationError::InvalidSource);
        }

        let regular_files = u64::try_from(post.files.len())
            .map_err(|_| CurrentSourceVerificationError::InventoryCapacity)?;
        let report = CurrentSourceVerificationReport {
            regular_files,
            source_bytes: post.source_bytes,
            logical_layers: scan.logical_layers,
            identity_tiles: scan.identity_tiles,
            canonical_value_bytes: scan.canonical_value_bytes,
            validity_bytes: scan.validity_bytes,
            inventory_root_blake3: post.root,
        };
        Ok(CurrentSourceVerification {
            scientific_content_id,
            catalog,
            inventory: post,
            report,
        })
    }

    /// Reopens an identical source closure under a runtime-owned ledger and
    /// exposes only the previously verified identity.
    ///
    /// The closure is checked both before and after current-format open so the
    /// returned source never trusts a path, manifest claim, or stale proof.
    pub fn open_verified(
        root: impl AsRef<Path>,
        verification: &CurrentSourceVerification,
        ledger: Arc<dyn CpuByteLedger>,
        is_cancelled: impl Fn() -> bool,
        mut report_progress: impl FnMut(CurrentSourceVerificationProgress),
    ) -> Result<Arc<Self>, CurrentSourceVerificationError> {
        let root = root.as_ref();
        let before = collect_inventory(
            root,
            CurrentSourceVerificationPhase::PreInventory,
            &is_cancelled,
            &mut report_progress,
        )?;
        if before != verification.inventory {
            return Err(CurrentSourceVerificationError::SourceChanged);
        }

        let source = Self::open_with_identity(
            root,
            ScientificIdentityStatus::Verified(verification.scientific_content_id),
            ledger,
        )
        .map_err(map_verified_open_error)?;
        if source.verification_catalog().as_ref() != verification.catalog.as_ref() {
            return Err(CurrentSourceVerificationError::SourceChanged);
        }

        let after = collect_inventory(
            source.verification_dataset().root(),
            CurrentSourceVerificationPhase::PostInventory,
            &is_cancelled,
            &mut report_progress,
        )?;
        if after != verification.inventory {
            return Err(CurrentSourceVerificationError::SourceChanged);
        }
        Ok(source)
    }
}

fn map_verified_open_error(error: CurrentDatasetSourceOpenError) -> CurrentSourceVerificationError {
    match error {
        CurrentDatasetSourceOpenError::ManifestMetadata(error) => {
            if error.kind() == io::ErrorKind::NotFound {
                CurrentSourceVerificationError::SourceChanged
            } else {
                CurrentSourceVerificationError::InventoryIo { kind: error.kind() }
            }
        }
        CurrentDatasetSourceOpenError::MetadataAdmission(error) => {
            CurrentSourceVerificationError::Capacity(error)
        }
        CurrentDatasetSourceOpenError::InvalidMetadataLease => {
            CurrentSourceVerificationError::InvalidLedgerLease
        }
        CurrentDatasetSourceOpenError::MetadataAccountingOverflow => {
            CurrentSourceVerificationError::InventoryCapacity
        }
        CurrentDatasetSourceOpenError::Dataset(_)
        | CurrentDatasetSourceOpenError::Catalog(_)
        | CurrentDatasetSourceOpenError::LayerOrdinalOverflow => {
            CurrentSourceVerificationError::SourceChanged
        }
    }
}

fn collect_inventory(
    root: &Path,
    phase: CurrentSourceVerificationPhase,
    is_cancelled: &impl Fn() -> bool,
    report_progress: &mut impl FnMut(CurrentSourceVerificationProgress),
) -> Result<InventorySnapshot, CurrentSourceVerificationError> {
    checkpoint(is_cancelled)?;
    let root_metadata = source_metadata(root)?;
    if !root_metadata.file_type().is_dir() {
        return Err(CurrentSourceVerificationError::SourceChanged);
    }

    let mut directories = Vec::new();
    let mut pending_files = Vec::new();
    let mut stack = vec![(root.to_path_buf(), Vec::<u8>::new(), 0_usize)];
    let mut entry_count = 0_usize;
    let mut source_bytes = 0_u64;

    while let Some((directory, relative, depth)) = stack.pop() {
        checkpoint(is_cancelled)?;
        let before = source_metadata(&directory)?;
        if !before.file_type().is_dir() {
            return Err(CurrentSourceVerificationError::SourceChanged);
        }
        let directory_identity = metadata_identity(&before);
        let mut children = Vec::new();
        for child in fs::read_dir(&directory).map_err(map_inventory_io)? {
            checkpoint(is_cancelled)?;
            let child = child.map_err(map_inventory_io)?;
            check_inventory_directory_fanout(
                children
                    .len()
                    .checked_add(1)
                    .ok_or(CurrentSourceVerificationError::InventoryCapacity)?,
            )?;
            children.push(child);
        }
        children.sort_by(|left, right| {
            use std::os::unix::ffi::OsStrExt;
            left.file_name()
                .as_bytes()
                .cmp(right.file_name().as_bytes())
        });

        for child in children.into_iter().rev() {
            checkpoint(is_cancelled)?;
            use std::os::unix::ffi::OsStrExt;
            let name = child.file_name();
            let name = name.as_bytes();
            let child_relative = joined_relative_path(&relative, name)?;
            entry_count = checked_inventory_entry_count(entry_count, 1)?;
            let child_path = child.path();
            let metadata = source_metadata(&child_path)?;
            let file_type = metadata.file_type();
            if file_type.is_dir() {
                let child_depth = checked_inventory_depth(depth)?;
                directories.push(child_relative.clone().into_boxed_slice());
                stack.push((child_path, child_relative, child_depth));
            } else if file_type.is_file() {
                source_bytes = source_bytes
                    .checked_add(metadata.len())
                    .ok_or(CurrentSourceVerificationError::InventoryCapacity)?;
                pending_files.push(PendingInventoryFile {
                    relative_path: child_relative.into_boxed_slice(),
                    absolute_path: child_path,
                    identity: metadata_identity(&metadata),
                });
            } else {
                return Err(CurrentSourceVerificationError::SourceChanged);
            }
        }

        let after = source_metadata(&directory)?;
        if !after.file_type().is_dir() || metadata_identity(&after) != directory_identity {
            return Err(CurrentSourceVerificationError::SourceChanged);
        }
    }

    directories.sort_unstable();
    pending_files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    report_progress(CurrentSourceVerificationProgress {
        phase,
        completed_units: 0,
        total_units: source_bytes,
    });

    let mut completed_bytes = 0_u64;
    let mut files = Vec::with_capacity(pending_files.len());
    for pending in pending_files {
        let digest = hash_inventory_file(
            &pending,
            phase,
            source_bytes,
            &mut completed_bytes,
            is_cancelled,
            report_progress,
        )?;
        files.push(InventoryFile {
            relative_path: pending.relative_path,
            byte_length: pending.identity.byte_length,
            digest,
        });
    }
    checkpoint(is_cancelled)?;
    let root = inventory_root(&files)?;
    Ok(InventorySnapshot {
        directories,
        files,
        source_bytes,
        root,
    })
}

fn hash_inventory_file(
    pending: &PendingInventoryFile,
    phase: CurrentSourceVerificationPhase,
    total_bytes: u64,
    completed_bytes: &mut u64,
    is_cancelled: &impl Fn() -> bool,
    report_progress: &mut impl FnMut(CurrentSourceVerificationProgress),
) -> Result<[u8; 32], CurrentSourceVerificationError> {
    checkpoint(is_cancelled)?;
    let path_before = source_metadata(&pending.absolute_path)?;
    if !path_before.file_type().is_file() || metadata_identity(&path_before) != pending.identity {
        return Err(CurrentSourceVerificationError::SourceChanged);
    }
    let mut file = File::open(&pending.absolute_path).map_err(map_inventory_io)?;
    let opened_before = file.metadata().map_err(map_inventory_io)?;
    if !opened_before.file_type().is_file() || metadata_identity(&opened_before) != pending.identity
    {
        return Err(CurrentSourceVerificationError::SourceChanged);
    }

    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; INVENTORY_HASH_BUFFER_BYTES];
    let mut observed = 0_u64;
    loop {
        checkpoint(is_cancelled)?;
        let read = file.read(&mut buffer).map_err(map_inventory_io)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        let read =
            u64::try_from(read).map_err(|_| CurrentSourceVerificationError::InventoryCapacity)?;
        observed = observed
            .checked_add(read)
            .ok_or(CurrentSourceVerificationError::InventoryCapacity)?;
        *completed_bytes = completed_bytes
            .checked_add(read)
            .ok_or(CurrentSourceVerificationError::InventoryCapacity)?;
        report_progress(CurrentSourceVerificationProgress {
            phase,
            completed_units: *completed_bytes,
            total_units: total_bytes,
        });
    }
    if observed != pending.identity.byte_length {
        return Err(CurrentSourceVerificationError::SourceChanged);
    }
    let opened_after = file.metadata().map_err(map_inventory_io)?;
    let path_after = source_metadata(&pending.absolute_path)?;
    if !opened_after.file_type().is_file()
        || !path_after.file_type().is_file()
        || metadata_identity(&opened_after) != pending.identity
        || metadata_identity(&path_after) != pending.identity
    {
        return Err(CurrentSourceVerificationError::SourceChanged);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn source_metadata(path: &Path) -> Result<fs::Metadata, CurrentSourceVerificationError> {
    fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            CurrentSourceVerificationError::SourceChanged
        } else {
            map_inventory_io(error)
        }
    })
}

fn map_inventory_io(error: io::Error) -> CurrentSourceVerificationError {
    if error.kind() == io::ErrorKind::NotFound {
        CurrentSourceVerificationError::SourceChanged
    } else {
        CurrentSourceVerificationError::InventoryIo { kind: error.kind() }
    }
}

fn metadata_identity(metadata: &fs::Metadata) -> FileIdentity {
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        byte_length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    }
}

fn joined_relative_path(
    parent: &[u8],
    name: &[u8],
) -> Result<Vec<u8>, CurrentSourceVerificationError> {
    if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') {
        return Err(CurrentSourceVerificationError::SourceChanged);
    }
    let separator = usize::from(!parent.is_empty());
    let length = parent
        .len()
        .checked_add(separator)
        .and_then(|length| length.checked_add(name.len()))
        .ok_or(CurrentSourceVerificationError::InventoryCapacity)?;
    if length > INVENTORY_PATH_BYTES_MAX {
        return Err(CurrentSourceVerificationError::InventoryCapacity);
    }
    let mut relative = Vec::with_capacity(length);
    relative.extend_from_slice(parent);
    if separator != 0 {
        relative.push(b'/');
    }
    relative.extend_from_slice(name);
    Ok(relative)
}

fn checked_inventory_entry_count(
    current: usize,
    addition: usize,
) -> Result<usize, CurrentSourceVerificationError> {
    let count = current
        .checked_add(addition)
        .ok_or(CurrentSourceVerificationError::InventoryCapacity)?;
    if count > INVENTORY_ENTRY_COUNT_MAX {
        Err(CurrentSourceVerificationError::InventoryCapacity)
    } else {
        Ok(count)
    }
}

fn check_inventory_directory_fanout(count: usize) -> Result<(), CurrentSourceVerificationError> {
    if count > INVENTORY_DIRECTORY_FANOUT_MAX {
        Err(CurrentSourceVerificationError::InventoryCapacity)
    } else {
        Ok(())
    }
}

fn checked_inventory_depth(parent: usize) -> Result<usize, CurrentSourceVerificationError> {
    let depth = parent
        .checked_add(1)
        .ok_or(CurrentSourceVerificationError::InventoryCapacity)?;
    if depth > INVENTORY_DEPTH_MAX {
        Err(CurrentSourceVerificationError::InventoryCapacity)
    } else {
        Ok(depth)
    }
}

fn inventory_root(files: &[InventoryFile]) -> Result<[u8; 32], CurrentSourceVerificationError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(INVENTORY_DOMAIN);
    let count = u64::try_from(files.len())
        .map_err(|_| CurrentSourceVerificationError::InventoryCapacity)?;
    hasher.update(&count.to_be_bytes());
    for file in files {
        let path_length = u64::try_from(file.relative_path.len())
            .map_err(|_| CurrentSourceVerificationError::InventoryCapacity)?;
        hasher.update(&path_length.to_be_bytes());
        hasher.update(&file.relative_path);
        hasher.update(&file.byte_length.to_be_bytes());
        hasher.update(&file.digest);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn scan_scientific_content(
    source: &CurrentDatasetSource,
    is_cancelled: &impl Fn() -> bool,
    report_progress: &mut impl FnMut(CurrentSourceVerificationProgress),
) -> Result<(ScientificContentId, ScientificScanReport), CurrentSourceVerificationError> {
    checkpoint(is_cancelled)?;
    let manifest = source.verification_dataset().manifest();
    let layer_count = u32::try_from(manifest.layers.len())
        .map_err(|_| CurrentSourceVerificationError::InventoryCapacity)?;
    let mut total_tiles = 0_u64;
    for (ordinal, layer) in manifest.layers.iter().enumerate() {
        let ordinal = u32::try_from(ordinal)
            .map_err(|_| CurrentSourceVerificationError::InventoryCapacity)?;
        let catalog_layer = source
            .verification_catalog()
            .layer(LogicalLayerKey::new(ordinal))
            .ok_or(CurrentSourceVerificationError::InvalidSource)?;
        if catalog_layer.key().ordinal() != ordinal {
            return Err(CurrentSourceVerificationError::InvalidSource);
        }
        validate_dtype_pair(layer.dtype.source, layer.dtype.stored)?;
        let descriptor = ScientificLayerDescriptor::new(
            LogicalLayerKey::new(ordinal),
            layer.dtype.source,
            layer.shape,
            ScientificTemporalCalibration::Unknown,
            layer.grid_to_world,
        )?;
        let layer_hasher = ScientificLayerHasher::new(descriptor)?;
        total_tiles = total_tiles
            .checked_add(layer_hasher.expected_tile_count())
            .ok_or(CurrentSourceVerificationError::InventoryCapacity)?;
    }
    report_progress(CurrentSourceVerificationProgress {
        phase: CurrentSourceVerificationPhase::ScientificScan,
        completed_units: 0,
        total_units: total_tiles,
    });

    let mut dataset_hasher = ScientificDatasetHasher::new(layer_count)?;
    let mut report = ScientificScanReport {
        logical_layers: layer_count,
        ..ScientificScanReport::default()
    };
    for (ordinal, layer) in manifest.layers.iter().enumerate() {
        checkpoint(is_cancelled)?;
        let ordinal = u32::try_from(ordinal)
            .map_err(|_| CurrentSourceVerificationError::InventoryCapacity)?;
        layer
            .scales
            .first()
            .filter(|scale| scale.level == 0)
            .ok_or(CurrentSourceVerificationError::InvalidSource)?;
        let descriptor = ScientificLayerDescriptor::new(
            LogicalLayerKey::new(ordinal),
            layer.dtype.source,
            layer.shape,
            ScientificTemporalCalibration::Unknown,
            layer.grid_to_world,
        )?;
        let mut layer_hasher = ScientificLayerHasher::new(descriptor)?;
        visit_identity_tiles(layer.shape, |origin, extent| {
            checkpoint(is_cancelled)?;
            let tile = decode_scientific_tile(
                source,
                LogicalLayerKey::new(ordinal),
                layer.dtype.source,
                layer.dtype.stored,
                origin,
                extent,
                is_cancelled,
            )?;
            layer_hasher.push_tile(ScientificTile::new(
                origin,
                extent,
                &tile.validity,
                &tile.values,
            ))?;
            report.identity_tiles = report
                .identity_tiles
                .checked_add(1)
                .ok_or(CurrentSourceVerificationError::InventoryCapacity)?;
            report.canonical_value_bytes = report
                .canonical_value_bytes
                .checked_add(
                    u64::try_from(tile.values.len())
                        .map_err(|_| CurrentSourceVerificationError::InventoryCapacity)?,
                )
                .ok_or(CurrentSourceVerificationError::InventoryCapacity)?;
            report.validity_bytes = report
                .validity_bytes
                .checked_add(
                    u64::try_from(tile.validity.len())
                        .map_err(|_| CurrentSourceVerificationError::InventoryCapacity)?,
                )
                .ok_or(CurrentSourceVerificationError::InventoryCapacity)?;
            report_progress(CurrentSourceVerificationProgress {
                phase: CurrentSourceVerificationPhase::ScientificScan,
                completed_units: report.identity_tiles,
                total_units: total_tiles,
            });
            Ok(())
        })?;
        let root = layer_hasher.finalize()?;
        dataset_hasher.push_layer(root)?;
    }
    checkpoint(is_cancelled)?;
    Ok((dataset_hasher.finalize()?, report))
}

fn validate_dtype_pair(
    source: IntensityDType,
    stored: IntensityDType,
) -> Result<(), CurrentSourceVerificationError> {
    if matches!(
        (source, stored),
        (IntensityDType::Uint8, IntensityDType::Uint8)
            | (IntensityDType::Uint8, IntensityDType::Uint16)
            | (IntensityDType::Uint16, IntensityDType::Uint16)
            | (IntensityDType::Float32, IntensityDType::Float32)
    ) {
        Ok(())
    } else {
        Err(CurrentSourceVerificationError::UnsupportedDTypePair {
            scientific_dtype: source,
            stored_dtype: stored,
        })
    }
}

fn visit_identity_tiles(
    shape: Shape4D,
    mut visit: impl FnMut([u64; 4], [u64; 4]) -> Result<(), CurrentSourceVerificationError>,
) -> Result<(), CurrentSourceVerificationError> {
    for t in 0..shape.t() {
        let mut z = 0;
        while z < shape.z() {
            let mut y = 0;
            while y < shape.y() {
                let mut x = 0;
                while x < shape.x() {
                    let origin = [t, z, y, x];
                    let extent = [
                        1,
                        SCIENTIFIC_TILE_SHAPE_TZYX[1].min(shape.z() - z),
                        SCIENTIFIC_TILE_SHAPE_TZYX[2].min(shape.y() - y),
                        SCIENTIFIC_TILE_SHAPE_TZYX[3].min(shape.x() - x),
                    ];
                    visit(origin, extent)?;
                    x = x
                        .checked_add(SCIENTIFIC_TILE_SHAPE_TZYX[3])
                        .ok_or(CurrentSourceVerificationError::InventoryCapacity)?;
                }
                y = y
                    .checked_add(SCIENTIFIC_TILE_SHAPE_TZYX[2])
                    .ok_or(CurrentSourceVerificationError::InventoryCapacity)?;
            }
            z = z
                .checked_add(SCIENTIFIC_TILE_SHAPE_TZYX[1])
                .ok_or(CurrentSourceVerificationError::InventoryCapacity)?;
        }
    }
    Ok(())
}

fn decode_scientific_tile(
    source: &CurrentDatasetSource,
    layer: LogicalLayerKey,
    scientific_dtype: IntensityDType,
    stored_dtype: IntensityDType,
    origin: [u64; 4],
    extent: [u64; 4],
    is_cancelled: &impl Fn() -> bool,
) -> Result<DecodedScientificTile, CurrentSourceVerificationError> {
    validate_dtype_pair(scientific_dtype, stored_dtype)?;
    let shape = Shape3D::new(extent[1], extent[2], extent[3])
        .map_err(|_| CurrentSourceVerificationError::InvalidSource)?;
    let region = ResourceRegion::new([origin[1], origin[2], origin[3]], shape)
        .map_err(|_| CurrentSourceVerificationError::InvalidSource)?;
    let key = DatasetResourceKey::new(
        source
            .verification_catalog()
            .scientific_identity()
            .resource_identity(),
        layer,
        TimeIndex::new(origin[0]),
        ScaleLevel::BASE,
        region,
    );
    let descriptor = source
        .verification_catalog()
        .resource_payload_descriptor(key)
        .map_err(|_| CurrentSourceVerificationError::InvalidSource)?;
    if descriptor.dtype() != stored_dtype {
        return Err(CurrentSourceVerificationError::InvalidSource);
    }
    let canonical_value_bytes = descriptor
        .sample_count()
        .checked_mul(u64::from(scientific_dtype.bytes_per_sample()))
        .ok_or(CurrentSourceVerificationError::InventoryCapacity)?;
    let validity_bytes = descriptor
        .sample_count()
        .checked_add(7)
        .ok_or(CurrentSourceVerificationError::InventoryCapacity)?
        / 8;
    let charged_bytes = descriptor
        .byte_len()
        .checked_add(canonical_value_bytes)
        .and_then(|bytes| bytes.checked_add(validity_bytes))
        .ok_or(CurrentSourceVerificationError::InventoryCapacity)?;
    let lease = source
        .verification_ledger()
        .try_acquire(CpuLedgerCategory::InFlightDecode, charged_bytes)
        .map_err(CurrentSourceVerificationError::Capacity)?;
    if lease.category() != CpuLedgerCategory::InFlightDecode
        || lease.reserved_bytes() != charged_bytes
    {
        return Err(CurrentSourceVerificationError::InvalidLedgerLease);
    }

    let capacity = usize::try_from(descriptor.byte_len())
        .map_err(|_| CurrentSourceVerificationError::InventoryCapacity)?;
    let mut sink = VerificationSink {
        key,
        descriptor,
        bytes: Vec::with_capacity(capacity),
        finished: false,
        is_cancelled,
    };
    source.decode_into(&mut sink).map_err(map_decode_error)?;
    if !sink.finished || sink.bytes.len() != capacity {
        return Err(CurrentSourceVerificationError::InvalidSource);
    }
    let value_length = usize::try_from(descriptor.value_byte_len())
        .map_err(|_| CurrentSourceVerificationError::InventoryCapacity)?;
    let (stored_values, stored_validity) = sink.bytes.split_at(value_length);
    let validity = scientific_validity(
        descriptor.validity(),
        descriptor.sample_count(),
        stored_validity,
    )?;
    let values = canonical_scientific_values(
        scientific_dtype,
        stored_dtype,
        descriptor.sample_count(),
        stored_values,
        &validity,
    )?;
    Ok(DecodedScientificTile {
        validity,
        values,
        _lease: lease,
    })
}

fn scientific_validity(
    representation: ResourceValidity,
    sample_count: u64,
    stored: &[u8],
) -> Result<Vec<u8>, CurrentSourceVerificationError> {
    let length = usize::try_from(
        sample_count
            .checked_add(7)
            .ok_or(CurrentSourceVerificationError::InventoryCapacity)?
            / 8,
    )
    .map_err(|_| CurrentSourceVerificationError::InventoryCapacity)?;
    match representation {
        ResourceValidity::BitMask => {
            if stored.len() != length {
                return Err(CurrentSourceVerificationError::InvalidSource);
            }
            let used = u8::try_from(sample_count % 8)
                .map_err(|_| CurrentSourceVerificationError::InventoryCapacity)?;
            if used != 0 {
                let used_mask = (1_u8 << used) - 1;
                if stored
                    .last()
                    .is_some_and(|final_byte| final_byte & !used_mask != 0)
                {
                    return Err(CurrentSourceVerificationError::InvalidSource);
                }
            }
            Ok(stored.to_vec())
        }
        ResourceValidity::AllValid => {
            if !stored.is_empty() {
                return Err(CurrentSourceVerificationError::InvalidSource);
            }
            let mut validity = vec![u8::MAX; length];
            let used = u8::try_from(sample_count % 8)
                .map_err(|_| CurrentSourceVerificationError::InventoryCapacity)?;
            if used != 0 {
                *validity
                    .last_mut()
                    .ok_or(CurrentSourceVerificationError::InvalidSource)? = (1_u8 << used) - 1;
            }
            Ok(validity)
        }
    }
}

fn canonical_scientific_values(
    scientific_dtype: IntensityDType,
    stored_dtype: IntensityDType,
    sample_count: u64,
    stored: &[u8],
    validity: &[u8],
) -> Result<Vec<u8>, CurrentSourceVerificationError> {
    validate_dtype_pair(scientific_dtype, stored_dtype)?;
    let samples = usize::try_from(sample_count)
        .map_err(|_| CurrentSourceVerificationError::InventoryCapacity)?;
    let stored_length = samples
        .checked_mul(usize::from(stored_dtype.bytes_per_sample()))
        .ok_or(CurrentSourceVerificationError::InventoryCapacity)?;
    if stored.len() != stored_length {
        return Err(CurrentSourceVerificationError::InvalidSource);
    }
    let validity_length = samples
        .checked_add(7)
        .ok_or(CurrentSourceVerificationError::InventoryCapacity)?
        / 8;
    if validity.len() != validity_length {
        return Err(CurrentSourceVerificationError::InvalidSource);
    }
    if samples % 8 != 0 {
        let used_mask = (1_u8 << (samples % 8)) - 1;
        if validity
            .last()
            .is_some_and(|final_byte| final_byte & !used_mask != 0)
        {
            return Err(CurrentSourceVerificationError::InvalidSource);
        }
    }
    let mut canonical = match (scientific_dtype, stored_dtype) {
        (IntensityDType::Uint8, IntensityDType::Uint8)
        | (IntensityDType::Uint16, IntensityDType::Uint16)
        | (IntensityDType::Float32, IntensityDType::Float32) => stored.to_vec(),
        (IntensityDType::Uint8, IntensityDType::Uint16) => {
            let mut values = Vec::with_capacity(samples);
            for (sample, bytes) in stored.chunks_exact(2).enumerate() {
                if sample_is_valid(validity, sample) {
                    let value = u16::from_le_bytes([bytes[0], bytes[1]]);
                    values.push(
                        u8::try_from(value)
                            .map_err(|_| CurrentSourceVerificationError::InvalidSource)?,
                    );
                } else {
                    values.push(0);
                }
            }
            values
        }
        _ => {
            return Err(CurrentSourceVerificationError::UnsupportedDTypePair {
                scientific_dtype,
                stored_dtype,
            });
        }
    };

    for sample in 0..samples {
        let valid = sample_is_valid(validity, sample);
        match scientific_dtype {
            IntensityDType::Uint8 => {
                if !valid {
                    canonical[sample] = 0;
                }
            }
            IntensityDType::Uint16 => {
                let offset = sample * 2;
                if !valid {
                    canonical[offset..offset + 2].fill(0);
                }
            }
            IntensityDType::Float32 => {
                let offset = sample * 4;
                if valid {
                    let bits = u32::from_le_bytes(
                        canonical[offset..offset + 4]
                            .try_into()
                            .map_err(|_| CurrentSourceVerificationError::InvalidSource)?,
                    );
                    if !f32::from_bits(bits).is_finite() {
                        return Err(CurrentSourceVerificationError::InvalidSource);
                    }
                } else {
                    canonical[offset..offset + 4].fill(0);
                }
            }
        }
    }
    Ok(canonical)
}

fn sample_is_valid(validity: &[u8], sample: usize) -> bool {
    validity[sample / 8] & (1_u8 << (sample % 8)) != 0
}

fn map_decode_error(error: DatasetSourceFault) -> CurrentSourceVerificationError {
    match error {
        DatasetSourceFault::Cancelled { .. } => CurrentSourceVerificationError::Cancelled,
        DatasetSourceFault::CapacityExceeded {
            category,
            requested_bytes,
            available_bytes,
            ..
        } => CurrentSourceVerificationError::Capacity(CpuLedgerError::CapacityExceeded {
            category,
            requested_bytes,
            available_bytes,
        }),
        DatasetSourceFault::ShuttingDown { .. } => {
            CurrentSourceVerificationError::Capacity(CpuLedgerError::ShuttingDown)
        }
        DatasetSourceFault::ResourceUnavailable { .. } => {
            CurrentSourceVerificationError::SourceChanged
        }
        DatasetSourceFault::CatalogUnavailable
        | DatasetSourceFault::InvalidResource { .. }
        | DatasetSourceFault::CorruptResource { .. }
        | DatasetSourceFault::UnsupportedResource { .. }
        | DatasetSourceFault::DecodeFailed { .. }
        | DatasetSourceFault::SinkRejected { .. } => CurrentSourceVerificationError::InvalidSource,
    }
}

fn checkpoint(is_cancelled: &impl Fn() -> bool) -> Result<(), CurrentSourceVerificationError> {
    if is_cancelled() {
        Err(CurrentSourceVerificationError::Cancelled)
    } else {
        Ok(())
    }
}

struct VerificationSink<'a, C: Fn() -> bool> {
    key: DatasetResourceKey,
    descriptor: ResourcePayloadDescriptor,
    bytes: Vec<u8>,
    finished: bool,
    is_cancelled: &'a C,
}

impl<C: Fn() -> bool> ReservedDecodeSink for VerificationSink<'_, C> {
    fn resource_key(&self) -> DatasetResourceKey {
        self.key
    }

    fn payload_descriptor(&self) -> ResourcePayloadDescriptor {
        self.descriptor
    }

    fn written_bytes(&self) -> u64 {
        u64::try_from(self.bytes.len()).unwrap_or(u64::MAX)
    }

    fn is_cancelled(&self) -> bool {
        (self.is_cancelled)()
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), DecodeSinkError> {
        if self.is_cancelled() {
            return Err(DecodeSinkError::Cancelled);
        }
        if self.finished {
            return Err(DecodeSinkError::AlreadyFinished);
        }
        let attempted = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or(DecodeSinkError::ByteCountOverflow)?;
        let attempted = u64::try_from(attempted).map_err(|_| DecodeSinkError::ByteCountOverflow)?;
        if attempted > self.descriptor.byte_len() {
            return Err(DecodeSinkError::ReservationExceeded {
                reserved: self.descriptor.byte_len(),
                attempted,
            });
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn finish(&mut self) -> Result<(), DecodeSinkError> {
        if self.is_cancelled() {
            return Err(DecodeSinkError::Cancelled);
        }
        if self.finished {
            return Err(DecodeSinkError::AlreadyFinished);
        }
        if self.written_bytes() != self.descriptor.byte_len() {
            return Err(DecodeSinkError::Incomplete {
                reserved: self.descriptor.byte_len(),
                written: self.written_bytes(),
            });
        }
        self.finished = true;
        Ok(())
    }

    fn is_finished(&self) -> bool {
        self.finished
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        os::unix::ffi::OsStrExt,
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use mirante4d_domain::{DisplayWindow, GridToWorld};
    use mirante4d_format::{
        ChannelMetadata, ExistingPackagePolicy, LayerDisplay, NativeMultiscaleDatasetWriter,
        NoDataPolicy, NoDataPolicyKind, NoDataVisibilityPolicy, ScaleReduction,
        StreamingF32LayerSpec, StreamingF32ScaleSpec, StreamingU8LayerSpec, StreamingU8ScaleSpec,
        StreamingU16LayerSpec, StreamingU16ScaleSpec, WorldSpace, WorldUnit,
    };

    use super::*;

    const ONE_VOXEL_U8_ID: &str =
        "m4d-sc-v1-sha256:1dd0a7a4ce0561326783f5cdf7b6eeff476a9628061d35c705f4af2c863ad392";

    #[derive(Debug)]
    struct TestLedgerState {
        caps: [u64; 7],
        used: Mutex<[u64; 7]>,
        peak: Mutex<[u64; 7]>,
    }

    #[derive(Debug, Clone)]
    struct TestLedger {
        state: Arc<TestLedgerState>,
    }

    #[derive(Debug)]
    struct TestLease {
        state: Arc<TestLedgerState>,
        category: CpuLedgerCategory,
        bytes: u64,
    }

    impl Drop for TestLease {
        fn drop(&mut self) {
            self.state.used.lock().unwrap()[category_index(self.category)] -= self.bytes;
        }
    }

    impl CpuByteLease for TestLease {
        fn category(&self) -> CpuLedgerCategory {
            self.category
        }

        fn reserved_bytes(&self) -> u64 {
            self.bytes
        }
    }

    impl CpuByteLedger for TestLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            if bytes == 0 {
                return Err(CpuLedgerError::ZeroByteReservation);
            }
            let index = category_index(category);
            let mut used = self.state.used.lock().unwrap();
            let available = self.state.caps[index].saturating_sub(used[index]);
            if bytes > available {
                return Err(CpuLedgerError::CapacityExceeded {
                    category,
                    requested_bytes: bytes,
                    available_bytes: available,
                });
            }
            used[index] += bytes;
            let current = used[index];
            drop(used);
            let mut peak = self.state.peak.lock().unwrap();
            peak[index] = peak[index].max(current);
            Ok(Box::new(TestLease {
                state: Arc::clone(&self.state),
                category,
                bytes,
            }))
        }
    }

    impl TestLedger {
        fn new(metadata: u64, decode: u64) -> Arc<Self> {
            let mut caps = [64 * 1024 * 1024; 7];
            caps[category_index(CpuLedgerCategory::MetadataAndIndexes)] = metadata;
            caps[category_index(CpuLedgerCategory::InFlightDecode)] = decode;
            Arc::new(Self {
                state: Arc::new(TestLedgerState {
                    caps,
                    used: Mutex::new([0; 7]),
                    peak: Mutex::new([0; 7]),
                }),
            })
        }

        fn used(&self, category: CpuLedgerCategory) -> u64 {
            self.state.used.lock().unwrap()[category_index(category)]
        }

        fn peak(&self, category: CpuLedgerCategory) -> u64 {
            self.state.peak.lock().unwrap()[category_index(category)]
        }
    }

    const fn category_index(category: CpuLedgerCategory) -> usize {
        match category {
            CpuLedgerCategory::DecodedResidency => 0,
            CpuLedgerCategory::UploadStaging => 1,
            CpuLedgerCategory::InFlightDecode => 2,
            CpuLedgerCategory::MetadataAndIndexes => 3,
            CpuLedgerCategory::QueuesAndResults => 4,
            CpuLedgerCategory::Prefetch => 5,
            CpuLedgerCategory::ImportWorkingSet => 6,
        }
    }

    fn ledger_trait(ledger: &Arc<TestLedger>) -> Arc<dyn CpuByteLedger> {
        ledger.clone()
    }

    fn source(root: &Path, ledger: &Arc<TestLedger>) -> Arc<CurrentDatasetSource> {
        CurrentDatasetSource::open(
            root,
            mirante4d_dataset::DatasetSourceId::new(7),
            ledger_trait(ledger),
        )
        .unwrap()
    }

    fn world() -> WorldSpace {
        WorldSpace {
            name: "sample".to_owned(),
            unit: WorldUnit::Micrometer,
        }
    }

    fn display(high: f32) -> LayerDisplay {
        LayerDisplay::new(true, DisplayWindow::new(0.0, high).unwrap(), 1.0).unwrap()
    }

    fn writer(root: &Path, id: &str) -> NativeMultiscaleDatasetWriter {
        NativeMultiscaleDatasetWriter::create(
            root,
            id.to_owned(),
            id.to_owned(),
            world(),
            ExistingPackagePolicy::Fail,
        )
        .unwrap()
    }

    fn write_u8(root: &Path, values: &[u8], validity: Option<&[u8]>) {
        let shape = Shape4D::new(1, 1, 1, values.len() as u64).unwrap();
        let grid = GridToWorld::identity();
        let mut writer = writer(root, "verification-u8");
        let mut layer = writer
            .begin_streaming_u8_layer(StreamingU8LayerSpec {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [1.0, 1.0, 1.0, 1.0],
                },
                shape,
                no_data_policy: validity.map(|_| NoDataPolicy {
                    kind: NoDataPolicyKind::SentinelValue,
                    source_value: 255.0,
                    source_dtype: IntensityDType::Uint8,
                    visibility_policy: NoDataVisibilityPolicy::InvisibleWith1VoxelInvalidDilation,
                }),
                grid_to_world: grid,
                display: display(255.0),
                scales: vec![StreamingU8ScaleSpec {
                    level: 0,
                    shape,
                    brick_shape: shape,
                    grid_to_world: grid,
                    source_scale: None,
                    reduction: ScaleReduction::Source,
                }],
            })
            .unwrap();
        match validity {
            Some(validity) => layer
                .write_timepoint_with_render_valid(0, 0, values, validity)
                .unwrap(),
            None => layer.write_timepoint(0, 0, values).unwrap(),
        }
        writer.finish_streaming_u8_layer(layer).unwrap();
        writer.finish().unwrap();
    }

    fn write_u16(root: &Path, source_dtype: IntensityDType, value: u16) {
        let shape = Shape4D::new(1, 1, 1, 1).unwrap();
        let grid = GridToWorld::identity();
        let mut writer = writer(root, "verification-u16");
        let mut layer = writer
            .begin_streaming_layer(StreamingU16LayerSpec {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [1.0, 1.0, 1.0, 1.0],
                },
                source_dtype,
                shape,
                grid_to_world: grid,
                display: display(65_535.0),
                scales: vec![StreamingU16ScaleSpec {
                    level: 0,
                    shape,
                    brick_shape: shape,
                    grid_to_world: grid,
                    source_scale: None,
                    reduction: ScaleReduction::Source,
                }],
            })
            .unwrap();
        layer.write_timepoint(0, 0, &[value]).unwrap();
        writer.finish_streaming_layer(layer).unwrap();
        writer.finish().unwrap();
    }

    fn write_f32(root: &Path, value: f32) {
        let shape = Shape4D::new(1, 1, 1, 1).unwrap();
        let grid = GridToWorld::identity();
        let mut writer = writer(root, "verification-f32");
        let mut layer = writer
            .begin_streaming_f32_layer(StreamingF32LayerSpec {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [1.0, 1.0, 1.0, 1.0],
                },
                shape,
                grid_to_world: grid,
                display: display(1.0),
                scales: vec![StreamingF32ScaleSpec {
                    level: 0,
                    shape,
                    brick_shape: shape,
                    grid_to_world: grid,
                    source_scale: None,
                    reduction: ScaleReduction::Source,
                }],
            })
            .unwrap();
        layer.write_timepoint(0, 0, &[value]).unwrap();
        writer.finish_streaming_f32_layer(layer).unwrap();
        writer.finish().unwrap();
    }

    fn verify(source: &CurrentDatasetSource) -> CurrentSourceVerification {
        source.verify_scientific_content(|| false, |_| {}).unwrap()
    }

    #[test]
    fn one_voxel_package_matches_the_independent_d009_vector_and_reopens_verified() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = tempdir.path().join("one-u8.m4d");
        write_u8(&root, &[7], None);
        let before = tree_bytes(&root);
        let ledger = TestLedger::new(8 * 1024 * 1024, 8 * 1024 * 1024);
        let source = source(&root, &ledger);
        let mut progress = Vec::new();
        let verification = source
            .verify_scientific_content(|| false, |update| progress.push(update))
            .unwrap();

        assert_eq!(
            verification.scientific_content_id(),
            ScientificContentId::parse(ONE_VOXEL_U8_ID).unwrap()
        );
        assert_eq!(verification.report().logical_layers(), 1);
        assert_eq!(verification.report().identity_tiles(), 1);
        assert_eq!(verification.report().canonical_value_bytes(), 1);
        assert_eq!(verification.report().validity_bytes(), 1);
        assert_eq!(
            verification.report().regular_files(),
            u64::try_from(before.len()).unwrap()
        );
        assert_eq!(
            verification.report().source_bytes(),
            before
                .values()
                .map(|bytes| u64::try_from(bytes.len()).unwrap())
                .sum::<u64>()
        );
        assert_eq!(tree_bytes(&root), before);
        assert!(matches!(
            verification.catalog().scientific_identity(),
            ScientificIdentityStatus::Verified(id)
                if *id == verification.scientific_content_id()
        ));
        assert_eq!(
            verification.catalog().layers().collect::<Vec<_>>(),
            source.verification_catalog().layers().collect::<Vec<_>>()
        );
        for phase in [
            CurrentSourceVerificationPhase::PreInventory,
            CurrentSourceVerificationPhase::ScientificScan,
            CurrentSourceVerificationPhase::PostInventory,
        ] {
            let updates = progress
                .iter()
                .copied()
                .filter(|update| update.phase() == phase)
                .collect::<Vec<_>>();
            assert!(!updates.is_empty());
            assert_eq!(updates[0].completed_units(), 0);
            assert_eq!(
                updates.last().unwrap().completed_units(),
                updates.last().unwrap().total_units()
            );
            assert!(updates.iter().all(|update| {
                update.completed_units() <= update.total_units()
                    && update.total_units() == updates[0].total_units()
            }));
            assert!(
                updates
                    .windows(2)
                    .all(|pair| { pair[0].completed_units() <= pair[1].completed_units() })
            );
        }

        let relocated = tempdir.path().join("relocated.m4d");
        copy_tree(&root, &relocated);
        let reopened_ledger = TestLedger::new(8 * 1024 * 1024, 8 * 1024 * 1024);
        let reopened = CurrentDatasetSource::open_verified(
            &relocated,
            &verification,
            ledger_trait(&reopened_ledger),
            || false,
            |_| {},
        )
        .unwrap();
        assert_eq!(
            reopened.catalog().unwrap().as_ref(),
            verification.catalog().as_ref()
        );
        assert_eq!(tree_bytes(&root), before);
        assert_eq!(tree_bytes(&relocated), before);
        assert!(ledger.peak(CpuLedgerCategory::InFlightDecode) > 0);
        assert_eq!(ledger.used(CpuLedgerCategory::InFlightDecode), 0);

        assert!(matches!(
            CurrentDatasetSource::open_verified(
                &root,
                &verification,
                ledger_trait(&reopened_ledger),
                || true,
                |_| {},
            ),
            Err(CurrentSourceVerificationError::Cancelled)
        ));
    }

    #[test]
    fn bridge_traversal_matches_manual_multi_layer_time_and_tile_edge_assembly() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = tempdir.path().join("traversal.m4d");
        let shape = Shape4D::new(2, 1, 1, 257).unwrap();
        let brick_shape = Shape4D::new(1, 1, 1, 128).unwrap();
        let grid = GridToWorld::identity();
        let make_values = |seed: usize| {
            (0..257)
                .map(|x| ((seed + x * 17) % 251) as u8)
                .collect::<Vec<_>>()
        };
        let layers = [
            [make_values(3), make_values(29)],
            [make_values(71), make_values(113)],
        ];

        let mut package_writer = writer(&root, "verification-traversal");
        for (ordinal, timepoints) in layers.iter().enumerate() {
            let mut layer = package_writer
                .begin_streaming_u8_layer(StreamingU8LayerSpec {
                    id: format!("ch{ordinal}"),
                    name: format!("Channel {ordinal}"),
                    channel: ChannelMetadata {
                        index: u32::try_from(ordinal).unwrap(),
                        color_rgba: [1.0, 1.0, 1.0, 1.0],
                    },
                    shape,
                    no_data_policy: None,
                    grid_to_world: grid,
                    display: display(255.0),
                    scales: vec![StreamingU8ScaleSpec {
                        level: 0,
                        shape,
                        brick_shape,
                        grid_to_world: grid,
                        source_scale: None,
                        reduction: ScaleReduction::Source,
                    }],
                })
                .unwrap();
            for (timepoint, values) in timepoints.iter().enumerate() {
                layer
                    .write_timepoint(0, u64::try_from(timepoint).unwrap(), values)
                    .unwrap();
            }
            package_writer.finish_streaming_u8_layer(layer).unwrap();
        }
        package_writer.finish().unwrap();

        let mut expected = ScientificDatasetHasher::new(2).unwrap();
        for (ordinal, timepoints) in layers.iter().enumerate() {
            let ordinal = u32::try_from(ordinal).unwrap();
            let descriptor = ScientificLayerDescriptor::new(
                LogicalLayerKey::new(ordinal),
                IntensityDType::Uint8,
                shape,
                ScientificTemporalCalibration::Unknown,
                grid,
            )
            .unwrap();
            let mut layer_hasher = ScientificLayerHasher::new(descriptor).unwrap();
            for (timepoint, values) in timepoints.iter().enumerate() {
                let timepoint = u64::try_from(timepoint).unwrap();
                layer_hasher
                    .push_tile(ScientificTile::new(
                        [timepoint, 0, 0, 0],
                        [1, 1, 1, 256],
                        &[u8::MAX; 32],
                        &values[..256],
                    ))
                    .unwrap();
                layer_hasher
                    .push_tile(ScientificTile::new(
                        [timepoint, 0, 0, 256],
                        [1, 1, 1, 1],
                        &[1],
                        &values[256..],
                    ))
                    .unwrap();
            }
            expected
                .push_layer(layer_hasher.finalize().unwrap())
                .unwrap();
        }

        let ledger = TestLedger::new(8 * 1024 * 1024, 8 * 1024 * 1024);
        let verification = verify(&source(&root, &ledger));
        assert_eq!(
            verification.scientific_content_id(),
            expected.finalize().unwrap()
        );
        assert_eq!(verification.report().logical_layers(), 2);
        assert_eq!(verification.report().identity_tiles(), 8);
        assert_eq!(verification.report().canonical_value_bytes(), 1_028);
        assert_eq!(verification.report().validity_bytes(), 132);
    }

    #[test]
    fn verification_rebinds_open_metadata_to_the_preinventory_closure() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = tempdir.path().join("stale-open.m4d");
        write_u8(&root, &[7], None);
        let ledger = TestLedger::new(8 * 1024 * 1024, 8 * 1024 * 1024);
        let source = source(&root, &ledger);

        let manifest_path = root.join("mirante4d.json");
        let manifest = fs::read_to_string(&manifest_path).unwrap();
        let changed = manifest.replacen("\"source\": \"uint8\"", "\"source\": \"uint16\"", 1);
        assert_ne!(changed, manifest);
        fs::write(manifest_path, changed).unwrap();

        assert!(matches!(
            source.verify_scientific_content(|| false, |_| {}),
            Err(CurrentSourceVerificationError::SourceChanged)
        ));
    }

    #[test]
    fn exact_four_dtype_pairs_are_admitted_and_u8_storage_is_identity_invariant() {
        let tempdir = tempfile::tempdir().unwrap();
        let u8 = tempdir.path().join("u8-u8.m4d");
        let widened = tempdir.path().join("u8-u16.m4d");
        let u16 = tempdir.path().join("u16-u16.m4d");
        let f32 = tempdir.path().join("f32-f32.m4d");
        let widened_out_of_range = tempdir.path().join("u8-u16-out-of-range.m4d");
        let unsupported = tempdir.path().join("f32-u16.m4d");
        write_u8(&u8, &[7], None);
        write_u16(&widened, IntensityDType::Uint8, 7);
        write_u16(&u16, IntensityDType::Uint16, 7);
        write_f32(&f32, 7.25);
        write_u16(&widened_out_of_range, IntensityDType::Uint8, 256);
        write_u16(&unsupported, IntensityDType::Float32, 7);
        let ledger = TestLedger::new(32 * 1024 * 1024, 16 * 1024 * 1024);

        let u8_id = verify(&source(&u8, &ledger)).scientific_content_id();
        let widened_id = verify(&source(&widened, &ledger)).scientific_content_id();
        assert_eq!(u8_id, widened_id);
        assert!(verify(&source(&u16, &ledger)).report().identity_tiles() > 0);
        assert!(verify(&source(&f32, &ledger)).report().identity_tiles() > 0);
        assert!(matches!(
            source(&widened_out_of_range, &ledger).verify_scientific_content(|| false, |_| {}),
            Err(CurrentSourceVerificationError::InvalidSource)
        ));
        assert!(matches!(
            source(&unsupported, &ledger).verify_scientific_content(|| false, |_| {}),
            Err(CurrentSourceVerificationError::UnsupportedDTypePair {
                scientific_dtype: IntensityDType::Float32,
                stored_dtype: IntensityDType::Uint16,
            })
        ));

        for (scientific, stored) in [
            (IntensityDType::Uint8, IntensityDType::Float32),
            (IntensityDType::Uint16, IntensityDType::Uint8),
            (IntensityDType::Uint16, IntensityDType::Float32),
            (IntensityDType::Float32, IntensityDType::Uint8),
            (IntensityDType::Float32, IntensityDType::Uint16),
        ] {
            assert!(matches!(
                validate_dtype_pair(scientific, stored),
                Err(CurrentSourceVerificationError::UnsupportedDTypePair {
                    scientific_dtype,
                    stored_dtype,
                }) if scientific_dtype == scientific && stored_dtype == stored
            ));
        }
    }

    #[test]
    fn validity_canonicalization_is_lsb_exact_and_float_rules_are_closed() {
        assert_eq!(
            scientific_validity(ResourceValidity::AllValid, 10, &[]).unwrap(),
            [0xff, 0x03]
        );
        assert_eq!(
            scientific_validity(ResourceValidity::BitMask, 3, &[0b0000_0101]).unwrap(),
            [0b0000_0101]
        );
        assert!(matches!(
            scientific_validity(ResourceValidity::BitMask, 3, &[0b1000_0101]),
            Err(CurrentSourceVerificationError::InvalidSource)
        ));
        let validity = [0b0000_0101];
        assert_eq!(
            canonical_scientific_values(
                IntensityDType::Uint8,
                IntensityDType::Uint8,
                3,
                &[0, 255, 9],
                &validity,
            )
            .unwrap(),
            [0, 0, 9],
            "valid zero remains data and invalid nonzero becomes zero"
        );

        let stored = [
            f32::NAN.to_bits().to_le_bytes(),
            1.5_f32.to_bits().to_le_bytes(),
        ]
        .concat();
        let canonical = canonical_scientific_values(
            IntensityDType::Float32,
            IntensityDType::Float32,
            2,
            &stored,
            &[0b0000_0010],
        )
        .unwrap();
        assert_eq!(&canonical[..4], &[0, 0, 0, 0]);
        assert_eq!(&canonical[4..], &1.5_f32.to_bits().to_le_bytes());
        assert_eq!(
            canonical_scientific_values(
                IntensityDType::Float32,
                IntensityDType::Float32,
                1,
                &(-0.0_f32).to_bits().to_le_bytes(),
                &[1],
            )
            .unwrap(),
            (-0.0_f32).to_bits().to_le_bytes()
        );
        assert!(matches!(
            canonical_scientific_values(
                IntensityDType::Float32,
                IntensityDType::Float32,
                1,
                &f32::INFINITY.to_bits().to_le_bytes(),
                &[1],
            ),
            Err(CurrentSourceVerificationError::InvalidSource)
        ));
        assert_eq!(
            canonical_scientific_values(
                IntensityDType::Uint8,
                IntensityDType::Uint16,
                1,
                &256_u16.to_le_bytes(),
                &[0],
            )
            .unwrap(),
            [0],
            "an invalid widened sample canonicalizes to zero before range conversion"
        );
        assert!(matches!(
            canonical_scientific_values(
                IntensityDType::Uint8,
                IntensityDType::Uint8,
                3,
                &[1, 2, 3],
                &[0b1000_0111],
            ),
            Err(CurrentSourceVerificationError::InvalidSource)
        ));
        assert!(matches!(
            canonical_scientific_values(
                IntensityDType::Uint8,
                IntensityDType::Uint16,
                1,
                &256_u16.to_le_bytes(),
                &[1],
            ),
            Err(CurrentSourceVerificationError::InvalidSource)
        ));

        let tempdir = tempfile::tempdir().unwrap();
        let root = tempdir.path().join("validity.m4d");
        write_u8(&root, &[0, 255, 9], Some(&[1, 0, 1]));
        let ledger = TestLedger::new(8 * 1024 * 1024, 8 * 1024 * 1024);
        assert_eq!(verify(&source(&root, &ledger)).report().validity_bytes(), 1);
    }

    #[test]
    fn drift_links_and_changed_verified_reopen_fail_closed_without_source_writes() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = tempdir.path().join("drift.m4d");
        write_u8(&root, &[7], None);
        let callback_drift_path = root.join("zz-callback-drift");
        fs::write(&callback_drift_path, b"aaaa").unwrap();
        let ledger = TestLedger::new(8 * 1024 * 1024, 8 * 1024 * 1024);
        let source = source(&root, &ledger);
        let before = tree_bytes(&root);
        let verification = verify(&source);
        assert_eq!(tree_bytes(&root), before);

        let manifest_path = root.join("mirante4d.json");
        let manifest = fs::read(&manifest_path).unwrap();
        let mut changed_manifest = manifest.clone();
        changed_manifest[0] ^= 1;
        fs::write(&manifest_path, changed_manifest).unwrap();
        assert!(matches!(
            CurrentDatasetSource::open_verified(
                &root,
                &verification,
                ledger_trait(&ledger),
                || false,
                |_| {},
            ),
            Err(CurrentSourceVerificationError::SourceChanged)
        ));
        fs::write(&manifest_path, &manifest).unwrap();

        fs::remove_file(&manifest_path).unwrap();
        assert!(matches!(
            CurrentDatasetSource::open_verified(
                &root,
                &verification,
                ledger_trait(&ledger),
                || false,
                |_| {},
            ),
            Err(CurrentSourceVerificationError::SourceChanged)
        ));
        fs::write(&manifest_path, &manifest).unwrap();

        fs::write(root.join("extra"), b"changed").unwrap();
        assert!(matches!(
            CurrentDatasetSource::open_verified(
                &root,
                &verification,
                ledger_trait(&ledger),
                || false,
                |_| {},
            ),
            Err(CurrentSourceVerificationError::SourceChanged)
        ));
        fs::remove_file(root.join("extra")).unwrap();

        std::os::unix::fs::symlink("mirante4d.json", root.join("unsafe-link")).unwrap();
        assert!(matches!(
            source.verify_scientific_content(|| false, |_| {}),
            Err(CurrentSourceVerificationError::SourceChanged)
        ));
        fs::remove_file(root.join("unsafe-link")).unwrap();

        let socket_path = root.join("unsafe-socket");
        let socket = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
        assert!(matches!(
            source.verify_scientific_content(|| false, |_| {}),
            Err(CurrentSourceVerificationError::SourceChanged)
        ));
        drop(socket);
        fs::remove_file(socket_path).unwrap();

        fs::create_dir(root.join("empty-drift")).unwrap();
        assert!(matches!(
            CurrentDatasetSource::open_verified(
                &root,
                &verification,
                ledger_trait(&ledger),
                || false,
                |_| {},
            ),
            Err(CurrentSourceVerificationError::SourceChanged)
        ));
        fs::remove_dir(root.join("empty-drift")).unwrap();

        let callback_drift = AtomicBool::new(false);
        assert!(matches!(
            CurrentDatasetSource::open_verified(
                &root,
                &verification,
                ledger_trait(&ledger),
                || false,
                |progress| {
                    if progress.phase() == CurrentSourceVerificationPhase::PostInventory
                        && progress.total_units() != 0
                        && progress.completed_units() == progress.total_units()
                        && !callback_drift.swap(true, Ordering::SeqCst)
                    {
                        fs::write(&callback_drift_path, b"bbbb").unwrap();
                    }
                },
            ),
            Err(CurrentSourceVerificationError::SourceChanged)
        ));
        assert!(callback_drift.load(Ordering::SeqCst));
        fs::write(&callback_drift_path, b"aaaa").unwrap();

        let injected = AtomicBool::new(false);
        let error = source
            .verify_scientific_content(
                || false,
                |progress| {
                    if progress.phase() == CurrentSourceVerificationPhase::ScientificScan
                        && !injected.swap(true, Ordering::SeqCst)
                    {
                        fs::write(root.join("mid-scan"), b"drift").unwrap();
                    }
                },
            )
            .unwrap_err();
        assert!(matches!(
            error,
            CurrentSourceVerificationError::SourceChanged
        ));
        assert!(injected.load(Ordering::SeqCst));
    }

    #[test]
    fn cancellation_and_capacity_release_every_verification_charge() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = tempdir.path().join("cancel.m4d");
        write_u8(&root, &(0..=255).collect::<Vec<_>>(), None);
        let before = tree_bytes(&root);

        let tiny = TestLedger::new(8 * 1024 * 1024, 1);
        let tiny_source = source(&root, &tiny);
        assert!(matches!(
            tiny_source.verify_scientific_content(|| false, |_| {}),
            Err(CurrentSourceVerificationError::Capacity(
                CpuLedgerError::CapacityExceeded {
                    category: CpuLedgerCategory::InFlightDecode,
                    ..
                }
            ))
        ));
        assert_eq!(tiny.used(CpuLedgerCategory::InFlightDecode), 0);

        let ledger = TestLedger::new(8 * 1024 * 1024, 8 * 1024 * 1024);
        let source = source(&root, &ledger);
        let armed = AtomicBool::new(false);
        let checks = AtomicUsize::new(0);
        let error = source
            .verify_scientific_content(
                || armed.load(Ordering::SeqCst) && checks.fetch_add(1, Ordering::SeqCst) >= 2,
                |progress| {
                    if progress.phase() == CurrentSourceVerificationPhase::ScientificScan {
                        armed.store(true, Ordering::SeqCst);
                    }
                },
            )
            .unwrap_err();
        assert!(matches!(error, CurrentSourceVerificationError::Cancelled));
        assert!(ledger.peak(CpuLedgerCategory::InFlightDecode) > 0);
        assert_eq!(ledger.used(CpuLedgerCategory::InFlightDecode), 0);
        assert_eq!(tree_bytes(&root), before);
    }

    #[test]
    fn inventory_bounds_reject_limit_plus_one_without_allocating_a_tree() {
        assert_eq!(
            checked_inventory_entry_count(INVENTORY_ENTRY_COUNT_MAX - 1, 1).unwrap(),
            INVENTORY_ENTRY_COUNT_MAX
        );
        assert!(matches!(
            checked_inventory_entry_count(INVENTORY_ENTRY_COUNT_MAX, 1),
            Err(CurrentSourceVerificationError::InventoryCapacity)
        ));
        assert!(check_inventory_directory_fanout(INVENTORY_DIRECTORY_FANOUT_MAX).is_ok());
        assert!(matches!(
            check_inventory_directory_fanout(INVENTORY_DIRECTORY_FANOUT_MAX + 1),
            Err(CurrentSourceVerificationError::InventoryCapacity)
        ));
        assert_eq!(
            checked_inventory_depth(INVENTORY_DEPTH_MAX - 1).unwrap(),
            INVENTORY_DEPTH_MAX
        );
        assert!(matches!(
            checked_inventory_depth(INVENTORY_DEPTH_MAX),
            Err(CurrentSourceVerificationError::InventoryCapacity)
        ));
        assert!(joined_relative_path(&[], &vec![b'x'; INVENTORY_PATH_BYTES_MAX]).is_ok());
        assert!(matches!(
            joined_relative_path(&[], &vec![b'x'; INVENTORY_PATH_BYTES_MAX + 1]),
            Err(CurrentSourceVerificationError::InventoryCapacity)
        ));
    }

    fn copy_tree(source: &Path, destination: &Path) {
        fs::create_dir(destination).unwrap();
        for entry in fs::read_dir(source).unwrap().map(Result::unwrap) {
            let destination_entry = destination.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &destination_entry);
            } else {
                fs::copy(entry.path(), destination_entry).unwrap();
            }
        }
    }

    fn tree_bytes(root: &Path) -> BTreeMap<Vec<u8>, Vec<u8>> {
        fn visit(root: &Path, directory: &Path, output: &mut BTreeMap<Vec<u8>, Vec<u8>>) {
            let mut entries = fs::read_dir(directory)
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let metadata = fs::symlink_metadata(entry.path()).unwrap();
                if metadata.is_dir() {
                    visit(root, &entry.path(), output);
                } else if metadata.is_file() {
                    output.insert(
                        entry
                            .path()
                            .strip_prefix(root)
                            .unwrap()
                            .as_os_str()
                            .as_bytes()
                            .to_vec(),
                        fs::read(entry.path()).unwrap(),
                    );
                }
            }
        }
        let mut output = BTreeMap::new();
        visit(root, root, &mut output);
        output
    }
}

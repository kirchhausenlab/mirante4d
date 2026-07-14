//! Target-package adapter for the storage-independent dataset contract.

#![allow(
    clippy::result_large_err,
    reason = "the frozen DatasetSource contract requires context-rich typed faults"
)]

use std::sync::Arc;

use mirante4d_dataset::{
    CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError, DatasetCatalog,
    DatasetCatalogError, DatasetLayer, DatasetResourceKey, DatasetScale, DatasetSource,
    DatasetSourceFault, DatasetSourceId, DecodeSinkError, ReservedDecodeSink,
    ResourceContractError, ResourcePayloadDescriptor, ResourceValidity, ScientificIdentityStatus,
};
use mirante4d_domain::{GridToWorld, IntensityDType, ScaleLevel, Shape3D};
use mirante4d_identity::PackageId;
use thiserror::Error;

use crate::{
    LocalBrickRead, LocalPackageCatalog, OmeLevelTransform, PackageAdmissionError, PackagePath,
    PackageReadError, PackedIndexCoordinates, ProfileValidityMode, RangeReadError,
    ShardProfileKind, VerifiedScientificPackageCapability,
};

const METADATA_MIN_BYTES: u64 = 64 * 1024;
const METADATA_ENCODED_MULTIPLIER: u64 = 8;
const SINK_WRITE_CHUNK_BYTES: usize = 8 * 1024;

/// Conservative caller reservation for one exact-plus-scientific validation.
///
/// The accepted validators retain bounded metadata, one 64 KiB hash buffer,
/// one fixed scientific tile, and one physical brick at a time. Product
/// verification acquires this from `InFlightDecode` before invoking them.
pub const PACKAGE_VALIDATION_WORKING_BYTES: u64 = 64 * 1024 * 1024;

/// Failure while binding an opened target package to the dataset contract.
#[derive(Debug, Error)]
pub enum LocalDatasetSourceOpenError {
    #[error("target-package metadata accounting overflowed")]
    MetadataAccountingOverflow,
    #[error("target-package metadata admission failed: {0}")]
    MetadataAdmission(#[source] CpuLedgerError),
    #[error("the CPU byte ledger returned an invalid metadata lease")]
    InvalidMetadataLease,
    #[error("target package does not fit a supported dataset profile: {0}")]
    Admission(#[source] PackageAdmissionError),
    #[error(transparent)]
    Catalog(#[from] DatasetCatalogError),
    #[error("target-package metadata is inconsistent: {reason}")]
    MetadataInvariant { reason: &'static str },
}

enum LocalPackageAccess {
    Provisional(Box<LocalPackageCatalog>),
    Verified(Box<VerifiedScientificPackageCapability>),
}

impl LocalPackageAccess {
    const fn storage_catalog(&self) -> &LocalPackageCatalog {
        match self {
            Self::Provisional(catalog) => catalog,
            Self::Verified(capability) => capability.catalog(),
        }
    }

    const fn package_id(&self) -> Option<PackageId> {
        match self {
            Self::Provisional(_) => None,
            Self::Verified(capability) => Some(capability.package_id()),
        }
    }

    fn read_brick(
        &self,
        coordinates: PackedIndexCoordinates,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<LocalBrickRead, PackageReadError> {
        if is_cancelled() {
            return Err(PackageReadError::Cancelled);
        }
        let read = match self {
            Self::Provisional(catalog) => catalog.read_brick_unverified(coordinates),
            Self::Verified(capability) => capability.read_brick(coordinates, &mut is_cancelled),
        }?;
        if is_cancelled() {
            Err(PackageReadError::Cancelled)
        } else {
            Ok(read)
        }
    }
}

#[derive(Clone, Copy)]
struct LayerStorageMapping {
    image: u32,
    physical_channel: u32,
    brick_shape: [u64; 3],
}

/// One target-package source for the shared dataset scheduler.
///
/// Provisional construction uses a caller-assigned opaque source ID and never
/// exposes the manifest's declared package identity. Verified construction
/// accepts only the capability issued after exact and scientific validation.
/// The caller supplies the human display label; it is UI metadata, not a
/// persisted or scientific identity.
pub struct LocalDatasetSource {
    access: LocalPackageAccess,
    catalog: Arc<DatasetCatalog>,
    mappings: Vec<LayerStorageMapping>,
    ledger: Arc<dyn CpuByteLedger>,
    _metadata_lease: Box<dyn CpuByteLease>,
}

impl LocalDatasetSource {
    pub fn from_provisional(
        storage: LocalPackageCatalog,
        source_id: DatasetSourceId,
        display_label: impl AsRef<str>,
        ledger: Arc<dyn CpuByteLedger>,
    ) -> Result<Arc<Self>, LocalDatasetSourceOpenError> {
        Self::new(
            LocalPackageAccess::Provisional(Box::new(storage)),
            ScientificIdentityStatus::Unverified(source_id),
            display_label.as_ref(),
            ledger,
        )
    }

    pub fn from_verified(
        capability: VerifiedScientificPackageCapability,
        display_label: impl AsRef<str>,
        ledger: Arc<dyn CpuByteLedger>,
    ) -> Result<Arc<Self>, LocalDatasetSourceOpenError> {
        let scientific_content_id = capability.scientific_content_id();
        Self::new(
            LocalPackageAccess::Verified(Box::new(capability)),
            ScientificIdentityStatus::Verified(scientific_content_id),
            display_label.as_ref(),
            ledger,
        )
    }

    fn new(
        access: LocalPackageAccess,
        identity: ScientificIdentityStatus,
        display_label: &str,
        ledger: Arc<dyn CpuByteLedger>,
    ) -> Result<Arc<Self>, LocalDatasetSourceOpenError> {
        let metadata_bytes = access
            .storage_catalog()
            .metadata_bytes_read()
            .checked_mul(METADATA_ENCODED_MULTIPLIER)
            .ok_or(LocalDatasetSourceOpenError::MetadataAccountingOverflow)?
            .max(METADATA_MIN_BYTES);
        let metadata_lease = ledger
            .try_acquire(CpuLedgerCategory::MetadataAndIndexes, metadata_bytes)
            .map_err(LocalDatasetSourceOpenError::MetadataAdmission)?;
        if metadata_lease.category() != CpuLedgerCategory::MetadataAndIndexes
            || metadata_lease.reserved_bytes() != metadata_bytes
        {
            return Err(LocalDatasetSourceOpenError::InvalidMetadataLease);
        }
        if matches!(&access, LocalPackageAccess::Provisional(_)) {
            access
                .storage_catalog()
                .admit_supported_dataset_profile(|| false)
                .map_err(LocalDatasetSourceOpenError::Admission)?;
        }

        let (catalog, mappings) =
            build_dataset_catalog(access.storage_catalog(), identity, display_label)?;
        Ok(Arc::new(Self {
            access,
            catalog: Arc::new(catalog),
            mappings,
            ledger,
            _metadata_lease: metadata_lease,
        }))
    }

    /// Returns the exact package identity only after full verification.
    pub const fn package_id(&self) -> Option<PackageId> {
        self.access.package_id()
    }

    fn mapping(&self, key: DatasetResourceKey) -> Result<LayerStorageMapping, DatasetSourceFault> {
        let index = usize::try_from(key.layer().ordinal())
            .map_err(|_| DatasetSourceFault::DecodeFailed { key })?;
        self.mappings
            .get(index)
            .copied()
            .ok_or(DatasetSourceFault::DecodeFailed { key })
    }

    fn checkpoint(
        sink: &dyn ReservedDecodeSink,
        key: DatasetResourceKey,
    ) -> Result<(), DatasetSourceFault> {
        if sink.is_cancelled() {
            Err(DatasetSourceFault::Cancelled { key })
        } else {
            Ok(())
        }
    }

    fn acquire_decode_scratch(
        &self,
        key: DatasetResourceKey,
        descriptor: ResourcePayloadDescriptor,
        mapping: LayerStorageMapping,
    ) -> Result<Box<dyn CpuByteLease>, DatasetSourceFault> {
        let bytes = descriptor
            .byte_len()
            .checked_add(physical_brick_working_bytes(
                key,
                descriptor.dtype(),
                descriptor.validity(),
                mapping.brick_shape[0] == 1,
            )?)
            .ok_or(DatasetSourceFault::DecodeFailed { key })?;
        let lease = self
            .ledger
            .try_acquire(CpuLedgerCategory::InFlightDecode, bytes)
            .map_err(|error| map_ledger_error(key, bytes, error))?;
        if lease.category() != CpuLedgerCategory::InFlightDecode || lease.reserved_bytes() != bytes
        {
            return Err(DatasetSourceFault::DecodeFailed { key });
        }
        Ok(lease)
    }

    fn assemble_region(
        &self,
        sink: &dyn ReservedDecodeSink,
        key: DatasetResourceKey,
        descriptor: ResourcePayloadDescriptor,
        mapping: LayerStorageMapping,
        staging: &mut [u8],
    ) -> Result<(), DatasetSourceFault> {
        let region_start = key.region().origin();
        let region_end = key.region().end_exclusive();
        let first: [u64; 3] =
            std::array::from_fn(|axis| region_start[axis] / mapping.brick_shape[axis]);
        let last: [u64; 3] =
            std::array::from_fn(|axis| (region_end[axis] - 1) / mapping.brick_shape[axis]);
        let timepoint = u32::try_from(key.timepoint().get())
            .map_err(|_| DatasetSourceFault::CorruptResource { key })?;

        for z in first[0]..=last[0] {
            for y in first[1]..=last[1] {
                for x in first[2]..=last[2] {
                    Self::checkpoint(sink, key)?;
                    let coordinates = PackedIndexCoordinates::new(
                        mapping.image,
                        key.scale().get(),
                        timepoint,
                        mapping.physical_channel,
                        coordinate_u32(key, z)?,
                        coordinate_u32(key, y)?,
                        coordinate_u32(key, x)?,
                    );
                    let brick = self
                        .access
                        .read_brick(coordinates, || sink.is_cancelled())
                        .map_err(|error| map_read_error(key, error))?;
                    copy_brick_intersection(
                        sink,
                        key,
                        descriptor,
                        mapping.brick_shape,
                        [z, y, x],
                        &brick,
                        staging,
                    )?;
                }
            }
        }
        Self::checkpoint(sink, key)
    }
}

impl DatasetSource for LocalDatasetSource {
    fn catalog(&self) -> Result<Arc<DatasetCatalog>, DatasetSourceFault> {
        Ok(Arc::clone(&self.catalog))
    }

    fn decode_into(&self, sink: &mut dyn ReservedDecodeSink) -> Result<(), DatasetSourceFault> {
        let key = sink.resource_key();
        Self::checkpoint(sink, key)?;
        let descriptor = self
            .catalog
            .validate_decode_reservation(sink)
            .map_err(|reason| invalid_resource(key, reason))?;
        let mapping = self.mapping(key)?;
        let _scratch = self.acquire_decode_scratch(key, descriptor, mapping)?;
        let staging_len = usize::try_from(descriptor.byte_len())
            .map_err(|_| DatasetSourceFault::DecodeFailed { key })?;
        let mut staging = vec![0_u8; staging_len];
        self.assemble_region(sink, key, descriptor, mapping, &mut staging)?;
        write_sink_bytes(sink, key, &staging)?;
        Self::checkpoint(sink, key)?;
        sink.finish().map_err(|reason| map_sink_error(key, reason))
    }
}

fn build_dataset_catalog(
    storage: &LocalPackageCatalog,
    identity: ScientificIdentityStatus,
    display_label: &str,
) -> Result<(DatasetCatalog, Vec<LayerStorageMapping>), LocalDatasetSourceOpenError> {
    let mut layers = Vec::with_capacity(storage.science().layers().len());
    let mut mappings = Vec::with_capacity(storage.science().layers().len());

    for science in storage.science().layers() {
        let logical_layer = science.logical_layer();
        let (image, physical_channel) = storage
            .profile()
            .images()
            .iter()
            .find_map(|image| {
                image
                    .logical_layers()
                    .iter()
                    .find(|mapping| mapping.logical_layer() == logical_layer)
                    .map(|mapping| (image, mapping.physical_channel()))
            })
            .ok_or(LocalDatasetSourceOpenError::MetadataInvariant {
                reason: "a scientific layer has no physical image/channel mapping",
            })?;
        let ome_path = metadata_path(image.image_group_path())?;
        let ome =
            storage
                .ome_image(&ome_path)
                .ok_or(LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "a physical image has no opened OME metadata",
                })?;
        let mut scales = Vec::with_capacity(image.levels().len());
        let mut brick_shape = None;
        for (ordinal, level) in image.levels().iter().enumerate() {
            let array_path = metadata_path(level.pixel_path())?;
            let array = storage.zarr_array(&array_path).ok_or(
                LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "a profile pixel level has no opened Zarr metadata",
                },
            )?;
            let shape = array.shape();
            if shape.len() != 5 {
                return Err(LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "a profile pixel array is not t,c,z,y,x",
                });
            }
            let shape = Shape3D::new(shape[2], shape[3], shape[4]).map_err(|_| {
                LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "a profile pixel level has an invalid spatial shape",
                }
            })?;
            let current_brick = pixel_brick_shape(array.kind()).ok_or(
                LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "a profile pixel level uses a non-pixel storage kind",
                },
            )?;
            if brick_shape
                .replace(current_brick)
                .is_some_and(|prior| prior != current_brick)
            {
                return Err(LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "one logical layer mixes 2D and 3D physical bricks",
                });
            }
            let transform = ome.level_transforms().get(ordinal).copied().ok_or(
                LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "OME transform count differs from the profile level count",
                },
            )?;
            scales.push(DatasetScale::new(
                ScaleLevel::new(level.scale_ordinal()),
                shape,
                dataset_transform(
                    science.grid_to_world_micrometer_f64_bits(),
                    transform,
                    ordinal,
                )?,
                match level.validity_mode() {
                    ProfileValidityMode::AllValid => ResourceValidity::AllValid,
                    ProfileValidityMode::Explicit => ResourceValidity::BitMask,
                },
            ));
        }
        let layer_label = format!("Layer {}", logical_layer.ordinal() + 1);
        layers.push(DatasetLayer::new_multiscale(
            logical_layer,
            layer_label,
            science.base_shape().t(),
            science.dtype(),
            scales,
        )?);
        mappings.push(LayerStorageMapping {
            image: image.image_ordinal(),
            physical_channel,
            brick_shape: brick_shape.ok_or(LocalDatasetSourceOpenError::MetadataInvariant {
                reason: "a logical layer has no physical scale",
            })?,
        });
    }
    Ok((
        DatasetCatalog::new(display_label, identity, layers)?,
        mappings,
    ))
}

fn metadata_path(base: &PackagePath) -> Result<PackagePath, LocalDatasetSourceOpenError> {
    PackagePath::parse(&format!("{base}/zarr.json")).map_err(|_| {
        LocalDatasetSourceOpenError::MetadataInvariant {
            reason: "a profile metadata path violates the package path grammar",
        }
    })
}

fn dataset_transform(
    base: &[crate::F64Bits; 16],
    ome: OmeLevelTransform,
    level: usize,
) -> Result<GridToWorld, LocalDatasetSourceOpenError> {
    let row_major = match ome {
        OmeLevelTransform::DiagonalMicrometer {
            scale_zyx: [z, y, x],
            translation_zyx: [tz, ty, tx],
        } => [
            x.value(),
            0.0,
            0.0,
            tx.value(),
            0.0,
            y.value(),
            0.0,
            ty.value(),
            0.0,
            0.0,
            z.value(),
            tz.value(),
            0.0,
            0.0,
            0.0,
            1.0,
        ],
        OmeLevelTransform::UnitlessIdentity => {
            let exponent = u32::try_from(level).map_err(|_| {
                LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "a scale level cannot be represented",
                }
            })?;
            let factor = 2_u64.checked_pow(exponent).ok_or(
                LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "a scale transform factor overflowed",
                },
            )? as f64;
            let mut row_major = base.map(crate::F64Bits::value);
            for row in 0..4 {
                for column in 0..3 {
                    row_major[row * 4 + column] *= factor;
                }
            }
            row_major
        }
    };
    GridToWorld::from_row_major(row_major).map_err(|_| {
        LocalDatasetSourceOpenError::MetadataInvariant {
            reason: "a scale transform is not finite affine metadata",
        }
    })
}

const fn pixel_brick_shape(kind: ShardProfileKind) -> Option<[u64; 3]> {
    match kind {
        ShardProfileKind::Pixel3dUint8
        | ShardProfileKind::Pixel3dUint16
        | ShardProfileKind::Pixel3dFloat32 => Some([64, 64, 64]),
        ShardProfileKind::Pixel2dUint8
        | ShardProfileKind::Pixel2dUint16
        | ShardProfileKind::Pixel2dFloat32 => Some([1, 256, 256]),
        ShardProfileKind::Validity3d
        | ShardProfileKind::Validity2d
        | ShardProfileKind::PackedIndex => None,
    }
}

fn physical_brick_working_bytes(
    key: DatasetResourceKey,
    dtype: IntensityDType,
    validity: ResourceValidity,
    two_dimensional: bool,
) -> Result<u64, DatasetSourceFault> {
    let pixel = match (dtype, two_dimensional) {
        (IntensityDType::Uint8, false) => ShardProfileKind::Pixel3dUint8,
        (IntensityDType::Uint16, false) => ShardProfileKind::Pixel3dUint16,
        (IntensityDType::Float32, false) => ShardProfileKind::Pixel3dFloat32,
        (IntensityDType::Uint8, true) => ShardProfileKind::Pixel2dUint8,
        (IntensityDType::Uint16, true) => ShardProfileKind::Pixel2dUint16,
        (IntensityDType::Float32, true) => ShardProfileKind::Pixel2dFloat32,
    };
    let mut total = component_working_bytes(key, ShardProfileKind::PackedIndex)?
        .checked_add(component_working_bytes(key, pixel)?)
        .ok_or(DatasetSourceFault::DecodeFailed { key })?;
    if validity == ResourceValidity::BitMask {
        let kind = if two_dimensional {
            ShardProfileKind::Validity2d
        } else {
            ShardProfileKind::Validity3d
        };
        total = total
            .checked_add(component_working_bytes(key, kind)?)
            .ok_or(DatasetSourceFault::DecodeFailed { key })?;
    }
    Ok(total)
}

fn component_working_bytes(
    key: DatasetResourceKey,
    kind: ShardProfileKind,
) -> Result<u64, DatasetSourceFault> {
    u64::try_from(kind.decoded_inner_bytes())
        .ok()
        .and_then(|bytes| bytes.checked_add(u64::try_from(kind.encoded_inner_bytes_max()).ok()?))
        .and_then(|bytes| bytes.checked_add(u64::try_from(kind.index_tail_bytes()).ok()?))
        .ok_or(DatasetSourceFault::DecodeFailed { key })
}

#[allow(clippy::too_many_arguments)]
fn copy_brick_intersection(
    sink: &dyn ReservedDecodeSink,
    key: DatasetResourceKey,
    descriptor: ResourcePayloadDescriptor,
    brick_shape: [u64; 3],
    brick_coordinates: [u64; 3],
    brick: &LocalBrickRead,
    staging: &mut [u8],
) -> Result<(), DatasetSourceFault> {
    let sample_bytes = usize::from(descriptor.dtype().bytes_per_sample());
    let brick_samples = checked_product(key, brick_shape)?;
    if let Some(pixel) = brick.pixel_payload() {
        let expected = usize::try_from(
            brick_samples
                .checked_mul(sample_bytes as u64)
                .ok_or(DatasetSourceFault::CorruptResource { key })?,
        )
        .map_err(|_| DatasetSourceFault::CorruptResource { key })?;
        if pixel.len() != expected {
            return Err(DatasetSourceFault::CorruptResource { key });
        }
    }
    if descriptor.validity() == ResourceValidity::BitMask {
        if !brick.record().explicit_validity() {
            return Err(DatasetSourceFault::CorruptResource { key });
        }
        if brick.record().statistics().valid_voxel_count() != 0 {
            let expected = usize::try_from(brick_samples.div_ceil(8))
                .map_err(|_| DatasetSourceFault::CorruptResource { key })?;
            if brick.validity_payload().map(<[u8]>::len) != Some(expected) {
                return Err(DatasetSourceFault::CorruptResource { key });
            }
        }
    } else if brick.record().explicit_validity() || brick.validity_payload().is_some() {
        return Err(DatasetSourceFault::CorruptResource { key });
    }

    let brick_start = [
        checked_mul(key, brick_coordinates[0], brick_shape[0])?,
        checked_mul(key, brick_coordinates[1], brick_shape[1])?,
        checked_mul(key, brick_coordinates[2], brick_shape[2])?,
    ];
    let brick_end = checked_end(key, brick_start, brick.logical_extent_zyx())?;
    let region_start = key.region().origin();
    let region_end = key.region().end_exclusive();
    let start: [u64; 3] = std::array::from_fn(|axis| region_start[axis].max(brick_start[axis]));
    let end: [u64; 3] = std::array::from_fn(|axis| region_end[axis].min(brick_end[axis]));
    let region_shape = key.region().shape().dimensions();
    let validity_offset = usize::try_from(descriptor.value_byte_len())
        .map_err(|_| DatasetSourceFault::DecodeFailed { key })?;

    for z in start[0]..end[0] {
        for y in start[1]..end[1] {
            LocalDatasetSource::checkpoint(sink, key)?;
            for x in start[2]..end[2] {
                let source = linear_3d(
                    key,
                    [z - brick_start[0], y - brick_start[1], x - brick_start[2]],
                    brick_shape,
                )?;
                let target = linear_3d(
                    key,
                    [
                        z - region_start[0],
                        y - region_start[1],
                        x - region_start[2],
                    ],
                    region_shape,
                )?;
                let source_byte = source
                    .checked_mul(sample_bytes)
                    .ok_or(DatasetSourceFault::DecodeFailed { key })?;
                let target_byte = target
                    .checked_mul(sample_bytes)
                    .ok_or(DatasetSourceFault::DecodeFailed { key })?;
                let source_bytes = brick
                    .pixel_payload()
                    .map(|pixel| &pixel[source_byte..source_byte + sample_bytes]);
                let valid = match descriptor.validity() {
                    ResourceValidity::AllValid => true,
                    ResourceValidity::BitMask
                        if brick.record().statistics().valid_voxel_count() == 0 =>
                    {
                        false
                    }
                    ResourceValidity::BitMask => {
                        let bits = brick
                            .validity_payload()
                            .ok_or(DatasetSourceFault::CorruptResource { key })?;
                        bits[source / 8] & (1 << (source % 8)) != 0
                    }
                };
                if valid {
                    if descriptor.dtype() == IntensityDType::Float32
                        && source_bytes.is_some_and(|bytes| {
                            let bits = u32::from_le_bytes(
                                bytes.try_into().expect("float32 is four bytes"),
                            );
                            !f32::from_bits(bits).is_finite()
                        })
                    {
                        return Err(DatasetSourceFault::CorruptResource { key });
                    }
                    if let Some(bytes) = source_bytes {
                        staging[target_byte..target_byte + sample_bytes].copy_from_slice(bytes);
                    }
                    if descriptor.validity() == ResourceValidity::BitMask {
                        staging[validity_offset + target / 8] |= 1 << (target % 8);
                    }
                }
            }
        }
    }
    Ok(())
}

fn coordinate_u32(key: DatasetResourceKey, value: u64) -> Result<u32, DatasetSourceFault> {
    u32::try_from(value).map_err(|_| DatasetSourceFault::CorruptResource { key })
}

fn checked_product(key: DatasetResourceKey, values: [u64; 3]) -> Result<u64, DatasetSourceFault> {
    values.into_iter().try_fold(1_u64, |result, value| {
        result
            .checked_mul(value)
            .ok_or(DatasetSourceFault::DecodeFailed { key })
    })
}

fn checked_mul(key: DatasetResourceKey, left: u64, right: u64) -> Result<u64, DatasetSourceFault> {
    left.checked_mul(right)
        .ok_or(DatasetSourceFault::DecodeFailed { key })
}

fn checked_end(
    key: DatasetResourceKey,
    start: [u64; 3],
    extent: [u64; 3],
) -> Result<[u64; 3], DatasetSourceFault> {
    Ok([
        start[0]
            .checked_add(extent[0])
            .ok_or(DatasetSourceFault::DecodeFailed { key })?,
        start[1]
            .checked_add(extent[1])
            .ok_or(DatasetSourceFault::DecodeFailed { key })?,
        start[2]
            .checked_add(extent[2])
            .ok_or(DatasetSourceFault::DecodeFailed { key })?,
    ])
}

fn linear_3d(
    key: DatasetResourceKey,
    coordinate: [u64; 3],
    shape: [u64; 3],
) -> Result<usize, DatasetSourceFault> {
    let ordinal = coordinate[0]
        .checked_mul(shape[1])
        .and_then(|value| value.checked_add(coordinate[1]))
        .and_then(|value| value.checked_mul(shape[2]))
        .and_then(|value| value.checked_add(coordinate[2]))
        .ok_or(DatasetSourceFault::DecodeFailed { key })?;
    usize::try_from(ordinal).map_err(|_| DatasetSourceFault::DecodeFailed { key })
}

fn write_sink_bytes(
    sink: &mut dyn ReservedDecodeSink,
    key: DatasetResourceKey,
    bytes: &[u8],
) -> Result<(), DatasetSourceFault> {
    for chunk in bytes.chunks(SINK_WRITE_CHUNK_BYTES) {
        LocalDatasetSource::checkpoint(sink, key)?;
        sink.write(chunk)
            .map_err(|reason| map_sink_error(key, reason))?;
        LocalDatasetSource::checkpoint(sink, key)?;
    }
    Ok(())
}

fn invalid_resource(key: DatasetResourceKey, reason: ResourceContractError) -> DatasetSourceFault {
    DatasetSourceFault::InvalidResource {
        key,
        reason: Box::new(reason),
    }
}

fn map_ledger_error(
    key: DatasetResourceKey,
    requested_bytes: u64,
    error: CpuLedgerError,
) -> DatasetSourceFault {
    match error {
        CpuLedgerError::CapacityExceeded {
            category,
            requested_bytes,
            available_bytes,
        } => DatasetSourceFault::CapacityExceeded {
            key,
            category,
            requested_bytes,
            available_bytes,
        },
        CpuLedgerError::ShuttingDown => DatasetSourceFault::ShuttingDown {
            key,
            category: CpuLedgerCategory::InFlightDecode,
            requested_bytes,
        },
        CpuLedgerError::ZeroByteReservation => DatasetSourceFault::DecodeFailed { key },
    }
}

fn map_sink_error(key: DatasetResourceKey, reason: DecodeSinkError) -> DatasetSourceFault {
    match reason {
        DecodeSinkError::Cancelled => DatasetSourceFault::Cancelled { key },
        reason => DatasetSourceFault::SinkRejected {
            key,
            reason: Box::new(reason),
        },
    }
}

fn map_read_error(key: DatasetResourceKey, error: PackageReadError) -> DatasetSourceFault {
    match error {
        PackageReadError::Cancelled => DatasetSourceFault::Cancelled { key },
        PackageReadError::Range(RangeReadError::Io {
            kind: std::io::ErrorKind::NotFound,
            ..
        }) => DatasetSourceFault::ResourceUnavailable { key },
        _ => DatasetSourceFault::CorruptResource { key },
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        io::Write,
        path::{Component, Path, PathBuf},
        sync::{
            Arc, Mutex,
            atomic::{AtomicU64, Ordering},
        },
    };

    use mirante4d_dataset::{
        CpuByteLease, DatasetResourceIdentity, ResourcePayloadDescriptor, ResourceRegion,
    };
    use mirante4d_domain::{LogicalLayerKey, TimeIndex};

    use super::*;

    const TEST_CAPACITY_BYTES: u64 = 16 * 1024 * 1024;
    const TAR_BLOCK_BYTES: usize = 512;

    #[derive(Debug)]
    struct TestLedgerState {
        used: Mutex<[u64; 7]>,
        peak: Mutex<[u64; 7]>,
    }

    #[derive(Clone, Debug)]
    struct TestLedger {
        state: Arc<TestLedgerState>,
    }

    impl Default for TestLedger {
        fn default() -> Self {
            Self {
                state: Arc::new(TestLedgerState {
                    used: Mutex::new([0; 7]),
                    peak: Mutex::new([0; 7]),
                }),
            }
        }
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
            let available = TEST_CAPACITY_BYTES.saturating_sub(used[index]);
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

    struct TestSink {
        key: DatasetResourceKey,
        descriptor: ResourcePayloadDescriptor,
        bytes: Vec<u8>,
        finished: bool,
    }

    impl TestSink {
        fn new(key: DatasetResourceKey, descriptor: ResourcePayloadDescriptor) -> Self {
            Self {
                key,
                descriptor,
                bytes: Vec::with_capacity(usize::try_from(descriptor.byte_len()).unwrap()),
                finished: false,
            }
        }
    }

    impl ReservedDecodeSink for TestSink {
        fn resource_key(&self) -> DatasetResourceKey {
            self.key
        }

        fn payload_descriptor(&self) -> ResourcePayloadDescriptor {
            self.descriptor
        }

        fn written_bytes(&self) -> u64 {
            u64::try_from(self.bytes.len()).unwrap()
        }

        fn is_cancelled(&self) -> bool {
            false
        }

        fn write(&mut self, bytes: &[u8]) -> Result<(), DecodeSinkError> {
            if self.finished {
                return Err(DecodeSinkError::AlreadyFinished);
            }
            let attempted = self
                .bytes
                .len()
                .checked_add(bytes.len())
                .ok_or(DecodeSinkError::ByteCountOverflow)?;
            if u64::try_from(attempted).unwrap_or(u64::MAX) > self.descriptor.byte_len() {
                return Err(DecodeSinkError::ReservationExceeded {
                    reserved: self.descriptor.byte_len(),
                    attempted: u64::try_from(attempted).unwrap_or(u64::MAX),
                });
            }
            self.bytes.extend_from_slice(bytes);
            Ok(())
        }

        fn finish(&mut self) -> Result<(), DecodeSinkError> {
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

    #[test]
    fn provisional_2d_source_decodes_one_region_across_physical_bricks() {
        let fixture = TargetFixture::extract("m4d-t1-u8-2d-sparse");
        let ledger = Arc::new(TestLedger::default());
        let injected: Arc<dyn CpuByteLedger> = ledger.clone();
        let source_id = DatasetSourceId::new(41);
        let source = LocalDatasetSource::from_provisional(
            LocalPackageCatalog::open(fixture.path()).unwrap(),
            source_id,
            "Sparse target",
            injected,
        )
        .unwrap();

        assert_eq!(source.package_id(), None);
        let catalog = source.catalog().unwrap();
        assert_eq!(catalog.label(), "Sparse target");
        assert_eq!(
            catalog.scientific_identity(),
            &ScientificIdentityStatus::Unverified(source_id)
        );
        let region = ResourceRegion::new([0, 256, 255], Shape3D::new(1, 1, 770).unwrap()).unwrap();
        let key = DatasetResourceKey::new(
            DatasetResourceIdentity::Unverified(source_id),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            region,
        );
        let descriptor = catalog.resource_payload_descriptor(key).unwrap();
        let mut sink = TestSink::new(key, descriptor);
        source.decode_into(&mut sink).unwrap();

        assert!(sink.finished);
        assert_eq!(sink.bytes.len(), 770);
        let nonzero = sink
            .bytes
            .iter()
            .enumerate()
            .filter_map(|(index, value)| (*value != 0).then_some((index, *value)))
            .collect::<Vec<_>>();
        assert_eq!(nonzero, vec![(1, 7), (257, 8), (513, 9), (769, 10)]);
        assert!(ledger.peak(CpuLedgerCategory::InFlightDecode) > descriptor.byte_len());
        assert_eq!(ledger.used(CpuLedgerCategory::InFlightDecode), 0);
        assert!(ledger.used(CpuLedgerCategory::MetadataAndIndexes) > 0);
        drop(source);
        assert_eq!(ledger.used(CpuLedgerCategory::MetadataAndIndexes), 0);
    }

    #[test]
    fn verified_f32_source_uses_proved_identity_and_compact_validity() {
        let fixture = TargetFixture::extract("m4d-t1-f32-3d-validity");
        let verified = LocalPackageCatalog::open(fixture.path())
            .unwrap()
            .validate_exact_supported_package(|| false)
            .unwrap()
            .validate_scientific_content(|| false)
            .unwrap();
        let expected_package_id = verified.package_id();
        let expected_scientific_id = verified.scientific_content_id();
        let ledger = Arc::new(TestLedger::default());
        let injected: Arc<dyn CpuByteLedger> = ledger.clone();
        let source =
            LocalDatasetSource::from_verified(verified, "Finite f32 target", injected).unwrap();

        assert_eq!(source.package_id(), Some(expected_package_id));
        let catalog = source.catalog().unwrap();
        assert_eq!(
            catalog.scientific_identity(),
            &ScientificIdentityStatus::Verified(expected_scientific_id)
        );
        let region = ResourceRegion::new([0, 0, 0], Shape3D::new(1, 1, 16).unwrap()).unwrap();
        let key = DatasetResourceKey::new(
            DatasetResourceIdentity::Verified(expected_scientific_id),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            region,
        );
        let descriptor = catalog.resource_payload_descriptor(key).unwrap();
        assert_eq!(descriptor.validity(), ResourceValidity::BitMask);
        let mut sink = TestSink::new(key, descriptor);
        source.decode_into(&mut sink).unwrap();

        let values = sink.bytes[..64]
            .chunks_exact(4)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            vec![
                0x0000_0000,
                0x8000_0000,
                0x0000_0001,
                0x8000_0001,
                0x007f_ffff,
                0x807f_ffff,
                0x0080_0000,
                0x8080_0000,
                0x3f7f_ffff,
                0x3f80_0000,
                0x3f80_0001,
                0x0000_0000,
                0xbf80_0000,
                0xbf80_0001,
                0x7f7f_ffff,
                0xff7f_ffff,
            ]
        );
        assert_eq!(&sink.bytes[64..], &[0b1111_1110, 0b1111_0111]);
        assert!(values.iter().all(|bits| f32::from_bits(*bits).is_finite()));
        assert_eq!(ledger.used(CpuLedgerCategory::InFlightDecode), 0);
    }

    struct TargetFixture(PathBuf);

    impl TargetFixture {
        fn extract(case: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            assert!(
                case.bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            );
            let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(Path::parent)
                .unwrap();
            let archive = fs::read(
                repository
                    .join("fixtures/target/archives")
                    .join(format!("{case}.tar")),
            )
            .unwrap();
            let path = std::env::temp_dir().join(format!(
                "mirante4d-dataset-source-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            extract_ustar(&archive, &path);
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TargetFixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn extract_ustar(archive: &[u8], root: &Path) {
        let mut offset = 0;
        while offset + TAR_BLOCK_BYTES <= archive.len() {
            let header = &archive[offset..offset + TAR_BLOCK_BYTES];
            if header.iter().all(|byte| *byte == 0) {
                break;
            }
            assert_eq!(&header[257..263], b"ustar\0");
            let name = tar_text(&header[..100]);
            let prefix = tar_text(&header[345..500]);
            let relative = if prefix.is_empty() {
                PathBuf::from(name)
            } else {
                PathBuf::from(prefix).join(name)
            };
            assert!(!relative.as_os_str().is_empty());
            assert!(
                relative
                    .components()
                    .all(|component| { matches!(component, Component::Normal(_)) })
            );
            let size = tar_octal(&header[124..136]);
            let body_start = offset + TAR_BLOCK_BYTES;
            let body_end = body_start.checked_add(size).unwrap();
            assert!(body_end <= archive.len());
            let destination = root.join(&relative);
            match header[156] {
                b'5' => fs::create_dir(&destination).unwrap(),
                0 | b'0' => {
                    fs::create_dir_all(destination.parent().unwrap()).unwrap();
                    let mut file = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&destination)
                        .unwrap();
                    file.write_all(&archive[body_start..body_end]).unwrap();
                }
                kind => panic!("unsupported fixture archive entry type {kind}"),
            }
            offset = body_start + size.div_ceil(TAR_BLOCK_BYTES) * TAR_BLOCK_BYTES;
        }
    }

    fn tar_text(bytes: &[u8]) -> &str {
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        std::str::from_utf8(&bytes[..end]).unwrap()
    }

    fn tar_octal(bytes: &[u8]) -> usize {
        let text = tar_text(bytes).trim();
        usize::from_str_radix(text, 8).unwrap()
    }
}

use mirante4d_domain::{GridToWorld, IntensityDType, LogicalLayerKey, Shape4D};
use mirante4d_identity::{
    PackageId, SCIENTIFIC_TILE_SHAPE_TZYX, ScientificContentId, ScientificDatasetHasher,
    ScientificHashError, ScientificLayerDescriptor, ScientificLayerHasher, ScientificLayerRoot,
    ScientificTemporalCalibration as IdentityTemporalCalibration, ScientificTile,
};
use thiserror::Error;

use crate::{
    DatasetProfileAdmission, ExactPackageCapability, LocalBrickRead, LocalPackageCatalog,
    PackageReadError, PackageValidationError, PackedIndexCoordinates, ScienceTemporalKind,
};

/// Deterministic work performed by one successful scientific-content scan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScientificValidationReport {
    layer_count: u32,
    identity_tiles: u64,
    brick_reads: u64,
    logical_voxels: u64,
    canonical_value_bytes: u64,
    validity_bytes: u64,
}

impl ScientificValidationReport {
    pub const fn layer_count(self) -> u32 {
        self.layer_count
    }

    pub const fn identity_tiles(self) -> u64 {
        self.identity_tiles
    }

    pub const fn brick_reads(self) -> u64 {
        self.brick_reads
    }

    pub const fn logical_voxels(self) -> u64 {
        self.logical_voxels
    }

    pub const fn canonical_value_bytes(self) -> u64 {
        self.canonical_value_bytes
    }

    pub const fn validity_bytes(self) -> u64 {
        self.validity_bytes
    }
}

/// A package whose exact byte closure and declared scientific content both
/// passed their distinct validation contracts.
#[derive(Debug)]
pub struct VerifiedScientificPackageCapability {
    exact: ExactPackageCapability,
    scientific_content_id: ScientificContentId,
    layer_roots: Vec<ScientificLayerRoot>,
    report: ScientificValidationReport,
}

impl VerifiedScientificPackageCapability {
    pub const fn package_id(&self) -> PackageId {
        self.exact.package_id()
    }

    pub const fn scientific_content_id(&self) -> ScientificContentId {
        self.scientific_content_id
    }

    pub const fn admission(&self) -> DatasetProfileAdmission {
        self.exact.admission()
    }

    pub const fn catalog(&self) -> &LocalPackageCatalog {
        self.exact.catalog()
    }

    pub fn layer_roots(&self) -> &[ScientificLayerRoot] {
        &self.layer_roots
    }

    pub const fn validation_report(&self) -> ScientificValidationReport {
        self.report
    }

    pub fn revalidate_complete(
        &self,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), PackageValidationError> {
        self.exact.revalidate_complete(is_cancelled)
    }

    pub fn read_brick(
        &self,
        coordinates: PackedIndexCoordinates,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<LocalBrickRead, PackageReadError> {
        self.exact.read_brick(coordinates, is_cancelled)
    }
}

/// Typed failure before a verified-scientific-package capability can issue.
#[derive(Debug, Error)]
pub enum ScientificPackageValidationError {
    #[error("scientific-content validation was cancelled")]
    Cancelled,
    #[error(transparent)]
    Read(PackageReadError),
    #[error(transparent)]
    Exact(PackageValidationError),
    #[error(transparent)]
    Identity(ScientificHashError),
    #[error("scientific metadata is internally inconsistent: {reason}")]
    MetadataInvariant { reason: &'static str },
    #[error("scientific validation {metric} arithmetic overflowed")]
    ArithmeticOverflow { metric: &'static str },
    #[error("scientific validation {metric} cannot be represented on this platform")]
    PlatformLength { metric: &'static str },
    #[error("logical layer {layer} has no unique physical image/channel mapping")]
    LogicalLayerMapping { layer: u32 },
    #[error("decoded {component} brick has {actual} bytes; expected exactly {expected}")]
    BrickPayloadLength {
        component: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("computed scientific content {computed} differs from declared {declared}")]
    ScientificContentMismatch {
        declared: ScientificContentId,
        computed: ScientificContentId,
    },
}

impl ExactPackageCapability {
    /// Consumes an exact-package capability and verifies the storage-independent
    /// scientific identity from base-scale pixels and effective validity.
    ///
    /// The operation retains at most one fixed D-009 identity tile plus one
    /// decoded storage brick. It authenticates manifest authority around the
    /// whole scan, checks every consumed shard against the exact-package proof,
    /// and performs a final complete snapshot sweep before issuing the stronger
    /// capability.
    pub fn validate_scientific_content(
        self,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<VerifiedScientificPackageCapability, ScientificPackageValidationError> {
        check_cancelled(&mut is_cancelled)?;
        self.begin_scientific_scan(&mut is_cancelled)
            .map_err(map_exact_error)?;
        let (computed, layer_roots, report) = compute_scientific_content(&self, &mut is_cancelled)?;
        let declared = self.catalog().science().scientific_content_id();
        if computed != declared || self.catalog().profile().scientific_content_id() != declared {
            return Err(
                ScientificPackageValidationError::ScientificContentMismatch { declared, computed },
            );
        }
        self.finish_scientific_scan(&mut is_cancelled)
            .map_err(map_exact_error)?;
        Ok(VerifiedScientificPackageCapability {
            exact: self,
            scientific_content_id: computed,
            layer_roots,
            report,
        })
    }
}

fn compute_scientific_content(
    exact: &ExactPackageCapability,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<
    (
        ScientificContentId,
        Vec<ScientificLayerRoot>,
        ScientificValidationReport,
    ),
    ScientificPackageValidationError,
> {
    let science = exact.catalog().science();
    let layer_count = u32::try_from(science.layers().len()).map_err(|_| {
        ScientificPackageValidationError::PlatformLength {
            metric: "layer count",
        }
    })?;
    let mut dataset = ScientificDatasetHasher::new(layer_count)
        .map_err(ScientificPackageValidationError::Identity)?;
    let mut roots = Vec::with_capacity(science.layers().len());
    let mut report = ScientificValidationReport {
        layer_count,
        ..ScientificValidationReport::default()
    };

    for layer in science.layers() {
        check_cancelled(is_cancelled)?;
        let (image, physical_channel) = logical_mapping(exact.catalog(), layer.logical_layer())?;
        let temporal = match layer.temporal_calibration().kind() {
            ScienceTemporalKind::Unknown => IdentityTemporalCalibration::Unknown,
            ScienceTemporalKind::Regular => IdentityTemporalCalibration::Regular {
                step_seconds: layer
                    .temporal_calibration()
                    .regular_step_seconds()
                    .ok_or(ScientificPackageValidationError::MetadataInvariant {
                        reason: "regular time has no step",
                    })?
                    .value(),
            },
            ScienceTemporalKind::Explicit => IdentityTemporalCalibration::Explicit {
                positions_seconds: layer
                    .temporal_calibration()
                    .explicit_positions_seconds()
                    .ok_or(ScientificPackageValidationError::MetadataInvariant {
                        reason: "explicit time has no positions",
                    })?
                    .iter()
                    .map(|value| value.value())
                    .collect(),
            },
        };
        let grid_to_world = GridToWorld::from_row_major(
            layer
                .grid_to_world_micrometer_f64_bits()
                .map(|value| value.value()),
        )
        .map_err(|_| ScientificPackageValidationError::MetadataInvariant {
            reason: "scientific transform stopped being finite affine metadata",
        })?;
        let descriptor = ScientificLayerDescriptor::new(
            layer.logical_layer(),
            layer.dtype(),
            layer.base_shape(),
            temporal,
            grid_to_world,
        )
        .map_err(ScientificPackageValidationError::Identity)?;
        let mut hasher = ScientificLayerHasher::new(descriptor)
            .map_err(ScientificPackageValidationError::Identity)?;
        push_layer_tiles(
            exact,
            image,
            physical_channel,
            layer.base_shape(),
            layer.dtype(),
            &mut hasher,
            &mut report,
            is_cancelled,
        )?;
        let root = hasher
            .finalize()
            .map_err(ScientificPackageValidationError::Identity)?;
        dataset
            .push_layer(root)
            .map_err(ScientificPackageValidationError::Identity)?;
        roots.push(root);
    }
    let scientific_content_id = dataset
        .finalize()
        .map_err(ScientificPackageValidationError::Identity)?;
    Ok((scientific_content_id, roots, report))
}

fn logical_mapping(
    catalog: &LocalPackageCatalog,
    logical_layer: LogicalLayerKey,
) -> Result<(u32, u32), ScientificPackageValidationError> {
    let mut result = None;
    for image in catalog.profile().images() {
        for mapping in image.logical_layers() {
            if mapping.logical_layer() == logical_layer
                && result
                    .replace((image.image_ordinal(), mapping.physical_channel()))
                    .is_some()
            {
                return Err(ScientificPackageValidationError::LogicalLayerMapping {
                    layer: logical_layer.ordinal(),
                });
            }
        }
    }
    result.ok_or(ScientificPackageValidationError::LogicalLayerMapping {
        layer: logical_layer.ordinal(),
    })
}

#[allow(clippy::too_many_arguments)]
fn push_layer_tiles(
    exact: &ExactPackageCapability,
    image: u32,
    physical_channel: u32,
    shape: Shape4D,
    dtype: IntensityDType,
    hasher: &mut ScientificLayerHasher,
    report: &mut ScientificValidationReport,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<(), ScientificPackageValidationError> {
    let tile_counts = [
        shape.t(),
        ceil_div(shape.z(), SCIENTIFIC_TILE_SHAPE_TZYX[1])?,
        ceil_div(shape.y(), SCIENTIFIC_TILE_SHAPE_TZYX[2])?,
        ceil_div(shape.x(), SCIENTIFIC_TILE_SHAPE_TZYX[3])?,
    ];
    for t in 0..tile_counts[0] {
        for z_tile in 0..tile_counts[1] {
            for y_tile in 0..tile_counts[2] {
                for x_tile in 0..tile_counts[3] {
                    check_cancelled(is_cancelled)?;
                    let origin = [
                        t,
                        checked_mul(z_tile, SCIENTIFIC_TILE_SHAPE_TZYX[1], "tile z origin")?,
                        checked_mul(y_tile, SCIENTIFIC_TILE_SHAPE_TZYX[2], "tile y origin")?,
                        checked_mul(x_tile, SCIENTIFIC_TILE_SHAPE_TZYX[3], "tile x origin")?,
                    ];
                    let extent = [
                        1,
                        SCIENTIFIC_TILE_SHAPE_TZYX[1].min(shape.z() - origin[1]),
                        SCIENTIFIC_TILE_SHAPE_TZYX[2].min(shape.y() - origin[2]),
                        SCIENTIFIC_TILE_SHAPE_TZYX[3].min(shape.x() - origin[3]),
                    ];
                    let (validity, values) = assemble_tile(
                        exact,
                        image,
                        physical_channel,
                        dtype,
                        origin,
                        extent,
                        report,
                        is_cancelled,
                    )?;
                    hasher
                        .push_tile(ScientificTile::new(origin, extent, &validity, &values))
                        .map_err(ScientificPackageValidationError::Identity)?;
                    report.identity_tiles =
                        checked_add(report.identity_tiles, 1, "identity tile count")?;
                    let voxels = checked_product(extent, "tile voxel count")?;
                    report.logical_voxels =
                        checked_add(report.logical_voxels, voxels, "logical voxel count")?;
                    report.canonical_value_bytes = checked_add(
                        report.canonical_value_bytes,
                        to_u64(values.len(), "canonical value bytes")?,
                        "canonical value bytes",
                    )?;
                    report.validity_bytes = checked_add(
                        report.validity_bytes,
                        to_u64(validity.len(), "validity bytes")?,
                        "validity bytes",
                    )?;
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn assemble_tile(
    exact: &ExactPackageCapability,
    image: u32,
    physical_channel: u32,
    dtype: IntensityDType,
    origin_tzyx: [u64; 4],
    extent_tzyx: [u64; 4],
    report: &mut ScientificValidationReport,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<(Vec<u8>, Vec<u8>), ScientificPackageValidationError> {
    let voxel_count = checked_product(extent_tzyx, "tile voxel count")?;
    let value_bytes = voxel_count
        .checked_mul(u64::from(dtype.bytes_per_sample()))
        .ok_or(ScientificPackageValidationError::ArithmeticOverflow {
            metric: "tile value bytes",
        })?;
    let mut values = vec![0; to_usize(value_bytes, "tile value bytes")?];
    let validity_bytes =
        voxel_count
            .checked_add(7)
            .ok_or(ScientificPackageValidationError::ArithmeticOverflow {
                metric: "tile validity bytes",
            })?
            / 8;
    let mut validity = vec![0; to_usize(validity_bytes, "tile validity bytes")?];
    let brick_shape = if extent_tzyx[1] == 1 && origin_tzyx[1] == 0 {
        [1, 256, 256]
    } else {
        [64, 64, 64]
    };
    let tile_start = [origin_tzyx[1], origin_tzyx[2], origin_tzyx[3]];
    let tile_extent = [extent_tzyx[1], extent_tzyx[2], extent_tzyx[3]];
    let tile_end = checked_end(tile_start, tile_extent)?;
    let first = [
        tile_start[0] / brick_shape[0],
        tile_start[1] / brick_shape[1],
        tile_start[2] / brick_shape[2],
    ];
    let last = [
        (tile_end[0] - 1) / brick_shape[0],
        (tile_end[1] - 1) / brick_shape[1],
        (tile_end[2] - 1) / brick_shape[2],
    ];
    for bz in first[0]..=last[0] {
        for by in first[1]..=last[1] {
            for bx in first[2]..=last[2] {
                check_cancelled(is_cancelled)?;
                let coordinates = PackedIndexCoordinates::new(
                    image,
                    0,
                    to_u32(origin_tzyx[0], "time coordinate")?,
                    physical_channel,
                    to_u32(bz, "z brick coordinate")?,
                    to_u32(by, "y brick coordinate")?,
                    to_u32(bx, "x brick coordinate")?,
                );
                let brick = exact
                    .read_brick_for_scientific_scan(coordinates)
                    .map_err(map_read_error)?;
                report.brick_reads = checked_add(report.brick_reads, 1, "brick read count")?;
                copy_intersection(
                    &brick,
                    dtype,
                    brick_shape,
                    [bz, by, bx],
                    tile_start,
                    tile_end,
                    tile_extent,
                    &mut validity,
                    &mut values,
                    is_cancelled,
                )?;
            }
        }
    }
    Ok((validity, values))
}

#[allow(clippy::too_many_arguments)]
fn copy_intersection(
    brick: &LocalBrickRead,
    dtype: IntensityDType,
    brick_shape: [u64; 3],
    brick_coordinates: [u64; 3],
    tile_start: [u64; 3],
    tile_end: [u64; 3],
    tile_extent: [u64; 3],
    validity: &mut [u8],
    values: &mut [u8],
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<(), ScientificPackageValidationError> {
    let sample_bytes = usize::from(dtype.bytes_per_sample());
    let brick_capacity = checked_product(brick_shape, "brick capacity")?;
    if let Some(pixel) = brick.pixel_payload() {
        let expected = to_usize(
            brick_capacity.checked_mul(sample_bytes as u64).ok_or(
                ScientificPackageValidationError::ArithmeticOverflow {
                    metric: "brick pixel bytes",
                },
            )?,
            "brick pixel bytes",
        )?;
        if pixel.len() != expected {
            return Err(ScientificPackageValidationError::BrickPayloadLength {
                component: "pixel",
                expected,
                actual: pixel.len(),
            });
        }
    }
    if let Some(bits) = brick.validity_payload() {
        let expected = to_usize(brick_capacity.div_ceil(8), "brick validity bytes")?;
        if bits.len() != expected {
            return Err(ScientificPackageValidationError::BrickPayloadLength {
                component: "validity",
                expected,
                actual: bits.len(),
            });
        }
    }
    let brick_start = [
        checked_mul(brick_coordinates[0], brick_shape[0], "brick z origin")?,
        checked_mul(brick_coordinates[1], brick_shape[1], "brick y origin")?,
        checked_mul(brick_coordinates[2], brick_shape[2], "brick x origin")?,
    ];
    let brick_end = checked_end(brick_start, brick.logical_extent_zyx())?;
    let start = [
        tile_start[0].max(brick_start[0]),
        tile_start[1].max(brick_start[1]),
        tile_start[2].max(brick_start[2]),
    ];
    let end = [
        tile_end[0].min(brick_end[0]),
        tile_end[1].min(brick_end[1]),
        tile_end[2].min(brick_end[2]),
    ];
    for z in start[0]..end[0] {
        for y in start[1]..end[1] {
            check_cancelled(is_cancelled)?;
            for x in start[2]..end[2] {
                let source = linear_3d(
                    [z - brick_start[0], y - brick_start[1], x - brick_start[2]],
                    brick_shape,
                )?;
                let target = linear_3d(
                    [z - tile_start[0], y - tile_start[1], x - tile_start[2]],
                    tile_extent,
                )?;
                let valid = if !brick.record().explicit_validity() {
                    true
                } else if brick.record().statistics().valid_voxel_count() == 0 {
                    false
                } else {
                    let bits = brick.validity_payload().ok_or(
                        ScientificPackageValidationError::MetadataInvariant {
                            reason: "a partly valid explicit brick has no validity payload",
                        },
                    )?;
                    bits[source / 8] & (1 << (source % 8)) != 0
                };
                if valid {
                    validity[target / 8] |= 1 << (target % 8);
                    if let Some(pixel) = brick.pixel_payload() {
                        let source = source * sample_bytes;
                        let target = target * sample_bytes;
                        values[target..target + sample_bytes]
                            .copy_from_slice(&pixel[source..source + sample_bytes]);
                    }
                }
            }
        }
    }
    Ok(())
}

fn linear_3d(
    coordinate: [u64; 3],
    shape: [u64; 3],
) -> Result<usize, ScientificPackageValidationError> {
    let ordinal = coordinate[0]
        .checked_mul(shape[1])
        .and_then(|value| value.checked_add(coordinate[1]))
        .and_then(|value| value.checked_mul(shape[2]))
        .and_then(|value| value.checked_add(coordinate[2]))
        .ok_or(ScientificPackageValidationError::ArithmeticOverflow {
            metric: "brick sample ordinal",
        })?;
    to_usize(ordinal, "brick sample ordinal")
}

fn checked_end(
    start: [u64; 3],
    extent: [u64; 3],
) -> Result<[u64; 3], ScientificPackageValidationError> {
    Ok([
        checked_add(start[0], extent[0], "z extent")?,
        checked_add(start[1], extent[1], "y extent")?,
        checked_add(start[2], extent[2], "x extent")?,
    ])
}

fn checked_product<const N: usize>(
    values: [u64; N],
    metric: &'static str,
) -> Result<u64, ScientificPackageValidationError> {
    values.into_iter().try_fold(1_u64, |result, value| {
        result
            .checked_mul(value)
            .ok_or(ScientificPackageValidationError::ArithmeticOverflow { metric })
    })
}

fn ceil_div(value: u64, divisor: u64) -> Result<u64, ScientificPackageValidationError> {
    value
        .checked_add(divisor - 1)
        .map(|value| value / divisor)
        .ok_or(ScientificPackageValidationError::ArithmeticOverflow {
            metric: "identity tile count",
        })
}

fn checked_add(
    left: u64,
    right: u64,
    metric: &'static str,
) -> Result<u64, ScientificPackageValidationError> {
    left.checked_add(right)
        .ok_or(ScientificPackageValidationError::ArithmeticOverflow { metric })
}

fn checked_mul(
    left: u64,
    right: u64,
    metric: &'static str,
) -> Result<u64, ScientificPackageValidationError> {
    left.checked_mul(right)
        .ok_or(ScientificPackageValidationError::ArithmeticOverflow { metric })
}

fn to_u64(value: usize, metric: &'static str) -> Result<u64, ScientificPackageValidationError> {
    u64::try_from(value).map_err(|_| ScientificPackageValidationError::PlatformLength { metric })
}

fn to_usize(value: u64, metric: &'static str) -> Result<usize, ScientificPackageValidationError> {
    usize::try_from(value).map_err(|_| ScientificPackageValidationError::PlatformLength { metric })
}

fn to_u32(value: u64, metric: &'static str) -> Result<u32, ScientificPackageValidationError> {
    u32::try_from(value).map_err(|_| ScientificPackageValidationError::PlatformLength { metric })
}

fn check_cancelled(
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<(), ScientificPackageValidationError> {
    if is_cancelled() {
        Err(ScientificPackageValidationError::Cancelled)
    } else {
        Ok(())
    }
}

fn map_read_error(error: PackageReadError) -> ScientificPackageValidationError {
    if matches!(error, PackageReadError::Cancelled) {
        ScientificPackageValidationError::Cancelled
    } else {
        ScientificPackageValidationError::Read(error)
    }
}

fn map_exact_error(error: PackageValidationError) -> ScientificPackageValidationError {
    if matches!(error, PackageValidationError::Cancelled) {
        ScientificPackageValidationError::Cancelled
    } else {
        ScientificPackageValidationError::Exact(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_and_checked_work_accounting_fail_closed() {
        assert!(matches!(
            check_cancelled(&mut || true),
            Err(ScientificPackageValidationError::Cancelled)
        ));
        assert!(matches!(
            checked_add(u64::MAX, 1, "test"),
            Err(ScientificPackageValidationError::ArithmeticOverflow { metric: "test" })
        ));
    }

    #[test]
    fn identity_hasher_rejects_valid_nonfinite_and_accepts_invalid_zero() {
        let descriptor = ScientificLayerDescriptor::new(
            LogicalLayerKey::new(0),
            IntensityDType::Float32,
            Shape4D::new(1, 1, 1, 1).unwrap(),
            IdentityTemporalCalibration::Unknown,
            GridToWorld::identity(),
        )
        .unwrap();
        let mut nonfinite = ScientificLayerHasher::new(descriptor.clone()).unwrap();
        assert!(matches!(
            nonfinite.push_tile(ScientificTile::new(
                [0; 4],
                [1; 4],
                &[1],
                &f32::NAN.to_bits().to_le_bytes(),
            )),
            Err(ScientificHashError::NonFiniteFloatSample { .. })
        ));

        let mut invalid = ScientificLayerHasher::new(descriptor).unwrap();
        invalid
            .push_tile(ScientificTile::new(
                [0; 4],
                [1; 4],
                &[0],
                &0_u32.to_le_bytes(),
            ))
            .unwrap();
        assert!(invalid.finalize().is_ok());
    }
}

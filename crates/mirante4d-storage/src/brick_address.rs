use std::collections::BTreeMap;

use thiserror::Error;

use crate::{
    PACKED_INDEX_RECORD_BYTES, PACKED_INDEX_RECORDS_PER_INNER_CHUNK,
    PACKED_INDEX_RECORDS_PER_OUTER_SHARD, PackageObjectDescriptor, PackageObjectKind, PackagePath,
    PackedIndexCoordinates, ProfileHeader, ProfileValidityMode, ShardProfileKind,
    StorageProfileError, ZarrArrayMetadata,
};

const SPATIAL_INNER_CHUNKS_PER_SHARD_AXIS: u64 = 4;

/// Descriptor-derived storage addresses for one logical brick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalBrickAddressPlan {
    coordinates: PackedIndexCoordinates,
    record_ordinal: u64,
    logical_extent_zyx: [u64; 3],
    pixel_kind: ShardProfileKind,
    pixel_shard_path: PackagePath,
    pixel_inner_chunk: u64,
    pixel_shard_listed: bool,
    validity_shard_path: Option<PackagePath>,
    validity_inner_chunk: Option<u64>,
    validity_shard_listed: Option<bool>,
    packed_index_shard_path: PackagePath,
    packed_index_inner_chunk: u64,
    packed_index_record_byte_offset: u64,
}

impl LocalBrickAddressPlan {
    pub const fn coordinates(&self) -> PackedIndexCoordinates {
        self.coordinates
    }

    pub const fn record_ordinal(&self) -> u64 {
        self.record_ordinal
    }

    pub const fn logical_extent_zyx(&self) -> [u64; 3] {
        self.logical_extent_zyx
    }

    pub const fn pixel_kind(&self) -> ShardProfileKind {
        self.pixel_kind
    }

    pub const fn pixel_shard_path(&self) -> &PackagePath {
        &self.pixel_shard_path
    }

    pub const fn pixel_inner_chunk(&self) -> u64 {
        self.pixel_inner_chunk
    }

    pub const fn pixel_shard_listed(&self) -> bool {
        self.pixel_shard_listed
    }

    pub const fn validity_shard_path(&self) -> Option<&PackagePath> {
        self.validity_shard_path.as_ref()
    }

    pub const fn validity_inner_chunk(&self) -> Option<u64> {
        self.validity_inner_chunk
    }

    pub const fn validity_shard_listed(&self) -> Option<bool> {
        self.validity_shard_listed
    }

    pub const fn packed_index_shard_path(&self) -> &PackagePath {
        &self.packed_index_shard_path
    }

    pub const fn packed_index_inner_chunk(&self) -> u64 {
        self.packed_index_inner_chunk
    }

    pub const fn packed_index_record_byte_offset(&self) -> u64 {
        self.packed_index_record_byte_offset
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum BrickAddressError {
    #[error(transparent)]
    Path(#[from] StorageProfileError),
    #[error("image ordinal {image} is not present in the package profile")]
    UnknownImage { image: u32 },
    #[error("scale ordinal {scale} is not present in image {image}")]
    UnknownScale { image: u32, scale: u32 },
    #[error("required array metadata {path} is missing from the opened catalog")]
    MissingArrayMetadata { path: String },
    #[error("pixel array {path} has non-pixel storage kind {kind:?}")]
    UnexpectedPixelKind {
        path: String,
        kind: ShardProfileKind,
    },
    #[error("brick coordinate {axis}={coordinate} is outside count {count}")]
    CoordinateOutOfBounds {
        axis: &'static str,
        coordinate: u64,
        count: u64,
    },
    #[error("{metric} arithmetic overflowed")]
    ArithmeticOverflow { metric: &'static str },
    #[error(
        "{axis} brick-coordinate count {count} cannot be represented by packed-index u32 coordinates"
    )]
    CoordinateFieldOverflow { axis: &'static str, count: u64 },
    #[error("required packed-index shard {path} is absent from the manifest")]
    MissingPackedIndexShard { path: String },
    #[error("manifest object {path} has kind {actual:?}; expected {expected:?}")]
    DescriptorKindMismatch {
        path: String,
        expected: PackageObjectKind,
        actual: PackageObjectKind,
    },
    #[error("packed-index array cannot address derived record ordinal {record}")]
    PackedRecordOutOfBounds { record: u64 },
}

pub(crate) fn plan_local_brick_address(
    profile: &ProfileHeader,
    arrays: &BTreeMap<PackagePath, ZarrArrayMetadata>,
    descriptors: &[PackageObjectDescriptor],
    coordinates: PackedIndexCoordinates,
) -> Result<LocalBrickAddressPlan, BrickAddressError> {
    let image = profile
        .images()
        .iter()
        .find(|image| image.image_ordinal() == coordinates.image_ordinal())
        .ok_or(BrickAddressError::UnknownImage {
            image: coordinates.image_ordinal(),
        })?;
    let level = image
        .levels()
        .iter()
        .find(|level| level.scale_ordinal() == coordinates.scale())
        .ok_or(BrickAddressError::UnknownScale {
            image: coordinates.image_ordinal(),
            scale: coordinates.scale(),
        })?;

    let pixel_metadata_path = metadata_path(level.pixel_path())?;
    let pixel = arrays.get(&pixel_metadata_path).ok_or_else(|| {
        BrickAddressError::MissingArrayMetadata {
            path: pixel_metadata_path.to_string(),
        }
    })?;
    let (brick_zyx, two_dimensional) =
        pixel_brick(pixel.kind()).ok_or_else(|| BrickAddressError::UnexpectedPixelKind {
            path: pixel_metadata_path.to_string(),
            kind: pixel.kind(),
        })?;
    let shape: [u64; 5] =
        pixel
            .shape()
            .try_into()
            .map_err(|_| BrickAddressError::UnexpectedPixelKind {
                path: pixel_metadata_path.to_string(),
                kind: pixel.kind(),
            })?;
    let grid = brick_grid(shape, brick_zyx)?;
    validate_coordinate_field_counts(grid)?;
    let coordinate_values = [
        u64::from(coordinates.t()),
        u64::from(coordinates.c()),
        u64::from(coordinates.z_chunk()),
        u64::from(coordinates.y_chunk()),
        u64::from(coordinates.x_chunk()),
    ];
    for ((axis, coordinate), count) in ["t", "c", "z", "y", "x"]
        .into_iter()
        .zip(coordinate_values)
        .zip(grid)
    {
        if coordinate >= count {
            return Err(BrickAddressError::CoordinateOutOfBounds {
                axis,
                coordinate,
                count,
            });
        }
    }

    let record_ordinal = linear_record_ordinal(grid, coordinate_values)?;
    let packed_metadata_path = metadata_path(level.packed_index_path())?;
    let packed = arrays.get(&packed_metadata_path).ok_or_else(|| {
        BrickAddressError::MissingArrayMetadata {
            path: packed_metadata_path.to_string(),
        }
    })?;
    if packed
        .shape()
        .first()
        .copied()
        .is_none_or(|count| record_ordinal >= count)
    {
        return Err(BrickAddressError::PackedRecordOutOfBounds {
            record: record_ordinal,
        });
    }

    let packed_outer = record_ordinal / PACKED_INDEX_RECORDS_PER_OUTER_SHARD;
    let packed_within_outer = record_ordinal % PACKED_INDEX_RECORDS_PER_OUTER_SHARD;
    let packed_index_inner_chunk = packed_within_outer / PACKED_INDEX_RECORDS_PER_INNER_CHUNK;
    let packed_index_record_byte_offset = (packed_within_outer
        % PACKED_INDEX_RECORDS_PER_INNER_CHUNK)
        .checked_mul(PACKED_INDEX_RECORD_BYTES)
        .ok_or(BrickAddressError::ArithmeticOverflow {
            metric: "packed-index record byte offset",
        })?;
    let packed_index_shard_path =
        PackagePath::parse(&format!("{}/c/{packed_outer}/0", level.packed_index_path()))?;
    require_descriptor(
        descriptors,
        &packed_index_shard_path,
        PackageObjectKind::PackedIndexShard,
        true,
    )?;

    let t = coordinate_values[0];
    let c = coordinate_values[1];
    let z = coordinate_values[2];
    let y = coordinate_values[3];
    let x = coordinate_values[4];
    let pixel_shard_path = PackagePath::parse(&format!(
        "{}/c/{t}/{c}/{}/{}/{}",
        level.pixel_path(),
        z / SPATIAL_INNER_CHUNKS_PER_SHARD_AXIS,
        y / SPATIAL_INNER_CHUNKS_PER_SHARD_AXIS,
        x / SPATIAL_INNER_CHUNKS_PER_SHARD_AXIS
    ))?;
    let pixel_shard_listed = require_descriptor(
        descriptors,
        &pixel_shard_path,
        PackageObjectKind::PixelShard,
        false,
    )?;
    let pixel_inner_chunk = spatial_inner_chunk(z, y, x, two_dimensional);

    let (validity_shard_path, validity_inner_chunk, validity_shard_listed) =
        if level.validity_mode() == ProfileValidityMode::Explicit {
            let base = level
                .validity_path()
                .ok_or(BrickAddressError::MissingArrayMetadata {
                    path: format!(
                        "validity path for image {} scale {}",
                        coordinates.image_ordinal(),
                        coordinates.scale()
                    ),
                })?;
            let path = PackagePath::parse(&format!(
                "{base}/c/{t}/{c}/{}/{}/{}",
                z / SPATIAL_INNER_CHUNKS_PER_SHARD_AXIS,
                y / SPATIAL_INNER_CHUNKS_PER_SHARD_AXIS,
                x / SPATIAL_INNER_CHUNKS_PER_SHARD_AXIS
            ))?;
            let listed =
                require_descriptor(descriptors, &path, PackageObjectKind::ValidityShard, false)?;
            (Some(path), Some(pixel_inner_chunk), Some(listed))
        } else {
            (None, None, None)
        };

    let logical_extent_zyx = edge_extent(shape, brick_zyx, [z, y, x])?;
    Ok(LocalBrickAddressPlan {
        coordinates,
        record_ordinal,
        logical_extent_zyx,
        pixel_kind: pixel.kind(),
        pixel_shard_path,
        pixel_inner_chunk,
        pixel_shard_listed,
        validity_shard_path,
        validity_inner_chunk,
        validity_shard_listed,
        packed_index_shard_path,
        packed_index_inner_chunk,
        packed_index_record_byte_offset,
    })
}

pub(crate) fn validate_coordinate_field_counts(grid: [u64; 5]) -> Result<(), BrickAddressError> {
    const MAX_COUNT: u64 = u32::MAX as u64 + 1;
    for (axis, count) in ["t", "c", "z", "y", "x"].into_iter().zip(grid) {
        if count > MAX_COUNT {
            return Err(BrickAddressError::CoordinateFieldOverflow { axis, count });
        }
    }
    Ok(())
}

pub(crate) fn brick_grid(
    shape: [u64; 5],
    brick_zyx: [u64; 3],
) -> Result<[u64; 5], BrickAddressError> {
    Ok([
        shape[0],
        shape[1],
        ceil_divide(shape[2], brick_zyx[0])?,
        ceil_divide(shape[3], brick_zyx[1])?,
        ceil_divide(shape[4], brick_zyx[2])?,
    ])
}

fn pixel_brick(kind: ShardProfileKind) -> Option<([u64; 3], bool)> {
    match kind {
        ShardProfileKind::Pixel3dUint8
        | ShardProfileKind::Pixel3dUint16
        | ShardProfileKind::Pixel3dFloat32 => Some(([64, 64, 64], false)),
        ShardProfileKind::Pixel2dUint8
        | ShardProfileKind::Pixel2dUint16
        | ShardProfileKind::Pixel2dFloat32 => Some(([1, 256, 256], true)),
        ShardProfileKind::Validity3d
        | ShardProfileKind::Validity2d
        | ShardProfileKind::PackedIndex => None,
    }
}

fn linear_record_ordinal(grid: [u64; 5], coordinates: [u64; 5]) -> Result<u64, BrickAddressError> {
    coordinates
        .into_iter()
        .zip(grid)
        .try_fold(0_u64, |ordinal, (coordinate, count)| {
            ordinal.checked_mul(count)?.checked_add(coordinate)
        })
        .ok_or(BrickAddressError::ArithmeticOverflow {
            metric: "packed-index record ordinal",
        })
}

fn spatial_inner_chunk(z: u64, y: u64, x: u64, two_dimensional: bool) -> u64 {
    let z = if two_dimensional {
        0
    } else {
        z % SPATIAL_INNER_CHUNKS_PER_SHARD_AXIS
    };
    (z * SPATIAL_INNER_CHUNKS_PER_SHARD_AXIS + y % SPATIAL_INNER_CHUNKS_PER_SHARD_AXIS)
        * SPATIAL_INNER_CHUNKS_PER_SHARD_AXIS
        + x % SPATIAL_INNER_CHUNKS_PER_SHARD_AXIS
}

fn edge_extent(
    shape: [u64; 5],
    brick_zyx: [u64; 3],
    coordinates_zyx: [u64; 3],
) -> Result<[u64; 3], BrickAddressError> {
    let spatial = [shape[2], shape[3], shape[4]];
    let mut extent = [0; 3];
    for index in 0..3 {
        let start = coordinates_zyx[index].checked_mul(brick_zyx[index]).ok_or(
            BrickAddressError::ArithmeticOverflow {
                metric: "logical brick origin",
            },
        )?;
        extent[index] = spatial[index]
            .checked_sub(start)
            .ok_or(BrickAddressError::ArithmeticOverflow {
                metric: "logical brick edge extent",
            })?
            .min(brick_zyx[index]);
    }
    Ok(extent)
}

fn require_descriptor(
    descriptors: &[PackageObjectDescriptor],
    path: &PackagePath,
    expected: PackageObjectKind,
    required: bool,
) -> Result<bool, BrickAddressError> {
    let descriptor = descriptors
        .binary_search_by(|descriptor| descriptor.path().cmp(path))
        .ok()
        .map(|index| &descriptors[index]);
    let Some(descriptor) = descriptor else {
        if required {
            return Err(BrickAddressError::MissingPackedIndexShard {
                path: path.to_string(),
            });
        }
        return Ok(false);
    };
    if descriptor.kind() != expected {
        return Err(BrickAddressError::DescriptorKindMismatch {
            path: path.to_string(),
            expected,
            actual: descriptor.kind(),
        });
    }
    Ok(true)
}

fn metadata_path(path: &PackagePath) -> Result<PackagePath, StorageProfileError> {
    PackagePath::parse(&format!("{path}/zarr.json"))
}

fn ceil_divide(value: u64, divisor: u64) -> Result<u64, BrickAddressError> {
    value
        .checked_add(divisor - 1)
        .map(|value| value / divisor)
        .ok_or(BrickAddressError::ArithmeticOverflow {
            metric: "brick-grid ceil division",
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_exact_2d_and_3d_storage_coordinates() {
        let grid_2d = brick_grid([3, 2, 1, 1_025, 2_049], [1, 256, 256]).unwrap();
        assert_eq!(grid_2d, [3, 2, 1, 5, 9]);
        validate_coordinate_field_counts(grid_2d).unwrap();
        let coordinates_2d = [2, 1, 0, 4, 8];
        assert_eq!(linear_record_ordinal(grid_2d, coordinates_2d).unwrap(), 269);
        assert_eq!(spatial_inner_chunk(0, 4, 8, true), 0);
        assert_eq!(
            edge_extent([3, 2, 1, 1_025, 2_049], [1, 256, 256], [0, 4, 8]).unwrap(),
            [1, 1, 1]
        );

        let grid_3d = brick_grid([2, 3, 257, 129, 65], [64, 64, 64]).unwrap();
        assert_eq!(grid_3d, [2, 3, 5, 3, 2]);
        let coordinates_3d = [1, 2, 4, 2, 1];
        assert_eq!(linear_record_ordinal(grid_3d, coordinates_3d).unwrap(), 179);
        assert_eq!(spatial_inner_chunk(4, 2, 1, false), 9);
        assert_eq!(
            edge_extent([2, 3, 257, 129, 65], [64, 64, 64], [4, 2, 1]).unwrap(),
            [1, 1, 1]
        );
    }

    #[test]
    fn address_math_rejects_bounds_field_overflow_and_arithmetic_overflow() {
        assert!(matches!(
            validate_coordinate_field_counts([u64::from(u32::MAX) + 2, 1, 1, 1, 1]),
            Err(BrickAddressError::CoordinateFieldOverflow { axis: "t", .. })
        ));
        assert!(matches!(
            brick_grid([1, 1, u64::MAX, 1, 1], [64, 1, 1]),
            Err(BrickAddressError::ArithmeticOverflow { .. })
        ));
    }
}

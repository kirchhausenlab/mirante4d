use std::collections::BTreeMap;

use thiserror::Error;

use crate::{
    DirectoryInventory, DirectoryInventoryError, PackageCounts, PackageObjectDescriptor,
    PackageObjectKind, PackagePath, ProfileHeader, ProfileKind, ScaleCountRule, ScienceDescriptor,
    ShardProfileKind, StorageProfileError, ZarrArrayMetadata, checked_ceil_div, profile_limits,
};

/// Exact aggregate facts for one explicitly selected dataset-size profile.
///
/// Admission checks addressing and count ceilings only. It is not full package
/// validation and does not authorize payload bytes as belonging to the package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DatasetProfileAdmission {
    profile: ProfileKind,
    counts: PackageCounts,
}

impl DatasetProfileAdmission {
    pub const fn profile(self) -> ProfileKind {
        self.profile
    }

    pub const fn counts(self) -> PackageCounts {
        self.counts
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PackageAdmissionError {
    #[error(transparent)]
    Inventory(#[from] DirectoryInventoryError),
    #[error(transparent)]
    Profile(#[from] StorageProfileError),
    #[error("required {component} array metadata {path} is missing from the catalog")]
    MissingArrayMetadata {
        component: &'static str,
        path: String,
    },
    #[error("{component} array {path} has unexpected storage kind {kind:?}")]
    UnexpectedArrayKind {
        component: &'static str,
        path: String,
        kind: ShardProfileKind,
    },
    #[error("shard descriptor {path} does not belong to a profile-addressed array")]
    OrphanShardDescriptor { path: String },
    #[error("shard descriptor {path} has {actual} coordinates; expected {expected}")]
    ShardCoordinateCount {
        path: String,
        actual: usize,
        expected: usize,
    },
    #[error("shard descriptor {path} has an invalid unsigned coordinate")]
    InvalidShardCoordinate { path: String },
    #[error(
        "shard descriptor {path} coordinate {axis}={coordinate} is outside addressed count {count}"
    )]
    ShardCoordinateOutOfBounds {
        path: String,
        axis: usize,
        coordinate: u64,
        count: u64,
    },
    #[error("manifest lists {actual:?} for shard {path}; addressed array requires {expected:?}")]
    ShardKindMismatch {
        path: String,
        expected: PackageObjectKind,
        actual: PackageObjectKind,
    },
    #[error("inventory fixed-control count is {actual}; expected exactly 4")]
    FixedControlCountMismatch { actual: u64 },
    #[error(
        "addressed validity shard count {validity} differs from pixel count {pixel} for {path}"
    )]
    ValidityAddressMismatch {
        path: String,
        pixel: u64,
        validity: u64,
    },
}

#[derive(Clone, Debug)]
struct AddressedArray {
    object_kind: PackageObjectKind,
    coordinate_counts: Vec<u64>,
    addressed_shards: u64,
    actual_shards: u64,
}

pub(crate) struct DatasetProfileAdmissionInput<'a> {
    pub(crate) profile: &'a ProfileHeader,
    pub(crate) science: &'a ScienceDescriptor,
    pub(crate) arrays: &'a BTreeMap<PackagePath, ZarrArrayMetadata>,
    pub(crate) descriptors: &'a [PackageObjectDescriptor],
    pub(crate) inventory: DirectoryInventory,
}

pub(crate) fn admit_dataset_profile(
    input: DatasetProfileAdmissionInput<'_>,
    requested: ProfileKind,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<DatasetProfileAdmission, PackageAdmissionError> {
    let DatasetProfileAdmissionInput {
        profile,
        science,
        arrays,
        descriptors,
        inventory,
    } = input;
    check_cancelled(is_cancelled)?;
    validate_scale_rules(profile, requested, is_cancelled)?;
    let maximum_scales_per_image = profile
        .images()
        .iter()
        .map(|image| checked_len("scales per image", image.levels().len()))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max()
        .ok_or(StorageProfileError::ZeroCount {
            metric: "image count",
        })?;
    let logical_s0_bytes = logical_s0_bytes(science, is_cancelled)?;

    let mut addressed_arrays = BTreeMap::new();
    let mut logical_bricks = 0_u64;
    let mut addressed_pixel_shards = 0_u64;
    let mut addressed_validity_shards = 0_u64;
    let mut addressed_packed_index_shards = 0_u64;

    for image in profile.images() {
        check_cancelled(is_cancelled)?;
        for level in image.levels() {
            check_cancelled(is_cancelled)?;
            let pixel_metadata_path = metadata_path(level.pixel_path())?;
            let pixel = required_array(arrays, &pixel_metadata_path, "pixel")?;
            let (pixel_inner, pixel_outer) = pixel_shapes(pixel.kind()).ok_or_else(|| {
                PackageAdmissionError::UnexpectedArrayKind {
                    component: "pixel",
                    path: pixel_metadata_path.to_string(),
                    kind: pixel.kind(),
                }
            })?;
            let pixel_inner_grid = grid_counts(pixel.shape(), &pixel_inner)?;
            let pixel_outer_grid = grid_counts(pixel.shape(), &pixel_outer)?;
            let level_bricks = checked_product("logical brick count", &pixel_inner_grid)?;
            let level_pixel_shards =
                checked_product("addressed pixel shard count", &pixel_outer_grid)?;
            logical_bricks = checked_add("logical brick count", logical_bricks, level_bricks)?;
            addressed_pixel_shards = checked_add(
                "addressed pixel shard count",
                addressed_pixel_shards,
                level_pixel_shards,
            )?;
            insert_array(
                &mut addressed_arrays,
                level.pixel_path().clone(),
                PackageObjectKind::PixelShard,
                pixel_outer_grid,
                level_pixel_shards,
            )?;

            if let Some(validity_base) = level.validity_path() {
                let validity_metadata_path = metadata_path(validity_base)?;
                let validity = required_array(arrays, &validity_metadata_path, "validity")?;
                let validity_outer = validity_outer_shape(validity.kind()).ok_or_else(|| {
                    PackageAdmissionError::UnexpectedArrayKind {
                        component: "validity",
                        path: validity_metadata_path.to_string(),
                        kind: validity.kind(),
                    }
                })?;
                let validity_grid = grid_counts(validity.shape(), &validity_outer)?;
                let level_validity_shards =
                    checked_product("addressed validity shard count", &validity_grid)?;
                if level_validity_shards != level_pixel_shards {
                    return Err(PackageAdmissionError::ValidityAddressMismatch {
                        path: validity_base.to_string(),
                        pixel: level_pixel_shards,
                        validity: level_validity_shards,
                    });
                }
                addressed_validity_shards = checked_add(
                    "addressed validity shard count",
                    addressed_validity_shards,
                    level_validity_shards,
                )?;
                insert_array(
                    &mut addressed_arrays,
                    validity_base.clone(),
                    PackageObjectKind::ValidityShard,
                    validity_grid,
                    level_validity_shards,
                )?;
            }

            let packed_metadata_path = metadata_path(level.packed_index_path())?;
            let packed = required_array(arrays, &packed_metadata_path, "packed-index")?;
            if packed.kind() != ShardProfileKind::PackedIndex {
                return Err(PackageAdmissionError::UnexpectedArrayKind {
                    component: "packed-index",
                    path: packed_metadata_path.to_string(),
                    kind: packed.kind(),
                });
            }
            let level_packed_shards =
                checked_ceil_div(level_bricks, crate::PACKED_INDEX_RECORDS_PER_OUTER_SHARD)?;
            addressed_packed_index_shards = checked_add(
                "addressed packed-index shard count",
                addressed_packed_index_shards,
                level_packed_shards,
            )?;
            insert_array(
                &mut addressed_arrays,
                level.packed_index_path().clone(),
                PackageObjectKind::PackedIndexShard,
                vec![level_packed_shards, 1],
                level_packed_shards,
            )?;
        }
    }

    count_and_validate_shard_descriptors(descriptors, &mut addressed_arrays, is_cancelled)?;
    for array in addressed_arrays.values() {
        if array.object_kind == PackageObjectKind::PackedIndexShard
            && array.actual_shards != array.addressed_shards
        {
            return Err(StorageProfileError::PackedIndexShardCoverageMismatch {
                actual: array.actual_shards,
                addressed: array.addressed_shards,
            }
            .into());
        }
        if array.actual_shards > array.addressed_shards {
            return Err(StorageProfileError::ActualShardCountExceedsAddressed {
                component: shard_component(array.object_kind),
                actual: array.actual_shards,
                addressed: array.addressed_shards,
            }
            .into());
        }
    }

    if inventory.fixed_control_objects() != crate::FIXED_CONTROL_OBJECTS {
        return Err(PackageAdmissionError::FixedControlCountMismatch {
            actual: inventory.fixed_control_objects(),
        });
    }
    let counts = PackageCounts {
        maximum_scales_per_image,
        logical_s0_bytes,
        logical_bricks,
        addressed_pixel_shards,
        actual_pixel_shards: inventory.pixel_shards(),
        addressed_validity_shards,
        actual_validity_shards: inventory.validity_shards(),
        addressed_packed_index_shards,
        actual_packed_index_shards: inventory.packed_index_shards(),
        zarr_metadata_objects: inventory.zarr_metadata_objects(),
        portable_provenance_records: inventory.portable_records(),
        manifest_pages: inventory.manifest_pages(),
        total_physical_objects: inventory.regular_files(),
        directories: inventory.directories(),
        maximum_directory_depth: inventory.maximum_directory_depth(),
        maximum_directory_fan_out: inventory.maximum_directory_fan_out(),
    };
    counts.validate(requested)?;
    check_cancelled(is_cancelled)?;
    Ok(DatasetProfileAdmission {
        profile: requested,
        counts,
    })
}

fn validate_scale_rules(
    profile: &ProfileHeader,
    requested: ProfileKind,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<(), PackageAdmissionError> {
    let rule = profile_limits(requested).scales;
    for image in profile.images() {
        check_cancelled(is_cancelled)?;
        let actual = checked_len("scales per image", image.levels().len())?;
        match rule {
            ScaleCountRule::Maximum(maximum) if actual > maximum => {
                return Err(StorageProfileError::CeilingExceeded {
                    profile: requested.name(),
                    metric: "scales per image",
                    actual,
                    maximum,
                }
                .into());
            }
            ScaleCountRule::Exact(expected) if actual != expected => {
                return Err(StorageProfileError::ExactCountMismatch {
                    profile: requested.name(),
                    metric: "scales per image",
                    actual,
                    expected,
                }
                .into());
            }
            ScaleCountRule::Maximum(_) | ScaleCountRule::Exact(_) => {}
        }
    }
    Ok(())
}

fn logical_s0_bytes(
    science: &ScienceDescriptor,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<u64, PackageAdmissionError> {
    science.layers().iter().try_fold(0_u64, |total, layer| {
        check_cancelled(is_cancelled)?;
        let shape = layer.base_shape();
        let samples = checked_product("logical S0 sample count", &shape.dimensions())?;
        let bytes = checked_mul(
            "logical S0 byte count",
            samples,
            u64::from(layer.dtype().bytes_per_sample()),
        )?;
        Ok(checked_add("logical S0 byte count", total, bytes)?)
    })
}

fn required_array<'a>(
    arrays: &'a BTreeMap<PackagePath, ZarrArrayMetadata>,
    path: &PackagePath,
    component: &'static str,
) -> Result<&'a ZarrArrayMetadata, PackageAdmissionError> {
    arrays
        .get(path)
        .ok_or_else(|| PackageAdmissionError::MissingArrayMetadata {
            component,
            path: path.to_string(),
        })
}

fn metadata_path(base: &PackagePath) -> Result<PackagePath, StorageProfileError> {
    PackagePath::parse(&format!("{base}/zarr.json"))
}

fn insert_array(
    arrays: &mut BTreeMap<PackagePath, AddressedArray>,
    base: PackagePath,
    object_kind: PackageObjectKind,
    coordinate_counts: Vec<u64>,
    addressed_shards: u64,
) -> Result<(), StorageProfileError> {
    if arrays
        .insert(
            base.clone(),
            AddressedArray {
                object_kind,
                coordinate_counts,
                addressed_shards,
                actual_shards: 0,
            },
        )
        .is_some()
    {
        return Err(StorageProfileError::DuplicatePath {
            path: base.to_string(),
        });
    }
    Ok(())
}

fn count_and_validate_shard_descriptors(
    descriptors: &[PackageObjectDescriptor],
    arrays: &mut BTreeMap<PackagePath, AddressedArray>,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<(), PackageAdmissionError> {
    for descriptor in descriptors.iter().filter(|descriptor| {
        matches!(
            descriptor.kind(),
            PackageObjectKind::PixelShard
                | PackageObjectKind::ValidityShard
                | PackageObjectKind::PackedIndexShard
        )
    }) {
        check_cancelled(is_cancelled)?;
        let path = descriptor.path();
        let (base_text, coordinates_text) = path.as_str().split_once("/c/").ok_or_else(|| {
            PackageAdmissionError::InvalidShardCoordinate {
                path: path.to_string(),
            }
        })?;
        let base = PackagePath::parse(base_text)?;
        let array =
            arrays
                .get_mut(&base)
                .ok_or_else(|| PackageAdmissionError::OrphanShardDescriptor {
                    path: path.to_string(),
                })?;
        if descriptor.kind() != array.object_kind {
            return Err(PackageAdmissionError::ShardKindMismatch {
                path: path.to_string(),
                expected: array.object_kind,
                actual: descriptor.kind(),
            });
        }
        let coordinates = coordinates_text.split('/').collect::<Vec<_>>();
        if coordinates.len() != array.coordinate_counts.len() {
            return Err(PackageAdmissionError::ShardCoordinateCount {
                path: path.to_string(),
                actual: coordinates.len(),
                expected: array.coordinate_counts.len(),
            });
        }
        for (axis, (coordinate, count)) in coordinates
            .into_iter()
            .zip(&array.coordinate_counts)
            .enumerate()
        {
            let coordinate = coordinate.parse::<u64>().map_err(|_| {
                PackageAdmissionError::InvalidShardCoordinate {
                    path: path.to_string(),
                }
            })?;
            if coordinate >= *count {
                return Err(PackageAdmissionError::ShardCoordinateOutOfBounds {
                    path: path.to_string(),
                    axis,
                    coordinate,
                    count: *count,
                });
            }
        }
        array.actual_shards = checked_add("actual shard count", array.actual_shards, 1)?;
    }
    Ok(())
}

fn check_cancelled(is_cancelled: &mut impl FnMut() -> bool) -> Result<(), PackageAdmissionError> {
    if is_cancelled() {
        Err(DirectoryInventoryError::Cancelled.into())
    } else {
        Ok(())
    }
}

fn grid_counts(shape: &[u64], chunk: &[u64]) -> Result<Vec<u64>, StorageProfileError> {
    if shape.len() != chunk.len() {
        return Err(StorageProfileError::InconsistentCount {
            metric: "array rank",
            reported: checked_len("array rank", shape.len())?,
            computed: checked_len("chunk rank", chunk.len())?,
        });
    }
    shape
        .iter()
        .zip(chunk)
        .map(|(dimension, chunk)| checked_ceil_div(*dimension, *chunk))
        .collect()
}

const fn pixel_shapes(kind: ShardProfileKind) -> Option<([u64; 5], [u64; 5])> {
    match kind {
        ShardProfileKind::Pixel3dUint8
        | ShardProfileKind::Pixel3dUint16
        | ShardProfileKind::Pixel3dFloat32 => Some(([1, 1, 64, 64, 64], [1, 1, 256, 256, 256])),
        ShardProfileKind::Pixel2dUint8
        | ShardProfileKind::Pixel2dUint16
        | ShardProfileKind::Pixel2dFloat32 => Some(([1, 1, 1, 256, 256], [1, 1, 1, 1024, 1024])),
        ShardProfileKind::Validity3d
        | ShardProfileKind::Validity2d
        | ShardProfileKind::PackedIndex => None,
    }
}

const fn validity_outer_shape(kind: ShardProfileKind) -> Option<[u64; 5]> {
    match kind {
        ShardProfileKind::Validity3d => Some([1, 1, 256, 256, 32]),
        ShardProfileKind::Validity2d => Some([1, 1, 1, 1024, 128]),
        ShardProfileKind::Pixel3dUint8
        | ShardProfileKind::Pixel3dUint16
        | ShardProfileKind::Pixel3dFloat32
        | ShardProfileKind::Pixel2dUint8
        | ShardProfileKind::Pixel2dUint16
        | ShardProfileKind::Pixel2dFloat32
        | ShardProfileKind::PackedIndex => None,
    }
}

const fn shard_component(kind: PackageObjectKind) -> &'static str {
    match kind {
        PackageObjectKind::PixelShard => "pixel",
        PackageObjectKind::ValidityShard => "validity",
        PackageObjectKind::PackedIndexShard => "packed-index",
        _ => "non-shard",
    }
}

fn checked_len(metric: &'static str, value: usize) -> Result<u64, StorageProfileError> {
    u64::try_from(value).map_err(|_| StorageProfileError::ArithmeticOverflow { metric })
}

fn checked_product(metric: &'static str, values: &[u64]) -> Result<u64, StorageProfileError> {
    values
        .iter()
        .try_fold(1_u64, |product, value| checked_mul(metric, product, *value))
}

fn checked_add(metric: &'static str, left: u64, right: u64) -> Result<u64, StorageProfileError> {
    left.checked_add(right)
        .ok_or(StorageProfileError::ArithmeticOverflow { metric })
}

fn checked_mul(metric: &'static str, left: u64, right: u64) -> Result<u64, StorageProfileError> {
    left.checked_mul(right)
        .ok_or(StorageProfileError::ArithmeticOverflow { metric })
}

#[cfg(test)]
mod tests {
    use mirante4d_domain::{IntensityDType, LogicalLayerKey, Shape4D};
    use mirante4d_identity::ScientificContentId;

    use super::*;
    use crate::{
        F64Bits, OmeInteroperabilityBase, ProfileImage, ProfileLevel, ProfileLogicalLayer,
        ProfileValidityMode, ScienceLayer, ScienceTemporalCalibration,
    };

    fn scientific_id() -> ScientificContentId {
        ScientificContentId::parse(
            "m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap()
    }

    fn identity_transform() -> [F64Bits; 16] {
        [
            "3ff0000000000000",
            "0000000000000000",
            "0000000000000000",
            "0000000000000000",
            "0000000000000000",
            "3ff0000000000000",
            "0000000000000000",
            "0000000000000000",
            "0000000000000000",
            "0000000000000000",
            "3ff0000000000000",
            "0000000000000000",
            "0000000000000000",
            "0000000000000000",
            "0000000000000000",
            "3ff0000000000000",
        ]
        .map(|bits| F64Bits::parse(bits).unwrap())
    }

    #[test]
    fn scale_rules_apply_to_every_image_without_summing() {
        let images = [7_u32, 6]
            .into_iter()
            .enumerate()
            .map(|(image, levels)| {
                let image = u32::try_from(image).unwrap();
                ProfileImage::new(
                    image,
                    vec![ProfileLogicalLayer::new(LogicalLayerKey::new(image), 0)],
                    (0..levels)
                        .map(|scale| {
                            ProfileLevel::new(image, scale, ProfileValidityMode::AllValid).unwrap()
                        })
                        .collect(),
                )
                .unwrap()
            })
            .collect();
        let profile =
            ProfileHeader::new(scientific_id(), images, 0, OmeInteroperabilityBase::Io2).unwrap();

        assert_eq!(
            validate_scale_rules(&profile, ProfileKind::Ds0, &mut || false),
            Ok(())
        );
        assert!(matches!(
            validate_scale_rules(&profile, ProfileKind::Ds3, &mut || false),
            Err(PackageAdmissionError::Profile(
                StorageProfileError::ExactCountMismatch {
                    actual: 6,
                    expected: 7,
                    ..
                }
            ))
        ));
    }

    #[test]
    fn admission_arithmetic_reproduces_frozen_3d_boundaries() {
        let cases = [
            (
                (4, 1, 119, 383, 518, 4),
                IntensityDType::Uint16,
                (524, 40, 4),
                188_871_088,
            ),
            (
                (1, 1, 600, 1_148, 998, 5),
                IntensityDType::Uint8,
                (3_314, 76, 5),
                687_422_400,
            ),
            (
                (8, 4, 256, 256, 256, 1),
                IntensityDType::Float32,
                (2_048, 32, 1),
                2_147_483_648,
            ),
            (
                (1, 1, 2_563, 2_240, 4_183, 7),
                IntensityDType::Uint8,
                (109_196, 2_014, 12),
                24_015_104_960,
            ),
            (
                (365, 1, 74, 608, 600, 4),
                IntensityDType::Float32,
                (86_870, 5_475, 8),
                39_412_992_000,
            ),
        ];
        for ((t, c, z, y, x, scales), dtype, expected_counts, expected_s0_bytes) in cases {
            let kind = match dtype {
                IntensityDType::Uint8 => ShardProfileKind::Pixel3dUint8,
                IntensityDType::Uint16 => ShardProfileKind::Pixel3dUint16,
                IntensityDType::Float32 => ShardProfileKind::Pixel3dFloat32,
            };
            let (inner, outer) = pixel_shapes(kind).unwrap();
            let mut dimensions = [z, y, x];
            let mut logical_bricks = 0_u64;
            let mut pixel_shards = 0_u64;
            let mut packed_shards = 0_u64;
            for _ in 0..scales {
                let shape = [t, c, dimensions[0], dimensions[1], dimensions[2]];
                let level_bricks =
                    checked_product("test logical bricks", &grid_counts(&shape, &inner).unwrap())
                        .unwrap();
                logical_bricks += level_bricks;
                pixel_shards +=
                    checked_product("test pixel shards", &grid_counts(&shape, &outer).unwrap())
                        .unwrap();
                packed_shards +=
                    checked_ceil_div(level_bricks, crate::PACKED_INDEX_RECORDS_PER_OUTER_SHARD)
                        .unwrap();
                dimensions = dimensions.map(|dimension| checked_ceil_div(dimension, 2).unwrap());
            }
            assert_eq!(
                (logical_bricks, pixel_shards, packed_shards),
                expected_counts
            );

            let temporal =
                ScienceTemporalCalibration::regular(F64Bits::parse("3ff0000000000000").unwrap())
                    .unwrap();
            let layers = (0..c)
                .map(|layer| {
                    ScienceLayer::new(
                        LogicalLayerKey::new(u32::try_from(layer).unwrap()),
                        Shape4D::new(t, z, y, x).unwrap(),
                        dtype,
                        temporal.clone(),
                        identity_transform(),
                    )
                    .unwrap()
                })
                .collect();
            let science = ScienceDescriptor::new(scientific_id(), layers).unwrap();
            let actual_s0 = logical_s0_bytes(&science, &mut || false).unwrap();
            assert_eq!(actual_s0, expected_s0_bytes);
        }
    }
}

use crate::{ProfileKind, ScaleCountRule, StorageProfileError, profile_limits};
use mirante4d_domain::{IntensityDType, Shape4D};

pub const PACKED_INDEX_RECORD_BYTES: u64 = 64;
pub const PACKED_INDEX_RECORDS_PER_INNER_CHUNK: u64 = 256;
pub const PACKED_INDEX_RECORDS_PER_OUTER_SHARD: u64 = 16_384;
pub const MANIFEST_DESCRIPTORS_PER_PAGE_GUARANTEED: u64 = 2_000;
/// Maximum references in the authenticated manifest root. The 1 MiB root
/// authority leaves at least 4 KiB per reference, comfortably above the
/// bounded path/digest wire representation.
pub const MANIFEST_PAGE_REFERENCES_MAX: u64 = 256;
/// Wire-level descriptor capacity follows from authenticated page capacity.
pub const MANIFEST_FORMAT_DESCRIPTORS_MAX: u64 =
    MANIFEST_DESCRIPTORS_PER_PAGE_GUARANTEED * MANIFEST_PAGE_REFERENCES_MAX;
/// Admission also keeps descriptor recovery, duplicate-path validation, and
/// manifest packing inside one explicit 64 MiB control working set. The
/// 1 KiB charge covers the bounded 512-byte wire descriptor plus owned path,
/// vector, tree-node, and allocator overhead in the current implementation.
pub const MANIFEST_DESCRIPTOR_WORKING_SET_BYTES_MAX: u64 = 64 * 1024 * 1024;
pub const MANIFEST_DESCRIPTOR_WORKING_BYTES: u64 = 1_024;
pub const MANIFEST_DESCRIPTORS_MAX: u64 = {
    let working = MANIFEST_DESCRIPTOR_WORKING_SET_BYTES_MAX / MANIFEST_DESCRIPTOR_WORKING_BYTES;
    if MANIFEST_FORMAT_DESCRIPTORS_MAX < working {
        MANIFEST_FORMAT_DESCRIPTORS_MAX
    } else {
        working
    }
};
/// A 3D outer shard contains at most 64 logical inner chunks.
pub const COMPOSITIONAL_LOGICAL_BRICKS_MAX: u64 = MANIFEST_DESCRIPTORS_MAX * 64;
pub const COMPOSITIONAL_SHARDS_PER_COMPONENT_MAX: u64 = MANIFEST_DESCRIPTORS_MAX;
pub const COMPOSITIONAL_ZARR_METADATA_OBJECTS_MAX: u64 = 4_096;
pub const COMPOSITIONAL_PHYSICAL_OBJECTS_MAX: u64 =
    MANIFEST_DESCRIPTORS_MAX + MANIFEST_PAGE_REFERENCES_MAX + FIXED_CONTROL_OBJECTS;
pub const COMPOSITIONAL_DIRECTORIES_MAX: u64 =
    COMPOSITIONAL_PHYSICAL_OBJECTS_MAX * crate::MAX_DIRECTORY_DEPTH as u64;
pub const COMPOSITIONAL_DIRECTORY_FAN_OUT_MAX: u64 = MANIFEST_DESCRIPTORS_MAX;
pub const PORTABLE_PROVENANCE_RECORDS_MAX: u64 = 14;
pub const FIXED_CONTROL_OBJECTS: u64 = 4;
pub const GLOBAL_UNCOMPRESSED_OUTER_SHARD_BYTES_MAX: u64 = 67_108_864;
pub const GLOBAL_ENCODED_OUTER_SHARD_BYTES_MAX: u64 = 83_890_176;
pub const PROFILE_PYRAMID_TERMINAL_MAX_DIMENSION: u64 = 64;
pub const PROFILE_PYRAMID_TERMINAL_VOXELS_PER_TIMEPOINT: u64 = 262_144;
pub const PROFILE_PYRAMID_SCALE_COUNT_MAX: u64 = 64;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OneBrickAmplification {
    pub cold_range_requests_max: u8,
    pub read_bytes_max: u64,
    pub read_ratio_max: f64,
    pub decoded_bytes_max: u64,
    pub decode_ratio_max: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ElidedAllFillAmplification {
    pub cold_range_requests_max: u8,
    pub read_bytes_max: u64,
    pub decoded_bytes_max: u64,
    pub pixel_or_validity_payload_read: bool,
}

pub const ELIDED_ALL_FILL_AMPLIFICATION: ElidedAllFillAmplification = ElidedAllFillAmplification {
    cold_range_requests_max: 2,
    read_bytes_max: 24_576,
    decoded_bytes_max: 17_408,
    pixel_or_validity_payload_read: false,
};

pub const fn amplification_3d(dtype: IntensityDType) -> OneBrickAmplification {
    match dtype {
        IntensityDType::Uint8 => OneBrickAmplification {
            cold_range_requests_max: 6,
            read_bytes_max: 401_408,
            read_ratio_max: 1.53125,
            decoded_bytes_max: 314_368,
            decode_ratio_max: 1.199_218_75,
        },
        IntensityDType::Uint16 => OneBrickAmplification {
            cold_range_requests_max: 6,
            read_bytes_max: 729_088,
            read_ratio_max: 1.390_625,
            decoded_bytes_max: 576_512,
            decode_ratio_max: 1.099_609_375,
        },
        IntensityDType::Float32 => OneBrickAmplification {
            cold_range_requests_max: 6,
            read_bytes_max: 1_384_448,
            read_ratio_max: 1.320_312_5,
            decoded_bytes_max: 1_100_800,
            decode_ratio_max: 1.049_804_687_5,
        },
    }
}

pub const fn amplification_2d(dtype: IntensityDType) -> OneBrickAmplification {
    match dtype {
        IntensityDType::Uint8 => OneBrickAmplification {
            cold_range_requests_max: 6,
            read_bytes_max: 124_928,
            read_ratio_max: 1.90625,
            decoded_bytes_max: 91_648,
            decode_ratio_max: 1.398_437_5,
        },
        IntensityDType::Uint16 => OneBrickAmplification {
            cold_range_requests_max: 6,
            read_bytes_max: 206_848,
            read_ratio_max: 1.578_125,
            decoded_bytes_max: 157_184,
            decode_ratio_max: 1.199_218_75,
        },
        IntensityDType::Float32 => OneBrickAmplification {
            cold_range_requests_max: 6,
            read_bytes_max: 370_688,
            read_ratio_max: 1.414_062_5,
            decoded_bytes_max: 288_256,
            decode_ratio_max: 1.099_609_375,
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileLimits {
    pub scales: ScaleCountRule,
    pub logical_s0_bytes_max: Option<u64>,
    pub logical_bricks: u64,
    pub pixel_shards: u64,
    pub validity_shards: u64,
    pub packed_index_shards: u64,
    pub zarr_metadata_objects: u64,
    pub manifest_pages: u64,
    pub total_physical_objects: u64,
    pub directories: u64,
    pub maximum_directory_fan_out: u64,
}

impl ProfileLimits {
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        scales: ScaleCountRule,
        logical_s0_bytes_max: Option<u64>,
        logical_bricks: u64,
        pixel_shards: u64,
        validity_shards: u64,
        packed_index_shards: u64,
        zarr_metadata_objects: u64,
        manifest_pages: u64,
        total_physical_objects: u64,
        directories: u64,
        maximum_directory_fan_out: u64,
    ) -> Self {
        Self {
            scales,
            logical_s0_bytes_max,
            logical_bricks,
            pixel_shards,
            validity_shards,
            packed_index_shards,
            zarr_metadata_objects,
            manifest_pages,
            total_physical_objects,
            directories,
            maximum_directory_fan_out,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PackageCounts {
    pub maximum_scales_per_image: u64,
    pub logical_s0_bytes: u64,
    pub logical_bricks: u64,
    pub addressed_pixel_shards: u64,
    pub actual_pixel_shards: u64,
    pub addressed_validity_shards: u64,
    pub actual_validity_shards: u64,
    pub addressed_packed_index_shards: u64,
    pub actual_packed_index_shards: u64,
    pub zarr_metadata_objects: u64,
    pub portable_provenance_records: u64,
    pub manifest_pages: u64,
    pub total_physical_objects: u64,
    pub directories: u64,
    pub maximum_directory_depth: u64,
    pub maximum_directory_fan_out: u64,
}

impl PackageCounts {
    pub fn validate(self, profile: ProfileKind) -> Result<(), StorageProfileError> {
        let maximum = profile_limits(profile);
        require_positive("maximum scales per image", self.maximum_scales_per_image)?;
        match maximum.scales {
            ScaleCountRule::Maximum(limit) => check(
                profile,
                "scales per image",
                self.maximum_scales_per_image,
                limit,
            )?,
            ScaleCountRule::Exact(expected) if self.maximum_scales_per_image != expected => {
                return Err(StorageProfileError::ExactCountMismatch {
                    profile: profile.name(),
                    metric: "scales per image",
                    actual: self.maximum_scales_per_image,
                    expected,
                });
            }
            ScaleCountRule::Exact(_) => {}
        }
        require_positive("logical S0 bytes", self.logical_s0_bytes)?;
        if let Some(limit) = maximum.logical_s0_bytes_max {
            check(profile, "logical S0 bytes", self.logical_s0_bytes, limit)?;
        }
        require_positive("logical brick count", self.logical_bricks)?;
        check(
            profile,
            "logical bricks",
            self.logical_bricks,
            maximum.logical_bricks,
        )?;
        require_positive("addressed pixel shard count", self.addressed_pixel_shards)?;
        check(
            profile,
            "addressed pixel shards",
            self.addressed_pixel_shards,
            maximum.pixel_shards,
        )?;
        if self.actual_pixel_shards > self.addressed_pixel_shards {
            return Err(StorageProfileError::ActualShardCountExceedsAddressed {
                component: "pixel",
                actual: self.actual_pixel_shards,
                addressed: self.addressed_pixel_shards,
            });
        }
        check(
            profile,
            "addressed validity shards",
            self.addressed_validity_shards,
            maximum.validity_shards,
        )?;
        if self.actual_validity_shards > self.addressed_validity_shards {
            return Err(StorageProfileError::ActualShardCountExceedsAddressed {
                component: "validity",
                actual: self.actual_validity_shards,
                addressed: self.addressed_validity_shards,
            });
        }
        require_positive(
            "addressed packed-index shard count",
            self.addressed_packed_index_shards,
        )?;
        check(
            profile,
            "addressed packed-index shards",
            self.addressed_packed_index_shards,
            maximum.packed_index_shards,
        )?;
        if self.actual_packed_index_shards != self.addressed_packed_index_shards {
            return Err(StorageProfileError::PackedIndexShardCoverageMismatch {
                actual: self.actual_packed_index_shards,
                addressed: self.addressed_packed_index_shards,
            });
        }
        require_positive("Zarr metadata object count", self.zarr_metadata_objects)?;
        check(
            profile,
            "Zarr metadata objects",
            self.zarr_metadata_objects,
            maximum.zarr_metadata_objects,
        )?;
        check(
            profile,
            "portable provenance records",
            self.portable_provenance_records,
            PORTABLE_PROVENANCE_RECORDS_MAX,
        )?;
        require_positive("manifest page count", self.manifest_pages)?;
        check(
            profile,
            "manifest pages",
            self.manifest_pages,
            maximum.manifest_pages,
        )?;
        let computed_total = self.recomputed_total_physical_objects()?;
        if self.total_physical_objects != computed_total {
            return Err(StorageProfileError::InconsistentCount {
                metric: "physical object count",
                reported: self.total_physical_objects,
                computed: computed_total,
            });
        }
        check(
            profile,
            "physical objects",
            self.total_physical_objects,
            maximum.total_physical_objects,
        )?;
        require_positive("directory count", self.directories)?;
        check(
            profile,
            "directories",
            self.directories,
            maximum.directories,
        )?;
        require_positive("maximum directory depth", self.maximum_directory_depth)?;
        let directory_depth_max = u64::try_from(crate::MAX_DIRECTORY_DEPTH).map_err(|_| {
            StorageProfileError::ArithmeticOverflow {
                metric: "directory depth ceiling",
            }
        })?;
        if self.maximum_directory_depth > directory_depth_max {
            return Err(StorageProfileError::CeilingExceeded {
                profile: profile.name(),
                metric: "directory depth",
                actual: self.maximum_directory_depth,
                maximum: directory_depth_max,
            });
        }
        require_positive("directory fan-out", self.maximum_directory_fan_out)?;
        check(
            profile,
            "directory fan-out",
            self.maximum_directory_fan_out,
            maximum.maximum_directory_fan_out,
        )
    }

    pub fn recomputed_total_physical_objects(self) -> Result<u64, StorageProfileError> {
        [
            self.actual_pixel_shards,
            self.actual_validity_shards,
            self.actual_packed_index_shards,
            self.zarr_metadata_objects,
            self.portable_provenance_records,
            self.manifest_pages,
            FIXED_CONTROL_OBJECTS,
        ]
        .into_iter()
        .try_fold(0_u64, |total, count| {
            checked_add("physical object count", total, count)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DatasetGeometry {
    pub t: u64,
    pub c: u64,
    pub z: u64,
    pub y: u64,
    pub x: u64,
    pub scales: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScaleCounts {
    pub logical_bricks: u64,
    pub pixel_shards: u64,
    pub packed_index_shards: u64,
}

/// Returns the active profile's complete factor-two spatial pyramid.
///
/// Time is preserved. Spatial dimensions are ceil-divided by two until both
/// terminal bounds are satisfied. The result always contains `base` as S0.
/// This is the sole geometry authority shared by package production and
/// storage-envelope calibration.
pub fn profile_pyramid_shapes(base: Shape4D) -> Result<Vec<Shape4D>, StorageProfileError> {
    let capacity = usize::try_from(PROFILE_PYRAMID_SCALE_COUNT_MAX).map_err(|_| {
        StorageProfileError::ArithmeticOverflow {
            metric: "profile pyramid scale capacity",
        }
    })?;
    let mut shapes = Vec::with_capacity(capacity);
    shapes.push(base);

    while !profile_pyramid_shape_is_terminal(*shapes.last().expect("S0 was inserted")) {
        let previous = *shapes.last().expect("S0 was inserted");
        let next = Shape4D::new(
            previous.t(),
            previous.z().div_ceil(2),
            previous.y().div_ceil(2),
            previous.x().div_ceil(2),
        )
        .map_err(|_| StorageProfileError::ArithmeticOverflow {
            metric: "profile pyramid shape",
        })?;
        if next == previous {
            return Err(StorageProfileError::PyramidDidNotProgress {
                z: previous.z(),
                y: previous.y(),
                x: previous.x(),
            });
        }
        shapes.push(next);
        let actual =
            u64::try_from(shapes.len()).map_err(|_| StorageProfileError::ArithmeticOverflow {
                metric: "profile pyramid scale count",
            })?;
        if actual > PROFILE_PYRAMID_SCALE_COUNT_MAX {
            return Err(StorageProfileError::CeilingExceeded {
                profile: "m4d-zarr3-local-1.0",
                metric: "geometry-derived scales per image",
                actual,
                maximum: PROFILE_PYRAMID_SCALE_COUNT_MAX,
            });
        }
    }
    shapes.shrink_to_fit();
    Ok(shapes)
}

pub fn profile_pyramid_shape_is_terminal(shape: Shape4D) -> bool {
    let maximum_dimension = shape.z().max(shape.y()).max(shape.x());
    maximum_dimension <= PROFILE_PYRAMID_TERMINAL_MAX_DIMENSION
        && shape
            .z()
            .checked_mul(shape.y())
            .and_then(|zy| zy.checked_mul(shape.x()))
            .is_some_and(|voxels| voxels <= PROFILE_PYRAMID_TERMINAL_VOXELS_PER_TIMEPOINT)
}

/// Counts the active geometry-derived pyramid for a validated base shape.
pub fn count_3d_profile_pyramid(
    base: Shape4D,
    channels: u64,
) -> Result<ScaleCounts, StorageProfileError> {
    let scales = u64::try_from(profile_pyramid_shapes(base)?.len()).map_err(|_| {
        StorageProfileError::ArithmeticOverflow {
            metric: "profile pyramid scale count",
        }
    })?;
    count_3d_pyramid(DatasetGeometry {
        t: base.t(),
        c: channels,
        z: base.z(),
        y: base.y(),
        x: base.x(),
        scales,
    })
}

/// Counts a factor-two ceil pyramid using the frozen 64-cubed bricks and
/// 256-cubed outer shards. This is arithmetic only and never materializes keys.
pub fn count_3d_pyramid(geometry: DatasetGeometry) -> Result<ScaleCounts, StorageProfileError> {
    if [
        geometry.t,
        geometry.c,
        geometry.z,
        geometry.y,
        geometry.x,
        geometry.scales,
    ]
    .contains(&0)
    {
        return Err(StorageProfileError::ZeroGeometry);
    }

    let mut dimensions = [geometry.z, geometry.y, geometry.x];
    let mut total = ScaleCounts::default();
    for _ in 0..geometry.scales {
        let prefix = checked_mul("time/channel count", geometry.t, geometry.c)?;
        let bricks = checked_product(
            "logical brick count",
            [
                prefix,
                checked_ceil_div(dimensions[0], 64)?,
                checked_ceil_div(dimensions[1], 64)?,
                checked_ceil_div(dimensions[2], 64)?,
            ],
        )?;
        let shards = checked_product(
            "pixel shard count",
            [
                prefix,
                checked_ceil_div(dimensions[0], 256)?,
                checked_ceil_div(dimensions[1], 256)?,
                checked_ceil_div(dimensions[2], 256)?,
            ],
        )?;
        total.logical_bricks = checked_add("logical brick count", total.logical_bricks, bricks)?;
        total.pixel_shards = checked_add("pixel shard count", total.pixel_shards, shards)?;
        total.packed_index_shards = checked_add(
            "packed-index shard count",
            total.packed_index_shards,
            checked_ceil_div(bricks, PACKED_INDEX_RECORDS_PER_OUTER_SHARD)?,
        )?;
        for dimension in &mut dimensions {
            *dimension = checked_ceil_div(*dimension, 2)?;
        }
    }
    Ok(total)
}

pub fn checked_ceil_div(value: u64, divisor: u64) -> Result<u64, StorageProfileError> {
    if divisor == 0 {
        return Err(StorageProfileError::ArithmeticOverflow {
            metric: "ceil division",
        });
    }
    Ok(value / divisor + u64::from(!value.is_multiple_of(divisor)))
}

pub fn encoded_outer_shard_limit(uncompressed: u64) -> Result<u64, StorageProfileError> {
    uncompressed
        .checked_mul(5)
        .and_then(|value| value.checked_div(4))
        .and_then(|value| value.checked_add(4_096))
        .ok_or(StorageProfileError::ArithmeticOverflow {
            metric: "encoded outer shard limit",
        })
}

pub fn encoded_inner_payload_limit(uncompressed: u64) -> Result<u64, StorageProfileError> {
    uncompressed
        .checked_mul(5)
        .and_then(|value| value.checked_div(4))
        .ok_or(StorageProfileError::ArithmeticOverflow {
            metric: "encoded inner payload limit",
        })
}

fn check(
    profile: ProfileKind,
    metric: &'static str,
    actual: u64,
    maximum: u64,
) -> Result<(), StorageProfileError> {
    if actual <= maximum {
        Ok(())
    } else {
        Err(StorageProfileError::CeilingExceeded {
            profile: profile.name(),
            metric,
            actual,
            maximum,
        })
    }
}

fn require_positive(metric: &'static str, value: u64) -> Result<(), StorageProfileError> {
    if value == 0 {
        Err(StorageProfileError::ZeroCount { metric })
    } else {
        Ok(())
    }
}

fn checked_product<const N: usize>(
    metric: &'static str,
    values: [u64; N],
) -> Result<u64, StorageProfileError> {
    values
        .into_iter()
        .try_fold(1_u64, |product, value| checked_mul(metric, product, value))
}

fn checked_mul(metric: &'static str, left: u64, right: u64) -> Result<u64, StorageProfileError> {
    left.checked_mul(right)
        .ok_or(StorageProfileError::ArithmeticOverflow { metric })
}

fn checked_add(metric: &'static str, left: u64, right: u64) -> Result<u64, StorageProfileError> {
    left.checked_add(right)
        .ok_or(StorageProfileError::ArithmeticOverflow { metric })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(t: u64, c: u64, z: u64, y: u64, x: u64, scales: u64) -> DatasetGeometry {
        DatasetGeometry {
            t,
            c,
            z,
            y,
            x,
            scales,
        }
    }

    fn minimal_package_counts(profile: ProfileKind) -> PackageCounts {
        let limits = profile_limits(profile);
        let scales = match limits.scales {
            ScaleCountRule::Maximum(_) => 1,
            ScaleCountRule::Exact(value) => value,
        };
        let mut counts = PackageCounts {
            maximum_scales_per_image: scales,
            logical_s0_bytes: 1,
            logical_bricks: 1,
            addressed_pixel_shards: 1,
            actual_pixel_shards: 1,
            addressed_validity_shards: 0,
            actual_validity_shards: 0,
            addressed_packed_index_shards: 1,
            actual_packed_index_shards: 1,
            zarr_metadata_objects: 1,
            portable_provenance_records: 0,
            manifest_pages: 1,
            total_physical_objects: 0,
            directories: 1,
            maximum_directory_depth: 1,
            maximum_directory_fan_out: 1,
        };
        counts.total_physical_objects = counts.recomputed_total_physical_objects().unwrap();
        counts
    }

    #[test]
    fn accepted_boundary_counts_recompute_exactly() {
        let cases = [
            (Shape4D::new(4, 119, 383, 518).unwrap(), 1, 5, (528, 44, 5)),
            (
                Shape4D::new(1, 600, 1_148, 998).unwrap(),
                1,
                6,
                (3_315, 77, 6),
            ),
            (
                Shape4D::new(8, 256, 256, 256).unwrap(),
                4,
                3,
                (2_336, 96, 3),
            ),
            (
                Shape4D::new(1, 2_563, 2_240, 4_183).unwrap(),
                1,
                8,
                (109_197, 2_015, 13),
            ),
            (
                Shape4D::new(365, 74, 608, 600).unwrap(),
                1,
                5,
                (87_235, 5_840, 9),
            ),
        ];
        for (base, channels, expected_scales, expected) in cases {
            assert_eq!(profile_pyramid_shapes(base).unwrap().len(), expected_scales);
            let actual = count_3d_profile_pyramid(base, channels).unwrap();
            assert_eq!(
                (
                    actual.logical_bricks,
                    actual.pixel_shards,
                    actual.packed_index_shards
                ),
                expected
            );
        }
    }

    #[test]
    fn compositional_limits_follow_manifest_and_shard_authorities() {
        let limits = profile_limits(ProfileKind::Current);
        assert_eq!(
            MANIFEST_FORMAT_DESCRIPTORS_MAX,
            MANIFEST_DESCRIPTORS_PER_PAGE_GUARANTEED * MANIFEST_PAGE_REFERENCES_MAX
        );
        assert_eq!(
            MANIFEST_DESCRIPTORS_MAX,
            MANIFEST_DESCRIPTOR_WORKING_SET_BYTES_MAX / MANIFEST_DESCRIPTOR_WORKING_BYTES
        );
        assert_eq!(limits.logical_bricks, MANIFEST_DESCRIPTORS_MAX * 64);
        assert_eq!(limits.pixel_shards, MANIFEST_DESCRIPTORS_MAX);
        assert_eq!(limits.validity_shards, MANIFEST_DESCRIPTORS_MAX);
        assert_eq!(limits.packed_index_shards, MANIFEST_DESCRIPTORS_MAX);
        assert_eq!(limits.manifest_pages, MANIFEST_PAGE_REFERENCES_MAX);
        assert_eq!(
            limits.total_physical_objects,
            MANIFEST_DESCRIPTORS_MAX + MANIFEST_PAGE_REFERENCES_MAX + FIXED_CONTROL_OBJECTS
        );
        assert_eq!(limits.logical_s0_bytes_max, None);
    }

    #[test]
    fn shared_pyramid_geometry_enforces_both_terminal_bounds() {
        let tiny = Shape4D::new(3, 8, 31, 256).unwrap();
        assert_eq!(
            profile_pyramid_shapes(tiny).unwrap(),
            vec![
                tiny,
                Shape4D::new(3, 4, 16, 128).unwrap(),
                Shape4D::new(3, 2, 8, 64).unwrap(),
            ]
        );

        let already_terminal = Shape4D::new(2, 64, 64, 64).unwrap();
        assert_eq!(
            profile_pyramid_shapes(already_terminal).unwrap(),
            vec![already_terminal]
        );

        let long_thin = Shape4D::new(1, 1, 1, 1_000_000).unwrap();
        let shapes = profile_pyramid_shapes(long_thin).unwrap();
        assert!(profile_pyramid_shape_is_terminal(*shapes.last().unwrap()));
        assert!(
            !profile_pyramid_shape_is_terminal(shapes[shapes.len() - 2]),
            "a small voxel count cannot terminate a pathologically long volume"
        );
    }

    #[test]
    fn every_representable_shape_reaches_the_terminal_contract_within_the_profile_bound() {
        let base = Shape4D::new(1, 1, 1, u64::MAX).unwrap();
        let shapes = profile_pyramid_shapes(base).unwrap();

        assert_eq!(shapes.len(), 59);
        assert_eq!(
            shapes.last().unwrap().dimensions(),
            [1, 1, 1, PROFILE_PYRAMID_TERMINAL_MAX_DIMENSION]
        );
        assert!(u64::try_from(shapes.len()).unwrap() <= PROFILE_PYRAMID_SCALE_COUNT_MAX);
    }

    #[test]
    fn frozen_encoded_limits_recompute_exactly() {
        assert_eq!(encoded_outer_shard_limit(67_108_864).unwrap(), 83_890_176);
        assert_eq!(encoded_inner_payload_limit(1_048_576).unwrap(), 1_310_720);
    }

    #[test]
    fn checked_arithmetic_rejects_zero_and_overflow() {
        assert!(count_3d_pyramid(geometry(0, 1, 1, 1, 1, 1)).is_err());
        assert_eq!(checked_ceil_div(u64::MAX, 2).unwrap(), 1_u64 << 63);
        assert!(encoded_outer_shard_limit(u64::MAX).is_err());
    }

    #[test]
    fn amplification_tables_match_the_accepted_absolute_and_ratio_limits() {
        let u8_3d = amplification_3d(IntensityDType::Uint8);
        assert_eq!(u8_3d.cold_range_requests_max, 6);
        assert_eq!(u8_3d.read_bytes_max, 401_408);
        assert_eq!(u8_3d.decoded_bytes_max, 314_368);
        assert_eq!(u8_3d.read_ratio_max, 1.53125);

        let f32_2d = amplification_2d(IntensityDType::Float32);
        assert_eq!(f32_2d.read_bytes_max, 370_688);
        assert_eq!(f32_2d.decoded_bytes_max, 288_256);
        assert_eq!(
            ELIDED_ALL_FILL_AMPLIFICATION,
            ElidedAllFillAmplification {
                cold_range_requests_max: 2,
                read_bytes_max: 24_576,
                decoded_bytes_max: 17_408,
                pixel_or_validity_payload_read: false,
            }
        );
    }

    #[test]
    fn compositional_contract_rejects_one_object_above_its_ceiling() {
        let profile = ProfileKind::Current;
        let limits = profile_limits(profile);
        let mut counts = minimal_package_counts(profile);
        counts.addressed_pixel_shards = limits.pixel_shards + 1;
        counts.total_physical_objects = counts.recomputed_total_physical_objects().unwrap();
        assert!(matches!(
            counts.validate(profile),
            Err(StorageProfileError::CeilingExceeded {
                metric: "addressed pixel shards",
                ..
            })
        ));
    }

    #[test]
    fn package_counts_reject_inconsistent_totals_and_invalid_scale_contracts() {
        let profile = ProfileKind::Current;
        assert!(minimal_package_counts(profile).validate(profile).is_ok());

        let mut underreported = minimal_package_counts(ProfileKind::Current);
        underreported.total_physical_objects -= 1;
        assert!(matches!(
            underreported.validate(ProfileKind::Current),
            Err(StorageProfileError::InconsistentCount { .. })
        ));

        let mut excessive_scale_count = minimal_package_counts(ProfileKind::Current);
        excessive_scale_count.maximum_scales_per_image =
            profile_limits(ProfileKind::Current).scales.maximum() + 1;
        assert!(matches!(
            excessive_scale_count.validate(ProfileKind::Current),
            Err(StorageProfileError::CeilingExceeded {
                metric: "scales per image",
                ..
            })
        ));

        let mut zero_scale = minimal_package_counts(ProfileKind::Current);
        zero_scale.maximum_scales_per_image = 0;
        assert!(matches!(
            zero_scale.validate(ProfileKind::Current),
            Err(StorageProfileError::ZeroCount {
                metric: "maximum scales per image"
            })
        ));
    }

    #[test]
    fn aggregate_s0_bytes_are_not_a_fixture_calibrated_admission_ceiling() {
        let mut counts = minimal_package_counts(ProfileKind::Current);
        counts.logical_s0_bytes = u64::MAX;
        assert!(counts.validate(ProfileKind::Current).is_ok());
    }
}

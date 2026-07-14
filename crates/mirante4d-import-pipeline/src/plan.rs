//! Checked deterministic import planning.

use mirante4d_domain::Shape4D;
use mirante4d_identity::{Sha256Digest, Sha256Hasher};
use mirante4d_storage::{
    PACKED_INDEX_RECORD_BYTES, ProfileKind, ScaleCountRule, ShardProfileKind, StorageProfileError,
    profile_limits,
};

use crate::{
    ImportError, ImportOptions, NoDataPolicy,
    chunk::{chunk_grid, pixel_kind, validity_kind},
    publish::{PUBLICATION_VALIDATION_BYTES_MAX, publication_shard_bytes},
    pyramid::pyramid_shapes,
};

const SPACE_OVERHEAD_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct ImportPlan {
    pub shapes: Vec<Shape4D>,
    pub is_2d: bool,
    pub explicit_validity: bool,
    pub pixel_kind: ShardProfileKind,
    pub validity_kind: ShardProfileKind,
    pub work_units: u64,
    pub spool_record_bytes: u64,
    pub logical_bricks_by_scale: Vec<u64>,
    pub plan_digest: Sha256Digest,
    pub free_space_required: u64,
}

impl ImportPlan {
    pub fn new(options: &ImportOptions) -> Result<Self, ImportError> {
        validate_request(options)?;
        let shapes = selected_shapes(options.inspection.shape, options.profile)?;
        let is_2d = options.inspection.shape.z() == 1;
        let explicit_validity = options.no_data.is_some();
        let pixel_kind = pixel_kind(options.inspection.dtype, is_2d);
        let validity_kind = validity_kind(is_2d);

        let mut logical_bricks_by_scale = Vec::with_capacity(shapes.len());
        let mut work_units = 0_u64;
        let mut addressed_pixel_shards = 0_u64;
        let mut addressed_packed_shards = 0_u64;
        let mut logical_pixel_bytes = 0_u64;
        let mut logical_validity_bytes = 0_u64;
        for shape in &shapes {
            let grid = chunk_grid([shape.z(), shape.y(), shape.x()], is_2d);
            let bricks = checked_product([
                shape.t(),
                u64::from(options.inspection.channels),
                grid[0],
                grid[1],
                grid[2],
            ])?;
            logical_bricks_by_scale.push(bricks);
            work_units = work_units
                .checked_add(bricks)
                .ok_or(ImportError::Overflow)?;

            let outer_divisor = if is_2d { [1, 4, 4] } else { [4, 4, 4] };
            let outer_shards = checked_product([
                shape.t(),
                u64::from(options.inspection.channels),
                grid[0].div_ceil(outer_divisor[0]),
                grid[1].div_ceil(outer_divisor[1]),
                grid[2].div_ceil(outer_divisor[2]),
            ])?;
            addressed_pixel_shards = addressed_pixel_shards
                .checked_add(outer_shards)
                .ok_or(ImportError::Overflow)?;
            addressed_packed_shards = addressed_packed_shards
                .checked_add(bricks.div_ceil(16_384))
                .ok_or(ImportError::Overflow)?;
            let samples = checked_product([
                shape.t(),
                u64::from(options.inspection.channels),
                shape.z(),
                shape.y(),
                shape.x(),
            ])?;
            logical_pixel_bytes = logical_pixel_bytes
                .checked_add(
                    samples
                        .checked_mul(u64::from(options.inspection.dtype.bytes_per_sample()))
                        .ok_or(ImportError::Overflow)?,
                )
                .ok_or(ImportError::Overflow)?;
            if explicit_validity {
                logical_validity_bytes = logical_validity_bytes
                    .checked_add(checked_product([
                        shape.t(),
                        u64::from(options.inspection.channels),
                        shape.z(),
                        shape.y(),
                        shape.x().div_ceil(8),
                    ])?)
                    .ok_or(ImportError::Overflow)?;
            }
        }
        validate_profile_counts(
            options,
            shapes.len(),
            work_units,
            addressed_pixel_shards,
            explicit_validity,
            addressed_packed_shards,
        )?;

        let spool_record_bytes = crate::spool::record_memory_bytes(work_units)?;
        let working_memory_required = working_memory_required(
            options,
            pixel_kind,
            validity_kind,
            is_2d,
            spool_record_bytes,
        )?;
        if options.working_memory_bytes < working_memory_required {
            return Err(ImportError::WorkingMemoryExceeded {
                required_bytes: working_memory_required,
                budget_bytes: options.working_memory_bytes,
            });
        }

        let free_space_required = free_space_required(
            work_units,
            logical_pixel_bytes,
            logical_validity_bytes,
            addressed_pixel_shards,
            addressed_packed_shards,
        )?;
        let plan_digest = plan_digest(options, &shapes);

        Ok(Self {
            shapes,
            is_2d,
            explicit_validity,
            pixel_kind,
            validity_kind,
            work_units,
            spool_record_bytes,
            logical_bricks_by_scale,
            plan_digest,
            free_space_required,
        })
    }
}

fn validate_request(options: &ImportOptions) -> Result<(), ImportError> {
    if options.inspection.channels == 0 {
        return Err(ImportError::InvalidRequest(
            "an import must contain at least one channel",
        ));
    }
    if options.inspection.maximum_decoded_chunk_bytes == 0 {
        return Err(ImportError::InvalidRequest(
            "inspection must declare a positive decoded TIFF chunk bound",
        ));
    }
    if options
        .calibration
        .spacing_zyx_um
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return Err(ImportError::InvalidRequest(
            "spatial calibration must be positive and finite",
        ));
    }
    if options
        .time_step_seconds
        .is_some_and(|value| !value.is_finite() || value <= 0.0)
    {
        return Err(ImportError::InvalidRequest(
            "time step must be positive and finite",
        ));
    }
    if options.no_data.is_some()
        && options.inspection.dtype != mirante4d_domain::IntensityDType::Uint8
    {
        return Err(ImportError::InvalidRequest(
            "the reviewed sentinel no-data rule applies only to uint8 sources",
        ));
    }
    if options.destination == options.checkpoint_directory
        || options
            .destination
            .starts_with(&options.checkpoint_directory)
        || options
            .checkpoint_directory
            .starts_with(&options.destination)
    {
        return Err(ImportError::InvalidRequest(
            "destination and checkpoint paths must be separate and unnested",
        ));
    }
    if options
        .destination
        .components()
        .chain(options.checkpoint_directory.components())
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(ImportError::InvalidRequest(
            "destination and checkpoint paths must not contain parent traversal",
        ));
    }
    if options.working_memory_bytes == 0 {
        return Err(ImportError::WorkingMemoryExceeded {
            required_bytes: 1,
            budget_bytes: 0,
        });
    }
    Ok(())
}

fn selected_shapes(base: Shape4D, profile: ProfileKind) -> Result<Vec<Shape4D>, ImportError> {
    let natural = pyramid_shapes(base)?;
    match profile_limits(profile).scales {
        ScaleCountRule::Maximum(maximum) => {
            let actual = u64::try_from(natural.len()).map_err(|_| ImportError::Overflow)?;
            if actual > maximum {
                return Err(StorageProfileError::CeilingExceeded {
                    profile: profile.name(),
                    metric: "scales per image",
                    actual,
                    maximum,
                }
                .into());
            }
            Ok(natural)
        }
        ScaleCountRule::Exact(expected) => {
            let count = usize::try_from(expected).map_err(|_| ImportError::Overflow)?;
            let mut shapes = Vec::with_capacity(count);
            shapes.push(base);
            while shapes.len() < count {
                let prior = *shapes.last().expect("the base shape was inserted");
                shapes.push(
                    Shape4D::new(
                        prior.t(),
                        prior.z().div_ceil(2),
                        prior.y().div_ceil(2),
                        prior.x().div_ceil(2),
                    )
                    .map_err(|_| ImportError::Overflow)?,
                );
            }
            Ok(shapes)
        }
    }
}

fn validate_profile_counts(
    options: &ImportOptions,
    scales: usize,
    logical_bricks: u64,
    pixel_shards: u64,
    explicit_validity: bool,
    packed_shards: u64,
) -> Result<(), ImportError> {
    let limits = profile_limits(options.profile);
    let scales = u64::try_from(scales).map_err(|_| ImportError::Overflow)?;
    match limits.scales {
        ScaleCountRule::Maximum(maximum) if scales > maximum => {
            return Err(StorageProfileError::CeilingExceeded {
                profile: options.profile.name(),
                metric: "scales per image",
                actual: scales,
                maximum,
            }
            .into());
        }
        ScaleCountRule::Exact(expected) if scales != expected => {
            return Err(StorageProfileError::ExactCountMismatch {
                profile: options.profile.name(),
                metric: "scales per image",
                actual: scales,
                expected,
            }
            .into());
        }
        _ => {}
    }

    let logical_s0_bytes = options
        .inspection
        .shape
        .element_count()
        .map_err(|_| ImportError::Overflow)?
        .checked_mul(u64::from(options.inspection.channels))
        .and_then(|value| value.checked_mul(u64::from(options.inspection.dtype.bytes_per_sample())))
        .ok_or(ImportError::Overflow)?;
    if let Some(maximum) = limits.logical_s0_bytes_max
        && logical_s0_bytes > maximum
    {
        return Err(StorageProfileError::CeilingExceeded {
            profile: options.profile.name(),
            metric: "logical S0 bytes",
            actual: logical_s0_bytes,
            maximum,
        }
        .into());
    }
    for (metric, actual, maximum) in [
        ("logical bricks", logical_bricks, limits.logical_bricks),
        ("pixel shards", pixel_shards, limits.pixel_shards),
        (
            "validity shards",
            if explicit_validity { pixel_shards } else { 0 },
            limits.validity_shards,
        ),
        (
            "packed-index shards",
            packed_shards,
            limits.packed_index_shards,
        ),
    ] {
        if actual > maximum {
            return Err(StorageProfileError::CeilingExceeded {
                profile: options.profile.name(),
                metric,
                actual,
                maximum,
            }
            .into());
        }
    }
    Ok(())
}

fn working_memory_required(
    options: &ImportOptions,
    pixel: ShardProfileKind,
    validity: ShardProfileKind,
    is_2d: bool,
    spool_record_bytes: u64,
) -> Result<u64, ImportError> {
    let pixel_inner =
        u64::try_from(pixel.decoded_inner_bytes()).map_err(|_| ImportError::Overflow)?;
    let validity_inner =
        u64::try_from(validity.decoded_inner_bytes()).map_err(|_| ImportError::Overflow)?;
    let pixel_encoded =
        u64::try_from(pixel.encoded_inner_bytes_max()).map_err(|_| ImportError::Overflow)?;
    let validity_encoded =
        u64::try_from(validity.encoded_inner_bytes_max()).map_err(|_| ImportError::Overflow)?;
    let source_multiplier = if is_2d { 4 } else { 8 };
    let pyramid_phase = pixel_inner
        .checked_mul(source_multiplier)
        .and_then(|value| value.checked_add(pixel_inner))
        .and_then(|value| value.checked_add(pixel_encoded))
        .and_then(|value| {
            if options.no_data.is_some() {
                value
                    .checked_add(pixel_inner.checked_mul(source_multiplier)?)
                    .and_then(|value| value.checked_add(validity_inner))
                    .and_then(|value| value.checked_add(validity_encoded))
            } else {
                Some(value)
            }
        })
        .ok_or(ImportError::Overflow)?;
    let source_phase = options
        .inspection
        .maximum_decoded_chunk_bytes
        .checked_add(crate::source::SOURCE_DECODE_OVERHEAD_BYTES_MAX)
        .and_then(|value| value.checked_add(pixel_inner.checked_mul(2)?))
        .and_then(|value| value.checked_add(pixel_encoded))
        .and_then(|value| {
            if options.no_data.is_some() {
                value
                    .checked_add(pixel_inner)?
                    .checked_add(validity_inner)?
                    .checked_add(validity_encoded)
            } else {
                Some(value)
            }
        })
        .ok_or(ImportError::Overflow)?;
    let identity_phase = options
        .inspection
        .maximum_decoded_chunk_bytes
        .checked_add(crate::source::SOURCE_DECODE_OVERHEAD_BYTES_MAX)
        .and_then(|value| {
            value.checked_add(
                16_u64
                    .checked_mul(256)
                    .and_then(|value| value.checked_mul(256))
                    .and_then(|value| {
                        value.checked_mul(u64::from(options.inspection.dtype.bytes_per_sample()))
                    })?,
            )
        })
        .and_then(|value| value.checked_add(16 * 256 * 256))
        .and_then(|value| value.checked_add((16 * 256 * 256) / 8))
        .ok_or(ImportError::Overflow)?;
    let publication_phase = publication_shard_bytes(pixel)?
        .max(if options.no_data.is_some() {
            publication_shard_bytes(validity)?
        } else {
            0
        })
        .max(publication_shard_bytes(ShardProfileKind::PackedIndex)?);
    let transient = source_phase
        .max(pyramid_phase)
        .max(identity_phase)
        .max(publication_phase)
        .max(PUBLICATION_VALIDATION_BYTES_MAX);
    spool_record_bytes
        .checked_add(transient)
        .ok_or(ImportError::Overflow)
}

fn free_space_required(
    work_units: u64,
    logical_pixel_bytes: u64,
    logical_validity_bytes: u64,
    pixel_shards: u64,
    packed_shards: u64,
) -> Result<u64, ImportError> {
    // The checkpoint and final package each carry one encoded copy. Use the
    // frozen codec's 5/4 ceiling for both logical payload copies. Fixed-size
    // zero padding compresses to small frames and is covered below per unit.
    let logical = logical_pixel_bytes
        .checked_add(logical_validity_bytes)
        .ok_or(ImportError::Overflow)?;
    let two_encoded_copies = logical
        .checked_mul(5)
        .and_then(|value| value.checked_div(2))
        .ok_or(ImportError::Overflow)?;
    let work_overhead = work_units
        .checked_mul(PACKED_INDEX_RECORD_BYTES + 512)
        .ok_or(ImportError::Overflow)?;
    let tails = pixel_shards
        .checked_add(if logical_validity_bytes == 0 {
            0
        } else {
            pixel_shards
        })
        .and_then(|value| value.checked_add(packed_shards))
        .and_then(|value| value.checked_mul(1_028))
        .ok_or(ImportError::Overflow)?;
    two_encoded_copies
        .checked_add(work_overhead)
        .and_then(|value| value.checked_add(tails))
        .and_then(|value| value.checked_add(SPACE_OVERHEAD_BYTES))
        .ok_or(ImportError::Overflow)
}

fn plan_digest(options: &ImportOptions, shapes: &[Shape4D]) -> Sha256Digest {
    let mut hasher = Sha256Hasher::new();
    hasher.update(b"MIRANTE4D-WP11-IMPORT-PLAN-V1\0");
    hasher.update(options.inspection.source_fingerprint.as_bytes());
    hasher.update(options.profile.name().as_bytes());
    hasher.update([match options.inspection.dtype {
        mirante4d_domain::IntensityDType::Uint8 => 1,
        mirante4d_domain::IntensityDType::Uint16 => 2,
        mirante4d_domain::IntensityDType::Float32 => 3,
    }]);
    hasher.update(options.inspection.channels.to_le_bytes());
    for shape in shapes {
        for dimension in shape.dimensions() {
            hasher.update(dimension.to_le_bytes());
        }
    }
    for spacing in options.calibration.spacing_zyx_um {
        hasher.update(normalized_f64_bits(spacing).to_le_bytes());
    }
    match options.time_step_seconds {
        Some(value) => {
            hasher.update([1]);
            hasher.update(normalized_f64_bits(value).to_le_bytes());
        }
        None => hasher.update([0]),
    }
    match options.no_data {
        Some(NoDataPolicy::U8Sentinel(value)) => hasher.update([1, value]),
        None => hasher.update([0, 0]),
    }
    hasher.finalize()
}

fn normalized_f64_bits(value: f64) -> u64 {
    if value == 0.0 { 0 } else { value.to_bits() }
}

fn checked_product<const N: usize>(values: [u64; N]) -> Result<u64, ImportError> {
    values
        .into_iter()
        .try_fold(1_u64, |product, value| product.checked_mul(value))
        .ok_or(ImportError::Overflow)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mirante4d_domain::{IntensityDType, Shape4D};
    use mirante4d_identity::Sha256Digest;

    use super::*;
    use crate::{SourceLayout, SpatialCalibration, TiffInspection, TiffSource};

    fn options(shape: Shape4D) -> ImportOptions {
        ImportOptions {
            inspection: TiffInspection {
                source: TiffSource::auto("source.tif"),
                files: Vec::new(),
                layout: SourceLayout::MultipageStacks,
                shape,
                channels: 1,
                dtype: IntensityDType::Uint16,
                ome_spacing_zyx_um: None,
                source_bytes: 1,
                source_fingerprint: Sha256Digest::parse(&"1".repeat(64)).unwrap(),
                maximum_decoded_chunk_bytes: 65_536,
            },
            destination: PathBuf::from("output.m4d"),
            checkpoint_directory: PathBuf::from("checkpoint"),
            profile: ProfileKind::Ds0,
            calibration: SpatialCalibration::new([1.0; 3]),
            time_step_seconds: None,
            no_data: None,
            working_memory_bytes: 192 * 1024 * 1024,
        }
    }

    #[test]
    fn plan_is_deterministic_and_bounded() {
        let options = options(Shape4D::new(1, 8, 300, 300).unwrap());
        let first = ImportPlan::new(&options).unwrap();
        let second = ImportPlan::new(&options).unwrap();
        assert_eq!(first.plan_digest, second.plan_digest);
        assert_eq!(first.shapes.len(), 2);
        assert!(first.work_units > 0);
    }

    #[test]
    fn exact_scale_profiles_choose_their_declared_count() {
        let mut options = options(Shape4D::new(1, 2, 2, 2).unwrap());
        options.profile = ProfileKind::Ds4;
        let plan = ImportPlan::new(&options).unwrap();
        assert_eq!(plan.shapes.len(), 4);
    }

    #[test]
    fn sentinel_is_restricted_to_uint8() {
        let mut options = options(Shape4D::new(1, 1, 2, 2).unwrap());
        options.no_data = Some(NoDataPolicy::U8Sentinel(255));
        assert!(matches!(
            ImportPlan::new(&options),
            Err(ImportError::InvalidRequest(_))
        ));
    }

    #[test]
    fn publication_control_and_validation_memory_are_admitted_up_front() {
        let mut options = options(Shape4D::new(1, 1, 2, 2).unwrap());
        let baseline = ImportPlan::new(&options).unwrap();
        let required = baseline
            .spool_record_bytes
            .checked_add(PUBLICATION_VALIDATION_BYTES_MAX)
            .unwrap();

        options.working_memory_bytes = required - 1;
        assert!(matches!(
            ImportPlan::new(&options),
            Err(ImportError::WorkingMemoryExceeded {
                required_bytes,
                budget_bytes
            }) if required_bytes == required && budget_bytes == required - 1
        ));

        options.working_memory_bytes = required;
        ImportPlan::new(&options).unwrap();
    }
}

//! Checked deterministic import planning.

use mirante4d_domain::Shape4D;
use mirante4d_identity::{
    SCIENTIFIC_TILE_SHAPE_TZYX, ScientificLayerHasher, Sha256Digest, Sha256Hasher,
};
use mirante4d_storage::{
    FIXED_CONTROL_OBJECTS, GLOBAL_UNCOMPRESSED_OUTER_SHARD_BYTES_MAX,
    INNER_CODEC_WORKING_BYTES_MAX, MANIFEST_DESCRIPTORS_MAX,
    MANIFEST_DESCRIPTORS_PER_PAGE_GUARANTEED, MAX_PORTABLE_CONTROL_OBJECT_BYTES,
    MAX_PROFILE_HEADER_BYTES, MAX_ZARR_METADATA_BYTES, PACKAGE_VALIDATION_WORKING_BYTES,
    PACKED_INDEX_RECORD_BYTES, PACKED_INDEX_RECORDS_PER_INNER_CHUNK,
    PACKED_INDEX_RECORDS_PER_OUTER_SHARD, ProfileKind, ScaleCountRule, ShardProfileKind,
    StorageProfileError, encoded_inner_payload_limit, encoded_outer_shard_limit, profile_limits,
    profile_pyramid_shapes,
};

use crate::{
    ImportCapacityPlan, ImportError, ImportOptions, NoDataValueRule,
    chunk::{chunk_grid, pixel_kind, validity_kind},
    cpu_chunk::{base_task_charge_bytes, pyramid_task_charge_bytes, scientific_task_charge_bytes},
    model::{ResolvedNoDataPolicy, ResolvedNoDataValue},
};

const PUBLICATION_CONTROL_BYTES_MAX: u64 = 64 * 1024 * 1024;
const PUBLICATION_SHARD_METADATA_BYTES_MAX: u64 = 64 * 1024;
const PUBLICATION_VALIDATION_BYTES_MAX: u64 =
    PACKAGE_VALIDATION_WORKING_BYTES + GLOBAL_UNCOMPRESSED_OUTER_SHARD_BYTES_MAX;

fn publication_shard_bytes(kind: ShardProfileKind) -> Result<u64, ImportError> {
    let retained = kind
        .decoded_outer_bytes()
        .max(kind.encoded_shard_bytes_max());
    u64::try_from(retained)
        .ok()
        .and_then(|value| value.checked_add(u64::try_from(kind.encoded_inner_bytes_max()).ok()?))
        .and_then(|value| value.checked_add(INNER_CODEC_WORKING_BYTES_MAX))
        .and_then(|value| value.checked_add(PUBLICATION_SHARD_METADATA_BYTES_MAX))
        .and_then(|value| value.checked_add(PUBLICATION_CONTROL_BYTES_MAX))
        .ok_or(ImportError::Overflow)
}

const SPACE_OVERHEAD_BYTES: u64 = 4 * 1024 * 1024;
const FILESYSTEM_ALLOCATION_GRANULARITY_MAX: u64 = 4_096;
const SPOOL_JOURNAL_RECORD_BYTES: u64 = 160;
const SPOOL_WATERMARK_RECORD_BYTES: u64 = 96;
const SPOOL_FIXED_CONTROL_BYTES: u64 = 4 * 4_096;
const CANONICAL_CACHE_STATE_BYTES_PER_PLANE_MAX: u64 = 64;
const UNIT_JOURNAL_FIXED_RECORD_BYTES: u64 = 8 + 8 + 4 + 32 + 4 + 32;
const UNIT_JOURNAL_FIXED_HEADER_BYTES: u64 = 4_096;
const NO_DATA_CONTROL_FIXED_BYTES: u64 = 4_096;
const STAGE_JOURNAL_RECORD_BYTES_MAX: u64 = 8 + 1 + 4 + 512 + 32;
const NO_DATA_ALGORITHM_ID: &[u8] =
    b"mirante4d-typed-no-data-first-volume-spatial-reconstruction-v2";
#[derive(Clone, Debug)]
pub(crate) struct ImportPlan {
    pub shapes: Vec<Shape4D>,
    pub is_2d: bool,
    pub explicit_validity: bool,
    pub pixel_kind: ShardProfileKind,
    pub validity_kind: ShardProfileKind,
    pub work_units: u64,
    pub unit_work_units: u64,
    pub logical_output_bytes: u64,
    pub spool_record_bytes: u64,
    pub source_index_working_bytes: u64,
    pub resident_working_bytes: u64,
    pub minimum_execution_bytes: u64,
    pub logical_bricks_by_scale: Vec<u64>,
    pub plan_digest: Sha256Digest,
    pub decoded_base_bytes: u64,
    pub final_package_upper_bound: u64,
    pub bounded_unit_scratch_bytes: u64,
    /// Complete decoded payload and fixed control/allocation ceiling for one
    /// ordinal-bound canonical cache. This is optional decode-ahead
    /// headroom, not part of hard Start admission.
    pub canonical_unit_cache_bytes: u64,
    pub growing_control_bytes: u64,
    pub maximum_unit_output_upper_bound: u64,
    pub unit_control_increment_upper_bound: u64,
    pub finalization_headroom_bytes: u64,
    pub finalization_commit_upper_bound: u64,
    pub start_required_headroom_bytes: u64,
}

impl ImportPlan {
    /// Conservative request-level plan used for profile selection and the
    /// pre-detection resource preflight.
    pub fn new(options: &ImportOptions) -> Result<Self, ImportError> {
        let explicit_validity = options
            .no_data
            .is_some_and(|policy| policy.may_produce_validity());
        Self::build(options, explicit_validity, None)
    }

    /// Exact checkpoint/package plan after first-volume policy resolution.
    pub(crate) fn new_resolved(
        options: &ImportOptions,
        policy: &ResolvedNoDataPolicy,
    ) -> Result<Self, ImportError> {
        if policy.request() != options.no_data
            || policy.base_depth() != options.inspection.shape.z()
            || policy
                .value()
                .is_some_and(|value| value.dtype() != options.inspection.dtype)
            || policy.automatic_mask().is_some_and(|mask| {
                mask.shape_zyx()
                    != [
                        options.inspection.shape.z(),
                        options.inspection.shape.y(),
                        options.inspection.shape.x(),
                    ]
            })
        {
            return Err(ImportError::InvalidRequest(
                "resolved no-data policy does not bind the reviewed request and source",
            ));
        }
        Self::build(options, policy.explicit_validity(), Some(policy))
    }

    fn build(
        options: &ImportOptions,
        explicit_validity: bool,
        resolved: Option<&ResolvedNoDataPolicy>,
    ) -> Result<Self, ImportError> {
        validate_request(options)?;
        let shapes = selected_shapes(options.inspection.shape, options.profile)?;
        let is_2d = options.inspection.shape.z() == 1;
        let pixel_kind = pixel_kind(options.inspection.dtype, is_2d);
        let validity_kind = validity_kind(is_2d);
        let canonical_base_bytes = checked_product([
            options.inspection.shape.t(),
            u64::from(options.inspection.channels),
            options.inspection.shape.z(),
            options.inspection.shape.y(),
            options.inspection.shape.x(),
            u64::from(options.inspection.dtype.bytes_per_sample()),
        ])?;

        let mut logical_bricks_by_scale = Vec::with_capacity(shapes.len());
        let mut work_units = 0_u64;
        let mut unit_work_units = 0_u64;
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
            let unit_bricks = checked_product([grid[0], grid[1], grid[2]])?;
            unit_work_units = unit_work_units
                .checked_add(unit_bricks)
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

        let spool_record_bytes = crate::spool::record_memory_bytes(unit_work_units)?;
        let source_index_working_bytes = options.inspection.source_index_working_bytes;
        let automatic_mask_bytes = match resolved {
            Some(policy) => policy.automatic_mask_resident_bytes(),
            None if options
                .no_data
                .is_some_and(|policy| policy.value_rule() == Some(NoDataValueRule::Automatic)) =>
            {
                crate::source::automatic_mask_packed_bytes([
                    options.inspection.shape.z(),
                    options.inspection.shape.y(),
                    options.inspection.shape.x(),
                ])
                .ok_or(ImportError::Overflow)?
            }
            None => 0,
        };
        let resident_working_bytes = spool_record_bytes
            .checked_add(source_index_working_bytes)
            .and_then(|value| value.checked_add(automatic_mask_bytes))
            .ok_or(ImportError::Overflow)?;
        let minimum_execution_bytes = minimum_execution_required(
            options,
            pixel_kind,
            validity_kind,
            is_2d,
            resident_working_bytes,
            explicit_validity,
        )?;

        let space = storage_placement_plan(StoragePlacementInput {
            work_units,
            canonical_base_bytes,
            unit_work_units,
            pixel_kind,
            validity_kind,
            explicit_validity,
            pixel_shards: addressed_pixel_shards,
            packed_shards: addressed_packed_shards,
            timepoints: options.inspection.shape.t(),
            channels: options.inspection.channels,
            spatial_shape_zyx: [
                options.inspection.shape.z(),
                options.inspection.shape.y(),
                options.inspection.shape.x(),
            ],
            scale_count: u64::try_from(shapes.len()).map_err(|_| ImportError::Overflow)?,
            profile: options.profile,
            unit_object_bounds: unit_object_bounds(
                &shapes,
                is_2d,
                pixel_kind,
                validity_kind,
                explicit_validity,
            )?,
            packed_output_upper_bound: packed_output_upper_bound(&logical_bricks_by_scale)?,
            automatic_reconstruction_spool_bytes: if options
                .no_data
                .is_some_and(|policy| policy.value_rule() == Some(NoDataValueRule::Automatic))
            {
                crate::source::automatic_reconstruction_spool_bytes([
                    options.inspection.shape.z(),
                    options.inspection.shape.y(),
                    options.inspection.shape.x(),
                ])
                .ok_or(ImportError::Overflow)?
            } else {
                0
            },
            automatic_mask_bytes,
        })?;
        let plan_digest = plan_digest(options, &shapes, resolved);
        let logical_output_bytes = logical_pixel_bytes
            .checked_add(logical_validity_bytes)
            .ok_or(ImportError::Overflow)?;

        Ok(Self {
            shapes,
            is_2d,
            explicit_validity,
            pixel_kind,
            validity_kind,
            work_units,
            unit_work_units,
            logical_output_bytes,
            spool_record_bytes,
            source_index_working_bytes,
            resident_working_bytes,
            minimum_execution_bytes,
            logical_bricks_by_scale,
            plan_digest,
            decoded_base_bytes: canonical_base_bytes,
            final_package_upper_bound: space.final_package,
            bounded_unit_scratch_bytes: space.bounded_unit_scratch,
            canonical_unit_cache_bytes: space.canonical_unit_cache,
            growing_control_bytes: space.growing_control,
            maximum_unit_output_upper_bound: space.maximum_unit_output,
            unit_control_increment_upper_bound: space.unit_control_increment,
            finalization_headroom_bytes: space.finalization_headroom,
            finalization_commit_upper_bound: space.finalization_commit,
            start_required_headroom_bytes: space.start_required_headroom,
        })
    }
}

/// Exact maximum of every phase's minimum complete execution path. The
/// process broker protects this capacity before an import is allowed to
/// transition from reviewed setup into running state.
pub fn minimum_import_progress_bytes(options: &ImportOptions) -> Result<u64, ImportError> {
    Ok(ImportPlan::new(options)?.minimum_execution_bytes)
}

pub fn import_capacity_plan(options: &ImportOptions) -> Result<ImportCapacityPlan, ImportError> {
    let plan = ImportPlan::new(options)?;
    Ok(ImportCapacityPlan {
        compressed_source_bytes: options.inspection.source_bytes,
        decoded_base_bytes: plan.decoded_base_bytes,
        logical_output_bytes: plan.logical_output_bytes,
        final_package_upper_bound: plan.final_package_upper_bound,
        bounded_unit_scratch_bytes: plan.bounded_unit_scratch_bytes,
        growing_control_bytes: plan.growing_control_bytes,
        maximum_unit_output_upper_bound: plan.maximum_unit_output_upper_bound,
        finalization_headroom_bytes: plan.finalization_headroom_bytes,
        start_required_headroom_bytes: plan.start_required_headroom_bytes,
    })
}

pub(crate) fn select_supported_profile(
    options: &ImportOptions,
) -> Result<ProfileKind, ImportError> {
    let mut candidate = options.clone();
    candidate.profile = ProfileKind::Current;
    ImportPlan::new(&candidate)?;
    Ok(ProfileKind::Current)
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
    if options.inspection.maximum_encoded_chunk_bytes == 0 {
        return Err(ImportError::InvalidRequest(
            "inspection must declare a positive encoded TIFF chunk bound",
        ));
    }
    let canonical_plane_bytes = options
        .inspection
        .shape
        .y()
        .checked_mul(options.inspection.shape.x())
        .and_then(|value| value.checked_mul(u64::from(options.inspection.dtype.bytes_per_sample())))
        .ok_or(ImportError::Overflow)?;
    if canonical_plane_bytes > crate::canonical_cache::CANONICAL_PLANE_BYTES_MAX {
        return Err(ImportError::UnsupportedSource(format!(
            "one canonical TIFF plane requires {canonical_plane_bytes} bytes, exceeding the fixed {}-byte checkpoint work-unit bound",
            crate::canonical_cache::CANONICAL_PLANE_BYTES_MAX
        )));
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
    if let Some(policy) = options.no_data {
        if !policy.may_produce_validity() {
            return Err(ImportError::InvalidRequest(
                "an enabled no-data policy must request at least one rule",
            ));
        }
        if matches!(policy.value_rule(), Some(NoDataValueRule::ManualUint8(_)))
            && options.inspection.dtype != mirante4d_domain::IntensityDType::Uint8
        {
            return Err(ImportError::InvalidRequest(
                "manual no-data values are supported only for uint8 TIFF input",
            ));
        }
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
    Ok(())
}

fn selected_shapes(base: Shape4D, profile: ProfileKind) -> Result<Vec<Shape4D>, ImportError> {
    let natural = profile_pyramid_shapes(base)?;
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
    let arrays_per_scale = if explicit_validity { 3_u64 } else { 2 };
    let zarr_metadata = 5_u64
        .checked_add(
            scales
                .checked_mul(arrays_per_scale)
                .ok_or(ImportError::Overflow)?,
        )
        .ok_or(ImportError::Overflow)?;
    let manifest_descriptors = pixel_shards
        .checked_mul(if explicit_validity { 2 } else { 1 })
        .and_then(|value| value.checked_add(packed_shards))
        .and_then(|value| value.checked_add(zarr_metadata))
        .and_then(|value| value.checked_add(3))
        .ok_or(ImportError::Overflow)?;
    if manifest_descriptors > MANIFEST_DESCRIPTORS_MAX {
        return Err(StorageProfileError::CeilingExceeded {
            profile: options.profile.name(),
            metric: "manifest descriptors",
            actual: manifest_descriptors,
            maximum: MANIFEST_DESCRIPTORS_MAX,
        }
        .into());
    }
    let manifest_pages = manifest_descriptors.div_ceil(MANIFEST_DESCRIPTORS_PER_PAGE_GUARANTEED);
    let physical_objects = manifest_descriptors
        .checked_add(manifest_pages)
        .and_then(|value| value.checked_add(FIXED_CONTROL_OBJECTS))
        .ok_or(ImportError::Overflow)?;
    if physical_objects > limits.total_physical_objects {
        return Err(StorageProfileError::CeilingExceeded {
            profile: options.profile.name(),
            metric: "physical objects",
            actual: physical_objects,
            maximum: limits.total_physical_objects,
        }
        .into());
    }
    Ok(())
}

fn minimum_execution_required(
    options: &ImportOptions,
    pixel: ShardProfileKind,
    validity: ShardProfileKind,
    is_2d: bool,
    resident_working_bytes: u64,
    explicit_validity: bool,
) -> Result<u64, ImportError> {
    let pyramid_phase = pyramid_task_charge_bytes(
        options.inspection.dtype,
        pixel,
        validity,
        is_2d,
        explicit_validity,
    )?;
    let row_bytes = options
        .inspection
        .shape
        .x()
        .checked_mul(u64::from(options.inspection.dtype.bytes_per_sample()))
        .ok_or(ImportError::Overflow)?;
    let maximum_file_pages = options
        .inspection
        .files
        .iter()
        .map(|file| file.planes)
        .max()
        .unwrap_or(1);
    let common_source_bytes = options
        .inspection
        .maximum_decoded_chunk_bytes
        .checked_add(options.inspection.maximum_encoded_chunk_bytes)
        .and_then(|value| value.checked_add(crate::source::SOURCE_DECODE_FIXED_OVERHEAD_BYTES_MAX))
        .ok_or(ImportError::Overflow)?;
    let serial_transition_bytes = if maximum_file_pages > 1 {
        crate::source::SOURCE_SERIAL_TRANSITION_BYTES_MAX
    } else {
        0
    };
    let serial_source_phase = common_source_bytes
        .checked_add(
            crate::source::retained_decoder_bytes(maximum_file_pages)
                .ok_or(ImportError::Overflow)?,
        )
        .and_then(|value| value.checked_add(serial_transition_bytes))
        .and_then(|value| value.checked_add(row_bytes))
        .and_then(|value| value.checked_add(crate::source::SOURCE_TASK_METADATA_BYTES_MAX))
        .ok_or(ImportError::Overflow)?;
    // Full-plane materialization is an optional acceleration. Runtime falls
    // back to this mandatory row-streaming phase when the parallel task does
    // not fit, so admission must not make the optimization compulsory.
    let source_phase = serial_source_phase;
    let detection_phase = if options.no_data.is_some_and(|policy| {
        policy.value_rule() == Some(NoDataValueRule::Automatic) || policy.hides_constant_z_planes()
    }) {
        let plane_samples = options
            .inspection
            .shape
            .y()
            .checked_mul(options.inspection.shape.x())
            .ok_or(ImportError::Overflow)?;
        let plane_bytes = plane_samples
            .checked_mul(u64::from(options.inspection.dtype.bytes_per_sample()))
            .ok_or(ImportError::Overflow)?;
        let automatic = options
            .no_data
            .is_some_and(|policy| policy.value_rule() == Some(NoDataValueRule::Automatic));
        let detector_bytes = if automatic {
            crate::source::automatic_discovery_detector_bytes(
                options.inspection.shape.x(),
                options.inspection.shape.y(),
            )
            .ok_or(ImportError::Overflow)?
        } else {
            0
        };
        let reconstruction_bytes = if automatic {
            crate::source::automatic_reconstruction_transient_bytes([
                options.inspection.shape.z(),
                options.inspection.shape.y(),
                options.inspection.shape.x(),
            ])
            .ok_or(ImportError::Overflow)?
        } else {
            0
        };
        common_source_bytes
            .checked_add(
                crate::source::retained_decoder_bytes(maximum_file_pages)
                    .ok_or(ImportError::Overflow)?,
            )
            .and_then(|value| value.checked_add(serial_transition_bytes))
            .and_then(|value| value.checked_add(plane_bytes))
            .and_then(|value| value.checked_add(detector_bytes.max(reconstruction_bytes)))
            .and_then(|value| value.checked_add(crate::source::SOURCE_TASK_METADATA_BYTES_MAX))
            .ok_or(ImportError::Overflow)?
    } else {
        0
    };
    let identity_phase = scientific_task_charge_bytes(options.inspection.dtype, explicit_validity)?;
    let publication_phase = publication_shard_bytes(pixel)?
        .max(if explicit_validity {
            publication_shard_bytes(validity)?
        } else {
            0
        })
        .max(publication_shard_bytes(ShardProfileKind::PackedIndex)?);
    let worker_phase =
        base_task_charge_bytes(options.inspection.dtype, pixel, validity, explicit_validity)?.max(
            pyramid_task_charge_bytes(
                options.inspection.dtype,
                pixel,
                validity,
                is_2d,
                explicit_validity,
            )?,
        );
    let transient = source_phase
        .max(pyramid_phase)
        .max(detection_phase)
        .max(identity_phase)
        .max(publication_phase)
        .max(worker_phase)
        .max(PUBLICATION_VALIDATION_BYTES_MAX);
    resident_working_bytes
        .checked_add(transient)
        .ok_or(ImportError::Overflow)
}

struct StoragePlacementPlan {
    final_package: u64,
    bounded_unit_scratch: u64,
    canonical_unit_cache: u64,
    growing_control: u64,
    maximum_unit_output: u64,
    unit_control_increment: u64,
    finalization_headroom: u64,
    finalization_commit: u64,
    start_required_headroom: u64,
}

#[derive(Clone, Copy, Debug)]
struct UnitObjectBounds {
    pixel_bytes: u64,
    validity_bytes: u64,
    shard_objects: u64,
}

#[derive(Clone, Copy, Debug)]
struct PackedObjectBounds {
    bytes: u64,
    shard_objects: u64,
}

struct StoragePlacementInput {
    work_units: u64,
    canonical_base_bytes: u64,
    unit_work_units: u64,
    pixel_kind: ShardProfileKind,
    validity_kind: ShardProfileKind,
    explicit_validity: bool,
    pixel_shards: u64,
    packed_shards: u64,
    automatic_reconstruction_spool_bytes: u64,
    automatic_mask_bytes: u64,
    timepoints: u64,
    channels: u32,
    spatial_shape_zyx: [u64; 3],
    scale_count: u64,
    profile: ProfileKind,
    unit_object_bounds: UnitObjectBounds,
    packed_output_upper_bound: PackedObjectBounds,
}

fn unit_object_bounds(
    shapes: &[Shape4D],
    is_2d: bool,
    pixel_kind: ShardProfileKind,
    validity_kind: ShardProfileKind,
    explicit_validity: bool,
) -> Result<UnitObjectBounds, ImportError> {
    let (pixel_bytes, pixel_objects) = occupied_unit_component_bound(shapes, is_2d, pixel_kind)?;
    let validity_bytes = if explicit_validity {
        let (bytes, objects) = occupied_unit_component_bound(shapes, is_2d, validity_kind)?;
        if objects != pixel_objects {
            return Err(ImportError::InvalidRequest(
                "pixel and validity unit shard traversals disagree",
            ));
        }
        bytes
    } else {
        0
    };
    Ok(UnitObjectBounds {
        pixel_bytes,
        validity_bytes,
        shard_objects: pixel_objects
            .checked_mul(if explicit_validity { 2 } else { 1 })
            .ok_or(ImportError::Overflow)?,
    })
}

/// Sums the codec bound for each actually addressed outer object. A boundary
/// object pays only for its occupied inner slots; it is never promoted to a
/// completely occupied 4x4x4 (or 1x4x4) shard for capacity guidance.
fn occupied_unit_component_bound(
    shapes: &[Shape4D],
    is_2d: bool,
    kind: ShardProfileKind,
) -> Result<(u64, u64), ImportError> {
    let ratios = if is_2d { [1_u64, 4, 4] } else { [4_u64; 3] };
    let slots = checked_product(ratios)?;
    if slots != u64::try_from(kind.chunks_per_shard()).map_err(|_| ImportError::Overflow)? {
        return Err(ImportError::InvalidRequest(
            "storage shard slots disagree with import traversal",
        ));
    }
    let decoded_inner =
        u64::try_from(kind.decoded_inner_bytes()).map_err(|_| ImportError::Overflow)?;
    let mut bytes = 0_u64;
    let mut objects = 0_u64;
    for shape in shapes {
        let grid = chunk_grid([shape.z(), shape.y(), shape.x()], is_2d);
        let outer = [
            grid[0].div_ceil(ratios[0]),
            grid[1].div_ceil(ratios[1]),
            grid[2].div_ceil(ratios[2]),
        ];
        for oz in 0..outer[0] {
            let occupied_z = (grid[0] - oz * ratios[0]).min(ratios[0]);
            for oy in 0..outer[1] {
                let occupied_y = (grid[1] - oy * ratios[1]).min(ratios[1]);
                for ox in 0..outer[2] {
                    let occupied_x = (grid[2] - ox * ratios[2]).min(ratios[2]);
                    let occupied = checked_product([occupied_z, occupied_y, occupied_x])?;
                    let decoded = occupied
                        .checked_mul(decoded_inner)
                        .ok_or(ImportError::Overflow)?;
                    bytes = bytes
                        .checked_add(encoded_outer_shard_limit(decoded)?)
                        .ok_or(ImportError::Overflow)?;
                    objects = objects.checked_add(1).ok_or(ImportError::Overflow)?;
                }
            }
        }
    }
    Ok((bytes, objects))
}

fn packed_output_upper_bound(
    logical_bricks_by_scale: &[u64],
) -> Result<PackedObjectBounds, ImportError> {
    let kind = ShardProfileKind::PackedIndex;
    let decoded_inner =
        u64::try_from(kind.decoded_inner_bytes()).map_err(|_| ImportError::Overflow)?;
    let mut bytes = 0_u64;
    let mut shard_objects = 0_u64;
    for &records in logical_bricks_by_scale {
        let mut remaining = records;
        while remaining > 0 {
            let outer_records = remaining.min(PACKED_INDEX_RECORDS_PER_OUTER_SHARD);
            let occupied_inner = outer_records.div_ceil(PACKED_INDEX_RECORDS_PER_INNER_CHUNK);
            bytes = bytes
                .checked_add(encoded_outer_shard_limit(
                    occupied_inner
                        .checked_mul(decoded_inner)
                        .ok_or(ImportError::Overflow)?,
                )?)
                .ok_or(ImportError::Overflow)?;
            shard_objects = shard_objects.checked_add(1).ok_or(ImportError::Overflow)?;
            remaining -= outer_records;
        }
    }
    Ok(PackedObjectBounds {
        bytes,
        shard_objects,
    })
}

/// Fixed profile-derived reserve for the payload/index/metadata work that can
/// remain after the last temporal unit. It intentionally uses format maxima
/// rather than the request's T/C extent, keeping hard unit admission stable as
/// temporal length grows.
fn finalization_headroom(
    profile: ProfileKind,
    scale_count: u64,
    explicit_validity: bool,
) -> Result<u64, ImportError> {
    let limits = profile_limits(profile);
    let scales = limits.scales.maximum();
    let scale_rounding = scales.saturating_sub(1);
    let packed_inner_chunks = limits
        .logical_bricks
        .div_ceil(PACKED_INDEX_RECORDS_PER_INNER_CHUNK)
        .checked_add(scale_rounding)
        .ok_or(ImportError::Overflow)?;
    let packed_shards = limits
        .logical_bricks
        .div_ceil(PACKED_INDEX_RECORDS_PER_OUTER_SHARD)
        .checked_add(scale_rounding)
        .ok_or(ImportError::Overflow)?;
    let packed_inner_bound = encoded_inner_payload_limit(
        u64::try_from(ShardProfileKind::PackedIndex.decoded_inner_bytes())
            .map_err(|_| ImportError::Overflow)?,
    )?;
    let packed_bytes = packed_inner_chunks
        .checked_mul(packed_inner_bound)
        .and_then(|value| value.checked_add(packed_shards.checked_mul(4_096)?))
        .and_then(|value| {
            value.checked_add(packed_shards.checked_mul(STAGE_JOURNAL_RECORD_BYTES_MAX)?)
        })
        .ok_or(ImportError::Overflow)?;

    let zarr_object_bytes =
        u64::try_from(MAX_ZARR_METADATA_BYTES).map_err(|_| ImportError::Overflow)?;
    let portable_object_bytes =
        u64::try_from(MAX_PORTABLE_CONTROL_OBJECT_BYTES).map_err(|_| ImportError::Overflow)?;
    let zarr_metadata_objects = 5_u64
        .checked_add(
            scale_count
                .checked_mul(if explicit_validity { 3 } else { 2 })
                .ok_or(ImportError::Overflow)?,
        )
        .ok_or(ImportError::Overflow)?;
    let metadata_bytes = zarr_metadata_objects
        .checked_mul(zarr_object_bytes)
        .and_then(|value| value.checked_add(u64::try_from(MAX_PROFILE_HEADER_BYTES).ok()?))
        .and_then(|value| value.checked_add(portable_object_bytes.checked_mul(2)?))
        .ok_or(ImportError::Overflow)?;
    let manifest_pages =
        MANIFEST_DESCRIPTORS_MAX.div_ceil(MANIFEST_DESCRIPTORS_PER_PAGE_GUARANTEED);
    let manifest_bytes = manifest_pages
        .checked_add(1)
        .and_then(|objects| objects.checked_mul(portable_object_bytes))
        .ok_or(ImportError::Overflow)?;
    let allocated_objects = packed_shards
        .checked_add(zarr_metadata_objects)
        .and_then(|value| value.checked_add(3))
        .and_then(|value| value.checked_add(manifest_pages))
        .and_then(|value| value.checked_add(1))
        .ok_or(ImportError::Overflow)?;
    let allocation_slop = allocated_objects
        .checked_mul(FILESYSTEM_ALLOCATION_GRANULARITY_MAX - 1)
        .ok_or(ImportError::Overflow)?;
    let incomplete_object = encoded_outer_shard_limit(
        u64::try_from(ShardProfileKind::PackedIndex.decoded_outer_bytes())
            .map_err(|_| ImportError::Overflow)?,
    )?;
    packed_bytes
        .checked_add(metadata_bytes)
        .and_then(|value| value.checked_add(manifest_bytes))
        .and_then(|value| value.checked_add(allocation_slop))
        .and_then(|value| value.checked_add(incomplete_object))
        .and_then(|value| value.checked_add(SPACE_OVERHEAD_BYTES))
        .ok_or(ImportError::Overflow)
}

fn storage_placement_plan(
    input: StoragePlacementInput,
) -> Result<StoragePlacementPlan, ImportError> {
    let StoragePlacementInput {
        work_units,
        canonical_base_bytes,
        unit_work_units,
        pixel_kind,
        validity_kind,
        explicit_validity,
        pixel_shards,
        packed_shards,
        automatic_reconstruction_spool_bytes,
        automatic_mask_bytes,
        timepoints,
        channels,
        spatial_shape_zyx,
        scale_count,
        profile,
        unit_object_bounds,
        packed_output_upper_bound,
    } = input;
    let pixel_outer_uncompressed = u64::try_from(pixel_kind.decoded_inner_bytes())
        .map_err(|_| ImportError::Overflow)?
        .checked_mul(
            u64::try_from(pixel_kind.chunks_per_shard()).map_err(|_| ImportError::Overflow)?,
        )
        .ok_or(ImportError::Overflow)?;
    let validity_outer_uncompressed = u64::try_from(validity_kind.decoded_inner_bytes())
        .map_err(|_| ImportError::Overflow)?
        .checked_mul(
            u64::try_from(validity_kind.chunks_per_shard()).map_err(|_| ImportError::Overflow)?,
        )
        .ok_or(ImportError::Overflow)?;
    let packed_outer_uncompressed =
        u64::try_from(ShardProfileKind::PackedIndex.decoded_inner_bytes())
            .map_err(|_| ImportError::Overflow)?
            .checked_mul(
                u64::try_from(ShardProfileKind::PackedIndex.chunks_per_shard())
                    .map_err(|_| ImportError::Overflow)?,
            )
            .ok_or(ImportError::Overflow)?;
    let temporal_units = timepoints
        .checked_mul(u64::from(channels))
        .ok_or(ImportError::Overflow)?;
    let final_pixels = unit_object_bounds
        .pixel_bytes
        .checked_mul(temporal_units)
        .ok_or(ImportError::Overflow)?;
    let final_validity = if explicit_validity {
        unit_object_bounds
            .validity_bytes
            .checked_mul(temporal_units)
            .ok_or(ImportError::Overflow)?
    } else {
        0
    };
    let final_packed = packed_output_upper_bound.bytes;
    let validity_shards = if explicit_validity { pixel_shards } else { 0 };
    let shard_objects = pixel_shards
        .checked_add(validity_shards)
        .and_then(|value| value.checked_add(packed_shards))
        .ok_or(ImportError::Overflow)?;
    if unit_object_bounds
        .shard_objects
        .checked_mul(temporal_units)
        .ok_or(ImportError::Overflow)?
        != pixel_shards
            .checked_add(validity_shards)
            .ok_or(ImportError::Overflow)?
        || packed_output_upper_bound.shard_objects != packed_shards
    {
        return Err(ImportError::InvalidRequest(
            "occupied-object capacity traversal disagrees with package counts",
        ));
    }
    let arrays_per_scale = if explicit_validity { 3 } else { 2 };
    let array_metadata_objects = scale_count
        .checked_mul(arrays_per_scale)
        .ok_or(ImportError::Overflow)?;
    // Four Zarr root/group objects, one image-group object, the per-scale
    // arrays, and profile/science/display control objects are all manifest
    // descriptors. Their individual wire ceilings are storage-owned.
    let zarr_metadata_objects = 5_u64
        .checked_add(array_metadata_objects)
        .ok_or(ImportError::Overflow)?;
    let control_objects = 3_u64;
    let descriptor_count = shard_objects
        .checked_add(zarr_metadata_objects)
        .and_then(|value| value.checked_add(control_objects))
        .ok_or(ImportError::Overflow)?;
    let zarr_object_bytes_max =
        u64::try_from(MAX_ZARR_METADATA_BYTES).map_err(|_| ImportError::Overflow)?;
    let profile_bytes_max =
        u64::try_from(MAX_PROFILE_HEADER_BYTES).map_err(|_| ImportError::Overflow)?;
    let portable_control_bytes_max =
        u64::try_from(MAX_PORTABLE_CONTROL_OBJECT_BYTES).map_err(|_| ImportError::Overflow)?;
    let metadata_bytes = zarr_metadata_objects
        .checked_mul(zarr_object_bytes_max)
        .and_then(|value| value.checked_add(profile_bytes_max))
        .and_then(|value| value.checked_add(portable_control_bytes_max.checked_mul(2)?))
        .ok_or(ImportError::Overflow)?;
    let manifest_pages = descriptor_count.div_ceil(MANIFEST_DESCRIPTORS_PER_PAGE_GUARANTEED);
    let manifest_bytes = manifest_pages
        .checked_add(1)
        .and_then(|objects| objects.checked_mul(portable_control_bytes_max))
        .ok_or(ImportError::Overflow)?;
    let allocated_objects = descriptor_count
        .checked_add(manifest_pages)
        .and_then(|value| value.checked_add(1))
        .ok_or(ImportError::Overflow)?;
    let allocation_slop = allocated_objects
        .checked_mul(FILESYSTEM_ALLOCATION_GRANULARITY_MAX - 1)
        .ok_or(ImportError::Overflow)?;
    let final_package = final_pixels
        .checked_add(final_validity)
        .and_then(|value| value.checked_add(final_packed))
        .and_then(|value| value.checked_add(metadata_bytes))
        .and_then(|value| value.checked_add(manifest_bytes))
        .and_then(|value| value.checked_add(allocation_slop))
        .and_then(|value| value.checked_add(SPACE_OVERHEAD_BYTES))
        .ok_or(ImportError::Overflow)?;
    let finalization_allocated_objects = packed_shards
        .checked_add(zarr_metadata_objects)
        .and_then(|value| value.checked_add(control_objects))
        .and_then(|value| value.checked_add(manifest_pages))
        .and_then(|value| value.checked_add(1))
        .ok_or(ImportError::Overflow)?;
    let finalization_allocation_slop = finalization_allocated_objects
        .checked_mul(FILESYSTEM_ALLOCATION_GRANULARITY_MAX - 1)
        .ok_or(ImportError::Overflow)?;
    let finalization_commit = final_packed
        .checked_add(
            packed_shards
                .checked_mul(STAGE_JOURNAL_RECORD_BYTES_MAX)
                .ok_or(ImportError::Overflow)?,
        )
        .and_then(|value| value.checked_add(metadata_bytes))
        .and_then(|value| value.checked_add(manifest_bytes))
        .and_then(|value| value.checked_add(finalization_allocation_slop))
        .and_then(|value| {
            value.checked_add(encoded_outer_shard_limit(packed_outer_uncompressed).ok()?)
        })
        .and_then(|value| value.checked_add(SPACE_OVERHEAD_BYTES))
        .ok_or(ImportError::Overflow)?;

    // Only one spatial unit is live outside the final-layout stage. Its
    // decoded cache and encoded-inner spool are bounded independently of T/C.
    if temporal_units == 0
        || unit_work_units == 0
        || unit_work_units
            .checked_mul(temporal_units)
            .ok_or(ImportError::Overflow)?
            != work_units
    {
        return Err(ImportError::InvalidRequest(
            "logical work units are not a whole number of spatial units",
        ));
    }
    let unit_decoded_base = canonical_base_bytes
        .checked_div(temporal_units)
        .ok_or(ImportError::Overflow)?;
    let pixel_inner = encoded_inner_payload_limit(
        u64::try_from(pixel_kind.decoded_inner_bytes()).map_err(|_| ImportError::Overflow)?,
    )?;
    let validity_inner = encoded_inner_payload_limit(
        u64::try_from(validity_kind.decoded_inner_bytes()).map_err(|_| ImportError::Overflow)?,
    )?;
    let unit_encoded = unit_work_units
        .checked_mul(
            pixel_inner
                .checked_add(if explicit_validity { validity_inner } else { 0 })
                .ok_or(ImportError::Overflow)?,
        )
        .ok_or(ImportError::Overflow)?;
    let unit_spool_control = unit_work_units
        .checked_mul(SPOOL_JOURNAL_RECORD_BYTES + SPOOL_WATERMARK_RECORD_BYTES)
        .and_then(|value| value.checked_add(SPOOL_FIXED_CONTROL_BYTES))
        .ok_or(ImportError::Overflow)?;
    let canonical_cache_control = spatial_shape_zyx[0]
        .checked_mul(CANONICAL_CACHE_STATE_BYTES_PER_PLANE_MAX)
        .and_then(|value| value.checked_add(SPOOL_FIXED_CONTROL_BYTES))
        .ok_or(ImportError::Overflow)?;
    let canonical_unit_cache = unit_decoded_base
        .checked_add(canonical_cache_control)
        .and_then(|value| {
            // One directory and two regular files may each consume a partial
            // filesystem allocation unit beyond their logical lengths.
            value.checked_add(3 * (FILESYSTEM_ALLOCATION_GRANULARITY_MAX - 1))
        })
        .ok_or(ImportError::Overflow)?;
    let packed_control = work_units
        .checked_mul(PACKED_INDEX_RECORD_BYTES)
        .ok_or(ImportError::Overflow)?;
    let stage_journal = pixel_shards
        .checked_mul(if explicit_validity { 2 } else { 1 })
        .and_then(|value| value.checked_add(packed_shards))
        .and_then(|value| value.checked_mul(STAGE_JOURNAL_RECORD_BYTES_MAX))
        .ok_or(ImportError::Overflow)?;
    let tiles_per_unit = spatial_shape_zyx
        .into_iter()
        .zip(SCIENTIFIC_TILE_SHAPE_TZYX[1..].iter().copied())
        .try_fold(1_u64, |product, (dimension, tile)| {
            product
                .checked_mul(dimension.div_ceil(tile))
                .ok_or(ImportError::Overflow)
        })?;
    let maximum_channel_tiles = timepoints
        .checked_mul(tiles_per_unit)
        .ok_or(ImportError::Overflow)?;
    let scientific_checkpoint =
        ScientificLayerHasher::checkpoint_bytes_upper_bound(maximum_channel_tiles)?;
    let unit_journal = temporal_units
        .checked_mul(
            UNIT_JOURNAL_FIXED_RECORD_BYTES
                .checked_add(scientific_checkpoint)
                .ok_or(ImportError::Overflow)?,
        )
        .and_then(|value| value.checked_add(UNIT_JOURNAL_FIXED_HEADER_BYTES))
        .ok_or(ImportError::Overflow)?;
    let decoded_digest_control = temporal_units
        .checked_mul(32)
        .ok_or(ImportError::Overflow)?;
    let no_data_control = automatic_mask_bytes
        .checked_add(
            spatial_shape_zyx[0]
                .checked_mul(8)
                .ok_or(ImportError::Overflow)?,
        )
        .and_then(|value| value.checked_add(NO_DATA_CONTROL_FIXED_BYTES))
        .ok_or(ImportError::Overflow)?;
    let incomplete_object = encoded_outer_shard_limit(pixel_outer_uncompressed)?
        .max(if explicit_validity {
            encoded_outer_shard_limit(validity_outer_uncompressed)?
        } else {
            0
        })
        .max(encoded_outer_shard_limit(packed_outer_uncompressed)?);
    let bounded_unit_scratch = unit_decoded_base
        .checked_add(unit_encoded)
        .and_then(|value| value.checked_add(unit_spool_control))
        .and_then(|value| value.checked_add(canonical_cache_control))
        .and_then(|value| value.checked_add(no_data_control))
        .and_then(|value| value.checked_add(automatic_reconstruction_spool_bytes))
        .and_then(|value| value.checked_add(incomplete_object))
        .and_then(|value| value.checked_add(SPACE_OVERHEAD_BYTES))
        .ok_or(ImportError::Overflow)?;
    let growing_control = packed_control
        .checked_add(stage_journal)
        .and_then(|value| value.checked_add(unit_journal))
        .and_then(|value| value.checked_add(decoded_digest_control))
        .ok_or(ImportError::Overflow)?;
    let maximum_unit_output = unit_object_bounds
        .pixel_bytes
        .checked_add(unit_object_bounds.validity_bytes)
        .and_then(|value| {
            value.checked_add(
                unit_object_bounds
                    .shard_objects
                    .checked_mul(FILESYSTEM_ALLOCATION_GRANULARITY_MAX - 1)?,
            )
        })
        .ok_or(ImportError::Overflow)?;
    let maximum_scientific_checkpoint =
        ScientificLayerHasher::checkpoint_bytes_upper_bound(u64::MAX)?;
    let unit_control_increment = unit_work_units
        .checked_mul(PACKED_INDEX_RECORD_BYTES)
        .and_then(|value| {
            value.checked_add(
                unit_object_bounds
                    .shard_objects
                    .checked_mul(STAGE_JOURNAL_RECORD_BYTES_MAX)?,
            )
        })
        .and_then(|value| {
            value.checked_add(
                UNIT_JOURNAL_FIXED_RECORD_BYTES.checked_add(maximum_scientific_checkpoint)?,
            )
        })
        .and_then(|value| value.checked_add(32))
        .ok_or(ImportError::Overflow)?;
    let finalization_headroom = finalization_headroom(profile, scale_count, explicit_validity)?;
    if finalization_commit > finalization_headroom {
        return Err(ImportError::InvalidRequest(
            "request finalization bound exceeded the profile-derived reserve",
        ));
    }
    let start_required_headroom = bounded_unit_scratch
        .checked_add(maximum_unit_output)
        .and_then(|value| value.checked_add(unit_control_increment))
        .and_then(|value| value.checked_add(finalization_headroom))
        .ok_or(ImportError::Overflow)?;
    Ok(StoragePlacementPlan {
        final_package,
        bounded_unit_scratch,
        canonical_unit_cache,
        growing_control,
        maximum_unit_output,
        unit_control_increment,
        finalization_headroom,
        finalization_commit,
        start_required_headroom,
    })
}

fn plan_digest(
    options: &ImportOptions,
    shapes: &[Shape4D],
    resolved: Option<&ResolvedNoDataPolicy>,
) -> Sha256Digest {
    let mut hasher = Sha256Hasher::new();
    hasher.update(b"MIRANTE4D-IMPORT-PLAN-V2\0");
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
    match resolved {
        Some(policy) => {
            hasher.update(b"resolved-no-data\0");
            hasher.update(NO_DATA_ALGORITHM_ID);
            hasher.update([u8::from(policy.request().is_some())]);
            if let Some(request) = policy.request() {
                match request.value_rule() {
                    None => hasher.update([0, 0]),
                    Some(NoDataValueRule::Automatic) => hasher.update([1, 0]),
                    Some(NoDataValueRule::ManualUint8(value)) => hasher.update([2, value]),
                }
                hasher.update([u8::from(request.hides_constant_z_planes())]);
            }
            match policy.value() {
                None => hasher.update([0]),
                Some(ResolvedNoDataValue::Uint8(value)) => hasher.update([1, value]),
                Some(ResolvedNoDataValue::Uint16(value)) => {
                    hasher.update([2]);
                    hasher.update(value.to_le_bytes());
                }
                Some(ResolvedNoDataValue::Float32Bits(bits)) => {
                    hasher.update([3]);
                    hasher.update(bits.to_le_bytes());
                }
            }
            match policy.automatic_mask() {
                None => hasher.update([0]),
                Some(mask) => {
                    hasher.update([1]);
                    for dimension in mask.shape_zyx() {
                        hasher.update(dimension.to_le_bytes());
                    }
                    hasher.update(mask.masked_voxels().to_le_bytes());
                    hasher.update(mask.digest().as_bytes());
                }
            }
            hasher.update(policy.base_depth().to_le_bytes());
            hasher.update(
                u64::try_from(policy.constant_z_planes().len())
                    .unwrap_or(u64::MAX)
                    .to_le_bytes(),
            );
            for z in policy.constant_z_planes() {
                hasher.update(z.to_le_bytes());
            }
        }
        None => {
            // This digest is used only for conservative preflight/profile
            // selection and never binds a durable checkpoint.
            hasher.update(b"unresolved-no-data\0");
            hasher.update([u8::from(options.no_data.is_some())]);
        }
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
    use crate::{SpatialCalibration, TiffInspection, TiffSource};

    fn options(shape: Shape4D) -> ImportOptions {
        ImportOptions {
            inspection: TiffInspection {
                source: TiffSource::single_3d("source.tif"),
                files: Vec::new(),
                source_index_working_bytes: 1,
                shape,
                channels: 1,
                channel_labels: vec!["channel 1".to_owned()],
                dtype: IntensityDType::Uint16,
                ome_spacing_zyx_um: None,
                source_bytes: 1,
                source_fingerprint: Sha256Digest::parse(&"1".repeat(64)).unwrap(),
                maximum_decoded_chunk_bytes: 65_536,
                maximum_encoded_chunk_bytes: 65_536,
            },
            destination: PathBuf::from("output.m4d"),
            checkpoint_directory: PathBuf::from("checkpoint"),
            profile: ProfileKind::Current,
            calibration: SpatialCalibration::new([1.0; 3]),
            time_step_seconds: None,
            no_data: None,
        }
    }

    #[test]
    fn plan_is_deterministic_and_bounded() {
        let options = options(Shape4D::new(1, 8, 300, 300).unwrap());
        let first = ImportPlan::new(&options).unwrap();
        let second = ImportPlan::new(&options).unwrap();
        assert_eq!(first.plan_digest, second.plan_digest);
        assert_eq!(first.shapes.len(), 4);
        assert!(first.work_units > 0);
    }

    #[test]
    fn every_profile_uses_the_same_geometry_derived_scale_count() {
        let mut options = options(Shape4D::new(1, 2, 2, 2).unwrap());
        options.profile = ProfileKind::Current;
        let plan = ImportPlan::new(&options).unwrap();
        assert_eq!(plan.shapes, vec![Shape4D::new(1, 2, 2, 2).unwrap()]);
    }

    #[test]
    fn supported_profile_selection_uses_the_compositional_contract() {
        let mut options = options(Shape4D::new(1, 513, 256, 256).unwrap());
        options.profile = ProfileKind::Current;
        ImportPlan::new(&options).unwrap();
        assert_eq!(
            select_supported_profile(&options).unwrap(),
            ProfileKind::Current
        );
    }

    #[test]
    fn temporal_growth_beyond_predecessor_fixtures_is_admitted() {
        let options = options(Shape4D::new(15_000, 1, 512, 512).unwrap());
        assert_eq!(
            select_supported_profile(&options).unwrap(),
            ProfileKind::Current
        );
    }

    #[test]
    fn temporal_growth_stops_at_the_manifest_addressability_authority() {
        let admitted = options(Shape4D::new(60_000, 1, 1, 1).unwrap());
        assert!(ImportPlan::new(&admitted).is_ok());

        let rejected = options(Shape4D::new(MANIFEST_DESCRIPTORS_MAX, 1, 1, 1).unwrap());
        assert!(matches!(
            ImportPlan::new(&rejected),
            Err(ImportError::Storage(StorageProfileError::CeilingExceeded {
                metric: "manifest descriptors",
                ..
            }))
        ));
    }

    #[test]
    fn multi_timepoint_terminal_tail_selects_the_recalibrated_profile() {
        let mut options = options(Shape4D::new(365, 74, 608, 600).unwrap());
        options.inspection.dtype = IntensityDType::Float32;
        assert_eq!(
            select_supported_profile(&options).unwrap(),
            ProfileKind::Current
        );
        options.profile = ProfileKind::Current;
        let plan = ImportPlan::new(&options).unwrap();
        assert_eq!(plan.shapes.len(), 5);
        assert_eq!(
            plan.logical_bricks_by_scale,
            [73_000, 9_125, 3_285, 1_460, 365]
        );
        assert_eq!(plan.work_units, 87_235);
    }

    #[test]
    fn sentinel_is_restricted_to_uint8() {
        let mut options = options(Shape4D::new(1, 1, 2, 2).unwrap());
        options.no_data = Some(crate::NoDataPolicy::manual_uint8(255));
        assert!(matches!(
            ImportPlan::new(&options),
            Err(ImportError::InvalidRequest(_))
        ));
    }

    #[test]
    fn resolved_no_data_plan_digest_binds_the_typed_policy() {
        let mut options = options(Shape4D::new(1, 1, 2, 2).unwrap());
        options.inspection.dtype = IntensityDType::Uint8;
        options.no_data = Some(crate::NoDataPolicy::manual_uint8(255));
        let resolved = ResolvedNoDataPolicy::new(
            options.no_data,
            Some(ResolvedNoDataValue::Uint8(255)),
            None,
            Vec::new(),
            1,
        )
        .unwrap();
        let plan = ImportPlan::new_resolved(&options, &resolved).unwrap();
        let changed = ResolvedNoDataPolicy::new(
            Some(crate::NoDataPolicy::manual_uint8(254)),
            Some(ResolvedNoDataValue::Uint8(254)),
            None,
            Vec::new(),
            1,
        )
        .unwrap();
        let mut changed_options = options.clone();
        changed_options.no_data = Some(crate::NoDataPolicy::manual_uint8(254));
        assert_ne!(
            plan.plan_digest,
            ImportPlan::new_resolved(&changed_options, &changed)
                .unwrap()
                .plan_digest
        );

        let all_valid = ResolvedNoDataPolicy::all_valid(1);
        let mut all_valid_options = options.clone();
        all_valid_options.no_data = None;
        assert_ne!(
            plan.plan_digest,
            ImportPlan::new_resolved(&all_valid_options, &all_valid)
                .unwrap()
                .plan_digest
        );
    }

    #[test]
    fn automatic_plan_binds_and_admits_the_fixed_spatial_mask() {
        let shape = Shape4D::new(1, 6, 6, 6).unwrap();
        let mut options = options(shape);
        options.no_data = Some(crate::NoDataPolicy::automatic());
        let conservative = ImportPlan::new(&options).unwrap();

        let make_mask = |offset: usize| {
            let mut bits = vec![0_u8; 6 * 6];
            for z in offset..offset + 5 {
                for y in offset..offset + 5 {
                    let row = z * 6 + y;
                    for x in offset..offset + 5 {
                        bits[row] |= 1 << x;
                    }
                }
            }
            crate::model::ResolvedAutomaticNoDataMask::new([6, 6, 6], bits).unwrap()
        };
        let first = ResolvedNoDataPolicy::new(
            options.no_data,
            Some(ResolvedNoDataValue::Uint16(42)),
            Some(make_mask(0)),
            Vec::new(),
            6,
        )
        .unwrap();
        let second = ResolvedNoDataPolicy::new(
            options.no_data,
            Some(ResolvedNoDataValue::Uint16(42)),
            Some(make_mask(1)),
            Vec::new(),
            6,
        )
        .unwrap();
        let first_plan = ImportPlan::new_resolved(&options, &first).unwrap();
        let second_plan = ImportPlan::new_resolved(&options, &second).unwrap();

        assert_ne!(first_plan.plan_digest, second_plan.plan_digest);
        assert_eq!(
            first_plan.resident_working_bytes,
            conservative.resident_working_bytes
        );
        assert_eq!(
            second_plan.resident_working_bytes,
            conservative.resident_working_bytes
        );
    }

    #[test]
    fn plan_exposes_publication_residency_without_a_request_local_memory_setting() {
        let options = options(Shape4D::new(1, 1, 2, 2).unwrap());
        let plan = ImportPlan::new(&options).unwrap();
        assert!(
            plan.resident_working_bytes
                .checked_add(PUBLICATION_VALIDATION_BYTES_MAX)
                .is_some()
        );
    }

    #[test]
    fn independent_capacity_oracle_matches_guidance_and_incremental_headroom_terms() {
        for timepoints in [1, 50] {
            let options = options(Shape4D::new(timepoints, 17, 300, 257).unwrap());
            let plan = ImportPlan::new(&options).unwrap();
            let oracle = independent_no_data_space_oracle(&options);
            assert_eq!(plan.final_package_upper_bound, oracle.final_package);
            assert_eq!(plan.bounded_unit_scratch_bytes, oracle.bounded_unit_scratch);
            assert_eq!(plan.growing_control_bytes, oracle.growing_control);
            assert_eq!(
                plan.maximum_unit_output_upper_bound,
                oracle.maximum_unit_output
            );
            assert_eq!(
                plan.unit_control_increment_upper_bound,
                oracle.unit_control_increment
            );
            assert_eq!(
                plan.finalization_headroom_bytes,
                oracle.finalization_headroom
            );
            assert_eq!(
                plan.finalization_commit_upper_bound,
                oracle.finalization_commit
            );
            assert_eq!(
                plan.start_required_headroom_bytes,
                oracle.start_required_headroom
            );
        }
    }

    #[test]
    fn temporal_growth_keeps_managed_memory_and_payload_scratch_spatially_bounded() {
        let one = ImportPlan::new(&options(Shape4D::new(1, 1, 256, 256).unwrap())).unwrap();
        let long = ImportPlan::new(&options(Shape4D::new(20_000, 1, 256, 256).unwrap())).unwrap();

        assert_eq!(long.unit_work_units, one.unit_work_units);
        assert_eq!(long.resident_working_bytes, one.resident_working_bytes);
        assert_eq!(long.minimum_execution_bytes, one.minimum_execution_bytes);
        assert_eq!(
            long.bounded_unit_scratch_bytes,
            one.bounded_unit_scratch_bytes
        );
        assert_eq!(
            long.canonical_unit_cache_bytes,
            one.canonical_unit_cache_bytes
        );
        assert!(long.final_package_upper_bound > one.final_package_upper_bound);
        assert!(long.growing_control_bytes > one.growing_control_bytes);
        assert_eq!(
            long.maximum_unit_output_upper_bound,
            one.maximum_unit_output_upper_bound
        );
        assert_eq!(
            long.unit_control_increment_upper_bound,
            one.unit_control_increment_upper_bound
        );
        assert_eq!(
            long.finalization_headroom_bytes,
            one.finalization_headroom_bytes
        );
        assert_eq!(
            long.start_required_headroom_bytes,
            one.start_required_headroom_bytes
        );
    }

    #[test]
    fn channel_growth_does_not_change_hard_headroom_for_fixed_spatial_geometry() {
        let one = ImportPlan::new(&options(Shape4D::new(7, 17, 300, 257).unwrap())).unwrap();
        let mut many_options = options(Shape4D::new(7, 17, 300, 257).unwrap());
        many_options.inspection.channels = 12;
        many_options.inspection.channel_labels = (0..12)
            .map(|channel| format!("channel {channel}"))
            .collect();
        let many = ImportPlan::new(&many_options).unwrap();

        assert!(many.final_package_upper_bound > one.final_package_upper_bound);
        assert_eq!(
            many.bounded_unit_scratch_bytes,
            one.bounded_unit_scratch_bytes
        );
        assert_eq!(
            many.canonical_unit_cache_bytes,
            one.canonical_unit_cache_bytes
        );
        assert_eq!(
            many.maximum_unit_output_upper_bound,
            one.maximum_unit_output_upper_bound
        );
        assert_eq!(
            many.unit_control_increment_upper_bound,
            one.unit_control_increment_upper_bound
        );
        assert_eq!(
            many.start_required_headroom_bytes,
            one.start_required_headroom_bytes
        );
    }

    #[test]
    fn edge_objects_pay_only_for_occupied_inner_slots() {
        let plan = ImportPlan::new(&options(Shape4D::new(1, 17, 65, 65).unwrap())).unwrap();
        let full_outer = encoded_outer_shard_limit(
            u64::try_from(plan.pixel_kind.decoded_outer_bytes()).unwrap(),
        )
        .unwrap();
        let full_shard_charge = unit_shard_count_for_test(&plan.shapes, plan.is_2d)
            .checked_mul(full_outer)
            .unwrap();
        assert!(plan.maximum_unit_output_upper_bound < full_shard_charge);
    }

    #[derive(Clone, Copy)]
    struct IndependentSpaceOracle {
        final_package: u64,
        bounded_unit_scratch: u64,
        growing_control: u64,
        maximum_unit_output: u64,
        unit_control_increment: u64,
        finalization_headroom: u64,
        finalization_commit: u64,
        start_required_headroom: u64,
    }

    fn independent_no_data_space_oracle(options: &ImportOptions) -> IndependentSpaceOracle {
        assert!(options.no_data.is_none());
        let shapes = profile_pyramid_shapes(options.inspection.shape).unwrap();
        let is_2d = options.inspection.shape.z() == 1;
        let pixel = pixel_kind(options.inspection.dtype, is_2d);
        let validity = validity_kind(is_2d);
        let units = options.inspection.shape.t() * u64::from(options.inspection.channels);
        let mut unit_work = 0_u64;
        let mut unit_pixel_shards = 0_u64;
        let mut unit_pixel_bytes = 0_u64;
        let mut packed_shards = 0_u64;
        let mut packed_bytes = 0_u64;
        for shape in &shapes {
            let grid = chunk_grid([shape.z(), shape.y(), shape.x()], is_2d);
            let spatial = grid.into_iter().product::<u64>();
            unit_work += spatial;
            let ratios = if is_2d { [1, 4, 4] } else { [4, 4, 4] };
            for oz in 0..grid[0].div_ceil(ratios[0]) {
                for oy in 0..grid[1].div_ceil(ratios[1]) {
                    for ox in 0..grid[2].div_ceil(ratios[2]) {
                        let mut occupied = 0_u64;
                        for lz in 0..ratios[0] {
                            for ly in 0..ratios[1] {
                                for lx in 0..ratios[2] {
                                    occupied += u64::from(
                                        oz * ratios[0] + lz < grid[0]
                                            && oy * ratios[1] + ly < grid[1]
                                            && ox * ratios[2] + lx < grid[2],
                                    );
                                }
                            }
                        }
                        unit_pixel_shards += 1;
                        unit_pixel_bytes += encoded_outer_shard_limit(
                            occupied * u64::try_from(pixel.decoded_inner_bytes()).unwrap(),
                        )
                        .unwrap();
                    }
                }
            }
            let records = units * spatial;
            let mut remaining = records;
            while remaining > 0 {
                let outer_records = remaining.min(PACKED_INDEX_RECORDS_PER_OUTER_SHARD);
                let inner_chunks = outer_records.div_ceil(PACKED_INDEX_RECORDS_PER_INNER_CHUNK);
                packed_bytes += encoded_outer_shard_limit(
                    inner_chunks
                        * u64::try_from(ShardProfileKind::PackedIndex.decoded_inner_bytes())
                            .unwrap(),
                )
                .unwrap();
                packed_shards += 1;
                remaining -= outer_records;
            }
        }
        let pixel_shards = units * unit_pixel_shards;
        let work_units = units * unit_work;
        let pixel_outer = u64::try_from(pixel.decoded_inner_bytes()).unwrap()
            * u64::try_from(pixel.chunks_per_shard()).unwrap();
        let validity_outer = u64::try_from(validity.decoded_inner_bytes()).unwrap()
            * u64::try_from(validity.chunks_per_shard()).unwrap();
        let packed_kind = ShardProfileKind::PackedIndex;
        let packed_outer = u64::try_from(packed_kind.decoded_inner_bytes()).unwrap()
            * u64::try_from(packed_kind.chunks_per_shard()).unwrap();
        let zarr_objects = 5 + 2 * u64::try_from(shapes.len()).unwrap();
        let descriptors = pixel_shards + packed_shards + zarr_objects + 3;
        let pages = descriptors.div_ceil(MANIFEST_DESCRIPTORS_PER_PAGE_GUARANTEED);
        let portable_max = u64::try_from(MAX_PORTABLE_CONTROL_OBJECT_BYTES).unwrap();
        let metadata = zarr_objects * u64::try_from(MAX_ZARR_METADATA_BYTES).unwrap()
            + u64::try_from(MAX_PROFILE_HEADER_BYTES).unwrap()
            + 2 * portable_max;
        let final_package = units * unit_pixel_bytes
            + packed_bytes
            + metadata
            + (pages + 1) * portable_max
            + (descriptors + pages + 1) * (FILESYSTEM_ALLOCATION_GRANULARITY_MAX - 1)
            + SPACE_OVERHEAD_BYTES;
        let finalization_objects = packed_shards + zarr_objects + 3 + pages + 1;
        let finalization_commit = packed_bytes
            + packed_shards * STAGE_JOURNAL_RECORD_BYTES_MAX
            + metadata
            + (pages + 1) * portable_max
            + finalization_objects * (FILESYSTEM_ALLOCATION_GRANULARITY_MAX - 1)
            + encoded_outer_shard_limit(packed_outer).unwrap()
            + SPACE_OVERHEAD_BYTES;

        let decoded_unit = options.inspection.shape.z()
            * options.inspection.shape.y()
            * options.inspection.shape.x()
            * u64::from(options.inspection.dtype.bytes_per_sample());
        let encoded_unit = unit_work
            * encoded_inner_payload_limit(u64::try_from(pixel.decoded_inner_bytes()).unwrap())
                .unwrap();
        let spool_control = unit_work * (SPOOL_JOURNAL_RECORD_BYTES + SPOOL_WATERMARK_RECORD_BYTES)
            + SPOOL_FIXED_CONTROL_BYTES;
        let cache_control = options.inspection.shape.z()
            * CANONICAL_CACHE_STATE_BYTES_PER_PLANE_MAX
            + SPOOL_FIXED_CONTROL_BYTES;
        let no_data_control = options.inspection.shape.z() * 8 + NO_DATA_CONTROL_FIXED_BYTES;
        let incomplete = encoded_outer_shard_limit(pixel_outer)
            .unwrap()
            .max(encoded_outer_shard_limit(validity_outer).unwrap())
            .max(encoded_outer_shard_limit(packed_outer).unwrap());
        let bounded_unit_scratch = decoded_unit
            + encoded_unit
            + spool_control
            + cache_control
            + no_data_control
            + incomplete
            + SPACE_OVERHEAD_BYTES;
        let tiles_per_unit = [
            options
                .inspection
                .shape
                .z()
                .div_ceil(SCIENTIFIC_TILE_SHAPE_TZYX[1]),
            options
                .inspection
                .shape
                .y()
                .div_ceil(SCIENTIFIC_TILE_SHAPE_TZYX[2]),
            options
                .inspection
                .shape
                .x()
                .div_ceil(SCIENTIFIC_TILE_SHAPE_TZYX[3]),
        ]
        .into_iter()
        .product::<u64>();
        let checkpoint = ScientificLayerHasher::checkpoint_bytes_upper_bound(
            options.inspection.shape.t() * tiles_per_unit,
        )
        .unwrap();
        let unit_journal = units * (UNIT_JOURNAL_FIXED_RECORD_BYTES + checkpoint)
            + UNIT_JOURNAL_FIXED_HEADER_BYTES;
        let growing_control = work_units * PACKED_INDEX_RECORD_BYTES
            + (pixel_shards + packed_shards) * STAGE_JOURNAL_RECORD_BYTES_MAX
            + unit_journal
            + units * 32;
        let maximum_unit_output =
            unit_pixel_bytes + unit_pixel_shards * (FILESYSTEM_ALLOCATION_GRANULARITY_MAX - 1);
        let maximum_checkpoint =
            ScientificLayerHasher::checkpoint_bytes_upper_bound(u64::MAX).unwrap();
        let unit_control_increment = unit_work * PACKED_INDEX_RECORD_BYTES
            + unit_pixel_shards * STAGE_JOURNAL_RECORD_BYTES_MAX
            + UNIT_JOURNAL_FIXED_RECORD_BYTES
            + maximum_checkpoint
            + 32;
        let finalization_headroom = independent_finalization_headroom(
            options.profile,
            u64::try_from(shapes.len()).unwrap(),
            false,
        );
        let start_required_headroom = bounded_unit_scratch
            + maximum_unit_output
            + unit_control_increment
            + finalization_headroom;
        IndependentSpaceOracle {
            final_package,
            bounded_unit_scratch,
            growing_control,
            maximum_unit_output,
            unit_control_increment,
            finalization_headroom,
            finalization_commit,
            start_required_headroom,
        }
    }

    fn independent_finalization_headroom(
        profile: ProfileKind,
        scale_count: u64,
        explicit_validity: bool,
    ) -> u64 {
        let limits = profile_limits(profile);
        let scale_rounding = limits.scales.maximum() - 1;
        let packed_inner_chunks = limits
            .logical_bricks
            .div_ceil(PACKED_INDEX_RECORDS_PER_INNER_CHUNK)
            + scale_rounding;
        let packed_shards = limits
            .logical_bricks
            .div_ceil(PACKED_INDEX_RECORDS_PER_OUTER_SHARD)
            + scale_rounding;
        let packed_bytes = packed_inner_chunks
            * encoded_inner_payload_limit(
                u64::try_from(ShardProfileKind::PackedIndex.decoded_inner_bytes()).unwrap(),
            )
            .unwrap()
            + packed_shards * FILESYSTEM_ALLOCATION_GRANULARITY_MAX
            + packed_shards * STAGE_JOURNAL_RECORD_BYTES_MAX;
        let portable = u64::try_from(MAX_PORTABLE_CONTROL_OBJECT_BYTES).unwrap();
        let zarr_objects = 5 + scale_count * if explicit_validity { 3 } else { 2 };
        let metadata = zarr_objects * u64::try_from(MAX_ZARR_METADATA_BYTES).unwrap()
            + u64::try_from(MAX_PROFILE_HEADER_BYTES).unwrap()
            + 2 * portable;
        let pages = MANIFEST_DESCRIPTORS_MAX.div_ceil(MANIFEST_DESCRIPTORS_PER_PAGE_GUARANTEED);
        let manifest = (pages + 1) * portable;
        let objects = packed_shards + zarr_objects + 3 + pages + 1;
        let incomplete = encoded_outer_shard_limit(
            u64::try_from(ShardProfileKind::PackedIndex.decoded_outer_bytes()).unwrap(),
        )
        .unwrap();
        packed_bytes
            + metadata
            + manifest
            + objects * (FILESYSTEM_ALLOCATION_GRANULARITY_MAX - 1)
            + incomplete
            + SPACE_OVERHEAD_BYTES
    }

    fn unit_shard_count_for_test(shapes: &[Shape4D], is_2d: bool) -> u64 {
        let ratios = if is_2d { [1, 4, 4] } else { [4, 4, 4] };
        shapes
            .iter()
            .map(|shape| {
                chunk_grid([shape.z(), shape.y(), shape.x()], is_2d)
                    .into_iter()
                    .zip(ratios)
                    .map(|(count, ratio)| count.div_ceil(ratio))
                    .product::<u64>()
            })
            .sum()
    }
}

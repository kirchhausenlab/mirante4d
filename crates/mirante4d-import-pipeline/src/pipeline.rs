use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use mirante4d_dataset::{CpuByteLease, CpuByteLedger, CpuLedgerCategory};
use mirante4d_domain::{GridToWorld, IntensityDType, LogicalLayerKey};
use mirante4d_identity::{
    SCIENTIFIC_TILE_SHAPE_TZYX, ScientificDatasetHasher, ScientificLayerDescriptor,
    ScientificLayerHasher, ScientificTemporalCalibration, ScientificTile,
};
use mirante4d_storage::{PackedIndexRecord, ProfileKind};

use crate::{
    ImportCancellation, ImportError, ImportEvent, ImportOptions, ImportReceipt, ImportStatistics,
    NoDataPolicy, TiffInspection, TiffSource,
    chunk::{
        checked_voxels, chunk_extent, chunk_grid, chunk_shape, compact_byte_len,
        copy_padded_region, normalize_u8_sentinel, prepare_chunk, unpack_validity,
    },
    package::{PackageMetadataInput, build_package_metadata},
    plan::ImportPlan,
    publish::publish_package,
    pyramid::downsample_region,
    spool::{ImportSpool, SpoolBinding, SpoolChunkInput, SpoolWorkUnitKey},
};

pub fn inspect_tiff(source: TiffSource) -> Result<TiffInspection, ImportError> {
    crate::source::inspect(source)
}

/// Inspects a TIFF source while allowing a background caller to stop bounded work.
pub fn inspect_tiff_cancellable(
    source: TiffSource,
    cancellation: &ImportCancellation,
) -> Result<TiffInspection, ImportError> {
    crate::source::inspect_cancellable(source, cancellation)
}

/// Chooses the first storage profile whose complete import plan is supported.
///
/// The fixed order is an internal storage decision; callers do not need to
/// present the DS profile codes as a user choice. `import_tiff` continues to
/// accept an explicit profile for focused tests and diagnostics.
pub fn select_supported_profile(options: &ImportOptions) -> Result<ProfileKind, ImportError> {
    crate::plan::select_supported_profile(options)
}

pub fn import_tiff(
    options: ImportOptions,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    progress: impl FnMut(ImportEvent),
) -> Result<ImportReceipt, ImportError> {
    run(options, ledger, cancellation, progress)
}

fn run(
    options: ImportOptions,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    mut progress: impl FnMut(ImportEvent),
) -> Result<ImportReceipt, ImportError> {
    check_cancelled(cancellation)?;
    let plan = ImportPlan::new(&options)?;
    validate_path_separation(&options)?;
    require_absent_destination(&options.destination)?;
    preflight_free_space(&options, &plan)?;
    crate::source::revalidate(&options.inspection, cancellation)?;
    prepare_checkpoint_directory(&options.checkpoint_directory)?;

    let binding = SpoolBinding::new(plan.plan_digest, options.inspection.source_fingerprint);
    let mut statistics = ImportStatistics::default();
    let _spool_record_lease =
        ledger.try_acquire(CpuLedgerCategory::ImportWorkingSet, plan.spool_record_bytes)?;
    statistics.peak_working_bytes = plan.spool_record_bytes;
    let spool_open_bytes = plan
        .pixel_kind
        .encoded_inner_bytes_max()
        .checked_add(plan.pixel_kind.decoded_inner_bytes())
        .and_then(|value| {
            if plan.explicit_validity {
                value
                    .checked_add(plan.validity_kind.encoded_inner_bytes_max())?
                    .checked_add(plan.validity_kind.decoded_inner_bytes())
            } else {
                Some(value)
            }
        })
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(ImportError::Overflow)?;
    let spool_open_lease = reserve_phase(
        &options,
        ledger,
        spool_open_bytes,
        plan.spool_record_bytes,
        &mut statistics,
    )?;
    let mut spool = ImportSpool::open_or_create(
        &options.checkpoint_directory,
        binding,
        plan.work_units,
        || cancellation.is_cancelled(),
    )?;
    drop(spool_open_lease);
    validate_checkpoint_prefix(&spool, &options, &plan)?;
    produce_base(
        &options,
        &plan,
        &mut spool,
        ledger,
        cancellation,
        &mut progress,
        &mut statistics,
    )?;
    produce_coarse_levels(
        &options,
        &plan,
        &mut spool,
        ledger,
        cancellation,
        &mut progress,
        &mut statistics,
    )?;
    if spool.len() != usize::try_from(plan.work_units).map_err(|_| ImportError::Overflow)? {
        return Err(ImportError::InvalidCheckpoint(
            "checkpoint does not contain the complete import plan".to_owned(),
        ));
    }

    crate::source::revalidate(&options.inspection, cancellation)?;
    check_cancelled(cancellation)?;
    progress(ImportEvent::HashingScience);
    let scientific_content_id = hash_scientific_content(
        &options,
        plan.spool_record_bytes,
        ledger,
        cancellation,
        &mut statistics,
    )?;
    crate::source::revalidate(&options.inspection, cancellation)?;
    check_cancelled(cancellation)?;

    let metadata = build_package_metadata(&PackageMetadataInput {
        profile_kind: options.profile,
        scientific_content_id,
        base_shape: options.inspection.shape,
        channel_count: options.inspection.channels,
        dtype: options.inspection.dtype,
        pyramid_shapes: plan.shapes.clone(),
        spacing_zyx_um: options.calibration.spacing_zyx_um,
        regular_time_step_seconds: options.time_step_seconds,
        explicit_validity: plan.explicit_validity,
        source_file_sha256: options
            .inspection
            .files
            .iter()
            .map(|file| file.sha256)
            .collect(),
        u8_sentinel: sentinel(&options),
    })?;

    progress(ImportEvent::Publishing);
    let receipt = publish_package(
        &options.destination,
        metadata,
        &mut spool,
        &plan,
        options.working_memory_bytes,
        plan.spool_record_bytes,
        ledger,
        cancellation,
        &mut statistics.peak_working_bytes,
    )?;
    drop(spool);
    cleanup_checkpoint(&options.checkpoint_directory);
    progress(ImportEvent::Finished);
    Ok(ImportReceipt {
        package_id: receipt.package_id(),
        scientific_content_id,
        statistics,
    })
}

fn cleanup_checkpoint(path: &Path) {
    for name in ["header", "journal", "payload"] {
        let _ = fs::remove_file(path.join(name));
    }
    let _ = fs::remove_dir(path);
}

#[allow(clippy::too_many_arguments)]
fn produce_base(
    options: &ImportOptions,
    plan: &ImportPlan,
    spool: &mut ImportSpool,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    progress: &mut impl FnMut(ImportEvent),
    statistics: &mut ImportStatistics,
) -> Result<(), ImportError> {
    let shape = plan.shapes[0];
    let grid = chunk_grid([shape.z(), shape.y(), shape.x()], plan.is_2d);
    let inner = chunk_shape(plan.is_2d);
    for t in 0..shape.t() {
        for c in 0..u64::from(options.inspection.channels) {
            for z in 0..grid[0] {
                for y in 0..grid[1] {
                    for x in 0..grid[2] {
                        check_cancelled(cancellation)?;
                        let key = work_key(0, t, c, [z, y, x])?;
                        if spool.contains(key) {
                            completed_existing(plan, statistics, progress)?;
                            continue;
                        }
                        let logical =
                            chunk_extent([shape.z(), shape.y(), shape.x()], [z, y, x], plan.is_2d)?;
                        let compact_bytes = compact_byte_len(options.inspection.dtype, logical)?;
                        let validity_bytes = if plan.explicit_validity {
                            u64::try_from(checked_voxels(logical)?)
                                .map_err(|_| ImportError::Overflow)?
                        } else {
                            0
                        };
                        let phase_bytes = options
                            .inspection
                            .maximum_decoded_chunk_bytes
                            .checked_add(crate::source::SOURCE_DECODE_OVERHEAD_BYTES_MAX)
                            .and_then(|value| {
                                value
                                    .checked_add(u64::try_from(compact_bytes).map_err(|_| ()).ok()?)
                            })
                            .and_then(|value| {
                                value.checked_add(
                                    u64::try_from(plan.pixel_kind.decoded_inner_bytes()).ok()?,
                                )
                            })
                            .and_then(|value| value.checked_add(validity_bytes))
                            .and_then(|value| {
                                value.checked_add(
                                    u64::try_from(plan.pixel_kind.encoded_inner_bytes_max())
                                        .ok()?,
                                )
                            })
                            .and_then(|value| {
                                if plan.explicit_validity {
                                    value
                                        .checked_add(
                                            u64::try_from(plan.validity_kind.decoded_inner_bytes())
                                                .ok()?,
                                        )
                                        .and_then(|value| {
                                            value.checked_add(
                                                u64::try_from(
                                                    plan.validity_kind.encoded_inner_bytes_max(),
                                                )
                                                .ok()?,
                                            )
                                        })
                                } else {
                                    Some(value)
                                }
                            })
                            .ok_or(ImportError::Overflow)?;
                        let _lease = reserve_phase(
                            options,
                            ledger,
                            phase_bytes,
                            plan.spool_record_bytes,
                            statistics,
                        )?;
                        let mut pixels = vec![0; compact_bytes];
                        let counters = crate::source::read_region_into(
                            &options.inspection,
                            u32::try_from(c).map_err(|_| ImportError::Overflow)?,
                            t,
                            [z * inner[0], y * inner[1], x * inner[2]],
                            logical,
                            &mut pixels,
                            options.inspection.maximum_decoded_chunk_bytes,
                            cancellation,
                        )?;
                        statistics.source_bytes_read = statistics
                            .source_bytes_read
                            .checked_add(counters.source_bytes_read)
                            .ok_or(ImportError::Overflow)?;
                        let validity = sentinel(options)
                            .map(|sentinel| normalize_u8_sentinel(&mut pixels, sentinel));
                        let prepared = prepare_chunk(
                            key.coordinates(),
                            options.inspection.dtype,
                            plan.is_2d,
                            logical,
                            &pixels,
                            validity.as_deref(),
                        )?;
                        append_prepared(spool, key, plan, &prepared)?;
                        completed_produced(plan, statistics, progress)?;
                    }
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn produce_coarse_levels(
    options: &ImportOptions,
    plan: &ImportPlan,
    spool: &mut ImportSpool,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    progress: &mut impl FnMut(ImportEvent),
    statistics: &mut ImportStatistics,
) -> Result<(), ImportError> {
    let inner = chunk_shape(plan.is_2d);
    for scale in 1..plan.shapes.len() {
        let shape = plan.shapes[scale];
        let previous = plan.shapes[scale - 1];
        let grid = chunk_grid([shape.z(), shape.y(), shape.x()], plan.is_2d);
        for t in 0..shape.t() {
            for c in 0..u64::from(options.inspection.channels) {
                for z in 0..grid[0] {
                    for y in 0..grid[1] {
                        for x in 0..grid[2] {
                            check_cancelled(cancellation)?;
                            let key = work_key(scale, t, c, [z, y, x])?;
                            if spool.contains(key) {
                                completed_existing(plan, statistics, progress)?;
                                continue;
                            }
                            let target_extent = chunk_extent(
                                [shape.z(), shape.y(), shape.x()],
                                [z, y, x],
                                plan.is_2d,
                            )?;
                            let target_origin = [z * inner[0], y * inner[1], x * inner[2]];
                            let source_origin = target_origin.map(|value| value * 2);
                            let previous_shape = [previous.z(), previous.y(), previous.x()];
                            let mut source_extent = [0; 3];
                            for axis in 0..3 {
                                source_extent[axis] = (target_extent[axis] * 2)
                                    .min(previous_shape[axis] - source_origin[axis]);
                            }
                            let source_pixels =
                                compact_byte_len(options.inspection.dtype, source_extent)?;
                            let source_voxels = checked_voxels(source_extent)?;
                            let target_pixels =
                                compact_byte_len(options.inspection.dtype, target_extent)?;
                            let phase_bytes = u64::try_from(
                                source_pixels
                                    .checked_add(target_pixels)
                                    .and_then(|value| {
                                        value.checked_add(plan.pixel_kind.decoded_inner_bytes())
                                    })
                                    .and_then(|value| {
                                        value.checked_add(plan.pixel_kind.encoded_inner_bytes_max())
                                    })
                                    .and_then(|value| {
                                        if plan.explicit_validity {
                                            value
                                                .checked_add(source_voxels)?
                                                .checked_add(checked_voxels(target_extent).ok()?)?
                                                .checked_add(
                                                    plan.validity_kind.decoded_inner_bytes(),
                                                )
                                                .and_then(|value| {
                                                    value.checked_add(
                                                        plan.validity_kind
                                                            .encoded_inner_bytes_max(),
                                                    )
                                                })
                                        } else {
                                            Some(value)
                                        }
                                    })
                                    .ok_or(ImportError::Overflow)?,
                            )
                            .map_err(|_| ImportError::Overflow)?;
                            let _lease = reserve_phase(
                                options,
                                ledger,
                                phase_bytes,
                                plan.spool_record_bytes,
                                statistics,
                            )?;
                            let (source_pixels, source_validity) = read_spooled_region(
                                spool,
                                options.inspection.dtype,
                                plan,
                                scale - 1,
                                t,
                                c,
                                previous_shape,
                                source_origin,
                                source_extent,
                            )?;
                            let downsampled = downsample_region(
                                options.inspection.dtype,
                                source_extent,
                                &source_pixels,
                                source_validity.as_deref(),
                            )?;
                            if downsampled.shape_zyx != target_extent {
                                return Err(ImportError::InvalidCheckpoint(
                                    "coarse chunk shape differs from its import plan".to_owned(),
                                ));
                            }
                            let prepared = prepare_chunk(
                                key.coordinates(),
                                options.inspection.dtype,
                                plan.is_2d,
                                target_extent,
                                &downsampled.pixels_le,
                                downsampled.validity.as_deref(),
                            )?;
                            append_prepared(spool, key, plan, &prepared)?;
                            completed_produced(plan, statistics, progress)?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn read_spooled_region(
    spool: &mut ImportSpool,
    dtype: IntensityDType,
    plan: &ImportPlan,
    scale: usize,
    t: u64,
    c: u64,
    level_shape: [u64; 3],
    origin: [u64; 3],
    extent: [u64; 3],
) -> Result<(Vec<u8>, Option<Vec<u8>>), ImportError> {
    let width = usize::from(dtype.bytes_per_sample());
    let mut pixels = vec![0; compact_byte_len(dtype, extent)?];
    let mut validity = plan
        .explicit_validity
        .then(|| vec![0; checked_voxels(extent).expect("extent was already checked")]);
    let inner = chunk_shape(plan.is_2d);
    let start = [
        origin[0] / inner[0],
        origin[1] / inner[1],
        origin[2] / inner[2],
    ];
    let end = [
        (origin[0] + extent[0] - 1) / inner[0],
        (origin[1] + extent[1] - 1) / inner[1],
        (origin[2] + extent[2] - 1) / inner[2],
    ];
    for z in start[0]..=end[0] {
        for y in start[1]..=end[1] {
            for x in start[2]..=end[2] {
                let key = work_key(scale, t, c, [z, y, x])?;
                let unit = spool.read_work_unit(key)?.ok_or_else(|| {
                    ImportError::InvalidCheckpoint(
                        "a coarse level depends on a missing work unit".to_owned(),
                    )
                })?;
                let logical = chunk_extent(level_shape, [z, y, x], plan.is_2d)?;
                let capacity =
                    u64::try_from(checked_voxels(logical)?).map_err(|_| ImportError::Overflow)?;
                let record = PackedIndexRecord::decode(&unit.packed_index, dtype, capacity)
                    .map_err(|error| {
                        ImportError::InvalidCheckpoint(format!(
                            "a packed-index record is invalid: {error}"
                        ))
                    })?;
                let chunk_origin = [z * inner[0], y * inner[1], x * inner[2]];
                let mut overlap_origin = [0; 3];
                let mut overlap_end = [0; 3];
                for axis in 0..3 {
                    overlap_origin[axis] = origin[axis].max(chunk_origin[axis]);
                    overlap_end[axis] =
                        (origin[axis] + extent[axis]).min(chunk_origin[axis] + logical[axis]);
                }
                let overlap = [
                    overlap_end[0] - overlap_origin[0],
                    overlap_end[1] - overlap_origin[1],
                    overlap_end[2] - overlap_origin[2],
                ];
                let source_local = [
                    overlap_origin[0] - chunk_origin[0],
                    overlap_origin[1] - chunk_origin[1],
                    overlap_origin[2] - chunk_origin[2],
                ];
                let destination_local = [
                    overlap_origin[0] - origin[0],
                    overlap_origin[1] - origin[1],
                    overlap_origin[2] - origin[2],
                ];
                if let Some(pixel) = unit.pixel {
                    if pixel.kind != plan.pixel_kind {
                        return Err(ImportError::InvalidCheckpoint(
                            "a spooled pixel chunk has the wrong storage kind".to_owned(),
                        ));
                    }
                    copy_padded_region(
                        &pixel.decoded,
                        inner,
                        source_local,
                        overlap,
                        width,
                        &mut pixels,
                        extent,
                        destination_local,
                    )?;
                } else if record.pixel_payload_present() {
                    return Err(ImportError::InvalidCheckpoint(
                        "a required spooled pixel chunk is absent".to_owned(),
                    ));
                }

                if let Some(destination) = validity.as_mut() {
                    let compact = match unit.validity {
                        Some(validity) => {
                            if validity.kind != plan.validity_kind {
                                return Err(ImportError::InvalidCheckpoint(
                                    "a spooled validity chunk has the wrong storage kind"
                                        .to_owned(),
                                ));
                            }
                            unpack_validity(&validity.decoded, logical, plan.is_2d)?
                        }
                        None if record.all_voxels_valid() => {
                            vec![1; checked_voxels(logical)?]
                        }
                        None if record.all_voxels_invalid() => {
                            vec![0; checked_voxels(logical)?]
                        }
                        None => {
                            return Err(ImportError::InvalidCheckpoint(
                                "an explicit-validity record has no effective mask".to_owned(),
                            ));
                        }
                    };
                    copy_mask_region(
                        &compact,
                        logical,
                        source_local,
                        overlap,
                        destination,
                        extent,
                        destination_local,
                    )?;
                }
            }
        }
    }
    Ok((pixels, validity))
}

fn hash_scientific_content(
    options: &ImportOptions,
    resident_working_bytes: u64,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    statistics: &mut ImportStatistics,
) -> Result<mirante4d_identity::ScientificContentId, ImportError> {
    let shape = options.inspection.shape;
    let temporal = match options.time_step_seconds {
        Some(step_seconds) => ScientificTemporalCalibration::Regular { step_seconds },
        None => ScientificTemporalCalibration::Unknown,
    };
    let grid_to_world = GridToWorld::scale(
        options.calibration.spacing_zyx_um[2],
        options.calibration.spacing_zyx_um[1],
        options.calibration.spacing_zyx_um[0],
    )
    .map_err(|_| ImportError::InvalidRequest("spatial calibration is not a valid transform"))?;
    let mut dataset = ScientificDatasetHasher::new(options.inspection.channels)?;
    for channel in 0..options.inspection.channels {
        let descriptor = ScientificLayerDescriptor::new(
            LogicalLayerKey::new(channel),
            options.inspection.dtype,
            shape,
            temporal.clone(),
            grid_to_world,
        )?;
        let mut layer = ScientificLayerHasher::new(descriptor)?;
        for t in 0..shape.t() {
            for z in (0..shape.z()).step_by(SCIENTIFIC_TILE_SHAPE_TZYX[1] as usize) {
                for y in (0..shape.y()).step_by(SCIENTIFIC_TILE_SHAPE_TZYX[2] as usize) {
                    for x in (0..shape.x()).step_by(SCIENTIFIC_TILE_SHAPE_TZYX[3] as usize) {
                        check_cancelled(cancellation)?;
                        let extent = [
                            (shape.z() - z).min(SCIENTIFIC_TILE_SHAPE_TZYX[1]),
                            (shape.y() - y).min(SCIENTIFIC_TILE_SHAPE_TZYX[2]),
                            (shape.x() - x).min(SCIENTIFIC_TILE_SHAPE_TZYX[3]),
                        ];
                        let pixels_len = compact_byte_len(options.inspection.dtype, extent)?;
                        let voxels = checked_voxels(extent)?;
                        let phase_bytes = options
                            .inspection
                            .maximum_decoded_chunk_bytes
                            .checked_add(crate::source::SOURCE_DECODE_OVERHEAD_BYTES_MAX)
                            .and_then(|value| value.checked_add(u64::try_from(pixels_len).ok()?))
                            .and_then(|value| value.checked_add(u64::try_from(voxels).ok()?))
                            .and_then(|value| {
                                value.checked_add(u64::try_from(voxels).ok()?.div_ceil(8))
                            })
                            .ok_or(ImportError::Overflow)?;
                        let _lease = reserve_phase(
                            options,
                            ledger,
                            phase_bytes,
                            resident_working_bytes,
                            statistics,
                        )?;
                        let mut pixels = vec![0; pixels_len];
                        let counters = crate::source::read_region_into(
                            &options.inspection,
                            channel,
                            t,
                            [z, y, x],
                            extent,
                            &mut pixels,
                            options.inspection.maximum_decoded_chunk_bytes,
                            cancellation,
                        )?;
                        statistics.source_bytes_read = statistics
                            .source_bytes_read
                            .checked_add(counters.source_bytes_read)
                            .ok_or(ImportError::Overflow)?;
                        let per_voxel_validity = match sentinel(options) {
                            Some(sentinel) => normalize_u8_sentinel(&mut pixels, sentinel),
                            None => vec![1; voxels],
                        };
                        let validity = pack_scientific_validity(&per_voxel_validity)?;
                        layer.push_tile(ScientificTile::new(
                            [t, z, y, x],
                            [1, extent[0], extent[1], extent[2]],
                            &validity,
                            &pixels,
                        ))?;
                    }
                }
            }
        }
        dataset.push_layer(layer.finalize()?)?;
    }
    Ok(dataset.finalize()?)
}

fn append_prepared(
    spool: &mut ImportSpool,
    key: SpoolWorkUnitKey,
    plan: &ImportPlan,
    prepared: &crate::chunk::PreparedChunk,
) -> Result<(), ImportError> {
    let pixel = prepared
        .pixel
        .as_deref()
        .map(|decoded| SpoolChunkInput::new(plan.pixel_kind, decoded));
    let validity = prepared
        .validity
        .as_deref()
        .map(|decoded| SpoolChunkInput::new(plan.validity_kind, decoded));
    if !spool.append_if_absent(key, pixel, validity, prepared.record)? {
        return Err(ImportError::InvalidCheckpoint(
            "a new work unit unexpectedly already exists".to_owned(),
        ));
    }
    Ok(())
}

fn reserve_phase<'a>(
    options: &ImportOptions,
    ledger: &'a dyn CpuByteLedger,
    bytes: u64,
    resident_bytes: u64,
    statistics: &mut ImportStatistics,
) -> Result<Box<dyn CpuByteLease + 'a>, ImportError> {
    let combined = resident_bytes
        .checked_add(bytes)
        .ok_or(ImportError::Overflow)?;
    if bytes == 0 || combined > options.working_memory_bytes {
        return Err(ImportError::WorkingMemoryExceeded {
            required_bytes: combined.max(1),
            budget_bytes: options.working_memory_bytes,
        });
    }
    let lease = ledger.try_acquire(CpuLedgerCategory::ImportWorkingSet, bytes)?;
    statistics.peak_working_bytes = statistics.peak_working_bytes.max(combined);
    Ok(lease)
}

fn sentinel(options: &ImportOptions) -> Option<u8> {
    options.no_data.map(|NoDataPolicy::U8Sentinel(value)| value)
}

fn work_key(
    scale: usize,
    t: u64,
    c: u64,
    chunk: [u64; 3],
) -> Result<SpoolWorkUnitKey, ImportError> {
    Ok(SpoolWorkUnitKey::new(
        0,
        u32::try_from(scale).map_err(|_| ImportError::Overflow)?,
        u32::try_from(t).map_err(|_| ImportError::Overflow)?,
        u32::try_from(c).map_err(|_| ImportError::Overflow)?,
        u32::try_from(chunk[0]).map_err(|_| ImportError::Overflow)?,
        u32::try_from(chunk[1]).map_err(|_| ImportError::Overflow)?,
        u32::try_from(chunk[2]).map_err(|_| ImportError::Overflow)?,
    ))
}

fn completed_existing(
    plan: &ImportPlan,
    statistics: &mut ImportStatistics,
    progress: &mut impl FnMut(ImportEvent),
) -> Result<(), ImportError> {
    statistics.resumed_work_units = statistics
        .resumed_work_units
        .checked_add(1)
        .ok_or(ImportError::Overflow)?;
    report_progress(plan, statistics, progress)
}

fn completed_produced(
    plan: &ImportPlan,
    statistics: &mut ImportStatistics,
    progress: &mut impl FnMut(ImportEvent),
) -> Result<(), ImportError> {
    statistics.produced_work_units = statistics
        .produced_work_units
        .checked_add(1)
        .ok_or(ImportError::Overflow)?;
    report_progress(plan, statistics, progress)
}

fn report_progress(
    plan: &ImportPlan,
    statistics: &ImportStatistics,
    progress: &mut impl FnMut(ImportEvent),
) -> Result<(), ImportError> {
    let completed = statistics
        .produced_work_units
        .checked_add(statistics.resumed_work_units)
        .ok_or(ImportError::Overflow)?;
    progress(ImportEvent::Producing {
        completed_work_units: completed,
        total_work_units: plan.work_units,
    });
    Ok(())
}

fn validate_checkpoint_prefix(
    spool: &ImportSpool,
    options: &ImportOptions,
    plan: &ImportPlan,
) -> Result<(), ImportError> {
    let mut expected = expected_keys(options, plan);
    for actual in spool.keys() {
        if Some(actual) != expected.next() {
            return Err(ImportError::InvalidCheckpoint(
                "checkpoint work units are not a prefix of this import plan".to_owned(),
            ));
        }
    }
    Ok(())
}

fn expected_keys<'a>(
    options: &'a ImportOptions,
    plan: &'a ImportPlan,
) -> impl Iterator<Item = SpoolWorkUnitKey> + 'a {
    plan.shapes
        .iter()
        .enumerate()
        .flat_map(move |(scale, shape)| {
            let grid = chunk_grid([shape.z(), shape.y(), shape.x()], plan.is_2d);
            (0..shape.t()).flat_map(move |t| {
                (0..u64::from(options.inspection.channels)).flat_map(move |c| {
                    (0..grid[0]).flat_map(move |z| {
                        (0..grid[1]).flat_map(move |y| {
                            (0..grid[2]).map(move |x| {
                                work_key(scale, t, c, [z, y, x])
                                    .expect("profile bounds make work keys fit u32")
                            })
                        })
                    })
                })
            })
        })
}

fn require_absent_destination(destination: &Path) -> Result<(), ImportError> {
    if destination.exists() {
        return Err(mirante4d_storage::PackageWriteError::DestinationExists.into());
    }
    let parent = destination.parent().ok_or(ImportError::InvalidRequest(
        "destination must have an existing parent directory",
    ))?;
    let metadata = fs::metadata(parent).map_err(|source| ImportError::Io {
        operation: "inspect destination parent",
        path: parent.to_path_buf(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(ImportError::InvalidRequest(
            "destination parent must be a directory",
        ));
    }
    Ok(())
}

fn validate_path_separation(options: &ImportOptions) -> Result<(), ImportError> {
    let source =
        fs::canonicalize(&options.inspection.source.path).map_err(|source| ImportError::Io {
            operation: "resolve source root",
            path: options.inspection.source.path.clone(),
            source,
        })?;
    let destination = resolved_candidate(&options.destination)?;
    let checkpoint = resolved_candidate(&options.checkpoint_directory)?;
    if nested(&source, &destination)
        || nested(&source, &checkpoint)
        || nested(&destination, &checkpoint)
    {
        return Err(ImportError::InvalidRequest(
            "source, destination, and checkpoint paths must be separate and unnested",
        ));
    }
    Ok(())
}

fn resolved_candidate(path: &Path) -> Result<PathBuf, ImportError> {
    if path.exists() {
        return fs::canonicalize(path).map_err(|source| ImportError::Io {
            operation: "resolve import path",
            path: path.to_path_buf(),
            source,
        });
    }
    let name = path.file_name().ok_or(ImportError::InvalidRequest(
        "import paths must name a filesystem entry",
    ))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(parent).map_err(|source| ImportError::Io {
        operation: "resolve import parent",
        path: parent.to_path_buf(),
        source,
    })?;
    Ok(parent.join(name))
}

fn nested(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn preflight_free_space(options: &ImportOptions, plan: &ImportPlan) -> Result<(), ImportError> {
    let destination_parent = options
        .destination
        .parent()
        .ok_or(ImportError::InvalidRequest(
            "destination must have a parent directory",
        ))?;
    let checkpoint_parent = options
        .checkpoint_directory
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    for path in BTreeSet::from([destination_parent, checkpoint_parent]) {
        let filesystem = rustix::fs::statvfs(path).map_err(|source| ImportError::Io {
            operation: "inspect free filesystem space",
            path: path.to_path_buf(),
            source: source.into(),
        })?;
        let available = filesystem
            .f_bavail
            .checked_mul(filesystem.f_frsize)
            .ok_or(ImportError::Overflow)?;
        require_available_space(plan.free_space_required, available)?;
    }
    Ok(())
}

fn prepare_checkpoint_directory(path: &Path) -> Result<(), ImportError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path).map_err(|source| ImportError::Io {
                operation: "inspect checkpoint directory",
                path: path.to_path_buf(),
                source,
            })?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(ImportError::InvalidCheckpoint(
                    "checkpoint path is not a real directory".to_owned(),
                ));
            }
            let allowed = BTreeSet::from(["header", "journal", "payload"]);
            for entry in fs::read_dir(path).map_err(|source| ImportError::Io {
                operation: "list checkpoint directory",
                path: path.to_path_buf(),
                source,
            })? {
                let entry = entry.map_err(|source| ImportError::Io {
                    operation: "read checkpoint directory entry",
                    path: path.to_path_buf(),
                    source,
                })?;
                let name = entry.file_name();
                if !name.to_str().is_some_and(|name| allowed.contains(name)) {
                    return Err(ImportError::InvalidCheckpoint(
                        "checkpoint directory contains an unrelated entry".to_owned(),
                    ));
                }
            }
            Ok(())
        }
        Err(source) => Err(ImportError::Io {
            operation: "create checkpoint directory",
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn copy_mask_region(
    source: &[u8],
    source_shape: [u64; 3],
    source_origin: [u64; 3],
    extent: [u64; 3],
    destination: &mut [u8],
    destination_shape: [u64; 3],
    destination_origin: [u64; 3],
) -> Result<(), ImportError> {
    if source.len() != checked_voxels(source_shape)?
        || destination.len() != checked_voxels(destination_shape)?
    {
        return Err(ImportError::InvalidCheckpoint(
            "validity region length differs from its shape".to_owned(),
        ));
    }
    for z in 0..extent[0] {
        for y in 0..extent[1] {
            for x in 0..extent[2] {
                let source_index = linear_index(
                    source_shape,
                    [
                        source_origin[0] + z,
                        source_origin[1] + y,
                        source_origin[2] + x,
                    ],
                )?;
                let destination_index = linear_index(
                    destination_shape,
                    [
                        destination_origin[0] + z,
                        destination_origin[1] + y,
                        destination_origin[2] + x,
                    ],
                )?;
                destination[destination_index] = source[source_index];
            }
        }
    }
    Ok(())
}

fn linear_index(shape: [u64; 3], coordinate: [u64; 3]) -> Result<usize, ImportError> {
    let index = coordinate[0]
        .checked_mul(shape[1])
        .and_then(|value| value.checked_add(coordinate[1]))
        .and_then(|value| value.checked_mul(shape[2]))
        .and_then(|value| value.checked_add(coordinate[2]))
        .ok_or(ImportError::Overflow)?;
    usize::try_from(index).map_err(|_| ImportError::Overflow)
}

fn pack_scientific_validity(per_voxel: &[u8]) -> Result<Vec<u8>, ImportError> {
    let mut packed = vec![0; per_voxel.len().div_ceil(8)];
    for (index, valid) in per_voxel.iter().copied().enumerate() {
        match valid {
            0 => {}
            1 => packed[index / 8] |= 1 << (index % 8),
            _ => {
                return Err(ImportError::InvalidRequest(
                    "scientific validity must contain canonical bits",
                ));
            }
        }
    }
    Ok(packed)
}

fn check_cancelled(cancellation: &ImportCancellation) -> Result<(), ImportError> {
    if cancellation.is_cancelled() {
        Err(ImportError::Cancelled)
    } else {
        Ok(())
    }
}

fn require_available_space(required_bytes: u64, available_bytes: u64) -> Result<(), ImportError> {
    if available_bytes < required_bytes {
        Err(ImportError::InsufficientSpace {
            required_bytes,
            available_bytes,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_preflight_refuses_before_production() {
        assert!(matches!(
            require_available_space(101, 100),
            Err(ImportError::InsufficientSpace {
                required_bytes: 101,
                available_bytes: 100,
            })
        ));
        require_available_space(100, 100).unwrap();
    }
}

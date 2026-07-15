use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use mirante4d_dataset::{CpuByteLease, CpuByteLedger, CpuLedgerCategory};
use mirante4d_domain::{GridToWorld, LogicalLayerKey};
use mirante4d_identity::{
    SCIENTIFIC_TILE_SHAPE_TZYX, ScientificDatasetHasher, ScientificLayerDescriptor,
    ScientificLayerHasher, ScientificTemporalCalibration,
};
use mirante4d_storage::{INNER_CODEC_WORKING_BYTES_MAX, ProfileKind};

use crate::{
    ImportCancellation, ImportError, ImportEvent, ImportOptions, ImportReceipt, ImportStage,
    ImportStatistics, NoDataPolicy, PublishedImport, TiffInspection, TiffSource,
    canonical_cache::{CanonicalBaseCache, CanonicalCacheBinding},
    chunk::{chunk_extent, chunk_grid, chunk_shape},
    cpu_chunk::{
        BaseChunkCpuTask, EncodedPreparedWorkUnit, PyramidChunkCpuTask, PyramidSourceChunk,
        ScientificTileCpuTask, base_task_charge_bytes, prepare_base_chunk, prepare_pyramid_chunk,
        prepare_scientific_tile, pyramid_task_charge_bytes, scientific_task_charge_bytes,
    },
    observability::{
        IMPORT_OPEN_FILE_DESCRIPTOR_STRUCTURAL_BOUND, ImportFileDescriptorMonitor, PrimaryClock,
        StageClock, conservative_file_descriptor_peak, sample_process_resources,
    },
    ordered_workers::{OrderedWorkerDiagnostics, OrderedWorkerPolicy, run_ordered},
    package::{PackageMetadataInput, build_package_metadata},
    plan::ImportPlan,
    publish::publish_package,
    spool::{ImportSpool, SpoolBinding, SpoolEncodedChunkInput, SpoolWorkUnitKey},
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
) -> Result<PublishedImport, ImportError> {
    run(options, ledger, cancellation, progress)
}

fn run(
    options: ImportOptions,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    mut progress: impl FnMut(ImportEvent),
) -> Result<PublishedImport, ImportError> {
    let primary_clock = PrimaryClock::start()?;
    let file_descriptor_monitor = ImportFileDescriptorMonitor::start(
        &options.inspection.source.path,
        &options.checkpoint_directory,
        &options.destination,
    )?;
    let mut statistics = ImportStatistics::default();

    let planning_clock =
        StageClock::start(ImportStage::PlanningAndPreflight, 0, None, &mut progress)?;
    check_cancelled(cancellation)?;
    let plan = ImportPlan::new(&options)?;
    statistics.logical_output_bytes = plan.logical_output_bytes;
    statistics.preflight_temporary_bytes_bound = plan.free_space_required;
    statistics.open_file_descriptor_structural_bound = IMPORT_OPEN_FILE_DESCRIPTOR_STRUCTURAL_BOUND;
    if statistics.open_file_descriptor_structural_bound > 64 {
        return Err(ImportError::InvalidRequest(
            "the import file-descriptor structural bound exceeds the product limit",
        ));
    }
    validate_path_separation(&options)?;
    require_absent_destination(&options.destination)?;
    preflight_free_space(&options, &plan)?;
    finish_stage(planning_clock, &options, &mut statistics, &mut progress)?;

    let _source_index_lease = ledger.try_acquire(
        CpuLedgerCategory::ImportWorkingSet,
        plan.source_index_working_bytes,
    )?;
    statistics.peak_working_bytes = plan.source_index_working_bytes;

    let revalidation_clock = StageClock::start(
        ImportStage::SourceRevalidation { pass: 1 },
        0,
        None,
        &mut progress,
    )?;
    let source_generation = crate::source::capture_generation(&options.inspection, cancellation)?;
    finish_stage(revalidation_clock, &options, &mut statistics, &mut progress)?;

    let checkpoint_clock = StageClock::start(
        ImportStage::CheckpointOpenOrResume,
        0,
        Some(plan.work_units),
        &mut progress,
    )?;
    prepare_checkpoint_directory(&options.checkpoint_directory)?;

    let spool_binding = SpoolBinding::new(plan.plan_digest, options.inspection.source_fingerprint);
    let cache_binding =
        CanonicalCacheBinding::new(plan.plan_digest, options.inspection.source_fingerprint);
    let _spool_record_lease =
        ledger.try_acquire(CpuLedgerCategory::ImportWorkingSet, plan.spool_record_bytes)?;
    statistics.peak_working_bytes = plan.resident_working_bytes;
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
        .and_then(|value| value.checked_add(INNER_CODEC_WORKING_BYTES_MAX))
        .ok_or(ImportError::Overflow)?
        .max(1024 * 1024);
    let spool_open_lease = reserve_phase(
        &options,
        ledger,
        spool_open_bytes,
        plan.resident_working_bytes,
        &mut statistics,
    )?;
    let mut spool = ImportSpool::open_or_create(
        &options.checkpoint_directory,
        spool_binding,
        plan.work_units,
        || cancellation.is_cancelled(),
    )?;
    let mut canonical = CanonicalBaseCache::open_or_create_in(
        &options.checkpoint_directory,
        spool.directory_fd(),
        cache_binding,
        options.inspection.shape,
        options.inspection.channels,
        options.inspection.dtype,
    )?;
    drop(spool_open_lease);
    validate_checkpoint_prefix(&spool, &options, &plan)?;
    finish_stage(checkpoint_clock, &options, &mut statistics, &mut progress)?;

    let base_clock = StageClock::start(
        ImportStage::BaseProduction,
        completed_work_units(&statistics)?,
        Some(plan.logical_bricks_by_scale[0]),
        &mut progress,
    )?;
    if !canonical.is_complete() {
        let report = crate::source::decode_canonical_into_cache(
            &options.inspection,
            &source_generation,
            &mut canonical,
            options.inspection.maximum_decoded_chunk_bytes,
            options.working_memory_bytes,
            plan.resident_working_bytes,
            ledger,
            cancellation,
            |_completed, _total| {},
        )?;
        statistics.peak_working_bytes = statistics.peak_working_bytes.max(
            plan.resident_working_bytes
                .checked_add(report.peak_transient_bytes)
                .ok_or(ImportError::Overflow)?,
        );
        record_source_read(&mut statistics, report.counters, false)?;
    }
    produce_base(
        &options,
        &plan,
        &mut canonical,
        &mut spool,
        ledger,
        cancellation,
        &mut progress,
        &mut statistics,
    )?;
    finish_stage(base_clock, &options, &mut statistics, &mut progress)?;
    produce_coarse_levels(
        &options,
        &plan,
        &mut spool,
        ledger,
        cancellation,
        &mut progress,
        &mut statistics,
    )?;
    spool.commit_pending()?;
    if spool.len() != usize::try_from(plan.work_units).map_err(|_| ImportError::Overflow)? {
        return Err(ImportError::InvalidCheckpoint(
            "checkpoint does not contain the complete import plan".to_owned(),
        ));
    }

    check_cancelled(cancellation)?;
    let scientific_clock = StageClock::start(
        ImportStage::SourceScientificIdentity,
        0,
        None,
        &mut progress,
    )?;
    let scientific_content_id = hash_scientific_content(
        &options,
        &mut canonical,
        plan.resident_working_bytes,
        ledger,
        cancellation,
        &mut statistics,
    )?;
    finish_stage(scientific_clock, &options, &mut statistics, &mut progress)?;
    let revalidation_clock = StageClock::start(
        ImportStage::SourceRevalidation { pass: 2 },
        0,
        None,
        &mut progress,
    )?;
    let revalidation = crate::source::revalidate(&options.inspection, cancellation)?;
    if revalidation.generation != source_generation {
        return Err(ImportError::SourceChanged(
            options.inspection.source.path.clone(),
        ));
    }
    record_revalidation(&mut statistics, &revalidation)?;
    finish_stage(revalidation_clock, &options, &mut statistics, &mut progress)?;
    check_cancelled(cancellation)?;
    record_checkpoint_diagnostics(
        &mut statistics,
        &canonical,
        &spool,
        &options.checkpoint_directory,
    )?;
    enforce_prepublication_resource_bounds(&statistics)?;

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

    let published = publish_package(
        &options.destination,
        metadata,
        &mut spool,
        &plan,
        options.working_memory_bytes,
        plan.resident_working_bytes,
        ledger,
        cancellation,
        &mut statistics,
        &mut progress,
    )?;
    // The atomically renamed destination has the exact byte footprint of the
    // immediately preceding owned stage. Sampling it while the checkpoint is
    // still present records the real peak checkpoint-plus-stage footprint.
    if sample_process_resources(
        &mut statistics,
        &[&options.checkpoint_directory, &options.destination],
    )
    .is_err()
    {
        // The destination is already durably visible. Diagnostics become
        // fail-closed qualification evidence, never a false import failure.
        statistics.peak_process_rss_bytes = u64::MAX;
        statistics.peak_temporary_bytes = u64::MAX;
    }
    let package_receipt = published.receipt();
    if let Some(validation) = package_receipt.scientific_validation_report() {
        statistics.scientific_brick_reads = validation.brick_reads();
        statistics.scientific_payload_object_reads = validation.object_reads();
        statistics.scientific_range_requests = validation.range_requests();
        statistics.scientific_encoded_bytes_read = validation.encoded_bytes_read();
        statistics.scientific_decoded_bytes = validation.decoded_bytes();
    } else {
        statistics.scientific_brick_reads = u64::MAX;
        statistics.scientific_payload_object_reads = u64::MAX;
        statistics.scientific_range_requests = u64::MAX;
        statistics.scientific_encoded_bytes_read = u64::MAX;
        statistics.scientific_decoded_bytes = u64::MAX;
    }
    let validation_reads = package_receipt.validation_read_report();
    statistics.staged_structure_object_reads = validation_reads.structure_object_reads();
    statistics.staged_exact_object_reads = validation_reads.exact_object_reads();
    statistics.scientific_object_reads = validation_reads.scientific_object_reads();
    statistics.object_reads = validation_reads.total_object_reads().unwrap_or(u64::MAX);
    let final_spool_diagnostics = spool.diagnostics();
    let validation_codecs = package_receipt.codec_report();
    statistics.codec_encode_calls = final_spool_diagnostics
        .codec_encode_calls
        .saturating_add(validation_codecs.encode_calls());
    statistics.codec_encode_time_ns = final_spool_diagnostics
        .codec_encode_time_ns
        .saturating_add(validation_codecs.encode_time_ns());
    statistics.codec_decode_calls = final_spool_diagnostics
        .codec_decode_calls
        .saturating_add(validation_codecs.decode_calls());
    statistics.codec_decode_time_ns = final_spool_diagnostics
        .codec_decode_time_ns
        .saturating_add(validation_codecs.decode_time_ns());
    statistics.sync_calls = statistics
        .sync_calls
        .saturating_add(package_receipt.sync_calls());
    statistics.sync_time_ns = statistics
        .sync_time_ns
        .saturating_add(package_receipt.sync_time_ns());
    let package_id = published.package_id();
    let published_scientific_content_id = published.scientific_content_id();
    canonical.cleanup_owned_files();
    drop(canonical);
    spool.cleanup_owned_files();
    spool.cleanup_owned_directory();
    drop(spool);
    statistics.sampled_peak_open_file_descriptors =
        file_descriptor_monitor.finish_after_publication();
    statistics.peak_open_file_descriptors =
        conservative_file_descriptor_peak(statistics.sampled_peak_open_file_descriptors);
    primary_clock.finish_after_publication(&mut statistics);
    progress(ImportEvent::Published);
    Ok(PublishedImport::new(
        ImportReceipt {
            package_id,
            scientific_content_id: published_scientific_content_id,
            statistics,
        },
        published,
    ))
}

#[allow(clippy::too_many_arguments)]
fn produce_base(
    options: &ImportOptions,
    plan: &ImportPlan,
    canonical: &mut CanonicalBaseCache,
    spool: &mut ImportSpool,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    progress: &mut impl FnMut(ImportEvent),
    statistics: &mut ImportStatistics,
) -> Result<(), ImportError> {
    let shape = plan.shapes[0];
    let stage = ImportStage::BaseProduction;
    let stage_start_completed = completed_work_units(statistics)?;
    let stage_total = plan.logical_bricks_by_scale[0];
    let inner = chunk_shape(plan.is_2d);
    let stage_total_usize = usize::try_from(stage_total).map_err(|_| ImportError::Overflow)?;
    let existing = spool.len().min(stage_total_usize);
    for _ in 0..existing {
        completed_existing(
            stage,
            stage_start_completed,
            stage_total,
            statistics,
            progress,
        )?;
    }
    if existing == stage_total_usize {
        return Ok(());
    }

    let task_charge = base_task_charge_bytes(
        options.inspection.dtype,
        plan.pixel_kind,
        plan.validity_kind,
        plan.explicit_validity,
    )?;
    let policy = OrderedWorkerPolicy::for_system(
        options.working_memory_bytes,
        plan.resident_working_bytes,
        task_charge,
    )?;
    let specifications = expected_keys(options, plan)
        .take(stage_total_usize)
        .skip(existing);
    let canonical_reader = Arc::new(canonical.reader()?);
    let diagnostics = run_ordered(
        policy,
        ledger,
        cancellation,
        specifications,
        spool,
        |_spool, key| {
            check_cancelled(cancellation)?;
            let coordinates = key.coordinates();
            let chunk = [
                u64::from(coordinates.z_chunk()),
                u64::from(coordinates.y_chunk()),
                u64::from(coordinates.x_chunk()),
            ];
            let logical = chunk_extent([shape.z(), shape.y(), shape.x()], chunk, plan.is_2d)?;
            Ok(BaseChunkCpuTask {
                key,
                dtype: options.inspection.dtype,
                is_2d: plan.is_2d,
                logical_shape_zyx: logical,
                canonical: Arc::clone(&canonical_reader),
                channel: coordinates.c(),
                timepoint: u64::from(coordinates.t()),
                origin_zyx: [
                    chunk[0] * inner[0],
                    chunk[1] * inner[1],
                    chunk[2] * inner[2],
                ],
                u8_sentinel: sentinel(options),
            })
        },
        prepare_base_chunk,
        ImportSpool::commit_expired,
        |spool, prepared| {
            append_encoded_prepared(spool, prepared)?;
            completed_produced(
                stage,
                stage_start_completed,
                stage_total,
                statistics,
                progress,
            )
        },
    )?;
    record_worker_diagnostics(
        statistics,
        plan.resident_working_bytes,
        task_charge,
        diagnostics,
    )?;
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
        let stage = ImportStage::PyramidProduction {
            scale: u32::try_from(scale).map_err(|_| ImportError::Overflow)?,
        };
        let stage_start_completed = completed_work_units(statistics)?;
        let stage_total = plan.logical_bricks_by_scale[scale];
        let stage_clock = StageClock::start(stage, 0, Some(stage_total), progress)?;
        let shape = plan.shapes[scale];
        let previous = plan.shapes[scale - 1];
        let prior_work_units = plan.logical_bricks_by_scale[..scale]
            .iter()
            .try_fold(0_u64, |total, count| total.checked_add(*count))
            .ok_or(ImportError::Overflow)?;
        let checkpoint_work_units =
            u64::try_from(spool.len()).map_err(|_| ImportError::Overflow)?;
        let existing = checkpoint_work_units
            .saturating_sub(prior_work_units)
            .min(stage_total);
        for _ in 0..existing {
            completed_existing(
                stage,
                stage_start_completed,
                stage_total,
                statistics,
                progress,
            )?;
        }
        if existing != stage_total {
            let task_charge = pyramid_task_charge_bytes(
                options.inspection.dtype,
                plan.pixel_kind,
                plan.validity_kind,
                plan.is_2d,
                plan.explicit_validity,
            )?;
            let policy = OrderedWorkerPolicy::for_system(
                options.working_memory_bytes,
                plan.resident_working_bytes,
                task_charge,
            )?;
            let skip = usize::try_from(
                prior_work_units
                    .checked_add(existing)
                    .ok_or(ImportError::Overflow)?,
            )
            .map_err(|_| ImportError::Overflow)?;
            let remaining =
                usize::try_from(stage_total - existing).map_err(|_| ImportError::Overflow)?;
            let specifications = expected_keys(options, plan).skip(skip).take(remaining);
            let previous_shape = [previous.z(), previous.y(), previous.x()];
            let payload = Arc::new(spool.payload_reader()?);
            let diagnostics = run_ordered(
                policy,
                ledger,
                cancellation,
                specifications,
                spool,
                |spool, key| {
                    check_cancelled(cancellation)?;
                    let coordinates = key.coordinates();
                    let chunk = [
                        u64::from(coordinates.z_chunk()),
                        u64::from(coordinates.y_chunk()),
                        u64::from(coordinates.x_chunk()),
                    ];
                    let target_extent =
                        chunk_extent([shape.z(), shape.y(), shape.x()], chunk, plan.is_2d)?;
                    let target_origin = [
                        chunk[0] * inner[0],
                        chunk[1] * inner[1],
                        chunk[2] * inner[2],
                    ];
                    let source_origin = target_origin.map(|value| value * 2);
                    let mut source_extent = [0; 3];
                    for axis in 0..3 {
                        source_extent[axis] = (target_extent[axis] * 2)
                            .min(previous_shape[axis] - source_origin[axis]);
                    }
                    let source_chunks = describe_spooled_region(
                        spool,
                        plan,
                        scale - 1,
                        u64::from(coordinates.t()),
                        u64::from(coordinates.c()),
                        source_origin,
                        source_extent,
                    )?;
                    Ok(PyramidChunkCpuTask {
                        key,
                        dtype: options.inspection.dtype,
                        is_2d: plan.is_2d,
                        payload: Arc::clone(&payload),
                        source_chunks,
                        source_level_shape_zyx: previous_shape,
                        source_origin_zyx: source_origin,
                        source_shape_zyx: source_extent,
                        pixel_kind: plan.pixel_kind,
                        validity_kind: plan.validity_kind,
                        explicit_validity: plan.explicit_validity,
                        target_shape_zyx: target_extent,
                    })
                },
                prepare_pyramid_chunk,
                ImportSpool::commit_expired,
                |spool, prepared| {
                    append_encoded_prepared(spool, prepared)?;
                    completed_produced(
                        stage,
                        stage_start_completed,
                        stage_total,
                        statistics,
                        progress,
                    )
                },
            )?;
            record_worker_diagnostics(
                statistics,
                plan.resident_working_bytes,
                task_charge,
                diagnostics,
            )?;
        }
        finish_stage(stage_clock, options, statistics, progress)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn describe_spooled_region(
    spool: &ImportSpool,
    plan: &ImportPlan,
    scale: usize,
    t: u64,
    c: u64,
    origin: [u64; 3],
    extent: [u64; 3],
) -> Result<Vec<PyramidSourceChunk>, ImportError> {
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
    let mut chunks = Vec::with_capacity(if plan.is_2d { 4 } else { 8 });
    for z in start[0]..=end[0] {
        for y in start[1]..=end[1] {
            for x in start[2]..=end[2] {
                let key = work_key(scale, t, c, [z, y, x])?;
                let descriptor = spool.work_unit_descriptor(key).ok_or_else(|| {
                    ImportError::InvalidCheckpoint(
                        "a coarse level depends on a missing work unit".to_owned(),
                    )
                })?;
                chunks.push(PyramidSourceChunk {
                    chunk_zyx: [z, y, x],
                    descriptor,
                });
            }
        }
    }
    Ok(chunks)
}

fn hash_scientific_content(
    options: &ImportOptions,
    canonical: &mut CanonicalBaseCache,
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
    let canonical_reader = Arc::new(canonical.reader()?);
    let task_charge = scientific_task_charge_bytes(options.inspection.dtype)?;
    let policy = OrderedWorkerPolicy::for_system(
        options.working_memory_bytes,
        resident_working_bytes,
        task_charge,
    )?;
    for channel in 0..options.inspection.channels {
        let descriptor = ScientificLayerDescriptor::new(
            LogicalLayerKey::new(channel),
            options.inspection.dtype,
            shape,
            temporal.clone(),
            grid_to_world,
        )?;
        let mut layer = ScientificLayerHasher::new(descriptor.clone())?;
        let specifications = (0..shape.t())
            .flat_map(|t| {
                (0..shape.z())
                    .step_by(SCIENTIFIC_TILE_SHAPE_TZYX[1] as usize)
                    .flat_map(move |z| {
                        (0..shape.y())
                            .step_by(SCIENTIFIC_TILE_SHAPE_TZYX[2] as usize)
                            .flat_map(move |y| {
                                (0..shape.x())
                                    .step_by(SCIENTIFIC_TILE_SHAPE_TZYX[3] as usize)
                                    .map(move |x| (t, z, y, x))
                            })
                    })
            })
            .enumerate();
        let diagnostics = run_ordered(
            policy,
            ledger,
            cancellation,
            specifications,
            &mut layer,
            |_layer, (linear_index, (t, z, y, x))| {
                let extent = [
                    (shape.z() - z).min(SCIENTIFIC_TILE_SHAPE_TZYX[1]),
                    (shape.y() - y).min(SCIENTIFIC_TILE_SHAPE_TZYX[2]),
                    (shape.x() - x).min(SCIENTIFIC_TILE_SHAPE_TZYX[3]),
                ];
                Ok(ScientificTileCpuTask {
                    canonical: Arc::clone(&canonical_reader),
                    descriptor: descriptor.clone(),
                    linear_index: u64::try_from(linear_index).map_err(|_| ImportError::Overflow)?,
                    channel,
                    origin_tzyx: [t, z, y, x],
                    extent_tzyx: [1, extent[0], extent[1], extent[2]],
                    u8_sentinel: sentinel(options),
                })
            },
            prepare_scientific_tile,
            |_| Ok(()),
            |layer, prepared| {
                layer.push_prepared_tile(prepared)?;
                Ok(())
            },
        )?;
        record_worker_diagnostics(statistics, resident_working_bytes, task_charge, diagnostics)?;
        dataset.push_layer(layer.finalize()?)?;
    }
    Ok(dataset.finalize()?)
}

fn append_encoded_prepared(
    spool: &mut ImportSpool,
    prepared: EncodedPreparedWorkUnit,
) -> Result<(), ImportError> {
    let pixel = prepared.pixel.as_ref().map(SpoolEncodedChunkInput::new);
    let validity = prepared.validity.as_ref().map(SpoolEncodedChunkInput::new);
    if !spool.append_encoded_if_absent(
        prepared.key,
        pixel,
        validity,
        prepared.packed_index,
        prepared.codec_encode_calls,
        prepared.codec_encode_time_ns,
    )? {
        return Err(ImportError::InvalidCheckpoint(
            "a new work unit unexpectedly already exists".to_owned(),
        ));
    }
    spool
        .record_worker_codec_decodes(prepared.codec_decode_calls, prepared.codec_decode_time_ns)?;
    Ok(())
}

fn record_worker_diagnostics(
    statistics: &mut ImportStatistics,
    resident_bytes: u64,
    task_charge_bytes: u64,
    diagnostics: OrderedWorkerDiagnostics,
) -> Result<(), ImportError> {
    let task_bytes = u64::try_from(diagnostics.peak_in_flight)
        .map_err(|_| ImportError::Overflow)?
        .checked_mul(task_charge_bytes)
        .ok_or(ImportError::Overflow)?;
    statistics.peak_working_bytes = statistics.peak_working_bytes.max(
        resident_bytes
            .checked_add(task_bytes)
            .ok_or(ImportError::Overflow)?,
    );
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

fn finish_stage(
    clock: StageClock,
    options: &ImportOptions,
    statistics: &mut ImportStatistics,
    progress: &mut impl FnMut(ImportEvent),
) -> Result<(), ImportError> {
    clock.finish(statistics, progress)?;
    sample_process_resources(statistics, &[&options.checkpoint_directory])
}

fn record_revalidation(
    statistics: &mut ImportStatistics,
    counters: &crate::source::SourceRevalidationCounters,
) -> Result<(), ImportError> {
    statistics.source_revalidation_bytes_read = statistics
        .source_revalidation_bytes_read
        .checked_add(counters.source_bytes_read)
        .ok_or(ImportError::Overflow)?;
    statistics.source_bytes_read = statistics
        .source_bytes_read
        .checked_add(counters.source_bytes_read)
        .ok_or(ImportError::Overflow)?;
    statistics.tiff_open_count = statistics
        .tiff_open_count
        .checked_add(counters.tiff_open_count)
        .ok_or(ImportError::Overflow)?;
    Ok(())
}

fn record_source_read(
    statistics: &mut ImportStatistics,
    counters: crate::source::SourceReadCounters,
    scientific_identity: bool,
) -> Result<(), ImportError> {
    statistics.source_bytes_read = statistics
        .source_bytes_read
        .checked_add(counters.source_bytes_read)
        .ok_or(ImportError::Overflow)?;
    statistics.native_decoded_bytes = statistics
        .native_decoded_bytes
        .checked_add(counters.decoded_bytes)
        .ok_or(ImportError::Overflow)?;
    if scientific_identity {
        statistics.scientific_identity_native_decoded_bytes = statistics
            .scientific_identity_native_decoded_bytes
            .checked_add(counters.decoded_bytes)
            .ok_or(ImportError::Overflow)?;
    } else {
        statistics.base_native_decoded_bytes = statistics
            .base_native_decoded_bytes
            .checked_add(counters.decoded_bytes)
            .ok_or(ImportError::Overflow)?;
    }
    statistics.tiff_open_count = statistics
        .tiff_open_count
        .checked_add(counters.tiff_open_count)
        .ok_or(ImportError::Overflow)?;
    statistics.native_chunk_decode_count = statistics
        .native_chunk_decode_count
        .checked_add(counters.native_chunk_decode_count)
        .ok_or(ImportError::Overflow)?;
    Ok(())
}

fn record_checkpoint_diagnostics(
    statistics: &mut ImportStatistics,
    canonical: &CanonicalBaseCache,
    spool: &ImportSpool,
    checkpoint_directory: &Path,
) -> Result<(), ImportError> {
    let cache = canonical.diagnostics()?;
    let spool = spool.diagnostics();
    statistics.checkpoint_payload_bytes = cache
        .data_bytes
        .checked_add(spool.checkpoint_payload_bytes)
        .ok_or(ImportError::Overflow)?;
    statistics.checkpoint_journal_bytes = cache
        .state_bytes
        .checked_add(spool.checkpoint_journal_bytes)
        .ok_or(ImportError::Overflow)?;
    statistics.checkpoint_watermark_bytes = spool.checkpoint_watermark_bytes;
    statistics.checkpoint_durable_work_units = spool.checkpoint_durable_work_units;
    statistics.checkpoint_pending_work_units = spool.checkpoint_pending_work_units;
    statistics.checkpoint_committed_batches = cache
        .committed_batches
        .checked_add(spool.checkpoint_committed_batches)
        .ok_or(ImportError::Overflow)?;
    statistics.sync_calls = cache
        .sync_calls
        .checked_add(spool.sync_calls)
        .ok_or(ImportError::Overflow)?;
    statistics.sync_time_ns = cache
        .sync_time_ns
        .checked_add(spool.sync_time_ns)
        .ok_or(ImportError::Overflow)?;
    statistics.codec_encode_calls = spool.codec_encode_calls;
    statistics.codec_encode_time_ns = spool.codec_encode_time_ns;
    statistics.codec_decode_calls = spool.codec_decode_calls;
    statistics.codec_decode_time_ns = spool.codec_decode_time_ns;
    statistics.peak_checkpoint_regular_files = statistics
        .peak_checkpoint_regular_files
        .max(count_regular_checkpoint_files(checkpoint_directory)?);
    Ok(())
}

fn count_regular_checkpoint_files(directory: &Path) -> Result<u64, ImportError> {
    fs::read_dir(directory)
        .map_err(|source| ImportError::Io {
            operation: "read checkpoint directory for resource accounting",
            path: directory.to_path_buf(),
            source,
        })?
        .try_fold(0_u64, |count, entry| {
            let entry = entry.map_err(|source| ImportError::Io {
                operation: "read checkpoint entry for resource accounting",
                path: directory.to_path_buf(),
                source,
            })?;
            let metadata =
                fs::symlink_metadata(entry.path()).map_err(|source| ImportError::Io {
                    operation: "inspect checkpoint entry for resource accounting",
                    path: entry.path(),
                    source,
                })?;
            if metadata.file_type().is_file() {
                count.checked_add(1).ok_or(ImportError::Overflow)
            } else {
                Ok(count)
            }
        })
}

fn enforce_prepublication_resource_bounds(
    statistics: &ImportStatistics,
) -> Result<(), ImportError> {
    if statistics.peak_temporary_bytes > statistics.preflight_temporary_bytes_bound {
        return Err(ImportError::InvalidRequest(
            "observed importer-owned temporary bytes exceeded the preflight bound",
        ));
    }
    if statistics.peak_checkpoint_regular_files > 8 {
        return Err(ImportError::InvalidRequest(
            "the import checkpoint exceeded its eight-regular-file structural bound",
        ));
    }
    Ok(())
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
    stage: ImportStage,
    stage_start_completed: u64,
    stage_total: u64,
    statistics: &mut ImportStatistics,
    progress: &mut impl FnMut(ImportEvent),
) -> Result<(), ImportError> {
    statistics.resumed_work_units = statistics
        .resumed_work_units
        .checked_add(1)
        .ok_or(ImportError::Overflow)?;
    report_progress(
        stage,
        stage_start_completed,
        stage_total,
        statistics,
        progress,
    )
}

fn completed_produced(
    stage: ImportStage,
    stage_start_completed: u64,
    stage_total: u64,
    statistics: &mut ImportStatistics,
    progress: &mut impl FnMut(ImportEvent),
) -> Result<(), ImportError> {
    statistics.produced_work_units = statistics
        .produced_work_units
        .checked_add(1)
        .ok_or(ImportError::Overflow)?;
    report_progress(
        stage,
        stage_start_completed,
        stage_total,
        statistics,
        progress,
    )
}

fn report_progress(
    stage: ImportStage,
    stage_start_completed: u64,
    stage_total: u64,
    statistics: &ImportStatistics,
    progress: &mut impl FnMut(ImportEvent),
) -> Result<(), ImportError> {
    let completed = completed_work_units(statistics)?;
    let stage_completed = completed
        .checked_sub(stage_start_completed)
        .ok_or(ImportError::Overflow)?;
    progress(ImportEvent::StageProgress {
        stage,
        completed_work_units: stage_completed,
        total_work_units: stage_total,
    });
    Ok(())
}

fn completed_work_units(statistics: &ImportStatistics) -> Result<u64, ImportError> {
    statistics
        .produced_work_units
        .checked_add(statistics.resumed_work_units)
        .ok_or(ImportError::Overflow)
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
            let allowed = BTreeSet::from([
                "header",
                "journal",
                "payload",
                "watermark",
                "canonical-data",
                "canonical-state",
            ]);
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
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
    };

    use mirante4d_dataset::{CpuByteLease, CpuLedgerError};
    use tempfile::tempdir;
    use tiff::encoder::{TiffEncoder, colortype};

    use super::*;

    struct TestLease(u64);

    impl CpuByteLease for TestLease {
        fn category(&self) -> CpuLedgerCategory {
            CpuLedgerCategory::ImportWorkingSet
        }

        fn reserved_bytes(&self) -> u64 {
            self.0
        }
    }

    struct TestLedger;

    impl CpuByteLedger for TestLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            assert_eq!(category, CpuLedgerCategory::ImportWorkingSet);
            assert!(bytes > 0);
            Ok(Box::new(TestLease(bytes)))
        }
    }

    #[derive(Default)]
    struct TrackingState {
        used: u64,
        peak: u64,
    }

    struct TrackingLedger {
        budget: u64,
        state: Arc<Mutex<TrackingState>>,
    }

    struct TrackingLease {
        bytes: u64,
        state: Arc<Mutex<TrackingState>>,
    }

    impl Drop for TrackingLease {
        fn drop(&mut self) {
            self.state.lock().unwrap().used -= self.bytes;
        }
    }

    impl CpuByteLease for TrackingLease {
        fn category(&self) -> CpuLedgerCategory {
            CpuLedgerCategory::ImportWorkingSet
        }

        fn reserved_bytes(&self) -> u64 {
            self.bytes
        }
    }

    impl CpuByteLedger for TrackingLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            assert_eq!(category, CpuLedgerCategory::ImportWorkingSet);
            assert!(bytes > 0);
            let mut state = self.state.lock().unwrap();
            let available = self.budget.saturating_sub(state.used);
            if bytes > available {
                return Err(CpuLedgerError::CapacityExceeded {
                    category,
                    requested_bytes: bytes,
                    available_bytes: available,
                });
            }
            state.used += bytes;
            state.peak = state.peak.max(state.used);
            drop(state);
            Ok(Box::new(TrackingLease {
                bytes,
                state: Arc::clone(&self.state),
            }))
        }
    }

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

    #[test]
    fn serial_and_parallel_import_policies_produce_identical_package_closures() {
        let temporary = tempdir().unwrap();
        let source = temporary.path().join("source");
        let channel = source.join("channel0");
        fs::create_dir_all(&channel).unwrap();
        for z in 0..6_u32 {
            let values = (0..67_u32 * 65)
                .map(|index| u8::try_from((index + z * 17) % 251).unwrap())
                .collect::<Vec<_>>();
            let file = fs::File::create(channel.join(format!("z{z:03}.tif"))).unwrap();
            TiffEncoder::new(file)
                .unwrap()
                .write_image::<colortype::Gray8>(67, 65, &values)
                .unwrap();
        }
        let inspection = crate::source::inspect(TiffSource::auto(&source)).unwrap();

        let serial_destination = temporary.path().join("serial.m4d");
        let serial = crate::ordered_workers::with_test_worker_count(1, || {
            import_with_test_policy(
                inspection.clone(),
                serial_destination.clone(),
                temporary.path().join("serial.checkpoint"),
            )
        });
        let parallel_destination = temporary.path().join("parallel.m4d");
        let parallel = crate::ordered_workers::with_test_worker_count(4, || {
            import_with_test_policy(
                inspection,
                parallel_destination.clone(),
                temporary.path().join("parallel.checkpoint"),
            )
        });

        assert_eq!(serial.scientific_content_id, parallel.scientific_content_id);
        assert_eq!(serial.package_id, parallel.package_id);
        let serial_closure = package_closure(&serial_destination);
        let parallel_closure = package_closure(&parallel_destination);
        assert!(!serial_closure.is_empty());
        assert_eq!(serial_closure, parallel_closure);
    }

    #[test]
    fn reviewed_byte_identical_replacement_fails_before_checkpoint_creation() {
        let temporary = tempdir().unwrap();
        let source = temporary.path().join("source.tif");
        let reviewed = temporary.path().join("reviewed.tif");
        let values = (0..67_u32 * 65)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect::<Vec<_>>();
        let file = fs::File::create(&source).unwrap();
        TiffEncoder::new(file)
            .unwrap()
            .write_image::<colortype::Gray8>(67, 65, &values)
            .unwrap();
        let inspection = crate::source::inspect(TiffSource::auto(&source)).unwrap();

        fs::rename(&source, &reviewed).unwrap();
        fs::copy(&reviewed, &source).unwrap();

        let destination = temporary.path().join("replaced.m4d");
        let checkpoint = temporary.path().join("replaced.checkpoint");
        let error = run(
            ImportOptions {
                inspection,
                destination: destination.clone(),
                checkpoint_directory: checkpoint.clone(),
                profile: ProfileKind::Ds0,
                calibration: crate::SpatialCalibration::new([1.0; 3]),
                time_step_seconds: None,
                no_data: None,
                working_memory_bytes: 512 * 1024 * 1024,
            },
            &TestLedger,
            &ImportCancellation::new(),
            |_| {},
        )
        .unwrap_err();

        assert!(matches!(error, ImportError::SourceChanged(changed) if changed == source));
        assert!(!checkpoint.exists());
        assert!(!destination.exists());
    }

    #[test]
    fn receipt_peak_reconciles_the_source_index_with_the_tracking_ledger() {
        let temporary = tempdir().unwrap();
        let source = temporary.path().join("source");
        let channel = source.join("channel0");
        fs::create_dir_all(&channel).unwrap();
        for z in 0..4_u32 {
            let values = (0..67_u32 * 65)
                .map(|index| u8::try_from((index + z * 17) % 251).unwrap())
                .collect::<Vec<_>>();
            let file =
                fs::File::create(channel.join(format!("long-reviewed-plane-{z:03}.tif"))).unwrap();
            TiffEncoder::new(file)
                .unwrap()
                .write_image::<colortype::Gray8>(67, 65, &values)
                .unwrap();
        }
        let inspection = crate::source::inspect(TiffSource::auto(&source)).unwrap();
        let source_index_working_bytes = inspection.source_index_working_bytes;
        let state = Arc::new(Mutex::new(TrackingState::default()));
        let ledger = TrackingLedger {
            budget: 512 * 1024 * 1024,
            state: Arc::clone(&state),
        };
        let published = run(
            ImportOptions {
                inspection,
                destination: temporary.path().join("tracked.m4d"),
                checkpoint_directory: temporary.path().join("tracked.checkpoint"),
                profile: ProfileKind::Ds0,
                calibration: crate::SpatialCalibration::new([1.0; 3]),
                time_step_seconds: None,
                no_data: None,
                working_memory_bytes: ledger.budget,
            },
            &ledger,
            &ImportCancellation::new(),
            |_| {},
        )
        .unwrap();
        let reported_peak = published.receipt().statistics.peak_working_bytes;
        let observed = state.lock().unwrap();
        assert_eq!(observed.used, 0);
        assert_eq!(reported_peak, observed.peak);
        assert!(reported_peak >= source_index_working_bytes);
    }

    fn import_with_test_policy(
        inspection: TiffInspection,
        destination: PathBuf,
        checkpoint_directory: PathBuf,
    ) -> ImportReceipt {
        run(
            ImportOptions {
                inspection,
                destination,
                checkpoint_directory,
                profile: ProfileKind::Ds0,
                calibration: crate::SpatialCalibration::new([1.0; 3]),
                time_step_seconds: None,
                no_data: None,
                working_memory_bytes: 512 * 1024 * 1024,
            },
            &TestLedger,
            &ImportCancellation::new(),
            |_| {},
        )
        .unwrap()
        .receipt()
        .clone()
    }

    fn package_closure(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        fn visit(root: &Path, directory: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(root, &path, files);
                } else {
                    files.push((
                        path.strip_prefix(root).unwrap().to_owned(),
                        fs::read(path).unwrap(),
                    ));
                }
            }
        }

        let mut files = Vec::new();
        visit(root, root, &mut files);
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    }
}

//! Temporal-unit import directly into the resumable final package layout.

use std::{
    cell::Cell,
    fs,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Instant,
};

use mirante4d_dataset::{CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError};
use mirante4d_domain::{GridToWorld, LogicalLayerKey, Shape4D};
use mirante4d_identity::{
    SCIENTIFIC_TILE_SHAPE_TZYX, ScientificDatasetHasher, ScientificLayerDescriptor,
    ScientificLayerHasher, ScientificTemporalCalibration, Sha256Digest, Sha256Hasher,
};
use mirante4d_storage::{
    PACKED_INDEX_RECORD_BYTES, PackageObjectKind, PackageShardInput, PackageWriteEvent,
    PackageWriteInput, PackageWriteStage, ProfileLevel, ProfileValidityMode,
    ResumableLocalPackageStage, ShardProfileKind,
};

use crate::{
    ImportCancellation, ImportError, ImportEvent, ImportOptions, ImportReceipt, ImportStage,
    ImportStageTiming, ImportStatistics, ImportStorageProgress, PublishedImport,
    canonical_cache::{CanonicalBaseCache, CanonicalCacheBinding},
    checkpoint::{
        PackedRecordStore, UnitCompletion, UnitJournal, control_directory, load_no_data_policy,
        remove_completed_unit_scratch, store_no_data_policy, unit_cache_directory,
        unit_spool_directory,
    },
    chunk::{chunk_extent, chunk_grid, chunk_shape},
    cpu_chunk::{
        BaseChunkCpuTask, EncodedPreparedWorkUnit, PyramidChunkCpuTask, PyramidSourceChunk,
        ScientificTileCpuTask, base_task_charge_bytes, prepare_base_chunk, prepare_pyramid_chunk,
        prepare_scientific_tile, pyramid_pixel_source_chunks_max, pyramid_task_charge_bytes,
        pyramid_validity_source_chunks_max, scientific_task_charge_bytes,
    },
    model::ResolvedNoDataPolicy,
    observability::{
        IMPORT_OPEN_FILE_DESCRIPTOR_STRUCTURAL_BOUND, ImportFileDescriptorMonitor, PrimaryClock,
        StageClock, conservative_file_descriptor_peak, owned_regular_file_bytes,
        sample_process_resources,
    },
    ordered_workers::{
        OrderedWorkerDiagnostics, OrderedWorkerPolicy, run_ordered, system_parallelism,
    },
    package::{PackageMetadata, PackageMetadataInput, build_package_metadata},
    plan::ImportPlan,
    sentinel::{Region3, clipped_halo, invalid_dilation_radius},
    spool::{
        ImportSpool, SpoolBinding, SpoolDiagnostics, SpoolEncodedChunkInput, SpoolWorkUnitKey,
    },
};

const TEMPORAL_DECODE_AHEAD_LANES: usize = 1;

fn temporal_decode_ahead_enabled() -> bool {
    #[cfg(test)]
    if let Some(enabled) = TEST_DECODE_AHEAD_ENABLED.with(Cell::get) {
        return enabled;
    }
    true
}

#[cfg(test)]
std::thread_local! {
    static TEST_DECODE_AHEAD_ENABLED: Cell<Option<bool>> = const { Cell::new(None) };
}

#[cfg(test)]
fn with_test_decode_ahead<R>(enabled: bool, operation: impl FnOnce() -> R) -> R {
    struct Restore<'a> {
        slot: &'a Cell<Option<bool>>,
        prior: Option<bool>,
    }
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            self.slot.set(self.prior);
        }
    }
    TEST_DECODE_AHEAD_ENABLED.with(|slot| {
        let prior = slot.replace(Some(enabled));
        let _restore = Restore { slot, prior };
        operation()
    })
}

#[derive(Clone, Default)]
struct TemporalPipelineStatus {
    future_ordinal: Option<u64>,
    preparing_timepoint: Option<u64>,
    preparing_channel: Option<u32>,
    preparing_completed_planes: Option<Arc<AtomicU64>>,
    preparing_total_planes: u64,
    prepared_temporal_units: u32,
    width: u32,
}

impl TemporalPipelineStatus {
    fn current_only() -> Self {
        Self {
            width: 1,
            ..Self::default()
        }
    }

    fn preparing(
        ordinal: u64,
        timepoint: u64,
        channel: u32,
        completed: Arc<AtomicU64>,
        total: u64,
    ) -> Self {
        Self {
            future_ordinal: Some(ordinal),
            preparing_timepoint: Some(timepoint),
            preparing_channel: Some(channel),
            preparing_completed_planes: Some(completed),
            preparing_total_planes: total,
            prepared_temporal_units: 0,
            width: 2,
        }
    }

    fn prepared(ordinal: u64) -> Self {
        Self {
            future_ordinal: Some(ordinal),
            prepared_temporal_units: 1,
            width: 2,
            ..Self::default()
        }
    }

    fn completed_planes(&self) -> u64 {
        self.preparing_completed_planes
            .as_ref()
            .map_or(0, |completed| completed.load(Ordering::Acquire))
    }
}

struct PreparedTemporalUnit {
    ordinal: u64,
    timepoint: u64,
    channel: u32,
    cache: CanonicalBaseCache,
}

#[derive(Default)]
struct WorkingSetTrackingState {
    used_bytes: u64,
    peak_bytes: u64,
}

/// Run-local observation around the injected process ledger. The process
/// authority still decides every acquisition; this wrapper makes concurrent
/// current/prefetch usage exactly observable without asking receipts to infer
/// overlap by summing phase maxima.
struct TrackingCpuLedger<'a> {
    inner: &'a dyn CpuByteLedger,
    state: Arc<Mutex<WorkingSetTrackingState>>,
}

impl<'a> TrackingCpuLedger<'a> {
    fn new(inner: &'a dyn CpuByteLedger) -> Self {
        Self {
            inner,
            state: Arc::new(Mutex::new(WorkingSetTrackingState::default())),
        }
    }

    fn peak_bytes(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .peak_bytes
    }
}

struct TrackingCpuLease {
    inner: Option<Box<dyn CpuByteLease>>,
    state: Arc<Mutex<WorkingSetTrackingState>>,
    bytes: u64,
}

impl CpuByteLease for TrackingCpuLease {
    fn category(&self) -> CpuLedgerCategory {
        self.inner
            .as_ref()
            .expect("a live tracking lease owns its process lease")
            .category()
    }

    fn reserved_bytes(&self) -> u64 {
        self.inner
            .as_ref()
            .expect("a live tracking lease owns its process lease")
            .reserved_bytes()
    }
}

impl Drop for TrackingCpuLease {
    fn drop(&mut self) {
        drop(self.inner.take());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.used_bytes = state
            .used_bytes
            .checked_sub(self.bytes)
            .expect("a live tracking lease is included in run-local usage");
    }
}

impl CpuByteLedger for TrackingCpuLedger<'_> {
    fn try_acquire(
        &self,
        category: CpuLedgerCategory,
        bytes: u64,
    ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let inner = self.inner.try_acquire(category, bytes)?;
        state.used_bytes =
            state
                .used_bytes
                .checked_add(bytes)
                .ok_or(CpuLedgerError::CapacityExceeded {
                    category,
                    requested_bytes: bytes,
                    available_bytes: 0,
                })?;
        state.peak_bytes = state.peak_bytes.max(state.used_bytes);
        drop(state);
        Ok(Box::new(TrackingCpuLease {
            inner: Some(inner),
            state: Arc::clone(&self.state),
            bytes,
        }))
    }

    fn capacity_epoch(&self) -> u64 {
        self.inner.capacity_epoch()
    }

    fn capacity_bytes(&self) -> u64 {
        self.inner.capacity_bytes()
    }
}

struct PrefetchDecodeResult {
    unit: PreparedTemporalUnit,
    result: Result<Option<crate::source::SourceDecodeReport>, ImportError>,
    ingest_busy_time_ns: u64,
}

/// A decode-ahead lane borrows from only the run's surplus memory. The inner
/// ledger remains the sole process authority; this view merely prevents the
/// optional lane from consuming bytes reserved for the canonical minimum
/// progress path.
struct CappedCpuLedger<'a> {
    inner: &'a dyn CpuByteLedger,
    transient_capacity_bytes: u64,
    reported_capacity_bytes: u64,
    used_bytes: Arc<AtomicU64>,
    capacity_refusals: Arc<AtomicU64>,
}

impl<'a> CappedCpuLedger<'a> {
    fn new(
        inner: &'a dyn CpuByteLedger,
        transient_capacity_bytes: u64,
        resident_working_bytes: u64,
        capacity_refusals: Arc<AtomicU64>,
    ) -> Result<Self, ImportError> {
        Ok(Self {
            inner,
            transient_capacity_bytes,
            reported_capacity_bytes: resident_working_bytes
                .checked_add(transient_capacity_bytes)
                .ok_or(ImportError::Overflow)?,
            used_bytes: Arc::new(AtomicU64::new(0)),
            capacity_refusals,
        })
    }
}

struct CappedCpuLease {
    inner: Box<dyn CpuByteLease>,
    used_bytes: Arc<AtomicU64>,
    bytes: u64,
}

impl CpuByteLease for CappedCpuLease {
    fn category(&self) -> CpuLedgerCategory {
        self.inner.category()
    }

    fn reserved_bytes(&self) -> u64 {
        self.inner.reserved_bytes()
    }
}

impl Drop for CappedCpuLease {
    fn drop(&mut self) {
        self.used_bytes.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

impl CpuByteLedger for CappedCpuLedger<'_> {
    fn try_acquire(
        &self,
        category: CpuLedgerCategory,
        bytes: u64,
    ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
        if bytes == 0 {
            return Err(CpuLedgerError::ZeroByteReservation);
        }
        let mut used = self.used_bytes.load(Ordering::Acquire);
        loop {
            let Some(next) = used.checked_add(bytes) else {
                self.capacity_refusals.fetch_add(1, Ordering::Relaxed);
                return Err(CpuLedgerError::CapacityExceeded {
                    category,
                    requested_bytes: bytes,
                    available_bytes: self.transient_capacity_bytes.saturating_sub(used),
                });
            };
            if next > self.transient_capacity_bytes {
                self.capacity_refusals.fetch_add(1, Ordering::Relaxed);
                return Err(CpuLedgerError::CapacityExceeded {
                    category,
                    requested_bytes: bytes,
                    available_bytes: self.transient_capacity_bytes.saturating_sub(used),
                });
            }
            match self.used_bytes.compare_exchange_weak(
                used,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => used = actual,
            }
        }
        let inner = match self.inner.try_acquire(category, bytes) {
            Ok(lease) => lease,
            Err(error) => {
                self.used_bytes.fetch_sub(bytes, Ordering::AcqRel);
                if matches!(error, CpuLedgerError::CapacityExceeded { .. }) {
                    self.capacity_refusals.fetch_add(1, Ordering::Relaxed);
                }
                return Err(error);
            }
        };
        Ok(Box::new(CappedCpuLease {
            inner,
            used_bytes: Arc::clone(&self.used_bytes),
            bytes,
        }))
    }

    fn capacity_epoch(&self) -> u64 {
        self.inner.capacity_epoch()
    }

    fn capacity_bytes(&self) -> u64 {
        self.reported_capacity_bytes
    }
}

pub(crate) fn run(
    options: ImportOptions,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    mut progress: impl FnMut(ImportEvent),
) -> Result<PublishedImport, ImportError> {
    let required_headroom = Cell::new(1_u64);
    match run_inner(
        options.clone(),
        ledger,
        cancellation,
        &required_headroom,
        &mut progress,
    ) {
        Err(error) if is_safe_storage_full_error(&error) => Err(ImportError::CapacityPaused {
            required_bytes: required_headroom.get(),
            available_bytes: available_space(&options).unwrap_or(0),
        }),
        result => result,
    }
}

fn run_inner(
    options: ImportOptions,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    required_headroom: &Cell<u64>,
    mut progress: impl FnMut(ImportEvent),
) -> Result<PublishedImport, ImportError> {
    let tracked_ledger = TrackingCpuLedger::new(ledger);
    let ledger: &dyn CpuByteLedger = &tracked_ledger;
    let primary_clock = PrimaryClock::start()?;
    let file_descriptor_monitor = ImportFileDescriptorMonitor::start(
        options.inspection.source.primary_path(),
        &options.checkpoint_directory,
        &options.destination,
    )?;
    let mut statistics = ImportStatistics::default();

    let planning_clock =
        StageClock::start(ImportStage::PlanningAndPreflight, 0, None, &mut progress)?;
    check_cancelled(cancellation)?;
    let conservative = ImportPlan::new(&options)?;
    required_headroom.set(conservative.start_required_headroom_bytes);
    statistics.logical_output_bytes = conservative.logical_output_bytes;
    statistics.preflight_required_headroom_bytes = conservative.start_required_headroom_bytes;
    statistics.open_file_descriptor_structural_bound = IMPORT_OPEN_FILE_DESCRIPTOR_STRUCTURAL_BOUND;
    validate_path_separation(&options)?;
    require_absent_destination(&options.destination)?;
    if !checkpoint_path_exists(&options.checkpoint_directory)? {
        preflight_free_space(&options, &conservative)?;
    }
    finish_stage(planning_clock, &options, &mut statistics, &mut progress)?;

    let _source_index_lease = ledger.try_acquire(
        CpuLedgerCategory::ImportWorkingSet,
        conservative.source_index_working_bytes,
    )?;
    statistics.peak_working_bytes = conservative.source_index_working_bytes;
    let revalidation_clock = StageClock::start(
        ImportStage::SourceRevalidation { pass: 1 },
        0,
        None,
        &mut progress,
    )?;
    let source_generation = crate::source::capture_generation(&options.inspection, cancellation)?;
    finish_stage(revalidation_clock, &options, &mut statistics, &mut progress)?;

    let checkpoint_clock =
        StageClock::start(ImportStage::CheckpointOpenOrResume, 0, None, &mut progress)?;
    let mut stage = ResumableLocalPackageStage::open_or_create(
        &options.destination,
        &options.checkpoint_directory,
        conservative.plan_digest,
    )?;
    let mut initial_cache = None;
    let mut detected_mask_lease = None;
    let resolved = match load_no_data_policy(&options.checkpoint_directory, &options)? {
        Some(policy) => policy,
        None => {
            required_headroom.set(recheck_unit_space(&options, &conservative, &stage, 0)?);
            let mut cache = open_unit_cache(&options, &conservative, 0, 0, 0)?;
            decode_unit_if_needed(
                &options,
                &conservative,
                &source_generation,
                0,
                0,
                &mut cache,
                ledger,
                cancellation,
                system_parallelism(),
                &mut progress,
                &mut statistics,
            )?;
            let detection_clock =
                StageClock::start(ImportStage::NoDataDetection, 0, None, &mut progress)?;
            let reader = cache.reader()?;
            let detection = crate::source::resolve_no_data_policy_from_cache(
                &options.inspection,
                &reader,
                &control_directory(&options.checkpoint_directory),
                options.no_data,
                ledger,
                cancellation,
                |completed, total| {
                    if let Some(total_work_units) = total {
                        progress(ImportEvent::StageProgress {
                            stage: ImportStage::NoDataDetection,
                            completed_work_units: completed,
                            total_work_units,
                        });
                    }
                },
            )?;
            statistics.peak_working_bytes = statistics.peak_working_bytes.max(
                conservative
                    .source_index_working_bytes
                    .checked_add(detection.peak_transient_bytes)
                    .and_then(|value| {
                        value.checked_add(detection.policy.automatic_mask_resident_bytes())
                    })
                    .ok_or(ImportError::Overflow)?,
            );
            finish_stage(detection_clock, &options, &mut statistics, &mut progress)?;
            record_no_data_detection_report(&mut statistics, &detection)?;
            store_no_data_policy(&options.checkpoint_directory, &options, &detection.policy)?;
            detected_mask_lease = detection.resident_mask_lease;
            initial_cache = Some(cache);
            detection.policy
        }
    };
    let no_data = Arc::new(resolved);
    let _no_data_mask_lease = if let Some(lease) = detected_mask_lease {
        Some(lease)
    } else if no_data.automatic_mask_resident_bytes() == 0 {
        None
    } else {
        Some(ledger.try_acquire(
            CpuLedgerCategory::ImportWorkingSet,
            no_data.automatic_mask_resident_bytes(),
        )?)
    };
    let plan = ImportPlan::new_resolved(&options, &no_data)?;
    if plan.start_required_headroom_bytes > conservative.start_required_headroom_bytes
        || plan.maximum_unit_output_upper_bound > conservative.maximum_unit_output_upper_bound
        || plan.bounded_unit_scratch_bytes > conservative.bounded_unit_scratch_bytes
        || plan.finalization_headroom_bytes > conservative.finalization_headroom_bytes
    {
        return Err(ImportError::InvalidRequest(
            "resolved no-data placement plan exceeded conservative preflight",
        ));
    }
    statistics.logical_output_bytes = plan.logical_output_bytes;
    let _spool_record_lease =
        ledger.try_acquire(CpuLedgerCategory::ImportWorkingSet, plan.spool_record_bytes)?;
    statistics.peak_working_bytes = statistics
        .peak_working_bytes
        .max(plan.resident_working_bytes);
    let mut units = UnitJournal::open_or_create(
        &options.checkpoint_directory,
        plan.plan_digest,
        options.inspection.shape.t(),
        options.inspection.channels,
    )?;
    remove_completed_unit_scratch(&options.checkpoint_directory, units.completed_units())?;
    let packed = PackedRecordStore::open_or_create(&options.checkpoint_directory, plan.work_units)?;
    validate_stage_prefix(
        &options,
        &plan,
        units.completed_units(),
        stage.durable_shard_inputs(),
    )?;
    finish_stage(checkpoint_clock, &options, &mut statistics, &mut progress)?;

    let descriptors = scientific_descriptors(&options)?;
    let mut scientific = restore_scientific_hashers(&descriptors, &units)?;
    let total_units = options
        .inspection
        .shape
        .t()
        .checked_mul(u64::from(options.inspection.channels))
        .ok_or(ImportError::Overflow)?;
    if units.completed_units() < total_units {
        required_headroom.set(recheck_unit_space(
            &options,
            &plan,
            &stage,
            units.completed_units(),
        )?);
    } else {
        required_headroom.set(recheck_finalization_space(&options, &plan, &stage)?);
    }
    let completed_units = units.completed_units();
    let mut pipeline_status = TemporalPipelineStatus::current_only();
    let mut prepared_unit: Option<PreparedTemporalUnit> = None;
    if completed_units < total_units {
        statistics.maximum_temporal_pipeline_width = 1;
    }
    emit_storage_progress(
        &options,
        &plan,
        &stage,
        completed_units,
        total_units,
        None,
        &pipeline_status,
        &mut progress,
    )?;
    for ordinal in completed_units..total_units {
        check_cancelled(cancellation)?;
        let timepoint = ordinal / u64::from(options.inspection.channels);
        let channel = u32::try_from(ordinal % u64::from(options.inspection.channels))
            .map_err(|_| ImportError::Overflow)?;
        required_headroom.set(recheck_unit_space(&options, &plan, &stage, ordinal)?);
        let mut cache = if let Some(prepared) = prepared_unit.take() {
            if prepared.ordinal != ordinal
                || prepared.timepoint != timepoint
                || prepared.channel != channel
            {
                return Err(ImportError::InvalidCheckpoint(
                    "decode-ahead cache escaped canonical temporal order".to_owned(),
                ));
            }
            statistics.prefetch_units_consumed = statistics
                .prefetch_units_consumed
                .checked_add(1)
                .ok_or(ImportError::Overflow)?;
            statistics.prefetch_cache_hits = statistics
                .prefetch_cache_hits
                .checked_add(1)
                .ok_or(ImportError::Overflow)?;
            prepared.cache
        } else if ordinal == 0 {
            initial_cache.take().unwrap_or(open_unit_cache(
                &options,
                &conservative,
                ordinal,
                channel,
                timepoint,
            )?)
        } else {
            open_unit_cache(&options, &conservative, ordinal, channel, timepoint)?
        };
        pipeline_status = TemporalPipelineStatus::current_only();
        emit_storage_progress(
            &options,
            &plan,
            &stage,
            ordinal,
            total_units,
            Some((ordinal, timepoint, channel)),
            &pipeline_status,
            &mut progress,
        )?;
        decode_unit_if_needed(
            &options,
            &conservative,
            &source_generation,
            channel,
            timepoint,
            &mut cache,
            ledger,
            cancellation,
            system_parallelism(),
            &mut progress,
            &mut statistics,
        )?;
        emit_storage_progress(
            &options,
            &plan,
            &stage,
            ordinal,
            total_units,
            Some((ordinal, timepoint, channel)),
            &pipeline_status,
            &mut progress,
        )?;

        let next_ordinal = ordinal.checked_add(1).ok_or(ImportError::Overflow)?;
        let prefetch = if next_ordinal < total_units {
            admit_decode_ahead(
                &options,
                &conservative,
                &plan,
                &stage,
                ordinal,
                next_ordinal,
                ledger,
                &mut statistics,
            )?
        } else {
            None
        };
        if let Some((future, transient_capacity_bytes)) = prefetch {
            let completed_planes = Arc::new(AtomicU64::new(future.cache.durable_planes()));
            pipeline_status = TemporalPipelineStatus::preparing(
                future.ordinal,
                future.timepoint,
                future.channel,
                Arc::clone(&completed_planes),
                future.cache.total_planes(),
            );
            statistics.prefetch_units_admitted = statistics
                .prefetch_units_admitted
                .checked_add(1)
                .ok_or(ImportError::Overflow)?;
            statistics.maximum_temporal_pipeline_width = statistics
                .maximum_temporal_pipeline_width
                .max(u64::from(pipeline_status.width));
            emit_storage_progress(
                &options,
                &plan,
                &stage,
                ordinal,
                total_units,
                Some((ordinal, timepoint, channel)),
                &pipeline_status,
                &mut progress,
            )?;

            let worker_cancellation = cancellation.child();
            let worker_stop = worker_cancellation.clone();
            let canonical_started = Instant::now();
            let historical_peak_working_bytes = statistics.peak_working_bytes;
            statistics.peak_working_bytes = plan.resident_working_bytes;
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            let worker_inspection = &options.inspection;
            let worker_generation = &source_generation;
            let worker_resident_working_bytes = plan.resident_working_bytes;
            let worker_maximum_decoded_chunk_bytes = options.inspection.maximum_decoded_chunk_bytes;
            let prefetch_capacity_refusals = Arc::new(AtomicU64::new(0));
            let (canonical_result, canonical_processing_time_ns) = thread::scope(|scope| {
                let capped = CappedCpuLedger::new(
                    ledger,
                    transient_capacity_bytes,
                    worker_resident_working_bytes,
                    Arc::clone(&prefetch_capacity_refusals),
                )?;
                let worker = thread::Builder::new()
                    .name("mirante4d-import-decode-ahead".to_owned())
                    .spawn_scoped(scope, move || {
                        let mut future = future;
                        let started = (!future.cache.is_complete()).then(Instant::now);
                        let result = if future.cache.is_complete() {
                            Ok(None)
                        } else {
                            crate::source::decode_canonical_volume_into_cache(
                                worker_inspection,
                                worker_generation,
                                future.channel,
                                future.timepoint,
                                &mut future.cache,
                                worker_maximum_decoded_chunk_bytes,
                                capped.capacity_bytes(),
                                worker_resident_working_bytes,
                                &capped,
                                &worker_cancellation,
                                1,
                                |completed, _| {
                                    completed_planes.store(completed, Ordering::Release);
                                },
                            )
                            .map(Some)
                        };
                        let ingest_busy_time_ns = started
                            .and_then(|started| duration_ns(started.elapsed()).ok())
                            .unwrap_or(0);
                        let _ = sender.send(PrefetchDecodeResult {
                            unit: future,
                            result,
                            ingest_busy_time_ns,
                        });
                    })
                    .map_err(|_| {
                        ImportError::InvalidRequest("failed to start temporal decode-ahead worker")
                    })?;

                let worker_parallelism_limit = system_parallelism().saturating_sub(1).max(1);
                let owner_result = process_canonical_unit(
                    &options,
                    &plan,
                    &no_data,
                    &descriptors,
                    ordinal,
                    timepoint,
                    channel,
                    cache,
                    &packed,
                    &mut stage,
                    &mut units,
                    &mut scientific,
                    total_units,
                    ledger,
                    cancellation,
                    required_headroom,
                    worker_parallelism_limit,
                    &pipeline_status,
                    &mut progress,
                    &mut statistics,
                );
                let canonical_processing_time_ns = duration_ns(canonical_started.elapsed())?;
                if owner_result.is_err() {
                    worker_stop.cancel();
                }

                let mut worker_result = match receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
                };
                if worker_result.is_none()
                    && prefetch_capacity_refusals.load(Ordering::Acquire) != 0
                {
                    // Optional ingest lost access to surplus bytes while the
                    // current unit ran. Stop it now so the next ordinal can
                    // use the protected serial progress lane immediately.
                    worker_stop.cancel();
                }
                while worker_result.is_none() {
                    match receiver.recv_timeout(std::time::Duration::from_millis(50)) {
                        Ok(result) => worker_result = Some(result),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            if cancellation.is_cancelled() {
                                worker_stop.cancel();
                            }
                            emit_storage_progress(
                                &options,
                                &plan,
                                &stage,
                                next_ordinal,
                                total_units,
                                None,
                                &pipeline_status,
                                &mut progress,
                            )?;
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                let joined = worker.join();
                if let Err(error) = owner_result {
                    drop(worker_result);
                    remove_unconsumed_unit_cache_directory(&options, next_ordinal);
                    return Err(error);
                }
                if cancellation.is_cancelled() {
                    return Err(ImportError::Cancelled);
                }
                if joined.is_err() {
                    return Err(ImportError::InvalidRequest(
                        "temporal decode-ahead worker panicked",
                    ));
                }
                Ok((
                    worker_result.ok_or(ImportError::InvalidRequest(
                        "temporal decode-ahead worker stopped without a result",
                    ))?,
                    canonical_processing_time_ns,
                ))
            })?;
            let current_unit_peak_working_bytes = statistics.peak_working_bytes;
            statistics.peak_working_bytes =
                historical_peak_working_bytes.max(current_unit_peak_working_bytes);
            statistics.temporal_canonical_processing_time_ns = statistics
                .temporal_canonical_processing_time_ns
                .checked_add(canonical_processing_time_ns)
                .ok_or(ImportError::Overflow)?;

            statistics.prefetch_ingest_busy_time_ns = statistics
                .prefetch_ingest_busy_time_ns
                .checked_add(canonical_result.ingest_busy_time_ns)
                .ok_or(ImportError::Overflow)?;
            statistics.temporal_ingest_busy_time_ns = statistics
                .temporal_ingest_busy_time_ns
                .checked_add(canonical_result.ingest_busy_time_ns)
                .ok_or(ImportError::Overflow)?;
            statistics.prefetch_overlap_time_ns = statistics
                .prefetch_overlap_time_ns
                .checked_add(canonical_processing_time_ns.min(canonical_result.ingest_busy_time_ns))
                .ok_or(ImportError::Overflow)?;
            match canonical_result.result {
                Ok(report) => {
                    if let Some(report) = report {
                        let concurrent_peak = current_unit_peak_working_bytes
                            .checked_add(report.peak_transient_bytes)
                            .ok_or(ImportError::Overflow)?;
                        record_source_report(&mut statistics, plan.resident_working_bytes, report)?;
                        statistics.peak_working_bytes =
                            statistics.peak_working_bytes.max(concurrent_peak);
                    }
                    pipeline_status =
                        TemporalPipelineStatus::prepared(canonical_result.unit.ordinal);
                    prepared_unit = Some(canonical_result.unit);
                }
                Err(error) if is_optional_prefetch_capacity_error(&error) => {
                    statistics.prefetch_cpu_capacity_deferrals = statistics
                        .prefetch_cpu_capacity_deferrals
                        .checked_add(1)
                        .ok_or(ImportError::Overflow)?;
                    pipeline_status = TemporalPipelineStatus::current_only();
                }
                Err(ImportError::Cancelled) if !cancellation.is_cancelled() => {
                    statistics.prefetch_cpu_capacity_deferrals = statistics
                        .prefetch_cpu_capacity_deferrals
                        .checked_add(1)
                        .ok_or(ImportError::Overflow)?;
                    pipeline_status = TemporalPipelineStatus::current_only();
                }
                Err(error) if is_safe_storage_full_error(&error) => {
                    cleanup_unit_cache(
                        canonical_result.unit.cache,
                        &options.checkpoint_directory,
                        canonical_result.unit.ordinal,
                    );
                    statistics.prefetch_disk_headroom_deferrals = statistics
                        .prefetch_disk_headroom_deferrals
                        .checked_add(1)
                        .ok_or(ImportError::Overflow)?;
                    pipeline_status = TemporalPipelineStatus::current_only();
                }
                Err(error) => {
                    cleanup_unit_cache(
                        canonical_result.unit.cache,
                        &options.checkpoint_directory,
                        canonical_result.unit.ordinal,
                    );
                    return Err(error);
                }
            }
            emit_storage_progress(
                &options,
                &plan,
                &stage,
                next_ordinal,
                total_units,
                None,
                &pipeline_status,
                &mut progress,
            )?;
        } else {
            let canonical_started = Instant::now();
            process_canonical_unit(
                &options,
                &plan,
                &no_data,
                &descriptors,
                ordinal,
                timepoint,
                channel,
                cache,
                &packed,
                &mut stage,
                &mut units,
                &mut scientific,
                total_units,
                ledger,
                cancellation,
                required_headroom,
                system_parallelism(),
                &pipeline_status,
                &mut progress,
                &mut statistics,
            )?;
            statistics.temporal_canonical_processing_time_ns = statistics
                .temporal_canonical_processing_time_ns
                .checked_add(duration_ns(canonical_started.elapsed())?)
                .ok_or(ImportError::Overflow)?;
        }
    }

    let finalization_required_headroom = recheck_finalization_space(&options, &plan, &stage)?;
    required_headroom.set(finalization_required_headroom);
    preserve_capacity_race(
        append_packed_shards(&options, &plan, &packed, &mut stage, cancellation),
        &options,
        finalization_required_headroom,
    )?;
    preserve_capacity_race(
        stage
            .commit_pending(|| cancellation.is_cancelled())
            .map_err(ImportError::from),
        &options,
        finalization_required_headroom,
    )?;
    emit_storage_progress(
        &options,
        &plan,
        &stage,
        total_units,
        total_units,
        None,
        &TemporalPipelineStatus::default(),
        &mut progress,
    )?;
    let decoded_source_sha256 = decoded_source_digest(&options, &units)?;
    let scientific_content_id = finalize_scientific(scientific)?;
    drop(units);
    packed.remove();

    let revalidation_clock = StageClock::start(
        ImportStage::SourceRevalidation { pass: 2 },
        0,
        None,
        &mut progress,
    )?;
    let revalidation = crate::source::revalidate(&options.inspection, cancellation)?;
    if revalidation.generation != source_generation {
        return Err(ImportError::SourceChanged(
            options.inspection.source.primary_path().to_path_buf(),
        ));
    }
    statistics.source_revalidation_bytes_read = revalidation.source_bytes_read;
    statistics.source_bytes_read = statistics
        .source_bytes_read
        .checked_add(revalidation.source_bytes_read)
        .ok_or(ImportError::Overflow)?;
    statistics.tiff_open_count = statistics
        .tiff_open_count
        .checked_add(revalidation.tiff_open_count)
        .ok_or(ImportError::Overflow)?;
    finish_stage(revalidation_clock, &options, &mut statistics, &mut progress)?;

    let metadata = build_package_metadata(&PackageMetadataInput {
        profile_kind: options.profile,
        scientific_content_id,
        base_shape: options.inspection.shape,
        channel_count: options.inspection.channels,
        channel_labels: options.inspection.channel_labels.clone(),
        dtype: options.inspection.dtype,
        pyramid_shapes: plan.shapes.clone(),
        spacing_zyx_um: options.calibration.spacing_zyx_um,
        regular_time_step_seconds: options.time_step_seconds,
        explicit_validity: plan.explicit_validity,
        decoded_source_sha256,
        no_data: (*no_data).clone(),
    })?;
    let finalization_required_headroom = recheck_finalization_space(&options, &plan, &stage)?;
    required_headroom.set(finalization_required_headroom);
    #[cfg(test)]
    maybe_inject_finalization_fault(&options.destination)?;
    let published = preserve_capacity_race(
        finalize_stage(
            stage,
            metadata,
            cancellation,
            &mut statistics,
            &mut progress,
        ),
        &options,
        finalization_required_headroom,
    )?;
    if sample_process_resources(&mut statistics, &[&options.destination]).is_err() {
        statistics.peak_process_rss_bytes = u64::MAX;
        statistics.peak_temporary_bytes = u64::MAX;
    }
    let receipt = published.receipt();
    if let Some(validation) = receipt.scientific_validation_report() {
        statistics.scientific_brick_reads = validation.brick_reads();
        statistics.scientific_payload_object_reads = validation.object_reads();
        statistics.scientific_range_requests = validation.range_requests();
        statistics.scientific_encoded_bytes_read = validation.encoded_bytes_read();
        statistics.scientific_decoded_bytes = validation.decoded_bytes();
    }
    let reads = receipt.validation_read_report();
    statistics.staged_structure_object_reads = reads.structure_object_reads();
    statistics.staged_exact_object_reads = reads.exact_object_reads();
    statistics.scientific_object_reads = reads.scientific_object_reads();
    statistics.object_reads = reads.total_object_reads().unwrap_or(u64::MAX);
    let codecs = receipt.codec_report();
    statistics.codec_encode_calls = statistics
        .codec_encode_calls
        .saturating_add(codecs.encode_calls());
    statistics.codec_encode_time_ns = statistics
        .codec_encode_time_ns
        .saturating_add(codecs.encode_time_ns());
    statistics.codec_decode_calls = statistics
        .codec_decode_calls
        .saturating_add(codecs.decode_calls());
    statistics.codec_decode_time_ns = statistics
        .codec_decode_time_ns
        .saturating_add(codecs.decode_time_ns());
    statistics.sync_calls = statistics.sync_calls.saturating_add(receipt.sync_calls());
    statistics.sync_time_ns = statistics
        .sync_time_ns
        .saturating_add(receipt.sync_time_ns());
    statistics.sampled_peak_open_file_descriptors =
        file_descriptor_monitor.finish_after_publication();
    statistics.peak_open_file_descriptors =
        conservative_file_descriptor_peak(statistics.sampled_peak_open_file_descriptors);
    statistics.peak_working_bytes = tracked_ledger.peak_bytes();
    primary_clock.finish_after_publication(&mut statistics);
    let package_id = published.package_id();
    let published_scientific_content_id = published.scientific_content_id();
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

fn open_unit_cache(
    options: &ImportOptions,
    conservative: &ImportPlan,
    ordinal: u64,
    channel: u32,
    timepoint: u64,
) -> Result<CanonicalBaseCache, ImportError> {
    let directory = unit_cache_directory(&options.checkpoint_directory, ordinal);
    create_private_directory(&directory)?;
    CanonicalBaseCache::open_or_create(
        &directory,
        CanonicalCacheBinding::new(
            unit_binding(conservative.plan_digest, channel, timepoint, b"canonical"),
            options.inspection.source_fingerprint,
        ),
        Shape4D::new(
            1,
            options.inspection.shape.z(),
            options.inspection.shape.y(),
            options.inspection.shape.x(),
        )
        .map_err(|_| ImportError::Overflow)?,
        1,
        options.inspection.dtype,
    )
}

#[allow(clippy::too_many_arguments)]
fn admit_decode_ahead(
    options: &ImportOptions,
    conservative: &ImportPlan,
    plan: &ImportPlan,
    stage: &ResumableLocalPackageStage,
    current_ordinal: u64,
    future_ordinal: u64,
    ledger: &dyn CpuByteLedger,
    statistics: &mut ImportStatistics,
) -> Result<Option<(PreparedTemporalUnit, u64)>, ImportError> {
    if !temporal_decode_ahead_enabled() {
        return Ok(None);
    }
    if system_parallelism() <= TEMPORAL_DECODE_AHEAD_LANES {
        statistics.prefetch_cpu_capacity_deferrals = statistics
            .prefetch_cpu_capacity_deferrals
            .checked_add(1)
            .ok_or(ImportError::Overflow)?;
        return Ok(None);
    }
    let transient_capacity_bytes =
        crate::source::canonical_volume_decode_transient_bytes(&options.inspection)?;
    let surplus = ledger
        .capacity_bytes()
        .saturating_sub(plan.minimum_execution_bytes);
    if transient_capacity_bytes > surplus {
        statistics.prefetch_cpu_capacity_deferrals = statistics
            .prefetch_cpu_capacity_deferrals
            .checked_add(1)
            .ok_or(ImportError::Overflow)?;
        return Ok(None);
    }

    let future_directory = unit_cache_directory(&options.checkpoint_directory, future_ordinal);
    let existing_cache_bytes = owned_regular_file_bytes(&future_directory)?;
    let mandatory_current = unit_additional_headroom(options, plan, stage, current_ordinal)?;
    if !optional_cache_headroom_fits(
        available_space(options)?,
        mandatory_current,
        plan.canonical_unit_cache_bytes,
        existing_cache_bytes,
    )? {
        statistics.prefetch_disk_headroom_deferrals = statistics
            .prefetch_disk_headroom_deferrals
            .checked_add(1)
            .ok_or(ImportError::Overflow)?;
        return Ok(None);
    }

    let timepoint = future_ordinal / u64::from(options.inspection.channels);
    let channel = u32::try_from(future_ordinal % u64::from(options.inspection.channels))
        .map_err(|_| ImportError::Overflow)?;
    match open_unit_cache(options, conservative, future_ordinal, channel, timepoint) {
        Ok(cache) => Ok(Some((
            PreparedTemporalUnit {
                ordinal: future_ordinal,
                timepoint,
                channel,
                cache,
            },
            transient_capacity_bytes,
        ))),
        Err(error) if is_safe_storage_full_error(&error) => {
            remove_unconsumed_unit_cache_directory(options, future_ordinal);
            statistics.prefetch_disk_headroom_deferrals = statistics
                .prefetch_disk_headroom_deferrals
                .checked_add(1)
                .ok_or(ImportError::Overflow)?;
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn optional_cache_headroom_fits(
    available_bytes: u64,
    mandatory_current_bytes: u64,
    cache_ceiling_bytes: u64,
    existing_cache_bytes: u64,
) -> Result<bool, ImportError> {
    let missing_cache_bytes = cache_ceiling_bytes
        .checked_sub(existing_cache_bytes)
        .ok_or(ImportError::InvalidCheckpoint(
            "decode-ahead canonical cache exceeded its placement bound".to_owned(),
        ))?;
    let required = mandatory_current_bytes
        .checked_add(missing_cache_bytes)
        .ok_or(ImportError::Overflow)?;
    Ok(available_bytes >= required)
}

fn is_optional_prefetch_capacity_error(error: &ImportError) -> bool {
    matches!(
        error,
        ImportError::ManagedCapacityInsufficient { .. }
            | ImportError::Ledger(CpuLedgerError::CapacityExceeded { .. })
    )
}

fn duration_ns(duration: std::time::Duration) -> Result<u64, ImportError> {
    u64::try_from(duration.as_nanos()).map_err(|_| ImportError::Overflow)
}

#[allow(clippy::too_many_arguments)]
fn decode_unit_if_needed(
    options: &ImportOptions,
    plan: &ImportPlan,
    generation: &crate::source::SourceGeneration,
    channel: u32,
    timepoint: u64,
    cache: &mut CanonicalBaseCache,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    worker_parallelism_limit: usize,
    progress: &mut impl FnMut(ImportEvent),
    statistics: &mut ImportStatistics,
) -> Result<(), ImportError> {
    let clock = StageClock::start(
        ImportStage::SourceIngest,
        cache.durable_planes(),
        Some(cache.total_planes()),
        progress,
    )?;
    let ingest_started = (!cache.is_complete()).then(Instant::now);
    if !cache.is_complete() {
        let report = crate::source::decode_canonical_volume_into_cache(
            &options.inspection,
            generation,
            channel,
            timepoint,
            cache,
            options.inspection.maximum_decoded_chunk_bytes,
            ledger.capacity_bytes(),
            plan.resident_working_bytes,
            ledger,
            cancellation,
            worker_parallelism_limit,
            |completed, total| {
                progress(ImportEvent::StageProgress {
                    stage: ImportStage::SourceIngest,
                    completed_work_units: completed,
                    total_work_units: total,
                });
            },
        )?;
        record_source_report(statistics, plan.resident_working_bytes, report)?;
    }
    if let Some(started) = ingest_started {
        statistics.temporal_ingest_busy_time_ns = statistics
            .temporal_ingest_busy_time_ns
            .checked_add(duration_ns(started.elapsed())?)
            .ok_or(ImportError::Overflow)?;
    }
    finish_stage(clock, options, statistics, progress)
}

/// The sole canonical temporal-unit owner. Decode-ahead may prepare the cache
/// supplied here, but scientific identity, spooling, package writes,
/// durability, and the unit journal remain in this exact ordered operation.
#[allow(clippy::too_many_arguments)]
fn process_canonical_unit(
    options: &ImportOptions,
    plan: &ImportPlan,
    no_data: &Arc<ResolvedNoDataPolicy>,
    descriptors: &[ScientificLayerDescriptor],
    ordinal: u64,
    timepoint: u64,
    channel: u32,
    mut cache: CanonicalBaseCache,
    packed: &PackedRecordStore,
    stage: &mut ResumableLocalPackageStage,
    units: &mut UnitJournal,
    scientific: &mut [ScientificLayerHasher],
    total_units: u64,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    required_headroom: &Cell<u64>,
    worker_parallelism_limit: usize,
    pipeline_status: &TemporalPipelineStatus,
    progress: &mut impl FnMut(ImportEvent),
    statistics: &mut ImportStatistics,
) -> Result<(), ImportError> {
    let decoded_digest = unit_decoded_digest(channel, timepoint, cache.decoded_source_digest()?);
    let channel_index = usize::try_from(channel).map_err(|_| ImportError::Overflow)?;
    hash_scientific_unit(
        options,
        plan,
        no_data,
        timepoint,
        &descriptors[channel_index],
        &mut cache,
        &mut scientific[channel_index],
        ledger,
        cancellation,
        worker_parallelism_limit,
        statistics,
    )?;

    let spool_directory = unit_spool_directory(&options.checkpoint_directory, ordinal);
    create_private_directory(&spool_directory)?;
    let unit_binding = SpoolBinding::new(
        unit_binding(plan.plan_digest, channel, timepoint, b"spool"),
        options.inspection.source_fingerprint,
    );
    let mut spool =
        ImportSpool::open_or_create(&spool_directory, unit_binding, plan.unit_work_units, || {
            cancellation.is_cancelled()
        })?;
    validate_unit_spool_prefix(&spool, plan, timepoint, channel)?;
    produce_base_unit(
        options,
        plan,
        no_data,
        timepoint,
        channel,
        &mut cache,
        &mut spool,
        ledger,
        cancellation,
        worker_parallelism_limit,
        progress,
        statistics,
    )?;
    produce_coarse_unit(
        options,
        plan,
        no_data,
        timepoint,
        channel,
        &mut spool,
        ledger,
        cancellation,
        worker_parallelism_limit,
        progress,
        statistics,
    )?;
    spool.commit_pending()?;
    if u64::try_from(spool.len()).map_err(|_| ImportError::Overflow)? != plan.unit_work_units {
        return Err(ImportError::InvalidCheckpoint(
            "temporal-unit spool is incomplete after production".to_owned(),
        ));
    }
    persist_unit_packed_records(options, plan, timepoint, channel, &spool, packed)?;
    packed.sync()?;
    emit_storage_progress(
        options,
        plan,
        stage,
        ordinal,
        total_units,
        Some((ordinal, timepoint, channel)),
        pipeline_status,
        progress,
    )?;
    let unit_required_headroom = recheck_unit_space(options, plan, stage, ordinal)?;
    required_headroom.set(unit_required_headroom);
    preserve_capacity_race(
        append_unit_shards(
            plan,
            timepoint,
            channel,
            ordinal,
            &mut spool,
            stage,
            cancellation,
        ),
        options,
        unit_required_headroom,
    )?;
    preserve_capacity_race(
        stage
            .commit_pending(|| cancellation.is_cancelled())
            .map_err(ImportError::from),
        options,
        unit_required_headroom,
    )?;
    record_spool_diagnostics(statistics, spool.diagnostics())?;
    units.append(UnitCompletion {
        ordinal,
        timepoint,
        channel,
        decoded_digest,
        scientific_checkpoint: scientific[channel_index].checkpoint_bytes()?,
    })?;
    cleanup_unit_cache(cache, &options.checkpoint_directory, ordinal);
    cleanup_unit_spool(spool);
    emit_storage_progress(
        options,
        plan,
        stage,
        ordinal.checked_add(1).ok_or(ImportError::Overflow)?,
        total_units,
        None,
        pipeline_status,
        progress,
    )
}

fn unit_decoded_digest(channel: u32, timepoint: u64, local: Sha256Digest) -> Sha256Digest {
    let mut hasher = Sha256Hasher::new();
    hasher.update(b"mirante4d-decoded-source-unit-v2\0");
    hasher.update(channel.to_le_bytes());
    hasher.update(timepoint.to_le_bytes());
    hasher.update(local.as_bytes());
    hasher.finalize()
}

fn unit_binding(plan: Sha256Digest, channel: u32, timepoint: u64, domain: &[u8]) -> Sha256Digest {
    let mut hasher = Sha256Hasher::new();
    hasher.update(b"mirante4d-import-temporal-unit-v1\0");
    hasher.update(domain);
    hasher.update(plan.as_bytes());
    hasher.update(channel.to_le_bytes());
    hasher.update(timepoint.to_le_bytes());
    hasher.finalize()
}

fn scientific_descriptors(
    options: &ImportOptions,
) -> Result<Vec<ScientificLayerDescriptor>, ImportError> {
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
    (0..options.inspection.channels)
        .map(|channel| {
            ScientificLayerDescriptor::new(
                LogicalLayerKey::new(channel),
                options.inspection.dtype,
                options.inspection.shape,
                temporal.clone(),
                grid_to_world,
            )
            .map_err(ImportError::from)
        })
        .collect()
}

fn restore_scientific_hashers(
    descriptors: &[ScientificLayerDescriptor],
    journal: &UnitJournal,
) -> Result<Vec<ScientificLayerHasher>, ImportError> {
    descriptors
        .iter()
        .enumerate()
        .map(|(index, descriptor)| {
            let channel = u32::try_from(index).map_err(|_| ImportError::Overflow)?;
            match journal.latest_scientific_checkpoint(channel) {
                Some(checkpoint) => Ok(ScientificLayerHasher::resume_from_checkpoint(
                    descriptor.clone(),
                    checkpoint,
                )?),
                None => Ok(ScientificLayerHasher::new(descriptor.clone())?),
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn hash_scientific_unit(
    options: &ImportOptions,
    plan: &ImportPlan,
    no_data: &Arc<ResolvedNoDataPolicy>,
    timepoint: u64,
    descriptor: &ScientificLayerDescriptor,
    canonical: &mut CanonicalBaseCache,
    layer: &mut ScientificLayerHasher,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    worker_parallelism_limit: usize,
    statistics: &mut ImportStatistics,
) -> Result<(), ImportError> {
    let shape = options.inspection.shape;
    let tiles_per_time = [shape.z(), shape.y(), shape.x()]
        .into_iter()
        .zip(SCIENTIFIC_TILE_SHAPE_TZYX[1..].iter().copied())
        .try_fold(1_u64, |product, (length, tile)| {
            product
                .checked_mul(length.div_ceil(tile))
                .ok_or(ImportError::Overflow)
        })?;
    let start = timepoint
        .checked_mul(tiles_per_time)
        .ok_or(ImportError::Overflow)?;
    if layer.accepted_tile_count() != start {
        return Err(ImportError::InvalidCheckpoint(
            "scientific hash checkpoint is not at the next temporal unit".to_owned(),
        ));
    }
    let reader = Arc::new(canonical.reader()?);
    let task_charge =
        scientific_task_charge_bytes(options.inspection.dtype, no_data.explicit_validity())?;
    let policy = OrderedWorkerPolicy::for_system_with_parallelism_limit(
        worker_parallelism_limit,
        ledger.capacity_bytes(),
        plan.resident_working_bytes,
        task_charge,
    )?;
    let specifications = (0..shape.z())
        .step_by(SCIENTIFIC_TILE_SHAPE_TZYX[1] as usize)
        .flat_map(|z| {
            (0..shape.y())
                .step_by(SCIENTIFIC_TILE_SHAPE_TZYX[2] as usize)
                .flat_map(move |y| {
                    (0..shape.x())
                        .step_by(SCIENTIFIC_TILE_SHAPE_TZYX[3] as usize)
                        .map(move |x| (z, y, x))
                })
        })
        .enumerate();
    let diagnostics = run_ordered(
        policy,
        ledger,
        cancellation,
        specifications,
        layer,
        |_layer, (local, (z, y, x))| {
            let extent = [
                (shape.z() - z).min(SCIENTIFIC_TILE_SHAPE_TZYX[1]),
                (shape.y() - y).min(SCIENTIFIC_TILE_SHAPE_TZYX[2]),
                (shape.x() - x).min(SCIENTIFIC_TILE_SHAPE_TZYX[3]),
            ];
            Ok(ScientificTileCpuTask {
                canonical: Arc::clone(&reader),
                descriptor: descriptor.clone(),
                linear_index: start
                    .checked_add(u64::try_from(local).map_err(|_| ImportError::Overflow)?)
                    .ok_or(ImportError::Overflow)?,
                source_channel: 0,
                source_timepoint: 0,
                origin_tzyx: [timepoint, z, y, x],
                extent_tzyx: [1, extent[0], extent[1], extent[2]],
                source_shape_zyx: [shape.z(), shape.y(), shape.x()],
                no_data: Arc::clone(no_data),
            })
        },
        prepare_scientific_tile,
        |_| Ok(()),
        |layer, prepared| {
            layer.push_prepared_tile(prepared)?;
            Ok(())
        },
    )?;
    record_worker_diagnostics(
        statistics,
        plan.resident_working_bytes,
        task_charge,
        diagnostics,
    )
}

#[allow(clippy::too_many_arguments)]
fn produce_base_unit(
    options: &ImportOptions,
    plan: &ImportPlan,
    no_data: &Arc<ResolvedNoDataPolicy>,
    timepoint: u64,
    channel: u32,
    canonical: &mut CanonicalBaseCache,
    spool: &mut ImportSpool,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    worker_parallelism_limit: usize,
    progress: &mut impl FnMut(ImportEvent),
    statistics: &mut ImportStatistics,
) -> Result<(), ImportError> {
    let shape = plan.shapes[0];
    let total = spatial_bricks(shape, plan.is_2d)?;
    let existing = u64::try_from(spool.len())
        .map_err(|_| ImportError::Overflow)?
        .min(total);
    let clock = StageClock::start(ImportStage::BaseProduction, existing, Some(total), progress)?;
    if existing < total {
        let task_charge = base_task_charge_bytes(
            options.inspection.dtype,
            plan.pixel_kind,
            plan.validity_kind,
            plan.explicit_validity,
        )?;
        let policy = OrderedWorkerPolicy::for_system_with_parallelism_limit(
            worker_parallelism_limit,
            ledger.capacity_bytes(),
            plan.resident_working_bytes,
            task_charge,
        )?;
        let specifications = unit_keys(plan, 0, timepoint, channel)
            .skip(usize::try_from(existing).map_err(|_| ImportError::Overflow)?);
        let reader = Arc::new(canonical.reader()?);
        let inner = chunk_shape(plan.is_2d);
        let mut completed = existing;
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
                    canonical: Arc::clone(&reader),
                    channel: 0,
                    timepoint: 0,
                    origin_zyx: [
                        chunk[0] * inner[0],
                        chunk[1] * inner[1],
                        chunk[2] * inner[2],
                    ],
                    source_shape_zyx: [shape.z(), shape.y(), shape.x()],
                    no_data: Arc::clone(no_data),
                })
            },
            prepare_base_chunk,
            ImportSpool::commit_expired,
            |spool, prepared| {
                append_encoded_prepared(spool, prepared)?;
                completed = completed.checked_add(1).ok_or(ImportError::Overflow)?;
                statistics.produced_work_units = statistics
                    .produced_work_units
                    .checked_add(1)
                    .ok_or(ImportError::Overflow)?;
                progress(ImportEvent::StageProgress {
                    stage: ImportStage::BaseProduction,
                    completed_work_units: completed,
                    total_work_units: total,
                });
                Ok(())
            },
        )?;
        record_worker_diagnostics(
            statistics,
            plan.resident_working_bytes,
            task_charge,
            diagnostics,
        )?;
    }
    statistics.resumed_work_units = statistics
        .resumed_work_units
        .checked_add(existing)
        .ok_or(ImportError::Overflow)?;
    finish_stage(clock, options, statistics, progress)
}

#[allow(clippy::too_many_arguments)]
fn produce_coarse_unit(
    options: &ImportOptions,
    plan: &ImportPlan,
    no_data: &Arc<ResolvedNoDataPolicy>,
    timepoint: u64,
    channel: u32,
    spool: &mut ImportSpool,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    worker_parallelism_limit: usize,
    progress: &mut impl FnMut(ImportEvent),
    statistics: &mut ImportStatistics,
) -> Result<(), ImportError> {
    let inner = chunk_shape(plan.is_2d);
    let mut prior = spatial_bricks(plan.shapes[0], plan.is_2d)?;
    for scale in 1..plan.shapes.len() {
        let stage = ImportStage::PyramidProduction {
            scale: u32::try_from(scale).map_err(|_| ImportError::Overflow)?,
        };
        let total = spatial_bricks(plan.shapes[scale], plan.is_2d)?;
        let existing = u64::try_from(spool.len())
            .map_err(|_| ImportError::Overflow)?
            .saturating_sub(prior)
            .min(total);
        let clock = StageClock::start(stage, existing, Some(total), progress)?;
        if existing < total {
            let task_charge = pyramid_task_charge_bytes(
                options.inspection.dtype,
                plan.pixel_kind,
                plan.validity_kind,
                plan.is_2d,
                plan.explicit_validity,
            )?;
            let policy = OrderedWorkerPolicy::for_system_with_parallelism_limit(
                worker_parallelism_limit,
                ledger.capacity_bytes(),
                plan.resident_working_bytes,
                task_charge,
            )?;
            let specifications = unit_keys(plan, scale, timepoint, channel)
                .skip(usize::try_from(existing).map_err(|_| ImportError::Overflow)?);
            let shape = plan.shapes[scale];
            let previous = plan.shapes[scale - 1];
            let previous_shape = [previous.z(), previous.y(), previous.x()];
            let payload = Arc::new(spool.payload_reader()?);
            let mut completed = existing;
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
                    let mut source_origin = [0; 3];
                    let mut source_extent = [0; 3];
                    for axis in 0..3 {
                        source_origin[axis] = target_origin[axis]
                            .checked_mul(2)
                            .ok_or(ImportError::Overflow)?;
                        source_extent[axis] = target_extent[axis]
                            .checked_mul(2)
                            .ok_or(ImportError::Overflow)?
                            .min(previous_shape[axis] - source_origin[axis]);
                    }
                    let pixel_source_chunks = describe_spooled_region(
                        spool,
                        plan,
                        SpooledRegionRequest {
                            scale: scale - 1,
                            timepoint,
                            channel: u64::from(channel),
                            region: Region3 {
                                origin: source_origin,
                                shape: source_extent,
                            },
                            maximum_chunks: pyramid_pixel_source_chunks_max(plan.is_2d),
                        },
                    )?;
                    let target_level_shape = [shape.z(), shape.y(), shape.x()];
                    let (validity_origin, validity_extent, validity_source_chunks) =
                        if plan.explicit_validity {
                            let target_halo = clipped_halo(
                                Region3 {
                                    origin: target_origin,
                                    shape: target_extent,
                                },
                                target_level_shape,
                                invalid_dilation_radius(target_level_shape),
                            )?;
                            let mut validity_origin = [0; 3];
                            let mut validity_extent = [0; 3];
                            for axis in 0..3 {
                                validity_origin[axis] = target_halo.origin[axis]
                                    .checked_mul(2)
                                    .ok_or(ImportError::Overflow)?;
                                validity_extent[axis] = target_halo.shape[axis]
                                    .checked_mul(2)
                                    .ok_or(ImportError::Overflow)?
                                    .min(previous_shape[axis] - validity_origin[axis]);
                            }
                            let chunks = describe_spooled_region(
                                spool,
                                plan,
                                SpooledRegionRequest {
                                    scale: scale - 1,
                                    timepoint,
                                    channel: u64::from(channel),
                                    region: Region3 {
                                        origin: validity_origin,
                                        shape: validity_extent,
                                    },
                                    maximum_chunks: pyramid_validity_source_chunks_max(plan.is_2d),
                                },
                            )?;
                            (validity_origin, validity_extent, chunks)
                        } else {
                            (source_origin, source_extent, Vec::new())
                        };
                    Ok(PyramidChunkCpuTask {
                        key,
                        dtype: options.inspection.dtype,
                        is_2d: plan.is_2d,
                        payload: Arc::clone(&payload),
                        pixel_source_chunks,
                        validity_source_chunks,
                        source_level_shape_zyx: previous_shape,
                        source_origin_zyx: source_origin,
                        source_shape_zyx: source_extent,
                        validity_origin_zyx: validity_origin,
                        validity_shape_zyx: validity_extent,
                        pixel_kind: plan.pixel_kind,
                        validity_kind: plan.validity_kind,
                        explicit_validity: plan.explicit_validity,
                        target_level_shape_zyx: target_level_shape,
                        target_origin_zyx: target_origin,
                        target_shape_zyx: target_extent,
                        target_scale: u32::try_from(scale).map_err(|_| ImportError::Overflow)?,
                        no_data: Arc::clone(no_data),
                    })
                },
                prepare_pyramid_chunk,
                ImportSpool::commit_expired,
                |spool, prepared| {
                    append_encoded_prepared(spool, prepared)?;
                    completed = completed.checked_add(1).ok_or(ImportError::Overflow)?;
                    statistics.produced_work_units = statistics
                        .produced_work_units
                        .checked_add(1)
                        .ok_or(ImportError::Overflow)?;
                    progress(ImportEvent::StageProgress {
                        stage,
                        completed_work_units: completed,
                        total_work_units: total,
                    });
                    Ok(())
                },
            )?;
            record_worker_diagnostics(
                statistics,
                plan.resident_working_bytes,
                task_charge,
                diagnostics,
            )?;
        }
        statistics.resumed_work_units = statistics
            .resumed_work_units
            .checked_add(existing)
            .ok_or(ImportError::Overflow)?;
        finish_stage(clock, options, statistics, progress)?;
        prior = prior.checked_add(total).ok_or(ImportError::Overflow)?;
    }
    Ok(())
}

struct SpooledRegionRequest {
    scale: usize,
    timepoint: u64,
    channel: u64,
    region: Region3,
    maximum_chunks: usize,
}

fn describe_spooled_region(
    spool: &ImportSpool,
    plan: &ImportPlan,
    request: SpooledRegionRequest,
) -> Result<Vec<PyramidSourceChunk>, ImportError> {
    let SpooledRegionRequest {
        scale,
        timepoint,
        channel,
        region,
        maximum_chunks,
    } = request;
    let inner = chunk_shape(plan.is_2d);
    let start = [
        region.origin[0] / inner[0],
        region.origin[1] / inner[1],
        region.origin[2] / inner[2],
    ];
    let end = [
        (region.origin[0] + region.shape[0] - 1) / inner[0],
        (region.origin[1] + region.shape[1] - 1) / inner[1],
        (region.origin[2] + region.shape[2] - 1) / inner[2],
    ];
    let capacity = (0..3).try_fold(1_u64, |product, axis| {
        product.checked_mul(end[axis] - start[axis] + 1)
    });
    let capacity = usize::try_from(capacity.ok_or(ImportError::Overflow)?)
        .map_err(|_| ImportError::Overflow)?;
    if capacity > maximum_chunks {
        return Err(ImportError::InvalidCheckpoint(
            "coarse source region exceeds its charged descriptor bound".to_owned(),
        ));
    }
    let mut chunks = Vec::with_capacity(capacity);
    for z in start[0]..=end[0] {
        for y in start[1]..=end[1] {
            for x in start[2]..=end[2] {
                let key = work_key(scale, timepoint, channel, [z, y, x])?;
                let descriptor = spool.work_unit_descriptor(key).ok_or_else(|| {
                    ImportError::InvalidCheckpoint(
                        "coarse level depends on a missing unit work item".to_owned(),
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
            "a new unit work item unexpectedly already exists".to_owned(),
        ));
    }
    spool
        .record_worker_codec_decodes(prepared.codec_decode_calls, prepared.codec_decode_time_ns)?;
    Ok(())
}

fn persist_unit_packed_records(
    options: &ImportOptions,
    plan: &ImportPlan,
    timepoint: u64,
    channel: u32,
    spool: &ImportSpool,
    packed: &PackedRecordStore,
) -> Result<(), ImportError> {
    let mut scale_base = 0_u64;
    for (scale, shape) in plan.shapes.iter().copied().enumerate() {
        let grid = chunk_grid([shape.z(), shape.y(), shape.x()], plan.is_2d);
        let spatial = checked_product(grid)?;
        let unit = timepoint
            .checked_mul(u64::from(options.inspection.channels))
            .and_then(|value| value.checked_add(u64::from(channel)))
            .ok_or(ImportError::Overflow)?;
        for z in 0..grid[0] {
            for y in 0..grid[1] {
                for x in 0..grid[2] {
                    let local = (z
                        .checked_mul(grid[1])
                        .and_then(|value| value.checked_add(y))
                        .and_then(|value| value.checked_mul(grid[2]))
                        .and_then(|value| value.checked_add(x)))
                    .ok_or(ImportError::Overflow)?;
                    let ordinal = scale_base
                        .checked_add(unit.checked_mul(spatial).ok_or(ImportError::Overflow)?)
                        .and_then(|value| value.checked_add(local))
                        .ok_or(ImportError::Overflow)?;
                    let bytes = spool
                        .read_packed_index(work_key(
                            scale,
                            timepoint,
                            u64::from(channel),
                            [z, y, x],
                        )?)
                        .ok_or_else(|| {
                            ImportError::InvalidCheckpoint(
                                "unit spool is missing a packed-index record".to_owned(),
                            )
                        })?;
                    packed.write(ordinal, &bytes)?;
                }
            }
        }
        scale_base = scale_base
            .checked_add(plan.logical_bricks_by_scale[scale])
            .ok_or(ImportError::Overflow)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_unit_shards(
    plan: &ImportPlan,
    timepoint: u64,
    channel: u32,
    unit_ordinal: u64,
    spool: &mut ImportSpool,
    stage: &mut ResumableLocalPackageStage,
    cancellation: &ImportCancellation,
) -> Result<(), ImportError> {
    let per_unit = unit_shard_inputs(plan)?;
    let mut input_ordinal = unit_ordinal
        .checked_mul(per_unit)
        .ok_or(ImportError::Overflow)?;
    for (scale, shape) in plan.shapes.iter().copied().enumerate() {
        let paths = level_paths(scale, plan.explicit_validity)?;
        let grid = chunk_grid([shape.z(), shape.y(), shape.x()], plan.is_2d);
        let ratios = if plan.is_2d { [1, 4, 4] } else { [4, 4, 4] };
        let outer = [
            grid[0].div_ceil(ratios[0]),
            grid[1].div_ceil(ratios[1]),
            grid[2].div_ceil(ratios[2]),
        ];
        for validity in [false, true] {
            if validity && !plan.explicit_validity {
                continue;
            }
            for oz in 0..outer[0] {
                for oy in 0..outer[1] {
                    for ox in 0..outer[2] {
                        check_cancelled(cancellation)?;
                        if input_ordinal >= stage.durable_shard_inputs() {
                            let kind = if validity {
                                plan.validity_kind
                            } else {
                                plan.pixel_kind
                            };
                            let mut chunks = std::iter::repeat_with(|| None)
                                .take(kind.chunks_per_shard())
                                .collect::<Vec<_>>();
                            for lz in 0..ratios[0] {
                                for ly in 0..ratios[1] {
                                    for lx in 0..ratios[2] {
                                        let chunk = [
                                            oz * ratios[0] + lz,
                                            oy * ratios[1] + ly,
                                            ox * ratios[2] + lx,
                                        ];
                                        if (0..3).any(|axis| chunk[axis] >= grid[axis]) {
                                            continue;
                                        }
                                        let slot =
                                            usize::try_from((lz * ratios[1] + ly) * ratios[2] + lx)
                                                .map_err(|_| ImportError::Overflow)?;
                                        let component = spool.read_encoded_component(
                                            work_key(scale, timepoint, u64::from(channel), chunk)?,
                                            validity,
                                        )?;
                                        if let Some(component) = component {
                                            if component.kind() != kind {
                                                return Err(ImportError::InvalidCheckpoint(
                                                    "unit component uses the wrong storage kind"
                                                        .to_owned(),
                                                ));
                                            }
                                            chunks[slot] = Some(component);
                                        }
                                    }
                                }
                            }
                            let (path, object_kind) = if validity {
                                (
                                    paths.validity.clone().ok_or_else(|| {
                                        ImportError::InvalidCheckpoint(
                                            "explicit validity level has no path".to_owned(),
                                        )
                                    })?,
                                    PackageObjectKind::ValidityShard,
                                )
                            } else {
                                (paths.pixel.clone(), PackageObjectKind::PixelShard)
                            };
                            stage.append_shard(
                                input_ordinal,
                                PackageShardInput::new_encoded(
                                    path,
                                    vec![timepoint, u64::from(channel), oz, oy, ox],
                                    chunks,
                                ),
                                object_kind,
                                kind,
                                || cancellation.is_cancelled(),
                            )?;
                        }
                        input_ordinal =
                            input_ordinal.checked_add(1).ok_or(ImportError::Overflow)?;
                    }
                }
            }
        }
    }
    let expected = unit_ordinal
        .checked_add(1)
        .and_then(|value| value.checked_mul(per_unit))
        .ok_or(ImportError::Overflow)?;
    if input_ordinal != expected {
        return Err(ImportError::InvalidRequest(
            "unit shard traversal disagrees with its deterministic count",
        ));
    }
    Ok(())
}

fn append_packed_shards(
    options: &ImportOptions,
    plan: &ImportPlan,
    packed: &PackedRecordStore,
    stage: &mut ResumableLocalPackageStage,
    cancellation: &ImportCancellation,
) -> Result<(), ImportError> {
    let total_units = options
        .inspection
        .shape
        .t()
        .checked_mul(u64::from(options.inspection.channels))
        .ok_or(ImportError::Overflow)?;
    let mut input_ordinal = total_units
        .checked_mul(unit_shard_inputs(plan)?)
        .ok_or(ImportError::Overflow)?;
    let mut record_base = 0_u64;
    for (scale, records) in plan.logical_bricks_by_scale.iter().copied().enumerate() {
        let path = level_paths(scale, plan.explicit_validity)?.packed;
        let outer_count = records.div_ceil(16_384);
        for outer in 0..outer_count {
            check_cancelled(cancellation)?;
            if input_ordinal >= stage.durable_shard_inputs() {
                let kind = ShardProfileKind::PackedIndex;
                let mut chunks = std::iter::repeat_with(|| None)
                    .take(kind.chunks_per_shard())
                    .collect::<Vec<_>>();
                for (slot, chunk) in chunks.iter_mut().enumerate() {
                    let start = outer
                        .checked_mul(16_384)
                        .and_then(|value| value.checked_add((slot as u64) * 256))
                        .ok_or(ImportError::Overflow)?;
                    if start >= records {
                        break;
                    }
                    let end = start.saturating_add(256).min(records);
                    let mut decoded = vec![0_u8; kind.decoded_inner_bytes()];
                    for local in start..end {
                        let mut record = [0_u8; PACKED_INDEX_RECORD_BYTES as usize];
                        packed.read(
                            record_base
                                .checked_add(local)
                                .ok_or(ImportError::Overflow)?,
                            &mut record,
                        )?;
                        let offset = usize::try_from(local - start)
                            .map_err(|_| ImportError::Overflow)?
                            .checked_mul(PACKED_INDEX_RECORD_BYTES as usize)
                            .ok_or(ImportError::Overflow)?;
                        decoded[offset..offset + PACKED_INDEX_RECORD_BYTES as usize]
                            .copy_from_slice(&record);
                    }
                    *chunk = Some(decoded);
                }
                stage.append_shard(
                    input_ordinal,
                    PackageShardInput::new(path.clone(), vec![outer, 0], chunks),
                    PackageObjectKind::PackedIndexShard,
                    kind,
                    || cancellation.is_cancelled(),
                )?;
            }
            input_ordinal = input_ordinal.checked_add(1).ok_or(ImportError::Overflow)?;
        }
        record_base = record_base
            .checked_add(records)
            .ok_or(ImportError::Overflow)?;
    }
    if input_ordinal != total_stage_inputs(options, plan)? {
        return Err(ImportError::InvalidRequest(
            "packed shard traversal disagrees with its deterministic count",
        ));
    }
    Ok(())
}

#[derive(Clone)]
struct LevelPaths {
    pixel: mirante4d_storage::PackagePath,
    validity: Option<mirante4d_storage::PackagePath>,
    packed: mirante4d_storage::PackagePath,
}

fn level_paths(scale: usize, explicit_validity: bool) -> Result<LevelPaths, ImportError> {
    let level = ProfileLevel::new(
        0,
        u32::try_from(scale).map_err(|_| ImportError::Overflow)?,
        if explicit_validity {
            ProfileValidityMode::Explicit
        } else {
            ProfileValidityMode::AllValid
        },
    )?;
    Ok(LevelPaths {
        pixel: level.pixel_path().clone(),
        validity: level.validity_path().cloned(),
        packed: level.packed_index_path().clone(),
    })
}

fn unit_shard_inputs(plan: &ImportPlan) -> Result<u64, ImportError> {
    let components = if plan.explicit_validity { 2_u64 } else { 1 };
    plan.shapes.iter().try_fold(0_u64, |total, shape| {
        let grid = chunk_grid([shape.z(), shape.y(), shape.x()], plan.is_2d);
        let ratios = if plan.is_2d { [1, 4, 4] } else { [4, 4, 4] };
        total
            .checked_add(
                checked_product([
                    grid[0].div_ceil(ratios[0]),
                    grid[1].div_ceil(ratios[1]),
                    grid[2].div_ceil(ratios[2]),
                ])?
                .checked_mul(components)
                .ok_or(ImportError::Overflow)?,
            )
            .ok_or(ImportError::Overflow)
    })
}

fn total_stage_inputs(options: &ImportOptions, plan: &ImportPlan) -> Result<u64, ImportError> {
    let units = options
        .inspection
        .shape
        .t()
        .checked_mul(u64::from(options.inspection.channels))
        .ok_or(ImportError::Overflow)?;
    let packed = plan
        .logical_bricks_by_scale
        .iter()
        .try_fold(0_u64, |total, records| {
            total
                .checked_add(records.div_ceil(16_384))
                .ok_or(ImportError::Overflow)
        })?;
    units
        .checked_mul(unit_shard_inputs(plan)?)
        .and_then(|value| value.checked_add(packed))
        .ok_or(ImportError::Overflow)
}

fn validate_stage_prefix(
    options: &ImportOptions,
    plan: &ImportPlan,
    completed_units: u64,
    durable_inputs: u64,
) -> Result<(), ImportError> {
    let unit_inputs = unit_shard_inputs(plan)?;
    let completed = completed_units
        .checked_mul(unit_inputs)
        .ok_or(ImportError::Overflow)?;
    let total = total_stage_inputs(options, plan)?;
    if durable_inputs < completed || durable_inputs > total {
        return Err(ImportError::InvalidCheckpoint(
            "final-layout stage prefix disagrees with completed temporal units".to_owned(),
        ));
    }
    let total_units = options
        .inspection
        .shape
        .t()
        .checked_mul(u64::from(options.inspection.channels))
        .ok_or(ImportError::Overflow)?;
    if completed_units < total_units
        && durable_inputs
            > completed
                .checked_add(unit_inputs)
                .ok_or(ImportError::Overflow)?
    {
        return Err(ImportError::InvalidCheckpoint(
            "stage is more than one recoverable temporal unit ahead of its identity checkpoint"
                .to_owned(),
        ));
    }
    Ok(())
}

fn validate_unit_spool_prefix(
    spool: &ImportSpool,
    plan: &ImportPlan,
    timepoint: u64,
    channel: u32,
) -> Result<(), ImportError> {
    let mut expected =
        (0..plan.shapes.len()).flat_map(|scale| unit_keys(plan, scale, timepoint, channel));
    for actual in spool.keys() {
        if Some(actual) != expected.next() {
            return Err(ImportError::InvalidCheckpoint(
                "temporal-unit spool is not a canonical plan prefix".to_owned(),
            ));
        }
    }
    Ok(())
}

fn decoded_source_digest(
    options: &ImportOptions,
    journal: &UnitJournal,
) -> Result<Sha256Digest, ImportError> {
    let expected = options
        .inspection
        .shape
        .t()
        .checked_mul(u64::from(options.inspection.channels))
        .ok_or(ImportError::Overflow)?;
    if journal.completed_units() != expected {
        return Err(ImportError::InvalidCheckpoint(
            "decoded-source identity requested before all units completed".to_owned(),
        ));
    }
    let mut hasher = Sha256Hasher::new();
    hasher.update(b"mirante4d-canonical-decoded-source-v2\0");
    for dimension in options.inspection.shape.dimensions() {
        hasher.update(dimension.to_le_bytes());
    }
    hasher.update(options.inspection.channels.to_le_bytes());
    hasher.update([match options.inspection.dtype {
        mirante4d_domain::IntensityDType::Uint8 => 1,
        mirante4d_domain::IntensityDType::Uint16 => 2,
        mirante4d_domain::IntensityDType::Float32 => 3,
    }]);
    for channel in 0..options.inspection.channels {
        for timepoint in 0..options.inspection.shape.t() {
            hasher.update(journal.read_decoded_digest(channel, timepoint)?.as_bytes());
        }
    }
    Ok(hasher.finalize())
}

fn finalize_scientific(
    scientific: Vec<ScientificLayerHasher>,
) -> Result<mirante4d_identity::ScientificContentId, ImportError> {
    let mut dataset = ScientificDatasetHasher::new(
        u32::try_from(scientific.len()).map_err(|_| ImportError::Overflow)?,
    )?;
    for layer in scientific {
        dataset.push_layer(layer.finalize()?)?;
    }
    Ok(dataset.finalize()?)
}

fn finalize_stage(
    stage: ResumableLocalPackageStage,
    metadata: PackageMetadata,
    cancellation: &ImportCancellation,
    statistics: &mut ImportStatistics,
    progress: &mut impl FnMut(ImportEvent),
) -> Result<mirante4d_storage::PublishedScientificPackageTransfer, ImportError> {
    let input = PackageWriteInput::new(
        metadata.profile_kind,
        metadata.profile,
        metadata.science,
        metadata.display_defaults,
        metadata.portable_records,
        metadata.ome_images,
        metadata.arrays,
        std::iter::empty(),
    );
    stage
        .finalize_scientifically_validated(
            input,
            || cancellation.is_cancelled(),
            |event| match event {
                PackageWriteEvent::StageStarted(stage) => progress(ImportEvent::StageStarted {
                    stage: import_stage(stage),
                    completed_work_units: 0,
                    total_work_units: None,
                }),
                PackageWriteEvent::StageFinished(timing) => {
                    let timing = ImportStageTiming {
                        stage: import_stage(timing.stage),
                        wall_time_ns: timing.wall_time_ns,
                        cpu_time_ns: timing.cpu_time_ns,
                    };
                    statistics.stages.push(timing);
                    progress(ImportEvent::StageFinished(timing));
                }
            },
        )
        .map_err(ImportError::from)
}

const fn import_stage(stage: PackageWriteStage) -> ImportStage {
    match stage {
        PackageWriteStage::ShardPublication => ImportStage::ShardPublication,
        PackageWriteStage::StagedStructureValidation => ImportStage::StagedStructureValidation,
        PackageWriteStage::StagedExactValidation => ImportStage::StagedExactValidation,
        PackageWriteStage::StagedScientificValidation => ImportStage::StagedScientificValidation,
        PackageWriteStage::Commit => ImportStage::Commit,
    }
}

fn unit_keys(
    plan: &ImportPlan,
    scale: usize,
    timepoint: u64,
    channel: u32,
) -> impl Iterator<Item = SpoolWorkUnitKey> + '_ {
    let shape = plan.shapes[scale];
    let grid = chunk_grid([shape.z(), shape.y(), shape.x()], plan.is_2d);
    (0..grid[0]).flat_map(move |z| {
        (0..grid[1]).flat_map(move |y| {
            (0..grid[2]).map(move |x| {
                work_key(scale, timepoint, u64::from(channel), [z, y, x])
                    .expect("compositional profile keeps work coordinates in u32")
            })
        })
    })
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

fn spatial_bricks(shape: Shape4D, is_2d: bool) -> Result<u64, ImportError> {
    checked_product(chunk_grid([shape.z(), shape.y(), shape.x()], is_2d))
}

fn checked_product(values: [u64; 3]) -> Result<u64, ImportError> {
    values.into_iter().try_fold(1_u64, |product, value| {
        product.checked_mul(value).ok_or(ImportError::Overflow)
    })
}

fn record_worker_diagnostics(
    statistics: &mut ImportStatistics,
    resident_working_bytes: u64,
    task_charge_bytes: u64,
    diagnostics: OrderedWorkerDiagnostics,
) -> Result<(), ImportError> {
    let in_flight = u64::try_from(diagnostics.peak_in_flight).map_err(|_| ImportError::Overflow)?;
    statistics.peak_working_bytes = statistics.peak_working_bytes.max(
        resident_working_bytes
            .checked_add(
                in_flight
                    .checked_mul(task_charge_bytes)
                    .ok_or(ImportError::Overflow)?,
            )
            .ok_or(ImportError::Overflow)?,
    );
    Ok(())
}

fn record_source_report(
    statistics: &mut ImportStatistics,
    resident_working_bytes: u64,
    report: crate::source::SourceDecodeReport,
) -> Result<(), ImportError> {
    statistics.peak_working_bytes = statistics.peak_working_bytes.max(
        resident_working_bytes
            .checked_add(report.peak_transient_bytes)
            .ok_or(ImportError::Overflow)?,
    );
    statistics.source_bytes_read = statistics
        .source_bytes_read
        .checked_add(report.counters.source_bytes_read)
        .ok_or(ImportError::Overflow)?;
    statistics.native_decoded_bytes = statistics
        .native_decoded_bytes
        .checked_add(report.counters.decoded_bytes)
        .ok_or(ImportError::Overflow)?;
    statistics.base_native_decoded_bytes = statistics
        .base_native_decoded_bytes
        .checked_add(report.counters.decoded_bytes)
        .ok_or(ImportError::Overflow)?;
    statistics.tiff_open_count = statistics
        .tiff_open_count
        .checked_add(report.counters.tiff_open_count)
        .ok_or(ImportError::Overflow)?;
    statistics.native_chunk_decode_count = statistics
        .native_chunk_decode_count
        .checked_add(report.counters.native_chunk_decode_count)
        .ok_or(ImportError::Overflow)?;
    Ok(())
}

fn record_no_data_detection_report(
    statistics: &mut ImportStatistics,
    report: &crate::source::NoDataDetectionReport,
) -> Result<(), ImportError> {
    statistics.source_bytes_read = statistics
        .source_bytes_read
        .checked_add(report.counters.source_bytes_read)
        .ok_or(ImportError::Overflow)?;
    statistics.native_decoded_bytes = statistics
        .native_decoded_bytes
        .checked_add(report.counters.decoded_bytes)
        .ok_or(ImportError::Overflow)?;
    statistics.no_data_detection_native_decoded_bytes = statistics
        .no_data_detection_native_decoded_bytes
        .checked_add(report.counters.decoded_bytes)
        .ok_or(ImportError::Overflow)?;
    statistics.tiff_open_count = statistics
        .tiff_open_count
        .checked_add(report.counters.tiff_open_count)
        .ok_or(ImportError::Overflow)?;
    statistics.native_chunk_decode_count = statistics
        .native_chunk_decode_count
        .checked_add(report.counters.native_chunk_decode_count)
        .ok_or(ImportError::Overflow)?;
    let _completed_planes = report.completed_planes;
    Ok(())
}

fn record_spool_diagnostics(
    statistics: &mut ImportStatistics,
    diagnostics: SpoolDiagnostics,
) -> Result<(), ImportError> {
    statistics.checkpoint_payload_bytes = statistics
        .checkpoint_payload_bytes
        .max(diagnostics.checkpoint_payload_bytes);
    statistics.checkpoint_journal_bytes = statistics
        .checkpoint_journal_bytes
        .max(diagnostics.checkpoint_journal_bytes);
    statistics.checkpoint_watermark_bytes = statistics
        .checkpoint_watermark_bytes
        .max(diagnostics.checkpoint_watermark_bytes);
    statistics.checkpoint_durable_work_units = statistics
        .checkpoint_durable_work_units
        .max(diagnostics.checkpoint_durable_work_units);
    statistics.checkpoint_pending_work_units = diagnostics.checkpoint_pending_work_units;
    statistics.checkpoint_committed_batches = statistics
        .checkpoint_committed_batches
        .checked_add(diagnostics.checkpoint_committed_batches)
        .ok_or(ImportError::Overflow)?;
    statistics.codec_encode_calls = statistics
        .codec_encode_calls
        .checked_add(diagnostics.codec_encode_calls)
        .ok_or(ImportError::Overflow)?;
    statistics.codec_encode_time_ns = statistics
        .codec_encode_time_ns
        .checked_add(diagnostics.codec_encode_time_ns)
        .ok_or(ImportError::Overflow)?;
    statistics.codec_decode_calls = statistics
        .codec_decode_calls
        .checked_add(diagnostics.codec_decode_calls)
        .ok_or(ImportError::Overflow)?;
    statistics.codec_decode_time_ns = statistics
        .codec_decode_time_ns
        .checked_add(diagnostics.codec_decode_time_ns)
        .ok_or(ImportError::Overflow)?;
    statistics.sync_calls = statistics
        .sync_calls
        .checked_add(diagnostics.sync_calls)
        .ok_or(ImportError::Overflow)?;
    statistics.sync_time_ns = statistics
        .sync_time_ns
        .checked_add(diagnostics.sync_time_ns)
        .ok_or(ImportError::Overflow)?;
    Ok(())
}

fn finish_stage(
    clock: StageClock,
    options: &ImportOptions,
    statistics: &mut ImportStatistics,
    progress: &mut impl FnMut(ImportEvent),
) -> Result<(), ImportError> {
    sample_process_resources(
        statistics,
        &[&options.checkpoint_directory, &options.destination],
    )?;
    let control = control_directory(&options.checkpoint_directory);
    if control.exists() {
        statistics.peak_checkpoint_regular_files = statistics
            .peak_checkpoint_regular_files
            .max(count_regular_files_recursive(&control)?);
    }
    clock.finish(statistics, progress).map(|_| ())
}

fn count_regular_files_recursive(root: &Path) -> Result<u64, ImportError> {
    let mut count = 0_u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|source| ImportError::Io {
            operation: "enumerate import control for resource accounting",
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| ImportError::Io {
                operation: "read import control for resource accounting",
                path: directory.clone(),
                source,
            })?;
            let metadata =
                fs::symlink_metadata(entry.path()).map_err(|source| ImportError::Io {
                    operation: "inspect import control for resource accounting",
                    path: entry.path(),
                    source,
                })?;
            if metadata.file_type().is_symlink() {
                return Err(ImportError::InvalidCheckpoint(
                    "private import control contains a symbolic link".to_owned(),
                ));
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                count = count.checked_add(1).ok_or(ImportError::Overflow)?;
            }
        }
    }
    Ok(count)
}

fn cleanup_unit_cache(cache: CanonicalBaseCache, stage: &Path, ordinal: u64) {
    cache.cleanup_owned_files();
    drop(cache);
    let _ = fs::remove_dir(unit_cache_directory(stage, ordinal));
}

fn remove_unconsumed_unit_cache_directory(options: &ImportOptions, ordinal: u64) {
    let directory = unit_cache_directory(&options.checkpoint_directory, ordinal);
    let Ok(metadata) = fs::symlink_metadata(&directory) else {
        return;
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        let _ = fs::remove_dir_all(directory);
    }
}

fn cleanup_unit_spool(spool: ImportSpool) {
    spool.cleanup_owned_files();
    spool.cleanup_owned_directory();
}

fn create_private_directory(path: &Path) -> Result<(), ImportError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path).map_err(|source| ImportError::Io {
                operation: "inspect temporal-unit scratch directory",
                path: path.to_path_buf(),
                source,
            })?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                Ok(())
            } else {
                Err(ImportError::InvalidCheckpoint(
                    "temporal-unit scratch path is not a real directory".to_owned(),
                ))
            }
        }
        Err(source) => Err(ImportError::Io {
            operation: "create temporal-unit scratch directory",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn require_absent_destination(destination: &Path) -> Result<(), ImportError> {
    if destination.exists() {
        return Err(mirante4d_storage::PackageWriteError::DestinationExists.into());
    }
    let parent = destination.parent().ok_or(ImportError::InvalidRequest(
        "destination must have an existing parent directory",
    ))?;
    if !fs::metadata(parent)
        .map_err(|source| ImportError::Io {
            operation: "inspect destination parent",
            path: parent.to_path_buf(),
            source,
        })?
        .is_dir()
    {
        return Err(ImportError::InvalidRequest(
            "destination parent must be a directory",
        ));
    }
    Ok(())
}

fn validate_path_separation(options: &ImportOptions) -> Result<(), ImportError> {
    let destination = resolved_candidate(&options.destination)?;
    let checkpoint = resolved_candidate(&options.checkpoint_directory)?;
    if destination.parent() != checkpoint.parent() {
        return Err(ImportError::InvalidRequest(
            "the final-layout import checkpoint must be a sibling of the destination",
        ));
    }
    let source_overlaps =
        options
            .inspection
            .source
            .channels()
            .iter()
            .try_fold(false, |overlaps, channel| {
                let source =
                    fs::canonicalize(channel.path()).map_err(|source| ImportError::Io {
                        operation: "resolve source root",
                        path: channel.path().to_path_buf(),
                        source,
                    })?;
                Ok::<_, ImportError>(
                    overlaps || nested(&source, &destination) || nested(&source, &checkpoint),
                )
            })?;
    if source_overlaps || nested(&destination, &checkpoint) {
        return Err(ImportError::InvalidRequest(
            "source, destination, and checkpoint paths must be separate and unnested",
        ));
    }
    Ok(())
}

fn resolved_candidate(path: &Path) -> Result<std::path::PathBuf, ImportError> {
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
    require_available_space(options, plan.start_required_headroom_bytes, false)
}

#[allow(clippy::too_many_arguments)]
fn emit_storage_progress(
    options: &ImportOptions,
    plan: &ImportPlan,
    stage: &ResumableLocalPackageStage,
    completed_temporal_units: u64,
    total_temporal_units: u64,
    active: Option<(u64, u64, u32)>,
    pipeline: &TemporalPipelineStatus,
    progress: &mut impl FnMut(ImportEvent),
) -> Result<(), ImportError> {
    let stage_payload_bytes = stage.durable_payload_bytes();
    let remaining_package_output_upper_bound = plan
        .final_package_upper_bound
        .checked_sub(stage_payload_bytes)
        .ok_or(ImportError::InvalidRequest(
            "durable stage payload exceeded its final-package upper bound",
        ))?;
    let unit_scratch_bytes = match active {
        Some((ordinal, _, _)) => current_unit_scratch_bytes(options, ordinal)?,
        None => 0,
    };
    let decode_ahead_scratch_bytes = match pipeline.future_ordinal {
        Some(ordinal) if active.map(|(active, _, _)| active) != Some(ordinal) => {
            owned_regular_file_bytes(&unit_cache_directory(
                &options.checkpoint_directory,
                ordinal,
            ))?
        }
        _ => 0,
    };
    let additional_headroom_required_bytes = if completed_temporal_units < total_temporal_units {
        unit_additional_headroom(
            options,
            plan,
            stage,
            active
                .map(|(ordinal, _, _)| ordinal)
                .unwrap_or(completed_temporal_units),
        )?
    } else {
        finalization_additional_headroom(options, plan, stage)?
    };
    progress(ImportEvent::StorageProgress(ImportStorageProgress {
        completed_temporal_units,
        total_temporal_units,
        active_timepoint: active.map(|(_, timepoint, _)| timepoint),
        active_channel: active.map(|(_, _, channel)| channel),
        preparing_timepoint: pipeline.preparing_timepoint,
        preparing_channel: pipeline.preparing_channel,
        preparing_completed_planes: pipeline.completed_planes(),
        preparing_total_planes: pipeline.preparing_total_planes,
        prepared_temporal_units: pipeline.prepared_temporal_units,
        temporal_pipeline_width: pipeline.width,
        stage_payload_bytes,
        remaining_package_output_upper_bound,
        unit_scratch_bytes,
        decode_ahead_scratch_bytes,
        additional_headroom_required_bytes,
    }));
    Ok(())
}

/// Checks only the unfinished suffix of one bounded temporal/channel unit and
/// the fixed later finalization reserve. Complete future units are deliberately
/// absent: they are admitted one at a time when they become current.
fn recheck_unit_space(
    options: &ImportOptions,
    plan: &ImportPlan,
    stage: &ResumableLocalPackageStage,
    active_ordinal: u64,
) -> Result<u64, ImportError> {
    let required = unit_additional_headroom(options, plan, stage, active_ordinal)?;
    require_available_space(options, required, true)?;
    Ok(required)
}

fn unit_additional_headroom(
    options: &ImportOptions,
    plan: &ImportPlan,
    stage: &ResumableLocalPackageStage,
    active_ordinal: u64,
) -> Result<u64, ImportError> {
    let inputs_per_unit = unit_shard_inputs(plan)?;
    let first_input = active_ordinal
        .checked_mul(inputs_per_unit)
        .ok_or(ImportError::Overflow)?;
    let after_unit = first_input
        .checked_add(inputs_per_unit)
        .ok_or(ImportError::Overflow)?;
    let durable_end = stage.durable_shard_inputs().min(after_unit);
    if durable_end < first_input || stage.durable_shard_inputs() > after_unit {
        return Err(ImportError::InvalidCheckpoint(
            "durable shard prefix escaped the active temporal unit".to_owned(),
        ));
    }
    let durable_unit_payload = stage
        .durable_payload_bytes_in_input_range(first_input, durable_end)
        .map_err(ImportError::from)?;
    let remaining_unit_output = plan
        .maximum_unit_output_upper_bound
        .checked_sub(durable_unit_payload)
        .ok_or(ImportError::InvalidCheckpoint(
            "durable unit payload exceeded its occupied-object bound".to_owned(),
        ))?;
    let scratch = current_unit_scratch_bytes(options, active_ordinal)?;
    let remaining_scratch = plan.bounded_unit_scratch_bytes.checked_sub(scratch).ok_or(
        ImportError::InvalidCheckpoint(
            "current-unit scratch exceeded its placement bound".to_owned(),
        ),
    )?;
    remaining_scratch
        .checked_add(remaining_unit_output)
        .and_then(|value| value.checked_add(plan.unit_control_increment_upper_bound))
        .and_then(|value| value.checked_add(plan.finalization_headroom_bytes))
        .ok_or(ImportError::Overflow)
}

fn recheck_finalization_space(
    options: &ImportOptions,
    plan: &ImportPlan,
    stage: &ResumableLocalPackageStage,
) -> Result<u64, ImportError> {
    let required = finalization_additional_headroom(options, plan, stage)?;
    require_available_space(options, required, true)?;
    Ok(required)
}

fn finalization_additional_headroom(
    options: &ImportOptions,
    plan: &ImportPlan,
    stage: &ResumableLocalPackageStage,
) -> Result<u64, ImportError> {
    let total_units = options
        .inspection
        .shape
        .t()
        .checked_mul(u64::from(options.inspection.channels))
        .ok_or(ImportError::Overflow)?;
    let packed_start = total_units
        .checked_mul(unit_shard_inputs(plan)?)
        .ok_or(ImportError::Overflow)?;
    if stage.durable_shard_inputs() < packed_start {
        return Err(ImportError::InvalidCheckpoint(
            "finalization began before all temporal-unit shards were durable".to_owned(),
        ));
    }
    let durable_packed_payload = stage
        .durable_payload_bytes_in_input_range(packed_start, stage.durable_shard_inputs())
        .map_err(ImportError::from)?;
    plan.finalization_commit_upper_bound
        .checked_sub(durable_packed_payload)
        .ok_or(ImportError::InvalidCheckpoint(
            "durable packed-index payload exceeded the finalization reserve".to_owned(),
        ))
}

fn current_unit_scratch_bytes(options: &ImportOptions, ordinal: u64) -> Result<u64, ImportError> {
    owned_regular_file_bytes(&unit_cache_directory(
        &options.checkpoint_directory,
        ordinal,
    ))?
    .checked_add(owned_regular_file_bytes(&unit_spool_directory(
        &options.checkpoint_directory,
        ordinal,
    ))?)
    .ok_or(ImportError::Overflow)
}

fn checkpoint_path_exists(path: &Path) -> Result<bool, ImportError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(ImportError::Io {
            operation: "inspect preprocessing checkpoint path",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn available_space(options: &ImportOptions) -> Result<u64, ImportError> {
    let parent = options
        .destination
        .parent()
        .ok_or(ImportError::InvalidRequest(
            "destination must have a parent directory",
        ))?;
    let filesystem = rustix::fs::statvfs(parent).map_err(|source| ImportError::Io {
        operation: "inspect destination filesystem space",
        path: parent.to_path_buf(),
        source: source.into(),
    })?;
    filesystem
        .f_bavail
        .checked_mul(filesystem.f_frsize)
        .ok_or(ImportError::Overflow)
}

fn require_available_space(
    options: &ImportOptions,
    required: u64,
    resumable: bool,
) -> Result<(), ImportError> {
    let available = available_space(options)?;
    capacity_result(required, available, resumable)
}

fn capacity_result(required: u64, available: u64, resumable: bool) -> Result<(), ImportError> {
    if available < required {
        return Err(if resumable {
            ImportError::CapacityPaused {
                required_bytes: required,
                available_bytes: available,
            }
        } else {
            ImportError::InsufficientSpace {
                required_bytes: required,
                available_bytes: available,
            }
        });
    }
    Ok(())
}

fn preserve_capacity_race<T>(
    result: Result<T, ImportError>,
    options: &ImportOptions,
    required: u64,
) -> Result<T, ImportError> {
    let available = available_space(options).unwrap_or(0);
    map_capacity_race(result, required, available)
}

fn map_capacity_race<T>(
    result: Result<T, ImportError>,
    required: u64,
    available: u64,
) -> Result<T, ImportError> {
    match result {
        Ok(value) => Ok(value),
        Err(error) if is_safe_storage_full_error(&error) => Err(ImportError::CapacityPaused {
            required_bytes: required,
            available_bytes: available,
        }),
        Err(error) => Err(error),
    }
}

fn is_safe_storage_full_error(error: &ImportError) -> bool {
    if matches!(
        error,
        ImportError::Writer(mirante4d_storage::PackageWriteError::CommitIndeterminate { .. })
    ) {
        return false;
    }
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(source) = current {
        if source
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::StorageFull)
        {
            return true;
        }
        current = source.source();
    }
    false
}

fn check_cancelled(cancellation: &ImportCancellation) -> Result<(), ImportError> {
    if cancellation.is_cancelled() {
        Err(ImportError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum TestFinalizationFault {
    StorageFull,
    PermissionDenied,
}

#[cfg(test)]
thread_local! {
    static TEST_FINALIZATION_FAULT: std::cell::RefCell<Option<(std::path::PathBuf, TestFinalizationFault)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn with_test_finalization_fault<T>(
    destination: &Path,
    fault: TestFinalizationFault,
    operation: impl FnOnce() -> T,
) -> T {
    TEST_FINALIZATION_FAULT.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "nested finalization fault injection"
        );
        *slot.borrow_mut() = Some((destination.to_path_buf(), fault));
    });
    let result = operation();
    TEST_FINALIZATION_FAULT.with(|slot| *slot.borrow_mut() = None);
    result
}

#[cfg(test)]
fn maybe_inject_finalization_fault(destination: &Path) -> Result<(), ImportError> {
    let fault = TEST_FINALIZATION_FAULT.with(|slot| {
        slot.borrow()
            .as_ref()
            .filter(|(expected, _)| expected == destination)
            .map(|(_, fault)| *fault)
    });
    let Some(fault) = fault else {
        return Ok(());
    };
    let kind = match fault {
        TestFinalizationFault::StorageFull => std::io::ErrorKind::StorageFull,
        TestFinalizationFault::PermissionDenied => std::io::ErrorKind::PermissionDenied,
    };
    Err(ImportError::Io {
        operation: "finalize package fault injection",
        path: destination.to_path_buf(),
        source: std::io::Error::from(kind),
    })
}

#[cfg(test)]
mod tests {
    use mirante4d_dataset::{CpuByteLease, CpuLedgerError};
    use tiff::encoder::{Compression, DeflateLevel, TiffEncoder, colortype};

    use super::*;

    struct UnlimitedTestLease(u64);

    impl CpuByteLease for UnlimitedTestLease {
        fn category(&self) -> CpuLedgerCategory {
            CpuLedgerCategory::ImportWorkingSet
        }

        fn reserved_bytes(&self) -> u64 {
            self.0
        }
    }

    struct UnlimitedTestLedger;

    impl CpuByteLedger for UnlimitedTestLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            assert_eq!(category, CpuLedgerCategory::ImportWorkingSet);
            assert_ne!(bytes, 0);
            Ok(Box::new(UnlimitedTestLease(bytes)))
        }
    }

    struct ReportedCapacityTestLedger(u64);

    impl CpuByteLedger for ReportedCapacityTestLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            assert_eq!(category, CpuLedgerCategory::ImportWorkingSet);
            assert_ne!(bytes, 0);
            Ok(Box::new(UnlimitedTestLease(bytes)))
        }

        fn capacity_bytes(&self) -> u64 {
            self.0
        }
    }

    #[derive(Default)]
    struct AccountingTestState {
        used: AtomicU64,
        peak: AtomicU64,
    }

    #[derive(Default)]
    struct AccountingTestLedger {
        state: Arc<AccountingTestState>,
    }

    struct AccountingTestLease {
        state: Arc<AccountingTestState>,
        bytes: u64,
    }

    impl CpuByteLease for AccountingTestLease {
        fn category(&self) -> CpuLedgerCategory {
            CpuLedgerCategory::ImportWorkingSet
        }

        fn reserved_bytes(&self) -> u64 {
            self.bytes
        }
    }

    impl Drop for AccountingTestLease {
        fn drop(&mut self) {
            self.state.used.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }

    impl CpuByteLedger for AccountingTestLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            assert_eq!(category, CpuLedgerCategory::ImportWorkingSet);
            let used = self.state.used.fetch_add(bytes, Ordering::AcqRel) + bytes;
            self.state.peak.fetch_max(used, Ordering::AcqRel);
            Ok(Box::new(AccountingTestLease {
                state: Arc::clone(&self.state),
                bytes,
            }))
        }
    }

    #[test]
    fn initial_shortage_is_a_setup_blocker_but_runtime_shortage_is_resumable() {
        assert!(matches!(
            capacity_result(2_048, 1_024, false),
            Err(ImportError::InsufficientSpace {
                required_bytes: 2_048,
                available_bytes: 1_024,
            })
        ));
        assert!(matches!(
            capacity_result(2_048, 1_024, true),
            Err(ImportError::CapacityPaused {
                required_bytes: 2_048,
                available_bytes: 1_024,
            })
        ));
        assert!(capacity_result(2_048, 2_048, true).is_ok());
    }

    #[test]
    fn an_enospc_write_race_is_recognized_as_safe_before_publication() {
        let error = ImportError::Io {
            operation: "capacity fault injection",
            path: std::path::PathBuf::from("checkpoint"),
            source: std::io::Error::from(std::io::ErrorKind::StorageFull),
        };
        assert!(is_safe_storage_full_error(&error));
        assert!(matches!(
            map_capacity_race::<()>(Err(error), 8_192, 4_096),
            Err(ImportError::CapacityPaused {
                required_bytes: 8_192,
                available_bytes: 4_096,
            })
        ));
    }

    #[test]
    fn full_pipeline_finalization_faults_preserve_source_and_resume_exactly() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.tif");
        let file = fs::File::create(&source).unwrap();
        let mut encoder = TiffEncoder::new(file).unwrap();
        for z in 0_u8..2 {
            let pixels = (0_u8..16)
                .map(|value| value.wrapping_add(z.wrapping_mul(19)))
                .collect::<Vec<_>>();
            encoder
                .write_image::<colortype::Gray8>(4, 4, &pixels)
                .unwrap();
        }
        let source_before = fs::read(&source).unwrap();
        let inspection = crate::source::inspect(crate::TiffSource::single_3d(&source)).unwrap();
        let options = |label: &str| ImportOptions {
            inspection: inspection.clone(),
            destination: root.path().join(format!("{label}.m4d")),
            checkpoint_directory: root.path().join(format!("{label}.checkpoint")),
            profile: mirante4d_storage::ProfileKind::Current,
            calibration: crate::SpatialCalibration::new([1.0; 3]),
            time_step_seconds: None,
            no_data: None,
        };
        let control = run(
            options("control"),
            &UnlimitedTestLedger,
            &ImportCancellation::new(),
            |_| {},
        )
        .unwrap();

        for (label, fault) in [
            ("storage-full", TestFinalizationFault::StorageFull),
            ("permission", TestFinalizationFault::PermissionDenied),
        ] {
            let faulted = options(label);
            let error = with_test_finalization_fault(&faulted.destination, fault, || {
                run(
                    faulted.clone(),
                    &UnlimitedTestLedger,
                    &ImportCancellation::new(),
                    |_| {},
                )
                .unwrap_err()
            });
            match fault {
                TestFinalizationFault::StorageFull => assert!(matches!(
                    error,
                    ImportError::CapacityPaused {
                        required_bytes: 1..,
                        ..
                    }
                )),
                TestFinalizationFault::PermissionDenied => assert!(matches!(
                    error,
                    ImportError::Io {
                        source,
                        ..
                    } if source.kind() == std::io::ErrorKind::PermissionDenied
                )),
            }
            assert!(!faulted.destination.exists());
            assert!(faulted.checkpoint_directory.is_dir());
            assert_eq!(fs::read(&source).unwrap(), source_before);

            let resumed = run(
                faulted,
                &UnlimitedTestLedger,
                &ImportCancellation::new(),
                |_| {},
            )
            .unwrap();
            assert_eq!(resumed.receipt().package_id, control.receipt().package_id);
            assert_eq!(
                resumed.receipt().scientific_content_id,
                control.receipt().scientific_content_id
            );
            assert_eq!(fs::read(&source).unwrap(), source_before);
        }
    }

    #[test]
    fn optional_cache_never_turns_serial_headroom_into_a_hard_failure() {
        assert!(!optional_cache_headroom_fits(1_999, 1_000, 1_000, 0).unwrap());
        assert!(optional_cache_headroom_fits(2_000, 1_000, 1_000, 0).unwrap());
        assert!(optional_cache_headroom_fits(1_250, 1_000, 1_000, 750).unwrap());
        assert!(matches!(
            optional_cache_headroom_fits(10_000, 1_000, 1_000, 1_001),
            Err(ImportError::InvalidCheckpoint(_))
        ));
    }

    #[test]
    fn speculative_ledger_view_cannot_borrow_the_protected_progress_bytes() {
        let refusals = Arc::new(AtomicU64::new(0));
        let capped =
            CappedCpuLedger::new(&UnlimitedTestLedger, 10, 20, Arc::clone(&refusals)).unwrap();
        assert_eq!(capped.capacity_bytes(), 30);
        let first = capped
            .try_acquire(CpuLedgerCategory::ImportWorkingSet, 6)
            .unwrap();
        assert!(matches!(
            capped.try_acquire(CpuLedgerCategory::ImportWorkingSet, 5),
            Err(CpuLedgerError::CapacityExceeded {
                available_bytes: 4,
                ..
            })
        ));
        assert_eq!(refusals.load(Ordering::Acquire), 1);
        drop(first);
        assert!(
            capped
                .try_acquire(CpuLedgerCategory::ImportWorkingSet, 10)
                .is_ok()
        );
    }

    #[test]
    fn forced_width_one_and_decode_ahead_publish_identical_packages_in_order() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        for timepoint in 0_u8..4 {
            let file = fs::File::create(source.join(format!("volume-{timepoint:03}.tif"))).unwrap();
            let mut encoder = TiffEncoder::new(file).unwrap();
            for z in 0_u8..8 {
                let pixels = (0_u32..64 * 64)
                    .map(|index| {
                        (index as u8)
                            .wrapping_mul(17)
                            .wrapping_add(z.wrapping_mul(13))
                            .wrapping_add(timepoint.wrapping_mul(19))
                    })
                    .collect::<Vec<_>>();
                encoder
                    .write_image::<colortype::Gray8>(64, 64, &pixels)
                    .unwrap();
            }
        }
        let inspection = crate::source::inspect(
            crate::TiffSource::new(vec![
                crate::TiffChannelSource::folder_of_3d("signal", &source).unwrap(),
            ])
            .unwrap(),
        )
        .unwrap();
        let options = |name: &str| ImportOptions {
            inspection: inspection.clone(),
            destination: root.path().join(format!("{name}.m4d")),
            checkpoint_directory: root.path().join(format!("{name}.checkpoint")),
            profile: mirante4d_storage::ProfileKind::Current,
            calibration: crate::SpatialCalibration::new([1.0; 3]),
            time_step_seconds: Some(1.0),
            no_data: None,
        };
        let ledger = UnlimitedTestLedger;
        let serial = crate::ordered_workers::with_test_worker_count(4, || {
            with_test_decode_ahead(false, || {
                run(
                    options("serial"),
                    &ledger,
                    &ImportCancellation::new(),
                    |_| {},
                )
                .unwrap()
            })
        });
        let constrained_options = options("constrained");
        let constrained_capacity = ImportPlan::new(&constrained_options)
            .unwrap()
            .minimum_execution_bytes;
        let constrained_ledger = ReportedCapacityTestLedger(constrained_capacity);
        let constrained = crate::ordered_workers::with_test_worker_count(4, || {
            with_test_decode_ahead(true, || {
                run(
                    constrained_options,
                    &constrained_ledger,
                    &ImportCancellation::new(),
                    |_| {},
                )
                .unwrap()
            })
        });
        let mut pipeline_events = Vec::new();
        let pipeline_ledger = AccountingTestLedger::default();
        let pipelined = crate::ordered_workers::with_test_worker_count(4, || {
            with_test_decode_ahead(true, || {
                run(
                    options("pipelined"),
                    &pipeline_ledger,
                    &ImportCancellation::new(),
                    |event| pipeline_events.push(event),
                )
                .unwrap()
            })
        });

        assert_eq!(serial.receipt().package_id, pipelined.receipt().package_id);
        assert_eq!(
            serial.receipt().scientific_content_id,
            pipelined.receipt().scientific_content_id
        );
        assert_eq!(
            serial.receipt().package_id,
            constrained.receipt().package_id
        );
        assert_eq!(
            serial.receipt().statistics.base_native_decoded_bytes,
            pipelined.receipt().statistics.base_native_decoded_bytes
        );
        assert_eq!(
            serial.receipt().statistics.native_chunk_decode_count,
            pipelined.receipt().statistics.native_chunk_decode_count
        );
        assert_eq!(
            pipelined.receipt().statistics.peak_working_bytes,
            pipeline_ledger.state.peak.load(Ordering::Acquire)
        );
        assert_eq!(pipeline_ledger.state.used.load(Ordering::Acquire), 0);
        assert_eq!(
            serial.receipt().statistics.maximum_temporal_pipeline_width,
            1
        );
        assert_eq!(
            constrained
                .receipt()
                .statistics
                .maximum_temporal_pipeline_width,
            1
        );
        assert_eq!(constrained.receipt().statistics.prefetch_units_admitted, 0);
        assert!(
            constrained
                .receipt()
                .statistics
                .prefetch_cpu_capacity_deferrals
                > 0
        );
        assert_eq!(
            pipelined
                .receipt()
                .statistics
                .maximum_temporal_pipeline_width,
            2
        );
        assert_eq!(pipelined.receipt().statistics.prefetch_units_admitted, 3);
        assert_eq!(pipelined.receipt().statistics.prefetch_units_consumed, 3);
        assert!(pipeline_events.iter().any(|event| matches!(
            event,
            ImportEvent::StorageProgress(progress)
                if progress.temporal_pipeline_width == 2
                    && progress.preparing_timepoint.is_some()
        )));
        assert!(pipeline_events.iter().all(|event| !matches!(
            event,
            ImportEvent::StorageProgress(progress) if progress.temporal_pipeline_width > 2
        )));
        let no_data_finished = pipeline_events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    ImportEvent::StageFinished(timing)
                        if timing.stage == ImportStage::NoDataDetection
                )
            })
            .unwrap();
        let first_decode_ahead = pipeline_events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    ImportEvent::StorageProgress(progress)
                        if progress.temporal_pipeline_width == 2
                )
            })
            .unwrap();
        assert!(first_decode_ahead > no_data_finished);
    }

    #[test]
    fn source_replacement_during_decode_ahead_fails_without_publication() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        for timepoint in 0_u8..2 {
            let file = fs::File::create(source.join(format!("volume-{timepoint:03}.tif"))).unwrap();
            let mut encoder = TiffEncoder::new(file).unwrap();
            for z in 0_u8..8 {
                let pixels = (0_u32..64 * 64)
                    .map(|index| {
                        (index as u8)
                            .wrapping_add(z.wrapping_mul(7))
                            .wrapping_add(timepoint.wrapping_mul(11))
                    })
                    .collect::<Vec<_>>();
                encoder
                    .write_image::<colortype::Gray8>(64, 64, &pixels)
                    .unwrap();
            }
        }
        let inspection = crate::source::inspect(
            crate::TiffSource::new(vec![
                crate::TiffChannelSource::folder_of_3d("signal", &source).unwrap(),
            ])
            .unwrap(),
        )
        .unwrap();
        let destination = root.path().join("dataset.m4d");
        let options = ImportOptions {
            inspection,
            destination: destination.clone(),
            checkpoint_directory: root.path().join("checkpoint"),
            profile: mirante4d_storage::ProfileKind::Current,
            calibration: crate::SpatialCalibration::new([1.0; 3]),
            time_step_seconds: Some(1.0),
            no_data: None,
        };
        let future_source = source.join("volume-001.tif");
        let mut replaced = false;
        let error = crate::ordered_workers::with_test_worker_count(4, || {
            with_test_decode_ahead(true, || {
                run(
                    options,
                    &UnlimitedTestLedger,
                    &ImportCancellation::new(),
                    |event| {
                        if !replaced
                            && matches!(
                                event,
                                ImportEvent::StorageProgress(progress)
                                    if progress.temporal_pipeline_width == 2
                                        && progress.preparing_timepoint == Some(1)
                            )
                        {
                            use std::io::Write as _;
                            fs::OpenOptions::new()
                                .append(true)
                                .open(&future_source)
                                .unwrap()
                                .write_all(&[0])
                                .unwrap();
                            replaced = true;
                        }
                    },
                )
                .unwrap_err()
            })
        });
        assert!(matches!(error, ImportError::SourceChanged(path) if path == future_source));
        assert!(!destination.exists());
    }

    #[test]
    #[ignore = "developer-local release performance evidence"]
    fn temporal_decode_ahead_release_benchmark() {
        const TIMEPOINTS: u8 = 20;
        const DEPTH: u8 = 32;
        const SIDE: u32 = 257;
        const SAMPLES: usize = 3;

        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        for timepoint in 0..TIMEPOINTS {
            let file = fs::File::create(source.join(format!("volume-{timepoint:03}.tif"))).unwrap();
            let mut encoder = TiffEncoder::new(file)
                .unwrap()
                .with_compression(Compression::Deflate(DeflateLevel::default()));
            for z in 0..DEPTH {
                let pixels = (0..SIDE * SIDE)
                    .map(|index| {
                        (index as u8)
                            .wrapping_mul(73)
                            .wrapping_add(((index >> 8) as u8).wrapping_mul(29))
                            .wrapping_add(z.wrapping_mul(31))
                            .wrapping_add(timepoint.wrapping_mul(43))
                    })
                    .collect::<Vec<_>>();
                encoder
                    .write_image::<colortype::Gray8>(SIDE, SIDE, &pixels)
                    .unwrap();
            }
        }
        let inspection = crate::source::inspect(
            crate::TiffSource::new(vec![
                crate::TiffChannelSource::folder_of_3d("signal", &source).unwrap(),
            ])
            .unwrap(),
        )
        .unwrap();
        let ledger = UnlimitedTestLedger;
        let mut serial = Vec::new();
        let mut pipelined = Vec::new();
        let mut serial_source_fraction = Vec::new();
        let mut serial_production_fraction = Vec::new();
        let mut package_id = None;
        for sample in 0..SAMPLES {
            let order = if sample.is_multiple_of(2) {
                [false, true]
            } else {
                [true, false]
            };
            for enabled in order {
                let name = if enabled { "pipeline" } else { "serial" };
                let options = ImportOptions {
                    inspection: inspection.clone(),
                    destination: root.path().join(format!("{name}-{sample}.m4d")),
                    checkpoint_directory: root.path().join(format!("{name}-{sample}.checkpoint")),
                    profile: mirante4d_storage::ProfileKind::Current,
                    calibration: crate::SpatialCalibration::new([1.0; 3]),
                    time_step_seconds: Some(1.0),
                    no_data: None,
                };
                let published = with_test_decode_ahead(enabled, || {
                    run(options, &ledger, &ImportCancellation::new(), |_| {}).unwrap()
                });
                let receipt = published.receipt();
                if let Some(expected) = package_id {
                    assert_eq!(receipt.package_id, expected);
                } else {
                    package_id = Some(receipt.package_id);
                }
                if enabled {
                    assert_eq!(receipt.statistics.maximum_temporal_pipeline_width, 2);
                    pipelined.push(receipt.statistics.primary_wall_time_ns);
                } else {
                    assert_eq!(receipt.statistics.maximum_temporal_pipeline_width, 1);
                    let source_ns = receipt
                        .statistics
                        .stages
                        .iter()
                        .filter(|timing| timing.stage == ImportStage::SourceIngest)
                        .map(|timing| timing.wall_time_ns)
                        .sum::<u64>();
                    let production_ns = receipt.statistics.temporal_canonical_processing_time_ns;
                    serial_source_fraction
                        .push(source_ns as f64 / receipt.statistics.primary_wall_time_ns as f64);
                    serial_production_fraction.push(
                        production_ns as f64 / receipt.statistics.primary_wall_time_ns as f64,
                    );
                    serial.push(receipt.statistics.primary_wall_time_ns);
                }
            }
        }
        serial.sort_unstable();
        pipelined.sort_unstable();
        let serial_median = serial[SAMPLES / 2];
        let pipeline_median = pipelined[SAMPLES / 2];
        let improvement = (serial_median - pipeline_median) as f64 / serial_median as f64 * 100.0;
        eprintln!(
            "temporal_pipeline_benchmark serial_ns={serial:?} pipeline_ns={pipelined:?} serial_median_ns={serial_median} pipeline_median_ns={pipeline_median} improvement_percent={improvement:.2} source_fraction={serial_source_fraction:?} production_fraction={serial_production_fraction:?}"
        );
        assert!(
            serial_source_fraction
                .iter()
                .all(|fraction| *fraction >= 0.20)
        );
        assert!(
            serial_production_fraction
                .iter()
                .all(|fraction| *fraction >= 0.20)
        );
        assert!(
            pipeline_median.saturating_mul(100) <= serial_median.saturating_mul(85),
            "one decode-ahead lane did not earn its 15% scheduling gate"
        );

        let single_inspection =
            crate::source::inspect(crate::TiffSource::single_3d(source.join("volume-000.tif")))
                .unwrap();
        let mut single_serial = Vec::new();
        let mut single_pipeline = Vec::new();
        for sample in 0..SAMPLES {
            let order = if sample.is_multiple_of(2) {
                [false, true]
            } else {
                [true, false]
            };
            for enabled in order {
                let name = if enabled {
                    "single-pipeline"
                } else {
                    "single-serial"
                };
                let options = ImportOptions {
                    inspection: single_inspection.clone(),
                    destination: root.path().join(format!("{name}-{sample}.m4d")),
                    checkpoint_directory: root.path().join(format!("{name}-{sample}.checkpoint")),
                    profile: mirante4d_storage::ProfileKind::Current,
                    calibration: crate::SpatialCalibration::new([1.0; 3]),
                    time_step_seconds: None,
                    no_data: None,
                };
                let published = with_test_decode_ahead(enabled, || {
                    run(options, &ledger, &ImportCancellation::new(), |_| {}).unwrap()
                });
                assert_eq!(
                    published
                        .receipt()
                        .statistics
                        .maximum_temporal_pipeline_width,
                    1
                );
                if enabled {
                    single_pipeline.push(published.receipt().statistics.primary_wall_time_ns);
                } else {
                    single_serial.push(published.receipt().statistics.primary_wall_time_ns);
                }
            }
        }
        single_serial.sort_unstable();
        single_pipeline.sort_unstable();
        let single_serial_median = single_serial[SAMPLES / 2];
        let single_pipeline_median = single_pipeline[SAMPLES / 2];
        let single_regression =
            (single_pipeline_median as f64 / single_serial_median as f64 - 1.0) * 100.0;
        eprintln!(
            "temporal_pipeline_single_unit serial_ns={single_serial:?} pipeline_ns={single_pipeline:?} serial_median_ns={single_serial_median} pipeline_median_ns={single_pipeline_median} regression_percent={single_regression:.2}"
        );
        assert!(
            single_pipeline_median.saturating_mul(100) <= single_serial_median.saturating_mul(105),
            "single-unit median regressed by more than 5%"
        );
    }
}

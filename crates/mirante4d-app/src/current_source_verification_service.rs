//! Bounded asynchronous current-source scientific verification.
//!
//! One worker scans the already-open source against D-009 and transfers the
//! proof capability only after the exact source-generation completion can be
//! accepted. The composition root then promotes the existing local source in
//! place, retaining its decoded leases and GPU resource keys.

use std::{
    fmt, io,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread::{self, JoinHandle},
};

use mirante4d_application::{
    OperationFailureCode, OperationKind, OperationToken, SourceSessionGeneration,
};
use mirante4d_dataset::CpuByteLedger;
use mirante4d_project_model::{DatasetLocatorHint, DatasetReference};
use mirante4d_storage::{
    DirectoryInventoryError, LocalDatasetSource, LocalDatasetSourcePromotionError,
    LocalPackageReadDiagnostics, PackageAdmissionError, PackageReadError, PackageValidationError,
    RangeReadError, ScientificPackageValidationError, StorageProfileError,
};

use crate::unified_source_open;

const RESULT_CHANNEL_CAPACITY: usize = 1;
const WORKER_NAME: &str = "mirante4d-current-source-verification";
const PHASE_WORK_UNITS: u64 = 1_000_000;
const TOTAL_WORK_UNITS: u64 = 3 * PHASE_WORK_UNITS;

pub(crate) struct CurrentSourceVerificationService {
    active: Option<ActiveVerification>,
    diagnostics: CurrentSourceVerificationDiagnostics,
    throttle: Arc<VerificationThrottle>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CurrentSourceVerificationDiagnostics {
    /// Workers successfully spawned by the ordinary verifier service.
    pub(crate) started_runs: u64,
    pub(crate) accepted_progress_updates: u64,
    pub(crate) cancelled_runs: u64,
    /// Started workers that returned a failure or lost their result/join path.
    pub(crate) failed_runs: u64,
    pub(crate) accepted_successes: u64,
    /// Completed separate-reader verification runs, irrespective of whether
    /// the later application completion was accepted.
    pub(crate) completed_reader_runs: u64,
    /// Cumulative exact operation/byte/time facts from the separate strict
    /// readers used by completed verification runs. Current gauges are zero;
    /// peak gauges are maxima across those completed runs.
    pub(crate) reader: LocalPackageReadDiagnostics,
}

struct ActiveVerification {
    token: OperationToken,
    cancellation: Arc<AtomicBool>,
    throttle: Arc<VerificationThrottle>,
    progress: Arc<Mutex<Option<CoalescedProgress>>>,
    results: Receiver<CurrentSourceVerificationResult>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct VerificationThrottle {
    interactive_busy: AtomicBool,
    wait_lock: Mutex<()>,
    changed: Condvar,
}

impl VerificationThrottle {
    fn set_interactive_busy(&self, busy: bool) {
        if self.interactive_busy.swap(busy, Ordering::AcqRel) == busy {
            return;
        }
        if !busy {
            // Synchronize with the waiter mutex after publishing the atomic
            // predicate. The waiter therefore either observes `false` before
            // sleeping or is already asleep when this notification occurs.
            let _guard = self
                .wait_lock
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            self.changed.notify_all();
        }
    }

    fn request_cancel(&self, cancellation: &AtomicBool) {
        // Hold the waiter lock while publishing cancellation so a worker
        // cannot observe `busy`, miss this notification, and then sleep.
        let _guard = self
            .wait_lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        cancellation.store(true, Ordering::Release);
        self.changed.notify_all();
    }

    fn checkpoint(&self, cancellation: &AtomicBool) -> bool {
        if is_cancelled(cancellation) {
            return true;
        }
        if !self.interactive_busy.load(Ordering::Acquire) {
            return false;
        }
        let mut guard = self
            .wait_lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        while self.interactive_busy.load(Ordering::Acquire) && !is_cancelled(cancellation) {
            guard = self
                .changed
                .wait(guard)
                .unwrap_or_else(|poison| poison.into_inner());
        }
        is_cancelled(cancellation)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CoalescedProgress {
    completed_work: u64,
    total_work: u64,
}

pub(crate) struct CurrentSourceVerificationProgressResult {
    pub(crate) token: OperationToken,
    pub(crate) completed_work: u64,
    pub(crate) total_work: u64,
}

pub(crate) struct CurrentSourceVerificationResult {
    pub(crate) token: OperationToken,
    pub(crate) outcome: CurrentSourceVerificationOutcome,
}

pub(crate) enum CurrentSourceVerificationOutcome {
    Prepared(Box<PreparedCurrentSourceVerification>),
    Cancelled,
    Failed(OperationFailureCode),
}

pub(crate) struct PreparedCurrentSourceVerification {
    dataset_reference: DatasetReference,
    source_generation: SourceSessionGeneration,
    reader_diagnostics: LocalPackageReadDiagnostics,
}

pub(crate) struct CurrentSourceVerificationPromotion {
    pub(crate) dataset_reference: DatasetReference,
    pub(crate) source_generation: SourceSessionGeneration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CurrentSourceVerificationServiceError {
    Busy,
    NoActiveOperation,
    OperationTokenMismatch,
    InvalidOperationKind,
    LocalSourceUnavailable,
    WorkerSpawnFailed(io::ErrorKind),
    WorkerPanicked,
    ResultChannelDisconnected,
}

impl fmt::Display for CurrentSourceVerificationServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => formatter.write_str("current-source verification is already active"),
            Self::NoActiveOperation => {
                formatter.write_str("no current-source verification is active")
            }
            Self::OperationTokenMismatch => {
                formatter.write_str("current-source verification token does not match")
            }
            Self::InvalidOperationKind => {
                formatter.write_str("current-source verification requires a verification token")
            }
            Self::LocalSourceUnavailable => {
                formatter.write_str("current-source verification requires a retained local source")
            }
            Self::WorkerSpawnFailed(kind) => {
                write!(
                    formatter,
                    "failed to spawn source-verification worker: {kind:?}"
                )
            }
            Self::WorkerPanicked => formatter.write_str("source-verification worker panicked"),
            Self::ResultChannelDisconnected => {
                formatter.write_str("source-verification result channel disconnected")
            }
        }
    }
}

impl std::error::Error for CurrentSourceVerificationServiceError {}

impl CurrentSourceVerificationService {
    pub(crate) fn new() -> Self {
        Self {
            active: None,
            diagnostics: CurrentSourceVerificationDiagnostics {
                started_runs: 0,
                accepted_progress_updates: 0,
                cancelled_runs: 0,
                failed_runs: 0,
                accepted_successes: 0,
                completed_reader_runs: 0,
                reader: LocalPackageReadDiagnostics::default(),
            },
            throttle: Arc::new(VerificationThrottle::default()),
        }
    }

    pub(crate) fn active_token(&self) -> Option<&OperationToken> {
        self.active.as_ref().map(|active| &active.token)
    }

    pub(crate) const fn diagnostics(&self) -> CurrentSourceVerificationDiagnostics {
        self.diagnostics
    }

    /// Verification remains mandatory, but cooperatively yields between its
    /// bounded scan units while interactive delivery or rendering is active.
    pub(crate) fn set_interactive_busy(&self, busy: bool) {
        self.throttle.set_interactive_busy(busy);
    }

    pub(crate) fn reset_diagnostics(
        &mut self,
    ) -> Result<(), CurrentSourceVerificationServiceError> {
        if self.active.is_some() {
            return Err(CurrentSourceVerificationServiceError::Busy);
        }
        self.diagnostics = CurrentSourceVerificationDiagnostics::default();
        Ok(())
    }

    pub(crate) fn note_accepted_progress(&mut self) {
        self.diagnostics.accepted_progress_updates =
            self.diagnostics.accepted_progress_updates.saturating_add(1);
    }

    pub(crate) fn note_cancelled_run(&mut self) {
        self.diagnostics.cancelled_runs = self.diagnostics.cancelled_runs.saturating_add(1);
    }

    fn note_failed_run(&mut self) {
        self.diagnostics.failed_runs = self.diagnostics.failed_runs.saturating_add(1);
    }

    pub(crate) fn note_accepted_success(&mut self) {
        self.diagnostics.accepted_successes = self.diagnostics.accepted_successes.saturating_add(1);
    }

    pub(crate) fn request_verification(
        &mut self,
        token: OperationToken,
        path: PathBuf,
        scan_ledger: Arc<dyn CpuByteLedger>,
        source: Arc<LocalDatasetSource>,
    ) -> Result<(), CurrentSourceVerificationServiceError> {
        if self.active.is_some() {
            return Err(CurrentSourceVerificationServiceError::Busy);
        }
        if token.kind() != OperationKind::SourceVerification {
            return Err(CurrentSourceVerificationServiceError::InvalidOperationKind);
        }

        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation);
        let progress = Arc::new(Mutex::new(None));
        let worker_progress = Arc::clone(&progress);
        let worker_throttle = Arc::clone(&self.throttle);
        let worker_token = token.clone();
        let (result_sender, results) = mpsc::sync_channel(RESULT_CHANNEL_CAPACITY);
        let worker = thread::Builder::new()
            .name(WORKER_NAME.to_owned())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_verification(
                        &worker_token,
                        path,
                        scan_ledger,
                        source,
                        worker_cancellation.as_ref(),
                        worker_throttle.as_ref(),
                        worker_progress,
                    )
                }))
                .unwrap_or(CurrentSourceVerificationOutcome::Failed(
                    OperationFailureCode::SourceVerificationReadFailed,
                ));
                // Promotion is the operation's commit point. Cancellation
                // wins before that point; once the source authority has been
                // swapped, the prepared completion must be delivered so the
                // application cannot remain provisionally classified over a
                // verified source.
                let outcome = if worker_cancellation.load(Ordering::Acquire)
                    && !matches!(outcome, CurrentSourceVerificationOutcome::Prepared(_))
                {
                    dispose_outcome(outcome);
                    CurrentSourceVerificationOutcome::Cancelled
                } else {
                    outcome
                };
                let result = CurrentSourceVerificationResult {
                    token: worker_token,
                    outcome,
                };
                if let Err(error) = result_sender.send(result) {
                    dispose_outcome(error.0.outcome);
                }
            })
            .map_err(|error| {
                CurrentSourceVerificationServiceError::WorkerSpawnFailed(error.kind())
            })?;

        self.active = Some(ActiveVerification {
            token,
            cancellation,
            throttle: Arc::clone(&self.throttle),
            progress,
            results,
            worker: Some(worker),
        });
        self.diagnostics.started_runs = self.diagnostics.started_runs.saturating_add(1);
        Ok(())
    }

    pub(crate) fn cancel(
        &self,
        token: &OperationToken,
    ) -> Result<(), CurrentSourceVerificationServiceError> {
        let active = self
            .active
            .as_ref()
            .ok_or(CurrentSourceVerificationServiceError::NoActiveOperation)?;
        if &active.token != token {
            return Err(CurrentSourceVerificationServiceError::OperationTokenMismatch);
        }
        active.throttle.request_cancel(active.cancellation.as_ref());
        Ok(())
    }

    pub(crate) fn take_progress(
        &self,
    ) -> Result<
        Option<CurrentSourceVerificationProgressResult>,
        CurrentSourceVerificationServiceError,
    > {
        let Some(active) = self.active.as_ref() else {
            return Ok(None);
        };
        let progress = active
            .progress
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        Ok(
            progress.map(|progress| CurrentSourceVerificationProgressResult {
                token: active.token.clone(),
                completed_work: progress.completed_work,
                total_work: progress.total_work,
            }),
        )
    }

    pub(crate) fn try_recv(
        &mut self,
    ) -> Result<Option<CurrentSourceVerificationResult>, CurrentSourceVerificationServiceError>
    {
        let receive = match self.active.as_ref() {
            None => return Ok(None),
            Some(active) => active.results.try_recv(),
        };
        match receive {
            Ok(mut result) => {
                let cancelled = self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.cancellation.load(Ordering::Acquire));
                if let Err(error) = join_active(self.active.take()) {
                    self.note_failed_run();
                    return Err(error);
                }
                if cancelled
                    && !matches!(
                        result.outcome,
                        CurrentSourceVerificationOutcome::Prepared(_)
                    )
                {
                    let outcome = std::mem::replace(
                        &mut result.outcome,
                        CurrentSourceVerificationOutcome::Cancelled,
                    );
                    dispose_outcome(outcome);
                }
                if matches!(&result.outcome, CurrentSourceVerificationOutcome::Failed(_)) {
                    self.note_failed_run();
                }
                if let CurrentSourceVerificationOutcome::Prepared(prepared) = &result.outcome {
                    self.diagnostics.completed_reader_runs =
                        self.diagnostics.completed_reader_runs.saturating_add(1);
                    self.diagnostics.reader = add_completed_reader_diagnostics(
                        self.diagnostics.reader,
                        prepared.reader_diagnostics,
                    );
                }
                Ok(Some(result))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                let join = join_active(self.active.take());
                self.note_failed_run();
                join?;
                Err(CurrentSourceVerificationServiceError::ResultChannelDisconnected)
            }
        }
    }

    pub(crate) fn shutdown(mut self) -> Result<(), CurrentSourceVerificationServiceError> {
        if let Some(active) = self.active.as_ref() {
            active.throttle.request_cancel(active.cancellation.as_ref());
        }
        join_active(self.active.take())
    }
}

impl Default for CurrentSourceVerificationService {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CurrentSourceVerificationService {
    fn drop(&mut self) {
        if let Some(mut active) = self.active.take() {
            active.throttle.request_cancel(active.cancellation.as_ref());
            let _ = active.worker.take();
        }
    }
}

impl PreparedCurrentSourceVerification {
    pub(crate) fn into_promotion(self) -> CurrentSourceVerificationPromotion {
        CurrentSourceVerificationPromotion {
            dataset_reference: self.dataset_reference,
            source_generation: self.source_generation,
        }
    }
}

fn run_verification(
    token: &OperationToken,
    path: PathBuf,
    scan_ledger: Arc<dyn CpuByteLedger>,
    source: Arc<LocalDatasetSource>,
    cancellation: &AtomicBool,
    throttle: &VerificationThrottle,
    progress: Arc<Mutex<Option<CoalescedProgress>>>,
) -> CurrentSourceVerificationOutcome {
    if verification_checkpoint(cancellation, throttle) {
        return CurrentSourceVerificationOutcome::Cancelled;
    }
    let source_generation = token.source_session_generation();
    let capability = match unified_source_open::verify_target_package(
        &path,
        scan_ledger,
        || verification_checkpoint(cancellation, throttle),
        |stage| store_progress(progress.as_ref(), stage_progress(stage)),
    ) {
        Ok(capability) => capability,
        Err(error) if target_verification_cancelled(&error) => {
            return CurrentSourceVerificationOutcome::Cancelled;
        }
        Err(error) => {
            return CurrentSourceVerificationOutcome::Failed(map_target_verification_error(&error));
        }
    };
    if verification_checkpoint(cancellation, throttle) {
        return CurrentSourceVerificationOutcome::Cancelled;
    }
    let scientific_content_id = capability.scientific_content_id();
    let package_id = capability.package_id();
    if verification_checkpoint(cancellation, throttle) {
        return CurrentSourceVerificationOutcome::Cancelled;
    }
    let reader_diagnostics = match source.promote_verified(capability, || {
        verification_checkpoint(cancellation, throttle)
    }) {
        Ok(diagnostics) => diagnostics,
        Err(failure) => {
            return CurrentSourceVerificationOutcome::Failed(map_promotion_error(failure.error()));
        }
    };
    store_progress(
        progress.as_ref(),
        CoalescedProgress {
            completed_work: TOTAL_WORK_UNITS,
            total_work: TOTAL_WORK_UNITS,
        },
    );
    let locator_hint = path
        .to_str()
        .and_then(|path| DatasetLocatorHint::new(path).ok());
    let dataset_reference =
        DatasetReference::new(scientific_content_id, Some(package_id), None, locator_hint);
    CurrentSourceVerificationOutcome::Prepared(Box::new(PreparedCurrentSourceVerification {
        dataset_reference,
        source_generation,
        reader_diagnostics,
    }))
}

fn add_completed_reader_diagnostics(
    total: LocalPackageReadDiagnostics,
    run: LocalPackageReadDiagnostics,
) -> LocalPackageReadDiagnostics {
    LocalPackageReadDiagnostics {
        object_open_operations: total
            .object_open_operations
            .saturating_add(run.object_open_operations),
        object_open_time_ns: total
            .object_open_time_ns
            .saturating_add(run.object_open_time_ns),
        open_object_handles_current: 0,
        open_object_handles_peak: total
            .open_object_handles_peak
            .max(run.open_object_handles_peak),
        object_handle_cache_entries: 0,
        object_handle_cache_peak_entries: total
            .object_handle_cache_peak_entries
            .max(run.object_handle_cache_peak_entries),
        object_handle_cache_hits: total
            .object_handle_cache_hits
            .saturating_add(run.object_handle_cache_hits),
        object_handle_cache_misses: total
            .object_handle_cache_misses
            .saturating_add(run.object_handle_cache_misses),
        object_handle_cache_evictions: total
            .object_handle_cache_evictions
            .saturating_add(run.object_handle_cache_evictions),
        object_handle_cache_lock_acquisitions: total
            .object_handle_cache_lock_acquisitions
            .saturating_add(run.object_handle_cache_lock_acquisitions),
        object_handle_cache_lock_contentions: total
            .object_handle_cache_lock_contentions
            .saturating_add(run.object_handle_cache_lock_contentions),
        object_handle_cache_lock_wait_time_ns: total
            .object_handle_cache_lock_wait_time_ns
            .saturating_add(run.object_handle_cache_lock_wait_time_ns),
        shard_index_cache_hits: total
            .shard_index_cache_hits
            .saturating_add(run.shard_index_cache_hits),
        shard_index_cache_misses: total
            .shard_index_cache_misses
            .saturating_add(run.shard_index_cache_misses),
        shard_index_decode_operations: total
            .shard_index_decode_operations
            .saturating_add(run.shard_index_decode_operations),
        packed_inner_cache_hits: total
            .packed_inner_cache_hits
            .saturating_add(run.packed_inner_cache_hits),
        packed_inner_cache_misses: total
            .packed_inner_cache_misses
            .saturating_add(run.packed_inner_cache_misses),
        currentness_pre_use_batches: total
            .currentness_pre_use_batches
            .saturating_add(run.currentness_pre_use_batches),
        currentness_post_use_batches: total
            .currentness_post_use_batches
            .saturating_add(run.currentness_post_use_batches),
        currentness_snapshot_batches: total
            .currentness_snapshot_batches
            .saturating_add(run.currentness_snapshot_batches),
        currentness_root_metadata_checks: total
            .currentness_root_metadata_checks
            .saturating_add(run.currentness_root_metadata_checks),
        currentness_named_object_resolutions: total
            .currentness_named_object_resolutions
            .saturating_add(run.currentness_named_object_resolutions),
        currentness_object_fd_metadata_checks: total
            .currentness_object_fd_metadata_checks
            .saturating_add(run.currentness_object_fd_metadata_checks),
        currentness_time_ns: total
            .currentness_time_ns
            .saturating_add(run.currentness_time_ns),
        physical_range_read_operations: total
            .physical_range_read_operations
            .saturating_add(run.physical_range_read_operations),
        physical_encoded_bytes_read: total
            .physical_encoded_bytes_read
            .saturating_add(run.physical_encoded_bytes_read),
        physical_range_read_time_ns: total
            .physical_range_read_time_ns
            .saturating_add(run.physical_range_read_time_ns),
        codec_decode_operations: total
            .codec_decode_operations
            .saturating_add(run.codec_decode_operations),
        codec_decoded_bytes: total
            .codec_decoded_bytes
            .saturating_add(run.codec_decoded_bytes),
        codec_decode_time_ns: total
            .codec_decode_time_ns
            .saturating_add(run.codec_decode_time_ns),
    }
}

fn stage_progress(stage: unified_source_open::TargetPackageVerificationStage) -> CoalescedProgress {
    let completed_work = match stage {
        unified_source_open::TargetPackageVerificationStage::MetadataOpened => PHASE_WORK_UNITS,
        unified_source_open::TargetPackageVerificationStage::ExactPackageVerified => {
            2 * PHASE_WORK_UNITS
        }
        unified_source_open::TargetPackageVerificationStage::ScientificContentVerified => {
            3 * PHASE_WORK_UNITS
        }
    };
    CoalescedProgress {
        completed_work,
        total_work: TOTAL_WORK_UNITS,
    }
}

fn store_progress(progress: &Mutex<Option<CoalescedProgress>>, candidate: CoalescedProgress) {
    let mut slot = progress.lock().unwrap_or_else(|poison| poison.into_inner());
    if slot.is_none_or(|current| candidate.completed_work >= current.completed_work) {
        *slot = Some(candidate);
    }
}

fn is_cancelled(cancellation: &AtomicBool) -> bool {
    cancellation.load(Ordering::Acquire)
}

fn verification_checkpoint(cancellation: &AtomicBool, throttle: &VerificationThrottle) -> bool {
    throttle.checkpoint(cancellation)
}

fn target_verification_cancelled(
    error: &unified_source_open::TargetPackageVerificationError,
) -> bool {
    matches!(
        error,
        unified_source_open::TargetPackageVerificationError::Cancelled
            | unified_source_open::TargetPackageVerificationError::Exact(
                PackageValidationError::Cancelled,
            )
            | unified_source_open::TargetPackageVerificationError::Scientific(
                ScientificPackageValidationError::Cancelled,
            )
    )
}

fn map_target_verification_error(
    error: &unified_source_open::TargetPackageVerificationError,
) -> OperationFailureCode {
    match error {
        unified_source_open::TargetPackageVerificationError::Cancelled => {
            OperationFailureCode::SourceVerificationReadFailed
        }
        unified_source_open::TargetPackageVerificationError::Reservation
        | unified_source_open::TargetPackageVerificationError::InvalidReservation => {
            OperationFailureCode::SourceVerificationCapacityExceeded
        }
        unified_source_open::TargetPackageVerificationError::Open(error) => {
            map_storage_failure(error, OperationFailureCode::SourceVerificationInvalid)
        }
        unified_source_open::TargetPackageVerificationError::Exact(error) => {
            map_exact_failure(error)
        }
        unified_source_open::TargetPackageVerificationError::Scientific(error) => match error {
            ScientificPackageValidationError::Cancelled => {
                OperationFailureCode::SourceVerificationReadFailed
            }
            ScientificPackageValidationError::Exact(error) => map_exact_failure(error),
            ScientificPackageValidationError::Read(PackageReadError::Cancelled) => {
                OperationFailureCode::SourceVerificationReadFailed
            }
            ScientificPackageValidationError::ArithmeticOverflow { .. }
            | ScientificPackageValidationError::PlatformLength { .. } => {
                OperationFailureCode::SourceVerificationCapacityExceeded
            }
            ScientificPackageValidationError::Read(error) => {
                map_storage_failure(error, OperationFailureCode::SourceVerificationInvalid)
            }
            _ => OperationFailureCode::SourceVerificationInvalid,
        },
    }
}

fn map_exact_failure(error: &PackageValidationError) -> OperationFailureCode {
    match error {
        PackageValidationError::Cancelled => OperationFailureCode::SourceVerificationReadFailed,
        PackageValidationError::AccountingOverflow { .. } => {
            OperationFailureCode::SourceVerificationCapacityExceeded
        }
        PackageValidationError::ObjectLengthMismatch { .. }
        | PackageValidationError::ObjectDigestMismatch { .. }
        | PackageValidationError::StructuralObjectMissing { .. } => {
            OperationFailureCode::SourceChanged
        }
        _ => map_storage_failure(error, OperationFailureCode::SourceVerificationInvalid),
    }
}

fn map_promotion_error(error: &LocalDatasetSourcePromotionError) -> OperationFailureCode {
    match error {
        LocalDatasetSourcePromotionError::StorageContractMismatch => {
            OperationFailureCode::SourceVerificationInvalid
        }
        LocalDatasetSourcePromotionError::ProvisionalGenerationDrift => {
            OperationFailureCode::SourceChanged
        }
        LocalDatasetSourcePromotionError::AuthorityEpochOverflow => {
            OperationFailureCode::SourceVerificationCapacityExceeded
        }
        LocalDatasetSourcePromotionError::MetadataAdmission(_)
        | LocalDatasetSourcePromotionError::InvalidMetadataLease => {
            OperationFailureCode::SourceVerificationCapacityExceeded
        }
        LocalDatasetSourcePromotionError::Currentness(error) => {
            map_storage_failure(error, OperationFailureCode::SourceChanged)
        }
    }
}

fn map_storage_failure(
    error: &(dyn std::error::Error + 'static),
    default: OperationFailureCode,
) -> OperationFailureCode {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(error) = error.downcast_ref::<RangeReadError>() {
            return map_verification_range_failure(error, default);
        }
        if let Some(error) = error.downcast_ref::<DirectoryInventoryError>() {
            return match error {
                DirectoryInventoryError::ObjectCountExceeded { .. }
                | DirectoryInventoryError::DirectoryCountExceeded { .. }
                | DirectoryInventoryError::DirectoryFanOutExceeded { .. } => {
                    OperationFailureCode::SourceVerificationCapacityExceeded
                }
                DirectoryInventoryError::Io { .. } => {
                    OperationFailureCode::SourceVerificationReadFailed
                }
                DirectoryInventoryError::Range(error) => {
                    map_verification_range_failure(error, default)
                }
                DirectoryInventoryError::ManifestAuthorityChanged
                | DirectoryInventoryError::ObjectLengthMismatch { .. }
                | DirectoryInventoryError::MissingFile { .. }
                | DirectoryInventoryError::UnexpectedFile { .. }
                | DirectoryInventoryError::UnexpectedDirectory { .. } => {
                    OperationFailureCode::SourceChanged
                }
                _ => default,
            };
        }
        if matches!(
            error.downcast_ref::<PackageAdmissionError>(),
            Some(PackageAdmissionError::NoSupportedProfile)
        ) || matches!(
            error.downcast_ref::<StorageProfileError>(),
            Some(
                StorageProfileError::ArithmeticOverflow { .. }
                    | StorageProfileError::CeilingExceeded { .. }
                    | StorageProfileError::ExactCountMismatch { .. }
            )
        ) {
            return OperationFailureCode::SourceVerificationCapacityExceeded;
        }
        if let Some(error) = error.downcast_ref::<PackageReadError>()
            && matches!(error, PackageReadError::ObjectLengthMismatch { .. })
        {
            return OperationFailureCode::SourceChanged;
        }
        current = error.source();
    }
    default
}

fn map_verification_range_failure(
    error: &RangeReadError,
    default: OperationFailureCode,
) -> OperationFailureCode {
    match error {
        RangeReadError::RootChanged | RangeReadError::ObjectChanged { .. } => {
            OperationFailureCode::SourceChanged
        }
        RangeReadError::ObjectTooLarge { .. }
        | RangeReadError::InvalidObjectLimit { .. }
        | RangeReadError::RangeOverflow
        | RangeReadError::LengthOverflow => {
            OperationFailureCode::SourceVerificationCapacityExceeded
        }
        RangeReadError::Io { .. } | RangeReadError::ShortRead { .. } => {
            OperationFailureCode::SourceVerificationReadFailed
        }
        _ => default,
    }
}

fn dispose_outcome(outcome: CurrentSourceVerificationOutcome) {
    drop(outcome);
}

fn join_active(
    active: Option<ActiveVerification>,
) -> Result<(), CurrentSourceVerificationServiceError> {
    let Some(mut active) = active else {
        return Ok(());
    };
    active.throttle.request_cancel(active.cancellation.as_ref());
    drop(active.results);
    match active.worker.take() {
        Some(worker) => worker
            .join()
            .map_err(|_| CurrentSourceVerificationServiceError::WorkerPanicked),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante4d_dataset::DatasetSourceId;

    #[test]
    fn verification_stages_map_to_one_fixed_monotonic_scalar() {
        assert_eq!(
            stage_progress(unified_source_open::TargetPackageVerificationStage::MetadataOpened),
            CoalescedProgress {
                completed_work: PHASE_WORK_UNITS,
                total_work: TOTAL_WORK_UNITS,
            }
        );
        assert_eq!(
            stage_progress(
                unified_source_open::TargetPackageVerificationStage::ExactPackageVerified,
            ),
            CoalescedProgress {
                completed_work: 2 * PHASE_WORK_UNITS,
                total_work: TOTAL_WORK_UNITS,
            }
        );
        assert_eq!(
            stage_progress(
                unified_source_open::TargetPackageVerificationStage::ScientificContentVerified,
            ),
            CoalescedProgress {
                completed_work: 3 * PHASE_WORK_UNITS,
                total_work: TOTAL_WORK_UNITS,
            }
        );
    }

    #[test]
    fn coalescing_never_regresses_progress() {
        let slot = Mutex::new(None);
        store_progress(
            &slot,
            CoalescedProgress {
                completed_work: 7,
                total_work: TOTAL_WORK_UNITS,
            },
        );
        store_progress(
            &slot,
            CoalescedProgress {
                completed_work: 3,
                total_work: TOTAL_WORK_UNITS,
            },
        );
        assert_eq!(
            *slot.lock().unwrap(),
            Some(CoalescedProgress {
                completed_work: 7,
                total_work: TOTAL_WORK_UNITS,
            })
        );
    }

    #[test]
    fn interactive_work_pauses_verification_checkpoints_but_not_cancellation() {
        let throttle = Arc::new(VerificationThrottle::default());
        throttle.set_interactive_busy(true);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_throttle = Arc::clone(&throttle);
        let worker_cancelled = Arc::clone(&cancelled);
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            sender
                .send(verification_checkpoint(
                    worker_cancelled.as_ref(),
                    worker_throttle.as_ref(),
                ))
                .unwrap();
        });

        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(10))
                .is_err()
        );
        throttle.set_interactive_busy(false);
        assert!(
            !receiver
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap()
        );
        worker.join().unwrap();

        let throttle = Arc::new(VerificationThrottle::default());
        throttle.set_interactive_busy(true);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_throttle = Arc::clone(&throttle);
        let worker_cancelled = Arc::clone(&cancelled);
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            sender
                .send(verification_checkpoint(
                    worker_cancelled.as_ref(),
                    worker_throttle.as_ref(),
                ))
                .unwrap();
        });
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(10))
                .is_err()
        );
        throttle.request_cancel(cancelled.as_ref());
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap()
        );
        worker.join().unwrap();
    }

    #[test]
    fn evidence_diagnostics_reset_only_while_idle() {
        let mut service = CurrentSourceVerificationService::new();
        service.note_accepted_progress();
        service.note_cancelled_run();
        service.note_failed_run();
        service.note_accepted_success();
        assert_eq!(
            service.diagnostics(),
            CurrentSourceVerificationDiagnostics {
                started_runs: 0,
                accepted_progress_updates: 1,
                cancelled_runs: 1,
                failed_runs: 1,
                accepted_successes: 1,
                ..CurrentSourceVerificationDiagnostics::default()
            }
        );
        service.reset_diagnostics().unwrap();
        assert_eq!(
            service.diagnostics(),
            CurrentSourceVerificationDiagnostics::default()
        );
    }

    #[test]
    fn completed_reader_diagnostics_sum_counters_and_preserve_only_peak_gauges() {
        let first = LocalPackageReadDiagnostics {
            object_open_operations: 3,
            open_object_handles_current: 2,
            open_object_handles_peak: 4,
            object_handle_cache_entries: 1,
            object_handle_cache_peak_entries: 3,
            physical_range_read_operations: 5,
            physical_encoded_bytes_read: 7,
            codec_decode_operations: 11,
            codec_decoded_bytes: 13,
            ..LocalPackageReadDiagnostics::default()
        };
        let second = LocalPackageReadDiagnostics {
            object_open_operations: 17,
            open_object_handles_current: 6,
            open_object_handles_peak: 8,
            object_handle_cache_entries: 5,
            object_handle_cache_peak_entries: 7,
            physical_range_read_operations: 19,
            physical_encoded_bytes_read: 23,
            codec_decode_operations: 29,
            codec_decoded_bytes: 31,
            ..LocalPackageReadDiagnostics::default()
        };

        let total = add_completed_reader_diagnostics(first, second);

        assert_eq!(total.object_open_operations, 20);
        assert_eq!(total.physical_range_read_operations, 24);
        assert_eq!(total.physical_encoded_bytes_read, 30);
        assert_eq!(total.codec_decode_operations, 40);
        assert_eq!(total.codec_decoded_bytes, 44);
        assert_eq!(total.open_object_handles_current, 0);
        assert_eq!(total.object_handle_cache_entries, 0);
        assert_eq!(total.open_object_handles_peak, 8);
        assert_eq!(total.object_handle_cache_peak_entries, 7);
    }

    #[test]
    fn started_worker_failure_is_counted_once_by_the_service() {
        let temp = tempfile::tempdir().unwrap();
        let startup_path = crate::tests::write_target_fixture(temp.path()).unwrap();
        let opened = crate::unified_source_open::open(
            &startup_path,
            mirante4d_settings::ResourcePolicy::default(),
            DatasetSourceId::new(1),
        )
        .unwrap();
        let mut application = mirante4d_application::ApplicationState::new_unbound(
            SourceSessionGeneration::new(1),
            opened.catalog.as_ref().clone(),
            opened.workspace.clone(),
            mirante4d_settings::ResourcePolicy::default(),
        )
        .unwrap();
        application
            .dispatch(mirante4d_application::ApplicationCommand::RequestSourceVerification)
            .unwrap();
        let token = application
            .drain_events(16)
            .into_iter()
            .find_map(|event| match event {
                mirante4d_application::ApplicationEvent::SourceVerificationRequested { token } => {
                    Some(token)
                }
                _ => None,
            })
            .unwrap();

        let mut service = CurrentSourceVerificationService::new();
        service
            .request_verification(
                token,
                temp.path().join("missing-target.m4d"),
                opened.dataset.cpu_ledger_arc(),
                Arc::clone(opened.dataset.local_source().unwrap()),
            )
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let result = loop {
            if let Some(result) = service.try_recv().unwrap() {
                break result;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert!(matches!(
            result.outcome,
            CurrentSourceVerificationOutcome::Failed(_)
        ));
        assert_eq!(service.diagnostics().started_runs, 1);
        assert_eq!(service.diagnostics().failed_runs, 1);
        opened.dataset.request_shutdown().unwrap();
    }

    #[test]
    fn committed_source_promotion_wins_over_late_cancellation() {
        let temp = tempfile::tempdir().unwrap();
        let path = crate::tests::write_target_fixture(temp.path()).unwrap();
        let opened = crate::unified_source_open::open(
            &path,
            mirante4d_settings::ResourcePolicy::default(),
            DatasetSourceId::new(1),
        )
        .unwrap();
        let mut application = mirante4d_application::ApplicationState::new_unbound(
            SourceSessionGeneration::new(1),
            opened.catalog.as_ref().clone(),
            opened.workspace.clone(),
            mirante4d_settings::ResourcePolicy::default(),
        )
        .unwrap();
        application
            .dispatch(mirante4d_application::ApplicationCommand::RequestSourceVerification)
            .unwrap();
        let token = application
            .drain_events(16)
            .into_iter()
            .find_map(|event| match event {
                mirante4d_application::ApplicationEvent::SourceVerificationRequested { token } => {
                    Some(token)
                }
                _ => None,
            })
            .unwrap();

        let mut service = CurrentSourceVerificationService::new();
        service
            .request_verification(
                token.clone(),
                path,
                opened.dataset.cpu_ledger_arc(),
                Arc::clone(opened.dataset.local_source().unwrap()),
            )
            .unwrap();
        assert_eq!(service.diagnostics().started_runs, 1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !service
            .active
            .as_ref()
            .and_then(|active| active.worker.as_ref())
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }

        service.cancel(&token).unwrap();
        let result = service.try_recv().unwrap().unwrap();
        assert!(matches!(
            result.outcome,
            CurrentSourceVerificationOutcome::Prepared(_)
        ));
        assert!(service.active_token().is_none());
        opened.dataset.request_shutdown().unwrap();
    }
}

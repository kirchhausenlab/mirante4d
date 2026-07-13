//! Bounded asynchronous current-source scientific verification.
//!
//! One worker scans the already-open source against D-009, prepares a verified
//! unified runtime, and transfers it only after the canonical reducer accepts
//! the exact source-generation completion.

use std::{
    fmt, io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread::{self, JoinHandle},
};

use mirante4d_application::{
    OperationCompletion, OperationFailureCode, OperationKind, OperationToken,
    SourceSessionGeneration,
};
use mirante4d_data::{
    CurrentDatasetSourceOpenError, CurrentSourceVerificationError, CurrentSourceVerificationPhase,
    CurrentSourceVerificationProgress,
};
use mirante4d_dataset::{CpuByteLedger, DatasetCatalog, DatasetSourceId};
use mirante4d_dataset_runtime::RuntimeFaultCode;
use mirante4d_project_model::{DatasetLocatorHint, DatasetReference};
use mirante4d_settings::ResourcePolicy;

use crate::{
    dataset_requests::DatasetDemandState,
    unified_source_open::{self, UnifiedVerifiedSourceOpenError},
};

const RESULT_CHANNEL_CAPACITY: usize = 1;
const WORKER_NAME: &str = "mirante4d-current-source-verification";
const PHASE_WORK_UNITS: u64 = 1_000_000;
const TOTAL_WORK_UNITS: u64 = 5 * PHASE_WORK_UNITS;

pub(crate) struct CurrentSourceVerificationService {
    active: Option<ActiveVerification>,
    diagnostics: CurrentSourceVerificationDiagnostics,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CurrentSourceVerificationDiagnostics {
    pub(crate) accepted_progress_updates: u64,
    pub(crate) cancelled_runs: u64,
    pub(crate) accepted_successes: u64,
}

struct ActiveVerification {
    token: OperationToken,
    cancellation: Arc<AtomicBool>,
    progress: Arc<Mutex<Option<CoalescedProgress>>>,
    results: Receiver<CurrentSourceVerificationResult>,
    worker: Option<JoinHandle<()>>,
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
    dataset: DatasetDemandState,
    catalog: Arc<DatasetCatalog>,
    dataset_reference: DatasetReference,
    source_generation: SourceSessionGeneration,
}

pub(crate) struct CurrentSourceVerificationRuntimeTransfer {
    pub(crate) dataset: DatasetDemandState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CurrentSourceVerificationServiceError {
    Busy,
    NoActiveOperation,
    OperationTokenMismatch,
    InvalidOperationKind,
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
    pub(crate) const fn new() -> Self {
        Self {
            active: None,
            diagnostics: CurrentSourceVerificationDiagnostics {
                accepted_progress_updates: 0,
                cancelled_runs: 0,
                accepted_successes: 0,
            },
        }
    }

    pub(crate) fn active_token(&self) -> Option<&OperationToken> {
        self.active.as_ref().map(|active| &active.token)
    }

    pub(crate) const fn diagnostics(&self) -> CurrentSourceVerificationDiagnostics {
        self.diagnostics
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

    pub(crate) fn note_accepted_success(&mut self) {
        self.diagnostics.accepted_successes = self.diagnostics.accepted_successes.saturating_add(1);
    }

    pub(crate) fn request_verification(
        &mut self,
        token: OperationToken,
        path: PathBuf,
        resource_policy: ResourcePolicy,
        scan_ledger: Arc<dyn CpuByteLedger>,
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
        let worker_token = token.clone();
        let (result_sender, results) = mpsc::sync_channel(RESULT_CHANNEL_CAPACITY);
        let worker = thread::Builder::new()
            .name(WORKER_NAME.to_owned())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_verification(
                        &worker_token,
                        path,
                        resource_policy,
                        scan_ledger,
                        worker_cancellation.as_ref(),
                        worker_progress,
                    )
                }))
                .unwrap_or(CurrentSourceVerificationOutcome::Failed(
                    OperationFailureCode::SourceVerificationReadFailed,
                ));
                let outcome = if worker_cancellation.load(Ordering::Acquire) {
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
            progress,
            results,
            worker: Some(worker),
        });
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
        active.cancellation.store(true, Ordering::Release);
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
                join_active(self.active.take())?;
                if cancelled {
                    let outcome = std::mem::replace(
                        &mut result.outcome,
                        CurrentSourceVerificationOutcome::Cancelled,
                    );
                    dispose_outcome(outcome);
                }
                Ok(Some(result))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                join_active(self.active.take())?;
                Err(CurrentSourceVerificationServiceError::ResultChannelDisconnected)
            }
        }
    }

    pub(crate) fn shutdown(mut self) -> Result<(), CurrentSourceVerificationServiceError> {
        if let Some(active) = self.active.as_ref() {
            active.cancellation.store(true, Ordering::Release);
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
            active.cancellation.store(true, Ordering::Release);
            let _ = active.worker.take();
        }
    }
}

impl PreparedCurrentSourceVerification {
    pub(crate) fn into_runtime_and_completion(
        self,
    ) -> (
        CurrentSourceVerificationRuntimeTransfer,
        OperationCompletion,
    ) {
        let runtime = CurrentSourceVerificationRuntimeTransfer {
            dataset: self.dataset,
        };
        let completion = OperationCompletion::SourceVerified {
            source_generation: self.source_generation,
            catalog: self.catalog,
            dataset: self.dataset_reference,
        };
        (runtime, completion)
    }
}

fn run_verification(
    token: &OperationToken,
    path: PathBuf,
    resource_policy: ResourcePolicy,
    scan_ledger: Arc<dyn CpuByteLedger>,
    cancellation: &AtomicBool,
    progress: Arc<Mutex<Option<CoalescedProgress>>>,
) -> CurrentSourceVerificationOutcome {
    if is_cancelled(cancellation) {
        return CurrentSourceVerificationOutcome::Cancelled;
    }
    let source_generation = token.source_session_generation();
    let verification = match unified_source_open::verify_current_source(
        &path,
        DatasetSourceId::new(source_generation.get()),
        scan_ledger,
        || is_cancelled(cancellation),
        |reported| store_progress(progress.as_ref(), normalize_progress(reported)),
    ) {
        Ok(verification) => verification,
        Err(unified_source_open::UnifiedCurrentSourceVerificationError::Verification(
            CurrentSourceVerificationError::Cancelled,
        )) => {
            return CurrentSourceVerificationOutcome::Cancelled;
        }
        Err(unified_source_open::UnifiedCurrentSourceVerificationError::Open(error)) => {
            return CurrentSourceVerificationOutcome::Failed(map_source_open_error(&error));
        }
        Err(unified_source_open::UnifiedCurrentSourceVerificationError::Verification(error)) => {
            return CurrentSourceVerificationOutcome::Failed(map_verification_error(&error));
        }
    };
    if is_cancelled(cancellation) {
        return CurrentSourceVerificationOutcome::Cancelled;
    }
    let scientific_content_id = verification.scientific_content_id();
    let preparation_progress = Arc::clone(&progress);
    let opened = match unified_source_open::open_verified(
        &path,
        resource_policy,
        verification,
        cancellation,
        move |reported| {
            store_progress(
                preparation_progress.as_ref(),
                normalize_preparation_progress(reported),
            );
        },
    ) {
        Ok(opened) => opened,
        Err(UnifiedVerifiedSourceOpenError::Verification(
            CurrentSourceVerificationError::Cancelled,
        )) => return CurrentSourceVerificationOutcome::Cancelled,
        Err(error) => {
            return CurrentSourceVerificationOutcome::Failed(map_verified_open_error(&error));
        }
    };
    if is_cancelled(cancellation) {
        let _ = opened.dataset.request_shutdown();
        drop(opened);
        return CurrentSourceVerificationOutcome::Cancelled;
    }
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
    let dataset_reference = DatasetReference::new(scientific_content_id, None, None, locator_hint);
    CurrentSourceVerificationOutcome::Prepared(Box::new(PreparedCurrentSourceVerification {
        dataset: opened.dataset,
        catalog: opened.catalog,
        dataset_reference,
        source_generation,
    }))
}

fn normalize_progress(progress: CurrentSourceVerificationProgress) -> CoalescedProgress {
    normalize_progress_parts(
        progress.phase(),
        progress.completed_units(),
        progress.total_units(),
    )
}

fn normalize_progress_parts(
    phase: CurrentSourceVerificationPhase,
    completed_units: u64,
    total_units: u64,
) -> CoalescedProgress {
    let phase_base = match phase {
        CurrentSourceVerificationPhase::PreInventory => 0,
        CurrentSourceVerificationPhase::ScientificScan => PHASE_WORK_UNITS,
        CurrentSourceVerificationPhase::PostInventory => 2 * PHASE_WORK_UNITS,
    };
    let phase_completed = if total_units == 0 {
        0
    } else {
        let completed = completed_units.min(total_units);
        u64::try_from(
            u128::from(completed) * u128::from(PHASE_WORK_UNITS) / u128::from(total_units),
        )
        .expect("normalized source-verification progress fits u64")
    };
    CoalescedProgress {
        completed_work: phase_base + phase_completed,
        total_work: TOTAL_WORK_UNITS,
    }
}

fn normalize_preparation_progress(
    progress: CurrentSourceVerificationProgress,
) -> CoalescedProgress {
    normalize_preparation_progress_parts(
        progress.phase(),
        progress.completed_units(),
        progress.total_units(),
    )
}

fn normalize_preparation_progress_parts(
    phase: CurrentSourceVerificationPhase,
    completed_units: u64,
    total_units: u64,
) -> CoalescedProgress {
    let phase_base = match phase {
        CurrentSourceVerificationPhase::PreInventory => 3 * PHASE_WORK_UNITS,
        CurrentSourceVerificationPhase::PostInventory => 4 * PHASE_WORK_UNITS,
        CurrentSourceVerificationPhase::ScientificScan => 3 * PHASE_WORK_UNITS,
    };
    let phase_completed = if total_units == 0 {
        0
    } else {
        let completed = completed_units.min(total_units);
        u64::try_from(
            u128::from(completed) * u128::from(PHASE_WORK_UNITS) / u128::from(total_units),
        )
        .expect("normalized source-preparation progress fits u64")
    };
    CoalescedProgress {
        completed_work: phase_base + phase_completed,
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

fn map_source_open_error(error: &CurrentDatasetSourceOpenError) -> OperationFailureCode {
    match error {
        CurrentDatasetSourceOpenError::ManifestMetadata(_) => {
            OperationFailureCode::SourceVerificationReadFailed
        }
        CurrentDatasetSourceOpenError::MetadataAccountingOverflow
        | CurrentDatasetSourceOpenError::MetadataAdmission(_)
        | CurrentDatasetSourceOpenError::InvalidMetadataLease => {
            OperationFailureCode::SourceVerificationCapacityExceeded
        }
        CurrentDatasetSourceOpenError::Dataset(_)
        | CurrentDatasetSourceOpenError::Catalog(_)
        | CurrentDatasetSourceOpenError::LayerOrdinalOverflow => {
            OperationFailureCode::SourceVerificationInvalid
        }
    }
}

fn map_verification_error(error: &CurrentSourceVerificationError) -> OperationFailureCode {
    match error {
        CurrentSourceVerificationError::Cancelled => {
            OperationFailureCode::SourceVerificationReadFailed
        }
        CurrentSourceVerificationError::SourceChanged => OperationFailureCode::SourceChanged,
        CurrentSourceVerificationError::InventoryCapacity
        | CurrentSourceVerificationError::Capacity(_)
        | CurrentSourceVerificationError::InvalidLedgerLease => {
            OperationFailureCode::SourceVerificationCapacityExceeded
        }
        CurrentSourceVerificationError::InventoryIo { .. } => {
            OperationFailureCode::SourceVerificationReadFailed
        }
        CurrentSourceVerificationError::UnsupportedDTypePair { .. }
        | CurrentSourceVerificationError::InvalidSource
        | CurrentSourceVerificationError::Identity(_) => {
            OperationFailureCode::SourceVerificationInvalid
        }
    }
}

fn map_verified_open_error(error: &UnifiedVerifiedSourceOpenError) -> OperationFailureCode {
    match error {
        UnifiedVerifiedSourceOpenError::Verification(error) => map_verification_error(error),
        UnifiedVerifiedSourceOpenError::RuntimeConfiguration(code) => match code {
            RuntimeFaultCode::InvalidConfiguration
            | RuntimeFaultCode::MinimumWorkUnitExceedsBudget
            | RuntimeFaultCode::CapacityExceeded { .. } => {
                OperationFailureCode::SourceVerificationCapacityExceeded
            }
            _ => OperationFailureCode::SourceVerificationReadFailed,
        },
        UnifiedVerifiedSourceOpenError::MissingCpuLedger => {
            OperationFailureCode::SourceVerificationCapacityExceeded
        }
        UnifiedVerifiedSourceOpenError::Runtime(error) => match error.code() {
            RuntimeFaultCode::InvalidConfiguration
            | RuntimeFaultCode::MinimumWorkUnitExceedsBudget
            | RuntimeFaultCode::CapacityExceeded { .. } => {
                OperationFailureCode::SourceVerificationCapacityExceeded
            }
            RuntimeFaultCode::Cancelled | RuntimeFaultCode::StaleGeneration => {
                OperationFailureCode::SourceVerificationReadFailed
            }
            RuntimeFaultCode::SourceRejected
            | RuntimeFaultCode::CorruptResource
            | RuntimeFaultCode::UnsupportedResource => {
                OperationFailureCode::SourceVerificationInvalid
            }
            _ => OperationFailureCode::SourceVerificationReadFailed,
        },
    }
}

fn dispose_outcome(outcome: CurrentSourceVerificationOutcome) {
    if let CurrentSourceVerificationOutcome::Prepared(prepared) = outcome {
        let _ = prepared.dataset.request_shutdown();
        drop(prepared);
    }
}

fn join_active(
    active: Option<ActiveVerification>,
) -> Result<(), CurrentSourceVerificationServiceError> {
    let Some(mut active) = active else {
        return Ok(());
    };
    active.cancellation.store(true, Ordering::Release);
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

    #[test]
    fn phase_progress_maps_to_one_fixed_monotonic_scalar() {
        assert_eq!(
            normalize_progress_parts(CurrentSourceVerificationPhase::PreInventory, 5, 10),
            CoalescedProgress {
                completed_work: 500_000,
                total_work: TOTAL_WORK_UNITS,
            }
        );
        assert_eq!(
            normalize_progress_parts(CurrentSourceVerificationPhase::ScientificScan, 1, 4),
            CoalescedProgress {
                completed_work: 1_250_000,
                total_work: TOTAL_WORK_UNITS,
            }
        );
        assert_eq!(
            normalize_progress_parts(CurrentSourceVerificationPhase::PostInventory, 10, 10),
            CoalescedProgress {
                completed_work: 3 * PHASE_WORK_UNITS,
                total_work: TOTAL_WORK_UNITS,
            }
        );
        assert_eq!(
            normalize_preparation_progress_parts(
                CurrentSourceVerificationPhase::PreInventory,
                1,
                2,
            ),
            CoalescedProgress {
                completed_work: 3_500_000,
                total_work: TOTAL_WORK_UNITS,
            }
        );
        assert_eq!(
            normalize_preparation_progress_parts(
                CurrentSourceVerificationPhase::PostInventory,
                2,
                2,
            ),
            CoalescedProgress {
                completed_work: TOTAL_WORK_UNITS,
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
    fn evidence_diagnostics_reset_only_while_idle() {
        let mut service = CurrentSourceVerificationService::new();
        service.note_accepted_progress();
        service.note_cancelled_run();
        service.note_accepted_success();
        assert_eq!(
            service.diagnostics(),
            CurrentSourceVerificationDiagnostics {
                accepted_progress_updates: 1,
                cancelled_runs: 1,
                accepted_successes: 1,
            }
        );
        service.reset_diagnostics().unwrap();
        assert_eq!(
            service.diagnostics(),
            CurrentSourceVerificationDiagnostics::default()
        );
    }

    #[test]
    fn cancellation_before_receive_wins_over_an_already_published_success() {
        let temp = tempfile::tempdir().unwrap();
        let path = mirante4d_format::write_fixture(
            mirante4d_format::FixtureKind::BasicU16_16Cube,
            temp.path(),
        )
        .unwrap();
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
                mirante4d_settings::ResourcePolicy::default(),
                opened.dataset.cpu_ledger_arc(),
            )
            .unwrap();
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
            CurrentSourceVerificationOutcome::Cancelled
        ));
        assert!(service.active_token().is_none());
        opened.dataset.request_shutdown().unwrap();
    }
}

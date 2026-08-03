//! Native lifetime owner for the accepted TIFF inspection and import workers.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use mirante4d_application::{OperationToken, import_workflow::ImportReviewId};
use mirante4d_dataset_runtime::ImportProgressReservation;
use mirante4d_import_pipeline::{
    ImportCancellation, ImportError, ImportEvent, ImportOptions, ImportReceipt, ImportStage,
    ImportStorageProgress, PublishedImport, SourceFingerprint, TiffInspection,
    TiffInspectionProgress, TiffSource, spawn_tiff_import_worker, spawn_tiff_inspection_worker,
};
use rustix::time::{ClockId, clock_gettime};

#[cfg(test)]
#[derive(Clone)]
struct RegisteredTestImportEventGate {
    stage: ImportStage,
    minimum_completed_work_units: u64,
    state: Arc<(std::sync::Condvar, Mutex<TestImportEventGateState>)>,
}

#[cfg(test)]
#[derive(Default)]
struct TestImportEventGateState {
    reached: bool,
    released: bool,
}

#[cfg(test)]
pub(crate) struct TestImportEventGate {
    state: Arc<(std::sync::Condvar, Mutex<TestImportEventGateState>)>,
}

#[cfg(test)]
impl TestImportEventGate {
    pub(crate) fn wait_until_reached(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let (condition, state) = self.state.as_ref();
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while !state.reached {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (next, wait) = condition
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
            if wait.timed_out() && !state.reached {
                return false;
            }
        }
        true
    }

    pub(crate) fn release(&self) {
        let (condition, state) = self.state.as_ref();
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.released = true;
        condition.notify_all();
    }
}

#[cfg(test)]
impl Drop for TestImportEventGate {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
fn test_import_event_gates() -> &'static Mutex<BTreeMap<PathBuf, RegisteredTestImportEventGate>> {
    static GATES: std::sync::OnceLock<Mutex<BTreeMap<PathBuf, RegisteredTestImportEventGate>>> =
        std::sync::OnceLock::new();
    GATES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(test)]
pub(crate) fn pause_test_import_after_progress(
    destination: PathBuf,
    stage: ImportStage,
    minimum_completed_work_units: u64,
) -> TestImportEventGate {
    let state = Arc::new((
        std::sync::Condvar::new(),
        Mutex::new(TestImportEventGateState::default()),
    ));
    let previous = test_import_event_gates()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            destination,
            RegisteredTestImportEventGate {
                stage,
                minimum_completed_work_units,
                state: Arc::clone(&state),
            },
        );
    assert!(
        previous.is_none(),
        "test import destination already has an event gate"
    );
    TestImportEventGate { state }
}

#[cfg(test)]
fn pause_for_test_import_event(destination: &std::path::Path, event: &ImportEvent) {
    let (stage, completed_work_units) = match event {
        ImportEvent::StageProgress {
            stage,
            completed_work_units,
            ..
        } => (*stage, *completed_work_units),
        _ => return,
    };
    let gate = {
        let mut gates = test_import_event_gates()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(gate) = gates.get(destination) else {
            return;
        };
        if stage != gate.stage || completed_work_units < gate.minimum_completed_work_units {
            return;
        }
        gates
            .remove(destination)
            .expect("observed test gate remains registered")
    };
    let (condition, state) = gate.state.as_ref();
    let mut state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.reached = true;
    condition.notify_all();
    while !state.released {
        state = condition
            .wait(state)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ImportWorkerBusy;

impl std::fmt::Display for ImportWorkerBusy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an import worker is already active")
    }
}

impl std::error::Error for ImportWorkerBusy {}

#[derive(Debug, Clone)]
pub(crate) enum ImportWorkerStatus {
    Idle,
    Inspecting {
        source: TiffSource,
        destination: PathBuf,
        progress: Option<TiffInspectionProgress>,
        cancellation_requested: bool,
    },
    Importing {
        destination: PathBuf,
        latest_event: Option<ImportEvent>,
        storage_progress: Option<Box<ImportStorageProgress>>,
        cancellation_requested: bool,
        elapsed: Duration,
    },
}

impl ImportWorkerStatus {
    pub(crate) const fn is_inspecting(&self) -> bool {
        matches!(self, Self::Inspecting { .. })
    }

    pub(crate) const fn is_importing(&self) -> bool {
        matches!(self, Self::Importing { .. })
    }

    pub(crate) const fn is_active(&self) -> bool {
        !matches!(self, Self::Idle)
    }
}

pub(crate) enum ImportWorkerOutcome<T> {
    Finished(Result<T, ImportError>),
    WorkerStopped,
}

pub(crate) struct InspectionWorkerCompletion {
    pub(crate) cancellation_requested: bool,
    pub(crate) outcome: ImportWorkerOutcome<TiffInspection>,
}

pub(crate) struct ImportExecutionCompletion {
    pub(crate) review_id: ImportReviewId,
    pub(crate) token: Option<OperationToken>,
    pub(crate) destination: PathBuf,
    pub(crate) source_fingerprint: SourceFingerprint,
    pub(crate) reviewed_source_bytes: u64,
    pub(crate) retry_options: Option<ImportOptions>,
    pub(crate) elapsed: Duration,
    pub(crate) outcome: ImportWorkerOutcome<PublishedImport>,
}

pub(crate) enum ImportWorkerCompletion {
    Inspection(Box<InspectionWorkerCompletion>),
    Import(Box<ImportExecutionCompletion>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ImportWorkerDiagnostics {
    pub(crate) emitted_stages: Vec<ImportStage>,
    pub(crate) maximum_completed_by_stage: BTreeMap<ImportStage, u64>,
    pub(crate) progress_updates: u64,
    pub(crate) published_events: u64,
    pub(crate) cancelled_runs: u64,
    pub(crate) successful_runs: u64,
    pub(crate) failed_runs: u64,
    pub(crate) maximum_resumed_work_units: u64,
    pub(crate) maximum_peak_working_bytes: u64,
    pub(crate) maximum_elapsed_ms: u64,
    last_published_timing: Option<ImportPublishedTiming>,
    /// Bounded raw evidence for product automation. Normal UI projection does
    /// not expose receipt identities or counters.
    pub(crate) last_successful_import: Option<SuccessfulImportEvidence>,
}

#[derive(Clone, Debug)]
pub(crate) struct ImportWorkerTimingOrigin {
    pub(crate) started_at: Instant,
    pub(crate) started_at_epoch_ms: u128,
    pub(crate) process_cpu_time_ns: u64,
    pub(crate) review_id: ImportReviewId,
    pub(crate) token: Option<OperationToken>,
    pub(crate) destination: PathBuf,
    pub(crate) source_fingerprint: SourceFingerprint,
    pub(crate) reviewed_source_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SuccessfulImportEvidence {
    pub(crate) review_id: ImportReviewId,
    pub(crate) token: Option<OperationToken>,
    pub(crate) destination: PathBuf,
    pub(crate) source_fingerprint: SourceFingerprint,
    pub(crate) reviewed_source_bytes: u64,
    pub(crate) published_timing: Option<ImportPublishedTiming>,
    pub(crate) receipt: ImportReceipt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImportPublishedTiming {
    pub(crate) published_at: Instant,
    pub(crate) published_at_epoch_ms: u128,
    pub(crate) process_cpu_time_ns: u64,
}

impl ImportWorkerDiagnostics {
    pub(crate) fn maximum_completed_for_name(&self, name: &str) -> u64 {
        self.maximum_completed_by_stage
            .iter()
            .filter(|(stage, _)| stage.name() == name)
            .map(|(_, completed)| *completed)
            .max()
            .unwrap_or(0)
    }

    pub(crate) fn emitted_stage_names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        for stage in &self.emitted_stages {
            let name = stage.name();
            if !names.contains(&name) {
                names.push(name);
            }
        }
        names
    }
}

#[derive(Clone, Default)]
struct ImportWorkerDiagnosticsHandle(Arc<Mutex<ImportWorkerDiagnostics>>);

impl ImportWorkerDiagnosticsHandle {
    fn begin_import(&self) {
        let mut diagnostics = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        diagnostics.last_successful_import = None;
        diagnostics.last_published_timing = None;
    }

    fn record_event(&self, event: &ImportEvent) {
        let mut diagnostics = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match event {
            ImportEvent::StageStarted {
                stage,
                completed_work_units,
                ..
            }
            | ImportEvent::StageProgress {
                stage,
                completed_work_units,
                ..
            } => {
                if !diagnostics.emitted_stages.contains(stage) {
                    diagnostics.emitted_stages.push(*stage);
                }
                let maximum = diagnostics
                    .maximum_completed_by_stage
                    .entry(*stage)
                    .or_default();
                *maximum = (*maximum).max(*completed_work_units);
                if matches!(event, ImportEvent::StageProgress { .. }) {
                    diagnostics.progress_updates = diagnostics.progress_updates.saturating_add(1);
                }
            }
            ImportEvent::StageFinished(timing) => {
                if !diagnostics.emitted_stages.contains(&timing.stage) {
                    diagnostics.emitted_stages.push(timing.stage);
                }
            }
            ImportEvent::StorageProgress(_) => {}
            ImportEvent::Published => {
                diagnostics.published_events = diagnostics.published_events.saturating_add(1);
                diagnostics.last_published_timing = Some(ImportPublishedTiming {
                    published_at: Instant::now(),
                    published_at_epoch_ms: epoch_ms(),
                    process_cpu_time_ns: process_cpu_time_ns(),
                });
            }
        }
    }

    fn record_completion(&self, completion: &ImportExecutionCompletion) {
        let mut diagnostics = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        diagnostics.maximum_elapsed_ms = diagnostics
            .maximum_elapsed_ms
            .max(u64::try_from(completion.elapsed.as_millis()).unwrap_or(u64::MAX));
        match &completion.outcome {
            ImportWorkerOutcome::Finished(Ok(published)) => {
                let receipt = published.receipt();
                diagnostics.successful_runs = diagnostics.successful_runs.saturating_add(1);
                diagnostics.maximum_resumed_work_units = diagnostics
                    .maximum_resumed_work_units
                    .max(receipt.statistics.resumed_work_units);
                diagnostics.maximum_peak_working_bytes = diagnostics
                    .maximum_peak_working_bytes
                    .max(receipt.statistics.peak_working_bytes);
                diagnostics.last_successful_import = Some(SuccessfulImportEvidence {
                    review_id: completion.review_id,
                    token: completion.token.clone(),
                    destination: completion.destination.clone(),
                    source_fingerprint: completion.source_fingerprint,
                    reviewed_source_bytes: completion.reviewed_source_bytes,
                    published_timing: diagnostics.last_published_timing.clone(),
                    receipt: receipt.clone(),
                });
            }
            ImportWorkerOutcome::Finished(Err(ImportError::Cancelled)) => {
                diagnostics.cancelled_runs = diagnostics.cancelled_runs.saturating_add(1);
            }
            ImportWorkerOutcome::Finished(Err(_)) | ImportWorkerOutcome::WorkerStopped => {
                diagnostics.failed_runs = diagnostics.failed_runs.saturating_add(1);
            }
        }
    }

    fn snapshot(&self) -> ImportWorkerDiagnostics {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

pub(crate) struct ImportWorkerService {
    active: Option<ActiveWorker>,
    diagnostics: ImportWorkerDiagnosticsHandle,
    completion_wake: Option<CompletionWake>,
}

type CompletionWake = Arc<dyn Fn() + Send + Sync>;

enum ActiveWorker {
    Inspection(InspectionWorker),
    Import(Box<ImportWorker>),
}

struct InspectionWorker {
    source: TiffSource,
    destination: PathBuf,
    cancellation: ImportCancellation,
    result: Receiver<Result<TiffInspection, ImportError>>,
    worker: Option<JoinHandle<()>>,
    latest_progress: LatestInspectionProgress,
}

#[derive(Clone, Default)]
struct LatestInspectionProgress(Arc<Mutex<Option<TiffInspectionProgress>>>);

impl LatestInspectionProgress {
    fn record(&self, progress: TiffInspectionProgress) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(progress);
    }

    fn get(&self) -> Option<TiffInspectionProgress> {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct ImportWorker {
    review_id: ImportReviewId,
    token: Option<OperationToken>,
    destination: PathBuf,
    retry_options: Option<ImportOptions>,
    cancellation: ImportCancellation,
    latest_progress: LatestImportProgress,
    result: Receiver<Result<PublishedImport, ImportError>>,
    worker: Option<JoinHandle<()>>,
    timing_origin: ImportWorkerTimingOrigin,
    _progress_reservation: Option<ImportProgressReservation>,
}

#[derive(Clone, Default)]
struct LatestImportProgress(Arc<Mutex<LatestImportProgressState>>);

#[derive(Clone, Default)]
struct LatestImportProgressState {
    latest_event: Option<ImportEvent>,
    storage: Option<ImportStorageProgress>,
}

impl LatestImportProgress {
    fn record(&self, event: ImportEvent) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match event {
            ImportEvent::StorageProgress(storage) => state.storage = Some(storage),
            event => state.latest_event = Some(event),
        }
    }

    fn get(&self) -> LatestImportProgressState {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl ImportWorkerService {
    pub(crate) fn new() -> Self {
        Self {
            active: None,
            diagnostics: ImportWorkerDiagnosticsHandle(Arc::new(Mutex::new(
                ImportWorkerDiagnostics {
                    emitted_stages: Vec::new(),
                    maximum_completed_by_stage: BTreeMap::new(),
                    progress_updates: 0,
                    published_events: 0,
                    cancelled_runs: 0,
                    successful_runs: 0,
                    failed_runs: 0,
                    maximum_resumed_work_units: 0,
                    maximum_peak_working_bytes: 0,
                    maximum_elapsed_ms: 0,
                    last_published_timing: None,
                    last_successful_import: None,
                },
            ))),
            completion_wake: None,
        }
    }

    /// Installs the native event-loop wake used after a subsequently started
    /// worker publishes its one terminal result. The worker never calls this
    /// before the bounded result channel accepts that result, so a wake always
    /// makes pollable progress available to composition.
    pub(crate) fn set_completion_wake(&mut self, wake: impl Fn() + Send + Sync + 'static) {
        self.completion_wake = Some(Arc::new(wake));
    }

    pub(crate) fn diagnostics(&self) -> ImportWorkerDiagnostics {
        self.diagnostics.snapshot()
    }

    pub(crate) fn active_import_timing_origin(&self) -> Option<ImportWorkerTimingOrigin> {
        match self.active.as_ref() {
            Some(ActiveWorker::Import(active)) => Some(active.timing_origin.clone()),
            _ => None,
        }
    }

    pub(crate) fn status(&self) -> ImportWorkerStatus {
        match self.active.as_ref() {
            None => ImportWorkerStatus::Idle,
            Some(ActiveWorker::Inspection(active)) => ImportWorkerStatus::Inspecting {
                source: active.source.clone(),
                destination: active.destination.clone(),
                progress: active.latest_progress.get(),
                cancellation_requested: active.cancellation.is_cancelled(),
            },
            Some(ActiveWorker::Import(active)) => {
                let progress = active.latest_progress.get();
                ImportWorkerStatus::Importing {
                    destination: active.destination.clone(),
                    latest_event: progress.latest_event,
                    storage_progress: progress.storage.map(Box::new),
                    cancellation_requested: active.cancellation.is_cancelled(),
                    elapsed: active.timing_origin.started_at.elapsed(),
                }
            }
        }
    }

    pub(crate) fn start_inspection(
        &mut self,
        source: TiffSource,
        destination: PathBuf,
    ) -> Result<(), ImportWorkerBusy> {
        if self.active.is_some() {
            return Err(ImportWorkerBusy);
        }
        let cancellation = ImportCancellation::new();
        let (sender, result) = mpsc::sync_channel(1);
        let completion_wake = self.completion_wake.clone();
        let latest_progress = LatestInspectionProgress::default();
        let worker_progress = latest_progress.clone();
        let worker = spawn_tiff_inspection_worker(
            source.clone(),
            cancellation.clone(),
            move |progress| worker_progress.record(progress),
            move |outcome| publish_inspection_completion(sender, outcome, completion_wake),
        );
        self.active = Some(ActiveWorker::Inspection(InspectionWorker {
            source,
            destination,
            cancellation,
            result,
            worker: Some(worker),
            latest_progress,
        }));
        Ok(())
    }

    pub(crate) fn start_import(
        &mut self,
        review_id: ImportReviewId,
        token: OperationToken,
        options: ImportOptions,
        progress_reservation: ImportProgressReservation,
    ) -> Result<(), ImportWorkerBusy> {
        self.start_import_inner(review_id, Some(token), options, progress_reservation)
    }

    pub(crate) fn start_shell_import(
        &mut self,
        review_id: ImportReviewId,
        options: ImportOptions,
        progress_reservation: ImportProgressReservation,
    ) -> Result<(), ImportWorkerBusy> {
        self.start_import_inner(review_id, None, options, progress_reservation)
    }

    fn start_import_inner(
        &mut self,
        review_id: ImportReviewId,
        token: Option<OperationToken>,
        options: ImportOptions,
        progress_reservation: ImportProgressReservation,
    ) -> Result<(), ImportWorkerBusy> {
        if self.active.is_some() {
            return Err(ImportWorkerBusy);
        }
        let destination = options.destination.clone();
        let source_fingerprint = options.inspection.source_fingerprint;
        let reviewed_source_bytes = options.inspection.source_bytes;
        self.diagnostics.begin_import();
        let cancellation = ImportCancellation::new();
        let latest_progress = LatestImportProgress::default();
        let worker_events = latest_progress.clone();
        let worker_diagnostics = self.diagnostics.clone();
        let (sender, result) = mpsc::sync_channel(1);
        let timing_token = token.clone();
        let timing_destination = destination.clone();
        let started_at_epoch_ms = epoch_ms();
        let process_cpu_time_ns = process_cpu_time_ns();
        let timing_origin = ImportWorkerTimingOrigin {
            started_at: Instant::now(),
            started_at_epoch_ms,
            process_cpu_time_ns,
            review_id,
            token: timing_token,
            destination: timing_destination,
            source_fingerprint,
            reviewed_source_bytes,
        };
        let completion_wake = self.completion_wake.clone();
        let ledger = progress_reservation.ledger();
        #[cfg(test)]
        let worker_destination = destination.clone();
        let worker = spawn_tiff_import_worker(
            options.clone(),
            ledger,
            cancellation.clone(),
            move |event| {
                worker_diagnostics.record_event(&event);
                #[cfg(test)]
                pause_for_test_import_event(&worker_destination, &event);
                worker_events.record(event);
            },
            move |outcome| {
                publish_import_completion(sender, outcome, completion_wake);
            },
        );
        self.active = Some(ActiveWorker::Import(Box::new(ImportWorker {
            review_id,
            token,
            destination,
            retry_options: Some(options),
            cancellation,
            latest_progress,
            result,
            worker: Some(worker),
            timing_origin,
            _progress_reservation: Some(progress_reservation),
        })));
        Ok(())
    }

    pub(crate) fn cancel_inspection(&self) -> bool {
        let Some(ActiveWorker::Inspection(active)) = self.active.as_ref() else {
            return false;
        };
        active.cancellation.cancel();
        true
    }

    pub(crate) fn cancel_import(&self) -> bool {
        let Some(ActiveWorker::Import(active)) = self.active.as_ref() else {
            return false;
        };
        active.cancellation.cancel();
        true
    }

    pub(crate) fn poll_completion(&mut self) -> Option<ImportWorkerCompletion> {
        let ready = match self.active.as_ref()? {
            ActiveWorker::Inspection(active) => match active.result.try_recv() {
                Ok(result) => ReadyCompletion::Inspection(Some(result)),
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => ReadyCompletion::Inspection(None),
            },
            ActiveWorker::Import(active) => match active.result.try_recv() {
                Ok(result) => ReadyCompletion::Import(Box::new(Some(result))),
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => ReadyCompletion::Import(Box::new(None)),
            },
        };
        let active = self
            .active
            .take()
            .expect("a ready import completion has an active worker");
        let completion = finish_worker(active, ready);
        if let ImportWorkerCompletion::Import(import) = &completion {
            self.diagnostics.record_completion(import);
        }
        Some(completion)
    }

    pub(crate) fn shutdown(&mut self) {
        let Some(mut active) = self.active.take() else {
            return;
        };
        active.cancellation().cancel();
        if join_worker(active.take_worker()).is_err() {
            tracing::error!("import worker panicked during shutdown");
        }
    }
}

impl Default for ImportWorkerService {
    fn default() -> Self {
        Self::new()
    }
}

fn publish_inspection_completion(
    sender: SyncSender<Result<TiffInspection, ImportError>>,
    outcome: Result<TiffInspection, ImportError>,
    completion_wake: Option<CompletionWake>,
) {
    publish_terminal_completion(sender, outcome, completion_wake);
}

fn publish_import_completion(
    sender: SyncSender<Result<PublishedImport, ImportError>>,
    outcome: Result<PublishedImport, ImportError>,
    completion_wake: Option<CompletionWake>,
) {
    publish_terminal_completion(sender, outcome, completion_wake);
}

fn publish_terminal_completion<T>(
    sender: SyncSender<T>,
    outcome: T,
    completion_wake: Option<CompletionWake>,
) {
    if sender.send(outcome).is_ok()
        && let Some(wake) = completion_wake
    {
        wake();
    }
}

impl Drop for ImportWorkerService {
    fn drop(&mut self) {
        if self.active.is_some() {
            tracing::warn!("joining an active import worker during drop");
            self.shutdown();
        }
    }
}

enum ReadyCompletion {
    Inspection(Option<Result<TiffInspection, ImportError>>),
    Import(Box<Option<Result<PublishedImport, ImportError>>>),
}

impl ActiveWorker {
    fn cancellation(&self) -> &ImportCancellation {
        match self {
            Self::Inspection(active) => &active.cancellation,
            Self::Import(active) => &active.cancellation,
        }
    }

    fn take_worker(&mut self) -> Option<JoinHandle<()>> {
        match self {
            Self::Inspection(active) => active.worker.take(),
            Self::Import(active) => active.worker.take(),
        }
    }
}

fn finish_worker(active: ActiveWorker, ready: ReadyCompletion) -> ImportWorkerCompletion {
    match (active, ready) {
        (ActiveWorker::Inspection(mut active), ReadyCompletion::Inspection(result)) => {
            let cancellation_requested = active.cancellation.is_cancelled();
            let joined = join_worker(active.worker.take()).is_ok();
            ImportWorkerCompletion::Inspection(Box::new(InspectionWorkerCompletion {
                cancellation_requested,
                outcome: match (result, joined) {
                    (Some(result), true) => ImportWorkerOutcome::Finished(result),
                    _ => ImportWorkerOutcome::WorkerStopped,
                },
            }))
        }
        (ActiveWorker::Import(active), ReadyCompletion::Import(result)) => {
            let mut active = *active;
            let result = *result;
            let joined = join_worker(active.worker.take()).is_ok();
            ImportWorkerCompletion::Import(Box::new(ImportExecutionCompletion {
                review_id: active.review_id,
                token: active.token,
                destination: active.destination,
                source_fingerprint: active.timing_origin.source_fingerprint,
                reviewed_source_bytes: active.timing_origin.reviewed_source_bytes,
                retry_options: active.retry_options.take(),
                elapsed: active.timing_origin.started_at.elapsed(),
                outcome: match (result, joined) {
                    (Some(result), true) => ImportWorkerOutcome::Finished(result),
                    _ => ImportWorkerOutcome::WorkerStopped,
                },
            }))
        }
        _ => unreachable!("completion kind matches the active import worker"),
    }
}

fn join_worker(worker: Option<JoinHandle<()>>) -> Result<(), ()> {
    match worker {
        Some(worker) => worker.join().map_err(|_| ()),
        None => Ok(()),
    }
}

fn process_cpu_time_ns() -> u64 {
    let time = clock_gettime(ClockId::ProcessCPUTime);
    u64::try_from(time.tv_sec)
        .unwrap_or(0)
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::try_from(time.tv_nsec).unwrap_or(0))
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn progress_is_coalesced_to_the_latest_event() {
        let progress = LatestImportProgress::default();
        progress.record(ImportEvent::StageStarted {
            stage: mirante4d_import_pipeline::ImportStage::BaseProduction,
            completed_work_units: 0,
            total_work_units: Some(3),
        });
        progress.record(ImportEvent::StageProgress {
            stage: mirante4d_import_pipeline::ImportStage::BaseProduction,
            completed_work_units: 1,
            total_work_units: 3,
        });
        let latest = ImportEvent::StageStarted {
            stage: mirante4d_import_pipeline::ImportStage::SourceIngest,
            completed_work_units: 0,
            total_work_units: None,
        };
        progress.record(latest.clone());

        assert_eq!(progress.get().latest_event, Some(latest));
    }

    #[test]
    fn storage_progress_is_retained_without_hiding_the_active_stage() {
        let progress = LatestImportProgress::default();
        let stage = ImportEvent::StageStarted {
            stage: ImportStage::BaseProduction,
            completed_work_units: 0,
            total_work_units: Some(9),
        };
        let storage = ImportStorageProgress {
            completed_temporal_units: 3,
            total_temporal_units: 12,
            active_timepoint: Some(1),
            active_channel: Some(0),
            preparing_timepoint: Some(2),
            preparing_channel: Some(0),
            preparing_completed_planes: 3,
            preparing_total_planes: 5,
            prepared_temporal_units: 0,
            temporal_pipeline_width: 2,
            stage_payload_bytes: 4_096,
            remaining_package_output_upper_bound: 8_192,
            unit_scratch_bytes: 1_024,
            decode_ahead_scratch_bytes: 2_048,
            additional_headroom_required_bytes: 4_096,
        };
        progress.record(stage.clone());
        progress.record(ImportEvent::StorageProgress(storage));

        let snapshot = progress.get();
        assert_eq!(snapshot.latest_event, Some(stage));
        assert_eq!(snapshot.storage, Some(storage));
    }

    #[test]
    fn diagnostics_retain_bounded_named_stage_progress() {
        let diagnostics = ImportWorkerDiagnosticsHandle::default();
        diagnostics.record_event(&ImportEvent::StageStarted {
            stage: ImportStage::BaseProduction,
            completed_work_units: 0,
            total_work_units: Some(700),
        });
        diagnostics.record_event(&ImportEvent::StageProgress {
            stage: ImportStage::BaseProduction,
            completed_work_units: 512,
            total_work_units: 700,
        });
        diagnostics.record_event(&ImportEvent::StageFinished(
            mirante4d_import_pipeline::ImportStageTiming {
                stage: ImportStage::BaseProduction,
                wall_time_ns: 1,
                cpu_time_ns: 1,
            },
        ));

        let snapshot = diagnostics.snapshot();
        assert_eq!(snapshot.emitted_stage_names(), vec!["base-production"]);
        assert_eq!(snapshot.maximum_completed_for_name("base-production"), 512);
        assert_eq!(snapshot.progress_updates, 1);
    }

    #[test]
    fn inspection_terminal_delivery_wakes_only_after_result_is_pollable() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let receiver = Arc::new(Mutex::new(receiver));
        let woke = Arc::new(AtomicBool::new(false));
        let wake_receiver = Arc::clone(&receiver);
        let wake_observed = Arc::clone(&woke);
        let wake: CompletionWake = Arc::new(move || {
            assert!(matches!(
                wake_receiver
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .try_recv(),
                Ok(Err(ImportError::Cancelled))
            ));
            wake_observed.store(true, Ordering::SeqCst);
        });

        publish_inspection_completion(sender, Err(ImportError::Cancelled), Some(wake));

        assert!(woke.load(Ordering::SeqCst));
    }

    #[test]
    fn import_terminal_delivery_wakes_only_after_result_is_pollable() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let receiver = Arc::new(Mutex::new(receiver));
        let woke = Arc::new(AtomicBool::new(false));
        let wake_receiver = Arc::clone(&receiver);
        let wake_observed = Arc::clone(&woke);
        let wake: CompletionWake = Arc::new(move || {
            assert!(matches!(
                wake_receiver
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .try_recv(),
                Ok(Err(ImportError::Cancelled))
            ));
            wake_observed.store(true, Ordering::SeqCst);
        });

        publish_import_completion(sender, Err(ImportError::Cancelled), Some(wake));

        assert!(woke.load(Ordering::SeqCst));
    }

    #[test]
    fn cancellation_keeps_the_worker_active_until_its_terminal_result() {
        let cancellation = ImportCancellation::new();
        let worker_cancellation = cancellation.clone();
        let (sender, result) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            while !worker_cancellation.is_cancelled() {
                std::thread::yield_now();
            }
            sender.send(Err(ImportError::Cancelled)).unwrap();
        });
        let mut service = ImportWorkerService {
            active: Some(ActiveWorker::Inspection(InspectionWorker {
                source: TiffSource::single_3d("cancel.tif"),
                destination: PathBuf::from("cancel.m4d"),
                cancellation,
                result,
                worker: Some(worker),
                latest_progress: LatestInspectionProgress::default(),
            })),
            diagnostics: ImportWorkerDiagnosticsHandle::default(),
            completion_wake: None,
        };

        assert!(service.cancel_inspection());
        assert!(matches!(
            service.status(),
            ImportWorkerStatus::Inspecting {
                cancellation_requested: true,
                ..
            }
        ));
        let ImportWorkerCompletion::Inspection(completion) = wait_for_completion(&mut service)
        else {
            panic!("expected inspection completion");
        };
        assert!(matches!(
            completion.outcome,
            ImportWorkerOutcome::Finished(Err(ImportError::Cancelled))
        ));
    }

    #[test]
    fn disconnected_worker_is_joined_once_and_reported_stopped() {
        let joins = Arc::new(AtomicUsize::new(0));
        let worker_joins = Arc::clone(&joins);
        let (sender, result) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            drop(sender);
            worker_joins.fetch_add(1, Ordering::SeqCst);
        });
        let mut service = ImportWorkerService {
            active: Some(ActiveWorker::Inspection(InspectionWorker {
                source: TiffSource::single_3d("stopped.tif"),
                destination: PathBuf::from("stopped.m4d"),
                cancellation: ImportCancellation::new(),
                result,
                worker: Some(worker),
                latest_progress: LatestInspectionProgress::default(),
            })),
            diagnostics: ImportWorkerDiagnosticsHandle::default(),
            completion_wake: None,
        };

        let ImportWorkerCompletion::Inspection(completion) = wait_for_completion(&mut service)
        else {
            panic!("expected inspection completion");
        };
        assert!(matches!(
            completion.outcome,
            ImportWorkerOutcome::WorkerStopped
        ));
        assert_eq!(joins.load(Ordering::SeqCst), 1);
        assert!(service.poll_completion().is_none());
        assert_eq!(joins.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn shutdown_cancels_and_joins_the_active_worker() {
        let joined = Arc::new(AtomicUsize::new(0));
        let worker_joined = Arc::clone(&joined);
        let cancellation = ImportCancellation::new();
        let worker_cancellation = cancellation.clone();
        let (_sender, result) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            while !worker_cancellation.is_cancelled() {
                std::thread::yield_now();
            }
            worker_joined.fetch_add(1, Ordering::SeqCst);
        });
        let mut service = ImportWorkerService {
            active: Some(ActiveWorker::Inspection(InspectionWorker {
                source: TiffSource::single_3d("shutdown.tif"),
                destination: PathBuf::from("shutdown.m4d"),
                cancellation,
                result,
                worker: Some(worker),
                latest_progress: LatestInspectionProgress::default(),
            })),
            diagnostics: ImportWorkerDiagnosticsHandle::default(),
            completion_wake: None,
        };

        service.shutdown();

        assert_eq!(joined.load(Ordering::SeqCst), 1);
        assert!(matches!(service.status(), ImportWorkerStatus::Idle));
    }

    #[test]
    fn drop_cancels_and_joins_an_active_worker() {
        let joined = Arc::new(AtomicUsize::new(0));
        let worker_joined = Arc::clone(&joined);
        let cancellation = ImportCancellation::new();
        let worker_cancellation = cancellation.clone();
        let (_sender, result) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            while !worker_cancellation.is_cancelled() {
                std::thread::yield_now();
            }
            worker_joined.fetch_add(1, Ordering::SeqCst);
        });
        let service = ImportWorkerService {
            active: Some(ActiveWorker::Inspection(InspectionWorker {
                source: TiffSource::single_3d("drop.tif"),
                destination: PathBuf::from("drop.m4d"),
                cancellation,
                result,
                worker: Some(worker),
                latest_progress: LatestInspectionProgress::default(),
            })),
            diagnostics: ImportWorkerDiagnosticsHandle::default(),
            completion_wake: None,
        };

        drop(service);

        assert_eq!(joined.load(Ordering::SeqCst), 1);
    }

    fn wait_for_completion(service: &mut ImportWorkerService) -> ImportWorkerCompletion {
        for _ in 0..10_000 {
            if let Some(completion) = service.poll_completion() {
                return completion;
            }
            std::thread::yield_now();
        }
        panic!("worker did not finish");
    }
}

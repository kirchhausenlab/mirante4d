//! Native lifetime owner for the accepted TIFF inspection and import workers.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, TryRecvError},
    },
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use mirante4d_application::{OperationToken, import_workflow::ImportReviewId};
use mirante4d_dataset::CpuByteLedger;
use mirante4d_import_pipeline::{
    ImportCancellation, ImportError, ImportEvent, ImportOptions, ImportReceipt, ImportStage,
    PublishedImport, SourceFingerprint, TiffInspection, TiffSource, spawn_tiff_import_worker,
    spawn_tiff_inspection_worker,
};
use rustix::time::{ClockId, clock_gettime};

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
        cancellation_requested: bool,
    },
    Importing {
        destination: PathBuf,
        latest_event: Option<ImportEvent>,
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
    pub(crate) source: TiffSource,
    pub(crate) destination: PathBuf,
    pub(crate) cancellation_requested: bool,
    pub(crate) outcome: ImportWorkerOutcome<TiffInspection>,
}

pub(crate) struct ImportExecutionCompletion {
    pub(crate) review_id: ImportReviewId,
    pub(crate) token: OperationToken,
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
    pub(crate) token: OperationToken,
    pub(crate) destination: PathBuf,
    pub(crate) source_fingerprint: SourceFingerprint,
    pub(crate) reviewed_source_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SuccessfulImportEvidence {
    pub(crate) review_id: ImportReviewId,
    pub(crate) token: OperationToken,
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

#[derive(Default)]
pub(crate) struct ImportWorkerService {
    active: Option<ActiveWorker>,
    diagnostics: ImportWorkerDiagnosticsHandle,
}

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
}

struct ImportWorker {
    review_id: ImportReviewId,
    token: OperationToken,
    destination: PathBuf,
    retry_options: Option<ImportOptions>,
    cancellation: ImportCancellation,
    latest_event: LatestImportEvent,
    result: Receiver<Result<PublishedImport, ImportError>>,
    worker: Option<JoinHandle<()>>,
    timing_origin: ImportWorkerTimingOrigin,
}

#[derive(Clone, Default)]
struct LatestImportEvent(Arc<Mutex<Option<ImportEvent>>>);

impl LatestImportEvent {
    fn record(&self, event: ImportEvent) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(event);
    }

    fn get(&self) -> Option<ImportEvent> {
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
        }
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
                cancellation_requested: active.cancellation.is_cancelled(),
            },
            Some(ActiveWorker::Import(active)) => ImportWorkerStatus::Importing {
                destination: active.destination.clone(),
                latest_event: active.latest_event.get(),
                cancellation_requested: active.cancellation.is_cancelled(),
                elapsed: active.timing_origin.started_at.elapsed(),
            },
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
        let worker =
            spawn_tiff_inspection_worker(source.clone(), cancellation.clone(), move |outcome| {
                let _ = sender.send(outcome);
            });
        self.active = Some(ActiveWorker::Inspection(InspectionWorker {
            source,
            destination,
            cancellation,
            result,
            worker: Some(worker),
        }));
        Ok(())
    }

    pub(crate) fn start_import(
        &mut self,
        review_id: ImportReviewId,
        token: OperationToken,
        options: ImportOptions,
        ledger: Arc<dyn CpuByteLedger>,
    ) -> Result<(), ImportWorkerBusy> {
        if self.active.is_some() {
            return Err(ImportWorkerBusy);
        }
        let destination = options.destination.clone();
        let source_fingerprint = options.inspection.source_fingerprint;
        let reviewed_source_bytes = options.inspection.source_bytes;
        self.diagnostics.begin_import();
        let cancellation = ImportCancellation::new();
        let latest_event = LatestImportEvent::default();
        let worker_events = latest_event.clone();
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
        let worker = spawn_tiff_import_worker(
            options.clone(),
            ledger,
            cancellation.clone(),
            move |event| {
                worker_diagnostics.record_event(&event);
                worker_events.record(event);
            },
            move |outcome| {
                let _ = sender.send(outcome);
            },
        );
        self.active = Some(ActiveWorker::Import(Box::new(ImportWorker {
            review_id,
            token,
            destination,
            retry_options: Some(options),
            cancellation,
            latest_event,
            result,
            worker: Some(worker),
            timing_origin,
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
                source: active.source,
                destination: active.destination,
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn progress_is_coalesced_to_the_latest_event() {
        let progress = LatestImportEvent::default();
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
            stage: mirante4d_import_pipeline::ImportStage::SourceScientificIdentity,
            completed_work_units: 0,
            total_work_units: None,
        };
        progress.record(latest.clone());

        assert_eq!(progress.get(), Some(latest));
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
                source: TiffSource::auto("cancel.tif"),
                destination: PathBuf::from("cancel.m4d"),
                cancellation,
                result,
                worker: Some(worker),
            })),
            diagnostics: ImportWorkerDiagnosticsHandle::default(),
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
                source: TiffSource::auto("stopped.tif"),
                destination: PathBuf::from("stopped.m4d"),
                cancellation: ImportCancellation::new(),
                result,
                worker: Some(worker),
            })),
            diagnostics: ImportWorkerDiagnosticsHandle::default(),
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
                source: TiffSource::auto("shutdown.tif"),
                destination: PathBuf::from("shutdown.m4d"),
                cancellation,
                result,
                worker: Some(worker),
            })),
            diagnostics: ImportWorkerDiagnosticsHandle::default(),
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
                source: TiffSource::auto("drop.tif"),
                destination: PathBuf::from("drop.m4d"),
                cancellation,
                result,
                worker: Some(worker),
            })),
            diagnostics: ImportWorkerDiagnosticsHandle::default(),
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

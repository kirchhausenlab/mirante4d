//! Native lifetime owner for the accepted TIFF inspection and import workers.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, TryRecvError},
    },
    thread::JoinHandle,
};

use mirante4d_application::OperationToken;
use mirante4d_dataset::CpuByteLedger;
use mirante4d_import_pipeline::{
    ImportCancellation, ImportError, ImportEvent, ImportOptions, ImportReceipt, TiffInspection,
    TiffSource, spawn_tiff_import_worker, spawn_tiff_inspection_worker,
};

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
    pub(crate) token: OperationToken,
    pub(crate) destination: PathBuf,
    pub(crate) retry_options: Option<ImportOptions>,
    pub(crate) outcome: ImportWorkerOutcome<ImportReceipt>,
}

pub(crate) enum ImportWorkerCompletion {
    Inspection(Box<InspectionWorkerCompletion>),
    Import(Box<ImportExecutionCompletion>),
}

#[derive(Default)]
pub(crate) struct ImportWorkerService {
    active: Option<ActiveWorker>,
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
    token: OperationToken,
    destination: PathBuf,
    retry_options: Option<ImportOptions>,
    cancellation: ImportCancellation,
    latest_event: LatestImportEvent,
    result: Receiver<Result<ImportReceipt, ImportError>>,
    worker: Option<JoinHandle<()>>,
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
    pub(crate) const fn new() -> Self {
        Self { active: None }
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
        token: OperationToken,
        options: ImportOptions,
        ledger: Arc<dyn CpuByteLedger>,
    ) -> Result<(), ImportWorkerBusy> {
        if self.active.is_some() {
            return Err(ImportWorkerBusy);
        }
        let destination = options.destination.clone();
        let cancellation = ImportCancellation::new();
        let latest_event = LatestImportEvent::default();
        let worker_events = latest_event.clone();
        let (sender, result) = mpsc::sync_channel(1);
        let worker = spawn_tiff_import_worker(
            options.clone(),
            ledger,
            cancellation.clone(),
            move |event| worker_events.record(event),
            move |outcome| {
                let _ = sender.send(outcome);
            },
        );
        self.active = Some(ActiveWorker::Import(Box::new(ImportWorker {
            token,
            destination,
            retry_options: Some(options),
            cancellation,
            latest_event,
            result,
            worker: Some(worker),
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
                Ok(result) => ReadyCompletion::Import(Some(result)),
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => ReadyCompletion::Import(None),
            },
        };
        let active = self
            .active
            .take()
            .expect("a ready import completion has an active worker");
        Some(finish_worker(active, ready))
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
        if let Some(active) = self.active.as_ref() {
            active.cancellation().cancel();
            tracing::error!("active import worker dropped without explicit shutdown");
        }
    }
}

enum ReadyCompletion {
    Inspection(Option<Result<TiffInspection, ImportError>>),
    Import(Option<Result<ImportReceipt, ImportError>>),
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
            let joined = join_worker(active.worker.take()).is_ok();
            ImportWorkerCompletion::Import(Box::new(ImportExecutionCompletion {
                token: active.token,
                destination: active.destination,
                retry_options: active.retry_options.take(),
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn progress_is_coalesced_to_the_latest_event() {
        let progress = LatestImportEvent::default();
        progress.record(ImportEvent::Producing {
            completed_work_units: 1,
            total_work_units: 3,
        });
        progress.record(ImportEvent::HashingScience);
        progress.record(ImportEvent::Publishing);

        assert_eq!(progress.get(), Some(ImportEvent::Publishing));
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
        };

        service.shutdown();

        assert_eq!(joined.load(Ordering::SeqCst), 1);
        assert!(matches!(service.status(), ImportWorkerStatus::Idle));
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

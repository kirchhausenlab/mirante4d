//! Bounded asynchronous service for opening the active M4D dataset profile.
//!
//! It owns no durable source identity and retains no path after its single
//! active operation completes.

use std::{
    fmt, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread::{self, JoinHandle},
};

use mirante4d_application::{
    ContentAddressOrigin, OperationCompletion, OperationFailureCode, OperationKind, OperationToken,
    RenderCoordinationState, SourceSessionGeneration, UnboundWorkspace,
};
use mirante4d_dataset::{DatasetCatalog, DatasetSourceId};
use mirante4d_dataset_runtime::{ProcessCpuBroker, RuntimeFault, RuntimeFaultCode};
use mirante4d_project_model::DatasetReference;
use mirante4d_settings::ResourcePolicy;
use mirante4d_storage::{
    ControlError, DirectoryInventoryError, LocalDatasetSourceOpenError, PackageAdmissionError,
    PublishedScientificPackageTransfer, RangeReadError, ScientificPublicationTransferEvidence,
    StorageProfileError,
};

use crate::{
    analysis_session::AnalysisProductRuntime,
    dataset_requests::DatasetDemandState,
    unified_source_open::{self, UnifiedOpenedSource, UnifiedPublishedSourceOpenError},
};

const RESULT_CHANNEL_CAPACITY: usize = 1;
const WORKER_NAME: &str = "mirante4d-current-source-open";

pub(crate) struct CurrentSourceOpenService {
    active: Option<ActiveOpen>,
    completed_imported_publication_transfer: Option<CompletedImportedPublicationTransferEvidence>,
    cpu_broker: ProcessCpuBroker,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompletedImportedPublicationTransferEvidence {
    destination: PathBuf,
    execution: ScientificPublicationTransferEvidence,
}

impl CompletedImportedPublicationTransferEvidence {
    pub(crate) fn destination(&self) -> &Path {
        &self.destination
    }

    pub(crate) const fn execution(&self) -> ScientificPublicationTransferEvidence {
        self.execution
    }
}

/// A linear request to replace the current source.
///
/// External paths use structural admission and lazy per-read integrity.
/// Imported packages carry the publication-bound self-consistency authority;
/// that variant can never degrade into an external path request.
pub(crate) enum CurrentSourceOpenRequest {
    External(PathBuf),
    ImportedPublication(Box<PublishedScientificPackageTransfer>),
}

impl CurrentSourceOpenRequest {
    const fn origin(&self) -> CurrentSourceOpenOrigin {
        match self {
            Self::External(_) => CurrentSourceOpenOrigin::External,
            Self::ImportedPublication(_) => CurrentSourceOpenOrigin::ImportedPublication,
        }
    }

    #[cfg(test)]
    pub(crate) fn selected_path(&self) -> &Path {
        match self {
            Self::External(path) => path,
            Self::ImportedPublication(transfer) => transfer.destination(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CurrentSourceOpenOrigin {
    External,
    ImportedPublication,
}

struct ActiveOpen {
    token: OperationToken,
    cancellation: Arc<AtomicBool>,
    results: Receiver<CurrentSourceOpenResult>,
    worker: Option<JoinHandle<()>>,
}

pub(crate) struct CurrentSourceOpenResult {
    pub(crate) token: OperationToken,
    pub(crate) origin: CurrentSourceOpenOrigin,
    pub(crate) outcome: CurrentSourceOpenOutcome,
}

pub(crate) enum CurrentSourceOpenOutcome {
    Prepared(Box<PreparedCurrentSourceOpen>),
    Cancelled,
    Failed(OperationFailureCode),
}

/// All current-runtime and canonical values prepared by one successful open.
///
/// No input path or broad application state is retained here.
pub(crate) struct PreparedCurrentSourceOpen {
    pub(crate) dataset: DatasetDemandState,
    pub(crate) render_coordination: RenderCoordinationState,
    pub(crate) analysis_runtime: AnalysisProductRuntime,
    pub(crate) catalog: Arc<DatasetCatalog>,
    pub(crate) workspace: UnboundWorkspace,
    source_generation: SourceSessionGeneration,
    source_reference: DatasetReference,
    content_address_origin: ContentAddressOrigin,
    imported_publication_transfer: Option<CompletedImportedPublicationTransferEvidence>,
}

/// Current-runtime values installed only after the application reducer accepts
/// the matching `DatasetOpened` completion.
pub(crate) struct CurrentSourceRuntimeTransfer {
    pub(crate) dataset: DatasetDemandState,
    pub(crate) render_coordination: RenderCoordinationState,
    pub(crate) analysis_runtime: AnalysisProductRuntime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CurrentSourceOpenServiceError {
    Busy,
    NoActiveOperation,
    OperationTokenMismatch,
    InvalidOperationKind,
    WorkerSpawnFailed(io::ErrorKind),
    WorkerPanicked,
    ResultChannelDisconnected,
}

impl fmt::Display for CurrentSourceOpenServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => formatter.write_str("a current-source open operation is already active"),
            Self::NoActiveOperation => formatter.write_str("no current-source open is active"),
            Self::OperationTokenMismatch => {
                formatter.write_str("current-source cancellation token does not match")
            }
            Self::InvalidOperationKind => {
                formatter.write_str("current-source open requires a dataset-open token")
            }
            Self::WorkerSpawnFailed(kind) => {
                write!(formatter, "failed to spawn current-source worker: {kind:?}")
            }
            Self::WorkerPanicked => formatter.write_str("current-source worker panicked"),
            Self::ResultChannelDisconnected => {
                formatter.write_str("current-source result channel disconnected")
            }
        }
    }
}

impl std::error::Error for CurrentSourceOpenServiceError {}

impl CurrentSourceOpenService {
    pub(crate) fn new(cpu_broker: ProcessCpuBroker) -> Self {
        Self {
            active: None,
            completed_imported_publication_transfer: None,
            cpu_broker,
        }
    }

    pub(crate) fn active_token(&self) -> Option<&OperationToken> {
        self.active.as_ref().map(|active| &active.token)
    }

    pub(crate) fn completed_imported_publication_transfer(
        &self,
    ) -> Option<&CompletedImportedPublicationTransferEvidence> {
        self.completed_imported_publication_transfer.as_ref()
    }

    pub(crate) fn request_open(
        &mut self,
        token: OperationToken,
        request: CurrentSourceOpenRequest,
        resource_policy: ResourcePolicy,
    ) -> Result<(), CurrentSourceOpenServiceError> {
        if self.active.is_some() {
            return Err(CurrentSourceOpenServiceError::Busy);
        }
        if token.kind() != OperationKind::DatasetOpen {
            return Err(CurrentSourceOpenServiceError::InvalidOperationKind);
        }
        self.completed_imported_publication_transfer = None;

        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation);
        let worker_token = token.clone();
        let cpu_broker = self.cpu_broker.clone();
        let origin = request.origin();
        let (result_sender, results) = mpsc::sync_channel(RESULT_CHANNEL_CAPACITY);
        let worker = thread::Builder::new()
            .name(WORKER_NAME.to_owned())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_open(
                        &worker_token,
                        request,
                        resource_policy,
                        cpu_broker,
                        worker_cancellation.as_ref(),
                    )
                }))
                .unwrap_or(CurrentSourceOpenOutcome::Failed(
                    OperationFailureCode::DatasetReadFailed,
                ));
                let outcome = if worker_cancellation.load(Ordering::Acquire)
                    && matches!(&outcome, CurrentSourceOpenOutcome::Prepared(_))
                {
                    CurrentSourceOpenOutcome::Cancelled
                } else {
                    outcome
                };
                let _ = result_sender.send(CurrentSourceOpenResult {
                    token: worker_token,
                    origin,
                    outcome,
                });
            })
            .map_err(|error| CurrentSourceOpenServiceError::WorkerSpawnFailed(error.kind()))?;

        self.active = Some(ActiveOpen {
            token,
            cancellation,
            results,
            worker: Some(worker),
        });
        Ok(())
    }

    pub(crate) fn cancel(
        &self,
        token: &OperationToken,
    ) -> Result<(), CurrentSourceOpenServiceError> {
        let active = self
            .active
            .as_ref()
            .ok_or(CurrentSourceOpenServiceError::NoActiveOperation)?;
        if &active.token != token {
            return Err(CurrentSourceOpenServiceError::OperationTokenMismatch);
        }
        active.cancellation.store(true, Ordering::Release);
        Ok(())
    }

    pub(crate) fn try_recv(
        &mut self,
    ) -> Result<Option<CurrentSourceOpenResult>, CurrentSourceOpenServiceError> {
        let receive = match self.active.as_ref() {
            None => return Ok(None),
            Some(active) => active.results.try_recv(),
        };
        match receive {
            Ok(result) => {
                join_active(self.active.take())?;
                self.completed_imported_publication_transfer = match &result.outcome {
                    CurrentSourceOpenOutcome::Prepared(prepared) => {
                        prepared.imported_publication_transfer.clone()
                    }
                    CurrentSourceOpenOutcome::Cancelled | CurrentSourceOpenOutcome::Failed(_) => {
                        None
                    }
                };
                Ok(Some(result))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                join_active(self.active.take())?;
                Err(CurrentSourceOpenServiceError::ResultChannelDisconnected)
            }
        }
    }

    pub(crate) fn shutdown(mut self) -> Result<(), CurrentSourceOpenServiceError> {
        if let Some(active) = self.active.as_ref() {
            active.cancellation.store(true, Ordering::Release);
        }
        join_active(self.active.take())
    }
}

impl Drop for CurrentSourceOpenService {
    fn drop(&mut self) {
        if let Some(mut active) = self.active.take() {
            active.cancellation.store(true, Ordering::Release);
            // UI-thread Drop is deliberately non-blocking. The composition
            // root must call `shutdown` when a joined stop is required.
            let _ = active.worker.take();
        }
    }
}

impl PreparedCurrentSourceOpen {
    pub(crate) fn into_runtime_and_completion(
        self,
    ) -> (CurrentSourceRuntimeTransfer, OperationCompletion) {
        let runtime = CurrentSourceRuntimeTransfer {
            dataset: self.dataset,
            render_coordination: self.render_coordination,
            analysis_runtime: self.analysis_runtime,
        };
        let completion = OperationCompletion::DatasetOpened {
            source_generation: self.source_generation,
            catalog: self.catalog,
            workspace: Box::new(self.workspace),
            dataset: self.source_reference,
            content_address_origin: self.content_address_origin,
        };
        (runtime, completion)
    }
}

fn join_active(active: Option<ActiveOpen>) -> Result<(), CurrentSourceOpenServiceError> {
    let Some(mut active) = active else {
        return Ok(());
    };
    active.cancellation.store(true, Ordering::Release);
    drop(active.results);
    match active.worker.take() {
        Some(worker) => worker
            .join()
            .map_err(|_| CurrentSourceOpenServiceError::WorkerPanicked),
        None => Ok(()),
    }
}

fn run_open(
    token: &OperationToken,
    request: CurrentSourceOpenRequest,
    resource_policy: ResourcePolicy,
    cpu_broker: ProcessCpuBroker,
    cancellation: &AtomicBool,
) -> CurrentSourceOpenOutcome {
    let Some(next_generation) = token
        .source_session_generation()
        .get()
        .checked_add(1)
        .map(SourceSessionGeneration::new)
    else {
        return CurrentSourceOpenOutcome::Failed(OperationFailureCode::DatasetCapacityExceeded);
    };
    if is_cancelled(cancellation) {
        return CurrentSourceOpenOutcome::Cancelled;
    }

    let (opened, imported_publication_transfer) = match request {
        CurrentSourceOpenRequest::External(path) => {
            let opened = match unified_source_open::open_with_broker(
                &path,
                resource_policy,
                DatasetSourceId::new(next_generation.get()),
                cpu_broker.clone(),
            ) {
                Ok(opened) => opened,
                Err(error) => {
                    return CurrentSourceOpenOutcome::Failed(map_open_failure(&error, &path));
                }
            };
            (opened, None)
        }
        CurrentSourceOpenRequest::ImportedPublication(transfer) => {
            let destination = transfer.destination().to_path_buf();
            let (capability, execution) = match (*transfer).consume(|| is_cancelled(cancellation)) {
                Ok(consumed) => consumed,
                Err(_) if is_cancelled(cancellation) => {
                    return CurrentSourceOpenOutcome::Cancelled;
                }
                Err(error) => {
                    tracing::warn!(%error, destination = %destination.display(), "published package capability transfer was rejected");
                    return CurrentSourceOpenOutcome::Failed(OperationFailureCode::DatasetInvalid);
                }
            };
            let self_consistent = match unified_source_open::open_published_with_broker(
                resource_policy,
                capability,
                cpu_broker,
            ) {
                Ok(opened) => opened,
                Err(error) => {
                    return CurrentSourceOpenOutcome::Failed(map_published_open_failure(&error));
                }
            };
            let opened = match unified_source_open::prepare_published_current_source(
                self_consistent,
            ) {
                Ok(opened) => opened,
                Err(error) => {
                    tracing::warn!(%error, destination = %destination.display(), "published imported source state preparation failed");
                    return CurrentSourceOpenOutcome::Failed(OperationFailureCode::DatasetInvalid);
                }
            };
            if normalize_existing_path(opened.dataset.selected_path())
                != normalize_existing_path(&destination)
            {
                let _ = opened.dataset.request_shutdown();
                return CurrentSourceOpenOutcome::Failed(OperationFailureCode::DatasetInvalid);
            }
            let imported_publication_transfer = CompletedImportedPublicationTransferEvidence {
                destination,
                execution,
            };
            (opened, Some(imported_publication_transfer))
        }
    };
    if is_cancelled(cancellation) {
        return CurrentSourceOpenOutcome::Cancelled;
    }

    let UnifiedOpenedSource {
        startup_diagnostics: _,
        catalog,
        workspace,
        dataset,
        render_coordination,
        analysis_runtime,
        source_reference,
        content_address_origin,
    } = opened;

    CurrentSourceOpenOutcome::Prepared(Box::new(PreparedCurrentSourceOpen {
        dataset,
        render_coordination,
        analysis_runtime,
        catalog,
        workspace,
        source_generation: next_generation,
        source_reference,
        content_address_origin,
        imported_publication_transfer,
    }))
}

fn normalize_existing_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn is_cancelled(cancellation: &AtomicBool) -> bool {
    cancellation.load(Ordering::Acquire)
}

fn map_open_failure(error: &anyhow::Error, path: &Path) -> OperationFailureCode {
    if let Err(error) = std::fs::metadata(path) {
        return map_io_kind(error.kind());
    }

    for cause in error.chain() {
        if let Some(error) = cause.downcast_ref::<io::Error>() {
            return map_io_kind(error.kind());
        }
        if let Some(error) = cause.downcast_ref::<LocalDatasetSourceOpenError>() {
            return map_source_adapter_error(error);
        }
        if let Some(error) = cause.downcast_ref::<RangeReadError>() {
            return map_range_error(error);
        }
        if let Some(error) = cause.downcast_ref::<ControlError>()
            && matches!(
                error,
                ControlError::InvalidControlObject {
                    reason: "the profile fixed schema, compatibility, capability, or path values are invalid",
                    ..
                }
            )
        {
            return OperationFailureCode::DatasetUnsupported;
        }
        if let Some(error) = cause.downcast_ref::<RuntimeFault>() {
            return map_runtime_fault(error);
        }
    }
    OperationFailureCode::DatasetReadFailed
}

fn map_published_open_failure(error: &UnifiedPublishedSourceOpenError) -> OperationFailureCode {
    match error {
        UnifiedPublishedSourceOpenError::RuntimeConfiguration(
            RuntimeFaultCode::InvalidConfiguration
            | RuntimeFaultCode::MinimumWorkUnitExceedsBudget
            | RuntimeFaultCode::CapacityExceeded { .. },
        )
        | UnifiedPublishedSourceOpenError::MissingCpuLedger => {
            OperationFailureCode::DatasetCapacityExceeded
        }
        UnifiedPublishedSourceOpenError::MissingLocalSource => {
            OperationFailureCode::DatasetReadFailed
        }
        UnifiedPublishedSourceOpenError::DemandPlanner(error) => {
            tracing::error!(%error, "current-source data-plane worker could not start");
            OperationFailureCode::DatasetReadFailed
        }
        UnifiedPublishedSourceOpenError::Adapter(
            LocalDatasetSourceOpenError::MetadataAccountingOverflow
            | LocalDatasetSourceOpenError::MetadataAdmission(_)
            | LocalDatasetSourceOpenError::InvalidMetadataLease,
        ) => OperationFailureCode::DatasetCapacityExceeded,
        UnifiedPublishedSourceOpenError::Adapter(
            LocalDatasetSourceOpenError::Catalog(_)
            | LocalDatasetSourceOpenError::MetadataInvariant { .. },
        ) => OperationFailureCode::DatasetInvalid,
        UnifiedPublishedSourceOpenError::Adapter(LocalDatasetSourceOpenError::Admission(error)) => {
            map_admission_error(error)
        }
        UnifiedPublishedSourceOpenError::Runtime(error) => map_runtime_fault(error),
        UnifiedPublishedSourceOpenError::RuntimeConfiguration(_) => {
            OperationFailureCode::DatasetReadFailed
        }
    }
}

fn map_runtime_fault(error: &RuntimeFault) -> OperationFailureCode {
    match error.code() {
        RuntimeFaultCode::MinimumWorkUnitExceedsBudget
        | RuntimeFaultCode::CapacityExceeded { .. }
        | RuntimeFaultCode::QueueFull => OperationFailureCode::DatasetCapacityExceeded,
        RuntimeFaultCode::UnsupportedResource => OperationFailureCode::DatasetUnsupported,
        RuntimeFaultCode::SourceRejected | RuntimeFaultCode::CorruptResource => {
            OperationFailureCode::DatasetInvalid
        }
        _ => OperationFailureCode::DatasetReadFailed,
    }
}

fn map_io_kind(kind: io::ErrorKind) -> OperationFailureCode {
    match kind {
        io::ErrorKind::NotFound => OperationFailureCode::DatasetNotFound,
        io::ErrorKind::PermissionDenied => OperationFailureCode::DatasetPermissionDenied,
        _ => OperationFailureCode::DatasetReadFailed,
    }
}

fn map_source_adapter_error(error: &LocalDatasetSourceOpenError) -> OperationFailureCode {
    match error {
        LocalDatasetSourceOpenError::MetadataAccountingOverflow
        | LocalDatasetSourceOpenError::MetadataAdmission(_)
        | LocalDatasetSourceOpenError::InvalidMetadataLease => {
            OperationFailureCode::DatasetCapacityExceeded
        }
        LocalDatasetSourceOpenError::Admission(error) => map_admission_error(error),
        LocalDatasetSourceOpenError::Catalog(_)
        | LocalDatasetSourceOpenError::MetadataInvariant { .. } => {
            OperationFailureCode::DatasetInvalid
        }
    }
}

fn map_admission_error(error: &PackageAdmissionError) -> OperationFailureCode {
    match error {
        PackageAdmissionError::Inventory(error) => match error {
            DirectoryInventoryError::ObjectCountExceeded { .. }
            | DirectoryInventoryError::DirectoryCountExceeded { .. }
            | DirectoryInventoryError::DirectoryFanOutExceeded { .. } => {
                OperationFailureCode::DatasetCapacityExceeded
            }
            DirectoryInventoryError::Io { kind, .. } => map_io_kind(*kind),
            DirectoryInventoryError::Range(error) => map_range_error(error),
            _ => OperationFailureCode::DatasetInvalid,
        },
        PackageAdmissionError::Profile(
            StorageProfileError::ArithmeticOverflow { .. }
            | StorageProfileError::CeilingExceeded { .. }
            | StorageProfileError::ExactCountMismatch { .. },
        ) => OperationFailureCode::DatasetCapacityExceeded,
        PackageAdmissionError::Profile(_) => OperationFailureCode::DatasetInvalid,
        _ => OperationFailureCode::DatasetInvalid,
    }
}

fn map_range_error(error: &RangeReadError) -> OperationFailureCode {
    match error {
        RangeReadError::UnsupportedPlatform => OperationFailureCode::DatasetUnsupported,
        RangeReadError::ObjectTooLarge { .. }
        | RangeReadError::InvalidObjectLimit { .. }
        | RangeReadError::RangeOverflow
        | RangeReadError::LengthOverflow => OperationFailureCode::DatasetCapacityExceeded,
        RangeReadError::Io { kind, .. } => map_io_kind(*kind),
        RangeReadError::ShortRead { .. } => OperationFailureCode::DatasetReadFailed,
        _ => OperationFailureCode::DatasetInvalid,
    }
}

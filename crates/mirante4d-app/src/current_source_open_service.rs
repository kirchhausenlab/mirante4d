//! Bounded asynchronous bridge for opening the current M4D dataset profile.
//!
//! This is an interim WP-07B composition service. It owns no durable source
//! identity and retains no path after its single active operation completes.

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
    OperationCompletion, OperationFailureCode, OperationKind, OperationToken,
    SourceSessionGeneration, UnboundWorkspace,
};
use mirante4d_dataset::{DatasetCatalog, DatasetSourceId};
use mirante4d_dataset_runtime::{RuntimeFault, RuntimeFaultCode};
use mirante4d_domain::ShapeError;
use mirante4d_renderer::RenderError;
use mirante4d_settings::ResourcePolicy;
use mirante4d_storage::{
    ControlError, DirectoryInventoryError, LocalDatasetSourceOpenError, PackageAdmissionError,
    RangeReadError, StorageProfileError,
};

use crate::{
    current_runtime::{analysis::AnalysisProductRuntime, render::CurrentRenderRuntime},
    dataset_requests::DatasetDemandState,
    unified_source_open::{self, UnifiedOpenedSource},
};

const RESULT_CHANNEL_CAPACITY: usize = 1;
const WORKER_NAME: &str = "mirante4d-current-source-open";

pub(crate) struct CurrentSourceOpenService {
    active: Option<ActiveOpen>,
}

struct ActiveOpen {
    token: OperationToken,
    cancellation: Arc<AtomicBool>,
    results: Receiver<CurrentSourceOpenResult>,
    worker: Option<JoinHandle<()>>,
}

pub(crate) struct CurrentSourceOpenResult {
    pub(crate) token: OperationToken,
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
    pub(crate) render_runtime: CurrentRenderRuntime,
    pub(crate) analysis_runtime: AnalysisProductRuntime,
    pub(crate) catalog: Arc<DatasetCatalog>,
    pub(crate) workspace: UnboundWorkspace,
    source_generation: SourceSessionGeneration,
}

/// Current-runtime values installed only after the application reducer accepts
/// the matching `DatasetOpened` completion.
pub(crate) struct CurrentSourceRuntimeTransfer {
    pub(crate) dataset: DatasetDemandState,
    pub(crate) render_runtime: CurrentRenderRuntime,
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
    pub(crate) const fn new() -> Self {
        Self { active: None }
    }

    pub(crate) fn active_token(&self) -> Option<&OperationToken> {
        self.active.as_ref().map(|active| &active.token)
    }

    pub(crate) fn request_open(
        &mut self,
        token: OperationToken,
        path: PathBuf,
        resource_policy: ResourcePolicy,
    ) -> Result<(), CurrentSourceOpenServiceError> {
        if self.active.is_some() {
            return Err(CurrentSourceOpenServiceError::Busy);
        }
        if token.kind() != OperationKind::DatasetOpen {
            return Err(CurrentSourceOpenServiceError::InvalidOperationKind);
        }

        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation);
        let worker_token = token.clone();
        let (result_sender, results) = mpsc::sync_channel(RESULT_CHANNEL_CAPACITY);
        let worker = thread::Builder::new()
            .name(WORKER_NAME.to_owned())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_open(
                        &worker_token,
                        path,
                        resource_policy,
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

impl Default for CurrentSourceOpenService {
    fn default() -> Self {
        Self::new()
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
            render_runtime: self.render_runtime,
            analysis_runtime: self.analysis_runtime,
        };
        let completion = OperationCompletion::DatasetOpened {
            source_generation: self.source_generation,
            catalog: self.catalog,
            workspace: Box::new(self.workspace),
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
    path: PathBuf,
    resource_policy: ResourcePolicy,
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

    let opened = match unified_source_open::open(
        &path,
        resource_policy,
        DatasetSourceId::new(next_generation.get()),
    ) {
        Ok(opened) => opened,
        Err(error) => {
            return CurrentSourceOpenOutcome::Failed(map_open_failure(&error, &path));
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
        render_runtime,
        analysis_runtime,
    } = opened;

    CurrentSourceOpenOutcome::Prepared(Box::new(PreparedCurrentSourceOpen {
        dataset,
        render_runtime,
        analysis_runtime,
        catalog,
        workspace,
        source_generation: next_generation,
    }))
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
        if let Some(error) = cause.downcast_ref::<RenderError>() {
            return map_render_error(error);
        }
        if let Some(error) = cause.downcast_ref::<RuntimeFault>() {
            return map_runtime_fault(error);
        }
    }
    OperationFailureCode::DatasetReadFailed
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
        PackageAdmissionError::NoSupportedProfile => OperationFailureCode::DatasetCapacityExceeded,
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

fn map_render_error(error: &RenderError) -> OperationFailureCode {
    match error {
        RenderError::DimensionTooLarge { .. }
        | RenderError::Shape(ShapeError::ElementCountOverflow) => {
            OperationFailureCode::DatasetCapacityExceeded
        }
        _ => OperationFailureCode::DatasetInvalid,
    }
}

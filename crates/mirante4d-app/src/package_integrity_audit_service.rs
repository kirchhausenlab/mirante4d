//! Explicit, cancellable package self-consistency audit.
//!
//! This service never starts itself, never promotes or revokes a source, and
//! never participates in ordinary rendering, project, analysis, or export
//! admission. It only reports what an owner-requested scan observed.

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
    OperationKind, OperationToken, PackageIntegrityAuditFailure, PackageIntegrityAuditProgress,
    PackageIntegrityAuditReport, PackageIntegrityAuditStage,
};
use mirante4d_dataset::CpuByteLedger;
use mirante4d_storage::{
    PackageOpenError, PackageReadError, PackageValidationError, RangeReadError,
    ScientificPackageValidationError, ScientificValidationProgressStage,
};

use crate::unified_source_open::{self, TargetPackageAuditError, TargetPackageAuditProgress};

const RESULT_CHANNEL_CAPACITY: usize = 1;
const WORKER_NAME: &str = "mirante4d-package-integrity-audit";

pub(crate) struct PackageIntegrityAuditService {
    active: Option<ActiveAudit>,
    diagnostics: PackageIntegrityAuditDiagnostics,
}

struct ActiveAudit {
    token: OperationToken,
    cancellation: Arc<AtomicBool>,
    progress: Arc<Mutex<Option<PackageIntegrityAuditProgress>>>,
    results: Receiver<PackageIntegrityAuditResult>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PackageIntegrityAuditDiagnostics {
    pub(crate) started_runs: u64,
    pub(crate) progress_updates: u64,
    pub(crate) cancelled_runs: u64,
    pub(crate) failed_runs: u64,
    pub(crate) completed_runs: u64,
}

pub(crate) struct PackageIntegrityAuditResult {
    pub(crate) token: OperationToken,
    pub(crate) outcome: PackageIntegrityAuditOutcome,
}

pub(crate) enum PackageIntegrityAuditOutcome {
    Completed(PackageIntegrityAuditReport),
    Failed(PackageIntegrityAuditFailure),
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PackageIntegrityAuditServiceError {
    Busy,
    NoActiveOperation,
    OperationTokenMismatch,
    InvalidOperationKind,
    WorkerSpawnFailed(io::ErrorKind),
    WorkerPanicked,
    ResultChannelDisconnected,
}

impl fmt::Display for PackageIntegrityAuditServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => formatter.write_str("a package integrity audit is already active"),
            Self::NoActiveOperation => formatter.write_str("no package integrity audit is active"),
            Self::OperationTokenMismatch => {
                formatter.write_str("package integrity audit token does not match")
            }
            Self::InvalidOperationKind => {
                formatter.write_str("operation token is not a package integrity audit")
            }
            Self::WorkerSpawnFailed(kind) => write!(
                formatter,
                "package integrity audit worker could not start: {kind:?}"
            ),
            Self::WorkerPanicked => formatter.write_str("package integrity audit worker panicked"),
            Self::ResultChannelDisconnected => {
                formatter.write_str("package integrity audit result channel disconnected")
            }
        }
    }
}

impl PackageIntegrityAuditService {
    pub(crate) const fn new() -> Self {
        Self {
            active: None,
            diagnostics: PackageIntegrityAuditDiagnostics {
                started_runs: 0,
                progress_updates: 0,
                cancelled_runs: 0,
                failed_runs: 0,
                completed_runs: 0,
            },
        }
    }

    pub(crate) fn active_token(&self) -> Option<&OperationToken> {
        self.active.as_ref().map(|active| &active.token)
    }

    pub(crate) const fn diagnostics(&self) -> PackageIntegrityAuditDiagnostics {
        self.diagnostics
    }

    pub(crate) fn reset_diagnostics(&mut self) -> Result<(), PackageIntegrityAuditServiceError> {
        if self.active.is_some() {
            return Err(PackageIntegrityAuditServiceError::Busy);
        }
        self.diagnostics = PackageIntegrityAuditDiagnostics::default();
        Ok(())
    }

    pub(crate) fn request(
        &mut self,
        token: OperationToken,
        path: PathBuf,
        scan_ledger: Arc<dyn CpuByteLedger>,
    ) -> Result<(), PackageIntegrityAuditServiceError> {
        if self.active.is_some() {
            return Err(PackageIntegrityAuditServiceError::Busy);
        }
        if token.kind() != OperationKind::PackageIntegrityAudit {
            return Err(PackageIntegrityAuditServiceError::InvalidOperationKind);
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation);
        let progress = Arc::new(Mutex::new(None));
        let worker_progress = Arc::clone(&progress);
        let worker_token = token.clone();
        let (sender, results) = mpsc::sync_channel(RESULT_CHANNEL_CAPACITY);
        let worker = thread::Builder::new()
            .name(WORKER_NAME.to_owned())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_audit(
                        path,
                        scan_ledger,
                        worker_cancellation.as_ref(),
                        worker_progress,
                    )
                }))
                .unwrap_or_else(|_| {
                    PackageIntegrityAuditOutcome::Failed(PackageIntegrityAuditFailure::new(
                        None,
                        None,
                        "package integrity audit worker panicked",
                    ))
                });
                let outcome = if worker_cancellation.load(Ordering::Acquire) {
                    PackageIntegrityAuditOutcome::Cancelled
                } else {
                    outcome
                };
                let _ = sender.send(PackageIntegrityAuditResult {
                    token: worker_token,
                    outcome,
                });
            })
            .map_err(|error| PackageIntegrityAuditServiceError::WorkerSpawnFailed(error.kind()))?;
        self.active = Some(ActiveAudit {
            token,
            cancellation,
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
    ) -> Result<(), PackageIntegrityAuditServiceError> {
        let active = self
            .active
            .as_ref()
            .ok_or(PackageIntegrityAuditServiceError::NoActiveOperation)?;
        if &active.token != token {
            return Err(PackageIntegrityAuditServiceError::OperationTokenMismatch);
        }
        active.cancellation.store(true, Ordering::Release);
        Ok(())
    }

    pub(crate) fn take_progress(
        &mut self,
    ) -> Option<(OperationToken, PackageIntegrityAuditProgress)> {
        let active = self.active.as_ref()?;
        let progress = active
            .progress
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()?;
        self.diagnostics.progress_updates = self.diagnostics.progress_updates.saturating_add(1);
        Some((active.token.clone(), progress))
    }

    pub(crate) fn try_recv(
        &mut self,
    ) -> Result<Option<PackageIntegrityAuditResult>, PackageIntegrityAuditServiceError> {
        let receive = match self.active.as_ref() {
            Some(active) => active.results.try_recv(),
            None => return Ok(None),
        };
        match receive {
            Ok(result) => {
                join_active(self.active.take())?;
                match &result.outcome {
                    PackageIntegrityAuditOutcome::Completed(_) => {
                        self.diagnostics.completed_runs =
                            self.diagnostics.completed_runs.saturating_add(1);
                    }
                    PackageIntegrityAuditOutcome::Failed(_) => {
                        self.diagnostics.failed_runs =
                            self.diagnostics.failed_runs.saturating_add(1);
                    }
                    PackageIntegrityAuditOutcome::Cancelled => {
                        self.diagnostics.cancelled_runs =
                            self.diagnostics.cancelled_runs.saturating_add(1);
                    }
                }
                Ok(Some(result))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                join_active(self.active.take())?;
                self.diagnostics.failed_runs = self.diagnostics.failed_runs.saturating_add(1);
                Err(PackageIntegrityAuditServiceError::ResultChannelDisconnected)
            }
        }
    }

    pub(crate) fn shutdown(mut self) -> Result<(), PackageIntegrityAuditServiceError> {
        if let Some(active) = self.active.as_ref() {
            active.cancellation.store(true, Ordering::Release);
        }
        join_active(self.active.take())
    }
}

impl Default for PackageIntegrityAuditService {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PackageIntegrityAuditService {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            active.cancellation.store(true, Ordering::Release);
            // A detached audit owns only read-only resources and observes the
            // cancellation flag at every validator checkpoint.
            drop(active.worker);
        }
    }
}

fn run_audit(
    path: PathBuf,
    scan_ledger: Arc<dyn CpuByteLedger>,
    cancellation: &AtomicBool,
    progress: Arc<Mutex<Option<PackageIntegrityAuditProgress>>>,
) -> PackageIntegrityAuditOutcome {
    store_progress(
        &progress,
        PackageIntegrityAuditStage::ExactObjectBytes,
        0,
        0,
        0,
        0,
    );
    let mut objects_hashed = 0;
    let mut bytes_hashed = 0;
    let capability = match unified_source_open::audit_target_package(
        path,
        scan_ledger,
        || cancellation.load(Ordering::Acquire),
        |update| match update {
            TargetPackageAuditProgress::Exact(exact) => {
                objects_hashed = exact.objects_hashed();
                bytes_hashed = exact.bytes_hashed();
                store_progress(
                    &progress,
                    PackageIntegrityAuditStage::ExactObjectBytes,
                    objects_hashed,
                    bytes_hashed,
                    0,
                    0,
                );
            }
            TargetPackageAuditProgress::Scientific(scientific) => {
                let stage = match scientific.stage() {
                    ScientificValidationProgressStage::CanonicalBaseContent => {
                        PackageIntegrityAuditStage::CanonicalBaseContent
                    }
                    ScientificValidationProgressStage::PyramidAccelerationFacts => {
                        PackageIntegrityAuditStage::PyramidAccelerationFacts
                    }
                };
                store_progress(
                    &progress,
                    stage,
                    objects_hashed,
                    bytes_hashed,
                    scientific.decoded_bricks(),
                    scientific.decoded_bytes(),
                );
            }
        },
    ) {
        Ok(capability) => capability,
        Err(TargetPackageAuditError::Cancelled) => {
            return PackageIntegrityAuditOutcome::Cancelled;
        }
        Err(error) => {
            return PackageIntegrityAuditOutcome::Failed(map_failure(error));
        }
    };
    let report = capability.validation_report();
    PackageIntegrityAuditOutcome::Completed(PackageIntegrityAuditReport::new(
        capability.scientific_content_id(),
        capability.objects_hashed(),
        capability.bytes_hashed(),
        report
            .brick_reads()
            .saturating_add(report.pyramid_fact_brick_reads()),
        report
            .decoded_bytes()
            .saturating_add(report.pyramid_fact_decoded_bytes()),
    ))
}

fn store_progress(
    target: &Mutex<Option<PackageIntegrityAuditProgress>>,
    stage: PackageIntegrityAuditStage,
    objects_hashed: u64,
    bytes_hashed: u64,
    decoded_bricks: u64,
    decoded_bytes: u64,
) {
    let progress = PackageIntegrityAuditProgress::new(
        stage,
        objects_hashed,
        bytes_hashed,
        decoded_bricks,
        decoded_bytes,
    );
    *target.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(progress);
}

fn map_failure(error: TargetPackageAuditError) -> PackageIntegrityAuditFailure {
    let stage = match &error {
        TargetPackageAuditError::Open(_)
        | TargetPackageAuditError::Reservation
        | TargetPackageAuditError::InvalidReservation
        | TargetPackageAuditError::Exact(_) => PackageIntegrityAuditStage::ExactObjectBytes,
        TargetPackageAuditError::Scientific { stage, .. } => match stage {
            ScientificValidationProgressStage::CanonicalBaseContent => {
                PackageIntegrityAuditStage::CanonicalBaseContent
            }
            ScientificValidationProgressStage::PyramidAccelerationFacts => {
                PackageIntegrityAuditStage::PyramidAccelerationFacts
            }
        },
        TargetPackageAuditError::Cancelled => {
            unreachable!("cancelled audit is handled before failure mapping")
        }
    };
    let object = match &error {
        TargetPackageAuditError::Open(error) => object_from_open_error(error),
        TargetPackageAuditError::Exact(error) => object_from_exact_error(error),
        TargetPackageAuditError::Scientific {
            source: ScientificPackageValidationError::Exact(error),
            ..
        } => object_from_exact_error(error),
        TargetPackageAuditError::Scientific {
            source: ScientificPackageValidationError::Read(error),
            ..
        } => object_from_read_error(error),
        _ => None,
    };
    PackageIntegrityAuditFailure::new(Some(stage), object, error_reason(&error))
}

fn object_from_open_error(error: &PackageOpenError) -> Option<String> {
    match error {
        PackageOpenError::ManifestPageLengthMismatch { path, .. }
        | PackageOpenError::ManifestPageDigestMismatch { path }
        | PackageOpenError::MissingMetadataObject { path }
        | PackageOpenError::UnexpectedMetadataObject { path }
        | PackageOpenError::MetadataKindMismatch { path }
        | PackageOpenError::ObjectLengthMismatch { path, .. }
        | PackageOpenError::ObjectDigestMismatch { path } => Some(path.clone()),
        _ => None,
    }
}

fn object_from_exact_error(error: &PackageValidationError) -> Option<String> {
    match error {
        PackageValidationError::ObjectLengthMismatch { path, .. }
        | PackageValidationError::ObjectDigestMismatch { path }
        | PackageValidationError::StructuralObjectMissing { path } => Some(path.clone()),
        _ => None,
    }
}

fn object_from_read_error(error: &PackageReadError) -> Option<String> {
    match error {
        PackageReadError::Range(error) => object_from_range_error(error),
        PackageReadError::DescriptorKindMismatch { path, .. }
        | PackageReadError::MissingRequiredShardDescriptor { path, .. }
        | PackageReadError::ObjectLengthMismatch { path, .. }
        | PackageReadError::MissingRequiredInnerPayload { path, .. } => Some(path.clone()),
        _ => None,
    }
}

fn object_from_range_error(error: &RangeReadError) -> Option<String> {
    match error {
        RangeReadError::Symlink { path }
        | RangeReadError::NonDirectoryComponent { path }
        | RangeReadError::NonRegularObject { path }
        | RangeReadError::Hardlink { path, .. }
        | RangeReadError::EscapedRoot { path }
        | RangeReadError::ObjectChanged { path }
        | RangeReadError::ObjectTooLarge { path, .. }
        | RangeReadError::ShortRead { path, .. }
        | RangeReadError::Io { path, .. } => Some(path.clone()),
        _ => None,
    }
}

fn error_reason(error: &TargetPackageAuditError) -> String {
    match error {
        TargetPackageAuditError::Cancelled => "package integrity audit was cancelled".to_owned(),
        TargetPackageAuditError::Open(error) => error.to_string(),
        TargetPackageAuditError::Reservation => {
            "audit working-memory reservation was refused".to_owned()
        }
        TargetPackageAuditError::InvalidReservation => {
            "audit working-memory reservation violated its contract".to_owned()
        }
        TargetPackageAuditError::Exact(error) => error.to_string(),
        TargetPackageAuditError::Scientific { source, .. } => source.to_string(),
    }
}

fn join_active(active: Option<ActiveAudit>) -> Result<(), PackageIntegrityAuditServiceError> {
    if let Some(mut active) = active
        && let Some(worker) = active.worker.take()
        && worker.join().is_err()
    {
        return Err(PackageIntegrityAuditServiceError::WorkerPanicked);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_failure_preserves_the_actual_scientific_stage_and_object() {
        let failure = map_failure(TargetPackageAuditError::Scientific {
            stage: ScientificValidationProgressStage::PyramidAccelerationFacts,
            source: ScientificPackageValidationError::Read(
                PackageReadError::ObjectLengthMismatch {
                    path: "images/i00000000/s01/c/0/0/0/0/0".to_owned(),
                    expected: 128,
                    actual: 64,
                },
            ),
        });

        assert_eq!(
            failure.stage(),
            Some(PackageIntegrityAuditStage::PyramidAccelerationFacts)
        );
        assert_eq!(failure.object(), Some("images/i00000000/s01/c/0/0/0/0/0"));
        assert!(failure.reason().contains("128"));
        assert!(failure.reason().contains("64"));
    }

    #[test]
    fn exact_audit_failure_preserves_its_manifest_object() {
        let failure = map_failure(TargetPackageAuditError::Exact(
            PackageValidationError::ObjectDigestMismatch {
                path: "m4d/display.json".to_owned(),
            },
        ));

        assert_eq!(
            failure.stage(),
            Some(PackageIntegrityAuditStage::ExactObjectBytes)
        );
        assert_eq!(failure.object(), Some("m4d/display.json"));
    }
}

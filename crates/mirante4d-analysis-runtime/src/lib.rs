//! Bounded, nonblocking coordination for exact analysis work.
//!
//! This crate does not own threads, dataset I/O, or artifact persistence. It
//! turns a typed analysis plan into at most two shared-runtime requests,
//! reduces ready leases in deterministic plan order, and hands a complete
//! artifact bundle to the application for transactional project commit.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use mirante4d_analysis_core::{
    AnalysisAccumulator, AnalysisArtifactSet, AnalysisDefinition, AnalysisError, AnalysisOperation,
    AnalysisPlan,
};
use mirante4d_dataset_runtime::{
    AccountedCpuLease, AccountedResourceLease, CancellationGeneration, RequestPriority,
    RequestTicket, ResourceRequest, RuntimeFault, RuntimeFaultCode, RuntimeOutcome,
    RuntimeRequestId,
};
use mirante4d_project_model::ProjectRevisionId;
use thiserror::Error;

/// The hard upper bound on submitted or buffered blocks for one analysis.
pub const ANALYSIS_REQUEST_WINDOW: usize = 2;

// Canonical artifact JSON is much smaller than this allowance. Keeping the
// estimate deliberately plain makes it easy to audit and reserves enough room
// for both provenance-bearing outputs and worst-case scalar spellings.
const RESULT_FIXED_BYTES: u64 = 32 * 1024;
const FULL_SUMMARY_BYTES_PER_TIMEPOINT: u64 = 4 * 1024;
const BOX_ROI_BYTES_PER_TIMEPOINT: u64 = 3 * 1024;

/// A conservative CPU reservation for the complete canonical artifact bundle.
///
/// The shared dataset runtime remains the accounting authority. Callers should
/// acquire this many analysis bytes there before starting the run.
pub fn required_result_bytes(definition: &AnalysisDefinition) -> Result<u64, AnalysisRuntimeError> {
    let timepoints = definition
        .time_end_exclusive()
        .checked_sub(definition.time_start())
        .ok_or(AnalysisRuntimeError::ResultSizeOverflow)?;
    let bytes_per_timepoint = match definition.operation() {
        AnalysisOperation::FullIntensitySummary => FULL_SUMMARY_BYTES_PER_TIMEPOINT,
        AnalysisOperation::BoxRoiIntensityStatistics => BOX_ROI_BYTES_PER_TIMEPOINT,
    };
    timepoints
        .checked_mul(bytes_per_timepoint)
        .and_then(|bytes| bytes.checked_add(RESULT_FIXED_BYTES))
        .ok_or(AnalysisRuntimeError::ResultSizeOverflow)
}

/// One plan block ready to submit to the shared dataset runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalysisDemand {
    ordinal: u64,
    request: ResourceRequest,
}

impl AnalysisDemand {
    pub const fn ordinal(self) -> u64 {
        self.ordinal
    }

    pub const fn request(self) -> ResourceRequest {
        self.request
    }
}

/// Small progress snapshot suitable for application state and UI display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalysisProgress {
    completed_blocks: u64,
    total_blocks: u64,
    submitted_blocks: u64,
    in_flight_blocks: usize,
    buffered_blocks: usize,
}

impl AnalysisProgress {
    pub const fn completed_blocks(self) -> u64 {
        self.completed_blocks
    }

    pub const fn total_blocks(self) -> u64 {
        self.total_blocks
    }

    pub const fn submitted_blocks(self) -> u64 {
        self.submitted_blocks
    }

    pub const fn in_flight_blocks(self) -> usize {
        self.in_flight_blocks
    }

    pub const fn buffered_blocks(self) -> usize {
        self.buffered_blocks
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisStatus {
    Idle,
    Running,
    PendingCommit,
}

/// A terminal analysis failure. In every case, partial reduction state and
/// leases have already been dropped before this value is returned.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AnalysisFailure {
    #[error("a dataset request failed: {0}")]
    Dataset(RuntimeFault),
    #[error("exact analysis reduction failed: {0}")]
    Reduction(AnalysisError),
    #[error(
        "the complete artifact bundle used {actual_bytes} bytes, exceeding its {reserved_bytes}-byte result reservation"
    )]
    ResultReservationExceeded {
        actual_bytes: u64,
        reserved_bytes: u64,
    },
}

/// The effect of one shared-runtime completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionEvent {
    Progressed(AnalysisProgress),
    PendingCommitReady,
    Cancelled,
    Failed(AnalysisFailure),
    /// The run has already ended or moved to a newer generation.
    IgnoredRetired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelEvent {
    CancelledRunning,
    DiscardedPendingCommit,
    Idle,
}

/// A complete artifact bundle that has not yet been committed to a project.
///
/// The private CPU lease remains alive with the bundle. Dropping this value
/// after either commit or rejection releases the shared-runtime charge.
#[derive(Debug)]
pub struct PendingAnalysisCommit {
    definition: AnalysisDefinition,
    project_revision: ProjectRevisionId,
    generation: CancellationGeneration,
    required_result_bytes: u64,
    artifacts: AnalysisArtifactSet,
    _result_charge: AccountedCpuLease,
}

impl PendingAnalysisCommit {
    pub const fn definition(&self) -> &AnalysisDefinition {
        &self.definition
    }

    pub const fn project_revision(&self) -> ProjectRevisionId {
        self.project_revision
    }

    pub const fn generation(&self) -> CancellationGeneration {
        self.generation
    }

    pub const fn required_result_bytes(&self) -> u64 {
        self.required_result_bytes
    }

    pub const fn artifacts(&self) -> &AnalysisArtifactSet {
        &self.artifacts
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AnalysisRuntimeError {
    #[error("an analysis is already running or waiting for commit")]
    Busy,
    #[error("no analysis is running")]
    NotRunning,
    #[error("the analysis result-size calculation overflowed")]
    ResultSizeOverflow,
    #[error(
        "the result reservation has {reserved_bytes} bytes but the analysis requires {required_bytes} bytes"
    )]
    InsufficientResultReservation {
        reserved_bytes: u64,
        required_bytes: u64,
    },
    #[error("a new analysis must use a later cancellation generation")]
    GenerationNotAdvanced,
    #[error("cancellation must advance exactly one generation")]
    InvalidCancellationStep,
    #[error("the submitted demand does not match the next planned analysis block")]
    DemandMismatch,
    #[error("the runtime ticket does not match its submitted analysis demand")]
    TicketMismatch,
    #[error("the runtime ticket is already registered")]
    DuplicateTicket,
    #[error("the completion ticket is not registered with this analysis")]
    UnexpectedTicket,
    #[error("analysis planning failed: {0}")]
    Planning(#[from] AnalysisError),
    #[error("dataset runtime generation contract failed: {0}")]
    Generation(#[from] RuntimeFaultCode),
}

#[derive(Debug, Default)]
pub struct AnalysisRuntime {
    state: RuntimeState,
    last_started_generation: Option<CancellationGeneration>,
}

#[derive(Debug, Default)]
enum RuntimeState {
    #[default]
    Idle,
    Running(Box<ActiveAnalysis>),
    PendingCommit(Box<PendingAnalysisCommit>),
}

#[derive(Debug)]
struct ActiveAnalysis {
    definition: AnalysisDefinition,
    plan: AnalysisPlan,
    accumulator: AnalysisAccumulator,
    project_revision: ProjectRevisionId,
    generation: CancellationGeneration,
    required_result_bytes: u64,
    result_charge: AccountedCpuLease,
    next_submission_ordinal: u64,
    tickets: BTreeMap<RuntimeRequestId, RegisteredRequest>,
    buffered: BTreeMap<u64, AccountedResourceLease>,
}

#[derive(Debug, Clone, Copy)]
struct RegisteredRequest {
    ordinal: u64,
    ticket: RequestTicket,
}

impl AnalysisRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub const fn status(&self) -> AnalysisStatus {
        match self.state {
            RuntimeState::Idle => AnalysisStatus::Idle,
            RuntimeState::Running(_) => AnalysisStatus::Running,
            RuntimeState::PendingCommit(_) => AnalysisStatus::PendingCommit,
        }
    }

    pub fn start(
        &mut self,
        definition: AnalysisDefinition,
        project_revision: ProjectRevisionId,
        generation: CancellationGeneration,
        result_charge: AccountedCpuLease,
    ) -> Result<(), AnalysisRuntimeError> {
        if !matches!(self.state, RuntimeState::Idle) {
            return Err(AnalysisRuntimeError::Busy);
        }
        if let Some(previous) = self.last_started_generation {
            if previous.scope() != generation.scope() {
                return Err(RuntimeFaultCode::CancellationScopeMismatch {
                    request_scope: generation.scope(),
                    current_scope: previous.scope(),
                }
                .into());
            }
            if generation.get() <= previous.get() {
                return Err(AnalysisRuntimeError::GenerationNotAdvanced);
            }
        }

        let required_result_bytes = required_result_bytes(&definition)?;
        if result_charge.accounted_bytes() < required_result_bytes {
            return Err(AnalysisRuntimeError::InsufficientResultReservation {
                reserved_bytes: result_charge.accounted_bytes(),
                required_bytes: required_result_bytes,
            });
        }
        let plan = AnalysisPlan::new(definition.clone())?;
        let accumulator = AnalysisAccumulator::new(plan.clone());
        self.state = RuntimeState::Running(Box::new(ActiveAnalysis {
            definition,
            plan,
            accumulator,
            project_revision,
            generation,
            required_result_bytes,
            result_charge,
            next_submission_ordinal: 0,
            tickets: BTreeMap::new(),
            buffered: BTreeMap::new(),
        }));
        self.last_started_generation = Some(generation);
        Ok(())
    }

    /// Returns the next request without reserving it. If submission is
    /// rejected by the shared runtime, callers may retry this same demand.
    pub fn next_demand(&self) -> Option<AnalysisDemand> {
        let RuntimeState::Running(active) = &self.state else {
            return None;
        };
        if active.occupied_window() >= ANALYSIS_REQUEST_WINDOW {
            return None;
        }
        let block = active.plan.block(active.next_submission_ordinal)?;
        Some(AnalysisDemand {
            ordinal: block.ordinal(),
            request: ResourceRequest::new(
                block.resource(),
                RequestPriority::Analysis,
                active.generation,
            ),
        })
    }

    /// Binds a successful shared-runtime submission to its plan ordinal.
    pub fn register_submission(
        &mut self,
        demand: AnalysisDemand,
        ticket: RequestTicket,
    ) -> Result<(), AnalysisRuntimeError> {
        let RuntimeState::Running(active) = &mut self.state else {
            return Err(AnalysisRuntimeError::NotRunning);
        };
        if active.occupied_window() >= ANALYSIS_REQUEST_WINDOW {
            return Err(AnalysisRuntimeError::DemandMismatch);
        }
        let expected = active
            .plan
            .block(active.next_submission_ordinal)
            .ok_or(AnalysisRuntimeError::DemandMismatch)?;
        let expected_request = ResourceRequest::new(
            expected.resource(),
            RequestPriority::Analysis,
            active.generation,
        );
        if demand.ordinal != expected.ordinal() || demand.request != expected_request {
            return Err(AnalysisRuntimeError::DemandMismatch);
        }
        if ticket.resource() != demand.request.resource()
            || ticket.generation() != demand.request.generation()
        {
            return Err(AnalysisRuntimeError::TicketMismatch);
        }
        if active.tickets.contains_key(&ticket.id()) {
            return Err(AnalysisRuntimeError::DuplicateTicket);
        }
        active.tickets.insert(
            ticket.id(),
            RegisteredRequest {
                ordinal: demand.ordinal,
                ticket,
            },
        );
        active.next_submission_ordinal += 1;
        Ok(())
    }

    /// Accepts one completion already drained by the application's sole
    /// dataset-runtime poller.
    pub fn accept_completion(
        &mut self,
        ticket: RequestTicket,
        outcome: RuntimeOutcome,
    ) -> Result<CompletionEvent, AnalysisRuntimeError> {
        let RuntimeState::Running(active) = std::mem::replace(&mut self.state, RuntimeState::Idle)
        else {
            self.check_retired_scope(ticket.generation())?;
            return Ok(CompletionEvent::IgnoredRetired);
        };
        let mut active = *active;

        match ticket.is_current(active.generation) {
            Ok(true) => {}
            Ok(false) => {
                self.state = RuntimeState::Running(Box::new(active));
                return Ok(CompletionEvent::IgnoredRetired);
            }
            Err(error) => {
                self.state = RuntimeState::Running(Box::new(active));
                return Err(error.into());
            }
        }
        let Some(registered) = active.tickets.get(&ticket.id()).copied() else {
            self.state = RuntimeState::Running(Box::new(active));
            return Err(AnalysisRuntimeError::UnexpectedTicket);
        };
        if registered.ticket != ticket
            || registered.ticket.resource()
                != active
                    .plan
                    .block(registered.ordinal)
                    .expect("a registered ordinal belongs to its immutable plan")
                    .resource()
        {
            self.state = RuntimeState::Running(Box::new(active));
            return Err(AnalysisRuntimeError::TicketMismatch);
        }
        active.tickets.remove(&ticket.id());

        match outcome {
            RuntimeOutcome::Ready(lease) => {
                active.buffered.insert(registered.ordinal, lease);
            }
            RuntimeOutcome::Cancelled => return Ok(CompletionEvent::Cancelled),
            RuntimeOutcome::Failed(fault) => {
                return Ok(CompletionEvent::Failed(AnalysisFailure::Dataset(fault)));
            }
        }

        if let Err(error) = active.reduce_ready_in_order() {
            return Ok(CompletionEvent::Failed(AnalysisFailure::Reduction(error)));
        }
        if active.accumulator.completed_blocks() != active.plan.total_blocks() {
            let progress = active.progress();
            self.state = RuntimeState::Running(Box::new(active));
            return Ok(CompletionEvent::Progressed(progress));
        }

        let artifacts = match active.accumulator.finish() {
            Ok(artifacts) => artifacts,
            Err(error) => {
                return Ok(CompletionEvent::Failed(AnalysisFailure::Reduction(error)));
            }
        };
        let actual_bytes = artifacts.payload_bytes();
        if actual_bytes > active.required_result_bytes
            || actual_bytes > active.result_charge.accounted_bytes()
        {
            return Ok(CompletionEvent::Failed(
                AnalysisFailure::ResultReservationExceeded {
                    actual_bytes,
                    reserved_bytes: active
                        .required_result_bytes
                        .min(active.result_charge.accounted_bytes()),
                },
            ));
        }
        self.state = RuntimeState::PendingCommit(Box::new(PendingAnalysisCommit {
            definition: active.definition,
            project_revision: active.project_revision,
            generation: active.generation,
            required_result_bytes: active.required_result_bytes,
            artifacts,
            _result_charge: active.result_charge,
        }));
        Ok(CompletionEvent::PendingCommitReady)
    }

    pub fn progress(&self) -> Option<AnalysisProgress> {
        let RuntimeState::Running(active) = &self.state else {
            return None;
        };
        Some(active.progress())
    }

    /// Drops all partial or pending work for the current run. The caller owns
    /// the matching `DatasetRuntime::cancel_before(next_generation)` call.
    pub fn cancel(
        &mut self,
        next_generation: CancellationGeneration,
    ) -> Result<CancelEvent, AnalysisRuntimeError> {
        let current = match &self.state {
            RuntimeState::Idle => return Ok(CancelEvent::Idle),
            RuntimeState::Running(active) => active.generation,
            RuntimeState::PendingCommit(pending) => pending.generation,
        };
        if current.scope() != next_generation.scope() {
            return Err(RuntimeFaultCode::CancellationScopeMismatch {
                request_scope: next_generation.scope(),
                current_scope: current.scope(),
            }
            .into());
        }
        if current.checked_next()? != next_generation {
            return Err(AnalysisRuntimeError::InvalidCancellationStep);
        }
        let old = std::mem::replace(&mut self.state, RuntimeState::Idle);
        Ok(match old {
            RuntimeState::Running(_) => CancelEvent::CancelledRunning,
            RuntimeState::PendingCommit(_) => CancelEvent::DiscardedPendingCommit,
            RuntimeState::Idle => unreachable!("idle returned before state replacement"),
        })
    }

    /// Transfers the complete bundle to application-owned commit code.
    pub fn take_pending_commit(&mut self) -> Option<PendingAnalysisCommit> {
        let old = std::mem::replace(&mut self.state, RuntimeState::Idle);
        match old {
            RuntimeState::PendingCommit(pending) => Some(*pending),
            other => {
                self.state = other;
                None
            }
        }
    }

    /// Rejects a complete but now-stale bundle and releases its result charge.
    pub fn discard_pending_commit(&mut self) -> bool {
        if !matches!(self.state, RuntimeState::PendingCommit(_)) {
            return false;
        }
        self.state = RuntimeState::Idle;
        true
    }

    fn check_retired_scope(
        &self,
        generation: CancellationGeneration,
    ) -> Result<(), AnalysisRuntimeError> {
        if let Some(previous) = self.last_started_generation {
            let _ = generation.is_stale_for(previous)?;
        }
        Ok(())
    }
}

impl ActiveAnalysis {
    fn occupied_window(&self) -> usize {
        self.tickets.len() + self.buffered.len()
    }

    fn reduce_ready_in_order(&mut self) -> Result<(), AnalysisError> {
        while let Some(lease) = self.buffered.remove(&self.accumulator.completed_blocks()) {
            let ordinal = self.accumulator.completed_blocks();
            let block = self
                .plan
                .block(ordinal)
                .expect("a buffered ordinal belongs to its immutable plan");
            self.accumulator
                .include(block.resource(), lease.payload())?;
        }
        Ok(())
    }

    fn progress(&self) -> AnalysisProgress {
        AnalysisProgress {
            completed_blocks: self.accumulator.completed_blocks(),
            total_blocks: self.plan.total_blocks(),
            submitted_blocks: self.next_submission_ordinal,
            in_flight_blocks: self.tickets.len(),
            buffered_blocks: self.buffered.len(),
        }
    }
}

#[cfg(test)]
mod tests;

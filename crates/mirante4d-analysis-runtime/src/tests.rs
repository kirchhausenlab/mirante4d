use std::{sync::Arc, thread, time::Duration};

use mirante4d_analysis_core::{AnalysisDefinition, AnalysisOperation};
use mirante4d_dataset::{
    CpuLedgerCategory, DatasetCatalog, DatasetLayer, DatasetSource, DatasetSourceFault,
    DecodeSinkError, ReservedDecodeSink, ResourceRegion, ResourceValidity,
    ScientificIdentityStatus,
};
use mirante4d_dataset_runtime::{
    CancellationGeneration, DatasetRuntime, DatasetRuntimeConfig, RequestPriority, RequestTicket,
    RuntimeCompletion, RuntimeFault, RuntimeFaultCode, RuntimeOutcome,
};
use mirante4d_domain::{GridToWorld, IntensityDType, LogicalLayerKey, Shape3D, Shape4D};
use mirante4d_identity::ScientificContentId;
use mirante4d_project_model::{ProjectId, ProjectRevisionId};

use super::*;

const SCIENTIFIC_ID: &str =
    "m4d-sc-v1-sha256:1111111111111111111111111111111111111111111111111111111111111111";
const ANALYSIS_SCOPE: u64 = 6;

struct FixtureSource {
    catalog: Arc<DatasetCatalog>,
}

impl DatasetSource for FixtureSource {
    fn catalog(&self) -> Result<Arc<DatasetCatalog>, DatasetSourceFault> {
        Ok(Arc::clone(&self.catalog))
    }

    fn decode_into(&self, sink: &mut dyn ReservedDecodeSink) -> Result<(), DatasetSourceFault> {
        let key = sink.resource_key();
        self.catalog
            .validate_decode_reservation(sink)
            .map_err(|reason| DatasetSourceFault::InvalidResource {
                key,
                reason: Box::new(reason),
            })?;
        let value_len = usize::try_from(sink.payload_descriptor().value_byte_len()).unwrap();
        let first_value = u8::try_from(key.region().origin()[2]).unwrap();
        let values = (0..value_len)
            .map(|offset| first_value + u8::try_from(offset).unwrap())
            .collect::<Vec<_>>();
        sink.write(&values)
            .map_err(|reason| sink_fault(key, reason))?;
        sink.finish().map_err(|reason| sink_fault(key, reason))
    }
}

fn sink_fault(
    key: mirante4d_dataset::DatasetResourceKey,
    reason: DecodeSinkError,
) -> DatasetSourceFault {
    DatasetSourceFault::SinkRejected {
        key,
        reason: Box::new(reason),
    }
}

fn fixture() -> (
    Arc<dyn DatasetRuntime>,
    Arc<DatasetCatalog>,
    AnalysisDefinition,
) {
    let layer = DatasetLayer::new(
        LogicalLayerKey::new(0),
        "intensity",
        Shape4D::new(1, 1, 1, 6).unwrap(),
        IntensityDType::Uint8,
        GridToWorld::identity(),
        ResourceValidity::AllValid,
    )
    .unwrap();
    let catalog = Arc::new(
        DatasetCatalog::new(
            "analysis-runtime-test",
            ScientificIdentityStatus::Verified(ScientificContentId::parse(SCIENTIFIC_ID).unwrap()),
            vec![layer],
        )
        .unwrap(),
    );
    let source: Arc<dyn DatasetSource> = Arc::new(FixtureSource {
        catalog: Arc::clone(&catalog),
    });
    let config = DatasetRuntimeConfig::new(16 * 1024 * 1024, 2, 16, 16).unwrap();
    let (runtime, opened_catalog) =
        <dyn DatasetRuntime>::start(config, move |_| Ok(source)).unwrap();
    let definition = AnalysisDefinition::new(
        opened_catalog.as_ref(),
        LogicalLayerKey::new(0),
        0,
        1,
        ResourceRegion::new([0, 0, 0], Shape3D::new(1, 1, 6).unwrap()).unwrap(),
        AnalysisOperation::FullIntensitySummary,
        Shape3D::new(1, 1, 2).unwrap(),
    )
    .unwrap();
    (runtime, opened_catalog, definition)
}

fn revision(sequence: u64) -> ProjectRevisionId {
    ProjectRevisionId::new(ProjectId::from_bytes([7; 16]), sequence)
}

fn collect(runtime: &Arc<dyn DatasetRuntime>, count: usize) -> Vec<RuntimeCompletion> {
    let mut completions = Vec::new();
    for _ in 0..200 {
        completions.extend(runtime.poll(count - completions.len()).unwrap());
        if completions.len() == count {
            return completions;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {count} runtime completions");
}

fn take_completion(
    completions: &mut Vec<RuntimeCompletion>,
    ticket: RequestTicket,
) -> (RequestTicket, RuntimeOutcome) {
    let index = completions
        .iter()
        .position(|completion| completion.ticket() == ticket)
        .unwrap();
    let completion = completions.swap_remove(index);
    (completion.ticket(), completion.outcome().clone())
}

#[test]
fn two_request_window_retries_unregistered_demand_and_reduces_in_plan_order() {
    let (runtime, _catalog, definition) = fixture();
    let required = required_result_bytes(&definition).unwrap();
    let charge = runtime.try_acquire_analysis_bytes(required).unwrap();
    let generation = CancellationGeneration::for_scope(ANALYSIS_SCOPE, 0);
    let mut analysis = AnalysisRuntime::new();
    analysis
        .start(definition, revision(4), generation, charge)
        .unwrap();

    let first = analysis.next_demand().unwrap();
    assert_eq!(first.request().priority(), RequestPriority::Analysis);
    assert_eq!(analysis.next_demand(), Some(first));
    let first_ticket = runtime.submit(first.request()).unwrap();
    analysis.register_submission(first, first_ticket).unwrap();
    let second = analysis.next_demand().unwrap();
    let second_ticket = runtime.submit(second.request()).unwrap();
    analysis.register_submission(second, second_ticket).unwrap();
    assert!(analysis.next_demand().is_none());

    let mut completions = collect(&runtime, 2);
    let (ticket, outcome) = take_completion(&mut completions, second_ticket);
    let event = analysis.accept_completion(ticket, outcome).unwrap();
    assert_eq!(
        event,
        CompletionEvent::Progressed(AnalysisProgress {
            completed_blocks: 0,
            total_blocks: 3,
            submitted_blocks: 2,
            in_flight_blocks: 1,
            buffered_blocks: 1,
        })
    );
    assert!(analysis.next_demand().is_none());

    let (ticket, outcome) = take_completion(&mut completions, first_ticket);
    let event = analysis.accept_completion(ticket, outcome).unwrap();
    assert_eq!(
        event,
        CompletionEvent::Progressed(analysis.progress().unwrap())
    );
    assert_eq!(analysis.progress().unwrap().completed_blocks(), 2);

    let third = analysis.next_demand().unwrap();
    let third_ticket = runtime.submit(third.request()).unwrap();
    analysis.register_submission(third, third_ticket).unwrap();
    let mut completions = collect(&runtime, 1);
    let (ticket, outcome) = take_completion(&mut completions, third_ticket);
    assert_eq!(
        analysis.accept_completion(ticket, outcome).unwrap(),
        CompletionEvent::PendingCommitReady
    );
    assert_eq!(analysis.status(), AnalysisStatus::PendingCommit);

    let used_with_pending = runtime
        .diagnostics()
        .unwrap()
        .category_used_bytes(CpuLedgerCategory::QueuesAndResults);
    let pending = analysis.take_pending_commit().unwrap();
    assert_eq!(pending.project_revision(), revision(4));
    assert_eq!(pending.generation(), generation);
    assert!(pending.artifacts().plot().is_some());
    assert!(pending.artifacts().payload_bytes() <= pending.required_result_bytes());
    let row = &pending.artifacts().table().value().rows()[0];
    assert_eq!(row.valid_sample_count(), 6);
    assert_eq!(row.sum(), Some(15.0));
    assert_eq!(row.mean(), Some(2.5));
    assert_eq!(
        runtime
            .diagnostics()
            .unwrap()
            .category_used_bytes(CpuLedgerCategory::QueuesAndResults),
        used_with_pending,
        "transferring pending work retains its result charge"
    );
    drop(pending);
    assert_eq!(
        used_with_pending
            - runtime
                .diagnostics()
                .unwrap()
                .category_used_bytes(CpuLedgerCategory::QueuesAndResults),
        required
    );
}

#[test]
fn cancellation_drops_partial_work_and_late_completion_is_ignored() {
    let (runtime, _catalog, definition) = fixture();
    let required = required_result_bytes(&definition).unwrap();
    let charge = runtime.try_acquire_analysis_bytes(required).unwrap();
    let generation = CancellationGeneration::for_scope(ANALYSIS_SCOPE, 8);
    let next_generation = generation.checked_next().unwrap();
    let mut analysis = AnalysisRuntime::new();
    analysis
        .start(definition, revision(8), generation, charge)
        .unwrap();

    let first = analysis.next_demand().unwrap();
    let first_ticket = runtime.submit(first.request()).unwrap();
    analysis.register_submission(first, first_ticket).unwrap();
    let second = analysis.next_demand().unwrap();
    let second_ticket = runtime.submit(second.request()).unwrap();
    analysis.register_submission(second, second_ticket).unwrap();
    let mut completions = collect(&runtime, 2);
    let (ticket, outcome) = take_completion(&mut completions, second_ticket);
    analysis.accept_completion(ticket, outcome).unwrap();
    assert_eq!(analysis.progress().unwrap().buffered_blocks(), 1);

    let before_cancel = runtime
        .diagnostics()
        .unwrap()
        .category_used_bytes(CpuLedgerCategory::QueuesAndResults);
    assert_eq!(
        analysis.cancel(next_generation).unwrap(),
        CancelEvent::CancelledRunning
    );
    assert_eq!(analysis.status(), AnalysisStatus::Idle);
    assert!(analysis.progress().is_none());
    assert!(analysis.take_pending_commit().is_none());
    assert_eq!(
        before_cancel
            - runtime
                .diagnostics()
                .unwrap()
                .category_used_bytes(CpuLedgerCategory::QueuesAndResults),
        required
    );

    let (ticket, outcome) = take_completion(&mut completions, first_ticket);
    assert_eq!(
        analysis.accept_completion(ticket, outcome).unwrap(),
        CompletionEvent::IgnoredRetired
    );
    runtime.cancel_before(next_generation).unwrap();
}

#[test]
fn dataset_failure_drops_partial_state_and_requires_a_new_generation() {
    let (runtime, _catalog, definition) = fixture();
    let required = required_result_bytes(&definition).unwrap();
    let generation = CancellationGeneration::for_scope(ANALYSIS_SCOPE, 12);
    let mut analysis = AnalysisRuntime::new();
    analysis
        .start(
            definition.clone(),
            revision(12),
            generation,
            runtime.try_acquire_analysis_bytes(required).unwrap(),
        )
        .unwrap();
    let demand = analysis.next_demand().unwrap();
    let ticket = runtime.submit(demand.request()).unwrap();
    analysis.register_submission(demand, ticket).unwrap();
    let before_failure = runtime
        .diagnostics()
        .unwrap()
        .category_used_bytes(CpuLedgerCategory::QueuesAndResults);
    let fault = RuntimeFault::for_ticket(RuntimeFaultCode::DecodeFailed, ticket);
    assert_eq!(
        analysis
            .accept_completion(ticket, RuntimeOutcome::Failed(fault.clone()))
            .unwrap(),
        CompletionEvent::Failed(AnalysisFailure::Dataset(fault))
    );
    assert_eq!(analysis.status(), AnalysisStatus::Idle);
    assert!(analysis.take_pending_commit().is_none());
    assert_eq!(
        before_failure
            - runtime
                .diagnostics()
                .unwrap()
                .category_used_bytes(CpuLedgerCategory::QueuesAndResults),
        required
    );

    let same_generation_charge = runtime.try_acquire_analysis_bytes(required).unwrap();
    assert_eq!(
        analysis.start(
            definition.clone(),
            revision(13),
            generation,
            same_generation_charge
        ),
        Err(AnalysisRuntimeError::GenerationNotAdvanced)
    );
    let next_generation = generation.checked_next().unwrap();
    runtime.cancel_before(next_generation).unwrap();
    analysis
        .start(
            definition,
            revision(13),
            next_generation,
            runtime.try_acquire_analysis_bytes(required).unwrap(),
        )
        .unwrap();
    analysis
        .cancel(next_generation.checked_next().unwrap())
        .unwrap();
}

#[test]
fn start_rejects_an_underfunded_result_reservation() {
    let (runtime, _catalog, definition) = fixture();
    let required = required_result_bytes(&definition).unwrap();
    let mut analysis = AnalysisRuntime::new();
    assert_eq!(
        analysis.start(
            definition,
            revision(0),
            CancellationGeneration::for_scope(ANALYSIS_SCOPE, 0),
            runtime.try_acquire_analysis_bytes(required - 1).unwrap(),
        ),
        Err(AnalysisRuntimeError::InsufficientResultReservation {
            reserved_bytes: required - 1,
            required_bytes: required,
        })
    );
    assert_eq!(analysis.status(), AnalysisStatus::Idle);
}

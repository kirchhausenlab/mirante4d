//! Dataset scheduling and byte-accounted lease contracts.
//!
//! WP-08A freezes this boundary; WP-08B supplies the production scheduler,
//! cache, queues, and workers. This crate performs no I/O and starts no
//! threads.

#![forbid(unsafe_code)]

use std::{cmp::Ordering, fmt, num::NonZeroU64, sync::Arc};

use mirante4d_dataset::{
    CpuByteLease, CpuByteLedger, CpuLedgerCategory, DatasetResourceKey, ResourceLease,
    ResourcePayloadDescriptor, ResourcePayloadView,
};
use thiserror::Error;

/// One nonzero request identity assigned by the dataset runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuntimeRequestId(NonZeroU64);

impl RuntimeRequestId {
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Monotonic currentness used to cancel and suppress superseded work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CancellationGeneration(u64);

impl CancellationGeneration {
    pub const INITIAL: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub const fn is_stale_for(self, current: Self) -> bool {
        self.0 < current.0
    }

    pub fn checked_next(self) -> Result<Self, RuntimeFaultCode> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(RuntimeFaultCode::GenerationExhausted)
    }
}

/// Deterministic demand classes shared by 3D, linked views, playback,
/// analysis, and speculative prefetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestPriority {
    CurrentView,
    LinkedView,
    Playback,
    Analysis,
    Prefetch,
}

impl RequestPriority {
    pub const fn rank(self) -> u8 {
        match self {
            Self::CurrentView => 0,
            Self::LinkedView => 1,
            Self::Playback => 2,
            Self::Analysis => 3,
            Self::Prefetch => 4,
        }
    }

    pub const fn outranks(self, other: Self) -> bool {
        self.rank() < other.rank()
    }
}

impl PartialOrd for RequestPriority {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RequestPriority {
    fn cmp(&self, other: &Self) -> Ordering {
        // Standard max-heaps pop the most urgent request first.
        other.rank().cmp(&self.rank())
    }
}

/// Stable identity for in-flight decode deduplication. Multiple request IDs
/// may wait on one semantic resource without duplicating decode or payload
/// allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestDedupeKey(DatasetResourceKey);

impl RequestDedupeKey {
    pub const fn resource(self) -> DatasetResourceKey {
        self.0
    }
}

/// One bounded semantic demand submitted to the unified runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResourceRequest {
    id: RuntimeRequestId,
    resource: DatasetResourceKey,
    priority: RequestPriority,
    generation: CancellationGeneration,
}

impl ResourceRequest {
    pub const fn new(
        id: RuntimeRequestId,
        resource: DatasetResourceKey,
        priority: RequestPriority,
        generation: CancellationGeneration,
    ) -> Self {
        Self {
            id,
            resource,
            priority,
            generation,
        }
    }

    pub const fn id(self) -> RuntimeRequestId {
        self.id
    }

    pub const fn resource(self) -> DatasetResourceKey {
        self.resource
    }

    pub const fn priority(self) -> RequestPriority {
        self.priority
    }

    pub const fn generation(self) -> CancellationGeneration {
        self.generation
    }

    pub const fn dedupe_key(self) -> RequestDedupeKey {
        RequestDedupeKey(self.resource)
    }
}

/// Admission token returned without waiting for I/O or decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestTicket {
    id: RuntimeRequestId,
    resource: DatasetResourceKey,
    generation: CancellationGeneration,
}

impl RequestTicket {
    pub const fn from_request(request: ResourceRequest) -> Self {
        Self {
            id: request.id(),
            resource: request.resource(),
            generation: request.generation(),
        }
    }

    pub const fn id(self) -> RuntimeRequestId {
        self.id
    }

    pub const fn resource(self) -> DatasetResourceKey {
        self.resource
    }

    pub const fn generation(self) -> CancellationGeneration {
        self.generation
    }

    pub const fn is_current(self, current: CancellationGeneration) -> bool {
        self.generation.0 == current.0
    }
}

/// A concrete immutable lease whose allocation and accounting metadata remain
/// private to the dataset runtime. Cloning the lease clones only the `Arc`.
#[derive(Debug, Clone)]
pub struct AccountedResourceLease {
    inner: Arc<AccountedPayload>,
}

/// Opaque runtime-issued lifetime token for non-payload CPU bytes, such as a
/// pending analysis result. Its private backing becomes the real ledger guard
/// in WP-08B; consumers can inspect but cannot construct or alter the charge.
#[derive(Debug, Clone)]
pub struct AccountedCpuLease {
    inner: Arc<AccountedCpuCharge>,
}

#[derive(Debug)]
struct AccountedCpuCharge {
    bytes: u64,
    category: CpuLedgerCategory,
}

impl AccountedCpuLease {
    pub fn accounted_bytes(&self) -> u64 {
        self.inner.bytes
    }

    pub fn ledger_category(&self) -> CpuLedgerCategory {
        self.inner.category
    }

    pub fn shares_charge_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

#[derive(Debug)]
struct AccountedPayload {
    key: DatasetResourceKey,
    descriptor: ResourcePayloadDescriptor,
    bytes: Box<[u8]>,
    accounted_bytes: u64,
    category: CpuLedgerCategory,
}

impl AccountedResourceLease {
    pub fn accounted_bytes(&self) -> u64 {
        self.inner.accounted_bytes
    }

    pub fn ledger_category(&self) -> CpuLedgerCategory {
        self.inner.category
    }

    pub fn shares_allocation_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl ResourceLease for AccountedResourceLease {
    fn key(&self) -> DatasetResourceKey {
        self.inner.key
    }

    fn payload(&self) -> ResourcePayloadView<'_> {
        self.inner
            .descriptor
            .view(&self.inner.bytes)
            .expect("runtime-issued lease preserves its validated descriptor")
    }
}

impl CpuByteLease for AccountedResourceLease {
    fn category(&self) -> CpuLedgerCategory {
        self.inner.category
    }

    fn reserved_bytes(&self) -> u64 {
        self.inner.accounted_bytes
    }
}

impl CpuByteLease for AccountedCpuLease {
    fn category(&self) -> CpuLedgerCategory {
        self.inner.category
    }

    fn reserved_bytes(&self) -> u64 {
        self.inner.bytes
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFaultCode {
    #[error("the minimum work unit exceeds the configured CPU dataset budget")]
    MinimumWorkUnitExceedsBudget,
    #[error(
        "CPU capacity in {category:?} cannot satisfy {requested_bytes} bytes with {available_bytes} bytes available"
    )]
    CapacityExceeded {
        category: CpuLedgerCategory,
        requested_bytes: u64,
        available_bytes: u64,
    },
    #[error("the bounded request queue is full")]
    QueueFull,
    #[error("the source rejected the semantic resource")]
    SourceRejected,
    #[error("the source resource is corrupt")]
    CorruptResource,
    #[error("the source resource representation is unsupported")]
    UnsupportedResource,
    #[error("decoding failed")]
    DecodeFailed,
    #[error("the runtime reservation rejected decoded output")]
    SinkRejected,
    #[error("the request was cancelled")]
    Cancelled,
    #[error("the request generation is stale")]
    StaleGeneration,
    #[error("the runtime is shutting down")]
    ShuttingDown,
    #[error("the cancellation generation counter is exhausted")]
    GenerationExhausted,
    #[error("a dataset runtime invariant was violated")]
    InvariantViolation,
}

/// A typed runtime failure with enough semantic context to attribute or
/// suppress it, and no storage path or backend error string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFault {
    inner: Box<RuntimeFaultContext>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuntimeFaultContext {
    code: RuntimeFaultCode,
    request_id: Option<RuntimeRequestId>,
    generation: Option<CancellationGeneration>,
    resource: Option<DatasetResourceKey>,
}

impl RuntimeFault {
    pub fn new(code: RuntimeFaultCode) -> Self {
        Self {
            inner: Box::new(RuntimeFaultContext {
                code,
                request_id: None,
                generation: None,
                resource: None,
            }),
        }
    }

    pub fn for_request(code: RuntimeFaultCode, request: ResourceRequest) -> Self {
        Self {
            inner: Box::new(RuntimeFaultContext {
                code,
                request_id: Some(request.id()),
                generation: Some(request.generation()),
                resource: Some(request.resource()),
            }),
        }
    }

    pub fn code(&self) -> RuntimeFaultCode {
        self.inner.code
    }

    pub fn request_id(&self) -> Option<RuntimeRequestId> {
        self.inner.request_id
    }

    pub fn generation(&self) -> Option<CancellationGeneration> {
        self.inner.generation
    }

    pub fn resource(&self) -> Option<DatasetResourceKey> {
        self.inner.resource
    }
}

impl fmt::Display for RuntimeFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "dataset runtime request failed: {}",
            self.inner.code
        )
    }
}

impl std::error::Error for RuntimeFault {}

/// Terminal state for one admitted request.
#[derive(Debug, Clone)]
pub enum RuntimeOutcome {
    Ready(AccountedResourceLease),
    Cancelled,
    Failed(RuntimeFault),
}

/// One completion drained in a caller-bounded batch.
#[derive(Debug, Clone)]
pub struct RuntimeCompletion {
    ticket: RequestTicket,
    outcome: RuntimeOutcome,
}

impl RuntimeCompletion {
    pub const fn new(ticket: RequestTicket, outcome: RuntimeOutcome) -> Self {
        Self { ticket, outcome }
    }

    pub const fn ticket(&self) -> RequestTicket {
        self.ticket
    }

    pub const fn outcome(&self) -> &RuntimeOutcome {
        &self.outcome
    }

    pub const fn is_current(&self, current: CancellationGeneration) -> bool {
        self.ticket.is_current(current)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownState {
    Running,
    Draining,
    Stopped,
}

/// Nonblocking scheduling boundary. Calls from UI/frame code may enqueue,
/// cancel, or drain a bounded number of completions; implementations perform
/// I/O, decode, eviction, and worker joins outside those calls.
pub trait DatasetRuntime: CpuByteLedger + Send + Sync {
    fn submit(&self, request: ResourceRequest) -> Result<RequestTicket, RuntimeFault>;
    fn cancel_before(&self, current: CancellationGeneration) -> Result<(), RuntimeFault>;
    fn poll(&self, max_completions: usize) -> Result<Vec<RuntimeCompletion>, RuntimeFault>;
    fn try_acquire_analysis_bytes(&self, bytes: u64) -> Result<AccountedCpuLease, RuntimeFault>;
    fn request_shutdown(&self) -> Result<(), RuntimeFault>;
    fn shutdown_state(&self) -> ShutdownState;
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::Mutex,
    };

    use super::*;
    use mirante4d_dataset::{
        CpuLedgerError, DatasetResourceIdentity, ResourcePayloadDescriptor, ResourceRegion,
    };
    use mirante4d_domain::{IntensityDType, LogicalLayerKey, ScaleLevel, Shape3D, TimeIndex};
    use mirante4d_identity::ScientificContentId;

    const SCIENTIFIC_ID: &str =
        "m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn key(region_origin: u64) -> DatasetResourceKey {
        DatasetResourceKey::new(
            DatasetResourceIdentity::Verified(ScientificContentId::parse(SCIENTIFIC_ID).unwrap()),
            LogicalLayerKey::new(2),
            TimeIndex::new(3),
            ScaleLevel::new(1),
            ResourceRegion::new([region_origin, 0, 0], Shape3D::new(1, 1, 2).unwrap()).unwrap(),
        )
    }

    fn request_at(
        id: u64,
        resource: DatasetResourceKey,
        generation: CancellationGeneration,
    ) -> ResourceRequest {
        ResourceRequest::new(
            RuntimeRequestId::new(id).unwrap(),
            resource,
            RequestPriority::CurrentView,
            generation,
        )
    }

    fn request(id: u64, resource: DatasetResourceKey) -> ResourceRequest {
        request_at(id, resource, CancellationGeneration::new(7))
    }

    fn lease(resource: DatasetResourceKey) -> AccountedResourceLease {
        let descriptor =
            ResourcePayloadDescriptor::new(IntensityDType::Uint16, resource.region().shape())
                .unwrap();
        AccountedResourceLease {
            inner: Arc::new(AccountedPayload {
                key: resource,
                descriptor,
                bytes: vec![1, 0, 2, 0].into_boxed_slice(),
                accounted_bytes: descriptor.byte_len(),
                category: CpuLedgerCategory::DecodedResidency,
            }),
        }
    }

    struct TestRuntime {
        state: Mutex<TestRuntimeState>,
        queue_capacity: usize,
        poll_limit: usize,
    }

    struct TestRuntimeState {
        current: CancellationGeneration,
        shutdown: ShutdownState,
        pending: BTreeMap<RequestDedupeKey, TestDecodeJob>,
        completions: VecDeque<RuntimeCompletion>,
        decode_count: usize,
    }

    struct TestDecodeJob {
        waiters: Vec<ResourceRequest>,
        started: bool,
    }

    #[derive(Clone, Copy)]
    struct CapturedDecode {
        key: RequestDedupeKey,
    }

    struct TestCpuLease {
        category: CpuLedgerCategory,
        bytes: u64,
    }

    impl CpuByteLease for TestCpuLease {
        fn category(&self) -> CpuLedgerCategory {
            self.category
        }

        fn reserved_bytes(&self) -> u64 {
            self.bytes
        }
    }

    impl TestRuntime {
        fn new(current: CancellationGeneration, queue_capacity: usize, poll_limit: usize) -> Self {
            Self {
                state: Mutex::new(TestRuntimeState {
                    current,
                    shutdown: ShutdownState::Running,
                    pending: BTreeMap::new(),
                    completions: VecDeque::new(),
                    decode_count: 0,
                }),
                queue_capacity,
                poll_limit,
            }
        }

        fn in_flight_decode_count(&self) -> usize {
            self.state.lock().unwrap().pending.len()
        }

        fn completed_decode_count(&self) -> usize {
            self.state.lock().unwrap().decode_count
        }

        fn capture_decode(&self, resource: DatasetResourceKey) -> CapturedDecode {
            let key = RequestDedupeKey(resource);
            let mut state = self.state.lock().unwrap();
            let job = state
                .pending
                .get_mut(&key)
                .expect("a decode can start only after demand is admitted");
            assert!(!job.started, "a deduplicated decode starts only once");
            job.started = true;
            CapturedDecode { key }
        }

        fn complete_captured(&self, captured: CapturedDecode) {
            let mut state = self.state.lock().unwrap();
            let job = state
                .pending
                .remove(&captured.key)
                .expect("a captured decode remains registered until completion");
            assert!(job.started);
            state.decode_count += 1;
            let shared = lease(captured.key.resource());
            let current = state.current;
            for request in job.waiters {
                if request.generation() == current {
                    state.completions.push_back(RuntimeCompletion::new(
                        RequestTicket::from_request(request),
                        RuntimeOutcome::Ready(shared.clone()),
                    ));
                }
            }
        }

        fn complete(&self, resource: DatasetResourceKey) {
            let captured = self.capture_decode(resource);
            self.complete_captured(captured);
        }
    }

    impl CpuByteLedger for TestRuntime {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            if bytes == 0 {
                return Err(CpuLedgerError::ZeroByteReservation);
            }
            Ok(Box::new(TestCpuLease { category, bytes }))
        }
    }

    impl DatasetRuntime for TestRuntime {
        fn submit(&self, request: ResourceRequest) -> Result<RequestTicket, RuntimeFault> {
            let mut state = self.state.lock().unwrap();
            if state.shutdown != ShutdownState::Running {
                return Err(RuntimeFault::for_request(
                    RuntimeFaultCode::ShuttingDown,
                    request,
                ));
            }
            if request.generation() != state.current {
                return Err(RuntimeFault::for_request(
                    RuntimeFaultCode::StaleGeneration,
                    request,
                ));
            }
            let queued = state
                .pending
                .values()
                .map(|job| job.waiters.len())
                .sum::<usize>();
            if queued >= self.queue_capacity {
                return Err(RuntimeFault::for_request(
                    RuntimeFaultCode::QueueFull,
                    request,
                ));
            }
            state
                .pending
                .entry(request.dedupe_key())
                .or_insert_with(|| TestDecodeJob {
                    waiters: Vec::new(),
                    started: false,
                })
                .waiters
                .push(request);
            Ok(RequestTicket::from_request(request))
        }

        fn cancel_before(&self, current: CancellationGeneration) -> Result<(), RuntimeFault> {
            let mut state = self.state.lock().unwrap();
            if state.shutdown != ShutdownState::Running {
                return Err(RuntimeFault::new(RuntimeFaultCode::ShuttingDown));
            }
            if current < state.current {
                return Err(RuntimeFault::new(RuntimeFaultCode::StaleGeneration));
            }
            state.current = current;
            let mut cancelled = Vec::new();
            state.pending.retain(|_, job| {
                job.waiters.retain(|request| {
                    if request.generation().is_stale_for(current) {
                        cancelled.push(RequestTicket::from_request(*request));
                        false
                    } else {
                        true
                    }
                });
                job.started || !job.waiters.is_empty()
            });
            state.completions.extend(
                cancelled
                    .into_iter()
                    .map(|ticket| RuntimeCompletion::new(ticket, RuntimeOutcome::Cancelled)),
            );
            Ok(())
        }

        fn poll(&self, max_completions: usize) -> Result<Vec<RuntimeCompletion>, RuntimeFault> {
            let mut state = self.state.lock().unwrap();
            let count = max_completions
                .min(self.poll_limit)
                .min(state.completions.len());
            let drained = state.completions.drain(..count).collect();
            if state.shutdown == ShutdownState::Draining
                && state.pending.is_empty()
                && state.completions.is_empty()
            {
                state.shutdown = ShutdownState::Stopped;
            }
            Ok(drained)
        }

        fn try_acquire_analysis_bytes(
            &self,
            bytes: u64,
        ) -> Result<AccountedCpuLease, RuntimeFault> {
            if bytes == 0 {
                return Err(RuntimeFault::new(RuntimeFaultCode::InvariantViolation));
            }
            Ok(AccountedCpuLease {
                inner: Arc::new(AccountedCpuCharge {
                    bytes,
                    category: CpuLedgerCategory::QueuesAndResults,
                }),
            })
        }

        fn request_shutdown(&self) -> Result<(), RuntimeFault> {
            let mut state = self.state.lock().unwrap();
            if state.shutdown == ShutdownState::Running {
                state.shutdown = ShutdownState::Draining;
            }
            Ok(())
        }

        fn shutdown_state(&self) -> ShutdownState {
            self.state.lock().unwrap().shutdown
        }
    }

    #[test]
    fn priorities_are_explicit_and_total() {
        let priorities = [
            RequestPriority::CurrentView,
            RequestPriority::LinkedView,
            RequestPriority::Playback,
            RequestPriority::Analysis,
            RequestPriority::Prefetch,
        ];
        for pair in priorities.windows(2) {
            assert!(pair[0].outranks(pair[1]));
            assert!(pair[0] > pair[1]);
        }
    }

    #[test]
    fn generation_currentness_is_exact_and_overflow_is_typed() {
        let issued = CancellationGeneration::new(4);
        assert!(issued.is_stale_for(CancellationGeneration::new(5)));
        assert!(!issued.is_stale_for(issued));
        assert_eq!(issued.checked_next().unwrap().get(), 5);
        assert_eq!(
            CancellationGeneration::new(u64::MAX).checked_next(),
            Err(RuntimeFaultCode::GenerationExhausted)
        );

        let runtime = TestRuntime::new(CancellationGeneration::new(7), 8, 8);
        let resource = key(0);
        let stale = request(1, resource);
        runtime.submit(stale).unwrap();
        let captured = runtime.capture_decode(resource);
        assert_eq!(runtime.in_flight_decode_count(), 1);
        runtime
            .cancel_before(CancellationGeneration::new(8))
            .unwrap();
        assert_eq!(runtime.in_flight_decode_count(), 1);
        let fault = runtime.submit(stale).unwrap_err();
        assert_eq!(fault.code(), RuntimeFaultCode::StaleGeneration);

        let current = request_at(2, resource, CancellationGeneration::new(8));
        runtime.submit(current).unwrap();
        runtime.complete_captured(captured);
        assert_eq!(runtime.completed_decode_count(), 1);
        let completions = runtime.poll(8).unwrap();
        assert!(completions.iter().any(|completion| {
            completion.ticket().id() == stale.id()
                && matches!(completion.outcome(), RuntimeOutcome::Cancelled)
        }));
        assert!(completions.iter().any(|completion| {
            completion.ticket().id() == current.id()
                && matches!(completion.outcome(), RuntimeOutcome::Ready(_))
        }));
        assert!(!completions.iter().any(|completion| {
            completion.ticket().id() == stale.id()
                && matches!(completion.outcome(), RuntimeOutcome::Ready(_))
        }));
    }

    #[test]
    fn deduplication_uses_semantic_resource_not_request_identity() {
        let resource = key(0);
        let first = request(1, resource);
        let second = request(2, resource);
        assert_eq!(first.dedupe_key(), second.dedupe_key());
        assert_ne!(first.id(), second.id());
        assert_ne!(first.dedupe_key(), request(3, key(1)).dedupe_key());

        let runtime = TestRuntime::new(CancellationGeneration::new(7), 8, 8);
        runtime.submit(first).unwrap();
        runtime.submit(second).unwrap();
        assert_eq!(runtime.in_flight_decode_count(), 1);
        runtime.complete(resource);
        assert_eq!(runtime.completed_decode_count(), 1);

        let completions = runtime.poll(8).unwrap();
        assert_eq!(completions.len(), 2);
        let RuntimeOutcome::Ready(first_lease) = completions[0].outcome() else {
            panic!("first deduplicated waiter did not receive a lease");
        };
        let RuntimeOutcome::Ready(second_lease) = completions[1].outcome() else {
            panic!("second deduplicated waiter did not receive a lease");
        };
        assert!(first_lease.shares_allocation_with(second_lease));
    }

    #[test]
    fn lease_clones_share_one_accounted_immutable_allocation() {
        let lease = lease(key(0));
        let clone = lease.clone();
        assert!(lease.shares_allocation_with(&clone));
        assert_eq!(lease.accounted_bytes(), 4);
        assert_eq!(lease.ledger_category(), CpuLedgerCategory::DecodedResidency);
        assert_eq!(lease.payload().bytes(), &[1, 0, 2, 0]);
        assert_eq!(lease.key(), clone.key());
        assert_eq!(CpuByteLease::reserved_bytes(&lease), 4);
        let runtime = TestRuntime::new(CancellationGeneration::new(7), 8, 8);
        let analysis = runtime.try_acquire_analysis_bytes(16).unwrap();
        let analysis_clone = analysis.clone();
        assert!(analysis.shares_charge_with(&analysis_clone));
        assert_eq!(analysis.accounted_bytes(), 16);
        assert_eq!(
            analysis.ledger_category(),
            CpuLedgerCategory::QueuesAndResults
        );
        assert_eq!(
            [
                CpuLedgerCategory::DecodedResidency,
                CpuLedgerCategory::UploadStaging,
                CpuLedgerCategory::InFlightDecode,
                CpuLedgerCategory::MetadataAndIndexes,
                CpuLedgerCategory::QueuesAndResults,
                CpuLedgerCategory::Prefetch,
                CpuLedgerCategory::ImportWorkingSet,
            ]
            .map(CpuLedgerCategory::contract_name),
            [
                "cpu.decoded-residency",
                "cpu.upload-staging",
                "cpu.in-flight-decode",
                "cpu.metadata-and-indexes",
                "cpu.queues-and-results",
                "cpu.prefetch",
                "cpu.import-working-set",
            ]
        );
    }

    #[test]
    fn tickets_and_faults_keep_typed_currentness_context() {
        let request = request(8, key(0));
        let ticket = RequestTicket::from_request(request);
        assert!(ticket.is_current(CancellationGeneration::new(7)));
        assert!(!ticket.is_current(CancellationGeneration::new(8)));

        let fault = RuntimeFault::for_request(RuntimeFaultCode::StaleGeneration, request);
        assert_eq!(fault.request_id(), Some(request.id()));
        assert_eq!(fault.generation(), Some(request.generation()));
        assert_eq!(fault.resource(), Some(request.resource()));
    }

    #[test]
    fn polling_is_runtime_bounded_and_shutdown_drains_before_stopping() {
        let runtime = TestRuntime::new(CancellationGeneration::new(7), 8, 2);
        for id in 1..=3 {
            let resource = key(id);
            runtime.submit(request(id, resource)).unwrap();
            runtime.complete(resource);
        }

        assert_eq!(runtime.poll(usize::MAX).unwrap().len(), 2);
        assert_eq!(runtime.poll(usize::MAX).unwrap().len(), 1);
        assert_eq!(runtime.shutdown_state(), ShutdownState::Running);
        runtime.request_shutdown().unwrap();
        assert_eq!(runtime.shutdown_state(), ShutdownState::Draining);
        assert!(runtime.poll(0).unwrap().is_empty());
        assert_eq!(runtime.shutdown_state(), ShutdownState::Stopped);

        let fault = runtime.submit(request(9, key(9))).unwrap_err();
        assert_eq!(fault.code(), RuntimeFaultCode::ShuttingDown);
    }
}

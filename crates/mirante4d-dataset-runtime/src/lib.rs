//! Dataset scheduling and byte-accounted lease contracts.
//!
//! WP-08A froze this boundary; WP-08B supplies the production scheduler,
//! cache, queues, byte ledger, and decode workers. Sources perform storage I/O
//! only on runtime-owned worker threads.

#![forbid(unsafe_code)]

use std::{cmp::Ordering, fmt, num::NonZeroU64, sync::Arc};

use mirante4d_dataset::{
    CpuByteLease, CpuByteLedger, CpuLedgerCategory, DatasetResourceKey, ResourceLease,
    ResourcePayloadDescriptor, ResourcePayloadView,
};
use thiserror::Error;

mod ledger;
mod production;

/// One nonzero request identity assigned by the dataset runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuntimeRequestId(NonZeroU64);

impl RuntimeRequestId {
    #[allow(
        dead_code,
        reason = "WP-08B uses this crate-private runtime issuance seam"
    )]
    pub(crate) const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Monotonic currentness within one independently cancellable demand scope.
///
/// Scope zero is reserved for source-wide work. Generations from different
/// scopes are deliberately not ordered: callers must receive a typed mismatch
/// instead of accidentally cancelling unrelated viewer, playback, analysis,
/// or verification work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CancellationGeneration {
    scope: u64,
    value: u64,
}

impl CancellationGeneration {
    pub const SOURCE_WIDE_SCOPE: u64 = 0;
    pub const INITIAL: Self = Self::new(0);

    /// Constructs a source-wide generation in reserved scope zero.
    pub const fn new(value: u64) -> Self {
        Self::for_scope(Self::SOURCE_WIDE_SCOPE, value)
    }

    pub const fn for_scope(scope: u64, value: u64) -> Self {
        Self { scope, value }
    }

    pub const fn scope(self) -> u64 {
        self.scope
    }

    pub const fn get(self) -> u64 {
        self.value
    }

    pub const fn is_stale_for(self, current: Self) -> Result<bool, RuntimeFaultCode> {
        if self.scope != current.scope {
            return Err(RuntimeFaultCode::CancellationScopeMismatch {
                request_scope: self.scope,
                current_scope: current.scope,
            });
        }
        Ok(self.value < current.value)
    }

    pub const fn is_current(self, current: Self) -> Result<bool, RuntimeFaultCode> {
        if self.scope != current.scope {
            return Err(RuntimeFaultCode::CancellationScopeMismatch {
                request_scope: self.scope,
                current_scope: current.scope,
            });
        }
        Ok(self.value == current.value)
    }

    pub fn checked_next(self) -> Result<Self, RuntimeFaultCode> {
        self.value
            .checked_add(1)
            .map(|value| Self::for_scope(self.scope, value))
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
    resource: DatasetResourceKey,
    priority: RequestPriority,
    generation: CancellationGeneration,
}

impl ResourceRequest {
    pub const fn new(
        resource: DatasetResourceKey,
        priority: RequestPriority,
        generation: CancellationGeneration,
    ) -> Self {
        Self {
            resource,
            priority,
            generation,
        }
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
    #[allow(
        dead_code,
        reason = "WP-08B uses this crate-private runtime issuance seam"
    )]
    const fn for_request(id: RuntimeRequestId, request: ResourceRequest) -> Self {
        Self {
            id,
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

    pub const fn is_current(
        self,
        current: CancellationGeneration,
    ) -> Result<bool, RuntimeFaultCode> {
        self.generation.is_current(current)
    }
}

const CPU_LEDGER_CATEGORIES: [CpuLedgerCategory; 7] = [
    CpuLedgerCategory::DecodedResidency,
    CpuLedgerCategory::UploadStaging,
    CpuLedgerCategory::InFlightDecode,
    CpuLedgerCategory::MetadataAndIndexes,
    CpuLedgerCategory::QueuesAndResults,
    CpuLedgerCategory::Prefetch,
    CpuLedgerCategory::ImportWorkingSet,
];

const fn category_index(category: CpuLedgerCategory) -> usize {
    match category {
        CpuLedgerCategory::DecodedResidency => 0,
        CpuLedgerCategory::UploadStaging => 1,
        CpuLedgerCategory::InFlightDecode => 2,
        CpuLedgerCategory::MetadataAndIndexes => 3,
        CpuLedgerCategory::QueuesAndResults => 4,
        CpuLedgerCategory::Prefetch => 5,
        CpuLedgerCategory::ImportWorkingSet => 6,
    }
}

const fn category_numerator(category: CpuLedgerCategory) -> u64 {
    match category {
        CpuLedgerCategory::DecodedResidency => 20,
        CpuLedgerCategory::UploadStaging | CpuLedgerCategory::InFlightDecode => 5,
        CpuLedgerCategory::MetadataAndIndexes => 4,
        CpuLedgerCategory::QueuesAndResults
        | CpuLedgerCategory::Prefetch
        | CpuLedgerCategory::ImportWorkingSet => 2,
    }
}

fn category_cap(total_cpu_bytes: u64, category: CpuLedgerCategory) -> u64 {
    // Fortieths express every accepted percentage exactly. Widening before
    // multiplication prevents overflow; flooring leaves at most 39 bytes
    // unassigned and can therefore never exceed the total budget.
    let cap = (u128::from(total_cpu_bytes) * u128::from(category_numerator(category))) / 40;
    u64::try_from(cap).expect("a category fraction cannot exceed its u64 total")
}

/// Validated immutable bounds for one unified dataset runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatasetRuntimeConfig {
    total_cpu_bytes: u64,
    worker_limit: usize,
    request_queue_limit: usize,
    completion_queue_limit: usize,
    category_caps: [u64; CPU_LEDGER_CATEGORIES.len()],
}

impl DatasetRuntimeConfig {
    pub fn new(
        total_cpu_bytes: u64,
        worker_limit: usize,
        request_queue_limit: usize,
        completion_queue_limit: usize,
    ) -> Result<Self, RuntimeFaultCode> {
        if total_cpu_bytes == 0
            || worker_limit == 0
            || request_queue_limit == 0
            || completion_queue_limit == 0
        {
            return Err(RuntimeFaultCode::InvalidConfiguration);
        }

        let category_caps =
            CPU_LEDGER_CATEGORIES.map(|category| category_cap(total_cpu_bytes, category));
        let allocated = category_caps.iter().try_fold(0_u64, |sum, cap| {
            sum.checked_add(*cap)
                .ok_or(RuntimeFaultCode::InvalidConfiguration)
        })?;
        if allocated > total_cpu_bytes {
            return Err(RuntimeFaultCode::InvalidConfiguration);
        }

        Ok(Self {
            total_cpu_bytes,
            worker_limit,
            request_queue_limit,
            completion_queue_limit,
            category_caps,
        })
    }

    pub const fn total_cpu_bytes(self) -> u64 {
        self.total_cpu_bytes
    }

    pub const fn worker_limit(self) -> usize {
        self.worker_limit
    }

    pub const fn request_queue_limit(self) -> usize {
        self.request_queue_limit
    }

    pub const fn completion_queue_limit(self) -> usize {
        self.completion_queue_limit
    }

    pub const fn category_cap(self, category: CpuLedgerCategory) -> u64 {
        self.category_caps[category_index(category)]
    }

    pub fn allocated_category_bytes(self) -> u64 {
        self.category_caps.iter().copied().sum()
    }
}

/// Immutable, internally consistent observation of runtime capacity and work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatasetRuntimeDiagnostics {
    config: DatasetRuntimeConfig,
    category_used: [u64; CPU_LEDGER_CATEGORIES.len()],
    queued_requests: usize,
    in_flight_decodes: usize,
    pending_completions: usize,
    resident_resources: usize,
    submitted_requests: u64,
    started_decodes: u64,
    completed_decodes: u64,
    ready_requests: u64,
    cancelled_requests: u64,
    failed_requests: u64,
}

impl DatasetRuntimeDiagnostics {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: DatasetRuntimeConfig,
        category_used: [u64; CPU_LEDGER_CATEGORIES.len()],
        queued_requests: usize,
        in_flight_decodes: usize,
        pending_completions: usize,
        resident_resources: usize,
        submitted_requests: u64,
        started_decodes: u64,
        completed_decodes: u64,
        ready_requests: u64,
        cancelled_requests: u64,
        failed_requests: u64,
    ) -> Result<Self, RuntimeFaultCode> {
        let mut total_used = 0_u64;
        for category in CPU_LEDGER_CATEGORIES {
            let used = category_used[category_index(category)];
            if used > config.category_cap(category) {
                return Err(RuntimeFaultCode::DiagnosticsInvariantViolation);
            }
            total_used = total_used
                .checked_add(used)
                .ok_or(RuntimeFaultCode::DiagnosticsInvariantViolation)?;
        }

        let terminal_requests = ready_requests
            .checked_add(cancelled_requests)
            .and_then(|count| count.checked_add(failed_requests))
            .ok_or(RuntimeFaultCode::DiagnosticsInvariantViolation)?;
        let observed_requests = u64::try_from(queued_requests)
            .ok()
            .and_then(|queued| queued.checked_add(terminal_requests))
            .ok_or(RuntimeFaultCode::DiagnosticsInvariantViolation)?;
        let observed_decodes = u64::try_from(in_flight_decodes)
            .ok()
            .and_then(|in_flight| in_flight.checked_add(completed_decodes))
            .ok_or(RuntimeFaultCode::DiagnosticsInvariantViolation)?;
        if total_used > config.total_cpu_bytes()
            || queued_requests > config.request_queue_limit()
            || in_flight_decodes > config.worker_limit()
            || pending_completions > config.completion_queue_limit()
            || started_decodes > submitted_requests
            || completed_decodes > started_decodes
            || terminal_requests > submitted_requests
            || observed_requests > submitted_requests
            || observed_decodes > started_decodes
            || u64::try_from(pending_completions)
                .map_or(true, |pending| pending > terminal_requests)
            || u64::try_from(resident_resources)
                .map_or(true, |resident| resident > completed_decodes)
        {
            return Err(RuntimeFaultCode::DiagnosticsInvariantViolation);
        }

        Ok(Self {
            config,
            category_used,
            queued_requests,
            in_flight_decodes,
            pending_completions,
            resident_resources,
            submitted_requests,
            started_decodes,
            completed_decodes,
            ready_requests,
            cancelled_requests,
            failed_requests,
        })
    }

    pub const fn total_cap_bytes(self) -> u64 {
        self.config.total_cpu_bytes()
    }

    pub fn total_used_bytes(self) -> u64 {
        self.category_used.iter().copied().sum()
    }

    pub const fn category_cap_bytes(self, category: CpuLedgerCategory) -> u64 {
        self.config.category_cap(category)
    }

    pub const fn category_used_bytes(self, category: CpuLedgerCategory) -> u64 {
        self.category_used[category_index(category)]
    }

    pub const fn queued_requests(self) -> usize {
        self.queued_requests
    }

    pub const fn request_queue_limit(self) -> usize {
        self.config.request_queue_limit()
    }

    pub const fn in_flight_decodes(self) -> usize {
        self.in_flight_decodes
    }

    pub const fn worker_limit(self) -> usize {
        self.config.worker_limit()
    }

    pub const fn pending_completions(self) -> usize {
        self.pending_completions
    }

    pub const fn completion_queue_limit(self) -> usize {
        self.config.completion_queue_limit()
    }

    pub const fn resident_resources(self) -> usize {
        self.resident_resources
    }

    pub const fn submitted_requests(self) -> u64 {
        self.submitted_requests
    }

    pub const fn started_decodes(self) -> u64 {
        self.started_decodes
    }

    pub const fn completed_decodes(self) -> u64 {
        self.completed_decodes
    }

    pub const fn ready_requests(self) -> u64 {
        self.ready_requests
    }

    pub const fn cancelled_requests(self) -> u64 {
        self.cancelled_requests
    }

    pub const fn failed_requests(self) -> u64 {
        self.failed_requests
    }
}

/// Reservation-bound progress for one runtime-issued request ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeRequestProgress {
    ticket: RequestTicket,
    written_bytes: u64,
    reserved_bytes: u64,
}

impl RuntimeRequestProgress {
    pub const fn new(
        ticket: RequestTicket,
        written_bytes: u64,
        reserved_bytes: u64,
    ) -> Result<Self, RuntimeFaultCode> {
        if reserved_bytes == 0 || written_bytes > reserved_bytes {
            return Err(RuntimeFaultCode::ProgressInvariantViolation {
                written_bytes,
                reserved_bytes,
            });
        }
        Ok(Self {
            ticket,
            written_bytes,
            reserved_bytes,
        })
    }

    pub const fn ticket(self) -> RequestTicket {
        self.ticket
    }

    pub const fn written_bytes(self) -> u64 {
        self.written_bytes
    }

    pub const fn reserved_bytes(self) -> u64 {
        self.reserved_bytes
    }

    pub const fn is_complete(self) -> bool {
        self.written_bytes == self.reserved_bytes
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

#[cfg(test)]
#[derive(Debug)]
struct TestUsageCharge {
    usage: Arc<std::sync::Mutex<[u64; CPU_LEDGER_CATEGORIES.len()]>>,
    category: CpuLedgerCategory,
    bytes: u64,
}

#[cfg(test)]
impl Drop for TestUsageCharge {
    fn drop(&mut self) {
        let mut usage = self.usage.lock().unwrap();
        let used = &mut usage[category_index(self.category)];
        *used = used
            .checked_sub(self.bytes)
            .expect("a test ledger charge releases exactly once");
    }
}

#[derive(Debug)]
struct AccountedCpuCharge {
    charge: RuntimeCharge,
}

#[derive(Debug)]
enum RuntimeCharge {
    Production(ledger::LedgerCharge),
    #[cfg(test)]
    Test {
        bytes: u64,
        category: CpuLedgerCategory,
        _usage_charge: Option<TestUsageCharge>,
    },
}

impl RuntimeCharge {
    fn bytes(&self) -> u64 {
        match self {
            Self::Production(charge) => charge.bytes(),
            #[cfg(test)]
            Self::Test { bytes, .. } => *bytes,
        }
    }

    fn category(&self) -> CpuLedgerCategory {
        match self {
            Self::Production(charge) => charge.category(),
            #[cfg(test)]
            Self::Test { category, .. } => *category,
        }
    }
}

impl AccountedCpuLease {
    pub fn accounted_bytes(&self) -> u64 {
        self.inner.charge.bytes()
    }

    pub fn ledger_category(&self) -> CpuLedgerCategory {
        self.inner.charge.category()
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
    charge: RuntimeCharge,
}

impl AccountedResourceLease {
    pub fn accounted_bytes(&self) -> u64 {
        self.inner.charge.bytes()
    }

    pub fn ledger_category(&self) -> CpuLedgerCategory {
        self.inner.charge.category()
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
        let value_len = usize::try_from(self.inner.descriptor.value_byte_len())
            .expect("runtime-issued payload lengths fit the process address space");
        let (value_bytes, validity_bytes) = self.inner.bytes.split_at(value_len);
        let validity_bits = match self.inner.descriptor.validity() {
            mirante4d_dataset::ResourceValidity::AllValid => None,
            mirante4d_dataset::ResourceValidity::BitMask => Some(validity_bytes),
        };
        self.inner
            .descriptor
            .view(value_bytes, validity_bits)
            .expect("runtime-issued lease preserves its validated descriptor")
    }
}

impl CpuByteLease for AccountedResourceLease {
    fn category(&self) -> CpuLedgerCategory {
        self.inner.charge.category()
    }

    fn reserved_bytes(&self) -> u64 {
        self.inner.charge.bytes()
    }
}

impl CpuByteLease for AccountedCpuLease {
    fn category(&self) -> CpuLedgerCategory {
        self.inner.charge.category()
    }

    fn reserved_bytes(&self) -> u64 {
        self.inner.charge.bytes()
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFaultCode {
    #[error("the dataset runtime configuration is invalid")]
    InvalidConfiguration,
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
    #[error(
        "cancellation scope {request_scope} cannot be compared with current scope {current_scope}"
    )]
    CancellationScopeMismatch {
        request_scope: u64,
        current_scope: u64,
    },
    #[error("the runtime request identity counter is exhausted")]
    RequestIdExhausted,
    #[error(
        "request progress wrote {written_bytes} bytes into a reservation of {reserved_bytes} bytes"
    )]
    ProgressInvariantViolation {
        written_bytes: u64,
        reserved_bytes: u64,
    },
    #[error("the runtime diagnostics snapshot violates its configured bounds or counters")]
    DiagnosticsInvariantViolation,
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
                request_id: None,
                generation: Some(request.generation()),
                resource: Some(request.resource()),
            }),
        }
    }

    pub fn for_ticket(code: RuntimeFaultCode, ticket: RequestTicket) -> Self {
        Self {
            inner: Box::new(RuntimeFaultContext {
                code,
                request_id: Some(ticket.id()),
                generation: Some(ticket.generation()),
                resource: Some(ticket.resource()),
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

    pub const fn is_current(
        &self,
        current: CancellationGeneration,
    ) -> Result<bool, RuntimeFaultCode> {
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
    fn diagnostics(&self) -> Result<DatasetRuntimeDiagnostics, RuntimeFault>;
    fn progress(
        &self,
        ticket: RequestTicket,
    ) -> Result<Option<RuntimeRequestProgress>, RuntimeFault>;
    fn try_acquire_analysis_bytes(&self, bytes: u64) -> Result<AccountedCpuLease, RuntimeFault>;
    fn request_shutdown(&self) -> Result<(), RuntimeFault>;
    fn shutdown_state(&self) -> ShutdownState;
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::{Arc, Mutex},
    };

    use super::*;
    use mirante4d_dataset::{
        CpuLedgerError, DatasetResourceIdentity, ResourcePayloadDescriptor, ResourceRegion,
        ResourceValidity,
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
        resource: DatasetResourceKey,
        generation: CancellationGeneration,
    ) -> ResourceRequest {
        ResourceRequest::new(resource, RequestPriority::CurrentView, generation)
    }

    fn request(resource: DatasetResourceKey) -> ResourceRequest {
        request_at(resource, CancellationGeneration::new(7))
    }

    fn lease(resource: DatasetResourceKey) -> AccountedResourceLease {
        let descriptor = ResourcePayloadDescriptor::new(
            IntensityDType::Uint16,
            resource.region().shape(),
            ResourceValidity::AllValid,
        )
        .unwrap();
        AccountedResourceLease {
            inner: Arc::new(AccountedPayload {
                key: resource,
                descriptor,
                bytes: vec![1, 0, 2, 0].into_boxed_slice(),
                charge: RuntimeCharge::Test {
                    bytes: descriptor.byte_len(),
                    category: CpuLedgerCategory::DecodedResidency,
                    _usage_charge: None,
                },
            }),
        }
    }

    struct TestRuntime {
        state: Mutex<TestRuntimeState>,
        config: DatasetRuntimeConfig,
        poll_limit: usize,
        usage: Arc<Mutex<[u64; CPU_LEDGER_CATEGORIES.len()]>>,
    }

    struct TestRuntimeState {
        current_by_scope: BTreeMap<u64, CancellationGeneration>,
        shutdown: ShutdownState,
        next_request_id: Option<RuntimeRequestId>,
        pending: BTreeMap<RequestDedupeKey, TestDecodeJob>,
        completions: VecDeque<RuntimeCompletion>,
        progress: BTreeMap<RuntimeRequestId, RuntimeRequestProgress>,
        resident_resources: usize,
        submitted_requests: u64,
        started_decodes: u64,
        completed_decodes: u64,
        ready_requests: u64,
        cancelled_requests: u64,
        failed_requests: u64,
    }

    struct TestDecodeJob {
        waiters: Vec<TestWaiter>,
        started: bool,
    }

    #[derive(Clone, Copy)]
    struct TestWaiter {
        request: ResourceRequest,
        ticket: RequestTicket,
    }

    #[derive(Clone, Copy)]
    struct CapturedDecode {
        key: RequestDedupeKey,
    }

    struct TestCpuLease {
        charge: TestUsageCharge,
    }

    impl CpuByteLease for TestCpuLease {
        fn category(&self) -> CpuLedgerCategory {
            self.charge.category
        }

        fn reserved_bytes(&self) -> u64 {
            self.charge.bytes
        }
    }

    impl TestRuntime {
        fn new(current: CancellationGeneration, queue_capacity: usize, poll_limit: usize) -> Self {
            let config =
                DatasetRuntimeConfig::new(1 << 20, 4, queue_capacity, queue_capacity).unwrap();
            Self::with_config(current, config, poll_limit)
        }

        fn with_config(
            current: CancellationGeneration,
            config: DatasetRuntimeConfig,
            poll_limit: usize,
        ) -> Self {
            let current_by_scope = BTreeMap::from([(current.scope(), current)]);
            Self {
                state: Mutex::new(TestRuntimeState {
                    current_by_scope,
                    shutdown: ShutdownState::Running,
                    next_request_id: RuntimeRequestId::new(1),
                    pending: BTreeMap::new(),
                    completions: VecDeque::new(),
                    progress: BTreeMap::new(),
                    resident_resources: 0,
                    submitted_requests: 0,
                    started_decodes: 0,
                    completed_decodes: 0,
                    ready_requests: 0,
                    cancelled_requests: 0,
                    failed_requests: 0,
                }),
                config,
                poll_limit,
                usage: Arc::new(Mutex::new([0; CPU_LEDGER_CATEGORIES.len()])),
            }
        }

        fn in_flight_decode_count(&self) -> usize {
            self.state.lock().unwrap().pending.len()
        }

        fn completed_decode_count(&self) -> usize {
            usize::try_from(self.state.lock().unwrap().completed_decodes).unwrap()
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
            state.started_decodes += 1;
            CapturedDecode { key }
        }

        fn complete_captured(&self, captured: CapturedDecode) {
            let mut state = self.state.lock().unwrap();
            let job = state
                .pending
                .remove(&captured.key)
                .expect("a captured decode remains registered until completion");
            assert!(job.started);
            state.completed_decodes += 1;
            state.resident_resources += 1;
            let shared = lease(captured.key.resource());
            for waiter in job.waiters {
                let current = state
                    .current_by_scope
                    .get(&waiter.request.generation().scope())
                    .copied()
                    .expect("an admitted request retains its cancellation scope");
                if waiter.request.generation().is_current(current).unwrap() {
                    let progress = state
                        .progress
                        .get_mut(&waiter.ticket.id())
                        .expect("an admitted request retains progress until delivery");
                    *progress = RuntimeRequestProgress::new(
                        waiter.ticket,
                        progress.reserved_bytes(),
                        progress.reserved_bytes(),
                    )
                    .unwrap();
                    state.completions.push_back(RuntimeCompletion::new(
                        waiter.ticket,
                        RuntimeOutcome::Ready(shared.clone()),
                    ));
                    state.ready_requests += 1;
                }
            }
            assert!(state.completions.len() <= self.config.completion_queue_limit());
        }

        fn complete(&self, resource: DatasetResourceKey) {
            let captured = self.capture_decode(resource);
            self.complete_captured(captured);
        }

        fn reserve_usage(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<TestUsageCharge, RuntimeFaultCode> {
            if bytes == 0 {
                return Err(RuntimeFaultCode::InvariantViolation);
            }
            let mut usage = self.usage.lock().unwrap();
            let category_slot = category_index(category);
            let category_used = usage[category_slot];
            let total_used = usage.iter().try_fold(0_u64, |total, used| {
                total
                    .checked_add(*used)
                    .ok_or(RuntimeFaultCode::InvariantViolation)
            })?;
            let category_available = self
                .config
                .category_cap(category)
                .checked_sub(category_used)
                .ok_or(RuntimeFaultCode::InvariantViolation)?;
            let total_available = self
                .config
                .total_cpu_bytes()
                .checked_sub(total_used)
                .ok_or(RuntimeFaultCode::InvariantViolation)?;
            let available_bytes = category_available.min(total_available);
            if bytes > available_bytes {
                return Err(RuntimeFaultCode::CapacityExceeded {
                    category,
                    requested_bytes: bytes,
                    available_bytes,
                });
            }
            usage[category_slot] = category_used + bytes;
            drop(usage);
            Ok(TestUsageCharge {
                usage: Arc::clone(&self.usage),
                category,
                bytes,
            })
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
            let charge = self
                .reserve_usage(category, bytes)
                .map_err(|code| match code {
                    RuntimeFaultCode::CapacityExceeded {
                        category,
                        requested_bytes,
                        available_bytes,
                    } => CpuLedgerError::CapacityExceeded {
                        category,
                        requested_bytes,
                        available_bytes,
                    },
                    _ => CpuLedgerError::ShuttingDown,
                })?;
            Ok(Box::new(TestCpuLease { charge }))
        }
    }

    impl DatasetRuntime for TestRuntime {
        fn submit(&self, request: ResourceRequest) -> Result<RequestTicket, RuntimeFault> {
            let mut state = self.state.lock().unwrap();
            let id = state
                .next_request_id
                .ok_or_else(|| RuntimeFault::new(RuntimeFaultCode::RequestIdExhausted))?;
            state.next_request_id = id.get().checked_add(1).and_then(RuntimeRequestId::new);
            let ticket = RequestTicket::for_request(id, request);

            if state.shutdown != ShutdownState::Running {
                return Err(RuntimeFault::for_ticket(
                    RuntimeFaultCode::ShuttingDown,
                    ticket,
                ));
            }
            match state
                .current_by_scope
                .get(&request.generation().scope())
                .copied()
            {
                Some(current) => match request.generation().is_current(current) {
                    Ok(true) => {}
                    Ok(false) => {
                        return Err(RuntimeFault::for_ticket(
                            RuntimeFaultCode::StaleGeneration,
                            ticket,
                        ));
                    }
                    Err(code) => return Err(RuntimeFault::for_ticket(code, ticket)),
                },
                None => {
                    state
                        .current_by_scope
                        .insert(request.generation().scope(), request.generation());
                }
            }
            let admitted = state
                .pending
                .values()
                .map(|job| job.waiters.len())
                .sum::<usize>()
                .checked_add(state.completions.len())
                .ok_or_else(|| RuntimeFault::for_ticket(RuntimeFaultCode::QueueFull, ticket))?;
            if admitted >= self.config.request_queue_limit()
                || admitted >= self.config.completion_queue_limit()
            {
                return Err(RuntimeFault::for_ticket(
                    RuntimeFaultCode::QueueFull,
                    ticket,
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
                .push(TestWaiter { request, ticket });
            let reserved_bytes = ResourcePayloadDescriptor::new(
                IntensityDType::Uint16,
                request.resource().region().shape(),
                ResourceValidity::AllValid,
            )
            .map_err(|_| RuntimeFault::for_ticket(RuntimeFaultCode::InvariantViolation, ticket))?
            .byte_len();
            let progress = RuntimeRequestProgress::new(ticket, 0, reserved_bytes)
                .map_err(|code| RuntimeFault::for_ticket(code, ticket))?;
            state.progress.insert(ticket.id(), progress);
            state.submitted_requests += 1;
            Ok(ticket)
        }

        fn cancel_before(&self, current: CancellationGeneration) -> Result<(), RuntimeFault> {
            let mut state = self.state.lock().unwrap();
            if state.shutdown != ShutdownState::Running {
                return Err(RuntimeFault::new(RuntimeFaultCode::ShuttingDown));
            }
            let moves_backwards = state
                .current_by_scope
                .get(&current.scope())
                .copied()
                .map(|previous| current.is_stale_for(previous))
                .transpose()
                .map_err(RuntimeFault::new)?
                .unwrap_or(false);
            if moves_backwards {
                return Err(RuntimeFault::new(RuntimeFaultCode::StaleGeneration));
            }
            state.current_by_scope.insert(current.scope(), current);
            let mut cancelled = Vec::new();
            state.pending.retain(|_, job| {
                job.waiters.retain(|waiter| {
                    if waiter.request.generation().scope() != current.scope() {
                        return true;
                    }
                    if waiter
                        .request
                        .generation()
                        .is_stale_for(current)
                        .expect("same-scope cancellation was selected explicitly")
                    {
                        cancelled.push(waiter.ticket);
                        false
                    } else {
                        true
                    }
                });
                job.started || !job.waiters.is_empty()
            });
            for ticket in cancelled {
                state.progress.remove(&ticket.id());
                state
                    .completions
                    .push_back(RuntimeCompletion::new(ticket, RuntimeOutcome::Cancelled));
                state.cancelled_requests += 1;
            }
            assert!(state.completions.len() <= self.config.completion_queue_limit());
            Ok(())
        }

        fn poll(&self, max_completions: usize) -> Result<Vec<RuntimeCompletion>, RuntimeFault> {
            let mut state = self.state.lock().unwrap();
            let count = max_completions
                .min(self.poll_limit)
                .min(state.completions.len());
            let drained: Vec<_> = state.completions.drain(..count).collect();
            for completion in &drained {
                state.progress.remove(&completion.ticket().id());
            }
            if state.shutdown == ShutdownState::Draining
                && state.pending.is_empty()
                && state.completions.is_empty()
            {
                state.shutdown = ShutdownState::Stopped;
            }
            Ok(drained)
        }

        fn diagnostics(&self) -> Result<DatasetRuntimeDiagnostics, RuntimeFault> {
            let state = self.state.lock().unwrap();
            let category_used = *self.usage.lock().unwrap();
            let queued_requests = state
                .pending
                .values()
                .filter(|job| !job.started)
                .map(|job| job.waiters.len())
                .sum();
            let in_flight_decodes = state.pending.values().filter(|job| job.started).count();
            DatasetRuntimeDiagnostics::new(
                self.config,
                category_used,
                queued_requests,
                in_flight_decodes,
                state.completions.len(),
                state.resident_resources,
                state.submitted_requests,
                state.started_decodes,
                state.completed_decodes,
                state.ready_requests,
                state.cancelled_requests,
                state.failed_requests,
            )
            .map_err(RuntimeFault::new)
        }

        fn progress(
            &self,
            ticket: RequestTicket,
        ) -> Result<Option<RuntimeRequestProgress>, RuntimeFault> {
            let progress = self
                .state
                .lock()
                .unwrap()
                .progress
                .get(&ticket.id())
                .copied();
            if progress.is_some_and(|progress| progress.ticket() != ticket) {
                return Err(RuntimeFault::for_ticket(
                    RuntimeFaultCode::ProgressInvariantViolation {
                        written_bytes: u64::MAX,
                        reserved_bytes: 0,
                    },
                    ticket,
                ));
            }
            Ok(progress)
        }

        fn try_acquire_analysis_bytes(
            &self,
            bytes: u64,
        ) -> Result<AccountedCpuLease, RuntimeFault> {
            let charge = self
                .reserve_usage(CpuLedgerCategory::QueuesAndResults, bytes)
                .map_err(RuntimeFault::new)?;
            Ok(AccountedCpuLease {
                inner: Arc::new(AccountedCpuCharge {
                    charge: RuntimeCharge::Test {
                        bytes,
                        category: CpuLedgerCategory::QueuesAndResults,
                        _usage_charge: Some(charge),
                    },
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
        let issued = CancellationGeneration::for_scope(19, 4);
        assert_eq!(issued.scope(), 19);
        assert!(
            issued
                .is_stale_for(CancellationGeneration::for_scope(19, 5))
                .unwrap()
        );
        assert!(!issued.is_stale_for(issued).unwrap());
        assert_eq!(
            issued.checked_next().unwrap(),
            CancellationGeneration::for_scope(19, 5)
        );
        assert_eq!(CancellationGeneration::new(4).scope(), 0);
        assert_eq!(
            issued.is_stale_for(CancellationGeneration::for_scope(20, 5)),
            Err(RuntimeFaultCode::CancellationScopeMismatch {
                request_scope: 19,
                current_scope: 20,
            })
        );
        assert_eq!(
            CancellationGeneration::for_scope(19, u64::MAX).checked_next(),
            Err(RuntimeFaultCode::GenerationExhausted)
        );

        let runtime = TestRuntime::new(CancellationGeneration::new(7), 8, 8);
        let resource = key(0);
        let stale = request(resource);
        let stale_ticket = runtime.submit(stale).unwrap();
        let captured = runtime.capture_decode(resource);
        assert_eq!(runtime.in_flight_decode_count(), 1);
        runtime
            .cancel_before(CancellationGeneration::new(8))
            .unwrap();
        assert_eq!(runtime.in_flight_decode_count(), 1);
        let fault = runtime.submit(stale).unwrap_err();
        assert_eq!(fault.code(), RuntimeFaultCode::StaleGeneration);

        let current = request_at(resource, CancellationGeneration::new(8));
        let current_ticket = runtime.submit(current).unwrap();
        runtime.complete_captured(captured);
        assert_eq!(runtime.completed_decode_count(), 1);
        let completions = runtime.poll(8).unwrap();
        assert!(completions.iter().any(|completion| {
            completion.ticket() == stale_ticket
                && matches!(completion.outcome(), RuntimeOutcome::Cancelled)
        }));
        assert!(completions.iter().any(|completion| {
            completion.ticket() == current_ticket
                && matches!(completion.outcome(), RuntimeOutcome::Ready(_))
        }));
        assert!(!completions.iter().any(|completion| {
            completion.ticket() == stale_ticket
                && matches!(completion.outcome(), RuntimeOutcome::Ready(_))
        }));
    }

    #[test]
    fn cancellation_scopes_are_isolated_on_one_deduplicated_decode() {
        let resource = key(0);
        let view_generation = CancellationGeneration::for_scope(11, 7);
        let verification_generation = CancellationGeneration::for_scope(29, 3);
        let runtime = TestRuntime::new(view_generation, 8, 8);
        let view_ticket = runtime
            .submit(request_at(resource, view_generation))
            .unwrap();
        let verification_ticket = runtime
            .submit(request_at(resource, verification_generation))
            .unwrap();
        let captured = runtime.capture_decode(resource);

        runtime
            .cancel_before(view_generation.checked_next().unwrap())
            .unwrap();
        runtime.complete_captured(captured);

        assert_eq!(runtime.completed_decode_count(), 1);
        let completions = runtime.poll(8).unwrap();
        assert!(completions.iter().any(|completion| {
            completion.ticket() == view_ticket
                && matches!(completion.outcome(), RuntimeOutcome::Cancelled)
        }));
        assert!(completions.iter().any(|completion| {
            completion.ticket() == verification_ticket
                && matches!(completion.outcome(), RuntimeOutcome::Ready(_))
        }));
    }

    #[test]
    fn submit_assigns_unique_ids_while_deduping_by_semantic_resource() {
        let resource = key(0);
        let first = request(resource);
        let second = request(resource);
        assert_eq!(first.dedupe_key(), second.dedupe_key());
        assert_ne!(first.dedupe_key(), request(key(1)).dedupe_key());

        let runtime = TestRuntime::new(CancellationGeneration::new(7), 8, 8);
        let first_ticket = runtime.submit(first).unwrap();
        let second_ticket = runtime.submit(second).unwrap();
        assert_ne!(first_ticket.id(), second_ticket.id());
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

        let exhausted = TestRuntime::new(CancellationGeneration::new(7), 8, 8);
        exhausted.state.lock().unwrap().next_request_id = RuntimeRequestId::new(u64::MAX);
        assert_eq!(
            exhausted.submit(request(key(4))).unwrap().id().get(),
            u64::MAX
        );
        assert_eq!(
            exhausted.submit(request(key(5))).unwrap_err().code(),
            RuntimeFaultCode::RequestIdExhausted
        );
    }

    #[test]
    fn runtime_config_uses_exact_hard_category_fractions() {
        let config = DatasetRuntimeConfig::new(1_000, 3, 17, 11).unwrap();
        assert_eq!(config.total_cpu_bytes(), 1_000);
        assert_eq!(config.worker_limit(), 3);
        assert_eq!(config.request_queue_limit(), 17);
        assert_eq!(config.completion_queue_limit(), 11);
        assert_eq!(
            config.category_cap(CpuLedgerCategory::DecodedResidency),
            500
        );
        assert_eq!(config.category_cap(CpuLedgerCategory::InFlightDecode), 125);
        assert_eq!(config.category_cap(CpuLedgerCategory::UploadStaging), 125);
        assert_eq!(
            config.category_cap(CpuLedgerCategory::MetadataAndIndexes),
            100
        );
        assert_eq!(config.category_cap(CpuLedgerCategory::QueuesAndResults), 50);
        assert_eq!(config.category_cap(CpuLedgerCategory::Prefetch), 50);
        assert_eq!(config.category_cap(CpuLedgerCategory::ImportWorkingSet), 50);
        assert_eq!(config.allocated_category_bytes(), 1_000);

        let rounded = DatasetRuntimeConfig::new(41, 1, 1, 1).unwrap();
        assert_eq!(rounded.allocated_category_bytes(), 40);
        assert!(rounded.allocated_category_bytes() <= rounded.total_cpu_bytes());
        let maximum = DatasetRuntimeConfig::new(u64::MAX, 1, 1, 1).unwrap();
        assert!(maximum.allocated_category_bytes() <= maximum.total_cpu_bytes());
        assert_eq!(
            DatasetRuntimeConfig::new(0, 1, 1, 1),
            Err(RuntimeFaultCode::InvalidConfiguration)
        );
        assert_eq!(
            DatasetRuntimeConfig::new(1_000, 0, 1, 1),
            Err(RuntimeFaultCode::InvalidConfiguration)
        );
        assert_eq!(
            DatasetRuntimeConfig::new(1_000, 1, 0, 1),
            Err(RuntimeFaultCode::InvalidConfiguration)
        );
        assert_eq!(
            DatasetRuntimeConfig::new(1_000, 1, 1, 0),
            Err(RuntimeFaultCode::InvalidConfiguration)
        );
    }

    #[test]
    fn diagnostics_and_progress_reject_impossible_states() {
        let config = DatasetRuntimeConfig::new(1_000, 2, 3, 4).unwrap();
        let valid = DatasetRuntimeDiagnostics::new(
            config,
            [10, 0, 0, 0, 0, 0, 0],
            1,
            1,
            1,
            1,
            3,
            2,
            1,
            1,
            1,
            0,
        )
        .unwrap();
        assert_eq!(valid.total_cap_bytes(), 1_000);
        assert_eq!(valid.total_used_bytes(), 10);
        assert_eq!(
            valid.category_used_bytes(CpuLedgerCategory::DecodedResidency),
            10
        );
        assert_eq!(valid.queued_requests(), 1);
        assert_eq!(valid.in_flight_decodes(), 1);
        assert_eq!(valid.pending_completions(), 1);
        assert_eq!(valid.resident_resources(), 1);

        assert_eq!(
            DatasetRuntimeDiagnostics::new(
                config,
                [501, 0, 0, 0, 0, 0, 0],
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ),
            Err(RuntimeFaultCode::DiagnosticsInvariantViolation)
        );
        assert_eq!(
            DatasetRuntimeDiagnostics::new(config, [0; 7], 4, 0, 0, 0, 0, 0, 0, 0, 0, 0,),
            Err(RuntimeFaultCode::DiagnosticsInvariantViolation)
        );
        assert_eq!(
            DatasetRuntimeDiagnostics::new(config, [0; 7], 1, 0, 0, 0, 0, 0, 0, 0, 0, 0,),
            Err(RuntimeFaultCode::DiagnosticsInvariantViolation)
        );
        assert_eq!(
            DatasetRuntimeDiagnostics::new(config, [0; 7], 0, 1, 0, 0, 1, 0, 0, 0, 0, 0,),
            Err(RuntimeFaultCode::DiagnosticsInvariantViolation)
        );

        let runtime = TestRuntime::with_config(CancellationGeneration::new(7), config, 4);
        let ticket = runtime.submit(request(key(0))).unwrap();
        let progress = runtime.progress(ticket).unwrap().unwrap();
        assert_eq!(progress.written_bytes(), 0);
        assert_eq!(progress.reserved_bytes(), 4);
        assert!(!progress.is_complete());
        assert_eq!(
            RuntimeRequestProgress::new(ticket, 5, 4),
            Err(RuntimeFaultCode::ProgressInvariantViolation {
                written_bytes: 5,
                reserved_bytes: 4,
            })
        );
        assert_eq!(
            RuntimeRequestProgress::new(ticket, 0, 0),
            Err(RuntimeFaultCode::ProgressInvariantViolation {
                written_bytes: 0,
                reserved_bytes: 0,
            })
        );

        let capacity = config.category_cap(CpuLedgerCategory::QueuesAndResults);
        let charge = runtime.try_acquire_analysis_bytes(capacity).unwrap();
        let diagnostics = runtime.diagnostics().unwrap();
        assert_eq!(
            diagnostics.category_used_bytes(CpuLedgerCategory::QueuesAndResults),
            capacity
        );
        assert_eq!(
            runtime.try_acquire_analysis_bytes(1).unwrap_err().code(),
            RuntimeFaultCode::CapacityExceeded {
                category: CpuLedgerCategory::QueuesAndResults,
                requested_bytes: 1,
                available_bytes: 0,
            }
        );
        drop(charge);
        assert_eq!(
            runtime
                .diagnostics()
                .unwrap()
                .category_used_bytes(CpuLedgerCategory::QueuesAndResults),
            0
        );
    }

    #[test]
    fn lease_clones_share_one_accounted_immutable_allocation() {
        let lease = lease(key(0));
        let clone = lease.clone();
        assert!(lease.shares_allocation_with(&clone));
        assert_eq!(lease.accounted_bytes(), 4);
        assert_eq!(lease.ledger_category(), CpuLedgerCategory::DecodedResidency);
        assert_eq!(lease.payload().value_bytes(), &[1, 0, 2, 0]);
        assert_eq!(lease.payload().validity_bits(), None);
        assert_eq!(lease.key(), clone.key());
        assert_eq!(CpuByteLease::reserved_bytes(&lease), 4);

        let masked_descriptor = ResourcePayloadDescriptor::new(
            IntensityDType::Uint8,
            Shape3D::new(1, 1, 2).unwrap(),
            ResourceValidity::BitMask,
        )
        .unwrap();
        let masked = AccountedResourceLease {
            inner: Arc::new(AccountedPayload {
                key: key(1),
                descriptor: masked_descriptor,
                bytes: vec![0, 17, 0b0000_0001].into_boxed_slice(),
                charge: RuntimeCharge::Test {
                    bytes: masked_descriptor.byte_len(),
                    category: CpuLedgerCategory::DecodedResidency,
                    _usage_charge: None,
                },
            }),
        };
        assert_eq!(masked.accounted_bytes(), 3);
        assert_eq!(masked.payload().value_bytes(), &[0, 17]);
        assert_eq!(masked.payload().validity_bits(), Some(&[0b0000_0001][..]));
        assert_eq!(masked.payload().sample_is_valid(0), Ok(true));
        assert_eq!(masked.payload().sample_is_valid(1), Ok(false));

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
        let runtime = TestRuntime::new(CancellationGeneration::new(7), 8, 8);
        let request = request(key(0));
        let ticket = runtime.submit(request).unwrap();
        assert!(ticket.is_current(CancellationGeneration::new(7)).unwrap());
        assert!(!ticket.is_current(CancellationGeneration::new(8)).unwrap());
        assert_eq!(
            ticket.is_current(CancellationGeneration::for_scope(8, 7)),
            Err(RuntimeFaultCode::CancellationScopeMismatch {
                request_scope: 0,
                current_scope: 8,
            })
        );

        let fault = RuntimeFault::for_ticket(RuntimeFaultCode::StaleGeneration, ticket);
        assert_eq!(fault.request_id(), Some(ticket.id()));
        assert_eq!(fault.generation(), Some(request.generation()));
        assert_eq!(fault.resource(), Some(request.resource()));
    }

    #[test]
    fn polling_is_runtime_bounded_and_shutdown_drains_before_stopping() {
        let runtime = TestRuntime::new(CancellationGeneration::new(7), 8, 2);
        for origin in 1..=3 {
            let resource = key(origin);
            runtime.submit(request(resource)).unwrap();
            runtime.complete(resource);
        }

        assert_eq!(runtime.poll(usize::MAX).unwrap().len(), 2);
        assert_eq!(runtime.poll(usize::MAX).unwrap().len(), 1);
        assert_eq!(runtime.shutdown_state(), ShutdownState::Running);
        runtime.request_shutdown().unwrap();
        assert_eq!(runtime.shutdown_state(), ShutdownState::Draining);
        assert!(runtime.poll(0).unwrap().is_empty());
        assert_eq!(runtime.shutdown_state(), ShutdownState::Stopped);

        let fault = runtime.submit(request(key(9))).unwrap_err();
        assert_eq!(fault.code(), RuntimeFaultCode::ShuttingDown);
    }
}

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BinaryHeap, VecDeque},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
    thread::{self, JoinHandle},
};

use mirante4d_dataset::{
    CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError, DatasetCatalog,
    DatasetResourceKey, DatasetSource, DatasetSourceFault, DecodeSinkError, ReservedDecodeSink,
    ResourcePayloadDescriptor,
};

use crate::{
    AccountedCpuCharge, AccountedCpuLease, AccountedPayload, AccountedResourceLease,
    CancellationGeneration, DatasetRuntime, DatasetRuntimeConfig, DatasetRuntimeDiagnostics,
    RequestDedupeKey, RequestPriority, RequestTicket, ResourceRequest, RuntimeCharge,
    RuntimeCompletion, RuntimeFault, RuntimeFaultCode, RuntimeOutcome, RuntimeRequestId,
    RuntimeRequestProgress, ShutdownState,
    ledger::{LedgerCharge, LedgerCore, LedgerHandle},
};

// These are explicit conservative charges for bounded scheduler metadata.
// Payload bytes are charged separately and exactly.
const REQUEST_RECORD_BYTES: u64 = 512;
const CACHE_RECORD_BYTES: u64 = 192;
const SCOPE_RECORD_BYTES: u64 = 128;

struct ProductionDatasetRuntime {
    shared: Arc<RuntimeShared>,
    // The supervisor joins every decode worker. Dropping this handle detaches
    // only the supervisor; it continues joining workers off the caller path.
    supervisor: Mutex<Option<JoinHandle<()>>>,
}

struct RuntimeShared {
    source: Arc<dyn DatasetSource>,
    catalog: Arc<DatasetCatalog>,
    config: DatasetRuntimeConfig,
    ledger: Arc<LedgerCore>,
    state: Mutex<RuntimeState>,
    work_available: Condvar,
}

struct RuntimeState {
    current_by_scope: BTreeMap<u64, ScopeRecord>,
    shutdown: ShutdownState,
    workers_joined: bool,
    next_request_id: Option<u64>,
    next_job_id: u64,
    next_queue_sequence: u64,
    next_cache_touch: u64,
    requests: BTreeMap<RuntimeRequestId, RequestRecord>,
    jobs: BTreeMap<u64, DecodeJob>,
    dedupe: BTreeMap<RequestDedupeKey, u64>,
    queue: BinaryHeap<QueueEntry>,
    completions: VecDeque<RuntimeCompletion>,
    cache: BTreeMap<DatasetResourceKey, CacheEntry>,
    submitted_requests: u64,
    started_decodes: u64,
    completed_decodes: u64,
    ready_requests: u64,
    cancelled_requests: u64,
    failed_requests: u64,
}

struct ScopeRecord {
    current: CancellationGeneration,
    _charge: LedgerCharge,
}

struct RequestRecord {
    request: ResourceRequest,
    ticket: RequestTicket,
    progress: RuntimeRequestProgress,
    job_id: Option<u64>,
    terminal: bool,
    _charge: LedgerCharge,
}

struct DecodeJob {
    key: RequestDedupeKey,
    descriptor: ResourcePayloadDescriptor,
    waiters: Vec<RuntimeRequestId>,
    priority: RequestPriority,
    phase: JobPhase,
    decode_started: bool,
    queue_version: u64,
    queue_sequence: u64,
    cancellation: Arc<AtomicBool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobPhase {
    Queued,
    Claimed,
    InFlight,
    Aborting,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct QueueEntry {
    priority: RequestPriority,
    sequence: u64,
    job_id: u64,
    version: u64,
}

impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            // Earlier sequence wins within one priority class.
            .then_with(|| other.sequence.cmp(&self.sequence))
            .then_with(|| other.job_id.cmp(&self.job_id))
    }
}

impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct CacheEntry {
    lease: AccountedResourceLease,
    last_touch: u64,
    _charge: LedgerCharge,
}

struct JobClaim {
    job_id: u64,
    key: DatasetResourceKey,
    descriptor: ResourcePayloadDescriptor,
    cancellation: Arc<AtomicBool>,
}

impl RuntimeState {
    fn new() -> Self {
        Self {
            current_by_scope: BTreeMap::new(),
            shutdown: ShutdownState::Running,
            workers_joined: false,
            next_request_id: Some(1),
            next_job_id: 1,
            next_queue_sequence: 1,
            next_cache_touch: 1,
            requests: BTreeMap::new(),
            jobs: BTreeMap::new(),
            dedupe: BTreeMap::new(),
            queue: BinaryHeap::new(),
            completions: VecDeque::new(),
            cache: BTreeMap::new(),
            submitted_requests: 0,
            started_decodes: 0,
            completed_decodes: 0,
            ready_requests: 0,
            cancelled_requests: 0,
            failed_requests: 0,
        }
    }

    fn allocate_request_id(&mut self) -> Result<RuntimeRequestId, RuntimeFaultCode> {
        let value = self
            .next_request_id
            .ok_or(RuntimeFaultCode::RequestIdExhausted)?;
        let id = RuntimeRequestId::new(value).ok_or(RuntimeFaultCode::RequestIdExhausted)?;
        self.next_request_id = value.checked_add(1);
        Ok(id)
    }

    fn allocate_job_id(&mut self) -> Result<u64, RuntimeFaultCode> {
        let id = self.next_job_id;
        self.next_job_id = self
            .next_job_id
            .checked_add(1)
            .ok_or(RuntimeFaultCode::InvariantViolation)?;
        Ok(id)
    }

    fn allocate_queue_sequence(&mut self) -> Result<u64, RuntimeFaultCode> {
        let sequence = self.next_queue_sequence;
        self.next_queue_sequence = self
            .next_queue_sequence
            .checked_add(1)
            .ok_or(RuntimeFaultCode::InvariantViolation)?;
        Ok(sequence)
    }

    fn touch(&mut self) -> u64 {
        let touch = self.next_cache_touch;
        self.next_cache_touch = self.next_cache_touch.saturating_add(1);
        touch
    }

    fn push_completion(&mut self, completion: RuntimeCompletion, limit: usize) {
        self.completions.push_back(completion);
        assert!(
            self.completions.len() <= limit,
            "each admitted request reserves one bounded completion slot"
        );
    }

    fn remove_job(&mut self, job_id: u64) -> Option<DecodeJob> {
        let job = self.jobs.remove(&job_id)?;
        self.queue.retain(|entry| entry.job_id != job_id);
        if self.dedupe.get(&job.key).copied() == Some(job_id) {
            self.dedupe.remove(&job.key);
        }
        Some(job)
    }

    fn replace_queued_entry(&mut self, entry: QueueEntry) {
        self.queue.retain(|queued| queued.job_id != entry.job_id);
        self.queue.push(entry);
        assert!(
            self.queue.len() <= self.jobs.len(),
            "one charged live decode job owns at most one scheduler entry"
        );
    }
}

impl dyn DatasetRuntime {
    pub fn start(
        config: DatasetRuntimeConfig,
        source_factory: impl FnOnce(
            Arc<dyn CpuByteLedger>,
        ) -> Result<Arc<dyn DatasetSource>, RuntimeFault>,
    ) -> Result<(Arc<dyn DatasetRuntime>, Arc<DatasetCatalog>), RuntimeFault> {
        let ledger = LedgerCore::new(config);
        let source_ledger: Arc<dyn CpuByteLedger> = Arc::new(LedgerHandle(Arc::clone(&ledger)));
        let source = catch_unwind(AssertUnwindSafe(|| source_factory(source_ledger)))
            .map_err(|_| RuntimeFault::new(RuntimeFaultCode::InvariantViolation))??;
        let catalog = catch_unwind(AssertUnwindSafe(|| {
            source
                .catalog()
                .map_err(|fault| map_source_fault_code(&fault))
        }))
        .map_err(|_| RuntimeFault::new(RuntimeFaultCode::InvariantViolation))?
        .map_err(RuntimeFault::new)?;

        let shared = Arc::new(RuntimeShared {
            source,
            catalog: Arc::clone(&catalog),
            config,
            ledger,
            state: Mutex::new(RuntimeState::new()),
            work_available: Condvar::new(),
        });

        let mut workers = Vec::with_capacity(config.worker_limit());
        for index in 0..config.worker_limit() {
            let worker_shared = Arc::clone(&shared);
            match thread::Builder::new()
                .name(format!("mirante4d-dataset-runtime-{index}"))
                .spawn(move || worker_loop(worker_shared))
            {
                Ok(worker) => workers.push(worker),
                Err(_) => {
                    shared.begin_shutdown();
                    for worker in workers {
                        let _ = worker.join();
                    }
                    return Err(RuntimeFault::new(RuntimeFaultCode::InvariantViolation));
                }
            }
        }

        let supervisor_shared = Arc::clone(&shared);
        let supervisor = thread::Builder::new()
            .name("mirante4d-dataset-runtime-supervisor".to_owned())
            .spawn(move || {
                for worker in workers {
                    let _ = worker.join();
                }
                supervisor_shared.mark_workers_joined();
            })
            .map_err(|_| {
                shared.begin_shutdown();
                RuntimeFault::new(RuntimeFaultCode::InvariantViolation)
            })?;

        let runtime: Arc<dyn DatasetRuntime> = Arc::new(ProductionDatasetRuntime {
            shared,
            supervisor: Mutex::new(Some(supervisor)),
        });
        Ok((runtime, catalog))
    }
}

impl RuntimeShared {
    fn lock_state(&self) -> std::sync::MutexGuard<'_, RuntimeState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn reserve_scope_record(&self, scope: u64) -> Result<Option<LedgerCharge>, RuntimeFaultCode> {
        let state = self.lock_state();
        if state.shutdown != ShutdownState::Running {
            return Err(RuntimeFaultCode::ShuttingDown);
        }
        if state.current_by_scope.contains_key(&scope) {
            return Ok(None);
        }
        // Scope currentness cannot be evicted safely. Reuse the frozen request
        // limit as the hard count bound and reject new scopes once it is full.
        if state.current_by_scope.len() >= self.config.request_queue_limit() {
            return Err(RuntimeFaultCode::QueueFull);
        }
        drop(state);

        self.ledger
            .acquire(CpuLedgerCategory::QueuesAndResults, SCOPE_RECORD_BYTES)
            .map(Some)
            .map_err(map_ledger_error_code)
    }

    fn begin_shutdown(&self) {
        self.ledger.stop_accepting();
        let mut state = self.lock_state();
        if state.shutdown != ShutdownState::Running {
            return;
        }
        state.shutdown = ShutdownState::Draining;
        let ids = state
            .requests
            .iter()
            .filter_map(|(id, record)| (!record.terminal).then_some(*id))
            .collect::<Vec<_>>();
        cancel_request_ids(&mut state, &ids, self.config.completion_queue_limit());
        for job in state.jobs.values_mut() {
            if matches!(
                job.phase,
                JobPhase::Claimed | JobPhase::InFlight | JobPhase::Aborting
            ) {
                job.phase = JobPhase::Aborting;
                job.cancellation.store(true, AtomicOrdering::Release);
            }
        }
        let queued = state
            .jobs
            .iter()
            .filter_map(|(id, job)| (job.phase == JobPhase::Queued).then_some(*id))
            .collect::<Vec<_>>();
        for id in queued {
            state.remove_job(id);
        }
        let cache = std::mem::take(&mut state.cache);
        drop(state);
        drop(cache);
        self.work_available.notify_all();
    }

    fn mark_workers_joined(&self) {
        let mut state = self.lock_state();
        state.workers_joined = true;
        if state.shutdown == ShutdownState::Draining && state.completions.is_empty() {
            state.shutdown = ShutdownState::Stopped;
        }
        self.work_available.notify_all();
    }

    fn claim_job(&self) -> Option<JobClaim> {
        let mut state = self.lock_state();
        loop {
            while let Some(entry) = state.queue.pop() {
                let Some(job) = state.jobs.get_mut(&entry.job_id) else {
                    continue;
                };
                if job.phase != JobPhase::Queued
                    || job.queue_version != entry.version
                    || job.priority != entry.priority
                {
                    continue;
                }
                job.phase = JobPhase::Claimed;
                return Some(JobClaim {
                    job_id: entry.job_id,
                    key: job.key.resource(),
                    descriptor: job.descriptor,
                    cancellation: Arc::clone(&job.cancellation),
                });
            }
            if state.shutdown != ShutdownState::Running {
                return None;
            }
            state = self
                .work_available
                .wait(state)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }

    fn requeue_claim(&self, claim: &JobClaim) {
        let mut state = self.lock_state();
        let running = state.shutdown == ShutdownState::Running;
        let Some(job) = state.jobs.get_mut(&claim.job_id) else {
            return;
        };
        if job.waiters.is_empty() || !running {
            job.phase = JobPhase::Aborting;
            job.cancellation.store(true, AtomicOrdering::Release);
            return;
        }
        job.phase = JobPhase::Queued;
        job.queue_version = job.queue_version.saturating_add(1);
        let entry = QueueEntry {
            priority: job.priority,
            sequence: job.queue_sequence,
            job_id: claim.job_id,
            version: job.queue_version,
        };
        state.replace_queued_entry(entry);
        self.work_available.notify_one();
    }

    fn activate_claim(&self, claim: &JobClaim) -> bool {
        let mut state = self.lock_state();
        let Some(job) = state.jobs.get_mut(&claim.job_id) else {
            return false;
        };
        if job.waiters.is_empty() || job.phase == JobPhase::Aborting {
            state.remove_job(claim.job_id);
            return false;
        }
        job.phase = JobPhase::InFlight;
        job.decode_started = true;
        state.started_decodes = state.started_decodes.saturating_add(1);
        true
    }

    fn update_progress(&self, job_id: u64, written_bytes: u64, reserved_bytes: u64) {
        let mut state = self.lock_state();
        let Some(job) = state.jobs.get(&job_id) else {
            return;
        };
        let waiters = job.waiters.clone();
        for id in waiters {
            if let Some(record) = state.requests.get_mut(&id)
                && !record.terminal
            {
                record.progress =
                    RuntimeRequestProgress::new(record.ticket, written_bytes, reserved_bytes)
                        .expect("the reservation-bound sink reports bounded progress");
            }
        }
    }

    fn finish_failure(&self, job_id: u64, code: RuntimeFaultCode, started: bool) {
        let mut state = self.lock_state();
        let Some(job) = state.remove_job(job_id) else {
            return;
        };
        if started {
            state.completed_decodes = state.completed_decodes.saturating_add(1);
        }
        for id in job.waiters {
            let Some(record) = state.requests.get_mut(&id) else {
                continue;
            };
            if record.terminal {
                continue;
            }
            record.terminal = true;
            let ticket = record.ticket;
            let outcome = if code == RuntimeFaultCode::Cancelled {
                state.cancelled_requests = state.cancelled_requests.saturating_add(1);
                RuntimeOutcome::Cancelled
            } else {
                state.failed_requests = state.failed_requests.saturating_add(1);
                RuntimeOutcome::Failed(RuntimeFault::for_ticket(code, ticket))
            };
            state.push_completion(
                RuntimeCompletion::new(ticket, outcome),
                self.config.completion_queue_limit(),
            );
        }
    }

    fn finish_success(&self, job_id: u64, bytes: Box<[u8]>, charge: LedgerCharge) {
        let target = {
            let state = self.lock_state();
            let Some(job) = state.jobs.get(&job_id) else {
                return;
            };
            if job.waiters.is_empty() {
                drop(state);
                self.finish_failure(job_id, RuntimeFaultCode::Cancelled, true);
                return;
            }
            destination_category(job.priority)
        };

        if let Err(error) = self.reclassify_with_eviction(job_id, &charge, target) {
            self.finish_failure(job_id, map_ledger_error_code(error), true);
            return;
        }

        let cache_charge = self
            .ledger
            .acquire(CpuLedgerCategory::QueuesAndResults, CACHE_RECORD_BYTES)
            .ok();
        let mut state = self.lock_state();
        let Some(job) = state.remove_job(job_id) else {
            return;
        };
        state.completed_decodes = state.completed_decodes.saturating_add(1);
        if job.waiters.is_empty() {
            return;
        }
        let lease = AccountedResourceLease {
            inner: Arc::new(AccountedPayload {
                key: job.key.resource(),
                descriptor: job.descriptor,
                bytes,
                charge: RuntimeCharge::Production(charge),
            }),
        };
        if let Some(cache_charge) = cache_charge {
            let touch = state.touch();
            state.cache.insert(
                job.key.resource(),
                CacheEntry {
                    lease: lease.clone(),
                    last_touch: touch,
                    _charge: cache_charge,
                },
            );
        }
        for id in job.waiters {
            let Some(record) = state.requests.get_mut(&id) else {
                continue;
            };
            if record.terminal {
                continue;
            }
            record.progress = RuntimeRequestProgress::new(
                record.ticket,
                job.descriptor.byte_len(),
                job.descriptor.byte_len(),
            )
            .expect("a completed decode filled its exact reservation");
            record.terminal = true;
            let ticket = record.ticket;
            state.ready_requests = state.ready_requests.saturating_add(1);
            state.push_completion(
                RuntimeCompletion::new(ticket, RuntimeOutcome::Ready(lease.clone())),
                self.config.completion_queue_limit(),
            );
        }
    }

    fn reclassify_with_eviction(
        &self,
        job_id: u64,
        charge: &LedgerCharge,
        target: CpuLedgerCategory,
    ) -> Result<(), CpuLedgerError> {
        let mut considered: Vec<DatasetResourceKey> = Vec::new();
        loop {
            match charge.reclassify(target) {
                Ok(()) => return Ok(()),
                Err(error @ CpuLedgerError::CapacityExceeded { .. }) => {
                    let evicted = {
                        let mut state = self.lock_state();
                        let current_key = state.jobs.get(&job_id).map(|job| job.key.resource());
                        let candidate = state
                            .cache
                            .iter()
                            .filter(|(key, entry)| {
                                Some(**key) != current_key
                                    && entry.lease.ledger_category() == target
                                    && !considered.contains(*key)
                            })
                            .min_by_key(|(_, entry)| entry.last_touch)
                            .map(|(key, _)| *key);
                        candidate.and_then(|key| {
                            considered.push(key);
                            state.cache.remove(&key)
                        })
                    };
                    if evicted.is_none() {
                        return Err(error);
                    }
                    drop(evicted);
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl ProductionDatasetRuntime {
    fn submit_inner(&self, request: ResourceRequest) -> Result<RequestTicket, RuntimeFault> {
        let descriptor = self
            .shared
            .catalog
            .resource_payload_descriptor(request.resource())
            .map_err(|_| RuntimeFault::for_request(RuntimeFaultCode::SourceRejected, request))?;
        let destination = destination_category(request.priority());
        if descriptor.byte_len()
            > self
                .shared
                .config
                .category_cap(CpuLedgerCategory::InFlightDecode)
            || descriptor.byte_len() > self.shared.config.category_cap(destination)
        {
            return Err(RuntimeFault::for_request(
                RuntimeFaultCode::MinimumWorkUnitExceedsBudget,
                request,
            ));
        }

        let mut scope_charge = self
            .shared
            .reserve_scope_record(request.generation().scope())
            .map_err(|code| RuntimeFault::for_request(code, request))?;
        let request_charge = self
            .shared
            .ledger
            .acquire(CpuLedgerCategory::QueuesAndResults, REQUEST_RECORD_BYTES)
            .map_err(|error| RuntimeFault::for_request(map_ledger_error_code(error), request))?;
        let mut state = self.shared.lock_state();
        let id = state
            .allocate_request_id()
            .map_err(|code| RuntimeFault::for_request(code, request))?;
        let ticket = RequestTicket::for_request(id, request);
        if state.shutdown != ShutdownState::Running {
            return Err(RuntimeFault::for_ticket(
                RuntimeFaultCode::ShuttingDown,
                ticket,
            ));
        }
        if state.requests.len() >= self.shared.config.request_queue_limit()
            || state.requests.len() >= self.shared.config.completion_queue_limit()
        {
            return Err(RuntimeFault::for_ticket(
                RuntimeFaultCode::QueueFull,
                ticket,
            ));
        }
        match state
            .current_by_scope
            .get(&request.generation().scope())
            .map(|record| record.current)
        {
            Some(current) => {
                if !request
                    .generation()
                    .is_current(current)
                    .map_err(|code| RuntimeFault::for_ticket(code, ticket))?
                {
                    return Err(RuntimeFault::for_ticket(
                        RuntimeFaultCode::StaleGeneration,
                        ticket,
                    ));
                }
            }
            None => {
                if state.current_by_scope.len() >= self.shared.config.request_queue_limit() {
                    return Err(RuntimeFault::for_ticket(
                        RuntimeFaultCode::QueueFull,
                        ticket,
                    ));
                }
                let charge = scope_charge.take().ok_or_else(|| {
                    RuntimeFault::for_ticket(RuntimeFaultCode::InvariantViolation, ticket)
                })?;
                state.current_by_scope.insert(
                    request.generation().scope(),
                    ScopeRecord {
                        current: request.generation(),
                        _charge: charge,
                    },
                );
            }
        }
        if let Some(cached) = state.cache.get(&request.resource()) {
            let lease = cached.lease.clone();
            if request.priority() != RequestPriority::Prefetch
                && lease.ledger_category() == CpuLedgerCategory::Prefetch
                && let RuntimeCharge::Production(charge) = &lease.inner.charge
            {
                reclassify_cached_with_eviction(
                    &mut state,
                    request.resource(),
                    charge,
                    CpuLedgerCategory::DecodedResidency,
                )
                .map_err(|error| RuntimeFault::for_ticket(map_ledger_error_code(error), ticket))?;
            }
            let touch = state.touch();
            state
                .cache
                .get_mut(&request.resource())
                .expect("the cache entry remains present while locked")
                .last_touch = touch;
            let progress =
                RuntimeRequestProgress::new(ticket, descriptor.byte_len(), descriptor.byte_len())
                    .map_err(|code| RuntimeFault::for_ticket(code, ticket))?;
            state.requests.insert(
                id,
                RequestRecord {
                    request,
                    ticket,
                    progress,
                    job_id: None,
                    terminal: true,
                    _charge: request_charge,
                },
            );
            state.submitted_requests = state.submitted_requests.saturating_add(1);
            state.ready_requests = state.ready_requests.saturating_add(1);
            state.push_completion(
                RuntimeCompletion::new(ticket, RuntimeOutcome::Ready(lease)),
                self.shared.config.completion_queue_limit(),
            );
            return Ok(ticket);
        }

        let progress = RuntimeRequestProgress::new(ticket, 0, descriptor.byte_len())
            .map_err(|code| RuntimeFault::for_ticket(code, ticket))?;
        let dedupe_key = request.dedupe_key();
        let job_id = if let Some(job_id) = state.dedupe.get(&dedupe_key).copied() {
            let mut queue_update = None;
            {
                let job = state
                    .jobs
                    .get_mut(&job_id)
                    .expect("the dedupe index points to a live job");
                job.waiters.push(id);
                if request.priority().outranks(job.priority) {
                    job.priority = request.priority();
                    if job.phase == JobPhase::Queued {
                        job.queue_version = job.queue_version.saturating_add(1);
                        queue_update = Some(QueueEntry {
                            priority: job.priority,
                            sequence: job.queue_sequence,
                            job_id,
                            version: job.queue_version,
                        });
                    }
                }
            }
            if let Some(entry) = queue_update {
                state.replace_queued_entry(entry);
            }
            job_id
        } else {
            let job_id = state
                .allocate_job_id()
                .map_err(|code| RuntimeFault::for_ticket(code, ticket))?;
            let sequence = state
                .allocate_queue_sequence()
                .map_err(|code| RuntimeFault::for_ticket(code, ticket))?;
            let job = DecodeJob {
                key: dedupe_key,
                descriptor,
                waiters: vec![id],
                priority: request.priority(),
                phase: JobPhase::Queued,
                decode_started: false,
                queue_version: 0,
                queue_sequence: sequence,
                cancellation: Arc::new(AtomicBool::new(false)),
            };
            state.dedupe.insert(dedupe_key, job_id);
            state.jobs.insert(job_id, job);
            state.replace_queued_entry(QueueEntry {
                priority: request.priority(),
                sequence,
                job_id,
                version: 0,
            });
            job_id
        };
        state.requests.insert(
            id,
            RequestRecord {
                request,
                ticket,
                progress,
                job_id: Some(job_id),
                terminal: false,
                _charge: request_charge,
            },
        );
        state.submitted_requests = state.submitted_requests.saturating_add(1);
        drop(state);
        self.shared.work_available.notify_one();
        Ok(ticket)
    }
}

impl CpuByteLedger for ProductionDatasetRuntime {
    fn try_acquire(
        &self,
        category: CpuLedgerCategory,
        bytes: u64,
    ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
        Ok(Box::new(self.shared.ledger.acquire(category, bytes)?))
    }
}

impl DatasetRuntime for ProductionDatasetRuntime {
    fn submit(&self, request: ResourceRequest) -> Result<RequestTicket, RuntimeFault> {
        self.submit_inner(request)
    }

    fn cancel_before(&self, current: CancellationGeneration) -> Result<(), RuntimeFault> {
        let mut scope_charge = self
            .shared
            .reserve_scope_record(current.scope())
            .map_err(RuntimeFault::new)?;
        let mut state = self.shared.lock_state();
        if state.shutdown != ShutdownState::Running {
            return Err(RuntimeFault::new(RuntimeFaultCode::ShuttingDown));
        }
        if let Some(record) = state.current_by_scope.get_mut(&current.scope()) {
            if current
                .is_stale_for(record.current)
                .map_err(RuntimeFault::new)?
            {
                return Err(RuntimeFault::new(RuntimeFaultCode::StaleGeneration));
            }
            record.current = current;
        } else {
            if state.current_by_scope.len() >= self.shared.config.request_queue_limit() {
                return Err(RuntimeFault::new(RuntimeFaultCode::QueueFull));
            }
            let charge = scope_charge
                .take()
                .ok_or_else(|| RuntimeFault::new(RuntimeFaultCode::InvariantViolation))?;
            state.current_by_scope.insert(
                current.scope(),
                ScopeRecord {
                    current,
                    _charge: charge,
                },
            );
        }
        let ids = state
            .requests
            .iter()
            .filter_map(|(id, record)| {
                (!record.terminal
                    && record.request.generation().scope() == current.scope()
                    && record
                        .request
                        .generation()
                        .is_stale_for(current)
                        .expect("the scope was selected explicitly"))
                .then_some(*id)
            })
            .collect::<Vec<_>>();
        cancel_request_ids(
            &mut state,
            &ids,
            self.shared.config.completion_queue_limit(),
        );
        drop(state);
        self.shared.work_available.notify_all();
        Ok(())
    }

    fn poll(&self, max_completions: usize) -> Result<Vec<RuntimeCompletion>, RuntimeFault> {
        let mut state = self.shared.lock_state();
        let count = max_completions.min(state.completions.len());
        let completions = state.completions.drain(..count).collect::<Vec<_>>();
        let mut removed = Vec::with_capacity(completions.len());
        for completion in &completions {
            if let Some(record) = state.requests.remove(&completion.ticket().id()) {
                removed.push(record);
            }
        }
        if state.shutdown == ShutdownState::Draining
            && state.workers_joined
            && state.completions.is_empty()
        {
            state.shutdown = ShutdownState::Stopped;
        }
        drop(state);
        drop(removed);
        Ok(completions)
    }

    fn diagnostics(&self) -> Result<DatasetRuntimeDiagnostics, RuntimeFault> {
        let state = self.shared.lock_state();
        let queued_requests = state
            .jobs
            .values()
            .filter(|job| job.phase == JobPhase::Queued)
            .map(|job| job.waiters.len())
            .sum();
        let in_flight_decodes = state
            .jobs
            .values()
            .filter(|job| {
                job.decode_started
                    && matches!(
                        job.phase,
                        JobPhase::Claimed | JobPhase::InFlight | JobPhase::Aborting
                    )
            })
            .count();
        DatasetRuntimeDiagnostics::new(
            self.shared.config,
            self.shared.ledger.snapshot(),
            queued_requests,
            in_flight_decodes,
            state.completions.len(),
            state.cache.len(),
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
        let state = self.shared.lock_state();
        let Some(record) = state.requests.get(&ticket.id()) else {
            return Ok(None);
        };
        if record.ticket != ticket {
            return Err(RuntimeFault::for_ticket(
                RuntimeFaultCode::InvariantViolation,
                ticket,
            ));
        }
        Ok(Some(record.progress))
    }

    fn try_acquire_analysis_bytes(&self, bytes: u64) -> Result<AccountedCpuLease, RuntimeFault> {
        let charge = self
            .shared
            .ledger
            .acquire(CpuLedgerCategory::QueuesAndResults, bytes)
            .map_err(|error| RuntimeFault::new(map_ledger_error_code(error)))?;
        Ok(AccountedCpuLease {
            inner: Arc::new(AccountedCpuCharge {
                charge: RuntimeCharge::Production(charge),
            }),
        })
    }

    fn request_shutdown(&self) -> Result<(), RuntimeFault> {
        self.shared.begin_shutdown();
        Ok(())
    }

    fn shutdown_state(&self) -> ShutdownState {
        self.shared.lock_state().shutdown
    }
}

impl Drop for ProductionDatasetRuntime {
    fn drop(&mut self) {
        self.shared.begin_shutdown();
        let _ = self
            .supervisor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
    }
}

fn cancel_request_ids(state: &mut RuntimeState, ids: &[RuntimeRequestId], completion_limit: usize) {
    let mut affected_jobs = Vec::new();
    for id in ids {
        let Some(record) = state.requests.get_mut(id) else {
            continue;
        };
        if record.terminal {
            continue;
        }
        record.terminal = true;
        let ticket = record.ticket;
        if let Some(job_id) = record.job_id {
            affected_jobs.push(job_id);
            if let Some(job) = state.jobs.get_mut(&job_id) {
                job.waiters.retain(|waiter| waiter != id);
            }
        }
        state.cancelled_requests = state.cancelled_requests.saturating_add(1);
        state.push_completion(
            RuntimeCompletion::new(ticket, RuntimeOutcome::Cancelled),
            completion_limit,
        );
    }
    affected_jobs.sort_unstable();
    affected_jobs.dedup();
    for job_id in affected_jobs {
        let Some(job) = state.jobs.get(&job_id) else {
            continue;
        };
        if job.waiters.is_empty() {
            match job.phase {
                JobPhase::Queued => {
                    state.remove_job(job_id);
                }
                JobPhase::Claimed | JobPhase::InFlight | JobPhase::Aborting => {
                    let key = job.key;
                    let cancellation = Arc::clone(&job.cancellation);
                    if state.dedupe.get(&key).copied() == Some(job_id) {
                        state.dedupe.remove(&key);
                    }
                    let job = state
                        .jobs
                        .get_mut(&job_id)
                        .expect("the affected job remains present");
                    job.phase = JobPhase::Aborting;
                    cancellation.store(true, AtomicOrdering::Release);
                }
            }
            continue;
        }
        let priority = state.jobs[&job_id]
            .waiters
            .iter()
            .filter_map(|id| {
                state
                    .requests
                    .get(id)
                    .map(|record| record.request.priority())
            })
            .max()
            .expect("a nonempty waiter set has an effective priority");
        let queue_update = {
            let job = state
                .jobs
                .get_mut(&job_id)
                .expect("the affected job remains present");
            if priority == job.priority {
                None
            } else {
                job.priority = priority;
                if job.phase == JobPhase::Queued {
                    job.queue_version = job.queue_version.saturating_add(1);
                    Some(QueueEntry {
                        priority,
                        sequence: job.queue_sequence,
                        job_id,
                        version: job.queue_version,
                    })
                } else {
                    None
                }
            }
        };
        if let Some(entry) = queue_update {
            state.replace_queued_entry(entry);
        }
    }
}

fn worker_loop(shared: Arc<RuntimeShared>) {
    while let Some(claim) = shared.claim_job() {
        let charge = match shared.ledger.acquire(
            CpuLedgerCategory::InFlightDecode,
            claim.descriptor.byte_len(),
        ) {
            Ok(charge) => charge,
            Err(CpuLedgerError::CapacityExceeded { .. }) => {
                shared.requeue_claim(&claim);
                shared.ledger.wait_for_change();
                continue;
            }
            Err(error) => {
                shared.finish_failure(claim.job_id, map_ledger_error_code(error), false);
                continue;
            }
        };
        let byte_len = match usize::try_from(claim.descriptor.byte_len()) {
            Ok(byte_len) => byte_len,
            Err(_) => {
                shared.finish_failure(
                    claim.job_id,
                    RuntimeFaultCode::MinimumWorkUnitExceedsBudget,
                    false,
                );
                continue;
            }
        };
        let mut buffer = Vec::new();
        if buffer.try_reserve_exact(byte_len).is_err() {
            shared.finish_failure(
                claim.job_id,
                RuntimeFaultCode::CapacityExceeded {
                    category: CpuLedgerCategory::InFlightDecode,
                    requested_bytes: claim.descriptor.byte_len(),
                    available_bytes: 0,
                },
                false,
            );
            continue;
        }
        buffer.resize(byte_len, 0);
        if !shared.activate_claim(&claim) {
            continue;
        }
        let mut sink = RuntimeDecodeSink {
            shared: Arc::clone(&shared),
            job_id: claim.job_id,
            key: claim.key,
            descriptor: claim.descriptor,
            cancellation: claim.cancellation,
            buffer,
            written: 0,
            finished: false,
            charge: Some(charge),
        };
        let decode = catch_unwind(AssertUnwindSafe(|| {
            shared
                .source
                .decode_into(&mut sink)
                .map_err(|fault| map_source_fault_code(&fault))
        }));
        match decode {
            Err(_) => {
                shared.finish_failure(claim.job_id, RuntimeFaultCode::InvariantViolation, true)
            }
            Ok(Err(code)) => shared.finish_failure(claim.job_id, code, true),
            Ok(Ok(())) if !sink.finished => {
                shared.finish_failure(claim.job_id, RuntimeFaultCode::SinkRejected, true)
            }
            Ok(Ok(())) => {
                let (bytes, charge) = sink.into_parts();
                shared.finish_success(claim.job_id, bytes, charge);
            }
        }
    }
}

struct RuntimeDecodeSink {
    shared: Arc<RuntimeShared>,
    job_id: u64,
    key: DatasetResourceKey,
    descriptor: ResourcePayloadDescriptor,
    cancellation: Arc<AtomicBool>,
    buffer: Vec<u8>,
    written: usize,
    finished: bool,
    charge: Option<LedgerCharge>,
}

impl RuntimeDecodeSink {
    fn into_parts(mut self) -> (Box<[u8]>, LedgerCharge) {
        assert!(self.finished);
        (
            std::mem::take(&mut self.buffer).into_boxed_slice(),
            self.charge
                .take()
                .expect("a completed sink retains its in-flight charge"),
        )
    }
}

impl ReservedDecodeSink for RuntimeDecodeSink {
    fn resource_key(&self) -> DatasetResourceKey {
        self.key
    }

    fn payload_descriptor(&self) -> ResourcePayloadDescriptor {
        self.descriptor
    }

    fn written_bytes(&self) -> u64 {
        self.written as u64
    }

    fn is_cancelled(&self) -> bool {
        self.cancellation.load(AtomicOrdering::Acquire)
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), DecodeSinkError> {
        if self.is_cancelled() {
            return Err(DecodeSinkError::Cancelled);
        }
        if self.finished {
            return Err(DecodeSinkError::AlreadyFinished);
        }
        let end = self
            .written
            .checked_add(bytes.len())
            .ok_or(DecodeSinkError::ByteCountOverflow)?;
        if end > self.buffer.len() {
            return Err(DecodeSinkError::ReservationExceeded {
                reserved: self.descriptor.byte_len(),
                attempted: end as u64,
            });
        }
        self.buffer[self.written..end].copy_from_slice(bytes);
        self.written = end;
        self.shared
            .update_progress(self.job_id, self.written as u64, self.descriptor.byte_len());
        Ok(())
    }

    fn finish(&mut self) -> Result<(), DecodeSinkError> {
        if self.is_cancelled() {
            return Err(DecodeSinkError::Cancelled);
        }
        if self.finished {
            return Err(DecodeSinkError::AlreadyFinished);
        }
        if self.written != self.buffer.len() {
            return Err(DecodeSinkError::Incomplete {
                reserved: self.descriptor.byte_len(),
                written: self.written as u64,
            });
        }
        let value_len = usize::try_from(self.descriptor.value_byte_len())
            .map_err(|_| DecodeSinkError::ByteCountOverflow)?;
        let (values, validity) = self.buffer.split_at(value_len);
        let validity = (self.descriptor.validity_byte_len() != 0).then_some(validity);
        self.descriptor.view(values, validity).map_err(|_| {
            DecodeSinkError::ReservationExceeded {
                reserved: self.descriptor.byte_len(),
                attempted: self.descriptor.byte_len(),
            }
        })?;
        self.finished = true;
        Ok(())
    }

    fn is_finished(&self) -> bool {
        self.finished
    }
}

fn destination_category(priority: RequestPriority) -> CpuLedgerCategory {
    if priority == RequestPriority::Prefetch {
        CpuLedgerCategory::Prefetch
    } else {
        CpuLedgerCategory::DecodedResidency
    }
}

fn reclassify_cached_with_eviction(
    state: &mut RuntimeState,
    current_key: DatasetResourceKey,
    charge: &LedgerCharge,
    target: CpuLedgerCategory,
) -> Result<(), CpuLedgerError> {
    let mut considered: Vec<DatasetResourceKey> = Vec::new();
    loop {
        match charge.reclassify(target) {
            Ok(()) => return Ok(()),
            Err(error @ CpuLedgerError::CapacityExceeded { .. }) => {
                let candidate = state
                    .cache
                    .iter()
                    .filter(|(key, entry)| {
                        **key != current_key
                            && entry.lease.ledger_category() == target
                            && !considered.contains(*key)
                    })
                    .min_by_key(|(_, entry)| entry.last_touch)
                    .map(|(key, _)| *key);
                let Some(candidate) = candidate else {
                    return Err(error);
                };
                considered.push(candidate);
                drop(state.cache.remove(&candidate));
            }
            Err(error) => return Err(error),
        }
    }
}

fn map_ledger_error_code(error: CpuLedgerError) -> RuntimeFaultCode {
    match error {
        CpuLedgerError::ZeroByteReservation => RuntimeFaultCode::InvariantViolation,
        CpuLedgerError::CapacityExceeded {
            category,
            requested_bytes,
            available_bytes,
        } => RuntimeFaultCode::CapacityExceeded {
            category,
            requested_bytes,
            available_bytes,
        },
        CpuLedgerError::ShuttingDown => RuntimeFaultCode::ShuttingDown,
    }
}

fn map_source_fault_code(fault: &DatasetSourceFault) -> RuntimeFaultCode {
    match fault {
        DatasetSourceFault::CatalogUnavailable
        | DatasetSourceFault::InvalidResource { .. }
        | DatasetSourceFault::ResourceUnavailable { .. } => RuntimeFaultCode::SourceRejected,
        DatasetSourceFault::CorruptResource { .. } => RuntimeFaultCode::CorruptResource,
        DatasetSourceFault::UnsupportedResource { .. } => RuntimeFaultCode::UnsupportedResource,
        DatasetSourceFault::Cancelled { .. } => RuntimeFaultCode::Cancelled,
        DatasetSourceFault::CapacityExceeded {
            category,
            requested_bytes,
            available_bytes,
            ..
        } => RuntimeFaultCode::CapacityExceeded {
            category: *category,
            requested_bytes: *requested_bytes,
            available_bytes: *available_bytes,
        },
        DatasetSourceFault::ShuttingDown { .. } => RuntimeFaultCode::ShuttingDown,
        DatasetSourceFault::DecodeFailed { .. } => RuntimeFaultCode::DecodeFailed,
        DatasetSourceFault::SinkRejected { reason, .. } => match reason.as_ref() {
            DecodeSinkError::Cancelled => RuntimeFaultCode::Cancelled,
            _ => RuntimeFaultCode::SinkRejected,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Condvar, Mutex, atomic::AtomicUsize},
        time::{Duration, Instant},
    };

    use mirante4d_dataset::{
        DatasetLayer, DatasetResourceIdentity, DatasetSourceId, ResourceLease, ResourceRegion,
        ResourceValidity, ScientificIdentityStatus,
    };
    use mirante4d_domain::{
        GridToWorld, IntensityDType, LogicalLayerKey, ScaleLevel, Shape3D, Shape4D, TimeIndex,
    };

    use super::*;

    #[derive(Clone, Copy)]
    enum GatePoint {
        None,
        BeforeWrite,
        AfterFirstByte,
    }

    struct TestSource {
        catalog: Arc<DatasetCatalog>,
        gate_point: GatePoint,
        gate: (Mutex<GateState>, Condvar),
        decode_count: AtomicUsize,
        decode_order: Mutex<Vec<u64>>,
        corrupt: bool,
    }

    struct GateState {
        entered: usize,
        first_byte_written: usize,
        released: bool,
    }

    impl TestSource {
        fn new(validity: ResourceValidity, gate_point: GatePoint) -> Arc<Self> {
            let layer = DatasetLayer::new(
                LogicalLayerKey::new(0),
                "intensity",
                Shape4D::new(1, 1, 1, 65_536).unwrap(),
                IntensityDType::Uint8,
                GridToWorld::identity(),
                validity,
            )
            .unwrap();
            let catalog = Arc::new(
                DatasetCatalog::new(
                    "runtime-test",
                    ScientificIdentityStatus::Unverified(DatasetSourceId::new(41)),
                    vec![layer],
                )
                .unwrap(),
            );
            Arc::new(Self {
                catalog,
                gate_point,
                gate: (
                    Mutex::new(GateState {
                        entered: 0,
                        first_byte_written: 0,
                        released: matches!(gate_point, GatePoint::None),
                    }),
                    Condvar::new(),
                ),
                decode_count: AtomicUsize::new(0),
                decode_order: Mutex::new(Vec::new()),
                corrupt: false,
            })
        }

        fn corrupt() -> Arc<Self> {
            let mut source = Self::new(ResourceValidity::AllValid, GatePoint::None);
            Arc::get_mut(&mut source).unwrap().corrupt = true;
            source
        }

        fn release(&self) {
            let (lock, changed) = &self.gate;
            lock.lock().unwrap().released = true;
            changed.notify_all();
        }

        fn wait_entered(&self, expected: usize) {
            let deadline = Instant::now() + Duration::from_secs(2);
            let (lock, changed) = &self.gate;
            let mut state = lock.lock().unwrap();
            while state.entered < expected {
                assert!(Instant::now() < deadline, "decode did not enter in time");
                state = changed
                    .wait_timeout(state, Duration::from_millis(5))
                    .unwrap()
                    .0;
            }
        }

        fn wait_first_byte(&self) {
            let deadline = Instant::now() + Duration::from_secs(2);
            let (lock, changed) = &self.gate;
            let mut state = lock.lock().unwrap();
            while state.first_byte_written == 0 {
                assert!(
                    Instant::now() < deadline,
                    "partial write did not occur in time"
                );
                state = changed
                    .wait_timeout(state, Duration::from_millis(5))
                    .unwrap()
                    .0;
            }
        }

        #[allow(
            clippy::result_large_err,
            reason = "the frozen DatasetSource contract requires this exact typed fault"
        )]
        fn wait_gate(&self, sink: &dyn ReservedDecodeSink) -> Result<(), DatasetSourceFault> {
            let (lock, changed) = &self.gate;
            let mut state = lock.lock().unwrap();
            while !state.released {
                if sink.is_cancelled() {
                    return Err(DatasetSourceFault::Cancelled {
                        key: sink.resource_key(),
                    });
                }
                state = changed
                    .wait_timeout(state, Duration::from_millis(5))
                    .unwrap()
                    .0;
            }
            Ok(())
        }
    }

    impl DatasetSource for TestSource {
        fn catalog(&self) -> Result<Arc<DatasetCatalog>, DatasetSourceFault> {
            Ok(Arc::clone(&self.catalog))
        }

        fn decode_into(&self, sink: &mut dyn ReservedDecodeSink) -> Result<(), DatasetSourceFault> {
            self.decode_count.fetch_add(1, AtomicOrdering::SeqCst);
            self.decode_order
                .lock()
                .unwrap()
                .push(sink.resource_key().region().origin()[2]);
            {
                let (lock, changed) = &self.gate;
                lock.lock().unwrap().entered += 1;
                changed.notify_all();
            }
            if self.corrupt {
                return Err(DatasetSourceFault::CorruptResource {
                    key: sink.resource_key(),
                });
            }
            if matches!(self.gate_point, GatePoint::BeforeWrite) {
                self.wait_gate(sink)?;
            }

            let descriptor = sink.payload_descriptor();
            let value_len = usize::try_from(descriptor.value_byte_len()).unwrap();
            let origin = sink.resource_key().region().origin()[2] as u8;
            let values = (0..value_len)
                .map(|offset| origin.wrapping_add(offset as u8))
                .collect::<Vec<_>>();
            if matches!(self.gate_point, GatePoint::AfterFirstByte) {
                sink.write(&values[..1])
                    .map_err(|reason| DatasetSourceFault::SinkRejected {
                        key: sink.resource_key(),
                        reason: Box::new(reason),
                    })?;
                {
                    let (lock, changed) = &self.gate;
                    lock.lock().unwrap().first_byte_written += 1;
                    changed.notify_all();
                }
                self.wait_gate(sink)?;
                sink.write(&values[1..])
                    .map_err(|reason| DatasetSourceFault::SinkRejected {
                        key: sink.resource_key(),
                        reason: Box::new(reason),
                    })?;
            } else {
                sink.write(&values)
                    .map_err(|reason| DatasetSourceFault::SinkRejected {
                        key: sink.resource_key(),
                        reason: Box::new(reason),
                    })?;
            }
            if descriptor.validity_byte_len() != 0 {
                let sample_count = descriptor.sample_count();
                let mut mask = vec![0_u8; usize::try_from(descriptor.validity_byte_len()).unwrap()];
                for index in 0..sample_count {
                    if index % 2 == 0 {
                        mask[usize::try_from(index / 8).unwrap()] |= 1 << (index % 8);
                    }
                }
                sink.write(&mask)
                    .map_err(|reason| DatasetSourceFault::SinkRejected {
                        key: sink.resource_key(),
                        reason: Box::new(reason),
                    })?;
            }
            sink.finish()
                .map_err(|reason| DatasetSourceFault::SinkRejected {
                    key: sink.resource_key(),
                    reason: Box::new(reason),
                })
        }
    }

    fn key(origin_x: u64, samples: u64) -> DatasetResourceKey {
        DatasetResourceKey::new(
            DatasetResourceIdentity::Unverified(DatasetSourceId::new(41)),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, origin_x], Shape3D::new(1, 1, samples).unwrap()).unwrap(),
        )
    }

    fn request(
        resource: DatasetResourceKey,
        priority: RequestPriority,
        scope: u64,
        generation: u64,
    ) -> ResourceRequest {
        ResourceRequest::new(
            resource,
            priority,
            CancellationGeneration::for_scope(scope, generation),
        )
    }

    fn start(
        source: Arc<TestSource>,
        workers: usize,
        requests: usize,
        completions: usize,
    ) -> Arc<dyn DatasetRuntime> {
        let config = DatasetRuntimeConfig::new(1 << 20, workers, requests, completions).unwrap();
        start_with_config(source, config)
    }

    fn start_with_config(
        source: Arc<TestSource>,
        config: DatasetRuntimeConfig,
    ) -> Arc<dyn DatasetRuntime> {
        let source_for_factory = Arc::clone(&source);
        let (runtime, catalog) = <dyn DatasetRuntime>::start(config, move |_| {
            let source: Arc<dyn DatasetSource> = source_for_factory;
            Ok(source)
        })
        .unwrap();
        assert!(Arc::ptr_eq(&catalog, &source.catalog));
        runtime
    }

    fn wait_completions(
        runtime: &Arc<dyn DatasetRuntime>,
        expected: usize,
    ) -> Vec<RuntimeCompletion> {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut completions = Vec::new();
        while completions.len() < expected {
            completions.extend(runtime.poll(expected - completions.len()).unwrap());
            assert!(Instant::now() < deadline, "runtime completions timed out");
            if completions.len() < expected {
                thread::sleep(Duration::from_millis(1));
            }
        }
        completions
    }

    #[test]
    fn scheduler_queue_replaces_priority_versions_and_removes_cancelled_jobs() {
        let source = TestSource::new(ResourceValidity::AllValid, GatePoint::BeforeWrite);
        let resource = key(9, 4);
        let request = request(resource, RequestPriority::Prefetch, 1, 0);
        let descriptor = source
            .catalog
            .resource_payload_descriptor(resource)
            .unwrap();
        let mut state = RuntimeState::new();
        let job_id = state.allocate_job_id().unwrap();
        let sequence = state.allocate_queue_sequence().unwrap();
        state.jobs.insert(
            job_id,
            DecodeJob {
                key: request.dedupe_key(),
                descriptor,
                waiters: Vec::new(),
                priority: RequestPriority::Prefetch,
                phase: JobPhase::Queued,
                decode_started: false,
                queue_version: 0,
                queue_sequence: sequence,
                cancellation: Arc::new(AtomicBool::new(false)),
            },
        );
        state.replace_queued_entry(QueueEntry {
            priority: RequestPriority::Prefetch,
            sequence,
            job_id,
            version: 0,
        });

        for (version, priority) in [
            RequestPriority::Playback,
            RequestPriority::LinkedView,
            RequestPriority::CurrentView,
        ]
        .into_iter()
        .enumerate()
        {
            let version = version as u64 + 1;
            let job = state.jobs.get_mut(&job_id).unwrap();
            job.priority = priority;
            job.queue_version = version;
            state.replace_queued_entry(QueueEntry {
                priority,
                sequence,
                job_id,
                version,
            });
            assert_eq!(state.queue.len(), 1);
            assert_eq!(state.queue.peek().unwrap().version, version);
        }

        state.remove_job(job_id);
        assert!(state.queue.is_empty());
    }

    #[test]
    fn cancellation_scope_records_are_bounded_and_byte_accounted() {
        let source = TestSource::new(ResourceValidity::AllValid, GatePoint::None);
        let config = DatasetRuntimeConfig::new(1 << 20, 1, 2, 2).unwrap();
        let runtime = start_with_config(source, config);

        runtime
            .cancel_before(CancellationGeneration::for_scope(10, 1))
            .unwrap();
        assert_eq!(
            runtime
                .diagnostics()
                .unwrap()
                .category_used_bytes(CpuLedgerCategory::QueuesAndResults),
            SCOPE_RECORD_BYTES
        );

        runtime
            .cancel_before(CancellationGeneration::for_scope(10, 2))
            .unwrap();
        runtime
            .cancel_before(CancellationGeneration::for_scope(20, 4))
            .unwrap();
        assert_eq!(
            runtime
                .diagnostics()
                .unwrap()
                .category_used_bytes(CpuLedgerCategory::QueuesAndResults),
            2 * SCOPE_RECORD_BYTES
        );

        assert_eq!(
            runtime
                .cancel_before(CancellationGeneration::for_scope(30, 1))
                .unwrap_err()
                .code(),
            RuntimeFaultCode::QueueFull
        );
        assert_eq!(
            runtime
                .submit(request(key(0, 2), RequestPriority::CurrentView, 30, 1))
                .unwrap_err()
                .code(),
            RuntimeFaultCode::QueueFull
        );
        assert_eq!(
            runtime
                .cancel_before(CancellationGeneration::for_scope(10, 1))
                .unwrap_err()
                .code(),
            RuntimeFaultCode::StaleGeneration
        );
        assert_eq!(
            runtime
                .diagnostics()
                .unwrap()
                .category_used_bytes(CpuLedgerCategory::QueuesAndResults),
            2 * SCOPE_RECORD_BYTES
        );

        let capacity_limited = start_with_config(
            TestSource::new(ResourceValidity::AllValid, GatePoint::None),
            DatasetRuntimeConfig::new(2_000, 1, 2, 2).unwrap(),
        );
        assert!(matches!(
            capacity_limited
                .cancel_before(CancellationGeneration::for_scope(1, 0))
                .unwrap_err()
                .code(),
            RuntimeFaultCode::CapacityExceeded {
                category: CpuLedgerCategory::QueuesAndResults,
                requested_bytes: SCOPE_RECORD_BYTES,
                available_bytes: 100,
            }
        ));
    }

    #[test]
    fn production_runtime_deduplicates_waiters_fans_out_one_lease_and_hits_cache() {
        let source = TestSource::new(ResourceValidity::AllValid, GatePoint::BeforeWrite);
        let runtime = start(Arc::clone(&source), 2, 16, 16);
        let resource = key(7, 4);
        let first = runtime
            .submit(request(resource, RequestPriority::CurrentView, 1, 0))
            .unwrap();
        source.wait_entered(1);
        let second = runtime
            .submit(request(resource, RequestPriority::Playback, 2, 0))
            .unwrap();
        assert_ne!(first.id(), second.id());
        source.release();
        let completions = wait_completions(&runtime, 2);
        let leases = completions
            .iter()
            .map(|completion| match completion.outcome() {
                RuntimeOutcome::Ready(lease) => lease.clone(),
                _ => panic!("deduplicated requests must become ready"),
            })
            .collect::<Vec<_>>();
        assert!(leases[0].shares_allocation_with(&leases[1]));
        assert_eq!(source.decode_count.load(AtomicOrdering::SeqCst), 1);

        runtime
            .submit(request(resource, RequestPriority::CurrentView, 3, 0))
            .unwrap();
        let cached = wait_completions(&runtime, 1);
        let RuntimeOutcome::Ready(cached) = cached[0].outcome() else {
            panic!("cache hit must be ready");
        };
        assert!(cached.shares_allocation_with(&leases[0]));
        assert_eq!(source.decode_count.load(AtomicOrdering::SeqCst), 1);
    }

    #[test]
    fn production_runtime_scoped_cancellation_preserves_other_waiter_on_shared_decode() {
        let source = TestSource::new(ResourceValidity::AllValid, GatePoint::BeforeWrite);
        let runtime = start(Arc::clone(&source), 1, 8, 8);
        let resource = key(11, 4);
        let cancelled = runtime
            .submit(request(resource, RequestPriority::CurrentView, 10, 1))
            .unwrap();
        source.wait_entered(1);
        let retained = runtime
            .submit(request(resource, RequestPriority::LinkedView, 20, 4))
            .unwrap();
        runtime
            .cancel_before(CancellationGeneration::for_scope(10, 2))
            .unwrap();
        source.release();
        let completions = wait_completions(&runtime, 2);
        assert!(completions.iter().any(|completion| {
            completion.ticket() == cancelled
                && matches!(completion.outcome(), RuntimeOutcome::Cancelled)
        }));
        assert!(completions.iter().any(|completion| {
            completion.ticket() == retained
                && matches!(completion.outcome(), RuntimeOutcome::Ready(_))
        }));
        assert_eq!(source.decode_count.load(AtomicOrdering::SeqCst), 1);
    }

    #[test]
    fn production_runtime_priority_upgrade_and_fifo_drive_one_worker() {
        let source = TestSource::new(ResourceValidity::AllValid, GatePoint::BeforeWrite);
        let runtime = start(Arc::clone(&source), 1, 8, 8);
        runtime
            .submit(request(key(0, 2), RequestPriority::Prefetch, 1, 0))
            .unwrap();
        source.wait_entered(1);
        runtime
            .submit(request(key(10, 2), RequestPriority::Prefetch, 2, 0))
            .unwrap();
        runtime
            .submit(request(key(20, 2), RequestPriority::Playback, 3, 0))
            .unwrap();
        runtime
            .submit(request(key(10, 2), RequestPriority::CurrentView, 4, 0))
            .unwrap();
        runtime
            .submit(request(key(30, 2), RequestPriority::LinkedView, 5, 0))
            .unwrap();
        source.release();
        let _ = wait_completions(&runtime, 5);
        assert_eq!(*source.decode_order.lock().unwrap(), vec![0, 10, 30, 20]);
    }

    #[test]
    fn production_runtime_bounds_admission_and_stalled_completion_delivery() {
        let source = TestSource::new(ResourceValidity::AllValid, GatePoint::BeforeWrite);
        let runtime = start(Arc::clone(&source), 1, 2, 2);
        runtime
            .submit(request(key(0, 2), RequestPriority::CurrentView, 1, 0))
            .unwrap();
        source.wait_entered(1);
        runtime
            .submit(request(key(4, 2), RequestPriority::CurrentView, 2, 0))
            .unwrap();
        assert_eq!(
            runtime
                .submit(request(key(8, 2), RequestPriority::CurrentView, 3, 0))
                .unwrap_err()
                .code(),
            RuntimeFaultCode::QueueFull
        );
        source.release();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let diagnostics = runtime.diagnostics().unwrap();
            assert!(diagnostics.pending_completions() <= 2);
            assert!(diagnostics.queued_requests() <= 2);
            if diagnostics.pending_completions() == 2 {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(runtime.poll(99).unwrap().len(), 2);
    }

    #[test]
    fn production_runtime_reports_partial_progress_and_preserves_packed_validity() {
        let source = TestSource::new(ResourceValidity::BitMask, GatePoint::AfterFirstByte);
        let runtime = start(Arc::clone(&source), 1, 4, 4);
        let ticket = runtime
            .submit(request(key(40, 4), RequestPriority::CurrentView, 1, 0))
            .unwrap();
        source.wait_first_byte();
        let progress = runtime.progress(ticket).unwrap().unwrap();
        assert_eq!(progress.written_bytes(), 1);
        assert_eq!(progress.reserved_bytes(), 5);
        source.release();
        let completion = wait_completions(&runtime, 1).pop().unwrap();
        let RuntimeOutcome::Ready(lease) = completion.outcome() else {
            panic!("valid payload must become ready");
        };
        assert_eq!(lease.payload().value_bytes(), &[40, 41, 42, 43]);
        assert_eq!(lease.payload().validity_bits(), Some(&[0b0000_0101][..]));
        assert_eq!(lease.payload().sample_is_valid(0), Ok(true));
        assert_eq!(lease.payload().sample_is_valid(1), Ok(false));
        assert_eq!(runtime.progress(ticket).unwrap(), None);
    }

    #[test]
    fn production_runtime_prefetch_hit_evicts_lru_before_zero_copy_promotion() {
        let source = TestSource::new(ResourceValidity::AllValid, GatePoint::None);
        let config = DatasetRuntimeConfig::new(40_000, 1, 16, 16).unwrap();
        let runtime = start_with_config(Arc::clone(&source), config);

        for origin in [0, 5_000, 10_000, 15_000] {
            runtime
                .submit(request(
                    key(origin, 5_000),
                    RequestPriority::CurrentView,
                    1,
                    0,
                ))
                .unwrap();
            let _ = wait_completions(&runtime, 1);
        }
        assert_eq!(
            runtime
                .diagnostics()
                .unwrap()
                .category_used_bytes(CpuLedgerCategory::DecodedResidency),
            20_000
        );

        let prefetched_key = key(25_000, 1_000);
        runtime
            .submit(request(prefetched_key, RequestPriority::Prefetch, 1, 0))
            .unwrap();
        let _ = wait_completions(&runtime, 1);
        assert_eq!(source.decode_count.load(AtomicOrdering::SeqCst), 5);

        runtime
            .submit(request(prefetched_key, RequestPriority::CurrentView, 1, 0))
            .unwrap();
        let completion = wait_completions(&runtime, 1).pop().unwrap();
        let RuntimeOutcome::Ready(lease) = completion.outcome() else {
            panic!("promoted prefetch must be ready");
        };
        assert_eq!(lease.ledger_category(), CpuLedgerCategory::DecodedResidency);
        assert_eq!(source.decode_count.load(AtomicOrdering::SeqCst), 5);
        let diagnostics = runtime.diagnostics().unwrap();
        assert!(
            diagnostics.category_used_bytes(CpuLedgerCategory::DecodedResidency)
                <= diagnostics.category_cap_bytes(CpuLedgerCategory::DecodedResidency)
        );
        assert_eq!(
            diagnostics.category_used_bytes(CpuLedgerCategory::Prefetch),
            0
        );
    }

    #[test]
    fn production_runtime_maps_source_fault_and_shutdown_is_cancellable_and_nonjoining() {
        let corrupt = TestSource::corrupt();
        let runtime = start(Arc::clone(&corrupt), 1, 4, 4);
        runtime
            .submit(request(key(1, 2), RequestPriority::CurrentView, 1, 0))
            .unwrap();
        let completion = wait_completions(&runtime, 1).pop().unwrap();
        let RuntimeOutcome::Failed(fault) = completion.outcome() else {
            panic!("corrupt source must fail");
        };
        assert_eq!(fault.code(), RuntimeFaultCode::CorruptResource);

        let blocked = TestSource::new(ResourceValidity::AllValid, GatePoint::BeforeWrite);
        let runtime = start(Arc::clone(&blocked), 1, 4, 4);
        runtime
            .submit(request(key(2, 2), RequestPriority::CurrentView, 1, 0))
            .unwrap();
        blocked.wait_entered(1);
        let started = Instant::now();
        runtime.request_shutdown().unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
        let completion = wait_completions(&runtime, 1).pop().unwrap();
        assert!(matches!(completion.outcome(), RuntimeOutcome::Cancelled));
        let deadline = Instant::now() + Duration::from_secs(2);
        while runtime.shutdown_state() != ShutdownState::Stopped {
            assert!(
                Instant::now() < deadline,
                "workers did not stop after cancellation"
            );
            let _ = runtime.poll(4).unwrap();
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            runtime
                .submit(request(key(3, 2), RequestPriority::CurrentView, 1, 0))
                .unwrap_err()
                .code(),
            RuntimeFaultCode::ShuttingDown
        );
    }

    #[test]
    fn production_runtime_deterministic_pressure_never_exceeds_hard_ledgers() {
        let source = TestSource::new(ResourceValidity::AllValid, GatePoint::None);
        let runtime = start(Arc::clone(&source), 4, 32, 32);
        for batch in 0..20_u64 {
            for offset in 0..16_u64 {
                runtime
                    .submit(request(
                        key((batch * 16 + offset) % 128, 1),
                        if offset % 5 == 0 {
                            RequestPriority::Prefetch
                        } else {
                            RequestPriority::CurrentView
                        },
                        offset + 1,
                        0,
                    ))
                    .unwrap();
            }
            let _ = wait_completions(&runtime, 16);
            let diagnostics = runtime.diagnostics().unwrap();
            assert!(diagnostics.total_used_bytes() <= diagnostics.total_cap_bytes());
            for category in super::super::CPU_LEDGER_CATEGORIES {
                assert!(
                    diagnostics.category_used_bytes(category)
                        <= diagnostics.category_cap_bytes(category)
                );
            }
            assert!(diagnostics.pending_completions() <= 32);
            assert!(diagnostics.in_flight_decodes() <= 4);
        }
    }
}

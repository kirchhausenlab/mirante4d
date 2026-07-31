use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use mirante4d_dataset::{
    BrickKey, CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError, DatasetCatalog,
    DatasetSource, DatasetSourceFault, DecodeSinkError, ReservedDecodeSink,
    ResourcePayloadDescriptor, ResourcePayloadFacts,
};

use crate::{
    AccountedCpuCharge, AccountedCpuLease, AccountedPayload, AccountedResourceLease,
    CancellationGeneration, DatasetRuntime, DatasetRuntimeConfig, DatasetRuntimeDiagnostics,
    DatasetRuntimePerformanceCounters, RequestDedupeKey, RequestPriority, RequestTicket,
    ResourceRequest, RuntimeCharge, RuntimeCompletion, RuntimeFault, RuntimeFaultCode,
    RuntimeOutcome, RuntimeRequestId, RuntimeRequestProgress, ShutdownState,
    ledger::{ChangeSignal, LedgerCharge, LedgerCore, LedgerHandle},
};

// These are explicit conservative charges for bounded scheduler metadata.
// Payload bytes are charged separately and exactly.
const REQUEST_RECORD_BYTES: u64 = 512;
const CACHE_RECORD_BYTES: u64 = 192;
const SCOPE_RECORD_BYTES: u64 = 128;
// Sources may stream decoded payloads in small chunks. Publishing every chunk
// through the runtime mutex made a 1 MiB brick take roughly 128 contended
// progress updates even though the product does not consume byte-granular
// progress. Preserve an immediate first update and exact completion while
// coalescing the middle of large writes.
const PROGRESS_UPDATE_GRANULARITY_BYTES: usize = 1024 * 1024;

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
    work_available: Arc<ChangeSignal>,
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
    queued_size_counts: BTreeMap<u64, usize>,
    queued_priority_counts: [usize; 6],
    completions: VecDeque<RuntimeCompletion>,
    cache: BTreeMap<BrickKey, CacheEntry>,
    decoded_cache_lru: BTreeSet<(u64, BrickKey)>,
    prefetch_cache_lru: BTreeSet<(u64, BrickKey)>,
    submitted_requests: u64,
    started_decodes: u64,
    completed_decodes: u64,
    ready_requests: u64,
    cancelled_requests: u64,
    failed_requests: u64,
    cache_hits: u64,
    cache_misses: u64,
    cache_evictions: u64,
    progress_updates: u64,
    queue_wait_ns: u64,
    decode_time_ns: u64,
    decoded_output_bytes: u64,
    cancelled_decode_executions: u64,
    cancelled_decode_time_ns: u64,
    cancelled_decode_bytes: u64,
    active_decode_cohorts: usize,
    workers_with_priority_claims: [usize; 6],
    decode_cohorts: u64,
    decode_cohort_members: u64,
    peak_decode_cohort_members: usize,
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
    admission_bytes: u64,
    waiters: Vec<RuntimeRequestId>,
    priority: RequestPriority,
    phase: JobPhase,
    decode_started: bool,
    queue_version: u64,
    queue_sequence: u64,
    cancellation: Arc<AtomicBool>,
    queued_at: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobPhase {
    Queued,
    Claimed,
    InFlight,
    // The source observes its existing cancellation checkpoint, while this
    // phase keeps cooperative yield distinct from terminal waiter retirement.
    Yielding,
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
    key: BrickKey,
    descriptor: ResourcePayloadDescriptor,
    admission_bytes: u64,
    priority: RequestPriority,
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
            queued_size_counts: BTreeMap::new(),
            queued_priority_counts: [0; 6],
            completions: VecDeque::new(),
            cache: BTreeMap::new(),
            decoded_cache_lru: BTreeSet::new(),
            prefetch_cache_lru: BTreeSet::new(),
            submitted_requests: 0,
            started_decodes: 0,
            completed_decodes: 0,
            ready_requests: 0,
            cancelled_requests: 0,
            failed_requests: 0,
            cache_hits: 0,
            cache_misses: 0,
            cache_evictions: 0,
            progress_updates: 0,
            queue_wait_ns: 0,
            decode_time_ns: 0,
            decoded_output_bytes: 0,
            cancelled_decode_executions: 0,
            cancelled_decode_time_ns: 0,
            cancelled_decode_bytes: 0,
            active_decode_cohorts: 0,
            workers_with_priority_claims: [0; 6],
            decode_cohorts: 0,
            decode_cohort_members: 0,
            peak_decode_cohort_members: 0,
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
        if job.phase == JobPhase::Queued {
            self.remove_queued_job(job.admission_bytes, job.priority);
        }
        if self.dedupe.get(&job.key).copied() == Some(job_id) {
            self.dedupe.remove(&job.key);
        }
        Some(job)
    }

    fn add_queued_job(&mut self, bytes: u64, priority: RequestPriority) {
        let count = self.queued_size_counts.entry(bytes).or_default();
        *count = count.saturating_add(1);
        let priority_count = &mut self.queued_priority_counts[usize::from(priority.rank())];
        *priority_count = priority_count.saturating_add(1);
    }

    fn remove_queued_job(&mut self, bytes: u64, priority: RequestPriority) {
        let remove = {
            let count = self
                .queued_size_counts
                .get_mut(&bytes)
                .expect("each queued job contributes one size-index entry");
            *count = count
                .checked_sub(1)
                .expect("a queued size count is removed exactly once");
            *count == 0
        };
        if remove {
            self.queued_size_counts.remove(&bytes);
        }
        let priority_count = &mut self.queued_priority_counts[usize::from(priority.rank())];
        *priority_count = priority_count
            .checked_sub(1)
            .expect("each queued job contributes one priority-count entry");
    }

    fn change_queued_priority(&mut self, previous: RequestPriority, next: RequestPriority) {
        if previous == next {
            return;
        }
        let previous_count = &mut self.queued_priority_counts[usize::from(previous.rank())];
        *previous_count = previous_count
            .checked_sub(1)
            .expect("a queued priority change removes one previous entry");
        let next_count = &mut self.queued_priority_counts[usize::from(next.rank())];
        *next_count = next_count.saturating_add(1);
    }

    fn minimum_queued_bytes(&self) -> Option<u64> {
        self.queued_size_counts
            .first_key_value()
            .map(|(bytes, _)| *bytes)
    }

    fn request_foreground_preemption(
        &mut self,
        incoming: RequestPriority,
        worker_limit: usize,
    ) -> bool {
        if !matches!(
            incoming,
            RequestPriority::CurrentView | RequestPriority::LinkedView
        ) || self.active_decode_cohorts < worker_limit
        {
            return false;
        }

        let mut requested = false;
        for job in self.jobs.values_mut() {
            if job.phase == JobPhase::InFlight
                && matches!(
                    job.priority,
                    RequestPriority::Playback
                        | RequestPriority::Analysis
                        | RequestPriority::Prefetch
                )
            {
                job.phase = JobPhase::Yielding;
                job.cancellation.store(true, AtomicOrdering::Release);
                requested = true;
            }
        }
        requested
    }

    fn cache_lru(&self, category: CpuLedgerCategory) -> &BTreeSet<(u64, BrickKey)> {
        if category == CpuLedgerCategory::Prefetch {
            &self.prefetch_cache_lru
        } else {
            debug_assert_eq!(category, CpuLedgerCategory::DecodedResidency);
            &self.decoded_cache_lru
        }
    }

    fn cache_lru_mut(&mut self, category: CpuLedgerCategory) -> &mut BTreeSet<(u64, BrickKey)> {
        if category == CpuLedgerCategory::Prefetch {
            &mut self.prefetch_cache_lru
        } else {
            debug_assert_eq!(category, CpuLedgerCategory::DecodedResidency);
            &mut self.decoded_cache_lru
        }
    }

    fn insert_cache_entry(&mut self, key: BrickKey, entry: CacheEntry) {
        if let Some(replaced) = self.remove_cache_entry(&key) {
            drop(replaced);
        }
        let category = entry.lease.ledger_category();
        self.cache_lru_mut(category).insert((entry.last_touch, key));
        self.cache.insert(key, entry);
    }

    fn remove_cache_entry(&mut self, key: &BrickKey) -> Option<CacheEntry> {
        let entry = self.cache.remove(key)?;
        let category = entry.lease.ledger_category();
        let removed = self
            .cache_lru_mut(category)
            .remove(&(entry.last_touch, *key));
        debug_assert!(removed, "a cached lease has one LRU index entry");
        Some(entry)
    }

    fn touch_cache_entry(&mut self, key: BrickKey) {
        let (category, old_touch) = {
            let entry = self
                .cache
                .get(&key)
                .expect("the touched cache entry remains present");
            (entry.lease.ledger_category(), entry.last_touch)
        };
        let removed = self.cache_lru_mut(category).remove(&(old_touch, key));
        debug_assert!(removed, "a touched lease has one old LRU entry");
        let touch = self.touch();
        self.cache
            .get_mut(&key)
            .expect("the touched cache entry remains present")
            .last_touch = touch;
        self.cache_lru_mut(category).insert((touch, key));
    }

    fn reclassify_cache_entry_index(
        &mut self,
        key: BrickKey,
        old: CpuLedgerCategory,
        new: CpuLedgerCategory,
    ) {
        if old == new {
            return;
        }
        let touch = self
            .cache
            .get(&key)
            .expect("the promoted cache entry remains present")
            .last_touch;
        let removed = self.cache_lru_mut(old).remove(&(touch, key));
        debug_assert!(removed, "promotion removes the old cache LRU class");
        self.cache_lru_mut(new).insert((touch, key));
    }

    fn oldest_cache_key(
        &self,
        category: CpuLedgerCategory,
        excluded: Option<BrickKey>,
    ) -> Option<BrickKey> {
        self.cache_lru(category)
            .iter()
            .map(|(_, key)| *key)
            .find(|key| Some(*key) != excluded)
    }

    /// Adds a versioned heap entry in logarithmic time. Older versions are
    /// rejected by `claim_job`; compacting only at a hard multiple of the
    /// admitted-request bound avoids the predecessor's O(n) heap scan on
    /// every one of tens of thousands of submissions.
    fn enqueue_version(&mut self, entry: QueueEntry, request_limit: usize) {
        let compact_at = request_limit.saturating_mul(2).max(1);
        if self.queue.len() >= compact_at {
            let jobs = &self.jobs;
            self.queue.retain(|queued| {
                jobs.get(&queued.job_id).is_some_and(|job| {
                    job.phase == JobPhase::Queued
                        && job.queue_version == queued.version
                        && job.priority == queued.priority
                })
            });
        }
        self.queue.push(entry);
        assert!(
            self.queue.len() <= compact_at,
            "versioned scheduler entries remain within twice admission"
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
        let work_available = Arc::new(ChangeSignal::default());
        let ledger = LedgerCore::new(config, Arc::clone(&work_available));
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
            work_available,
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
        cancel_request_ids(
            &mut state,
            ids,
            self.config.completion_queue_limit(),
            self.config.request_queue_limit(),
        );
        for job in state.jobs.values_mut() {
            if matches!(
                job.phase,
                JobPhase::Claimed | JobPhase::InFlight | JobPhase::Yielding | JobPhase::Aborting
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
        loop {
            // Queue mutations and ledger releases advance this predicate before
            // notifying. Sampling before either authority prevents a change in
            // the former check/wait window from being lost.
            let observed_change = self.work_available.generation();
            let available = self.ledger.available(CpuLedgerCategory::InFlightDecode);
            let mut state = self.lock_state();
            if state
                .minimum_queued_bytes()
                .is_some_and(|minimum| minimum > available)
            {
                if state.shutdown != ShutdownState::Running {
                    return None;
                }
                // No queued work can fit the current exact ledger headroom.
                // Avoid repeatedly draining and rebuilding the entire priority
                // heap while other workers hold decode reservations.
                drop(state);
                self.work_available.wait_for_change_after(observed_change);
                continue;
            }
            let mut capacity_blocked = Vec::new();
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
                if job.admission_bytes > available {
                    capacity_blocked.push(entry);
                    continue;
                }
                job.phase = JobPhase::Claimed;
                let claimed_bytes = job.admission_bytes;
                let claim = JobClaim {
                    job_id: entry.job_id,
                    key: job.key.resource(),
                    descriptor: job.descriptor,
                    admission_bytes: job.admission_bytes,
                    priority: job.priority,
                    cancellation: Arc::clone(&job.cancellation),
                };
                state.remove_queued_job(claimed_bytes, claim.priority);
                state.queue.extend(capacity_blocked);
                return Some(claim);
            }
            state.queue.extend(capacity_blocked);
            if state.shutdown != ShutdownState::Running {
                return None;
            }
            // An empty queue waits for work; a capacity-blocked queue waits for
            // either released bytes or newly queued smaller work. Both are one
            // predicate-driven notification path with no recovery polling.
            drop(state);
            self.work_available.wait_for_change_after(observed_change);
        }
    }

    /// Claims one immediately executable peer for an already admitted
    /// cohort. This never waits, never crosses the first member's priority,
    /// and preserves the normal lazy-version/no-head-of-line selector within
    /// that class. The caller acquires the exact byte charge before asking for
    /// another member.
    fn try_claim_cohort_peer(
        &self,
        priority: RequestPriority,
        available: u64,
        current_members: usize,
    ) -> Option<JobClaim> {
        let mut state = self.lock_state();
        if state.shutdown != ShutdownState::Running {
            return None;
        }
        let priority_index = usize::from(priority.rank());
        let future_workers = self
            .config
            .worker_limit()
            .saturating_sub(state.workers_with_priority_claims[priority_index]);
        let participating_workers = future_workers.saturating_add(1);
        let remaining_members =
            state.queued_priority_counts[priority_index].saturating_add(current_members);
        let fair_member_limit = remaining_members.div_ceil(participating_workers);
        if current_members >= fair_member_limit {
            return None;
        }
        let mut capacity_blocked = Vec::new();
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
            if job.priority != priority {
                capacity_blocked.push(entry);
                break;
            }
            if job.admission_bytes > available {
                capacity_blocked.push(entry);
                continue;
            }
            job.phase = JobPhase::Claimed;
            let claimed_bytes = job.admission_bytes;
            let claim = JobClaim {
                job_id: entry.job_id,
                key: job.key.resource(),
                descriptor: job.descriptor,
                admission_bytes: job.admission_bytes,
                priority: job.priority,
                cancellation: Arc::clone(&job.cancellation),
            };
            state.remove_queued_job(claimed_bytes, claim.priority);
            state.queue.extend(capacity_blocked);
            return Some(claim);
        }
        state.queue.extend(capacity_blocked);
        None
    }

    fn requeue_claim(&self, claim: &JobClaim) -> Result<(), RuntimeFaultCode> {
        let mut state = self.lock_state();
        let running = state.shutdown == ShutdownState::Running;
        // Capacity-blocked work moves to the back of its priority class. The
        // predecessor retained its original FIFO sequence, so one large job
        // that could not fit was immediately reclaimed while smaller ready
        // jobs starved behind it.
        let sequence = state.allocate_queue_sequence()?;
        let (queued_bytes, priority, entry) = {
            let Some(job) = state.jobs.get_mut(&claim.job_id) else {
                return Ok(());
            };
            if job.waiters.is_empty() || !running {
                job.phase = JobPhase::Aborting;
                job.cancellation.store(true, AtomicOrdering::Release);
                return Ok(());
            }
            job.phase = JobPhase::Queued;
            job.queue_version = job.queue_version.saturating_add(1);
            job.queue_sequence = sequence;
            let priority = job.priority;
            (
                job.admission_bytes,
                priority,
                QueueEntry {
                    priority,
                    sequence,
                    job_id: claim.job_id,
                    version: job.queue_version,
                },
            )
        };
        state.add_queued_job(queued_bytes, priority);
        state.enqueue_version(entry, self.config.request_queue_limit());
        self.work_available.notify_one();
        Ok(())
    }

    fn retry_in_flight_capacity(&self, job_id: u64) -> Result<(), RuntimeFaultCode> {
        let mut state = self.lock_state();
        if state.shutdown != ShutdownState::Running {
            return Err(RuntimeFaultCode::ShuttingDown);
        }
        let sequence = state.allocate_queue_sequence()?;
        let (admission_bytes, priority, entry) = {
            let Some(job) = state.jobs.get_mut(&job_id) else {
                return Err(RuntimeFaultCode::Cancelled);
            };
            if job.waiters.is_empty() || job.phase == JobPhase::Aborting {
                return Err(RuntimeFaultCode::Cancelled);
            }
            job.phase = JobPhase::Queued;
            job.queue_version = job.queue_version.saturating_add(1);
            job.queue_sequence = sequence;
            (
                job.admission_bytes,
                job.priority,
                QueueEntry {
                    priority: job.priority,
                    sequence,
                    job_id,
                    version: job.queue_version,
                },
            )
        };
        state.add_queued_job(admission_bytes, priority);
        state.enqueue_version(entry, self.config.request_queue_limit());
        self.work_available.notify_one();
        Ok(())
    }

    fn retry_preempted(&self, job_id: u64) -> Result<bool, RuntimeFaultCode> {
        let mut state = self.lock_state();
        if state.shutdown != ShutdownState::Running {
            return Ok(false);
        }
        if !state
            .jobs
            .get(&job_id)
            .is_some_and(|job| job.phase == JobPhase::Yielding && !job.waiters.is_empty())
        {
            return Ok(false);
        }
        let sequence = state.allocate_queue_sequence()?;
        let (admission_bytes, priority, entry, waiters, reserved_bytes) = {
            let job = state
                .jobs
                .get_mut(&job_id)
                .expect("the prevalidated yielded job remains installed");
            job.cancellation.store(false, AtomicOrdering::Release);
            job.phase = JobPhase::Queued;
            job.queue_version = job.queue_version.saturating_add(1);
            job.queue_sequence = sequence;
            (
                job.admission_bytes,
                job.priority,
                QueueEntry {
                    priority: job.priority,
                    sequence,
                    job_id,
                    version: job.queue_version,
                },
                job.waiters.clone(),
                job.descriptor.byte_len(),
            )
        };
        for id in waiters {
            if let Some(record) = state.requests.get_mut(&id)
                && !record.terminal
            {
                record.progress = RuntimeRequestProgress::new(record.ticket, 0, reserved_bytes)
                    .expect("a yielded decode restarts within its original reservation");
            }
        }
        state.add_queued_job(admission_bytes, priority);
        state.enqueue_version(entry, self.config.request_queue_limit());
        self.work_available.notify_one();
        Ok(true)
    }

    fn activate_claim(&self, claim: &JobClaim) -> bool {
        let mut state = self.lock_state();
        let (first_start, queue_wait_ns) = {
            let Some(job) = state.jobs.get_mut(&claim.job_id) else {
                return false;
            };
            if job.waiters.is_empty() || job.phase == JobPhase::Aborting {
                state.remove_job(claim.job_id);
                return false;
            }
            job.phase = JobPhase::InFlight;
            let first_start = !job.decode_started;
            job.decode_started = true;
            (first_start, duration_ns(job.queued_at.elapsed()))
        };
        if first_start {
            state.started_decodes = state.started_decodes.saturating_add(1);
        }
        state.queue_wait_ns = state.queue_wait_ns.saturating_add(queue_wait_ns);
        true
    }

    fn record_decode_observation(&self, elapsed: Duration, written_bytes: u64) {
        let mut state = self.lock_state();
        state.decode_time_ns = state.decode_time_ns.saturating_add(duration_ns(elapsed));
        state.decoded_output_bytes = state.decoded_output_bytes.saturating_add(written_bytes);
    }

    fn record_cancelled_decode_waste(&self, elapsed: Duration, written_bytes: u64) {
        let mut state = self.lock_state();
        state.cancelled_decode_executions = state.cancelled_decode_executions.saturating_add(1);
        state.cancelled_decode_time_ns = state
            .cancelled_decode_time_ns
            .saturating_add(duration_ns(elapsed));
        state.cancelled_decode_bytes = state.cancelled_decode_bytes.saturating_add(written_bytes);
    }

    fn begin_worker_claim(&self, priority: RequestPriority) {
        let mut state = self.lock_state();
        let priority_claims = &mut state.workers_with_priority_claims[usize::from(priority.rank())];
        *priority_claims = priority_claims.saturating_add(1);
        assert!(
            state.workers_with_priority_claims.iter().sum::<usize>() <= self.config.worker_limit(),
            "each worker owns at most one primary claim"
        );
    }

    fn end_worker_claim(&self, priority: RequestPriority) {
        let mut state = self.lock_state();
        let priority_claims = &mut state.workers_with_priority_claims[usize::from(priority.rank())];
        *priority_claims = priority_claims
            .checked_sub(1)
            .expect("each primary worker claim ends exactly once");
    }

    fn begin_decode_cohort(&self, members: usize) {
        let mut state = self.lock_state();
        state.active_decode_cohorts = state.active_decode_cohorts.saturating_add(1);
        state.decode_cohorts = state.decode_cohorts.saturating_add(1);
        state.decode_cohort_members = state
            .decode_cohort_members
            .saturating_add(u64::try_from(members).unwrap_or(u64::MAX));
        state.peak_decode_cohort_members = state.peak_decode_cohort_members.max(members);
        assert!(
            state.active_decode_cohorts <= self.config.worker_limit(),
            "one worker owns at most one active source cohort"
        );
        assert!(
            members <= self.config.worker_limit(),
            "one source cohort is bounded by the existing worker limit"
        );
    }

    fn end_decode_cohort(&self) {
        let mut state = self.lock_state();
        state.active_decode_cohorts = state
            .active_decode_cohorts
            .checked_sub(1)
            .expect("each active source cohort ends exactly once");
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
        state.progress_updates = state.progress_updates.saturating_add(1);
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

    fn finish_success(
        &self,
        job_id: u64,
        bytes: Box<[u8]>,
        facts: ResourcePayloadFacts,
        charge: LedgerCharge,
        decode_elapsed: Duration,
    ) {
        let target = {
            let mut state = self.lock_state();
            let Some(job) = state.jobs.get(&job_id) else {
                return;
            };
            if job.waiters.is_empty() {
                let discarded_bytes = job.descriptor.byte_len();
                state.cancelled_decode_executions =
                    state.cancelled_decode_executions.saturating_add(1);
                state.cancelled_decode_time_ns = state
                    .cancelled_decode_time_ns
                    .saturating_add(duration_ns(decode_elapsed));
                state.cancelled_decode_bytes =
                    state.cancelled_decode_bytes.saturating_add(discarded_bytes);
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
            state.cancelled_decode_executions = state.cancelled_decode_executions.saturating_add(1);
            state.cancelled_decode_time_ns = state
                .cancelled_decode_time_ns
                .saturating_add(duration_ns(decode_elapsed));
            state.cancelled_decode_bytes = state
                .cancelled_decode_bytes
                .saturating_add(job.descriptor.byte_len());
            return;
        }
        let lease = AccountedResourceLease {
            inner: Arc::new(AccountedPayload {
                key: job.key.resource(),
                descriptor: job.descriptor,
                facts,
                bytes,
                charge: RuntimeCharge::Production(charge),
            }),
        };
        if let Some(cache_charge) = cache_charge {
            let touch = state.touch();
            state.insert_cache_entry(
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
        loop {
            match charge.reclassify(target) {
                Ok(()) => return Ok(()),
                Err(error @ CpuLedgerError::CapacityExceeded { .. }) => {
                    let evicted = {
                        let mut state = self.lock_state();
                        let current_key = state.jobs.get(&job_id).map(|job| job.key.resource());
                        let candidate = state.oldest_cache_key(target, current_key);
                        candidate.and_then(|key| {
                            let removed = state.remove_cache_entry(&key);
                            if removed.is_some() {
                                state.cache_evictions = state.cache_evictions.saturating_add(1);
                            }
                            removed
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
    #[allow(
        clippy::result_large_err,
        reason = "the frozen DatasetSource contract requires its context-rich typed fault"
    )]
    fn submit_inner(&self, request: ResourceRequest) -> Result<RequestTicket, RuntimeFault> {
        let descriptor = self
            .shared
            .catalog
            .resource_payload_descriptor(request.resource())
            .map_err(|_| RuntimeFault::for_request(RuntimeFaultCode::SourceRejected, request))?;
        let working_bytes = catch_unwind(AssertUnwindSafe(|| {
            self.shared
                .source
                .minimum_decode_working_bytes(request.resource(), descriptor)
        }))
        .map_err(|_| RuntimeFault::for_request(RuntimeFaultCode::InvariantViolation, request))?
        .map_err(|fault| RuntimeFault::for_request(map_source_fault_code(&fault), request))?;
        let admission_bytes = descriptor
            .byte_len()
            .checked_add(working_bytes)
            .ok_or_else(|| {
                RuntimeFault::for_request(RuntimeFaultCode::MinimumWorkUnitExceedsBudget, request)
            })?;
        let destination = destination_category(request.priority());
        if admission_bytes
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
            state.touch_cache_entry(request.resource());
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
            state.cache_hits = state.cache_hits.saturating_add(1);
            state.push_completion(
                RuntimeCompletion::new(ticket, RuntimeOutcome::Ready(lease)),
                self.shared.config.completion_queue_limit(),
            );
            return Ok(ticket);
        }
        state.cache_misses = state.cache_misses.saturating_add(1);

        let progress = RuntimeRequestProgress::new(ticket, 0, descriptor.byte_len())
            .map_err(|code| RuntimeFault::for_ticket(code, ticket))?;
        let dedupe_key = request.dedupe_key();
        let job_id = if let Some(job_id) = state.dedupe.get(&dedupe_key).copied() {
            let mut queue_update = None;
            let mut priority_change = None;
            {
                let job = state
                    .jobs
                    .get_mut(&job_id)
                    .expect("the dedupe index points to a live job");
                job.waiters.push(id);
                if request.priority().outranks(job.priority) {
                    let previous = job.priority;
                    job.priority = request.priority();
                    if job.phase == JobPhase::Queued {
                        priority_change = Some((previous, job.priority));
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
            if let Some((previous, next)) = priority_change {
                state.change_queued_priority(previous, next);
            }
            if let Some(entry) = queue_update {
                state.enqueue_version(entry, self.shared.config.request_queue_limit());
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
                admission_bytes,
                waiters: vec![id],
                priority: request.priority(),
                phase: JobPhase::Queued,
                decode_started: false,
                queue_version: 0,
                queue_sequence: sequence,
                cancellation: Arc::new(AtomicBool::new(false)),
                queued_at: Instant::now(),
            };
            state.dedupe.insert(dedupe_key, job_id);
            state.jobs.insert(job_id, job);
            state.add_queued_job(admission_bytes, request.priority());
            state.enqueue_version(
                QueueEntry {
                    priority: request.priority(),
                    sequence,
                    job_id,
                    version: 0,
                },
                self.shared.config.request_queue_limit(),
            );
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
        let preemption_requested = state
            .request_foreground_preemption(request.priority(), self.shared.config.worker_limit());
        drop(state);
        if preemption_requested {
            self.shared.work_available.notify_all();
        } else {
            self.shared.work_available.notify_one();
        }
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

    fn capacity_epoch(&self) -> u64 {
        self.shared.ledger.capacity_epoch()
    }
}

impl DatasetRuntime for ProductionDatasetRuntime {
    fn config(&self) -> DatasetRuntimeConfig {
        self.shared.config
    }

    fn submit(&self, request: ResourceRequest) -> Result<RequestTicket, RuntimeFault> {
        self.submit_inner(request)
    }

    fn promote_priority(
        &self,
        ticket: RequestTicket,
        priority: RequestPriority,
    ) -> Result<bool, RuntimeFault> {
        let mut state = self.shared.lock_state();
        if state.shutdown != ShutdownState::Running {
            return Err(RuntimeFault::for_ticket(
                RuntimeFaultCode::ShuttingDown,
                ticket,
            ));
        }
        let Some(record) = state.requests.get(&ticket.id()) else {
            return Ok(false);
        };
        if record.ticket != ticket {
            return Err(RuntimeFault::for_ticket(
                RuntimeFaultCode::InvariantViolation,
                ticket,
            ));
        }
        if record.terminal || !priority.outranks(record.request.priority()) {
            return Ok(false);
        }
        let promoted_request = record.request.with_priority(priority);
        let job_id = record.job_id.ok_or_else(|| {
            RuntimeFault::for_ticket(RuntimeFaultCode::InvariantViolation, ticket)
        })?;

        let mut queue_update = None;
        let mut queued_priority_change = None;
        {
            let job = state.jobs.get_mut(&job_id).ok_or_else(|| {
                RuntimeFault::for_ticket(RuntimeFaultCode::InvariantViolation, ticket)
            })?;
            if priority.outranks(job.priority) {
                let previous = job.priority;
                job.priority = priority;
                if job.phase == JobPhase::Queued {
                    job.queue_version = job.queue_version.saturating_add(1);
                    queued_priority_change = Some((previous, priority));
                    queue_update = Some(QueueEntry {
                        priority,
                        sequence: job.queue_sequence,
                        job_id,
                        version: job.queue_version,
                    });
                }
            }
        }
        state
            .requests
            .get_mut(&ticket.id())
            .expect("the validated live request remains installed")
            .request = promoted_request;
        if let Some((previous, next)) = queued_priority_change {
            state.change_queued_priority(previous, next);
        }
        if let Some(entry) = queue_update {
            state.enqueue_version(entry, self.shared.config.request_queue_limit());
        }
        let preemption_requested =
            state.request_foreground_preemption(priority, self.shared.config.worker_limit());
        drop(state);
        if preemption_requested {
            self.shared.work_available.notify_all();
        } else {
            self.shared.work_available.notify_one();
        }
        Ok(true)
    }

    fn cancel_many(&self, tickets: &[RequestTicket]) -> Result<(), RuntimeFault> {
        if tickets.is_empty() {
            return Ok(());
        }
        let mut state = self.shared.lock_state();
        if state.shutdown != ShutdownState::Running {
            return Err(tickets.first().map_or_else(
                || RuntimeFault::new(RuntimeFaultCode::ShuttingDown),
                |ticket| RuntimeFault::for_ticket(RuntimeFaultCode::ShuttingDown, *ticket),
            ));
        }
        let mut has_live = false;
        for ticket in tickets {
            let Some(record) = state.requests.get(&ticket.id()) else {
                return Err(RuntimeFault::for_ticket(
                    RuntimeFaultCode::InvariantViolation,
                    *ticket,
                ));
            };
            if record.ticket != *ticket {
                return Err(RuntimeFault::for_ticket(
                    RuntimeFaultCode::InvariantViolation,
                    *ticket,
                ));
            }
            has_live |= !record.terminal;
        }
        if has_live {
            cancel_request_ids(
                &mut state,
                tickets.iter().map(|ticket| ticket.id()),
                self.shared.config.completion_queue_limit(),
                self.shared.config.request_queue_limit(),
            );
        }
        drop(state);
        if has_live {
            self.shared.work_available.notify_all();
        }
        Ok(())
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
            ids,
            self.shared.config.completion_queue_limit(),
            self.shared.config.request_queue_limit(),
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
        // Runtime mutations acquire the byte ledger before scheduler state.
        // Never invert that order here: diagnostics is allowed to observe a
        // near-simultaneous ledger snapshot, but it must not deadlock submit,
        // completion, or eviction while the panel is open.
        let ledger_snapshot = self.shared.ledger.snapshot();
        let state = self.shared.lock_state();
        let queued_requests = state
            .jobs
            .values()
            .filter(|job| job.phase == JobPhase::Queued)
            .map(|job| job.waiters.len())
            .sum();
        // This remains an execution/worker count, not a member count. A
        // worker owns one source cohort at a time; cumulative and peak cohort
        // member counts are exposed separately below.
        let in_flight_decodes = state.active_decode_cohorts;
        DatasetRuntimeDiagnostics::new_with_performance(
            self.shared.config,
            ledger_snapshot,
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
            DatasetRuntimePerformanceCounters::new(
                state.cache_hits,
                state.cache_misses,
                state.cache_evictions,
                state.progress_updates,
                state.queue_wait_ns,
                state.decode_time_ns,
                state.decoded_output_bytes,
                state.cancelled_decode_executions,
                state.cancelled_decode_time_ns,
                state.cancelled_decode_bytes,
                state.decode_cohorts,
                state.decode_cohort_members,
                u64::try_from(state.peak_decode_cohort_members).unwrap_or(u64::MAX),
            ),
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

fn cancel_request_ids(
    state: &mut RuntimeState,
    ids: impl IntoIterator<Item = RuntimeRequestId>,
    completion_limit: usize,
    request_limit: usize,
) {
    let mut affected_jobs = Vec::new();
    for id in ids {
        let Some(record) = state.requests.get_mut(&id) else {
            continue;
        };
        if record.terminal {
            continue;
        }
        record.terminal = true;
        let ticket = record.ticket;
        if let Some(job_id) = record.job_id {
            affected_jobs.push(job_id);
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
        if let Some(job) = state.jobs.get_mut(&job_id) {
            let requests = &state.requests;
            job.waiters
                .retain(|waiter| requests.get(waiter).is_some_and(|record| !record.terminal));
        }
        let Some(job) = state.jobs.get(&job_id) else {
            continue;
        };
        if job.waiters.is_empty() {
            match job.phase {
                JobPhase::Queued => {
                    state.remove_job(job_id);
                }
                JobPhase::Claimed
                | JobPhase::InFlight
                | JobPhase::Yielding
                | JobPhase::Aborting => {
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
        let (queue_update, priority_change) = {
            let job = state
                .jobs
                .get_mut(&job_id)
                .expect("the affected job remains present");
            if priority == job.priority {
                (None, None)
            } else {
                let previous = job.priority;
                job.priority = priority;
                if job.phase == JobPhase::Queued {
                    job.queue_version = job.queue_version.saturating_add(1);
                    (
                        Some(QueueEntry {
                            priority,
                            sequence: job.queue_sequence,
                            job_id,
                            version: job.queue_version,
                        }),
                        Some((previous, priority)),
                    )
                } else {
                    (None, None)
                }
            }
        };
        if let Some((previous, next)) = priority_change {
            state.change_queued_priority(previous, next);
        }
        if let Some(entry) = queue_update {
            state.enqueue_version(entry, request_limit);
        }
    }
}

fn worker_loop(shared: Arc<RuntimeShared>) {
    while let Some(first_claim) = shared.claim_job() {
        let observed_capacity_epoch = shared.ledger.capacity_epoch();
        let first_charge = match shared.ledger.acquire(
            CpuLedgerCategory::InFlightDecode,
            first_claim.descriptor.byte_len(),
        ) {
            Ok(charge) => charge,
            Err(CpuLedgerError::CapacityExceeded { .. }) => {
                if let Err(code) = shared.requeue_claim(&first_claim) {
                    shared.finish_failure(first_claim.job_id, code, false);
                    continue;
                }
                shared.ledger.wait_for_change_after(observed_capacity_epoch);
                continue;
            }
            Err(error) => {
                shared.finish_failure(first_claim.job_id, map_ledger_error_code(error), false);
                continue;
            }
        };

        shared.begin_worker_claim(first_claim.priority);
        let _worker_claim = WorkerClaimGuard {
            shared: Arc::clone(&shared),
            priority: first_claim.priority,
        };

        let mut claimed = vec![(first_claim, first_charge)];
        let mut uncharged_working_bytes = claimed[0]
            .0
            .admission_bytes
            .saturating_sub(claimed[0].0.descriptor.byte_len());
        // Only the highest priority can be safely executed as a multi-member
        // synchronous source cohort: it has no higher class that could arrive
        // while the source is between members. Lower classes remain one-member
        // cohorts, so newly visible work waits for at most the currently
        // decoding lower-priority member rather than an already-claimed batch.
        if claimed[0].0.priority == RequestPriority::CurrentView {
            while claimed.len() < shared.config.worker_limit() {
                // Final buffers already hold exact ledger charges. Subtract
                // every claimed member's not-yet-acquired source working bound
                // as a virtual reservation so this cohort cannot overcommit
                // itself and deterministically retry the same impossible
                // partition forever. Other workers remain intentionally
                // optimistic; their genuine races use the source retry path.
                let available = shared
                    .ledger
                    .available(CpuLedgerCategory::InFlightDecode)
                    .saturating_sub(uncharged_working_bytes);
                let Some(peer) =
                    shared.try_claim_cohort_peer(claimed[0].0.priority, available, claimed.len())
                else {
                    break;
                };
                match shared.ledger.acquire(
                    CpuLedgerCategory::InFlightDecode,
                    peer.descriptor.byte_len(),
                ) {
                    Ok(charge) => {
                        uncharged_working_bytes = uncharged_working_bytes.saturating_add(
                            peer.admission_bytes
                                .saturating_sub(peer.descriptor.byte_len()),
                        );
                        claimed.push((peer, charge));
                    }
                    Err(CpuLedgerError::CapacityExceeded { .. }) => {
                        if let Err(code) = shared.requeue_claim(&peer) {
                            shared.finish_failure(peer.job_id, code, false);
                        }
                        break;
                    }
                    Err(error) => {
                        shared.finish_failure(peer.job_id, map_ledger_error_code(error), false);
                        break;
                    }
                }
            }
        }

        let mut sinks = Vec::with_capacity(claimed.len());
        for (claim, charge) in claimed {
            if claim.cancellation.load(AtomicOrdering::Acquire) {
                shared.finish_failure(claim.job_id, RuntimeFaultCode::Cancelled, false);
                continue;
            }
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
            sinks.push(RuntimeDecodeSink {
                shared: Arc::clone(&shared),
                job_id: claim.job_id,
                key: claim.key,
                descriptor: claim.descriptor,
                cancellation: claim.cancellation,
                buffer,
                written: 0,
                last_reported: 0,
                offered_span: 0,
                finished: false,
                facts: None,
                charge: Some(charge),
            });
        }
        if sinks.is_empty() {
            continue;
        }

        shared.begin_decode_cohort(sinks.len());
        let decode_started = Instant::now();
        let decode = catch_unwind(AssertUnwindSafe(|| {
            let mut cohort = sinks
                .iter_mut()
                .map(|sink| sink as &mut dyn ReservedDecodeSink)
                .collect::<Vec<_>>();
            shared.source.decode_cohort_into(&mut cohort)
        }));
        let decode_elapsed = decode_started.elapsed();
        shared.end_decode_cohort();
        let written_bytes = sinks.iter().fold(0_u64, |total, sink| {
            total.saturating_add(sink.written as u64)
        });
        shared.record_decode_observation(decode_elapsed, written_bytes);
        let member_elapsed = decode_elapsed / u32::try_from(sinks.len()).unwrap_or(u32::MAX);
        let outcomes = match decode {
            Err(_) => None,
            Ok(outcomes) if outcomes.len() != sinks.len() => None,
            Ok(outcomes) => Some(outcomes),
        };
        for (index, sink) in sinks.into_iter().enumerate() {
            let job_id = sink.job_id;
            let outcome = outcomes
                .as_ref()
                .map(|outcomes| &outcomes[index])
                .map(|outcome| outcome.as_ref().map_err(map_source_fault_code));
            match outcome {
                None => shared.finish_failure(job_id, RuntimeFaultCode::InvariantViolation, true),
                Some(Err(code)) => {
                    if code == RuntimeFaultCode::Cancelled {
                        let written = sink.written as u64;
                        drop(sink);
                        // Only a runtime-issued Yielding phase may convert the
                        // source's cancellation outcome back into queued work.
                        // Ordinary cancellation first moves the job to
                        // Aborting and therefore remains terminal.
                        match shared.retry_preempted(job_id) {
                            Ok(true) => continue,
                            Ok(false) => {
                                shared.record_cancelled_decode_waste(member_elapsed, written);
                                shared.finish_failure(job_id, code, true);
                            }
                            Err(retry_code) => {
                                shared.finish_failure(job_id, retry_code, true);
                            }
                        }
                        continue;
                    }
                    if sink.written == 0
                        && !sink.finished
                        && matches!(
                            code,
                            RuntimeFaultCode::CapacityExceeded {
                                category: CpuLedgerCategory::InFlightDecode,
                                ..
                            }
                        )
                    {
                        // The source's pure admission bound established that
                        // this job fits when run alone. A source-side capacity
                        // miss is therefore only a race with another cohort's
                        // exact staging/scratch leases. Release this final
                        // buffer and retry instead of terminally failing valid
                        // submitted work.
                        drop(sink);
                        if let Err(retry_code) = shared.retry_in_flight_capacity(job_id) {
                            shared.finish_failure(job_id, retry_code, true);
                        }
                        continue;
                    }
                    shared.finish_failure(job_id, code, true);
                }
                Some(Ok(())) if !sink.finished => {
                    shared.finish_failure(job_id, RuntimeFaultCode::SinkRejected, true)
                }
                Some(Ok(())) => {
                    let (bytes, facts, charge) = sink.into_parts();
                    shared.finish_success(job_id, bytes, facts, charge, member_elapsed);
                }
            }
        }
    }
}

struct WorkerClaimGuard {
    shared: Arc<RuntimeShared>,
    priority: RequestPriority,
}

impl Drop for WorkerClaimGuard {
    fn drop(&mut self) {
        self.shared.end_worker_claim(self.priority);
    }
}

struct RuntimeDecodeSink {
    shared: Arc<RuntimeShared>,
    job_id: u64,
    key: BrickKey,
    descriptor: ResourcePayloadDescriptor,
    cancellation: Arc<AtomicBool>,
    buffer: Vec<u8>,
    written: usize,
    last_reported: usize,
    offered_span: usize,
    finished: bool,
    facts: Option<ResourcePayloadFacts>,
    charge: Option<LedgerCharge>,
}

impl RuntimeDecodeSink {
    fn into_parts(mut self) -> (Box<[u8]>, ResourcePayloadFacts, LedgerCharge) {
        assert!(self.finished);
        (
            std::mem::take(&mut self.buffer).into_boxed_slice(),
            self.facts
                .take()
                .expect("a completed sink retains decoded payload facts"),
            self.charge
                .take()
                .expect("a completed sink retains its in-flight charge"),
        )
    }

    fn report_progress_after_advance(&mut self) {
        let report = self.last_reported == 0
            || self.written == self.buffer.len()
            || self.written.saturating_sub(self.last_reported) >= PROGRESS_UPDATE_GRANULARITY_BYTES;
        if report {
            self.shared.update_progress(
                self.job_id,
                self.written as u64,
                self.descriptor.byte_len(),
            );
            self.last_reported = self.written;
        }
    }
}

impl ReservedDecodeSink for RuntimeDecodeSink {
    fn resource_key(&self) -> BrickKey {
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

    fn writable_span(&mut self, maximum_bytes: usize) -> Result<&mut [u8], DecodeSinkError> {
        if self.is_cancelled() {
            return Err(DecodeSinkError::Cancelled);
        }
        if self.finished {
            return Err(DecodeSinkError::AlreadyFinished);
        }
        if self.offered_span != 0 {
            return Err(DecodeSinkError::WritableSpanOutstanding);
        }
        let remaining = self.buffer.len().saturating_sub(self.written);
        if maximum_bytes == 0 || remaining == 0 {
            return Err(DecodeSinkError::InvalidWritableSpanRequest);
        }
        let offered = remaining.min(maximum_bytes);
        self.offered_span = offered;
        Ok(&mut self.buffer[self.written..self.written + offered])
    }

    fn commit_written(&mut self, bytes: usize) -> Result<(), DecodeSinkError> {
        if self.is_cancelled() {
            return Err(DecodeSinkError::Cancelled);
        }
        if self.finished {
            return Err(DecodeSinkError::AlreadyFinished);
        }
        let offered = self.offered_span;
        if offered == 0 {
            return Err(DecodeSinkError::WritableCommitWithoutSpan);
        }
        if bytes > offered {
            return Err(DecodeSinkError::WritableCommitExceeded {
                offered,
                attempted: bytes,
            });
        }
        self.written = self
            .written
            .checked_add(bytes)
            .ok_or(DecodeSinkError::ByteCountOverflow)?;
        self.offered_span = 0;
        self.report_progress_after_advance();
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), DecodeSinkError> {
        if self.is_cancelled() {
            return Err(DecodeSinkError::Cancelled);
        }
        if self.finished {
            return Err(DecodeSinkError::AlreadyFinished);
        }
        if self.offered_span != 0 {
            return Err(DecodeSinkError::WritableSpanOutstanding);
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
        self.report_progress_after_advance();
        Ok(())
    }

    fn finish(&mut self) -> Result<(), DecodeSinkError> {
        if self.is_cancelled() {
            return Err(DecodeSinkError::Cancelled);
        }
        if self.finished {
            return Err(DecodeSinkError::AlreadyFinished);
        }
        if self.offered_span != 0 {
            return Err(DecodeSinkError::WritableSpanOutstanding);
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
        let payload = self.descriptor.view(values, validity).map_err(|_| {
            DecodeSinkError::ReservationExceeded {
                reserved: self.descriptor.byte_len(),
                attempted: self.descriptor.byte_len(),
            }
        })?;
        self.facts = Some(
            ResourcePayloadFacts::from_payload(payload)
                .map_err(|_| DecodeSinkError::InvalidPayloadValues)?,
        );
        self.finished = true;
        Ok(())
    }

    fn finish_with_facts(&mut self, facts: ResourcePayloadFacts) -> Result<(), DecodeSinkError> {
        if self.is_cancelled() {
            return Err(DecodeSinkError::Cancelled);
        }
        if self.finished {
            return Err(DecodeSinkError::AlreadyFinished);
        }
        if self.offered_span != 0 {
            return Err(DecodeSinkError::WritableSpanOutstanding);
        }
        if self.written != self.buffer.len() {
            return Err(DecodeSinkError::Incomplete {
                reserved: self.descriptor.byte_len(),
                written: self.written as u64,
            });
        }
        if self.descriptor.validity() == mirante4d_dataset::ResourceValidity::AllValid
            && !facts.all_valid()
        {
            return Err(DecodeSinkError::InvalidPayloadValues);
        }
        self.facts = Some(facts);
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

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn reclassify_cached_with_eviction(
    state: &mut RuntimeState,
    current_key: BrickKey,
    charge: &LedgerCharge,
    target: CpuLedgerCategory,
) -> Result<(), CpuLedgerError> {
    let old_category = charge.category();
    loop {
        match charge.reclassify(target) {
            Ok(()) => {
                state.reclassify_cache_entry_index(current_key, old_category, target);
                return Ok(());
            }
            Err(error @ CpuLedgerError::CapacityExceeded { .. }) => {
                let candidate = state.oldest_cache_key(target, Some(current_key));
                let Some(candidate) = candidate else {
                    return Err(error);
                };
                let removed = state.remove_cache_entry(&candidate);
                if removed.is_some() {
                    state.cache_evictions = state.cache_evictions.saturating_add(1);
                }
                drop(removed);
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
        DatasetLayer, DatasetResourceIdentity, DatasetSourceId, ResourceRegion, ResourceValidity,
        ScientificIdentityStatus,
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
        AfterFinish,
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
        finished: usize,
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
                        finished: 0,
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

        fn wait_finished(&self) {
            let deadline = Instant::now() + Duration::from_secs(2);
            let (lock, changed) = &self.gate;
            let mut state = lock.lock().unwrap();
            while state.finished == 0 {
                assert!(Instant::now() < deadline, "decode did not finish in time");
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

        #[allow(
            clippy::result_large_err,
            reason = "the frozen DatasetSource contract requires this exact typed fault"
        )]
        fn decode_cohort_into(
            &self,
            sinks: &mut [&mut dyn ReservedDecodeSink],
        ) -> Vec<Result<(), DatasetSourceFault>> {
            sinks
                .iter_mut()
                .map(|sink| {
                    let sink = &mut **sink;
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
                        sink.write(&values[..1]).map_err(|reason| {
                            DatasetSourceFault::SinkRejected {
                                key: sink.resource_key(),
                                reason: Box::new(reason),
                            }
                        })?;
                        {
                            let (lock, changed) = &self.gate;
                            lock.lock().unwrap().first_byte_written += 1;
                            changed.notify_all();
                        }
                        self.wait_gate(sink)?;
                        sink.write(&values[1..]).map_err(|reason| {
                            DatasetSourceFault::SinkRejected {
                                key: sink.resource_key(),
                                reason: Box::new(reason),
                            }
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
                        let mut mask =
                            vec![0_u8; usize::try_from(descriptor.validity_byte_len()).unwrap()];
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
                        })?;
                    if matches!(self.gate_point, GatePoint::AfterFinish) {
                        let (lock, changed) = &self.gate;
                        let mut state = lock.lock().unwrap();
                        state.finished += 1;
                        changed.notify_all();
                        while !state.released {
                            state = changed.wait(state).unwrap();
                        }
                    }
                    Ok(())
                })
                .collect()
        }
    }

    #[derive(Clone, Copy)]
    enum DirectSpanBehavior {
        Complete,
        OverrunCommit,
        IncompleteFinish,
        OutstandingFinish,
    }

    struct DirectSpanSource {
        catalog: Arc<DatasetCatalog>,
        behavior: DirectSpanBehavior,
    }

    struct TransientCapacitySource {
        catalog: Arc<DatasetCatalog>,
        attempts: AtomicUsize,
    }

    struct CohortAdmissionSource {
        catalog: Arc<DatasetCatalog>,
        ledger: Arc<dyn CpuByteLedger>,
        control: Arc<CohortAdmissionControl>,
    }

    #[derive(Default)]
    struct CohortAdmissionState {
        blocker_entered: bool,
        blocker_released: bool,
        target_cohort_sizes: Vec<usize>,
        target_capacity_failures: usize,
    }

    #[derive(Default)]
    struct CohortAdmissionControl {
        state: Mutex<CohortAdmissionState>,
        changed: Condvar,
    }

    impl CohortAdmissionControl {
        fn wait_for_blocker(&self) {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut state = self.state.lock().unwrap();
            while !state.blocker_entered {
                assert!(Instant::now() < deadline, "blocker did not occupy a worker");
                state = self
                    .changed
                    .wait_timeout(state, Duration::from_millis(5))
                    .unwrap()
                    .0;
            }
        }

        fn release_blocker(&self) {
            self.state.lock().unwrap().blocker_released = true;
            self.changed.notify_all();
        }
    }

    const VISIBLE_BURST_ORIGIN: u64 = 10_000;

    struct BurstGateSource {
        catalog: Arc<DatasetCatalog>,
        gate: (Mutex<BurstGateState>, Condvar),
    }

    #[derive(Default)]
    struct BurstGateState {
        low_members_entered: usize,
        low_released: bool,
        visible_members_entered: usize,
        visible_cohorts_entered: usize,
        active_visible_cohorts: usize,
        peak_active_visible_cohorts: usize,
        largest_visible_cohort: usize,
        visible_released: bool,
    }

    impl DirectSpanSource {
        fn new(behavior: DirectSpanBehavior) -> Arc<Self> {
            let layer = DatasetLayer::new(
                LogicalLayerKey::new(0),
                "intensity",
                Shape4D::new(1, 1, 1, 65_536).unwrap(),
                IntensityDType::Uint8,
                GridToWorld::identity(),
                ResourceValidity::AllValid,
            )
            .unwrap();
            Arc::new(Self {
                catalog: Arc::new(
                    DatasetCatalog::new(
                        "direct-span-runtime-test",
                        ScientificIdentityStatus::Unverified(DatasetSourceId::new(41)),
                        vec![layer],
                    )
                    .unwrap(),
                ),
                behavior,
            })
        }
    }

    impl TransientCapacitySource {
        fn new() -> Arc<Self> {
            let layer = DatasetLayer::new(
                LogicalLayerKey::new(0),
                "intensity",
                Shape4D::new(1, 1, 1, 65_536).unwrap(),
                IntensityDType::Uint8,
                GridToWorld::identity(),
                ResourceValidity::AllValid,
            )
            .unwrap();
            Arc::new(Self {
                catalog: Arc::new(
                    DatasetCatalog::new(
                        "transient-capacity-runtime-test",
                        ScientificIdentityStatus::Unverified(DatasetSourceId::new(41)),
                        vec![layer],
                    )
                    .unwrap(),
                ),
                attempts: AtomicUsize::new(0),
            })
        }
    }

    impl BurstGateSource {
        fn new() -> Arc<Self> {
            let layer = DatasetLayer::new(
                LogicalLayerKey::new(0),
                "intensity",
                Shape4D::new(1, 1, 1, 65_536).unwrap(),
                IntensityDType::Uint8,
                GridToWorld::identity(),
                ResourceValidity::AllValid,
            )
            .unwrap();
            Arc::new(Self {
                catalog: Arc::new(
                    DatasetCatalog::new(
                        "burst-gate-runtime-test",
                        ScientificIdentityStatus::Unverified(DatasetSourceId::new(41)),
                        vec![layer],
                    )
                    .unwrap(),
                ),
                gate: (Mutex::new(BurstGateState::default()), Condvar::new()),
            })
        }

        fn wait_for(&self, predicate: impl Fn(&BurstGateState) -> bool, reason: &'static str) {
            let deadline = Instant::now() + Duration::from_secs(3);
            let (lock, changed) = &self.gate;
            let mut state = lock.lock().unwrap();
            while !predicate(&state) {
                assert!(Instant::now() < deadline, "{reason}");
                state = changed
                    .wait_timeout(state, Duration::from_millis(5))
                    .unwrap()
                    .0;
            }
        }

        fn release_low(&self) {
            let (lock, changed) = &self.gate;
            lock.lock().unwrap().low_released = true;
            changed.notify_all();
        }

        fn release_visible(&self) {
            let (lock, changed) = &self.gate;
            lock.lock().unwrap().visible_released = true;
            changed.notify_all();
        }
    }

    impl DatasetSource for DirectSpanSource {
        fn catalog(&self) -> Result<Arc<DatasetCatalog>, DatasetSourceFault> {
            Ok(Arc::clone(&self.catalog))
        }

        #[allow(
            clippy::result_large_err,
            reason = "the frozen DatasetSource contract requires this exact typed fault"
        )]
        fn decode_cohort_into(
            &self,
            sinks: &mut [&mut dyn ReservedDecodeSink],
        ) -> Vec<Result<(), DatasetSourceFault>> {
            sinks
                .iter_mut()
                .map(|sink| {
                    let sink = &mut **sink;
                    let key = sink.resource_key();
                    let result = (|| -> Result<(), DecodeSinkError> {
                        match self.behavior {
                            DirectSpanBehavior::Complete => {
                                {
                                    let span = sink.writable_span(2)?;
                                    span.copy_from_slice(&[11, 12]);
                                }
                                sink.commit_written(2)?;
                                {
                                    let span = sink.writable_span(usize::MAX)?;
                                    span.copy_from_slice(&[13, 14]);
                                }
                                sink.commit_written(2)?;
                                sink.finish()
                            }
                            DirectSpanBehavior::OverrunCommit => {
                                let _ = sink.writable_span(2)?;
                                sink.commit_written(3)
                            }
                            DirectSpanBehavior::IncompleteFinish => {
                                {
                                    let span = sink.writable_span(2)?;
                                    span[0] = 11;
                                }
                                sink.commit_written(1)?;
                                sink.finish()
                            }
                            DirectSpanBehavior::OutstandingFinish => {
                                let _ = sink.writable_span(2)?;
                                sink.finish()
                            }
                        }
                    })();
                    result.map_err(|reason| DatasetSourceFault::SinkRejected {
                        key,
                        reason: Box::new(reason),
                    })
                })
                .collect()
        }
    }

    impl DatasetSource for TransientCapacitySource {
        fn catalog(&self) -> Result<Arc<DatasetCatalog>, DatasetSourceFault> {
            Ok(Arc::clone(&self.catalog))
        }

        fn minimum_decode_working_bytes(
            &self,
            _key: BrickKey,
            _descriptor: ResourcePayloadDescriptor,
        ) -> Result<u64, DatasetSourceFault> {
            Ok(64)
        }

        #[allow(
            clippy::result_large_err,
            reason = "the frozen DatasetSource contract requires this exact typed fault"
        )]
        fn decode_cohort_into(
            &self,
            sinks: &mut [&mut dyn ReservedDecodeSink],
        ) -> Vec<Result<(), DatasetSourceFault>> {
            if self.attempts.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
                return sinks
                    .iter()
                    .map(|sink| DatasetSourceFault::CapacityExceeded {
                        key: sink.resource_key(),
                        category: CpuLedgerCategory::InFlightDecode,
                        requested_bytes: 64,
                        available_bytes: 0,
                    })
                    .map(Err)
                    .collect();
            }

            sinks
                .iter_mut()
                .map(|sink| {
                    let sink = &mut **sink;
                    let key = sink.resource_key();
                    let bytes =
                        vec![7; usize::try_from(sink.payload_descriptor().byte_len()).unwrap()];
                    sink.write(&bytes)
                        .and_then(|()| sink.finish())
                        .map_err(|reason| DatasetSourceFault::SinkRejected {
                            key,
                            reason: Box::new(reason),
                        })
                })
                .collect()
        }
    }

    impl DatasetSource for CohortAdmissionSource {
        fn catalog(&self) -> Result<Arc<DatasetCatalog>, DatasetSourceFault> {
            Ok(Arc::clone(&self.catalog))
        }

        fn minimum_decode_working_bytes(
            &self,
            _key: BrickKey,
            descriptor: ResourcePayloadDescriptor,
        ) -> Result<u64, DatasetSourceFault> {
            Ok(if descriptor.byte_len() == 1 {
                0
            } else {
                30_000
            })
        }

        #[allow(
            clippy::result_large_err,
            reason = "the frozen DatasetSource contract requires this exact typed fault"
        )]
        fn decode_cohort_into(
            &self,
            sinks: &mut [&mut dyn ReservedDecodeSink],
        ) -> Vec<Result<(), DatasetSourceFault>> {
            if sinks[0].payload_descriptor().byte_len() == 1 {
                let mut state = self.control.state.lock().unwrap();
                state.blocker_entered = true;
                self.control.changed.notify_all();
                while !state.blocker_released {
                    state = self.control.changed.wait(state).unwrap();
                }
                drop(state);
            } else {
                self.control
                    .state
                    .lock()
                    .unwrap()
                    .target_cohort_sizes
                    .push(sinks.len());
            }

            // Model an all-or-none source cohort such as unaligned staging:
            // every member's zero-publication working set must coexist before
            // shared decode may begin. A partial acquisition is discarded and
            // the complete cohort is retried without publishing sink bytes.
            let mut working = Vec::with_capacity(sinks.len());
            if sinks[0].payload_descriptor().byte_len() != 1 {
                for _ in 0..sinks.len() {
                    match self
                        .ledger
                        .try_acquire(CpuLedgerCategory::InFlightDecode, 30_000)
                    {
                        Ok(lease) => working.push(lease),
                        Err(error) => {
                            self.control.state.lock().unwrap().target_capacity_failures += 1;
                            return sinks
                                .iter()
                                .map(|sink| {
                                    let key = sink.resource_key();
                                    Err(match error {
                                        CpuLedgerError::CapacityExceeded {
                                            category,
                                            requested_bytes,
                                            available_bytes,
                                        } => DatasetSourceFault::CapacityExceeded {
                                            key,
                                            category,
                                            requested_bytes,
                                            available_bytes,
                                        },
                                        CpuLedgerError::ShuttingDown => {
                                            DatasetSourceFault::ShuttingDown {
                                                key,
                                                category: CpuLedgerCategory::InFlightDecode,
                                                requested_bytes: 30_000,
                                            }
                                        }
                                        CpuLedgerError::ZeroByteReservation => {
                                            DatasetSourceFault::DecodeFailed { key }
                                        }
                                    })
                                })
                                .collect();
                        }
                    }
                }
            }

            sinks
                .iter_mut()
                .map(|sink| {
                    let sink = &mut **sink;
                    let key = sink.resource_key();
                    let bytes =
                        vec![9; usize::try_from(sink.payload_descriptor().byte_len()).unwrap()];
                    sink.write(&bytes)
                        .and_then(|()| sink.finish())
                        .map_err(|reason| DatasetSourceFault::SinkRejected {
                            key,
                            reason: Box::new(reason),
                        })
                })
                .collect::<Vec<_>>()
        }
    }

    impl DatasetSource for BurstGateSource {
        fn catalog(&self) -> Result<Arc<DatasetCatalog>, DatasetSourceFault> {
            Ok(Arc::clone(&self.catalog))
        }

        #[allow(
            clippy::result_large_err,
            reason = "the frozen DatasetSource contract requires this exact typed fault"
        )]
        fn decode_cohort_into(
            &self,
            sinks: &mut [&mut dyn ReservedDecodeSink],
        ) -> Vec<Result<(), DatasetSourceFault>> {
            let visible = sinks[0].resource_key().region().origin()[2] >= VISIBLE_BURST_ORIGIN;
            let (lock, changed) = &self.gate;
            let mut state = lock.lock().unwrap();
            if visible {
                state.visible_members_entered += sinks.len();
                state.visible_cohorts_entered += 1;
                state.active_visible_cohorts += 1;
                state.peak_active_visible_cohorts = state
                    .peak_active_visible_cohorts
                    .max(state.active_visible_cohorts);
                state.largest_visible_cohort = state.largest_visible_cohort.max(sinks.len());
                changed.notify_all();
                while !state.visible_released {
                    state = changed.wait(state).unwrap();
                }
                state.active_visible_cohorts -= 1;
            } else {
                state.low_members_entered += sinks.len();
                changed.notify_all();
                while !state.low_released {
                    state = changed.wait(state).unwrap();
                }
            }
            drop(state);

            sinks
                .iter_mut()
                .map(|sink| {
                    let sink = &mut **sink;
                    let key = sink.resource_key();
                    let bytes =
                        vec![0; usize::try_from(sink.payload_descriptor().byte_len()).unwrap()];
                    sink.write(&bytes)
                        .and_then(|()| sink.finish())
                        .map_err(|reason| DatasetSourceFault::SinkRejected {
                            key,
                            reason: Box::new(reason),
                        })
                })
                .collect()
        }
    }

    fn start_direct_span_source(source: Arc<DirectSpanSource>) -> Arc<dyn DatasetRuntime> {
        let config = DatasetRuntimeConfig::new(1 << 20, 1, 4, 4).unwrap();
        let source_for_factory = Arc::clone(&source);
        let (runtime, catalog) = <dyn DatasetRuntime>::start(config, move |_| {
            let source: Arc<dyn DatasetSource> = source_for_factory;
            Ok(source)
        })
        .unwrap();
        assert!(Arc::ptr_eq(&catalog, &source.catalog));
        runtime
    }

    fn start_burst_gate_source(source: Arc<BurstGateSource>) -> Arc<dyn DatasetRuntime> {
        let config = DatasetRuntimeConfig::new(1 << 20, 8, 32, 32).unwrap();
        let source_for_factory = Arc::clone(&source);
        let (runtime, catalog) = <dyn DatasetRuntime>::start(config, move |_| {
            let source: Arc<dyn DatasetSource> = source_for_factory;
            Ok(source)
        })
        .unwrap();
        assert!(Arc::ptr_eq(&catalog, &source.catalog));
        runtime
    }

    fn key(origin_x: u64, samples: u64) -> BrickKey {
        BrickKey::new(
            DatasetResourceIdentity::Unverified(DatasetSourceId::new(41)),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, origin_x], Shape3D::new(1, 1, samples).unwrap()).unwrap(),
        )
    }

    fn request(
        resource: BrickKey,
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
    fn scheduler_queue_lazily_versions_entries_and_compacts_at_a_hard_bound() {
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
                admission_bytes: descriptor.byte_len(),
                waiters: Vec::new(),
                priority: RequestPriority::Prefetch,
                phase: JobPhase::Queued,
                decode_started: false,
                queue_version: 0,
                queue_sequence: sequence,
                cancellation: Arc::new(AtomicBool::new(false)),
                queued_at: Instant::now(),
            },
        );
        state.add_queued_job(descriptor.byte_len(), RequestPriority::Prefetch);
        state.enqueue_version(
            QueueEntry {
                priority: RequestPriority::Prefetch,
                sequence,
                job_id,
                version: 0,
            },
            2,
        );

        for (version, priority) in [
            RequestPriority::Playback,
            RequestPriority::LinkedView,
            RequestPriority::CurrentView,
        ]
        .into_iter()
        .enumerate()
        {
            let version = version as u64 + 1;
            let previous = state.jobs[&job_id].priority;
            {
                let job = state.jobs.get_mut(&job_id).unwrap();
                job.priority = priority;
                job.queue_version = version;
            }
            state.change_queued_priority(previous, priority);
            state.enqueue_version(
                QueueEntry {
                    priority,
                    sequence,
                    job_id,
                    version,
                },
                2,
            );
            assert_eq!(state.queue.len(), usize::try_from(version + 1).unwrap());
            assert_eq!(state.queue.peek().unwrap().version, version);
        }

        // The next update reaches twice the request-admission bound, compacts
        // stale versions once, and then appends the new current version.
        let previous = state.jobs[&job_id].priority;
        {
            let job = state.jobs.get_mut(&job_id).unwrap();
            job.priority = RequestPriority::VisibleRefinement;
            job.queue_version = 4;
        }
        state.change_queued_priority(previous, RequestPriority::VisibleRefinement);
        state.enqueue_version(
            QueueEntry {
                priority: RequestPriority::VisibleRefinement,
                sequence,
                job_id,
                version: 4,
            },
            2,
        );
        assert_eq!(state.queue.len(), 1);

        state.remove_job(job_id);
        assert!(state.queue.iter().all(|entry| entry.job_id == job_id));
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
        let performance = runtime.diagnostics().unwrap().performance();
        assert_eq!(performance.cache_hits(), 1);
        assert_eq!(performance.cache_misses(), 2);
        assert_eq!(performance.cache_evictions(), 0);
        assert_eq!(performance.decoded_output_bytes(), 4);
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
    fn production_runtime_ticket_cancellation_preserves_shared_decode_and_scope() {
        let source = TestSource::new(ResourceValidity::AllValid, GatePoint::BeforeWrite);
        let runtime = start(Arc::clone(&source), 1, 8, 8);
        let resource = key(13, 4);
        let retired = runtime
            .submit(request(resource, RequestPriority::CurrentView, 10, 1))
            .unwrap();
        source.wait_entered(1);
        let retained = runtime
            .submit(request(resource, RequestPriority::CurrentView, 10, 1))
            .unwrap();
        runtime.cancel(retired).unwrap();
        source.release();

        let completions = wait_completions(&runtime, 2);
        assert!(completions.iter().any(|completion| {
            completion.ticket() == retired
                && matches!(completion.outcome(), RuntimeOutcome::Cancelled)
        }));
        assert!(completions.iter().any(|completion| {
            completion.ticket() == retained
                && matches!(completion.outcome(), RuntimeOutcome::Ready(_))
        }));
        assert_eq!(source.decode_count.load(AtomicOrdering::SeqCst), 1);

        // The scope generation remains current and accepts new differential
        // demand without cancelling retained work.
        runtime
            .submit(request(key(21, 4), RequestPriority::CurrentView, 10, 1))
            .unwrap();
        assert!(matches!(
            wait_completions(&runtime, 1)[0].outcome(),
            RuntimeOutcome::Ready(_)
        ));
    }

    #[test]
    fn cancel_many_prevalidates_every_ticket_before_atomic_retirement() {
        let source = TestSource::new(ResourceValidity::AllValid, GatePoint::BeforeWrite);
        let runtime = start(Arc::clone(&source), 1, 8, 8);
        let first = runtime
            .submit(request(key(0, 4), RequestPriority::CurrentView, 1, 0))
            .unwrap();
        source.wait_entered(1);
        let second = runtime
            .submit(request(key(8, 4), RequestPriority::CurrentView, 2, 0))
            .unwrap();
        let forged = RequestTicket::for_request(
            second.id(),
            request(key(16, 4), RequestPriority::CurrentView, 2, 0),
        );

        assert_eq!(
            runtime.cancel_many(&[first, forged]).unwrap_err().code(),
            RuntimeFaultCode::InvariantViolation
        );
        assert!(runtime.progress(first).unwrap().is_some());
        assert!(runtime.progress(second).unwrap().is_some());
        source.release();
        let completions = wait_completions(&runtime, 2);
        assert!(
            completions
                .iter()
                .all(|completion| matches!(completion.outcome(), RuntimeOutcome::Ready(_)))
        );
    }

    #[test]
    fn cancel_many_retires_active_and_queued_waiters_together() {
        let source = TestSource::new(ResourceValidity::AllValid, GatePoint::BeforeWrite);
        let runtime = start(Arc::clone(&source), 1, 8, 8);
        let first = runtime
            .submit(request(key(0, 4), RequestPriority::CurrentView, 1, 0))
            .unwrap();
        source.wait_entered(1);
        let second = runtime
            .submit(request(key(8, 4), RequestPriority::CurrentView, 2, 0))
            .unwrap();
        runtime.cancel_many(&[first, second, first]).unwrap();
        let completions = wait_completions(&runtime, 2);
        assert!(
            completions
                .iter()
                .all(|completion| matches!(completion.outcome(), RuntimeOutcome::Cancelled))
        );
        assert_eq!(runtime.progress(first).unwrap(), None);
        assert_eq!(runtime.progress(second).unwrap(), None);
        let deadline = Instant::now() + Duration::from_secs(2);
        let diagnostics = loop {
            let diagnostics = runtime.diagnostics().unwrap();
            if diagnostics.in_flight_decodes() == 0 {
                break diagnostics;
            }
            assert!(Instant::now() < deadline, "cancelled cohort did not retire");
            thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(diagnostics.cancelled_requests(), 2);
    }

    #[test]
    fn cancel_many_duplicate_max_batch_is_linear_and_empty_shutdown_batch_is_noop() {
        const BATCH: usize = 65_536;
        let source = TestSource::new(ResourceValidity::AllValid, GatePoint::BeforeWrite);
        let runtime = start(Arc::clone(&source), 1, 2, 2);
        let ticket = runtime
            .submit(request(key(0, 4), RequestPriority::CurrentView, 1, 0))
            .unwrap();
        source.wait_entered(1);
        let tickets = vec![ticket; BATCH];
        runtime.cancel_many(&tickets).unwrap();
        assert!(matches!(
            wait_completions(&runtime, 1)[0].outcome(),
            RuntimeOutcome::Cancelled
        ));
        runtime.request_shutdown().unwrap();
        assert_eq!(runtime.cancel_many(&[]), Ok(()));
    }

    #[test]
    fn cancellation_waste_counts_only_bytes_from_a_discarded_started_decode() {
        let source = TestSource::new(ResourceValidity::AllValid, GatePoint::AfterFirstByte);
        let runtime = start(Arc::clone(&source), 1, 8, 8);
        let ticket = runtime
            .submit(request(key(19, 4), RequestPriority::CurrentView, 10, 1))
            .unwrap();
        source.wait_first_byte();
        runtime.cancel(ticket).unwrap();
        assert!(matches!(
            wait_completions(&runtime, 1)[0].outcome(),
            RuntimeOutcome::Cancelled
        ));

        let deadline = Instant::now() + Duration::from_secs(2);
        let diagnostics = loop {
            let diagnostics = runtime.diagnostics().unwrap();
            if diagnostics.completed_decodes() == 1 {
                break diagnostics;
            }
            assert!(Instant::now() < deadline, "cancelled decode did not retire");
            thread::sleep(Duration::from_millis(1));
        };
        let performance = diagnostics.performance();
        assert_eq!(performance.decoded_output_bytes(), 1);
        assert_eq!(performance.cancelled_decode_executions(), 1);
        assert_eq!(performance.cancelled_decode_bytes(), 1);
        assert!(performance.cancelled_decode_time_ns() > 0);
    }

    #[test]
    fn cancellation_after_source_finish_counts_the_discarded_complete_payload() {
        let source = TestSource::new(ResourceValidity::AllValid, GatePoint::AfterFinish);
        let runtime = start(Arc::clone(&source), 1, 8, 8);
        let ticket = runtime
            .submit(request(key(23, 4), RequestPriority::CurrentView, 10, 1))
            .unwrap();
        source.wait_finished();
        runtime.cancel(ticket).unwrap();
        source.release();
        assert!(matches!(
            wait_completions(&runtime, 1)[0].outcome(),
            RuntimeOutcome::Cancelled
        ));

        let deadline = Instant::now() + Duration::from_secs(2);
        let diagnostics = loop {
            let diagnostics = runtime.diagnostics().unwrap();
            if diagnostics.completed_decodes() == 1 {
                break diagnostics;
            }
            assert!(Instant::now() < deadline, "cancelled decode did not retire");
            thread::sleep(Duration::from_millis(1));
        };
        let performance = diagnostics.performance();
        assert_eq!(performance.decoded_output_bytes(), 4);
        assert_eq!(performance.cancelled_decode_executions(), 1);
        assert_eq!(performance.cancelled_decode_bytes(), 4);
        assert!(performance.cancelled_decode_time_ns() > 0);
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
        assert_eq!(
            *source.decode_order.lock().unwrap(),
            vec![0, 10, 30, 20, 0],
            "foreground admission must yield and later resume the occupied prefetch decode"
        );
    }

    #[test]
    fn live_waiter_priority_promotion_reorders_without_a_duplicate_waiter() {
        let source = TestSource::new(ResourceValidity::AllValid, GatePoint::BeforeWrite);
        let runtime = start(Arc::clone(&source), 1, 8, 8);
        runtime
            .submit(request(key(0, 2), RequestPriority::CurrentView, 1, 0))
            .unwrap();
        source.wait_entered(1);
        let guard = runtime
            .submit(request(key(10, 2), RequestPriority::Prefetch, 2, 0))
            .unwrap();
        runtime
            .submit(request(key(20, 2), RequestPriority::Playback, 3, 0))
            .unwrap();
        runtime
            .submit(request(key(30, 2), RequestPriority::LinkedView, 4, 0))
            .unwrap();

        assert!(
            runtime
                .promote_priority(guard, RequestPriority::CurrentView)
                .unwrap()
        );
        assert!(
            !runtime
                .promote_priority(guard, RequestPriority::CurrentView)
                .unwrap()
        );
        source.release();
        let _ = wait_completions(&runtime, 4);

        assert_eq!(*source.decode_order.lock().unwrap(), vec![0, 10, 30, 20]);
        assert_eq!(runtime.diagnostics().unwrap().submitted_requests(), 4);
    }

    #[test]
    fn foreground_preemption_resumes_the_same_low_priority_job_after_foreground_completes() {
        let source = TestSource::new(ResourceValidity::AllValid, GatePoint::AfterFirstByte);
        let runtime = start(Arc::clone(&source), 1, 4, 4);
        let low = runtime
            .submit(request(key(0, 4), RequestPriority::Prefetch, 1, 0))
            .unwrap();
        source.wait_first_byte();

        let foreground = runtime
            .submit(request(
                key(VISIBLE_BURST_ORIGIN, 4),
                RequestPriority::CurrentView,
                2,
                0,
            ))
            .unwrap();
        source.wait_entered(2);
        assert_eq!(
            *source.decode_order.lock().unwrap(),
            vec![0, VISIBLE_BURST_ORIGIN]
        );

        source.release();
        let completions = wait_completions(&runtime, 2);
        assert_eq!(completions[0].ticket(), foreground);
        assert_eq!(completions[1].ticket(), low);
        let RuntimeOutcome::Ready(foreground_lease) = completions[0].outcome() else {
            panic!("foreground request must complete with its decoded payload");
        };
        let RuntimeOutcome::Ready(resumed_lease) = completions[1].outcome() else {
            panic!("resumed low-priority request must complete with its decoded payload");
        };
        assert_eq!(foreground_lease.payload().value_bytes(), &[16, 17, 18, 19]);
        assert_eq!(resumed_lease.payload().value_bytes(), &[0, 1, 2, 3]);
        assert_eq!(
            *source.decode_order.lock().unwrap(),
            vec![0, VISIBLE_BURST_ORIGIN, 0]
        );

        let diagnostics = runtime.diagnostics().unwrap();
        assert_eq!(diagnostics.submitted_requests(), 2);
        assert_eq!(diagnostics.started_decodes(), 2);
        assert_eq!(diagnostics.completed_decodes(), 2);
        assert_eq!(diagnostics.ready_requests(), 2);
        assert_eq!(diagnostics.resident_resources(), 2);
        assert_eq!(diagnostics.cancelled_requests(), 0);
        assert_eq!(diagnostics.failed_requests(), 0);
        assert_eq!(source.decode_count.load(AtomicOrdering::SeqCst), 3);
    }

    #[test]
    fn capacity_blocked_priority_does_not_starve_a_decode_that_fits() {
        let source = TestSource::new(ResourceValidity::AllValid, GatePoint::BeforeWrite);
        // In-flight decode receives one eighth of this budget: 65,536 bytes.
        // The first worker holds 60,000, leaving room for the 1,000-byte
        // prefetch but not the higher-priority 6,000-byte request.
        let runtime = start_with_config(
            Arc::clone(&source),
            DatasetRuntimeConfig::new(512 * 1024, 2, 8, 8).unwrap(),
        );
        runtime
            .submit(request(key(0, 60_000), RequestPriority::CurrentView, 1, 0))
            .unwrap();
        source.wait_entered(1);
        runtime
            .submit(request(key(1, 6_000), RequestPriority::CurrentView, 2, 0))
            .unwrap();
        runtime
            .submit(request(key(2, 1_000), RequestPriority::Prefetch, 3, 0))
            .unwrap();

        source.wait_entered(2);
        assert_eq!(*source.decode_order.lock().unwrap(), vec![0, 2]);
        source.release();
        let _ = wait_completions(&runtime, 3);
        assert_eq!(*source.decode_order.lock().unwrap(), vec![0, 2, 1]);
    }

    #[test]
    fn eight_worker_visible_burst_reserves_parallel_primary_cohorts() {
        const WORKERS: usize = 8;
        let source = BurstGateSource::new();
        let runtime = start_burst_gate_source(Arc::clone(&source));
        for index in 0..WORKERS {
            runtime
                .submit(request(
                    key(index as u64 * 4, 4),
                    RequestPriority::Prefetch,
                    100 + index as u64,
                    0,
                ))
                .unwrap();
        }
        source.wait_for(
            |state| state.low_members_entered == WORKERS,
            "all workers did not enter the low-priority gate",
        );
        for index in 0..WORKERS {
            runtime
                .submit(request(
                    key(VISIBLE_BURST_ORIGIN + index as u64 * 4, 4),
                    RequestPriority::CurrentView,
                    200 + index as u64,
                    0,
                ))
                .unwrap();
        }
        source.release_low();
        source.wait_for(
            |state| state.visible_members_entered == WORKERS,
            "visible burst did not enter all worker cohorts",
        );
        {
            let state = source.gate.0.lock().unwrap();
            assert_eq!(state.visible_cohorts_entered, WORKERS);
            assert_eq!(state.peak_active_visible_cohorts, WORKERS);
            assert_eq!(state.largest_visible_cohort, 1);
        }
        assert_eq!(runtime.diagnostics().unwrap().in_flight_decodes(), WORKERS);
        source.release_visible();
        let completions = wait_completions(&runtime, WORKERS * 2);
        assert!(
            completions
                .iter()
                .all(|completion| matches!(completion.outcome(), RuntimeOutcome::Ready(_)))
        );
        let performance = runtime.diagnostics().unwrap().performance();
        assert_eq!(performance.decode_cohorts(), (WORKERS * 3) as u64);
        assert_eq!(performance.decode_cohort_members(), (WORKERS * 3) as u64);
        assert_eq!(performance.peak_decode_cohort_members(), 1);
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
        assert_eq!(lease.payload_facts().minimum(), 40.0);
        assert_eq!(lease.payload_facts().maximum(), 42.0);
        assert!(lease.payload_facts().any_valid());
        assert!(!lease.payload_facts().all_valid());
        assert_eq!(runtime.progress(ticket).unwrap(), None);
        assert_eq!(
            runtime
                .diagnostics()
                .unwrap()
                .performance()
                .progress_updates(),
            2
        );
    }

    #[test]
    fn transient_source_in_flight_capacity_miss_requeues_once_and_completes() {
        let source = TransientCapacitySource::new();
        let source_for_factory = Arc::clone(&source);
        let config = DatasetRuntimeConfig::new(1 << 20, 1, 4, 4).unwrap();
        let (runtime, _) = <dyn DatasetRuntime>::start(config, move |_| {
            let source: Arc<dyn DatasetSource> = source_for_factory;
            Ok(source)
        })
        .unwrap();
        runtime
            .submit(request(key(0, 4), RequestPriority::CurrentView, 1, 0))
            .unwrap();

        let completion = wait_completions(&runtime, 1).pop().unwrap();
        let RuntimeOutcome::Ready(lease) = completion.outcome() else {
            panic!("a transient source capacity race must be retried");
        };
        assert_eq!(lease.payload().value_bytes(), &[7, 7, 7, 7]);
        assert_eq!(source.attempts.load(AtomicOrdering::SeqCst), 2);
        assert!(runtime.poll(4).unwrap().is_empty());

        let diagnostics = runtime.diagnostics().unwrap();
        assert_eq!(diagnostics.submitted_requests(), 1);
        assert_eq!(diagnostics.started_decodes(), 1);
        assert_eq!(diagnostics.completed_decodes(), 1);
        assert_eq!(diagnostics.ready_requests(), 1);
        assert_eq!(diagnostics.failed_requests(), 0);
        assert_eq!(diagnostics.performance().decode_cohorts(), 2);
        assert_eq!(diagnostics.performance().decode_cohort_members(), 2);
    }

    #[test]
    fn cohort_virtual_admission_prevents_self_overcommit_retry_livelock() {
        const TARGETS: usize = 4;

        let layer = DatasetLayer::new(
            LogicalLayerKey::new(0),
            "intensity",
            Shape4D::new(1, 1, 1, 200_000).unwrap(),
            IntensityDType::Uint8,
            GridToWorld::identity(),
            ResourceValidity::AllValid,
        )
        .unwrap();
        let catalog = Arc::new(
            DatasetCatalog::new(
                "cohort-virtual-admission-test",
                ScientificIdentityStatus::Unverified(DatasetSourceId::new(41)),
                vec![layer],
            )
            .unwrap(),
        );
        let control = Arc::new(CohortAdmissionControl::default());
        let factory_catalog = Arc::clone(&catalog);
        let factory_control = Arc::clone(&control);
        let config = DatasetRuntimeConfig::new(1 << 20, 2, 8, 8).unwrap();
        let peer_headroom = config
            .category_cap(CpuLedgerCategory::InFlightDecode)
            .saturating_sub(1)
            .saturating_sub(40_000);
        assert!(40_000 + 30_000 <= peer_headroom);
        assert!(30_000 + 40_000 + 30_000 > peer_headroom);
        let (runtime, _) = <dyn DatasetRuntime>::start(config, move |ledger| {
            let source: Arc<dyn DatasetSource> = Arc::new(CohortAdmissionSource {
                catalog: factory_catalog,
                ledger,
                control: factory_control,
            });
            Ok(source)
        })
        .unwrap();

        runtime
            .submit(request(key(199_999, 1), RequestPriority::Analysis, 1, 0))
            .unwrap();
        control.wait_for_blocker();
        for index in 0..TARGETS {
            runtime
                .submit(request(
                    key(u64::try_from(index).unwrap() * 40_000, 40_000),
                    RequestPriority::CurrentView,
                    10 + u64::try_from(index).unwrap(),
                    0,
                ))
                .unwrap();
        }

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut target_completions = Vec::with_capacity(TARGETS);
        while target_completions.len() < TARGETS {
            target_completions.extend(runtime.poll(TARGETS - target_completions.len()).unwrap());
            if Instant::now() >= deadline {
                control.release_blocker();
                panic!("individually admissible targets made no forward progress");
            }
            if target_completions.len() < TARGETS {
                thread::sleep(Duration::from_millis(1));
            }
        }
        assert!(
            target_completions
                .iter()
                .all(|completion| matches!(completion.outcome(), RuntimeOutcome::Ready(_)))
        );
        {
            let state = control.state.lock().unwrap();
            assert_eq!(state.target_cohort_sizes, vec![1; TARGETS]);
            assert_eq!(state.target_capacity_failures, 0);
        }

        control.release_blocker();
        let blocker = wait_completions(&runtime, 1).pop().unwrap();
        assert!(matches!(blocker.outcome(), RuntimeOutcome::Ready(_)));
        let diagnostics = runtime.diagnostics().unwrap();
        assert_eq!(diagnostics.ready_requests(), (TARGETS + 1) as u64);
        assert_eq!(diagnostics.failed_requests(), 0);
        assert_eq!(
            diagnostics.performance().decode_cohorts(),
            (TARGETS + 1) as u64
        );
        assert_eq!(
            diagnostics.performance().decode_cohort_members(),
            (TARGETS + 1) as u64
        );
        assert_eq!(diagnostics.performance().peak_decode_cohort_members(), 1);
    }

    #[test]
    fn runtime_direct_spans_commit_progress_and_complete_without_a_copy_write() {
        let source = DirectSpanSource::new(DirectSpanBehavior::Complete);
        let runtime = start_direct_span_source(source);
        runtime
            .submit(request(key(0, 4), RequestPriority::CurrentView, 1, 0))
            .unwrap();
        let completion = wait_completions(&runtime, 1).pop().unwrap();
        let RuntimeOutcome::Ready(lease) = completion.outcome() else {
            panic!("direct-span payload must become ready");
        };
        assert_eq!(lease.payload().value_bytes(), &[11, 12, 13, 14]);
        let diagnostics = runtime.diagnostics().unwrap();
        assert_eq!(diagnostics.performance().progress_updates(), 2);
        assert_eq!(diagnostics.performance().decoded_output_bytes(), 4);
        assert_eq!(
            diagnostics.category_used_bytes(CpuLedgerCategory::InFlightDecode),
            0
        );
    }

    #[test]
    fn runtime_direct_span_overrun_incomplete_and_outstanding_fail_unfinished() {
        for (behavior, expected_progress, expected_written) in [
            (DirectSpanBehavior::OverrunCommit, 0, 0),
            (DirectSpanBehavior::IncompleteFinish, 1, 1),
            (DirectSpanBehavior::OutstandingFinish, 0, 0),
        ] {
            let source = DirectSpanSource::new(behavior);
            let runtime = start_direct_span_source(source);
            runtime
                .submit(request(key(0, 4), RequestPriority::CurrentView, 1, 0))
                .unwrap();
            let completion = wait_completions(&runtime, 1).pop().unwrap();
            let RuntimeOutcome::Failed(fault) = completion.outcome() else {
                panic!("invalid direct-span sequence must fail");
            };
            assert_eq!(fault.code(), RuntimeFaultCode::SinkRejected);
            let diagnostics = runtime.diagnostics().unwrap();
            assert_eq!(
                diagnostics.performance().progress_updates(),
                expected_progress
            );
            assert_eq!(
                diagnostics.performance().decoded_output_bytes(),
                expected_written
            );
            assert_eq!(
                diagnostics.category_used_bytes(CpuLedgerCategory::InFlightDecode),
                0
            );
            assert_eq!(diagnostics.in_flight_decodes(), 0);
        }
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

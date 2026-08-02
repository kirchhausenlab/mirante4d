//! Byte-accounted CPU workers with one deterministic commit owner.

use std::{
    collections::BTreeMap,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex,
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::Duration,
};

use mirante4d_dataset::{CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError};

use crate::{ImportCancellation, ImportError};

pub(crate) const CPU_WORKERS_MAX: usize = 16;
const IN_FLIGHT_TASKS_MAX: usize = 32;
const OWNER_MAINTENANCE_INTERVAL: Duration = Duration::from_millis(10);

/// One checked worker/queue policy for an import CPU phase.
///
/// `in_flight_limit` covers queued tasks, running tasks, completed results,
/// and results waiting for canonical-order commit. Every one of those values
/// owns one `task_charge_bytes` ledger lease for its complete lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OrderedWorkerPolicy {
    worker_count: usize,
    task_queue_capacity: usize,
    in_flight_limit: usize,
    task_charge_bytes: u64,
}

impl OrderedWorkerPolicy {
    #[cfg(test)]
    pub(crate) fn for_system(
        managed_capacity_bytes: u64,
        resident_bytes: u64,
        task_charge_bytes: u64,
    ) -> Result<Self, ImportError> {
        let available_parallelism = system_parallelism();
        Self::derive(
            available_parallelism,
            managed_capacity_bytes,
            resident_bytes,
            task_charge_bytes,
        )
    }

    /// Derives the ordinary byte-accounted policy while respecting a
    /// run-local CPU ceiling. The temporal importer uses this to give each
    /// active source-ingest lane one of the process's existing slots rather
    /// than adding another thread on top of all transform workers.
    pub(crate) fn for_system_with_parallelism_limit(
        parallelism_limit: usize,
        managed_capacity_bytes: u64,
        resident_bytes: u64,
        task_charge_bytes: u64,
    ) -> Result<Self, ImportError> {
        Self::derive(
            system_parallelism().min(parallelism_limit.max(1)),
            managed_capacity_bytes,
            resident_bytes,
            task_charge_bytes,
        )
    }

    fn derive(
        available_parallelism: usize,
        managed_capacity_bytes: u64,
        resident_bytes: u64,
        task_charge_bytes: u64,
    ) -> Result<Self, ImportError> {
        if task_charge_bytes == 0 {
            return Err(ImportError::InvalidRequest(
                "an import CPU task must have a positive byte charge",
            ));
        }
        let available_bytes = managed_capacity_bytes.checked_sub(resident_bytes).ok_or(
            ImportError::ManagedCapacityInsufficient {
                required_bytes: resident_bytes,
                capacity_bytes: managed_capacity_bytes,
            },
        )?;
        let byte_slots = available_bytes / task_charge_bytes;
        if byte_slots == 0 {
            return Err(ImportError::ManagedCapacityInsufficient {
                required_bytes: resident_bytes
                    .checked_add(task_charge_bytes)
                    .ok_or(ImportError::Overflow)?,
                capacity_bytes: managed_capacity_bytes,
            });
        }
        let byte_slots = usize::try_from(byte_slots).unwrap_or(usize::MAX);
        let worker_count = available_parallelism
            .clamp(1, CPU_WORKERS_MAX)
            .min(byte_slots);
        let in_flight_limit = worker_count
            .saturating_mul(2)
            .min(IN_FLIGHT_TASKS_MAX)
            .min(byte_slots)
            .max(worker_count);
        let task_queue_capacity = in_flight_limit.saturating_sub(worker_count).max(1);
        Ok(Self {
            worker_count,
            task_queue_capacity,
            in_flight_limit,
            task_charge_bytes,
        })
    }
}

pub(crate) fn system_parallelism() -> usize {
    #[cfg(test)]
    if let Some(worker_count) = TEST_WORKER_COUNT.with(std::cell::Cell::get) {
        return worker_count;
    }

    thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

#[cfg(test)]
std::thread_local! {
    static TEST_WORKER_COUNT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Runs one test on the real import path with a deterministic CPU worker input.
///
/// The override is thread-local because every worker policy is derived by the
/// import owner before that phase starts. This keeps parallel tests isolated
/// without exposing a production policy selector.
#[cfg(test)]
pub(crate) fn with_test_worker_count<R>(worker_count: usize, operation: impl FnOnce() -> R) -> R {
    assert!(worker_count > 0, "a test worker count must be positive");

    struct Restore<'a> {
        slot: &'a std::cell::Cell<Option<usize>>,
        previous: Option<usize>,
    }

    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            self.slot.set(self.previous);
        }
    }

    TEST_WORKER_COUNT.with(|slot| {
        let previous = slot.replace(Some(worker_count));
        let _restore = Restore { slot, previous };
        operation()
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OrderedWorkerDiagnostics {
    pub(crate) worker_count: usize,
    pub(crate) in_flight_limit: usize,
    pub(crate) peak_in_flight: usize,
    pub(crate) peak_reorder_results: usize,
    pub(crate) committed_tasks: u64,
}

struct TaskEnvelope<T> {
    ordinal: u64,
    task: T,
    lease: Box<dyn CpuByteLease>,
}

struct ResultEnvelope<R> {
    ordinal: u64,
    result: Result<R, ImportError>,
    _lease: Box<dyn CpuByteLease>,
}

/// Runs lazy task specifications concurrently and commits results in their
/// original order on the calling thread.
///
/// The caller must include all input, output, scratch, and codec allocations
/// in the policy's per-task charge. The lease is acquired before `make_task`
/// materializes the input and is retained until `commit_result` returns (or
/// the result is discarded after an error). No worker can mutate checkpoint
/// or package state.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_ordered<O, S, T, R, I, M, W, K, C>(
    policy: OrderedWorkerPolicy,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    specifications: I,
    owner: &mut O,
    mut make_task: M,
    work: W,
    mut maintain_owner: K,
    mut commit_result: C,
) -> Result<OrderedWorkerDiagnostics, ImportError>
where
    S: Send,
    T: Send,
    R: Send,
    I: IntoIterator<Item = S>,
    M: FnMut(&mut O, S) -> Result<T, ImportError>,
    W: Fn(T, &ImportCancellation) -> Result<R, ImportError> + Sync,
    K: FnMut(&mut O) -> Result<(), ImportError>,
    C: FnMut(&mut O, R) -> Result<(), ImportError>,
{
    let mut specifications = specifications.into_iter();
    let mut diagnostics = OrderedWorkerDiagnostics {
        worker_count: policy.worker_count,
        in_flight_limit: policy.in_flight_limit,
        ..OrderedWorkerDiagnostics::default()
    };

    thread::scope(|scope| {
        let (task_sender, task_receiver) =
            mpsc::sync_channel::<TaskEnvelope<T>>(policy.task_queue_capacity);
        let task_receiver = Arc::new(Mutex::new(task_receiver));
        let (result_sender, result_receiver) =
            mpsc::sync_channel::<ResultEnvelope<R>>(policy.in_flight_limit);

        let mut workers = Vec::with_capacity(policy.worker_count);
        for ordinal in 0..policy.worker_count {
            let task_receiver = Arc::clone(&task_receiver);
            let result_sender = result_sender.clone();
            let work = &work;
            workers.push(
                thread::Builder::new()
                    .name(format!("mirante4d-import-cpu-{ordinal}"))
                    .spawn_scoped(scope, move || {
                        loop {
                            let task = {
                                let receiver = task_receiver
                                    .lock()
                                    .unwrap_or_else(|poison| poison.into_inner());
                                receiver.recv()
                            };
                            let Ok(task) = task else {
                                break;
                            };
                            let result = if cancellation.is_cancelled() {
                                Err(ImportError::Cancelled)
                            } else {
                                match catch_unwind(AssertUnwindSafe(|| {
                                    work(task.task, cancellation)
                                })) {
                                    Ok(Ok(_result)) if cancellation.is_cancelled() => {
                                        Err(ImportError::Cancelled)
                                    }
                                    Ok(result) => result,
                                    Err(_) => Err(ImportError::InvalidRequest(
                                        "an import CPU worker panicked",
                                    )),
                                }
                            };
                            if result_sender
                                .send(ResultEnvelope {
                                    ordinal: task.ordinal,
                                    result,
                                    _lease: task.lease,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    })
                    .map_err(|_| {
                        ImportError::InvalidRequest("failed to start an import CPU worker")
                    })?,
            );
        }
        drop(result_sender);

        let mut next_task_ordinal = 0_u64;
        let mut next_commit_ordinal = 0_u64;
        let mut outstanding = 0_usize;
        let mut awaiting_results = 0_usize;
        let mut exhausted = false;
        let mut pending_specification = None;
        let mut first_error = None;
        let mut reorder = BTreeMap::new();

        loop {
            if first_error.is_none()
                && let Err(error) = maintain_owner(owner)
            {
                first_error = Some(error);
            }
            let mut capacity_blocked = false;
            while first_error.is_none() && !exhausted && outstanding < policy.in_flight_limit {
                if cancellation.is_cancelled() {
                    first_error = Some(ImportError::Cancelled);
                    break;
                }
                let Some(specification) = pending_specification
                    .take()
                    .or_else(|| specifications.next())
                else {
                    exhausted = true;
                    break;
                };
                let lease = match ledger.try_acquire(
                    CpuLedgerCategory::ImportWorkingSet,
                    policy.task_charge_bytes,
                ) {
                    Ok(lease) => lease,
                    Err(CpuLedgerError::CapacityExceeded { .. }) => {
                        // Ordinary process contention narrows the contiguous
                        // sliding window. Retain the earliest unadmitted task,
                        // drain useful work, and retry after capacity changes.
                        pending_specification = Some(specification);
                        capacity_blocked = true;
                        break;
                    }
                    Err(CpuLedgerError::ShuttingDown) => {
                        first_error = Some(ImportError::Cancelled);
                        break;
                    }
                    Err(error) => {
                        first_error = Some(error.into());
                        break;
                    }
                };
                if lease.category() != CpuLedgerCategory::ImportWorkingSet
                    || lease.reserved_bytes() != policy.task_charge_bytes
                {
                    first_error = Some(ImportError::InvalidRequest(
                        "the CPU byte ledger returned a mismatched import lease",
                    ));
                    break;
                }
                let task = match make_task(owner, specification) {
                    Ok(task) => task,
                    Err(error) => {
                        first_error = Some(error);
                        break;
                    }
                };
                let envelope = TaskEnvelope {
                    ordinal: next_task_ordinal,
                    task,
                    lease,
                };
                if task_sender.send(envelope).is_err() {
                    first_error = Some(ImportError::InvalidRequest(
                        "all import CPU workers stopped before accepting bounded work",
                    ));
                    break;
                }
                next_task_ordinal = next_task_ordinal
                    .checked_add(1)
                    .ok_or(ImportError::Overflow)?;
                outstanding += 1;
                awaiting_results += 1;
                diagnostics.peak_in_flight = diagnostics.peak_in_flight.max(outstanding);
            }

            if awaiting_results == 0 {
                if capacity_blocked && first_error.is_none() && !cancellation.is_cancelled() {
                    let observed = ledger.capacity_epoch();
                    maintain_owner(owner)?;
                    // The ledger boundary intentionally stays scheduler-free.
                    // A short bounded park keeps cancellation responsive while
                    // another subsystem owns the bytes that must be released.
                    if ledger.capacity_epoch() == observed {
                        thread::sleep(OWNER_MAINTENANCE_INTERVAL);
                    }
                    continue;
                }
                break;
            }
            let result = match result_receiver.recv_timeout(OWNER_MAINTENANCE_INTERVAL) {
                Ok(result) => result,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    first_error.get_or_insert(ImportError::InvalidRequest(
                        "all import CPU workers stopped before returning bounded work",
                    ));
                    break;
                }
            };
            awaiting_results -= 1;
            if first_error.is_some() {
                outstanding -= 1;
                continue;
            }
            match result.result {
                Ok(value) => {
                    if reorder.contains_key(&result.ordinal) {
                        first_error = Some(ImportError::InvalidRequest(
                            "an import CPU worker returned a duplicate task result",
                        ));
                        drop(result._lease);
                        outstanding -= 1;
                        outstanding -= reorder.len();
                        reorder.clear();
                        continue;
                    }
                    reorder.insert(result.ordinal, (value, result._lease));
                    diagnostics.peak_reorder_results =
                        diagnostics.peak_reorder_results.max(reorder.len());
                    while let Some((value, lease)) = reorder.remove(&next_commit_ordinal) {
                        if cancellation.is_cancelled() {
                            first_error = Some(ImportError::Cancelled);
                            drop(lease);
                            outstanding -= 1;
                            outstanding -= reorder.len();
                            reorder.clear();
                            break;
                        }
                        if let Err(error) = commit_result(owner, value) {
                            first_error = Some(error);
                            drop(lease);
                            outstanding -= 1;
                            outstanding -= reorder.len();
                            reorder.clear();
                            break;
                        }
                        drop(lease);
                        outstanding -= 1;
                        diagnostics.committed_tasks = diagnostics
                            .committed_tasks
                            .checked_add(1)
                            .ok_or(ImportError::Overflow)?;
                        next_commit_ordinal = next_commit_ordinal
                            .checked_add(1)
                            .ok_or(ImportError::Overflow)?;
                    }
                }
                Err(error) => {
                    first_error = Some(error);
                    outstanding -= 1;
                    outstanding -= reorder.len();
                    reorder.clear();
                }
            }
        }

        drop(task_sender);
        for worker in workers {
            if worker.join().is_err() && first_error.is_none() {
                first_error = Some(ImportError::InvalidRequest(
                    "an import CPU worker panicked outside its task boundary",
                ));
            }
        }
        if let Some(error) = first_error {
            Err(error)
        } else if !reorder.is_empty() || next_commit_ordinal != next_task_ordinal {
            Err(ImportError::InvalidRequest(
                "import CPU workers did not return a complete ordered result prefix",
            ))
        } else {
            Ok(diagnostics)
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    use mirante4d_dataset::{CpuByteLease, CpuLedgerError};

    use super::*;

    #[derive(Default)]
    struct LedgerState {
        used: AtomicU64,
        peak: AtomicU64,
    }

    struct TestLease {
        state: Arc<LedgerState>,
        bytes: u64,
    }

    impl CpuByteLease for TestLease {
        fn category(&self) -> CpuLedgerCategory {
            CpuLedgerCategory::ImportWorkingSet
        }

        fn reserved_bytes(&self) -> u64 {
            self.bytes
        }
    }

    impl Drop for TestLease {
        fn drop(&mut self) {
            self.state.used.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }

    struct TestLedger {
        state: Arc<LedgerState>,
        budget: u64,
    }

    impl CpuByteLedger for TestLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            assert_eq!(category, CpuLedgerCategory::ImportWorkingSet);
            let mut used = self.state.used.load(Ordering::Acquire);
            loop {
                let Some(next) = used.checked_add(bytes) else {
                    return Err(CpuLedgerError::CapacityExceeded {
                        category,
                        requested_bytes: bytes,
                        available_bytes: self.budget.saturating_sub(used),
                    });
                };
                if next > self.budget {
                    return Err(CpuLedgerError::CapacityExceeded {
                        category,
                        requested_bytes: bytes,
                        available_bytes: self.budget.saturating_sub(used),
                    });
                }
                match self.state.used.compare_exchange_weak(
                    used,
                    next,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        self.state.peak.fetch_max(next, Ordering::AcqRel);
                        return Ok(Box::new(TestLease {
                            state: Arc::clone(&self.state),
                            bytes,
                        }));
                    }
                    Err(actual) => used = actual,
                }
            }
        }
    }

    fn ledger(budget: u64) -> (TestLedger, Arc<LedgerState>) {
        let state = Arc::new(LedgerState::default());
        (
            TestLedger {
                state: Arc::clone(&state),
                budget,
            },
            state,
        )
    }

    struct TemporarilyRefusingLedger {
        inner: TestLedger,
        refusals_remaining: AtomicU64,
    }

    impl CpuByteLedger for TemporarilyRefusingLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            if self
                .refusals_remaining
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(CpuLedgerError::CapacityExceeded {
                    category,
                    requested_bytes: bytes,
                    available_bytes: 0,
                });
            }
            self.inner.try_acquire(category, bytes)
        }

        fn capacity_bytes(&self) -> u64 {
            self.inner.budget
        }
    }

    #[test]
    fn policy_is_capped_by_cores_bytes_and_fixed_limits() {
        let memory_limited = OrderedWorkerPolicy::derive(8, 256, 16, 32).unwrap();
        assert_eq!(memory_limited.worker_count, 7);
        assert_eq!(memory_limited.in_flight_limit, 7);
        assert_eq!(memory_limited.task_queue_capacity, 1);

        let fixed_caps = OrderedWorkerPolicy::derive(64, 1_024, 0, 1).unwrap();
        assert_eq!(fixed_caps.worker_count, CPU_WORKERS_MAX);
        assert_eq!(fixed_caps.in_flight_limit, IN_FLIGHT_TASKS_MAX);
        assert_eq!(fixed_caps.task_queue_capacity, 16);

        let two_slots = OrderedWorkerPolicy::derive(8, 256, 128, 64).unwrap();
        assert_eq!(two_slots.worker_count, 2);
        assert_eq!(two_slots.in_flight_limit, 2);

        let normalized_core_count = OrderedWorkerPolicy::derive(0, 8, 0, 4).unwrap();
        assert_eq!(normalized_core_count.worker_count, 1);
        assert_eq!(normalized_core_count.in_flight_limit, 2);
    }

    #[test]
    fn test_worker_override_drives_real_system_policy_and_restores_nested_scopes() {
        with_test_worker_count(1, || {
            let serial = OrderedWorkerPolicy::for_system(1_024, 0, 1).unwrap();
            assert_eq!(serial.worker_count, 1);

            with_test_worker_count(4, || {
                let parallel = OrderedWorkerPolicy::for_system(1_024, 0, 1).unwrap();
                assert_eq!(parallel.worker_count, 4);
            });

            let restored = OrderedWorkerPolicy::for_system(1_024, 0, 1).unwrap();
            assert_eq!(restored.worker_count, 1);
        });
    }

    #[test]
    fn run_local_parallelism_limit_reserves_an_existing_slot() {
        with_test_worker_count(8, || {
            let policy =
                OrderedWorkerPolicy::for_system_with_parallelism_limit(7, 1_024, 0, 1).unwrap();
            assert_eq!(policy.worker_count, 7);
        });
    }

    #[test]
    fn policy_rejects_a_task_that_cannot_fit_beside_resident_state() {
        assert!(matches!(
            OrderedWorkerPolicy::derive(8, 255, 224, 32),
            Err(ImportError::ManagedCapacityInsufficient {
                required_bytes: 256,
                capacity_bytes: 255
            })
        ));
    }

    fn ordered_output(worker_count: usize) -> (Vec<u64>, OrderedWorkerDiagnostics, u64) {
        let policy = OrderedWorkerPolicy::derive(worker_count, 16 * 1_024, 0, 1_024).unwrap();
        let (ledger, state) = ledger(16 * 1_024);
        let cancellation = ImportCancellation::new();
        let mut output = Vec::new();
        let diagnostics = run_ordered(
            policy,
            &ledger,
            &cancellation,
            0_u64..64,
            &mut output,
            |_, value| Ok(value),
            |value, _| {
                for _ in 0..(value % 5) {
                    thread::yield_now();
                }
                Ok(value * value)
            },
            |_| Ok(()),
            |output, value| {
                output.push(value);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(state.used.load(Ordering::Acquire), 0);
        let peak = state.peak.load(Ordering::Acquire);
        (output, diagnostics, peak)
    }

    #[test]
    fn ordered_commits_are_deterministic_and_byte_bounded_across_worker_counts() {
        let (serial, serial_diagnostics, serial_peak) = ordered_output(1);
        let (parallel, parallel_diagnostics, parallel_peak) = ordered_output(8);
        let expected = (0_u64..64).map(|value| value * value).collect::<Vec<_>>();
        assert_eq!(serial, expected);
        assert_eq!(parallel, expected);
        assert_eq!(serial, parallel);
        assert_eq!(serial_diagnostics.committed_tasks, 64);
        assert_eq!(parallel_diagnostics.committed_tasks, 64);
        assert!(
            serial_diagnostics.peak_in_flight <= serial_diagnostics.in_flight_limit
                && serial_diagnostics.peak_reorder_results <= serial_diagnostics.in_flight_limit
        );
        assert!(
            parallel_diagnostics.peak_in_flight <= parallel_diagnostics.in_flight_limit
                && parallel_diagnostics.peak_reorder_results
                    <= parallel_diagnostics.in_flight_limit
        );
        assert!(serial_peak <= u64::try_from(serial_diagnostics.in_flight_limit).unwrap() * 1_024);
        assert!(
            parallel_peak <= u64::try_from(parallel_diagnostics.in_flight_limit).unwrap() * 1_024
        );
    }

    #[test]
    fn cancellation_suppresses_commit_and_releases_every_lease() {
        let policy = OrderedWorkerPolicy::derive(4, 8 * 1_024, 0, 1_024).unwrap();
        let (ledger, state) = ledger(8 * 1_024);
        let cancellation = ImportCancellation::new();
        cancellation.cancel();
        let committed = AtomicU64::new(0);
        let mut owner = ();
        assert!(matches!(
            run_ordered(
                policy,
                &ledger,
                &cancellation,
                0_u64..8,
                &mut owner,
                |_, value| Ok(value),
                |value, _| Ok(value),
                |_| Ok(()),
                |_, _| {
                    committed.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
            ),
            Err(ImportError::Cancelled)
        ));
        assert_eq!(committed.load(Ordering::Relaxed), 0);
        assert_eq!(state.used.load(Ordering::Acquire), 0);
    }

    #[test]
    fn owner_maintenance_runs_while_a_worker_is_slow() {
        let policy = OrderedWorkerPolicy::derive(1, 2 * 1_024, 0, 1_024).unwrap();
        let (ledger, _state) = ledger(2 * 1_024);
        let cancellation = ImportCancellation::new();
        let mut maintenance_ticks = 0_u64;
        run_ordered(
            policy,
            &ledger,
            &cancellation,
            0_u64..1,
            &mut maintenance_ticks,
            |_, value| Ok(value),
            |value, _| {
                thread::sleep(Duration::from_millis(35));
                Ok(value)
            },
            |ticks| {
                *ticks += 1;
                Ok(())
            },
            |_, _| Ok(()),
        )
        .unwrap();
        assert!(maintenance_ticks >= 2);
    }

    #[test]
    fn temporary_capacity_refusal_drains_retries_and_never_becomes_a_failure() {
        let policy = OrderedWorkerPolicy::derive(2, 4 * 1_024, 0, 1_024).unwrap();
        let (inner, state) = ledger(4 * 1_024);
        let ledger = TemporarilyRefusingLedger {
            inner,
            refusals_remaining: AtomicU64::new(3),
        };
        let cancellation = ImportCancellation::new();
        let mut output = Vec::new();
        let diagnostics = run_ordered(
            policy,
            &ledger,
            &cancellation,
            0_u64..6,
            &mut output,
            |_, value| Ok(value),
            |value, _| Ok(value + 10),
            |_| Ok(()),
            |output, value| {
                output.push(value);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(output, (10_u64..16).collect::<Vec<_>>());
        assert_eq!(diagnostics.committed_tasks, 6);
        assert_eq!(state.used.load(Ordering::Acquire), 0);
    }
}

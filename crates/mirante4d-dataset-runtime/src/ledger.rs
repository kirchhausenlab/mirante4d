use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};

use mirante4d_dataset::{CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError};

use crate::{CPU_LEDGER_CATEGORIES, DatasetRuntimeConfig, category_index};

#[derive(Debug, Default)]
pub(super) struct ChangeSignal {
    generation: Mutex<u64>,
    changed: Condvar,
}

impl ChangeSignal {
    pub(super) fn generation(&self) -> u64 {
        *self
            .generation
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub(super) fn notify_one(&self) {
        let mut generation = self
            .generation
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *generation = generation.wrapping_add(1);
        self.changed.notify_one();
    }

    pub(super) fn notify_all(&self) {
        let mut generation = self
            .generation
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *generation = generation.wrapping_add(1);
        self.changed.notify_all();
    }

    pub(super) fn wait_for_change_after(&self, observed: u64) {
        let mut generation = self
            .generation
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        while *generation == observed {
            generation = self
                .changed
                .wait(generation)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }
}

#[derive(Debug)]
pub(super) struct LedgerCore {
    config: DatasetRuntimeConfig,
    usage: Mutex<LedgerUsage>,
    changed: Condvar,
    scheduler_changed: Arc<ChangeSignal>,
    accepting: AtomicBool,
    capacity_epoch: AtomicU64,
    foreground_reserve: AtomicU64,
    next_progress_reservation_id: AtomicU64,
}

#[derive(Debug)]
struct LedgerUsage {
    by_category: [u64; CPU_LEDGER_CATEGORIES.len()],
    foreground_bytes: u64,
    background_bytes: u64,
    progress: Option<ProgressUsage>,
}

#[derive(Debug, Clone, Copy)]
struct ProgressUsage {
    id: u64,
    reserved_bytes: u64,
    used_bytes: u64,
    closing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LedgerClass {
    Foreground,
    Background,
    Progress(u64),
}

impl LedgerCore {
    pub(super) fn new(
        config: DatasetRuntimeConfig,
        scheduler_changed: Arc<ChangeSignal>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            usage: Mutex::new(LedgerUsage {
                by_category: [0; CPU_LEDGER_CATEGORIES.len()],
                foreground_bytes: 0,
                background_bytes: 0,
                progress: None,
            }),
            changed: Condvar::new(),
            scheduler_changed,
            accepting: AtomicBool::new(true),
            capacity_epoch: AtomicU64::new(0),
            foreground_reserve: AtomicU64::new(0),
            next_progress_reservation_id: AtomicU64::new(1),
        })
    }

    pub(super) const fn config(&self) -> DatasetRuntimeConfig {
        self.config
    }

    pub(super) fn acquire(
        self: &Arc<Self>,
        category: CpuLedgerCategory,
        bytes: u64,
    ) -> Result<LedgerCharge, CpuLedgerError> {
        self.acquire_as(LedgerClass::Foreground, category, bytes)
    }

    pub(super) fn acquire_as(
        self: &Arc<Self>,
        class: LedgerClass,
        category: CpuLedgerCategory,
        bytes: u64,
    ) -> Result<LedgerCharge, CpuLedgerError> {
        if bytes == 0 {
            return Err(CpuLedgerError::ZeroByteReservation);
        }
        if !self.accepting.load(Ordering::Acquire) {
            return Err(CpuLedgerError::ShuttingDown);
        }

        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !self.accepting.load(Ordering::Acquire) {
            return Err(CpuLedgerError::ShuttingDown);
        }
        let slot = category_index(category);
        let category_used = usage.by_category[slot];
        let total_used = usage.by_category.iter().try_fold(0_u64, |sum, value| {
            sum.checked_add(*value)
                .ok_or(CpuLedgerError::CapacityExceeded {
                    category,
                    requested_bytes: bytes,
                    available_bytes: 0,
                })
        })?;
        let category_available = self
            .config
            .category_cap(category)
            .saturating_sub(category_used);
        let total_available = self.config.total_cpu_bytes().saturating_sub(total_used);
        let class_available = self.class_available(&usage, class)?;
        let available_bytes = category_available.min(total_available).min(class_available);
        if bytes > available_bytes {
            return Err(CpuLedgerError::CapacityExceeded {
                category,
                requested_bytes: bytes,
                available_bytes,
            });
        }
        usage.by_category[slot] = category_used + bytes;
        match class {
            LedgerClass::Foreground => {
                usage.foreground_bytes = usage.foreground_bytes.checked_add(bytes).ok_or(
                    CpuLedgerError::CapacityExceeded {
                        category,
                        requested_bytes: bytes,
                        available_bytes: 0,
                    },
                )?;
            }
            LedgerClass::Background => {
                usage.background_bytes = usage.background_bytes.checked_add(bytes).ok_or(
                    CpuLedgerError::CapacityExceeded {
                        category,
                        requested_bytes: bytes,
                        available_bytes: 0,
                    },
                )?;
            }
            LedgerClass::Progress(id) => {
                let progress = usage
                    .progress
                    .as_mut()
                    .filter(|progress| progress.id == id && !progress.closing);
                let Some(progress) = progress else {
                    usage.by_category[slot] = category_used;
                    return Err(CpuLedgerError::ShuttingDown);
                };
                progress.used_bytes = progress.used_bytes.checked_add(bytes).ok_or(
                    CpuLedgerError::CapacityExceeded {
                        category,
                        requested_bytes: bytes,
                        available_bytes: 0,
                    },
                )?;
            }
        }
        Ok(LedgerCharge {
            core: Arc::clone(self),
            category: AtomicU8::new(category_index(category) as u8),
            bytes,
            class,
        })
    }

    fn class_available(
        &self,
        usage: &LedgerUsage,
        class: LedgerClass,
    ) -> Result<u64, CpuLedgerError> {
        let background_capacity = self
            .config
            .total_cpu_bytes()
            .saturating_sub(self.foreground_reserve.load(Ordering::Acquire));
        match class {
            LedgerClass::Foreground => {
                let unused_progress_lane = usage
                    .progress
                    .map(|progress| progress.reserved_bytes.saturating_sub(progress.used_bytes))
                    .unwrap_or(0);
                Ok(self
                    .config
                    .total_cpu_bytes()
                    .saturating_sub(usage.by_category.iter().copied().sum::<u64>())
                    .saturating_sub(unused_progress_lane))
            }
            LedgerClass::Background => {
                let protected_progress = usage
                    .progress
                    .map(|progress| progress.reserved_bytes.max(progress.used_bytes))
                    .unwrap_or(0);
                Ok(background_capacity
                    .saturating_sub(usage.background_bytes)
                    .saturating_sub(protected_progress))
            }
            LedgerClass::Progress(id) => {
                let Some(progress) = usage
                    .progress
                    .filter(|progress| progress.id == id && !progress.closing)
                else {
                    return Err(CpuLedgerError::ShuttingDown);
                };
                Ok(background_capacity
                    .saturating_sub(usage.background_bytes)
                    .saturating_sub(progress.used_bytes))
            }
        }
    }

    pub(super) fn snapshot(&self) -> [u64; CPU_LEDGER_CATEGORIES.len()] {
        self.usage
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .by_category
    }

    pub(super) fn available(&self, category: CpuLedgerCategory) -> u64 {
        let usage = self
            .usage
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let category_available = self
            .config
            .category_cap(category)
            .saturating_sub(usage.by_category[category_index(category)]);
        let total_used = usage.by_category.iter().copied().sum::<u64>();
        category_available.min(self.config.total_cpu_bytes().saturating_sub(total_used))
    }

    pub(super) fn foreground_reserve(&self) -> u64 {
        self.foreground_reserve.load(Ordering::Acquire)
    }

    pub(super) fn set_foreground_reserve(&self, bytes: u64) -> Result<(), CpuLedgerError> {
        if bytes > self.config.total_cpu_bytes() {
            return Err(CpuLedgerError::CapacityExceeded {
                category: CpuLedgerCategory::QueuesAndResults,
                requested_bytes: bytes,
                available_bytes: self.config.total_cpu_bytes(),
            });
        }
        let _usage = self
            .usage
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let previous = self.foreground_reserve.swap(bytes, Ordering::AcqRel);
        if previous != bytes {
            self.note_capacity_may_have_increased();
            self.changed.notify_all();
            self.scheduler_changed.notify_all();
        }
        Ok(())
    }

    pub(super) fn reserve_progress(
        self: &Arc<Self>,
        bytes: u64,
    ) -> Result<ProgressReservation, CpuLedgerError> {
        if bytes == 0 {
            return Err(CpuLedgerError::ZeroByteReservation);
        }
        if !self.accepting.load(Ordering::Acquire) {
            return Err(CpuLedgerError::ShuttingDown);
        }
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let background_capacity = self
            .config
            .total_cpu_bytes()
            .saturating_sub(self.foreground_reserve.load(Ordering::Acquire));
        let total_used = usage.by_category.iter().copied().sum::<u64>();
        let available = background_capacity
            .saturating_sub(usage.background_bytes)
            .min(self.config.total_cpu_bytes().saturating_sub(total_used));
        if usage.progress.is_some() || bytes > available {
            return Err(CpuLedgerError::CapacityExceeded {
                category: CpuLedgerCategory::ImportWorkingSet,
                requested_bytes: bytes,
                available_bytes: if usage.progress.is_some() {
                    0
                } else {
                    available
                },
            });
        }
        let id = self
            .next_progress_reservation_id
            .fetch_add(1, Ordering::Relaxed);
        if id == 0 {
            return Err(CpuLedgerError::ShuttingDown);
        }
        usage.progress = Some(ProgressUsage {
            id,
            reserved_bytes: bytes,
            used_bytes: 0,
            closing: false,
        });
        Ok(ProgressReservation {
            core: Arc::clone(self),
            id,
            bytes,
        })
    }

    fn release_progress_reservation(&self, id: u64) {
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(progress) = usage.progress.as_mut().filter(|progress| progress.id == id) else {
            return;
        };
        if progress.used_bytes == 0 {
            usage.progress = None;
        } else {
            progress.closing = true;
        }
        self.note_capacity_may_have_increased();
        self.changed.notify_all();
        self.scheduler_changed.notify_all();
    }

    pub(super) fn capacity_epoch(&self) -> u64 {
        self.capacity_epoch.load(Ordering::Acquire)
    }

    fn note_capacity_may_have_increased(&self) {
        let _ = self
            .capacity_epoch
            .fetch_update(Ordering::Release, Ordering::Relaxed, |epoch| {
                Some(epoch.saturating_add(1))
            });
    }

    pub(super) fn stop_accepting(&self) {
        // `used` is the predicate mutex for capacity waiters. Holding it while
        // changing acceptance prevents shutdown from landing between a waiter's
        // predicate check and its condition-variable wait.
        let _usage = self
            .usage
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.accepting.store(false, Ordering::Release);
        self.changed.notify_all();
        self.scheduler_changed.notify_all();
    }

    pub(super) fn wait_for_change_after(&self, observed_epoch: u64) {
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        while self.accepting.load(Ordering::Acquire)
            && self.capacity_epoch.load(Ordering::Acquire) == observed_epoch
        {
            usage = self
                .changed
                .wait(usage)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }

    fn reclassify(
        &self,
        old: CpuLedgerCategory,
        new: CpuLedgerCategory,
        bytes: u64,
    ) -> Result<(), CpuLedgerError> {
        if old == new {
            return Ok(());
        }
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let old_slot = category_index(old);
        let new_slot = category_index(new);
        let new_available = self
            .config
            .category_cap(new)
            .saturating_sub(usage.by_category[new_slot]);
        if bytes > new_available {
            return Err(CpuLedgerError::CapacityExceeded {
                category: new,
                requested_bytes: bytes,
                available_bytes: new_available,
            });
        }
        usage.by_category[old_slot] = usage.by_category[old_slot]
            .checked_sub(bytes)
            .expect("a live charge is present in its recorded category");
        usage.by_category[new_slot] = usage.by_category[new_slot]
            .checked_add(bytes)
            .expect("a checked category transfer cannot overflow");
        self.note_capacity_may_have_increased();
        self.changed.notify_all();
        self.scheduler_changed.notify_all();
        Ok(())
    }
}

pub(super) struct LedgerHandle {
    pub(super) core: Arc<LedgerCore>,
    pub(super) class: LedgerClass,
}

impl CpuByteLedger for LedgerHandle {
    fn try_acquire(
        &self,
        category: CpuLedgerCategory,
        bytes: u64,
    ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
        Ok(Box::new(self.core.acquire_as(self.class, category, bytes)?))
    }

    fn capacity_epoch(&self) -> u64 {
        self.core.capacity_epoch()
    }

    fn capacity_bytes(&self) -> u64 {
        match self.class {
            LedgerClass::Foreground => self.core.config.total_cpu_bytes(),
            LedgerClass::Background | LedgerClass::Progress(_) => self
                .core
                .config
                .total_cpu_bytes()
                .saturating_sub(self.core.foreground_reserve()),
        }
    }
}

pub(super) struct ProgressReservation {
    core: Arc<LedgerCore>,
    id: u64,
    bytes: u64,
}

impl ProgressReservation {
    pub(super) fn ledger(&self) -> LedgerHandle {
        LedgerHandle {
            core: Arc::clone(&self.core),
            class: LedgerClass::Progress(self.id),
        }
    }

    pub(super) const fn reserved_bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for ProgressReservation {
    fn drop(&mut self) {
        self.core.release_progress_reservation(self.id);
    }
}

#[derive(Debug)]
pub(super) struct LedgerCharge {
    core: Arc<LedgerCore>,
    category: AtomicU8,
    bytes: u64,
    class: LedgerClass,
}

impl LedgerCharge {
    pub(super) fn category(&self) -> CpuLedgerCategory {
        CPU_LEDGER_CATEGORIES[usize::from(self.category.load(Ordering::Acquire))]
    }

    pub(super) const fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(super) fn reclassify(&self, category: CpuLedgerCategory) -> Result<(), CpuLedgerError> {
        let old = self.category();
        self.core.reclassify(old, category, self.bytes)?;
        self.category
            .store(category_index(category) as u8, Ordering::Release);
        Ok(())
    }
}

impl CpuByteLease for LedgerCharge {
    fn category(&self) -> CpuLedgerCategory {
        self.category()
    }

    fn reserved_bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for LedgerCharge {
    fn drop(&mut self) {
        let category = self.category();
        let mut usage = self
            .core
            .usage
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let slot = category_index(category);
        usage.by_category[slot] = usage.by_category[slot]
            .checked_sub(self.bytes)
            .expect("a live ledger charge releases exactly once");
        match self.class {
            LedgerClass::Foreground => {
                usage.foreground_bytes = usage
                    .foreground_bytes
                    .checked_sub(self.bytes)
                    .expect("foreground accounting matches category accounting");
            }
            LedgerClass::Background => {
                usage.background_bytes = usage
                    .background_bytes
                    .checked_sub(self.bytes)
                    .expect("background accounting matches category accounting");
            }
            LedgerClass::Progress(id) => {
                let progress = usage
                    .progress
                    .as_mut()
                    .filter(|progress| progress.id == id)
                    .expect("a progress charge cannot outlive its reservation state");
                progress.used_bytes = progress
                    .used_bytes
                    .checked_sub(self.bytes)
                    .expect("progress accounting matches category accounting");
                if progress.closing && progress.used_bytes == 0 {
                    usage.progress = None;
                }
            }
        }
        self.core.note_capacity_may_have_increased();
        self.core.changed.notify_all();
        self.core.scheduler_changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread, time::Duration};

    #[test]
    fn capacity_epoch_advances_only_when_availability_may_increase() {
        let config = DatasetRuntimeConfig::new(4_000, 1, 2, 2).unwrap();
        let ledger = LedgerCore::new(config, Arc::new(ChangeSignal::default()));
        assert_eq!(ledger.capacity_epoch(), 0);

        let retained = ledger
            .acquire(CpuLedgerCategory::InFlightDecode, 250)
            .unwrap();
        let unrelated = ledger
            .acquire(CpuLedgerCategory::MetadataAndIndexes, 250)
            .unwrap();
        assert_eq!(
            ledger.capacity_epoch(),
            0,
            "successful acquisitions reduce availability and must not reopen a failure latch"
        );

        drop(unrelated);
        assert_eq!(
            ledger.capacity_epoch(),
            1,
            "releasing bytes can make a rejected reservation runnable"
        );
        retained
            .reclassify(CpuLedgerCategory::DecodedResidency)
            .unwrap();
        assert_eq!(
            ledger.capacity_epoch(),
            2,
            "reclassification can increase availability in the old category"
        );
        drop(retained);
        assert_eq!(ledger.capacity_epoch(), 3);
    }

    #[test]
    fn ledger_reclassifies_without_total_overcommit_and_releases() {
        let config = DatasetRuntimeConfig::new(4_000, 1, 2, 2).unwrap();
        let ledger = LedgerCore::new(config, Arc::new(ChangeSignal::default()));
        let charge = ledger
            .acquire(CpuLedgerCategory::InFlightDecode, 500)
            .unwrap();
        assert_eq!(
            ledger.snapshot()[category_index(CpuLedgerCategory::InFlightDecode)],
            500
        );
        charge
            .reclassify(CpuLedgerCategory::DecodedResidency)
            .unwrap();
        let used = ledger.snapshot();
        assert_eq!(used[category_index(CpuLedgerCategory::InFlightDecode)], 0);
        assert_eq!(
            used[category_index(CpuLedgerCategory::DecodedResidency)],
            500
        );
        assert_eq!(used.iter().sum::<u64>(), 500);

        let error = ledger
            .acquire(CpuLedgerCategory::DecodedResidency, 3_501)
            .unwrap_err();
        assert!(matches!(
            error,
            CpuLedgerError::CapacityExceeded {
                category: CpuLedgerCategory::DecodedResidency,
                requested_bytes: 3_501,
                available_bytes: 3_500,
            }
        ));
        drop(charge);
        assert_eq!(ledger.snapshot(), [0; CPU_LEDGER_CATEGORIES.len()]);
    }

    #[test]
    fn capacity_wait_observes_release_in_the_former_check_wait_window() {
        let config = DatasetRuntimeConfig::new(4_000, 1, 2, 2).unwrap();
        let ledger = LedgerCore::new(config, Arc::new(ChangeSignal::default()));
        let available = ledger.available(CpuLedgerCategory::InFlightDecode);
        let blocking = ledger
            .acquire(CpuLedgerCategory::InFlightDecode, available)
            .unwrap();

        // This is the exact former race: observe, fail admission, then release
        // capacity before the worker has entered its condition-variable wait.
        let observed_epoch = ledger.capacity_epoch();
        assert!(matches!(
            ledger.acquire(CpuLedgerCategory::InFlightDecode, 1),
            Err(CpuLedgerError::CapacityExceeded { .. })
        ));
        drop(blocking);

        let waiter_ledger = Arc::clone(&ledger);
        let (finished_tx, finished_rx) = mpsc::channel();
        let waiter = thread::spawn(move || {
            waiter_ledger.wait_for_change_after(observed_epoch);
            finished_tx.send(()).unwrap();
        });
        let result = finished_rx.recv_timeout(Duration::from_millis(250));
        if result.is_err() {
            ledger.stop_accepting();
        }
        waiter.join().unwrap();
        assert!(
            result.is_ok(),
            "a capacity release before the wait must satisfy its generation predicate"
        );
    }
}

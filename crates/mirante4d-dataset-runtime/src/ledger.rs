use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicU8, Ordering},
};

use mirante4d_dataset::{CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError};

use crate::{CPU_LEDGER_CATEGORIES, DatasetRuntimeConfig, category_index};

#[derive(Debug)]
pub(super) struct LedgerCore {
    config: DatasetRuntimeConfig,
    used: Mutex<[u64; CPU_LEDGER_CATEGORIES.len()]>,
    changed: Condvar,
    accepting: AtomicBool,
}

impl LedgerCore {
    pub(super) fn new(config: DatasetRuntimeConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            used: Mutex::new([0; CPU_LEDGER_CATEGORIES.len()]),
            changed: Condvar::new(),
            accepting: AtomicBool::new(true),
        })
    }

    pub(super) fn acquire(
        self: &Arc<Self>,
        category: CpuLedgerCategory,
        bytes: u64,
    ) -> Result<LedgerCharge, CpuLedgerError> {
        if bytes == 0 {
            return Err(CpuLedgerError::ZeroByteReservation);
        }
        if !self.accepting.load(Ordering::Acquire) {
            return Err(CpuLedgerError::ShuttingDown);
        }

        let mut used = self
            .used
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !self.accepting.load(Ordering::Acquire) {
            return Err(CpuLedgerError::ShuttingDown);
        }
        let slot = category_index(category);
        let category_used = used[slot];
        let total_used = used.iter().try_fold(0_u64, |sum, value| {
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
        let available_bytes = category_available.min(total_available);
        if bytes > available_bytes {
            return Err(CpuLedgerError::CapacityExceeded {
                category,
                requested_bytes: bytes,
                available_bytes,
            });
        }
        used[slot] = category_used + bytes;
        Ok(LedgerCharge {
            core: Arc::clone(self),
            category: AtomicU8::new(category_index(category) as u8),
            bytes,
        })
    }

    pub(super) fn snapshot(&self) -> [u64; CPU_LEDGER_CATEGORIES.len()] {
        *self
            .used
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub(super) fn stop_accepting(&self) {
        self.accepting.store(false, Ordering::Release);
        self.changed.notify_all();
    }

    pub(super) fn wait_for_change(&self) {
        let used = self
            .used
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let _ = self
            .changed
            .wait_timeout(used, std::time::Duration::from_millis(10))
            .unwrap_or_else(|poison| poison.into_inner());
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
        let mut used = self
            .used
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let old_slot = category_index(old);
        let new_slot = category_index(new);
        let new_available = self.config.category_cap(new).saturating_sub(used[new_slot]);
        if bytes > new_available {
            return Err(CpuLedgerError::CapacityExceeded {
                category: new,
                requested_bytes: bytes,
                available_bytes: new_available,
            });
        }
        used[old_slot] = used[old_slot]
            .checked_sub(bytes)
            .expect("a live charge is present in its recorded category");
        used[new_slot] = used[new_slot]
            .checked_add(bytes)
            .expect("a checked category transfer cannot overflow");
        self.changed.notify_all();
        Ok(())
    }
}

pub(super) struct LedgerHandle(pub(super) Arc<LedgerCore>);

impl CpuByteLedger for LedgerHandle {
    fn try_acquire(
        &self,
        category: CpuLedgerCategory,
        bytes: u64,
    ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
        Ok(Box::new(self.0.acquire(category, bytes)?))
    }
}

#[derive(Debug)]
pub(super) struct LedgerCharge {
    core: Arc<LedgerCore>,
    category: AtomicU8,
    bytes: u64,
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
        let mut used = self
            .core
            .used
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let slot = category_index(category);
        used[slot] = used[slot]
            .checked_sub(self.bytes)
            .expect("a live ledger charge releases exactly once");
        self.core.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_enforces_category_caps_reclassifies_without_total_overcommit_and_releases() {
        let config = DatasetRuntimeConfig::new(4_000, 1, 2, 2).unwrap();
        let ledger = LedgerCore::new(config);
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
            .acquire(CpuLedgerCategory::DecodedResidency, 1_501)
            .unwrap_err();
        assert!(matches!(
            error,
            CpuLedgerError::CapacityExceeded {
                category: CpuLedgerCategory::DecodedResidency,
                requested_bytes: 1_501,
                available_bytes: 1_500,
            }
        ));
        drop(charge);
        assert_eq!(ledger.snapshot(), [0; CPU_LEDGER_CATEGORIES.len()]);
    }
}

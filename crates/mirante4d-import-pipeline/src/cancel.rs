use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Cheap cloneable cancellation signal checked between bounded work units.
#[derive(Clone, Debug, Default)]
pub struct ImportCancellation {
    cancelled: Arc<AtomicBool>,
}

impl ImportCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

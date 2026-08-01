use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Default)]
struct CancellationState {
    cancelled: AtomicBool,
    parent: Option<Arc<CancellationState>>,
}

/// Cheap cloneable cancellation signal checked between bounded work units.
#[derive(Clone, Debug)]
pub struct ImportCancellation {
    state: Arc<CancellationState>,
}

impl Default for ImportCancellation {
    fn default() -> Self {
        Self {
            state: Arc::new(CancellationState::default()),
        }
    }
}

impl ImportCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.state.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        let mut state = Some(&self.state);
        while let Some(current) = state {
            if current.cancelled.load(Ordering::Acquire) {
                return true;
            }
            state = current.parent.as_ref();
        }
        false
    }

    /// Creates a run-local child which observes cancellation from this token
    /// but can also be stopped independently. The temporal pipeline uses a
    /// child for speculative ingest so an owner-side failure can join that
    /// worker promptly without mutating the caller's cancellation authority.
    pub(crate) fn child(&self) -> Self {
        Self {
            state: Arc::new(CancellationState {
                cancelled: AtomicBool::new(false),
                parent: Some(Arc::clone(&self.state)),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_observes_parent_but_cancels_independently() {
        let parent = ImportCancellation::new();
        let first = parent.child();
        let second = parent.child();

        first.cancel();
        assert!(first.is_cancelled());
        assert!(!parent.is_cancelled());
        assert!(!second.is_cancelled());

        parent.cancel();
        assert!(second.is_cancelled());
    }
}

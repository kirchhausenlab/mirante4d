use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

use eframe::egui;

/// A process-wide, monotonic request for graceful application shutdown.
#[derive(Default)]
pub struct ProcessTerminationLatch {
    requested: AtomicBool,
    egui_context: Mutex<Option<egui::Context>>,
}

impl ProcessTerminationLatch {
    /// Requests shutdown and wakes the native UI loop if it is already bound.
    pub fn request(&self) {
        self.requested.store(true, Ordering::Release);
        let context = self
            .egui_context
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if let Some(context) = context {
            context.request_repaint();
        }
    }

    pub(crate) fn bind_egui_context(&self, context: &egui::Context) {
        *self
            .egui_context
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(context.clone());
        if self.requested() {
            context.request_repaint();
        }
    }

    pub(crate) fn requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    #[test]
    fn request_is_monotonic() {
        let termination = ProcessTerminationLatch::default();
        assert!(!termination.requested());

        termination.request();
        assert!(termination.requested());

        termination.request();
        assert!(termination.requested());
    }

    #[test]
    fn request_wakes_a_bound_egui_context() {
        let wake_count = Arc::new(AtomicUsize::new(0));
        let context = egui::Context::default();
        let callback_wake_count = Arc::clone(&wake_count);
        context.set_request_repaint_callback(move |_| {
            callback_wake_count.fetch_add(1, Ordering::Relaxed);
        });
        let termination = ProcessTerminationLatch::default();
        termination.bind_egui_context(&context);

        termination.request();

        assert!(wake_count.load(Ordering::Relaxed) >= 1);
    }

    #[test]
    fn binding_after_a_request_wakes_the_context() {
        let wake_count = Arc::new(AtomicUsize::new(0));
        let context = egui::Context::default();
        let callback_wake_count = Arc::clone(&wake_count);
        context.set_request_repaint_callback(move |_| {
            callback_wake_count.fetch_add(1, Ordering::Relaxed);
        });
        let termination = ProcessTerminationLatch::default();
        termination.request();

        termination.bind_egui_context(&context);

        assert!(wake_count.load(Ordering::Relaxed) >= 1);
    }
}

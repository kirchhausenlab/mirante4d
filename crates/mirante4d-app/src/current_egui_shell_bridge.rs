//! Sole temporary route between the current egui shell and the application.
//!
//! This module owns translation only. Canonical state remains in
//! `ApplicationState`; callers receive immutable snapshots, typed events, and
//! typed command faults. The bridge is deleted with the current shell at
//! WP-09C.

use mirante4d_application::{
    ApplicationCommand, ApplicationEvent, ApplicationFault, ApplicationSnapshot, ApplicationState,
    CommandEffect,
};

pub(crate) fn dispatch(
    application: &mut ApplicationState,
    command: ApplicationCommand,
) -> Result<CommandEffect, ApplicationFault> {
    application.dispatch(command)
}

pub(crate) fn snapshot(application: &ApplicationState) -> ApplicationSnapshot {
    application.snapshot()
}

pub(crate) fn drain_events(
    application: &mut ApplicationState,
    limit: usize,
) -> Vec<ApplicationEvent> {
    application.drain_events(limit)
}

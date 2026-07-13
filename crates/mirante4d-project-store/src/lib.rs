//! Bounded public boundary for the experimental Mirante4D project store.
//!
//! WP-10B keeps this crate off-product while its transactional implementation
//! and durability evidence are built. Persistence wire types and filesystem
//! machinery remain private to the crate.

#![forbid(unsafe_code)]

#[cfg(target_os = "linux")]
mod actor;
mod api;
#[cfg(target_os = "linux")]
mod full_verify;
mod generation;
#[cfg(target_os = "linux")]
mod inspection;
#[cfg(target_os = "linux")]
mod lease;
#[cfg(target_os = "linux")]
mod local;
#[cfg(target_os = "linux")]
mod pin;
#[cfg(target_os = "linux")]
mod transaction;
mod wire;

pub use api::{
    ProjectCommitCapture, ProjectGenerationId, ProjectObjectSource, ProjectOpenMode,
    ProjectRecoveryCandidate, ProjectStoreActor, ProjectStoreCommand, ProjectStoreCompletion,
    ProjectStoreConfig, ProjectStoreDiagnostics, ProjectStoreFault, ProjectStoreLimits,
    ProjectStorePath, ProjectStoreReceipt, ProjectStoreRequestId, ProjectStoreSession,
};

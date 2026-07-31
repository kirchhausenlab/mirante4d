//! Bounded TIFF/OME-TIFF import into the target Mirante4D profile.
//!
//! This crate owns product source inspection, import execution, and their
//! worker threads.

#![forbid(unsafe_code)]

mod cancel;
mod canonical_cache;
mod chunk;
mod cpu_chunk;
mod error;
mod model;
mod observability;
mod ordered_workers;
mod package;
mod pipeline;
mod plan;
mod publish;
mod pyramid;
mod sentinel;
mod source;
mod spool;
mod worker;

pub use cancel::ImportCancellation;
pub use error::ImportError;
pub use model::{
    ImportEvent, ImportOptions, ImportReceipt, ImportStage, ImportStageTiming, ImportStatistics,
    NoDataPolicy, PublishedImport, SourceFingerprint, SourceLayout, SpatialCalibration,
    TiffInspection, TiffSource, deterministic_tiff_destination,
};
pub use pipeline::{import_tiff, inspect_tiff, inspect_tiff_cancellable, select_supported_profile};
pub use worker::{spawn_tiff_import_worker, spawn_tiff_inspection_worker};

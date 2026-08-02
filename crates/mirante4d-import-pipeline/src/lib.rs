//! Bounded TIFF/OME-TIFF import into the target Mirante4D profile.
//!
//! This crate owns product source inspection, import execution, and their
//! worker threads.

#![forbid(unsafe_code)]

mod cancel;
mod canonical_cache;
mod checkpoint;
mod chunk;
mod cpu_chunk;
mod error;
mod model;
mod no_data;
mod observability;
mod ordered_workers;
mod package;
mod pipeline;
mod plan;
mod pyramid;
mod sentinel;
mod source;
mod spool;
mod streaming;
mod worker;

pub use cancel::ImportCancellation;
pub use error::ImportError;
pub use model::{
    ImportCapacityPlan, ImportEvent, ImportOptions, ImportReceipt, ImportStage, ImportStageTiming,
    ImportStatistics, ImportStorageProgress, NoDataPolicy, NoDataValueRule, PublishedImport,
    SourceFingerprint, SpatialCalibration, TiffChannelSource, TiffChannelSourceKind,
    TiffInspection, TiffInspectionProgress, TiffSource, deterministic_tiff_destination,
};
pub use pipeline::{
    combine_tiff_channel_inspections, import_tiff, inspect_tiff, inspect_tiff_cancellable,
    inspect_tiff_cancellable_with_progress, relabel_tiff_channel_inspection,
    select_supported_profile,
};
pub use plan::{import_capacity_plan, minimum_import_progress_bytes};
pub use worker::{spawn_tiff_import_worker, spawn_tiff_inspection_worker};

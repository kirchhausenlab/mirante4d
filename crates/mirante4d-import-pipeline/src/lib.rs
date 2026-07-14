//! Bounded off-product TIFF/OME-TIFF import into the target Mirante4D profile.
//!
//! WP-11 owns this replacement producer. It remains unreachable from the
//! application until WP-10C removes the predecessor importer.

#![forbid(unsafe_code)]

mod cancel;
mod chunk;
mod error;
mod model;
mod package;
mod pipeline;
mod plan;
mod publish;
mod pyramid;
mod source;
mod spool;

pub use cancel::ImportCancellation;
pub use error::ImportError;
pub use model::{
    ImportEvent, ImportOptions, ImportReceipt, ImportStatistics, NoDataPolicy, SourceLayout,
    SpatialCalibration, TiffInspection, TiffSource,
};
pub use pipeline::{import_tiff, inspect_tiff, inspect_tiff_cancellable, select_supported_profile};

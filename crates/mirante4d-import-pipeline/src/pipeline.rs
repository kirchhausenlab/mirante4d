//! Public import entry points.

use mirante4d_dataset::CpuByteLedger;
use mirante4d_storage::ProfileKind;

use crate::{
    ImportCancellation, ImportError, ImportEvent, ImportOptions, PublishedImport, TiffInspection,
    TiffSource,
};

pub fn inspect_tiff(source: TiffSource) -> Result<TiffInspection, ImportError> {
    crate::source::inspect(source)
}

pub fn inspect_tiff_cancellable(
    source: TiffSource,
    cancellation: &ImportCancellation,
) -> Result<TiffInspection, ImportError> {
    crate::source::inspect_cancellable(source, cancellation)
}

pub fn inspect_tiff_cancellable_with_progress(
    source: TiffSource,
    cancellation: &ImportCancellation,
    progress: impl FnMut(crate::TiffInspectionProgress),
) -> Result<TiffInspection, ImportError> {
    crate::source::inspect_cancellable_with_progress(source, cancellation, progress)
}

pub fn combine_tiff_channel_inspections(
    inspections: Vec<TiffInspection>,
) -> Result<TiffInspection, ImportError> {
    crate::source::combine_channel_inspections(inspections)
}

pub fn relabel_tiff_channel_inspection(
    inspection: TiffInspection,
    label: &str,
) -> Result<TiffInspection, ImportError> {
    crate::source::relabel_channel_inspection(inspection, label)
}

/// Validates the request against the single compositional storage contract.
pub fn select_supported_profile(options: &ImportOptions) -> Result<ProfileKind, ImportError> {
    crate::plan::select_supported_profile(options)
}

pub fn import_tiff(
    options: ImportOptions,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    progress: impl FnMut(ImportEvent),
) -> Result<PublishedImport, ImportError> {
    crate::streaming::run(options, ledger, cancellation, progress)
}

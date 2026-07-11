//! Current import execution facts retained only until WP-10C.

use crate::{ImportTask, PendingTiffImport, TiffImportSetupTask};

/// Exact four-field temporary owner frozen by the WP-07B entry.
pub(crate) struct CurrentImportRuntime {
    pub(crate) tiff_import_setup_task: Option<TiffImportSetupTask>,
    pub(crate) tiff_import_setup_error: Option<String>,
    pub(crate) pending_tiff_import: Option<PendingTiffImport>,
    pub(crate) import_task: Option<ImportTask>,
}

impl CurrentImportRuntime {
    pub(crate) const fn idle() -> Self {
        Self {
            tiff_import_setup_task: None,
            tiff_import_setup_error: None,
            pending_tiff_import: None,
            import_task: None,
        }
    }
}

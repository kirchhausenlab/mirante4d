//! Product TIFF inspection and import execution facts.

use mirante4d_import_pipeline::ImportOptions;

use crate::PendingTiffImport;

// The product importer is final; this remaining review draft moves at WP-09C.
pub(crate) struct ImportRuntime {
    pub(crate) tiff_import_setup_error: Option<String>,
    pub(crate) pending_tiff_import: Option<PendingTiffImport>,
    pub(crate) checkpoint_retry_options: Option<ImportOptions>,
    pub(crate) checkpoint_reset_confirmed: bool,
}

impl ImportRuntime {
    pub(crate) const fn idle() -> Self {
        Self {
            tiff_import_setup_error: None,
            pending_tiff_import: None,
            checkpoint_retry_options: None,
            checkpoint_reset_confirmed: false,
        }
    }
}

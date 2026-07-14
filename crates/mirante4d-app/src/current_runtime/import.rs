//! Product TIFF inspection and import execution facts.

use mirante4d_import_pipeline::ImportOptions;

use crate::{ImportTask, PendingTiffImport, TiffImportSetupTask};

// The product importer is final; only these UI-owned task handles move at WP-09C.
pub(crate) struct ImportRuntime {
    pub(crate) tiff_import_setup_task: Option<TiffImportSetupTask>,
    pub(crate) tiff_import_setup_error: Option<String>,
    pub(crate) pending_tiff_import: Option<PendingTiffImport>,
    pub(crate) import_task: Option<ImportTask>,
    pub(crate) checkpoint_retry_options: Option<ImportOptions>,
    pub(crate) checkpoint_reset_confirmed: bool,
}

impl ImportRuntime {
    pub(crate) const fn idle() -> Self {
        Self {
            tiff_import_setup_task: None,
            tiff_import_setup_error: None,
            pending_tiff_import: None,
            import_task: None,
            checkpoint_retry_options: None,
            checkpoint_reset_confirmed: false,
        }
    }
}

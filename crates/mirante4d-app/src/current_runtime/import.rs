//! Product TIFF inspection and import execution facts.

use crate::{ImportTask, PendingTiffImport, TiffImportSetupTask};

pub(crate) struct ImportRuntime {
    pub(crate) tiff_import_setup_task: Option<TiffImportSetupTask>,
    pub(crate) tiff_import_setup_error: Option<String>,
    pub(crate) pending_tiff_import: Option<PendingTiffImport>,
    pub(crate) import_task: Option<ImportTask>,
}

impl ImportRuntime {
    pub(crate) const fn idle() -> Self {
        Self {
            tiff_import_setup_task: None,
            tiff_import_setup_error: None,
            pending_tiff_import: None,
            import_task: None,
        }
    }
}

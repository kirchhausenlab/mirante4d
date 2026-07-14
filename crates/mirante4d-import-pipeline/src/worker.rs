use std::{sync::Arc, thread};

use mirante4d_dataset::CpuByteLedger;

use crate::{
    ImportCancellation, ImportError, ImportEvent, ImportOptions, ImportReceipt, TiffInspection,
    TiffSource, import_tiff, inspect_tiff_cancellable,
};

/// Starts one bounded TIFF inspection worker owned by the import pipeline.
pub fn spawn_tiff_inspection_worker(
    source: TiffSource,
    cancellation: ImportCancellation,
    completion: impl FnOnce(Result<TiffInspection, ImportError>) + Send + 'static,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("mirante4d-tiff-inspection".to_owned())
        .spawn(move || completion(inspect_tiff_cancellable(source, &cancellation)))
        .expect("failed to start the TIFF inspection worker")
}

/// Starts one bounded TIFF import worker owned by the import pipeline.
pub fn spawn_tiff_import_worker(
    options: ImportOptions,
    ledger: Arc<dyn CpuByteLedger>,
    cancellation: ImportCancellation,
    progress: impl FnMut(ImportEvent) + Send + 'static,
    completion: impl FnOnce(Result<ImportReceipt, ImportError>) + Send + 'static,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("mirante4d-tiff-import".to_owned())
        .spawn(move || {
            completion(import_tiff(
                options,
                ledger.as_ref(),
                &cancellation,
                progress,
            ));
        })
        .expect("failed to start the TIFF import worker")
}

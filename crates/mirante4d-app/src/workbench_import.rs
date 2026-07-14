use super::*;

impl MiranteWorkbenchApp {
    pub(super) fn import_tiff_directory_from_dialog(&mut self, ctx: &egui::Context) {
        if self.import_runtime.import_task.is_some()
            || self.import_runtime.tiff_import_setup_task.is_some()
        {
            tracing::info!("TIFF import workflow is already running");
            return;
        }
        let Some(input_dir) = rfd::FileDialog::new()
            .set_title("Select TIFF directory to import")
            .pick_folder()
        else {
            return;
        };
        let Some(output_parent) = rfd::FileDialog::new()
            .set_title("Select output directory for Mirante4D package")
            .pick_folder()
        else {
            return;
        };
        self.start_tiff_import_setup_task(TiffSource::auto(input_dir), output_parent, ctx);
    }

    pub(super) fn import_tiff_file_from_dialog(&mut self, ctx: &egui::Context) {
        if self.import_runtime.import_task.is_some()
            || self.import_runtime.tiff_import_setup_task.is_some()
        {
            tracing::info!("TIFF import workflow is already running");
            return;
        }
        let Some(input_file) = rfd::FileDialog::new()
            .set_title("Select TIFF file to import")
            .add_filter("TIFF", &["tif", "tiff"])
            .pick_file()
        else {
            return;
        };
        let Some(output_parent) = rfd::FileDialog::new()
            .set_title("Select output directory for Mirante4D package")
            .pick_folder()
        else {
            return;
        };
        self.start_tiff_import_setup_task(TiffSource::auto(input_file), output_parent, ctx);
    }

    pub(super) fn start_tiff_import_setup_task(
        &mut self,
        source: TiffSource,
        output_parent: PathBuf,
        ctx: &egui::Context,
    ) {
        let destination = tiff_destination(&source, &output_parent);
        let task_source = source.clone();
        let worker_source = source.clone();
        let worker_destination = destination.clone();
        let cancellation = ImportCancellation::new();
        let worker_cancellation = cancellation.clone();
        let repaint_ctx = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = inspect_tiff_cancellable(worker_source, &worker_cancellation);
            let _ = sender.send(TiffImportSetupTaskMessage::Finished(result));
            request_background_work_repaint(&repaint_ctx);
        });

        self.enter_tiff_import_setup_waiting_state(
            source,
            destination,
            cancellation,
            receiver,
            Some(worker),
        );
        tracing::info!(
            source = %task_source.path.display(),
            destination = %worker_destination.display(),
            "started TIFF inspection"
        );
        request_background_work_repaint(ctx);
    }

    pub(super) fn enter_tiff_import_setup_waiting_state(
        &mut self,
        source: TiffSource,
        destination: PathBuf,
        cancellation: ImportCancellation,
        receiver: Receiver<TiffImportSetupTaskMessage>,
        worker: Option<thread::JoinHandle<()>>,
    ) {
        self.import_runtime.pending_tiff_import = None;
        self.import_runtime.tiff_import_setup_error = None;
        self.import_runtime.tiff_import_setup_task = Some(TiffImportSetupTask {
            source,
            destination,
            cancellation,
            receiver,
            worker,
        });
    }

    pub(super) fn drain_tiff_import_setup_results(&mut self, ctx: &egui::Context) {
        enum SetupCompletion {
            Finished(Result<TiffInspection, ImportError>),
            WorkerStopped,
        }

        let Some(completion) = self
            .import_runtime
            .tiff_import_setup_task
            .as_ref()
            .and_then(|task| match task.receiver.try_recv() {
                Ok(TiffImportSetupTaskMessage::Finished(result)) => {
                    Some(SetupCompletion::Finished(result))
                }
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(SetupCompletion::WorkerStopped),
            })
        else {
            return;
        };

        let task = self
            .import_runtime
            .tiff_import_setup_task
            .take()
            .expect("an inspection completion has an active task");
        let source = task.source.clone();
        let destination = task.destination.clone();
        let cancelled = task.cancellation.is_cancelled();
        drop(task);

        match completion {
            SetupCompletion::Finished(Ok(inspection)) if !cancelled => {
                self.import_runtime.pending_tiff_import = Some(PendingTiffImport::from_inspection(
                    source,
                    inspection,
                    destination,
                ));
                self.import_runtime.tiff_import_setup_error = None;
            }
            SetupCompletion::Finished(Err(ImportError::Cancelled))
            | SetupCompletion::Finished(Ok(_)) => {
                self.import_runtime.pending_tiff_import = None;
                self.import_runtime.tiff_import_setup_error = None;
            }
            SetupCompletion::Finished(Err(error)) => {
                self.import_runtime.pending_tiff_import = None;
                self.import_runtime.tiff_import_setup_error = Some(error.to_string());
                tracing::error!(%error, "failed to inspect TIFF input");
            }
            SetupCompletion::WorkerStopped => {
                self.import_runtime.pending_tiff_import = None;
                self.import_runtime.tiff_import_setup_error =
                    Some("TIFF inspection worker stopped unexpectedly".to_owned());
                tracing::error!("TIFF inspection worker stopped unexpectedly");
            }
        }
        ctx.request_repaint();
    }

    pub(super) fn start_pending_tiff_import(&mut self) {
        let Some(pending) = self.import_runtime.pending_tiff_import.take() else {
            return;
        };
        match build_import_options(&pending) {
            Ok(options) => {
                self.import_runtime.tiff_import_setup_error = None;
                self.start_import_task(options);
            }
            Err(error) => {
                self.import_runtime.pending_tiff_import = Some(pending);
                self.import_runtime.tiff_import_setup_error = Some(error.to_string());
            }
        }
    }

    pub(super) fn cancel_pending_tiff_import(&mut self) {
        if let Some(task) = self.import_runtime.tiff_import_setup_task.as_ref() {
            task.cancellation.cancel();
        }
        self.import_runtime.pending_tiff_import = None;
        self.import_runtime.tiff_import_setup_error = None;
    }

    pub(super) fn start_import_task(&mut self, options: ImportOptions) {
        let destination = options.destination.clone();
        let Some(token) = self.begin_background_operation(OperationKind::Import) else {
            return;
        };
        let ledger = self.dataset.cpu_ledger_arc();
        let cancellation = ImportCancellation::new();
        let worker_cancellation = cancellation.clone();
        let progress_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::channel();
        let progress_sender = sender.clone();
        let worker = thread::spawn(move || {
            let result = import_tiff(options, ledger.as_ref(), &worker_cancellation, |event| {
                if progress_sender
                    .send(ImportTaskMessage::Progress(event))
                    .is_err()
                {
                    progress_cancellation.cancel();
                }
            });
            let _ = sender.send(ImportTaskMessage::Finished(result));
        });
        self.import_runtime.import_task = Some(ImportTask {
            token,
            destination: destination.clone(),
            cancellation,
            receiver,
            latest_event: None,
            worker: Some(worker),
        });
        tracing::info!(destination = %destination.display(), "started TIFF import");
    }

    pub(super) fn cancel_import_task(&mut self) {
        if let Some(task) = self.import_runtime.import_task.as_ref() {
            let token = task.token.clone();
            task.cancellation.cancel();
            self.cancel_background_operation(&token);
        }
    }

    pub(super) fn drain_import_results(&mut self, ctx: &egui::Context) {
        enum ImportCompletion {
            Finished(Result<ImportReceipt, ImportError>),
            WorkerStopped,
        }

        let mut completion = None;
        let mut saw_progress = false;
        if let Some(task) = self.import_runtime.import_task.as_mut() {
            loop {
                match task.receiver.try_recv() {
                    Ok(ImportTaskMessage::Progress(event)) => {
                        task.latest_event = Some(event);
                        saw_progress = true;
                    }
                    Ok(ImportTaskMessage::Finished(result)) => {
                        completion = Some(ImportCompletion::Finished(result));
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        if completion.is_none() {
                            completion = Some(ImportCompletion::WorkerStopped);
                        }
                        break;
                    }
                }
            }
        }

        if let Some(completion) = completion {
            let task = self
                .import_runtime
                .import_task
                .take()
                .expect("an import completion has an active task");
            let token = task.token.clone();
            let destination = task.destination.clone();
            drop(task);
            match completion {
                ImportCompletion::Finished(Ok(receipt)) => {
                    if self.complete_background_operation(token, OperationCompletion::Succeeded) {
                        self.finish_successful_import(receipt, destination, ctx);
                    }
                }
                ImportCompletion::Finished(Err(ImportError::Cancelled)) => {
                    self.complete_background_operation(token, OperationCompletion::Cancelled);
                    self.import_runtime.tiff_import_setup_error = None;
                }
                ImportCompletion::Finished(Err(error)) => {
                    self.complete_background_operation(
                        token,
                        OperationCompletion::Failed(import_failure_code(&error)),
                    );
                    self.import_runtime.tiff_import_setup_error = Some(error.to_string());
                    tracing::error!(%error, "failed to import TIFF input");
                }
                ImportCompletion::WorkerStopped => {
                    self.complete_background_operation(
                        token,
                        OperationCompletion::Failed(OperationFailureCode::ImportExecutionFailed),
                    );
                    self.import_runtime.tiff_import_setup_error =
                        Some("TIFF import worker stopped unexpectedly".to_owned());
                    tracing::error!("TIFF import worker stopped unexpectedly");
                }
            }
            saw_progress = true;
        }

        if saw_progress {
            ctx.request_repaint();
        }
    }

    pub(super) fn finish_successful_import(
        &mut self,
        receipt: ImportReceipt,
        destination: PathBuf,
        ctx: &egui::Context,
    ) {
        if let Err(error) = self.replace_state_from_dataset_path(destination.clone(), Some(ctx)) {
            self.import_runtime.tiff_import_setup_error = Some(format!(
                "The package was created, but Mirante4D could not open it: {error}"
            ));
            tracing::error!(%error, "failed to open imported dataset");
            return;
        }
        self.import_runtime.tiff_import_setup_error = None;
        tracing::info!(
            source_bytes_read = receipt.statistics.source_bytes_read,
            peak_working_bytes = receipt.statistics.peak_working_bytes,
            resumed_work_units = receipt.statistics.resumed_work_units,
            produced_work_units = receipt.statistics.produced_work_units,
            destination = %destination.display(),
            "TIFF import completed and package open was requested"
        );
    }

    pub(super) fn show_tiff_import_setup_window(
        &mut self,
        ctx: &egui::Context,
        start_pending_tiff_import: &mut bool,
        cancel_pending_tiff_import: &mut bool,
        dismiss_setup_error: &mut bool,
    ) {
        if self.import_runtime.tiff_import_setup_task.is_none()
            && self.import_runtime.pending_tiff_import.is_none()
            && self.import_runtime.tiff_import_setup_error.is_none()
        {
            return;
        }

        egui::Window::new("TIFF Import")
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                if let Some(task) = &self.import_runtime.tiff_import_setup_task {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        ui_kit::status_badge(
                            ui,
                            StatusTone::Warning,
                            if task.cancellation.is_cancelled() {
                                "stopping inspection"
                            } else {
                                "inspecting input"
                            },
                        );
                    });
                    ui_kit::property_row(ui, "source", task.source.path.display());
                    ui_kit::property_row(ui, "destination", task.destination.display());
                    ui.add_space(8.0);
                    if ui_kit::toolbar_button(
                        ui,
                        "Cancel Inspection",
                        !task.cancellation.is_cancelled(),
                    )
                    .clicked()
                    {
                        *cancel_pending_tiff_import = true;
                    }
                    return;
                }

                if let Some(error) = self.import_runtime.tiff_import_setup_error.as_deref() {
                    ui_kit::status_badge(ui, StatusTone::Error, "import could not continue");
                    ui.add_space(6.0);
                    ui.label(error);
                    ui.add_space(6.0);
                    ui.label(
                        "Select a supported grayscale TIFF file or an unambiguous TIFF directory.",
                    );
                    ui.add_space(8.0);
                    if ui_kit::toolbar_button(ui, "Dismiss", true).clicked() {
                        *dismiss_setup_error = true;
                    }
                    return;
                }

                if let Some(pending) = &mut self.import_runtime.pending_tiff_import {
                    ui_kit::status_badge(ui, StatusTone::Warning, "review import");
                    ui.add_space(6.0);
                    show_pending_tiff_import_controls(ui, pending);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui_kit::toolbar_button(
                            ui,
                            "Start Import",
                            pending_tiff_import_ready_to_start(pending),
                        )
                        .clicked()
                        {
                            *start_pending_tiff_import = true;
                        }
                        if ui_kit::toolbar_button(ui, "Cancel", true).clicked() {
                            *cancel_pending_tiff_import = true;
                        }
                    });
                }
            });
    }
}

fn import_failure_code(error: &ImportError) -> OperationFailureCode {
    match error {
        ImportError::NoSupportedProfile
        | ImportError::InsufficientSpace { .. }
        | ImportError::WorkingMemoryExceeded { .. }
        | ImportError::Ledger(_)
        | ImportError::Overflow => OperationFailureCode::ImportCapacityExceeded,
        ImportError::MissingSource(_)
        | ImportError::AmbiguousSource(_)
        | ImportError::UnsupportedSource(_)
        | ImportError::InvalidRequest(_)
        | ImportError::SourceChanged(_)
        | ImportError::InvalidCheckpoint(_) => OperationFailureCode::ImportInvalidInput,
        ImportError::Cancelled => unreachable!("cancellation is handled separately"),
        _ => OperationFailureCode::ImportExecutionFailed,
    }
}

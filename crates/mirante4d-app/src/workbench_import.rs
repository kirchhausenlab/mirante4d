use super::*;

impl MiranteWorkbenchApp {
    pub(super) fn import_tiff_directory_from_dialog(&mut self, ctx: &egui::Context) {
        if self.import_workers.status().is_active() {
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
        if self.import_workers.status().is_active() {
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
        if let Err(error) = self.enter_tiff_import_setup_waiting_state(source, destination) {
            self.import_runtime.tiff_import_setup_error = Some(error.to_string());
            tracing::error!(%error, "TIFF inspection could not start");
        } else {
            request_background_work_repaint(ctx);
        }
    }

    pub(super) fn enter_tiff_import_setup_waiting_state(
        &mut self,
        source: TiffSource,
        destination: PathBuf,
    ) -> Result<(), import_worker_service::ImportWorkerBusy> {
        self.import_runtime.pending_tiff_import = None;
        self.import_runtime.tiff_import_setup_error = None;
        self.import_runtime.checkpoint_retry_options = None;
        self.import_runtime.checkpoint_reset_confirmed = false;
        let source_path = source.path.clone();
        let logged_destination = destination.clone();
        self.import_workers.start_inspection(source, destination)?;
        tracing::info!(
            source = %source_path.display(),
            destination = %logged_destination.display(),
            "started TIFF inspection"
        );
        Ok(())
    }

    pub(super) fn drain_tiff_import_setup_results(&mut self, ctx: &egui::Context) {
        if !self.import_workers.status().is_inspecting() {
            return;
        }
        let Some(ImportWorkerCompletion::Inspection(completion)) =
            self.import_workers.poll_completion()
        else {
            return;
        };
        let import_worker_service::InspectionWorkerCompletion {
            source,
            destination,
            cancellation_requested,
            outcome,
        } = *completion;

        match outcome {
            ImportWorkerOutcome::Finished(Ok(inspection)) if !cancellation_requested => {
                self.import_runtime.pending_tiff_import = Some(PendingTiffImport::from_inspection(
                    source,
                    inspection,
                    destination,
                ));
                self.import_runtime.tiff_import_setup_error = None;
            }
            ImportWorkerOutcome::Finished(Err(ImportError::Cancelled))
            | ImportWorkerOutcome::Finished(Ok(_)) => {
                self.import_runtime.pending_tiff_import = None;
                self.import_runtime.tiff_import_setup_error = None;
            }
            ImportWorkerOutcome::Finished(Err(error)) => {
                self.import_runtime.pending_tiff_import = None;
                self.import_runtime.tiff_import_setup_error = Some(error.to_string());
                tracing::error!(%error, "failed to inspect TIFF input");
            }
            ImportWorkerOutcome::WorkerStopped => {
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
        self.import_workers.cancel_inspection();
        self.import_runtime.pending_tiff_import = None;
        self.import_runtime.tiff_import_setup_error = None;
        self.import_runtime.checkpoint_retry_options = None;
        self.import_runtime.checkpoint_reset_confirmed = false;
    }

    pub(super) fn start_import_task(&mut self, options: ImportOptions) {
        let destination = options.destination.clone();
        let Some(token) = self.begin_background_operation(OperationKind::Import) else {
            return;
        };
        self.import_runtime.checkpoint_retry_options = None;
        self.import_runtime.checkpoint_reset_confirmed = false;
        let ledger = self.dataset.cpu_ledger_arc();
        match self
            .import_workers
            .start_import(token.clone(), options, ledger)
        {
            Ok(()) => {
                tracing::info!(destination = %destination.display(), "started TIFF import");
            }
            Err(error) => {
                self.complete_background_operation(
                    token,
                    OperationCompletion::Failed(OperationFailureCode::ImportExecutionFailed),
                );
                self.import_runtime.tiff_import_setup_error = Some(error.to_string());
                tracing::error!(%error, "TIFF import could not start");
            }
        }
    }

    pub(super) fn cancel_import_task(&mut self) {
        self.import_workers.cancel_import();
    }

    pub(super) fn drain_import_results(&mut self, ctx: &egui::Context) {
        if !self.import_workers.status().is_importing() {
            return;
        }
        let Some(ImportWorkerCompletion::Import(completion)) =
            self.import_workers.poll_completion()
        else {
            return;
        };
        let import_worker_service::ImportExecutionCompletion {
            token,
            destination,
            retry_options,
            outcome,
        } = *completion;
        match outcome {
            ImportWorkerOutcome::Finished(Ok(receipt)) => {
                self.import_runtime.checkpoint_retry_options = None;
                self.import_runtime.checkpoint_reset_confirmed = false;
                if self.complete_background_operation(token, OperationCompletion::Succeeded) {
                    self.finish_successful_import(receipt, destination, ctx);
                }
            }
            ImportWorkerOutcome::Finished(Err(ImportError::Cancelled)) => {
                self.complete_background_operation(token, OperationCompletion::Cancelled);
                self.import_runtime.tiff_import_setup_error = None;
                self.import_runtime.checkpoint_retry_options = None;
                self.import_runtime.checkpoint_reset_confirmed = false;
            }
            ImportWorkerOutcome::Finished(Err(ImportError::InvalidCheckpoint(reason))) => {
                self.complete_background_operation(
                    token,
                    OperationCompletion::Failed(OperationFailureCode::ImportInvalidInput),
                );
                self.import_runtime.tiff_import_setup_error = Some(format!(
                    "The saved import checkpoint is corrupt or belongs to different inputs: {reason}. Confirm Reset and Restart below to remove only that checkpoint and retry."
                ));
                self.import_runtime.checkpoint_retry_options = retry_options;
                self.import_runtime.checkpoint_reset_confirmed = false;
                tracing::error!(%reason, "failed to reuse TIFF import checkpoint");
            }
            ImportWorkerOutcome::Finished(Err(error)) => {
                self.complete_background_operation(
                    token,
                    OperationCompletion::Failed(import_failure_code(&error)),
                );
                self.import_runtime.tiff_import_setup_error = Some(error.to_string());
                self.import_runtime.checkpoint_retry_options = None;
                self.import_runtime.checkpoint_reset_confirmed = false;
                tracing::error!(%error, "failed to import TIFF input");
            }
            ImportWorkerOutcome::WorkerStopped => {
                self.complete_background_operation(
                    token,
                    OperationCompletion::Failed(OperationFailureCode::ImportExecutionFailed),
                );
                self.import_runtime.tiff_import_setup_error =
                    Some("TIFF import worker stopped unexpectedly".to_owned());
                self.import_runtime.checkpoint_retry_options = None;
                self.import_runtime.checkpoint_reset_confirmed = false;
                tracing::error!("TIFF import worker stopped unexpectedly");
            }
        }
        ctx.request_repaint();
    }

    pub(super) fn finish_successful_import(
        &mut self,
        receipt: ImportReceipt,
        destination: PathBuf,
        ctx: &egui::Context,
    ) {
        let open_started = match self.open_or_queue_dataset_path(destination.clone(), Some(ctx)) {
            Ok(open_started) => open_started,
            Err(error) => {
                self.import_runtime.tiff_import_setup_error = Some(format!(
                    "The package was created, but Mirante4D could not open it: {error}"
                ));
                tracing::error!(%error, "failed to open imported dataset");
                return;
            }
        };
        self.import_runtime.tiff_import_setup_error = None;
        if !open_started {
            self.project_status_message = Some(
                "Import completed. Save or discard the current project to open the new package."
                    .to_owned(),
            );
        }
        tracing::info!(
            source_bytes_read = receipt.statistics.source_bytes_read,
            peak_working_bytes = receipt.statistics.peak_working_bytes,
            resumed_work_units = receipt.statistics.resumed_work_units,
            produced_work_units = receipt.statistics.produced_work_units,
            destination = %destination.display(),
            open_started,
            "TIFF import completed"
        );
    }

    pub(super) fn reset_invalid_checkpoint_and_restart(&mut self) {
        if !self.import_runtime.checkpoint_reset_confirmed {
            return;
        }
        let Some(options) = self.import_runtime.checkpoint_retry_options.take() else {
            self.import_runtime.checkpoint_reset_confirmed = false;
            return;
        };
        let checkpoint = options.checkpoint_directory.clone();
        match reset_checkpoint_directory(&checkpoint) {
            Ok(()) => {
                self.import_runtime.tiff_import_setup_error = None;
                self.import_runtime.checkpoint_reset_confirmed = false;
                self.start_import_task(options);
            }
            Err(error) => {
                self.import_runtime.checkpoint_retry_options = Some(options);
                self.import_runtime.checkpoint_reset_confirmed = false;
                self.import_runtime.tiff_import_setup_error = Some(format!(
                    "The checkpoint was not reset, so nothing was restarted: {error}"
                ));
            }
        }
    }

    pub(super) fn show_tiff_import_setup_window(
        &mut self,
        ctx: &egui::Context,
        start_pending_tiff_import: &mut bool,
        cancel_pending_tiff_import: &mut bool,
        dismiss_setup_error: &mut bool,
        reset_invalid_checkpoint: &mut bool,
    ) {
        let worker_status = self.import_workers.status();
        if !worker_status.is_inspecting()
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
                if let ImportWorkerStatus::Inspecting {
                    source,
                    destination,
                    cancellation_requested,
                } = &worker_status
                {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        ui_kit::status_badge(
                            ui,
                            StatusTone::Warning,
                            if *cancellation_requested {
                                "stopping inspection"
                            } else {
                                "inspecting input"
                            },
                        );
                    });
                    ui_kit::property_row(ui, "source", source.path.display());
                    ui_kit::property_row(ui, "destination", destination.display());
                    ui.add_space(8.0);
                    if ui_kit::toolbar_button(
                        ui,
                        "Cancel Inspection",
                        !cancellation_requested,
                    )
                    .clicked()
                    {
                        *cancel_pending_tiff_import = true;
                    }
                    return;
                }

                if let Some(error) = self.import_runtime.tiff_import_setup_error.clone() {
                    ui_kit::status_badge(ui, StatusTone::Error, "import could not continue");
                    ui.add_space(6.0);
                    ui.label(error);
                    ui.add_space(6.0);
                    if let Some(options) = self.import_runtime.checkpoint_retry_options.as_ref() {
                        ui_kit::property_row(
                            ui,
                            "checkpoint",
                            options.checkpoint_directory.display(),
                        );
                        ui.checkbox(
                            &mut self.import_runtime.checkpoint_reset_confirmed,
                            "I confirm this saved import checkpoint may be deleted",
                        );
                    } else {
                        ui.label(
                            "Select a supported grayscale TIFF file or an unambiguous TIFF directory.",
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if self.import_runtime.checkpoint_retry_options.is_some()
                            && ui_kit::toolbar_button(
                                ui,
                                "Reset and Restart",
                                self.import_runtime.checkpoint_reset_confirmed,
                            )
                            .clicked()
                        {
                            *reset_invalid_checkpoint = true;
                        }
                        if ui_kit::toolbar_button(ui, "Dismiss", true).clicked() {
                            *dismiss_setup_error = true;
                        }
                    });
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

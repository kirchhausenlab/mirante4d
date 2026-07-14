use super::*;

impl MiranteWorkbenchApp {
    pub(super) fn import_tiff_directory_from_dialog(&mut self, ctx: &egui::Context) {
        if self.import.workers.status().is_active() {
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
        if self.import.workers.status().is_active() {
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
            self.import.problem = Some(error.to_string());
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
        let source_path = source.path.clone();
        let logged_destination = destination.clone();
        self.import.workers.start_inspection(source, destination)?;
        self.import.pending_review = None;
        self.import.problem = None;
        self.import.checkpoint_retry = None;
        tracing::info!(
            source = %source_path.display(),
            destination = %logged_destination.display(),
            "started TIFF inspection"
        );
        Ok(())
    }

    pub(super) fn drain_tiff_import_setup_results(&mut self, ctx: &egui::Context) {
        if !self.import.workers.status().is_inspecting() {
            return;
        }
        let Some(ImportWorkerCompletion::Inspection(completion)) =
            self.import.workers.poll_completion()
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
                if let Err(error) = self.import.install_review(source, inspection, destination) {
                    self.import.problem = Some(error.to_string());
                    tracing::error!(%error, "TIFF import review could not be prepared");
                }
            }
            ImportWorkerOutcome::Finished(Err(ImportError::Cancelled))
            | ImportWorkerOutcome::Finished(Ok(_)) => {
                self.import.pending_review = None;
                self.import.problem = None;
            }
            ImportWorkerOutcome::Finished(Err(error)) => {
                self.import.pending_review = None;
                self.import.problem = Some(error.to_string());
                tracing::error!(%error, "failed to inspect TIFF input");
            }
            ImportWorkerOutcome::WorkerStopped => {
                self.import.pending_review = None;
                self.import.problem =
                    Some("TIFF inspection worker stopped unexpectedly".to_owned());
                tracing::error!("TIFF inspection worker stopped unexpectedly");
            }
        }
        ctx.request_repaint();
    }

    pub(super) fn apply_import_command(&mut self, command: ImportCommand, ctx: &egui::Context) {
        match command {
            ImportCommand::CancelInspection => {
                self.import.workers.cancel_inspection();
            }
            ImportCommand::Start { review_id, draft } => {
                match self.import.start_options(review_id, draft) {
                    Ok(Some(options)) => {
                        self.start_import_task(review_id, options);
                    }
                    Ok(None) => {
                        tracing::info!(
                            review_id = review_id.get(),
                            "ignored a stale TIFF import review action"
                        );
                    }
                    Err(error) => {
                        self.import.problem = Some(error.to_string());
                    }
                }
            }
            ImportCommand::CancelReview { review_id } => {
                self.import.cancel_review(review_id);
            }
            ImportCommand::CancelImport => {
                self.import.workers.cancel_import();
            }
            ImportCommand::DismissProblem => {
                self.import.problem = None;
                self.import.checkpoint_retry = None;
            }
            ImportCommand::ResetCheckpointAndRestart { retry_id } => {
                self.reset_invalid_checkpoint_and_restart(retry_id);
            }
        }
        ctx.request_repaint();
    }

    pub(super) fn start_import_task(
        &mut self,
        review_id: ImportReviewId,
        options: ImportOptions,
    ) -> bool {
        let destination = options.destination.clone();
        let Some(token) = self.begin_background_operation(OperationKind::Import) else {
            self.import.problem =
                Some("the import could not start while another operation is active".to_owned());
            return false;
        };
        let ledger = self.dataset.cpu_ledger_arc();
        match self
            .import
            .workers
            .start_import(review_id, token.clone(), options, ledger)
        {
            Ok(()) => {
                self.import.complete_review(review_id);
                self.import.checkpoint_retry = None;
                tracing::info!(destination = %destination.display(), "started TIFF import");
                true
            }
            Err(error) => {
                self.complete_background_operation(
                    token,
                    OperationCompletion::Failed(OperationFailureCode::ImportExecutionFailed),
                );
                self.import.problem = Some(error.to_string());
                tracing::error!(%error, "TIFF import could not start");
                false
            }
        }
    }

    pub(super) fn drain_import_results(&mut self, ctx: &egui::Context) {
        if !self.import.workers.status().is_importing() {
            return;
        }
        let Some(ImportWorkerCompletion::Import(completion)) =
            self.import.workers.poll_completion()
        else {
            return;
        };
        let import_worker_service::ImportExecutionCompletion {
            review_id,
            token,
            destination,
            retry_options,
            outcome,
        } = *completion;
        match outcome {
            ImportWorkerOutcome::Finished(Ok(receipt)) => {
                self.import.checkpoint_retry = None;
                self.import.problem = None;
                if self.complete_background_operation(token, OperationCompletion::Succeeded) {
                    self.finish_successful_import(receipt, destination, ctx);
                }
            }
            ImportWorkerOutcome::Finished(Err(ImportError::Cancelled)) => {
                self.complete_background_operation(token, OperationCompletion::Cancelled);
                self.import.problem = None;
                self.import.checkpoint_retry = None;
            }
            ImportWorkerOutcome::Finished(Err(ImportError::InvalidCheckpoint(reason))) => {
                self.complete_background_operation(
                    token,
                    OperationCompletion::Failed(OperationFailureCode::ImportInvalidInput),
                );
                self.import.problem = Some(format!(
                    "The saved import checkpoint is corrupt or belongs to different inputs: {reason}. Confirm Reset and Restart below to remove only that checkpoint and retry."
                ));
                self.import.checkpoint_retry = retry_options.map(|options| (review_id, options));
                tracing::error!(%reason, "failed to reuse TIFF import checkpoint");
            }
            ImportWorkerOutcome::Finished(Err(error)) => {
                self.complete_background_operation(
                    token,
                    OperationCompletion::Failed(import_failure_code(&error)),
                );
                self.import.problem = Some(error.to_string());
                self.import.checkpoint_retry = None;
                tracing::error!(%error, "failed to import TIFF input");
            }
            ImportWorkerOutcome::WorkerStopped => {
                self.complete_background_operation(
                    token,
                    OperationCompletion::Failed(OperationFailureCode::ImportExecutionFailed),
                );
                self.import.problem = Some("TIFF import worker stopped unexpectedly".to_owned());
                self.import.checkpoint_retry = None;
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
                self.import.problem = Some(format!(
                    "The package was created, but Mirante4D could not open it: {error}"
                ));
                tracing::error!(%error, "failed to open imported dataset");
                return;
            }
        };
        self.import.problem = None;
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

    fn reset_invalid_checkpoint_and_restart(&mut self, retry_id: ImportReviewId) {
        let Some((stored_id, options)) = self.import.checkpoint_retry.take() else {
            return;
        };
        if stored_id != retry_id {
            self.import.checkpoint_retry = Some((stored_id, options));
            tracing::info!(
                retry_id = retry_id.get(),
                "ignored a stale TIFF checkpoint reset action"
            );
            return;
        }
        let checkpoint = options.checkpoint_directory.clone();
        match reset_checkpoint_directory(&checkpoint) {
            Ok(()) => {
                self.import.problem = None;
                if !self.start_import_task(retry_id, options.clone()) {
                    self.import.checkpoint_retry = Some((retry_id, options));
                }
            }
            Err(error) => {
                self.import.checkpoint_retry = Some((retry_id, options));
                self.import.problem = Some(format!(
                    "The checkpoint was not reset, so nothing was restarted: {error}"
                ));
            }
        }
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

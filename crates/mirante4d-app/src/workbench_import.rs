use super::*;

impl MiranteWorkbenchApp {
    pub(super) fn begin_preprocessing_setup(&mut self, ctx: &egui::Context) {
        self.bind_import_worker_completion_repaint(ctx);
        self.import.begin_setup();
        ctx.request_repaint();
    }

    fn choose_setup_channel_source(&mut self, channel: usize, ctx: &egui::Context) {
        if self.import.workers.status().is_active() {
            return;
        }
        let Some(row) = self
            .import
            .setup
            .as_ref()
            .and_then(|setup| setup.channels.get(channel))
        else {
            return;
        };
        let path = match row.source_kind {
            ImportChannelSourceKind::Single3dTiff => rfd::FileDialog::new()
                .set_title("Choose one 3D TIFF")
                .add_filter("TIFF", &["tif", "tiff"])
                .pick_file(),
            ImportChannelSourceKind::FolderOf3dTiffs => rfd::FileDialog::new()
                .set_title("Choose folder of 3D TIFF timepoints")
                .pick_folder(),
            ImportChannelSourceKind::FolderOf2dTiffs => rfd::FileDialog::new()
                .set_title("Choose folder of 2D TIFF planes")
                .pick_folder(),
        };
        let Some(path) = path else { return };
        self.import.install_channel_selection(channel, path.clone());
        let row = &self
            .import
            .setup
            .as_ref()
            .expect("setup remains active")
            .channels[channel];
        let source = match row.source_kind {
            ImportChannelSourceKind::Single3dTiff => {
                TiffChannelSource::single_3d(&row.label, &path)
            }
            ImportChannelSourceKind::FolderOf3dTiffs => {
                TiffChannelSource::folder_of_3d(&row.label, &path)
            }
            ImportChannelSourceKind::FolderOf2dTiffs => {
                TiffChannelSource::folder_of_2d(&row.label, &path)
            }
        }
        .and_then(|channel| TiffSource::new(vec![channel]))
        .map_err(anyhow::Error::msg);
        match source {
            Ok(source) => {
                self.bind_import_worker_completion_repaint(ctx);
                if self
                    .import
                    .workers
                    .start_inspection(source, PathBuf::new())
                    .is_ok()
                {
                    self.import.mark_channel_inspection_active(channel);
                }
            }
            Err(error) => {
                if let Some(row) = self
                    .import
                    .setup
                    .as_mut()
                    .and_then(|setup| setup.channels.get_mut(channel))
                {
                    row.error = Some(error.to_string());
                }
            }
        }
        request_background_work_repaint(ctx);
    }

    fn validate_setup_channels(&mut self, ctx: &egui::Context) {
        let inspection = match self.import.validated_setup_inspection() {
            Ok(inspection) => inspection,
            Err(error) => {
                if let Some(setup) = self.import.setup.as_mut() {
                    setup.validation_error = Some(error.to_string());
                }
                return;
            }
        };
        let Some(output_parent) = rfd::FileDialog::new()
            .set_title("Choose output directory for the Mirante4D package")
            .pick_folder()
        else {
            return;
        };
        let source = inspection.source().clone();
        let destination = deterministic_tiff_destination(&source, &output_parent);
        match self.import.install_review(source, inspection, destination) {
            Ok(_) => self.import.setup = None,
            Err(error) => {
                if let Some(setup) = self.import.setup.as_mut() {
                    setup.validation_error = Some(error.to_string());
                }
            }
        }
        ctx.request_repaint();
    }

    pub(super) fn bind_import_worker_completion_repaint(&mut self, ctx: &egui::Context) {
        let completion_ctx = ctx.clone();
        self.import
            .workers
            .set_completion_wake(move || completion_ctx.request_repaint());
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
            cancellation_requested,
            outcome,
        } = *completion;

        match outcome {
            ImportWorkerOutcome::Finished(Ok(inspection)) if !cancellation_requested => {
                if let Some(channel) = self.import.take_current_inspection_channel()
                    && let Some(row) = self
                        .import
                        .setup
                        .as_mut()
                        .and_then(|setup| setup.channels.get_mut(channel))
                {
                    row.inspection = Some(inspection);
                    row.error = None;
                }
            }
            ImportWorkerOutcome::Finished(Err(ImportError::Cancelled))
            | ImportWorkerOutcome::Finished(Ok(_)) => {
                self.import.active_setup_inspection = None;
                self.import.problem = None;
            }
            ImportWorkerOutcome::Finished(Err(error)) => {
                if let Some(channel) = self.import.take_current_inspection_channel()
                    && let Some(row) = self
                        .import
                        .setup
                        .as_mut()
                        .and_then(|setup| setup.channels.get_mut(channel))
                {
                    row.inspection = None;
                    row.error = Some(error.to_string());
                }
            }
            ImportWorkerOutcome::WorkerStopped => {
                self.import.active_setup_inspection = None;
                self.import.problem =
                    Some("TIFF inspection worker stopped unexpectedly".to_owned());
                tracing::error!("TIFF inspection worker stopped unexpectedly");
            }
        }
        ctx.request_repaint();
    }

    pub(super) fn apply_import_command(&mut self, command: ImportCommand, ctx: &egui::Context) {
        match command {
            ImportCommand::BeginSetup => self.begin_preprocessing_setup(ctx),
            ImportCommand::SetChannelCount { count } => self.import.set_channel_count(count),
            ImportCommand::SetChannelLabel { channel, label } => {
                self.import.set_channel_label(channel, label)
            }
            ImportCommand::SetChannelSourceKind { channel, kind } => {
                self.import.set_channel_kind(channel, kind)
            }
            ImportCommand::ChooseChannelSource { channel } => {
                self.choose_setup_channel_source(channel, ctx)
            }
            ImportCommand::ValidateChannels => self.validate_setup_channels(ctx),
            ImportCommand::CancelSetup => self.import.cancel_setup(),
            ImportCommand::CancelInspection => {
                self.import.workers.cancel_inspection();
            }
            ImportCommand::Start { review_id, draft } => {
                self.bind_import_worker_completion_repaint(ctx);
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
                self.import.checkpoint_recovery = None;
            }
            ImportCommand::RecoverCheckpoint { retry_id, action } => {
                self.bind_import_worker_completion_repaint(ctx);
                self.recover_checkpoint(retry_id, action);
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
        let progress_bytes = match minimum_import_progress_bytes(&options) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.complete_background_operation(
                    token,
                    OperationCompletion::Failed(OperationFailureCode::ImportExecutionFailed),
                );
                self.import.problem = Some(error.to_string());
                return false;
            }
        };
        let progress_reservation = match self.cpu_broker.reserve_import_progress(progress_bytes) {
            Ok(reservation) => reservation,
            Err(error) => {
                self.complete_background_operation(
                    token,
                    OperationCompletion::Failed(OperationFailureCode::ImportExecutionFailed),
                );
                self.import.problem = Some(format!(
                    "Preprocessing cannot start because its minimum complete progress path needs {progress_bytes} managed CPU bytes: {error}"
                ));
                return false;
            }
        };
        match self.import.workers.start_import(
            review_id,
            token.clone(),
            options,
            progress_reservation,
        ) {
            Ok(()) => {
                self.import.complete_review(review_id);
                self.import.checkpoint_recovery = None;
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
            source_fingerprint: _,
            reviewed_source_bytes: _,
            retry_options,
            elapsed: _,
            outcome,
        } = *completion;
        let Some(token) = token else {
            self.import.problem = Some(
                "An internal import ownership error prevented completion from being installed."
                    .to_owned(),
            );
            ctx.request_repaint();
            return;
        };
        match outcome {
            ImportWorkerOutcome::Finished(Ok(published)) => {
                self.import.checkpoint_recovery = None;
                self.import.problem = None;
                if !same_existing_import_destination(published.destination(), &destination) {
                    self.complete_background_operation(
                        token,
                        OperationCompletion::Failed(OperationFailureCode::ImportExecutionFailed),
                    );
                    self.import.problem = Some(
                        "The published package authority did not match the reviewed destination. The package was not opened."
                            .to_owned(),
                    );
                    tracing::error!(
                        reviewed_destination = %destination.display(),
                        published_destination = %published.destination().display(),
                        "published import destination binding mismatch"
                    );
                    ctx.request_repaint();
                    return;
                }
                if self.complete_background_operation(token, OperationCompletion::Succeeded) {
                    self.finish_successful_import(published, destination, ctx);
                }
            }
            ImportWorkerOutcome::Finished(Err(ImportError::Cancelled)) => {
                self.complete_background_operation(token, OperationCompletion::Cancelled);
                self.import.problem = None;
                self.import.checkpoint_recovery = None;
            }
            ImportWorkerOutcome::Finished(Err(ImportError::InvalidCheckpoint(reason))) => {
                self.complete_background_operation(
                    token,
                    OperationCompletion::Failed(OperationFailureCode::ImportInvalidInput),
                );
                self.import.problem = Some(format!(
                    "The saved import checkpoint is corrupt or belongs to different inputs: {reason}. Confirm Reset and Restart below to remove only that checkpoint and retry."
                ));
                self.import.checkpoint_recovery =
                    retry_options.map(|options| PendingImportRecovery {
                        id: review_id,
                        options,
                        action: ImportRecoveryAction::ResetAndRestart,
                    });
                tracing::error!(%reason, "failed to reuse TIFF import checkpoint");
            }
            ImportWorkerOutcome::Finished(Err(ImportError::CapacityPaused {
                required_bytes,
                available_bytes,
            })) => {
                self.complete_background_operation(
                    token,
                    OperationCompletion::Failed(OperationFailureCode::ImportCapacityExceeded),
                );
                self.import.problem = Some(format!(
                    "Preprocessing paused before the next durable step because it needs {required_bytes} additional filesystem bytes and {available_bytes} are available. Free space, then Resume; the saved checkpoint will not be deleted."
                ));
                self.import.checkpoint_recovery =
                    retry_options.map(|options| PendingImportRecovery {
                        id: review_id,
                        options,
                        action: ImportRecoveryAction::Resume,
                    });
                tracing::warn!(
                    required_bytes,
                    available_bytes,
                    "paused TIFF import for destination capacity"
                );
            }
            ImportWorkerOutcome::Finished(Err(error)) => {
                self.complete_background_operation(
                    token,
                    OperationCompletion::Failed(import_failure_code(&error)),
                );
                self.import.problem = Some(error.to_string());
                self.import.checkpoint_recovery = None;
                tracing::error!(%error, "failed to import TIFF input");
            }
            ImportWorkerOutcome::WorkerStopped => {
                self.complete_background_operation(
                    token,
                    OperationCompletion::Failed(OperationFailureCode::ImportExecutionFailed),
                );
                self.import.problem = Some("TIFF import worker stopped unexpectedly".to_owned());
                self.import.checkpoint_recovery = None;
                tracing::error!("TIFF import worker stopped unexpectedly");
            }
        }
        ctx.request_repaint();
    }

    pub(super) fn finish_successful_import(
        &mut self,
        published: PublishedImport,
        destination: PathBuf,
        ctx: &egui::Context,
    ) {
        let receipt = published.receipt().clone();
        let open_started = match self.open_or_queue_imported_dataset(published, Some(ctx)) {
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

    fn recover_checkpoint(&mut self, retry_id: ImportReviewId, action: ImportRecoveryAction) {
        let Some(recovery) = self.import.checkpoint_recovery.take() else {
            return;
        };
        if recovery.id != retry_id || recovery.action != action {
            self.import.checkpoint_recovery = Some(recovery);
            tracing::info!(
                retry_id = retry_id.get(),
                "ignored a stale TIFF checkpoint recovery action"
            );
            return;
        }
        if action == ImportRecoveryAction::ResetAndRestart
            && let Err(error) = reset_checkpoint_directory(&recovery.options.checkpoint_directory)
        {
            self.import.problem = Some(format!(
                "The checkpoint was not reset, so nothing was restarted: {error}"
            ));
            self.import.checkpoint_recovery = Some(recovery);
            return;
        }
        self.import.problem = None;
        if !self.start_import_task(retry_id, recovery.options.clone()) {
            self.import.checkpoint_recovery = Some(recovery);
        }
    }
}

fn same_existing_import_destination(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
    left == right
}

fn import_failure_code(error: &ImportError) -> OperationFailureCode {
    match error {
        ImportError::InsufficientSpace { .. }
        | ImportError::CapacityPaused { .. }
        | ImportError::ManagedCapacityInsufficient { .. }
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

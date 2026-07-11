use super::*;

impl MiranteWorkbenchApp {
    pub(super) fn import_tiff_directory_from_dialog(&mut self, ctx: &egui::Context) {
        if self.import_task.is_some() || self.tiff_import_setup_task.is_some() {
            self.state.last_workflow_message =
                Some("TIFF import workflow already running".to_owned());
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
        self.start_tiff_import_setup_task(
            TiffImportSource::Directory(input_dir),
            output_parent,
            ctx,
        );
    }

    pub(super) fn import_tiff_file_from_dialog(&mut self, ctx: &egui::Context) {
        if self.import_task.is_some() || self.tiff_import_setup_task.is_some() {
            self.state.last_workflow_message =
                Some("TIFF import workflow already running".to_owned());
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
        self.start_tiff_import_setup_task(
            TiffImportSource::SingleFile(input_file),
            output_parent,
            ctx,
        );
    }

    pub(super) fn start_tiff_import_setup_task(
        &mut self,
        source: TiffImportSource,
        output_parent: PathBuf,
        ctx: &egui::Context,
    ) {
        let task_source = source.clone();
        let task_output_parent = output_parent.clone();
        let repaint_ctx = ctx.clone();
        let (sender, receiver) = mpsc::channel();

        self.enter_tiff_import_setup_waiting_state(source, output_parent, receiver);
        tracing::info!(
            source = %task_source.path().display(),
            output_parent = %task_output_parent.display(),
            "started TIFF import setup"
        );
        request_background_work_repaint(ctx);

        thread::spawn(move || {
            let result = prepare_tiff_source_import(task_source, &task_output_parent)
                .map_err(|err| err.to_string());
            let _ = sender.send(TiffImportSetupTaskMessage::Finished(result));
            request_background_work_repaint(&repaint_ctx);
        });
    }

    pub(super) fn enter_tiff_import_setup_waiting_state(
        &mut self,
        source: TiffImportSource,
        output_parent: PathBuf,
        receiver: Receiver<TiffImportSetupTaskMessage>,
    ) {
        self.pending_tiff_import = None;
        self.tiff_import_setup_error = None;
        self.tiff_import_setup_task = Some(TiffImportSetupTask {
            source,
            output_parent,
            receiver,
        });
        self.state.last_render_error = None;
        self.state.last_workflow_message =
            Some("Inspecting TIFF input before package creation".to_owned());
    }

    pub(super) fn drain_tiff_import_setup_results(&mut self, ctx: &egui::Context) {
        let Some(message) =
            self.tiff_import_setup_task
                .as_ref()
                .and_then(|task| match task.receiver.try_recv() {
                    Ok(message) => Some(message),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(TiffImportSetupTaskMessage::Finished(Err(
                            "TIFF import setup worker stopped unexpectedly".to_owned(),
                        )))
                    }
                })
        else {
            return;
        };

        self.tiff_import_setup_task = None;
        match message {
            TiffImportSetupTaskMessage::Finished(Ok((options, inspection))) => {
                let grouping_confirmed = matches!(options.source, TiffImportSource::SingleFile(_));
                self.pending_tiff_import = Some(PendingTiffImport {
                    grouping_confirmed,
                    options,
                    inspection,
                    voxel_spacing_confirmed: false,
                });
                self.tiff_import_setup_error = None;
                self.state.last_render_error = None;
                self.state.last_workflow_message = Some("Review TIFF import settings".to_owned());
            }
            TiffImportSetupTaskMessage::Finished(Err(error)) => {
                self.pending_tiff_import = None;
                self.tiff_import_setup_error = Some(error.clone());
                self.state.last_render_error = Some(error.clone());
                self.state.last_workflow_message = Some("TIFF import setup failed".to_owned());
                tracing::error!(error = %error, "failed to configure TIFF import");
            }
        }
        ctx.request_repaint();
    }

    pub(super) fn start_pending_tiff_import(&mut self) {
        let Some(pending) = self.pending_tiff_import.take() else {
            return;
        };
        match validate_pending_tiff_import(&pending) {
            Ok(()) => {
                self.tiff_import_setup_error = None;
                let reviewed_plan = accepted_reviewed_plan_for_pending_tiff_import(&pending);
                let mut options = pending.options;
                options.reviewed_plan = reviewed_plan;
                self.start_import_task(options);
            }
            Err(err) => {
                self.pending_tiff_import = Some(pending);
                let error = err.to_string();
                self.tiff_import_setup_error = Some(error.clone());
                self.state.last_render_error = Some(error);
            }
        }
    }

    pub(super) fn cancel_pending_tiff_import(&mut self) {
        if self.tiff_import_setup_task.take().is_some() {
            self.state.last_workflow_message = Some("Cancelled TIFF import setup".to_owned());
        }
        if self.pending_tiff_import.take().is_some() {
            self.state.last_workflow_message = Some("Cancelled TIFF import setup".to_owned());
        }
    }

    pub(super) fn start_import_task(&mut self, options: TiffSourceImportOptions) {
        let source_path = options.source.path().to_path_buf();
        let output_package = options.output_package.clone();
        let cancellation = ImportCancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::channel();
        let progress_sender = sender.clone();
        let _worker = thread::spawn(move || {
            let result = import_tiff_source_with_progress(options, &worker_cancellation, |event| {
                progress_sender
                    .send(ImportTaskMessage::Progress(event))
                    .map_err(|_| ImportError::Cancelled)?;
                Ok(())
            });
            let _ = sender.send(ImportTaskMessage::Finished(result));
        });
        self.import_task = Some(ImportTask {
            cancellation,
            receiver,
            latest_event: None,
        });
        self.state.last_render_error = None;
        self.state.last_workflow_message = Some(format!(
            "Importing {} to {}",
            source_path.display(),
            output_package.display()
        ));
    }

    pub(super) fn cancel_import_task(&mut self) {
        if let Some(task) = &self.import_task {
            task.cancellation.cancel();
            self.state.last_workflow_message = Some("Cancelling import".to_owned());
        }
    }

    pub(super) fn drain_import_results(&mut self, ctx: &egui::Context) {
        let mut completion = None;
        let mut saw_progress = false;
        if let Some(task) = self.import_task.as_mut() {
            while let Ok(message) = task.receiver.try_recv() {
                match message {
                    ImportTaskMessage::Progress(event) => {
                        self.state.last_workflow_message = Some(import_progress_message(&event));
                        task.latest_event = Some(event);
                        saw_progress = true;
                    }
                    ImportTaskMessage::Finished(result) => {
                        completion = Some(result);
                    }
                }
            }
        }

        if let Some(result) = completion {
            self.import_task = None;
            match result {
                Ok(report) => self.finish_successful_import(report, ctx),
                Err(ImportError::Cancelled) => {
                    self.state.last_workflow_message = Some("Import cancelled".to_owned());
                    self.state.last_render_error = None;
                }
                Err(err) => {
                    self.state.last_render_error = Some(err.to_string());
                    tracing::error!(error = %err, "failed to import TIFF directory");
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
        report: TiffDirectoryImportReport,
        ctx: &egui::Context,
    ) {
        let output = report.output_package.clone();
        if let Err(err) = self.replace_state_from_dataset_path(output.clone(), Some(ctx)) {
            self.state.last_render_error = Some(err.to_string());
            tracing::error!(error = %err, "failed to open imported dataset");
            return;
        }
        self.state.last_workflow_message = Some(format!(
            "Imported {} channel(s), {} timepoint(s), {} z-plane(s) to {}",
            report.channel_count,
            report.timepoint_count,
            report.z_planes,
            output.display()
        ));
    }

    pub(super) fn show_tiff_import_setup_window(
        &mut self,
        ctx: &egui::Context,
        start_pending_tiff_import: &mut bool,
        cancel_pending_tiff_import: &mut bool,
        dismiss_setup_error: &mut bool,
    ) {
        if self.tiff_import_setup_task.is_none()
            && self.pending_tiff_import.is_none()
            && self.tiff_import_setup_error.is_none()
        {
            return;
        }

        egui::Window::new("TIFF Import")
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                if let Some(task) = &self.tiff_import_setup_task {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                    ui_kit::status_badge(ui, StatusTone::Warning, "inspecting input");
                    });
                    ui_kit::property_row(ui, "source", task.source.path().display());
                    ui_kit::property_row(ui, "output parent", task.output_parent.display());
                    ui_kit::property_row(ui, "package", "created after review");
                    ui.add_space(8.0);
                    if ui_kit::toolbar_button(ui, "Cancel Setup", true).clicked() {
                        *cancel_pending_tiff_import = true;
                    }
                    return;
                }

                if let Some(error) = self.tiff_import_setup_error.as_deref() {
                    ui_kit::status_badge(ui, StatusTone::Error, "setup failed");
                    ui.add_space(6.0);
                    ui.label(error);
                    ui.add_space(6.0);
                    ui.label(
                        "Expected grayscale uint8 or uint16 TIFF input: either one TIFF file or a directory of TIFF stacks.",
                    );
                    ui.add_space(8.0);
                    if ui_kit::toolbar_button(ui, "Dismiss", true).clicked() {
                        *dismiss_setup_error = true;
                    }
                    return;
                }

                if let Some(pending_import) = &mut self.pending_tiff_import {
                    ui_kit::status_badge(ui, StatusTone::Warning, "review settings");
                    ui.add_space(6.0);
                    ui_kit::property_row(
                        ui,
                        "source",
                        pending_import.options.source.path().display(),
                    );
                    ui_kit::property_row(
                        ui,
                        "output",
                        pending_import.options.output_package.display(),
                    );
                    ui_kit::property_row(
                        ui,
                        "files",
                        format!(
                            "{} file(s), {} channel(s), {} timepoint(s)",
                            pending_import.inspection.file_count,
                            pending_import.inspection.channel_count,
                            pending_import.inspection.timepoint_count
                        ),
                    );
                    ui_kit::property_row(
                        ui,
                        "source profile",
                        tiff_source_profile_label(pending_import.inspection.source_profile),
                    );
                    ui_kit::property_row(
                        ui,
                        "stack",
                        format!(
                            "z{} y{} x{}",
                            pending_import.inspection.shape.z,
                            pending_import.inspection.shape.y,
                            pending_import.inspection.shape.x
                        ),
                    );
                    ui_kit::property_row(
                        ui,
                        "source dtype",
                        format!("{:?}", pending_import.inspection.source_dtype),
                    );
                    show_tiff_no_data_controls(ui, pending_import);
                    ui_kit::property_row(
                        ui,
                        "value range",
                        format_tiff_value_range(pending_import.inspection.value_range),
                    );
                    ui_kit::property_row(
                        ui,
                        "metadata confidence",
                        format!("{:?}", pending_import.inspection.metadata_confidence),
                    );
                    ui_kit::property_row(
                        ui,
                        "spacing metadata",
                        tiff_voxel_spacing_metadata_label(&pending_import.inspection),
                    );
                    ui_kit::property_row(
                        ui,
                        "storage estimate",
                        tiff_import_storage_estimate_label(&pending_import.inspection),
                    );
                    ui.add_space(8.0);
                    ui.label("dataset name");
                    ui.add(
                        egui::TextEdit::singleline(&mut pending_import.options.dataset_name)
                            .desired_width(320.0),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label("voxel spacing um");
                        ui.add(
                            egui::DragValue::new(&mut pending_import.options.voxel_spacing_um[0])
                                .speed(0.01)
                                .prefix("x "),
                        );
                        ui.add(
                            egui::DragValue::new(&mut pending_import.options.voxel_spacing_um[1])
                                .speed(0.01)
                                .prefix("y "),
                        );
                        ui.add(
                            egui::DragValue::new(&mut pending_import.options.voxel_spacing_um[2])
                                .speed(0.01)
                                .prefix("z "),
                        );
                    });
                    show_tiff_channel_metadata_controls(
                        ui,
                        &mut pending_import.options,
                        &pending_import.inspection,
                        240.0,
                    );
                    show_tiff_grouping_controls(ui, pending_import);
                    ui.checkbox(
                        &mut pending_import.voxel_spacing_confirmed,
                        "voxel spacing reviewed",
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui_kit::toolbar_button(
                            ui,
                            "Start Import",
                            pending_tiff_import_ready_to_start(pending_import),
                        )
                        .clicked()
                        {
                            *start_pending_tiff_import = true;
                        }
                        if ui_kit::toolbar_button(ui, "Cancel Setup", true).clicked() {
                            *cancel_pending_tiff_import = true;
                        }
                    });
                }
            });
    }
}

//! Dataset-independent native application shell.

use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    thread::JoinHandle,
};

use eframe::egui;
use mirante4d_application::import_workflow::{
    ImportChannelSourceKind, ImportCommand, ImportRecoveryAction, ImportReviewId,
};
use mirante4d_dataset::DatasetSourceId;
use mirante4d_dataset_runtime::ProcessCpuBroker;
use mirante4d_import_pipeline::{
    ImportError, PublishedImport, TiffChannelSource, TiffSource, deterministic_tiff_destination,
    minimum_import_progress_bytes,
};
use mirante4d_settings::{ResourcePolicy, recommended_for_current_system};

use crate::{
    MiranteWorkbenchApp, ProcessTerminationLatch, current_settings_connection, gpu_memory,
    import_worker_service::{ImportWorkerCompletion, ImportWorkerOutcome},
    import_workflow::{ImportWorkflow, PendingImportRecovery, reset_checkpoint_directory},
    ui_kit, unified_source_open,
};

pub struct MiranteApplicationShell {
    egui_ctx: egui::Context,
    render_state: eframe::egui_wgpu::RenderState,
    selected_adapter_memory: gpu_memory::SelectedAdapterMemoryFacts,
    settings_connection: Option<current_settings_connection::CurrentSettingsConnection>,
    resource_policy: ResourcePolicy,
    cpu_broker: ProcessCpuBroker,
    process_termination: Arc<ProcessTerminationLatch>,
    workbench: Option<Box<MiranteWorkbenchApp>>,
    preprocessing: ImportWorkflow,
    preprocessing_ui: ui_kit::EguiUiState,
    opening: Option<OpeningDataset>,
    status: Option<String>,
}

struct OpeningDataset {
    path: PathBuf,
    result: mpsc::Receiver<anyhow::Result<unified_source_open::UnifiedOpenedSource>>,
    worker: Option<JoinHandle<()>>,
    previous_foreground_reserve_bytes: u64,
}

impl MiranteApplicationShell {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        initial_dataset: Option<PathBuf>,
        process_termination: Arc<ProcessTerminationLatch>,
    ) -> anyhow::Result<Self> {
        process_termination.bind_egui_context(&cc.egui_ctx);
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("the interactive viewer requires the WGPU renderer"))?
            .clone();
        let selected_adapter_memory =
            gpu_memory::SelectedAdapterMemoryFacts::discover(&render_state.adapter);
        let recommended =
            recommended_for_current_system(selected_adapter_memory.recommended_capacity_bytes())
                .unwrap_or_default();
        let (settings_connection, resource_policy) =
            current_settings_connection::CurrentSettingsConnection::start(recommended);
        let cpu_broker = ProcessCpuBroker::new(resource_policy.cpu_dataset_budget_bytes())
            .map_err(|code| anyhow::anyhow!("process CPU broker configuration failed: {code}"))?;
        let mut preprocessing = ImportWorkflow::new();
        let completion_ctx = cc.egui_ctx.clone();
        preprocessing
            .workers
            .set_completion_wake(move || completion_ctx.request_repaint());
        ui_kit::configure_visuals(&cc.egui_ctx);
        let mut shell = Self {
            egui_ctx: cc.egui_ctx.clone(),
            render_state,
            selected_adapter_memory,
            settings_connection: Some(settings_connection),
            resource_policy,
            cpu_broker,
            process_termination,
            workbench: None,
            preprocessing,
            preprocessing_ui: ui_kit::EguiUiState::new(
                resource_policy.cpu_dataset_budget_bytes(),
                resource_policy.gpu_budget_bytes(),
            ),
            opening: None,
            status: None,
        };
        if let Some(path) = initial_dataset {
            shell.start_open(path);
        }
        Ok(shell)
    }

    fn start_open(&mut self, path: PathBuf) {
        if self.opening.is_some() {
            return;
        }
        let policy = self.resource_policy;
        let broker = self.cpu_broker.clone();
        let previous_foreground_reserve_bytes = broker.foreground_reserve_bytes();
        let (sender, result) = mpsc::sync_channel(1);
        let worker_path = path.clone();
        match std::thread::Builder::new()
            .name("mirante4d-shell-source-open".to_owned())
            .spawn(move || {
                let opened = unified_source_open::open_with_broker(
                    &worker_path,
                    policy,
                    DatasetSourceId::new(1),
                    broker,
                );
                let _ = sender.send(opened);
            }) {
            Ok(worker) => {
                self.status = Some(format!("Opening {}", path.display()));
                self.opening = Some(OpeningDataset {
                    path,
                    result,
                    worker: Some(worker),
                    previous_foreground_reserve_bytes,
                });
            }
            Err(error) => {
                self.status = Some(format!("Dataset open worker could not start: {error}"));
            }
        }
    }

    fn start_open_published(&mut self, published: PublishedImport) {
        if self.opening.is_some() {
            self.status = Some(
                "The package was created, but another dataset open is already running.".to_owned(),
            );
            return;
        }
        let destination = published.destination().to_path_buf();
        let (_, transfer) = published.into_parts();
        let policy = self.resource_policy;
        let broker = self.cpu_broker.clone();
        let previous_foreground_reserve_bytes = broker.foreground_reserve_bytes();
        let (sender, result) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("mirante4d-shell-published-open".to_owned())
            .spawn(move || {
                let opened = (|| -> anyhow::Result<_> {
                    let (capability, _) = transfer.consume(|| false).map_err(|error| {
                        anyhow::anyhow!("published package transfer failed: {error}")
                    })?;
                    let opened =
                        unified_source_open::open_published_with_broker(policy, capability, broker)
                            .map_err(|error| {
                                anyhow::anyhow!("published package open failed: {error:?}")
                            })?;
                    unified_source_open::prepare_published_current_source(opened)
                })();
                let _ = sender.send(opened);
            }) {
            Ok(worker) => {
                self.status = Some(format!("Opening {}", destination.display()));
                self.opening = Some(OpeningDataset {
                    path: destination,
                    result,
                    worker: Some(worker),
                    previous_foreground_reserve_bytes,
                });
            }
            Err(error) => {
                self.status = Some(format!(
                    "The package was created at {}, but its open worker could not start: {error}",
                    destination.display()
                ));
            }
        }
    }

    fn poll_open(&mut self) {
        let Some(opening) = self.opening.as_mut() else {
            return;
        };
        let outcome = match opening.result.try_recv() {
            Ok(outcome) => outcome,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(anyhow::anyhow!("dataset open worker stopped unexpectedly"))
            }
        };
        let mut opening = self.opening.take().expect("a completed open is active");
        if let Some(worker) = opening.worker.take() {
            let _ = worker.join();
        }
        match outcome {
            Ok(opened) => {
                let settings = self.settings_connection.take().unwrap_or_else(|| {
                    let recommended = recommended_for_current_system(
                        self.selected_adapter_memory.recommended_capacity_bytes(),
                    )
                    .unwrap_or_default();
                    current_settings_connection::CurrentSettingsConnection::start(recommended).0
                });
                match MiranteWorkbenchApp::new_with_settings(
                    self.egui_ctx.clone(),
                    self.render_state.clone(),
                    opened,
                    settings,
                    self.resource_policy,
                    self.selected_adapter_memory.clone(),
                    Some(Arc::clone(&self.process_termination)),
                    self.cpu_broker.clone(),
                ) {
                    Ok(workbench) => {
                        self.workbench = Some(Box::new(workbench));
                        self.status = None;
                    }
                    Err(error) => {
                        let _ = self
                            .cpu_broker
                            .set_foreground_reserve(opening.previous_foreground_reserve_bytes);
                        self.restore_settings_connection();
                        self.status = Some(format!(
                            "Could not initialize {}: {error}",
                            opening.path.display()
                        ));
                    }
                }
            }
            Err(error) => {
                let _ = self
                    .cpu_broker
                    .set_foreground_reserve(opening.previous_foreground_reserve_bytes);
                self.status = Some(format!(
                    "Could not open {}: {error}",
                    opening.path.display()
                ));
            }
        }
    }

    fn restore_settings_connection(&mut self) {
        if self.settings_connection.is_some() {
            return;
        }
        let recommended = recommended_for_current_system(
            self.selected_adapter_memory.recommended_capacity_bytes(),
        )
        .unwrap_or_default();
        let (connection, policy) =
            current_settings_connection::CurrentSettingsConnection::start(recommended);
        self.settings_connection = Some(connection);
        self.resource_policy = policy;
    }

    fn show_welcome(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.poll_open();
        self.drain_preprocessing_results(&ctx);
        let workflow = self.preprocessing.snapshot();
        let workflow_busy = !matches!(
            workflow,
            mirante4d_application::import_workflow::ImportWorkflowSnapshot::Idle
        );
        let action = ui_kit::show_welcome_shell(
            ui,
            self.status.as_deref(),
            self.opening.is_some() || workflow_busy,
        );
        match action {
            Some(ui_kit::WelcomeShellAction::LoadPreprocessedDataset) => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Open Mirante4D dataset package")
                    .pick_folder()
                {
                    self.start_open(path);
                }
            }
            Some(ui_kit::WelcomeShellAction::PreprocessNewDataset) => {
                self.status = None;
                self.preprocessing.begin_setup();
            }
            None => {}
        }
        let commands = ui_kit::show_import_workflow_window(
            &ctx,
            &mut self.preprocessing_ui,
            &self.preprocessing.snapshot(),
        );
        for command in commands {
            self.apply_preprocessing_command(command, &ctx);
        }
        if self.opening.is_some() || self.preprocessing.workers.status().is_active() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    fn apply_preprocessing_command(&mut self, command: ImportCommand, ctx: &egui::Context) {
        match command {
            ImportCommand::BeginSetup => self.preprocessing.begin_setup(),
            ImportCommand::SetChannelCount { count } => self.preprocessing.set_channel_count(count),
            ImportCommand::SetChannelLabel { channel, label } => {
                self.preprocessing.set_channel_label(channel, label)
            }
            ImportCommand::SetChannelSourceKind { channel, kind } => {
                self.preprocessing.set_channel_kind(channel, kind)
            }
            ImportCommand::ChooseChannelSource { channel } => {
                self.choose_preprocessing_channel_source(channel)
            }
            ImportCommand::ValidateChannels => self.validate_preprocessing_channels(),
            ImportCommand::CancelSetup => self.preprocessing.cancel_setup(),
            ImportCommand::CancelInspection => {
                self.preprocessing.workers.cancel_inspection();
            }
            ImportCommand::Start { review_id, draft } => {
                match self.preprocessing.start_options(review_id, draft) {
                    Ok(Some(options)) => {
                        match minimum_import_progress_bytes(&options)
                            .map_err(|error| error.to_string())
                            .and_then(|bytes| {
                                self.cpu_broker
                                    .reserve_import_progress(bytes)
                                    .map_err(|error| {
                                        format!(
                                            "Preprocessing cannot start because its minimum complete progress path needs {bytes} managed CPU bytes: {error}"
                                        )
                                    })
                            }) {
                            Ok(progress_reservation) => {
                                if let Err(error) = self.preprocessing.workers.start_shell_import(
                                    review_id,
                                    options,
                                    progress_reservation,
                                ) {
                                    self.preprocessing.problem = Some(error.to_string());
                                } else {
                                    self.preprocessing.complete_review(review_id);
                                    self.preprocessing.checkpoint_recovery = None;
                                }
                            }
                            Err(error) => self.preprocessing.problem = Some(error),
                        }
                    }
                    Ok(None) => {}
                    Err(error) => self.preprocessing.problem = Some(error.to_string()),
                }
            }
            ImportCommand::CancelReview { review_id } => {
                self.preprocessing.cancel_review(review_id)
            }
            ImportCommand::CancelImport => {
                self.preprocessing.workers.cancel_import();
            }
            ImportCommand::DismissProblem => {
                self.preprocessing.problem = None;
                self.preprocessing.checkpoint_recovery = None;
            }
            ImportCommand::RecoverCheckpoint { retry_id, action } => {
                self.recover_preprocessing_checkpoint(retry_id, action)
            }
        }
        ctx.request_repaint();
    }

    fn choose_preprocessing_channel_source(&mut self, channel: usize) {
        if self.preprocessing.workers.status().is_active() {
            return;
        }
        let Some(row) = self
            .preprocessing
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
        let label = row.label.clone();
        let kind = row.source_kind;
        self.preprocessing
            .install_channel_selection(channel, path.clone());
        let source = match kind {
            ImportChannelSourceKind::Single3dTiff => TiffChannelSource::single_3d(label, path),
            ImportChannelSourceKind::FolderOf3dTiffs => {
                TiffChannelSource::folder_of_3d(label, path)
            }
            ImportChannelSourceKind::FolderOf2dTiffs => {
                TiffChannelSource::folder_of_2d(label, path)
            }
        }
        .and_then(|source| TiffSource::new(vec![source]));
        match source {
            Ok(source) => {
                if self
                    .preprocessing
                    .workers
                    .start_inspection(source, PathBuf::new())
                    .is_ok()
                {
                    self.preprocessing.mark_channel_inspection_active(channel);
                }
            }
            Err(error) => {
                if let Some(row) = self
                    .preprocessing
                    .setup
                    .as_mut()
                    .and_then(|setup| setup.channels.get_mut(channel))
                {
                    row.error = Some(error.to_owned());
                }
            }
        }
    }

    fn validate_preprocessing_channels(&mut self) {
        let inspection = match self.preprocessing.validated_setup_inspection() {
            Ok(inspection) => inspection,
            Err(error) => {
                if let Some(setup) = self.preprocessing.setup.as_mut() {
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
        match self
            .preprocessing
            .install_review(source, inspection, destination)
        {
            Ok(_) => self.preprocessing.setup = None,
            Err(error) => {
                if let Some(setup) = self.preprocessing.setup.as_mut() {
                    setup.validation_error = Some(error.to_string());
                }
            }
        }
    }

    fn drain_preprocessing_results(&mut self, ctx: &egui::Context) {
        let Some(completion) = self.preprocessing.workers.poll_completion() else {
            return;
        };
        match completion {
            ImportWorkerCompletion::Inspection(completion) => {
                let completion = *completion;
                match completion.outcome {
                    ImportWorkerOutcome::Finished(Ok(inspection))
                        if !completion.cancellation_requested =>
                    {
                        if let Some(channel) = self.preprocessing.take_current_inspection_channel()
                            && let Some(row) = self
                                .preprocessing
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
                        self.preprocessing.active_setup_inspection = None;
                    }
                    ImportWorkerOutcome::Finished(Err(error)) => {
                        let setup_active = self.preprocessing.setup.is_some();
                        if let Some(channel) = self.preprocessing.take_current_inspection_channel()
                            && let Some(row) = self
                                .preprocessing
                                .setup
                                .as_mut()
                                .and_then(|setup| setup.channels.get_mut(channel))
                        {
                            row.inspection = None;
                            row.error = Some(error.to_string());
                        } else if !setup_active {
                            self.preprocessing.problem = Some(error.to_string());
                        }
                    }
                    ImportWorkerOutcome::WorkerStopped => {
                        self.preprocessing.active_setup_inspection = None;
                        self.preprocessing.problem =
                            Some("TIFF inspection worker stopped unexpectedly".to_owned());
                    }
                }
            }
            ImportWorkerCompletion::Import(completion) => {
                let completion = *completion;
                match completion.outcome {
                    ImportWorkerOutcome::Finished(Ok(published)) => {
                        self.preprocessing.problem = None;
                        self.preprocessing.checkpoint_recovery = None;
                        self.start_open_published(published);
                    }
                    ImportWorkerOutcome::Finished(Err(ImportError::Cancelled)) => {
                        self.preprocessing.problem = None;
                        self.preprocessing.checkpoint_recovery = None;
                    }
                    ImportWorkerOutcome::Finished(Err(ImportError::InvalidCheckpoint(reason))) => {
                        self.preprocessing.problem = Some(format!(
                            "The saved import checkpoint is corrupt or belongs to different inputs: {reason}. Confirm Reset and Restart below to remove only that checkpoint and retry."
                        ));
                        self.preprocessing.checkpoint_recovery =
                            completion
                                .retry_options
                                .map(|options| PendingImportRecovery {
                                    id: completion.review_id,
                                    options,
                                    action: ImportRecoveryAction::ResetAndRestart,
                                });
                    }
                    ImportWorkerOutcome::Finished(Err(ImportError::CapacityPaused {
                        required_bytes,
                        available_bytes,
                    })) => {
                        self.preprocessing.problem = Some(format!(
                            "Preprocessing paused before the next durable step because it needs {required_bytes} additional filesystem bytes and {available_bytes} are available. Free space, then Resume; the saved checkpoint will not be deleted."
                        ));
                        self.preprocessing.checkpoint_recovery =
                            completion
                                .retry_options
                                .map(|options| PendingImportRecovery {
                                    id: completion.review_id,
                                    options,
                                    action: ImportRecoveryAction::Resume,
                                });
                    }
                    ImportWorkerOutcome::Finished(Err(error)) => {
                        self.preprocessing.problem = Some(error.to_string());
                        self.preprocessing.checkpoint_recovery = None;
                    }
                    ImportWorkerOutcome::WorkerStopped => {
                        self.preprocessing.problem =
                            Some("TIFF import worker stopped unexpectedly".to_owned());
                        self.preprocessing.checkpoint_recovery = None;
                    }
                }
            }
        }
        ctx.request_repaint();
    }

    fn recover_preprocessing_checkpoint(
        &mut self,
        retry_id: ImportReviewId,
        action: ImportRecoveryAction,
    ) {
        let Some(recovery) = self.preprocessing.checkpoint_recovery.take() else {
            return;
        };
        if recovery.id != retry_id || recovery.action != action {
            self.preprocessing.checkpoint_recovery = Some(recovery);
            return;
        }
        let options = recovery.options.clone();
        if action == ImportRecoveryAction::ResetAndRestart
            && let Err(error) = reset_checkpoint_directory(&options.checkpoint_directory)
        {
            self.preprocessing.problem = Some(format!(
                "The checkpoint was not reset, so nothing was restarted: {error}"
            ));
            self.preprocessing.checkpoint_recovery = Some(recovery);
            return;
        }
        let reservation = minimum_import_progress_bytes(&options)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                self.cpu_broker
                    .reserve_import_progress(bytes)
                    .map_err(|error| {
                        format!(
                            "Preprocessing cannot continue because its minimum complete progress path needs {bytes} managed CPU bytes: {error}"
                        )
                    })
            });
        match reservation {
            Ok(progress_reservation) => match self.preprocessing.workers.start_shell_import(
                retry_id,
                options,
                progress_reservation,
            ) {
                Ok(()) => self.preprocessing.problem = None,
                Err(error) => {
                    self.preprocessing.problem = Some(error.to_string());
                    self.preprocessing.checkpoint_recovery = Some(recovery);
                }
            },
            Err(error) => {
                self.preprocessing.problem = Some(error);
                self.preprocessing.checkpoint_recovery = Some(recovery);
            }
        }
    }
}

impl eframe::App for MiranteApplicationShell {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(workbench) = self.workbench.as_mut() {
            workbench.raw_input_hook(ctx, raw_input);
        }
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(workbench) = self.workbench.as_mut() {
            workbench.logic(ctx, frame);
        } else if self.process_termination.requested() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if let Some(workbench) = self.workbench.as_mut() {
            workbench.ui(ui, frame);
        } else {
            self.show_welcome(ui);
        }
    }

    fn on_exit(&mut self) {
        if let Some(workbench) = self.workbench.as_mut() {
            workbench.on_exit();
        }
        self.preprocessing.workers.shutdown();
        self.cpu_broker.shutdown();
    }
}

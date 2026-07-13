#[cfg(test)]
use std::collections::BTreeMap;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

mod analysis_workspace;
mod cross_section_readout;
mod cross_section_runtime;
mod cross_section_scheduler;
mod current_egui_shell_bridge;
mod current_project_persistence_bridge;
mod current_runtime;
mod current_settings_connection;
mod current_source_open_service;
mod current_source_verification_service;
mod dataset_demand_plan;
mod dataset_requests;
mod diagnostics;
mod display_graph;
mod display_identity;
mod display_refresh;
mod fidelity;
mod histogram;
mod image_compositing;
mod import_ui;
mod layer_state;
mod lod_scheduler;
mod playback;
mod product_automation;
mod render_state;
mod resident_rendering;
mod runtime_diagnostics_panel;
mod scene_artifacts;
mod semantic_tiles;
mod smoke;
mod state;
mod tool_interactions;
mod tools;
mod transfer_presets;
mod ui_kit;
mod unified_source_open;
mod viewer_layout;
mod viewport;
mod workbench_brick_runtime;
mod workbench_controls;
mod workbench_import;
mod workbench_playback_runtime;
mod workbench_ui;

#[cfg(test)]
use analysis_workspace::{
    AnalysisPlotBounds, analysis_plot_bounds, analysis_plot_visible_bounds,
    analysis_table_preview_rows, nearest_analysis_plot_point,
    normalize_analysis_plot_view_for_plot, pan_analysis_plot_view, plot_screen_position,
    zoom_analysis_plot_view,
};
use analysis_workspace::{
    AnalysisPlotViewRange, AnalysisTableExportInput, AnalysisTableSort, AnalysisWorkspaceViewInput,
    export_selected_analysis_table, show_analysis_workspace, show_analysis_workspace_window,
};
use cross_section_readout::cross_section_hover_readout_for_response;
use cross_section_runtime::CrossSectionRuntime;
pub use diagnostics::{StartupDiagnostics, collect_startup_diagnostics, default_log_path};
use display_identity::GpuDisplayedFrameIdentity;
use display_refresh::{DisplayRefreshTiming, ViewportDisplayImage, duration_ms};
use eframe::egui;
use fidelity::{
    channel_fidelity_label, composite_fidelity_label, format_adapter_summary,
    iso_shading_policy_label, render_sampling_policy_label, show_frame_fidelity_property_rows,
    visible_channel_fidelity_is_mixed,
};
#[cfg(test)]
use fidelity::{
    frame_completeness_label, frame_failure_kind_label, frame_fidelity_label, frame_reason_label,
};
use histogram::{
    active_layer_histogram_summary, auto_dense_window_from_histogram,
    auto_dvr_opacity_transfer_from_histogram, auto_signal_window_from_histogram,
    histogram_bins_label, histogram_can_auto_window, histogram_status_label,
};
pub use image_compositing::mip_to_color_image;
use import_ui::{
    ImportTask, ImportTaskMessage, PendingTiffImport, TiffImportSetupTask,
    TiffImportSetupTaskMessage, accepted_reviewed_plan_for_pending_tiff_import,
    active_layer_no_data_policy_label, format_tiff_value_range, import_progress_fraction,
    import_task_status_text, pending_tiff_import_ready_to_start, prepare_tiff_source_import,
    show_tiff_channel_metadata_controls, show_tiff_grouping_controls, show_tiff_no_data_controls,
    tiff_import_storage_estimate_label, tiff_source_profile_label,
    tiff_voxel_spacing_metadata_label, validate_pending_tiff_import,
};
use mirante4d_analysis::IntensitySummary;
#[cfg(test)]
use mirante4d_analysis::{
    AnalysisCell, AnalysisExecutionClass, AnalysisPlot, AnalysisProvenance, AnalysisResultState,
    AnalysisTable,
};
#[cfg(test)]
use mirante4d_application::{AnalysisTableDescriptor, AnalysisTableId};
use mirante4d_application::{
    ApplicationCommand, ApplicationEvent, ApplicationFault, ApplicationFaultCode,
    ApplicationSnapshot, ApplicationState, CommandEffect, OperationCompletion,
    OperationFailureCode, OperationKind, OperationToken, SourceSessionGeneration,
    SourceVerificationSnapshot, WorkspaceSnapshot,
};
use mirante4d_dataset::DatasetSourceId;
use mirante4d_domain::{
    CameraView, CrossSectionView, DisplayWindow, DvrOpacityTransfer as CanonicalDvrOpacityTransfer,
    IsoLightState, IsoShadingPolicy, LayerTransfer, RenderState as CanonicalRenderState, RgbColor,
    SamplingPolicy, ScaleLevel, TRANSFER_GAMMA_MAX, TRANSFER_GAMMA_MIN, TransferCurve,
    UnitQuaternion, ViewerLayout as CanonicalViewerLayout, WorldPoint3,
};
#[cfg(test)]
use mirante4d_domain::{IntensityDType, RenderMode, Shape3D, TimeIndex};
use mirante4d_import::{
    ImportCancellationToken, ImportError, TiffDirectoryImportReport, TiffImportSource,
    TiffSourceImportOptions, import_tiff_source_with_progress,
};
use mirante4d_project_model::{ChannelPreset, LayerViewState, ViewState};
use mirante4d_render_api::PresentationViewport;
use mirante4d_renderer::gpu::{GpuDisplayFrame, GpuRenderer};
#[cfg(test)]
use mirante4d_renderer::{PixelCoverage, RenderViewport};
use mirante4d_settings::{RejectedFileDisposition, ResourcePolicy, recommended_for_current_system};
use playback::playback_status_label;
#[cfg(test)]
use playback::stepped_timepoint;
use product_automation::{ProductAutomationAppUpdateTiming, ProductAutomationController};
#[cfg(test)]
use render_state::placeholder_frame_for_mode;
use render_state::{set_presentation_viewport, set_render_viewport, take_lod_replan_pending};
use resident_rendering::{
    render_gpu_cross_section_panel_frame_from_global_runtime,
    render_gpu_display_frame_from_resident_bricks,
};
use scene_artifacts::show_scene_artifacts_editor;
pub use smoke::{AppSmokeOptions, AppSmokeReport, PlaybackSmokeFrame, run_headless_smoke};
pub use state::{
    ChannelFidelityStatus, ChannelFidelityWarning, DisplayedFrameFreshness, FrameCompleteness,
    FrameFailureKind, FrameFidelityStatus, HistogramStatus, LayerHistogramSummary,
    LodDecisionReason, LodScheduleState, RenderBackend, ViewportHover, ViewportIntensity,
};
use tool_interactions::apply_viewport_tool_response;
use tools::{ViewerTool, ViewerToolState};
use transfer_presets::{
    built_in_transfer_preset_curve, built_in_transfer_preset_label, built_in_transfer_presets,
    channel_preset_from_current_view, next_user_channel_preset_id,
};
use ui_kit::{StatusTone, WorkbenchLayoutSpec};
use viewport::{
    default_camera_for_shape, fit_camera_to_shape_preserving_view, fit_size,
    presentation_viewport_for_display_size, render_viewport_for_display_size,
    viewport_hover_from_response, viewport_interaction_commands,
};
use workbench_controls::{
    dataset_path_status_label, projection_selector, render_mode_label, render_mode_selector,
    request_background_work_repaint, request_background_work_repaint_after, show_playback_controls,
};

const BACKGROUND_WORK_REPAINT_INTERVAL: Duration = Duration::from_millis(50);
pub(crate) const CROSS_SECTION_INTERACTION_SETTLE_DURATION: Duration = Duration::from_millis(120);
const DVR_DENSITY_SCALE_MIN: f64 = 0.1;
const DVR_DENSITY_SCALE_MAX: f64 = 64.0;
const DEFAULT_DVR_DENSITY_SCALE: f64 = 12.0;
const DEFAULT_ISO_DISPLAY_LEVEL: f32 = 0.5;
const DEFAULT_DVR_OPACITY_GAMMA: f32 = 0.25;
const CROSS_SECTION_FAST_SLICE_MULTIPLIER: f64 = 10.0;
const CROSS_SECTION_ROTATE_RADIANS_PER_POINT: f64 = 0.005;
const MIB: u64 = 1024 * 1024;

fn bytes_to_mib_rounded(bytes: u64) -> u64 {
    (bytes.saturating_add(MIB / 2) / MIB).max(1)
}

fn mib_to_bytes(mib: u64) -> u64 {
    mib.saturating_mul(MIB)
}

fn application_view(snapshot: &ApplicationSnapshot) -> &ViewState {
    match snapshot.workspace() {
        WorkspaceSnapshot::Unbound { workspace } => workspace.view(),
        WorkspaceSnapshot::Bound { project, .. } => project.view(),
    }
}

fn project_failure_code(
    operation: OperationKind,
    error: &current_project_persistence_bridge::ProjectPersistenceError,
) -> OperationFailureCode {
    use current_project_persistence_bridge::ProjectPersistenceError as Error;
    match error {
        Error::UnsupportedSchema | Error::UnsupportedSchemaVersion => {
            OperationFailureCode::ProjectUnsupportedSchema
        }
        Error::InvalidDocument { .. }
        | Error::InvalidValue { .. }
        | Error::ExistingTargetRejected { .. }
        | Error::ReadbackMismatch
        | Error::UnsafeSymlink
        | Error::DocumentTooLarge { .. } => OperationFailureCode::ProjectInvalidDocument,
        Error::CommitIndeterminate { .. } => OperationFailureCode::ProjectCommitIndeterminate,
        Error::PreCommitIo { kind, .. } if *kind == std::io::ErrorKind::PermissionDenied => {
            OperationFailureCode::ProjectPermissionDenied
        }
        Error::PreCommitIo { kind, .. }
            if *kind == std::io::ErrorKind::NotFound && operation == OperationKind::ProjectOpen =>
        {
            OperationFailureCode::ProjectNotFound
        }
        Error::PreCommitIo { .. }
        | Error::ActorQueueFull
        | Error::ActorUnavailable
        | Error::ActorThreadPanicked
        | Error::DuplicateOperationToken
        | Error::UnknownOperationToken
        | Error::ProjectPathUnavailable => match operation {
            OperationKind::ProjectOpen => OperationFailureCode::ProjectReadFailed,
            _ => OperationFailureCode::ProjectWriteFailed,
        },
    }
}

pub struct MiranteWorkbenchApp {
    application: ApplicationState,
    startup_diagnostics: StartupDiagnostics,
    dataset: dataset_requests::DatasetDemandState,
    render_runtime: current_runtime::render::CurrentRenderRuntime,
    ui_runtime: current_runtime::ui::CurrentUiRuntime,
    project_runtime: current_runtime::project::CurrentProjectRuntime,
    import_runtime: current_runtime::import::CurrentImportRuntime,
    analysis_runtime: current_runtime::analysis::CurrentAnalysisRuntime,
    validation_runtime: current_runtime::validation::CurrentValidationRuntime,
    project_persistence:
        Option<current_project_persistence_bridge::CurrentProjectPersistenceBridge>,
    settings_connection: current_settings_connection::CurrentSettingsConnection,
    source_open_service: Option<current_source_open_service::CurrentSourceOpenService>,
    source_verification_service:
        Option<current_source_verification_service::CurrentSourceVerificationService>,
    pending_automatic_source_verification: Option<SourceSessionGeneration>,
}

struct CrossSectionPanelGpuDisplayFrame {
    generation: u64,
    frame: GpuDisplayFrame,
    texture_id: egui::TextureId,
}

impl MiranteWorkbenchApp {
    pub fn open_dataset(
        cc: &eframe::CreationContext<'_>,
        path: impl AsRef<Path>,
    ) -> anyhow::Result<Self> {
        let (settings_connection, resource_policy) =
            current_settings_connection::CurrentSettingsConnection::start();
        let opened = unified_source_open::open(path, resource_policy, DatasetSourceId::new(1))?;
        Self::new_with_settings(cc, opened, settings_connection, resource_policy)
    }

    fn new_with_settings(
        cc: &eframe::CreationContext<'_>,
        opened: unified_source_open::UnifiedOpenedSource,
        settings_connection: current_settings_connection::CurrentSettingsConnection,
        resource_policy: ResourcePolicy,
    ) -> anyhow::Result<Self> {
        ui_kit::configure_visuals(&cc.egui_ctx);
        let unified_source_open::UnifiedOpenedSource {
            mut startup_diagnostics,
            catalog,
            workspace,
            dataset,
            mut render_runtime,
            analysis_runtime,
        } = opened;
        let application = ApplicationState::new_unbound(
            SourceSessionGeneration::new(1),
            catalog.as_ref().clone(),
            workspace,
            resource_policy,
        )
        .map_err(|code| anyhow::anyhow!("initial application state rejected: {code:?}"))?;
        let runtime_policy = resource_policy.current_runtime_adapter();
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("the interactive viewer requires the WGPU renderer"))?;
        let wgpu_texture_renderer = Some(render_state.renderer.clone());
        let gpu_renderer_result = GpuRenderer::from_existing_device_with_cache_budgets(
            &render_state.adapter,
            render_state.device.clone(),
            render_state.queue.clone(),
            runtime_policy.gpu_dense_cache_budget_bytes(),
            runtime_policy.gpu_brick_cache_budget_bytes(),
        );
        let renderer = gpu_renderer_result
            .map_err(|error| anyhow::anyhow!("a working GPU renderer is required: {error}"))?;
        let adapter_summary = format_adapter_summary(&renderer);
        startup_diagnostics.gpu_adapter = Some(adapter_summary);
        let gpu_renderer = Arc::new(renderer);
        render_runtime.gpu_renderer = Some(Arc::clone(&gpu_renderer));
        let ui_runtime =
            current_runtime::ui::CurrentUiRuntime::new(resource_policy, wgpu_texture_renderer);
        let mut app = Self {
            application,
            startup_diagnostics,
            dataset,
            render_runtime,
            ui_runtime,
            project_runtime: current_runtime::project::CurrentProjectRuntime::unbound(),
            import_runtime: current_runtime::import::CurrentImportRuntime::idle(),
            analysis_runtime,
            validation_runtime:
                current_runtime::validation::CurrentValidationRuntime::from_environment(),
            project_persistence: Some(
                current_project_persistence_bridge::CurrentProjectPersistenceBridge::spawn()?,
            ),
            settings_connection,
            source_open_service: Some(current_source_open_service::CurrentSourceOpenService::new()),
            source_verification_service: Some(
                current_source_verification_service::CurrentSourceVerificationService::new(),
            ),
            pending_automatic_source_verification: None,
        };
        app.request_current_source_verification();
        app.pump_application_services();
        app.request_opened_state_visible_work(Some(&cc.egui_ctx));
        Ok(app)
    }

    fn show_runtime_diagnostics_body(&self, ui: &mut egui::Ui) {
        runtime_diagnostics_panel::show_runtime_diagnostics_body(self, ui);
    }

    fn diagnostics_summary_text(&self) -> String {
        runtime_diagnostics_panel::diagnostics_summary_text(self)
    }

    fn show_settings_body(&mut self, ui: &mut egui::Ui) {
        let mut cpu_dataset_mib = bytes_to_mib_rounded(
            self.ui_runtime
                .settings_runtime_draft
                .cpu_dataset_budget_bytes,
        );
        if ui
            .add(
                egui::DragValue::new(&mut cpu_dataset_mib)
                    .range(2_048..=32_768)
                    .speed(64)
                    .suffix(" MiB"),
            )
            .on_hover_text("total CPU dataset ledger")
            .changed()
        {
            self.ui_runtime
                .settings_runtime_draft
                .cpu_dataset_budget_bytes = mib_to_bytes(cpu_dataset_mib);
        }
        ui_kit::property_row(ui, "CPU dataset MiB", cpu_dataset_mib.to_string());

        let mut gpu_mib =
            bytes_to_mib_rounded(self.ui_runtime.settings_runtime_draft.gpu_budget_bytes);
        if ui
            .add(
                egui::DragValue::new(&mut gpu_mib)
                    .range(1_024..=8_192)
                    .speed(256)
                    .suffix(" MiB"),
            )
            .on_hover_text("total GPU ledger")
            .changed()
        {
            self.ui_runtime.settings_runtime_draft.gpu_budget_bytes = mib_to_bytes(gpu_mib);
        }
        ui_kit::property_row(ui, "GPU MiB", gpu_mib.to_string());

        let policy = ResourcePolicy::new(
            self.ui_runtime
                .settings_runtime_draft
                .cpu_dataset_budget_bytes,
            self.ui_runtime.settings_runtime_draft.gpu_budget_bytes,
        )
        .ok();
        let pending = self.settings_connection.pending().is_some();
        ui.horizontal(|ui| {
            if ui_kit::toolbar_button(ui, "Save Settings", policy.is_some() && !pending).clicked()
                && let Some(policy) = policy
            {
                self.request_resource_policy_change(policy, RejectedFileDisposition::Preserve);
            }
            if ui_kit::toolbar_button(ui, "Recommended", !pending).clicked()
                && let Ok(policy) = recommended_for_current_system(None)
            {
                self.ui_runtime.settings_runtime_draft = policy.into();
            }
        });
        if self.settings_connection.rejected_file_present()
            && ui_kit::toolbar_button(
                ui,
                "Replace Rejected Settings",
                policy.is_some() && !pending,
            )
            .clicked()
            && let Some(policy) = policy
        {
            self.request_resource_policy_change(policy, RejectedFileDisposition::ReplaceExplicitly);
        }
        ui_kit::property_row(
            ui,
            "settings status",
            if pending {
                "saving; restart required when complete".to_owned()
            } else {
                format!("{:?}", self.settings_connection.startup_status())
            },
        );
        if policy.is_none() {
            ui_kit::status_badge(
                ui,
                StatusTone::Error,
                "resource budget is outside valid bounds",
            );
        }
    }

    fn request_resource_policy_change(
        &mut self,
        policy: ResourcePolicy,
        rejected_file_disposition: RejectedFileDisposition,
    ) {
        if let Err(fault) = current_egui_shell_bridge::dispatch(
            &mut self.application,
            ApplicationCommand::RequestResourcePolicyChange {
                policy,
                rejected_file_disposition,
            },
        ) {
            tracing::warn!(?fault, "resource policy command rejected");
        }
        self.pump_application_services();
    }

    fn pump_application_services(&mut self) {
        for _ in 0..4 {
            let mut completion_commands = self.settings_connection.poll();
            let events = current_egui_shell_bridge::drain_events(&mut self.application, 256);
            for event in &events {
                if let Some(command) = self.settings_connection.observe_application_event(event) {
                    completion_commands.push(command);
                }
                self.observe_source_application_event(event);
                self.observe_project_application_event(event);
            }
            let had_completion_commands = !completion_commands.is_empty();
            for command in completion_commands {
                if let Err(fault) =
                    current_egui_shell_bridge::dispatch(&mut self.application, command)
                {
                    tracing::warn!(?fault, "application service completion rejected");
                }
            }
            // Drain first so a full bounded event queue cannot prevent a
            // terminal actor result from retiring its operation.
            self.poll_source_open_service();
            self.poll_source_verification_service();
            self.poll_project_persistence();
            self.try_start_pending_automatic_source_verification();
            if events.is_empty()
                && !had_completion_commands
                && current_egui_shell_bridge::snapshot(&self.application).pending_event_count() == 0
            {
                break;
            }
        }
    }

    fn observe_project_application_event(&mut self, event: &ApplicationEvent) {
        match event {
            ApplicationEvent::ProjectOpenRequested { token } => {
                let Some(path) = rfd::FileDialog::new()
                    .set_title("Open Mirante4D project package")
                    .pick_folder()
                else {
                    self.complete_project_operation(token.clone(), OperationCompletion::Cancelled);
                    return;
                };
                let request = self
                    .project_persistence
                    .as_ref()
                    .ok_or(
                        current_project_persistence_bridge::ProjectPersistenceError::ActorUnavailable,
                    )
                    .and_then(|bridge| bridge.request_open(token.clone(), path));
                if let Err(error) = request {
                    self.complete_project_operation(
                        token.clone(),
                        OperationCompletion::Failed(project_failure_code(token.kind(), &error)),
                    );
                }
            }
            ApplicationEvent::ProjectSaveRequested { token, projection } => {
                let path = self
                    .project_runtime
                    .current_project_path
                    .clone()
                    .or_else(|| {
                        rfd::FileDialog::new()
                            .set_title("Save Mirante4D project package")
                            .add_filter("Mirante4D project", &["m4dproj"])
                            .set_file_name("project.m4dproj")
                            .save_file()
                    });
                let Some(path) = path else {
                    self.complete_project_operation(token.clone(), OperationCompletion::Cancelled);
                    return;
                };
                let request = self
                    .project_persistence
                    .as_ref()
                    .ok_or(
                        current_project_persistence_bridge::ProjectPersistenceError::ActorUnavailable,
                    )
                    .and_then(|bridge| {
                        bridge.request_save(token.clone(), path, Arc::clone(projection))
                    });
                if let Err(error) = request {
                    self.complete_project_operation(
                        token.clone(),
                        OperationCompletion::Failed(project_failure_code(token.kind(), &error)),
                    );
                }
            }
            ApplicationEvent::OperationCancellationRequested { token }
                if matches!(
                    token.kind(),
                    OperationKind::ProjectOpen | OperationKind::ProjectSave
                ) =>
            {
                if let Some(bridge) = self.project_persistence.as_ref() {
                    match bridge.cancel(token.clone()) {
                        Ok(())
                        | Err(
                            current_project_persistence_bridge::ProjectPersistenceError::UnknownOperationToken,
                        ) => {}
                        Err(error) => {
                            tracing::warn!(%error, "project cancellation request failed");
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn observe_source_application_event(&mut self, event: &ApplicationEvent) {
        match event {
            ApplicationEvent::SourceVerificationRequested { token } => {
                let path = self.dataset.selected_path().to_path_buf();
                let resource_policy =
                    current_egui_shell_bridge::snapshot(&self.application).resource_policy();
                let scan_ledger = self.dataset.cpu_ledger_arc();
                let request = self
                    .source_verification_service
                    .as_mut()
                    .ok_or(
                        current_source_verification_service::CurrentSourceVerificationServiceError::NoActiveOperation,
                    )
                    .and_then(|service| {
                        service.request_verification(
                            token.clone(),
                            path,
                            resource_policy,
                            scan_ledger,
                        )
                    });
                if let Err(error) = request {
                    tracing::warn!(%error, "source-verification request failed");
                    self.complete_source_operation(
                        token.clone(),
                        OperationCompletion::Failed(
                            OperationFailureCode::SourceVerificationReadFailed,
                        ),
                    );
                }
            }
            ApplicationEvent::SourceVerificationInvalidated { source_generation } => {
                let snapshot = current_egui_shell_bridge::snapshot(&self.application);
                if *source_generation == snapshot.source_generation()
                    && self.dataset.resource_identity()
                        != snapshot.catalog().scientific_identity().resource_identity()
                {
                    self.retire_invalidated_source_runtime();
                }
            }
            ApplicationEvent::OperationCancellationRequested { token }
                if token.kind() == OperationKind::DatasetOpen =>
            {
                if let Some(service) = self.source_open_service.as_ref() {
                    match service.cancel(token) {
                        Ok(())
                        | Err(
                            current_source_open_service::CurrentSourceOpenServiceError::NoActiveOperation,
                        ) => {}
                        Err(error) => {
                            tracing::warn!(%error, "dataset cancellation request failed");
                        }
                    }
                }
            }
            ApplicationEvent::OperationCancellationRequested { token }
                if token.kind() == OperationKind::SourceVerification =>
            {
                if let Some(service) = self.source_verification_service.as_ref() {
                    match service.cancel(token) {
                        Ok(())
                        | Err(
                            current_source_verification_service::CurrentSourceVerificationServiceError::NoActiveOperation,
                        ) => {}
                        Err(error) => {
                            tracing::warn!(%error, "source-verification cancellation failed");
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn complete_source_operation(
        &mut self,
        token: OperationToken,
        completion: OperationCompletion,
    ) -> bool {
        let operation_id = token.operation_id();
        match current_egui_shell_bridge::dispatch(
            &mut self.application,
            ApplicationCommand::CompleteOperation { token, completion },
        ) {
            Ok(_) => true,
            Err(fault) if fault.code() == ApplicationFaultCode::OperationNotFound => false,
            Err(fault) => {
                tracing::warn!(?fault, "source operation completion was rejected");
                match current_egui_shell_bridge::dispatch(
                    &mut self.application,
                    ApplicationCommand::CancelOperation(operation_id),
                ) {
                    Ok(_) => {}
                    Err(cancel_fault)
                        if cancel_fault.code() == ApplicationFaultCode::OperationNotFound => {}
                    Err(cancel_fault) => {
                        tracing::warn!(
                            ?cancel_fault,
                            "rejected source operation could not be retired"
                        );
                    }
                }
                false
            }
        }
    }

    fn complete_project_operation(
        &mut self,
        token: mirante4d_application::OperationToken,
        completion: OperationCompletion,
    ) -> bool {
        let operation_id = token.operation_id();
        match current_egui_shell_bridge::dispatch(
            &mut self.application,
            ApplicationCommand::CompleteOperation { token, completion },
        ) {
            Ok(_) => true,
            Err(fault) if fault.code() == ApplicationFaultCode::OperationNotFound => false,
            Err(fault) => {
                tracing::warn!(?fault, "project persistence completion was rejected");
                match current_egui_shell_bridge::dispatch(
                    &mut self.application,
                    ApplicationCommand::CancelOperation(operation_id),
                ) {
                    Ok(_) => {}
                    Err(cancel_fault)
                        if cancel_fault.code() == ApplicationFaultCode::OperationNotFound => {}
                    Err(cancel_fault) => {
                        tracing::warn!(?cancel_fault, "project operation retirement was rejected");
                    }
                }
                false
            }
        }
    }

    fn begin_background_operation(&mut self, kind: OperationKind) -> Option<OperationToken> {
        let before = current_egui_shell_bridge::snapshot(&self.application)
            .active_operations()
            .iter()
            .map(OperationToken::operation_id)
            .collect::<Vec<_>>();
        if let Err(fault) = current_egui_shell_bridge::dispatch(
            &mut self.application,
            ApplicationCommand::BeginOperation(kind),
        ) {
            tracing::warn!(?fault, ?kind, "background operation was rejected");
            return None;
        }
        current_egui_shell_bridge::snapshot(&self.application)
            .active_operations()
            .iter()
            .find(|token| token.kind() == kind && !before.contains(&token.operation_id()))
            .cloned()
    }

    fn complete_background_operation(
        &mut self,
        token: OperationToken,
        completion: OperationCompletion,
    ) -> bool {
        let operation_id = token.operation_id();
        match current_egui_shell_bridge::dispatch(
            &mut self.application,
            ApplicationCommand::CompleteOperation { token, completion },
        ) {
            Ok(_) => true,
            Err(fault) => {
                tracing::warn!(?fault, "background operation completion was rejected");
                let _ = current_egui_shell_bridge::dispatch(
                    &mut self.application,
                    ApplicationCommand::CancelOperation(operation_id),
                );
                false
            }
        }
    }

    fn cancel_background_operation(&mut self, token: &OperationToken) {
        if let Err(fault) = current_egui_shell_bridge::dispatch(
            &mut self.application,
            ApplicationCommand::CancelOperation(token.operation_id()),
        ) {
            tracing::warn!(?fault, "background operation cancellation was rejected");
        }
    }

    fn poll_project_persistence(&mut self) {
        loop {
            let event = match self
                .project_persistence
                .as_ref()
                .map(|bridge| bridge.try_recv())
            {
                None | Some(Ok(None)) => break,
                Some(Ok(Some(event))) => event,
                Some(Err(error)) => {
                    tracing::error!(%error, "project persistence actor failed");
                    let tokens = current_egui_shell_bridge::snapshot(&self.application)
                        .active_operations()
                        .iter()
                        .filter(|token| {
                            matches!(
                                token.kind(),
                                OperationKind::ProjectOpen | OperationKind::ProjectSave
                            )
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    for token in tokens {
                        let code = project_failure_code(token.kind(), &error);
                        self.complete_project_operation(token, OperationCompletion::Failed(code));
                    }
                    if let Some(bridge) = self.project_persistence.take()
                        && let Err(shutdown_error) = bridge.shutdown()
                    {
                        tracing::warn!(%shutdown_error, "failed to join unavailable project actor");
                    }
                    break;
                }
            };
            match event {
                current_project_persistence_bridge::ProjectPersistenceEvent::OpenCompleted {
                    token,
                    path,
                    result,
                } => match result {
                    Ok(projection) => {
                        if self.complete_project_operation(
                            token,
                            OperationCompletion::ProjectOpened(projection),
                        ) {
                            self.project_runtime.current_project_path = Some(path);
                        }
                    }
                    Err(error) => {
                        self.complete_project_operation(
                            token.clone(),
                            OperationCompletion::Failed(project_failure_code(token.kind(), &error)),
                        );
                    }
                },
                current_project_persistence_bridge::ProjectPersistenceEvent::SaveCompleted {
                    token,
                    path,
                    result,
                } => match result {
                    Ok(revision) => {
                        if self.complete_project_operation(
                            token,
                            OperationCompletion::ProjectSaved(revision),
                        ) {
                            self.project_runtime.current_project_path = Some(path);
                        }
                    }
                    Err(error) => {
                        self.complete_project_operation(
                            token.clone(),
                            OperationCompletion::Failed(project_failure_code(token.kind(), &error)),
                        );
                    }
                },
                current_project_persistence_bridge::ProjectPersistenceEvent::Cancelled {
                    token,
                } => {
                    self.complete_project_operation(token, OperationCompletion::Cancelled);
                }
            }
        }
    }

    fn project_dirty(&self) -> bool {
        current_egui_shell_bridge::snapshot(&self.application)
            .dirty()
            .unwrap_or(false)
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|input| input.viewport().close_requested()) {
            return;
        }
        if self.ui_runtime.allow_close_without_prompt || !self.project_dirty() {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        self.ui_runtime.close_prompt_open = true;
    }

    fn show_dirty_project_close_prompt(&mut self, ctx: &egui::Context) {
        if !self.ui_runtime.close_prompt_open {
            return;
        }
        egui::Window::new("Unsaved Project")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("Project changes have not been saved.");
                ui.horizontal(|ui| {
                    if ui_kit::toolbar_button(ui, "Save", true).clicked() {
                        self.save_current_project();
                    }
                    if ui_kit::toolbar_button(ui, "Discard", true).clicked() {
                        self.ui_runtime.allow_close_without_prompt = true;
                        self.ui_runtime.close_prompt_open = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui_kit::toolbar_button(ui, "Cancel", true).clicked() {
                        self.ui_runtime.close_prompt_open = false;
                        self.ui_runtime.allow_close_without_prompt = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    }
                });
            });
    }

    fn open_session_from_dialog(&mut self, _ctx: &egui::Context) {
        if let Err(fault) = current_egui_shell_bridge::dispatch(
            &mut self.application,
            ApplicationCommand::RequestProjectOpen,
        ) {
            tracing::info!(?fault, "project open is unavailable for the current source");
        }
        self.pump_application_services();
    }

    fn open_native_from_dialog(&mut self, ctx: &egui::Context) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Open Mirante4D dataset package")
            .pick_folder()
        else {
            return;
        };
        if let Err(error) = self.replace_state_from_dataset_path(path, Some(ctx)) {
            tracing::warn!(%error, "dataset open request was rejected");
        }
    }

    fn replace_state_from_dataset_path(
        &mut self,
        path: PathBuf,
        ctx: Option<&egui::Context>,
    ) -> anyhow::Result<()> {
        current_egui_shell_bridge::dispatch(
            &mut self.application,
            ApplicationCommand::RequestDatasetOpen,
        )
        .map_err(|fault| anyhow::anyhow!("dataset open command rejected: {fault:?}"))?;
        let events = current_egui_shell_bridge::drain_events(&mut self.application, 256);
        let token = events
            .iter()
            .find_map(|event| match event {
                ApplicationEvent::DatasetOpenRequested { token } => Some(token.clone()),
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("dataset open request emitted no operation token"))?;
        let resource_policy =
            current_egui_shell_bridge::snapshot(&self.application).resource_policy();
        let Some(service) = self.source_open_service.as_mut() else {
            self.complete_source_operation(
                token,
                OperationCompletion::Failed(OperationFailureCode::DatasetReadFailed),
            );
            anyhow::bail!("dataset open service is unavailable");
        };
        if let Err(error) = service.request_open(token.clone(), path, resource_policy) {
            self.complete_source_operation(
                token,
                OperationCompletion::Failed(OperationFailureCode::DatasetReadFailed),
            );
            return Err(anyhow::anyhow!(error));
        }
        for event in &events {
            if !matches!(event, ApplicationEvent::DatasetOpenRequested { .. }) {
                self.observe_project_application_event(event);
            }
        }
        if let Some(ctx) = ctx {
            request_background_work_repaint_after(ctx);
        }
        Ok(())
    }

    fn save_current_project(&mut self) {
        if let Err(fault) = current_egui_shell_bridge::dispatch(
            &mut self.application,
            ApplicationCommand::RequestProjectSave,
        ) {
            tracing::info!(?fault, "project save is unavailable for the current source");
        }
        self.pump_application_services();
    }

    fn request_current_source_verification(&mut self) {
        if let Err(fault) = current_egui_shell_bridge::dispatch(
            &mut self.application,
            ApplicationCommand::RequestSourceVerification,
        ) {
            tracing::warn!(?fault, "source verification request was rejected");
        }
    }

    fn try_start_pending_automatic_source_verification(&mut self) {
        let Some(pending_generation) = self.pending_automatic_source_verification else {
            return;
        };
        let snapshot = current_egui_shell_bridge::snapshot(&self.application);
        if snapshot.source_generation() != pending_generation
            || !matches!(snapshot.source(), SourceVerificationSnapshot::Required)
        {
            self.pending_automatic_source_verification = None;
            return;
        }
        if self
            .source_verification_service
            .as_ref()
            .is_none_or(|service| service.active_token().is_some())
        {
            return;
        }

        match current_egui_shell_bridge::dispatch(
            &mut self.application,
            ApplicationCommand::RequestSourceVerification,
        ) {
            Ok(_) => self.pending_automatic_source_verification = None,
            Err(fault) if fault.code() == ApplicationFaultCode::OperationConflict => {}
            Err(fault) => {
                self.pending_automatic_source_verification = None;
                tracing::warn!(?fault, "automatic source verification request was rejected");
            }
        }
    }

    fn retire_invalidated_source_runtime(&mut self) {
        if let Err(error) = self.dataset.cancel_and_clear_interactive_demand() {
            tracing::warn!(%error, "invalidated dataset demand cancellation failed");
        }
        let retired_leases = std::mem::take(&mut self.render_runtime.lease_bridge);
        self.render_runtime.frame_fidelity.completeness = FrameCompleteness::Loading;
        self.render_runtime.frame_fidelity.reason = LodDecisionReason::LoadingTargetScale;
        self.render_runtime.frame_fidelity.backend = RenderBackend::Loading;
        std::thread::spawn(move || drop(retired_leases));
    }

    fn request_opened_state_visible_work(&mut self, _ctx: Option<&egui::Context>) {
        self.request_visible_bricks();
    }

    fn poll_source_open_service(&mut self) {
        let (active_token, result) = match self.source_open_service.as_mut() {
            Some(service) => (service.active_token().cloned(), service.try_recv()),
            None => return,
        };
        let result = match result {
            Ok(Some(result)) => result,
            Ok(None) => return,
            Err(error) => {
                tracing::error!(%error, "dataset open worker failed");
                if let Some(token) = active_token {
                    self.complete_source_operation(
                        token,
                        OperationCompletion::Failed(OperationFailureCode::DatasetReadFailed),
                    );
                }
                return;
            }
        };
        let token = result.token;
        match result.outcome {
            current_source_open_service::CurrentSourceOpenOutcome::Prepared(prepared) => {
                let (runtime, completion) = prepared.into_runtime_and_completion();
                if self.complete_source_operation(token, completion) {
                    self.install_current_source_runtime(runtime);
                } else {
                    tracing::warn!("stale dataset open result was suppressed");
                    if let Err(error) = runtime.dataset.request_shutdown() {
                        tracing::warn!(%error, "stale dataset runtime shutdown request failed");
                    }
                    std::thread::spawn(move || drop(runtime));
                }
            }
            current_source_open_service::CurrentSourceOpenOutcome::Cancelled => {
                self.complete_source_operation(token, OperationCompletion::Cancelled);
            }
            current_source_open_service::CurrentSourceOpenOutcome::Failed(code) => {
                self.complete_source_operation(token, OperationCompletion::Failed(code));
            }
        }
    }

    fn poll_source_verification_service(&mut self) {
        let (active_token, progress, result) = match self.source_verification_service.as_mut() {
            Some(service) => (
                service.active_token().cloned(),
                service.take_progress(),
                service.try_recv(),
            ),
            None => return,
        };

        match progress {
            Ok(Some(progress)) => {
                match current_egui_shell_bridge::dispatch(
                    &mut self.application,
                    ApplicationCommand::UpdateSourceVerificationProgress {
                        token: progress.token,
                        completed_work: progress.completed_work,
                        total_work: progress.total_work,
                    },
                ) {
                    Ok(_) => {
                        if let Some(service) = self.source_verification_service.as_mut() {
                            service.note_accepted_progress();
                        }
                    }
                    Err(fault) if fault.code() == ApplicationFaultCode::OperationNotFound => {}
                    Err(fault) => tracing::warn!(?fault, "source-verification progress rejected"),
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "source-verification progress failed"),
        }

        let result = match result {
            Ok(Some(result)) => result,
            Ok(None) => return,
            Err(error) => {
                tracing::error!(%error, "source-verification worker failed");
                if let Some(token) = active_token {
                    self.complete_source_operation(
                        token,
                        OperationCompletion::Failed(
                            OperationFailureCode::SourceVerificationReadFailed,
                        ),
                    );
                }
                return;
            }
        };

        let token = result.token;
        match result.outcome {
            current_source_verification_service::CurrentSourceVerificationOutcome::Prepared(
                prepared,
            ) => {
                let (runtime, completion) = prepared.into_runtime_and_completion();
                if self.complete_source_operation(token, completion) {
                    if let Some(service) = self.source_verification_service.as_mut() {
                        service.note_accepted_success();
                    }
                    self.install_verified_source_runtime(runtime);
                } else {
                    tracing::warn!("stale source-verification result was suppressed");
                    if let Err(error) = runtime.dataset.request_shutdown() {
                        tracing::warn!(%error, "stale verified runtime shutdown request failed");
                    }
                    std::thread::spawn(move || drop(runtime));
                }
            }
            current_source_verification_service::CurrentSourceVerificationOutcome::Cancelled => {
                if let Some(service) = self.source_verification_service.as_mut() {
                    service.note_cancelled_run();
                }
                self.complete_source_operation(token, OperationCompletion::Cancelled);
            }
            current_source_verification_service::CurrentSourceVerificationOutcome::Failed(code) => {
                self.complete_source_operation(token, OperationCompletion::Failed(code));
            }
        }
    }

    fn install_verified_source_runtime(
        &mut self,
        transfer: current_source_verification_service::CurrentSourceVerificationRuntimeTransfer,
    ) {
        let retired_leases = std::mem::take(&mut self.render_runtime.lease_bridge);
        let old_dataset = std::mem::replace(&mut self.dataset, transfer.dataset);
        if let Err(error) = old_dataset.request_shutdown() {
            tracing::warn!(%error, "unverified dataset runtime shutdown request failed");
        }
        self.request_opened_state_visible_work(None);
        std::thread::spawn(move || drop((old_dataset, retired_leases)));
    }

    fn install_current_source_runtime(
        &mut self,
        transfer: current_source_open_service::CurrentSourceRuntimeTransfer,
    ) {
        let current_source_open_service::CurrentSourceRuntimeTransfer {
            dataset,
            mut render_runtime,
            analysis_runtime,
        } = transfer;
        render_runtime.gpu_renderer = self.render_runtime.gpu_renderer.clone();
        self.clear_gpu_display_frame();
        self.retire_gpu_display_texture_id();
        self.retire_cross_section_gpu_display_texture_ids();
        let old_dataset = std::mem::replace(&mut self.dataset, dataset);
        let old_render_runtime = std::mem::replace(&mut self.render_runtime, render_runtime);
        let old_analysis_runtime = std::mem::replace(&mut self.analysis_runtime, analysis_runtime);
        let old_import_runtime = std::mem::replace(
            &mut self.import_runtime,
            current_runtime::import::CurrentImportRuntime::idle(),
        );
        self.project_runtime.current_project_path = None;
        self.ui_runtime.viewer_tools = ViewerToolState::default();
        self.ui_runtime.analysis_plot_view = None;
        self.ui_runtime.analysis_filter.clear();
        self.ui_runtime.analysis_sort = None;
        self.ui_runtime.hovered_pixel = None;
        self.ui_runtime.hovered_source_readout = None;
        if let Err(error) = old_dataset.request_shutdown() {
            tracing::warn!(%error, "replaced dataset runtime shutdown request failed");
        }
        self.pending_automatic_source_verification =
            Some(current_egui_shell_bridge::snapshot(&self.application).source_generation());
        self.try_start_pending_automatic_source_verification();
        self.request_opened_state_visible_work(None);

        std::thread::spawn(move || {
            drop((
                old_dataset,
                old_render_runtime,
                old_analysis_runtime,
                old_import_runtime,
            ));
        });
    }

    fn active_histogram_summary(&mut self) -> LayerHistogramSummary {
        let snapshot = current_egui_shell_bridge::snapshot(&self.application);
        let view = application_view(&snapshot);
        let active_key = view.active_layer();
        let layer = snapshot
            .catalog()
            .layer(active_key)
            .expect("application view closes over the dataset catalog");
        let scale = ScaleLevel::new(
            self.render_runtime
                .lod_schedule
                .displayed_scale_level
                .unwrap_or(self.render_runtime.lod_schedule.target_scale_level),
        );
        active_layer_histogram_summary(
            &self.render_runtime.lease_bridge,
            histogram::ActiveLayerHistogramInput {
                requirements: self
                    .dataset
                    .scope_requirements(dataset_requests::SCOPE_CURRENT_3D),
                identity: snapshot.catalog().scientific_identity().resource_identity(),
                layer: active_key,
                layer_name: layer.label(),
                dtype: layer.dtype(),
                timepoint: view.timepoint(),
                scale,
            },
        )
    }

    pub(crate) fn apply_application_command(
        &mut self,
        command: ApplicationCommand,
        ctx: &egui::Context,
    ) -> Result<CommandEffect, ApplicationFault> {
        let before = current_egui_shell_bridge::snapshot(&self.application);
        let previous_view = application_view(&before).clone();
        let effect = current_egui_shell_bridge::dispatch(&mut self.application, command)?;
        if effect == CommandEffect::Changed {
            let after = current_egui_shell_bridge::snapshot(&self.application);
            self.reconcile_application_change(&previous_view, &after, ctx);
        }
        self.pump_application_services();
        Ok(effect)
    }

    fn reconcile_application_change(
        &mut self,
        previous_view: &ViewState,
        snapshot: &ApplicationSnapshot,
        ctx: &egui::Context,
    ) {
        let next_view = application_view(snapshot);
        let playback_lod_downshift_active = snapshot.transient().playback_active();
        let playback_lod_changed =
            self.render_runtime.playback_lod_downshift_active != playback_lod_downshift_active;
        self.render_runtime.playback_lod_downshift_active = playback_lod_downshift_active;
        if previous_view == next_view && !playback_lod_changed {
            return;
        }

        if playback_lod_changed {
            self.request_visible_bricks();
        }

        if previous_view == next_view {
            ctx.request_repaint();
            return;
        }

        let source_selection_changed = match layer_state::reconcile_view_runtime(
            previous_view,
            snapshot,
            &mut self.dataset,
            &mut self.render_runtime,
            &mut self.analysis_runtime,
        ) {
            Ok(changed) => changed,
            Err(error) => {
                tracing::error!(%error, "failed to reconcile the canonical view with current runtime");
                self.dataset.record_plan_error(error.to_string());
                self.render_runtime.visible_brick_plan_error = Some(error.to_string());
                self.render_runtime.frame_fidelity.completeness = FrameCompleteness::Incomplete;
                false
            }
        };
        if previous_view.layout() != next_view.layout() {
            self.ui_runtime.hovered_pixel = None;
            self.ui_runtime.hovered_source_readout = None;
            self.ui_runtime.viewport_orbit_drag = None;
            if next_view.layout() == CanonicalViewerLayout::Single3d {
                self.retire_cross_section_gpu_display_texture_ids();
                self.render_runtime
                    .cross_section_runtime
                    .mark_cross_section_panels_dirty();
            }
        }
        if source_selection_changed {
            self.clear_gpu_display_frame();
            self.retire_gpu_display_texture_id();
            self.request_visible_bricks();
        } else {
            self.invalidate_cross_section_panel_display_frames();
            self.clear_gpu_display_frame();
            self.retire_gpu_display_texture_id();
            if let Err(error) = self.rerender_display_state() {
                tracing::error!(%error, "failed to render the accepted canonical view");
                self.dataset.record_plan_error(error.to_string());
                self.render_runtime.visible_brick_plan_error = Some(error.to_string());
                self.render_runtime.frame_fidelity.completeness = FrameCompleteness::Incomplete;
                self.request_visible_bricks();
            } else {
                self.request_visible_bricks();
            }
        }
        ctx.request_repaint();
        ctx.request_repaint_after(CROSS_SECTION_INTERACTION_SETTLE_DURATION);
    }
}
#[cfg(test)]
mod tests;

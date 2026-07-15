use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

mod analysis_product;
mod analysis_workspace;
mod cross_section_readout;
mod cross_section_scheduler;
mod current_runtime;
mod current_settings_connection;
mod current_source_open_service;
mod current_source_verification_service;
mod dataset_demand_plan;
mod dataset_requests;
mod diagnostics;
mod display_graph;
mod display_refresh;
mod fidelity;
mod histogram;
mod import_worker_service;
mod import_workflow;
mod layer_state;
mod lod_scheduler;
mod native_presentation;
mod playback;
mod product_automation;
mod product_render_intent;
mod render_state;
mod retained_leases;
mod runtime_diagnostics_panel;
mod semantic_demand;
mod semantic_tiles;
mod smoke;
mod state;
mod tool_interactions;
mod transfer_presets;
mod unified_source_open;
mod viewer_layout;
mod viewport;
mod workbench_brick_runtime;
mod workbench_controls;
mod workbench_import;
mod workbench_playback_runtime;
mod workbench_ui;
mod workbench_ui_output;

#[cfg(test)]
use analysis_workspace::{
    AnalysisPlotBounds, analysis_plot_bounds, analysis_plot_visible_bounds,
    analysis_table_preview_rows, nearest_analysis_plot_point,
    normalize_analysis_plot_view_for_plot, pan_analysis_plot_view, plot_screen_position,
    zoom_analysis_plot_view,
};
use analysis_workspace::{
    AnalysisTableExportInput, AnalysisWorkspaceViewInput, export_selected_analysis_table,
    show_analysis_workspace, show_analysis_workspace_window,
};
use cross_section_readout::cross_section_hover_readout_for_response;
pub use diagnostics::{StartupDiagnostics, collect_startup_diagnostics, default_log_path};
use display_refresh::{ViewportDisplayImage, duration_ms};
use eframe::egui;
use fidelity::{composite_fidelity_label, iso_shading_policy_label, render_sampling_policy_label};
use histogram::{
    active_layer_histogram_summary, auto_dense_window_from_histogram,
    auto_dvr_opacity_transfer_from_histogram, auto_signal_window_from_histogram,
    histogram_bins_label, histogram_can_auto_window, histogram_status_label,
};
#[cfg(test)]
use import_worker_service::ImportWorkerStatus;
use import_worker_service::{ImportWorkerCompletion, ImportWorkerOutcome};
use import_workflow::{ImportWorkflow, reset_checkpoint_directory, tiff_destination};
use mirante4d_application::{
    ApplicationCommand, ApplicationEvent, ApplicationFault, ApplicationFaultCode,
    ApplicationSnapshot, ApplicationState, CommandEffect, DisplayRefreshPath, DisplayRefreshTiming,
    OperationCompletion, OperationFailureCode, OperationKind, OperationToken, PresentationSlot,
    PresentationSnapshot, PresentationSurface, ProjectRecoveryStoreLocator,
    ProjectStoreApplicationService, ProjectStoreLifecycle, ProjectStoreServiceEvent,
    RenderCoordinationState, ResidentRenderFailureStatus, SourceSessionGeneration,
    SourceVerificationSnapshot, SystemMonotonicClock, WorkspaceSnapshot,
    import_workflow::{ImportCommand, ImportReviewId, ImportWorkflowSnapshot},
    viewer_tools::{ViewerTool, ViewerToolState},
};
pub use mirante4d_application::{
    CrossSectionPanelScheduleReason, CrossSectionPanelScheduleState,
    CrossSectionPanelScheduleStatus, DisplayedFrameFreshness, FrameCompleteness, FrameFailureKind,
    FrameFidelityStatus, LodDecisionReason, RenderBackend,
};
use mirante4d_dataset::{DatasetSourceId, ResourceValidity};
use mirante4d_domain::{
    CameraView, CrossSectionView, DisplayWindow, DvrOpacityTransfer as CanonicalDvrOpacityTransfer,
    IsoLightState, IsoShadingPolicy, LayerTransfer, RenderState as CanonicalRenderState, RgbColor,
    SamplingPolicy, ScaleLevel, TRANSFER_GAMMA_MAX, TRANSFER_GAMMA_MIN, TransferCurve,
    UnitQuaternion, ViewerLayout as CanonicalViewerLayout,
};
#[cfg(test)]
use mirante4d_domain::{IntensityDType, Shape3D, TimeIndex};
#[cfg(test)]
use mirante4d_import_pipeline::ImportCancellation;
use mirante4d_import_pipeline::{ImportError, ImportOptions, ImportReceipt, TiffSource};
use mirante4d_project_model::{
    ChannelPreset, LayerViewState, ProjectId, ProjectRevisionId, ViewState,
};
use mirante4d_project_store::{
    ProjectGenerationId, ProjectOpenMode, ProjectRecoveryCandidate, ProjectStoreConfig,
    ProjectStoreFault, ProjectStorePath, ProjectStoreRequestId,
};
use mirante4d_render_api::PresentationViewport;
use mirante4d_render_wgpu::{WgpuRenderRuntime, WgpuRenderRuntimeConfig};
use mirante4d_settings::{RejectedFileDisposition, ResourcePolicy, recommended_for_current_system};
use mirante4d_ui_egui as ui_kit;
use playback::playback_status_label;
#[cfg(test)]
use playback::stepped_timepoint;
use product_automation::{ProductAutomationAppUpdateTiming, ProductAutomationController};
use render_state::set_render_viewport;
pub use smoke::{AppSmokeOptions, AppSmokeReport, PlaybackSmokeFrame, run_headless_smoke};
pub use state::{HistogramStatus, LayerHistogramSummary};
use tool_interactions::apply_viewport_tool_response;
use transfer_presets::{
    built_in_transfer_preset_curve, built_in_transfer_preset_label, built_in_transfer_presets,
    channel_preset_from_current_view, next_user_channel_preset_id,
};
#[cfg(test)]
use ui_kit::{AnalysisPlotViewRange, AnalysisTableSort, ViewportIntensity};
use ui_kit::{
    RenderUiRequest, StatusTone, ViewportHover, ViewportObservation, WorkbenchAnalysisKind,
    WorkbenchLayoutSpec, WorkbenchUiAction, WorkbenchUiOutput,
};
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
const PROJECT_RECOVERY_ROOT_ENTRIES_MAX: usize = 64;

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
    fault: &ProjectStoreFault,
) -> OperationFailureCode {
    if operation == OperationKind::Analysis {
        return match fault {
            ProjectStoreFault::Capacity { .. } | ProjectStoreFault::QueueFull { .. } => {
                OperationFailureCode::AnalysisCapacityExceeded
            }
            _ => OperationFailureCode::AnalysisExecutionFailed,
        };
    }
    match fault {
        ProjectStoreFault::ReadOnly => OperationFailureCode::ProjectReadOnly,
        ProjectStoreFault::WriterContended => OperationFailureCode::ProjectWriterContended,
        ProjectStoreFault::StaleParent => OperationFailureCode::ProjectStaleParent,
        ProjectStoreFault::DestinationExists => OperationFailureCode::ProjectDestinationExists,
        ProjectStoreFault::UnsupportedFilesystem => {
            OperationFailureCode::ProjectUnsupportedFilesystem
        }
        ProjectStoreFault::Capacity { .. } => OperationFailureCode::ProjectCapacityExceeded,
        ProjectStoreFault::SourceChanged => OperationFailureCode::ProjectSourceChanged,
        ProjectStoreFault::DigestMismatch => OperationFailureCode::ProjectDigestMismatch,
        ProjectStoreFault::Corruption { .. } | ProjectStoreFault::ConfirmationRequired => {
            OperationFailureCode::ProjectCorrupt
        }
        ProjectStoreFault::QueueFull { .. } => OperationFailureCode::ProjectBusy,
        ProjectStoreFault::Cancelled => match operation {
            OperationKind::ProjectOpen | OperationKind::ProjectRecovery => {
                OperationFailureCode::ProjectReadFailed
            }
            _ => OperationFailureCode::ProjectWriteFailed,
        },
        ProjectStoreFault::CommitIndeterminate => OperationFailureCode::ProjectCommitIndeterminate,
    }
}

fn project_service_error_fault(
    error: mirante4d_application::ProjectStoreServiceError,
) -> ProjectStoreFault {
    match error {
        mirante4d_application::ProjectStoreServiceError::Store(fault) => fault,
        mirante4d_application::ProjectStoreServiceError::ReadOnly => ProjectStoreFault::ReadOnly,
        mirante4d_application::ProjectStoreServiceError::WritesSuspended => {
            ProjectStoreFault::CommitIndeterminate
        }
        mirante4d_application::ProjectStoreServiceError::RecoveryCandidateUnavailable
        | mirante4d_application::ProjectStoreServiceError::InvalidProjection
        | mirante4d_application::ProjectStoreServiceError::InvalidApplicationSnapshot
        | mirante4d_application::ProjectStoreServiceError::InvalidOperationToken
        | mirante4d_application::ProjectStoreServiceError::ProjectMismatch
        | mirante4d_application::ProjectStoreServiceError::UnexpectedCompletion => {
            ProjectStoreFault::Corruption {
                stage: "application_service",
            }
        }
        mirante4d_application::ProjectStoreServiceError::OperationConflict
        | mirante4d_application::ProjectStoreServiceError::Closing => {
            ProjectStoreFault::QueueFull {
                queue: "application_service",
            }
        }
        mirante4d_application::ProjectStoreServiceError::SaveAsRequired => {
            ProjectStoreFault::ReadOnly
        }
        mirante4d_application::ProjectStoreServiceError::ClockRegressed { .. }
        | mirante4d_application::ProjectStoreServiceError::ClockOverflow
        | mirante4d_application::ProjectStoreServiceError::RequestIdOverflow
        | mirante4d_application::ProjectStoreServiceError::ActorPanicked => {
            ProjectStoreFault::Corruption {
                stage: "application_service_runtime",
            }
        }
    }
}

struct ProjectRecoveryReview {
    token: OperationToken,
    automatic_newer: ProjectGenerationId,
}

#[derive(Debug)]
enum ProjectRecoveryDiscoveryError {
    Io(io::ErrorKind),
    Capacity,
    InvalidPath(ProjectStoreFault),
}

impl std::fmt::Display for ProjectRecoveryDiscoveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(kind) => write!(formatter, "recovery directory I/O failed: {kind:?}"),
            Self::Capacity => formatter.write_str("too many recovery-root entries"),
            Self::InvalidPath(fault) => write!(formatter, "invalid recovery path: {fault}"),
        }
    }
}

struct PendingSourceInstall {
    token: OperationToken,
    runtime: current_source_open_service::CurrentSourceRuntimeTransfer,
    completion: OperationCompletion,
}

#[derive(Default)]
struct ProjectStoreNoninteractivePaths {
    open: Option<PathBuf>,
    initial_save: Option<PathBuf>,
    save_as: Option<PathBuf>,
}

#[derive(Debug)]
enum ProjectStoreRecordedResult {
    Succeeded,
    Failed(String),
}

#[derive(Default)]
struct ProjectStoreProductEvidence {
    initial_save_captured_revision: Option<ProjectRevisionId>,
    latest_autosave_captured_revision: Option<ProjectRevisionId>,
    durable_edit_started_at: Option<Instant>,
    autosave_elapsed_from_durable_edit_ms: Option<u64>,
    close_result: Option<ProjectStoreRecordedResult>,
    actor_join: Option<ProjectStoreRecordedResult>,
}

fn initialize_project_recovery_root(source: &Path) -> (Option<PathBuf>, Option<String>) {
    let Some(state_root) = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".local").join("state"))
        })
    else {
        return (
            None,
            Some("Project recovery is unavailable; provisional autosave is disabled.".to_owned()),
        );
    };
    let recovery_root = match project_recovery_root_path(&state_root, source) {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(kind = ?error.kind(), "project recovery area overlaps the source or cannot be validated");
            return (
                None,
                Some(
                    "Project recovery is unavailable for this source; provisional autosave is disabled."
                        .to_owned(),
                ),
            );
        }
    };
    match fs::create_dir_all(&recovery_root) {
        Ok(()) => (Some(recovery_root), None),
        Err(error) => {
            tracing::warn!(kind = ?error.kind(), "project recovery area is unavailable");
            (
                None,
                Some(
                    "Project recovery is unavailable; provisional autosave is disabled.".to_owned(),
                ),
            )
        }
    }
}

fn project_recovery_root_path(state_root: &Path, source: &Path) -> io::Result<PathBuf> {
    let recovery_root = state_root.join("mirante4d").join("recovery");
    if project_destination_is_outside_source_closure(source, &recovery_root)? {
        Ok(recovery_root)
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "project recovery root overlaps the microscopy source",
        ))
    }
}

fn discover_project_recovery_locators(
    recovery_root: Option<&Path>,
    current_project_id: ProjectId,
) -> Result<Vec<ProjectRecoveryStoreLocator>, ProjectRecoveryDiscoveryError> {
    let Some(recovery_root) = recovery_root else {
        return Ok(Vec::new());
    };
    let entries = fs::read_dir(recovery_root)
        .map_err(|error| ProjectRecoveryDiscoveryError::Io(error.kind()))?;
    let mut locators = Vec::new();
    for (entry_index, entry) in entries.enumerate() {
        if entry_index >= PROJECT_RECOVERY_ROOT_ENTRIES_MAX {
            return Err(ProjectRecoveryDiscoveryError::Capacity);
        }
        let entry = entry.map_err(|error| ProjectRecoveryDiscoveryError::Io(error.kind()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| ProjectRecoveryDiscoveryError::Io(error.kind()))?;
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(project_id) = name
            .strip_suffix(".m4dproj")
            .and_then(|value| ProjectId::parse(value).ok())
        else {
            continue;
        };
        if project_id == current_project_id {
            continue;
        }
        let path = ProjectStorePath::new(entry.path())
            .map_err(ProjectRecoveryDiscoveryError::InvalidPath)?;
        let locator = ProjectRecoveryStoreLocator::new(project_id, path).map_err(|_| {
            ProjectRecoveryDiscoveryError::InvalidPath(ProjectStoreFault::Corruption {
                stage: "recovery_locator",
            })
        })?;
        locators.push(locator);
    }
    locators.sort_by_key(ProjectRecoveryStoreLocator::project_id);
    Ok(locators)
}

fn project_destination_is_outside_source_closure(
    source: &Path,
    destination: &Path,
) -> io::Result<bool> {
    let source = fs::canonicalize(source)?;
    let destination = canonicalize_from_existing_parent(destination)?;
    Ok(destination != source
        && !destination.starts_with(&source)
        && !source.starts_with(&destination))
}

fn canonicalize_from_existing_parent(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut existing = absolute.as_path();
    let mut missing = Vec::new();
    while !existing.try_exists()? {
        let name = existing.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "destination has no existing ancestor",
            )
        })?;
        missing.push(name.to_os_string());
        existing = existing.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "destination has no existing parent",
            )
        })?;
    }
    let mut canonical = fs::canonicalize(existing)?;
    for component in missing.iter().rev() {
        canonical.push(component);
    }
    Ok(canonical)
}

fn start_project_store_service(
    recovery_root: Option<&Path>,
    provisional_project_id: ProjectId,
) -> Result<
    (
        ProjectStoreApplicationService<SystemMonotonicClock>,
        Option<String>,
    ),
    ProjectStoreFault,
> {
    let discovered_provisional_destination = recovery_root
        .map(|root| root.join(format!("{provisional_project_id}.m4dproj")))
        .map(ProjectStorePath::new)
        .transpose()?;
    let (recovery_store_locators, warning, recovery_discovery_succeeded) =
        match discover_project_recovery_locators(recovery_root, provisional_project_id) {
            Ok(locators) => (locators, None, true),
            Err(error) => {
                tracing::warn!(%error, "project recovery discovery failed");
                (
                    Vec::new(),
                    Some(format!("Project recovery discovery failed: {error}")),
                    false,
                )
            }
        };
    let provisional_destination = recovery_discovery_succeeded
        .then_some(discovered_provisional_destination)
        .flatten();
    let service = ProjectStoreApplicationService::start_with_recovery_locators(
        ProjectStoreConfig::default(),
        SystemMonotonicClock::new(),
        provisional_destination,
        recovery_store_locators,
    )
    .map_err(|error| match error {
        mirante4d_application::ProjectStoreServiceError::Store(fault) => fault,
        _ => ProjectStoreFault::Corruption {
            stage: "application_service_start",
        },
    })?;
    Ok((service, warning))
}

pub struct MiranteWorkbenchApp {
    application: ApplicationState,
    startup_diagnostics: StartupDiagnostics,
    dataset: dataset_requests::DatasetDemandState,
    render_coordination: RenderCoordinationState,
    native_presentation: native_presentation::NativePresentationBridge,
    egui_ui: ui_kit::EguiUiState,
    import: ImportWorkflow,
    analysis_runtime: current_runtime::analysis::AnalysisProductRuntime,
    validation_runtime: current_runtime::validation::CurrentValidationRuntime,
    project_store: Option<ProjectStoreApplicationService<SystemMonotonicClock>>,
    project_recovery_root: Option<PathBuf>,
    project_recovery_candidates: Vec<ProjectRecoveryCandidate>,
    project_recovery_review: Option<ProjectRecoveryReview>,
    project_recovery_panel_open: bool,
    pending_recovery_selection: Option<ProjectGenerationId>,
    pending_project_open_locator: Option<ProjectId>,
    pending_analysis_artifact_load: Option<ProjectStoreRequestId>,
    project_store_noninteractive_paths: ProjectStoreNoninteractivePaths,
    project_store_product_evidence: ProjectStoreProductEvidence,
    pending_dataset_open_path: Option<PathBuf>,
    project_status_message: Option<String>,
    close_after_project_save: bool,
    exit_after_project_close: bool,
    restart_project_store_after_close: bool,
    pending_viewport_close: bool,
    pending_source_install: Option<PendingSourceInstall>,
    settings_connection: current_settings_connection::CurrentSettingsConnection,
    source_open_service: Option<current_source_open_service::CurrentSourceOpenService>,
    source_verification_service:
        Option<current_source_verification_service::CurrentSourceVerificationService>,
    pending_automatic_source_verification: Option<SourceSessionGeneration>,
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
            render_coordination,
            analysis_runtime,
        } = opened;
        let provisional_project_id = workspace.provisional_project_id();
        let application = ApplicationState::new_unbound(
            SourceSessionGeneration::new(1),
            catalog.as_ref().clone(),
            workspace,
            resource_policy,
        )
        .map_err(|code| anyhow::anyhow!("initial application state rejected: {code:?}"))?;
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("the interactive viewer requires the WGPU renderer"))?;
        let validation_runtime =
            current_runtime::validation::CurrentValidationRuntime::from_environment();
        let validation_capture = validation_runtime.product_automation.is_some();
        let product_renderer = WgpuRenderRuntime::from_existing_device(
            &render_state.adapter,
            render_state.device.clone(),
            render_state.queue.clone(),
            WgpuRenderRuntimeConfig::new(resource_policy.gpu_budget_bytes())?
                .with_validation_capture(validation_capture),
        )
        .map_err(|error| anyhow::anyhow!("the progressive GPU renderer is required: {error}"))?;
        let renderer_diagnostics = product_renderer.diagnostics();
        startup_diagnostics.gpu_adapter = Some(format!(
            "{} {} driver={}",
            renderer_diagnostics.backend(),
            renderer_diagnostics.adapter_name(),
            renderer_diagnostics.driver(),
        ));
        let native_presentation = native_presentation::NativePresentationBridge::new(
            render_state.renderer.clone(),
            render_state.device.clone(),
            product_renderer,
        );
        let egui_ui = ui_kit::EguiUiState::new(
            resource_policy.cpu_dataset_budget_bytes(),
            resource_policy.gpu_budget_bytes(),
        );
        let (project_recovery_root, recovery_root_warning) =
            initialize_project_recovery_root(dataset.selected_path());
        let (project_store, discovery_warning) =
            start_project_store_service(project_recovery_root.as_deref(), provisional_project_id)?;
        let project_status_message = recovery_root_warning.or(discovery_warning);
        let project_store = Some(project_store);
        let mut app = Self {
            application,
            startup_diagnostics,
            dataset,
            render_coordination,
            native_presentation,
            egui_ui,
            import: ImportWorkflow::new(),
            analysis_runtime,
            validation_runtime,
            project_store,
            project_recovery_root,
            project_recovery_candidates: Vec::new(),
            project_recovery_review: None,
            project_recovery_panel_open: false,
            pending_recovery_selection: None,
            pending_project_open_locator: None,
            pending_analysis_artifact_load: None,
            project_store_noninteractive_paths: ProjectStoreNoninteractivePaths::default(),
            project_store_product_evidence: ProjectStoreProductEvidence::default(),
            pending_dataset_open_path: None,
            project_status_message,
            close_after_project_save: false,
            exit_after_project_close: false,
            restart_project_store_after_close: false,
            pending_viewport_close: false,
            pending_source_install: None,
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
        let mut cpu_dataset_mib =
            bytes_to_mib_rounded(self.egui_ui.settings_runtime_draft.cpu_dataset_budget_bytes);
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
            self.egui_ui.settings_runtime_draft.cpu_dataset_budget_bytes =
                mib_to_bytes(cpu_dataset_mib);
        }
        ui_kit::property_row(ui, "CPU dataset MiB", cpu_dataset_mib.to_string());

        let mut gpu_mib =
            bytes_to_mib_rounded(self.egui_ui.settings_runtime_draft.gpu_budget_bytes);
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
            self.egui_ui.settings_runtime_draft.gpu_budget_bytes = mib_to_bytes(gpu_mib);
        }
        ui_kit::property_row(ui, "GPU MiB", gpu_mib.to_string());

        let policy = ResourcePolicy::new(
            self.egui_ui.settings_runtime_draft.cpu_dataset_budget_bytes,
            self.egui_ui.settings_runtime_draft.gpu_budget_bytes,
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
                self.egui_ui.settings_runtime_draft = ui_kit::ResourcePolicyDraft {
                    cpu_dataset_budget_bytes: policy.cpu_dataset_budget_bytes(),
                    gpu_budget_bytes: policy.gpu_budget_bytes(),
                };
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
        if let Err(fault) =
            self.application
                .dispatch(ApplicationCommand::RequestResourcePolicyChange {
                    policy,
                    rejected_file_disposition,
                })
        {
            tracing::warn!(?fault, "resource policy command rejected");
        }
        self.pump_application_services();
    }

    fn pump_application_services(&mut self) {
        for _ in 0..4 {
            let mut completion_commands = self.settings_connection.poll();
            let events = self.application.drain_events(256);
            for event in &events {
                if let Some(command) = self.settings_connection.observe_application_event(event) {
                    completion_commands.push(command);
                }
                self.observe_source_application_event(event);
                self.observe_project_application_event(event);
                self.observe_analysis_application_event(event);
            }
            let had_completion_commands = !completion_commands.is_empty();
            for command in completion_commands {
                if let Err(fault) = self.application.dispatch(command) {
                    tracing::warn!(?fault, "application service completion rejected");
                }
            }
            // Drain first so a full bounded event queue cannot prevent a
            // terminal actor result from retiring its operation.
            self.poll_source_open_service();
            self.poll_source_verification_service();
            self.poll_project_store();
            self.try_start_pending_automatic_source_verification();
            if events.is_empty()
                && !had_completion_commands
                && self.application.snapshot().pending_event_count() == 0
            {
                break;
            }
        }
    }

    fn observe_project_application_event(&mut self, event: &ApplicationEvent) {
        match event {
            ApplicationEvent::ProjectOpenRequested { token } => {
                let request = if let Some(project_id) = self.pending_project_open_locator.take() {
                    self.project_store
                        .as_mut()
                        .ok_or(ProjectStoreFault::Corruption {
                            stage: "application_service_unavailable",
                        })
                        .and_then(|service| {
                            service
                                .submit_open_recovery_store(
                                    token.clone(),
                                    project_id,
                                    ProjectOpenMode::PreferWritable,
                                )
                                .map_err(project_service_error_fault)
                        })
                } else {
                    let path = match self.project_store_noninteractive_paths.open.take() {
                        Some(path) => path,
                        None => {
                            let Some(path) = rfd::FileDialog::new()
                                .set_title("Open Mirante4D project package")
                                .pick_folder()
                            else {
                                self.complete_project_operation(
                                    token.clone(),
                                    OperationCompletion::Cancelled,
                                );
                                return;
                            };
                            path
                        }
                    };
                    let path = match ProjectStorePath::new(path) {
                        Ok(path) => path,
                        Err(fault) => {
                            self.complete_project_fault(token.clone(), fault);
                            return;
                        }
                    };
                    if !project_destination_is_outside_source_closure(
                        self.dataset.selected_path(),
                        path.as_path(),
                    )
                    .unwrap_or(false)
                    {
                        self.complete_project_fault(
                            token.clone(),
                            ProjectStoreFault::Corruption {
                                stage: "project_destination_source_overlap",
                            },
                        );
                        return;
                    }
                    self.project_store
                        .as_mut()
                        .ok_or(ProjectStoreFault::Corruption {
                            stage: "application_service_unavailable",
                        })
                        .and_then(|service| {
                            service
                                .submit_open(token.clone(), path, ProjectOpenMode::PreferWritable)
                                .map_err(project_service_error_fault)
                        })
                };
                if let Err(fault) = request {
                    self.complete_project_fault(token.clone(), fault);
                }
            }
            ApplicationEvent::ProjectSaveRequested { token, projection } => {
                let needs_destination = self.project_store.as_ref().is_some_and(|service| {
                    matches!(
                        service.status().lifecycle(),
                        ProjectStoreLifecycle::Unbound | ProjectStoreLifecycle::Provisional
                    )
                });
                let initial_destination = if needs_destination {
                    let path = match self.project_store_noninteractive_paths.initial_save.take() {
                        Some(path) => path,
                        None => {
                            let Some(path) = rfd::FileDialog::new()
                                .set_title("Save Mirante4D project package")
                                .add_filter("Mirante4D project", &["m4dproj"])
                                .set_file_name("project.m4dproj")
                                .save_file()
                            else {
                                self.complete_project_operation(
                                    token.clone(),
                                    OperationCompletion::Cancelled,
                                );
                                return;
                            };
                            path
                        }
                    };
                    match ProjectStorePath::new(path) {
                        Ok(path)
                            if project_destination_is_outside_source_closure(
                                self.dataset.selected_path(),
                                path.as_path(),
                            )
                            .unwrap_or(false) =>
                        {
                            Some(path)
                        }
                        Ok(_) => {
                            self.complete_project_fault(
                                token.clone(),
                                ProjectStoreFault::Corruption {
                                    stage: "project_destination_source_overlap",
                                },
                            );
                            return;
                        }
                        Err(fault) => {
                            self.complete_project_fault(token.clone(), fault);
                            return;
                        }
                    }
                } else {
                    None
                };
                let request = self
                    .project_store
                    .as_mut()
                    .ok_or(ProjectStoreFault::Corruption {
                        stage: "application_service_unavailable",
                    })
                    .and_then(|service| {
                        service
                            .submit_save(
                                token.clone(),
                                projection.as_ref().clone(),
                                initial_destination,
                                Vec::new(),
                            )
                            .map_err(project_service_error_fault)
                    });
                match request {
                    Ok(_) if needs_destination => {
                        self.project_store_product_evidence
                            .initial_save_captured_revision = Some(projection.revision());
                    }
                    Ok(_) => {}
                    Err(fault) => self.complete_project_fault(token.clone(), fault),
                }
            }
            ApplicationEvent::ProjectSaveAsRequested { token, projection } => {
                let path = match self.project_store_noninteractive_paths.save_as.take() {
                    Some(path) => path,
                    None => {
                        let Some(path) = rfd::FileDialog::new()
                            .set_title("Save Mirante4D project package as")
                            .add_filter("Mirante4D project", &["m4dproj"])
                            .set_file_name("project.m4dproj")
                            .save_file()
                        else {
                            self.complete_project_operation(
                                token.clone(),
                                OperationCompletion::Cancelled,
                            );
                            return;
                        };
                        path
                    }
                };
                let path = match ProjectStorePath::new(path) {
                    Ok(path)
                        if project_destination_is_outside_source_closure(
                            self.dataset.selected_path(),
                            path.as_path(),
                        )
                        .unwrap_or(false) =>
                    {
                        path
                    }
                    Ok(_) => {
                        self.complete_project_fault(
                            token.clone(),
                            ProjectStoreFault::Corruption {
                                stage: "project_destination_source_overlap",
                            },
                        );
                        return;
                    }
                    Err(fault) => {
                        self.complete_project_fault(token.clone(), fault);
                        return;
                    }
                };
                let request = self
                    .project_store
                    .as_mut()
                    .ok_or(ProjectStoreFault::Corruption {
                        stage: "application_service_unavailable",
                    })
                    .and_then(|service| {
                        service
                            .submit_save_as(
                                &self.application.snapshot(),
                                token.clone(),
                                path,
                                projection.as_ref().clone(),
                                Vec::new(),
                            )
                            .map_err(project_service_error_fault)
                    });
                if let Err(fault) = request {
                    self.complete_project_fault(token.clone(), fault);
                }
            }
            ApplicationEvent::ProjectRecoveryRequested { token } => {
                let Some(generation_id) = self.pending_recovery_selection.take() else {
                    self.complete_project_operation(token.clone(), OperationCompletion::Cancelled);
                    return;
                };
                let request = self
                    .project_store
                    .as_mut()
                    .ok_or(ProjectStoreFault::Corruption {
                        stage: "application_service_unavailable",
                    })
                    .and_then(|service| {
                        service
                            .submit_open_recovery(token.clone(), generation_id)
                            .map_err(project_service_error_fault)
                    });
                if let Err(fault) = request {
                    self.complete_project_fault(token.clone(), fault);
                }
            }
            ApplicationEvent::OperationCancellationRequested { token }
                if matches!(
                    token.kind(),
                    OperationKind::ProjectOpen
                        | OperationKind::ProjectSave
                        | OperationKind::ProjectSaveAs
                        | OperationKind::ProjectRecovery
                ) =>
            {
                let pending_cancel = self.project_store.as_mut().and_then(|service| match service
                    .cancel_operation(token.operation_id())
                {
                    Ok(_) => None,
                    Err(mirante4d_application::ProjectStoreServiceError::OperationConflict) => {
                        match service.cancel_pending_open(token.operation_id()) {
                            Ok(event) => Some(Ok(event)),
                            Err(error) => Some(Err(error)),
                        }
                    }
                    Err(error) => Some(Err(error)),
                });
                match pending_cancel {
                    Some(Ok(event)) => {
                        self.restart_project_store_after_close = true;
                        self.handle_project_store_event(event);
                    }
                    Some(Err(error)) => {
                        tracing::warn!(?error, "project cancellation request failed");
                    }
                    None => {}
                }
            }
            ApplicationEvent::ProjectSaved { .. } | ApplicationEvent::ProjectSavedAs { .. }
                if self.close_after_project_save =>
            {
                if self.project_dirty() {
                    self.close_after_project_save = false;
                    self.egui_ui.close_prompt_open = true;
                    self.project_status_message =
                        Some("Project changed while saving; save again before closing.".to_owned());
                } else {
                    self.close_after_project_save = false;
                    if let Some(path) = self.pending_dataset_open_path.take() {
                        self.egui_ui.close_prompt_open = false;
                        if let Err(error) = self.replace_state_from_dataset_path(path, None) {
                            self.project_status_message =
                                Some(format!("Dataset open could not start: {error}"));
                        }
                    } else {
                        self.request_project_store_close_for_exit();
                    }
                }
            }
            ApplicationEvent::OperationCompleted { token, outcome }
                if self.close_after_project_save
                    && matches!(
                        token.kind(),
                        OperationKind::ProjectSave | OperationKind::ProjectSaveAs
                    )
                    && matches!(
                        outcome,
                        mirante4d_application::OperationOutcome::Cancelled
                            | mirante4d_application::OperationOutcome::Failed(_)
                    ) =>
            {
                self.close_after_project_save = false;
                self.egui_ui.close_prompt_open = true;
            }
            ApplicationEvent::CurrentSourceReplaced { .. } => {
                self.project_recovery_candidates.clear();
                self.project_recovery_review = None;
                self.project_recovery_panel_open = false;
                self.pending_recovery_selection = None;
                self.pending_project_open_locator = None;
                self.pending_analysis_artifact_load = None;
                self.project_store_noninteractive_paths =
                    ProjectStoreNoninteractivePaths::default();
                self.project_store_product_evidence = ProjectStoreProductEvidence::default();
                self.pending_dataset_open_path = None;
                self.project_status_message = self.project_recovery_root.is_none().then(|| {
                    "Project recovery is unavailable for this source; provisional autosave is disabled."
                        .to_owned()
                });
            }
            _ => {}
        }
    }

    fn observe_source_application_event(&mut self, event: &ApplicationEvent) {
        match event {
            ApplicationEvent::SourceVerificationRequested { token } => {
                let path = self.dataset.selected_path().to_path_buf();
                let resource_policy = self.application.snapshot().resource_policy();
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
                let snapshot = self.application.snapshot();
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
        match self
            .application
            .dispatch(ApplicationCommand::CompleteOperation { token, completion })
        {
            Ok(_) => true,
            Err(fault) if fault.code() == ApplicationFaultCode::OperationNotFound => false,
            Err(fault) => {
                tracing::warn!(?fault, "source operation completion was rejected");
                match self
                    .application
                    .dispatch(ApplicationCommand::CancelOperation(operation_id))
                {
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
        match self
            .application
            .dispatch(ApplicationCommand::CompleteOperation { token, completion })
        {
            Ok(_) => true,
            Err(fault) if fault.code() == ApplicationFaultCode::OperationNotFound => false,
            Err(fault) => {
                tracing::warn!(?fault, "project persistence completion was rejected");
                match self
                    .application
                    .dispatch(ApplicationCommand::CancelOperation(operation_id))
                {
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

    fn complete_project_store_operation(
        &mut self,
        token: OperationToken,
        completion: OperationCompletion,
    ) -> bool {
        if self.complete_project_operation(token, completion) {
            return true;
        }
        self.project_status_message = Some(
            "Project storage completed after its application request became stale. Further project I/O is disabled until the application is reopened."
                .to_owned(),
        );
        if matches!(
            self.application.snapshot().workspace(),
            WorkspaceSnapshot::Unbound { .. }
        ) {
            self.request_project_store_restart();
            return false;
        }
        let close_error = self.project_store.as_mut().and_then(|service| {
            (!matches!(
                service.status().lifecycle(),
                ProjectStoreLifecycle::Closing | ProjectStoreLifecycle::Closed
            ))
            .then(|| service.close().err())
            .flatten()
        });
        if let Some(error) = close_error {
            tracing::error!(?error, "failed to close incoherent project-store service");
        }
        false
    }

    fn complete_project_store_fault(
        &mut self,
        token: OperationToken,
        fault: ProjectStoreFault,
    ) -> bool {
        let completion = if fault == ProjectStoreFault::Cancelled {
            OperationCompletion::Cancelled
        } else {
            OperationCompletion::Failed(project_failure_code(token.kind(), &fault))
        };
        if self.complete_project_store_operation(token, completion) {
            self.project_status_message = Some(format!("Project operation failed: {fault}"));
            true
        } else {
            false
        }
    }

    fn begin_background_operation(&mut self, kind: OperationKind) -> Option<OperationToken> {
        let before = self
            .application
            .snapshot()
            .active_operations()
            .iter()
            .map(OperationToken::operation_id)
            .collect::<Vec<_>>();
        if let Err(fault) = self
            .application
            .dispatch(ApplicationCommand::BeginOperation(kind))
        {
            tracing::warn!(?fault, ?kind, "background operation was rejected");
            return None;
        }
        self.application
            .snapshot()
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
        match self
            .application
            .dispatch(ApplicationCommand::CompleteOperation { token, completion })
        {
            Ok(_) => true,
            Err(fault) => {
                tracing::warn!(?fault, "background operation completion was rejected");
                let _ = self
                    .application
                    .dispatch(ApplicationCommand::CancelOperation(operation_id));
                false
            }
        }
    }

    fn poll_project_store(&mut self) {
        let snapshot = self.application.snapshot();
        let result = self
            .project_store
            .as_mut()
            .map(|service| service.drive(&snapshot, |_| Ok(Vec::new())));
        let events = match result {
            None => return,
            Some(Ok(events)) => events,
            Some(Err(error)) => {
                tracing::error!(?error, "project-store service failed");
                self.project_status_message = Some(format!("Project storage failed: {error:?}"));
                return;
            }
        };
        for event in events {
            self.handle_project_store_event(event);
        }
    }

    fn handle_project_store_event(&mut self, event: ProjectStoreServiceEvent) {
        match event {
            ProjectStoreServiceEvent::Opened {
                token,
                projection,
                candidates,
                opens_dirty,
            } => {
                let completion = if opens_dirty {
                    OperationCompletion::ProjectRecovered(projection)
                } else {
                    OperationCompletion::ProjectOpened(projection)
                };
                if !self.complete_project_store_operation(token, completion) {
                    return;
                }
                self.project_recovery_candidates = candidates;
                self.project_recovery_review = None;
                self.project_status_message = Some(if opens_dirty {
                    "Opened provisional recovery as an unsaved project.".to_owned()
                } else {
                    "Project opened.".to_owned()
                });
                if opens_dirty {
                    self.analysis_runtime.clear_loaded();
                } else {
                    self.request_current_analysis_artifacts();
                }
            }
            ProjectStoreServiceEvent::RecoveryReviewRequired {
                token,
                candidates,
                automatic_newer,
            } => {
                self.project_recovery_candidates = candidates;
                self.project_recovery_review = Some(ProjectRecoveryReview {
                    token,
                    automatic_newer,
                });
                self.project_status_message =
                    Some("A newer same-project autosave is available.".to_owned());
            }
            ProjectStoreServiceEvent::OpenFailed {
                token,
                fault,
                candidates,
            } => {
                let restart_needed = candidates.is_empty()
                    && self
                        .project_store
                        .as_ref()
                        .is_some_and(|service| !service.can_open());
                self.project_recovery_candidates = candidates;
                self.project_recovery_panel_open = !self.project_recovery_candidates.is_empty();
                if self.complete_project_store_fault(token, fault) && restart_needed {
                    self.request_project_store_restart();
                }
            }
            ProjectStoreServiceEvent::Created {
                token,
                saved_revision,
            } => {
                if !self.complete_project_store_operation(
                    token,
                    OperationCompletion::ProjectSaved(saved_revision),
                ) {
                    return;
                }
                self.project_store_product_evidence
                    .initial_save_captured_revision = Some(saved_revision);
                self.project_recovery_candidates.clear();
                self.project_recovery_panel_open = false;
                self.project_status_message = Some("Project saved.".to_owned());
            }
            ProjectStoreServiceEvent::ManualSaved { token, receipt } => {
                if !self.complete_project_store_operation(
                    token,
                    OperationCompletion::ProjectSaved(receipt.captured_revision()),
                ) {
                    return;
                }
                self.project_recovery_candidates.clear();
                self.project_recovery_panel_open = false;
                self.project_status_message = Some("Project saved.".to_owned());
            }
            ProjectStoreServiceEvent::AnalysisCommitted {
                token,
                projection,
                receipt: _,
            } => {
                let descriptors = self.analysis_runtime.staged_descriptors(&token);
                let (table, plot) = match descriptors {
                    Ok(descriptors) => descriptors,
                    Err(error) => {
                        tracing::error!(%error, "durable analysis descriptors were unavailable");
                        let _ = self.analysis_runtime.drop_commit(&token);
                        self.complete_background_operation(
                            token.clone(),
                            OperationCompletion::Failed(
                                OperationFailureCode::AnalysisExecutionFailed,
                            ),
                        );
                        if let Some(service) = self.project_store.as_mut() {
                            let _ = service.close();
                        }
                        self.project_status_message = Some(
                            "Analysis was saved, but the application could not admit it. Reopen the project before further project I/O."
                                .to_owned(),
                        );
                        return;
                    }
                };
                if self.complete_project_store_operation(
                    token.clone(),
                    OperationCompletion::AnalysisCommitted {
                        projection,
                        table,
                        plot,
                    },
                ) {
                    if let Err(error) = self.analysis_runtime.finish_commit(&token) {
                        tracing::error!(%error, "durable analysis values could not be installed");
                        self.project_status_message = Some(
                            "Analysis was saved, but its values could not be shown until reopen."
                                .to_owned(),
                        );
                    } else {
                        self.project_status_message = Some("Analysis saved.".to_owned());
                    }
                } else {
                    let _ = self.analysis_runtime.drop_commit(&token);
                    self.complete_background_operation(
                        token,
                        OperationCompletion::Failed(OperationFailureCode::AnalysisExecutionFailed),
                    );
                }
            }
            ProjectStoreServiceEvent::SavedAs {
                token,
                projection,
                receipt: _,
            } => {
                if !self.complete_project_store_operation(
                    token,
                    OperationCompletion::ProjectSavedAs(projection),
                ) {
                    return;
                }
                self.project_recovery_candidates.clear();
                self.project_recovery_panel_open = false;
                self.project_status_message = Some("Project saved as a new project.".to_owned());
            }
            ProjectStoreServiceEvent::RecoveryCandidatesListed { candidates } => {
                self.project_recovery_candidates = candidates;
            }
            ProjectStoreServiceEvent::RecoveryInspectionFailed { fault } => {
                self.project_status_message = Some(format!("Recovery inspection failed: {fault}"));
            }
            ProjectStoreServiceEvent::RecoveryOpened {
                token,
                generation_id: _,
                projection,
            } => {
                if !self.complete_project_store_operation(
                    token,
                    OperationCompletion::ProjectRecovered(projection),
                ) {
                    return;
                }
                self.project_recovery_review = None;
                self.project_recovery_panel_open = false;
                self.analysis_runtime.clear_loaded();
                self.project_status_message =
                    Some("Recovery opened as an unsaved Save-As-only project.".to_owned());
            }
            ProjectStoreServiceEvent::RecoverySelectionFailed {
                token,
                fault,
                normal_open_still_available,
            } => {
                if normal_open_still_available {
                    self.project_status_message =
                        Some(format!("Recovery selection failed: {fault}"));
                } else {
                    self.complete_project_store_fault(token, fault);
                }
            }
            ProjectStoreServiceEvent::ArtifactsLoaded {
                project_id,
                revision,
                currentness,
                generation_id: _,
                result,
            } => {
                self.pending_analysis_artifact_load = None;
                match result {
                    Ok(artifacts) => {
                        let snapshot = self.application.snapshot();
                        let expected_source = match snapshot.source() {
                            SourceVerificationSnapshot::Verified(source) => Some(source.clone()),
                            SourceVerificationSnapshot::Required
                            | SourceVerificationSnapshot::Verifying { .. } => None,
                        };
                        let staged = expected_source
                            .ok_or_else(|| {
                                anyhow::anyhow!("the analysis source is no longer verified")
                            })
                            .and_then(|source| {
                                self.analysis_runtime
                                    .stage_authenticated_bundles(artifacts, &source)
                            })
                            .and_then(|bundles| {
                                self.application
                                    .dispatch(
                                        ApplicationCommand::InstallLoadedAnalysisDescriptors {
                                            project_id,
                                            revision,
                                            currentness,
                                            bundles,
                                        },
                                    )
                                    .map_err(|fault| {
                                        anyhow::anyhow!(
                                            "saved analysis descriptors were rejected: {fault:?}"
                                        )
                                    })?;
                                self.analysis_runtime.finish_authenticated_bundles()
                            });
                        if let Err(error) = staged {
                            self.analysis_runtime.drop_authenticated_bundles();
                            self.project_status_message =
                                Some(format!("Saved analysis values could not be shown: {error}"));
                        }
                    }
                    Err(fault) => {
                        self.analysis_runtime.drop_authenticated_bundles();
                        self.project_status_message = Some(format!(
                            "Saved analysis values could not be loaded: {fault}"
                        ));
                    }
                }
            }
            ProjectStoreServiceEvent::OperationFailed { token, fault }
                if token.kind() == OperationKind::Analysis =>
            {
                let _ = self.analysis_runtime.drop_commit(&token);
                self.complete_project_store_fault(token, fault);
            }
            ProjectStoreServiceEvent::OperationFailed { token, fault } => {
                self.complete_project_store_fault(token, fault);
            }
            ProjectStoreServiceEvent::AutosaveSubmitted { .. } => {
                self.project_status_message = Some("Autosaving project…".to_owned());
            }
            ProjectStoreServiceEvent::AutosaveFinished { result, .. } => match result {
                Ok(receipt) => {
                    self.project_store_product_evidence
                        .latest_autosave_captured_revision = Some(receipt.captured_revision());
                    self.project_store_product_evidence
                        .autosave_elapsed_from_durable_edit_ms = self
                        .project_store_product_evidence
                        .durable_edit_started_at
                        .map(|started| {
                            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
                        });
                    self.project_status_message = Some("Project autosaved.".to_owned());
                }
                Err(fault) => {
                    self.project_status_message = Some(format!("Autosave failed: {fault}"));
                }
            },
            ProjectStoreServiceEvent::CancellationAcknowledged { .. } => {}
            ProjectStoreServiceEvent::Closed { result, .. } => {
                let close_succeeded = result.is_ok();
                self.project_store_product_evidence.close_result = Some(match result {
                    Ok(()) => ProjectStoreRecordedResult::Succeeded,
                    Err(fault) => {
                        tracing::warn!(%fault, "project-store close reported a failure");
                        ProjectStoreRecordedResult::Failed(fault.to_string())
                    }
                });
                self.project_store_product_evidence.actor_join =
                    Some(match self.project_store.take() {
                        Some(service) => match service.join() {
                            Ok(()) => ProjectStoreRecordedResult::Succeeded,
                            Err(error) => {
                                tracing::warn!(?error, "project-store actor join failed");
                                ProjectStoreRecordedResult::Failed(format!("{error:?}"))
                            }
                        },
                        None => ProjectStoreRecordedResult::Failed(
                            "project-store actor was unavailable at close completion".to_owned(),
                        ),
                    });
                if self.exit_after_project_close {
                    self.exit_after_project_close = false;
                    self.restart_project_store_after_close = false;
                    if let Some(pending) = self.pending_source_install.take() {
                        if let Err(error) = pending.runtime.dataset.request_shutdown() {
                            tracing::warn!(%error, "prepared dataset shutdown before exit failed");
                        }
                        std::thread::spawn(move || drop(pending.runtime));
                    }
                    self.pending_viewport_close = true;
                } else if self.pending_source_install.is_some() && close_succeeded {
                    self.finish_pending_source_install();
                } else if self.pending_source_install.is_some() {
                    self.abort_pending_source_install(
                        "The current project could not close; the new dataset was not installed.",
                    );
                } else if self.restart_project_store_after_close {
                    self.restart_project_store_after_close = false;
                    self.restart_unbound_project_store();
                }
            }
        }
    }

    fn complete_project_fault(&mut self, token: OperationToken, fault: ProjectStoreFault) {
        let completion = if fault == ProjectStoreFault::Cancelled {
            OperationCompletion::Cancelled
        } else {
            OperationCompletion::Failed(project_failure_code(token.kind(), &fault))
        };
        self.project_status_message = Some(format!("Project operation failed: {fault}"));
        self.complete_project_operation(token, completion);
    }

    fn project_dirty(&self) -> bool {
        self.application.snapshot().dirty().unwrap_or(false)
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if self.pending_viewport_close {
            self.pending_viewport_close = false;
            self.egui_ui.allow_close_without_prompt = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if !ctx.input(|input| input.viewport().close_requested()) {
            return;
        }
        if self.egui_ui.allow_close_without_prompt {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if self.project_dirty() {
            self.egui_ui.close_prompt_open = true;
        } else {
            self.request_project_store_close_for_exit();
        }
    }

    fn show_dirty_project_close_prompt(&mut self, ctx: &egui::Context) {
        if !self.egui_ui.close_prompt_open {
            return;
        }
        egui::Window::new("Unsaved Project")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(if self.pending_dataset_open_path.is_some() {
                    "Project changes have not been saved. Save or discard them before opening another dataset."
                } else {
                    "Project changes have not been saved."
                });
                ui.horizontal(|ui| {
                    let (save_available, save_as_required) = self
                        .project_store
                        .as_ref()
                        .map(|service| {
                            let can_save = service.can_save();
                            let can_save_as = service.can_save_as();
                            (can_save || can_save_as, !can_save && can_save_as)
                        })
                        .unwrap_or((false, false));
                    let save_label = if save_as_required { "Save As" } else { "Save" };
                    if ui_kit::toolbar_button(ui, save_label, save_available).clicked() {
                        self.close_after_project_save = true;
                        let started = if save_as_required {
                            self.save_current_project_as()
                        } else {
                            self.save_current_project()
                        };
                        if started && self.close_after_project_save {
                            self.egui_ui.close_prompt_open = false;
                        } else if !started {
                            self.close_after_project_save = false;
                        }
                    }
                    if ui_kit::toolbar_button(ui, "Discard", true).clicked() {
                        self.egui_ui.close_prompt_open = false;
                        self.close_after_project_save = false;
                        if let Some(path) = self.pending_dataset_open_path.take() {
                            if let Err(error) = self.replace_state_from_dataset_path(path, None) {
                                self.project_status_message =
                                    Some(format!("Dataset open could not start: {error}"));
                            }
                        } else {
                            self.request_project_store_close_for_exit();
                        }
                    }
                    if ui_kit::toolbar_button(ui, "Cancel", true).clicked() {
                        self.egui_ui.close_prompt_open = false;
                        self.egui_ui.allow_close_without_prompt = false;
                        self.close_after_project_save = false;
                        self.pending_dataset_open_path = None;
                        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    }
                });
            });
    }

    fn show_project_recovery_ui(&mut self, ctx: &egui::Context) {
        #[derive(Clone, Copy)]
        enum ReviewAction {
            Recover(ProjectGenerationId),
            OpenSaved,
        }

        let review = self
            .project_recovery_review
            .as_ref()
            .map(|review| review.automatic_newer);
        let mut review_action = None;
        if let Some(automatic_newer) = review {
            egui::Window::new("Recover autosaved project?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label(
                        "A newer autosave was found for the saved project. Recovery opens it as an unsaved branch that must be saved with Save As.",
                    );
                    ui.horizontal(|ui| {
                        if ui_kit::toolbar_button(ui, "Recover Autosave", true).clicked() {
                            review_action = Some(ReviewAction::Recover(automatic_newer));
                        }
                        if ui_kit::toolbar_button(ui, "Open Saved Project", true).clicked() {
                            review_action = Some(ReviewAction::OpenSaved);
                        }
                    });
                });
        }
        match review_action {
            Some(ReviewAction::Recover(generation_id)) => {
                self.recover_project_candidate(generation_id);
            }
            Some(ReviewAction::OpenSaved) => self.accept_saved_project_after_recovery_review(),
            None => {}
        }

        if !self.project_recovery_panel_open {
            return;
        }
        let candidates = self.project_recovery_candidates.clone();
        let locators = self
            .project_store
            .as_ref()
            .map(|service| service.recovery_store_project_ids().collect::<Vec<_>>())
            .unwrap_or_default();
        let can_open_locator = self
            .project_store
            .as_ref()
            .is_some_and(ProjectStoreApplicationService::can_open);
        let mut panel_open = true;
        let mut selected = None;
        let mut selected_locator = None;
        egui::Window::new("Project Recovery")
            .open(&mut panel_open)
            .resizable(true)
            .default_width(520.0)
            .show(ctx, |ui| {
                if candidates.is_empty() && locators.is_empty() {
                    ui.label("No validated recovery branches are available.");
                    return;
                }
                ui.label(
                    "Recovery never changes the stored project. A selected branch opens dirty and must be saved with Save As.",
                );
                ui.separator();
                if !locators.is_empty() {
                    ui.heading("Unsaved projects from earlier launches");
                    for project_id in &locators {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(format!("Project {project_id}"));
                            if ui_kit::toolbar_button(
                                ui,
                                "Inspect and Recover",
                                can_open_locator,
                            )
                            .on_hover_text("Validated by the project-store actor before opening")
                            .clicked()
                            {
                                selected_locator = Some(*project_id);
                            }
                        });
                    }
                    if !candidates.is_empty() {
                        ui.separator();
                    }
                }
                if !candidates.is_empty() {
                    ui.heading("Branches in the selected project");
                }
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        for candidate in &candidates {
                            ui.group(|ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(format!(
                                        "{} · {} · revision {}",
                                        candidate.classification(),
                                        candidate.origin(),
                                        candidate.revision_sequence()
                                    ));
                                    if ui_kit::toolbar_button(ui, "Open Recovery", true).clicked()
                                    {
                                        selected = Some(candidate.generation_id());
                                    }
                                });
                                ui_kit::muted_label(
                                    ui,
                                    format!(
                                        "generation {} · artifacts {} ({} non-regenerable)",
                                        candidate.generation_sequence(),
                                        candidate.artifact_count(),
                                        candidate.non_regenerable_artifact_count()
                                    ),
                                );
                            });
                        }
                    });
            });
        self.project_recovery_panel_open = panel_open;
        if let Some(generation_id) = selected {
            self.project_recovery_panel_open = false;
            self.recover_project_candidate(generation_id);
        } else if let Some(project_id) = selected_locator {
            self.project_recovery_panel_open = false;
            self.open_recovery_locator(project_id);
        }
    }

    fn open_project_recovery_panel(&mut self) {
        self.project_recovery_panel_open = true;
        let can_inspect = self.project_store.as_ref().is_some_and(|service| {
            let status = service.status();
            !status.foreground_active()
                && !status.autosave_active()
                && matches!(
                    status.lifecycle(),
                    ProjectStoreLifecycle::Provisional
                        | ProjectStoreLifecycle::Established
                        | ProjectStoreLifecycle::RecoveryOnly
                        | ProjectStoreLifecycle::RecoverySelected
                )
        });
        if !can_inspect {
            return;
        }
        match self
            .project_store
            .as_mut()
            .expect("inspection eligibility requires a project-store service")
            .submit_inspect_recovery()
        {
            Ok(_) => {
                self.project_recovery_candidates.clear();
                self.project_status_message = Some("Inspecting project recovery…".to_owned());
            }
            Err(error) => {
                self.project_status_message =
                    Some(format!("Recovery inspection could not start: {error:?}"));
            }
        }
    }

    fn request_project_store_close_for_exit(&mut self) {
        self.exit_after_project_close = true;
        self.egui_ui.close_prompt_open = false;
        match self.project_store.as_mut() {
            Some(service) if service.status().lifecycle() != ProjectStoreLifecycle::Closed => {
                if let Err(error) = service.close()
                    && !matches!(
                        error,
                        mirante4d_application::ProjectStoreServiceError::Closing
                    )
                {
                    self.exit_after_project_close = false;
                    self.project_status_message =
                        Some(format!("Could not close project storage: {error:?}"));
                }
            }
            Some(_) => {
                if let Some(service) = self.project_store.take()
                    && let Err(error) = service.join()
                {
                    tracing::warn!(?error, "project-store actor join failed");
                }
                self.exit_after_project_close = false;
                self.pending_viewport_close = true;
            }
            None => {
                self.exit_after_project_close = false;
                self.pending_viewport_close = true;
            }
        }
    }

    fn request_project_store_restart(&mut self) {
        self.restart_project_store_after_close = true;
        let close_result = match self.project_store.as_mut() {
            Some(service)
                if !matches!(
                    service.status().lifecycle(),
                    ProjectStoreLifecycle::Closing | ProjectStoreLifecycle::Closed
                ) =>
            {
                service.close().map(|_| ())
            }
            Some(_) => Ok(()),
            None => {
                self.restart_project_store_after_close = false;
                self.restart_unbound_project_store();
                return;
            }
        };
        if let Err(error) = close_result {
            self.restart_project_store_after_close = false;
            self.project_status_message =
                Some(format!("Project storage could not restart: {error:?}"));
        }
    }

    fn restart_unbound_project_store(&mut self) {
        let snapshot = self.application.snapshot();
        let WorkspaceSnapshot::Unbound { workspace } = snapshot.workspace() else {
            self.project_status_message = Some(
                "Project storage is unavailable for the bound project; reopen the application before saving again."
                    .to_owned(),
            );
            return;
        };
        match start_project_store_service(
            self.project_recovery_root.as_deref(),
            workspace.provisional_project_id(),
        ) {
            Ok((service, warning)) => {
                self.project_store = Some(service);
                if warning.is_some() {
                    self.project_status_message = warning;
                }
            }
            Err(fault) => {
                self.project_status_message =
                    Some(format!("Project storage could not restart: {fault}"));
            }
        }
    }

    fn open_session_from_dialog(&mut self, _ctx: &egui::Context) {
        if let Err(fault) = self
            .application
            .dispatch(ApplicationCommand::RequestProjectOpen)
        {
            tracing::info!(?fault, "project open is unavailable for the current source");
        }
        self.pump_application_services();
    }

    fn open_recovery_locator(&mut self, project_id: ProjectId) {
        self.pending_project_open_locator = Some(project_id);
        if let Err(fault) = self
            .application
            .dispatch(ApplicationCommand::RequestProjectOpen)
        {
            self.pending_project_open_locator = None;
            self.project_status_message = Some(format!(
                "Recovery cannot open for the current dataset: {fault:?}"
            ));
            return;
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
        if let Err(error) = self.open_or_queue_dataset_path(path, Some(ctx)) {
            tracing::warn!(%error, "dataset open request was rejected");
        }
    }

    fn open_or_queue_dataset_path(
        &mut self,
        path: PathBuf,
        ctx: Option<&egui::Context>,
    ) -> anyhow::Result<bool> {
        if self.project_dirty() {
            self.pending_dataset_open_path = Some(path);
            self.egui_ui.close_prompt_open = true;
            Ok(false)
        } else {
            self.replace_state_from_dataset_path(path, ctx)?;
            Ok(true)
        }
    }

    fn replace_state_from_dataset_path(
        &mut self,
        path: PathBuf,
        ctx: Option<&egui::Context>,
    ) -> anyhow::Result<()> {
        self.application
            .dispatch(ApplicationCommand::RequestDatasetOpen)
            .map_err(|fault| anyhow::anyhow!("dataset open command rejected: {fault:?}"))?;
        let events = self.application.drain_events(256);
        let token = events
            .iter()
            .find_map(|event| match event {
                ApplicationEvent::DatasetOpenRequested { token } => Some(token.clone()),
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("dataset open request emitted no operation token"))?;
        let resource_policy = self.application.snapshot().resource_policy();
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

    fn save_current_project(&mut self) -> bool {
        if let Err(fault) = self
            .application
            .dispatch(ApplicationCommand::RequestProjectSave)
        {
            tracing::info!(?fault, "project save is unavailable for the current source");
            return false;
        }
        self.pump_application_services();
        true
    }

    fn save_current_project_as(&mut self) -> bool {
        let new_project_id = ProjectId::from_bytes(*uuid::Uuid::new_v4().as_bytes());
        if let Err(fault) = self
            .application
            .dispatch(ApplicationCommand::RequestProjectSaveAs { new_project_id })
        {
            tracing::info!(
                ?fault,
                "project Save As is unavailable for the current source"
            );
            return false;
        }
        self.pump_application_services();
        true
    }

    fn new_current_project(&mut self) {
        if let Err(fault) = self
            .application
            .dispatch(ApplicationCommand::AttachVerifiedDataset)
        {
            tracing::info!(?fault, "new project is unavailable for the current source");
        }
        self.pump_application_services();
    }

    fn accept_saved_project_after_recovery_review(&mut self) {
        let Some(review) = self.project_recovery_review.as_ref() else {
            return;
        };
        let event = self
            .project_store
            .as_mut()
            .and_then(|service| service.accept_normal_open(review.token.operation_id()).ok());
        match event {
            Some(event) => self.handle_project_store_event(event),
            None => {
                self.project_status_message =
                    Some("Could not accept the saved project state.".to_owned());
            }
        }
    }

    fn recover_project_candidate(&mut self, generation_id: ProjectGenerationId) {
        if let Some(review) = self.project_recovery_review.as_ref() {
            let result = self
                .project_store
                .as_mut()
                .ok_or(ProjectStoreFault::Corruption {
                    stage: "application_service_unavailable",
                })
                .and_then(|service| {
                    service
                        .submit_open_recovery(review.token.clone(), generation_id)
                        .map_err(project_service_error_fault)
                });
            if let Err(fault) = result {
                self.project_status_message = Some(format!("Could not open recovery: {fault}"));
            }
            return;
        }
        self.pending_recovery_selection = Some(generation_id);
        if let Err(fault) = self
            .application
            .dispatch(ApplicationCommand::RequestProjectRecovery)
        {
            self.pending_recovery_selection = None;
            tracing::info!(?fault, "project recovery is unavailable");
        }
        self.pump_application_services();
    }

    fn request_current_source_verification(&mut self) {
        if let Err(fault) = self
            .application
            .dispatch(ApplicationCommand::RequestSourceVerification)
        {
            tracing::warn!(?fault, "source verification request was rejected");
        }
    }

    fn try_start_pending_automatic_source_verification(&mut self) {
        let Some(pending_generation) = self.pending_automatic_source_verification else {
            return;
        };
        let snapshot = self.application.snapshot();
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

        match self
            .application
            .dispatch(ApplicationCommand::RequestSourceVerification)
        {
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
        let retired_leases = self.dataset.take_retained_leases();
        self.clear_product_presentations();
        self.render_coordination.frame_fidelity.completeness = FrameCompleteness::Loading;
        self.render_coordination.frame_fidelity.reason = LodDecisionReason::LoadingTargetScale;
        self.render_coordination.frame_fidelity.backend = RenderBackend::Loading;
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
                let token_is_current = self
                    .application
                    .snapshot()
                    .active_operations()
                    .iter()
                    .any(|active| active == &token);
                if !token_is_current {
                    tracing::warn!("stale prepared dataset was suppressed before project close");
                    if let Err(error) = runtime.dataset.request_shutdown() {
                        tracing::warn!(%error, "stale dataset runtime shutdown request failed");
                    }
                    std::thread::spawn(move || drop(runtime));
                    return;
                }
                let pending = PendingSourceInstall {
                    token,
                    runtime,
                    completion,
                };
                let store_is_closed = self.project_store.as_ref().is_none_or(|service| {
                    service.status().lifecycle() == ProjectStoreLifecycle::Closed
                });
                if store_is_closed {
                    self.pending_source_install = Some(pending);
                    self.finish_pending_source_install();
                } else {
                    self.pending_source_install = Some(pending);
                    let close_error = if let Some(service) = self.project_store.as_mut()
                        && service.status().lifecycle() != ProjectStoreLifecycle::Closing
                    {
                        service.close().err()
                    } else {
                        None
                    };
                    if let Some(error) = close_error {
                        tracing::warn!(?error, "current project close request failed");
                        self.abort_pending_source_install(
                            "The current project could not close; the new dataset was not installed.",
                        );
                    }
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

    fn finish_pending_source_install(&mut self) {
        if let Some(service) = self.project_store.take()
            && let Err(error) = service.join()
        {
            tracing::warn!(?error, "replaced project-store actor join failed");
        }
        let Some(pending) = self.pending_source_install.take() else {
            return;
        };
        if self.complete_source_operation(pending.token, pending.completion) {
            self.install_current_source_runtime(pending.runtime);
            let snapshot = self.application.snapshot();
            let WorkspaceSnapshot::Unbound { workspace } = snapshot.workspace() else {
                tracing::error!("new source did not produce an unbound project workspace");
                return;
            };
            let (project_recovery_root, recovery_root_warning) =
                initialize_project_recovery_root(self.dataset.selected_path());
            self.project_recovery_root = project_recovery_root;
            match start_project_store_service(
                self.project_recovery_root.as_deref(),
                workspace.provisional_project_id(),
            ) {
                Ok((service, discovery_warning)) => {
                    self.project_store = Some(service);
                    let warning = recovery_root_warning.or(discovery_warning);
                    if warning.is_some() {
                        self.project_status_message = warning;
                    }
                }
                Err(fault) => {
                    self.project_status_message =
                        Some(format!("Project storage could not start: {fault}"));
                }
            }
        } else {
            tracing::warn!("stale dataset open result was suppressed");
            self.project_status_message = Some(
                "The prepared dataset became stale; the current project remains closed and the application must be reopened before saving again."
                    .to_owned(),
            );
            if let Err(error) = pending.runtime.dataset.request_shutdown() {
                tracing::warn!(%error, "stale dataset runtime shutdown request failed");
            }
            std::thread::spawn(move || drop(pending.runtime));
        }
    }

    fn abort_pending_source_install(&mut self, message: &str) {
        let Some(pending) = self.pending_source_install.take() else {
            return;
        };
        self.project_status_message = Some(message.to_owned());
        self.complete_source_operation(
            pending.token,
            OperationCompletion::Failed(OperationFailureCode::DatasetReadFailed),
        );
        if let Err(error) = pending.runtime.dataset.request_shutdown() {
            tracing::warn!(%error, "rejected dataset runtime shutdown request failed");
        }
        std::thread::spawn(move || drop(pending.runtime));
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
                match self.application.dispatch(
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
        let old_dataset = std::mem::replace(&mut self.dataset, transfer.dataset);
        if let Err(error) = old_dataset.request_shutdown() {
            tracing::warn!(%error, "unverified dataset runtime shutdown request failed");
        }
        self.request_opened_state_visible_work(None);
        std::thread::spawn(move || drop(old_dataset));
    }

    fn install_current_source_runtime(
        &mut self,
        transfer: current_source_open_service::CurrentSourceRuntimeTransfer,
    ) {
        let current_source_open_service::CurrentSourceRuntimeTransfer {
            dataset,
            render_coordination,
            analysis_runtime,
        } = transfer;
        self.clear_product_presentations();
        let old_dataset = std::mem::replace(&mut self.dataset, dataset);
        let old_render_coordination =
            std::mem::replace(&mut self.render_coordination, render_coordination);
        let old_analysis_runtime = std::mem::replace(&mut self.analysis_runtime, analysis_runtime);
        self.pending_analysis_artifact_load = None;
        self.import.clear_for_source_replacement();
        self.egui_ui.viewer_tools = ViewerToolState::default();
        self.egui_ui.analysis_plot_view = None;
        self.egui_ui.analysis_filter.clear();
        self.egui_ui.analysis_sort = None;
        self.egui_ui.hovered_pixel = None;
        self.egui_ui.hovered_source_readout = None;
        if let Err(error) = old_dataset.request_shutdown() {
            tracing::warn!(%error, "replaced dataset runtime shutdown request failed");
        }
        self.pending_automatic_source_verification =
            Some(self.application.snapshot().source_generation());
        self.try_start_pending_automatic_source_verification();
        self.request_opened_state_visible_work(None);

        std::thread::spawn(move || {
            drop((old_dataset, old_render_coordination, old_analysis_runtime));
        });
    }

    fn active_histogram_summary(&self, snapshot: &ApplicationSnapshot) -> LayerHistogramSummary {
        let view = application_view(snapshot);
        let active_key = view.active_layer();
        let layer = snapshot
            .catalog()
            .layer(active_key)
            .expect("application view closes over the dataset catalog");
        let scale = ScaleLevel::new(
            self.render_coordination
                .frame_fidelity
                .displayed_scale_level
                .unwrap_or(self.dataset.current_scale().get()),
        );
        active_layer_histogram_summary(
            self.dataset.retained_leases(),
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
        let before = self.application.snapshot();
        let previous_view = application_view(&before).clone();
        let previous_playback_active = before.transient().playback_active();
        let effect = self.application.dispatch(command)?;
        if effect == CommandEffect::Changed {
            let after = self.application.snapshot();
            self.reconcile_application_change(
                &previous_view,
                previous_playback_active,
                &after,
                ctx,
            );
            if let Err(error) = self.reconcile_analysis_currentness() {
                tracing::warn!(%error, "stale analysis could not be retired");
            }
        }
        self.pump_application_services();
        Ok(effect)
    }

    fn reconcile_application_change(
        &mut self,
        previous_view: &ViewState,
        previous_playback_active: bool,
        snapshot: &ApplicationSnapshot,
        ctx: &egui::Context,
    ) {
        let next_view = application_view(snapshot);
        let playback_lod_changed =
            previous_playback_active != snapshot.transient().playback_active();
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
            &mut self.render_coordination,
            &mut self.analysis_runtime,
        ) {
            Ok(changed) => changed,
            Err(error) => {
                tracing::error!(%error, "failed to reconcile the canonical view with current runtime");
                self.dataset.record_plan_error(error.to_string());
                self.render_coordination.frame_fidelity.completeness =
                    FrameCompleteness::Incomplete;
                false
            }
        };
        if previous_view.layout() != next_view.layout() {
            self.egui_ui.hovered_pixel = None;
            self.egui_ui.hovered_source_readout = None;
            self.egui_ui.viewport_orbit_drag = None;
            if next_view.layout() == CanonicalViewerLayout::Single3d {
                self.clear_cross_section_product_presentations();
                self.render_coordination.invalidate_cross_sections();
            }
        }
        if source_selection_changed {
            self.clear_3d_product_presentation();
            self.request_visible_bricks();
        } else {
            self.invalidate_cross_section_panel_display_frames();
            self.clear_3d_product_presentation();
            if let Err(error) = self.rerender_display_state() {
                tracing::error!(%error, "failed to render the accepted canonical view");
                self.dataset.record_plan_error(error.to_string());
                self.render_coordination.frame_fidelity.completeness =
                    FrameCompleteness::Incomplete;
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

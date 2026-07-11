#[cfg(test)]
use std::path::Path;
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

mod analysis_jobs;
mod analysis_workspace;
mod brick_streaming;
mod commands;
mod cross_section_read_queue;
mod cross_section_readout;
mod cross_section_runtime;
mod cross_section_scheduler;
mod cross_section_streaming;
mod dataset_opening;
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
mod preferences;
mod product_automation;
mod project_session;
mod project_store;
mod render_state;
mod resident_rendering;
mod runtime_diagnostics_panel;
mod scene_artifacts;
mod scene_extraction;
mod session_state;
mod smoke;
mod state;
mod tool_interactions;
mod tools;
mod transfer_presets;
mod ui_kit;
mod viewer_layout;
mod viewport;
mod workbench_brick_runtime;
mod workbench_controls;
mod workbench_import;
mod workbench_playback_runtime;
mod workbench_project;
mod workbench_ui;

#[cfg(test)]
use analysis_jobs::{
    AnalysisJobContext, AnalysisProgress, compute_active_roi_analysis,
    compute_full_time_series_analysis, run_full_time_series_analysis_job,
    run_roi_intensity_analysis_job,
};
use analysis_jobs::{
    AnalysisTask, AnalysisTaskKind, AnalysisTaskMessage, analysis_progress_fraction,
    analysis_task_label, analysis_task_status_text, spawn_analysis_task,
    store_analysis_task_output,
};
#[cfg(test)]
use analysis_workspace::{
    AnalysisPlotBounds, analysis_plot_bounds, analysis_plot_visible_bounds,
    analysis_table_preview_rows, nearest_analysis_plot_point,
    normalize_analysis_plot_view_for_plot, pan_analysis_plot_view, plot_screen_position,
    zoom_analysis_plot_view,
};
use analysis_workspace::{
    AnalysisPlotPointSelection, AnalysisPlotViewRange, AnalysisTableSort,
    export_selected_analysis_table, normalize_analysis_selection, show_analysis_workspace,
    show_analysis_workspace_window,
};
#[cfg(test)]
use brick_streaming::brick_runtime_work_active;
use brick_streaming::{
    BrickPrefetchRequestKey, BrickStreamRequestKey, BrickWarmRequestKey, PrefetchedBrickPayload,
    create_brick_read_pool,
};
#[cfg(test)]
use brick_streaming::{
    BrickSubmissionOptions, cancel_brick_tickets, current_brick_stream_request_key,
    prefetch_timepoints_for_state, spatial_warm_brick_candidates,
    submit_visible_bricks_to_pool_with_options,
};
use commands::{WorkbenchCommand, WorkbenchCommandOutcome};
use cross_section_read_queue::create_cross_section_read_pool;
use cross_section_readout::cross_section_hover_readout_for_response;
use cross_section_runtime::CrossSectionRuntime;
use cross_section_streaming::retire_cross_section_streaming_state;
pub use dataset_opening::{
    open_dataset_and_render_first_frame, open_dataset_with_preferences_and_render_first_frame,
};
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
use glam::DVec3;
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
    import_progress_message, import_task_status_text, pending_tiff_import_ready_to_start,
    prepare_tiff_source_import, show_tiff_channel_metadata_controls, show_tiff_grouping_controls,
    show_tiff_no_data_controls, tiff_import_storage_estimate_label, tiff_source_profile_label,
    tiff_voxel_spacing_metadata_label, validate_pending_tiff_import,
};
#[cfg(test)]
use import_ui::{
    import_tiff_source_options, no_data_policy_label, normalize_pending_tiff_no_data_policy,
    set_pending_tiff_no_data_policy, validate_tiff_import_options,
};
#[cfg(test)]
use layer_state::activate_layer_timepoint;
use layer_state::{
    activate_layer_timepoint_state_only, activate_streaming_timepoint_preserving_frame,
    default_dvr_opacity_transfer, layer_dvr_opacity_transfer, layer_render_state_for_mode,
    set_layer_display_state, set_layer_dvr_opacity_transfer_state, set_layer_render_state,
    set_layer_transfer_curve, set_layer_transfer_invert,
    sync_active_layer_render_state_from_runtime,
};
use lod_scheduler::update_visible_brick_plan;
#[cfg(test)]
use lod_scheduler::{
    MAX_LOD_CANDIDATE_VISIBLE_BRICKS, MIN_LOD_CANDIDATE_VISIBLE_BRICKS,
    lod_candidate_visible_brick_budget, select_stream_scale_for_state,
};
#[cfg(test)]
use mirante4d_analysis::{
    AnalysisCell, AnalysisExecutionClass, AnalysisOperationKind, AnalysisParameterValue,
    AnalysisProvenance, AnalysisResultState, AnnotationArtifact,
    SceneStyleRgba as AnalysisSceneStyleRgba, TrackArtifact, TrackPoint,
};
use mirante4d_analysis::{
    AnalysisOperationRecord, AnalysisPlot, AnalysisTable, IntensitySummary, SceneArtifactStore,
};
#[cfg(test)]
use mirante4d_analysis::{
    MeasurementArtifact, MeasurementGeometry as AnalysisMeasurementGeometry, MeasurementProvenance,
    RoiArtifact, SceneArtifactId, SceneArtifactTime, SceneEditCommand,
    WorldGeometry as AnalysisWorldGeometry,
};
use mirante4d_core::{
    CameraView, ChannelColor, ChannelTransferFunction, DisplayWindow, GridToWorld, IntensityDType,
    IsoLightMode, IsoLightState, LayerDisplay, LayerId, PresentationViewport, Projection, Shape3D,
    Shape4D, TRANSFER_GAMMA_MAX, TRANSFER_GAMMA_MIN, TimeIndex, TransferCurve, TransferPresetId,
};
use mirante4d_data::{
    BrickHistogramSample, BrickReadPool, BrickReadTicket, CrossSectionChunkReadPool, DatasetHandle,
    DenseVolumeF32, DenseVolumeU8, DenseVolumeU16, SpatialBrickIndex, VolumeBrickF32,
    VolumeBrickU8, VolumeBrickU16,
};
#[cfg(test)]
use mirante4d_data::{BrickReadOutcome, BrickReadStatus, BrickRequestPriority, CancellationToken};
#[cfg(test)]
use mirante4d_format::{NoDataPolicy, NoDataPolicyKind, NoDataVisibilityPolicy};
use mirante4d_import::{
    ImportCancellationToken, ImportError, TiffDirectoryImportReport, TiffImportSource,
    TiffSourceImportOptions, import_tiff_source_with_progress,
};
#[cfg(test)]
use mirante4d_import::{
    ImportProgressEvent, TiffChannelInspection, TiffDirectoryInspection, TiffImportReviewStatus,
    TiffImportStorageEstimate, TiffMetadataConfidence, TiffNoDataPolicyReview, TiffSourceProfile,
    TiffStackShape, TiffValueRangeSummary, accepted_tiff_reviewed_import_plan,
    default_tiff_channel_metadata_override, inspect_tiff_source_for_review,
};
#[cfg(test)]
use mirante4d_renderer::PickValue;
use mirante4d_renderer::{
    CrossSectionPanel, FrameDiagnostics, FrameDiagnosticsF32, MipImageF32, MipImageU16,
    RenderViewport,
    gpu::{GpuDisplayFrame, GpuRenderer},
};
#[cfg(test)]
use mirante4d_renderer::{
    IsoSurfaceFrameF32, IsoSurfaceNormal, PixelCoverage, RenderError, gpu::GpuRenderError,
};
#[cfg(test)]
use mirante4d_renderer::{PickCompleteness, PickHit, PickHitKind, PickPolicy, ScreenPosition};
use playback::{PlaybackState, playback_status_label, stepped_timepoint};
#[cfg(test)]
use preferences::APP_GIB;
use preferences::{APP_MIB, PREFERENCES_FORMAT, bytes_to_mib_rounded, mib_to_bytes};
pub use preferences::{
    AppPreferences, AppRuntimePreferences, default_app_preferences_for_system,
    default_preferences_path, load_app_preferences, write_app_preferences,
};
use product_automation::{ProductAutomationAppUpdateTiming, ProductAutomationController};
use project_session::ProjectDirtySnapshot;
#[cfg(test)]
use project_session::SESSION_FORMAT;
#[cfg(test)]
use project_session::{AnalysisTableArtifactPayload, AppSessionManifest};
pub use project_session::{
    AppDatasetReference, AppLayerDisplayState, AppRecoverySession, AppSession,
    parse_project_session_manifest, read_autosave_snapshot, read_session_file, session_from_state,
    write_autosave_snapshot, write_session_file,
};
pub use project_store::AppAnalysisArtifactReference;
#[cfg(test)]
use project_store::{
    PROJECT_AUTOSAVE_DIR, PROJECT_PLOTS_DIR, PROJECT_TABLES_DIR, autosave_project_json_path,
    dataset_reference_path_for_manifest, dataset_reference_path_from_manifest,
    project_artifact_dir, project_json_path,
    write_json_artifact_atomically_with_forced_commit_failure,
    write_project_json_atomically_with_forced_commit_failure,
};
#[cfg(test)]
use render_state::{
    ResidentRenderFailureStatus, f32_frame_to_display_u16, f32_frame_to_display_u16_for_mode,
    frame_failure_kind_for_gpu_error, placeholder_frame_for_mode, record_completed_frame_time,
    render_app_frame, request_lod_downgrade_after_resident_capacity_failure,
};
use render_state::{
    dense_startup_allowed, refresh_fidelity_resource_stats, rerender_state_with_backend,
    set_presentation_viewport, set_render_viewport, take_lod_replan_pending,
    update_channel_fidelity_status,
};
#[cfg(test)]
use resident_rendering::{
    cross_section_panel_render_request_for_state, render_state_from_resident_bricks,
};
use resident_rendering::{
    render_gpu_cross_section_panel_frame_from_global_runtime,
    render_gpu_display_frame_from_resident_bricks, render_state_from_resident_bricks_with_backend,
};
use scene_artifacts::show_scene_artifacts_editor;
#[cfg(test)]
use scene_artifacts::{
    EditableSceneArtifactKind, SceneEditHandle, SceneEditHandleId, normalize_world_geometry,
    refresh_measurement_result, remove_scene_artifact, select_scene_artifact,
    selected_scene_artifact_matches, update_scene_annotation_artifact, update_scene_roi_artifact,
    update_scene_track_artifact,
};
use scene_extraction::scene_draw_list_for_state;
#[cfg(test)]
use scene_extraction::selected_scene_handle_pick_targets;
#[cfg(test)]
use scene_extraction::{SCENE_HANDLE_LAYER_ID, world_geometry_edit_handles};
#[cfg(test)]
use session_state::{open_state_from_recovery_session, open_state_from_session};
use session_state::{
    open_state_from_session_with_preferences, open_state_from_session_with_relocated_dataset,
};
pub use session_state::{write_autosave_snapshot_for_state, write_session_file_for_state};
pub use smoke::{
    PlaybackSmokeFrame, render_first_streamed_frame_for_smoke, render_playback_steps_for_smoke,
};
pub use state::{
    AppLayerSummary, ChannelFidelityStatus, ChannelFidelityWarning, ChannelRenderState,
    DEFAULT_DVR_DENSITY_SCALE, DEFAULT_ISO_DISPLAY_LEVEL, DisplayedFrameFreshness,
    DvrOpacityTransfer, DvrRenderParameters as ChannelDvrRenderParameters, FrameCompleteness,
    FrameFailureKind, FrameFidelityStatus, HistogramStatus, IsoRenderParameters,
    LayerHistogramSummary, LodDecisionReason, LodScheduleState, MipRenderParameters, RenderBackend,
    RenderIsoShadingPolicy, RenderMode, RenderSamplingPolicy, RenderedIntensityChannel,
    ViewportHover, ViewportIntensity,
};
use state::{LayerHistogramCache, ResidentHistogramSampleKey};
use tool_interactions::apply_viewport_tool_response;
#[cfg(test)]
use tool_interactions::{
    apply_viewer_tool_commands, pick_hit_from_viewport_hover, update_world_geometry_from_handle,
};
#[cfg(test)]
use tools::{ToolSelection, ViewerToolCommand};
use tools::{ViewerTool, ViewerToolState};
#[cfg(test)]
use transfer_presets::fluorescence_palette_color;
pub use transfer_presets::{ChannelDisplayPreset, ChannelDisplayPresetEntry};
use transfer_presets::{
    apply_channel_display_preset, built_in_transfer_preset_curve, built_in_transfer_preset_id,
    built_in_transfer_preset_label, built_in_transfer_presets, channel_preset_from_current_state,
    default_channel_presets_from_layers, next_user_channel_preset_id, transfer_preset_label_for_id,
};
use ui_kit::{StatusTone, WorkbenchLayoutSpec};
#[cfg(test)]
use viewer_layout::PanelKind;
use viewer_layout::{PanelId, ViewerLayout, ViewerLayoutState};
#[cfg(test)]
use viewport::camera_render_quality;
use viewport::{
    ViewportOrbitDragState, apply_camera_orbit, apply_camera_pan, apply_camera_zoom,
    default_camera_for_shape, fit_camera_to_shape_preserving_view, fit_size,
    presentation_viewport_for_display_size, render_viewport_for_display_size,
    resident_brick_render_supported, viewport_hover_from_response, viewport_interaction_commands,
};
#[cfg(test)]
use viewport::{viewport_hover_from_image_point, viewport_hover_from_normalized_point};
use workbench_controls::{
    dataset_path_status_label, projection_selector, render_mode_label, render_mode_selector,
    request_background_work_repaint, request_background_work_repaint_after, show_playback_controls,
};

const SMALL_DENSE_VIEWER_VOXEL_LIMIT: u64 = 16 * 1024 * 1024;
const GPU_RESIDENT_BRICKS_PER_BATCH: usize = 64;
pub(crate) const SOURCE_ANALYSIS_SCALE_LEVEL: u32 = 0;

const BACKGROUND_WORK_REPAINT_INTERVAL: Duration = Duration::from_millis(50);
pub(crate) const CROSS_SECTION_INTERACTION_SETTLE_DURATION: Duration = Duration::from_millis(120);
const DVR_DENSITY_SCALE_MIN: f64 = 0.1;
const DVR_DENSITY_SCALE_MAX: f64 = 64.0;
const DEFAULT_DVR_OPACITY_GAMMA: f32 = 0.25;
const CROSS_SECTION_FAST_SLICE_MULTIPLIER: f64 = 10.0;
const CROSS_SECTION_ROTATE_RADIANS_PER_POINT: f64 = 0.005;

#[derive(Debug, Clone)]
pub struct AppState {
    pub startup_diagnostics: StartupDiagnostics,
    pub dataset_name: String,
    pub dataset_path: PathBuf,
    pub layer_count: usize,
    pub layers: Vec<AppLayerSummary>,
    pub active_layer_index: usize,
    pub active_layer_name: String,
    pub active_layer_id: String,
    pub active_layer_shape: Shape4D,
    pub active_layer_dtype: IntensityDType,
    pub active_layer_display: LayerDisplay,
    pub active_layer_color: ChannelColor,
    pub active_layer_transfer: ChannelTransferFunction,
    pub active_dvr_opacity_transfer: DvrOpacityTransfer,
    pub active_source_shape: Shape3D,
    pub active_source_grid_to_world: GridToWorld,
    pub active_timepoint: TimeIndex,
    pub timepoint_count: u64,
    pub active_projection: Projection,
    pub active_render_mode: RenderMode,
    pub render_sampling_policy: RenderSamplingPolicy,
    pub render_iso_shading_policy: RenderIsoShadingPolicy,
    pub iso_display_level: f32,
    pub iso_light_state: IsoLightState,
    pub dvr_density_scale: f64,
    pub presentation_viewport: PresentationViewport,
    pub render_viewport: RenderViewport,
    pub(crate) viewer_layout: ViewerLayoutState,
    pub render_backend: RenderBackend,
    pub frame_fidelity: FrameFidelityStatus,
    pub channel_fidelity: Vec<ChannelFidelityStatus>,
    pub lod_schedule: LodScheduleState,
    pub lod_replan_pending: bool,
    pub(crate) playback_lod_downshift_active: bool,
    pub renderer_gpu_brick_budget_bytes: u64,
    pub visible_brick_count: usize,
    pub visible_brick_plan_stride: u64,
    pub visible_brick_plan_error: Option<String>,
    pub visible_bricks: Vec<SpatialBrickIndex>,
    pub brick_stream_scale_level: u32,
    pub brick_stream_scale_shape: Shape3D,
    pub brick_stream_generation: u64,
    pub brick_stream_requested: usize,
    pub brick_stream_completed: usize,
    pub brick_stream_cancelled: usize,
    pub brick_stream_stale: usize,
    pub brick_stream_failed: usize,
    pub brick_stream_last_error: Option<String>,
    pub brick_stream_complete: bool,
    pub brick_result_drain_limit: usize,
    pub brick_result_drain_time_budget_ms: f64,
    pub brick_result_drain_last_count: usize,
    pub brick_result_drain_last_budget_limited: bool,
    pub brick_result_drain_last_repaint_reason: Option<String>,
    pub brick_result_drain_budget_hit_count: u64,
    pub brick_result_drain_total_drained: u64,
    pub brick_prefetch_timepoints: Vec<TimeIndex>,
    pub brick_prefetch_requested: usize,
    pub brick_prefetch_completed: usize,
    pub brick_prefetch_cancelled: usize,
    pub brick_prefetch_stale: usize,
    pub brick_prefetch_failed: usize,
    pub brick_prefetch_skipped: usize,
    pub brick_prefetch_last_error: Option<String>,
    pub brick_warm_brick_count: usize,
    pub brick_warm_requested: usize,
    pub brick_warm_completed: usize,
    pub brick_warm_cancelled: usize,
    pub brick_warm_stale: usize,
    pub brick_warm_failed: usize,
    pub brick_warm_skipped: usize,
    pub brick_warm_last_error: Option<String>,
    pub resident_bricks_u8: Vec<VolumeBrickU8>,
    pub resident_bricks_u8_by_layer: BTreeMap<String, Vec<VolumeBrickU8>>,
    pub resident_bricks: Vec<VolumeBrickU16>,
    pub resident_bricks_by_layer: BTreeMap<String, Vec<VolumeBrickU16>>,
    pub resident_bricks_f32: Vec<VolumeBrickF32>,
    pub resident_bricks_f32_by_layer: BTreeMap<String, Vec<VolumeBrickF32>>,
    pub(crate) cross_section_runtime: CrossSectionRuntime,
    pub(crate) cross_section_last_interaction_at: Option<Instant>,
    pub(crate) resident_histogram_generation: u64,
    pub(crate) resident_histogram_samples:
        HashMap<ResidentHistogramSampleKey, BrickHistogramSample>,
    prefetched_brick_payloads: Vec<PrefetchedBrickPayload>,
    pub adapter_summary: Option<String>,
    pub camera: CameraView,
    pub(crate) viewport_orbit_drag: Option<ViewportOrbitDragState>,
    pub frame: MipImageU16,
    pub frame_f32: Option<MipImageF32>,
    pub diagnostics: FrameDiagnostics,
    pub diagnostics_f32: Option<FrameDiagnosticsF32>,
    pub active_intensity_summary: IntensitySummary,
    pub channel_presets: Vec<ChannelDisplayPreset>,
    pub selected_channel_preset_index: Option<usize>,
    pub channel_preset_warnings: Vec<String>,
    pub analysis_tables: Vec<AnalysisTable>,
    pub analysis_plots: Vec<AnalysisPlot>,
    pub analysis_operations: Vec<AnalysisOperationRecord>,
    pub last_analysis_export_csv: Option<String>,
    selected_analysis_table_index: Option<usize>,
    selected_analysis_plot_index: Option<usize>,
    selected_analysis_plot_point: Option<AnalysisPlotPointSelection>,
    analysis_plot_view: Option<AnalysisPlotViewRange>,
    analysis_filter: String,
    analysis_sort: Option<AnalysisTableSort>,
    pub rendered_channels: Vec<RenderedIntensityChannel>,
    pub scene_artifacts: SceneArtifactStore,
    pub viewer_tools: ViewerToolState,
    pub hovered_pixel: Option<ViewportHover>,
    pub hovered_source_readout: Option<String>,
    pub last_render_error: Option<String>,
    pub last_workflow_message: Option<String>,
    dataset: DatasetHandle,
    active_volume_u8: Option<DenseVolumeU8>,
    active_volume: Option<DenseVolumeU16>,
    active_volume_f32: Option<DenseVolumeF32>,
    active_histogram_cache: Option<LayerHistogramCache>,
    brick_stream_request_key: Option<BrickStreamRequestKey>,
    brick_prefetch_request_key: Option<BrickPrefetchRequestKey>,
    brick_warm_request_key: Option<BrickWarmRequestKey>,
    #[cfg(test)]
    fixed_frame_time_ms_for_snapshots: Option<f64>,
}

pub struct MiranteWorkbenchApp {
    state: AppState,
    current_project_path: Option<PathBuf>,
    clean_project_snapshot: ProjectDirtySnapshot,
    close_prompt_open: bool,
    allow_close_without_prompt: bool,
    preferences: AppPreferences,
    preferences_path: Option<PathBuf>,
    settings_runtime_draft: AppRuntimePreferences,
    settings_message: Option<String>,
    texture: Option<egui::TextureHandle>,
    gpu_display_frame: Option<GpuDisplayFrame>,
    gpu_display_frame_identity: Option<GpuDisplayedFrameIdentity>,
    gpu_display_texture_id: Option<egui::TextureId>,
    cross_section_gpu_display_frames: BTreeMap<PanelId, CrossSectionPanelGpuDisplayFrame>,
    retired_gpu_display_texture_ids: Vec<egui::TextureId>,
    wgpu_texture_renderer: Option<Arc<egui::mutex::RwLock<eframe::egui_wgpu::Renderer>>>,
    last_display_refresh_timing: Option<DisplayRefreshTiming>,
    gpu_renderer: Option<Arc<GpuRenderer>>,
    brick_read_pool: Option<BrickReadPool>,
    cross_section_read_pool: Option<CrossSectionChunkReadPool>,
    current_brick_tickets: Vec<BrickReadTicket>,
    prefetch_brick_tickets: Vec<BrickReadTicket>,
    warm_brick_tickets: Vec<BrickReadTicket>,
    tiff_import_setup_task: Option<TiffImportSetupTask>,
    tiff_import_setup_error: Option<String>,
    pending_tiff_import: Option<PendingTiffImport>,
    import_task: Option<ImportTask>,
    analysis_task: Option<AnalysisTask>,
    analysis_workspace_open: bool,
    product_automation: Option<ProductAutomationController>,
    playback: PlaybackState,
    #[cfg(test)]
    test_render_viewport_max_side: Option<usize>,
}

struct CrossSectionPanelGpuDisplayFrame {
    generation: u64,
    frame: GpuDisplayFrame,
    texture_id: egui::TextureId,
}

impl MiranteWorkbenchApp {
    pub fn new(cc: &eframe::CreationContext<'_>, state: AppState) -> Self {
        Self::new_with_preferences(cc, state, AppPreferences::default(), None)
    }

    pub fn new_with_preferences(
        cc: &eframe::CreationContext<'_>,
        mut state: AppState,
        preferences: AppPreferences,
        preferences_path: Option<PathBuf>,
    ) -> Self {
        ui_kit::configure_visuals(&cc.egui_ctx);
        let wgpu_texture_renderer = cc
            .wgpu_render_state
            .as_ref()
            .map(|render_state| render_state.renderer.clone());
        let gpu_renderer_result = if let Some(render_state) = cc.wgpu_render_state.as_ref() {
            GpuRenderer::from_existing_device_with_cache_budgets(
                &render_state.adapter,
                render_state.device.clone(),
                render_state.queue.clone(),
                preferences.runtime.gpu_volume_cache_budget_bytes,
                preferences.runtime.gpu_brick_cache_budget_bytes,
            )
        } else {
            GpuRenderer::new_with_cache_budgets_blocking(
                preferences.runtime.gpu_volume_cache_budget_bytes,
                preferences.runtime.gpu_brick_cache_budget_bytes,
            )
        };
        let gpu_renderer = match gpu_renderer_result {
            Ok(renderer) => {
                let adapter_summary = format_adapter_summary(&renderer);
                state.startup_diagnostics.gpu_adapter = Some(adapter_summary.clone());
                state.adapter_summary = Some(adapter_summary);
                Some(Arc::new(renderer))
            }
            Err(err) => {
                let unavailable = format!("GPU unavailable: {err}");
                state.startup_diagnostics.gpu_adapter = Some(unavailable.clone());
                state.adapter_summary = Some(unavailable);
                tracing::warn!(error = %err, "GPU renderer unavailable; using CPU reference path");
                None
            }
        };
        if let Err(err) = rerender_state_with_backend(&mut state, gpu_renderer.as_deref()) {
            state.last_render_error = Some(err.to_string());
            tracing::error!(error = %err, "initial backend render failed");
        }
        let brick_read_pool = create_brick_read_pool(&state);
        let cross_section_read_pool = create_cross_section_read_pool(&state);
        let settings_runtime_draft = preferences.runtime;
        let clean_project_snapshot = ProjectDirtySnapshot::from_state(&state);
        let mut app = Self {
            state,
            current_project_path: None,
            clean_project_snapshot,
            close_prompt_open: false,
            allow_close_without_prompt: false,
            preferences,
            preferences_path,
            settings_runtime_draft,
            settings_message: None,
            texture: None,
            gpu_display_frame: None,
            gpu_display_frame_identity: None,
            gpu_display_texture_id: None,
            cross_section_gpu_display_frames: BTreeMap::new(),
            retired_gpu_display_texture_ids: Vec::new(),
            wgpu_texture_renderer,
            last_display_refresh_timing: None,
            gpu_renderer,
            brick_read_pool,
            cross_section_read_pool,
            current_brick_tickets: Vec::new(),
            prefetch_brick_tickets: Vec::new(),
            warm_brick_tickets: Vec::new(),
            tiff_import_setup_task: None,
            tiff_import_setup_error: None,
            pending_tiff_import: None,
            import_task: None,
            analysis_task: None,
            analysis_workspace_open: false,
            product_automation: ProductAutomationController::from_env(),
            playback: PlaybackState::default(),
            #[cfg(test)]
            test_render_viewport_max_side: None,
        };
        app.request_opened_state_visible_work(Some(&cc.egui_ctx));
        app
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    fn show_runtime_diagnostics_body(&self, ui: &mut egui::Ui) {
        runtime_diagnostics_panel::show_runtime_diagnostics_body(self, ui);
    }

    fn diagnostics_summary_text(&self) -> String {
        runtime_diagnostics_panel::diagnostics_summary_text(self)
    }

    fn show_settings_body(&mut self, ui: &mut egui::Ui) {
        let mut volume_cache_mib =
            bytes_to_mib_rounded(self.settings_runtime_draft.volume_cache_budget_bytes);
        if ui
            .add(
                egui::DragValue::new(&mut volume_cache_mib)
                    .range(1..=1_048_576)
                    .speed(64)
                    .suffix(" MiB"),
            )
            .on_hover_text("volume cache MiB")
            .changed()
        {
            self.settings_runtime_draft.volume_cache_budget_bytes = mib_to_bytes(volume_cache_mib);
        }
        ui_kit::property_row(ui, "volume cache MiB", volume_cache_mib.to_string());

        let mut brick_cache_mib =
            bytes_to_mib_rounded(self.settings_runtime_draft.brick_cache_budget_bytes);
        if ui
            .add(
                egui::DragValue::new(&mut brick_cache_mib)
                    .range(1..=1_048_576)
                    .speed(256)
                    .suffix(" MiB"),
            )
            .on_hover_text("brick cache MiB")
            .changed()
        {
            self.settings_runtime_draft.brick_cache_budget_bytes = mib_to_bytes(brick_cache_mib);
        }
        ui_kit::property_row(ui, "brick cache MiB", brick_cache_mib.to_string());

        let mut gpu_volume_cache_mib =
            bytes_to_mib_rounded(self.settings_runtime_draft.gpu_volume_cache_budget_bytes);
        if ui
            .add(
                egui::DragValue::new(&mut gpu_volume_cache_mib)
                    .range(1..=1_048_576)
                    .speed(256)
                    .suffix(" MiB"),
            )
            .on_hover_text("GPU dense diagnostic cache MiB")
            .changed()
        {
            self.settings_runtime_draft.gpu_volume_cache_budget_bytes =
                mib_to_bytes(gpu_volume_cache_mib);
        }
        ui_kit::property_row(ui, "GPU dense cache MiB", gpu_volume_cache_mib.to_string());

        let mut gpu_brick_cache_mib =
            bytes_to_mib_rounded(self.settings_runtime_draft.gpu_brick_cache_budget_bytes);
        if ui
            .add(
                egui::DragValue::new(&mut gpu_brick_cache_mib)
                    .range(1..=1_048_576)
                    .speed(256)
                    .suffix(" MiB"),
            )
            .on_hover_text("GPU resident brick cache MiB")
            .changed()
        {
            self.settings_runtime_draft.gpu_brick_cache_budget_bytes =
                mib_to_bytes(gpu_brick_cache_mib);
        }
        ui_kit::property_row(ui, "GPU brick cache MiB", gpu_brick_cache_mib.to_string());

        ui.horizontal(|ui| {
            if ui_kit::toolbar_button(ui, "Save Settings", true).clicked() {
                self.save_preferences_from_settings();
            }
            if ui_kit::toolbar_button(ui, "Reset Settings", true).clicked() {
                self.settings_runtime_draft = AppRuntimePreferences::default();
                self.settings_message = None;
            }
        });
        if let Some(path) = &self.preferences_path {
            ui_kit::property_row(ui, "settings file", path.display());
        }
        if let Some(message) = &self.settings_message {
            ui_kit::property_row(ui, "settings status", message);
        }
    }

    fn save_preferences_from_settings(&mut self) {
        let next_preferences = AppPreferences {
            format: PREFERENCES_FORMAT.to_owned(),
            runtime: self.settings_runtime_draft,
        };
        if let Err(err) = next_preferences.validate() {
            self.settings_message = Some(err.to_string());
            self.state.last_render_error = Some(err.to_string());
            return;
        }
        let Some(path) = self.preferences_path.clone() else {
            let message = "preferences path is unavailable".to_owned();
            self.settings_message = Some(message.clone());
            self.state.last_render_error = Some(message);
            return;
        };
        match write_app_preferences(&path, &next_preferences) {
            Ok(()) => {
                self.preferences = next_preferences;
                self.settings_message = Some("saved".to_owned());
                self.state.last_render_error = None;
                self.state.last_workflow_message = Some("Saved settings".to_owned());
            }
            Err(err) => {
                self.settings_message = Some(err.to_string());
                self.state.last_render_error = Some(err.to_string());
            }
        }
    }

    fn start_analysis_task(&mut self, kind: AnalysisTaskKind) {
        if self.analysis_task.is_some() {
            self.state.last_workflow_message = Some("Analysis already running".to_owned());
            return;
        }
        self.analysis_task = Some(spawn_analysis_task(
            &self.state,
            self.gpu_renderer.clone(),
            kind,
        ));
        self.state.last_render_error = None;
        self.state.last_workflow_message = Some(format!("Running {}", analysis_task_label(kind)));
    }

    fn cancel_analysis_task(&mut self) {
        if let Some(task) = &self.analysis_task {
            task.cancellation.cancel();
            self.state.last_workflow_message = Some("Cancelling analysis".to_owned());
        }
    }

    fn drain_analysis_results(&mut self, ctx: &egui::Context) {
        let mut completion = None;
        let mut saw_progress = false;
        if let Some(task) = self.analysis_task.as_mut() {
            while let Ok(message) = task.receiver.try_recv() {
                match message {
                    AnalysisTaskMessage::Progress(progress) => {
                        self.state.last_workflow_message = Some(progress.label.clone());
                        task.latest_progress = Some(progress);
                        saw_progress = true;
                    }
                    AnalysisTaskMessage::Finished(result) => {
                        completion = Some(*result);
                    }
                }
            }
        }

        if let Some(result) = completion {
            self.analysis_task = None;
            match result {
                Ok(output) => {
                    let message = store_analysis_task_output(&mut self.state, output);
                    self.state.last_render_error = None;
                    self.state.last_workflow_message = Some(message);
                }
                Err(error) if error == "analysis was cancelled" => {
                    self.state.last_render_error = None;
                    self.state.last_workflow_message = Some("Analysis cancelled".to_owned());
                }
                Err(error) => {
                    self.state.last_render_error = Some(error);
                }
            }
            saw_progress = true;
        }

        if saw_progress {
            ctx.request_repaint();
        }
    }

    fn activate_layer_timepoint(
        &mut self,
        layer_index: usize,
        timepoint: TimeIndex,
        ctx: &egui::Context,
    ) {
        if layer_index == self.state.active_layer_index
            && !dense_startup_allowed(self.state.active_source_shape)
        {
            match activate_streaming_timepoint_preserving_frame(&mut self.state, timepoint) {
                Ok(()) => {
                    self.state.last_render_error = None;
                    self.invalidate_cross_section_panel_display_frames();
                    let total_start = Instant::now();
                    let brick_request_start = Instant::now();
                    self.request_visible_bricks();
                    let visible_brick_request_ms = duration_ms(brick_request_start.elapsed());
                    let cpu_texture_update_ms = self.update_cpu_texture_if_needed();
                    self.record_preserved_display_refresh_timing(
                        visible_brick_request_ms,
                        cpu_texture_update_ms,
                        duration_ms(total_start.elapsed()),
                    );
                    ctx.request_repaint();
                }
                Err(err) => {
                    self.state.last_render_error = Some(err.to_string());
                    tracing::error!(error = %err, "failed to activate streaming timepoint");
                }
            }
            return;
        }
        match activate_layer_timepoint_state_only(&mut self.state, layer_index, timepoint) {
            Ok(()) => {
                self.state.last_render_error = None;
                self.invalidate_cross_section_panel_display_frames();
                match self.rerender_display_state() {
                    Ok(render_timing) => {
                        let total_start = Instant::now();
                        let brick_request_start = Instant::now();
                        self.request_visible_bricks();
                        let visible_brick_request_ms = duration_ms(brick_request_start.elapsed());
                        let cpu_texture_update_ms = self.update_cpu_texture_if_needed();
                        self.record_display_refresh_timing(
                            render_timing,
                            visible_brick_request_ms,
                            cpu_texture_update_ms,
                            duration_ms(total_start.elapsed()),
                        );
                        ctx.request_repaint();
                    }
                    Err(err) => {
                        self.state.last_render_error = Some(err.to_string());
                        tracing::error!(error = %err, "failed to activate layer/timepoint");
                    }
                }
            }
            Err(err) => {
                self.state.last_render_error = Some(err.to_string());
                tracing::error!(error = %err, "failed to activate layer/timepoint");
            }
        }
    }

    fn apply_cross_section_state_change(
        &mut self,
        ctx: &egui::Context,
        panel_id: PanelId,
        change: impl FnOnce(&mut mirante4d_renderer::CrossSectionViewState, CrossSectionPanel),
    ) -> WorkbenchCommandOutcome {
        if self.state.viewer_layout.layout() != ViewerLayout::FourPanel {
            return WorkbenchCommandOutcome::default();
        }
        let Some(panel) = panel_id.cross_section_panel() else {
            self.state.last_render_error = Some("3D panel is not a cross-section panel".to_owned());
            return WorkbenchCommandOutcome::default();
        };
        let active_panel_changed = self
            .state
            .viewer_layout
            .mark_active_cross_section_panel(panel_id);
        self.state.cross_section_last_interaction_at = Some(Instant::now());
        let before = self.state.viewer_layout.cross_section;
        change(&mut self.state.viewer_layout.cross_section, panel);
        if self.state.viewer_layout.cross_section != before {
            self.invalidate_cross_section_panel_display_frames();
            self.state.last_render_error = None;
            ctx.request_repaint();
            ctx.request_repaint_after(CROSS_SECTION_INTERACTION_SETTLE_DURATION);
        } else if active_panel_changed {
            ctx.request_repaint();
            ctx.request_repaint_after(CROSS_SECTION_INTERACTION_SETTLE_DURATION);
        }
        WorkbenchCommandOutcome::default()
    }

    fn apply_workbench_command(
        &mut self,
        command: WorkbenchCommand,
        ctx: &egui::Context,
    ) -> WorkbenchCommandOutcome {
        match command {
            WorkbenchCommand::SetRenderMode(mode) => {
                if mode == self.state.active_render_mode {
                    WorkbenchCommandOutcome::default()
                } else {
                    self.state.active_render_mode = mode;
                    sync_active_layer_render_state_from_runtime(&mut self.state);
                    self.clear_gpu_display_frame();
                    self.retire_gpu_display_texture_id();
                    self.state.brick_stream_request_key = None;
                    self.state.frame_fidelity.display_freshness = DisplayedFrameFreshness::Unknown;
                    WorkbenchCommandOutcome {
                        rerender_requested: true,
                        ..WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::SetLayerRenderMode { layer_index, mode } => {
                let Ok(render_state) = layer_render_state_for_mode(&self.state, layer_index, mode)
                else {
                    self.state.last_render_error =
                        Some(format!("layer index {layer_index} is out of range"));
                    return WorkbenchCommandOutcome::default();
                };
                if layer_index == self.state.active_layer_index {
                    if mode == self.state.active_render_mode {
                        WorkbenchCommandOutcome::default()
                    } else {
                        self.state.active_render_mode = mode;
                        sync_active_layer_render_state_from_runtime(&mut self.state);
                        self.clear_gpu_display_frame();
                        self.retire_gpu_display_texture_id();
                        self.state.brick_stream_request_key = None;
                        self.state.frame_fidelity.display_freshness =
                            DisplayedFrameFreshness::Unknown;
                        WorkbenchCommandOutcome {
                            rerender_requested: true,
                            ..WorkbenchCommandOutcome::default()
                        }
                    }
                } else {
                    match set_layer_render_state(&mut self.state, layer_index, render_state) {
                        Ok(false) => WorkbenchCommandOutcome::default(),
                        Ok(true) => {
                            self.clear_gpu_display_frame();
                            self.retire_gpu_display_texture_id();
                            self.state.brick_stream_request_key = None;
                            WorkbenchCommandOutcome {
                                rerender_requested: true,
                                ..WorkbenchCommandOutcome::default()
                            }
                        }
                        Err(err) => {
                            self.state.last_render_error = Some(err.to_string());
                            WorkbenchCommandOutcome::default()
                        }
                    }
                }
            }
            WorkbenchCommand::SetIsoDisplayLevel { display_level } => {
                if !display_level.is_finite() || !(0.0..=1.0).contains(&display_level) {
                    self.state.last_render_error =
                        Some("ISO display level must be finite and between 0.0 and 1.0".to_owned());
                    return WorkbenchCommandOutcome::default();
                }
                if (display_level - self.state.iso_display_level).abs() <= f32::EPSILON {
                    WorkbenchCommandOutcome::default()
                } else {
                    self.state.iso_display_level = display_level;
                    sync_active_layer_render_state_from_runtime(&mut self.state);
                    WorkbenchCommandOutcome {
                        rerender_requested: true,
                        ..WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::SetIsoLightAttached { attached } => {
                let next = if attached {
                    self.state.iso_light_state.reset_attached()
                } else {
                    let position = self.state.iso_light_state.detached_screen_position;
                    match self
                        .state
                        .iso_light_state
                        .with_detached_screen_position(position.x, position.y)
                    {
                        Ok(state) => state,
                        Err(err) => {
                            self.state.last_render_error = Some(err.to_string());
                            return WorkbenchCommandOutcome::default();
                        }
                    }
                };
                if next == self.state.iso_light_state {
                    WorkbenchCommandOutcome::default()
                } else {
                    self.state.iso_light_state = next;
                    if self.state.active_render_mode == RenderMode::Isosurface {
                        self.refresh_texture_only(ctx);
                    }
                    WorkbenchCommandOutcome::default()
                }
            }
            WorkbenchCommand::SetIsoLightDetachedPosition { x, y } => {
                match self
                    .state
                    .iso_light_state
                    .with_detached_screen_position(x, y)
                {
                    Ok(next) if next != self.state.iso_light_state => {
                        self.state.iso_light_state = next;
                        if self.state.active_render_mode == RenderMode::Isosurface {
                            self.refresh_texture_only(ctx);
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        self.state.last_render_error = Some(err.to_string());
                    }
                }
                WorkbenchCommandOutcome::default()
            }
            WorkbenchCommand::ResetIsoLight => {
                let next = IsoLightState::attached_camera();
                if next != self.state.iso_light_state {
                    self.state.iso_light_state = next;
                    if self.state.active_render_mode == RenderMode::Isosurface {
                        self.refresh_texture_only(ctx);
                    }
                }
                WorkbenchCommandOutcome::default()
            }
            WorkbenchCommand::SetDvrDensityScale { density_scale } => {
                if !density_scale.is_finite()
                    || !(DVR_DENSITY_SCALE_MIN..=DVR_DENSITY_SCALE_MAX).contains(&density_scale)
                {
                    self.state.last_render_error = Some(format!(
                        "DVR density scale must be finite and between {DVR_DENSITY_SCALE_MIN:.1} and {DVR_DENSITY_SCALE_MAX:.1}"
                    ));
                    return WorkbenchCommandOutcome::default();
                }
                if (density_scale - self.state.dvr_density_scale).abs() <= f64::EPSILON {
                    WorkbenchCommandOutcome::default()
                } else {
                    self.state.dvr_density_scale = density_scale;
                    sync_active_layer_render_state_from_runtime(&mut self.state);
                    WorkbenchCommandOutcome {
                        rerender_requested: true,
                        ..WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::SetRenderSamplingPolicy(policy) => {
                if policy == self.state.render_sampling_policy {
                    WorkbenchCommandOutcome::default()
                } else {
                    self.state.render_sampling_policy = policy;
                    sync_active_layer_render_state_from_runtime(&mut self.state);
                    self.invalidate_cross_section_panel_display_frames();
                    WorkbenchCommandOutcome {
                        rerender_requested: true,
                        ..WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::SetRenderIsoShadingPolicy(policy) => {
                if policy == self.state.render_iso_shading_policy {
                    WorkbenchCommandOutcome::default()
                } else {
                    self.state.render_iso_shading_policy = policy;
                    sync_active_layer_render_state_from_runtime(&mut self.state);
                    WorkbenchCommandOutcome {
                        rerender_requested: true,
                        ..WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::SetViewerLayout(layout) => {
                if layout == self.state.viewer_layout.layout() {
                    WorkbenchCommandOutcome::default()
                } else {
                    match layout {
                        ViewerLayout::Single3d => {
                            self.state.viewer_layout.switch_to_single_3d();
                            self.state.cross_section_last_interaction_at = None;
                            self.retire_cross_section_gpu_display_texture_ids();
                            retire_cross_section_streaming_state(&mut self.state);
                            if let Some(pool) = &self.cross_section_read_pool {
                                pool.advance_generation();
                            }
                        }
                        ViewerLayout::FourPanel => self.state.viewer_layout.switch_to_four_panel(),
                    }
                    self.state.hovered_pixel = None;
                    self.state.hovered_source_readout = None;
                    self.state.viewport_orbit_drag = None;
                    WorkbenchCommandOutcome {
                        rerender_requested: true,
                        ..WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::SetProjection(projection) => {
                self.state.viewport_orbit_drag = None;
                if projection == self.state.camera.projection {
                    WorkbenchCommandOutcome::default()
                } else {
                    self.state.camera.set_projection(projection);
                    self.state.active_projection = projection;
                    WorkbenchCommandOutcome {
                        rerender_requested: true,
                        ..WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::ResetView => {
                let projection = self.state.camera.projection;
                let mut reset_camera = default_camera_for_shape(
                    self.state.active_source_shape,
                    self.state.active_source_grid_to_world,
                );
                reset_camera.set_projection(projection);
                let camera = fit_camera_to_shape_preserving_view(
                    reset_camera,
                    self.state.active_source_shape,
                    self.state.active_source_grid_to_world,
                    self.state.presentation_viewport,
                );
                let changed = self.state.camera != camera;
                self.state.camera = camera;
                self.state.active_projection = self.state.camera.projection;
                self.state.viewport_orbit_drag = None;
                let cross_section_changed =
                    self.state.viewer_layout.reset_cross_section_for_dataset(
                        self.state.active_source_shape,
                        self.state.active_source_grid_to_world,
                        self.state.presentation_viewport,
                    );
                if cross_section_changed {
                    self.state.cross_section_last_interaction_at = None;
                    ctx.request_repaint();
                }
                WorkbenchCommandOutcome {
                    rerender_requested: changed,
                    ..WorkbenchCommandOutcome::default()
                }
            }
            WorkbenchCommand::FitData => {
                let camera = fit_camera_to_shape_preserving_view(
                    self.state.camera,
                    self.state.active_source_shape,
                    self.state.active_source_grid_to_world,
                    self.state.presentation_viewport,
                );
                let changed = self.state.camera != camera;
                self.state.camera = camera;
                self.state.active_projection = self.state.camera.projection;
                self.state.viewport_orbit_drag = None;
                WorkbenchCommandOutcome {
                    rerender_requested: changed,
                    ..WorkbenchCommandOutcome::default()
                }
            }
            WorkbenchCommand::SelectLayer(layer_index) => {
                self.activate_layer_timepoint(layer_index, TimeIndex(0), ctx);
                WorkbenchCommandOutcome::default()
            }
            WorkbenchCommand::SetTimepoint(timepoint) => {
                self.playback.last_step_at = Some(Instant::now());
                self.activate_layer_timepoint(self.state.active_layer_index, timepoint, ctx);
                self.playback.waiting_for_timepoint = self.playback.playing.then_some(timepoint);
                WorkbenchCommandOutcome::default()
            }
            WorkbenchCommand::StepTimepoint { delta } => {
                let timepoint = stepped_timepoint(
                    self.state.active_timepoint,
                    self.state.timepoint_count,
                    delta,
                );
                self.playback.last_step_at = Some(Instant::now());
                self.activate_layer_timepoint(self.state.active_layer_index, timepoint, ctx);
                self.playback.waiting_for_timepoint = self.playback.playing.then_some(timepoint);
                WorkbenchCommandOutcome::default()
            }
            WorkbenchCommand::SetPlayback { playing } => {
                let was_playing = self.playback.playing;
                let had_playback_lod_downshift = self.state.playback_lod_downshift_active;
                self.playback.playing = playing && self.state.timepoint_count > 1;
                self.playback.waiting_for_timepoint = None;
                self.state.playback_lod_downshift_active = self.playback.playing;
                if self.playback.playing {
                    let now = Instant::now();
                    self.playback.last_step_at =
                        Some(now.checked_sub(self.playback.frame_interval).unwrap_or(now));
                    self.enter_playback_streaming_mode(ctx);
                    ctx.request_repaint();
                } else {
                    self.playback.last_step_at = None;
                    if was_playing || had_playback_lod_downshift {
                        self.exit_playback_streaming_mode(ctx);
                    }
                }
                if self.playback.playing {
                    ctx.request_repaint_after(self.playback.frame_interval);
                }
                WorkbenchCommandOutcome::default()
            }
            WorkbenchCommand::SetLayerVisibility {
                layer_index,
                visible,
            } => {
                let Some(layer) = self.state.layers.get(layer_index) else {
                    self.state.last_render_error =
                        Some(format!("layer index {layer_index} is out of range"));
                    return WorkbenchCommandOutcome::default();
                };
                let layer_display = layer.display;
                let layer_color = layer.color;
                let display = LayerDisplay {
                    visible,
                    ..layer_display
                };
                match set_layer_display_state(&mut self.state, layer_index, display, layer_color) {
                    Ok(changed) => {
                        if changed {
                            self.invalidate_cross_section_panel_display_frames();
                        }
                        WorkbenchCommandOutcome {
                            rerender_requested: changed,
                            ..WorkbenchCommandOutcome::default()
                        }
                    }
                    Err(err) => {
                        self.state.last_render_error = Some(err.to_string());
                        WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::SetLayerOpacity {
                layer_index,
                opacity,
            } => {
                let Some(layer) = self.state.layers.get(layer_index) else {
                    self.state.last_render_error =
                        Some(format!("layer index {layer_index} is out of range"));
                    return WorkbenchCommandOutcome::default();
                };
                let layer_display = layer.display;
                let layer_color = layer.color;
                let result = (|| -> anyhow::Result<bool> {
                    let display =
                        LayerDisplay::new(layer_display.visible, layer_display.window, opacity)?;
                    set_layer_display_state(&mut self.state, layer_index, display, layer_color)
                })();
                match result {
                    Ok(changed) => {
                        if changed {
                            self.invalidate_cross_section_panel_display_frames();
                        }
                        WorkbenchCommandOutcome {
                            rerender_requested: changed,
                            ..WorkbenchCommandOutcome::default()
                        }
                    }
                    Err(err) => {
                        self.state.last_render_error = Some(err.to_string());
                        WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::SetLayerWindow {
                layer_index,
                low,
                high,
            } => {
                let Some(layer) = self.state.layers.get(layer_index) else {
                    self.state.last_render_error =
                        Some(format!("layer index {layer_index} is out of range"));
                    return WorkbenchCommandOutcome::default();
                };
                let layer_display = layer.display;
                let layer_color = layer.color;
                let result = (|| -> anyhow::Result<bool> {
                    let window = DisplayWindow::new(low, high)?;
                    let display =
                        LayerDisplay::new(layer_display.visible, window, layer_display.opacity)?;
                    set_layer_display_state(&mut self.state, layer_index, display, layer_color)
                })();
                match result {
                    Ok(changed) => {
                        if changed {
                            self.invalidate_cross_section_panel_display_frames();
                        }
                        WorkbenchCommandOutcome {
                            rerender_requested: changed,
                            ..WorkbenchCommandOutcome::default()
                        }
                    }
                    Err(err) => {
                        self.state.last_render_error = Some(err.to_string());
                        WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::SetLayerColor {
                layer_index,
                color_rgba,
            } => {
                let Some(layer) = self.state.layers.get(layer_index) else {
                    self.state.last_render_error =
                        Some(format!("layer index {layer_index} is out of range"));
                    return WorkbenchCommandOutcome::default();
                };
                let layer_display = layer.display;
                let result = (|| -> anyhow::Result<bool> {
                    let color = ChannelColor::new(color_rgba)?;
                    set_layer_display_state(&mut self.state, layer_index, layer_display, color)
                })();
                match result {
                    Ok(changed) => {
                        if changed {
                            self.invalidate_cross_section_panel_display_frames();
                        }
                        WorkbenchCommandOutcome {
                            rerender_requested: changed,
                            ..WorkbenchCommandOutcome::default()
                        }
                    }
                    Err(err) => {
                        self.state.last_render_error = Some(err.to_string());
                        WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::SetLayerGamma { layer_index, gamma } => {
                let result = (|| -> anyhow::Result<bool> {
                    let preset = TransferPresetId::new("custom_gamma")?;
                    set_layer_transfer_curve(
                        &mut self.state,
                        layer_index,
                        TransferCurve::gamma(gamma)?,
                        preset,
                    )
                })();
                match result {
                    Ok(changed) => {
                        if changed {
                            self.invalidate_cross_section_panel_display_frames();
                        }
                        WorkbenchCommandOutcome {
                            rerender_requested: changed,
                            ..WorkbenchCommandOutcome::default()
                        }
                    }
                    Err(err) => {
                        self.state.last_render_error = Some(err.to_string());
                        WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::SetLayerInvert {
                layer_index,
                invert,
            } => {
                let result = set_layer_transfer_invert(&mut self.state, layer_index, invert);
                match result {
                    Ok(changed) => {
                        if changed {
                            self.invalidate_cross_section_panel_display_frames();
                        }
                        WorkbenchCommandOutcome {
                            rerender_requested: changed,
                            ..WorkbenchCommandOutcome::default()
                        }
                    }
                    Err(err) => {
                        self.state.last_render_error = Some(err.to_string());
                        WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::SetLayerDvrOpacityWindow {
                layer_index,
                low,
                high,
            } => {
                let result = (|| -> anyhow::Result<bool> {
                    let current = layer_dvr_opacity_transfer(&self.state, layer_index)?;
                    let window = DisplayWindow::new(low, high)?;
                    let transfer = DvrOpacityTransfer::new(window, current.curve)?;
                    set_layer_dvr_opacity_transfer_state(&mut self.state, layer_index, transfer)
                })();
                match result {
                    Ok(changed) => WorkbenchCommandOutcome {
                        rerender_requested: changed,
                        ..WorkbenchCommandOutcome::default()
                    },
                    Err(err) => {
                        self.state.last_render_error = Some(err.to_string());
                        WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::SetLayerDvrOpacityGamma { layer_index, gamma } => {
                let result = (|| -> anyhow::Result<bool> {
                    let current = layer_dvr_opacity_transfer(&self.state, layer_index)?;
                    let transfer =
                        DvrOpacityTransfer::new(current.window, TransferCurve::gamma(gamma)?)?;
                    set_layer_dvr_opacity_transfer_state(&mut self.state, layer_index, transfer)
                })();
                match result {
                    Ok(changed) => WorkbenchCommandOutcome {
                        rerender_requested: changed,
                        ..WorkbenchCommandOutcome::default()
                    },
                    Err(err) => {
                        self.state.last_render_error = Some(err.to_string());
                        WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::AutoLayerDvrOpacity { layer_index } => {
                let result = (|| -> anyhow::Result<bool> {
                    if layer_index != self.state.active_layer_index {
                        anyhow::bail!("DVR auto opacity can only use the active layer histogram");
                    }
                    let histogram = active_layer_histogram_summary(&mut self.state);
                    let transfer = auto_dvr_opacity_transfer_from_histogram(&histogram)?;
                    set_layer_dvr_opacity_transfer_state(&mut self.state, layer_index, transfer)
                })();
                match result {
                    Ok(changed) => WorkbenchCommandOutcome {
                        rerender_requested: changed,
                        ..WorkbenchCommandOutcome::default()
                    },
                    Err(err) => {
                        self.state.last_render_error = Some(err.to_string());
                        WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::ResetLayerDvrOpacity { layer_index } => {
                let result = (|| -> anyhow::Result<bool> {
                    let display = self
                        .state
                        .layers
                        .get(layer_index)
                        .ok_or_else(|| {
                            anyhow::anyhow!("layer index {layer_index} is out of range")
                        })?
                        .display;
                    set_layer_dvr_opacity_transfer_state(
                        &mut self.state,
                        layer_index,
                        default_dvr_opacity_transfer(display),
                    )
                })();
                match result {
                    Ok(changed) => WorkbenchCommandOutcome {
                        rerender_requested: changed,
                        ..WorkbenchCommandOutcome::default()
                    },
                    Err(err) => {
                        self.state.last_render_error = Some(err.to_string());
                        WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::SetLayerTransferPreset {
                layer_index,
                preset,
            } => {
                let result = set_layer_transfer_curve(
                    &mut self.state,
                    layer_index,
                    built_in_transfer_preset_curve(preset),
                    built_in_transfer_preset_id(preset),
                );
                match result {
                    Ok(changed) => {
                        if changed {
                            self.invalidate_cross_section_panel_display_frames();
                        }
                        WorkbenchCommandOutcome {
                            rerender_requested: changed,
                            ..WorkbenchCommandOutcome::default()
                        }
                    }
                    Err(err) => {
                        self.state.last_render_error = Some(err.to_string());
                        WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::ApplyChannelPreset { preset_index } => {
                match apply_channel_display_preset(&mut self.state, preset_index) {
                    Ok(changed) => {
                        if changed {
                            self.invalidate_cross_section_panel_display_frames();
                        }
                        WorkbenchCommandOutcome {
                            rerender_requested: changed,
                            ..WorkbenchCommandOutcome::default()
                        }
                    }
                    Err(err) => {
                        self.state.last_render_error = Some(err.to_string());
                        WorkbenchCommandOutcome::default()
                    }
                }
            }
            WorkbenchCommand::SaveCurrentChannelPreset => {
                let preset_id = next_user_channel_preset_id(&self.state);
                let name = format!("Display {}", self.state.channel_presets.len() + 1);
                let preset = channel_preset_from_current_state(&self.state, preset_id, name);
                self.state.channel_presets.push(preset);
                self.state.selected_channel_preset_index =
                    Some(self.state.channel_presets.len() - 1);
                self.state.last_workflow_message = Some("Saved channel display preset".to_owned());
                WorkbenchCommandOutcome::default()
            }
            WorkbenchCommand::UpdateChannelPreset { preset_index } => {
                if preset_index >= self.state.channel_presets.len() {
                    self.state.last_render_error = Some(format!(
                        "channel preset index {preset_index} is out of range"
                    ));
                    WorkbenchCommandOutcome::default()
                } else {
                    let preset_id = self.state.channel_presets[preset_index].preset_id.clone();
                    let name = self.state.channel_presets[preset_index].name.clone();
                    let preset = channel_preset_from_current_state(&self.state, preset_id, name);
                    self.state.channel_presets[preset_index] = preset;
                    self.state.selected_channel_preset_index = Some(preset_index);
                    self.state.last_workflow_message =
                        Some("Updated channel display preset".to_owned());
                    WorkbenchCommandOutcome::default()
                }
            }
            WorkbenchCommand::CameraPanDrag { motion_points } => {
                let before = self.state.camera;
                self.state.viewport_orbit_drag = None;
                apply_camera_pan(&mut self.state.camera, motion_points);
                self.state.active_projection = self.state.camera.projection;
                WorkbenchCommandOutcome {
                    rerender_requested: self.state.camera != before,
                    ..WorkbenchCommandOutcome::default()
                }
            }
            WorkbenchCommand::CameraOrbitDrag {
                start_camera,
                start_position_points,
                current_position_points,
                viewport_size_points,
            } => {
                let before = self.state.camera;
                apply_camera_orbit(
                    &mut self.state.camera,
                    start_camera,
                    start_position_points,
                    current_position_points,
                    viewport_size_points,
                );
                self.state.active_projection = self.state.camera.projection;
                WorkbenchCommandOutcome {
                    rerender_requested: self.state.camera != before,
                    ..WorkbenchCommandOutcome::default()
                }
            }
            WorkbenchCommand::CameraZoom { scroll_y_points } => {
                let before = self.state.camera;
                self.state.viewport_orbit_drag = None;
                apply_camera_zoom(&mut self.state.camera, scroll_y_points);
                self.state.active_projection = self.state.camera.projection;
                WorkbenchCommandOutcome {
                    rerender_requested: self.state.camera != before,
                    ..WorkbenchCommandOutcome::default()
                }
            }
            WorkbenchCommand::CrossSectionPanDrag {
                panel_id,
                motion_points,
            } => self.apply_cross_section_state_change(ctx, panel_id, |state, panel| {
                state.pan_by_panel_points(
                    panel,
                    f64::from(motion_points.x),
                    f64::from(motion_points.y),
                );
            }),
            WorkbenchCommand::CrossSectionSliceStep {
                panel_id,
                notches,
                fast,
            } => {
                let Ok(step_world) = cross_section_effective_voxel_world_step(&self.state) else {
                    self.state.last_render_error =
                        Some("failed to derive cross-section slice step from dataset".to_owned());
                    return WorkbenchCommandOutcome::default();
                };
                let multiplier = if fast {
                    CROSS_SECTION_FAST_SLICE_MULTIPLIER
                } else {
                    1.0
                };
                let distance_world = step_world * notches * multiplier;
                self.apply_cross_section_state_change(ctx, panel_id, |state, panel| {
                    state.slice_by_world_distance(panel, distance_world);
                })
            }
            WorkbenchCommand::CrossSectionZoom {
                panel_id,
                presentation_viewport,
                pointer_position_points,
                scroll_y_points,
            } => self.apply_cross_section_state_change(ctx, panel_id, |state, panel| {
                let factor = (-f64::from(scroll_y_points) * 0.001).exp();
                state.zoom_around_panel_point(
                    panel,
                    presentation_viewport,
                    f64::from(pointer_position_points.x),
                    f64::from(pointer_position_points.y),
                    factor,
                );
            }),
            WorkbenchCommand::CrossSectionRotateDrag {
                panel_id,
                motion_points,
            } => self.apply_cross_section_state_change(ctx, panel_id, |state, panel| {
                state.rotate_oblique_by_panel_drag(
                    panel,
                    f64::from(motion_points.x),
                    f64::from(motion_points.y),
                    CROSS_SECTION_ROTATE_RADIANS_PER_POINT,
                );
            }),
        }
    }
}

fn cross_section_effective_voxel_world_step(state: &AppState) -> anyhow::Result<f64> {
    let layer_id = LayerId::new(state.active_layer_id.clone())?;
    let grid_to_world = state
        .dataset
        .scale_grid_to_world(&layer_id, state.brick_stream_scale_level)?;
    Ok(effective_voxel_world_step(grid_to_world))
}

fn effective_voxel_world_step(grid_to_world: GridToWorld) -> f64 {
    let x = grid_to_world.transform_vector(DVec3::X).length();
    let y = grid_to_world.transform_vector(DVec3::Y).length();
    let z = grid_to_world.transform_vector(DVec3::Z).length();
    x.min(y).min(z).max(f64::EPSILON)
}

#[cfg(test)]
mod tests;

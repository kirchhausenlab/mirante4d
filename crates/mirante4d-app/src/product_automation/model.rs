use std::path::PathBuf;

use mirante4d_dataset::CpuLedgerCategory;
use mirante4d_dataset_runtime::DatasetRuntimeDiagnostics;
use mirante4d_domain::{
    IsoLightState, IsoShadingPolicy, Projection, RenderMode, SamplingPolicy, ToolKind, ViewerLayout,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::viewer_layout::PanelId;

use super::{AUTOMATION_SCHEMA_VERSION, AUTOMATION_SCRIPT_SCHEMA};

pub(super) const MAX_INPUT_SEQUENCE_SAMPLES: u32 = 4_096;
pub(super) const MAX_INPUT_SEQUENCE_DURATION_MS: u64 = 120_000;
pub(super) const MAX_SLEEP_FRAMES: u32 = 600;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProductAutomationScript {
    pub(super) schema: String,
    pub(super) schema_version: u32,
    pub(super) scenario: String,
    /// Optional asynchronous GPU timestamps for product diagnostics. Normal
    /// interactive startup keeps this renderer instrumentation off.
    #[serde(default)]
    pub(super) gpu_timing: bool,
    pub(super) hard_safety_limits: ProductAutomationHardSafetyLimits,
    pub(super) commands: Vec<ProductAutomationCommand>,
}

impl ProductAutomationScript {
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        if self.schema != AUTOMATION_SCRIPT_SCHEMA {
            anyhow::bail!(
                "unsupported automation script schema {:?}; expected {AUTOMATION_SCRIPT_SCHEMA:?}",
                self.schema
            );
        }
        if self.schema_version != AUTOMATION_SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported automation script schema version {}; expected {}",
                self.schema_version,
                AUTOMATION_SCHEMA_VERSION
            );
        }
        if self.commands.is_empty() {
            anyhow::bail!("automation script must contain at least one command");
        }
        for command in &self.commands {
            command.validate()?;
        }
        Ok(())
    }

    pub(super) fn empty_failed_script() -> Self {
        Self {
            schema: AUTOMATION_SCRIPT_SCHEMA.to_owned(),
            schema_version: AUTOMATION_SCHEMA_VERSION,
            scenario: "failed_to_initialize".to_owned(),
            gpu_timing: false,
            hard_safety_limits: ProductAutomationHardSafetyLimits::default(),
            commands: Vec::new(),
        }
    }

    pub(super) fn requires_validation_capture(&self) -> bool {
        self.commands.iter().any(|command| match command {
            ProductAutomationCommand::CaptureScreenshot { .. } => true,
            ProductAutomationCommand::Assert { condition } => {
                condition.requires_validation_capture()
            }
            _ => false,
        })
    }

    pub(super) const fn requires_gpu_timing(&self) -> bool {
        self.gpu_timing
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct ProductAutomationHardSafetyLimits {
    pub(super) max_cpu_total_bytes: Option<u64>,
    pub(super) max_cpu_decoded_residency_bytes: Option<u64>,
    pub(super) max_cpu_upload_staging_bytes: Option<u64>,
    pub(super) max_cpu_in_flight_decode_bytes: Option<u64>,
    pub(super) max_cpu_metadata_and_indexes_bytes: Option<u64>,
    pub(super) max_cpu_queues_and_results_bytes: Option<u64>,
    pub(super) max_cpu_prefetch_bytes: Option<u64>,
    pub(super) max_cpu_import_working_set_bytes: Option<u64>,
    pub(super) max_runtime_queued_requests: Option<u64>,
    pub(super) max_runtime_in_flight_decodes: Option<u64>,
    pub(super) max_runtime_pending_completions: Option<u64>,
    pub(super) max_runtime_resident_resources: Option<u64>,
}

impl ProductAutomationHardSafetyLimits {
    pub(super) fn check_dataset_runtime(
        self,
        diagnostics: DatasetRuntimeDiagnostics,
    ) -> Result<(), String> {
        check_limit(
            "cpu_total_bytes",
            diagnostics.total_used_bytes(),
            self.max_cpu_total_bytes,
        )?;
        check_limit(
            "cpu_decoded_residency_bytes",
            diagnostics.category_used_bytes(CpuLedgerCategory::DecodedResidency),
            self.max_cpu_decoded_residency_bytes,
        )?;
        check_limit(
            "cpu_upload_staging_bytes",
            diagnostics.category_used_bytes(CpuLedgerCategory::UploadStaging),
            self.max_cpu_upload_staging_bytes,
        )?;
        check_limit(
            "cpu_in_flight_decode_bytes",
            diagnostics.category_used_bytes(CpuLedgerCategory::InFlightDecode),
            self.max_cpu_in_flight_decode_bytes,
        )?;
        check_limit(
            "cpu_metadata_and_indexes_bytes",
            diagnostics.category_used_bytes(CpuLedgerCategory::MetadataAndIndexes),
            self.max_cpu_metadata_and_indexes_bytes,
        )?;
        check_limit(
            "cpu_queues_and_results_bytes",
            diagnostics.category_used_bytes(CpuLedgerCategory::QueuesAndResults),
            self.max_cpu_queues_and_results_bytes,
        )?;
        check_limit(
            "cpu_prefetch_bytes",
            diagnostics.category_used_bytes(CpuLedgerCategory::Prefetch),
            self.max_cpu_prefetch_bytes,
        )?;
        check_limit(
            "cpu_import_working_set_bytes",
            diagnostics.category_used_bytes(CpuLedgerCategory::ImportWorkingSet),
            self.max_cpu_import_working_set_bytes,
        )?;
        check_limit(
            "runtime_queued_requests",
            diagnostics.queued_requests() as u64,
            self.max_runtime_queued_requests,
        )?;
        check_limit(
            "runtime_in_flight_decodes",
            diagnostics.in_flight_decodes() as u64,
            self.max_runtime_in_flight_decodes,
        )?;
        check_limit(
            "runtime_pending_completions",
            diagnostics.pending_completions() as u64,
            self.max_runtime_pending_completions,
        )?;
        check_limit(
            "runtime_resident_resources",
            diagnostics.resident_resources() as u64,
            self.max_runtime_resident_resources,
        )?;
        Ok(())
    }
}

fn check_limit(name: &'static str, observed: u64, limit: Option<u64>) -> Result<(), String> {
    if let Some(limit) = limit
        && observed > limit
    {
        return Err(format!(
            "automation hard-safety limit exceeded for {name}: observed {observed}, limit {limit}"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub(super) struct ProductAutomationLimitObservations {
    pub(super) max_cpu_total_bytes: u64,
    pub(super) max_cpu_decoded_residency_bytes: u64,
    pub(super) max_cpu_upload_staging_bytes: u64,
    pub(super) max_cpu_in_flight_decode_bytes: u64,
    pub(super) max_cpu_metadata_and_indexes_bytes: u64,
    pub(super) max_cpu_queues_and_results_bytes: u64,
    pub(super) max_cpu_prefetch_bytes: u64,
    pub(super) max_cpu_import_working_set_bytes: u64,
    pub(super) max_runtime_queued_requests: u64,
    pub(super) max_runtime_in_flight_decodes: u64,
    pub(super) max_runtime_pending_completions: u64,
    pub(super) max_runtime_resident_resources: u64,
}

impl ProductAutomationLimitObservations {
    pub(super) fn observe_dataset_runtime(&mut self, diagnostics: DatasetRuntimeDiagnostics) {
        self.max_cpu_total_bytes = self.max_cpu_total_bytes.max(diagnostics.total_used_bytes());
        self.max_cpu_decoded_residency_bytes = self
            .max_cpu_decoded_residency_bytes
            .max(diagnostics.category_used_bytes(CpuLedgerCategory::DecodedResidency));
        self.max_cpu_upload_staging_bytes = self
            .max_cpu_upload_staging_bytes
            .max(diagnostics.category_used_bytes(CpuLedgerCategory::UploadStaging));
        self.max_cpu_in_flight_decode_bytes = self
            .max_cpu_in_flight_decode_bytes
            .max(diagnostics.category_used_bytes(CpuLedgerCategory::InFlightDecode));
        self.max_cpu_metadata_and_indexes_bytes = self
            .max_cpu_metadata_and_indexes_bytes
            .max(diagnostics.category_used_bytes(CpuLedgerCategory::MetadataAndIndexes));
        self.max_cpu_queues_and_results_bytes = self
            .max_cpu_queues_and_results_bytes
            .max(diagnostics.category_used_bytes(CpuLedgerCategory::QueuesAndResults));
        self.max_cpu_prefetch_bytes = self
            .max_cpu_prefetch_bytes
            .max(diagnostics.category_used_bytes(CpuLedgerCategory::Prefetch));
        self.max_cpu_import_working_set_bytes = self
            .max_cpu_import_working_set_bytes
            .max(diagnostics.category_used_bytes(CpuLedgerCategory::ImportWorkingSet));
        self.max_runtime_queued_requests = self
            .max_runtime_queued_requests
            .max(diagnostics.queued_requests() as u64);
        self.max_runtime_in_flight_decodes = self
            .max_runtime_in_flight_decodes
            .max(diagnostics.in_flight_decodes() as u64);
        self.max_runtime_pending_completions = self
            .max_runtime_pending_completions
            .max(diagnostics.pending_completions() as u64);
        self.max_runtime_resident_resources = self
            .max_runtime_resident_resources
            .max(diagnostics.resident_resources() as u64);
    }

    pub(super) fn json(self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| Value::Null)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ProductAutomationCommand {
    OpenDataset {
        path: PathBuf,
    },
    SwitchDataset {
        path: PathBuf,
    },
    NewProject,
    InitialSaveWithEdit {
        path: PathBuf,
    },
    OpenProject {
        path: PathBuf,
    },
    RecoverAutomaticAutosave,
    SaveProjectAs {
        path: PathBuf,
    },
    CloseProjectStore,
    WriteExternalKillCheckpoint {
        path: PathBuf,
        stage: String,
    },
    HoldForExternalKill,
    CancelSourceVerification,
    CancelActiveSourceVerification,
    RequestSourceVerification,
    BeginTiffImportSetup {
        source: PathBuf,
        output_parent: PathBuf,
    },
    StartReviewedImport {
        spacing_zyx_um: [f64; 3],
        time_step_seconds: Option<f64>,
        no_data_sentinel: Option<u8>,
        working_memory_bytes: u64,
    },
    WaitForImportProgress {
        stage: String,
        minimum_completed_work_units: u64,
        timeout_ms: u64,
    },
    CancelImport,
    WaitForImportedOpenReady {
        path: PathBuf,
        timeout_ms: u64,
    },
    WaitFor {
        condition: ProductAutomationWaitCondition,
        timeout_ms: u64,
    },
    SetViewportSize {
        width: u32,
        height: u32,
    },
    SetMappedClientPixels {
        width: u32,
        height: u32,
    },
    SetRenderTargetSize {
        width: u32,
        height: u32,
    },
    SetViewerLayout {
        layout: ProductAutomationViewerLayout,
    },
    SetTimeIndex {
        time_index: u64,
    },
    SetLayerVisibility {
        layer_index: usize,
        visible: bool,
    },
    SetLayerOrder {
        layer_indices: Vec<usize>,
    },
    SetRenderMode {
        mode: ProductAutomationRenderMode,
    },
    SetLayerRenderMode {
        layer_index: usize,
        mode: ProductAutomationRenderMode,
    },
    SetProjection {
        projection: ProductAutomationProjection,
    },
    SetLayerSampling {
        layer_index: usize,
        sampling: ProductAutomationSamplingPolicy,
    },
    SetLayerIsoShading {
        layer_index: usize,
        shading: ProductAutomationIsoShading,
    },
    SetIsoLight {
        light: ProductAutomationIsoLight,
    },
    SetIsoDisplayLevel {
        display_level: f32,
    },
    SetDvrDensityScale {
        density_scale: f64,
    },
    SetLayerOpacity {
        layer_index: usize,
        opacity: f32,
    },
    SetLayerWindow {
        layer_index: usize,
        low: f32,
        high: f32,
    },
    CameraFitData,
    CameraOrbit {
        yaw_points: f32,
        pitch_points: f32,
    },
    CameraPan {
        x_points: f32,
        y_points: f32,
    },
    CameraZoom {
        scroll_y_points: f32,
    },
    CameraOrbitSequence {
        samples: u32,
        duration_ms: u64,
        yaw_points_per_sample: f32,
        pitch_points_per_sample: f32,
    },
    CameraPanSequence {
        samples: u32,
        duration_ms: u64,
        x_points_per_sample: f32,
        y_points_per_sample: f32,
    },
    CameraZoomSequence {
        samples: u32,
        duration_ms: u64,
        scroll_y_points_per_sample: f32,
    },
    SetActiveCrossSectionPanel {
        panel: ProductAutomationPanelId,
    },
    SetCrossSectionView {
        center_world: [f64; 3],
        orientation_xyzw: [f64; 4],
        scale_world_per_screen_point: f64,
        depth_world: f64,
    },
    CrossSectionRotateSequence {
        panel: ProductAutomationPanelId,
        samples: u32,
        duration_ms: u64,
        x_points_per_sample: f64,
        y_points_per_sample: f64,
        radians_per_point: f64,
    },
    CrossSectionPanSequence {
        panel: ProductAutomationPanelId,
        samples: u32,
        duration_ms: u64,
        x_points_per_sample: f64,
        y_points_per_sample: f64,
    },
    CrossSectionZoomSequence {
        panel: ProductAutomationPanelId,
        samples: u32,
        duration_ms: u64,
        x_fraction: f64,
        y_fraction: f64,
        factor_per_sample: f64,
    },
    CrossSectionSliceSequence {
        panel: ProductAutomationPanelId,
        samples: u32,
        duration_ms: u64,
        distance_world_per_sample: f64,
    },
    SetActiveTool {
        tool: ProductAutomationViewerTool,
    },
    ProbeHover {
        x_fraction: f32,
        y_fraction: f32,
    },
    PrimaryClick {
        x_fraction: f32,
        y_fraction: f32,
    },
    CopyDiagnostics,
    CaptureScreenshot {
        name: Option<String>,
    },
    Assert {
        condition: ProductAutomationAssertCondition,
    },
    SleepFrames {
        frames: u32,
    },
    Quit,
}

impl ProductAutomationCommand {
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::OpenDataset { .. } => "open_dataset",
            Self::SwitchDataset { .. } => "switch_dataset",
            Self::NewProject => "new_project",
            Self::InitialSaveWithEdit { .. } => "initial_save_with_edit",
            Self::OpenProject { .. } => "open_project",
            Self::RecoverAutomaticAutosave => "recover_automatic_autosave",
            Self::SaveProjectAs { .. } => "save_project_as",
            Self::CloseProjectStore => "close_project_store",
            Self::WriteExternalKillCheckpoint { .. } => "write_external_kill_checkpoint",
            Self::HoldForExternalKill => "hold_for_external_kill",
            Self::CancelSourceVerification => "cancel_source_verification",
            Self::CancelActiveSourceVerification => "cancel_active_source_verification",
            Self::RequestSourceVerification => "request_source_verification",
            Self::BeginTiffImportSetup { .. } => "begin_tiff_import_setup",
            Self::StartReviewedImport { .. } => "start_reviewed_import",
            Self::WaitForImportProgress { .. } => "wait_for_import_progress",
            Self::CancelImport => "cancel_import",
            Self::WaitForImportedOpenReady { .. } => "wait_for_imported_open_ready",
            Self::WaitFor { .. } => "wait_for",
            Self::SetViewportSize { .. } => "set_viewport_size",
            Self::SetMappedClientPixels { .. } => "set_mapped_client_pixels",
            Self::SetRenderTargetSize { .. } => "set_render_target_size",
            Self::SetViewerLayout { .. } => "set_viewer_layout",
            Self::SetTimeIndex { .. } => "set_time_index",
            Self::SetLayerVisibility { .. } => "set_layer_visibility",
            Self::SetLayerOrder { .. } => "set_layer_order",
            Self::SetRenderMode { .. } => "set_render_mode",
            Self::SetLayerRenderMode { .. } => "set_layer_render_mode",
            Self::SetProjection { .. } => "set_projection",
            Self::SetLayerSampling { .. } => "set_layer_sampling",
            Self::SetLayerIsoShading { .. } => "set_layer_iso_shading",
            Self::SetIsoLight { .. } => "set_iso_light",
            Self::SetIsoDisplayLevel { .. } => "set_iso_display_level",
            Self::SetDvrDensityScale { .. } => "set_dvr_density_scale",
            Self::SetLayerOpacity { .. } => "set_layer_opacity",
            Self::SetLayerWindow { .. } => "set_layer_window",
            Self::CameraFitData => "camera_fit_data",
            Self::CameraOrbit { .. } => "camera_orbit",
            Self::CameraPan { .. } => "camera_pan",
            Self::CameraZoom { .. } => "camera_zoom",
            Self::CameraOrbitSequence { .. } => "camera_orbit_sequence",
            Self::CameraPanSequence { .. } => "camera_pan_sequence",
            Self::CameraZoomSequence { .. } => "camera_zoom_sequence",
            Self::SetActiveCrossSectionPanel { .. } => "set_active_cross_section_panel",
            Self::SetCrossSectionView { .. } => "set_cross_section_view",
            Self::CrossSectionRotateSequence { .. } => "cross_section_rotate_sequence",
            Self::CrossSectionPanSequence { .. } => "cross_section_pan_sequence",
            Self::CrossSectionZoomSequence { .. } => "cross_section_zoom_sequence",
            Self::CrossSectionSliceSequence { .. } => "cross_section_slice_sequence",
            Self::SetActiveTool { .. } => "set_active_tool",
            Self::ProbeHover { .. } => "probe_hover",
            Self::PrimaryClick { .. } => "primary_click",
            Self::CopyDiagnostics => "copy_diagnostics",
            Self::CaptureScreenshot { .. } => "capture_screenshot",
            Self::Assert { .. } => "assert",
            Self::SleepFrames { .. } => "sleep_frames",
            Self::Quit => "quit",
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        if let Self::SwitchDataset { path } = self
            && path.as_os_str().is_empty()
        {
            anyhow::bail!("switch_dataset requires a nonempty package path");
        }
        if let Self::SetCrossSectionView {
            center_world,
            orientation_xyzw,
            scale_world_per_screen_point,
            depth_world,
        } = self
            && (!center_world.iter().all(|value| value.is_finite())
                || !orientation_xyzw.iter().all(|value| value.is_finite())
                || orientation_xyzw.iter().all(|value| *value == 0.0)
                || !scale_world_per_screen_point.is_finite()
                || *scale_world_per_screen_point <= 0.0
                || !depth_world.is_finite()
                || *depth_world <= 0.0)
        {
            anyhow::bail!(
                "cross-section view must contain finite coordinates and positive scale and depth"
            );
        }
        let sequence = match self {
            Self::CameraOrbitSequence {
                samples,
                duration_ms,
                yaw_points_per_sample,
                pitch_points_per_sample,
            } => Some((
                *samples,
                *duration_ms,
                yaw_points_per_sample.is_finite()
                    && pitch_points_per_sample.is_finite()
                    && (*yaw_points_per_sample != 0.0 || *pitch_points_per_sample != 0.0),
            )),
            Self::CameraPanSequence {
                samples,
                duration_ms,
                x_points_per_sample,
                y_points_per_sample,
            } => Some((
                *samples,
                *duration_ms,
                x_points_per_sample.is_finite()
                    && y_points_per_sample.is_finite()
                    && (*x_points_per_sample != 0.0 || *y_points_per_sample != 0.0),
            )),
            Self::CameraZoomSequence {
                samples,
                duration_ms,
                scroll_y_points_per_sample,
            } => Some((
                *samples,
                *duration_ms,
                scroll_y_points_per_sample.is_finite() && *scroll_y_points_per_sample != 0.0,
            )),
            Self::CrossSectionRotateSequence {
                samples,
                duration_ms,
                x_points_per_sample,
                y_points_per_sample,
                radians_per_point,
                ..
            } => Some((
                *samples,
                *duration_ms,
                x_points_per_sample.is_finite()
                    && y_points_per_sample.is_finite()
                    && radians_per_point.is_finite()
                    && (*x_points_per_sample != 0.0 || *y_points_per_sample != 0.0)
                    && *radians_per_point != 0.0,
            )),
            Self::CrossSectionPanSequence {
                samples,
                duration_ms,
                x_points_per_sample,
                y_points_per_sample,
                ..
            } => Some((
                *samples,
                *duration_ms,
                x_points_per_sample.is_finite()
                    && y_points_per_sample.is_finite()
                    && (*x_points_per_sample != 0.0 || *y_points_per_sample != 0.0),
            )),
            Self::CrossSectionZoomSequence {
                samples,
                duration_ms,
                x_fraction,
                y_fraction,
                factor_per_sample,
                ..
            } => {
                if !(0.0..=1.0).contains(x_fraction)
                    || !(0.0..=1.0).contains(y_fraction)
                    || !factor_per_sample.is_finite()
                    || *factor_per_sample <= 0.0
                    || *factor_per_sample == 1.0
                {
                    anyhow::bail!("cross-section zoom sequence has invalid anchor or factor");
                }
                Some((*samples, *duration_ms, true))
            }
            Self::CrossSectionSliceSequence {
                samples,
                duration_ms,
                distance_world_per_sample,
                ..
            } => Some((
                *samples,
                *duration_ms,
                distance_world_per_sample.is_finite() && *distance_world_per_sample != 0.0,
            )),
            _ => None,
        };
        if let Some((samples, duration_ms, values_are_finite)) = sequence {
            if samples == 0 || samples > MAX_INPUT_SEQUENCE_SAMPLES {
                anyhow::bail!(
                    "automation input sequence samples must be in 1..={MAX_INPUT_SEQUENCE_SAMPLES}"
                );
            }
            if duration_ms == 0 || duration_ms > MAX_INPUT_SEQUENCE_DURATION_MS {
                anyhow::bail!(
                    "automation input sequence duration_ms must be in 1..={MAX_INPUT_SEQUENCE_DURATION_MS}"
                );
            }
            if !values_are_finite {
                anyhow::bail!("automation input sequence deltas must be finite and nonzero");
            }
        }
        if let Self::SleepFrames { frames } = self
            && !(1..=MAX_SLEEP_FRAMES).contains(frames)
        {
            anyhow::bail!("sleep_frames frames must be in 1..={MAX_SLEEP_FRAMES}");
        }
        if let Self::SetLayerOrder { layer_indices } = self
            && (layer_indices.is_empty()
                || layer_indices.len() > mirante4d_render_api::MAX_RENDER_LAYERS)
        {
            anyhow::bail!(
                "layer order must contain 1..={} indices",
                mirante4d_render_api::MAX_RENDER_LAYERS
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationWaitCondition {
    WindowReady,
    FirstFrame,
    RuntimeIdle,
    FrameFreshnessCurrent,
    CoordinatedPresentationSettled,
    SourceVerificationInactive,
    SourceVerificationRequired,
    SourceVerificationVerified,
    ImportReviewReady,
    ImportIdle,
    ProjectStoreIdle,
    ProjectAutosaved,
    RecoveryReviewRequired,
    ProjectStoreClosed,
}

impl ProductAutomationWaitCondition {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::WindowReady => "window_ready",
            Self::FirstFrame => "first_frame",
            Self::RuntimeIdle => "runtime_idle",
            Self::FrameFreshnessCurrent => "frame_freshness_current",
            Self::CoordinatedPresentationSettled => "coordinated_presentation_settled",
            Self::SourceVerificationInactive => "source_verification_inactive",
            Self::SourceVerificationRequired => "source_verification_required",
            Self::SourceVerificationVerified => "source_verification_verified",
            Self::ImportReviewReady => "import_review_ready",
            Self::ImportIdle => "import_idle",
            Self::ProjectStoreIdle => "project_store_idle",
            Self::ProjectAutosaved => "project_autosaved",
            Self::RecoveryReviewRequired => "recovery_review_required",
            Self::ProjectStoreClosed => "project_store_closed",
        }
    }

    pub(super) const fn is_passive(self) -> bool {
        matches!(
            self,
            Self::ProjectStoreIdle
                | Self::ProjectAutosaved
                | Self::RecoveryReviewRequired
                | Self::ProjectStoreClosed
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationAssertCondition {
    NonblankFrame,
    NoRenderError,
    FrameFidelity {
        scale_level: u32,
        complete: bool,
    },
    RenderMode {
        mode: ProductAutomationRenderMode,
    },
    Projection {
        projection: ProductAutomationProjection,
    },
    LayerSampling {
        layer_index: usize,
        sampling: ProductAutomationSamplingPolicy,
    },
    LayerIsoShading {
        layer_index: usize,
        shading: ProductAutomationIsoShading,
    },
    IsoLight {
        light: ProductAutomationIsoLight,
    },
    ActiveTool {
        tool: ProductAutomationViewerTool,
    },
    CrosshairLinked,
    RoiCommitted,
    DistanceCommitted,
    ViewerLayout {
        layout: ProductAutomationViewerLayout,
    },
    CrossSectionPanelSchedule {
        panel: ProductAutomationPanelId,
        min_generation: Option<u64>,
        min_selected_resources: Option<usize>,
    },
    FourPanelImagesDistinct {
        min_different_pixels: Option<usize>,
    },
    CrossSectionRetired,
    SourceVerificationEvidence {
        min_accepted_progress_updates: u64,
        min_cancelled_runs: u64,
        min_accepted_successes: u64,
    },
    ImportWorkflowEvidence {
        required_stage_names: Vec<String>,
        min_projected_named_stages: usize,
        min_cancelled_runs: u64,
        min_successful_runs: u64,
        min_resumed_work_units: u64,
        min_elapsed_ms: u64,
        min_projected_elapsed_ms: u64,
        max_peak_working_bytes: u64,
    },
    RenderTargetPixels {
        width: u64,
        height: u64,
    },
    ProjectState {
        bound: bool,
        dirty: bool,
        lifecycle: ProductAutomationProjectStoreLifecycle,
        can_save: bool,
        can_save_as: bool,
        manual: bool,
        autosave: bool,
    },
}

impl ProductAutomationAssertCondition {
    fn requires_validation_capture(&self) -> bool {
        matches!(
            self,
            Self::NonblankFrame | Self::FourPanelImagesDistinct { .. }
        )
    }

    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::NonblankFrame => "nonblank_frame",
            Self::NoRenderError => "no_render_error",
            Self::FrameFidelity { .. } => "frame_fidelity",
            Self::RenderMode { .. } => "render_mode",
            Self::Projection { .. } => "projection",
            Self::LayerSampling { .. } => "layer_sampling",
            Self::LayerIsoShading { .. } => "layer_iso_shading",
            Self::IsoLight { .. } => "iso_light",
            Self::ActiveTool { .. } => "active_tool",
            Self::CrosshairLinked => "crosshair_linked",
            Self::RoiCommitted => "roi_committed",
            Self::DistanceCommitted => "distance_committed",
            Self::ViewerLayout { .. } => "viewer_layout",
            Self::CrossSectionPanelSchedule { .. } => "cross_section_panel_schedule",
            Self::FourPanelImagesDistinct { .. } => "four_panel_images_distinct",
            Self::CrossSectionRetired => "cross_section_retired",
            Self::SourceVerificationEvidence { .. } => "source_verification_evidence",
            Self::ImportWorkflowEvidence { .. } => "import_workflow_evidence",
            Self::RenderTargetPixels { .. } => "render_target_pixels",
            Self::ProjectState { .. } => "project_state",
        }
    }

    pub(super) fn is_cross_section_condition(&self) -> bool {
        matches!(
            self,
            Self::CrossSectionPanelSchedule { .. }
                | Self::FourPanelImagesDistinct { .. }
                | Self::CrossSectionRetired
                | Self::CrosshairLinked
        )
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationProjectStoreLifecycle {
    Established,
    RecoverySelected,
}

impl ProductAutomationProjectStoreLifecycle {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Established => "established",
            Self::RecoverySelected => "recovery_selected",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationViewerLayout {
    Single3d,
    FourPanel,
}

impl ProductAutomationViewerLayout {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Single3d => "single3d",
            Self::FourPanel => "four_panel",
        }
    }
}

impl From<ProductAutomationViewerLayout> for ViewerLayout {
    fn from(value: ProductAutomationViewerLayout) -> Self {
        match value {
            ProductAutomationViewerLayout::Single3d => Self::Single3d,
            ProductAutomationViewerLayout::FourPanel => Self::FourPanel,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationPanelId {
    Xy,
    Xz,
    Yz,
}

impl From<ProductAutomationPanelId> for PanelId {
    fn from(value: ProductAutomationPanelId) -> Self {
        match value {
            ProductAutomationPanelId::Xy => Self::Xy,
            ProductAutomationPanelId::Xz => Self::Xz,
            ProductAutomationPanelId::Yz => Self::Yz,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationRenderMode {
    Mip,
    Dvr,
    Iso,
}

impl ProductAutomationRenderMode {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Mip => "mip",
            Self::Dvr => "dvr",
            Self::Iso => "iso",
        }
    }
}

impl From<ProductAutomationRenderMode> for RenderMode {
    fn from(value: ProductAutomationRenderMode) -> Self {
        match value {
            ProductAutomationRenderMode::Mip => Self::Mip,
            ProductAutomationRenderMode::Dvr => Self::Dvr,
            ProductAutomationRenderMode::Iso => Self::Isosurface,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationProjection {
    Orthographic,
    Perspective,
}

impl ProductAutomationProjection {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Orthographic => "orthographic",
            Self::Perspective => "perspective",
        }
    }
}

impl From<ProductAutomationProjection> for Projection {
    fn from(value: ProductAutomationProjection) -> Self {
        match value {
            ProductAutomationProjection::Orthographic => Self::Orthographic,
            ProductAutomationProjection::Perspective => Self::Perspective,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationSamplingPolicy {
    VoxelExact,
    SmoothLinear,
}

impl ProductAutomationSamplingPolicy {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::VoxelExact => "voxel_exact",
            Self::SmoothLinear => "smooth_linear",
        }
    }
}

impl From<ProductAutomationSamplingPolicy> for SamplingPolicy {
    fn from(value: ProductAutomationSamplingPolicy) -> Self {
        match value {
            ProductAutomationSamplingPolicy::VoxelExact => Self::VoxelExact,
            ProductAutomationSamplingPolicy::SmoothLinear => Self::SmoothLinear,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationIsoShading {
    Flat,
    GradientLighting,
}

impl ProductAutomationIsoShading {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::GradientLighting => "gradient_lighting",
        }
    }
}

impl From<ProductAutomationIsoShading> for IsoShadingPolicy {
    fn from(value: ProductAutomationIsoShading) -> Self {
        match value {
            ProductAutomationIsoShading::Flat => Self::Flat,
            ProductAutomationIsoShading::GradientLighting => Self::GradientLighting,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ProductAutomationIsoLight {
    AttachedCamera,
    DetachedScreen { x: f32, y: f32 },
}

impl ProductAutomationIsoLight {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::AttachedCamera => "attached_camera",
            Self::DetachedScreen { .. } => "detached_screen",
        }
    }

    pub(super) fn into_domain(self) -> Result<IsoLightState, String> {
        match self {
            Self::AttachedCamera => Ok(IsoLightState::attached_camera()),
            Self::DetachedScreen { x, y } => {
                IsoLightState::detached_screen(x, y).map_err(|error| error.to_string())
            }
        }
    }

    pub(super) fn matches(self, actual: IsoLightState) -> bool {
        match self {
            Self::AttachedCamera => actual.is_attached_camera(),
            Self::DetachedScreen { x, y } => actual.detached_screen_position() == Some([x, y]),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationViewerTool {
    Navigate,
    Inspect,
    Crosshair,
    RoiBox,
    MeasureDistance,
}

impl ProductAutomationViewerTool {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Navigate => "navigate",
            Self::Inspect => "inspect",
            Self::Crosshair => "crosshair",
            Self::RoiBox => "roi_box",
            Self::MeasureDistance => "measure_distance",
        }
    }
}

impl From<ProductAutomationViewerTool> for ToolKind {
    fn from(value: ProductAutomationViewerTool) -> Self {
        match value {
            ProductAutomationViewerTool::Navigate => Self::Navigate,
            ProductAutomationViewerTool::Inspect => Self::Inspect,
            ProductAutomationViewerTool::Crosshair => Self::Crosshair,
            ProductAutomationViewerTool::RoiBox => Self::RoiBox,
            ProductAutomationViewerTool::MeasureDistance => Self::MeasureDistance,
        }
    }
}

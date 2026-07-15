use std::path::PathBuf;

use mirante4d_dataset::CpuLedgerCategory;
use mirante4d_dataset_runtime::DatasetRuntimeDiagnostics;
use mirante4d_domain::{RenderMode, ViewerLayout};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::viewer_layout::PanelId;

use super::{AUTOMATION_SCHEMA_VERSION, AUTOMATION_SCRIPT_SCHEMA};

#[derive(Debug, Deserialize)]
pub(super) struct ProductAutomationScript {
    pub(super) schema: String,
    pub(super) schema_version: u32,
    pub(super) scenario: String,
    #[serde(default)]
    pub(super) limits: ProductAutomationLimits,
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
        Ok(())
    }

    pub(super) fn empty_failed_script() -> Self {
        Self {
            schema: AUTOMATION_SCRIPT_SCHEMA.to_owned(),
            schema_version: AUTOMATION_SCHEMA_VERSION,
            scenario: "failed_to_initialize".to_owned(),
            limits: ProductAutomationLimits::default(),
            commands: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct ProductAutomationLimits {
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

impl ProductAutomationLimits {
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
            "automation limit exceeded for {name}: observed {observed}, limit {limit}"
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
    RequestSourceVerification,
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
    SetRenderMode {
        mode: ProductAutomationRenderMode,
    },
    SetLayerRenderMode {
        layer_index: usize,
        mode: ProductAutomationRenderMode,
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
        viewport_height_points: Option<f32>,
    },
    CameraPan {
        x_points: f32,
        y_points: f32,
        viewport_height_points: Option<f32>,
    },
    CameraZoom {
        scroll_y_points: f32,
    },
    ProbeHover {
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
    SleepOrFrames {
        millis: Option<u64>,
        frames: Option<u32>,
    },
    Quit,
}

impl ProductAutomationCommand {
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::OpenDataset { .. } => "open_dataset",
            Self::NewProject => "new_project",
            Self::InitialSaveWithEdit { .. } => "initial_save_with_edit",
            Self::OpenProject { .. } => "open_project",
            Self::RecoverAutomaticAutosave => "recover_automatic_autosave",
            Self::SaveProjectAs { .. } => "save_project_as",
            Self::CloseProjectStore => "close_project_store",
            Self::WriteExternalKillCheckpoint { .. } => "write_external_kill_checkpoint",
            Self::HoldForExternalKill => "hold_for_external_kill",
            Self::CancelSourceVerification => "cancel_source_verification",
            Self::RequestSourceVerification => "request_source_verification",
            Self::WaitFor { .. } => "wait_for",
            Self::SetViewportSize { .. } => "set_viewport_size",
            Self::SetMappedClientPixels { .. } => "set_mapped_client_pixels",
            Self::SetRenderTargetSize { .. } => "set_render_target_size",
            Self::SetViewerLayout { .. } => "set_viewer_layout",
            Self::SetRenderMode { .. } => "set_render_mode",
            Self::SetLayerRenderMode { .. } => "set_layer_render_mode",
            Self::SetIsoDisplayLevel { .. } => "set_iso_display_level",
            Self::SetDvrDensityScale { .. } => "set_dvr_density_scale",
            Self::SetLayerOpacity { .. } => "set_layer_opacity",
            Self::SetLayerWindow { .. } => "set_layer_window",
            Self::CameraFitData => "camera_fit_data",
            Self::CameraOrbit { .. } => "camera_orbit",
            Self::CameraPan { .. } => "camera_pan",
            Self::CameraZoom { .. } => "camera_zoom",
            Self::ProbeHover { .. } => "probe_hover",
            Self::CopyDiagnostics => "copy_diagnostics",
            Self::CaptureScreenshot { .. } => "capture_screenshot",
            Self::Assert { .. } => "assert",
            Self::SleepOrFrames { .. } => "sleep_or_frames",
            Self::Quit => "quit",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationWaitCondition {
    WindowReady,
    FirstFrame,
    RuntimeIdle,
    FrameFreshnessCurrent,
    SourceVerificationRequired,
    SourceVerificationVerified,
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
            Self::SourceVerificationRequired => "source_verification_required",
            Self::SourceVerificationVerified => "source_verification_verified",
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
    RenderMode {
        mode: ProductAutomationRenderMode,
    },
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
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::NonblankFrame => "nonblank_frame",
            Self::NoRenderError => "no_render_error",
            Self::RenderMode { .. } => "render_mode",
            Self::ViewerLayout { .. } => "viewer_layout",
            Self::CrossSectionPanelSchedule { .. } => "cross_section_panel_schedule",
            Self::FourPanelImagesDistinct { .. } => "four_panel_images_distinct",
            Self::CrossSectionRetired => "cross_section_retired",
            Self::SourceVerificationEvidence { .. } => "source_verification_evidence",
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
    Xz,
}

impl From<ProductAutomationPanelId> for PanelId {
    fn from(value: ProductAutomationPanelId) -> Self {
        match value {
            ProductAutomationPanelId::Xz => Self::Xz,
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

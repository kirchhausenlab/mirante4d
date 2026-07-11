use std::path::PathBuf;

use mirante4d_data::{BrickReadQueueDiagnostics, BrickRequestPriority, DataEngineStats};
use mirante4d_domain::{RenderMode, ViewerLayout};
use mirante4d_renderer::gpu::GpuRendererStats;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::viewer_layout::{CrossSectionPanelScheduleStatus, PanelId};

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
    pub(super) max_decoded_bytes: Option<u64>,
    pub(super) max_decoded_brick_bytes: Option<u64>,
    pub(super) max_encoded_payload_bytes_read: Option<u64>,
    pub(super) max_cpu_brick_cache_bytes: Option<u64>,
    pub(super) max_brick_requests_queued: Option<u64>,
    pub(super) max_brick_queue_depth: Option<u64>,
    pub(super) max_gpu_brick_atlas_uploaded_bytes: Option<u64>,
    pub(super) max_gpu_brick_atlas_resident_bytes: Option<u64>,
    pub(super) max_gpu_display_resource_resident_bytes: Option<u64>,
}

impl ProductAutomationLimits {
    pub(super) fn requires_data_engine(self) -> bool {
        self.max_decoded_bytes.is_some()
            || self.max_decoded_brick_bytes.is_some()
            || self.max_encoded_payload_bytes_read.is_some()
            || self.max_cpu_brick_cache_bytes.is_some()
            || self.max_brick_requests_queued.is_some()
    }

    pub(super) fn requires_gpu_renderer(self) -> bool {
        self.max_gpu_brick_atlas_uploaded_bytes.is_some()
            || self.max_gpu_brick_atlas_resident_bytes.is_some()
            || self.max_gpu_display_resource_resident_bytes.is_some()
    }

    pub(super) fn requires_brick_queue(self) -> bool {
        self.max_brick_queue_depth.is_some()
    }

    pub(super) fn check_data_engine(self, stats: DataEngineStats) -> Result<(), String> {
        check_limit("decoded_bytes", stats.decoded_bytes, self.max_decoded_bytes)?;
        check_limit(
            "decoded_brick_bytes",
            stats.decoded_brick_bytes,
            self.max_decoded_brick_bytes,
        )?;
        check_limit(
            "encoded_payload_bytes_read",
            stats.encoded_payload_bytes_read,
            self.max_encoded_payload_bytes_read,
        )?;
        check_limit(
            "cpu_brick_cache_bytes",
            stats.brick_cache_bytes,
            self.max_cpu_brick_cache_bytes,
        )?;
        check_limit(
            "brick_requests_queued",
            stats.brick_requests_queued,
            self.max_brick_requests_queued,
        )?;
        Ok(())
    }

    pub(super) fn check_brick_queue(self, queue: &BrickReadQueueDiagnostics) -> Result<(), String> {
        check_limit(
            "brick_queue_depth",
            queue.queued_total as u64,
            self.max_brick_queue_depth,
        )
    }

    pub(super) fn check_gpu_renderer(self, stats: GpuRendererStats) -> Result<(), String> {
        check_limit(
            "gpu_brick_atlas_uploaded_bytes",
            stats.brick_atlas_uploaded_bytes,
            self.max_gpu_brick_atlas_uploaded_bytes,
        )?;
        check_limit(
            "gpu_brick_atlas_resident_bytes",
            stats.brick_atlas_resident_bytes,
            self.max_gpu_brick_atlas_resident_bytes,
        )?;
        check_limit(
            "gpu_display_resource_resident_bytes",
            stats.display_resource_resident_bytes,
            self.max_gpu_display_resource_resident_bytes,
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
    pub(super) max_decoded_bytes: u64,
    pub(super) max_decoded_brick_bytes: u64,
    pub(super) max_encoded_payload_bytes_read: u64,
    pub(super) max_cpu_brick_cache_bytes: u64,
    pub(super) max_brick_requests_queued: u64,
    pub(super) max_brick_queue_depth: u64,
    pub(super) max_gpu_brick_atlas_uploaded_bytes: Option<u64>,
    pub(super) max_gpu_brick_atlas_resident_bytes: Option<u64>,
    pub(super) max_gpu_display_resource_resident_bytes: Option<u64>,
}

impl ProductAutomationLimitObservations {
    pub(super) fn observe_data_engine(&mut self, stats: DataEngineStats) {
        self.max_decoded_bytes = self.max_decoded_bytes.max(stats.decoded_bytes);
        self.max_decoded_brick_bytes = self.max_decoded_brick_bytes.max(stats.decoded_brick_bytes);
        self.max_encoded_payload_bytes_read = self
            .max_encoded_payload_bytes_read
            .max(stats.encoded_payload_bytes_read);
        self.max_cpu_brick_cache_bytes =
            self.max_cpu_brick_cache_bytes.max(stats.brick_cache_bytes);
        self.max_brick_requests_queued = self
            .max_brick_requests_queued
            .max(stats.brick_requests_queued);
    }

    pub(super) fn observe_brick_queue(&mut self, queue: &BrickReadQueueDiagnostics) {
        self.max_brick_queue_depth = self.max_brick_queue_depth.max(queue.queued_total as u64);
    }

    pub(super) fn observe_gpu_renderer(&mut self, stats: GpuRendererStats) {
        self.max_gpu_brick_atlas_uploaded_bytes = Some(
            self.max_gpu_brick_atlas_uploaded_bytes
                .unwrap_or(0)
                .max(stats.brick_atlas_uploaded_bytes),
        );
        self.max_gpu_brick_atlas_resident_bytes = Some(
            self.max_gpu_brick_atlas_resident_bytes
                .unwrap_or(0)
                .max(stats.brick_atlas_resident_bytes),
        );
        self.max_gpu_display_resource_resident_bytes = Some(
            self.max_gpu_display_resource_resident_bytes
                .unwrap_or(0)
                .max(stats.display_resource_resident_bytes),
        );
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
    WaitFor {
        condition: ProductAutomationWaitCondition,
        timeout_ms: u64,
    },
    SetViewportSize {
        width: u32,
        height: u32,
    },
    SetViewerLayout {
        layout: ProductAutomationViewerLayout,
    },
    SetTimepoint {
        timepoint: u64,
    },
    StepTimepoint {
        delta: i64,
    },
    SetPlayback {
        playing: bool,
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
    SetChannelVisibility {
        layer_index: usize,
        visible: bool,
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
    CameraReset,
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
    CrossSectionPan {
        panel: ProductAutomationPanelId,
        x_points: f32,
        y_points: f32,
        probe_after: Option<ProductAutomationPanelHoverProbe>,
    },
    CrossSectionSliceStep {
        panel: ProductAutomationPanelId,
        notches: f64,
        #[serde(default)]
        fast: bool,
    },
    CrossSectionZoom {
        panel: ProductAutomationPanelId,
        x_fraction: f32,
        y_fraction: f32,
        scroll_y_points: f32,
    },
    CrossSectionRotate {
        panel: ProductAutomationPanelId,
        x_points: f32,
        y_points: f32,
    },
    ProbePanelHover {
        panel: ProductAutomationPanelId,
        x_fraction: f32,
        y_fraction: f32,
        expected_status: Option<ProductAutomationCrossSectionHoverStatus>,
        expect_value: Option<bool>,
        expected_generation_status: Option<ProductAutomationCrossSectionGenerationStatus>,
        expected_display_current: Option<bool>,
        expected_target_generation: Option<u64>,
        expected_displayed_generation: Option<u64>,
        expected_schedule_generation: Option<u64>,
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
            Self::WaitFor { .. } => "wait_for",
            Self::SetViewportSize { .. } => "set_viewport_size",
            Self::SetViewerLayout { .. } => "set_viewer_layout",
            Self::SetTimepoint { .. } => "set_timepoint",
            Self::StepTimepoint { .. } => "step_timepoint",
            Self::SetPlayback { .. } => "set_playback",
            Self::SetRenderMode { .. } => "set_render_mode",
            Self::SetLayerRenderMode { .. } => "set_layer_render_mode",
            Self::SetIsoDisplayLevel { .. } => "set_iso_display_level",
            Self::SetDvrDensityScale { .. } => "set_dvr_density_scale",
            Self::SetChannelVisibility { .. } => "set_channel_visibility",
            Self::SetLayerOpacity { .. } => "set_layer_opacity",
            Self::SetLayerWindow { .. } => "set_layer_window",
            Self::CameraFitData => "camera_fit_data",
            Self::CameraReset => "camera_reset",
            Self::CameraOrbit { .. } => "camera_orbit",
            Self::CameraPan { .. } => "camera_pan",
            Self::CameraZoom { .. } => "camera_zoom",
            Self::CrossSectionPan { .. } => "cross_section_pan",
            Self::CrossSectionSliceStep { .. } => "cross_section_slice_step",
            Self::CrossSectionZoom { .. } => "cross_section_zoom",
            Self::CrossSectionRotate { .. } => "cross_section_rotate",
            Self::ProbePanelHover { .. } => "probe_panel_hover",
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
    NoRenderError,
    GpuFramePresented,
}

impl ProductAutomationWaitCondition {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::WindowReady => "window_ready",
            Self::FirstFrame => "first_frame",
            Self::RuntimeIdle => "runtime_idle",
            Self::FrameFreshnessCurrent => "frame_freshness_current",
            Self::NoRenderError => "no_render_error",
            Self::GpuFramePresented => "gpu_frame_presented",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationAssertCondition {
    NonblankFrame,
    NoRenderError,
    FrameFreshnessCurrent,
    RuntimeIdle,
    RenderMode {
        mode: ProductAutomationRenderMode,
    },
    ViewerLayout {
        layout: ProductAutomationViewerLayout,
    },
    ActiveTimepoint {
        timepoint: u64,
    },
    ObservedTimepoints {
        min_distinct: usize,
    },
    Playback {
        playing: bool,
    },
    CrossSectionActivePanel {
        panel: Option<ProductAutomationPanelId>,
    },
    CrossSectionPanelSchedule {
        panel: ProductAutomationPanelId,
        status: Option<ProductAutomationCrossSectionScheduleStatus>,
        min_generation: Option<u64>,
        target_scale_level: Option<u32>,
        render_scale_level: Option<u32>,
        min_selected_bricks: Option<usize>,
        max_missing_occupied_bricks: Option<usize>,
        display_current: Option<bool>,
    },
    CrossSectionStream {
        panel: ProductAutomationPanelId,
        timepoint: Option<u64>,
        priority: Option<ProductAutomationBrickPriority>,
        fairness_promoted: Option<bool>,
        active_panel_at_submission: Option<ProductAutomationPanelId>,
        min_queued_current_frame: Option<usize>,
        min_queued_prefetch: Option<usize>,
        min_requested: Option<usize>,
        min_completed: Option<usize>,
        min_visible_chunks: Option<usize>,
        max_stale: Option<usize>,
        max_failed: Option<usize>,
    },
    CrossSectionStreamsMatchActiveTimepoint {
        min_completed: Option<usize>,
        min_visible_chunks: Option<usize>,
        max_failed: Option<usize>,
    },
    CrossSectionPanelNonblank {
        panel: ProductAutomationPanelId,
        min_nonzero_rgb_pixels: Option<usize>,
    },
    CrossSectionPanelImagesDistinct {
        min_different_pixels: Option<usize>,
    },
    FourPanelImagesDistinct {
        min_different_pixels: Option<usize>,
    },
    CrossSectionRetired,
}

impl ProductAutomationAssertCondition {
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::NonblankFrame => "nonblank_frame",
            Self::NoRenderError => "no_render_error",
            Self::FrameFreshnessCurrent => "frame_freshness_current",
            Self::RuntimeIdle => "runtime_idle",
            Self::RenderMode { .. } => "render_mode",
            Self::ViewerLayout { .. } => "viewer_layout",
            Self::ActiveTimepoint { .. } => "active_timepoint",
            Self::ObservedTimepoints { .. } => "observed_timepoints",
            Self::Playback { .. } => "playback",
            Self::CrossSectionActivePanel { .. } => "cross_section_active_panel",
            Self::CrossSectionPanelSchedule { .. } => "cross_section_panel_schedule",
            Self::CrossSectionStream { .. } => "cross_section_stream",
            Self::CrossSectionStreamsMatchActiveTimepoint { .. } => {
                "cross_section_streams_match_active_timepoint"
            }
            Self::CrossSectionPanelNonblank { .. } => "cross_section_panel_nonblank",
            Self::CrossSectionPanelImagesDistinct { .. } => "cross_section_panel_images_distinct",
            Self::FourPanelImagesDistinct { .. } => "four_panel_images_distinct",
            Self::CrossSectionRetired => "cross_section_retired",
        }
    }

    pub(super) fn is_cross_section_condition(&self) -> bool {
        matches!(
            self,
            Self::CrossSectionActivePanel { .. }
                | Self::CrossSectionPanelSchedule { .. }
                | Self::CrossSectionStream { .. }
                | Self::CrossSectionStreamsMatchActiveTimepoint { .. }
                | Self::CrossSectionPanelNonblank { .. }
                | Self::CrossSectionPanelImagesDistinct { .. }
                | Self::FourPanelImagesDistinct { .. }
                | Self::CrossSectionRetired
        )
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationViewerLayout {
    #[serde(alias = "single_3d")]
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
    #[serde(rename = "3d", alias = "three_d")]
    ThreeD,
    Yz,
}

impl ProductAutomationPanelId {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Xy => "xy",
            Self::Xz => "xz",
            Self::ThreeD => "3d",
            Self::Yz => "yz",
        }
    }
}

impl From<ProductAutomationPanelId> for PanelId {
    fn from(value: ProductAutomationPanelId) -> Self {
        match value {
            ProductAutomationPanelId::Xy => Self::Xy,
            ProductAutomationPanelId::Xz => Self::Xz,
            ProductAutomationPanelId::ThreeD => Self::ThreeD,
            ProductAutomationPanelId::Yz => Self::Yz,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProductAutomationPanelHoverProbe {
    pub(super) x_fraction: f32,
    pub(super) y_fraction: f32,
    pub(super) expected_status: Option<ProductAutomationCrossSectionHoverStatus>,
    pub(super) expect_value: Option<bool>,
    pub(super) expected_generation_status: Option<ProductAutomationCrossSectionGenerationStatus>,
    pub(super) expected_display_current: Option<bool>,
    pub(super) expected_target_generation: Option<u64>,
    pub(super) expected_displayed_generation: Option<u64>,
    pub(super) expected_schedule_generation: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationBrickPriority {
    CurrentFrame,
    Prefetch,
    Warm,
}

impl From<ProductAutomationBrickPriority> for BrickRequestPriority {
    fn from(value: ProductAutomationBrickPriority) -> Self {
        match value {
            ProductAutomationBrickPriority::CurrentFrame => Self::CurrentFrame,
            ProductAutomationBrickPriority::Prefetch => Self::Prefetch,
            ProductAutomationBrickPriority::Warm => Self::Warm,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationCrossSectionHoverStatus {
    Value,
    Loading,
    Stale,
    Incomplete,
    Unavailable,
    InvalidNoData,
    Outside,
}

impl ProductAutomationCrossSectionHoverStatus {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Loading => "loading",
            Self::Stale => "stale",
            Self::Incomplete => "incomplete",
            Self::Unavailable => "unavailable",
            Self::InvalidNoData => "invalid_no_data",
            Self::Outside => "outside",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationCrossSectionGenerationStatus {
    CurrentDisplayed,
    CurrentUndisplayed,
    RetainedStale,
    Unavailable,
}

impl ProductAutomationCrossSectionGenerationStatus {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::CurrentDisplayed => "current_displayed",
            Self::CurrentUndisplayed => "current_undisplayed",
            Self::RetainedStale => "retained_stale",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationCrossSectionScheduleStatus {
    MissingViewport,
    Loading,
    Ready,
    Current,
    Coarse,
    Incomplete,
    BudgetLimited,
    Unavailable,
}

impl From<ProductAutomationCrossSectionScheduleStatus> for CrossSectionPanelScheduleStatus {
    fn from(value: ProductAutomationCrossSectionScheduleStatus) -> Self {
        match value {
            ProductAutomationCrossSectionScheduleStatus::MissingViewport => Self::MissingViewport,
            ProductAutomationCrossSectionScheduleStatus::Loading => Self::Loading,
            ProductAutomationCrossSectionScheduleStatus::Ready => Self::Ready,
            ProductAutomationCrossSectionScheduleStatus::Current => Self::Current,
            ProductAutomationCrossSectionScheduleStatus::Coarse => Self::Coarse,
            ProductAutomationCrossSectionScheduleStatus::Incomplete => Self::Incomplete,
            ProductAutomationCrossSectionScheduleStatus::BudgetLimited => Self::BudgetLimited,
            ProductAutomationCrossSectionScheduleStatus::Unavailable => Self::Unavailable,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProductAutomationRenderMode {
    Mip,
    Dvr,
    Iso,
    Isosurface,
}

impl ProductAutomationRenderMode {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Mip => "mip",
            Self::Dvr => "dvr",
            Self::Iso | Self::Isosurface => "iso",
        }
    }
}

impl From<ProductAutomationRenderMode> for RenderMode {
    fn from(value: ProductAutomationRenderMode) -> Self {
        match value {
            ProductAutomationRenderMode::Mip => Self::Mip,
            ProductAutomationRenderMode::Dvr => Self::Dvr,
            ProductAutomationRenderMode::Iso | ProductAutomationRenderMode::Isosurface => {
                Self::Isosurface
            }
        }
    }
}

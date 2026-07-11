//! Current render/presentation facts retained only until WP-09B.

use std::{collections::BTreeMap, sync::Arc};

use eframe::egui;
use mirante4d_data::SpatialBrickIndex;
use mirante4d_render_api::PresentationViewport;
use mirante4d_renderer::{
    FrameDiagnostics, FrameDiagnosticsF32, MipImageF32, MipImageU16, RenderViewport,
    gpu::{GpuDisplayFrame, GpuRenderer},
};

use crate::{
    ChannelFidelityStatus, CrossSectionPanelGpuDisplayFrame, CrossSectionRuntime,
    DisplayRefreshTiming, FrameFidelityStatus, GpuDisplayedFrameIdentity, LodScheduleState,
    RenderBackend, RenderedIntensityChannel, viewer_layout::PanelId,
};

/// Exact 24-field temporary owner frozen by the WP-07B entry.
pub(crate) struct CurrentRenderRuntime {
    pub(crate) presentation_viewport: PresentationViewport,
    pub(crate) render_viewport: RenderViewport,
    pub(crate) render_backend: RenderBackend,
    pub(crate) frame_fidelity: FrameFidelityStatus,
    pub(crate) channel_fidelity: Vec<ChannelFidelityStatus>,
    pub(crate) lod_schedule: LodScheduleState,
    pub(crate) lod_replan_pending: bool,
    pub(crate) playback_lod_downshift_active: bool,
    pub(crate) visible_brick_count: usize,
    pub(crate) visible_brick_plan_stride: u64,
    pub(crate) visible_brick_plan_error: Option<String>,
    pub(crate) diagnostics: FrameDiagnostics,
    pub(crate) diagnostics_f32: Option<FrameDiagnosticsF32>,
    pub(crate) visible_bricks: Vec<SpatialBrickIndex>,
    pub(crate) cross_section_runtime: CrossSectionRuntime,
    pub(crate) frame: MipImageU16,
    pub(crate) frame_f32: Option<MipImageF32>,
    pub(crate) rendered_channels: Vec<RenderedIntensityChannel>,
    pub(crate) texture: Option<egui::TextureHandle>,
    pub(crate) gpu_display_frame: Option<GpuDisplayFrame>,
    pub(crate) gpu_renderer: Option<Arc<GpuRenderer>>,
    pub(crate) gpu_display_frame_identity: Option<GpuDisplayedFrameIdentity>,
    pub(crate) last_display_refresh_timing: Option<DisplayRefreshTiming>,
    pub(crate) cross_section_gpu_display_frames:
        BTreeMap<PanelId, CrossSectionPanelGpuDisplayFrame>,
}

impl CurrentRenderRuntime {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn opened(
        presentation_viewport: PresentationViewport,
        render_viewport: RenderViewport,
        frame_fidelity: FrameFidelityStatus,
        lod_schedule: LodScheduleState,
        diagnostics: FrameDiagnostics,
        diagnostics_f32: Option<FrameDiagnosticsF32>,
        cross_section_runtime: CrossSectionRuntime,
        frame: MipImageU16,
        frame_f32: Option<MipImageF32>,
        rendered_channels: Vec<RenderedIntensityChannel>,
    ) -> Self {
        Self {
            presentation_viewport,
            render_viewport,
            render_backend: RenderBackend::CpuReference,
            frame_fidelity,
            channel_fidelity: Vec::new(),
            lod_schedule,
            lod_replan_pending: false,
            playback_lod_downshift_active: false,
            visible_brick_count: 0,
            visible_brick_plan_stride: 1,
            visible_brick_plan_error: None,
            diagnostics,
            diagnostics_f32,
            visible_bricks: Vec::new(),
            cross_section_runtime,
            frame,
            frame_f32,
            rendered_channels,
            texture: None,
            gpu_display_frame: None,
            gpu_renderer: None,
            gpu_display_frame_identity: None,
            last_display_refresh_timing: None,
            cross_section_gpu_display_frames: BTreeMap::new(),
        }
    }
}

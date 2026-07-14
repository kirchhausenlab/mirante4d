//! Current render/presentation facts retained only until WP-09B.

use std::{collections::BTreeMap, sync::Arc};

use eframe::egui;
use mirante4d_render_api::{
    FrameIdentity, PresentationToken, PresentationViewport, PresentedFrame, RenderExtent,
};
use mirante4d_render_wgpu::WgpuRenderRuntime;
use mirante4d_renderer::{
    FrameDiagnostics, MipImageF32, MipImageU16, RenderViewport,
    gpu::{GpuDisplayFrame, GpuRenderer},
};

use crate::{
    ChannelFidelityStatus, CrossSectionPanelGpuDisplayFrame, CrossSectionRuntime,
    DisplayRefreshTiming, FrameFidelityStatus, GpuDisplayedFrameIdentity, LodScheduleState,
    RenderBackend, product_render_intent::ProductRenderRequest, retained_leases::RetainedLeases,
    viewer_layout::PanelId,
};

pub(crate) struct ProductPresentationTarget {
    pub(crate) token: PresentationToken,
    pub(crate) extent: RenderExtent,
    pub(crate) request: Option<ProductRenderRequest>,
    pub(crate) presented: Option<PresentedFrame>,
    pub(crate) texture_id: Option<egui::TextureId>,
}

pub(crate) struct ProductGpuRenderRuntime {
    pub(crate) renderer: WgpuRenderRuntime,
    pub(crate) targets: BTreeMap<PanelId, ProductPresentationTarget>,
    next_frame_identity: u64,
    pub(crate) current_partial_frames_presented: u64,
    pub(crate) partial_to_settled_transitions: u64,
    pub(crate) stale_frames_rejected: u64,
}

impl ProductGpuRenderRuntime {
    pub(crate) fn new(renderer: WgpuRenderRuntime) -> Self {
        Self {
            renderer,
            targets: BTreeMap::new(),
            next_frame_identity: 1,
            current_partial_frames_presented: 0,
            partial_to_settled_transitions: 0,
            stale_frames_rejected: 0,
        }
    }

    pub(crate) fn allocate_frame_identity(&mut self) -> FrameIdentity {
        let frame = FrameIdentity::new(self.next_frame_identity);
        self.next_frame_identity = self.next_frame_identity.saturating_add(1);
        frame
    }
}

/// Temporary render/presentation owner scheduled for replacement in WP-09B.
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
    pub(crate) visible_brick_plan_error: Option<String>,
    pub(crate) diagnostics: FrameDiagnostics,
    pub(crate) cross_section_runtime: CrossSectionRuntime,
    pub(crate) frame: MipImageU16,
    pub(crate) frame_f32: Option<MipImageF32>,
    pub(crate) texture: Option<egui::TextureHandle>,
    pub(crate) gpu_display_frame: Option<GpuDisplayFrame>,
    pub(crate) gpu_renderer: Option<Arc<GpuRenderer>>,
    pub(crate) gpu_display_frame_identity: Option<GpuDisplayedFrameIdentity>,
    pub(crate) last_display_refresh_timing: Option<DisplayRefreshTiming>,
    pub(crate) cross_section_gpu_display_frames:
        BTreeMap<PanelId, CrossSectionPanelGpuDisplayFrame>,
    pub(crate) retained_leases: RetainedLeases,
    pub(crate) product_gpu: Option<ProductGpuRenderRuntime>,
}

impl CurrentRenderRuntime {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn opened(
        presentation_viewport: PresentationViewport,
        render_viewport: RenderViewport,
        frame_fidelity: FrameFidelityStatus,
        lod_schedule: LodScheduleState,
        diagnostics: FrameDiagnostics,
        cross_section_runtime: CrossSectionRuntime,
        frame: MipImageU16,
        frame_f32: Option<MipImageF32>,
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
            visible_brick_plan_error: None,
            diagnostics,
            cross_section_runtime,
            frame,
            frame_f32,
            texture: None,
            gpu_display_frame: None,
            gpu_renderer: None,
            gpu_display_frame_identity: None,
            last_display_refresh_timing: None,
            cross_section_gpu_display_frames: BTreeMap::new(),
            retained_leases: RetainedLeases::new(),
            product_gpu: None,
        }
    }
}

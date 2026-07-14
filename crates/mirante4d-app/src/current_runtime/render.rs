//! Application-side state for successor GPU presentation.

use std::collections::BTreeMap;

use mirante4d_render_api::{
    FrameIdentity, PresentationToken, PresentationViewport, PresentedFrame, RenderExtent,
};
use mirante4d_render_wgpu::{ValidationCapture, ValidationCaptureTicket, WgpuRenderRuntime};

use crate::{
    CrossSectionRuntime, DisplayRefreshTiming, FrameFidelityStatus, LodScheduleState,
    product_render_intent::ProductRenderRequest, retained_leases::RetainedLeases,
    viewer_layout::PanelId,
};

pub(crate) struct ProductPresentationTarget {
    pub(crate) token: PresentationToken,
    pub(crate) extent: RenderExtent,
    pub(crate) request: Option<ProductRenderRequest>,
    pub(crate) presented: Option<PresentedFrame>,
    pub(crate) pending_capture: Option<(PresentedFrame, ValidationCaptureTicket)>,
    pub(crate) completed_capture: Option<(PresentedFrame, ValidationCapture)>,
    pub(crate) partial_seen: bool,
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

pub(crate) struct CurrentRenderRuntime {
    pub(crate) presentation_viewport: PresentationViewport,
    pub(crate) render_viewport: RenderExtent,
    pub(crate) frame_fidelity: FrameFidelityStatus,
    pub(crate) lod_schedule: LodScheduleState,
    pub(crate) lod_replan_pending: bool,
    pub(crate) playback_lod_downshift_active: bool,
    pub(crate) cross_section_runtime: CrossSectionRuntime,
    pub(crate) last_display_refresh_timing: Option<DisplayRefreshTiming>,
    pub(crate) retained_leases: RetainedLeases,
    pub(crate) product_gpu: Option<ProductGpuRenderRuntime>,
}

impl CurrentRenderRuntime {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn opened(
        presentation_viewport: PresentationViewport,
        render_viewport: RenderExtent,
        frame_fidelity: FrameFidelityStatus,
        lod_schedule: LodScheduleState,
        cross_section_runtime: CrossSectionRuntime,
    ) -> Self {
        Self {
            presentation_viewport,
            render_viewport,
            frame_fidelity,
            lod_schedule,
            lod_replan_pending: false,
            playback_lod_downshift_active: false,
            cross_section_runtime,
            last_display_refresh_timing: None,
            retained_leases: RetainedLeases::new(),
            product_gpu: None,
        }
    }
}

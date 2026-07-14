//! Application-side state for successor render coordination.

use mirante4d_application::RenderCoordinationState;
use mirante4d_render_api::{PresentationViewport, RenderExtent};

use crate::{DisplayRefreshTiming, FrameFidelityStatus};

pub(crate) struct CurrentRenderRuntime {
    pub(crate) presentation_viewport: PresentationViewport,
    pub(crate) render_viewport: RenderExtent,
    pub(crate) frame_fidelity: FrameFidelityStatus,
    pub(crate) lod_replan_pending: bool,
    pub(crate) render_coordination: RenderCoordinationState,
    pub(crate) last_display_refresh_timing: Option<DisplayRefreshTiming>,
}

impl CurrentRenderRuntime {
    pub(crate) fn opened(
        presentation_viewport: PresentationViewport,
        render_viewport: RenderExtent,
        frame_fidelity: FrameFidelityStatus,
        render_coordination: RenderCoordinationState,
    ) -> Self {
        Self {
            presentation_viewport,
            render_viewport,
            frame_fidelity,
            lod_replan_pending: false,
            render_coordination,
            last_display_refresh_timing: None,
        }
    }
}

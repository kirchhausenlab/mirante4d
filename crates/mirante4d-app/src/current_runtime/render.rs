//! Application-side state for successor render coordination.

use mirante4d_render_api::{PresentationViewport, RenderExtent};

use crate::{
    CrossSectionRuntime, DisplayRefreshTiming, FrameFidelityStatus, LodScheduleState,
    retained_leases::RetainedLeases,
};

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
        }
    }
}

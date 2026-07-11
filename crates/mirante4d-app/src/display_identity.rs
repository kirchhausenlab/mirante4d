use mirante4d_core::{CameraView, PresentationViewport, TimeIndex};
use mirante4d_renderer::RenderViewport;

use crate::{
    AppState, DisplayedFrameFreshness, RenderMode,
    display_graph::{DisplayChannelModeIdentity, DisplayGraph},
};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GpuDisplayedFrameIdentity {
    pub(crate) mode: RenderMode,
    pub(crate) channel_modes: Vec<DisplayChannelModeIdentity>,
    pub(crate) viewport: RenderViewport,
    pub(crate) presentation_viewport: PresentationViewport,
    pub(crate) camera: CameraView,
    pub(crate) timepoint: TimeIndex,
    pub(crate) displayed_scale_level: Option<u32>,
    pub(crate) brick_stream_generation: u64,
    pub(crate) layer_ids: Vec<String>,
}

impl GpuDisplayedFrameIdentity {
    pub(crate) fn from_state(state: &AppState) -> Self {
        let graph = DisplayGraph::from_state(state);
        let layer_ids = graph
            .channels
            .iter()
            .map(|channel| channel.layer_id.clone())
            .collect();
        Self {
            mode: state.active_render_mode,
            channel_modes: graph.mode_identities(),
            viewport: state.render_viewport,
            presentation_viewport: state.presentation_viewport,
            camera: state.camera,
            timepoint: state.active_timepoint,
            displayed_scale_level: state.frame_fidelity.displayed_scale_level,
            brick_stream_generation: state.brick_stream_generation,
            layer_ids,
        }
    }

    pub(crate) fn compatible_with_pending_request(&self, requested: &Self) -> bool {
        self.mode == requested.mode
            && self.channel_modes == requested.channel_modes
            && self.viewport == requested.viewport
            && self.presentation_viewport == requested.presentation_viewport
            && self.camera == requested.camera
            && self.timepoint == requested.timepoint
            && self.displayed_scale_level == requested.displayed_scale_level
            && self.layer_ids == requested.layer_ids
    }

    pub(crate) fn display_freshness_for_camera(
        &self,
        requested_camera: CameraView,
        requested_presentation_viewport: PresentationViewport,
    ) -> DisplayedFrameFreshness {
        if self.camera == requested_camera
            && self.presentation_viewport == requested_presentation_viewport
        {
            DisplayedFrameFreshness::Current
        } else {
            DisplayedFrameFreshness::Stale
        }
    }

    pub(crate) fn display_freshness_for_state(&self, state: &AppState) -> DisplayedFrameFreshness {
        let requested = Self::from_state(state);
        if self.mode != requested.mode
            || self.channel_modes != requested.channel_modes
            || self.viewport != requested.viewport
            || self.presentation_viewport != requested.presentation_viewport
            || self.timepoint != requested.timepoint
            || self.displayed_scale_level != requested.displayed_scale_level
            || self.layer_ids != requested.layer_ids
        {
            return DisplayedFrameFreshness::Stale;
        }
        self.display_freshness_for_camera(state.camera, state.presentation_viewport)
    }
}

use mirante4d_application::ApplicationSnapshot;
use mirante4d_dataset::DatasetResourceKey;
use mirante4d_domain::{CameraView, LogicalLayerKey, RenderMode, ScaleLevel, TimeIndex};
use mirante4d_render_api::PresentationViewport;
use mirante4d_renderer::RenderViewport;

use crate::{
    DisplayedFrameFreshness, application_view,
    current_runtime::render::CurrentRenderRuntime,
    dataset_requests::{DatasetDemandState, SCOPE_CURRENT_3D},
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
    pub(crate) displayed_scale: ScaleLevel,
    pub(crate) layers: Vec<LogicalLayerKey>,
    pub(crate) resources: Vec<DatasetResourceKey>,
}

impl GpuDisplayedFrameIdentity {
    pub(crate) fn from_snapshot(
        snapshot: &ApplicationSnapshot,
        dataset: &DatasetDemandState,
        render: &CurrentRenderRuntime,
    ) -> anyhow::Result<Self> {
        let view = application_view(snapshot);
        let graph = DisplayGraph::from_snapshot(snapshot);
        Ok(Self {
            mode: view
                .layer(view.active_layer())
                .expect("application view has an active layer")
                .render_state()
                .mode(),
            channel_modes: graph.mode_identities(),
            viewport: render.render_viewport,
            presentation_viewport: render.presentation_viewport,
            camera: *view.camera(),
            timepoint: view.timepoint(),
            displayed_scale: dataset.current_scale(),
            layers: graph.channels.iter().map(|channel| channel.layer).collect(),
            resources: dataset.scope_requirements(SCOPE_CURRENT_3D).to_vec(),
        })
    }

    pub(crate) fn compatible_with_pending_request(&self, requested: &Self) -> bool {
        self.mode == requested.mode
            && self.channel_modes == requested.channel_modes
            && self.viewport == requested.viewport
            && self.presentation_viewport == requested.presentation_viewport
            && self.camera == requested.camera
            && self.timepoint == requested.timepoint
            && self.displayed_scale == requested.displayed_scale
            && self.layers == requested.layers
            && self.resources == requested.resources
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

    pub(crate) fn display_freshness_for_snapshot(
        &self,
        snapshot: &ApplicationSnapshot,
        dataset: &DatasetDemandState,
        render: &CurrentRenderRuntime,
    ) -> anyhow::Result<DisplayedFrameFreshness> {
        let view = application_view(snapshot);
        let requested = Self::from_snapshot(snapshot, dataset, render)?;
        if self.mode != requested.mode
            || self.channel_modes != requested.channel_modes
            || self.viewport != requested.viewport
            || self.presentation_viewport != requested.presentation_viewport
            || self.timepoint != requested.timepoint
            || self.displayed_scale != requested.displayed_scale
            || self.layers != requested.layers
            || self.resources != requested.resources
        {
            return Ok(DisplayedFrameFreshness::Stale);
        }
        Ok(self.display_freshness_for_camera(*view.camera(), render.presentation_viewport))
    }
}

//! Product-to-renderer intent translation.

use glam::DQuat;
use mirante4d_application::{ApplicationSnapshot, WorkspaceSnapshot};
use mirante4d_domain::{CrossSectionView, UnitQuaternion};
use mirante4d_project_model::ViewState;
use mirante4d_render_api::{
    FrameIdentity, LayerRenderIntent, MAX_RENDER_REQUIREMENTS, PresentationViewport, RenderExtent,
    RenderIntent, RenderRequirements, RenderViewIntent,
};

use crate::viewer_layout::PanelId;

/// Count ceiling shared with the renderer's bounded metadata envelope.
/// Dataset demand is additionally constrained by its decoded-byte ledger, so
/// raising this removes the hidden LOD fallback without forcing 65,536 payloads
/// into CPU or GPU residency on smaller configurations.
pub(crate) const PRODUCT_RENDER_RESOURCE_LIMIT: usize = MAX_RENDER_REQUIREMENTS;

#[derive(PartialEq)]
pub(crate) struct ProductRenderRequest {
    pub(crate) intent: RenderIntent,
    pub(crate) requirements: RenderRequirements,
}

impl ProductRenderRequest {
    pub(crate) fn rebind(&self, intent: RenderIntent) -> anyhow::Result<Self> {
        let requirements = self.requirements.rebind(&intent)?;
        Ok(Self {
            intent,
            requirements,
        })
    }
}

pub(crate) fn volume_intent(
    snapshot: &ApplicationSnapshot,
    frame: FrameIdentity,
    presentation: PresentationViewport,
    extent: RenderExtent,
) -> anyhow::Result<Option<RenderIntent>> {
    build_intent(
        snapshot,
        frame,
        RenderViewIntent::volume(
            *application_view(snapshot).camera(),
            *application_view(snapshot).iso_light(),
        ),
        presentation,
        extent,
    )
}

pub(crate) fn cross_section_intent(
    snapshot: &ApplicationSnapshot,
    frame: FrameIdentity,
    panel: PanelId,
    presentation: PresentationViewport,
    extent: RenderExtent,
) -> anyhow::Result<Option<RenderIntent>> {
    let Some(relative) = panel_relative_orientation(panel) else {
        anyhow::bail!("the 3D panel is not a cross-section target");
    };
    let source = *application_view(snapshot).cross_section();
    let orientation = DQuat::from_array(source.orientation().xyzw()) * relative;
    let [x, y, z, w] = orientation.to_array();
    let view = CrossSectionView::new(
        source.center_world(),
        UnitQuaternion::new_xyzw(x, y, z, w)?,
        source.scale_world_per_screen_point(),
        source.depth_world(),
    )?;
    build_intent(
        snapshot,
        frame,
        RenderViewIntent::cross_section(view),
        presentation,
        extent,
    )
}

fn build_intent(
    snapshot: &ApplicationSnapshot,
    frame: FrameIdentity,
    view_intent: RenderViewIntent,
    presentation: PresentationViewport,
    extent: RenderExtent,
) -> anyhow::Result<Option<RenderIntent>> {
    let view = application_view(snapshot);
    let layers = view
        .layers()
        .iter()
        .filter(|layer| layer.visible())
        .map(|layer| {
            LayerRenderIntent::new(
                layer.layer_key(),
                layer.transfer().clone(),
                *layer.render_state(),
            )
        })
        .collect::<Vec<_>>();
    if layers.is_empty() {
        return Ok(None);
    }
    let intent = RenderIntent::new(
        frame,
        snapshot.catalog().resource_identity(),
        view.timepoint(),
        view_intent,
        presentation,
        extent,
        layers,
    )?;
    Ok(Some(intent))
}

fn panel_relative_orientation(panel: PanelId) -> Option<DQuat> {
    match panel {
        PanelId::Xy => Some(DQuat::IDENTITY),
        PanelId::Xz => Some(DQuat::from_rotation_x(std::f64::consts::FRAC_PI_2)),
        PanelId::Yz => Some(DQuat::from_rotation_y(std::f64::consts::FRAC_PI_2)),
        PanelId::ThreeD => None,
    }
}

fn application_view(snapshot: &ApplicationSnapshot) -> &ViewState {
    match snapshot.workspace() {
        WorkspaceSnapshot::Unbound { workspace } => workspace.view(),
        WorkspaceSnapshot::Bound { project, .. } => project.view(),
    }
}

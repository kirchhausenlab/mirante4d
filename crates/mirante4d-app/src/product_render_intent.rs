//! Product-to-renderer intent translation.

use glam::DQuat;
use mirante4d_application::{ApplicationSnapshot, WorkspaceSnapshot};
use mirante4d_dataset::DatasetResourceKey;
use mirante4d_domain::{
    CrossSectionView, IsoShadingPolicy, RenderState, SamplingPolicy, UnitQuaternion,
};
use mirante4d_project_model::ViewState;
use mirante4d_render_api::{
    FrameIdentity, LayerRenderIntent, PresentationViewport, RenderExtent, RenderIntent,
    RenderRequirement, RenderRequirementRole, RenderRequirements, RenderViewIntent,
};

use crate::viewer_layout::PanelId;

/// The product deliberately stays inside the renderer's single-call lease
/// window. Aggregate dataset demand uses the same bound.
pub(crate) const PRODUCT_RENDER_RESOURCE_LIMIT: usize = 128;

#[derive(Clone, PartialEq)]
pub(crate) struct ProductRenderRequest {
    pub(crate) intent: RenderIntent,
    pub(crate) requirements: RenderRequirements,
}

pub(crate) fn volume_request(
    snapshot: &ApplicationSnapshot,
    frame: FrameIdentity,
    presentation: PresentationViewport,
    extent: RenderExtent,
    resources: &[DatasetResourceKey],
) -> anyhow::Result<Option<ProductRenderRequest>> {
    build_request(
        snapshot,
        frame,
        RenderViewIntent::volume(
            *application_view(snapshot).camera(),
            *application_view(snapshot).iso_light(),
        ),
        presentation,
        extent,
        resources,
    )
}

pub(crate) fn cross_section_request(
    snapshot: &ApplicationSnapshot,
    frame: FrameIdentity,
    panel: PanelId,
    presentation: PresentationViewport,
    extent: RenderExtent,
    resources: &[DatasetResourceKey],
) -> anyhow::Result<Option<ProductRenderRequest>> {
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
    build_request(
        snapshot,
        frame,
        RenderViewIntent::cross_section(view),
        presentation,
        extent,
        resources,
    )
}

fn build_request(
    snapshot: &ApplicationSnapshot,
    frame: FrameIdentity,
    view_intent: RenderViewIntent,
    presentation: PresentationViewport,
    extent: RenderExtent,
    resources: &[DatasetResourceKey],
) -> anyhow::Result<Option<ProductRenderRequest>> {
    if resources.is_empty() {
        return Ok(None);
    }
    if resources.len() > PRODUCT_RENDER_RESOURCE_LIMIT {
        anyhow::bail!(
            "product render request contains {} resources, exceeding the bounded limit of {}",
            resources.len(),
            PRODUCT_RENDER_RESOURCE_LIMIT
        );
    }
    let view = application_view(snapshot);
    let layers = view
        .layers()
        .iter()
        .filter(|layer| layer.visible())
        .map(|layer| {
            Ok(LayerRenderIntent::new(
                layer.layer_key(),
                layer.transfer().clone(),
                supported_render_state(*layer.render_state())?,
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    if layers.is_empty() {
        return Ok(None);
    }
    let intent = RenderIntent::new(
        frame,
        snapshot.catalog().scientific_identity().resource_identity(),
        view.timepoint(),
        view_intent,
        presentation,
        extent,
        layers,
    )?;
    let requirements = resources
        .iter()
        .copied()
        .enumerate()
        .map(|(index, key)| {
            RenderRequirement::new(
                key,
                if index == 0 {
                    RenderRequirementRole::FirstUsefulFrame
                } else {
                    RenderRequirementRole::Refinement
                },
            )
        })
        .collect();
    let requirements = RenderRequirements::new(&intent, requirements)?;
    Ok(Some(ProductRenderRequest {
        intent,
        requirements,
    }))
}

fn supported_render_state(state: RenderState) -> anyhow::Result<RenderState> {
    if state.sampling_policy() != SamplingPolicy::VoxelExact {
        anyhow::bail!("the product renderer supports only voxel-exact sampling");
    }
    if state
        .iso_parameters()
        .is_some_and(|parameters| parameters.shading_policy() != IsoShadingPolicy::Flat)
    {
        anyhow::bail!("the product renderer supports only flat ISO shading");
    }
    Ok(state)
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

#[cfg(test)]
mod tests {
    use mirante4d_domain::{
        DisplayWindow, DvrOpacityTransfer, IsoShadingPolicy, RenderState, SamplingPolicy,
        TransferCurve,
    };

    use super::supported_render_state;

    #[test]
    fn product_modes_preserve_supported_state_and_reject_unsupported_quality() {
        let mip = supported_render_state(RenderState::mip(SamplingPolicy::VoxelExact)).unwrap();
        assert_eq!(mip.sampling_policy(), SamplingPolicy::VoxelExact);

        let dvr = RenderState::dvr(
            SamplingPolicy::VoxelExact,
            DvrOpacityTransfer::new(
                DisplayWindow::new(0.0, 1.0).unwrap(),
                TransferCurve::linear(),
            ),
            2.0,
        )
        .unwrap();
        let dvr = supported_render_state(dvr).unwrap();
        assert_eq!(dvr.sampling_policy(), SamplingPolicy::VoxelExact);
        assert_eq!(dvr.dvr_parameters().unwrap().density_scale(), 2.0);

        let iso =
            RenderState::iso(SamplingPolicy::VoxelExact, IsoShadingPolicy::Flat, 0.4).unwrap();
        let iso = supported_render_state(iso).unwrap();
        assert_eq!(iso.sampling_policy(), SamplingPolicy::VoxelExact);
        assert_eq!(
            iso.iso_parameters().unwrap().shading_policy(),
            IsoShadingPolicy::Flat
        );
        assert_eq!(iso.iso_parameters().unwrap().display_level(), 0.4);

        assert!(supported_render_state(RenderState::mip(SamplingPolicy::SmoothLinear)).is_err());
        let unsupported_iso = RenderState::iso(
            SamplingPolicy::VoxelExact,
            IsoShadingPolicy::GradientLighting,
            0.4,
        )
        .unwrap();
        assert!(supported_render_state(unsupported_iso).is_err());
    }
}

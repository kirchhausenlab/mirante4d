use mirante4d_application::ApplicationSnapshot;
use mirante4d_domain::ViewerLayout;
use mirante4d_project_model::ViewState;

use crate::{
    RenderCoordinationState, analysis_session::AnalysisProductRuntime, application_view,
    dataset_requests::DatasetDemandState,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ViewRuntimeChange {
    pub(crate) playback_priority_changed: bool,
    pub(crate) timepoint_changed: bool,
}

/// Reconciles payload-free presentation state after a canonical view change.
/// Unified demand planning performs the actual scoped cancellation and lease
/// requirement replacement immediately after this function returns.
pub(crate) fn reconcile_view_runtime(
    previous_view: &ViewState,
    snapshot: &ApplicationSnapshot,
    dataset: &mut DatasetDemandState,
    render: &mut RenderCoordinationState,
    analysis: &mut AnalysisProductRuntime,
) -> anyhow::Result<ViewRuntimeChange> {
    let view = application_view(snapshot);
    if previous_view == view {
        return Ok(ViewRuntimeChange::default());
    }

    let analysis_binding_changed = previous_view.active_layer() != view.active_layer();
    // Ordinary rendering is visible-set authoritative. Playback is outside
    // that static cut and retains its established active-first quality
    // priority, so an analysis-focus change remains a playback material
    // change only while a temporal session is active.
    let playback_priority_changed =
        snapshot.transient().playback_active() && analysis_binding_changed;
    let timepoint_changed = previous_view.timepoint() != view.timepoint();
    if analysis_binding_changed || timepoint_changed {
        let layer = snapshot
            .catalog()
            .layer(view.active_layer())
            .ok_or_else(|| anyhow::anyhow!("active logical layer is absent from the catalog"))?;
        if view.timepoint().get() >= layer.shape().t() {
            anyhow::bail!(
                "timepoint {} is out of range for active layer {} with {} timepoint(s)",
                view.timepoint().get(),
                layer.label(),
                layer.shape().t()
            );
        }
        if playback_priority_changed {
            drop(dataset.take_retained_leases_for_playback_rewarm());
        }
        if (playback_priority_changed || timepoint_changed)
            && (previous_view.layout() == ViewerLayout::FourPanel
                || view.layout() == ViewerLayout::FourPanel)
        {
            render.invalidate_cross_sections();
        }
        if analysis_binding_changed {
            analysis.set_roi([0; 3], layer.shape().spatial().dimensions())?;
        }
    }

    if playback_priority_changed || volume_render_changed(previous_view, view) {
        render.mark_3d_display_stale();
    }
    Ok(ViewRuntimeChange {
        playback_priority_changed,
        timepoint_changed,
    })
}

/// Canonical fields consumed by the 3D render and demand pipeline.
/// Cross-section authority is deliberately excluded: linked-slice motion must
/// not invalidate an otherwise reusable volume frame.
pub(crate) fn volume_render_changed(previous: &ViewState, next: &ViewState) -> bool {
    previous.layers() != next.layers()
        || previous.timepoint() != next.timepoint()
        || previous.camera() != next.camera()
        || previous.layout() != next.layout()
        || previous.iso_light() != next.iso_light()
}

/// Canonical fields consumed by the linked-panel render and demand pipeline.
/// Camera and isosurface-light motion are 3D-only and must leave linked panels
/// reusable.
pub(crate) fn cross_section_render_changed(previous: &ViewState, next: &ViewState) -> bool {
    previous.layers() != next.layers()
        || previous.timepoint() != next.timepoint()
        || previous.layout() != next.layout()
        || previous.cross_section() != next.cross_section()
}

#[cfg(test)]
mod tests {
    use mirante4d_domain::{
        CameraView, CrossSectionView, DisplayWindow, IsoLightState, LayerTransfer, LogicalLayerKey,
        Opacity, Projection, RenderState, RgbColor, SamplingPolicy, TimeIndex, TransferCurve,
        UnitQuaternion, ViewerLayout, WorldPoint3,
    };
    use mirante4d_project_model::{LayerViewState, ViewState};

    use super::{cross_section_render_changed, volume_render_changed};

    fn two_layer_view(active: LogicalLayerKey) -> ViewState {
        let transfer = LayerTransfer::new(
            DisplayWindow::new(0.0, 1.0).unwrap(),
            RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
            Opacity::new(1.0).unwrap(),
            TransferCurve::linear(),
            false,
        );
        let layers = [LogicalLayerKey::new(0), LogicalLayerKey::new(1)]
            .into_iter()
            .map(|layer| {
                LayerViewState::new(
                    layer,
                    true,
                    transfer.clone(),
                    RenderState::mip(SamplingPolicy::VoxelExact),
                )
            })
            .collect();
        ViewState::new(
            layers,
            active,
            TimeIndex::new(0),
            CameraView::new(
                Projection::Orthographic,
                WorldPoint3::origin(),
                UnitQuaternion::identity(),
                1.0,
                320.0,
                10.0,
            )
            .unwrap(),
            ViewerLayout::FourPanel,
            CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 1.0, 1.0)
                .unwrap(),
            IsoLightState::attached_camera(),
        )
        .unwrap()
    }

    #[test]
    fn analysis_only_active_change_invalidates_no_render_family() {
        let previous = two_layer_view(LogicalLayerKey::new(0));
        let next = two_layer_view(LogicalLayerKey::new(1));

        assert!(!volume_render_changed(&previous, &next));
        assert!(!cross_section_render_changed(&previous, &next));
    }
}

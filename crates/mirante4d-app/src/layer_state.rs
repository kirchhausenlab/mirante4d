use mirante4d_application::ApplicationSnapshot;
use mirante4d_domain::ViewerLayout;
use mirante4d_project_model::ViewState;

use crate::{
    RenderCoordinationState, analysis_session::AnalysisProductRuntime, application_view,
    dataset_requests::DatasetDemandState,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ViewRuntimeChange {
    pub(crate) dataset_binding_changed: bool,
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

    let dataset_binding_changed = previous_view.active_layer() != view.active_layer();
    let timepoint_changed = previous_view.timepoint() != view.timepoint();
    if dataset_binding_changed || timepoint_changed {
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
        if dataset_binding_changed {
            drop(dataset.take_retained_leases());
        }
        if previous_view.layout() == ViewerLayout::FourPanel
            || view.layout() == ViewerLayout::FourPanel
        {
            render.invalidate_cross_sections();
        }
        if dataset_binding_changed {
            analysis.set_roi([0; 3], layer.shape().spatial().dimensions())?;
        }
    }

    if volume_render_changed(previous_view, view) {
        render.mark_3d_display_stale();
    }
    Ok(ViewRuntimeChange {
        dataset_binding_changed,
        timepoint_changed,
    })
}

/// Canonical fields consumed by the 3D render and demand pipeline.
/// Cross-section authority is deliberately excluded: linked-slice motion must
/// not invalidate an otherwise reusable volume frame.
pub(crate) fn volume_render_changed(previous: &ViewState, next: &ViewState) -> bool {
    previous.layers() != next.layers()
        || previous.active_layer() != next.active_layer()
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
        || previous.active_layer() != next.active_layer()
        || previous.timepoint() != next.timepoint()
        || previous.layout() != next.layout()
        || previous.cross_section() != next.cross_section()
}

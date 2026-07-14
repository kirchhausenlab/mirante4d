use mirante4d_application::ApplicationSnapshot;
use mirante4d_project_model::ViewState;

use crate::{
    DisplayedFrameFreshness, FrameCompleteness, LodDecisionReason, application_view,
    current_runtime::{analysis::AnalysisProductRuntime, render::CurrentRenderRuntime},
    dataset_requests::DatasetDemandState,
};

/// Reconciles payload-free presentation state after a canonical view change.
/// Unified demand planning performs the actual scoped cancellation and lease
/// requirement replacement immediately after this function returns.
pub(crate) fn reconcile_view_runtime(
    previous_view: &ViewState,
    snapshot: &ApplicationSnapshot,
    dataset: &mut DatasetDemandState,
    render: &mut CurrentRenderRuntime,
    analysis: &mut AnalysisProductRuntime,
) -> anyhow::Result<bool> {
    let view = application_view(snapshot);
    if previous_view == view {
        return Ok(false);
    }

    let source_selection_changed = previous_view.active_layer() != view.active_layer()
        || previous_view.timepoint() != view.timepoint();
    if source_selection_changed {
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
        drop(dataset.take_retained_leases());
        render
            .cross_section_runtime
            .mark_cross_section_panels_dirty();
        analysis.set_roi([0; 3], layer.shape().spatial().dimensions())?;
    }

    mark_preserved_frame_stale(render);
    Ok(source_selection_changed)
}

fn mark_preserved_frame_stale(render: &mut CurrentRenderRuntime) {
    render.frame_fidelity.display_freshness = DisplayedFrameFreshness::Stale;
    render.frame_fidelity.completeness = FrameCompleteness::Loading;
    render.frame_fidelity.reason = LodDecisionReason::LoadingTargetScale;
    render.frame_fidelity.frame_time_ms = None;
    render.frame_fidelity.last_failure_kind = None;
    render.frame_fidelity.last_capacity_error = None;
    render.lod_replan_pending = true;
}

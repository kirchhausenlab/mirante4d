use mirante4d_application::ApplicationSnapshot;
use mirante4d_project_model::ViewState;

use crate::{
    DisplayedFrameFreshness, FrameCompleteness, LodDecisionReason, application_view,
    current_runtime::{analysis::CurrentAnalysisRuntime, render::CurrentRenderRuntime},
    dataset_requests::DatasetDemandState,
    render_state::metadata_intensity_summary,
};

/// Reconciles payload-free presentation state after a canonical view change.
/// Unified demand planning performs the actual scoped cancellation and lease
/// requirement replacement immediately after this function returns.
pub(crate) fn reconcile_view_runtime(
    previous_view: &ViewState,
    snapshot: &ApplicationSnapshot,
    _dataset: &mut DatasetDemandState,
    render: &mut CurrentRenderRuntime,
    analysis: &mut CurrentAnalysisRuntime,
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
        render.lease_bridge.replace_current_requirements([])?;
        render
            .cross_section_runtime
            .mark_cross_section_panels_dirty();
        analysis.active_intensity_summary = metadata_intensity_summary(layer.shape().spatial())?;
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
    render.lod_schedule.pending_scale_level = None;
    render.lod_schedule.hard_failed_scale_level = None;
    render.lod_schedule.hard_failure_reason = None;
    render.lod_replan_pending = true;
}

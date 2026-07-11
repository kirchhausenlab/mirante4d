use mirante4d_application::ApplicationSnapshot;
use mirante4d_project_model::ViewState;

use crate::{
    DisplayedFrameFreshness, FrameCompleteness, LodDecisionReason,
    brick_streaming::{cancel_brick_tickets, reset_prefetch_state, view_for_snapshot},
    current_runtime::{
        analysis::CurrentAnalysisRuntime, dataset::CurrentDatasetRuntime,
        render::CurrentRenderRuntime,
    },
    lod_scheduler::update_visible_brick_plan,
    render_state::{metadata_intensity_summary, update_channel_fidelity_status},
};

/// Reconciles runtime payloads after the canonical application view changes.
///
/// The canonical view remains immutable here. A source-selection change
/// invalidates work and payload aliases tied to the previous active layer or
/// timepoint. The displayed frame itself remains intact and is explicitly
/// marked stale while a new bounded visible-brick plan is prepared.
///
/// Returns `true` only when the active logical layer or timepoint changed. In
/// that case the caller should submit the prepared visible-brick plan without
/// replacing the preserved frame first. Other view changes return `false` so
/// the caller may render immediately from still-current payloads.
pub(crate) fn reconcile_view_runtime(
    previous_view: &ViewState,
    snapshot: &ApplicationSnapshot,
    dataset: &mut CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    analysis: &mut CurrentAnalysisRuntime,
) -> anyhow::Result<bool> {
    let view = view_for_snapshot(snapshot);
    if previous_view == view {
        return Ok(false);
    }

    let source_selection_changed = previous_view.active_layer() != view.active_layer()
        || previous_view.timepoint() != view.timepoint();
    if source_selection_changed {
        prepare_changed_source_selection(snapshot, dataset, render, analysis)?;
    }

    mark_preserved_frame_stale(render);
    update_visible_brick_plan(snapshot, dataset, render);
    update_channel_fidelity_status(snapshot, dataset, render);
    Ok(source_selection_changed)
}

fn prepare_changed_source_selection(
    snapshot: &ApplicationSnapshot,
    dataset: &mut CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    analysis: &mut CurrentAnalysisRuntime,
) -> anyhow::Result<()> {
    let view = view_for_snapshot(snapshot);
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

    cancel_stale_payload_work(dataset);
    dataset.active_volume_u8 = None;
    dataset.active_volume = None;
    dataset.active_volume_f32 = None;
    dataset.resident_bricks_u8.clear();
    dataset.resident_bricks.clear();
    dataset.resident_bricks_f32.clear();
    dataset.brick_stream_scale_shape = layer.shape().spatial();
    dataset.cross_section_last_interaction_at = None;
    render
        .cross_section_runtime
        .mark_cross_section_panels_dirty();
    render.cross_section_runtime.clear_visible_work();

    let histogram_payload_changed = !analysis.resident_histogram_samples.is_empty();
    analysis.resident_histogram_samples.clear();
    if histogram_payload_changed {
        analysis.resident_histogram_generation =
            analysis.resident_histogram_generation.saturating_add(1);
    }
    analysis.active_histogram_cache = None;
    analysis.active_intensity_summary = metadata_intensity_summary(layer.shape().spatial())?;
    analysis.analysis_task = None;
    Ok(())
}

fn cancel_stale_payload_work(dataset: &mut CurrentDatasetRuntime) {
    let next_generation = dataset
        .brick_read_pool
        .as_ref()
        .map(|pool| pool.advance_generation().0)
        .unwrap_or_else(|| dataset.brick_stream_generation.saturating_add(1));
    if let Some(pool) = dataset.cross_section_read_pool.as_ref() {
        pool.advance_generation();
    }
    cancel_brick_tickets(&mut dataset.current_brick_tickets);
    cancel_brick_tickets(&mut dataset.prefetch_brick_tickets);
    cancel_brick_tickets(&mut dataset.warm_brick_tickets);

    dataset.brick_stream_generation = next_generation;
    dataset.brick_stream_requested = 0;
    dataset.brick_stream_completed = 0;
    dataset.brick_stream_cancelled = 0;
    dataset.brick_stream_stale = 0;
    dataset.brick_stream_failed = 0;
    dataset.brick_stream_last_error = None;
    dataset.brick_stream_complete = false;
    dataset.brick_stream_request_key = None;
    reset_prefetch_state(dataset);
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

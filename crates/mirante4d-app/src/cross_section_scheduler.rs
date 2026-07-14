//! Generation-scoped cross-section presentation scheduling.
//!
//! This scheduler observes semantic lease cohorts. It does not plan storage
//! chunks, own payloads, submit reads, or maintain a second residency model.

use std::time::Instant;

use mirante4d_dataset::{DatasetCatalog, DatasetResourceKey};
use mirante4d_domain::{LogicalLayerKey, ScaleLevel, ViewerLayout};
use mirante4d_project_model::ViewState;

use crate::{
    current_runtime::render::CurrentRenderRuntime,
    lod_scheduler::representative_voxel_world_size,
    render_state::ResidentRenderFailureStatus,
    retained_leases::RetainedLeases,
    viewer_layout::{
        CrossSectionPanelScheduleReason, CrossSectionPanelScheduleState,
        CrossSectionPanelScheduleStatus, PanelId,
    },
};

pub(crate) const CROSS_SECTION_PANEL_SCHEDULER_CPU_BUDGET_MS: f64 = 1.0;
pub(crate) const CROSS_SECTION_PANEL_RENDER_SUBMISSIONS_PER_PANEL_REFRESH: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CrossSectionPanelSchedulePlan {
    pub(crate) schedule: CrossSectionPanelScheduleState,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CrossSectionScheduleInput<'a> {
    pub(crate) catalog: &'a DatasetCatalog,
    pub(crate) view: &'a ViewState,
    pub(crate) active_layer: LogicalLayerKey,
    pub(crate) requirements: &'a [DatasetResourceKey],
    pub(crate) retained_leases: &'a RetainedLeases,
    pub(crate) render_scale: ScaleLevel,
    pub(crate) dataset_failed: bool,
}

pub(crate) fn schedule_cross_section_panel(
    render: &mut CurrentRenderRuntime,
    input: CrossSectionScheduleInput<'_>,
    panel_id: PanelId,
    gpu_display_available: bool,
) -> anyhow::Result<CrossSectionPanelSchedulePlan> {
    let schedule_start = Instant::now();
    let schedule =
        build_cross_section_panel_schedule(render, input, panel_id, gpu_display_available)?;
    let schedule_ms = schedule_start.elapsed().as_secs_f64() * 1000.0;
    if schedule_ms > CROSS_SECTION_PANEL_SCHEDULER_CPU_BUDGET_MS {
        tracing::debug!(
            panel = panel_id.label(),
            schedule_ms,
            budget_ms = CROSS_SECTION_PANEL_SCHEDULER_CPU_BUDGET_MS,
            "cross-section panel scheduling exceeded its per-panel CPU budget"
        );
    }
    render
        .cross_section_runtime
        .set_panel_schedule(panel_id, schedule);
    Ok(CrossSectionPanelSchedulePlan { schedule })
}

pub(crate) fn mark_cross_section_panel_rendered(
    render: &mut CurrentRenderRuntime,
    panel_id: PanelId,
    schedule: CrossSectionPanelScheduleState,
) {
    render
        .cross_section_runtime
        .set_panel_schedule(panel_id, schedule.rendered());
}

pub(crate) fn mark_cross_section_panel_render_failed(
    render: &mut CurrentRenderRuntime,
    panel_id: PanelId,
    mut schedule: CrossSectionPanelScheduleState,
    failure: ResidentRenderFailureStatus,
) {
    schedule.status = CrossSectionPanelScheduleStatus::Unavailable;
    schedule.reason = CrossSectionPanelScheduleReason::RenderFailed;
    render
        .cross_section_runtime
        .set_panel_schedule(panel_id, schedule);
    render
        .cross_section_runtime
        .mark_panel_render_failed(panel_id, schedule.generation, failure);
}

fn build_cross_section_panel_schedule(
    render: &CurrentRenderRuntime,
    input: CrossSectionScheduleInput<'_>,
    panel_id: PanelId,
    gpu_display_available: bool,
) -> anyhow::Result<CrossSectionPanelScheduleState> {
    if input.view.layout() != ViewerLayout::FourPanel || panel_id.cross_section_panel().is_none() {
        return Ok(CrossSectionPanelScheduleState {
            status: CrossSectionPanelScheduleStatus::Unavailable,
            reason: CrossSectionPanelScheduleReason::GpuUnavailable,
            ..CrossSectionPanelScheduleState::missing_viewport(0)
        });
    }

    let generation = render
        .cross_section_runtime
        .panel(panel_id)
        .map_or(0, |panel| panel.generation);
    let Some(panel_runtime) = render.cross_section_runtime.panel(panel_id) else {
        return Ok(CrossSectionPanelScheduleState::missing_viewport(generation));
    };
    if panel_runtime.presentation_viewport.is_none() || panel_runtime.render_viewport.is_none() {
        return Ok(CrossSectionPanelScheduleState::missing_viewport(generation));
    }

    let target_scale_level =
        cross_section_target_scale(input.catalog, input.view, input.active_layer)?;
    let render_scale_level = input.render_scale.get();
    let fallback_scale_level =
        (render_scale_level > target_scale_level).then_some(render_scale_level);
    let required = input.requirements.len();
    let retained = input
        .requirements
        .iter()
        .filter(|key| input.retained_leases.payload(**key).is_some())
        .count();
    let missing = required.saturating_sub(retained);

    let (status, reason) = classify_schedule(
        input.dataset_failed,
        gpu_display_available,
        required,
        missing,
        fallback_scale_level.is_some(),
    );

    Ok(CrossSectionPanelScheduleState {
        generation,
        target_scale_level: Some(target_scale_level),
        render_scale_level: Some(render_scale_level),
        fallback_scale_level,
        selected_bricks: required,
        occupied_selected_bricks: retained,
        missing_occupied_bricks: missing,
        estimated_decoded_bytes: 0,
        decoded_budget_bytes: 0,
        status,
        reason,
    })
}

fn classify_schedule(
    dataset_failed: bool,
    gpu_display_available: bool,
    required: usize,
    missing: usize,
    fallback: bool,
) -> (
    CrossSectionPanelScheduleStatus,
    CrossSectionPanelScheduleReason,
) {
    if dataset_failed {
        (
            CrossSectionPanelScheduleStatus::Unavailable,
            CrossSectionPanelScheduleReason::RenderFailed,
        )
    } else if !gpu_display_available {
        (
            CrossSectionPanelScheduleStatus::Unavailable,
            CrossSectionPanelScheduleReason::GpuUnavailable,
        )
    } else if required == 0 {
        (
            CrossSectionPanelScheduleStatus::Empty,
            CrossSectionPanelScheduleReason::NoSelectedData,
        )
    } else if missing > 0 {
        (
            CrossSectionPanelScheduleStatus::Loading,
            CrossSectionPanelScheduleReason::MissingSelectedBricks,
        )
    } else if fallback {
        (
            CrossSectionPanelScheduleStatus::Coarse,
            CrossSectionPanelScheduleReason::ResidentScaleCoarserThanTarget,
        )
    } else {
        (
            CrossSectionPanelScheduleStatus::Ready,
            CrossSectionPanelScheduleReason::TargetScaleReady,
        )
    }
}

pub(crate) fn cross_section_target_scale(
    catalog: &DatasetCatalog,
    view: &ViewState,
    layer_key: LogicalLayerKey,
) -> anyhow::Result<u32> {
    let layer = catalog
        .layer(layer_key)
        .ok_or_else(|| anyhow::anyhow!("active layer is absent from the dataset catalog"))?;
    let world_per_point = view
        .cross_section()
        .scale_world_per_screen_point()
        .max(f64::EPSILON);
    let mut selected = 0;
    for scale in layer.scales() {
        let scale_level = scale.level().get();
        let voxel_size = representative_voxel_world_size(scale.grid_to_world());
        if voxel_size <= world_per_point {
            selected = scale_level;
        }
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cross_section_demand_is_terminal_not_loading() {
        let (status, reason) = classify_schedule(false, true, 0, 0, false);
        assert_eq!(status, CrossSectionPanelScheduleStatus::Empty);
        assert_eq!(reason, CrossSectionPanelScheduleReason::NoSelectedData);
    }
}

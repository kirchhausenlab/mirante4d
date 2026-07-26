//! Generation-scoped cross-section presentation scheduling.
//!
//! This scheduler observes semantic lease cohorts. It does not plan storage
//! chunks, own payloads, submit reads, or maintain a second residency model.

use std::time::Instant;

use mirante4d_application::{
    CrossSectionPanelScheduleReason, CrossSectionPanelScheduleState,
    CrossSectionPanelScheduleStatus, RenderCoordinationState,
};
use mirante4d_dataset::DatasetResourceKey;
use mirante4d_domain::{ScaleLevel, ViewerLayout};
use mirante4d_project_model::ViewState;

use crate::{retained_leases::RetainedLeases, viewer_layout::PanelId};

pub(crate) const CROSS_SECTION_PANEL_SCHEDULER_CPU_BUDGET_MS: f64 = 1.0;
pub(crate) const CROSS_SECTION_PANEL_RENDER_SUBMISSIONS_PER_PANEL_REFRESH: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CrossSectionPanelSchedulePlan {
    pub(crate) schedule: CrossSectionPanelScheduleState,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CrossSectionScheduleInput<'a> {
    pub(crate) view: &'a ViewState,
    pub(crate) active_layer_target: Option<ScaleLevel>,
    pub(crate) requirements: &'a [DatasetResourceKey],
    pub(crate) retained_leases: &'a RetainedLeases,
    pub(crate) dataset_failed: bool,
}

pub(crate) fn schedule_cross_section_panel(
    coordination: &mut RenderCoordinationState,
    input: CrossSectionScheduleInput<'_>,
    panel_id: PanelId,
    gpu_display_available: bool,
) -> anyhow::Result<CrossSectionPanelSchedulePlan> {
    let schedule_start = Instant::now();
    let schedule =
        build_cross_section_panel_schedule(coordination, input, panel_id, gpu_display_available)?;
    let schedule_ms = schedule_start.elapsed().as_secs_f64() * 1000.0;
    if schedule_ms > CROSS_SECTION_PANEL_SCHEDULER_CPU_BUDGET_MS {
        tracing::debug!(
            panel = panel_id.label(),
            schedule_ms,
            budget_ms = CROSS_SECTION_PANEL_SCHEDULER_CPU_BUDGET_MS,
            "cross-section panel scheduling exceeded its per-panel CPU budget"
        );
    }
    coordination.set_cross_section_schedule(panel_id.presentation_slot(), schedule);
    Ok(CrossSectionPanelSchedulePlan { schedule })
}

fn build_cross_section_panel_schedule(
    coordination: &RenderCoordinationState,
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

    let panel_runtime = coordination.surface(panel_id.presentation_slot());
    let generation = panel_runtime.generation();
    if panel_runtime.presentation_viewport().is_none() || panel_runtime.render_viewport().is_none()
    {
        return Ok(CrossSectionPanelScheduleState::missing_viewport(generation));
    }

    let active_layer_scale_level = input.active_layer_target.map(ScaleLevel::get);
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
    );

    Ok(CrossSectionPanelScheduleState {
        generation,
        target_scale_level: active_layer_scale_level,
        render_scale_level: active_layer_scale_level,
        fallback_scale_level: None,
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
    } else {
        (
            CrossSectionPanelScheduleStatus::Ready,
            CrossSectionPanelScheduleReason::TargetScaleReady,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cross_section_demand_is_terminal_not_loading() {
        let (status, reason) = classify_schedule(false, true, 0, 0);
        assert_eq!(status, CrossSectionPanelScheduleStatus::Empty);
        assert_eq!(reason, CrossSectionPanelScheduleReason::NoSelectedData);
    }

    #[test]
    fn complete_cross_section_demand_has_no_coarse_fallback_classification() {
        let (status, reason) = classify_schedule(false, true, 4, 0);
        assert_eq!(status, CrossSectionPanelScheduleStatus::Ready);
        assert_eq!(reason, CrossSectionPanelScheduleReason::TargetScaleReady);
    }
}

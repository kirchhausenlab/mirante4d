use std::time::Instant;

use mirante4d_core::{IntensityDType, LayerId};

use crate::{
    APP_MIB, AppLayerSummary, AppState, CROSS_SECTION_INTERACTION_SETTLE_DURATION,
    cross_section_runtime::{
        CrossSectionChunkKey, CrossSectionVisibleChunkPlan,
        plan_cross_section_visible_chunks_for_state,
    },
    cross_section_streaming::cross_section_panel_stream_work_active,
    lod_scheduler::{current_frame_gpu_budget_bytes, representative_voxel_world_size},
    resident_rendering::cross_section_panel_render_request_for_state,
    viewer_layout::{
        CrossSectionPanelScheduleReason, CrossSectionPanelScheduleState,
        CrossSectionPanelScheduleStatus, PanelId, ViewerLayout,
    },
};

pub(crate) const CROSS_SECTION_PANEL_SCHEDULER_CPU_BUDGET_MS: f64 = 1.0;
pub(crate) const CROSS_SECTION_PANEL_RENDER_SUBMISSIONS_PER_PANEL_REFRESH: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CrossSectionPanelSchedulePlan {
    pub(crate) schedule: CrossSectionPanelScheduleState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CrossSectionBrickPressure {
    selected_bricks: usize,
    occupied_selected_bricks: usize,
    missing_occupied_bricks: usize,
    estimated_decoded_bytes: u64,
}

pub(crate) fn schedule_cross_section_panel_for_state(
    state: &mut AppState,
    panel_id: PanelId,
    gpu_display_available: bool,
) -> anyhow::Result<CrossSectionPanelSchedulePlan> {
    let schedule_start = Instant::now();
    let (schedule, visible_plan) =
        build_cross_section_panel_schedule(state, panel_id, gpu_display_available)?;
    let schedule_ms = schedule_start.elapsed().as_secs_f64() * 1000.0;
    if schedule_ms > CROSS_SECTION_PANEL_SCHEDULER_CPU_BUDGET_MS {
        tracing::debug!(
            panel = panel_id.label(),
            schedule_ms,
            budget_ms = CROSS_SECTION_PANEL_SCHEDULER_CPU_BUDGET_MS,
            "cross-section panel scheduling exceeded its per-panel CPU budget"
        );
    }
    state
        .viewer_layout
        .set_cross_section_panel_schedule(panel_id, schedule);
    if let Some(visible_plan) = visible_plan {
        state
            .cross_section_runtime
            .apply_visible_chunk_plan(visible_plan);
    }
    Ok(CrossSectionPanelSchedulePlan { schedule })
}

pub(crate) fn mark_cross_section_panel_rendered(
    state: &mut AppState,
    panel_id: PanelId,
    schedule: CrossSectionPanelScheduleState,
) {
    state
        .viewer_layout
        .set_cross_section_panel_schedule(panel_id, schedule.rendered());
}

pub(crate) fn mark_cross_section_panel_render_failed(
    state: &mut AppState,
    panel_id: PanelId,
    mut schedule: CrossSectionPanelScheduleState,
) {
    schedule.status = CrossSectionPanelScheduleStatus::Unavailable;
    schedule.reason = CrossSectionPanelScheduleReason::RenderFailed;
    state
        .viewer_layout
        .set_cross_section_panel_schedule(panel_id, schedule);
}

fn build_cross_section_panel_schedule(
    state: &AppState,
    panel_id: PanelId,
    gpu_display_available: bool,
) -> anyhow::Result<(
    CrossSectionPanelScheduleState,
    Option<CrossSectionVisibleChunkPlan>,
)> {
    if state.viewer_layout.layout() != ViewerLayout::FourPanel
        || panel_id.cross_section_panel().is_none()
    {
        return Ok((
            CrossSectionPanelScheduleState {
                status: CrossSectionPanelScheduleStatus::Unavailable,
                reason: CrossSectionPanelScheduleReason::GpuUnavailable,
                ..CrossSectionPanelScheduleState::missing_viewport(0)
            },
            None,
        ));
    }

    let runtime = state.viewer_layout.four_panel_runtime();
    let generation = runtime
        .and_then(|runtime| runtime.panel(panel_id))
        .map(|panel| panel.generation)
        .unwrap_or(0);
    let request = match cross_section_panel_render_request_for_state(state, panel_id) {
        Ok(request) => request,
        Err(_) => {
            return Ok((
                CrossSectionPanelScheduleState::missing_viewport(generation),
                None,
            ));
        }
    };

    let layer_id = LayerId::new(state.active_layer_id.clone())?;
    let target_scale_level = cross_section_target_scale_for_state(state, &layer_id)?;
    let render_scale_level =
        cross_section_render_scale_for_state(state, &layer_id, target_scale_level)?;
    let decoded_budget_bytes = cross_section_panel_decoded_budget_bytes(state);
    let visible_plan =
        plan_cross_section_visible_chunks_for_state(state, panel_id, render_scale_level)?;
    let pressure = cross_section_brick_pressure_for_visible_plan(state, &visible_plan)?;
    let fallback_scale_level =
        (render_scale_level > target_scale_level).then_some(render_scale_level);

    let (status, reason) = if !gpu_display_available {
        (
            CrossSectionPanelScheduleStatus::Unavailable,
            CrossSectionPanelScheduleReason::GpuUnavailable,
        )
    } else if pressure.estimated_decoded_bytes > decoded_budget_bytes {
        (
            CrossSectionPanelScheduleStatus::BudgetLimited,
            CrossSectionPanelScheduleReason::DecodedBudgetExceeded,
        )
    } else if pressure.missing_occupied_bricks > 0 {
        let status = if cross_section_panel_stream_work_active(state, panel_id) {
            CrossSectionPanelScheduleStatus::Loading
        } else {
            CrossSectionPanelScheduleStatus::Incomplete
        };
        (
            status,
            CrossSectionPanelScheduleReason::MissingSelectedBricks,
        )
    } else if fallback_scale_level.is_some() {
        (
            CrossSectionPanelScheduleStatus::Coarse,
            CrossSectionPanelScheduleReason::ResidentScaleCoarserThanTarget,
        )
    } else {
        (
            CrossSectionPanelScheduleStatus::Ready,
            CrossSectionPanelScheduleReason::TargetScaleReady,
        )
    };

    Ok((
        CrossSectionPanelScheduleState {
            generation: request.generation,
            target_scale_level: Some(target_scale_level),
            render_scale_level: Some(render_scale_level),
            fallback_scale_level,
            selected_bricks: pressure.selected_bricks,
            occupied_selected_bricks: pressure.occupied_selected_bricks,
            missing_occupied_bricks: pressure.missing_occupied_bricks,
            estimated_decoded_bytes: pressure.estimated_decoded_bytes,
            decoded_budget_bytes,
            status,
            reason,
        },
        Some(visible_plan),
    ))
}

pub(crate) fn cross_section_target_scale_for_state(
    state: &AppState,
    layer_id: &LayerId,
) -> anyhow::Result<u32> {
    let scale_count = state.dataset.scale_count(layer_id)?;
    let world_per_point = state
        .viewer_layout
        .cross_section
        .scale_world_per_screen_point
        .max(f64::EPSILON);
    let mut selected = 0;
    for scale_index in 0..scale_count {
        let scale_level = scale_index as u32;
        let grid_to_world = state.dataset.scale_grid_to_world(layer_id, scale_level)?;
        let voxel_size = representative_voxel_world_size(grid_to_world);
        if voxel_size <= world_per_point {
            selected = scale_level;
        }
    }
    Ok(selected)
}

pub(crate) fn cross_section_render_scale_for_state(
    state: &AppState,
    layer_id: &LayerId,
    target_scale_level: u32,
) -> anyhow::Result<u32> {
    if !cross_section_interaction_recent(state) {
        return Ok(target_scale_level);
    }
    let scale_count = state.dataset.scale_count(layer_id)?;
    Ok(target_scale_level
        .saturating_add(1)
        .min(scale_count.saturating_sub(1) as u32))
}

pub(crate) fn cross_section_interaction_recent(state: &AppState) -> bool {
    state
        .cross_section_last_interaction_at
        .is_some_and(|last_interaction_at| {
            last_interaction_at.elapsed() < CROSS_SECTION_INTERACTION_SETTLE_DURATION
        })
}

pub(crate) fn cross_section_interaction_settled(state: &AppState) -> bool {
    !cross_section_interaction_recent(state)
}

pub(crate) fn cross_section_panel_refinement_due(state: &AppState, panel_id: PanelId) -> bool {
    if !cross_section_interaction_settled(state) {
        return false;
    }
    state
        .viewer_layout
        .four_panel_runtime()
        .and_then(|runtime| runtime.panel(panel_id))
        .and_then(|panel| panel.cross_section_schedule)
        .is_some_and(|schedule| {
            schedule.status == CrossSectionPanelScheduleStatus::Coarse
                || schedule.fallback_scale_level.is_some()
        })
}

pub(crate) fn cross_section_refinement_work_pending(state: &AppState) -> bool {
    state
        .viewer_layout
        .four_panel_runtime()
        .is_some_and(|runtime| {
            runtime
                .panels()
                .iter()
                .any(|panel| cross_section_panel_refinement_due(state, panel.panel_id))
        })
}

pub(crate) fn cross_section_panel_decoded_budget_bytes(state: &AppState) -> u64 {
    (current_frame_gpu_budget_bytes(state) / 3).max(APP_MIB)
}

fn cross_section_brick_pressure_for_visible_plan(
    state: &AppState,
    visible_plan: &CrossSectionVisibleChunkPlan,
) -> anyhow::Result<CrossSectionBrickPressure> {
    let mut occupied_selected_bricks = 0usize;
    let mut missing_occupied_bricks = 0usize;
    let mut estimated_decoded_bytes = 0u64;

    for chunk_key in &visible_plan.visible_chunks {
        if &chunk_key.dataset_id != state.dataset.dataset_id()
            || chunk_key.scale_level != visible_plan.scale_level
            || chunk_key.timepoint != state.active_timepoint
        {
            continue;
        }
        let Some(layer) = layer_for_id(state, &chunk_key.layer_id) else {
            continue;
        };
        let dtype_bytes = dtype_decoded_bytes(layer.dtype);
        let metadata = state.dataset.brick_metadata_at_scale(
            &chunk_key.layer_id,
            visible_plan.scale_level,
            state.active_timepoint,
            chunk_key.brick_index,
        )?;
        if !metadata.occupied {
            continue;
        }
        occupied_selected_bricks = occupied_selected_bricks.saturating_add(1);
        estimated_decoded_bytes = estimated_decoded_bytes.saturating_add(
            metadata
                .region
                .shape()?
                .element_count()?
                .saturating_mul(dtype_bytes),
        );
        let key = CrossSectionChunkKey {
            dataset_id: state.dataset.dataset_id().clone(),
            layer_id: chunk_key.layer_id.clone(),
            timepoint: state.active_timepoint,
            scale_level: visible_plan.scale_level,
            brick_index: chunk_key.brick_index,
        };
        if !state
            .cross_section_runtime
            .has_cpu_resident_chunk(&key, metadata.region)
        {
            missing_occupied_bricks = missing_occupied_bricks.saturating_add(1);
        }
    }

    Ok(CrossSectionBrickPressure {
        selected_bricks: visible_plan.visible_chunks.len(),
        occupied_selected_bricks,
        missing_occupied_bricks,
        estimated_decoded_bytes,
    })
}

fn layer_for_id<'a>(state: &'a AppState, layer_id: &LayerId) -> Option<&'a AppLayerSummary> {
    state
        .layers
        .iter()
        .find(|layer| layer.id == layer_id.as_str())
}

fn dtype_decoded_bytes(dtype: IntensityDType) -> u64 {
    match dtype {
        IntensityDType::Uint8 => std::mem::size_of::<u8>() as u64,
        IntensityDType::Uint16 => std::mem::size_of::<u16>() as u64,
        IntensityDType::Float32 => std::mem::size_of::<f32>() as u64,
    }
}

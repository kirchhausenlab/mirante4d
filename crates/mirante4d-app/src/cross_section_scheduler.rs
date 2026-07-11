use std::time::Instant;

use mirante4d_application::CrossSectionPanelId;
use mirante4d_domain::{IntensityDType, ViewerLayout};
use mirante4d_format::LayerId;
use mirante4d_project_model::ViewState;

use crate::{
    CROSS_SECTION_INTERACTION_SETTLE_DURATION,
    cross_section_runtime::{
        CrossSectionChunkKey, CrossSectionLayerInput, CrossSectionRuntime,
        CrossSectionVisibleChunkPlan, CrossSectionVisiblePlanInput,
        plan_cross_section_visible_chunks,
    },
    cross_section_streaming::cross_section_panel_stream_work_active,
    current_runtime::{dataset::CurrentDatasetRuntime, render::CurrentRenderRuntime},
    lod_scheduler::representative_voxel_world_size,
    render_state::ResidentRenderFailureStatus,
    viewer_layout::{
        CrossSectionPanelScheduleReason, CrossSectionPanelScheduleState,
        CrossSectionPanelScheduleStatus, PanelId,
    },
};

const MIB: u64 = 1024 * 1024;

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

#[derive(Debug, Clone, Copy)]
pub(crate) struct CrossSectionScheduleInput<'a> {
    pub(crate) view: &'a ViewState,
    pub(crate) active_layer_id: &'a LayerId,
    pub(crate) layers: &'a [CrossSectionLayerInput<'a>],
    pub(crate) active_panel: Option<CrossSectionPanelId>,
    pub(crate) gpu_budget_bytes: u64,
}

pub(crate) fn schedule_cross_section_panel(
    dataset: &CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    input: CrossSectionScheduleInput<'_>,
    panel_id: PanelId,
    gpu_display_available: bool,
) -> anyhow::Result<CrossSectionPanelSchedulePlan> {
    let schedule_start = Instant::now();
    let (schedule, visible_plan) = build_cross_section_panel_schedule(
        dataset,
        render,
        input,
        panel_id,
        gpu_display_available,
    )?;
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
    if let Some(visible_plan) = visible_plan {
        render
            .cross_section_runtime
            .apply_visible_chunk_plan(visible_plan);
    }
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
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    input: CrossSectionScheduleInput<'_>,
    panel_id: PanelId,
    gpu_display_available: bool,
) -> anyhow::Result<(
    CrossSectionPanelScheduleState,
    Option<CrossSectionVisibleChunkPlan>,
)> {
    if input.view.layout() != ViewerLayout::FourPanel || panel_id.cross_section_panel().is_none() {
        return Ok((
            CrossSectionPanelScheduleState {
                status: CrossSectionPanelScheduleStatus::Unavailable,
                reason: CrossSectionPanelScheduleReason::GpuUnavailable,
                ..CrossSectionPanelScheduleState::missing_viewport(0)
            },
            None,
        ));
    }

    let generation = render
        .cross_section_runtime
        .panel(panel_id)
        .map(|panel| panel.generation)
        .unwrap_or(0);
    let Some(panel_runtime) = render.cross_section_runtime.panel(panel_id) else {
        return Ok((
            CrossSectionPanelScheduleState::missing_viewport(generation),
            None,
        ));
    };
    if panel_runtime.presentation_viewport.is_none() || panel_runtime.render_viewport.is_none() {
        return Ok((
            CrossSectionPanelScheduleState::missing_viewport(generation),
            None,
        ));
    }

    let target_scale_level =
        cross_section_target_scale(dataset, input.view, input.active_layer_id)?;
    let render_scale_level =
        cross_section_render_scale(dataset, input.active_layer_id, target_scale_level)?;
    let decoded_budget_bytes = cross_section_panel_decoded_budget_bytes(input.gpu_budget_bytes);
    let layer_ids = input
        .layers
        .iter()
        .map(|layer| layer.id.clone())
        .collect::<Vec<_>>();
    let visible_plan = plan_cross_section_visible_chunks(
        dataset,
        &render.cross_section_runtime,
        CrossSectionVisiblePlanInput {
            view: input.view,
            active_panel: input.active_panel.map(PanelId::from_application_panel),
            layer_ids: &layer_ids,
        },
        panel_id,
        render_scale_level,
    )?;
    let pressure = cross_section_brick_pressure_for_visible_plan(
        dataset,
        &render.cross_section_runtime,
        input,
        &visible_plan,
    )?;
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
        let status =
            if cross_section_panel_stream_work_active(&render.cross_section_runtime, panel_id) {
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
            generation,
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

pub(crate) fn cross_section_target_scale(
    dataset: &CurrentDatasetRuntime,
    view: &ViewState,
    layer_id: &LayerId,
) -> anyhow::Result<u32> {
    let scale_count = dataset.dataset.scale_count(layer_id)?;
    let world_per_point = view
        .cross_section()
        .scale_world_per_screen_point()
        .max(f64::EPSILON);
    let mut selected = 0;
    for scale_index in 0..scale_count {
        let scale_level = scale_index as u32;
        let grid_to_world = dataset.dataset.scale_grid_to_world(layer_id, scale_level)?;
        let voxel_size = representative_voxel_world_size(grid_to_world);
        if voxel_size <= world_per_point {
            selected = scale_level;
        }
    }
    Ok(selected)
}

pub(crate) fn cross_section_render_scale(
    dataset: &CurrentDatasetRuntime,
    layer_id: &LayerId,
    target_scale_level: u32,
) -> anyhow::Result<u32> {
    if !cross_section_interaction_recent(dataset) {
        return Ok(target_scale_level);
    }
    let scale_count = dataset.dataset.scale_count(layer_id)?;
    Ok(target_scale_level
        .saturating_add(1)
        .min(scale_count.saturating_sub(1) as u32))
}

pub(crate) fn cross_section_interaction_recent(dataset: &CurrentDatasetRuntime) -> bool {
    dataset
        .cross_section_last_interaction_at
        .is_some_and(|last_interaction_at| {
            last_interaction_at.elapsed() < CROSS_SECTION_INTERACTION_SETTLE_DURATION
        })
}

pub(crate) fn cross_section_interaction_settled(dataset: &CurrentDatasetRuntime) -> bool {
    !cross_section_interaction_recent(dataset)
}

pub(crate) fn cross_section_panel_refinement_due(
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    panel_id: PanelId,
) -> bool {
    if !cross_section_interaction_settled(dataset) {
        return false;
    }
    render
        .cross_section_runtime
        .panel(panel_id)
        .and_then(|panel| panel.cross_section_schedule)
        .is_some_and(|schedule| {
            schedule.status == CrossSectionPanelScheduleStatus::Coarse
                || schedule.fallback_scale_level.is_some()
        })
}

pub(crate) fn cross_section_refinement_work_pending(
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
) -> bool {
    render
        .cross_section_runtime
        .panels()
        .any(|panel| cross_section_panel_refinement_due(dataset, render, panel.panel_id))
}

pub(crate) fn cross_section_panel_decoded_budget_bytes(gpu_budget_bytes: u64) -> u64 {
    (gpu_budget_bytes / 3).max(MIB)
}

fn cross_section_brick_pressure_for_visible_plan(
    dataset: &CurrentDatasetRuntime,
    runtime: &CrossSectionRuntime,
    input: CrossSectionScheduleInput<'_>,
    visible_plan: &CrossSectionVisibleChunkPlan,
) -> anyhow::Result<CrossSectionBrickPressure> {
    let mut occupied_selected_bricks = 0usize;
    let mut missing_occupied_bricks = 0usize;
    let mut estimated_decoded_bytes = 0u64;

    for chunk_key in &visible_plan.visible_chunks {
        if &chunk_key.dataset_id != dataset.dataset.dataset_id()
            || chunk_key.scale_level != visible_plan.scale_level
            || chunk_key.timepoint != input.view.timepoint()
        {
            continue;
        }
        let Some(layer) = layer_for_id(input.layers, &chunk_key.layer_id) else {
            continue;
        };
        let dtype_bytes = dtype_decoded_bytes(layer.dtype);
        let metadata = dataset.dataset.brick_metadata_at_scale(
            &chunk_key.layer_id,
            visible_plan.scale_level,
            input.view.timepoint(),
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
            dataset_id: dataset.dataset.dataset_id().clone(),
            layer_id: chunk_key.layer_id.clone(),
            timepoint: input.view.timepoint(),
            scale_level: visible_plan.scale_level,
            brick_index: chunk_key.brick_index,
        };
        if !runtime.has_cpu_resident_chunk(&key, metadata.region) {
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

fn layer_for_id<'a>(
    layers: &'a [CrossSectionLayerInput<'a>],
    layer_id: &LayerId,
) -> Option<CrossSectionLayerInput<'a>> {
    layers.iter().copied().find(|layer| layer.id == layer_id)
}

fn dtype_decoded_bytes(dtype: IntensityDType) -> u64 {
    match dtype {
        IntensityDType::Uint8 => std::mem::size_of::<u8>() as u64,
        IntensityDType::Uint16 => std::mem::size_of::<u16>() as u64,
        IntensityDType::Float32 => std::mem::size_of::<f32>() as u64,
    }
}

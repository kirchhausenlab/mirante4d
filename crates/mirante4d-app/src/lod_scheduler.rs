use std::collections::HashSet;

use glam::DVec3;
use mirante4d_core::{IntensityDType, LayerId, Projection, Shape3D};
use mirante4d_data::SpatialBrickIndex;
use mirante4d_renderer::{BrickGridSpec, BrickPlanOptions, RenderViewport, plan_visible_bricks};

use crate::{
    APP_MIB, AppState, FrameCompleteness, LodDecisionReason, RenderMode, RenderSamplingPolicy,
    brick_streaming::{current_resident_frame_ready, stream_layer_ids_for_state},
    playback::playback_effective_lod_target,
    render_state::frame_completeness_for_rendered_scale,
};

pub(crate) const MIN_LOD_CANDIDATE_VISIBLE_BRICKS: usize = 256;
pub(crate) const MAX_LOD_CANDIDATE_VISIBLE_BRICKS: usize = 4096;
const LOD_CANDIDATE_VISIBLE_BRICK_BUDGET_BYTES_PER_BRICK: u64 = 2 * APP_MIB;

pub(crate) fn update_visible_brick_plan(state: &mut AppState) {
    match visible_bricks_for_state(state) {
        Ok(plan) => {
            state.visible_brick_count = plan.bricks.len();
            state.visible_brick_plan_stride = plan.stride;
            state.visible_brick_plan_error = None;
            state.visible_bricks = plan.bricks;
            state.brick_stream_scale_level = plan.scale_level;
            state.brick_stream_scale_shape = plan.scale_shape;
            state.lod_schedule.target_scale_level = plan.target_scale_level;
            state.lod_schedule.fallback_scale_level = plan.fallback_scale_level;
            state.lod_schedule.pending_scale_level = Some(plan.scale_level);
            state.frame_fidelity.target_scale_level = plan.target_scale_level;
            state.frame_fidelity.reason = plan.reason;
            if current_resident_frame_ready(state) {
                state.frame_fidelity.completeness = frame_completeness_for_rendered_scale(
                    plan.scale_level,
                    plan.target_scale_level,
                    plan.reason,
                );
            } else if matches!(
                plan.reason,
                LodDecisionReason::GpuBudgetLimited
                    | LodDecisionReason::CpuBudgetLimited
                    | LodDecisionReason::FrameBudgetLimited
                    | LodDecisionReason::BackendLimit
            ) {
                state.frame_fidelity.completeness = FrameCompleteness::BudgetLimited;
            } else {
                state.frame_fidelity.completeness = FrameCompleteness::Loading;
            }
        }
        Err(err) => {
            state.visible_brick_count = 0;
            state.visible_bricks.clear();
            state.lod_schedule.pending_scale_level = None;
            state.visible_brick_plan_error = Some(err.to_string());
            state.frame_fidelity.completeness = FrameCompleteness::Incomplete;
            state.frame_fidelity.reason = LodDecisionReason::BackendLimit;
        }
    }
}

#[cfg(test)]
pub(crate) fn select_stream_scale_for_state(
    state: &AppState,
    layer_id: &LayerId,
) -> anyhow::Result<u32> {
    Ok(lod_plan_for_state(state, layer_id)?.scale_level)
}

pub(crate) fn representative_voxel_world_size(grid_to_world: mirante4d_core::GridToWorld) -> f64 {
    let x = grid_to_world.transform_vector(DVec3::X).length();
    let y = grid_to_world.transform_vector(DVec3::Y).length();
    let z = grid_to_world.transform_vector(DVec3::Z).length();
    x.max(y).max(z).max(f64::EPSILON)
}

#[derive(Debug)]
struct VisibleBrickPlan {
    bricks: Vec<SpatialBrickIndex>,
    stride: u64,
    scale_level: u32,
    target_scale_level: u32,
    fallback_scale_level: Option<u32>,
    scale_shape: Shape3D,
    reason: LodDecisionReason,
    estimated_decoded_bytes: u64,
}

fn visible_bricks_for_state(state: &AppState) -> anyhow::Result<VisibleBrickPlan> {
    let layer_id = LayerId::new(state.active_layer_id.clone())?;
    lod_plan_for_state(state, &layer_id)
}

fn lod_plan_for_state(state: &AppState, layer_id: &LayerId) -> anyhow::Result<VisibleBrickPlan> {
    let scale_count = state.dataset.scale_count(layer_id)?;
    let (target_scale_level, reason) = target_scale_for_state(state, layer_id, scale_count)?;

    let visible_brick_budget = Some(lod_candidate_visible_brick_budget(state));
    let decoded_budget = current_frame_decoded_budget_bytes(state);
    let gpu_budget = current_frame_gpu_budget_bytes(state);
    let mut last_budget_reason = reason;
    for scale_index in target_scale_level as usize..scale_count {
        let scale_level = scale_index as u32;
        if state.lod_schedule.hard_failed_scale_level == Some(scale_level) {
            last_budget_reason = state
                .lod_schedule
                .hard_failure_reason
                .unwrap_or(LodDecisionReason::BackendLimit);
            continue;
        }
        let plan = visible_bricks_for_scale(state, layer_id, scale_level)?;
        if let Some(visible_brick_budget) = visible_brick_budget
            && plan.bricks.len() > visible_brick_budget
        {
            last_budget_reason = LodDecisionReason::GpuBudgetLimited;
            continue;
        }
        if plan.estimated_decoded_bytes > gpu_budget {
            last_budget_reason = LodDecisionReason::GpuBudgetLimited;
            continue;
        }
        if plan.estimated_decoded_bytes > decoded_budget {
            last_budget_reason = LodDecisionReason::CpuBudgetLimited;
            continue;
        }
        let selected_reason = if scale_level == target_scale_level {
            reason
        } else {
            last_budget_reason
        };
        return Ok(VisibleBrickPlan {
            target_scale_level,
            fallback_scale_level: (scale_level != target_scale_level).then_some(scale_level),
            reason: selected_reason,
            ..plan
        });
    }

    let mut plan = visible_bricks_for_scale(state, layer_id, (scale_count - 1) as u32)?;
    plan.target_scale_level = target_scale_level;
    plan.fallback_scale_level =
        (plan.scale_level != target_scale_level).then_some(plan.scale_level);
    plan.reason = last_budget_reason;
    Ok(plan)
}

fn target_scale_for_state(
    state: &AppState,
    layer_id: &LayerId,
    scale_count: usize,
) -> anyhow::Result<(u32, LodDecisionReason)> {
    let normal_target_scale_level =
        screen_equivalent_scale_for_state(state, layer_id, scale_count)?;
    Ok(playback_effective_lod_target(
        normal_target_scale_level,
        scale_count,
        state.playback_lod_downshift_active,
    ))
}

fn screen_equivalent_scale_for_state(
    state: &AppState,
    layer_id: &LayerId,
    scale_count: usize,
) -> anyhow::Result<u32> {
    let world_per_pixel =
        screen_equivalent_world_per_screen_point(state, layer_id)?.max(f64::EPSILON);
    let mut selected = 0;
    for scale_index in 0..scale_count {
        let scale_level = scale_index as u32;
        let grid_to_world = state.dataset.scale_grid_to_world(layer_id, scale_level)?;
        let voxel_size = representative_voxel_world_size(grid_to_world);
        if voxel_size <= world_per_pixel {
            selected = scale_level;
        }
    }
    Ok(selected)
}

fn screen_equivalent_world_per_screen_point(
    state: &AppState,
    layer_id: &LayerId,
) -> anyhow::Result<f64> {
    if state.camera.projection == Projection::Orthographic {
        return Ok(state.camera.orthographic_world_per_screen_point);
    }

    let camera = state.camera.to_camera_state(state.presentation_viewport);
    let forward = (camera.target - camera.eye).normalize();
    let shape = state.dataset.scale_shape(layer_id, 0)?;
    let grid_to_world = state.dataset.scale_grid_to_world(layer_id, 0)?;
    let mut nearest_depth = f64::INFINITY;
    for z in [0.0, shape.z as f64] {
        for y in [0.0, shape.y as f64] {
            for x in [0.0, shape.x as f64] {
                let world = grid_to_world.transform_point(DVec3::new(x, y, z));
                let depth = (world - camera.eye).dot(forward);
                if depth > 0.0 {
                    nearest_depth = nearest_depth.min(depth);
                }
            }
        }
    }
    if !nearest_depth.is_finite() {
        nearest_depth = state.camera.perspective_view_distance_world;
    }
    Ok(nearest_depth.max(1.0) / state.camera.perspective_focal_length_screen_points)
}

pub(crate) fn lod_candidate_visible_brick_budget(state: &AppState) -> usize {
    let budget = state
        .renderer_gpu_brick_budget_bytes
        .checked_div(LOD_CANDIDATE_VISIBLE_BRICK_BUDGET_BYTES_PER_BRICK)
        .and_then(|bricks| usize::try_from(bricks).ok())
        .unwrap_or(MAX_LOD_CANDIDATE_VISIBLE_BRICKS);
    budget.clamp(
        MIN_LOD_CANDIDATE_VISIBLE_BRICKS,
        MAX_LOD_CANDIDATE_VISIBLE_BRICKS,
    )
}

fn visible_bricks_for_scale(
    state: &AppState,
    layer_id: &LayerId,
    scale_level: u32,
) -> anyhow::Result<VisibleBrickPlan> {
    let brick_shape = state.dataset.brick_shape_at_scale(layer_id, scale_level)?;
    let scale_shape = state.dataset.scale_shape(layer_id, scale_level)?;
    let grid_to_world = state.dataset.scale_grid_to_world(layer_id, scale_level)?;
    let viewport = state.render_viewport;
    let stride = brick_plan_stride(viewport);
    let spec = BrickGridSpec {
        volume_shape: scale_shape,
        brick_shape,
        grid_to_world,
    };
    let options = BrickPlanOptions {
        pixel_stride: stride,
    };
    let camera = state.camera.to_camera_state(state.presentation_viewport);
    let mut bricks = plan_visible_bricks(camera, viewport, spec, options)?;
    if matches!(
        state.active_render_mode,
        RenderMode::Mip | RenderMode::Isosurface | RenderMode::Dvr
    ) && (state.render_sampling_policy == RenderSamplingPolicy::SmoothLinear
        || state.camera.projection == Projection::Perspective)
    {
        let brick_grid_shape = state
            .dataset
            .brick_grid_shape_at_scale(layer_id, scale_level)?;
        bricks = expand_spatial_brick_plan(&bricks, brick_grid_shape, BrickPlanMargins::uniform(1));
    }
    let estimated_decoded_bytes =
        estimate_visible_decoded_bytes(state, layer_id, scale_level, &bricks)?;
    Ok(VisibleBrickPlan {
        bricks,
        stride,
        scale_level,
        target_scale_level: scale_level,
        fallback_scale_level: None,
        scale_shape,
        reason: LodDecisionReason::ExactS0,
        estimated_decoded_bytes,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BrickPlanMargins {
    z: u64,
    y: u64,
    x: u64,
}

impl BrickPlanMargins {
    const ZERO: Self = Self { z: 0, y: 0, x: 0 };

    const fn uniform(margin: u64) -> Self {
        Self {
            z: margin,
            y: margin,
            x: margin,
        }
    }
}

pub(crate) fn expand_spatial_brick_plan(
    bricks: &[SpatialBrickIndex],
    grid_shape: Shape3D,
    margins: BrickPlanMargins,
) -> Vec<SpatialBrickIndex> {
    if margins == BrickPlanMargins::ZERO || bricks.is_empty() {
        return bricks.to_vec();
    }
    let mut expanded = HashSet::new();
    for brick in bricks {
        let z_start = brick.z.saturating_sub(margins.z);
        let y_start = brick.y.saturating_sub(margins.y);
        let x_start = brick.x.saturating_sub(margins.x);
        let z_end = brick.z.saturating_add(margins.z).min(grid_shape.z - 1);
        let y_end = brick.y.saturating_add(margins.y).min(grid_shape.y - 1);
        let x_end = brick.x.saturating_add(margins.x).min(grid_shape.x - 1);
        for z in z_start..=z_end {
            for y in y_start..=y_end {
                for x in x_start..=x_end {
                    expanded.insert(SpatialBrickIndex::new(z, y, x));
                }
            }
        }
    }
    let mut expanded = expanded.into_iter().collect::<Vec<_>>();
    expanded.sort_by_key(|brick| (brick.z, brick.y, brick.x));
    expanded
}

fn current_frame_decoded_budget_bytes(state: &AppState) -> u64 {
    let config = state.dataset.runtime_config();
    config
        .max_in_flight_decoded_bytes
        .min(config.brick_cache_budget_bytes / 2)
        .max(64 * APP_MIB)
}

pub(crate) fn current_frame_gpu_budget_bytes(state: &AppState) -> u64 {
    (state.renderer_gpu_brick_budget_bytes / 2).max(APP_MIB)
}

fn estimate_visible_decoded_bytes(
    state: &AppState,
    active_layer_id: &LayerId,
    scale_level: u32,
    bricks: &[SpatialBrickIndex],
) -> anyhow::Result<u64> {
    let layer_ids =
        stream_layer_ids_for_state(state).unwrap_or_else(|_| vec![active_layer_id.clone()]);
    let mut total = 0u64;
    for layer_id in layer_ids {
        let dtype_bytes = state
            .layers
            .iter()
            .find(|layer| layer.id == layer_id.as_str())
            .map(|layer| dtype_decoded_bytes(layer.dtype))
            .unwrap_or(std::mem::size_of::<u16>() as u64);
        for brick in bricks {
            let metadata = state.dataset.brick_metadata_at_scale(
                &layer_id,
                scale_level,
                state.active_timepoint,
                *brick,
            )?;
            if !metadata.occupied {
                continue;
            }
            total = total.saturating_add(metadata.region.shape()?.element_count()? * dtype_bytes);
        }
    }
    Ok(total)
}

fn dtype_decoded_bytes(dtype: IntensityDType) -> u64 {
    match dtype {
        IntensityDType::Uint8 => std::mem::size_of::<u8>() as u64,
        IntensityDType::Uint16 => std::mem::size_of::<u16>() as u64,
        IntensityDType::Float32 => std::mem::size_of::<f32>() as u64,
    }
}

fn brick_plan_stride(viewport: RenderViewport) -> u64 {
    viewport.width.max(viewport.height).div_ceil(128).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_byte_budget_preserves_source_dtype_width() {
        assert_eq!(dtype_decoded_bytes(IntensityDType::Uint8), 1);
        assert_eq!(dtype_decoded_bytes(IntensityDType::Uint16), 2);
        assert_eq!(dtype_decoded_bytes(IntensityDType::Float32), 4);
    }
}

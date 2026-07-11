use std::collections::HashSet;

use glam::DVec3;
use mirante4d_application::ApplicationSnapshot;
use mirante4d_data::SpatialBrickIndex;
use mirante4d_domain::{
    GridToWorld, IntensityDType, LogicalLayerKey, Projection, SamplingPolicy, Shape3D,
};
use mirante4d_format::{CurrentGridToWorldExt, LayerId};
use mirante4d_render_api::CameraFrame;
use mirante4d_renderer::{BrickGridSpec, BrickPlanOptions, RenderViewport, plan_visible_bricks};

use crate::{
    FrameCompleteness, LodDecisionReason,
    brick_streaming::{
        current_resident_frame_ready, physical_layer_id_for_key, stream_layer_ids_for_snapshot,
        view_for_snapshot,
    },
    current_runtime::{dataset::CurrentDatasetRuntime, render::CurrentRenderRuntime},
    playback::playback_effective_lod_target,
    render_state::frame_completeness_for_rendered_scale,
};

const MIB: u64 = 1024 * 1024;

pub(crate) const MIN_LOD_CANDIDATE_VISIBLE_BRICKS: usize = 256;
pub(crate) const MAX_LOD_CANDIDATE_VISIBLE_BRICKS: usize = 4096;
const LOD_CANDIDATE_VISIBLE_BRICK_BUDGET_BYTES_PER_BRICK: u64 = 2 * MIB;

pub(crate) fn update_visible_brick_plan(
    snapshot: &ApplicationSnapshot,
    dataset: &mut CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
) {
    match visible_bricks_for_snapshot(snapshot, dataset, render) {
        Ok(plan) => {
            render.visible_brick_count = plan.bricks.len();
            render.visible_brick_plan_stride = plan.stride;
            render.visible_brick_plan_error = None;
            render.visible_bricks = plan.bricks;
            dataset.brick_stream_scale_level = plan.scale_level;
            dataset.brick_stream_scale_shape = plan.scale_shape;
            render.lod_schedule.target_scale_level = plan.target_scale_level;
            render.lod_schedule.fallback_scale_level = plan.fallback_scale_level;
            render.lod_schedule.pending_scale_level = Some(plan.scale_level);
            render.frame_fidelity.target_scale_level = plan.target_scale_level;
            render.frame_fidelity.reason = plan.reason;
            if current_resident_frame_ready(snapshot, dataset, render) {
                render.frame_fidelity.completeness = frame_completeness_for_rendered_scale(
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
                render.frame_fidelity.completeness = FrameCompleteness::BudgetLimited;
            } else {
                render.frame_fidelity.completeness = FrameCompleteness::Loading;
            }
        }
        Err(err) => {
            render.visible_brick_count = 0;
            render.visible_bricks.clear();
            render.lod_schedule.pending_scale_level = None;
            render.visible_brick_plan_error = Some(err.to_string());
            render.frame_fidelity.completeness = FrameCompleteness::Incomplete;
            render.frame_fidelity.reason = LodDecisionReason::BackendLimit;
        }
    }
}

#[cfg(test)]
pub(crate) fn select_stream_scale(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    layer_id: &LayerId,
) -> anyhow::Result<u32> {
    Ok(lod_plan(snapshot, dataset, render, layer_id)?.scale_level)
}

pub(crate) fn representative_voxel_world_size(grid_to_world: GridToWorld) -> f64 {
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

fn visible_bricks_for_snapshot(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
) -> anyhow::Result<VisibleBrickPlan> {
    let active_layer_id =
        physical_layer_id_for_key(dataset, view_for_snapshot(snapshot).active_layer())?;
    lod_plan(snapshot, dataset, render, &active_layer_id)
}

fn lod_plan(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    layer_id: &LayerId,
) -> anyhow::Result<VisibleBrickPlan> {
    let scale_count = dataset.dataset.scale_count(layer_id)?;
    let (target_scale_level, reason) =
        target_scale(snapshot, dataset, render, layer_id, scale_count)?;

    let visible_brick_budget = Some(lod_candidate_visible_brick_budget(snapshot));
    let decoded_budget = current_frame_decoded_budget_bytes(dataset);
    let gpu_budget = current_frame_gpu_budget_bytes(snapshot);
    let mut last_budget_reason = reason;
    for scale_index in target_scale_level as usize..scale_count {
        let scale_level = scale_index as u32;
        if render.lod_schedule.hard_failed_scale_level == Some(scale_level) {
            last_budget_reason = render
                .lod_schedule
                .hard_failure_reason
                .unwrap_or(LodDecisionReason::BackendLimit);
            continue;
        }
        let plan = visible_bricks_for_scale(snapshot, dataset, render, layer_id, scale_level)?;
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

    let mut plan = visible_bricks_for_scale(
        snapshot,
        dataset,
        render,
        layer_id,
        (scale_count - 1) as u32,
    )?;
    plan.target_scale_level = target_scale_level;
    plan.fallback_scale_level =
        (plan.scale_level != target_scale_level).then_some(plan.scale_level);
    plan.reason = last_budget_reason;
    Ok(plan)
}

fn target_scale(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    layer_id: &LayerId,
    scale_count: usize,
) -> anyhow::Result<(u32, LodDecisionReason)> {
    let normal_target_scale_level =
        screen_equivalent_scale(snapshot, dataset, render, layer_id, scale_count)?;
    Ok(playback_effective_lod_target(
        normal_target_scale_level,
        scale_count,
        render.playback_lod_downshift_active,
    ))
}

fn screen_equivalent_scale(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    layer_id: &LayerId,
    scale_count: usize,
) -> anyhow::Result<u32> {
    let world_per_pixel =
        screen_equivalent_world_per_screen_point(snapshot, dataset, render, layer_id)?
            .max(f64::EPSILON);
    let mut selected = 0;
    for scale_index in 0..scale_count {
        let scale_level = scale_index as u32;
        let grid_to_world = dataset.dataset.scale_grid_to_world(layer_id, scale_level)?;
        let voxel_size = representative_voxel_world_size(grid_to_world);
        if voxel_size <= world_per_pixel {
            selected = scale_level;
        }
    }
    Ok(selected)
}

fn screen_equivalent_world_per_screen_point(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    layer_id: &LayerId,
) -> anyhow::Result<f64> {
    let camera_view = *view_for_snapshot(snapshot).camera();
    if camera_view.projection() == Projection::Orthographic {
        return Ok(camera_view.orthographic_world_per_screen_point());
    }

    let camera = CameraFrame::new(camera_view, render.presentation_viewport)?;
    let eye = DVec3::from_array(camera.eye().components());
    let forward = DVec3::from_array(camera.axes().forward());
    let shape = dataset.dataset.scale_shape(layer_id, 0)?;
    let grid_to_world = dataset.dataset.scale_grid_to_world(layer_id, 0)?;
    let mut nearest_depth = f64::INFINITY;
    for z in [0.0, shape.z() as f64] {
        for y in [0.0, shape.y() as f64] {
            for x in [0.0, shape.x() as f64] {
                let world = grid_to_world.transform_point_vec(DVec3::new(x, y, z));
                let depth = (world - eye).dot(forward);
                if depth > 0.0 {
                    nearest_depth = nearest_depth.min(depth);
                }
            }
        }
    }
    if !nearest_depth.is_finite() {
        nearest_depth = camera_view.perspective_view_distance_world();
    }
    Ok(nearest_depth.max(1.0) / camera_view.perspective_focal_length_screen_points())
}

pub(crate) fn lod_candidate_visible_brick_budget(snapshot: &ApplicationSnapshot) -> usize {
    let budget = snapshot
        .resource_policy()
        .current_runtime_adapter()
        .gpu_brick_cache_budget_bytes()
        .checked_div(LOD_CANDIDATE_VISIBLE_BRICK_BUDGET_BYTES_PER_BRICK)
        .and_then(|bricks| usize::try_from(bricks).ok())
        .unwrap_or(MAX_LOD_CANDIDATE_VISIBLE_BRICKS);
    budget.clamp(
        MIN_LOD_CANDIDATE_VISIBLE_BRICKS,
        MAX_LOD_CANDIDATE_VISIBLE_BRICKS,
    )
}

fn visible_bricks_for_scale(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    layer_id: &LayerId,
    scale_level: u32,
) -> anyhow::Result<VisibleBrickPlan> {
    let brick_shape = dataset
        .dataset
        .brick_shape_at_scale(layer_id, scale_level)?;
    let scale_shape = dataset.dataset.scale_shape(layer_id, scale_level)?;
    let grid_to_world = dataset.dataset.scale_grid_to_world(layer_id, scale_level)?;
    let viewport = render.render_viewport;
    let stride = brick_plan_stride(viewport);
    let spec = BrickGridSpec {
        volume_shape: scale_shape,
        brick_shape,
        grid_to_world,
    };
    let options = BrickPlanOptions {
        pixel_stride: stride,
    };
    let view = view_for_snapshot(snapshot);
    let camera = CameraFrame::new(*view.camera(), render.presentation_viewport)?;
    let mut bricks = plan_visible_bricks(camera, viewport, spec, options)?;
    let active_render_state = view
        .layer(view.active_layer())
        .ok_or_else(|| anyhow::anyhow!("active logical layer is absent from the canonical view"))?
        .render_state();
    if active_render_state.sampling_policy() == SamplingPolicy::SmoothLinear
        || view.camera().projection() == Projection::Perspective
    {
        let brick_grid_shape = dataset
            .dataset
            .brick_grid_shape_at_scale(layer_id, scale_level)?;
        bricks = expand_spatial_brick_plan(&bricks, brick_grid_shape, BrickPlanMargins::uniform(1));
    }
    let estimated_decoded_bytes =
        estimate_visible_decoded_bytes(snapshot, dataset, scale_level, &bricks)?;
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
        let z_end = brick.z.saturating_add(margins.z).min(grid_shape.z() - 1);
        let y_end = brick.y.saturating_add(margins.y).min(grid_shape.y() - 1);
        let x_end = brick.x.saturating_add(margins.x).min(grid_shape.x() - 1);
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

fn current_frame_decoded_budget_bytes(dataset: &CurrentDatasetRuntime) -> u64 {
    let config = dataset.dataset.runtime_config();
    config
        .max_in_flight_decoded_bytes
        .min(config.brick_cache_budget_bytes / 2)
        .max(64 * MIB)
}

pub(crate) fn current_frame_gpu_budget_bytes(snapshot: &ApplicationSnapshot) -> u64 {
    (snapshot
        .resource_policy()
        .current_runtime_adapter()
        .gpu_brick_cache_budget_bytes()
        / 2)
    .max(MIB)
}

fn estimate_visible_decoded_bytes(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    scale_level: u32,
    bricks: &[SpatialBrickIndex],
) -> anyhow::Result<u64> {
    let layer_ids = stream_layer_ids_for_snapshot(snapshot, dataset)?;
    let timepoint = view_for_snapshot(snapshot).timepoint();
    let mut total = 0u64;
    for layer_id in layer_ids {
        let ordinal = dataset
            .dataset
            .manifest()
            .layers
            .iter()
            .position(|layer| layer.id == layer_id.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!("stream layer {layer_id} is absent from the manifest")
            })?;
        let key = LogicalLayerKey::new(u32::try_from(ordinal)?);
        let dtype_bytes = snapshot
            .catalog()
            .layer(key)
            .map(|layer| dtype_decoded_bytes(layer.dtype()))
            .ok_or_else(|| {
                anyhow::anyhow!("stream layer {layer_id} is absent from the canonical catalog")
            })?;
        for brick in bricks {
            let metadata = dataset.dataset.brick_metadata_at_scale(
                &layer_id,
                scale_level,
                timepoint,
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

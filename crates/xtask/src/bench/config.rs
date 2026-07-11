use std::env;

use anyhow::Context;
use glam::{DQuat, DVec3};
use mirante4d_core::{
    CameraView, DEFAULT_PRESENTATION_VIEWPORT_POINTS, GridToWorld, Projection, Shape3D,
    default_perspective_view_distance,
};
use mirante4d_data::DenseVolumeU16;
use mirante4d_renderer::RenderViewport;

pub(crate) const PHASE11_DEFAULT_MAX_VISIBLE_BRICKS: usize = 1024;
pub(crate) const PHASE11_DEFAULT_MAX_RESPONSIVE_VISIBLE_BRICKS: usize = 128;
const PHASE11_DEFAULT_GPU_VOLUME_CACHE_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const PHASE11_DEFAULT_GPU_BRICK_CACHE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub(crate) const PHASE11_GPU_MIP_BRICKS_PER_BATCH: usize = 64;
const PHASE11_DEFAULT_INTERACTION_STEPS_PER_SCENARIO: u64 = 5;

pub(crate) fn phase11_benchmark_viewport_for_shape(
    shape: Shape3D,
) -> anyhow::Result<RenderViewport> {
    let width = env_u64("MIRANTE4D_BENCH_VIEWPORT_WIDTH")?.unwrap_or(shape.x.min(1024));
    let height = env_u64("MIRANTE4D_BENCH_VIEWPORT_HEIGHT")?.unwrap_or(shape.y.min(1024));
    Ok(RenderViewport::new(width, height)?)
}

pub(crate) fn phase11_brick_pixel_stride(viewport: RenderViewport) -> anyhow::Result<u64> {
    Ok(env_u64("MIRANTE4D_BENCH_BRICK_PIXEL_STRIDE")?
        .unwrap_or_else(|| viewport.width.max(viewport.height).div_ceil(128).max(1))
        .max(1))
}

pub(crate) fn phase11_max_visible_bricks() -> anyhow::Result<usize> {
    let value = env_u64("MIRANTE4D_PHASE11_MAX_VISIBLE_BRICKS")?
        .unwrap_or(PHASE11_DEFAULT_MAX_VISIBLE_BRICKS as u64)
        .max(1);
    usize::try_from(value).context("MIRANTE4D_PHASE11_MAX_VISIBLE_BRICKS does not fit usize")
}

pub(crate) fn phase11_max_decoded_bytes() -> anyhow::Result<u64> {
    Ok(env_u64("MIRANTE4D_PHASE11_MAX_DECODED_BYTES")?
        .unwrap_or(PHASE11_DEFAULT_GPU_BRICK_CACHE_BYTES / 2)
        .max(1))
}

pub(crate) fn phase11_gpu_volume_cache_budget_bytes() -> anyhow::Result<u64> {
    Ok(env_u64("MIRANTE4D_PHASE11_GPU_VOLUME_CACHE_BYTES")?
        .unwrap_or(PHASE11_DEFAULT_GPU_VOLUME_CACHE_BYTES)
        .max(1))
}

pub(crate) fn phase11_gpu_brick_cache_budget_bytes() -> anyhow::Result<u64> {
    Ok(env_u64("MIRANTE4D_PHASE11_GPU_BRICK_CACHE_BYTES")?
        .unwrap_or(PHASE11_DEFAULT_GPU_BRICK_CACHE_BYTES)
        .max(1))
}

pub(crate) fn phase11_interaction_steps_per_scenario() -> anyhow::Result<u64> {
    Ok(env_u64("MIRANTE4D_PHASE11_INTERACTION_STEPS")?
        .unwrap_or(PHASE11_DEFAULT_INTERACTION_STEPS_PER_SCENARIO)
        .max(1))
}

pub(crate) fn env_u64(name: &str) -> anyhow::Result<Option<u64>> {
    match env::var(name) {
        Ok(raw) => raw
            .parse::<u64>()
            .map(Some)
            .with_context(|| format!("invalid {name}={raw:?}")),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(err) => Err(err).with_context(|| format!("failed to read {name}")),
    }
}

pub(crate) fn env_f64(name: &str) -> anyhow::Result<Option<f64>> {
    match env::var(name) {
        Ok(raw) => raw
            .parse::<f64>()
            .map(Some)
            .with_context(|| format!("invalid {name}={raw:?}")),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(err) => Err(err).with_context(|| format!("failed to read {name}")),
    }
}

pub(crate) fn benchmark_camera_for_volume(volume: &DenseVolumeU16) -> CameraView {
    benchmark_camera_for_shape(volume.shape, volume.grid_to_world)
}

pub(crate) fn benchmark_camera_for_shape(shape: Shape3D, grid_to_world: GridToWorld) -> CameraView {
    let width_world = grid_to_world
        .transform_vector(DVec3::X * shape.x as f64)
        .length();
    let height_world = grid_to_world
        .transform_vector(DVec3::Y * shape.y as f64)
        .length();
    let depth_world = grid_to_world
        .transform_vector(DVec3::Z * shape.z as f64)
        .length();
    let target = grid_to_world.transform_point(DVec3::new(
        (shape.x.saturating_sub(1)) as f64 * 0.5,
        (shape.y.saturating_sub(1)) as f64 * 0.5,
        (shape.z.saturating_sub(1)) as f64 * 0.5,
    ));
    let orthographic_world_per_screen_point =
        (width_world.max(height_world).max(depth_world).max(1.0) * 1.25)
            / DEFAULT_PRESENTATION_VIEWPORT_POINTS.height_points;
    let perspective_focal_length_screen_points = DEFAULT_PRESENTATION_VIEWPORT_POINTS.height_points
        / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan());
    let perspective_view_distance_world = default_perspective_view_distance(
        width_world,
        height_world,
        depth_world,
        Default::default(),
    );
    CameraView::new(
        Projection::Orthographic,
        target,
        DQuat::IDENTITY,
        orthographic_world_per_screen_point,
        perspective_focal_length_screen_points,
        perspective_view_distance_world,
    )
}

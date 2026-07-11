use std::env;

use anyhow::Context;
use glam::{DQuat, DVec3};
use mirante4d_data::DenseVolumeU16;
use mirante4d_domain::{CameraView, GridToWorld, Projection, Shape3D, UnitQuaternion, WorldPoint3};
use mirante4d_format::CurrentGridToWorldExt;
use mirante4d_render_api::{CameraFrame, PresentationViewport};
use mirante4d_renderer::RenderViewport;

pub(crate) const PHASE11_DEFAULT_MAX_VISIBLE_BRICKS: usize = 1024;
pub(crate) const PHASE11_DEFAULT_MAX_RESPONSIVE_VISIBLE_BRICKS: usize = 128;
const PHASE11_DEFAULT_GPU_VOLUME_CACHE_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const PHASE11_DEFAULT_GPU_BRICK_CACHE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const PHASE11_DEFAULT_INTERACTION_STEPS_PER_SCENARIO: u64 = 5;
pub(crate) const BENCHMARK_PRESENTATION_POINTS: f64 = 512.0;

pub(crate) fn benchmark_presentation() -> PresentationViewport {
    PresentationViewport::new(BENCHMARK_PRESENTATION_POINTS, BENCHMARK_PRESENTATION_POINTS)
        .expect("benchmark presentation dimensions are valid")
}

pub(crate) fn benchmark_camera_frame(view: CameraView) -> CameraFrame {
    CameraFrame::new(view, benchmark_presentation()).expect("benchmark camera frame is valid")
}

pub(crate) fn benchmark_camera_world_per_screen_point(view: CameraView) -> f64 {
    match view.projection() {
        Projection::Orthographic => view.orthographic_world_per_screen_point(),
        Projection::Perspective => (view.perspective_view_distance_world()
            / view.perspective_focal_length_screen_points())
        .max(1.0e-9),
    }
}

pub(crate) fn benchmark_camera_orbit(
    view: CameraView,
    horizontal_radians: f64,
    vertical_radians: f64,
) -> CameraView {
    let [x, y, z, w] = view.orientation().xyzw();
    let orientation = DQuat::from_xyzw(x, y, z, w)
        * (DQuat::from_rotation_y(-horizontal_radians) * DQuat::from_rotation_x(vertical_radians));
    rebuild_camera(
        view,
        view.target(),
        UnitQuaternion::new_xyzw(orientation.x, orientation.y, orientation.z, orientation.w)
            .expect("benchmark orbit is finite"),
        view.orthographic_world_per_screen_point(),
        view.perspective_focal_length_screen_points(),
    )
}

pub(crate) fn benchmark_camera_pan(
    view: CameraView,
    right_world: f64,
    up_world: f64,
) -> CameraView {
    let axes = benchmark_camera_frame(view).axes();
    let target = DVec3::from_array(view.target().components())
        + DVec3::from_array(axes.right()) * right_world
        + DVec3::from_array(axes.up()) * up_world;
    rebuild_camera(
        view,
        WorldPoint3::new(target.x, target.y, target.z).expect("benchmark pan is finite"),
        view.orientation(),
        view.orthographic_world_per_screen_point(),
        view.perspective_focal_length_screen_points(),
    )
}

pub(crate) fn benchmark_camera_zoom(view: CameraView, factor: f64) -> CameraView {
    let factor = factor.clamp(0.01, 100.0);
    let (orthographic_scale, perspective_focal_length) = match view.projection() {
        Projection::Orthographic => (
            (view.orthographic_world_per_screen_point() * factor).max(1.0e-9),
            view.perspective_focal_length_screen_points(),
        ),
        Projection::Perspective => (
            view.orthographic_world_per_screen_point(),
            (view.perspective_focal_length_screen_points() / factor).max(1.0e-9),
        ),
    };
    rebuild_camera(
        view,
        view.target(),
        view.orientation(),
        orthographic_scale,
        perspective_focal_length,
    )
}

fn rebuild_camera(
    view: CameraView,
    target: WorldPoint3,
    orientation: UnitQuaternion,
    orthographic_scale: f64,
    perspective_focal_length: f64,
) -> CameraView {
    CameraView::new(
        view.projection(),
        target,
        orientation,
        orthographic_scale,
        perspective_focal_length,
        view.perspective_view_distance_world(),
    )
    .expect("benchmark camera mutation preserves valid values")
}

pub(crate) fn phase11_benchmark_viewport_for_shape(
    shape: Shape3D,
) -> anyhow::Result<RenderViewport> {
    let width = env_u64("MIRANTE4D_BENCH_VIEWPORT_WIDTH")?.unwrap_or(shape.x().min(1024));
    let height = env_u64("MIRANTE4D_BENCH_VIEWPORT_HEIGHT")?.unwrap_or(shape.y().min(1024));
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
        .transform_vector(DVec3::X * shape.x() as f64)
        .length();
    let height_world = grid_to_world
        .transform_vector(DVec3::Y * shape.y() as f64)
        .length();
    let depth_world = grid_to_world
        .transform_vector(DVec3::Z * shape.z() as f64)
        .length();
    let target = grid_to_world.transform_point_vec(DVec3::new(
        (shape.x().saturating_sub(1)) as f64 * 0.5,
        (shape.y().saturating_sub(1)) as f64 * 0.5,
        (shape.z().saturating_sub(1)) as f64 * 0.5,
    ));
    let orthographic_world_per_screen_point =
        (width_world.max(height_world).max(depth_world).max(1.0) * 1.25)
            / BENCHMARK_PRESENTATION_POINTS;
    let perspective_focal_length_screen_points =
        BENCHMARK_PRESENTATION_POINTS / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan());
    let perspective_view_distance_world = (depth_world * 2.0)
        .max(DVec3::new(width_world, height_world, depth_world).length() * 1.25)
        .max(1.0);
    CameraView::new(
        Projection::Orthographic,
        WorldPoint3::new(target.x, target.y, target.z).expect("benchmark target is finite"),
        UnitQuaternion::identity(),
        orthographic_world_per_screen_point,
        perspective_focal_length_screen_points,
        perspective_view_distance_world,
    )
    .expect("benchmark camera values are valid")
}

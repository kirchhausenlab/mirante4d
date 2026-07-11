use eframe::egui;
use glam::{DQuat, DVec3};
use mirante4d_core::{
    CameraView, DEFAULT_PRESENTATION_VIEWPORT_POINTS, GridToWorld, IntensityDType,
    PresentationViewport, Projection, Shape3D,
};
use mirante4d_renderer::{
    CameraRenderQuality, IntensitySamplingPolicy, IsoShadingMode, MipImageF32, MipImageU16,
    RenderViewport,
};

use crate::{
    AppState, RenderIsoShadingPolicy, RenderMode, RenderSamplingPolicy, ViewportHover,
    ViewportIntensity, commands::WorkbenchCommand,
};

const DEFAULT_INITIAL_VIEWPORT_SIDE: u64 = 512;
const FIT_MARGIN: f64 = 1.25;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ViewportOrbitDragState {
    pub(crate) start_camera: CameraView,
}

pub(crate) fn default_camera_for_shape(shape: Shape3D, grid_to_world: GridToWorld) -> CameraView {
    let target = shape_center_world(shape, grid_to_world);
    let corners = shape_bounds_corners_world(shape, grid_to_world);
    fit_camera_to_world_bounds(
        Projection::Orthographic,
        target,
        DQuat::from_rotation_x(std::f64::consts::PI),
        &corners,
        DEFAULT_PRESENTATION_VIEWPORT_POINTS,
    )
}

pub(crate) fn fit_camera_to_shape_preserving_view(
    camera: CameraView,
    shape: Shape3D,
    grid_to_world: GridToWorld,
    presentation_viewport: PresentationViewport,
) -> CameraView {
    let target = shape_center_world(shape, grid_to_world);
    let corners = shape_bounds_corners_world(shape, grid_to_world);
    fit_camera_to_world_bounds(
        camera.projection,
        target,
        camera.orientation,
        &corners,
        presentation_viewport,
    )
}

pub(crate) fn default_render_viewport_for_shape(shape: Shape3D) -> anyhow::Result<RenderViewport> {
    let _ = shape;
    let width = DEFAULT_INITIAL_VIEWPORT_SIDE;
    let height = DEFAULT_INITIAL_VIEWPORT_SIDE;
    RenderViewport::new(width, height).map_err(Into::into)
}

pub(crate) fn default_presentation_viewport() -> PresentationViewport {
    DEFAULT_PRESENTATION_VIEWPORT_POINTS
}

pub(crate) fn presentation_viewport_for_display_size(
    display_size_points: egui::Vec2,
) -> Option<PresentationViewport> {
    PresentationViewport::new(
        f64::from(display_size_points.x),
        f64::from(display_size_points.y),
    )
    .ok()
}

pub(crate) fn render_viewport_for_display_size(
    display_size_points: egui::Vec2,
    pixels_per_point: f32,
    max_texture_side: usize,
) -> Option<RenderViewport> {
    if display_size_points.x <= 0.0
        || display_size_points.y <= 0.0
        || !display_size_points.x.is_finite()
        || !display_size_points.y.is_finite()
        || pixels_per_point <= 0.0
        || !pixels_per_point.is_finite()
        || max_texture_side == 0
    {
        return None;
    }
    let desired_width = f64::from(display_size_points.x * pixels_per_point).max(1.0);
    let desired_height = f64::from(display_size_points.y * pixels_per_point).max(1.0);
    let max_side = max_texture_side as f64;
    let scale = (max_side / desired_width.max(desired_height)).min(1.0);
    let width = (desired_width * scale).round().max(1.0) as u64;
    let height = (desired_height * scale).round().max(1.0) as u64;
    RenderViewport::new(width, height).ok()
}

fn shape_center_world(shape: Shape3D, grid_to_world: GridToWorld) -> DVec3 {
    grid_to_world.transform_point(DVec3::new(
        (shape.x.saturating_sub(1)) as f64 * 0.5,
        (shape.y.saturating_sub(1)) as f64 * 0.5,
        (shape.z.saturating_sub(1)) as f64 * 0.5,
    ))
}

fn shape_bounds_corners_world(shape: Shape3D, grid_to_world: GridToWorld) -> [DVec3; 8] {
    let min_x = -0.5;
    let min_y = -0.5;
    let min_z = -0.5;
    let max_x = shape.x as f64 - 0.5;
    let max_y = shape.y as f64 - 0.5;
    let max_z = shape.z as f64 - 0.5;
    [
        grid_to_world.transform_point(DVec3::new(min_x, min_y, min_z)),
        grid_to_world.transform_point(DVec3::new(max_x, min_y, min_z)),
        grid_to_world.transform_point(DVec3::new(min_x, max_y, min_z)),
        grid_to_world.transform_point(DVec3::new(max_x, max_y, min_z)),
        grid_to_world.transform_point(DVec3::new(min_x, min_y, max_z)),
        grid_to_world.transform_point(DVec3::new(max_x, min_y, max_z)),
        grid_to_world.transform_point(DVec3::new(min_x, max_y, max_z)),
        grid_to_world.transform_point(DVec3::new(max_x, max_y, max_z)),
    ]
}

fn fit_camera_to_world_bounds(
    projection: Projection,
    target: DVec3,
    orientation: DQuat,
    corners: &[DVec3; 8],
    presentation_viewport: PresentationViewport,
) -> CameraView {
    let fit_width_points = (presentation_viewport.width_points / FIT_MARGIN).max(1.0);
    let fit_height_points = (presentation_viewport.height_points / FIT_MARGIN).max(1.0);
    let axes = CameraView::new(projection, target, orientation, 1.0, 1.0, 1.0).axes();

    let mut max_abs_right = 0.0_f64;
    let mut max_abs_up = 0.0_f64;
    let mut min_depth = f64::INFINITY;
    let mut max_depth = f64::NEG_INFINITY;
    let mut max_pair_distance = 0.0_f64;
    for corner in corners {
        let from_target = *corner - target;
        max_abs_right = max_abs_right.max(from_target.dot(axes.right).abs());
        max_abs_up = max_abs_up.max(from_target.dot(axes.up).abs());
        let depth = from_target.dot(axes.forward);
        min_depth = min_depth.min(depth);
        max_depth = max_depth.max(depth);
        for other in corners {
            max_pair_distance = max_pair_distance.max(corner.distance(*other));
        }
    }

    let orthographic_world_per_screen_point = (max_abs_right / (fit_width_points * 0.5))
        .max(max_abs_up / (fit_height_points * 0.5))
        .max(1.0e-9);
    let bounds_depth_along_view = (max_depth - min_depth).abs();
    let perspective_view_distance_world = (bounds_depth_along_view * 2.0)
        .max(max_pair_distance * 1.25)
        .max(1.0);
    let eye = target - axes.forward * perspective_view_distance_world;
    let mut max_abs_projected_x_at_focal_1 = 0.0_f64;
    let mut max_abs_projected_y_at_focal_1 = 0.0_f64;
    for corner in corners {
        let from_eye = *corner - eye;
        let depth = from_eye.dot(axes.forward).max(1.0e-9);
        max_abs_projected_x_at_focal_1 =
            max_abs_projected_x_at_focal_1.max((from_eye.dot(axes.right) / depth).abs());
        max_abs_projected_y_at_focal_1 =
            max_abs_projected_y_at_focal_1.max((from_eye.dot(axes.up) / depth).abs());
    }
    let focal_limit_x = focal_limit_for_axis(fit_width_points, max_abs_projected_x_at_focal_1);
    let focal_limit_y = focal_limit_for_axis(fit_height_points, max_abs_projected_y_at_focal_1);
    let mut perspective_focal_length_screen_points = focal_limit_x.min(focal_limit_y);
    if !perspective_focal_length_screen_points.is_finite() {
        perspective_focal_length_screen_points = fit_width_points.min(fit_height_points);
    }
    perspective_focal_length_screen_points = perspective_focal_length_screen_points.max(1.0e-9);

    CameraView::new(
        projection,
        target,
        orientation,
        orthographic_world_per_screen_point,
        perspective_focal_length_screen_points,
        perspective_view_distance_world,
    )
}

fn focal_limit_for_axis(fit_points: f64, max_abs_projected_at_focal_1: f64) -> f64 {
    if max_abs_projected_at_focal_1 <= 1.0e-12 {
        f64::INFINITY
    } else {
        (fit_points * 0.5) / max_abs_projected_at_focal_1
    }
}

pub(crate) fn viewport_hover_from_response(
    state: &AppState,
    response: &egui::Response,
) -> Option<ViewportHover> {
    let position = response.hover_pos()?;
    viewport_hover_from_image_point(
        &state.frame,
        state.frame_f32.as_ref(),
        state.active_layer_dtype,
        response.rect,
        position,
    )
}

pub(crate) fn viewport_hover_from_image_point(
    frame: &MipImageU16,
    frame_f32: Option<&MipImageF32>,
    active_layer_dtype: IntensityDType,
    rect: egui::Rect,
    position: egui::Pos2,
) -> Option<ViewportHover> {
    if frame.width == 0 || frame.height == 0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    if !rect.contains(position) {
        return None;
    }
    let normalized_x = ((position.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
    let normalized_y = ((position.y - rect.min.y) / rect.height()).clamp(0.0, 1.0);
    viewport_hover_from_normalized_point(
        frame,
        frame_f32,
        active_layer_dtype,
        normalized_x,
        normalized_y,
    )
}

pub(crate) fn viewport_hover_from_normalized_point(
    frame: &MipImageU16,
    frame_f32: Option<&MipImageF32>,
    active_layer_dtype: IntensityDType,
    normalized_x: f32,
    normalized_y: f32,
) -> Option<ViewportHover> {
    if frame.width == 0
        || frame.height == 0
        || !normalized_x.is_finite()
        || !normalized_y.is_finite()
    {
        return None;
    }
    let normalized_x = normalized_x.clamp(0.0, 1.0);
    let normalized_y = normalized_y.clamp(0.0, 1.0);
    let x = ((normalized_x * frame.width as f32).floor() as u64).min(frame.width - 1);
    let y = ((normalized_y * frame.height as f32).floor() as u64).min(frame.height - 1);
    Some(ViewportHover {
        x,
        y,
        intensity: frame_f32
            .and_then(|frame| frame.pixel(y, x))
            .map(ViewportIntensity::F32)
            .unwrap_or_else(|| {
                let value = frame.pixel(y, x).unwrap_or(0);
                match active_layer_dtype {
                    IntensityDType::Uint8 => {
                        ViewportIntensity::U8(u8::try_from(value).unwrap_or(u8::MAX))
                    }
                    IntensityDType::Uint16 | IntensityDType::Float32 => {
                        ViewportIntensity::U16(value)
                    }
                }
            }),
    })
}

pub(crate) fn viewport_interaction_commands(
    state: &mut AppState,
    response: &egui::Response,
    viewport_size: egui::Vec2,
) -> Vec<WorkbenchCommand> {
    let mut commands = Vec::new();
    if response.drag_stopped() {
        state.viewport_orbit_drag = None;
    }
    if response.dragged() {
        let camera_pan_requested = response.ctx.input(|input| {
            input.pointer.middle_down() || input.pointer.secondary_down() || input.modifiers.shift
        });
        if camera_pan_requested {
            state.viewport_orbit_drag = None;
        }
        if let Some(command) =
            viewport_drag_command(state, response, viewport_size, camera_pan_requested)
        {
            commands.push(command);
        }
    }

    if response.hovered() {
        let scroll_y = response.ctx.input(|input| input.smooth_scroll_delta().y);
        if scroll_y != 0.0
            && let Some(command) = viewport_scroll_command(scroll_y)
        {
            commands.push(command);
        }
    }
    commands
}

pub(crate) fn viewport_drag_command(
    state: &mut AppState,
    response: &egui::Response,
    viewport_size_points: egui::Vec2,
    camera_pan_requested: bool,
) -> Option<WorkbenchCommand> {
    if viewport_size_points.x <= 0.0
        || viewport_size_points.y <= 0.0
        || !viewport_size_points.x.is_finite()
        || !viewport_size_points.y.is_finite()
    {
        return None;
    }
    if camera_pan_requested {
        let motion_points = response.drag_motion();
        if !motion_points.x.is_finite() || !motion_points.y.is_finite() {
            return None;
        }
        return Some(WorkbenchCommand::CameraPanDrag { motion_points });
    }

    let current_pointer = response.interact_pointer_pos()?;
    let total_drag_delta = response.total_drag_delta()?;
    if !current_pointer.x.is_finite()
        || !current_pointer.y.is_finite()
        || !total_drag_delta.x.is_finite()
        || !total_drag_delta.y.is_finite()
    {
        return None;
    }
    let drag_state = state
        .viewport_orbit_drag
        .get_or_insert(ViewportOrbitDragState {
            start_camera: state.camera,
        });
    let current_position_points = current_pointer - response.rect.min.to_vec2();
    let start_position_points = current_position_points - total_drag_delta;
    Some(WorkbenchCommand::CameraOrbitDrag {
        start_camera: drag_state.start_camera,
        start_position_points,
        current_position_points,
        viewport_size_points,
    })
}

pub(crate) fn viewport_scroll_command(scroll_y_points: f32) -> Option<WorkbenchCommand> {
    if scroll_y_points == 0.0 || !scroll_y_points.is_finite() {
        return None;
    }
    Some(WorkbenchCommand::CameraZoom { scroll_y_points })
}

pub(crate) fn apply_camera_pan(camera: &mut CameraView, motion_points: egui::Vec2) {
    let world_per_point = camera.world_per_screen_point_at_target();
    camera.pan_by(
        -f64::from(motion_points.x) * world_per_point,
        f64::from(motion_points.y) * world_per_point,
    );
}

pub(crate) fn apply_camera_orbit(
    camera: &mut CameraView,
    start_camera: CameraView,
    start_position_points: egui::Pos2,
    current_position_points: egui::Pos2,
    viewport_size_points: egui::Vec2,
) {
    camera.orbit_arcball(
        f64::from(start_position_points.x),
        f64::from(start_position_points.y),
        f64::from(current_position_points.x),
        f64::from(current_position_points.y),
        f64::from(viewport_size_points.x),
        f64::from(viewport_size_points.y),
        start_camera,
    );
}

pub(crate) fn apply_camera_zoom(camera: &mut CameraView, scroll_y_points: f32) {
    let factor = (-f64::from(scroll_y_points) * 0.001).exp();
    camera.zoom_by(factor);
}

pub(crate) fn camera_render_quality_for_render_state(
    render_state: crate::ChannelRenderState,
) -> CameraRenderQuality {
    CameraRenderQuality {
        intensity_sampling: match render_state.sampling_policy() {
            RenderSamplingPolicy::VoxelExact => IntensitySamplingPolicy::VoxelExact,
            RenderSamplingPolicy::SmoothLinear => IntensitySamplingPolicy::SmoothLinear,
        },
        iso_shading: match render_state.iso_shading_policy() {
            RenderIsoShadingPolicy::Flat => IsoShadingMode::Flat,
            RenderIsoShadingPolicy::GradientLighting => IsoShadingMode::GradientLighting,
        },
    }
}

pub(crate) fn camera_render_quality(state: &AppState) -> CameraRenderQuality {
    camera_render_quality_for_render_state(
        crate::layer_state::active_layer_render_state_from_runtime(state),
    )
}

pub(crate) fn resident_brick_render_supported(mode: RenderMode) -> bool {
    matches!(
        mode,
        RenderMode::Mip | RenderMode::Isosurface | RenderMode::Dvr
    )
}

pub(crate) fn fit_size(image_size: egui::Vec2, available: egui::Vec2) -> egui::Vec2 {
    if image_size.x <= 0.0 || image_size.y <= 0.0 || available.x <= 0.0 || available.y <= 0.0 {
        return egui::Vec2::ZERO;
    }
    let scale = (available.x / image_size.x).min(available.y / image_size.y);
    image_size * scale
}

use eframe::egui;
use glam::{DQuat, DVec3};
use mirante4d_application::{
    ApplicationCommand, ApplicationSnapshot, ViewState,
    viewport_interaction::{ViewportOrbitDrag, orbit_camera, pan_camera, zoom_camera},
};
use mirante4d_domain::{CameraView, GridToWorld, Projection, Shape3D, UnitQuaternion, WorldPoint3};
use mirante4d_render_api::{
    CameraFrame, DEFAULT_PRESENTATION_VIEWPORT, PresentationViewport, RenderExtent,
};

use mirante4d_ui_egui::{EguiUiState, ViewportHover};

const DEFAULT_INITIAL_VIEWPORT_SIDE: u32 = 512;
const FIT_MARGIN: f64 = 1.25;

pub(crate) fn default_camera_for_shape(shape: Shape3D, grid_to_world: GridToWorld) -> CameraView {
    let target = shape_center_world(shape, grid_to_world);
    let corners = shape_bounds_corners_world(shape, grid_to_world);
    fit_camera_to_world_bounds(
        Projection::Orthographic,
        target,
        DQuat::from_rotation_x(std::f64::consts::PI),
        &corners,
        DEFAULT_PRESENTATION_VIEWPORT,
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
        camera.projection(),
        target,
        dquat(camera.orientation()),
        &corners,
        presentation_viewport,
    )
}

pub(crate) fn default_render_viewport_for_shape(shape: Shape3D) -> anyhow::Result<RenderExtent> {
    let _ = shape;
    let width = DEFAULT_INITIAL_VIEWPORT_SIDE;
    let height = DEFAULT_INITIAL_VIEWPORT_SIDE;
    RenderExtent::new(width, height).map_err(Into::into)
}

pub(crate) fn default_presentation_viewport() -> PresentationViewport {
    DEFAULT_PRESENTATION_VIEWPORT
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
) -> Option<RenderExtent> {
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
    let max_side = max_texture_side.min(u32::MAX as usize) as f64;
    let scale = (max_side / desired_width.max(desired_height)).min(1.0);
    let width = (desired_width * scale).round().max(1.0) as u32;
    let height = (desired_height * scale).round().max(1.0) as u32;
    RenderExtent::new(width, height).ok()
}

fn shape_center_world(shape: Shape3D, grid_to_world: GridToWorld) -> DVec3 {
    transform_grid_point(
        grid_to_world,
        DVec3::new(
            (shape.x().saturating_sub(1)) as f64 * 0.5,
            (shape.y().saturating_sub(1)) as f64 * 0.5,
            (shape.z().saturating_sub(1)) as f64 * 0.5,
        ),
    )
}

fn shape_bounds_corners_world(shape: Shape3D, grid_to_world: GridToWorld) -> [DVec3; 8] {
    let min_x = -0.5;
    let min_y = -0.5;
    let min_z = -0.5;
    let max_x = shape.x() as f64 - 0.5;
    let max_y = shape.y() as f64 - 0.5;
    let max_z = shape.z() as f64 - 0.5;
    [
        transform_grid_point(grid_to_world, DVec3::new(min_x, min_y, min_z)),
        transform_grid_point(grid_to_world, DVec3::new(max_x, min_y, min_z)),
        transform_grid_point(grid_to_world, DVec3::new(min_x, max_y, min_z)),
        transform_grid_point(grid_to_world, DVec3::new(max_x, max_y, min_z)),
        transform_grid_point(grid_to_world, DVec3::new(min_x, min_y, max_z)),
        transform_grid_point(grid_to_world, DVec3::new(max_x, min_y, max_z)),
        transform_grid_point(grid_to_world, DVec3::new(min_x, max_y, max_z)),
        transform_grid_point(grid_to_world, DVec3::new(max_x, max_y, max_z)),
    ]
}

fn transform_grid_point(grid_to_world: GridToWorld, grid_point: DVec3) -> DVec3 {
    let grid_point = WorldPoint3::new(grid_point.x, grid_point.y, grid_point.z)
        .expect("shape-derived grid point is finite");
    dvec3(
        grid_to_world
            .transform_point(grid_point)
            .expect("validated grid transform maps the shape to finite world coordinates"),
    )
}

fn fit_camera_to_world_bounds(
    projection: Projection,
    target: DVec3,
    orientation: DQuat,
    corners: &[DVec3; 8],
    presentation_viewport: PresentationViewport,
) -> CameraView {
    let fit_width_points = (presentation_viewport.width_points() / FIT_MARGIN).max(1.0);
    let fit_height_points = (presentation_viewport.height_points() / FIT_MARGIN).max(1.0);
    let provisional = CameraView::new(
        projection,
        world_point(target),
        unit_quaternion(orientation),
        1.0,
        1.0,
        1.0,
    )
    .expect("camera fit inputs are finite and positive");
    let axes = CameraFrame::new(provisional, presentation_viewport)
        .expect("camera fit inputs produce finite axes")
        .axes();
    let right = DVec3::from_array(axes.right());
    let up = DVec3::from_array(axes.up());
    let forward = DVec3::from_array(axes.forward());

    let mut max_abs_right = 0.0_f64;
    let mut max_abs_up = 0.0_f64;
    let mut min_depth = f64::INFINITY;
    let mut max_depth = f64::NEG_INFINITY;
    let mut max_pair_distance = 0.0_f64;
    for corner in corners {
        let from_target = *corner - target;
        max_abs_right = max_abs_right.max(from_target.dot(right).abs());
        max_abs_up = max_abs_up.max(from_target.dot(up).abs());
        let depth = from_target.dot(forward);
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
    let eye = target - forward * perspective_view_distance_world;
    let mut max_abs_projected_x_at_focal_1 = 0.0_f64;
    let mut max_abs_projected_y_at_focal_1 = 0.0_f64;
    for corner in corners {
        let from_eye = *corner - eye;
        let depth = from_eye.dot(forward).max(1.0e-9);
        max_abs_projected_x_at_focal_1 =
            max_abs_projected_x_at_focal_1.max((from_eye.dot(right) / depth).abs());
        max_abs_projected_y_at_focal_1 =
            max_abs_projected_y_at_focal_1.max((from_eye.dot(up) / depth).abs());
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
        world_point(target),
        unit_quaternion(orientation),
        orthographic_world_per_screen_point,
        perspective_focal_length_screen_points,
        perspective_view_distance_world,
    )
    .expect("camera fit derives finite positive framing")
}

fn focal_limit_for_axis(fit_points: f64, max_abs_projected_at_focal_1: f64) -> f64 {
    if max_abs_projected_at_focal_1 <= 1.0e-12 {
        f64::INFINITY
    } else {
        (fit_points * 0.5) / max_abs_projected_at_focal_1
    }
}

pub(crate) fn viewport_hover_from_response(
    _snapshot: &ApplicationSnapshot,
    _view: &ViewState,
    _response: &egui::Response,
) -> Option<ViewportHover> {
    // The current product path retains only a GPU presentation texture.
    // `frame` is always a loading placeholder, including before the first GPU
    // frame, so 3D scientific hover remains unavailable rather than guessed.
    None
}

pub(crate) fn viewport_interaction_commands(
    egui_ui: &mut EguiUiState,
    view: &ViewState,
    response: &egui::Response,
    viewport_size: egui::Vec2,
) -> Vec<ApplicationCommand> {
    let mut commands = Vec::new();
    if response.drag_stopped() {
        egui_ui.viewport_orbit_drag = None;
    }
    if response.dragged() {
        let camera_pan_requested = response.ctx.input(|input| {
            input.pointer.middle_down() || input.pointer.secondary_down() || input.modifiers.shift
        });
        if camera_pan_requested {
            egui_ui.viewport_orbit_drag = None;
        }
        if let Some(command) = viewport_drag_command(
            egui_ui,
            *view.camera(),
            response,
            viewport_size,
            camera_pan_requested,
        ) {
            commands.push(command);
        }
    }

    if response.hovered() {
        let scroll_y = response.ctx.input(|input| input.smooth_scroll_delta().y);
        if scroll_y != 0.0
            && let Some(command) = viewport_scroll_command(*view.camera(), scroll_y)
        {
            commands.push(command);
        }
    }
    commands
}

pub(crate) fn viewport_drag_command(
    egui_ui: &mut EguiUiState,
    camera: CameraView,
    response: &egui::Response,
    viewport_size_points: egui::Vec2,
    camera_pan_requested: bool,
) -> Option<ApplicationCommand> {
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
        let camera = pan_camera(camera, [motion_points.x, motion_points.y]);
        return Some(ApplicationCommand::SetCamera(camera));
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
    let drag_state = egui_ui
        .viewport_orbit_drag
        .get_or_insert(ViewportOrbitDrag::new(camera));
    let current_position_points = current_pointer - response.rect.min.to_vec2();
    let start_position_points = current_position_points - total_drag_delta;
    let camera = orbit_camera(
        drag_state.start_camera(),
        [start_position_points.x, start_position_points.y],
        [current_position_points.x, current_position_points.y],
        [viewport_size_points.x, viewport_size_points.y],
    );
    Some(ApplicationCommand::SetCamera(camera))
}

pub(crate) fn viewport_scroll_command(
    camera: CameraView,
    scroll_y_points: f32,
) -> Option<ApplicationCommand> {
    if scroll_y_points == 0.0 || !scroll_y_points.is_finite() {
        return None;
    }
    let camera = zoom_camera(camera, scroll_y_points);
    Some(ApplicationCommand::SetCamera(camera))
}

fn dvec3(point: WorldPoint3) -> DVec3 {
    DVec3::from_array(point.components())
}

fn world_point(point: DVec3) -> WorldPoint3 {
    WorldPoint3::new(point.x, point.y, point.z).expect("interaction math produced a finite point")
}

fn dquat(quaternion: UnitQuaternion) -> DQuat {
    DQuat::from_array(quaternion.xyzw())
}

fn unit_quaternion(quaternion: DQuat) -> UnitQuaternion {
    let [x, y, z, w] = quaternion.to_array();
    UnitQuaternion::new_xyzw(x, y, z, w)
        .expect("interaction math produced a finite nonzero quaternion")
}

pub(crate) fn fit_size(image_size: egui::Vec2, available: egui::Vec2) -> egui::Vec2 {
    if image_size.x <= 0.0 || image_size.y <= 0.0 || available.x <= 0.0 || available.y <= 0.0 {
        return egui::Vec2::ZERO;
    }
    let scale = (available.x / image_size.x).min(available.y / image_size.y);
    image_size * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_camera_targets_the_affine_world_center() {
        let shape = Shape3D::new(7, 5, 3).unwrap();
        let grid_to_world = GridToWorld::from_row_major([
            2.0, 0.0, 0.0, 10.0, 0.0, 3.0, 0.0, 20.0, 0.0, 0.0, 4.0, 30.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .unwrap();

        let camera = default_camera_for_shape(shape, grid_to_world);

        assert_eq!(camera.target().components(), [12.0, 26.0, 42.0]);
    }
}

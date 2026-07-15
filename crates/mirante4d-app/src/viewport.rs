use eframe::egui;
#[cfg(test)]
use mirante4d_application::viewport_interaction::default_camera_for_shape;
use mirante4d_application::{
    ApplicationCommand, ApplicationSnapshot, ViewState,
    viewport_interaction::{ViewportOrbitDrag, orbit_camera, pan_camera, zoom_camera},
};
#[cfg(test)]
use mirante4d_domain::GridToWorld;
use mirante4d_domain::{CameraView, Shape3D};
use mirante4d_render_api::{DEFAULT_PRESENTATION_VIEWPORT, PresentationViewport, RenderExtent};

use mirante4d_ui_egui::{EguiUiState, ViewportHover};

const DEFAULT_INITIAL_VIEWPORT_SIDE: u32 = 512;

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

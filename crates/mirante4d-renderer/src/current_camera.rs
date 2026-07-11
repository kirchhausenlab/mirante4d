//! Operational helpers for the predecessor renderer over the canonical
//! `mirante4d-render-api` camera frame.

use glam::DVec3;
use mirante4d_domain::{IsoLightState, Projection, WorldPoint3};
use mirante4d_render_api::{CameraAxes, CameraFrame};

use crate::{RenderError, RenderViewport};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CurrentViewRay {
    pub(crate) origin: DVec3,
    pub(crate) direction: DVec3,
}

pub(crate) fn point(value: WorldPoint3) -> DVec3 {
    DVec3::from_array(value.components())
}

pub(crate) fn vector(value: [f64; 3]) -> DVec3 {
    DVec3::from_array(value)
}

pub(crate) fn eye(camera: CameraFrame) -> DVec3 {
    point(camera.eye())
}

pub(crate) fn target(camera: CameraFrame) -> DVec3 {
    point(camera.view().target())
}

pub(crate) fn forward(camera: CameraFrame) -> DVec3 {
    vector(camera.axes().forward())
}

pub(crate) fn right(camera: CameraFrame) -> DVec3 {
    vector(camera.axes().right())
}

pub(crate) fn up(camera: CameraFrame) -> DVec3 {
    vector(camera.axes().up())
}

pub(crate) fn projection(camera: CameraFrame) -> Projection {
    camera.view().projection()
}

pub(crate) fn orthographic_world_per_screen_point(camera: CameraFrame) -> f64 {
    camera.view().orthographic_world_per_screen_point()
}

pub(crate) fn perspective_focal_length_screen_points(camera: CameraFrame) -> f64 {
    camera.view().perspective_focal_length_screen_points()
}

pub(crate) fn presentation_width_points(camera: CameraFrame) -> f64 {
    camera.presentation().width_points()
}

pub(crate) fn presentation_height_points(camera: CameraFrame) -> f64 {
    camera.presentation().height_points()
}

pub(crate) fn ray_for_render_pixel(
    camera: CameraFrame,
    pixel_x: f64,
    pixel_y: f64,
    viewport: RenderViewport,
) -> Result<CurrentViewRay, RenderError> {
    let width = u32::try_from(viewport.width).map_err(|_| RenderError::DimensionTooLarge {
        axis: "viewport_width",
        value: viewport.width,
    })?;
    let height = u32::try_from(viewport.height).map_err(|_| RenderError::DimensionTooLarge {
        axis: "viewport_height",
        value: viewport.height,
    })?;
    let ray = camera.ray_for_render_pixel(pixel_x, pixel_y, width, height)?;
    Ok(CurrentViewRay {
        origin: point(ray.origin()),
        direction: vector(ray.direction()),
    })
}

pub(crate) fn axes_vectors(axes: CameraAxes) -> (DVec3, DVec3, DVec3) {
    (
        vector(axes.forward()),
        vector(axes.right()),
        vector(axes.up()),
    )
}

pub(crate) fn iso_light_direction(light: IsoLightState, axes: CameraAxes) -> DVec3 {
    let (forward, right, up) = axes_vectors(axes);
    match light.detached_screen_position() {
        None => -forward,
        Some([x, y]) => {
            let radius_squared = (x * x + y * y).min(1.0);
            let z = (1.0 - radius_squared).sqrt();
            right * f64::from(x) + up * f64::from(y) - forward * f64::from(z)
        }
    }
    .normalize_or_zero()
}

#[cfg(test)]
pub(crate) fn frame_from_look_at(
    projection: Projection,
    eye: DVec3,
    target: DVec3,
    up_hint: DVec3,
    orthographic_world_per_screen_point: f64,
    perspective_focal_length_screen_points: f64,
    presentation: mirante4d_render_api::PresentationViewport,
) -> CameraFrame {
    use glam::{DMat3, DQuat};
    use mirante4d_domain::{CameraView, UnitQuaternion};

    let forward = (target - eye).normalize_or_zero();
    let forward = if forward.length_squared() == 0.0 {
        -DVec3::Z
    } else {
        forward
    };
    let right = forward.cross(up_hint).normalize_or_zero();
    let right = if right.length_squared() == 0.0 {
        DVec3::X
    } else {
        right
    };
    let up = right.cross(forward).normalize();
    let orientation = DQuat::from_mat3(&DMat3::from_cols(right, up, -forward));
    let orientation =
        UnitQuaternion::new_xyzw(orientation.x, orientation.y, orientation.z, orientation.w)
            .expect("look-at basis produces a finite unit quaternion");
    let view = CameraView::new(
        projection,
        WorldPoint3::new(target.x, target.y, target.z).expect("test target is finite"),
        orientation,
        orthographic_world_per_screen_point,
        perspective_focal_length_screen_points,
        eye.distance(target).max(1.0e-9),
    )
    .expect("test camera values are valid");
    CameraFrame::new(view, presentation).expect("test camera frame is valid")
}

#[cfg(test)]
pub(crate) fn presentation(
    width_points: f64,
    height_points: f64,
) -> mirante4d_render_api::PresentationViewport {
    mirante4d_render_api::PresentationViewport::new(width_points, height_points)
        .expect("test presentation dimensions are valid")
}

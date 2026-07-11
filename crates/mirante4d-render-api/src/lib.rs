//! Framework-neutral camera projection values and view-ray math.
//!
//! This preparatory boundary deliberately contains no renderer requirements,
//! scheduling, resource leases, frame lifecycle, presentation backend, GPU,
//! UI, serialization, or I/O contract. Those broader contracts belong to
//! WP-08A and later packages.

#![forbid(unsafe_code)]

use mirante4d_domain::{CameraView, Projection, UnitQuaternion, WorldPoint3};
use thiserror::Error;

pub const DEFAULT_PRESENTATION_VIEWPORT: PresentationViewport =
    PresentationViewport::new_unchecked(512.0, 512.0);

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RenderApiError {
    #[error("presentation viewport dimensions must be finite and positive")]
    InvalidPresentationViewport,
    #[error("screen-point coordinates must be finite")]
    NonFiniteScreenPoint,
    #[error("render extent dimensions must be nonzero")]
    InvalidRenderExtent,
    #[error("render-pixel coordinates must be finite")]
    NonFiniteRenderPixel,
    #[error("camera projection math produced a non-finite value")]
    CameraMathNotFinite,
    #[error("camera projection math produced a zero-length direction")]
    DegenerateViewDirection,
}

/// The logical presentation size in UI-independent screen points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresentationViewport {
    width_points: f64,
    height_points: f64,
}

impl PresentationViewport {
    const fn new_unchecked(width_points: f64, height_points: f64) -> Self {
        Self {
            width_points,
            height_points,
        }
    }

    pub fn new(width_points: f64, height_points: f64) -> Result<Self, RenderApiError> {
        if !is_finite_positive(width_points) || !is_finite_positive(height_points) {
            return Err(RenderApiError::InvalidPresentationViewport);
        }
        Ok(Self::new_unchecked(width_points, height_points))
    }

    pub const fn width_points(self) -> f64 {
        self.width_points
    }

    pub const fn height_points(self) -> f64 {
        self.height_points
    }
}

/// Orthonormal world-space axes derived from a canonical camera orientation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraAxes {
    forward: [f64; 3],
    right: [f64; 3],
    up: [f64; 3],
}

impl CameraAxes {
    pub const fn forward(self) -> [f64; 3] {
        self.forward
    }

    pub const fn right(self) -> [f64; 3] {
        self.right
    }

    pub const fn up(self) -> [f64; 3] {
        self.up
    }
}

/// A finite world-space ray with a unit-length direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewRay {
    origin: WorldPoint3,
    direction: [f64; 3],
}

impl ViewRay {
    pub const fn origin(self) -> WorldPoint3 {
        self.origin
    }

    pub const fn direction(self) -> [f64; 3] {
        self.direction
    }
}

/// Operational projection facts derived from one canonical durable view.
///
/// The canonical `CameraView` remains the authority. This value only combines
/// it with the current presentation extent and provides deterministic math.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraFrame {
    view: CameraView,
    presentation: PresentationViewport,
    axes: CameraAxes,
    eye: WorldPoint3,
}

impl CameraFrame {
    pub fn new(
        view: CameraView,
        presentation: PresentationViewport,
    ) -> Result<Self, RenderApiError> {
        let axes = axes_from_orientation(view.orientation())?;
        let target = Vec3::from_array(view.target().components());
        let eye = target.checked_sub(
            Vec3::from_array(axes.forward).checked_mul(view.perspective_view_distance_world())?,
        )?;
        Ok(Self {
            view,
            presentation,
            axes,
            eye: eye.to_world_point()?,
        })
    }

    pub const fn view(self) -> CameraView {
        self.view
    }

    pub const fn presentation(self) -> PresentationViewport {
        self.presentation
    }

    pub const fn axes(self) -> CameraAxes {
        self.axes
    }

    pub const fn eye(self) -> WorldPoint3 {
        self.eye
    }

    pub fn ray_for_screen_point(
        self,
        screen_x_points: f64,
        screen_y_points: f64,
    ) -> Result<ViewRay, RenderApiError> {
        if !screen_x_points.is_finite() || !screen_y_points.is_finite() {
            return Err(RenderApiError::NonFiniteScreenPoint);
        }

        let forward = Vec3::from_array(self.axes.forward);
        let right = Vec3::from_array(self.axes.right);
        let up = Vec3::from_array(self.axes.up);
        match self.view.projection() {
            Projection::Perspective => {
                let focal_length = self.view.perspective_focal_length_screen_points();
                let direction = forward
                    .checked_add(right.checked_mul(screen_x_points / focal_length)?)?
                    .checked_add(up.checked_mul(screen_y_points / focal_length)?)?
                    .normalized()?;
                Ok(ViewRay {
                    origin: self.eye,
                    direction: direction.0,
                })
            }
            Projection::Orthographic => {
                let scale = self.view.orthographic_world_per_screen_point();
                let origin = Vec3::from_array(self.eye.components())
                    .checked_add(right.checked_mul(screen_x_points * scale)?)?
                    .checked_add(up.checked_mul(screen_y_points * scale)?)?
                    .to_world_point()?;
                Ok(ViewRay {
                    origin,
                    direction: forward.0,
                })
            }
        }
    }

    /// Maps a physical render pixel center into presentation points before
    /// deriving its ray. Pixel coordinates may be outside the render extent so
    /// callers can deliberately evaluate border samples; they must be finite.
    pub fn ray_for_render_pixel(
        self,
        pixel_x: f64,
        pixel_y: f64,
        render_width: u32,
        render_height: u32,
    ) -> Result<ViewRay, RenderApiError> {
        if render_width == 0 || render_height == 0 {
            return Err(RenderApiError::InvalidRenderExtent);
        }
        if !pixel_x.is_finite() || !pixel_y.is_finite() {
            return Err(RenderApiError::NonFiniteRenderPixel);
        }
        let screen_x_points =
            (((pixel_x + 0.5) / f64::from(render_width)) - 0.5) * self.presentation.width_points;
        let screen_y_points =
            (0.5 - ((pixel_y + 0.5) / f64::from(render_height))) * self.presentation.height_points;
        if !screen_x_points.is_finite() || !screen_y_points.is_finite() {
            return Err(RenderApiError::CameraMathNotFinite);
        }
        self.ray_for_screen_point(screen_x_points, screen_y_points)
    }

    pub fn orthographic_world_span_width(self) -> Result<f64, RenderApiError> {
        checked_scalar(
            self.presentation.width_points * self.view.orthographic_world_per_screen_point(),
        )
    }

    pub fn orthographic_world_span_height(self) -> Result<f64, RenderApiError> {
        checked_scalar(
            self.presentation.height_points * self.view.orthographic_world_per_screen_point(),
        )
    }

    pub fn perspective_vertical_fov_radians(self) -> Result<f64, RenderApiError> {
        let ratio = (self.presentation.height_points * 0.5)
            / self.view.perspective_focal_length_screen_points();
        checked_scalar(2.0 * ratio.atan())
    }

    pub fn world_per_screen_point_at_target(self) -> Result<f64, RenderApiError> {
        match self.view.projection() {
            Projection::Orthographic => Ok(self.view.orthographic_world_per_screen_point()),
            Projection::Perspective => checked_scalar(
                self.view.perspective_view_distance_world()
                    / self.view.perspective_focal_length_screen_points(),
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Vec3([f64; 3]);

impl Vec3 {
    const X: Self = Self([1.0, 0.0, 0.0]);
    const Y: Self = Self([0.0, 1.0, 0.0]);
    const NEG_Z: Self = Self([0.0, 0.0, -1.0]);

    const fn from_array(value: [f64; 3]) -> Self {
        Self(value)
    }

    fn checked_add(self, other: Self) -> Result<Self, RenderApiError> {
        Self::checked([
            self.0[0] + other.0[0],
            self.0[1] + other.0[1],
            self.0[2] + other.0[2],
        ])
    }

    fn checked_sub(self, other: Self) -> Result<Self, RenderApiError> {
        Self::checked([
            self.0[0] - other.0[0],
            self.0[1] - other.0[1],
            self.0[2] - other.0[2],
        ])
    }

    fn checked_mul(self, scalar: f64) -> Result<Self, RenderApiError> {
        if !scalar.is_finite() {
            return Err(RenderApiError::CameraMathNotFinite);
        }
        Self::checked([self.0[0] * scalar, self.0[1] * scalar, self.0[2] * scalar])
    }

    fn checked(value: [f64; 3]) -> Result<Self, RenderApiError> {
        if value.iter().all(|component| component.is_finite()) {
            Ok(Self(value.map(canonical_zero)))
        } else {
            Err(RenderApiError::CameraMathNotFinite)
        }
    }

    fn cross(self, other: Self) -> Self {
        Self([
            self.0[1] * other.0[2] - self.0[2] * other.0[1],
            self.0[2] * other.0[0] - self.0[0] * other.0[2],
            self.0[0] * other.0[1] - self.0[1] * other.0[0],
        ])
    }

    fn normalized(self) -> Result<Self, RenderApiError> {
        if !self.0.iter().all(|component| component.is_finite()) {
            return Err(RenderApiError::CameraMathNotFinite);
        }
        let scale = self.0.iter().map(|value| value.abs()).fold(0.0, f64::max);
        if scale == 0.0 {
            return Err(RenderApiError::DegenerateViewDirection);
        }
        let scaled = self.0.map(|value| value / scale);
        let length = scaled.iter().map(|value| value * value).sum::<f64>().sqrt();
        Self::checked(scaled.map(|value| value / length))
    }

    fn to_world_point(self) -> Result<WorldPoint3, RenderApiError> {
        WorldPoint3::new(self.0[0], self.0[1], self.0[2])
            .map_err(|_| RenderApiError::CameraMathNotFinite)
    }
}

fn axes_from_orientation(orientation: UnitQuaternion) -> Result<CameraAxes, RenderApiError> {
    let right = rotate(orientation, Vec3::X)?.normalized()?;
    let up = rotate(orientation, Vec3::Y)?.normalized()?;
    let forward = rotate(orientation, Vec3::NEG_Z)?.normalized()?;
    Ok(CameraAxes {
        forward: forward.0,
        right: right.0,
        up: up.0,
    })
}

fn rotate(quaternion: UnitQuaternion, vector: Vec3) -> Result<Vec3, RenderApiError> {
    let [x, y, z, w] = quaternion.xyzw();
    let imaginary = Vec3([x, y, z]);
    let twice_cross = imaginary.cross(vector).checked_mul(2.0)?;
    vector
        .checked_add(twice_cross.checked_mul(w)?)?
        .checked_add(imaginary.cross(twice_cross))
}

fn is_finite_positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

fn checked_scalar(value: f64) -> Result<f64, RenderApiError> {
    if value.is_finite() {
        Ok(canonical_zero(value))
    } else {
        Err(RenderApiError::CameraMathNotFinite)
    }
}

fn canonical_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f64 = 1.0e-12;

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= EPSILON,
            "expected {expected}, got {actual}"
        );
    }

    fn camera(projection: Projection) -> CameraFrame {
        let view = CameraView::new(
            projection,
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            1.0,
            8.0,
            10.0,
        )
        .unwrap();
        CameraFrame::new(view, PresentationViewport::new(8.0, 8.0).unwrap()).unwrap()
    }

    #[test]
    fn presentation_viewport_rejects_nonpositive_or_nonfinite_dimensions() {
        assert_eq!(
            PresentationViewport::new(0.0, 1.0),
            Err(RenderApiError::InvalidPresentationViewport)
        );
        assert_eq!(
            PresentationViewport::new(1.0, f64::NAN),
            Err(RenderApiError::InvalidPresentationViewport)
        );
        assert_eq!(
            PresentationViewport::new(f64::INFINITY, 1.0),
            Err(RenderApiError::InvalidPresentationViewport)
        );
    }

    #[test]
    fn canonical_identity_orientation_defines_expected_axes_and_eye() {
        let camera = camera(Projection::Orthographic);
        assert_eq!(camera.axes().right(), [1.0, 0.0, 0.0]);
        assert_eq!(camera.axes().up(), [0.0, 1.0, 0.0]);
        assert_eq!(camera.axes().forward(), [0.0, 0.0, -1.0]);
        assert_eq!(camera.eye().components(), [0.0, 0.0, 10.0]);
    }

    #[test]
    fn quarter_turn_camera_orientation_has_known_axes_and_eye() {
        let half_angle = std::f64::consts::FRAC_PI_4;
        let orientation =
            UnitQuaternion::new_xyzw(0.0, half_angle.sin(), 0.0, half_angle.cos()).unwrap();
        let view = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::origin(),
            orientation,
            1.0,
            8.0,
            10.0,
        )
        .unwrap();
        let camera = CameraFrame::new(view, PresentationViewport::new(8.0, 8.0).unwrap()).unwrap();

        for (actual, expected) in camera.axes().right().into_iter().zip([0.0, 0.0, -1.0]) {
            assert_close(actual, expected);
        }
        for (actual, expected) in camera.axes().up().into_iter().zip([0.0, 1.0, 0.0]) {
            assert_close(actual, expected);
        }
        for (actual, expected) in camera.axes().forward().into_iter().zip([-1.0, 0.0, 0.0]) {
            assert_close(actual, expected);
        }
        for (actual, expected) in camera.eye().components().into_iter().zip([10.0, 0.0, 0.0]) {
            assert_close(actual, expected);
        }
    }

    #[test]
    fn orthographic_rays_are_parallel_with_screen_shifted_origins() {
        let camera = camera(Projection::Orthographic);
        let center = camera.ray_for_screen_point(0.0, 0.0).unwrap();
        let corner = camera.ray_for_screen_point(4.0, 4.0).unwrap();

        assert_eq!(center.direction(), [0.0, 0.0, -1.0]);
        assert_eq!(corner.direction(), center.direction());
        assert_eq!(center.origin().components(), [0.0, 0.0, 10.0]);
        assert_eq!(corner.origin().components(), [4.0, 4.0, 10.0]);
    }

    #[test]
    fn perspective_rays_diverge_from_one_eye() {
        let camera = camera(Projection::Perspective);
        let center = camera.ray_for_screen_point(0.0, 0.0).unwrap();
        let corner = camera.ray_for_screen_point(4.0, 4.0).unwrap();

        assert_eq!(center.origin(), corner.origin());
        assert_eq!(center.direction(), [0.0, 0.0, -1.0]);
        assert_ne!(center.direction(), corner.direction());
        let direction = corner.direction();
        assert_close(
            direction
                .iter()
                .map(|component| component * component)
                .sum::<f64>(),
            1.0,
        );
    }

    #[test]
    fn render_pixel_centers_map_y_opposite_camera_up() {
        let camera = camera(Projection::Orthographic);
        let top = camera.ray_for_render_pixel(1.0, 0.0, 4, 4).unwrap();
        let bottom = camera.ray_for_render_pixel(1.0, 3.0, 4, 4).unwrap();
        assert!(top.origin().y() > bottom.origin().y());
    }

    #[test]
    fn projection_measurements_use_canonical_view_values() {
        let orthographic = camera(Projection::Orthographic);
        assert_close(orthographic.orthographic_world_span_width().unwrap(), 8.0);
        assert_close(orthographic.orthographic_world_span_height().unwrap(), 8.0);
        assert_close(
            orthographic.world_per_screen_point_at_target().unwrap(),
            1.0,
        );

        let perspective = camera(Projection::Perspective);
        assert_close(
            perspective.perspective_vertical_fov_radians().unwrap(),
            2.0 * 0.5_f64.atan(),
        );
        assert_close(
            perspective.world_per_screen_point_at_target().unwrap(),
            1.25,
        );
    }

    #[test]
    fn invalid_queries_and_nonfinite_results_fail_explicitly() {
        let camera = camera(Projection::Orthographic);
        assert_eq!(
            camera.ray_for_screen_point(f64::NAN, 0.0),
            Err(RenderApiError::NonFiniteScreenPoint)
        );
        assert_eq!(
            camera.ray_for_render_pixel(0.0, 0.0, 0, 4),
            Err(RenderApiError::InvalidRenderExtent)
        );
        assert_eq!(
            camera.ray_for_render_pixel(f64::INFINITY, 0.0, 4, 4),
            Err(RenderApiError::NonFiniteRenderPixel)
        );

        let extreme = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            f64::MAX,
            1.0,
            1.0,
        )
        .unwrap();
        let extreme =
            CameraFrame::new(extreme, PresentationViewport::new(f64::MAX, 1.0).unwrap()).unwrap();
        assert_eq!(
            extreme.orthographic_world_span_width(),
            Err(RenderApiError::CameraMathNotFinite)
        );
        assert_eq!(
            extreme.ray_for_screen_point(f64::MAX, 0.0),
            Err(RenderApiError::CameraMathNotFinite)
        );
    }
}

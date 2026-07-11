use glam::{DQuat, DVec3};
use serde::{Deserialize, Serialize};

pub const DEFAULT_PRESENTATION_VIEWPORT_POINTS: PresentationViewport =
    PresentationViewport::new_unchecked(512.0, 512.0);

const MIN_CAMERA_SCALE: f64 = 1.0e-9;
const MIN_VIEW_DISTANCE: f64 = 1.0e-9;
const MIN_ORIENTATION_LENGTH_SQUARED: f64 = 1.0e-18;
const ARCBALL_EPSILON: f64 = 1.0e-12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Projection {
    Perspective,
    Orthographic,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CameraState {
    pub projection: Projection,
    pub eye: DVec3,
    pub target: DVec3,
    pub up: DVec3,
    pub orthographic_world_per_screen_point: f64,
    pub perspective_focal_length_screen_points: f64,
    pub presentation_width_points: f64,
    pub presentation_height_points: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PresentationViewport {
    pub width_points: f64,
    pub height_points: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationViewportError {
    InvalidDimensions,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ViewRay {
    pub origin: DVec3,
    pub direction: DVec3,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CameraView {
    pub projection: Projection,
    pub target: DVec3,
    pub orientation: DQuat,
    pub orthographic_world_per_screen_point: f64,
    pub perspective_focal_length_screen_points: f64,
    pub perspective_view_distance_world: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CameraAxes {
    pub forward: DVec3,
    pub right: DVec3,
    pub up: DVec3,
}

impl CameraState {
    pub fn new(
        projection: Projection,
        eye: DVec3,
        target: DVec3,
        up: DVec3,
        orthographic_world_per_screen_point: f64,
        perspective_focal_length_screen_points: f64,
        presentation: PresentationViewport,
    ) -> Self {
        Self {
            projection,
            eye,
            target,
            up,
            orthographic_world_per_screen_point: orthographic_world_per_screen_point
                .max(MIN_CAMERA_SCALE),
            perspective_focal_length_screen_points: perspective_focal_length_screen_points
                .max(MIN_CAMERA_SCALE),
            presentation_width_points: presentation.width_points,
            presentation_height_points: presentation.height_points,
        }
    }

    pub fn presentation(self) -> PresentationViewport {
        PresentationViewport::new_unchecked(
            self.presentation_width_points,
            self.presentation_height_points,
        )
    }

    pub fn ray_for_render_pixel(
        self,
        pixel_x: f64,
        pixel_y: f64,
        viewport_width: f64,
        viewport_height: f64,
    ) -> ViewRay {
        let screen_x_points =
            (((pixel_x + 0.5) / viewport_width.max(1.0)) - 0.5) * self.presentation_width_points;
        let screen_y_points =
            (0.5 - ((pixel_y + 0.5) / viewport_height.max(1.0))) * self.presentation_height_points;
        self.ray_for_screen_point(screen_x_points, screen_y_points)
    }

    pub fn ray_for_screen_point(self, screen_x_points: f64, screen_y_points: f64) -> ViewRay {
        let forward = (self.target - self.eye).normalize();
        let right = forward.cross(self.up).normalize();
        let up = right.cross(forward).normalize();

        match self.projection {
            Projection::Perspective => {
                let direction = (forward
                    + right * (screen_x_points / self.perspective_focal_length_screen_points)
                    + up * (screen_y_points / self.perspective_focal_length_screen_points))
                    .normalize();
                ViewRay {
                    origin: self.eye,
                    direction,
                }
            }
            Projection::Orthographic => ViewRay {
                origin: self.eye
                    + right * (screen_x_points * self.orthographic_world_per_screen_point)
                    + up * (screen_y_points * self.orthographic_world_per_screen_point),
                direction: forward,
            },
        }
    }

    pub fn orthographic_world_span_height(self) -> f64 {
        self.presentation_height_points * self.orthographic_world_per_screen_point
    }

    pub fn orthographic_world_span_width(self) -> f64 {
        self.presentation_width_points * self.orthographic_world_per_screen_point
    }

    pub fn perspective_vertical_fov_radians(self) -> f64 {
        2.0 * ((self.presentation_height_points * 0.5)
            / self.perspective_focal_length_screen_points)
            .atan()
    }
}

impl PresentationViewport {
    pub const fn new_unchecked(width_points: f64, height_points: f64) -> Self {
        Self {
            width_points,
            height_points,
        }
    }

    pub fn new(width_points: f64, height_points: f64) -> Result<Self, PresentationViewportError> {
        if width_points <= 0.0
            || height_points <= 0.0
            || !width_points.is_finite()
            || !height_points.is_finite()
        {
            return Err(PresentationViewportError::InvalidDimensions);
        }
        Ok(Self {
            width_points,
            height_points,
        })
    }
}

impl CameraView {
    pub fn new(
        projection: Projection,
        target: DVec3,
        orientation: DQuat,
        orthographic_world_per_screen_point: f64,
        perspective_focal_length_screen_points: f64,
        perspective_view_distance_world: f64,
    ) -> Self {
        Self {
            projection,
            target,
            orientation: normalized_orientation_or_identity(orientation),
            orthographic_world_per_screen_point: orthographic_world_per_screen_point
                .max(MIN_CAMERA_SCALE),
            perspective_focal_length_screen_points: perspective_focal_length_screen_points
                .max(MIN_CAMERA_SCALE),
            perspective_view_distance_world: perspective_view_distance_world.max(MIN_VIEW_DISTANCE),
        }
    }

    pub fn default_for_bounds(width_world: f64, height_world: f64, depth_world: f64) -> Self {
        let max_world_extent = width_world.max(height_world).max(depth_world).max(1.0);
        let orthographic_world_per_screen_point =
            (max_world_extent * 1.25) / DEFAULT_PRESENTATION_VIEWPORT_POINTS.height_points;
        let perspective_view_distance_world = default_perspective_view_distance(
            width_world,
            height_world,
            depth_world,
            CameraAxes::default(),
        );
        let perspective_focal_length_screen_points = DEFAULT_PRESENTATION_VIEWPORT_POINTS
            .height_points
            / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan());
        Self::new(
            Projection::Orthographic,
            DVec3::new(width_world * 0.5, height_world * 0.5, depth_world * 0.5),
            DQuat::from_rotation_x(std::f64::consts::PI),
            orthographic_world_per_screen_point,
            perspective_focal_length_screen_points,
            perspective_view_distance_world,
        )
    }

    pub fn axes(self) -> CameraAxes {
        let orientation = normalized_orientation_or_identity(self.orientation);
        let right = (orientation * DVec3::X).normalize();
        let up = (orientation * DVec3::Y).normalize();
        let forward = (orientation * -DVec3::Z).normalize();
        CameraAxes { forward, right, up }
    }

    pub fn to_camera_state(self, presentation: PresentationViewport) -> CameraState {
        let axes = self.axes();
        CameraState::new(
            self.projection,
            self.target - axes.forward * self.perspective_view_distance_world,
            self.target,
            axes.up,
            self.orthographic_world_per_screen_point,
            self.perspective_focal_length_screen_points,
            presentation,
        )
    }

    pub fn set_projection(&mut self, projection: Projection) {
        self.projection = projection;
    }

    pub fn reset_projection_preserving_framing(&mut self, projection: Projection) {
        self.set_projection(projection);
    }

    pub fn orbit_by(&mut self, delta_horizontal_radians: f64, delta_vertical_radians: f64) {
        if !delta_horizontal_radians.is_finite() || !delta_vertical_radians.is_finite() {
            return;
        }
        let delta = DQuat::from_rotation_y(-delta_horizontal_radians)
            * DQuat::from_rotation_x(delta_vertical_radians);
        self.orientation = normalized_orientation_or_identity(self.orientation * delta);
    }

    pub fn orbit_arcball(
        &mut self,
        start_x_points: f64,
        start_y_points: f64,
        current_x_points: f64,
        current_y_points: f64,
        viewport_width_points: f64,
        viewport_height_points: f64,
        start_camera: CameraView,
    ) {
        let Some(delta) = arcball_delta(
            start_x_points,
            start_y_points,
            current_x_points,
            current_y_points,
            viewport_width_points,
            viewport_height_points,
        ) else {
            return;
        };
        self.orientation = normalized_orientation_or_identity(start_camera.orientation * delta);
        self.target = start_camera.target;
        self.projection = start_camera.projection;
        self.orthographic_world_per_screen_point = start_camera.orthographic_world_per_screen_point;
        self.perspective_focal_length_screen_points =
            start_camera.perspective_focal_length_screen_points;
        self.perspective_view_distance_world = start_camera.perspective_view_distance_world;
    }

    pub fn pan_by(&mut self, right_world: f64, up_world: f64) {
        let axes = self.axes();
        self.target += axes.right * right_world + axes.up * up_world;
    }

    pub fn zoom_by(&mut self, factor: f64) {
        let factor = factor.clamp(0.01, 100.0);
        match self.projection {
            Projection::Orthographic => {
                self.orthographic_world_per_screen_point =
                    (self.orthographic_world_per_screen_point * factor).max(MIN_CAMERA_SCALE);
            }
            Projection::Perspective => {
                self.perspective_focal_length_screen_points =
                    (self.perspective_focal_length_screen_points / factor).max(MIN_CAMERA_SCALE);
            }
        }
    }

    pub fn dolly_by(&mut self, distance_delta: f64) {
        self.perspective_view_distance_world =
            (self.perspective_view_distance_world + distance_delta).max(MIN_VIEW_DISTANCE);
    }

    pub fn world_per_screen_point_at_target(self) -> f64 {
        match self.projection {
            Projection::Orthographic => self.orthographic_world_per_screen_point,
            Projection::Perspective => (self.perspective_view_distance_world
                / self.perspective_focal_length_screen_points)
                .max(MIN_CAMERA_SCALE),
        }
    }
}

fn normalized_orientation_or_identity(orientation: DQuat) -> DQuat {
    if !orientation.is_finite() {
        return DQuat::IDENTITY;
    }
    let length_squared = orientation.length_squared();
    if !length_squared.is_finite() || length_squared <= MIN_ORIENTATION_LENGTH_SQUARED {
        DQuat::IDENTITY
    } else {
        orientation.normalize()
    }
}

fn arcball_delta(
    start_x_points: f64,
    start_y_points: f64,
    current_x_points: f64,
    current_y_points: f64,
    viewport_width_points: f64,
    viewport_height_points: f64,
) -> Option<DQuat> {
    if viewport_width_points <= 0.0
        || viewport_height_points <= 0.0
        || !viewport_width_points.is_finite()
        || !viewport_height_points.is_finite()
    {
        return None;
    }
    let start = arcball_vector(
        start_x_points,
        start_y_points,
        viewport_width_points,
        viewport_height_points,
    )?;
    let current = arcball_vector(
        current_x_points,
        current_y_points,
        viewport_width_points,
        viewport_height_points,
    )?;
    if start.abs_diff_eq(current, ARCBALL_EPSILON) {
        return None;
    }
    Some(DQuat::from_rotation_arc(current, start).normalize())
}

fn arcball_vector(
    x_points: f64,
    y_points: f64,
    viewport_width_points: f64,
    viewport_height_points: f64,
) -> Option<DVec3> {
    if !x_points.is_finite() || !y_points.is_finite() {
        return None;
    }
    let side = viewport_width_points.min(viewport_height_points);
    if side <= 0.0 || !side.is_finite() {
        return None;
    }
    let x = 2.0 * (x_points - viewport_width_points * 0.5) / side;
    let y = -2.0 * (y_points - viewport_height_points * 0.5) / side;
    let radius_squared = x * x + y * y;
    let vector = if radius_squared <= 1.0 {
        DVec3::new(x, y, (1.0 - radius_squared).sqrt())
    } else {
        DVec3::new(x, y, 0.0)
    };
    let length_squared = vector.length_squared();
    if !length_squared.is_finite() || length_squared <= ARCBALL_EPSILON {
        None
    } else {
        Some(vector.normalize())
    }
}

impl Default for CameraAxes {
    fn default() -> Self {
        Self {
            forward: DVec3::Z,
            right: DVec3::X,
            up: -DVec3::Y,
        }
    }
}

pub fn default_perspective_view_distance(
    width_world: f64,
    height_world: f64,
    depth_world: f64,
    axes: CameraAxes,
) -> f64 {
    let extents = [
        DVec3::new(0.0, 0.0, 0.0),
        DVec3::new(width_world, 0.0, 0.0),
        DVec3::new(0.0, height_world, 0.0),
        DVec3::new(width_world, height_world, 0.0),
        DVec3::new(0.0, 0.0, depth_world),
        DVec3::new(width_world, 0.0, depth_world),
        DVec3::new(0.0, height_world, depth_world),
        DVec3::new(width_world, height_world, depth_world),
    ];
    let center = DVec3::new(width_world * 0.5, height_world * 0.5, depth_world * 0.5);
    let mut min_depth = f64::INFINITY;
    let mut max_depth = f64::NEG_INFINITY;
    for corner in extents {
        let depth = (corner - center).dot(axes.forward);
        min_depth = min_depth.min(depth);
        max_depth = max_depth.max(depth);
    }
    let depth_extent = (max_depth - min_depth).abs();
    let diagonal = DVec3::new(width_world, height_world, depth_world).length();
    (depth_extent * 2.0).max(diagonal * 1.25).max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;
    use proptest::prelude::*;
    use proptest::test_runner::RngSeed;

    fn presentation(width_points: f64, height_points: f64) -> PresentationViewport {
        PresentationViewport::new(width_points, height_points).unwrap()
    }

    fn camera(projection: Projection) -> CameraState {
        CameraState::new(
            projection,
            DVec3::new(0.0, 0.0, 10.0),
            DVec3::ZERO,
            DVec3::Y,
            1.0,
            8.0,
            presentation(8.0, 8.0),
        )
    }

    fn assert_quat_abs_diff_eq(left: DQuat, right: DQuat, epsilon: f64) {
        assert_abs_diff_eq!(left.x, right.x, epsilon = epsilon);
        assert_abs_diff_eq!(left.y, right.y, epsilon = epsilon);
        assert_abs_diff_eq!(left.z, right.z, epsilon = epsilon);
        assert_abs_diff_eq!(left.w, right.w, epsilon = epsilon);
    }

    fn assert_orthonormal(axes: CameraAxes) {
        assert_abs_diff_eq!(axes.forward.length(), 1.0, epsilon = 1e-12);
        assert_abs_diff_eq!(axes.right.length(), 1.0, epsilon = 1e-12);
        assert_abs_diff_eq!(axes.up.length(), 1.0, epsilon = 1e-12);
        assert_abs_diff_eq!(axes.forward.dot(axes.right), 0.0, epsilon = 1e-12);
        assert_abs_diff_eq!(axes.forward.dot(axes.up), 0.0, epsilon = 1e-12);
        assert_abs_diff_eq!(axes.right.dot(axes.up), 0.0, epsilon = 1e-12);
    }

    #[test]
    fn orthographic_rays_are_parallel() {
        let camera = camera(Projection::Orthographic);
        let center = camera.ray_for_screen_point(0.0, 0.0);
        let corner = camera.ray_for_screen_point(4.0, 4.0);

        assert_abs_diff_eq!(center.direction.x, corner.direction.x, epsilon = 1e-12);
        assert_abs_diff_eq!(center.direction.y, corner.direction.y, epsilon = 1e-12);
        assert_abs_diff_eq!(center.direction.z, corner.direction.z, epsilon = 1e-12);
        assert_ne!(center.origin, corner.origin);
    }

    #[test]
    fn perspective_rays_diverge_from_common_origin() {
        let camera = camera(Projection::Perspective);
        let center = camera.ray_for_screen_point(0.0, 0.0);
        let corner = camera.ray_for_screen_point(4.0, 4.0);

        assert_eq!(center.origin, corner.origin);
        assert_ne!(center.direction, corner.direction);
    }

    #[test]
    fn projection_switch_preserves_target_orientation_and_scale_fields() {
        let orientation = (DQuat::from_rotation_y(-0.4) * DQuat::from_rotation_x(-0.2)).normalize();
        let mut view = CameraView::new(
            Projection::Orthographic,
            DVec3::new(1.0, 2.0, 3.0),
            orientation,
            0.25,
            320.0,
            80.0,
        );

        view.set_projection(Projection::Perspective);

        assert_eq!(view.projection, Projection::Perspective);
        assert_abs_diff_eq!(view.target.x, 1.0, epsilon = 1e-12);
        assert_abs_diff_eq!(view.target.y, 2.0, epsilon = 1e-12);
        assert_abs_diff_eq!(view.target.z, 3.0, epsilon = 1e-12);
        assert_quat_abs_diff_eq(view.orientation, orientation, 1e-12);
        assert_abs_diff_eq!(
            view.orthographic_world_per_screen_point,
            0.25,
            epsilon = 1e-12
        );
        assert_abs_diff_eq!(
            view.perspective_focal_length_screen_points,
            320.0,
            epsilon = 1e-12
        );
        assert_abs_diff_eq!(view.perspective_view_distance_world, 80.0, epsilon = 1e-12);
    }

    #[test]
    fn orthographic_dolly_does_not_change_projected_scale() {
        let mut view = CameraView::new(
            Projection::Orthographic,
            DVec3::ZERO,
            (DQuat::from_rotation_y(-0.3) * DQuat::from_rotation_x(0.2)).normalize(),
            0.125,
            320.0,
            40.0,
        );
        let before = horizontal_ray_span(view.to_camera_state(presentation(96.0, 64.0)), 1.5);

        view.dolly_by(100.0);
        let after = horizontal_ray_span(view.to_camera_state(presentation(96.0, 64.0)), 1.5);

        assert_abs_diff_eq!(after, before, epsilon = 1e-12);
    }

    #[test]
    fn orthographic_zoom_changes_projected_scale() {
        let mut view = CameraView::new(
            Projection::Orthographic,
            DVec3::ZERO,
            DQuat::IDENTITY,
            0.125,
            320.0,
            40.0,
        );
        let before = horizontal_ray_span(view.to_camera_state(presentation(64.0, 64.0)), 1.0);

        view.zoom_by(0.5);
        let after = horizontal_ray_span(view.to_camera_state(presentation(64.0, 64.0)), 1.0);

        assert_abs_diff_eq!(after, before * 0.5, epsilon = 1e-12);
    }

    #[test]
    fn orthographic_resize_changes_visible_extent_not_scale() {
        let view = CameraView::new(
            Projection::Orthographic,
            DVec3::ZERO,
            DQuat::IDENTITY,
            2.0,
            320.0,
            40.0,
        );
        let short = view.to_camera_state(presentation(100.0, 100.0));
        let tall = view.to_camera_state(presentation(100.0, 200.0));

        assert_abs_diff_eq!(
            short.orthographic_world_per_screen_point,
            tall.orthographic_world_per_screen_point,
            epsilon = 1e-12
        );
        assert_abs_diff_eq!(
            short.orthographic_world_span_height(),
            200.0,
            epsilon = 1e-12
        );
        assert_abs_diff_eq!(
            tall.orthographic_world_span_height(),
            400.0,
            epsilon = 1e-12
        );
    }

    #[test]
    fn perspective_resize_keeps_eye_distance_and_focal_length() {
        let view = CameraView::new(
            Projection::Perspective,
            DVec3::ZERO,
            DQuat::IDENTITY,
            2.0,
            320.0,
            40.0,
        );
        let short = view.to_camera_state(presentation(100.0, 100.0));
        let tall = view.to_camera_state(presentation(100.0, 200.0));

        assert_eq!(short.eye, tall.eye);
        assert_abs_diff_eq!(
            short.perspective_focal_length_screen_points,
            tall.perspective_focal_length_screen_points,
            epsilon = 1e-12
        );
        assert!(tall.perspective_vertical_fov_radians() > short.perspective_vertical_fov_radians());
    }

    #[test]
    fn identity_orientation_defines_camera_axes() {
        let axes = CameraView::new(
            Projection::Orthographic,
            DVec3::ZERO,
            DQuat::IDENTITY,
            1.0,
            1.0,
            1.0,
        )
        .axes();

        assert_abs_diff_eq!(axes.right.x, 1.0, epsilon = 1e-12);
        assert_abs_diff_eq!(axes.up.y, 1.0, epsilon = 1e-12);
        assert_abs_diff_eq!(axes.forward.z, -1.0, epsilon = 1e-12);
        assert_orthonormal(axes);
    }

    #[test]
    fn default_for_bounds_uses_fiji_front_axes() {
        let axes = CameraView::default_for_bounds(16.0, 16.0, 16.0).axes();

        assert_abs_diff_eq!(axes.right.x, 1.0, epsilon = 1e-12);
        assert_abs_diff_eq!(axes.up.y, -1.0, epsilon = 1e-12);
        assert_abs_diff_eq!(axes.forward.z, 1.0, epsilon = 1e-12);
        assert_orthonormal(axes);
    }

    #[test]
    fn render_pixel_y_increases_opposite_camera_up() {
        let camera = CameraState::new(
            Projection::Orthographic,
            DVec3::new(0.0, 0.0, -10.0),
            DVec3::ZERO,
            DVec3::Y,
            1.0,
            8.0,
            presentation(4.0, 4.0),
        );

        let top = camera.ray_for_render_pixel(1.0, 0.0, 4.0, 4.0);
        let bottom = camera.ray_for_render_pixel(1.0, 3.0, 4.0, 4.0);

        assert!(top.origin.y > bottom.origin.y);
    }

    #[test]
    fn constructor_normalizes_orientation() {
        let view = CameraView::new(
            Projection::Orthographic,
            DVec3::ZERO,
            DQuat::from_xyzw(0.0, 2.0, 0.0, 0.0),
            1.0,
            320.0,
            40.0,
        );

        assert_abs_diff_eq!(view.orientation.length_squared(), 1.0, epsilon = 1e-12);
        assert_orthonormal(view.axes());
    }

    #[test]
    fn programmatic_orbit_preserves_normalized_orientation() {
        let mut view = CameraView::default_for_bounds(16.0, 16.0, 16.0);

        for _ in 0..256 {
            view.orbit_by(0.03, -0.02);
        }

        assert_abs_diff_eq!(view.orientation.length_squared(), 1.0, epsilon = 1e-12);
        assert_orthonormal(view.axes());
    }

    #[test]
    fn arcball_drag_right_turns_default_view_toward_positive_x() {
        let start = CameraView::default_for_bounds(16.0, 16.0, 16.0);
        let mut view = start;

        view.orbit_arcball(50.0, 50.0, 75.0, 50.0, 100.0, 100.0, start);

        assert!(view.axes().forward.x > 0.0);
        assert_abs_diff_eq!(view.orientation.length_squared(), 1.0, epsilon = 1e-12);
    }

    #[test]
    fn arcball_drag_up_turns_fiji_default_view_toward_negative_y() {
        let start = CameraView::default_for_bounds(16.0, 16.0, 16.0);
        let mut view = start;

        view.orbit_arcball(50.0, 50.0, 50.0, 25.0, 100.0, 100.0, start);

        assert!(view.axes().forward.y < 0.0);
        assert_abs_diff_eq!(view.orientation.length_squared(), 1.0, epsilon = 1e-12);
    }

    fn horizontal_ray_span(camera: CameraState, _aspect: f64) -> f64 {
        let half_width_points = camera.presentation_width_points * 0.5;
        let left = camera.ray_for_screen_point(-half_width_points, 0.0);
        let right = camera.ray_for_screen_point(half_width_points, 0.0);
        left.origin.distance(right.origin)
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 64,
            max_shrink_iters: 1_024,
            failure_persistence: None,
            rng_seed: RngSeed::Fixed(0x4d34_4341_4d45_5241),
            .. ProptestConfig::default()
        })]

        #[test]
        fn orthographic_rays_are_parallel_for_all_viewport_points(
            ax in -1.0_f64..1.0,
            ay in -1.0_f64..1.0,
            bx in -1.0_f64..1.0,
            by in -1.0_f64..1.0,
            _aspect in 0.1_f64..10.0,
        ) {
            let camera = camera(Projection::Orthographic);
            let a = camera.ray_for_screen_point(ax * 4.0, ay * 4.0);
            let b = camera.ray_for_screen_point(bx * 4.0, by * 4.0);

            prop_assert!((a.direction - b.direction).length() <= 1.0e-12);
            prop_assert!((a.direction.length() - 1.0).abs() <= 1.0e-12);
            prop_assert!((b.direction.length() - 1.0).abs() <= 1.0e-12);
            prop_assert!(a.origin.x.is_finite() && a.origin.y.is_finite() && a.origin.z.is_finite());
            prop_assert!(b.origin.x.is_finite() && b.origin.y.is_finite() && b.origin.z.is_finite());
        }
    }
}

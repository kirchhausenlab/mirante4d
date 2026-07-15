//! Framework-neutral camera and linked cross-section interaction geometry.

use glam::{DQuat, DVec3};
use mirante4d_domain::{
    CameraView, CrossSectionView, GeometryError, GridToWorld, Projection, Shape3D, UnitQuaternion,
    ViewError, WorldPoint3,
};
use mirante4d_render_api::{CameraFrame, DEFAULT_PRESENTATION_VIEWPORT, PresentationViewport};

use crate::{ApplicationSnapshot, ViewState};

const CROSS_SECTION_EPSILON: f64 = 1.0e-9;
const MIN_ORIENTATION_LENGTH_SQUARED: f64 = 1.0e-18;
const CAMERA_FIT_MARGIN: f64 = 1.25;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CrossSectionPanel {
    Xy,
    Xz,
    Yz,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct CrossSectionBasis {
    right_world: DVec3,
    down_world: DVec3,
    normal_away_world: DVec3,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrossSectionPanelView {
    center_world: DVec3,
    basis: CrossSectionBasis,
    scale_world_per_screen_point: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrossSectionViewState {
    center_world: DVec3,
    orientation: DQuat,
    scale_world_per_screen_point: f64,
    depth_world: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CrossSectionInteractionError {
    Geometry(GeometryError),
    View(ViewError),
}

impl std::fmt::Display for CrossSectionInteractionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Geometry(error) => write!(formatter, "invalid cross-section geometry: {error}"),
            Self::View(error) => write!(formatter, "invalid cross-section view: {error}"),
        }
    }
}

impl std::error::Error for CrossSectionInteractionError {}

impl From<GeometryError> for CrossSectionInteractionError {
    fn from(error: GeometryError) -> Self {
        Self::Geometry(error)
    }
}

impl From<ViewError> for CrossSectionInteractionError {
    fn from(error: ViewError) -> Self {
        Self::View(error)
    }
}

impl CrossSectionPanel {
    fn basis(self, cross_section_orientation: DQuat) -> CrossSectionBasis {
        let relative_orientation = match self {
            Self::Xy => DQuat::IDENTITY,
            Self::Xz => DQuat::from_rotation_x(std::f64::consts::FRAC_PI_2),
            Self::Yz => DQuat::from_rotation_y(std::f64::consts::FRAC_PI_2),
        };
        let orientation =
            normalized_orientation_or_identity(cross_section_orientation) * relative_orientation;
        CrossSectionBasis {
            right_world: (orientation * DVec3::X).normalize(),
            down_world: (orientation * DVec3::Y).normalize(),
            normal_away_world: (orientation * DVec3::Z).normalize(),
        }
    }
}

impl CrossSectionPanelView {
    pub fn world_point_for_panel_point(
        self,
        x_points: f64,
        y_points: f64,
        viewport: PresentationViewport,
    ) -> [f64; 3] {
        let dx = (x_points - viewport.width_points() * 0.5) * self.scale_world_per_screen_point;
        let dy = (y_points - viewport.height_points() * 0.5) * self.scale_world_per_screen_point;
        (self.center_world + self.basis.right_world * dx + self.basis.down_world * dy).to_array()
    }
}

impl CrossSectionViewState {
    pub fn from_canonical(view: CrossSectionView) -> Self {
        Self {
            center_world: DVec3::from_array(view.center_world().components()),
            orientation: normalized_orientation_or_identity(DQuat::from_array(
                view.orientation().xyzw(),
            )),
            scale_world_per_screen_point: view.scale_world_per_screen_point(),
            depth_world: view.depth_world(),
        }
    }

    pub fn into_canonical(self) -> Result<CrossSectionView, CrossSectionInteractionError> {
        let [x, y, z] = self.center_world.to_array();
        let [qx, qy, qz, qw] = self.orientation.to_array();
        CrossSectionView::new(
            WorldPoint3::new(x, y, z)?,
            UnitQuaternion::new_xyzw(qx, qy, qz, qw)?,
            self.scale_world_per_screen_point,
            self.depth_world,
        )
        .map_err(Into::into)
    }

    pub fn view(self, panel: CrossSectionPanel) -> CrossSectionPanelView {
        CrossSectionPanelView {
            center_world: self.center_world,
            basis: panel.basis(self.orientation),
            scale_world_per_screen_point: self.scale_world_per_screen_point,
        }
    }

    pub fn pan_by_panel_points(
        &mut self,
        panel: CrossSectionPanel,
        motion_x_points: f64,
        motion_y_points: f64,
    ) {
        if !motion_x_points.is_finite() || !motion_y_points.is_finite() {
            return;
        }
        let basis = panel.basis(self.orientation);
        self.center_world -=
            basis.right_world * motion_x_points * self.scale_world_per_screen_point;
        self.center_world -= basis.down_world * motion_y_points * self.scale_world_per_screen_point;
    }

    pub fn slice_by_world_distance(&mut self, panel: CrossSectionPanel, distance_world: f64) {
        if !distance_world.is_finite() {
            return;
        }
        self.center_world += panel.basis(self.orientation).normal_away_world * distance_world;
    }

    pub fn zoom_around_panel_point(
        &mut self,
        panel: CrossSectionPanel,
        viewport: PresentationViewport,
        x_points: f64,
        y_points: f64,
        factor: f64,
    ) {
        if !x_points.is_finite() || !y_points.is_finite() || !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let old_view = self.view(panel);
        let anchored_world =
            DVec3::from_array(old_view.world_point_for_panel_point(x_points, y_points, viewport));
        let new_scale = (self.scale_world_per_screen_point * factor).max(CROSS_SECTION_EPSILON);
        let dx_points = x_points - viewport.width_points() * 0.5;
        let dy_points = y_points - viewport.height_points() * 0.5;
        self.scale_world_per_screen_point = new_scale;
        self.center_world = anchored_world
            - old_view.basis.right_world * dx_points * new_scale
            - old_view.basis.down_world * dy_points * new_scale;
    }

    pub fn rotate_oblique_by_panel_drag(
        &mut self,
        panel: CrossSectionPanel,
        delta_x_points: f64,
        delta_y_points: f64,
        radians_per_point: f64,
    ) {
        if !delta_x_points.is_finite()
            || !delta_y_points.is_finite()
            || !radians_per_point.is_finite()
        {
            return;
        }
        let basis = panel.basis(self.orientation);
        let yaw = DQuat::from_axis_angle(basis.down_world, delta_x_points * radians_per_point);
        let pitch = DQuat::from_axis_angle(basis.right_world, -delta_y_points * radians_per_point);
        self.orientation = normalized_orientation_or_identity((yaw * pitch) * self.orientation);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportOrbitDrag {
    start_camera: CameraView,
}

impl ViewportOrbitDrag {
    pub const fn new(start_camera: CameraView) -> Self {
        Self { start_camera }
    }

    pub const fn start_camera(self) -> CameraView {
        self.start_camera
    }
}

pub fn default_camera_for_shape(shape: Shape3D, grid_to_world: GridToWorld) -> CameraView {
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

pub fn fit_camera_to_shape_preserving_view(
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

pub fn fit_active_layer_camera(
    snapshot: &ApplicationSnapshot,
    presentation_viewport: PresentationViewport,
) -> CameraView {
    let view = snapshot.view();
    let layer = snapshot
        .catalog()
        .layer(view.active_layer())
        .expect("application view closes over the dataset catalog");
    fit_camera_to_shape_preserving_view(
        *view.camera(),
        layer.shape().spatial(),
        layer.grid_to_world(),
        presentation_viewport,
    )
}

pub fn reset_active_layer_view(
    snapshot: &ApplicationSnapshot,
    presentation_viewport: PresentationViewport,
) -> ViewState {
    let view = snapshot.view();
    let layer = snapshot
        .catalog()
        .layer(view.active_layer())
        .expect("application view closes over the dataset catalog");
    let default_camera = default_camera_for_shape(layer.shape().spatial(), layer.grid_to_world());
    let camera = CameraView::new(
        view.camera().projection(),
        default_camera.target(),
        default_camera.orientation(),
        default_camera.orthographic_world_per_screen_point(),
        default_camera.perspective_focal_length_screen_points(),
        default_camera.perspective_view_distance_world(),
    )
    .expect("reset preserves validated camera invariants");
    let camera = fit_camera_to_shape_preserving_view(
        camera,
        layer.shape().spatial(),
        layer.grid_to_world(),
        presentation_viewport,
    );
    let cross_section = CrossSectionView::new(
        camera.target(),
        UnitQuaternion::identity(),
        camera.orthographic_world_per_screen_point(),
        representative_voxel_world_size(layer.grid_to_world()),
    )
    .expect("reset derives a valid linked cross-section");
    ViewState::new(
        view.layers().to_vec(),
        view.active_layer(),
        view.timepoint(),
        camera,
        view.layout(),
        cross_section,
        *view.iso_light(),
    )
    .expect("reset preserves the validated application view")
}

pub fn pan_camera(mut camera: CameraView, motion_points: [f32; 2]) -> CameraView {
    let [motion_x, motion_y] = motion_points;
    if !motion_x.is_finite() || !motion_y.is_finite() {
        return camera;
    }
    let world_per_point = match camera.projection() {
        Projection::Orthographic => camera.orthographic_world_per_screen_point(),
        Projection::Perspective => {
            camera.perspective_view_distance_world()
                / camera.perspective_focal_length_screen_points()
        }
    };
    let frame = CameraFrame::new(camera, DEFAULT_PRESENTATION_VIEWPORT)
        .expect("validated camera has finite axes");
    let axes = frame.axes();
    let target = dvec3(camera.target())
        + DVec3::from_array(axes.right()) * (-f64::from(motion_x) * world_per_point)
        + DVec3::from_array(axes.up()) * (f64::from(motion_y) * world_per_point);
    camera = CameraView::new(
        camera.projection(),
        world_point(target),
        camera.orientation(),
        camera.orthographic_world_per_screen_point(),
        camera.perspective_focal_length_screen_points(),
        camera.perspective_view_distance_world(),
    )
    .expect("pan preserves validated camera invariants");
    camera
}

pub fn orbit_camera(
    start_camera: CameraView,
    start_position_points: [f32; 2],
    current_position_points: [f32; 2],
    viewport_size_points: [f32; 2],
) -> CameraView {
    let Some(delta) = arcball_delta(
        f64::from(start_position_points[0]),
        f64::from(start_position_points[1]),
        f64::from(current_position_points[0]),
        f64::from(current_position_points[1]),
        f64::from(viewport_size_points[0]),
        f64::from(viewport_size_points[1]),
    ) else {
        return start_camera;
    };
    let orientation = dquat(start_camera.orientation()) * delta;
    CameraView::new(
        start_camera.projection(),
        start_camera.target(),
        unit_quaternion(orientation),
        start_camera.orthographic_world_per_screen_point(),
        start_camera.perspective_focal_length_screen_points(),
        start_camera.perspective_view_distance_world(),
    )
    .expect("orbit preserves validated camera invariants")
}

pub fn zoom_camera(mut camera: CameraView, scroll_y_points: f32) -> CameraView {
    if !scroll_y_points.is_finite() || scroll_y_points == 0.0 {
        return camera;
    }
    let factor = (-f64::from(scroll_y_points) * 0.001).exp();
    let (orthographic_scale, focal_length) = match camera.projection() {
        Projection::Orthographic => (
            (camera.orthographic_world_per_screen_point() * factor).max(1.0e-9),
            camera.perspective_focal_length_screen_points(),
        ),
        Projection::Perspective => (
            camera.orthographic_world_per_screen_point(),
            (camera.perspective_focal_length_screen_points() / factor).max(1.0e-9),
        ),
    };
    camera = CameraView::new(
        camera.projection(),
        camera.target(),
        camera.orientation(),
        orthographic_scale,
        focal_length,
        camera.perspective_view_distance_world(),
    )
    .expect("zoom preserves validated camera invariants");
    camera
}

pub fn representative_voxel_world_size(grid_to_world: GridToWorld) -> f64 {
    let matrix = grid_to_world.row_major();
    let x = (matrix[0].powi(2) + matrix[4].powi(2) + matrix[8].powi(2)).sqrt();
    let y = (matrix[1].powi(2) + matrix[5].powi(2) + matrix[9].powi(2)).sqrt();
    let z = (matrix[2].powi(2) + matrix[6].powi(2) + matrix[10].powi(2)).sqrt();
    x.max(y).max(z).max(f64::EPSILON)
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
    let max_x = shape.x() as f64 - 0.5;
    let max_y = shape.y() as f64 - 0.5;
    let max_z = shape.z() as f64 - 0.5;
    [
        transform_grid_point(grid_to_world, DVec3::new(-0.5, -0.5, -0.5)),
        transform_grid_point(grid_to_world, DVec3::new(max_x, -0.5, -0.5)),
        transform_grid_point(grid_to_world, DVec3::new(-0.5, max_y, -0.5)),
        transform_grid_point(grid_to_world, DVec3::new(max_x, max_y, -0.5)),
        transform_grid_point(grid_to_world, DVec3::new(-0.5, -0.5, max_z)),
        transform_grid_point(grid_to_world, DVec3::new(max_x, -0.5, max_z)),
        transform_grid_point(grid_to_world, DVec3::new(-0.5, max_y, max_z)),
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
    let fit_width_points = (presentation_viewport.width_points() / CAMERA_FIT_MARGIN).max(1.0);
    let fit_height_points = (presentation_viewport.height_points() / CAMERA_FIT_MARGIN).max(1.0);
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

fn normalized_orientation_or_identity(orientation: DQuat) -> DQuat {
    if !orientation.is_finite() || orientation.length_squared() <= MIN_ORIENTATION_LENGTH_SQUARED {
        DQuat::IDENTITY
    } else {
        orientation.normalize()
    }
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

fn arcball_delta(
    start_x_points: f64,
    start_y_points: f64,
    current_x_points: f64,
    current_y_points: f64,
    viewport_width_points: f64,
    viewport_height_points: f64,
) -> Option<DQuat> {
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
    if start.abs_diff_eq(current, 1.0e-12) {
        None
    } else {
        Some(DQuat::from_rotation_arc(current, start).normalize())
    }
}

fn arcball_vector(
    x_points: f64,
    y_points: f64,
    width_points: f64,
    height_points: f64,
) -> Option<DVec3> {
    if !x_points.is_finite()
        || !y_points.is_finite()
        || !width_points.is_finite()
        || !height_points.is_finite()
        || width_points <= 0.0
        || height_points <= 0.0
    {
        return None;
    }
    let radius = width_points.min(height_points) * 0.5;
    let mut x = (2.0 * x_points - width_points) / (2.0 * radius);
    let mut y = (height_points - 2.0 * y_points) / (2.0 * radius);
    let length_squared = x * x + y * y;
    let z = if length_squared <= 1.0 {
        (1.0 - length_squared).sqrt()
    } else {
        let inverse_length = length_squared.sqrt().recip();
        x *= inverse_length;
        y *= inverse_length;
        0.0
    };
    Some(DVec3::new(x, y, z).normalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera(projection: Projection) -> CameraView {
        CameraView::new(
            projection,
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            2.0,
            400.0,
            100.0,
        )
        .unwrap()
    }

    fn assert_point_close(actual: [f64; 3], expected: [f64; 3]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!(
                (actual - expected).abs() <= 1.0e-10,
                "{actual} != {expected}"
            );
        }
    }

    #[test]
    fn camera_pan_and_orbit_preserve_unedited_camera_facts() {
        let original = camera(Projection::Orthographic);
        let panned = pan_camera(original, [5.0, -3.0]);
        assert_ne!(panned.target(), original.target());
        assert_eq!(panned.orientation(), original.orientation());
        assert_eq!(panned.projection(), original.projection());

        let unchanged = orbit_camera(original, [100.0, 100.0], [100.0, 100.0], [400.0, 300.0]);
        assert_eq!(unchanged, original);
        let orbited = orbit_camera(original, [200.0, 150.0], [230.0, 130.0], [400.0, 300.0]);
        assert_eq!(orbited.target(), original.target());
        assert_ne!(orbited.orientation(), original.orientation());
    }

    #[test]
    fn camera_zoom_uses_the_projection_specific_scale() {
        let orthographic = camera(Projection::Orthographic);
        let orthographic_zoomed = zoom_camera(orthographic, 120.0);
        assert!(
            orthographic_zoomed.orthographic_world_per_screen_point()
                < orthographic.orthographic_world_per_screen_point()
        );
        assert_eq!(
            orthographic_zoomed.perspective_focal_length_screen_points(),
            orthographic.perspective_focal_length_screen_points()
        );

        let perspective = camera(Projection::Perspective);
        let perspective_zoomed = zoom_camera(perspective, 120.0);
        assert_eq!(
            perspective_zoomed.orthographic_world_per_screen_point(),
            perspective.orthographic_world_per_screen_point()
        );
        assert!(
            perspective_zoomed.perspective_focal_length_screen_points()
                > perspective.perspective_focal_length_screen_points()
        );
    }

    #[test]
    fn cross_section_panel_bases_follow_canonical_axes() {
        let canonical =
            CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 1.0, 1.0)
                .unwrap();
        let state = CrossSectionViewState::from_canonical(canonical);
        let viewport = PresentationViewport::new(2.0, 2.0).unwrap();

        assert_point_close(
            state
                .view(CrossSectionPanel::Xy)
                .world_point_for_panel_point(2.0, 2.0, viewport),
            [1.0, 1.0, 0.0],
        );
        assert_point_close(
            state
                .view(CrossSectionPanel::Xz)
                .world_point_for_panel_point(2.0, 2.0, viewport),
            [1.0, 0.0, 1.0],
        );
        assert_point_close(
            state
                .view(CrossSectionPanel::Yz)
                .world_point_for_panel_point(2.0, 2.0, viewport),
            [0.0, 1.0, -1.0],
        );
    }

    #[test]
    fn cross_section_zoom_keeps_the_pointer_anchor_and_round_trips() {
        let canonical = CrossSectionView::new(
            WorldPoint3::new(4.0, 5.0, 6.0).unwrap(),
            UnitQuaternion::identity(),
            0.5,
            2.0,
        )
        .unwrap();
        let viewport = PresentationViewport::new(640.0, 360.0).unwrap();
        let mut state = CrossSectionViewState::from_canonical(canonical);
        let before = state
            .view(CrossSectionPanel::Xy)
            .world_point_for_panel_point(410.0, 120.0, viewport);

        state.zoom_around_panel_point(CrossSectionPanel::Xy, viewport, 410.0, 120.0, 1.75);

        let after = state
            .view(CrossSectionPanel::Xy)
            .world_point_for_panel_point(410.0, 120.0, viewport);
        assert_point_close(after, before);
        let restored = state.into_canonical().unwrap();
        assert_eq!(restored.depth_world(), canonical.depth_world());
        assert_eq!(restored.orientation(), canonical.orientation());
        assert!(restored.scale_world_per_screen_point() > canonical.scale_world_per_screen_point());
    }
}

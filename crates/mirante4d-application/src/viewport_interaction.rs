//! Framework-neutral camera and linked cross-section interaction geometry.

use glam::{DQuat, DVec3};
use mirante4d_domain::{
    CameraView, CrossSectionView, GridToWorld, Projection, UnitQuaternion, WorldPoint3,
};
use mirante4d_render_api::{CameraFrame, DEFAULT_PRESENTATION_VIEWPORT, PresentationViewport};

const CROSS_SECTION_EPSILON: f64 = 1.0e-9;
const MIN_ORIENTATION_LENGTH_SQUARED: f64 = 1.0e-18;

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

    pub fn into_canonical(self) -> Result<CrossSectionView, String> {
        let [x, y, z] = self.center_world.to_array();
        let [qx, qy, qz, qw] = self.orientation.to_array();
        CrossSectionView::new(
            WorldPoint3::new(x, y, z).map_err(|error| error.to_string())?,
            UnitQuaternion::new_xyzw(qx, qy, qz, qw).map_err(|error| error.to_string())?,
            self.scale_world_per_screen_point,
            self.depth_world,
        )
        .map_err(|error| error.to_string())
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

use glam::{DQuat, DVec2, DVec3};
use mirante4d_data::SpatialBrickIndex;
use mirante4d_domain::{GridToWorld, Shape3D};
use mirante4d_format::CurrentGridToWorldExt;
use mirante4d_render_api::PresentationViewport;

use crate::BrickGridSpec;

const EPSILON: f64 = 1.0e-9;
const BRICK_INDEX_EPSILON: f64 = 1.0e-9;
const MIN_ORIENTATION_LENGTH_SQUARED: f64 = 1.0e-18;
const POLYGON_VERTEX_EPSILON: f64 = 1.0e-7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CrossSectionPanel {
    Xy,
    Xz,
    Yz,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrossSectionBasis {
    pub right_world: DVec3,
    pub down_world: DVec3,
    pub normal_away_world: DVec3,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrossSectionView {
    pub center_world: DVec3,
    pub basis: CrossSectionBasis,
    pub scale_world_per_screen_point: f64,
    pub depth_world: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrossSectionViewState {
    pub center_world: DVec3,
    pub orientation: DQuat,
    pub scale_world_per_screen_point: f64,
    pub depth_world: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrossSectionSlab {
    pub center_world: DVec3,
    pub basis: CrossSectionBasis,
    pub half_width_world: f64,
    pub half_height_world: f64,
    pub half_depth_world: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossSectionBrickPlan {
    pub selected_bricks: Vec<SpatialBrickIndex>,
    pub candidate_bricks: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrossSectionChunkPlaneVertex {
    pub world: DVec3,
    pub grid: DVec3,
    pub panel_points: DVec2,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrossSectionPanelBounds {
    pub min_points: DVec2,
    pub max_points: DVec2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CrossSectionChunkPlanePolygon {
    pub brick_index: SpatialBrickIndex,
    pub vertices: Vec<CrossSectionChunkPlaneVertex>,
    pub panel_bounds: CrossSectionPanelBounds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CrossSectionBrickCandidateBounds {
    z_min: u64,
    z_max: u64,
    y_min: u64,
    y_max: u64,
    x_min: u64,
    x_max: u64,
}

impl CrossSectionPanel {
    pub fn relative_orientation(self) -> DQuat {
        match self {
            Self::Xy => DQuat::IDENTITY,
            Self::Xz => DQuat::from_rotation_x(std::f64::consts::FRAC_PI_2),
            Self::Yz => DQuat::from_rotation_y(std::f64::consts::FRAC_PI_2),
        }
    }

    pub fn basis(self, cross_section_orientation: DQuat) -> CrossSectionBasis {
        let orientation = normalized_orientation_or_identity(cross_section_orientation)
            * self.relative_orientation();
        CrossSectionBasis::from_orientation(orientation)
    }
}

impl CrossSectionBasis {
    pub fn from_orientation(orientation: DQuat) -> Self {
        let orientation = normalized_orientation_or_identity(orientation);
        Self {
            right_world: (orientation * DVec3::X).normalize(),
            down_world: (orientation * DVec3::Y).normalize(),
            normal_away_world: (orientation * DVec3::Z).normalize(),
        }
    }
}

impl CrossSectionView {
    pub fn new(
        center_world: DVec3,
        panel: CrossSectionPanel,
        cross_section_orientation: DQuat,
        scale_world_per_screen_point: f64,
        depth_world: f64,
    ) -> Self {
        Self {
            center_world,
            basis: panel.basis(cross_section_orientation),
            scale_world_per_screen_point: scale_world_per_screen_point.max(EPSILON),
            depth_world: depth_world.max(EPSILON),
        }
    }

    pub fn slab(self, viewport: PresentationViewport) -> CrossSectionSlab {
        CrossSectionSlab {
            center_world: self.center_world,
            basis: self.basis,
            half_width_world: viewport.width_points() * self.scale_world_per_screen_point * 0.5,
            half_height_world: viewport.height_points() * self.scale_world_per_screen_point * 0.5,
            half_depth_world: self.depth_world * 0.5,
        }
    }

    pub fn world_point_for_panel_point(
        self,
        x_points: f64,
        y_points: f64,
        viewport: PresentationViewport,
    ) -> DVec3 {
        let dx = (x_points - viewport.width_points() * 0.5) * self.scale_world_per_screen_point;
        let dy = (y_points - viewport.height_points() * 0.5) * self.scale_world_per_screen_point;
        self.center_world + self.basis.right_world * dx + self.basis.down_world * dy
    }
}

impl CrossSectionViewState {
    pub fn new(
        center_world: DVec3,
        orientation: DQuat,
        scale_world_per_screen_point: f64,
        depth_world: f64,
    ) -> Self {
        Self {
            center_world,
            orientation: normalized_orientation_or_identity(orientation),
            scale_world_per_screen_point: scale_world_per_screen_point.max(EPSILON),
            depth_world: depth_world.max(EPSILON),
        }
    }

    pub fn view(self, panel: CrossSectionPanel) -> CrossSectionView {
        CrossSectionView::new(
            self.center_world,
            panel,
            self.orientation,
            self.scale_world_per_screen_point,
            self.depth_world,
        )
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
        let anchored_world = old_view.world_point_for_panel_point(x_points, y_points, viewport);
        let new_scale = (self.scale_world_per_screen_point * factor).max(EPSILON);
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

impl CrossSectionSlab {
    pub fn intersects_brick(self, spec: BrickGridSpec, brick: SpatialBrickIndex) -> bool {
        let Some(corners) = brick_world_corners(spec, brick) else {
            return false;
        };
        let mut min_right = f64::INFINITY;
        let mut max_right = f64::NEG_INFINITY;
        let mut min_down = f64::INFINITY;
        let mut max_down = f64::NEG_INFINITY;
        let mut min_normal = f64::INFINITY;
        let mut max_normal = f64::NEG_INFINITY;

        for corner in corners {
            let relative = corner - self.center_world;
            let right = relative.dot(self.basis.right_world);
            let down = relative.dot(self.basis.down_world);
            let normal = relative.dot(self.basis.normal_away_world);
            min_right = min_right.min(right);
            max_right = max_right.max(right);
            min_down = min_down.min(down);
            max_down = max_down.max(down);
            min_normal = min_normal.min(normal);
            max_normal = max_normal.max(normal);
        }

        ranges_overlap(
            min_right,
            max_right,
            -self.half_width_world,
            self.half_width_world,
        ) && ranges_overlap(
            min_down,
            max_down,
            -self.half_height_world,
            self.half_height_world,
        ) && ranges_overlap(
            min_normal,
            max_normal,
            -self.half_depth_world,
            self.half_depth_world,
        )
    }
}

pub fn plan_cross_section_bricks(
    slab: CrossSectionSlab,
    spec: BrickGridSpec,
) -> Vec<SpatialBrickIndex> {
    plan_cross_section_bricks_with_diagnostics(slab, spec).selected_bricks
}

pub fn plan_cross_section_bricks_with_diagnostics(
    slab: CrossSectionSlab,
    spec: BrickGridSpec,
) -> CrossSectionBrickPlan {
    let grid_shape = brick_grid_shape(spec.volume_shape, spec.brick_shape);
    let Some(bounds) = cross_section_candidate_brick_bounds(slab, spec, grid_shape) else {
        return CrossSectionBrickPlan {
            selected_bricks: Vec::new(),
            candidate_bricks: 0,
        };
    };
    let candidate_bricks = bounds.candidate_count();
    let mut bricks = Vec::new();
    for z in bounds.z_min..=bounds.z_max {
        for y in bounds.y_min..=bounds.y_max {
            for x in bounds.x_min..=bounds.x_max {
                let brick = SpatialBrickIndex::new(z, y, x);
                if slab.intersects_brick(spec, brick) {
                    bricks.push(brick);
                }
            }
        }
    }
    CrossSectionBrickPlan {
        selected_bricks: bricks,
        candidate_bricks,
    }
}

pub fn cross_section_chunk_plane_polygon(
    view: CrossSectionView,
    presentation_viewport: PresentationViewport,
    spec: BrickGridSpec,
    brick: SpatialBrickIndex,
) -> Option<CrossSectionChunkPlanePolygon> {
    let corners = brick_world_corners(spec, brick)?;
    let world_to_grid = spec.grid_to_world.inverse().ok()?;
    let normal = normalized_vector(view.basis.normal_away_world)?;
    let mut world_vertices = Vec::with_capacity(6);

    for (start_index, end_index) in BOX_EDGE_INDICES {
        let start = corners[start_index];
        let end = corners[end_index];
        let start_distance = (start - view.center_world).dot(normal);
        let end_distance = (end - view.center_world).dot(normal);

        if start_distance.abs() <= POLYGON_VERTEX_EPSILON {
            push_unique_vertex(&mut world_vertices, start);
        }
        if end_distance.abs() <= POLYGON_VERTEX_EPSILON {
            push_unique_vertex(&mut world_vertices, end);
        }
        if start_distance * end_distance < -POLYGON_VERTEX_EPSILON * POLYGON_VERTEX_EPSILON {
            let t = start_distance / (start_distance - end_distance);
            if t.is_finite() {
                push_unique_vertex(&mut world_vertices, start + (end - start) * t);
            }
        }
    }

    if world_vertices.len() < 3 {
        return None;
    }
    let centroid = world_vertices
        .iter()
        .copied()
        .fold(DVec3::ZERO, |sum, vertex| sum + vertex)
        / world_vertices.len() as f64;
    world_vertices.sort_by(|left, right| {
        let left_angle = plane_vertex_angle(*left, centroid, view.basis);
        let right_angle = plane_vertex_angle(*right, centroid, view.basis);
        left_angle.total_cmp(&right_angle)
    });

    let vertices = world_vertices
        .into_iter()
        .map(|world| CrossSectionChunkPlaneVertex {
            world,
            grid: world_to_grid.transform_point(world),
            panel_points: panel_points_for_world(world, view, presentation_viewport),
        })
        .collect::<Vec<_>>();
    if polygon_panel_area_abs(&vertices) <= POLYGON_VERTEX_EPSILON {
        return None;
    }
    let panel_bounds = panel_bounds_for_vertices(&vertices)?;
    Some(CrossSectionChunkPlanePolygon {
        brick_index: brick,
        vertices,
        panel_bounds,
    })
}

fn brick_grid_shape(volume_shape: Shape3D, brick_shape: Shape3D) -> Shape3D {
    Shape3D::new(
        volume_shape.z().div_ceil(brick_shape.z()),
        volume_shape.y().div_ceil(brick_shape.y()),
        volume_shape.x().div_ceil(brick_shape.x()),
    )
    .expect("nonzero Shape3D dimensions produce a nonzero brick grid")
}

fn cross_section_candidate_brick_bounds(
    slab: CrossSectionSlab,
    spec: BrickGridSpec,
    grid_shape: Shape3D,
) -> Option<CrossSectionBrickCandidateBounds> {
    let world_to_grid = spec.grid_to_world.inverse().ok()?;
    let mut grid_min = DVec3::splat(f64::INFINITY);
    let mut grid_max = DVec3::splat(f64::NEG_INFINITY);

    for corner in slab_world_corners(slab) {
        let grid = world_to_grid.transform_point(corner);
        grid_min = grid_min.min(grid);
        grid_max = grid_max.max(grid);
    }

    let x_bounds = candidate_axis_bounds(
        grid_min.x,
        grid_max.x,
        spec.volume_shape.x(),
        spec.brick_shape.x(),
        grid_shape.x(),
    )?;
    let y_bounds = candidate_axis_bounds(
        grid_min.y,
        grid_max.y,
        spec.volume_shape.y(),
        spec.brick_shape.y(),
        grid_shape.y(),
    )?;
    let z_bounds = candidate_axis_bounds(
        grid_min.z,
        grid_max.z,
        spec.volume_shape.z(),
        spec.brick_shape.z(),
        grid_shape.z(),
    )?;

    Some(CrossSectionBrickCandidateBounds {
        z_min: z_bounds.0,
        z_max: z_bounds.1,
        y_min: y_bounds.0,
        y_max: y_bounds.1,
        x_min: x_bounds.0,
        x_max: x_bounds.1,
    })
}

fn slab_world_corners(slab: CrossSectionSlab) -> [DVec3; 8] {
    let right = slab.basis.right_world * slab.half_width_world;
    let down = slab.basis.down_world * slab.half_height_world;
    let normal = slab.basis.normal_away_world * slab.half_depth_world;
    [
        slab.center_world - right - down - normal,
        slab.center_world + right - down - normal,
        slab.center_world - right + down - normal,
        slab.center_world + right + down - normal,
        slab.center_world - right - down + normal,
        slab.center_world + right - down + normal,
        slab.center_world - right + down + normal,
        slab.center_world + right + down + normal,
    ]
}

fn candidate_axis_bounds(
    min_coord: f64,
    max_coord: f64,
    volume_axis: u64,
    brick_axis: u64,
    grid_axis: u64,
) -> Option<(u64, u64)> {
    if !min_coord.is_finite() || !max_coord.is_finite() || grid_axis == 0 {
        return None;
    }
    let volume_min = -0.5;
    let volume_max = volume_axis as f64 - 0.5;
    if max_coord < volume_min - EPSILON || min_coord > volume_max + EPSILON {
        return None;
    }
    let last = grid_axis - 1;
    let lower = (((min_coord + 0.5 - BRICK_INDEX_EPSILON) / brick_axis as f64).floor() as i64)
        .clamp(0, last as i64) as u64;
    let upper = (((max_coord + 0.5 + BRICK_INDEX_EPSILON) / brick_axis as f64).floor() as i64)
        .clamp(0, last as i64) as u64;
    if lower > upper {
        return None;
    }
    // The precise slab/brick predicate below is conservative in slab-basis
    // space. Pad the broad phase by one chunk to preserve that existing
    // conservative selection while still avoiding a full-volume scan.
    Some((lower.saturating_sub(1), upper.saturating_add(1).min(last)))
}

impl CrossSectionBrickCandidateBounds {
    fn candidate_count(self) -> usize {
        self.z_max
            .saturating_sub(self.z_min)
            .saturating_add(1)
            .saturating_mul(self.y_max.saturating_sub(self.y_min).saturating_add(1))
            .saturating_mul(self.x_max.saturating_sub(self.x_min).saturating_add(1))
            .try_into()
            .unwrap_or(usize::MAX)
    }
}

fn brick_world_corners(spec: BrickGridSpec, brick: SpatialBrickIndex) -> Option<[DVec3; 8]> {
    let min_x = brick.x.checked_mul(spec.brick_shape.x())?;
    let min_y = brick.y.checked_mul(spec.brick_shape.y())?;
    let min_z = brick.z.checked_mul(spec.brick_shape.z())?;
    if min_x >= spec.volume_shape.x()
        || min_y >= spec.volume_shape.y()
        || min_z >= spec.volume_shape.z()
    {
        return None;
    }
    let max_x = min_x
        .saturating_add(spec.brick_shape.x())
        .min(spec.volume_shape.x());
    let max_y = min_y
        .saturating_add(spec.brick_shape.y())
        .min(spec.volume_shape.y());
    let max_z = min_z
        .saturating_add(spec.brick_shape.z())
        .min(spec.volume_shape.z());
    let xs = [min_x as f64 - 0.5, max_x as f64 - 0.5];
    let ys = [min_y as f64 - 0.5, max_y as f64 - 0.5];
    let zs = [min_z as f64 - 0.5, max_z as f64 - 0.5];

    Some([
        grid_point_to_world(spec.grid_to_world, xs[0], ys[0], zs[0]),
        grid_point_to_world(spec.grid_to_world, xs[1], ys[0], zs[0]),
        grid_point_to_world(spec.grid_to_world, xs[0], ys[1], zs[0]),
        grid_point_to_world(spec.grid_to_world, xs[1], ys[1], zs[0]),
        grid_point_to_world(spec.grid_to_world, xs[0], ys[0], zs[1]),
        grid_point_to_world(spec.grid_to_world, xs[1], ys[0], zs[1]),
        grid_point_to_world(spec.grid_to_world, xs[0], ys[1], zs[1]),
        grid_point_to_world(spec.grid_to_world, xs[1], ys[1], zs[1]),
    ])
}

fn grid_point_to_world(grid_to_world: GridToWorld, x: f64, y: f64, z: f64) -> DVec3 {
    grid_to_world.transform_point_vec(DVec3::new(x, y, z))
}

const BOX_EDGE_INDICES: [(usize, usize); 12] = [
    (0, 1),
    (0, 2),
    (1, 3),
    (2, 3),
    (4, 5),
    (4, 6),
    (5, 7),
    (6, 7),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];

fn normalized_vector(vector: DVec3) -> Option<DVec3> {
    if !vector.is_finite() || vector.length_squared() <= MIN_ORIENTATION_LENGTH_SQUARED {
        return None;
    }
    Some(vector.normalize())
}

fn push_unique_vertex(vertices: &mut Vec<DVec3>, candidate: DVec3) {
    if !candidate.is_finite() {
        return;
    }
    let epsilon_squared = POLYGON_VERTEX_EPSILON * POLYGON_VERTEX_EPSILON;
    if vertices
        .iter()
        .any(|existing| existing.distance_squared(candidate) <= epsilon_squared)
    {
        return;
    }
    vertices.push(candidate);
}

fn plane_vertex_angle(vertex: DVec3, centroid: DVec3, basis: CrossSectionBasis) -> f64 {
    let relative = vertex - centroid;
    let x = relative.dot(basis.right_world);
    let y = relative.dot(basis.down_world);
    y.atan2(x)
}

fn panel_points_for_world(
    world: DVec3,
    view: CrossSectionView,
    presentation_viewport: PresentationViewport,
) -> DVec2 {
    let relative = world - view.center_world;
    DVec2::new(
        presentation_viewport.width_points() * 0.5
            + relative.dot(view.basis.right_world) / view.scale_world_per_screen_point,
        presentation_viewport.height_points() * 0.5
            + relative.dot(view.basis.down_world) / view.scale_world_per_screen_point,
    )
}

fn panel_bounds_for_vertices(
    vertices: &[CrossSectionChunkPlaneVertex],
) -> Option<CrossSectionPanelBounds> {
    let mut min_points = DVec2::splat(f64::INFINITY);
    let mut max_points = DVec2::splat(f64::NEG_INFINITY);
    for vertex in vertices {
        min_points = min_points.min(vertex.panel_points);
        max_points = max_points.max(vertex.panel_points);
    }
    if !min_points.is_finite() || !max_points.is_finite() {
        return None;
    }
    Some(CrossSectionPanelBounds {
        min_points,
        max_points,
    })
}

fn polygon_panel_area_abs(vertices: &[CrossSectionChunkPlaneVertex]) -> f64 {
    if vertices.len() < 3 {
        return 0.0;
    }
    let mut area_twice = 0.0;
    for index in 0..vertices.len() {
        let current = vertices[index].panel_points;
        let next = vertices[(index + 1) % vertices.len()].panel_points;
        area_twice += current.x * next.y - next.x * current.y;
    }
    area_twice.abs() * 0.5
}

fn ranges_overlap(min_a: f64, max_a: f64, min_b: f64, max_b: f64) -> bool {
    max_a >= min_b - EPSILON && min_a <= max_b + EPSILON
}

fn normalized_orientation_or_identity(orientation: DQuat) -> DQuat {
    if !orientation.is_finite() || orientation.length_squared() <= MIN_ORIENTATION_LENGTH_SQUARED {
        return DQuat::IDENTITY;
    }
    orientation.normalize()
}

#[cfg(test)]
mod tests {
    use approx::assert_abs_diff_eq;
    use glam::{DQuat, DVec2, DVec3};
    use mirante4d_data::SpatialBrickIndex;
    use mirante4d_domain::{GridToWorld, Shape3D};
    use mirante4d_render_api::PresentationViewport;

    use crate::{BrickGridSpec, cross_section::*};

    fn assert_vec3_abs_diff_eq(actual: DVec3, expected: DVec3) {
        assert_abs_diff_eq!(actual.x, expected.x, epsilon = 1e-12);
        assert_abs_diff_eq!(actual.y, expected.y, epsilon = 1e-12);
        assert_abs_diff_eq!(actual.z, expected.z, epsilon = 1e-12);
    }

    fn assert_vec2_abs_diff_eq(actual: DVec2, expected: DVec2) {
        assert_abs_diff_eq!(actual.x, expected.x, epsilon = 1e-12);
        assert_abs_diff_eq!(actual.y, expected.y, epsilon = 1e-12);
    }

    fn spec() -> BrickGridSpec {
        BrickGridSpec {
            volume_shape: Shape3D::new(8, 8, 8).unwrap(),
            brick_shape: Shape3D::new(4, 4, 4).unwrap(),
            grid_to_world: GridToWorld::identity(),
        }
    }

    #[test]
    fn panel_bases_match_neuroglancer_relative_orientations() {
        let xy = CrossSectionPanel::Xy.basis(DQuat::IDENTITY);
        assert_vec3_abs_diff_eq(xy.right_world, DVec3::X);
        assert_vec3_abs_diff_eq(xy.down_world, DVec3::Y);
        assert_vec3_abs_diff_eq(xy.normal_away_world, DVec3::Z);

        let xz = CrossSectionPanel::Xz.basis(DQuat::IDENTITY);
        assert_vec3_abs_diff_eq(xz.right_world, DVec3::X);
        assert_vec3_abs_diff_eq(xz.down_world, DVec3::Z);
        assert_vec3_abs_diff_eq(xz.normal_away_world, -DVec3::Y);

        let yz = CrossSectionPanel::Yz.basis(DQuat::IDENTITY);
        assert_vec3_abs_diff_eq(yz.right_world, -DVec3::Z);
        assert_vec3_abs_diff_eq(yz.down_world, DVec3::Y);
        assert_vec3_abs_diff_eq(yz.normal_away_world, DVec3::X);
    }

    #[test]
    fn oblique_orientation_keeps_panels_relative() {
        let oblique = DQuat::from_rotation_z(0.25);
        let xy = CrossSectionPanel::Xy.basis(oblique);
        let xz = CrossSectionPanel::Xz.basis(oblique);

        assert_vec3_abs_diff_eq(xz.right_world, xy.right_world);
        assert_abs_diff_eq!(
            xz.down_world.dot(xy.normal_away_world),
            1.0,
            epsilon = 1e-12
        );
        assert_abs_diff_eq!(
            xz.normal_away_world.dot(-xy.down_world),
            1.0,
            epsilon = 1e-12
        );
    }

    #[test]
    fn panel_point_maps_through_scale_and_basis() {
        let view = CrossSectionView::new(
            DVec3::new(10.0, 20.0, 30.0),
            CrossSectionPanel::Xy,
            DQuat::IDENTITY,
            0.5,
            1.0,
        );
        let viewport = PresentationViewport::new(100.0, 80.0).unwrap();

        assert_vec3_abs_diff_eq(
            view.world_point_for_panel_point(50.0, 40.0, viewport),
            DVec3::new(10.0, 20.0, 30.0),
        );
        assert_vec3_abs_diff_eq(
            view.world_point_for_panel_point(60.0, 46.0, viewport),
            DVec3::new(15.0, 23.0, 30.0),
        );
    }

    #[test]
    fn view_state_derives_panel_view_from_shared_orientation() {
        let orientation = DQuat::from_rotation_z(0.25);
        let state = CrossSectionViewState::new(DVec3::new(1.0, 2.0, 3.0), orientation, 0.5, 2.0);
        let view = state.view(CrossSectionPanel::Xz);
        let expected = CrossSectionPanel::Xz.basis(orientation);

        assert_vec3_abs_diff_eq(view.center_world, state.center_world);
        assert_vec3_abs_diff_eq(view.basis.right_world, expected.right_world);
        assert_vec3_abs_diff_eq(view.basis.down_world, expected.down_world);
        assert_vec3_abs_diff_eq(view.basis.normal_away_world, expected.normal_away_world);
        assert_abs_diff_eq!(view.scale_world_per_screen_point, 0.5, epsilon = 1e-12);
        assert_abs_diff_eq!(view.depth_world, 2.0, epsilon = 1e-12);
    }

    #[test]
    fn pan_by_panel_points_moves_center_opposite_screen_drag() {
        let mut state = CrossSectionViewState::new(DVec3::ZERO, DQuat::IDENTITY, 0.5, 1.0);

        state.pan_by_panel_points(CrossSectionPanel::Xy, 10.0, -4.0);

        assert_vec3_abs_diff_eq(state.center_world, DVec3::new(-5.0, 2.0, 0.0));
    }

    #[test]
    fn slice_step_moves_along_active_panel_normal() {
        let mut state = CrossSectionViewState::new(DVec3::ZERO, DQuat::IDENTITY, 1.0, 1.0);

        state.slice_by_world_distance(CrossSectionPanel::Xz, 3.0);

        assert_vec3_abs_diff_eq(state.center_world, DVec3::new(0.0, -3.0, 0.0));
    }

    #[test]
    fn cursor_anchored_zoom_preserves_world_point_under_cursor() {
        let viewport = PresentationViewport::new(100.0, 80.0).unwrap();
        let mut state =
            CrossSectionViewState::new(DVec3::new(10.0, 20.0, 30.0), DQuat::IDENTITY, 0.5, 1.0);
        let before = state
            .view(CrossSectionPanel::Xy)
            .world_point_for_panel_point(70.0, 50.0, viewport);

        state.zoom_around_panel_point(CrossSectionPanel::Xy, viewport, 70.0, 50.0, 0.25);
        let after = state
            .view(CrossSectionPanel::Xy)
            .world_point_for_panel_point(70.0, 50.0, viewport);

        assert_vec3_abs_diff_eq(after, before);
        assert_abs_diff_eq!(state.scale_world_per_screen_point, 0.125, epsilon = 1e-12);
        assert_vec3_abs_diff_eq(state.center_world, DVec3::new(17.5, 23.75, 30.0));
    }

    #[test]
    fn oblique_drag_rotates_shared_orientation_around_panel_axes() {
        let mut state = CrossSectionViewState::new(DVec3::ZERO, DQuat::IDENTITY, 1.0, 1.0);

        state.rotate_oblique_by_panel_drag(CrossSectionPanel::Xy, 100.0, 0.0, 0.01);

        let xy = state.view(CrossSectionPanel::Xy);
        let xz = state.view(CrossSectionPanel::Xz);
        assert_abs_diff_eq!(state.orientation.length_squared(), 1.0, epsilon = 1e-12);
        assert!(xy.basis.normal_away_world.x > 0.8);
        assert_vec3_abs_diff_eq(xz.basis.right_world, xy.basis.right_world);
    }

    #[test]
    fn axis_aligned_xy_slab_selects_intersecting_z_brick_row() {
        let view = CrossSectionView::new(
            DVec3::new(3.5, 3.5, 3.5),
            CrossSectionPanel::Xy,
            DQuat::IDENTITY,
            1.0,
            1.0,
        );
        let slab = view.slab(PresentationViewport::new(8.0, 8.0).unwrap());
        let bricks = plan_cross_section_bricks(slab, spec());

        assert_eq!(
            bricks,
            vec![
                SpatialBrickIndex::new(0, 0, 0),
                SpatialBrickIndex::new(0, 0, 1),
                SpatialBrickIndex::new(0, 1, 0),
                SpatialBrickIndex::new(0, 1, 1),
                SpatialBrickIndex::new(1, 0, 0),
                SpatialBrickIndex::new(1, 0, 1),
                SpatialBrickIndex::new(1, 1, 0),
                SpatialBrickIndex::new(1, 1, 1),
            ]
        );
    }

    #[test]
    fn axis_aligned_chunk_plane_polygon_maps_to_panel_quad() {
        let spec = BrickGridSpec {
            volume_shape: Shape3D::new(4, 4, 4).unwrap(),
            brick_shape: Shape3D::new(4, 4, 4).unwrap(),
            grid_to_world: GridToWorld::identity(),
        };
        let viewport = PresentationViewport::new(4.0, 4.0).unwrap();
        let view = CrossSectionView::new(
            DVec3::new(1.5, 1.5, 1.5),
            CrossSectionPanel::Xy,
            DQuat::IDENTITY,
            1.0,
            1.0,
        );

        let polygon = cross_section_chunk_plane_polygon(
            view,
            viewport,
            spec,
            SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();

        assert_eq!(polygon.brick_index, SpatialBrickIndex::new(0, 0, 0));
        assert_eq!(polygon.vertices.len(), 4);
        assert_vec2_abs_diff_eq(polygon.panel_bounds.min_points, DVec2::new(0.0, 0.0));
        assert_vec2_abs_diff_eq(polygon.panel_bounds.max_points, DVec2::new(4.0, 4.0));
        let expected_panel_points = [
            DVec2::new(0.0, 0.0),
            DVec2::new(4.0, 0.0),
            DVec2::new(4.0, 4.0),
            DVec2::new(0.0, 4.0),
        ];
        for (vertex, expected_panel_point) in polygon.vertices.iter().zip(expected_panel_points) {
            assert_abs_diff_eq!(vertex.world.z, 1.5, epsilon = 1e-12);
            assert_abs_diff_eq!(vertex.grid.z, 1.5, epsilon = 1e-12);
            assert_vec2_abs_diff_eq(vertex.panel_points, expected_panel_point);
        }
    }

    #[test]
    fn oblique_chunk_plane_polygon_produces_ordered_hexagon() {
        let spec = BrickGridSpec {
            volume_shape: Shape3D::new(2, 2, 2).unwrap(),
            brick_shape: Shape3D::new(2, 2, 2).unwrap(),
            grid_to_world: GridToWorld::identity(),
        };
        let normal = DVec3::new(1.0, 1.0, 1.0).normalize();
        let right = DVec3::new(1.0, -1.0, 0.0).normalize();
        let down = normal.cross(right).normalize();
        let view = CrossSectionView {
            center_world: DVec3::splat(0.5),
            basis: CrossSectionBasis {
                right_world: right,
                down_world: down,
                normal_away_world: normal,
            },
            scale_world_per_screen_point: 1.0,
            depth_world: 1.0,
        };

        let polygon = cross_section_chunk_plane_polygon(
            view,
            PresentationViewport::new(8.0, 8.0).unwrap(),
            spec,
            SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();

        assert_eq!(polygon.vertices.len(), 6);
        assert!(polygon.panel_bounds.min_points.is_finite());
        assert!(polygon.panel_bounds.max_points.is_finite());
        assert!(
            signed_panel_area(&polygon.vertices).abs() > 1.0,
            "oblique chunk polygon should have non-degenerate ordered area"
        );
        for vertex in &polygon.vertices {
            assert_abs_diff_eq!(
                (vertex.world - view.center_world).dot(normal),
                0.0,
                epsilon = 1e-12
            );
            assert_vec3_abs_diff_eq(vertex.world, vertex.grid);
            assert!(vertex.panel_points.is_finite());
        }
    }

    #[test]
    fn chunk_plane_polygon_is_absent_when_plane_misses_chunk() {
        let spec = BrickGridSpec {
            volume_shape: Shape3D::new(4, 4, 4).unwrap(),
            brick_shape: Shape3D::new(4, 4, 4).unwrap(),
            grid_to_world: GridToWorld::identity(),
        };
        let view = CrossSectionView::new(
            DVec3::new(1.5, 1.5, 10.0),
            CrossSectionPanel::Xy,
            DQuat::IDENTITY,
            1.0,
            1.0,
        );

        assert!(
            cross_section_chunk_plane_polygon(
                view,
                PresentationViewport::new(4.0, 4.0).unwrap(),
                spec,
                SpatialBrickIndex::new(0, 0, 0),
            )
            .is_none()
        );
    }

    #[test]
    fn thin_xy_slab_inside_lower_z_brick_selects_lower_z_only() {
        let view = CrossSectionView::new(
            DVec3::new(3.5, 3.5, 1.0),
            CrossSectionPanel::Xy,
            DQuat::IDENTITY,
            1.0,
            0.25,
        );
        let slab = view.slab(PresentationViewport::new(8.0, 8.0).unwrap());
        let bricks = plan_cross_section_bricks(slab, spec());

        assert_eq!(
            bricks,
            vec![
                SpatialBrickIndex::new(0, 0, 0),
                SpatialBrickIndex::new(0, 0, 1),
                SpatialBrickIndex::new(0, 1, 0),
                SpatialBrickIndex::new(0, 1, 1),
            ]
        );
    }

    fn signed_panel_area(vertices: &[CrossSectionChunkPlaneVertex]) -> f64 {
        let mut area_twice = 0.0;
        for index in 0..vertices.len() {
            let current = vertices[index].panel_points;
            let next = vertices[(index + 1) % vertices.len()].panel_points;
            area_twice += current.x * next.y - next.x * current.y;
        }
        area_twice * 0.5
    }

    #[test]
    fn oblique_slab_culls_to_local_brick_subset() {
        let spec = BrickGridSpec {
            volume_shape: Shape3D::new(16, 16, 16).unwrap(),
            brick_shape: Shape3D::new(4, 4, 4).unwrap(),
            grid_to_world: GridToWorld::identity(),
        };
        let view = CrossSectionView::new(
            DVec3::new(7.5, 7.5, 7.5),
            CrossSectionPanel::Xy,
            DQuat::from_rotation_x(std::f64::consts::FRAC_PI_4),
            1.0,
            0.5,
        );
        let slab = view.slab(PresentationViewport::new(7.0, 7.0).unwrap());
        let bricks = plan_cross_section_bricks(slab, spec);

        assert!(!bricks.is_empty());
        assert!(bricks.len() < 64);
        assert!(bricks.iter().any(|brick| brick.z == 1));
        assert!(bricks.iter().any(|brick| brick.z == 2));
        assert!(bricks.iter().all(|brick| brick.x == 1 || brick.x == 2));
    }

    #[test]
    fn bounded_planner_matches_bruteforce_for_oblique_slab() {
        let spec = BrickGridSpec {
            volume_shape: Shape3D::new(32, 32, 32).unwrap(),
            brick_shape: Shape3D::new(4, 4, 4).unwrap(),
            grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.5, 2.0),
        };
        let view = CrossSectionView::new(
            DVec3::new(15.5, 22.5, 31.0),
            CrossSectionPanel::Xz,
            DQuat::from_rotation_y(0.4) * DQuat::from_rotation_x(-0.2),
            1.0,
            0.75,
        );
        let slab = view.slab(PresentationViewport::new(11.0, 9.0).unwrap());
        let plan = plan_cross_section_bricks_with_diagnostics(slab, spec);
        let mut brute_force = Vec::new();
        let grid_shape = brick_grid_shape(spec.volume_shape, spec.brick_shape);
        for z in 0..grid_shape.z() {
            for y in 0..grid_shape.y() {
                for x in 0..grid_shape.x() {
                    let brick = SpatialBrickIndex::new(z, y, x);
                    if slab.intersects_brick(spec, brick) {
                        brute_force.push(brick);
                    }
                }
            }
        }

        assert_eq!(plan.selected_bricks, brute_force);
        assert!(plan.candidate_bricks < grid_shape.element_count().unwrap() as usize);
    }

    #[test]
    fn bounded_planner_does_not_scan_full_volume_for_local_oblique_view() {
        let spec = BrickGridSpec {
            volume_shape: Shape3D::new(512, 512, 512).unwrap(),
            brick_shape: Shape3D::new(16, 16, 16).unwrap(),
            grid_to_world: GridToWorld::identity(),
        };
        let view = CrossSectionView::new(
            DVec3::new(255.5, 255.5, 255.5),
            CrossSectionPanel::Xy,
            DQuat::from_rotation_x(0.33) * DQuat::from_rotation_z(-0.2),
            1.0,
            1.0,
        );
        let slab = view.slab(PresentationViewport::new(64.0, 64.0).unwrap());
        let plan = plan_cross_section_bricks_with_diagnostics(slab, spec);
        let full_grid_bricks = brick_grid_shape(spec.volume_shape, spec.brick_shape)
            .element_count()
            .unwrap() as usize;

        assert!(!plan.selected_bricks.is_empty());
        assert!(plan.candidate_bricks < full_grid_bricks / 16);
    }

    #[test]
    fn bounded_planner_reports_no_candidates_outside_volume() {
        let view = CrossSectionView::new(
            DVec3::new(100.0, 100.0, 100.0),
            CrossSectionPanel::Xy,
            DQuat::IDENTITY,
            1.0,
            1.0,
        );
        let slab = view.slab(PresentationViewport::new(4.0, 4.0).unwrap());
        let plan = plan_cross_section_bricks_with_diagnostics(slab, spec());

        assert_eq!(plan.candidate_bricks, 0);
        assert!(plan.selected_bricks.is_empty());
    }

    #[test]
    fn anisotropic_transform_affects_slab_intersection_in_world_space() {
        let spec = BrickGridSpec {
            volume_shape: Shape3D::new(8, 8, 8).unwrap(),
            brick_shape: Shape3D::new(4, 4, 4).unwrap(),
            grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 4.0),
        };
        let view = CrossSectionView::new(
            DVec3::new(3.5, 3.5, 7.0),
            CrossSectionPanel::Xy,
            DQuat::IDENTITY,
            1.0,
            1.0,
        );
        let slab = view.slab(PresentationViewport::new(8.0, 8.0).unwrap());
        let bricks = plan_cross_section_bricks(slab, spec);

        assert_eq!(
            bricks,
            vec![
                SpatialBrickIndex::new(0, 0, 0),
                SpatialBrickIndex::new(0, 0, 1),
                SpatialBrickIndex::new(0, 1, 0),
                SpatialBrickIndex::new(0, 1, 1),
            ]
        );
    }
}

use std::collections::HashSet;

use glam::DVec3;
use mirante4d_data::SpatialBrickIndex;
use mirante4d_domain::{GridToWorld, Projection, Shape3D};
use mirante4d_format::CurrentGridToWorldExt;
use mirante4d_render_api::CameraFrame;

use crate::{RenderError, RenderViewport};

const EPSILON: f64 = 1.0e-9;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrickGridSpec {
    pub volume_shape: Shape3D,
    pub brick_shape: Shape3D,
    pub grid_to_world: GridToWorld,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrickPlanOptions {
    pub pixel_stride: u64,
}

#[derive(Debug, Clone, Copy)]
struct GridRay {
    origin: DVec3,
    direction: DVec3,
}

#[derive(Debug, Clone, Copy)]
struct RayBoxHit {
    enter: f64,
    exit: f64,
}

#[derive(Debug, Clone, Copy)]
struct AxisTraversal {
    index: i64,
    step: i64,
    next_t: f64,
    delta_t: f64,
    limit: i64,
}

#[derive(Debug, Clone, Copy)]
struct OrthographicView {
    eye: DVec3,
    forward: DVec3,
    right: DVec3,
    up: DVec3,
    half_width: f64,
    half_height: f64,
}

impl Default for BrickPlanOptions {
    fn default() -> Self {
        Self { pixel_stride: 1 }
    }
}

pub fn plan_visible_bricks(
    camera: CameraFrame,
    viewport: RenderViewport,
    spec: BrickGridSpec,
    options: BrickPlanOptions,
) -> Result<Vec<SpatialBrickIndex>, RenderError> {
    if options.pixel_stride == 0 {
        return Err(RenderError::InvalidBrickPixelStride);
    }

    if crate::current_camera::projection(camera) == Projection::Orthographic {
        return plan_orthographic_volume_bricks(camera, viewport, spec);
    }

    let world_to_grid = spec.grid_to_world.inverse()?;
    let mut bricks = HashSet::new();

    for row in stepped_pixel_indices(viewport.height, options.pixel_stride) {
        for col in stepped_pixel_indices(viewport.width, options.pixel_stride) {
            let world_ray = crate::current_camera::ray_for_render_pixel(
                camera, col as f64, row as f64, viewport,
            )?;
            let grid_ray = GridRay {
                origin: world_to_grid.transform_point(world_ray.origin),
                direction: world_to_grid.transform_vector(world_ray.direction),
            };
            collect_ray_bricks(grid_ray, spec, &mut bricks);
        }
    }

    let mut bricks = bricks.into_iter().collect::<Vec<_>>();
    bricks.sort_by_key(|brick| (brick.z, brick.y, brick.x));
    Ok(bricks)
}

fn plan_orthographic_volume_bricks(
    camera: CameraFrame,
    viewport: RenderViewport,
    spec: BrickGridSpec,
) -> Result<Vec<SpatialBrickIndex>, RenderError> {
    let Some((forward, right, up)) = camera_basis(camera) else {
        return Ok(Vec::new());
    };
    let half_height = camera.orthographic_world_span_height()? * 0.5;
    let half_width = camera.orthographic_world_span_width()? * 0.5;
    let view = OrthographicView {
        eye: crate::current_camera::eye(camera),
        forward,
        right,
        up,
        half_width: sampled_center_half_extent(half_width, viewport.width),
        half_height: sampled_center_half_extent(half_height, viewport.height),
    };
    let grid_shape = brick_grid_shape(spec.volume_shape, spec.brick_shape);
    let mut bricks = Vec::new();

    for brick_z in 0..grid_shape.z() {
        for brick_y in 0..grid_shape.y() {
            for brick_x in 0..grid_shape.x() {
                if orthographic_brick_overlaps_view(
                    view,
                    spec,
                    SpatialBrickIndex::new(brick_z, brick_y, brick_x),
                ) {
                    bricks.push(SpatialBrickIndex::new(brick_z, brick_y, brick_x));
                }
            }
        }
    }

    Ok(bricks)
}

fn sampled_center_half_extent(half_extent: f64, pixels: u64) -> f64 {
    half_extent * (1.0 - 1.0 / pixels as f64).max(0.0)
}

fn camera_basis(camera: CameraFrame) -> Option<(DVec3, DVec3, DVec3)> {
    let forward = crate::current_camera::target(camera) - crate::current_camera::eye(camera);
    if forward.length_squared() <= EPSILON {
        return None;
    }
    let forward = forward.normalize();
    let right = forward.cross(crate::current_camera::up(camera));
    if right.length_squared() <= EPSILON {
        return None;
    }
    let right = right.normalize();
    let up = right.cross(forward).normalize();
    Some((forward, right, up))
}

fn brick_grid_shape(volume_shape: Shape3D, brick_shape: Shape3D) -> Shape3D {
    Shape3D::new(
        volume_shape.z().div_ceil(brick_shape.z()),
        volume_shape.y().div_ceil(brick_shape.y()),
        volume_shape.x().div_ceil(brick_shape.x()),
    )
    .expect("nonzero Shape3D dimensions produce a nonzero brick grid")
}

fn orthographic_brick_overlaps_view(
    view: OrthographicView,
    spec: BrickGridSpec,
    brick: SpatialBrickIndex,
) -> bool {
    let min_x = brick.x * spec.brick_shape.x();
    let min_y = brick.y * spec.brick_shape.y();
    let min_z = brick.z * spec.brick_shape.z();
    let max_x = (min_x + spec.brick_shape.x()).min(spec.volume_shape.x());
    let max_y = (min_y + spec.brick_shape.y()).min(spec.volume_shape.y());
    let max_z = (min_z + spec.brick_shape.z()).min(spec.volume_shape.z());
    let min_x_bound = min_x as f64 - 0.5;
    let min_y_bound = min_y as f64 - 0.5;
    let min_z_bound = min_z as f64 - 0.5;
    let max_x_bound = max_x as f64 - 0.5;
    let max_y_bound = max_y as f64 - 0.5;
    let max_z_bound = max_z as f64 - 0.5;

    let mut min_view_x = f64::INFINITY;
    let mut max_view_x = f64::NEG_INFINITY;
    let mut min_view_y = f64::INFINITY;
    let mut max_view_y = f64::NEG_INFINITY;
    let mut max_depth = f64::NEG_INFINITY;

    for z in [min_z_bound, max_z_bound] {
        for y in [min_y_bound, max_y_bound] {
            for x in [min_x_bound, max_x_bound] {
                let world = spec.grid_to_world.transform_point_vec(DVec3::new(x, y, z));
                let relative = world - view.eye;
                let view_x = relative.dot(view.right);
                let view_y = relative.dot(view.up);
                let depth = relative.dot(view.forward);
                min_view_x = min_view_x.min(view_x);
                max_view_x = max_view_x.max(view_x);
                min_view_y = min_view_y.min(view_y);
                max_view_y = max_view_y.max(view_y);
                max_depth = max_depth.max(depth);
            }
        }
    }

    max_depth >= -EPSILON
        && max_view_x >= -view.half_width - EPSILON
        && min_view_x <= view.half_width + EPSILON
        && max_view_y >= -view.half_height - EPSILON
        && min_view_y <= view.half_height + EPSILON
}

fn stepped_pixel_indices(extent: u64, stride: u64) -> impl Iterator<Item = u64> {
    (0..extent).step_by(stride as usize)
}

fn collect_ray_bricks(ray: GridRay, spec: BrickGridSpec, bricks: &mut HashSet<SpatialBrickIndex>) {
    if ray.direction.length_squared() <= EPSILON {
        return;
    }
    let Some(hit) = intersect_grid_box(ray, spec.volume_shape) else {
        return;
    };

    let entry = ray.origin + ray.direction * hit.enter;
    let mut x = AxisTraversal::new(entry.x, ray.direction.x, hit.enter, spec.volume_shape.x());
    let mut y = AxisTraversal::new(entry.y, ray.direction.y, hit.enter, spec.volume_shape.y());
    let mut z = AxisTraversal::new(entry.z, ray.direction.z, hit.enter, spec.volume_shape.z());

    loop {
        if !x.is_inside() || !y.is_inside() || !z.is_inside() {
            break;
        }

        bricks.insert(SpatialBrickIndex::new(
            z.index as u64 / spec.brick_shape.z(),
            y.index as u64 / spec.brick_shape.y(),
            x.index as u64 / spec.brick_shape.x(),
        ));

        let next_t = x.next_t.min(y.next_t.min(z.next_t));
        if !next_t.is_finite() || next_t > hit.exit + EPSILON {
            break;
        }

        if x.next_t <= next_t + EPSILON {
            x.advance();
        }
        if y.next_t <= next_t + EPSILON {
            y.advance();
        }
        if z.next_t <= next_t + EPSILON {
            z.advance();
        }
    }
}

impl AxisTraversal {
    fn new(entry_coordinate: f64, direction: f64, entry_t: f64, limit: u64) -> Self {
        let limit = limit as i64;
        let index = initial_voxel_index(entry_coordinate, direction, limit);
        if direction > EPSILON {
            let next_boundary = index as f64 + 0.5;
            Self {
                index,
                step: 1,
                next_t: entry_t + ((next_boundary - entry_coordinate) / direction).max(0.0),
                delta_t: 1.0 / direction,
                limit,
            }
        } else if direction < -EPSILON {
            let next_boundary = index as f64 - 0.5;
            Self {
                index,
                step: -1,
                next_t: entry_t + ((next_boundary - entry_coordinate) / direction).max(0.0),
                delta_t: -1.0 / direction,
                limit,
            }
        } else {
            Self {
                index,
                step: 0,
                next_t: f64::INFINITY,
                delta_t: f64::INFINITY,
                limit,
            }
        }
    }

    fn is_inside(self) -> bool {
        self.index >= 0 && self.index < self.limit
    }

    fn advance(&mut self) {
        if self.step == 0 {
            return;
        }
        self.index += self.step;
        self.next_t += self.delta_t;
    }
}

fn initial_voxel_index(coordinate: f64, direction: f64, limit: i64) -> i64 {
    let adjusted = if direction < -EPSILON {
        coordinate + 0.5 - EPSILON
    } else {
        coordinate + 0.5
    };
    (adjusted.floor() as i64).clamp(0, limit - 1)
}

fn intersect_grid_box(ray: GridRay, shape: Shape3D) -> Option<RayBoxHit> {
    let mut enter = f64::NEG_INFINITY;
    let mut exit = f64::INFINITY;

    slab(
        ray.origin.x,
        ray.direction.x,
        -0.5,
        shape.x() as f64 - 0.5,
        &mut enter,
        &mut exit,
    )?;
    slab(
        ray.origin.y,
        ray.direction.y,
        -0.5,
        shape.y() as f64 - 0.5,
        &mut enter,
        &mut exit,
    )?;
    slab(
        ray.origin.z,
        ray.direction.z,
        -0.5,
        shape.z() as f64 - 0.5,
        &mut enter,
        &mut exit,
    )?;

    if exit < enter || exit < 0.0 {
        return None;
    }

    Some(RayBoxHit {
        enter: enter.max(0.0),
        exit,
    })
}

fn slab(
    origin: f64,
    direction: f64,
    minimum: f64,
    maximum: f64,
    enter: &mut f64,
    exit: &mut f64,
) -> Option<()> {
    if direction.abs() <= EPSILON {
        if origin < minimum || origin > maximum {
            return None;
        }
        return Some(());
    }

    let near = (minimum - origin) / direction;
    let far = (maximum - origin) / direction;
    let axis_enter = near.min(far);
    let axis_exit = near.max(far);
    *enter = enter.max(axis_enter);
    *exit = exit.min(axis_exit);
    Some(())
}

#[cfg(test)]
mod tests {
    use glam::DVec3;
    use mirante4d_domain::Projection;

    use super::*;

    #[test]
    fn front_orthographic_plan_visits_all_depth_bricks() {
        let spec = test_grid_spec();
        let camera = front_camera(Projection::Orthographic, 4.0, 10.0);
        let viewport = RenderViewport::new(4, 4).unwrap();

        let bricks =
            plan_visible_bricks(camera, viewport, spec, BrickPlanOptions::default()).unwrap();

        assert_eq!(bricks.len(), 8);
        assert_eq!(bricks[0], SpatialBrickIndex::new(0, 0, 0));
        assert_eq!(bricks[7], SpatialBrickIndex::new(1, 1, 1));
    }

    #[test]
    fn orthographic_dolly_does_not_change_visible_bricks() {
        let spec = test_grid_spec();
        let near = front_camera(Projection::Orthographic, 4.0, 10.0);
        let far = front_camera(Projection::Orthographic, 4.0, 100.0);
        let viewport = RenderViewport::new(4, 4).unwrap();

        let near_bricks =
            plan_visible_bricks(near, viewport, spec, BrickPlanOptions::default()).unwrap();
        let far_bricks =
            plan_visible_bricks(far, viewport, spec, BrickPlanOptions::default()).unwrap();

        assert_eq!(far_bricks, near_bricks);
    }

    #[test]
    fn orthographic_volume_plan_is_stride_independent() {
        let spec = test_grid_spec();
        let camera = front_camera(Projection::Orthographic, 4.0, 10.0);
        let viewport = RenderViewport::new(4, 4).unwrap();

        let exact =
            plan_visible_bricks(camera, viewport, spec, BrickPlanOptions { pixel_stride: 1 })
                .unwrap();
        let coarse =
            plan_visible_bricks(camera, viewport, spec, BrickPlanOptions { pixel_stride: 4 })
                .unwrap();

        assert_eq!(coarse, exact);
    }

    #[test]
    fn orthographic_volume_plan_includes_partially_visible_edge_bricks() {
        let spec = test_grid_spec();
        let camera = front_camera_at(
            Projection::Orthographic,
            DVec3::new(0.75, 0.75, 2.0),
            4.0,
            10.0,
        );
        let viewport = RenderViewport::new(4, 4).unwrap();

        let bricks =
            plan_visible_bricks(camera, viewport, spec, BrickPlanOptions { pixel_stride: 4 })
                .unwrap();

        assert_eq!(bricks.len(), 8);
        assert!(bricks.contains(&SpatialBrickIndex::new(0, 0, 0)));
        assert!(bricks.contains(&SpatialBrickIndex::new(0, 0, 1)));
        assert!(bricks.contains(&SpatialBrickIndex::new(1, 1, 0)));
        assert!(bricks.contains(&SpatialBrickIndex::new(1, 1, 1)));
    }

    #[test]
    fn zoomed_orthographic_plan_returns_current_ray_bricks() {
        let spec = test_grid_spec();
        let camera = front_camera_at(
            Projection::Orthographic,
            DVec3::new(2.25, 2.25, 2.0),
            1.0,
            10.0,
        );
        let viewport = RenderViewport::new(1, 1).unwrap();

        let bricks =
            plan_visible_bricks(camera, viewport, spec, BrickPlanOptions::default()).unwrap();

        assert_eq!(
            bricks,
            vec![
                SpatialBrickIndex::new(0, 1, 1),
                SpatialBrickIndex::new(1, 1, 1)
            ]
        );
    }

    #[test]
    fn perspective_plan_is_nonempty_and_in_bounds() {
        let spec = test_grid_spec();
        let camera = front_camera(Projection::Perspective, 4.0, 6.0);
        let viewport = RenderViewport::new(3, 3).unwrap();

        let bricks =
            plan_visible_bricks(camera, viewport, spec, BrickPlanOptions::default()).unwrap();

        assert!(!bricks.is_empty());
        assert!(
            bricks
                .iter()
                .all(|brick| brick.z < 2 && brick.y < 2 && brick.x < 2)
        );
    }

    #[test]
    fn rejects_zero_planner_stride() {
        let err = plan_visible_bricks(
            front_camera(Projection::Orthographic, 4.0, 10.0),
            RenderViewport::new(4, 4).unwrap(),
            test_grid_spec(),
            BrickPlanOptions { pixel_stride: 0 },
        )
        .unwrap_err();

        assert_eq!(err, RenderError::InvalidBrickPixelStride);
    }

    fn test_grid_spec() -> BrickGridSpec {
        BrickGridSpec {
            volume_shape: Shape3D::new(4, 4, 4).unwrap(),
            brick_shape: Shape3D::new(2, 2, 2).unwrap(),
            grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
        }
    }

    fn front_camera(projection: Projection, height: f64, eye_distance_z: f64) -> CameraFrame {
        front_camera_at(
            projection,
            DVec3::new(2.0, 2.0, 2.0),
            height,
            eye_distance_z,
        )
    }

    fn front_camera_at(
        projection: Projection,
        target: DVec3,
        height: f64,
        eye_distance_z: f64,
    ) -> CameraFrame {
        crate::current_camera::frame_from_look_at(
            projection,
            DVec3::new(target.x, target.y, target.z - eye_distance_z),
            target,
            -DVec3::Y,
            1.0,
            height / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan()),
            crate::current_camera::presentation(height, height),
        )
    }
}

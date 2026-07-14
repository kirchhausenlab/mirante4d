//! Storage-independent resource-region planning for product views.

use std::{collections::HashSet, fmt};

use glam::{DMat4, DQuat, DVec3};
use mirante4d_dataset::{ResourceContractError, ResourceRegion};
use mirante4d_domain::{CrossSectionView, GridToWorld, Projection, Shape3D};
use mirante4d_render_api::{CameraFrame, PresentationViewport, RenderApiError, RenderExtent};

const EPSILON: f64 = 1.0e-9;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RegionIndex {
    z: u64,
    y: u64,
    x: u64,
}

impl RegionIndex {
    const fn new(z: u64, y: u64, x: u64) -> Self {
        Self { z, y, x }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SemanticRegionGridSpec {
    pub(crate) volume_shape: Shape3D,
    pub(crate) resource_shape: Shape3D,
    pub(crate) grid_to_world: GridToWorld,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VolumePlanOptions {
    pub(crate) pixel_stride: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SemanticPlanLimits {
    pub(crate) max_candidates: usize,
    pub(crate) max_resources: usize,
}

impl SemanticPlanLimits {
    pub(crate) const fn new(max_candidates: usize, max_resources: usize) -> Self {
        Self {
            max_candidates,
            max_resources,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CrossSectionPlane {
    Xy,
    Xz,
    Yz,
}

impl CrossSectionPlane {
    fn relative_orientation(self) -> DQuat {
        match self {
            Self::Xy => DQuat::IDENTITY,
            Self::Xz => DQuat::from_rotation_x(std::f64::consts::FRAC_PI_2),
            Self::Yz => DQuat::from_rotation_y(std::f64::consts::FRAC_PI_2),
        }
    }
}

#[derive(Debug)]
pub(crate) enum SemanticPlanError {
    InvalidPixelStride,
    Capacity { kind: &'static str, maximum: usize },
    NonInvertibleTransform,
    Resource(ResourceContractError),
    Camera(RenderApiError),
}

impl SemanticPlanError {
    pub(crate) const fn is_capacity(&self) -> bool {
        matches!(self, Self::Capacity { .. })
    }

    const fn capacity(kind: &'static str, maximum: usize) -> Self {
        Self::Capacity { kind, maximum }
    }
}

impl fmt::Display for SemanticPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPixelStride => {
                formatter.write_str("semantic planner pixel stride must be positive")
            }
            Self::Capacity { kind, maximum } => {
                write!(
                    formatter,
                    "semantic planning exceeded the {kind} limit of {maximum}"
                )
            }
            Self::NonInvertibleTransform => {
                formatter.write_str("grid-to-world matrix must be invertible")
            }
            Self::Resource(error) => error.fmt(formatter),
            Self::Camera(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SemanticPlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Resource(error) => Some(error),
            Self::Camera(error) => Some(error),
            Self::InvalidPixelStride | Self::Capacity { .. } | Self::NonInvertibleTransform => None,
        }
    }
}

impl From<ResourceContractError> for SemanticPlanError {
    fn from(error: ResourceContractError) -> Self {
        Self::Resource(error)
    }
}

impl From<RenderApiError> for SemanticPlanError {
    fn from(error: RenderApiError) -> Self {
        Self::Camera(error)
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CandidateBounds {
    z_min: u64,
    z_max: u64,
    y_min: u64,
    y_max: u64,
    x_min: u64,
    x_max: u64,
}

impl CandidateBounds {
    fn count(self) -> usize {
        self.z_max
            .saturating_sub(self.z_min)
            .saturating_add(1)
            .saturating_mul(self.y_max.saturating_sub(self.y_min).saturating_add(1))
            .saturating_mul(self.x_max.saturating_sub(self.x_min).saturating_add(1))
            .try_into()
            .unwrap_or(usize::MAX)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct CrossSectionBasis {
    right_world: DVec3,
    down_world: DVec3,
    normal_away_world: DVec3,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct CrossSectionSlab {
    center_world: DVec3,
    basis: CrossSectionBasis,
    half_width_world: f64,
    half_height_world: f64,
    half_depth_world: f64,
}

pub(crate) fn plan_visible_resource_regions(
    camera: CameraFrame,
    extent: RenderExtent,
    spec: SemanticRegionGridSpec,
    options: VolumePlanOptions,
    limits: SemanticPlanLimits,
) -> Result<Vec<ResourceRegion>, SemanticPlanError> {
    if options.pixel_stride == 0 {
        return Err(SemanticPlanError::InvalidPixelStride);
    }

    let indices = if camera.view().projection() == Projection::Orthographic {
        plan_orthographic_regions(camera, extent, spec, limits)?
    } else {
        plan_perspective_regions(camera, extent, spec, options, limits)?
    };
    indices
        .into_iter()
        .map(|index| semantic_region(spec, index))
        .collect()
}

pub(crate) fn plan_cross_section_resource_regions(
    view: CrossSectionView,
    panel: CrossSectionPlane,
    presentation: PresentationViewport,
    spec: SemanticRegionGridSpec,
    limits: SemanticPlanLimits,
) -> Result<Vec<ResourceRegion>, SemanticPlanError> {
    let orientation = DQuat::from_array(view.orientation().xyzw()) * panel.relative_orientation();
    let basis = CrossSectionBasis {
        right_world: (orientation * DVec3::X).normalize(),
        down_world: (orientation * DVec3::Y).normalize(),
        normal_away_world: (orientation * DVec3::Z).normalize(),
    };
    let slab = CrossSectionSlab {
        center_world: DVec3::from_array(view.center_world().components()),
        basis,
        half_width_world: presentation.width_points() * view.scale_world_per_screen_point() * 0.5,
        half_height_world: presentation.height_points() * view.scale_world_per_screen_point() * 0.5,
        half_depth_world: view.depth_world() * 0.5,
    };
    let grid_shape = region_grid_shape(spec.volume_shape, spec.resource_shape);
    let Some(bounds) = cross_section_candidate_bounds(slab, spec, grid_shape) else {
        return Ok(Vec::new());
    };
    let candidate_count = bounds.count();
    if candidate_count > limits.max_candidates {
        return Err(SemanticPlanError::capacity(
            "candidate",
            limits.max_candidates,
        ));
    }

    let mut regions = Vec::with_capacity(candidate_count.min(limits.max_resources));
    for z in bounds.z_min..=bounds.z_max {
        for y in bounds.y_min..=bounds.y_max {
            for x in bounds.x_min..=bounds.x_max {
                let index = RegionIndex::new(z, y, x);
                if cross_section_intersects_region(slab, spec, index) {
                    if regions.len() == limits.max_resources {
                        return Err(SemanticPlanError::capacity(
                            "resource",
                            limits.max_resources,
                        ));
                    }
                    regions.push(semantic_region(spec, index)?);
                }
            }
        }
    }
    Ok(regions)
}

fn plan_perspective_regions(
    camera: CameraFrame,
    extent: RenderExtent,
    spec: SemanticRegionGridSpec,
    options: VolumePlanOptions,
    limits: SemanticPlanLimits,
) -> Result<Vec<RegionIndex>, SemanticPlanError> {
    let world_to_grid = inverse_grid_to_world(spec.grid_to_world)?;
    let mut regions = HashSet::new();
    let mut candidates = 0_usize;
    let width = extent.width_pixels();
    let height = extent.height_pixels();

    for row in stepped_pixel_indices(height, options.pixel_stride) {
        for column in stepped_pixel_indices(width, options.pixel_stride) {
            let ray =
                camera.ray_for_render_pixel(f64::from(column), f64::from(row), width, height)?;
            let ray = GridRay {
                origin: world_to_grid
                    .transform_point3(DVec3::from_array(ray.origin().components())),
                direction: world_to_grid.transform_vector3(DVec3::from_array(ray.direction())),
            };
            collect_ray_regions(ray, spec, &mut regions, &mut candidates, limits)?;
        }
    }

    let mut regions = regions.into_iter().collect::<Vec<_>>();
    regions.sort_by_key(|index| (index.z, index.y, index.x));
    Ok(regions)
}

fn plan_orthographic_regions(
    camera: CameraFrame,
    extent: RenderExtent,
    spec: SemanticRegionGridSpec,
    limits: SemanticPlanLimits,
) -> Result<Vec<RegionIndex>, SemanticPlanError> {
    let Some((forward, right, up)) = camera_basis(camera) else {
        return Ok(Vec::new());
    };
    let view = OrthographicView {
        eye: camera_eye(camera),
        forward,
        right,
        up,
        half_width: sampled_center_half_extent(
            camera.orthographic_world_span_width()? * 0.5,
            u64::from(extent.width_pixels()),
        ),
        half_height: sampled_center_half_extent(
            camera.orthographic_world_span_height()? * 0.5,
            u64::from(extent.height_pixels()),
        ),
    };
    let grid_shape = region_grid_shape(spec.volume_shape, spec.resource_shape);
    let Some(bounds) = orthographic_candidate_bounds(view, spec, grid_shape)? else {
        return Ok(Vec::new());
    };
    let candidate_count = bounds.count();
    if candidate_count > limits.max_candidates {
        return Err(SemanticPlanError::capacity(
            "candidate",
            limits.max_candidates,
        ));
    }

    let mut regions = Vec::with_capacity(candidate_count.min(limits.max_resources));
    for z in bounds.z_min..=bounds.z_max {
        for y in bounds.y_min..=bounds.y_max {
            for x in bounds.x_min..=bounds.x_max {
                let index = RegionIndex::new(z, y, x);
                if orthographic_region_overlaps_view(view, spec, index) {
                    if regions.len() == limits.max_resources {
                        return Err(SemanticPlanError::capacity(
                            "resource",
                            limits.max_resources,
                        ));
                    }
                    regions.push(index);
                }
            }
        }
    }
    Ok(regions)
}

fn semantic_region(
    spec: SemanticRegionGridSpec,
    index: RegionIndex,
) -> Result<ResourceRegion, SemanticPlanError> {
    let tile_index = [index.z, index.y, index.x];
    let tile_shape = spec.resource_shape.dimensions();
    let volume_shape = spec.volume_shape.dimensions();
    let mut origin = [0_u64; 3];
    for axis in 0..3 {
        origin[axis] = tile_index[axis]
            .checked_mul(tile_shape[axis])
            .ok_or(ResourceContractError::RegionEndOverflow { axis })?;
    }
    let shape = Shape3D::new(
        tile_shape[0].min(volume_shape[0] - origin[0]),
        tile_shape[1].min(volume_shape[1] - origin[1]),
        tile_shape[2].min(volume_shape[2] - origin[2]),
    )
    .expect("a planned in-bounds resource has a nonzero clipped shape");
    Ok(ResourceRegion::new(origin, shape)?)
}

fn orthographic_candidate_bounds(
    view: OrthographicView,
    spec: SemanticRegionGridSpec,
    grid_shape: Shape3D,
) -> Result<Option<CandidateBounds>, SemanticPlanError> {
    let mut min_depth = f64::INFINITY;
    let mut max_depth = f64::NEG_INFINITY;
    for corner in volume_grid_corners(spec.volume_shape) {
        let world = transform_grid_point(spec.grid_to_world, corner);
        let depth = (world - view.eye).dot(view.forward);
        min_depth = min_depth.min(depth);
        max_depth = max_depth.max(depth);
    }
    if max_depth < -EPSILON {
        return Ok(None);
    }

    let world_to_grid = inverse_grid_to_world(spec.grid_to_world)?;
    let near_depth = min_depth.max(0.0);
    let far_depth = max_depth.max(0.0);
    let mut grid_min = DVec3::splat(f64::INFINITY);
    let mut grid_max = DVec3::splat(f64::NEG_INFINITY);
    for depth in [near_depth, far_depth] {
        for view_y in [-view.half_height, view.half_height] {
            for view_x in [-view.half_width, view.half_width] {
                let world =
                    view.eye + view.forward * depth + view.right * view_x + view.up * view_y;
                let grid = world_to_grid.transform_point3(world);
                grid_min = grid_min.min(grid);
                grid_max = grid_max.max(grid);
            }
        }
    }

    Ok(candidate_bounds_from_grid_box(
        grid_min, grid_max, spec, grid_shape,
    ))
}

fn cross_section_candidate_bounds(
    slab: CrossSectionSlab,
    spec: SemanticRegionGridSpec,
    grid_shape: Shape3D,
) -> Option<CandidateBounds> {
    let world_to_grid = inverse_grid_to_world(spec.grid_to_world).ok()?;
    let mut grid_min = DVec3::splat(f64::INFINITY);
    let mut grid_max = DVec3::splat(f64::NEG_INFINITY);
    for corner in cross_section_slab_corners(slab) {
        let grid = world_to_grid.transform_point3(corner);
        grid_min = grid_min.min(grid);
        grid_max = grid_max.max(grid);
    }
    candidate_bounds_from_grid_box(grid_min, grid_max, spec, grid_shape)
}

fn candidate_bounds_from_grid_box(
    grid_min: DVec3,
    grid_max: DVec3,
    spec: SemanticRegionGridSpec,
    grid_shape: Shape3D,
) -> Option<CandidateBounds> {
    let (x_min, x_max) = candidate_axis_bounds(
        grid_min.x,
        grid_max.x,
        spec.volume_shape.x(),
        spec.resource_shape.x(),
        grid_shape.x(),
    )?;
    let (y_min, y_max) = candidate_axis_bounds(
        grid_min.y,
        grid_max.y,
        spec.volume_shape.y(),
        spec.resource_shape.y(),
        grid_shape.y(),
    )?;
    let (z_min, z_max) = candidate_axis_bounds(
        grid_min.z,
        grid_max.z,
        spec.volume_shape.z(),
        spec.resource_shape.z(),
        grid_shape.z(),
    )?;
    Some(CandidateBounds {
        z_min,
        z_max,
        y_min,
        y_max,
        x_min,
        x_max,
    })
}

fn candidate_axis_bounds(
    min_coord: f64,
    max_coord: f64,
    volume_axis: u64,
    resource_axis: u64,
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
    let lower = (((min_coord + 0.5 - EPSILON) / resource_axis as f64).floor() as i64)
        .clamp(0, last as i64) as u64;
    let upper = (((max_coord + 0.5 + EPSILON) / resource_axis as f64).floor() as i64)
        .clamp(0, last as i64) as u64;
    (lower <= upper).then_some((lower.saturating_sub(1), upper.saturating_add(1).min(last)))
}

fn orthographic_region_overlaps_view(
    view: OrthographicView,
    spec: SemanticRegionGridSpec,
    index: RegionIndex,
) -> bool {
    let Some(corners) = region_world_corners(spec, index) else {
        return false;
    };
    let mut min_view_x = f64::INFINITY;
    let mut max_view_x = f64::NEG_INFINITY;
    let mut min_view_y = f64::INFINITY;
    let mut max_view_y = f64::NEG_INFINITY;
    let mut max_depth = f64::NEG_INFINITY;
    for world in corners {
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
    max_depth >= -EPSILON
        && max_view_x >= -view.half_width - EPSILON
        && min_view_x <= view.half_width + EPSILON
        && max_view_y >= -view.half_height - EPSILON
        && min_view_y <= view.half_height + EPSILON
}

fn cross_section_intersects_region(
    slab: CrossSectionSlab,
    spec: SemanticRegionGridSpec,
    index: RegionIndex,
) -> bool {
    let Some(corners) = region_world_corners(spec, index) else {
        return false;
    };
    let mut min_right = f64::INFINITY;
    let mut max_right = f64::NEG_INFINITY;
    let mut min_down = f64::INFINITY;
    let mut max_down = f64::NEG_INFINITY;
    let mut min_normal = f64::INFINITY;
    let mut max_normal = f64::NEG_INFINITY;
    for corner in corners {
        let relative = corner - slab.center_world;
        let right = relative.dot(slab.basis.right_world);
        let down = relative.dot(slab.basis.down_world);
        let normal = relative.dot(slab.basis.normal_away_world);
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
        -slab.half_width_world,
        slab.half_width_world,
    ) && ranges_overlap(
        min_down,
        max_down,
        -slab.half_height_world,
        slab.half_height_world,
    ) && ranges_overlap(
        min_normal,
        max_normal,
        -slab.half_depth_world,
        slab.half_depth_world,
    )
}

fn collect_ray_regions(
    ray: GridRay,
    spec: SemanticRegionGridSpec,
    regions: &mut HashSet<RegionIndex>,
    candidates: &mut usize,
    limits: SemanticPlanLimits,
) -> Result<(), SemanticPlanError> {
    if ray.direction.length_squared() <= EPSILON {
        return Ok(());
    }
    let Some(hit) = intersect_grid_box(ray, spec.volume_shape) else {
        return Ok(());
    };

    let entry = ray.origin + ray.direction * hit.enter;
    let grid_shape = region_grid_shape(spec.volume_shape, spec.resource_shape);
    let region_entry = DVec3::new(
        region_axis_coordinate(entry.x, spec.resource_shape.x()),
        region_axis_coordinate(entry.y, spec.resource_shape.y()),
        region_axis_coordinate(entry.z, spec.resource_shape.z()),
    );
    let region_direction = DVec3::new(
        ray.direction.x / spec.resource_shape.x() as f64,
        ray.direction.y / spec.resource_shape.y() as f64,
        ray.direction.z / spec.resource_shape.z() as f64,
    );
    let mut x = AxisTraversal::new(
        region_entry.x,
        region_direction.x,
        hit.enter,
        grid_shape.x(),
    );
    let mut y = AxisTraversal::new(
        region_entry.y,
        region_direction.y,
        hit.enter,
        grid_shape.y(),
    );
    let mut z = AxisTraversal::new(
        region_entry.z,
        region_direction.z,
        hit.enter,
        grid_shape.z(),
    );

    loop {
        if !x.is_inside() || !y.is_inside() || !z.is_inside() {
            break;
        }
        if *candidates == limits.max_candidates {
            return Err(SemanticPlanError::capacity(
                "candidate",
                limits.max_candidates,
            ));
        }
        *candidates += 1;

        let index = RegionIndex::new(z.index as u64, y.index as u64, x.index as u64);
        if !regions.contains(&index) {
            if regions.len() == limits.max_resources {
                return Err(SemanticPlanError::capacity(
                    "resource",
                    limits.max_resources,
                ));
            }
            regions.insert(index);
        }

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
    Ok(())
}

impl AxisTraversal {
    fn new(entry_coordinate: f64, direction: f64, entry_t: f64, limit: u64) -> Self {
        let limit = limit as i64;
        let index = initial_region_index(entry_coordinate, direction, limit);
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
        if self.step != 0 {
            self.index += self.step;
            self.next_t += self.delta_t;
        }
    }
}

fn region_axis_coordinate(grid_coordinate: f64, resource_axis: u64) -> f64 {
    (grid_coordinate + 0.5) / resource_axis as f64 - 0.5
}

fn initial_region_index(coordinate: f64, direction: f64, limit: i64) -> i64 {
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
    ray_box_axis(
        ray.origin.x,
        ray.direction.x,
        -0.5,
        shape.x() as f64 - 0.5,
        &mut enter,
        &mut exit,
    )?;
    ray_box_axis(
        ray.origin.y,
        ray.direction.y,
        -0.5,
        shape.y() as f64 - 0.5,
        &mut enter,
        &mut exit,
    )?;
    ray_box_axis(
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

fn ray_box_axis(
    origin: f64,
    direction: f64,
    minimum: f64,
    maximum: f64,
    enter: &mut f64,
    exit: &mut f64,
) -> Option<()> {
    if direction.abs() <= EPSILON {
        return (minimum..=maximum).contains(&origin).then_some(());
    }
    let near = (minimum - origin) / direction;
    let far = (maximum - origin) / direction;
    *enter = enter.max(near.min(far));
    *exit = exit.min(near.max(far));
    Some(())
}

fn region_grid_shape(volume_shape: Shape3D, resource_shape: Shape3D) -> Shape3D {
    Shape3D::new(
        volume_shape.z().div_ceil(resource_shape.z()),
        volume_shape.y().div_ceil(resource_shape.y()),
        volume_shape.x().div_ceil(resource_shape.x()),
    )
    .expect("nonzero shapes produce a nonzero resource grid")
}

fn region_world_corners(spec: SemanticRegionGridSpec, index: RegionIndex) -> Option<[DVec3; 8]> {
    let min_x = index.x.checked_mul(spec.resource_shape.x())?;
    let min_y = index.y.checked_mul(spec.resource_shape.y())?;
    let min_z = index.z.checked_mul(spec.resource_shape.z())?;
    if min_x >= spec.volume_shape.x()
        || min_y >= spec.volume_shape.y()
        || min_z >= spec.volume_shape.z()
    {
        return None;
    }
    let max_x = min_x
        .saturating_add(spec.resource_shape.x())
        .min(spec.volume_shape.x());
    let max_y = min_y
        .saturating_add(spec.resource_shape.y())
        .min(spec.volume_shape.y());
    let max_z = min_z
        .saturating_add(spec.resource_shape.z())
        .min(spec.volume_shape.z());
    let xs = [min_x as f64 - 0.5, max_x as f64 - 0.5];
    let ys = [min_y as f64 - 0.5, max_y as f64 - 0.5];
    let zs = [min_z as f64 - 0.5, max_z as f64 - 0.5];
    Some([
        transform_grid_point(spec.grid_to_world, DVec3::new(xs[0], ys[0], zs[0])),
        transform_grid_point(spec.grid_to_world, DVec3::new(xs[1], ys[0], zs[0])),
        transform_grid_point(spec.grid_to_world, DVec3::new(xs[0], ys[1], zs[0])),
        transform_grid_point(spec.grid_to_world, DVec3::new(xs[1], ys[1], zs[0])),
        transform_grid_point(spec.grid_to_world, DVec3::new(xs[0], ys[0], zs[1])),
        transform_grid_point(spec.grid_to_world, DVec3::new(xs[1], ys[0], zs[1])),
        transform_grid_point(spec.grid_to_world, DVec3::new(xs[0], ys[1], zs[1])),
        transform_grid_point(spec.grid_to_world, DVec3::new(xs[1], ys[1], zs[1])),
    ])
}

fn volume_grid_corners(shape: Shape3D) -> [DVec3; 8] {
    let xs = [-0.5, shape.x() as f64 - 0.5];
    let ys = [-0.5, shape.y() as f64 - 0.5];
    let zs = [-0.5, shape.z() as f64 - 0.5];
    [
        DVec3::new(xs[0], ys[0], zs[0]),
        DVec3::new(xs[1], ys[0], zs[0]),
        DVec3::new(xs[0], ys[1], zs[0]),
        DVec3::new(xs[1], ys[1], zs[0]),
        DVec3::new(xs[0], ys[0], zs[1]),
        DVec3::new(xs[1], ys[0], zs[1]),
        DVec3::new(xs[0], ys[1], zs[1]),
        DVec3::new(xs[1], ys[1], zs[1]),
    ]
}

fn cross_section_slab_corners(slab: CrossSectionSlab) -> [DVec3; 8] {
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

fn camera_basis(camera: CameraFrame) -> Option<(DVec3, DVec3, DVec3)> {
    let forward = DVec3::from_array(camera.view().target().components()) - camera_eye(camera);
    if forward.length_squared() <= EPSILON {
        return None;
    }
    let forward = forward.normalize();
    let right = forward.cross(DVec3::from_array(camera.axes().up()));
    if right.length_squared() <= EPSILON {
        return None;
    }
    let right = right.normalize();
    let up = right.cross(forward).normalize();
    Some((forward, right, up))
}

fn camera_eye(camera: CameraFrame) -> DVec3 {
    DVec3::from_array(camera.eye().components())
}

fn sampled_center_half_extent(half_extent: f64, pixels: u64) -> f64 {
    half_extent * (1.0 - 1.0 / pixels as f64).max(0.0)
}

fn stepped_pixel_indices(extent: u32, stride: u64) -> impl Iterator<Item = u32> {
    (0..extent).step_by(stride as usize)
}

fn ranges_overlap(min_a: f64, max_a: f64, min_b: f64, max_b: f64) -> bool {
    max_a >= min_b - EPSILON && min_a <= max_b + EPSILON
}

fn transform_grid_point(transform: GridToWorld, point: DVec3) -> DVec3 {
    grid_to_world_matrix(transform).transform_point3(point)
}

fn inverse_grid_to_world(transform: GridToWorld) -> Result<DMat4, SemanticPlanError> {
    let matrix = grid_to_world_matrix(transform);
    let inverse = matrix.inverse();
    if inverse.is_finite() && (matrix * inverse).abs_diff_eq(DMat4::IDENTITY, EPSILON) {
        Ok(inverse)
    } else {
        Err(SemanticPlanError::NonInvertibleTransform)
    }
}

fn grid_to_world_matrix(transform: GridToWorld) -> DMat4 {
    let row_major = transform.row_major();
    let mut column_major = [0.0; 16];
    for row in 0..4 {
        for column in 0..4 {
            column_major[column * 4 + row] = row_major[row * 4 + column];
        }
    }
    DMat4::from_cols_array(&column_major)
}

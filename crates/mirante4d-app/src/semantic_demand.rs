//! Storage-independent resource-region planning for product views.

use std::fmt;

use glam::{DMat4, DQuat, DVec3};
use mirante4d_dataset::{
    CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError, ResourceContractError,
    ResourceRegion,
};
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
    Capacity { kind: &'static str, maximum: usize },
    Cancelled,
    NonInvertibleTransform,
    Resource(ResourceContractError),
    Camera(RenderApiError),
    ScratchCapacity(CpuLedgerError),
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
            Self::Capacity { kind, maximum } => {
                write!(
                    formatter,
                    "semantic planning exceeded the {kind} limit of {maximum}"
                )
            }
            Self::Cancelled => formatter.write_str("semantic planning was cancelled"),
            Self::NonInvertibleTransform => {
                formatter.write_str("grid-to-world matrix must be invertible")
            }
            Self::Resource(error) => error.fmt(formatter),
            Self::Camera(error) => error.fmt(formatter),
            Self::ScratchCapacity(error) => write!(formatter, "semantic planning scratch: {error}"),
        }
    }
}

impl std::error::Error for SemanticPlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Resource(error) => Some(error),
            Self::Camera(error) => Some(error),
            Self::ScratchCapacity(error) => Some(error),
            Self::Capacity { .. } | Self::Cancelled | Self::NonInvertibleTransform => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SemanticPlanWork {
    pub(crate) candidates_visited: usize,
}

pub(crate) struct VisibleResourceRegionPlan {
    pub(crate) regions: Vec<ResourceRegion>,
    /// Prefix visible to the unexpanded camera. Any following regions belong
    /// only to a bounded navigation guard and are always lower priority.
    pub(crate) primary_regions: usize,
    pub(crate) work: SemanticPlanWork,
    /// Keeps the candidate/output allocation charged until its caller has
    /// converted the region vector into the final prepared result body.
    pub(crate) scratch_charge: Option<Box<dyn CpuByteLease>>,
}

impl fmt::Debug for VisibleResourceRegionPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VisibleResourceRegionPlan")
            .field("regions", &self.regions)
            .field("primary_regions", &self.primary_regions)
            .field("work", &self.work)
            .field(
                "scratch_charge_bytes",
                &self
                    .scratch_charge
                    .as_ref()
                    .map(|charge| charge.reserved_bytes()),
            )
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct VisibilityHalfSpace {
    normal: DVec3,
    offset: f64,
}

impl VisibilityHalfSpace {
    fn through_eye(normal: DVec3, eye: DVec3) -> Self {
        Self {
            normal,
            offset: -normal.dot(eye),
        }
    }

    fn signed_distance(self, point: DVec3) -> f64 {
        self.normal.dot(point) + self.offset
    }
}

/// Constant-size proof that a later camera cannot expose a brick omitted by
/// one expanded semantic plan. It stores only five guard half-spaces and the
/// affine volume corners for each visible layer/scale; it is deliberately not
/// a scene graph or per-brick spatial index.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SemanticCameraReuseEnvelope {
    projection: Projection,
    guard_planes: [VisibilityHalfSpace; 5],
    volume_corners: Box<[[DVec3; 8]]>,
    reusable_candidates: usize,
}

// Construction rejects every non-finite plane/corner below, so the derived
// floating-point equality is reflexive for all values of this private type.
impl Eq for SemanticCameraReuseEnvelope {}

impl SemanticCameraReuseEnvelope {
    pub(crate) fn new(
        guard_camera: CameraFrame,
        extent: RenderExtent,
        specs: impl IntoIterator<Item = SemanticRegionGridSpec>,
        reusable_candidates: usize,
    ) -> Result<Self, SemanticPlanError> {
        let guard_planes = camera_visibility_planes(guard_camera, extent)?;
        let volume_corners = specs
            .into_iter()
            .map(|spec| {
                volume_grid_corners(spec.volume_shape)
                    .map(|corner| transform_grid_point(spec.grid_to_world, corner))
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        if !guard_planes
            .iter()
            .all(|plane| plane.normal.is_finite() && plane.offset.is_finite())
            || !volume_corners
                .iter()
                .flatten()
                .all(|corner| corner.is_finite())
        {
            return Err(SemanticPlanError::Camera(
                RenderApiError::CameraMathNotFinite,
            ));
        }
        Ok(Self {
            projection: guard_camera.view().projection(),
            guard_planes,
            volume_corners,
            reusable_candidates,
        })
    }

    /// Proves containment using the same corresponding plane tests as the
    /// semantic brick classifier. For each new plane, clipping an affine box
    /// produces vertices only at retained box corners and crossed box edges;
    /// checking those constant 20 points therefore bounds every possible
    /// brick corner without visiting a candidate.
    pub(crate) fn contains(
        &self,
        camera: CameraFrame,
        extent: RenderExtent,
    ) -> Result<bool, SemanticPlanError> {
        if camera.view().projection() != self.projection {
            return Ok(false);
        }
        let next_planes = camera_visibility_planes(camera, extent)?;
        Ok(self.volume_corners.iter().all(|corners| {
            next_planes
                .iter()
                .zip(self.guard_planes.iter())
                .all(|(next, guard)| {
                    clipped_volume_half_space_is_contained(*corners, *next, *guard)
                })
        }))
    }

    pub(crate) const fn reusable_candidates(&self) -> usize {
        self.reusable_candidates
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
struct OrthographicView {
    eye: DVec3,
    forward: DVec3,
    right: DVec3,
    up: DVec3,
    half_width: f64,
    half_height: f64,
}

#[derive(Debug, Clone, Copy)]
struct PerspectiveFrustum {
    eye: DVec3,
    forward: DVec3,
    right: DVec3,
    up: DVec3,
    tan_half_width: f64,
    tan_half_height: f64,
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

#[derive(Debug, Clone, Copy, PartialEq)]
struct ScreenContribution {
    overlap_area: f64,
    center_distance_squared: f64,
    near_depth: f64,
}

#[cfg(test)]
pub(crate) fn plan_visible_resource_regions(
    camera: CameraFrame,
    extent: RenderExtent,
    spec: SemanticRegionGridSpec,
    sampling_footprint_halo_voxels: u64,
    limits: SemanticPlanLimits,
) -> Result<Vec<ResourceRegion>, SemanticPlanError> {
    Ok(plan_visible_resource_regions_cancellable(
        camera,
        extent,
        spec,
        sampling_footprint_halo_voxels,
        limits,
        None,
        || false,
    )?
    .regions)
}

/// Plans exact 3D visibility while allowing a superseded camera generation to
/// abandon its bounded candidate traversal. Cancellation is sampled every 256
/// candidates: frequent enough to make replacement prompt without adding an
/// atomic load to every brick test.
#[cfg(test)]
pub(crate) fn plan_visible_resource_regions_cancellable(
    camera: CameraFrame,
    extent: RenderExtent,
    spec: SemanticRegionGridSpec,
    sampling_footprint_halo_voxels: u64,
    limits: SemanticPlanLimits,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    cancelled: impl FnMut() -> bool,
) -> Result<VisibleResourceRegionPlan, SemanticPlanError> {
    plan_prioritized_visible_resource_regions_cancellable(
        camera,
        None,
        extent,
        spec,
        sampling_footprint_halo_voxels,
        limits,
        scratch_ledger,
        cancelled,
    )
}

/// Plans one expanded visibility envelope while retaining the exact current
/// camera as the first ranking tier. The optional priority camera changes no
/// visibility result: it only partitions the expanded result into current
/// resources followed by guard-only resources.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_prioritized_visible_resource_regions_cancellable(
    camera: CameraFrame,
    priority_camera: Option<CameraFrame>,
    extent: RenderExtent,
    spec: SemanticRegionGridSpec,
    sampling_footprint_halo_voxels: u64,
    limits: SemanticPlanLimits,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    mut cancelled: impl FnMut() -> bool,
) -> Result<VisibleResourceRegionPlan, SemanticPlanError> {
    if cancelled() {
        return Err(SemanticPlanError::Cancelled);
    }
    let planned = if camera.view().projection() == Projection::Orthographic {
        plan_orthographic_regions(
            camera,
            priority_camera,
            extent,
            spec,
            sampling_footprint_halo_voxels,
            limits,
            scratch_ledger,
            &mut cancelled,
        )?
    } else {
        plan_perspective_regions(
            camera,
            priority_camera,
            spec,
            sampling_footprint_halo_voxels,
            limits,
            scratch_ledger,
            &mut cancelled,
        )?
    };
    if cancelled() {
        return Err(SemanticPlanError::Cancelled);
    }
    let mut regions = Vec::with_capacity(planned.indices.len());
    for (offset, index) in planned.indices.into_iter().enumerate() {
        if offset.is_multiple_of(256) && cancelled() {
            return Err(SemanticPlanError::Cancelled);
        }
        regions.push(semantic_region(spec, index)?);
    }
    Ok(VisibleResourceRegionPlan {
        regions,
        primary_regions: planned.primary_regions,
        work: planned.work,
        scratch_charge: planned.scratch_charge,
    })
}

#[cfg(test)]
pub(crate) fn plan_cross_section_resource_regions(
    view: CrossSectionView,
    panel: CrossSectionPlane,
    presentation: PresentationViewport,
    spec: SemanticRegionGridSpec,
    sampling_footprint_halo_voxels: u64,
    limits: SemanticPlanLimits,
) -> Result<Vec<ResourceRegion>, SemanticPlanError> {
    Ok(plan_cross_section_resource_regions_cancellable(
        view,
        panel,
        presentation,
        spec,
        sampling_footprint_halo_voxels,
        limits,
        None,
        || false,
    )?
    .regions)
}

// The planner takes the complete immutable geometry/budget contract plus its
// ledger and cancellation authorities; a wrapper would be used only here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_cross_section_resource_regions_cancellable(
    view: CrossSectionView,
    panel: CrossSectionPlane,
    presentation: PresentationViewport,
    spec: SemanticRegionGridSpec,
    sampling_footprint_halo_voxels: u64,
    limits: SemanticPlanLimits,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    mut cancelled: impl FnMut() -> bool,
) -> Result<VisibleResourceRegionPlan, SemanticPlanError> {
    if cancelled() {
        return Err(SemanticPlanError::Cancelled);
    }
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
    let grid_to_world = grid_to_world_matrix(spec.grid_to_world);
    let Some(bounds) = cross_section_candidate_bounds(slab, spec, grid_shape) else {
        return Ok(VisibleResourceRegionPlan {
            regions: Vec::new(),
            primary_regions: 0,
            work: SemanticPlanWork::default(),
            scratch_charge: None,
        });
    };
    let candidate_count = bounds.count();
    if candidate_count > limits.max_candidates {
        return Err(SemanticPlanError::capacity(
            "candidate",
            limits.max_candidates,
        ));
    }
    let scratch_charge = reserve_semantic_scratch(
        scratch_ledger,
        cross_section_scratch_bytes(candidate_count.min(limits.max_resources))?,
    )?;

    let mut regions = Vec::with_capacity(candidate_count.min(limits.max_resources));
    let mut candidates_visited = 0_usize;
    for z in bounds.z_min..=bounds.z_max {
        for y in bounds.y_min..=bounds.y_max {
            for x in bounds.x_min..=bounds.x_max {
                if candidates_visited.is_multiple_of(256) && cancelled() {
                    return Err(SemanticPlanError::Cancelled);
                }
                candidates_visited = candidates_visited.saturating_add(1);
                let index = RegionIndex::new(z, y, x);
                if cross_section_intersects_region(
                    slab,
                    spec,
                    index,
                    sampling_footprint_halo_voxels,
                    grid_to_world,
                ) {
                    if regions.len() == limits.max_resources {
                        return Err(SemanticPlanError::capacity(
                            "resource",
                            limits.max_resources,
                        ));
                    }
                    let contribution =
                        cross_section_screen_contribution(slab, spec, index, grid_to_world)
                            .expect("an intersecting region has finite screen bounds");
                    regions.push((semantic_region(spec, index)?, contribution));
                }
            }
        }
    }
    if cancelled() {
        return Err(SemanticPlanError::Cancelled);
    }
    regions.sort_unstable_by(|left, right| {
        compare_screen_contribution(left.1, right.1)
            .then_with(|| left.0.origin().cmp(&right.0.origin()))
    });
    if cancelled() {
        return Err(SemanticPlanError::Cancelled);
    }
    Ok(VisibleResourceRegionPlan {
        primary_regions: regions.len(),
        regions: regions.into_iter().map(|(region, _)| region).collect(),
        work: SemanticPlanWork { candidates_visited },
        scratch_charge,
    })
}

struct PlannedRegionIndices {
    indices: Vec<RegionIndex>,
    primary_regions: usize,
    work: SemanticPlanWork,
    scratch_charge: Option<Box<dyn CpuByteLease>>,
}

fn plan_perspective_regions(
    camera: CameraFrame,
    priority_camera: Option<CameraFrame>,
    spec: SemanticRegionGridSpec,
    sampling_footprint_halo_voxels: u64,
    limits: SemanticPlanLimits,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<PlannedRegionIndices, SemanticPlanError> {
    let frustum = perspective_frustum(camera)?;
    let priority_frustum = priority_camera
        .filter(|camera| camera.view().projection() == Projection::Perspective)
        .map(perspective_frustum)
        .transpose()?;
    let grid_to_world = grid_to_world_matrix(spec.grid_to_world);
    let grid_shape = region_grid_shape(spec.volume_shape, spec.resource_shape);
    let Some(bounds) = perspective_candidate_bounds(frustum, spec, grid_shape)? else {
        return Ok(PlannedRegionIndices {
            indices: Vec::new(),
            primary_regions: 0,
            work: SemanticPlanWork::default(),
            scratch_charge: None,
        });
    };
    let candidate_count = bounds.count();
    if candidate_count > limits.max_candidates {
        return Err(SemanticPlanError::capacity(
            "candidate",
            limits.max_candidates,
        ));
    }
    let scratch_charge = reserve_semantic_scratch(
        scratch_ledger,
        volume_scratch_bytes(candidate_count.min(limits.max_resources))?,
    )?;

    let mut regions = Vec::with_capacity(candidate_count.min(limits.max_resources));
    let mut candidates_visited = 0_usize;
    for z in bounds.z_min..=bounds.z_max {
        for y in bounds.y_min..=bounds.y_max {
            for x in bounds.x_min..=bounds.x_max {
                if candidates_visited.is_multiple_of(256) && cancelled() {
                    return Err(SemanticPlanError::Cancelled);
                }
                candidates_visited = candidates_visited.saturating_add(1);
                let index = RegionIndex::new(z, y, x);
                if perspective_region_overlaps_frustum(
                    frustum,
                    spec,
                    index,
                    sampling_footprint_halo_voxels,
                    grid_to_world,
                ) {
                    if regions.len() == limits.max_resources {
                        return Err(SemanticPlanError::capacity(
                            "resource",
                            limits.max_resources,
                        ));
                    }
                    let primary = priority_frustum.is_none_or(|priority| {
                        perspective_region_overlaps_frustum(
                            priority,
                            spec,
                            index,
                            sampling_footprint_halo_voxels,
                            grid_to_world,
                        )
                    });
                    let contribution_camera =
                        priority_frustum.filter(|_| primary).unwrap_or(frustum);
                    let contribution = perspective_screen_contribution(
                        contribution_camera,
                        spec,
                        index,
                        grid_to_world,
                    )
                    .expect("a finite intersecting region has finite screen bounds");
                    regions.push((index, primary, contribution));
                }
            }
        }
    }
    if cancelled() {
        return Err(SemanticPlanError::Cancelled);
    }
    regions.sort_unstable_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| compare_screen_contribution(left.2, right.2))
            .then_with(|| (left.0.z, left.0.y, left.0.x).cmp(&(right.0.z, right.0.y, right.0.x)))
    });
    if cancelled() {
        return Err(SemanticPlanError::Cancelled);
    }
    let primary_regions = regions.partition_point(|(_, primary, _)| *primary);
    Ok(PlannedRegionIndices {
        indices: regions.into_iter().map(|(index, _, _)| index).collect(),
        primary_regions,
        work: SemanticPlanWork { candidates_visited },
        scratch_charge,
    })
}

// The hot planner receives borrowed/scalar camera, grid, bound, accounting,
// and cancellation authorities; aggregating them would add a one-use model.
#[allow(clippy::too_many_arguments)]
fn plan_orthographic_regions(
    camera: CameraFrame,
    priority_camera: Option<CameraFrame>,
    extent: RenderExtent,
    spec: SemanticRegionGridSpec,
    sampling_footprint_halo_voxels: u64,
    limits: SemanticPlanLimits,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<PlannedRegionIndices, SemanticPlanError> {
    let Some(view) = orthographic_view(camera, extent)? else {
        return Ok(PlannedRegionIndices {
            indices: Vec::new(),
            primary_regions: 0,
            work: SemanticPlanWork::default(),
            scratch_charge: None,
        });
    };
    let priority_view = priority_camera
        .filter(|camera| camera.view().projection() == Projection::Orthographic)
        .map(|camera| orthographic_view(camera, extent))
        .transpose()?
        .flatten();
    let grid_to_world = grid_to_world_matrix(spec.grid_to_world);
    let grid_shape = region_grid_shape(spec.volume_shape, spec.resource_shape);
    let Some(bounds) = orthographic_candidate_bounds(view, spec, grid_shape)? else {
        return Ok(PlannedRegionIndices {
            indices: Vec::new(),
            primary_regions: 0,
            work: SemanticPlanWork::default(),
            scratch_charge: None,
        });
    };
    let candidate_count = bounds.count();
    if candidate_count > limits.max_candidates {
        return Err(SemanticPlanError::capacity(
            "candidate",
            limits.max_candidates,
        ));
    }
    let scratch_charge = reserve_semantic_scratch(
        scratch_ledger,
        volume_scratch_bytes(candidate_count.min(limits.max_resources))?,
    )?;

    let mut regions = Vec::with_capacity(candidate_count.min(limits.max_resources));
    let mut candidates_visited = 0_usize;
    for z in bounds.z_min..=bounds.z_max {
        for y in bounds.y_min..=bounds.y_max {
            for x in bounds.x_min..=bounds.x_max {
                if candidates_visited.is_multiple_of(256) && cancelled() {
                    return Err(SemanticPlanError::Cancelled);
                }
                candidates_visited = candidates_visited.saturating_add(1);
                let index = RegionIndex::new(z, y, x);
                if orthographic_region_overlaps_view(
                    view,
                    spec,
                    index,
                    sampling_footprint_halo_voxels,
                    grid_to_world,
                ) {
                    if regions.len() == limits.max_resources {
                        return Err(SemanticPlanError::capacity(
                            "resource",
                            limits.max_resources,
                        ));
                    }
                    let primary = priority_view.is_none_or(|priority| {
                        orthographic_region_overlaps_view(
                            priority,
                            spec,
                            index,
                            sampling_footprint_halo_voxels,
                            grid_to_world,
                        )
                    });
                    let contribution_view = priority_view.filter(|_| primary).unwrap_or(view);
                    let contribution = orthographic_screen_contribution(
                        contribution_view,
                        spec,
                        index,
                        grid_to_world,
                    )
                    .expect("an overlapping region has finite screen bounds");
                    regions.push((index, primary, contribution));
                }
            }
        }
    }
    if cancelled() {
        return Err(SemanticPlanError::Cancelled);
    }
    regions.sort_unstable_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| compare_screen_contribution(left.2, right.2))
            .then_with(|| (left.0.z, left.0.y, left.0.x).cmp(&(right.0.z, right.0.y, right.0.x)))
    });
    if cancelled() {
        return Err(SemanticPlanError::Cancelled);
    }
    let primary_regions = regions.partition_point(|(_, primary, _)| *primary);
    Ok(PlannedRegionIndices {
        indices: regions.into_iter().map(|(index, _, _)| index).collect(),
        primary_regions,
        work: SemanticPlanWork { candidates_visited },
        scratch_charge,
    })
}

fn reserve_semantic_scratch(
    ledger: Option<&dyn CpuByteLedger>,
    bytes: u64,
) -> Result<Option<Box<dyn CpuByteLease>>, SemanticPlanError> {
    let Some(ledger) = ledger.filter(|_| bytes != 0) else {
        return Ok(None);
    };
    ledger
        .try_acquire(CpuLedgerCategory::MetadataAndIndexes, bytes)
        .map(Some)
        .map_err(SemanticPlanError::ScratchCapacity)
}

fn volume_scratch_bytes(capacity: usize) -> Result<u64, SemanticPlanError> {
    scratch_bytes(
        capacity,
        std::mem::size_of::<(RegionIndex, bool, ScreenContribution)>(),
        std::mem::size_of::<RegionIndex>().max(std::mem::size_of::<ResourceRegion>()),
    )
}

fn cross_section_scratch_bytes(capacity: usize) -> Result<u64, SemanticPlanError> {
    scratch_bytes(
        capacity,
        std::mem::size_of::<(ResourceRegion, ScreenContribution)>(),
        std::mem::size_of::<ResourceRegion>(),
    )
}

fn scratch_bytes(
    capacity: usize,
    primary_record_bytes: usize,
    conversion_record_bytes: usize,
) -> Result<u64, SemanticPlanError> {
    let record_bytes = primary_record_bytes
        .checked_add(conversion_record_bytes)
        .ok_or_else(|| SemanticPlanError::capacity("scratch byte", usize::MAX))?;
    let bytes = capacity
        .checked_mul(record_bytes)
        .ok_or_else(|| SemanticPlanError::capacity("scratch byte", usize::MAX))?;
    u64::try_from(bytes).map_err(|_| SemanticPlanError::capacity("scratch byte", usize::MAX))
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

fn perspective_frustum(camera: CameraFrame) -> Result<PerspectiveFrustum, SemanticPlanError> {
    let axes = camera.axes();
    let focal = camera.view().perspective_focal_length_screen_points();
    let presentation = camera.presentation();
    let tan_half_width = presentation.width_points() * 0.5 / focal;
    let tan_half_height = presentation.height_points() * 0.5 / focal;
    if !tan_half_width.is_finite()
        || !tan_half_height.is_finite()
        || tan_half_width <= 0.0
        || tan_half_height <= 0.0
    {
        return Err(SemanticPlanError::Camera(
            RenderApiError::CameraMathNotFinite,
        ));
    }
    Ok(PerspectiveFrustum {
        eye: DVec3::from_array(camera.eye().components()),
        forward: DVec3::from_array(axes.forward()),
        right: DVec3::from_array(axes.right()),
        up: DVec3::from_array(axes.up()),
        tan_half_width,
        tan_half_height,
    })
}

fn orthographic_view(
    camera: CameraFrame,
    extent: RenderExtent,
) -> Result<Option<OrthographicView>, SemanticPlanError> {
    let Some((forward, right, up)) = camera_basis(camera) else {
        return Ok(None);
    };
    Ok(Some(OrthographicView {
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
    }))
}

fn camera_visibility_planes(
    camera: CameraFrame,
    extent: RenderExtent,
) -> Result<[VisibilityHalfSpace; 5], SemanticPlanError> {
    match camera.view().projection() {
        Projection::Perspective => {
            let frustum = perspective_frustum(camera)?;
            Ok([
                VisibilityHalfSpace::through_eye(frustum.forward, frustum.eye),
                VisibilityHalfSpace::through_eye(
                    frustum.forward * frustum.tan_half_width + frustum.right,
                    frustum.eye,
                ),
                VisibilityHalfSpace::through_eye(
                    frustum.forward * frustum.tan_half_width - frustum.right,
                    frustum.eye,
                ),
                VisibilityHalfSpace::through_eye(
                    frustum.forward * frustum.tan_half_height + frustum.up,
                    frustum.eye,
                ),
                VisibilityHalfSpace::through_eye(
                    frustum.forward * frustum.tan_half_height - frustum.up,
                    frustum.eye,
                ),
            ])
        }
        Projection::Orthographic => {
            let Some(view) = orthographic_view(camera, extent)? else {
                return Err(SemanticPlanError::Camera(
                    RenderApiError::CameraMathNotFinite,
                ));
            };
            Ok([
                VisibilityHalfSpace::through_eye(view.forward, view.eye),
                VisibilityHalfSpace {
                    normal: view.right,
                    offset: view.half_width - view.right.dot(view.eye),
                },
                VisibilityHalfSpace {
                    normal: -view.right,
                    offset: view.half_width + view.right.dot(view.eye),
                },
                VisibilityHalfSpace {
                    normal: view.up,
                    offset: view.half_height - view.up.dot(view.eye),
                },
                VisibilityHalfSpace {
                    normal: -view.up,
                    offset: view.half_height + view.up.dot(view.eye),
                },
            ])
        }
    }
}

const PARALLELEPIPED_EDGES: [(usize, usize); 12] = [
    (0, 1),
    (0, 2),
    (0, 4),
    (1, 3),
    (1, 5),
    (2, 3),
    (2, 6),
    (3, 7),
    (4, 5),
    (4, 6),
    (5, 7),
    (6, 7),
];

fn clipped_volume_half_space_is_contained(
    corners: [DVec3; 8],
    next: VisibilityHalfSpace,
    guard: VisibilityHalfSpace,
) -> bool {
    let safely_inside_guard = |point: DVec3| {
        let numerical_margin =
            EPSILON * 32.0 * (1.0 + guard.normal.length() * point.length() + guard.offset.abs());
        guard.signed_distance(point) >= numerical_margin
    };
    let next_distances = corners.map(|corner| next.signed_distance(corner) + EPSILON);
    for (corner, next_distance) in corners.into_iter().zip(next_distances) {
        if next_distance >= 0.0 && !safely_inside_guard(corner) {
            return false;
        }
    }
    for (left, right) in PARALLELEPIPED_EDGES {
        let left_distance = next_distances[left];
        let right_distance = next_distances[right];
        if (left_distance < 0.0) == (right_distance < 0.0) {
            continue;
        }
        let denominator = left_distance - right_distance;
        if denominator.abs() <= f64::EPSILON {
            return false;
        }
        let interpolation = left_distance / denominator;
        let point = corners[left].lerp(corners[right], interpolation);
        if !safely_inside_guard(point) {
            return false;
        }
    }
    true
}

fn perspective_candidate_bounds(
    frustum: PerspectiveFrustum,
    spec: SemanticRegionGridSpec,
    grid_shape: Shape3D,
) -> Result<Option<CandidateBounds>, SemanticPlanError> {
    let mut min_depth = f64::INFINITY;
    let mut max_depth = f64::NEG_INFINITY;
    for corner in volume_grid_corners(spec.volume_shape) {
        let world = transform_grid_point(spec.grid_to_world, corner);
        let depth = (world - frustum.eye).dot(frustum.forward);
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
        let right = frustum.right * (depth * frustum.tan_half_width);
        let up = frustum.up * (depth * frustum.tan_half_height);
        for right_sign in [-1.0, 1.0] {
            for up_sign in [-1.0, 1.0] {
                let world =
                    frustum.eye + frustum.forward * depth + right * right_sign + up * up_sign;
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

/// Plane rejection is conservative for the affine brick parallelepiped: if
/// every corner lies outside one frustum half-space the brick cannot be
/// visible. Passing all planes may retain a few edge/corner false positives,
/// which is preferable to a missing fine brick in a supposedly complete
/// presentation cohort.
fn perspective_region_overlaps_frustum(
    frustum: PerspectiveFrustum,
    spec: SemanticRegionGridSpec,
    index: RegionIndex,
    sampling_footprint_halo_voxels: u64,
    grid_to_world: DMat4,
) -> bool {
    let Some(corners) = region_world_corners_with_halo_matrix(
        spec,
        index,
        sampling_footprint_halo_voxels,
        grid_to_world,
    ) else {
        return false;
    };
    let planes = [
        frustum.forward,
        frustum.forward * frustum.tan_half_width + frustum.right,
        frustum.forward * frustum.tan_half_width - frustum.right,
        frustum.forward * frustum.tan_half_height + frustum.up,
        frustum.forward * frustum.tan_half_height - frustum.up,
    ];
    !planes.into_iter().any(|normal| {
        corners
            .iter()
            .all(|corner| (*corner - frustum.eye).dot(normal) < -EPSILON)
    })
}

fn perspective_screen_contribution(
    frustum: PerspectiveFrustum,
    spec: SemanticRegionGridSpec,
    index: RegionIndex,
    grid_to_world: DMat4,
) -> Option<ScreenContribution> {
    let corners = region_world_corners_with_halo_matrix(spec, index, 0, grid_to_world)?;
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut near_depth = f64::INFINITY;
    for corner in corners {
        let relative = corner - frustum.eye;
        let depth = relative.dot(frustum.forward);
        near_depth = near_depth.min(depth);
        if depth <= EPSILON {
            return Some(ScreenContribution {
                overlap_area: 4.0 * frustum.tan_half_width * frustum.tan_half_height,
                center_distance_squared: 0.0,
                near_depth: 0.0,
            });
        }
        let x = relative.dot(frustum.right) / depth;
        let y = relative.dot(frustum.up) / depth;
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    screen_contribution(
        min_x,
        max_x,
        min_y,
        max_y,
        frustum.tan_half_width,
        frustum.tan_half_height,
        near_depth,
    )
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
    sampling_footprint_halo_voxels: u64,
    grid_to_world: DMat4,
) -> bool {
    let Some(corners) = region_world_corners_with_halo_matrix(
        spec,
        index,
        sampling_footprint_halo_voxels,
        grid_to_world,
    ) else {
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
    sampling_footprint_halo_voxels: u64,
    grid_to_world: DMat4,
) -> bool {
    let Some(corners) = region_world_corners_with_halo_matrix(
        spec,
        index,
        sampling_footprint_halo_voxels,
        grid_to_world,
    ) else {
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

fn orthographic_screen_contribution(
    view: OrthographicView,
    spec: SemanticRegionGridSpec,
    index: RegionIndex,
    grid_to_world: DMat4,
) -> Option<ScreenContribution> {
    let corners = region_world_corners_with_halo_matrix(spec, index, 0, grid_to_world)?;
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut near_depth = f64::INFINITY;
    for corner in corners {
        let relative = corner - view.eye;
        let x = relative.dot(view.right);
        let y = relative.dot(view.up);
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
        near_depth = near_depth.min(relative.dot(view.forward));
    }
    screen_contribution(
        min_x,
        max_x,
        min_y,
        max_y,
        view.half_width,
        view.half_height,
        near_depth,
    )
}

fn cross_section_screen_contribution(
    slab: CrossSectionSlab,
    spec: SemanticRegionGridSpec,
    index: RegionIndex,
    grid_to_world: DMat4,
) -> Option<ScreenContribution> {
    let corners = region_world_corners_with_halo_matrix(spec, index, 0, grid_to_world)?;
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut near_depth = f64::INFINITY;
    for corner in corners {
        let relative = corner - slab.center_world;
        let x = relative.dot(slab.basis.right_world);
        let y = relative.dot(slab.basis.down_world);
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
        near_depth = near_depth.min(relative.dot(slab.basis.normal_away_world).abs());
    }
    screen_contribution(
        min_x,
        max_x,
        min_y,
        max_y,
        slab.half_width_world,
        slab.half_height_world,
        near_depth,
    )
}

fn screen_contribution(
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
    half_width: f64,
    half_height: f64,
    near_depth: f64,
) -> Option<ScreenContribution> {
    if ![
        min_x,
        max_x,
        min_y,
        max_y,
        half_width,
        half_height,
        near_depth,
    ]
    .into_iter()
    .all(f64::is_finite)
    {
        return None;
    }
    let overlap_width = max_x.min(half_width) - min_x.max(-half_width);
    let overlap_height = max_y.min(half_height) - min_y.max(-half_height);
    let center_x = (min_x + max_x) * 0.5;
    let center_y = (min_y + max_y) * 0.5;
    Some(ScreenContribution {
        overlap_area: overlap_width.max(0.0) * overlap_height.max(0.0),
        center_distance_squared: center_x.mul_add(center_x, center_y * center_y),
        near_depth,
    })
}

fn compare_screen_contribution(
    left: ScreenContribution,
    right: ScreenContribution,
) -> std::cmp::Ordering {
    right
        .overlap_area
        .total_cmp(&left.overlap_area)
        .then_with(|| {
            left.center_distance_squared
                .total_cmp(&right.center_distance_squared)
        })
        .then_with(|| left.near_depth.total_cmp(&right.near_depth))
}

fn region_grid_shape(volume_shape: Shape3D, resource_shape: Shape3D) -> Shape3D {
    Shape3D::new(
        volume_shape.z().div_ceil(resource_shape.z()),
        volume_shape.y().div_ceil(resource_shape.y()),
        volume_shape.x().div_ceil(resource_shape.x()),
    )
    .expect("nonzero shapes produce a nonzero resource grid")
}

fn region_world_corners_with_halo_matrix(
    spec: SemanticRegionGridSpec,
    index: RegionIndex,
    halo_voxels: u64,
    grid_to_world: DMat4,
) -> Option<[DVec3; 8]> {
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
    let min_x = min_x.saturating_sub(halo_voxels);
    let min_y = min_y.saturating_sub(halo_voxels);
    let min_z = min_z.saturating_sub(halo_voxels);
    let max_x = max_x.saturating_add(halo_voxels).min(spec.volume_shape.x());
    let max_y = max_y.saturating_add(halo_voxels).min(spec.volume_shape.y());
    let max_z = max_z.saturating_add(halo_voxels).min(spec.volume_shape.z());
    let xs = [min_x as f64 - 0.5, max_x as f64 - 0.5];
    let ys = [min_y as f64 - 0.5, max_y as f64 - 0.5];
    let zs = [min_z as f64 - 0.5, max_z as f64 - 0.5];
    Some([
        grid_to_world.transform_point3(DVec3::new(xs[0], ys[0], zs[0])),
        grid_to_world.transform_point3(DVec3::new(xs[1], ys[0], zs[0])),
        grid_to_world.transform_point3(DVec3::new(xs[0], ys[1], zs[0])),
        grid_to_world.transform_point3(DVec3::new(xs[1], ys[1], zs[0])),
        grid_to_world.transform_point3(DVec3::new(xs[0], ys[0], zs[1])),
        grid_to_world.transform_point3(DVec3::new(xs[1], ys[0], zs[1])),
        grid_to_world.transform_point3(DVec3::new(xs[0], ys[1], zs[1])),
        grid_to_world.transform_point3(DVec3::new(xs[1], ys[1], zs[1])),
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

#[cfg(test)]
mod tests {
    use mirante4d_domain::{CameraView, UnitQuaternion, WorldPoint3};

    use super::*;

    #[test]
    fn orthographic_demand_starts_with_the_highest_contribution_center_region() {
        let presentation = PresentationViewport::new(192.0, 192.0).unwrap();
        let camera = CameraFrame::new(
            CameraView::new(
                Projection::Orthographic,
                WorldPoint3::new(96.0, 96.0, 32.0).unwrap(),
                UnitQuaternion::identity(),
                1.0,
                192.0,
                256.0,
            )
            .unwrap(),
            presentation,
        )
        .unwrap();
        let regions = plan_visible_resource_regions(
            camera,
            RenderExtent::new(192, 192).unwrap(),
            SemanticRegionGridSpec {
                volume_shape: Shape3D::new(64, 192, 192).unwrap(),
                resource_shape: Shape3D::new(64, 64, 64).unwrap(),
                grid_to_world: GridToWorld::scale(1.0, 1.0, 1.0).unwrap(),
            },
            0,
            SemanticPlanLimits::new(64, 64),
        )
        .unwrap();

        assert_eq!(regions.len(), 9);
        assert_eq!(regions[0].origin(), [0, 64, 64]);
    }

    #[test]
    fn perspective_candidate_bound_counts_unique_regions_not_duplicate_ray_visits() {
        let presentation = PresentationViewport::new(1280.0, 720.0).unwrap();
        let camera = CameraFrame::new(
            CameraView::new(
                Projection::Perspective,
                WorldPoint3::new(32.0, 32.0, 32.0).unwrap(),
                UnitQuaternion::identity(),
                1.0,
                720.0,
                128.0,
            )
            .unwrap(),
            presentation,
        )
        .unwrap();
        let regions = plan_visible_resource_regions(
            camera,
            RenderExtent::new(1280, 720).unwrap(),
            SemanticRegionGridSpec {
                volume_shape: Shape3D::new(64, 64, 64).unwrap(),
                resource_shape: Shape3D::new(64, 64, 64).unwrap(),
                grid_to_world: GridToWorld::identity(),
            },
            0,
            SemanticPlanLimits::new(1, 1),
        )
        .unwrap();

        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].origin(), [0, 0, 0]);
    }

    #[test]
    fn superseded_volume_plan_stops_at_a_bounded_candidate_checkpoint() {
        let presentation = PresentationViewport::new(256.0, 256.0).unwrap();
        let camera = CameraFrame::new(
            CameraView::new(
                Projection::Orthographic,
                WorldPoint3::new(8.0, 8.0, 8.0).unwrap(),
                UnitQuaternion::identity(),
                1.0,
                256.0,
                64.0,
            )
            .unwrap(),
            presentation,
        )
        .unwrap();
        let probes = std::cell::Cell::new(0_u32);
        let error = plan_visible_resource_regions_cancellable(
            camera,
            RenderExtent::new(256, 256).unwrap(),
            SemanticRegionGridSpec {
                volume_shape: Shape3D::new(16, 16, 16).unwrap(),
                resource_shape: Shape3D::new(1, 1, 1).unwrap(),
                grid_to_world: GridToWorld::identity(),
            },
            0,
            SemanticPlanLimits::new(4_096, 4_096),
            None,
            || {
                let next = probes.get() + 1;
                probes.set(next);
                next >= 4
            },
        )
        .unwrap_err();

        assert!(matches!(error, SemanticPlanError::Cancelled));
        // Initial, traversal-start, and 256-candidate checkpoints bound how
        // much stale work can pass before the fourth probe observes change.
        assert_eq!(probes.get(), 4);
    }

    #[test]
    fn perspective_coverage_includes_every_subpixel_brick_without_ray_sampling_holes() {
        let presentation = PresentationViewport::new(1280.0, 720.0).unwrap();
        let camera = CameraFrame::new(
            CameraView::new(
                Projection::Perspective,
                WorldPoint3::new(8.0, 8.0, 0.0).unwrap(),
                UnitQuaternion::identity(),
                1.0,
                720.0,
                1_000.0,
            )
            .unwrap(),
            presentation,
        )
        .unwrap();
        let regions = plan_visible_resource_regions(
            camera,
            RenderExtent::new(1280, 720).unwrap(),
            SemanticRegionGridSpec {
                volume_shape: Shape3D::new(1, 16, 16).unwrap(),
                resource_shape: Shape3D::new(1, 1, 1).unwrap(),
                grid_to_world: GridToWorld::identity(),
            },
            0,
            SemanticPlanLimits::new(256, 256),
        )
        .unwrap();

        // The complete volume projects to only about twelve pixels in each
        // dimension, so a former ~10-pixel ray grid could not discover all
        // 256 one-voxel resources. Frustum coverage is resolution-independent.
        assert_eq!(regions.len(), 256);
        let origins = regions
            .iter()
            .map(|region| region.origin())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(origins.contains(&[0, 0, 0]));
        assert!(origins.contains(&[0, 15, 15]));
    }

    #[test]
    fn perspective_coverage_keeps_a_rotated_affine_brick_at_the_frustum_edge() {
        let presentation = PresentationViewport::new(1280.0, 720.0).unwrap();
        let camera = CameraFrame::new(
            CameraView::new(
                Projection::Perspective,
                WorldPoint3::new(0.0, 0.0, 0.0).unwrap(),
                UnitQuaternion::identity(),
                1.0,
                720.0,
                32.0,
            )
            .unwrap(),
            presentation,
        )
        .unwrap();
        let c = std::f64::consts::FRAC_1_SQRT_2;
        let transform = GridToWorld::from_row_major([
            c,
            -c,
            0.0,
            28.5,
            c,
            c,
            0.0,
            -3.0 * c,
            0.0,
            0.0,
            1.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ])
        .unwrap();
        let regions = plan_visible_resource_regions(
            camera,
            RenderExtent::new(1280, 720).unwrap(),
            SemanticRegionGridSpec {
                volume_shape: Shape3D::new(1, 4, 4).unwrap(),
                resource_shape: Shape3D::new(1, 1, 1).unwrap(),
                grid_to_world: transform,
            },
            0,
            SemanticPlanLimits::new(16, 16),
        )
        .unwrap();

        assert!(regions.iter().any(|region| region.origin() == [0, 3, 0]));
    }

    #[test]
    fn oblique_candidate_scan_can_exceed_renderer_output_without_false_capacity() {
        let diagonal = DVec3::ONE.normalize();
        let rotation = DQuat::from_rotation_arc(diagonal, DVec3::Z);
        let matrix = DMat4::from_quat(rotation);
        let columns = matrix.to_cols_array();
        let mut row_major = [0.0; 16];
        for row in 0..4 {
            for column in 0..4 {
                row_major[row * 4 + column] = columns[column * 4 + row];
            }
        }
        let transform = GridToWorld::from_row_major(row_major).unwrap();
        let center = matrix.transform_point3(DVec3::splat(23.5));
        let presentation = PresentationViewport::new(2.0, 2.0).unwrap();
        let camera = CameraFrame::new(
            CameraView::new(
                Projection::Orthographic,
                WorldPoint3::new(center.x, center.y, center.z).unwrap(),
                UnitQuaternion::identity(),
                1.0,
                2.0,
                200.0,
            )
            .unwrap(),
            presentation,
        )
        .unwrap();
        let spec = SemanticRegionGridSpec {
            volume_shape: Shape3D::new(48, 48, 48).unwrap(),
            resource_shape: Shape3D::new(1, 1, 1).unwrap(),
            grid_to_world: transform,
        };

        let conservative_error = plan_visible_resource_regions(
            camera,
            RenderExtent::new(2, 2).unwrap(),
            spec,
            0,
            SemanticPlanLimits::new(65_536, 65_536),
        )
        .unwrap_err();
        assert!(matches!(
            conservative_error,
            SemanticPlanError::Capacity {
                kind: "candidate",
                ..
            }
        ));

        let visible = plan_visible_resource_regions(
            camera,
            RenderExtent::new(2, 2).unwrap(),
            spec,
            0,
            SemanticPlanLimits::new(131_072, 65_536),
        )
        .unwrap();
        assert!(!visible.is_empty());
        assert!(visible.len() <= 65_536);
    }

    #[test]
    fn volume_demand_includes_adjacent_brick_for_one_voxel_sampling_footprint() {
        let presentation = PresentationViewport::new(1.0, 1.0).unwrap();
        let camera = CameraFrame::new(
            CameraView::new(
                Projection::Orthographic,
                WorldPoint3::new(63.1, 0.0, 0.0).unwrap(),
                UnitQuaternion::identity(),
                0.1,
                1.0,
                10.0,
            )
            .unwrap(),
            presentation,
        )
        .unwrap();
        let regions = plan_visible_resource_regions(
            camera,
            RenderExtent::new(1, 1).unwrap(),
            SemanticRegionGridSpec {
                volume_shape: Shape3D::new(1, 1, 128).unwrap(),
                resource_shape: Shape3D::new(1, 1, 64).unwrap(),
                grid_to_world: GridToWorld::identity(),
            },
            1,
            SemanticPlanLimits::new(8, 8),
        )
        .unwrap();

        let origins = regions
            .iter()
            .map(|region| region.origin())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            origins,
            std::collections::BTreeSet::from([[0, 0, 0], [0, 0, 64]])
        );
    }

    #[test]
    fn cross_section_demand_uses_only_the_exact_composed_sampling_footprint() {
        let origins = |center_x, halo_voxels| {
            plan_cross_section_resource_regions(
                CrossSectionView::new(
                    WorldPoint3::new(center_x, 0.0, 0.0).unwrap(),
                    UnitQuaternion::identity(),
                    0.1,
                    0.1,
                )
                .unwrap(),
                CrossSectionPlane::Xy,
                PresentationViewport::new(1.0, 1.0).unwrap(),
                SemanticRegionGridSpec {
                    volume_shape: Shape3D::new(1, 1, 192).unwrap(),
                    resource_shape: Shape3D::new(1, 1, 64).unwrap(),
                    grid_to_world: GridToWorld::identity(),
                },
                halo_voxels,
                SemanticPlanLimits::new(8, 8),
            )
            .unwrap()
            .iter()
            .map(|region| region.origin())
            .collect::<std::collections::BTreeSet<_>>()
        };

        assert_eq!(
            origins(63.1, 0),
            std::collections::BTreeSet::from([[0, 0, 0]])
        );
        assert_eq!(
            origins(63.1, 1),
            std::collections::BTreeSet::from([[0, 0, 0], [0, 0, 64]])
        );
        assert_eq!(
            origins(126.1, 1),
            std::collections::BTreeSet::from([[0, 0, 64]])
        );
        assert_eq!(
            origins(126.1, 2),
            std::collections::BTreeSet::from([[0, 0, 64], [0, 0, 128]])
        );
    }
}

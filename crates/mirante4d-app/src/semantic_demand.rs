//! Storage-independent resource-region planning for product views.

use std::fmt;

use glam::{DMat3, DMat4, DQuat, DVec3};
use mirante4d_dataset::{
    CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError, ResourceContractError,
    ResourceRegion,
};
use mirante4d_domain::{CrossSectionView, GridToWorld, Projection, Shape3D, UnitQuaternion};
use mirante4d_render_api::{
    CameraFrame, PresentationViewport, RenderApiError, RenderExtent, ShaderControlAffineError,
    shader_control_world_to_grid_rows,
};

const EPSILON: f64 = 1.0e-9;
const MAX_CLIPPED_POLYGON_VERTICES: usize = 10;
const MAX_BINARY32_HALF_INTEGER_EXTENT: u64 = 1 << 23;
const MAX_SHADER_GRID_ROUNDING_RADIUS: f64 = 0.5;
/// The resident-plane contract covers a complete four-world-unit slice,
/// followed by the declared 0.30/0.15-radian compound drag.  Summing the two
/// component angles is a conservative geodesic bound for the sequential
/// rotations.
const PLANE_REUSE_TRANSLATION_WORLD: f64 = 4.0;
const PLANE_REUSE_ROTATION_RADIANS: f64 = 0.30 + 0.15;
const MAX_EFFECTIVE_AFFINE_CONDITION: f64 = 1.0e12;
const MAX_EFFECTIVE_AFFINE_RESIDUAL: f64 = 1.0e-9;

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
    ControlPrecision { field: &'static str },
    DegeneratePlane,
    NonInvertibleTransform,
    Resource(ResourceContractError),
    Camera(RenderApiError),
    ScratchCapacity(CpuLedgerError),
}

impl SemanticPlanError {
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
            Self::ControlPrecision { field } => {
                write!(
                    formatter,
                    "{field} cannot be represented by the cross-section shader with sub-voxel certainty"
                )
            }
            Self::DegeneratePlane => {
                formatter.write_str("cross-section plane steps must be finite and independent")
            }
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
            Self::Capacity { .. }
            | Self::Cancelled
            | Self::ControlPrecision { .. }
            | Self::DegeneratePlane
            | Self::NonInvertibleTransform => None,
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
    /// Constant-size proof for the dormant plane-guard suffix, when the
    /// guarded attempt fit its ordinary hard limits.
    pub(crate) plane_reuse_guard: Option<SemanticPlaneLayerReuseGuard>,
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
            .field("plane_reuse_guard", &self.plane_reuse_guard)
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

/// Per-selected-scale proof for one immutable cross-section guard body.
///
/// `base_plane` is the initial finite shader parallelogram expressed in the
/// effective world coordinates obtained by inverting the quantized
/// world-to-grid affine used by the renderer. The immutable body contains
/// every brick whose complete shader-addressable support can intersect the
/// parallelogram Minkowski-summed with `planning_radius_world`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SemanticPlaneLayerReuseGuard {
    spec: SemanticRegionGridSpec,
    effective_grid_to_world: EffectiveGridToWorld,
    base_plane: EffectiveWorldPlane,
    acceptance_radius_world: f64,
    planning_radius_world: f64,
}

// Construction rejects non-finite geometry, so equality remains reflexive.
impl Eq for SemanticPlaneLayerReuseGuard {}

/// Bounded proof attached to one panel's immutable requirement body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SemanticPlaneReuseEnvelope {
    layers: Box<[SemanticPlaneLayerReuseGuard]>,
    covers_full_selected_volumes: bool,
    reusable_candidates: usize,
}

impl SemanticPlaneReuseEnvelope {
    pub(crate) fn new(
        layers: Vec<SemanticPlaneLayerReuseGuard>,
        covers_full_selected_volumes: bool,
        reusable_candidates: usize,
    ) -> Option<Self> {
        (!layers.is_empty()).then(|| Self {
            layers: layers.into_boxed_slice(),
            covers_full_selected_volumes,
            reusable_candidates,
        })
    }

    /// Proves that every shader-addressable point of the new finite plane is
    /// inside the swept capsule whose intersecting brick body was planned. A body
    /// covering every selected visible-layer volume is the stronger proof and
    /// accepts any otherwise-valid plane geometry.
    pub(crate) fn contains(
        &self,
        view: CrossSectionView,
        panel: CrossSectionPlane,
        presentation: PresentationViewport,
        extent: RenderExtent,
    ) -> Result<bool, SemanticPlanError> {
        self.layers.iter().try_fold(true, |contained, guard| {
            if !contained {
                return Ok(false);
            }
            let footprint =
                cross_section_plane_grid_footprint(view, panel, presentation, extent, guard.spec)?;
            if self.covers_full_selected_volumes {
                // Full residency removes geometric membership work, not
                // shader-control validation.
                return Ok(true);
            }
            guard.contains_footprint(footprint)
        })
    }

    pub(crate) const fn reusable_candidates(&self) -> usize {
        self.reusable_candidates
    }

    /// True while the geometry is still safely renderable from the installed
    /// fine guard but has consumed enough of that window to justify preparing
    /// an overlapping successor. Full-volume selected-scale coverage never
    /// needs a rolling window.
    pub(crate) fn needs_rolling_replan(
        &self,
        view: CrossSectionView,
        panel: CrossSectionPlane,
        presentation: PresentationViewport,
        extent: RenderExtent,
    ) -> Result<bool, SemanticPlanError> {
        if self.covers_full_selected_volumes {
            return Ok(false);
        }
        self.layers.iter().try_fold(false, |near, guard| {
            let footprint =
                cross_section_plane_grid_footprint(view, panel, presentation, extent, guard.spec)?;
            Ok(near || guard.footprint_uses_rolling_margin(footprint)?)
        })
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
struct ScreenContribution {
    overlap_area: f64,
    center_distance_squared: f64,
    near_depth: f64,
}

/// Grid-space affine map of the finite physical render-pixel centers sampled
/// by one cross-section target.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PlaneGridFootprint {
    origin_grid_xyz: [f64; 3],
    step_x_grid_xyz: [f64; 3],
    step_y_grid_xyz: [f64; 3],
    rounding_radius_grid_xyz: [f64; 3],
    world_to_grid_rows: [[f64; 4]; 3],
    width_pixels: u32,
    height_pixels: u32,
}

impl PlaneGridFootprint {
    fn corners(self) -> Result<[[f64; 3]; 4], SemanticPlanError> {
        let x = checked_vector_scale(
            self.step_x_grid_xyz,
            f64::from(self.width_pixels.saturating_sub(1)),
        )?;
        let y = checked_vector_scale(
            self.step_y_grid_xyz,
            f64::from(self.height_pixels.saturating_sub(1)),
        )?;
        let xy = checked_vector_add(x, y)?;
        Ok([
            self.origin_grid_xyz,
            checked_vector_add(self.origin_grid_xyz, x)?,
            checked_vector_add(self.origin_grid_xyz, xy)?,
            checked_vector_add(self.origin_grid_xyz, y)?,
        ])
    }
}

impl SemanticPlaneLayerReuseGuard {
    fn for_declared_sweep(
        footprint: PlaneGridFootprint,
        spec: SemanticRegionGridSpec,
    ) -> Result<Option<Self>, SemanticPlanError> {
        let Some(effective_grid_to_world) =
            EffectiveGridToWorld::from_world_to_grid_rows(footprint.world_to_grid_rows)
        else {
            // Exact planning remains valid, but an ill-conditioned inverse
            // cannot establish a trustworthy constant-time reuse proof.
            return Ok(None);
        };
        let corners_grid = footprint.corners()?;
        let corners_world =
            corners_grid.map(|corner| effective_grid_to_world.transform_point(corner));
        if !corners_world.iter().all(|corner| corner.is_finite()) {
            return Ok(None);
        }
        let Ok(base_plane) = EffectiveWorldPlane::from_corners(corners_world) else {
            // A one-pixel or numerically degenerate finite target keeps its
            // exact plan but cannot establish an area-capsule proof.
            return Ok(None);
        };
        let center = base_plane.center();
        let half_diagonal = corners_world
            .iter()
            .map(|corner| (*corner - center).length())
            .fold(0.0_f64, f64::max);
        if !half_diagonal.is_finite() || half_diagonal <= 0.0 {
            return Ok(None);
        }

        // A point at radius R displaced by a rotation of theta moves by at
        // most 2R sin(theta/2). The sequential 0.30/0.15-radian drag has
        // geodesic length at most their sum. A control-quantization allowance
        // keeps the declared path inside the acceptance capsule; containment
        // below remains the final authority for every actual sample.
        let maximum_coordinate = corners_world
            .iter()
            .flat_map(|corner| corner.to_array())
            .map(f64::abs)
            .fold(0.0_f64, f64::max);
        let control_quantization_allowance = 64.0
            * f64::from(f32::EPSILON)
            * (maximum_coordinate + half_diagonal + PLANE_REUSE_TRANSLATION_WORLD + 1.0);
        let acceptance_radius_world = PLANE_REUSE_TRANSLATION_WORLD
            + 2.0 * half_diagonal * (PLANE_REUSE_ROTATION_RADIANS * 0.5).sin()
            + control_quantization_allowance;
        // Keep a strict numerical gap between the runtime acceptance set and
        // the capsule used to classify support spheres. Floating-point error
        // can therefore reject reuse, but cannot admit an unplanned brick.
        let planning_margin = (256.0
            * f64::EPSILON
            * (maximum_coordinate + acceptance_radius_world + half_diagonal + 1.0))
            .max(f64::EPSILON);
        let planning_radius_world = acceptance_radius_world + planning_margin;
        if !acceptance_radius_world.is_finite()
            || !planning_radius_world.is_finite()
            || acceptance_radius_world <= 0.0
            || planning_radius_world <= acceptance_radius_world
        {
            return Ok(None);
        }
        Ok(Some(Self {
            spec,
            effective_grid_to_world,
            base_plane,
            acceptance_radius_world,
            planning_radius_world,
        }))
    }

    fn contains_footprint(&self, footprint: PlaneGridFootprint) -> Result<bool, SemanticPlanError> {
        let corners = footprint.corners()?;
        let radius_squared = self.acceptance_radius_world * self.acceptance_radius_world;
        Ok(corners.into_iter().all(|corner| {
            let point = self.effective_grid_to_world.transform_point(corner);
            point.is_finite()
                && point_to_parallelogram_distance_squared(point, self.base_plane)
                    .is_some_and(|distance_squared| distance_squared <= radius_squared)
        }))
    }

    fn footprint_uses_rolling_margin(
        &self,
        footprint: PlaneGridFootprint,
    ) -> Result<bool, SemanticPlanError> {
        let rolling_radius = self.acceptance_radius_world * 0.7;
        let rolling_radius_squared = rolling_radius * rolling_radius;
        Ok(footprint.corners()?.into_iter().any(|corner| {
            let point = self.effective_grid_to_world.transform_point(corner);
            point.is_finite()
                && point_to_parallelogram_distance_squared(point, self.base_plane)
                    .is_some_and(|distance_squared| distance_squared >= rolling_radius_squared)
        }))
    }

    fn resource_support_sphere_intersects(
        &self,
        index: RegionIndex,
        sampling_footprint_halo_voxels: u64,
    ) -> Result<bool, SemanticPlanError> {
        let strict_half_voxel =
            f64::from_bits(MAX_SHADER_GRID_ROUNDING_RADIUS.to_bits().saturating_sub(1));
        let volume_xyz = [
            self.spec.volume_shape.x(),
            self.spec.volume_shape.y(),
            self.spec.volume_shape.z(),
        ];
        let resource_xyz = [
            self.spec.resource_shape.x(),
            self.spec.resource_shape.y(),
            self.spec.resource_shape.z(),
        ];
        let coordinates_xyz = [index.x, index.y, index.z];
        let support = std::array::from_fn::<_, 3, _>(|axis| {
            projected_region_support_interval(
                coordinates_xyz[axis],
                volume_xyz[axis],
                resource_xyz[axis],
                sampling_footprint_halo_voxels,
            )
        });
        let support = [
            support[0].expanded(strict_half_voxel)?,
            support[1].expanded(strict_half_voxel)?,
            support[2].expanded(strict_half_voxel)?,
        ];
        let center_grid =
            std::array::from_fn(|axis| (support[axis].low + support[axis].high) * 0.5);
        let center_world = self.effective_grid_to_world.transform_point(center_grid);
        if !center_world.is_finite() {
            return Err(SemanticPlanError::ControlPrecision {
                field: "cross-section effective support transform",
            });
        }
        let mut sphere_radius = 0.0_f64;
        let mut maximum_coordinate = center_world.abs().max_element();
        for mask in 0_u8..8 {
            let corner_grid = std::array::from_fn(|axis| {
                if mask & (1 << axis) == 0 {
                    support[axis].low
                } else {
                    support[axis].high
                }
            });
            let corner_world = self.effective_grid_to_world.transform_point(corner_grid);
            if !corner_world.is_finite() {
                return Err(SemanticPlanError::ControlPrecision {
                    field: "cross-section effective support transform",
                });
            }
            maximum_coordinate = maximum_coordinate.max(corner_world.abs().max_element());
            sphere_radius = sphere_radius.max((corner_world - center_world).length());
        }
        let sphere_margin = 128.0
            * f64::EPSILON
            * (maximum_coordinate + sphere_radius + self.planning_radius_world + 1.0);
        sphere_radius += sphere_margin;
        let maximum_distance = self.planning_radius_world + sphere_radius;
        Ok(
            point_to_parallelogram_distance_squared(center_world, self.base_plane).is_some_and(
                |distance_squared| distance_squared <= maximum_distance * maximum_distance,
            ),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct AxisInterval {
    low: f64,
    high: f64,
}

impl AxisInterval {
    fn overlaps(self, low: f64, high: f64) -> bool {
        self.high >= low && self.low <= high
    }

    fn expanded(self, radius: f64) -> Result<Self, SemanticPlanError> {
        let expanded = Self {
            low: self.low - radius,
            high: self.high + radius,
        };
        (radius.is_finite()
            && radius >= 0.0
            && expanded.low.is_finite()
            && expanded.high.is_finite())
        .then_some(expanded)
        .ok_or(SemanticPlanError::ControlPrecision {
            field: "cross-section shader grid rounding",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Point2 {
    x: f64,
    y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct EffectiveGridToWorld {
    inverse_linear: DMat3,
    world_to_grid_translation: DVec3,
    grid_round_trip_residual: [[f64; 3]; 3],
}

impl EffectiveGridToWorld {
    fn from_world_to_grid_rows(rows: [[f64; 4]; 3]) -> Option<Self> {
        if !rows.iter().flatten().all(|value| value.is_finite()) {
            return None;
        }
        let linear = DMat3::from_cols(
            DVec3::new(rows[0][0], rows[1][0], rows[2][0]),
            DVec3::new(rows[0][1], rows[1][1], rows[2][1]),
            DVec3::new(rows[0][2], rows[1][2], rows[2][2]),
        );
        let determinant = linear.determinant();
        if !determinant.is_finite() || determinant == 0.0 {
            return None;
        }
        let inverse_linear = linear.inverse();
        if !inverse_linear.is_finite() {
            return None;
        }
        let grid_round_trip = linear * inverse_linear;
        let left_residual = matrix_identity_residual(grid_round_trip);
        let right_residual = matrix_identity_residual(inverse_linear * linear);
        if !left_residual.is_finite()
            || !right_residual.is_finite()
            || left_residual > MAX_EFFECTIVE_AFFINE_RESIDUAL
            || right_residual > MAX_EFFECTIVE_AFFINE_RESIDUAL
        {
            return None;
        }
        let linear_rows = [
            DVec3::new(rows[0][0], rows[0][1], rows[0][2]),
            DVec3::new(rows[1][0], rows[1][1], rows[1][2]),
            DVec3::new(rows[2][0], rows[2][1], rows[2][2]),
        ];
        let inverse_columns = inverse_linear.to_cols_array_2d();
        let inverse_rows = [
            DVec3::new(
                inverse_columns[0][0],
                inverse_columns[1][0],
                inverse_columns[2][0],
            ),
            DVec3::new(
                inverse_columns[0][1],
                inverse_columns[1][1],
                inverse_columns[2][1],
            ),
            DVec3::new(
                inverse_columns[0][2],
                inverse_columns[1][2],
                inverse_columns[2][2],
            ),
        ];
        let infinity_norm = linear_rows
            .iter()
            .map(|row| row.abs().element_sum())
            .fold(0.0, f64::max);
        let inverse_infinity_norm = inverse_rows
            .iter()
            .map(|row| row.abs().element_sum())
            .fold(0.0, f64::max);
        let condition = infinity_norm * inverse_infinity_norm;
        if !condition.is_finite() || condition > MAX_EFFECTIVE_AFFINE_CONDITION {
            return None;
        }
        Some(Self {
            inverse_linear,
            world_to_grid_translation: DVec3::new(rows[0][3], rows[1][3], rows[2][3]),
            grid_round_trip_residual: matrix_identity_residual_rows(grid_round_trip),
        })
    }

    fn transform_point(self, point_grid: [f64; 3]) -> DVec3 {
        self.inverse_linear * (DVec3::from_array(point_grid) - self.world_to_grid_translation)
    }

    fn grid_round_trip_error_bound(
        self,
        lower_grid_xyz: [f64; 3],
        upper_grid_xyz: [f64; 3],
    ) -> [f64; 3] {
        let translation = self.world_to_grid_translation.to_array();
        let maximum_relative = std::array::from_fn::<_, 3, _>(|axis| {
            (lower_grid_xyz[axis] - translation[axis])
                .abs()
                .max((upper_grid_xyz[axis] - translation[axis]).abs())
        });
        self.grid_round_trip_residual.map(|row| {
            row.into_iter()
                .zip(maximum_relative)
                .map(|(residual, magnitude)| residual * magnitude)
                .sum()
        })
    }
}

fn matrix_identity_residual(matrix: DMat3) -> f64 {
    matrix_identity_residual_rows(matrix)
        .into_iter()
        .flatten()
        .fold(0.0, f64::max)
}

fn matrix_identity_residual_rows(matrix: DMat3) -> [[f64; 3]; 3] {
    let columns = matrix.to_cols_array_2d();
    std::array::from_fn(|row| {
        std::array::from_fn(|column| {
            (columns[column][row] - if column == row { 1.0 } else { 0.0 }).abs()
        })
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct EffectiveWorldPlane {
    origin: DVec3,
    edge_x: DVec3,
    edge_y: DVec3,
}

impl EffectiveWorldPlane {
    fn from_corners(corners: [DVec3; 4]) -> Result<Self, SemanticPlanError> {
        let plane = Self {
            origin: corners[0],
            edge_x: corners[1] - corners[0],
            edge_y: corners[3] - corners[0],
        };
        let normal = plane.edge_x.cross(plane.edge_y);
        if !plane.origin.is_finite()
            || !plane.edge_x.is_finite()
            || !plane.edge_y.is_finite()
            || !normal.is_finite()
            || normal.length_squared() <= EPSILON * EPSILON
        {
            return Err(SemanticPlanError::DegeneratePlane);
        }
        Ok(plane)
    }

    fn center(self) -> DVec3 {
        self.origin + (self.edge_x + self.edge_y) * 0.5
    }

    fn corners(self) -> [DVec3; 4] {
        [
            self.origin,
            self.origin + self.edge_x,
            self.origin + self.edge_x + self.edge_y,
            self.origin + self.edge_y,
        ]
    }
}

/// Stack-only convex polygon for one projected-cell intersection.
///
/// A convex render quadrilateral clipped by the six projected brick
/// half-spaces has at most ten vertices. Keeping this fixed-size avoids any
/// allocation proportional to candidate cells; only the final exact region
/// population is retained and charged.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ClippedPolygon {
    vertices: [Point2; MAX_CLIPPED_POLYGON_VERTICES],
    len: usize,
}

impl ClippedPolygon {
    const fn empty() -> Self {
        Self {
            vertices: [Point2 { x: 0.0, y: 0.0 }; MAX_CLIPPED_POLYGON_VERTICES],
            len: 0,
        }
    }

    fn from_quad(corners: [Point2; 4]) -> Self {
        let mut polygon = Self::empty();
        polygon.vertices[..corners.len()].copy_from_slice(&corners);
        polygon.len = corners.len();
        polygon
    }

    const fn is_empty(self) -> bool {
        self.len == 0
    }

    fn as_slice(&self) -> &[Point2] {
        &self.vertices[..self.len]
    }

    fn last(self) -> Option<Point2> {
        self.as_slice().last().copied()
    }

    fn push(&mut self, point: Point2) -> Result<(), SemanticPlanError> {
        if self.len == self.vertices.len() {
            return Err(SemanticPlanError::capacity(
                "plane clipping vertex",
                self.vertices.len(),
            ));
        }
        self.vertices[self.len] = point;
        self.len += 1;
        Ok(())
    }

    fn deduplicate_adjacent(&mut self) {
        if self.len < 2 {
            return;
        }
        let mut write = 1;
        for read in 1..self.len {
            if !points_approximately_equal(self.vertices[write - 1], self.vertices[read]) {
                self.vertices[write] = self.vertices[read];
                write += 1;
            }
        }
        self.len = write;
        if self.len > 1 && points_approximately_equal(self.vertices[0], self.vertices[self.len - 1])
        {
            self.len -= 1;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ProjectedPlaneContribution {
    overlap_area: f64,
    center_distance_squared: f64,
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
        plane_reuse_guard: None,
        scratch_charge: planned.scratch_charge,
    })
}

#[cfg(test)]
pub(crate) fn plan_cross_section_resource_regions(
    view: CrossSectionView,
    panel: CrossSectionPlane,
    presentation: PresentationViewport,
    extent: RenderExtent,
    spec: SemanticRegionGridSpec,
    sampling_footprint_halo_voxels: u64,
    limits: SemanticPlanLimits,
) -> Result<Vec<ResourceRegion>, SemanticPlanError> {
    Ok(plan_cross_section_resource_regions_cancellable(
        view,
        panel,
        presentation,
        extent,
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
    let footprint = cross_section_plane_grid_footprint(view, panel, presentation, extent, spec)?;
    let Some(projection) =
        ProjectedPlaneTraversal::new(footprint, spec, sampling_footprint_halo_voxels)?
    else {
        return Ok(VisibleResourceRegionPlan {
            regions: Vec::new(),
            primary_regions: 0,
            work: SemanticPlanWork::default(),
            plane_reuse_guard: None,
            scratch_charge: None,
        });
    };
    let candidate_count = projection.projected_cell_count()?;
    if candidate_count > limits.max_candidates {
        return Err(SemanticPlanError::capacity(
            "candidate",
            limits.max_candidates,
        ));
    }

    // Count before allocating so the sole CPU ledger can charge the exact
    // retained result. This repeats only the two-dimensional projected-cell
    // traversal; it never reconstructs or stores a three-dimensional AABB.
    let mut region_count = 0_usize;
    projection.visit_hits(&mut cancelled, |_index, _contribution| {
        if region_count == limits.max_resources {
            return Err(SemanticPlanError::capacity(
                "resource",
                limits.max_resources,
            ));
        }
        region_count = region_count
            .checked_add(1)
            .ok_or_else(|| SemanticPlanError::capacity("resource", limits.max_resources))?;
        Ok(())
    })?;
    let scratch_charge =
        reserve_semantic_scratch(scratch_ledger, cross_section_scratch_bytes(region_count)?)?;

    let mut regions = Vec::new();
    regions
        .try_reserve_exact(region_count)
        .map_err(|_| SemanticPlanError::capacity("resource allocation", region_count))?;
    projection.visit_hits(&mut cancelled, |index, contribution| {
        regions.push((semantic_region(spec, index)?, contribution));
        Ok(())
    })?;
    debug_assert_eq!(regions.len(), region_count);
    if cancelled() {
        return Err(SemanticPlanError::Cancelled);
    }
    regions.sort_unstable_by(|left, right| {
        compare_projected_plane_contribution(left.1, right.1)
            .then_with(|| left.0.origin().cmp(&right.0.origin()))
    });
    if cancelled() {
        return Err(SemanticPlanError::Cancelled);
    }
    Ok(VisibleResourceRegionPlan {
        primary_regions: regions.len(),
        regions: regions.into_iter().map(|(region, _)| region).collect(),
        work: SemanticPlanWork {
            candidates_visited: candidate_count
                .checked_mul(2)
                .ok_or_else(|| SemanticPlanError::capacity("candidate visit", usize::MAX))?,
        },
        plane_reuse_guard: None,
        scratch_charge,
    })
}

/// Plans the exact plane as the visible prefix and a conservative capsule for
/// the declared slice-then-rotation workload as a dormant navigation suffix.
/// Any capacity failure is returned to the caller, which retries the exact
/// planner without changing fidelity.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_guarded_cross_section_resource_regions_cancellable(
    view: CrossSectionView,
    panel: CrossSectionPlane,
    presentation: PresentationViewport,
    extent: RenderExtent,
    spec: SemanticRegionGridSpec,
    sampling_footprint_halo_voxels: u64,
    limits: SemanticPlanLimits,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    mut cancelled: impl FnMut() -> bool,
) -> Result<VisibleResourceRegionPlan, SemanticPlanError> {
    let exact = plan_cross_section_resource_regions_cancellable(
        view,
        panel,
        presentation,
        extent,
        spec,
        sampling_footprint_halo_voxels,
        limits,
        scratch_ledger,
        &mut cancelled,
    )?;
    if exact.regions.is_empty() || cancelled() {
        return if cancelled() {
            Err(SemanticPlanError::Cancelled)
        } else {
            Ok(exact)
        };
    }

    let exact_footprint =
        cross_section_plane_grid_footprint(view, panel, presentation, extent, spec)?;
    let Some(guard) = SemanticPlaneLayerReuseGuard::for_declared_sweep(exact_footprint, spec)?
    else {
        return Ok(exact);
    };
    append_swept_capsule_guard(
        exact,
        guard,
        exact_footprint,
        spec,
        sampling_footprint_halo_voxels,
        limits,
        scratch_ledger,
        &mut cancelled,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the geometry hot path keeps its independent proof and capacity facts borrowed"
)]
fn append_swept_capsule_guard(
    mut exact: VisibleResourceRegionPlan,
    guard: SemanticPlaneLayerReuseGuard,
    footprint: PlaneGridFootprint,
    spec: SemanticRegionGridSpec,
    sampling_footprint_halo_voxels: u64,
    limits: SemanticPlanLimits,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<VisibleResourceRegionPlan, SemanticPlanError> {
    let Some(ranges) = capsule_candidate_ranges(footprint, &guard, sampling_footprint_halo_voxels)?
    else {
        return Ok(exact);
    };
    let capsule_candidates = ranges
        .into_iter()
        .try_fold(1_usize, |count, range| {
            count.checked_mul(projected_range_len(range))
        })
        .ok_or_else(|| SemanticPlanError::capacity("candidate", limits.max_candidates))?;
    let exact_candidates = exact.work.candidates_visited / 2;
    if exact_candidates.saturating_add(capsule_candidates) > limits.max_candidates {
        return Err(SemanticPlanError::capacity(
            "candidate",
            limits.max_candidates,
        ));
    }
    if cancelled() {
        return Err(SemanticPlanError::Cancelled);
    }

    let membership_charge = reserve_semantic_scratch(
        scratch_ledger,
        scratch_bytes(exact.regions.len(), std::mem::size_of::<[u64; 3]>(), 0)?,
    )?;
    let mut exact_origins = Vec::new();
    exact_origins
        .try_reserve_exact(exact.regions.len())
        .map_err(|_| {
            SemanticPlanError::capacity("guard membership allocation", exact.regions.len())
        })?;
    exact_origins.extend(exact.regions.iter().map(|region| region.origin()));
    exact_origins.sort_unstable();

    let mut suffix_count = 0_usize;
    visit_capsule_candidates(
        ranges,
        &guard,
        sampling_footprint_halo_voxels,
        cancelled,
        |index| {
            let origin = semantic_region(spec, index)?.origin();
            if exact_origins.binary_search(&origin).is_err() {
                let total = exact
                    .regions
                    .len()
                    .checked_add(suffix_count)
                    .and_then(|count| count.checked_add(1))
                    .ok_or_else(|| SemanticPlanError::capacity("resource", limits.max_resources))?;
                if total > limits.max_resources {
                    return Err(SemanticPlanError::capacity(
                        "resource",
                        limits.max_resources,
                    ));
                }
                suffix_count += 1;
            }
            Ok(())
        },
    )?;
    let guarded_count = exact
        .regions
        .len()
        .checked_add(suffix_count)
        .ok_or_else(|| SemanticPlanError::capacity("resource", limits.max_resources))?;
    let guarded_charge =
        reserve_semantic_scratch(scratch_ledger, cross_section_scratch_bytes(guarded_count)?)?;
    let mut guarded_regions = Vec::new();
    guarded_regions
        .try_reserve_exact(guarded_count)
        .map_err(|_| SemanticPlanError::capacity("resource allocation", guarded_count))?;
    guarded_regions.extend(std::mem::take(&mut exact.regions));
    visit_capsule_candidates(
        ranges,
        &guard,
        sampling_footprint_halo_voxels,
        cancelled,
        |index| {
            let region = semantic_region(spec, index)?;
            if exact_origins.binary_search(&region.origin()).is_err() {
                guarded_regions.push(region);
            }
            Ok(())
        },
    )?;
    debug_assert_eq!(guarded_regions.len(), guarded_count);
    exact.regions = guarded_regions;
    exact.work.candidates_visited = exact
        .work
        .candidates_visited
        .saturating_add(capsule_candidates.saturating_mul(2));
    exact.plane_reuse_guard = Some(guard);
    drop(exact.scratch_charge.take());
    drop(exact_origins);
    drop(membership_charge);
    exact.scratch_charge = guarded_charge;
    Ok(exact)
}

fn capsule_candidate_ranges(
    footprint: PlaneGridFootprint,
    guard: &SemanticPlaneLayerReuseGuard,
    sampling_footprint_halo_voxels: u64,
) -> Result<Option<[(u64, u64); 3]>, SemanticPlanError> {
    let corners = footprint.corners()?;
    let volume_xyz = [
        guard.spec.volume_shape.x(),
        guard.spec.volume_shape.y(),
        guard.spec.volume_shape.z(),
    ];
    let resource_xyz = [
        guard.spec.resource_shape.x(),
        guard.spec.resource_shape.y(),
        guard.spec.resource_shape.z(),
    ];
    let grid_xyz: [u64; 3] =
        std::array::from_fn(|axis| volume_xyz[axis].div_ceil(resource_xyz[axis]));
    let base_lower = std::array::from_fn(|axis| {
        corners
            .iter()
            .map(|corner| corner[axis])
            .fold(f64::INFINITY, f64::min)
    });
    let base_upper = std::array::from_fn(|axis| {
        corners
            .iter()
            .map(|corner| corner[axis])
            .fold(f64::NEG_INFINITY, f64::max)
    });
    let support_lower = [-(sampling_footprint_halo_voxels as f64) - 1.0; 3];
    let support_upper =
        volume_xyz.map(|dimension| dimension as f64 + sampling_footprint_halo_voxels as f64);
    let base_round_trip_error = guard
        .effective_grid_to_world
        .grid_round_trip_error_bound(base_lower, base_upper);
    let support_round_trip_error = guard
        .effective_grid_to_world
        .grid_round_trip_error_bound(support_lower, support_upper);
    let strict_half_voxel =
        f64::from_bits(MAX_SHADER_GRID_ROUNDING_RADIUS.to_bits().saturating_sub(1));
    let mut ranges = [(0_u64, 0_u64); 3];
    for axis in 0..3 {
        let row = footprint.world_to_grid_rows[axis];
        let row_norm = row[..3]
            .iter()
            .map(|value| value * value)
            .sum::<f64>()
            .sqrt();
        // The effective inverse is residual-bounded rather than assumed
        // algebraically exact. Both the initial-plane point and the accepted
        // in-volume support point can carry that error, so expand by their
        // separate coordinate/translation-aware bounds before shader support.
        let displacement = row_norm * guard.planning_radius_world
            + base_round_trip_error[axis]
            + support_round_trip_error[axis];
        let low = base_lower[axis] - displacement - strict_half_voxel;
        let high = base_upper[axis] + displacement + strict_half_voxel;
        let Some(range) = projected_region_range(
            low,
            high,
            volume_xyz[axis],
            resource_xyz[axis],
            grid_xyz[axis],
            sampling_footprint_halo_voxels,
        )?
        else {
            return Ok(None);
        };
        ranges[axis] = range;
    }
    Ok(Some(ranges))
}

fn visit_capsule_candidates(
    ranges_xyz: [(u64, u64); 3],
    guard: &SemanticPlaneLayerReuseGuard,
    sampling_footprint_halo_voxels: u64,
    cancelled: &mut impl FnMut() -> bool,
    mut visit: impl FnMut(RegionIndex) -> Result<(), SemanticPlanError>,
) -> Result<(), SemanticPlanError> {
    let mut candidates_visited = 0_usize;
    for z in ranges_xyz[2].0..=ranges_xyz[2].1 {
        for y in ranges_xyz[1].0..=ranges_xyz[1].1 {
            for x in ranges_xyz[0].0..=ranges_xyz[0].1 {
                if candidates_visited.is_multiple_of(256) && cancelled() {
                    return Err(SemanticPlanError::Cancelled);
                }
                candidates_visited = candidates_visited.saturating_add(1);
                let index = RegionIndex::new(z, y, x);
                if guard
                    .resource_support_sphere_intersects(index, sampling_footprint_halo_voxels)?
                {
                    visit(index)?;
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct ProjectedPlaneTraversal {
    spec: SemanticRegionGridSpec,
    sampling_footprint_halo_voxels: u64,
    plane_origin: [f64; 3],
    normal: [f64; 3],
    dominant_axis: usize,
    projected_axes: [usize; 2],
    projected_corners: [Point2; 4],
    projected_center: Point2,
    rounding_radius_grid_xyz: [f64; 3],
    dominant_padding: f64,
    first_range: (u64, u64),
    second_range: (u64, u64),
}

impl ProjectedPlaneTraversal {
    fn new(
        footprint: PlaneGridFootprint,
        spec: SemanticRegionGridSpec,
        sampling_footprint_halo_voxels: u64,
    ) -> Result<Option<Self>, SemanticPlanError> {
        Self::new_with_dominant_padding(footprint, spec, sampling_footprint_halo_voxels, 0.0)
    }

    fn new_with_dominant_padding(
        footprint: PlaneGridFootprint,
        spec: SemanticRegionGridSpec,
        sampling_footprint_halo_voxels: u64,
        dominant_padding: f64,
    ) -> Result<Option<Self>, SemanticPlanError> {
        let corners = footprint.corners()?;
        let normal = cross3(footprint.step_x_grid_xyz, footprint.step_y_grid_xyz);
        if !normal.iter().all(|value| value.is_finite())
            || normal.iter().map(|value| value.abs()).fold(0.0, f64::max) == 0.0
        {
            return Err(SemanticPlanError::DegeneratePlane);
        }
        let dominant_axis = dominant_axis(normal);
        let projected_axes = projected_axes(dominant_axis);
        let projected_corners = corners.map(|corner| Point2 {
            x: corner[projected_axes[0]],
            y: corner[projected_axes[1]],
        });
        let projected_center = Point2 {
            x: projected_corners.iter().map(|point| point.x).sum::<f64>() * 0.25,
            y: projected_corners.iter().map(|point| point.y).sum::<f64>() * 0.25,
        };
        if footprint.rounding_radius_grid_xyz.iter().any(|radius| {
            !radius.is_finite() || *radius < 0.0 || *radius >= MAX_SHADER_GRID_ROUNDING_RADIUS
        }) || !dominant_padding.is_finite()
            || dominant_padding < 0.0
        {
            return Err(SemanticPlanError::ControlPrecision {
                field: "cross-section shader grid rounding",
            });
        }
        let volume_xyz = [
            spec.volume_shape.x(),
            spec.volume_shape.y(),
            spec.volume_shape.z(),
        ];
        let resource_xyz = [
            spec.resource_shape.x(),
            spec.resource_shape.y(),
            spec.resource_shape.z(),
        ];
        let grid_xyz = [
            volume_xyz[0].div_ceil(resource_xyz[0]),
            volume_xyz[1].div_ceil(resource_xyz[1]),
            volume_xyz[2].div_ceil(resource_xyz[2]),
        ];
        let first_low = projected_corners
            .iter()
            .map(|point| point.x)
            .fold(f64::INFINITY, f64::min);
        let first_high = projected_corners
            .iter()
            .map(|point| point.x)
            .fold(f64::NEG_INFINITY, f64::max);
        let second_low = projected_corners
            .iter()
            .map(|point| point.y)
            .fold(f64::INFINITY, f64::min);
        let second_high = projected_corners
            .iter()
            .map(|point| point.y)
            .fold(f64::NEG_INFINITY, f64::max);
        let Some(first_range) = projected_region_range(
            first_low - footprint.rounding_radius_grid_xyz[projected_axes[0]],
            first_high + footprint.rounding_radius_grid_xyz[projected_axes[0]],
            volume_xyz[projected_axes[0]],
            resource_xyz[projected_axes[0]],
            grid_xyz[projected_axes[0]],
            sampling_footprint_halo_voxels,
        )?
        else {
            return Ok(None);
        };
        let Some(second_range) = projected_region_range(
            second_low - footprint.rounding_radius_grid_xyz[projected_axes[1]],
            second_high + footprint.rounding_radius_grid_xyz[projected_axes[1]],
            volume_xyz[projected_axes[1]],
            resource_xyz[projected_axes[1]],
            grid_xyz[projected_axes[1]],
            sampling_footprint_halo_voxels,
        )?
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            spec,
            sampling_footprint_halo_voxels,
            plane_origin: footprint.origin_grid_xyz,
            normal,
            dominant_axis,
            projected_axes,
            projected_corners,
            projected_center,
            rounding_radius_grid_xyz: footprint.rounding_radius_grid_xyz,
            dominant_padding,
            first_range,
            second_range,
        }))
    }

    fn projected_cell_count(self) -> Result<usize, SemanticPlanError> {
        projected_range_len(self.first_range)
            .checked_mul(projected_range_len(self.second_range))
            .ok_or_else(|| SemanticPlanError::capacity("candidate", usize::MAX))
    }

    fn visit_hits(
        self,
        cancelled: &mut impl FnMut() -> bool,
        mut visit: impl FnMut(RegionIndex, ProjectedPlaneContribution) -> Result<(), SemanticPlanError>,
    ) -> Result<(), SemanticPlanError> {
        let volume_xyz = [
            self.spec.volume_shape.x(),
            self.spec.volume_shape.y(),
            self.spec.volume_shape.z(),
        ];
        let resource_xyz = [
            self.spec.resource_shape.x(),
            self.spec.resource_shape.y(),
            self.spec.resource_shape.z(),
        ];
        let grid_xyz = [
            volume_xyz[0].div_ceil(resource_xyz[0]),
            volume_xyz[1].div_ceil(resource_xyz[1]),
            volume_xyz[2].div_ceil(resource_xyz[2]),
        ];
        let mut projected_cells_visited = 0_usize;
        for first in self.first_range.0..=self.first_range.1 {
            for second in self.second_range.0..=self.second_range.1 {
                if projected_cells_visited.is_multiple_of(256) && cancelled() {
                    return Err(SemanticPlanError::Cancelled);
                }
                projected_cells_visited = projected_cells_visited.saturating_add(1);
                let first_interval = projected_region_support_interval(
                    first,
                    volume_xyz[self.projected_axes[0]],
                    resource_xyz[self.projected_axes[0]],
                    self.sampling_footprint_halo_voxels,
                )
                .expanded(self.rounding_radius_grid_xyz[self.projected_axes[0]])?;
                let second_interval = projected_region_support_interval(
                    second,
                    volume_xyz[self.projected_axes[1]],
                    resource_xyz[self.projected_axes[1]],
                    self.sampling_footprint_halo_voxels,
                )
                .expanded(self.rounding_radius_grid_xyz[self.projected_axes[1]])?;
                let clipped = clip_quad_to_rectangle(
                    self.projected_corners,
                    first_interval,
                    second_interval,
                )?;
                if clipped.is_empty() {
                    continue;
                }

                let mut dominant_low = f64::INFINITY;
                let mut dominant_high = f64::NEG_INFINITY;
                for point in clipped.as_slice() {
                    let coordinate = solve_dominant_coordinate(
                        *point,
                        self.projected_axes,
                        self.dominant_axis,
                        self.plane_origin,
                        self.normal,
                    )?;
                    dominant_low = dominant_low.min(coordinate);
                    dominant_high = dominant_high.max(coordinate);
                }
                let Some(dominant_range) = projected_region_range(
                    dominant_low
                        - self.rounding_radius_grid_xyz[self.dominant_axis]
                        - self.dominant_padding,
                    dominant_high
                        + self.rounding_radius_grid_xyz[self.dominant_axis]
                        + self.dominant_padding,
                    volume_xyz[self.dominant_axis],
                    resource_xyz[self.dominant_axis],
                    grid_xyz[self.dominant_axis],
                    self.sampling_footprint_halo_voxels,
                )?
                else {
                    continue;
                };

                for third in dominant_range.0..=dominant_range.1 {
                    let interval = projected_region_support_interval(
                        third,
                        volume_xyz[self.dominant_axis],
                        resource_xyz[self.dominant_axis],
                        self.sampling_footprint_halo_voxels,
                    )
                    .expanded(self.rounding_radius_grid_xyz[self.dominant_axis])?;
                    let guarded_low = dominant_low - self.dominant_padding;
                    let guarded_high = dominant_high + self.dominant_padding;
                    if !interval.overlaps(guarded_low, guarded_high) {
                        continue;
                    }
                    let brick_clipped = if self.dominant_padding == 0.0 {
                        clip_polygon_to_dominant_interval(
                            clipped,
                            self.projected_axes,
                            self.dominant_axis,
                            self.plane_origin,
                            self.normal,
                            interval,
                        )?
                    } else {
                        // For the bounded prism, retaining the complete
                        // projected polygon is deliberately conservative:
                        // every accepted third-axis interval intersects the
                        // prism somewhere, and false-positive guard bricks
                        // are harmless dormant prefetch.
                        clipped
                    };
                    if brick_clipped.is_empty() {
                        continue;
                    }
                    let clipped_center = polygon_center(brick_clipped.as_slice());
                    let center_x = clipped_center.x - self.projected_center.x;
                    let center_y = clipped_center.y - self.projected_center.y;
                    let contribution = ProjectedPlaneContribution {
                        overlap_area: polygon_measure(brick_clipped.as_slice()),
                        center_distance_squared: center_x.mul_add(center_x, center_y * center_y),
                    };
                    let mut coordinate_xyz = [0_u64; 3];
                    coordinate_xyz[self.projected_axes[0]] = first;
                    coordinate_xyz[self.projected_axes[1]] = second;
                    coordinate_xyz[self.dominant_axis] = third;
                    visit(
                        RegionIndex::new(coordinate_xyz[2], coordinate_xyz[1], coordinate_xyz[0]),
                        contribution,
                    )?;
                }
            }
        }
        Ok(())
    }
}

fn cross_section_plane_grid_footprint(
    view: CrossSectionView,
    panel: CrossSectionPlane,
    presentation: PresentationViewport,
    extent: RenderExtent,
    spec: SemanticRegionGridSpec,
) -> Result<PlaneGridFootprint, SemanticPlanError> {
    validate_shader_addressing_extent(extent, spec.volume_shape)?;
    let controls = quantized_shader_plane_controls(view, panel, presentation, spec.grid_to_world)?;
    let width = f64::from(extent.width_pixels());
    let height = f64::from(extent.height_pixels());
    let first_screen_x = (0.5 / width - 0.5) * controls.presentation_points[0];
    let first_screen_y = (0.5 - 0.5 / height) * controls.presentation_points[1];
    let screen_step_x = controls.presentation_points[0] / width;
    let screen_step_y = -controls.presentation_points[1] / height;

    let origin_world = std::array::from_fn(|axis| {
        controls.center_world[axis]
            + (controls.right_world[axis] * first_screen_x
                + controls.up_world[axis] * first_screen_y)
                * controls.scale_world_per_point
    });
    let step_x_world = std::array::from_fn(|axis| {
        controls.right_world[axis] * screen_step_x * controls.scale_world_per_point
    });
    // Render targets grow downward: the next physical row moves opposite the
    // view's positive-up axis.
    let step_y_world = std::array::from_fn(|axis| {
        controls.up_world[axis] * screen_step_y * controls.scale_world_per_point
    });
    let origin_grid_xyz = transform_affine_point(controls.world_to_grid_rows, origin_world)?;
    let step_x_grid_xyz = transform_affine_vector(controls.world_to_grid_rows, step_x_world)?;
    let step_y_grid_xyz = transform_affine_vector(controls.world_to_grid_rows, step_y_world)?;
    let rounding_radius_grid_xyz = shader_grid_rounding_radius(controls, extent, origin_grid_xyz)?;

    let footprint = PlaneGridFootprint {
        origin_grid_xyz,
        step_x_grid_xyz,
        step_y_grid_xyz,
        rounding_radius_grid_xyz,
        world_to_grid_rows: controls.world_to_grid_rows,
        width_pixels: extent.width_pixels(),
        height_pixels: extent.height_pixels(),
    };
    let normal = cross3(footprint.step_x_grid_xyz, footprint.step_y_grid_xyz);
    if normal.iter().map(|value| value.abs()).fold(0.0, f64::max) == 0.0 {
        return Err(SemanticPlanError::DegeneratePlane);
    }
    Ok(footprint)
}

#[derive(Debug, Clone, Copy)]
struct QuantizedShaderPlaneControls {
    center_world: [f64; 3],
    right_world: [f64; 3],
    up_world: [f64; 3],
    scale_world_per_point: f64,
    presentation_points: [f64; 2],
    world_to_grid_rows: [[f64; 4]; 3],
}

#[derive(Debug, Clone, Copy)]
struct ShaderMagnitudeBound {
    nominal_magnitude: f64,
    error: f64,
}

fn validate_shader_addressing_extent(
    extent: RenderExtent,
    volume: Shape3D,
) -> Result<(), SemanticPlanError> {
    if u64::from(extent.width_pixels()) > MAX_BINARY32_HALF_INTEGER_EXTENT
        || u64::from(extent.height_pixels()) > MAX_BINARY32_HALF_INTEGER_EXTENT
    {
        return Err(SemanticPlanError::ControlPrecision {
            field: "cross-section physical render extent",
        });
    }
    if [volume.x(), volume.y(), volume.z()]
        .into_iter()
        .any(|axis| axis > MAX_BINARY32_HALF_INTEGER_EXTENT)
    {
        return Err(SemanticPlanError::ControlPrecision {
            field: "cross-section voxel addressing extent",
        });
    }
    Ok(())
}

fn quantized_shader_plane_controls(
    view: CrossSectionView,
    panel: CrossSectionPlane,
    presentation: PresentationViewport,
    grid_to_world: GridToWorld,
) -> Result<QuantizedShaderPlaneControls, SemanticPlanError> {
    // Product intent construction composes and canonicalizes this quaternion
    // before WGPU derives its axes. Mirror that exact control-side operation
    // instead of planning from a higher-precision basis the shader never sees.
    let composed = DQuat::from_array(view.orientation().xyzw()) * panel.relative_orientation();
    let [x, y, z, w] = composed.to_array();
    let orientation =
        UnitQuaternion::new_xyzw(x, y, z, w).map_err(|_| SemanticPlanError::ControlPrecision {
            field: "cross-section orientation control",
        })?;
    let [right_world, up_world] = renderer_cross_section_axes(orientation.xyzw());
    let scale_world_per_point = quantized_f32_control(
        view.scale_world_per_screen_point(),
        "cross-section scale control",
    )?;
    let presentation_points = [
        quantized_f32_control(
            presentation.width_points(),
            "cross-section presentation width control",
        )?,
        quantized_f32_control(
            presentation.height_points(),
            "cross-section presentation height control",
        )?,
    ];
    if scale_world_per_point == 0.0
        || presentation_points[0] == 0.0
        || presentation_points[1] == 0.0
    {
        return Err(SemanticPlanError::ControlPrecision {
            field: "cross-section plane step control",
        });
    }
    let world_to_grid_rows = shader_control_world_to_grid_rows(grid_to_world)
        .map_err(|error| match error {
            ShaderControlAffineError::NonInvertible => SemanticPlanError::NonInvertibleTransform,
            ShaderControlAffineError::NotRepresentable => SemanticPlanError::ControlPrecision {
                field: "cross-section world-to-grid control",
            },
        })?
        .map(|row| row.map(f64::from));
    Ok(QuantizedShaderPlaneControls {
        center_world: quantized_f32_vec3(
            view.center_world().components(),
            "cross-section center control",
        )?,
        right_world: quantized_f32_vec3(right_world, "cross-section right-axis control")?,
        up_world: quantized_f32_vec3(up_world, "cross-section up-axis control")?,
        scale_world_per_point,
        presentation_points,
        world_to_grid_rows,
    })
}

fn renderer_cross_section_axes(quaternion: [f64; 4]) -> [[f64; 3]; 2] {
    let [x, y, z, w] = quaternion;
    let rotate = |vector: [f64; 3]| {
        let cross = [
            y * vector[2] - z * vector[1],
            z * vector[0] - x * vector[2],
            x * vector[1] - y * vector[0],
        ];
        let twice = cross.map(|value| 2.0 * value);
        let second = [
            y * twice[2] - z * twice[1],
            z * twice[0] - x * twice[2],
            x * twice[1] - y * twice[0],
        ];
        std::array::from_fn(|axis| vector[axis] + w * twice[axis] + second[axis])
    };
    [rotate([1.0, 0.0, 0.0]), rotate([0.0, 1.0, 0.0])]
}

fn quantized_f32_vec3(
    values: [f64; 3],
    field: &'static str,
) -> Result<[f64; 3], SemanticPlanError> {
    Ok([
        quantized_f32_control(values[0], field)?,
        quantized_f32_control(values[1], field)?,
        quantized_f32_control(values[2], field)?,
    ])
}

fn quantized_f32_control(value: f64, field: &'static str) -> Result<f64, SemanticPlanError> {
    let converted = value as f32;
    converted
        .is_finite()
        .then_some(f64::from(if converted == 0.0 { 0.0 } else { converted }))
        .ok_or(SemanticPlanError::ControlPrecision { field })
}

fn transform_affine_point(
    rows: [[f64; 4]; 3],
    point: [f64; 3],
) -> Result<[f64; 3], SemanticPlanError> {
    let transformed =
        rows.map(|row| row[0] * point[0] + row[1] * point[1] + row[2] * point[2] + row[3]);
    transformed
        .iter()
        .all(|value| value.is_finite())
        .then_some(transformed)
        .ok_or(SemanticPlanError::ControlPrecision {
            field: "cross-section nominal grid projection",
        })
}

fn transform_affine_vector(
    rows: [[f64; 4]; 3],
    vector: [f64; 3],
) -> Result<[f64; 3], SemanticPlanError> {
    let transformed = rows.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2]);
    transformed
        .iter()
        .all(|value| value.is_finite())
        .then_some(transformed)
        .ok_or(SemanticPlanError::ControlPrecision {
            field: "cross-section nominal grid step",
        })
}

fn shader_grid_rounding_radius(
    controls: QuantizedShaderPlaneControls,
    extent: RenderExtent,
    nominal_origin_grid: [f64; 3],
) -> Result<[f64; 3], SemanticPlanError> {
    let screen_x =
        shader_screen_magnitude_bound(extent.width_pixels(), controls.presentation_points[0])?;
    let screen_y =
        shader_screen_magnitude_bound(extent.height_pixels(), controls.presentation_points[1])?;
    let world = [
        shader_world_component_bound(
            controls.center_world[0],
            controls.right_world[0],
            controls.up_world[0],
            controls.scale_world_per_point,
            screen_x,
            screen_y,
        )?,
        shader_world_component_bound(
            controls.center_world[1],
            controls.right_world[1],
            controls.up_world[1],
            controls.scale_world_per_point,
            screen_x,
            screen_y,
        )?,
        shader_world_component_bound(
            controls.center_world[2],
            controls.right_world[2],
            controls.up_world[2],
            controls.scale_world_per_point,
            screen_x,
            screen_y,
        )?,
    ];
    let grid = [
        shader_grid_component_bound(controls.world_to_grid_rows[0], world)?,
        shader_grid_component_bound(controls.world_to_grid_rows[1], world)?,
        shader_grid_component_bound(controls.world_to_grid_rows[2], world)?,
    ];
    let mut radius = [0.0; 3];
    for axis in 0..3 {
        let address_magnitude = grid[axis].nominal_magnitude + grid[axis].error + 0.5;
        let address_rounding = checked_binary32_rounding_bound(
            1,
            address_magnitude,
            "cross-section voxel addressing arithmetic",
        )?;
        let f64_construction_rounding = 64.0
            * f64::EPSILON
            * (grid[axis]
                .nominal_magnitude
                .max(nominal_origin_grid[axis].abs())
                + 1.0);
        radius[axis] = grid[axis].error + address_rounding + f64_construction_rounding;
        if !radius[axis].is_finite()
            || radius[axis] < 0.0
            || radius[axis] >= MAX_SHADER_GRID_ROUNDING_RADIUS
        {
            return Err(SemanticPlanError::ControlPrecision {
                field: "cross-section shader grid rounding",
            });
        }
    }
    Ok(radius)
}

fn shader_screen_magnitude_bound(
    dimension: u32,
    presentation_points: f64,
) -> Result<ShaderMagnitudeBound, SemanticPlanError> {
    let dimension = f64::from(dimension);
    let maximum_quotient = (dimension - 0.5) / dimension;
    let quotient_error = checked_binary32_rounding_bound(
        1,
        maximum_quotient,
        "cross-section screen-coordinate division",
    )?;
    let centered_magnitude = 0.5 - 0.5 / dimension;
    let centered_error = quotient_error
        + checked_binary32_rounding_bound(
            1,
            centered_magnitude + quotient_error,
            "cross-section screen-coordinate subtraction",
        )?;
    let nominal_magnitude = centered_magnitude * presentation_points.abs();
    let propagated_error = presentation_points.abs() * centered_error;
    let error = propagated_error
        + checked_binary32_rounding_bound(
            1,
            nominal_magnitude + propagated_error,
            "cross-section screen-coordinate scaling",
        )?;
    checked_shader_bound(ShaderMagnitudeBound {
        nominal_magnitude,
        error,
    })
}

fn shader_world_component_bound(
    center: f64,
    right: f64,
    up: f64,
    scale: f64,
    screen_x: ShaderMagnitudeBound,
    screen_y: ShaderMagnitudeBound,
) -> Result<ShaderMagnitudeBound, SemanticPlanError> {
    let right_term = rounded_scaled_bound(screen_x, right)?;
    let up_term = rounded_scaled_bound(screen_y, up)?;
    let offset = rounded_sum_bound(right_term, up_term)?;
    let scaled = rounded_scaled_bound(offset, scale)?;
    rounded_sum_bound(
        ShaderMagnitudeBound {
            nominal_magnitude: center.abs(),
            error: 0.0,
        },
        scaled,
    )
}

fn shader_grid_component_bound(
    row: [f64; 4],
    world: [ShaderMagnitudeBound; 3],
) -> Result<ShaderMagnitudeBound, SemanticPlanError> {
    let first = rounded_scaled_bound(world[0], row[0])?;
    let second = rounded_scaled_bound(world[1], row[1])?;
    let third = rounded_scaled_bound(world[2], row[2])?;
    let dot = rounded_sum_bound(rounded_sum_bound(first, second)?, third)?;
    rounded_sum_bound(
        dot,
        ShaderMagnitudeBound {
            nominal_magnitude: row[3].abs(),
            error: 0.0,
        },
    )
}

fn rounded_scaled_bound(
    input: ShaderMagnitudeBound,
    coefficient: f64,
) -> Result<ShaderMagnitudeBound, SemanticPlanError> {
    let nominal_magnitude = coefficient.abs() * input.nominal_magnitude;
    let propagated_error = coefficient.abs() * input.error;
    let error = propagated_error
        + checked_binary32_rounding_bound(
            1,
            nominal_magnitude + propagated_error,
            "cross-section shader multiplication",
        )?;
    checked_shader_bound(ShaderMagnitudeBound {
        nominal_magnitude,
        error,
    })
}

fn rounded_sum_bound(
    left: ShaderMagnitudeBound,
    right: ShaderMagnitudeBound,
) -> Result<ShaderMagnitudeBound, SemanticPlanError> {
    let nominal_magnitude = left.nominal_magnitude + right.nominal_magnitude;
    let propagated_error = left.error + right.error;
    let error = propagated_error
        + checked_binary32_rounding_bound(
            1,
            nominal_magnitude + propagated_error,
            "cross-section shader addition",
        )?;
    checked_shader_bound(ShaderMagnitudeBound {
        nominal_magnitude,
        error,
    })
}

fn checked_shader_bound(
    bound: ShaderMagnitudeBound,
) -> Result<ShaderMagnitudeBound, SemanticPlanError> {
    if bound.nominal_magnitude.is_finite()
        && bound.error.is_finite()
        && bound.nominal_magnitude >= 0.0
        && bound.error >= 0.0
        && bound.nominal_magnitude + bound.error <= f64::from(f32::MAX)
    {
        Ok(bound)
    } else {
        Err(SemanticPlanError::ControlPrecision {
            field: "cross-section shader arithmetic",
        })
    }
}

fn checked_binary32_rounding_bound(
    operation_count: u32,
    magnitude: f64,
    field: &'static str,
) -> Result<f64, SemanticPlanError> {
    if !magnitude.is_finite() || magnitude < 0.0 || magnitude > f64::from(f32::MAX) {
        return Err(SemanticPlanError::ControlPrecision { field });
    }
    let bound = binary32_rounding_bound(operation_count, magnitude);
    bound
        .is_finite()
        .then_some(bound)
        .ok_or(SemanticPlanError::ControlPrecision { field })
}

fn binary32_rounding_bound(operation_count: u32, magnitude: f64) -> f64 {
    let operation_count = f64::from(operation_count);
    let unit_roundoff = f64::from(f32::EPSILON) / 2.0;
    let gamma = operation_count * unit_roundoff / (1.0 - operation_count * unit_roundoff);
    gamma * magnitude + operation_count * f64::from(f32::from_bits(1))
}

fn projected_region_range(
    low: f64,
    high: f64,
    volume_axis: u64,
    resource_axis: u64,
    grid_axis: u64,
    halo_voxels: u64,
) -> Result<Option<(u64, u64)>, SemanticPlanError> {
    if !low.is_finite()
        || !high.is_finite()
        || low > high
        || volume_axis == 0
        || resource_axis == 0
        || grid_axis == 0
    {
        return Err(SemanticPlanError::DegeneratePlane);
    }
    let volume_low = -0.5;
    let volume_high = volume_axis as f64 - 0.5;
    if high < volume_low || low > volume_high {
        return Ok(None);
    }
    let edge = resource_axis as f64;
    let halo = halo_voxels as f64;
    let first_coordinate = (low + 0.5 - halo) / edge;
    let last_coordinate = (high + 0.5 + halo) / edge;
    if !first_coordinate.is_finite() || !last_coordinate.is_finite() {
        return Err(SemanticPlanError::DegeneratePlane);
    }
    // The exact interval test below removes these one-cell numerical guards.
    // They prevent a rounded projected endpoint from hiding a boundary touch
    // without ever introducing a third candidate dimension.
    let first = (first_coordinate.ceil() as i128)
        .saturating_sub(2)
        .clamp(0, i128::from(grid_axis - 1)) as u64;
    let last = (last_coordinate.floor() as i128)
        .saturating_add(1)
        .clamp(i128::from(first), i128::from(grid_axis - 1)) as u64;
    Ok(Some((first, last)))
}

fn projected_region_support_interval(
    index: u64,
    dimension: u64,
    resource_axis: u64,
    halo_voxels: u64,
) -> AxisInterval {
    let origin = index.saturating_mul(resource_axis);
    let end = origin.saturating_add(resource_axis).min(dimension);
    AxisInterval {
        low: origin.saturating_sub(halo_voxels) as f64 - 0.5,
        high: end.saturating_add(halo_voxels).min(dimension) as f64 - 0.5,
    }
}

fn projected_range_len(range: (u64, u64)) -> usize {
    range
        .1
        .saturating_sub(range.0)
        .saturating_add(1)
        .try_into()
        .unwrap_or(usize::MAX)
}

fn dominant_axis(normal: [f64; 3]) -> usize {
    let absolute = normal.map(f64::abs);
    if absolute[0] >= absolute[1] && absolute[0] >= absolute[2] {
        0
    } else if absolute[1] >= absolute[2] {
        1
    } else {
        2
    }
}

fn projected_axes(dominant: usize) -> [usize; 2] {
    match dominant {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
    }
}

fn solve_dominant_coordinate(
    point: Point2,
    projected: [usize; 2],
    dominant: usize,
    plane_origin: [f64; 3],
    normal: [f64; 3],
) -> Result<f64, SemanticPlanError> {
    let denominator = normal[dominant];
    if denominator == 0.0 {
        return Err(SemanticPlanError::DegeneratePlane);
    }
    let value = plane_origin[dominant]
        - (normal[projected[0]] * (point.x - plane_origin[projected[0]])
            + normal[projected[1]] * (point.y - plane_origin[projected[1]]))
            / denominator;
    value
        .is_finite()
        .then_some(value)
        .ok_or(SemanticPlanError::DegeneratePlane)
}

fn clip_polygon_to_dominant_interval(
    polygon: ClippedPolygon,
    projected: [usize; 2],
    dominant: usize,
    plane_origin: [f64; 3],
    normal: [f64; 3],
    interval: AxisInterval,
) -> Result<ClippedPolygon, SemanticPlanError> {
    let polygon = clip_dominant_half_space(
        polygon,
        projected,
        dominant,
        plane_origin,
        normal,
        interval.low,
        true,
    )?;
    clip_dominant_half_space(
        polygon,
        projected,
        dominant,
        plane_origin,
        normal,
        interval.high,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn clip_dominant_half_space(
    polygon: ClippedPolygon,
    projected: [usize; 2],
    dominant: usize,
    plane_origin: [f64; 3],
    normal: [f64; 3],
    boundary: f64,
    keep_above: bool,
) -> Result<ClippedPolygon, SemanticPlanError> {
    if polygon.is_empty() {
        return Ok(polygon);
    }
    let value = |point| solve_dominant_coordinate(point, projected, dominant, plane_origin, normal);
    let inside = |sample: f64| {
        if keep_above {
            sample >= boundary
        } else {
            sample <= boundary
        }
    };
    let mut clipped = ClippedPolygon::empty();
    let mut previous = polygon
        .last()
        .expect("a nonempty clipped polygon has a last point");
    let mut previous_value = value(previous)?;
    let mut previous_inside = inside(previous_value);
    for &current in polygon.as_slice() {
        let current_value = value(current)?;
        let current_inside = inside(current_value);
        if current_inside != previous_inside {
            let denominator = current_value - previous_value;
            let t = if denominator == 0.0 {
                0.0
            } else {
                ((boundary - previous_value) / denominator).clamp(0.0, 1.0)
            };
            clipped.push(Point2 {
                x: previous.x + (current.x - previous.x) * t,
                y: previous.y + (current.y - previous.y) * t,
            })?;
        }
        if current_inside {
            clipped.push(current)?;
        }
        previous = current;
        previous_value = current_value;
        previous_inside = current_inside;
    }
    clipped.deduplicate_adjacent();
    Ok(clipped)
}

fn clip_quad_to_rectangle(
    corners: [Point2; 4],
    x: AxisInterval,
    y: AxisInterval,
) -> Result<ClippedPolygon, SemanticPlanError> {
    let mut polygon = ClippedPolygon::from_quad(corners);
    polygon = clip_half_space(polygon, 0, x.low, true)?;
    polygon = clip_half_space(polygon, 0, x.high, false)?;
    polygon = clip_half_space(polygon, 1, y.low, true)?;
    polygon = clip_half_space(polygon, 1, y.high, false)?;
    polygon.deduplicate_adjacent();
    Ok(polygon)
}

fn clip_half_space(
    polygon: ClippedPolygon,
    axis: usize,
    boundary: f64,
    keep_above: bool,
) -> Result<ClippedPolygon, SemanticPlanError> {
    if polygon.is_empty() {
        return Ok(polygon);
    }
    let mut clipped = ClippedPolygon::empty();
    let mut previous = polygon
        .last()
        .expect("a nonempty clipped polygon has a last point");
    let mut previous_inside = point_inside_half_space(previous, axis, boundary, keep_above);
    for &current in polygon.as_slice() {
        let current_inside = point_inside_half_space(current, axis, boundary, keep_above);
        if current_inside != previous_inside {
            clipped.push(boundary_intersection(previous, current, axis, boundary))?;
        }
        if current_inside {
            clipped.push(current)?;
        }
        previous = current;
        previous_inside = current_inside;
    }
    Ok(clipped)
}

fn point_inside_half_space(point: Point2, axis: usize, boundary: f64, keep_above: bool) -> bool {
    let value = if axis == 0 { point.x } else { point.y };
    if keep_above {
        value >= boundary
    } else {
        value <= boundary
    }
}

fn boundary_intersection(first: Point2, second: Point2, axis: usize, boundary: f64) -> Point2 {
    let first_axis = if axis == 0 { first.x } else { first.y };
    let second_axis = if axis == 0 { second.x } else { second.y };
    let denominator = second_axis - first_axis;
    let t = if denominator == 0.0 {
        0.0
    } else {
        ((boundary - first_axis) / denominator).clamp(0.0, 1.0)
    };
    let mut point = Point2 {
        x: first.x + (second.x - first.x) * t,
        y: first.y + (second.y - first.y) * t,
    };
    if axis == 0 {
        point.x = boundary;
    } else {
        point.y = boundary;
    }
    point
}

fn points_approximately_equal(left: Point2, right: Point2) -> bool {
    (left.x - right.x).abs() <= 64.0 * f64::EPSILON
        && (left.y - right.y).abs() <= 64.0 * f64::EPSILON
}

fn polygon_measure(polygon: &[Point2]) -> f64 {
    if polygon.len() < 3 {
        return 0.0;
    }
    polygon
        .iter()
        .zip(polygon.iter().cycle().skip(1))
        .take(polygon.len())
        .map(|(first, second)| first.x * second.y - second.x * first.y)
        .sum::<f64>()
        .abs()
        * 0.5
}

fn polygon_center(polygon: &[Point2]) -> Point2 {
    let count = polygon.len() as f64;
    Point2 {
        x: polygon.iter().map(|point| point.x).sum::<f64>() / count,
        y: polygon.iter().map(|point| point.y).sum::<f64>() / count,
    }
}

fn compare_projected_plane_contribution(
    left: ProjectedPlaneContribution,
    right: ProjectedPlaneContribution,
) -> std::cmp::Ordering {
    right
        .overlap_area
        .total_cmp(&left.overlap_area)
        .then_with(|| {
            left.center_distance_squared
                .total_cmp(&right.center_distance_squared)
        })
}

fn cross3(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

/// Exact Euclidean distance to a finite, possibly sheared parallelogram.
///
/// The closest point is either the orthogonal projection into the interior or
/// a point on one of the four boundary segments.
fn point_to_parallelogram_distance_squared(
    point: DVec3,
    plane: EffectiveWorldPlane,
) -> Option<f64> {
    if !point.is_finite() {
        return None;
    }
    let relative = point - plane.origin;
    let xx = plane.edge_x.length_squared();
    let xy = plane.edge_x.dot(plane.edge_y);
    let yy = plane.edge_y.length_squared();
    let determinant = xx.mul_add(yy, -(xy * xy));
    if !xx.is_finite()
        || !xy.is_finite()
        || !yy.is_finite()
        || !determinant.is_finite()
        || determinant <= 0.0
    {
        return None;
    }
    let rx = relative.dot(plane.edge_x);
    let ry = relative.dot(plane.edge_y);
    let u = rx.mul_add(yy, -(ry * xy)) / determinant;
    let v = ry.mul_add(xx, -(rx * xy)) / determinant;
    if u.is_finite() && v.is_finite() && (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v) {
        let residual = relative - plane.edge_x * u - plane.edge_y * v;
        let distance = residual.length_squared();
        return distance.is_finite().then_some(distance.max(0.0));
    }
    let corners = plane.corners();
    [
        (corners[0], corners[1]),
        (corners[1], corners[2]),
        (corners[2], corners[3]),
        (corners[3], corners[0]),
    ]
    .into_iter()
    .filter_map(|(start, end)| point_to_segment_distance_squared(point, start, end))
    .reduce(f64::min)
}

fn point_to_segment_distance_squared(point: DVec3, start: DVec3, end: DVec3) -> Option<f64> {
    let edge = end - start;
    let length_squared = edge.length_squared();
    if !length_squared.is_finite() || length_squared <= 0.0 {
        return None;
    }
    let fraction = ((point - start).dot(edge) / length_squared).clamp(0.0, 1.0);
    let distance = (point - (start + edge * fraction)).length_squared();
    distance.is_finite().then_some(distance.max(0.0))
}

fn checked_vector_scale(vector: [f64; 3], factor: f64) -> Result<[f64; 3], SemanticPlanError> {
    let value = vector.map(|component| component * factor);
    value
        .iter()
        .all(|component| component.is_finite())
        .then_some(value)
        .ok_or(SemanticPlanError::DegeneratePlane)
}

fn checked_vector_add(left: [f64; 3], right: [f64; 3]) -> Result<[f64; 3], SemanticPlanError> {
    let value = std::array::from_fn(|axis| left[axis] + right[axis]);
    value
        .iter()
        .all(|component| component.is_finite())
        .then_some(value)
        .ok_or(SemanticPlanError::DegeneratePlane)
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
        std::mem::size_of::<(ResourceRegion, ProjectedPlaneContribution)>(),
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
    use std::collections::BTreeSet;

    use mirante4d_application::viewport_interaction::{
        CrossSectionPanel as InteractionPanel, CrossSectionViewState,
    };
    use mirante4d_domain::{CameraView, UnitQuaternion, WorldPoint3};

    use super::*;

    #[derive(Debug, Clone, Copy)]
    struct OracleParameterPoint {
        x: f64,
        y: f64,
    }

    /// Independent small-grid oracle: enumerate every logical region, then
    /// clip the render-pixel parameter rectangle against all six expanded
    /// region half-spaces. It deliberately does not use dominant-axis
    /// projection, projected candidate ranges, or production clipping.
    fn brute_force_cross_section_regions(
        view: CrossSectionView,
        panel: CrossSectionPlane,
        presentation: PresentationViewport,
        extent: RenderExtent,
        spec: SemanticRegionGridSpec,
        halo_voxels: u64,
    ) -> BTreeSet<[u64; 3]> {
        let relative = match panel {
            CrossSectionPlane::Xy => DQuat::IDENTITY,
            CrossSectionPlane::Xz => DQuat::from_rotation_x(std::f64::consts::FRAC_PI_2),
            CrossSectionPlane::Yz => DQuat::from_rotation_y(std::f64::consts::FRAC_PI_2),
        };
        let orientation = DQuat::from_array(view.orientation().xyzw()) * relative;
        let right = orientation * DVec3::X;
        let up = orientation * DVec3::Y;
        let row_major = spec.grid_to_world.row_major();
        let mut column_major = [0.0; 16];
        for row in 0..4 {
            for column in 0..4 {
                column_major[column * 4 + row] = row_major[row * 4 + column];
            }
        }
        let world_to_grid = DMat4::from_cols_array(&column_major).inverse();
        assert!(world_to_grid.is_finite());
        let width = f64::from(extent.width_pixels());
        let height = f64::from(extent.height_pixels());
        let grid_at_pixel = |pixel_x: f64, pixel_y: f64| {
            let screen_x = ((pixel_x + 0.5) / width - 0.5) * presentation.width_points();
            let screen_y = (0.5 - (pixel_y + 0.5) / height) * presentation.height_points();
            let world = DVec3::from_array(view.center_world().components())
                + right * screen_x * view.scale_world_per_screen_point()
                + up * screen_y * view.scale_world_per_screen_point();
            world_to_grid.transform_point3(world)
        };
        let plane_origin = grid_at_pixel(0.0, 0.0);
        let step_x = grid_at_pixel(1.0, 0.0) - plane_origin;
        let step_y = grid_at_pixel(0.0, 1.0) - plane_origin;
        let volume_xyz = [
            spec.volume_shape.x(),
            spec.volume_shape.y(),
            spec.volume_shape.z(),
        ];
        let resource_xyz = [
            spec.resource_shape.x(),
            spec.resource_shape.y(),
            spec.resource_shape.z(),
        ];
        let grid_xyz = [
            volume_xyz[0].div_ceil(resource_xyz[0]),
            volume_xyz[1].div_ceil(resource_xyz[1]),
            volume_xyz[2].div_ceil(resource_xyz[2]),
        ];
        let mut expected = BTreeSet::new();
        for z in 0..grid_xyz[2] {
            for y in 0..grid_xyz[1] {
                for x in 0..grid_xyz[0] {
                    let index_xyz = [x, y, z];
                    let mut support = [[0.0; 2]; 3];
                    for (axis, axis_support) in support.iter_mut().enumerate() {
                        let origin = index_xyz[axis].saturating_mul(resource_xyz[axis]);
                        let end = origin
                            .saturating_add(resource_xyz[axis])
                            .min(volume_xyz[axis]);
                        *axis_support = [
                            origin.saturating_sub(halo_voxels) as f64 - 0.5,
                            end.saturating_add(halo_voxels).min(volume_xyz[axis]) as f64 - 0.5,
                        ];
                    }
                    let mut polygon = vec![
                        OracleParameterPoint { x: 0.0, y: 0.0 },
                        OracleParameterPoint {
                            x: f64::from(extent.width_pixels() - 1),
                            y: 0.0,
                        },
                        OracleParameterPoint {
                            x: f64::from(extent.width_pixels() - 1),
                            y: f64::from(extent.height_pixels() - 1),
                        },
                        OracleParameterPoint {
                            x: 0.0,
                            y: f64::from(extent.height_pixels() - 1),
                        },
                    ];
                    for (axis, axis_support) in support.iter().enumerate() {
                        polygon = oracle_clip_parameter_half_space(
                            polygon,
                            plane_origin,
                            step_x,
                            step_y,
                            axis,
                            axis_support[0],
                            true,
                        );
                        polygon = oracle_clip_parameter_half_space(
                            polygon,
                            plane_origin,
                            step_x,
                            step_y,
                            axis,
                            axis_support[1],
                            false,
                        );
                    }
                    if !polygon.is_empty() {
                        expected.insert([
                            z.saturating_mul(resource_xyz[2]),
                            y.saturating_mul(resource_xyz[1]),
                            x.saturating_mul(resource_xyz[0]),
                        ]);
                    }
                }
            }
        }
        expected
    }

    #[allow(clippy::too_many_arguments)]
    fn oracle_clip_parameter_half_space(
        polygon: Vec<OracleParameterPoint>,
        plane_origin: DVec3,
        step_x: DVec3,
        step_y: DVec3,
        axis: usize,
        boundary: f64,
        keep_above: bool,
    ) -> Vec<OracleParameterPoint> {
        if polygon.is_empty() {
            return polygon;
        }
        let value = |point: OracleParameterPoint| {
            plane_origin[axis] + step_x[axis] * point.x + step_y[axis] * point.y
        };
        let inside = |sample: f64| {
            if keep_above {
                sample >= boundary
            } else {
                sample <= boundary
            }
        };
        let mut clipped = Vec::new();
        let mut previous = *polygon.last().expect("a nonempty polygon has a last point");
        let mut previous_value = value(previous);
        let mut previous_inside = inside(previous_value);
        for current in polygon {
            let current_value = value(current);
            let current_inside = inside(current_value);
            if current_inside != previous_inside {
                let denominator = current_value - previous_value;
                let t = if denominator == 0.0 {
                    0.0
                } else {
                    ((boundary - previous_value) / denominator).clamp(0.0, 1.0)
                };
                clipped.push(OracleParameterPoint {
                    x: previous.x + (current.x - previous.x) * t,
                    y: previous.y + (current.y - previous.y) * t,
                });
            }
            if current_inside {
                clipped.push(current);
            }
            previous = current;
            previous_value = current_value;
            previous_inside = current_inside;
        }
        clipped
    }

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
    fn cross_section_uses_physical_pixel_centers_and_never_the_depth_aabb() {
        let view = CrossSectionView::new(
            WorldPoint3::new(127.5, 127.5, 127.0).unwrap(),
            UnitQuaternion::identity(),
            1.0,
            512.0,
        )
        .unwrap();
        let presentation = PresentationViewport::new(128.0, 128.0).unwrap();
        let extent = RenderExtent::new(2, 2).unwrap();
        let spec = SemanticRegionGridSpec {
            volume_shape: Shape3D::new(256, 256, 256).unwrap(),
            resource_shape: Shape3D::new(64, 64, 64).unwrap(),
            grid_to_world: GridToWorld::identity(),
        };
        let plan = plan_cross_section_resource_regions_cancellable(
            view,
            CrossSectionPlane::Xy,
            presentation,
            extent,
            spec,
            0,
            SemanticPlanLimits::new(16, 16),
            None,
            || false,
        )
        .unwrap();
        let actual = plan
            .regions
            .iter()
            .map(|region| region.origin())
            .collect::<BTreeSet<_>>();
        let expected = brute_force_cross_section_regions(
            view,
            CrossSectionPlane::Xy,
            presentation,
            extent,
            spec,
            0,
        );

        assert_eq!(actual, expected);
        assert_eq!(
            actual,
            BTreeSet::from([[64, 64, 64], [64, 64, 128], [64, 128, 64], [64, 128, 128],])
        );
    }

    #[test]
    fn swept_capsule_proves_the_resident_slice_then_compound_rotation_sequence() {
        let initial = CrossSectionView::new(
            WorldPoint3::new(32.0, 32.0, 32.0).unwrap(),
            UnitQuaternion::identity(),
            1.0,
            1.0,
        )
        .unwrap();
        let presentation = PresentationViewport::new(64.0, 64.0).unwrap();
        let extent = RenderExtent::new(64, 64).unwrap();
        let spec = SemanticRegionGridSpec {
            volume_shape: Shape3D::new(65, 65, 65).unwrap(),
            resource_shape: Shape3D::new(64, 64, 64).unwrap(),
            grid_to_world: GridToWorld::identity(),
        };
        let plan = plan_guarded_cross_section_resource_regions_cancellable(
            initial,
            CrossSectionPlane::Xy,
            presentation,
            extent,
            spec,
            0,
            SemanticPlanLimits::new(64, 64),
            None,
            || false,
        )
        .unwrap();
        assert_eq!(plan.primary_regions, 4);
        assert!(plan.regions.len() > plan.primary_regions);
        assert!(
            plan.regions.len() < 8,
            "the fixture must exercise the swept proof rather than full-volume residency"
        );
        let guard = plan
            .plane_reuse_guard
            .expect("the swept body establishes a reuse proof");
        let envelope =
            SemanticPlaneReuseEnvelope::new(vec![guard], false, plan.work.candidates_visited)
                .unwrap();
        let guarded_origins = plan
            .regions
            .iter()
            .map(|region| region.origin())
            .collect::<BTreeSet<_>>();

        let mut state = CrossSectionViewState::from_canonical(initial);
        assert!(
            !envelope
                .needs_rolling_replan(initial, CrossSectionPlane::Xy, presentation, extent)
                .unwrap(),
            "a newly installed guard must not immediately replace itself"
        );
        for sample in 1..=400 {
            state.slice_by_world_distance(InteractionPanel::Xy, 0.01);
            let view = state.into_canonical().unwrap();
            assert!(
                envelope
                    .contains(view, CrossSectionPlane::Xy, presentation, extent)
                    .unwrap(),
                "the cumulative four-world-unit slice left the guard at sample {sample}"
            );
            let exact = plan_cross_section_resource_regions(
                view,
                CrossSectionPlane::Xy,
                presentation,
                extent,
                spec,
                0,
                SemanticPlanLimits::new(64, 64),
            )
            .unwrap();
            assert!(
                exact
                    .iter()
                    .all(|region| guarded_origins.contains(&region.origin()))
            );
        }
        let mut observed_overlap_window = false;
        for sample in 1..=300 {
            state.rotate_oblique_by_panel_drag(InteractionPanel::Xy, 0.2, 0.1, 0.005);
            let view = state.into_canonical().unwrap();
            assert!(
                envelope
                    .contains(view, CrossSectionPlane::Xy, presentation, extent)
                    .unwrap(),
                "the cumulative compound rotation left the guard at sample {sample}"
            );
            let exact = plan_cross_section_resource_regions(
                view,
                CrossSectionPlane::Xy,
                presentation,
                extent,
                spec,
                0,
                SemanticPlanLimits::new(64, 64),
            )
            .unwrap();
            assert!(
                exact
                    .iter()
                    .all(|region| guarded_origins.contains(&region.origin()))
            );
            observed_overlap_window |= envelope
                .needs_rolling_replan(view, CrossSectionPlane::Xy, presentation, extent)
                .unwrap();
        }
        assert!(
            observed_overlap_window,
            "a still-contained near-boundary sample must request an overlapping guard"
        );

        let outside = CrossSectionView::new(
            WorldPoint3::new(32.0, 32.0, 96.0).unwrap(),
            UnitQuaternion::identity(),
            1.0,
            1.0,
        )
        .unwrap();
        assert!(
            !envelope
                .contains(outside, CrossSectionPlane::Xy, presentation, extent)
                .unwrap()
        );
    }

    #[test]
    fn swept_capsule_still_validates_shader_addressing_before_reuse() {
        let presentation = PresentationViewport::new(64.0, 64.0).unwrap();
        let extent = RenderExtent::new(64, 64).unwrap();
        let spec = SemanticRegionGridSpec {
            volume_shape: Shape3D::new(128, 65, 65).unwrap(),
            resource_shape: Shape3D::new(64, 64, 64).unwrap(),
            grid_to_world: GridToWorld::identity(),
        };
        let initial = CrossSectionView::new(
            WorldPoint3::new(32.0, 32.0, 40.0).unwrap(),
            UnitQuaternion::identity(),
            1.0,
            1.0,
        )
        .unwrap();
        let plan = plan_guarded_cross_section_resource_regions_cancellable(
            initial,
            CrossSectionPlane::Xy,
            presentation,
            extent,
            spec,
            0,
            SemanticPlanLimits::new(64, 64),
            None,
            || false,
        )
        .unwrap();
        let envelope = SemanticPlaneReuseEnvelope::new(
            vec![
                plan.plane_reuse_guard
                    .expect("the swept body owns a reuse proof"),
            ],
            false,
            plan.work.candidates_visited,
        )
        .unwrap();
        let boundary = CrossSectionView::new(
            WorldPoint3::new(32.0, 32.0, 63.5).unwrap(),
            UnitQuaternion::identity(),
            1.0,
            1.0,
        )
        .unwrap();
        assert!(
            envelope
                .contains(boundary, CrossSectionPlane::Xy, presentation, extent)
                .unwrap()
        );
        let exact_boundary = plan_cross_section_resource_regions(
            boundary,
            CrossSectionPlane::Xy,
            presentation,
            extent,
            spec,
            0,
            SemanticPlanLimits::new(64, 64),
        )
        .unwrap();
        assert!(
            exact_boundary.iter().any(|region| region.origin()[0] == 64),
            "the independent exact plane must demonstrate the missing upper brick"
        );
        let guarded_origins = plan
            .regions
            .iter()
            .map(|region| region.origin())
            .collect::<BTreeSet<_>>();
        assert!(
            exact_boundary
                .iter()
                .all(|region| guarded_origins.contains(&region.origin()))
        );

        assert!(matches!(
            envelope.contains(
                boundary,
                CrossSectionPlane::Xy,
                presentation,
                RenderExtent::new((1 << 23) + 1, 64).unwrap(),
            ),
            Err(SemanticPlanError::ControlPrecision {
                field: "cross-section physical render extent"
            })
        ));
    }

    #[test]
    fn swept_capsule_contains_large_affine_all_panel_exact_bodies_for_the_full_workload() {
        let transform = GridToWorld::from_row_major([
            0.75, 0.10, 0.0, 10.0, 0.0, 0.90, 0.05, -4.0, 0.08, 0.0, 1.30, 2.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .unwrap();
        let spec = SemanticRegionGridSpec {
            volume_shape: Shape3D::new(1_025, 1_537, 2_049).unwrap(),
            resource_shape: Shape3D::new(64, 64, 64).unwrap(),
            grid_to_world: transform,
        };
        let center_world =
            grid_to_world_matrix(transform).transform_point3(DVec3::new(1_024.0, 768.0, 512.0));
        let orientation = DQuat::from_rotation_z(0.23) * DQuat::from_rotation_x(-0.19);
        let [x, y, z, w] = orientation.to_array();
        let initial = CrossSectionView::new(
            WorldPoint3::new(center_world.x, center_world.y, center_world.z).unwrap(),
            UnitQuaternion::new_xyzw(x, y, z, w).unwrap(),
            0.6,
            1_000.0,
        )
        .unwrap();
        let presentation = PresentationViewport::new(960.0, 540.0).unwrap();
        let extent = RenderExtent::new(960, 540).unwrap();
        let limits = SemanticPlanLimits::new(65_536, 65_536);
        let full_volume_regions = usize::try_from(
            spec.volume_shape.z().div_ceil(spec.resource_shape.z())
                * spec.volume_shape.y().div_ceil(spec.resource_shape.y())
                * spec.volume_shape.x().div_ceil(spec.resource_shape.x()),
        )
        .unwrap();

        for panel in [
            CrossSectionPlane::Xy,
            CrossSectionPlane::Xz,
            CrossSectionPlane::Yz,
        ] {
            for halo_voxels in [0, 1] {
                let plan = plan_guarded_cross_section_resource_regions_cancellable(
                    initial,
                    panel,
                    presentation,
                    extent,
                    spec,
                    halo_voxels,
                    limits,
                    None,
                    || false,
                )
                .unwrap();
                assert!(
                    plan.regions.len() > plan.primary_regions,
                    "panel={panel:?} halo={halo_voxels} must retain a real dormant suffix"
                );
                assert!(
                    plan.regions.len() < full_volume_regions,
                    "panel={panel:?} halo={halo_voxels} must prove a sweep, not fill the volume"
                );
                assert!(
                    plan.regions.len() <= 2_048,
                    "panel={panel:?} halo={halo_voxels} swept body {} exceeds a one-GiB uint16 64-cube cohort",
                    plan.regions.len()
                );
                let envelope = SemanticPlaneReuseEnvelope::new(
                    vec![
                        plan.plane_reuse_guard
                            .expect("the within-cap swept body must retain its proof"),
                    ],
                    false,
                    plan.work.candidates_visited,
                )
                .unwrap();
                let guarded_origins = plan
                    .regions
                    .iter()
                    .map(|region| region.origin())
                    .collect::<BTreeSet<_>>();
                let assert_sample = |sample: usize, phase: &str, view: CrossSectionView| {
                    assert!(
                        envelope
                            .contains(view, panel, presentation, extent)
                            .unwrap(),
                        "panel={panel:?} halo={halo_voxels} {phase} sample={sample} left the guard"
                    );
                    let exact = plan_cross_section_resource_regions(
                        view,
                        panel,
                        presentation,
                        extent,
                        spec,
                        halo_voxels,
                        limits,
                    )
                    .unwrap();
                    assert!(
                        exact
                            .iter()
                            .all(|region| guarded_origins.contains(&region.origin())),
                        "panel={panel:?} halo={halo_voxels} {phase} sample={sample} exact body escaped the immutable guard"
                    );
                };

                let mut state = CrossSectionViewState::from_canonical(initial);
                assert_sample(0, "initial", initial);
                for sample in 1..=400 {
                    state.slice_by_world_distance(InteractionPanel::Xy, 0.01);
                    assert_sample(sample, "slice", state.into_canonical().unwrap());
                }
                for sample in 1..=300 {
                    state.rotate_oblique_by_panel_drag(InteractionPanel::Xy, 0.2, 0.1, 0.005);
                    assert_sample(sample, "rotation", state.into_canonical().unwrap());
                }
            }
        }
    }

    #[test]
    fn residual_bounded_capsule_range_keeps_large_affine_boundary_bodies() {
        let transform = GridToWorld::from_row_major([
            0.700_000_3,
            0.110_000_1,
            -0.030_000_2,
            100_003.25,
            0.020_000_1,
            1.100_000_3,
            0.070_000_1,
            -80_007.5,
            0.090_000_2,
            -0.040_000_1,
            1.300_000_3,
            60_001.75,
            0.0,
            0.0,
            0.0,
            1.0,
        ])
        .unwrap();
        let spec = SemanticRegionGridSpec {
            volume_shape: Shape3D::new(65, 65, 500_001).unwrap(),
            resource_shape: Shape3D::new(64, 64, 64).unwrap(),
            grid_to_world: transform,
        };
        let boundary_grid_x = 64.0 * 7_500.0 - 0.5;
        let center_world = grid_to_world_matrix(transform).transform_point3(DVec3::new(
            boundary_grid_x,
            32.0,
            32.0,
        ));
        let initial = CrossSectionView::new(
            WorldPoint3::new(center_world.x, center_world.y, center_world.z).unwrap(),
            UnitQuaternion::identity(),
            0.25,
            100.0,
        )
        .unwrap();
        let presentation = PresentationViewport::new(32.0, 32.0).unwrap();
        let extent = RenderExtent::new(32, 32).unwrap();
        let limits = SemanticPlanLimits::new(4_096, 4_096);
        let plan = plan_guarded_cross_section_resource_regions_cancellable(
            initial,
            CrossSectionPlane::Yz,
            presentation,
            extent,
            spec,
            1,
            limits,
            None,
            || false,
        )
        .unwrap();
        let guard = plan
            .plane_reuse_guard
            .expect("the large-coordinate affine still admits a bounded guard");
        assert!(
            guard
                .effective_grid_to_world
                .grid_round_trip_residual
                .into_iter()
                .flatten()
                .any(|residual| residual > 0.0),
            "the fixture must exercise a real finite-precision inverse residual"
        );
        let envelope =
            SemanticPlaneReuseEnvelope::new(vec![guard], false, plan.work.candidates_visited)
                .unwrap();
        let guarded_origins = plan
            .regions
            .iter()
            .map(|region| region.origin())
            .collect::<BTreeSet<_>>();
        let assert_subset = |sample: usize, phase: &str, view: CrossSectionView| {
            assert!(
                envelope
                    .contains(view, CrossSectionPlane::Yz, presentation, extent)
                    .unwrap(),
                "{phase} sample {sample} left the residual-bounded capsule"
            );
            let exact = plan_cross_section_resource_regions(
                view,
                CrossSectionPlane::Yz,
                presentation,
                extent,
                spec,
                1,
                limits,
            )
            .unwrap();
            assert!(
                exact
                    .iter()
                    .all(|region| guarded_origins.contains(&region.origin())),
                "{phase} sample {sample} crossed an affine brick boundary outside the planned body"
            );
        };
        let mut state = CrossSectionViewState::from_canonical(initial);
        assert_subset(0, "initial", initial);
        for sample in 1..=400 {
            state.slice_by_world_distance(InteractionPanel::Xy, 0.01);
            assert_subset(sample, "slice", state.into_canonical().unwrap());
        }
        for sample in 1..=300 {
            state.rotate_oblique_by_panel_drag(InteractionPanel::Xy, 0.2, 0.1, 0.005);
            assert_subset(sample, "rotation", state.into_canonical().unwrap());
        }
    }

    #[test]
    fn cross_section_demand_contains_the_voxel_selected_by_shader_f32_at_a_brick_boundary() {
        let center_x = 63.499_999_f64;
        let view = CrossSectionView::new(
            WorldPoint3::new(center_x, 0.0, 0.0).unwrap(),
            UnitQuaternion::identity(),
            0.1,
            1.0,
        )
        .unwrap();
        let origins = plan_cross_section_resource_regions(
            view,
            CrossSectionPlane::Xy,
            PresentationViewport::new(1.0, 1.0).unwrap(),
            RenderExtent::new(1, 1).unwrap(),
            SemanticRegionGridSpec {
                volume_shape: Shape3D::new(1, 1, 128).unwrap(),
                resource_shape: Shape3D::new(1, 1, 64).unwrap(),
                grid_to_world: GridToWorld::identity(),
            },
            0,
            SemanticPlanLimits::new(8, 8),
        )
        .unwrap()
        .into_iter()
        .map(|region| region.origin())
        .collect::<BTreeSet<_>>();

        // Independent reproduction of the relevant WGSL operations: WGPU
        // packs the center as f32, identity world-to-grid leaves it unchanged,
        // and VoxelExact uses floor(grid + 0.5).
        let shader_grid_x = center_x as f32;
        let shader_voxel_x = (shader_grid_x + 0.5).floor() as u64;
        let shader_brick_x = shader_voxel_x / 64 * 64;
        assert_eq!(shader_grid_x, 63.5);
        assert_eq!(shader_voxel_x, 64);
        assert!(
            origins.contains(&[0, 0, shader_brick_x]),
            "the demand set must contain the brick the shader actually addresses"
        );
    }

    #[test]
    fn projected_plane_ranks_a_large_dominant_axis_intersection_before_a_sliver() {
        let angle = 40.0_f64.to_radians();
        let rotation = DQuat::from_rotation_y(angle);
        let [x, y, z, w] = rotation.to_array();
        let view = CrossSectionView::new(
            WorldPoint3::new(31.5, 31.5, 86.0).unwrap(),
            UnitQuaternion::new_xyzw(x, y, z, w).unwrap(),
            1.0,
            1.0,
        )
        .unwrap();
        let origins = plan_cross_section_resource_regions(
            view,
            CrossSectionPlane::Xy,
            PresentationViewport::new(156.0, 2.0).unwrap(),
            RenderExtent::new(2, 2).unwrap(),
            SemanticRegionGridSpec {
                volume_shape: Shape3D::new(128, 64, 64).unwrap(),
                resource_shape: Shape3D::new(64, 64, 64).unwrap(),
                grid_to_world: GridToWorld::identity(),
            },
            0,
            SemanticPlanLimits::new(16, 16),
        )
        .unwrap()
        .into_iter()
        .map(|region| region.origin())
        .collect::<Vec<_>>();

        // Independent geometry for this two-pixel fixture. The rotated
        // screen-right segment spans z=86±39*sin(40°); the two brick supports
        // meet at the voxel-center boundary z=63.5. Projected length differs
        // from z length only by the same positive scale factor.
        let half_screen_span = 39.0_f64;
        let z_low = 86.0 - half_screen_span * angle.sin();
        let z_high = 86.0 + half_screen_span * angle.sin();
        let brick_boundary = 63.5;
        assert!(z_low < brick_boundary && brick_boundary < z_high);
        let sliver_span = brick_boundary - z_low;
        let large_span = z_high - brick_boundary;
        assert!(large_span > 10.0 * sliver_span);
        assert_eq!(origins, vec![[64, 0, 0], [0, 0, 0]]);
    }

    #[test]
    fn projected_plane_matches_independent_brute_force_for_affine_oblique_panels_and_halos() {
        let transform = GridToWorld::from_row_major([
            0.75, 0.10, 0.0, 10.0, 0.0, 0.90, 0.05, -4.0, 0.08, 0.0, 1.30, 2.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .unwrap();
        let center_world =
            grid_to_world_matrix(transform).transform_point3(DVec3::new(159.5, 127.5, 95.5));
        let rotation = DQuat::from_rotation_z(0.37) * DQuat::from_rotation_x(-0.41);
        let [x, y, z, w] = rotation.to_array();
        let view = CrossSectionView::new(
            WorldPoint3::new(center_world.x, center_world.y, center_world.z).unwrap(),
            UnitQuaternion::new_xyzw(x, y, z, w).unwrap(),
            0.4,
            600.0,
        )
        .unwrap();
        let presentation = PresentationViewport::new(173.0, 121.0).unwrap();
        let extent = RenderExtent::new(7, 5).unwrap();
        let spec = SemanticRegionGridSpec {
            volume_shape: Shape3D::new(192, 256, 320).unwrap(),
            resource_shape: Shape3D::new(64, 64, 64).unwrap(),
            grid_to_world: transform,
        };

        for panel in [
            CrossSectionPlane::Xy,
            CrossSectionPlane::Xz,
            CrossSectionPlane::Yz,
        ] {
            for halo_voxels in [0, 1, 2] {
                let actual = plan_cross_section_resource_regions(
                    view,
                    panel,
                    presentation,
                    extent,
                    spec,
                    halo_voxels,
                    SemanticPlanLimits::new(256, 256),
                )
                .unwrap()
                .into_iter()
                .map(|region| region.origin())
                .collect::<BTreeSet<_>>();
                let expected = brute_force_cross_section_regions(
                    view,
                    panel,
                    presentation,
                    extent,
                    spec,
                    halo_voxels,
                );
                assert!(!expected.is_empty());
                assert_eq!(
                    actual, expected,
                    "panel={panel:?} halo_voxels={halo_voxels}"
                );
            }
        }
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
                RenderExtent::new(1, 1).unwrap(),
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

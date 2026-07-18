//! Independent projected-affine LOD facts for viewer qualification.
//!
//! This module deliberately does not call the product LOD selector. It works
//! only from immutable scale geometry and view facts so qualification can
//! detect a product selector that silently substitutes a coarser scale.

use mirante4d_dataset::{DatasetScale, MAX_SCALES_PER_LAYER};
use mirante4d_domain::{
    CameraView, CrossSectionView, GridToWorld, Projection, ScaleLevel, UnitQuaternion,
};
use mirante4d_render_api::{PresentationViewport, RenderExtent};
use thiserror::Error;

const MAX_PROJECTED_VOXEL_FOOTPRINT_PIXELS: f64 = 1.0;

/// One independently calculated scale observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProjectedScaleFacts {
    level: ScaleLevel,
    projected_footprint_x_pixels: f64,
    projected_footprint_y_pixels: f64,
    minimum_basis_world: f64,
    satisfies_screen_sampling: bool,
}

impl ProjectedScaleFacts {
    pub const fn level(self) -> ScaleLevel {
        self.level
    }

    pub const fn projected_footprint_x_pixels(self) -> f64 {
        self.projected_footprint_x_pixels
    }

    pub const fn projected_footprint_y_pixels(self) -> f64 {
        self.projected_footprint_y_pixels
    }

    pub const fn minimum_basis_world(self) -> f64 {
        self.minimum_basis_world
    }

    pub const fn satisfies_screen_sampling(self) -> bool {
        self.satisfies_screen_sampling
    }
}

/// The oracle-selected target and all scale facts used to select it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedAffineLodDecision {
    selected: ScaleLevel,
    scales: Box<[ProjectedScaleFacts]>,
}

impl ProjectedAffineLodDecision {
    pub const fn selected(&self) -> ScaleLevel {
        self.selected
    }

    pub fn scales(&self) -> &[ProjectedScaleFacts] {
        &self.scales
    }

    pub fn selected_facts(&self) -> ProjectedScaleFacts {
        self.scales
            .iter()
            .copied()
            .find(|facts| facts.level == self.selected)
            .expect("the selected scale is retained in the decision")
    }

    /// Checks a pinned volume sample step against the independently selected
    /// scale. A step greater than one shortest affine basis can skip an entire
    /// selected-scale voxel and cannot be accepted as that scale's fidelity.
    pub fn sample_step_is_supported(&self, sample_step_world: f64) -> bool {
        sample_step_world.is_finite()
            && sample_step_world > 0.0
            && sample_step_world <= self.selected_facts().minimum_basis_world
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum LodOracleError {
    #[error("the projected-affine LOD oracle requires at least one scale")]
    EmptyScales,
    #[error("the projected-affine LOD oracle received too many scales")]
    TooManyScales,
    #[error("the projected-affine LOD oracle requires scale zero")]
    MissingBaseScale,
    #[error("the projected-affine LOD oracle received duplicate scale levels")]
    DuplicateScale,
    #[error("the projected-affine LOD calculation produced invalid geometry")]
    InvalidGeometry,
    #[error("the volume lies on or behind the perspective eye plane")]
    UnprojectableVolume,
}

/// Stateless independent LOD oracle used by EP-00 evidence and tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProjectedAffineLodOracle;

/// Closed-form camera facts owned by the qualification oracle.
///
/// This intentionally does not use `mirante4d_render_api::CameraFrame`: the
/// product renderer and the oracle must not share the projection operation
/// whose correctness the oracle is intended to check.
#[derive(Debug, Clone, Copy)]
struct IndependentCamera {
    projection: Projection,
    eye: [f64; 3],
    right: [f64; 3],
    up: [f64; 3],
    forward: [f64; 3],
    orthographic_world_per_screen_point: f64,
    perspective_focal_length_screen_points: f64,
}

impl IndependentCamera {
    fn from_view(view: CameraView) -> Result<Self, LodOracleError> {
        let right = normalized(rotate(view.orientation(), [1.0, 0.0, 0.0])?)?;
        let up = normalized(rotate(view.orientation(), [0.0, 1.0, 0.0])?)?;
        let forward = normalized(rotate(view.orientation(), [0.0, 0.0, -1.0])?)?;
        let target = view.target().components();
        let eye = subtract(
            target,
            multiply(forward, view.perspective_view_distance_world())?,
        )?;
        if !is_finite_positive(view.orthographic_world_per_screen_point())
            || !is_finite_positive(view.perspective_focal_length_screen_points())
        {
            return Err(LodOracleError::InvalidGeometry);
        }
        Ok(Self {
            projection: view.projection(),
            eye,
            right,
            up,
            forward,
            orthographic_world_per_screen_point: view.orthographic_world_per_screen_point(),
            perspective_focal_length_screen_points: view.perspective_focal_length_screen_points(),
        })
    }

    fn project_screen_points(self, world: [f64; 3]) -> Result<[f64; 2], LodOracleError> {
        let relative = subtract(world, self.eye)?;
        let depth_world = checked(dot(relative, self.forward))?;
        if depth_world <= 0.0 {
            return Err(LodOracleError::UnprojectableVolume);
        }
        let right_world = checked(dot(relative, self.right))?;
        let up_world = checked(dot(relative, self.up))?;
        let scale = match self.projection {
            Projection::Perspective => {
                checked(self.perspective_focal_length_screen_points / depth_world)?
            }
            Projection::Orthographic => checked(1.0 / self.orthographic_world_per_screen_point)?,
        };
        Ok([checked(right_world * scale)?, checked(up_world * scale)?])
    }
}

impl ProjectedAffineLodOracle {
    pub const fn new() -> Self {
        Self
    }

    /// Selects the coarsest scale whose projected affine voxel cell is no
    /// larger than one physical render pixel along either plane axis.
    ///
    /// The footprint is the conservative projection of the voxel
    /// parallelepiped: the sum of the absolute projections of all three affine
    /// basis vectors. This remains meaningful for rotated, sheared, and mixed
    /// grids and intentionally differs from the product's predecessor
    /// representative-voxel scalar.
    pub fn cross_section(
        self,
        scales: &[DatasetScale],
        view: CrossSectionView,
        presentation: PresentationViewport,
        extent: RenderExtent,
    ) -> Result<ProjectedAffineLodDecision, LodOracleError> {
        let scales = checked_scales(scales)?;
        let [right, up] = cross_section_axes(view);
        let world_per_pixel_x = view.scale_world_per_screen_point() * presentation.width_points()
            / f64::from(extent.width_pixels());
        let world_per_pixel_y = view.scale_world_per_screen_point() * presentation.height_points()
            / f64::from(extent.height_pixels());
        if !is_finite_positive(world_per_pixel_x) || !is_finite_positive(world_per_pixel_y) {
            return Err(LodOracleError::InvalidGeometry);
        }

        decision(scales, |scale| {
            let basis = affine_basis(scale.grid_to_world())?;
            let footprint_x = projected_cell_extent(basis, right) / world_per_pixel_x;
            let footprint_y = projected_cell_extent(basis, up) / world_per_pixel_y;
            scale_facts(scale, footprint_x, footprint_y, basis)
        })
    }

    /// Selects the coarsest scale whose voxel cell remains within one render
    /// pixel at every projectable volume corner. Perspective evaluation uses
    /// exact point projections for the corner and its three basis offsets,
    /// making the closest admitted corner the conservative screen-space case.
    pub fn volume(
        self,
        scales: &[DatasetScale],
        camera: CameraView,
        presentation: PresentationViewport,
        extent: RenderExtent,
    ) -> Result<ProjectedAffineLodDecision, LodOracleError> {
        let scales = checked_scales(scales)?;
        let camera = IndependentCamera::from_view(camera)?;
        decision(scales, |scale| {
            let basis = affine_basis(scale.grid_to_world())?;
            let [shape_x, shape_y, shape_z] = [
                scale.shape().x() as f64,
                scale.shape().y() as f64,
                scale.shape().z() as f64,
            ];
            let mut maximum_x = 0.0_f64;
            let mut maximum_y = 0.0_f64;
            for z in [-0.5, shape_z - 0.5] {
                for y in [-0.5, shape_y - 0.5] {
                    for x in [-0.5, shape_x - 0.5] {
                        let world = transform_point(scale.grid_to_world(), [x, y, z])?;
                        let projected = project_pixels(camera, world, presentation, extent)?;
                        let mut footprint_x = 0.0;
                        let mut footprint_y = 0.0;
                        for vector in basis {
                            let offset = add(world, vector)?;
                            let offset = project_pixels(camera, offset, presentation, extent)?;
                            footprint_x += (offset[0] - projected[0]).abs();
                            footprint_y += (offset[1] - projected[1]).abs();
                        }
                        maximum_x = maximum_x.max(footprint_x);
                        maximum_y = maximum_y.max(footprint_y);
                    }
                }
            }
            scale_facts(scale, maximum_x, maximum_y, basis)
        })
    }
}

fn checked_scales(scales: &[DatasetScale]) -> Result<&[DatasetScale], LodOracleError> {
    if scales.is_empty() {
        return Err(LodOracleError::EmptyScales);
    }
    if scales.len() > MAX_SCALES_PER_LAYER {
        return Err(LodOracleError::TooManyScales);
    }
    if !scales.iter().any(|scale| scale.level() == ScaleLevel::BASE) {
        return Err(LodOracleError::MissingBaseScale);
    }
    for (index, scale) in scales.iter().enumerate() {
        if scales[..index]
            .iter()
            .any(|other| other.level() == scale.level())
        {
            return Err(LodOracleError::DuplicateScale);
        }
    }
    Ok(scales)
}

fn decision(
    scales: &[DatasetScale],
    mut calculate: impl FnMut(DatasetScale) -> Result<ProjectedScaleFacts, LodOracleError>,
) -> Result<ProjectedAffineLodDecision, LodOracleError> {
    let mut facts = Vec::with_capacity(scales.len());
    for scale in scales.iter().copied() {
        facts.push(calculate(scale)?);
    }
    facts.sort_by_key(|facts| facts.level.get());
    let selected = facts
        .iter()
        .rev()
        .find(|facts| facts.satisfies_screen_sampling)
        .map_or(ScaleLevel::BASE, |facts| facts.level);
    Ok(ProjectedAffineLodDecision {
        selected,
        scales: facts.into_boxed_slice(),
    })
}

fn scale_facts(
    scale: DatasetScale,
    footprint_x: f64,
    footprint_y: f64,
    basis: [[f64; 3]; 3],
) -> Result<ProjectedScaleFacts, LodOracleError> {
    let minimum_basis_world = basis
        .iter()
        .map(|vector| dot(*vector, *vector).sqrt())
        .fold(f64::INFINITY, f64::min);
    if !footprint_x.is_finite()
        || footprint_x < 0.0
        || !footprint_y.is_finite()
        || footprint_y < 0.0
        || !is_finite_positive(minimum_basis_world)
    {
        return Err(LodOracleError::InvalidGeometry);
    }
    Ok(ProjectedScaleFacts {
        level: scale.level(),
        projected_footprint_x_pixels: footprint_x,
        projected_footprint_y_pixels: footprint_y,
        minimum_basis_world,
        satisfies_screen_sampling: footprint_x <= MAX_PROJECTED_VOXEL_FOOTPRINT_PIXELS
            && footprint_y <= MAX_PROJECTED_VOXEL_FOOTPRINT_PIXELS,
    })
}

fn affine_basis(transform: GridToWorld) -> Result<[[f64; 3]; 3], LodOracleError> {
    let matrix = transform.row_major();
    let basis = [
        [matrix[0], matrix[4], matrix[8]],
        [matrix[1], matrix[5], matrix[9]],
        [matrix[2], matrix[6], matrix[10]],
    ];
    if basis.iter().flatten().all(|value| value.is_finite()) {
        Ok(basis)
    } else {
        Err(LodOracleError::InvalidGeometry)
    }
}

fn cross_section_axes(view: CrossSectionView) -> [[f64; 3]; 2] {
    let [x, y, z, w] = view.orientation().xyzw();
    let rotate = |vector: [f64; 3]| {
        let cross = [
            y * vector[2] - z * vector[1],
            z * vector[0] - x * vector[2],
            x * vector[1] - y * vector[0],
        ];
        let twice_cross = cross.map(|value| 2.0 * value);
        let second_cross = [
            y * twice_cross[2] - z * twice_cross[1],
            z * twice_cross[0] - x * twice_cross[2],
            x * twice_cross[1] - y * twice_cross[0],
        ];
        std::array::from_fn(|axis| vector[axis] + w * twice_cross[axis] + second_cross[axis])
    };
    [rotate([1.0, 0.0, 0.0]), rotate([0.0, 1.0, 0.0])]
}

fn projected_cell_extent(basis: [[f64; 3]; 3], axis: [f64; 3]) -> f64 {
    basis.iter().map(|vector| dot(*vector, axis).abs()).sum()
}

fn transform_point(transform: GridToWorld, point: [f64; 3]) -> Result<[f64; 3], LodOracleError> {
    if !point.iter().all(|component| component.is_finite()) {
        return Err(LodOracleError::InvalidGeometry);
    }
    let matrix = transform.row_major();
    let transformed = [
        matrix[0] * point[0] + matrix[1] * point[1] + matrix[2] * point[2] + matrix[3],
        matrix[4] * point[0] + matrix[5] * point[1] + matrix[6] * point[2] + matrix[7],
        matrix[8] * point[0] + matrix[9] * point[1] + matrix[10] * point[2] + matrix[11],
    ];
    if transformed.iter().all(|component| component.is_finite()) {
        Ok(transformed)
    } else {
        Err(LodOracleError::InvalidGeometry)
    }
}

fn project_pixels(
    camera: IndependentCamera,
    world: [f64; 3],
    presentation: PresentationViewport,
    extent: RenderExtent,
) -> Result<[f64; 2], LodOracleError> {
    let projected = camera.project_screen_points(world)?;
    Ok([
        projected[0] * f64::from(extent.width_pixels()) / presentation.width_points(),
        projected[1] * f64::from(extent.height_pixels()) / presentation.height_points(),
    ])
}

fn add(left: [f64; 3], right: [f64; 3]) -> Result<[f64; 3], LodOracleError> {
    let value = std::array::from_fn(|axis| left[axis] + right[axis]);
    if value.iter().all(|component| component.is_finite()) {
        Ok(value)
    } else {
        Err(LodOracleError::InvalidGeometry)
    }
}

fn subtract(left: [f64; 3], right: [f64; 3]) -> Result<[f64; 3], LodOracleError> {
    checked_vector(std::array::from_fn(|axis| left[axis] - right[axis]))
}

fn multiply(vector: [f64; 3], scalar: f64) -> Result<[f64; 3], LodOracleError> {
    checked_vector(vector.map(|component| component * scalar))
}

fn checked_vector(value: [f64; 3]) -> Result<[f64; 3], LodOracleError> {
    if value.iter().all(|component| component.is_finite()) {
        Ok(value)
    } else {
        Err(LodOracleError::InvalidGeometry)
    }
}

fn checked(value: f64) -> Result<f64, LodOracleError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(LodOracleError::InvalidGeometry)
    }
}

fn normalized(vector: [f64; 3]) -> Result<[f64; 3], LodOracleError> {
    let length = checked(dot(vector, vector))?.sqrt();
    if !is_finite_positive(length) {
        return Err(LodOracleError::InvalidGeometry);
    }
    multiply(vector, 1.0 / length)
}

fn rotate(quaternion: UnitQuaternion, vector: [f64; 3]) -> Result<[f64; 3], LodOracleError> {
    let [x, y, z, w] = quaternion.xyzw();
    let imaginary = [x, y, z];
    let twice_cross = multiply(cross(imaginary, vector), 2.0)?;
    add(
        add(vector, multiply(twice_cross, w)?)?,
        cross(imaginary, twice_cross),
    )
}

fn cross(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn is_finite_positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

#[cfg(test)]
mod tests {
    use mirante4d_dataset::ResourceValidity;
    use mirante4d_domain::{Projection, Shape3D, UnitQuaternion, WorldPoint3};

    use super::*;

    fn scale(level: u32, edge: u64, spacing: f64, translation: [f64; 3]) -> DatasetScale {
        let transform = GridToWorld::from_row_major([
            spacing,
            0.0,
            0.0,
            translation[0],
            0.0,
            spacing,
            0.0,
            translation[1],
            0.0,
            0.0,
            spacing,
            translation[2],
            0.0,
            0.0,
            0.0,
            1.0,
        ])
        .unwrap();
        DatasetScale::new(
            ScaleLevel::new(level),
            Shape3D::new(edge, edge, edge).unwrap(),
            transform,
            ResourceValidity::AllValid,
        )
    }

    fn presentation() -> PresentationViewport {
        PresentationViewport::new(512.0, 512.0).unwrap()
    }

    fn extent() -> RenderExtent {
        RenderExtent::new(512, 512).unwrap()
    }

    fn cross(world_per_point: f64, orientation: UnitQuaternion) -> CrossSectionView {
        CrossSectionView::new(WorldPoint3::origin(), orientation, world_per_point, 1.0).unwrap()
    }

    #[test]
    fn perspective_projection_matches_closed_form_camera_geometry() {
        let view = CameraView::new(
            Projection::Perspective,
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            1.0,
            100.0,
            10.0,
        )
        .unwrap();
        let camera = IndependentCamera::from_view(view).unwrap();

        // Identity orientation looks down -Z. The eye is (0, 0, 10), so this
        // point is 10 world units deep with right/up offsets 2 and 3.
        let projected = camera.project_screen_points([2.0, 3.0, 0.0]).unwrap();
        assert!((projected[0] - 20.0).abs() < 1.0e-12);
        assert!((projected[1] - 30.0).abs() < 1.0e-12);
        assert_eq!(
            camera.project_screen_points([0.0, 0.0, 10.0]),
            Err(LodOracleError::UnprojectableVolume)
        );
        assert_eq!(
            camera.project_screen_points([0.0, 0.0, 11.0]),
            Err(LodOracleError::UnprojectableVolume)
        );
    }

    #[test]
    fn orthographic_projection_uses_rotated_camera_axes() {
        let half_angle = std::f64::consts::FRAC_PI_4;
        let orientation =
            UnitQuaternion::new_xyzw(0.0, half_angle.sin(), 0.0, half_angle.cos()).unwrap();
        let view = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::origin(),
            orientation,
            2.0,
            100.0,
            10.0,
        )
        .unwrap();
        let camera = IndependentCamera::from_view(view).unwrap();

        // A +90 degree world-Y rotation maps camera right to -Z and leaves up
        // on +Y. The target remains ten world units in front of the eye.
        let projected = camera.project_screen_points([0.0, -6.0, -4.0]).unwrap();
        assert!((projected[0] - 2.0).abs() < 1.0e-12);
        assert!((projected[1] + 3.0).abs() < 1.0e-12);
    }

    #[test]
    fn cross_section_selects_coarsest_scale_with_one_pixel_affine_footprint() {
        let scales = [
            scale(0, 64, 1.0, [0.0; 3]),
            scale(1, 32, 2.0, [0.5; 3]),
            scale(2, 16, 4.0, [1.5; 3]),
        ];
        let exact = ProjectedAffineLodOracle::new()
            .cross_section(
                &scales,
                cross(1.0, UnitQuaternion::identity()),
                presentation(),
                extent(),
            )
            .unwrap();
        assert_eq!(exact.selected(), ScaleLevel::BASE);

        let coarse = ProjectedAffineLodOracle::new()
            .cross_section(
                &scales,
                cross(2.0, UnitQuaternion::identity()),
                presentation(),
                extent(),
            )
            .unwrap();
        assert_eq!(coarse.selected(), ScaleLevel::new(1));
    }

    #[test]
    fn cross_section_uses_physical_render_pixels_not_ui_points() {
        let scales = [scale(0, 64, 1.0, [0.0; 3])];
        let decision = ProjectedAffineLodOracle::new()
            .cross_section(
                &scales,
                cross(1.0, UnitQuaternion::identity()),
                presentation(),
                RenderExtent::new(1024, 1024).unwrap(),
            )
            .unwrap();
        assert_eq!(decision.selected(), ScaleLevel::BASE);
        assert_eq!(decision.scales()[0].projected_footprint_x_pixels(), 2.0);
        assert!(!decision.scales()[0].satisfies_screen_sampling());
    }

    #[test]
    fn rotated_affine_footprint_is_not_reduced_to_one_scalar_norm() {
        let forty_five = UnitQuaternion::new_xyzw(
            0.0,
            0.0,
            (std::f64::consts::FRAC_PI_8).sin(),
            (std::f64::consts::FRAC_PI_8).cos(),
        )
        .unwrap();
        let scales = [scale(0, 64, 0.5, [0.0; 3]), scale(1, 32, 1.0, [0.25; 3])];
        let decision = ProjectedAffineLodOracle::new()
            .cross_section(&scales, cross(1.0, forty_five), presentation(), extent())
            .unwrap();
        assert_eq!(decision.selected(), ScaleLevel::BASE);
        assert!(decision.scales()[1].projected_footprint_x_pixels() > 1.0);
    }

    #[test]
    fn orthographic_volume_and_sample_step_have_independent_fidelity_facts() {
        let scales = [
            scale(0, 64, 1.0, [-32.0, -32.0, 1.0]),
            scale(1, 32, 2.0, [-31.5, -31.5, 1.5]),
        ];
        let camera = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::new(0.0, 0.0, 32.0).unwrap(),
            UnitQuaternion::identity(),
            1.0,
            512.0,
            128.0,
        )
        .unwrap();
        let decision = ProjectedAffineLodOracle::new()
            .volume(&scales, camera, presentation(), extent())
            .unwrap();
        assert_eq!(decision.selected(), ScaleLevel::BASE);
        assert!(decision.sample_step_is_supported(1.0));
        assert!(!decision.sample_step_is_supported(1.000_000_1));
    }

    #[test]
    fn perspective_volume_rejects_geometry_behind_the_eye_plane() {
        let scales = [scale(0, 8, 1.0, [0.0; 3])];
        let camera = CameraView::new(
            Projection::Perspective,
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            1.0,
            512.0,
            1.0,
        )
        .unwrap();
        assert_eq!(
            ProjectedAffineLodOracle::new().volume(&scales, camera, presentation(), extent()),
            Err(LodOracleError::UnprojectableVolume)
        );
    }
}

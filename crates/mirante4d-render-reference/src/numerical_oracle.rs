//! Independent CPU numerical facts for the EP-00 rendering contract.
//!
//! This module owns no product renderer structures and does not translate or
//! execute shader source. It evaluates small, bounded scalar volumes in `f64`
//! from explicit affine, sampling, transfer, ray, and mode facts. Consumers
//! can therefore compare production cross-section, MIP, DVR, ISO, and pick
//! observations with an independently ordered calculation.

use mirante4d_domain::GridToWorld;
use thiserror::Error;

const MAX_ORACLE_VOXELS: u64 = 1024 * 1024;
const MAX_ORACLE_RAY_SAMPLES: u64 = 65_536;
const MAX_ORACLE_COMPOSITE_LAYERS: usize = 64;
const GRADIENT_LENGTH_SQUARED_EPSILON: f64 = 1.0e-24;

/// Exact scientific state of one independently supplied scalar voxel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NumericalVoxel {
    Missing,
    Invalid,
    Valid(f64),
}

/// Exact semantic result of one sampling footprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericalSampleState {
    Outside,
    Missing,
    Invalid,
    Valid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericalSampling {
    VoxelExact,
    SmoothLinear,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum NumericalOracleError {
    #[error("the numerical oracle volume shape must be nonzero")]
    EmptyShape,
    #[error("the numerical oracle volume voxel count overflowed")]
    VoxelCountOverflow,
    #[error("the numerical oracle volume exceeds its voxel bound")]
    VolumeTooLarge,
    #[error("the numerical oracle voxel count does not match its shape")]
    VoxelCountMismatch,
    #[error("the numerical oracle received a non-finite valid scalar")]
    NonFiniteVoxel,
    #[error("the numerical oracle grid-to-world transform is singular")]
    SingularTransform,
    #[error("the numerical oracle received an invalid transfer contract")]
    InvalidTransfer,
    #[error("the numerical oracle received an invalid plane")]
    InvalidPlane,
    #[error("the numerical oracle received an invalid world ray")]
    InvalidRay,
    #[error("the numerical oracle received an invalid volume-mode contract")]
    InvalidMode,
    #[error("the numerical oracle sample step must be finite and positive")]
    InvalidSampleStep,
    #[error("one numerical oracle ray exceeds its sample-count bound")]
    RaySampleLimitExceeded,
    #[error("the numerical oracle composite exceeds its layer bound")]
    CompositeLayerLimitExceeded,
}

/// One positive-weight footprint tap. Zero-weight taps are deliberately
/// absent, so invalid or missing values at those taps cannot poison a sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericalTapFacts {
    grid_xyz: [u32; 3],
    weight: f64,
    state: NumericalSampleState,
    value: Option<f64>,
}

impl NumericalTapFacts {
    pub const fn grid_xyz(self) -> [u32; 3] {
        self.grid_xyz
    }

    pub const fn weight(self) -> f64 {
        self.weight
    }

    pub const fn state(self) -> NumericalSampleState {
        self.state
    }

    pub const fn value(self) -> Option<f64> {
        self.value
    }
}

/// Independent scalar and footprint facts at one grid-space point.
#[derive(Debug, Clone, PartialEq)]
pub struct NumericalSampleFacts {
    grid_position: [f64; 3],
    state: NumericalSampleState,
    value: Option<f64>,
    taps: Box<[NumericalTapFacts]>,
}

impl NumericalSampleFacts {
    pub const fn grid_position(&self) -> [f64; 3] {
        self.grid_position
    }

    pub const fn state(&self) -> NumericalSampleState {
        self.state
    }

    pub const fn value(&self) -> Option<f64> {
        self.value
    }

    pub fn taps(&self) -> &[NumericalTapFacts] {
        &self.taps
    }
}

/// Bounded dense scalar facts used only by this independent CPU oracle.
#[derive(Debug, Clone, PartialEq)]
pub struct NumericalVolume {
    shape_xyz: [u32; 3],
    grid_to_world: [f64; 16],
    world_to_grid: InverseAffine,
    voxels: Box<[NumericalVoxel]>,
}

impl NumericalVolume {
    /// `voxels` are tightly ordered with X fastest, then Y, then Z.
    pub fn new(
        shape_xyz: [u32; 3],
        grid_to_world: GridToWorld,
        voxels: Vec<NumericalVoxel>,
    ) -> Result<Self, NumericalOracleError> {
        if shape_xyz.contains(&0) {
            return Err(NumericalOracleError::EmptyShape);
        }
        let count = shape_xyz
            .into_iter()
            .try_fold(1_u64, |count, axis| count.checked_mul(u64::from(axis)));
        let count = count.ok_or(NumericalOracleError::VoxelCountOverflow)?;
        if count > MAX_ORACLE_VOXELS {
            return Err(NumericalOracleError::VolumeTooLarge);
        }
        if usize::try_from(count).ok() != Some(voxels.len()) {
            return Err(NumericalOracleError::VoxelCountMismatch);
        }
        if voxels
            .iter()
            .any(|voxel| matches!(voxel, NumericalVoxel::Valid(value) if !value.is_finite()))
        {
            return Err(NumericalOracleError::NonFiniteVoxel);
        }
        let grid_to_world = grid_to_world.row_major();
        let world_to_grid =
            InverseAffine::new(grid_to_world).ok_or(NumericalOracleError::SingularTransform)?;
        Ok(Self {
            shape_xyz,
            grid_to_world,
            world_to_grid,
            voxels: voxels.into_boxed_slice(),
        })
    }

    pub const fn shape_xyz(&self) -> [u32; 3] {
        self.shape_xyz
    }

    pub const fn grid_to_world_row_major(&self) -> [f64; 16] {
        self.grid_to_world
    }

    fn voxel(&self, xyz: [u32; 3]) -> NumericalVoxel {
        let [shape_x, shape_y, _shape_z] = self.shape_xyz.map(u64::from);
        let index = u64::from(xyz[2])
            .checked_mul(shape_y)
            .and_then(|index| index.checked_add(u64::from(xyz[1])))
            .and_then(|index| index.checked_mul(shape_x))
            .and_then(|index| index.checked_add(u64::from(xyz[0])))
            .and_then(|index| usize::try_from(index).ok())
            .expect("validated bounded volume indexing cannot overflow");
        self.voxels[index]
    }

    fn world_point(&self, grid_xyz: [f64; 3]) -> [f64; 3] {
        let matrix = self.grid_to_world;
        std::array::from_fn(|row| {
            matrix[row * 4] * grid_xyz[0]
                + matrix[row * 4 + 1] * grid_xyz[1]
                + matrix[row * 4 + 2] * grid_xyz[2]
                + matrix[row * 4 + 3]
        })
    }
}

/// Transfer order frozen by EP-00: normalize by the authored window, clamp,
/// optionally invert, then apply gamma.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericalTransfer {
    window: [f64; 2],
    gamma: f64,
    inverted: bool,
    color: [f64; 3],
    opacity: f64,
}

impl NumericalTransfer {
    pub fn new(
        window: [f64; 2],
        gamma: f64,
        inverted: bool,
        color: [f64; 3],
        opacity: f64,
    ) -> Result<Self, NumericalOracleError> {
        if !window.into_iter().all(f64::is_finite)
            || window[1] <= window[0]
            || !gamma.is_finite()
            || gamma <= 0.0
            || !color
                .into_iter()
                .all(|component| component.is_finite() && (0.0..=1.0).contains(&component))
            || !opacity.is_finite()
            || !(0.0..=1.0).contains(&opacity)
        {
            return Err(NumericalOracleError::InvalidTransfer);
        }
        Ok(Self {
            window,
            gamma,
            inverted,
            color,
            opacity,
        })
    }

    pub const fn window(self) -> [f64; 2] {
        self.window
    }

    pub const fn gamma(self) -> f64 {
        self.gamma
    }

    pub const fn inverted(self) -> bool {
        self.inverted
    }

    pub const fn color(self) -> [f64; 3] {
        self.color
    }

    pub const fn opacity(self) -> f64 {
        self.opacity
    }

    pub fn display_value(self, value: f64) -> f64 {
        curve_value(value, self.window, self.gamma, self.inverted)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericalDvrParameters {
    opacity_window: [f64; 2],
    opacity_gamma: f64,
    density_per_world_unit: f64,
}

impl NumericalDvrParameters {
    pub fn new(
        opacity_window: [f64; 2],
        opacity_gamma: f64,
        density_per_world_unit: f64,
    ) -> Result<Self, NumericalOracleError> {
        if !opacity_window.into_iter().all(f64::is_finite)
            || opacity_window[1] <= opacity_window[0]
            || !opacity_gamma.is_finite()
            || opacity_gamma <= 0.0
            || !density_per_world_unit.is_finite()
            || density_per_world_unit < 0.0
        {
            return Err(NumericalOracleError::InvalidMode);
        }
        Ok(Self {
            opacity_window,
            opacity_gamma,
            density_per_world_unit,
        })
    }

    pub const fn opacity_window(self) -> [f64; 2] {
        self.opacity_window
    }

    pub const fn opacity_gamma(self) -> f64 {
        self.opacity_gamma
    }

    pub const fn density_per_world_unit(self) -> f64 {
        self.density_per_world_unit
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NumericalIsoShading {
    Flat,
    Gradient {
        light_direction_world: [f64; 3],
        ambient: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericalIsoParameters {
    display_level: f64,
    shading: NumericalIsoShading,
}

impl NumericalIsoParameters {
    pub fn new(
        display_level: f64,
        shading: NumericalIsoShading,
    ) -> Result<Self, NumericalOracleError> {
        if !display_level.is_finite() || !(0.0..=1.0).contains(&display_level) {
            return Err(NumericalOracleError::InvalidMode);
        }
        if let NumericalIsoShading::Gradient {
            light_direction_world,
            ambient,
        } = shading
            && (normalized(light_direction_world).is_none()
                || !ambient.is_finite()
                || !(0.0..=1.0).contains(&ambient))
        {
            return Err(NumericalOracleError::InvalidMode);
        }
        Ok(Self {
            display_level,
            shading,
        })
    }

    pub const fn display_level(self) -> f64 {
        self.display_level
    }

    pub const fn shading(self) -> NumericalIsoShading {
        self.shading
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NumericalVolumeMode {
    Mip,
    Dvr(NumericalDvrParameters),
    Iso(NumericalIsoParameters),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericalVolumeModeKind {
    Mip,
    Dvr,
    Iso,
}

impl NumericalVolumeMode {
    pub const fn kind(self) -> NumericalVolumeModeKind {
        match self {
            Self::Mip => NumericalVolumeModeKind::Mip,
            Self::Dvr(_) => NumericalVolumeModeKind::Dvr,
            Self::Iso(_) => NumericalVolumeModeKind::Iso,
        }
    }
}

/// World position of pixel `(0, 0)` plus exact per-pixel world increments.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericalCrossSectionPlane {
    first_pixel_center_world: [f64; 3],
    pixel_x_world: [f64; 3],
    pixel_y_world: [f64; 3],
}

impl NumericalCrossSectionPlane {
    pub fn new(
        first_pixel_center_world: [f64; 3],
        pixel_x_world: [f64; 3],
        pixel_y_world: [f64; 3],
    ) -> Result<Self, NumericalOracleError> {
        if !first_pixel_center_world.into_iter().all(f64::is_finite)
            || normalized(pixel_x_world).is_none()
            || normalized(pixel_y_world).is_none()
        {
            return Err(NumericalOracleError::InvalidPlane);
        }
        Ok(Self {
            first_pixel_center_world,
            pixel_x_world,
            pixel_y_world,
        })
    }

    pub fn pixel_center_world(self, pixel_xy: [u32; 2]) -> [f64; 3] {
        std::array::from_fn(|axis| {
            self.first_pixel_center_world[axis]
                + self.pixel_x_world[axis] * f64::from(pixel_xy[0])
                + self.pixel_y_world[axis] * f64::from(pixel_xy[1])
        })
    }
}

/// A direction-normalized ray. Its parameter is therefore physical world
/// distance even for off-axis perspective rays.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericalWorldRay {
    origin: [f64; 3],
    direction: [f64; 3],
}

impl NumericalWorldRay {
    pub fn new(origin: [f64; 3], direction: [f64; 3]) -> Result<Self, NumericalOracleError> {
        if !origin.into_iter().all(f64::is_finite) {
            return Err(NumericalOracleError::InvalidRay);
        }
        let direction = normalized(direction).ok_or(NumericalOracleError::InvalidRay)?;
        Ok(Self { origin, direction })
    }

    pub const fn origin(self) -> [f64; 3] {
        self.origin
    }

    pub const fn direction(self) -> [f64; 3] {
        self.direction
    }

    pub fn point_at_world_distance(self, distance: f64) -> [f64; 3] {
        std::array::from_fn(|axis| self.origin[axis] + self.direction[axis] * distance)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericalVolumeQuery {
    ray: NumericalWorldRay,
    sampling: NumericalSampling,
    transfer: NumericalTransfer,
    mode: NumericalVolumeMode,
    sample_step_world: f64,
}

impl NumericalVolumeQuery {
    pub fn new(
        ray: NumericalWorldRay,
        sampling: NumericalSampling,
        transfer: NumericalTransfer,
        mode: NumericalVolumeMode,
        sample_step_world: f64,
    ) -> Result<Self, NumericalOracleError> {
        if !sample_step_world.is_finite() || sample_step_world <= 0.0 {
            return Err(NumericalOracleError::InvalidSampleStep);
        }
        Ok(Self {
            ray,
            sampling,
            transfer,
            mode,
            sample_step_world,
        })
    }

    pub const fn ray(self) -> NumericalWorldRay {
        self.ray
    }

    pub const fn sampling(self) -> NumericalSampling {
        self.sampling
    }

    pub const fn transfer(self) -> NumericalTransfer {
        self.transfer
    }

    pub const fn mode(self) -> NumericalVolumeMode {
        self.mode
    }

    pub const fn sample_step_world(self) -> f64 {
        self.sample_step_world
    }
}

/// Unquantized premultiplied output plus exact semantic and RGBA8 facts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericalColorFacts {
    premultiplied_rgba: [f64; 4],
    rgba8: [u8; 4],
    covered: bool,
    valid: bool,
}

impl NumericalColorFacts {
    pub const fn premultiplied_rgba(self) -> [f64; 4] {
        self.premultiplied_rgba
    }

    pub const fn rgba8(self) -> [u8; 4] {
        self.rgba8
    }

    pub const fn covered(self) -> bool {
        self.covered
    }

    pub const fn valid(self) -> bool {
        self.valid
    }

    fn new(premultiplied_rgba: [f64; 4], covered: bool, valid: bool) -> Self {
        let premultiplied_rgba = premultiplied_rgba.map(|value| value.clamp(0.0, 1.0));
        Self {
            rgba8: premultiplied_rgba.map(quantize),
            premultiplied_rgba,
            covered,
            valid,
        }
    }

    fn transparent(covered: bool, valid: bool) -> Self {
        Self::new([0.0; 4], covered, valid)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NumericalCrossSectionFacts {
    pixel_xy: [u32; 2],
    world_position: [f64; 3],
    sample: NumericalSampleFacts,
    color: NumericalColorFacts,
}

impl NumericalCrossSectionFacts {
    pub const fn pixel_xy(&self) -> [u32; 2] {
        self.pixel_xy
    }

    pub const fn world_position(&self) -> [f64; 3] {
        self.world_position
    }

    pub const fn sample(&self) -> &NumericalSampleFacts {
        &self.sample
    }

    pub const fn color(&self) -> NumericalColorFacts {
        self.color
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericalPickKind {
    Voxel,
    InterpolatedSample,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericalPickCompleteness {
    Exact,
    Incomplete,
}

/// Mode-specific pick selected by the same independent ray facts as color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericalPickFacts {
    kind: NumericalPickKind,
    value: f64,
    world_position: [f64; 3],
    grid_position: [f64; 3],
    ray_distance_world: f64,
    sample_ordinal: u64,
    selection_score: f64,
    completeness: NumericalPickCompleteness,
}

impl NumericalPickFacts {
    pub const fn kind(self) -> NumericalPickKind {
        self.kind
    }

    pub const fn value(self) -> f64 {
        self.value
    }

    pub const fn world_position(self) -> [f64; 3] {
        self.world_position
    }

    pub const fn grid_position(self) -> [f64; 3] {
        self.grid_position
    }

    pub const fn ray_distance_world(self) -> f64 {
        self.ray_distance_world
    }

    pub const fn sample_ordinal(self) -> u64 {
        self.sample_ordinal
    }

    pub const fn selection_score(self) -> f64 {
        self.selection_score
    }

    pub const fn completeness(self) -> NumericalPickCompleteness {
        self.completeness
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NumericalVolumeFacts {
    mode: NumericalVolumeModeKind,
    ray_entry_world: Option<f64>,
    ray_exit_world: Option<f64>,
    sample_count: u64,
    encountered_missing: bool,
    encountered_invalid: bool,
    color: NumericalColorFacts,
    hit_depth_world: Option<f64>,
    pick: Option<NumericalPickFacts>,
}

impl NumericalVolumeFacts {
    pub const fn mode(&self) -> NumericalVolumeModeKind {
        self.mode
    }

    pub const fn ray_entry_world(&self) -> Option<f64> {
        self.ray_entry_world
    }

    pub const fn ray_exit_world(&self) -> Option<f64> {
        self.ray_exit_world
    }

    pub const fn sample_count(&self) -> u64 {
        self.sample_count
    }

    pub const fn encountered_missing(&self) -> bool {
        self.encountered_missing
    }

    pub const fn encountered_invalid(&self) -> bool {
        self.encountered_invalid
    }

    pub const fn color(&self) -> NumericalColorFacts {
        self.color
    }

    /// First ISO threshold depth in physical world units. MIP and DVR expose
    /// their scientific selection distance through `pick` instead.
    pub const fn hit_depth_world(&self) -> Option<f64> {
        self.hit_depth_world
    }

    pub const fn pick(&self) -> Option<NumericalPickFacts> {
        self.pick
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NumericalCompositeFacts {
    color: NumericalColorFacts,
    source_order: Box<[usize]>,
}

impl NumericalCompositeFacts {
    pub const fn color(&self) -> NumericalColorFacts {
        self.color
    }

    pub fn source_order(&self) -> &[usize] {
        &self.source_order
    }
}

/// Frozen numerical comparison bounds. Semantic states, coverage, validity,
/// selected order, and sample ordinals remain exact comparisons. RGBA8 allows
/// one code value per channel because the independent oracle evaluates in
/// `f64` while the portable GPU contract evaluates in `f32` before the final
/// quantization boundary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericalConformanceContract {
    scalar_absolute_tolerance: f64,
    scalar_relative_tolerance: f64,
    premultiplied_rgba_absolute_tolerance: f64,
    world_position_absolute_tolerance: f64,
    ray_distance_absolute_tolerance: f64,
    rgba8_channel_tolerance: u8,
}

impl NumericalConformanceContract {
    pub const fn independent() -> Self {
        Self {
            scalar_absolute_tolerance: 1.0e-6,
            scalar_relative_tolerance: 4.0 * f32::EPSILON as f64,
            premultiplied_rgba_absolute_tolerance: 2.0e-6,
            world_position_absolute_tolerance: 1.0e-5,
            ray_distance_absolute_tolerance: 1.0e-5,
            rgba8_channel_tolerance: 1,
        }
    }

    pub const fn scalar_absolute_tolerance(self) -> f64 {
        self.scalar_absolute_tolerance
    }

    pub const fn scalar_relative_tolerance(self) -> f64 {
        self.scalar_relative_tolerance
    }

    pub const fn premultiplied_rgba_absolute_tolerance(self) -> f64 {
        self.premultiplied_rgba_absolute_tolerance
    }

    pub const fn world_position_absolute_tolerance(self) -> f64 {
        self.world_position_absolute_tolerance
    }

    pub const fn ray_distance_absolute_tolerance(self) -> f64 {
        self.ray_distance_absolute_tolerance
    }

    pub const fn rgba8_channel_tolerance(self) -> u8 {
        self.rgba8_channel_tolerance
    }

    pub fn scalar_matches(self, expected: f64, observed: f64) -> bool {
        finite_close(
            expected,
            observed,
            self.scalar_absolute_tolerance,
            self.scalar_relative_tolerance,
        )
    }

    pub fn premultiplied_rgba_matches(self, expected: [f64; 4], observed: [f64; 4]) -> bool {
        expected
            .into_iter()
            .zip(observed)
            .all(|(expected, observed)| {
                finite_close(
                    expected,
                    observed,
                    self.premultiplied_rgba_absolute_tolerance,
                    0.0,
                )
            })
    }

    pub fn world_position_matches(self, expected: [f64; 3], observed: [f64; 3]) -> bool {
        expected
            .into_iter()
            .zip(observed)
            .all(|(expected, observed)| {
                finite_close(
                    expected,
                    observed,
                    self.world_position_absolute_tolerance,
                    0.0,
                )
            })
    }

    pub fn ray_distance_matches(self, expected: f64, observed: f64) -> bool {
        finite_close(
            expected,
            observed,
            self.ray_distance_absolute_tolerance,
            0.0,
        )
    }

    pub fn rgba8_matches(self, expected: [u8; 4], observed: [u8; 4]) -> bool {
        expected
            .into_iter()
            .zip(observed)
            .all(|(expected, observed)| expected.abs_diff(observed) <= self.rgba8_channel_tolerance)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NumericalConformanceOracle;

impl NumericalConformanceOracle {
    pub const fn new() -> Self {
        Self
    }

    pub fn sample_grid(
        self,
        volume: &NumericalVolume,
        grid_position: [f64; 3],
        sampling: NumericalSampling,
    ) -> NumericalSampleFacts {
        sample_grid(volume, grid_position, sampling)
    }

    pub fn cross_section_pixel(
        self,
        volume: &NumericalVolume,
        plane: NumericalCrossSectionPlane,
        pixel_xy: [u32; 2],
        sampling: NumericalSampling,
        transfer: NumericalTransfer,
    ) -> NumericalCrossSectionFacts {
        let world_position = plane.pixel_center_world(pixel_xy);
        let grid_position = volume.world_to_grid.point(world_position);
        let sample = sample_grid(volume, grid_position, sampling);
        let color = color_from_sample(&sample, transfer);
        NumericalCrossSectionFacts {
            pixel_xy,
            world_position,
            sample,
            color,
        }
    }

    pub fn volume(
        self,
        volume: &NumericalVolume,
        query: NumericalVolumeQuery,
    ) -> Result<NumericalVolumeFacts, NumericalOracleError> {
        let Some(prepared) = prepare_ray(volume, query)? else {
            return Ok(empty_volume_facts(query.mode.kind()));
        };
        match query.mode {
            NumericalVolumeMode::Mip => render_mip(volume, query, prepared),
            NumericalVolumeMode::Dvr(parameters) => render_dvr(volume, query, parameters, prepared),
            NumericalVolumeMode::Iso(parameters) => render_iso(volume, query, parameters, prepared),
        }
    }

    /// Composites explicit authored front-to-back layer order.
    pub fn composite_authored(
        self,
        layers: &[NumericalColorFacts],
    ) -> Result<NumericalCompositeFacts, NumericalOracleError> {
        checked_composite_len(layers.len())?;
        let order = (0..layers.len()).collect::<Vec<_>>();
        Ok(NumericalCompositeFacts {
            color: composite_in_order(layers, &order),
            source_order: order.into_boxed_slice(),
        })
    }

    /// Composites ISO hits nearest-first using physical world-ray depth.
    /// Equal-depth ties retain authored order.
    pub fn composite_iso_depth_ordered(
        self,
        layers: &[NumericalVolumeFacts],
    ) -> Result<NumericalCompositeFacts, NumericalOracleError> {
        checked_composite_len(layers.len())?;
        let mut order = layers
            .iter()
            .enumerate()
            .filter_map(|(index, facts)| facts.hit_depth_world.map(|depth| (index, depth)))
            .collect::<Vec<_>>();
        order.sort_by(|(left_index, left_depth), (right_index, right_depth)| {
            left_depth
                .total_cmp(right_depth)
                .then(left_index.cmp(right_index))
        });
        let order = order
            .into_iter()
            .map(|(index, _depth)| index)
            .collect::<Vec<_>>();
        let colors = layers.iter().map(|facts| facts.color).collect::<Vec<_>>();
        let mut color = composite_in_order(&colors, &order);
        color.covered = layers.iter().all(|facts| facts.color.covered);
        color.valid = layers.iter().any(|facts| facts.color.valid);
        Ok(NumericalCompositeFacts {
            color,
            source_order: order.into_boxed_slice(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct InverseAffine {
    inverse: [[f64; 3]; 3],
    translation: [f64; 3],
}

impl InverseAffine {
    fn new(matrix: [f64; 16]) -> Option<Self> {
        let a = [
            [matrix[0], matrix[1], matrix[2]],
            [matrix[4], matrix[5], matrix[6]],
            [matrix[8], matrix[9], matrix[10]],
        ];
        let determinant = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
            - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
        if !determinant.is_finite() || determinant.abs() <= f64::MIN_POSITIVE {
            return None;
        }
        let inverse_determinant = determinant.recip();
        let inverse = [
            [
                (a[1][1] * a[2][2] - a[1][2] * a[2][1]) * inverse_determinant,
                (a[0][2] * a[2][1] - a[0][1] * a[2][2]) * inverse_determinant,
                (a[0][1] * a[1][2] - a[0][2] * a[1][1]) * inverse_determinant,
            ],
            [
                (a[1][2] * a[2][0] - a[1][0] * a[2][2]) * inverse_determinant,
                (a[0][0] * a[2][2] - a[0][2] * a[2][0]) * inverse_determinant,
                (a[0][2] * a[1][0] - a[0][0] * a[1][2]) * inverse_determinant,
            ],
            [
                (a[1][0] * a[2][1] - a[1][1] * a[2][0]) * inverse_determinant,
                (a[0][1] * a[2][0] - a[0][0] * a[2][1]) * inverse_determinant,
                (a[0][0] * a[1][1] - a[0][1] * a[1][0]) * inverse_determinant,
            ],
        ];
        inverse
            .iter()
            .flatten()
            .all(|value| value.is_finite())
            .then_some(Self {
                inverse,
                translation: [matrix[3], matrix[7], matrix[11]],
            })
    }

    fn point(self, world: [f64; 3]) -> [f64; 3] {
        self.vector(std::array::from_fn(|axis| {
            world[axis] - self.translation[axis]
        }))
    }

    fn vector(self, world: [f64; 3]) -> [f64; 3] {
        std::array::from_fn(|row| dot(self.inverse[row], world))
    }

    fn normal(self, grid: [f64; 3]) -> [f64; 3] {
        std::array::from_fn(|column| {
            self.inverse[0][column] * grid[0]
                + self.inverse[1][column] * grid[1]
                + self.inverse[2][column] * grid[2]
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct PreparedRay {
    grid_origin: [f64; 3],
    grid_direction_per_world: [f64; 3],
    entry_world: f64,
    exit_world: f64,
    count: u64,
}

fn sample_grid(
    volume: &NumericalVolume,
    grid_position: [f64; 3],
    sampling: NumericalSampling,
) -> NumericalSampleFacts {
    if !grid_position.into_iter().all(f64::is_finite)
        || (0..3).any(|axis| {
            grid_position[axis] < -0.5
                || grid_position[axis] >= f64::from(volume.shape_xyz[axis]) - 0.5
        })
    {
        return NumericalSampleFacts {
            grid_position,
            state: NumericalSampleState::Outside,
            value: None,
            taps: Box::new([]),
        };
    }
    match sampling {
        NumericalSampling::VoxelExact => sample_nearest(volume, grid_position),
        NumericalSampling::SmoothLinear => sample_smooth(volume, grid_position),
    }
}

fn sample_nearest(volume: &NumericalVolume, grid_position: [f64; 3]) -> NumericalSampleFacts {
    let grid_xyz = std::array::from_fn(|axis| {
        let rounded = (grid_position[axis] + 0.5).floor();
        rounded.clamp(0.0, f64::from(volume.shape_xyz[axis] - 1)) as u32
    });
    let voxel = volume.voxel(grid_xyz);
    let (state, value) = voxel_state_value(voxel);
    NumericalSampleFacts {
        grid_position,
        state,
        value,
        taps: vec![NumericalTapFacts {
            grid_xyz,
            weight: 1.0,
            state,
            value,
        }]
        .into_boxed_slice(),
    }
}

fn sample_smooth(volume: &NumericalVolume, grid_position: [f64; 3]) -> NumericalSampleFacts {
    let mut lower = [0_u32; 3];
    let mut upper = [0_u32; 3];
    let mut fraction = [0.0_f64; 3];
    for axis in 0..3 {
        let coordinate = grid_position[axis].clamp(0.0, f64::from(volume.shape_xyz[axis] - 1));
        lower[axis] = coordinate.floor() as u32;
        upper[axis] = (lower[axis] + 1).min(volume.shape_xyz[axis] - 1);
        fraction[axis] = coordinate - f64::from(lower[axis]);
    }

    let mut taps = Vec::with_capacity(8);
    let mut accumulated = 0.0;
    let mut missing = false;
    let mut invalid = false;
    for z_upper in [false, true] {
        for y_upper in [false, true] {
            for x_upper in [false, true] {
                let upper_axes = [x_upper, y_upper, z_upper];
                let weight = (0..3)
                    .map(|axis| {
                        if upper_axes[axis] {
                            fraction[axis]
                        } else {
                            1.0 - fraction[axis]
                        }
                    })
                    .product::<f64>();
                if weight == 0.0 {
                    continue;
                }
                let grid_xyz = std::array::from_fn(|axis| {
                    if upper_axes[axis] {
                        upper[axis]
                    } else {
                        lower[axis]
                    }
                });
                let (state, value) = voxel_state_value(volume.voxel(grid_xyz));
                match state {
                    NumericalSampleState::Valid => {
                        accumulated += value.expect("valid tap has a scalar") * weight;
                    }
                    NumericalSampleState::Missing => missing = true,
                    NumericalSampleState::Invalid => invalid = true,
                    NumericalSampleState::Outside => {
                        unreachable!("clamped SmoothLinear tap is inside the volume")
                    }
                }
                taps.push(NumericalTapFacts {
                    grid_xyz,
                    weight,
                    state,
                    value,
                });
            }
        }
    }
    let (state, value) = if missing {
        (NumericalSampleState::Missing, None)
    } else if invalid {
        (NumericalSampleState::Invalid, None)
    } else {
        (NumericalSampleState::Valid, Some(accumulated))
    };
    NumericalSampleFacts {
        grid_position,
        state,
        value,
        taps: taps.into_boxed_slice(),
    }
}

fn voxel_state_value(voxel: NumericalVoxel) -> (NumericalSampleState, Option<f64>) {
    match voxel {
        NumericalVoxel::Missing => (NumericalSampleState::Missing, None),
        NumericalVoxel::Invalid => (NumericalSampleState::Invalid, None),
        NumericalVoxel::Valid(value) => (NumericalSampleState::Valid, Some(value)),
    }
}

fn color_from_sample(
    sample: &NumericalSampleFacts,
    transfer: NumericalTransfer,
) -> NumericalColorFacts {
    match (sample.state, sample.value) {
        (NumericalSampleState::Valid, Some(value)) => {
            color_from_display(transfer, transfer.display_value(value), 1.0, true, true)
        }
        (NumericalSampleState::Missing, None) => NumericalColorFacts::transparent(false, false),
        (NumericalSampleState::Invalid | NumericalSampleState::Outside, None) => {
            NumericalColorFacts::transparent(true, false)
        }
        _ => unreachable!("sample state and value are internally coherent"),
    }
}

fn color_from_display(
    transfer: NumericalTransfer,
    display: f64,
    lighting: f64,
    covered: bool,
    valid: bool,
) -> NumericalColorFacts {
    let alpha = transfer.opacity;
    let premultiplied_rgba = [
        transfer.color[0] * display * lighting * alpha,
        transfer.color[1] * display * lighting * alpha,
        transfer.color[2] * display * lighting * alpha,
        alpha,
    ];
    NumericalColorFacts::new(premultiplied_rgba, covered, valid)
}

fn prepare_ray(
    volume: &NumericalVolume,
    query: NumericalVolumeQuery,
) -> Result<Option<PreparedRay>, NumericalOracleError> {
    let grid_origin = volume.world_to_grid.point(query.ray.origin);
    let grid_direction_per_world = volume.world_to_grid.vector(query.ray.direction);
    let Some((entry_world, exit_world)) =
        intersect_grid(grid_origin, grid_direction_per_world, volume.shape_xyz)
    else {
        return Ok(None);
    };
    let entry_world = entry_world.max(0.0);
    if exit_world <= entry_world {
        return Ok(None);
    }
    let count = ((exit_world - entry_world) / query.sample_step_world)
        .ceil()
        .max(1.0) as u64;
    if count > MAX_ORACLE_RAY_SAMPLES {
        return Err(NumericalOracleError::RaySampleLimitExceeded);
    }
    Ok(Some(PreparedRay {
        grid_origin,
        grid_direction_per_world,
        entry_world,
        exit_world,
        count,
    }))
}

fn intersect_grid(
    origin: [f64; 3],
    direction: [f64; 3],
    shape_xyz: [u32; 3],
) -> Option<(f64, f64)> {
    let mut entry = f64::NEG_INFINITY;
    let mut exit = f64::INFINITY;
    for axis in 0..3 {
        let lower = -0.5;
        let upper = f64::from(shape_xyz[axis]) - 0.5;
        if direction[axis] == 0.0 {
            if origin[axis] < lower || origin[axis] >= upper {
                return None;
            }
            continue;
        }
        let first = (lower - origin[axis]) / direction[axis];
        let second = (upper - origin[axis]) / direction[axis];
        entry = entry.max(first.min(second));
        exit = exit.min(first.max(second));
        if exit <= entry {
            return None;
        }
    }
    (entry.is_finite() && exit.is_finite()).then_some((entry, exit))
}

fn ray_sample(
    volume: &NumericalVolume,
    query: NumericalVolumeQuery,
    prepared: PreparedRay,
    ordinal: u64,
) -> (f64, [f64; 3], NumericalSampleFacts) {
    let distance = prepared.entry_world + (ordinal as f64 + 0.5) * query.sample_step_world;
    let grid_position = std::array::from_fn(|axis| {
        prepared.grid_origin[axis] + prepared.grid_direction_per_world[axis] * distance
    });
    let sample = sample_grid(volume, grid_position, query.sampling);
    (distance, grid_position, sample)
}

fn render_mip(
    volume: &NumericalVolume,
    query: NumericalVolumeQuery,
    prepared: PreparedRay,
) -> Result<NumericalVolumeFacts, NumericalOracleError> {
    let mut missing = false;
    let mut invalid = false;
    let mut selected: Option<(f64, [f64; 3], NumericalSampleFacts, u64)> = None;
    for ordinal in 0..prepared.count {
        let (distance, grid_position, sample) = ray_sample(volume, query, prepared, ordinal);
        match sample.state {
            NumericalSampleState::Missing => missing = true,
            NumericalSampleState::Invalid => invalid = true,
            NumericalSampleState::Outside => {}
            NumericalSampleState::Valid => {
                let value = sample.value.expect("valid sample has a scalar");
                if selected
                    .as_ref()
                    .is_none_or(|(_, _, best, _)| value > best.value.expect("best sample is valid"))
                {
                    selected = Some((distance, grid_position, sample, ordinal));
                }
            }
        }
    }
    let (color, pick) = selected.map_or_else(
        || (NumericalColorFacts::transparent(!missing, false), None),
        |(distance, grid_position, sample, ordinal)| {
            let value = sample.value.expect("selected MIP sample is valid");
            (
                color_from_display(
                    query.transfer,
                    query.transfer.display_value(value),
                    1.0,
                    !missing,
                    true,
                ),
                Some(pick_facts(
                    volume,
                    query,
                    PickSelection {
                        sample: &sample,
                        grid_position,
                        distance,
                        ordinal,
                        selection_score: value,
                        complete: !missing,
                    },
                )),
            )
        },
    );
    Ok(NumericalVolumeFacts {
        mode: NumericalVolumeModeKind::Mip,
        ray_entry_world: Some(prepared.entry_world),
        ray_exit_world: Some(prepared.exit_world),
        sample_count: prepared.count,
        encountered_missing: missing,
        encountered_invalid: invalid,
        color,
        hit_depth_world: None,
        pick,
    })
}

fn render_dvr(
    volume: &NumericalVolume,
    query: NumericalVolumeQuery,
    parameters: NumericalDvrParameters,
    prepared: PreparedRay,
) -> Result<NumericalVolumeFacts, NumericalOracleError> {
    let mut missing = false;
    let mut invalid = false;
    let mut any_valid = false;
    let mut transmittance = 1.0_f64;
    let mut premultiplied_rgb = [0.0_f64; 3];
    let mut selected: Option<(f64, [f64; 3], NumericalSampleFacts, u64, f64)> = None;
    for ordinal in 0..prepared.count {
        let (distance, grid_position, sample) = ray_sample(volume, query, prepared, ordinal);
        match sample.state {
            NumericalSampleState::Missing => missing = true,
            NumericalSampleState::Invalid => invalid = true,
            NumericalSampleState::Outside => {}
            NumericalSampleState::Valid => {
                any_valid = true;
                let value = sample.value.expect("valid sample has a scalar");
                let opacity_display = curve_value(
                    value,
                    parameters.opacity_window,
                    parameters.opacity_gamma,
                    false,
                );
                let optical_depth =
                    (opacity_display * parameters.density_per_world_unit * query.sample_step_world)
                        .max(0.0);
                let base_alpha = 1.0 - (-optical_depth).exp();
                let sample_alpha = query.transfer.opacity * base_alpha;
                let contribution = transmittance * sample_alpha;
                if selected
                    .as_ref()
                    .is_none_or(|(_, _, _, _, best)| contribution > *best)
                {
                    selected = Some((
                        distance,
                        grid_position,
                        sample.clone(),
                        ordinal,
                        contribution,
                    ));
                }
                let emitted = query.transfer.display_value(value);
                for (output, color) in premultiplied_rgb.iter_mut().zip(query.transfer.color) {
                    *output += contribution * color * emitted;
                }
                transmittance *= 1.0 - sample_alpha;
            }
        }
    }
    let alpha = 1.0 - transmittance;
    let color = NumericalColorFacts::new(
        [
            premultiplied_rgb[0],
            premultiplied_rgb[1],
            premultiplied_rgb[2],
            alpha,
        ],
        !missing,
        any_valid,
    );
    let pick = selected.map(|(distance, grid_position, sample, ordinal, score)| {
        pick_facts(
            volume,
            query,
            PickSelection {
                sample: &sample,
                grid_position,
                distance,
                ordinal,
                selection_score: score,
                complete: !missing,
            },
        )
    });
    Ok(NumericalVolumeFacts {
        mode: NumericalVolumeModeKind::Dvr,
        ray_entry_world: Some(prepared.entry_world),
        ray_exit_world: Some(prepared.exit_world),
        sample_count: prepared.count,
        encountered_missing: missing,
        encountered_invalid: invalid,
        color,
        hit_depth_world: None,
        pick,
    })
}

fn render_iso(
    volume: &NumericalVolume,
    query: NumericalVolumeQuery,
    parameters: NumericalIsoParameters,
    prepared: PreparedRay,
) -> Result<NumericalVolumeFacts, NumericalOracleError> {
    let mut missing = false;
    let mut invalid = false;
    let mut any_valid = false;
    let mut color = NumericalColorFacts::transparent(true, false);
    let mut hit_depth_world = None;
    let mut pick = None;
    for ordinal in 0..prepared.count {
        let (distance, grid_position, sample) = ray_sample(volume, query, prepared, ordinal);
        match sample.state {
            NumericalSampleState::Missing => missing = true,
            NumericalSampleState::Invalid => invalid = true,
            NumericalSampleState::Outside => {}
            NumericalSampleState::Valid => {
                any_valid = true;
                let value = sample.value.expect("valid sample has a scalar");
                let display = query.transfer.display_value(value);
                if display >= parameters.display_level {
                    let (lighting, lighting_missing) =
                        iso_lighting(volume, grid_position, query.sampling, parameters.shading);
                    missing |= lighting_missing;
                    color = color_from_display(query.transfer, display, lighting, !missing, true);
                    hit_depth_world = Some(distance);
                    pick = Some(pick_facts(
                        volume,
                        query,
                        PickSelection {
                            sample: &sample,
                            grid_position,
                            distance,
                            ordinal,
                            selection_score: display,
                            complete: !missing,
                        },
                    ));
                    break;
                }
            }
        }
    }
    if hit_depth_world.is_none() {
        color = NumericalColorFacts::transparent(!missing, any_valid);
    }
    Ok(NumericalVolumeFacts {
        mode: NumericalVolumeModeKind::Iso,
        ray_entry_world: Some(prepared.entry_world),
        ray_exit_world: Some(prepared.exit_world),
        sample_count: prepared.count,
        encountered_missing: missing,
        encountered_invalid: invalid,
        color,
        hit_depth_world,
        pick,
    })
}

struct PickSelection<'a> {
    sample: &'a NumericalSampleFacts,
    grid_position: [f64; 3],
    distance: f64,
    ordinal: u64,
    selection_score: f64,
    complete: bool,
}

fn pick_facts(
    volume: &NumericalVolume,
    query: NumericalVolumeQuery,
    selection: PickSelection<'_>,
) -> NumericalPickFacts {
    let value = selection
        .sample
        .value
        .expect("selected pick sample is valid");
    let (kind, world_position) = match query.sampling {
        NumericalSampling::VoxelExact => {
            let grid = selection.sample.taps[0].grid_xyz.map(f64::from);
            (NumericalPickKind::Voxel, volume.world_point(grid))
        }
        NumericalSampling::SmoothLinear => (
            NumericalPickKind::InterpolatedSample,
            query.ray.point_at_world_distance(selection.distance),
        ),
    };
    NumericalPickFacts {
        kind,
        value,
        world_position,
        grid_position: selection.grid_position,
        ray_distance_world: selection.distance,
        sample_ordinal: selection.ordinal,
        selection_score: selection.selection_score,
        completeness: if selection.complete {
            NumericalPickCompleteness::Exact
        } else {
            NumericalPickCompleteness::Incomplete
        },
    }
}

fn iso_lighting(
    volume: &NumericalVolume,
    grid_position: [f64; 3],
    sampling: NumericalSampling,
    shading: NumericalIsoShading,
) -> (f64, bool) {
    let NumericalIsoShading::Gradient {
        light_direction_world,
        ambient,
    } = shading
    else {
        return (1.0, false);
    };
    let mut missing = false;
    let mut gradient_grid = [0.0_f64; 3];
    for axis in 0..3 {
        let mut negative = grid_position;
        negative[axis] -= 1.0;
        let negative = sample_grid(volume, negative, sampling);
        let mut positive = grid_position;
        positive[axis] += 1.0;
        let positive = sample_grid(volume, positive, sampling);
        missing |= negative.state == NumericalSampleState::Missing
            || positive.state == NumericalSampleState::Missing;
        let (Some(negative), Some(positive)) = (negative.value, positive.value) else {
            return (ambient, missing);
        };
        gradient_grid[axis] = positive - negative;
    }
    let Some(normal) = normalized(volume.world_to_grid.normal(gradient_grid)) else {
        return (ambient, missing);
    };
    let light = normalized(light_direction_world)
        .expect("validated gradient-light contract has a direction");
    let diffuse = dot(normal, light).abs();
    (ambient + (1.0 - ambient) * diffuse, missing)
}

fn empty_volume_facts(mode: NumericalVolumeModeKind) -> NumericalVolumeFacts {
    NumericalVolumeFacts {
        mode,
        ray_entry_world: None,
        ray_exit_world: None,
        sample_count: 0,
        encountered_missing: false,
        encountered_invalid: false,
        color: NumericalColorFacts::transparent(true, false),
        hit_depth_world: None,
        pick: None,
    }
}

fn checked_composite_len(len: usize) -> Result<(), NumericalOracleError> {
    if len > MAX_ORACLE_COMPOSITE_LAYERS {
        Err(NumericalOracleError::CompositeLayerLimitExceeded)
    } else {
        Ok(())
    }
}

fn composite_in_order(layers: &[NumericalColorFacts], order: &[usize]) -> NumericalColorFacts {
    let mut rgba = [0.0_f64; 4];
    let mut covered = true;
    let mut valid = false;
    for &index in order {
        let layer = layers[index];
        let remaining = 1.0 - rgba[3];
        for (output, source) in rgba[..3].iter_mut().zip(&layer.premultiplied_rgba[..3]) {
            *output += remaining * source;
        }
        rgba[3] += remaining * layer.premultiplied_rgba[3];
        covered &= layer.covered;
        valid |= layer.valid;
    }
    NumericalColorFacts::new(rgba, covered, valid)
}

fn curve_value(value: f64, window: [f64; 2], gamma: f64, inverted: bool) -> f64 {
    let mut normalized = ((value - window[0]) / (window[1] - window[0])).clamp(0.0, 1.0);
    if inverted {
        normalized = 1.0 - normalized;
    }
    normalized.powf(gamma)
}

fn quantize(value: f64) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn normalized(value: [f64; 3]) -> Option<[f64; 3]> {
    let length_squared = dot(value, value);
    if !length_squared.is_finite() || length_squared <= GRADIENT_LENGTH_SQUARED_EPSILON {
        return None;
    }
    let inverse_length = length_squared.sqrt().recip();
    let normalized = value.map(|component| component * inverse_length);
    normalized
        .into_iter()
        .all(f64::is_finite)
        .then_some(normalized)
}

fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn finite_close(expected: f64, observed: f64, absolute: f64, relative: f64) -> bool {
    expected.is_finite()
        && observed.is_finite()
        && (expected - observed).abs() <= absolute + relative * expected.abs().max(observed.abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_volume(shape_xyz: [u32; 3], voxels: Vec<NumericalVoxel>) -> NumericalVolume {
        NumericalVolume::new(shape_xyz, GridToWorld::identity(), voxels).unwrap()
    }

    fn transfer(color: [f64; 3], opacity: f64) -> NumericalTransfer {
        NumericalTransfer::new([0.0, 1.0], 1.0, false, color, opacity).unwrap()
    }

    fn z_ray(origin_z: f64) -> NumericalWorldRay {
        NumericalWorldRay::new([0.0, 0.0, origin_z], [0.0, 0.0, -7.0]).unwrap()
    }

    fn query(
        ray: NumericalWorldRay,
        sampling: NumericalSampling,
        transfer: NumericalTransfer,
        mode: NumericalVolumeMode,
        step: f64,
    ) -> NumericalVolumeQuery {
        NumericalVolumeQuery::new(ray, sampling, transfer, mode, step).unwrap()
    }

    #[test]
    fn smooth_linear_ignores_zero_weight_invalid_and_missing_taps() {
        let volume = identity_volume(
            [2, 2, 2],
            vec![
                NumericalVoxel::Valid(0.25),
                NumericalVoxel::Invalid,
                NumericalVoxel::Missing,
                NumericalVoxel::Invalid,
                NumericalVoxel::Missing,
                NumericalVoxel::Invalid,
                NumericalVoxel::Missing,
                NumericalVoxel::Invalid,
            ],
        );
        let exact_corner = NumericalConformanceOracle::new().sample_grid(
            &volume,
            [0.0, 0.0, 0.0],
            NumericalSampling::SmoothLinear,
        );
        assert_eq!(exact_corner.state(), NumericalSampleState::Valid);
        assert_eq!(exact_corner.value(), Some(0.25));
        assert_eq!(exact_corner.taps().len(), 1);
        assert_eq!(exact_corner.taps()[0].weight(), 1.0);

        let invalid_weighted = NumericalConformanceOracle::new().sample_grid(
            &volume,
            [0.5, 0.0, 0.0],
            NumericalSampling::SmoothLinear,
        );
        assert_eq!(invalid_weighted.state(), NumericalSampleState::Invalid);

        let missing_and_invalid = NumericalConformanceOracle::new().sample_grid(
            &volume,
            [0.5, 0.5, 0.0],
            NumericalSampling::SmoothLinear,
        );
        assert_eq!(missing_and_invalid.state(), NumericalSampleState::Missing);
    }

    #[test]
    fn cross_section_uses_f64_affine_plane_coefficients_at_large_translation() {
        let transform = GridToWorld::from_row_major([
            0.0,
            -2.0,
            0.0,
            1_000_000_000.25,
            2.0,
            0.0,
            0.0,
            -999_999_999.75,
            0.0,
            0.0,
            3.0,
            500_000_000.5,
            0.0,
            0.0,
            0.0,
            1.0,
        ])
        .unwrap();
        let volume = NumericalVolume::new(
            [2, 1, 1],
            transform,
            vec![NumericalVoxel::Valid(0.25), NumericalVoxel::Valid(0.75)],
        )
        .unwrap();
        let matrix = transform.row_major();
        let first = [matrix[3], matrix[7], matrix[11]];
        let x_step = [matrix[0], matrix[4], matrix[8]];
        let plane = NumericalCrossSectionPlane::new(first, x_step, [0.0, 0.0, 1.0]).unwrap();
        let facts = NumericalConformanceOracle::new().cross_section_pixel(
            &volume,
            plane,
            [1, 0],
            NumericalSampling::VoxelExact,
            transfer([1.0, 0.0, 0.0], 1.0),
        );
        assert_eq!(facts.sample().state(), NumericalSampleState::Valid);
        assert_eq!(facts.sample().value(), Some(0.75));
        assert_eq!(facts.color().rgba8(), [191, 0, 0, 255]);
    }

    #[test]
    fn mip_keeps_nearest_equal_argmax_and_separates_validity_from_coverage() {
        let volume = identity_volume(
            [1, 1, 4],
            vec![
                NumericalVoxel::Valid(1.0),
                NumericalVoxel::Missing,
                NumericalVoxel::Invalid,
                NumericalVoxel::Valid(1.0),
            ],
        );
        let facts = NumericalConformanceOracle::new()
            .volume(
                &volume,
                query(
                    z_ray(4.0),
                    NumericalSampling::VoxelExact,
                    transfer([1.0, 0.0, 0.0], 1.0),
                    NumericalVolumeMode::Mip,
                    1.0,
                ),
            )
            .unwrap();
        assert!(facts.color().valid());
        assert!(!facts.color().covered());
        assert!(facts.encountered_missing());
        assert!(facts.encountered_invalid());
        let pick = facts.pick().unwrap();
        assert_eq!(pick.value(), 1.0);
        assert_eq!(
            pick.sample_ordinal(),
            0,
            "equal maxima keep the nearest sample"
        );
        assert_eq!(pick.world_position(), [0.0, 0.0, 3.0]);
        assert_eq!(pick.completeness(), NumericalPickCompleteness::Incomplete);
    }

    #[test]
    fn off_axis_dvr_uses_normalized_world_distance_for_extinction_and_pick() {
        let volume = identity_volume([9, 9, 1], vec![NumericalVoxel::Valid(1.0); 81]);
        let parameters = NumericalDvrParameters::new([0.0, 1.0], 1.0, 0.2).unwrap();
        let render = |direction| {
            NumericalConformanceOracle::new()
                .volume(
                    &volume,
                    query(
                        NumericalWorldRay::new([0.0, 0.0, 0.0], direction).unwrap(),
                        NumericalSampling::VoxelExact,
                        transfer([0.0, 1.0, 0.0], 1.0),
                        NumericalVolumeMode::Dvr(parameters),
                        0.5,
                    ),
                )
                .unwrap()
        };
        let scaled = render([3.0, 4.0, 0.0]);
        let unit = render([0.6, 0.8, 0.0]);
        let contract = NumericalConformanceContract::independent();
        assert!(contract.premultiplied_rgba_matches(
            scaled.color().premultiplied_rgba(),
            unit.color().premultiplied_rgba()
        ));
        let scaled_pick = scaled.pick().unwrap();
        let unit_pick = unit.pick().unwrap();
        assert!(contract.ray_distance_matches(
            scaled_pick.ray_distance_world(),
            unit_pick.ray_distance_world()
        ));
        assert!(
            contract
                .world_position_matches(scaled_pick.world_position(), unit_pick.world_position())
        );
        assert_eq!(scaled_pick.ray_distance_world(), 0.25);
        assert_eq!(scaled_pick.world_position(), [0.0, 0.0, 0.0]);
        assert!(scaled.color().premultiplied_rgba()[3] > 0.8);
    }

    #[test]
    fn iso_depth_and_first_threshold_pick_are_the_same_world_hit() {
        let volume = identity_volume(
            [1, 1, 3],
            vec![
                NumericalVoxel::Valid(0.2),
                NumericalVoxel::Valid(0.8),
                NumericalVoxel::Valid(0.1),
            ],
        );
        let parameters = NumericalIsoParameters::new(0.5, NumericalIsoShading::Flat).unwrap();
        let facts = NumericalConformanceOracle::new()
            .volume(
                &volume,
                query(
                    z_ray(3.0),
                    NumericalSampling::VoxelExact,
                    transfer([0.0, 0.0, 1.0], 1.0),
                    NumericalVolumeMode::Iso(parameters),
                    1.0,
                ),
            )
            .unwrap();
        let pick = facts.pick().unwrap();
        assert_eq!(facts.hit_depth_world(), Some(pick.ray_distance_world()));
        assert_eq!(pick.ray_distance_world(), 2.0);
        assert_eq!(pick.world_position(), [0.0, 0.0, 1.0]);
        assert_eq!(pick.value(), 0.8);
        assert_eq!(facts.color().rgba8(), [0, 0, 204, 255]);
    }

    #[test]
    fn smooth_pick_is_interpolated_and_reports_the_sample_world_point() {
        let volume = identity_volume(
            [1, 1, 2],
            vec![NumericalVoxel::Valid(0.0), NumericalVoxel::Valid(1.0)],
        );
        let facts = NumericalConformanceOracle::new()
            .volume(
                &volume,
                query(
                    NumericalWorldRay::new([0.0, 0.0, 1.5], [0.0, 0.0, -1.0]).unwrap(),
                    NumericalSampling::SmoothLinear,
                    transfer([1.0, 1.0, 1.0], 1.0),
                    NumericalVolumeMode::Mip,
                    0.5,
                ),
            )
            .unwrap();
        let pick = facts.pick().unwrap();
        assert_eq!(pick.kind(), NumericalPickKind::InterpolatedSample);
        assert_eq!(pick.world_position(), [0.0, 0.0, 1.25]);
        assert_eq!(pick.value(), 1.0);
    }

    #[test]
    fn iso_gradient_uses_inverse_transpose_and_preserves_missing_coverage() {
        let mut voxels = Vec::new();
        for z in 0..3 {
            for _y in 0..3 {
                for x in 0..3 {
                    voxels.push(NumericalVoxel::Valid(if z == 1 {
                        f64::from(x) / 2.0
                    } else {
                        0.0
                    }));
                }
            }
        }
        let volume = identity_volume([3, 3, 3], voxels);
        let parameters = NumericalIsoParameters::new(
            0.5,
            NumericalIsoShading::Gradient {
                light_direction_world: [1.0, 0.0, 0.0],
                ambient: 0.2,
            },
        )
        .unwrap();
        let facts = NumericalConformanceOracle::new()
            .volume(
                &volume,
                query(
                    NumericalWorldRay::new([1.0, 1.0, 3.0], [0.0, 0.0, -1.0]).unwrap(),
                    NumericalSampling::VoxelExact,
                    transfer([1.0, 0.0, 0.0], 1.0),
                    NumericalVolumeMode::Iso(parameters),
                    1.0,
                ),
            )
            .unwrap();
        assert_eq!(facts.color().rgba8(), [128, 0, 0, 255]);
        assert!(facts.color().covered());

        let mut missing = volume.clone();
        let index = 9 + 3 + 2;
        missing.voxels[index] = NumericalVoxel::Missing;
        let missing_facts = NumericalConformanceOracle::new()
            .volume(
                &missing,
                query(
                    NumericalWorldRay::new([1.0, 1.0, 3.0], [0.0, 0.0, -1.0]).unwrap(),
                    NumericalSampling::VoxelExact,
                    transfer([1.0, 0.0, 0.0], 1.0),
                    NumericalVolumeMode::Iso(parameters),
                    1.0,
                ),
            )
            .unwrap();
        assert!(!missing_facts.color().covered());
        assert_eq!(missing_facts.color().rgba8(), [26, 0, 0, 255]);
    }

    #[test]
    fn iso_layers_are_composited_by_world_depth_not_authored_order() {
        let iso = NumericalIsoParameters::new(0.0, NumericalIsoShading::Flat).unwrap();
        let volume_at = |z: f64| {
            NumericalVolume::new(
                [1, 1, 1],
                GridToWorld::from_row_major([
                    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, z, 0.0, 0.0, 0.0, 1.0,
                ])
                .unwrap(),
                vec![NumericalVoxel::Valid(1.0)],
            )
            .unwrap()
        };
        let far = volume_at(0.0);
        let near = volume_at(2.0);
        let ray = NumericalWorldRay::new([0.0, 0.0, 4.0], [0.0, 0.0, -1.0]).unwrap();
        let far_facts = NumericalConformanceOracle::new()
            .volume(
                &far,
                query(
                    ray,
                    NumericalSampling::VoxelExact,
                    transfer([0.0, 1.0, 0.0], 0.5),
                    NumericalVolumeMode::Iso(iso),
                    1.0,
                ),
            )
            .unwrap();
        let near_facts = NumericalConformanceOracle::new()
            .volume(
                &near,
                query(
                    ray,
                    NumericalSampling::VoxelExact,
                    transfer([1.0, 0.0, 0.0], 0.5),
                    NumericalVolumeMode::Iso(iso),
                    1.0,
                ),
            )
            .unwrap();
        let composite = NumericalConformanceOracle::new()
            .composite_iso_depth_ordered(&[far_facts, near_facts])
            .unwrap();
        assert_eq!(composite.source_order(), &[1, 0]);
        assert_eq!(composite.color().rgba8(), [128, 64, 0, 191]);
    }

    #[test]
    fn authored_order_and_floating_tolerance_boundaries_are_explicit() {
        let red = NumericalColorFacts::new([0.5, 0.0, 0.0, 0.5], true, true);
        let green = NumericalColorFacts::new([0.0, 0.5, 0.0, 0.5], true, true);
        let red_first = NumericalConformanceOracle::new()
            .composite_authored(&[red, green])
            .unwrap();
        let green_first = NumericalConformanceOracle::new()
            .composite_authored(&[green, red])
            .unwrap();
        assert_eq!(red_first.color().rgba8(), [128, 64, 0, 191]);
        assert_eq!(green_first.color().rgba8(), [64, 128, 0, 191]);

        let contract = NumericalConformanceContract::independent();
        assert!(contract.scalar_matches(65_535.0, 65_535.02));
        assert!(!contract.scalar_matches(65_535.0, 65_535.1));
        assert!(contract.ray_distance_matches(10.0, 10.0 + 1.0e-5));
        assert!(!contract.ray_distance_matches(10.0, 10.0 + 1.1e-5));
        assert_eq!(contract.rgba8_channel_tolerance(), 1);
        assert!(contract.rgba8_matches([1, 2, 3, 4], [1, 2, 3, 4]));
        assert!(contract.rgba8_matches([1, 2, 3, 4], [2, 1, 4, 5]));
        assert!(!contract.rgba8_matches([1, 2, 3, 4], [1, 2, 3, 6]));
    }
}

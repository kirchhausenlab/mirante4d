//! Backend-neutral render, progressive-frame, and presentation contracts.
//!
//! This crate describes what to render and how a rendered frame is identified
//! and presented. It owns no dataset payload, scheduler, GPU resource,
//! presentation backend, UI object, serialization, or I/O behavior.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, OnceLock},
};

use mirante4d_dataset::{BrickKey, CpuByteLease, DatasetCatalog, DatasetResourceIdentity};
use mirante4d_domain::{
    CameraView, CrossSectionView, GridToWorld, IsoLightState, LayerTransfer, RenderState, Shape3D,
    UnitQuaternion,
};
pub use mirante4d_domain::{
    IsoShadingPolicy, LogicalLayerKey, Projection, SamplingPolicy, ScaleLevel, TimeIndex,
    WorldPoint3,
};
use thiserror::Error;

pub const MAX_RENDER_LAYERS: usize = 64;
pub const MAX_RENDER_REQUIREMENTS: usize = 65_536;
pub const DEFAULT_LOGICAL_BRICK_SIDE: u64 = 64;
pub const MAX_EXACT_SHADER_GRID_END_EXCLUSIVE: u64 = 1 << 23;
pub const MAX_EXACT_SHADER_SAMPLE_COUNT: u64 = 1 << 23;
pub const DEFAULT_PRESENTATION_VIEWPORT: PresentationViewport =
    PresentationViewport::new_unchecked(512.0, 512.0);

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RenderApiError {
    #[error("presentation viewport dimensions must be finite and positive")]
    InvalidPresentationViewport,
    #[error("screen-point coordinates must be finite")]
    NonFiniteScreenPoint,
    #[error("render extent dimensions must be nonzero")]
    InvalidRenderExtent,
    #[error("render extent envelope dimensions must be nonzero")]
    InvalidRenderExtentEnvelope,
    #[error("render-pixel coordinates must be finite")]
    NonFiniteRenderPixel,
    #[error("volume-pick pixel lies outside its presented render extent")]
    PickPixelOutsideExtent,
    #[error("volume-pick result fields are inconsistent")]
    InvalidVolumePickResult,
    #[error("volume-pick ticket sequence must be nonzero")]
    InvalidVolumePickTicket,
    #[error("camera projection math produced a non-finite value")]
    CameraMathNotFinite,
    #[error("camera projection math produced a zero-length direction")]
    DegenerateViewDirection,
    #[error("a render intent must contain at least one visible layer")]
    EmptyRenderLayers,
    #[error("render intent contains {actual} layers, exceeding the limit of {maximum}")]
    TooManyRenderLayers { actual: usize, maximum: usize },
    #[error("logical layer key {ordinal} occurs more than once in one render intent")]
    DuplicateRenderLayer { ordinal: u32 },
    #[error("a render requirement set must not be empty")]
    EmptyRenderRequirements,
    #[error("render requirement set contains {actual} entries, exceeding the limit of {maximum}")]
    TooManyRenderRequirements { actual: usize, maximum: usize },
    #[error("one dataset resource occurs more than once in a render requirement set")]
    DuplicateRenderRequirement,
    #[error("render resource-grid metadata is duplicated, missing, or incompatible with its keys")]
    InvalidRenderResourceGrid,
    #[error("render layer scale chains are missing, duplicated, unordered, or incompatible")]
    InvalidRenderScaleChain,
    #[error("requirement layer {ordinal} is absent from the render intent")]
    RequirementLayerNotInIntent { ordinal: u32 },
    #[error("requirement timepoint {actual} differs from render-intent timepoint {expected}")]
    RequirementTimepointMismatch { expected: u64, actual: u64 },
    #[error("requirement dataset/source identity differs from the render intent")]
    RequirementIdentityMismatch,
    #[error("a render requirement set must contain at least one first-useful-frame resource")]
    MissingFirstUsefulRequirement,
    #[error("prepared render requirement accounting charge is smaller than its host body")]
    PreparedRequirementChargeTooSmall,
    #[error("prepared render requirement accounting charge was already attached")]
    PreparedRequirementChargeAlreadyAttached,
    #[error("prepared render requirement host allocation size overflowed")]
    PreparedRequirementHostAllocationOverflow,
    #[error("one covered dataset resource occurs more than once")]
    DuplicateCoveredResource,
    #[error("frame coverage contains {actual} entries, exceeding its {maximum} requirements")]
    TooManyCoveredResources { actual: usize, maximum: usize },
    #[error("a covered dataset resource does not belong to the frame requirements")]
    CoveredResourceNotRequired,
    #[error("frame completeness, coverage, and limitation are inconsistent")]
    InvalidFrameProgress,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ShaderControlAffineError {
    #[error("grid-to-world transform cannot be inverted for shader controls")]
    NonInvertible,
    #[error("world-to-grid transform cannot be represented by finite binary32 shader controls")]
    NotRepresentable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis3 {
    X,
    Y,
    Z,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AffineColumn {
    X,
    Y,
    Z,
    Translation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AffineValidationStage {
    Input,
    Quantized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderControlField {
    WorldToGrid { row: Axis3, column: AffineColumn },
    PlaneCenter(Axis3),
    PlaneRight(Axis3),
    PlaneUp(Axis3),
    PlaneWorldUnitsPerLogicalPoint,
    PlaneScreenSpanX,
    PlaneScreenSpanY,
    VolumeRayOriginBase(Axis3),
    VolumeRayOriginStepX(Axis3),
    VolumeRayOriginStepY(Axis3),
    VolumeRayDirectionBase(Axis3),
    VolumeRayDirectionStepX(Axis3),
    VolumeRayDirectionStepY(Axis3),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderEnvelopeStage {
    PlanePixelCenter,
    PlaneWorldPoint,
    PlaneGridAddress,
    VolumeRayConstruction,
    VolumeDirectionNormalization,
    VolumeWorldToGrid,
    VolumeSlabIntersection,
    VolumePageExit,
    VolumeSamplePoint,
    GeneralDvrInterval,
    SubnormalDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderEnvelopeAxis {
    Axis(Axis3),
    All,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShaderEnvelopeFailure {
    NonFiniteBound,
    ZeroOrIndeterminateDirectionNorm,
    DivisionDenominatorNotProvablyNormal,
    ReachableQuotientNotProvablyFinite,
    ErrorBudget { upper_voxels: f64 },
    AllDirectionsStationary,
}

#[derive(Debug, Error, Clone, Copy, PartialEq)]
pub enum ShaderAdmissionError {
    #[error("grid-to-world affine is exactly singular")]
    SingularAffine,
    #[error("{stage:?} affine condition bound {condition_upper} exceeds the validator envelope")]
    AffineConditionExceeded {
        stage: AffineValidationStage,
        condition_upper: f64,
    },
    #[error("shader control {field:?} is not finite in binary32")]
    ShaderControlNotFinite { field: ShaderControlField },
    #[error("the quantized world-to-grid affine is exactly singular")]
    ShaderQuantizedAffineSingular,
    #[error("shader coordinate precision exceeds half a voxel on {axis:?}: {error_voxels}")]
    ShaderCoordinatePrecisionExceeded { axis: Axis3, error_voxels: f64 },
    #[error("shader coordinate envelope failed at {stage:?} for {axis:?}: {reason:?}")]
    ShaderCoordinateEnvelopeExceeded {
        stage: ShaderEnvelopeStage,
        axis: ShaderEnvelopeAxis,
        reason: ShaderEnvelopeFailure,
    },
    #[error("shader sample count {bound} for layer {layer:?} exceeds {maximum}")]
    ShaderSampleCountExceeded {
        layer: LogicalLayerKey,
        bound: u64,
        maximum: u64,
    },
}

/// The single validated affine representation consumed by shader planning and
/// control construction. Its rows are the exact binary32 values uploaded to
/// WGSL; inverse intervals and coordinate-error facts describe those rows over
/// the complete declared half-voxel grid domain.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValidatedShaderAffine {
    grid_to_world: GridToWorld,
    grid_shape: Shape3D,
    world_to_grid_rows: [[f32; 4]; 3],
    grid_to_world_rows: [[f32; 4]; 3],
    quantized_inverse_center: [[f64; 4]; 3],
    quantized_inverse_radius: [[f64; 4]; 3],
    condition_inf_upper: f64,
    maximum_grid_error: [f64; 3],
}

impl ValidatedShaderAffine {
    pub fn new(
        grid_to_world: GridToWorld,
        grid_shape: Shape3D,
    ) -> Result<Self, ShaderAdmissionError> {
        for (axis, end) in [Axis3::X, Axis3::Y, Axis3::Z].into_iter().zip([
            grid_shape.x(),
            grid_shape.y(),
            grid_shape.z(),
        ]) {
            if end > MAX_EXACT_SHADER_GRID_END_EXCLUSIVE {
                return Err(ShaderAdmissionError::ShaderCoordinateEnvelopeExceeded {
                    stage: ShaderEnvelopeStage::VolumeWorldToGrid,
                    axis: ShaderEnvelopeAxis::Axis(axis),
                    reason: ShaderEnvelopeFailure::ErrorBudget { upper_voxels: 0.5 },
                });
            }
        }

        let matrix = grid_to_world.row_major();
        let linear = [
            [matrix[0], matrix[1], matrix[2]],
            [matrix[4], matrix[5], matrix[6]],
            [matrix[8], matrix[9], matrix[10]],
        ];
        if exact_determinant_is_zero(linear) {
            return Err(ShaderAdmissionError::SingularAffine);
        }
        let input_inverse = verified_inverse(linear, AffineValidationStage::Input)?;
        let translation = [matrix[3], matrix[7], matrix[11]];
        let inverse_translation =
            matrix_vector(input_inverse.center, translation).map(|value| -value);
        let source_rows = [
            [
                input_inverse.center[0][0],
                input_inverse.center[0][1],
                input_inverse.center[0][2],
                inverse_translation[0],
            ],
            [
                input_inverse.center[1][0],
                input_inverse.center[1][1],
                input_inverse.center[1][2],
                inverse_translation[1],
            ],
            [
                input_inverse.center[2][0],
                input_inverse.center[2][1],
                input_inverse.center[2][2],
                inverse_translation[2],
            ],
        ];
        let mut world_to_grid_rows = [[0.0_f32; 4]; 3];
        for row in 0..3 {
            for column in 0..4 {
                let converted = source_rows[row][column] as f32;
                if !converted.is_finite() {
                    return Err(ShaderAdmissionError::ShaderControlNotFinite {
                        field: ShaderControlField::WorldToGrid {
                            row: axis_for_index(row),
                            column: affine_column_for_index(column),
                        },
                    });
                }
                world_to_grid_rows[row][column] = canonical_f32_zero(converted);
            }
        }

        let quantized_linear =
            world_to_grid_rows.map(|row| [f64::from(row[0]), f64::from(row[1]), f64::from(row[2])]);
        if exact_determinant_is_zero(quantized_linear) {
            return Err(ShaderAdmissionError::ShaderQuantizedAffineSingular);
        }
        let quantized_inverse =
            verified_inverse(quantized_linear, AffineValidationStage::Quantized)?;
        let quantized_translation = world_to_grid_rows.map(|row| f64::from(row[3]));
        let inverse_translation_center =
            matrix_vector(quantized_inverse.center, quantized_translation).map(|value| -value);
        let translation_input_norm = quantized_translation
            .into_iter()
            .map(f64::abs)
            .fold(0.0, f64::max);
        let translation_radius: [f64; 3] = std::array::from_fn(|row| {
            // The inverse-translation dot product may cancel almost
            // completely. Bound its floating evaluation from the sum of the
            // absolute products, never from the (possibly tiny) result.
            let product_sum = (0..3)
                .map(|column| {
                    (quantized_inverse.center[row][column] * quantized_translation[column]).abs()
                })
                .sum::<f64>();
            let operation_count = 5.0;
            let gamma = operation_count * f64::EPSILON / (1.0 - operation_count * f64::EPSILON);
            next_up(
                3.0 * quantized_inverse.radius * translation_input_norm
                    + gamma * product_sum
                    + operation_count * f64::from_bits(1),
            )
        });
        let mut quantized_inverse_center = [[0.0; 4]; 3];
        let mut quantized_inverse_radius = [[0.0; 4]; 3];
        let mut grid_to_world_rows = [[0.0_f32; 4]; 3];
        for row in 0..3 {
            let center = [
                quantized_inverse.center[row][0],
                quantized_inverse.center[row][1],
                quantized_inverse.center[row][2],
                inverse_translation_center[row],
            ];
            let mut radius = [
                quantized_inverse.radius,
                quantized_inverse.radius,
                quantized_inverse.radius,
                translation_radius[row],
            ];
            for column in 0..4 {
                let quantized = canonical_f32_zero(center[column] as f32);
                if !quantized.is_finite() {
                    return Err(shader_envelope_error(
                        ShaderEnvelopeStage::VolumeWorldToGrid,
                        ShaderEnvelopeAxis::Axis(axis_for_index(row)),
                        ShaderEnvelopeFailure::NonFiniteBound,
                    ));
                }
                grid_to_world_rows[row][column] = quantized;
                let quantized_center = f64::from(quantized);
                radius[column] =
                    next_up(radius[column] + (center[column] - quantized_center).abs());
                quantized_inverse_center[row][column] = quantized_center;
            }
            quantized_inverse_radius[row] = radius;
        }

        let maximum_grid_error = maximum_shader_grid_error(matrix, grid_shape, world_to_grid_rows);
        for (axis, error_voxels) in [Axis3::X, Axis3::Y, Axis3::Z]
            .into_iter()
            .zip(maximum_grid_error)
        {
            if !error_voxels.is_finite() {
                return Err(ShaderAdmissionError::ShaderCoordinatePrecisionExceeded {
                    axis,
                    error_voxels: f64::INFINITY,
                });
            }
            if error_voxels >= 0.5 {
                return Err(ShaderAdmissionError::ShaderCoordinatePrecisionExceeded {
                    axis,
                    error_voxels,
                });
            }
        }

        Ok(Self {
            grid_to_world,
            grid_shape,
            world_to_grid_rows,
            grid_to_world_rows,
            quantized_inverse_center,
            quantized_inverse_radius,
            condition_inf_upper: input_inverse.condition_inf_upper,
            maximum_grid_error,
        })
    }

    pub const fn grid_to_world(&self) -> GridToWorld {
        self.grid_to_world
    }

    pub const fn grid_shape(&self) -> Shape3D {
        self.grid_shape
    }

    pub const fn world_to_grid_rows(&self) -> [[f32; 4]; 3] {
        self.world_to_grid_rows
    }

    pub const fn grid_to_world_rows(&self) -> [[f32; 4]; 3] {
        self.grid_to_world_rows
    }

    pub const fn quantized_inverse_center(&self) -> [[f64; 4]; 3] {
        self.quantized_inverse_center
    }

    pub const fn quantized_inverse_radius(&self) -> [[f64; 4]; 3] {
        self.quantized_inverse_radius
    }

    pub const fn condition_inf_upper(&self) -> f64 {
        self.condition_inf_upper
    }

    pub const fn maximum_grid_error(&self) -> [f64; 3] {
        self.maximum_grid_error
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderRenderMode {
    MaximumIntensity,
    DirectVolume,
    IsoSurface,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShaderLayerSchedule {
    layer: LogicalLayerKey,
    mode: ShaderRenderMode,
    sampling: SamplingPolicy,
}

impl ShaderLayerSchedule {
    pub const fn layer(self) -> LogicalLayerKey {
        self.layer
    }

    pub const fn mode(self) -> ShaderRenderMode {
        self.mode
    }

    pub const fn sampling(self) -> SamplingPolicy {
        self.sampling
    }
}

/// One catalog layer/scale affine captured by a target work proof.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShaderLayerAffine {
    layer: LogicalLayerKey,
    scale: ScaleLevel,
    affine: ValidatedShaderAffine,
}

impl ShaderLayerAffine {
    pub fn new(
        layer: LogicalLayerKey,
        scale: ScaleLevel,
        grid_to_world: GridToWorld,
        grid_shape: Shape3D,
    ) -> Result<Self, ShaderAdmissionError> {
        Ok(Self {
            layer,
            scale,
            affine: ValidatedShaderAffine::new(grid_to_world, grid_shape)?,
        })
    }

    pub const fn layer(self) -> LogicalLayerKey {
        self.layer
    }

    pub const fn scale(self) -> ScaleLevel {
        self.scale
    }

    pub const fn affine(self) -> ValidatedShaderAffine {
        self.affine
    }
}

/// Exact binary32 controls uploaded for a cross-section shader.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaneShaderControls {
    center_world: [f32; 3],
    right_world: [f32; 3],
    up_world: [f32; 3],
    world_units_per_logical_point: f32,
    screen_span: [f32; 2],
}

impl PlaneShaderControls {
    pub const fn center_world(self) -> [f32; 3] {
        self.center_world
    }

    pub const fn right_world(self) -> [f32; 3] {
        self.right_world
    }

    pub const fn up_world(self) -> [f32; 3] {
        self.up_world
    }

    pub const fn world_units_per_logical_point(self) -> f32 {
        self.world_units_per_logical_point
    }

    pub const fn screen_span(self) -> [f32; 2] {
        self.screen_span
    }
}

/// Exact affine binary32 controls uploaded for volume-ray construction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolumeRayShaderControls {
    origin_base: [f32; 3],
    origin_step_x: [f32; 3],
    origin_step_y: [f32; 3],
    direction_base: [f32; 3],
    direction_step_x: [f32; 3],
    direction_step_y: [f32; 3],
}

impl VolumeRayShaderControls {
    pub const fn origin_base(self) -> [f32; 3] {
        self.origin_base
    }

    pub const fn origin_step_x(self) -> [f32; 3] {
        self.origin_step_x
    }

    pub const fn origin_step_y(self) -> [f32; 3] {
        self.origin_step_y
    }

    pub const fn direction_base(self) -> [f32; 3] {
        self.direction_base
    }

    pub const fn direction_step_x(self) -> [f32; 3] {
        self.direction_step_x
    }

    pub const fn direction_step_y(self) -> [f32; 3] {
        self.direction_step_y
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShaderLayerWorkFacts {
    layer: LogicalLayerKey,
    scale: ScaleLevel,
    grid_error_upper: [f64; 3],
    address_range: [[f64; 2]; 3],
    sample_count_upper: u64,
}

impl ShaderLayerWorkFacts {
    pub const fn layer(self) -> LogicalLayerKey {
        self.layer
    }

    pub const fn scale(self) -> ScaleLevel {
        self.scale
    }

    pub const fn grid_error_upper(self) -> [f64; 3] {
        self.grid_error_upper
    }

    pub const fn address_range(self) -> [[f64; 2]; 3] {
        self.address_range
    }

    pub const fn sample_count_upper(self) -> u64 {
        self.sample_count_upper
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedPlaneWorkEnvelope {
    view: CrossSectionView,
    presentation: PresentationViewport,
    extent: RenderExtent,
    controls: PlaneShaderControls,
    schedules: Box<[ShaderLayerSchedule]>,
    affines: Box<[ShaderLayerAffine]>,
    facts: Box<[ShaderLayerWorkFacts]>,
}

/// Canonical finite physical-pixel footprint for one Plane affine. Semantic
/// demand uses this exact result for coverage and guard containment; the
/// target envelope and WGPU encoder use the same controls and affine object.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValidatedPlaneGridFootprint {
    controls: PlaneShaderControls,
    affine: ValidatedShaderAffine,
    origin_grid: [f64; 3],
    step_x_grid: [f64; 3],
    step_y_grid: [f64; 3],
    grid_error_upper: [f64; 3],
    world_to_grid_rows: [[f64; 4]; 3],
    width_pixels: u32,
    height_pixels: u32,
}

impl ValidatedPlaneGridFootprint {
    pub fn new(
        view: CrossSectionView,
        presentation: PresentationViewport,
        extent: RenderExtent,
        grid_to_world: GridToWorld,
        grid_shape: Shape3D,
    ) -> Result<Self, ShaderAdmissionError> {
        validate_shader_extent(extent)?;
        let controls = plane_shader_controls(view, presentation)?;
        let affine = ValidatedShaderAffine::new(grid_to_world, grid_shape)?;
        let geometry = plane_layer_geometry(affine, controls, extent)?;
        Ok(Self {
            controls,
            affine,
            origin_grid: geometry.origin_grid,
            step_x_grid: geometry.step_x_grid,
            step_y_grid: geometry.step_y_grid,
            grid_error_upper: geometry.grid_error_upper,
            world_to_grid_rows: affine.world_to_grid_rows().map(|row| row.map(f64::from)),
            width_pixels: extent.width_pixels(),
            height_pixels: extent.height_pixels(),
        })
    }

    pub const fn controls(self) -> PlaneShaderControls {
        self.controls
    }

    pub const fn affine(self) -> ValidatedShaderAffine {
        self.affine
    }

    pub const fn origin_grid(self) -> [f64; 3] {
        self.origin_grid
    }

    pub const fn step_x_grid(self) -> [f64; 3] {
        self.step_x_grid
    }

    pub const fn step_y_grid(self) -> [f64; 3] {
        self.step_y_grid
    }

    pub const fn grid_error_upper(self) -> [f64; 3] {
        self.grid_error_upper
    }

    pub const fn world_to_grid_rows(self) -> [[f64; 4]; 3] {
        self.world_to_grid_rows
    }

    pub const fn width_pixels(self) -> u32 {
        self.width_pixels
    }

    pub const fn height_pixels(self) -> u32 {
        self.height_pixels
    }
}

impl ValidatedPlaneWorkEnvelope {
    pub const fn controls(&self) -> PlaneShaderControls {
        self.controls
    }

    pub fn affines(&self) -> &[ShaderLayerAffine] {
        &self.affines
    }

    pub fn facts(&self) -> &[ShaderLayerWorkFacts] {
        &self.facts
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VolumeRayWorkEnvelope {
    camera: CameraView,
    iso_light: IsoLightState,
    presentation: PresentationViewport,
    extent: RenderExtent,
    controls: VolumeRayShaderControls,
    schedules: Box<[ShaderLayerSchedule]>,
    affines: Box<[ShaderLayerAffine]>,
    facts: Box<[ShaderLayerWorkFacts]>,
    general_dvr_sample_count_upper: Option<u64>,
}

impl VolumeRayWorkEnvelope {
    pub const fn controls(&self) -> VolumeRayShaderControls {
        self.controls
    }

    pub fn affines(&self) -> &[ShaderLayerAffine] {
        &self.affines
    }

    pub fn facts(&self) -> &[ShaderLayerWorkFacts] {
        &self.facts
    }

    pub const fn general_dvr_sample_count_upper(&self) -> Option<u64> {
        self.general_dvr_sample_count_upper
    }
}

/// Canonical target-specific proof consumed unchanged by semantic demand and
/// the renderer control encoder.
#[derive(Debug, Clone, PartialEq)]
pub enum ShaderWorkEnvelope {
    Plane(ValidatedPlaneWorkEnvelope),
    Volume(VolumeRayWorkEnvelope),
}

/// One shareable shader proof whose CPU-ledger reservation has the same
/// lifetime as every render-intent clone that can retain the allocation.
pub struct SharedShaderWorkEnvelope {
    envelope: ShaderWorkEnvelope,
    _charge: Option<Arc<dyn CpuByteLease>>,
}

impl std::fmt::Debug for SharedShaderWorkEnvelope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SharedShaderWorkEnvelope")
            .field("envelope", &self.envelope)
            .field("accounted", &self._charge.is_some())
            .finish()
    }
}

impl SharedShaderWorkEnvelope {
    pub fn accounted(envelope: ShaderWorkEnvelope, charge: Arc<dyn CpuByteLease>) -> Self {
        Self {
            envelope,
            _charge: Some(charge),
        }
    }

    fn unaccounted(envelope: ShaderWorkEnvelope) -> Self {
        Self {
            envelope,
            _charge: None,
        }
    }

    pub const fn envelope(&self) -> &ShaderWorkEnvelope {
        &self.envelope
    }
}

impl ShaderWorkEnvelope {
    pub fn for_intent(
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
    ) -> Result<Self, ShaderAdmissionError> {
        let mut affines = Vec::new();
        for layer in intent.layers() {
            let chain = requirements.scale_chain(layer.layer()).ok_or_else(|| {
                shader_envelope_error(
                    ShaderEnvelopeStage::VolumeWorldToGrid,
                    ShaderEnvelopeAxis::All,
                    ShaderEnvelopeFailure::NonFiniteBound,
                )
            })?;
            let catalog_layer = catalog.layer(layer.layer()).ok_or_else(|| {
                shader_envelope_error(
                    ShaderEnvelopeStage::VolumeWorldToGrid,
                    ShaderEnvelopeAxis::All,
                    ShaderEnvelopeFailure::NonFiniteBound,
                )
            })?;
            for scale in chain.scales().iter().copied() {
                let catalog_scale = catalog_layer.scale(scale).ok_or_else(|| {
                    shader_envelope_error(
                        ShaderEnvelopeStage::VolumeWorldToGrid,
                        ShaderEnvelopeAxis::All,
                        ShaderEnvelopeFailure::NonFiniteBound,
                    )
                })?;
                affines.push(ShaderLayerAffine::new(
                    layer.layer(),
                    scale,
                    catalog_scale.grid_to_world(),
                    catalog_scale.shape(),
                )?);
            }
        }
        Self::for_intent_with_validated_affines(catalog, intent, requirements, affines)
    }

    /// Constructs one target envelope from dataset-generation cached affine
    /// results. The supplied list must contain exactly the participating
    /// layer/scale entries in intent/scale-chain order; every identity and
    /// durable catalog transform is revalidated before a cached object can be
    /// consumed.
    pub fn for_intent_with_validated_affines(
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        affines: Vec<ShaderLayerAffine>,
    ) -> Result<Self, ShaderAdmissionError> {
        validate_shader_extent(intent.extent())?;
        let schedules = intent
            .layers()
            .iter()
            .map(shader_layer_schedule)
            .collect::<Result<Vec<_>, _>>()?;
        validate_cached_shader_affines(catalog, intent, requirements, &affines)?;
        match intent.view() {
            RenderViewIntent::CrossSection(view) => {
                let controls = plane_shader_controls(view, intent.presentation())?;
                let facts = affines
                    .iter()
                    .copied()
                    .map(|layer| plane_layer_facts(layer, controls, intent.extent()))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Self::Plane(ValidatedPlaneWorkEnvelope {
                    view,
                    presentation: intent.presentation(),
                    extent: intent.extent(),
                    controls,
                    schedules: schedules.into_boxed_slice(),
                    affines: affines.into_boxed_slice(),
                    facts: facts.into_boxed_slice(),
                }))
            }
            RenderViewIntent::Volume { camera, iso_light } => {
                let controls =
                    volume_ray_shader_controls(camera, intent.presentation(), intent.extent())?;
                let facts = affines
                    .iter()
                    .copied()
                    .map(|layer| volume_layer_facts(layer, controls, intent.extent()))
                    .collect::<Result<Vec<_>, _>>()?;
                let general_dvr_sample_count_upper = general_dvr_required(&schedules, &affines)
                    .then(|| general_dvr_count_bound(&affines))
                    .transpose()?;
                Ok(Self::Volume(VolumeRayWorkEnvelope {
                    camera,
                    iso_light,
                    presentation: intent.presentation(),
                    extent: intent.extent(),
                    controls,
                    schedules: schedules.into_boxed_slice(),
                    affines: affines.into_boxed_slice(),
                    facts: facts.into_boxed_slice(),
                    general_dvr_sample_count_upper,
                }))
            }
        }
    }

    pub const fn pass_kind(&self) -> RenderPassKind {
        match self {
            Self::Plane(_) => RenderPassKind::Plane,
            Self::Volume(_) => RenderPassKind::Volume,
        }
    }

    /// Conservative owned-byte charge for a latest-only cached proof. This
    /// excludes the `Arc` control block retained by its owner and includes all
    /// boxed schedule, affine, and per-layer fact arrays.
    pub fn owned_allocation_bytes(&self) -> u64 {
        let (schedules, affines, facts) = match self {
            Self::Plane(envelope) => (
                envelope.schedules.len(),
                envelope.affines.len(),
                envelope.facts.len(),
            ),
            Self::Volume(envelope) => (
                envelope.schedules.len(),
                envelope.affines.len(),
                envelope.facts.len(),
            ),
        };
        (std::mem::size_of::<Self>() as u64)
            .saturating_add(
                (schedules as u64)
                    .saturating_mul(std::mem::size_of::<ShaderLayerSchedule>() as u64),
            )
            .saturating_add(
                (affines as u64).saturating_mul(std::mem::size_of::<ShaderLayerAffine>() as u64),
            )
            .saturating_add(
                (facts as u64).saturating_mul(std::mem::size_of::<ShaderLayerWorkFacts>() as u64),
            )
            .max(1)
    }

    pub fn affine(
        &self,
        layer: LogicalLayerKey,
        scale: ScaleLevel,
    ) -> Option<ValidatedShaderAffine> {
        let affines = match self {
            Self::Plane(envelope) => envelope.affines(),
            Self::Volume(envelope) => envelope.affines(),
        };
        affines
            .iter()
            .find(|candidate| candidate.layer == layer && candidate.scale == scale)
            .map(|candidate| candidate.affine)
    }

    pub fn matches(
        &self,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
    ) -> bool {
        let (view_matches, presentation, extent, schedules, affines) = match self {
            Self::Plane(envelope) => (
                intent.view() == RenderViewIntent::CrossSection(envelope.view),
                envelope.presentation,
                envelope.extent,
                envelope.schedules.as_ref(),
                envelope.affines.as_ref(),
            ),
            Self::Volume(envelope) => (
                intent.view()
                    == RenderViewIntent::Volume {
                        camera: envelope.camera,
                        iso_light: envelope.iso_light,
                    },
                envelope.presentation,
                envelope.extent,
                envelope.schedules.as_ref(),
                envelope.affines.as_ref(),
            ),
        };
        if !view_matches
            || intent.view().pass_kind() != self.pass_kind()
            || intent.presentation() != presentation
            || intent.extent() != extent
            || schedules.len() != intent.layers().len()
            || schedules
                .iter()
                .zip(intent.layers())
                .any(|(expected, actual)| shader_layer_schedule(actual).ok() != Some(*expected))
        {
            return false;
        }
        let mut index = 0;
        for layer in intent.layers() {
            let Some(chain) = requirements.scale_chain(layer.layer()) else {
                return false;
            };
            let Some(catalog_layer) = catalog.layer(layer.layer()) else {
                return false;
            };
            for scale in chain.scales().iter().copied() {
                let Some(expected) = affines.get(index) else {
                    return false;
                };
                let Some(catalog_scale) = catalog_layer.scale(scale) else {
                    return false;
                };
                if expected.layer != layer.layer()
                    || expected.scale != scale
                    || expected.affine.grid_to_world() != catalog_scale.grid_to_world()
                    || expected.affine.grid_shape() != catalog_scale.shape()
                {
                    return false;
                }
                index += 1;
            }
        }
        index == affines.len()
    }

    pub const fn plane(&self) -> Option<&ValidatedPlaneWorkEnvelope> {
        match self {
            Self::Plane(envelope) => Some(envelope),
            Self::Volume(_) => None,
        }
    }

    pub const fn volume(&self) -> Option<&VolumeRayWorkEnvelope> {
        match self {
            Self::Volume(envelope) => Some(envelope),
            Self::Plane(_) => None,
        }
    }
}

fn validate_cached_shader_affines(
    catalog: &DatasetCatalog,
    intent: &RenderIntent,
    requirements: &RenderRequirements,
    affines: &[ShaderLayerAffine],
) -> Result<(), ShaderAdmissionError> {
    let mismatch = || {
        shader_envelope_error(
            ShaderEnvelopeStage::VolumeWorldToGrid,
            ShaderEnvelopeAxis::All,
            ShaderEnvelopeFailure::NonFiniteBound,
        )
    };
    let mut index = 0;
    for layer in intent.layers() {
        let chain = requirements
            .scale_chain(layer.layer())
            .ok_or_else(mismatch)?;
        let catalog_layer = catalog.layer(layer.layer()).ok_or_else(mismatch)?;
        for scale in chain.scales().iter().copied() {
            let catalog_scale = catalog_layer.scale(scale).ok_or_else(mismatch)?;
            let supplied = affines.get(index).ok_or_else(mismatch)?;
            if supplied.layer != layer.layer()
                || supplied.scale != scale
                || supplied.affine.grid_to_world() != catalog_scale.grid_to_world()
                || supplied.affine.grid_shape() != catalog_scale.shape()
            {
                return Err(mismatch());
            }
            index += 1;
        }
    }
    if index != affines.len() {
        return Err(mismatch());
    }
    Ok(())
}

fn shader_layer_schedule(
    layer: &LayerRenderIntent,
) -> Result<ShaderLayerSchedule, ShaderAdmissionError> {
    let state = layer.render_state();
    let mode = if state.mip_parameters().is_some() {
        ShaderRenderMode::MaximumIntensity
    } else if state.dvr_parameters().is_some() {
        ShaderRenderMode::DirectVolume
    } else if state.iso_parameters().is_some() {
        ShaderRenderMode::IsoSurface
    } else {
        return Err(shader_envelope_error(
            ShaderEnvelopeStage::VolumeSamplePoint,
            ShaderEnvelopeAxis::All,
            ShaderEnvelopeFailure::NonFiniteBound,
        ));
    };
    Ok(ShaderLayerSchedule {
        layer: layer.layer(),
        mode,
        sampling: state.sampling_policy(),
    })
}

fn validate_shader_extent(extent: RenderExtent) -> Result<(), ShaderAdmissionError> {
    for (axis, dimension) in [Axis3::X, Axis3::Y]
        .into_iter()
        .zip([extent.width_pixels(), extent.height_pixels()])
    {
        if u64::from(dimension) > MAX_EXACT_SHADER_GRID_END_EXCLUSIVE {
            return Err(shader_envelope_error(
                ShaderEnvelopeStage::PlanePixelCenter,
                ShaderEnvelopeAxis::Axis(axis),
                ShaderEnvelopeFailure::ErrorBudget { upper_voxels: 0.5 },
            ));
        }
    }
    Ok(())
}

const fn shader_envelope_error(
    stage: ShaderEnvelopeStage,
    axis: ShaderEnvelopeAxis,
    reason: ShaderEnvelopeFailure,
) -> ShaderAdmissionError {
    ShaderAdmissionError::ShaderCoordinateEnvelopeExceeded {
        stage,
        axis,
        reason,
    }
}

fn quantized_shader_control(
    value: f64,
    field: ShaderControlField,
) -> Result<f32, ShaderAdmissionError> {
    let value = canonical_f32_zero(value as f32);
    value
        .is_finite()
        .then_some(value)
        .ok_or(ShaderAdmissionError::ShaderControlNotFinite { field })
}

fn quantized_shader_vec3(
    values: [f64; 3],
    field: impl Fn(Axis3) -> ShaderControlField,
) -> Result<[f32; 3], ShaderAdmissionError> {
    let mut result = [0.0; 3];
    for axis in 0..3 {
        result[axis] = quantized_shader_control(values[axis], field(axis_for_index(axis)))?;
    }
    Ok(result)
}

fn plane_shader_controls(
    view: CrossSectionView,
    presentation: PresentationViewport,
) -> Result<PlaneShaderControls, ShaderAdmissionError> {
    let axes = axes_from_orientation(view.orientation()).map_err(|_| {
        shader_envelope_error(
            ShaderEnvelopeStage::PlaneWorldPoint,
            ShaderEnvelopeAxis::All,
            ShaderEnvelopeFailure::NonFiniteBound,
        )
    })?;
    Ok(PlaneShaderControls {
        center_world: quantized_shader_vec3(view.center_world().components(), |axis| {
            ShaderControlField::PlaneCenter(axis)
        })?,
        right_world: quantized_shader_vec3(axes.right(), |axis| {
            ShaderControlField::PlaneRight(axis)
        })?,
        up_world: quantized_shader_vec3(axes.up(), ShaderControlField::PlaneUp)?,
        world_units_per_logical_point: quantized_shader_control(
            view.scale_world_per_screen_point(),
            ShaderControlField::PlaneWorldUnitsPerLogicalPoint,
        )?,
        screen_span: [
            quantized_shader_control(
                presentation.width_points(),
                ShaderControlField::PlaneScreenSpanX,
            )?,
            quantized_shader_control(
                presentation.height_points(),
                ShaderControlField::PlaneScreenSpanY,
            )?,
        ],
    })
}

fn volume_ray_shader_controls(
    camera: CameraView,
    presentation: PresentationViewport,
    extent: RenderExtent,
) -> Result<VolumeRayShaderControls, ShaderAdmissionError> {
    let frame = CameraFrame::new(camera, presentation).map_err(|_| {
        shader_envelope_error(
            ShaderEnvelopeStage::VolumeRayConstruction,
            ShaderEnvelopeAxis::All,
            ShaderEnvelopeFailure::NonFiniteBound,
        )
    })?;
    let axes = frame.axes();
    let width = f64::from(extent.width_pixels());
    let height = f64::from(extent.height_pixels());
    let screen_x = (0.5 / width - 0.5) * presentation.width_points();
    let screen_y = (0.5 - 0.5 / height) * presentation.height_points();
    let screen_dx = presentation.width_points() / width;
    let screen_dy = -presentation.height_points() / height;
    let forward = axes.forward();
    let right = axes.right();
    let up = axes.up();
    let (origin, origin_step_x, origin_step_y, direction, direction_step_x, direction_step_y) =
        match camera.projection() {
            Projection::Orthographic => {
                let scale = camera.orthographic_world_per_screen_point();
                let eye = frame.eye().components();
                (
                    std::array::from_fn(|axis| {
                        eye[axis] + right[axis] * screen_x * scale + up[axis] * screen_y * scale
                    }),
                    right.map(|value| value * screen_dx * scale),
                    up.map(|value| value * screen_dy * scale),
                    forward,
                    [0.0; 3],
                    [0.0; 3],
                )
            }
            Projection::Perspective => {
                let inverse_focal = camera.perspective_focal_length_screen_points().recip();
                (
                    frame.eye().components(),
                    [0.0; 3],
                    [0.0; 3],
                    std::array::from_fn(|axis| {
                        forward[axis]
                            + right[axis] * screen_x * inverse_focal
                            + up[axis] * screen_y * inverse_focal
                    }),
                    right.map(|value| value * screen_dx * inverse_focal),
                    up.map(|value| value * screen_dy * inverse_focal),
                )
            }
        };
    Ok(VolumeRayShaderControls {
        origin_base: quantized_shader_vec3(origin, ShaderControlField::VolumeRayOriginBase)?,
        origin_step_x: quantized_shader_vec3(
            origin_step_x,
            ShaderControlField::VolumeRayOriginStepX,
        )?,
        origin_step_y: quantized_shader_vec3(
            origin_step_y,
            ShaderControlField::VolumeRayOriginStepY,
        )?,
        direction_base: quantized_shader_vec3(
            direction,
            ShaderControlField::VolumeRayDirectionBase,
        )?,
        direction_step_x: quantized_shader_vec3(
            direction_step_x,
            ShaderControlField::VolumeRayDirectionStepX,
        )?,
        direction_step_y: quantized_shader_vec3(
            direction_step_y,
            ShaderControlField::VolumeRayDirectionStepY,
        )?,
    })
}

fn plane_layer_facts(
    layer: ShaderLayerAffine,
    controls: PlaneShaderControls,
    extent: RenderExtent,
) -> Result<ShaderLayerWorkFacts, ShaderAdmissionError> {
    let geometry = plane_layer_geometry(layer.affine, controls, extent)?;
    Ok(ShaderLayerWorkFacts {
        layer: layer.layer,
        scale: layer.scale,
        grid_error_upper: geometry.grid_error_upper,
        address_range: geometry.address_range,
        sample_count_upper: 1,
    })
}

struct PlaneLayerGeometry {
    origin_grid: [f64; 3],
    step_x_grid: [f64; 3],
    step_y_grid: [f64; 3],
    grid_error_upper: [f64; 3],
    address_range: [[f64; 2]; 3],
}

fn plane_layer_geometry(
    affine: ValidatedShaderAffine,
    controls: PlaneShaderControls,
    extent: RenderExtent,
) -> Result<PlaneLayerGeometry, ShaderAdmissionError> {
    let width = f64::from(extent.width_pixels());
    let height = f64::from(extent.height_pixels());
    let pixel_corners = [
        [0.5, 0.5],
        [width - 0.5, 0.5],
        [0.5, height - 0.5],
        [width - 0.5, height - 0.5],
    ];
    let rows = affine.world_to_grid_rows().map(|row| row.map(f64::from));
    let center = controls.center_world.map(f64::from);
    let right = controls.right_world.map(f64::from);
    let up = controls.up_world.map(f64::from);
    let scale = f64::from(controls.world_units_per_logical_point);
    let span = controls.screen_span.map(f64::from);
    let first_screen_x = (0.5 / width - 0.5) * span[0];
    let first_screen_y = (0.5 - 0.5 / height) * span[1];
    let screen_step_x = span[0] / width;
    let screen_step_y = -span[1] / height;
    let origin_world = std::array::from_fn(|axis| {
        center[axis] + (right[axis] * first_screen_x + up[axis] * first_screen_y) * scale
    });
    let step_x_world = right.map(|value| value * screen_step_x * scale);
    let step_y_world = up.map(|value| value * screen_step_y * scale);
    let transform_point = |point: [f64; 3]| {
        rows.map(|row| row[0] * point[0] + row[1] * point[1] + row[2] * point[2] + row[3])
    };
    let transform_vector = |vector: [f64; 3]| {
        rows.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
    };
    let origin_grid = transform_point(origin_world);
    let step_x_grid = transform_vector(step_x_world);
    let step_y_grid = transform_vector(step_y_world);
    if !origin_grid.iter().all(|value| value.is_finite())
        || !step_x_grid.iter().all(|value| value.is_finite())
        || !step_y_grid.iter().all(|value| value.is_finite())
    {
        return Err(shader_envelope_error(
            ShaderEnvelopeStage::PlaneGridAddress,
            ShaderEnvelopeAxis::All,
            ShaderEnvelopeFailure::NonFiniteBound,
        ));
    }
    let mut address_range = [[f64::INFINITY, f64::NEG_INFINITY]; 3];
    let mut maximum_world = [0.0_f64; 3];
    for pixel in pixel_corners {
        let screen_x = (pixel[0] / width - 0.5) * span[0];
        let screen_y = (0.5 - pixel[1] / height) * span[1];
        let world: [f64; 3] = std::array::from_fn(|axis| {
            center[axis] + (right[axis] * screen_x + up[axis] * screen_y) * scale
        });
        for axis in 0..3 {
            maximum_world[axis] = maximum_world[axis].max(world[axis].abs());
            let grid = rows[axis][0] * world[0]
                + rows[axis][1] * world[1]
                + rows[axis][2] * world[2]
                + rows[axis][3];
            if !grid.is_finite() {
                return Err(shader_envelope_error(
                    ShaderEnvelopeStage::PlaneGridAddress,
                    ShaderEnvelopeAxis::Axis(axis_for_index(axis)),
                    ShaderEnvelopeFailure::NonFiniteBound,
                ));
            }
            address_range[axis][0] = address_range[axis][0].min(grid);
            address_range[axis][1] = address_range[axis][1].max(grid);
        }
    }
    let screen_magnitude = [span[0].abs() * 0.5, span[1].abs() * 0.5];
    let mut world_error = [0.0; 3];
    for axis in 0..3 {
        let magnitude = center[axis].abs()
            + scale.abs()
                * (right[axis].abs() * screen_magnitude[0] + up[axis].abs() * screen_magnitude[1]);
        world_error[axis] = wgsl_f32_operation_error(8, magnitude)?;
    }
    let mut grid_error_upper = affine.maximum_grid_error();
    for axis in 0..3 {
        let propagated = rows[axis][0].abs() * world_error[0]
            + rows[axis][1].abs() * world_error[1]
            + rows[axis][2].abs() * world_error[2];
        let magnitude = rows[axis][0].abs() * maximum_world[0]
            + rows[axis][1].abs() * maximum_world[1]
            + rows[axis][2].abs() * maximum_world[2]
            + rows[axis][3].abs();
        grid_error_upper[axis] = next_up(
            grid_error_upper[axis]
                + propagated
                + wgsl_f32_operation_error(7, magnitude + propagated)?,
        );
        validate_grid_error(
            ShaderEnvelopeStage::PlaneGridAddress,
            axis,
            grid_error_upper[axis],
        )?;
        address_range[axis][0] = next_down(address_range[axis][0] - grid_error_upper[axis]);
        address_range[axis][1] = next_up(address_range[axis][1] + grid_error_upper[axis]);
    }
    Ok(PlaneLayerGeometry {
        origin_grid,
        step_x_grid,
        step_y_grid,
        grid_error_upper,
        address_range,
    })
}

fn volume_layer_facts(
    layer: ShaderLayerAffine,
    controls: VolumeRayShaderControls,
    extent: RenderExtent,
) -> Result<ShaderLayerWorkFacts, ShaderAdmissionError> {
    let shape = layer.affine.grid_shape();
    let dimensions = [shape.x(), shape.y(), shape.z()];
    let sample_count_upper = dimensions.into_iter().max().unwrap_or(0);
    if sample_count_upper > MAX_EXACT_SHADER_SAMPLE_COUNT {
        return Err(ShaderAdmissionError::ShaderSampleCountExceeded {
            layer: layer.layer,
            bound: sample_count_upper,
            maximum: MAX_EXACT_SHADER_SAMPLE_COUNT,
        });
    }
    let rows = layer
        .affine
        .world_to_grid_rows()
        .map(|row| row.map(f64::from));
    let origin_bounds = affine_direction_bounds(
        controls.origin_base.map(f64::from),
        controls.origin_step_x.map(f64::from),
        controls.origin_step_y.map(f64::from),
        extent,
    )?;
    let grid_origin_bounds = transformed_point_bounds(rows, origin_bounds)?;
    let direction_magnitude = volume_direction_component_magnitudes(controls, extent);
    let direction_bounds = affine_direction_bounds(
        controls.direction_base.map(f64::from),
        controls.direction_step_x.map(f64::from),
        controls.direction_step_y.map(f64::from),
        extent,
    )?;
    let direction_norm_error =
        wgsl_f32_operation_error(8, direction_magnitude.into_iter().fold(0.0, f64::max))?
            * 3.0_f64.sqrt();
    let direction_norm_lower = 1.0 - direction_norm_error;
    if !direction_norm_lower.is_finite() || direction_norm_lower <= 0.0 {
        return Err(shader_envelope_error(
            ShaderEnvelopeStage::VolumeDirectionNormalization,
            ShaderEnvelopeAxis::All,
            ShaderEnvelopeFailure::ZeroOrIndeterminateDirectionNorm,
        ));
    }
    let inverse = layer.affine.quantized_inverse_center();
    let inverse_radius = layer.affine.quantized_inverse_radius();
    let inverse_norm_upper = (0..3)
        .map(|row| {
            (0..3)
                .map(|column| inverse[row][column].abs() + inverse_radius[row][column])
                .sum::<f64>()
        })
        .fold(0.0, f64::max);
    let grid_direction_bounds = transformed_direction_bounds(rows, direction_bounds)?;
    let raw_grid_speed_lower = (grid_direction_bounds.norm_lower / 3.0_f64.sqrt()).max(
        grid_direction_bounds
            .components
            .iter()
            .map(|component| component.abs_lower)
            .fold(0.0, f64::max),
    );
    let normalized_grid_speed_lower =
        (direction_norm_lower / (3.0_f64.sqrt() * inverse_norm_upper)).max(
            grid_direction_bounds
                .components
                .iter()
                .filter_map(|component| {
                    normalized_component_abs_lower(*component, direction_bounds.norm_upper, rows)
                })
                .fold(0.0, f64::max),
        );
    let grid_speed_lower = raw_grid_speed_lower.min(normalized_grid_speed_lower);
    let minimum_normal = f64::from(f32::MIN_POSITIVE);
    if !grid_speed_lower.is_finite() || grid_speed_lower < minimum_normal {
        let all_raw_stationary = grid_direction_bounds
            .components
            .iter()
            .all(|component| component.abs_upper < minimum_normal);
        let all_normalized_stationary = grid_direction_bounds.components.iter().all(|component| {
            normalized_component_abs_upper(*component, direction_bounds.norm_lower, rows)
                .is_some_and(|upper| upper < minimum_normal)
        });
        return Err(shader_envelope_error(
            ShaderEnvelopeStage::SubnormalDirection,
            ShaderEnvelopeAxis::All,
            if all_raw_stationary && all_normalized_stationary {
                ShaderEnvelopeFailure::AllDirectionsStationary
            } else {
                ShaderEnvelopeFailure::DivisionDenominatorNotProvablyNormal
            },
        ));
    }
    let ray_parameter_span_upper = next_up(sample_count_upper as f64 / grid_speed_lower);
    let maximum_grid_distance = (0..3)
        .map(|axis| {
            let lower = -0.5;
            let upper = dimensions[axis] as f64 - 0.5;
            let origin = grid_origin_bounds.components[axis];
            [
                (lower - origin.low).abs(),
                (lower - origin.high).abs(),
                (upper - origin.low).abs(),
                (upper - origin.high).abs(),
            ]
            .into_iter()
            .fold(0.0, f64::max)
        })
        .fold(0.0, f64::max);
    let ray_parameter_absolute_upper = next_up(maximum_grid_distance / grid_speed_lower);
    if !ray_parameter_span_upper.is_finite()
        || !ray_parameter_absolute_upper.is_finite()
        || ray_parameter_span_upper > f64::from(f32::MAX)
        || ray_parameter_absolute_upper > f64::from(f32::MAX)
    {
        return Err(shader_envelope_error(
            ShaderEnvelopeStage::VolumeSlabIntersection,
            ShaderEnvelopeAxis::All,
            ShaderEnvelopeFailure::ReachableQuotientNotProvablyFinite,
        ));
    }
    let maximum_grid_direction = grid_direction_bounds
        .components
        .iter()
        .map(|component| component.abs_upper)
        .fold(0.0, f64::max);
    let page_comparison_product = next_up(ray_parameter_span_upper * maximum_grid_direction);
    if !page_comparison_product.is_finite() || page_comparison_product > f64::from(f32::MAX) {
        return Err(shader_envelope_error(
            ShaderEnvelopeStage::VolumePageExit,
            ShaderEnvelopeAxis::All,
            ShaderEnvelopeFailure::ReachableQuotientNotProvablyFinite,
        ));
    }
    let mut grid_error_upper = layer.affine.maximum_grid_error();
    let mut all_raw_stationary = true;
    let mut all_normalized_stationary = true;
    for axis in 0..3 {
        let raw = grid_direction_bounds.components[axis];
        let normalized_abs_lower =
            normalized_component_abs_lower(raw, direction_bounds.norm_upper, rows);
        let normalized_abs_upper =
            normalized_component_abs_upper(raw, direction_bounds.norm_lower, rows);
        all_raw_stationary &= raw.abs_upper < minimum_normal;
        all_normalized_stationary &=
            normalized_abs_upper.is_some_and(|upper| upper < minimum_normal);

        // Either the color path's native-scale direction or Pick's normalized
        // direction can land in WGSL's portable-subnormal range. The shared
        // shader canonicalizes that representation to zero, so charge the
        // largest component that could be discarded anywhere in the finite
        // viewport/ray interval. A component that is not proven normal for
        // both paths must pay this bound even when it is normal at the four
        // viewport corners.
        if raw.abs_lower < minimum_normal
            || normalized_abs_lower.is_none_or(|lower| lower < minimum_normal)
        {
            let discarded_component_upper = raw
                .abs_upper
                .max(normalized_abs_upper.unwrap_or(minimum_normal))
                .min(minimum_normal);
            let discarded = next_up(discarded_component_upper * ray_parameter_span_upper);
            if !discarded.is_finite() {
                return Err(shader_envelope_error(
                    ShaderEnvelopeStage::SubnormalDirection,
                    ShaderEnvelopeAxis::Axis(axis_for_index(axis)),
                    ShaderEnvelopeFailure::NonFiniteBound,
                ));
            }
            grid_error_upper[axis] = next_up(grid_error_upper[axis] + discarded);
            validate_grid_error(
                ShaderEnvelopeStage::SubnormalDirection,
                axis,
                grid_error_upper[axis],
            )?;
        }
        let direction_error =
            grid_direction_bounds.component_error[axis].max(normalized_component_error(rows));
        let sample_construction_error = next_up(
            grid_origin_bounds.component_error[axis]
                + direction_error * ray_parameter_absolute_upper
                + wgsl_f32_operation_error(
                    4,
                    dimensions[axis] as f64 + grid_origin_bounds.components[axis].abs_upper,
                )?,
        );
        grid_error_upper[axis] = next_up(grid_error_upper[axis] + sample_construction_error);
        let coordinate_magnitude = dimensions[axis] as f64 + 0.5;
        grid_error_upper[axis] = next_up(
            grid_error_upper[axis]
                + wgsl_f32_operation_error(4, coordinate_magnitude.min(16_777_216.0))?,
        );
        validate_grid_error(
            ShaderEnvelopeStage::VolumeSamplePoint,
            axis,
            grid_error_upper[axis],
        )?;
    }
    if all_raw_stationary && all_normalized_stationary {
        return Err(shader_envelope_error(
            ShaderEnvelopeStage::SubnormalDirection,
            ShaderEnvelopeAxis::All,
            ShaderEnvelopeFailure::AllDirectionsStationary,
        ));
    }
    Ok(ShaderLayerWorkFacts {
        layer: layer.layer,
        scale: layer.scale,
        grid_error_upper,
        address_range: [
            [-0.5, shape.x() as f64 - 0.5],
            [-0.5, shape.y() as f64 - 0.5],
            [-0.5, shape.z() as f64 - 0.5],
        ],
        sample_count_upper,
    })
}

#[derive(Debug, Clone, Copy)]
struct AffineComponentBounds {
    low: f64,
    high: f64,
    abs_lower: f64,
    abs_upper: f64,
}

impl AffineComponentBounds {
    fn from_interval(low: f64, high: f64) -> Result<Self, ShaderAdmissionError> {
        if !low.is_finite() || !high.is_finite() || low > high {
            return Err(shader_envelope_error(
                ShaderEnvelopeStage::VolumeRayConstruction,
                ShaderEnvelopeAxis::All,
                ShaderEnvelopeFailure::NonFiniteBound,
            ));
        }
        let abs_lower = if low <= 0.0 && high >= 0.0 {
            0.0
        } else {
            low.abs().min(high.abs())
        };
        Ok(Self {
            low,
            high,
            abs_lower,
            abs_upper: low.abs().max(high.abs()),
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct AffineDirectionBounds {
    base: [f64; 3],
    step_x: [f64; 3],
    step_y: [f64; 3],
    component_error: [f64; 3],
    components: [AffineComponentBounds; 3],
    norm_lower: f64,
    norm_upper: f64,
    maximum_x: f64,
    maximum_y: f64,
}

fn affine_direction_bounds(
    base: [f64; 3],
    step_x: [f64; 3],
    step_y: [f64; 3],
    extent: RenderExtent,
) -> Result<AffineDirectionBounds, ShaderAdmissionError> {
    let maximum_x = f64::from(extent.width_pixels().saturating_sub(1));
    let maximum_y = f64::from(extent.height_pixels().saturating_sub(1));
    let component_error = std::array::from_fn(|axis| {
        let magnitude =
            base[axis].abs() + step_x[axis].abs() * maximum_x + step_y[axis].abs() * maximum_y;
        wgsl_f32_operation_error(4, magnitude).unwrap_or(f64::INFINITY)
    });
    if component_error.iter().any(|error| !error.is_finite()) {
        return Err(shader_envelope_error(
            ShaderEnvelopeStage::VolumeRayConstruction,
            ShaderEnvelopeAxis::All,
            ShaderEnvelopeFailure::NonFiniteBound,
        ));
    }
    let mut components = [AffineComponentBounds {
        low: 0.0,
        high: 0.0,
        abs_lower: 0.0,
        abs_upper: 0.0,
    }; 3];
    for axis in 0..3 {
        components[axis] = affine_scalar_rectangle_interval(
            base[axis],
            step_x[axis],
            step_y[axis],
            maximum_x,
            maximum_y,
            component_error[axis],
        )?;
    }
    let nominal_norm_lower =
        affine_vector_rectangle_minimum_norm(base, step_x, step_y, maximum_x, maximum_y)?;
    let nominal_norm_upper =
        affine_vector_rectangle_maximum_norm(base, step_x, step_y, maximum_x, maximum_y)?;
    let vector_error = component_error
        .into_iter()
        .map(|error| error * error)
        .sum::<f64>()
        .sqrt();
    let norm_lower = (nominal_norm_lower - vector_error).max(0.0);
    let norm_upper = next_up(nominal_norm_upper + vector_error);
    if !norm_lower.is_finite() || !norm_upper.is_finite() || norm_upper <= 0.0 {
        return Err(shader_envelope_error(
            ShaderEnvelopeStage::VolumeDirectionNormalization,
            ShaderEnvelopeAxis::All,
            ShaderEnvelopeFailure::ZeroOrIndeterminateDirectionNorm,
        ));
    }
    Ok(AffineDirectionBounds {
        base,
        step_x,
        step_y,
        component_error,
        components,
        norm_lower,
        norm_upper,
        maximum_x,
        maximum_y,
    })
}

fn transformed_direction_bounds(
    rows: [[f64; 4]; 3],
    direction: AffineDirectionBounds,
) -> Result<AffineDirectionBounds, ShaderAdmissionError> {
    transformed_affine_bounds(rows, direction, false)
}

fn transformed_point_bounds(
    rows: [[f64; 4]; 3],
    points: AffineDirectionBounds,
) -> Result<AffineDirectionBounds, ShaderAdmissionError> {
    transformed_affine_bounds(rows, points, true)
}

fn transformed_affine_bounds(
    rows: [[f64; 4]; 3],
    direction: AffineDirectionBounds,
    include_translation: bool,
) -> Result<AffineDirectionBounds, ShaderAdmissionError> {
    let transform = |vector: [f64; 3]| {
        rows.map(|row| {
            row[0] * vector[0]
                + row[1] * vector[1]
                + row[2] * vector[2]
                + if include_translation { row[3] } else { 0.0 }
        })
    };
    let base = transform(direction.base);
    let transform_vector = |vector: [f64; 3]| {
        rows.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
    };
    let step_x = transform_vector(direction.step_x);
    let step_y = transform_vector(direction.step_y);
    let maximum_x = direction.maximum_x;
    let maximum_y = direction.maximum_y;
    let component_error = std::array::from_fn(|axis| {
        let propagated = rows[axis][0].abs() * direction.component_error[0]
            + rows[axis][1].abs() * direction.component_error[1]
            + rows[axis][2].abs() * direction.component_error[2];
        let magnitude = rows[axis][0].abs() * direction.components[0].abs_upper
            + rows[axis][1].abs() * direction.components[1].abs_upper
            + rows[axis][2].abs() * direction.components[2].abs_upper
            + if include_translation {
                rows[axis][3].abs()
            } else {
                0.0
            };
        next_up(
            propagated
                + wgsl_f32_operation_error(5, magnitude + propagated).unwrap_or(f64::INFINITY),
        )
    });
    if component_error.iter().any(|error| !error.is_finite()) {
        return Err(shader_envelope_error(
            ShaderEnvelopeStage::VolumeWorldToGrid,
            ShaderEnvelopeAxis::All,
            ShaderEnvelopeFailure::NonFiniteBound,
        ));
    }
    let mut components = [AffineComponentBounds {
        low: 0.0,
        high: 0.0,
        abs_lower: 0.0,
        abs_upper: 0.0,
    }; 3];
    for axis in 0..3 {
        components[axis] = affine_scalar_rectangle_interval(
            base[axis],
            step_x[axis],
            step_y[axis],
            maximum_x,
            maximum_y,
            component_error[axis],
        )?;
    }
    let nominal_norm_lower =
        affine_vector_rectangle_minimum_norm(base, step_x, step_y, maximum_x, maximum_y)?;
    let nominal_norm_upper =
        affine_vector_rectangle_maximum_norm(base, step_x, step_y, maximum_x, maximum_y)?;
    let vector_error = component_error
        .into_iter()
        .map(|error| error * error)
        .sum::<f64>()
        .sqrt();
    Ok(AffineDirectionBounds {
        base,
        step_x,
        step_y,
        component_error,
        components,
        norm_lower: (nominal_norm_lower - vector_error).max(0.0),
        norm_upper: next_up(nominal_norm_upper + vector_error),
        maximum_x,
        maximum_y,
    })
}

fn affine_scalar_rectangle_interval(
    base: f64,
    step_x: f64,
    step_y: f64,
    maximum_x: f64,
    maximum_y: f64,
    error: f64,
) -> Result<AffineComponentBounds, ShaderAdmissionError> {
    let x = step_x * maximum_x;
    let y = step_y * maximum_y;
    let low = next_down(base + x.min(0.0) + y.min(0.0) - error);
    let high = next_up(base + x.max(0.0) + y.max(0.0) + error);
    AffineComponentBounds::from_interval(low, high)
}

fn affine_vector_rectangle_minimum_norm(
    base: [f64; 3],
    step_x: [f64; 3],
    step_y: [f64; 3],
    maximum_x: f64,
    maximum_y: f64,
) -> Result<f64, ShaderAdmissionError> {
    let dot = |left: [f64; 3], right: [f64; 3]| {
        left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
    };
    let point = |x: f64, y: f64| {
        std::array::from_fn::<_, 3, _>(|axis| base[axis] + step_x[axis] * x + step_y[axis] * y)
    };
    let mut candidates = Vec::with_capacity(9);
    candidates.extend([
        (0.0, 0.0),
        (maximum_x, 0.0),
        (0.0, maximum_y),
        (maximum_x, maximum_y),
    ]);
    let step_x_squared = dot(step_x, step_x);
    let step_y_squared = dot(step_y, step_y);
    if step_y_squared > 0.0 {
        for x in [0.0, maximum_x] {
            let edge_base = point(x, 0.0);
            candidates.push((
                x,
                (-dot(edge_base, step_y) / step_y_squared).clamp(0.0, maximum_y),
            ));
        }
    }
    if step_x_squared > 0.0 {
        for y in [0.0, maximum_y] {
            let edge_base = point(0.0, y);
            candidates.push((
                (-dot(edge_base, step_x) / step_x_squared).clamp(0.0, maximum_x),
                y,
            ));
        }
    }
    let cross = dot(step_x, step_y);
    let determinant = step_x_squared * step_y_squared - cross * cross;
    if determinant > 0.0 {
        let rhs_x = -dot(base, step_x);
        let rhs_y = -dot(base, step_y);
        let x = (rhs_x * step_y_squared - rhs_y * cross) / determinant;
        let y = (rhs_y * step_x_squared - rhs_x * cross) / determinant;
        if (0.0..=maximum_x).contains(&x) && (0.0..=maximum_y).contains(&y) {
            candidates.push((x, y));
        }
    }
    let squared = candidates
        .into_iter()
        .map(|(x, y)| dot(point(x, y), point(x, y)))
        .fold(f64::INFINITY, f64::min);
    if !squared.is_finite() || squared < 0.0 {
        return Err(shader_envelope_error(
            ShaderEnvelopeStage::VolumeDirectionNormalization,
            ShaderEnvelopeAxis::All,
            ShaderEnvelopeFailure::NonFiniteBound,
        ));
    }
    Ok(next_down(squared.sqrt()).max(0.0))
}

fn affine_vector_rectangle_maximum_norm(
    base: [f64; 3],
    step_x: [f64; 3],
    step_y: [f64; 3],
    maximum_x: f64,
    maximum_y: f64,
) -> Result<f64, ShaderAdmissionError> {
    let norm = |x: f64, y: f64| {
        (0..3)
            .map(|axis| (base[axis] + step_x[axis] * x + step_y[axis] * y).powi(2))
            .sum::<f64>()
            .sqrt()
    };
    let maximum = [
        norm(0.0, 0.0),
        norm(maximum_x, 0.0),
        norm(0.0, maximum_y),
        norm(maximum_x, maximum_y),
    ]
    .into_iter()
    .fold(0.0, f64::max);
    if !maximum.is_finite() {
        return Err(shader_envelope_error(
            ShaderEnvelopeStage::VolumeDirectionNormalization,
            ShaderEnvelopeAxis::All,
            ShaderEnvelopeFailure::NonFiniteBound,
        ));
    }
    Ok(next_up(maximum))
}

fn normalized_component_error(rows: [[f64; 4]; 3]) -> f64 {
    let maximum_row_norm = rows
        .into_iter()
        .map(|row| (row[0].powi(2) + row[1].powi(2) + row[2].powi(2)).sqrt())
        .fold(0.0, f64::max);
    wgsl_f32_operation_error(16, maximum_row_norm).unwrap_or(f64::INFINITY)
}

fn normalized_component_abs_lower(
    raw: AffineComponentBounds,
    direction_norm_upper: f64,
    rows: [[f64; 4]; 3],
) -> Option<f64> {
    (direction_norm_upper.is_finite() && direction_norm_upper > 0.0)
        .then(|| (raw.abs_lower / direction_norm_upper - normalized_component_error(rows)).max(0.0))
        .filter(|value| value.is_finite())
}

fn normalized_component_abs_upper(
    raw: AffineComponentBounds,
    direction_norm_lower: f64,
    rows: [[f64; 4]; 3],
) -> Option<f64> {
    (direction_norm_lower.is_finite() && direction_norm_lower > 0.0)
        .then(|| next_up(raw.abs_upper / direction_norm_lower + normalized_component_error(rows)))
        .filter(|value| value.is_finite())
}

fn volume_direction_component_magnitudes(
    controls: VolumeRayShaderControls,
    extent: RenderExtent,
) -> [f64; 3] {
    let base = controls.direction_base.map(|value| f64::from(value).abs());
    let step_x = controls
        .direction_step_x
        .map(|value| f64::from(value).abs());
    let step_y = controls
        .direction_step_y
        .map(|value| f64::from(value).abs());
    let maximum_x = f64::from(extent.width_pixels().saturating_sub(1));
    let maximum_y = f64::from(extent.height_pixels().saturating_sub(1));
    std::array::from_fn(|axis| base[axis] + step_x[axis] * maximum_x + step_y[axis] * maximum_y)
}

fn general_dvr_required(schedules: &[ShaderLayerSchedule], affines: &[ShaderLayerAffine]) -> bool {
    schedules.len() > 1
        && schedules
            .iter()
            .all(|schedule| schedule.mode == ShaderRenderMode::DirectVolume)
        && !(schedules
            .iter()
            .all(|schedule| schedule.sampling == SamplingPolicy::VoxelExact)
            && affines.windows(2).all(|pair| {
                pair[0].scale == pair[1].scale
                    && pair[0].affine.grid_shape() == pair[1].affine.grid_shape()
                    && pair[0].affine.world_to_grid_rows() == pair[1].affine.world_to_grid_rows()
            }))
}

fn general_dvr_count_bound(affines: &[ShaderLayerAffine]) -> Result<u64, ShaderAdmissionError> {
    let mut world_min = [f64::INFINITY; 3];
    let mut world_max = [f64::NEG_INFINITY; 3];
    let mut maximum_grid_speed = 0.0_f64;
    for layer in affines {
        let inverse = layer.affine.quantized_inverse_center();
        let radius = layer.affine.quantized_inverse_radius();
        let shape = layer.affine.grid_shape();
        let bounds = [
            [-0.5, shape.x() as f64 - 0.5],
            [-0.5, shape.y() as f64 - 0.5],
            [-0.5, shape.z() as f64 - 0.5],
        ];
        for mask in 0_u8..8 {
            let grid: [f64; 3] =
                std::array::from_fn(|axis| bounds[axis][usize::from(mask & (1 << axis) != 0)]);
            for axis in 0..3 {
                let center = inverse[axis][0] * grid[0]
                    + inverse[axis][1] * grid[1]
                    + inverse[axis][2] * grid[2]
                    + inverse[axis][3];
                let error = radius[axis][0] * grid[0].abs()
                    + radius[axis][1] * grid[1].abs()
                    + radius[axis][2] * grid[2].abs()
                    + radius[axis][3];
                world_min[axis] = world_min[axis].min(next_down(center - error));
                world_max[axis] = world_max[axis].max(next_up(center + error));
            }
        }
        let rows = layer.affine.world_to_grid_rows();
        maximum_grid_speed = maximum_grid_speed.max(
            rows.into_iter()
                .map(|row| {
                    (f64::from(row[0]).powi(2)
                        + f64::from(row[1]).powi(2)
                        + f64::from(row[2]).powi(2))
                    .sqrt()
                })
                .fold(0.0, f64::max),
        );
    }
    let diagonal = (0..3)
        .map(|axis| (world_max[axis] - world_min[axis]).powi(2))
        .sum::<f64>()
        .sqrt();
    let count = next_up(diagonal * maximum_grid_speed).ceil();
    if !count.is_finite() || count < 0.0 {
        return Err(shader_envelope_error(
            ShaderEnvelopeStage::GeneralDvrInterval,
            ShaderEnvelopeAxis::All,
            ShaderEnvelopeFailure::NonFiniteBound,
        ));
    }
    let count = count.min(u64::MAX as f64) as u64;
    if count > MAX_EXACT_SHADER_SAMPLE_COUNT {
        return Err(ShaderAdmissionError::ShaderSampleCountExceeded {
            layer: affines
                .first()
                .expect("general DVR has at least two layer affines")
                .layer,
            bound: count,
            maximum: MAX_EXACT_SHADER_SAMPLE_COUNT,
        });
    }
    Ok(count)
}

fn validate_grid_error(
    stage: ShaderEnvelopeStage,
    axis: usize,
    upper: f64,
) -> Result<(), ShaderAdmissionError> {
    if !upper.is_finite() || upper < 0.0 {
        return Err(shader_envelope_error(
            stage,
            ShaderEnvelopeAxis::Axis(axis_for_index(axis)),
            ShaderEnvelopeFailure::NonFiniteBound,
        ));
    }
    if upper >= 0.5 {
        return Err(shader_envelope_error(
            stage,
            ShaderEnvelopeAxis::Axis(axis_for_index(axis)),
            ShaderEnvelopeFailure::ErrorBudget {
                upper_voxels: upper,
            },
        ));
    }
    Ok(())
}

fn wgsl_f32_operation_error(
    operation_count: u32,
    magnitude: f64,
) -> Result<f64, ShaderAdmissionError> {
    if !magnitude.is_finite() || magnitude < 0.0 || magnitude > f64::from(f32::MAX) {
        return Err(shader_envelope_error(
            ShaderEnvelopeStage::VolumeRayConstruction,
            ShaderEnvelopeAxis::All,
            ShaderEnvelopeFailure::NonFiniteBound,
        ));
    }
    let operation_count = f64::from(operation_count);
    let unit_roundoff = f64::from(f32::EPSILON);
    let denominator = 1.0 - operation_count * unit_roundoff;
    if denominator <= 0.0 {
        return Err(shader_envelope_error(
            ShaderEnvelopeStage::VolumeRayConstruction,
            ShaderEnvelopeAxis::All,
            ShaderEnvelopeFailure::NonFiniteBound,
        ));
    }
    Ok(next_up(
        operation_count * unit_roundoff / denominator * magnitude
            + operation_count * f64::from(f32::from_bits(1)),
    ))
}

fn next_down(value: f64) -> f64 {
    if value.is_nan() || value == f64::NEG_INFINITY {
        return value;
    }
    if value == 0.0 {
        return -f64::from_bits(1);
    }
    if value > 0.0 {
        f64::from_bits(value.to_bits() - 1)
    } else {
        f64::from_bits(value.to_bits() + 1)
    }
}

#[derive(Debug, Clone, Copy)]
struct VerifiedInverse {
    center: [[f64; 3]; 3],
    radius: f64,
    condition_inf_upper: f64,
}

fn verified_inverse(
    matrix: [[f64; 3]; 3],
    stage: AffineValidationStage,
) -> Result<VerifiedInverse, ShaderAdmissionError> {
    let scale = matrix
        .iter()
        .flatten()
        .map(|value| value.abs())
        .fold(0.0, f64::max);
    if !scale.is_finite() || scale == 0.0 {
        return Err(ShaderAdmissionError::AffineConditionExceeded {
            stage,
            condition_upper: f64::INFINITY,
        });
    }
    let scaled = matrix.map(|row| row.map(|value| value / scale));
    let mut augmented = [[0.0_f64; 6]; 3];
    for row in 0..3 {
        augmented[row][..3].copy_from_slice(&scaled[row]);
        augmented[row][3 + row] = 1.0;
    }
    for column in 0..3 {
        let mut pivot_row = column;
        for row in column + 1..3 {
            if augmented[row][column].abs() > augmented[pivot_row][column].abs() {
                pivot_row = row;
            }
        }
        let pivot = augmented[pivot_row][column];
        if !pivot.is_finite() || pivot == 0.0 {
            return Err(ShaderAdmissionError::AffineConditionExceeded {
                stage,
                condition_upper: f64::INFINITY,
            });
        }
        augmented.swap(column, pivot_row);
        for value in &mut augmented[column] {
            *value /= pivot;
        }
        let pivot_values = augmented[column];
        for (row_index, row_values) in augmented.iter_mut().enumerate() {
            if row_index == column {
                continue;
            }
            let factor = row_values[column];
            for (entry, pivot_entry) in row_values.iter_mut().zip(pivot_values) {
                *entry -= factor * pivot_entry;
            }
        }
    }
    let inverse_scaled = augmented.map(|row| [row[3], row[4], row[5]]);
    if !inverse_scaled
        .iter()
        .flatten()
        .all(|value| value.is_finite())
    {
        return Err(ShaderAdmissionError::AffineConditionExceeded {
            stage,
            condition_upper: f64::INFINITY,
        });
    }
    let left = matrix_product(scaled, inverse_scaled);
    let right = matrix_product(inverse_scaled, scaled);
    let residual_bound = |product: [[f64; 3]; 3], lhs: [[f64; 3]; 3], rhs: [[f64; 3]; 3]| {
        (0..3)
            .map(|row| {
                (0..3)
                    .map(|column| {
                        let expected = f64::from(row == column);
                        let roundoff = (0..3)
                            .map(|term| lhs[row][term].abs() * rhs[term][column].abs())
                            .sum::<f64>()
                            * 8.0
                            * f64::EPSILON;
                        next_up((expected - product[row][column]).abs() + roundoff)
                    })
                    .sum::<f64>()
            })
            .fold(0.0, f64::max)
    };
    let residual = residual_bound(left, scaled, inverse_scaled).max(residual_bound(
        right,
        inverse_scaled,
        scaled,
    ));
    let inverse_scaled_norm = matrix_inf_norm(inverse_scaled);
    let matrix_norm = matrix_inf_norm(scaled);
    if !residual.is_finite() || residual >= 1.0 {
        return Err(ShaderAdmissionError::AffineConditionExceeded {
            stage,
            condition_upper: f64::INFINITY,
        });
    }
    let inverse_norm_upper = next_up(inverse_scaled_norm / (1.0 - residual));
    let condition_inf_upper = next_up(matrix_norm * inverse_norm_upper);
    if !condition_inf_upper.is_finite() || condition_inf_upper * f64::EPSILON > 1.0 / 64.0 {
        return Err(ShaderAdmissionError::AffineConditionExceeded {
            stage,
            condition_upper: condition_inf_upper,
        });
    }
    let center = inverse_scaled.map(|row| row.map(|value| value / scale));
    let radius = next_up(inverse_scaled_norm * residual / (1.0 - residual) / scale.abs());
    if !center.iter().flatten().all(|value| value.is_finite()) || !radius.is_finite() {
        return Err(ShaderAdmissionError::AffineConditionExceeded {
            stage,
            condition_upper: condition_inf_upper,
        });
    }
    Ok(VerifiedInverse {
        center,
        radius,
        condition_inf_upper,
    })
}

fn maximum_shader_grid_error(
    grid_to_world: [f64; 16],
    shape: Shape3D,
    world_to_grid: [[f32; 4]; 3],
) -> [f64; 3] {
    let bounds = [
        [-0.5, shape.x() as f64 - 0.5],
        [-0.5, shape.y() as f64 - 0.5],
        [-0.5, shape.z() as f64 - 0.5],
    ];
    let mut maximum = [0.0_f64; 3];
    for bits in 0_u8..8 {
        let grid = [
            bounds[0][usize::from(bits & 1 != 0)],
            bounds[1][usize::from(bits & 2 != 0)],
            bounds[2][usize::from(bits & 4 != 0)],
        ];
        let world = [
            grid_to_world[0] * grid[0]
                + grid_to_world[1] * grid[1]
                + grid_to_world[2] * grid[2]
                + grid_to_world[3],
            grid_to_world[4] * grid[0]
                + grid_to_world[5] * grid[1]
                + grid_to_world[6] * grid[2]
                + grid_to_world[7],
            grid_to_world[8] * grid[0]
                + grid_to_world[9] * grid[1]
                + grid_to_world[10] * grid[2]
                + grid_to_world[11],
        ];
        let world = world.map(F32ErrorBound::from_f64);
        for axis in 0..3 {
            let row = world_to_grid[axis];
            let reconstructed = F32ErrorBound::product(row[0], world[0])
                .sum(F32ErrorBound::product(row[1], world[1]))
                .sum(F32ErrorBound::product(row[2], world[2]))
                .sum(F32ErrorBound::exact(row[3]));
            maximum[axis] = maximum[axis].max(next_up(
                (f64::from(reconstructed.center) - grid[axis]).abs() + reconstructed.radius,
            ));
        }
    }
    maximum
}

#[derive(Debug, Clone, Copy)]
struct F32ErrorBound {
    center: f32,
    radius: f64,
}

impl F32ErrorBound {
    const fn exact(center: f32) -> Self {
        Self {
            center: canonical_f32_zero(center),
            radius: 0.0,
        }
    }

    fn from_f64(value: f64) -> Self {
        let center = canonical_f32_zero(value as f32);
        if !center.is_finite() {
            return Self {
                center,
                radius: f64::INFINITY,
            };
        }
        let mut radius = (f64::from(center) - value).abs();
        if center != 0.0 && center.abs() < f32::MIN_POSITIVE {
            radius = radius.max(f64::from(center.abs()));
        }
        Self {
            center,
            radius: outward_nonnegative(radius),
        }
    }

    fn product(coefficient: f32, input: Self) -> Self {
        let exact_center = f64::from(coefficient) * f64::from(input.center);
        let propagated = f64::from(coefficient.abs()) * input.radius;
        Self::from_operation(exact_center, propagated)
    }

    fn sum(self, other: Self) -> Self {
        let exact_center = f64::from(self.center) + f64::from(other.center);
        Self::from_operation(
            exact_center,
            outward_nonnegative(self.radius + other.radius),
        )
    }

    fn from_operation(exact_center: f64, propagated: f64) -> Self {
        let center = canonical_f32_zero(exact_center as f32);
        if !center.is_finite() || !propagated.is_finite() {
            return Self {
                center,
                radius: f64::INFINITY,
            };
        }
        let center_error = (f64::from(center) - exact_center).abs();
        let rounding = if propagated == 0.0 {
            center_error
        } else {
            next_up(center_error + f32_half_ulp(center))
        };
        let flush = if center != 0.0 && center.abs() < f32::MIN_POSITIVE {
            f64::from(center.abs())
        } else {
            0.0
        };
        Self {
            center,
            radius: outward_nonnegative(propagated + rounding.max(flush)),
        }
    }
}

fn outward_nonnegative(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { next_up(value) }
}

fn f32_half_ulp(value: f32) -> f64 {
    if !value.is_finite() {
        return f64::INFINITY;
    }
    let magnitude = value.abs();
    if magnitude == 0.0 || magnitude < f32::MIN_POSITIVE {
        return f64::from(f32::from_bits(1)) * 0.5;
    }
    let next = f32::from_bits(magnitude.to_bits() + 1);
    f64::from(next - magnitude) * 0.5
}

fn matrix_product(left: [[f64; 3]; 3], right: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut result = [[0.0; 3]; 3];
    for row in 0..3 {
        for column in 0..3 {
            result[row][column] = (0..3)
                .map(|term| left[row][term] * right[term][column])
                .sum();
        }
    }
    result
}

fn matrix_vector(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    matrix.map(|row| (0..3).map(|column| row[column] * vector[column]).sum())
}

fn matrix_inf_norm(matrix: [[f64; 3]; 3]) -> f64 {
    matrix
        .into_iter()
        .map(|row| row.into_iter().map(f64::abs).sum())
        .fold(0.0, f64::max)
}

fn next_up(value: f64) -> f64 {
    if value.is_nan() || value == f64::INFINITY {
        return value;
    }
    if value == 0.0 {
        return f64::from_bits(1);
    }
    if value > 0.0 {
        f64::from_bits(value.to_bits() + 1)
    } else {
        f64::from_bits(value.to_bits() - 1)
    }
}

const EXACT_DETERMINANT_LIMBS: usize = 100;
const MIN_TRIPLE_EXPONENT: i32 = -3222;

fn exact_determinant_is_zero(matrix: [[f64; 3]; 3]) -> bool {
    let terms = [
        (false, [matrix[0][0], matrix[1][1], matrix[2][2]]),
        (false, [matrix[0][1], matrix[1][2], matrix[2][0]]),
        (false, [matrix[0][2], matrix[1][0], matrix[2][1]]),
        (true, [matrix[0][2], matrix[1][1], matrix[2][0]]),
        (true, [matrix[0][1], matrix[1][0], matrix[2][2]]),
        (true, [matrix[0][0], matrix[1][2], matrix[2][1]]),
    ];
    let mut positive = [0_u64; EXACT_DETERMINANT_LIMBS];
    let mut negative = [0_u64; EXACT_DETERMINANT_LIMBS];
    for (subtract, values) in terms {
        let decoded = values.map(decode_f64_integer_exponent);
        if decoded.iter().any(|(_, significand, _)| *significand == 0) {
            continue;
        }
        let sign = subtract ^ decoded.iter().fold(false, |sign, item| sign ^ item.0);
        let exponent = decoded.iter().map(|item| item.2).sum::<i32>();
        let product = multiply_three_significands(decoded[0].1, decoded[1].1, decoded[2].1);
        let shift = usize::try_from(exponent - MIN_TRIPLE_EXPONENT)
            .expect("the finite binary64 triple-product exponent is in range");
        add_shifted_exact(
            if sign { &mut negative } else { &mut positive },
            product,
            shift,
        );
    }
    positive == negative
}

fn decode_f64_integer_exponent(value: f64) -> (bool, u64, i32) {
    let bits = value.to_bits();
    let sign = bits >> 63 != 0;
    let exponent_bits = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1_u64 << 52) - 1);
    if exponent_bits == 0 {
        (sign, fraction, -1074)
    } else {
        (sign, (1_u64 << 52) | fraction, exponent_bits - 1023 - 52)
    }
}

fn multiply_three_significands(first: u64, second: u64, third: u64) -> [u64; 3] {
    let pair = u128::from(first) * u128::from(second);
    let pair_limbs = [pair as u64, (pair >> 64) as u64];
    let mut result = [0_u64; 3];
    let mut carry = 0_u128;
    for (index, limb) in pair_limbs.into_iter().enumerate() {
        let value = u128::from(limb) * u128::from(third) + carry;
        result[index] = value as u64;
        carry = value >> 64;
    }
    result[2] = carry as u64;
    result
}

fn add_shifted_exact(
    destination: &mut [u64; EXACT_DETERMINANT_LIMBS],
    source: [u64; 3],
    shift: usize,
) {
    let word_shift = shift / 64;
    let bit_shift = shift % 64;
    let mut shifted = [0_u64; EXACT_DETERMINANT_LIMBS];
    for (index, limb) in source.into_iter().enumerate() {
        let destination_index = word_shift + index;
        shifted[destination_index] |= limb << bit_shift;
        if bit_shift != 0 {
            shifted[destination_index + 1] |= limb >> (64 - bit_shift);
        }
    }
    let mut carry = 0_u128;
    for (target, addend) in destination.iter_mut().zip(shifted) {
        let sum = u128::from(*target) + u128::from(addend) + carry;
        *target = sum as u64;
        carry = sum >> 64;
    }
    debug_assert_eq!(carry, 0);
}

const fn axis_for_index(index: usize) -> Axis3 {
    match index {
        0 => Axis3::X,
        1 => Axis3::Y,
        2 => Axis3::Z,
        _ => unreachable!(),
    }
}

const fn affine_column_for_index(index: usize) -> AffineColumn {
    match index {
        0 => AffineColumn::X,
        1 => AffineColumn::Y,
        2 => AffineColumn::Z,
        3 => AffineColumn::Translation,
        _ => unreachable!(),
    }
}

const fn canonical_f32_zero(value: f32) -> f32 {
    if value == 0.0 { 0.0 } else { value }
}

/// Derives the canonical binary32 world-to-grid rows consumed by render
/// shaders from one validated affine grid-to-world transform.
pub fn shader_control_world_to_grid_rows(
    transform: GridToWorld,
) -> Result<[[f32; 4]; 3], ShaderControlAffineError> {
    let shape = Shape3D::new(1, 1, 1).expect("the compatibility domain is valid");
    ValidatedShaderAffine::new(transform, shape)
        .map(|affine| affine.world_to_grid_rows())
        .map_err(|error| match error {
            ShaderAdmissionError::SingularAffine
            | ShaderAdmissionError::AffineConditionExceeded { .. }
            | ShaderAdmissionError::ShaderQuantizedAffineSingular => {
                ShaderControlAffineError::NonInvertible
            }
            ShaderAdmissionError::ShaderControlNotFinite { .. }
            | ShaderAdmissionError::ShaderCoordinatePrecisionExceeded { .. }
            | ShaderAdmissionError::ShaderCoordinateEnvelopeExceeded { .. }
            | ShaderAdmissionError::ShaderSampleCountExceeded { .. } => {
                ShaderControlAffineError::NotRepresentable
            }
        })
}

/// A monotonically assigned identity used to suppress stale render results.
///
/// The assigning application/runtime decides when an intent is superseded;
/// render backends compare this value but do not reinterpret it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FrameIdentity(u64);

impl FrameIdentity {
    pub const fn initial() -> Self {
        Self(0)
    }

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One of the viewer's four fixed logical presentation targets.
///
/// This identity is stable application intent shared with renderer frame
/// coordination. Dynamic backend presentation handles remain a separate,
/// opaque lifecycle concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PresentationTarget {
    ThreeD,
    Xy,
    Xz,
    Yz,
}

impl PresentationTarget {
    pub const ALL: [Self; 4] = [Self::ThreeD, Self::Xy, Self::Xz, Self::Yz];

    pub const fn is_cross_section(self) -> bool {
        !matches!(self, Self::ThreeD)
    }

    pub const fn index(self) -> usize {
        match self {
            Self::ThreeD => 0,
            Self::Xy => 1,
            Self::Xz => 2,
            Self::Yz => 3,
        }
    }
}

/// A backend-neutral target size in physical render pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderExtent {
    width_pixels: u32,
    height_pixels: u32,
}

/// Backend capability envelope used by presentation code to negotiate HiDPI
/// render sizes before submitting them to a renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderExtentEnvelope {
    max_width_pixels: u32,
    max_height_pixels: u32,
}

impl RenderExtentEnvelope {
    pub fn new(max_width_pixels: u32, max_height_pixels: u32) -> Result<Self, RenderApiError> {
        if max_width_pixels == 0 || max_height_pixels == 0 {
            return Err(RenderApiError::InvalidRenderExtentEnvelope);
        }
        Ok(Self {
            max_width_pixels,
            max_height_pixels,
        })
    }

    pub const fn max_width_pixels(self) -> u32 {
        self.max_width_pixels
    }

    pub const fn max_height_pixels(self) -> u32 {
        self.max_height_pixels
    }
}

impl RenderExtent {
    pub fn new(width_pixels: u32, height_pixels: u32) -> Result<Self, RenderApiError> {
        if width_pixels == 0 || height_pixels == 0 {
            return Err(RenderApiError::InvalidRenderExtent);
        }
        Ok(Self {
            width_pixels,
            height_pixels,
        })
    }

    pub const fn width_pixels(self) -> u32 {
        self.width_pixels
    }

    pub const fn height_pixels(self) -> u32 {
        self.height_pixels
    }
}

/// The framework-neutral view for one render target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RenderViewIntent {
    Volume {
        camera: CameraView,
        iso_light: IsoLightState,
    },
    CrossSection(CrossSectionView),
}

/// Geometry family of the render pass required by one target.
///
/// This identity is deliberately independent of the GPU backend and volume
/// mode. A mixed-channel volume pass remains one volume pass, while every
/// arbitrary cross-section uses the plane family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RenderPassKind {
    Plane,
    Volume,
}

impl RenderViewIntent {
    pub const fn volume(camera: CameraView, iso_light: IsoLightState) -> Self {
        Self::Volume { camera, iso_light }
    }

    pub const fn cross_section(view: CrossSectionView) -> Self {
        Self::CrossSection(view)
    }

    pub const fn pass_kind(self) -> RenderPassKind {
        match self {
            Self::Volume { .. } => RenderPassKind::Volume,
            Self::CrossSection(_) => RenderPassKind::Plane,
        }
    }
}

/// One visible logical layer and its validated scientific display intent.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerRenderIntent {
    layer: LogicalLayerKey,
    transfer: LayerTransfer,
    render_state: RenderState,
}

impl LayerRenderIntent {
    pub fn new(layer: LogicalLayerKey, transfer: LayerTransfer, render_state: RenderState) -> Self {
        Self {
            layer,
            transfer,
            render_state,
        }
    }

    pub const fn layer(&self) -> LogicalLayerKey {
        self.layer
    }

    pub const fn transfer(&self) -> &LayerTransfer {
        &self.transfer
    }

    pub const fn render_state(&self) -> &RenderState {
        &self.render_state
    }
}

/// One immutable, bounded request to produce a current frame.
#[derive(Debug, Clone)]
pub struct RenderIntent {
    frame: FrameIdentity,
    resource_identity: DatasetResourceIdentity,
    timepoint: TimeIndex,
    view: RenderViewIntent,
    presentation: PresentationViewport,
    extent: RenderExtent,
    layers: Vec<LayerRenderIntent>,
    shader_work_envelope: Option<Arc<SharedShaderWorkEnvelope>>,
}

impl PartialEq for RenderIntent {
    fn eq(&self, other: &Self) -> bool {
        self.same_semantic_request(other)
            && match (
                self.shader_work_envelope.as_ref(),
                other.shader_work_envelope.as_ref(),
            ) {
                (None, None) => true,
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                _ => false,
            }
    }
}

impl RenderIntent {
    /// Compares the immutable scientific/display request while excluding its
    /// derived shader-work proof allocation. A progressive quality handoff
    /// may keep one semantic frame while changing requirement scales and
    /// therefore its proof. Prepared-execution equality remains stricter via
    /// [`PartialEq`], which also requires identical proof ownership.
    pub fn same_semantic_request(&self, other: &Self) -> bool {
        self.frame == other.frame
            && self.resource_identity == other.resource_identity
            && self.timepoint == other.timepoint
            && self.view == other.view
            && self.presentation == other.presentation
            && self.extent == other.extent
            && self.layers == other.layers
    }

    pub fn new(
        frame: FrameIdentity,
        resource_identity: DatasetResourceIdentity,
        timepoint: TimeIndex,
        view: RenderViewIntent,
        presentation: PresentationViewport,
        extent: RenderExtent,
        layers: Vec<LayerRenderIntent>,
    ) -> Result<Self, RenderApiError> {
        if layers.is_empty() {
            return Err(RenderApiError::EmptyRenderLayers);
        }
        if layers.len() > MAX_RENDER_LAYERS {
            return Err(RenderApiError::TooManyRenderLayers {
                actual: layers.len(),
                maximum: MAX_RENDER_LAYERS,
            });
        }
        let mut seen = HashSet::with_capacity(layers.len());
        for layer in &layers {
            if !seen.insert(layer.layer()) {
                return Err(RenderApiError::DuplicateRenderLayer {
                    ordinal: layer.layer().ordinal(),
                });
            }
        }
        Ok(Self {
            frame,
            resource_identity,
            timepoint,
            view,
            presentation,
            extent,
            layers,
            shader_work_envelope: None,
        })
    }

    pub const fn frame(&self) -> FrameIdentity {
        self.frame
    }

    /// Rebinds an already validated owned intent to a new frame identity.
    /// Frame identity does not participate in layer/view validation, so this
    /// avoids rebuilding and revalidating the small layer cohort merely to
    /// compare a candidate before allocating its final frame number.
    pub fn with_frame(mut self, frame: FrameIdentity) -> Self {
        self.frame = frame;
        self
    }

    /// Finalizes a semantic intent with the exact shader-work proof prepared
    /// for its viewport, layer order, scale chains, and camera controls.
    pub fn with_shader_work_envelope(mut self, envelope: ShaderWorkEnvelope) -> Self {
        debug_assert_eq!(self.view.pass_kind(), envelope.pass_kind());
        self.shader_work_envelope = Some(Arc::new(SharedShaderWorkEnvelope::unaccounted(envelope)));
        self
    }

    /// Finalizes an intent with one worker-cached proof allocation. Clones and
    /// frame-coordinator reuse checks preserve this O(1) identity instead of
    /// comparing the proof's layer/scale arrays on the UI or renderer path.
    pub fn with_shared_shader_work_envelope(
        mut self,
        envelope: Arc<SharedShaderWorkEnvelope>,
    ) -> Self {
        debug_assert_eq!(self.view.pass_kind(), envelope.envelope().pass_kind());
        self.shader_work_envelope = Some(envelope);
        self
    }

    pub const fn resource_identity(&self) -> DatasetResourceIdentity {
        self.resource_identity
    }

    pub const fn timepoint(&self) -> TimeIndex {
        self.timepoint
    }

    pub const fn view(&self) -> RenderViewIntent {
        self.view
    }

    pub const fn presentation(&self) -> PresentationViewport {
        self.presentation
    }

    pub const fn extent(&self) -> RenderExtent {
        self.extent
    }

    pub fn layers(&self) -> &[LayerRenderIntent] {
        &self.layers
    }

    pub fn shader_work_envelope(&self) -> Option<&ShaderWorkEnvelope> {
        self.shader_work_envelope
            .as_deref()
            .map(SharedShaderWorkEnvelope::envelope)
    }
}

/// How a semantic dataset resource contributes to progressive rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderRequirementRole {
    FirstUsefulFrame,
    Refinement,
    /// Resident navigation guard. It is uploaded and retained but does not
    /// affect current-frame coverage until explicitly promoted.
    Prefetch,
}

/// One semantic dataset resource needed by a render intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderRequirement {
    key: BrickKey,
    role: RenderRequirementRole,
}

/// Ordered target-to-coarser catalog levels available to one rendered layer.
///
/// The first entry is the selected target. Later entries are strictly coarser
/// fallback candidates. A volume requirement uses exactly one entry; a Plane
/// requirement may include catalog levels that are not demanded by this frame
/// so already-resident intermediate data can be reused without serializing
/// target loading through it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderLayerScaleChain {
    layer: LogicalLayerKey,
    scales: Box<[ScaleLevel]>,
}

impl RenderLayerScaleChain {
    pub fn new(
        layer: LogicalLayerKey,
        scales: impl Into<Box<[ScaleLevel]>>,
    ) -> Result<Self, RenderApiError> {
        let scales = scales.into();
        if scales.is_empty()
            || scales.len() > mirante4d_dataset::MAX_SCALES_PER_LAYER
            || scales.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(RenderApiError::InvalidRenderScaleChain);
        }
        Ok(Self { layer, scales })
    }

    pub const fn layer(&self) -> LogicalLayerKey {
        self.layer
    }

    pub fn scales(&self) -> &[ScaleLevel] {
        &self.scales
    }

    pub fn target(&self) -> ScaleLevel {
        self.scales[0]
    }

    pub fn fallback(&self) -> Option<ScaleLevel> {
        self.scales
            .last()
            .copied()
            .filter(|_| self.scales.len() > 1)
    }

    /// Finest catalog level eligible after the target. Together with
    /// `fallback()`, this bounds what a progressive Plane may show when
    /// already-resident intermediate levels are reused.
    pub fn finest_fallback(&self) -> Option<ScaleLevel> {
        self.scales.get(1).copied()
    }
}

/// Canonical regular logical-brick grid for one rendered layer and scale.
///
/// This is semantic demand metadata, not a physical package-chunk shape. It
/// belongs to the dataset generation so every backend can project a
/// `BrickKey` into its own residency representation without inferring geometry
/// from whichever resources happen to be required by one presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderResourceGrid {
    layer: LogicalLayerKey,
    scale: ScaleLevel,
    volume_shape: Shape3D,
    cell_shape: Shape3D,
}

impl RenderResourceGrid {
    pub const fn new(
        layer: LogicalLayerKey,
        scale: ScaleLevel,
        volume_shape: Shape3D,
        cell_shape: Shape3D,
    ) -> Self {
        Self {
            layer,
            scale,
            volume_shape,
            cell_shape,
        }
    }

    pub const fn layer(self) -> LogicalLayerKey {
        self.layer
    }

    pub const fn scale(self) -> ScaleLevel {
        self.scale
    }

    pub const fn volume_shape(self) -> Shape3D {
        self.volume_shape
    }

    pub const fn cell_shape(self) -> Shape3D {
        self.cell_shape
    }
}

pub fn default_logical_brick_shape(volume: Shape3D) -> Shape3D {
    Shape3D::new(
        volume.z().min(DEFAULT_LOGICAL_BRICK_SIDE),
        volume.y().min(DEFAULT_LOGICAL_BRICK_SIDE),
        volume.x().min(DEFAULT_LOGICAL_BRICK_SIDE),
    )
    .expect("a logical brick clipped to a non-empty volume is non-empty")
}

/// Dataset-generation-scoped canonical logical resource grids.
///
/// This is installed once into the GPU residency owner. Camera or
/// presentation bodies select entries but never infer or redefine them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderResourceGridCatalog {
    resource_identity: DatasetResourceIdentity,
    grids: Box<[RenderResourceGrid]>,
}

impl RenderResourceGridCatalog {
    pub fn new(
        catalog: &DatasetCatalog,
        mut grids: Vec<RenderResourceGrid>,
    ) -> Result<Self, RenderApiError> {
        grids.sort_unstable_by_key(|grid| (grid.layer(), grid.scale()));
        let expected_len = catalog
            .layers()
            .map(|layer| layer.scales().len())
            .sum::<usize>();
        if grids.len() != expected_len {
            return Err(RenderApiError::InvalidRenderResourceGrid);
        }
        for pair in grids.windows(2) {
            if (pair[0].layer(), pair[0].scale()) == (pair[1].layer(), pair[1].scale()) {
                return Err(RenderApiError::InvalidRenderResourceGrid);
            }
        }
        for grid in &grids {
            let Some(scale) = catalog
                .layer(grid.layer())
                .and_then(|layer| layer.scale(grid.scale()))
            else {
                return Err(RenderApiError::InvalidRenderResourceGrid);
            };
            let volume = grid.volume_shape().dimensions();
            let cell = grid.cell_shape().dimensions();
            if scale.shape() != grid.volume_shape() || (0..3).any(|axis| cell[axis] > volume[axis])
            {
                return Err(RenderApiError::InvalidRenderResourceGrid);
            }
        }
        for layer in catalog.layers() {
            for scale in layer.scales() {
                if grids
                    .binary_search_by_key(&(layer.key(), scale.level()), |grid| {
                        (grid.layer(), grid.scale())
                    })
                    .is_err()
                {
                    return Err(RenderApiError::InvalidRenderResourceGrid);
                }
            }
        }
        Ok(Self {
            resource_identity: catalog.resource_identity(),
            grids: grids.into(),
        })
    }

    pub fn current(catalog: &DatasetCatalog) -> Self {
        let grids = catalog
            .layers()
            .flat_map(|layer| {
                layer.scales().map(move |scale| {
                    RenderResourceGrid::new(
                        layer.key(),
                        scale.level(),
                        scale.shape(),
                        default_logical_brick_shape(scale.shape()),
                    )
                })
            })
            .collect::<Vec<_>>();
        Self::new(catalog, grids)
            .expect("the current logical-brick policy covers every non-empty dataset scale")
    }

    pub const fn resource_identity(&self) -> DatasetResourceIdentity {
        self.resource_identity
    }

    pub fn grids(&self) -> &[RenderResourceGrid] {
        &self.grids
    }

    pub fn validate_catalog(&self, catalog: &DatasetCatalog) -> Result<(), RenderApiError> {
        if self.resource_identity != catalog.resource_identity() {
            return Err(RenderApiError::RequirementIdentityMismatch);
        }
        Self::new(catalog, self.grids.to_vec()).map(|_| ())
    }

    pub fn grid(&self, layer: LogicalLayerKey, scale: ScaleLevel) -> Option<RenderResourceGrid> {
        self.grids
            .binary_search_by_key(&(layer, scale), |grid| (grid.layer(), grid.scale()))
            .ok()
            .map(|index| self.grids[index])
    }

    pub fn validate_key(&self, key: BrickKey) -> Result<RenderResourceGrid, RenderApiError> {
        if key.identity() != self.resource_identity {
            return Err(RenderApiError::RequirementIdentityMismatch);
        }
        let grid = self
            .grid(key.layer(), key.scale())
            .ok_or(RenderApiError::InvalidRenderResourceGrid)?;
        let origin = key.region().origin();
        let actual_shape = key.region().shape().dimensions();
        let volume = grid.volume_shape().dimensions();
        let cell = grid.cell_shape().dimensions();
        let valid = (0..3).all(|axis| {
            origin[axis].is_multiple_of(cell[axis])
                && origin[axis] < volume[axis]
                && actual_shape[axis] == cell[axis].min(volume[axis] - origin[axis])
        });
        valid
            .then_some(grid)
            .ok_or(RenderApiError::InvalidRenderResourceGrid)
    }
}

impl RenderRequirement {
    pub const fn new(key: BrickKey, role: RenderRequirementRole) -> Self {
        Self { key, role }
    }

    pub const fn key(self) -> BrickKey {
        self.key
    }

    pub const fn role(self) -> RenderRequirementRole {
        self.role
    }
}

/// The bounded, deduplicated semantic resources for one frame identity.
///
/// Input order is preserved so a planner can emit a deterministic traversal;
/// runtime request priority remains owned by the dataset runtime.
struct PreparedResourceBodyData {
    canonical: Arc<[BrickKey]>,
    ranked: Arc<[BrickKey]>,
    charge: OnceLock<Arc<dyn CpuByteLease>>,
    host_allocation_bytes: u64,
}

/// One immutable, accounted authority for a planned semantic cohort.
/// Dataset admission, render binding, and backend static preparation share
/// these exact Arcs; no consumer clones the up-to-65,536-key bodies or index.
#[derive(Clone)]
pub struct PreparedResourceBody {
    body: Arc<PreparedResourceBodyData>,
}

impl std::fmt::Debug for PreparedResourceBody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedResourceBody")
            .field("requirements", &self.body.canonical.len())
            .field("host_allocation_bytes", &self.body.host_allocation_bytes)
            .field(
                "charged_bytes",
                &self.body.charge.get().map(|charge| charge.reserved_bytes()),
            )
            .finish()
    }
}

impl PartialEq for PreparedResourceBody {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.body, &other.body)
    }
}

impl Eq for PreparedResourceBody {}

impl PreparedResourceBody {
    pub fn new(
        canonical: Arc<[BrickKey]>,
        ranked: Arc<[BrickKey]>,
        charge: Option<Arc<dyn CpuByteLease>>,
    ) -> Result<Self, RenderApiError> {
        if canonical.len() > MAX_RENDER_REQUIREMENTS {
            return Err(RenderApiError::TooManyRenderRequirements {
                actual: canonical.len(),
                maximum: MAX_RENDER_REQUIREMENTS,
            });
        }
        if !canonical.is_sorted()
            || canonical.windows(2).any(|pair| pair[0] == pair[1])
            || canonical.len() != ranked.len()
        {
            return Err(RenderApiError::DuplicateRenderRequirement);
        }
        for key in ranked.iter() {
            if canonical.binary_search(key).is_err() {
                return Err(RenderApiError::DuplicateRenderRequirement);
            }
        }
        let host_allocation_bytes =
            Self::preflight_host_allocation_bytes(canonical.len(), ranked.len())?;
        let prepared = Self {
            body: Arc::new(PreparedResourceBodyData {
                canonical,
                ranked,
                charge: OnceLock::new(),
                host_allocation_bytes,
            }),
        };
        if let Some(charge) = charge {
            prepared.attach_charge(charge)?;
        }
        Ok(prepared)
    }

    pub fn canonical(&self) -> &Arc<[BrickKey]> {
        &self.body.canonical
    }

    pub fn ranked(&self) -> &Arc<[BrickKey]> {
        &self.body.ranked
    }

    pub fn host_allocation_bytes(&self) -> u64 {
        self.body.host_allocation_bytes
    }

    pub fn charged_bytes(&self) -> Option<u64> {
        self.body.charge.get().map(|charge| charge.reserved_bytes())
    }

    /// Attaches the one shared ledger lifetime after all worker-prepared
    /// backend artifacts have reported their exact host-byte contribution.
    /// The immutable body identity is preserved.
    pub fn attach_charge(&self, charge: Arc<dyn CpuByteLease>) -> Result<(), RenderApiError> {
        if charge.reserved_bytes() < self.body.host_allocation_bytes {
            return Err(RenderApiError::PreparedRequirementChargeTooSmall);
        }
        if let Some(current) = self.body.charge.get() {
            return if Arc::ptr_eq(current, &charge) {
                Ok(())
            } else {
                Err(RenderApiError::PreparedRequirementChargeAlreadyAttached)
            };
        }
        self.body
            .charge
            .set(charge)
            .map_err(|_| RenderApiError::PreparedRequirementChargeAlreadyAttached)
    }

    pub fn shares_storage_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.body, &other.body)
    }

    pub fn resource_index(&self, key: BrickKey) -> Option<usize> {
        self.body.canonical.binary_search(&key).ok()
    }

    pub fn preflight_host_allocation_bytes(
        canonical_len: usize,
        ranked_len: usize,
    ) -> Result<u64, RenderApiError> {
        let key_count = canonical_len
            .checked_add(ranked_len)
            .ok_or(RenderApiError::PreparedRequirementHostAllocationOverflow)?;
        let keys = key_count
            .checked_mul(std::mem::size_of::<BrickKey>())
            .ok_or(RenderApiError::PreparedRequirementHostAllocationOverflow)?;
        let bytes = keys
            .checked_add(std::mem::size_of::<PreparedResourceBodyData>())
            .ok_or(RenderApiError::PreparedRequirementHostAllocationOverflow)?;
        u64::try_from(bytes).map_err(|_| RenderApiError::PreparedRequirementHostAllocationOverflow)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RenderRequirementResources {
    resource_identity: DatasetResourceIdentity,
    timepoint: TimeIndex,
    layers: Box<[LogicalLayerKey]>,
    scale_chains: Box<[RenderLayerScaleChain]>,
    body: PreparedResourceBody,
    first_useful_words: Arc<[u64]>,
    required_words: Arc<[u64]>,
    total_first_useful: u64,
    total_required: u64,
    dormant_residency_suffix: bool,
}

impl RenderRequirementResources {
    fn base_role_at(&self, index: usize) -> RenderRequirementRole {
        if self.first_useful_words[index / 64] & (1_u64 << (index % 64)) != 0 {
            RenderRequirementRole::FirstUsefulFrame
        } else if self.required_words[index / 64] & (1_u64 << (index % 64)) != 0 {
            RenderRequirementRole::Refinement
        } else {
            RenderRequirementRole::Prefetch
        }
    }

    fn role_at(&self, index: usize, prefetch_promoted: bool) -> RenderRequirementRole {
        match self.base_role_at(index) {
            RenderRequirementRole::Prefetch if prefetch_promoted => {
                RenderRequirementRole::Refinement
            }
            role => role,
        }
    }

    fn is_required(&self, index: usize, prefetch_promoted: bool) -> bool {
        prefetch_promoted || self.required_words[index / 64] & (1_u64 << (index % 64)) != 0
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RenderRequirementSet {
    frame: FrameIdentity,
    resources: Arc<RenderRequirementResources>,
    prefetch_promoted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderRequirements {
    set: Arc<RenderRequirementSet>,
}

/// A worker-built semantic body validated independently of a frame/camera.
/// Binding it to a `RenderIntent` touches only the at-most-64 layer list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRenderRequirements {
    resources: Arc<RenderRequirementResources>,
    prefetch_promoted: bool,
}

impl PreparedRenderRequirements {
    pub fn new(
        resource_identity: DatasetResourceIdentity,
        timepoint: TimeIndex,
        layers: Vec<LogicalLayerKey>,
        body: PreparedResourceBody,
        first_useful_prefix_len: usize,
    ) -> Result<Self, RenderApiError> {
        let required_prefix_len = body.ranked().len();
        Self::new_with_required_prefix(
            resource_identity,
            timepoint,
            layers,
            body,
            first_useful_prefix_len,
            required_prefix_len,
        )
    }

    /// Prepares one body whose ranked suffix is a resident-navigation guard.
    /// Guard resources share the canonical body and renderer-global resource
    /// grid, but do not affect current-frame completeness until O(1)
    /// promotion.
    pub fn new_with_required_prefix(
        resource_identity: DatasetResourceIdentity,
        timepoint: TimeIndex,
        layers: Vec<LogicalLayerKey>,
        body: PreparedResourceBody,
        first_useful_prefix_len: usize,
        required_prefix_len: usize,
    ) -> Result<Self, RenderApiError> {
        let layer_set = layers.iter().copied().collect::<HashSet<_>>();
        for key in body.canonical().iter().copied() {
            if key.identity() != resource_identity {
                return Err(RenderApiError::RequirementIdentityMismatch);
            }
            if key.timepoint() != timepoint {
                return Err(RenderApiError::RequirementTimepointMismatch {
                    expected: timepoint.get(),
                    actual: key.timepoint().get(),
                });
            }
            if !layer_set.contains(&key.layer()) {
                return Err(RenderApiError::RequirementLayerNotInIntent {
                    ordinal: key.layer().ordinal(),
                });
            }
        }
        let mut scales_by_layer = BTreeMap::<LogicalLayerKey, Vec<ScaleLevel>>::new();
        for key in body.canonical().iter().copied() {
            let scales = scales_by_layer.entry(key.layer()).or_default();
            if !scales.contains(&key.scale()) {
                scales.push(key.scale());
            }
        }
        let scale_chains = layers
            .iter()
            .copied()
            .map(|layer| {
                let mut scales = scales_by_layer
                    .remove(&layer)
                    .ok_or(RenderApiError::InvalidRenderScaleChain)?;
                scales.sort_unstable();
                RenderLayerScaleChain::new(layer, scales)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::new_with_required_prefix_and_scale_chains(
            resource_identity,
            timepoint,
            layers,
            scale_chains,
            body,
            first_useful_prefix_len,
            required_prefix_len,
        )
    }

    /// Prepares one immutable body with explicit target-to-coarser scale
    /// chains. Plane rendering uses every chain entry for resident fallback;
    /// volume rendering supplies one target entry and remains uniform-scale.
    pub fn new_with_required_prefix_and_scale_chains(
        resource_identity: DatasetResourceIdentity,
        timepoint: TimeIndex,
        layers: Vec<LogicalLayerKey>,
        scale_chains: Vec<RenderLayerScaleChain>,
        body: PreparedResourceBody,
        first_useful_prefix_len: usize,
        required_prefix_len: usize,
    ) -> Result<Self, RenderApiError> {
        Self::new_with_scale_chains_and_suffix_policy(
            resource_identity,
            timepoint,
            layers,
            scale_chains,
            body,
            first_useful_prefix_len,
            required_prefix_len,
            false,
        )
    }

    /// Prepares a uniform presentation body whose suffix is GPU-residency
    /// prefetch for other immutable presentation wrappers.
    ///
    /// Required keys still have to belong to this wrapper's explicit
    /// target-to-coarser chains. Suffix keys may name other scales, remain
    /// permanently dormant for this wrapper, and are never promoted into its
    /// coverage or pixels.
    pub fn new_with_dormant_residency_suffix_and_scale_chains(
        resource_identity: DatasetResourceIdentity,
        timepoint: TimeIndex,
        layers: Vec<LogicalLayerKey>,
        scale_chains: Vec<RenderLayerScaleChain>,
        body: PreparedResourceBody,
        first_useful_prefix_len: usize,
        required_prefix_len: usize,
    ) -> Result<Self, RenderApiError> {
        Self::new_with_scale_chains_and_suffix_policy(
            resource_identity,
            timepoint,
            layers,
            scale_chains,
            body,
            first_useful_prefix_len,
            required_prefix_len,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_scale_chains_and_suffix_policy(
        resource_identity: DatasetResourceIdentity,
        timepoint: TimeIndex,
        layers: Vec<LogicalLayerKey>,
        mut scale_chains: Vec<RenderLayerScaleChain>,
        body: PreparedResourceBody,
        first_useful_prefix_len: usize,
        required_prefix_len: usize,
        dormant_residency_suffix: bool,
    ) -> Result<Self, RenderApiError> {
        if first_useful_prefix_len == 0
            || first_useful_prefix_len > required_prefix_len
            || required_prefix_len > body.canonical().len()
        {
            return Err(RenderApiError::MissingFirstUsefulRequirement);
        }
        if layers.is_empty() {
            return Err(RenderApiError::EmptyRenderLayers);
        }
        if layers.len() > MAX_RENDER_LAYERS {
            return Err(RenderApiError::TooManyRenderLayers {
                actual: layers.len(),
                maximum: MAX_RENDER_LAYERS,
            });
        }
        Self::preflight_host_allocation_bytes_with_scale_count(
            layers.len(),
            scale_chains.iter().map(|chain| chain.scales().len()).sum(),
            body.canonical().len(),
        )?;
        let layer_set = layers.iter().copied().collect::<HashSet<_>>();
        if layer_set.len() != layers.len() {
            let duplicate = layers
                .iter()
                .copied()
                .find(|layer| {
                    layers
                        .iter()
                        .filter(|candidate| **candidate == *layer)
                        .count()
                        > 1
                })
                .expect("a shorter unique set proves a duplicate layer");
            return Err(RenderApiError::DuplicateRenderLayer {
                ordinal: duplicate.ordinal(),
            });
        }
        for key in body.canonical().iter().copied() {
            if key.identity() != resource_identity {
                return Err(RenderApiError::RequirementIdentityMismatch);
            }
            if key.timepoint() != timepoint {
                return Err(RenderApiError::RequirementTimepointMismatch {
                    expected: timepoint.get(),
                    actual: key.timepoint().get(),
                });
            }
            if !layer_set.contains(&key.layer()) {
                return Err(RenderApiError::RequirementLayerNotInIntent {
                    ordinal: key.layer().ordinal(),
                });
            }
        }
        scale_chains.sort_unstable_by_key(RenderLayerScaleChain::layer);
        if scale_chains.len() != layers.len()
            || scale_chains
                .windows(2)
                .any(|pair| pair[0].layer() == pair[1].layer())
            || layers.iter().any(|layer| {
                scale_chains
                    .binary_search_by_key(layer, |chain| chain.layer())
                    .is_err()
            })
        {
            return Err(RenderApiError::InvalidRenderScaleChain);
        }
        let validated_keys = if dormant_residency_suffix {
            &body.ranked()[..required_prefix_len]
        } else {
            body.canonical().as_ref()
        };
        for key in validated_keys.iter().copied() {
            let chain = &scale_chains[scale_chains
                .binary_search_by_key(&key.layer(), |chain| chain.layer())
                .expect("every validated requirement layer owns one scale chain")];
            if chain.scales().binary_search(&key.scale()).is_err() {
                return Err(RenderApiError::InvalidRenderScaleChain);
            }
        }
        let mut first_useful_words = vec![0_u64; body.canonical().len().div_ceil(64)];
        for key in body.ranked()[..first_useful_prefix_len].iter().copied() {
            let index = body
                .resource_index(key)
                .expect("a prepared ranked key belongs to its canonical body");
            first_useful_words[index / 64] |= 1_u64 << (index % 64);
        }
        let mut required_words = vec![0_u64; body.canonical().len().div_ceil(64)];
        for key in body.ranked()[..required_prefix_len].iter().copied() {
            let index = body
                .resource_index(key)
                .expect("a prepared ranked key belongs to its canonical body");
            required_words[index / 64] |= 1_u64 << (index % 64);
        }
        Ok(Self {
            resources: Arc::new(RenderRequirementResources {
                resource_identity,
                timepoint,
                layers: layers.into(),
                scale_chains: scale_chains.into(),
                body,
                first_useful_words: first_useful_words.into(),
                required_words: required_words.into(),
                total_first_useful: first_useful_prefix_len as u64,
                total_required: required_prefix_len as u64,
                dormant_residency_suffix,
            }),
            prefetch_promoted: false,
        })
    }

    pub fn bind(&self, intent: &RenderIntent) -> Result<RenderRequirements, RenderApiError> {
        validate_prepared_render_binding(&self.resources, intent)?;
        Ok(RenderRequirements {
            set: Arc::new(RenderRequirementSet {
                frame: intent.frame(),
                resources: Arc::clone(&self.resources),
                prefetch_promoted: self.prefetch_promoted,
            }),
        })
    }

    /// Promotes the resident guard to required refinement without touching a
    /// key, bitmap, or backend layout. The next frame remains truthful when a
    /// promoted guard resource has not arrived yet.
    pub fn promote_prefetch(&self) -> Self {
        Self {
            resources: Arc::clone(&self.resources),
            prefetch_promoted: !self.resources.dormant_residency_suffix,
        }
    }

    pub const fn prefetch_promoted(&self) -> bool {
        self.prefetch_promoted
    }

    pub fn required_prefix_len(&self) -> usize {
        if self.prefetch_promoted && !self.resources.dormant_residency_suffix {
            self.resources.body.ranked().len()
        } else {
            self.resources.total_required as usize
        }
    }

    pub fn prefetch_resource_count(&self) -> usize {
        self.resources
            .body
            .ranked()
            .len()
            .saturating_sub(self.resources.total_required as usize)
    }

    pub fn body(&self) -> &PreparedResourceBody {
        &self.resources.body
    }

    /// True when this prepared handle and a frame-bound handle share the
    /// exact validated ordered resource body. This comparison is O(1).
    pub fn shares_resources_with(&self, bound: &RenderRequirements) -> bool {
        Arc::ptr_eq(&self.resources, &bound.set.resources)
    }

    pub fn resource_identity(&self) -> DatasetResourceIdentity {
        self.resources.resource_identity
    }

    pub fn timepoint(&self) -> TimeIndex {
        self.resources.timepoint
    }

    pub fn layers(&self) -> &[LogicalLayerKey] {
        &self.resources.layers
    }

    pub fn scale_chains(&self) -> &[RenderLayerScaleChain] {
        &self.resources.scale_chains
    }

    pub fn first_useful_prefix_len(&self) -> usize {
        self.resources.total_first_useful as usize
    }

    /// Host allocation owned by this render wrapper, excluding the shared
    /// `PreparedResourceBody` reported separately.
    pub fn host_allocation_bytes(&self) -> u64 {
        Self::preflight_host_allocation_bytes_with_scale_count(
            self.resources.layers.len(),
            self.resources
                .scale_chains
                .iter()
                .map(|chain| chain.scales().len())
                .sum(),
            self.resources.body.canonical().len(),
        )
        .expect("a constructed prepared render wrapper has representable host bytes")
    }

    /// Exact retained bytes known before the wrapper allocates its layer body
    /// and compact role bitmaps. The shared resource body is accounted
    /// independently by [`PreparedResourceBody`].
    pub fn preflight_host_allocation_bytes(
        layer_count: usize,
        requirement_count: usize,
    ) -> Result<u64, RenderApiError> {
        Self::preflight_host_allocation_bytes_with_scale_count(
            layer_count,
            layer_count,
            requirement_count,
        )
    }

    pub fn preflight_host_allocation_bytes_with_scale_count(
        layer_count: usize,
        scale_count: usize,
        requirement_count: usize,
    ) -> Result<u64, RenderApiError> {
        let layer_bytes = layer_count
            .checked_mul(std::mem::size_of::<LogicalLayerKey>())
            .ok_or(RenderApiError::PreparedRequirementHostAllocationOverflow)?;
        let scale_chain_bytes = layer_count
            .checked_mul(std::mem::size_of::<RenderLayerScaleChain>())
            .and_then(|bytes| {
                scale_count
                    .checked_mul(std::mem::size_of::<ScaleLevel>())
                    .and_then(|scale_bytes| bytes.checked_add(scale_bytes))
            })
            .ok_or(RenderApiError::PreparedRequirementHostAllocationOverflow)?;
        let first_useful_words = requirement_count
            .checked_add(63)
            .ok_or(RenderApiError::PreparedRequirementHostAllocationOverflow)?
            / 64;
        let role_bitmap_bytes = first_useful_words
            .checked_mul(std::mem::size_of::<u64>())
            .and_then(|bytes| bytes.checked_mul(2))
            .ok_or(RenderApiError::PreparedRequirementHostAllocationOverflow)?;
        let bytes = std::mem::size_of::<RenderRequirementResources>()
            .checked_add(layer_bytes)
            .and_then(|bytes| bytes.checked_add(scale_chain_bytes))
            .and_then(|bytes| bytes.checked_add(role_bitmap_bytes))
            .ok_or(RenderApiError::PreparedRequirementHostAllocationOverflow)?;
        u64::try_from(bytes).map_err(|_| RenderApiError::PreparedRequirementHostAllocationOverflow)
    }

    pub fn shares_body_with(&self, other: &Self) -> bool {
        self.resources
            .body
            .shares_storage_with(&other.resources.body)
    }
}

fn validate_prepared_render_binding(
    resources: &RenderRequirementResources,
    intent: &RenderIntent,
) -> Result<(), RenderApiError> {
    if resources.resource_identity != intent.resource_identity() {
        return Err(RenderApiError::RequirementIdentityMismatch);
    }
    if resources.timepoint != intent.timepoint() {
        return Err(RenderApiError::RequirementTimepointMismatch {
            expected: intent.timepoint().get(),
            actual: resources.timepoint.get(),
        });
    }
    let intent_layers = intent
        .layers()
        .iter()
        .map(LayerRenderIntent::layer)
        .collect::<HashSet<_>>();
    if resources.layers.len() != intent_layers.len() {
        let missing = resources
            .layers
            .iter()
            .find(|layer| !intent_layers.contains(layer))
            .copied()
            .unwrap_or_else(|| resources.layers[0]);
        return Err(RenderApiError::RequirementLayerNotInIntent {
            ordinal: missing.ordinal(),
        });
    }
    if let Some(missing) = resources
        .layers
        .iter()
        .find(|layer| !intent_layers.contains(layer))
    {
        return Err(RenderApiError::RequirementLayerNotInIntent {
            ordinal: missing.ordinal(),
        });
    }
    if intent.view().pass_kind() == RenderPassKind::Volume
        && resources
            .scale_chains
            .iter()
            .any(|chain| chain.scales().len() != 1)
    {
        return Err(RenderApiError::InvalidRenderScaleChain);
    }
    Ok(())
}

impl RenderRequirements {
    pub fn new(
        intent: &RenderIntent,
        resources: Vec<RenderRequirement>,
    ) -> Result<Self, RenderApiError> {
        if resources.is_empty() {
            return Err(RenderApiError::EmptyRenderRequirements);
        }
        if resources.len() > MAX_RENDER_REQUIREMENTS {
            return Err(RenderApiError::TooManyRenderRequirements {
                actual: resources.len(),
                maximum: MAX_RENDER_REQUIREMENTS,
            });
        }
        let total_first_useful = resources
            .iter()
            .filter(|requirement| requirement.role() == RenderRequirementRole::FirstUsefulFrame)
            .count();
        if total_first_useful == 0 {
            return Err(RenderApiError::MissingFirstUsefulRequirement);
        }
        let mut canonical = resources
            .iter()
            .map(|requirement| requirement.key())
            .collect::<Vec<_>>();
        canonical.sort_unstable();
        canonical.dedup();
        if canonical.len() != resources.len() {
            return Err(RenderApiError::DuplicateRenderRequirement);
        }
        let ranked = resources
            .iter()
            .filter(|requirement| requirement.role() == RenderRequirementRole::FirstUsefulFrame)
            .chain(
                resources
                    .iter()
                    .filter(|requirement| requirement.role() == RenderRequirementRole::Refinement),
            )
            .chain(
                resources
                    .iter()
                    .filter(|requirement| requirement.role() == RenderRequirementRole::Prefetch),
            )
            .map(|requirement| requirement.key())
            .collect::<Vec<_>>();
        let body = PreparedResourceBody::new(canonical.into(), ranked.into(), None)?;
        let total_required = resources
            .iter()
            .filter(|requirement| requirement.role() != RenderRequirementRole::Prefetch)
            .count();
        PreparedRenderRequirements::new_with_required_prefix(
            intent.resource_identity(),
            intent.timepoint(),
            intent
                .layers()
                .iter()
                .map(LayerRenderIntent::layer)
                .collect(),
            body,
            total_first_useful,
            total_required,
        )?
        .bind(intent)
    }

    /// Rebinds an immutable semantic requirement body to a new camera/frame
    /// intent in O(layer-count) time. This is the camera-navigation path: the
    /// up-to-65,536 resource records remain shared and are not cloned or
    /// revalidated.
    pub fn rebind(&self, intent: &RenderIntent) -> Result<Self, RenderApiError> {
        let resources = &self.set.resources;
        validate_prepared_render_binding(resources, intent)?;
        Ok(Self {
            set: Arc::new(RenderRequirementSet {
                frame: intent.frame(),
                resources: Arc::clone(resources),
                prefetch_promoted: self.set.prefetch_promoted,
            }),
        })
    }

    /// True when two frame-bound handles share the exact validated ordered
    /// resource body. This comparison is O(1).
    pub fn shares_resources_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.set.resources, &other.set.resources)
    }

    pub fn prefetch_promoted(&self) -> bool {
        self.set.prefetch_promoted
    }

    pub fn is_required_resource(&self, key: BrickKey) -> bool {
        self.resource_index(key).is_some_and(|index| {
            self.set
                .resources
                .is_required(index, self.set.prefetch_promoted)
        })
    }

    pub fn frame(&self) -> FrameIdentity {
        self.set.frame
    }

    pub fn resources(&self) -> RenderRequirementIter<'_> {
        RenderRequirementIter {
            resources: &self.set.resources,
            prefetch_promoted: self.set.prefetch_promoted,
            index: 0,
        }
    }

    pub fn resource_keys(&self) -> &[BrickKey] {
        self.set.resources.body.canonical().as_ref()
    }

    pub fn scale_chains(&self) -> &[RenderLayerScaleChain] {
        &self.set.resources.scale_chains
    }

    pub fn scale_chain(&self, layer: LogicalLayerKey) -> Option<&RenderLayerScaleChain> {
        self.set
            .resources
            .scale_chains
            .binary_search_by_key(&layer, |chain| chain.layer())
            .ok()
            .map(|index| &self.set.resources.scale_chains[index])
    }

    pub fn len(&self) -> usize {
        self.set.resources.body.canonical().len()
    }

    pub fn is_empty(&self) -> bool {
        self.set.resources.body.canonical().is_empty()
    }

    pub fn prepared_body(&self) -> &PreparedResourceBody {
        &self.set.resources.body
    }

    pub fn contains_resource(&self, key: BrickKey) -> bool {
        self.set.resources.body.resource_index(key).is_some()
    }

    pub fn resource_index(&self, key: BrickKey) -> Option<usize> {
        self.set.resources.body.resource_index(key)
    }

    pub fn requirement(&self, index: usize) -> Option<RenderRequirement> {
        let key = self.resource_keys().get(index).copied()?;
        Some(RenderRequirement::new(
            key,
            self.set
                .resources
                .role_at(index, self.set.prefetch_promoted),
        ))
    }
}

pub struct RenderRequirementIter<'a> {
    resources: &'a RenderRequirementResources,
    prefetch_promoted: bool,
    index: usize,
}

impl Iterator for RenderRequirementIter<'_> {
    type Item = RenderRequirement;

    fn next(&mut self) -> Option<Self::Item> {
        let key = self.resources.body.canonical().get(self.index).copied()?;
        let role = self.resources.role_at(self.index, self.prefetch_promoted);
        self.index += 1;
        Some(RenderRequirement::new(key, role))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.resources.body.canonical().len() - self.index;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for RenderRequirementIter<'_> {}

/// Requirement-bound availability for one progressive frame.
///
/// Coverage can only be constructed by matching semantic resource keys back
/// to one validated requirement set. It separately preserves the
/// first-useful and refinement roles; it is not pixel coverage and cannot
/// classify uncovered pixels as scientifically empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameCoverage {
    frame: FrameIdentity,
    requirements: Arc<RenderRequirementResources>,
    available_words: Arc<[u64]>,
    available_first_useful: u64,
    total_first_useful: u64,
    /// Base required refinement, excluding the dormant prefetch suffix.
    available_refinement: u64,
    total_refinement: u64,
    available_prefetch: u64,
    total_prefetch: u64,
    prefetch_promoted: bool,
    layers: Arc<[FrameLayerCoverageState]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameLayerCoverageState {
    layer: LogicalLayerKey,
    target_scale: ScaleLevel,
    finest_fallback_scale: Option<ScaleLevel>,
    fallback_scale: Option<ScaleLevel>,
    available_target_required: u64,
    total_target_required: u64,
    available_target_prefetch: u64,
    total_target_prefetch: u64,
    available_required: u64,
    total_required: u64,
    available_prefetch: u64,
    total_prefetch: u64,
}

/// Requirement availability for one layer in an actual frame-coverage
/// snapshot.
///
/// `scale()` is the one truthful scalar scale only for a uniform target or a
/// single eligible fallback level. It is `None` while target and fallback
/// regions coexist or when already-resident intermediate fallback levels make
/// a scalar value unprovable without a pixel readback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameLayerCoverage {
    layer: LogicalLayerKey,
    scale: Option<ScaleLevel>,
    target_scale: ScaleLevel,
    finest_fallback_scale: Option<ScaleLevel>,
    fallback_scale: Option<ScaleLevel>,
    available_target_requirements: u64,
    total_target_requirements: u64,
    available_requirements: u64,
    total_requirements: u64,
}

impl FrameLayerCoverage {
    pub const fn layer(self) -> LogicalLayerKey {
        self.layer
    }

    pub const fn scale(self) -> Option<ScaleLevel> {
        self.scale
    }

    pub const fn target_scale(self) -> ScaleLevel {
        self.target_scale
    }

    pub const fn fallback_scale(self) -> Option<ScaleLevel> {
        self.fallback_scale
    }

    pub const fn finest_fallback_scale(self) -> Option<ScaleLevel> {
        self.finest_fallback_scale
    }

    pub const fn fallback_range(self) -> Option<(ScaleLevel, ScaleLevel)> {
        match (self.finest_fallback_scale, self.fallback_scale) {
            (Some(finest), Some(coarsest)) if self.scale.is_none() => Some((finest, coarsest)),
            _ => None,
        }
    }

    pub const fn is_mixed(self) -> bool {
        self.fallback_scale.is_some()
            && self.available_target_requirements > 0
            && self.available_target_requirements < self.total_target_requirements
    }

    pub const fn available_target_requirements(self) -> u64 {
        self.available_target_requirements
    }

    pub const fn total_target_requirements(self) -> u64 {
        self.total_target_requirements
    }

    pub const fn available_requirements(self) -> u64 {
        self.available_requirements
    }

    pub const fn total_requirements(self) -> u64 {
        self.total_requirements
    }
}

fn initial_frame_layer_coverage(
    resources: &RenderRequirementResources,
    available_words: &[u64],
) -> Arc<[FrameLayerCoverageState]> {
    let mut layers = BTreeMap::<LogicalLayerKey, FrameLayerCoverageState>::new();
    for chain in resources.scale_chains.iter() {
        layers.insert(
            chain.layer(),
            FrameLayerCoverageState {
                layer: chain.layer(),
                target_scale: chain.target(),
                finest_fallback_scale: chain.finest_fallback(),
                fallback_scale: chain.fallback(),
                available_target_required: 0,
                total_target_required: 0,
                available_target_prefetch: 0,
                total_target_prefetch: 0,
                available_required: 0,
                total_required: 0,
                available_prefetch: 0,
                total_prefetch: 0,
            },
        );
    }
    for (index, key) in resources.body.canonical().iter().copied().enumerate() {
        let available = available_words[index / 64] & (1_u64 << (index % 64)) != 0;
        let layer = layers
            .get_mut(&key.layer())
            .expect("every validated requirement layer owns one scale chain");
        let target = key.scale() == layer.target_scale;
        if resources.base_role_at(index) == RenderRequirementRole::Prefetch {
            layer.total_prefetch += 1;
            layer.available_prefetch += u64::from(available);
            if target {
                layer.total_target_prefetch += 1;
                layer.available_target_prefetch += u64::from(available);
            }
        } else {
            layer.total_required += 1;
            layer.available_required += u64::from(available);
            if target {
                layer.total_target_required += 1;
                layer.available_target_required += u64::from(available);
            }
        }
    }
    layers.into_values().collect::<Vec<_>>().into()
}

impl FrameCoverage {
    /// Creates an empty bitmap for an already validated requirement body.
    pub fn empty(requirements: &RenderRequirements) -> Self {
        let resources = &requirements.set.resources;
        let available_words: Arc<[u64]> =
            vec![0_u64; resources.body.canonical().len().div_ceil(64)].into();
        let layers = initial_frame_layer_coverage(resources, &available_words);
        Self {
            frame: requirements.frame(),
            requirements: Arc::clone(resources),
            available_words,
            available_first_useful: 0,
            total_first_useful: resources.total_first_useful,
            available_refinement: 0,
            total_refinement: resources.total_required - resources.total_first_useful,
            available_prefetch: 0,
            total_prefetch: resources.body.canonical().len() as u64 - resources.total_required,
            prefetch_promoted: requirements.set.prefetch_promoted,
            layers,
        }
    }

    pub fn from_available(
        requirements: &RenderRequirements,
        available: &[BrickKey],
    ) -> Result<Self, RenderApiError> {
        if available.len() > requirements.resources().len() {
            return Err(RenderApiError::TooManyCoveredResources {
                actual: available.len(),
                maximum: requirements.resources().len(),
            });
        }
        let mut seen = HashSet::with_capacity(available.len());
        let mut available_words = vec![0_u64; requirements.resources().len().div_ceil(64)];
        let mut available_first_useful = 0_u64;
        let mut available_refinement = 0_u64;
        let mut available_prefetch = 0_u64;
        for key in available {
            if !seen.insert(*key) {
                return Err(RenderApiError::DuplicateCoveredResource);
            }
            match requirements.resource_index(*key) {
                Some(index)
                    if requirements.set.resources.base_role_at(index)
                        == RenderRequirementRole::FirstUsefulFrame =>
                {
                    available_words[index / 64] |= 1_u64 << (index % 64);
                    available_first_useful += 1;
                }
                Some(index)
                    if requirements.set.resources.base_role_at(index)
                        == RenderRequirementRole::Refinement =>
                {
                    available_words[index / 64] |= 1_u64 << (index % 64);
                    available_refinement += 1;
                }
                Some(index) => {
                    available_words[index / 64] |= 1_u64 << (index % 64);
                    available_prefetch += 1;
                }
                None => return Err(RenderApiError::CoveredResourceNotRequired),
            }
        }

        let total_first_useful = requirements.set.resources.total_first_useful;
        let total_refinement = requirements.set.resources.total_required - total_first_useful;
        let total_prefetch =
            requirements.resources().len() as u64 - requirements.set.resources.total_required;
        let layers = initial_frame_layer_coverage(&requirements.set.resources, &available_words);
        Ok(Self {
            frame: requirements.frame(),
            requirements: Arc::clone(&requirements.set.resources),
            available_words: available_words.into(),
            available_first_useful,
            total_first_useful,
            available_refinement,
            total_refinement,
            available_prefetch,
            total_prefetch,
            prefetch_promoted: requirements.set.prefetch_promoted,
            layers,
        })
    }

    /// Recorded dataset timepoint carried by the exact immutable requirement
    /// body that produced this coverage. This is presentation evidence, not a
    /// projection of the application's currently selected timepoint.
    pub fn timepoint(&self) -> TimeIndex {
        self.requirements.timepoint
    }

    /// Applies a bounded list of residency additions/removals to a retained
    /// bitmap. The bitmap clone is N/64 words and each changed key is an O(1)
    /// lookup, so a complete progressive stream is O(N) rather than O(N²).
    pub fn with_availability_changes(
        &self,
        requirements: &RenderRequirements,
        changes: &[(BrickKey, bool)],
    ) -> Result<Self, RenderApiError> {
        if !Arc::ptr_eq(&self.requirements, &requirements.set.resources) {
            return Err(RenderApiError::CoveredResourceNotRequired);
        }
        if changes.is_empty() {
            return self.rebind(requirements);
        }
        let mut words = self.available_words.to_vec();
        let mut available_first_useful = self.available_first_useful;
        let mut available_refinement = self.available_refinement;
        let mut available_prefetch = self.available_prefetch;
        let mut layers = self.layers.to_vec();
        for (key, available) in changes {
            let Some(index) = self.requirements.body.resource_index(*key) else {
                return Err(RenderApiError::CoveredResourceNotRequired);
            };
            let mask = 1_u64 << (index % 64);
            let word = &mut words[index / 64];
            let was_available = *word & mask != 0;
            if was_available == *available {
                continue;
            }
            if *available {
                *word |= mask;
            } else {
                *word &= !mask;
            }
            let count = match self.requirements.base_role_at(index) {
                RenderRequirementRole::FirstUsefulFrame => &mut available_first_useful,
                RenderRequirementRole::Refinement => &mut available_refinement,
                RenderRequirementRole::Prefetch => &mut available_prefetch,
            };
            if *available {
                *count += 1;
            } else {
                *count -= 1;
            }
            let layer_index = layers
                .binary_search_by_key(&key.layer(), |layer| layer.layer)
                .expect("every validated requirement layer has coverage accounting");
            let layer = &mut layers[layer_index];
            let prefetch = self.requirements.base_role_at(index) == RenderRequirementRole::Prefetch;
            let layer_count = if prefetch {
                &mut layer.available_prefetch
            } else {
                &mut layer.available_required
            };
            if *available {
                *layer_count += 1;
            } else {
                *layer_count -= 1;
            }
            if key.scale() == layer.target_scale {
                let target_count = if prefetch {
                    &mut layer.available_target_prefetch
                } else {
                    &mut layer.available_target_required
                };
                if *available {
                    *target_count += 1;
                } else {
                    *target_count -= 1;
                }
            }
        }
        Ok(Self {
            frame: requirements.frame(),
            requirements: Arc::clone(&self.requirements),
            available_words: words.into(),
            available_first_useful,
            total_first_useful: self.total_first_useful,
            available_refinement,
            total_refinement: self.total_refinement,
            available_prefetch,
            total_prefetch: self.total_prefetch,
            prefetch_promoted: requirements.set.prefetch_promoted,
            layers: layers.into(),
        })
    }

    pub fn frame(&self) -> FrameIdentity {
        self.frame
    }

    pub const fn available_requirements(&self) -> u64 {
        self.available_first_useful
            + self.available_refinement
            + if self.prefetch_promoted {
                self.available_prefetch
            } else {
                0
            }
    }

    pub const fn total_requirements(&self) -> u64 {
        self.total_first_useful
            + self.total_refinement
            + if self.prefetch_promoted {
                self.total_prefetch
            } else {
                0
            }
    }

    pub const fn available_first_useful(&self) -> u64 {
        self.available_first_useful
    }

    pub const fn total_first_useful(&self) -> u64 {
        self.total_first_useful
    }

    pub const fn available_refinement(&self) -> u64 {
        self.available_refinement
            + if self.prefetch_promoted {
                self.available_prefetch
            } else {
                0
            }
    }

    pub const fn total_refinement(&self) -> u64 {
        self.total_refinement
            + if self.prefetch_promoted {
                self.total_prefetch
            } else {
                0
            }
    }

    pub const fn available_prefetch(&self) -> u64 {
        self.available_prefetch
    }

    pub const fn total_prefetch(&self) -> u64 {
        self.total_prefetch
    }

    pub const fn prefetch_promoted(&self) -> bool {
        self.prefetch_promoted
    }

    /// Compact per-layer facts for this exact coverage snapshot. Iteration is
    /// proportional to visible layers, not to the requirement body.
    pub fn layer_coverages(&self) -> impl ExactSizeIterator<Item = FrameLayerCoverage> + '_ {
        self.layers.iter().map(|layer| {
            let available_target = layer.available_target_required
                + if self.prefetch_promoted {
                    layer.available_target_prefetch
                } else {
                    0
                };
            let total_target = layer.total_target_required
                + if self.prefetch_promoted {
                    layer.total_target_prefetch
                } else {
                    0
                };
            let scale = if layer.fallback_scale.is_none()
                || (total_target != 0 && available_target == total_target)
            {
                Some(layer.target_scale)
            } else if available_target == 0 && layer.finest_fallback_scale == layer.fallback_scale {
                layer.fallback_scale
            } else {
                None
            };
            FrameLayerCoverage {
                layer: layer.layer,
                scale,
                target_scale: layer.target_scale,
                finest_fallback_scale: layer.finest_fallback_scale,
                fallback_scale: layer.fallback_scale,
                available_target_requirements: available_target,
                total_target_requirements: total_target,
                available_requirements: layer.available_required
                    + if self.prefetch_promoted {
                        layer.available_prefetch
                    } else {
                        0
                    },
                total_requirements: layer.total_required
                    + if self.prefetch_promoted {
                        layer.total_prefetch
                    } else {
                        0
                    },
            }
        })
    }

    pub const fn is_first_useful(&self) -> bool {
        self.available_first_useful == self.total_first_useful
    }

    pub const fn is_full(&self) -> bool {
        self.is_first_useful() && self.available_refinement() == self.total_refinement()
    }

    pub fn fraction(&self) -> f64 {
        self.available_requirements() as f64 / self.total_requirements() as f64
    }

    pub fn rebind(&self, requirements: &RenderRequirements) -> Result<Self, RenderApiError> {
        if !Arc::ptr_eq(&self.requirements, &requirements.set.resources) {
            return Err(RenderApiError::CoveredResourceNotRequired);
        }
        Ok(Self {
            frame: requirements.frame(),
            requirements: Arc::clone(&self.requirements),
            available_words: Arc::clone(&self.available_words),
            available_first_useful: self.available_first_useful,
            total_first_useful: self.total_first_useful,
            available_refinement: self.available_refinement,
            total_refinement: self.total_refinement,
            available_prefetch: self.available_prefetch,
            total_prefetch: self.total_prefetch,
            prefetch_promoted: requirements.set.prefetch_promoted,
            layers: Arc::clone(&self.layers),
        })
    }
}

/// Whether the presented frame is still progressive, complete with an
/// explicit limitation, or exact for its current intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameCompleteness {
    Progressive,
    Complete,
    Exact,
}

/// Why a frame cannot yet or cannot ever be exact for its current intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameLimitation {
    CoarserScale,
    BudgetLimited,
    CapacityLimited,
    MissingResources,
}

/// Truthful progressive status for one presented frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameProgress {
    coverage: FrameCoverage,
    completeness: FrameCompleteness,
    limitation: Option<FrameLimitation>,
}

impl FrameProgress {
    pub fn new(
        coverage: FrameCoverage,
        completeness: FrameCompleteness,
        limitation: Option<FrameLimitation>,
    ) -> Result<Self, RenderApiError> {
        let valid = coverage.is_first_useful()
            && match completeness {
                FrameCompleteness::Progressive => !coverage.is_full(),
                FrameCompleteness::Complete => coverage.is_full() && limitation.is_some(),
                FrameCompleteness::Exact => coverage.is_full() && limitation.is_none(),
            };
        if !valid {
            return Err(RenderApiError::InvalidFrameProgress);
        }
        Ok(Self {
            coverage,
            completeness,
            limitation,
        })
    }

    pub const fn coverage(&self) -> &FrameCoverage {
        &self.coverage
    }

    pub const fn completeness(&self) -> FrameCompleteness {
        self.completeness
    }

    pub const fn limitation(&self) -> Option<FrameLimitation> {
        self.limitation
    }

    /// Reuses an unchanged coverage bitmap for a new frame-bound requirement
    /// handle that shares the same validated semantic resource body.
    pub fn rebind(&self, requirements: &RenderRequirements) -> Result<Self, RenderApiError> {
        Self::new(
            self.coverage.rebind(requirements)?,
            self.completeness,
            self.limitation,
        )
    }
}

/// The sole categories to which large production GPU allocations are charged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuLedgerCategory {
    PayloadResidency,
    TransferStaging,
    DisplayTarget,
    ControlAndResidencyMetadata,
    Scratch,
}

/// The current renderer-owned frame facts safe to carry in an application
/// snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentedFrame {
    target: PresentationTarget,
    extent: RenderExtent,
    progress: FrameProgress,
}

impl PresentedFrame {
    pub const fn new(
        target: PresentationTarget,
        extent: RenderExtent,
        progress: FrameProgress,
    ) -> Self {
        Self {
            target,
            extent,
            progress,
        }
    }

    pub const fn target(&self) -> PresentationTarget {
        self.target
    }

    pub fn frame(&self) -> FrameIdentity {
        self.progress.coverage().frame()
    }

    pub const fn extent(&self) -> RenderExtent {
        self.extent
    }

    pub const fn progress(&self) -> &FrameProgress {
        &self.progress
    }

    /// Recorded dataset timepoint whose requirement body produced the visible
    /// frame. Keeping this derivation on the retained requirement body makes
    /// stale-frame retention observable without inventing a second identity.
    pub fn timepoint(&self) -> TimeIndex {
        self.progress.coverage().timepoint()
    }
}

/// Mode-specific scientific selection made by one volume-pick ray.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VolumePickPolicy {
    /// The nearest sample that first crosses the ISO display threshold.
    FirstThresholdHit,
    /// The nearest sample among equal raw-intensity maxima.
    MipArgmax,
    /// The nearest sample among equal maxima of
    /// `transmittance_before * sample_alpha`.
    MaximumOpacityContribution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VolumePickHitKind {
    Voxel,
    InterpolatedSample,
    Empty,
}

/// Completeness of the scientific volume result, independent of asynchronous
/// request status. A result can contain a hit and still be incomplete when a
/// missing page occurred earlier along the ray.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VolumePickCompleteness {
    Exact,
    Approximate,
    Incomplete,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VolumePickValue {
    IntensityU8(u8),
    IntensityU16(u16),
    IntensityF32(f32),
}

/// Opaque monotonic identity for one bounded asynchronous volume pick.
/// Presentation and frame facts make accidental cross-target polling visible
/// without transferring ownership of any backend resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VolumePickTicket {
    sequence: u64,
    target: PresentationTarget,
    frame: FrameIdentity,
}

impl VolumePickTicket {
    pub fn new(
        sequence: u64,
        target: PresentationTarget,
        frame: FrameIdentity,
    ) -> Result<Self, RenderApiError> {
        if sequence == 0 {
            return Err(RenderApiError::InvalidVolumePickTicket);
        }
        Ok(Self {
            sequence,
            target,
            frame,
        })
    }

    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    pub const fn target(self) -> PresentationTarget {
        self.target
    }

    pub const fn frame(self) -> FrameIdentity {
        self.frame
    }
}

/// One bounded pick bound to the exact presented resource, frame, extent,
/// timepoint, and active layer from which it was requested.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolumePickQuery {
    target: PresentationTarget,
    frame: FrameIdentity,
    extent: RenderExtent,
    timepoint: TimeIndex,
    layer: LogicalLayerKey,
    render_pixel: [f64; 2],
    policy: VolumePickPolicy,
}

impl VolumePickQuery {
    pub fn new(
        presented: &PresentedFrame,
        timepoint: TimeIndex,
        layer: LogicalLayerKey,
        render_pixel: [f64; 2],
        policy: VolumePickPolicy,
    ) -> Result<Self, RenderApiError> {
        if !render_pixel.into_iter().all(f64::is_finite) {
            return Err(RenderApiError::NonFiniteRenderPixel);
        }
        let extent = presented.extent();
        if render_pixel[0] < 0.0
            || render_pixel[1] < 0.0
            || render_pixel[0] >= f64::from(extent.width_pixels())
            || render_pixel[1] >= f64::from(extent.height_pixels())
        {
            return Err(RenderApiError::PickPixelOutsideExtent);
        }
        Ok(Self {
            target: presented.target(),
            frame: presented.frame(),
            extent,
            timepoint,
            layer,
            render_pixel: render_pixel.map(canonical_zero),
            policy,
        })
    }

    pub const fn target(self) -> PresentationTarget {
        self.target
    }

    pub const fn frame(self) -> FrameIdentity {
        self.frame
    }

    pub const fn extent(self) -> RenderExtent {
        self.extent
    }

    pub const fn timepoint(self) -> TimeIndex {
        self.timepoint
    }

    pub const fn layer(self) -> LogicalLayerKey {
        self.layer
    }

    pub const fn render_pixel(self) -> [f64; 2] {
        self.render_pixel
    }

    pub const fn policy(self) -> VolumePickPolicy {
        self.policy
    }
}

/// Small asynchronous readback payload. It contains no resource lease,
/// framebuffer copy, backend handle, or unbounded collection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolumePickResult {
    query: VolumePickQuery,
    kind: VolumePickHitKind,
    world_position: Option<WorldPoint3>,
    value: Option<VolumePickValue>,
    ray_distance_world: Option<f64>,
    completeness: VolumePickCompleteness,
}

impl VolumePickResult {
    pub const fn empty(query: VolumePickQuery, completeness: VolumePickCompleteness) -> Self {
        Self {
            query,
            kind: VolumePickHitKind::Empty,
            world_position: None,
            value: None,
            ray_distance_world: None,
            completeness,
        }
    }

    pub fn voxel(
        query: VolumePickQuery,
        world_position: WorldPoint3,
        value: VolumePickValue,
        ray_distance_world: f64,
        completeness: VolumePickCompleteness,
    ) -> Result<Self, RenderApiError> {
        Self::sample(
            query,
            VolumePickHitKind::Voxel,
            world_position,
            value,
            ray_distance_world,
            completeness,
        )
    }

    pub fn interpolated_sample(
        query: VolumePickQuery,
        world_position: WorldPoint3,
        value: VolumePickValue,
        ray_distance_world: f64,
        completeness: VolumePickCompleteness,
    ) -> Result<Self, RenderApiError> {
        Self::sample(
            query,
            VolumePickHitKind::InterpolatedSample,
            world_position,
            value,
            ray_distance_world,
            completeness,
        )
    }

    fn sample(
        query: VolumePickQuery,
        kind: VolumePickHitKind,
        world_position: WorldPoint3,
        value: VolumePickValue,
        ray_distance_world: f64,
        completeness: VolumePickCompleteness,
    ) -> Result<Self, RenderApiError> {
        let value_is_finite = match value {
            VolumePickValue::IntensityF32(value) => value.is_finite(),
            VolumePickValue::IntensityU8(_) | VolumePickValue::IntensityU16(_) => true,
        };
        if !value_is_finite || !ray_distance_world.is_finite() || ray_distance_world < 0.0 {
            return Err(RenderApiError::InvalidVolumePickResult);
        }
        Ok(Self {
            query,
            kind,
            world_position: Some(world_position),
            value: Some(value),
            ray_distance_world: Some(canonical_zero(ray_distance_world)),
            completeness,
        })
    }

    pub const fn query(self) -> VolumePickQuery {
        self.query
    }

    pub const fn kind(self) -> VolumePickHitKind {
        self.kind
    }

    pub const fn world_position(self) -> Option<WorldPoint3> {
        self.world_position
    }

    pub const fn value(self) -> Option<VolumePickValue> {
        self.value
    }

    pub const fn ray_distance_world(self) -> Option<f64> {
        self.ray_distance_world
    }

    pub const fn completeness(self) -> VolumePickCompleteness {
        self.completeness
    }
}

/// A UI-originated request to paint one fixed target in logical points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresentationPaintRequest {
    target: PresentationTarget,
    viewport: PresentationViewport,
}

impl PresentationPaintRequest {
    pub const fn new(target: PresentationTarget, viewport: PresentationViewport) -> Self {
        Self { target, viewport }
    }

    pub const fn target(self) -> PresentationTarget {
        self.target
    }

    pub const fn viewport(self) -> PresentationViewport {
        self.viewport
    }
}

/// Stable, actionable render failures with no backend strings or private paths.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RenderFault {
    #[error("no qualifying GPU device is available")]
    DeviceUnavailable,
    #[error("the active GPU device was lost")]
    DeviceLost,
    #[error(
        "GPU capacity in {category:?} cannot satisfy {requested_bytes} bytes with {available_bytes} bytes available"
    )]
    CapacityExceeded {
        category: GpuLedgerCategory,
        requested_bytes: u64,
        available_bytes: u64,
    },
    #[error("a required semantic dataset resource is unavailable")]
    ResourceUnavailable { key: BrickKey },
    #[error("the render runtime is shutting down")]
    ShuttingDown,
}

/// The logical presentation size in UI-independent screen points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresentationViewport {
    width_points: f64,
    height_points: f64,
}

impl PresentationViewport {
    const fn new_unchecked(width_points: f64, height_points: f64) -> Self {
        Self {
            width_points,
            height_points,
        }
    }

    pub fn new(width_points: f64, height_points: f64) -> Result<Self, RenderApiError> {
        if !is_finite_positive(width_points) || !is_finite_positive(height_points) {
            return Err(RenderApiError::InvalidPresentationViewport);
        }
        Ok(Self::new_unchecked(width_points, height_points))
    }

    pub const fn width_points(self) -> f64 {
        self.width_points
    }

    pub const fn height_points(self) -> f64 {
        self.height_points
    }
}

/// Orthonormal world-space axes derived from a canonical camera orientation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraAxes {
    forward: [f64; 3],
    right: [f64; 3],
    up: [f64; 3],
}

impl CameraAxes {
    pub const fn forward(self) -> [f64; 3] {
        self.forward
    }

    pub const fn right(self) -> [f64; 3] {
        self.right
    }

    pub const fn up(self) -> [f64; 3] {
        self.up
    }
}

/// A finite world-space ray with a unit-length direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewRay {
    origin: WorldPoint3,
    direction: [f64; 3],
}

/// A visible world point projected into camera-centered presentation points.
///
/// Positive `screen_x_points` points along camera right and positive
/// `screen_y_points` points along camera up. `depth_world` is the positive
/// distance along camera forward from the eye plane. Points on or behind that
/// plane are not projectable and are reported as `None`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProjectedWorldPoint {
    screen_x_points: f64,
    screen_y_points: f64,
    depth_world: f64,
}

impl ProjectedWorldPoint {
    pub const fn screen_x_points(self) -> f64 {
        self.screen_x_points
    }

    pub const fn screen_y_points(self) -> f64 {
        self.screen_y_points
    }

    pub const fn depth_world(self) -> f64 {
        self.depth_world
    }
}

impl ViewRay {
    pub const fn origin(self) -> WorldPoint3 {
        self.origin
    }

    pub const fn direction(self) -> [f64; 3] {
        self.direction
    }
}

/// Operational projection facts derived from one canonical durable view.
///
/// The canonical `CameraView` remains the authority. This value only combines
/// it with the current presentation extent and provides deterministic math.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraFrame {
    view: CameraView,
    presentation: PresentationViewport,
    axes: CameraAxes,
    eye: WorldPoint3,
}

impl CameraFrame {
    pub fn new(
        view: CameraView,
        presentation: PresentationViewport,
    ) -> Result<Self, RenderApiError> {
        let axes = axes_from_orientation(view.orientation())?;
        let target = Vec3::from_array(view.target().components());
        let eye = target.checked_sub(
            Vec3::from_array(axes.forward).checked_mul(view.perspective_view_distance_world())?,
        )?;
        Ok(Self {
            view,
            presentation,
            axes,
            eye: eye.to_world_point()?,
        })
    }

    pub const fn view(self) -> CameraView {
        self.view
    }

    pub const fn presentation(self) -> PresentationViewport {
        self.presentation
    }

    pub const fn axes(self) -> CameraAxes {
        self.axes
    }

    pub const fn eye(self) -> WorldPoint3 {
        self.eye
    }

    pub fn ray_for_screen_point(
        self,
        screen_x_points: f64,
        screen_y_points: f64,
    ) -> Result<ViewRay, RenderApiError> {
        if !screen_x_points.is_finite() || !screen_y_points.is_finite() {
            return Err(RenderApiError::NonFiniteScreenPoint);
        }

        let forward = Vec3::from_array(self.axes.forward);
        let right = Vec3::from_array(self.axes.right);
        let up = Vec3::from_array(self.axes.up);
        match self.view.projection() {
            Projection::Perspective => {
                let focal_length = self.view.perspective_focal_length_screen_points();
                let direction = forward
                    .checked_add(right.checked_mul(screen_x_points / focal_length)?)?
                    .checked_add(up.checked_mul(screen_y_points / focal_length)?)?
                    .normalized()?;
                Ok(ViewRay {
                    origin: self.eye,
                    direction: direction.0,
                })
            }
            Projection::Orthographic => {
                let scale = self.view.orthographic_world_per_screen_point();
                let origin = Vec3::from_array(self.eye.components())
                    .checked_add(right.checked_mul(screen_x_points * scale)?)?
                    .checked_add(up.checked_mul(screen_y_points * scale)?)?
                    .to_world_point()?;
                Ok(ViewRay {
                    origin,
                    direction: forward.0,
                })
            }
        }
    }

    /// Maps a physical render pixel center into presentation points before
    /// deriving its ray. Pixel coordinates may be outside the render extent so
    /// callers can deliberately evaluate border samples; they must be finite.
    pub fn ray_for_render_pixel(
        self,
        pixel_x: f64,
        pixel_y: f64,
        render_width: u32,
        render_height: u32,
    ) -> Result<ViewRay, RenderApiError> {
        if render_width == 0 || render_height == 0 {
            return Err(RenderApiError::InvalidRenderExtent);
        }
        if !pixel_x.is_finite() || !pixel_y.is_finite() {
            return Err(RenderApiError::NonFiniteRenderPixel);
        }
        let screen_x_points =
            (((pixel_x + 0.5) / f64::from(render_width)) - 0.5) * self.presentation.width_points;
        let screen_y_points =
            (0.5 - ((pixel_y + 0.5) / f64::from(render_height))) * self.presentation.height_points;
        if !screen_x_points.is_finite() || !screen_y_points.is_finite() {
            return Err(RenderApiError::CameraMathNotFinite);
        }
        self.ray_for_screen_point(screen_x_points, screen_y_points)
    }

    /// Projects a finite world point into camera-centered presentation points.
    ///
    /// The returned point is deliberately not clipped to the presentation
    /// rectangle. Overlay owners can let their UI painter clip line segments
    /// while retaining correct geometry at the viewport boundary.
    pub fn project_world_point(
        self,
        world: WorldPoint3,
    ) -> Result<Option<ProjectedWorldPoint>, RenderApiError> {
        let relative = Vec3::from_array(world.components())
            .checked_sub(Vec3::from_array(self.eye.components()))?;
        let forward = Vec3::from_array(self.axes.forward);
        let right = Vec3::from_array(self.axes.right);
        let up = Vec3::from_array(self.axes.up);
        let depth_world = relative.dot(forward)?;
        if depth_world <= 0.0 {
            return Ok(None);
        }

        let right_world = relative.dot(right)?;
        let up_world = relative.dot(up)?;
        let (screen_x_points, screen_y_points) = match self.view.projection() {
            Projection::Perspective => {
                let scale = self.view.perspective_focal_length_screen_points() / depth_world;
                (
                    checked_scalar(right_world * scale)?,
                    checked_scalar(up_world * scale)?,
                )
            }
            Projection::Orthographic => {
                let scale = self.view.orthographic_world_per_screen_point();
                (
                    checked_scalar(right_world / scale)?,
                    checked_scalar(up_world / scale)?,
                )
            }
        };
        Ok(Some(ProjectedWorldPoint {
            screen_x_points,
            screen_y_points,
            depth_world: checked_scalar(depth_world)?,
        }))
    }

    pub fn orthographic_world_span_width(self) -> Result<f64, RenderApiError> {
        checked_scalar(
            self.presentation.width_points * self.view.orthographic_world_per_screen_point(),
        )
    }

    pub fn orthographic_world_span_height(self) -> Result<f64, RenderApiError> {
        checked_scalar(
            self.presentation.height_points * self.view.orthographic_world_per_screen_point(),
        )
    }

    pub fn perspective_vertical_fov_radians(self) -> Result<f64, RenderApiError> {
        let ratio = (self.presentation.height_points * 0.5)
            / self.view.perspective_focal_length_screen_points();
        checked_scalar(2.0 * ratio.atan())
    }

    pub fn world_per_screen_point_at_target(self) -> Result<f64, RenderApiError> {
        match self.view.projection() {
            Projection::Orthographic => Ok(self.view.orthographic_world_per_screen_point()),
            Projection::Perspective => checked_scalar(
                self.view.perspective_view_distance_world()
                    / self.view.perspective_focal_length_screen_points(),
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Vec3([f64; 3]);

impl Vec3 {
    const X: Self = Self([1.0, 0.0, 0.0]);
    const Y: Self = Self([0.0, 1.0, 0.0]);
    const NEG_Z: Self = Self([0.0, 0.0, -1.0]);

    const fn from_array(value: [f64; 3]) -> Self {
        Self(value)
    }

    fn checked_add(self, other: Self) -> Result<Self, RenderApiError> {
        Self::checked([
            self.0[0] + other.0[0],
            self.0[1] + other.0[1],
            self.0[2] + other.0[2],
        ])
    }

    fn checked_sub(self, other: Self) -> Result<Self, RenderApiError> {
        Self::checked([
            self.0[0] - other.0[0],
            self.0[1] - other.0[1],
            self.0[2] - other.0[2],
        ])
    }

    fn checked_mul(self, scalar: f64) -> Result<Self, RenderApiError> {
        if !scalar.is_finite() {
            return Err(RenderApiError::CameraMathNotFinite);
        }
        Self::checked([self.0[0] * scalar, self.0[1] * scalar, self.0[2] * scalar])
    }

    fn checked(value: [f64; 3]) -> Result<Self, RenderApiError> {
        if value.iter().all(|component| component.is_finite()) {
            Ok(Self(value.map(canonical_zero)))
        } else {
            Err(RenderApiError::CameraMathNotFinite)
        }
    }

    fn cross(self, other: Self) -> Self {
        Self([
            self.0[1] * other.0[2] - self.0[2] * other.0[1],
            self.0[2] * other.0[0] - self.0[0] * other.0[2],
            self.0[0] * other.0[1] - self.0[1] * other.0[0],
        ])
    }

    fn dot(self, other: Self) -> Result<f64, RenderApiError> {
        checked_scalar(self.0[0] * other.0[0] + self.0[1] * other.0[1] + self.0[2] * other.0[2])
    }

    fn normalized(self) -> Result<Self, RenderApiError> {
        if !self.0.iter().all(|component| component.is_finite()) {
            return Err(RenderApiError::CameraMathNotFinite);
        }
        let scale = self.0.iter().map(|value| value.abs()).fold(0.0, f64::max);
        if scale == 0.0 {
            return Err(RenderApiError::DegenerateViewDirection);
        }
        let scaled = self.0.map(|value| value / scale);
        let length = scaled.iter().map(|value| value * value).sum::<f64>().sqrt();
        Self::checked(scaled.map(|value| value / length))
    }

    fn to_world_point(self) -> Result<WorldPoint3, RenderApiError> {
        WorldPoint3::new(self.0[0], self.0[1], self.0[2])
            .map_err(|_| RenderApiError::CameraMathNotFinite)
    }
}

fn axes_from_orientation(orientation: UnitQuaternion) -> Result<CameraAxes, RenderApiError> {
    let right = rotate(orientation, Vec3::X)?.normalized()?;
    let up = rotate(orientation, Vec3::Y)?.normalized()?;
    let forward = rotate(orientation, Vec3::NEG_Z)?.normalized()?;
    Ok(CameraAxes {
        forward: forward.0,
        right: right.0,
        up: up.0,
    })
}

fn rotate(quaternion: UnitQuaternion, vector: Vec3) -> Result<Vec3, RenderApiError> {
    let [x, y, z, w] = quaternion.xyzw();
    let imaginary = Vec3([x, y, z]);
    let twice_cross = imaginary.cross(vector).checked_mul(2.0)?;
    vector
        .checked_add(twice_cross.checked_mul(w)?)?
        .checked_add(imaginary.cross(twice_cross))
}

fn is_finite_positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

fn checked_scalar(value: f64) -> Result<f64, RenderApiError> {
    if value.is_finite() {
        Ok(canonical_zero(value))
    } else {
        Err(RenderApiError::CameraMathNotFinite)
    }
}

fn canonical_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

#[cfg(test)]
mod tests {
    use mirante4d_dataset::{
        CpuByteLease, CpuLedgerCategory, DatasetResourceIdentity, DatasetSourceId, ResourceRegion,
    };
    use mirante4d_domain::{
        DisplayWindow, Opacity, RgbColor, SamplingPolicy, ScaleLevel, Shape3D, TransferCurve,
    };

    use super::*;

    const EPSILON: f64 = 1.0e-12;

    #[test]
    fn canonical_frame_and_presentation_identities_have_fixed_initial_projection() {
        assert_eq!(FrameIdentity::initial(), FrameIdentity::new(0));
        assert_eq!(
            PresentationTarget::ALL.map(PresentationTarget::index),
            [0, 1, 2, 3]
        );
        assert!(!PresentationTarget::ThreeD.is_cross_section());
        assert!(
            [
                PresentationTarget::Xy,
                PresentationTarget::Xz,
                PresentationTarget::Yz
            ]
            .into_iter()
            .all(PresentationTarget::is_cross_section)
        );
    }

    #[test]
    fn shader_control_affine_rows_have_exact_values_and_typed_failures() {
        let transform = GridToWorld::from_row_major([
            1.0, 2.0, 0.0, 5.0, 0.0, 1.0, 0.0, 6.0, 0.0, 0.0, 2.0, 8.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .unwrap();
        let rows = shader_control_world_to_grid_rows(transform).unwrap();
        assert_eq!(
            rows.map(|row| row.map(f32::to_bits)),
            [
                [1.0_f32, -2.0, 0.0, 7.0].map(f32::to_bits),
                [0.0_f32, 1.0, 0.0, -6.0].map(f32::to_bits),
                [0.0_f32, 0.0, 0.5, -4.0].map(f32::to_bits),
            ]
        );

        assert_eq!(
            shader_control_world_to_grid_rows(GridToWorld::scale(1.0, 0.0, 1.0).unwrap()),
            Err(ShaderControlAffineError::NonInvertible)
        );
        let unrepresentable = GridToWorld::from_row_major([
            1.0,
            0.0,
            0.0,
            f64::MAX,
            0.0,
            1.0,
            0.0,
            0.0,
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
        assert_eq!(
            shader_control_world_to_grid_rows(unrepresentable),
            Err(ShaderControlAffineError::NotRepresentable)
        );
    }

    #[test]
    fn uniform_affine_scale_is_not_rejected_by_determinant_magnitude() {
        for scale in [1.0e-6, 2.0e6] {
            let transform = GridToWorld::scale(scale, scale, scale).unwrap();
            let rows = shader_control_world_to_grid_rows(transform).unwrap_or_else(|error| {
                panic!(
                    "a finite, uniformly scaled affine must not be rejected solely because of its determinant magnitude: scale={scale} error={error}"
                )
            });
            let expected = (1.0 / scale) as f32;
            assert_eq!(rows[0][0].to_bits(), expected.to_bits());
            assert_eq!(rows[1][1].to_bits(), expected.to_bits());
            assert_eq!(rows[2][2].to_bits(), expected.to_bits());
        }
    }

    #[test]
    fn validated_affine_has_exact_singularity_condition_and_grid_end_boundaries() {
        let maximum_shape = Shape3D::new(1, 1, MAX_EXACT_SHADER_GRID_END_EXCLUSIVE).unwrap();
        ValidatedShaderAffine::new(GridToWorld::identity(), maximum_shape)
            .expect("the exact binary32 grid-end boundary is admitted");

        let excessive_shape = Shape3D::new(1, 1, MAX_EXACT_SHADER_GRID_END_EXCLUSIVE + 1).unwrap();
        assert!(matches!(
            ValidatedShaderAffine::new(GridToWorld::identity(), excessive_shape),
            Err(ShaderAdmissionError::ShaderCoordinateEnvelopeExceeded {
                stage: ShaderEnvelopeStage::VolumeWorldToGrid,
                axis: ShaderEnvelopeAxis::Axis(Axis3::X),
                ..
            })
        ));

        let singular = GridToWorld::scale(1.0, 0.0, 1.0).unwrap();
        assert_eq!(
            ValidatedShaderAffine::new(singular, Shape3D::new(8, 8, 8).unwrap()),
            Err(ShaderAdmissionError::SingularAffine)
        );

        let ill_conditioned = GridToWorld::scale(1.0, 1.0, 1.0e-20).unwrap();
        assert!(matches!(
            ValidatedShaderAffine::new(ill_conditioned, Shape3D::new(8, 8, 8).unwrap()),
            Err(ShaderAdmissionError::AffineConditionExceeded {
                stage: AffineValidationStage::Input,
                ..
            })
        ));
    }

    #[test]
    fn precision_bound_translation_is_rejected_with_the_exact_axis() {
        let translated = GridToWorld::from_row_major([
            1.0, 0.0, 0.0, 1.0e12, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .unwrap();
        assert!(matches!(
            ValidatedShaderAffine::new(translated, Shape3D::new(64, 64, 64).unwrap()),
            Err(ShaderAdmissionError::ShaderCoordinatePrecisionExceeded { axis: Axis3::X, .. })
        ));
    }

    #[test]
    fn shader_half_step_coordinate_envelope_has_exact_boundary() {
        let maximum = MAX_EXACT_SHADER_GRID_END_EXCLUSIVE;
        let accepted = Shape3D::new(maximum, 1, 1).unwrap();
        ValidatedShaderAffine::new(GridToWorld::identity(), accepted)
            .expect("the exact half-step endpoint remains representable");

        let rejected = Shape3D::new(maximum + 1, 1, 1).unwrap();
        assert!(matches!(
            ValidatedShaderAffine::new(GridToWorld::identity(), rejected),
            Err(ShaderAdmissionError::ShaderCoordinateEnvelopeExceeded {
                stage: ShaderEnvelopeStage::VolumeWorldToGrid,
                axis: ShaderEnvelopeAxis::Axis(Axis3::Z),
                reason: ShaderEnvelopeFailure::ErrorBudget { upper_voxels: 0.5 },
            })
        ));
        validate_shader_extent(RenderExtent::new(maximum as u32, 1).unwrap())
            .expect("the physical pixel-center endpoint uses the same exact boundary");
        assert!(matches!(
            validate_shader_extent(RenderExtent::new(maximum as u32 + 1, 1).unwrap()),
            Err(ShaderAdmissionError::ShaderCoordinateEnvelopeExceeded {
                stage: ShaderEnvelopeStage::PlanePixelCenter,
                axis: ShaderEnvelopeAxis::Axis(Axis3::X),
                ..
            })
        ));
    }

    #[test]
    fn singular_condition_control_and_coordinate_failures_are_distinct() {
        let shape = Shape3D::new(8, 8, 8).unwrap();
        assert_eq!(
            ValidatedShaderAffine::new(GridToWorld::scale(1.0, 0.0, 1.0).unwrap(), shape,),
            Err(ShaderAdmissionError::SingularAffine)
        );
        assert!(matches!(
            ValidatedShaderAffine::new(GridToWorld::scale(1.0, 1.0, 1.0e-20).unwrap(), shape,),
            Err(ShaderAdmissionError::AffineConditionExceeded {
                stage: AffineValidationStage::Input,
                ..
            })
        ));
        assert!(matches!(
            ValidatedShaderAffine::new(
                GridToWorld::scale(1.0e-39, 1.0e-39, 1.0e-39).unwrap(),
                shape,
            ),
            Err(ShaderAdmissionError::ShaderControlNotFinite { .. })
        ));

        let epsilon = 1.0e-8;
        let quantized_singular = GridToWorld::from_row_major([
            (1.0 + epsilon) / epsilon,
            -1.0 / epsilon,
            0.0,
            0.0,
            -1.0 / epsilon,
            1.0 / epsilon,
            0.0,
            0.0,
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
        assert_eq!(
            ValidatedShaderAffine::new(quantized_singular, shape),
            Err(ShaderAdmissionError::ShaderQuantizedAffineSingular)
        );

        let translated = GridToWorld::from_row_major([
            1.0, 0.0, 0.0, 1.0e12, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .unwrap();
        assert!(matches!(
            ValidatedShaderAffine::new(translated, shape),
            Err(ShaderAdmissionError::ShaderCoordinatePrecisionExceeded { axis: Axis3::X, .. })
        ));

        let far = ShaderLayerAffine::new(
            LogicalLayerKey::new(0),
            ScaleLevel::BASE,
            GridToWorld::from_row_major([
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ])
            .unwrap(),
            Shape3D::new(maximum_shader_dimension_for_test(), 1, 1).unwrap(),
        )
        .unwrap();
        let shifted = ShaderLayerAffine::new(
            LogicalLayerKey::new(1),
            ScaleLevel::BASE,
            GridToWorld::scale(1_000_000.0, 1_000_000.0, 1_000_000.0).unwrap(),
            Shape3D::new(maximum_shader_dimension_for_test(), 1, 1).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            general_dvr_count_bound(&[far, shifted]),
            Err(ShaderAdmissionError::ShaderSampleCountExceeded { .. })
        ));
    }

    const fn maximum_shader_dimension_for_test() -> u64 {
        64
    }

    #[test]
    fn plane_work_envelope_retains_viewport_wide_rounding_radius() {
        let view =
            CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 0.01, 1.0)
                .unwrap();
        let presentation = PresentationViewport::new(8.0, 8.0).unwrap();
        let extent = RenderExtent::new(64, 64).unwrap();
        let footprint = ValidatedPlaneGridFootprint::new(
            view,
            presentation,
            extent,
            GridToWorld::identity(),
            Shape3D::new(64, 64, 64).unwrap(),
        )
        .expect("a small identity Plane footprint is admitted");
        assert!(
            footprint
                .grid_error_upper()
                .into_iter()
                .all(|upper| upper.is_finite() && upper < 0.5)
        );

        let ambiguous = CrossSectionView::new(
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            100_000_000.0,
            1.0,
        )
        .unwrap();
        assert!(matches!(
            ValidatedPlaneGridFootprint::new(
                ambiguous,
                presentation,
                extent,
                GridToWorld::identity(),
                Shape3D::new(64, 64, 64).unwrap(),
            ),
            Err(ShaderAdmissionError::ShaderCoordinateEnvelopeExceeded {
                stage: ShaderEnvelopeStage::PlaneGridAddress,
                reason: ShaderEnvelopeFailure::ErrorBudget { .. },
                ..
            })
        ));
    }

    fn volume_controls(direction: [f32; 3]) -> VolumeRayShaderControls {
        VolumeRayShaderControls {
            origin_base: [0.0; 3],
            origin_step_x: [0.0; 3],
            origin_step_y: [0.0; 3],
            direction_base: direction,
            direction_step_x: [0.0; 3],
            direction_step_y: [0.0; 3],
        }
    }

    #[test]
    fn volume_ray_envelope_checks_interior_direction_minimum_and_count() {
        let minimum = affine_vector_rectangle_minimum_norm(
            [-1.0, -1.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            1.0,
            1.0,
        )
        .unwrap();
        assert_eq!(minimum, 0.0, "the interior stationary point is evaluated");

        let layer = ShaderLayerAffine::new(
            LogicalLayerKey::new(0),
            ScaleLevel::BASE,
            GridToWorld::identity(),
            Shape3D::new(31, 23, 17).unwrap(),
        )
        .unwrap();
        let facts = volume_layer_facts(
            layer,
            volume_controls([0.0, 0.0, 1.0]),
            RenderExtent::new(32, 24).unwrap(),
        )
        .expect("finite axis-aligned rays have a bounded shader envelope");
        assert_eq!(facts.sample_count_upper(), 31);
        assert!(
            facts
                .grid_error_upper()
                .into_iter()
                .all(|upper| upper < 0.5)
        );
    }

    #[test]
    fn volume_page_traversal_subnormal_direction_is_portably_canonicalized_or_rejected() {
        let minimum_normal = f64::from(f32::MIN_POSITIVE);
        let world_scale = (2.0 * minimum_normal).recip();
        let small = ShaderLayerAffine::new(
            LogicalLayerKey::new(0),
            ScaleLevel::BASE,
            GridToWorld::scale(world_scale, world_scale, world_scale).unwrap(),
            Shape3D::new(1, 1, 1).unwrap(),
        )
        .unwrap();
        let accepted = volume_layer_facts(
            small,
            volume_controls([0.1, 0.0, 1.0]),
            RenderExtent::new(1, 1).unwrap(),
        )
        .expect("a discarded subnormal component below the half-voxel budget is admitted");
        assert!(accepted.grid_error_upper()[0] < 0.5);

        let large = ShaderLayerAffine::new(
            LogicalLayerKey::new(0),
            ScaleLevel::BASE,
            GridToWorld::scale(world_scale, world_scale, world_scale).unwrap(),
            Shape3D::new(5, 5, 5).unwrap(),
        )
        .unwrap();
        let rejection = volume_layer_facts(
            large,
            volume_controls([0.1, 0.0, 1.0]),
            RenderExtent::new(1, 1).unwrap(),
        );
        assert!(
            matches!(
                rejection,
                Err(ShaderAdmissionError::ShaderCoordinateEnvelopeExceeded {
                    stage: ShaderEnvelopeStage::SubnormalDirection,
                    axis: ShaderEnvelopeAxis::Axis(Axis3::X),
                    reason: ShaderEnvelopeFailure::ErrorBudget { .. },
                })
            ),
            "unexpected rejection: {rejection:?}"
        );

        let all_stationary_scale = (minimum_normal * 0.5).recip();
        let all_stationary = ShaderLayerAffine::new(
            LogicalLayerKey::new(0),
            ScaleLevel::BASE,
            GridToWorld::scale(
                all_stationary_scale,
                all_stationary_scale,
                all_stationary_scale,
            )
            .unwrap(),
            Shape3D::new(1, 1, 1).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            volume_layer_facts(
                all_stationary,
                volume_controls([1.0, 1.0, 1.0]),
                RenderExtent::new(1, 1).unwrap(),
            ),
            Err(ShaderAdmissionError::ShaderCoordinateEnvelopeExceeded {
                stage: ShaderEnvelopeStage::SubnormalDirection,
                axis: ShaderEnvelopeAxis::All,
                reason: ShaderEnvelopeFailure::AllDirectionsStationary,
            })
        ));
    }

    struct TestCharge(u64);

    impl CpuByteLease for TestCharge {
        fn category(&self) -> CpuLedgerCategory {
            CpuLedgerCategory::QueuesAndResults
        }

        fn reserved_bytes(&self) -> u64 {
            self.0
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= EPSILON,
            "expected {expected}, got {actual}"
        );
    }

    fn camera(projection: Projection) -> CameraFrame {
        let view = CameraView::new(
            projection,
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            1.0,
            8.0,
            10.0,
        )
        .unwrap();
        CameraFrame::new(view, PresentationViewport::new(8.0, 8.0).unwrap()).unwrap()
    }

    #[test]
    fn render_pass_kind_is_bound_to_target_geometry() {
        assert_eq!(
            RenderViewIntent::volume(
                camera(Projection::Orthographic).view(),
                IsoLightState::attached_camera(),
            )
            .pass_kind(),
            RenderPassKind::Volume
        );
        assert_eq!(
            RenderViewIntent::cross_section(
                CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 1.0, 1.0,)
                    .unwrap(),
            )
            .pass_kind(),
            RenderPassKind::Plane
        );
    }

    fn layer(key: u32) -> LayerRenderIntent {
        LayerRenderIntent::new(
            LogicalLayerKey::new(key),
            LayerTransfer::new(
                DisplayWindow::new(0.0, 1.0).unwrap(),
                RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
                Opacity::new(1.0).unwrap(),
                TransferCurve::linear(),
                false,
            ),
            RenderState::mip(SamplingPolicy::VoxelExact),
        )
    }

    fn intent(layers: Vec<LayerRenderIntent>) -> Result<RenderIntent, RenderApiError> {
        RenderIntent::new(
            FrameIdentity::new(7),
            resource_identity(),
            TimeIndex::new(2),
            RenderViewIntent::volume(
                camera(Projection::Orthographic).view(),
                IsoLightState::attached_camera(),
            ),
            PresentationViewport::new(800.0, 600.0).unwrap(),
            RenderExtent::new(1600, 1200).unwrap(),
            layers,
        )
    }

    fn resource_key(x: u64) -> BrickKey {
        resource_key_at(3, 5, x)
    }

    fn resource_identity() -> DatasetResourceIdentity {
        DatasetResourceIdentity::ContentAddress(
            "m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
        )
    }

    fn resource_key_at(layer: u32, timepoint: u64, x: u64) -> BrickKey {
        resource_key_at_scale(layer, timepoint, ScaleLevel::new(1), x)
    }

    fn resource_key_at_scale(layer: u32, timepoint: u64, scale: ScaleLevel, x: u64) -> BrickKey {
        BrickKey::new(
            resource_identity(),
            LogicalLayerKey::new(layer),
            TimeIndex::new(timepoint),
            scale,
            ResourceRegion::new([0, 0, x], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
        )
    }

    fn requirements_intent(frame: u64) -> RenderIntent {
        RenderIntent::new(
            FrameIdentity::new(frame),
            resource_identity(),
            TimeIndex::new(5),
            RenderViewIntent::volume(
                camera(Projection::Orthographic).view(),
                IsoLightState::attached_camera(),
            ),
            PresentationViewport::new(800.0, 600.0).unwrap(),
            RenderExtent::new(1600, 1200).unwrap(),
            vec![layer(3)],
        )
        .unwrap()
    }

    fn plane_requirements_intent(frame: u64) -> RenderIntent {
        RenderIntent::new(
            FrameIdentity::new(frame),
            resource_identity(),
            TimeIndex::new(5),
            RenderViewIntent::cross_section(
                CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 1.0, 1.0)
                    .unwrap(),
            ),
            PresentationViewport::new(800.0, 600.0).unwrap(),
            RenderExtent::new(1600, 1200).unwrap(),
            vec![layer(3)],
        )
        .unwrap()
    }

    fn presented_frame(frame: u64, extent: RenderExtent) -> PresentedFrame {
        let intent = requirements_intent(frame);
        let key = resource_key_at(3, 5, 0);
        let requirements = RenderRequirements::new(
            &intent,
            vec![RenderRequirement::new(
                key,
                RenderRequirementRole::FirstUsefulFrame,
            )],
        )
        .unwrap();
        let coverage = FrameCoverage::from_available(&requirements, &[key]).unwrap();
        let progress = FrameProgress::new(coverage, FrameCompleteness::Exact, None).unwrap();
        PresentedFrame::new(PresentationTarget::ThreeD, extent, progress)
    }

    #[test]
    fn render_extent_and_intent_are_validated_and_bounded() {
        assert_eq!(
            RenderExtent::new(0, 1),
            Err(RenderApiError::InvalidRenderExtent)
        );
        assert_eq!(
            RenderExtentEnvelope::new(1920, 0),
            Err(RenderApiError::InvalidRenderExtentEnvelope)
        );
        let envelope = RenderExtentEnvelope::new(1920, 1080).unwrap();
        assert_eq!(envelope.max_width_pixels(), 1920);
        assert_eq!(envelope.max_height_pixels(), 1080);
        assert_eq!(intent(Vec::new()), Err(RenderApiError::EmptyRenderLayers));
        assert_eq!(
            intent(vec![layer(2), layer(2)]),
            Err(RenderApiError::DuplicateRenderLayer { ordinal: 2 })
        );

        let too_many = (0..=MAX_RENDER_LAYERS)
            .map(|index| layer(u32::try_from(index).unwrap()))
            .collect();
        assert_eq!(
            intent(too_many),
            Err(RenderApiError::TooManyRenderLayers {
                actual: MAX_RENDER_LAYERS + 1,
                maximum: MAX_RENDER_LAYERS,
            })
        );

        let intent = intent(vec![layer(2), layer(9)]).unwrap();
        assert_eq!(intent.frame(), FrameIdentity::new(7));
        let reframed = intent.clone().with_frame(FrameIdentity::new(8));
        assert_eq!(reframed.frame(), FrameIdentity::new(8));
        assert_eq!(reframed.layers(), intent.layers());
        assert_eq!(intent.timepoint(), TimeIndex::new(2));
        assert_eq!(intent.extent().width_pixels(), 1600);
        assert_eq!(
            intent
                .layers()
                .iter()
                .map(LayerRenderIntent::layer)
                .collect::<Vec<_>>(),
            vec![LogicalLayerKey::new(2), LogicalLayerKey::new(9)]
        );
    }

    #[test]
    fn volume_pick_query_is_bound_to_presented_frame_and_pixel_extent() {
        let presented = presented_frame(17, RenderExtent::new(640, 360).unwrap());
        let query = VolumePickQuery::new(
            &presented,
            TimeIndex::new(5),
            LogicalLayerKey::new(3),
            [639.5, 359.5],
            VolumePickPolicy::MipArgmax,
        )
        .unwrap();
        assert_eq!(query.target(), presented.target());
        assert_eq!(query.frame(), FrameIdentity::new(17));
        assert_eq!(query.extent(), RenderExtent::new(640, 360).unwrap());
        assert_eq!(query.timepoint(), TimeIndex::new(5));
        assert_eq!(query.layer(), LogicalLayerKey::new(3));
        assert_eq!(query.render_pixel(), [639.5, 359.5]);

        assert_eq!(
            VolumePickQuery::new(
                &presented,
                TimeIndex::new(5),
                LogicalLayerKey::new(3),
                [640.0, 0.0],
                VolumePickPolicy::MipArgmax,
            ),
            Err(RenderApiError::PickPixelOutsideExtent)
        );
        assert_eq!(
            VolumePickQuery::new(
                &presented,
                TimeIndex::new(5),
                LogicalLayerKey::new(3),
                [f64::NAN, 0.0],
                VolumePickPolicy::MipArgmax,
            ),
            Err(RenderApiError::NonFiniteRenderPixel)
        );
    }

    #[test]
    fn volume_pick_result_is_fixed_size_and_scientifically_explicit() {
        let presented = presented_frame(18, RenderExtent::new(64, 32).unwrap());
        let query = VolumePickQuery::new(
            &presented,
            TimeIndex::new(5),
            LogicalLayerKey::new(3),
            [12.5, 4.5],
            VolumePickPolicy::MaximumOpacityContribution,
        )
        .unwrap();
        let hit = VolumePickResult::voxel(
            query,
            WorldPoint3::new(3.0, 4.0, 5.0).unwrap(),
            VolumePickValue::IntensityU16(42),
            7.5,
            VolumePickCompleteness::Incomplete,
        )
        .unwrap();
        assert_eq!(hit.kind(), VolumePickHitKind::Voxel);
        assert_eq!(hit.world_position().unwrap().components(), [3.0, 4.0, 5.0]);
        assert_eq!(hit.value(), Some(VolumePickValue::IntensityU16(42)));
        assert_eq!(hit.ray_distance_world(), Some(7.5));
        assert_eq!(hit.completeness(), VolumePickCompleteness::Incomplete);

        let empty = VolumePickResult::empty(query, VolumePickCompleteness::Exact);
        assert_eq!(empty.kind(), VolumePickHitKind::Empty);
        assert_eq!(empty.world_position(), None);
        assert_eq!(empty.value(), None);
        assert_eq!(empty.ray_distance_world(), None);

        assert_eq!(
            VolumePickResult::voxel(
                query,
                WorldPoint3::origin(),
                VolumePickValue::IntensityF32(f32::NAN),
                0.0,
                VolumePickCompleteness::Exact,
            ),
            Err(RenderApiError::InvalidVolumePickResult)
        );
        assert_eq!(
            VolumePickResult::voxel(
                query,
                WorldPoint3::origin(),
                VolumePickValue::IntensityU8(1),
                -1.0,
                VolumePickCompleteness::Exact,
            ),
            Err(RenderApiError::InvalidVolumePickResult)
        );
    }

    #[test]
    fn requirements_use_semantic_keys_and_reject_duplicate_or_unbounded_work() {
        let intent = requirements_intent(11);
        let first =
            RenderRequirement::new(resource_key(0), RenderRequirementRole::FirstUsefulFrame);
        let refinement = RenderRequirement::new(resource_key(1), RenderRequirementRole::Refinement);
        let requirements = RenderRequirements::new(&intent, vec![first, refinement]).unwrap();
        assert_eq!(requirements.frame(), FrameIdentity::new(11));
        assert_eq!(
            requirements.resources().collect::<Vec<_>>(),
            vec![first, refinement]
        );

        assert_eq!(
            RenderRequirements::new(&intent, Vec::new()),
            Err(RenderApiError::EmptyRenderRequirements)
        );
        assert_eq!(
            RenderRequirements::new(&intent, vec![first, first]),
            Err(RenderApiError::DuplicateRenderRequirement)
        );
        assert_eq!(
            RenderRequirements::new(&intent, vec![refinement]),
            Err(RenderApiError::MissingFirstUsefulRequirement)
        );
        assert_eq!(
            RenderRequirements::new(
                &intent,
                vec![RenderRequirement::new(
                    BrickKey::new(
                        DatasetResourceIdentity::SessionLocal(DatasetSourceId::new(99)),
                        LogicalLayerKey::new(3),
                        TimeIndex::new(5),
                        ScaleLevel::new(1),
                        ResourceRegion::new([0, 0, 2], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
                    ),
                    RenderRequirementRole::FirstUsefulFrame,
                )],
            ),
            Err(RenderApiError::RequirementIdentityMismatch)
        );
        assert_eq!(
            RenderRequirements::new(
                &intent,
                vec![RenderRequirement::new(
                    resource_key_at(4, 5, 2),
                    RenderRequirementRole::FirstUsefulFrame,
                )],
            ),
            Err(RenderApiError::RequirementLayerNotInIntent { ordinal: 4 })
        );
        assert_eq!(
            RenderRequirements::new(
                &intent,
                vec![RenderRequirement::new(
                    resource_key_at(3, 6, 2),
                    RenderRequirementRole::FirstUsefulFrame,
                )],
            ),
            Err(RenderApiError::RequirementTimepointMismatch {
                expected: 5,
                actual: 6,
            })
        );

        let too_many = (0..=MAX_RENDER_REQUIREMENTS)
            .map(|index| {
                RenderRequirement::new(
                    resource_key(u64::try_from(index).unwrap()),
                    RenderRequirementRole::Refinement,
                )
            })
            .collect();
        assert_eq!(
            RenderRequirements::new(&intent, too_many),
            Err(RenderApiError::TooManyRenderRequirements {
                actual: MAX_RENDER_REQUIREMENTS + 1,
                maximum: MAX_RENDER_REQUIREMENTS,
            })
        );
    }

    #[test]
    fn prepared_requirement_authority_shares_exact_bodies_and_has_linear_host_bound() {
        let canonical: Arc<[BrickKey]> = Arc::from([resource_key(0), resource_key(1)]);
        let ranked: Arc<[BrickKey]> = Arc::from([resource_key(1), resource_key(0)]);
        let body =
            PreparedResourceBody::new(Arc::clone(&canonical), Arc::clone(&ranked), None).unwrap();
        assert!(Arc::ptr_eq(body.canonical(), &canonical));
        assert!(Arc::ptr_eq(body.ranked(), &ranked));
        assert!(body.shares_storage_with(&body.clone()));
        assert_eq!(body.resource_index(resource_key(0)), Some(0));
        assert_eq!(body.resource_index(resource_key(1)), Some(1));

        let key_bytes =
            u64::try_from((canonical.len() + ranked.len()) * std::mem::size_of::<BrickKey>())
                .unwrap();
        assert!(body.host_allocation_bytes() >= key_bytes);
        assert!(
            body.host_allocation_bytes()
                <= key_bytes
                    + u64::try_from(std::mem::size_of::<PreparedResourceBodyData>()).unwrap()
        );
        let charge: Arc<dyn CpuByteLease> = Arc::new(TestCharge(body.host_allocation_bytes()));
        body.attach_charge(Arc::clone(&charge)).unwrap();
        assert_eq!(body.charged_bytes(), Some(body.host_allocation_bytes()));
        assert!(matches!(
            body.attach_charge(Arc::new(TestCharge(body.host_allocation_bytes()))),
            Err(RenderApiError::PreparedRequirementChargeAlreadyAttached)
        ));

        let prepared = PreparedRenderRequirements::new(
            resource_identity(),
            TimeIndex::new(5),
            vec![LogicalLayerKey::new(3)],
            body.clone(),
            1,
        )
        .unwrap();
        let bound = prepared.bind(&requirements_intent(12)).unwrap();
        assert!(prepared.shares_resources_with(&bound));
        assert!(bound.prepared_body().shares_storage_with(&body));
        let requirements = bound.resources().collect::<Vec<_>>();
        assert_eq!(requirements[0].key(), resource_key(0));
        assert_eq!(requirements[0].role(), RenderRequirementRole::Refinement);
        assert_eq!(requirements[1].key(), resource_key(1));
        assert_eq!(
            requirements[1].role(),
            RenderRequirementRole::FirstUsefulFrame
        );
    }

    #[test]
    fn navigation_prefetch_is_resident_but_not_required_until_o1_promotion() {
        let first = resource_key(0);
        let refinement = resource_key(1);
        let guard = resource_key(2);
        let body = PreparedResourceBody::new(
            Arc::from([first, refinement, guard]),
            Arc::from([first, refinement, guard]),
            None,
        )
        .unwrap();
        let prepared = PreparedRenderRequirements::new_with_required_prefix(
            resource_identity(),
            TimeIndex::new(5),
            vec![LogicalLayerKey::new(3)],
            body,
            1,
            2,
        )
        .unwrap();
        assert_eq!(prepared.required_prefix_len(), 2);
        assert_eq!(prepared.prefetch_resource_count(), 1);

        let current = prepared.bind(&requirements_intent(20)).unwrap();
        assert_eq!(
            current.requirement(2).unwrap().role(),
            RenderRequirementRole::Prefetch
        );
        let current_coverage =
            FrameCoverage::from_available(&current, &[first, refinement]).unwrap();
        assert!(current_coverage.is_full());
        assert_eq!(current_coverage.fraction(), 1.0);
        assert_eq!(current_coverage.available_prefetch(), 0);
        assert_eq!(current_coverage.total_prefetch(), 1);
        let current_layer = current_coverage.layer_coverages().next().unwrap();
        assert_eq!(current_layer.layer(), LogicalLayerKey::new(3));
        assert_eq!(current_layer.scale(), Some(ScaleLevel::new(1)));
        assert_eq!(current_layer.available_requirements(), 2);
        assert_eq!(current_layer.total_requirements(), 2);

        let promoted_prepared = prepared.promote_prefetch();
        assert_eq!(promoted_prepared.required_prefix_len(), 3);
        let promoted = promoted_prepared.bind(&requirements_intent(21)).unwrap();
        assert!(current.shares_resources_with(&promoted));
        assert_eq!(
            promoted.requirement(2).unwrap().role(),
            RenderRequirementRole::Refinement
        );
        let promoted_coverage = current_coverage.rebind(&promoted).unwrap();
        assert!(!promoted_coverage.is_full());
        assert_eq!(promoted_coverage.available_requirements(), 2);
        assert_eq!(promoted_coverage.total_requirements(), 3);
        let promoted_layer = promoted_coverage.layer_coverages().next().unwrap();
        assert_eq!(promoted_layer.available_requirements(), 2);
        assert_eq!(promoted_layer.total_requirements(), 3);

        let full = promoted_coverage
            .with_availability_changes(&promoted, &[(guard, true)])
            .unwrap();
        assert!(full.is_full());
        assert_eq!(full.fraction(), 1.0);
        assert_eq!(
            full.layer_coverages()
                .next()
                .unwrap()
                .available_requirements(),
            3
        );
    }

    #[test]
    fn dormant_multiscale_residency_suffix_never_changes_uniform_volume_coverage() {
        let layer = LogicalLayerKey::new(3);
        let terminal = resource_key_at_scale(3, 5, ScaleLevel::new(3), 0);
        let finer = resource_key_at_scale(3, 5, ScaleLevel::new(2), 0);
        let body = PreparedResourceBody::new(
            Arc::from([finer, terminal]),
            Arc::from([terminal, finer]),
            None,
        )
        .unwrap();
        let chain = RenderLayerScaleChain::new(layer, vec![ScaleLevel::new(3)]).unwrap();
        assert_eq!(
            PreparedRenderRequirements::new_with_required_prefix_and_scale_chains(
                resource_identity(),
                TimeIndex::new(5),
                vec![layer],
                vec![chain.clone()],
                body.clone(),
                1,
                1,
            ),
            Err(RenderApiError::InvalidRenderScaleChain),
            "ordinary promotable prefetch must remain inside its render chain"
        );

        let prepared =
            PreparedRenderRequirements::new_with_dormant_residency_suffix_and_scale_chains(
                resource_identity(),
                TimeIndex::new(5),
                vec![layer],
                vec![chain],
                body,
                1,
                1,
            )
            .unwrap();
        let bound = prepared.bind(&requirements_intent(31)).unwrap();
        assert_eq!(
            bound.scale_chain(layer).unwrap().target(),
            ScaleLevel::new(3)
        );
        assert_eq!(
            bound
                .requirement(bound.resource_index(finer).unwrap())
                .unwrap()
                .role(),
            RenderRequirementRole::Prefetch
        );
        let coverage = FrameCoverage::from_available(&bound, &[terminal]).unwrap();
        assert!(coverage.is_full());
        assert_eq!(
            coverage.layer_coverages().next().unwrap().scale(),
            Some(ScaleLevel::new(3))
        );

        let promoted = prepared.promote_prefetch();
        assert!(!promoted.prefetch_promoted());
        assert_eq!(promoted.required_prefix_len(), 1);
        let rebound = promoted.bind(&requirements_intent(32)).unwrap();
        assert_eq!(
            rebound
                .requirement(rebound.resource_index(finer).unwrap())
                .unwrap()
                .role(),
            RenderRequirementRole::Prefetch
        );
    }

    #[test]
    fn multiscale_layer_coverage_distinguishes_fallback_mixed_and_target() {
        let layer = LogicalLayerKey::new(3);
        let target_a = resource_key_at_scale(3, 5, ScaleLevel::new(1), 0);
        let target_b = resource_key_at_scale(3, 5, ScaleLevel::new(1), 1);
        let floor = resource_key_at_scale(3, 5, ScaleLevel::new(3), 0);
        let body = PreparedResourceBody::new(
            Arc::from([target_a, target_b, floor]),
            Arc::from([floor, target_a, target_b]),
            None,
        )
        .unwrap();
        let prepared = PreparedRenderRequirements::new_with_required_prefix_and_scale_chains(
            resource_identity(),
            TimeIndex::new(5),
            vec![layer],
            vec![
                RenderLayerScaleChain::new(
                    layer,
                    vec![ScaleLevel::new(1), ScaleLevel::new(2), ScaleLevel::new(3)],
                )
                .unwrap(),
            ],
            body.clone(),
            1,
            3,
        )
        .unwrap();
        assert_eq!(
            prepared.bind(&requirements_intent(22)),
            Err(RenderApiError::InvalidRenderScaleChain)
        );
        let requirements = prepared.bind(&plane_requirements_intent(22)).unwrap();

        let fallback = FrameCoverage::from_available(&requirements, &[floor]).unwrap();
        let fallback_layer = fallback.layer_coverages().next().unwrap();
        assert_eq!(fallback_layer.target_scale(), ScaleLevel::new(1));
        assert_eq!(
            fallback_layer.finest_fallback_scale(),
            Some(ScaleLevel::new(2))
        );
        assert_eq!(fallback_layer.fallback_scale(), Some(ScaleLevel::new(3)));
        assert_eq!(fallback_layer.scale(), None);
        assert_eq!(
            fallback_layer.fallback_range(),
            Some((ScaleLevel::new(2), ScaleLevel::new(3)))
        );
        assert!(!fallback_layer.is_mixed());
        assert_eq!(fallback_layer.available_target_requirements(), 0);
        assert_eq!(fallback_layer.total_target_requirements(), 2);
        assert!(fallback.is_first_useful());
        assert!(!fallback.is_full());

        let mixed = fallback
            .with_availability_changes(&requirements, &[(target_a, true)])
            .unwrap();
        let mixed_layer = mixed.layer_coverages().next().unwrap();
        assert_eq!(mixed_layer.scale(), None);
        assert!(mixed_layer.is_mixed());
        assert_eq!(mixed_layer.available_target_requirements(), 1);
        assert_eq!(mixed_layer.total_target_requirements(), 2);

        let target = mixed
            .with_availability_changes(&requirements, &[(target_b, true)])
            .unwrap();
        let target_layer = target.layer_coverages().next().unwrap();
        assert_eq!(target_layer.scale(), Some(ScaleLevel::new(1)));
        assert!(!target_layer.is_mixed());
        assert_eq!(target_layer.available_target_requirements(), 2);
        assert!(target.is_full());

        let single_fallback =
            PreparedRenderRequirements::new_with_required_prefix_and_scale_chains(
                resource_identity(),
                TimeIndex::new(5),
                vec![layer],
                vec![
                    RenderLayerScaleChain::new(layer, vec![ScaleLevel::new(1), ScaleLevel::new(3)])
                        .unwrap(),
                ],
                body,
                1,
                3,
            )
            .unwrap()
            .bind(&plane_requirements_intent(23))
            .unwrap();
        let uniform_floor = FrameCoverage::from_available(&single_fallback, &[floor]).unwrap();
        let uniform_floor_layer = uniform_floor.layer_coverages().next().unwrap();
        assert_eq!(uniform_floor_layer.scale(), Some(ScaleLevel::new(3)));
        assert_eq!(uniform_floor_layer.fallback_range(), None);
    }

    #[test]
    fn scale_chains_are_target_first_strict_and_cover_every_layer() {
        let layer = LogicalLayerKey::new(3);
        assert_eq!(
            RenderLayerScaleChain::new(layer, vec![ScaleLevel::new(1), ScaleLevel::new(1)]),
            Err(RenderApiError::InvalidRenderScaleChain)
        );
        assert_eq!(
            RenderLayerScaleChain::new(layer, vec![ScaleLevel::new(2), ScaleLevel::new(1)]),
            Err(RenderApiError::InvalidRenderScaleChain)
        );

        let key = resource_key_at_scale(3, 5, ScaleLevel::new(1), 0);
        let body = PreparedResourceBody::new(Arc::from([key]), Arc::from([key]), None).unwrap();
        assert_eq!(
            PreparedRenderRequirements::new_with_required_prefix_and_scale_chains(
                resource_identity(),
                TimeIndex::new(5),
                vec![layer],
                Vec::new(),
                body,
                1,
                1,
            ),
            Err(RenderApiError::InvalidRenderScaleChain)
        );
    }

    #[test]
    fn progressive_frame_status_cannot_claim_uncovered_work_is_exact() {
        let first_a = resource_key(0);
        let first_b = resource_key(1);
        let refinement_a = resource_key(2);
        let refinement_b = resource_key(3);
        let intent = requirements_intent(12);
        let requirements = RenderRequirements::new(
            &intent,
            vec![
                RenderRequirement::new(first_a, RenderRequirementRole::FirstUsefulFrame),
                RenderRequirement::new(first_b, RenderRequirementRole::FirstUsefulFrame),
                RenderRequirement::new(refinement_a, RenderRequirementRole::Refinement),
                RenderRequirement::new(refinement_b, RenderRequirementRole::Refinement),
            ],
        )
        .unwrap();

        assert_eq!(
            FrameCoverage::from_available(&requirements, &[first_a, first_a]),
            Err(RenderApiError::DuplicateCoveredResource)
        );
        assert_eq!(
            FrameCoverage::from_available(
                &requirements,
                &[
                    first_a,
                    first_b,
                    refinement_a,
                    refinement_b,
                    resource_key(99)
                ],
            ),
            Err(RenderApiError::TooManyCoveredResources {
                actual: 5,
                maximum: 4,
            })
        );
        assert_eq!(
            FrameCoverage::from_available(&requirements, &[resource_key(99)]),
            Err(RenderApiError::CoveredResourceNotRequired)
        );

        let before_first_useful =
            FrameCoverage::from_available(&requirements, &[first_a, refinement_a]).unwrap();
        assert_eq!(before_first_useful.available_first_useful(), 1);
        assert_eq!(before_first_useful.total_first_useful(), 2);
        assert_eq!(
            FrameProgress::new(before_first_useful, FrameCompleteness::Progressive, None,),
            Err(RenderApiError::InvalidFrameProgress)
        );

        let partial =
            FrameCoverage::from_available(&requirements, &[first_a, first_b, refinement_a])
                .unwrap();
        let full = FrameCoverage::from_available(
            &requirements,
            &[first_a, first_b, refinement_a, refinement_b],
        )
        .unwrap();
        assert_eq!(partial.frame(), FrameIdentity::new(12));
        assert_eq!(partial.available_first_useful(), 2);
        assert_eq!(partial.available_refinement(), 1);
        assert_eq!(partial.total_refinement(), 2);
        assert_eq!(partial.fraction(), 0.75);
        assert!(partial.is_first_useful());
        assert!(!partial.is_full());
        assert_eq!(
            FrameProgress::new(partial.clone(), FrameCompleteness::Exact, None),
            Err(RenderApiError::InvalidFrameProgress)
        );
        assert_eq!(
            FrameProgress::new(full.clone(), FrameCompleteness::Complete, None),
            Err(RenderApiError::InvalidFrameProgress)
        );

        assert!(FrameProgress::new(partial, FrameCompleteness::Progressive, None).is_ok());
        assert!(
            FrameProgress::new(
                full.clone(),
                FrameCompleteness::Complete,
                Some(FrameLimitation::CoarserScale),
            )
            .is_ok()
        );
        assert!(FrameProgress::new(full, FrameCompleteness::Exact, None).is_ok());
    }

    #[test]
    fn volume_pick_ticket_rejects_zero_and_preserves_frame_identity() {
        let target = PresentationTarget::ThreeD;
        let frame = FrameIdentity::new(23);
        assert_eq!(
            VolumePickTicket::new(0, target, frame),
            Err(RenderApiError::InvalidVolumePickTicket)
        );

        let ticket = VolumePickTicket::new(41, target, frame).unwrap();
        assert_eq!(ticket.sequence(), 41);
        assert_eq!(ticket.target(), target);
        assert_eq!(ticket.frame(), frame);
    }

    #[test]
    fn render_faults_are_typed_and_backend_neutral() {
        assert!(matches!(
            RenderFault::CapacityExceeded {
                category: GpuLedgerCategory::PayloadResidency,
                requested_bytes: 8,
                available_bytes: 4,
            },
            RenderFault::CapacityExceeded {
                category: GpuLedgerCategory::PayloadResidency,
                requested_bytes: 8,
                available_bytes: 4,
            }
        ));
        assert_eq!(
            RenderFault::ResourceUnavailable {
                key: resource_key(9)
            },
            RenderFault::ResourceUnavailable {
                key: resource_key(9)
            }
        );
    }

    #[test]
    fn presentation_viewport_rejects_nonpositive_or_nonfinite_dimensions() {
        assert_eq!(
            PresentationViewport::new(0.0, 1.0),
            Err(RenderApiError::InvalidPresentationViewport)
        );
        assert_eq!(
            PresentationViewport::new(1.0, f64::NAN),
            Err(RenderApiError::InvalidPresentationViewport)
        );
        assert_eq!(
            PresentationViewport::new(f64::INFINITY, 1.0),
            Err(RenderApiError::InvalidPresentationViewport)
        );
    }

    #[test]
    fn canonical_identity_orientation_defines_expected_axes_and_eye() {
        let camera = camera(Projection::Orthographic);
        assert_eq!(camera.axes().right(), [1.0, 0.0, 0.0]);
        assert_eq!(camera.axes().up(), [0.0, 1.0, 0.0]);
        assert_eq!(camera.axes().forward(), [0.0, 0.0, -1.0]);
        assert_eq!(camera.eye().components(), [0.0, 0.0, 10.0]);
    }

    #[test]
    fn quarter_turn_camera_orientation_has_known_axes_and_eye() {
        let half_angle = std::f64::consts::FRAC_PI_4;
        let orientation =
            UnitQuaternion::new_xyzw(0.0, half_angle.sin(), 0.0, half_angle.cos()).unwrap();
        let view = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::origin(),
            orientation,
            1.0,
            8.0,
            10.0,
        )
        .unwrap();
        let camera = CameraFrame::new(view, PresentationViewport::new(8.0, 8.0).unwrap()).unwrap();

        for (actual, expected) in camera.axes().right().into_iter().zip([0.0, 0.0, -1.0]) {
            assert_close(actual, expected);
        }
        for (actual, expected) in camera.axes().up().into_iter().zip([0.0, 1.0, 0.0]) {
            assert_close(actual, expected);
        }
        for (actual, expected) in camera.axes().forward().into_iter().zip([-1.0, 0.0, 0.0]) {
            assert_close(actual, expected);
        }
        for (actual, expected) in camera.eye().components().into_iter().zip([10.0, 0.0, 0.0]) {
            assert_close(actual, expected);
        }
    }

    #[test]
    fn orthographic_rays_are_parallel_with_screen_shifted_origins() {
        let camera = camera(Projection::Orthographic);
        let center = camera.ray_for_screen_point(0.0, 0.0).unwrap();
        let corner = camera.ray_for_screen_point(4.0, 4.0).unwrap();

        assert_eq!(center.direction(), [0.0, 0.0, -1.0]);
        assert_eq!(corner.direction(), center.direction());
        assert_eq!(center.origin().components(), [0.0, 0.0, 10.0]);
        assert_eq!(corner.origin().components(), [4.0, 4.0, 10.0]);
    }

    #[test]
    fn perspective_rays_diverge_from_one_eye() {
        let camera = camera(Projection::Perspective);
        let center = camera.ray_for_screen_point(0.0, 0.0).unwrap();
        let corner = camera.ray_for_screen_point(4.0, 4.0).unwrap();

        assert_eq!(center.origin(), corner.origin());
        assert_eq!(center.direction(), [0.0, 0.0, -1.0]);
        assert_ne!(center.direction(), corner.direction());
        let direction = corner.direction();
        assert_close(
            direction
                .iter()
                .map(|component| component * component)
                .sum::<f64>(),
            1.0,
        );
    }

    #[test]
    fn render_pixel_centers_map_y_opposite_camera_up() {
        let camera = camera(Projection::Orthographic);
        let top = camera.ray_for_render_pixel(1.0, 0.0, 4, 4).unwrap();
        let bottom = camera.ray_for_render_pixel(1.0, 3.0, 4, 4).unwrap();
        assert!(top.origin().y() > bottom.origin().y());
    }

    #[test]
    fn projection_measurements_use_canonical_view_values() {
        let orthographic = camera(Projection::Orthographic);
        assert_close(orthographic.orthographic_world_span_width().unwrap(), 8.0);
        assert_close(orthographic.orthographic_world_span_height().unwrap(), 8.0);
        assert_close(
            orthographic.world_per_screen_point_at_target().unwrap(),
            1.0,
        );

        let perspective = camera(Projection::Perspective);
        assert_close(
            perspective.perspective_vertical_fov_radians().unwrap(),
            2.0 * 0.5_f64.atan(),
        );
        assert_close(
            perspective.world_per_screen_point_at_target().unwrap(),
            1.25,
        );
    }

    #[test]
    fn orthographic_world_projection_has_analytic_screen_coordinates() {
        let camera = camera(Projection::Orthographic);
        let projected = camera
            .project_world_point(WorldPoint3::new(3.0, -2.0, 4.0).unwrap())
            .unwrap()
            .unwrap();

        assert_close(projected.screen_x_points(), 3.0);
        assert_close(projected.screen_y_points(), -2.0);
        assert_close(projected.depth_world(), 6.0);
    }

    #[test]
    fn perspective_world_projection_uses_depth_and_inverts_ray_construction() {
        let camera = camera(Projection::Perspective);
        let projected = camera
            .project_world_point(WorldPoint3::new(2.0, -1.0, 5.0).unwrap())
            .unwrap()
            .unwrap();

        // The identity camera eye is [0, 0, 10], focal length is 8 points,
        // and this point is five world units in front of the eye.
        assert_close(projected.screen_x_points(), 3.2);
        assert_close(projected.screen_y_points(), -1.6);
        assert_close(projected.depth_world(), 5.0);

        let ray = camera
            .ray_for_screen_point(projected.screen_x_points(), projected.screen_y_points())
            .unwrap();
        let from_eye = [2.0, -1.0, -5.0];
        let length = from_eye
            .iter()
            .map(|component| component * component)
            .sum::<f64>()
            .sqrt();
        for (actual, expected) in ray
            .direction()
            .into_iter()
            .zip(from_eye.map(|component| component / length))
        {
            assert_close(actual, expected);
        }
    }

    #[test]
    fn projection_rejects_the_eye_plane_and_points_behind_it() {
        for projection in [Projection::Orthographic, Projection::Perspective] {
            let camera = camera(projection);
            assert_eq!(
                camera
                    .project_world_point(WorldPoint3::new(1.0, 1.0, 10.0).unwrap())
                    .unwrap(),
                None
            );
            assert_eq!(
                camera
                    .project_world_point(WorldPoint3::new(1.0, 1.0, 11.0).unwrap())
                    .unwrap(),
                None
            );
        }
    }

    #[test]
    fn invalid_queries_and_nonfinite_results_fail_explicitly() {
        let camera = camera(Projection::Orthographic);
        assert_eq!(
            camera.ray_for_screen_point(f64::NAN, 0.0),
            Err(RenderApiError::NonFiniteScreenPoint)
        );
        assert_eq!(
            camera.ray_for_render_pixel(0.0, 0.0, 0, 4),
            Err(RenderApiError::InvalidRenderExtent)
        );
        assert_eq!(
            camera.ray_for_render_pixel(f64::INFINITY, 0.0, 4, 4),
            Err(RenderApiError::NonFiniteRenderPixel)
        );

        let extreme = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            f64::MAX,
            1.0,
            1.0,
        )
        .unwrap();
        let extreme =
            CameraFrame::new(extreme, PresentationViewport::new(f64::MAX, 1.0).unwrap()).unwrap();
        assert_eq!(
            extreme.orthographic_world_span_width(),
            Err(RenderApiError::CameraMathNotFinite)
        );
        assert_eq!(
            extreme.ray_for_screen_point(f64::MAX, 0.0),
            Err(RenderApiError::CameraMathNotFinite)
        );
    }
}

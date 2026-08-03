//! Small deterministic CPU oracle for the product rendering contract.
//!
//! The implementation deliberately borrows runtime-issued dataset leases. It
//! owns no scheduler, payload, GPU object, filesystem access, or product path.

#![forbid(unsafe_code)]

mod exact_affine_oracle;
mod lod_oracle;
mod numerical_oracle;
mod traversal_oracle;

pub use exact_affine_oracle::{
    quantized_affine_grid_corners_are_bounded, quantized_affine_inverse_is_contained,
};

pub use lod_oracle::{
    LodOracleError, ProjectedAffineLodDecision, ProjectedAffineLodOracle, ProjectedScaleFacts,
};
pub use numerical_oracle::{
    NumericalColorFacts, NumericalCompositeFacts, NumericalConformanceContract,
    NumericalConformanceOracle, NumericalCrossSectionFacts, NumericalCrossSectionPlane,
    NumericalDvrParameters, NumericalIsoParameters, NumericalIsoShading, NumericalOracleError,
    NumericalPickCompleteness, NumericalPickFacts, NumericalPickKind, NumericalSampleFacts,
    NumericalSampleState, NumericalSampling, NumericalTapFacts, NumericalTransfer, NumericalVolume,
    NumericalVolumeFacts, NumericalVolumeMode, NumericalVolumeModeKind, NumericalVolumeQuery,
    NumericalVoxel, NumericalWorldRay,
};
pub use traversal_oracle::{
    PortableDirectionComponent, TraversalOracleError, classify_portable_direction,
    page_exit_distance_reference, segment_end_index_reference,
};

use std::collections::{BTreeMap, HashSet};

use mirante4d_dataset::{
    BrickKey, DatasetCatalog, ResourceContractError, ResourceLease, ResourcePayloadView,
};
use mirante4d_domain::{
    GridToWorld, IntensityDType, IsoLightState, IsoShadingPolicy, LayerTransfer, LogicalLayerKey,
    RenderMode, SamplingPolicy, ScaleLevel, TransferCurve,
};
use mirante4d_render_api::{CameraFrame, RenderExtent, RenderIntent, RenderViewIntent, ViewRay};
use thiserror::Error;

const MAX_REFERENCE_PIXELS: u64 = 1_920 * 1_080;
const MAX_REFERENCE_RESOURCES: usize = 128;
const MAX_REFERENCE_RAY_SAMPLES: u64 = 16_384;
const GRADIENT_LENGTH_SQUARED_EPSILON: f64 = 1.0e-6;

/// One bounded RGBA8 CPU rendering plus exact per-pixel semantic masks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceFrame {
    extent: RenderExtent,
    rgba8: Box<[u8]>,
    coverage: Box<[u8]>,
    validity: Box<[u8]>,
}

impl ReferenceFrame {
    pub const fn extent(&self) -> RenderExtent {
        self.extent
    }

    pub fn rgba8(&self) -> &[u8] {
        &self.rgba8
    }

    /// One byte per pixel: `1` means every sampled in-volume location was
    /// backed by a supplied lease; `0` means the displayed result is partial.
    pub fn coverage(&self) -> &[u8] {
        &self.coverage
    }

    /// One byte per pixel: `1` means at least one scientifically valid sample
    /// contributed (or proved an empty ISO ray); `0` means no valid data did.
    pub fn validity(&self) -> &[u8] {
        &self.validity
    }

    pub fn rgba8_pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        let index = pixel_index(self.extent, x, y)?;
        let start = index.checked_mul(4)?;
        self.rgba8.get(start..start + 4)?.try_into().ok()
    }

    pub fn pixel_is_covered(&self, x: u32, y: u32) -> Option<bool> {
        pixel_index(self.extent, x, y).map(|index| self.coverage[index] != 0)
    }

    pub fn pixel_is_valid(&self, x: u32, y: u32) -> Option<bool> {
        pixel_index(self.extent, x, y).map(|index| self.validity[index] != 0)
    }
}

/// Stable failures from the bounded CPU oracle.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ReferenceRenderError {
    #[error("reference output contains {actual} pixels, exceeding its limit of {maximum}")]
    OutputPixelLimitExceeded { actual: u64, maximum: u64 },
    #[error("reference render received {actual} leases, exceeding its limit of {maximum}")]
    ResourceLimitExceeded { actual: usize, maximum: usize },
    #[error("reference output byte length overflows the host address space")]
    OutputByteLengthOverflow,
    #[error("reference render intent names a layer absent from the catalog")]
    IntentLayerMissing { layer: LogicalLayerKey },
    #[error("reference render has no catalog scale for intent layer {layer:?}")]
    IntentLayerScaleMissing { layer: LogicalLayerKey },
    #[error("reference lease does not belong to this render intent")]
    LeaseNotInIntent { key: BrickKey },
    #[error("reference render received the same resource lease more than once")]
    DuplicateLease { key: BrickKey },
    #[error("reference render received overlapping resources at one semantic scale")]
    OverlappingLeases {
        layer: LogicalLayerKey,
        scale: ScaleLevel,
    },
    #[error("reference lease violates the catalog payload contract")]
    InvalidLease { key: BrickKey },
    #[error("reference lease contains a non-finite Float32 sample")]
    NonFiniteFloatSample { key: BrickKey, index: u64 },
    #[error("reference grid-to-world transform is singular")]
    SingularGridTransform { layer: LogicalLayerKey },
    #[error("reference camera or projection math failed")]
    CameraMath,
    #[error("one reference ray requires {actual} samples, exceeding its limit of {maximum}")]
    RaySampleLimitExceeded { actual: u64, maximum: u64 },
}

/// Stateless deterministic CPU renderer used only as an oracle and diagnostic.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReferenceRenderer;

impl ReferenceRenderer {
    pub const fn new() -> Self {
        Self
    }

    pub fn render(
        &self,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        leases: &[&dyn ResourceLease],
    ) -> Result<ReferenceFrame, ReferenceRenderError> {
        let extent = intent.extent();
        let pixel_count = u64::from(extent.width_pixels()) * u64::from(extent.height_pixels());
        if pixel_count > MAX_REFERENCE_PIXELS {
            return Err(ReferenceRenderError::OutputPixelLimitExceeded {
                actual: pixel_count,
                maximum: MAX_REFERENCE_PIXELS,
            });
        }
        if leases.len() > MAX_REFERENCE_RESOURCES {
            return Err(ReferenceRenderError::ResourceLimitExceeded {
                actual: leases.len(),
                maximum: MAX_REFERENCE_RESOURCES,
            });
        }

        let resources = validate_leases(catalog, intent, leases)?;
        let layers = prepare_layers(catalog, intent, &resources)?;
        let pixel_count = usize::try_from(pixel_count)
            .map_err(|_| ReferenceRenderError::OutputByteLengthOverflow)?;
        let rgba_len = pixel_count
            .checked_mul(4)
            .ok_or(ReferenceRenderError::OutputByteLengthOverflow)?;
        let mut rgba8 = vec![0_u8; rgba_len];
        let mut coverage = vec![1_u8; pixel_count];
        let mut validity = vec![0_u8; pixel_count];

        let volume_view = match intent.view() {
            RenderViewIntent::Volume { camera, iso_light } => {
                let camera = CameraFrame::new(camera, intent.presentation())
                    .map_err(|_| ReferenceRenderError::CameraMath)?;
                Some(PreparedVolumeView::new(camera, iso_light)?)
            }
            RenderViewIntent::CrossSection(_) => None,
        };

        for y in 0..extent.height_pixels() {
            for x in 0..extent.width_pixels() {
                let index =
                    usize::try_from(u64::from(y) * u64::from(extent.width_pixels()) + u64::from(x))
                        .map_err(|_| ReferenceRenderError::OutputByteLengthOverflow)?;
                let pixel = match intent.view() {
                    RenderViewIntent::Volume { .. } => {
                        let volume_view = volume_view.expect("volume view prepared a camera");
                        let ray = volume_view
                            .camera
                            .ray_for_render_pixel(
                                f64::from(x),
                                f64::from(y),
                                extent.width_pixels(),
                                extent.height_pixels(),
                            )
                            .map_err(|_| ReferenceRenderError::CameraMath)?;
                        render_volume_stack(&layers, ray, volume_view.detached_iso_light)?
                    }
                    RenderViewIntent::CrossSection(view) => render_cross_section_stack(
                        &layers,
                        view,
                        intent.presentation(),
                        extent,
                        x,
                        y,
                    )?,
                };
                let start = index * 4;
                rgba8[start..start + 4].copy_from_slice(&pixel.rgba8());
                coverage[index] = u8::from(pixel.covered);
                validity[index] = u8::from(pixel.valid);
            }
        }

        Ok(ReferenceFrame {
            extent,
            rgba8: rgba8.into_boxed_slice(),
            coverage: coverage.into_boxed_slice(),
            validity: validity.into_boxed_slice(),
        })
    }
}

#[derive(Clone, Copy)]
struct LeaseView<'a> {
    key: BrickKey,
    payload: ResourcePayloadView<'a>,
}

struct PreparedLayer<'a> {
    transform: InverseAffine,
    shape_xyz: [u64; 3],
    transfer: &'a LayerTransfer,
    render_state: mirante4d_domain::RenderState,
    resources: Vec<LeaseView<'a>>,
}

#[derive(Debug, Clone, Copy)]
struct PreparedVolumeView {
    camera: CameraFrame,
    /// A detached light is fixed in world space. `None` means the light stays
    /// attached to the camera and is derived from each pixel's ray direction.
    detached_iso_light: Option<[f64; 3]>,
}

impl PreparedVolumeView {
    fn new(camera: CameraFrame, iso_light: IsoLightState) -> Result<Self, ReferenceRenderError> {
        let detached_iso_light = iso_light
            .detached_screen_position()
            .map(|[screen_x, screen_y]| {
                let axes = camera.axes();
                let screen_x = f64::from(screen_x);
                let screen_y = f64::from(screen_y);
                let disc_z = (1.0 - screen_x * screen_x - screen_y * screen_y)
                    .max(0.0)
                    .sqrt();
                normalized(std::array::from_fn(|axis| {
                    axes.right()[axis] * screen_x + axes.up()[axis] * screen_y
                        - axes.forward()[axis] * disc_z
                }))
                .ok_or(ReferenceRenderError::CameraMath)
            })
            .transpose()?;
        Ok(Self {
            camera,
            detached_iso_light,
        })
    }
}

fn validate_leases<'a>(
    catalog: &DatasetCatalog,
    intent: &RenderIntent,
    leases: &'a [&'a dyn ResourceLease],
) -> Result<Vec<LeaseView<'a>>, ReferenceRenderError> {
    let intent_layers = intent
        .layers()
        .iter()
        .map(mirante4d_render_api::LayerRenderIntent::layer)
        .collect::<HashSet<_>>();
    let mut seen = HashSet::with_capacity(leases.len());
    let mut resources = Vec::with_capacity(leases.len());

    for lease in leases {
        let key = lease.key();
        if key.identity() != intent.resource_identity()
            || key.timepoint() != intent.timepoint()
            || !intent_layers.contains(&key.layer())
        {
            return Err(ReferenceRenderError::LeaseNotInIntent { key });
        }
        if !seen.insert(key) {
            return Err(ReferenceRenderError::DuplicateLease { key });
        }
        let expected = catalog
            .resource_payload_descriptor(key)
            .map_err(|_| ReferenceRenderError::InvalidLease { key })?;
        let payload = lease.payload();
        if payload.descriptor() != expected || payload.shape() != key.region().shape() {
            return Err(ReferenceRenderError::InvalidLease { key });
        }
        if payload.dtype() == IntensityDType::Float32 {
            for (index, bytes) in payload.value_bytes().chunks_exact(4).enumerate() {
                let value = f32::from_le_bytes(
                    bytes
                        .try_into()
                        .expect("Float32 payload length is a multiple of four"),
                );
                if !value.is_finite() {
                    return Err(ReferenceRenderError::NonFiniteFloatSample {
                        key,
                        index: u64::try_from(index).unwrap_or(u64::MAX),
                    });
                }
            }
        }
        resources.push(LeaseView { key, payload });
    }

    for first_index in 0..resources.len() {
        for second in &resources[first_index + 1..] {
            let first = resources[first_index];
            if first.key.layer() == second.key.layer()
                && first.key.timepoint() == second.key.timepoint()
                && first.key.scale() == second.key.scale()
                && regions_overlap(first.key, second.key)
            {
                return Err(ReferenceRenderError::OverlappingLeases {
                    layer: first.key.layer(),
                    scale: first.key.scale(),
                });
            }
        }
    }
    Ok(resources)
}

fn prepare_layers<'a>(
    catalog: &DatasetCatalog,
    intent: &'a RenderIntent,
    resources: &[LeaseView<'a>],
) -> Result<Vec<PreparedLayer<'a>>, ReferenceRenderError> {
    let selected_scales = resources
        .iter()
        .fold(BTreeMap::new(), |mut scales, resource| {
            scales
                .entry(resource.key.layer())
                .and_modify(|level: &mut ScaleLevel| {
                    if resource.key.scale() < *level {
                        *level = resource.key.scale();
                    }
                })
                .or_insert(resource.key.scale());
            scales
        });

    intent
        .layers()
        .iter()
        .map(|layer_intent| {
            let key = layer_intent.layer();
            let catalog_layer = catalog
                .layer(key)
                .ok_or(ReferenceRenderError::IntentLayerMissing { layer: key })?;
            let scale_level = selected_scales
                .get(&key)
                .copied()
                .unwrap_or(ScaleLevel::BASE);
            let scale = catalog_layer
                .scale(scale_level)
                .ok_or(ReferenceRenderError::IntentLayerScaleMissing { layer: key })?;
            let dimensions = scale.shape().dimensions();
            let transform = InverseAffine::new(scale.grid_to_world())
                .ok_or(ReferenceRenderError::SingularGridTransform { layer: key })?;
            Ok(PreparedLayer {
                transform,
                shape_xyz: [dimensions[2], dimensions[1], dimensions[0]],
                transfer: layer_intent.transfer(),
                render_state: *layer_intent.render_state(),
                resources: resources
                    .iter()
                    .copied()
                    .filter(|resource| {
                        resource.key.layer() == key && resource.key.scale() == scale_level
                    })
                    .collect(),
            })
        })
        .collect()
}

fn regions_overlap(first: BrickKey, second: BrickKey) -> bool {
    let first_start = first.region().origin();
    let first_end = first.region().end_exclusive();
    let second_start = second.region().origin();
    let second_end = second.region().end_exclusive();
    (0..3).all(|axis| first_start[axis] < second_end[axis] && second_start[axis] < first_end[axis])
}

#[derive(Debug, Clone, Copy)]
struct InverseAffine {
    inverse: [[f64; 3]; 3],
    translation: [f64; 3],
}

impl InverseAffine {
    fn new(transform: GridToWorld) -> Option<Self> {
        let m = transform.row_major();
        let a = [[m[0], m[1], m[2]], [m[4], m[5], m[6]], [m[8], m[9], m[10]]];
        let determinant = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
            - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
        if !determinant.is_finite() || determinant.abs() <= f64::MIN_POSITIVE {
            return None;
        }
        let d = determinant.recip();
        let inverse = [
            [
                (a[1][1] * a[2][2] - a[1][2] * a[2][1]) * d,
                (a[0][2] * a[2][1] - a[0][1] * a[2][2]) * d,
                (a[0][1] * a[1][2] - a[0][2] * a[1][1]) * d,
            ],
            [
                (a[1][2] * a[2][0] - a[1][0] * a[2][2]) * d,
                (a[0][0] * a[2][2] - a[0][2] * a[2][0]) * d,
                (a[0][2] * a[1][0] - a[0][0] * a[1][2]) * d,
            ],
            [
                (a[1][0] * a[2][1] - a[1][1] * a[2][0]) * d,
                (a[0][1] * a[2][0] - a[0][0] * a[2][1]) * d,
                (a[0][0] * a[1][1] - a[0][1] * a[1][0]) * d,
            ],
        ];
        if !inverse.iter().flatten().all(|value| value.is_finite()) {
            return None;
        }
        Some(Self {
            inverse,
            translation: [m[3], m[7], m[11]],
        })
    }

    fn point(self, world: [f64; 3]) -> [f64; 3] {
        self.vector([
            world[0] - self.translation[0],
            world[1] - self.translation[1],
            world[2] - self.translation[2],
        ])
    }

    fn vector(self, value: [f64; 3]) -> [f64; 3] {
        std::array::from_fn(|row| {
            self.inverse[row][0] * value[0]
                + self.inverse[row][1] * value[1]
                + self.inverse[row][2] * value[2]
        })
    }

    /// Transforms a grid-space covector into world space with the inverse
    /// transpose of the grid-to-world linear part.
    fn normal(self, value: [f64; 3]) -> [f64; 3] {
        std::array::from_fn(|column| {
            self.inverse[0][column] * value[0]
                + self.inverse[1][column] * value[1]
                + self.inverse[2][column] * value[2]
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum Sample {
    Missing,
    Invalid,
    Valid(f32),
    Outside,
}

fn sample_nearest(
    layer: &PreparedLayer<'_>,
    grid_xyz: [f64; 3],
) -> Result<Sample, ReferenceRenderError> {
    let mut xyz = [0_u64; 3];
    for axis in 0..3 {
        if grid_xyz[axis] < -0.5 || grid_xyz[axis] >= layer.shape_xyz[axis] as f64 - 0.5 {
            return Ok(Sample::Outside);
        }
        let rounded = (grid_xyz[axis] + 0.5).floor();
        xyz[axis] = rounded.clamp(0.0, layer.shape_xyz[axis].saturating_sub(1) as f64) as u64;
    }
    sample_grid_index(layer, [xyz[2], xyz[1], xyz[0]])
}

fn sample_linear(
    layer: &PreparedLayer<'_>,
    grid_xyz: [f64; 3],
) -> Result<Sample, ReferenceRenderError> {
    if (0..3)
        .any(|axis| grid_xyz[axis] < -0.5 || grid_xyz[axis] >= layer.shape_xyz[axis] as f64 - 0.5)
    {
        return Ok(Sample::Outside);
    }
    let mut lower = [0_u64; 3];
    let mut upper = [0_u64; 3];
    let mut fraction = [0.0_f64; 3];
    for axis in 0..3 {
        let coordinate = grid_xyz[axis].clamp(0.0, layer.shape_xyz[axis] as f64 - 1.0);
        lower[axis] = coordinate.floor() as u64;
        upper[axis] = (lower[axis] + 1).min(layer.shape_xyz[axis] - 1);
        fraction[axis] = coordinate - lower[axis] as f64;
    }

    let mut value = 0.0_f64;
    for dz in 0..2 {
        for dy in 0..2 {
            for dx in 0..2 {
                let weight = axis_weight(dx, fraction[0])
                    * axis_weight(dy, fraction[1])
                    * axis_weight(dz, fraction[2]);
                if weight == 0.0 {
                    continue;
                }
                let xyz = [
                    if dx == 0 { lower[0] } else { upper[0] },
                    if dy == 0 { lower[1] } else { upper[1] },
                    if dz == 0 { lower[2] } else { upper[2] },
                ];
                match sample_grid_index(layer, [xyz[2], xyz[1], xyz[0]])? {
                    Sample::Valid(sample) => value += f64::from(sample) * weight,
                    Sample::Missing => return Ok(Sample::Missing),
                    Sample::Invalid => return Ok(Sample::Invalid),
                    Sample::Outside => unreachable!("clamped interpolation indices are in bounds"),
                }
            }
        }
    }
    Ok(Sample::Valid(value as f32))
}

fn axis_weight(upper: usize, fraction: f64) -> f64 {
    if upper == 0 { 1.0 - fraction } else { fraction }
}

fn sample_grid_index(
    layer: &PreparedLayer<'_>,
    global_zyx: [u64; 3],
) -> Result<Sample, ReferenceRenderError> {
    for resource in &layer.resources {
        let origin = resource.key.region().origin();
        let end = resource.key.region().end_exclusive();
        if (0..3).all(|axis| global_zyx[axis] >= origin[axis] && global_zyx[axis] < end[axis]) {
            let local = std::array::from_fn::<_, 3, _>(|axis| global_zyx[axis] - origin[axis]);
            let shape = resource.payload.shape().dimensions();
            let index = local[0]
                .checked_mul(shape[1])
                .and_then(|value| value.checked_add(local[1]))
                .and_then(|value| value.checked_mul(shape[2]))
                .and_then(|value| value.checked_add(local[2]))
                .ok_or(ReferenceRenderError::InvalidLease { key: resource.key })?;
            let valid = resource.payload.sample_is_valid(index).map_err(
                |_reason: ResourceContractError| ReferenceRenderError::InvalidLease {
                    key: resource.key,
                },
            )?;
            if !valid {
                return Ok(Sample::Invalid);
            }
            return Ok(Sample::Valid(decode_sample(*resource, index)?));
        }
    }
    Ok(Sample::Missing)
}

fn decode_sample(resource: LeaseView<'_>, index: u64) -> Result<f32, ReferenceRenderError> {
    let width = usize::from(resource.payload.dtype().bytes_per_sample());
    let index = usize::try_from(index)
        .map_err(|_| ReferenceRenderError::InvalidLease { key: resource.key })?;
    let start = index
        .checked_mul(width)
        .ok_or(ReferenceRenderError::InvalidLease { key: resource.key })?;
    let bytes = resource
        .payload
        .value_bytes()
        .get(start..start + width)
        .ok_or(ReferenceRenderError::InvalidLease { key: resource.key })?;
    let value = match resource.payload.dtype() {
        IntensityDType::Uint8 => f32::from(bytes[0]),
        IntensityDType::Uint16 => f32::from(u16::from_le_bytes(
            bytes.try_into().expect("Uint16 sample has two bytes"),
        )),
        IntensityDType::Float32 => {
            f32::from_le_bytes(bytes.try_into().expect("Float32 sample has four bytes"))
        }
    };
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ReferenceRenderError::NonFiniteFloatSample {
            key: resource.key,
            index: u64::try_from(index).unwrap_or(u64::MAX),
        })
    }
}

fn sample(layer: &PreparedLayer<'_>, grid_xyz: [f64; 3]) -> Result<Sample, ReferenceRenderError> {
    match layer.render_state.sampling_policy() {
        SamplingPolicy::VoxelExact => sample_nearest(layer, grid_xyz),
        SamplingPolicy::SmoothLinear => sample_linear(layer, grid_xyz),
    }
}

#[derive(Debug, Clone, Copy)]
enum IsoGradient {
    Missing,
    Invalid,
    Valid([f64; 3]),
}

#[derive(Debug, Clone, Copy)]
struct IsoLightingContext {
    world_direction: [f64; 3],
    detached_iso_light: Option<[f64; 3]>,
}

fn iso_gradient(
    layer: &PreparedLayer<'_>,
    point: [f64; 3],
) -> Result<IsoGradient, ReferenceRenderError> {
    let mut taps = [Sample::Outside; 6];
    for axis in 0..3 {
        let mut negative = point;
        negative[axis] -= 1.0;
        taps[axis * 2] = sample(layer, negative)?;
        let mut positive = point;
        positive[axis] += 1.0;
        taps[axis * 2 + 1] = sample(layer, positive)?;
    }
    if taps.iter().any(|tap| matches!(tap, Sample::Missing)) {
        return Ok(IsoGradient::Missing);
    }
    if taps.iter().any(|tap| !matches!(tap, Sample::Valid(_))) {
        return Ok(IsoGradient::Invalid);
    }
    let grid_gradient = std::array::from_fn(|axis| {
        let Sample::Valid(negative) = taps[axis * 2] else {
            unreachable!("gradient taps were proved valid")
        };
        let Sample::Valid(positive) = taps[axis * 2 + 1] else {
            unreachable!("gradient taps were proved valid")
        };
        f64::from(positive) - f64::from(negative)
    });
    let world_gradient = layer.transform.normal(grid_gradient);
    Ok(IsoGradient::Valid(
        normalized(world_gradient).unwrap_or([0.0; 3]),
    ))
}

fn normalized(value: [f64; 3]) -> Option<[f64; 3]> {
    let length_squared = value
        .iter()
        .map(|component| component * component)
        .sum::<f64>();
    if !length_squared.is_finite() || length_squared <= GRADIENT_LENGTH_SQUARED_EPSILON {
        return None;
    }
    let inverse_length = length_squared.sqrt().recip();
    Some(value.map(|component| component * inverse_length))
}

fn iso_lighting(
    layer: &PreparedLayer<'_>,
    point: [f64; 3],
    context: IsoLightingContext,
) -> Result<(f32, bool), ReferenceRenderError> {
    let parameters = layer
        .render_state
        .iso_parameters()
        .expect("ISO mode exposes ISO parameters");
    if parameters.shading_policy() == IsoShadingPolicy::Flat {
        return Ok((1.0, false));
    }
    let gradient = iso_gradient(layer, point)?;
    let missing = matches!(gradient, IsoGradient::Missing);
    let IsoGradient::Valid(gradient) = gradient else {
        return Ok((0.2, missing));
    };
    let Some(gradient) = normalized(gradient) else {
        return Ok((0.2, false));
    };
    let light = match context.detached_iso_light {
        Some(light) => light,
        None => normalized(context.world_direction.map(|component| -component))
            .ok_or(ReferenceRenderError::CameraMath)?,
    };
    let cosine = gradient
        .iter()
        .zip(light)
        .map(|(gradient, light)| gradient * light)
        .sum::<f64>()
        .abs();
    Ok(((0.2 + 0.8 * cosine) as f32, false))
}

#[derive(Debug, Clone, Copy)]
struct PixelResult {
    premultiplied_rgb: [f32; 3],
    alpha: f32,
    covered: bool,
    valid: bool,
    depth: f64,
}

impl PixelResult {
    const fn transparent() -> Self {
        Self {
            premultiplied_rgb: [0.0; 3],
            alpha: 0.0,
            covered: true,
            valid: false,
            depth: f64::INFINITY,
        }
    }

    fn from_display(transfer: &LayerTransfer, display: f32, alpha: f32) -> Self {
        let alpha = alpha.clamp(0.0, 1.0);
        let color = transfer.color().rgb();
        Self {
            premultiplied_rgb: color.map(|component| component * display * alpha),
            alpha,
            covered: true,
            valid: true,
            depth: f64::INFINITY,
        }
    }

    fn composite(&mut self, over: Self) {
        let remaining = 1.0 - self.alpha;
        for channel in 0..3 {
            self.premultiplied_rgb[channel] += over.premultiplied_rgb[channel] * remaining;
        }
        self.alpha += over.alpha * remaining;
        self.covered &= over.covered;
        self.valid |= over.valid;
        self.depth = self.depth.min(over.depth);
    }

    fn add(&mut self, other: Self) {
        for channel in 0..3 {
            self.premultiplied_rgb[channel] = (self.premultiplied_rgb[channel]
                + other.premultiplied_rgb[channel])
                .clamp(0.0, 1.0);
        }
        self.alpha = 1.0 - (1.0 - self.alpha) * (1.0 - other.alpha);
        self.covered &= other.covered;
        self.valid |= other.valid;
        self.depth = self.depth.min(other.depth);
    }

    fn rgba8(self) -> [u8; 4] {
        [
            quantize(self.premultiplied_rgb[0]),
            quantize(self.premultiplied_rgb[1]),
            quantize(self.premultiplied_rgb[2]),
            quantize(self.alpha),
        ]
    }
}

fn quantize(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn render_volume_stack(
    layers: &[PreparedLayer<'_>],
    ray: ViewRay,
    detached_iso_light: Option<[f64; 3]>,
) -> Result<PixelResult, ReferenceRenderError> {
    if layers.len() == 1 {
        return render_volume_layer(&layers[0], ray, detached_iso_light);
    }

    if layers
        .iter()
        .all(|layer| layer.render_state.mode() == RenderMode::Dvr)
    {
        return render_joint_dvr(layers, ray);
    }

    if layers
        .iter()
        .all(|layer| layer.render_state.mode() == RenderMode::Mip)
    {
        let mut result = PixelResult::transparent();
        for layer in layers {
            result.add(render_volume_layer(layer, ray, detached_iso_light)?);
        }
        return Ok(result);
    }

    if layers
        .iter()
        .all(|layer| layer.render_state.mode() == RenderMode::Isosurface)
    {
        return render_iso_stack(layers, ray, detached_iso_light);
    }

    // A heterogeneous stack is an authored 2D over-composition. Grouping a
    // subset of equal modes would move that subset across its neighbors.
    let mut result = PixelResult::transparent();
    for layer in layers {
        result.composite(render_volume_layer(layer, ray, detached_iso_light)?);
    }
    Ok(result)
}

#[derive(Debug, Clone, Copy)]
struct PreparedLayerRay {
    origin: [f64; 3],
    direction: [f64; 3],
    entry: f64,
    exit: f64,
    step: f64,
}

fn prepare_layer_ray(
    layer: &PreparedLayer<'_>,
    ray: ViewRay,
) -> Result<Option<PreparedLayerRay>, ReferenceRenderError> {
    let origin = layer.transform.point(ray.origin().components());
    let mut direction = layer.transform.vector(ray.direction());
    for component in &mut direction {
        *component =
            portable_direction_component(*component).ok_or(ReferenceRenderError::CameraMath)?;
    }
    let Some((entry, exit)) = intersect_grid(origin, direction, layer.shape_xyz) else {
        return Ok(None);
    };
    let entry = entry.max(0.0);
    if exit <= entry {
        return Ok(None);
    }
    let grid_speed = direction
        .iter()
        .map(|value| value.abs())
        .fold(0.0, f64::max);
    if !grid_speed.is_finite() || grid_speed == 0.0 {
        return Err(ReferenceRenderError::CameraMath);
    }
    Ok(Some(PreparedLayerRay {
        origin,
        direction,
        entry,
        exit,
        step: grid_speed.recip(),
    }))
}

fn checked_sample_count(entry: f64, exit: f64, step: f64) -> Result<u64, ReferenceRenderError> {
    let sample_count = ((exit - entry) / step).ceil().max(1.0) as u64;
    if sample_count > MAX_REFERENCE_RAY_SAMPLES {
        return Err(ReferenceRenderError::RaySampleLimitExceeded {
            actual: sample_count,
            maximum: MAX_REFERENCE_RAY_SAMPLES,
        });
    }
    Ok(sample_count)
}

/// Integrates an all-DVR stack as one participating medium. The common world
/// step is the finest step requested by any intersected layer. At each world
/// sample, channel extinction is summed and emitted color is extinction-
/// weighted, which makes the result independent of layer enumeration.
fn render_joint_dvr(
    layers: &[PreparedLayer<'_>],
    ray: ViewRay,
) -> Result<PixelResult, ReferenceRenderError> {
    let mut prepared = Vec::with_capacity(layers.len());
    for layer in layers {
        if let Some(layer_ray) = prepare_layer_ray(layer, ray)? {
            prepared.push((layer, layer_ray));
        }
    }
    if prepared.is_empty() {
        return Ok(PixelResult::transparent());
    }

    let entry = prepared
        .iter()
        .map(|(_, ray)| ray.entry)
        .fold(f64::INFINITY, f64::min);
    let exit = prepared
        .iter()
        .map(|(_, ray)| ray.exit)
        .fold(f64::NEG_INFINITY, f64::max);
    let step = prepared
        .iter()
        .map(|(_, ray)| ray.step)
        .fold(f64::INFINITY, f64::min);
    let count = checked_sample_count(entry, exit, step)?;

    let mut result = PixelResult::transparent();
    let mut any_valid = false;
    for index in 0..count {
        let distance = entry + (index as f64 + 0.5) * step;
        let mut total_optical_depth = 0.0_f64;
        let mut optical_depth_weighted_rgb = [0.0_f64; 3];
        for (layer, layer_ray) in &prepared {
            let grid = sample_position(layer_ray.origin, layer_ray.direction, distance);
            match sample(layer, grid)? {
                Sample::Valid(value) => {
                    any_valid = true;
                    let parameters = layer
                        .render_state
                        .dvr_parameters()
                        .expect("DVR mode exposes DVR parameters");
                    let opacity_display = curve_value(
                        value,
                        parameters.opacity_transfer().window(),
                        parameters.opacity_transfer().curve(),
                        false,
                    );
                    let base_optical_depth =
                        f64::from(opacity_display) * parameters.density_scale() * step;
                    let base_alpha = 1.0 - (-base_optical_depth.max(0.0)).exp();
                    let effective_alpha = f64::from(layer.transfer.opacity().get()) * base_alpha;
                    let optical_depth = -(1.0 - effective_alpha).max(1.0e-6).ln();
                    if optical_depth <= 0.0 {
                        continue;
                    }
                    let display = f64::from(transfer_value(value, layer.transfer));
                    let color = layer.transfer.color().rgb();
                    total_optical_depth += optical_depth;
                    for (weighted, color) in optical_depth_weighted_rgb.iter_mut().zip(color) {
                        *weighted += f64::from(color) * display * optical_depth;
                    }
                }
                Sample::Missing => result.covered = false,
                Sample::Invalid | Sample::Outside => {}
            }
        }
        if total_optical_depth > 0.0 {
            let sample_alpha = (1.0 - (-total_optical_depth).exp()) as f32;
            let remaining = 1.0 - result.alpha;
            for (output, weighted) in result
                .premultiplied_rgb
                .iter_mut()
                .zip(optical_depth_weighted_rgb)
            {
                let emitted = (weighted / total_optical_depth) as f32;
                *output += remaining * emitted * sample_alpha;
            }
            result.alpha += remaining * sample_alpha;
        }
    }
    result.valid = any_valid;
    Ok(result)
}

fn render_iso_stack(
    layers: &[PreparedLayer<'_>],
    ray: ViewRay,
    detached_iso_light: Option<[f64; 3]>,
) -> Result<PixelResult, ReferenceRenderError> {
    let mut facts = PixelResult::transparent();
    let mut hits = Vec::with_capacity(layers.len());
    for layer in layers {
        let hit = render_volume_layer(layer, ray, detached_iso_light)?;
        facts.covered &= hit.covered;
        facts.valid |= hit.valid;
        if hit.alpha > 0.0 {
            hits.push(hit);
        }
    }
    hits.sort_by(|first, second| first.depth.total_cmp(&second.depth));

    let mut result = PixelResult::transparent();
    for hit in hits.into_iter().rev() {
        let mut near = hit;
        near.composite(result);
        result = near;
    }
    result.covered = facts.covered;
    result.valid = facts.valid;
    Ok(result)
}

fn render_volume_layer(
    layer: &PreparedLayer<'_>,
    ray: ViewRay,
    detached_iso_light: Option<[f64; 3]>,
) -> Result<PixelResult, ReferenceRenderError> {
    let Some(prepared) = prepare_layer_ray(layer, ray)? else {
        return Ok(PixelResult::transparent());
    };
    let sample_count = checked_sample_count(prepared.entry, prepared.exit, prepared.step)?;

    match layer.render_state.mode() {
        RenderMode::Mip => render_mip(
            layer,
            prepared.origin,
            prepared.direction,
            prepared.entry,
            prepared.step,
            sample_count,
        ),
        RenderMode::Dvr => render_dvr(
            layer,
            prepared.origin,
            prepared.direction,
            prepared.entry,
            prepared.step,
            sample_count,
        ),
        RenderMode::Isosurface => render_iso(
            layer,
            prepared.origin,
            prepared.direction,
            IsoLightingContext {
                world_direction: ray.direction(),
                detached_iso_light,
            },
            prepared.entry,
            prepared.step,
            sample_count,
        ),
    }
}

fn sample_position(origin: [f64; 3], direction: [f64; 3], distance: f64) -> [f64; 3] {
    std::array::from_fn(|axis| origin[axis] + direction[axis] * distance)
}

fn render_mip(
    layer: &PreparedLayer<'_>,
    origin: [f64; 3],
    direction: [f64; 3],
    entry: f64,
    step: f64,
    count: u64,
) -> Result<PixelResult, ReferenceRenderError> {
    let mut maximum: Option<f32> = None;
    let mut covered = true;
    for index in 0..count {
        let distance = entry + (index as f64 + 0.5) * step;
        match sample(layer, sample_position(origin, direction, distance))? {
            Sample::Valid(value) => {
                maximum = Some(maximum.map_or(value, |current| current.max(value)));
            }
            Sample::Missing => covered = false,
            Sample::Invalid | Sample::Outside => {}
        }
    }
    let Some(value) = maximum else {
        return Ok(PixelResult {
            covered,
            ..PixelResult::transparent()
        });
    };
    let mut result = PixelResult::from_display(
        layer.transfer,
        transfer_value(value, layer.transfer),
        layer.transfer.opacity().get(),
    );
    result.covered = covered;
    Ok(result)
}

fn render_dvr(
    layer: &PreparedLayer<'_>,
    origin: [f64; 3],
    direction: [f64; 3],
    entry: f64,
    step: f64,
    count: u64,
) -> Result<PixelResult, ReferenceRenderError> {
    let parameters = layer
        .render_state
        .dvr_parameters()
        .expect("DVR mode exposes DVR parameters");
    let mut result = PixelResult::transparent();
    let mut any_valid = false;
    for index in 0..count {
        let distance = entry + (index as f64 + 0.5) * step;
        match sample(layer, sample_position(origin, direction, distance))? {
            Sample::Valid(value) => {
                any_valid = true;
                let opacity_display = curve_value(
                    value,
                    parameters.opacity_transfer().window(),
                    parameters.opacity_transfer().curve(),
                    false,
                );
                let sample_alpha = (1.0
                    - (-f64::from(opacity_display) * parameters.density_scale() * step).exp())
                    as f32
                    * layer.transfer.opacity().get();
                let sample_result = PixelResult::from_display(
                    layer.transfer,
                    transfer_value(value, layer.transfer),
                    sample_alpha,
                );
                result.composite(sample_result);
            }
            Sample::Missing => result.covered = false,
            Sample::Invalid | Sample::Outside => {}
        }
    }
    result.valid = any_valid;
    Ok(result)
}

fn render_iso(
    layer: &PreparedLayer<'_>,
    origin: [f64; 3],
    direction: [f64; 3],
    lighting_context: IsoLightingContext,
    entry: f64,
    step: f64,
    count: u64,
) -> Result<PixelResult, ReferenceRenderError> {
    let parameters = layer
        .render_state
        .iso_parameters()
        .expect("ISO mode exposes ISO parameters");
    let mut covered = true;
    let mut any_valid = false;
    for index in 0..count {
        let distance = entry + (index as f64 + 0.5) * step;
        match sample(layer, sample_position(origin, direction, distance))? {
            Sample::Valid(value) => {
                any_valid = true;
                let display = transfer_value(value, layer.transfer);
                if display >= parameters.display_level() {
                    let (lighting, lighting_missing) = iso_lighting(
                        layer,
                        sample_position(origin, direction, distance),
                        lighting_context,
                    )?;
                    let mut result = PixelResult::from_display(
                        layer.transfer,
                        display * lighting,
                        layer.transfer.opacity().get(),
                    );
                    result.covered = covered && !lighting_missing;
                    result.depth = distance;
                    return Ok(result);
                }
            }
            Sample::Missing => covered = false,
            Sample::Invalid | Sample::Outside => {}
        }
    }
    Ok(PixelResult {
        covered,
        valid: any_valid,
        ..PixelResult::transparent()
    })
}

fn intersect_grid(
    origin: [f64; 3],
    direction: [f64; 3],
    shape_xyz: [u64; 3],
) -> Option<(f64, f64)> {
    let mut entry = f64::NEG_INFINITY;
    let mut exit = f64::INFINITY;
    for axis in 0..3 {
        let lower = -0.5;
        let upper = shape_xyz[axis] as f64 - 0.5;
        let direction = portable_direction_component(direction[axis])?;
        if direction == 0.0 {
            if origin[axis] < lower || origin[axis] >= upper {
                return None;
            }
            continue;
        }
        let first = (lower - origin[axis]) / direction;
        let second = (upper - origin[axis]) / direction;
        entry = entry.max(first.min(second));
        exit = exit.min(first.max(second));
        if exit <= entry {
            return None;
        }
    }
    Some((entry, exit))
}

/// Applies the same representation-level direction rule as the shared WGSL
/// traversal. This is deliberately a binary32 bit classification, not a
/// geometric epsilon: every finite normal value (including `5e-7`) remains
/// directional, while either-signed zero and subnormal values are the one
/// portable stationary representation admitted by the shader contract.
fn portable_direction_component(value: f64) -> Option<f64> {
    let value = value as f32;
    if !value.is_finite() {
        return None;
    }
    let magnitude_bits = value.to_bits() & 0x7fff_ffff;
    Some(if magnitude_bits < 0x0080_0000 {
        0.0
    } else {
        f64::from(value)
    })
}

fn render_cross_section_layer(
    layer: &PreparedLayer<'_>,
    view: mirante4d_domain::CrossSectionView,
    presentation: mirante4d_render_api::PresentationViewport,
    extent: RenderExtent,
    x: u32,
    y: u32,
) -> Result<PixelResult, ReferenceRenderError> {
    let screen_x = (((f64::from(x) + 0.5) / f64::from(extent.width_pixels())) - 0.5)
        * presentation.width_points();
    let screen_y = (0.5 - ((f64::from(y) + 0.5) / f64::from(extent.height_pixels())))
        * presentation.height_points();
    let [right, up] = cross_section_axes(view.orientation());
    let center = view.center_world().components();
    let scale = view.scale_world_per_screen_point();
    let world = std::array::from_fn(|axis| {
        center[axis] + right[axis] * screen_x * scale + up[axis] * screen_y * scale
    });
    let grid = layer.transform.point(world);
    match sample(layer, grid)? {
        Sample::Valid(value) => Ok(PixelResult::from_display(
            layer.transfer,
            transfer_value(value, layer.transfer),
            layer.transfer.opacity().get(),
        )),
        Sample::Invalid => Ok(PixelResult::transparent()),
        Sample::Missing => Ok(PixelResult {
            covered: false,
            ..PixelResult::transparent()
        }),
        Sample::Outside => Ok(PixelResult::transparent()),
    }
}

fn render_cross_section_stack(
    layers: &[PreparedLayer<'_>],
    view: mirante4d_domain::CrossSectionView,
    presentation: mirante4d_render_api::PresentationViewport,
    extent: RenderExtent,
    x: u32,
    y: u32,
) -> Result<PixelResult, ReferenceRenderError> {
    let mut result = PixelResult::transparent();
    for layer in layers {
        result.add(render_cross_section_layer(
            layer,
            view,
            presentation,
            extent,
            x,
            y,
        )?);
    }
    Ok(result)
}

fn cross_section_axes(orientation: mirante4d_domain::UnitQuaternion) -> [[f64; 3]; 2] {
    let [x, y, z, w] = orientation.xyzw();
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

fn transfer_value(value: f32, transfer: &LayerTransfer) -> f32 {
    curve_value(
        value,
        transfer.window(),
        transfer.curve(),
        transfer.invert(),
    )
}

fn curve_value(
    value: f32,
    window: mirante4d_domain::DisplayWindow,
    curve: TransferCurve,
    invert: bool,
) -> f32 {
    let mut normalized = ((value - window.low()) / (window.high() - window.low())).clamp(0.0, 1.0);
    if invert {
        normalized = 1.0 - normalized;
    }
    normalized.powf(curve.gamma_value())
}

fn pixel_index(extent: RenderExtent, x: u32, y: u32) -> Option<usize> {
    if x >= extent.width_pixels() || y >= extent.height_pixels() {
        return None;
    }
    usize::try_from(u64::from(y) * u64::from(extent.width_pixels()) + u64::from(x)).ok()
}

#[cfg(test)]
mod tests {
    use mirante4d_dataset::{
        DatasetResourceIdentity, DatasetSourceId, ResourceRegion, ResourceValidity,
    };
    use mirante4d_domain::{
        CameraView, CrossSectionView, DisplayWindow, DvrOpacityTransfer, IsoShadingPolicy, Opacity,
        Projection, RgbColor, Shape3D, UnitQuaternion, WorldPoint3,
    };
    use mirante4d_render_api::PresentationViewport;

    use super::*;

    const RED_CHANNEL_VALUES: [u8; 3] = [64; 3];
    const GREEN_CHANNEL_VALUES: [u8; 3] = [192; 3];

    #[test]
    fn traversal_direction_classification_is_binary32_representation_based() {
        for value in [5.0e-7, -5.0e-7, f64::from(f32::MIN_POSITIVE)] {
            assert_eq!(
                portable_direction_component(value),
                Some(f64::from(value as f32))
            );
        }
        for value in [
            0.0,
            -0.0,
            f64::from(f32::from_bits(0x007f_ffff)),
            f64::from(f32::from_bits(0x807f_ffff)),
        ] {
            assert_eq!(portable_direction_component(value), Some(0.0));
        }
        assert_eq!(portable_direction_component(f64::INFINITY), None);
    }

    fn transfer() -> LayerTransfer {
        LayerTransfer::new(
            DisplayWindow::new(0.0, 255.0).unwrap(),
            RgbColor::new([1.0, 0.0, 0.0]).unwrap(),
            Opacity::new(1.0).unwrap(),
            TransferCurve::linear(),
            false,
        )
    }

    fn key(shape: Shape3D) -> BrickKey {
        BrickKey::new(
            DatasetResourceIdentity::SessionLocal(DatasetSourceId::new(7)),
            LogicalLayerKey::new(0),
            mirante4d_domain::TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0; 3], shape).unwrap(),
        )
    }

    fn layer<'a>(
        transfer: &'a LayerTransfer,
        render_state: mirante4d_domain::RenderState,
        shape: Shape3D,
        values: &'a [u8],
        validity: ResourceValidity,
        bits: Option<&'a [u8]>,
        layer_shape_xyz: [u64; 3],
    ) -> PreparedLayer<'a> {
        PreparedLayer {
            transform: InverseAffine::new(GridToWorld::identity()).unwrap(),
            shape_xyz: layer_shape_xyz,
            transfer,
            render_state,
            resources: vec![LeaseView {
                key: key(shape),
                payload: ResourcePayloadView::new(
                    IntensityDType::Uint8,
                    shape,
                    validity,
                    values,
                    bits,
                )
                .unwrap(),
            }],
        }
    }

    fn colored_transfer(color: [f32; 3], opacity: f32) -> LayerTransfer {
        LayerTransfer::new(
            DisplayWindow::new(0.0, 255.0).unwrap(),
            RgbColor::new(color).unwrap(),
            Opacity::new(opacity).unwrap(),
            TransferCurve::linear(),
            false,
        )
    }

    fn center_test_ray() -> ViewRay {
        CameraFrame::new(
            CameraView::new(
                Projection::Orthographic,
                WorldPoint3::new(0.0, 0.0, 1.0).unwrap(),
                UnitQuaternion::identity(),
                1.0,
                320.0,
                10.0,
            )
            .unwrap(),
            PresentationViewport::new(1.0, 1.0).unwrap(),
        )
        .unwrap()
        .ray_for_render_pixel(0.0, 0.0, 1, 1)
        .unwrap()
    }

    fn two_channel_layers<'a>(
        red_transfer: &'a LayerTransfer,
        green_transfer: &'a LayerTransfer,
        state: mirante4d_domain::RenderState,
        translate_green_z: bool,
        reverse: bool,
    ) -> Vec<PreparedLayer<'a>> {
        let shape = Shape3D::new(3, 1, 1).unwrap();
        let red = layer(
            red_transfer,
            state,
            shape,
            &RED_CHANNEL_VALUES,
            ResourceValidity::AllValid,
            None,
            [1, 1, 3],
        );
        let mut green = layer(
            green_transfer,
            state,
            shape,
            &GREEN_CHANNEL_VALUES,
            ResourceValidity::AllValid,
            None,
            [1, 1, 3],
        );
        if translate_green_z {
            green.transform = InverseAffine::new(
                GridToWorld::from_row_major([
                    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 1.0,
                ])
                .unwrap(),
            )
            .unwrap();
        }
        if reverse {
            vec![green, red]
        } else {
            vec![red, green]
        }
    }

    #[test]
    fn mip_distinguishes_valid_zero_invalidity_and_missing_coverage() {
        let transfer = transfer();
        let layer = layer(
            &transfer,
            mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
            Shape3D::new(2, 1, 1).unwrap(),
            &[0, 255],
            ResourceValidity::BitMask,
            Some(&[0b0000_0001]),
            [1, 1, 3],
        );
        let pixel = render_mip(&layer, [0.0, 0.0, 2.5], [0.0, 0.0, -1.0], 0.0, 1.0, 3).unwrap();

        assert_eq!(pixel.rgba8(), [0, 0, 0, 255]);
        assert!(pixel.valid, "valid zero remains scientific data");
        assert!(!pixel.covered, "the absent third voxel remains missing");
    }

    #[test]
    fn dvr_and_iso_have_fixed_small_ray_hand_facts() {
        let transfer = transfer();
        let shape = Shape3D::new(3, 1, 1).unwrap();
        let dvr = layer(
            &transfer,
            mirante4d_domain::RenderState::dvr(
                SamplingPolicy::VoxelExact,
                DvrOpacityTransfer::new(
                    DisplayWindow::new(0.0, 255.0).unwrap(),
                    TransferCurve::linear(),
                ),
                1.0,
            )
            .unwrap(),
            shape,
            &[0, 128, 255],
            ResourceValidity::AllValid,
            None,
            [1, 1, 3],
        );
        let dvr_pixel = render_dvr(&dvr, [0.0, 0.0, 2.5], [0.0, 0.0, -1.0], 0.0, 1.0, 3).unwrap();
        assert!(dvr_pixel.valid && dvr_pixel.covered);
        assert_eq!(dvr_pixel.rgba8(), [180, 0, 0, 198]);

        let iso = layer(
            &transfer,
            mirante4d_domain::RenderState::iso(
                SamplingPolicy::VoxelExact,
                IsoShadingPolicy::Flat,
                1.0,
            )
            .unwrap(),
            shape,
            &[0, 128, 255],
            ResourceValidity::AllValid,
            None,
            [1, 1, 3],
        );
        let iso_pixel = render_iso(
            &iso,
            [0.0, 0.0, 2.5],
            [0.0, 0.0, -1.0],
            IsoLightingContext {
                world_direction: [0.0, 0.0, -1.0],
                detached_iso_light: None,
            },
            0.0,
            1.0,
            3,
        )
        .unwrap();
        assert_eq!(iso_pixel.rgba8(), [255, 0, 0, 255]);
        assert!(iso_pixel.valid && iso_pixel.covered);
    }

    #[test]
    fn gradient_iso_has_independent_attached_and_detached_light_hand_facts() {
        let transfer = transfer();
        let shape = Shape3D::new(3, 3, 3).unwrap();
        let mut values = Vec::with_capacity(27);
        for z in 0..3 {
            for _y in 0..3 {
                for x in 0..3 {
                    values.push(if z == 1 { [0_u8, 128, 255][x] } else { 0 });
                }
            }
        }
        let iso = layer(
            &transfer,
            mirante4d_domain::RenderState::iso(
                SamplingPolicy::VoxelExact,
                IsoShadingPolicy::GradientLighting,
                0.4,
            )
            .unwrap(),
            shape,
            &values,
            ResourceValidity::AllValid,
            None,
            [3, 3, 3],
        );
        let render = |detached_iso_light| {
            render_iso(
                &iso,
                [1.0, 1.0, 2.5],
                [0.0, 0.0, -1.0],
                IsoLightingContext {
                    world_direction: [0.0, 0.0, -1.0],
                    detached_iso_light,
                },
                0.0,
                1.0,
                3,
            )
            .unwrap()
        };

        let attached = render(None);
        let detached = render(Some([1.0, 0.0, 0.0]));
        assert_eq!(attached.rgba8(), [26, 0, 0, 255]);
        assert_eq!(detached.rgba8(), [128, 0, 0, 255]);
        assert!(attached.valid && attached.covered);
        assert!(detached.valid && detached.covered);
    }

    #[test]
    fn full_affine_inverse_and_normal_have_independent_hand_facts() {
        let transform = GridToWorld::from_row_major([
            0.0, -1.0, 0.25, 10.0, 1.0, 0.0, 0.0, 2.0, 0.2, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .unwrap();
        let inverse = InverseAffine::new(transform).unwrap();

        for (actual, expected) in inverse
            .point([8.0, 4.0, 5.4])
            .into_iter()
            .zip([2.0, 3.0, 4.0])
        {
            assert!((actual - expected).abs() <= 1.0e-12);
        }
        for (actual, expected) in inverse
            .normal([0.0, 1.0, 0.0])
            .into_iter()
            .zip([-1.0, -0.05, 0.25])
        {
            assert!((actual - expected).abs() <= 1.0e-12);
        }
    }

    #[test]
    fn cross_section_samples_the_pixel_center_without_a_volume_projection() {
        let transfer = transfer();
        let layer = layer(
            &transfer,
            mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
            Shape3D::new(1, 1, 1).unwrap(),
            &[128],
            ResourceValidity::AllValid,
            None,
            [1, 1, 1],
        );
        let view =
            CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 1.0, 1.0)
                .unwrap();
        let pixel = render_cross_section_layer(
            &layer,
            view,
            PresentationViewport::new(1.0, 1.0).unwrap(),
            RenderExtent::new(1, 1).unwrap(),
            0,
            0,
        )
        .unwrap();
        assert_eq!(pixel.rgba8(), [128, 0, 0, 255]);
        assert!(pixel.valid && pixel.covered);
    }

    #[test]
    fn nonopaque_multichannel_modes_have_order_reversal_hand_facts() {
        let red = colored_transfer([1.0, 0.0, 0.0], 0.4);
        let green = colored_transfer([0.0, 1.0, 0.0], 0.6);
        let ray = center_test_ray();

        let mip_state = mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact);
        for reverse in [false, true] {
            let layers = two_channel_layers(&red, &green, mip_state, false, reverse);
            let pixel = render_volume_stack(&layers, ray, None).unwrap();
            assert_eq!(pixel.rgba8(), [26, 115, 0, 194]);
            assert!(pixel.valid && pixel.covered);

            let section = render_cross_section_stack(
                &layers,
                CrossSectionView::new(
                    WorldPoint3::new(0.0, 0.0, 1.0).unwrap(),
                    UnitQuaternion::identity(),
                    1.0,
                    1.0,
                )
                .unwrap(),
                PresentationViewport::new(1.0, 1.0).unwrap(),
                RenderExtent::new(1, 1).unwrap(),
                0,
                0,
            )
            .unwrap();
            assert_eq!(section.rgba8(), [26, 115, 0, 194]);
            assert!(section.valid && section.covered);
        }

        let dvr_state = mirante4d_domain::RenderState::dvr(
            SamplingPolicy::VoxelExact,
            DvrOpacityTransfer::new(
                DisplayWindow::new(0.0, 255.0).unwrap(),
                TransferCurve::linear(),
            ),
            0.2,
        )
        .unwrap();
        for reverse in [false, true] {
            let layers = two_channel_layers(&red, &green, dvr_state, false, reverse);
            let pixel = render_volume_stack(&layers, ray, None).unwrap();
            assert_eq!(pixel.rgba8(), [3, 43, 0, 70]);
            assert!(pixel.valid && pixel.covered);
        }

        let iso_state = mirante4d_domain::RenderState::iso(
            SamplingPolicy::VoxelExact,
            IsoShadingPolicy::Flat,
            0.1,
        )
        .unwrap();
        for reverse in [false, true] {
            let layers = two_channel_layers(&red, &green, iso_state, true, reverse);
            let pixel = render_volume_stack(&layers, ray, None).unwrap();
            assert_eq!(pixel.rgba8(), [10, 115, 0, 194]);
            assert!(pixel.valid && pixel.covered);
        }

        let mut authored = two_channel_layers(&red, &green, mip_state, false, false);
        authored[1].render_state = iso_state;
        assert_eq!(
            render_volume_stack(&authored, ray, None).unwrap().rgba8(),
            [26, 69, 0, 194]
        );
        let mut reversed = two_channel_layers(&red, &green, mip_state, false, true);
        reversed[0].render_state = iso_state;
        assert_eq!(
            render_volume_stack(&reversed, ray, None).unwrap().rgba8(),
            [10, 115, 0, 194]
        );
    }

    #[test]
    fn dtype_decoding_uses_canonical_little_endian_values() {
        let shape = Shape3D::new(1, 1, 1).unwrap();
        let resource_key = key(shape);
        let cases = [
            (IntensityDType::Uint8, vec![7], 7.0),
            (
                IntensityDType::Uint16,
                513_u16.to_le_bytes().to_vec(),
                513.0,
            ),
            (
                IntensityDType::Float32,
                0.25_f32.to_le_bytes().to_vec(),
                0.25,
            ),
        ];
        for (dtype, bytes, expected) in cases {
            let payload =
                ResourcePayloadView::new(dtype, shape, ResourceValidity::AllValid, &bytes, None)
                    .unwrap();
            assert_eq!(
                decode_sample(
                    LeaseView {
                        key: resource_key,
                        payload,
                    },
                    0,
                )
                .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn frame_accessors_are_bounded_at_the_declared_extent() {
        let frame = ReferenceFrame {
            extent: RenderExtent::new(1, 1).unwrap(),
            rgba8: vec![1, 2, 3, 4].into_boxed_slice(),
            coverage: vec![1].into_boxed_slice(),
            validity: vec![0].into_boxed_slice(),
        };
        assert_eq!(frame.rgba8_pixel(0, 0), Some([1, 2, 3, 4]));
        assert_eq!(frame.rgba8_pixel(1, 0), None);
        assert_eq!(frame.pixel_is_covered(0, 0), Some(true));
        assert_eq!(frame.pixel_is_valid(0, 0), Some(false));
    }
}

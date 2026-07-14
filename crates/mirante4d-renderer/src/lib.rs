pub mod brick_plan;
pub mod camera_mip;
pub mod cross_section;
mod current_camera;
mod current_lease_bridge;
mod diagnostics;
pub mod gpu;
pub mod resources;
pub mod scene;
pub mod scene_render;
pub mod transfer;

use mirante4d_dataset::ResourceContractError;
use mirante4d_domain::ShapeError;
use mirante4d_format::CurrentTransformError;
use mirante4d_render_api::RenderApiError;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourcePlanCapacityKind {
    Candidates,
    Resources,
}

impl std::fmt::Display for ResourcePlanCapacityKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Candidates => "candidate",
            Self::Resources => "resource",
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RenderError {
    #[error("volume has no voxels")]
    EmptyVolume,
    #[error("viewport dimensions must be positive, got {width}x{height}")]
    InvalidViewport { width: u64, height: u64 },
    #[error("readout pixel ({x}, {y}) is outside viewport {width}x{height}")]
    InvalidReadoutPixel {
        x: u64,
        y: u64,
        width: u64,
        height: u64,
    },
    #[error("brick planner pixel stride must be positive")]
    InvalidBrickPixelStride,
    #[error("semantic planning exceeded the {kind} limit of {maximum}")]
    ResourcePlanCapacityExceeded {
        kind: ResourcePlanCapacityKind,
        maximum: usize,
    },
    #[error("{axis} dimension {value} exceeds GPU u32 limits")]
    DimensionTooLarge { axis: &'static str, value: u64 },
    #[error("invalid brick atlas metadata: {0}")]
    InvalidBrickAtlas(&'static str),
    #[error("RGBA image expects {expected} pixels for {width}x{height}, got {actual}")]
    InvalidRgbaImageBuffer {
        width: u64,
        height: u64,
        expected: usize,
        actual: usize,
    },
    #[error("invalid intensity channel composite input: {0}")]
    InvalidChannelComposite(&'static str),
    #[error("intensity frame expects {expected} pixels for {width}x{height}, got {actual}")]
    InvalidIntensityFrameBuffer {
        width: u64,
        height: u64,
        expected: usize,
        actual: usize,
    },
    #[error(
        "intensity frame coverage expects {expected} pixels for {width}x{height}, got {actual}"
    )]
    InvalidPixelCoverageBuffer {
        width: u64,
        height: u64,
        expected: usize,
        actual: usize,
    },
    #[error("intensity frame coverage mask value at index {index} must be 0 or 1, got {value}")]
    InvalidPixelCoverageValue { index: usize, value: u8 },
    #[error(
        "intensity frame lighting expects {expected} pixels for {width}x{height}, got {actual}"
    )]
    InvalidPixelLightingBuffer {
        width: u64,
        height: u64,
        expected: usize,
        actual: usize,
    },
    #[error(
        "ISO surface frame {field} expects {expected} pixels for {width}x{height}, got {actual}"
    )]
    InvalidIsoSurfaceFrameBuffer {
        field: &'static str,
        width: u64,
        height: u64,
        expected: usize,
        actual: usize,
    },
    #[error(
        "ISO surface frame dimensions {surface_width}x{surface_height} do not match intensity frame {width}x{height}"
    )]
    InvalidIsoSurfaceFrameDimensions {
        width: u64,
        height: u64,
        surface_width: u64,
        surface_height: u64,
    },
    #[error("DVR RGBA frame expects {expected} pixels for {width}x{height}, got {actual}")]
    InvalidDvrRgbaFrameBuffer {
        width: u64,
        height: u64,
        expected: usize,
        actual: usize,
    },
    #[error(
        "DVR RGBA frame dimensions {dvr_width}x{dvr_height} do not match intensity frame {width}x{height}"
    )]
    InvalidDvrRgbaFrameDimensions {
        width: u64,
        height: u64,
        dvr_width: u64,
        dvr_height: u64,
    },
    #[error("invalid DVR channel set: {0}")]
    InvalidDvrChannelSet(&'static str),
    #[error("invalid intensity summary region: {0}")]
    InvalidIntensitySummaryRegion(&'static str),
    #[error("renderer resource identity mismatch: {0}")]
    ResourceIdentityMismatch(&'static str),
    #[error(
        "{kind} resource id must contain only ASCII letters, digits, '-' or '_', got {value:?}"
    )]
    InvalidResourceId { kind: &'static str, value: String },
    #[error(transparent)]
    Shape(#[from] ShapeError),
    #[error(transparent)]
    ResourceContract(#[from] ResourceContractError),
    #[error(transparent)]
    Space(#[from] CurrentTransformError),
    #[error(transparent)]
    Camera(#[from] RenderApiError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderViewport {
    pub width: u64,
    pub height: u64,
}

impl RenderViewport {
    pub fn new(width: u64, height: u64) -> Result<Self, RenderError> {
        if width == 0 || height == 0 {
            return Err(RenderError::InvalidViewport { width, height });
        }
        Ok(Self { width, height })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PixelCoverage {
    All,
    Mask(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MipImageU16 {
    pub width: u64,
    pub height: u64,
    pixels: Vec<u16>,
    coverage: PixelCoverage,
    iso_surface: Option<IsoSurfaceFrameU16>,
    dvr_rgba: Option<DvrRgbaFrame>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MipImageF32 {
    pub width: u64,
    pub height: u64,
    pixels: Vec<f32>,
    coverage: PixelCoverage,
    iso_surface: Option<IsoSurfaceFrameF32>,
    dvr_rgba: Option<DvrRgbaFrame>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IsoSurfaceNormal {
    pub x: i16,
    pub y: i16,
    pub z: i16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IsoSurfaceFrameU16 {
    pub width: u64,
    pub height: u64,
    source_values: Vec<u16>,
    display_scalars: Vec<u16>,
    material_scalars: Vec<u16>,
    hit_depth: Vec<f32>,
    normals: Vec<IsoSurfaceNormal>,
    diffuse_lighting: Vec<u16>,
    specular_lighting: Vec<u16>,
    coverage: PixelCoverage,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IsoSurfaceFrameF32 {
    pub width: u64,
    pub height: u64,
    source_values: Vec<f32>,
    display_scalars: Vec<f32>,
    material_scalars: Vec<f32>,
    hit_depth: Vec<f32>,
    normals: Vec<IsoSurfaceNormal>,
    diffuse_lighting: Vec<u16>,
    specular_lighting: Vec<u16>,
    coverage: PixelCoverage,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DvrRgbaFrame {
    pub width: u64,
    pub height: u64,
    premultiplied_rgba: Vec<[f32; 4]>,
    coverage: PixelCoverage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameDiagnostics {
    pub input_voxels: u64,
    pub output_pixels: u64,
    pub nonzero_pixels: u64,
    pub max_value: u16,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameDiagnosticsF32 {
    pub input_voxels: u64,
    pub output_pixels: u64,
    pub nonzero_pixels: u64,
    pub max_value: f32,
}

impl MipImageU16 {
    pub fn new(width: u64, height: u64, pixels: Vec<u16>) -> Self {
        Self::try_new(width, height, pixels, PixelCoverage::All)
            .expect("all-covered intensity frame dimensions are valid")
    }

    pub fn try_new(
        width: u64,
        height: u64,
        pixels: Vec<u16>,
        coverage: PixelCoverage,
    ) -> Result<Self, RenderError> {
        Self::try_new_with_iso_surface(width, height, pixels, coverage, None)
    }

    pub fn try_new_with_iso_surface(
        width: u64,
        height: u64,
        pixels: Vec<u16>,
        coverage: PixelCoverage,
        iso_surface: Option<IsoSurfaceFrameU16>,
    ) -> Result<Self, RenderError> {
        Self::try_new_with_mode_frames(width, height, pixels, coverage, iso_surface, None)
    }

    pub fn try_new_with_mode_frames(
        width: u64,
        height: u64,
        pixels: Vec<u16>,
        coverage: PixelCoverage,
        iso_surface: Option<IsoSurfaceFrameU16>,
        dvr_rgba: Option<DvrRgbaFrame>,
    ) -> Result<Self, RenderError> {
        validate_frame_parts(width, height, pixels.len(), &coverage)?;
        validate_iso_surface_dimensions(
            width,
            height,
            iso_surface.as_ref().map(IsoSurfaceDims::U16),
        )?;
        validate_dvr_rgba_dimensions(width, height, dvr_rgba.as_ref())?;
        Ok(Self {
            width,
            height,
            pixels,
            coverage,
            iso_surface,
            dvr_rgba,
        })
    }

    pub fn with_coverage(
        width: u64,
        height: u64,
        pixels: Vec<u16>,
        coverage: Vec<u8>,
    ) -> Result<Self, RenderError> {
        Self::try_new(width, height, pixels, PixelCoverage::Mask(coverage))
    }

    pub fn pixels(&self) -> &[u16] {
        &self.pixels
    }

    pub fn coverage(&self) -> &PixelCoverage {
        &self.coverage
    }

    pub fn iso_surface(&self) -> Option<&IsoSurfaceFrameU16> {
        self.iso_surface.as_ref()
    }

    pub fn dvr_rgba(&self) -> Option<&DvrRgbaFrame> {
        self.dvr_rgba.as_ref()
    }

    pub fn surface_lighting_factor_index(&self, index: usize) -> f32 {
        self.iso_surface
            .as_ref()
            .and_then(|surface| surface.diffuse_lighting.get(index))
            .map(|value| f32::from(*value) / f32::from(u16::MAX))
            .unwrap_or(1.0)
    }

    pub fn is_covered_index(&self, index: usize) -> bool {
        self.coverage.is_covered_index(index)
    }

    pub fn covered_pixel(&self, y: u64, x: u64) -> Option<bool> {
        if y >= self.height || x >= self.width {
            return None;
        }
        Some(
            self.coverage
                .is_covered_index((y * self.width + x) as usize),
        )
    }

    pub fn pixel(&self, y: u64, x: u64) -> Option<u16> {
        if y >= self.height || x >= self.width {
            return None;
        }
        self.pixels.get((y * self.width + x) as usize).copied()
    }
}

impl MipImageF32 {
    pub fn new(width: u64, height: u64, pixels: Vec<f32>) -> Self {
        Self::try_new(width, height, pixels, PixelCoverage::All)
            .expect("all-covered intensity frame dimensions are valid")
    }

    pub fn try_new(
        width: u64,
        height: u64,
        pixels: Vec<f32>,
        coverage: PixelCoverage,
    ) -> Result<Self, RenderError> {
        Self::try_new_with_iso_surface(width, height, pixels, coverage, None)
    }

    pub fn try_new_with_iso_surface(
        width: u64,
        height: u64,
        pixels: Vec<f32>,
        coverage: PixelCoverage,
        iso_surface: Option<IsoSurfaceFrameF32>,
    ) -> Result<Self, RenderError> {
        Self::try_new_with_mode_frames(width, height, pixels, coverage, iso_surface, None)
    }

    pub fn try_new_with_mode_frames(
        width: u64,
        height: u64,
        pixels: Vec<f32>,
        coverage: PixelCoverage,
        iso_surface: Option<IsoSurfaceFrameF32>,
        dvr_rgba: Option<DvrRgbaFrame>,
    ) -> Result<Self, RenderError> {
        validate_frame_parts(width, height, pixels.len(), &coverage)?;
        validate_iso_surface_dimensions(
            width,
            height,
            iso_surface.as_ref().map(IsoSurfaceDims::F32),
        )?;
        validate_dvr_rgba_dimensions(width, height, dvr_rgba.as_ref())?;
        Ok(Self {
            width,
            height,
            pixels,
            coverage,
            iso_surface,
            dvr_rgba,
        })
    }

    pub fn with_coverage(
        width: u64,
        height: u64,
        pixels: Vec<f32>,
        coverage: Vec<u8>,
    ) -> Result<Self, RenderError> {
        Self::try_new(width, height, pixels, PixelCoverage::Mask(coverage))
    }

    pub fn pixels(&self) -> &[f32] {
        &self.pixels
    }

    pub fn coverage(&self) -> &PixelCoverage {
        &self.coverage
    }

    pub fn iso_surface(&self) -> Option<&IsoSurfaceFrameF32> {
        self.iso_surface.as_ref()
    }

    pub fn dvr_rgba(&self) -> Option<&DvrRgbaFrame> {
        self.dvr_rgba.as_ref()
    }

    pub fn surface_lighting_factor_index(&self, index: usize) -> f32 {
        self.iso_surface
            .as_ref()
            .and_then(|surface| surface.diffuse_lighting.get(index))
            .map(|value| f32::from(*value) / f32::from(u16::MAX))
            .unwrap_or(1.0)
    }

    pub fn is_covered_index(&self, index: usize) -> bool {
        self.coverage.is_covered_index(index)
    }

    pub fn covered_pixel(&self, y: u64, x: u64) -> Option<bool> {
        if y >= self.height || x >= self.width {
            return None;
        }
        Some(
            self.coverage
                .is_covered_index((y * self.width + x) as usize),
        )
    }

    pub fn pixel(&self, y: u64, x: u64) -> Option<f32> {
        if y >= self.height || x >= self.width {
            return None;
        }
        self.pixels.get((y * self.width + x) as usize).copied()
    }
}

impl IsoSurfaceNormal {
    pub const ZERO: Self = Self { x: 0, y: 0, z: 0 };

    pub fn from_unit_components(x: f64, y: f64, z: f64) -> Self {
        Self {
            x: encode_normal_component(x),
            y: encode_normal_component(y),
            z: encode_normal_component(z),
        }
    }

    pub fn components_f32(self) -> [f32; 3] {
        [
            f32::from(self.x) / f32::from(i16::MAX),
            f32::from(self.y) / f32::from(i16::MAX),
            f32::from(self.z) / f32::from(i16::MAX),
        ]
    }
}

impl IsoSurfaceFrameU16 {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        width: u64,
        height: u64,
        source_values: Vec<u16>,
        display_scalars: Vec<u16>,
        material_scalars: Vec<u16>,
        hit_depth: Vec<f32>,
        normals: Vec<IsoSurfaceNormal>,
        diffuse_lighting: Vec<u16>,
        specular_lighting: Vec<u16>,
        coverage: PixelCoverage,
    ) -> Result<Self, RenderError> {
        let expected =
            validate_iso_surface_part(width, height, "source_values", source_values.len())?;
        validate_iso_surface_part(width, height, "display_scalars", display_scalars.len())?;
        validate_iso_surface_part(width, height, "material_scalars", material_scalars.len())?;
        validate_iso_surface_part(width, height, "hit_depth", hit_depth.len())?;
        validate_iso_surface_part(width, height, "normals", normals.len())?;
        validate_iso_surface_part(width, height, "diffuse_lighting", diffuse_lighting.len())?;
        validate_iso_surface_part(width, height, "specular_lighting", specular_lighting.len())?;
        coverage.validate(width, height, expected)?;
        Ok(Self {
            width,
            height,
            source_values,
            display_scalars,
            material_scalars,
            hit_depth,
            normals,
            diffuse_lighting,
            specular_lighting,
            coverage,
        })
    }

    pub fn source_values(&self) -> &[u16] {
        &self.source_values
    }

    pub fn display_scalars(&self) -> &[u16] {
        &self.display_scalars
    }

    pub fn material_scalars(&self) -> &[u16] {
        &self.material_scalars
    }

    pub fn hit_depth(&self) -> &[f32] {
        &self.hit_depth
    }

    pub fn normals(&self) -> &[IsoSurfaceNormal] {
        &self.normals
    }

    pub fn diffuse_lighting(&self) -> &[u16] {
        &self.diffuse_lighting
    }

    pub fn specular_lighting(&self) -> &[u16] {
        &self.specular_lighting
    }

    pub fn coverage(&self) -> &PixelCoverage {
        &self.coverage
    }

    pub fn is_covered_index(&self, index: usize) -> bool {
        self.coverage.is_covered_index(index)
    }
}

impl IsoSurfaceFrameF32 {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        width: u64,
        height: u64,
        source_values: Vec<f32>,
        display_scalars: Vec<f32>,
        material_scalars: Vec<f32>,
        hit_depth: Vec<f32>,
        normals: Vec<IsoSurfaceNormal>,
        diffuse_lighting: Vec<u16>,
        specular_lighting: Vec<u16>,
        coverage: PixelCoverage,
    ) -> Result<Self, RenderError> {
        let expected =
            validate_iso_surface_part(width, height, "source_values", source_values.len())?;
        validate_iso_surface_part(width, height, "display_scalars", display_scalars.len())?;
        validate_iso_surface_part(width, height, "material_scalars", material_scalars.len())?;
        validate_iso_surface_part(width, height, "hit_depth", hit_depth.len())?;
        validate_iso_surface_part(width, height, "normals", normals.len())?;
        validate_iso_surface_part(width, height, "diffuse_lighting", diffuse_lighting.len())?;
        validate_iso_surface_part(width, height, "specular_lighting", specular_lighting.len())?;
        coverage.validate(width, height, expected)?;
        Ok(Self {
            width,
            height,
            source_values,
            display_scalars,
            material_scalars,
            hit_depth,
            normals,
            diffuse_lighting,
            specular_lighting,
            coverage,
        })
    }

    pub fn source_values(&self) -> &[f32] {
        &self.source_values
    }

    pub fn display_scalars(&self) -> &[f32] {
        &self.display_scalars
    }

    pub fn material_scalars(&self) -> &[f32] {
        &self.material_scalars
    }

    pub fn hit_depth(&self) -> &[f32] {
        &self.hit_depth
    }

    pub fn normals(&self) -> &[IsoSurfaceNormal] {
        &self.normals
    }

    pub fn diffuse_lighting(&self) -> &[u16] {
        &self.diffuse_lighting
    }

    pub fn specular_lighting(&self) -> &[u16] {
        &self.specular_lighting
    }

    pub fn coverage(&self) -> &PixelCoverage {
        &self.coverage
    }

    pub fn is_covered_index(&self, index: usize) -> bool {
        self.coverage.is_covered_index(index)
    }
}

impl DvrRgbaFrame {
    pub fn try_new(
        width: u64,
        height: u64,
        premultiplied_rgba: Vec<[f32; 4]>,
        coverage: PixelCoverage,
    ) -> Result<Self, RenderError> {
        let expected = validate_dvr_rgba_part(width, height, premultiplied_rgba.len())?;
        coverage.validate(width, height, expected)?;
        Ok(Self {
            width,
            height,
            premultiplied_rgba,
            coverage,
        })
    }

    pub fn premultiplied_rgba(&self) -> &[[f32; 4]] {
        &self.premultiplied_rgba
    }

    pub fn coverage(&self) -> &PixelCoverage {
        &self.coverage
    }

    pub fn is_covered_index(&self, index: usize) -> bool {
        self.coverage.is_covered_index(index)
    }
}

impl PixelCoverage {
    pub fn from_bool_mask(mask: Vec<bool>) -> Self {
        Self::Mask(mask.into_iter().map(u8::from).collect())
    }

    pub fn is_covered_index(&self, index: usize) -> bool {
        match self {
            Self::All => true,
            Self::Mask(mask) => mask.get(index).copied() == Some(1),
        }
    }

    pub fn covered_count(&self, pixel_count: usize) -> usize {
        match self {
            Self::All => pixel_count,
            Self::Mask(mask) => mask.iter().filter(|&&covered| covered == 1).count(),
        }
    }

    fn validate(&self, width: u64, height: u64, expected: usize) -> Result<(), RenderError> {
        match self {
            Self::All => Ok(()),
            Self::Mask(mask) => {
                if mask.len() != expected {
                    return Err(RenderError::InvalidPixelCoverageBuffer {
                        width,
                        height,
                        expected,
                        actual: mask.len(),
                    });
                }
                for (index, &value) in mask.iter().enumerate() {
                    if value > 1 {
                        return Err(RenderError::InvalidPixelCoverageValue { index, value });
                    }
                }
                Ok(())
            }
        }
    }
}

fn validate_frame_parts(
    width: u64,
    height: u64,
    pixel_len: usize,
    coverage: &PixelCoverage,
) -> Result<usize, RenderError> {
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| usize::try_from(pixels).ok())
        .ok_or(RenderError::InvalidIntensityFrameBuffer {
            width,
            height,
            expected: usize::MAX,
            actual: pixel_len,
        })?;
    if pixel_len != expected {
        return Err(RenderError::InvalidIntensityFrameBuffer {
            width,
            height,
            expected,
            actual: pixel_len,
        });
    }
    coverage.validate(width, height, expected)?;
    Ok(expected)
}

enum IsoSurfaceDims<'a> {
    U16(&'a IsoSurfaceFrameU16),
    F32(&'a IsoSurfaceFrameF32),
}

impl IsoSurfaceDims<'_> {
    fn width(&self) -> u64 {
        match self {
            Self::U16(surface) => surface.width,
            Self::F32(surface) => surface.width,
        }
    }

    fn height(&self) -> u64 {
        match self {
            Self::U16(surface) => surface.height,
            Self::F32(surface) => surface.height,
        }
    }
}

fn validate_iso_surface_dimensions(
    width: u64,
    height: u64,
    iso_surface: Option<IsoSurfaceDims<'_>>,
) -> Result<(), RenderError> {
    let Some(iso_surface) = iso_surface else {
        return Ok(());
    };
    if iso_surface.width() != width || iso_surface.height() != height {
        return Err(RenderError::InvalidIsoSurfaceFrameDimensions {
            width,
            height,
            surface_width: iso_surface.width(),
            surface_height: iso_surface.height(),
        });
    }
    Ok(())
}

fn validate_dvr_rgba_dimensions(
    width: u64,
    height: u64,
    dvr_rgba: Option<&DvrRgbaFrame>,
) -> Result<(), RenderError> {
    let Some(dvr_rgba) = dvr_rgba else {
        return Ok(());
    };
    if dvr_rgba.width != width || dvr_rgba.height != height {
        return Err(RenderError::InvalidDvrRgbaFrameDimensions {
            width,
            height,
            dvr_width: dvr_rgba.width,
            dvr_height: dvr_rgba.height,
        });
    }
    Ok(())
}

fn validate_iso_surface_part(
    width: u64,
    height: u64,
    field: &'static str,
    actual: usize,
) -> Result<usize, RenderError> {
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| usize::try_from(pixels).ok())
        .ok_or(RenderError::InvalidIsoSurfaceFrameBuffer {
            field,
            width,
            height,
            expected: usize::MAX,
            actual,
        })?;
    if actual != expected {
        return Err(RenderError::InvalidIsoSurfaceFrameBuffer {
            field,
            width,
            height,
            expected,
            actual,
        });
    }
    Ok(expected)
}

fn validate_dvr_rgba_part(width: u64, height: u64, actual: usize) -> Result<usize, RenderError> {
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| usize::try_from(pixels).ok())
        .ok_or(RenderError::InvalidDvrRgbaFrameBuffer {
            width,
            height,
            expected: usize::MAX,
            actual,
        })?;
    if actual != expected {
        return Err(RenderError::InvalidDvrRgbaFrameBuffer {
            width,
            height,
            expected,
            actual,
        });
    }
    Ok(expected)
}

fn encode_normal_component(value: f64) -> i16 {
    (value.clamp(-1.0, 1.0) * f64::from(i16::MAX)).round() as i16
}

pub fn frame_diagnostics(input_voxels: u64, pixels: &[u16]) -> FrameDiagnostics {
    FrameDiagnostics {
        input_voxels,
        output_pixels: pixels.len() as u64,
        nonzero_pixels: pixels.iter().filter(|&&pixel| pixel != 0).count() as u64,
        max_value: pixels.iter().copied().max().unwrap_or(0),
    }
}

pub fn frame_diagnostics_f32(input_voxels: u64, pixels: &[f32]) -> FrameDiagnosticsF32 {
    FrameDiagnosticsF32 {
        input_voxels,
        output_pixels: pixels.len() as u64,
        nonzero_pixels: pixels.iter().filter(|&&pixel| pixel != 0.0).count() as u64,
        max_value: pixels.iter().copied().reduce(f32::max).unwrap_or(0.0),
    }
}

pub use brick_plan::{
    BrickGridSpec, BrickPlanOptions, ResourcePlanLimits, SemanticRegionGridSpec,
    plan_visible_bricks, plan_visible_resource_regions,
};
pub use camera_mip::{
    CameraRenderMode, CameraRenderModeF32, CameraRenderQuality, DvrRenderParameters,
    IntensitySamplingPolicy, IsoShadingMode, IsoSurfaceParameters,
};
pub use cross_section::{
    CrossSectionBasis, CrossSectionBrickPlan, CrossSectionChunkPlanePolygon,
    CrossSectionChunkPlaneVertex, CrossSectionPanel, CrossSectionPanelBounds, CrossSectionSlab,
    CrossSectionView, CrossSectionViewState, cross_section_chunk_plane_polygon,
    plan_cross_section_bricks, plan_cross_section_bricks_with_diagnostics,
    plan_cross_section_resource_regions,
};
pub use current_lease_bridge::{
    CurrentLeaseBridge, CurrentLeaseBridgeError, CurrentLeaseCohortStatus, CurrentLeaseResidentSet,
    CurrentLeaseResource, CurrentLeaseSample, CurrentLeaseVolume, MAX_CURRENT_LEASE_REQUIREMENTS,
};
pub use diagnostics::{BrickFrameDiagnostics, BrickFrameDiagnosticsF32, BrickSkipDiagnostics};
pub use resources::{BrickAtlasResourceKey, ResourceRepresentation, TransformKey};
pub use scene::{
    CoordinateSpace, GridPosition, OcclusionPolicy, PickCompleteness, PickHit, PickHitKind,
    PickPolicy, PickPrimitive, PickQuery, PickValue, PlaneId, SceneColorRgba, SceneDrawItem,
    SceneDrawList, SceneError, SceneFrameContext, SceneGeometry, SceneGridTransform, SceneLayer,
    SceneLayerId, SceneLayerKind, SceneObject, SceneObjectId, ScenePickTarget, ScenePlaneTransform,
    SceneRenderPass, SceneStyle, SceneTime, ScreenPosition, ScreenRect, VolumePickProbe,
    empty_pick_hit, extract_scene_draw_list, pick_scene_targets, render_pass_for, voxel_pick_hit,
    voxel_pick_hit_f32, voxel_pick_hit_u8,
};
pub use scene_render::{
    SceneRenderCommand, SceneRenderCommandKind, SceneRenderCommandList, SceneRenderDiagnostics,
    SceneRenderOutput, SceneRgbaImage, build_scene_render_commands,
};
pub use transfer::{
    DisplayRgbaImage, DvrRgbaChannelFrame, IntensityChannelFrame, IntensityChannelFrameF32,
    IntensityTransfer, IsoSurfaceChannelFrame, IsoSurfaceChannelFrameF32, ScalarDisplayTransfer,
    composite_dvr_rgba_channels, composite_f32_intensity_channels, composite_intensity_channels,
    composite_iso_surface_channels, composite_iso_surface_f32_channels,
};

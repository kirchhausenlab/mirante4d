use glam::DVec3;
use mirante4d_data::{DenseVolumeF32, DenseVolumeU8, DenseVolumeU16};
use mirante4d_domain::{GridToWorld, Shape3D};
use mirante4d_format::CurrentGridToWorldExt;
use mirante4d_render_api::CameraFrame;

use crate::{
    DvrRgbaFrame, FrameDiagnostics, FrameDiagnosticsF32, GridPosition, IsoSurfaceFrameF32,
    IsoSurfaceFrameU16, IsoSurfaceNormal, MipImageF32, MipImageU16, PickCompleteness, PickPolicy,
    PixelCoverage, RenderError, RenderViewport, ScalarDisplayTransfer, frame_diagnostics,
    frame_diagnostics_f32,
};

const EPSILON: f64 = 1.0e-9;
const SMOOTH_RAY_STEP_VOXELS: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CameraRenderMode {
    Mip,
    Isosurface { parameters: IsoSurfaceParameters },
    Dvr { parameters: DvrRenderParameters },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CameraRenderModeF32 {
    Mip,
    Isosurface { parameters: IsoSurfaceParameters },
    Dvr { parameters: DvrRenderParameters },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IsoSurfaceParameters {
    pub display_level: f32,
    pub transfer: ScalarDisplayTransfer,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DvrRenderParameters {
    pub color_transfer: ScalarDisplayTransfer,
    pub opacity_transfer: ScalarDisplayTransfer,
    pub color_rgba: [f32; 4],
    pub channel_opacity: f32,
    pub density_scale: f64,
}

#[derive(Debug, Clone, Copy)]
pub enum DvrVolumeChannel<'a> {
    U16 {
        volume: &'a DenseVolumeU16,
        parameters: DvrRenderParameters,
    },
    F32 {
        volume: &'a DenseVolumeF32,
        parameters: DvrRenderParameters,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct IsoSurfaceHit {
    source_value: f64,
    display_scalar: f64,
    material_display_scalar: f64,
    hit_t: f64,
    grid_position: DVec3,
}

impl IsoSurfaceParameters {
    pub fn new(display_level: f32, transfer: ScalarDisplayTransfer) -> Self {
        Self {
            display_level: display_level.clamp(0.0, 1.0),
            transfer,
        }
    }

    pub(crate) fn level_f64(self) -> f64 {
        f64::from(self.display_level.clamp(0.0, 1.0))
    }

    pub(crate) fn map_u16(self, value: u16) -> f64 {
        self.transfer.map_source_value_f64(f64::from(value))
    }

    pub(crate) fn map_u8(self, value: u8) -> f64 {
        self.transfer.map_source_value_f64(f64::from(value))
    }

    pub(crate) fn map_f32(self, value: f32) -> f64 {
        self.transfer.map_source_value_f64(f64::from(value))
    }
}

impl DvrRenderParameters {
    pub fn new(
        color_transfer: ScalarDisplayTransfer,
        opacity_transfer: ScalarDisplayTransfer,
        color_rgba: [f32; 4],
        channel_opacity: f32,
        density_scale: f64,
    ) -> Self {
        Self {
            color_transfer,
            opacity_transfer,
            color_rgba,
            channel_opacity: channel_opacity.clamp(0.0, 1.0),
            density_scale: density_scale.max(0.0),
        }
    }

    pub(crate) fn visible(self) -> bool {
        self.channel_opacity > f32::EPSILON
            && self.color_rgba[3] > f32::EPSILON
            && self.density_scale > EPSILON
    }

    pub(crate) fn color_scalar(self, value: f64) -> f64 {
        self.color_transfer
            .map_source_value_f64(value)
            .clamp(0.0, 1.0)
    }

    pub(crate) fn opacity_scalar(self, value: f64) -> f64 {
        self.opacity_transfer
            .map_source_value_f64(value)
            .clamp(0.0, 1.0)
    }

    pub(crate) fn source_interval_can_contribute(self, min: f64, max: f64) -> bool {
        if !self.visible() {
            return false;
        }
        if !min.is_finite() || !max.is_finite() || max < min {
            return true;
        }
        self.opacity_scalar(min).max(self.opacity_scalar(max)) > EPSILON
    }
}

impl<'a> DvrVolumeChannel<'a> {
    pub fn u16(volume: &'a DenseVolumeU16, parameters: DvrRenderParameters) -> Self {
        Self::U16 { volume, parameters }
    }

    pub fn f32(volume: &'a DenseVolumeF32, parameters: DvrRenderParameters) -> Self {
        Self::F32 { volume, parameters }
    }

    fn shape(self) -> Shape3D {
        match self {
            Self::U16 { volume, .. } => volume.shape,
            Self::F32 { volume, .. } => volume.shape,
        }
    }

    fn grid_to_world(self) -> GridToWorld {
        match self {
            Self::U16 { volume, .. } => volume.grid_to_world,
            Self::F32 { volume, .. } => volume.grid_to_world,
        }
    }

    fn render_valid_voxel_count(self) -> u64 {
        match self {
            Self::U16 { volume, .. } => volume.render_valid_voxel_count(),
            Self::F32 { volume, .. } => volume.render_valid_voxel_count(),
        }
    }

    fn parameters(self) -> DvrRenderParameters {
        match self {
            Self::U16 { parameters, .. } | Self::F32 { parameters, .. } => parameters,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntensitySamplingPolicy {
    VoxelExact,
    SmoothLinear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsoShadingMode {
    Flat,
    GradientLighting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraRenderQuality {
    pub intensity_sampling: IntensitySamplingPolicy,
    pub iso_shading: IsoShadingMode,
}

impl CameraRenderQuality {
    pub const fn voxel_exact() -> Self {
        Self {
            intensity_sampling: IntensitySamplingPolicy::VoxelExact,
            iso_shading: IsoShadingMode::Flat,
        }
    }

    pub const fn smooth_linear() -> Self {
        Self {
            intensity_sampling: IntensitySamplingPolicy::SmoothLinear,
            iso_shading: IsoShadingMode::GradientLighting,
        }
    }
}

impl Default for CameraRenderQuality {
    fn default() -> Self {
        Self::voxel_exact()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraVolumePick {
    pub intensity: u16,
    pub world_position: Option<DVec3>,
    pub grid_position: Option<GridPosition>,
    pub policy: PickPolicy,
    pub completeness: PickCompleteness,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraVolumePickU8 {
    pub intensity: u8,
    pub world_position: Option<DVec3>,
    pub grid_position: Option<GridPosition>,
    pub policy: PickPolicy,
    pub completeness: PickCompleteness,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraVolumePickF32 {
    pub intensity: f32,
    pub world_position: Option<DVec3>,
    pub grid_position: Option<GridPosition>,
    pub policy: PickPolicy,
    pub completeness: PickCompleteness,
}

mod sampling;
pub use sampling::{
    pick_camera_volume, pick_camera_volume_f32, pick_camera_volume_u8, render_camera,
    render_camera_f32, render_camera_f32_with_quality, render_camera_mip, render_camera_mip_f32,
    render_camera_u8, render_camera_u8_with_quality, render_camera_with_quality,
    render_dvr_channels_with_quality,
};

#[cfg(test)]
mod tests;

use crate::ScalarDisplayTransfer;

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

impl IsoSurfaceParameters {
    pub fn new(display_level: f32, transfer: ScalarDisplayTransfer) -> Self {
        Self {
            display_level: display_level.clamp(0.0, 1.0),
            transfer,
        }
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

#[cfg(test)]
mod tests {
    use mirante4d_domain::{DisplayWindow, TransferCurve};

    use super::*;

    fn transfer() -> ScalarDisplayTransfer {
        ScalarDisplayTransfer::new(
            DisplayWindow::new(0.0, 100.0).unwrap(),
            TransferCurve::linear(),
            false,
        )
    }

    #[test]
    fn render_parameters_clamp_ui_inputs() {
        let iso = IsoSurfaceParameters::new(2.0, transfer());
        let dvr = DvrRenderParameters::new(transfer(), transfer(), [1.0; 4], 2.0, -1.0);

        assert_eq!(iso.display_level, 1.0);
        assert_eq!(dvr.channel_opacity, 1.0);
        assert_eq!(dvr.density_scale, 0.0);
    }
}

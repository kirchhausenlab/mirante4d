use thiserror::Error;

use crate::{DisplayWindow, TransferCurve};

#[derive(Debug, Error, Clone, Copy, PartialEq)]
pub enum RenderError {
    #[error("ISO display level must be finite and in [0, 1]")]
    InvalidIsoDisplayLevel,
    #[error("DVR density scale must be finite and positive")]
    InvalidDvrDensityScale,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SamplingPolicy {
    #[default]
    SmoothLinear,
    VoxelExact,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IsoShadingPolicy {
    #[default]
    GradientLighting,
    Flat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderMode {
    Mip,
    Isosurface,
    Dvr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MipParameters {
    sampling_policy: SamplingPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IsoParameters {
    sampling_policy: SamplingPolicy,
    shading_policy: IsoShadingPolicy,
    display_level: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DvrOpacityTransfer {
    window: DisplayWindow,
    curve: TransferCurve,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DvrParameters {
    sampling_policy: SamplingPolicy,
    opacity_transfer: DvrOpacityTransfer,
    density_scale: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderState(RenderParameters);

#[derive(Debug, Clone, Copy, PartialEq)]
enum RenderParameters {
    Mip(MipParameters),
    Isosurface(IsoParameters),
    Dvr(DvrParameters),
}

impl MipParameters {
    pub const fn sampling_policy(self) -> SamplingPolicy {
        self.sampling_policy
    }
}

impl IsoParameters {
    pub const fn sampling_policy(self) -> SamplingPolicy {
        self.sampling_policy
    }

    pub const fn shading_policy(self) -> IsoShadingPolicy {
        self.shading_policy
    }

    pub const fn display_level(self) -> f32 {
        self.display_level
    }
}

impl DvrOpacityTransfer {
    pub const fn new(window: DisplayWindow, curve: TransferCurve) -> Self {
        Self { window, curve }
    }

    pub const fn window(self) -> DisplayWindow {
        self.window
    }

    pub const fn curve(self) -> TransferCurve {
        self.curve
    }
}

impl DvrParameters {
    pub const fn sampling_policy(self) -> SamplingPolicy {
        self.sampling_policy
    }

    pub const fn opacity_transfer(self) -> DvrOpacityTransfer {
        self.opacity_transfer
    }

    pub const fn density_scale(self) -> f64 {
        self.density_scale
    }
}

impl RenderState {
    pub const fn mip(sampling_policy: SamplingPolicy) -> Self {
        Self(RenderParameters::Mip(MipParameters { sampling_policy }))
    }

    pub fn iso(
        sampling_policy: SamplingPolicy,
        shading_policy: IsoShadingPolicy,
        display_level: f32,
    ) -> Result<Self, RenderError> {
        if !display_level.is_finite() || !(0.0..=1.0).contains(&display_level) {
            return Err(RenderError::InvalidIsoDisplayLevel);
        }
        Ok(Self(RenderParameters::Isosurface(IsoParameters {
            sampling_policy,
            shading_policy,
            display_level: canonical_zero(display_level),
        })))
    }

    pub fn dvr(
        sampling_policy: SamplingPolicy,
        opacity_transfer: DvrOpacityTransfer,
        density_scale: f64,
    ) -> Result<Self, RenderError> {
        if !density_scale.is_finite() || density_scale <= 0.0 {
            return Err(RenderError::InvalidDvrDensityScale);
        }
        Ok(Self(RenderParameters::Dvr(DvrParameters {
            sampling_policy,
            opacity_transfer,
            density_scale,
        })))
    }

    pub const fn mode(self) -> RenderMode {
        match self.0 {
            RenderParameters::Mip(_) => RenderMode::Mip,
            RenderParameters::Isosurface(_) => RenderMode::Isosurface,
            RenderParameters::Dvr(_) => RenderMode::Dvr,
        }
    }

    pub const fn sampling_policy(self) -> SamplingPolicy {
        match self.0 {
            RenderParameters::Mip(parameters) => parameters.sampling_policy,
            RenderParameters::Isosurface(parameters) => parameters.sampling_policy,
            RenderParameters::Dvr(parameters) => parameters.sampling_policy,
        }
    }

    pub const fn mip_parameters(self) -> Option<MipParameters> {
        match self.0 {
            RenderParameters::Mip(parameters) => Some(parameters),
            _ => None,
        }
    }

    pub const fn iso_parameters(self) -> Option<IsoParameters> {
        match self.0 {
            RenderParameters::Isosurface(parameters) => Some(parameters),
            _ => None,
        }
    }

    pub const fn dvr_parameters(self) -> Option<DvrParameters> {
        match self.0 {
            RenderParameters::Dvr(parameters) => Some(parameters),
            _ => None,
        }
    }
}

impl Default for RenderState {
    fn default() -> Self {
        Self::mip(SamplingPolicy::default())
    }
}

fn canonical_zero(value: f32) -> f32 {
    if value == 0.0 { 0.0 } else { value }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use proptest::test_runner::RngSeed;

    use super::*;

    fn opacity_transfer() -> DvrOpacityTransfer {
        DvrOpacityTransfer::new(
            DisplayWindow::new(0.0, 1.0).unwrap(),
            TransferCurve::gamma(0.5).unwrap(),
        )
    }

    #[test]
    fn each_mode_exposes_only_its_own_parameters() {
        let mip = RenderState::mip(SamplingPolicy::VoxelExact);
        assert_eq!(mip.mode(), RenderMode::Mip);
        assert!(mip.mip_parameters().is_some());
        assert!(mip.iso_parameters().is_none());
        assert!(mip.dvr_parameters().is_none());

        let iso =
            RenderState::iso(SamplingPolicy::SmoothLinear, IsoShadingPolicy::Flat, 0.75).unwrap();
        assert_eq!(iso.mode(), RenderMode::Isosurface);
        assert_eq!(iso.iso_parameters().unwrap().display_level(), 0.75);
    }

    #[test]
    fn iso_rejects_out_of_range_display_levels() {
        assert_eq!(
            RenderState::iso(
                SamplingPolicy::SmoothLinear,
                IsoShadingPolicy::GradientLighting,
                1.1,
            ),
            Err(RenderError::InvalidIsoDisplayLevel)
        );
    }

    #[test]
    fn dvr_has_one_canonical_opacity_transfer_and_positive_density() {
        let opacity = opacity_transfer();
        let state = RenderState::dvr(SamplingPolicy::VoxelExact, opacity, 12.0).unwrap();
        let parameters = state.dvr_parameters().unwrap();
        assert_eq!(parameters.opacity_transfer(), opacity);
        assert_eq!(parameters.density_scale(), 12.0);
        assert_eq!(
            RenderState::dvr(SamplingPolicy::VoxelExact, opacity, 0.0),
            Err(RenderError::InvalidDvrDensityScale)
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 64,
            max_shrink_iters: 1_024,
            failure_persistence: None,
            rng_seed: RngSeed::Fixed(0x4d34_444f_4d52_454e),
            ..ProptestConfig::default()
        })]

        #[test]
        fn valid_dvr_density_round_trips(density in 1.0e-9_f64..1.0e9) {
            let state = RenderState::dvr(SamplingPolicy::SmoothLinear, opacity_transfer(), density).unwrap();
            prop_assert_eq!(state.dvr_parameters().unwrap().density_scale(), density);
        }
    }
}

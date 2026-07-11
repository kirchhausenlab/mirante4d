use thiserror::Error;

pub const TRANSFER_GAMMA_MIN: f32 = 0.05;
pub const TRANSFER_GAMMA_MAX: f32 = 8.0;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum DisplayError {
    #[error("display window bounds must be finite and high must be greater than low")]
    InvalidWindow,
    #[error("RGB components must be finite and in [0, 1]")]
    InvalidColor,
    #[error("opacity must be finite and in [0, 1]")]
    InvalidOpacity,
    #[error("transfer gamma must be finite and in [{TRANSFER_GAMMA_MIN}, {TRANSFER_GAMMA_MAX}]")]
    InvalidGamma,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayWindow {
    low: f32,
    high: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RgbColor([f32; 3]);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Opacity(f32);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransferCurve(TransferCurveKind);

#[derive(Debug, Clone, Copy, PartialEq)]
enum TransferCurveKind {
    Linear,
    Gamma(f32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayerTransfer {
    window: DisplayWindow,
    color: RgbColor,
    opacity: Opacity,
    curve: TransferCurve,
    invert: bool,
}

impl DisplayWindow {
    pub fn new(low: f32, high: f32) -> Result<Self, DisplayError> {
        if low.is_finite() && high.is_finite() && high > low {
            Ok(Self { low, high })
        } else {
            Err(DisplayError::InvalidWindow)
        }
    }

    pub const fn low(self) -> f32 {
        self.low
    }

    pub const fn high(self) -> f32 {
        self.high
    }
}

impl RgbColor {
    pub fn new(rgb: [f32; 3]) -> Result<Self, DisplayError> {
        if rgb
            .iter()
            .all(|value| value.is_finite() && (0.0..=1.0).contains(value))
        {
            Ok(Self(rgb.map(canonical_zero)))
        } else {
            Err(DisplayError::InvalidColor)
        }
    }

    pub const fn rgb(self) -> [f32; 3] {
        self.0
    }
}

impl Opacity {
    pub fn new(value: f32) -> Result<Self, DisplayError> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(canonical_zero(value)))
        } else {
            Err(DisplayError::InvalidOpacity)
        }
    }

    pub const fn get(self) -> f32 {
        self.0
    }
}

impl TransferCurve {
    pub const fn linear() -> Self {
        Self(TransferCurveKind::Linear)
    }

    pub fn gamma(gamma: f32) -> Result<Self, DisplayError> {
        if gamma.is_finite() && (TRANSFER_GAMMA_MIN..=TRANSFER_GAMMA_MAX).contains(&gamma) {
            Ok(Self(TransferCurveKind::Gamma(gamma)))
        } else {
            Err(DisplayError::InvalidGamma)
        }
    }

    pub const fn gamma_value(self) -> f32 {
        match self.0 {
            TransferCurveKind::Linear => 1.0,
            TransferCurveKind::Gamma(gamma) => gamma,
        }
    }

    pub const fn is_linear(self) -> bool {
        matches!(self.0, TransferCurveKind::Linear)
    }
}

impl Default for TransferCurve {
    fn default() -> Self {
        Self::linear()
    }
}

impl LayerTransfer {
    pub fn new(
        window: DisplayWindow,
        color: RgbColor,
        opacity: Opacity,
        curve: TransferCurve,
        invert: bool,
    ) -> Self {
        Self {
            window,
            color,
            opacity,
            curve,
            invert,
        }
    }

    pub const fn window(&self) -> DisplayWindow {
        self.window
    }

    pub const fn color(&self) -> RgbColor {
        self.color
    }

    pub const fn opacity(&self) -> Opacity {
        self.opacity
    }

    pub const fn curve(&self) -> TransferCurve {
        self.curve
    }

    pub const fn invert(&self) -> bool {
        self.invert
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

    #[test]
    fn rejects_non_finite_or_reversed_windows() {
        assert_eq!(
            DisplayWindow::new(1.0, 1.0),
            Err(DisplayError::InvalidWindow)
        );
        assert_eq!(
            DisplayWindow::new(0.0, f32::INFINITY),
            Err(DisplayError::InvalidWindow)
        );
    }

    #[test]
    fn validates_colors_and_opacity() {
        assert!(RgbColor::new([0.0, 0.5, 1.0]).is_ok());
        assert_eq!(
            RgbColor::new([0.0, -0.1, 1.0]),
            Err(DisplayError::InvalidColor)
        );
        assert_eq!(Opacity::new(f32::NAN), Err(DisplayError::InvalidOpacity));
    }

    #[test]
    fn transfer_gamma_is_bounded() {
        assert_eq!(
            TransferCurve::gamma(TRANSFER_GAMMA_MIN / 2.0),
            Err(DisplayError::InvalidGamma)
        );
        assert_eq!(TransferCurve::gamma(2.0).unwrap().gamma_value(), 2.0);
    }

    #[test]
    fn color_is_rgb_and_cannot_duplicate_opacity() {
        let color = RgbColor::new([0.25, 0.5, 0.75]).unwrap();
        assert_eq!(color.rgb(), [0.25, 0.5, 0.75]);
    }

    #[test]
    fn layer_transfer_has_one_validated_value_for_each_control() {
        let transfer = LayerTransfer::new(
            DisplayWindow::new(0.0, 4095.0).unwrap(),
            RgbColor::new([0.0, 1.0, 0.0]).unwrap(),
            Opacity::new(0.75).unwrap(),
            TransferCurve::gamma(2.0).unwrap(),
            true,
        );
        assert_eq!(transfer.window().high(), 4095.0);
        assert_eq!(transfer.opacity().get(), 0.75);
        assert!(transfer.invert());
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 64,
            max_shrink_iters: 1_024,
            failure_persistence: None,
            rng_seed: RngSeed::Fixed(0x4d34_444f_4d44_4953),
            ..ProptestConfig::default()
        })]

        #[test]
        fn every_ordered_finite_window_round_trips(low in -1.0e6_f32..1.0e6, width in 0.001_f32..1.0e6) {
            let high = low + width;
            prop_assume!(high.is_finite() && high > low);
            let window = DisplayWindow::new(low, high).unwrap();
            prop_assert_eq!(window.low(), low);
            prop_assert_eq!(window.high(), high);
        }
    }
}

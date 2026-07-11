use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum DisplayError {
    #[error("display window high value must be greater than low value")]
    InvalidWindow,
    #[error("opacity must be in [0, 1], got {0}")]
    InvalidOpacity(f32),
    #[error("color components must be in [0, 1], got {0:?}")]
    InvalidColor([f32; 4]),
    #[error("transfer gamma must be finite and in [{min}, {max}], got {value}")]
    InvalidGamma { value: f32, min: f32, max: f32 },
    #[error("transfer preset id must not be empty")]
    EmptyTransferPresetId,
    #[error("transfer preset id must contain only ASCII letters, digits, '-' or '_', got {0:?}")]
    InvalidTransferPresetId(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DisplayWindow {
    pub low: f32,
    pub high: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChannelColor {
    pub color_rgba: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LayerDisplay {
    pub visible: bool,
    pub window: DisplayWindow,
    pub opacity: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TransferCurve {
    Linear,
    Gamma { gamma: f32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TransferPresetId(String);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelTransferFunction {
    pub display: LayerDisplay,
    pub color: ChannelColor,
    pub curve: TransferCurve,
    pub preset: TransferPresetId,
    pub invert: bool,
}

pub const TRANSFER_GAMMA_MIN: f32 = 0.05;
pub const TRANSFER_GAMMA_MAX: f32 = 8.0;

impl DisplayWindow {
    pub fn new(low: f32, high: f32) -> Result<Self, DisplayError> {
        if high > low {
            Ok(Self { low, high })
        } else {
            Err(DisplayError::InvalidWindow)
        }
    }
}

impl ChannelColor {
    pub fn new(color_rgba: [f32; 4]) -> Result<Self, DisplayError> {
        if color_rgba.iter().all(|v| (0.0..=1.0).contains(v)) {
            Ok(Self { color_rgba })
        } else {
            Err(DisplayError::InvalidColor(color_rgba))
        }
    }
}

impl LayerDisplay {
    pub fn new(visible: bool, window: DisplayWindow, opacity: f32) -> Result<Self, DisplayError> {
        if (0.0..=1.0).contains(&opacity) {
            Ok(Self {
                visible,
                window,
                opacity,
            })
        } else {
            Err(DisplayError::InvalidOpacity(opacity))
        }
    }
}

impl TransferCurve {
    pub fn gamma(gamma: f32) -> Result<Self, DisplayError> {
        if gamma.is_finite() && (TRANSFER_GAMMA_MIN..=TRANSFER_GAMMA_MAX).contains(&gamma) {
            Ok(Self::Gamma { gamma })
        } else {
            Err(DisplayError::InvalidGamma {
                value: gamma,
                min: TRANSFER_GAMMA_MIN,
                max: TRANSFER_GAMMA_MAX,
            })
        }
    }

    pub fn validate(self) -> Result<Self, DisplayError> {
        match self {
            Self::Linear => Ok(Self::Linear),
            Self::Gamma { gamma } => Self::gamma(gamma),
        }
    }

    pub fn map_normalized(self, value: f32) -> f32 {
        let value = value.clamp(0.0, 1.0);
        match self {
            Self::Linear => value,
            Self::Gamma { gamma } => value.powf(1.0 / gamma),
        }
    }

    pub fn gamma_value(self) -> f32 {
        match self {
            Self::Linear => 1.0,
            Self::Gamma { gamma } => gamma,
        }
    }
}

impl TransferPresetId {
    pub fn new(value: impl Into<String>) -> Result<Self, DisplayError> {
        let value = value.into();
        if value.is_empty() {
            return Err(DisplayError::EmptyTransferPresetId);
        }
        if value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            Ok(Self(value))
        } else {
            Err(DisplayError::InvalidTransferPresetId(value))
        }
    }

    pub fn linear() -> Self {
        Self::new("linear").expect("built-in preset id is valid")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl ChannelTransferFunction {
    pub fn new(
        display: LayerDisplay,
        color: ChannelColor,
        curve: TransferCurve,
        preset: TransferPresetId,
    ) -> Result<Self, DisplayError> {
        Ok(Self {
            display,
            color,
            curve: curve.validate()?,
            preset,
            invert: false,
        })
    }

    pub fn linear(display: LayerDisplay, color: ChannelColor) -> Self {
        Self::new(
            display,
            color,
            TransferCurve::Linear,
            TransferPresetId::linear(),
        )
        .expect("linear transfer with validated display/color is valid")
    }

    pub fn with_invert(mut self, invert: bool) -> Self {
        self.invert = invert;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamma_curve_maps_display_values_without_changing_window() {
        let curve = TransferCurve::gamma(2.0).unwrap();

        assert_eq!(curve.gamma_value(), 2.0);
        assert!((curve.map_normalized(0.25) - 0.5).abs() < 1.0e-6);
        assert_eq!(curve.map_normalized(-1.0), 0.0);
        assert_eq!(curve.map_normalized(2.0), 1.0);
    }

    #[test]
    fn rejects_invalid_transfer_gamma_and_preset_ids() {
        assert!(TransferCurve::gamma(0.0).is_err());
        assert!(TransferCurve::gamma(f32::NAN).is_err());
        assert!(TransferPresetId::new("").is_err());
        assert!(TransferPresetId::new("bad/preset").is_err());
    }

    #[test]
    fn channel_transfer_function_roundtrips_through_json() {
        let display =
            LayerDisplay::new(true, DisplayWindow::new(10.0, 20.0).unwrap(), 0.75).unwrap();
        let color = ChannelColor::new([1.0, 0.0, 0.5, 1.0]).unwrap();
        let transfer = ChannelTransferFunction::new(
            display,
            color,
            TransferCurve::gamma(1.5).unwrap(),
            TransferPresetId::new("magenta_gamma").unwrap(),
        )
        .unwrap()
        .with_invert(true);

        let encoded = serde_json::to_string(&transfer).unwrap();
        let decoded: ChannelTransferFunction = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded, transfer);
    }
}

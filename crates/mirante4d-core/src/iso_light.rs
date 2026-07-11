use glam::DVec3;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::CameraAxes;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsoLightMode {
    AttachedCamera,
    DetachedScreen,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IsoLightScreenPosition {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IsoLightState {
    pub mode: IsoLightMode,
    pub detached_screen_position: IsoLightScreenPosition,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum IsoLightStateError {
    #[error("ISO detached light coordinates must be finite")]
    NonFiniteDetachedPosition,
}

impl Default for IsoLightScreenPosition {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0 }
    }
}

impl Default for IsoLightState {
    fn default() -> Self {
        Self::attached_camera()
    }
}

impl IsoLightScreenPosition {
    pub fn new_clamped(x: f32, y: f32) -> Result<Self, IsoLightStateError> {
        if !x.is_finite() || !y.is_finite() {
            return Err(IsoLightStateError::NonFiniteDetachedPosition);
        }
        let radius_squared = x * x + y * y;
        if radius_squared > 1.0 {
            let radius = radius_squared.sqrt();
            Ok(Self {
                x: x / radius,
                y: y / radius,
            })
        } else {
            Ok(Self { x, y })
        }
    }

    pub fn validate(self) -> Result<Self, IsoLightStateError> {
        Self::new_clamped(self.x, self.y)
    }
}

impl IsoLightState {
    pub fn attached_camera() -> Self {
        Self {
            mode: IsoLightMode::AttachedCamera,
            detached_screen_position: IsoLightScreenPosition::default(),
        }
    }

    pub fn detached_screen(x: f32, y: f32) -> Result<Self, IsoLightStateError> {
        Ok(Self {
            mode: IsoLightMode::DetachedScreen,
            detached_screen_position: IsoLightScreenPosition::new_clamped(x, y)?,
        })
    }

    pub fn with_detached_screen_position(
        mut self,
        x: f32,
        y: f32,
    ) -> Result<Self, IsoLightStateError> {
        self.mode = IsoLightMode::DetachedScreen;
        self.detached_screen_position = IsoLightScreenPosition::new_clamped(x, y)?;
        Ok(self)
    }

    pub fn reset_attached(self) -> Self {
        Self {
            mode: IsoLightMode::AttachedCamera,
            detached_screen_position: self.detached_screen_position,
        }
    }

    pub fn validate(self) -> Result<Self, IsoLightStateError> {
        Ok(Self {
            mode: self.mode,
            detached_screen_position: self.detached_screen_position.validate()?,
        })
    }

    pub fn light_direction_world(self, camera_axes: CameraAxes) -> DVec3 {
        match self.mode {
            IsoLightMode::AttachedCamera => (-camera_axes.forward).normalize_or_zero(),
            IsoLightMode::DetachedScreen => {
                let position = self
                    .detached_screen_position
                    .validate()
                    .expect("ISO light state was validated before use");
                let radius_squared = (position.x * position.x + position.y * position.y).min(1.0);
                let z = (1.0 - radius_squared).sqrt();
                (camera_axes.right * f64::from(position.x) + camera_axes.up * f64::from(position.y)
                    - camera_axes.forward * f64::from(z))
                .normalize_or_zero()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CameraView, Projection};
    use glam::DQuat;

    #[test]
    fn detached_screen_position_clamps_to_unit_disc() {
        let position = IsoLightScreenPosition::new_clamped(2.0, 0.0).unwrap();
        assert_eq!(position, IsoLightScreenPosition { x: 1.0, y: 0.0 });

        let diagonal = IsoLightScreenPosition::new_clamped(1.0, 1.0).unwrap();
        assert!((diagonal.x - std::f32::consts::FRAC_1_SQRT_2).abs() < 1.0e-6);
        assert!((diagonal.y - std::f32::consts::FRAC_1_SQRT_2).abs() < 1.0e-6);
    }

    #[test]
    fn detached_screen_position_rejects_non_finite_coordinates() {
        assert_eq!(
            IsoLightScreenPosition::new_clamped(f32::NAN, 0.0).unwrap_err(),
            IsoLightStateError::NonFiniteDetachedPosition
        );
    }

    #[test]
    fn detached_light_maps_screen_position_through_camera_basis() {
        let camera = CameraView::new(
            Projection::Orthographic,
            DVec3::ZERO,
            DQuat::IDENTITY,
            10.0,
            320.0,
            40.0,
        );
        let axes = camera.axes();

        let center = IsoLightState::detached_screen(0.0, 0.0)
            .unwrap()
            .light_direction_world(axes);
        assert!(center.abs_diff_eq(-axes.forward, 1.0e-12));

        let right = IsoLightState::detached_screen(1.0, 0.0)
            .unwrap()
            .light_direction_world(axes);
        assert!(right.abs_diff_eq(axes.right, 1.0e-12));
    }

    #[test]
    fn iso_light_state_serializes_as_stable_viewer_state() {
        let state = IsoLightState::detached_screen(0.25, -0.5).unwrap();
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(
            json,
            r#"{"mode":"detached_screen","detached_screen_position":{"x":0.25,"y":-0.5}}"#
        );
    }
}

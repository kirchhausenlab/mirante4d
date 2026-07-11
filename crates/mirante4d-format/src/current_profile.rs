//! Values and operations that belong only to the predecessor `mirante4d-v1`
//! package profile.
//!
//! Canonical scientific geometry, shapes, indices, dtype, and display values
//! come from `mirante4d-domain`. The types here are physical package labels or
//! current-profile composites that disappear with this crate at WP-10C.

use std::fmt;

use glam::{DMat4, DVec3};
use mirante4d_domain::{DisplayWindow, GridToWorld, Opacity, Shape4D, ShapeError, TransferCurve};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

pub const AXES_TZYX: [&str; 4] = ["t", "z", "y", "x"];

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CurrentFormatIdError {
    #[error("{kind} id must not be empty")]
    Empty { kind: &'static str },
    #[error("{kind} id must contain only ASCII letters, digits, '-' or '_', got {value:?}")]
    InvalidCharacters { kind: &'static str, value: String },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CurrentAxisError {
    #[error("axis order must be exactly [\"t\", \"z\", \"y\", \"x\"], got {got:?}")]
    InvalidOrder { got: Vec<String> },
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum CurrentTransformError {
    #[error("grid-to-world matrix must be invertible")]
    NonInvertibleTransform,
    #[error("downsample factors must be positive")]
    InvalidDownsampleFactor,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DatasetId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LayerId(String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorldUnit {
    Micrometer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldSpace {
    pub name: String,
    pub unit: WorldUnit,
}

/// The display subset serialized by the current package profile.
///
/// Color remains in `ChannelMetadata`; curve and inversion are viewer/project
/// values. Window and opacity are canonical validated domain values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerDisplay {
    visible: bool,
    window: DisplayWindow,
    opacity: Opacity,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LayerDisplayWire {
    visible: bool,
    window: DisplayWindowWire,
    opacity: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayWindowWire {
    low: f32,
    high: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldToGrid {
    matrix: DMat4,
}

pub trait CurrentGridToWorldExt {
    fn to_dmat4(self) -> DMat4;
    fn transform_point_vec(self, grid_xyz: DVec3) -> DVec3;
    fn transform_vector(self, grid_vector: DVec3) -> DVec3;
    fn inverse(self) -> Result<WorldToGrid, CurrentTransformError>;
    fn downsampled_integer_centered(
        self,
        factor_x: u64,
        factor_y: u64,
        factor_z: u64,
    ) -> Result<GridToWorld, CurrentTransformError>;
}

pub trait CurrentShape4DExt {
    fn to_zarr_shape(self) -> Vec<u64>;
    fn chunk_grid(self, chunk_shape: Self) -> Result<Self, ShapeError>
    where
        Self: Sized;
}

impl DatasetId {
    pub fn new(value: impl Into<String>) -> Result<Self, CurrentFormatIdError> {
        validate_id("dataset", value.into()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl LayerId {
    pub fn new(value: impl Into<String>) -> Result<Self, CurrentFormatIdError> {
        validate_id("layer", value.into()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DatasetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl fmt::Display for LayerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl LayerDisplay {
    pub fn new(
        visible: bool,
        window: DisplayWindow,
        opacity: f32,
    ) -> Result<Self, mirante4d_domain::DisplayError> {
        Ok(Self {
            visible,
            window,
            opacity: Opacity::new(opacity)?,
        })
    }

    pub const fn visible(self) -> bool {
        self.visible
    }

    pub const fn window(self) -> DisplayWindow {
        self.window
    }

    pub const fn opacity(self) -> Opacity {
        self.opacity
    }

    pub fn layer_transfer(
        self,
        color: mirante4d_domain::RgbColor,
    ) -> mirante4d_domain::LayerTransfer {
        mirante4d_domain::LayerTransfer::new(
            self.window,
            color,
            self.opacity,
            TransferCurve::linear(),
            false,
        )
    }
}

impl Serialize for LayerDisplay {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        LayerDisplayWire {
            visible: self.visible,
            window: DisplayWindowWire {
                low: self.window.low(),
                high: self.window.high(),
            },
            opacity: self.opacity.get(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LayerDisplay {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = LayerDisplayWire::deserialize(deserializer)?;
        let window = DisplayWindow::new(wire.window.low, wire.window.high)
            .map_err(serde::de::Error::custom)?;
        Self::new(wire.visible, window, wire.opacity).map_err(serde::de::Error::custom)
    }
}

impl CurrentGridToWorldExt for GridToWorld {
    fn to_dmat4(self) -> DMat4 {
        let row_major = self.row_major();
        let mut column_major = [0.0; 16];
        for row in 0..4 {
            for column in 0..4 {
                column_major[column * 4 + row] = row_major[row * 4 + column];
            }
        }
        DMat4::from_cols_array(&column_major)
    }

    fn transform_point_vec(self, grid_xyz: DVec3) -> DVec3 {
        self.to_dmat4().transform_point3(grid_xyz)
    }

    fn transform_vector(self, grid_vector: DVec3) -> DVec3 {
        self.to_dmat4().transform_vector3(grid_vector)
    }

    fn inverse(self) -> Result<WorldToGrid, CurrentTransformError> {
        let matrix = self.to_dmat4();
        let inverse = matrix.inverse();
        if inverse.is_finite() && (matrix * inverse).abs_diff_eq(DMat4::IDENTITY, 1.0e-9) {
            Ok(WorldToGrid { matrix: inverse })
        } else {
            Err(CurrentTransformError::NonInvertibleTransform)
        }
    }

    fn downsampled_integer_centered(
        self,
        factor_x: u64,
        factor_y: u64,
        factor_z: u64,
    ) -> Result<GridToWorld, CurrentTransformError> {
        if factor_x == 0 || factor_y == 0 || factor_z == 0 {
            return Err(CurrentTransformError::InvalidDownsampleFactor);
        }
        let factors = DVec3::new(factor_x as f64, factor_y as f64, factor_z as f64);
        let offset = (factors - DVec3::ONE) * 0.5;
        let coarse_to_source_grid = DMat4::from_translation(offset) * DMat4::from_scale(factors);
        grid_to_world_from_dmat4(self.to_dmat4() * coarse_to_source_grid)
            .map_err(|_| CurrentTransformError::NonInvertibleTransform)
    }
}

impl WorldToGrid {
    pub fn transform_point(self, world_xyz: DVec3) -> DVec3 {
        self.matrix.transform_point3(world_xyz)
    }

    pub fn transform_vector(self, world_vector: DVec3) -> DVec3 {
        self.matrix.transform_vector3(world_vector)
    }
}

impl CurrentShape4DExt for Shape4D {
    fn to_zarr_shape(self) -> Vec<u64> {
        self.dimensions().to_vec()
    }

    fn chunk_grid(self, chunk_shape: Self) -> Result<Self, ShapeError> {
        Shape4D::new(
            self.t().div_ceil(chunk_shape.t()),
            self.z().div_ceil(chunk_shape.z()),
            self.y().div_ceil(chunk_shape.y()),
            self.x().div_ceil(chunk_shape.x()),
        )
    }
}

pub fn validate_axes_tzyx<S>(axes: &[S]) -> Result<(), CurrentAxisError>
where
    S: AsRef<str>,
{
    if axes.len() == AXES_TZYX.len()
        && axes
            .iter()
            .zip(AXES_TZYX)
            .all(|(actual, expected)| actual.as_ref() == expected)
    {
        Ok(())
    } else {
        Err(CurrentAxisError::InvalidOrder {
            got: axes.iter().map(|axis| axis.as_ref().to_owned()).collect(),
        })
    }
}

pub fn grid_to_world_from_dmat4(
    matrix: DMat4,
) -> Result<GridToWorld, mirante4d_domain::GeometryError> {
    let column_major = matrix.to_cols_array();
    let mut row_major = [0.0; 16];
    for row in 0..4 {
        for column in 0..4 {
            row_major[row * 4 + column] = column_major[column * 4 + row];
        }
    }
    GridToWorld::from_row_major(row_major)
}

/// Builds the micrometer transform used by the current package profile.
/// Callers validate physical spacing before reaching this constructor.
pub fn grid_to_world_scale_um(x_um: f64, y_um: f64, z_um: f64) -> GridToWorld {
    GridToWorld::scale(x_um, y_um, z_um).expect("current-profile voxel spacing must be finite")
}

fn validate_id(kind: &'static str, value: String) -> Result<String, CurrentFormatIdError> {
    if value.is_empty() {
        return Err(CurrentFormatIdError::Empty { kind });
    }
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        Ok(value)
    } else {
        Err(CurrentFormatIdError::InvalidCharacters { kind, value })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_ids_remain_strict_current_profile_labels() {
        assert_eq!(LayerId::new("ch0").unwrap().as_str(), "ch0");
        assert!(DatasetId::new("bad/path").is_err());
    }

    #[test]
    fn shape_grid_and_transform_operations_keep_predecessor_behavior() {
        let shape = Shape4D::new(3, 17, 32, 33).unwrap();
        let chunk = Shape4D::new(1, 16, 16, 16).unwrap();
        assert_eq!(shape.chunk_grid(chunk).unwrap().dimensions(), [3, 2, 2, 3]);

        let base = GridToWorld::scale(0.2, 0.3, 0.5).unwrap();
        let coarse = base.downsampled_integer_centered(2, 4, 8).unwrap();
        let origin = coarse.transform_point_vec(DVec3::ZERO);
        assert!(origin.abs_diff_eq(DVec3::new(0.1, 0.45, 1.75), 1.0e-12));
        let round_trip = coarse.inverse().unwrap().transform_point(origin);
        assert!(round_trip.abs_diff_eq(DVec3::ZERO, 1.0e-9));
    }

    #[test]
    fn layer_display_wire_conversion_validates_domain_values() {
        let display = LayerDisplay::new(true, DisplayWindow::new(1.0, 4.0).unwrap(), 0.75).unwrap();
        let json = serde_json::to_string(&display).unwrap();
        assert_eq!(
            serde_json::from_str::<LayerDisplay>(&json).unwrap(),
            display
        );
        assert!(
            serde_json::from_str::<LayerDisplay>(
                r#"{"visible":true,"window":{"low":1.0,"high":1.0},"opacity":1.0}"#
            )
            .is_err()
        );
    }
}

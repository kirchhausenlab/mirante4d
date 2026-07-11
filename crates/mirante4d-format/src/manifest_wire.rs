//! Private current-profile wire conversions for canonical domain values.

use mirante4d_domain::{GridToWorld, IntensityDType, Shape4D};
use serde::{Deserialize, Serialize};

pub(crate) mod shape4d {
    use super::*;

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wire {
        t: u64,
        z: u64,
        y: u64,
        x: u64,
    }

    pub(crate) fn serialize<S>(value: &Shape4D, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let [t, z, y, x] = value.dimensions();
        Wire { t, z, y, x }.serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<Shape4D, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = Wire::deserialize(deserializer)?;
        Shape4D::new(wire.t, wire.z, wire.y, wire.x).map_err(serde::de::Error::custom)
    }
}

pub(crate) mod dtype {
    use super::*;

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    enum Wire {
        Uint8,
        Uint16,
        Float32,
    }

    pub(crate) fn serialize<S>(value: &IntensityDType, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match value {
            IntensityDType::Uint8 => Wire::Uint8,
            IntensityDType::Uint16 => Wire::Uint16,
            IntensityDType::Float32 => Wire::Float32,
        }
        .serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<IntensityDType, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Uint8 => IntensityDType::Uint8,
            Wire::Uint16 => IntensityDType::Uint16,
            Wire::Float32 => IntensityDType::Float32,
        })
    }
}

pub(crate) mod grid_to_world {
    use super::*;

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wire {
        matrix4x4_row_major: [f64; 16],
    }

    pub(crate) fn serialize<S>(value: &GridToWorld, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        Wire {
            matrix4x4_row_major: value.row_major(),
        }
        .serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<GridToWorld, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = Wire::deserialize(deserializer)?;
        GridToWorld::from_row_major(wire.matrix4x4_row_major).map_err(serde::de::Error::custom)
    }
}

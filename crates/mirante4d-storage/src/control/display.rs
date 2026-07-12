use mirante4d_domain::LogicalLayerKey;
use serde::{Deserialize, Serialize};

use super::{ControlError, F32Bits, MAX_PORTABLE_CONTROL_OBJECT_BYTES, Rgb24, U64Decimal, jcs};

const OBJECT: &str = "display defaults";
const SCHEMA: &str = "m4d-display-defaults";

/// One validated logical-layer display default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayLayerDefaults {
    logical_layer: LogicalLayerKey,
    visible: bool,
    color: Rgb24,
    window_min: F32Bits,
    window_max: F32Bits,
}

impl DisplayLayerDefaults {
    pub fn new(
        logical_layer: LogicalLayerKey,
        visible: bool,
        color: Rgb24,
        window_min: F32Bits,
        window_max: F32Bits,
    ) -> Result<Self, ControlError> {
        if window_min.value() >= window_max.value() {
            return Err(ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "each display window must have min < max",
            });
        }
        Ok(Self {
            logical_layer,
            visible,
            color,
            window_min,
            window_max,
        })
    }

    pub const fn logical_layer(&self) -> LogicalLayerKey {
        self.logical_layer
    }

    pub const fn visible(&self) -> bool {
        self.visible
    }

    pub const fn color(&self) -> Rgb24 {
        self.color
    }

    pub const fn window_min(&self) -> F32Bits {
        self.window_min
    }

    pub const fn window_max(&self) -> F32Bits {
        self.window_max
    }
}

/// A structurally validated closed version-1 display-defaults control object.
///
/// Object-local validation requires ordered unique layers. Package validation
/// must separately require nonempty exact layer membership against the profile
/// and scientific descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayDefaults {
    layers: Vec<DisplayLayerDefaults>,
}

impl DisplayDefaults {
    pub fn new(layers: Vec<DisplayLayerDefaults>) -> Result<Self, ControlError> {
        require_layer_order(&layers)?;
        Ok(Self { layers })
    }

    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ControlError> {
        if bytes.len() > MAX_PORTABLE_CONTROL_OBJECT_BYTES {
            return Err(ControlError::ControlObjectTooLarge {
                object: OBJECT,
                maximum: MAX_PORTABLE_CONTROL_OBJECT_BYTES,
            });
        }
        let wire: WireDisplayDefaults = serde_json::from_slice(bytes).map_err(|error| {
            ControlError::MalformedControlObject {
                object: OBJECT,
                detail: error.to_string(),
            }
        })?;
        let value = Self::try_from(wire)?;
        if value.canonical_bytes()?.as_slice() != bytes {
            return Err(ControlError::NonCanonicalControlObject { object: OBJECT });
        }
        Ok(value)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ControlError> {
        require_layer_order(&self.layers)?;
        let wire = WireDisplayDefaults::from(self);
        let value =
            serde_json::to_value(wire).map_err(|error| ControlError::MalformedControlObject {
                object: OBJECT,
                detail: error.to_string(),
            })?;
        jcs::encode(&value, OBJECT, MAX_PORTABLE_CONTROL_OBJECT_BYTES)
    }

    pub fn layers(&self) -> &[DisplayLayerDefaults] {
        &self.layers
    }
}

fn require_layer_order(layers: &[DisplayLayerDefaults]) -> Result<(), ControlError> {
    if !layers
        .windows(2)
        .all(|pair| pair[0].logical_layer.ordinal() < pair[1].logical_layer.ordinal())
    {
        return Err(ControlError::InvalidControlObject {
            object: OBJECT,
            reason: "display layers must be strictly sorted and unique",
        });
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDisplayDefaults {
    schema: String,
    schema_version: u64,
    layers: Vec<WireDisplayLayerDefaults>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDisplayLayerDefaults {
    logical_layer_ordinal: String,
    visible: bool,
    color_rgb: String,
    window_min_f32_bits: String,
    window_max_f32_bits: String,
}

impl TryFrom<WireDisplayDefaults> for DisplayDefaults {
    type Error = ControlError;

    fn try_from(wire: WireDisplayDefaults) -> Result<Self, Self::Error> {
        if wire.schema != SCHEMA || wire.schema_version != 1 {
            return Err(ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "the schema identity must be m4d-display-defaults version 1",
            });
        }
        wire.layers
            .into_iter()
            .map(DisplayLayerDefaults::try_from)
            .collect::<Result<Vec<_>, _>>()
            .and_then(Self::new)
    }
}

impl TryFrom<WireDisplayLayerDefaults> for DisplayLayerDefaults {
    type Error = ControlError;

    fn try_from(wire: WireDisplayLayerDefaults) -> Result<Self, Self::Error> {
        let ordinal = U64Decimal::parse(&wire.logical_layer_ordinal)?.get();
        let ordinal = u32::try_from(ordinal).map_err(|_| ControlError::InvalidControlObject {
            object: OBJECT,
            reason: "logical layer ordinal exceeds u32",
        })?;
        Self::new(
            LogicalLayerKey::new(ordinal),
            wire.visible,
            Rgb24::parse(&wire.color_rgb)?,
            F32Bits::parse(&wire.window_min_f32_bits)?,
            F32Bits::parse(&wire.window_max_f32_bits)?,
        )
    }
}

impl From<&DisplayDefaults> for WireDisplayDefaults {
    fn from(value: &DisplayDefaults) -> Self {
        Self {
            schema: SCHEMA.to_owned(),
            schema_version: 1,
            layers: value
                .layers
                .iter()
                .map(WireDisplayLayerDefaults::from)
                .collect(),
        }
    }
}

impl From<&DisplayLayerDefaults> for WireDisplayLayerDefaults {
    fn from(value: &DisplayLayerDefaults) -> Self {
        Self {
            logical_layer_ordinal: value.logical_layer.ordinal().to_string(),
            visible: value.visible,
            color_rgb: value.color.to_string(),
            window_min_f32_bits: value.window_min.to_string(),
            window_max_f32_bits: value.window_max.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(ordinal: u32, minimum: &str, maximum: &str) -> DisplayLayerDefaults {
        DisplayLayerDefaults::new(
            LogicalLayerKey::new(ordinal),
            true,
            Rgb24::parse("ff007f").unwrap(),
            F32Bits::parse(minimum).unwrap(),
            F32Bits::parse(maximum).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn display_defaults_roundtrip_exact_canonical_bytes() {
        let value = DisplayDefaults::new(vec![
            layer(0, "00000000", "3f800000"),
            layer(2, "bf800000", "40000000"),
        ])
        .unwrap();
        let expected = r#"{"layers":[{"color_rgb":"ff007f","logical_layer_ordinal":"0","visible":true,"window_max_f32_bits":"3f800000","window_min_f32_bits":"00000000"},{"color_rgb":"ff007f","logical_layer_ordinal":"2","visible":true,"window_max_f32_bits":"40000000","window_min_f32_bits":"bf800000"}],"schema":"m4d-display-defaults","schema_version":1}"#;

        let bytes = value.canonical_bytes().unwrap();
        assert_eq!(bytes, expected.as_bytes());
        assert_eq!(DisplayDefaults::parse_canonical(&bytes).unwrap(), value);
        assert_eq!(value.layers()[1].logical_layer().ordinal(), 2);

        // Gaps and emptiness remain structurally representable until the later
        // cross-object package validator has the authoritative layer set.
        assert!(DisplayDefaults::new(Vec::new()).is_ok());
    }

    #[test]
    fn display_defaults_reject_malformed_noncanonical_and_invalid_layers() {
        for wire in [
            r#"{"layers":[],"schema":"m4d-display-defaults","schema":"m4d-display-defaults","schema_version":1}"#,
            r#"{"extra":false,"layers":[],"schema":"m4d-display-defaults","schema_version":1}"#,
            r#"{ "layers":[],"schema":"m4d-display-defaults","schema_version":1}"#,
            r#"{"layers":[],"schema":"wrong","schema_version":1}"#,
            r#"{"layers":[],"schema":"m4d-display-defaults","schema_version":3}"#,
            r#"{"layers":[{"color_rgb":"ff007f","logical_layer_ordinal":"01","visible":true,"window_max_f32_bits":"3f800000","window_min_f32_bits":"00000000"}],"schema":"m4d-display-defaults","schema_version":1}"#,
            r#"{"layers":[{"color_rgb":"FF007F","logical_layer_ordinal":"0","visible":true,"window_max_f32_bits":"3f800000","window_min_f32_bits":"00000000"}],"schema":"m4d-display-defaults","schema_version":1}"#,
            r#"{"layers":[{"color_rgb":"ff007f","logical_layer_ordinal":"0","visible":true,"window_max_f32_bits":"7f800000","window_min_f32_bits":"00000000"}],"schema":"m4d-display-defaults","schema_version":1}"#,
            r#"{"layers":[{"color_rgb":"ff007f","logical_layer_ordinal":"0","visible":true,"window_max_f32_bits":"3f800000","window_min_f32_bits":"3f800000"}],"schema":"m4d-display-defaults","schema_version":1}"#,
            r#"{"layers":[{"color_rgb":"ff007f","logical_layer_ordinal":"2","visible":true,"window_max_f32_bits":"3f800000","window_min_f32_bits":"00000000"},{"color_rgb":"ff007f","logical_layer_ordinal":"1","visible":true,"window_max_f32_bits":"3f800000","window_min_f32_bits":"00000000"}],"schema":"m4d-display-defaults","schema_version":1}"#,
        ] {
            assert!(
                DisplayDefaults::parse_canonical(wire.as_bytes()).is_err(),
                "accepted {wire}"
            );
        }

        assert!(
            DisplayDefaults::new(vec![
                layer(0, "00000000", "3f800000"),
                layer(0, "00000000", "40000000"),
            ])
            .is_err()
        );
        assert!(
            DisplayLayerDefaults::new(
                LogicalLayerKey::new(0),
                true,
                Rgb24::parse("000000").unwrap(),
                F32Bits::parse("3f800000").unwrap(),
                F32Bits::parse("00000000").unwrap(),
            )
            .is_err()
        );
        assert!(
            DisplayDefaults::parse_canonical(&vec![b' '; MAX_PORTABLE_CONTROL_OBJECT_BYTES + 1])
                .is_err()
        );
    }
}

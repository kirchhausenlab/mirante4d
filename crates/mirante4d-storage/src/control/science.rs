use mirante4d_domain::{GridToWorld, IntensityDType, LogicalLayerKey, Shape4D};
use mirante4d_identity::ScientificContentId;
use serde::{Deserialize, Serialize};

use super::{ControlError, F64Bits, MAX_PORTABLE_CONTROL_OBJECT_BYTES, U64Decimal, jcs};

const OBJECT: &str = "scientific descriptor";
const SCHEMA: &str = "m4d-science";
const SEMANTIC_SCHEMA: &str = "m4d-science-1.0";
const VOXEL_CENTER_CONVENTION: &str = "integer coordinates address voxel centers";
const SPATIAL_UNIT: &str = "micrometer";

/// The closed temporal-calibration variant set for one scientific layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScienceTemporalKind {
    Unknown,
    Regular,
    Explicit,
}

/// A validated version-1 relative-second temporal calibration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScienceTemporalCalibration(TemporalKind);

#[derive(Clone, Debug, PartialEq, Eq)]
enum TemporalKind {
    Unknown,
    Regular(F64Bits),
    Explicit(Vec<F64Bits>),
}

impl ScienceTemporalCalibration {
    pub const fn unknown() -> Self {
        Self(TemporalKind::Unknown)
    }

    pub fn regular(step_seconds: F64Bits) -> Result<Self, ControlError> {
        if step_seconds.value() <= 0.0 {
            return invalid("regular temporal step must be positive");
        }
        Ok(Self(TemporalKind::Regular(step_seconds)))
    }

    pub fn explicit(positions_seconds: Vec<F64Bits>) -> Result<Self, ControlError> {
        if positions_seconds
            .first()
            .is_none_or(|position| position.bits() != 0)
        {
            return invalid("explicit temporal positions must begin at positive zero");
        }
        if !positions_seconds
            .windows(2)
            .all(|pair| pair[0].value() < pair[1].value())
        {
            return invalid("explicit temporal positions must be strictly increasing");
        }
        Ok(Self(TemporalKind::Explicit(positions_seconds)))
    }

    pub const fn kind(&self) -> ScienceTemporalKind {
        match self.0 {
            TemporalKind::Unknown => ScienceTemporalKind::Unknown,
            TemporalKind::Regular(_) => ScienceTemporalKind::Regular,
            TemporalKind::Explicit(_) => ScienceTemporalKind::Explicit,
        }
    }

    pub const fn regular_step_seconds(&self) -> Option<F64Bits> {
        match self.0 {
            TemporalKind::Regular(value) => Some(value),
            _ => None,
        }
    }

    pub fn explicit_positions_seconds(&self) -> Option<&[F64Bits]> {
        match &self.0 {
            TemporalKind::Explicit(values) => Some(values),
            _ => None,
        }
    }

    fn validate_timepoints(&self, timepoints: u64) -> Result<(), ControlError> {
        if let TemporalKind::Explicit(positions) = &self.0
            && u64::try_from(positions.len()).ok() != Some(timepoints)
        {
            return invalid("explicit temporal position count must equal base shape t");
        }
        Ok(())
    }
}

/// One normalized layer in a version-1 scientific descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScienceLayer {
    logical_layer: LogicalLayerKey,
    base_shape: Shape4D,
    dtype: IntensityDType,
    temporal_calibration: ScienceTemporalCalibration,
    grid_to_world_micrometer_f64_bits: [F64Bits; 16],
}

impl ScienceLayer {
    pub fn new(
        logical_layer: LogicalLayerKey,
        base_shape: Shape4D,
        dtype: IntensityDType,
        temporal_calibration: ScienceTemporalCalibration,
        grid_to_world_micrometer_f64_bits: [F64Bits; 16],
    ) -> Result<Self, ControlError> {
        temporal_calibration.validate_timepoints(base_shape.t())?;
        let transform = grid_to_world_micrometer_f64_bits.map(F64Bits::normalized_zero);
        GridToWorld::from_row_major(transform.map(F64Bits::value)).map_err(|_| {
            ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "grid-to-world transform must be a finite affine matrix",
            }
        })?;
        Ok(Self {
            logical_layer,
            base_shape,
            dtype,
            temporal_calibration,
            grid_to_world_micrometer_f64_bits: transform,
        })
    }

    pub const fn logical_layer(&self) -> LogicalLayerKey {
        self.logical_layer
    }

    pub const fn base_shape(&self) -> Shape4D {
        self.base_shape
    }

    pub const fn dtype(&self) -> IntensityDType {
        self.dtype
    }

    pub const fn temporal_calibration(&self) -> &ScienceTemporalCalibration {
        &self.temporal_calibration
    }

    pub const fn grid_to_world_micrometer_f64_bits(&self) -> &[F64Bits; 16] {
        &self.grid_to_world_micrometer_f64_bits
    }
}

/// A structurally validated closed version-1 scientific descriptor.
///
/// The later streaming package validator must recompute and match the claimed
/// scientific-content identity from the exact layer data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScienceDescriptor {
    scientific_content_id: ScientificContentId,
    layers: Vec<ScienceLayer>,
}

impl ScienceDescriptor {
    pub fn new(
        scientific_content_id: ScientificContentId,
        layers: Vec<ScienceLayer>,
    ) -> Result<Self, ControlError> {
        require_science_layers(&layers)?;
        Ok(Self {
            scientific_content_id,
            layers,
        })
    }

    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ControlError> {
        if bytes.len() > MAX_PORTABLE_CONTROL_OBJECT_BYTES {
            return Err(ControlError::ControlObjectTooLarge {
                object: OBJECT,
                maximum: MAX_PORTABLE_CONTROL_OBJECT_BYTES,
            });
        }
        let wire: WireScienceDescriptor = serde_json::from_slice(bytes).map_err(|error| {
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
        require_science_layers(&self.layers)?;
        let wire = WireScienceDescriptor::from(self);
        let value =
            serde_json::to_value(wire).map_err(|error| ControlError::MalformedControlObject {
                object: OBJECT,
                detail: error.to_string(),
            })?;
        jcs::encode(&value, OBJECT, MAX_PORTABLE_CONTROL_OBJECT_BYTES)
    }

    pub const fn scientific_content_id(&self) -> ScientificContentId {
        self.scientific_content_id
    }

    pub fn layers(&self) -> &[ScienceLayer] {
        &self.layers
    }
}

fn require_science_layers(layers: &[ScienceLayer]) -> Result<(), ControlError> {
    if layers.is_empty() {
        return invalid("scientific layers must be nonempty");
    }
    for (expected, layer) in layers.iter().enumerate() {
        let expected = u32::try_from(expected).map_err(|_| ControlError::InvalidControlObject {
            object: OBJECT,
            reason: "scientific layer count exceeds u32",
        })?;
        if layer.logical_layer.ordinal() != expected {
            return invalid("scientific layers must have contiguous zero-based ordinals");
        }
    }
    Ok(())
}

fn invalid<T>(reason: &'static str) -> Result<T, ControlError> {
    Err(ControlError::InvalidControlObject {
        object: OBJECT,
        reason,
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireScienceDescriptor {
    schema: String,
    schema_version: u64,
    semantic_schema: String,
    voxel_center_convention: String,
    spatial_unit: String,
    scientific_content_id: String,
    layers: Vec<WireScienceLayer>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireScienceLayer {
    logical_layer_ordinal: String,
    base_shape_tzyx: [String; 4],
    dtype: String,
    temporal_calibration: WireTemporalCalibration,
    grid_to_world_micrometer_f64_bits: [String; 16],
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum WireTemporalCalibration {
    #[serde(rename = "unknown")]
    Unknown,
    #[serde(rename = "regular")]
    Regular { step_seconds_f64_bits: String },
    #[serde(rename = "explicit")]
    Explicit {
        positions_seconds_f64_bits: Vec<String>,
    },
}

impl TryFrom<WireScienceDescriptor> for ScienceDescriptor {
    type Error = ControlError;

    fn try_from(wire: WireScienceDescriptor) -> Result<Self, Self::Error> {
        if wire.schema != SCHEMA
            || wire.schema_version != 1
            || wire.semantic_schema != SEMANTIC_SCHEMA
            || wire.voxel_center_convention != VOXEL_CENTER_CONVENTION
            || wire.spatial_unit != SPATIAL_UNIT
        {
            return invalid("the scientific descriptor fixed schema values are invalid");
        }
        let scientific_content_id = ScientificContentId::parse(&wire.scientific_content_id)
            .map_err(|_| ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "scientific_content_id is invalid",
            })?;
        let layers = wire
            .layers
            .into_iter()
            .map(ScienceLayer::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(scientific_content_id, layers)
    }
}

impl TryFrom<WireScienceLayer> for ScienceLayer {
    type Error = ControlError;

    fn try_from(wire: WireScienceLayer) -> Result<Self, Self::Error> {
        let ordinal = U64Decimal::parse(&wire.logical_layer_ordinal)?.get();
        let ordinal = u32::try_from(ordinal).map_err(|_| ControlError::InvalidControlObject {
            object: OBJECT,
            reason: "logical layer ordinal exceeds u32",
        })?;
        let [t, z, y, x] = wire.base_shape_tzyx;
        let base_shape = Shape4D::new(
            U64Decimal::parse(&t)?.get(),
            U64Decimal::parse(&z)?.get(),
            U64Decimal::parse(&y)?.get(),
            U64Decimal::parse(&x)?.get(),
        )
        .map_err(|_| ControlError::InvalidControlObject {
            object: OBJECT,
            reason: "base shape must contain four positive dimensions with a bounded product",
        })?;
        let dtype = match wire.dtype.as_str() {
            "uint8" => IntensityDType::Uint8,
            "uint16" => IntensityDType::Uint16,
            "float32" => IntensityDType::Float32,
            _ => return invalid("scientific dtype is not admitted"),
        };
        let temporal_calibration = ScienceTemporalCalibration::try_from(wire.temporal_calibration)?;
        let transform = wire
            .grid_to_world_micrometer_f64_bits
            .into_iter()
            .map(|value| F64Bits::parse(&value))
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "grid-to-world transform must have sixteen values",
            })?;
        Self::new(
            LogicalLayerKey::new(ordinal),
            base_shape,
            dtype,
            temporal_calibration,
            transform,
        )
    }
}

impl TryFrom<WireTemporalCalibration> for ScienceTemporalCalibration {
    type Error = ControlError;

    fn try_from(wire: WireTemporalCalibration) -> Result<Self, Self::Error> {
        match wire {
            WireTemporalCalibration::Unknown => Ok(Self::unknown()),
            WireTemporalCalibration::Regular {
                step_seconds_f64_bits,
            } => Self::regular(F64Bits::parse(&step_seconds_f64_bits)?),
            WireTemporalCalibration::Explicit {
                positions_seconds_f64_bits,
            } => positions_seconds_f64_bits
                .into_iter()
                .map(|value| F64Bits::parse(&value))
                .collect::<Result<Vec<_>, _>>()
                .and_then(Self::explicit),
        }
    }
}

impl From<&ScienceDescriptor> for WireScienceDescriptor {
    fn from(value: &ScienceDescriptor) -> Self {
        Self {
            schema: SCHEMA.to_owned(),
            schema_version: 1,
            semantic_schema: SEMANTIC_SCHEMA.to_owned(),
            voxel_center_convention: VOXEL_CENTER_CONVENTION.to_owned(),
            spatial_unit: SPATIAL_UNIT.to_owned(),
            scientific_content_id: value.scientific_content_id.to_string(),
            layers: value.layers.iter().map(WireScienceLayer::from).collect(),
        }
    }
}

impl From<&ScienceLayer> for WireScienceLayer {
    fn from(value: &ScienceLayer) -> Self {
        Self {
            logical_layer_ordinal: value.logical_layer.ordinal().to_string(),
            base_shape_tzyx: value.base_shape.dimensions().map(|value| value.to_string()),
            dtype: match value.dtype {
                IntensityDType::Uint8 => "uint8",
                IntensityDType::Uint16 => "uint16",
                IntensityDType::Float32 => "float32",
            }
            .to_owned(),
            temporal_calibration: WireTemporalCalibration::from(&value.temporal_calibration),
            grid_to_world_micrometer_f64_bits: value
                .grid_to_world_micrometer_f64_bits
                .map(|value| value.to_string()),
        }
    }
}

impl From<&ScienceTemporalCalibration> for WireTemporalCalibration {
    fn from(value: &ScienceTemporalCalibration) -> Self {
        match &value.0 {
            TemporalKind::Unknown => Self::Unknown,
            TemporalKind::Regular(step) => Self::Regular {
                step_seconds_f64_bits: step.to_string(),
            },
            TemporalKind::Explicit(positions) => Self::Explicit {
                positions_seconds_f64_bits: positions.iter().map(ToString::to_string).collect(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(value: &str) -> F64Bits {
        F64Bits::parse(value).unwrap()
    }

    fn identity_transform() -> [F64Bits; 16] {
        [
            bits("3ff0000000000000"),
            bits("0000000000000000"),
            bits("0000000000000000"),
            bits("0000000000000000"),
            bits("0000000000000000"),
            bits("3ff0000000000000"),
            bits("0000000000000000"),
            bits("0000000000000000"),
            bits("0000000000000000"),
            bits("0000000000000000"),
            bits("3ff0000000000000"),
            bits("0000000000000000"),
            bits("0000000000000000"),
            bits("0000000000000000"),
            bits("0000000000000000"),
            bits("3ff0000000000000"),
        ]
    }

    fn scientific_id() -> ScientificContentId {
        ScientificContentId::parse(&format!(
            "{}{}",
            ScientificContentId::PREFIX,
            "0".repeat(64)
        ))
        .unwrap()
    }

    fn descriptor(temporal: ScienceTemporalCalibration) -> ScienceDescriptor {
        ScienceDescriptor::new(
            scientific_id(),
            vec![
                ScienceLayer::new(
                    LogicalLayerKey::new(0),
                    Shape4D::new(2, 1, 2, 3).unwrap(),
                    IntensityDType::Uint16,
                    temporal,
                    identity_transform(),
                )
                .unwrap(),
            ],
        )
        .unwrap()
    }

    #[test]
    fn science_descriptor_roundtrips_exact_bytes_and_temporal_variants() {
        let regular =
            descriptor(ScienceTemporalCalibration::regular(bits("3ff0000000000000")).unwrap());
        let expected = r#"{"layers":[{"base_shape_tzyx":["2","1","2","3"],"dtype":"uint16","grid_to_world_micrometer_f64_bits":["3ff0000000000000","0000000000000000","0000000000000000","0000000000000000","0000000000000000","3ff0000000000000","0000000000000000","0000000000000000","0000000000000000","0000000000000000","3ff0000000000000","0000000000000000","0000000000000000","0000000000000000","0000000000000000","3ff0000000000000"],"logical_layer_ordinal":"0","temporal_calibration":{"step_seconds_f64_bits":"3ff0000000000000","type":"regular"}}],"schema":"m4d-science","schema_version":1,"scientific_content_id":"m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000","semantic_schema":"m4d-science-1.0","spatial_unit":"micrometer","voxel_center_convention":"integer coordinates address voxel centers"}"#;
        assert_eq!(regular.canonical_bytes().unwrap(), expected.as_bytes());

        for value in [
            regular,
            descriptor(ScienceTemporalCalibration::unknown()),
            descriptor(
                ScienceTemporalCalibration::explicit(vec![
                    bits("0000000000000000"),
                    bits("3ff0000000000000"),
                ])
                .unwrap(),
            ),
        ] {
            let bytes = value.canonical_bytes().unwrap();
            assert_eq!(ScienceDescriptor::parse_canonical(&bytes).unwrap(), value);
        }
    }

    #[test]
    fn science_descriptor_rejects_malformed_noncanonical_and_invalid_facts() {
        let canonical = descriptor(ScienceTemporalCalibration::unknown())
            .canonical_bytes()
            .unwrap();
        let canonical = String::from_utf8(canonical).unwrap();
        for wire in [
            canonical.replacen("\"schema\":", "\"schema\":\"m4d-science\",\"schema\":", 1),
            canonical.replacen("\"layers\":", "\"extra\":false,\"layers\":", 1),
            format!(" {canonical}"),
            canonical.replacen("\"m4d-science\"", "\"wrong\"", 1),
            canonical.replacen(
                "\"logical_layer_ordinal\":\"0\"",
                "\"logical_layer_ordinal\":\"00\"",
                1,
            ),
            canonical.replacen("\"2\",\"1\",\"2\",\"3\"", "\"0\",\"1\",\"2\",\"3\"", 1),
            canonical.replacen("m4d-sc-v1-sha256:", "m4d-package-v1-sha256:", 1),
        ] {
            assert!(
                ScienceDescriptor::parse_canonical(wire.as_bytes()).is_err(),
                "accepted {wire}"
            );
        }

        assert!(ScienceDescriptor::new(scientific_id(), Vec::new()).is_err());
        let gap = ScienceLayer::new(
            LogicalLayerKey::new(1),
            Shape4D::new(1, 1, 1, 1).unwrap(),
            IntensityDType::Uint8,
            ScienceTemporalCalibration::unknown(),
            identity_transform(),
        )
        .unwrap();
        assert!(ScienceDescriptor::new(scientific_id(), vec![gap]).is_err());
        assert!(ScienceTemporalCalibration::regular(bits("0000000000000000")).is_err());
        assert!(ScienceTemporalCalibration::explicit(vec![bits("8000000000000000")]).is_err());
        assert!(
            ScienceLayer::new(
                LogicalLayerKey::new(0),
                Shape4D::new(2, 1, 1, 1).unwrap(),
                IntensityDType::Uint8,
                ScienceTemporalCalibration::explicit(vec![bits("0000000000000000")]).unwrap(),
                identity_transform(),
            )
            .is_err()
        );
        let mut non_affine = identity_transform();
        non_affine[15] = bits("4000000000000000");
        assert!(
            ScienceLayer::new(
                LogicalLayerKey::new(0),
                Shape4D::new(1, 1, 1, 1).unwrap(),
                IntensityDType::Uint8,
                ScienceTemporalCalibration::unknown(),
                non_affine,
            )
            .is_err()
        );
        let negative_zero = canonical.replacen("0000000000000000", "8000000000000000", 1);
        assert!(ScienceDescriptor::parse_canonical(negative_zero.as_bytes()).is_err());
        assert!(
            ScienceDescriptor::parse_canonical(&vec![b' '; MAX_PORTABLE_CONTROL_OBJECT_BYTES + 1])
                .is_err()
        );
    }
}

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::zarr_metadata::encode_sorted_wire;
use crate::{
    F64Bits, MAX_ZARR_METADATA_BYTES, ProfileImage, ScienceTemporalCalibration, ZarrMetadataError,
};

const OBJECT: &str = "OME image-group metadata";
const LEVEL_COUNT_MAX: usize = 7;
const ZERO_BITS: u64 = 0.0_f64.to_bits();
const ONE_BITS: u64 = 1.0_f64.to_bits();

/// The OME-representable spatial transform for one image-pyramid level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OmeLevelTransform {
    DiagonalMicrometer {
        scale_zyx: [F64Bits; 3],
        translation_zyx: [F64Bits; 3],
    },
    UnitlessIdentity,
}

impl OmeLevelTransform {
    fn normalized(self) -> Self {
        match self {
            Self::DiagonalMicrometer {
                scale_zyx,
                translation_zyx,
            } => Self::DiagonalMicrometer {
                scale_zyx: scale_zyx.map(F64Bits::normalized_zero),
                translation_zyx: translation_zyx.map(F64Bits::normalized_zero),
            },
            Self::UnitlessIdentity => Self::UnitlessIdentity,
        }
    }

    const fn is_diagonal(self) -> bool {
        matches!(self, Self::DiagonalMicrometer { .. })
    }
}

/// Closed OME-NGFF 0.5 metadata for one Mirante4D physical image group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OmeImageGroupMetadata {
    regular_time_step_seconds: Option<F64Bits>,
    level_transforms: Vec<OmeLevelTransform>,
}

impl OmeImageGroupMetadata {
    pub fn new(
        image: &ProfileImage,
        temporal: &ScienceTemporalCalibration,
        level_transforms: Vec<OmeLevelTransform>,
    ) -> Result<Self, ZarrMetadataError> {
        if level_transforms.len() != image.levels().len() {
            return invalid("OME transform count must equal the image level count");
        }
        let regular_time_step_seconds = temporal
            .regular_step_seconds()
            .map(F64Bits::normalized_zero);
        Self::from_parts(regular_time_step_seconds, level_transforms)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, ZarrMetadataError> {
        require_size(bytes)?;
        let wire: WireImageGroup =
            serde_json::from_slice(bytes).map_err(|error| ZarrMetadataError::MalformedJson {
                object: OBJECT,
                message: error.to_string(),
            })?;
        serde_json::from_slice::<zarrs_metadata::v3::GroupMetadataV3>(bytes).map_err(|error| {
            ZarrMetadataError::CoreMetadata {
                object: OBJECT,
                message: error.to_string(),
            }
        })?;
        if wire.zarr_format != 3
            || wire.node_type != "group"
            || wire.attributes.ome.version != "0.5"
            || wire.attributes.ome.multiscales.len() != 1
        {
            return invalid("expected one version-0.5 OME multiscale in a Zarr-v3 group");
        }

        let multiscale = &wire.attributes.ome.multiscales[0];
        let (regular_time, diagonal_spatial) = parse_axes(&multiscale.axes)?;
        if multiscale.datasets.is_empty() || multiscale.datasets.len() > LEVEL_COUNT_MAX {
            return invalid("OME datasets must contain one through seven levels");
        }

        let mut time_scale = None;
        let mut levels = Vec::with_capacity(multiscale.datasets.len());
        for (ordinal, dataset) in multiscale.datasets.iter().enumerate() {
            if dataset.path != format!("s{ordinal:02}") {
                return invalid("OME dataset paths must be contiguous s00 through s06");
            }
            let (scale, translation, translation_present) =
                parse_transform_sequence(&dataset.coordinate_transformations)?;
            reject_negative_zero(&scale, &translation)?;
            if translation_present && translation.iter().all(|value| *value == 0.0) {
                return invalid("an all-zero OME translation must be omitted");
            }
            if scale[1].to_bits() != ONE_BITS
                || translation[0].to_bits() != ZERO_BITS
                || translation[1].to_bits() != ZERO_BITS
            {
                return invalid("OME time/channel transforms violate the frozen profile");
            }
            if regular_time {
                if !scale[0].is_finite() || scale[0] <= 0.0 {
                    return invalid("regular OME time scale must be positive and finite");
                }
                let observed = normalized_f64_bits(scale[0])?;
                if time_scale
                    .replace(observed)
                    .is_some_and(|prior| prior != observed)
                {
                    return invalid("OME time is never downsampled between levels");
                }
            } else if scale[0].to_bits() != ONE_BITS {
                return invalid("uncalibrated OME time scale must be one");
            }

            levels.push(if diagonal_spatial {
                OmeLevelTransform::DiagonalMicrometer {
                    scale_zyx: [
                        normalized_f64_bits(scale[2])?,
                        normalized_f64_bits(scale[3])?,
                        normalized_f64_bits(scale[4])?,
                    ],
                    translation_zyx: [
                        normalized_f64_bits(translation[2])?,
                        normalized_f64_bits(translation[3])?,
                        normalized_f64_bits(translation[4])?,
                    ],
                }
            } else {
                if scale[2..].iter().any(|value| value.to_bits() != ONE_BITS)
                    || translation[2..]
                        .iter()
                        .any(|value| value.to_bits() != ZERO_BITS)
                {
                    return invalid(
                        "unitless affine projection must use identity OME spatial transforms",
                    );
                }
                OmeLevelTransform::UnitlessIdentity
            });
        }

        Self::from_parts(time_scale, levels)
    }

    pub fn deterministic_bytes(&self) -> Result<Vec<u8>, ZarrMetadataError> {
        validate_parts(self.regular_time_step_seconds, &self.level_transforms)?;
        encode_sorted_wire(&WireImageGroup::from(self), OBJECT)
    }

    pub const fn regular_time_step_seconds(&self) -> Option<F64Bits> {
        self.regular_time_step_seconds
    }

    pub fn level_transforms(&self) -> &[OmeLevelTransform] {
        &self.level_transforms
    }

    fn from_parts(
        regular_time_step_seconds: Option<F64Bits>,
        level_transforms: Vec<OmeLevelTransform>,
    ) -> Result<Self, ZarrMetadataError> {
        let level_transforms = level_transforms
            .into_iter()
            .map(OmeLevelTransform::normalized)
            .collect::<Vec<_>>();
        validate_parts(regular_time_step_seconds, &level_transforms)?;
        Ok(Self {
            regular_time_step_seconds,
            level_transforms,
        })
    }
}

fn validate_parts(
    regular_time_step_seconds: Option<F64Bits>,
    levels: &[OmeLevelTransform],
) -> Result<(), ZarrMetadataError> {
    if levels.is_empty() || levels.len() > LEVEL_COUNT_MAX {
        return invalid("OME datasets must contain one through seven levels");
    }
    if regular_time_step_seconds.is_some_and(|step| step.value() <= 0.0) {
        return invalid("regular OME time scale must be positive and finite");
    }
    let diagonal = levels[0].is_diagonal();
    if levels.iter().any(|level| level.is_diagonal() != diagonal) {
        return invalid("one OME image cannot mix calibrated and unitless spatial axes");
    }
    Ok(())
}

fn parse_axes(axes: &[WireAxis]) -> Result<(bool, bool), ZarrMetadataError> {
    if axes.len() != 5
        || !axis_matches(&axes[0], "t", "time")
        || !axis_matches(&axes[1], "c", "channel")
        || !axis_matches(&axes[2], "z", "space")
        || !axis_matches(&axes[3], "y", "space")
        || !axis_matches(&axes[4], "x", "space")
        || !axes[1].unit.is_absent()
    {
        return invalid("OME axes must be exactly ordered t,c,z,y,x");
    }
    let regular_time = if axes[0].unit.is_exact("second") {
        true
    } else if axes[0].unit.is_absent() {
        false
    } else {
        return invalid("OME time unit must be second or omitted");
    };
    let diagonal_spatial = if axes[2..]
        .iter()
        .all(|axis| axis.unit.is_exact("micrometer"))
    {
        true
    } else if axes[2..].iter().all(|axis| axis.unit.is_absent()) {
        false
    } else {
        return invalid("OME spatial units must be all micrometer or all omitted");
    };
    Ok((regular_time, diagonal_spatial))
}

fn axis_matches(axis: &WireAxis, name: &str, kind: &str) -> bool {
    axis.name == name && axis.kind == kind
}

fn parse_transform_sequence(
    transforms: &[WireCoordinateTransformation],
) -> Result<([f64; 5], [f64; 5], bool), ZarrMetadataError> {
    match transforms {
        [WireCoordinateTransformation::Scale { scale }] => Ok((*scale, [0.0; 5], false)),
        [
            WireCoordinateTransformation::Scale { scale },
            WireCoordinateTransformation::Translation { translation },
        ] => Ok((*scale, *translation, true)),
        _ => invalid("each OME level requires scale then at most one translation"),
    }
}

fn reject_negative_zero(scale: &[f64; 5], translation: &[f64; 5]) -> Result<(), ZarrMetadataError> {
    if scale
        .iter()
        .chain(translation)
        .any(|value| *value == 0.0 && value.is_sign_negative())
    {
        return invalid("stored OME transforms must not contain negative zero");
    }
    Ok(())
}

fn normalized_f64_bits(value: f64) -> Result<F64Bits, ZarrMetadataError> {
    F64Bits::from_finite_value(value).ok_or(ZarrMetadataError::Invalid {
        object: OBJECT,
        reason: "OME transform values must be finite",
    })
}

fn require_size(bytes: &[u8]) -> Result<(), ZarrMetadataError> {
    if bytes.is_empty() || bytes.len() > MAX_ZARR_METADATA_BYTES {
        return Err(ZarrMetadataError::Size {
            object: OBJECT,
            maximum: MAX_ZARR_METADATA_BYTES,
        });
    }
    Ok(())
}

fn invalid<T>(reason: &'static str) -> Result<T, ZarrMetadataError> {
    Err(ZarrMetadataError::Invalid {
        object: OBJECT,
        reason,
    })
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireImageGroup {
    zarr_format: u64,
    node_type: String,
    attributes: WireAttributes,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireAttributes {
    ome: WireOme,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireOme {
    version: String,
    multiscales: Vec<WireMultiscale>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireMultiscale {
    axes: Vec<WireAxis>,
    datasets: Vec<WireDataset>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireAxis {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, skip_serializing_if = "OptionalStringMember::is_absent")]
    unit: OptionalStringMember,
}

#[derive(Debug, Default)]
struct OptionalStringMember {
    present: bool,
    value: Option<String>,
}

impl OptionalStringMember {
    fn present(value: &str) -> Self {
        Self {
            present: true,
            value: Some(value.to_owned()),
        }
    }

    const fn absent() -> Self {
        Self {
            present: false,
            value: None,
        }
    }

    const fn is_absent(&self) -> bool {
        !self.present
    }

    fn is_exact(&self, value: &str) -> bool {
        self.present && self.value.as_deref() == Some(value)
    }
}

impl<'de> Deserialize<'de> for OptionalStringMember {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self {
            present: true,
            value: Option::<String>::deserialize(deserializer)?,
        })
    }
}

impl Serialize for OptionalStringMember {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.value.serialize(serializer)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireDataset {
    path: String,
    #[serde(rename = "coordinateTransformations")]
    coordinate_transformations: Vec<WireCoordinateTransformation>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum WireCoordinateTransformation {
    #[serde(rename = "scale")]
    Scale { scale: [f64; 5] },
    #[serde(rename = "translation")]
    Translation { translation: [f64; 5] },
}

impl From<&OmeImageGroupMetadata> for WireImageGroup {
    fn from(metadata: &OmeImageGroupMetadata) -> Self {
        let regular_time = metadata.regular_time_step_seconds.is_some();
        let diagonal_spatial = metadata.level_transforms[0].is_diagonal();
        let axes = [
            wire_axis("t", "time", regular_time.then_some("second")),
            wire_axis("c", "channel", None),
            wire_axis("z", "space", diagonal_spatial.then_some("micrometer")),
            wire_axis("y", "space", diagonal_spatial.then_some("micrometer")),
            wire_axis("x", "space", diagonal_spatial.then_some("micrometer")),
        ]
        .into();
        let time_scale = metadata
            .regular_time_step_seconds
            .map_or(1.0, F64Bits::value);
        let datasets = metadata
            .level_transforms
            .iter()
            .enumerate()
            .map(|(ordinal, level)| wire_dataset(ordinal, time_scale, *level))
            .collect();
        Self {
            zarr_format: 3,
            node_type: "group".to_owned(),
            attributes: WireAttributes {
                ome: WireOme {
                    version: "0.5".to_owned(),
                    multiscales: vec![WireMultiscale { axes, datasets }],
                },
            },
        }
    }
}

fn wire_axis(name: &str, kind: &str, unit: Option<&str>) -> WireAxis {
    WireAxis {
        name: name.to_owned(),
        kind: kind.to_owned(),
        unit: unit.map_or_else(OptionalStringMember::absent, OptionalStringMember::present),
    }
}

fn wire_dataset(ordinal: usize, time_scale: f64, level: OmeLevelTransform) -> WireDataset {
    let (spatial_scale, spatial_translation) = match level {
        OmeLevelTransform::DiagonalMicrometer {
            scale_zyx,
            translation_zyx,
        } => (
            scale_zyx.map(F64Bits::value),
            translation_zyx.map(F64Bits::value),
        ),
        OmeLevelTransform::UnitlessIdentity => ([1.0; 3], [0.0; 3]),
    };
    let scale = [
        time_scale,
        1.0,
        spatial_scale[0],
        spatial_scale[1],
        spatial_scale[2],
    ];
    let translation = [
        0.0,
        0.0,
        spatial_translation[0],
        spatial_translation[1],
        spatial_translation[2],
    ];
    let mut coordinate_transformations = vec![WireCoordinateTransformation::Scale { scale }];
    if translation.iter().any(|value| *value != 0.0) {
        coordinate_transformations.push(WireCoordinateTransformation::Translation { translation });
    }
    WireDataset {
        path: format!("s{ordinal:02}"),
        coordinate_transformations,
    }
}

#[cfg(test)]
mod tests {
    use mirante4d_domain::LogicalLayerKey;
    use serde_json::{Value, json};

    use super::*;
    use crate::{ProfileLevel, ProfileLogicalLayer, ProfileValidityMode};

    fn bits(value: &str) -> F64Bits {
        F64Bits::parse(value).unwrap()
    }

    fn image(level_count: u32) -> ProfileImage {
        let levels = (0..level_count)
            .map(|ordinal| ProfileLevel::new(0, ordinal, ProfileValidityMode::AllValid).unwrap())
            .collect();
        ProfileImage::new(
            0,
            vec![ProfileLogicalLayer::new(LogicalLayerKey::new(0), 0)],
            levels,
        )
        .unwrap()
    }

    fn diagonal(scale: [&str; 3], translation: [&str; 3]) -> OmeLevelTransform {
        OmeLevelTransform::DiagonalMicrometer {
            scale_zyx: scale.map(bits),
            translation_zyx: translation.map(bits),
        }
    }

    #[test]
    fn ome_image_group_emits_exact_regular_diagonal_metadata() {
        let metadata = OmeImageGroupMetadata::new(
            &image(2),
            &ScienceTemporalCalibration::regular(bits("3fe0000000000000")).unwrap(),
            vec![
                diagonal(
                    ["3fd0000000000000", "3fb999999999999a", "3fb999999999999a"],
                    ["0000000000000000"; 3],
                ),
                diagonal(
                    ["3fe0000000000000", "3fc999999999999a", "3fc999999999999a"],
                    ["3fc0000000000000", "0000000000000000", "0000000000000000"],
                ),
            ],
        )
        .unwrap();
        let bytes = metadata.deterministic_bytes().unwrap();
        let expected = br#"{"attributes":{"ome":{"multiscales":[{"axes":[{"name":"t","type":"time","unit":"second"},{"name":"c","type":"channel"},{"name":"z","type":"space","unit":"micrometer"},{"name":"y","type":"space","unit":"micrometer"},{"name":"x","type":"space","unit":"micrometer"}],"datasets":[{"coordinateTransformations":[{"scale":[0.5,1.0,0.25,0.1,0.1],"type":"scale"}],"path":"s00"},{"coordinateTransformations":[{"scale":[0.5,1.0,0.5,0.2,0.2],"type":"scale"},{"translation":[0.0,0.0,0.125,0.0,0.0],"type":"translation"}],"path":"s01"}]}],"version":"0.5"}},"node_type":"group","zarr_format":3}"#;
        assert_eq!(bytes, expected);
        assert_eq!(OmeImageGroupMetadata::parse(&bytes).unwrap(), metadata);
    }

    #[test]
    fn ome_image_group_projects_nonregular_and_affine_semantics() {
        for temporal in [
            ScienceTemporalCalibration::unknown(),
            ScienceTemporalCalibration::explicit(vec![bits("0000000000000000")]).unwrap(),
        ] {
            let metadata = OmeImageGroupMetadata::new(
                &image(1),
                &temporal,
                vec![OmeLevelTransform::UnitlessIdentity],
            )
            .unwrap();
            let bytes = metadata.deterministic_bytes().unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            let axes = &value["attributes"]["ome"]["multiscales"][0]["axes"];
            assert!(axes[0].get("unit").is_none());
            assert!(axes[2].get("unit").is_none());
            assert_eq!(
                value["attributes"]["ome"]["multiscales"][0]["datasets"][0]["coordinateTransformations"]
                    [0]["scale"],
                json!([1.0, 1.0, 1.0, 1.0, 1.0])
            );
            assert_eq!(OmeImageGroupMetadata::parse(&bytes).unwrap(), metadata);
        }
    }

    #[test]
    fn ome_image_group_rejects_alternate_unknown_and_oversized_metadata() {
        let metadata = OmeImageGroupMetadata::new(
            &image(1),
            &ScienceTemporalCalibration::unknown(),
            vec![OmeLevelTransform::UnitlessIdentity],
        )
        .unwrap();
        let bytes = metadata.deterministic_bytes().unwrap();
        let base: Value = serde_json::from_slice(&bytes).unwrap();

        let mut value = base.clone();
        value["attributes"]["ome"]["omero"] = json!({});
        assert!(OmeImageGroupMetadata::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        value = base.clone();
        value["attributes"]["ome"]["multiscales"][0]["datasets"][0]["path"] = Value::from("s01");
        assert!(OmeImageGroupMetadata::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        value = base.clone();
        value["attributes"]["ome"]["multiscales"][0]["datasets"][0]["coordinateTransformations"]
            [0]["scale"][1] = Value::from(2.0);
        assert!(OmeImageGroupMetadata::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        value = base.clone();
        value["attributes"]["ome"]["multiscales"][0]["datasets"][0]["coordinateTransformations"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type": "translation", "translation": [0.0, 0.0, 0.0, 0.0, 0.0]}));
        assert!(OmeImageGroupMetadata::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        value = base;
        value["attributes"]["ome"]["multiscales"][0]["datasets"][0]["coordinateTransformations"]
            [0]["scale"][0] = Value::from(-0.0);
        assert!(OmeImageGroupMetadata::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        assert!(OmeImageGroupMetadata::parse(&vec![b' '; MAX_ZARR_METADATA_BYTES + 1]).is_err());
        assert!(
            OmeImageGroupMetadata::new(
                &image(2),
                &ScienceTemporalCalibration::unknown(),
                vec![
                    OmeLevelTransform::UnitlessIdentity,
                    diagonal(["3ff0000000000000"; 3], ["0000000000000000"; 3]),
                ],
            )
            .is_err()
        );
    }
}

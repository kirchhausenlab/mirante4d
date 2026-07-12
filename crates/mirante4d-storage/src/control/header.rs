use std::collections::BTreeSet;

use mirante4d_domain::LogicalLayerKey;
use mirante4d_identity::ScientificContentId;
use serde::{Deserialize, Serialize};

use super::{ControlError, MAX_PROFILE_HEADER_BYTES, U64Decimal, jcs};
use crate::{CAPABILITIES, PROFILE, PackagePath};

const OBJECT: &str = "profile header";
const SCHEMA: &str = "m4d-profile";
const SCIENCE_PATH: &str = "m4d/science.json";
const DISPLAY_PATH: &str = "m4d/display.json";
const MANIFEST_ROOT_PATH: &str = "m4d/manifest/root.json";
const IMAGE_COUNT_MAX: usize = 4;
const LEVEL_COUNT_MAX: usize = 7;
const PORTABLE_RECORD_COUNT_MAX: usize = 14;

/// The maximum OME interoperability claim stored in a version-1 profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OmeInteroperabilityBase {
    Io1,
    Io2,
}

impl OmeInteroperabilityBase {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Io1 => "IO-1",
            Self::Io2 => "IO-2",
        }
    }
}

/// The closed validity-storage mode for one pixel level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileValidityMode {
    AllValid,
    Explicit,
}

impl ProfileValidityMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::AllValid => "all_valid",
            Self::Explicit => "explicit",
        }
    }
}

/// One logical-to-physical channel mapping in the profile header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileLogicalLayer {
    logical_layer: LogicalLayerKey,
    physical_channel: u32,
}

impl ProfileLogicalLayer {
    pub const fn new(logical_layer: LogicalLayerKey, physical_channel: u32) -> Self {
        Self {
            logical_layer,
            physical_channel,
        }
    }

    pub const fn logical_layer(self) -> LogicalLayerKey {
        self.logical_layer
    }

    pub const fn physical_channel(self) -> u32 {
        self.physical_channel
    }
}

/// One validated multiscale level mapping in the profile header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileLevel {
    image_ordinal: u32,
    scale_ordinal: u32,
    pixel_path: PackagePath,
    validity_mode: ProfileValidityMode,
    validity_path: Option<PackagePath>,
    packed_index_path: PackagePath,
}

impl ProfileLevel {
    pub fn new(
        image_ordinal: u32,
        scale_ordinal: u32,
        validity_mode: ProfileValidityMode,
    ) -> Result<Self, ControlError> {
        if image_ordinal >= IMAGE_COUNT_MAX as u32 || scale_ordinal >= LEVEL_COUNT_MAX as u32 {
            return invalid("image or scale ordinal exceeds the frozen profile bound");
        }
        let image = format!("i{image_ordinal:08}");
        let scale = format!("s{scale_ordinal:02}");
        let pixel_path = package_path(&format!("images/{image}/{scale}"))?;
        let validity_path = match validity_mode {
            ProfileValidityMode::AllValid => None,
            ProfileValidityMode::Explicit => {
                Some(package_path(&format!("validity/{image}-{scale}"))?)
            }
        };
        let packed_index_path = package_path(&format!("indexes/{image}-{scale}"))?;
        Ok(Self {
            image_ordinal,
            scale_ordinal,
            pixel_path,
            validity_mode,
            validity_path,
            packed_index_path,
        })
    }

    pub const fn scale_ordinal(&self) -> u32 {
        self.scale_ordinal
    }

    pub fn pixel_path(&self) -> &PackagePath {
        &self.pixel_path
    }

    pub const fn validity_mode(&self) -> ProfileValidityMode {
        self.validity_mode
    }

    pub fn validity_path(&self) -> Option<&PackagePath> {
        self.validity_path.as_ref()
    }

    pub fn packed_index_path(&self) -> &PackagePath {
        &self.packed_index_path
    }
}

/// One validated physical image-group mapping in the profile header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileImage {
    image_ordinal: u32,
    image_group_path: PackagePath,
    logical_layers: Vec<ProfileLogicalLayer>,
    levels: Vec<ProfileLevel>,
}

impl ProfileImage {
    pub fn new(
        image_ordinal: u32,
        logical_layers: Vec<ProfileLogicalLayer>,
        levels: Vec<ProfileLevel>,
    ) -> Result<Self, ControlError> {
        if image_ordinal >= IMAGE_COUNT_MAX as u32 {
            return invalid("image ordinal exceeds the frozen profile bound");
        }
        if logical_layers.is_empty() {
            return invalid("each image must map at least one logical layer");
        }
        let mut physical_channels = BTreeSet::new();
        for layer in &logical_layers {
            if !physical_channels.insert(layer.physical_channel) {
                return invalid("physical channels must be unique within one image");
            }
        }
        if levels.is_empty() || levels.len() > LEVEL_COUNT_MAX {
            return invalid("each image must contain one through seven levels");
        }
        for (expected, level) in levels.iter().enumerate() {
            if level.image_ordinal != image_ordinal || level.scale_ordinal != expected as u32 {
                return invalid("levels must have contiguous zero-based ordinals for their image");
            }
        }
        Ok(Self {
            image_ordinal,
            image_group_path: package_path(&format!("images/i{image_ordinal:08}"))?,
            logical_layers,
            levels,
        })
    }

    pub const fn image_ordinal(&self) -> u32 {
        self.image_ordinal
    }

    pub fn image_group_path(&self) -> &PackagePath {
        &self.image_group_path
    }

    pub fn logical_layers(&self) -> &[ProfileLogicalLayer] {
        &self.logical_layers
    }

    pub fn levels(&self) -> &[ProfileLevel] {
        &self.levels
    }
}

/// A structurally validated closed version-1 bootstrap profile header.
///
/// Package validation must additionally match channel bounds, the scientific
/// and display layer sets, and the corresponding OME/Zarr metadata. It must
/// also prove regular time and exact diagonal scale/translation before an IO-2
/// claim; this object-local validator already caps explicit validity at IO-1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileHeader {
    scientific_content_id: ScientificContentId,
    science_path: PackagePath,
    display_defaults_path: PackagePath,
    manifest_root_path: PackagePath,
    images: Vec<ProfileImage>,
    portable_record_paths: Vec<PackagePath>,
    ome_interoperability_base: OmeInteroperabilityBase,
}

impl ProfileHeader {
    pub fn new(
        scientific_content_id: ScientificContentId,
        images: Vec<ProfileImage>,
        portable_record_count: u32,
        ome_interoperability_base: OmeInteroperabilityBase,
    ) -> Result<Self, ControlError> {
        validate_images(&images)?;
        if ome_interoperability_base == OmeInteroperabilityBase::Io2
            && images
                .iter()
                .flat_map(|image| &image.levels)
                .any(|level| level.validity_mode == ProfileValidityMode::Explicit)
        {
            return invalid("explicit validity limits the OME interoperability base to IO-1");
        }
        let portable_record_count = usize::try_from(portable_record_count).map_err(|_| {
            ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "portable record count does not fit this platform",
            }
        })?;
        if portable_record_count > PORTABLE_RECORD_COUNT_MAX {
            return invalid("portable record count exceeds fourteen");
        }
        let portable_record_paths = (0..portable_record_count)
            .map(|ordinal| package_path(&format!("m4d/records/r{ordinal:08}.json")))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            scientific_content_id,
            science_path: package_path(SCIENCE_PATH)?,
            display_defaults_path: package_path(DISPLAY_PATH)?,
            manifest_root_path: package_path(MANIFEST_ROOT_PATH)?,
            images,
            portable_record_paths,
            ome_interoperability_base,
        })
    }

    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ControlError> {
        if bytes.len() > MAX_PROFILE_HEADER_BYTES {
            return Err(ControlError::ControlObjectTooLarge {
                object: OBJECT,
                maximum: MAX_PROFILE_HEADER_BYTES,
            });
        }
        let wire: WireProfileHeader = serde_json::from_slice(bytes).map_err(|error| {
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
        validate_images(&self.images)?;
        let wire = WireProfileHeader::from(self);
        let value =
            serde_json::to_value(wire).map_err(|error| ControlError::MalformedControlObject {
                object: OBJECT,
                detail: error.to_string(),
            })?;
        jcs::encode(&value, OBJECT, MAX_PROFILE_HEADER_BYTES)
    }

    pub const fn scientific_content_id(&self) -> ScientificContentId {
        self.scientific_content_id
    }

    pub fn science_path(&self) -> &PackagePath {
        &self.science_path
    }

    pub fn display_defaults_path(&self) -> &PackagePath {
        &self.display_defaults_path
    }

    pub fn manifest_root_path(&self) -> &PackagePath {
        &self.manifest_root_path
    }

    pub fn images(&self) -> &[ProfileImage] {
        &self.images
    }

    pub fn portable_record_paths(&self) -> &[PackagePath] {
        &self.portable_record_paths
    }

    pub const fn ome_interoperability_base(&self) -> OmeInteroperabilityBase {
        self.ome_interoperability_base
    }
}

fn validate_images(images: &[ProfileImage]) -> Result<(), ControlError> {
    if images.is_empty() || images.len() > IMAGE_COUNT_MAX {
        return invalid("the profile must contain one through four images");
    }
    let mut expected_layer = 0_u32;
    for (expected_image, image) in images.iter().enumerate() {
        if image.image_ordinal != expected_image as u32 {
            return invalid("images must have contiguous zero-based ordinals");
        }
        for layer in &image.logical_layers {
            if layer.logical_layer.ordinal() != expected_layer {
                return invalid("logical layers must have package-wide contiguous ordinals");
            }
            expected_layer =
                expected_layer
                    .checked_add(1)
                    .ok_or(ControlError::InvalidControlObject {
                        object: OBJECT,
                        reason: "logical layer ordinal overflowed u32",
                    })?;
        }
    }
    Ok(())
}

fn package_path(value: &str) -> Result<PackagePath, ControlError> {
    PackagePath::parse(value).map_err(|_| ControlError::InvalidControlObject {
        object: OBJECT,
        reason: "profile path violates the portable path grammar",
    })
}

fn invalid<T>(reason: &'static str) -> Result<T, ControlError> {
    Err(ControlError::InvalidControlObject {
        object: OBJECT,
        reason,
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProfileHeader {
    schema: String,
    schema_version: u64,
    compatibility: WireCompatibility,
    required_capabilities: Vec<String>,
    scientific_content_id: String,
    science_path: String,
    display_defaults_path: String,
    manifest_root_path: String,
    images: Vec<WireProfileImage>,
    portable_record_paths: Vec<String>,
    ome_interoperability_base: String,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct WireCompatibility {
    format_family: String,
    lifecycle: String,
    semantic_schema: String,
    storage_profile: String,
    index_profile: String,
    identity_profile: String,
    ome_metadata_version: String,
    ome_release: String,
    zarr_format: u64,
    zarr_core: String,
    required_capabilities: Vec<String>,
    unknown_major_or_required_capability: String,
    compatibility_fallback: String,
}

impl WireCompatibility {
    fn frozen() -> Self {
        Self {
            format_family: PROFILE.format_family.to_owned(),
            lifecycle: PROFILE.lifecycle.to_owned(),
            semantic_schema: PROFILE.semantic_schema.to_owned(),
            storage_profile: PROFILE.storage_profile.to_owned(),
            index_profile: PROFILE.index_profile.to_owned(),
            identity_profile: PROFILE.identity_profile.to_owned(),
            ome_metadata_version: PROFILE.ome_metadata_version.to_owned(),
            ome_release: PROFILE.ome_release.to_owned(),
            zarr_format: u64::from(PROFILE.zarr_format),
            zarr_core: PROFILE.zarr_core.to_owned(),
            required_capabilities: CAPABILITIES.map(str::to_owned).to_vec(),
            unknown_major_or_required_capability: "reject".to_owned(),
            compatibility_fallback: "forbidden".to_owned(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProfileImage {
    image_ordinal: String,
    image_group_path: String,
    logical_layers: Vec<WireProfileLogicalLayer>,
    levels: Vec<WireProfileLevel>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProfileLogicalLayer {
    logical_layer_ordinal: String,
    physical_channel: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProfileLevel {
    scale_ordinal: String,
    pixel_path: String,
    validity_mode: String,
    validity_path: Option<String>,
    packed_index_path: String,
}

impl TryFrom<WireProfileHeader> for ProfileHeader {
    type Error = ControlError;

    fn try_from(wire: WireProfileHeader) -> Result<Self, Self::Error> {
        if wire.schema != SCHEMA
            || wire.schema_version != 1
            || wire.compatibility != WireCompatibility::frozen()
            || wire.required_capabilities != CAPABILITIES
            || wire.science_path != SCIENCE_PATH
            || wire.display_defaults_path != DISPLAY_PATH
            || wire.manifest_root_path != MANIFEST_ROOT_PATH
        {
            return invalid(
                "the profile fixed schema, compatibility, capability, or path values are invalid",
            );
        }
        let scientific_content_id = ScientificContentId::parse(&wire.scientific_content_id)
            .map_err(|_| ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "scientific_content_id is invalid",
            })?;
        let images = wire
            .images
            .into_iter()
            .map(ProfileImage::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let record_count = u32::try_from(wire.portable_record_paths.len()).map_err(|_| {
            ControlError::InvalidControlObject {
                object: OBJECT,
                reason: "portable record count exceeds u32",
            }
        })?;
        let ome_interoperability_base = match wire.ome_interoperability_base.as_str() {
            "IO-1" => OmeInteroperabilityBase::Io1,
            "IO-2" => OmeInteroperabilityBase::Io2,
            _ => return invalid("OME interoperability base must be IO-1 or IO-2"),
        };
        let value = Self::new(
            scientific_content_id,
            images,
            record_count,
            ome_interoperability_base,
        )?;
        if wire.portable_record_paths
            != value
                .portable_record_paths
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        {
            return invalid("portable record paths must be contiguous and canonical");
        }
        Ok(value)
    }
}

impl TryFrom<WireProfileImage> for ProfileImage {
    type Error = ControlError;

    fn try_from(wire: WireProfileImage) -> Result<Self, Self::Error> {
        let image_ordinal = parse_u32(&wire.image_ordinal, "image ordinal exceeds u32")?;
        let logical_layers = wire
            .logical_layers
            .into_iter()
            .map(ProfileLogicalLayer::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let levels = wire
            .levels
            .into_iter()
            .map(|level| ProfileLevel::from_wire(image_ordinal, level))
            .collect::<Result<Vec<_>, _>>()?;
        let value = Self::new(image_ordinal, logical_layers, levels)?;
        if wire.image_group_path != value.image_group_path.as_str() {
            return invalid("image group path does not match its ordinal");
        }
        Ok(value)
    }
}

impl TryFrom<WireProfileLogicalLayer> for ProfileLogicalLayer {
    type Error = ControlError;

    fn try_from(wire: WireProfileLogicalLayer) -> Result<Self, Self::Error> {
        Ok(Self::new(
            LogicalLayerKey::new(parse_u32(
                &wire.logical_layer_ordinal,
                "logical layer ordinal exceeds u32",
            )?),
            parse_u32(&wire.physical_channel, "physical channel exceeds u32")?,
        ))
    }
}

impl ProfileLevel {
    fn from_wire(image_ordinal: u32, wire: WireProfileLevel) -> Result<Self, ControlError> {
        let scale_ordinal = parse_u32(&wire.scale_ordinal, "scale ordinal exceeds u32")?;
        let validity_mode = match wire.validity_mode.as_str() {
            "all_valid" => ProfileValidityMode::AllValid,
            "explicit" => ProfileValidityMode::Explicit,
            _ => return invalid("validity mode is not admitted"),
        };
        let value = Self::new(image_ordinal, scale_ordinal, validity_mode)?;
        if wire.pixel_path != value.pixel_path.as_str()
            || wire.validity_path.as_deref()
                != value.validity_path.as_ref().map(PackagePath::as_str)
            || wire.packed_index_path != value.packed_index_path.as_str()
        {
            return invalid(
                "level paths or validity nullability do not match their ordinals and mode",
            );
        }
        Ok(value)
    }
}

fn parse_u32(value: &str, reason: &'static str) -> Result<u32, ControlError> {
    u32::try_from(U64Decimal::parse(value)?.get()).map_err(|_| ControlError::InvalidControlObject {
        object: OBJECT,
        reason,
    })
}

impl From<&ProfileHeader> for WireProfileHeader {
    fn from(value: &ProfileHeader) -> Self {
        Self {
            schema: SCHEMA.to_owned(),
            schema_version: 1,
            compatibility: WireCompatibility::frozen(),
            required_capabilities: CAPABILITIES.map(str::to_owned).to_vec(),
            scientific_content_id: value.scientific_content_id.to_string(),
            science_path: value.science_path.to_string(),
            display_defaults_path: value.display_defaults_path.to_string(),
            manifest_root_path: value.manifest_root_path.to_string(),
            images: value.images.iter().map(WireProfileImage::from).collect(),
            portable_record_paths: value
                .portable_record_paths
                .iter()
                .map(ToString::to_string)
                .collect(),
            ome_interoperability_base: value.ome_interoperability_base.as_str().to_owned(),
        }
    }
}

impl From<&ProfileImage> for WireProfileImage {
    fn from(value: &ProfileImage) -> Self {
        Self {
            image_ordinal: value.image_ordinal.to_string(),
            image_group_path: value.image_group_path.to_string(),
            logical_layers: value
                .logical_layers
                .iter()
                .map(WireProfileLogicalLayer::from)
                .collect(),
            levels: value.levels.iter().map(WireProfileLevel::from).collect(),
        }
    }
}

impl From<&ProfileLogicalLayer> for WireProfileLogicalLayer {
    fn from(value: &ProfileLogicalLayer) -> Self {
        Self {
            logical_layer_ordinal: value.logical_layer.ordinal().to_string(),
            physical_channel: value.physical_channel.to_string(),
        }
    }
}

impl From<&ProfileLevel> for WireProfileLevel {
    fn from(value: &ProfileLevel) -> Self {
        Self {
            scale_ordinal: value.scale_ordinal.to_string(),
            pixel_path: value.pixel_path.to_string(),
            validity_mode: value.validity_mode.as_str().to_owned(),
            validity_path: value.validity_path.as_ref().map(ToString::to_string),
            packed_index_path: value.packed_index_path.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scientific_id() -> ScientificContentId {
        ScientificContentId::parse(&format!(
            "{}{}",
            ScientificContentId::PREFIX,
            "0".repeat(64)
        ))
        .unwrap()
    }

    fn header(mode: ProfileValidityMode) -> ProfileHeader {
        let interoperability = match mode {
            ProfileValidityMode::AllValid => OmeInteroperabilityBase::Io2,
            ProfileValidityMode::Explicit => OmeInteroperabilityBase::Io1,
        };
        ProfileHeader::new(
            scientific_id(),
            vec![
                ProfileImage::new(
                    0,
                    vec![ProfileLogicalLayer::new(LogicalLayerKey::new(0), 0)],
                    vec![ProfileLevel::new(0, 0, mode).unwrap()],
                )
                .unwrap(),
            ],
            1,
            interoperability,
        )
        .unwrap()
    }

    #[test]
    fn profile_header_roundtrips_exact_frozen_bytes() {
        let value = header(ProfileValidityMode::Explicit);
        let expected = r#"{"compatibility":{"compatibility_fallback":"forbidden","format_family":"mirante4d","identity_profile":"m4d-id-1","index_profile":"m4d-packed-index-1.0","lifecycle":"EXPERIMENTAL","ome_metadata_version":"0.5","ome_release":"0.5.2","required_capabilities":["m4d.bit-validity.v1","m4d.identity.v1","m4d.packed-index.v1","m4d.strict-profile.v1","zarr.sharding-indexed.v1"],"semantic_schema":"m4d-science-1.0","storage_profile":"m4d-zarr3-local-1.0","unknown_major_or_required_capability":"reject","zarr_core":"3.0","zarr_format":3},"display_defaults_path":"m4d/display.json","images":[{"image_group_path":"images/i00000000","image_ordinal":"0","levels":[{"packed_index_path":"indexes/i00000000-s00","pixel_path":"images/i00000000/s00","scale_ordinal":"0","validity_mode":"explicit","validity_path":"validity/i00000000-s00"}],"logical_layers":[{"logical_layer_ordinal":"0","physical_channel":"0"}]}],"manifest_root_path":"m4d/manifest/root.json","ome_interoperability_base":"IO-1","portable_record_paths":["m4d/records/r00000000.json"],"required_capabilities":["m4d.bit-validity.v1","m4d.identity.v1","m4d.packed-index.v1","m4d.strict-profile.v1","zarr.sharding-indexed.v1"],"schema":"m4d-profile","schema_version":1,"science_path":"m4d/science.json","scientific_content_id":"m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000"}"#;

        let bytes = value.canonical_bytes().unwrap();
        assert_eq!(bytes, expected.as_bytes());
        assert_eq!(ProfileHeader::parse_canonical(&bytes).unwrap(), value);
    }

    #[test]
    fn profile_header_rejects_malformed_noncanonical_and_inconsistent_mappings() {
        let canonical = String::from_utf8(
            header(ProfileValidityMode::AllValid)
                .canonical_bytes()
                .unwrap(),
        )
        .unwrap();
        for wire in [
            canonical.replacen("\"schema\":", "\"schema\":\"m4d-profile\",\"schema\":", 1),
            canonical.replacen("\"images\":", "\"package_id\":\"forbidden\",\"images\":", 1),
            format!(" {canonical}"),
            canonical.replacen(
                "\"lifecycle\":\"EXPERIMENTAL\"",
                "\"lifecycle\":\"STABLE\"",
                1,
            ),
            canonical.replacen("m4d/science.json", "science.json", 1),
            canonical.replacen("\"image_ordinal\":\"0\"", "\"image_ordinal\":\"00\"", 1),
            canonical.replacen(
                "\"validity_mode\":\"all_valid\",\"validity_path\":null",
                "\"validity_mode\":\"explicit\",\"validity_path\":null",
                1,
            ),
            canonical.replacen("r00000000.json", "r00000001.json", 1),
        ] {
            assert!(
                ProfileHeader::parse_canonical(wire.as_bytes()).is_err(),
                "accepted {wire}"
            );
        }

        assert!(
            ProfileHeader::new(scientific_id(), Vec::new(), 0, OmeInteroperabilityBase::Io1)
                .is_err()
        );
        let duplicate_channels = ProfileImage::new(
            0,
            vec![
                ProfileLogicalLayer::new(LogicalLayerKey::new(0), 0),
                ProfileLogicalLayer::new(LogicalLayerKey::new(1), 0),
            ],
            vec![ProfileLevel::new(0, 0, ProfileValidityMode::AllValid).unwrap()],
        );
        assert!(duplicate_channels.is_err());
        let explicit = ProfileImage::new(
            0,
            vec![ProfileLogicalLayer::new(LogicalLayerKey::new(0), 0)],
            vec![ProfileLevel::new(0, 0, ProfileValidityMode::Explicit).unwrap()],
        )
        .unwrap();
        assert!(
            ProfileHeader::new(
                scientific_id(),
                vec![explicit],
                0,
                OmeInteroperabilityBase::Io2
            )
            .is_err()
        );
        let gap = ProfileImage::new(
            0,
            vec![ProfileLogicalLayer::new(LogicalLayerKey::new(1), 0)],
            vec![ProfileLevel::new(0, 0, ProfileValidityMode::AllValid).unwrap()],
        )
        .unwrap();
        assert!(
            ProfileHeader::new(scientific_id(), vec![gap], 0, OmeInteroperabilityBase::Io1)
                .is_err()
        );
        assert!(ProfileHeader::parse_canonical(&vec![b' '; MAX_PROFILE_HEADER_BYTES + 1]).is_err());
    }
}

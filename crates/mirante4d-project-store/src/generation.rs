//! Closed generation and logical-object-binding codecs for the project store.
//!
//! These types are deliberately crate-private. They map the validated project
//! model to the one frozen on-disk schema; they are neither a second durable
//! model nor a compatibility boundary.

// The private transaction slice consumes the write-side surface. Read-side
// accessors remain unreachable until the later open/recovery slice.
#![cfg_attr(not(test), allow(dead_code))]

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    marker::PhantomData,
};

use mirante4d_domain::{
    CameraView, CrossSectionView, DisplayWindow, DvrOpacityTransfer, IsoLightState,
    IsoShadingPolicy, LayerTransfer, LogicalLayerKey, Opacity, Projection, RenderMode, RenderState,
    RgbColor, SamplingPolicy, TimeIndex, TransferCurve, UnitQuaternion, ViewerLayout, WorldPoint3,
};
use mirante4d_identity::{
    ArtifactContentId, DerivationRecordId, ExactBytesDigest, ExactBytesHasher, MediaType,
    ObjectRole, PackageId, RawObjectDescriptor, RecipeId, ReleaseId, ScientificContentId,
};
use mirante4d_project_model::{
    ArtifactCompleteness, ArtifactHandleId, ArtifactRecoverability, ArtifactReference,
    ArtifactSchema, ChannelPreset, ChannelPresetEntry, ChannelPresetId, DatasetLocatorHint,
    DatasetReference, LayerViewState, MAX_ARTIFACT_SOURCE_LAYERS, MAX_ARTIFACTS,
    MAX_CHANNEL_PRESET_ENTRIES, MAX_CHANNEL_PRESETS, MAX_TOTAL_ARTIFACT_SOURCE_LAYER_REFERENCES,
    MAX_TOTAL_CHANNEL_PRESET_ENTRIES, MAX_VIEW_LAYERS, ProjectGenerationProjection, ProjectId,
    ProjectRevisionHighWater, ProjectRevisionId, ProjectState, ViewState,
};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, IgnoredAny, SeqAccess, Visitor},
};
use thiserror::Error;

use crate::{
    ProjectCommitCapture, ProjectGenerationId, ProjectStoreLimits,
    wire::{WireError, encode_canonical_json, generation_id_from_validated_canonical},
};

const GENERATION_SCHEMA: &str = "mirante4d-project-generation";
const GENERATION_SCHEMA_VERSION: u32 = 1;
const BINDING_SCHEMA: &str = "mirante4d-project-logical-object-binding";
const BINDING_SCHEMA_VERSION: u32 = 1;
const BINDING_MEDIA_TYPE: &str = "application/vnd.mirante4d.project-object-binding-v1+json";
const BINDING_ROLE: &str = "project.object-binding.v1";
pub(crate) const PAGE_BYTES: u64 = 16_777_216;
const GENERATION_BYTES_MAX: usize = 67_108_864;
const OBJECT_BYTES_MAX: usize = 16_777_216;
const REACHABLE_OBJECTS_MAX: usize = 65_536;

#[derive(Debug, Error)]
pub(crate) enum GenerationCodecError {
    #[error("generation or binding JSON has an invalid closed shape")]
    JsonShape,
    #[error("generation or binding JSON is not in the canonical byte form")]
    NonCanonical,
    #[error("generation or binding schema is not the frozen version")]
    Schema,
    #[error("a typed project identity is invalid")]
    Identity,
    #[error("the generation does not belong to the expected project")]
    ProjectMismatch,
    #[error("the generation identity does not match its canonical bytes")]
    GenerationIdentity,
    #[error("capacity was exceeded while validating {stage}")]
    Capacity { stage: &'static str },
    #[error("invalid persisted project value in {stage}")]
    Semantic { stage: &'static str },
    #[error("artifact storage does not close over the project artifacts")]
    ArtifactClosure,
    #[error("the logical-object binding does not match its expected descriptor")]
    BindingMismatch,
    #[error(transparent)]
    Wire(#[from] WireError),
}

/// Manual and autosave generations share one schema but remain separate lanes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GenerationKind {
    Manual,
    Autosave,
}

/// Exact physical object facts used by generation closures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PhysicalObject {
    digest: ExactBytesDigest,
    byte_length: u64,
}

impl PhysicalObject {
    pub(crate) const fn new(digest: ExactBytesDigest, byte_length: u64) -> Self {
        Self {
            digest,
            byte_length,
        }
    }

    pub(crate) const fn digest(self) -> ExactBytesDigest {
        self.digest
    }

    pub(crate) const fn byte_length(self) -> u64 {
        self.byte_length
    }

    fn from_raw(descriptor: &RawObjectDescriptor) -> Self {
        Self::new(descriptor.digest(), descriptor.byte_length())
    }
}

/// The physical binding selected for one logical artifact object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ArtifactStorage {
    Direct {
        object: PhysicalObject,
    },
    Paged {
        binding_manifest: RawObjectDescriptor,
    },
}

impl ArtifactStorage {
    pub(crate) fn direct(logical: &RawObjectDescriptor) -> Result<Self, GenerationCodecError> {
        if logical.byte_length() > PAGE_BYTES {
            return Err(GenerationCodecError::ArtifactClosure);
        }
        Ok(Self::Direct {
            object: PhysicalObject::from_raw(logical),
        })
    }

    pub(crate) fn paged(
        logical: &RawObjectDescriptor,
        binding_manifest: RawObjectDescriptor,
    ) -> Result<Self, GenerationCodecError> {
        if logical.byte_length() <= PAGE_BYTES || !is_binding_descriptor(&binding_manifest) {
            return Err(GenerationCodecError::ArtifactClosure);
        }
        Ok(Self::Paged { binding_manifest })
    }

    fn required_physical(&self) -> PhysicalObject {
        match self {
            Self::Direct { object } => *object,
            Self::Paged { binding_manifest } => PhysicalObject::from_raw(binding_manifest),
        }
    }
}

/// One deterministic page in a logical-object binding manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LogicalObjectPage {
    ordinal: u32,
    offset: u64,
    object: PhysicalObject,
}

impl LogicalObjectPage {
    pub(crate) const fn new(ordinal: u32, offset: u64, object: PhysicalObject) -> Self {
        Self {
            ordinal,
            offset,
            object,
        }
    }

    pub(crate) const fn ordinal(self) -> u32 {
        self.ordinal
    }

    pub(crate) const fn offset(self) -> u64 {
        self.offset
    }

    pub(crate) const fn object(self) -> PhysicalObject {
        self.object
    }
}

/// A validated deterministic page plan for one large logical artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogicalObjectBinding {
    logical_descriptor: RawObjectDescriptor,
    pages: Vec<LogicalObjectPage>,
}

/// Canonical binding bytes and their typed immutable-object descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EncodedLogicalObjectBinding {
    descriptor: RawObjectDescriptor,
    bytes: Vec<u8>,
}

impl EncodedLogicalObjectBinding {
    pub(crate) fn descriptor(&self) -> &RawObjectDescriptor {
        &self.descriptor
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// One closed generation reconstructed into the canonical project model.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GenerationDocument {
    kind: GenerationKind,
    generation_sequence: u64,
    parent_generation_id: Option<ProjectGenerationId>,
    base_manual_generation_id: Option<ProjectGenerationId>,
    forked_from: Option<(ProjectId, ProjectGenerationId)>,
    projection: ProjectGenerationProjection,
    bindings: BTreeMap<ExactBytesDigest, ArtifactStorage>,
    reachable_objects: Vec<PhysicalObject>,
}

/// Canonical generation bytes paired with their unforgeable framed identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EncodedGeneration {
    id: ProjectGenerationId,
    bytes: Vec<u8>,
}

impl EncodedGeneration {
    pub(crate) const fn id(&self) -> ProjectGenerationId {
        self.id
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BoundedVec<T, const MAX: usize>(Vec<T>);

/// A nullable field which must still be physically present in the closed JSON
/// record. A bare `Option<T>` would let Serde silently accept a missing member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
struct RequiredNullable<T>(Option<T>);

impl<T> RequiredNullable<T> {
    const fn new(value: Option<T>) -> Self {
        Self(value)
    }

    fn into_option(self) -> Option<T> {
        self.0
    }
}

fn deserialize_required_nullable<'de, D, T>(
    deserializer: D,
) -> Result<RequiredNullable<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(RequiredNullable)
}

impl<T, const MAX: usize> BoundedVec<T, MAX> {
    fn new(values: Vec<T>, stage: &'static str) -> Result<Self, GenerationCodecError> {
        if values.len() > MAX {
            Err(GenerationCodecError::Capacity { stage })
        } else {
            Ok(Self(values))
        }
    }

    fn as_slice(&self) -> &[T] {
        &self.0
    }

    fn into_vec(self) -> Vec<T> {
        self.0
    }
}

impl<T: Serialize, const MAX: usize> Serialize for BoundedVec<T, MAX> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>, const MAX: usize> Deserialize<'de> for BoundedVec<T, MAX> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BoundedVisitor<T, const MAX: usize>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>, const MAX: usize> Visitor<'de> for BoundedVisitor<T, MAX> {
            type Value = BoundedVec<T, MAX>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "an array with at most {MAX} elements")
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(MAX));
                while values.len() < MAX {
                    let Some(value) = sequence.next_element()? else {
                        return Ok(BoundedVec(values));
                    };
                    values.push(value);
                }
                if sequence.next_element::<IgnoredAny>()?.is_some() {
                    return Err(de::Error::custom(format_args!(
                        "array exceeds the {MAX}-element limit"
                    )));
                }
                Ok(BoundedVec(values))
            }
        }

        deserializer.deserialize_seq(BoundedVisitor::<T, MAX>(PhantomData))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct U64String(u64);

impl Serialize for U64String {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for U64String {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct DecimalVisitor;

        impl Visitor<'_> for DecimalVisitor {
            type Value = U64String;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a minimal unsigned decimal string")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value.is_empty()
                    || (value.len() > 1 && value.starts_with('0'))
                    || !value.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(E::custom("invalid minimal u64 string"));
                }
                value
                    .parse::<u64>()
                    .map(U64String)
                    .map_err(|_| E::custom("u64 string overflow"))
            }
        }

        deserializer.deserialize_str(DecimalVisitor)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct F32Bits(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct F64Bits(u64);

macro_rules! bit_string {
    ($type:ident, $bits:ty, $width:literal, $float:ty) => {
        impl $type {
            fn from_value(value: $float) -> Self {
                Self(value.to_bits())
            }

            fn value(self) -> $float {
                <$float>::from_bits(self.0)
            }
        }

        impl Serialize for $type {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&format!(concat!("{:0", $width, "x}"), self.0))
            }
        }

        impl<'de> Deserialize<'de> for $type {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                struct BitsVisitor;

                impl Visitor<'_> for BitsVisitor {
                    type Value = $type;

                    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                        write!(
                            formatter,
                            "exactly {} lowercase hexadecimal characters",
                            $width
                        )
                    }

                    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
                    where
                        E: de::Error,
                    {
                        if value.len() != $width
                            || !value
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                        {
                            return Err(E::custom("invalid IEEE-754 bit string"));
                        }
                        let bits = <$bits>::from_str_radix(value, 16)
                            .map_err(|_| E::custom("invalid IEEE-754 bit string"))?;
                        let decoded = <$float>::from_bits(bits);
                        if !decoded.is_finite() {
                            return Err(E::custom("non-finite IEEE-754 value"));
                        }
                        Ok($type(bits))
                    }
                }

                deserializer.deserialize_str(BitsVisitor)
            }
        }
    };
}

bit_string!(F32Bits, u32, 8, f32);
bit_string!(F64Bits, u64, 16, f64);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationWire {
    artifacts: BoundedVec<ArtifactWire, MAX_ARTIFACTS>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    base_manual_generation_id: RequiredNullable<String>,
    dataset: DatasetWire,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    forked_from: RequiredNullable<ForkWire>,
    generation_kind: GenerationKindWire,
    generation_sequence: U64String,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    parent_generation_id: RequiredNullable<String>,
    project_id: String,
    reachable_objects: BoundedVec<PhysicalObjectWire, REACHABLE_OBJECTS_MAX>,
    revision_high_water_sequence: U64String,
    revision_sequence: U64String,
    schema: String,
    schema_version: u32,
    state: StateWire,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GenerationKindWire {
    Manual,
    Autosave,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForkWire {
    generation_id: String,
    project_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DatasetWire {
    #[serde(deserialize_with = "deserialize_required_nullable")]
    locator_hint: RequiredNullable<String>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    package_id: RequiredNullable<String>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    release_id: RequiredNullable<String>,
    scientific_content_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateWire {
    channel_presets: BoundedVec<ChannelPresetWire, MAX_CHANNEL_PRESETS>,
    view: ViewWire,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewWire {
    active_layer: u32,
    camera: CameraWire,
    cross_section: CrossSectionWire,
    iso_light: IsoLightWire,
    layers: BoundedVec<LayerWire, MAX_VIEW_LAYERS>,
    layout: ViewerLayoutWire,
    timepoint: U64String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LayerWire {
    layer: u32,
    render: RenderWire,
    transfer: TransferWire,
    visible: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransferWire {
    color_rgb: [F32Bits; 3],
    curve: TransferCurveWire,
    invert: bool,
    opacity: F32Bits,
    window: DisplayWindowWire,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayWindowWire {
    high: F32Bits,
    low: F32Bits,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum TransferCurveWire {
    Linear,
    Gamma { value: F32Bits },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum RenderWire {
    Mip {
        sampling: SamplingWire,
    },
    Isosurface {
        display_level: F32Bits,
        sampling: SamplingWire,
        shading: IsoShadingWire,
    },
    Dvr {
        density_scale: F64Bits,
        opacity_transfer: DvrOpacityWire,
        sampling: SamplingWire,
    },
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SamplingWire {
    SmoothLinear,
    VoxelExact,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum IsoShadingWire {
    GradientLighting,
    Flat,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DvrOpacityWire {
    curve: TransferCurveWire,
    window: DisplayWindowWire,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CameraWire {
    orientation_xyzw: [F64Bits; 4],
    orthographic_world_per_screen_point: F64Bits,
    perspective_focal_length_screen_points: F64Bits,
    perspective_view_distance_world: F64Bits,
    projection: ProjectionWire,
    target: [F64Bits; 3],
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProjectionWire {
    Perspective,
    Orthographic,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ViewerLayoutWire {
    Single3d,
    FourPanel,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrossSectionWire {
    center: [F64Bits; 3],
    depth_world: F64Bits,
    orientation_xyzw: [F64Bits; 4],
    scale_world_per_screen_point: F64Bits,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum IsoLightWire {
    AttachedCamera,
    DetachedScreen { x: F32Bits, y: F32Bits },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelPresetWire {
    entries: BoundedVec<LayerWire, MAX_CHANNEL_PRESET_ENTRIES>,
    id: String,
    label: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactWire {
    completeness: ArtifactCompletenessWire,
    content_id: String,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    derivation_id: RequiredNullable<String>,
    handle_id: String,
    label: String,
    logical_object: RawDescriptorWire,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    recipe_id: RequiredNullable<String>,
    recoverability: ArtifactRecoverabilityWire,
    schema: ArtifactSchemaWire,
    source_layers: BoundedVec<u32, MAX_ARTIFACT_SOURCE_LAYERS>,
    storage: ArtifactStorageWire,
    visible: bool,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
enum ArtifactSchemaWire {
    #[serde(rename = "roi.v1")]
    RoiV1,
    #[serde(rename = "track.v1")]
    TrackV1,
    #[serde(rename = "annotation.v1")]
    AnnotationV1,
    #[serde(rename = "measurement.v1")]
    MeasurementV1,
    #[serde(rename = "analysis-table.v1")]
    AnalysisTableV1,
    #[serde(rename = "analysis-plot.v1")]
    AnalysisPlotV1,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArtifactCompletenessWire {
    Partial,
    Complete,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArtifactRecoverabilityWire {
    Regenerable,
    NonRegenerable,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDescriptorWire {
    byte_length: U64String,
    digest: String,
    media_type: String,
    role: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PhysicalObjectWire {
    byte_length: U64String,
    digest: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ArtifactStorageWire {
    Direct { object: PhysicalObjectWire },
    Paged { binding_manifest: RawDescriptorWire },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LogicalBindingWire {
    logical_descriptor: RawDescriptorWire,
    pages: BoundedVec<PageWire, REACHABLE_OBJECTS_MAX>,
    schema: String,
    schema_version: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PageWire {
    byte_length: U64String,
    digest: String,
    offset: U64String,
    ordinal: u32,
}

impl LogicalObjectBinding {
    pub(crate) fn new(
        logical_descriptor: RawObjectDescriptor,
        pages: Vec<LogicalObjectPage>,
        limits: ProjectStoreLimits,
    ) -> Result<Self, GenerationCodecError> {
        let binding = Self {
            logical_descriptor,
            pages,
        };
        binding.validate(limits)?;
        Ok(binding)
    }

    pub(crate) fn decode(
        encoded: &[u8],
        expected_logical: &RawObjectDescriptor,
        expected_binding: &RawObjectDescriptor,
        limits: ProjectStoreLimits,
    ) -> Result<Self, GenerationCodecError> {
        if !is_binding_descriptor(expected_binding) {
            return Err(GenerationCodecError::BindingMismatch);
        }
        validate_byte_limit(
            encoded.len(),
            limits.object_or_page_bytes_max,
            OBJECT_BYTES_MAX,
            "logical object binding bytes",
        )?;
        let wire: LogicalBindingWire =
            serde_json::from_slice(encoded).map_err(|_| GenerationCodecError::JsonShape)?;
        let canonical = encode_canonical_json(&wire)?;
        if canonical != encoded {
            return Err(GenerationCodecError::NonCanonical);
        }
        if wire.schema != BINDING_SCHEMA || wire.schema_version != BINDING_SCHEMA_VERSION {
            return Err(GenerationCodecError::Schema);
        }
        let logical_descriptor = wire.logical_descriptor.into_descriptor()?;
        if &logical_descriptor != expected_logical {
            return Err(GenerationCodecError::BindingMismatch);
        }
        let pages = wire
            .pages
            .into_vec()
            .into_iter()
            .map(PageWire::into_page)
            .collect::<Result<Vec<_>, _>>()?;
        let binding = Self::new(logical_descriptor, pages, limits)?;
        let facts =
            ExactBytesHasher::hash(encoded).map_err(|_| GenerationCodecError::Semantic {
                stage: "binding digest",
            })?;
        if facts.digest() != expected_binding.digest()
            || facts.byte_length() != expected_binding.byte_length()
        {
            return Err(GenerationCodecError::BindingMismatch);
        }
        if binding
            .pages
            .iter()
            .any(|page| page.object.digest == facts.digest())
        {
            return Err(GenerationCodecError::ArtifactClosure);
        }
        Ok(binding)
    }

    pub(crate) fn encode(
        &self,
        limits: ProjectStoreLimits,
    ) -> Result<EncodedLogicalObjectBinding, GenerationCodecError> {
        self.validate(limits)?;
        let wire = LogicalBindingWire {
            logical_descriptor: RawDescriptorWire::from_descriptor(&self.logical_descriptor),
            pages: BoundedVec::new(
                self.pages
                    .iter()
                    .copied()
                    .map(PageWire::from_page)
                    .collect(),
                "logical object pages",
            )?,
            schema: BINDING_SCHEMA.to_owned(),
            schema_version: BINDING_SCHEMA_VERSION,
        };
        let bytes = encode_canonical_json(&wire)?;
        validate_byte_limit(
            bytes.len(),
            limits.object_or_page_bytes_max,
            OBJECT_BYTES_MAX,
            "logical object binding bytes",
        )?;
        let facts = ExactBytesHasher::hash(&bytes).map_err(|_| GenerationCodecError::Semantic {
            stage: "binding digest",
        })?;
        let descriptor = RawObjectDescriptor::new(
            facts.digest(),
            facts.byte_length(),
            MediaType::parse(BINDING_MEDIA_TYPE).expect("the frozen binding media type is valid"),
            ObjectRole::parse(BINDING_ROLE).expect("the frozen binding role is valid"),
        );
        if self
            .pages
            .iter()
            .any(|page| page.object.digest == descriptor.digest())
        {
            return Err(GenerationCodecError::ArtifactClosure);
        }
        Ok(EncodedLogicalObjectBinding { descriptor, bytes })
    }

    pub(crate) fn logical_descriptor(&self) -> &RawObjectDescriptor {
        &self.logical_descriptor
    }

    pub(crate) fn pages(&self) -> &[LogicalObjectPage] {
        &self.pages
    }

    fn validate(&self, limits: ProjectStoreLimits) -> Result<(), GenerationCodecError> {
        if self.logical_descriptor.byte_length() <= PAGE_BYTES
            || self.pages.is_empty()
            || self.pages.len() > REACHABLE_OBJECTS_MAX
        {
            return Err(GenerationCodecError::ArtifactClosure);
        }
        let mut offset = 0_u64;
        for (index, page) in self.pages.iter().copied().enumerate() {
            let ordinal = u32::try_from(index).map_err(|_| GenerationCodecError::Capacity {
                stage: "page ordinal",
            })?;
            if page.ordinal != ordinal || page.offset != offset {
                return Err(GenerationCodecError::ArtifactClosure);
            }
            let length = page.object.byte_length;
            let is_final = index + 1 == self.pages.len();
            if (is_final && !(1..=PAGE_BYTES).contains(&length))
                || (!is_final && length != PAGE_BYTES)
                || length > limits.object_or_page_bytes_max
            {
                return Err(GenerationCodecError::ArtifactClosure);
            }
            offset = offset
                .checked_add(length)
                .ok_or(GenerationCodecError::Capacity {
                    stage: "page offsets",
                })?;
        }
        if offset != self.logical_descriptor.byte_length() {
            return Err(GenerationCodecError::BindingMismatch);
        }
        Ok(())
    }
}

impl GenerationDocument {
    pub(crate) fn build(
        capture: &ProjectCommitCapture,
        kind: GenerationKind,
        generation_sequence: u64,
        bindings: BTreeMap<ExactBytesDigest, ArtifactStorage>,
        reachable_objects: Vec<PhysicalObject>,
        limits: ProjectStoreLimits,
    ) -> Result<Self, GenerationCodecError> {
        Self::build_from_projection(
            capture.projection().clone(),
            capture.expected_parent(),
            capture.autosave_base(),
            capture.forked_from(),
            kind,
            generation_sequence,
            bindings,
            reachable_objects,
            limits,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_from_projection(
        projection: ProjectGenerationProjection,
        parent_generation_id: Option<ProjectGenerationId>,
        base_manual_generation_id: Option<ProjectGenerationId>,
        forked_from: Option<(ProjectId, ProjectGenerationId)>,
        kind: GenerationKind,
        generation_sequence: u64,
        bindings: BTreeMap<ExactBytesDigest, ArtifactStorage>,
        reachable_objects: Vec<PhysicalObject>,
        limits: ProjectStoreLimits,
    ) -> Result<Self, GenerationCodecError> {
        let document = Self {
            kind,
            generation_sequence,
            parent_generation_id,
            base_manual_generation_id,
            forked_from,
            projection,
            bindings,
            reachable_objects,
        };
        document.validate(limits)?;
        Ok(document)
    }

    pub(crate) fn decode(
        expected_id: ProjectGenerationId,
        expected_project: ProjectId,
        encoded: &[u8],
        limits: ProjectStoreLimits,
    ) -> Result<Self, GenerationCodecError> {
        validate_byte_limit(
            encoded.len(),
            limits.generation_bytes_max,
            GENERATION_BYTES_MAX,
            "generation bytes",
        )?;
        let wire: GenerationWire =
            serde_json::from_slice(encoded).map_err(|_| GenerationCodecError::JsonShape)?;
        let canonical = encode_canonical_json(&wire)?;
        if canonical != encoded {
            return Err(GenerationCodecError::NonCanonical);
        }
        let actual_id = generation_id_from_validated_canonical(encoded)?;
        if actual_id != expected_id {
            return Err(GenerationCodecError::GenerationIdentity);
        }
        let document = wire.into_document(expected_project, limits)?;
        document.validate(limits)?;
        let round_trip = document.encode(limits)?;
        if round_trip.id != expected_id || round_trip.bytes != encoded {
            return Err(GenerationCodecError::NonCanonical);
        }
        Ok(document)
    }

    pub(crate) fn encode(
        &self,
        limits: ProjectStoreLimits,
    ) -> Result<EncodedGeneration, GenerationCodecError> {
        self.validate(limits)?;
        let wire = GenerationWire::from_document(self)?;
        let bytes = encode_canonical_json(&wire)?;
        validate_byte_limit(
            bytes.len(),
            limits.generation_bytes_max,
            GENERATION_BYTES_MAX,
            "generation bytes",
        )?;
        let id = generation_id_from_validated_canonical(&bytes)?;
        Ok(EncodedGeneration { id, bytes })
    }

    pub(crate) const fn kind(&self) -> GenerationKind {
        self.kind
    }

    pub(crate) const fn generation_sequence(&self) -> u64 {
        self.generation_sequence
    }

    pub(crate) const fn parent_generation_id(&self) -> Option<ProjectGenerationId> {
        self.parent_generation_id
    }

    pub(crate) const fn base_manual_generation_id(&self) -> Option<ProjectGenerationId> {
        self.base_manual_generation_id
    }

    pub(crate) const fn forked_from(&self) -> Option<(ProjectId, ProjectGenerationId)> {
        self.forked_from
    }

    pub(crate) fn projection(&self) -> &ProjectGenerationProjection {
        &self.projection
    }

    pub(crate) fn bindings(&self) -> &BTreeMap<ExactBytesDigest, ArtifactStorage> {
        &self.bindings
    }

    pub(crate) fn reachable_objects(&self) -> &[PhysicalObject] {
        &self.reachable_objects
    }

    fn validate(&self, limits: ProjectStoreLimits) -> Result<(), GenerationCodecError> {
        if self.kind == GenerationKind::Manual && self.base_manual_generation_id.is_some() {
            return Err(GenerationCodecError::Semantic {
                stage: "manual base",
            });
        }
        let artifacts = self.projection.state().artifacts();
        if artifacts.len() > limits.artifact_records_per_generation_max {
            return Err(GenerationCodecError::Capacity { stage: "artifacts" });
        }
        if self.reachable_objects.len() > limits.reachable_objects_per_generation_max
            || self.reachable_objects.len() > REACHABLE_OBJECTS_MAX
        {
            return Err(GenerationCodecError::Capacity {
                stage: "reachable objects",
            });
        }

        let mut expected = BTreeMap::new();
        for artifact in artifacts {
            match expected.entry(artifact.object().digest()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(artifact.object());
                }
                std::collections::btree_map::Entry::Occupied(entry)
                    if *entry.get() == artifact.object() => {}
                std::collections::btree_map::Entry::Occupied(_) => {
                    return Err(GenerationCodecError::ArtifactClosure);
                }
            }
        }
        if expected.keys().copied().collect::<Vec<_>>()
            != self.bindings.keys().copied().collect::<Vec<_>>()
        {
            return Err(GenerationCodecError::ArtifactClosure);
        }

        let reachable = self
            .reachable_objects
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let reachable_digest_count = self
            .reachable_objects
            .iter()
            .map(|object| object.digest)
            .collect::<BTreeSet<_>>()
            .len();
        if reachable.len() != self.reachable_objects.len()
            || reachable_digest_count != self.reachable_objects.len()
            || self
                .reachable_objects
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || self.reachable_objects.iter().any(|object| {
                object.byte_length > PAGE_BYTES
                    || object.byte_length > limits.object_or_page_bytes_max
            })
        {
            return Err(GenerationCodecError::ArtifactClosure);
        }
        let mut has_paged = false;
        let mut required = BTreeSet::new();
        for (digest, logical) in expected {
            let storage = self
                .bindings
                .get(&digest)
                .expect("binding key closure checked");
            match storage {
                ArtifactStorage::Direct { object }
                    if logical.byte_length() <= PAGE_BYTES
                        && *object == PhysicalObject::from_raw(logical) => {}
                ArtifactStorage::Paged { binding_manifest }
                    if logical.byte_length() > PAGE_BYTES
                        && is_binding_descriptor(binding_manifest) =>
                {
                    has_paged = true;
                }
                _ => return Err(GenerationCodecError::ArtifactClosure),
            }
            required.insert(storage.required_physical());
        }
        if !required.is_subset(&reachable) || (!has_paged && required != reachable) {
            return Err(GenerationCodecError::ArtifactClosure);
        }
        Ok(())
    }
}

fn validate_byte_limit(
    actual: usize,
    configured_max: u64,
    frozen_max: usize,
    stage: &'static str,
) -> Result<(), GenerationCodecError> {
    if actual > frozen_max || u64::try_from(actual).map_or(true, |actual| actual > configured_max) {
        Err(GenerationCodecError::Capacity { stage })
    } else {
        Ok(())
    }
}

fn is_binding_descriptor(descriptor: &RawObjectDescriptor) -> bool {
    descriptor.byte_length() <= PAGE_BYTES
        && descriptor.media_type().as_str() == BINDING_MEDIA_TYPE
        && descriptor.role().as_str() == BINDING_ROLE
}

impl RawDescriptorWire {
    fn from_descriptor(descriptor: &RawObjectDescriptor) -> Self {
        Self {
            byte_length: U64String(descriptor.byte_length()),
            digest: descriptor.digest().to_string(),
            media_type: descriptor.media_type().as_str().to_owned(),
            role: descriptor.role().as_str().to_owned(),
        }
    }

    fn into_descriptor(self) -> Result<RawObjectDescriptor, GenerationCodecError> {
        Ok(RawObjectDescriptor::new(
            ExactBytesDigest::parse(&self.digest).map_err(|_| GenerationCodecError::Identity)?,
            self.byte_length.0,
            MediaType::parse(&self.media_type).map_err(|_| GenerationCodecError::Identity)?,
            ObjectRole::parse(&self.role).map_err(|_| GenerationCodecError::Identity)?,
        ))
    }
}

impl PhysicalObjectWire {
    fn from_object(object: PhysicalObject) -> Self {
        Self {
            byte_length: U64String(object.byte_length),
            digest: object.digest.to_string(),
        }
    }

    fn into_object(self) -> Result<PhysicalObject, GenerationCodecError> {
        Ok(PhysicalObject::new(
            ExactBytesDigest::parse(&self.digest).map_err(|_| GenerationCodecError::Identity)?,
            self.byte_length.0,
        ))
    }
}

impl PageWire {
    fn from_page(page: LogicalObjectPage) -> Self {
        Self {
            byte_length: U64String(page.object.byte_length),
            digest: page.object.digest.to_string(),
            offset: U64String(page.offset),
            ordinal: page.ordinal,
        }
    }

    fn into_page(self) -> Result<LogicalObjectPage, GenerationCodecError> {
        Ok(LogicalObjectPage::new(
            self.ordinal,
            self.offset.0,
            PhysicalObject::new(
                ExactBytesDigest::parse(&self.digest)
                    .map_err(|_| GenerationCodecError::Identity)?,
                self.byte_length.0,
            ),
        ))
    }
}

impl GenerationWire {
    fn from_document(document: &GenerationDocument) -> Result<Self, GenerationCodecError> {
        let state = document.projection.state();
        let artifacts = state
            .artifacts()
            .iter()
            .map(|artifact| {
                let storage = document
                    .bindings
                    .get(&artifact.object().digest())
                    .ok_or(GenerationCodecError::ArtifactClosure)?;
                ArtifactWire::from_artifact(artifact, storage)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            artifacts: BoundedVec::new(artifacts, "artifacts")?,
            base_manual_generation_id: RequiredNullable::new(
                document.base_manual_generation_id.map(|id| id.to_string()),
            ),
            dataset: DatasetWire::from_reference(state.dataset()),
            forked_from: RequiredNullable::new(document.forked_from.map(
                |(project_id, generation_id)| ForkWire {
                    generation_id: generation_id.to_string(),
                    project_id: project_id.to_string(),
                },
            )),
            generation_kind: match document.kind {
                GenerationKind::Manual => GenerationKindWire::Manual,
                GenerationKind::Autosave => GenerationKindWire::Autosave,
            },
            generation_sequence: U64String(document.generation_sequence),
            parent_generation_id: RequiredNullable::new(
                document.parent_generation_id.map(|id| id.to_string()),
            ),
            project_id: state.project_id().to_string(),
            reachable_objects: BoundedVec::new(
                document
                    .reachable_objects
                    .iter()
                    .copied()
                    .map(PhysicalObjectWire::from_object)
                    .collect(),
                "reachable objects",
            )?,
            revision_high_water_sequence: U64String(
                document.projection.revision_high_water().sequence(),
            ),
            revision_sequence: U64String(document.projection.revision().sequence()),
            schema: GENERATION_SCHEMA.to_owned(),
            schema_version: GENERATION_SCHEMA_VERSION,
            state: StateWire::from_state(state)?,
        })
    }

    fn into_document(
        self,
        expected_project: ProjectId,
        limits: ProjectStoreLimits,
    ) -> Result<GenerationDocument, GenerationCodecError> {
        if self.schema != GENERATION_SCHEMA || self.schema_version != GENERATION_SCHEMA_VERSION {
            return Err(GenerationCodecError::Schema);
        }
        let project_id =
            ProjectId::parse(&self.project_id).map_err(|_| GenerationCodecError::Identity)?;
        if project_id != expected_project {
            return Err(GenerationCodecError::ProjectMismatch);
        }
        let parent_generation_id =
            parse_optional_generation(self.parent_generation_id.into_option())?;
        let base_manual_generation_id =
            parse_optional_generation(self.base_manual_generation_id.into_option())?;
        let forked_from = self
            .forked_from
            .into_option()
            .map(|fork| {
                Ok::<_, GenerationCodecError>((
                    ProjectId::parse(&fork.project_id)
                        .map_err(|_| GenerationCodecError::Identity)?,
                    ProjectGenerationId::parse(&fork.generation_id)
                        .map_err(|_| GenerationCodecError::Identity)?,
                ))
            })
            .transpose()?;
        let dataset = self.dataset.into_reference()?;
        let (view, channel_presets) = self.state.into_state_parts()?;

        let mut artifacts = Vec::with_capacity(self.artifacts.as_slice().len());
        let mut bindings = BTreeMap::new();
        for artifact in self.artifacts.into_vec() {
            let (reference, storage) = artifact.into_artifact()?;
            match bindings.entry(reference.object().digest()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(storage);
                }
                std::collections::btree_map::Entry::Occupied(entry) if entry.get() == &storage => {}
                std::collections::btree_map::Entry::Occupied(_) => {
                    return Err(GenerationCodecError::ArtifactClosure);
                }
            }
            artifacts.push(reference);
        }
        let total_sources = artifacts
            .iter()
            .map(|artifact| artifact.source_layers().len())
            .sum::<usize>();
        if total_sources > MAX_TOTAL_ARTIFACT_SOURCE_LAYER_REFERENCES {
            return Err(GenerationCodecError::Capacity {
                stage: "artifact source layers",
            });
        }
        let state = ProjectState::new(project_id, dataset, view, channel_presets, artifacts)
            .map_err(|_| GenerationCodecError::Semantic {
                stage: "project state",
            })?;
        let revision = ProjectRevisionId::new(project_id, self.revision_sequence.0);
        let high_water =
            ProjectRevisionHighWater::new(project_id, self.revision_high_water_sequence.0);
        let projection =
            ProjectGenerationProjection::new(revision, high_water, state).map_err(|_| {
                GenerationCodecError::Semantic {
                    stage: "project revision",
                }
            })?;
        let reachable_objects = self
            .reachable_objects
            .into_vec()
            .into_iter()
            .map(PhysicalObjectWire::into_object)
            .collect::<Result<Vec<_>, _>>()?;
        let document = GenerationDocument {
            kind: match self.generation_kind {
                GenerationKindWire::Manual => GenerationKind::Manual,
                GenerationKindWire::Autosave => GenerationKind::Autosave,
            },
            generation_sequence: self.generation_sequence.0,
            parent_generation_id,
            base_manual_generation_id,
            forked_from,
            projection,
            bindings,
            reachable_objects,
        };
        document.validate(limits)?;
        Ok(document)
    }
}

fn parse_optional_generation(
    value: Option<String>,
) -> Result<Option<ProjectGenerationId>, GenerationCodecError> {
    value
        .map(|value| ProjectGenerationId::parse(&value).map_err(|_| GenerationCodecError::Identity))
        .transpose()
}

impl DatasetWire {
    fn from_reference(reference: &DatasetReference) -> Self {
        Self {
            locator_hint: RequiredNullable::new(
                reference
                    .locator_hint()
                    .map(|hint| hint.as_str().to_owned()),
            ),
            package_id: RequiredNullable::new(reference.package_id().map(ToString::to_string)),
            release_id: RequiredNullable::new(reference.release_id().map(ToString::to_string)),
            scientific_content_id: reference.scientific_content_id().to_string(),
        }
    }

    fn into_reference(self) -> Result<DatasetReference, GenerationCodecError> {
        let scientific_content_id = ScientificContentId::parse(&self.scientific_content_id)
            .map_err(|_| GenerationCodecError::Identity)?;
        let package_id = self
            .package_id
            .into_option()
            .map(|value| PackageId::parse(&value).map_err(|_| GenerationCodecError::Identity))
            .transpose()?;
        let release_id = self
            .release_id
            .into_option()
            .map(|value| ReleaseId::parse(&value).map_err(|_| GenerationCodecError::Identity))
            .transpose()?;
        let locator_hint = self
            .locator_hint
            .into_option()
            .map(|value| {
                DatasetLocatorHint::new(value).map_err(|_| GenerationCodecError::Semantic {
                    stage: "dataset locator",
                })
            })
            .transpose()?;
        Ok(DatasetReference::new(
            scientific_content_id,
            package_id,
            release_id,
            locator_hint,
        ))
    }
}

impl StateWire {
    fn from_state(state: &ProjectState) -> Result<Self, GenerationCodecError> {
        Ok(Self {
            channel_presets: BoundedVec::new(
                state
                    .channel_presets()
                    .iter()
                    .map(ChannelPresetWire::from_preset)
                    .collect::<Result<Vec<_>, _>>()?,
                "channel presets",
            )?,
            view: ViewWire::from_view(state.view())?,
        })
    }

    fn into_state_parts(self) -> Result<(ViewState, Vec<ChannelPreset>), GenerationCodecError> {
        let view = self.view.into_view()?;
        let mut presets = Vec::with_capacity(self.channel_presets.as_slice().len());
        let mut total_entries = 0_usize;
        for preset in self.channel_presets.into_vec() {
            let preset = preset.into_preset()?;
            total_entries = total_entries.checked_add(preset.entries().len()).ok_or(
                GenerationCodecError::Capacity {
                    stage: "channel preset entries",
                },
            )?;
            if total_entries > MAX_TOTAL_CHANNEL_PRESET_ENTRIES {
                return Err(GenerationCodecError::Capacity {
                    stage: "channel preset entries",
                });
            }
            presets.push(preset);
        }
        Ok((view, presets))
    }
}

impl ArtifactWire {
    fn from_artifact(
        artifact: &ArtifactReference,
        storage: &ArtifactStorage,
    ) -> Result<Self, GenerationCodecError> {
        Ok(Self {
            completeness: ArtifactCompletenessWire::from_value(artifact.completeness()),
            content_id: artifact.content_id().to_string(),
            derivation_id: RequiredNullable::new(artifact.derivation_id().map(ToString::to_string)),
            handle_id: artifact.handle_id().to_string(),
            label: artifact.label().to_owned(),
            logical_object: RawDescriptorWire::from_descriptor(artifact.object()),
            recipe_id: RequiredNullable::new(artifact.recipe_id().map(ToString::to_string)),
            recoverability: ArtifactRecoverabilityWire::from_value(artifact.recoverability()),
            schema: ArtifactSchemaWire::from_value(artifact.schema()),
            source_layers: BoundedVec::new(
                artifact
                    .source_layers()
                    .iter()
                    .map(|layer| layer.ordinal())
                    .collect(),
                "artifact source layers",
            )?,
            storage: ArtifactStorageWire::from_storage(storage),
            visible: artifact.visible(),
        })
    }

    fn into_artifact(self) -> Result<(ArtifactReference, ArtifactStorage), GenerationCodecError> {
        ensure_strictly_increasing(self.source_layers.as_slice(), "artifact source layers")?;
        let logical_object = self.logical_object.into_descriptor()?;
        let storage = self.storage.into_storage(&logical_object)?;
        let derivation_id = self
            .derivation_id
            .into_option()
            .map(|value| {
                DerivationRecordId::parse(&value).map_err(|_| GenerationCodecError::Identity)
            })
            .transpose()?;
        let recipe_id = self
            .recipe_id
            .into_option()
            .map(|value| RecipeId::parse(&value).map_err(|_| GenerationCodecError::Identity))
            .transpose()?;
        let reference = ArtifactReference::new(
            ArtifactHandleId::parse(&self.handle_id).map_err(|_| GenerationCodecError::Identity)?,
            self.schema.into_value(),
            ArtifactContentId::parse(&self.content_id)
                .map_err(|_| GenerationCodecError::Identity)?,
            logical_object,
            derivation_id,
            recipe_id,
            self.source_layers
                .into_vec()
                .into_iter()
                .map(LogicalLayerKey::new)
                .collect(),
            self.label,
            self.visible,
            self.completeness.into_value(),
            self.recoverability.into_value(),
        )
        .map_err(|_| GenerationCodecError::Semantic { stage: "artifact" })?;
        Ok((reference, storage))
    }
}

impl ArtifactStorageWire {
    fn from_storage(storage: &ArtifactStorage) -> Self {
        match storage {
            ArtifactStorage::Direct { object } => Self::Direct {
                object: PhysicalObjectWire::from_object(*object),
            },
            ArtifactStorage::Paged { binding_manifest } => Self::Paged {
                binding_manifest: RawDescriptorWire::from_descriptor(binding_manifest),
            },
        }
    }

    fn into_storage(
        self,
        logical: &RawObjectDescriptor,
    ) -> Result<ArtifactStorage, GenerationCodecError> {
        match self {
            Self::Direct { object } => {
                let object = object.into_object()?;
                if logical.byte_length() > PAGE_BYTES || object != PhysicalObject::from_raw(logical)
                {
                    return Err(GenerationCodecError::ArtifactClosure);
                }
                Ok(ArtifactStorage::Direct { object })
            }
            Self::Paged { binding_manifest } => {
                ArtifactStorage::paged(logical, binding_manifest.into_descriptor()?)
            }
        }
    }
}

impl ViewWire {
    fn from_view(view: &ViewState) -> Result<Self, GenerationCodecError> {
        Ok(Self {
            active_layer: view.active_layer().ordinal(),
            camera: CameraWire::from_camera(*view.camera()),
            cross_section: CrossSectionWire::from_view(*view.cross_section()),
            iso_light: IsoLightWire::from_state(*view.iso_light()),
            layers: BoundedVec::new(
                view.layers().iter().map(LayerWire::from_layer).collect(),
                "view layers",
            )?,
            layout: ViewerLayoutWire::from_value(view.layout()),
            timepoint: U64String(view.timepoint().get()),
        })
    }

    fn into_view(self) -> Result<ViewState, GenerationCodecError> {
        let layers = self
            .layers
            .into_vec()
            .into_iter()
            .map(LayerWire::into_layer)
            .collect::<Result<Vec<_>, _>>()?;
        ViewState::new(
            layers,
            LogicalLayerKey::new(self.active_layer),
            TimeIndex::new(self.timepoint.0),
            self.camera.into_camera()?,
            self.layout.into_value(),
            self.cross_section.into_view()?,
            self.iso_light.into_state()?,
        )
        .map_err(|_| GenerationCodecError::Semantic { stage: "view" })
    }
}

impl LayerWire {
    fn from_parts(
        layer: LogicalLayerKey,
        visible: bool,
        transfer: &LayerTransfer,
        render: RenderState,
    ) -> Self {
        Self {
            layer: layer.ordinal(),
            render: RenderWire::from_state(render),
            transfer: TransferWire::from_transfer(transfer),
            visible,
        }
    }

    fn from_layer(layer: &LayerViewState) -> Self {
        Self::from_parts(
            layer.layer_key(),
            layer.visible(),
            layer.transfer(),
            *layer.render_state(),
        )
    }

    fn from_preset_entry(entry: &ChannelPresetEntry) -> Self {
        Self::from_parts(
            entry.layer_key(),
            entry.visible(),
            entry.transfer(),
            *entry.render_state(),
        )
    }

    fn into_parts(
        self,
    ) -> Result<(LogicalLayerKey, bool, LayerTransfer, RenderState), GenerationCodecError> {
        Ok((
            LogicalLayerKey::new(self.layer),
            self.visible,
            self.transfer.into_transfer()?,
            self.render.into_state()?,
        ))
    }

    fn into_layer(self) -> Result<LayerViewState, GenerationCodecError> {
        let (layer, visible, transfer, render) = self.into_parts()?;
        Ok(LayerViewState::new(layer, visible, transfer, render))
    }

    fn into_preset_entry(self) -> Result<ChannelPresetEntry, GenerationCodecError> {
        let (layer, visible, transfer, render) = self.into_parts()?;
        Ok(ChannelPresetEntry::new(layer, visible, transfer, render))
    }
}

impl TransferWire {
    fn from_transfer(transfer: &LayerTransfer) -> Self {
        Self {
            color_rgb: transfer.color().rgb().map(F32Bits::from_value),
            curve: TransferCurveWire::from_curve(transfer.curve()),
            invert: transfer.invert(),
            opacity: F32Bits::from_value(transfer.opacity().get()),
            window: DisplayWindowWire::from_window(transfer.window()),
        }
    }

    fn into_transfer(self) -> Result<LayerTransfer, GenerationCodecError> {
        let color_bits = self.color_rgb.map(|value| value.0);
        let color = RgbColor::new(self.color_rgb.map(F32Bits::value)).map_err(|_| {
            GenerationCodecError::Semantic {
                stage: "layer color",
            }
        })?;
        if color.rgb().map(f32::to_bits) != color_bits {
            return Err(GenerationCodecError::Semantic {
                stage: "layer color encoding",
            });
        }
        let opacity_bits = self.opacity.0;
        let opacity =
            Opacity::new(self.opacity.value()).map_err(|_| GenerationCodecError::Semantic {
                stage: "layer opacity",
            })?;
        if opacity.get().to_bits() != opacity_bits {
            return Err(GenerationCodecError::Semantic {
                stage: "layer opacity encoding",
            });
        }
        Ok(LayerTransfer::new(
            self.window.into_window()?,
            color,
            opacity,
            self.curve.into_curve()?,
            self.invert,
        ))
    }
}

impl DisplayWindowWire {
    fn from_window(window: DisplayWindow) -> Self {
        Self {
            high: F32Bits::from_value(window.high()),
            low: F32Bits::from_value(window.low()),
        }
    }

    fn into_window(self) -> Result<DisplayWindow, GenerationCodecError> {
        DisplayWindow::new(self.low.value(), self.high.value()).map_err(|_| {
            GenerationCodecError::Semantic {
                stage: "display window",
            }
        })
    }
}

impl TransferCurveWire {
    fn from_curve(curve: TransferCurve) -> Self {
        if curve.is_linear() {
            Self::Linear
        } else {
            Self::Gamma {
                value: F32Bits::from_value(curve.gamma_value()),
            }
        }
    }

    fn into_curve(self) -> Result<TransferCurve, GenerationCodecError> {
        match self {
            Self::Linear => Ok(TransferCurve::linear()),
            Self::Gamma { value } => {
                TransferCurve::gamma(value.value()).map_err(|_| GenerationCodecError::Semantic {
                    stage: "transfer gamma",
                })
            }
        }
    }
}

impl RenderWire {
    fn from_state(state: RenderState) -> Self {
        match state.mode() {
            RenderMode::Mip => Self::Mip {
                sampling: SamplingWire::from_value(state.sampling_policy()),
            },
            RenderMode::Isosurface => {
                let parameters = state
                    .iso_parameters()
                    .expect("isosurface state exposes isosurface parameters");
                Self::Isosurface {
                    display_level: F32Bits::from_value(parameters.display_level()),
                    sampling: SamplingWire::from_value(parameters.sampling_policy()),
                    shading: IsoShadingWire::from_value(parameters.shading_policy()),
                }
            }
            RenderMode::Dvr => {
                let parameters = state
                    .dvr_parameters()
                    .expect("DVR state exposes DVR parameters");
                Self::Dvr {
                    density_scale: F64Bits::from_value(parameters.density_scale()),
                    opacity_transfer: DvrOpacityWire::from_transfer(parameters.opacity_transfer()),
                    sampling: SamplingWire::from_value(parameters.sampling_policy()),
                }
            }
        }
    }

    fn into_state(self) -> Result<RenderState, GenerationCodecError> {
        match self {
            Self::Mip { sampling } => Ok(RenderState::mip(sampling.into_value())),
            Self::Isosurface {
                display_level,
                sampling,
                shading,
            } => {
                let bits = display_level.0;
                let state = RenderState::iso(
                    sampling.into_value(),
                    shading.into_value(),
                    display_level.value(),
                )
                .map_err(|_| GenerationCodecError::Semantic {
                    stage: "ISO render",
                })?;
                if state
                    .iso_parameters()
                    .expect("just constructed ISO state")
                    .display_level()
                    .to_bits()
                    != bits
                {
                    return Err(GenerationCodecError::Semantic {
                        stage: "ISO level encoding",
                    });
                }
                Ok(state)
            }
            Self::Dvr {
                density_scale,
                opacity_transfer,
                sampling,
            } => RenderState::dvr(
                sampling.into_value(),
                opacity_transfer.into_transfer()?,
                density_scale.value(),
            )
            .map_err(|_| GenerationCodecError::Semantic {
                stage: "DVR render",
            }),
        }
    }
}

impl DvrOpacityWire {
    fn from_transfer(transfer: DvrOpacityTransfer) -> Self {
        Self {
            curve: TransferCurveWire::from_curve(transfer.curve()),
            window: DisplayWindowWire::from_window(transfer.window()),
        }
    }

    fn into_transfer(self) -> Result<DvrOpacityTransfer, GenerationCodecError> {
        Ok(DvrOpacityTransfer::new(
            self.window.into_window()?,
            self.curve.into_curve()?,
        ))
    }
}

impl CameraWire {
    fn from_camera(camera: CameraView) -> Self {
        Self {
            orientation_xyzw: camera.orientation().xyzw().map(F64Bits::from_value),
            orthographic_world_per_screen_point: F64Bits::from_value(
                camera.orthographic_world_per_screen_point(),
            ),
            perspective_focal_length_screen_points: F64Bits::from_value(
                camera.perspective_focal_length_screen_points(),
            ),
            perspective_view_distance_world: F64Bits::from_value(
                camera.perspective_view_distance_world(),
            ),
            projection: ProjectionWire::from_value(camera.projection()),
            target: camera.target().components().map(F64Bits::from_value),
        }
    }

    fn into_camera(self) -> Result<CameraView, GenerationCodecError> {
        CameraView::new(
            self.projection.into_value(),
            restore_world_point(self.target, "camera target")?,
            restore_quaternion(self.orientation_xyzw, "camera orientation")?,
            self.orthographic_world_per_screen_point.value(),
            self.perspective_focal_length_screen_points.value(),
            self.perspective_view_distance_world.value(),
        )
        .map_err(|_| GenerationCodecError::Semantic { stage: "camera" })
    }
}

impl CrossSectionWire {
    fn from_view(view: CrossSectionView) -> Self {
        Self {
            center: view.center_world().components().map(F64Bits::from_value),
            depth_world: F64Bits::from_value(view.depth_world()),
            orientation_xyzw: view.orientation().xyzw().map(F64Bits::from_value),
            scale_world_per_screen_point: F64Bits::from_value(view.scale_world_per_screen_point()),
        }
    }

    fn into_view(self) -> Result<CrossSectionView, GenerationCodecError> {
        CrossSectionView::new(
            restore_world_point(self.center, "cross-section center")?,
            restore_quaternion(self.orientation_xyzw, "cross-section orientation")?,
            self.scale_world_per_screen_point.value(),
            self.depth_world.value(),
        )
        .map_err(|_| GenerationCodecError::Semantic {
            stage: "cross-section",
        })
    }
}

impl IsoLightWire {
    fn from_state(state: IsoLightState) -> Self {
        match state.detached_screen_position() {
            Some([x, y]) => Self::DetachedScreen {
                x: F32Bits::from_value(x),
                y: F32Bits::from_value(y),
            },
            None => Self::AttachedCamera,
        }
    }

    fn into_state(self) -> Result<IsoLightState, GenerationCodecError> {
        match self {
            Self::AttachedCamera => Ok(IsoLightState::attached_camera()),
            Self::DetachedScreen { x, y } => {
                let expected = [x.0, y.0];
                let state = IsoLightState::detached_screen(x.value(), y.value()).map_err(|_| {
                    GenerationCodecError::Semantic {
                        stage: "detached ISO light",
                    }
                })?;
                if state
                    .detached_screen_position()
                    .expect("just constructed detached light")
                    .map(f32::to_bits)
                    != expected
                {
                    return Err(GenerationCodecError::Semantic {
                        stage: "detached ISO light encoding",
                    });
                }
                Ok(state)
            }
        }
    }
}

impl ChannelPresetWire {
    fn from_preset(preset: &ChannelPreset) -> Result<Self, GenerationCodecError> {
        Ok(Self {
            entries: BoundedVec::new(
                preset
                    .entries()
                    .iter()
                    .map(LayerWire::from_preset_entry)
                    .collect(),
                "channel preset entries",
            )?,
            id: preset.id().as_str().to_owned(),
            label: preset.label().to_owned(),
        })
    }

    fn into_preset(self) -> Result<ChannelPreset, GenerationCodecError> {
        let keys = self
            .entries
            .as_slice()
            .iter()
            .map(|entry| entry.layer)
            .collect::<Vec<_>>();
        ensure_strictly_increasing(&keys, "channel preset entry order")?;
        let entries = self
            .entries
            .into_vec()
            .into_iter()
            .map(LayerWire::into_preset_entry)
            .collect::<Result<Vec<_>, _>>()?;
        ChannelPreset::new(
            ChannelPresetId::new(self.id)
                .map_err(|_| GenerationCodecError::Semantic { stage: "preset ID" })?,
            self.label,
            entries,
        )
        .map_err(|_| GenerationCodecError::Semantic {
            stage: "channel preset",
        })
    }
}

fn restore_world_point(
    values: [F64Bits; 3],
    stage: &'static str,
) -> Result<WorldPoint3, GenerationCodecError> {
    let bits = values.map(|value| value.0);
    let [x, y, z] = values.map(F64Bits::value);
    let point = WorldPoint3::new(x, y, z).map_err(|_| GenerationCodecError::Semantic { stage })?;
    if point.components().map(f64::to_bits) != bits {
        return Err(GenerationCodecError::Semantic { stage });
    }
    Ok(point)
}

fn restore_quaternion(
    values: [F64Bits; 4],
    stage: &'static str,
) -> Result<UnitQuaternion, GenerationCodecError> {
    UnitQuaternion::from_canonical_xyzw(values.map(F64Bits::value))
        .map_err(|_| GenerationCodecError::Semantic { stage })
}

fn ensure_strictly_increasing<T: Ord>(
    values: &[T],
    stage: &'static str,
) -> Result<(), GenerationCodecError> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        Err(GenerationCodecError::Semantic { stage })
    } else {
        Ok(())
    }
}

impl SamplingWire {
    fn from_value(value: SamplingPolicy) -> Self {
        match value {
            SamplingPolicy::SmoothLinear => Self::SmoothLinear,
            SamplingPolicy::VoxelExact => Self::VoxelExact,
        }
    }

    fn into_value(self) -> SamplingPolicy {
        match self {
            Self::SmoothLinear => SamplingPolicy::SmoothLinear,
            Self::VoxelExact => SamplingPolicy::VoxelExact,
        }
    }
}

impl IsoShadingWire {
    fn from_value(value: IsoShadingPolicy) -> Self {
        match value {
            IsoShadingPolicy::GradientLighting => Self::GradientLighting,
            IsoShadingPolicy::Flat => Self::Flat,
        }
    }

    fn into_value(self) -> IsoShadingPolicy {
        match self {
            Self::GradientLighting => IsoShadingPolicy::GradientLighting,
            Self::Flat => IsoShadingPolicy::Flat,
        }
    }
}

impl ProjectionWire {
    fn from_value(value: Projection) -> Self {
        match value {
            Projection::Perspective => Self::Perspective,
            Projection::Orthographic => Self::Orthographic,
        }
    }

    fn into_value(self) -> Projection {
        match self {
            Self::Perspective => Projection::Perspective,
            Self::Orthographic => Projection::Orthographic,
        }
    }
}

impl ViewerLayoutWire {
    fn from_value(value: ViewerLayout) -> Self {
        match value {
            ViewerLayout::Single3d => Self::Single3d,
            ViewerLayout::FourPanel => Self::FourPanel,
        }
    }

    fn into_value(self) -> ViewerLayout {
        match self {
            Self::Single3d => ViewerLayout::Single3d,
            Self::FourPanel => ViewerLayout::FourPanel,
        }
    }
}

impl ArtifactSchemaWire {
    fn from_value(value: ArtifactSchema) -> Self {
        match value {
            ArtifactSchema::RoiV1 => Self::RoiV1,
            ArtifactSchema::TrackV1 => Self::TrackV1,
            ArtifactSchema::AnnotationV1 => Self::AnnotationV1,
            ArtifactSchema::MeasurementV1 => Self::MeasurementV1,
            ArtifactSchema::AnalysisTableV1 => Self::AnalysisTableV1,
            ArtifactSchema::AnalysisPlotV1 => Self::AnalysisPlotV1,
        }
    }

    fn into_value(self) -> ArtifactSchema {
        match self {
            Self::RoiV1 => ArtifactSchema::RoiV1,
            Self::TrackV1 => ArtifactSchema::TrackV1,
            Self::AnnotationV1 => ArtifactSchema::AnnotationV1,
            Self::MeasurementV1 => ArtifactSchema::MeasurementV1,
            Self::AnalysisTableV1 => ArtifactSchema::AnalysisTableV1,
            Self::AnalysisPlotV1 => ArtifactSchema::AnalysisPlotV1,
        }
    }
}

impl ArtifactCompletenessWire {
    fn from_value(value: ArtifactCompleteness) -> Self {
        match value {
            ArtifactCompleteness::Partial => Self::Partial,
            ArtifactCompleteness::Complete => Self::Complete,
        }
    }

    fn into_value(self) -> ArtifactCompleteness {
        match self {
            Self::Partial => ArtifactCompleteness::Partial,
            Self::Complete => ArtifactCompleteness::Complete,
        }
    }
}

impl ArtifactRecoverabilityWire {
    fn from_value(value: ArtifactRecoverability) -> Self {
        match value {
            ArtifactRecoverability::Regenerable => Self::Regenerable,
            ArtifactRecoverability::NonRegenerable => Self::NonRegenerable,
        }
    }

    fn into_value(self) -> ArtifactRecoverability {
        match self {
            Self::Regenerable => ArtifactRecoverability::Regenerable,
            Self::NonRegenerable => ArtifactRecoverability::NonRegenerable,
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::{io::Cursor, path::PathBuf, process::Command};

    use super::*;

    const RECOVERABLE_PROJECT: &str = "11111111-2222-4333-8444-555555555555";
    const DIVERGENT_PROJECT: &str = "66666666-7777-4888-8999-aaaaaaaaaaaa";
    const STALE_PROJECT: &str = "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff";
    const PROVISIONAL_PROJECT: &str = "12345678-9abc-4def-8123-456789abcdef";

    #[test]
    fn every_frozen_generation_decodes_to_the_model_and_reencodes_exactly() {
        let paths = archive_paths();
        let generations = paths
            .iter()
            .filter(|path| path.contains("/generations/sha256/") && path.ends_with(".json"))
            .collect::<Vec<_>>();
        assert_eq!(
            generations.len(),
            12,
            "the frozen fixture generation inventory changed"
        );

        for path in generations {
            let bytes = extract(path);
            let project_id = project_for_path(path);
            let expected_id = generation_id_for_path(path);
            let document = GenerationDocument::decode(
                expected_id,
                project_id,
                &bytes,
                ProjectStoreLimits::default(),
            )
            .unwrap_or_else(|error| panic!("failed to decode {path}: {error:?}"));
            assert_eq!(document.projection().state().project_id(), project_id);
            let reencoded = document.encode(ProjectStoreLimits::default()).unwrap();
            assert_eq!(reencoded.id(), expected_id, "identity drift for {path}");
            assert_eq!(reencoded.bytes(), bytes, "canonical-byte drift for {path}");
        }
    }

    #[test]
    fn frozen_paged_binding_decodes_and_reencodes_exactly() {
        let generation_path = concat!(
            "recoverable.m4dproj/generations/sha256/50/",
            "fc92ea0e67a54336658f1638596642f17177ceb72c3afbc364c941e6a9b854.json"
        );
        let generation_bytes = extract(generation_path);
        let generation = GenerationDocument::decode(
            generation_id_for_path(generation_path),
            ProjectId::parse(RECOVERABLE_PROJECT).unwrap(),
            &generation_bytes,
            ProjectStoreLimits::default(),
        )
        .unwrap();
        let logical = generation
            .projection()
            .state()
            .artifacts()
            .iter()
            .find(|artifact| artifact.object().byte_length() > PAGE_BYTES)
            .unwrap()
            .object();
        let binding_descriptor = match generation.bindings().get(&logical.digest()).unwrap() {
            ArtifactStorage::Paged { binding_manifest } => binding_manifest,
            ArtifactStorage::Direct { .. } => panic!("large fixture object was stored directly"),
        };
        let digest = binding_descriptor.digest().digest().to_string();
        let binding_path = format!(
            "recoverable.m4dproj/objects/sha256/{}/{}",
            &digest[..2],
            &digest[2..]
        );
        let binding_bytes = extract(&binding_path);
        let binding = LogicalObjectBinding::decode(
            &binding_bytes,
            logical,
            binding_descriptor,
            ProjectStoreLimits::default(),
        )
        .unwrap();
        assert_eq!(binding.pages().len(), 2);
        assert_eq!(binding.pages()[0].ordinal(), 0);
        assert_eq!(binding.pages()[0].offset(), 0);
        assert_eq!(binding.pages()[0].object().byte_length(), PAGE_BYTES);
        assert_eq!(binding.logical_descriptor(), logical);
        let reencoded = binding.encode(ProjectStoreLimits::default()).unwrap();
        assert_eq!(reencoded.descriptor(), binding_descriptor);
        assert_eq!(reencoded.bytes(), binding_bytes);
    }

    #[test]
    fn generation_decoder_rejects_noncanonical_wrong_identity_and_unbounded_arrays() {
        let path = concat!(
            "stale.m4dproj/generations/sha256/d5/",
            "020fa3c69a493b34ffbbf3a67a249354e83e5a6d738479d46c7e301786d2ec.json"
        );
        let bytes = extract(path);
        let project = ProjectId::parse(STALE_PROJECT).unwrap();
        let expected = generation_id_for_path(path);

        let mut whitespace = bytes.clone();
        whitespace.push(b'\n');
        assert!(matches!(
            GenerationDocument::decode(
                expected,
                project,
                &whitespace,
                ProjectStoreLimits::default()
            ),
            Err(GenerationCodecError::NonCanonical)
        ));
        assert!(matches!(
            GenerationDocument::decode(
                ProjectGenerationId::from_digest(mirante4d_identity::Sha256Digest::from_bytes(
                    [0; 32]
                )),
                project,
                &bytes,
                ProjectStoreLimits::default()
            ),
            Err(GenerationCodecError::GenerationIdentity)
        ));
        assert!(serde_json::from_slice::<BoundedVec<u32, 2>>(b"[0,1,2]").is_err());
        assert!(serde_json::from_slice::<U64String>(b"\"01\"").is_err());
        assert!(serde_json::from_slice::<F32Bits>(b"\"7f800000\"").is_err());

        let missing_nullable = String::from_utf8(bytes)
            .unwrap()
            .replacen(",\"base_manual_generation_id\":null", "", 1)
            .into_bytes();
        let missing_id = generation_id_from_validated_canonical(&missing_nullable).unwrap();
        let missing_error = GenerationDocument::decode(
            missing_id,
            project,
            &missing_nullable,
            ProjectStoreLimits::default(),
        )
        .unwrap_err();
        assert!(
            matches!(missing_error, GenerationCodecError::JsonShape),
            "unexpected missing-field result: {missing_error:?}"
        );
    }

    #[test]
    fn commit_capture_builds_the_same_frozen_direct_generation() {
        let path = concat!(
            "stale.m4dproj/generations/sha256/d5/",
            "020fa3c69a493b34ffbbf3a67a249354e83e5a6d738479d46c7e301786d2ec.json"
        );
        let bytes = extract(path);
        let project = ProjectId::parse(STALE_PROJECT).unwrap();
        let expected = generation_id_for_path(path);
        let decoded =
            GenerationDocument::decode(expected, project, &bytes, ProjectStoreLimits::default())
                .unwrap();
        let artifact = &decoded.projection().state().artifacts()[0];
        let digest = artifact.object().digest().digest().to_string();
        let object_path = format!(
            "stale.m4dproj/objects/sha256/{}/{}",
            &digest[..2],
            &digest[2..]
        );
        let source = MemorySource {
            descriptor: artifact.object().clone(),
            bytes: extract(&object_path),
        };
        assert_eq!(
            ArtifactStorage::direct(artifact.object()).unwrap(),
            decoded
                .bindings()
                .get(&artifact.object().digest())
                .unwrap()
                .clone()
        );
        let capture = ProjectCommitCapture::new(
            decoded.projection().clone(),
            decoded.parent_generation_id(),
            decoded.base_manual_generation_id(),
            decoded.forked_from(),
            vec![Box::new(source)],
        )
        .unwrap();
        let rebuilt = GenerationDocument::build(
            &capture,
            decoded.kind(),
            decoded.generation_sequence(),
            decoded.bindings().clone(),
            decoded.reachable_objects().to_vec(),
            ProjectStoreLimits::default(),
        )
        .unwrap();
        let encoded = rebuilt.encode(ProjectStoreLimits::default()).unwrap();
        assert_eq!(encoded.id(), expected);
        assert_eq!(encoded.into_bytes(), bytes);
    }

    struct MemorySource {
        descriptor: RawObjectDescriptor,
        bytes: Vec<u8>,
    }

    impl crate::ProjectObjectSource for MemorySource {
        fn descriptor(&self) -> &RawObjectDescriptor {
            &self.descriptor
        }

        fn open(&self) -> std::io::Result<Box<dyn std::io::Read + Send>> {
            Ok(Box::new(Cursor::new(self.bytes.clone())))
        }
    }

    fn archive() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/project/project-store-v1.tar.gz")
    }

    fn archive_paths() -> Vec<String> {
        let output = Command::new("tar")
            .arg("-tzf")
            .arg(archive())
            .output()
            .expect("system tar must be available for the frozen fixture test");
        assert!(
            output.status.success(),
            "tar list failed: {:?}",
            output.status
        );
        String::from_utf8(output.stdout)
            .expect("fixture paths are UTF-8")
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn extract(path: &str) -> Vec<u8> {
        let output = Command::new("tar")
            .arg("-xOzf")
            .arg(archive())
            .arg(path)
            .output()
            .expect("system tar must be available for the frozen fixture test");
        assert!(
            output.status.success(),
            "tar extraction failed for {path}: {:?}",
            output.status
        );
        output.stdout
    }

    fn project_for_path(path: &str) -> ProjectId {
        let value = if path.starts_with("recoverable.m4dproj/") {
            RECOVERABLE_PROJECT
        } else if path.starts_with("divergent.m4dproj/") {
            DIVERGENT_PROJECT
        } else if path.starts_with("stale.m4dproj/") {
            STALE_PROJECT
        } else if path.starts_with("provisional.m4dproj/") {
            PROVISIONAL_PROJECT
        } else {
            panic!("unknown fixture store for {path}")
        };
        ProjectId::parse(value).unwrap()
    }

    fn generation_id_for_path(path: &str) -> ProjectGenerationId {
        let components = path.split('/').collect::<Vec<_>>();
        let fanout = components[components.len() - 2];
        let tail = components
            .last()
            .expect("generation path has a filename")
            .strip_suffix(".json")
            .expect("generation filename has JSON suffix");
        ProjectGenerationId::parse(&format!(
            "{}{}{}",
            ProjectGenerationId::PREFIX,
            fanout,
            tail
        ))
        .unwrap()
    }
}

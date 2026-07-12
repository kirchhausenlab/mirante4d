use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::ShardProfileKind;

pub const MAX_ZARR_METADATA_BYTES: usize = 1_048_576;

const PIXEL_DIMENSIONS: [&str; 5] = ["t", "c", "z", "y", "x"];
const VALIDITY_DIMENSIONS: [&str; 5] = ["t", "c", "z", "y", "x_byte"];

/// Strict storage-only Zarr-v3 group metadata.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ZarrGroupMetadata;

impl ZarrGroupMetadata {
    pub const fn new() -> Self {
        Self
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, ZarrMetadataError> {
        require_size(bytes, "Zarr group metadata")?;
        let wire: WireGroup =
            serde_json::from_slice(bytes).map_err(|error| ZarrMetadataError::MalformedJson {
                object: "Zarr group metadata",
                message: error.to_string(),
            })?;
        if wire.zarr_format != 3 || wire.node_type != "group" || !wire.attributes.is_empty() {
            return invalid("Zarr group metadata", "expected an empty version-3 group");
        }
        serde_json::from_slice::<zarrs_metadata::v3::GroupMetadataV3>(bytes).map_err(|error| {
            ZarrMetadataError::CoreMetadata {
                object: "Zarr group metadata",
                message: error.to_string(),
            }
        })?;
        Ok(Self)
    }

    pub fn deterministic_bytes(self) -> Result<Vec<u8>, ZarrMetadataError> {
        encode_wire(&WireGroup::default(), "Zarr group metadata")
    }
}

/// Strict storage-only Zarr-v3 array metadata for one frozen shard row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZarrArrayMetadata {
    kind: ShardProfileKind,
    shape: Vec<u64>,
}

impl ZarrArrayMetadata {
    pub fn new(kind: ShardProfileKind, shape: Vec<u64>) -> Result<Self, ZarrMetadataError> {
        validate_shape(kind, &shape)?;
        Ok(Self { kind, shape })
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, ZarrMetadataError> {
        require_size(bytes, "Zarr array metadata")?;
        let wire: WireArray =
            serde_json::from_slice(bytes).map_err(|error| ZarrMetadataError::MalformedJson {
                object: "Zarr array metadata",
                message: error.to_string(),
            })?;
        serde_json::from_slice::<zarrs_metadata::v3::ArrayMetadataV3>(bytes).map_err(|error| {
            ZarrMetadataError::CoreMetadata {
                object: "Zarr array metadata",
                message: error.to_string(),
            }
        })?;
        let kind = validate_wire(&wire)?;
        Self::new(kind, wire.shape)
    }

    pub const fn kind(&self) -> ShardProfileKind {
        self.kind
    }

    pub fn shape(&self) -> &[u64] {
        &self.shape
    }

    pub fn deterministic_bytes(&self) -> Result<Vec<u8>, ZarrMetadataError> {
        encode_wire(&WireArray::from(self), "Zarr array metadata")
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ZarrMetadataError {
    #[error("{object} is empty or exceeds {maximum} bytes")]
    Size {
        object: &'static str,
        maximum: usize,
    },
    #[error("malformed {object}: {message}")]
    MalformedJson {
        object: &'static str,
        message: String,
    },
    #[error("invalid core {object}: {message}")]
    CoreMetadata {
        object: &'static str,
        message: String,
    },
    #[error("invalid {object}: {reason}")]
    Invalid {
        object: &'static str,
        reason: &'static str,
    },
    #[error("could not encode {object}: {message}")]
    Encode {
        object: &'static str,
        message: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireGroup {
    zarr_format: u64,
    node_type: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    attributes: Map<String, Value>,
}

impl Default for WireGroup {
    fn default() -> Self {
        Self {
            zarr_format: 3,
            node_type: "group".to_owned(),
            attributes: Map::new(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireArray {
    zarr_format: u64,
    node_type: String,
    shape: Vec<u64>,
    data_type: String,
    chunk_grid: NamedConfiguration<RegularGridConfiguration>,
    chunk_key_encoding: NamedConfiguration<ChunkKeyConfiguration>,
    fill_value: Value,
    codecs: Vec<NamedConfiguration<ShardingConfiguration>>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    attributes: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    storage_transformers: Vec<Value>,
    #[serde(default, skip_serializing_if = "DimensionNamesMember::is_absent")]
    dimension_names: DimensionNamesMember,
}

#[derive(Debug, Default)]
struct DimensionNamesMember {
    present: bool,
    value: Option<Vec<String>>,
}

impl DimensionNamesMember {
    fn present(value: Vec<String>) -> Self {
        Self {
            present: true,
            value: Some(value),
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
}

impl<'de> Deserialize<'de> for DimensionNamesMember {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self {
            present: true,
            value: Option::<Vec<String>>::deserialize(deserializer)?,
        })
    }
}

impl Serialize for DimensionNamesMember {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.value.serialize(serializer)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NamedConfiguration<T> {
    name: String,
    configuration: T,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RegularGridConfiguration {
    chunk_shape: Vec<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ChunkKeyConfiguration {
    separator: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ShardingConfiguration {
    chunk_shape: Vec<u64>,
    codecs: Vec<InnerCodec>,
    index_codecs: Vec<IndexCodec>,
    index_location: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum InnerCodec {
    Bytes(NamedConfiguration<BytesConfiguration>),
    Zstd(NamedConfiguration<ZstdConfiguration>),
    NoConfiguration(NoConfigurationCodec),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum IndexCodec {
    Bytes(NamedConfiguration<BytesConfiguration>),
    NoConfiguration(NoConfigurationCodec),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BytesConfiguration {
    endian: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ZstdConfiguration {
    level: i64,
    checksum: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NoConfigurationCodec {
    name: String,
}

impl From<&ZarrArrayMetadata> for WireArray {
    fn from(metadata: &ZarrArrayMetadata) -> Self {
        let kind = metadata.kind;
        Self {
            zarr_format: 3,
            node_type: "array".to_owned(),
            shape: metadata.shape.clone(),
            data_type: data_type(kind).to_owned(),
            chunk_grid: NamedConfiguration {
                name: "regular".to_owned(),
                configuration: RegularGridConfiguration {
                    chunk_shape: outer_chunk_shape(kind).to_vec(),
                },
            },
            chunk_key_encoding: NamedConfiguration {
                name: "default".to_owned(),
                configuration: ChunkKeyConfiguration {
                    separator: "/".to_owned(),
                },
            },
            fill_value: fill_value(kind),
            codecs: vec![NamedConfiguration {
                name: "sharding_indexed".to_owned(),
                configuration: ShardingConfiguration {
                    chunk_shape: inner_chunk_shape(kind).to_vec(),
                    codecs: vec![
                        InnerCodec::Bytes(NamedConfiguration {
                            name: "bytes".to_owned(),
                            configuration: BytesConfiguration {
                                endian: "little".to_owned(),
                            },
                        }),
                        InnerCodec::Zstd(NamedConfiguration {
                            name: "zstd".to_owned(),
                            configuration: ZstdConfiguration {
                                level: 3,
                                checksum: false,
                            },
                        }),
                        InnerCodec::NoConfiguration(NoConfigurationCodec {
                            name: "crc32c".to_owned(),
                        }),
                    ],
                    index_codecs: vec![
                        IndexCodec::Bytes(NamedConfiguration {
                            name: "bytes".to_owned(),
                            configuration: BytesConfiguration {
                                endian: "little".to_owned(),
                            },
                        }),
                        IndexCodec::NoConfiguration(NoConfigurationCodec {
                            name: "crc32c".to_owned(),
                        }),
                    ],
                    index_location: "end".to_owned(),
                },
            }],
            attributes: Map::new(),
            storage_transformers: Vec::new(),
            dimension_names: match dimension_names(kind) {
                Some(value) => DimensionNamesMember::present(value),
                None => DimensionNamesMember::absent(),
            },
        }
    }
}

fn validate_wire(wire: &WireArray) -> Result<ShardProfileKind, ZarrMetadataError> {
    if wire.zarr_format != 3
        || wire.node_type != "array"
        || !wire.attributes.is_empty()
        || !wire.storage_transformers.is_empty()
        || wire.chunk_grid.name != "regular"
        || wire.chunk_key_encoding.name != "default"
        || wire.chunk_key_encoding.configuration.separator != "/"
        || wire.codecs.len() != 1
        || wire.codecs[0].name != "sharding_indexed"
    {
        return invalid(
            "Zarr array metadata",
            "unsupported node or storage configuration",
        );
    }
    let sharding = &wire.codecs[0].configuration;
    if sharding.index_location != "end"
        || !valid_inner_codecs(&sharding.codecs)
        || !valid_index_codecs(&sharding.index_codecs)
    {
        return invalid("Zarr array metadata", "unsupported codec pipeline");
    }

    for kind in ALL_KINDS {
        if wire.data_type == data_type(kind)
            && wire.chunk_grid.configuration.chunk_shape == outer_chunk_shape(kind)
            && sharding.chunk_shape == inner_chunk_shape(kind)
            && wire.dimension_names.value == dimension_names(kind)
            && (kind != ShardProfileKind::PackedIndex || !wire.dimension_names.present)
            && valid_fill(kind, &wire.fill_value)
        {
            validate_shape(kind, &wire.shape)?;
            return Ok(kind);
        }
    }
    invalid(
        "Zarr array metadata",
        "array row, fill, or dimension names are outside the profile",
    )
}

fn valid_inner_codecs(codecs: &[InnerCodec]) -> bool {
    matches!(
        codecs,
        [
            InnerCodec::Bytes(NamedConfiguration { name: bytes_name, configuration: BytesConfiguration { endian } }),
            InnerCodec::Zstd(NamedConfiguration { name: zstd_name, configuration: ZstdConfiguration { level: 3, checksum: false } }),
            InnerCodec::NoConfiguration(crc)
        ] if bytes_name == "bytes" && endian == "little" && zstd_name == "zstd" && crc.name == "crc32c"
    )
}

fn valid_index_codecs(codecs: &[IndexCodec]) -> bool {
    matches!(
        codecs,
        [
            IndexCodec::Bytes(NamedConfiguration { name: bytes_name, configuration: BytesConfiguration { endian } }),
            IndexCodec::NoConfiguration(crc)
        ] if bytes_name == "bytes" && endian == "little" && crc.name == "crc32c"
    )
}

const ALL_KINDS: [ShardProfileKind; 9] = [
    ShardProfileKind::Pixel3dUint8,
    ShardProfileKind::Pixel3dUint16,
    ShardProfileKind::Pixel3dFloat32,
    ShardProfileKind::Pixel2dUint8,
    ShardProfileKind::Pixel2dUint16,
    ShardProfileKind::Pixel2dFloat32,
    ShardProfileKind::Validity3d,
    ShardProfileKind::Validity2d,
    ShardProfileKind::PackedIndex,
];

fn data_type(kind: ShardProfileKind) -> &'static str {
    match kind {
        ShardProfileKind::Pixel3dUint8
        | ShardProfileKind::Pixel2dUint8
        | ShardProfileKind::Validity3d
        | ShardProfileKind::Validity2d
        | ShardProfileKind::PackedIndex => "uint8",
        ShardProfileKind::Pixel3dUint16 | ShardProfileKind::Pixel2dUint16 => "uint16",
        ShardProfileKind::Pixel3dFloat32 | ShardProfileKind::Pixel2dFloat32 => "float32",
    }
}

fn fill_value(kind: ShardProfileKind) -> Value {
    match kind {
        ShardProfileKind::Pixel3dFloat32 | ShardProfileKind::Pixel2dFloat32 => Value::from(0.0_f64),
        _ => Value::from(0_u64),
    }
}

fn valid_fill(kind: ShardProfileKind, value: &Value) -> bool {
    match kind {
        ShardProfileKind::Pixel3dFloat32 | ShardProfileKind::Pixel2dFloat32 => value
            .as_f64()
            .is_some_and(|value| value == 0.0 && value.is_sign_positive()),
        _ => value.as_u64() == Some(0),
    }
}

fn dimension_names(kind: ShardProfileKind) -> Option<Vec<String>> {
    match kind {
        ShardProfileKind::Validity3d | ShardProfileKind::Validity2d => {
            Some(VALIDITY_DIMENSIONS.into_iter().map(str::to_owned).collect())
        }
        ShardProfileKind::PackedIndex => None,
        _ => Some(PIXEL_DIMENSIONS.into_iter().map(str::to_owned).collect()),
    }
}

fn inner_chunk_shape(kind: ShardProfileKind) -> &'static [u64] {
    match kind {
        ShardProfileKind::Pixel3dUint8
        | ShardProfileKind::Pixel3dUint16
        | ShardProfileKind::Pixel3dFloat32 => &[1, 1, 64, 64, 64],
        ShardProfileKind::Pixel2dUint8
        | ShardProfileKind::Pixel2dUint16
        | ShardProfileKind::Pixel2dFloat32 => &[1, 1, 1, 256, 256],
        ShardProfileKind::Validity3d => &[1, 1, 64, 64, 8],
        ShardProfileKind::Validity2d => &[1, 1, 1, 256, 32],
        ShardProfileKind::PackedIndex => &[256, 64],
    }
}

fn outer_chunk_shape(kind: ShardProfileKind) -> &'static [u64] {
    match kind {
        ShardProfileKind::Pixel3dUint8
        | ShardProfileKind::Pixel3dUint16
        | ShardProfileKind::Pixel3dFloat32 => &[1, 1, 256, 256, 256],
        ShardProfileKind::Pixel2dUint8
        | ShardProfileKind::Pixel2dUint16
        | ShardProfileKind::Pixel2dFloat32 => &[1, 1, 1, 1024, 1024],
        ShardProfileKind::Validity3d => &[1, 1, 256, 256, 32],
        ShardProfileKind::Validity2d => &[1, 1, 1, 1024, 128],
        ShardProfileKind::PackedIndex => &[16_384, 64],
    }
}

fn validate_shape(kind: ShardProfileKind, shape: &[u64]) -> Result<(), ZarrMetadataError> {
    let expected_rank = if kind == ShardProfileKind::PackedIndex {
        2
    } else {
        5
    };
    if shape.len() != expected_rank || shape.contains(&0) {
        return invalid(
            "Zarr array metadata",
            "shape rank and dimensions must match the profile",
        );
    }
    if kind == ShardProfileKind::PackedIndex && shape[1] != 64 {
        return invalid(
            "Zarr array metadata",
            "packed-index shape must be record_count by 64",
        );
    }
    if matches!(
        kind,
        ShardProfileKind::Pixel2dUint8
            | ShardProfileKind::Pixel2dUint16
            | ShardProfileKind::Pixel2dFloat32
            | ShardProfileKind::Validity2d
    ) && shape[2] != 1
    {
        return invalid(
            "Zarr array metadata",
            "two-dimensional arrays must have a singleton z dimension",
        );
    }
    Ok(())
}

fn require_size(bytes: &[u8], object: &'static str) -> Result<(), ZarrMetadataError> {
    if bytes.is_empty() || bytes.len() > MAX_ZARR_METADATA_BYTES {
        return Err(ZarrMetadataError::Size {
            object,
            maximum: MAX_ZARR_METADATA_BYTES,
        });
    }
    Ok(())
}

fn encode_wire<T: Serialize>(wire: &T, object: &'static str) -> Result<Vec<u8>, ZarrMetadataError> {
    let bytes = serde_json::to_vec(wire).map_err(|error| ZarrMetadataError::Encode {
        object,
        message: error.to_string(),
    })?;
    require_size(&bytes, object)?;
    Ok(bytes)
}

fn invalid<T>(object: &'static str, reason: &'static str) -> Result<T, ZarrMetadataError> {
    Err(ZarrMetadataError::Invalid { object, reason })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Clone, Copy)]
    enum ExpectedFill {
        IntegerZero,
        FloatZero,
    }

    struct ExpectedRow {
        kind: ShardProfileKind,
        shape: &'static [u64],
        data_type: &'static str,
        outer_chunk_shape: &'static [u64],
        inner_chunk_shape: &'static [u64],
        fill: ExpectedFill,
        dimension_names: Option<&'static [&'static str]>,
    }

    const PIXEL_NAMES: &[&str] = &["t", "c", "z", "y", "x"];
    const VALIDITY_NAMES: &[&str] = &["t", "c", "z", "y", "x_byte"];
    const SHAPE_3D: &[u64] = &[2, 3, 65, 513, 1_025];
    const SHAPE_2D: &[u64] = &[2, 3, 1, 513, 1_025];
    const SHAPE_VALIDITY_3D: &[u64] = &[2, 3, 65, 513, 129];
    const SHAPE_VALIDITY_2D: &[u64] = &[2, 3, 1, 513, 129];
    const SHAPE_INDEX: &[u64] = &[1_025, 64];
    const OUTER_PIXEL_3D: &[u64] = &[1, 1, 256, 256, 256];
    const INNER_PIXEL_3D: &[u64] = &[1, 1, 64, 64, 64];
    const OUTER_PIXEL_2D: &[u64] = &[1, 1, 1, 1_024, 1_024];
    const INNER_PIXEL_2D: &[u64] = &[1, 1, 1, 256, 256];
    const OUTER_VALIDITY_3D: &[u64] = &[1, 1, 256, 256, 32];
    const INNER_VALIDITY_3D: &[u64] = &[1, 1, 64, 64, 8];
    const OUTER_VALIDITY_2D: &[u64] = &[1, 1, 1, 1_024, 128];
    const INNER_VALIDITY_2D: &[u64] = &[1, 1, 1, 256, 32];
    const OUTER_INDEX: &[u64] = &[16_384, 64];
    const INNER_INDEX: &[u64] = &[256, 64];

    const EXPECTED_ROWS: &[ExpectedRow] = &[
        ExpectedRow {
            kind: ShardProfileKind::Pixel3dUint8,
            shape: SHAPE_3D,
            data_type: "uint8",
            outer_chunk_shape: OUTER_PIXEL_3D,
            inner_chunk_shape: INNER_PIXEL_3D,
            fill: ExpectedFill::IntegerZero,
            dimension_names: Some(PIXEL_NAMES),
        },
        ExpectedRow {
            kind: ShardProfileKind::Pixel3dUint16,
            shape: SHAPE_3D,
            data_type: "uint16",
            outer_chunk_shape: OUTER_PIXEL_3D,
            inner_chunk_shape: INNER_PIXEL_3D,
            fill: ExpectedFill::IntegerZero,
            dimension_names: Some(PIXEL_NAMES),
        },
        ExpectedRow {
            kind: ShardProfileKind::Pixel3dFloat32,
            shape: SHAPE_3D,
            data_type: "float32",
            outer_chunk_shape: OUTER_PIXEL_3D,
            inner_chunk_shape: INNER_PIXEL_3D,
            fill: ExpectedFill::FloatZero,
            dimension_names: Some(PIXEL_NAMES),
        },
        ExpectedRow {
            kind: ShardProfileKind::Pixel2dUint8,
            shape: SHAPE_2D,
            data_type: "uint8",
            outer_chunk_shape: OUTER_PIXEL_2D,
            inner_chunk_shape: INNER_PIXEL_2D,
            fill: ExpectedFill::IntegerZero,
            dimension_names: Some(PIXEL_NAMES),
        },
        ExpectedRow {
            kind: ShardProfileKind::Pixel2dUint16,
            shape: SHAPE_2D,
            data_type: "uint16",
            outer_chunk_shape: OUTER_PIXEL_2D,
            inner_chunk_shape: INNER_PIXEL_2D,
            fill: ExpectedFill::IntegerZero,
            dimension_names: Some(PIXEL_NAMES),
        },
        ExpectedRow {
            kind: ShardProfileKind::Pixel2dFloat32,
            shape: SHAPE_2D,
            data_type: "float32",
            outer_chunk_shape: OUTER_PIXEL_2D,
            inner_chunk_shape: INNER_PIXEL_2D,
            fill: ExpectedFill::FloatZero,
            dimension_names: Some(PIXEL_NAMES),
        },
        ExpectedRow {
            kind: ShardProfileKind::Validity3d,
            shape: SHAPE_VALIDITY_3D,
            data_type: "uint8",
            outer_chunk_shape: OUTER_VALIDITY_3D,
            inner_chunk_shape: INNER_VALIDITY_3D,
            fill: ExpectedFill::IntegerZero,
            dimension_names: Some(VALIDITY_NAMES),
        },
        ExpectedRow {
            kind: ShardProfileKind::Validity2d,
            shape: SHAPE_VALIDITY_2D,
            data_type: "uint8",
            outer_chunk_shape: OUTER_VALIDITY_2D,
            inner_chunk_shape: INNER_VALIDITY_2D,
            fill: ExpectedFill::IntegerZero,
            dimension_names: Some(VALIDITY_NAMES),
        },
        ExpectedRow {
            kind: ShardProfileKind::PackedIndex,
            shape: SHAPE_INDEX,
            data_type: "uint8",
            outer_chunk_shape: OUTER_INDEX,
            inner_chunk_shape: INNER_INDEX,
            fill: ExpectedFill::IntegerZero,
            dimension_names: None,
        },
    ];

    fn expected_wire(row: &ExpectedRow) -> Value {
        let fill = match row.fill {
            ExpectedFill::IntegerZero => json!(0),
            ExpectedFill::FloatZero => json!(0.0),
        };
        let mut value = json!({
            "zarr_format": 3,
            "node_type": "array",
            "shape": row.shape,
            "data_type": row.data_type,
            "chunk_grid": {
                "name": "regular",
                "configuration": { "chunk_shape": row.outer_chunk_shape }
            },
            "chunk_key_encoding": {
                "name": "default",
                "configuration": { "separator": "/" }
            },
            "fill_value": fill,
            "codecs": [{
                "name": "sharding_indexed",
                "configuration": {
                    "chunk_shape": row.inner_chunk_shape,
                    "codecs": [
                        { "name": "bytes", "configuration": { "endian": "little" } },
                        { "name": "zstd", "configuration": { "level": 3, "checksum": false } },
                        { "name": "crc32c" }
                    ],
                    "index_codecs": [
                        { "name": "bytes", "configuration": { "endian": "little" } },
                        { "name": "crc32c" }
                    ],
                    "index_location": "end"
                }
            }]
        });
        if let Some(names) = row.dimension_names {
            value
                .as_object_mut()
                .unwrap()
                .insert("dimension_names".to_owned(), json!(names));
        }
        value
    }

    #[test]
    fn empty_groups_emit_deterministic_bytes_and_accept_semantic_json() {
        let group = ZarrGroupMetadata::new();
        assert_eq!(
            group.deterministic_bytes().unwrap(),
            br#"{"zarr_format":3,"node_type":"group"}"#
        );
        assert_eq!(
            ZarrGroupMetadata::parse(
                br#"{ "attributes": {}, "node_type": "group", "zarr_format": 3 }"#
            )
            .unwrap(),
            group
        );
        assert!(
            ZarrGroupMetadata::parse(
                br#"{"zarr_format":3,"node_type":"group","consolidated_metadata":null}"#
            )
            .is_err()
        );
    }

    #[test]
    fn every_frozen_array_row_round_trips_the_exact_storage_pipeline() {
        assert_eq!(EXPECTED_ROWS.len(), 9);
        for row in EXPECTED_ROWS {
            let metadata = ZarrArrayMetadata::new(row.kind, row.shape.to_vec()).unwrap();
            let bytes = metadata.deterministic_bytes().unwrap();
            assert_eq!(ZarrArrayMetadata::parse(&bytes).unwrap(), metadata);
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value, expected_wire(row), "wrong row for {:?}", row.kind);
        }
    }

    #[test]
    fn arrays_reject_unknown_alternate_and_oversized_metadata() {
        let metadata =
            ZarrArrayMetadata::new(ShardProfileKind::Pixel3dFloat32, vec![1, 1, 65, 65, 65])
                .unwrap();
        let bytes = metadata.deterministic_bytes().unwrap();
        let mut value: Value = serde_json::from_slice(&bytes).unwrap();

        value["fill_value"] = Value::from(-0.0_f64);
        assert!(ZarrArrayMetadata::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        value = serde_json::from_slice(&bytes).unwrap();
        value["codecs"][0]["configuration"]["index_location"] = Value::from("start");
        assert!(ZarrArrayMetadata::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        value = serde_json::from_slice(&bytes).unwrap();
        value["codecs"][0]["configuration"]["codecs"][2] = Value::from("crc32c");
        assert!(ZarrArrayMetadata::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        value = serde_json::from_slice(&bytes).unwrap();
        value["unexpected"] = Value::Bool(true);
        assert!(ZarrArrayMetadata::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        let packed = ZarrArrayMetadata::new(ShardProfileKind::PackedIndex, vec![1_025, 64])
            .unwrap()
            .deterministic_bytes()
            .unwrap();
        value = serde_json::from_slice(&packed).unwrap();
        value["dimension_names"] = Value::Null;
        assert!(ZarrArrayMetadata::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        assert!(ZarrArrayMetadata::new(ShardProfileKind::PackedIndex, vec![1_025, 63]).is_err());
        assert!(
            ZarrArrayMetadata::new(ShardProfileKind::Pixel2dUint8, vec![1, 1, 2, 16, 16]).is_err()
        );
        assert!(ZarrArrayMetadata::parse(&vec![b' '; MAX_ZARR_METADATA_BYTES + 1]).is_err());
    }
}

use mirante4d_domain::{GridToWorld, IntensityDType, Shape4D, ShapeError};
use serde::{Deserialize, Serialize};

use crate::{CurrentShape4DExt, LayerDisplay, WorldSpace};

pub const FORMAT_ID: &str = "mirante4d-v1";
pub const SCHEMA_VERSION: u32 = 1;
pub const LOSSLESS_CONVERSION: &str = "lossless";
pub const DENSE_INTENSITY_KIND: &str = "dense_intensity";
pub const BOOTSTRAP_CHECKSUM_ALGORITHM: &str = "blake3";
pub const BOOTSTRAP_CHECKSUM_SCOPE: &str = "zarr_chunk_payload";
pub const SHARDED_CHECKSUM_SCOPE: &str = "zarr_shard_payload";
pub const ZARR_V3_SHARDED_STORAGE_KIND: &str = "zarr_v3_sharded";
pub const DENSE_INTENSITY_ZSTD_LEVEL: i32 = 3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NativeManifest {
    pub format: String,
    pub schema_version: u32,
    pub writer: WriterMetadata,
    pub dataset: DatasetMetadata,
    pub axes: Vec<String>,
    pub world_space: WorldSpace,
    pub provenance: NativeDatasetProvenance,
    pub layers: Vec<LayerManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterMetadata {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetMetadata {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NativeDatasetProvenance {
    pub kind: NativeDatasetProvenanceKind,
    pub created_at_utc: String,
    pub app_name: String,
    pub app_version: String,
    pub native_schema_version: u32,
    pub source_format: Option<String>,
    pub source_files: Vec<SourceFileProvenance>,
    pub source_metadata: Option<SourceMetadataProvenance>,
    pub user_corrections: Vec<UserCorrectionProvenance>,
    pub storage_policy: StoragePolicyProvenance,
    pub checksum_policy: ChecksumPolicyProvenance,
    pub conversion_policy: String,
}

impl NativeDatasetProvenance {
    pub fn generated_default() -> Self {
        Self {
            kind: NativeDatasetProvenanceKind::Generated,
            created_at_utc: "not-recorded".to_owned(),
            app_name: "mirante4d".to_owned(),
            app_version: "0.0.0-dev".to_owned(),
            native_schema_version: SCHEMA_VERSION,
            source_format: None,
            source_files: Vec::new(),
            source_metadata: None,
            user_corrections: Vec::new(),
            storage_policy: StoragePolicyProvenance::default(),
            checksum_policy: ChecksumPolicyProvenance::default(),
            conversion_policy: LOSSLESS_CONVERSION.to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeDatasetProvenanceKind {
    Generated,
    Imported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFileProvenance {
    pub absolute_path: String,
    pub display_name: String,
    pub file_size_bytes: u64,
    pub modified_unix_seconds: Option<i64>,
    pub fingerprint_blake3: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceMetadataProvenance {
    pub source_axes: Vec<String>,
    pub native_axes: Vec<String>,
    pub channels_as_layers: bool,
    #[serde(with = "crate::manifest_wire::dtype")]
    pub source_dtype: IntensityDType,
    #[serde(with = "crate::manifest_wire::shape4d")]
    pub shape_tzyx: Shape4D,
    pub voxel_spacing_um: [f64; 3],
    pub voxel_spacing_status: String,
    pub voxel_spacing_source: Option<String>,
    pub channel_count: usize,
    pub timepoint_count: u64,
    pub value_range: ValueRangeProvenance,
    pub metadata_confidence: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ValueRangeProvenance {
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserCorrectionProvenance {
    pub field: String,
    pub source_value: Option<String>,
    pub reviewed_value: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoragePolicyProvenance {
    pub dense_axes: Vec<String>,
    pub channels: String,
    pub chunking: String,
    pub compression: String,
    pub multiscale: String,
}

impl Default for StoragePolicyProvenance {
    fn default() -> Self {
        Self {
            dense_axes: ["t", "z", "y", "x"].map(str::to_owned).to_vec(),
            channels: "separate_layers".to_owned(),
            chunking: "zarr_v3_sharded_axis_aligned_bricks".to_owned(),
            compression: format!("zstd_level_{DENSE_INTENSITY_ZSTD_LEVEL}"),
            multiscale: "source_s0_required_mean_downsampled_acceleration_scales".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecksumPolicyProvenance {
    pub algorithm: String,
    pub scope: String,
}

impl Default for ChecksumPolicyProvenance {
    fn default() -> Self {
        Self {
            algorithm: BOOTSTRAP_CHECKSUM_ALGORITHM.to_owned(),
            scope: SHARDED_CHECKSUM_SCOPE.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerManifest {
    pub id: String,
    pub kind: LayerKind,
    pub name: String,
    pub channel: ChannelMetadata,
    #[serde(with = "crate::manifest_wire::shape4d")]
    pub shape: Shape4D,
    pub dtype: DTypeMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_data_policy: Option<NoDataPolicy>,
    #[serde(with = "crate::manifest_wire::grid_to_world")]
    pub grid_to_world: GridToWorld,
    pub display: LayerDisplay,
    pub scales: Vec<ScaleManifest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerKind {
    DenseIntensity,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelMetadata {
    pub index: u32,
    pub color_rgba: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DTypeMetadata {
    #[serde(with = "crate::manifest_wire::dtype")]
    pub source: IntensityDType,
    #[serde(with = "crate::manifest_wire::dtype")]
    pub stored: IntensityDType,
    pub conversion: DTypeConversion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DTypeConversion {
    Lossless,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NoDataPolicy {
    pub kind: NoDataPolicyKind,
    pub source_value: f64,
    #[serde(with = "crate::manifest_wire::dtype")]
    pub source_dtype: IntensityDType,
    pub visibility_policy: NoDataVisibilityPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoDataPolicyKind {
    SentinelValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoDataVisibilityPolicy {
    InvisibleWith1VoxelInvalidDilation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScaleManifest {
    pub level: u32,
    pub array_path: String,
    #[serde(with = "crate::manifest_wire::shape4d")]
    pub shape: Shape4D,
    pub storage: ScaleStorage,
    #[serde(with = "crate::manifest_wire::grid_to_world")]
    pub grid_to_world: GridToWorld,
    pub source_scale: Option<u32>,
    pub reduction: ScaleReduction,
    pub statistics: Statistics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validity: Option<ScaleValidityMask>,
    pub bricks: BrickTable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScaleValidityMask {
    pub array_path: String,
    pub encoding: ValidityMaskEncoding,
    pub storage: ScaleStorage,
    pub valid_voxel_count: u64,
    pub invalid_voxel_count: u64,
    pub records: Vec<ValidityMaskRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScaleStorage {
    pub kind: String,
    pub array_path: String,
    #[serde(with = "crate::manifest_wire::dtype")]
    pub dtype: IntensityDType,
    pub codec_chain: Vec<String>,
    #[serde(with = "crate::manifest_wire::shape4d")]
    pub brick_shape: Shape4D,
    #[serde(with = "crate::manifest_wire::shape4d")]
    pub brick_grid_shape: Shape4D,
    #[serde(with = "crate::manifest_wire::shape4d")]
    pub subchunk_shape: Shape4D,
    #[serde(with = "crate::manifest_wire::shape4d")]
    pub chunks_per_shard: Shape4D,
    #[serde(with = "crate::manifest_wire::shape4d")]
    pub shard_shape: Shape4D,
    #[serde(with = "crate::manifest_wire::shape4d")]
    pub shard_grid_shape: Shape4D,
    pub checksum_scope: String,
    pub shard_records: Vec<ShardRecord>,
}

impl ScaleStorage {
    pub fn sharded(
        array_path: String,
        dtype: IntensityDType,
        shape: Shape4D,
        brick_shape: Shape4D,
        shard_shape: Shape4D,
        shard_records: Vec<ShardRecord>,
    ) -> Result<Self, ShapeError> {
        let brick_grid_shape = shape.chunk_grid(brick_shape)?;
        let shard_grid_shape = shape.chunk_grid(shard_shape)?;
        Ok(Self {
            kind: ZARR_V3_SHARDED_STORAGE_KIND.to_owned(),
            array_path,
            dtype,
            codec_chain: ["sharding", "bytes", "zstd"].map(str::to_owned).to_vec(),
            brick_shape,
            brick_grid_shape,
            subchunk_shape: brick_shape,
            chunks_per_shard: Shape4D::new(
                shard_shape.t() / brick_shape.t(),
                shard_shape.z() / brick_shape.z(),
                shard_shape.y() / brick_shape.y(),
                shard_shape.x() / brick_shape.x(),
            )?,
            shard_shape,
            shard_grid_shape,
            checksum_scope: SHARDED_CHECKSUM_SCOPE.to_owned(),
            shard_records,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShardRecord {
    pub index: BrickIndex,
    pub payload_bytes: u64,
    pub payload_checksum: PayloadChecksum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidityMaskEncoding {
    Uint8RenderValidMask,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidityMaskRecord {
    pub index: BrickIndex,
    pub valid_voxel_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_checksum: Option<PayloadChecksum>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleReduction {
    Source,
    Mean,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Statistics {
    pub min: f64,
    pub max: f64,
    pub histogram: Histogram,
    pub percentiles: Percentiles,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Histogram {
    pub bin_count: u32,
    pub range_min: f64,
    pub range_max: f64,
    pub bins: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Percentiles {
    pub p0_1: f64,
    pub p1: f64,
    pub p50: f64,
    pub p99: f64,
    pub p99_9: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrickTable {
    #[serde(with = "crate::manifest_wire::shape4d")]
    pub grid_shape: Shape4D,
    pub records: Vec<BrickRecord>,
    pub range_hierarchy: BrickRangeHierarchy,
}

impl BrickTable {
    pub fn new(grid_shape: Shape4D, records: Vec<BrickRecord>) -> Self {
        let range_hierarchy = BrickRangeHierarchy::from_brick_records(grid_shape, &records);
        Self {
            grid_shape,
            records,
            range_hierarchy,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrickRecord {
    pub index: BrickIndex,
    pub occupied: bool,
    pub valid_voxel_count: u64,
    pub min: f64,
    pub max: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_checksum: Option<PayloadChecksum>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrickRangeHierarchy {
    pub levels: Vec<BrickRangeLevel>,
}

impl BrickRangeHierarchy {
    pub fn from_brick_records(grid_shape: Shape4D, records: &[BrickRecord]) -> Self {
        let mut current_shape = grid_shape;
        let mut current_records = records
            .iter()
            .map(|record| BrickRangeRecord {
                index: record.index,
                has_valid_voxels: record.occupied,
                valid_voxel_count: record.valid_voxel_count,
                min: if record.occupied {
                    record.min
                } else {
                    f64::INFINITY
                },
                max: if record.occupied {
                    record.max
                } else {
                    f64::NEG_INFINITY
                },
            })
            .collect::<Vec<_>>();
        current_records.sort_by_key(|record| {
            (
                record.index.t,
                record.index.z,
                record.index.y,
                record.index.x,
            )
        });

        let mut levels = Vec::new();
        levels.push(BrickRangeLevel {
            level: 0,
            grid_shape: current_shape,
            records: current_records.clone(),
        });

        let mut level = 1;
        while current_shape.z() > 1 || current_shape.y() > 1 || current_shape.x() > 1 {
            let next_shape = Shape4D::new(
                current_shape.t(),
                current_shape.z().div_ceil(2),
                current_shape.y().div_ceil(2),
                current_shape.x().div_ceil(2),
            )
            .expect("range hierarchy dimensions remain positive and bounded");
            let mut next_records = Vec::new();
            for t in 0..next_shape.t() {
                for z in 0..next_shape.z() {
                    for y in 0..next_shape.y() {
                        for x in 0..next_shape.x() {
                            next_records.push(BrickRangeRecord {
                                index: BrickIndex { t, z, y, x },
                                has_valid_voxels: false,
                                valid_voxel_count: 0,
                                min: f64::INFINITY,
                                max: f64::NEG_INFINITY,
                            });
                        }
                    }
                }
            }
            for record in &current_records {
                let parent = BrickIndex {
                    t: record.index.t,
                    z: record.index.z / 2,
                    y: record.index.y / 2,
                    x: record.index.x / 2,
                };
                let parent_index = brick_range_record_offset(next_shape, parent);
                let parent_record = &mut next_records[parent_index];
                parent_record.has_valid_voxels |= record.has_valid_voxels;
                parent_record.valid_voxel_count += record.valid_voxel_count;
                if record.has_valid_voxels {
                    parent_record.min = parent_record.min.min(record.min);
                    parent_record.max = parent_record.max.max(record.max);
                }
            }

            for record in &mut next_records {
                if !record.has_valid_voxels {
                    record.min = 0.0;
                    record.max = 0.0;
                }
            }

            levels.push(BrickRangeLevel {
                level,
                grid_shape: next_shape,
                records: next_records.clone(),
            });
            current_shape = next_shape;
            current_records = next_records;
            level += 1;
        }

        for level in &mut levels {
            for record in &mut level.records {
                if !record.has_valid_voxels {
                    record.min = 0.0;
                    record.max = 0.0;
                }
            }
        }

        Self { levels }
    }
}

fn brick_range_record_offset(shape: Shape4D, index: BrickIndex) -> usize {
    (((index.t * shape.z() + index.z) * shape.y() + index.y) * shape.x() + index.x) as usize
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrickRangeLevel {
    pub level: u32,
    #[serde(with = "crate::manifest_wire::shape4d")]
    pub grid_shape: Shape4D,
    pub records: Vec<BrickRangeRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrickRangeRecord {
    pub index: BrickIndex,
    pub has_valid_voxels: bool,
    pub valid_voxel_count: u64,
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BrickIndex {
    pub t: u64,
    pub z: u64,
    pub y: u64,
    pub x: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadChecksum {
    pub algorithm: String,
    pub scope: String,
    pub hex: String,
}

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use mirante4d_domain::{GridToWorld, IntensityDType, RgbColor, Shape4D, ShapeError};
use thiserror::Error;

use crate::{
    AXES_TZYX, CurrentFormatIdError, CurrentGridToWorldExt, CurrentShape4DExt,
    CurrentTransformError, DatasetId, LayerId, validate_axes_tzyx,
};

use crate::manifest::{
    BOOTSTRAP_CHECKSUM_ALGORITHM, BOOTSTRAP_CHECKSUM_SCOPE, BrickIndex, BrickRangeHierarchy,
    DENSE_INTENSITY_ZSTD_LEVEL, DTypeConversion, FORMAT_ID, LayerKind, NativeDatasetProvenanceKind,
    NativeManifest, NoDataPolicyKind, NoDataVisibilityPolicy, SCHEMA_VERSION,
    SHARDED_CHECKSUM_SCOPE, ScaleManifest, ScaleReduction, ScaleStorage, ValidityMaskEncoding,
    ZARR_V3_SHARDED_STORAGE_KIND,
};
use crate::multiscale::{
    GRID_TO_WORLD_EPSILON, expected_downsampled_grid_to_world, grid_to_world_approx_eq,
    infer_downsample_factors,
};
use crate::zarr_io::{
    array_chunk_shape, array_dimension_names, array_shape, array_subchunk_shape, open_array,
};

#[derive(Debug, Error)]
pub enum FormatError {
    #[error("native dataset package is missing mirante4d.json at {0}")]
    MissingManifest(PathBuf),
    #[error("failed to read {path}: {source}")]
    ReadManifest {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    ParseManifest {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("failed to write {path}: {source}")]
    WriteManifest {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("native dataset package already exists at {0}")]
    PackageExists(PathBuf),
    #[error("unsupported format {0:?}; expected \"mirante4d-v1\"")]
    UnsupportedFormat(String),
    #[error("unsupported schema version {0}; expected 1")]
    UnsupportedSchemaVersion(u32),
    #[error("invalid axis order: {0}")]
    InvalidAxisOrder(String),
    #[error("dataset must contain at least one layer")]
    EmptyLayers,
    #[error("invalid native provenance: {0}")]
    InvalidProvenance(String),
    #[error("invalid id: {0}")]
    InvalidId(#[from] CurrentFormatIdError),
    #[error("duplicate layer id {0:?}")]
    DuplicateLayerId(String),
    #[error("invalid shape: {0}")]
    InvalidShape(#[from] ShapeError),
    #[error("invalid layer kind for layer {layer_id}: {kind:?}")]
    InvalidLayerKind { layer_id: String, kind: LayerKind },
    #[error("layer {layer_id} uses unsupported dtype conversion {conversion:?}")]
    InvalidDTypeConversion {
        layer_id: String,
        conversion: DTypeConversion,
    },
    #[error("invalid no-data policy for layer {layer_id}: {message}")]
    InvalidNoDataPolicy { layer_id: String, message: String },
    #[error("layer {layer_id} has invalid display window")]
    InvalidDisplayWindow { layer_id: String },
    #[error("layer {layer_id} has invalid color {color:?}")]
    InvalidColor { layer_id: String, color: [f32; 4] },
    #[error("layer {layer_id} has invalid opacity {opacity}")]
    InvalidOpacity { layer_id: String, opacity: f32 },
    #[error("invalid transform for layer {layer_id}: {source}")]
    InvalidTransform {
        layer_id: String,
        source: CurrentTransformError,
    },
    #[error("layer {layer_id} must contain at least source scale s0")]
    InvalidScaleCount { layer_id: String },
    #[error("layer {layer_id} has invalid scale level sequence at {level}")]
    InvalidScaleLevel { layer_id: String, level: u32 },
    #[error("scale shape or metadata for layer {layer_id} is invalid")]
    ScaleShapeMismatch { layer_id: String },
    #[error(
        "scale transform for layer {layer_id}, scale {level} is not center-aligned with its source scale"
    )]
    ScaleTransformMismatch { layer_id: String, level: u32 },
    #[error("brick grid shape mismatch for layer {layer_id}")]
    BrickGridShapeMismatch { layer_id: String },
    #[error("brick records for layer {layer_id} do not match the declared grid")]
    BrickRecordGridMismatch { layer_id: String },
    #[error("brick range hierarchy for layer {layer_id} is missing or inconsistent")]
    BrickRangeHierarchyMismatch { layer_id: String },
    #[error("layer {layer_id} scale s{level} is missing required no-data validity metadata")]
    ValidityMaskMissing { layer_id: String, level: u32 },
    #[error("layer {layer_id} scale s{level} has validity metadata without a no-data policy")]
    ValidityMaskUnexpected { layer_id: String, level: u32 },
    #[error("validity metadata mismatch for layer {layer_id}, scale s{level}: {message}")]
    ValidityMaskMismatch {
        layer_id: String,
        level: u32,
        message: String,
    },
    #[error("invalid checksum metadata for layer {layer_id}, brick {index:?}")]
    InvalidChecksum { layer_id: String, index: BrickIndex },
    #[error(
        "payload byte count mismatch for layer {layer_id}, brick {index:?}: recorded {recorded}, actual {actual}"
    )]
    PayloadByteCountMismatch {
        layer_id: String,
        index: BrickIndex,
        recorded: u64,
        actual: u64,
    },
    #[error("payload missing for layer {layer_id}, brick {index:?}")]
    PayloadMissing { layer_id: String, index: BrickIndex },
    #[error("payload checksum mismatch for layer {layer_id}, brick {index:?}")]
    PayloadChecksumMismatch { layer_id: String, index: BrickIndex },
    #[error("array path {array_path:?} for layer {layer_id} is invalid")]
    InvalidArrayPath {
        layer_id: String,
        array_path: String,
    },
    #[error("Zarr array metadata mismatch for layer {layer_id}: {message}")]
    ZarrMetadataMismatch { layer_id: String, message: String },
    #[error("unsupported Zarr codec for layer {layer_id}, array {array_path}: {message}")]
    UnsupportedZarrCodec {
        layer_id: String,
        array_path: String,
        message: String,
    },
    #[error("Zarr storage error for layer {layer_id}: {message}")]
    ZarrStorage { layer_id: String, message: String },
    #[error("layer {layer_id} has {actual} values, expected {expected}")]
    InvalidLayerValues {
        layer_id: String,
        actual: usize,
        expected: usize,
    },
    #[error("layer {layer_id} has non-finite float32 value {value} at flat index {index}")]
    InvalidFloatValue {
        layer_id: String,
        index: usize,
        value: f32,
    },
    #[error("timepoint {timepoint} is out of bounds for layer {layer_id}, scale {level}")]
    InvalidTimepoint {
        layer_id: String,
        level: u32,
        timepoint: u64,
    },
    #[error("timepoint {timepoint} for layer {layer_id}, scale {level} was written more than once")]
    DuplicateTimepointWrite {
        layer_id: String,
        level: u32,
        timepoint: u64,
    },
    #[error(
        "layer {layer_id}, scale {level} is incomplete; written {written} of {expected} timepoints"
    )]
    IncompleteScaleWrites {
        layer_id: String,
        level: u32,
        written: usize,
        expected: usize,
    },
}

#[derive(Debug, Clone)]
pub struct ValidatedDataset {
    pub root: PathBuf,
    pub manifest: NativeManifest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatasetValidationMode {
    Quick,
    Full,
}

pub fn load_and_validate_dataset(root: impl AsRef<Path>) -> Result<ValidatedDataset, FormatError> {
    load_and_validate_dataset_with_mode(root, DatasetValidationMode::Full)
}

pub fn load_and_validate_dataset_quick(
    root: impl AsRef<Path>,
) -> Result<ValidatedDataset, FormatError> {
    load_and_validate_dataset_with_mode(root, DatasetValidationMode::Quick)
}

pub fn load_and_validate_dataset_with_mode(
    root: impl AsRef<Path>,
    mode: DatasetValidationMode,
) -> Result<ValidatedDataset, FormatError> {
    let root = root.as_ref();
    let manifest = load_manifest(root)?;
    validate_manifest_with_mode(root, &manifest, mode)?;
    Ok(ValidatedDataset {
        root: root.to_path_buf(),
        manifest,
    })
}

pub fn load_manifest(root: impl AsRef<Path>) -> Result<NativeManifest, FormatError> {
    let path = root.as_ref().join("mirante4d.json");
    if !path.exists() {
        return Err(FormatError::MissingManifest(path));
    }
    let text = fs::read_to_string(&path).map_err(|source| FormatError::ReadManifest {
        path: path.clone(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| FormatError::ParseManifest { path, source })
}

pub fn write_manifest(
    root: impl AsRef<Path>,
    manifest: &NativeManifest,
) -> Result<(), FormatError> {
    let path = root.as_ref().join("mirante4d.json");
    let json = serde_json::to_string_pretty(manifest).expect("manifest structs serialize");
    fs::write(&path, format!("{json}\n")).map_err(|source| FormatError::WriteManifest {
        path: path.clone(),
        source,
    })
}

pub fn validate_manifest(
    root: impl AsRef<Path>,
    manifest: &NativeManifest,
) -> Result<(), FormatError> {
    validate_manifest_with_mode(root, manifest, DatasetValidationMode::Full)
}

pub fn validate_manifest_quick(
    root: impl AsRef<Path>,
    manifest: &NativeManifest,
) -> Result<(), FormatError> {
    validate_manifest_with_mode(root, manifest, DatasetValidationMode::Quick)
}

pub fn validate_manifest_with_mode(
    root: impl AsRef<Path>,
    manifest: &NativeManifest,
    mode: DatasetValidationMode,
) -> Result<(), FormatError> {
    if manifest.format != FORMAT_ID {
        return Err(FormatError::UnsupportedFormat(manifest.format.clone()));
    }
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(FormatError::UnsupportedSchemaVersion(
            manifest.schema_version,
        ));
    }
    validate_axes_tzyx(&manifest.axes)
        .map_err(|err| FormatError::InvalidAxisOrder(err.to_string()))?;
    DatasetId::new(manifest.dataset.id.clone())?;
    if manifest.layers.is_empty() {
        return Err(FormatError::EmptyLayers);
    }
    validate_provenance(manifest)?;

    let mut seen_layers = HashSet::new();
    for layer in &manifest.layers {
        LayerId::new(layer.id.clone())?;
        if !seen_layers.insert(layer.id.clone()) {
            return Err(FormatError::DuplicateLayerId(layer.id.clone()));
        }
        validate_layer(root.as_ref(), layer, mode)?;
    }

    Ok(())
}

fn validate_provenance(manifest: &NativeManifest) -> Result<(), FormatError> {
    let provenance = &manifest.provenance;
    if provenance.native_schema_version != manifest.schema_version {
        return Err(FormatError::InvalidProvenance(format!(
            "native_schema_version {} does not match manifest schema_version {}",
            provenance.native_schema_version, manifest.schema_version
        )));
    }
    if provenance.app_name.trim().is_empty() {
        return Err(FormatError::InvalidProvenance(
            "app_name must not be empty".to_owned(),
        ));
    }
    if provenance.app_version.trim().is_empty() {
        return Err(FormatError::InvalidProvenance(
            "app_version must not be empty".to_owned(),
        ));
    }
    if provenance.created_at_utc.trim().is_empty() {
        return Err(FormatError::InvalidProvenance(
            "created_at_utc must not be empty".to_owned(),
        ));
    }
    if provenance.storage_policy.dense_axes != AXES_TZYX.map(str::to_owned) {
        return Err(FormatError::InvalidProvenance(
            "storage policy dense axes must be exactly t,z,y,x".to_owned(),
        ));
    }
    if provenance.storage_policy.channels != "separate_layers" {
        return Err(FormatError::InvalidProvenance(
            "storage policy must record channels as separate_layers".to_owned(),
        ));
    }
    if provenance.checksum_policy.algorithm != BOOTSTRAP_CHECKSUM_ALGORITHM
        || provenance.checksum_policy.scope != SHARDED_CHECKSUM_SCOPE
    {
        return Err(FormatError::InvalidProvenance(
            "checksum policy does not match sharded dense payload checksum contract".to_owned(),
        ));
    }
    if provenance.conversion_policy.trim().is_empty() {
        return Err(FormatError::InvalidProvenance(
            "conversion_policy must not be empty".to_owned(),
        ));
    }
    match provenance.kind {
        NativeDatasetProvenanceKind::Generated => {}
        NativeDatasetProvenanceKind::Imported => {
            if provenance
                .source_format
                .as_deref()
                .is_none_or(str::is_empty)
            {
                return Err(FormatError::InvalidProvenance(
                    "imported datasets must record source_format".to_owned(),
                ));
            }
            if provenance.source_files.is_empty() {
                return Err(FormatError::InvalidProvenance(
                    "imported datasets must record at least one source file".to_owned(),
                ));
            }
            if provenance.source_metadata.is_none() {
                return Err(FormatError::InvalidProvenance(
                    "imported datasets must record source metadata".to_owned(),
                ));
            }
        }
    }
    for source_file in &provenance.source_files {
        if source_file.absolute_path.trim().is_empty() {
            return Err(FormatError::InvalidProvenance(
                "source file absolute_path must not be empty".to_owned(),
            ));
        }
        if source_file.display_name.trim().is_empty() {
            return Err(FormatError::InvalidProvenance(
                "source file display_name must not be empty".to_owned(),
            ));
        }
        if let Some(fingerprint) = &source_file.fingerprint_blake3
            && (fingerprint.len() != 64
                || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(FormatError::InvalidProvenance(
                "source file fingerprint_blake3 must be a 64-character hex digest".to_owned(),
            ));
        }
    }
    if let Some(source_metadata) = &provenance.source_metadata {
        if source_metadata.native_axes != AXES_TZYX.map(str::to_owned) {
            return Err(FormatError::InvalidProvenance(
                "source metadata native_axes must be exactly t,z,y,x".to_owned(),
            ));
        }
        if !source_metadata.channels_as_layers {
            return Err(FormatError::InvalidProvenance(
                "source metadata must record channels_as_layers=true".to_owned(),
            ));
        }
        if source_metadata.shape_tzyx.t() == 0
            || source_metadata.shape_tzyx.z() == 0
            || source_metadata.shape_tzyx.y() == 0
            || source_metadata.shape_tzyx.x() == 0
        {
            return Err(FormatError::InvalidProvenance(
                "source metadata shape_tzyx must be nonzero".to_owned(),
            ));
        }
        if source_metadata
            .voxel_spacing_um
            .iter()
            .any(|spacing| !spacing.is_finite() || *spacing <= 0.0)
        {
            return Err(FormatError::InvalidProvenance(
                "source metadata voxel spacing must be positive finite micrometers".to_owned(),
            ));
        }
        if !source_metadata.value_range.min.is_finite()
            || !source_metadata.value_range.max.is_finite()
            || source_metadata.value_range.max < source_metadata.value_range.min
        {
            return Err(FormatError::InvalidProvenance(
                "source metadata value range is invalid".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_layer(
    root: &Path,
    layer: &crate::manifest::LayerManifest,
    mode: DatasetValidationMode,
) -> Result<(), FormatError> {
    let layer_id = layer.id.clone();
    if layer.kind != LayerKind::DenseIntensity {
        return Err(FormatError::InvalidLayerKind {
            layer_id,
            kind: layer.kind,
        });
    }
    if layer.dtype.conversion != DTypeConversion::Lossless {
        return Err(FormatError::InvalidDTypeConversion {
            layer_id: layer.id.clone(),
            conversion: layer.dtype.conversion,
        });
    }
    validate_no_data_policy(layer)?;

    let color = layer.channel.color_rgba;
    if RgbColor::new([color[0], color[1], color[2]]).is_err()
        || !color[3].is_finite()
        || !(0.0..=1.0).contains(&color[3])
    {
        return Err(FormatError::InvalidColor {
            layer_id: layer.id.clone(),
            color,
        });
    }
    layer
        .grid_to_world
        .inverse()
        .map_err(|source| FormatError::InvalidTransform {
            layer_id: layer.id.clone(),
            source,
        })?;

    if layer.scales.is_empty() {
        return Err(FormatError::InvalidScaleCount {
            layer_id: layer.id.clone(),
        });
    }
    for (expected_level, scale) in layer.scales.iter().enumerate() {
        validate_scale(root, layer, scale, expected_level as u32, mode)?;
    }
    Ok(())
}

fn validate_scale(
    root: &Path,
    layer: &crate::manifest::LayerManifest,
    scale: &ScaleManifest,
    expected_level: u32,
    mode: DatasetValidationMode,
) -> Result<(), FormatError> {
    if scale.level != expected_level {
        return Err(FormatError::InvalidScaleLevel {
            layer_id: layer.id.clone(),
            level: scale.level,
        });
    }
    if scale.level == 0 {
        if scale.shape != layer.shape
            || scale.grid_to_world != layer.grid_to_world
            || scale.source_scale.is_some()
            || scale.reduction != ScaleReduction::Source
        {
            return Err(FormatError::ScaleShapeMismatch {
                layer_id: layer.id.clone(),
            });
        }
    } else if scale.shape.t() != layer.shape.t()
        || scale.source_scale != Some(scale.level - 1)
        || scale.reduction == ScaleReduction::Source
    {
        return Err(FormatError::ScaleShapeMismatch {
            layer_id: layer.id.clone(),
        });
    }
    if scale.level > 0 {
        let previous = &layer.scales[(expected_level - 1) as usize];
        validate_scale_registration(
            layer.id.as_str(),
            scale.level,
            previous.shape,
            previous.grid_to_world,
            scale.shape,
            scale.grid_to_world,
        )?;
    }
    scale
        .grid_to_world
        .inverse()
        .map_err(|source| FormatError::InvalidTransform {
            layer_id: layer.id.clone(),
            source,
        })?;
    let brick_shape = scale.storage.brick_shape;
    validate_scale_storage(layer.id.as_str(), layer.dtype.stored, scale)?;
    let expected_grid = scale.shape.chunk_grid(brick_shape)?;
    if scale.bricks.grid_shape != expected_grid {
        return Err(FormatError::BrickGridShapeMismatch {
            layer_id: layer.id.clone(),
        });
    }
    validate_bricks(layer.id.as_str(), scale, expected_grid)?;
    validate_scale_validity_metadata(root, layer, scale, expected_grid, mode)?;
    validate_array_metadata(root, layer.id.as_str(), scale, mode)?;
    Ok(())
}

fn validate_scale_storage(
    layer_id: &str,
    stored_dtype: IntensityDType,
    scale: &ScaleManifest,
) -> Result<(), FormatError> {
    validate_storage_metadata(
        layer_id,
        stored_dtype,
        scale.array_path.as_str(),
        scale.shape,
        &scale.storage,
    )
}

fn validate_storage_metadata(
    layer_id: &str,
    expected_dtype: IntensityDType,
    expected_array_path: &str,
    shape: Shape4D,
    storage: &ScaleStorage,
) -> Result<(), FormatError> {
    if storage.kind != ZARR_V3_SHARDED_STORAGE_KIND {
        return Err(FormatError::ZarrMetadataMismatch {
            layer_id: layer_id.to_owned(),
            message: format!("storage kind {:?} is not zarr_v3_sharded", storage.kind),
        });
    }
    if storage.array_path != expected_array_path {
        return Err(FormatError::ZarrMetadataMismatch {
            layer_id: layer_id.to_owned(),
            message: "storage array_path does not match scale array_path".to_owned(),
        });
    }
    if storage.dtype != expected_dtype {
        return Err(FormatError::ZarrMetadataMismatch {
            layer_id: layer_id.to_owned(),
            message: format!(
                "storage dtype {:?} does not match layer dtype {:?}",
                storage.dtype, expected_dtype
            ),
        });
    }
    if storage.codec_chain != ["sharding", "bytes", "zstd"].map(str::to_owned) {
        return Err(FormatError::UnsupportedZarrCodec {
            layer_id: layer_id.to_owned(),
            array_path: expected_array_path.to_owned(),
            message: "expected sharding, bytes, zstd codec chain".to_owned(),
        });
    }
    let brick_shape = storage.brick_shape;
    if storage.subchunk_shape != brick_shape {
        return Err(FormatError::ZarrMetadataMismatch {
            layer_id: layer_id.to_owned(),
            message: "storage brick/subchunk shape must match logical brick shape".to_owned(),
        });
    }
    let expected_brick_grid = shape.chunk_grid(brick_shape)?;
    if storage.brick_grid_shape != expected_brick_grid {
        return Err(FormatError::BrickGridShapeMismatch {
            layer_id: layer_id.to_owned(),
        });
    }
    if storage.chunks_per_shard.t() != 1 {
        return Err(FormatError::ZarrMetadataMismatch {
            layer_id: layer_id.to_owned(),
            message: "shards must not span multiple timepoints".to_owned(),
        });
    }
    let expected_shard_shape = Shape4D::new(
        storage.brick_shape.t() * storage.chunks_per_shard.t(),
        storage.brick_shape.z() * storage.chunks_per_shard.z(),
        storage.brick_shape.y() * storage.chunks_per_shard.y(),
        storage.brick_shape.x() * storage.chunks_per_shard.x(),
    )?;
    if storage.shard_shape != expected_shard_shape {
        return Err(FormatError::ZarrMetadataMismatch {
            layer_id: layer_id.to_owned(),
            message: "storage shard_shape does not equal brick_shape * chunks_per_shard".to_owned(),
        });
    }
    let expected_shard_grid = shape.chunk_grid(storage.shard_shape)?;
    if storage.shard_grid_shape != expected_shard_grid {
        return Err(FormatError::ZarrMetadataMismatch {
            layer_id: layer_id.to_owned(),
            message: "storage shard_grid_shape does not match shape/shard_shape".to_owned(),
        });
    }
    if storage.checksum_scope != SHARDED_CHECKSUM_SCOPE {
        return Err(FormatError::ZarrMetadataMismatch {
            layer_id: layer_id.to_owned(),
            message: "storage checksum_scope must be zarr_shard_payload".to_owned(),
        });
    }
    validate_shard_records(layer_id, storage)
}

fn validate_shard_records(layer_id: &str, storage: &ScaleStorage) -> Result<(), FormatError> {
    let mut seen = HashSet::new();
    for record in &storage.shard_records {
        if record.index.t >= storage.shard_grid_shape.t()
            || record.index.z >= storage.shard_grid_shape.z()
            || record.index.y >= storage.shard_grid_shape.y()
            || record.index.x >= storage.shard_grid_shape.x()
            || !seen.insert(record.index)
        {
            return Err(FormatError::BrickRecordGridMismatch {
                layer_id: layer_id.to_owned(),
            });
        }
        if record.payload_checksum.algorithm != BOOTSTRAP_CHECKSUM_ALGORITHM
            || record.payload_checksum.scope != SHARDED_CHECKSUM_SCOPE
            || record.payload_checksum.hex.len() != 64
            || !record
                .payload_checksum
                .hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(FormatError::InvalidChecksum {
                layer_id: layer_id.to_owned(),
                index: record.index,
            });
        }
        if record.payload_bytes == 0 {
            return Err(FormatError::PayloadByteCountMismatch {
                layer_id: layer_id.to_owned(),
                index: record.index,
                recorded: record.payload_bytes,
                actual: 0,
            });
        }
    }
    if seen.len() != storage.shard_grid_shape.element_count()? as usize {
        return Err(FormatError::BrickRecordGridMismatch {
            layer_id: layer_id.to_owned(),
        });
    }
    Ok(())
}

fn validate_no_data_policy(layer: &crate::manifest::LayerManifest) -> Result<(), FormatError> {
    let Some(policy) = layer.no_data_policy else {
        return Ok(());
    };
    if policy.kind != NoDataPolicyKind::SentinelValue {
        return Err(FormatError::InvalidNoDataPolicy {
            layer_id: layer.id.clone(),
            message: "kind must be sentinel_value".to_owned(),
        });
    }
    if policy.visibility_policy != NoDataVisibilityPolicy::InvisibleWith1VoxelInvalidDilation {
        return Err(FormatError::InvalidNoDataPolicy {
            layer_id: layer.id.clone(),
            message: "visibility_policy must be invisible_with_1_voxel_invalid_dilation".to_owned(),
        });
    }
    if policy.source_dtype != layer.dtype.source {
        return Err(FormatError::InvalidNoDataPolicy {
            layer_id: layer.id.clone(),
            message: "source_dtype must match the layer source dtype".to_owned(),
        });
    }
    match policy.source_dtype {
        IntensityDType::Uint8 => {
            if policy.source_value.fract() != 0.0
                || !(0.0..=f64::from(u8::MAX)).contains(&policy.source_value)
            {
                return Err(FormatError::InvalidNoDataPolicy {
                    layer_id: layer.id.clone(),
                    message: "uint8 sentinel source_value must be an integer in 0..=255".to_owned(),
                });
            }
        }
        IntensityDType::Uint16 => {
            if policy.source_value.fract() != 0.0
                || !(0.0..=f64::from(u16::MAX)).contains(&policy.source_value)
            {
                return Err(FormatError::InvalidNoDataPolicy {
                    layer_id: layer.id.clone(),
                    message: "uint16 sentinel source_value must be an integer in 0..=65535"
                        .to_owned(),
                });
            }
        }
        IntensityDType::Float32 => {
            if !policy.source_value.is_finite() {
                return Err(FormatError::InvalidNoDataPolicy {
                    layer_id: layer.id.clone(),
                    message: "float32 sentinel source_value must be finite".to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn validate_scale_registration(
    layer_id: &str,
    level: u32,
    previous_shape: Shape4D,
    previous_grid_to_world: GridToWorld,
    shape: Shape4D,
    grid_to_world: GridToWorld,
) -> Result<(), FormatError> {
    let Some(factors) = infer_downsample_factors(previous_shape, shape) else {
        return Err(FormatError::ScaleTransformMismatch {
            layer_id: layer_id.to_owned(),
            level,
        });
    };
    let expected =
        expected_downsampled_grid_to_world(previous_grid_to_world, factors).map_err(|source| {
            FormatError::InvalidTransform {
                layer_id: layer_id.to_owned(),
                source,
            }
        })?;
    if !grid_to_world_approx_eq(grid_to_world, expected, GRID_TO_WORLD_EPSILON) {
        return Err(FormatError::ScaleTransformMismatch {
            layer_id: layer_id.to_owned(),
            level,
        });
    }
    Ok(())
}

fn validate_bricks(
    layer_id: &str,
    scale: &ScaleManifest,
    expected_grid: Shape4D,
) -> Result<(), FormatError> {
    let mut seen = HashSet::new();
    for record in &scale.bricks.records {
        if record.index.t >= expected_grid.t()
            || record.index.z >= expected_grid.z()
            || record.index.y >= expected_grid.y()
            || record.index.x >= expected_grid.x()
            || !seen.insert(record.index)
        {
            return Err(FormatError::BrickRecordGridMismatch {
                layer_id: layer_id.to_owned(),
            });
        }
        if let Some(checksum) = &record.payload_checksum
            && (checksum.algorithm != BOOTSTRAP_CHECKSUM_ALGORITHM
                || checksum.scope != BOOTSTRAP_CHECKSUM_SCOPE
                || checksum.hex.len() != 64
                || !checksum.hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(FormatError::InvalidChecksum {
                layer_id: layer_id.to_owned(),
                index: record.index,
            });
        }
        if let Some(payload_bytes) = record.payload_bytes
            && payload_bytes == 0
        {
            return Err(FormatError::PayloadByteCountMismatch {
                layer_id: layer_id.to_owned(),
                index: record.index,
                recorded: payload_bytes,
                actual: 0,
            });
        }
        if record.payload_bytes.is_some() != record.payload_checksum.is_some() {
            return Err(FormatError::InvalidChecksum {
                layer_id: layer_id.to_owned(),
                index: record.index,
            });
        }
    }
    if seen.len() != expected_grid.element_count()? as usize {
        return Err(FormatError::BrickRecordGridMismatch {
            layer_id: layer_id.to_owned(),
        });
    }
    for record in &scale.bricks.records {
        let max_valid = chunk_voxel_count(scale.shape, scale.storage.brick_shape, record.index)?;
        if record.valid_voxel_count > max_valid
            || record.occupied != (record.valid_voxel_count > 0)
            || (!record.occupied && (record.min != 0.0 || record.max != 0.0))
            || (record.occupied && record.max < record.min)
        {
            return Err(FormatError::BrickRangeHierarchyMismatch {
                layer_id: layer_id.to_owned(),
            });
        }
    }

    let expected_hierarchy =
        BrickRangeHierarchy::from_brick_records(expected_grid, &scale.bricks.records);
    if scale.bricks.range_hierarchy != expected_hierarchy {
        return Err(FormatError::BrickRangeHierarchyMismatch {
            layer_id: layer_id.to_owned(),
        });
    }
    Ok(())
}

fn validate_scale_validity_metadata(
    root: &Path,
    layer: &crate::manifest::LayerManifest,
    scale: &ScaleManifest,
    expected_grid: Shape4D,
    mode: DatasetValidationMode,
) -> Result<(), FormatError> {
    let layer_has_policy = layer.no_data_policy.is_some();
    let Some(validity) = &scale.validity else {
        return if layer_has_policy {
            Err(FormatError::ValidityMaskMissing {
                layer_id: layer.id.clone(),
                level: scale.level,
            })
        } else {
            for record in &scale.bricks.records {
                let expected_count =
                    chunk_voxel_count(scale.shape, scale.storage.brick_shape, record.index)?;
                if record.valid_voxel_count != expected_count || !record.occupied {
                    return Err(FormatError::BrickRangeHierarchyMismatch {
                        layer_id: layer.id.clone(),
                    });
                }
            }
            Ok(())
        };
    };
    if !layer_has_policy {
        return Err(FormatError::ValidityMaskUnexpected {
            layer_id: layer.id.clone(),
            level: scale.level,
        });
    }
    if validity.encoding != ValidityMaskEncoding::Uint8RenderValidMask {
        return Err(FormatError::ValidityMaskMismatch {
            layer_id: layer.id.clone(),
            level: scale.level,
            message: "encoding must be uint8_render_valid_mask".to_owned(),
        });
    }
    if validity.storage.brick_shape != scale.storage.brick_shape {
        return Err(FormatError::ValidityMaskMismatch {
            layer_id: layer.id.clone(),
            level: scale.level,
            message: "validity brick shape must match intensity brick shape".to_owned(),
        });
    }
    validate_storage_metadata(
        layer.id.as_str(),
        IntensityDType::Uint8,
        validity.array_path.as_str(),
        scale.shape,
        &validity.storage,
    )?;
    if validity.array_path.starts_with('/')
        || validity.array_path.contains("..")
        || validity.array_path.trim().is_empty()
    {
        return Err(FormatError::InvalidArrayPath {
            layer_id: layer.id.clone(),
            array_path: validity.array_path.clone(),
        });
    }

    let shape =
        array_shape(root, &validity.array_path).map_err(|err| FormatError::ZarrStorage {
            layer_id: layer.id.clone(),
            message: err.to_string(),
        })?;
    if shape != scale.shape.to_zarr_shape() {
        return Err(FormatError::ValidityMaskMismatch {
            layer_id: layer.id.clone(),
            level: scale.level,
            message: format!(
                "validity shape {shape:?} != intensity shape {:?}",
                scale.shape.to_zarr_shape()
            ),
        });
    }
    let chunk_shape =
        array_chunk_shape(root, &validity.array_path).map_err(|err| FormatError::ZarrStorage {
            layer_id: layer.id.clone(),
            message: err.to_string(),
        })?;
    if chunk_shape != validity.storage.shard_shape.to_zarr_shape() {
        return Err(FormatError::ValidityMaskMismatch {
            layer_id: layer.id.clone(),
            level: scale.level,
            message: format!(
                "validity shard shape {chunk_shape:?} != storage shard shape {:?}",
                validity.storage.shard_shape.to_zarr_shape()
            ),
        });
    }
    let subchunk_shape = array_subchunk_shape(root, &validity.array_path).map_err(|err| {
        FormatError::ZarrStorage {
            layer_id: layer.id.clone(),
            message: err.to_string(),
        }
    })?;
    if subchunk_shape != Some(validity.storage.subchunk_shape.to_zarr_shape()) {
        return Err(FormatError::ValidityMaskMismatch {
            layer_id: layer.id.clone(),
            level: scale.level,
            message: format!(
                "validity subchunk shape {subchunk_shape:?} != storage subchunk shape {:?}",
                validity.storage.subchunk_shape.to_zarr_shape()
            ),
        });
    }
    let names = array_dimension_names(root, &validity.array_path).map_err(|err| {
        FormatError::ZarrStorage {
            layer_id: layer.id.clone(),
            message: err.to_string(),
        }
    })?;
    if names != AXES_TZYX.map(str::to_owned) {
        return Err(FormatError::ValidityMaskMismatch {
            layer_id: layer.id.clone(),
            level: scale.level,
            message: format!("validity dimension names {names:?} != {:?}", AXES_TZYX),
        });
    }
    validate_array_codecs(root, layer.id.as_str(), &validity.array_path)?;

    let mut seen = HashSet::new();
    let mut record_valid_sum = 0_u64;
    for record in &validity.records {
        if record.index.t >= expected_grid.t()
            || record.index.z >= expected_grid.z()
            || record.index.y >= expected_grid.y()
            || record.index.x >= expected_grid.x()
            || !seen.insert(record.index)
        {
            return Err(FormatError::ValidityMaskMismatch {
                layer_id: layer.id.clone(),
                level: scale.level,
                message: "validity records do not match the declared brick grid".to_owned(),
            });
        }
        if let Some(checksum) = &record.payload_checksum
            && (checksum.algorithm != BOOTSTRAP_CHECKSUM_ALGORITHM
                || checksum.scope != BOOTSTRAP_CHECKSUM_SCOPE
                || checksum.hex.len() != 64
                || !checksum.hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(FormatError::InvalidChecksum {
                layer_id: layer.id.clone(),
                index: record.index,
            });
        }
        if let Some(payload_bytes) = record.payload_bytes
            && payload_bytes == 0
        {
            return Err(FormatError::PayloadByteCountMismatch {
                layer_id: layer.id.clone(),
                index: record.index,
                recorded: payload_bytes,
                actual: 0,
            });
        }
        if record.payload_bytes.is_some() != record.payload_checksum.is_some() {
            return Err(FormatError::InvalidChecksum {
                layer_id: layer.id.clone(),
                index: record.index,
            });
        }
        let Some(brick_record) = scale
            .bricks
            .records
            .iter()
            .find(|brick_record| brick_record.index == record.index)
        else {
            return Err(FormatError::ValidityMaskMismatch {
                layer_id: layer.id.clone(),
                level: scale.level,
                message: "validity record has no matching brick record".to_owned(),
            });
        };
        if brick_record.valid_voxel_count != record.valid_voxel_count
            || brick_record.occupied != (record.valid_voxel_count > 0)
        {
            return Err(FormatError::ValidityMaskMismatch {
                layer_id: layer.id.clone(),
                level: scale.level,
                message: "brick valid counts or occupancy do not match validity records".to_owned(),
            });
        }
        let max_valid = chunk_voxel_count(scale.shape, scale.storage.brick_shape, record.index)?;
        if record.valid_voxel_count > max_valid {
            return Err(FormatError::ValidityMaskMismatch {
                layer_id: layer.id.clone(),
                level: scale.level,
                message: "validity record count exceeds chunk voxel count".to_owned(),
            });
        }
        record_valid_sum += record.valid_voxel_count;
    }
    if seen.len() != expected_grid.element_count()? as usize {
        return Err(FormatError::ValidityMaskMismatch {
            layer_id: layer.id.clone(),
            level: scale.level,
            message: "validity record count does not match brick grid".to_owned(),
        });
    }
    let total_voxels = scale.shape.element_count()?;
    if validity.valid_voxel_count != record_valid_sum
        || validity.invalid_voxel_count != total_voxels - record_valid_sum
    {
        return Err(FormatError::ValidityMaskMismatch {
            layer_id: layer.id.clone(),
            level: scale.level,
            message: "validity total counts do not match record counts".to_owned(),
        });
    }

    if mode == DatasetValidationMode::Quick {
        return Ok(());
    }

    let array = open_array(root, &validity.array_path).map_err(|err| FormatError::ZarrStorage {
        layer_id: layer.id.clone(),
        message: err.to_string(),
    })?;
    for record in &validity.storage.shard_records {
        let chunk_indices = [
            record.index.t,
            record.index.z,
            record.index.y,
            record.index.x,
        ];
        let bytes = array
            .retrieve_encoded_chunk(&chunk_indices)
            .map_err(|err| FormatError::ZarrStorage {
                layer_id: layer.id.clone(),
                message: err.to_string(),
            })?
            .ok_or_else(|| FormatError::PayloadMissing {
                layer_id: layer.id.clone(),
                index: record.index,
            })?;
        if record.payload_bytes != bytes.len() as u64 {
            return Err(FormatError::PayloadByteCountMismatch {
                layer_id: layer.id.clone(),
                index: record.index,
                recorded: record.payload_bytes,
                actual: bytes.len() as u64,
            });
        }
        let actual = blake3::hash(&bytes).to_hex().to_string();
        if actual != record.payload_checksum.hex {
            return Err(FormatError::PayloadChecksumMismatch {
                layer_id: layer.id.clone(),
                index: record.index,
            });
        }
    }
    for record in &validity.records {
        let ranges = chunk_ranges(scale.shape, scale.storage.brick_shape, record.index);
        let mask_values: Vec<u8> =
            array
                .retrieve_array_subset(&ranges)
                .map_err(|err| FormatError::ZarrStorage {
                    layer_id: layer.id.clone(),
                    message: err.to_string(),
                })?;
        if mask_values.iter().any(|value| !matches!(value, 0 | 1)) {
            return Err(FormatError::ValidityMaskMismatch {
                layer_id: layer.id.clone(),
                level: scale.level,
                message: "validity payload values must be 0 or 1".to_owned(),
            });
        }
        let decoded_valid_count = mask_values.iter().filter(|value| **value == 1).count() as u64;
        if decoded_valid_count != record.valid_voxel_count {
            return Err(FormatError::ValidityMaskMismatch {
                layer_id: layer.id.clone(),
                level: scale.level,
                message: "decoded validity count does not match record".to_owned(),
            });
        }
    }

    Ok(())
}

fn chunk_voxel_count(
    shape: Shape4D,
    chunk_shape: Shape4D,
    index: BrickIndex,
) -> Result<u64, FormatError> {
    let ranges = chunk_ranges(shape, chunk_shape, index);
    Ok(ranges.iter().map(|range| range.end - range.start).product())
}

fn chunk_ranges(
    shape: Shape4D,
    chunk_shape: Shape4D,
    index: BrickIndex,
) -> [std::ops::Range<u64>; 4] {
    let t0 = index.t * chunk_shape.t();
    let z0 = index.z * chunk_shape.z();
    let y0 = index.y * chunk_shape.y();
    let x0 = index.x * chunk_shape.x();
    [
        t0..(t0 + chunk_shape.t()).min(shape.t()),
        z0..(z0 + chunk_shape.z()).min(shape.z()),
        y0..(y0 + chunk_shape.y()).min(shape.y()),
        x0..(x0 + chunk_shape.x()).min(shape.x()),
    ]
}

fn validate_array_metadata(
    root: &Path,
    layer_id: &str,
    scale: &ScaleManifest,
    mode: DatasetValidationMode,
) -> Result<(), FormatError> {
    if scale.array_path.starts_with('/')
        || scale.array_path.contains("..")
        || scale.array_path.trim().is_empty()
    {
        return Err(FormatError::InvalidArrayPath {
            layer_id: layer_id.to_owned(),
            array_path: scale.array_path.clone(),
        });
    }

    let shape = array_shape(root, &scale.array_path).map_err(|err| FormatError::ZarrStorage {
        layer_id: layer_id.to_owned(),
        message: err.to_string(),
    })?;
    if shape != scale.shape.to_zarr_shape() {
        return Err(FormatError::ZarrMetadataMismatch {
            layer_id: layer_id.to_owned(),
            message: format!(
                "shape {shape:?} != manifest {:?}",
                scale.shape.to_zarr_shape()
            ),
        });
    }

    let chunk_shape =
        array_chunk_shape(root, &scale.array_path).map_err(|err| FormatError::ZarrStorage {
            layer_id: layer_id.to_owned(),
            message: err.to_string(),
        })?;
    if chunk_shape != scale.storage.shard_shape.to_zarr_shape() {
        return Err(FormatError::ZarrMetadataMismatch {
            layer_id: layer_id.to_owned(),
            message: format!(
                "shard shape {chunk_shape:?} != manifest {:?}",
                scale.storage.shard_shape.to_zarr_shape()
            ),
        });
    }
    let subchunk_shape =
        array_subchunk_shape(root, &scale.array_path).map_err(|err| FormatError::ZarrStorage {
            layer_id: layer_id.to_owned(),
            message: err.to_string(),
        })?;
    if subchunk_shape != Some(scale.storage.subchunk_shape.to_zarr_shape()) {
        return Err(FormatError::ZarrMetadataMismatch {
            layer_id: layer_id.to_owned(),
            message: format!(
                "subchunk shape {subchunk_shape:?} != manifest {:?}",
                scale.storage.subchunk_shape.to_zarr_shape()
            ),
        });
    }

    let names =
        array_dimension_names(root, &scale.array_path).map_err(|err| FormatError::ZarrStorage {
            layer_id: layer_id.to_owned(),
            message: err.to_string(),
        })?;
    if names != AXES_TZYX.map(str::to_owned) {
        return Err(FormatError::ZarrMetadataMismatch {
            layer_id: layer_id.to_owned(),
            message: format!("dimension names {names:?} != {:?}", AXES_TZYX),
        });
    }

    validate_array_codecs(root, layer_id, &scale.array_path)?;

    if mode == DatasetValidationMode::Quick {
        return Ok(());
    }

    let array = open_array(root, &scale.array_path).map_err(|err| FormatError::ZarrStorage {
        layer_id: layer_id.to_owned(),
        message: err.to_string(),
    })?;
    for record in &scale.storage.shard_records {
        let chunk_indices = [
            record.index.t,
            record.index.z,
            record.index.y,
            record.index.x,
        ];
        let bytes = array
            .retrieve_encoded_chunk(&chunk_indices)
            .map_err(|err| FormatError::ZarrStorage {
                layer_id: layer_id.to_owned(),
                message: err.to_string(),
            })?
            .ok_or_else(|| FormatError::PayloadMissing {
                layer_id: layer_id.to_owned(),
                index: record.index,
            })?;
        if record.payload_bytes != bytes.len() as u64 {
            return Err(FormatError::PayloadByteCountMismatch {
                layer_id: layer_id.to_owned(),
                index: record.index,
                recorded: record.payload_bytes,
                actual: bytes.len() as u64,
            });
        }
        let actual = blake3::hash(&bytes).to_hex().to_string();
        if actual != record.payload_checksum.hex {
            return Err(FormatError::PayloadChecksumMismatch {
                layer_id: layer_id.to_owned(),
                index: record.index,
            });
        }
    }
    Ok(())
}

fn validate_array_codecs(root: &Path, layer_id: &str, array_path: &str) -> Result<(), FormatError> {
    let metadata_path = root.join(array_path).join("zarr.json");
    let metadata_text =
        fs::read_to_string(&metadata_path).map_err(|source| FormatError::ReadManifest {
            path: metadata_path.clone(),
            source,
        })?;
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_text).map_err(|source| FormatError::ParseManifest {
            path: metadata_path,
            source,
        })?;
    let codecs = metadata
        .get("codecs")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| FormatError::ZarrMetadataMismatch {
            layer_id: layer_id.to_owned(),
            message: format!("array {array_path:?} is missing codec metadata"),
        })?;

    match codecs.as_slice() {
        [sharding] if is_supported_sharding_codec(sharding) => Ok(()),
        _ => Err(FormatError::UnsupportedZarrCodec {
            layer_id: layer_id.to_owned(),
            array_path: array_path.to_owned(),
            message:
                "expected sharding codec with bytes+zstd(level=3, checksum=false) inner codecs"
                    .to_owned(),
        }),
    }
}

fn is_supported_sharding_codec(codec: &serde_json::Value) -> bool {
    if codec.get("name").and_then(serde_json::Value::as_str) != Some("sharding_indexed") {
        return false;
    }
    let Some(configuration) = codec.get("configuration") else {
        return false;
    };
    let Some(inner_codecs) = configuration
        .get("codecs")
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    matches!(
        inner_codecs.as_slice(),
        [bytes, zstd] if is_little_endian_bytes_codec(bytes) && is_supported_zstd_codec(zstd)
    )
}

fn is_little_endian_bytes_codec(codec: &serde_json::Value) -> bool {
    codec.get("name").and_then(serde_json::Value::as_str) == Some("bytes")
        && codec
            .get("configuration")
            .and_then(|configuration| configuration.get("endian"))
            .and_then(serde_json::Value::as_str)
            == Some("little")
}

fn is_supported_zstd_codec(codec: &serde_json::Value) -> bool {
    codec.get("name").and_then(serde_json::Value::as_str) == Some("zstd")
        && codec
            .get("configuration")
            .and_then(|configuration| configuration.get("level"))
            .and_then(serde_json::Value::as_i64)
            == Some(i64::from(DENSE_INTENSITY_ZSTD_LEVEL))
        && codec
            .get("configuration")
            .and_then(|configuration| configuration.get("checksum"))
            .and_then(serde_json::Value::as_bool)
            == Some(false)
}

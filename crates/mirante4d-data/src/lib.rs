use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use mirante4d_domain::{GridToWorld, Shape3D, TimeIndex};
use mirante4d_format::{
    BrickIndex, DatasetId, LayerId, LayerManifest, NativeManifest, ScaleManifest, ValidatedDataset,
    load_and_validate_dataset_quick,
};
use zarrs::array::ArrayShardedReadableExtCache;

mod error;
mod payloads;
mod regions;
mod runtime_config;
mod runtime_support;
mod sharded_bricks;
mod types;
mod worker;
pub use error::DataError;
use payloads::*;
pub use regions::translated_region_grid_to_world;
use regions::{
    brick_record, brick_record_and_region, brick_region, validate_region_within_brick,
    validate_spatial_brick_index, validate_timepoint,
};
use runtime_config::DEFAULT_BRICK_CACHE_BYTES;
pub use runtime_config::{DataEngineDiagnostics, DataEngineStats, DataRuntimeConfig};
#[cfg(test)]
use runtime_config::{GIB, MIB};
use runtime_support::*;
use sharded_bricks::{
    ShardSplitContext, brick_cache_key, split_f32_shard_brick, split_u8_shard_brick,
    split_u16_shard_brick, storage_shard_brick_indices, storage_shard_region,
};
pub use types::{
    BrickMetadata, DenseVolumeF32, DenseVolumeU8, DenseVolumeU16, SpatialBrickIndex,
    VolumeBrickF32, VolumeBrickU8, VolumeBrickU16, VolumeRegion,
};
pub use worker::{
    BrickHistogramSample, BrickReadMetrics, BrickReadOutcome, BrickReadPayload, BrickReadPool,
    BrickReadQueueDiagnostics, BrickReadSpec, BrickReadStatus, BrickReadTicket,
    BrickRequestPriority, CancellationToken, CrossSectionChunkReadPool, CrossSectionChunkReadSpec,
    DataGenerationId, DataRequestId,
};

#[derive(Debug, Clone)]
pub struct DatasetHandle {
    root: PathBuf,
    dataset_id: DatasetId,
    manifest: NativeManifest,
    runtime: Arc<DataRuntime>,
}

#[derive(Debug)]
struct DataRuntime {
    config: DataRuntimeConfig,
    manifest_index: ManifestLookupIndex,
    cache: Mutex<VolumeCache>,
    brick_cache: Mutex<BrickCache>,
    shard_index_caches: Mutex<ShardIndexCaches>,
    stats: Mutex<DataEngineStats>,
}

#[derive(Debug, Default)]
struct ManifestLookupIndex {
    scales: HashMap<(String, u32), ScaleLookupIndex>,
}

#[derive(Debug, Default)]
struct ScaleLookupIndex {
    brick_records: HashMap<BrickIndex, usize>,
    intensity_shard_records: HashMap<BrickIndex, usize>,
    validity_shard_records: HashMap<BrickIndex, usize>,
}

impl DatasetHandle {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, DataError> {
        let ValidatedDataset { root, manifest } = load_and_validate_dataset_quick(root)?;
        Self::from_validated(root, manifest, DataRuntimeConfig::default())
    }

    pub fn open_with_cache_budget(
        root: impl AsRef<Path>,
        max_cache_bytes: u64,
    ) -> Result<Self, DataError> {
        Self::open_with_cache_budgets(root, max_cache_bytes, DEFAULT_BRICK_CACHE_BYTES)
    }

    pub fn open_with_cache_budgets(
        root: impl AsRef<Path>,
        max_volume_cache_bytes: u64,
        max_brick_cache_bytes: u64,
    ) -> Result<Self, DataError> {
        let ValidatedDataset { root, manifest } = load_and_validate_dataset_quick(root)?;
        Self::from_validated(
            root,
            manifest,
            DataRuntimeConfig::from_cache_budgets(max_volume_cache_bytes, max_brick_cache_bytes),
        )
    }

    pub fn open_with_runtime_config(
        root: impl AsRef<Path>,
        config: DataRuntimeConfig,
    ) -> Result<Self, DataError> {
        let ValidatedDataset { root, manifest } = load_and_validate_dataset_quick(root)?;
        Self::from_validated(root, manifest, config)
    }

    fn from_validated(
        root: PathBuf,
        manifest: NativeManifest,
        config: DataRuntimeConfig,
    ) -> Result<Self, DataError> {
        let dataset_id = DatasetId::new(manifest.dataset.id.clone())?;
        let manifest_index = manifest_lookup_index(&manifest);
        Ok(Self {
            root,
            dataset_id,
            manifest,
            runtime: Arc::new(DataRuntime {
                config,
                manifest_index,
                cache: Mutex::new(VolumeCache::new(config.volume_cache_budget_bytes)),
                brick_cache: Mutex::new(BrickCache::new(config.brick_cache_budget_bytes)),
                shard_index_caches: Mutex::new(ShardIndexCaches::default()),
                stats: Mutex::new(DataEngineStats::default()),
            }),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manifest(&self) -> &NativeManifest {
        &self.manifest
    }

    pub fn dataset_id(&self) -> &DatasetId {
        &self.dataset_id
    }

    pub fn dataset_name(&self) -> &str {
        &self.manifest.dataset.name
    }

    pub fn layer_count(&self) -> usize {
        self.manifest.layers.len()
    }

    pub fn layer(&self, layer_id: &LayerId) -> Option<&LayerManifest> {
        self.manifest
            .layers
            .iter()
            .find(|layer| layer.id == layer_id.as_str())
    }

    pub fn first_layer_id(&self) -> Result<LayerId, DataError> {
        let layer = self
            .manifest
            .layers
            .first()
            .ok_or_else(|| DataError::LayerNotFound("<first>".to_owned()))?;
        Ok(LayerId::new(layer.id.clone()).expect("validated layer id"))
    }

    pub fn stats(&self) -> Result<DataEngineStats, DataError> {
        self.runtime
            .stats
            .lock()
            .map(|stats| *stats)
            .map_err(|_| DataError::CachePoisoned)
    }

    pub fn runtime_config(&self) -> DataRuntimeConfig {
        self.runtime.config
    }

    pub fn diagnostics(&self) -> Result<DataEngineDiagnostics, DataError> {
        Ok(DataEngineDiagnostics {
            config: self.runtime_config(),
            stats: self.stats()?,
        })
    }

    pub fn read_u16_volume(
        &self,
        layer_id: &LayerId,
        timepoint: TimeIndex,
    ) -> Result<DenseVolumeU16, DataError> {
        self.read_u16_volume_at_scale(layer_id, 0, timepoint)
    }

    pub fn read_u8_volume(
        &self,
        layer_id: &LayerId,
        timepoint: TimeIndex,
    ) -> Result<DenseVolumeU8, DataError> {
        self.read_u8_volume_at_scale(layer_id, 0, timepoint)
    }

    pub fn read_u8_volume_at_scale(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
    ) -> Result<DenseVolumeU8, DataError> {
        let key = VolumeCacheKey {
            layer_id: layer_id.to_string(),
            scale_level,
            timepoint: timepoint.get(),
        };
        if let Some(volume) = self.cache_get_u8(&key)? {
            self.record_cache_hit()?;
            return Ok(volume);
        }
        self.record_cache_miss()?;
        let read = self.read_u8_volume_uncached(layer_id, scale_level, timepoint)?;
        let volume = read.volume;
        self.record_subset_read(
            volume.values.len() as u64,
            std::mem::size_of::<u8>() as u64,
            read.diagnostics,
        )?;
        let cache_update = self.cache_insert_u8(key, volume.clone())?;
        self.record_cache_update(cache_update)?;
        Ok(volume)
    }

    pub fn read_u16_volume_at_scale(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
    ) -> Result<DenseVolumeU16, DataError> {
        let key = VolumeCacheKey {
            layer_id: layer_id.to_string(),
            scale_level,
            timepoint: timepoint.get(),
        };
        if let Some(volume) = self.cache_get_u16(&key)? {
            self.record_cache_hit()?;
            return Ok(volume);
        }
        self.record_cache_miss()?;
        let read = self.read_u16_volume_uncached(layer_id, scale_level, timepoint)?;
        let volume = read.volume;
        self.record_subset_read(
            volume.values.len() as u64,
            std::mem::size_of::<u16>() as u64,
            read.diagnostics,
        )?;
        let cache_update = self.cache_insert_u16(key, volume.clone())?;
        self.record_cache_update(cache_update)?;
        Ok(volume)
    }

    pub fn read_u8_region(
        &self,
        layer_id: &LayerId,
        timepoint: TimeIndex,
        region: VolumeRegion,
    ) -> Result<DenseVolumeU8, DataError> {
        let read = self.read_u8_region_at_scale_uncached(layer_id, 0, timepoint, region)?;
        self.record_subset_read(
            read.volume.values.len() as u64,
            std::mem::size_of::<u8>() as u64,
            read.diagnostics,
        )?;
        Ok(read.volume)
    }

    pub fn read_u16_region(
        &self,
        layer_id: &LayerId,
        timepoint: TimeIndex,
        region: VolumeRegion,
    ) -> Result<DenseVolumeU16, DataError> {
        let read = self.read_u16_region_at_scale_uncached(layer_id, 0, timepoint, region)?;
        self.record_subset_read(
            read.volume.values.len() as u64,
            std::mem::size_of::<u16>() as u64,
            read.diagnostics,
        )?;
        Ok(read.volume)
    }

    pub fn read_f32_volume(
        &self,
        layer_id: &LayerId,
        timepoint: TimeIndex,
    ) -> Result<DenseVolumeF32, DataError> {
        self.read_f32_volume_at_scale(layer_id, 0, timepoint)
    }

    pub fn read_f32_volume_at_scale(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
    ) -> Result<DenseVolumeF32, DataError> {
        let read = self.read_f32_volume_uncached(layer_id, scale_level, timepoint)?;
        self.record_subset_read(
            read.volume.values.len() as u64,
            std::mem::size_of::<f32>() as u64,
            read.diagnostics,
        )?;
        Ok(read.volume)
    }

    pub fn read_f32_region(
        &self,
        layer_id: &LayerId,
        timepoint: TimeIndex,
        region: VolumeRegion,
    ) -> Result<DenseVolumeF32, DataError> {
        let read = self.read_f32_region_at_scale_uncached(layer_id, 0, timepoint, region)?;
        self.record_subset_read(
            read.volume.values.len() as u64,
            std::mem::size_of::<f32>() as u64,
            read.diagnostics,
        )?;
        Ok(read.volume)
    }

    pub fn brick_grid_shape(&self, layer_id: &LayerId) -> Result<Shape3D, DataError> {
        self.brick_grid_shape_at_scale(layer_id, 0)
    }

    pub fn brick_grid_shape_at_scale(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
    ) -> Result<Shape3D, DataError> {
        let scale = self.scale(layer_id, scale_level)?;
        Ok(scale.bricks.grid_shape.spatial())
    }

    pub fn brick_shape(&self, layer_id: &LayerId) -> Result<Shape3D, DataError> {
        self.brick_shape_at_scale(layer_id, 0)
    }

    pub fn brick_shape_at_scale(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
    ) -> Result<Shape3D, DataError> {
        let scale = self.scale(layer_id, scale_level)?;
        Ok(scale.storage.brick_shape.spatial())
    }

    pub fn storage_shard_index_for_brick(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        brick_index: SpatialBrickIndex,
    ) -> Result<SpatialBrickIndex, DataError> {
        let scale = self.scale(layer_id, scale_level)?;
        validate_spatial_brick_index(scale, brick_index)?;
        let chunks_per_shard = scale.storage.chunks_per_shard;
        if chunks_per_shard.z() == 0 || chunks_per_shard.y() == 0 || chunks_per_shard.x() == 0 {
            return Err(DataError::ReadFailed {
                layer_id: layer_id.to_string(),
                message: "storage chunks_per_shard must be positive".to_owned(),
            });
        }
        Ok(SpatialBrickIndex {
            z: brick_index.z / chunks_per_shard.z(),
            y: brick_index.y / chunks_per_shard.y(),
            x: brick_index.x / chunks_per_shard.x(),
        })
    }

    pub fn storage_shard_shape_for_brick(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        brick_index: SpatialBrickIndex,
    ) -> Result<Shape3D, DataError> {
        let scale = self.scale(layer_id, scale_level)?;
        let shard_index = self.storage_shard_index_for_brick(layer_id, scale_level, brick_index)?;
        storage_shard_region(scale, shard_index)?
            .shape()
            .map_err(DataError::InvalidShape)
    }

    fn storage_shard_bricks_for_request(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        anchor_brick_index: SpatialBrickIndex,
        requested_bricks: &[SpatialBrickIndex],
    ) -> Result<(SpatialBrickIndex, Vec<SpatialBrickIndex>), DataError> {
        let scale = self.scale(layer_id, scale_level)?;
        let shard_index =
            self.storage_shard_index_for_brick(layer_id, scale_level, anchor_brick_index)?;
        for brick in requested_bricks {
            let candidate = self.storage_shard_index_for_brick(layer_id, scale_level, *brick)?;
            if candidate != shard_index {
                return Err(DataError::ReadFailed {
                    layer_id: layer_id.to_string(),
                    message: "coalesced brick request spans multiple storage shards".to_owned(),
                });
            }
        }
        Ok((
            shard_index,
            storage_shard_brick_indices(scale, shard_index)?,
        ))
    }

    pub fn scale_shape(&self, layer_id: &LayerId, scale_level: u32) -> Result<Shape3D, DataError> {
        let scale = self.scale(layer_id, scale_level)?;
        Ok(scale.shape.spatial())
    }

    pub fn scale_grid_to_world(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
    ) -> Result<GridToWorld, DataError> {
        let scale = self.scale(layer_id, scale_level)?;
        Ok(scale.grid_to_world)
    }

    pub fn scale_count(&self, layer_id: &LayerId) -> Result<usize, DataError> {
        let layer = self
            .layer(layer_id)
            .ok_or_else(|| DataError::LayerNotFound(layer_id.to_string()))?;
        validate_dense_layer(layer_id, layer)?;
        Ok(layer.scales.len())
    }

    fn scale(&self, layer_id: &LayerId, scale_level: u32) -> Result<&ScaleManifest, DataError> {
        let layer = self
            .layer(layer_id)
            .ok_or_else(|| DataError::LayerNotFound(layer_id.to_string()))?;
        validate_dense_layer(layer_id, layer)?;
        layer
            .scales
            .iter()
            .find(|scale| scale.level == scale_level)
            .ok_or_else(|| DataError::ScaleNotFound {
                layer_id: layer_id.to_string(),
                scale_level,
            })
    }

    fn scale_lookup(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
    ) -> Result<&ScaleLookupIndex, DataError> {
        self.runtime
            .manifest_index
            .scales
            .get(&(layer_id.to_string(), scale_level))
            .ok_or_else(|| DataError::ScaleNotFound {
                layer_id: layer_id.to_string(),
                scale_level,
            })
    }

    fn indexed_brick_record(
        &self,
        layer_id: &LayerId,
        scale: &ScaleManifest,
        scale_level: u32,
        index: BrickIndex,
    ) -> Result<mirante4d_format::BrickRecord, DataError> {
        let lookup = self.scale_lookup(layer_id, scale_level)?;
        lookup
            .brick_records
            .get(&index)
            .and_then(|record_index| scale.bricks.records.get(*record_index))
            .cloned()
            .ok_or_else(|| DataError::BrickRecordMissing {
                layer_id: layer_id.to_string(),
                index,
            })
    }

    fn indexed_encoded_payload_bytes_for_timepoint_region(
        &self,
        layer_id: &LayerId,
        scale: &ScaleManifest,
        scale_level: u32,
        timepoint: TimeIndex,
        region: &VolumeRegion,
    ) -> Result<u64, DataError> {
        let lookup = self.scale_lookup(layer_id, scale_level)?;
        let ends = region.ends()?;
        let t_chunks = chunk_index_range(
            timepoint.get()..timepoint.get() + 1,
            scale.storage.shard_shape.t(),
        );
        let z_chunks = chunk_index_range(region.z_start..ends.z, scale.storage.shard_shape.z());
        let y_chunks = chunk_index_range(region.y_start..ends.y, scale.storage.shard_shape.y());
        let x_chunks = chunk_index_range(region.x_start..ends.x, scale.storage.shard_shape.x());

        let mut payload_bytes = 0u64;
        let mut seen = HashSet::new();
        for t in t_chunks {
            for z in z_chunks.clone() {
                for y in y_chunks.clone() {
                    for x in x_chunks.clone() {
                        let index = BrickIndex { t, z, y, x };
                        if seen.insert(("intensity", index)) {
                            payload_bytes = payload_bytes.saturating_add(
                                lookup
                                    .intensity_shard_records
                                    .get(&index)
                                    .and_then(|record_index| {
                                        scale.storage.shard_records.get(*record_index)
                                    })
                                    .map(|record| record.payload_bytes)
                                    .ok_or_else(|| DataError::BrickRecordMissing {
                                        layer_id: layer_id.to_string(),
                                        index,
                                    })?,
                            );
                        }
                        if let Some(validity) = &scale.validity
                            && seen.insert(("validity", index))
                        {
                            payload_bytes = payload_bytes.saturating_add(
                                lookup
                                    .validity_shard_records
                                    .get(&index)
                                    .and_then(|record_index| {
                                        validity.storage.shard_records.get(*record_index)
                                    })
                                    .map(|record| record.payload_bytes)
                                    .ok_or_else(|| DataError::BrickRecordMissing {
                                        layer_id: layer_id.to_string(),
                                        index,
                                    })?,
                            );
                        }
                    }
                }
            }
        }
        Ok(payload_bytes)
    }

    pub fn brick_metadata(
        &self,
        layer_id: &LayerId,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
    ) -> Result<BrickMetadata, DataError> {
        self.brick_metadata_at_scale(layer_id, 0, timepoint, brick_index)
    }

    pub fn brick_metadata_at_scale(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
    ) -> Result<BrickMetadata, DataError> {
        let layer = self
            .layer(layer_id)
            .ok_or_else(|| DataError::LayerNotFound(layer_id.to_string()))?;
        validate_dense_layer(layer_id, layer)?;
        validate_timepoint(layer_id, layer, timepoint)?;

        let scale = self.scale(layer_id, scale_level)?;
        validate_spatial_brick_index(scale, brick_index)?;
        let chunk_index = BrickIndex {
            t: timepoint.get() / scale.storage.brick_shape.t(),
            z: brick_index.z,
            y: brick_index.y,
            x: brick_index.x,
        };
        let record = self.indexed_brick_record(layer_id, scale, scale_level, chunk_index)?;
        let region = brick_region(scale, brick_index)?;
        Ok(BrickMetadata {
            scale_level,
            brick_index,
            chunk_index,
            region,
            grid_to_world: translated_region_grid_to_world(scale.grid_to_world, region),
            occupied: record.occupied,
            valid_voxel_count: record.valid_voxel_count,
            min: record.min,
            max: record.max,
            payload_bytes: self.indexed_encoded_payload_bytes_for_timepoint_region(
                layer_id,
                scale,
                scale_level,
                timepoint,
                &region,
            )?,
        })
    }

    pub fn read_u16_brick(
        &self,
        layer_id: &LayerId,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
    ) -> Result<VolumeBrickU16, DataError> {
        self.read_u16_brick_at_scale(layer_id, 0, timepoint, brick_index)
    }

    pub fn read_u8_brick(
        &self,
        layer_id: &LayerId,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
    ) -> Result<VolumeBrickU8, DataError> {
        self.read_u8_brick_at_scale(layer_id, 0, timepoint, brick_index)
    }

    pub fn read_u8_brick_at_scale(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
    ) -> Result<VolumeBrickU8, DataError> {
        self.read_u8_brick_at_scale_cancellable(
            layer_id,
            scale_level,
            timepoint,
            brick_index,
            || false,
        )?
        .ok_or(DataError::WorkerQueueClosed)
    }

    pub(crate) fn read_u8_brick_at_scale_cancellable(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
        is_cancelled: impl Fn() -> bool,
    ) -> Result<Option<VolumeBrickU8>, DataError> {
        let key = BrickCacheKey {
            layer_id: layer_id.to_string(),
            scale_level,
            timepoint: timepoint.get(),
            z: brick_index.z,
            y: brick_index.y,
            x: brick_index.x,
        };
        if is_cancelled() {
            return Ok(None);
        }
        if let Some(brick) = self.brick_cache_get_u8(&key)? {
            self.record_brick_cache_hit()?;
            if is_cancelled() {
                return Ok(None);
            }
            return Ok(Some(brick));
        }
        self.record_brick_cache_miss()?;
        let read = self.read_u8_brick_uncached(layer_id, scale_level, timepoint, brick_index)?;
        let brick = read.brick;
        if is_cancelled() {
            return Ok(None);
        }
        let decoded = brick.volume.values.len() as u64;
        self.record_subset_read(decoded, std::mem::size_of::<u8>() as u64, read.diagnostics)?;
        self.record_brick_read(decoded, std::mem::size_of::<u8>() as u64)?;
        let cache_update = self.brick_cache_insert_u8(key, brick.clone())?;
        self.record_brick_cache_update(cache_update)?;
        Ok(Some(brick))
    }

    pub(crate) fn read_u8_brick_region_at_scale_cancellable(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
        region: VolumeRegion,
        is_cancelled: impl Fn() -> bool,
    ) -> Result<Option<VolumeBrickU8>, DataError> {
        if is_cancelled() {
            return Ok(None);
        }
        let read = self.read_u8_brick_region_uncached(
            layer_id,
            scale_level,
            timepoint,
            brick_index,
            region,
        )?;
        let brick = read.brick;
        if is_cancelled() {
            return Ok(None);
        }
        let decoded = brick.volume.values.len() as u64;
        self.record_subset_read(decoded, std::mem::size_of::<u8>() as u64, read.diagnostics)?;
        self.record_brick_read(decoded, std::mem::size_of::<u8>() as u64)?;
        Ok(Some(brick))
    }

    pub fn read_u16_brick_at_scale(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
    ) -> Result<VolumeBrickU16, DataError> {
        self.read_u16_brick_at_scale_cancellable(
            layer_id,
            scale_level,
            timepoint,
            brick_index,
            || false,
        )?
        .ok_or(DataError::WorkerQueueClosed)
    }

    pub(crate) fn read_u16_brick_at_scale_cancellable(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
        is_cancelled: impl Fn() -> bool,
    ) -> Result<Option<VolumeBrickU16>, DataError> {
        let key = BrickCacheKey {
            layer_id: layer_id.to_string(),
            scale_level,
            timepoint: timepoint.get(),
            z: brick_index.z,
            y: brick_index.y,
            x: brick_index.x,
        };
        if is_cancelled() {
            return Ok(None);
        }
        if let Some(brick) = self.brick_cache_get_u16(&key)? {
            self.record_brick_cache_hit()?;
            if is_cancelled() {
                return Ok(None);
            }
            return Ok(Some(brick));
        }
        self.record_brick_cache_miss()?;
        let read = self.read_u16_brick_uncached(layer_id, scale_level, timepoint, brick_index)?;
        let brick = read.brick;
        if is_cancelled() {
            return Ok(None);
        }
        let decoded = brick.volume.values.len() as u64;
        self.record_subset_read(decoded, std::mem::size_of::<u16>() as u64, read.diagnostics)?;
        self.record_brick_read(decoded, std::mem::size_of::<u16>() as u64)?;
        let cache_update = self.brick_cache_insert_u16(key, brick.clone())?;
        self.record_brick_cache_update(cache_update)?;
        Ok(Some(brick))
    }

    pub(crate) fn read_u16_brick_region_at_scale_cancellable(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
        region: VolumeRegion,
        is_cancelled: impl Fn() -> bool,
    ) -> Result<Option<VolumeBrickU16>, DataError> {
        if is_cancelled() {
            return Ok(None);
        }
        let read = self.read_u16_brick_region_uncached(
            layer_id,
            scale_level,
            timepoint,
            brick_index,
            region,
        )?;
        let brick = read.brick;
        if is_cancelled() {
            return Ok(None);
        }
        let decoded = brick.volume.values.len() as u64;
        self.record_subset_read(decoded, std::mem::size_of::<u16>() as u64, read.diagnostics)?;
        self.record_brick_read(decoded, std::mem::size_of::<u16>() as u64)?;
        Ok(Some(brick))
    }

    pub fn read_f32_brick(
        &self,
        layer_id: &LayerId,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
    ) -> Result<VolumeBrickF32, DataError> {
        self.read_f32_brick_at_scale(layer_id, 0, timepoint, brick_index)
    }

    pub fn read_f32_brick_at_scale(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
    ) -> Result<VolumeBrickF32, DataError> {
        self.read_f32_brick_at_scale_cancellable(
            layer_id,
            scale_level,
            timepoint,
            brick_index,
            || false,
        )?
        .ok_or(DataError::WorkerQueueClosed)
    }

    pub(crate) fn read_f32_brick_at_scale_cancellable(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
        is_cancelled: impl Fn() -> bool,
    ) -> Result<Option<VolumeBrickF32>, DataError> {
        let key = BrickCacheKey {
            layer_id: layer_id.to_string(),
            scale_level,
            timepoint: timepoint.get(),
            z: brick_index.z,
            y: brick_index.y,
            x: brick_index.x,
        };
        if is_cancelled() {
            return Ok(None);
        }
        if let Some(brick) = self.brick_cache_get_f32(&key)? {
            self.record_brick_cache_hit()?;
            if is_cancelled() {
                return Ok(None);
            }
            return Ok(Some(brick));
        }
        self.record_brick_cache_miss()?;
        let read = self.read_f32_brick_uncached(layer_id, scale_level, timepoint, brick_index)?;
        let brick = read.brick;
        if is_cancelled() {
            return Ok(None);
        }
        let decoded = brick.volume.values.len() as u64;
        self.record_subset_read(decoded, std::mem::size_of::<f32>() as u64, read.diagnostics)?;
        self.record_brick_read(decoded, std::mem::size_of::<f32>() as u64)?;
        let cache_update = self.brick_cache_insert_f32(key, brick.clone())?;
        self.record_brick_cache_update(cache_update)?;
        Ok(Some(brick))
    }

    pub(crate) fn read_f32_brick_region_at_scale_cancellable(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
        region: VolumeRegion,
        is_cancelled: impl Fn() -> bool,
    ) -> Result<Option<VolumeBrickF32>, DataError> {
        if is_cancelled() {
            return Ok(None);
        }
        let read = self.read_f32_brick_region_uncached(
            layer_id,
            scale_level,
            timepoint,
            brick_index,
            region,
        )?;
        let brick = read.brick;
        if is_cancelled() {
            return Ok(None);
        }
        let decoded = brick.volume.values.len() as u64;
        self.record_subset_read(decoded, std::mem::size_of::<f32>() as u64, read.diagnostics)?;
        self.record_brick_read(decoded, std::mem::size_of::<f32>() as u64)?;
        Ok(Some(brick))
    }

    pub(crate) fn read_u8_brick_group_at_scale_cancellable(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        anchor_brick_index: SpatialBrickIndex,
        requested_bricks: &[SpatialBrickIndex],
        is_cancelled: impl Fn() -> bool,
    ) -> Result<Option<Vec<VolumeBrickU8>>, DataError> {
        let (shard_index, shard_bricks) = self.storage_shard_bricks_for_request(
            layer_id,
            scale_level,
            anchor_brick_index,
            requested_bricks,
        )?;
        let mut cached = Vec::with_capacity(shard_bricks.len());
        let mut missing = 0usize;
        for brick in &shard_bricks {
            let key = brick_cache_key(layer_id, scale_level, timepoint, *brick);
            if let Some(payload) = self.brick_cache_get_u8(&key)? {
                self.record_brick_cache_hit()?;
                cached.push(payload);
            } else {
                self.record_brick_cache_miss()?;
                missing += 1;
            }
        }
        if is_cancelled() {
            return Ok(None);
        }
        if missing == 0 {
            return Ok(Some(cached));
        }

        let scale = self.scale(layer_id, scale_level)?;
        let shard_region = storage_shard_region(scale, shard_index)?;
        let read =
            self.read_u8_region_at_scale_uncached(layer_id, scale_level, timepoint, shard_region)?;
        if is_cancelled() {
            return Ok(None);
        }
        let decoded = read.volume.values.len() as u64;
        self.record_subset_read(decoded, std::mem::size_of::<u8>() as u64, read.diagnostics)?;
        self.record_brick_read(decoded, std::mem::size_of::<u8>() as u64)?;
        let mut bricks = Vec::with_capacity(shard_bricks.len());
        let split_context = ShardSplitContext {
            dataset_id: &self.dataset_id,
            layer_id,
            scale,
            scale_level,
            timepoint,
            shard_region,
        };
        for brick_index in shard_bricks {
            let brick = split_u8_shard_brick(split_context, &read.volume, brick_index)?;
            let key = brick_cache_key(layer_id, scale_level, timepoint, brick_index);
            let cache_update = self.brick_cache_insert_u8(key, brick.clone())?;
            self.record_brick_cache_update(cache_update)?;
            bricks.push(brick);
        }
        Ok(Some(bricks))
    }

    pub(crate) fn read_u16_brick_group_at_scale_cancellable(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        anchor_brick_index: SpatialBrickIndex,
        requested_bricks: &[SpatialBrickIndex],
        is_cancelled: impl Fn() -> bool,
    ) -> Result<Option<Vec<VolumeBrickU16>>, DataError> {
        let (shard_index, shard_bricks) = self.storage_shard_bricks_for_request(
            layer_id,
            scale_level,
            anchor_brick_index,
            requested_bricks,
        )?;
        let mut cached = Vec::with_capacity(shard_bricks.len());
        let mut missing = 0usize;
        for brick in &shard_bricks {
            let key = brick_cache_key(layer_id, scale_level, timepoint, *brick);
            if let Some(payload) = self.brick_cache_get_u16(&key)? {
                self.record_brick_cache_hit()?;
                cached.push(payload);
            } else {
                self.record_brick_cache_miss()?;
                missing += 1;
            }
        }
        if is_cancelled() {
            return Ok(None);
        }
        if missing == 0 {
            return Ok(Some(cached));
        }

        let scale = self.scale(layer_id, scale_level)?;
        let shard_region = storage_shard_region(scale, shard_index)?;
        let read =
            self.read_u16_region_at_scale_uncached(layer_id, scale_level, timepoint, shard_region)?;
        if is_cancelled() {
            return Ok(None);
        }
        let decoded = read.volume.values.len() as u64;
        self.record_subset_read(decoded, std::mem::size_of::<u16>() as u64, read.diagnostics)?;
        self.record_brick_read(decoded, std::mem::size_of::<u16>() as u64)?;
        let mut bricks = Vec::with_capacity(shard_bricks.len());
        let split_context = ShardSplitContext {
            dataset_id: &self.dataset_id,
            layer_id,
            scale,
            scale_level,
            timepoint,
            shard_region,
        };
        for brick_index in shard_bricks {
            let brick = split_u16_shard_brick(split_context, &read.volume, brick_index)?;
            let key = brick_cache_key(layer_id, scale_level, timepoint, brick_index);
            let cache_update = self.brick_cache_insert_u16(key, brick.clone())?;
            self.record_brick_cache_update(cache_update)?;
            bricks.push(brick);
        }
        Ok(Some(bricks))
    }

    pub(crate) fn read_f32_brick_group_at_scale_cancellable(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        anchor_brick_index: SpatialBrickIndex,
        requested_bricks: &[SpatialBrickIndex],
        is_cancelled: impl Fn() -> bool,
    ) -> Result<Option<Vec<VolumeBrickF32>>, DataError> {
        let (shard_index, shard_bricks) = self.storage_shard_bricks_for_request(
            layer_id,
            scale_level,
            anchor_brick_index,
            requested_bricks,
        )?;
        let mut cached = Vec::with_capacity(shard_bricks.len());
        let mut missing = 0usize;
        for brick in &shard_bricks {
            let key = brick_cache_key(layer_id, scale_level, timepoint, *brick);
            if let Some(payload) = self.brick_cache_get_f32(&key)? {
                self.record_brick_cache_hit()?;
                cached.push(payload);
            } else {
                self.record_brick_cache_miss()?;
                missing += 1;
            }
        }
        if is_cancelled() {
            return Ok(None);
        }
        if missing == 0 {
            return Ok(Some(cached));
        }

        let scale = self.scale(layer_id, scale_level)?;
        let shard_region = storage_shard_region(scale, shard_index)?;
        let read =
            self.read_f32_region_at_scale_uncached(layer_id, scale_level, timepoint, shard_region)?;
        if is_cancelled() {
            return Ok(None);
        }
        let decoded = read.volume.values.len() as u64;
        self.record_subset_read(decoded, std::mem::size_of::<f32>() as u64, read.diagnostics)?;
        self.record_brick_read(decoded, std::mem::size_of::<f32>() as u64)?;
        let mut bricks = Vec::with_capacity(shard_bricks.len());
        let split_context = ShardSplitContext {
            dataset_id: &self.dataset_id,
            layer_id,
            scale,
            scale_level,
            timepoint,
            shard_region,
        };
        for brick_index in shard_bricks {
            let brick = split_f32_shard_brick(split_context, &read.volume, brick_index)?;
            let key = brick_cache_key(layer_id, scale_level, timepoint, brick_index);
            let cache_update = self.brick_cache_insert_f32(key, brick.clone())?;
            self.record_brick_cache_update(cache_update)?;
            bricks.push(brick);
        }
        Ok(Some(bricks))
    }

    fn read_u8_region_at_scale_uncached(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        region: VolumeRegion,
    ) -> Result<UncachedU8VolumeRead, DataError> {
        let layer = self
            .layer(layer_id)
            .ok_or_else(|| DataError::LayerNotFound(layer_id.to_string()))?;
        validate_u8_dense_layer(layer_id, layer)?;
        validate_timepoint(layer_id, layer, timepoint)?;

        let scale = self.scale(layer_id, scale_level)?;
        let spatial = scale.shape.spatial();
        let ends = region.validate_within(spatial)?;
        let encoded =
            encoded_payload_diagnostics_for_timepoint_region(layer_id, scale, timepoint, &region)?;
        let array = mirante4d_format::zarr_io::open_array(&self.root, &scale.array_path).map_err(
            |err| DataError::ReadFailed {
                layer_id: layer_id.to_string(),
                message: err.to_string(),
            },
        )?;
        let subset = [
            timepoint.get()..timepoint.get() + 1,
            region.z_start..ends.z,
            region.y_start..ends.y,
            region.x_start..ends.x,
        ];
        let intensity_cache = self.shard_index_cache(&scale.array_path, &array)?;
        let values_read = retrieve_sharded_subset::<Vec<u8>>(
            &array,
            &intensity_cache,
            layer_id,
            &subset,
            encoded.intensity_shard_payloads,
        )?;
        let render_valid_read = self.retrieve_render_valid_subset(
            layer_id,
            scale,
            &subset,
            encoded.validity_shard_payloads,
        )?;
        let values = values_read.values;
        let render_valid = render_valid_read.values;
        let shape = region.shape()?;
        let expected_len = shape.element_count()? as usize;
        if values.len() != expected_len {
            return Err(DataError::ReadFailed {
                layer_id: layer_id.to_string(),
                message: format!("decoded {} values, expected {expected_len}", values.len()),
            });
        }
        validate_render_valid_len(layer_id, render_valid.as_deref(), expected_len)?;
        Ok(UncachedU8VolumeRead {
            volume: DenseVolumeU8 {
                dataset_id: self.dataset_id.clone(),
                layer_id: layer_id.clone(),
                scale_level,
                timepoint,
                shape,
                grid_to_world: translated_region_grid_to_world(scale.grid_to_world, region),
                values,
                render_valid,
            },
            diagnostics: self.sharded_read_diagnostics(
                encoded,
                values_read.cache,
                render_valid_read.cache,
            )?,
        })
    }

    fn read_u16_region_at_scale_uncached(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        region: VolumeRegion,
    ) -> Result<UncachedU16VolumeRead, DataError> {
        let layer = self
            .layer(layer_id)
            .ok_or_else(|| DataError::LayerNotFound(layer_id.to_string()))?;
        validate_u16_dense_layer(layer_id, layer)?;
        validate_timepoint(layer_id, layer, timepoint)?;

        let scale = self.scale(layer_id, scale_level)?;
        let spatial = scale.shape.spatial();
        let ends = region.validate_within(spatial)?;
        let encoded =
            encoded_payload_diagnostics_for_timepoint_region(layer_id, scale, timepoint, &region)?;
        let array = mirante4d_format::zarr_io::open_array(&self.root, &scale.array_path).map_err(
            |err| DataError::ReadFailed {
                layer_id: layer_id.to_string(),
                message: err.to_string(),
            },
        )?;
        let subset = [
            timepoint.get()..timepoint.get() + 1,
            region.z_start..ends.z,
            region.y_start..ends.y,
            region.x_start..ends.x,
        ];
        let intensity_cache = self.shard_index_cache(&scale.array_path, &array)?;
        let values_read = retrieve_sharded_subset::<Vec<u16>>(
            &array,
            &intensity_cache,
            layer_id,
            &subset,
            encoded.intensity_shard_payloads,
        )?;
        let render_valid_read = self.retrieve_render_valid_subset(
            layer_id,
            scale,
            &subset,
            encoded.validity_shard_payloads,
        )?;
        let values = values_read.values;
        let render_valid = render_valid_read.values;
        let shape = region.shape()?;
        let expected_len = shape.element_count()? as usize;
        if values.len() != expected_len {
            return Err(DataError::ReadFailed {
                layer_id: layer_id.to_string(),
                message: format!("decoded {} values, expected {expected_len}", values.len()),
            });
        }
        validate_render_valid_len(layer_id, render_valid.as_deref(), expected_len)?;
        Ok(UncachedU16VolumeRead {
            volume: DenseVolumeU16 {
                dataset_id: self.dataset_id.clone(),
                layer_id: layer_id.clone(),
                scale_level,
                timepoint,
                shape,
                grid_to_world: translated_region_grid_to_world(scale.grid_to_world, region),
                values,
                render_valid,
            },
            diagnostics: self.sharded_read_diagnostics(
                encoded,
                values_read.cache,
                render_valid_read.cache,
            )?,
        })
    }

    fn read_u8_brick_region_uncached(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
        region: VolumeRegion,
    ) -> Result<UncachedU8BrickRead, DataError> {
        let layer = self
            .layer(layer_id)
            .ok_or_else(|| DataError::LayerNotFound(layer_id.to_string()))?;
        validate_u8_dense_layer(layer_id, layer)?;
        validate_timepoint(layer_id, layer, timepoint)?;

        let scale = self.scale(layer_id, scale_level)?;
        let (chunk_index, record, full_region) =
            brick_record_and_region(layer_id, scale, timepoint, brick_index)?;
        validate_region_within_brick(region, full_region)?;
        let read =
            self.read_u8_region_at_scale_uncached(layer_id, scale_level, timepoint, region)?;

        Ok(UncachedU8BrickRead {
            brick: VolumeBrickU8 {
                scale_level,
                brick_index,
                chunk_index,
                region,
                occupied: record.occupied,
                valid_voxel_count: record.valid_voxel_count,
                min: record.min,
                max: record.max,
                volume: read.volume,
            },
            diagnostics: read.diagnostics,
        })
    }

    fn read_u16_brick_region_uncached(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
        region: VolumeRegion,
    ) -> Result<UncachedU16BrickRead, DataError> {
        let layer = self
            .layer(layer_id)
            .ok_or_else(|| DataError::LayerNotFound(layer_id.to_string()))?;
        validate_u16_dense_layer(layer_id, layer)?;
        validate_timepoint(layer_id, layer, timepoint)?;

        let scale = self.scale(layer_id, scale_level)?;
        let (chunk_index, record, full_region) =
            brick_record_and_region(layer_id, scale, timepoint, brick_index)?;
        validate_region_within_brick(region, full_region)?;
        let read =
            self.read_u16_region_at_scale_uncached(layer_id, scale_level, timepoint, region)?;

        Ok(UncachedU16BrickRead {
            brick: VolumeBrickU16 {
                scale_level,
                brick_index,
                chunk_index,
                region,
                occupied: record.occupied,
                valid_voxel_count: record.valid_voxel_count,
                min: record.min,
                max: record.max,
                volume: read.volume,
            },
            diagnostics: read.diagnostics,
        })
    }

    fn read_f32_brick_region_uncached(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
        region: VolumeRegion,
    ) -> Result<UncachedF32BrickRead, DataError> {
        let layer = self
            .layer(layer_id)
            .ok_or_else(|| DataError::LayerNotFound(layer_id.to_string()))?;
        validate_f32_dense_layer(layer_id, layer)?;
        validate_timepoint(layer_id, layer, timepoint)?;

        let scale = self.scale(layer_id, scale_level)?;
        let (chunk_index, record, full_region) =
            brick_record_and_region(layer_id, scale, timepoint, brick_index)?;
        validate_region_within_brick(region, full_region)?;
        let read =
            self.read_f32_region_at_scale_uncached(layer_id, scale_level, timepoint, region)?;

        Ok(UncachedF32BrickRead {
            brick: VolumeBrickF32 {
                scale_level,
                brick_index,
                chunk_index,
                region,
                occupied: record.occupied,
                valid_voxel_count: record.valid_voxel_count,
                min: record.min,
                max: record.max,
                volume: read.volume,
            },
            diagnostics: read.diagnostics,
        })
    }

    fn read_u16_brick_uncached(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
    ) -> Result<UncachedU16BrickRead, DataError> {
        let layer = self
            .layer(layer_id)
            .ok_or_else(|| DataError::LayerNotFound(layer_id.to_string()))?;
        validate_u16_dense_layer(layer_id, layer)?;
        validate_timepoint(layer_id, layer, timepoint)?;

        let scale = self.scale(layer_id, scale_level)?;
        validate_spatial_brick_index(scale, brick_index)?;
        let chunk_index = BrickIndex {
            t: timepoint.get() / scale.storage.brick_shape.t(),
            z: brick_index.z,
            y: brick_index.y,
            x: brick_index.x,
        };
        let record = brick_record(layer_id, scale, chunk_index)?;
        let region = brick_region(scale, brick_index)?;
        let read =
            self.read_u16_region_at_scale_uncached(layer_id, scale_level, timepoint, region)?;

        Ok(UncachedU16BrickRead {
            brick: VolumeBrickU16 {
                scale_level,
                brick_index,
                chunk_index,
                region,
                occupied: record.occupied,
                valid_voxel_count: record.valid_voxel_count,
                min: record.min,
                max: record.max,
                volume: read.volume,
            },
            diagnostics: read.diagnostics,
        })
    }

    fn read_u8_brick_uncached(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
    ) -> Result<UncachedU8BrickRead, DataError> {
        let layer = self
            .layer(layer_id)
            .ok_or_else(|| DataError::LayerNotFound(layer_id.to_string()))?;
        validate_u8_dense_layer(layer_id, layer)?;
        validate_timepoint(layer_id, layer, timepoint)?;

        let scale = self.scale(layer_id, scale_level)?;
        validate_spatial_brick_index(scale, brick_index)?;
        let chunk_index = BrickIndex {
            t: timepoint.get() / scale.storage.brick_shape.t(),
            z: brick_index.z,
            y: brick_index.y,
            x: brick_index.x,
        };
        let record = brick_record(layer_id, scale, chunk_index)?;
        let region = brick_region(scale, brick_index)?;
        let read =
            self.read_u8_region_at_scale_uncached(layer_id, scale_level, timepoint, region)?;

        Ok(UncachedU8BrickRead {
            brick: VolumeBrickU8 {
                scale_level,
                brick_index,
                chunk_index,
                region,
                occupied: record.occupied,
                valid_voxel_count: record.valid_voxel_count,
                min: record.min,
                max: record.max,
                volume: read.volume,
            },
            diagnostics: read.diagnostics,
        })
    }

    fn read_f32_brick_uncached(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
    ) -> Result<UncachedF32BrickRead, DataError> {
        let layer = self
            .layer(layer_id)
            .ok_or_else(|| DataError::LayerNotFound(layer_id.to_string()))?;
        validate_f32_dense_layer(layer_id, layer)?;
        validate_timepoint(layer_id, layer, timepoint)?;

        let scale = self.scale(layer_id, scale_level)?;
        validate_spatial_brick_index(scale, brick_index)?;
        let chunk_index = BrickIndex {
            t: timepoint.get() / scale.storage.brick_shape.t(),
            z: brick_index.z,
            y: brick_index.y,
            x: brick_index.x,
        };
        let record = brick_record(layer_id, scale, chunk_index)?;
        let region = brick_region(scale, brick_index)?;
        let read =
            self.read_f32_region_at_scale_uncached(layer_id, scale_level, timepoint, region)?;

        Ok(UncachedF32BrickRead {
            brick: VolumeBrickF32 {
                scale_level,
                brick_index,
                chunk_index,
                region,
                occupied: record.occupied,
                valid_voxel_count: record.valid_voxel_count,
                min: record.min,
                max: record.max,
                volume: read.volume,
            },
            diagnostics: read.diagnostics,
        })
    }

    fn read_u8_volume_uncached(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
    ) -> Result<UncachedU8VolumeRead, DataError> {
        let layer = self
            .layer(layer_id)
            .ok_or_else(|| DataError::LayerNotFound(layer_id.to_string()))?;
        validate_u8_dense_layer(layer_id, layer)?;
        validate_timepoint(layer_id, layer, timepoint)?;

        let scale = self.scale(layer_id, scale_level)?;
        let array = mirante4d_format::zarr_io::open_array(&self.root, &scale.array_path).map_err(
            |err| DataError::ReadFailed {
                layer_id: layer_id.to_string(),
                message: err.to_string(),
            },
        )?;
        let spatial = scale.shape.spatial();
        let region = VolumeRegion::new(0, 0, 0, spatial.z(), spatial.y(), spatial.x())?;
        let encoded =
            encoded_payload_diagnostics_for_timepoint_region(layer_id, scale, timepoint, &region)?;
        let subset = [
            timepoint.get()..timepoint.get() + 1,
            0..spatial.z(),
            0..spatial.y(),
            0..spatial.x(),
        ];
        let intensity_cache = self.shard_index_cache(&scale.array_path, &array)?;
        let values_read = retrieve_sharded_subset::<Vec<u8>>(
            &array,
            &intensity_cache,
            layer_id,
            &subset,
            encoded.intensity_shard_payloads,
        )?;
        let render_valid_read = self.retrieve_render_valid_subset(
            layer_id,
            scale,
            &subset,
            encoded.validity_shard_payloads,
        )?;
        let values = values_read.values;
        let render_valid = render_valid_read.values;
        let expected_len = spatial.element_count()? as usize;
        if values.len() != expected_len {
            return Err(DataError::ReadFailed {
                layer_id: layer_id.to_string(),
                message: format!("decoded {} values, expected {expected_len}", values.len()),
            });
        }
        validate_render_valid_len(layer_id, render_valid.as_deref(), expected_len)?;

        Ok(UncachedU8VolumeRead {
            volume: DenseVolumeU8 {
                dataset_id: self.dataset_id.clone(),
                layer_id: layer_id.clone(),
                scale_level,
                timepoint,
                shape: spatial,
                grid_to_world: scale.grid_to_world,
                values,
                render_valid,
            },
            diagnostics: self.sharded_read_diagnostics(
                encoded,
                values_read.cache,
                render_valid_read.cache,
            )?,
        })
    }

    fn read_u16_volume_uncached(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
    ) -> Result<UncachedU16VolumeRead, DataError> {
        let layer = self
            .layer(layer_id)
            .ok_or_else(|| DataError::LayerNotFound(layer_id.to_string()))?;
        validate_u16_dense_layer(layer_id, layer)?;
        validate_timepoint(layer_id, layer, timepoint)?;

        let scale = self.scale(layer_id, scale_level)?;
        let array = mirante4d_format::zarr_io::open_array(&self.root, &scale.array_path).map_err(
            |err| DataError::ReadFailed {
                layer_id: layer_id.to_string(),
                message: err.to_string(),
            },
        )?;
        let spatial = scale.shape.spatial();
        let region = VolumeRegion::new(0, 0, 0, spatial.z(), spatial.y(), spatial.x())?;
        let encoded =
            encoded_payload_diagnostics_for_timepoint_region(layer_id, scale, timepoint, &region)?;
        let subset = [
            timepoint.get()..timepoint.get() + 1,
            0..spatial.z(),
            0..spatial.y(),
            0..spatial.x(),
        ];
        let intensity_cache = self.shard_index_cache(&scale.array_path, &array)?;
        let values_read = retrieve_sharded_subset::<Vec<u16>>(
            &array,
            &intensity_cache,
            layer_id,
            &subset,
            encoded.intensity_shard_payloads,
        )?;
        let render_valid_read = self.retrieve_render_valid_subset(
            layer_id,
            scale,
            &subset,
            encoded.validity_shard_payloads,
        )?;
        let values = values_read.values;
        let render_valid = render_valid_read.values;
        let expected_len = spatial.element_count()? as usize;
        if values.len() != expected_len {
            return Err(DataError::ReadFailed {
                layer_id: layer_id.to_string(),
                message: format!("decoded {} values, expected {expected_len}", values.len()),
            });
        }
        validate_render_valid_len(layer_id, render_valid.as_deref(), expected_len)?;

        Ok(UncachedU16VolumeRead {
            volume: DenseVolumeU16 {
                dataset_id: self.dataset_id.clone(),
                layer_id: layer_id.clone(),
                scale_level,
                timepoint,
                shape: spatial,
                grid_to_world: scale.grid_to_world,
                values,
                render_valid,
            },
            diagnostics: self.sharded_read_diagnostics(
                encoded,
                values_read.cache,
                render_valid_read.cache,
            )?,
        })
    }

    fn read_f32_region_at_scale_uncached(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        region: VolumeRegion,
    ) -> Result<UncachedF32VolumeRead, DataError> {
        let layer = self
            .layer(layer_id)
            .ok_or_else(|| DataError::LayerNotFound(layer_id.to_string()))?;
        validate_f32_dense_layer(layer_id, layer)?;
        validate_timepoint(layer_id, layer, timepoint)?;

        let scale = self.scale(layer_id, scale_level)?;
        let spatial = scale.shape.spatial();
        let ends = region.validate_within(spatial)?;
        let encoded =
            encoded_payload_diagnostics_for_timepoint_region(layer_id, scale, timepoint, &region)?;
        let array = mirante4d_format::zarr_io::open_array(&self.root, &scale.array_path).map_err(
            |err| DataError::ReadFailed {
                layer_id: layer_id.to_string(),
                message: err.to_string(),
            },
        )?;
        let subset = [
            timepoint.get()..timepoint.get() + 1,
            region.z_start..ends.z,
            region.y_start..ends.y,
            region.x_start..ends.x,
        ];
        let intensity_cache = self.shard_index_cache(&scale.array_path, &array)?;
        let values_read = retrieve_sharded_subset::<Vec<f32>>(
            &array,
            &intensity_cache,
            layer_id,
            &subset,
            encoded.intensity_shard_payloads,
        )?;
        let render_valid_read = self.retrieve_render_valid_subset(
            layer_id,
            scale,
            &subset,
            encoded.validity_shard_payloads,
        )?;
        let values = values_read.values;
        let render_valid = render_valid_read.values;
        let shape = region.shape()?;
        let expected_len = shape.element_count()? as usize;
        if values.len() != expected_len {
            return Err(DataError::ReadFailed {
                layer_id: layer_id.to_string(),
                message: format!("decoded {} values, expected {expected_len}", values.len()),
            });
        }
        validate_render_valid_len(layer_id, render_valid.as_deref(), expected_len)?;
        Ok(UncachedF32VolumeRead {
            volume: DenseVolumeF32 {
                dataset_id: self.dataset_id.clone(),
                layer_id: layer_id.clone(),
                scale_level,
                timepoint,
                shape,
                grid_to_world: translated_region_grid_to_world(scale.grid_to_world, region),
                values,
                render_valid,
            },
            diagnostics: self.sharded_read_diagnostics(
                encoded,
                values_read.cache,
                render_valid_read.cache,
            )?,
        })
    }

    fn read_f32_volume_uncached(
        &self,
        layer_id: &LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
    ) -> Result<UncachedF32VolumeRead, DataError> {
        let layer = self
            .layer(layer_id)
            .ok_or_else(|| DataError::LayerNotFound(layer_id.to_string()))?;
        validate_f32_dense_layer(layer_id, layer)?;
        validate_timepoint(layer_id, layer, timepoint)?;

        let scale = self.scale(layer_id, scale_level)?;
        let array = mirante4d_format::zarr_io::open_array(&self.root, &scale.array_path).map_err(
            |err| DataError::ReadFailed {
                layer_id: layer_id.to_string(),
                message: err.to_string(),
            },
        )?;
        let spatial = scale.shape.spatial();
        let region = VolumeRegion::new(0, 0, 0, spatial.z(), spatial.y(), spatial.x())?;
        let encoded =
            encoded_payload_diagnostics_for_timepoint_region(layer_id, scale, timepoint, &region)?;
        let subset = [
            timepoint.get()..timepoint.get() + 1,
            0..spatial.z(),
            0..spatial.y(),
            0..spatial.x(),
        ];
        let intensity_cache = self.shard_index_cache(&scale.array_path, &array)?;
        let values_read = retrieve_sharded_subset::<Vec<f32>>(
            &array,
            &intensity_cache,
            layer_id,
            &subset,
            encoded.intensity_shard_payloads,
        )?;
        let render_valid_read = self.retrieve_render_valid_subset(
            layer_id,
            scale,
            &subset,
            encoded.validity_shard_payloads,
        )?;
        let values = values_read.values;
        let render_valid = render_valid_read.values;
        let expected_len = spatial.element_count()? as usize;
        if values.len() != expected_len {
            return Err(DataError::ReadFailed {
                layer_id: layer_id.to_string(),
                message: format!("decoded {} values, expected {expected_len}", values.len()),
            });
        }
        validate_render_valid_len(layer_id, render_valid.as_deref(), expected_len)?;

        Ok(UncachedF32VolumeRead {
            volume: DenseVolumeF32 {
                dataset_id: self.dataset_id.clone(),
                layer_id: layer_id.clone(),
                scale_level,
                timepoint,
                shape: spatial,
                grid_to_world: scale.grid_to_world,
                values,
                render_valid,
            },
            diagnostics: self.sharded_read_diagnostics(
                encoded,
                values_read.cache,
                render_valid_read.cache,
            )?,
        })
    }

    fn shard_index_cache(
        &self,
        array_path: &str,
        array: &mirante4d_format::zarr_io::ZarrArray,
    ) -> Result<Arc<ArrayShardedReadableExtCache>, DataError> {
        self.runtime
            .shard_index_caches
            .lock()
            .map_err(|_| DataError::CachePoisoned)
            .map(|mut caches| caches.cache_for_array(array_path, array))
    }

    fn shard_index_cache_entry_count(&self) -> Result<u64, DataError> {
        self.runtime
            .shard_index_caches
            .lock()
            .map(|caches| caches.entry_count())
            .map_err(|_| DataError::CachePoisoned)
    }

    fn sharded_read_diagnostics(
        &self,
        encoded: EncodedPayloadDiagnostics,
        intensity_cache: ShardIndexCacheRead,
        validity_cache: ShardIndexCacheRead,
    ) -> Result<ShardedReadDiagnostics, DataError> {
        Ok(ShardedReadDiagnostics::from_parts(
            encoded,
            intensity_cache,
            validity_cache,
            self.shard_index_cache_entry_count()?,
        ))
    }

    fn retrieve_render_valid_subset(
        &self,
        layer_id: &LayerId,
        scale: &ScaleManifest,
        subset: &[Range<u64>; 4],
        touched_shards: u64,
    ) -> Result<RenderValidSubsetRead, DataError> {
        let Some(validity) = &scale.validity else {
            return Ok(RenderValidSubsetRead {
                values: None,
                cache: ShardIndexCacheRead::default(),
            });
        };
        let array = mirante4d_format::zarr_io::open_array(&self.root, &validity.array_path)
            .map_err(|err| DataError::ReadFailed {
                layer_id: layer_id.to_string(),
                message: err.to_string(),
            })?;
        let cache = self.shard_index_cache(&validity.array_path, &array)?;
        let read =
            retrieve_sharded_subset::<Vec<u8>>(&array, &cache, layer_id, subset, touched_shards)?;
        if read.values.iter().any(|value| !matches!(value, 0 | 1)) {
            return Err(DataError::ReadFailed {
                layer_id: layer_id.to_string(),
                message: "render-valid mask contains values other than 0 or 1".to_owned(),
            });
        }
        Ok(RenderValidSubsetRead {
            values: Some(read.values),
            cache: read.cache,
        })
    }

    fn cache_get_u8(&self, key: &VolumeCacheKey) -> Result<Option<DenseVolumeU8>, DataError> {
        self.runtime
            .cache
            .lock()
            .map_err(|_| DataError::CachePoisoned)
            .map(|mut cache| cache.get_u8(key))
    }

    fn cache_insert_u8(
        &self,
        key: VolumeCacheKey,
        volume: DenseVolumeU8,
    ) -> Result<CacheUpdate, DataError> {
        self.runtime
            .cache
            .lock()
            .map_err(|_| DataError::CachePoisoned)
            .map(|mut cache| cache.insert_u8(key, volume))
    }

    fn cache_get_u16(&self, key: &VolumeCacheKey) -> Result<Option<DenseVolumeU16>, DataError> {
        self.runtime
            .cache
            .lock()
            .map_err(|_| DataError::CachePoisoned)
            .map(|mut cache| cache.get_u16(key))
    }

    fn cache_insert_u16(
        &self,
        key: VolumeCacheKey,
        volume: DenseVolumeU16,
    ) -> Result<CacheUpdate, DataError> {
        self.runtime
            .cache
            .lock()
            .map_err(|_| DataError::CachePoisoned)
            .map(|mut cache| cache.insert_u16(key, volume))
    }

    fn brick_cache_get_u8(&self, key: &BrickCacheKey) -> Result<Option<VolumeBrickU8>, DataError> {
        self.runtime
            .brick_cache
            .lock()
            .map_err(|_| DataError::CachePoisoned)
            .map(|mut cache| cache.get_u8(key))
    }

    fn brick_cache_insert_u8(
        &self,
        key: BrickCacheKey,
        brick: VolumeBrickU8,
    ) -> Result<CacheUpdate, DataError> {
        self.runtime
            .brick_cache
            .lock()
            .map_err(|_| DataError::CachePoisoned)
            .map(|mut cache| cache.insert_u8(key, brick))
    }

    fn brick_cache_get_u16(
        &self,
        key: &BrickCacheKey,
    ) -> Result<Option<VolumeBrickU16>, DataError> {
        self.runtime
            .brick_cache
            .lock()
            .map_err(|_| DataError::CachePoisoned)
            .map(|mut cache| cache.get_u16(key))
    }

    fn brick_cache_insert_u16(
        &self,
        key: BrickCacheKey,
        brick: VolumeBrickU16,
    ) -> Result<CacheUpdate, DataError> {
        self.runtime
            .brick_cache
            .lock()
            .map_err(|_| DataError::CachePoisoned)
            .map(|mut cache| cache.insert_u16(key, brick))
    }

    fn brick_cache_get_f32(
        &self,
        key: &BrickCacheKey,
    ) -> Result<Option<VolumeBrickF32>, DataError> {
        self.runtime
            .brick_cache
            .lock()
            .map_err(|_| DataError::CachePoisoned)
            .map(|mut cache| cache.get_f32(key))
    }

    fn brick_cache_insert_f32(
        &self,
        key: BrickCacheKey,
        brick: VolumeBrickF32,
    ) -> Result<CacheUpdate, DataError> {
        self.runtime
            .brick_cache
            .lock()
            .map_err(|_| DataError::CachePoisoned)
            .map(|mut cache| cache.insert_f32(key, brick))
    }

    fn record_cache_hit(&self) -> Result<(), DataError> {
        self.update_stats(|stats| {
            stats.volume_cache_hits += 1;
        })
    }

    fn record_cache_miss(&self) -> Result<(), DataError> {
        self.update_stats(|stats| {
            stats.volume_cache_misses += 1;
        })
    }

    fn record_brick_cache_hit(&self) -> Result<(), DataError> {
        self.update_stats(|stats| {
            stats.brick_cache_hits += 1;
        })
    }

    fn record_brick_cache_miss(&self) -> Result<(), DataError> {
        self.update_stats(|stats| {
            stats.brick_cache_misses += 1;
        })
    }

    fn record_brick_read(
        &self,
        decoded_values: u64,
        decoded_value_bytes: u64,
    ) -> Result<(), DataError> {
        self.update_stats(|stats| {
            stats.brick_reads += 1;
            stats.decoded_brick_values += decoded_values;
            stats.decoded_brick_bytes += decoded_values * decoded_value_bytes;
        })
    }

    pub(crate) fn record_brick_request_queued(&self) -> Result<(), DataError> {
        self.update_stats(|stats| {
            stats.brick_requests_queued += 1;
        })
    }

    pub(crate) fn record_brick_request_completed(&self) -> Result<(), DataError> {
        self.update_stats(|stats| {
            stats.brick_requests_completed += 1;
        })
    }

    pub(crate) fn record_brick_request_cancelled(&self) -> Result<(), DataError> {
        self.update_stats(|stats| {
            stats.brick_requests_cancelled += 1;
        })
    }

    pub(crate) fn record_brick_request_stale(&self) -> Result<(), DataError> {
        self.update_stats(|stats| {
            stats.brick_requests_stale += 1;
        })
    }

    pub(crate) fn record_brick_request_failed(&self) -> Result<(), DataError> {
        self.update_stats(|stats| {
            stats.brick_requests_failed += 1;
        })
    }

    pub(crate) fn record_brick_queue_full(&self) -> Result<(), DataError> {
        self.update_stats(|stats| {
            stats.brick_queue_full += 1;
        })
    }

    fn record_subset_read(
        &self,
        decoded_values: u64,
        decoded_value_bytes: u64,
        diagnostics: ShardedReadDiagnostics,
    ) -> Result<(), DataError> {
        self.update_stats(|stats| {
            stats.subset_reads += 1;
            stats.decoded_values += decoded_values;
            stats.decoded_bytes += decoded_values * decoded_value_bytes;
            stats.encoded_payload_bytes_read += diagnostics.encoded_payload_bytes;
            stats.encoded_shard_payloads_read += diagnostics.encoded_shard_payloads;
            stats.shard_index_cache_hits += diagnostics.shard_index_cache_hits;
            stats.shard_index_cache_misses += diagnostics.shard_index_cache_misses;
            stats.shard_index_cache_entries = diagnostics.shard_index_cache_entries;
        })
    }

    fn record_cache_update(&self, update: CacheUpdate) -> Result<(), DataError> {
        self.update_stats(|stats| {
            stats.volume_cache_evictions += update.evictions;
            stats.volume_cache_bytes = update.current_bytes;
        })
    }

    fn record_brick_cache_update(&self, update: CacheUpdate) -> Result<(), DataError> {
        self.update_stats(|stats| {
            stats.brick_cache_evictions += update.evictions;
            stats.brick_cache_bytes = update.current_bytes;
            stats.brick_cache_u8_bytes = update.current_u8_bytes;
            stats.brick_cache_u16_bytes = update.current_u16_bytes;
            stats.brick_cache_f32_bytes = update.current_f32_bytes;
        })
    }

    fn update_stats(&self, update: impl FnOnce(&mut DataEngineStats)) -> Result<(), DataError> {
        let mut stats = self
            .runtime
            .stats
            .lock()
            .map_err(|_| DataError::CachePoisoned)?;
        update(&mut stats);
        Ok(())
    }
}

fn manifest_lookup_index(manifest: &NativeManifest) -> ManifestLookupIndex {
    let mut index = ManifestLookupIndex::default();
    for layer in &manifest.layers {
        for scale in &layer.scales {
            let mut scale_index = ScaleLookupIndex::default();
            for (record_index, record) in scale.bricks.records.iter().enumerate() {
                scale_index.brick_records.insert(record.index, record_index);
            }
            for (record_index, record) in scale.storage.shard_records.iter().enumerate() {
                scale_index
                    .intensity_shard_records
                    .insert(record.index, record_index);
            }
            if let Some(validity) = &scale.validity {
                for (record_index, record) in validity.storage.shard_records.iter().enumerate() {
                    scale_index
                        .validity_shard_records
                        .insert(record.index, record_index);
                }
            }
            index
                .scales
                .insert((layer.id.clone(), scale.level), scale_index);
        }
    }
    index
}

fn chunk_index_range(axis: Range<u64>, chunk_len: u64) -> Range<u64> {
    axis.start / chunk_len..axis.end.div_ceil(chunk_len)
}

#[cfg(test)]
mod tests;

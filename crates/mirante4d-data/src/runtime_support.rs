use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    ops::Range,
    sync::Arc,
};

use mirante4d_core::{IntensityDType, LayerId, TimeIndex};
use mirante4d_format::{BrickIndex, LayerKind, LayerManifest, ScaleManifest};
use zarrs::array::{
    ArrayShardedReadableExt, ArrayShardedReadableExtCache, CodecOptions, FromArrayBytes,
};

use super::{
    DataError, DenseVolumeF32, DenseVolumeU8, DenseVolumeU16, VolumeBrickF32, VolumeBrickU8,
    VolumeBrickU16, VolumeRegion,
};
use crate::payloads::{
    CacheUpdate, brick_payload_byte_len, u8_volume_byte_len, u16_volume_byte_len,
};

#[derive(Debug)]
pub(super) struct VolumeCache {
    max_bytes: u64,
    current_bytes: u64,
    order: VecDeque<VolumeCacheKey>,
    volumes: HashMap<VolumeCacheKey, CachedVolume>,
}

#[derive(Debug)]
pub(super) struct BrickCache {
    max_bytes: u64,
    current_bytes: u64,
    current_u8_bytes: u64,
    current_u16_bytes: u64,
    current_f32_bytes: u64,
    order: VecDeque<BrickCacheKey>,
    bricks: HashMap<BrickCacheKey, CachedBrick>,
}

#[derive(Default)]
pub(super) struct ShardIndexCaches {
    caches: HashMap<String, Arc<ArrayShardedReadableExtCache>>,
}

impl fmt::Debug for ShardIndexCaches {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShardIndexCaches")
            .field("array_count", &self.caches.len())
            .field("entry_count", &self.entry_count())
            .finish()
    }
}

#[derive(Debug, Clone)]
struct CachedVolume {
    payload: CachedVolumePayload,
    bytes: u64,
}

#[derive(Debug, Clone)]
enum CachedVolumePayload {
    U8(DenseVolumeU8),
    U16(DenseVolumeU16),
}

#[derive(Debug, Clone)]
struct CachedBrick {
    payload: CachedBrickPayload,
    bytes: u64,
}

#[derive(Debug, Clone)]
pub(super) enum CachedBrickPayload {
    U8(VolumeBrickU8),
    U16(VolumeBrickU16),
    F32(VolumeBrickF32),
}

#[derive(Debug)]
pub(super) struct UncachedU8VolumeRead {
    pub(super) volume: DenseVolumeU8,
    pub(super) diagnostics: ShardedReadDiagnostics,
}

#[derive(Debug)]
pub(super) struct UncachedU16VolumeRead {
    pub(super) volume: DenseVolumeU16,
    pub(super) diagnostics: ShardedReadDiagnostics,
}

#[derive(Debug)]
pub(super) struct UncachedF32VolumeRead {
    pub(super) volume: DenseVolumeF32,
    pub(super) diagnostics: ShardedReadDiagnostics,
}

#[derive(Debug)]
pub(super) struct UncachedU8BrickRead {
    pub(super) brick: VolumeBrickU8,
    pub(super) diagnostics: ShardedReadDiagnostics,
}

#[derive(Debug)]
pub(super) struct UncachedU16BrickRead {
    pub(super) brick: VolumeBrickU16,
    pub(super) diagnostics: ShardedReadDiagnostics,
}

#[derive(Debug)]
pub(super) struct UncachedF32BrickRead {
    pub(super) brick: VolumeBrickF32,
    pub(super) diagnostics: ShardedReadDiagnostics,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct EncodedPayloadDiagnostics {
    pub(super) payload_bytes: u64,
    pub(super) intensity_shard_payloads: u64,
    pub(super) validity_shard_payloads: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ShardIndexCacheRead {
    pub(super) hits: u64,
    pub(super) misses: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ShardedReadDiagnostics {
    pub(super) encoded_payload_bytes: u64,
    pub(super) encoded_shard_payloads: u64,
    pub(super) shard_index_cache_hits: u64,
    pub(super) shard_index_cache_misses: u64,
    pub(super) shard_index_cache_entries: u64,
}

#[derive(Debug)]
pub(super) struct ShardedSubsetRead<T> {
    pub(super) values: T,
    pub(super) cache: ShardIndexCacheRead,
}

#[derive(Debug)]
pub(super) struct RenderValidSubsetRead {
    pub(super) values: Option<Vec<u8>>,
    pub(super) cache: ShardIndexCacheRead,
}

impl EncodedPayloadDiagnostics {
    fn shard_payloads(self) -> u64 {
        self.intensity_shard_payloads + self.validity_shard_payloads
    }
}

impl ShardedReadDiagnostics {
    pub(super) fn from_parts(
        encoded: EncodedPayloadDiagnostics,
        intensity_cache: ShardIndexCacheRead,
        validity_cache: ShardIndexCacheRead,
        shard_index_cache_entries: u64,
    ) -> Self {
        Self {
            encoded_payload_bytes: encoded.payload_bytes,
            encoded_shard_payloads: encoded.shard_payloads(),
            shard_index_cache_hits: intensity_cache.hits + validity_cache.hits,
            shard_index_cache_misses: intensity_cache.misses + validity_cache.misses,
            shard_index_cache_entries,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct VolumeCacheKey {
    pub(super) layer_id: String,
    pub(super) scale_level: u32,
    pub(super) timepoint: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct BrickCacheKey {
    pub(super) layer_id: String,
    pub(super) scale_level: u32,
    pub(super) timepoint: u64,
    pub(super) z: u64,
    pub(super) y: u64,
    pub(super) x: u64,
}

#[cfg(test)]
pub(super) fn encoded_payload_bytes_for_timepoint_region(
    layer_id: &LayerId,
    scale: &ScaleManifest,
    timepoint: TimeIndex,
    region: &VolumeRegion,
) -> Result<u64, DataError> {
    Ok(
        encoded_payload_diagnostics_for_timepoint_region(layer_id, scale, timepoint, region)?
            .payload_bytes,
    )
}

pub(super) fn encoded_payload_diagnostics_for_timepoint_region(
    layer_id: &LayerId,
    scale: &ScaleManifest,
    timepoint: TimeIndex,
    region: &VolumeRegion,
) -> Result<EncodedPayloadDiagnostics, DataError> {
    let ends = region.ends()?;
    let t_chunks = chunk_index_range(timepoint.0..timepoint.0 + 1, scale.storage.shard_shape.t);
    let z_chunks = chunk_index_range(region.z_start..ends.z, scale.storage.shard_shape.z);
    let y_chunks = chunk_index_range(region.y_start..ends.y, scale.storage.shard_shape.y);
    let x_chunks = chunk_index_range(region.x_start..ends.x, scale.storage.shard_shape.x);

    let mut diagnostics = EncodedPayloadDiagnostics::default();
    let mut seen = HashSet::new();
    for t in t_chunks {
        for z in z_chunks.clone() {
            for y in y_chunks.clone() {
                for x in x_chunks.clone() {
                    let index = BrickIndex { t, z, y, x };
                    if seen.insert(("intensity", index)) {
                        diagnostics.payload_bytes +=
                            shard_record(layer_id, scale, index)?.payload_bytes;
                        diagnostics.intensity_shard_payloads += 1;
                    }
                    if let Some(validity) = &scale.validity
                        && seen.insert(("validity", index))
                    {
                        diagnostics.payload_bytes +=
                            validity_shard_record(layer_id, validity, index)?.payload_bytes;
                        diagnostics.validity_shard_payloads += 1;
                    }
                }
            }
        }
    }
    Ok(diagnostics)
}

fn chunk_index_range(axis: Range<u64>, chunk_len: u64) -> Range<u64> {
    axis.start / chunk_len..axis.end.div_ceil(chunk_len)
}

fn shard_record<'a>(
    layer_id: &LayerId,
    scale: &'a ScaleManifest,
    index: BrickIndex,
) -> Result<&'a mirante4d_format::ShardRecord, DataError> {
    scale
        .storage
        .shard_records
        .iter()
        .find(|record| record.index == index)
        .ok_or_else(|| DataError::BrickRecordMissing {
            layer_id: layer_id.to_string(),
            index,
        })
}

fn validity_shard_record<'a>(
    layer_id: &LayerId,
    validity: &'a mirante4d_format::ScaleValidityMask,
    index: BrickIndex,
) -> Result<&'a mirante4d_format::ShardRecord, DataError> {
    validity
        .storage
        .shard_records
        .iter()
        .find(|record| record.index == index)
        .ok_or_else(|| DataError::BrickRecordMissing {
            layer_id: layer_id.to_string(),
            index,
        })
}

pub(super) fn validate_dense_layer(
    layer_id: &LayerId,
    layer: &LayerManifest,
) -> Result<(), DataError> {
    if layer.kind != LayerKind::DenseIntensity {
        return Err(DataError::UnsupportedLayerKind {
            layer_id: layer_id.to_string(),
            kind: layer.kind,
        });
    }
    Ok(())
}

pub(super) fn validate_u8_dense_layer(
    layer_id: &LayerId,
    layer: &LayerManifest,
) -> Result<(), DataError> {
    validate_dense_layer(layer_id, layer)?;
    if layer.dtype.stored != IntensityDType::Uint8 {
        return Err(DataError::UnsupportedDType {
            layer_id: layer_id.to_string(),
            dtype: layer.dtype.stored,
        });
    }
    Ok(())
}

pub(super) fn validate_u16_dense_layer(
    layer_id: &LayerId,
    layer: &LayerManifest,
) -> Result<(), DataError> {
    validate_dense_layer(layer_id, layer)?;
    if layer.dtype.stored != IntensityDType::Uint16 {
        return Err(DataError::UnsupportedDType {
            layer_id: layer_id.to_string(),
            dtype: layer.dtype.stored,
        });
    }
    Ok(())
}

pub(super) fn validate_f32_dense_layer(
    layer_id: &LayerId,
    layer: &LayerManifest,
) -> Result<(), DataError> {
    validate_dense_layer(layer_id, layer)?;
    if layer.dtype.stored != IntensityDType::Float32 {
        return Err(DataError::UnsupportedDType {
            layer_id: layer_id.to_string(),
            dtype: layer.dtype.stored,
        });
    }
    Ok(())
}

pub(super) fn retrieve_sharded_subset<T: FromArrayBytes>(
    array: &mirante4d_format::zarr_io::ZarrArray,
    cache: &ArrayShardedReadableExtCache,
    layer_id: &LayerId,
    subset: &[Range<u64>; 4],
    touched_shards: u64,
) -> Result<ShardedSubsetRead<T>, DataError> {
    let entries_before = cache.len() as u64;
    let values = array
        .retrieve_array_subset_sharded_opt(cache, subset, &CodecOptions::default())
        .map_err(|err| DataError::ReadFailed {
            layer_id: layer_id.to_string(),
            message: err.to_string(),
        })?;
    let entries_after = cache.len() as u64;
    let misses = entries_after
        .saturating_sub(entries_before)
        .min(touched_shards);
    Ok(ShardedSubsetRead {
        values,
        cache: ShardIndexCacheRead {
            hits: touched_shards.saturating_sub(misses),
            misses,
        },
    })
}

pub(super) fn validate_render_valid_len(
    layer_id: &LayerId,
    render_valid: Option<&[u8]>,
    expected: usize,
) -> Result<(), DataError> {
    if let Some(render_valid) = render_valid
        && render_valid.len() != expected
    {
        return Err(DataError::ReadFailed {
            layer_id: layer_id.to_string(),
            message: format!(
                "decoded {} render-valid values, expected {expected}",
                render_valid.len()
            ),
        });
    }
    Ok(())
}

impl ShardIndexCaches {
    pub(super) fn cache_for_array(
        &mut self,
        array_path: &str,
        array: &mirante4d_format::zarr_io::ZarrArray,
    ) -> Arc<ArrayShardedReadableExtCache> {
        self.caches
            .entry(array_path.to_owned())
            .or_insert_with(|| Arc::new(ArrayShardedReadableExtCache::new(array)))
            .clone()
    }

    pub(super) fn entry_count(&self) -> u64 {
        self.caches.values().map(|cache| cache.len() as u64).sum()
    }
}

impl VolumeCache {
    pub(super) fn new(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            current_bytes: 0,
            order: VecDeque::new(),
            volumes: HashMap::new(),
        }
    }

    pub(super) fn get_u8(&mut self, key: &VolumeCacheKey) -> Option<DenseVolumeU8> {
        let volume = self
            .volumes
            .get(key)
            .and_then(|cached| match &cached.payload {
                CachedVolumePayload::U8(volume) => Some(volume.clone()),
                CachedVolumePayload::U16(_) => None,
            })?;
        self.order.retain(|existing| existing != key);
        self.order.push_back(key.clone());
        Some(volume)
    }

    pub(super) fn insert_u8(&mut self, key: VolumeCacheKey, volume: DenseVolumeU8) -> CacheUpdate {
        self.insert_payload(key, CachedVolumePayload::U8(volume))
    }

    pub(super) fn get_u16(&mut self, key: &VolumeCacheKey) -> Option<DenseVolumeU16> {
        let volume = self
            .volumes
            .get(key)
            .and_then(|cached| match &cached.payload {
                CachedVolumePayload::U8(_) => None,
                CachedVolumePayload::U16(volume) => Some(volume.clone()),
            })?;
        self.order.retain(|existing| existing != key);
        self.order.push_back(key.clone());
        Some(volume)
    }

    pub(super) fn insert_u16(
        &mut self,
        key: VolumeCacheKey,
        volume: DenseVolumeU16,
    ) -> CacheUpdate {
        self.insert_payload(key, CachedVolumePayload::U16(volume))
    }

    fn insert_payload(&mut self, key: VolumeCacheKey, payload: CachedVolumePayload) -> CacheUpdate {
        let bytes = match &payload {
            CachedVolumePayload::U8(volume) => u8_volume_byte_len(volume),
            CachedVolumePayload::U16(volume) => u16_volume_byte_len(volume),
        };
        if bytes > self.max_bytes {
            return CacheUpdate {
                evictions: self.clear(),
                current_bytes: self.current_bytes,
                current_u8_bytes: 0,
                current_u16_bytes: 0,
                current_f32_bytes: 0,
            };
        }
        self.order.retain(|existing| existing != &key);
        if let Some(existing) = self.volumes.remove(&key) {
            self.current_bytes -= existing.bytes;
        }
        self.order.push_back(key.clone());
        self.current_bytes += bytes;
        self.volumes.insert(key, CachedVolume { payload, bytes });
        let mut evictions = 0;
        while self.current_bytes > self.max_bytes {
            if let Some(evicted) = self.order.pop_front() {
                if let Some(volume) = self.volumes.remove(&evicted) {
                    self.current_bytes -= volume.bytes;
                    evictions += 1;
                }
            } else {
                break;
            }
        }
        CacheUpdate {
            evictions,
            current_bytes: self.current_bytes,
            current_u8_bytes: 0,
            current_u16_bytes: 0,
            current_f32_bytes: 0,
        }
    }

    fn clear(&mut self) -> u64 {
        let evictions = self.volumes.len() as u64;
        self.order.clear();
        self.volumes.clear();
        self.current_bytes = 0;
        evictions
    }
}

impl BrickCache {
    pub(super) fn new(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            current_bytes: 0,
            current_u8_bytes: 0,
            current_u16_bytes: 0,
            current_f32_bytes: 0,
            order: VecDeque::new(),
            bricks: HashMap::new(),
        }
    }

    pub(super) fn get_u16(&mut self, key: &BrickCacheKey) -> Option<VolumeBrickU16> {
        let brick = self
            .bricks
            .get(key)
            .and_then(|cached| match &cached.payload {
                CachedBrickPayload::U8(_) => None,
                CachedBrickPayload::U16(brick) => Some(brick.clone()),
                CachedBrickPayload::F32(_) => None,
            })?;
        self.order.retain(|existing| existing != key);
        self.order.push_back(key.clone());
        Some(brick)
    }

    pub(super) fn insert_u16(&mut self, key: BrickCacheKey, brick: VolumeBrickU16) -> CacheUpdate {
        self.insert_payload(key, CachedBrickPayload::U16(brick))
    }

    pub(super) fn get_u8(&mut self, key: &BrickCacheKey) -> Option<VolumeBrickU8> {
        let brick = self
            .bricks
            .get(key)
            .and_then(|cached| match &cached.payload {
                CachedBrickPayload::U8(brick) => Some(brick.clone()),
                CachedBrickPayload::U16(_) | CachedBrickPayload::F32(_) => None,
            })?;
        self.order.retain(|existing| existing != key);
        self.order.push_back(key.clone());
        Some(brick)
    }

    pub(super) fn insert_u8(&mut self, key: BrickCacheKey, brick: VolumeBrickU8) -> CacheUpdate {
        self.insert_payload(key, CachedBrickPayload::U8(brick))
    }

    pub(super) fn get_f32(&mut self, key: &BrickCacheKey) -> Option<VolumeBrickF32> {
        let brick = self
            .bricks
            .get(key)
            .and_then(|cached| match &cached.payload {
                CachedBrickPayload::U8(_) | CachedBrickPayload::U16(_) => None,
                CachedBrickPayload::F32(brick) => Some(brick.clone()),
            })?;
        self.order.retain(|existing| existing != key);
        self.order.push_back(key.clone());
        Some(brick)
    }

    pub(super) fn insert_f32(&mut self, key: BrickCacheKey, brick: VolumeBrickF32) -> CacheUpdate {
        self.insert_payload(key, CachedBrickPayload::F32(brick))
    }

    fn insert_payload(&mut self, key: BrickCacheKey, payload: CachedBrickPayload) -> CacheUpdate {
        let bytes = brick_payload_byte_len(&payload);
        if bytes > self.max_bytes {
            return CacheUpdate {
                evictions: self.clear(),
                current_bytes: self.current_bytes,
                current_u8_bytes: self.current_u8_bytes,
                current_u16_bytes: self.current_u16_bytes,
                current_f32_bytes: self.current_f32_bytes,
            };
        }
        self.order.retain(|existing| existing != &key);
        if let Some(existing) = self.bricks.remove(&key) {
            self.remove_bytes(&existing);
        }
        self.order.push_back(key.clone());
        self.add_bytes(&payload, bytes);
        self.bricks.insert(key, CachedBrick { payload, bytes });
        let mut evictions = 0;
        while self.current_bytes > self.max_bytes {
            if let Some(evicted) = self.order.pop_front() {
                if let Some(brick) = self.bricks.remove(&evicted) {
                    self.remove_bytes(&brick);
                    evictions += 1;
                }
            } else {
                break;
            }
        }
        CacheUpdate {
            evictions,
            current_bytes: self.current_bytes,
            current_u8_bytes: self.current_u8_bytes,
            current_u16_bytes: self.current_u16_bytes,
            current_f32_bytes: self.current_f32_bytes,
        }
    }

    fn add_bytes(&mut self, payload: &CachedBrickPayload, bytes: u64) {
        self.current_bytes += bytes;
        match payload {
            CachedBrickPayload::U8(_) => self.current_u8_bytes += bytes,
            CachedBrickPayload::U16(_) => self.current_u16_bytes += bytes,
            CachedBrickPayload::F32(_) => self.current_f32_bytes += bytes,
        }
    }

    fn remove_bytes(&mut self, brick: &CachedBrick) {
        self.current_bytes -= brick.bytes;
        match &brick.payload {
            CachedBrickPayload::U8(_) => self.current_u8_bytes -= brick.bytes,
            CachedBrickPayload::U16(_) => self.current_u16_bytes -= brick.bytes,
            CachedBrickPayload::F32(_) => self.current_f32_bytes -= brick.bytes,
        }
    }

    fn clear(&mut self) -> u64 {
        let evictions = self.bricks.len() as u64;
        self.order.clear();
        self.bricks.clear();
        self.current_bytes = 0;
        self.current_u8_bytes = 0;
        self.current_u16_bytes = 0;
        self.current_f32_bytes = 0;
        evictions
    }
}

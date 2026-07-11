use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use mirante4d_data::{DenseVolumeF32, DenseVolumeU16};
use wgpu::util::DeviceExt;

use super::{
    GpuRenderError, GpuRenderer, GpuRendererStats,
    buffers::{checked_buffer_byte_count, validate_storage_buffer_bytes},
};
use crate::resources::DenseVolumeResourceKey;

pub(super) const DEFAULT_GPU_VOLUME_CACHE_BYTES: u64 = 1024 * 1024 * 1024;
pub(super) const GPU_SAMPLE_INVALID_FLAG: u32 = 0x0001_0000;

struct GpuCachedVolume {
    buffer: Arc<wgpu::Buffer>,
    bytes: u64,
}

pub(super) struct GpuVolumeCache {
    pub(super) max_bytes: u64,
    current_bytes: u64,
    order: VecDeque<DenseVolumeResourceKey>,
    volumes: HashMap<DenseVolumeResourceKey, GpuCachedVolume>,
    pub(super) stats: GpuRendererStats,
}

impl GpuRenderer {
    pub(super) fn cached_volume_buffer(
        &self,
        volume: &DenseVolumeU16,
    ) -> Result<Arc<wgpu::Buffer>, GpuRenderError> {
        let key = DenseVolumeResourceKey::from_volume(volume)?;
        if let Some(buffer) = self
            .volume_cache
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?
            .get(&key)
        {
            return Ok(buffer);
        }

        let packed_len = volume.values().len();
        let bytes = checked_buffer_byte_count(
            "dense uint16 volume input",
            packed_len,
            std::mem::size_of::<u32>(),
        )?;
        validate_storage_buffer_bytes(&self.device.limits(), "dense uint16 volume input", bytes)?;
        let packed_values = render_upload_samples_u16(volume);
        let buffer = Arc::new(
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("mirante4d-camera-volume-input-packed-u16"),
                    contents: bytemuck::cast_slice(packed_values.as_ref()),
                    usage: wgpu::BufferUsages::STORAGE,
                }),
        );
        self.volume_cache
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?
            .insert(key, Arc::clone(&buffer), bytes);
        Ok(buffer)
    }

    pub(super) fn cached_f32_volume_buffer(
        &self,
        volume: &DenseVolumeF32,
    ) -> Result<Arc<wgpu::Buffer>, GpuRenderError> {
        let key = DenseVolumeResourceKey::from_f32_volume(volume)?;
        if let Some(buffer) = self
            .volume_cache
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?
            .get(&key)
        {
            return Ok(buffer);
        }

        let bytes = checked_buffer_byte_count(
            "dense float32 volume input",
            volume.values().len(),
            std::mem::size_of::<f32>(),
        )?;
        validate_storage_buffer_bytes(&self.device.limits(), "dense float32 volume input", bytes)?;
        let buffer = Arc::new(
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("mirante4d-camera-volume-input-f32"),
                    contents: bytemuck::cast_slice(render_upload_values_f32(volume).as_ref()),
                    usage: wgpu::BufferUsages::STORAGE,
                }),
        );
        self.volume_cache
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?
            .insert(key, Arc::clone(&buffer), bytes);
        Ok(buffer)
    }
}

impl GpuVolumeCache {
    pub(super) fn new(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            current_bytes: 0,
            order: VecDeque::new(),
            volumes: HashMap::new(),
            stats: GpuRendererStats::default(),
        }
    }

    fn get(&mut self, key: &DenseVolumeResourceKey) -> Option<Arc<wgpu::Buffer>> {
        let buffer = self
            .volumes
            .get(key)
            .map(|cached| Arc::clone(&cached.buffer));
        if buffer.is_some() {
            self.stats.volume_cache_hits += 1;
            self.order.retain(|existing| existing != key);
            self.order.push_back(key.clone());
        } else {
            self.stats.volume_cache_misses += 1;
        }
        buffer
    }

    fn insert(&mut self, key: DenseVolumeResourceKey, buffer: Arc<wgpu::Buffer>, bytes: u64) {
        self.stats.volume_uploads += 1;
        self.stats.volume_uploaded_bytes += bytes;
        if bytes > self.max_bytes {
            self.stats.volume_evictions += self.clear();
            self.stats.volume_resident_bytes = self.current_bytes;
            return;
        }

        self.order.retain(|existing| existing != &key);
        if let Some(existing) = self.volumes.remove(&key) {
            self.current_bytes -= existing.bytes;
        }
        self.order.push_back(key.clone());
        self.current_bytes += bytes;
        self.volumes.insert(key, GpuCachedVolume { buffer, bytes });

        while self.current_bytes > self.max_bytes {
            let Some(evicted) = self.order.pop_front() else {
                break;
            };
            if let Some(volume) = self.volumes.remove(&evicted) {
                self.current_bytes -= volume.bytes;
                self.stats.volume_evictions += 1;
            }
        }
        self.stats.volume_resident_bytes = self.current_bytes;
    }

    fn clear(&mut self) -> u64 {
        let evictions = self.volumes.len() as u64;
        self.order.clear();
        self.volumes.clear();
        self.current_bytes = 0;
        evictions
    }
}

pub(super) fn render_upload_samples_u16(volume: &DenseVolumeU16) -> Cow<'_, [u32]> {
    match volume.render_valid_mask() {
        Some(mask) => Cow::Owned(
            volume
                .values()
                .iter()
                .zip(mask.iter())
                .map(|(&value, &valid)| {
                    if valid == 1 {
                        u32::from(value)
                    } else {
                        GPU_SAMPLE_INVALID_FLAG
                    }
                })
                .collect(),
        ),
        None => Cow::Owned(volume.values().iter().copied().map(u32::from).collect()),
    }
}

pub(super) fn render_upload_values_f32(volume: &DenseVolumeF32) -> Cow<'_, [f32]> {
    match volume.render_valid_mask() {
        Some(mask) => Cow::Owned(
            volume
                .values()
                .iter()
                .zip(mask.iter())
                .map(|(&value, &valid)| if valid == 1 { value } else { f32::NAN })
                .collect(),
        ),
        None => Cow::Borrowed(volume.values()),
    }
}

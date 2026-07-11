use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use mirante4d_dataset::{CpuByteLease, CpuByteLedger, CpuLedgerCategory, DatasetResourceKey};
use mirante4d_domain::Shape3D;

use crate::{RenderError, gpu::GpuRenderError};

use super::{IntegerAtlasDType, LeaseAtlasPage};

pub(super) struct PackedIntegerBrick {
    pub(super) values: Vec<u32>,
    pub(super) validity_bits: Vec<u32>,
    pub(super) valid_voxel_count: u64,
    pub(super) min_value: u32,
    pub(super) max_value: u32,
    _charge: Arc<dyn CpuByteLease>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct UploadReadyIntegerBrickKey {
    resource: DatasetResourceKey,
    atlas_resource_shape: Shape3D,
    dtype: IntegerAtlasDType,
}

pub(super) struct UploadReadyIntegerBrickCache {
    pub(super) max_bytes: u64,
    pub(super) current_bytes: u64,
    pub(super) hits: u64,
    pub(super) misses: u64,
    pub(super) evictions: u64,
    order: VecDeque<UploadReadyIntegerBrickKey>,
    bricks: HashMap<UploadReadyIntegerBrickKey, Arc<PackedIntegerBrick>>,
}

impl UploadReadyIntegerBrickCache {
    pub(super) fn new(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            current_bytes: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
            order: VecDeque::new(),
            bricks: HashMap::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn get_or_pack(
        &mut self,
        page: LeaseAtlasPage<'_>,
        atlas_resource_shape: Shape3D,
        packed_u32_per_resource: u64,
        valid_u32_per_resource: u64,
        dtype: IntegerAtlasDType,
        cpu_ledger: &dyn CpuByteLedger,
    ) -> Result<Arc<PackedIntegerBrick>, GpuRenderError> {
        let key = UploadReadyIntegerBrickKey {
            resource: page.resource.key(),
            atlas_resource_shape,
            dtype,
        };
        if let Some(packed) = self.bricks.get(&key).cloned() {
            self.hits += 1;
            self.touch_key(&key);
            return Ok(packed);
        }

        self.misses += 1;
        let bytes = packed_integer_byte_len(packed_u32_per_resource, valid_u32_per_resource)?;
        let charge: Arc<dyn CpuByteLease> =
            Arc::from(cpu_ledger.try_acquire(CpuLedgerCategory::UploadStaging, bytes)?);
        let packed = Arc::new(pack_lease_page(
            page,
            atlas_resource_shape,
            packed_u32_per_resource,
            valid_u32_per_resource,
            dtype,
            charge,
        )?);
        if bytes > self.max_bytes || self.max_bytes == 0 {
            return Ok(packed);
        }

        self.current_bytes =
            self.current_bytes
                .checked_add(bytes)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                    resource: "upload-ready integer lease cache",
                })?;
        self.order.push_back(key);
        self.bricks.insert(key, packed.clone());
        self.evict_to_budget()?;
        Ok(packed)
    }

    fn touch_key(&mut self, key: &UploadReadyIntegerBrickKey) {
        self.order.retain(|candidate| candidate != key);
        self.order.push_back(*key);
    }

    fn evict_to_budget(&mut self) -> Result<(), GpuRenderError> {
        while self.current_bytes > self.max_bytes {
            let Some(evicted_key) = self.order.pop_front() else {
                self.current_bytes = 0;
                break;
            };
            if let Some(evicted) = self.bricks.remove(&evicted_key) {
                self.current_bytes = self
                    .current_bytes
                    .checked_sub(packed_integer_brick_bytes(&evicted)?)
                    .ok_or(RenderError::InvalidBrickAtlas(
                        "upload-ready lease cache byte count underflow",
                    ))?;
                self.evictions += 1;
            }
        }
        Ok(())
    }
}

fn pack_lease_page(
    page: LeaseAtlasPage<'_>,
    atlas_resource_shape: Shape3D,
    packed_u32_per_resource: u64,
    valid_u32_per_resource: u64,
    dtype: IntegerAtlasDType,
    charge: Arc<dyn CpuByteLease>,
) -> Result<PackedIntegerBrick, RenderError> {
    let mut packed = PackedIntegerBrick {
        values: vec![
            0;
            usize::try_from(packed_u32_per_resource).map_err(|_| {
                RenderError::InvalidBrickAtlas("packed integer resource exceeds usize")
            })?
        ],
        validity_bits: vec![
            0;
            usize::try_from(valid_u32_per_resource).map_err(|_| {
                RenderError::InvalidBrickAtlas("packed validity resource exceeds usize")
            })?
        ],
        valid_voxel_count: 0,
        min_value: u32::MAX,
        max_value: 0,
        _charge: charge,
    };
    let source_shape = page.payload.shape();
    let bytes = page.payload.value_bytes();
    for z in 0..source_shape.z() {
        for y in 0..source_shape.y() {
            for x in 0..source_shape.x() {
                let source_index = (z * source_shape.y() + y) * source_shape.x() + x;
                if !page.payload.sample_is_valid(source_index)? {
                    continue;
                }
                let value = match dtype {
                    IntegerAtlasDType::U8 => u32::from(
                        bytes[usize::try_from(source_index).map_err(|_| {
                            RenderError::InvalidBrickAtlas("sample index exceeds usize")
                        })?],
                    ),
                    IntegerAtlasDType::U16 => {
                        let offset = usize::try_from(source_index.checked_mul(2).ok_or(
                            RenderError::InvalidBrickAtlas("uint16 byte offset overflows"),
                        )?)
                        .map_err(|_| {
                            RenderError::InvalidBrickAtlas("uint16 offset exceeds usize")
                        })?;
                        u32::from(u16::from_le_bytes([bytes[offset], bytes[offset + 1]]))
                    }
                };
                let atlas_index = usize::try_from(
                    (z * atlas_resource_shape.y() + y) * atlas_resource_shape.x() + x,
                )
                .map_err(|_| RenderError::InvalidBrickAtlas("atlas sample index exceeds usize"))?;
                pack_integer_sample(&mut packed, atlas_index, value, dtype);
                packed.valid_voxel_count += 1;
                packed.min_value = packed.min_value.min(value);
                packed.max_value = packed.max_value.max(value);
            }
        }
    }
    if packed.valid_voxel_count == 0 {
        packed.min_value = 0;
    }
    Ok(packed)
}

pub(super) fn upload_ready_cache_budget(atlas_budget_bytes: u64) -> u64 {
    const MAX_UPLOAD_READY_CACHE_BYTES: u64 = 256 * 1024 * 1024;
    (atlas_budget_bytes / 4).min(MAX_UPLOAD_READY_CACHE_BYTES)
}

fn packed_integer_byte_len(
    packed_u32_per_resource: u64,
    valid_u32_per_resource: u64,
) -> Result<u64, GpuRenderError> {
    packed_u32_per_resource
        .checked_add(valid_u32_per_resource)
        .and_then(|words| words.checked_mul(std::mem::size_of::<u32>() as u64))
        .ok_or(GpuRenderError::BufferSizeOverflow {
            resource: "upload-ready packed integer lease",
        })
}

fn packed_integer_brick_bytes(packed: &PackedIntegerBrick) -> Result<u64, GpuRenderError> {
    u64::try_from(
        packed
            .values
            .len()
            .saturating_add(packed.validity_bits.len()),
    )
    .ok()
    .and_then(|words| words.checked_mul(std::mem::size_of::<u32>() as u64))
    .ok_or(GpuRenderError::BufferSizeOverflow {
        resource: "upload-ready cached integer lease",
    })
}

fn pack_integer_sample(
    packed: &mut PackedIntegerBrick,
    local_index: usize,
    value: u32,
    dtype: IntegerAtlasDType,
) {
    let values_per_word = dtype.values_per_word() as usize;
    let value_word = local_index / values_per_word;
    let value_shift = ((local_index % values_per_word) as u32) * dtype.bits_per_value();
    packed.values[value_word] |= (value & dtype.value_mask()) << value_shift;

    let validity_word = local_index / 32;
    let validity_bit = local_index % 32;
    packed.validity_bits[validity_word] |= 1_u32 << validity_bit;
}
